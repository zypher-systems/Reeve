//! `reeved`: the observer loop. Samples every 5 s, runs detectors every
//! minute, learns baselines hourly, checks kernels and updates every 6 h,
//! follows the journal the whole time, and (only if enabled) lets the
//! drafter prepare a proposal. It never changes the machine.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{Local, Utc};
use reeve_core::config;
use reeve_core::findings::{FindingStore, ObserverStatus, Signal};
use reeve_core::memory::Memory;
use serde::Serialize;
use tokio::sync::mpsc;

use crate::baselines::Baselines;
use crate::{HostInfo, Sampler, Snapshot, detect, drafter, failed_units, journal, notify};

/// Five-second samples kept for "sustained" rules (10 minutes).
const WINDOW: usize = 120;

#[derive(Serialize)]
struct MinuteRow<'a> {
    ts: chrono::DateTime<Utc>,
    cpu: f32,
    cpu_max: f32,
    mem: f64,
    swap: f64,
    load1: f32,
    temp: Option<f32>,
    rx: f64,
    tx: f64,
    disks: BTreeMap<&'a str, f64>,
}

/// Recent journal activity per unit.
#[derive(Default)]
struct JournalState {
    /// Per unit: per-minute counts for the last 10 minutes.
    minutes: HashMap<String, VecDeque<u64>>,
    /// Per unit: this hour's count (for baselines).
    hour: BTreeMap<String, u64>,
    /// Per unit: latest example lines.
    examples: HashMap<String, VecDeque<String>>,
    /// Critical messages to raise on the next tick.
    critical: Vec<Signal>,
}

impl JournalState {
    fn add(&mut self, e: &journal::Entry) {
        let m = self
            .minutes
            .entry(e.unit.clone())
            .or_insert_with(|| VecDeque::from([0]));
        if let Some(last) = m.back_mut() {
            *last += 1;
        }
        *self.hour.entry(e.unit.clone()).or_default() += 1;
        let ex = self.examples.entry(e.unit.clone()).or_default();
        ex.push_back(format!(
            "{} {}",
            e.ts.with_timezone(&Local).format("%H:%M:%S"),
            e.message.chars().take(300).collect::<String>()
        ));
        if ex.len() > 6 {
            ex.pop_front();
        }
        if e.priority <= 2 {
            self.critical.push(detect::journal_critical(
                &e.unit,
                &journal::template(&e.message),
                &e.message,
            ));
        }
    }

    /// Close the minute; return (unit, count over the last 10 minutes).
    fn tick(&mut self) -> Vec<(String, u64)> {
        let mut out = Vec::new();
        for (u, m) in &mut self.minutes {
            out.push((u.clone(), m.iter().sum()));
            m.push_back(0);
            while m.len() > 10 {
                m.pop_front();
            }
        }
        self.minutes.retain(|_, m| m.iter().any(|c| *c > 0));
        out
    }
}

/// One instance at a time: a lock file held for the process's life.
fn lock(home: &Path) -> Result<fs::File, String> {
    let dir = home.join("observer");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(dir.join("reeved.lock"))
        .map_err(|e| e.to_string())?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| "reeved is already running".to_string())?;
    Ok(f)
}

struct Observer {
    home: PathBuf,
    store: FindingStore,
    memory: Memory,
    baselines: Baselines,
    sampler: Sampler,
    host: HostInfo,
    os: String,
    cpu: VecDeque<f32>,
    mem: VecDeque<f64>,
    temp: VecDeque<f32>,
    load: VecDeque<f32>,
    minute: Vec<Snapshot>,
    last: Snapshot,
    journal: JournalState,
    status: ObserverStatus,
    pending_notify: Vec<String>,
    last_notify: Option<chrono::DateTime<Utc>>,
}

fn push<T>(q: &mut VecDeque<T>, v: T) {
    if q.len() == WINDOW {
        q.pop_front();
    }
    q.push_back(v);
}

impl Observer {
    fn new(home: PathBuf) -> Self {
        Self {
            store: FindingStore::new(&home),
            memory: Memory::new(&home),
            baselines: Baselines::load(&home),
            sampler: Sampler::new(),
            host: HostInfo::read(),
            os: reeve_core::distro::os_label(),
            cpu: VecDeque::new(),
            mem: VecDeque::new(),
            temp: VecDeque::new(),
            load: VecDeque::new(),
            minute: Vec::new(),
            last: Snapshot::default(),
            journal: JournalState::default(),
            status: ObserverStatus {
                pid: std::process::id(),
                started: Some(Utc::now()),
                ..Default::default()
            },
            pending_notify: Vec::new(),
            last_notify: None,
            home,
        }
    }

    fn sample(&mut self) {
        let s = self.sampler.sample();
        if let Some(c) = s.cpu_pct {
            push(&mut self.cpu, c);
        }
        if s.mem_total > 0 {
            push(&mut self.mem, s.mem_used as f64 / s.mem_total as f64);
        }
        if let Some(t) = s.temp_c {
            push(&mut self.temp, t);
        }
        push(&mut self.load, s.load[0]);
        self.minute.push(s.clone());
        self.last = s;
    }

    /// Close a minute: record it, fold it into baselines, run detectors.
    fn minute_tick(&mut self, cfg: &config::Config) {
        let now = Utc::now();
        let samples = std::mem::take(&mut self.minute);
        if let Some(last) = samples.last() {
            let avg = |f: &dyn Fn(&Snapshot) -> Option<f64>| {
                let v: Vec<f64> = samples.iter().filter_map(f).collect();
                (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
            };
            let cpu = avg(&|s| s.cpu_pct.map(f64::from)).unwrap_or(0.0);
            let mem = avg(&|s| (s.mem_total > 0).then(|| s.mem_used as f64 / s.mem_total as f64))
                .unwrap_or(0.0);
            let swap =
                avg(&|s| (s.swap_total > 0).then(|| s.swap_used as f64 / s.swap_total as f64))
                    .unwrap_or(0.0);
            let load = avg(&|s| Some(f64::from(s.load[0]))).unwrap_or(0.0);
            let temp = avg(&|s| s.temp_c.map(f64::from));
            let row = MinuteRow {
                ts: now,
                cpu: cpu as f32,
                cpu_max: samples.iter().filter_map(|s| s.cpu_pct).fold(0.0, f32::max),
                mem,
                swap,
                load1: load as f32,
                temp: temp.map(|t| t as f32),
                rx: avg(&|s| s.net_rx_bps).unwrap_or(0.0),
                tx: avg(&|s| s.net_tx_bps).unwrap_or(0.0),
                disks: last
                    .disks
                    .iter()
                    .map(|d| (d.mount.as_str(), d.ratio()))
                    .collect(),
            };
            let day = Local::now().format("%Y-%m-%d");
            let path = self
                .home
                .join("observer")
                .join("metrics")
                .join(format!("{day}.jsonl"));
            let _ = reeve_core::ledger::append_jsonl(&path, &row);
            let mut readings = vec![("cpu", cpu), ("mem", mem), ("swap", swap), ("load", load)];
            if let Some(t) = temp {
                readings.push(("temp", t));
            }
            self.baselines.minute(&readings, now);
        }

        let s = self.last.clone();
        let mut signals: Vec<Signal> = Vec::new();
        signals.extend(detect::disks(
            &s.disks,
            &self.baselines,
            cfg.observer.disk_warn,
        ));
        let mem_ratio = if s.mem_total > 0 {
            s.mem_used as f64 / s.mem_total as f64
        } else {
            0.0
        };
        signals.extend(detect::swap(s.swap_total, s.swap_used, mem_ratio));
        let mem: Vec<f64> = self.mem.iter().copied().collect();
        signals.extend(detect::memory(&mem[mem.len().saturating_sub(60)..]));
        let temp: Vec<f32> = self.temp.iter().copied().collect();
        signals.extend(detect::temp(
            &temp[temp.len().saturating_sub(36)..],
            cfg.observer.temp_warn,
        ));
        let load: Vec<f32> = self.load.iter().copied().collect();
        signals.extend(detect::load(&load, self.host.cpus));
        signals.extend(detect::failed_units(&failed_units()));
        for (unit, n) in self.journal.tick() {
            let examples: Vec<String> = self
                .journal
                .examples
                .get(&unit)
                .map(|e| e.iter().cloned().collect())
                .unwrap_or_default();
            signals.extend(detect::journal_spike(
                &unit,
                n,
                self.baselines.journal_rate(&unit),
                &examples,
            ));
        }
        signals.append(&mut self.journal.critical);
        self.record(signals, now);
        let _ = self
            .store
            .resolve_stale("journal-spike", chrono::Duration::hours(2), now);
        let _ = self
            .store
            .resolve_stale("journal-critical", chrono::Duration::hours(24), now);
        self.flush_notifications(cfg, now);
    }

    fn record(&mut self, signals: Vec<Signal>, now: chrono::DateTime<Utc>) {
        let mut seen: HashMap<&str, Vec<String>> = HashMap::new();
        for k in detect::STATE_KINDS {
            seen.insert(k, Vec::new());
        }
        for s in signals {
            let kind = s.id.split(':').next().unwrap_or("").to_string();
            if let Some(v) = seen.get_mut(kind.as_str()) {
                v.push(s.id.clone());
            }
            match self.store.observe(s, now) {
                Ok((f, true)) => self.pending_notify.push(f.id),
                Ok(_) => {}
                Err(e) => self.status.last_error = Some(e.to_string()),
            }
        }
        for (kind, ids) in seen {
            let _ = self.store.resolve_missing(kind, &ids, now);
        }
    }

    /// At most one notification a minute; several findings share one.
    fn flush_notifications(&mut self, cfg: &config::Config, now: chrono::DateTime<Utc>) {
        if self.pending_notify.is_empty() || !cfg.observer.notify {
            self.pending_notify.clear();
            return;
        }
        if self
            .last_notify
            .is_some_and(|t| now - t < chrono::Duration::seconds(60))
        {
            return;
        }
        let ids = std::mem::take(&mut self.pending_notify);
        let findings: Vec<_> = ids.iter().filter_map(|id| self.store.get(id)).collect();
        let refs: Vec<&_> = findings.iter().collect();
        notify::send(&refs);
        self.last_notify = Some(now);
    }

    fn hour_tick(&mut self) {
        let now = Utc::now();
        let disks: Vec<(String, u64)> = self
            .last
            .disks
            .iter()
            .map(|d| (d.mount.clone(), d.used))
            .collect();
        let counts = std::mem::take(&mut self.journal.hour);
        self.baselines.hour(&disks, &counts, now);
        let _ = self.baselines.save(&self.home);
        self.baselines.write_notes(&self.memory, &self.os);
        self.store.prune(30, now);
        prune_metrics(&self.home.join("observer").join("metrics"), 14);
    }

    async fn slow_checks(&mut self) {
        let now = Utc::now();
        let mut signals = Vec::new();
        let newest = run("rpm -q kernel-core --last 2>/dev/null | head -n 1 | awk '{print $1}' | sed 's/^kernel-core-//'").await;
        let running = run("uname -r").await;
        signals.extend(detect::reboot_pending(running.trim(), newest.trim()));
        let adv = run("command -v dnf5 >/dev/null && timeout 120 dnf5 -q advisory list --security --updates 2>/dev/null | tail -n +2").await;
        let lines: Vec<String> = adv
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect();
        signals.extend(detect::security_updates(lines.len(), &lines));
        let ids: Vec<String> = signals.iter().map(|s| s.id.clone()).collect();
        for s in signals {
            if let Ok((f, true)) = self.store.observe(s, now) {
                self.pending_notify.push(f.id);
            }
        }
        for kind in ["reboot-pending", "updates-security"] {
            let _ = self.store.resolve_missing(kind, &ids, now);
        }
    }

    fn beat(&mut self, cfg: &config::Config) {
        self.status.beat = Some(Utc::now());
        self.status.drafter = cfg.observer.drafter.enabled;
        self.status.drafter_usd_today =
            reeve_core::ledger::role_today(&self.home, "drafter", Local::now()).usd;
        self.status.drafts_today = drafter::drafts_today(&self.store);
        let _ = self.status.save(&self.home);
    }
}

async fn run(cmd: &str) -> String {
    tokio::process::Command::new("bash")
        .args(["--noprofile", "--norc", "-c", cmd])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn prune_metrics(dir: &Path, days: i64) {
    let cutoff = (Local::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") && name[..name.len() - 6] < *cutoff {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// Run the observer until the process is stopped. With `once`, sample for
/// a few seconds, run one detection pass, and return (for testing).
pub async fn run_observer(home: PathBuf, once: bool) -> Result<(), String> {
    let _lock = lock(&home)?;
    let mut obs = Observer::new(home.clone());
    obs.status.journal = journal::system_readable().await;
    let (jtx, mut jrx) = mpsc::channel::<journal::Entry>(1024);
    tokio::spawn(async move {
        // journalctl can die (journal rotation, a restart): follow it again.
        loop {
            let _ = journal::follow(jtx.clone()).await;
            if jtx.is_closed() {
                return;
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
    });
    let mut cfg = config::load_at(&home).unwrap_or_default();
    obs.sample();
    if once {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            obs.sample();
        }
        obs.minute_tick(&cfg);
        obs.slow_checks().await;
        obs.flush_notifications(&cfg, Utc::now());
        obs.beat(&cfg);
        return Ok(());
    }
    let mut sample = tokio::time::interval(Duration::from_secs(5));
    let mut minute = tokio::time::interval(Duration::from_secs(60));
    let mut hour = tokio::time::interval(Duration::from_secs(3600));
    let mut slow = tokio::time::interval(Duration::from_secs(6 * 3600));
    let mut beat = tokio::time::interval(Duration::from_secs(10));
    let mut draft = tokio::time::interval(Duration::from_secs(120));
    minute.tick().await;
    hour.tick().await;
    let profile = obs.host.profile();
    loop {
        tokio::select! {
            _ = sample.tick() => obs.sample(),
            _ = minute.tick() => {
                // Settings change while it runs (the drafter toggle, thresholds).
                if let Ok(c) = config::load_at(&home) {
                    cfg = c;
                }
                obs.minute_tick(&cfg);
            }
            _ = hour.tick() => obs.hour_tick(),
            _ = slow.tick() => obs.slow_checks().await,
            _ = beat.tick() => obs.beat(&cfg),
            _ = draft.tick() => {
                if cfg.observer.drafter.enabled {
                    match drafter::pass(&home, &cfg, &obs.store, &profile).await {
                        drafter::Pass::Blocked(why) => obs.status.last_error = Some(why),
                        drafter::Pass::Drafted(_) => obs.status.last_error = None,
                        drafter::Pass::Idle => {}
                    }
                }
            }
            Some(e) = jrx.recv() => obs.journal.add(&e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn journal_state_counts_ten_minutes() {
        let mut j = JournalState::default();
        let e = journal::Entry {
            unit: "x.service".into(),
            priority: 4,
            message: "boom 1".into(),
            ts: Utc::now(),
        };
        for _ in 0..40 {
            j.add(&e);
        }
        let counts = j.tick();
        assert_eq!(counts, vec![("x.service".to_string(), 40)]);
        assert_eq!(j.hour.get("x.service"), Some(&40));
        let crit = journal::Entry { priority: 2, ..e };
        j.add(&crit);
        assert_eq!(j.critical.len(), 1);
    }

    #[tokio::test]
    async fn only_one_observer_runs() {
        let home = tempfile::tempdir().unwrap();
        let _held = lock(home.path()).unwrap();
        assert!(lock(home.path()).unwrap_err().contains("already running"));
    }
}
