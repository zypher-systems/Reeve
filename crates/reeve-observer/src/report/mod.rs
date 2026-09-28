//! The state of the machine as one page: `reeve report` and `/report`.
//!
//! Everything comes from what's already on disk (reeved's minute metrics,
//! findings, receipts, the spend ledger, memory) plus a look at packages,
//! units, and `/etc` for what changed. It's drawn locally into a single
//! HTML file with inline SVG: no model, no cost, no network, and nothing
//! leaves the machine.

pub mod drift;
mod html;
mod svg;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use serde::Deserialize;

use reeve_core::findings::{Finding, FindingStore, ObserverStatus, Severity};
use reeve_core::ledger::SpendRecord;
use reeve_core::memory::{Layer, Memory};
use reeve_core::policy::Tier;
use reeve_core::receipts::{Receipt, ReceiptBook, Status};

use crate::HostInfo;

/// One minute as reeved wrote it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MetricRow {
    /// When.
    pub ts: DateTime<Utc>,
    /// CPU % (average over the minute).
    pub cpu: f32,
    /// CPU % (highest sample).
    pub cpu_max: f32,
    /// Memory in use, 0–1.
    pub mem: f64,
    /// Swap in use, 0–1.
    pub swap: f64,
    /// 1-minute load.
    pub load1: f32,
    /// °C.
    #[serde(default)]
    pub temp: Option<f32>,
    /// Bytes/s received.
    pub rx: f64,
    /// Bytes/s sent.
    pub tx: f64,
    /// Mount → use, 0–1.
    #[serde(default)]
    pub disks: BTreeMap<String, f64>,
}

/// One point on a chart: a bucket of minutes, or `None` where reeved wasn't running.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    /// CPU % average.
    pub cpu: f64,
    /// CPU % peak.
    pub cpu_max: f64,
    /// Memory %.
    pub mem: f64,
    /// Swap %.
    pub swap: f64,
    /// Load.
    pub load: f64,
    /// °C peak.
    pub temp: Option<f64>,
    /// Bytes/s in.
    pub rx: f64,
    /// Bytes/s out.
    pub tx: f64,
}

/// Summary numbers for one series.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Stat {
    /// Lowest.
    pub min: f64,
    /// Mean.
    pub avg: f64,
    /// Highest.
    pub max: f64,
    /// When the highest was.
    pub max_at: Option<DateTime<Utc>>,
}

/// A mount over the window.
#[derive(Debug, Clone, PartialEq)]
pub struct DiskTrend {
    /// Mount point.
    pub mount: String,
    /// Use now, 0–1.
    pub now: f64,
    /// Bytes.
    pub total: u64,
    /// Bytes.
    pub used: u64,
    /// Change in use per day, 0–1 (least squares over hourly means).
    pub per_day: f64,
    /// At this rate, days until full.
    pub days_to_full: Option<f64>,
    /// Hourly use for the sparkline.
    pub series: Vec<(DateTime<Utc>, f64)>,
}

/// How much a headline matters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    /// Needs you.
    Bad,
    /// Worth a look.
    Warn,
    /// Good to know.
    Info,
    /// Good news.
    Good,
}

/// One line at the top of the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Headline {
    /// How much it matters.
    pub tone: Tone,
    /// What.
    pub text: String,
}

/// Spend on one day, by role.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DaySpend {
    /// Local day.
    pub day: NaiveDate,
    /// `chat`, `drafter`, `orders`, `reflect` → USD.
    pub by_role: BTreeMap<String, f64>,
    /// Calls with no known price.
    pub unpriced: u32,
}

/// Memory at a glance.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemorySummary {
    /// Facts in use.
    pub facts: usize,
    /// Runbooks in use.
    pub runbooks: usize,
    /// Preferences in effect.
    pub preferences: usize,
    /// Waiting for the owner.
    pub pending: usize,
    /// Learned in the window.
    pub new: usize,
    /// Most-used runbooks: title, worked, failed.
    pub top_runbooks: Vec<(String, u32, u32)>,
}

/// Everything on the page.
#[derive(Debug, Clone)]
pub struct Report {
    /// When it was made.
    pub generated: DateTime<Local>,
    /// Window, in days.
    pub days: u32,
    /// Window start.
    pub since: DateTime<Utc>,
    /// The machine.
    pub host: HostInfo,
    /// Seconds.
    pub uptime_secs: u64,
    /// Buckets across the charted span, oldest first.
    pub points: Vec<(DateTime<Utc>, Option<Point>)>,
    /// Where the charts start: the window's start, or the first reading
    /// when reeved began recording later.
    pub chart_since: DateTime<Utc>,
    /// Minutes reeved recorded / minutes in the window.
    pub coverage: f64,
    /// CPU, memory, swap, load, temperature.
    pub cpu: Stat,
    /// Memory.
    pub mem: Stat,
    /// Swap.
    pub swap: Stat,
    /// Load.
    pub load: Stat,
    /// Temperature, when the machine reports one.
    pub temp: Option<Stat>,
    /// Share of minutes with swap over 95%.
    pub swap_full_share: f64,
    /// Disks.
    pub disks: Vec<DiskTrend>,
    /// Open findings, worst first.
    pub findings: Vec<Finding>,
    /// Resolved in the window.
    pub resolved: usize,
    /// What changed.
    pub drift: drift::Drift,
    /// Reeve's actions in the window.
    pub receipts: Vec<Receipt>,
    /// Spend per day.
    pub spend: Vec<DaySpend>,
    /// Daily cap, USD (0: none).
    pub daily_cap: f64,
    /// Memory.
    pub memory: MemorySummary,
    /// reeved's heartbeat.
    pub observer: Option<ObserverStatus>,
    /// The top of the page.
    pub headlines: Vec<Headline>,
    /// Reeve's version.
    pub version: String,
}

/// Points per chart.
const BUCKETS: usize = 360;

/// Read reeved's metrics for the window.
pub fn read_metrics(home: &Path, since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<MetricRow> {
    let dir = home.join("observer").join("metrics");
    let mut rows = Vec::new();
    let mut day = since.with_timezone(&Local).date_naive() - Duration::days(1);
    let last = now.with_timezone(&Local).date_naive();
    while day <= last {
        if let Ok(text) = fs::read_to_string(dir.join(format!("{day}.jsonl"))) {
            rows.extend(
                text.lines()
                    .filter_map(|l| serde_json::from_str::<MetricRow>(l).ok())
                    .filter(|r| r.ts >= since && r.ts <= now),
            );
        }
        day += Duration::days(1);
    }
    rows.sort_by_key(|r| r.ts);
    rows
}

/// Average minutes into `BUCKETS` points; an empty bucket is a gap.
fn bucket(
    rows: &[MetricRow],
    since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Vec<(DateTime<Utc>, Option<Point>)> {
    let span = (now - since).num_seconds().max(60) as f64;
    let width = span / BUCKETS as f64;
    let mut acc: Vec<Vec<&MetricRow>> = vec![Vec::new(); BUCKETS];
    for r in rows {
        let i = (((r.ts - since).num_seconds() as f64) / width) as usize;
        acc[i.min(BUCKETS - 1)].push(r);
    }
    acc.iter()
        .enumerate()
        .map(|(i, rs)| {
            let t = since + Duration::seconds((width * (i as f64 + 0.5)) as i64);
            if rs.is_empty() {
                return (t, None);
            }
            let n = rs.len() as f64;
            let mean = |f: &dyn Fn(&MetricRow) -> f64| rs.iter().map(|r| f(r)).sum::<f64>() / n;
            let temps: Vec<f64> = rs.iter().filter_map(|r| r.temp.map(f64::from)).collect();
            (
                t,
                Some(Point {
                    cpu: mean(&|r| f64::from(r.cpu)),
                    cpu_max: rs.iter().map(|r| f64::from(r.cpu_max)).fold(0.0, f64::max),
                    mem: mean(&|r| r.mem * 100.0),
                    swap: mean(&|r| r.swap * 100.0),
                    load: mean(&|r| f64::from(r.load1)),
                    temp: (!temps.is_empty())
                        .then(|| temps.iter().copied().fold(f64::MIN, f64::max)),
                    rx: mean(&|r| r.rx),
                    tx: mean(&|r| r.tx),
                }),
            )
        })
        .collect()
}

fn stat(rows: &[MetricRow], f: impl Fn(&MetricRow) -> Option<f64>) -> Option<Stat> {
    let vals: Vec<(DateTime<Utc>, f64)> = rows
        .iter()
        .filter_map(|r| f(r).map(|v| (r.ts, v)))
        .collect();
    if vals.is_empty() {
        return None;
    }
    let (max_at, max) = vals
        .iter()
        .copied()
        .fold((vals[0].0, f64::MIN), |a, b| if b.1 > a.1 { b } else { a });
    Some(Stat {
        min: vals.iter().map(|v| v.1).fold(f64::MAX, f64::min),
        avg: vals.iter().map(|v| v.1).sum::<f64>() / vals.len() as f64,
        max,
        max_at: Some(max_at),
    })
}

/// Least-squares slope, per day, of `(t, y)`.
fn slope_per_day(points: &[(DateTime<Utc>, f64)]) -> f64 {
    if points.len() < 3 {
        return 0.0;
    }
    let t0 = points[0].0;
    let xs: Vec<f64> = points
        .iter()
        .map(|(t, _)| (*t - t0).num_seconds() as f64 / 86_400.0)
        .collect();
    let n = xs.len() as f64;
    let mx = xs.iter().sum::<f64>() / n;
    let my = points.iter().map(|p| p.1).sum::<f64>() / n;
    let (mut num, mut den) = (0.0, 0.0);
    for (x, (_, y)) in xs.iter().zip(points) {
        num += (x - mx) * (y - my);
        den += (x - mx) * (x - mx);
    }
    if den <= f64::EPSILON { 0.0 } else { num / den }
}

fn disk_trends(rows: &[MetricRow], now: &crate::Snapshot) -> Vec<DiskTrend> {
    let mut hourly: BTreeMap<String, BTreeMap<i64, (f64, u32)>> = BTreeMap::new();
    for r in rows {
        for (m, v) in &r.disks {
            let e = hourly
                .entry(m.clone())
                .or_default()
                .entry(r.ts.timestamp() / 3600)
                .or_default();
            e.0 += v;
            e.1 += 1;
        }
    }
    now.disks
        .iter()
        .map(|d| {
            let series: Vec<(DateTime<Utc>, f64)> = hourly
                .get(&d.mount)
                .map(|h| {
                    h.iter()
                        .filter_map(|(hour, (sum, n))| {
                            DateTime::from_timestamp(hour * 3600, 0)
                                .map(|t| (t, sum / f64::from(*n)))
                        })
                        .collect()
                })
                .unwrap_or_default();
            // Less than a day of history says nothing about a trend.
            let long_enough = series
                .first()
                .zip(series.last())
                .is_some_and(|(a, b)| b.0 - a.0 >= Duration::hours(20));
            let per_day = if long_enough {
                slope_per_day(&series)
            } else {
                0.0
            };
            let ratio = d.ratio();
            // Growth under 0.01%/day is noise (logs rotating, caches).
            let days_to_full = (per_day > 0.0001).then(|| (1.0 - ratio) / per_day);
            DiskTrend {
                mount: d.mount.clone(),
                now: ratio,
                total: d.total,
                used: d.used,
                per_day,
                days_to_full,
                series,
            }
        })
        .collect()
}

fn spend(home: &Path, since: DateTime<Utc>, now: DateTime<Utc>) -> Vec<DaySpend> {
    let mut days: BTreeMap<NaiveDate, DaySpend> = BTreeMap::new();
    let mut d = since.with_timezone(&Local).date_naive();
    while d <= now.with_timezone(&Local).date_naive() {
        days.insert(
            d,
            DaySpend {
                day: d,
                ..Default::default()
            },
        );
        d += Duration::days(1);
    }
    let mut months: Vec<(i32, u32)> = days
        .keys()
        .map(|d| (chrono::Datelike::year(d), chrono::Datelike::month(d)))
        .collect();
    months.dedup();
    for (y, m) in months {
        let Ok(text) = fs::read_to_string(reeve_core::ledger::month_path(home, y, m)) else {
            continue;
        };
        for rec in text
            .lines()
            .filter_map(|l| serde_json::from_str::<SpendRecord>(l).ok())
        {
            if rec.ts < since {
                continue;
            }
            let Some(day) = days.get_mut(&rec.ts.with_timezone(&Local).date_naive()) else {
                continue;
            };
            let role = match rec.role.as_deref() {
                None => "chat".to_string(),
                Some(r) if r.starts_with("order:") => "orders".to_string(),
                Some(r) => r.to_string(),
            };
            match rec.usd {
                Some(u) => *day.by_role.entry(role).or_default() += u,
                None => day.unpriced += 1,
            }
        }
    }
    days.into_values().collect()
}

fn memory(home: &Path, since: DateTime<Utc>) -> MemorySummary {
    let notes = Memory::new(home).all();
    let mut s = MemorySummary::default();
    for n in &notes {
        if n.status.in_use() {
            match n.layer {
                Layer::Facts => s.facts += 1,
                Layer::Runbooks => s.runbooks += 1,
                Layer::Preferences => s.preferences += 1,
                Layer::Baselines => {}
            }
        }
        if n.status == reeve_core::memory::NoteStatus::Pending {
            s.pending += 1;
        }
        if n.observed >= since && !n.source.starts_with("survey") && n.layer != Layer::Baselines {
            s.new += 1;
        }
    }
    let mut runbooks: Vec<&reeve_core::memory::Note> = notes
        .iter()
        .filter(|n| n.layer == Layer::Runbooks)
        .collect();
    runbooks.sort_by_key(|n| std::cmp::Reverse(n.successes + n.failures));
    s.top_runbooks = runbooks
        .iter()
        .filter(|n| n.successes + n.failures > 0)
        .take(5)
        .map(|n| (n.title.clone(), n.successes, n.failures))
        .collect();
    s
}

/// Gather the page for the last `days` days.
pub fn gather(home: &Path, days: u32) -> Report {
    let now = Utc::now();
    let days = days.clamp(1, 31);
    let since = now - Duration::days(i64::from(days));
    let cfg = reeve_core::config::load_at(home).unwrap_or_default();
    let host = HostInfo::read();
    let snap = crate::Sampler::new().sample();
    let rows = read_metrics(home, since, now);
    let coverage = (rows.len() as f64 / (now - since).num_minutes().max(1) as f64).min(1.0);
    // Charts start where the readings do, rather than squeezing a day of
    // data against the right edge of a week.
    let chart_since = rows
        .first()
        .map(|r| r.ts - Duration::minutes(5))
        .filter(|t| *t - since > (now - since) / 20)
        .unwrap_or(since);

    let receipts: Vec<Receipt> = ReceiptBook::new(home)
        .all()
        .into_iter()
        .filter(|r| r.ts >= since)
        .collect();
    let mut findings: Vec<Finding> = FindingStore::new(home).list();
    let resolved = findings
        .iter()
        .filter(|f| f.resolved_at.is_some_and(|t| t >= since))
        .count();
    findings.retain(Finding::is_live);
    findings.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then(b.last_seen.cmp(&a.last_seen))
    });

    let distro = reeve_core::distro::Distro::detect();
    let drift = drift::gather(home, &distro, since, &receipts);

    let mut r = Report {
        generated: Local::now(),
        days,
        since,
        uptime_secs: snap.uptime_secs,
        points: bucket(&rows, chart_since, now),
        chart_since,
        coverage,
        cpu: stat(&rows, |r| Some(f64::from(r.cpu))).unwrap_or_default(),
        mem: stat(&rows, |r| Some(r.mem * 100.0)).unwrap_or_default(),
        swap: stat(&rows, |r| Some(r.swap * 100.0)).unwrap_or_default(),
        load: stat(&rows, |r| Some(f64::from(r.load1))).unwrap_or_default(),
        temp: stat(&rows, |r| r.temp.map(f64::from)),
        swap_full_share: if rows.is_empty() {
            0.0
        } else {
            rows.iter().filter(|r| r.swap >= 0.95).count() as f64 / rows.len() as f64
        },
        disks: disk_trends(&rows, &snap),
        findings,
        resolved,
        drift,
        receipts,
        spend: spend(home, since, now),
        daily_cap: cfg.spend.daily_usd,
        memory: memory(home, since),
        observer: ObserverStatus::load(home),
        headlines: Vec::new(),
        host,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };
    r.headlines = headlines(&r, &cfg);
    r
}

fn days_label(d: f64) -> String {
    if d < 1.5 {
        "about a day".into()
    } else if d < 60.0 {
        format!("about {} days", d.round())
    } else {
        format!("about {} months", (d / 30.0).round())
    }
}

/// The few things worth saying first, worst first.
fn headlines(r: &Report, cfg: &reeve_core::config::Config) -> Vec<Headline> {
    let mut h = Vec::new();
    let mut say = |tone, text: String| {
        let text = if text.chars().count() > 120 {
            format!("{}…", text.chars().take(118).collect::<String>().trim_end())
        } else {
            text
        };
        h.push(Headline { tone, text });
    };
    for f in r
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .take(3)
    {
        say(Tone::Bad, f.title.clone());
    }
    for d in &r.disks {
        if d.now >= cfg.observer.disk_warn {
            say(
                Tone::Bad,
                format!("{} is {:.0}% full", d.mount, d.now * 100.0),
            );
        } else if let Some(days) = d.days_to_full.filter(|d| *d < 90.0) {
            let tone = if days < 30.0 { Tone::Warn } else { Tone::Info };
            say(
                tone,
                format!(
                    "{} is filling at {}/day: full in {}",
                    d.mount,
                    bytes(d.per_day * d.total as f64),
                    days_label(days)
                ),
            );
        }
    }
    if r.swap_full_share >= 0.5 {
        say(
            Tone::Warn,
            format!(
                "Swap was nearly full {:.0}% of the time (memory averaged {:.0}%)",
                r.swap_full_share * 100.0,
                r.mem.avg
            ),
        );
    }
    if let Some(t) = r
        .temp
        .filter(|t| t.max >= f64::from(cfg.observer.temp_warn))
    {
        say(
            Tone::Warn,
            format!(
                "The CPU reached {:.0}°C{}",
                t.max,
                t.max_at.map_or(String::new(), when)
            ),
        );
    }
    if r.cpu.avg >= 70.0 {
        say(
            Tone::Warn,
            format!("The CPU averaged {:.0}% busy", r.cpu.avg),
        );
    }
    let warnings = r
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    if warnings > 0 {
        say(
            Tone::Warn,
            format!(
                "{warnings} finding{} to look at",
                if warnings == 1 { "" } else { "s" }
            ),
        );
    }
    if let Some(k) = &r.drift.reboot_for {
        say(
            Tone::Info,
            format!(
                "Kernel {k} is installed; a reboot puts it in use (running {})",
                r.host.kernel
            ),
        );
    }
    let verified = r
        .receipts
        .iter()
        .filter(|x| x.tool == "change_commit" && x.outcome.status == Status::Ok)
        .count();
    let rolled = r
        .receipts
        .iter()
        .filter(|x| x.tool == "change_commit" && x.outcome.status == Status::Error)
        .count();
    if verified > 0 {
        say(
            Tone::Good,
            format!(
                "{verified} fix{} verified by their checks",
                if verified == 1 { "" } else { "es" }
            ),
        );
    }
    if rolled > 0 {
        say(
            Tone::Info,
            if rolled == 1 {
                "1 fix failed its checks and was rolled back".to_string()
            } else {
                format!("{rolled} fixes failed their checks and were rolled back")
            },
        );
    }
    let changed = r.drift.installed.len() + r.drift.upgraded.len() + r.drift.removed.len();
    if changed > 0 {
        say(
            Tone::Info,
            format!("{changed} package changes this {}", window_word(r.days)),
        );
    }
    if r.chart_since > r.since {
        say(
            Tone::Info,
            format!(
                "reeved has been recording since {}; the charts start there",
                r.chart_since
                    .with_timezone(&Local)
                    .format("%a %-d %b, %H:%M")
            ),
        );
    } else if r.coverage > 0.0 && r.coverage < 0.8 {
        say(
            Tone::Info,
            format!(
                "reeved recorded {:.0}% of this {}, so the charts have gaps",
                r.coverage * 100.0,
                window_word(r.days)
            ),
        );
    }
    h.sort_by_key(|x| x.tone);
    if !h.iter().any(|x| x.tone <= Tone::Warn) {
        h.insert(
            0,
            Headline {
                tone: Tone::Good,
                text: "Nothing needs you right now".into(),
            },
        );
    }
    h
}

fn window_word(days: u32) -> &'static str {
    match days {
        1 => "day",
        7 => "week",
        28..=31 => "month",
        _ => "period",
    }
}

fn when(t: DateTime<Utc>) -> String {
    format!(" ({})", t.with_timezone(&Local).format("%a %-d %b, %H:%M"))
}

/// `1.2 GB`.
pub fn bytes(b: f64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = b.abs();
    let mut i = 0;
    while v >= 1000.0 && i < units.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let s = if v >= 100.0 || i == 0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    };
    format!("{}{s} {}", if b < 0.0 { "−" } else { "" }, units[i])
}

/// Receipts that changed something.
pub fn changes(r: &Report) -> impl Iterator<Item = &Receipt> {
    r.receipts
        .iter()
        .filter(|x| x.tier >= Tier::T1 || x.undoes.is_some() || x.tool.starts_with("change_"))
}

/// Write the page into `~/.reeve/reports/` (keeping the last 20) and return its path.
pub fn write(home: &Path, r: &Report) -> std::io::Result<PathBuf> {
    let dir = home.join("reports");
    fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "state-{}.html",
        r.generated.format("%Y-%m-%d-%H%M%S")
    ));
    // Only the owner: the page lists paths, findings, and commands.
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(html::render(r).as_bytes())?;
    }
    let mut old: Vec<PathBuf> = fs::read_dir(&dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("state-") && n.ends_with(".html"))
        })
        .collect();
    old.sort();
    let excess = old.len().saturating_sub(20);
    for p in old.into_iter().take(excess) {
        let _ = fs::remove_file(p);
    }
    Ok(path)
}

/// Render to a string (tests, `--out`).
pub fn render(r: &Report) -> String {
    html::render(r)
}

/// Open in the desktop's browser, without waiting on it.
pub fn open(path: &Path) -> Result<(), String> {
    let mut child = std::process::Command::new("xdg-open")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("couldn't run xdg-open: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests_support {
    use super::*;

    /// An empty report with these findings.
    pub fn report_with(findings: Vec<Finding>) -> Report {
        Report {
            generated: Local::now(),
            days: 7,
            since: Utc::now() - Duration::days(7),
            host: HostInfo::default(),
            uptime_secs: 3600,
            points: Vec::new(),
            chart_since: Utc::now() - Duration::days(7),
            coverage: 0.0,
            cpu: Stat::default(),
            mem: Stat::default(),
            swap: Stat::default(),
            load: Stat::default(),
            temp: None,
            swap_full_share: 0.0,
            disks: Vec::new(),
            findings,
            resolved: 0,
            drift: drift::Drift::default(),
            receipts: Vec::new(),
            spend: Vec::new(),
            daily_cap: 0.0,
            memory: MemorySummary::default(),
            observer: None,
            headlines: Vec::new(),
            version: "test".into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(ts: DateTime<Utc>, cpu: f32, disk: f64) -> MetricRow {
        MetricRow {
            ts,
            cpu,
            cpu_max: cpu + 5.0,
            mem: 0.5,
            swap: 0.99,
            load1: 1.0,
            temp: Some(50.0),
            rx: 1000.0,
            tx: 500.0,
            disks: BTreeMap::from([("/".to_string(), disk)]),
        }
    }

    #[test]
    fn buckets_average_and_leave_gaps() {
        let now = Utc::now();
        let since = now - Duration::days(1);
        let rows = vec![
            row(since + Duration::minutes(1), 10.0, 0.5),
            row(since + Duration::minutes(2), 30.0, 0.5),
        ];
        let pts = bucket(&rows, since, now);
        assert_eq!(pts.len(), BUCKETS);
        assert_eq!(pts[0].1.unwrap().cpu, 20.0);
        assert_eq!(pts[0].1.unwrap().cpu_max, 35.0);
        assert!(pts[100].1.is_none(), "reeved wasn't running");
    }

    #[test]
    fn a_growing_disk_gets_a_forecast() {
        let now = Utc::now();
        let rows: Vec<MetricRow> = (0..72)
            .map(|h| {
                row(
                    now - Duration::hours(72 - h),
                    1.0,
                    0.50 + 0.01 * (h as f64 / 24.0),
                )
            })
            .collect();
        let snap = crate::Snapshot {
            disks: vec![crate::Disk {
                mount: "/".into(),
                fs: "btrfs".into(),
                total: 1_000_000_000_000,
                used: 530_000_000_000,
                avail: 470_000_000_000,
            }],
            ..Default::default()
        };
        let d = &disk_trends(&rows, &snap)[0];
        assert!((d.per_day - 0.01).abs() < 0.001, "{}", d.per_day);
        let days = d.days_to_full.unwrap();
        assert!((days - 47.0).abs() < 2.0, "{days}");
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(bytes(1_234_567_890.0), "1.2 GB");
        assert_eq!(bytes(512.0), "512 B");
        assert_eq!(bytes(-2_500_000.0), "−2.5 MB");
    }
}
