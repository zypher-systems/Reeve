//! The tools Reeve's model can call. Every call goes through the same gate:
//! [`prepare`] (parse, assess, preview) → approval → [`execute`] → receipt.

mod fs;
mod shell;

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
}

impl ToolCtx {
    /// For this machine and user.
    pub fn new(reeve_home: PathBuf, secret_env: Vec<String>) -> Self {
        Self {
            undo: UndoStore::new(&reeve_home),
            paths: PathCtx::current(reeve_home),
            secret_env,
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
    /// Key for "allow for this session" (T1 only).
    pub rule: Option<String>,
    call: Call,
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
    let call = match tool {
        "fs_read" => Call::Read(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_list" => Call::List(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_search" => Call::Search(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_stat" => Call::Stat(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_write" => Call::Write(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_edit" => Call::Edit(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_move" => Call::Move(serde_json::from_value(args.clone()).map_err(parse)?),
        "fs_delete" => Call::Delete(serde_json::from_value(args.clone()).map_err(parse)?),
        "shell" => Call::Shell(serde_json::from_value(args.clone()).map_err(parse)?),
        other => return Err(format!("there is no tool named {other}")),
    };
    let (assessment, summary, preview, undoable, rule) = match &call {
        Call::Read(a) => fs::plan_read(ctx, a),
        Call::List(a) => fs::plan_list(ctx, a),
        Call::Search(a) => fs::plan_search(ctx, a),
        Call::Stat(a) => fs::plan_stat(ctx, a),
        Call::Write(a) => fs::plan_write(ctx, a),
        Call::Edit(a) => fs::plan_edit(ctx, a)?,
        Call::Move(a) => fs::plan_move(ctx, a),
        Call::Delete(a) => fs::plan_delete(ctx, a),
        Call::Shell(a) => shell::plan(ctx, a),
    };
    Ok(Plan {
        tool: tool.to_string(),
        args,
        assessment,
        summary,
        preview,
        why,
        undoable,
        rule,
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
        Call::Write(a) => fs::write(ctx, a),
        Call::Edit(a) => fs::edit(ctx, a),
        Call::Move(a) => fs::mv(ctx, a),
        Call::Delete(a) => fs::delete(ctx, a),
        Call::Shell(a) => shell::run(ctx, a).await,
    }
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
            "shell",
            "Run a bash command. Non-interactive: no terminal, no prompts (use -y flags), pagers off. Output is capped. Root commands (sudo) are not available yet and will fail.",
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
