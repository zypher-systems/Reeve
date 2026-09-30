//! The tools Reeve's model can call. Every call goes through the same gate:
//! [`prepare`] (parse, assess, preview) → approval → [`execute`] → receipt.

mod fs;
mod mem;
pub mod obs;
pub mod order;
mod shell;
mod sys;

use std::path::PathBuf;

use serde::Deserialize;
use serde_json::{Value, json};

use crate::diff::FileDiff;
use crate::llm::ToolSpec;
use crate::policy::{Assessment, PathCtx};
use crate::receipts::Outcome;
use crate::undo::{Undo, UndoStore};

/// What tools need to know.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    /// Path classification (home, cwd, uid, running kernel).
    pub paths: PathCtx,
    /// The undo store.
    pub undo: UndoStore,
    /// Environment variables to strip from commands (API keys).
    pub secret_env: Vec<String>,
    /// Root through the TUI's password prompt. `None`: sudo fails fast.
    pub askpass: Option<crate::sudo::Askpass>,
    /// Run sudo on the terminal instead (the CLI).
    pub interactive_sudo: bool,
    /// This binary, for `sudo reeve root …`.
    pub exe: PathBuf,
    /// Which distribution.
    pub distro: crate::distro::Distro,
    /// Snapper config for `/`, once looked up.
    pub snapper: std::sync::Arc<tokio::sync::OnceCell<Option<String>>>,
    /// Reeve's memory.
    pub memory: crate::memory::Memory,
    /// Rules from the owner's confirmed preferences.
    pub rules: Vec<crate::memory::Rule>,
    /// Session writing memories.
    pub session: String,
    /// `Fedora 44`, for tagging memories.
    pub os: String,
}

impl ToolCtx {
    /// For this machine and user.
    pub fn new(reeve_home: PathBuf, secret_env: Vec<String>) -> Self {
        let reeve_home_for_memory = reeve_home.clone();
        Self {
            undo: UndoStore::new(&reeve_home),
            paths: PathCtx::current(reeve_home),
            secret_env,
            askpass: None,
            interactive_sudo: false,
            exe: std::env::current_exe().unwrap_or_else(|_| "reeve".into()),
            distro: crate::distro::Distro::detect(),
            snapper: Default::default(),
            memory: crate::memory::Memory::new(&reeve_home_for_memory),
            rules: Vec::new(),
            session: String::new(),
            os: crate::distro::os_label(),
        }
    }
}

/// A parsed, assessed call waiting for approval.
#[derive(Debug, Clone)]
pub struct Plan {
    /// Tool name.
    pub tool: String,
    /// Arguments as the model sent them.
    pub args: Value,
    /// The policy's verdict.
    pub assessment: Assessment,
    /// One line: the command, or the path and what happens to it.
    pub summary: String,
    /// For file changes: what will change.
    pub preview: Option<FileDiff>,
    /// The model's stated reason.
    pub why: Option<String>,
    /// Whether the receipt will carry an undo.
    pub undoable: bool,
    /// What "allow for this session" remembers (T1 only): every one must
    /// already be allowed for the action to run without asking.
    pub rules: Vec<String>,
    /// What it means, in plain words, for the approval card (standing
    /// orders: when it runs, what it may do, what it spends).
    pub details: Vec<String>,
    call: Call,
}

impl Plan {
    /// The command it runs, for shell and system tools.
    pub fn command(&self) -> Option<String> {
        match &self.call {
            Call::Shell(_) => self
                .args
                .get("command")
                .and_then(Value::as_str)
                .map(|c| c.trim().to_string()),
            Call::Sys(c) => Some(c.command.clone()),
            _ => None,
        }
    }

    /// The paths it changes, resolved, for file tools.
    pub fn paths(&self, ctx: &ToolCtx) -> Vec<String> {
        match &self.call {
            Call::Write(_) | Call::Edit(_) | Call::Move(_) | Call::Delete(_) => {
                ["path", "from", "to"]
                    .iter()
                    .filter_map(|k| self.args.get(*k).and_then(Value::as_str))
                    .map(|p| ctx.paths.resolve(p).to_string_lossy().into_owned())
                    .collect()
            }
            _ => Vec::new(),
        }
    }
}

/// What running a plan produced.
#[derive(Debug, Clone)]
pub struct Executed {
    /// Text for the model.
    pub output: String,
    /// For the receipt.
    pub outcome: Outcome,
    /// How to reverse it.
    pub undo: Option<Undo>,
    /// What changed, for the chat.
    pub diff: Option<FileDiff>,
}

#[derive(Debug, Clone)]
enum Call {
    Read(fs::ReadArgs),
    List(fs::ListArgs),
    Search(fs::SearchArgs),
    Stat(fs::PathArg),
    Write(fs::WriteArgs),
    Edit(fs::EditArgs),
    Move(fs::MoveArgs),
    Delete(fs::DeleteArgs),
    Shell(shell::ShellArgs),
    Sys(sys::SysCall),
    MemSearch(mem::SearchArgs),
    MemRead(mem::ReadArgs),
    MemWrite(mem::WriteArgs),
    Findings(obs::Args),
    Order(Box<order::Planned>),
}

#[derive(Deserialize)]
struct Why {
    #[serde(default)]
    reason: Option<String>,
}

/// Parse and assess a call. `Err` is a message for the model (bad arguments).
pub fn prepare(ctx: &ToolCtx, tool: &str, raw_args: &str) -> Result<Plan, String> {
    let args: Value = if raw_args.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(raw_args).map_err(|e| format!("arguments aren't valid JSON: {e}"))?
    };
    let why = serde_json::from_value::<Why>(args.clone())
        .ok()
        .and_then(|w| w.reason);
    let parse = |e: serde_json::Error| format!("bad arguments for {tool}: {e}");
    let call = if let Some(c) = sys::parse(ctx, tool, &args)? {
        Call::Sys(c)
    } else {
        match tool {
            "fs_read" => Call::Read(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_list" => Call::List(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_search" => Call::Search(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_stat" => Call::Stat(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_write" => Call::Write(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_edit" => Call::Edit(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_move" => Call::Move(serde_json::from_value(args.clone()).map_err(parse)?),
            "fs_delete" => Call::Delete(serde_json::from_value(args.clone()).map_err(parse)?),
            "shell" => Call::Shell(serde_json::from_value(args.clone()).map_err(parse)?),
            "memory_search" => {
                Call::MemSearch(serde_json::from_value(args.clone()).map_err(parse)?)
            }
            "memory_read" => Call::MemRead(serde_json::from_value(args.clone()).map_err(parse)?),
            "memory_write" => Call::MemWrite(serde_json::from_value(args.clone()).map_err(parse)?),
            "findings" => Call::Findings(serde_json::from_value(args.clone()).map_err(parse)?),
            "order_save" => Call::Order(Box::new(order::prepare_save(
                ctx,
                &serde_json::from_value(args.clone()).map_err(parse)?,
            )?)),
            "order_delete" => Call::Order(Box::new(order::prepare_delete(
                ctx,
                &serde_json::from_value(args.clone()).map_err(parse)?,
            )?)),
            other => return Err(format!("there is no tool named {other}")),
        }
    };
    let (assessment, summary, preview, undoable, rules) = match &call {
        Call::Read(a) => fs::plan_read(ctx, a),
        Call::List(a) => fs::plan_list(ctx, a),
        Call::Search(a) => fs::plan_search(ctx, a),
        Call::Stat(a) => fs::plan_stat(ctx, a),
        Call::Write(a) => fs::plan_write(ctx, a),
        Call::Edit(a) => fs::plan_edit(ctx, a)?,
        Call::Move(a) => fs::plan_move(ctx, a),
        Call::Delete(a) => fs::plan_delete(ctx, a),
        Call::Shell(a) => shell::plan(ctx, a),
        Call::Sys(c) => sys::plan(ctx, c),
        Call::MemSearch(a) => (
            Assessment::new(crate::policy::Tier::T0),
            format!("search memory for {}", a.query()),
            None,
            false,
            Vec::new(),
        ),
        Call::MemRead(a) => (
            Assessment::new(crate::policy::Tier::T0),
            format!("read memory {}", a.id()),
            None,
            false,
            Vec::new(),
        ),
        Call::MemWrite(a) => (
            Assessment::new(crate::policy::Tier::T0),
            a.summary(),
            None,
            false,
            Vec::new(),
        ),
        Call::Findings(a) => (
            Assessment::new(crate::policy::Tier::T0),
            a.summary(),
            None,
            false,
            Vec::new(),
        ),
        Call::Order(p) => order::plan(p),
    };
    let details = match &call {
        Call::Order(p) => order::details(p),
        _ => Vec::new(),
    };
    let mut assessment = assessment;
    apply_rules(ctx, &call, &args, &mut assessment);
    Ok(Plan {
        tool: tool.to_string(),
        args,
        assessment,
        summary,
        preview,
        why,
        undoable,
        rules,
        details,
        call,
    })
}

/// Run an approved plan.
pub async fn execute(ctx: &ToolCtx, plan: &Plan) -> Executed {
    match &plan.call {
        Call::Read(a) => fs::read(ctx, a),
        Call::List(a) => fs::list(ctx, a),
        Call::Search(a) => fs::search(ctx, a).await,
        Call::Stat(a) => fs::stat(ctx, a),
        Call::Write(a) => fs::write(ctx, a).await,
        Call::Edit(a) => fs::edit(ctx, a).await,
        Call::Move(a) => fs::mv(ctx, a),
        Call::Delete(a) => fs::delete(ctx, a).await,
        Call::Shell(a) => shell::run(ctx, a, plan.assessment.sudo).await,
        Call::Sys(c) => sys::run(ctx, c, plan.assessment.sudo).await,
        Call::MemSearch(a) => mem::search(ctx, a),
        Call::MemRead(a) => mem::read(ctx, a),
        Call::MemWrite(a) => mem::write(ctx, a),
        Call::Findings(a) => obs::run(ctx, a),
        Call::Order(p) => order::run(ctx, p),
    }
}

/// Refuse changes the owner's confirmed preferences forbid.
fn apply_rules(ctx: &ToolCtx, call: &Call, args: &Value, a: &mut Assessment) {
    if ctx.rules.is_empty() || a.tier == crate::policy::Tier::T0 {
        return;
    }
    let paths: Vec<PathBuf> = ["path", "from", "to"]
        .iter()
        .filter_map(|k| args.get(*k).and_then(Value::as_str))
        .map(|p| ctx.paths.resolve(p))
        .collect();
    let command = match call {
        Call::Shell(_) => args
            .get("command")
            .and_then(Value::as_str)
            .map(String::from),
        Call::Sys(c) => Some(c.command.clone()),
        _ => None,
    };
    for r in &ctx.rules {
        if let Some(why) = r.blocks(&paths, command.as_deref()) {
            a.refuse(why);
            return;
        }
    }
}

/// Run a short command and return its stdout, or `None` if it failed.
pub(crate) async fn shell_exec(ctx: &ToolCtx, command: &str, sudo: bool) -> Option<String> {
    let out = shell::run_command(
        ctx,
        shell::RunSpec {
            command,
            cwd: &ctx.paths.home,
            timeout: std::time::Duration::from_secs(60),
            sudo,
            stdin: None,
        },
    )
    .await;
    (out.code == Some(0)).then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Run a read-only command (a check): its exit code and its output, stdout
/// then stderr.
pub(crate) async fn shell_exec_status(ctx: &ToolCtx, command: &str) -> (Option<i32>, String) {
    let out = shell::run_command(
        ctx,
        shell::RunSpec {
            command,
            cwd: &ctx.paths.home,
            timeout: std::time::Duration::from_secs(30),
            sudo: false,
            stdin: None,
        },
    )
    .await;
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    if let Some(f) = out.failure {
        text.push_str(&f);
    }
    (out.code, text)
}

/// Undo receipt `seq` (files, root files, packages, or a unit) and write a
/// receipt for the undo. May need sudo; with the TUI, the password prompt
/// appears there.
pub async fn undo_receipt(
    ctx: &ToolCtx,
    book: &crate::receipts::ReceiptBook,
    seq: u64,
    session: &str,
) -> crate::Result<crate::receipts::Receipt> {
    undo_receipt_by(ctx, book, seq, session, "user").await
}

/// [`undo_receipt`], recorded as approved by `by` (`txn:<id>` for a rollback).
pub async fn undo_receipt_by(
    ctx: &ToolCtx,
    book: &crate::receipts::ReceiptBook,
    seq: u64,
    session: &str,
    by: &str,
) -> crate::Result<crate::receipts::Receipt> {
    let (target, undo) = book.undo_target(seq)?;
    let root_files = matches!(&undo, Undo::Files { changes } if changes.iter().any(|c| c.root));
    let result = match &undo {
        Undo::Packages { .. } | Undo::Unit { .. } => sys::revert(ctx, &undo).await,
        _ if root_files => fs::root_revert(ctx, &undo).await,
        _ => ctx.undo.revert(&undo).map_err(|e| e.to_string()),
    };
    book.record_undo(&target, session, by, result)
}

/// Arguments as they go into a receipt: file contents become size and hash.
pub fn receipt_args(args: &Value) -> Value {
    let mut v = args.clone();
    if let Some(obj) = v.as_object_mut() {
        obj.remove("reason");
        for key in ["content", "old", "new"] {
            if let Some(Value::String(s)) = obj.get(key) {
                let digest = json!({
                    "bytes": s.len(),
                    "sha256": crate::undo::sha256_hex(s.as_bytes()),
                });
                obj.insert(key.to_string(), digest);
            }
        }
    }
    v
}

fn reason_param() -> Value {
    json!({"type": "string", "description": "One short line: why this action. Shown to the owner and kept in the receipt."})
}

fn spec(name: &str, description: &str, mut params: Value, required: &[&str]) -> ToolSpec {
    params["reason"] = reason_param();
    ToolSpec {
        name: name.into(),
        description: description.into(),
        parameters: json!({
            "type": "object",
            "properties": params,
            "required": required,
            "additionalProperties": false,
        }),
    }
}

/// The tools, as advertised to the model.
pub fn specs() -> Vec<ToolSpec> {
    let path = json!({"type": "string", "description": "Absolute path, or ~/…"});
    vec![
        spec(
            "fs_read",
            "Read a text file with line numbers. Use offset/limit for long files.",
            json!({"path": path, "offset": {"type": "integer", "description": "First line (1-based)"}, "limit": {"type": "integer", "description": "Lines to read (default 400, max 2000)"}}),
            &["path"],
        ),
        spec(
            "fs_list",
            "List a directory: type, size, modified date, name. Hidden files included.",
            json!({"path": path, "depth": {"type": "integer", "description": "1–4 levels (default 1)"}}),
            &["path"],
        ),
        spec(
            "fs_search",
            "Search file contents under a directory with a regex, or find files by name glob when pattern is empty. Skips /proc, /sys, /dev, binaries, and secrets.",
            json!({"path": path, "pattern": {"type": "string", "description": "Regex over lines; empty to match names only"}, "glob": {"type": "string", "description": "File name glob, e.g. *.conf"}, "ignore_case": {"type": "boolean"}, "max_results": {"type": "integer", "description": "Default 100, max 500"}}),
            &["path"],
        ),
        spec(
            "fs_stat",
            "Metadata for a path: type, size, mode, owner, modified time, symlink target.",
            json!({"path": path}),
            &["path"],
        ),
        spec(
            "fs_write",
            "Create or replace a whole file. The old version is kept for undo. Prefer fs_edit for small changes.",
            json!({"path": path, "content": {"type": "string"}, "create_dirs": {"type": "boolean", "description": "Create missing parent directories"}}),
            &["path", "content"],
        ),
        spec(
            "fs_edit",
            "Replace an exact piece of text in a file. `old` must appear exactly once unless replace_all is set. The old version is kept for undo.",
            json!({"path": path, "old": {"type": "string"}, "new": {"type": "string"}, "replace_all": {"type": "boolean"}}),
            &["path", "old", "new"],
        ),
        spec(
            "fs_move",
            "Move or rename a file or directory. Refuses to overwrite unless overwrite is set.",
            json!({"from": path, "to": path, "overwrite": {"type": "boolean"}}),
            &["from", "to"],
        ),
        spec(
            "fs_delete",
            "Delete a file, or a directory with recursive=true. Contents are kept for undo (up to 5000 files / 64 MB).",
            json!({"path": path, "recursive": {"type": "boolean"}}),
            &["path"],
        ),
        spec(
            "sys_info",
            "A summary of the machine: OS, kernel (and newer installed kernels), memory and swap, disks, failed units, SELinux, snapper.",
            json!({}),
            &[],
        ),
        spec(
            "pkg_search",
            "Search available packages by name and summary.",
            json!({"query": {"type": "string"}}),
            &["query"],
        ),
        spec(
            "pkg_info",
            "Details of one package, installed or available.",
            json!({"name": {"type": "string"}}),
            &["name"],
        ),
        spec(
            "pkg_list",
            "List packages: installed (name, version, size), user (explicitly installed), leaves (nothing depends on them), or updates (pending).",
            json!({"which": {"type": "string", "enum": ["installed", "user", "leaves", "updates"]}, "filter": {"type": "string", "description": "Case-insensitive substring"}}),
            &[],
        ),
        spec(
            "pkg_install",
            "Install packages (needs root). The transaction is recorded so it can be rolled back.",
            json!({"packages": {"type": "array", "items": {"type": "string"}}}),
            &["packages"],
        ),
        spec(
            "pkg_remove",
            "Remove packages (needs root). The transaction is recorded so it can be rolled back.",
            json!({"packages": {"type": "array", "items": {"type": "string"}}}),
            &["packages"],
        ),
        spec(
            "pkg_upgrade",
            "Upgrade the named packages, or everything when the list is empty (needs root). Recorded for rollback.",
            json!({"packages": {"type": "array", "items": {"type": "string"}}}),
            &[],
        ),
        spec(
            "pkg_history",
            "Recent package transactions.",
            json!({"limit": {"type": "integer"}}),
            &[],
        ),
        spec(
            "svc_status",
            "Status of a systemd unit, with its recent log lines.",
            json!({"unit": {"type": "string"}, "user": {"type": "boolean", "description": "A user unit (systemctl --user)"}}),
            &["unit"],
        ),
        spec(
            "svc_list",
            "List units: failed (default), running, enabled, timers, or all.",
            json!({"state": {"type": "string", "enum": ["failed", "running", "enabled", "timers", "all"]}, "user": {"type": "boolean"}}),
            &[],
        ),
        spec(
            "svc_control",
            "Start, stop, restart, reload, enable, disable, mask, or unmask a unit. System units need root. The unit's previous state is recorded so it can be put back.",
            json!({"unit": {"type": "string"}, "action": {"type": "string", "enum": ["start", "stop", "restart", "reload", "enable", "disable", "mask", "unmask", "enable --now", "disable --now"]}, "user": {"type": "boolean"}}),
            &["unit", "action"],
        ),
        spec(
            "logs_query",
            "Read the systemd journal. Filter by unit, priority (emerg…debug, or a range like err..warning), time (since/until: 'today', '-1h', '2026-09-26 10:00'), boot (0 current, -1 previous), kernel messages, or a grep pattern.",
            json!({"unit": {"type": "string"}, "user": {"type": "boolean"}, "priority": {"type": "string"}, "since": {"type": "string"}, "until": {"type": "string"}, "boot": {"type": "integer"}, "kernel": {"type": "boolean"}, "grep": {"type": "string"}, "limit": {"type": "integer", "description": "Newest lines (default 200, max 2000)"}}),
            &[],
        ),
        spec(
            "proc_list",
            "Top processes by CPU or memory.",
            json!({"sort": {"type": "string", "enum": ["cpu", "mem"]}, "filter": {"type": "string"}, "limit": {"type": "integer"}}),
            &[],
        ),
        spec(
            "proc_signal",
            "Send a signal to a process (TERM by default). Other users' processes need root.",
            json!({"pid": {"type": "integer"}, "signal": {"type": "string", "enum": ["TERM", "KILL", "HUP", "INT", "STOP", "CONT", "USR1", "USR2"]}}),
            &["pid"],
        ),
        spec(
            "findings",
            "What reeved, the background observer, has noticed: open findings (crashes, full disks, failed units, journal spikes, pending updates), or one finding's details and evidence by id.",
            json!({"id": {"type": "string"}, "include_closed": {"type": "boolean", "description": "Include resolved and dismissed ones"}}),
            &[],
        ),
        spec(
            "order_save",
            &format!(
                "Make a standing order, or change one (give its id): work reeved, the background \
observer, does on its own from now on, on a schedule and/or when it finds something. Use it when the \
owner wants something done regularly or whenever something happens. The owner approves it once, \
seeing it in plain words; after that it runs unattended within exactly what the order allows. \
Write the task for yourself running with nobody there: what to check first, what to change, what \
to report, and when to do nothing. Scope it narrowly: the commands it may run (`*` is one word; \
reads never need listing) and the files it may change. Leave the limits out and they fit the \
schedule. See existing orders with `reeve orders list` / `reeve orders show <id>`. Finding kinds: {}.",
                crate::orders::FINDING_KINDS
                    .iter()
                    .map(|(k, what)| format!("{k} ({what})"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            json!({
                "id": {"type": "string", "description": "An existing order's id, to change it; leave out for a new one"},
                "name": {"type": "string", "description": "Short, for the list: \"Clear the thumbnail cache\""},
                "task": {"type": "string", "description": "What to do, in plain words, for yourself running unattended"},
                "schedule": {"type": "string", "description": "hourly, every 30m (at least 5m), every 6h, daily 03:00, or weekly sun 03:00 (local time); \"none\" removes it"},
                "findings": {"type": "array", "items": {"type": "string"}, "description": "Finding ids that start a run; * matches anything: disk-full:*, unit-failed:bluetooth*"},
                "min_severity": {"type": "string", "enum": ["info", "warning", "critical"]},
                "max_tier": {"type": "string", "enum": ["T0", "T1", "T2"], "description": "The most it may do: T0 look and report, T1 your files and user services, T2 the system (sudo). Default: what its commands need"},
                "tools": {"type": "array", "items": {"type": "string", "enum": order::CHANGE_TOOLS}, "description": "Tools it may change things with; empty: any, still limited by commands and paths"},
                "commands": {"type": "array", "items": {"type": "string"}, "description": "Commands it may run to change things, as globs: \"sudo journalctl --vacuum-size=*\""},
                "paths": {"type": "array", "items": {"type": "string"}, "description": "Files it may change, as globs: \"~/.cache/thumbnails/**\""},
                "per_run_usd": {"type": "number", "description": "Most one run may spend (default 0.05)"},
                "runs_per_day": {"type": "integer"},
                "cooldown_hours": {"type": "number"},
                "notify": {"type": "string", "enum": ["never", "after", "before"], "description": "never: a popup only when it needs the owner (default); after: after every run; before: at the start too"},
                "enabled": {"type": "boolean", "description": "Default true for a new order"}
            }),
            &[],
        ),
        spec(
            "order_delete",
            "Delete a standing order the owner no longer wants (asks them; it can be undone). To pause one instead, order_save it with enabled false.",
            json!({"id": {"type": "string"}}),
            &["id"],
        ),
        spec(
            "change_begin",
            "Open a verified change before fixing something: the goal, and checks Reeve will run after your changes. If any check fails, Reeve undoes every change made until change_commit. Kinds: unit_active {unit, user?}; journal_quiet {unit, max?, user?} (no more than max errors after the last change); disk_below {mount, percent}; command {command, expect?} (read-only, no sudo; passes on exit 0 and, with expect, when the output contains it).",
            json!({
                "goal": {"type": "string", "description": "What the change fixes, in a line"},
                "checks": {"type": "array", "items": {
                    "type": "object",
                    "properties": {
                        "kind": {"type": "string", "enum": ["unit_active", "journal_quiet", "disk_below", "command"]},
                        "unit": {"type": "string"},
                        "user": {"type": "boolean"},
                        "max": {"type": "integer"},
                        "mount": {"type": "string"},
                        "percent": {"type": "integer"},
                        "command": {"type": "string"},
                        "expect": {"type": "string"}
                    },
                    "required": ["kind"]
                }},
                "wait_secs": {"type": "integer", "description": "Let things settle before checking (default 3, max 120)"}
            }),
            &["goal", "checks"],
        ),
        spec(
            "change_commit",
            "Finish the open verified change: Reeve waits, runs its checks, and keeps the change if they pass or rolls it back if any fails. The result says which.",
            json!({}),
            &[],
        ),
        spec(
            "memory_search",
            "Search Reeve's memory of this machine: facts, runbooks (past fixes and how often they worked), and the owner's preferences. Do this before diagnosing a problem from scratch.",
            json!({"query": {"type": "string"}, "layer": {"type": "string", "enum": ["fact", "runbook", "preference"]}}),
            &["query"],
        ),
        spec(
            "memory_read",
            "Read one memory in full.",
            json!({"id": {"type": "string"}}),
            &["id"],
        ),
        spec(
            "memory_write",
            "Remember something for future sessions. fact: true about this machine, seen in tool output (never secrets). runbook: a problem and the steps that fixed it; write it only after checking the fix worked, and record later uses with outcome worked/failed (give its id). preference: how the owner wants things done, in their words; it stays pending until they confirm it. A rule (preferences only) is `deny-path: <glob>` or `deny-command: <glob>`.",
            json!({"layer": {"type": "string", "enum": ["fact", "runbook", "preference"]}, "title": {"type": "string"}, "body": {"type": "string"}, "tags": {"type": "array", "items": {"type": "string"}}, "id": {"type": "string", "description": "Update this memory instead of adding one"}, "rule": {"type": "string"}, "outcome": {"type": "string", "enum": ["worked", "failed"]}}),
            &["layer"],
        ),
        spec(
            "shell",
            "Run a bash command. There is no terminal: nothing can prompt (pass -y and similar), pagers are off, output is capped. sudo works: the owner approves the action and types their password into Reeve. Prefer the pkg_, svc_, logs_, and proc_ tools when they fit.",
            json!({"command": {"type": "string"}, "cwd": {"type": "string", "description": "Working directory (default: home)"}, "timeout_secs": {"type": "integer", "description": "Default 120, max 600"}}),
            &["command"],
        ),
    ]
}

/// Keep the head and tail of long output.
pub(crate) fn cap(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_string();
    }
    let head_end = floor_char(text, max * 2 / 3);
    let tail_start = ceil_char(text, text.len() - max / 3);
    let skipped = text[head_end..tail_start].lines().count();
    format!(
        "{}\n… [{skipped} lines omitted] …\n{}",
        &text[..head_end],
        &text[tail_start..]
    )
}

fn floor_char(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_never_hold_file_contents() {
        let args = json!({"path": "~/.bashrc", "content": "export TOKEN=hunter2\n", "reason": "x"});
        let r = receipt_args(&args);
        let s = r.to_string();
        assert!(!s.contains("hunter2") && !s.contains("reason"), "{s}");
        assert_eq!(r["content"]["bytes"], 21);
    }

    #[test]
    fn every_tool_takes_a_reason() {
        for s in specs() {
            assert!(
                s.parameters["properties"]["reason"].is_object(),
                "{}",
                s.name
            );
        }
    }

    #[test]
    fn capping_keeps_both_ends() {
        let text: String = (0..1000).map(|i| format!("line {i}\n")).collect();
        let c = cap(&text, 400);
        assert!(c.starts_with("line 0") && c.trim_end().ends_with("line 999"));
        assert!(c.contains("omitted"));
        assert!(!cap("é".repeat(10).as_str(), 5).is_empty());
    }

    #[test]
    fn bad_calls_explain_themselves() {
        let ctx = ToolCtx::new(tempfile::tempdir().unwrap().path().into(), vec![]);
        assert!(prepare(&ctx, "nope", "{}").unwrap_err().contains("no tool"));
        assert!(prepare(&ctx, "fs_read", "{").unwrap_err().contains("JSON"));
        assert!(
            prepare(&ctx, "fs_read", "{\"pth\":1}")
                .unwrap_err()
                .contains("bad arguments")
        );
    }
}
