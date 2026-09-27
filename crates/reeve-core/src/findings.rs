//! Findings: what the observer noticed. One JSON file per finding in
//! `~/.reeve/findings/`, shared by `reeved` (which opens, updates, and
//! resolves them) and the TUI (which acknowledges or dismisses them and
//! turns them into work).
//!
//! Each finding has a stable id (`disk-full:/boot`, `unit-failed:x.service`)
//! so a condition that persists is one finding, not a stream of them.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// How much it matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Worth knowing.
    Info,
    /// Worth fixing.
    Warning,
    /// Fix soon.
    Critical,
}

impl Severity {
    /// Parse `info`, `warning`, `critical`.
    pub fn parse(s: &str) -> Self {
        match s.trim() {
            "critical" => Self::Critical,
            "info" => Self::Info,
            _ => Self::Warning,
        }
    }

    /// Lowercase name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Critical => "critical",
        }
    }
}

/// Where it stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingStatus {
    /// Happening, not yet looked at.
    Open,
    /// Seen by the owner: still tracked, no more notifications.
    Acknowledged,
    /// The condition went away.
    Resolved,
    /// The owner doesn't want to hear about it: stays quiet even if it recurs.
    Dismissed,
}

/// A drafted fix, waiting for the owner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Proposal {
    /// The plan, in Markdown.
    pub text: String,
    /// When.
    pub drafted_at: DateTime<Utc>,
    /// By which model.
    pub model: String,
    /// What it cost.
    #[serde(default)]
    pub usd: Option<f64>,
}

/// One finding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Finding {
    /// Stable id: `<kind>:<subject>`.
    pub id: String,
    /// Severity (can rise while open).
    pub severity: Severity,
    /// One line.
    pub title: String,
    /// A few lines of explanation, with numbers.
    pub detail: String,
    /// Log lines or readings behind it. Written by programs on the machine:
    /// data, not instructions.
    #[serde(default)]
    pub evidence: Vec<String>,
    /// First seen.
    pub first_seen: DateTime<Utc>,
    /// Last seen active.
    pub last_seen: DateTime<Utc>,
    /// Times seen (ticks, or matching log lines).
    pub count: u64,
    /// Status.
    pub status: FindingStatus,
    /// When it resolved.
    #[serde(default)]
    pub resolved_at: Option<DateTime<Utc>>,
    /// When the owner was last notified.
    #[serde(default)]
    pub notified_at: Option<DateTime<Utc>>,
    /// Severity last notified at.
    #[serde(default)]
    pub notified_severity: Option<Severity>,
    /// A drafted fix.
    #[serde(default)]
    pub proposal: Option<Proposal>,
    /// Why a draft wasn't made (budget reached, no priced model…).
    #[serde(default)]
    pub draft_note: Option<String>,
}

impl Finding {
    /// The kind: the id before its first `:`.
    pub fn kind(&self) -> &str {
        self.id.split(':').next().unwrap_or(&self.id)
    }

    /// Still needs attention.
    pub fn is_live(&self) -> bool {
        matches!(
            self.status,
            FindingStatus::Open | FindingStatus::Acknowledged
        )
    }
}

/// What a detector reports on a tick.
#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    /// Stable id.
    pub id: String,
    /// Severity now.
    pub severity: Severity,
    /// One line.
    pub title: String,
    /// Detail.
    pub detail: String,
    /// Evidence.
    pub evidence: Vec<String>,
    /// How many occurrences this signal stands for (log lines), else 1.
    pub count: u64,
}

/// The findings directory.
#[derive(Debug, Clone)]
pub struct FindingStore {
    dir: PathBuf,
}

fn file_name(id: &str) -> String {
    let safe: String = id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "{}-{}.json",
        &safe[..safe.len().min(80)],
        &crate::undo::sha256_hex(id.as_bytes())[..8]
    )
}

impl FindingStore {
    /// Findings under `reeve_home/findings`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("findings"),
        }
    }

    fn path(&self, id: &str) -> PathBuf {
        self.dir.join(file_name(id))
    }

    /// Every finding, most severe and most recent first.
    pub fn list(&self) -> Vec<Finding> {
        let mut v: Vec<Finding> = fs::read_dir(&self.dir)
            .map(|rd| {
                rd.flatten()
                    .filter_map(|e| {
                        serde_json::from_str::<Finding>(&fs::read_to_string(e.path()).ok()?).ok()
                    })
                    .collect()
            })
            .unwrap_or_default();
        v.sort_by(|a, b| {
            b.is_live()
                .cmp(&a.is_live())
                .then(b.severity.cmp(&a.severity))
                .then(b.last_seen.cmp(&a.last_seen))
        });
        v
    }

    /// One finding.
    pub fn get(&self, id: &str) -> Option<Finding> {
        serde_json::from_str(&fs::read_to_string(self.path(id)).ok()?).ok()
    }

    /// Write one finding (atomically).
    pub fn put(&self, f: &Finding) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_string_pretty(f).map_err(|e| Error::Io(e.to_string()))?;
        crate::undo::write_atomic(&self.path(&f.id), json.as_bytes(), Some(0o600))
    }

    /// Record a signal: open a new finding, or update the one it belongs to.
    /// Returns the finding and whether the owner should be notified.
    pub fn observe(&self, s: Signal, now: DateTime<Utc>) -> Result<(Finding, bool)> {
        let mut f = match self.get(&s.id) {
            Some(mut f) => {
                if f.status == FindingStatus::Resolved {
                    // It came back: a fresh episode.
                    f.status = FindingStatus::Open;
                    f.first_seen = now;
                    f.count = 0;
                    f.resolved_at = None;
                    f.proposal = None;
                    f.draft_note = None;
                    f.notified_at = None;
                    f.notified_severity = None;
                }
                f.severity = s.severity;
                f.title = s.title;
                f.detail = s.detail;
                if !s.evidence.is_empty() {
                    f.evidence = s.evidence;
                }
                f.last_seen = now;
                f.count += s.count;
                f
            }
            None => Finding {
                id: s.id,
                severity: s.severity,
                title: s.title,
                detail: s.detail,
                evidence: s.evidence,
                first_seen: now,
                last_seen: now,
                count: s.count,
                status: FindingStatus::Open,
                resolved_at: None,
                notified_at: None,
                notified_severity: None,
                proposal: None,
                draft_note: None,
            },
        };
        // Notify once per episode, again if it got worse, and at most every 6 h.
        let quiet = f.status != FindingStatus::Open;
        let worse = f.notified_severity.is_none_or(|n| f.severity > n);
        let stale = f
            .notified_at
            .is_none_or(|t| now - t > chrono::Duration::hours(6))
            && f.notified_severity.is_none();
        let notify = !quiet && (worse || stale);
        if notify {
            f.notified_at = Some(now);
            f.notified_severity = Some(f.severity);
        }
        self.put(&f)?;
        Ok((f, notify))
    }

    /// Resolve live findings of `kind` that weren't seen since `before`.
    pub fn resolve_missing(
        &self,
        kind: &str,
        seen: &[String],
        now: DateTime<Utc>,
    ) -> Result<Vec<Finding>> {
        let mut out = Vec::new();
        for mut f in self.list() {
            if f.kind() == kind && f.is_live() && !seen.contains(&f.id) {
                f.status = FindingStatus::Resolved;
                f.resolved_at = Some(now);
                self.put(&f)?;
                out.push(f);
            }
        }
        Ok(out)
    }

    /// Resolve live findings whose last sighting is older than `age`.
    pub fn resolve_stale(
        &self,
        kind: &str,
        age: chrono::Duration,
        now: DateTime<Utc>,
    ) -> Result<()> {
        for mut f in self.list() {
            if f.kind() == kind && f.is_live() && now - f.last_seen > age {
                f.status = FindingStatus::Resolved;
                f.resolved_at = Some(now);
                self.put(&f)?;
            }
        }
        Ok(())
    }

    /// Set a finding's status (the owner acknowledging or dismissing it).
    pub fn set_status(&self, id: &str, status: FindingStatus) -> Result<()> {
        let mut f = self
            .get(id)
            .ok_or_else(|| Error::Io(format!("no finding {id}")))?;
        f.status = status;
        self.put(&f)
    }

    /// Forget resolved findings older than `days`.
    pub fn prune(&self, days: i64, now: DateTime<Utc>) {
        for f in self.list() {
            if f.status == FindingStatus::Resolved
                && f.resolved_at
                    .is_some_and(|t| now - t > chrono::Duration::days(days))
            {
                let _ = fs::remove_file(self.path(&f.id));
            }
        }
    }
}

/// `reeved`'s heartbeat, in `~/.reeve/observer/status.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ObserverStatus {
    /// Process id.
    pub pid: u32,
    /// When it started.
    pub started: Option<DateTime<Utc>>,
    /// Last heartbeat.
    pub beat: Option<DateTime<Utc>>,
    /// Whether it can read the system journal.
    pub journal: bool,
    /// Drafter on.
    pub drafter: bool,
    /// Drafter spend today, USD.
    pub drafter_usd_today: f64,
    /// Drafts made today.
    pub drafts_today: u32,
    /// Last problem it hit, if any.
    pub last_error: Option<String>,
}

impl ObserverStatus {
    /// The status file.
    pub fn path(reeve_home: &Path) -> PathBuf {
        reeve_home.join("observer").join("status.json")
    }

    /// Read it.
    pub fn load(reeve_home: &Path) -> Option<Self> {
        serde_json::from_str(&fs::read_to_string(Self::path(reeve_home)).ok()?).ok()
    }

    /// Write it.
    pub fn save(&self, reeve_home: &Path) -> Result<()> {
        let p = Self::path(reeve_home);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Io(e.to_string()))?;
        crate::undo::write_atomic(&p, json.as_bytes(), Some(0o600))
    }

    /// Alive: a heartbeat in the last 45 seconds.
    pub fn alive(&self, now: DateTime<Utc>) -> bool {
        self.beat
            .is_some_and(|b| now - b < chrono::Duration::seconds(45))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sig(id: &str, sev: Severity) -> Signal {
        Signal {
            id: id.into(),
            severity: sev,
            title: "t".into(),
            detail: "d".into(),
            evidence: vec![],
            count: 1,
        }
    }

    #[test]
    fn a_persisting_condition_is_one_finding_and_one_notification() {
        let d = tempfile::tempdir().unwrap();
        let s = FindingStore::new(d.path());
        let t0 = Utc::now();
        let (_, n1) = s.observe(sig("swap-full", Severity::Warning), t0).unwrap();
        let (f, n2) = s
            .observe(
                sig("swap-full", Severity::Warning),
                t0 + chrono::Duration::minutes(1),
            )
            .unwrap();
        assert!(n1 && !n2);
        assert_eq!(f.count, 2);
        // Worse: notify again.
        let (_, n3) = s
            .observe(
                sig("swap-full", Severity::Critical),
                t0 + chrono::Duration::minutes(2),
            )
            .unwrap();
        assert!(n3);
        assert_eq!(s.list().len(), 1);
    }

    #[test]
    fn resolving_and_recurring() {
        let d = tempfile::tempdir().unwrap();
        let s = FindingStore::new(d.path());
        let t = Utc::now();
        s.observe(sig("disk-full:/boot", Severity::Warning), t)
            .unwrap();
        s.observe(sig("disk-full:/", Severity::Warning), t).unwrap();
        let gone = s
            .resolve_missing("disk-full", &["disk-full:/".into()], t)
            .unwrap();
        assert_eq!(gone.len(), 1);
        assert_eq!(
            s.get("disk-full:/boot").unwrap().status,
            FindingStatus::Resolved
        );
        let (f, notify) = s
            .observe(sig("disk-full:/boot", Severity::Warning), t)
            .unwrap();
        assert!(notify && f.status == FindingStatus::Open && f.count == 1);
    }

    #[test]
    fn dismissed_stays_quiet() {
        let d = tempfile::tempdir().unwrap();
        let s = FindingStore::new(d.path());
        let t = Utc::now();
        s.observe(sig("unit-failed:x.service", Severity::Warning), t)
            .unwrap();
        s.set_status("unit-failed:x.service", FindingStatus::Dismissed)
            .unwrap();
        let (f, notify) = s
            .observe(sig("unit-failed:x.service", Severity::Critical), t)
            .unwrap();
        assert!(!notify && f.status == FindingStatus::Dismissed);
    }

    #[test]
    fn odd_ids_make_safe_file_names() {
        assert!(!file_name("journal:../../etc/passwd").contains('/'));
        assert_ne!(file_name("a:b"), file_name("a_b"));
    }
}
