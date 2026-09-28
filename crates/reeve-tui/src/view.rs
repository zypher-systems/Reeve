//! Everything the screen draws, and the pure functions that change it.
//! No terminal in here, so every state is testable.

use std::collections::VecDeque;

use chrono::{DateTime, Local};
use reeve_core::agent::{AgentEvent, ApprovalRequest};
use reeve_core::diff::FileDiff;
use reeve_core::ledger::Totals;
use reeve_core::policy::Tier;
use reeve_core::receipts::{Receipt, Status};
use reeve_core::spend::{Tally, Usage};
use reeve_observer::{HostInfo, Snapshot};

/// Samples of history kept for the charts (one per second).
pub const HISTORY: usize = 240;

/// Who said a chat entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speaker {
    /// The person at the keyboard.
    User,
    /// The agent.
    Reeve,
    /// Reeve itself, about itself (startup, notices).
    System,
    /// Something failed.
    Error,
    /// A tool call (see [`Entry::tool`]).
    Tool,
}

/// A tool call as the chat shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolView {
    /// Provider call id.
    pub id: String,
    /// Tool name.
    pub tool: String,
    /// Tier.
    pub tier: Tier,
    /// The command or path.
    pub summary: String,
    /// `None` while running.
    pub status: Option<Status>,
    /// What happened.
    pub result: String,
    /// What changed.
    pub diff: Option<FileDiff>,
    /// Receipt number.
    pub seq: Option<u64>,
    /// The receipt carries an undo.
    pub undoable: bool,
    /// Who approved it.
    pub approved_by: Option<String>,
}

/// An approval waiting on the person.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    /// What's asked.
    pub req: ApprovalRequest,
    /// For T3: the confirmation typed so far.
    pub typed: String,
}

/// Receipts kept for the rail.
pub const RAIL_RECEIPTS: usize = 40;

/// One chat entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Speaker.
    pub who: Speaker,
    /// Body (Markdown-ish for Reeve).
    pub text: String,
    /// When it began.
    pub at: DateTime<Local>,
    /// Setup chatter that goes away once Reeve is connected.
    pub transient: bool,
    /// For [`Speaker::Tool`].
    pub tool: Option<ToolView>,
}

/// The last call's cost details, for the Spend panel.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LastCall {
    /// USD, if known.
    pub usd: Option<f64>,
    /// Tokens.
    pub usage: Usage,
}

/// Drawable state.
#[derive(Debug)]
pub struct View {
    /// Chat history.
    pub entries: Vec<Entry>,
    /// Composer text.
    pub input: String,
    /// Cursor byte offset in `input`.
    pub cursor: usize,
    /// Lines scrolled up from the bottom of the chat.
    pub scroll: usize,
    /// A turn is in flight.
    pub busy: bool,
    /// Reasoning text of the in-flight turn.
    pub thinking: String,
    /// The last entry is the in-flight reply, still receiving text.
    streaming: bool,
    /// Frames drawn (drives animation).
    pub frame: u64,
    /// Whether to animate.
    pub animate: bool,
    /// Auto-approve mode.
    pub yolo: bool,
    /// Show the right rail on a narrow screen instead of the chat.
    pub rail_only: bool,
    /// Machine identity.
    pub host: HostInfo,
    /// Latest readings.
    pub snap: Snapshot,
    /// CPU % history.
    pub cpu_hist: VecDeque<f32>,
    /// Memory use ratio history.
    pub mem_hist: VecDeque<f32>,
    /// Failed units (`None` until the first check finishes).
    pub failed: Option<Vec<String>>,
    /// Connection name.
    pub connection: String,
    /// Model id.
    pub model: String,
    /// `$3/M in · $15/M out`.
    pub rates: String,
    /// This session's spend.
    pub session: Tally,
    /// Today / month across Reeve.
    pub totals: Totals,
    /// Caps (0 = off).
    pub caps: Caps,
    /// Previous call.
    pub last: Option<LastCall>,
    /// The agent is ready (has a key and a model).
    pub ready: bool,
    /// Asked to quit.
    pub quit: bool,
    /// Floating panels, top last.
    pub overlays: Vec<crate::overlay::Overlay>,
    /// Selected row of the slash-command palette.
    pub palette_sel: usize,
    /// An approval card waiting for an answer.
    pub approval: Option<Pending>,
    /// Newest receipts first.
    pub receipts: Vec<Receipt>,
    /// Memory notes in use, and new or pending ones.
    pub memory: (usize, usize),
    /// A memory to open in `$EDITOR` (the loop hands over the terminal).
    pub edit_request: Option<(reeve_core::memory::Layer, String)>,
    /// A file to open in `$EDITOR` (an order).
    pub edit_file: Option<std::path::PathBuf>,
    /// reeved has a fresh heartbeat.
    pub observer_alive: bool,
    /// Open findings.
    pub findings: Vec<reeve_core::findings::Finding>,
    /// Drafter spend today and its cap, when it's on.
    pub drafter: Option<(f64, f64)>,
}

/// Spending caps, USD; 0 is off.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Caps {
    /// Per session.
    pub session: f64,
    /// Per day.
    pub daily: f64,
    /// Per month.
    pub monthly: f64,
}

impl View {
    /// A fresh screen.
    pub fn new(host: HostInfo) -> Self {
        Self {
            entries: Vec::new(),
            input: String::new(),
            cursor: 0,
            scroll: 0,
            busy: false,
            thinking: String::new(),
            streaming: false,
            frame: 0,
            animate: true,
            yolo: false,
            rail_only: false,
            host,
            snap: Snapshot::default(),
            cpu_hist: VecDeque::with_capacity(HISTORY),
            mem_hist: VecDeque::with_capacity(HISTORY),
            failed: None,
            connection: String::new(),
            model: String::new(),
            rates: String::new(),
            session: Tally::default(),
            totals: Totals::default(),
            caps: Caps::default(),
            last: None,
            ready: false,
            quit: false,
            overlays: Vec::new(),
            palette_sel: 0,
            approval: None,
            receipts: Vec::new(),
            memory: (0, 0),
            edit_request: None,
            edit_file: None,
            observer_alive: false,
            findings: Vec::new(),
            drafter: None,
        }
    }

    /// Remember a receipt for the rail (and mark an undone one).
    pub fn add_receipt(&mut self, r: Receipt) {
        if let Some(seq) = r.undoes {
            for e in &mut self.entries {
                if let Some(t) = e.tool.as_mut().filter(|t| t.seq == Some(seq)) {
                    t.undoable = false;
                    t.result.push_str(&format!("  · undone by #{}", r.seq));
                }
            }
        }
        self.receipts.insert(0, r);
        self.receipts.truncate(RAIL_RECEIPTS);
    }

    /// Whether receipt `seq` has been undone (as far as the rail knows).
    pub fn is_undone(&self, seq: u64) -> bool {
        self.receipts
            .iter()
            .any(|r| r.undoes == Some(seq) && r.outcome.status == Status::Ok)
    }

    /// Add a chat entry.
    pub fn push(&mut self, who: Speaker, text: impl Into<String>) {
        self.streaming = false;
        self.entries.push(Entry {
            who,
            text: text.into(),
            at: Local::now(),
            transient: false,
            tool: None,
        });
        self.scroll = 0;
    }

    /// A setup note, dropped by [`View::drop_transient`].
    pub fn push_transient(&mut self, text: impl Into<String>) {
        self.push(Speaker::System, text);
        if let Some(e) = self.entries.last_mut() {
            e.transient = true;
        }
    }

    /// Forget setup notes (Reeve is connected now).
    pub fn drop_transient(&mut self) {
        self.entries.retain(|e| !e.transient);
    }

    /// Fold in a new reading.
    pub fn sample(&mut self, snap: Snapshot) {
        if let Some(c) = snap.cpu_pct {
            push_hist(&mut self.cpu_hist, c);
        }
        if snap.mem_total > 0 {
            push_hist(
                &mut self.mem_hist,
                snap.mem_used as f32 / snap.mem_total as f32,
            );
        }
        self.snap = snap;
    }

    /// Fold in what the agent said.
    pub fn apply(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::TurnStarted => {
                self.busy = true;
                self.streaming = false;
                self.thinking.clear();
            }
            AgentEvent::Text(t) => {
                match self.entries.last_mut() {
                    Some(e) if self.streaming => e.text.push_str(&t),
                    _ => {
                        self.push(Speaker::Reeve, t);
                        self.streaming = true;
                    }
                }
                self.thinking.clear();
            }
            AgentEvent::Reasoning(r) => self.thinking.push_str(&r),
            AgentEvent::Spend {
                usd,
                usage,
                session,
                totals,
            } => {
                self.last = Some(LastCall { usd, usage });
                self.session = session;
                self.totals = *totals;
            }
            AgentEvent::TurnDone { truncated } => {
                self.busy = false;
                self.thinking.clear();
                self.approval = None;
                if truncated {
                    self.push(
                        Speaker::System,
                        "The reply hit the model's output limit and was cut short.",
                    );
                }
                self.close_reply();
            }
            AgentEvent::Error(e) => {
                self.busy = false;
                self.thinking.clear();
                self.approval = None;
                self.settle_running("stopped");
                self.close_reply();
                self.push(Speaker::Error, e);
            }
            AgentEvent::Models(_) => {}
            AgentEvent::Receipt(r) => self.add_receipt(*r),
            AgentEvent::ToolStarted {
                id,
                tool,
                tier,
                summary,
            } => {
                self.push(Speaker::Tool, String::new());
                if let Some(e) = self.entries.last_mut() {
                    e.tool = Some(ToolView {
                        id,
                        tool,
                        tier,
                        summary,
                        status: None,
                        result: String::new(),
                        diff: None,
                        seq: None,
                        undoable: false,
                        approved_by: None,
                    });
                }
                self.thinking.clear();
            }
            AgentEvent::ToolFinished {
                id,
                status,
                summary,
                diff,
                receipt,
            } => {
                self.approval = None;
                if let Some(t) = self
                    .entries
                    .iter_mut()
                    .rev()
                    .filter_map(|e| e.tool.as_mut())
                    .find(|t| t.id == id && t.status.is_none())
                {
                    t.status = Some(status);
                    t.result = summary;
                    t.diff = diff;
                    if let Some(r) = &receipt {
                        t.seq = Some(r.seq);
                        t.undoable = r.undo.is_some() && status == Status::Ok;
                        t.approved_by = Some(r.approved_by.clone());
                    }
                }
                if let Some(r) = receipt {
                    self.add_receipt(*r);
                }
            }
        }
    }

    /// Mark tool calls still shown as running (the turn ended under them).
    fn settle_running(&mut self, why: &str) {
        for t in self.entries.iter_mut().filter_map(|e| e.tool.as_mut()) {
            if t.status.is_none() {
                t.status = Some(Status::Error);
                t.result = why.into();
            }
        }
    }

    /// Stop appending to the current Reeve entry: the next text starts a new one.
    fn close_reply(&mut self) {
        if self.streaming
            && self
                .entries
                .last()
                .is_some_and(|e| e.text.trim().is_empty())
        {
            self.entries.pop();
        }
        self.streaming = false;
    }

    /// Insert text at the cursor.
    pub fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
    }

    /// Delete the character before the cursor.
    pub fn backspace(&mut self) {
        if let Some((i, _)) = self.input[..self.cursor].char_indices().next_back() {
            self.input.replace_range(i..self.cursor, "");
            self.cursor = i;
        }
    }

    /// Delete the character under the cursor.
    pub fn delete(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.input
                .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
        }
    }

    /// Move the cursor one character.
    pub fn left(&mut self) {
        if let Some((i, _)) = self.input[..self.cursor].char_indices().next_back() {
            self.cursor = i;
        }
    }

    /// Move the cursor one character.
    pub fn right(&mut self) {
        if let Some(c) = self.input[self.cursor..].chars().next() {
            self.cursor += c.len_utf8();
        }
    }

    /// Delete the word before the cursor.
    pub fn delete_word(&mut self) {
        let before = &self.input[..self.cursor];
        let trimmed = before.trim_end();
        let start = trimmed.rfind(char::is_whitespace).map_or(0, |i| {
            i + trimmed[i..].chars().next().map_or(1, char::len_utf8)
        });
        self.input.replace_range(start..self.cursor, "");
        self.cursor = start;
    }

    /// Take the composer's text for sending, if there is any.
    pub fn take_input(&mut self) -> Option<String> {
        let text = self.input.trim().to_string();
        if text.is_empty() {
            return None;
        }
        self.input.clear();
        self.cursor = 0;
        Some(text)
    }
}

fn push_hist(h: &mut VecDeque<f32>, v: f32) {
    if h.len() == HISTORY {
        h.pop_front();
    }
    h.push_back(v);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streamed_text_joins_one_entry_per_turn() {
        let mut v = View::new(HostInfo::default());
        v.push(Speaker::User, "hi");
        v.apply(AgentEvent::TurnStarted);
        v.apply(AgentEvent::Reasoning("hmm".into()));
        assert_eq!(v.thinking, "hmm");
        v.apply(AgentEvent::Text("Hel".into()));
        v.apply(AgentEvent::Text("lo".into()));
        v.apply(AgentEvent::TurnDone { truncated: false });
        assert_eq!(v.entries.len(), 2);
        assert_eq!(v.entries[1].text, "Hello");
        assert!(!v.busy && v.thinking.is_empty());
        // The next turn's text is a new entry.
        v.apply(AgentEvent::TurnStarted);
        v.apply(AgentEvent::Text("again".into()));
        assert_eq!(v.entries.len(), 3);
    }

    #[test]
    fn composer_edits_by_character() {
        let mut v = View::new(HostInfo::default());
        v.insert("héllo wörld");
        v.backspace();
        v.left();
        v.left();
        v.delete();
        assert_eq!(v.input, "héllo wöl");
        v.cursor = v.input.len();
        v.delete_word();
        assert_eq!(v.input, "héllo ");
        assert_eq!(v.take_input().as_deref(), Some("héllo"));
        assert_eq!(v.take_input(), None);
    }

    #[test]
    fn an_error_ends_the_turn() {
        let mut v = View::new(HostInfo::default());
        v.apply(AgentEvent::TurnStarted);
        v.apply(AgentEvent::Error("budget: session cap reached".into()));
        assert!(!v.busy);
        assert_eq!(v.entries.last().unwrap().who, Speaker::Error);
    }
}
