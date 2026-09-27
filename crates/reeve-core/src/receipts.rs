//! Receipts: one JSON line per action in `~/.reeve/receipts/YYYY-MM.jsonl`,
//! each hashing the one before it, so an edited or deleted line shows.
//!
//! `hash = sha256(prev ‖ "\n" ‖ json(receipt without hash))`, hex.

use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::policy::Tier;
use crate::undo::{Undo, sha256_hex};

/// How an action ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Done.
    Ok,
    /// Tried and failed.
    Error,
    /// The person said no.
    Denied,
    /// The policy said no.
    Refused,
}

/// The result part of a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    /// Status.
    pub status: Status,
    /// Exit code, for commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit: Option<i32>,
    /// One line on what happened.
    pub summary: String,
    /// Hash of the full output, which isn't stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_sha256: Option<String>,
}

/// One action.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    /// Position in the chain, from 1.
    pub seq: u64,
    /// When it finished.
    pub ts: DateTime<Utc>,
    /// Session (or standing-order run).
    pub session: String,
    /// Previous receipt's hash; empty for the first.
    pub prev: String,
    /// This receipt's hash.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub hash: String,
    /// Tool name.
    pub tool: String,
    /// Arguments, with file contents replaced by their size and hash.
    pub args: serde_json::Value,
    /// Tier.
    pub tier: Tier,
    /// Why the tier.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
    /// `policy`, `user`, `session-rule`, `yolo`, `order:<name>`, or `-` when not approved.
    pub approved_by: String,
    /// The model's stated reason.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub why: Option<String>,
    /// Result.
    pub outcome: Outcome,
    /// How to reverse it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo: Option<Undo>,
    /// This receipt reverses that one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<u64>,
    /// Snapper snapshots taken around it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<crate::snapshots::SnapPair>,
}

impl Receipt {
    /// A receipt to be filled in by [`ReceiptBook::append`].
    pub fn draft(session: &str, tool: &str, args: serde_json::Value, tier: Tier) -> Self {
        Self {
            seq: 0,
            ts: Utc::now(),
            session: session.into(),
            prev: String::new(),
            hash: String::new(),
            tool: tool.into(),
            args,
            tier,
            reasons: Vec::new(),
            approved_by: "-".into(),
            why: None,
            outcome: Outcome {
                status: Status::Ok,
                exit: None,
                summary: String::new(),
                output_sha256: None,
            },
            undo: None,
            undoes: None,
            snapshot: None,
        }
    }

    fn compute_hash(&self) -> Result<String> {
        let mut bare = self.clone();
        bare.hash.clear();
        let json = serde_json::to_string(&bare).map_err(|e| Error::Io(e.to_string()))?;
        Ok(sha256_hex(format!("{}\n{json}", self.prev).as_bytes()))
    }

    /// What it acted on, for one-line lists: the command, path, unit,
    /// packages, query, or process.
    pub fn target(&self) -> String {
        for k in ["command", "path", "from", "unit", "name", "query"] {
            if let Some(v) = self.args.get(k).and_then(|v| v.as_str()) {
                return v.to_string();
            }
        }
        if let Some(list) = self.args.get("packages").and_then(|v| v.as_array()) {
            let names: Vec<&str> = list.iter().filter_map(|v| v.as_str()).collect();
            return if names.is_empty() {
                "all packages".into()
            } else {
                names.join(" ")
            };
        }
        if let Some(pid) = self.args.get("pid").and_then(serde_json::Value::as_u64) {
            return format!("pid {pid}");
        }
        if let Some(n) = self.undoes {
            return format!("#{n}");
        }
        String::new()
    }
}

/// The receipt chain on disk.
#[derive(Debug, Clone)]
pub struct ReceiptBook {
    dir: PathBuf,
}

/// Result of walking the chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verify {
    /// Receipts checked.
    pub count: u64,
    /// The first problem, if any.
    pub problem: Option<String>,
}

impl ReceiptBook {
    /// Receipts under `reeve_home/receipts`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("receipts"),
        }
    }

    fn months(&self) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    /// Seal and append a receipt: sets `seq`, `prev`, `hash`. Holds a lock
    /// so two Reeve processes can't fork the chain.
    pub fn append(&self, mut r: Receipt) -> Result<Receipt> {
        fs::create_dir_all(&self.dir)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(".lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|e| Error::Io(format!("receipt lock: {e}")))?;
        let head = self.head()?;
        r.seq = head.as_ref().map_or(1, |h| h.seq + 1);
        r.prev = head.map(|h| h.hash).unwrap_or_default();
        r.hash = r.compute_hash()?;
        let month = r.ts.format("%Y-%m").to_string();
        let mut line = serde_json::to_string(&r).map_err(|e| Error::Io(e.to_string()))?;
        line.push('\n');
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(format!("{month}.jsonl")))?;
        f.write_all(line.as_bytes())?;
        f.sync_data()?;
        drop(lock);
        Ok(r)
    }

    /// The newest receipt.
    pub fn head(&self) -> Result<Option<Receipt>> {
        for file in self.months().iter().rev() {
            if let Some(line) = last_line(file)? {
                return serde_json::from_str(&line).map(Some).map_err(|e| {
                    Error::Io(format!("{}: last receipt unreadable: {e}", file.display()))
                });
            }
        }
        Ok(None)
    }

    /// Every receipt, oldest first. Unreadable lines are skipped here;
    /// [`verify`](Self::verify) reports them.
    pub fn all(&self) -> Vec<Receipt> {
        self.months()
            .iter()
            .filter_map(|f| fs::read_to_string(f).ok())
            .flat_map(|t| {
                t.lines()
                    .filter_map(|l| serde_json::from_str::<Receipt>(l).ok())
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    /// The newest `n`, newest first.
    pub fn recent(&self, n: usize) -> Vec<Receipt> {
        let mut out = Vec::new();
        for f in self.months().iter().rev() {
            let Ok(text) = fs::read_to_string(f) else {
                continue;
            };
            for l in text.lines().rev() {
                if let Ok(r) = serde_json::from_str::<Receipt>(l) {
                    out.push(r);
                    if out.len() == n {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// One receipt by number.
    pub fn find(&self, seq: u64) -> Option<Receipt> {
        self.all().into_iter().find(|r| r.seq == seq)
    }

    /// The receipt that undid `seq`, if any.
    pub fn undone_by(&self, seq: u64) -> Option<u64> {
        self.all()
            .into_iter()
            .find(|r| r.undoes == Some(seq) && r.outcome.status == Status::Ok)
            .map(|r| r.seq)
    }

    /// The receipt to undo and its undo record, or why it can't be undone.
    pub fn undo_target(&self, seq: u64) -> Result<(Receipt, Undo)> {
        let target = self
            .find(seq)
            .ok_or_else(|| Error::Io(format!("there's no receipt #{seq}")))?;
        let Some(undo) = target.undo.clone() else {
            return Err(Error::Io(format!(
                "#{seq} ({}) has nothing to undo",
                target.tool
            )));
        };
        if matches!(target.outcome.status, Status::Denied | Status::Refused) {
            return Err(Error::Io(format!("#{seq} never ran")));
        }
        if let Some(by) = self.undone_by(seq) {
            return Err(Error::Io(format!("#{seq} was already undone by #{by}")));
        }
        Ok((target, undo))
    }

    /// Write the receipt for an undo attempt (successful or not).
    pub fn record_undo(
        &self,
        target: &Receipt,
        session: &str,
        result: std::result::Result<(Undo, String), String>,
    ) -> Result<Receipt> {
        let mut r = Receipt::draft(
            session,
            "undo",
            serde_json::json!({ "seq": target.seq }),
            target.tier,
        );
        r.approved_by = "user".into();
        r.undoes = Some(target.seq);
        r.why = Some(format!("undo #{}: {}", target.seq, target.outcome.summary));
        match result {
            Ok((inverse, summary)) => {
                r.outcome = Outcome {
                    status: Status::Ok,
                    exit: None,
                    summary,
                    output_sha256: None,
                };
                r.undo = Some(inverse);
                self.append(r)
            }
            Err(e) => {
                r.outcome = Outcome {
                    status: Status::Error,
                    exit: None,
                    summary: e.clone(),
                    output_sha256: None,
                };
                self.append(r)?;
                Err(Error::Io(e))
            }
        }
    }

    /// Undo a user-level file change (no sudo). See `tools::undo_receipt`
    /// for everything else.
    pub fn undo(&self, store: &crate::undo::UndoStore, seq: u64, session: &str) -> Result<Receipt> {
        let (target, undo) = self.undo_target(seq)?;
        let result = store.revert(&undo).map_err(|e| e.to_string());
        self.record_undo(&target, session, result)
    }

    /// Walk the whole chain: sequence, links, and hashes.
    pub fn verify(&self) -> Verify {
        let mut count = 0;
        let mut prev_hash = String::new();
        let mut prev_seq = 0;
        for f in self.months() {
            let name = f
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let text = match fs::read_to_string(&f) {
                Ok(t) => t,
                Err(e) => {
                    return Verify {
                        count,
                        problem: Some(format!("{name}: {e}")),
                    };
                }
            };
            for (i, line) in text.lines().enumerate() {
                let at = format!("{name} line {}", i + 1);
                let r: Receipt = match serde_json::from_str(line) {
                    Ok(r) => r,
                    Err(e) => {
                        return Verify {
                            count,
                            problem: Some(format!("{at}: unreadable ({e})")),
                        };
                    }
                };
                if r.seq != prev_seq + 1 {
                    return Verify {
                        count,
                        problem: Some(format!(
                            "{at}: expected #{}, found #{} — receipts are missing",
                            prev_seq + 1,
                            r.seq
                        )),
                    };
                }
                if r.prev != prev_hash {
                    return Verify {
                        count,
                        problem: Some(format!(
                            "#{}: doesn't link to #{prev_seq} — something before it was changed",
                            r.seq
                        )),
                    };
                }
                match r.compute_hash() {
                    Ok(h) if h == r.hash => {}
                    _ => {
                        return Verify {
                            count,
                            problem: Some(format!(
                                "#{}: its contents were changed after it was written",
                                r.seq
                            )),
                        };
                    }
                }
                prev_hash = r.hash;
                prev_seq = r.seq;
                count += 1;
            }
        }
        Verify {
            count,
            problem: None,
        }
    }
}

/// The last non-empty line of a file, read from the end.
fn last_line(path: &Path) -> Result<Option<String>> {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = f.metadata()?.len();
    let mut take = 64 * 1024u64;
    loop {
        let start = len.saturating_sub(take);
        f.seek(SeekFrom::Start(start))?;
        let mut buf = String::new();
        f.read_to_string(&mut buf)?;
        let trimmed = buf.trim_end();
        if let Some(i) = trimmed.rfind('\n') {
            return Ok(Some(trimmed[i + 1..].to_string()));
        }
        if start == 0 {
            return Ok((!trimmed.is_empty()).then(|| trimmed.to_string()));
        }
        take *= 4;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn book() -> (tempfile::TempDir, ReceiptBook) {
        let d = tempfile::tempdir().unwrap();
        let b = ReceiptBook::new(d.path());
        (d, b)
    }

    #[test]
    fn the_chain_links_and_verifies() {
        let (_d, b) = book();
        for i in 0..5 {
            let mut r = Receipt::draft(
                "s",
                "fs_read",
                json!({"path": format!("/etc/f{i}")}),
                Tier::T0,
            );
            r.approved_by = "policy".into();
            b.append(r).unwrap();
        }
        assert_eq!(b.head().unwrap().unwrap().seq, 5);
        assert_eq!(
            b.verify(),
            Verify {
                count: 5,
                problem: None
            }
        );
        assert_eq!(b.recent(2)[0].seq, 5);
        assert_eq!(b.find(3).unwrap().target(), "/etc/f2");
    }

    #[test]
    fn an_edited_receipt_is_caught() {
        let (d, b) = book();
        for _ in 0..3 {
            b.append(Receipt::draft(
                "s",
                "shell",
                json!({"command": "rm ~/x"}),
                Tier::T1,
            ))
            .unwrap();
        }
        let file = b.months().pop().unwrap();
        let text = fs::read_to_string(&file)
            .unwrap()
            .replacen("rm ~/x", "ls ~/x", 1);
        fs::write(&file, text).unwrap();
        let v = b.verify();
        assert!(v.problem.as_deref().unwrap().contains("#1"), "{v:?}");
        drop(d);
    }

    #[test]
    fn a_deleted_receipt_is_caught() {
        let (_d, b) = book();
        for _ in 0..3 {
            b.append(Receipt::draft("s", "fs_read", json!({}), Tier::T0))
                .unwrap();
        }
        let file = b.months().pop().unwrap();
        let text: Vec<String> = fs::read_to_string(&file)
            .unwrap()
            .lines()
            .map(String::from)
            .collect();
        fs::write(&file, format!("{}\n{}\n", text[0], text[2])).unwrap();
        assert!(b.verify().problem.unwrap().contains("missing"));
    }
}
