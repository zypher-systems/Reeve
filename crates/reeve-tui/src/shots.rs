//! Screens rendered to colored HTML, for looking at the design without a
//! terminal. Test-only, and only when `REEVE_SHOTS` names an output folder:
//!
//! ```sh
//! REEVE_SHOTS=/tmp/shots REEVE_SHOTS_HOME=~/.reeve cargo test -p reeve-tui shots -- --ignored
//! ```
//!
//! With `REEVE_SHOTS_HOME`, the board and the tiles read that Reeve home
//! (read-only); the chat is a made-up conversation.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::{Color, Modifier};

use reeve_core::agent::AgentEvent;
use reeve_core::policy::Tier;
use reeve_core::receipts::{Receipt, Status};
use reeve_core::spend::{Tally, Usage};
use reeve_observer::{Disk, HostInfo, Snapshot};

use crate::overlay::Overlay;
use crate::theme::Theme;
use crate::view::{Speaker, View};

const W: u16 = 160;
const H: u16 = 46;

fn hex(c: Color, fallback: &str) -> String {
    match c {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        Color::Black => "#000000".into(),
        _ => fallback.into(),
    }
}

/// One frame as HTML: a `<pre>` of colored cells.
fn html(v: &View, t: &Theme, title: &str) -> String {
    html_at(v, t, title, W, H)
}

/// One frame of `w`×`h` cells as HTML.
fn html_at(v: &View, t: &Theme, title: &str, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::draw::draw(f, v, t)).unwrap();
    let buf = term.backend().buffer().clone();
    let bg = hex(t.bg, "#151412");
    let fg = hex(t.fg, "#e9e4d8");
    let mut body = String::new();
    for y in 0..h {
        for x in 0..w {
            let c = &buf[(x, y)];
            let mut style = format!("color:{};background:{}", hex(c.fg, &fg), hex(c.bg, &bg));
            if c.modifier.contains(Modifier::BOLD) {
                style.push_str(";font-weight:700");
            }
            if c.modifier.contains(Modifier::ITALIC) {
                style.push_str(";font-style:italic");
            }
            if c.modifier.contains(Modifier::UNDERLINED) {
                style.push_str(";text-decoration:underline");
            }
            if c.modifier.contains(Modifier::CROSSED_OUT) {
                style.push_str(";text-decoration:line-through");
            }
            // Block elements fill the cell in a terminal; a font's glyphs
            // don't fill an 18px line, so draw them as backgrounds.
            let (f, b) = (hex(c.fg, &fg), hex(c.bg, &bg));
            let block = |layer: &str| format!("background:{layer},{b};color:transparent");
            let part =
                |pos: &str, size: &str| format!("linear-gradient({f},{f}) no-repeat {pos}/{size}");
            let fill = match c.symbol() {
                "█" => Some(block(&part("0 0", "100% 100%"))),
                "▀" => Some(block(&part("0 0", "100% 50%"))),
                "▄" => Some(block(&part("0 100%", "100% 50%"))),
                "▖" => Some(block(&part("0 100%", "50% 50%"))),
                "▗" => Some(block(&part("100% 100%", "50% 50%"))),
                "▘" => Some(block(&part("0 0", "50% 50%"))),
                "▝" => Some(block(&part("100% 0", "50% 50%"))),
                s => {
                    let lower = ["▁", "▂", "▃", "▅", "▆", "▇"].iter().position(|x| *x == s);
                    let left = ["▏", "▎", "▍", "▌", "▋", "▊", "▉"]
                        .iter()
                        .position(|x| *x == s);
                    match (lower, left) {
                        (Some(i), _) => {
                            let eighths = [1, 2, 3, 5, 6, 7][i];
                            Some(block(&part(
                                "0 100%",
                                &format!("100% {}%", eighths * 100 / 8),
                            )))
                        }
                        (_, Some(i)) => {
                            Some(block(&part("0 0", &format!("{}% 100%", (i + 1) * 100 / 8))))
                        }
                        _ => None,
                    }
                }
            };
            if let Some(fill) = fill {
                style = fill;
            }
            let sym = match c.symbol() {
                "&" => "&amp;".to_string(),
                "<" => "&lt;".to_string(),
                ">" => "&gt;".to_string(),
                s => s.to_string(),
            };
            let _ = write!(body, "<span style=\"{style}\">{sym}</span>");
        }
        body.push('\n');
    }
    format!(
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>body{{margin:0;background:{bg}}}pre{{margin:0;padding:10px;font:13px/18px 'DejaVu Sans Mono',monospace}}pre span{{display:inline-block;width:8px;height:18px;vertical-align:top;overflow:visible}}</style><pre>{body}</pre>"
    )
}

/// pipes.sh's PKGBUILD from the AUR, for the AUR card.
const AUR_PIPES_PKGBUILD: &str = r##"

# pipes.sh

# Maintainer: Stefans Mezulis <stefans.mezulis@gmail.com>
pkgname=pipes.sh
pkgver=1.3.0
pkgrel=1
pkgdesc='Animated pipes terminal screensaver'
arch=('any')
url='https://github.com/pipeseroni/pipes.sh'
license=('MIT')
groups=()
depends=('bash>=4.0.0')
makedepends=()
optdepends=()
provides=()
conflicts=()
replaces=()
backup=()
options=()
install=
changelog=
source=("https://github.com/pipeseroni/$pkgname/archive/v$pkgver.tar.gz")
noextract=()
sha256sums=('532976dd8dc2d98330c45a8bcb6d7dc19e0b0e30bba8872dcce352361655a426')

package() {
  cd "$pkgname-$pkgver"

  make DESTDIR="$pkgdir/" PREFIX=/usr install

  install -Dm644 -t "$pkgdir/usr/share/doc/$pkgname" LICENSE
  install -Dm644 -t "$pkgdir/usr/share/doc/$pkgname" README.rst
}
"##;

/// Omarchy's Tokyo Night `colors.toml`, as it ships.
const OMARCHY_TOKYO_NIGHT: &str = r##"mode = "dark"

accent = "#7aa2f7"
selection = "#292e42"
muted = "#414868"

background = "#1a1b26"
dark_background = "#13141c"
darker_background = "#0e0e14"
lighter_background = "#24283b"

foreground = "#a9b1d6"
dark_foreground = "#565f89"
light_foreground = "#b4bee6"
bright_foreground = "#c0caf5"

red = "#f7768e"
yellow = "#e0af68"
orange = "#eb927b"
green = "#9ece6a"
cyan = "#449dab"
blue = "#7aa2f7"
magenta = "#ad8ee6"
brown = "#75493d"

bright_red = "#ff7a93"
bright_yellow = "#ff9e64"
bright_green = "#b9f27c"
bright_cyan = "#0db9d7"
bright_blue = "#7da6ff"
bright_magenta = "#bb9af7"
"##;

/// Omarchy's Catppuccin Latte `colors.toml`, as it ships.
const OMARCHY_LATTE: &str = r##"mode = "light"

accent = "#1e66f5"
selection = "#ccd0da"
muted = "#acb0be"

background = "#eff1f5"
dark_background = "#e3e4e8"
darker_background = "#d7d8dc"
lighter_background = "#dce0e8"

foreground = "#4c4f69"
dark_foreground = "#9ca0b0"
light_foreground = "#5c5f77"
bright_foreground = "#4c4f69"

red = "#d20f39"
yellow = "#df8e1d"
orange = "#d84e2b"
green = "#40a02b"
cyan = "#179299"
blue = "#1e66f5"
magenta = "#ea76cb"
brown = "#6c2715"

bright_red = "#d20f39"
bright_yellow = "#df8e1d"
bright_green = "#40a02b"
bright_cyan = "#179299"
bright_blue = "#1e66f5"
bright_magenta = "#ea76cb"
"##;

fn host() -> HostInfo {
    HostInfo {
        hostname: "Nexus".into(),
        os_short: "Fedora 44".into(),
        os_pretty: "Fedora Linux 44".into(),
        kernel: "7.2.6-200.fc44.x86_64".into(),
        cpus: 32,
        ..HostInfo::default()
    }
}

fn base() -> View {
    let mut v = View::new(host());
    v.model = "x-ai/grok-4.7".into();
    v.connection = "openrouter".into();
    v.ready = true;
    v.observer_alive = true;
    v.session_id = "s7".into();
    v.sample(Snapshot {
        uptime_secs: 5 * 86_400 + 14 * 3600,
        cpu_pct: Some(7.0),
        load: [1.2, 1.1, 1.0],
        mem_total: 126 << 30,
        mem_used: 34 << 30,
        swap_total: 8 << 30,
        swap_used: 8 << 30,
        temp_c: Some(33.0),
        net_rx_bps: Some(56_000.0),
        net_tx_bps: Some(83_000.0),
        disks: vec![
            Disk {
                mount: "/".into(),
                fs: "btrfs".into(),
                total: 2 << 40,
                used: 162 << 30,
                avail: 1800 << 30,
            },
            Disk {
                mount: "/boot".into(),
                fs: "ext4".into(),
                total: 4 << 30,
                used: 893 << 20,
                avail: 3 << 30,
            },
            Disk {
                mount: "/home".into(),
                fs: "btrfs".into(),
                total: 4 << 40,
                used: 923 << 30,
                avail: 3100 << 30,
            },
        ],
        ..Snapshot::default()
    });
    for i in 0..240 {
        v.cpu_hist
            .push_back(4.0 + ((i * 7) % 11) as f32 + if i % 53 == 0 { 40.0 } else { 0.0 });
    }
    v.failed = Some(Vec::new());
    v
}

#[allow(clippy::too_many_arguments)]
fn tool(
    v: &mut View,
    id: &str,
    name: &str,
    tier: Tier,
    summary: &str,
    status: Status,
    result: &str,
    seq: u64,
    undo: bool,
) {
    v.apply(AgentEvent::ToolStarted {
        id: id.into(),
        tool: name.into(),
        tier,
        summary: summary.into(),
    });
    let mut r = Receipt::draft("s7", name, Default::default(), tier);
    r.seq = seq;
    r.approved_by = if tier == Tier::T0 {
        "policy".into()
    } else {
        "user".into()
    };
    if undo {
        r.undo = Some(reeve_core::undo::Undo::Files { changes: vec![] });
    }
    v.apply(AgentEvent::ToolFinished {
        id: id.into(),
        status,
        summary: result.into(),
        diff: None,
        receipt: Some(Box::new(r)),
    });
}

fn spend(usd: f64, session: f64, input: u64, output: u64) -> AgentEvent {
    AgentEvent::Spend {
        usd: Some(usd),
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
            ..Usage::default()
        },
        session: Tally {
            usd: session,
            calls: 1,
            ..Tally::default()
        },
        totals: Box::default(),
    }
}

/// The mailsync story, as a ledger.
fn ledger() -> View {
    let mut v = base();
    v.push(
        Speaker::Drafter,
        "drafted a fix for mailsync keeps crashing",
    );
    if let Some(e) = v.entries.last_mut() {
        e.cost = Some(crate::view::RoundCost {
            usd: Some(0.0276),
            usage: Usage::default(),
            session: 0.0,
        });
    }
    v.push(
        Speaker::User,
        "mailsync keeps crashing and swap is full. what's going on?",
    );
    v.apply(AgentEvent::TurnStarted);
    v.apply(AgentEvent::Text("Looking in three places.".into()));
    v.apply(spend(0.0120, 0.0120, 7900, 153));
    tool(
        &mut v,
        "1",
        "logs_query",
        Tier::T0,
        "coredumpctl list mailsync --since -24h",
        Status::Ok,
        "102 dumps",
        81,
        false,
    );
    tool(
        &mut v,
        "2",
        "proc_list",
        Tier::T0,
        "proc_list · by memory",
        Status::Ok,
        "top 20 by memory",
        82,
        false,
    );
    tool(
        &mut v,
        "3",
        "memory_search",
        Tier::T0,
        "memory_search \"mailsync crash\"",
        Status::Ok,
        "1 runbook",
        83,
        false,
    );
    v.apply(AgentEvent::Text("It dumps core **every 15 minutes**, like a timer — 102 times since yesterday. Swap is 99% full while memory sits at 30%, so whatever leaked was pushed out. The drafter's fix restarts it with its cache moved aside; I'll check it held.".into()));
    v.apply(spend(0.0060, 0.0180, 9230, 129));
    v.push(Speaker::Reeved, "mailsync dumped core again (103×)");
    tool(
        &mut v,
        "4",
        "change_begin",
        Tier::T0,
        "verified change: stop mailsync crashing",
        Status::Ok,
        "will check: app-com.getmailspring.service (user) is active; app-com.getmailspring.service logs no errors",
        84,
        false,
    );
    tool(
        &mut v,
        "5",
        "svc_control",
        Tier::T1,
        "stop app-com.getmailspring… (user)",
        Status::Ok,
        "stopped",
        85,
        true,
    );
    v.apply(AgentEvent::ToolStarted {
        id: "6".into(),
        tool: "fs_move".into(),
        tier: Tier::T1,
        summary: "move ~/.config/Mailspring/cache → cache.bak".into(),
    });
    v
}

fn write(dir: &Path, name: &str, page: String) {
    let _ = std::fs::write(dir.join(format!("{name}.html")), page);
}

/// The approval the mailsync story is waiting on.
fn asking() -> crate::view::Pending {
    crate::view::Pending {
        req: reeve_core::agent::ApprovalRequest {
            tool: "fs_move".into(),
            summary: "move ~/.config/Mailspring/cache → cache.bak".into(),
            tier: Tier::T1,
            reasons: vec!["changes your files".into()],
            sudo: false,
            why: Some("a corrupt cache is the likeliest cause of the loop".into()),
            preview: None,
            undoable: true,
            can_allow_session: true,
            session_scope: Some("writes in ~/.config/Mailspring".into()),
            can_allow_turn: true,
            command: None,
            paths: vec![],
            txn: Some(reeve_core::txn::TxnBrief {
                goal: "stop mailsync crashing".into(),
                checks: vec!["app-com.getmailspring.service (user) is active".into()],
            }),
            details: Vec::new(),
        },
        typed: String::new(),
    }
}

/// The board's data from a real home, when there is one.
fn fill(v: &mut View, home: Option<&Path>) {
    let Some(home) = home else { return };
    let items = reeve_core::findings::FindingStore::new(home).list();
    v.findings = items
        .into_iter()
        .filter(|f| f.status == reeve_core::findings::FindingStatus::Open)
        .collect();
    v.board = crate::board::read(home, &reeve_core::memory::Memory::new(home));
    v.board.report = Some(Box::new(reeve_observer::report::gather(home, 1)));
    v.totals = reeve_core::ledger::totals(home, chrono::Local::now());
    v.drafter = Some((0.13, 0.25));
    let mut r = reeve_core::receipts::ReceiptBook::new(home).recent(40);
    r.truncate(40);
    if !r.is_empty() {
        v.receipts = r;
    }
}

#[test]
#[ignore = "writes HTML for looking at; needs REEVE_SHOTS"]
fn shots() {
    let Some(dir) = std::env::var_os("REEVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let home = std::env::var_os("REEVE_SHOTS_HOME").map(PathBuf::from);
    let home = home.as_deref();
    let t = Theme::slate();

    // Home: the board, with an approval asking.
    let mut v = ledger();
    fill(&mut v, home);
    v.approval = Some(asking());
    write(&dir, "0-board", html(&v, &t, "board"));

    // The same, with a newer Reeve out.
    v.update = Some(reeve_core::update::Badge::Available(
        reeve_core::update::Version(0, 5, 0),
    ));
    write(&dir, "0-board-update", html(&v, &t, "board, update out"));
    v.update = None;

    // On Omarchy (theme "auto"): the same board in two of its themes.
    for (name, colors) in [
        ("0c-board-tokyo-night", OMARCHY_TOKYO_NIGHT),
        ("0d-board-catppuccin-latte", OMARCHY_LATTE),
    ] {
        let t = Theme::from_palette(&crate::theme::Palette::parse(colors)).unwrap();
        write(&dir, name, html(&v, &t, name));
    }

    // An AUR install on Arch: the PKGBUILD on the card, asked every time.
    let mut aur = ledger();
    aur.chat = true;
    aur.approval = Some(crate::view::Pending {
        req: reeve_core::agent::ApprovalRequest {
            tool: "pkg_install".into(),
            summary: "yay -S --needed --noconfirm --answerclean None --answerdiff None --answeredit None pipes.sh".into(),
            tier: Tier::T2,
            reasons: vec![
                "changes installed packages".into(),
                "may build from the AUR (user-submitted PKGBUILDs Arch doesn't review)".into(),
            ],
            sudo: false,
            why: Some("you asked for the pipes screensaver".into()),
            preview: None,
            undoable: true,
            can_allow_session: false,
            session_scope: None,
            can_allow_turn: false,
            command: None,
            paths: vec![],
            txn: None,
            details: reeve_core::pacman::aur_details(
                &[("pipes.sh".into(), AUR_PIPES_PKGBUILD.into())],
                "yay",
            ),
        },
        typed: String::new(),
    });
    write(&dir, "5-aur-card", html_at(&aur, &t, "aur card", 120, 60));

    // Skills: the starter three in F8 memory, and one offered by the slash palette.
    let tmp = tempfile::tempdir().unwrap();
    let skills = reeve_core::skills::Skills::new(tmp.path());
    skills.seed_examples_once().unwrap();
    let mut sk = base();
    let mut panel =
        crate::overlay::MemoryPanel::load(&reeve_core::memory::Memory::new(tmp.path()), &skills);
    panel.select_skill("tidy-downloads");
    sk.overlays.push(Overlay::Memory(panel));
    write(&dir, "f8-skills", html(&sk, &t, "skills"));
    let mut pal = ledger();
    pal.chat = true;
    pal.skills = skills
        .load()
        .0
        .into_iter()
        .map(|s| (s.id, s.description))
        .collect();
    pal.input = "/u".into();
    pal.cursor = 2;
    write(
        &dir,
        "1c-palette-skills",
        html(&pal, &t, "palette with skills"),
    );

    // The chat, the approval asking there.
    v.chat = true;
    write(&dir, "1-chat", html(&v, &t, "chat"));

    // The chat after the change was checked.
    let mut v = ledger();
    fill(&mut v, home);
    v.chat = true;
    let mut r = Receipt::draft("s7", "fs_move", Default::default(), Tier::T1);
    r.seq = 86;
    r.approved_by = "user".into();
    v.apply(AgentEvent::ToolFinished {
        id: "6".into(),
        status: Status::Ok,
        summary: "moved".into(),
        diff: None,
        receipt: Some(Box::new(r)),
    });
    tool(
        &mut v,
        "7",
        "svc_control",
        Tier::T1,
        "start app-com.getmailspring… (user)",
        Status::Ok,
        "started",
        87,
        true,
    );
    tool(
        &mut v,
        "8",
        "change_commit",
        Tier::T0,
        "check: stop mailsync crashing",
        Status::Ok,
        "verified: stop mailsync crashing (2 checks passed)",
        88,
        false,
    );
    v.apply(AgentEvent::Text(
        "Fixed and checked: no crash in 20 minutes. Saved as a runbook.".into(),
    ));
    v.apply(spend(0.0082, 0.0262, 10946, 357));
    v.apply(AgentEvent::TurnDone { truncated: false });
    write(&dir, "1b-chat-done", html(&v, &t, "chat"));

    // ⌃K over the board.
    let mut v = ledger();
    fill(&mut v, home);
    let hits = vec![
        crate::overlay::Hit {
            group: "FIX",
            title: "Run the drafted fix: mailsync keeps crashing".into(),
            detail: "verified, in the chat".into(),
            place: "F1 needs you".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "SEE",
            title: "mailsync keeps crashing".into(),
            detail: "warning · 103×".into(),
            place: "F3 findings".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "GO",
            title: "#81 logs_query coredumpctl list mailsync".into(),
            detail: "102 dumps".into(),
            place: "F4 activity".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "KNOW",
            title: "Mailspring crash loop".into(),
            detail: "runbook".into(),
            place: "F8 memory".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "KEEP",
            title: "Standing order: restart mailsync when it loops".into(),
            detail: "off".into(),
            place: "F7 orders".into(),
            action: crate::overlay::Action::None,
        },
    ];
    v.overlays
        .push(Overlay::Everything(crate::overlay::EverythingPanel {
            query: "mailsync".into(),
            items: hits,
            sel: 0,
        }));
    write(&dir, "2-search", html(&v, &t, "search"));

    // A board in a smaller terminal.
    let mut v = ledger();
    fill(&mut v, home);
    v.approval = Some(asking());
    write(&dir, "0b-board-100x32", html_at(&v, &t, "board", 100, 32));

    let Some(home) = home else { return };
    let store = reeve_core::findings::FindingStore::new(home);

    // F1 · needs you.
    let mut v = ledger();
    fill(&mut v, Some(home));
    v.approval = Some(asking());
    let mut fixes: Vec<_> = store
        .list()
        .into_iter()
        .filter(|f| f.status == reeve_core::findings::FindingStatus::Open && f.proposal.is_some())
        .collect();
    fixes.sort_by(|a, b| b.severity.cmp(&a.severity));
    v.overlays.push(Overlay::Needs(crate::overlay::NeedsPanel {
        asking: true,
        fixes: fixes.clone(),
        reboot: v
            .board
            .report
            .as_ref()
            .and_then(|r| r.drift.reboot_for.clone()),
        sel: 0,
        scroll: 0,
    }));
    write(&dir, "f1-needs", html(&v, &t, "needs you"));
    if let Some(Overlay::Needs(p)) = v.overlays.first_mut() {
        p.sel = 1;
    }
    write(&dir, "f1b-needs-fix", html(&v, &t, "needs you"));

    // F2 · health, and F6 · what changed.
    let mut v = base();
    fill(&mut v, Some(home));
    let report = v.board.report.clone();
    v.overlays
        .push(Overlay::System(crate::overlay::SystemPanel {
            changed: false,
            days: 1,
            report: report.clone(),
            loading: false,
        }));
    write(&dir, "f2-health", html(&v, &t, "health"));
    v.overlays.clear();
    v.overlays
        .push(Overlay::System(crate::overlay::SystemPanel {
            changed: true,
            days: 1,
            report,
            loading: false,
        }));
    write(&dir, "f6-changed", html(&v, &t, "changed"));

    // F3 · findings.
    let mut v = base();
    fill(&mut v, Some(home));
    let mut p = crate::overlay::FindingsPanel::default();
    p.refresh(store.list());
    if let Some(i) = p.items.iter().position(|f| f.id.contains("mailsync")) {
        p.sel = i;
    }
    v.overlays.push(Overlay::Findings(p));
    write(&dir, "f3-findings", html(&v, &t, "findings"));

    // F4 · activity.
    let mut v = base();
    fill(&mut v, Some(home));
    let items = reeve_core::receipts::ReceiptBook::new(home).recent(500);
    let undone = items.iter().filter_map(|r| r.undoes).collect();
    v.overlays
        .push(Overlay::Receipts(crate::overlay::ReceiptsPanel {
            items,
            undone,
            sel: 0,
            note: None,
        }));
    write(&dir, "f4-activity", html(&v, &t, "activity"));

    // F5 · spend.
    let mut v = base();
    fill(&mut v, Some(home));
    let records = reeve_core::ledger::since(home, crate::overlay::SpendRange::Day.since());
    let n = reeve_core::ledger::statement(&records).len();
    v.overlays.push(Overlay::Spend(crate::overlay::SpendPanel {
        range: crate::overlay::SpendRange::Day,
        records,
        month: reeve_core::ledger::since(home, crate::overlay::SpendRange::Month.since()),
        sel: n.saturating_sub(1),
        current: String::new(),
        caps: (0.0, 0.0, 0.0, 1.0),
        drafter_cap: Some(0.25),
        note: None,
    }));
    write(&dir, "f5-spend", html(&v, &t, "spend"));

    // F7 · orders, and F8 · memory.
    let mut v = base();
    fill(&mut v, Some(home));
    v.overlays
        .push(Overlay::Orders(crate::overlay::OrdersPanel::load(
            &reeve_core::orders::Orders::new(home),
        )));
    write(&dir, "f7-orders", html(&v, &t, "orders"));
    v.overlays.clear();
    v.overlays
        .push(Overlay::Memory(crate::overlay::MemoryPanel::load(
            &reeve_core::memory::Memory::new(home),
            &reeve_core::skills::Skills::new(home),
        )));
    write(&dir, "f8-memory", html(&v, &t, "memory"));
}

#[test]
#[ignore = "needs REEVE_SHOTS_HOME"]
fn everything_with_a_real_home() {
    let Some(home) = std::env::var_os("REEVE_SHOTS_HOME").map(PathBuf::from) else {
        return;
    };
    let mut items = Vec::new();
    for f in reeve_core::findings::FindingStore::new(&home)
        .list()
        .iter()
        .filter(|f| f.is_live())
    {
        items.push(crate::overlay::Hit {
            group: "SEE",
            title: f.title.clone(),
            detail: f.severity.as_str().into(),
            place: "F3 findings".into(),
            action: crate::overlay::Action::None,
        });
    }
    for n in reeve_core::memory::Memory::new(&home).all() {
        items.push(crate::overlay::Hit {
            group: "KNOW",
            title: n.title.clone(),
            detail: "fact".into(),
            place: "F5 memory".into(),
            action: crate::overlay::Action::None,
        });
    }
    for r in reeve_core::receipts::ReceiptBook::new(&home).recent(40) {
        items.push(crate::overlay::Hit {
            group: "GO",
            title: format!("#{} {} {}", r.seq, r.tool, r.target()),
            detail: r.outcome.summary.clone(),
            place: "receipts".into(),
            action: crate::overlay::Action::None,
        });
    }
    for (w, h) in [(150u16, 40u16), (120, 30), (80, 24), (200, 60)] {
        let mut v = base();
        v.overlays
            .push(Overlay::Everything(crate::overlay::EverythingPanel {
                query: String::new(),
                items: items.clone(),
                sel: 0,
            }));
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| crate::draw::draw(f, &v, &Theme::ink()))
            .unwrap();
        let buf = term.backend().buffer().clone();
        let text: String = (0..h)
            .map(|y| (0..w).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        println!("{w}x{h}:\n{text}");
    }
}
