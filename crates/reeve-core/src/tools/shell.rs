//! The `shell` tool: `bash -c` in its own session, with no terminal.
//!
//! `setsid` detaches the command from Reeve's terminal, so nothing it runs
//! (sudo, ssh, a pager) can draw over the TUI or wait on a keyboard that
//! will never answer. The command leads its own process group, which is
//! killed as a whole on timeout or when the turn is stopped.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde::Deserialize;
use tokio::io::AsyncReadExt;

use super::{Executed, ToolCtx, cap};
use crate::policy::{Assessment, Tier, shell as classify};
use crate::receipts::{Outcome, Status};
use crate::undo::sha256_hex;

const MAX_OUTPUT: usize = 32 * 1024;
const DEFAULT_TIMEOUT: u64 = 120;
const MAX_TIMEOUT: u64 = 600;

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ShellArgs {
    command: String,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
}

pub(super) fn plan(
    ctx: &ToolCtx,
    a: &ShellArgs,
) -> (
    Assessment,
    String,
    Option<crate::diff::FileDiff>,
    bool,
    Option<String>,
) {
    let mut paths = ctx.paths.clone();
    if let Some(cwd) = &a.cwd {
        paths.cwd = paths.resolve(cwd);
    }
    let asm = classify::assess(&paths, &a.command);
    // Allowing "this exact command" for the session is the only rule a
    // command gets: a program name alone is too broad.
    let rule = (asm.tier == Tier::T1).then(|| format!("shell:{}", a.command.trim()));
    (asm, a.command.trim().to_string(), None, false, rule)
}

/// Kills the whole process group when dropped: on timeout, on error, and
/// when the turn is cancelled mid-command.
struct GroupGuard(Option<rustix::process::Pid>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
        }
    }
}

/// A command to run.
pub(crate) struct RunSpec<'a> {
    pub command: &'a str,
    pub cwd: &'a Path,
    pub timeout: Duration,
    /// It may call sudo: arm the askpass for its lifetime.
    pub sudo: bool,
    pub stdin: Option<Vec<u8>>,
}

/// What a command did.
pub(crate) struct RunOut {
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub secs: f32,
    /// Couldn't start, or timed out: the message.
    pub failure: Option<String>,
}

/// Run `bash -c` in its own session with no terminal (or, for the CLI,
/// on the terminal so sudo can prompt there).
pub(crate) async fn run_command(ctx: &ToolCtx, spec: RunSpec<'_>) -> RunOut {
    let mut cmd = if ctx.interactive_sudo {
        tokio::process::Command::new("bash")
    } else {
        let mut c = tokio::process::Command::new("setsid");
        c.arg("bash");
        c
    };
    cmd.arg("--noprofile")
        .arg("--norc")
        .arg("-c")
        .arg(spec.command)
        .current_dir(spec.cwd)
        .stdin(if spec.stdin.is_some() {
            Stdio::piped()
        } else if ctx.interactive_sudo {
            Stdio::inherit()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for k in &ctx.secret_env {
        cmd.env_remove(k);
    }
    for k in [
        "SUDO_ASKPASS",
        "SSH_ASKPASS",
        "REEVE_ASKPASS_SOCK",
        "REEVE_ASKPASS_TOKEN",
    ] {
        cmd.env_remove(k);
    }
    // Nothing may wait for a person or page its output.
    for (k, v) in [
        ("PAGER", "cat"),
        ("SYSTEMD_PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("MANPAGER", "cat"),
        ("EDITOR", "false"),
        ("VISUAL", "false"),
        ("GIT_TERMINAL_PROMPT", "0"),
        ("SYSTEMD_COLORS", "0"),
        ("TERM", "dumb"),
        ("DEBIAN_FRONTEND", "noninteractive"),
    ] {
        cmd.env(k, v);
    }
    let _armed = match (&ctx.askpass, spec.sudo) {
        (Some(ap), true) => {
            for (k, v) in ap.env() {
                cmd.env(k, v);
            }
            Some(ap.arm())
        }
        _ => None,
    };
    let started = Instant::now();
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return RunOut {
                code: None,
                stdout: vec![],
                stderr: vec![],
                secs: 0.0,
                failure: Some(format!("couldn't start bash: {e}")),
            };
        }
    };
    // setsid execs bash in place (Reeve's child isn't a group leader), so
    // the child's pid is the new session and group.
    let guard = GroupGuard(
        (!ctx.interactive_sudo)
            .then(|| {
                child
                    .id()
                    .and_then(|p| rustix::process::Pid::from_raw(p as i32))
            })
            .flatten(),
    );
    if let (Some(input), Some(mut sin)) = (spec.stdin, child.stdin.take()) {
        tokio::spawn(async move {
            let _ = tokio::io::AsyncWriteExt::write_all(&mut sin, &input).await;
        });
    }
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let read_all = async {
        let mut o = Vec::new();
        let mut e = Vec::new();
        tokio::join!(
            async {
                if let Some(s) = out.as_mut() {
                    let _ = s.take(8 * 1024 * 1024).read_to_end(&mut o).await;
                }
            },
            async {
                if let Some(s) = err.as_mut() {
                    let _ = s.take(8 * 1024 * 1024).read_to_end(&mut e).await;
                }
            }
        );
        let status = child.wait().await;
        (o, e, status)
    };
    let result = tokio::time::timeout(spec.timeout, read_all).await;
    // Finished or not, anything it left running goes too.
    drop(guard);
    let secs = started.elapsed().as_secs_f32();
    match result {
        Ok((stdout, stderr, status)) => RunOut {
            code: status.ok().and_then(|s| s.code()),
            stdout,
            stderr,
            secs,
            failure: None,
        },
        Err(_) => RunOut {
            code: None,
            stdout: vec![],
            stderr: vec![],
            secs,
            failure: Some(format!(
                "timed out after {}s and was stopped. For long jobs, raise timeout_secs (max {MAX_TIMEOUT}).",
                spec.timeout.as_secs()
            )),
        },
    }
}

/// Model-facing text, one-line summary, and receipt outcome for a run.
pub(crate) fn report(out: &RunOut) -> Executed {
    if let Some(msg) = &out.failure {
        return failed(msg.clone());
    }
    let code = out.code;
    let so = String::from_utf8_lossy(&out.stdout);
    let se = String::from_utf8_lossy(&out.stderr);
    let mut text = format!(
        "exit {} · {:.1}s\n",
        code.map_or("signal".to_string(), |c| c.to_string()),
        out.secs
    );
    if !so.trim().is_empty() {
        text.push_str(&cap(&so, MAX_OUTPUT));
        if !text.ends_with('\n') {
            text.push('\n');
        }
    }
    if !se.trim().is_empty() {
        text.push_str("--- stderr ---\n");
        text.push_str(&cap(&se, MAX_OUTPUT / 2));
    }
    if se.contains("no password was provided")
        || se.contains("a password is required")
        || se.contains("a terminal is required")
    {
        text.push_str("\n[reeve] sudo got no password (the owner cancelled, or no one was there to type it). Don't retry unless asked.\n");
    }
    let last = so
        .lines()
        .chain(se.lines())
        .rev()
        .find(|l| !l.trim().is_empty())
        .map(|l| l.chars().take(100).collect::<String>())
        .unwrap_or_default();
    let summary = match code {
        Some(0) if last.is_empty() => format!("exit 0 · {:.1}s", out.secs),
        Some(0) => format!("exit 0 · {:.1}s · {last}", out.secs),
        Some(c) => format!("exit {c} · {last}"),
        None => "killed by a signal".into(),
    };
    Executed {
        output: text,
        outcome: Outcome {
            status: if code == Some(0) {
                Status::Ok
            } else {
                Status::Error
            },
            exit: code,
            summary,
            output_sha256: Some(sha256_hex(
                [out.stdout.as_slice(), out.stderr.as_slice()]
                    .concat()
                    .as_slice(),
            )),
        },
        undo: None,
        diff: None,
    }
}

pub(super) async fn run(ctx: &ToolCtx, a: &ShellArgs, sudo: bool) -> Executed {
    let cwd = a
        .cwd
        .as_deref()
        .map_or_else(|| ctx.paths.home.clone(), |c| ctx.paths.resolve(c));
    let timeout = Duration::from_secs(
        a.timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT)
            .clamp(1, MAX_TIMEOUT),
    );
    let out = run_command(
        ctx,
        RunSpec {
            command: &a.command,
            cwd: &cwd,
            timeout,
            sudo,
            stdin: None,
        },
    )
    .await;
    report(&out)
}

pub(crate) fn failed(msg: String) -> Executed {
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

#[cfg(test)]
mod tests {
    use crate::receipts::Status;
    use crate::tools::{ToolCtx, execute, prepare};
    use serde_json::json;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolCtx::new(d.path().join(".reeve"), vec!["REEVE_TEST_SECRET".into()]);
        c.paths.home = d.path().to_path_buf();
        (d, c)
    }

    #[tokio::test]
    async fn runs_captures_and_reports_exit() {
        let (_d, c) = ctx();
        let p = prepare(
            &c,
            "shell",
            &json!({"command": "echo hi; echo oops >&2; exit 3"}).to_string(),
        )
        .unwrap();
        let e = execute(&c, &p).await;
        assert_eq!(e.outcome.exit, Some(3));
        assert_eq!(e.outcome.status, Status::Error);
        assert!(
            e.output.contains("hi") && e.output.contains("--- stderr ---\noops"),
            "{}",
            e.output
        );
    }

    #[tokio::test]
    async fn keys_never_reach_commands() {
        let (_d, c) = ctx();
        let p = prepare(
            &c,
            "shell",
            &json!({"command": "REEVE_TEST_SECRET=leak; env | grep -c REEVE_TEST_SECRET || true"})
                .to_string(),
        )
        .unwrap();
        let e = execute(&c, &p).await;
        // The assignment is shell-local (not exported), so nothing is seen.
        assert!(e.output.contains("\n0\n"), "{}", e.output);
    }

    #[tokio::test]
    async fn timeouts_kill_the_whole_group() {
        let (d, c) = ctx();
        let marker = d.path().join("survived");
        let cmd = format!("(sleep 3; touch {}) & sleep 30", marker.display());
        let p = prepare(
            &c,
            "shell",
            &json!({"command": cmd, "timeout_secs": 1}).to_string(),
        )
        .unwrap();
        let e = execute(&c, &p).await;
        assert!(e.output.contains("timed out"), "{}", e.output);
        tokio::time::sleep(std::time::Duration::from_secs(4)).await;
        assert!(!marker.exists(), "a background child outlived the timeout");
    }

    #[tokio::test]
    async fn no_terminal_means_sudo_fails_fast() {
        // A real sudo attempt: it logs an auth failure (at alert level) and may
        // count toward pam_faillock. Opt in on a developer machine.
        if std::env::var_os("REEVE_TEST_SUDO").is_none() {
            return;
        }
        // CI runners have passwordless sudo: nothing to prove there.
        let passwordless = std::process::Command::new("sudo")
            .args(["-n", "true"])
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if passwordless {
            return;
        }
        let (_d, c) = ctx();
        let p = prepare(
            &c,
            "shell",
            &json!({"command": "sudo -k; sudo true", "timeout_secs": 10}).to_string(),
        )
        .unwrap();
        assert!(p.assessment.sudo);
        let started = std::time::Instant::now();
        let e = execute(&c, &p).await;
        assert!(
            started.elapsed().as_secs() < 8,
            "sudo waited for a password"
        );
        assert_ne!(e.outcome.exit, Some(0));
    }
}
