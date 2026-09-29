//! Changes that prove they worked: a transaction declares its checks before
//! it changes anything, its changes are tagged as they're made, and at the
//! end Reeve runs the checks itself. Pass: the transaction is verified.
//! Fail: every change in it is undone, newest first, each with its own
//! receipt. The model's say-so never counts as a pass.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::distro::quote;
use crate::policy::{Tier, shell as classify};
use crate::tools::{ToolCtx, shell_exec_status};

/// One check Reeve runs after the changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// A read-only command: passes when it exits 0 (and, if given, its
    /// output contains `expect`).
    Command {
        /// The command (must be T0).
        command: String,
        /// Text the output must contain.
        #[serde(default)]
        expect: Option<String>,
    },
    /// A systemd unit is active.
    UnitActive {
        /// Unit name.
        unit: String,
        /// A user unit.
        #[serde(default)]
        user: bool,
    },
    /// A unit logged no more than `max` errors since the changes began.
    JournalQuiet {
        /// Unit name.
        unit: String,
        /// Errors allowed (default 0).
        #[serde(default)]
        max: u32,
        /// A user unit.
        #[serde(default)]
        user: bool,
    },
    /// A mount is below a use percentage.
    DiskBelow {
        /// Mount point.
        mount: String,
        /// Percent, 1–100.
        percent: u8,
    },
}

impl Check {
    /// One line for cards and receipts.
    pub fn describe(&self) -> String {
        match self {
            Self::Command {
                command,
                expect: Some(e),
            } => format!("`{command}` succeeds and shows \"{e}\""),
            Self::Command {
                command,
                expect: None,
            } => format!("`{command}` succeeds"),
            Self::UnitActive { unit, user } => {
                format!("{unit}{} is active", if *user { " (user)" } else { "" })
            }
            Self::JournalQuiet { unit, max: 0, .. } => format!("{unit} logs no errors"),
            Self::JournalQuiet { unit, max, .. } => format!("{unit} logs at most {max} errors"),
            Self::DiskBelow { mount, percent } => format!("{mount} is under {percent}% full"),
        }
    }

    /// Why this check isn't acceptable, if it isn't.
    pub fn problem(&self, ctx: &ToolCtx) -> Option<String> {
        let unit_ok = |u: &str| {
            !u.is_empty()
                && u.chars()
                    .all(|c| c.is_ascii_alphanumeric() || "@._-:\\".contains(c))
        };
        match self {
            Self::Command { command, .. } => {
                let a = classify::assess(&ctx.paths, command);
                if a.deny.is_some() || a.tier != Tier::T0 {
                    Some(format!(
                        "checks must only read, and `{command}` is {}",
                        a.tier.label()
                    ))
                } else if a.quiet_write {
                    Some(format!(
                        "checks must only read, and `{command}` writes files"
                    ))
                } else if a.sudo {
                    Some("checks run without root".into())
                } else {
                    None
                }
            }
            Self::UnitActive { unit, .. } | Self::JournalQuiet { unit, .. } => {
                (!unit_ok(unit)).then(|| format!("{unit:?} isn't a unit name"))
            }
            Self::DiskBelow { mount, percent } => {
                if !mount.starts_with('/') {
                    Some("mount must be an absolute path".into())
                } else if *percent == 0 || *percent > 100 {
                    Some("percent must be 1–100".into())
                } else {
                    None
                }
            }
        }
    }

    /// Run it: (passed, what was seen).
    pub async fn run(&self, ctx: &ToolCtx, since: DateTime<Utc>) -> (bool, String) {
        match self {
            Self::Command { command, expect } => {
                let (code, out) = shell_exec_status(ctx, command).await;
                let shown = out
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .chars()
                    .take(160)
                    .collect::<String>();
                let contains = expect.as_ref().is_none_or(|e| out.contains(e.as_str()));
                (
                    code == Some(0) && contains,
                    format!(
                        "exit {} · {shown}",
                        code.map_or("signal".into(), |c| c.to_string())
                    ),
                )
            }
            Self::UnitActive { unit, user } => {
                let cmd = format!(
                    "systemctl {}is-active -- {}",
                    if *user { "--user " } else { "" },
                    quote(unit)
                );
                let (_, out) = shell_exec_status(ctx, &cmd).await;
                let state = out.trim().to_string();
                (state == "active", format!("{unit} is {state}"))
            }
            Self::JournalQuiet { unit, max, user } => {
                let flag = if *user { "--user-unit" } else { "-u" };
                let cmd = format!(
                    "journalctl {flag} {} --since @{} -p err -q --no-pager | wc -l",
                    quote(unit),
                    since.timestamp()
                );
                let (_, out) = shell_exec_status(ctx, &cmd).await;
                let n: u32 = out.trim().parse().unwrap_or(u32::MAX);
                (
                    n <= *max,
                    format!("{n} errors from {unit} since the change"),
                )
            }
            Self::DiskBelow { mount, percent } => {
                let cmd = format!("df --output=pcent {} | tail -n 1", quote(mount));
                let (_, out) = shell_exec_status(ctx, &cmd).await;
                let used: u8 = out
                    .trim()
                    .trim_end_matches('%')
                    .trim()
                    .parse()
                    .unwrap_or(100);
                (used < *percent, format!("{mount} is {used}% full"))
            }
        }
    }
}

/// An open transaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Txn {
    /// Short id (`t<unix seconds>`).
    pub id: String,
    /// What the changes are for.
    pub goal: String,
    /// What must be true afterwards.
    pub checks: Vec<Check>,
    /// Seconds to let things settle before checking.
    pub wait_secs: u64,
    /// When it began.
    pub started: DateTime<Utc>,
    /// Receipts of the changes made in it, in order.
    pub changes: Vec<u64>,
    /// Changes that can't be undone automatically.
    pub unrevertable: Vec<u64>,
    /// When the last change finished; journal checks count from here.
    pub last_change: Option<DateTime<Utc>>,
}

impl Txn {
    /// Start one.
    pub fn new(goal: &str, checks: Vec<Check>, wait_secs: Option<u64>) -> Self {
        let now = Utc::now();
        Self {
            id: format!("t{}", now.timestamp()),
            goal: goal.trim().to_string(),
            checks,
            wait_secs: wait_secs.unwrap_or(3).min(120),
            started: now,
            changes: Vec::new(),
            unrevertable: Vec::new(),
            last_change: None,
        }
    }

    /// A change made inside it: receipt `seq`, which can be undone or not.
    pub fn record(&mut self, seq: u64, undoable: bool) {
        if undoable {
            self.changes.push(seq);
        } else {
            self.unrevertable.push(seq);
        }
        self.last_change = Some(Utc::now());
    }

    /// Goal and checks, for approval cards.
    pub fn brief(&self) -> TxnBrief {
        TxnBrief {
            goal: self.goal.clone(),
            checks: self.checks.iter().map(Check::describe).collect(),
        }
    }

    /// Wait, then run every check.
    pub async fn verify(&self, ctx: &ToolCtx) -> Vec<CheckResult> {
        tokio::time::sleep(Duration::from_secs(self.wait_secs)).await;
        let mut out = Vec::new();
        for c in &self.checks {
            let (ok, seen) = c.run(ctx, self.last_change.unwrap_or(self.started)).await;
            out.push(CheckResult {
                check: c.describe(),
                ok,
                seen,
            });
        }
        out
    }
}

/// What an approval card says about the open transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TxnBrief {
    /// What the changes are for.
    pub goal: String,
    /// The checks, described.
    pub checks: Vec<String>,
}

/// One check's result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckResult {
    /// The check, described.
    pub check: String,
    /// Passed.
    pub ok: bool,
    /// What Reeve saw.
    pub seen: String,
}

/// Arguments of `change_begin`.
#[derive(Debug, Clone, Deserialize)]
pub struct BeginArgs {
    /// What the changes are for.
    pub goal: String,
    /// What must be true afterwards.
    #[serde(default)]
    pub checks: Vec<Check>,
    /// Settle time before checking.
    #[serde(default)]
    pub wait_secs: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> ToolCtx {
        ToolCtx::new(tempfile::tempdir().unwrap().keep(), vec![])
    }

    #[test]
    fn checks_must_only_read() {
        let c = ctx();
        assert!(
            Check::Command {
                command: "systemctl is-active sshd".into(),
                expect: None
            }
            .problem(&c)
            .is_none()
        );
        // Whatever is or isn't in /tmp: deleting there, or writing a new
        // file there, is still not a read.
        for command in [
            "rm -rf /tmp/x",
            "rm -rf /tmp/reeve-test-not-there",
            "echo hi > /tmp/reeve-test-not-there",
        ] {
            assert!(
                Check::Command {
                    command: command.into(),
                    expect: None
                }
                .problem(&c)
                .is_some(),
                "{command}"
            );
        }
        assert!(
            Check::UnitActive {
                unit: "a; rm".into(),
                user: false
            }
            .problem(&c)
            .is_some()
        );
        assert!(
            Check::DiskBelow {
                mount: "/".into(),
                percent: 0
            }
            .problem(&c)
            .is_some()
        );
    }

    #[tokio::test]
    async fn checks_really_run() {
        let c = ctx();
        let since = Utc::now();
        assert!(
            Check::Command {
                command: "true".into(),
                expect: None
            }
            .run(&c, since)
            .await
            .0
        );
        assert!(
            !Check::Command {
                command: "false".into(),
                expect: None
            }
            .run(&c, since)
            .await
            .0
        );
        assert!(
            !Check::Command {
                command: "echo hi".into(),
                expect: Some("bye".into())
            }
            .run(&c, since)
            .await
            .0
        );
        assert!(
            Check::DiskBelow {
                mount: "/".into(),
                percent: 100
            }
            .run(&c, since)
            .await
            .0 || cfg!(not(target_os = "linux"))
        );
    }

    #[test]
    fn check_json_is_what_the_model_writes() {
        let v: Vec<Check> = serde_json::from_str(
            r#"[{"kind":"unit_active","unit":"bluetooth.service"},{"kind":"journal_quiet","unit":"bluetooth.service"},{"kind":"command","command":"bluetoothctl show","expect":"Powered: yes"}]"#,
        )
        .unwrap();
        assert_eq!(v[0].describe(), "bluetooth.service is active");
        assert_eq!(v[1].describe(), "bluetooth.service logs no errors");
        assert!(v[2].describe().contains("Powered: yes"));
    }
}
