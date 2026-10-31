//! `reeved`: the observer loop. Samples every 5 s, runs detectors every
//! minute, learns baselines hourly, checks kernels and updates every 6 h,
//! asks GitHub for a newer Reeve every 12 h, follows the journal the whole
//! time, and (only if enabled) lets the drafter prepare a proposal. It never
//! changes the machine.
//!
//! Nothing waits on slow work. The 6-hourly checks (package queries) and a
//! draft (model calls, minutes) run as tasks beside the loop, each with a
//! deadline, and report back on a channel the loop listens to; sampling,
//! the heartbeat, the journal and standing orders keep their cadence
//! meanwhile. Checks that didn't finish resolve nothing; a draft that
//! didn't finish leaves its note on the finding.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use chrono::{Local, Utc};
use reeve_core::config;
use reeve_core::findings::{Finding, FindingStore, ObserverStatus, Signal};
use reeve_core::memory::Memory;
use serde::Serialize;
use tokio::sync::mpsc;
use tokio::time::Interval;

use crate::baselines::Baselines;
use crate::{HostInfo, Sampler, Snapshot, detect, drafter, failed_units, journal, notify};

/// Five-second samples kept for "sustained" rules (10 minutes).
const WINDOW: usize = 120;

/// The 6-hourly checks have this long. Past it they resolve nothing, and
/// the next round is in 6 hours.
const CHECKS_DEADLINE: Duration = Duration::from_secs(10 * 60);

/// A draft stops itself after `drafter::DEADLINE`, between model calls. This
/// is the backstop for one stuck in a call that never returned: the deadline,
/// a local model's longest allowed stall, and a margin.
const DRAFT_BACKSTOP: Duration = Duration::from_secs(35 * 60);

/// A piece of work run beside the loop.
type Task<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What the 6-hourly checks found, and which kinds of finding they can
/// speak for: a kind whose probe didn't run gets no verdict, so its
/// findings are left as they were.
struct Checks {
    signals: Vec<Signal>,
    verdicts: Vec<&'static str>,
}

/// The kinds the checks maintain, and how to name each.
const CHECK_KINDS: [(&str, &str); 2] = [
    ("reboot-pending", "a pending reboot"),
    ("updates-security", "security updates"),
];

/// Work that ran beside the loop, reporting back.
enum Done {
    /// The 6-hourly checks; `None` when they didn't finish.
    Checks(Option<Checks>),
    /// A draft.
    Draft(drafter::Pass),
}

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
    /// A standing order is running (one at a time).
    order_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// The 6-hourly checks are running, since then.
    checking: Option<chrono::DateTime<Utc>>,
    /// A draft is running, since then.
    drafting: Option<chrono::DateTime<Utc>>,
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
            order_running: Default::default(),
            checking: None,
            drafting: None,
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
        let _ = self
            .store
            .resolve_stale("app-crash", chrono::Duration::hours(24), now);
        let _ = self
            .store
            .resolve_stale("auth-failure", chrono::Duration::hours(24), now);
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
        // Findings are reported, not announced, unless the owner asked for it.
        if self.pending_notify.is_empty() || !cfg.observer.notify || !cfg.observer.notify_findings {
            self.pending_notify.clear();
            return;
        }
        // One popup per interval at most; everything new in between is summed
        // up in it.
        let every = chrono::Duration::minutes(i64::from(cfg.observer.notify_every_minutes.max(1)));
        if self.last_notify.is_some_and(|t| now - t < every) {
            return;
        }
        let ids = std::mem::take(&mut self.pending_notify);
        let findings: Vec<_> = ids.iter().filter_map(|id| self.store.get(id)).collect();
        let refs: Vec<&_> = findings.iter().collect();
        notify::send(&refs);
        self.last_notify = Some(now);
    }

    /// Start at most one standing order that's due: asked for, triggered by
    /// a finding, or scheduled. It runs in the background; readings go on.
    fn orders_tick(&mut self, cfg: &config::Config, profile: &str) {
        use reeve_core::orders::{Orders, Run};
        use std::sync::atomic::Ordering;
        if self.order_running.load(Ordering::SeqCst) {
            return;
        }
        let orders = Orders::new(&self.home);
        let (list, _) = orders.load();
        // Asked for by the owner: first come, first served, one a tick; the
        // rest wait on disk. A request for an order that's gone is dropped.
        // "Run now" means now: it skips the order's frequency limits.
        let mut due = None;
        for id in orders.pending_requests() {
            match list.iter().find(|o| o.id == id) {
                Some(o) => {
                    due = Some((o.clone(), crate::orders::Cause::Manual));
                    break;
                }
                None => {
                    orders.take_request(&id);
                }
            }
        }
        if list.is_empty() {
            return;
        }
        let now = Utc::now();
        let findings = self.store.list();
        // Picked and claimed under the state's lock, so a run finishing or
        // `reeve orders run` meanwhile doesn't drop the claim, nor this the
        // run. A state file that can't be read means forgotten limits:
        // nothing starts until it's fixed.
        let picked = orders.modify_states(|states| {
            for o in &list {
                if due.is_some() {
                    break;
                }
                let st = states.entry(o.id.clone()).or_default();
                if !o.enabled {
                    continue;
                }
                if let Some(f) = findings
                    .iter()
                    .find(|f| o.wants(f) && st.handled.get(&f.id) != Some(&f.first_seen))
                {
                    // Each episode of a finding is handled once, run or not.
                    st.handled.insert(f.id.clone(), f.first_seen);
                    match o.can_run(st, now) {
                        Ok(()) => {
                            due = Some((
                                o.clone(),
                                crate::orders::Cause::Finding(Box::new(f.clone())),
                            ));
                            break;
                        }
                        Err(why) => st.push(Run {
                            ts: now,
                            trigger: format!("finding:{}", f.id),
                            status: "skipped".into(),
                            summary: why,
                            receipts: vec![],
                            usd: None,
                            session: None,
                        }),
                    }
                    continue;
                }
                if o.schedule_due(st, now) && o.can_run(st, now).is_ok() {
                    due = Some((o.clone(), crate::orders::Cause::Schedule));
                    break;
                }
            }
            if let Some((order, cause)) = &due {
                // Claimed before it starts: it holds the cooldown and counts
                // toward today's limit, so a slow run can't start twice and
                // one that never comes back still counted.
                states.entry(order.id.clone()).or_default().claim(now);
                if matches!(cause, crate::orders::Cause::Manual) {
                    orders.take_request(&order.id);
                }
            }
            due
        });
        let due = match picked {
            Ok(d) => d,
            Err(e) => {
                self.status.last_error =
                    Some(format!("orders: {e}; nothing starts until it's fixed"));
                return;
            }
        };
        let Some((order, cause)) = due else { return };
        self.order_running.store(true, Ordering::SeqCst);
        let (home, cfg, profile, running) = (
            self.home.clone(),
            cfg.clone(),
            profile.to_string(),
            self.order_running.clone(),
        );
        tokio::spawn(async move {
            if order.notify == "before" && cfg.observer.notify {
                notify::plain(
                    &format!("Reeve: starting \"{}\"", order.name),
                    &cause.label(),
                    false,
                );
            }
            let run = crate::orders::run(&home, &cfg, &order, cause, &profile).await;
            let orders = Orders::new(&home);
            let summary_line = run
                .summary
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .to_string();
            let status = run.status.clone();
            let receipts = run.receipts.len();
            // A state that stopped parsing while the run was on isn't
            // replaced with one that remembers only this run: the file
            // stays as it is, the tick reports it, and this run is known
            // by its receipts alone.
            if let Err(e) =
                orders.modify_states(|s| s.entry(order.id.clone()).or_default().push(run))
            {
                eprintln!(
                    "reeved: orders: {e}; the run of \"{}\" ({status}) isn't in its history",
                    order.name
                );
            }
            // A blocked run left a proposal: that's worth a popup. A finished
            // or failed one only if the order asks for it.
            let proposal = status == "blocked" || status == "rolled_back";
            if cfg.observer.notify && (proposal || order.notify == "after") {
                let title = match status.as_str() {
                    "done" => format!("Reeve did: {}", order.name),
                    "blocked" | "rolled_back" => format!("Reeve has a proposal: {}", order.name),
                    _ => format!("Reeve couldn't: {}", order.name),
                };
                notify::plain(
                    &title,
                    &format!("{summary_line}\n{receipts} receipts · /orders to review"),
                    status != "done",
                );
            }
            running.store(false, std::sync::atomic::Ordering::SeqCst);
        });
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
        prune_metrics(&self.home.join("observer").join("metrics"), 31);
        // Today's snapshot of packages and units, for the report's drift.
        crate::report::drift::save_daily(&self.home, &reeve_core::distro::Distro::detect());
    }

    /// Start the 6-hourly checks beside the loop, unless they're running.
    fn start_checks<F>(&mut self, tx: &mpsc::Sender<Done>, gather: F)
    where
        F: Future<Output = Option<Checks>> + Send + 'static,
    {
        if self.checking.is_some() {
            return;
        }
        self.checking = Some(Utc::now());
        let tx = tx.clone();
        tokio::spawn(async move {
            let out = tokio::time::timeout(CHECKS_DEADLINE, gather)
                .await
                .ok()
                .flatten();
            let _ = tx.send(Done::Checks(out)).await;
        });
    }

    /// Record what the checks found. A kind whose probe didn't run, or a
    /// round that didn't finish, resolves nothing of its own: a finding the
    /// checks didn't see isn't gone.
    fn finish_checks(&mut self, out: Option<Checks>) {
        self.checking = None;
        let now = Utc::now();
        let Some(c) = out else {
            self.status.last_error = Some(format!(
                "the 6-hourly checks didn't finish in {} minutes, or no probe ran; nothing was resolved",
                CHECKS_DEADLINE.as_secs() / 60
            ));
            return;
        };
        let ids: Vec<String> = c.signals.iter().map(|s| s.id.clone()).collect();
        for s in c.signals {
            if let Ok((f, true)) = self.store.observe(s, now) {
                self.pending_notify.push(f.id);
            }
        }
        for kind in &c.verdicts {
            let _ = self.store.resolve_missing(kind, &ids, now);
        }
        let silent: Vec<&str> = CHECK_KINDS
            .iter()
            .filter(|(kind, _)| !c.verdicts.contains(kind))
            .map(|(_, what)| *what)
            .collect();
        if !silent.is_empty() {
            self.status.last_error = Some(format!(
                "the 6-hourly checks couldn't ask about {}; those findings were left as they were",
                silent.join(" or ")
            ));
        }
    }

    /// Start a draft of `id` beside the loop, unless one is running. The
    /// draft stops itself after `drafter::DEADLINE`; this is the backstop
    /// for one stuck in a call that never returns, and it writes the note
    /// on the finding, so the next pass doesn't pick it again.
    fn start_draft<F>(
        &mut self,
        tx: &mpsc::Sender<Done>,
        id: String,
        episode: chrono::DateTime<Utc>,
        pass: F,
    ) where
        F: Future<Output = drafter::Pass> + Send + 'static,
    {
        if self.drafting.is_some() {
            return;
        }
        self.drafting = Some(Utc::now());
        let (tx, home) = (tx.clone(), self.home.clone());
        tokio::spawn(async move {
            let out = match tokio::time::timeout(DRAFT_BACKSTOP, pass).await {
                Ok(p) => p,
                Err(_) => {
                    let why = format!(
                        "the draft was cut off after {} minutes in a model call that never returned; what that call cost may be missing from the ledger",
                        DRAFT_BACKSTOP.as_secs() / 60
                    );
                    // The note goes on the finding as it is now, and only
                    // while it's still this episode and open.
                    let store = FindingStore::new(&home);
                    if let Some(mut f) = store.get(&id) {
                        if f.first_seen == episode
                            && f.status == reeve_core::findings::FindingStatus::Open
                        {
                            f.draft_note = Some(why.clone());
                            let _ = store.put(&f);
                        }
                    }
                    drafter::Pass::Blocked(why)
                }
            };
            let _ = tx.send(Done::Draft(out)).await;
        });
    }

    /// Take in what a draft did.
    fn finish_draft(&mut self, cfg: &config::Config, p: drafter::Pass) {
        self.drafting = None;
        match p {
            drafter::Pass::Blocked(why) => self.status.last_error = Some(why),
            drafter::Pass::Drafted(id) => {
                self.status.last_error = None;
                if cfg.observer.notify {
                    if let Some(f) = self.store.get(&id) {
                        let first = f
                            .proposal
                            .as_ref()
                            .and_then(|p| {
                                p.text
                                    .lines()
                                    .find(|l| !l.trim().is_empty())
                                    .map(str::to_string)
                            })
                            .unwrap_or_default();
                        notify::plain(
                            &format!("Reeve has a fix ready: {}", f.title),
                            &format!("{first}\nOpen Reeve: /findings, then p to use it."),
                            false,
                        );
                    }
                }
            }
            drafter::Pass::Idle => {}
        }
    }

    fn beat(&mut self, cfg: &config::Config) {
        self.status.beat = Some(Utc::now());
        self.status.drafter = cfg.observer.drafter.enabled;
        self.status.drafter_usd_today =
            reeve_core::ledger::role_today(&self.home, "drafter", Local::now()).usd;
        self.status.drafts_today = drafter::drafts_today(&self.store);
        self.status.drafting_since = self.drafting;
        self.status.checking_since = self.checking;
        let _ = self.status.save(&self.home);
    }
}

/// The 6-hourly checks: the running kernel against the newest installed,
/// and security updates. Each kind gets a verdict only when every probe
/// behind it ran and succeeded; a probe that didn't (its tool missing,
/// failing, or timing out) is no verdict, where an empty answer used to read
/// as "no newer kernel" or "no advisories". `None` when nothing ran.
async fn gather_checks() -> Option<Checks> {
    let mut c = Checks {
        signals: Vec::new(),
        verdicts: Vec::new(),
    };
    let running = run("uname -r").await?;
    let running = running.trim();
    use reeve_core::distro::Distro;
    let advisories = match Distro::detect() {
        Distro::Arch { .. } => {
            // The negative is an answer, so the probe says it and exits 0.
            if let Some(present) =
                run("if [ -d \"/usr/lib/modules/$(uname -r)\" ]; then echo yes; else echo no; fi")
                    .await
            {
                c.signals
                    .extend(detect::kernel_replaced(running, present.trim() == "yes"));
                c.verdicts.push("reboot-pending");
            }
            // arch-audit (when installed) knows the advisories with a fix
            // out; without it there's no verdict on security updates.
            run("command -v arch-audit >/dev/null && timeout 120 arch-audit -u 2>/dev/null").await
        }
        Distro::Debian { .. } => {
            // A newer kernel package (a new ABI is a new name; debug
            // symbols aren't kernels, and the unsigned build is the same
            // ABI), or the running one rebuilt under the same name (its
            // package version no longer matches /proc/version's build),
            // or the flag the kernel's postinst leaves. dpkg-query exits 1
            // for "no such package", which is an answer; anything else isn't.
            let reboot = async {
                let newest = run(
                    "dpkg-query -W -f='${db:Status-Status} ${Package}\\n' 'linux-image-[0-9]*' 2>/dev/null \
                     | awk '$1==\"installed\" && $2 !~ /-dbg(sym)?$/ {sub(/-unsigned$/, \"\", $2); print substr($2, 13)}' | sort -V | tail -n 1",
                )
                .await?;
                let installed = run(
                    "dpkg-query -W -f='${Version}' \"linux-image-$(uname -r)\" 2>/dev/null || [ $? -eq 1 ]",
                )
                .await?;
                let proc = run("cat /proc/version").await?;
                let flagged = run(
                    "if [ -f /var/run/reboot-required ]; then cat /var/run/reboot-required.pkgs 2>/dev/null; echo; fi",
                )
                .await?;
                let mut s = Vec::new();
                s.extend(detect::reboot_pending(running, newest.trim()));
                s.extend(detect::kernel_rebuilt(running, installed.trim(), &proc));
                s.extend(detect::reboot_required(&flagged));
                Some(s)
            }
            .await;
            if let Some(s) = reboot {
                c.signals.extend(s);
                c.verdicts.push("reboot-pending");
            }
            // Updates from a security suite, as of the last apt-get
            // update: the archive in the parentheses says, not the name.
            run(
                "timeout 120 apt-get -s -q upgrade --with-new-pkgs 2>/dev/null \
                 | awk '/^Inst / {s=$0; sub(/^[^(]*\\(/, \"\", s); if (s ~ /[Ss]ecurity/) print $2}'",
            )
            .await
        }
        Distro::Fedora { .. } | Distro::Other(_) => {
            // awk reads all of rpm's output, so rpm is never cut off, and
            // the pipeline's status is rpm's when rpm fails.
            if let Some(newest) = run(
                "rpm -q kernel-core --last 2>/dev/null | awk 'NR==1{print $1}' | sed 's/^kernel-core-//'",
            )
            .await
            {
                c.signals
                    .extend(detect::reboot_pending(running, newest.trim()));
                c.verdicts.push("reboot-pending");
            }
            run("command -v dnf5 >/dev/null && timeout 120 dnf5 -q advisory list --security --updates 2>/dev/null | tail -n +2").await
        }
    };
    if let Some(adv) = advisories {
        let lines: Vec<String> = adv
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(String::from)
            .collect();
        c.signals
            .extend(detect::security_updates(lines.len(), &lines));
        c.verdicts.push("updates-security");
    }
    (!c.verdicts.is_empty()).then_some(c)
}

/// Run a probe in bash with `pipefail`: its output only when it exited 0,
/// so a tool that is missing, fails, or is killed by `timeout` is `None`,
/// and so is a pipeline whose producer failed. The child dies with the
/// future, so a dropped (timed out) round leaves no process behind.
async fn run(cmd: &str) -> Option<String> {
    let o = tokio::process::Command::new("bash")
        .args(["--noprofile", "--norc", "-o", "pipefail", "-c", cmd])
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).into_owned())
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
    let (jtx, jrx) = mpsc::channel::<journal::Entry>(1024);
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
    // With no config that loads there are no trusted limits: the observer
    // watches on defaults, and nothing starts until it loads.
    let (cfg, cfg_error) = match config::load_at(&home) {
        Ok(c) => (c, None),
        Err(e) => (config::Config::default(), Some(e.to_string())),
    };
    obs.sample();
    if once {
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_secs(1)).await;
            obs.sample();
        }
        obs.minute_tick(&cfg);
        obs.finish_checks(gather_checks().await);
        obs.flush_notifications(&cfg, Utc::now());
        obs.beat(&cfg);
        return Ok(());
    }
    // reeved runs the installed binary (an update restarts it), so this is
    // what's on disk.
    if reeve_core::update::UpdateState::load(&home)
        .installed
        .as_deref()
        != Some(reeve_core::update::CURRENT)
    {
        let _ = reeve_core::update::UpdateState::modify(&home, |s| {
            s.installed = Some(reeve_core::update::CURRENT.into());
        });
    }
    let profile = obs.host.profile();
    let mut lp = Loop::new(obs, home, cfg, cfg_error, profile, jrx);
    loop {
        lp.step().await;
    }
}

/// What a draft runs. A test puts a stand-in here.
type DraftFn = fn(PathBuf, config::Config, String, Finding) -> Task<drafter::Pass>;

fn draft_task(
    home: PathBuf,
    cfg: config::Config,
    profile: String,
    f: Finding,
) -> Task<drafter::Pass> {
    Box::pin(async move {
        let store = FindingStore::new(&home);
        drafter::draft(&home, &cfg, &store, &profile, f).await
    })
}

/// The loop: the observer, its clocks, and the channels work comes back
/// on. One `step` handles one event, whichever is first.
struct Loop {
    obs: Observer,
    home: PathBuf,
    cfg: config::Config,
    cfg_error: Option<String>,
    profile: String,
    journal: mpsc::Receiver<journal::Entry>,
    done_tx: mpsc::Sender<Done>,
    done_rx: mpsc::Receiver<Done>,
    sample: Interval,
    minute: Interval,
    hour: Interval,
    slow: Interval,
    beat: Interval,
    draft: Interval,
    release: Interval,
    draft_with: DraftFn,
}

impl Loop {
    fn new(
        obs: Observer,
        home: PathBuf,
        cfg: config::Config,
        cfg_error: Option<String>,
        profile: String,
        journal: mpsc::Receiver<journal::Entry>,
    ) -> Self {
        let now = tokio::time::Instant::now();
        let every = |secs: u64| tokio::time::interval(Duration::from_secs(secs));
        // The first minute and hour close a minute and an hour from now,
        // not at once; the rest start right away.
        let after = |secs: u64| {
            tokio::time::interval_at(now + Duration::from_secs(secs), Duration::from_secs(secs))
        };
        let (done_tx, done_rx) = mpsc::channel(8);
        Self {
            obs,
            home,
            cfg,
            cfg_error,
            profile,
            journal,
            done_tx,
            done_rx,
            sample: every(5),
            minute: after(60),
            hour: after(3600),
            slow: every(6 * 3600),
            beat: every(10),
            draft: every(120),
            // A minute in, so a login doesn't wait on the network, then
            // every 12 h.
            release: tokio::time::interval_at(
                now + Duration::from_secs(60),
                reeve_core::update::EVERY,
            ),
            draft_with: draft_task,
        }
    }

    async fn step(&mut self) {
        tokio::select! {
            _ = self.sample.tick() => self.obs.sample(),
            _ = self.minute.tick() => {
                // Settings change while it runs (the drafter toggle, thresholds).
                match config::load_at(&self.home) {
                    Ok(c) => {
                        self.cfg = c;
                        self.cfg_error = None;
                    }
                    Err(e) => self.cfg_error = Some(e.to_string()),
                }
                self.obs.minute_tick(&self.cfg);
            }
            _ = self.hour.tick() => self.obs.hour_tick(),
            _ = self.slow.tick() => self.obs.start_checks(&self.done_tx, gather_checks()),
            _ = self.release.tick() => {
                if self.cfg.updates.check {
                    let h = self.home.clone();
                    tokio::spawn(async move {
                        reeve_core::update::check(&h).await;
                    });
                }
            }
            _ = self.beat.tick() => {
                self.obs.beat(&self.cfg);
                // Every 10 s, so "run now" in /orders doesn't wait a minute.
                match &self.cfg_error {
                    Some(e) => {
                        self.obs.status.last_error = Some(format!(
                            "config.toml: {e}. Standing orders and the drafter are paused until it loads."
                        ));
                    }
                    None => self.obs.orders_tick(&self.cfg, &self.profile),
                }
            }
            _ = self.draft.tick() => self.draft_tick(),
            Some(d) = self.done_rx.recv() => match d {
                Done::Checks(c) => self.obs.finish_checks(c),
                Done::Draft(p) => self.obs.finish_draft(&self.cfg, p),
            },
            Some(e) = self.journal.recv() => self.obs.journal.add(&e),
        }
    }

    /// Start a draft if the drafter is on, the config loads, none is
    /// running, and a finding has settled.
    fn draft_tick(&mut self) {
        if self.cfg_error.is_some() || self.obs.drafting.is_some() {
            return;
        }
        let Some(f) = drafter::next(&self.obs.store, &self.cfg) else {
            return;
        };
        let (id, episode) = (f.id.clone(), f.first_seen);
        let pass = (self.draft_with)(self.home.clone(), self.cfg.clone(), self.profile.clone(), f);
        self.obs.start_draft(&self.done_tx, id, episode, pass);
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

    fn sig(id: &str) -> Signal {
        Signal {
            id: id.into(),
            severity: reeve_core::findings::Severity::Warning,
            title: id.into(),
            detail: "d".into(),
            evidence: vec![],
            count: 1,
        }
    }

    static STARTS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// A draft whose model never answers.
    fn stalled(_: PathBuf, _: config::Config, _: String, _: Finding) -> Task<drafter::Pass> {
        STARTS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(std::future::pending())
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_draft_does_not_hold_the_loop() {
        use reeve_core::findings::FindingStatus;
        let home = tempfile::tempdir().unwrap();
        let (_jtx, jrx) = mpsc::channel(1);
        let mut cfg = config::Config::default();
        cfg.observer.drafter.enabled = true;
        cfg.observer.notify = false;
        let mut lp = Loop::new(
            Observer::new(home.path().to_path_buf()),
            home.path().to_path_buf(),
            cfg,
            None,
            String::new(),
            jrx,
        );
        lp.draft_with = stalled;
        // Only samples and heartbeats tick here: the rest would run the
        // detectors and the probes on the test machine.
        let far = Duration::from_secs(24 * 3600);
        for i in [
            &mut lp.minute,
            &mut lp.hour,
            &mut lp.slow,
            &mut lp.release,
            &mut lp.draft,
        ] {
            i.reset_after(far);
        }
        // A finding that has settled, so the drafter wants it.
        lp.obs
            .store
            .observe(sig("swap-full"), Utc::now() - chrono::Duration::minutes(10))
            .unwrap();
        lp.draft_tick();
        assert!(lp.obs.drafting.is_some(), "a draft started");
        lp.draft_tick();
        assert_eq!(
            STARTS.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "one at a time"
        );

        // A minute of the loop with the draft stuck: sampling kept its
        // 5-second cadence, and the heartbeat says what's running.
        let before = lp.obs.minute.len();
        let _ = tokio::time::timeout(Duration::from_secs(61), async {
            loop {
                lp.step().await;
            }
        })
        .await;
        let sampled = lp.obs.minute.len() - before;
        assert!(sampled >= 12, "sampled {sampled} times in a minute");
        assert!(lp.obs.drafting.is_some());
        let status = ObserverStatus::load(home.path()).unwrap();
        assert!(status.drafting_since.is_some());
        assert!(status.beat.is_some());

        // The backstop: the loop hears of it, the finding keeps the note so
        // the next pass doesn't pick it again, and a new draft may start.
        let _ = tokio::time::timeout(DRAFT_BACKSTOP + Duration::from_secs(5), async {
            while lp.obs.drafting.is_some() {
                lp.step().await;
            }
        })
        .await;
        assert!(lp.obs.drafting.is_none(), "the draft was cut off");
        let note = lp.obs.status.last_error.clone().unwrap_or_default();
        assert!(note.contains("cut off"), "{note}");
        let f = lp.obs.store.get("swap-full").unwrap();
        assert_eq!(f.status, FindingStatus::Open);
        assert_eq!(f.draft_note.as_deref(), Some(note.as_str()));
        assert!(
            drafter::next(&lp.obs.store, &lp.cfg).is_none(),
            "not picked again"
        );
        assert_eq!(STARTS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn checks_that_do_not_finish_resolve_nothing() {
        use reeve_core::findings::FindingStatus;
        let home = tempfile::tempdir().unwrap();
        let mut obs = Observer::new(home.path().to_path_buf());
        // Found by the last round of checks.
        obs.store
            .observe(sig("reboot-pending"), Utc::now())
            .unwrap();
        let (tx, mut rx) = mpsc::channel(8);
        obs.start_checks(&tx, std::future::pending());
        assert!(obs.checking.is_some());
        // Ignored while one runs.
        obs.start_checks(&tx, async {
            Some(Checks {
                signals: vec![],
                verdicts: vec![],
            })
        });
        let Some(Done::Checks(out)) = rx.recv().await else {
            panic!("the checks report back")
        };
        assert!(out.is_none(), "a stuck probe is no verdict");
        obs.finish_checks(out);
        assert!(obs.checking.is_none());
        assert!(
            obs.status
                .last_error
                .as_deref()
                .is_some_and(|e| e.contains("didn't finish")),
            "{:?}",
            obs.status.last_error
        );
        assert_eq!(
            obs.store.get("reboot-pending").unwrap().status,
            FindingStatus::Open,
            "a finding the checks didn't see isn't gone"
        );
        // Checks that finish and don't see it resolve it.
        obs.start_checks(&tx, async {
            Some(Checks {
                signals: vec![],
                verdicts: vec!["reboot-pending", "updates-security"],
            })
        });
        let Some(Done::Checks(out)) = rx.recv().await else {
            panic!("the checks report back")
        };
        obs.finish_checks(out);
        assert_eq!(
            obs.store.get("reboot-pending").unwrap().status,
            FindingStatus::Resolved
        );
    }

    #[test]
    fn a_probe_that_did_not_run_leaves_its_findings_alone() {
        use reeve_core::findings::FindingStatus;
        let home = tempfile::tempdir().unwrap();
        let mut obs = Observer::new(home.path().to_path_buf());
        obs.store
            .observe(sig("reboot-pending"), Utc::now())
            .unwrap();
        obs.store
            .observe(sig("updates-security"), Utc::now())
            .unwrap();
        // Only the advisory probe ran this round.
        obs.finish_checks(Some(Checks {
            signals: vec![],
            verdicts: vec!["updates-security"],
        }));
        assert_eq!(
            obs.store.get("updates-security").unwrap().status,
            FindingStatus::Resolved
        );
        assert_eq!(
            obs.store.get("reboot-pending").unwrap().status,
            FindingStatus::Open,
            "no probe spoke for it"
        );
        let note = obs.status.last_error.clone().unwrap_or_default();
        assert!(note.contains("a pending reboot"), "{note}");
    }

    #[tokio::test]
    async fn a_probe_is_an_answer_only_when_it_succeeds() {
        assert_eq!(run("printf hi").await.as_deref(), Some("hi"));
        // The negative of a test is said, not exited.
        assert_eq!(
            run("if [ -f /nonexistent/flag ]; then echo yes; else echo no; fi")
                .await
                .as_deref(),
            Some("no\n")
        );
        assert_eq!(run("exit 42").await, None);
        // A producer that failed into a consumer that didn't.
        assert_eq!(run("false | cat").await, None);
        assert_eq!(run("nonexistent-tool-xyz 2>/dev/null | cat").await, None);
    }
}
