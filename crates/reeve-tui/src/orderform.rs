//! The standing order form. Every part of an order in plain words, each
//! with a line on what it means, and beside it what the order will do,
//! written out as a sentence. Orders (F7): `n` opens it (empty, or from an
//! example), `e` edits the selected order, and `ctrl+s` checks and saves it
//! to `~/.reeve/orders/`.

use chrono::NaiveTime;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub use reeve_core::orders::slug;
use reeve_core::orders::{
    Budget, EXAMPLES, FINDING_KINDS, Order, Schedule, Scope, TOOL_GROUPS, Trigger, finding_words,
    limits_for,
};
use reeve_core::policy::Tier;

use crate::draw::{pad, plain_wrap, truncate};
use crate::overlay::Action;
use crate::theme::Theme;
use crate::view::View;

/// An editable line or block of text, with a cursor.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Text {
    /// The text.
    pub s: String,
    /// Cursor, a byte offset.
    pub cur: usize,
}

impl Text {
    fn new(s: impl Into<String>) -> Self {
        let s = s.into();
        let cur = s.len();
        Self { s, cur }
    }

    fn insert(&mut self, c: char) {
        self.s.insert(self.cur, c);
        self.cur += c.len_utf8();
    }

    fn backspace(&mut self) {
        if let Some((i, _)) = self.s[..self.cur].char_indices().next_back() {
            self.s.replace_range(i..self.cur, "");
            self.cur = i;
        }
    }

    fn delete(&mut self) {
        if let Some(c) = self.s[self.cur..].chars().next() {
            self.s.replace_range(self.cur..self.cur + c.len_utf8(), "");
        }
    }

    fn left(&mut self) {
        if let Some((i, _)) = self.s[..self.cur].char_indices().next_back() {
            self.cur = i;
        }
    }

    fn right(&mut self) {
        if let Some(c) = self.s[self.cur..].chars().next() {
            self.cur += c.len_utf8();
        }
    }

    /// Non-empty lines, trimmed.
    fn lines(&self) -> Vec<String> {
        self.s
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect()
    }
}

/// One part of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Begin from an example (new orders only).
    Start,
    /// Its name.
    Name,
    /// What to do.
    Task,
    /// Schedule, findings, or both.
    When,
    /// How often.
    Repeat,
    /// Every how many hours or minutes.
    Every,
    /// At what time.
    Time,
    /// On which day.
    Day,
    /// A finding kind to act on.
    Kind(usize),
    /// Other finding patterns.
    Custom,
    /// The least severe finding that counts.
    Severity,
    /// The highest tier.
    Tier,
    /// A group of tools it may use.
    Tool(usize),
    /// Commands it may run.
    Commands,
    /// Files it may change.
    Paths,
    /// Most per run.
    PerRun,
    /// Most runs a day.
    PerDay,
    /// Least hours between runs.
    Between,
    /// When to tell you.
    Notify,
    /// On or off.
    Enabled,
}

enum Kind {
    Line,
    Block,
    Choice,
    Check,
}

impl Field {
    fn kind(self) -> Kind {
        match self {
            Field::Task | Field::Custom | Field::Commands | Field::Paths => Kind::Block,
            Field::Name
            | Field::Every
            | Field::Time
            | Field::PerRun
            | Field::PerDay
            | Field::Between => Kind::Line,
            Field::Kind(_) | Field::Tool(_) | Field::Enabled => Kind::Check,
            _ => Kind::Choice,
        }
    }

    /// Which numbered section it's in.
    fn section(self) -> usize {
        match self {
            Field::Start | Field::Name | Field::Task => 0,
            Field::When
            | Field::Repeat
            | Field::Every
            | Field::Time
            | Field::Day
            | Field::Kind(_)
            | Field::Custom
            | Field::Severity => 1,
            Field::Tier | Field::Tool(_) | Field::Commands | Field::Paths => 2,
            Field::PerRun | Field::PerDay | Field::Between => 3,
            Field::Notify | Field::Enabled => 4,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Field::Start => "start from",
            Field::Name => "name",
            Field::Task => "what to do",
            Field::When => "it runs",
            Field::Repeat => "how often",
            Field::Every => "every",
            Field::Time => "at",
            Field::Day => "on",
            Field::Kind(0) => "when reeved finds",
            Field::Custom => "other findings",
            Field::Severity => "at least",
            Field::Tier => "at most",
            Field::Tool(0) => "using",
            Field::Commands => "commands",
            Field::Paths => "files",
            Field::PerRun => "per run",
            Field::PerDay => "runs a day",
            Field::Between => "between runs",
            Field::Notify => "tell me",
            Field::Enabled => "turn it on",
            Field::Kind(_) | Field::Tool(_) => "",
        }
    }

    /// One line on what it means. Checkbox groups explain once, on their last row.
    fn help(self, repeat: usize) -> &'static str {
        match self {
            Field::Start => "Begin blank, or from an example you can change.",
            Field::Name => "A short name you'll recognize in the list.",
            Field::Task => {
                "In your words: what to check first, what to change, what to report, and when to do nothing. Reeve reads this every time it runs."
            }
            Field::When => {
                "On a schedule, when reeved (Reeve's observer) finds a problem, or both. reeved must be running."
            }
            Field::Repeat => "Missed runs (the machine was off) happen when reeved is next up.",
            Field::Every if repeat == 2 => "How many hours between runs.",
            Field::Every => "How many minutes between runs (at least 5).",
            Field::Time => "24-hour clock, like 03:00.",
            Field::Day => "",
            Field::Kind(i) if i + 1 == FINDING_KINDS.len() => {
                "Tick what should start a run. The findings screen (F3) shows what reeved has found."
            }
            Field::Custom => {
                "Findings by id, one per line. * matches anything: unit-failed:bluetooth*, disk-full:/home"
            }
            Field::Severity => "Findings less serious than this don't start a run.",
            Field::Tier => {
                "The most it may do on its own. Anything more is refused and waits for you as a proposal. The floor (T3) always needs you."
            }
            Field::Tool(i) if i + 1 == TOOL_GROUPS.len() => {
                "How it may change things. Reading is always allowed. None ticked: any, still limited by the lists below."
            }
            Field::Commands => {
                "Commands it may run to change things, one per line. * is one word: sudo journalctl --vacuum-size=*. sudo needs a sudoers rule (s in Orders shows it)."
            }
            Field::Paths => {
                "Files it may write, edit, move, or delete, one per line. ** covers a folder: ~/.cache/thumbnails/**"
            }
            Field::PerRun => "The most one run may spend on the model, in dollars.",
            Field::PerDay => "The most times it runs in one day.",
            Field::Between => "The least time between two runs, in hours.",
            Field::Notify => "A proposal (it needed something it may not do) always pops up.",
            Field::Enabled => {
                "Leave off to read it through first: space in Orders turns it on later."
            }
            Field::Kind(_) | Field::Tool(_) => "",
        }
    }
}

/// `it runs`.
pub const WHEN: [&str; 3] = ["on a schedule", "when reeved finds something", "both"];
/// `how often`.
pub const REPEAT: [&str; 4] = [
    "every day",
    "every week",
    "every few hours",
    "every few minutes",
];
/// `on`.
pub const DAYS: [(&str, &str); 7] = [
    ("mon", "Monday"),
    ("tue", "Tuesday"),
    ("wed", "Wednesday"),
    ("thu", "Thursday"),
    ("fri", "Friday"),
    ("sat", "Saturday"),
    ("sun", "Sunday"),
];
/// `at least`.
pub const SEVERITY: [&str; 3] = ["info", "warning", "critical"];
/// `at most`.
pub const TIERS: [(Tier, &str); 3] = [
    (Tier::T0, "T0 look and report"),
    (Tier::T1, "T1 your files and user services"),
    (Tier::T2, "T2 the system: packages, services, sudo"),
];
/// `tell me`.
pub const NOTIFY: [(&str, &str); 3] = [
    ("never", "only when it needs me"),
    ("after", "after every run"),
    ("before", "when it starts, and after"),
];

/// The form's state.
#[derive(Debug, Clone, PartialEq)]
pub struct OrderForm {
    /// The order's id, when editing one.
    pub editing: Option<String>,
    /// The example it started from (0 is blank).
    pub start: usize,
    /// Name.
    pub name: Text,
    /// What to do.
    pub task: Text,
    /// Index into [`WHEN`].
    pub when: usize,
    /// Index into [`REPEAT`].
    pub repeat: usize,
    /// Hours or minutes.
    pub every: Text,
    /// `03:00`.
    pub time: Text,
    /// Index into [`DAYS`].
    pub day: usize,
    /// Ticked [`FINDING_KINDS`].
    pub kinds: Vec<bool>,
    /// Other finding patterns.
    pub custom: Text,
    /// Index into [`SEVERITY`].
    pub severity: usize,
    /// Index into [`TIERS`].
    pub tier: usize,
    /// Ticked [`TOOL_GROUPS`].
    pub tools: Vec<bool>,
    /// Tools the order names that no group covers (kept as they are).
    pub other_tools: Vec<String>,
    /// Command globs.
    pub commands: Text,
    /// Path globs.
    pub paths: Text,
    /// Dollars.
    pub per_run: Text,
    /// Count.
    pub per_day: Text,
    /// Hours.
    pub between: Text,
    /// Index into [`NOTIFY`].
    pub notify: usize,
    /// On.
    pub enabled: bool,
    /// Kept from the file.
    pub connection: Option<String>,
    /// Kept from the file.
    pub model: Option<String>,
    /// Focused field, an index into [`OrderForm::fields`].
    pub focus: usize,
    /// Changed since opened.
    pub dirty: bool,
    /// Esc was pressed once with changes.
    pub confirm_close: bool,
    /// Why it can't be saved yet.
    pub error: Option<String>,
    /// A word about the last key (what a second press would do), until
    /// the next key.
    pub note: Option<String>,
    /// Typed into since the last "start from" pick: picking another asks.
    pub typed: bool,
    /// "Start from" pressed once over typing.
    pub confirm_start: bool,
    /// The order as it was opened, when editing: saving writes only what
    /// changed since.
    pub base: Option<Order>,
    /// Save anyway: over a clash in the file, or where it was deleted.
    pub over: bool,
    /// Brought back from an unsaved draft.
    pub restored: bool,
    /// The limits were typed by hand: a new schedule leaves them be.
    pub limits_typed: bool,
}

impl OrderForm {
    /// An empty order, off, with safe limits.
    pub fn new() -> Self {
        Self {
            editing: None,
            start: 0,
            name: Text::default(),
            task: Text::default(),
            when: 0,
            repeat: 0,
            every: Text::new("6"),
            time: Text::new("03:00"),
            day: 6,
            kinds: vec![false; FINDING_KINDS.len()],
            custom: Text::default(),
            severity: 1,
            tier: 1,
            tools: TOOL_GROUPS.iter().map(|(_, t)| t == &["shell"]).collect(),
            other_tools: Vec::new(),
            commands: Text::default(),
            paths: Text::default(),
            per_run: Text::new("0.05"),
            per_day: Text::new(limits_for(Some("daily 03:00")).0.to_string()),
            between: Text::new(number(limits_for(Some("daily 03:00")).1)),
            notify: 0,
            enabled: false,
            connection: None,
            model: None,
            focus: 0,
            dirty: false,
            confirm_close: false,
            error: None,
            note: None,
            typed: false,
            confirm_start: false,
            base: None,
            over: false,
            restored: false,
            limits_typed: false,
        }
    }

    /// An existing order, to edit.
    pub fn edit(o: &Order) -> Self {
        let mut f = Self::from_order(o);
        f.editing = Some(o.id.clone());
        f.base = Some(o.clone());
        f
    }

    fn from_order(o: &Order) -> Self {
        let mut f = Self::new();
        f.name = Text::new(o.name.clone());
        f.task = Text::new(o.task.trim().to_string());
        let has_schedule = o.trigger.schedule.is_some();
        let has_findings = !o.trigger.findings.is_empty();
        f.when = match (has_schedule, has_findings) {
            (true, true) => 2,
            (false, true) => 1,
            _ => 0,
        };
        if let Some(s) = o
            .trigger
            .schedule
            .as_deref()
            .and_then(|s| Schedule::parse(s).ok())
        {
            match s {
                Schedule::Daily(t) => {
                    f.repeat = 0;
                    f.time = Text::new(t.format("%H:%M").to_string());
                }
                Schedule::Weekly(d, t) => {
                    f.repeat = 1;
                    f.day = d.num_days_from_monday() as usize;
                    f.time = Text::new(t.format("%H:%M").to_string());
                }
                Schedule::Every(min) if min % 60 == 0 => {
                    f.repeat = 2;
                    f.every = Text::new((min / 60).to_string());
                }
                Schedule::Every(min) => {
                    f.repeat = 3;
                    f.every = Text::new(min.to_string());
                }
            }
        }
        let mut custom = Vec::new();
        for pat in &o.trigger.findings {
            match FINDING_KINDS.iter().position(|(k, _)| k == pat) {
                Some(i) => f.kinds[i] = true,
                None => custom.push(pat.clone()),
            }
        }
        f.custom = Text::new(custom.join("\n"));
        f.severity = o
            .trigger
            .min_severity
            .as_deref()
            .and_then(|s| SEVERITY.iter().position(|x| x.eq_ignore_ascii_case(s)))
            .unwrap_or(0);
        f.tier = TIERS
            .iter()
            .position(|(t, _)| *t == o.scope.max_tier)
            .unwrap_or(1);
        f.tools = TOOL_GROUPS
            .iter()
            .map(|(_, names)| names.iter().all(|n| o.scope.tools.iter().any(|t| t == n)))
            .collect();
        f.other_tools = o
            .scope
            .tools
            .iter()
            .filter(|t| {
                !TOOL_GROUPS
                    .iter()
                    .any(|(_, names)| names.contains(&t.as_str()))
            })
            .cloned()
            .collect();
        f.commands = Text::new(o.scope.commands.join("\n"));
        f.paths = Text::new(o.scope.paths.join("\n"));
        f.per_run = Text::new(number(o.budget.per_run_usd));
        f.per_day = Text::new(o.budget.runs_per_day.to_string());
        f.between = Text::new(number(o.budget.cooldown_hours));
        f.notify = NOTIFY.iter().position(|(n, _)| *n == o.notify).unwrap_or(0);
        f.enabled = o.enabled;
        f.connection.clone_from(&o.connection);
        f.model.clone_from(&o.model);
        f
    }

    /// Changed by hand.
    fn touch(&mut self) {
        self.dirty = true;
        self.typed = true;
    }

    /// Fill the form from example `i` (0 empties it).
    fn start_from(&mut self, i: usize) {
        let focus = self.focus;
        let mut next = match i.checked_sub(1).and_then(|k| EXAMPLES.get(k)) {
            Some((id, text)) => reeve_core::orders::parse(id, text)
                .map(|mut o| {
                    o.enabled = false;
                    Self::from_order(&o)
                })
                .unwrap_or_else(|_| Self::new()),
            None => Self::new(),
        };
        next.start = i;
        next.focus = focus;
        next.dirty = true;
        *self = next;
    }

    /// The fields showing, in order: what's hidden depends on the choices.
    pub fn fields(&self) -> Vec<Field> {
        let mut v = Vec::new();
        if self.editing.is_none() {
            v.push(Field::Start);
        }
        v.extend([Field::Name, Field::Task, Field::When]);
        if self.when != 1 {
            v.push(Field::Repeat);
            match self.repeat {
                0 => v.push(Field::Time),
                1 => v.extend([Field::Day, Field::Time]),
                _ => v.push(Field::Every),
            }
        }
        if self.when != 0 {
            v.extend((0..FINDING_KINDS.len()).map(Field::Kind));
            v.extend([Field::Custom, Field::Severity]);
        }
        v.push(Field::Tier);
        if self.tier > 0 {
            v.extend((0..TOOL_GROUPS.len()).map(Field::Tool));
            v.extend([Field::Commands, Field::Paths]);
        }
        v.extend([
            Field::PerRun,
            Field::PerDay,
            Field::Between,
            Field::Notify,
            Field::Enabled,
        ]);
        v
    }

    /// The field with the focus.
    pub fn focused(&self) -> Field {
        let f = self.fields();
        f[self.focus.min(f.len() - 1)]
    }

    fn focus_on(&mut self, field: Field) {
        if let Some(i) = self.fields().iter().position(|f| *f == field) {
            self.focus = i;
        }
    }

    fn text_mut(&mut self, f: Field) -> Option<&mut Text> {
        Some(match f {
            Field::Name => &mut self.name,
            Field::Task => &mut self.task,
            Field::Every => &mut self.every,
            Field::Time => &mut self.time,
            Field::Custom => &mut self.custom,
            Field::Commands => &mut self.commands,
            Field::Paths => &mut self.paths,
            Field::PerRun => &mut self.per_run,
            Field::PerDay => &mut self.per_day,
            Field::Between => &mut self.between,
            _ => return None,
        })
    }

    fn text(&self, f: Field) -> Option<&Text> {
        Some(match f {
            Field::Name => &self.name,
            Field::Task => &self.task,
            Field::Every => &self.every,
            Field::Time => &self.time,
            Field::Custom => &self.custom,
            Field::Commands => &self.commands,
            Field::Paths => &self.paths,
            Field::PerRun => &self.per_run,
            Field::PerDay => &self.per_day,
            Field::Between => &self.between,
            _ => return None,
        })
    }

    /// Step a choice by `d` (±1), wrapping.
    fn choose(&mut self, f: Field, d: isize) {
        let step = |v: usize, n: usize| ((v as isize + d).rem_euclid(n as isize)) as usize;
        match f {
            Field::Start => {
                if self.typed && !self.confirm_start {
                    self.confirm_start = true;
                    self.note = Some(
                        "that replaces what you've written here: press it again to switch".into(),
                    );
                    return;
                }
                let i = step(self.start, EXAMPLES.len() + 1);
                self.start_from(i);
                return;
            }
            Field::When => self.when = step(self.when, WHEN.len()),
            Field::Repeat => self.repeat = step(self.repeat, REPEAT.len()),
            Field::Day => self.day = step(self.day, DAYS.len()),
            Field::Severity => self.severity = step(self.severity, SEVERITY.len()),
            Field::Tier => self.tier = step(self.tier, TIERS.len()),
            Field::Notify => self.notify = step(self.notify, NOTIFY.len()),
            _ => return,
        }
        self.touch();
    }

    fn toggle(&mut self, f: Field) {
        match f {
            Field::Kind(i) => self.kinds[i] = !self.kinds[i],
            Field::Tool(i) => self.tools[i] = !self.tools[i],
            Field::Enabled => self.enabled = !self.enabled,
            _ => return,
        }
        self.touch();
    }

    /// The schedule, as the order file writes it.
    fn schedule(&self) -> Result<String, (Field, String)> {
        let time = self.time.s.trim();
        let t = || {
            NaiveTime::parse_from_str(time, "%H:%M")
                .map(|t| t.format("%H:%M").to_string())
                .map_err(|_| (Field::Time, format!("{time:?} isn't a time like 03:00")))
        };
        let n = || {
            self.every
                .s
                .trim()
                .parse::<u32>()
                .map_err(|_| (Field::Every, "give a whole number".to_string()))
        };
        let s = match self.repeat {
            0 => format!("daily {}", t()?),
            1 => format!("weekly {} {}", DAYS[self.day].0, t()?),
            2 => match n()? {
                0 => return Err((Field::Every, "at least every hour".into())),
                h => format!("every {h}h"),
            },
            _ => match n()? {
                m if m < 5 => return Err((Field::Every, "at least every 5 minutes".into())),
                m => format!("every {m}m"),
            },
        };
        Schedule::parse(&s).map_err(|e| (Field::Repeat, e))?;
        Ok(s)
    }

    /// The schedule it would save, if any.
    fn saved_schedule(&self) -> Option<String> {
        (self.when != 1).then(|| self.schedule().ok()).flatten()
    }

    fn findings(&self) -> Vec<String> {
        let mut v: Vec<String> = FINDING_KINDS
            .iter()
            .zip(&self.kinds)
            .filter(|(_, on)| **on)
            .map(|((k, _), _)| (*k).to_string())
            .collect();
        v.extend(self.custom.lines());
        v
    }

    fn tool_names(&self) -> Vec<String> {
        let mut v: Vec<String> = TOOL_GROUPS
            .iter()
            .zip(&self.tools)
            .filter(|(_, on)| **on)
            .flat_map(|((_, names), _)| names.iter().map(|n| (*n).to_string()))
            .collect();
        v.extend(self.other_tools.iter().cloned());
        v
    }

    /// The order the form describes, or the first thing that stops it, and where.
    pub fn order(&self) -> Result<Order, (Field, String)> {
        let name = self.name.s.trim();
        if name.is_empty() {
            return Err((Field::Name, "give it a name".into()));
        }
        let task = self.task.s.trim();
        if task.is_empty() {
            return Err((Field::Task, "say what Reeve should do".into()));
        }
        let schedule = if self.when != 1 {
            Some(self.schedule()?)
        } else {
            None
        };
        let findings = if self.when != 0 {
            self.findings()
        } else {
            Vec::new()
        };
        if self.when != 0 && findings.is_empty() {
            return Err((
                Field::Kind(0),
                "tick at least one thing reeved finds (or run it on a schedule)".into(),
            ));
        }
        let money = |t: &Text, f: Field| {
            t.s.trim()
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v >= 0.0)
                .ok_or((f, format!("{:?} isn't a number", t.s.trim())))
        };
        let per_run = money(&self.per_run, Field::PerRun)?;
        if per_run <= 0.0 {
            return Err((
                Field::PerRun,
                "give it something to spend, like 0.05".into(),
            ));
        }
        let per_day = self
            .per_day
            .s
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|n| *n >= 1)
            .ok_or((Field::PerDay, "at least 1 run a day".to_string()))?;
        let between = money(&self.between, Field::Between)?;
        let tier = TIERS[self.tier].0;
        let changes = tier > Tier::T0;
        Ok(Order {
            id: self.editing.clone().unwrap_or_default(),
            name: name.to_string(),
            task: task.to_string(),
            enabled: self.enabled,
            trigger: Trigger {
                findings,
                schedule,
                // info is every finding, which is what leaving it out means.
                min_severity: (self.when != 0 && self.severity > 0)
                    .then(|| SEVERITY[self.severity].to_string()),
            },
            scope: Scope {
                max_tier: tier,
                tools: if changes {
                    self.tool_names()
                } else {
                    Vec::new()
                },
                commands: if changes {
                    self.commands.lines()
                } else {
                    Vec::new()
                },
                paths: if changes {
                    self.paths.lines()
                } else {
                    Vec::new()
                },
            },
            budget: Budget {
                per_run_usd: per_run,
                runs_per_day: per_day,
                cooldown_hours: between,
            },
            connection: self.connection.clone(),
            model: self.model.clone(),
            notify: NOTIFY[self.notify].0.to_string(),
        })
    }

    /// Handle a key.
    pub fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let n = self.fields().len();
        let f = self.focused();
        if k.code != KeyCode::Esc {
            self.confirm_close = false;
        }
        let before = self.saved_schedule();
        if matches!(f, Field::PerDay | Field::Between)
            && matches!(
                k.code,
                KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
            )
        {
            self.limits_typed = true;
        }
        let picking = f == Field::Start
            && matches!(k.code, KeyCode::Left | KeyCode::Right | KeyCode::Char(' '));
        if !picking {
            self.confirm_start = false;
        }
        self.note = None;
        match k.code {
            KeyCode::Esc => {
                if self.dirty && !self.confirm_close {
                    self.confirm_close = true;
                    self.error = None;
                    self.note = Some(if self.restored {
                        "esc again to throw this draft away".into()
                    } else {
                        "esc again to close: n brings it back until you quit".into()
                    });
                    return Action::None;
                }
                return Action::Close;
            }
            KeyCode::Char('s') if ctrl => match self.order() {
                Ok(o) => {
                    return Action::SaveOrder {
                        editing: self.editing.clone(),
                        order: Box::new(o),
                    };
                }
                Err((field, why)) => {
                    self.focus_on(field);
                    self.error = Some(why);
                    return Action::None;
                }
            },
            KeyCode::Up | KeyCode::BackTab => self.focus = self.focus.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => self.focus = (self.focus + 1).min(n - 1),
            KeyCode::PageUp => self.focus = self.focus.saturating_sub(5),
            KeyCode::PageDown => self.focus = (self.focus + 5).min(n - 1),
            KeyCode::Enter => match f.kind() {
                Kind::Block if !alt => {
                    if let Some(t) = self.text_mut(f) {
                        t.insert('\n');
                        self.touch();
                    }
                }
                Kind::Check => self.toggle(f),
                _ => self.focus = (self.focus + 1).min(n - 1),
            },
            KeyCode::Left => match f.kind() {
                Kind::Choice => self.choose(f, -1),
                _ => {
                    if let Some(t) = self.text_mut(f) {
                        t.left();
                    }
                }
            },
            KeyCode::Right => match f.kind() {
                Kind::Choice => self.choose(f, 1),
                _ => {
                    if let Some(t) = self.text_mut(f) {
                        t.right();
                    }
                }
            },
            KeyCode::Char(' ') if matches!(f.kind(), Kind::Check) => self.toggle(f),
            KeyCode::Char(' ') if matches!(f.kind(), Kind::Choice) => self.choose(f, 1),
            KeyCode::Home => {
                if let Some(t) = self.text_mut(f) {
                    t.cur = 0;
                }
            }
            KeyCode::End => {
                if let Some(t) = self.text_mut(f) {
                    t.cur = t.s.len();
                }
            }
            KeyCode::Backspace => {
                if let Some(t) = self.text_mut(f) {
                    t.backspace();
                    self.touch();
                }
            }
            KeyCode::Delete => {
                if let Some(t) = self.text_mut(f) {
                    t.delete();
                    self.touch();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                let fits = match f {
                    Field::Every | Field::PerDay => c.is_ascii_digit(),
                    Field::PerRun | Field::Between => c.is_ascii_digit() || c == '.',
                    Field::Time => c.is_ascii_digit() || c == ':',
                    _ => true,
                };
                if fits {
                    if let Some(t) = self.text_mut(f) {
                        t.insert(c);
                        self.touch();
                    }
                }
            }
            _ => {}
        }
        // A new schedule brings limits that let it run each time it's due,
        // unless they were set by hand, or (editing) the old ones still fit.
        let after = self.saved_schedule();
        if after != before && f != Field::Start && !self.limits_typed {
            let fits = self.order().is_ok_and(|o| o.pace_warning().is_none());
            if self.editing.is_none() || !fits {
                let (per_day, between) = limits_for(after.as_deref());
                self.per_day = Text::new(per_day.to_string());
                self.between = Text::new(number(between));
            }
        }
        // The error was about what's being fixed: let it go once something changes.
        if self.dirty && self.order().is_ok() {
            self.error = None;
        }
        Action::None
    }

    /// Pasted text goes into the focused text field.
    pub fn paste(&mut self, s: &str) {
        let f = self.focused();
        let block = matches!(f.kind(), Kind::Block);
        if let Some(t) = self.text_mut(f) {
            for c in s.chars() {
                if c != '\n' || block {
                    t.insert(c);
                }
            }
            self.touch();
        }
    }
}

impl Default for OrderForm {
    fn default() -> Self {
        Self::new()
    }
}

/// `0.05`, `6`: short, and it reads back as the same number.
fn number(v: f64) -> String {
    let s = format!("{v}");
    s.strip_suffix(".0").map(String::from).unwrap_or(s)
}

// ── drawing ─────────────────────────────────────────────────────────────────

const LABEL_W: usize = 19;
const SECTIONS: [&str; 5] = ["What", "When", "What it may do", "Limits", "After"];

/// The form over everything, with what it will do beside it.
pub fn draw(f: &mut Frame, area: Rect, form: &OrderForm, v: &View, t: &Theme) {
    let w = area.width.saturating_sub(4).min(150);
    let h = area.height.saturating_sub(2);
    let r = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + 1,
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    crate::board::shadow(f, r, t);
    let title = if form.editing.is_some() {
        " edit a standing order "
    } else {
        " new standing order "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(
            Style::default()
                .fg(t.mix(t.border, t.brass, 0.45))
                .bg(t.panel),
        )
        .title(Span::styled(
            title,
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ))
        .style(Style::default().bg(t.panel));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
    };
    if inner.height < 6 || inner.width < 40 {
        return;
    }
    // Footer: why it can't be saved yet (or what a second press does),
    // wrapped above the keys.
    let msg: Vec<Line<'static>> = match (&form.error, &form.note) {
        (Some(e), _) => plain_wrap(&format!("! {e}"), inner.width as usize)
            .into_iter()
            .take(3)
            .map(|l| Line::from(Span::styled(l, Style::default().fg(t.warn))))
            .collect(),
        (None, Some(n)) => plain_wrap(n, inner.width as usize)
            .into_iter()
            .take(3)
            .map(|l| Line::from(Span::styled(l, t.muted())))
            .collect(),
        _ => Vec::new(),
    };
    let foot_h = 1 + msg.len() as u16;
    let foot = Rect {
        y: inner.bottom().saturating_sub(foot_h),
        height: foot_h,
        ..inner
    };
    let keys = crate::board::hints(
        &[
            ("↑↓", "move"),
            ("←→", "choose"),
            ("space", "tick"),
            ("ctrl+s", "save"),
            ("esc", "cancel"),
        ],
        t,
    );
    let mut foot_lines = msg;
    foot_lines.push(Line::from(keys));
    f.render_widget(Paragraph::new(foot_lines), foot);
    let body = Rect {
        height: inner.height.saturating_sub(foot_h + 1),
        ..inner
    };
    // Side by side when there's room; else what it will do goes below.
    let wide = body.width >= 110;
    let (form_r, side_r) = if wide {
        let side_w = (body.width * 38 / 100).clamp(40, 56);
        (
            Rect {
                width: body.width - side_w - 3,
                ..body
            },
            Some(Rect {
                x: body.right() - side_w,
                width: side_w,
                ..body
            }),
        )
    } else {
        (body, None)
    };
    let (mut lines, focus_at, cursor) = form_lines(form, form_r.width as usize, t);
    let summary = plain(
        form,
        v,
        side_r.map_or(form_r.width, |s| s.width.saturating_sub(4)) as usize,
        t,
    );
    if side_r.is_none() {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            "What it will do",
            Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
        )));
        lines.extend(summary.clone());
    }
    // Keep the focused field on screen, a little below the top. Below the
    // last field, narrow, is what it will do: scroll all the way to it.
    let h = form_r.height as usize;
    let last = form.fields().last() == Some(&form.focused());
    let bottom = lines.len().saturating_sub(h);
    let skip = if last && side_r.is_none() {
        bottom.min(focus_at)
    } else {
        focus_at.saturating_sub(h / 3).min(bottom)
    };
    let shown: Vec<Line> = lines.into_iter().skip(skip).take(h).collect();
    f.render_widget(Paragraph::new(shown), form_r);
    if let Some((row, col)) = cursor {
        if row >= skip && row < skip + h {
            f.set_cursor_position(Position::new(
                form_r.x + col as u16,
                form_r.y + (row - skip) as u16,
            ));
        }
    }
    if let Some(s) = side_r {
        let inside = crate::board::surface(f, s, t.inset, t.panel, t);
        let mut out = vec![
            Line::from(Span::styled(
                "What it will do",
                Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
        ];
        out.extend(summary);
        f.render_widget(Paragraph::new(out), inside);
    }
}

/// The form's rows, the row the focused field starts on, and where the
/// cursor goes (row, column) when a text field has the focus.
fn form_lines(
    form: &OrderForm,
    width: usize,
    t: &Theme,
) -> (Vec<Line<'static>>, usize, Option<(usize, usize)>) {
    let val_w = width.saturating_sub(LABEL_W + 1).max(10);
    let fields = form.fields();
    let focused = form.focused();
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut focus_at = 0;
    let mut cursor = None;
    let mut section = usize::MAX;
    for f in fields {
        if f.section() != section {
            section = f.section();
            if !out.is_empty() {
                out.push(Line::raw(""));
            }
            out.push(Line::from(vec![
                Span::styled(
                    format!("{}  ", section + 1),
                    Style::default().fg(t.faint).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    SECTIONS[section],
                    Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        let on = f == focused;
        if on {
            focus_at = out.len();
        }
        let label = Span::styled(
            pad(
                &format!("{}{}", if on { "▸ " } else { "  " }, f.label()),
                LABEL_W,
            ),
            if on {
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
            } else {
                t.muted()
            },
        );
        let blank = || Span::raw(" ".repeat(LABEL_W));
        match f.kind() {
            Kind::Line | Kind::Block => {
                let text = form.text(f).cloned().unwrap_or_default();
                let bg = if on { t.input } else { t.panel };
                let placeholder = text.s.is_empty() && !on;
                let mut rows = if placeholder {
                    plain_wrap(example(f), val_w.saturating_sub(2))
                } else {
                    edit_rows(&text.s, val_w.saturating_sub(2))
                        .into_iter()
                        .map(|(_, r)| r)
                        .collect()
                };
                let min = match (f.kind(), on) {
                    (Kind::Block, true) => 3,
                    _ => 1,
                };
                while rows.len() < min {
                    rows.push(String::new());
                }
                for (i, row) in rows.iter().enumerate() {
                    let shown = if placeholder {
                        Span::styled(
                            format!(" {}", pad(row, val_w.saturating_sub(1))),
                            t.ghost().bg(bg),
                        )
                    } else {
                        Span::styled(
                            format!(" {}", pad(row, val_w.saturating_sub(1))),
                            t.text().bg(bg),
                        )
                    };
                    out.push(Line::from(vec![
                        if i == 0 { label.clone() } else { blank() },
                        shown,
                    ]));
                }
                if on {
                    let (r, c) = edit_cursor(&text.s, text.cur, val_w.saturating_sub(2));
                    cursor = Some((focus_at + r, LABEL_W + 1 + c));
                }
            }
            Kind::Choice => {
                let (opts, sel): (Vec<String>, usize) = match f {
                    Field::Start => (
                        std::iter::once("blank".to_string())
                            .chain(EXAMPLES.iter().map(|(id, text)| {
                                reeve_core::orders::parse(id, text)
                                    .map_or((*id).to_string(), |o| o.name)
                            }))
                            .collect(),
                        form.start,
                    ),
                    Field::When => (WHEN.iter().map(|s| (*s).to_string()).collect(), form.when),
                    Field::Repeat => (
                        REPEAT.iter().map(|s| (*s).to_string()).collect(),
                        form.repeat,
                    ),
                    Field::Day => (DAYS.iter().map(|d| d.1.to_string()).collect(), form.day),
                    Field::Severity => (
                        SEVERITY.iter().map(|s| format!("{s} or worse")).collect(),
                        form.severity,
                    ),
                    Field::Tier => (TIERS.iter().map(|x| x.1.to_string()).collect(), form.tier),
                    Field::Notify => (
                        NOTIFY.iter().map(|x| x.1.to_string()).collect(),
                        form.notify,
                    ),
                    _ => (Vec::new(), 0),
                };
                // The options as pills, wrapped to the width; the chosen one lit.
                let mut row: Vec<Span<'static>> = vec![label.clone()];
                let mut used = LABEL_W;
                for (i, o) in opts.iter().enumerate() {
                    let wd = o.width() + 3;
                    if used + wd > width && used > LABEL_W {
                        out.push(Line::from(std::mem::take(&mut row)));
                        row.push(blank());
                        used = LABEL_W;
                    }
                    row.push(Span::styled(
                        format!(" {o} "),
                        if i == sel {
                            if on { t.key() } else { t.pill(t.fg) }
                        } else {
                            Style::default().fg(t.faint)
                        },
                    ));
                    row.push(Span::raw(" "));
                    used += wd;
                }
                if on {
                    row.push(Span::styled(" ←→", t.ghost()));
                }
                out.push(Line::from(row));
            }
            Kind::Check => {
                let (text, ticked) = match f {
                    Field::Kind(i) => (FINDING_KINDS[i].1.to_string(), form.kinds[i]),
                    Field::Tool(i) => (TOOL_GROUPS[i].0.to_string(), form.tools[i]),
                    _ => (
                        if form.enabled { "on" } else { "off, for now" }.to_string(),
                        form.enabled,
                    ),
                };
                let bg = if on { t.input } else { t.panel };
                out.push(Line::from(vec![
                    label.clone(),
                    Span::styled(
                        if ticked { " [✓] " } else { " [ ] " },
                        Style::default()
                            .fg(if ticked { t.good } else { t.faint })
                            .bg(bg),
                    ),
                    Span::styled(
                        pad(&text, val_w.saturating_sub(6)),
                        (if ticked { t.text() } else { t.muted() }).bg(bg),
                    ),
                ]));
            }
        }
        let help = f.help(form.repeat);
        if !help.is_empty() {
            // The focused field's help reads a shade brighter.
            for l in plain_wrap(help, val_w) {
                out.push(Line::from(vec![
                    blank(),
                    Span::styled(l, if on { t.muted() } else { t.ghost() }),
                ]));
            }
        }
    }
    (out, focus_at, cursor)
}

/// A text field's rows, wrapped at spaces where it can, each with the
/// byte it starts at. Rows keep their trailing space so every byte of the
/// text is on exactly one row.
fn edit_rows(text: &str, width: usize) -> Vec<(usize, String)> {
    let width = width.max(2);
    let mut out = Vec::new();
    let mut start = 0;
    for para in text.split('\n') {
        let mut at = 0;
        loop {
            let rest = &para[at..];
            let mut used = 0;
            let mut fit = rest.len();
            let mut space = None;
            for (i, ch) in rest.char_indices() {
                let cw = ch.width().unwrap_or(0);
                if used + cw > width {
                    fit = i;
                    break;
                }
                used += cw;
                if ch == ' ' {
                    space = Some(i + 1);
                }
            }
            if fit == rest.len() {
                out.push((start + at, rest.to_string()));
                break;
            }
            let cut = space.filter(|&s| s > 0).unwrap_or(fit.max(1));
            out.push((start + at, rest[..cut].to_string()));
            at += cut;
        }
        start += para.len() + 1;
    }
    out
}

/// Row and column of byte `cur` under [`edit_rows`].
fn edit_cursor(text: &str, cur: usize, width: usize) -> (usize, usize) {
    let rows = edit_rows(text, width);
    let row = rows.iter().rposition(|(s, _)| *s <= cur).unwrap_or(0);
    let (s, _) = &rows[row];
    (row, text[*s..cur].width())
}

/// What an empty field could hold.
fn example(f: Field) -> &'static str {
    match f {
        Field::Name => "Keep the journal small",
        Field::Task => {
            "If the journal uses more than 1 GB, vacuum it to 1 GB and say how much was freed."
        }
        Field::Custom => "unit-failed:bluetooth*",
        Field::Commands => "sudo journalctl --vacuum-size=*",
        Field::Paths => "~/.cache/thumbnails/**",
        _ => "",
    }
}

/// The order in plain words: when, what, what it may change, what it
/// spends, and what to fix before saving.
fn plain(form: &OrderForm, v: &View, w: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let para = |out: &mut Vec<Line<'static>>, text: String, style: Style| {
        for l in plain_wrap(&text, w) {
            out.push(Line::from(Span::styled(l, style)));
        }
    };
    let name = form.name.s.trim();
    if !name.is_empty() {
        para(
            &mut out,
            name.to_string(),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        );
    }
    // When.
    let schedule = match form.repeat {
        0 => format!("every day at {}", form.time.s.trim()),
        1 => format!("every {} at {}", DAYS[form.day].1, form.time.s.trim()),
        2 => match form.every.s.trim() {
            "1" => "every hour".to_string(),
            n => format!("every {n} hours"),
        },
        _ => format!("every {} minutes", form.every.s.trim()),
    };
    let mut patterns: Vec<String> = FINDING_KINDS
        .iter()
        .zip(&form.kinds)
        .filter(|(_, on)| **on)
        .map(|((k, _), _)| (*k).to_string())
        .collect();
    patterns.extend(form.custom.lines());
    let finds = finding_words(&patterns);
    let when = match (form.when, finds.is_empty()) {
        (0, _) => format!("Runs {schedule}."),
        (1, true) => "Runs when reeved finds… (tick something).".to_string(),
        (1, false) => format!(
            "Runs when reeved finds that {finds} ({} or worse).",
            SEVERITY[form.severity]
        ),
        (_, true) => format!("Runs {schedule}, and when reeved finds… (tick something)."),
        (_, false) => format!(
            "Runs {schedule}, and when reeved finds that {finds} ({} or worse).",
            SEVERITY[form.severity]
        ),
    };
    para(&mut out, when, t.text());
    out.push(Line::raw(""));
    // What.
    if form.task.s.trim().is_empty() {
        para(
            &mut out,
            "Reeve will do what you write under “what to do”.".into(),
            t.muted(),
        );
    } else {
        para(&mut out, "Reeve will:".into(), t.muted());
        for l in form.task.s.trim().lines().take(8) {
            for row in plain_wrap(l, w.saturating_sub(2)) {
                out.push(Line::from(vec![
                    Span::styled("▎ ", Style::default().fg(t.border)),
                    Span::styled(row, t.text()),
                ]));
            }
        }
    }
    out.push(Line::raw(""));
    // What it may change.
    let tier = TIERS[form.tier];
    if form.tier == 0 {
        para(
            &mut out,
            "It only looks and reports: it can't change anything.".into(),
            t.text(),
        );
    } else {
        let commands = form.commands.lines();
        let paths = form.paths.lines();
        let (code, what) = tier.1.split_once(' ').unwrap_or((tier.1, ""));
        para(
            &mut out,
            format!("It may change things up to {code} ({what}), and only:"),
            t.text(),
        );
        let item = |out: &mut Vec<Line<'static>>, lead: &str, what: &str| {
            out.push(Line::from(vec![
                Span::styled(format!("  · {lead}"), t.muted()),
                Span::styled(
                    truncate(what, w.saturating_sub(lead.width() + 4)),
                    Style::default().fg(t.code),
                ),
            ]));
        };
        for c in &commands {
            item(&mut out, "running ", c);
        }
        for p in &paths {
            item(&mut out, "files in ", p);
        }
        if commands.is_empty() && paths.is_empty() {
            out.push(Line::from(Span::styled(
                "  · (no commands or files listed yet)",
                t.ghost(),
            )));
        }
        let groups: Vec<&str> = TOOL_GROUPS
            .iter()
            .zip(&form.tools)
            .filter(|(_, on)| **on)
            .map(|((g, _), _)| *g)
            .collect();
        if !groups.is_empty() {
            let text = format!("with tools to {}", groups.join(" · "));
            for (i, row) in plain_wrap(&text, w.saturating_sub(4))
                .into_iter()
                .enumerate()
            {
                out.push(Line::from(vec![
                    Span::styled(if i == 0 { "  · " } else { "    " }, t.muted()),
                    Span::styled(row, t.muted()),
                ]));
            }
        }
        para(
            &mut out,
            "Anything outside that is refused and waits for you as a proposal.".into(),
            t.muted(),
        );
    }
    out.push(Line::raw(""));
    // What it spends, and when it tells you.
    para(
        &mut out,
        format!(
            "It spends at most ${} a run{}, runs at most {} time{} a day, {} hour{} apart.",
            form.per_run.s.trim(),
            match (
                form.per_run.s.trim().parse::<f64>(),
                form.per_day.s.trim().parse::<u32>(),
            ) {
                (Ok(r), Ok(n)) if n > 1 => format!(" (${:.2} a day at most)", r * f64::from(n)),
                _ => String::new(),
            },
            form.per_day.s.trim(),
            if form.per_day.s.trim() == "1" {
                ""
            } else {
                "s"
            },
            form.between.s.trim(),
            if form.between.s.trim() == "1" {
                ""
            } else {
                "s"
            },
        ),
        t.text(),
    );
    para(
        &mut out,
        format!(
            "It tells you {}.",
            match form.notify {
                0 => "only when it needs you",
                1 => "after every run",
                _ => "when it starts and after each run",
            }
        ),
        t.text(),
    );
    para(
        &mut out,
        if form.enabled {
            "It's on: reeved runs it when it's due.".into()
        } else {
            "It starts off: space in Orders turns it on.".into()
        },
        t.muted(),
    );
    // Matches now, and what to fix.
    out.push(Line::raw(""));
    // ✓ good, ○ still to do, ! worth knowing.
    let mut notes: Vec<(char, String)> = Vec::new();
    match form.order() {
        Ok(o) => {
            let matching = v.findings.iter().filter(|x| o.wants(x)).count();
            if form.when != 0 {
                notes.push((
                    '✓',
                    format!(
                        "matches {matching} open finding{} right now",
                        if matching == 1 { "" } else { "s" }
                    ),
                ));
            }
            if let Some(w) = o.pace_warning() {
                notes.push((
                    '!',
                    format!("{w}: raise runs a day or shorten between runs"),
                ));
            }
            notes.push(('✓', "ready to save (ctrl+s)".into()));
        }
        Err((_, why)) => notes.push(('○', format!("still to do: {why}"))),
    }
    if form.tier > 0 && form.commands.lines().is_empty() && form.paths.lines().is_empty() {
        notes.push((
            '○',
            "it can't change anything yet: list the commands or files it may change".into(),
        ));
    }
    if form.tier > 0 && form.commands.lines().iter().any(|c| c.starts_with("sudo ")) {
        notes.push((
            '!',
            "sudo needs a sudoers rule, since nobody's there to type a password: after saving, s in Orders shows the lines".into(),
        ));
    }
    if !v.observer_alive {
        notes.push((
            '!',
            "reeved isn't running: orders only run while it is (/observer)".into(),
        ));
    }
    for (icon, text) in notes {
        let c = match icon {
            '✓' => t.good,
            '○' => t.faint,
            _ => t.warn,
        };
        for (i, row) in plain_wrap(&text, w.saturating_sub(2))
            .into_iter()
            .enumerate()
        {
            out.push(Line::from(vec![
                Span::styled(
                    if i == 0 {
                        format!("{icon} ")
                    } else {
                        "  ".into()
                    },
                    Style::default().fg(c),
                ),
                Span::styled(row, t.muted()),
            ]));
        }
    }
    let id = form
        .editing
        .clone()
        .unwrap_or_else(|| slug(if name.is_empty() { "order" } else { name }));
    out.push(Line::raw(""));
    para(
        &mut out,
        format!("Saves to ~/.reeve/orders/{id}.toml"),
        t.ghost(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEventKind;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new_with_kind(code, KeyModifiers::NONE, KeyEventKind::Press)
    }

    fn typed(f: &mut OrderForm, s: &str) {
        for c in s.chars() {
            f.on_key(key(KeyCode::Char(c)));
        }
    }

    #[test]
    fn a_new_order_filled_in_saves_as_a_valid_file() {
        let mut f = OrderForm::new();
        // start from → name → what to do.
        f.on_key(key(KeyCode::Down));
        typed(&mut f, "Tidy the journal");
        f.on_key(key(KeyCode::Down));
        typed(&mut f, "Vacuum the journal to 1 GB.");
        // It runs: both a schedule and findings.
        f.focus_on(Field::When);
        f.on_key(key(KeyCode::Left));
        assert_eq!(f.when, 2);
        f.focus_on(Field::Repeat);
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.repeat, 1, "every week");
        f.focus_on(Field::Kind(0));
        f.on_key(key(KeyCode::Char(' ')));
        f.focus_on(Field::Tier);
        f.on_key(key(KeyCode::Right));
        f.focus_on(Field::Commands);
        typed(&mut f, "sudo journalctl --vacuum-size=*");
        let ctrl_s = KeyEvent::new_with_kind(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        let Action::SaveOrder { editing, order } = f.on_key(ctrl_s) else {
            panic!("didn't save: {:?}", f.error);
        };
        assert_eq!(editing, None);
        assert_eq!(order.trigger.schedule.as_deref(), Some("weekly sun 03:00"));
        assert_eq!(order.trigger.findings, vec!["disk-full:*"]);
        assert_eq!(order.scope.max_tier, Tier::T2);
        assert_eq!(order.scope.tools, vec!["shell"]);
        assert!(!order.enabled, "new orders start off");
        let text = reeve_core::orders::render(&order);
        assert!(
            reeve_core::orders::parse("tidy-the-journal", &text).is_ok(),
            "{text}"
        );
        assert_eq!(slug(&order.name), "tidy-the-journal");
    }

    #[test]
    fn what_stops_a_save_is_named_and_focused() {
        let mut f = OrderForm::new();
        f.name = Text::new("x");
        f.when = 1;
        let ctrl_s = KeyEvent::new_with_kind(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
            KeyEventKind::Press,
        );
        assert_eq!(f.on_key(ctrl_s), Action::None);
        assert_eq!(f.focused(), Field::Task);
        f.task = Text::new("do it");
        f.on_key(ctrl_s);
        assert_eq!(f.focused(), Field::Kind(0));
        assert!(f.error.as_deref().unwrap().contains("tick"));
    }

    #[test]
    fn every_example_edits_back_to_itself() {
        for (id, text) in EXAMPLES {
            let mut o = reeve_core::orders::parse(id, text).unwrap();
            let again = OrderForm::edit(&o).order().unwrap();
            // The form trims the task; a """ block ends with a newline.
            o.task = o.task.trim().to_string();
            assert_eq!(again, o, "{id}");
        }
    }

    #[test]
    fn starting_from_an_example_fills_the_form_and_stays_off() {
        let mut f = OrderForm::new();
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.start, 1);
        assert!(!f.name.s.is_empty() && !f.task.s.is_empty());
        assert!(!f.enabled);
        // Esc with changes asks once more.
        assert_eq!(f.on_key(key(KeyCode::Esc)), Action::None);
        assert_eq!(f.on_key(key(KeyCode::Esc)), Action::Close);
    }

    #[test]
    fn text_wraps_at_spaces_and_the_cursor_follows() {
        let text = "tidy the cache\nthen report";
        let rows: Vec<String> = edit_rows(text, 10).into_iter().map(|(_, r)| r).collect();
        assert_eq!(rows, ["tidy the ", "cache", "then ", "report"]);
        assert_eq!(edit_cursor(text, 0, 10), (0, 0));
        assert_eq!(edit_cursor(text, 9, 10), (1, 0));
        assert_eq!(edit_cursor(text, 14, 10), (1, 5));
        assert_eq!(edit_cursor(text, 15, 10), (2, 0));
        assert_eq!(edit_cursor(text, text.len(), 10), (3, 6));
        // A word longer than the row breaks where it must.
        let long = "abcdefghijklmnop";
        assert_eq!(edit_rows(long, 10).len(), 2);
        assert_eq!(edit_cursor(long, 10, 10), (1, 0));
        assert_eq!(edit_rows("", 10), [(0, String::new())]);
    }

    #[test]
    fn picking_an_example_over_typing_asks_first() {
        let mut f = OrderForm::new();
        f.on_key(key(KeyCode::Down));
        typed(&mut f, "My own order");
        f.on_key(key(KeyCode::Up));
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.name.s, "My own order", "kept");
        assert!(f.note.as_deref().unwrap().contains("again"));
        // Anything else in between, and it asks again.
        f.on_key(key(KeyCode::Down));
        f.on_key(key(KeyCode::Up));
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.name.s, "My own order");
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.start, 1);
        assert_ne!(f.name.s, "My own order");
        // An untouched example switches at once.
        f.on_key(key(KeyCode::Right));
        assert_eq!(f.start, 2);
    }

    #[test]
    fn the_form_draws_at_any_size() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let v = View::new(reeve_observer::HostInfo::default());
        let mut f = OrderForm::new();
        f.start_from(1);
        for (w, h) in [(160u16, 44u16), (100, 30), (60, 20), (30, 8)] {
            let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
            term.draw(|fr| draw(fr, fr.area(), &f, &v, &Theme::slate()))
                .unwrap();
        }
        let mut term = Terminal::new(TestBackend::new(160, 44)).unwrap();
        term.draw(|fr| draw(fr, fr.area(), &f, &v, &Theme::slate()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let s: String = (0..44)
            .map(|y| (0..160).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        for needle in [
            "new standing order",
            "What it will do",
            "1  What",
            "2  When",
            "ctrl+s",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }
}
