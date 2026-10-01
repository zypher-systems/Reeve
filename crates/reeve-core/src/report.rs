//! The session report: `sessions/<id>/report.md`, rewritten after every
//! turn so it's current even if Reeve is killed. What was asked, what was
//! done (by receipt), what can be undone, what failed, and what it cost.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Local, Utc};

use crate::error::Result;
use crate::llm::Message;
use crate::receipts::{Receipt, Status};
use crate::session::Meta;
use crate::spend::{Tally, format_tokens};
use crate::undo::Undo;

/// Write the report into `dir`.
pub fn write(
    dir: &Path,
    meta: &Meta,
    transcript: &[Message],
    receipts: &[Receipt],
    tally: &Tally,
) -> Result<PathBuf> {
    let path = dir.join("report.md");
    fs::write(&path, render(meta, transcript, receipts, tally))?;
    Ok(path)
}

/// The report as Markdown.
pub fn render(meta: &Meta, transcript: &[Message], receipts: &[Receipt], tally: &Tally) -> String {
    let mut s = String::new();
    let started = meta.started.with_timezone(&Local);
    let _ = writeln!(s, "# Reeve session · {}", started.format("%Y-%m-%d %H:%M"));
    let _ = writeln!(s);
    let _ = writeln!(s, "- **Model:** {} via {}", meta.model, meta.connection);
    let _ = writeln!(
        s,
        "- **Updated:** {}",
        Utc::now().with_timezone(&Local).format("%Y-%m-%d %H:%M")
    );
    let _ = writeln!(
        s,
        "- **Spent:** {} over {} call{} ({} tokens)",
        tally.label(),
        tally.calls,
        if tally.calls == 1 { "" } else { "s" },
        format_tokens(tally.usage.total())
    );
    let _ = writeln!(s, "- **Session:** `{}`", meta.id);

    let asked: Vec<&Message> = transcript.iter().filter(|m| m.role == "user").collect();
    let _ = writeln!(s, "\n## What you asked\n");
    if asked.is_empty() {
        let _ = writeln!(s, "Nothing yet.");
    }
    for (i, m) in asked.iter().enumerate() {
        let first = m.content.lines().next().unwrap_or("").trim();
        let _ = writeln!(s, "{}. {}", i + 1, clip(first, 160));
    }

    let _ = writeln!(s, "\n## What Reeve did\n");
    if receipts.is_empty() {
        let _ = writeln!(s, "No actions.");
    } else {
        let _ = writeln!(s, "| # | tier | action | on | result | approved |");
        let _ = writeln!(s, "| --- | --- | --- | --- | --- | --- |");
        for r in receipts {
            let result = match r.outcome.status {
                Status::Ok => "✓ ".to_string() + &r.outcome.summary,
                Status::Error => "✗ ".to_string() + &r.outcome.summary,
                Status::Denied => "declined".into(),
                Status::Refused => "refused by policy".into(),
            };
            let _ = writeln!(
                s,
                "| {} | {} | {} | {} | {} | {} |",
                r.seq,
                r.tier.label(),
                r.tool,
                cell(&clip(&r.target(), 60)),
                cell(&clip(&result, 80)),
                r.approved_by
            );
        }
    }

    let undoable: Vec<&Receipt> = receipts
        .iter()
        .filter(|r| r.undo.is_some() && r.outcome.status == Status::Ok && r.undoes.is_none())
        .filter(|r| {
            !receipts
                .iter()
                .any(|u| u.undoes == Some(r.seq) && u.outcome.status == Status::Ok)
        })
        .collect();
    if !undoable.is_empty() {
        let _ = writeln!(s, "\n## Can be undone\n");
        for r in undoable {
            let what = match &r.undo {
                Some(Undo::Packages { transaction, .. }) => {
                    format!("package transaction {transaction}")
                }
                Some(Undo::Pacman { changes }) => {
                    format!("packages: {}", crate::pacman::summary(changes))
                }
                Some(Undo::Unit { unit, .. }) => format!("{unit}'s previous state"),
                Some(Undo::Move { from, .. }) => format!("move back to {from}"),
                Some(Undo::Files { changes }) if changes.len() == 1 => changes[0].path.clone(),
                Some(Undo::Files { changes }) => format!("{} files", changes.len()),
                None => String::new(),
            };
            let _ = writeln!(
                s,
                "- #{} {} → `reeve undo {}` ({what})",
                r.seq, r.tool, r.seq
            );
        }
    }
    let snaps: Vec<&Receipt> = receipts.iter().filter(|r| r.snapshot.is_some()).collect();
    if !snaps.is_empty() {
        let _ = writeln!(s, "\n## Snapshots\n");
        for r in snaps {
            if let Some(p) = &r.snapshot {
                let post = p.post.map(|n| n.to_string()).unwrap_or_else(|| "?".into());
                let _ = writeln!(
                    s,
                    "- #{}: snapper `{}` {}..{} (`sudo snapper -c {} undochange {}..{}`)",
                    r.seq, p.config, p.pre, post, p.config, p.pre, post
                );
            }
        }
    }
    let problems: Vec<&Receipt> = receipts
        .iter()
        .filter(|r| {
            matches!(
                r.outcome.status,
                Status::Error | Status::Denied | Status::Refused
            )
        })
        .collect();
    if !problems.is_empty() {
        let _ = writeln!(s, "\n## Failed or declined\n");
        for r in problems {
            let _ = writeln!(
                s,
                "- #{} {} {}: {}",
                r.seq,
                r.tool,
                clip(&r.target(), 60),
                clip(&r.outcome.summary, 120)
            );
        }
    }
    s
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

fn cell(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::Tier;

    #[test]
    fn a_report_lists_asks_actions_and_undos() {
        let meta = Meta {
            id: "s1".into(),
            started: Utc::now(),
            connection: "openrouter".into(),
            model: "m".into(),
            reflected: None,
        };
        let t = vec![
            Message::new("user", "tidy my downloads"),
            Message::new("assistant", "ok"),
        ];
        let mut r1 = Receipt::draft(
            "s1",
            "fs_delete",
            serde_json::json!({"path": "~/Downloads/x.iso"}),
            Tier::T1,
        );
        r1.seq = 1;
        r1.outcome.summary = "deleted ~/Downloads/x.iso".into();
        r1.undo = Some(Undo::Files { changes: vec![] });
        let mut r2 = Receipt::draft(
            "s1",
            "shell",
            serde_json::json!({"command": "rm -rf /"}),
            Tier::T3,
        );
        r2.seq = 2;
        r2.outcome.status = Status::Denied;
        let md = render(&meta, &t, &[r1, r2], &Tally::default());
        assert!(md.contains("1. tidy my downloads"));
        assert!(md.contains("`reeve undo 1`"));
        assert!(md.contains("## Failed or declined") && md.contains("#2 shell"));
    }
}
