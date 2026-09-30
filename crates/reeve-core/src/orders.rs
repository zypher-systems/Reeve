//! Standing orders: work the owner wrote down once, which Reeve does
//! unattended when a finding or a schedule calls for it. The only way
//! Reeve acts without someone at the keyboard.
//!
//! ```toml
//! # ~/.reeve/orders/keep-journal-small.toml
//! name = "Keep the journal small"
//! task = "When /var fills up, vacuum the journal to 1G and report how much was freed."
//! enabled = true
//!
//! [trigger]
//! findings = ["disk-full:/var", "disk-full:/"]   # finding ids; `*` matches anything
//! schedule = "weekly sun 03:00"                    # hourly, every 6h, daily 03:00, weekly sun 03:00
//! min_severity = "warning"
//!
//! [scope]
//! max_tier = "T2"                                  # never T3: the floor always needs the owner
//! tools = ["shell"]                                # tools allowed to change things (reads are always fine)
//! commands = ["sudo journalctl --vacuum-size=*"]   # every change must match one of these…
//! paths = []                                       # …or, for file tools, one of these
//!
//! [budget]
//! per_run_usd = 0.05
//! runs_per_day = 4
//! cooldown_hours = 6
//! ```
//!
//! Anything outside the scope is refused and ends with a proposal for the
//! owner. Root works only through a sudoers drop-in for the exact commands
//! (`reeve orders sudoers <id>`), since nobody is there to type a password.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveTime, TimeZone, Utc, Weekday};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::findings::{Finding, Severity};
use crate::memory::glob;
use crate::policy::{PathCtx, Tier, shell};
use crate::receipts::{Receipt, ReceiptBook};
use crate::undo::{FileChange, Undo, UndoStore, sha256_hex, write_atomic};

/// One order, as written in its file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Order {
    /// File stem (set on load).
    #[serde(skip)]
    pub id: String,
    /// Display name (default: the file name).
    #[serde(default)]
    pub name: String,
    /// What to do, for the model.
    pub task: String,
    /// Runs only when true.
    #[serde(default)]
    pub enabled: bool,
    /// When it runs.
    #[serde(default)]
    pub trigger: Trigger,
    /// What it may change.
    #[serde(default)]
    pub scope: Scope,
    /// What it may spend.
    #[serde(default)]
    pub budget: Budget,
    /// Connection (default: the main one).
    #[serde(default)]
    pub connection: Option<String>,
    /// Model (default: the main one).
    #[serde(default)]
    pub model: Option<String>,
    /// Popups for this order beyond the one for a proposal (a blocked run
    /// always gets one): `never` (default), `after` (every run), or
    /// `before` (every run, and when it starts).
    #[serde(default = "never")]
    pub notify: String,
}

fn never() -> String {
    "never".into()
}

/// What starts a run.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Trigger {
    /// Finding ids to act on; `*` matches anything (`unit-failed:*`).
    pub findings: Vec<String>,
    /// `hourly`, `every 30m`, `every 6h`, `daily 03:00`, `weekly sun 03:00`.
    pub schedule: Option<String>,
    /// The least severe finding that counts.
    pub min_severity: Option<String>,
}

/// The hard allowlist for changes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Scope {
    /// Highest tier it may reach. T3 is never allowed.
    pub max_tier: Tier,
    /// Tools that may change things (empty: any tool, if its command or
    /// paths match).
    pub tools: Vec<String>,
    /// Globs a command must match (shell, packages, services, processes).
    pub commands: Vec<String>,
    /// Globs every changed path must match (file tools). `~` is expanded.
    pub paths: Vec<String>,
}

impl Default for Scope {
    fn default() -> Self {
        Self {
            max_tier: Tier::T1,
            tools: Vec::new(),
            commands: Vec::new(),
            paths: Vec::new(),
        }
    }
}

/// Spending and frequency limits.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Budget {
    /// Cap per run, USD.
    pub per_run_usd: f64,
    /// Most runs per day.
    pub runs_per_day: u32,
    /// Least time between runs.
    pub cooldown_hours: f64,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            per_run_usd: 0.05,
            runs_per_day: 4,
            cooldown_hours: 6.0,
        }
    }
}

/// One change an order's run wants to make.
#[derive(Debug, Clone, PartialEq)]
pub struct Action<'a> {
    /// Tool name.
    pub tool: &'a str,
    /// Tier.
    pub tier: Tier,
    /// The command, for shell and system tools.
    pub command: Option<&'a str>,
    /// Paths it changes (resolved), for file tools.
    pub paths: &'a [String],
}

/// Collapse whitespace and drop the ` -- ` separators the system tools add,
/// so `systemctl restart bluetooth*` matches `sudo systemctl restart -- bluetooth.service`.
fn normalize(cmd: &str) -> String {
    cmd.split_whitespace()
        .filter(|w| *w != "--")
        .collect::<Vec<_>>()
        .join(" ")
}

/// A command glob: `*` matches within one word (never a space), `?` one
/// character. `sudo dnf clean *` allows `sudo dnf clean all`, not
/// `sudo dnf clean all && rm -rf ~`.
pub fn command_glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => {
                go(&p[1..], t) || (t.first().is_some_and(|c| !c.is_whitespace()) && go(p, &t[1..]))
            }
            (Some('?'), Some(c)) if !c.is_whitespace() => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = normalize(pattern).chars().collect();
    let t: Vec<char> = normalize(text).chars().collect();
    go(&p, &t)
}

impl Scope {
    /// Why an action is outside the scope, or `Ok`.
    pub fn allows(&self, a: &Action<'_>, ctx: &PathCtx) -> std::result::Result<(), String> {
        let home = ctx.home.as_path();
        if a.tier == Tier::T3 {
            return Err("it's on the safeguard floor, which always needs the owner".into());
        }
        if a.tier > self.max_tier {
            return Err(format!(
                "it's {}, above this order's limit of {}",
                a.tier.label(),
                self.max_tier.label()
            ));
        }
        if !self.tools.is_empty() && !self.tools.iter().any(|t| t == a.tool) {
            return Err(format!(
                "{} isn't one of this order's tools ({})",
                a.tool,
                self.tools.join(", ")
            ));
        }
        if let Some(cmd) = a.command {
            // Every part of the line must be allowed on its own.
            for (words, redirects) in shell::simple_commands(cmd)? {
                if let Some((t, _)) = redirects
                    .iter()
                    .find(|(t, w)| *w && !shell::harmless_sink(t))
                {
                    return Err(format!(
                        "it writes to {t} with a redirect, which an unattended run may not do"
                    ));
                }
                // A leading `sudo` is optional in a glob: root is already
                // bounded by `max_tier`, and by the sudoers rule it needs.
                let bare = words.strip_prefix("sudo ").unwrap_or(&words);
                let listed = self
                    .commands
                    .iter()
                    .any(|g| command_glob(g, &words) || command_glob(g, bare));
                // A part that only reads (`| tail -n 5`) is always fine;
                // one that writes, even where that needs no yes, isn't.
                let reads = {
                    let asm = shell::assess(ctx, &words);
                    asm.tier == Tier::T0 && asm.deny.is_none() && !asm.quiet_write
                };
                if !(listed || reads) {
                    return Err(if self.commands.is_empty() {
                        "this order allows no commands".into()
                    } else {
                        format!("`{words}` matches none of: {}", self.commands.join(" | "))
                    });
                }
            }
            return Ok(());
        }
        if !a.paths.is_empty() {
            let globs: Vec<String> = self.paths.iter().map(|g| expand(g, home)).collect();
            for p in a.paths {
                let hit = globs.iter().any(|g| {
                    let base = g.trim_end_matches("/**");
                    glob(g, p) || p == base || p.starts_with(&format!("{base}/"))
                });
                if !hit {
                    return Err(if self.paths.is_empty() {
                        "this order allows no file changes".into()
                    } else {
                        format!("{p} is outside: {}", self.paths.join(" | "))
                    });
                }
            }
            return Ok(());
        }
        Err("it changes something this order can't check".into())
    }
}

fn expand(g: &str, home: &Path) -> String {
    match g.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", home.display()),
        None => g.to_string(),
    }
}

fn day_name(d: Weekday) -> &'static str {
    match d {
        Weekday::Mon => "Monday",
        Weekday::Tue => "Tuesday",
        Weekday::Wed => "Wednesday",
        Weekday::Thu => "Thursday",
        Weekday::Fri => "Friday",
        Weekday::Sat => "Saturday",
        Weekday::Sun => "Sunday",
    }
}

/// Limits that let a schedule run every time it comes due: runs a day, and
/// hours between runs (a little under the schedule's own gap). Without a
/// schedule, two runs a day at least 12 hours apart.
pub fn limits_for(schedule: Option<&str>) -> (u32, f64) {
    match schedule.and_then(|s| Schedule::parse(s).ok()) {
        Some(Schedule::Every(m)) => {
            let per_day = (1440 + m - 1) / m;
            let between = (m as f64 * 0.9 / 60.0 * 100.0).floor() / 100.0;
            (u32::try_from(per_day).unwrap_or(u32::MAX), between)
        }
        _ => (2, 12.0),
    }
}

/// A parsed schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Schedule {
    /// Every N minutes.
    Every(i64),
    /// Daily at a time.
    Daily(NaiveTime),
    /// Weekly on a day at a time.
    Weekly(Weekday, NaiveTime),
}

impl Schedule {
    /// In words: `every 30 minutes`, `every day at 03:00`, `every Sunday at 03:00`.
    pub fn words(&self) -> String {
        match *self {
            Self::Every(60) => "every hour".into(),
            Self::Every(m) if m % 60 == 0 => format!("every {} hours", m / 60),
            Self::Every(m) => format!("every {m} minutes"),
            Self::Daily(t) => format!("every day at {}", t.format("%H:%M")),
            Self::Weekly(d, t) => format!("every {} at {}", day_name(d), t.format("%H:%M")),
        }
    }

    /// Minutes between runs, at most.
    pub fn minutes(&self) -> i64 {
        match *self {
            Self::Every(m) => m,
            Self::Daily(_) => 1440,
            Self::Weekly(..) => 7 * 1440,
        }
    }

    /// Parse `hourly`, `every 30m`, `every 6h`, `daily 03:00`, `weekly sun 03:00`.
    pub fn parse(s: &str) -> std::result::Result<Self, String> {
        let words: Vec<String> = s.split_whitespace().map(str::to_ascii_lowercase).collect();
        let time = |t: &str| {
            NaiveTime::parse_from_str(t, "%H:%M")
                .map_err(|_| format!("{t:?} isn't a time like 03:00"))
        };
        match words
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["hourly"] => Ok(Self::Every(60)),
            ["daily"] => Ok(Self::Daily(NaiveTime::from_hms_opt(3, 0, 0).expect("time"))),
            ["every", n] => {
                let (num, unit) = n.split_at(n.len().saturating_sub(1));
                let v: i64 = num
                    .parse()
                    .map_err(|_| format!("{n:?}: use like 30m or 6h"))?;
                match unit {
                    "m" if v >= 5 => Ok(Self::Every(v)),
                    "h" if v >= 1 => Ok(Self::Every(v * 60)),
                    _ => Err(format!("{n:?}: use like 30m (at least 5) or 6h")),
                }
            }
            ["daily", t] => Ok(Self::Daily(time(t)?)),
            ["weekly", d, t] => {
                let day = d
                    .parse::<Weekday>()
                    .map_err(|_| format!("{d:?} isn't a weekday"))?;
                Ok(Self::Weekly(day, time(t)?))
            }
            _ => Err(format!(
                "{s:?}: use hourly, every 30m, every 6h, daily 03:00, or weekly sun 03:00"
            )),
        }
    }

    /// The first run time after `last` (or the latest one due if it never ran).
    pub fn next_after(&self, last: Option<DateTime<Utc>>, now: DateTime<Utc>) -> DateTime<Utc> {
        let local_now = now.with_timezone(&Local);
        match *self {
            Self::Every(min) => last.map_or(now, |l| l + chrono::Duration::minutes(min)),
            Self::Daily(t) => {
                let today = Local
                    .from_local_datetime(&local_now.date_naive().and_time(t))
                    .earliest()
                    .unwrap_or(local_now)
                    .with_timezone(&Utc);
                match last {
                    Some(l) if l >= today => today + chrono::Duration::days(1),
                    // Never ran, or last ran before today's slot: today's slot
                    // (which may already be past: then it's due now).
                    _ => today,
                }
            }
            Self::Weekly(day, t) => {
                let back = (local_now.weekday().num_days_from_monday() + 7
                    - day.num_days_from_monday())
                    % 7;
                let date = local_now.date_naive() - chrono::Duration::days(i64::from(back));
                let slot = Local
                    .from_local_datetime(&date.and_time(t))
                    .earliest()
                    .unwrap_or(local_now)
                    .with_timezone(&Utc);
                let slot = if slot > now {
                    slot - chrono::Duration::days(7)
                } else {
                    slot
                };
                match last {
                    Some(l) if l >= slot => slot + chrono::Duration::days(7),
                    _ => slot,
                }
            }
        }
    }
}

/// One run, for the history.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Run {
    /// When it started.
    pub ts: DateTime<Utc>,
    /// `schedule`, `finding:<id>`, or `manual`.
    pub trigger: String,
    /// `done`, `blocked` (needed something outside its scope), `failed`, `skipped`.
    pub status: String,
    /// What the model reported, or why it didn't run.
    pub summary: String,
    /// Receipts it wrote.
    #[serde(default)]
    pub receipts: Vec<u64>,
    /// What it cost.
    #[serde(default)]
    pub usd: Option<f64>,
    /// Session id.
    #[serde(default)]
    pub session: Option<String>,
}

/// What the daemon remembers about each order.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OrderState {
    /// Last run start.
    pub last_run: Option<DateTime<Utc>>,
    /// Finding episodes already handled: id → first_seen of that episode.
    #[serde(default)]
    pub handled: BTreeMap<String, DateTime<Utc>>,
    /// Newest last, at most 20.
    #[serde(default)]
    pub runs: Vec<Run>,
}

impl OrderState {
    /// Runs started today.
    pub fn runs_today(&self) -> u32 {
        let today = Local::now().date_naive();
        self.runs
            .iter()
            .filter(|r| r.status != "skipped" && r.ts.with_timezone(&Local).date_naive() == today)
            .count() as u32
    }

    /// Record a run.
    pub fn push(&mut self, run: Run) {
        if run.status != "skipped" {
            self.last_run = Some(run.ts);
        }
        self.runs.push(run);
        let n = self.runs.len();
        if n > 20 {
            self.runs.drain(..n - 20);
        }
    }
}

/// The orders directory.
#[derive(Debug, Clone)]
pub struct Orders {
    dir: PathBuf,
    home: PathBuf,
}

/// What an order's file must hold for a change to go through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expect {
    /// Nothing: a new order never replaces another.
    Absent,
    /// Exactly this (sha256): what was there when the change began.
    Sha(String),
}

/// What saving from the form did.
#[derive(Debug, Clone, PartialEq)]
pub enum Saved {
    /// Written, with its receipt.
    Written(Box<Receipt>),
    /// The file already says this.
    Unchanged,
    /// Fields changed in the file too since the edit began, differently:
    /// nothing was written.
    Clash(Vec<&'static str>),
    /// The file was removed since the edit began: nothing was written.
    Gone,
}

impl Orders {
    /// Orders under `reeve_home/orders`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("orders"),
            home: reeve_home.to_path_buf(),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// An order's file.
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.toml"))
    }

    /// Every order, with any that don't parse reported separately.
    pub fn load(&self) -> (Vec<Order>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return (ok, bad);
        };
        let mut paths: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "toml"))
            .collect();
        paths.sort();
        for p in paths {
            let id = p
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default();
            match fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|t| parse(&id, &t))
            {
                Ok(o) => ok.push(o),
                Err(e) => bad.push((id, e)),
            }
        }
        (ok, bad)
    }

    /// One order.
    pub fn get(&self, id: &str) -> Result<Order> {
        let text =
            fs::read_to_string(self.path(id)).map_err(|e| Error::Io(format!("order {id}: {e}")))?;
        parse(id, &text).map_err(Error::Config)
    }

    /// Turn an order on or off, keeping the rest of its file as written.
    /// Receipted, so it can be undone.
    pub fn set_enabled(&self, id: &str, on: bool, session: &str) -> Result<Receipt> {
        let text = fs::read_to_string(self.path(id))?;
        let base = parse(id, &text).map_err(Error::Config)?;
        let new = Order {
            enabled: on,
            ..base.clone()
        };
        let up = update(&text, &base, &new).map_err(Error::Config)?;
        parse(id, &up.text).map_err(Error::Config)?;
        self.commit(
            id,
            Some(up.text.as_bytes()),
            &Expect::Sha(sha256_hex(text.as_bytes())),
            "order_toggle",
            &format!("turned {} “{}”", if on { "on" } else { "off" }, base.name),
            session,
        )
    }

    /// Save an order from the form: a new file for a new order (`base` is
    /// `None`), never over another; or, for an existing one, only what
    /// changed since `base` (the order when the edit began), leaving the
    /// rest of the file as it is now. Stops at fields changed differently
    /// in the file meanwhile, unless `over` says to keep the edit's.
    pub fn save(
        &self,
        id: &str,
        base: Option<&Order>,
        new: &Order,
        over: bool,
        session: &str,
    ) -> Result<Saved> {
        let path = self.path(id);
        let current = match fs::read_to_string(&path) {
            Ok(t) => Some(t),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let (text, expect, tool, summary) = match (base, current) {
            (Some(_), None) if !over => return Ok(Saved::Gone),
            (None, Some(_)) => {
                return Err(Error::Io(format!(
                    "{id}.toml already exists; not writing over it"
                )));
            }
            (_, None) => (
                render(new),
                Expect::Absent,
                "order_new",
                format!("wrote “{}”", new.name),
            ),
            (Some(base), Some(current)) => {
                let up = update(&current, base, new).map_err(Error::Config)?;
                if !up.clashes.is_empty() && !over {
                    return Ok(Saved::Clash(up.clashes));
                }
                if up.text == current {
                    return Ok(Saved::Unchanged);
                }
                (
                    up.text,
                    Expect::Sha(sha256_hex(current.as_bytes())),
                    "order_edit",
                    format!(
                        "changed “{}”: {}",
                        new.name,
                        up.changed
                            .iter()
                            .map(|k| field_label(k))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        };
        parse(id, &text).map_err(Error::Config)?;
        self.commit(id, Some(text.as_bytes()), &expect, tool, &summary, session)
            .map(|r| Saved::Written(Box::new(r)))
    }

    /// Delete an order. Receipted, with a copy, so it can be put back.
    pub fn delete(&self, id: &str, session: &str) -> Result<Receipt> {
        let text = fs::read(self.path(id))?;
        let name = parse(id, &String::from_utf8_lossy(&text)).map_or(id.to_string(), |o| o.name);
        self.commit(
            id,
            None,
            &Expect::Sha(sha256_hex(&text)),
            "order_delete",
            &format!("deleted “{name}”"),
            session,
        )
    }

    /// Write an order's file (or, with `None`, remove it), keeping what
    /// was there in the undo store and receipting both sides, so F4
    /// activity (or `u` in Orders) can put it back. Refuses, changing
    /// nothing, when the file doesn't hold what `expect` says.
    pub fn commit(
        &self,
        id: &str,
        bytes: Option<&[u8]>,
        expect: &Expect,
        tool: &str,
        summary: &str,
        session: &str,
    ) -> Result<Receipt> {
        let path = self.path(id);
        self.check(id, expect)?;
        let pre = UndoStore::new(&self.home).snapshot(&path)?;
        fs::create_dir_all(&self.dir)?;
        match bytes {
            Some(b) => write_atomic(&path, b, Some(0o600))?,
            None => fs::remove_file(&path)?,
        }
        self.receipt(id, pre, tool, summary, session)
    }

    /// Write an order's file (or, with `None`, remove it) when it holds
    /// what `expect` says, keeping what was there in the undo store.
    /// Returns the change, for a receipt someone else writes (a tool's).
    pub fn write_file(
        &self,
        id: &str,
        bytes: Option<&[u8]>,
        expect: &Expect,
    ) -> Result<FileChange> {
        let path = self.path(id);
        self.check(id, expect)?;
        let store = UndoStore::new(&self.home);
        let pre = store.snapshot(&path)?;
        fs::create_dir_all(&self.dir)?;
        match bytes {
            Some(b) => write_atomic(&path, b, Some(0o600))?,
            None => fs::remove_file(&path)?,
        }
        Ok(FileChange {
            path: path.display().to_string(),
            pre,
            post: store.snapshot(&path)?,
            root: false,
        })
    }

    /// A free id for a new order named `name`.
    pub fn free_id(&self, name: &str) -> String {
        let base = slug(name);
        let mut id = base.clone();
        let mut n = 2;
        while self.path(&id).exists() {
            id = format!("{base}-{n}");
            n += 1;
        }
        id
    }

    /// Refuse, changing nothing, unless the file holds what `expect` says.
    fn check(&self, id: &str, expect: &Expect) -> Result<()> {
        let now = UndoStore::current(&self.path(id))?;
        match (expect, &now) {
            (Expect::Absent, Some(_)) => Err(Error::Io(format!(
                "{id}.toml already exists; not writing over it"
            ))),
            (Expect::Sha(want), now) if now.as_deref() != Some(want.as_str()) => {
                Err(Error::Io(format!(
                    "{id}.toml changed while this was being saved; nothing written, open it again"
                )))
            }
            _ => Ok(()),
        }
    }

    /// Receipt a change already made to an order's file (in `$EDITOR`),
    /// with `before` as what an undo puts back.
    pub fn record(
        &self,
        id: &str,
        before: Option<&[u8]>,
        summary: &str,
        session: &str,
    ) -> Result<Receipt> {
        let pre = match before {
            Some(b) => Some(crate::undo::Blob {
                sha256: UndoStore::new(&self.home).put(b)?,
                mode: 0o600,
                link: None,
            }),
            None => None,
        };
        self.receipt(id, pre, "order_edit", summary, session)
    }

    /// The receipt for a change to an order's file: `pre` before, the file
    /// as it is now after.
    fn receipt(
        &self,
        id: &str,
        pre: Option<crate::undo::Blob>,
        tool: &str,
        summary: &str,
        session: &str,
    ) -> Result<Receipt> {
        let path = self.path(id);
        let post = UndoStore::new(&self.home).snapshot(&path)?;
        let shown = path.display().to_string();
        let mut r = Receipt::draft(
            session,
            tool,
            serde_json::json!({ "path": shown, "order": id }),
            Tier::T3,
        );
        r.approved_by = "user".into();
        r.reasons = vec!["a standing order (powers Reeve uses unattended)".into()];
        r.outcome.summary = summary.to_string();
        r.undo = Some(Undo::Files {
            changes: vec![FileChange {
                path: shown,
                pre,
                post,
                root: false,
            }],
        });
        ReceiptBook::new(&self.home)
            .append(r)
            .map_err(|e| Error::Io(format!("saved, but its receipt wasn't written: {e}")))
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join(".state.json")
    }

    /// Every order's state.
    pub fn states(&self) -> BTreeMap<String, OrderState> {
        fs::read_to_string(self.state_path())
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save every order's state.
    pub fn save_states(&self, s: &BTreeMap<String, OrderState>) -> Result<()> {
        fs::create_dir_all(&self.dir)?;
        let json = serde_json::to_string_pretty(s).map_err(|e| Error::Io(e.to_string()))?;
        crate::undo::write_atomic(&self.state_path(), json.as_bytes(), Some(0o600))
    }

    /// Ask the daemon to run an order at its next tick.
    pub fn request_run(&self, id: &str) -> Result<()> {
        let d = self.dir.join(".run-now");
        fs::create_dir_all(&d)?;
        fs::write(d.join(id), b"")?;
        Ok(())
    }

    /// Take the pending run requests.
    pub fn take_requests(&self) -> Vec<String> {
        let d = self.dir.join(".run-now");
        let Ok(rd) = fs::read_dir(&d) else {
            return Vec::new();
        };
        rd.flatten()
            .filter_map(|e| {
                let id = e.file_name().to_string_lossy().into_owned();
                fs::remove_file(e.path()).ok().map(|()| id)
            })
            .collect()
    }

    /// Write the example orders (disabled) if there are no orders yet.
    /// [`Orders::seed_examples`], once: examples the owner deleted stay
    /// deleted.
    pub fn seed_examples_once(&self) -> Result<usize> {
        let mark = self.dir.join(".examples");
        if mark.exists() {
            return Ok(0);
        }
        let n = self.seed_examples()?;
        fs::write(
            &mark,
            "Reeve wrote its example orders once. `reeve orders examples` writes them again.\n",
        )?;
        Ok(n)
    }

    /// Write the examples, all off, if there are no orders yet.
    pub fn seed_examples(&self) -> Result<usize> {
        fs::create_dir_all(&self.dir)?;
        if self.load().0.iter().any(|_| true) || self.load().1.iter().any(|_| true) {
            return Ok(0);
        }
        for (id, text) in EXAMPLES {
            write_atomic(&self.path(id), text.as_bytes(), Some(0o600))?;
        }
        Ok(EXAMPLES.len())
    }
}

/// Parse and check an order.
pub fn parse(id: &str, text: &str) -> std::result::Result<Order, String> {
    let mut o: Order = toml::from_str(text).map_err(|e| e.to_string())?;
    o.id = id.to_string();
    if o.name.trim().is_empty() {
        o.name = id.to_string();
    }
    if o.task.trim().is_empty() {
        return Err("`task` is empty: say what the order should do".into());
    }
    if o.scope.max_tier == Tier::T3 {
        return Err("max_tier can't be T3: the safeguard floor always needs the owner".into());
    }
    if let Some(s) = &o.trigger.schedule {
        Schedule::parse(s)?;
    }
    if o.trigger.schedule.is_none() && o.trigger.findings.is_empty() {
        return Err("no trigger: give [trigger] findings, a schedule, or both".into());
    }
    if !["after", "before", "never"].contains(&o.notify.as_str()) {
        return Err("notify must be after, before, or never".into());
    }
    Ok(o)
}

impl Order {
    /// Whether a finding calls for this order.
    pub fn wants(&self, f: &Finding) -> bool {
        let min = self
            .trigger
            .min_severity
            .as_deref()
            .map_or(Severity::Info, Severity::parse);
        f.is_live() && f.severity >= min && self.trigger.findings.iter().any(|g| glob(g, &f.id))
    }

    /// Whether frequency limits allow a run now.
    pub fn can_run(&self, st: &OrderState, now: DateTime<Utc>) -> std::result::Result<(), String> {
        if st.runs_today() >= self.budget.runs_per_day {
            return Err(format!(
                "already ran {} times today",
                self.budget.runs_per_day
            ));
        }
        if let Some(l) = st.last_run {
            let cool = chrono::Duration::minutes((self.budget.cooldown_hours * 60.0) as i64);
            if now - l < cool {
                return Err(format!(
                    "cooling down until {}",
                    (l + cool).with_timezone(&Local).format("%H:%M")
                ));
            }
        }
        Ok(())
    }

    /// When the limits would make a schedule run less often than it says.
    pub fn pace_warning(&self) -> Option<String> {
        let sch = Schedule::parse(self.trigger.schedule.as_deref()?).ok()?;
        let gap = sch.minutes();
        let per_day = (1440 + gap - 1) / gap;
        let cool = (self.budget.cooldown_hours * 60.0) as i64;
        (cool > gap || i64::from(self.budget.runs_per_day) < per_day).then(|| {
            format!(
                "it's set to run {}, but its limits ({} a day, {} hours apart) make it run less often",
                sch.words(),
                self.budget.runs_per_day,
                number(self.budget.cooldown_hours)
            )
        })
    }

    /// The order in plain words, a sentence a line: when it runs, what it
    /// does, what it may change, what it spends, and when it says so.
    pub fn describe(&self) -> Vec<String> {
        let mut out = Vec::new();
        let when = self
            .trigger
            .schedule
            .as_deref()
            .and_then(|s| Schedule::parse(s).ok())
            .map(|s| s.words());
        let finds = (!self.trigger.findings.is_empty()).then(|| {
            format!(
                "when reeved finds that {}{}",
                finding_words(&self.trigger.findings),
                match self.trigger.min_severity.as_deref() {
                    Some(sev) if sev != "info" => format!(" ({sev} or worse)"),
                    _ => String::new(),
                }
            )
        });
        out.push(match (when, finds) {
            (Some(w), Some(f)) => format!("Runs {w}, and {f}."),
            (Some(w), None) => format!("Runs {w}."),
            (None, Some(f)) => format!("Runs {f}."),
            (None, None) => "Never runs: it has no schedule and watches for nothing.".into(),
        });
        out.push(format!("Does: {}", self.task.trim()));
        if self.scope.max_tier == Tier::T0 {
            out.push("Only looks and reports: it changes nothing.".into());
        } else {
            let what = match self.scope.max_tier {
                Tier::T1 => "your files and user services",
                _ => "the system: packages, services, sudo",
            };
            let mut only: Vec<String> = self
                .scope
                .commands
                .iter()
                .map(|c| format!("running `{c}`"))
                .collect();
            only.extend(self.scope.paths.iter().map(|p| format!("files in `{p}`")));
            out.push(if only.is_empty() {
                format!(
                    "May change up to {} ({what}), but lists no commands or files, so it can't change anything yet.",
                    self.scope.max_tier.label()
                )
            } else {
                format!(
                    "May change up to {} ({what}), only by {}.",
                    self.scope.max_tier.label(),
                    only.join(", ")
                )
            });
        }
        out.push(format!(
            "Spends at most ${:.2} a run and ${:.2} a day ({} runs a day, {} hours apart).",
            self.budget.per_run_usd,
            self.budget.per_run_usd * f64::from(self.budget.runs_per_day),
            self.budget.runs_per_day,
            number(self.budget.cooldown_hours)
        ));
        out.push(match self.notify.as_str() {
            "after" => "Tells you after every run.".into(),
            "before" => "Tells you when it starts and after each run.".into(),
            _ => "Tells you only when it needs you.".into(),
        });
        out.push(if self.enabled {
            "Starts on.".into()
        } else {
            "Starts off.".into()
        });
        if let Some(w) = self.pace_warning() {
            out.push(format!("Note: {w}."));
        }
        if self
            .scope
            .commands
            .iter()
            .any(|c| c.trim_start().starts_with("sudo "))
        {
            out.push(
                "Note: its sudo commands need a sudoers rule, since nobody's there to type a password (s in Orders shows it)."
                    .into(),
            );
        }
        out
    }

    /// Whether the schedule has a run due.
    pub fn schedule_due(&self, st: &OrderState, now: DateTime<Utc>) -> bool {
        self.trigger
            .schedule
            .as_deref()
            .and_then(|s| Schedule::parse(s).ok())
            .is_some_and(|s| s.next_after(st.last_run, now) <= now)
    }

    /// Sudoers lines for this order's root commands, for the owner to
    /// install with `visudo`. Only concrete commands (no `*`) get a line:
    /// a wildcard in a sudoers rule grants more than it looks like.
    pub fn sudoers(
        &self,
        user: &str,
        which: impl Fn(&str) -> Option<String>,
    ) -> (Vec<String>, Vec<String>) {
        let mut lines = Vec::new();
        let mut skipped = Vec::new();
        for c in &self.scope.commands {
            let Some(rest) = c.trim().strip_prefix("sudo ") else {
                continue;
            };
            if rest.contains('*') || rest.contains('?') {
                skipped.push(c.clone());
                continue;
            }
            let mut words = rest.split_whitespace();
            let Some(prog) = words.next() else { continue };
            let path = if prog.starts_with('/') {
                Some(prog.to_string())
            } else {
                which(prog)
            };
            match path {
                Some(p) => {
                    let args: Vec<&str> = words.collect();
                    let args = if args.is_empty() {
                        String::from(" \"\"")
                    } else {
                        format!(" {}", args.join(" "))
                    };
                    lines.push(format!("{user} ALL=(root) NOPASSWD: {p}{args}"));
                }
                None => skipped.push(c.clone()),
            }
        }
        (lines, skipped)
    }
}

/// What reeved can find, as an order's form offers it: the finding id
/// pattern and what it means in words.
pub const FINDING_KINDS: &[(&str, &str)] = &[
    ("disk-full:*", "a disk is nearly full"),
    ("disk-trend:*", "a disk will be full soon"),
    ("swap-full", "swap is full"),
    ("unit-failed:*", "a service failed"),
    ("app-crash:*", "an app keeps crashing"),
    ("updates-security", "security updates are waiting"),
    ("reboot-pending", "a newer kernel waits for a reboot"),
    ("journal-spike:*", "a burst of warnings in the logs"),
    ("journal-critical:*", "a critical message in the logs"),
];

/// The tools that change things, grouped as an order's form offers them.
pub const TOOL_GROUPS: &[(&str, &[&str])] = &[
    ("run commands", &["shell"]),
    (
        "write, edit, and move files",
        &["fs_write", "fs_edit", "fs_move"],
    ),
    ("delete files", &["fs_delete"]),
    (
        "install, remove, and upgrade packages",
        &["pkg_install", "pkg_remove", "pkg_upgrade"],
    ),
    ("start, stop, and restart services", &["svc_control"]),
    ("stop processes", &["proc_signal"]),
];

/// An order as the file its form saves: the same fields `parse` reads,
/// with a comment on each part.
pub fn render(o: &Order) -> String {
    use std::fmt::Write as _;
    let q = |s: &str| toml::Value::String(s.to_string()).to_string();
    let list = |v: &[String]| {
        format!(
            "[{}]",
            v.iter().map(|s| q(s)).collect::<Vec<_>>().join(", ")
        )
    };
    // Whole numbers without the ".0"; serde reads either into an f64.
    let num = |v: f64| {
        if v.fract() == 0.0 && v.abs() < 1e15 {
            format!("{}", v as i64)
        } else {
            format!("{v:?}")
        }
    };
    // `key = value`, with its comment lined up with the others.
    let kv = |out: &mut String, key: &str, value: String, note: &str| {
        let line = format!("{key} = {value}");
        if note.is_empty() {
            let _ = writeln!(out, "{line}");
        } else {
            let pad = 34usize.saturating_sub(line.chars().count()).max(2);
            let _ = writeln!(out, "{line}{}# {note}", " ".repeat(pad));
        }
    };
    let mut out = String::new();
    let _ = writeln!(out, "# {}", o.name.replace('\n', " "));
    let _ = writeln!(
        out,
        "# Made with Reeve's order form (Orders, F7: e edits it there). Editing here works too.\n"
    );
    kv(&mut out, "name", q(&o.name), "");
    let _ = writeln!(out, "task = {}", task_repr(o.task.trim()));
    kv(
        &mut out,
        "enabled",
        o.enabled.to_string(),
        "space in Orders turns it on or off",
    );
    kv(
        &mut out,
        "notify",
        q(&o.notify),
        "never: only when it needs you · after: every run · before: start and end",
    );
    if let Some(c) = &o.connection {
        kv(&mut out, "connection", q(c), "");
    }
    if let Some(m) = &o.model {
        kv(&mut out, "model", q(m), "");
    }
    let _ = writeln!(out, "\n[trigger]");
    if let Some(sch) = &o.trigger.schedule {
        kv(
            &mut out,
            "schedule",
            q(sch),
            "hourly, every 30m, every 6h, daily 03:00, weekly sun 03:00",
        );
    }
    kv(
        &mut out,
        "findings",
        list(&o.trigger.findings),
        "what reeved finds; * matches anything",
    );
    if let Some(sev) = &o.trigger.min_severity {
        kv(
            &mut out,
            "min_severity",
            q(sev),
            "info, warning, or critical",
        );
    }
    let _ = writeln!(out, "\n[scope]");
    kv(
        &mut out,
        "max_tier",
        q(o.scope.max_tier.label()),
        "T0 look only · T1 your files · T2 system; never T3",
    );
    kv(
        &mut out,
        "tools",
        list(&o.scope.tools),
        "empty: any tool, still limited below",
    );
    kv(
        &mut out,
        "commands",
        list(&o.scope.commands),
        "every change it runs must match one; * is one word",
    );
    kv(
        &mut out,
        "paths",
        list(&o.scope.paths),
        "every file it changes must match one; ** is a whole folder",
    );
    let _ = writeln!(out, "\n[budget]");
    kv(
        &mut out,
        "per_run_usd",
        num(o.budget.per_run_usd),
        "dollars",
    );
    kv(
        &mut out,
        "runs_per_day",
        o.budget.runs_per_day.to_string(),
        "",
    );
    kv(
        &mut out,
        "cooldown_hours",
        num(o.budget.cooldown_hours),
        "the least time between runs",
    );
    out
}

/// Finding patterns in words, where a kind covers them: `disk-full:*` is
/// "a disk is nearly full", `disk-full:/var` "a disk is nearly full (/var)".
pub fn finding_words(patterns: &[String]) -> String {
    let mut named: Vec<(&str, Vec<String>)> = Vec::new();
    let mut raw: Vec<String> = Vec::new();
    for pat in patterns {
        if let Some((_, what)) = FINDING_KINDS.iter().find(|(k, _)| k == pat) {
            if !named.iter().any(|(w, _)| w == what) {
                named.push((what, Vec::new()));
            }
            continue;
        }
        let kind = FINDING_KINDS.iter().find(|(k, _)| {
            k.strip_suffix('*')
                .is_some_and(|pre| pat.len() > pre.len() && pat.starts_with(pre))
        });
        match kind {
            Some((k, what)) => {
                let rest = pat[k.len() - 1..].to_string();
                match named.iter_mut().find(|(w, _)| w == what) {
                    Some((_, v)) => v.push(rest),
                    None => named.push((what, vec![rest])),
                }
            }
            None => raw.push(format!("`{pat}`")),
        }
    }
    let mut all: Vec<String> = named
        .into_iter()
        .map(|(what, v)| {
            if v.is_empty() {
                what.to_string()
            } else {
                format!("{what} ({})", v.join(", "))
            }
        })
        .collect();
    all.extend(raw);
    match all.len() {
        0 => String::new(),
        1 => all.remove(0),
        n => format!("{}, or {}", all[..n - 1].join(", "), all[n - 1]),
    }
}

/// A file name from an order's name: `Tidy the journal` is `tidy-the-journal`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_end_matches('-').chars().take(48).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "order".into() } else { out }
}

/// `0.05`, `6`: short, and it reads back as the same number.
fn number(v: f64) -> String {
    let s = format!("{v}");
    s.strip_suffix(".0").map(String::from).unwrap_or(s)
}

/// An order's file rewritten by [`update`].
#[derive(Debug, Clone, PartialEq)]
pub struct Update {
    /// The new file.
    pub text: String,
    /// The fields the edit changed, as their keys.
    pub changed: Vec<&'static str>,
    /// Fields also changed in the file since the edit began, differently.
    pub clashes: Vec<&'static str>,
}

/// Rewrite an order's file so it says `new`, changing only the fields that
/// differ from `base` (the order when the edit began). Comments, layout,
/// and keys Reeve doesn't know stay as written, and so does anything
/// changed in the file since, in fields the edit didn't touch.
pub fn update(text: &str, base: &Order, new: &Order) -> std::result::Result<Update, String> {
    let mut doc: toml_edit::DocumentMut = text
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    let now = parse(&base.id, text)?;
    let (b, n, c) = (fields(base), fields(new), fields(&now));
    let mut up = Update {
        text: String::new(),
        changed: Vec::new(),
        clashes: Vec::new(),
    };
    for (((table, key), bv), ((_, nv), (_, cv))) in b.iter().zip(n.iter().zip(c.iter())) {
        if bv == nv {
            continue;
        }
        up.changed.push(key);
        if cv != bv && cv != nv {
            up.clashes.push(key);
        }
        set(&mut doc, table, key, nv.as_ref())?;
    }
    up.text = doc.to_string();
    Ok(up)
}

/// A field's name as the order form shows it.
pub fn field_label(key: &str) -> &str {
    match key {
        "task" => "what to do",
        "enabled" => "on or off",
        "notify" => "tell me",
        "schedule" => "how often",
        "findings" => "what reeved finds",
        "min_severity" => "at least",
        "max_tier" => "at most",
        "tools" => "tools",
        "paths" => "files",
        "per_run_usd" => "per run",
        "runs_per_day" => "runs a day",
        "cooldown_hours" => "between runs",
        k => k,
    }
}

/// Each field [`update`] writes, by table and key, as a TOML value
/// (`None`: left out).
fn fields(o: &Order) -> Vec<((&'static str, &'static str), Option<toml::Value>)> {
    use toml::Value as V;
    let s = |x: &str| Some(V::String(x.to_string()));
    let opt = |x: &Option<String>| x.as_deref().map(|x| V::String(x.to_string()));
    let list = |v: &[String]| Some(V::Array(v.iter().map(|x| V::String(x.clone())).collect()));
    let num = |x: f64| {
        Some(if x.fract() == 0.0 && x.abs() < 1e15 {
            V::Integer(x as i64)
        } else {
            V::Float(x)
        })
    };
    vec![
        (("", "name"), s(o.name.trim())),
        (("", "task"), s(o.task.trim())),
        (("", "enabled"), Some(V::Boolean(o.enabled))),
        (("", "notify"), s(&o.notify)),
        (("", "connection"), opt(&o.connection)),
        (("", "model"), opt(&o.model)),
        (("trigger", "findings"), list(&o.trigger.findings)),
        (("trigger", "schedule"), opt(&o.trigger.schedule)),
        (("trigger", "min_severity"), opt(&o.trigger.min_severity)),
        (("scope", "max_tier"), s(o.scope.max_tier.label())),
        (("scope", "tools"), list(&o.scope.tools)),
        (("scope", "commands"), list(&o.scope.commands)),
        (("scope", "paths"), list(&o.scope.paths)),
        (("budget", "per_run_usd"), num(o.budget.per_run_usd)),
        (
            ("budget", "runs_per_day"),
            Some(V::Integer(o.budget.runs_per_day.into())),
        ),
        (("budget", "cooldown_hours"), num(o.budget.cooldown_hours)),
    ]
}

/// A task as a `"""` block when it can be one.
fn task_repr(task: &str) -> String {
    if task.contains("\"\"\"") || task.contains('\\') || task.ends_with('"') {
        toml::Value::String(task.to_string()).to_string()
    } else {
        format!("\"\"\"\n{task}\n\"\"\"")
    }
}

/// Set `table.key` to `v` (or remove it), keeping the comment after it.
fn set(
    doc: &mut toml_edit::DocumentMut,
    table: &str,
    key: &str,
    v: Option<&toml::Value>,
) -> std::result::Result<(), String> {
    let tbl: &mut dyn toml_edit::TableLike = if table.is_empty() {
        doc.as_table_mut()
    } else {
        if doc.get(table).is_none() {
            if v.is_none() {
                return Ok(());
            }
            doc.insert(table, toml_edit::table());
        }
        doc.get_mut(table)
            .and_then(|i| i.as_table_like_mut())
            .ok_or_else(|| format!("`{table}` isn't a table"))?
    };
    let Some(v) = v else {
        tbl.remove(key);
        return Ok(());
    };
    let repr = match v {
        toml::Value::String(t) if key == "task" => task_repr(t),
        v => v.to_string(),
    };
    let mut new: toml_edit::Value = repr
        .parse()
        .map_err(|e: toml_edit::TomlError| e.to_string())?;
    new.decor_mut().clear();
    match tbl.get_mut(key) {
        Some(item) => match item.as_value_mut() {
            Some(old) => {
                let decor = old.decor().clone();
                *old = new;
                *old.decor_mut() = decor;
            }
            None => *item = toml_edit::Item::Value(new),
        },
        None => {
            tbl.insert(key, toml_edit::Item::Value(new));
        }
    }
    Ok(())
}

/// Examples written (disabled) the first time the orders panel opens.
pub const EXAMPLES: &[(&str, &str)] = &[
    (
        "reset-crash-units",
        r#"# Clear failed crash-report units once they've done their job.
name = "Reset crash-report units"
task = """
KDE's drkonqi-coredump-processor units show as failed after an app crash. Note which apps
crashed (from the unit names or `coredumpctl list`), then reset the failed units and report
the apps, so the owner can decide whether a crash needs attention.
"""
enabled = false

[trigger]
findings = ["unit-failed:drkonqi-coredump-processor@*"]

[scope]
max_tier = "T2"
tools = ["shell"]
commands = ["sudo systemctl reset-failed drkonqi-coredump-processor@*", "systemctl --user reset-failed drkonqi-coredump-processor@*"]

[budget]
per_run_usd = 0.03
runs_per_day = 2
cooldown_hours = 12
"#,
    ),
    (
        "keep-journal-small",
        r#"# Keep the systemd journal from filling the disk.
name = "Keep the journal small"
task = """
If the journal is using more than 1 GB, vacuum it to 1 GB and report how much space was freed.
If it's already under 1 GB, do nothing and say so.
"""
enabled = false

[trigger]
findings = ["disk-full:/", "disk-full:/var", "disk-trend:/", "disk-trend:/var"]
schedule = "weekly sun 03:00"
min_severity = "warning"

[scope]
max_tier = "T2"
tools = ["shell"]
commands = ["sudo journalctl --vacuum-size=1G"]

[budget]
per_run_usd = 0.03
runs_per_day = 2
cooldown_hours = 24
"#,
    ),
    (
        "tidy-user-cache",
        r#"# Clear old thumbnails and caches in your home when space runs low.
name = "Tidy my caches"
task = """
When the home disk is getting full, delete thumbnail caches older than 30 days under
~/.cache/thumbnails, and report how much was freed. Don't touch anything else.
"""
enabled = false

[trigger]
findings = ["disk-full:/home", "disk-full:/"]
min_severity = "warning"

[scope]
max_tier = "T1"
tools = ["shell", "fs_delete"]
commands = ["find ~/.cache/thumbnails -type f -mtime +30 -delete", "find */.cache/thumbnails -type f -mtime +30 -delete"]
paths = ["~/.cache/thumbnails/**"]

[budget]
per_run_usd = 0.03
runs_per_day = 1
cooldown_hours = 24
"#,
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rendered_order_reads_back_the_same() {
        for (id, text) in EXAMPLES {
            let o = parse(id, text).unwrap();
            let again = parse(id, &render(&o)).unwrap();
            assert_eq!(again, o, "{id}\n{}", render(&o));
        }
        let o = Order {
            id: "quotes".into(),
            name: "Say \"hi\"".into(),
            task: "Print a backslash \\ and \"\"\"three quotes\"\"\".".into(),
            enabled: true,
            trigger: Trigger {
                findings: vec!["unit-failed:*".into()],
                schedule: Some("every 6h".into()),
                min_severity: Some("warning".into()),
            },
            scope: Scope {
                max_tier: Tier::T2,
                tools: vec!["shell".into()],
                commands: vec!["sudo systemctl restart bluetooth*".into()],
                paths: vec![],
            },
            budget: Budget {
                per_run_usd: 0.1,
                runs_per_day: 2,
                cooldown_hours: 12.0,
            },
            connection: None,
            model: Some("x-ai/grok-4.7".into()),
            notify: "after".into(),
        };
        assert_eq!(parse("quotes", &render(&o)).unwrap(), o);
    }

    fn order(text: &str) -> Order {
        parse("t", text).unwrap()
    }

    const JOURNAL: &str = r#"
name = "j"
task = "vacuum"
enabled = true
[trigger]
findings = ["disk-full:/var"]
[scope]
max_tier = "T2"
tools = ["shell", "svc_control"]
commands = ["sudo journalctl --vacuum-size=*", "systemctl restart bluetooth*"]
paths = ["~/.cache/**"]
"#;

    fn act<'a>(
        tool: &'a str,
        tier: Tier,
        command: Option<&'a str>,
        paths: &'a [String],
    ) -> Action<'a> {
        Action {
            tool,
            tier,
            command,
            paths,
        }
    }

    #[test]
    fn the_scope_is_a_hard_allowlist() {
        let o = order(JOURNAL);
        let ctx = PathCtx::for_tests();
        assert!(
            o.scope
                .allows(
                    &act(
                        "shell",
                        Tier::T2,
                        Some("sudo journalctl --vacuum-size=1G"),
                        &[]
                    ),
                    &ctx
                )
                .is_ok()
        );
        assert!(
            o.scope
                .allows(
                    &act("shell", Tier::T2, Some("sudo dnf remove htop"), &[]),
                    &ctx
                )
                .is_err()
        );
        assert!(
            o.scope
                .allows(
                    &act(
                        "svc_control",
                        Tier::T2,
                        Some("sudo systemctl restart -- bluetooth.service"),
                        &[]
                    ),
                    &ctx
                )
                .is_ok()
        );
        assert!(
            o.scope
                .allows(
                    &act("pkg_install", Tier::T2, Some("sudo dnf5 install -y x"), &[]),
                    &ctx
                )
                .is_err(),
            "tool not listed"
        );
        assert!(
            o.scope
                .allows(
                    &act(
                        "shell",
                        Tier::T3,
                        Some("sudo journalctl --vacuum-size=1G"),
                        &[]
                    ),
                    &ctx
                )
                .unwrap_err()
                .contains("floor")
        );
        let inside = vec!["/home/u/.cache/thumbnails/a.png".to_string()];
        let outside = vec!["/home/u/.bashrc".to_string()];
        let mut o2 = o.clone();
        o2.scope.tools.clear();
        assert!(
            o2.scope
                .allows(&act("fs_delete", Tier::T1, None, &inside), &ctx)
                .is_ok()
        );
        assert!(
            o2.scope
                .allows(&act("fs_delete", Tier::T1, None, &outside), &ctx)
                .is_err()
        );
    }

    #[test]
    fn chaining_redirecting_and_substituting_are_caught() {
        let o = order(JOURNAL);
        let ctx = PathCtx::for_tests();
        let check = |c: &str| o.scope.allows(&act("shell", Tier::T2, Some(c), &[]), &ctx);
        assert!(
            check("sudo journalctl --vacuum-size=1G && sudo dnf remove x").is_err(),
            "every part must match"
        );
        assert!(check("sudo journalctl --vacuum-size=1G ; rm -rf /tmp/x").is_err());
        assert!(
            check("sudo journalctl --vacuum-size=$(rm -rf ~)")
                .unwrap_err()
                .contains("$(")
        );
        assert!(
            check("sudo journalctl --vacuum-size=1G > /etc/motd")
                .unwrap_err()
                .contains("redirect")
        );
        assert!(
            check("sudo journalctl --vacuum-size=1G 2>&1 | tail -n 5").is_ok(),
            "a read on the end is fine"
        );
        assert!(check("sudo journalctl --vacuum-size=1G >/dev/null").is_ok());
        assert!(
            !command_glob("sudo dnf clean *", "sudo dnf clean all extra"),
            "* stays within a word"
        );
        assert!(command_glob("sudo dnf clean *", "sudo dnf clean all"));
    }

    #[test]
    fn orders_are_checked_when_loaded() {
        assert!(
            parse("x", "name='x'\ntask=''\n[trigger]\nschedule='daily'")
                .unwrap_err()
                .contains("task")
        );
        assert!(parse("x", "task='t'\n").unwrap_err().contains("no trigger"));
        assert!(parse("x", "task='t'\n[trigger]\nschedule='sometimes'").is_err());
        assert!(
            parse(
                "x",
                "task='t'\n[trigger]\nschedule='daily'\n[scope]\nmax_tier='T3'"
            )
            .unwrap_err()
            .contains("floor")
        );
        for (id, text) in EXAMPLES {
            let o = parse(id, text).unwrap_or_else(|e| panic!("{id}: {e}"));
            assert!(!o.enabled, "{id} ships disabled");
        }
    }

    #[test]
    fn schedules() {
        let now = Local
            .with_ymd_and_hms(2026, 9, 27, 10, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        let daily = Schedule::parse("daily 03:00").unwrap();
        let slot = Local
            .with_ymd_and_hms(2026, 9, 27, 3, 0, 0)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(
            daily.next_after(None, now),
            slot,
            "never ran: today's slot, already due"
        );
        assert_eq!(
            daily.next_after(Some(slot + chrono::Duration::minutes(1)), now),
            slot + chrono::Duration::days(1)
        );
        let every = Schedule::parse("every 6h").unwrap();
        assert_eq!(
            every.next_after(Some(now), now),
            now + chrono::Duration::hours(6)
        );
        let weekly = Schedule::parse("weekly sun 03:00").unwrap(); // 2026-09-27 is a Sunday
        assert_eq!(weekly.next_after(None, now), slot);
        assert!(Schedule::parse("every 1m").is_err());
    }

    #[test]
    fn findings_and_limits() {
        let o = order(JOURNAL);
        let now = Utc::now();
        let f = Finding {
            id: "disk-full:/var".into(),
            severity: Severity::Warning,
            title: "t".into(),
            detail: String::new(),
            evidence: vec![],
            first_seen: now,
            last_seen: now,
            count: 1,
            status: crate::findings::FindingStatus::Open,
            resolved_at: None,
            notified_at: None,
            notified_severity: None,
            proposal: None,
            draft_note: None,
        };
        assert!(o.wants(&f));
        let mut st = OrderState::default();
        assert!(o.can_run(&st, now).is_ok());
        st.push(Run {
            ts: now,
            trigger: "manual".into(),
            status: "done".into(),
            summary: String::new(),
            receipts: vec![],
            usd: None,
            session: None,
        });
        assert!(o.can_run(&st, now).unwrap_err().contains("cooling"));
    }

    #[test]
    fn sudoers_lines_only_for_exact_commands() {
        let mut o = order(JOURNAL);
        o.scope.commands = vec![
            "sudo journalctl --vacuum-size=1G".into(),
            "sudo dnf clean *".into(),
            "ls".into(),
        ];
        let (lines, skipped) = o.sudoers("zypher", |p| Some(format!("/usr/bin/{p}")));
        assert_eq!(
            lines,
            vec!["zypher ALL=(root) NOPASSWD: /usr/bin/journalctl --vacuum-size=1G"]
        );
        assert_eq!(skipped, vec!["sudo dnf clean *"]);
    }

    #[test]
    fn toggling_keeps_the_file() {
        let d = tempfile::tempdir().unwrap();
        let os = Orders::new(d.path());
        os.seed_examples().unwrap();
        let before = fs::read_to_string(os.path("keep-journal-small")).unwrap();
        os.set_enabled("keep-journal-small", true, "t").unwrap();
        let text = fs::read_to_string(os.path("keep-journal-small")).unwrap();
        assert_eq!(text, before.replace("enabled = false", "enabled = true"));
        assert!(os.get("keep-journal-small").unwrap().enabled);
        assert_eq!(os.seed_examples().unwrap(), 0, "never overwrites");
    }

    const HAND: &str = r#"# Mine: keep this.
name = "Tidy"   # the name
task = """
Tidy up.
"""
enabled = false  # not yet
colour = "blue"  # a key Reeve doesn't know

[trigger]
# every night
schedule = "daily 03:00"

[scope]
max_tier = "T1"
paths = ["~/.cache/**"]   # only here
"#;

    #[test]
    fn an_edit_changes_only_what_it_changed() {
        let base = parse("tidy", HAND).unwrap();
        let mut new = base.clone();
        new.trigger.schedule = Some("weekly sun 04:00".into());
        new.scope.paths.push("~/.thumbnails/**".into());
        new.budget.runs_per_day = 1;
        let up = update(HAND, &base, &new).unwrap();
        assert_eq!(up.changed, ["schedule", "paths", "runs_per_day"]);
        assert!(up.clashes.is_empty());
        for kept in [
            "# Mine: keep this.",
            "name = \"Tidy\"   # the name",
            "enabled = false  # not yet",
            "colour = \"blue\"  # a key Reeve doesn't know",
            "# every night",
            "schedule = \"weekly sun 04:00\"",
            "paths = [\"~/.cache/**\", \"~/.thumbnails/**\"]   # only here",
            "runs_per_day = 1",
        ] {
            assert!(up.text.contains(kept), "{kept}\n{}", up.text);
        }
        let back = parse("tidy", &up.text).unwrap();
        assert_eq!(back, new);
        // Nothing changed, nothing rewritten.
        assert_eq!(update(HAND, &base, &base).unwrap().text, HAND);
    }

    #[test]
    fn an_edit_keeps_changes_made_in_the_file_meanwhile() {
        let base = parse("tidy", HAND).unwrap();
        // Meanwhile, by hand: a new task and a new schedule.
        let meanwhile = HAND
            .replace("Tidy up.", "Tidy up, then report.")
            .replace("daily 03:00", "daily 05:00");
        // The form changed the schedule too, and the name.
        let mut new = base.clone();
        new.name = "Tidy caches".into();
        new.trigger.schedule = Some("hourly".into());
        let up = update(&meanwhile, &base, &new).unwrap();
        assert!(up.text.contains("Tidy up, then report."), "kept theirs");
        assert!(up.text.contains("name = \"Tidy caches\""));
        assert_eq!(up.clashes, ["schedule"]);

        let d = tempfile::tempdir().unwrap();
        let os = Orders::new(d.path());
        fs::create_dir_all(os.dir()).unwrap();
        fs::write(os.path("tidy"), &meanwhile).unwrap();
        let saved = os.save("tidy", Some(&base), &new, false, "t").unwrap();
        assert_eq!(saved, Saved::Clash(vec!["schedule"]));
        assert_eq!(
            fs::read_to_string(os.path("tidy")).unwrap(),
            meanwhile,
            "untouched"
        );
        assert!(matches!(
            os.save("tidy", Some(&base), &new, true, "t").unwrap(),
            Saved::Written(_)
        ));
        let o = os.get("tidy").unwrap();
        assert_eq!(o.trigger.schedule.as_deref(), Some("hourly"));
        assert!(o.task.contains("then report"));
    }

    #[test]
    fn saving_never_writes_over_another_order() {
        let d = tempfile::tempdir().unwrap();
        let os = Orders::new(d.path());
        os.seed_examples().unwrap();
        let before = fs::read_to_string(os.path("tidy-user-cache")).unwrap();
        let o = parse("tidy-user-cache", EXAMPLES[2].1).unwrap();
        assert!(os.save("tidy-user-cache", None, &o, true, "t").is_err());
        assert_eq!(
            fs::read_to_string(os.path("tidy-user-cache")).unwrap(),
            before
        );
        // Deleted while the form was open: asks before writing it back.
        fs::remove_file(os.path("tidy-user-cache")).unwrap();
        assert_eq!(
            os.save("tidy-user-cache", Some(&o), &o, false, "t")
                .unwrap(),
            Saved::Gone
        );
        assert!(!os.path("tidy-user-cache").exists());
    }

    #[test]
    fn every_change_to_an_order_can_be_undone() {
        let d = tempfile::tempdir().unwrap();
        let os = Orders::new(d.path());
        os.seed_examples().unwrap();
        let id = "keep-journal-small";
        let original = fs::read_to_string(os.path(id)).unwrap();
        let book = ReceiptBook::new(d.path());
        let store = UndoStore::new(d.path());

        let r = os.set_enabled(id, true, "t").unwrap();
        assert_eq!(r.tool, "order_toggle");
        book.undo(&store, r.seq, "t").unwrap();
        assert_eq!(fs::read_to_string(os.path(id)).unwrap(), original);

        let r = os.delete(id, "t").unwrap();
        assert!(!os.path(id).exists());
        book.undo(&store, r.seq, "t").unwrap();
        assert_eq!(fs::read_to_string(os.path(id)).unwrap(), original);

        let mut new = parse(id, EXAMPLES[1].1).unwrap();
        new.name = "Mine".into();
        new.id = "mine".into();
        let Saved::Written(r) = os.save("mine", None, &new, false, "t").unwrap() else {
            panic!("not written");
        };
        book.undo(&store, r.seq, "t").unwrap();
        assert!(!os.path("mine").exists());
    }

    #[test]
    fn examples_are_written_once() {
        let d = tempfile::tempdir().unwrap();
        let os = Orders::new(d.path());
        assert_eq!(os.seed_examples_once().unwrap(), EXAMPLES.len());
        for (id, _) in EXAMPLES {
            os.delete(id, "t").unwrap();
        }
        assert_eq!(os.seed_examples_once().unwrap(), 0);
        assert!(os.load().0.is_empty());
    }
}
