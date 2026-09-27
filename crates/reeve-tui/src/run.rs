//! The terminal loop (UI thread) and the worker thread that owns the agent.
//!
//! The worker runs one tokio runtime with two jobs: the agent task (turns,
//! reconnects, new sessions; one at a time) and quick side requests (model
//! lists, key checks) that run alongside a turn instead of waiting for it.

use std::collections::HashMap;
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
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use reeve_core::agent::{Agent, AgentEvent};
use reeve_core::config::{self, Config};
use reeve_core::ledger::{self, Totals};
use reeve_core::llm::{HttpProvider, ModelInfo, Provider};
use reeve_core::settings::Settings;
use reeve_core::spend::format_rates;
use reeve_observer::{HostInfo, Sampler, failed_units};

use crate::draw::draw;
use crate::overlay::{Action, KeyEntry, ModelPicker, Overlay, ProviderRow, Providers, palette};
use crate::theme::{ColorMode, Theme};
use crate::view::{Caps, Speaker, View};

/// What the UI asks of the worker.
enum Work {
    Send(String),
    /// Rebuild the agent from this config (new key, connection, or model).
    Connect(Box<Config>),
    NewSession,
    /// Side requests carry the config they were asked under: a connection
    /// added a moment ago must be known to them.
    ListModels(String, Box<Config>),
    Verify(String, Box<Config>),
}

/// What the worker (and helper threads) tell the UI.
enum UiMsg {
    Agent(AgentEvent),
    Ready {
        connection: String,
        model: String,
    },
    NotReady,
    Rates(String),
    Totals(Totals),
    Notice(Speaker, String),
    Failed(Vec<String>),
    Models {
        connection: String,
        result: Result<Vec<ModelInfo>, String>,
    },
    Verified {
        connection: String,
        result: Result<String, String>,
    },
    SessionReset,
}

type Term = Terminal<CrosstermBackend<Stdout>>;

/// UI-side state that isn't drawn.
struct App {
    cfg: Config,
    home: PathBuf,
    work: UnboundedSender<Work>,
    cancel: Arc<Notify>,
    catalog: HashMap<String, Vec<ModelInfo>>,
}

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
    let (work_tx, work_rx) = unbounded_channel::<Work>();
    let cancel = Arc::new(Notify::new());
    spawn_worker(
        home.clone(),
        host.profile(),
        ui_tx.clone(),
        work_rx,
        cancel.clone(),
    );
    spawn_unit_watch(ui_tx);

    let mut app = App {
        cfg,
        home,
        work: work_tx,
        cancel,
        catalog: HashMap::new(),
    };
    app.connect_quietly(&mut view);

    let mouse = app.cfg.ui.mouse;
    let mut term = setup(mouse)?;
    let result = event_loop(&mut term, &mut view, &theme, &ui_rx, &mut app);
    restore(mouse);
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
    app: &mut App,
) -> io::Result<()> {
    let mut sampler = Sampler::new();
    view.sample(sampler.sample());
    let mut last_sample = Instant::now();
    let tick = Duration::from_millis(50);
    loop {
        while let Ok(msg) = ui_rx.try_recv() {
            app.receive(view, msg);
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
                    Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(view, k),
                    Event::Paste(s) => {
                        let s = s.replace("\r\n", "\n").replace('\r', "\n");
                        match view.overlays.last_mut() {
                            Some(o) => o.on_paste(&s),
                            None => view.insert(&s),
                        }
                    }
                    Event::Mouse(m) if view.overlays.is_empty() => match m.kind {
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

impl App {
    fn receive(&mut self, view: &mut View, msg: UiMsg) {
        match msg {
            UiMsg::Agent(ev) => view.apply(ev),
            UiMsg::Ready { connection, model } => {
                view.drop_transient();
                if !view.ready && (view.connection != connection || view.model != model) {
                    view.push(
                        Speaker::System,
                        format!("Connected: {model} via {connection}."),
                    );
                }
                view.connection = connection;
                view.model = model;
                view.ready = true;
                self.refresh_providers(view);
            }
            UiMsg::NotReady => {
                view.ready = false;
                self.refresh_providers(view);
            }
            UiMsg::Rates(r) => view.rates = r,
            UiMsg::Totals(t) => view.totals = t,
            UiMsg::Notice(who, text) => view.push(who, text),
            UiMsg::Failed(f) => view.failed = Some(f),
            UiMsg::Models { connection, result } => {
                if let Ok(list) = &result {
                    self.catalog.insert(connection.clone(), list.clone());
                }
                for o in &mut view.overlays {
                    if let Overlay::Models(p) = o {
                        if p.connection == connection {
                            match &result {
                                Ok(list) => p.set_models(list.clone()),
                                Err(e) => {
                                    p.loading = false;
                                    p.error = Some(e.clone());
                                }
                            }
                        }
                    }
                }
            }
            UiMsg::Verified { connection, result } => {
                for o in &mut view.overlays {
                    if let Overlay::Providers(p) = o {
                        p.set_status(&connection, result.clone());
                    }
                }
                if view.overlays.is_empty() {
                    match result {
                        Ok(s) => view.push(Speaker::System, format!("{connection}: {s}")),
                        Err(e) => view.push(Speaker::Error, format!("{connection}: {e}")),
                    }
                }
            }
            UiMsg::SessionReset => {
                view.entries.clear();
                view.session = Default::default();
                view.last = None;
                view.scroll = 0;
                view.push(Speaker::System, "New session.");
            }
        }
    }

    fn on_key(&mut self, view: &mut View, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('c') && !view.overlays.is_empty() {
            view.overlays.clear();
            return;
        }
        if let Some(top) = view.overlays.last_mut() {
            let action = top.on_key(k);
            self.perform(view, action);
            return;
        }
        // The slash palette steals arrows, tab, and enter while it's open.
        let hits = palette(&view.input);
        if !hits.is_empty() {
            let sel = view.palette_sel.min(hits.len() - 1);
            match k.code {
                KeyCode::Up => {
                    view.palette_sel = sel.checked_sub(1).unwrap_or(hits.len() - 1);
                    return;
                }
                KeyCode::Down => {
                    view.palette_sel = (sel + 1) % hits.len();
                    return;
                }
                KeyCode::Tab => {
                    view.input = hits[sel].name.to_string();
                    view.cursor = view.input.len();
                    return;
                }
                KeyCode::Enter if !k.modifiers.contains(KeyModifiers::ALT) => {
                    let exact = hits.iter().find(|c| c.name == view.input);
                    let name = exact.unwrap_or(&hits[sel]).name;
                    view.input.clear();
                    view.cursor = 0;
                    view.palette_sel = 0;
                    self.command(view, name);
                    return;
                }
                _ => view.palette_sel = 0,
            }
        }
        self.composer_key(view, k);
    }

    fn composer_key(&mut self, view: &mut View, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);
        match k.code {
            KeyCode::Char('c') if ctrl => {
                if view.busy {
                    self.cancel.notify_one();
                } else if !view.input.is_empty() {
                    view.input.clear();
                    view.cursor = 0;
                } else {
                    view.quit = true;
                }
            }
            KeyCode::Char('q' | 'd') if ctrl => view.quit = true,
            KeyCode::Char('y') if ctrl => toggle_yolo(view),
            KeyCode::Char('b') if ctrl => view.rail_only = !view.rail_only,
            KeyCode::Char('p') if ctrl => self.command(view, "/providers"),
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
                    if !view.input.trim().is_empty() {
                        view.push_transient(
                            "Reeve isn't connected to a model yet. Opening /providers.",
                        );
                    }
                    self.command(view, "/providers");
                    return;
                }
                if let Some(text) = view.take_input() {
                    view.push(Speaker::User, text.clone());
                    view.busy = true;
                    let _ = self.work.send(Work::Send(text));
                }
            }
            KeyCode::Esc => {
                if view.busy {
                    self.cancel.notify_one();
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

    fn command(&mut self, view: &mut View, name: &str) {
        match name {
            "/providers" => {
                let mut p = Providers::default();
                p.refresh(self.provider_rows(view));
                let default = &self.cfg.default_connection;
                p.sel = p.rows.iter().position(|r| &r.name == default).unwrap_or(0);
                view.overlays.push(Overlay::Providers(p));
            }
            "/model" => {
                let conn = self.cfg.default_connection.clone();
                self.open_models(view, conn);
            }
            "/new" => {
                if view.busy {
                    view.push(Speaker::System, "Stop the running turn first (esc).");
                } else {
                    let _ = self.work.send(Work::NewSession);
                }
            }
            "/yolo" => toggle_yolo(view),
            "/help" => view.overlays.push(Overlay::Help),
            "/quit" => view.quit = true,
            _ => {}
        }
    }

    fn perform(&mut self, view: &mut View, action: Action) {
        match action {
            Action::None => {}
            Action::Close => {
                view.overlays.pop();
            }
            Action::Push(o) => view.overlays.push(o),
            Action::SaveKey {
                connection,
                secret,
                then_use,
            } => {
                view.overlays.pop();
                match config::store_secret_at(&self.home, &connection, &secret) {
                    Ok(_) => {
                        self.reload(view);
                        let _ = self
                            .work
                            .send(Work::Verify(connection.clone(), Box::new(self.cfg.clone())));
                        self.set_status(view, &connection, Ok("checking…".into()));
                        if then_use || connection == self.cfg.default_connection {
                            self.use_connection(view, &connection);
                        }
                    }
                    Err(e) => view.push(Speaker::Error, format!("couldn't save the key: {e}")),
                }
            }
            Action::ForgetKey(connection) => {
                match config::remove_secret_at(&self.home, &connection) {
                    Ok(true) => {
                        view.push(
                            Speaker::System,
                            format!("Forgot the stored key for {connection}."),
                        );
                    }
                    Ok(false) => view.push(
                        Speaker::System,
                        format!(
                            "{connection}'s key isn't stored by Reeve ({}); remove it there.",
                            config::secret_source(&self.cfg, &self.home, &connection)
                                .unwrap_or_else(|| "nowhere".into())
                        ),
                    ),
                    Err(e) => view.push(Speaker::Error, e.to_string()),
                }
                self.reload(view);
                if connection == self.cfg.default_connection {
                    self.connect(view);
                }
            }
            Action::Verify(connection) => {
                self.set_status(view, &connection, Ok("checking…".into()));
                let _ = self
                    .work
                    .send(Work::Verify(connection, Box::new(self.cfg.clone())));
            }
            Action::Use(connection) => self.use_connection(view, &connection),
            Action::OpenModels(connection) => self.open_models(view, connection),
            Action::ChooseModel { connection, model } => {
                // Picking a model is the end of setup: back to the chat.
                view.overlays.clear();
                self.edit_settings(view, |s| {
                    s.models.insert(connection.clone(), model.clone());
                    s.default_connection = Some(connection.clone());
                });
                self.connect(view);
            }
            Action::AddConnection { name, conn } => {
                if self.cfg.connections.contains_key(&name) {
                    if let Some(Overlay::Add(f)) = view.overlays.last_mut() {
                        f.error = Some(format!("there's already a connection named {name}"));
                        f.focus = 0;
                    }
                    return;
                }
                let local = conn.is_local();
                view.overlays.pop();
                self.edit_settings(view, |s| {
                    s.connections.insert(name.clone(), conn.clone());
                });
                if let Some(Overlay::Providers(p)) = view.overlays.last_mut() {
                    if let Some(i) = p.rows.iter().position(|r| r.name == name) {
                        p.sel = i;
                    }
                }
                if !local {
                    view.overlays.push(Overlay::Key(KeyEntry::new(&name, true)));
                }
            }
        }
    }

    fn open_models(&mut self, view: &mut View, connection: String) {
        let current = self
            .cfg
            .connections
            .get(&connection)
            .and_then(|c| c.default_model.clone());
        let current = if connection == self.cfg.default_connection {
            self.cfg.route().ok().map(|(_, _, m)| m).or(current)
        } else {
            current
        };
        let cached = self.catalog.get(&connection).cloned();
        view.overlays.push(Overlay::Models(ModelPicker::loading(
            &connection,
            current,
            cached,
        )));
        let _ = self
            .work
            .send(Work::ListModels(connection, Box::new(self.cfg.clone())));
    }

    fn use_connection(&mut self, view: &mut View, connection: &str) {
        if config::secret_source(&self.cfg, &self.home, connection).is_none() {
            view.overlays
                .push(Overlay::Key(KeyEntry::new(connection, true)));
            return;
        }
        let name = connection.to_string();
        self.edit_settings(view, |s| s.default_connection = Some(name.clone()));
        if self.cfg.route().is_err() {
            // No model yet: choose one first; choosing it connects.
            self.open_models(view, name);
        } else {
            view.overlays.clear();
            self.connect(view);
        }
    }

    fn edit_settings(&mut self, view: &mut View, f: impl FnOnce(&mut Settings)) {
        let mut s = match Settings::load(&self.home) {
            Ok(s) => s,
            Err(e) => {
                view.push(Speaker::Error, e.to_string());
                return;
            }
        };
        f(&mut s);
        if let Err(e) = s.save(&self.home) {
            view.push(Speaker::Error, format!("couldn't save settings: {e}"));
            return;
        }
        self.reload(view);
    }

    fn reload(&mut self, view: &mut View) {
        match config::load_at(&self.home) {
            Ok(cfg) => self.cfg = cfg,
            Err(e) => view.push(Speaker::Error, e.to_string()),
        }
        self.refresh_providers(view);
    }

    /// Tell the worker to (re)build the agent from the current config.
    fn connect(&mut self, view: &mut View) {
        view.ready = false;
        let _ = self.work.send(Work::Connect(Box::new(self.cfg.clone())));
    }

    /// First connection at startup: a missing key is a welcome, not an error.
    fn connect_quietly(&mut self, view: &mut View) {
        match self.cfg.route() {
            Ok((name, _, _)) if config::secret_source(&self.cfg, &self.home, &name).is_none() => {
                view.push_transient(format!(
                    "Welcome to Reeve. There's no API key for {name} yet: type /providers \
                         (or press ^p) to add one and pick a model. The live panels work in \
                         the meantime."
                ));
            }
            _ => self.connect(view),
        }
    }

    fn provider_rows(&self, view: &View) -> Vec<ProviderRow> {
        self.cfg
            .connections
            .iter()
            .map(|(name, c)| {
                let active = name == &self.cfg.default_connection;
                ProviderRow {
                    name: name.clone(),
                    kind: c.kind.clone(),
                    base_url: c.base_url.clone(),
                    key: config::secret_source(&self.cfg, &self.home, name),
                    active: active && view.ready,
                    model: if active {
                        self.cfg.route().ok().map(|(_, _, m)| m)
                    } else {
                        c.default_model.clone()
                    },
                    status: None,
                }
            })
            .collect()
    }

    fn refresh_providers(&self, view: &mut View) {
        let rows = self.provider_rows(view);
        for o in &mut view.overlays {
            if let Overlay::Providers(p) = o {
                p.refresh(rows.clone());
            }
        }
    }

    fn set_status(&self, view: &mut View, connection: &str, s: Result<String, String>) {
        for o in &mut view.overlays {
            if let Overlay::Providers(p) = o {
                p.set_status(connection, s.clone());
            }
        }
    }
}

fn toggle_yolo(view: &mut View) {
    view.yolo = !view.yolo;
    view.push(
        Speaker::System,
        if view.yolo {
            "YOLO on: T0–T2 actions will be approved without asking. The safeguard floor \
             still asks, and every action still gets a receipt."
        } else {
            "YOLO off: back to tiered approvals."
        },
    );
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

/// Work for the agent task, which handles one thing at a time.
enum AgentWork {
    Send(String),
    Connect(Box<Config>),
    NewSession,
}

fn spawn_worker(
    home: PathBuf,
    profile: String,
    tx: Sender<UiMsg>,
    mut work: UnboundedReceiver<Work>,
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
            let (agent_tx, agent_rx) = unbounded_channel::<AgentWork>();
            let agent_task = tokio::spawn(agent_loop(
                home.clone(),
                profile,
                tx.clone(),
                agent_rx,
                cancel,
            ));
            while let Some(w) = work.recv().await {
                match w {
                    Work::Send(t) => {
                        let _ = agent_tx.send(AgentWork::Send(t));
                    }
                    Work::NewSession => {
                        let _ = agent_tx.send(AgentWork::NewSession);
                    }
                    Work::Connect(c) => {
                        let _ = agent_tx.send(AgentWork::Connect(c));
                    }
                    Work::ListModels(connection, cfg) => {
                        let (tx, home) = (tx.clone(), home.clone());
                        tokio::spawn(async move {
                            let result = match side_provider(&cfg, &home, &connection) {
                                Ok(p) => p.list_models().await.map_err(|e| e.to_string()),
                                Err(e) => Err(e),
                            };
                            let _ = tx.send(UiMsg::Models { connection, result });
                        });
                    }
                    Work::Verify(connection, cfg) => {
                        let (tx, home) = (tx.clone(), home.clone());
                        tokio::spawn(async move {
                            let result = match side_provider(&cfg, &home, &connection) {
                                Ok(p) => p.verify().await.map_err(|e| e.to_string()),
                                Err(e) => Err(e),
                            };
                            let _ = tx.send(UiMsg::Verified { connection, result });
                        });
                    }
                }
            }
            drop(agent_tx);
            let _ = agent_task.await;
        });
    });
}

/// A provider for a one-off request. The OpenRouter catalog is public, so a
/// missing key only matters for the key check.
fn side_provider(
    cfg: &Config,
    home: &std::path::Path,
    connection: &str,
) -> Result<HttpProvider, String> {
    let conn = cfg
        .connections
        .get(connection)
        .ok_or_else(|| format!("no connection named {connection}"))?;
    let key = config::resolve_secret(cfg, home, connection).unwrap_or_default();
    Ok(HttpProvider::new(conn, key))
}

async fn agent_loop(
    home: PathBuf,
    profile: String,
    tx: Sender<UiMsg>,
    mut work: UnboundedReceiver<AgentWork>,
    cancel: Arc<Notify>,
) {
    let mut agent: Option<Agent> = None;
    let emit_tx = tx.clone();
    let emit = move |e: AgentEvent| {
        let _ = emit_tx.send(UiMsg::Agent(e));
    };
    while let Some(w) = work.recv().await {
        match w {
            AgentWork::Send(text) => {
                let Some(a) = agent.as_mut() else {
                    emit(AgentEvent::TurnStarted);
                    emit(AgentEvent::Error("not connected: open /providers".into()));
                    continue;
                };
                tokio::select! {
                    () = a.turn(text, &emit) => {}
                    () = cancel.notified() => emit(AgentEvent::Error("stopped".into())),
                }
                let _ = tx.send(UiMsg::Totals(ledger::totals(&home, Local::now())));
            }
            AgentWork::NewSession => {
                if let Some(a) = agent.as_mut() {
                    match a.reset() {
                        Ok(()) => {
                            let _ = tx.send(UiMsg::SessionReset);
                        }
                        Err(e) => emit(AgentEvent::Error(e.to_string())),
                    }
                }
            }
            AgentWork::Connect(cfg) => connect(&mut agent, *cfg, &home, &profile, &tx).await,
        }
    }
}

/// Build or re-point the agent, then load prices. Failures are said in the
/// chat; the agent is left unset so a turn can't run on a half-built route.
async fn connect(
    agent: &mut Option<Agent>,
    cfg: Config,
    home: &std::path::Path,
    profile: &str,
    tx: &Sender<UiMsg>,
) {
    let fail = |text: String| {
        let _ = tx.send(UiMsg::Notice(Speaker::Error, text));
        let _ = tx.send(UiMsg::NotReady);
    };
    let (name, conn, model) = match cfg.route() {
        Ok((n, c, m)) => (n, c.clone(), m),
        Err(e) => {
            *agent = None;
            return fail(format!("{e} — pick one with /model"));
        }
    };
    let key = match config::resolve_secret(&cfg, home, &name) {
        Ok(k) => k,
        Err(e) => {
            *agent = None;
            return fail(e.to_string());
        }
    };
    let provider = Box::new(HttpProvider::new(&conn, key));
    match agent.take() {
        Some(mut a) => {
            a.switch(provider, cfg.clone(), name.clone(), model.clone());
            *agent = Some(a);
        }
        None => match Agent::new(
            provider,
            cfg.clone(),
            home.to_path_buf(),
            name.clone(),
            model.clone(),
            profile,
        ) {
            Ok(a) => *agent = Some(a),
            Err(e) => return fail(e.to_string()),
        },
    }
    let _ = tx.send(UiMsg::Ready {
        connection: name.clone(),
        model: model.clone(),
    });
    let _ = tx.send(UiMsg::Rates("loading prices…".into()));
    let Some(a) = agent.as_mut() else { return };
    match a.refresh_models().await {
        Ok(models) => {
            let mut label = format_rates(a.book().rates(&model));
            if !models.is_empty() && !models.iter().any(|m| m.id == model) {
                label = format!("{label} · not in {name}'s catalog");
            }
            let _ = tx.send(UiMsg::Rates(label));
            let _ = tx.send(UiMsg::Models {
                connection: name,
                result: Ok(models),
            });
        }
        Err(e) => {
            let _ = tx.send(UiMsg::Rates("$?.??/M · prices unavailable".into()));
            let _ = tx.send(UiMsg::Notice(
                Speaker::System,
                format!(
                    "Couldn't fetch model prices ({e}). Costs the provider reports still count; \
                     others show as $?.??."
                ),
            ));
        }
    }
}
