//! The global spend ledger: every priced (or unpriced) model call Reeve
//! makes, from the TUI or the daemon, in `~/.reeve/spend/YYYY-MM.jsonl`.
//! Day and month budgets count from here.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use crate::config::SpendConfig;
use crate::error::Result;
use crate::spend::{Tally, Usage};

/// One model call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SpendRecord {
    /// When the call finished.
    pub ts: DateTime<Utc>,
    /// Session (or standing-order run) that spent it.
    pub session: String,
    /// Connection name.
    pub connection: String,
    /// Model id.
    pub model: String,
    /// Token counts.
    #[serde(flatten)]
    pub usage: Usage,
    /// USD, or omitted when unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usd: Option<f64>,
    /// Where `usd` came from: `provider` (reported), `book` (price × tokens),
    /// or `local` (free).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priced_by: Option<String>,
    /// Who spent it, when not the main agent: `drafter`, `reflect`, `order:<name>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// Ledger file for a month.
pub fn month_path(home: &Path, year: i32, month: u32) -> PathBuf {
    home.join("spend")
        .join(format!("{year:04}-{month:02}.jsonl"))
}

/// Append a record to the ledger.
pub fn record(home: &Path, rec: &SpendRecord) -> Result<()> {
    let local = rec.ts.with_timezone(&Local);
    let path = month_path(home, local.year(), local.month());
    append_jsonl(&path, rec)
}

/// Append one JSON line, creating parent directories.
pub fn append_jsonl<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut line = serde_json::to_string(value).map_err(|e| crate::Error::Io(e.to_string()))?;
    line.push('\n');
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    f.write_all(line.as_bytes())?;
    Ok(())
}

/// What has been spent today and this month, plus a per-day series for the
/// month (index 0 = the 1st).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Totals {
    /// Today, local time.
    pub today: Tally,
    /// This calendar month, local time.
    pub month: Tally,
    /// USD per day of this month so far.
    pub by_day: Vec<f64>,
}

/// Read this month's ledger. Unreadable lines are skipped: a torn final
/// line from a crash must not hide the rest of the month.
pub fn totals(home: &Path, now: DateTime<Local>) -> Totals {
    let today = now.date_naive();
    let path = month_path(home, now.year(), now.month());
    let mut t = Totals {
        by_day: vec![0.0; today.day() as usize],
        ..Totals::default()
    };
    let Ok(text) = fs::read_to_string(path) else {
        return t;
    };
    for rec in text
        .lines()
        .filter_map(|l| serde_json::from_str::<SpendRecord>(l).ok())
    {
        let day: NaiveDate = rec.ts.with_timezone(&Local).date_naive();
        if day.year() != today.year() || day.month() != today.month() {
            continue;
        }
        t.month.add(rec.usd, rec.usage);
        if day == today {
            t.today.add(rec.usd, rec.usage);
        }
        if let Some(slot) = t.by_day.get_mut(day.day0() as usize) {
            *slot += rec.usd.unwrap_or(0.0);
        }
    }
    t
}

/// Every record since `since`, oldest first (reads the months it spans).
pub fn since(home: &Path, since: DateTime<Utc>) -> Vec<SpendRecord> {
    let mut out = Vec::new();
    let start = since.with_timezone(&Local);
    let now = Local::now();
    let (mut y, mut m) = (start.year(), start.month());
    while (y, m) <= (now.year(), now.month()) {
        if let Ok(text) = fs::read_to_string(month_path(home, y, m)) {
            out.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<SpendRecord>(l).ok())
                    .filter(|r| r.ts >= since),
            );
        }
        (y, m) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    }
    out.sort_by_key(|r| r.ts);
    out
}

/// Who spent it, for grouping: `chat`, `drafter`, `reflect`, `orders`.
pub fn role_name(role: Option<&str>) -> &str {
    match role {
        None => "chat",
        Some(r) if r.starts_with("order:") => "orders",
        Some(r) => r,
    }
}

/// One line of a statement: a session's (or a draft's) calls by one role.
#[derive(Debug, Clone, PartialEq)]
pub struct StatementLine {
    /// First call.
    pub first: DateTime<Utc>,
    /// Session id.
    pub session: String,
    /// Role, as [`role_name`] gives it.
    pub role: String,
    /// The model of its last call.
    pub model: String,
    /// Calls.
    pub calls: u64,
    /// Tokens.
    pub usage: Usage,
    /// USD of the priced calls.
    pub usd: f64,
    /// Calls with no known price.
    pub unpriced: u64,
}

/// Group records into statement lines, in order of their first call.
pub fn statement(records: &[SpendRecord]) -> Vec<StatementLine> {
    let mut lines: Vec<StatementLine> = Vec::new();
    for r in records {
        let role = role_name(r.role.as_deref()).to_string();
        match lines
            .iter_mut()
            .find(|l| l.session == r.session && l.role == role)
        {
            Some(l) => {
                l.calls += 1;
                l.usage = l.usage + r.usage;
                l.usd += r.usd.unwrap_or(0.0);
                l.unpriced += u64::from(r.usd.is_none());
                l.model.clone_from(&r.model);
            }
            None => lines.push(StatementLine {
                first: r.ts,
                session: r.session.clone(),
                role,
                model: r.model.clone(),
                calls: 1,
                usage: r.usage,
                usd: r.usd.unwrap_or(0.0),
                unpriced: u64::from(r.usd.is_none()),
            }),
        }
    }
    lines.sort_by_key(|l| l.first);
    lines
}

/// Today's spend by one role.
pub fn role_today(home: &Path, role: &str, now: DateTime<Local>) -> Tally {
    let path = month_path(home, now.year(), now.month());
    let mut t = Tally::default();
    let Ok(text) = fs::read_to_string(path) else {
        return t;
    };
    for rec in text
        .lines()
        .filter_map(|l| serde_json::from_str::<SpendRecord>(l).ok())
    {
        if rec.role.as_deref() == Some(role)
            && rec.ts.with_timezone(&Local).date_naive() == now.date_naive()
        {
            t.add(rec.usd, rec.usage);
        }
    }
    t
}

/// Which cap, if any, stops the next call. Caps of 0 are off.
pub fn over_cap(spend: &SpendConfig, session: &Tally, totals: &Totals) -> Option<String> {
    let checks = [
        ("session", spend.session_usd, session.usd),
        ("daily", spend.daily_usd, totals.today.usd),
        ("monthly", spend.monthly_usd, totals.month.usd),
    ];
    checks
        .into_iter()
        .find(|(_, cap, used)| *cap > 0.0 && used >= cap)
        .map(|(name, cap, used)| format!("{name} cap reached: ${used:.4} of ${cap:.2}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statements_group_by_session_and_role() {
        let rec = |mins: i64, session: &str, role: Option<&str>, usd: Option<f64>| SpendRecord {
            ts: Utc::now() - chrono::Duration::minutes(100 - mins),
            session: session.into(),
            connection: "openrouter".into(),
            model: "m".into(),
            usage: Usage {
                input_tokens: 100,
                ..Usage::default()
            },
            usd,
            priced_by: None,
            role: role.map(String::from),
        };
        let lines = statement(&[
            rec(1, "a", None, Some(0.01)),
            rec(2, "d", Some("drafter"), Some(0.02)),
            rec(3, "a", None, Some(0.03)),
            rec(4, "a", Some("reflect"), None),
            rec(5, "o", Some("order:disk"), Some(0.001)),
        ]);
        let got: Vec<(&str, &str, u64)> = lines
            .iter()
            .map(|l| (l.session.as_str(), l.role.as_str(), l.calls))
            .collect();
        assert_eq!(
            got,
            [
                ("a", "chat", 2),
                ("d", "drafter", 1),
                ("a", "reflect", 1),
                ("o", "orders", 1)
            ]
        );
        assert!((lines[0].usd - 0.04).abs() < 1e-9);
        assert_eq!(lines[0].usage.input_tokens, 200);
        assert_eq!(lines[2].unpriced, 1);
    }
    use chrono::TimeZone;

    fn rec(ts: DateTime<Utc>, usd: Option<f64>) -> SpendRecord {
        SpendRecord {
            ts,
            session: "s".into(),
            connection: "openrouter".into(),
            model: "m".into(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 5,
                ..Usage::default()
            },
            usd,
            priced_by: Some("provider".into()),
            role: None,
        }
    }

    #[test]
    fn totals_split_today_from_the_month() {
        let home = tempfile::tempdir().unwrap();
        let now = Local.with_ymd_and_hms(2026, 9, 26, 12, 0, 0).unwrap();
        let earlier = Local.with_ymd_and_hms(2026, 9, 3, 12, 0, 0).unwrap();
        record(home.path(), &rec(now.with_timezone(&Utc), Some(0.5))).unwrap();
        record(home.path(), &rec(now.with_timezone(&Utc), None)).unwrap();
        record(home.path(), &rec(earlier.with_timezone(&Utc), Some(1.0))).unwrap();
        // A torn line doesn't hide the rest.
        let path = month_path(home.path(), 2026, 9);
        let mut f = fs::OpenOptions::new().append(true).open(&path).unwrap();
        f.write_all(b"{\"ts\":").unwrap();
        let t = totals(home.path(), now);
        assert!((t.today.usd - 0.5).abs() < 1e-9);
        assert!(t.today.partial);
        assert!((t.month.usd - 1.5).abs() < 1e-9);
        assert_eq!(t.by_day.len(), 26);
        assert!((t.by_day[2] - 1.0).abs() < 1e-9);
        assert_eq!(t.month.calls, 3);
    }

    #[test]
    fn caps_of_zero_are_off() {
        let mut spend = SpendConfig::default();
        let mut session = Tally::default();
        session.add(Some(5.0), Usage::default());
        assert_eq!(over_cap(&spend, &session, &Totals::default()), None);
        spend.session_usd = 5.0;
        assert!(
            over_cap(&spend, &session, &Totals::default())
                .unwrap()
                .starts_with("session")
        );
    }
}
