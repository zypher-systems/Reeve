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
                // A part that only reads (`| tail -n 5`) is always fine.
                let reads = {
                    let asm = shell::assess(ctx, &words);
                    asm.tier == Tier::T0 && asm.deny.is_none()
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
}

impl Orders {
    /// Orders under `reeve_home/orders`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("orders"),
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
    pub fn set_enabled(&self, id: &str, on: bool) -> Result<()> {
        let p = self.path(id);
        let text = fs::read_to_string(&p)?;
        let mut out = String::new();
        let mut done = false;
        for line in text.lines() {
            if !done && line.trim_start().starts_with("enabled") && line.contains('=') {
                out.push_str(&format!("enabled = {on}\n"));
                done = true;
            } else {
                out.push_str(line);
                out.push('\n');
            }
        }
        if !done {
            out = format!("enabled = {on}\n{out}");
        }
        parse(id, &out).map_err(Error::Config)?;
        crate::undo::write_atomic(&p, out.as_bytes(), Some(0o600))
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
    pub fn seed_examples(&self) -> Result<usize> {
        fs::create_dir_all(&self.dir)?;
        if self.load().0.iter().any(|_| true) || self.load().1.iter().any(|_| true) {
            return Ok(0);
        }
        for (id, text) in EXAMPLES {
            crate::undo::write_atomic(&self.path(id), text.as_bytes(), Some(0o600))?;
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
        os.set_enabled("keep-journal-small", true).unwrap();
        let text = fs::read_to_string(os.path("keep-journal-small")).unwrap();
        assert!(text.contains("enabled = true") && text.contains("# Keep the systemd journal"));
        assert!(os.get("keep-journal-small").unwrap().enabled);
        assert_eq!(os.seed_examples().unwrap(), 0, "never overwrites");
    }
}
