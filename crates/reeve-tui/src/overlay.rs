//! Floating panels (`/providers`, the model picker, key entry, adding a
//! connection, help) and the slash-command palette. Pure state and key
//! handling: side effects come back as an [`Action`] for `run.rs` to perform.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use reeve_core::config::{ConnectionConfig, connection_template};
use reeve_core::llm::ModelInfo;
use reeve_core::settings::valid_connection_name;

/// A slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Command {
    /// `/providers`.
    pub name: &'static str,
    /// One line on what it does.
    pub about: &'static str,
}

/// Every slash command, in palette order.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "/providers",
        about: "connections and API keys",
    },
    Command {
        name: "/model",
        about: "choose the model (live prices)",
    },
    Command {
        name: "/orders",
        about: "standing orders: what Reeve may do unattended",
    },
    Command {
        name: "/findings",
        about: "what the observer noticed; drafted fixes",
    },
    Command {
        name: "/observer",
        about: "reeved service and the drafter's budget",
    },
    Command {
        name: "/spend",
        about: "what it cost: day, week, month, by session and role",
    },
    Command {
        name: "/system",
        about: "vitals over time, disks, what changed",
    },
    Command {
        name: "/report",
        about: "the state of the machine as a page: charts, drift, findings",
    },
    Command {
        name: "/update",
        about: "is a newer Reeve out, and how to get it",
    },
    Command {
        name: "/privacy",
        about: "what the model sees; OpenRouter routing",
    },
    Command {
        name: "/memory",
        about: "what Reeve knows: facts, runbooks, preferences",
    },
    Command {
        name: "/reflect",
        about: "learn from this session now",
    },
    Command {
        name: "/receipts",
        about: "everything Reeve did; undo and verify",
    },
    Command {
        name: "/new",
        about: "start a fresh session",
    },
    Command {
        name: "/yolo",
        about: "toggle auto-approve (the floor still asks)",
    },
    Command {
        name: "/help",
        about: "keys and commands",
    },
    Command {
        name: "/quit",
        about: "leave Reeve",
    },
];

/// Commands matching what's typed, while the composer holds a bare `/word`.
pub fn palette(input: &str, skills: &[(String, String)]) -> Vec<PaletteItem> {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return Vec::new();
    }
    let q = input.to_ascii_lowercase();
    let mut out: Vec<PaletteItem> = COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(&q))
        .map(|c| PaletteItem {
            name: c.name.to_string(),
            about: c.about.to_string(),
            skill: false,
        })
        .collect();
    // A skill named like a command stays reachable from ⌃K and F8.
    out.extend(
        skills
            .iter()
            .filter(|(id, _)| {
                format!("/{id}").starts_with(&q) && !COMMANDS.iter().any(|c| c.name[1..] == **id)
            })
            .map(|(id, about)| PaletteItem {
                name: format!("/{id}"),
                about: about.clone(),
                skill: true,
            }),
    );
    out
}

/// A row of the slash palette: a command, or one of the owner's skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteItem {
    /// `/name`.
    pub name: String,
    /// What it does.
    pub about: String,
    /// One of the owner's skills (Reeve runs it), not a command.
    pub skill: bool,
}

/// What to send Reeve to run a skill, with anything typed after its name.
pub fn skill_request(id: &str, rest: &str) -> String {
    let rest = rest.trim();
    if rest.is_empty() {
        format!("Use the skill {id}.")
    } else {
        format!("Use the skill {id}: {rest}")
    }
}

/// What an overlay asks `run.rs` to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Nothing to do.
    None,
    /// Close the top panel.
    Close,
    /// Open a panel on top.
    Push(Overlay),
    /// Store a key; with `then_use`, switch to the connection afterwards.
    SaveKey {
        /// Connection.
        connection: String,
        /// The key. Never logged or drawn.
        secret: String,
        /// Switch to it once saved.
        then_use: bool,
    },
    /// Delete a stored key.
    ForgetKey(String),
    /// Ask the provider whether the key works.
    Verify(String),
    /// Switch to a connection.
    Use(String),
    /// Open the model picker for a connection.
    OpenModels(String),
    /// Use this model (and its connection).
    ChooseModel {
        /// Connection.
        connection: String,
        /// Model id.
        model: String,
    },
    /// Undo the action on this receipt.
    Undo(u64),
    /// Answer sudo's password request (`None` refuses).
    Password(Option<(String, bool)>),
    /// Check the whole receipt chain.
    VerifyReceipts,
    /// Put a new or pending memory in effect.
    MemoryAccept(reeve_core::memory::Layer, String),
    /// Retire (or restore) a memory.
    MemoryRetire(reeve_core::memory::Layer, String),
    /// Delete a memory's file.
    MemoryDelete(reeve_core::memory::Layer, String),
    /// Open a memory in `$EDITOR`.
    MemoryEdit(reeve_core::memory::Layer, String),
    /// Re-run the survey.
    Survey,
    /// Ask Reeve to investigate a finding, in the chat.
    Diagnose(String),
    /// Carry out a finding's drafted proposal, in the chat.
    UseProposal(String),
    /// Set a finding's status.
    FindingStatus(String, reeve_core::findings::FindingStatus),
    /// Turn a standing order on or off.
    OrderToggle(String, bool),
    /// Ask reeved to run an order now.
    OrderRun(String),
    /// Open an order's file in `$EDITOR`.
    OrderEdit(String),
    /// Open the order form: a new order, or this one to edit.
    OrderForm(Option<String>),
    /// Save the form's order (a new one when `editing` is `None`).
    SaveOrder {
        /// The id of the order edited.
        editing: Option<String>,
        /// What the form says.
        order: Box<reeve_core::orders::Order>,
    },
    /// Delete an order's file.
    OrderDelete(String),
    /// Show an order's sudoers lines.
    OrderSudoers(String),
    /// Install and start reeved.
    InstallDaemon,
    /// Stop and remove reeved.
    UninstallDaemon,
    /// Save the drafter's settings.
    SaveDrafter(reeve_core::config::DrafterConfig),
    /// Pick the drafter's model.
    DrafterModel(String),
    /// Use this model for the drafter.
    ChooseDrafterModel {
        /// Connection.
        connection: String,
        /// Model id.
        model: String,
    },
    /// Save privacy settings.
    SavePrivacy(reeve_core::config::PrivacyConfig),
    /// Reflect on this session now.
    Reflect,
    /// Open the conversation.
    Chat,
    /// Open a tile.
    Open(crate::view::Tile),
    /// Open a tile and select this item there (finding, order, or memory id).
    OpenSelect(crate::view::Tile, String),
    /// Send this to Reeve, in the chat.
    Ask(String),
    /// Run a slash command.
    Command(&'static str),
    /// Open a skill's file in `$EDITOR`.
    SkillEdit(String),
    /// Delete a skill (with a receipt, so it can be undone).
    SkillDelete(String),
    /// Start a new skill: describe it to Reeve, who writes it for a yes.
    SkillNew,
    /// Open the receipts panel on this receipt.
    ShowReceipt(u64),
    /// Gather health and what changed for this many days.
    SystemReport(u32),
    /// Open the full report page in the browser.
    OpenReport,
    /// Write the statement shown to a CSV file.
    ExportSpend,
    /// Save a new connection.
    AddConnection {
        /// Name (also the key's file name).
        name: String,
        /// Its settings.
        conn: ConnectionConfig,
    },
}

/// A floating panel.
#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    /// `/providers`.
    Providers(Providers),
    /// Masked key entry.
    Key(KeyEntry),
    /// `/model`.
    Models(ModelPicker),
    /// New connection form.
    Add(AddForm),
    /// `/receipts`.
    Receipts(ReceiptsPanel),
    /// sudo's password.
    Password(PasswordEntry),
    /// `/memory`.
    Memory(MemoryPanel),
    /// `/findings`.
    Findings(FindingsPanel),
    /// `/observer`.
    Observer(ObserverPanel),
    /// `/orders`.
    Orders(OrdersPanel),
    /// The spend tab.
    Spend(SpendPanel),
    /// The system tab.
    System(SystemPanel),
    /// ⌃K: search everything.
    Everything(EverythingPanel),
    /// F1: what's waiting on you.
    Needs(NeedsPanel),
    /// The standing order form.
    OrderForm(Box<crate::orderform::OrderForm>),
    /// `/privacy`.
    Privacy(PrivacyPanel),
    /// `/help`.
    Help,
}

impl Overlay {
    /// Handle a key.
    pub fn on_key(&mut self, k: KeyEvent) -> Action {
        // The form decides what esc means (it asks before dropping changes).
        if let Self::OrderForm(f) = self {
            return f.on_key(k);
        }
        if k.code == KeyCode::Esc {
            return Action::Close;
        }
        match self {
            Self::Providers(p) => p.on_key(k),
            Self::Key(e) => e.on_key(k),
            Self::Models(m) => m.on_key(k),
            Self::Add(a) => a.on_key(k),
            Self::Receipts(r) => r.on_key(k),
            Self::Password(p) => p.on_key(k),
            Self::Memory(m) => m.on_key(k),
            Self::Findings(f) => f.on_key(k),
            Self::Observer(o) => o.on_key(k),
            Self::Orders(o) => o.on_key(k),
            Self::Spend(s) => s.on_key(k),
            Self::System(s) => s.on_key(k),
            Self::Everything(e) => e.on_key(k),
            Self::Needs(n) => n.on_key(k),
            Self::OrderForm(f) => f.on_key(k),
            Self::Privacy(p) => p.on_key(k),
            Self::Help => Action::Close,
        }
    }

    /// The tile this panel is, when it's one of the board's screens.
    pub fn tile(&self) -> Option<crate::view::Tile> {
        use crate::view::Tile;
        match self {
            Self::Needs(_) => Some(Tile::Needs),
            Self::System(p) if p.changed => Some(Tile::Changed),
            Self::System(_) => Some(Tile::Health),
            Self::Findings(_) => Some(Tile::Findings),
            Self::Receipts(_) => Some(Tile::Activity),
            Self::Spend(_) => Some(Tile::Spend),
            Self::Orders(_) => Some(Tile::Orders),
            Self::Memory(_) => Some(Tile::Memory),
            _ => None,
        }
    }

    /// Handle pasted text.
    pub fn on_paste(&mut self, s: &str) {
        let line = s.trim();
        match self {
            Self::Everything(e) => {
                e.query.push_str(line);
                e.sel = 0;
            }
            Self::Key(e) => e.secret.push_str(line),
            Self::OrderForm(f) => f.paste(s),
            Self::Password(p) => p.secret.push_str(line),
            Self::Models(m) => {
                m.query.push_str(line);
                m.sel = 0;
            }
            Self::Add(a) => {
                if let Some(f) = a.text_field() {
                    f.push_str(line);
                }
            }
            _ => {}
        }
    }
}

// ── /providers ──────────────────────────────────────────────────────────────

/// One connection as the panel shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRow {
    /// Name.
    pub name: String,
    /// `openrouter`, `openai`, `local`.
    pub kind: String,
    /// Base URL.
    pub base_url: String,
    /// Where its key comes from, or `None`.
    pub key: Option<String>,
    /// In use now.
    pub active: bool,
    /// Its model.
    pub model: Option<String>,
    /// Last verify result.
    pub status: Option<Result<String, String>>,
}

/// The `/providers` panel. The row after the last connection is "add".
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Providers {
    /// Connections.
    pub rows: Vec<ProviderRow>,
    /// Selected row.
    pub sel: usize,
}

impl Providers {
    /// The selected connection, unless "add" is selected.
    pub fn selected(&self) -> Option<&ProviderRow> {
        self.rows.get(self.sel)
    }

    /// Keep the selection and verify results across a refresh.
    pub fn refresh(&mut self, rows: Vec<ProviderRow>) {
        let keep = self.selected().map(|r| r.name.clone());
        let old = std::mem::take(&mut self.rows);
        self.rows = rows;
        for r in &mut self.rows {
            if let Some(o) = old.iter().find(|o| o.name == r.name) {
                r.status.clone_from(&o.status);
            }
        }
        if let Some(name) = keep {
            if let Some(i) = self.rows.iter().position(|r| r.name == name) {
                self.sel = i;
            }
        }
        self.sel = self.sel.min(self.rows.len());
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let last = self.rows.len();
        match k.code {
            KeyCode::Up => self.sel = self.sel.checked_sub(1).unwrap_or(last),
            KeyCode::Down | KeyCode::Tab => {
                self.sel = if self.sel >= last { 0 } else { self.sel + 1 }
            }
            KeyCode::Enter if self.sel == last => {
                return Action::Push(Overlay::Add(AddForm::new()));
            }
            KeyCode::Char('a') => return Action::Push(Overlay::Add(AddForm::new())),
            _ => {}
        }
        let Some(row) = self.selected() else {
            return Action::None;
        };
        let name = row.name.clone();
        let needs_key = row.key.is_none();
        match k.code {
            KeyCode::Enter if needs_key => Action::Push(Overlay::Key(KeyEntry::new(&name, true))),
            KeyCode::Enter => Action::Use(name),
            KeyCode::Char('s' | 'k') => Action::Push(Overlay::Key(KeyEntry::new(&name, false))),
            KeyCode::Char('m') => Action::OpenModels(name),
            KeyCode::Char('v') if !needs_key => Action::Verify(name),
            KeyCode::Char('x') if !needs_key => Action::ForgetKey(name),
            _ => Action::None,
        }
    }

    /// Record a verify result.
    pub fn set_status(&mut self, connection: &str, status: Result<String, String>) {
        if let Some(r) = self.rows.iter_mut().find(|r| r.name == connection) {
            r.status = Some(status);
        }
    }
}

// ── /receipts ───────────────────────────────────────────────────────────────

/// Everything Reeve did (F4): the receipts, newest first.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ReceiptsPanel {
    /// Receipts, newest first.
    pub items: Vec<reeve_core::receipts::Receipt>,
    /// Receipts that have been undone (by seq).
    pub undone: std::collections::HashSet<u64>,
    /// Selected row.
    pub sel: usize,
    /// The last verify or undo result.
    pub note: Option<Result<String, String>>,
}

impl ReceiptsPanel {
    /// The panel over `items` (newest first).
    pub fn new(items: Vec<reeve_core::receipts::Receipt>) -> Self {
        let mut p = Self::default();
        p.load(items);
        p
    }

    /// Show `items`, and work out which of them have been undone.
    pub fn load(&mut self, items: Vec<reeve_core::receipts::Receipt>) {
        self.undone = reeve_core::receipts::undone(&items);
        self.items = items;
    }

    /// The selected receipt.
    pub fn selected(&self) -> Option<&reeve_core::receipts::Receipt> {
        self.items.get(self.sel)
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = self.items.len();
        match k.code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Char('u') => {
                if let Some(r) = self.selected() {
                    if r.undo.is_some() && !self.undone.contains(&r.seq) {
                        return Action::Undo(r.seq);
                    }
                    self.note = Some(Err(if self.undone.contains(&r.seq) {
                        format!("#{} was already undone", r.seq)
                    } else {
                        format!("#{} has nothing to undo", r.seq)
                    }));
                }
            }
            KeyCode::Char('v') => return Action::VerifyReceipts,
            _ => {}
        }
        Action::None
    }
}

// ── /findings ───────────────────────────────────────────────────────────────

/// The findings inbox.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FindingsPanel {
    /// Findings, live first.
    pub items: Vec<reeve_core::findings::Finding>,
    /// Selected row.
    pub sel: usize,
    /// Detail scroll.
    pub scroll: usize,
}

impl FindingsPanel {
    /// The selected finding.
    pub fn selected(&self) -> Option<&reeve_core::findings::Finding> {
        self.items.get(self.sel)
    }

    /// New list, same selection.
    pub fn refresh(&mut self, items: Vec<reeve_core::findings::Finding>) {
        let keep = self.selected().map(|f| f.id.clone());
        self.items = items;
        if let Some(id) = keep {
            if let Some(i) = self.items.iter().position(|f| f.id == id) {
                self.sel = i;
            }
        }
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        use reeve_core::findings::FindingStatus as S;
        let n = self.items.len();
        match k.code {
            KeyCode::Up => {
                self.sel = self.sel.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::PageDown => self.scroll += 5,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            _ => {}
        }
        let Some(f) = self.selected() else {
            return Action::None;
        };
        let id = f.id.clone();
        match k.code {
            KeyCode::Char('d') | KeyCode::Enter => Action::Diagnose(id),
            KeyCode::Char('p') if f.proposal.is_some() => Action::UseProposal(id),
            KeyCode::Char('a') => Action::FindingStatus(id, S::Acknowledged),
            KeyCode::Char('x') => Action::FindingStatus(id, S::Dismissed),
            KeyCode::Char('o') => Action::FindingStatus(id, S::Open),
            _ => Action::None,
        }
    }
}

// ── /orders ─────────────────────────────────────────────────────────────────

/// The standing orders panel.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct OrdersPanel {
    /// Orders that parse.
    pub items: Vec<reeve_core::orders::Order>,
    /// Files that don't: (id, why).
    pub bad: Vec<(String, String)>,
    /// Run history.
    pub states: std::collections::BTreeMap<String, reeve_core::orders::OrderState>,
    /// Selected row.
    pub sel: usize,
    /// Last action's result.
    pub note: Option<Result<String, String>>,
    /// Extra lines to show (sudoers rules).
    pub extra: Vec<String>,
    /// Delete asked once.
    pub confirm_delete: Option<String>,
    /// The receipt of the last change made here, for `u`.
    pub last: Option<u64>,
}

impl OrdersPanel {
    /// Load from disk.
    pub fn load(orders: &reeve_core::orders::Orders) -> Self {
        let mut p = Self::default();
        p.reload(orders);
        p
    }

    /// Re-read, keeping the selection.
    pub fn reload(&mut self, orders: &reeve_core::orders::Orders) {
        let keep = self.selected().map(|o| o.id.clone());
        let (items, bad) = orders.load();
        self.items = items;
        self.bad = bad;
        self.states = orders.states();
        if let Some(id) = keep {
            if let Some(i) = self.items.iter().position(|o| o.id == id) {
                self.sel = i;
            }
        }
        self.sel = self.sel.min(self.items.len().saturating_sub(1));
    }

    /// The selected order.
    pub fn selected(&self) -> Option<&reeve_core::orders::Order> {
        self.items.get(self.sel)
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = self.items.len();
        if !matches!(k.code, KeyCode::Char('D')) {
            self.confirm_delete = None;
        }
        match k.code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('n') => return Action::OrderForm(None),
            KeyCode::Char('u') => {
                return match self.last {
                    Some(seq) => {
                        self.note = Some(Ok(format!("undoing #{seq}…")));
                        Action::Undo(seq)
                    }
                    None => {
                        self.note = Some(Err(
                            "nothing to undo here; F4 activity has every change".into()
                        ));
                        Action::None
                    }
                };
            }
            _ => {}
        }
        if !matches!(k.code, KeyCode::Char('s')) {
            self.extra.clear();
        }
        let Some(o) = self.selected() else {
            return Action::None;
        };
        let id = o.id.clone();
        match k.code {
            KeyCode::Char(' ') => Action::OrderToggle(id, !o.enabled),
            KeyCode::Char('r') => Action::OrderRun(id),
            KeyCode::Char('e') | KeyCode::Enter => Action::OrderForm(Some(id)),
            KeyCode::Char('E') => Action::OrderEdit(id),
            KeyCode::Char('s') => Action::OrderSudoers(id),
            KeyCode::Char('D') => {
                if self.confirm_delete.as_deref() == Some(id.as_str()) {
                    self.confirm_delete = None;
                    Action::OrderDelete(id)
                } else {
                    self.note = Some(Err(format!(
                        "press D again to delete {id} (u puts it back; space only turns it off)"
                    )));
                    self.confirm_delete = Some(id);
                    Action::None
                }
            }
            _ => Action::None,
        }
    }
}

// ── /observer ───────────────────────────────────────────────────────────────

/// Drafter fields, in order.
pub const DRAFTER_FIELDS: &[&str] = &[
    "drafter",
    "connection",
    "model",
    "daily budget",
    "per draft",
    "drafts/day",
    "severity",
];

/// The observer panel: service state and the drafter's settings.
#[derive(Debug, Clone, PartialEq)]
pub struct ObserverPanel {
    /// `active`, `inactive`, `not installed`…
    pub service: String,
    /// The last heartbeat, if any.
    pub status: Option<reeve_core::findings::ObserverStatus>,
    /// Drafter settings being edited.
    pub drafter: reeve_core::config::DrafterConfig,
    /// Connections to choose from.
    pub connections: Vec<String>,
    /// The main model (the drafter's default).
    pub main_model: String,
    /// Selected field.
    pub field: usize,
    /// Last result.
    pub note: Option<Result<String, String>>,
}

impl ObserverPanel {
    fn on_key(&mut self, k: KeyEvent) -> Action {
        let d = &mut self.drafter;
        let step = |v: &mut f64, by: f64, up: bool| {
            *v = if up { *v + by } else { (*v - by).max(0.0) };
            *v = (*v * 100.0).round() / 100.0;
        };
        let changed = match k.code {
            KeyCode::Up => {
                self.field = self.field.saturating_sub(1);
                false
            }
            KeyCode::Down | KeyCode::Tab => {
                self.field = (self.field + 1).min(DRAFTER_FIELDS.len() - 1);
                false
            }
            KeyCode::Char('i') => return Action::InstallDaemon,
            KeyCode::Char('u') => return Action::UninstallDaemon,
            KeyCode::Enter | KeyCode::Char(' ') if self.field == 0 => {
                d.enabled = !d.enabled;
                true
            }
            KeyCode::Enter | KeyCode::Char('m') if self.field == 2 => {
                let conn = d.connection.clone().unwrap_or_default();
                return Action::DrafterModel(conn);
            }
            KeyCode::Left | KeyCode::Right => {
                let up = k.code == KeyCode::Right;
                match self.field {
                    0 => d.enabled = !d.enabled,
                    1 if !self.connections.is_empty() => {
                        let cur = d
                            .connection
                            .as_ref()
                            .and_then(|c| self.connections.iter().position(|x| x == c));
                        let n = self.connections.len();
                        let next = match (cur, up) {
                            (None, true) => 0,
                            (None, false) => n - 1,
                            (Some(i), true) => (i + 1) % n,
                            (Some(i), false) => (i + n - 1) % n,
                        };
                        d.connection = Some(self.connections[next].clone());
                        d.model = None;
                    }
                    3 => step(&mut d.daily_usd, 0.05, up),
                    4 => step(&mut d.per_draft_usd, 0.01, up),
                    5 => {
                        d.max_drafts_per_day = if up {
                            d.max_drafts_per_day + 1
                        } else {
                            d.max_drafts_per_day.saturating_sub(1)
                        }
                    }
                    6 => {
                        let order = ["info", "warning", "critical"];
                        let i = order.iter().position(|x| *x == d.min_severity).unwrap_or(1);
                        d.min_severity = order[if up {
                            (i + 1).min(2)
                        } else {
                            i.saturating_sub(1)
                        }]
                        .into();
                    }
                    _ => return Action::None,
                }
                true
            }
            _ => false,
        };
        if changed {
            Action::SaveDrafter(self.drafter.clone())
        } else {
            Action::None
        }
    }
}

// ── /memory ─────────────────────────────────────────────────────────────────

/// The memory browser: one tab per layer.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MemoryPanel {
    /// Index into [`reeve_core::memory::Layer::ALL`].
    pub tab: usize,
    /// Notes of every layer, by tab.
    pub notes: Vec<Vec<reeve_core::memory::Note>>,
    /// Selected row in the current tab.
    pub sel: usize,
    /// Last action's result.
    pub note: Option<Result<String, String>>,
    /// Delete asked once; asking again deletes.
    pub confirm_delete: Option<String>,
    /// The owner's skills: the tab after the layers.
    pub skills: Vec<reeve_core::skills::Skill>,
    /// Skill files that don't parse, and why.
    pub skill_errors: Vec<(String, String)>,
}

impl MemoryPanel {
    /// The skills tab: after the four layers.
    pub const SKILLS_TAB: usize = reeve_core::memory::Layer::ALL.len();

    /// Load every layer, and the skills.
    pub fn load(mem: &reeve_core::memory::Memory, skills: &reeve_core::skills::Skills) -> Self {
        let mut p = Self::default();
        p.reload(mem, skills);
        p
    }

    /// On the skills tab.
    pub fn on_skills(&self) -> bool {
        self.tab == Self::SKILLS_TAB
    }

    /// The selected skill, on the skills tab.
    pub fn selected_skill(&self) -> Option<&reeve_core::skills::Skill> {
        self.on_skills()
            .then(|| self.skills.get(self.sel))
            .flatten()
    }

    /// Show this skill, on the skills tab.
    pub fn select_skill(&mut self, id: &str) {
        self.tab = Self::SKILLS_TAB;
        self.sel = self.skills.iter().position(|s| s.id == id).unwrap_or(0);
    }

    /// Re-read from disk, keeping the tab and (where possible) the selection.
    /// Show the note with this id, on its layer's tab.
    pub fn select(&mut self, id: &str) {
        for (tab, notes) in self.notes.iter().enumerate() {
            if let Some(i) = notes.iter().position(|n| n.id == id) {
                self.tab = tab;
                self.sel = i;
                return;
            }
        }
    }

    /// Reload every layer and the skills, keeping the selection.
    pub fn reload(
        &mut self,
        mem: &reeve_core::memory::Memory,
        skills: &reeve_core::skills::Skills,
    ) {
        let keep = self.selected().map(|n| n.id.clone());
        let keep_skill = self.selected_skill().map(|s| s.id.clone());
        self.notes = reeve_core::memory::Layer::ALL
            .iter()
            .map(|l| mem.list(*l))
            .collect();
        (self.skills, self.skill_errors) = skills.load();
        if self.on_skills() {
            if let Some(i) = keep_skill.and_then(|id| self.skills.iter().position(|s| s.id == id)) {
                self.sel = i;
            }
            self.sel = self.sel.min(self.skills.len().saturating_sub(1));
            return;
        }
        if let Some(id) = keep {
            if let Some(i) = self.current().iter().position(|n| n.id == id) {
                self.sel = i;
            }
        }
        self.sel = self.sel.min(self.current().len().saturating_sub(1));
    }

    /// Notes on the current tab.
    pub fn current(&self) -> &[reeve_core::memory::Note] {
        self.notes.get(self.tab).map_or(&[], Vec::as_slice)
    }

    /// The selected note.
    pub fn selected(&self) -> Option<&reeve_core::memory::Note> {
        self.current().get(self.sel)
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = if self.on_skills() {
            self.skills.len()
        } else {
            self.current().len()
        };
        // The four layers, then skills.
        let tabs = Self::SKILLS_TAB + 1;
        if !matches!(k.code, KeyCode::Char('D')) {
            self.confirm_delete = None;
        }
        if self.on_skills() {
            match k.code {
                KeyCode::Char('n') => return Action::SkillNew,
                KeyCode::Char('s' | 'r' | 'a' | 'x') => return Action::None,
                _ => {}
            }
            if let Some(id) = self.selected_skill().map(|s| s.id.clone()) {
                match k.code {
                    KeyCode::Enter => return Action::Ask(skill_request(&id, "")),
                    KeyCode::Char('e') => return Action::SkillEdit(id),
                    KeyCode::Char('D') => {
                        if self.confirm_delete.as_deref() == Some(id.as_str()) {
                            self.confirm_delete = None;
                            return Action::SkillDelete(id);
                        }
                        self.note = Some(Err(format!(
                            "press D again to delete the skill {id} (F4 activity can undo it)"
                        )));
                        self.confirm_delete = Some(id);
                        return Action::None;
                    }
                    _ => {}
                }
            }
        }
        match k.code {
            KeyCode::Left => {
                self.tab = (self.tab + tabs - 1) % tabs;
                self.sel = 0;
            }
            KeyCode::Right => {
                self.tab = (self.tab + 1) % tabs;
                self.sel = 0;
            }
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('s') => return Action::Survey,
            KeyCode::Char('r') => return Action::Reflect,
            _ => {}
        }
        let Some(note) = self.selected() else {
            return Action::None;
        };
        let (layer, id) = (note.layer, note.id.clone());
        match k.code {
            KeyCode::Char('a') | KeyCode::Enter => Action::MemoryAccept(layer, id),
            KeyCode::Char('x') => Action::MemoryRetire(layer, id),
            KeyCode::Char('e') => Action::MemoryEdit(layer, id),
            KeyCode::Char('D') => {
                if self.confirm_delete.as_deref() == Some(id.as_str()) {
                    self.confirm_delete = None;
                    Action::MemoryDelete(layer, id)
                } else {
                    self.note = Some(Err(format!(
                        "press D again to delete {id} for good (x retires it instead)"
                    )));
                    self.confirm_delete = Some(id);
                    Action::None
                }
            }
            _ => Action::None,
        }
    }
}

// ── sudo password ───────────────────────────────────────────────────────────

/// sudo's password, masked. Never drawn, logged, or sent to the model.
#[derive(Clone, PartialEq)]
pub struct PasswordEntry {
    /// What sudo said (`[sudo] password for …:`).
    pub prompt: String,
    /// The root action it's for.
    pub action: String,
    /// Typed so far.
    pub secret: String,
    /// Keep it in memory for a few minutes.
    pub remember: bool,
}

impl std::fmt::Debug for PasswordEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasswordEntry")
            .field("prompt", &self.prompt)
            .field(
                "secret",
                &format_args!("<{} chars>", self.secret.chars().count()),
            )
            .finish()
    }
}

impl PasswordEntry {
    fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter => {
                Action::Password(Some((std::mem::take(&mut self.secret), self.remember)))
            }
            KeyCode::Tab => {
                self.remember = !self.remember;
                Action::None
            }
            KeyCode::Char('u') if ctrl => {
                self.secret.clear();
                Action::None
            }
            KeyCode::Backspace => {
                self.secret.pop();
                Action::None
            }
            KeyCode::Char(c) if !ctrl => {
                self.secret.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }
}

// ── key entry ───────────────────────────────────────────────────────────────

/// Masked key entry. The secret is never drawn; only its length is.
#[derive(Clone, PartialEq)]
pub struct KeyEntry {
    /// Connection.
    pub connection: String,
    /// Typed so far.
    pub secret: String,
    /// Switch to the connection once saved.
    pub then_use: bool,
}

impl std::fmt::Debug for KeyEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyEntry")
            .field("connection", &self.connection)
            .field(
                "secret",
                &format_args!("<{} chars>", self.secret.chars().count()),
            )
            .finish()
    }
}

impl KeyEntry {
    /// Empty entry for `connection`.
    pub fn new(connection: &str, then_use: bool) -> Self {
        Self {
            connection: connection.into(),
            secret: String::new(),
            then_use,
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Enter if !self.secret.trim().is_empty() => Action::SaveKey {
                connection: self.connection.clone(),
                secret: std::mem::take(&mut self.secret),
                then_use: self.then_use,
            },
            KeyCode::Char('u') if ctrl => {
                self.secret.clear();
                Action::None
            }
            KeyCode::Backspace => {
                self.secret.pop();
                Action::None
            }
            KeyCode::Char(c) if !ctrl => {
                self.secret.push(c);
                Action::None
            }
            _ => Action::None,
        }
    }
}

// ── model picker ────────────────────────────────────────────────────────────

/// `/model`: a searchable list with live prices.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModelPicker {
    /// Connection whose models these are.
    pub connection: String,
    /// All models.
    pub models: Vec<ModelInfo>,
    /// Search text; every word must match.
    pub query: String,
    /// Selected index into the filtered list.
    pub sel: usize,
    /// Still fetching.
    pub loading: bool,
    /// The fetch failed.
    pub error: Option<String>,
    /// The model in use on this connection.
    pub current: Option<String>,
    /// Choosing for the drafter, not the main agent.
    pub for_drafter: bool,
    /// Also list models Reeve can't drive (no tools, images, embeddings…).
    pub show_all: bool,
}

impl ModelPicker {
    /// A picker waiting for its list.
    pub fn loading(
        connection: &str,
        current: Option<String>,
        cached: Option<Vec<ModelInfo>>,
    ) -> Self {
        let mut p = Self {
            connection: connection.into(),
            loading: true,
            current,
            ..Self::default()
        };
        if let Some(models) = cached {
            p.set_models(models);
            p.loading = true;
        }
        p
    }

    /// The list arrived; select the current model.
    pub fn set_models(&mut self, mut models: Vec<ModelInfo>) {
        models.sort_by(|a, b| a.id.cmp(&b.id));
        self.models = models;
        self.loading = false;
        self.error = None;
        if self.query.is_empty() {
            if let Some(cur) = self.current.clone() {
                if let Some(i) = self.filtered().iter().position(|m| m.id == cur) {
                    self.sel = i;
                }
            }
        }
    }

    /// Models hidden because Reeve can't drive them.
    pub fn hidden(&self) -> usize {
        if self.show_all {
            return 0;
        }
        self.models.iter().filter(|m| !self.shown(m)).count()
    }

    /// Usable, the one in use, or everything with `show_all`.
    fn shown(&self, m: &ModelInfo) -> bool {
        self.show_all || m.unusable().is_none() || self.current.as_deref() == Some(m.id.as_str())
    }

    /// Models matching the query.
    pub fn filtered(&self) -> Vec<&ModelInfo> {
        let words: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_ascii_lowercase)
            .collect();
        self.models
            .iter()
            .filter(|m| self.shown(m))
            .filter(|m| {
                let hay = format!(
                    "{} {}",
                    m.id.to_ascii_lowercase(),
                    m.name.as_deref().unwrap_or("").to_ascii_lowercase()
                );
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .collect()
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = self.filtered().len();
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::PageUp => self.sel = self.sel.saturating_sub(10),
            KeyCode::PageDown => self.sel = (self.sel + 10).min(n.saturating_sub(1)),
            KeyCode::Home => self.sel = 0,
            KeyCode::End => self.sel = n.saturating_sub(1),
            KeyCode::Tab => {
                self.show_all = !self.show_all;
                self.sel = 0;
            }
            KeyCode::Enter => {
                let chosen = self
                    .filtered()
                    .get(self.sel)
                    .map(|m| m.id.clone())
                    // A server with no model list: take the typed id as is.
                    .or_else(|| {
                        let q = self.query.trim();
                        (!q.is_empty() && !q.contains(char::is_whitespace)).then(|| q.to_string())
                    });
                if let Some(model) = chosen {
                    if self.for_drafter {
                        return Action::ChooseDrafterModel {
                            connection: self.connection.clone(),
                            model,
                        };
                    }
                    return Action::ChooseModel {
                        connection: self.connection.clone(),
                        model,
                    };
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.sel = 0;
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.sel = 0;
            }
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.sel = 0;
            }
            _ => {}
        }
        Action::None
    }
}

// ── add a connection ────────────────────────────────────────────────────────

/// Kinds offered in the form, with a label each.
pub const KINDS: &[(&str, &str)] = &[
    ("openai", "OpenAI-compatible"),
    ("openrouter", "OpenRouter"),
    ("local", "local server (no key, $0)"),
];

/// Form fields, in order.
pub const FIELDS: &[&str] = &["name", "kind", "base url", "model"];

/// New connection form.
#[derive(Debug, Clone, PartialEq)]
pub struct AddForm {
    /// Name.
    pub name: String,
    /// Index into [`KINDS`].
    pub kind: usize,
    /// Base URL.
    pub base_url: String,
    /// Default model (optional).
    pub model: String,
    /// Focused field, index into [`FIELDS`].
    pub focus: usize,
    /// Why the last save was refused.
    pub error: Option<String>,
}

impl AddForm {
    /// Blank form (OpenAI-compatible, OpenAI's URL).
    pub fn new() -> Self {
        Self {
            name: String::new(),
            kind: 0,
            base_url: template_url(KINDS[0].0),
            model: String::new(),
            focus: 0,
            error: None,
        }
    }

    fn text_field(&mut self) -> Option<&mut String> {
        match self.focus {
            0 => Some(&mut self.name),
            2 => Some(&mut self.base_url),
            3 => Some(&mut self.model),
            _ => None,
        }
    }

    fn cycle_kind(&mut self, step: isize) {
        let old = template_url(KINDS[self.kind].0);
        let n = KINDS.len() as isize;
        self.kind = ((self.kind as isize + step).rem_euclid(n)) as usize;
        // Follow the template until the URL has been edited.
        if self.base_url == old || self.base_url.is_empty() {
            self.base_url = template_url(KINDS[self.kind].0);
        }
    }

    fn submit(&mut self) -> Action {
        let name = self.name.trim().to_string();
        if !valid_connection_name(&name) {
            self.error = Some("name: letters, digits, - and _ only".into());
            self.focus = 0;
            return Action::None;
        }
        let url = self.base_url.trim().trim_end_matches('/').to_string();
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            self.error = Some("base url must start with http:// or https://".into());
            self.focus = 2;
            return Action::None;
        }
        let Ok(mut conn) = connection_template(KINDS[self.kind].0) else {
            return Action::None;
        };
        conn.base_url = url;
        let model = self.model.trim();
        conn.default_model = (!model.is_empty()).then(|| model.to_string());
        if self.kind != 1 {
            // Only OpenRouter's variable name is a safe guess.
            conn.env_key = None;
        }
        Action::AddConnection { name, conn }
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1) % FIELDS.len(),
            KeyCode::BackTab | KeyCode::Up => {
                self.focus = (self.focus + FIELDS.len() - 1) % FIELDS.len();
            }
            KeyCode::Enter => return self.submit(),
            KeyCode::Left if self.focus == 1 => self.cycle_kind(-1),
            KeyCode::Right | KeyCode::Char(' ') if self.focus == 1 => self.cycle_kind(1),
            KeyCode::Backspace => {
                if let Some(f) = self.text_field() {
                    f.pop();
                }
            }
            KeyCode::Char('u') if ctrl => {
                if let Some(f) = self.text_field() {
                    f.clear();
                }
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(f) = self.text_field() {
                    f.push(c);
                }
            }
            _ => {}
        }
        Action::None
    }
}

impl Default for AddForm {
    fn default() -> Self {
        Self::new()
    }
}

fn template_url(kind: &str) -> String {
    connection_template(kind)
        .map(|c| c.base_url)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_undo_leaves_its_target_undoable() {
        use reeve_core::policy::Tier;
        use reeve_core::receipts::{Receipt, Status};
        use reeve_core::undo::Undo;
        let receipt = |seq: u64, undoes: Option<u64>, status: Status, undo: bool| {
            let mut r = Receipt::draft("s", "pkg_install", serde_json::json!({}), Tier::T2);
            r.seq = seq;
            r.undoes = undoes;
            r.outcome.status = status;
            r.undo = undo.then(|| Undo::Pacman { changes: vec![] });
            r
        };
        let u = |p: &mut ReceiptsPanel, seq: u64| {
            p.sel = p.items.iter().position(|r| r.seq == seq).unwrap();
            p.note = None;
            p.on_key(KeyEvent::from(KeyCode::Char('u')))
        };
        // #1 changed packages. #2 tried to undo it, stopped partway, and
        // carries the inverse of the part that ran.
        let one = receipt(1, None, Status::Ok, true);
        let two = receipt(2, Some(1), Status::Error, true);
        let mut p = ReceiptsPanel::new(vec![two.clone(), one.clone()]);
        assert!(p.undone.is_empty());
        assert_eq!(u(&mut p, 2), Action::Undo(2), "the partial can be undone");
        assert_eq!(u(&mut p, 1), Action::Undo(1), "and #1 tried again");

        // #3 took the partial back: #2 is undone, #1 still isn't.
        let three = receipt(3, Some(2), Status::Ok, true);
        p.load(vec![three.clone(), two.clone(), one.clone()]);
        assert_eq!(u(&mut p, 2), Action::None);
        assert_eq!(p.note, Some(Err("#2 was already undone".into())));
        assert_eq!(u(&mut p, 1), Action::Undo(1));

        // #4 undid #1 for good.
        p.load(vec![receipt(4, Some(1), Status::Ok, true), three, two, one]);
        assert_eq!(u(&mut p, 1), Action::None);
        assert_eq!(p.note, Some(Err("#1 was already undone".into())));
    }

    #[test]
    fn the_picker_hides_models_reeve_cant_drive() {
        let mut usable = ModelInfo::named("deepseek/deepseek-v4-pro");
        usable.tools = Some(true);
        let mut image = ModelInfo::named("openai/gpt-5-image");
        image.tools = Some(false);
        let mut p = ModelPicker::loading("openrouter", None, None);
        p.set_models(vec![
            usable,
            image,
            ModelInfo::named("text-embedding-3-small"),
        ]);
        let ids = |p: &ModelPicker| {
            p.filtered()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&p), ["deepseek/deepseek-v4-pro"]);
        assert_eq!(p.hidden(), 2);
        p.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(p.filtered().len(), 3);
        // The model in use always shows, usable or not.
        let mut q = ModelPicker::loading("openrouter", Some("openai/gpt-5-image".into()), None);
        q.set_models(p.models.clone());
        assert!(ids(&q).contains(&"openai/gpt-5-image".to_string()));
        assert_eq!(q.filtered()[q.sel].id, "openai/gpt-5-image");
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn typed(o: &mut Overlay, s: &str) {
        for c in s.chars() {
            o.on_key(key(KeyCode::Char(c)));
        }
    }

    fn row(name: &str, key: Option<&str>) -> ProviderRow {
        ProviderRow {
            name: name.into(),
            kind: "openrouter".into(),
            base_url: "https://x".into(),
            key: key.map(Into::into),
            active: false,
            model: None,
            status: None,
        }
    }

    fn finding(id: &str, fix: bool) -> reeve_core::findings::Finding {
        let now = chrono::Utc::now();
        reeve_core::findings::Finding {
            id: id.into(),
            severity: reeve_core::findings::Severity::Warning,
            title: format!("{id} keeps crashing"),
            detail: String::new(),
            evidence: vec![],
            first_seen: now,
            last_seen: now,
            count: 3,
            status: reeve_core::findings::FindingStatus::Open,
            resolved_at: None,
            notified_at: None,
            notified_severity: None,
            proposal: fix.then(|| reeve_core::findings::Proposal {
                text: "restart it".into(),
                drafted_at: now,
                model: "m".into(),
                usd: Some(0.01),
            }),
            draft_note: None,
        }
    }

    #[test]
    fn needs_you_is_one_queue_approval_first() {
        let mut p = NeedsPanel {
            asking: true,
            fixes: vec![finding("mailsync", true), finding("kwin", true)],
            reboot: Some("7.2.7".into()),
            sel: 0,
            scroll: 0,
        };
        assert_eq!(
            p.rows(),
            vec![Need::Approval, Need::Fix(0), Need::Fix(1), Need::Reboot]
        );
        assert!(p.on_approval());
        let mut o = Overlay::Needs(p.clone());
        assert_eq!(o.on_key(key(KeyCode::Down)), Action::None);
        assert_eq!(
            o.on_key(key(KeyCode::Enter)),
            Action::UseProposal("mailsync".into())
        );
        assert_eq!(o.tile(), Some(crate::view::Tile::Needs));
        // Without an approval, the fixes lead; the reboot is last.
        p.asking = false;
        p.sel = 2;
        let mut o = Overlay::Needs(p);
        assert!(matches!(o.on_key(key(KeyCode::Enter)), Action::Ask(q) if q.contains("7.2.7")));
    }

    #[test]
    fn each_tile_panel_says_which_tile_it_is() {
        use crate::view::Tile;
        let sys = |changed| {
            Overlay::System(SystemPanel {
                changed,
                days: 1,
                report: None,
                loading: true,
            })
        };
        assert_eq!(sys(false).tile(), Some(Tile::Health));
        assert_eq!(sys(true).tile(), Some(Tile::Changed));
        assert_eq!(
            Overlay::Receipts(ReceiptsPanel::default()).tile(),
            Some(Tile::Activity)
        );
        assert_eq!(Overlay::Help.tile(), None);
    }

    #[test]
    fn palette_matches_prefixes_only_for_a_bare_word() {
        assert_eq!(palette("/p", &[])[0].name, "/providers");
        assert_eq!(palette("/", &[]).len(), COMMANDS.len());
        assert!(palette("/providers now", &[]).is_empty());
        assert!(palette("hello", &[]).is_empty());
        // Skills follow the commands; one named like a command doesn't shadow it.
        let skills = vec![
            ("tidy-downloads".to_string(), "Sort Downloads".to_string()),
            ("update".to_string(), "mine".to_string()),
        ];
        let hits = palette("/ti", &skills);
        assert_eq!(
            (hits.len(), hits[0].name.as_str(), hits[0].skill),
            (1, "/tidy-downloads", true)
        );
        assert_eq!(palette("/", &skills).len(), COMMANDS.len() + 1);
        assert!(palette("/update", &skills).iter().all(|h| !h.skill));
        assert_eq!(
            skill_request("tidy-downloads", ""),
            "Use the skill tidy-downloads."
        );
        assert_eq!(
            skill_request("why-slow", " since the update "),
            "Use the skill why-slow: since the update"
        );
    }

    #[test]
    fn enter_on_a_keyless_provider_asks_for_the_key_first() {
        let mut o = Overlay::Providers(Providers {
            rows: vec![
                row("openrouter", None),
                row("openai", Some("$OPENAI_API_KEY")),
            ],
            sel: 0,
        });
        let a = o.on_key(key(KeyCode::Enter));
        assert!(matches!(
            a,
            Action::Push(Overlay::Key(KeyEntry { then_use: true, .. }))
        ));
        o.on_key(key(KeyCode::Down));
        assert_eq!(o.on_key(key(KeyCode::Enter)), Action::Use("openai".into()));
        // Past the last row is "add a connection".
        o.on_key(key(KeyCode::Down));
        assert!(matches!(
            o.on_key(key(KeyCode::Enter)),
            Action::Push(Overlay::Add(_))
        ));
    }

    #[test]
    fn a_key_is_never_shown_in_debug_output() {
        let mut o = Overlay::Key(KeyEntry::new("openrouter", false));
        typed(&mut o, "sk-or-secret");
        o.on_paste("-more\n");
        let dbg = format!("{o:?}");
        assert!(!dbg.contains("sk-or"), "{dbg}");
        match o.on_key(key(KeyCode::Enter)) {
            Action::SaveKey { secret, .. } => assert_eq!(secret, "sk-or-secret-more"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn model_search_needs_every_word() {
        let mut p = ModelPicker::loading("openrouter", Some("b/two".into()), None);
        p.set_models(vec![
            ModelInfo::named("a/one-fast"),
            ModelInfo::named("b/two"),
            ModelInfo::named("a/one"),
        ]);
        assert_eq!(
            p.filtered()[p.sel].id,
            "b/two",
            "starts on the current model"
        );
        let mut o = Overlay::Models(p);
        typed(&mut o, "one fast");
        let Overlay::Models(p) = &o else {
            unreachable!()
        };
        assert_eq!(p.filtered().len(), 1);
        assert!(
            matches!(o.on_key(key(KeyCode::Enter)), Action::ChooseModel { model, .. } if model == "a/one-fast")
        );
    }

    #[test]
    fn an_unknown_model_id_can_be_typed() {
        let mut o = Overlay::Models(ModelPicker::loading("box", None, None));
        typed(&mut o, "qwen3:32b");
        assert!(
            matches!(o.on_key(key(KeyCode::Enter)), Action::ChooseModel { model, .. } if model == "qwen3:32b")
        );
    }

    #[test]
    fn the_add_form_validates_and_follows_the_kind_template() {
        let mut o = Overlay::Add(AddForm::new());
        typed(&mut o, "bad name");
        assert_eq!(o.on_key(key(KeyCode::Enter)), Action::None);
        let Overlay::Add(f) = &mut o else {
            unreachable!()
        };
        assert!(f.error.is_some());
        f.name = "box".into();
        f.focus = 1;
        o.on_key(key(KeyCode::Right));
        o.on_key(key(KeyCode::Right));
        let Overlay::Add(f) = &o else { unreachable!() };
        assert_eq!(KINDS[f.kind].0, "local");
        assert!(f.base_url.starts_with("http://localhost"));
        match o.on_key(key(KeyCode::Enter)) {
            Action::AddConnection { name, conn } => {
                assert_eq!(name, "box");
                assert!(conn.is_local());
            }
            other => panic!("{other:?}"),
        }
    }
}

// ── /privacy ────────────────────────────────────────────────────────────────

/// The `/privacy` panel: masking levels, OpenRouter routing, and what's
/// been masked this session.
#[derive(Debug, Clone, PartialEq)]
pub struct PrivacyPanel {
    /// Settings being edited.
    pub cfg: reeve_core::config::PrivacyConfig,
    /// This session's masking.
    pub state: Option<reeve_core::agent::PrivacyState>,
    /// The connection is local (never masked).
    pub local: bool,
    /// The connection is OpenRouter (routing applies).
    pub openrouter: bool,
    /// First row of the masked list shown.
    pub scroll: usize,
    /// Last result.
    pub note: Option<Result<String, String>>,
}

impl PrivacyPanel {
    fn on_key(&mut self, k: KeyEvent) -> Action {
        use reeve_core::privacy::Level;
        let c = &mut self.cfg;
        match k.code {
            KeyCode::Char('l') => c.level = Level::parse(&c.level).next().as_str().into(),
            KeyCode::Char('b') => {
                c.background = Level::parse(&c.background).next().as_str().into();
            }
            KeyCode::Char('t') => c.no_training = !c.no_training,
            KeyCode::Char('z') => c.zdr = !c.zdr,
            KeyCode::Down | KeyCode::Char('j') => {
                self.scroll += 1;
                return Action::None;
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.scroll = self.scroll.saturating_sub(1);
                return Action::None;
            }
            _ => return Action::None,
        }
        Action::SavePrivacy(self.cfg.clone())
    }
}

// ── spend ───────────────────────────────────────────────────────────────────

/// How far back the statement looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SpendRange {
    /// Since local midnight.
    #[default]
    Day,
    /// The last 7 days.
    Week,
    /// This calendar month.
    Month,
}

impl SpendRange {
    /// The start of the range.
    pub fn since(self) -> chrono::DateTime<chrono::Utc> {
        use chrono::{Datelike, Local, TimeZone};
        let now = Local::now();
        let midnight = |d: chrono::NaiveDate| {
            Local
                .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
                .earliest()
                .map_or_else(chrono::Utc::now, |t| t.with_timezone(&chrono::Utc))
        };
        match self {
            Self::Day => midnight(now.date_naive()),
            Self::Week => midnight(now.date_naive() - chrono::Duration::days(6)),
            Self::Month => midnight(now.date_naive().with_day(1).unwrap_or(now.date_naive())),
        }
    }

    /// For the tab bar.
    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    fn step(self, up: bool) -> Self {
        match (self, up) {
            (Self::Day, true) => Self::Week,
            (Self::Week, true) | (Self::Month, true) => Self::Month,
            (Self::Month, false) => Self::Week,
            (Self::Week, false) | (Self::Day, false) => Self::Day,
        }
    }
}

/// The spend tab: a statement of what each session and draft cost.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SpendPanel {
    /// Range shown.
    pub range: SpendRange,
    /// Records in the range, oldest first.
    pub records: Vec<reeve_core::ledger::SpendRecord>,
    /// Records this month (for the month total and by-model).
    pub month: Vec<reeve_core::ledger::SpendRecord>,
    /// Selected statement line.
    pub sel: usize,
    /// The session in the ledger now.
    pub current: String,
    /// Daily, session, monthly caps and the warning level.
    pub caps: (f64, f64, f64, f64),
    /// The drafter's daily budget, when it's on.
    pub drafter_cap: Option<f64>,
    /// Last action's result.
    pub note: Option<Result<String, String>>,
}

impl SpendPanel {
    /// The statement lines for the range.
    pub fn lines(&self) -> Vec<reeve_core::ledger::StatementLine> {
        reeve_core::ledger::statement(&self.records)
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = self.lines().len();
        match k.code {
            KeyCode::Left => return self.set_range(self.range.step(false)),
            KeyCode::Right => return self.set_range(self.range.step(true)),
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(n.saturating_sub(1)),
            KeyCode::Char('e') => return Action::ExportSpend,
            KeyCode::Enter => {
                let lines = self.lines();
                if let Some(l) = lines.get(self.sel) {
                    if l.session == self.current {
                        return Action::Chat;
                    }
                    self.note = Some(Ok(format!(
                        "an earlier session: its report is in ~/.reeve/sessions/{}/report.md",
                        l.session
                    )));
                }
            }
            _ => {}
        }
        Action::None
    }

    fn set_range(&mut self, r: SpendRange) -> Action {
        if r != self.range {
            self.range = r;
            self.sel = usize::MAX;
            return Action::Open(crate::view::Tile::Spend);
        }
        Action::None
    }
}

// ── system ──────────────────────────────────────────────────────────────────

/// Health (F2) and what changed (F6): both drawn from one report.
#[derive(Debug, Clone, Default)]
pub struct SystemPanel {
    /// Showing what changed, not health.
    pub changed: bool,
    /// Window, in days.
    pub days: u32,
    /// The gathered page data (it takes a second or two).
    pub report: Option<Box<reeve_observer::report::Report>>,
    /// Being gathered.
    pub loading: bool,
}

impl PartialEq for SystemPanel {
    fn eq(&self, other: &Self) -> bool {
        self.days == other.days
            && self.changed == other.changed
            && self.loading == other.loading
            && self.report.is_some() == other.report.is_some()
    }
}

impl SystemPanel {
    fn on_key(&mut self, k: KeyEvent) -> Action {
        let steps = [1, 7, 30];
        let i = steps.iter().position(|d| *d == self.days).unwrap_or(0);
        match k.code {
            KeyCode::Left if i > 0 => Action::SystemReport(steps[i - 1]),
            KeyCode::Right if i + 1 < steps.len() => Action::SystemReport(steps[i + 1]),
            KeyCode::Char('r') => Action::OpenReport,
            _ => Action::None,
        }
    }
}

// ── needs you ───────────────────────────────────────────────────────────────

/// One row of F1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    /// The approval that's asking now (it lives in the view).
    Approval,
    /// A finding with a drafted fix, by index into `fixes`.
    Fix(usize),
    /// A kernel installed and waiting for a reboot.
    Reboot,
}

/// F1: what's waiting on you, in one queue. The approval asking now comes
/// first, then the drafted fixes, then suggestions.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NeedsPanel {
    /// An approval is asking.
    pub asking: bool,
    /// Live findings with a drafted fix.
    pub fixes: Vec<reeve_core::findings::Finding>,
    /// A kernel installed and waiting for a reboot.
    pub reboot: Option<String>,
    /// Selected row.
    pub sel: usize,
    /// Detail scroll.
    pub scroll: usize,
}

impl NeedsPanel {
    /// The rows, in order.
    pub fn rows(&self) -> Vec<Need> {
        let mut out = Vec::new();
        if self.asking {
            out.push(Need::Approval);
        }
        out.extend((0..self.fixes.len()).map(Need::Fix));
        if self.reboot.is_some() {
            out.push(Need::Reboot);
        }
        out
    }

    /// The selected row.
    pub fn selected(&self) -> Option<Need> {
        let rows = self.rows();
        rows.get(self.sel.min(rows.len().saturating_sub(1)))
            .copied()
    }

    /// The approval row is selected: its keys answer it.
    pub fn on_approval(&self) -> bool {
        self.selected() == Some(Need::Approval)
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let n = self.rows().len();
        match k.code {
            KeyCode::Up => {
                self.sel = self.sel.saturating_sub(1);
                self.scroll = 0;
            }
            KeyCode::Down => {
                self.sel = (self.sel + 1).min(n.saturating_sub(1));
                self.scroll = 0;
            }
            KeyCode::PageDown => self.scroll += 5,
            KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(5),
            _ => {}
        }
        use reeve_core::findings::FindingStatus as S;
        match (self.selected(), k.code) {
            (Some(Need::Fix(i)), code) => {
                let Some(f) = self.fixes.get(i) else {
                    return Action::None;
                };
                let id = f.id.clone();
                match code {
                    KeyCode::Enter => Action::UseProposal(id),
                    KeyCode::Char('d') => Action::Diagnose(id),
                    KeyCode::Char('a') => Action::FindingStatus(id, S::Acknowledged),
                    KeyCode::Char('x') => Action::FindingStatus(id, S::Dismissed),
                    _ => Action::None,
                }
            }
            (Some(Need::Reboot), KeyCode::Enter) => Action::Ask(format!(
                "Kernel {} is installed and waiting for a reboot. What would a reboot interrupt right now, and when is a good time?",
                self.reboot.clone().unwrap_or_default()
            )),
            _ => Action::None,
        }
    }
}

// ── ⌃K ──────────────────────────────────────────────────────────────────────

/// One thing ⌃K can find.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// `FIX`, `SEE`, `GO`, `KNOW`, `KEEP`, `DO`.
    pub group: &'static str,
    /// What it is.
    pub title: String,
    /// A little more, dim.
    pub detail: String,
    /// Where it lives, flush right.
    pub place: String,
    /// What ⏎ does.
    pub action: Action,
}

/// ⌃K: one search over chat, findings, fixes, orders, memory, receipts, and commands.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EverythingPanel {
    /// Typed so far.
    pub query: String,
    /// Everything there is to find.
    pub items: Vec<Hit>,
    /// Selected row of the matches.
    pub sel: usize,
}

impl EverythingPanel {
    /// Matches for the query, an "ask" first when something is typed.
    pub fn matches(&self) -> Vec<Hit> {
        let q = self.query.trim();
        let words: Vec<String> = q.split_whitespace().map(str::to_lowercase).collect();
        let mut out = Vec::new();
        if !q.is_empty() {
            out.push(Hit {
                group: "ASK",
                title: q.to_string(),
                detail: String::new(),
                place: "to the chat".into(),
                action: Action::Ask(q.to_string()),
            });
        }
        out.extend(
            self.items
                .iter()
                .filter(|h| {
                    let hay = format!("{} {} {}", h.title, h.detail, h.place).to_lowercase();
                    words.iter().all(|w| hay.contains(w.as_str()))
                })
                .cloned(),
        );
        out
    }

    fn on_key(&mut self, k: KeyEvent) -> Action {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let hits = self.matches();
        match k.code {
            KeyCode::Up => self.sel = self.sel.saturating_sub(1),
            KeyCode::Down => self.sel = (self.sel + 1).min(hits.len().saturating_sub(1)),
            KeyCode::Enter => {
                if let Some(h) = hits.get(self.sel.min(hits.len().saturating_sub(1))) {
                    return h.action.clone();
                }
            }
            KeyCode::Tab if !self.query.trim().is_empty() => {
                return Action::Ask(self.query.trim().to_string());
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.sel = 0;
            }
            KeyCode::Char('u') if ctrl => {
                self.query.clear();
                self.sel = 0;
            }
            KeyCode::Char('k') if ctrl => return Action::Close,
            KeyCode::Char(c) if !ctrl => {
                self.query.push(c);
                self.sel = 0;
            }
            _ => {}
        }
        Action::None
    }
}
