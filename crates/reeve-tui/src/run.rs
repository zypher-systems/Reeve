//! The terminal loop (UI thread) and the agent worker thread.

use std::io::{self, Stdout};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Local;
use crossterm::ExecutableCommand;
use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::Notify;

use reeve_core::agent::{Agent, AgentEvent};
use reeve_core::config::{self, Config};
use reeve_core::ledger::{self, Totals};
use reeve_core::llm::HttpProvider;
use reeve_core::spend::format_rates;
use reeve_observer::{HostInfo, Sampler, failed_units};

use crate::draw::draw;
use crate::theme::{ColorMode, Theme};
use crate::view::{Caps, Speaker, View};

/// What the UI asks of the worker.
enum Work {
    Send(String),
}

/// What the worker (and helper threads) tell the UI.
enum UiMsg {
    Agent(AgentEvent),
    Ready { connection: String, model: String },
    Rates(String),
    Totals(Totals),
    Notice(Speaker, String),
    Failed(Vec<String>),
}

type Term = Terminal<CrosstermBackend<Stdout>>;

/// Run the TUI until the user quits.
pub fn run(cfg: Config, home: PathBuf) -> io::Result<()> {
    let mode = ColorMode::detect(&cfg.ui.colors, |k| std::env::var(k).ok());
    let theme = Theme::named(&cfg.ui.theme, mode);
    let host = HostInfo::read();

    let mut view = View::new(host.clone());
    view.animate = cfg.ui.animate;
    view.yolo = cfg.approvals.yolo;
    view.caps = Caps {
        session: cfg.spend.session_usd,
        daily: cfg.spend.daily_usd,
        monthly: cfg.spend.monthly_usd,
    };
    view.totals = ledger::totals(&home, Local::now());

    let (ui_tx, ui_rx) = mpsc::channel::<UiMsg>();
    let (work_tx, work_rx) = tokio::sync::mpsc::unbounded_channel::<Work>();
    let cancel = Arc::new(Notify::new());
    spawn_worker(
        cfg.clone(),
        home,
        host.profile(),
        ui_tx.clone(),
        work_rx,
        cancel.clone(),
    );
    spawn_unit_watch(ui_tx);

    let mut term = setup(cfg.ui.mouse)?;
    let result = event_loop(&mut term, &mut view, &theme, &ui_rx, &work_tx, &cancel);
    restore(cfg.ui.mouse);
    result
}

fn setup(mouse: bool) -> io::Result<Term> {
    // Put the terminal back even if something panics mid-frame.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore(true);
        hook(info);
    }));
    enable_raw_mode()?;
    let mut out = io::stdout();
    out.execute(EnterAlternateScreen)?;
    out.execute(EnableBracketedPaste)?;
    if mouse {
        let _ = out.execute(EnableMouseCapture);
    }
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    term.clear()?;
    Ok(term)
}

fn restore(mouse: bool) {
    let mut out = io::stdout();
    if mouse {
        let _ = out.execute(DisableMouseCapture);
    }
    let _ = out.execute(DisableBracketedPaste);
    let _ = out.execute(LeaveAlternateScreen);
    let _ = disable_raw_mode();
    let _ = out.execute(crossterm::cursor::Show);
}

fn event_loop(
    term: &mut Term,
    view: &mut View,
    theme: &Theme,
    ui_rx: &Receiver<UiMsg>,
    work_tx: &tokio::sync::mpsc::UnboundedSender<Work>,
    cancel: &Notify,
) -> io::Result<()> {
    let mut sampler = Sampler::new();
    view.sample(sampler.sample());
    let mut last_sample = Instant::now();
    let tick = Duration::from_millis(50);
    loop {
        while let Ok(msg) = ui_rx.try_recv() {
            apply(view, msg);
        }
        if last_sample.elapsed() >= Duration::from_secs(1) {
            view.sample(sampler.sample());
            last_sample = Instant::now();
        }
        term.draw(|f| draw(f, view, theme))?;
        view.frame = view.frame.wrapping_add(1);

        if event::poll(tick)? {
            // Drain everything queued so a paste or key repeat lands at once.
            loop {
                match event::read()? {
                    Event::Key(k) if k.kind != KeyEventKind::Release => {
                        on_key(view, k, work_tx, cancel);
                    }
                    Event::Paste(s) => view.insert(&s.replace("\r\n", "\n").replace('\r', "\n")),
                    Event::Mouse(m) => match m.kind {
                        MouseEventKind::ScrollUp => view.scroll += 3,
                        MouseEventKind::ScrollDown => view.scroll = view.scroll.saturating_sub(3),
                        _ => {}
                    },
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
        if view.quit {
            return Ok(());
        }
    }
}

fn apply(view: &mut View, msg: UiMsg) {
    match msg {
        UiMsg::Agent(ev) => view.apply(ev),
        UiMsg::Ready { connection, model } => {
            view.connection = connection;
            view.model = model;
            view.ready = true;
        }
        UiMsg::Rates(r) => view.rates = r,
        UiMsg::Totals(t) => view.totals = t,
        UiMsg::Notice(who, text) => view.push(who, text),
        UiMsg::Failed(f) => view.failed = Some(f),
    }
}

fn on_key(
    view: &mut View,
    k: KeyEvent,
    work_tx: &tokio::sync::mpsc::UnboundedSender<Work>,
    cancel: &Notify,
) {
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    let alt = k.modifiers.contains(KeyModifiers::ALT);
    let shift = k.modifiers.contains(KeyModifiers::SHIFT);
    match k.code {
        KeyCode::Char('c') if ctrl => {
            if view.busy {
                cancel.notify_one();
            } else if !view.input.is_empty() {
                view.input.clear();
                view.cursor = 0;
            } else {
                view.quit = true;
            }
        }
        KeyCode::Char('q' | 'd') if ctrl => view.quit = true,
        KeyCode::Char('y') if ctrl => {
            view.yolo = !view.yolo;
            view.push(
                Speaker::System,
                if view.yolo {
                    "YOLO on: T0–T2 actions will be approved without asking. The safeguard \
                     floor still asks, and every action still gets a receipt."
                } else {
                    "YOLO off: back to tiered approvals."
                },
            );
        }
        KeyCode::Char('b') if ctrl => view.rail_only = !view.rail_only,
        KeyCode::Char('w') if ctrl => view.delete_word(),
        KeyCode::Char('u') if ctrl => {
            view.input.clear();
            view.cursor = 0;
        }
        KeyCode::Char('a') if ctrl => view.cursor = 0,
        KeyCode::Char('e') if ctrl => view.cursor = view.input.len(),
        KeyCode::Char('j') if ctrl => view.insert("\n"),
        KeyCode::Enter if alt || shift => view.insert("\n"),
        KeyCode::Enter => {
            if view.busy {
                return;
            }
            if !view.ready {
                view.push(
                    Speaker::Error,
                    "Reeve isn't connected to a model yet — see the setup note above.",
                );
                return;
            }
            if let Some(text) = view.take_input() {
                view.push(Speaker::User, text.clone());
                view.busy = true;
                let _ = work_tx.send(Work::Send(text));
            }
        }
        KeyCode::Esc => {
            if view.busy {
                cancel.notify_one();
            } else {
                view.input.clear();
                view.cursor = 0;
            }
        }
        KeyCode::Backspace => view.backspace(),
        KeyCode::Delete => view.delete(),
        KeyCode::Left => view.left(),
        KeyCode::Right => view.right(),
        KeyCode::Home => view.cursor = 0,
        KeyCode::End => view.cursor = view.input.len(),
        KeyCode::PageUp => view.scroll += 10,
        KeyCode::PageDown => view.scroll = view.scroll.saturating_sub(10),
        KeyCode::Char(c) if !ctrl => {
            let mut buf = [0u8; 4];
            view.insert(c.encode_utf8(&mut buf));
        }
        _ => {}
    }
}

fn spawn_unit_watch(tx: Sender<UiMsg>) {
    thread::spawn(move || {
        loop {
            if tx.send(UiMsg::Failed(failed_units())).is_err() {
                return;
            }
            thread::sleep(Duration::from_secs(30));
        }
    });
}

fn spawn_worker(
    cfg: Config,
    home: PathBuf,
    profile: String,
    tx: Sender<UiMsg>,
    mut work: tokio::sync::mpsc::UnboundedReceiver<Work>,
    cancel: Arc<Notify>,
) {
    thread::spawn(move || {
        let Ok(rt) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            let _ = tx.send(UiMsg::Notice(
                Speaker::Error,
                "could not start the async runtime".into(),
            ));
            return;
        };
        rt.block_on(async move {
            let Some(mut agent) = connect(&cfg, &home, &profile, &tx) else {
                return;
            };
            match agent.refresh_models().await {
                Ok(models) => {
                    let model = agent_model(&cfg);
                    let rates = agent.book().rates(&model);
                    let known = models.iter().any(|m| m.id == model);
                    let mut label = format_rates(rates);
                    if !known && !models.is_empty() {
                        label = format!("{label} · not in the catalog");
                    }
                    let _ = tx.send(UiMsg::Rates(label));
                }
                Err(e) => {
                    let _ = tx.send(UiMsg::Rates("$?.??/M · prices unavailable".into()));
                    let _ = tx.send(UiMsg::Notice(
                        Speaker::System,
                        format!(
                            "Couldn't fetch model prices ({e}). Costs the provider reports \
                             still count; others show as $?.??."
                        ),
                    ));
                }
            }
            let emit_tx = tx.clone();
            let emit = move |e: AgentEvent| {
                let _ = emit_tx.send(UiMsg::Agent(e));
            };
            while let Some(Work::Send(text)) = work.recv().await {
                tokio::select! {
                    () = agent.turn(text, &emit) => {}
                    () = cancel.notified() => {
                        emit(AgentEvent::Error("stopped".into()));
                    }
                }
                let _ = tx.send(UiMsg::Totals(ledger::totals(&home, Local::now())));
            }
        });
    });
}

fn agent_model(cfg: &Config) -> String {
    cfg.route().map(|(_, _, m)| m).unwrap_or_default()
}

/// Build the agent, or explain in the chat why it can't be built.
fn connect(
    cfg: &Config,
    home: &std::path::Path,
    profile: &str,
    tx: &Sender<UiMsg>,
) -> Option<Agent> {
    let say = |who, text: String| {
        let _ = tx.send(UiMsg::Notice(who, text));
    };
    let (name, conn, model) = match cfg.route() {
        Ok(r) => r,
        Err(e) => {
            say(Speaker::Error, e.to_string());
            return None;
        }
    };
    let key = match config::resolve_secret(cfg, home, &name) {
        Ok(k) => k,
        Err(_) => {
            let env = conn
                .env_key
                .as_deref()
                .map(|v| format!(" (or export {v})"))
                .unwrap_or_default();
            say(
                Speaker::System,
                format!(
                    "Welcome to Reeve. There's no API key for `{name}` yet.\n\nQuit with ^c, \
                     run `reeve key set {name}`{env}, and start Reeve again. The live panels \
                     work in the meantime."
                ),
            );
            return None;
        }
    };
    let provider = Box::new(HttpProvider::new(conn, key));
    match Agent::new(
        provider,
        cfg.clone(),
        home.to_path_buf(),
        name.clone(),
        model.clone(),
        profile,
    ) {
        Ok(agent) => {
            let _ = tx.send(UiMsg::Ready {
                connection: name,
                model,
            });
            Some(agent)
        }
        Err(e) => {
            say(Speaker::Error, e.to_string());
            None
        }
    }
}
