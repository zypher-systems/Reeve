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
    /// List, inspect, or verify receipts.
    Receipts {
        #[command(subcommand)]
        cmd: Option<ReceiptsCmd>,
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
