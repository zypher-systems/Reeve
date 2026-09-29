//! File tools. Paths may be anywhere on the machine: Reeve is not bound to
//! the directory it started in.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::shell::{RunSpec, run_command};
use super::{Executed, ToolCtx, cap};
use crate::diff::{FileDiff, diff};
use crate::distro::quote;
use crate::policy::{self, Assessment, PathClass, Tier};
use crate::receipts::{Outcome, Status};
use crate::root::{RootOp, RootReply};
use crate::undo::{FileChange, Undo, write_atomic};

const MAX_READ: usize = 64 * 1024;
const MAX_OUTPUT: usize = 48 * 1024;
const DIFF_ROWS: usize = 400;

type Planned = (Assessment, String, Option<FileDiff>, bool, Vec<String>);

#[derive(Debug, Clone, Deserialize)]
pub(super) struct PathArg {
    path: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ReadArgs {
    path: String,
    #[serde(default)]
    offset: Option<usize>,
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ListArgs {
    path: String,
    #[serde(default)]
    depth: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SearchArgs {
    path: String,
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    glob: Option<String>,
    #[serde(default)]
    ignore_case: bool,
    #[serde(default)]
    max_results: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct WriteArgs {
    path: String,
    content: String,
    #[serde(default)]
    create_dirs: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct EditArgs {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct MoveArgs {
    from: String,
    to: String,
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct DeleteArgs {
    path: String,
    #[serde(default)]
    recursive: bool,
}

fn show(ctx: &ToolCtx, p: &Path) -> String {
    match p.strip_prefix(&ctx.paths.home) {
        Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
        Ok(rel) => format!("~/{}", rel.display()),
        Err(_) => p.display().to_string(),
    }
}

/// What "allow for this session" remembers for a T1 file change: what it
/// does and where (`write:~/notes`).
fn session_rules(a: &Assessment) -> Vec<String> {
    a.session_keys().unwrap_or_default()
}

fn ok(output: String, summary: String) -> Executed {
    Executed {
        output,
        outcome: Outcome {
            status: Status::Ok,
            exit: None,
            summary,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    }
}

fn fail(msg: String) -> Executed {
    Executed {
        output: format!("error: {msg}"),
        outcome: Outcome {
            status: Status::Error,
            exit: None,
            summary: msg,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    }
}

fn io_msg(p: &Path, e: &std::io::Error) -> String {
    match e.kind() {
        ErrorKind::PermissionDenied => format!(
            "permission denied on {} — it belongs to another user (likely root). Root file changes \
             aren't available yet: tell the owner what to change and the exact command to run.",
            p.display()
        ),
        ErrorKind::NotFound => format!("{} doesn't exist", p.display()),
        _ => format!("{}: {e}", p.display()),
    }
}

// ── plans ───────────────────────────────────────────────────────────────────

pub(super) fn plan_read(ctx: &ToolCtx, a: &ReadArgs) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    (
        policy::read(&ctx.paths, &a.path),
        format!("read {}", show(ctx, &p)),
        None,
        false,
        Vec::new(),
    )
}

pub(super) fn plan_list(ctx: &ToolCtx, a: &ListArgs) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    (
        policy::read(&ctx.paths, &a.path),
        format!("list {}", show(ctx, &p)),
        None,
        false,
        Vec::new(),
    )
}

pub(super) fn plan_search(ctx: &ToolCtx, a: &SearchArgs) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    let what = if a.pattern.is_empty() {
        format!(
            "find {} in {}",
            a.glob.as_deref().unwrap_or("*"),
            show(ctx, &p)
        )
    } else {
        format!("search {} for /{}/", show(ctx, &p), a.pattern)
    };
    // The walk itself skips secrets, so a search is a plain read of its root.
    let mut asm = policy::read(&ctx.paths, &a.path);
    if matches!(ctx.paths.classify(&p), PathClass::Sensitive(_)) {
        asm.tier = Tier::T3;
    }
    (asm, what, None, false, Vec::new())
}

pub(super) fn plan_stat(ctx: &ToolCtx, a: &PathArg) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    // Metadata isn't content: only Reeve's keys are off limits.
    let mut asm = Assessment::new(Tier::T0);
    if ctx.paths.classify(&p) == PathClass::ReeveKeys {
        asm.refuse("Reeve's own API keys are never readable by tools");
    }
    (
        asm,
        format!("stat {}", show(ctx, &p)),
        None,
        false,
        Vec::new(),
    )
}

/// You can't write it yourself: it goes through `sudo reeve root`.
pub(crate) fn needs_root(p: &Path) -> bool {
    use rustix::fs::{Access, access};
    let mut probe = p;
    loop {
        if fs::symlink_metadata(probe).is_ok() {
            return access(probe, Access::WRITE_OK).is_err();
        }
        match probe.parent() {
            Some(parent) => probe = parent,
            None => return true,
        }
    }
}

fn as_root(asm: &mut Assessment, p: &Path) -> bool {
    let root = asm.deny.is_none() && needs_root(p);
    if root {
        asm.sudo = true;
        asm.raise(Tier::T2, "needs root (you'll be asked for your password)");
    }
    root
}

pub(super) fn plan_write(ctx: &ToolCtx, a: &WriteArgs) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    let mut asm = policy::write(&ctx.paths, &a.path);
    as_root(&mut asm, &p);
    let old = fs::read_to_string(&p).ok();
    let preview = diff(old.as_deref().unwrap_or(""), &a.content, DIFF_ROWS);
    let verb = if old.is_some() || p.exists() {
        "rewrite"
    } else {
        "create"
    };
    let rule = session_rules(&asm);
    (
        asm,
        format!("{verb} {}", show(ctx, &p)),
        Some(preview),
        true,
        rule,
    )
}

pub(super) fn plan_edit(ctx: &ToolCtx, a: &EditArgs) -> Result<Planned, String> {
    let p = ctx.paths.resolve(&a.path);
    let mut asm = policy::write(&ctx.paths, &a.path);
    if asm.deny.is_some() {
        return Ok((
            asm,
            format!("edit {}", show(ctx, &p)),
            None,
            false,
            Vec::new(),
        ));
    }
    let root = as_root(&mut asm, &p);
    let old = match fs::read_to_string(&p) {
        Ok(t) => t,
        // Unreadable as you (0600 root files): the root side checks the edit.
        Err(e) if root && e.kind() == ErrorKind::PermissionDenied => {
            return Ok((
                asm,
                format!("edit {}", show(ctx, &p)),
                None,
                true,
                Vec::new(),
            ));
        }
        Err(e) => return Err(io_msg(&p, &e)),
    };
    let new = apply_edit(&old, a)?;
    let rule = session_rules(&asm);
    Ok((
        asm,
        format!("edit {}", show(ctx, &p)),
        Some(diff(&old, &new, DIFF_ROWS)),
        true,
        rule,
    ))
}

fn apply_edit(old: &str, a: &EditArgs) -> Result<String, String> {
    if a.old.is_empty() {
        return Err("`old` is empty; use fs_write to create a file".into());
    }
    let n = old.matches(&a.old).count();
    match n {
        0 => Err("`old` text not found in the file; re-read it and copy the text exactly".into()),
        1 => Ok(old.replacen(&a.old, &a.new, 1)),
        _ if a.replace_all => Ok(old.replace(&a.old, &a.new)),
        _ => Err(format!(
            "`old` appears {n} times; include more surrounding text, or set replace_all"
        )),
    }
}

pub(super) fn plan_move(ctx: &ToolCtx, a: &MoveArgs) -> Planned {
    let from = ctx.paths.resolve(&a.from);
    let to = ctx.paths.resolve(&a.to);
    let mut asm = policy::write(&ctx.paths, &a.from);
    asm.merge(policy::write(&ctx.paths, &a.to));
    if from.is_dir() {
        asm.merge(policy::recursive(&ctx.paths, &a.from));
    }
    let rule = session_rules(&asm);
    (
        asm,
        format!("move {} → {}", show(ctx, &from), show(ctx, &to)),
        None,
        true,
        rule,
    )
}

pub(super) fn plan_delete(ctx: &ToolCtx, a: &DeleteArgs) -> Planned {
    let p = ctx.paths.resolve(&a.path);
    let is_dir = fs::symlink_metadata(&p).is_ok_and(|m| m.is_dir());
    let mut asm = if is_dir {
        policy::recursive(&ctx.paths, &a.path)
    } else {
        policy::write(&ctx.paths, &a.path)
    };
    if !is_dir {
        as_root(&mut asm, &p);
    }
    let preview = if is_dir {
        None
    } else {
        fs::read_to_string(&p).ok().map(|old| diff(&old, "", 60))
    };
    // Allowing deletes in a folder is not allowing writes there.
    asm.rekey("write:", "delete:");
    let rule = session_rules(&asm);
    let what = if is_dir { "delete folder" } else { "delete" };
    (
        asm,
        format!("{what} {}", show(ctx, &p)),
        preview,
        true,
        rule,
    )
}

// ── runs ────────────────────────────────────────────────────────────────────

pub(super) fn read(ctx: &ToolCtx, a: &ReadArgs) -> Executed {
    let p = ctx.paths.resolve(&a.path);
    let meta = match fs::metadata(&p) {
        Ok(m) => m,
        Err(e) => return fail(io_msg(&p, &e)),
    };
    if meta.is_dir() {
        return fail(format!("{} is a directory; use fs_list", p.display()));
    }
    let bytes = match fs::read(&p) {
        Ok(b) => b,
        Err(e) => return fail(io_msg(&p, &e)),
    };
    if bytes.iter().take(8192).any(|b| *b == 0) {
        return ok(
            format!("{} is a binary file ({} bytes)", p.display(), bytes.len()),
            format!("binary, {} bytes", bytes.len()),
        );
    }
    let text = String::from_utf8_lossy(&bytes);
    let offset = a.offset.unwrap_or(1).max(1);
    let limit = a.limit.unwrap_or(400).clamp(1, 2000);
    let total = text.lines().count();
    let mut out = String::new();
    let mut shown = 0;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        out.push_str(&format!("{:>5}  {line}\n", i + 1));
        shown += 1;
        if out.len() > MAX_READ {
            out.push_str("… [stopped: 64 KB shown; use offset to read on]\n");
            break;
        }
    }
    if total == 0 {
        out = "(empty file)\n".into();
    } else if offset - 1 + shown < total {
        out.push_str(&format!(
            "… {} more lines (total {total}); read on with offset={}\n",
            total - (offset - 1 + shown),
            offset + shown
        ));
    }
    ok(out, format!("{shown} of {total} lines"))
}

pub(super) fn list(ctx: &ToolCtx, a: &ListArgs) -> Executed {
    let p = ctx.paths.resolve(&a.path);
    let depth = a.depth.unwrap_or(1).clamp(1, 4);
    let mut rows = Vec::new();
    let walker = walkdir::WalkDir::new(&p)
        .min_depth(1)
        .max_depth(depth)
        .follow_links(false)
        .sort_by(|x, y| {
            y.file_type()
                .is_dir()
                .cmp(&x.file_type().is_dir())
                .then(x.file_name().cmp(y.file_name()))
        });
    let mut errors = 0;
    for entry in walker {
        let Ok(e) = entry else {
            errors += 1;
            continue;
        };
        if ctx.paths.classify(e.path()) == PathClass::ReeveKeys && e.depth() > 0 && e.path() != p {
            continue;
        }
        let meta = e.metadata().ok();
        let kind = if e.file_type().is_dir() {
            "d"
        } else if e.file_type().is_symlink() {
            "l"
        } else {
            "-"
        };
        let size = meta
            .as_ref()
            .filter(|m| m.is_file())
            .map_or(String::new(), |m| human(m.len()));
        let date = meta
            .as_ref()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                chrono::DateTime::<chrono::Local>::from(t)
                    .format("%Y-%m-%d")
                    .to_string()
            })
            .unwrap_or_default();
        let rel = e
            .path()
            .strip_prefix(&p)
            .unwrap_or(e.path())
            .display()
            .to_string();
        let mut name = format!("{rel}{}", if kind == "d" { "/" } else { "" });
        if kind == "l" {
            if let Ok(t) = fs::read_link(e.path()) {
                name.push_str(&format!(" → {}", t.display()));
            }
        }
        rows.push(format!("{kind} {size:>7}  {date}  {name}"));
        if rows.len() >= 500 {
            rows.push("… [stopped at 500 entries]".into());
            break;
        }
    }
    if rows.is_empty() && errors > 0 {
        return match fs::read_dir(&p) {
            Err(e) => fail(io_msg(&p, &e)),
            Ok(_) => fail(format!("couldn't read {}", p.display())),
        };
    }
    let n = rows.len();
    let mut out = rows.join("\n");
    if errors > 0 {
        out.push_str(&format!("\n({errors} entries unreadable)"));
    }
    if n == 0 {
        out = "(empty directory)".into();
    }
    ok(out, format!("{n} entries"))
}

pub(super) async fn search(ctx: &ToolCtx, a: &SearchArgs) -> Executed {
    let ctx = ctx.clone();
    let a = a.clone();
    tokio::task::spawn_blocking(move || search_blocking(&ctx, &a))
        .await
        .unwrap_or_else(|e| fail(format!("search failed: {e}")))
}

fn search_blocking(ctx: &ToolCtx, a: &SearchArgs) -> Executed {
    let root = ctx.paths.resolve(&a.path);
    let re = if a.pattern.is_empty() {
        None
    } else {
        match regex::RegexBuilder::new(&a.pattern)
            .case_insensitive(a.ignore_case)
            .size_limit(1 << 20)
            .build()
        {
            Ok(r) => Some(r),
            Err(e) => return fail(format!("bad regex: {e}")),
        }
    };
    let max = a.max_results.unwrap_or(100).clamp(1, 500);
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut hits = Vec::new();
    let mut files = 0usize;
    let mut stopped = None;
    let skip_roots = ["/proc", "/sys", "/dev", "/run", "/snap"];
    let walker = walkdir::WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| {
            let p = e.path();
            if p != root && skip_roots.iter().any(|s| p == Path::new(s)) {
                return false;
            }
            !matches!(
                ctx.paths.classify(p),
                PathClass::ReeveKeys | PathClass::Sensitive(_)
            )
        });
    for entry in walker.flatten() {
        if Instant::now() > deadline {
            stopped = Some("time limit (20 s)");
            break;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy();
        if let Some(g) = &a.glob {
            if !glob_match(g, &name) {
                continue;
            }
        }
        files += 1;
        let shown = show(ctx, entry.path());
        let Some(re) = &re else {
            hits.push(shown);
            if hits.len() >= max {
                stopped = Some("result limit");
                break;
            }
            continue;
        };
        if entry.metadata().is_ok_and(|m| m.len() > 4 * 1024 * 1024) {
            continue;
        }
        let Ok(bytes) = fs::read(entry.path()) else {
            continue;
        };
        if bytes.iter().take(4096).any(|b| *b == 0) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        for (i, line) in text.lines().enumerate() {
            if re.is_match(line) {
                let line: String = line.chars().take(240).collect();
                hits.push(format!("{shown}:{}: {line}", i + 1));
                if hits.len() >= max {
                    break;
                }
            }
        }
        if hits.len() >= max {
            stopped = Some("result limit");
            break;
        }
    }
    let mut out = if hits.is_empty() {
        "no matches".to_string()
    } else {
        hits.join("\n")
    };
    if let Some(why) = stopped {
        out.push_str(&format!("\n… [stopped: {why}; narrow the path or pattern]"));
    }
    ok(
        cap(&out, MAX_OUTPUT),
        format!("{} hits in {files} files", hits.len()),
    )
}

/// `*` and `?` over a file name.
fn glob_match(pattern: &str, name: &str) -> bool {
    fn go(p: &[char], n: &[char]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], n) || (!n.is_empty() && go(p, &n[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &n[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    go(&p, &n)
}

pub(super) fn stat(ctx: &ToolCtx, a: &PathArg) -> Executed {
    use std::os::unix::fs::MetadataExt;
    let p = ctx.paths.resolve(&a.path);
    let meta = match fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(e) => return fail(io_msg(&p, &e)),
    };
    let kind = if meta.is_dir() {
        "directory"
    } else if meta.file_type().is_symlink() {
        "symlink"
    } else if meta.is_file() {
        "file"
    } else {
        "special file"
    };
    let mut out = format!(
        "path: {}\ntype: {kind}\nsize: {} ({} bytes)\nmode: {:o}\nowner: {} ({})  group: {} ({})\nmodified: {}\n",
        p.display(),
        human(meta.len()),
        meta.len(),
        meta.mode() & 0o7777,
        name_of("/etc/passwd", meta.uid()),
        meta.uid(),
        name_of("/etc/group", meta.gid()),
        meta.gid(),
        chrono::DateTime::<chrono::Local>::from(meta.modified().unwrap_or(std::time::UNIX_EPOCH))
            .format("%Y-%m-%d %H:%M:%S"),
    );
    if meta.file_type().is_symlink() {
        if let Ok(t) = fs::read_link(&p) {
            out.push_str(&format!("target: {}\n", t.display()));
        }
    }
    ok(out, kind.to_string())
}

fn name_of(db: &str, id: u32) -> String {
    fs::read_to_string(db)
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                let mut f = l.split(':');
                let name = f.next()?;
                let _ = f.next();
                (f.next()? == id.to_string()).then(|| name.to_string())
            })
        })
        .unwrap_or_else(|| "?".into())
}

fn changed(
    ctx: &ToolCtx,
    p: &Path,
    pre: Option<crate::undo::Blob>,
    old: Option<String>,
    new: &str,
    created: bool,
) -> Executed {
    let post = match ctx.undo.snapshot(p) {
        Ok(b) => b,
        Err(e) => return fail(format!("written, but the undo copy failed: {e}")),
    };
    let d = diff(old.as_deref().unwrap_or(""), new, DIFF_ROWS);
    let summary = if created {
        format!("created {} ({} lines)", show(ctx, p), d.added)
    } else {
        format!("{} +{} −{}", show(ctx, p), d.added, d.removed)
    };
    let mut e = ok(format!("{summary}\n"), summary);
    e.undo = Some(Undo::Files {
        changes: vec![FileChange {
            path: p.to_string_lossy().into_owned(),
            pre,
            post,
            root: false,
        }],
    });
    e.diff = Some(d);
    e
}

/// Run one root operation through `sudo reeve root`.
async fn root_exec(ctx: &ToolCtx, op: RootOp) -> Executed {
    let Ok(json) = serde_json::to_vec(&op) else {
        return fail("couldn't encode the root request".into());
    };
    let command = format!("sudo {} root", quote(&ctx.exe.to_string_lossy()));
    let out = run_command(
        ctx,
        RunSpec {
            command: &command,
            cwd: &ctx.paths.home,
            timeout: Duration::from_secs(180),
            sudo: true,
            stdin: Some(json),
        },
    )
    .await;
    if let Some(f) = out.failure {
        return fail(f);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let reply = stdout
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<RootReply>(l).ok());
    let Some(reply) = reply else {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = err
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("no reply");
        return fail(format!("root step failed: {why}"));
    };
    if !reply.ok {
        return fail(reply.error.unwrap_or_else(|| "root step failed".into()));
    }
    let d = match (&reply.old_text, &reply.new_text) {
        (o, Some(n)) => Some(diff(o.as_deref().unwrap_or(""), n, DIFF_ROWS)),
        _ => None,
    };
    let summary = match &d {
        Some(d) => format!("{} +{} −{} (as root)", reply.summary, d.added, d.removed),
        None => format!("{} (as root)", reply.summary),
    };
    let mut e = ok(format!("{summary}\n"), summary);
    e.undo = reply.undo;
    e.diff = d;
    e
}

/// Reverse root-owned file changes.
pub(crate) async fn root_revert(ctx: &ToolCtx, undo: &Undo) -> Result<(Undo, String), String> {
    let e = root_exec(ctx, RootOp::Revert { undo: undo.clone() }).await;
    match (e.outcome.status, e.undo) {
        (Status::Ok, Some(u)) => Ok((u, e.outcome.summary)),
        _ => Err(e.outcome.summary),
    }
}

pub(super) async fn write(ctx: &ToolCtx, a: &WriteArgs) -> Executed {
    let p = ctx.paths.resolve(&a.path);
    if needs_root(&p) {
        return root_exec(
            ctx,
            RootOp::Write {
                path: p.to_string_lossy().into_owned(),
                content: a.content.clone(),
                create_dirs: a.create_dirs,
            },
        )
        .await;
    }
    if p.is_dir() {
        return fail(format!("{} is a directory", p.display()));
    }
    let pre = match ctx.undo.snapshot(&p) {
        Ok(b) => b,
        Err(e) => return fail(format!("won't change it without an undo copy: {e}")),
    };
    let old = fs::read_to_string(&p).ok();
    if let Some(parent) = p.parent() {
        if !parent.exists() {
            if !a.create_dirs {
                return fail(format!(
                    "{} doesn't exist; set create_dirs to make it",
                    parent.display()
                ));
            }
            if let Err(e) = fs::create_dir_all(parent) {
                return fail(io_msg(parent, &e));
            }
        }
    }
    if let Err(e) = put(&p, a.content.as_bytes(), pre.as_ref()) {
        return fail(io_msg(&p, &e));
    }
    changed(ctx, &p, pre.clone(), old, &a.content, pre.is_none())
}

/// Write keeping the file's mode. A file owned by someone else is written
/// in place, since replacing it would change its owner.
fn put(p: &Path, bytes: &[u8], pre: Option<&crate::undo::Blob>) -> std::io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let ours = fs::metadata(p).map_or(true, |m| m.uid() == rustix::process::getuid().as_raw());
    if ours {
        write_atomic(p, bytes, pre.map(|b| b.mode))
            .map_err(|e| std::io::Error::other(e.to_string()))
    } else {
        fs::write(p, bytes)
    }
}

pub(super) async fn edit(ctx: &ToolCtx, a: &EditArgs) -> Executed {
    let p = ctx.paths.resolve(&a.path);
    if needs_root(&p) {
        return root_exec(
            ctx,
            RootOp::Edit {
                path: p.to_string_lossy().into_owned(),
                old: a.old.clone(),
                new: a.new.clone(),
                replace_all: a.replace_all,
            },
        )
        .await;
    }
    let old = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) => return fail(io_msg(&p, &e)),
    };
    let new = match apply_edit(&old, a) {
        Ok(n) => n,
        Err(e) => return fail(e),
    };
    let pre = match ctx.undo.snapshot(&p) {
        Ok(b) => b,
        Err(e) => return fail(format!("won't change it without an undo copy: {e}")),
    };
    if let Err(e) = put(&p, new.as_bytes(), pre.as_ref()) {
        return fail(io_msg(&p, &e));
    }
    changed(ctx, &p, pre, Some(old), &new, false)
}

pub(super) fn mv(ctx: &ToolCtx, a: &MoveArgs) -> Executed {
    let from = ctx.paths.resolve(&a.from);
    let mut to = ctx.paths.resolve(&a.to);
    if to.is_dir() && !from.is_dir() {
        if let Some(name) = from.file_name() {
            to = to.join(name);
        }
    }
    if !from.exists() && fs::symlink_metadata(&from).is_err() {
        return fail(format!("{} doesn't exist", from.display()));
    }
    let mut replaced = None;
    if to.exists() {
        if !a.overwrite {
            return fail(format!(
                "{} already exists; set overwrite to replace it",
                to.display()
            ));
        }
        if to.is_dir() {
            return fail(format!(
                "{} is a directory; won't replace a directory",
                to.display()
            ));
        }
        replaced = match ctx.undo.snapshot(&to) {
            Ok(b) => b,
            Err(e) => return fail(format!("won't overwrite without an undo copy: {e}")),
        };
    }
    if let Err(e) = fs::rename(&from, &to) {
        let msg = if e.raw_os_error() == Some(18) {
            format!(
                "{} and {} are on different filesystems; copy then delete instead",
                from.display(),
                to.display()
            )
        } else {
            io_msg(&from, &e)
        };
        return fail(msg);
    }
    let summary = format!("moved {} → {}", show(ctx, &from), show(ctx, &to));
    let mut e = ok(format!("{summary}\n"), summary);
    e.undo = Some(Undo::Move {
        from: from.to_string_lossy().into_owned(),
        to: to.to_string_lossy().into_owned(),
        replaced,
    });
    e
}

pub(super) async fn delete(ctx: &ToolCtx, a: &DeleteArgs) -> Executed {
    let p = ctx.paths.resolve(&a.path);
    let meta = match fs::symlink_metadata(&p) {
        Ok(m) => m,
        Err(e) => return fail(io_msg(&p, &e)),
    };
    if !meta.is_dir() && needs_root(&p) {
        return root_exec(
            ctx,
            RootOp::Delete {
                path: p.to_string_lossy().into_owned(),
            },
        )
        .await;
    }
    if meta.is_dir() {
        if !a.recursive {
            return fail(format!(
                "{} is a directory; set recursive to delete it and everything in it",
                p.display()
            ));
        }
        let changes = match ctx.undo.snapshot_tree(&p) {
            Ok(c) => c,
            Err(e) => return fail(format!("won't delete without an undo copy: {e}")),
        };
        if let Err(e) = fs::remove_dir_all(&p) {
            // Some of it may be gone; the receipt still says how to restore what went.
            let mut ex = fail(io_msg(&p, &e));
            ex.undo = Some(Undo::Files {
                changes: only_gone(changes),
            });
            return ex;
        }
        let summary = format!("deleted {} ({} files)", show(ctx, &p), changes.len());
        let mut e = ok(format!("{summary}\n"), summary);
        e.undo = Some(Undo::Files { changes });
        return e;
    }
    let pre = match ctx.undo.snapshot(&p) {
        Ok(b) => b,
        Err(e) => return fail(format!("won't delete without an undo copy: {e}")),
    };
    if let Err(e) = fs::remove_file(&p) {
        return fail(io_msg(&p, &e));
    }
    let summary = format!("deleted {}", show(ctx, &p));
    let mut e = ok(format!("{summary}\n"), summary);
    e.undo = Some(Undo::Files {
        changes: vec![FileChange {
            path: p.to_string_lossy().into_owned(),
            pre,
            post: None,
            root: false,
        }],
    });
    e
}

fn only_gone(changes: Vec<FileChange>) -> Vec<FileChange> {
    changes
        .into_iter()
        .filter(|c| fs::symlink_metadata(&c.path).is_err())
        .collect()
}

fn human(n: u64) -> String {
    let f = n as f64;
    match n {
        0..1024 => format!("{n}B"),
        1024..1_048_576 => format!("{:.1}K", f / 1024.0),
        1_048_576..1_073_741_824 => format!("{:.1}M", f / 1_048_576.0),
        _ => format!("{:.1}G", f / 1_073_741_824.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolCtx, execute, prepare};
    use crate::undo::UndoStore;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolCtx::new(d.path().join(".reeve"), vec![]);
        c.paths.home = d.path().canonicalize().unwrap();
        c.paths.reeve_home = c.paths.home.join(".reeve");
        c.paths.cwd = c.paths.home.clone();
        c.undo = UndoStore::new(&c.paths.reeve_home);
        (d, c)
    }

    async fn run(c: &ToolCtx, tool: &str, args: serde_json::Value) -> Executed {
        let plan = prepare(c, tool, &args.to_string()).unwrap();
        execute(c, &plan).await
    }

    #[tokio::test]
    async fn write_edit_and_undo_round_trip() {
        let (_d, c) = ctx();
        let f = c.paths.home.join("app.conf");
        let e = run(
            &c,
            "fs_write",
            serde_json::json!({"path": f, "content": "a=1\nb=2\n"}),
        )
        .await;
        assert_eq!(e.outcome.status, Status::Ok, "{}", e.output);
        let plan = prepare(
            &c,
            "fs_edit",
            &serde_json::json!({"path": "~/app.conf", "old": "b=2", "new": "b=3"}).to_string(),
        )
        .unwrap();
        assert_eq!(plan.assessment.tier, Tier::T1);
        assert_eq!(plan.preview.as_ref().unwrap().added, 1);
        let e = execute(&c, &plan).await;
        assert_eq!(fs::read_to_string(&f).unwrap(), "a=1\nb=3\n");
        c.undo.revert(e.undo.as_ref().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "a=1\nb=2\n");
    }

    #[tokio::test]
    async fn edits_that_cant_apply_are_rejected_before_asking() {
        let (_d, c) = ctx();
        fs::write(c.paths.home.join("x"), "same same").unwrap();
        let err = prepare(
            &c,
            "fs_edit",
            &serde_json::json!({"path": "~/x", "old": "same", "new": "diff"}).to_string(),
        )
        .unwrap_err();
        assert!(err.contains("2 times"), "{err}");
    }

    #[tokio::test]
    async fn read_numbers_lines_and_pages() {
        let (_d, c) = ctx();
        let text: String = (1..=10).map(|i| format!("l{i}\n")).collect();
        fs::write(c.paths.home.join("f"), text).unwrap();
        let e = run(
            &c,
            "fs_read",
            serde_json::json!({"path": "~/f", "offset": 3, "limit": 2}),
        )
        .await;
        assert!(
            e.output.contains("    3  l3") && e.output.contains("offset=5"),
            "{}",
            e.output
        );
    }

    #[tokio::test]
    async fn search_skips_secrets() {
        let (_d, c) = ctx();
        fs::create_dir_all(c.paths.home.join(".ssh")).unwrap();
        fs::write(c.paths.home.join(".ssh/id_ed25519"), "PRIVATE needle").unwrap();
        fs::write(c.paths.home.join("notes.txt"), "a needle here").unwrap();
        let e = run(
            &c,
            "fs_search",
            serde_json::json!({"path": "~", "pattern": "needle"}),
        )
        .await;
        assert!(
            e.output.contains("notes.txt") && !e.output.contains("PRIVATE"),
            "{}",
            e.output
        );
        let e = run(
            &c,
            "fs_search",
            serde_json::json!({"path": "~", "glob": "*.txt"}),
        )
        .await;
        assert!(e.output.contains("~/notes.txt"), "{}", e.output);
    }

    #[tokio::test]
    async fn delete_a_folder_and_bring_it_back() {
        let (_d, c) = ctx();
        let dir = c.paths.home.join("old");
        fs::create_dir_all(dir.join("x")).unwrap();
        fs::write(dir.join("x/y"), "keep me").unwrap();
        let plan = prepare(
            &c,
            "fs_delete",
            &serde_json::json!({"path": "~/old", "recursive": true}).to_string(),
        )
        .unwrap();
        assert_eq!(plan.assessment.tier, Tier::T1);
        let e = execute(&c, &plan).await;
        assert!(!dir.exists(), "{}", e.output);
        c.undo.revert(e.undo.as_ref().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(dir.join("x/y")).unwrap(), "keep me");
    }

    #[tokio::test]
    async fn moves_refuse_to_clobber_unless_asked() {
        let (_d, c) = ctx();
        fs::write(c.paths.home.join("a"), "A").unwrap();
        fs::write(c.paths.home.join("b"), "B").unwrap();
        let e = run(
            &c,
            "fs_move",
            serde_json::json!({"from": "~/a", "to": "~/b"}),
        )
        .await;
        assert_eq!(e.outcome.status, Status::Error);
        let e = run(
            &c,
            "fs_move",
            serde_json::json!({"from": "~/a", "to": "~/b", "overwrite": true}),
        )
        .await;
        assert_eq!(fs::read_to_string(c.paths.home.join("b")).unwrap(), "A");
        c.undo.revert(e.undo.as_ref().unwrap()).unwrap();
        assert_eq!(fs::read_to_string(c.paths.home.join("a")).unwrap(), "A");
        assert_eq!(fs::read_to_string(c.paths.home.join("b")).unwrap(), "B");
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.conf", "dnf.conf"));
        assert!(glob_match("kernel-?.*", "kernel-6.x"));
        assert!(!glob_match("*.conf", "dnf.conf.bak"));
    }
}
