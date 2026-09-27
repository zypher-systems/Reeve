//! `reeve`: the TUI by default, plus a few plain commands.

#![forbid(unsafe_code)]

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use reeve_core::config::{self, Config};
use reeve_core::llm::{HttpProvider, Provider};
use reeve_core::receipts::ReceiptBook;
use reeve_core::spend::{PriceBook, format_rates, format_tokens};

#[derive(Parser)]
#[command(name = "reeve", version, about = "An operator agent for your computer")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage API keys.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
    /// List a connection's models with live prices.
    Models {
        /// Connection (default: the configured one).
        #[arg(short, long)]
        connection: Option<String>,
        /// Only models whose id contains this.
        filter: Option<String>,
    },
    /// Show what Reeve has spent today and this month.
    Spend,
    /// Check the install: binary, keys, sudo, journal, observer, snapper, receipts.
    Doctor,
    /// List, inspect, or verify receipts.
    Receipts {
        #[command(subcommand)]
        cmd: Option<ReceiptsCmd>,
    },
    /// The background observer (reeved).
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Undo the action on a receipt.
    Undo {
        /// Receipt number.
        seq: u64,
        /// Don't ask for confirmation.
        #[arg(short, long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
enum DaemonCmd {
    /// Install reeved as a systemd user service and start it.
    Install,
    /// Stop and remove the service.
    Uninstall,
    /// Is it running, and what has it found.
    Status,
    /// Run the observer in the foreground (what the service runs).
    Run {
        /// One detection pass, then exit.
        #[arg(long)]
        once: bool,
    },
}

#[derive(Subcommand)]
enum ReceiptsCmd {
    /// The newest receipts (default).
    List {
        /// How many.
        #[arg(short, default_value_t = 30)]
        n: usize,
    },
    /// One receipt in full.
    Show {
        /// Receipt number.
        seq: u64,
    },
    /// Check the whole chain: nothing missing, nothing changed.
    Verify,
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Store a key in ~/.reeve/keys/<connection> (mode 0600). Reads stdin when piped.
    Set {
        /// Connection name, e.g. `openrouter` or `openai`.
        connection: String,
    },
}

fn main() -> ExitCode {
    // sudo runs the askpass link; `sudo reeve root` is the root file helper.
    // Neither reads Reeve's config or touches the network.
    let argv0 = std::env::args().next().unwrap_or_default();
    if std::path::Path::new(&argv0)
        .file_name()
        .is_some_and(|n| n == reeve_core::sudo::HELPER_NAME)
    {
        return ExitCode::from(reeve_core::sudo::helper_main() as u8);
    }
    if std::env::args().nth(1).as_deref() == Some("root") {
        return ExitCode::from(reeve_core::root::root_main() as u8);
    }
    let cli = Cli::parse();
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("reeve: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<(), String> {
    let home = config::home_dir();
    let cfg = config::load_at(&home).map_err(|e| e.to_string())?;
    match cli.cmd {
        None => reeve_tui::run(cfg, home).map_err(|e| e.to_string()),
        Some(Cmd::Key {
            cmd: KeyCmd::Set { connection },
        }) => {
            if !cfg.connections.contains_key(&connection) {
                return Err(format!(
                    "no connection named {connection:?}; known: {}",
                    cfg.connections
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let secret = read_secret(&format!("API key for {connection}: "))?;
            let path =
                config::store_secret_at(&home, &connection, &secret).map_err(|e| e.to_string())?;
            println!("Stored in {} (mode 0600).", path.display());
            Ok(())
        }
        Some(Cmd::Models { connection, filter }) => models(&cfg, &home, connection, filter),
        Some(Cmd::Daemon { cmd }) => daemon(&home, cmd),
        Some(Cmd::Doctor) => {
            doctor(&cfg, &home);
            Ok(())
        }
        Some(Cmd::Receipts { cmd }) => receipts(&home, cmd.unwrap_or(ReceiptsCmd::List { n: 30 })),
        Some(Cmd::Undo { seq, yes }) => {
            let book = ReceiptBook::new(&home);
            let r = book
                .find(seq)
                .ok_or_else(|| format!("there's no receipt #{seq}"))?;
            println!("#{} {} {} — {}", r.seq, r.tier.label(), r.tool, r.target());
            if !yes && !confirm("Undo it? [y/N] ")? {
                return Ok(());
            }
            // Root files, packages, and units need sudo: ask on this terminal.
            let mut ctx = reeve_core::tools::ToolCtx::new(home.clone(), vec![]);
            ctx.interactive_sudo = true;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            let done = rt
                .block_on(reeve_core::tools::undo_receipt(&ctx, &book, seq, "cli"))
                .map_err(|e| e.to_string())?;
            println!("{} (receipt #{})", done.outcome.summary, done.seq);
            Ok(())
        }
        Some(Cmd::Spend) => {
            let t = reeve_core::ledger::totals(&home, chrono_now());
            println!(
                "today  {:>10}  {} calls  {} tokens",
                t.today.label(),
                t.today.calls,
                format_tokens(t.today.usage.total())
            );
            println!(
                "month  {:>10}  {} calls  {} tokens",
                t.month.label(),
                t.month.calls,
                format_tokens(t.month.usage.total())
            );
            let caps = &cfg.spend;
            for (name, cap) in [
                ("session", caps.session_usd),
                ("daily", caps.daily_usd),
                ("monthly", caps.monthly_usd),
            ] {
                if cap > 0.0 {
                    println!("{name} cap ${cap:.2}");
                }
            }
            Ok(())
        }
    }
}

fn receipts(home: &std::path::Path, cmd: ReceiptsCmd) -> Result<(), String> {
    let book = ReceiptBook::new(home);
    match cmd {
        ReceiptsCmd::List { n } => {
            let list = book.recent(n);
            if list.is_empty() {
                println!("No receipts yet.");
            }
            let undone: std::collections::HashSet<u64> =
                book.all().iter().filter_map(|r| r.undoes).collect();
            for r in list.iter().rev() {
                let mark = if undone.contains(&r.seq) {
                    " (undone)"
                } else if r.undo.is_some() {
                    " ↶"
                } else {
                    ""
                };
                println!(
                    "#{:<5} {}  {}  {:<9} {:<10} {}{mark}",
                    r.seq,
                    r.ts.with_timezone(&chrono::Local).format("%m-%d %H:%M"),
                    r.tier.label(),
                    format!("{:?}", r.outcome.status).to_lowercase(),
                    r.tool,
                    r.target()
                );
            }
            Ok(())
        }
        ReceiptsCmd::Show { seq } => {
            let r = book
                .find(seq)
                .ok_or_else(|| format!("there's no receipt #{seq}"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&r).map_err(|e| e.to_string())?
            );
            Ok(())
        }
        ReceiptsCmd::Verify => {
            let v = book.verify();
            match v.problem {
                None => {
                    println!("✓ {} receipts: none missing, none changed.", v.count);
                    Ok(())
                }
                Some(p) => Err(format!("✗ after {} good receipts: {p}", v.count)),
            }
        }
    }
}

fn daemon(home: &std::path::Path, cmd: DaemonCmd) -> Result<(), String> {
    use reeve_core::findings::{FindingStore, ObserverStatus};
    use reeve_observer::service;
    match cmd {
        DaemonCmd::Install => {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let custom = std::env::var_os("REEVE_HOME").map(std::path::PathBuf::from);
            println!("{}", service::install(&exe, custom.as_deref())?);
            if exe.starts_with(std::env::var("HOME").unwrap_or_default()) {
                println!(
                    "note: the service runs {} from your home; after rebuilding, restart it (systemctl --user restart reeved).",
                    exe.display()
                );
            }
            Ok(())
        }
        DaemonCmd::Uninstall => {
            println!("{}", service::uninstall()?);
            Ok(())
        }
        DaemonCmd::Status => {
            println!("service: {}", service::state());
            match ObserverStatus::load(home) {
                Some(s) if s.alive(chrono::Utc::now()) => println!(
                    "reeved: running (pid {}), system journal {}, drafter {} (${:.2} today, {} drafts)",
                    s.pid,
                    if s.journal {
                        "readable"
                    } else {
                        "NOT readable (add yourself to the systemd-journal group)"
                    },
                    if s.drafter { "on" } else { "off" },
                    s.drafter_usd_today,
                    s.drafts_today
                ),
                Some(_) => println!("reeved: not running (last heartbeat is old)"),
                None => println!("reeved: has never run"),
            }
            let live: Vec<_> = FindingStore::new(home)
                .list()
                .into_iter()
                .filter(|f| f.is_live())
                .collect();
            if live.is_empty() {
                println!("no open findings");
            }
            for f in live {
                println!("[{}] {}  ({})", f.severity.as_str(), f.title, f.id);
            }
            Ok(())
        }
        DaemonCmd::Run { once } => {
            let rt = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(2)
                .build()
                .map_err(|e| e.to_string())?;
            rt.block_on(reeve_observer::daemon::run_observer(
                home.to_path_buf(),
                once,
            ))
        }
    }
}

/// Plain checks, each one line: ✓ fine, ! worth fixing, ✗ broken.
fn doctor(cfg: &Config, home: &std::path::Path) {
    use std::os::unix::fs::MetadataExt;
    let ok = |m: String| println!("  ✓ {m}");
    let warn = |m: String| println!("  ! {m}");
    let bad = |m: String| println!("  ✗ {m}");
    let has = |b: &str| {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {b}")])
            .output()
            .is_ok_and(|o| o.status.success())
    };
    println!("reeve {}", env!("CARGO_PKG_VERSION"));

    println!("binary");
    match std::env::current_exe() {
        Ok(exe) => match std::fs::metadata(&exe) {
            Ok(m) if m.uid() == 0 && m.mode() & 0o022 == 0 => ok(format!(
                "{} is owned by root and not writable by others",
                exe.display()
            )),
            Ok(_) => warn(format!(
                "{} is writable by you: `sudo reeve root` runs it as root for file edits. Install system-wide (install.sh, or a package) for daily use.",
                exe.display()
            )),
            Err(e) => bad(format!("{}: {e}", exe.display())),
        },
        Err(e) => bad(format!("can't find this binary: {e}")),
    }

    println!("model");
    match cfg.route() {
        Ok((name, _, model)) => {
            ok(format!("{model} via {name}"));
            match config::secret_source(cfg, home, &name) {
                Some(src) => ok(format!("key for {name}: {src}")),
                None => bad(format!(
                    "no key for {name}: run `reeve`, then /providers (or `reeve key set {name}`)"
                )),
            }
        }
        Err(e) => bad(e.to_string()),
    }

    println!("tools");
    for b in ["bash", "setsid", "sudo", "journalctl", "systemctl"] {
        if has(b) {
            ok(b.to_string())
        } else {
            bad(format!("{b} is missing"))
        }
    }
    if !has("notify-send") {
        warn(
            "notify-send is missing: findings won't pop up on the desktop (install libnotify)"
                .into(),
        );
    }
    let distro = reeve_core::distro::Distro::detect();
    match distro.package_block() {
        None => ok(format!("{} package tools", distro.name())),
        Some(why) => warn(why),
    }

    println!("observer");
    let journal = std::process::Command::new("journalctl")
        .args(["--system", "-n", "1", "-q", "--no-pager"])
        .output()
        .is_ok_and(|o| o.status.success() && o.stderr.is_empty());
    if journal {
        ok("the system journal is readable".into())
    } else {
        warn("the system journal isn't readable: add yourself to the systemd-journal group".into())
    }
    let unit = reeve_observer::service::packaged_unit()
        .or_else(|| Some(reeve_observer::service::unit_path()).filter(|p| p.exists()));
    match &unit {
        Some(u) => {
            ok(format!("unit {}", u.display()));
            let runs = reeve_observer::service::unit_exec(u);
            let me = std::env::current_exe().ok();
            match (runs, me) {
                (Some(r), Some(m)) if r != m => warn(format!(
                    "the unit runs {}, not this binary ({})",
                    r.display(),
                    m.display()
                )),
                _ => {}
            }
        }
        None => {
            warn("reeved isn't installed (install.sh does it; or `reeve daemon install`)".into())
        }
    }
    match reeve_core::findings::ObserverStatus::load(home) {
        Some(s) if s.alive(chrono::Utc::now()) => ok(format!("reeved is running (pid {})", s.pid)),
        _ if unit.is_some() => warn(format!(
            "reeved isn't running ({}): systemctl --user start reeved",
            reeve_observer::service::state()
        )),
        _ => {}
    }

    println!("safety");
    let snapper = std::process::Command::new("sh")
        .args([
            "-c",
            "snapper --csvout list-configs 2>/dev/null | tail -n +2 | grep -c ',/$'",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if snapper.parse::<u32>().unwrap_or(0) > 0 {
        ok("snapper covers /: root actions get snapshot pairs".into());
    } else if has("snapper") {
        warn(
            "snapper has no config for /: root actions get no snapshots (ask Reeve to set it up)"
                .into(),
        );
    } else {
        warn("snapper isn't installed: root actions get no snapshots".into());
    }
    let v = ReceiptBook::new(home).verify();
    match v.problem {
        None => ok(format!("{} receipts, chain intact", v.count)),
        Some(p) => bad(format!("receipt chain: {p}")),
    }
}

fn confirm(prompt: &str) -> Result<bool, String> {
    eprint!("{prompt}");
    io::stderr().flush().ok();
    let mut s = String::new();
    io::stdin().read_line(&mut s).map_err(|e| e.to_string())?;
    Ok(matches!(s.trim(), "y" | "Y" | "yes"))
}

fn chrono_now() -> chrono::DateTime<chrono::Local> {
    chrono::Local::now()
}

fn models(
    cfg: &Config,
    home: &std::path::Path,
    connection: Option<String>,
    filter: Option<String>,
) -> Result<(), String> {
    let name = connection.unwrap_or_else(|| cfg.default_connection.clone());
    let conn = cfg
        .connections
        .get(&name)
        .ok_or_else(|| format!("no connection named {name:?}"))?;
    // OpenRouter's catalog is public; other servers want the key.
    let key = config::resolve_secret(cfg, home, &name).unwrap_or_default();
    let provider = HttpProvider::new(conn, key);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let mut list = rt
        .block_on(provider.list_models())
        .map_err(|e| e.to_string())?;
    if let Some(f) = &filter {
        let f = f.to_ascii_lowercase();
        list.retain(|m| m.id.to_ascii_lowercase().contains(&f));
    }
    list.sort_by(|a, b| a.id.cmp(&b.id));
    let mut book = PriceBook::from_config(cfg);
    book.ingest(&list);
    let width = list.iter().map(|m| m.id.len()).max().unwrap_or(10).min(60);
    for m in &list {
        let cache = match (m.cache_read_per_million, m.cache_write_per_million) {
            (Some(r), Some(w)) => format!("  cache ${r:.3}r/${w:.3}w"),
            (Some(r), None) => format!("  cache ${r:.3}r"),
            _ => String::new(),
        };
        let tools = if m.tools == Some(false) {
            "  (no tools)"
        } else {
            ""
        };
        println!(
            "{:<width$}  {}{cache}{tools}",
            m.id,
            format_rates(book.rates(&m.id))
        );
    }
    eprintln!("{} models on {name}", list.len());
    Ok(())
}

/// A secret from stdin: hidden when typed at a terminal, whole when piped.
fn read_secret(prompt: &str) -> Result<String, String> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        let mut s = String::new();
        stdin
            .lock()
            .read_to_string(&mut s)
            .map_err(|e| e.to_string())?;
        return Ok(s.trim().to_string());
    }
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    eprint!("{prompt}");
    io::stderr().flush().ok();
    crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let mut s = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err("cancelled".to_string());
                }
                KeyCode::Backspace => {
                    s.pop();
                }
                KeyCode::Char(c) => s.push(c),
                _ => {}
            },
            Ok(Event::Paste(p)) => s.push_str(&p),
            Ok(_) => {}
            Err(e) => break Err(e.to_string()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    result.map(|()| s.trim().to_string())
}
