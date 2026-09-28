//! Screens rendered to colored HTML, for looking at the design without a
//! terminal. Test-only, and only when `REEVE_SHOTS` names an output folder:
//!
//! ```sh
//! REEVE_SHOTS=/tmp/shots REEVE_SHOTS_HOME=~/.reeve cargo test -p reeve-tui shots -- --ignored
//! ```
//!
//! With `REEVE_SHOTS_HOME`, the findings, spend, and system screens read that
//! Reeve home (read-only); the ledger is a made-up conversation.

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
    let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
    term.draw(|f| crate::draw::draw(f, v, t)).unwrap();
    let buf = term.backend().buffer().clone();
    let bg = hex(t.bg, "#151412");
    let fg = hex(t.fg, "#e9e4d8");
    let mut body = String::new();
    for y in 0..H {
        for x in 0..W {
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
        "<!doctype html><meta charset=utf-8><title>{title}</title><style>body{{margin:0;background:{bg}}}pre{{margin:0;padding:10px;font:13px/18px 'DejaVu Sans Mono',monospace}}pre span{{display:inline-block;width:8px;overflow:visible}}</style><pre>{body}</pre>"
    )
}

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

#[test]
#[ignore = "writes HTML for looking at; needs REEVE_SHOTS"]
fn shots() {
    let Some(dir) = std::env::var_os("REEVE_SHOTS").map(PathBuf::from) else {
        return;
    };
    std::fs::create_dir_all(&dir).unwrap();
    let home = std::env::var_os("REEVE_SHOTS_HOME").map(PathBuf::from);
    let t = Theme::ink();

    // 1 · ledger, mid-change, with the approval asking.
    let mut v = ledger();
    v.approval = Some(crate::view::Pending {
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
            command: None,
            paths: vec![],
            txn: Some(reeve_core::txn::TxnBrief {
                goal: "stop mailsync crashing".into(),
                checks: vec!["app-com.getmailspring.service (user) is active".into()],
            }),
        },
        typed: String::new(),
    });
    write(&dir, "1-ledger", html(&v, &t, "ledger"));

    // 1b · the ledger after the change was checked.
    let mut v = ledger();
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
    write(&dir, "1b-ledger-done", html(&v, &t, "ledger"));

    // ⌃K over the ledger.
    let mut v = ledger();
    let hits = vec![
        crate::overlay::Hit {
            group: "FIX",
            title: "Run the drafted fix: mailsync keeps crashing".into(),
            detail: "verified, in the ledger".into(),
            place: "F2 findings".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "SEE",
            title: "mailsync keeps crashing".into(),
            detail: "warning · 103×".into(),
            place: "F2 findings".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "GO",
            title: "#81 logs_query coredumpctl list mailsync".into(),
            detail: "102 dumps".into(),
            place: "receipts".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "KNOW",
            title: "Mailspring crash loop".into(),
            detail: "runbook".into(),
            place: "F5 memory".into(),
            action: crate::overlay::Action::None,
        },
        crate::overlay::Hit {
            group: "KEEP",
            title: "Standing order: restart mailsync when it loops".into(),
            detail: "off".into(),
            place: "F3 orders".into(),
            action: crate::overlay::Action::None,
        },
    ];
    v.overlays
        .push(Overlay::Everything(crate::overlay::EverythingPanel {
            query: "mailsync".into(),
            items: hits,
            sel: 0,
        }));
    write(&dir, "2-everything", html(&v, &t, "everything"));

    let Some(home) = home else { return };
    // Findings, from that home.
    let mut v = base();
    let items = reeve_core::findings::FindingStore::new(&home).list();
    v.findings = items.iter().filter(|f| f.is_live()).cloned().collect();
    let mut p = crate::overlay::FindingsPanel::default();
    p.refresh(items);
    if let Some(i) = p.items.iter().position(|f| f.id.contains("mailsync")) {
        p.sel = i;
    }
    v.overlays.push(Overlay::Findings(p));
    write(&dir, "3-findings", html(&v, &t, "findings"));

    // Spend.
    let mut v = base();
    v.findings.clear();
    let records = reeve_core::ledger::since(&home, crate::overlay::SpendRange::Day.since());
    let n = reeve_core::ledger::statement(&records).len();
    v.drafter = Some((0.13, 0.25));
    v.overlays.push(Overlay::Spend(crate::overlay::SpendPanel {
        range: crate::overlay::SpendRange::Day,
        records,
        month: reeve_core::ledger::since(&home, crate::overlay::SpendRange::Month.since()),
        sel: n.saturating_sub(1),
        current: String::new(),
        caps: (0.0, 0.0, 0.0, 1.0),
        drafter_cap: Some(0.25),
        note: None,
    }));
    write(&dir, "4-spend", html(&v, &t, "spend"));

    // System.
    let mut v = base();
    let r = reeve_observer::report::gather(&home, 1);
    v.overlays
        .push(Overlay::System(crate::overlay::SystemPanel {
            days: 1,
            report: Some(Box::new(r)),
            loading: false,
        }));
    write(&dir, "6-system", html(&v, &t, "system"));

    // Memory and orders, as they are.
    let mut v = base();
    v.overlays
        .push(Overlay::Memory(crate::overlay::MemoryPanel::load(
            &reeve_core::memory::Memory::new(&home),
        )));
    write(&dir, "5-memory", html(&v, &t, "memory"));
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
            place: "F2 findings".into(),
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
