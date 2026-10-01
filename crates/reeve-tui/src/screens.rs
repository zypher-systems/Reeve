//! The tiles, opened: needs you, health, findings, activity, spend, what
//! changed, orders, and memory. Each fills the main area under the strip,
//! with a header line and its keys along the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use chrono::{DateTime, Local, Utc};
use reeve_core::findings::{Finding, FindingStatus, Severity};
use reeve_core::receipts::Status;
use reeve_core::spend::{format_tokens, format_usd};

use crate::board::{self, hints, split, surface, tier_pill};
use crate::draw::{braille, markdown, pad, plain_wrap, system_lines, truncate};
use crate::overlay::{
    FindingsPanel, MemoryPanel, Need, NeedsPanel, OrdersPanel, Overlay, ReceiptsPanel, SpendPanel,
    SystemPanel,
};
use crate::theme::Theme;
use crate::view::{Tile, View};

/// Draw an open tile's screen inside `area` (the main surface's inside).
pub fn draw(f: &mut Frame, area: Rect, v: &View, t: &Theme, tile: Tile) {
    let Some(top) = v.overlays.first() else {
        return;
    };
    let name = tile.label();
    let mut title = vec![Span::styled(
        format!("{}{}", name[..1].to_uppercase(), &name[1..]),
        Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
    )];
    let about = summary(top, v);
    if !about.is_empty() {
        title.push(Span::styled(format!("   {about}"), t.muted()));
    }
    f.render_widget(
        Paragraph::new(split(
            title,
            hints(&[("esc", "back")], t),
            area.width as usize,
        )),
        Rect { height: 1, ..area },
    );
    let body = Rect {
        y: area.y + 2,
        height: area.height.saturating_sub(2),
        ..area
    };
    match top {
        Overlay::Needs(p) => needs(f, body, v, p, t),
        Overlay::Findings(p) => findings(f, body, p, t),
        Overlay::Receipts(p) => activity(f, body, v, p, t),
        Overlay::Orders(p) => orders(f, body, v, p, t),
        Overlay::Memory(p) => memory(f, body, p, t),
        Overlay::Spend(p) => spend(f, body, v, p, t),
        Overlay::System(p) if p.changed => changed(f, body, p, t),
        Overlay::System(p) => system(f, body, v, p, t),
        _ => {}
    }
}

/// The header's one line on what the screen holds.
fn summary(top: &Overlay, v: &View) -> String {
    match top {
        Overlay::Needs(p) => {
            let mut parts = Vec::new();
            if p.asking {
                parts.push("1 approval".to_string());
            }
            if !p.fixes.is_empty() {
                parts.push(format!(
                    "{} fix{} ready",
                    p.fixes.len(),
                    if p.fixes.len() == 1 { "" } else { "es" }
                ));
            }
            if p.reboot.is_some() {
                parts.push("1 suggestion".into());
            }
            parts.join(" · ")
        }
        Overlay::Findings(p) => {
            let live = p.items.iter().filter(|x| x.is_live()).count();
            format!("{live} open · {} earlier", p.items.len() - live)
        }
        Overlay::Receipts(p) => format!("{} receipts, newest first", p.items.len()),
        Overlay::Spend(_) => format!(
            "today {} · month {}",
            v.totals.today.label(),
            v.totals.month.label()
        ),
        Overlay::System(p) if p.changed => "packages, kernels, /etc, and services".into(),
        Overlay::System(_) => "minute readings from reeved".into(),
        Overlay::Orders(_) => "work Reeve does on its own, inside limits you set".into(),
        Overlay::Memory(_) => format!("{} in use · {} new", v.memory.0, v.memory.1),
        _ => String::new(),
    }
}

/// Split `area` into a list on the left and a sunken detail card on the
/// right; returns (list, detail inside).
fn list_detail(f: &mut Frame, area: Rect, list_pct: u16, t: &Theme) -> (Rect, Rect) {
    let list_w = (area.width * list_pct / 100).clamp(34.min(area.width), 72);
    let [list, _gap, detail] = Layout::horizontal([
        Constraint::Length(list_w),
        Constraint::Length(2),
        Constraint::Min(20),
    ])
    .areas(area);
    let detail = surface(f, detail, t.inset, t.panel, t);
    (list, detail)
}

fn label(s: &str, t: &Theme) -> Span<'static> {
    Span::styled(s.to_string(), Style::default().fg(t.faint))
}

fn keys(pairs: &[(&str, &str)], t: &Theme) -> Line<'static> {
    Line::from(hints(pairs, t))
}

fn ago(t: DateTime<Utc>) -> String {
    let s = (Utc::now() - t).num_seconds().max(0);
    match s {
        0..=89 => "just now".into(),
        90..=5399 => format!("{}m ago", s / 60),
        5400..=129_599 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

fn severity(sev: Severity, t: &Theme) -> (&'static str, ratatui::style::Color) {
    board::severity(sev, t)
}

/// A row that stays on screen: the lines from `sel_line`'s neighborhood.
fn keep_in_view(lines: Vec<Line<'static>>, sel_line: usize, h: usize) -> Vec<Line<'static>> {
    let skip = sel_line.saturating_sub(h.saturating_sub(2));
    lines.into_iter().skip(skip).collect()
}

// ── findings ────────────────────────────────────────────────────────────────

fn finding_row(x: &Finding, on: bool, w: usize, t: &Theme) -> Line<'static> {
    let bg = if on { t.input } else { t.panel };
    let (icon, c) = severity(x.severity, t);
    let live = x.is_live();
    let mut right = Vec::new();
    if x.proposal.is_some() && live {
        right.push(Span::styled("fix ", Style::default().fg(t.good).bg(bg)));
    }
    if x.count > 1 {
        right.push(Span::styled(
            format!("{}×", x.count),
            Style::default().fg(t.dim).bg(bg),
        ));
    }
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    let title_w = w.saturating_sub(4 + rw + 1);
    let title_style = if !live {
        Style::default().fg(t.faint)
    } else if on {
        Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
    } else {
        t.text()
    };
    let mut spans = vec![
        Span::styled(" ", Style::default().bg(bg)),
        Span::styled(
            format!("{icon} "),
            Style::default().fg(if live { c } else { t.faint }).bg(bg),
        ),
        Span::styled(
            pad(&truncate(&x.title, title_w), title_w),
            title_style.bg(bg),
        ),
        Span::styled(" ", Style::default().bg(bg)),
    ];
    spans.extend(right);
    let used: usize = spans.iter().map(|s| s.content.width()).sum();
    spans.push(Span::styled(
        " ".repeat(w.saturating_sub(used)),
        Style::default().bg(bg),
    ));
    Line::from(spans)
}

fn findings(f: &mut Frame, area: Rect, p: &FindingsPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let (list, detail) = list_detail(f, body, 40, t);
    let w = list.width as usize;
    let mut lines = Vec::new();
    let live: Vec<(usize, &Finding)> = p
        .items
        .iter()
        .enumerate()
        .filter(|(_, x)| x.is_live())
        .collect();
    let rest: Vec<(usize, &Finding)> = p
        .items
        .iter()
        .enumerate()
        .filter(|(_, x)| !x.is_live())
        .collect();
    lines.push(Line::from(vec![
        label("open", t),
        Span::styled(format!("  {}", live.len()), t.ghost()),
    ]));
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "Nothing found. reeved reports here; /observer starts it.",
            Style::default().fg(t.dim),
        )));
    }
    let mut sel_line = 0;
    for (i, x) in &live {
        if *i == p.sel {
            sel_line = lines.len();
        }
        lines.push(finding_row(x, *i == p.sel, w, t));
    }
    if !rest.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            label("earlier", t),
            Span::styled(format!("  {}", rest.len()), t.ghost()),
        ]));
        for (i, x) in rest.iter().take(60) {
            if *i == p.sel {
                sel_line = lines.len();
            }
            lines.push(finding_row(x, *i == p.sel, w, t));
        }
    }
    f.render_widget(
        Paragraph::new(keep_in_view(lines, sel_line, list.height as usize)),
        list,
    );

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(x) = p.selected() {
        let (icon, c) = severity(x.severity, t);
        out.push(Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(c)),
            Span::styled(
                x.title.clone(),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
        ]));
        let status = match x.status {
            FindingStatus::Open => "open",
            FindingStatus::Acknowledged => "seen",
            FindingStatus::Resolved => "resolved",
            FindingStatus::Dismissed => "dismissed",
        };
        out.push(Line::from(Span::styled(
            truncate(
                &format!(
                    "{} · {} · first {} · last {} · {}×",
                    x.severity.as_str(),
                    status,
                    ago(x.first_seen),
                    ago(x.last_seen),
                    x.count
                ),
                dw,
            ),
            Style::default().fg(t.dim),
        )));
        out.push(Line::raw(""));
        for l in plain_wrap(&x.detail, dw) {
            out.push(Line::from(Span::styled(l, t.text())));
        }
        if !x.evidence.is_empty() {
            out.push(Line::raw(""));
            out.push(Line::from(vec![
                label("evidence", t),
                Span::styled(
                    "  written by programs on this machine: data, not instructions",
                    Style::default().fg(t.faint),
                ),
            ]));
            for e in x.evidence.iter().take(8) {
                out.push(Line::from(Span::styled(
                    truncate(e, dw),
                    Style::default().fg(t.dim).bg(t.bg),
                )));
            }
        }
        out.push(Line::raw(""));
        match &x.proposal {
            Some(pr) => {
                out.push(Line::from(vec![
                    label("the fix", t),
                    Span::styled(
                        format!(
                            "  drafted by {}{}",
                            pr.model.rsplit('/').next().unwrap_or(&pr.model),
                            pr.usd.map_or(String::new(), |u| format!(
                                " · {} of its own budget",
                                format_usd(Some(u))
                            ))
                        ),
                        Style::default().fg(t.violet),
                    ),
                ]));
                out.extend(markdown(&pr.text, dw, t));
            }
            None => {
                if let Some(n) = &x.draft_note {
                    out.push(Line::from(Span::styled(
                        format!("no draft: {n}"),
                        Style::default().fg(t.faint),
                    )));
                }
            }
        }
    }
    let out: Vec<Line> = out.into_iter().skip(p.scroll).collect();
    f.render_widget(Paragraph::new(out), detail);
    let mut k = vec![("⏎", "ask about it")];
    if p.selected().is_some_and(|x| x.proposal.is_some()) {
        k.insert(0, ("p", "run the fix"));
    }
    k.extend([
        ("a", "acknowledge"),
        ("x", "dismiss"),
        ("o", "reopen"),
        ("pgup/pgdn", "detail"),
        ("?", "ask about it, in words"),
    ]);
    f.render_widget(Paragraph::new(keys(&k, t)), foot);
}

// ── needs you ───────────────────────────────────────────────────────────────

fn needs(f: &mut Frame, area: Rect, v: &View, p: &NeedsPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let rows = p.rows();
    if rows.is_empty() {
        let lines = vec![
            Line::raw(""),
            Line::from(vec![
                Span::styled("✓ ", Style::default().fg(t.good)),
                Span::styled(
                    "Nothing needs you.",
                    Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(Span::styled(
                "Approvals Reeve asks for, fixes the drafter writes, and suggestions land here.",
                t.muted(),
            )),
        ];
        f.render_widget(Paragraph::new(lines), body);
        return;
    }
    let (list, detail) = list_detail(f, body, 42, t);
    let w = list.width as usize;
    let sel = p.selected();
    let mut lines = Vec::new();
    let mut sel_line = 0;
    let mut row =
        |lines: &mut Vec<Line<'static>>, need: Need, head: Vec<Span<'static>>, sub: String| {
            let on = sel == Some(need);
            let bg = if on { t.input } else { t.panel };
            if on {
                sel_line = lines.len();
            }
            let mut spans = vec![Span::styled(" ", Style::default().bg(bg))];
            spans.extend(head.into_iter().map(|s| {
                let st = s.style.bg(bg);
                s.style(st)
            }));
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            spans.push(Span::styled(
                " ".repeat(w.saturating_sub(used)),
                Style::default().bg(bg),
            ));
            lines.push(Line::from(spans));
            lines.push(Line::from(Span::styled(
                pad(&format!("   {}", truncate(&sub, w.saturating_sub(3))), w),
                t.ghost().bg(bg),
            )));
        };
    if let (true, Some(a)) = (p.asking, &v.approval) {
        lines.push(Line::from(label("approval", t)));
        let head = vec![
            Span::styled(
                "◐ ",
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ),
            tier_pill(a.req.tier, t),
            Span::styled(
                format!(" {}", truncate(&a.req.summary, w.saturating_sub(9))),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
        ];
        let sub = match &a.req.txn {
            Some(txn) => format!("verified change: {}", txn.goal),
            None => a.req.reasons.join(" · "),
        };
        row(&mut lines, Need::Approval, head, sub);
        lines.push(Line::raw(""));
    }
    if !p.fixes.is_empty() {
        lines.push(Line::from(vec![
            label("fixes ready", t),
            Span::styled("  drafted by the drafter", t.ghost()),
        ]));
        for (i, x) in p.fixes.iter().enumerate() {
            let (icon, c) = severity(x.severity, t);
            let head = vec![
                Span::styled(format!("{icon} "), Style::default().fg(c)),
                Span::styled(truncate(&x.title, w.saturating_sub(4)), t.text()),
            ];
            let sub = match &x.proposal {
                Some(pr) => format!(
                    "drafted {}{}",
                    pr.drafted_at.with_timezone(&Local).format("%a %H:%M"),
                    pr.usd
                        .map_or(String::new(), |u| format!(" · {}", format_usd(Some(u))))
                ),
                None => String::new(),
            };
            row(&mut lines, Need::Fix(i), head, sub);
        }
        lines.push(Line::raw(""));
    }
    if let Some(k) = &p.reboot {
        lines.push(Line::from(label("suggested", t)));
        let head = vec![
            Span::styled("◆ ", Style::default().fg(t.brass)),
            Span::styled(format!("reboot into kernel {k}"), t.text()),
        ];
        row(
            &mut lines,
            Need::Reboot,
            head,
            "installed, waiting for a reboot".into(),
        );
    }
    if let Some((used, cap)) = v.drafter {
        let n = 16usize;
        let mut spans = vec![Span::styled("drafter  ", t.ghost())];
        spans.extend(board::meter(used / cap.max(1e-9), n, t.violet, t));
        spans.push(Span::styled(
            format!("  ${used:.2} of ${cap:.2} today"),
            t.muted(),
        ));
        let room = (list.height as usize).saturating_sub(lines.len());
        if room > 1 {
            lines.extend((0..room - 1).map(|_| Line::raw("")));
        }
        lines.push(Line::from(spans));
    }
    f.render_widget(
        Paragraph::new(keep_in_view(lines, sel_line, list.height as usize)),
        list,
    );

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut k: Vec<(&str, &str)> = vec![("↑↓", "move")];
    match sel {
        Some(Need::Approval) => {
            if let Some(a) = &v.approval {
                out.push(Line::from(vec![
                    Span::styled(
                        "◐ approve",
                        Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    tier_pill(a.req.tier, t),
                    Span::styled(
                        format!("  {}", a.req.tier.name()),
                        Style::default().fg(t.tier(a.req.tier)),
                    ),
                ]));
                out.push(Line::raw(""));
                let mut card = crate::cards::card_lines(a, dw, t);
                // Its keys are the footer's here.
                card.truncate(card.len().saturating_sub(2));
                out.extend(card);
                if a.req.tier != reeve_core::policy::Tier::T3 {
                    k = vec![("⏎", "yes")];
                    if a.req.txn.is_some() {
                        k.push(("a", "yes to the rest of this change"));
                    } else if a.req.can_allow_turn {
                        k.push(("a", "yes to the rest of this request"));
                    }
                    if a.req.can_allow_session {
                        k.push(("s", "this session"));
                    }
                    k.push(("n", "no"));
                }
            }
        }
        Some(Need::Fix(i)) => {
            if let Some(x) = p.fixes.get(i) {
                let (icon, c) = severity(x.severity, t);
                out.push(Line::from(vec![
                    Span::styled(format!("{icon} "), Style::default().fg(c)),
                    Span::styled(
                        x.title.clone(),
                        Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
                    ),
                ]));
                out.push(Line::from(Span::styled(
                    format!(
                        "{} · {}× · last {}",
                        x.severity.as_str(),
                        x.count,
                        ago(x.last_seen)
                    ),
                    t.muted(),
                )));
                out.push(Line::raw(""));
                if let Some(pr) = &x.proposal {
                    out.push(Line::from(vec![
                        label("the fix", t),
                        Span::styled(
                            format!(
                                "  drafted by {}",
                                pr.model.rsplit('/').next().unwrap_or(&pr.model)
                            ),
                            Style::default().fg(t.violet),
                        ),
                    ]));
                    out.extend(markdown(&pr.text, dw, t));
                }
                k = vec![
                    ("⏎", "run the fix"),
                    ("d", "ask about it"),
                    ("a", "acknowledge"),
                    ("x", "dismiss"),
                ];
            }
        }
        Some(Need::Reboot) => {
            let kernel = p.reboot.clone().unwrap_or_default();
            out.push(Line::from(Span::styled(
                format!("Kernel {kernel} is installed and waits for a reboot."),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            )));
            out.push(Line::raw(""));
            for l in plain_wrap(
                "Until then the running kernel is the old one. Reeve won't reboot on its own: ask it what a reboot would interrupt and when is a good time.",
                dw,
            ) {
                out.push(Line::from(Span::styled(l, t.muted())));
            }
            k = vec![("⏎", "ask Reeve when")];
        }
        None => {}
    }
    let out: Vec<Line> = out.into_iter().skip(p.scroll).collect();
    f.render_widget(Paragraph::new(out), detail);
    f.render_widget(Paragraph::new(keys(&k, t)), foot);
}

// ── activity ────────────────────────────────────────────────────────────────

fn activity(f: &mut Frame, area: Rect, v: &View, p: &ReceiptsPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let (list, detail) = list_detail(f, body, 58, t);
    let w = list.width as usize;
    let mut lines = vec![Line::from(Span::styled(
        pad(
            &format!("{:<6}{:<7}{:<5}{:<15}what", "#", "time", "tier", "tool"),
            w,
        ),
        t.ghost(),
    ))];
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "No receipts yet: Reeve hasn't done anything.",
            t.muted(),
        )));
    }
    let h = list.height as usize;
    let start = p.sel.saturating_sub(h.saturating_sub(3));
    let what_w = w.saturating_sub(6 + 7 + 5 + 15 + 3);
    for (i, rc) in p
        .items
        .iter()
        .enumerate()
        .skip(start)
        .take(h.saturating_sub(1))
    {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.panel };
        let undone = p.undone.contains(&rc.seq);
        let (icon, ic) = match rc.outcome.status {
            Status::Ok => ("✓", t.good),
            Status::Error => ("✗", t.bad),
            Status::Denied => ("⊘", t.warn),
            Status::Refused => ("⊗", t.bad),
        };
        let mark = if rc.undoes.is_some() {
            "↺"
        } else if undone {
            "·"
        } else if rc.undo.is_some() && rc.outcome.status == Status::Ok {
            "↶"
        } else {
            " "
        };
        let when = rc.ts.with_timezone(&Local);
        let time = if (Local::now() - when).num_hours() < 20 {
            when.format("%H:%M").to_string()
        } else {
            when.format("%m-%d").to_string()
        };
        let tier = rc.tier.label().to_string();
        lines.push(Line::from(vec![
            Span::styled(
                pad(&rc.seq.to_string(), 6),
                Style::default()
                    .fg(if on { t.fg } else { t.faint })
                    .bg(bg)
                    .add_modifier(if on {
                        Modifier::BOLD
                    } else {
                        Modifier::empty()
                    }),
            ),
            Span::styled(pad(&time, 7), t.ghost().bg(bg)),
            Span::styled(pad(&tier, 5), Style::default().fg(t.tier(rc.tier)).bg(bg)),
            Span::styled(pad(&truncate(&rc.tool, 14), 15), t.muted().bg(bg)),
            Span::styled(format!("{icon} "), Style::default().fg(ic).bg(bg)),
            Span::styled(
                pad(&truncate(&rc.target(), what_w), what_w),
                (if undone {
                    t.ghost().add_modifier(Modifier::CROSSED_OUT)
                } else if on {
                    Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
                } else {
                    t.text()
                })
                .bg(bg),
            ),
            Span::styled(mark.to_string(), Style::default().fg(t.brass).bg(bg)),
        ]));
    }
    f.render_widget(Paragraph::new(lines), list);

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(rc) = p.selected() {
        let field = |k: &str, val: String, st: Style| {
            Line::from(vec![
                Span::styled(pad(k, 10), t.ghost()),
                Span::styled(truncate(&val, dw.saturating_sub(10)), st),
            ])
        };
        let undone = p.undone.contains(&rc.seq);
        let reversible = rc.undo.is_some() && !undone && rc.outcome.status == Status::Ok;
        out.push(split(
            vec![Span::styled(
                format!("receipt #{}", rc.seq),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            )],
            vec![
                tier_pill(rc.tier, t),
                Span::styled(
                    if reversible {
                        "  reversible"
                    } else if undone {
                        "  undone"
                    } else {
                        ""
                    },
                    Style::default().fg(t.good),
                ),
            ],
            dw,
        ));
        for l in plain_wrap(&format!("{} {}", rc.tool, rc.target()), dw)
            .into_iter()
            .take(3)
        {
            out.push(Line::from(Span::styled(l, Style::default().fg(t.code))));
        }
        let exit = rc
            .outcome
            .exit
            .map(|c| format!(" · exit {c}"))
            .unwrap_or_default();
        out.push(Line::from(Span::styled(
            truncate(
                &format!(
                    "{}{exit} · approved by {}",
                    rc.ts.with_timezone(&Local).format("%a %d %b %H:%M:%S"),
                    rc.approved_by
                ),
                dw,
            ),
            t.ghost(),
        )));
        out.push(Line::raw(""));
        out.push(field("result", rc.outcome.summary.clone(), t.text()));
        if let Some(why) = &rc.why {
            out.push(field(
                "why",
                why.clone(),
                t.muted().add_modifier(Modifier::ITALIC),
            ));
        }
        if !rc.reasons.is_empty() {
            out.push(field(
                "risk",
                rc.reasons.join(" · "),
                Style::default().fg(t.tier(rc.tier)),
            ));
        }
        if let Some(txn) = &rc.txn {
            out.push(field("change", txn.clone(), t.muted()));
        }
        out.push(Line::raw(""));
        out.push(field(
            "undo",
            match (&rc.undo, undone) {
                (Some(_), true) => "already undone".into(),
                (Some(_), false) => "u undoes it (asks first)".into(),
                (None, _) => "nothing to undo".into(),
            },
            t.muted(),
        ));
        if let Some(sp) = &rc.snapshot {
            let post = sp.post.map_or("?".to_string(), |n| n.to_string());
            out.push(field(
                "snapshot",
                format!("snapper {} #{}..#{post}", sp.config, sp.pre),
                Style::default().fg(t.teal),
            ));
        }
        out.push(Line::raw(""));
        out.push(field(
            "chain",
            format!("prev {}…", rc.prev.get(..12).unwrap_or("")),
            t.ghost(),
        ));
        out.push(field(
            "",
            format!("this {}…", rc.hash.get(..12).unwrap_or("")),
            t.muted(),
        ));
        out.push(Line::raw(""));
        for l in plain_wrap(&rc.args.to_string(), dw).into_iter().take(6) {
            out.push(Line::from(Span::styled(l, t.ghost())));
        }
    }
    let _ = v;
    f.render_widget(Paragraph::new(out), detail);
    let mut foot_line = keys(
        &[
            ("↑↓", "move"),
            ("u", "undo"),
            ("v", "verify the chain"),
            ("?", "ask about it"),
        ],
        t,
    );
    match &p.note {
        Some(Ok(m)) => foot_line.spans.push(Span::styled(
            format!("   ✓ {m}"),
            Style::default().fg(t.good),
        )),
        Some(Err(m)) => foot_line.spans.push(Span::styled(
            format!("   ✗ {m}"),
            Style::default().fg(t.bad),
        )),
        None => {}
    }
    f.render_widget(Paragraph::new(foot_line), foot);
}

// ── what changed ────────────────────────────────────────────────────────────

fn changed(f: &mut Frame, area: Rect, p: &SystemPanel, t: &Theme) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(range_line(p, t)), head);
    let Some(r) = p.report.as_deref() else {
        f.render_widget(
            Paragraph::new(Span::styled(
                "Reading packages, kernels, /etc, and units…",
                t.muted(),
            )),
            body,
        );
        return;
    };
    let d = &r.drift;
    let [left, _gap, right] = Layout::horizontal([
        Constraint::Percentage(50),
        Constraint::Length(2),
        Constraint::Min(20),
    ])
    .areas(body);
    // Packages, newest first.
    let pk = surface(f, left, t.inset, t.panel, t);
    let pw = pk.width as usize;
    let mut out = vec![Line::from(vec![
        label("packages", t),
        Span::styled(
            format!(
                "  {} installed · {} upgraded · {} removed",
                d.installed.len(),
                d.upgraded.len() + d.touched.len(),
                d.removed.len()
            ),
            t.ghost(),
        ),
    ])];
    let mut all: Vec<(
        &str,
        ratatui::style::Color,
        &reeve_observer::report::drift::Pkg,
    )> = Vec::new();
    all.extend(d.upgraded.iter().map(|x| ("↑", t.good, x)));
    all.extend(d.touched.iter().map(|x| ("↑", t.good, x)));
    all.extend(d.installed.iter().map(|x| ("+", t.brass, x)));
    all.extend(d.removed.iter().map(|x| ("−", t.bad, x)));
    all.sort_by(|a, b| b.2.at.cmp(&a.2.at));
    if all.is_empty() {
        out.push(Line::from(Span::styled("no package changes", t.muted())));
    }
    let name_w = (pw / 2).min(34);
    for (icon, c, x) in all.iter().take(pk.height.saturating_sub(1) as usize) {
        let ver = match (&x.from, &x.to) {
            (Some(a), Some(b)) => format!("{a} → {b}"),
            (None, Some(b)) => b.clone(),
            (Some(a), None) => a.clone(),
            (None, None) => String::new(),
        };
        out.push(Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(*c)),
            Span::styled(pad(&truncate(&x.name, name_w), name_w + 1), t.text()),
            Span::styled(truncate(&ver, pw.saturating_sub(name_w + 4)), t.muted()),
        ]));
    }
    f.render_widget(Paragraph::new(out), pk);
    // Kernel, /etc, services.
    let etc_h = (d.etc.len().min(10) as u16 + 4).min(right.height.saturating_sub(10).max(6));
    let [kern, etc, units] = Layout::vertical([
        Constraint::Length(5),
        Constraint::Length(etc_h),
        Constraint::Min(4),
    ])
    .areas(right);
    let k = surface(f, kern, t.inset, t.panel, t);
    let mut out = vec![Line::from(label("kernel", t))];
    match &d.reboot_for {
        Some(kv) => out.push(Line::from(vec![
            Span::styled("◆ ", Style::default().fg(t.brass)),
            Span::styled(format!("{kv} waits for a reboot"), t.text()),
        ])),
        None if !d.kernels.is_empty() => out.push(Line::from(Span::styled(
            format!("{} · running it", d.kernels.join(", ")),
            t.text(),
        ))),
        None => out.push(Line::from(Span::styled("no new kernel", t.muted()))),
    }
    f.render_widget(Paragraph::new(out), k);
    let e = surface(f, etc, t.inset, t.panel, t);
    let ew = e.width as usize;
    let mine = d.etc.iter().filter(|x| x.by_reeve.is_some()).count();
    let mut out = vec![Line::from(vec![
        label("/etc", t),
        Span::styled(
            format!(
                "  {} changed · {}",
                d.etc_total,
                if mine > 0 {
                    format!("{mine} by Reeve")
                } else {
                    "none by Reeve".into()
                }
            ),
            t.ghost(),
        ),
    ])];
    for x in d.etc.iter().take(e.height.saturating_sub(1) as usize) {
        let right = match x.by_reeve {
            Some(n) => vec![Span::styled(
                format!("Reeve #{n}"),
                Style::default().fg(t.brass),
            )],
            None => vec![Span::styled(
                x.at.with_timezone(&Local).format("%a %H:%M").to_string(),
                t.ghost(),
            )],
        };
        out.push(split(
            vec![Span::styled(
                truncate(&x.path, ew.saturating_sub(14)),
                t.text(),
            )],
            right,
            ew,
        ));
    }
    f.render_widget(Paragraph::new(out), e);
    let u = surface(f, units, t.inset, t.panel, t);
    let mut out = vec![Line::from(label("services", t))];
    if d.baseline.is_none() {
        out.push(Line::from(Span::styled(
            "needs a snapshot from before: reeved takes one a day",
            t.muted(),
        )));
    } else if d.units_enabled.is_empty() && d.units_disabled.is_empty() {
        out.push(Line::from(Span::styled(
            "none enabled or disabled",
            t.muted(),
        )));
    }
    for x in &d.units_enabled {
        out.push(Line::from(vec![
            Span::styled("+ ", Style::default().fg(t.good)),
            Span::styled(x.clone(), t.text()),
        ]));
    }
    for x in &d.units_disabled {
        out.push(Line::from(vec![
            Span::styled("− ", Style::default().fg(t.bad)),
            Span::styled(x.clone(), t.text()),
        ]));
    }
    f.render_widget(Paragraph::new(out), u);
    f.render_widget(
        Paragraph::new(keys(
            &[
                ("←→", "24h · 7d · 30d"),
                ("r", "the full report page"),
                ("?", "ask about it"),
            ],
            t,
        )),
        foot,
    );
}

/// `24h  7d  30d  ←→`, the window's range selector.
fn range_line(p: &SystemPanel, t: &Theme) -> Line<'static> {
    let mut range = vec![];
    for d in [1u32, 7, 30] {
        let on = d == p.days;
        let label = match d {
            1 => "24h",
            7 => "7d",
            _ => "30d",
        };
        range.push(Span::styled(
            format!(" {label} "),
            if on {
                t.key()
            } else {
                Style::default().fg(t.dim)
            },
        ));
        range.push(Span::raw(" "));
    }
    range.push(Span::styled(" ←→", t.ghost()));
    if p.loading {
        range.push(Span::styled("   gathering…", Style::default().fg(t.amber)));
    }
    Line::from(range)
}

// ── spend ───────────────────────────────────────────────────────────────────

/// A small sunken card: a faint label, then a value.
fn stat(f: &mut Frame, r: Rect, name: &str, value: Vec<Span<'static>>, t: &Theme) {
    let inner = surface(f, r, t.inset, t.panel, t);
    f.render_widget(
        Paragraph::new(vec![Line::from(label(name, t)), Line::from(value)]),
        inner,
    );
}

fn spend(f: &mut Frame, area: Rect, v: &View, p: &SpendPanel, t: &Theme) {
    let lines_data = p.lines();
    let total: f64 = lines_data.iter().map(|l| l.usd).sum();
    let calls: u64 = lines_data.iter().map(|l| l.calls).sum();
    let unpriced: u64 = lines_data.iter().map(|l| l.unpriced).sum();
    let month: f64 = p.month.iter().filter_map(|r| r.usd).sum();
    let input: u64 = p.month.iter().map(|r| r.usage.input_tokens).sum();
    let cached: u64 = p.month.iter().map(|r| r.usage.cached_tokens).sum();
    let [range_r, stats_r, charts_r, table_r, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(4),
        Constraint::Length(6),
        Constraint::Min(4),
        Constraint::Length(1),
    ])
    .areas(area);

    // Day, week, month.
    let mut range = vec![];
    for r in [
        crate::overlay::SpendRange::Day,
        crate::overlay::SpendRange::Week,
        crate::overlay::SpendRange::Month,
    ] {
        let on = r == p.range;
        range.push(Span::styled(
            format!(" {} ", r.label()),
            if on {
                t.key()
            } else {
                Style::default().fg(t.dim)
            },
        ));
        range.push(Span::raw(" "));
    }
    range.push(Span::styled(" ←→", t.ghost()));
    f.render_widget(Paragraph::new(Line::from(range)), range_r);

    // The numbers, as cards.
    let cards: [Rect; 5] = Layout::horizontal([Constraint::Ratio(1, 5); 5])
        .spacing(1)
        .areas(stats_r);
    let bold = |s: String| Span::styled(s, Style::default().fg(t.fg).add_modifier(Modifier::BOLD));
    stat(
        f,
        cards[0],
        &format!("this {}", p.range.label()),
        vec![
            bold(format_usd(Some(total))),
            Span::styled(
                format!(
                    "  {calls} calls{}",
                    if unpriced > 0 {
                        format!(" · {unpriced} unpriced")
                    } else {
                        String::new()
                    }
                ),
                t.muted(),
            ),
        ],
        t,
    );
    stat(
        f,
        cards[1],
        "this month",
        vec![
            bold(format_usd(Some(month))),
            Span::styled(format!("  {} calls", p.month.len()), t.muted()),
        ],
        t,
    );
    stat(
        f,
        cards[2],
        "cached",
        vec![
            Span::styled(
                if input > 0 {
                    format!("{:.0}%", cached as f64 / input as f64 * 100.0)
                } else {
                    "—".into()
                },
                Style::default().fg(t.good).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  of input tokens", t.muted()),
        ],
        t,
    );
    let (daily, session, monthly, warn) = p.caps;
    let caps = if daily + session + monthly == 0.0 {
        "no caps".to_string()
    } else {
        let c = |v: f64| {
            if v > 0.0 {
                format!("${v:.2}")
            } else {
                "off".into()
            }
        };
        format!("day {} · month {}", c(daily), c(monthly))
    };
    let lim = if warn > 0.0 && v.totals.today.usd >= warn {
        vec![
            Span::styled(format!(" over ${warn:.2} "), t.pill(t.warn)),
            Span::styled(format!(" {caps}"), t.muted()),
        ]
    } else {
        vec![Span::styled(
            format!("{caps} · warn at ${warn:.2}"),
            t.muted(),
        )]
    };
    stat(f, cards[3], "limits", lim, t);
    let drafter = match (p.drafter_cap, v.drafter) {
        (Some(cap), Some((used, _))) => {
            let w = (cards[4].width as usize)
                .saturating_sub(4 + 12)
                .clamp(4, 16);
            let mut spans = board::meter(used / cap.max(1e-9), w, t.violet, t);
            spans.push(Span::styled(format!(" ${used:.2}/{cap:.2}"), t.muted()));
            spans
        }
        _ => vec![Span::styled("off", t.muted())],
    };
    stat(f, cards[4], "drafter budget", drafter, t);

    // By hour (or day), by role, by model.
    let charts: [Rect; 3] = Layout::horizontal([Constraint::Ratio(1, 3); 3])
        .spacing(1)
        .areas(charts_r);
    let when = surface(f, charts[0], t.inset, t.panel, t);
    let ww = when.width as usize;
    let day = p.range == crate::overlay::SpendRange::Day;
    let buckets: Vec<f64> = if day {
        let mut b = vec![0.0; 24];
        for r in &p.records {
            if let Ok(h) =
                r.ts.with_timezone(&Local)
                    .format("%H")
                    .to_string()
                    .parse::<usize>()
            {
                b[h.min(23)] += r.usd.unwrap_or(0.0);
            }
        }
        b
    } else {
        let mut days: Vec<(String, f64)> = Vec::new();
        for r in &p.records {
            let d = r.ts.with_timezone(&Local).format("%m-%d").to_string();
            match days.iter_mut().find(|x| x.0 == d) {
                Some(x) => x.1 += r.usd.unwrap_or(0.0),
                None => days.push((d, r.usd.unwrap_or(0.0))),
            }
        }
        days.into_iter().map(|x| x.1).collect()
    };
    let peak = buckets.iter().copied().fold(0.0, f64::max);
    let bw = if day {
        24.min(ww)
    } else {
        buckets.len().clamp(1, ww)
    };
    let mut out = vec![split(
        vec![label(if day { "by hour" } else { "by day" }, t)],
        vec![Span::styled(
            format!("{} peak", format_usd(Some(peak))),
            t.ghost(),
        )],
        ww,
    )];
    // Two rows tall: the top row carries what spills over the bottom one.
    let doubled: Vec<f64> = buckets.iter().map(|x| x * 2.0).collect();
    let top: Vec<f64> = doubled.iter().map(|x| (x - peak).max(0.0)).collect();
    out.push(Line::from(Span::styled(
        board::spark(&top, bw, peak),
        Style::default().fg(t.brass),
    )));
    out.push(Line::from(Span::styled(
        board::spark(
            &doubled.iter().map(|x| x.min(peak)).collect::<Vec<_>>(),
            bw,
            peak,
        ),
        Style::default().fg(t.brass),
    )));
    if day {
        out.push(Line::from(Span::styled(
            truncate("00    06    12    18    24", ww),
            t.ghost(),
        )));
    }
    f.render_widget(Paragraph::new(out), when);

    let mut roles: Vec<(String, f64, u64)> = Vec::new();
    for l in &lines_data {
        match roles.iter_mut().find(|r| r.0 == l.role) {
            Some(r) => {
                r.1 += l.usd;
                r.2 += l.calls;
            }
            None => roles.push((l.role.clone(), l.usd, l.calls)),
        }
    }
    roles.sort_by(|a, b| b.1.total_cmp(&a.1));
    let by_role = surface(f, charts[1], t.inset, t.panel, t);
    let rw = by_role.width as usize;
    let mut out = vec![Line::from(label(
        &format!("by role · {}", p.range.label()),
        t,
    ))];
    let top = roles.iter().map(|r| r.1).fold(0.0, f64::max).max(1e-9);
    let bar_w = rw.saturating_sub(9 + 14).max(4);
    for (r, usd, n) in roles.iter().take(3) {
        let mut spans = vec![Span::styled(pad(r, 9), t.muted())];
        spans.extend(board::meter(usd / top, bar_w, board::role_color(r, t), t));
        spans.push(Span::styled(
            format!(" {:>8}", format_usd(Some(*usd))),
            t.text(),
        ));
        spans.push(Span::styled(format!(" {n:>4}"), t.ghost()));
        out.push(Line::from(spans));
    }
    if roles.is_empty() {
        out.push(Line::from(Span::styled("nothing spent", t.muted())));
    }
    f.render_widget(Paragraph::new(out), by_role);

    let mut models: Vec<(String, f64, u64)> = Vec::new();
    for r in &p.month {
        match models.iter_mut().find(|m| m.0 == r.model) {
            Some(m) => {
                m.1 += r.usd.unwrap_or(0.0);
                m.2 += 1;
            }
            None => models.push((r.model.clone(), r.usd.unwrap_or(0.0), 1)),
        }
    }
    models.sort_by(|a, b| b.1.total_cmp(&a.1));
    let by_model = surface(f, charts[2], t.inset, t.panel, t);
    let mw = by_model.width as usize;
    let mut out = vec![Line::from(label("by model · month", t))];
    let palette = [t.brass, t.violet, t.good, t.amber];
    for (i, (m, usd, n)) in models.iter().take(3).enumerate() {
        out.push(split(
            vec![
                Span::styled("■ ", Style::default().fg(palette[i % palette.len()])),
                Span::styled(truncate(m, mw.saturating_sub(18)), t.muted()),
            ],
            vec![
                Span::styled(format_usd(Some(*usd)), t.text()),
                Span::styled(format!(" {n:>4}"), t.ghost()),
            ],
            mw,
        ));
    }
    f.render_widget(Paragraph::new(out), by_model);

    // The statement.
    let table = surface(f, table_r, t.inset, t.panel, t);
    let w = table.width as usize;
    let cols = [7usize, 9, 0, 6, 9, 9, 7, 10, 11];
    let what_w = w.saturating_sub(cols.iter().sum::<usize>() + cols.len());
    let cell = |s: String, w: usize, right: bool| {
        if right {
            format!("{s:>w$} ")
        } else {
            format!("{} ", pad(&truncate(&s, w), w))
        }
    };
    let head = [
        "time", "who", "what", "calls", "in", "cached", "out", "cost", "running",
    ];
    let mut head_s = String::new();
    for (i, h) in head.iter().enumerate() {
        let cw = if i == 2 { what_w } else { cols[i] };
        head_s.push_str(&cell((*h).to_string(), cw, i >= 3));
    }
    let mut rows = vec![Line::from(Span::styled(head_s, t.ghost()))];
    let mut running = 0.0;
    let sel = p.sel.min(lines_data.len().saturating_sub(1));
    for (i, l) in lines_data.iter().enumerate() {
        running += l.usd;
        let on = i == sel;
        let bg = if on { t.input } else { t.inset };
        let when = l.first.with_timezone(&Local);
        let time = if p.range == crate::overlay::SpendRange::Day {
            when.format("%H:%M").to_string()
        } else {
            when.format("%a %d").to_string()
        };
        let what = match l.role.as_str() {
            "chat" if l.session == p.current => "this session".to_string(),
            "chat" => "a session".to_string(),
            "drafter" => "a draft".to_string(),
            "reflect" => "learning from a session".to_string(),
            "orders" => "a standing order".to_string(),
            r => r.to_string(),
        };
        let what = format!(
            "{what} · {}",
            l.model.rsplit('/').next().unwrap_or(&l.model)
        );
        let cells: [(String, ratatui::style::Color); 9] = [
            (time, t.dim),
            (l.role.clone(), board::role_color(&l.role, t)),
            (what, t.fg),
            (l.calls.to_string(), t.dim),
            (format_tokens(l.usage.input_tokens), t.fg),
            (format_tokens(l.usage.cached_tokens), t.good),
            (format_tokens(l.usage.output_tokens), t.fg),
            (
                if l.unpriced > 0 {
                    format!("≥{}", format_usd(Some(l.usd)))
                } else {
                    format_usd(Some(l.usd))
                },
                t.fg,
            ),
            (format_usd(Some(running)), t.dim),
        ];
        rows.push(Line::from(
            cells
                .into_iter()
                .enumerate()
                .map(|(k, (s, c))| {
                    let cw = if k == 2 { what_w } else { cols[k] };
                    let mut st = Style::default().fg(c).bg(bg);
                    if on && k == 7 {
                        st = st.add_modifier(Modifier::BOLD);
                    }
                    Span::styled(cell(s, cw, k >= 3), st)
                })
                .collect::<Vec<_>>(),
        ));
    }
    if lines_data.is_empty() {
        rows.push(Line::from(Span::styled(
            "no model calls in this range",
            t.muted(),
        )));
    }
    // Keep the selected row on screen under the header.
    let h = (table.height as usize).saturating_sub(1).max(1);
    let start = (sel + 1).saturating_sub(h);
    let shown: Vec<Line> = rows[..1]
        .iter()
        .cloned()
        .chain(rows[1..].iter().skip(start).cloned())
        .collect();
    f.render_widget(Paragraph::new(shown), table);

    let mut foot_line = keys(
        &[
            ("↑↓", "rows"),
            ("⏎", "open"),
            ("e", "export CSV"),
            ("←→", "day · week · month"),
            ("?", "ask about it"),
        ],
        t,
    );
    match &p.note {
        Some(Ok(n)) => foot_line.spans.push(Span::styled(
            format!("   ✓ {n}"),
            Style::default().fg(t.good),
        )),
        Some(Err(n)) => foot_line.spans.push(Span::styled(
            format!("   ✗ {n}"),
            Style::default().fg(t.bad),
        )),
        None => {}
    }
    f.render_widget(Paragraph::new(foot_line), foot);
}

// ── orders ──────────────────────────────────────────────────────────────────

fn short_ago(t: DateTime<Utc>) -> String {
    ago(t).trim_end_matches(" ago").to_string()
}

fn orders(f: &mut Frame, area: Rect, v: &View, p: &OrdersPanel, t: &Theme) {
    let [body, note, foot] = Layout::vertical([
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let (list, detail) = list_detail(f, body, 44, t);
    let w = list.width as usize;
    let on_count = p.items.iter().filter(|o| o.enabled).count();
    let mut lines = vec![split(
        vec![label(&format!("{} orders", p.items.len()), t)],
        vec![Span::styled(
            if on_count == 0 {
                "all off".to_string()
            } else {
                format!("{on_count} on")
            },
            t.ghost(),
        )],
        w,
    )];
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled("No orders yet.", t.muted())));
        lines.push(Line::raw(""));
        for l in [
            "A standing order is work Reeve does on its own: when reeved",
            "finds something, or on a schedule you pick. It can only do",
            "what the order allows and spend what it budgets.",
        ] {
            lines.push(Line::from(Span::styled(
                truncate(l, w.saturating_sub(1)),
                t.ghost(),
            )));
        }
    }
    let mut sel_line = 0;
    for (i, o) in p.items.iter().enumerate() {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.panel };
        if on {
            sel_line = lines.len();
        }
        let matching = v
            .findings
            .iter()
            .filter(|x| x.is_live() && o.wants(x))
            .count();
        let mut right = Vec::new();
        if matching > 0 {
            right.push(Span::styled(
                format!(" matches {matching} open "),
                t.pill(t.warn),
            ));
        }
        let rw: usize = right.iter().map(|s| s.content.width()).sum();
        let mut first = vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                if o.enabled { "● " } else { "○ " },
                Style::default()
                    .fg(if o.enabled { t.good } else { t.faint })
                    .bg(bg),
            ),
            Span::styled(
                pad(
                    &truncate(&o.name, w.saturating_sub(4 + rw + 1)),
                    w.saturating_sub(3 + rw),
                ),
                if on {
                    Style::default()
                        .fg(t.fg)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    t.text().bg(bg)
                },
            ),
        ];
        first.extend(right);
        lines.push(Line::from(first));
        let st = p.states.get(&o.id).cloned().unwrap_or_default();
        let mut sub = Vec::new();
        if let Some(s) = &o.trigger.schedule {
            sub.push(s.clone());
        }
        if !o.trigger.findings.is_empty() {
            sub.push("on findings".into());
        }
        sub.push(format!("up to {}", o.scope.max_tier.label()));
        sub.push(format!("${:.2} a run", o.budget.per_run_usd));
        sub.push(st.runs.last().map_or("never ran".to_string(), |r| {
            format!("{} {} ago", r.status, short_ago(r.ts))
        }));
        lines.push(Line::from(Span::styled(
            pad(
                &format!("   {}", truncate(&sub.join(" · "), w.saturating_sub(3))),
                w,
            ),
            t.ghost().bg(bg),
        )));
        lines.push(Line::raw(""));
    }
    for (id, e) in &p.bad {
        lines.push(Line::from(Span::styled(
            format!(" ✗ {id}: {}", truncate(e, w.saturating_sub(6))),
            Style::default().fg(t.bad),
        )));
    }
    let room = (list.height as usize).saturating_sub(lines.len());
    if room >= 3 {
        lines.extend((0..room - 3).map(|_| Line::raw("")));
        lines.push(Line::from(vec![
            board::key("n", t),
            Span::styled(" new order", t.text()),
        ]));
        lines.push(Line::from(Span::styled(
            "   a form walks you through it, one step at a time",
            t.ghost(),
        )));
    }
    f.render_widget(
        Paragraph::new(keep_in_view(lines, sel_line, list.height as usize)),
        list,
    );

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(o) = p.selected() {
        out.push(split(
            vec![Span::styled(
                o.name.clone(),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            )],
            vec![Span::styled(
                if o.enabled { " ● on " } else { " ○ off " },
                t.pill(if o.enabled { t.good } else { t.dim }),
            )],
            dw,
        ));
        out.push(Line::raw(""));
        for l in plain_wrap(o.task.trim(), dw).into_iter().take(6) {
            out.push(Line::from(Span::styled(l, t.text())));
        }
        out.push(Line::raw(""));
        let field = |k: &str, val: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(pad(k, 10), t.ghost())];
            spans.extend(val);
            Line::from(spans)
        };
        let mut when = Vec::new();
        if let Some(s) = &o.trigger.schedule {
            when.push(s.clone());
        }
        for x in &o.trigger.findings {
            when.push(x.clone());
        }
        for (i, wl) in when.iter().enumerate() {
            out.push(field(
                if i == 0 { "when" } else { "" },
                vec![Span::styled(
                    truncate(wl, dw.saturating_sub(10)),
                    Style::default().fg(t.code),
                )],
            ));
        }
        for x in v
            .findings
            .iter()
            .filter(|x| x.is_live() && o.wants(x))
            .take(3)
        {
            let (icon, c) = severity(x.severity, t);
            out.push(field(
                "",
                vec![
                    Span::styled(format!("{icon} "), Style::default().fg(c)),
                    Span::styled(truncate(&x.title, dw.saturating_sub(14)), t.text()),
                    Span::styled(" · open now", t.muted()),
                ],
            ));
        }
        out.push(Line::raw(""));
        out.push(field(
            "may use",
            vec![
                Span::styled(o.scope.tools.join(", "), t.text()),
                Span::styled(", up to ", t.muted()),
                tier_pill(o.scope.max_tier, t),
            ],
        ));
        for (i, c) in o.scope.commands.iter().enumerate() {
            out.push(field(
                if i == 0 { "commands" } else { "" },
                vec![Span::styled(
                    truncate(c, dw.saturating_sub(10)),
                    Style::default().fg(t.code),
                )],
            ));
        }
        for (i, c) in o.scope.paths.iter().enumerate() {
            out.push(field(
                if i == 0 { "paths" } else { "" },
                vec![Span::styled(
                    truncate(c, dw.saturating_sub(10)),
                    Style::default().fg(t.code),
                )],
            ));
        }
        let st = p.states.get(&o.id).cloned().unwrap_or_default();
        out.push(field(
            "budget",
            vec![
                Span::styled(format!("${:.2} a run", o.budget.per_run_usd), t.text()),
                Span::styled(
                    format!(
                        " · {} a day ({} today) · {}h between",
                        o.budget.runs_per_day,
                        st.runs_today(),
                        o.budget.cooldown_hours
                    ),
                    t.muted(),
                ),
            ],
        ));
        if let Some(m) = o.model.as_ref().or(o.connection.as_ref()) {
            out.push(field("model", vec![Span::styled(m.clone(), t.muted())]));
        }
        out.push(Line::raw(""));
        if !p.extra.is_empty() {
            out.push(Line::from(label("sudoers lines", t)));
            for l in &p.extra {
                out.push(Line::from(Span::styled(
                    truncate(l, dw),
                    Style::default().fg(t.code),
                )));
            }
        } else if st.runs.is_empty() {
            out.push(field(
                "runs",
                vec![Span::styled(
                    "none yet · each run leaves receipts in F4",
                    t.muted(),
                )],
            ));
        } else {
            out.push(Line::from(label("runs", t)));
            for run in st.runs.iter().rev().take(6) {
                let c = match run.status.as_str() {
                    "done" => t.good,
                    "blocked" | "rolled_back" => t.warn,
                    "skipped" => t.dim,
                    _ => t.bad,
                };
                let receipts = if run.receipts.is_empty() {
                    String::new()
                } else {
                    format!(
                        " #{}",
                        run.receipts
                            .iter()
                            .map(u64::to_string)
                            .collect::<Vec<_>>()
                            .join(" #")
                    )
                };
                out.push(Line::from(vec![
                    Span::styled(pad(&format!("{} ago", short_ago(run.ts)), 9), t.ghost()),
                    Span::styled(pad(&run.status, 8), Style::default().fg(c)),
                    Span::styled(
                        truncate(
                            &format!(
                                "{}{receipts}",
                                run.summary
                                    .lines()
                                    .find(|l| !l.trim().is_empty())
                                    .unwrap_or("")
                            ),
                            dw.saturating_sub(17),
                        ),
                        t.muted(),
                    ),
                ]));
            }
        }
    }
    f.render_widget(Paragraph::new(out), detail);
    f.render_widget(Paragraph::new(note_line(&p.note, t)), note);
    f.render_widget(
        Paragraph::new(keys(
            &[
                ("space", "on / off"),
                ("r", "run now"),
                ("e", "edit"),
                ("n", "new"),
                ("E", "edit the file"),
                ("s", "sudoers"),
                ("D", "delete"),
                ("u", "undo"),
                ("?", "ask about it"),
            ],
            t,
        )),
        foot,
    );
}

/// A panel's last note: ✓ done, or ! what went wrong.
fn note_line(note: &Option<Result<String, String>>, t: &Theme) -> Line<'static> {
    match note {
        Some(Ok(m)) => Line::from(Span::styled(format!("✓ {m}"), Style::default().fg(t.good))),
        Some(Err(m)) => Line::from(Span::styled(format!("! {m}"), Style::default().fg(t.warn))),
        None => Line::raw(""),
    }
}

// ── memory ──────────────────────────────────────────────────────────────────

fn memory(f: &mut Frame, area: Rect, p: &MemoryPanel, t: &Theme) {
    use reeve_core::memory::{Layer, NoteStatus};
    let [layers, body, note, foot] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(area);
    let mut tabs = Vec::new();
    for (i, l) in Layer::ALL.iter().enumerate() {
        let notes = p.notes.get(i).map_or(0, Vec::len);
        let fresh = p.notes.get(i).map_or(0, |v| {
            v.iter()
                .filter(|n| matches!(n.status, NoteStatus::New | NoteStatus::Pending))
                .count()
        });
        tabs.push(Span::styled(
            format!(
                " {} {notes}{} ",
                l.dir(),
                if fresh > 0 {
                    format!(" · {fresh} new")
                } else {
                    String::new()
                }
            ),
            if i == p.tab {
                t.key()
            } else {
                Style::default().fg(t.dim)
            },
        ));
        tabs.push(Span::raw("  "));
    }
    tabs.push(Span::styled(
        format!(" skills {} ", p.skills.len()),
        if p.on_skills() {
            t.key()
        } else {
            Style::default().fg(t.dim)
        },
    ));
    tabs.push(Span::raw("  "));
    tabs.push(Span::styled("←→", t.ghost()));
    f.render_widget(Paragraph::new(Line::from(tabs)), layers);
    if p.on_skills() {
        skills_tab(f, body, note, foot, p, t);
        return;
    }

    let (list, detail) = list_detail(f, body, 60, t);
    let w = list.width as usize;
    let cur = p.current();
    let mut lines = Vec::new();
    if cur.is_empty() {
        let empty = match Layer::ALL[p.tab] {
            Layer::Facts => "No facts yet. `s` surveys the machine (read-only).",
            Layer::Runbooks => {
                "No runbooks yet. They're written after fixes that were checked to work."
            }
            Layer::Preferences => "No preferences. Tell Reeve how you want things done.",
            Layer::Baselines => "Baselines come from reeved as it learns what normal looks like.",
        };
        lines.push(Line::from(Span::styled(empty, t.muted())));
    }
    let mut sel_line = 0;
    for (i, n) in cur.iter().enumerate() {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.panel };
        if on {
            sel_line = lines.len();
        }
        let (mark, mc) = match n.status {
            NoteStatus::New => ("● new ", t.brass),
            NoteStatus::Pending => ("? ask ", t.warn),
            NoteStatus::Active => ("✓     ", t.faint),
            NoteStatus::Retired => ("✗ old ", t.faint),
        };
        let track = if n.layer == Layer::Runbooks {
            format!("{}✓ {}✗ ", n.successes, n.failures)
        } else {
            String::new()
        };
        let src = n.source.split(':').next().unwrap_or("").to_string();
        let right = format!("{track}{src}");
        let title_w = w.saturating_sub(1 + 6 + right.width() + 2);
        let title_style = if n.status == NoteStatus::Retired {
            t.ghost().add_modifier(Modifier::CROSSED_OUT)
        } else if on {
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
        } else {
            t.text()
        };
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(mark, Style::default().fg(mc).bg(bg)),
            Span::styled(
                pad(&truncate(&n.title, title_w), title_w + 1),
                title_style.bg(bg),
            ),
            Span::styled(
                format!("{right:>rw$} ", rw = right.width()),
                t.ghost().bg(bg),
            ),
        ]));
    }
    f.render_widget(
        Paragraph::new(keep_in_view(lines, sel_line, list.height as usize)),
        list,
    );

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(n) = p.selected() {
        for l in plain_wrap(&n.title, dw) {
            out.push(Line::from(Span::styled(
                l,
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            )));
        }
        if !n.tags.is_empty() {
            let mut tags = Vec::new();
            for tag in &n.tags {
                tags.push(Span::styled(format!(" #{tag} "), t.pill(t.dim)));
                tags.push(Span::raw(" "));
            }
            out.push(Line::raw(""));
            out.push(Line::from(tags));
        }
        out.push(Line::raw(""));
        let field = |k: &str, val: Vec<Span<'static>>| {
            let mut spans = vec![Span::styled(pad(k, 12), t.ghost())];
            spans.extend(val);
            Line::from(spans)
        };
        out.push(field(
            "from",
            vec![Span::styled(
                format!(
                    "{} · {}",
                    n.source,
                    n.observed.with_timezone(&Local).format("%Y-%m-%d %H:%M")
                ),
                t.muted(),
            )],
        ));
        let mut conf = board::meter(f64::from(n.confidence), 10, t.brass, t);
        conf.push(Span::styled(
            format!(" {:.0}%", n.confidence * 100.0),
            t.text(),
        ));
        out.push(field("confidence", conf));
        if let Some(os) = &n.os {
            out.push(field(
                "applies to",
                vec![Span::styled(os.clone(), t.muted())],
            ));
        }
        let state = match n.status {
            NoteStatus::New => ("● new · not used in a session yet", t.brass),
            NoteStatus::Pending => ("? waits for you to accept it", t.warn),
            NoteStatus::Active => ("✓ in use", t.good),
            NoteStatus::Retired => ("✗ retired", t.faint),
        };
        out.push(field(
            "state",
            vec![Span::styled(state.0, Style::default().fg(state.1))],
        ));
        if let Some(rule) = &n.rule {
            let live = n.status.in_use();
            out.push(field(
                "rule",
                vec![
                    Span::styled(
                        rule.clone(),
                        Style::default()
                            .fg(if live { t.bad } else { t.dim })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        if live {
                            "  enforced"
                        } else {
                            "  not in effect until accepted"
                        },
                        t.ghost(),
                    ),
                ],
            ));
        }
        out.push(Line::raw(""));
        for raw in n.body.lines() {
            for l in plain_wrap(raw, dw) {
                out.push(Line::from(Span::styled(l, t.muted())));
            }
        }
    }
    f.render_widget(Paragraph::new(out), detail);
    f.render_widget(Paragraph::new(note_line(&p.note, t)), note);
    f.render_widget(
        Paragraph::new(keys(
            &[
                ("a", "accept"),
                ("x", "retire"),
                ("e", "edit"),
                ("D", "delete"),
                ("s", "survey"),
                ("r", "reflect now"),
                ("?", "ask about it"),
            ],
            t,
        )),
        foot,
    );
}

/// The skills tab of memory: the owner's skills, and the selected one's steps.
fn skills_tab(f: &mut Frame, body: Rect, note: Rect, foot: Rect, p: &MemoryPanel, t: &Theme) {
    let (list, detail) = list_detail(f, body, 60, t);
    let w = list.width as usize;
    let mut lines = Vec::new();
    if p.skills.is_empty() {
        for l in plain_wrap(
            "No skills yet. A skill is a job you want done your way, by name. Press n and tell Reeve what to save, or say \"save that as a skill\" after it does something.",
            w.saturating_sub(1),
        ) {
            lines.push(Line::from(Span::styled(l, t.muted())));
        }
    }
    let mut sel_line = 0;
    let id_w = p
        .skills
        .iter()
        .map(|s| s.id.len() + 1)
        .max()
        .unwrap_or(0)
        .min(w / 2);
    for (i, s) in p.skills.iter().enumerate() {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.panel };
        if on {
            sel_line = lines.len();
        }
        let about_w = w.saturating_sub(id_w + 3);
        lines.push(Line::from(vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(
                pad(&truncate(&format!("/{}", s.id), id_w), id_w + 1),
                Style::default()
                    .fg(t.brass)
                    .bg(bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                pad(&truncate(&s.description, about_w), about_w + 1),
                if on { t.text() } else { t.muted() }.bg(bg),
            ),
        ]));
    }
    for (id, why) in &p.skill_errors {
        lines.push(Line::from(Span::styled(
            truncate(&format!(" ! {id}.md isn't listed: {why}"), w),
            Style::default().fg(t.warn),
        )));
    }
    f.render_widget(
        Paragraph::new(keep_in_view(lines, sel_line, list.height as usize)),
        list,
    );

    let dw = detail.width as usize;
    let mut out: Vec<Line<'static>> = Vec::new();
    if let Some(s) = p.selected_skill() {
        out.push(Line::from(vec![
            Span::styled(
                s.name.clone(),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(format!(" /{} ", s.id), t.pill(t.brass)),
        ]));
        for l in plain_wrap(&s.description, dw) {
            out.push(Line::from(Span::styled(l, t.text())));
        }
        out.push(Line::raw(""));
        for raw in s.body.lines() {
            if raw.trim().is_empty() {
                out.push(Line::raw(""));
            }
            for l in plain_wrap(raw, dw) {
                out.push(Line::from(Span::styled(l, t.muted())));
            }
        }
        out.push(Line::raw(""));
        for l in plain_wrap("A skill grants nothing: each step still asks as usual.", dw) {
            out.push(Line::from(Span::styled(l, t.ghost())));
        }
    }
    f.render_widget(Paragraph::new(out), detail);
    f.render_widget(Paragraph::new(note_line(&p.note, t)), note);
    f.render_widget(
        Paragraph::new(keys(
            &[
                ("⏎", "run it"),
                ("e", "edit"),
                ("D", "delete"),
                ("n", "new: tell Reeve"),
            ],
            t,
        )),
        foot,
    );
}

// ── system ──────────────────────────────────────────────────────────────────

fn system(f: &mut Frame, area: Rect, v: &View, p: &SystemPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let side_w = (body.width * 34 / 100).clamp(36, 60);
    let [charts, _gap, side] = Layout::horizontal([
        Constraint::Min(30),
        Constraint::Length(2),
        Constraint::Length(side_w),
    ])
    .areas(body);

    // Left: the window's charts, two by two.
    let mut out: Vec<Line<'static>> = vec![range_line(p, t), Line::raw("")];
    match p.report.as_deref() {
        None => out.push(Line::from(Span::styled(
            "Reading reeved's minute readings, packages, and /etc…",
            Style::default().fg(t.dim),
        ))),
        Some(r) if r.coverage == 0.0 => {
            out.push(Line::from(Span::styled(
                "No readings yet: reeved records a row every minute once it's running (/observer).",
                Style::default().fg(t.dim),
            )));
        }
        Some(r) => {
            let cw = (charts.width as usize).saturating_sub(3) / 2;
            // Two rows of charts share the height left under the range line.
            let ch = ((charts.height as usize).saturating_sub(6) / 2)
                .saturating_sub(4)
                .clamp(3, 12);
            let series = |f: &dyn Fn(&reeve_observer::report::Point) -> f64| -> Vec<f32> {
                r.points
                    .iter()
                    .map(|(_, p)| p.as_ref().map_or(0.0, |p| f(p) as f32))
                    .collect()
            };
            let now = &v.snap;
            let gauge = |name: &str,
                         big: String,
                         sub: String,
                         data: Vec<f32>,
                         max: f32|
             -> Vec<Line<'static>> {
                let mut l = vec![Line::from(vec![
                    label(name, t),
                    Span::raw("  "),
                    Span::styled(big, Style::default().fg(t.fg).add_modifier(Modifier::BOLD)),
                    Span::styled(format!("  {sub}"), Style::default().fg(t.dim)),
                ])];
                // Scale the samples to the chart's width.
                let n = cw * 2;
                let step = data.len() as f32 / n.max(1) as f32;
                let sampled: Vec<f32> = (0..n)
                    .map(|i| {
                        let a = (i as f32 * step) as usize;
                        let b = (((i + 1) as f32 * step) as usize)
                            .max(a + 1)
                            .min(data.len());
                        data[a.min(data.len().saturating_sub(1))..b]
                            .iter()
                            .copied()
                            .fold(0.0, f32::max)
                    })
                    .collect();
                for row in braille(&sampled, max.max(1e-3), cw, ch, t) {
                    l.push(Line::from(row));
                }
                l
            };
            let peak = |s: &reeve_observer::report::Stat| s.max;
            let temp_now = now.temp_c.map_or("—".to_string(), |c| format!("{c:.0}°C"));
            let net_top = r
                .points
                .iter()
                .filter_map(|(_, p)| p.map(|p| p.rx))
                .fold(0.0, f64::max) as f32;
            let grid = [
                gauge(
                    "cpu",
                    now.cpu_pct.map_or("—".into(), |c| format!("{c:.0}%")),
                    format!("avg {:.0} · peak {:.0}%", r.cpu.avg, peak(&r.cpu)),
                    series(&|p| p.cpu),
                    (peak(&r.cpu) as f32 * 1.1).max(10.0),
                ),
                gauge(
                    "memory",
                    if now.mem_total > 0 {
                        format!("{:.0}%", now.mem_used as f64 / now.mem_total as f64 * 100.0)
                    } else {
                        "—".into()
                    },
                    format!("avg {:.0}% · swap {:.0}%", r.mem.avg, r.swap.avg),
                    series(&|p| p.mem),
                    100.0,
                ),
                gauge(
                    "temperature",
                    temp_now,
                    r.temp.map_or(String::new(), |s| {
                        format!("avg {:.0} · peak {:.0}°C", s.avg, s.max)
                    }),
                    series(&|p| p.temp.unwrap_or(0.0)),
                    100.0,
                ),
                gauge(
                    "network in",
                    now.net_rx_bps.map_or("—".into(), crate::draw::rate),
                    format!("peak {}", crate::draw::rate(f64::from(net_top))),
                    series(&|p| p.rx),
                    net_top.max(1.0),
                ),
            ];
            for pair in grid.chunks(2) {
                let h = pair.iter().map(Vec::len).max().unwrap_or(0);
                for i in 0..h {
                    let mut spans: Vec<Span<'static>> =
                        pair[0].get(i).map(|l| l.spans.clone()).unwrap_or_default();
                    let lw: usize = spans.iter().map(|s| s.content.width()).sum();
                    spans.push(Span::raw(" ".repeat((cw + 3).saturating_sub(lw))));
                    if let Some(b) = pair.get(1) {
                        spans.extend(b.get(i).map(|l| l.spans.clone()).unwrap_or_default());
                    }
                    out.push(Line::from(spans));
                }
                let axis = match p.days {
                    1 => ("24h ago", "now"),
                    7 => ("7 days ago", "now"),
                    _ => ("30 days ago", "now"),
                };
                let one = format!(
                    "{}{:>w$}",
                    axis.0,
                    axis.1,
                    w = cw.saturating_sub(axis.0.width())
                );
                out.push(Line::from(Span::styled(
                    format!("{one}   {one}"),
                    Style::default().fg(t.faint),
                )));
                out.push(Line::raw(""));
            }
            if r.chart_since > r.since {
                out.push(Line::from(Span::styled(
                    format!(
                        "reeved has been recording since {}; the charts start there",
                        r.chart_since
                            .with_timezone(&Local)
                            .format("%a %-d %b %H:%M")
                    ),
                    Style::default().fg(t.faint),
                )));
            }
        }
    }
    f.render_widget(Paragraph::new(out), charts);
    // Right: now, and the headlines.
    let side = surface(f, side, t.inset, t.panel, t);
    let sw = side.width as usize;
    let mut s: Vec<Line<'static>> = vec![Line::from(label("now", t))];
    s.extend(system_lines(v, t, sw));
    if let Some(r) = p.report.as_deref() {
        if !r.disks.is_empty() {
            s.push(Line::raw(""));
            s.push(Line::from(Span::styled(
                format!(
                    "{:<10}{:>5}  {:>10}  {}",
                    "disks", "used", "grows/day", "full in"
                ),
                t.ghost(),
            )));
            for d in &r.disks {
                let grows = d.per_day * d.total as f64;
                let full = match d.days_to_full {
                    Some(x) if x < 60.0 => format!("{x:.0} days"),
                    Some(x) if x < 730.0 => format!("{:.0} months", x / 30.0),
                    Some(_) | None => "years".to_string(),
                };
                s.push(Line::from(vec![
                    Span::styled(pad(&truncate(&d.mount, 9), 10), t.text()),
                    Span::styled(
                        format!("{:>4.0}%", d.now * 100.0),
                        Style::default().fg(board::level(d.now, t)),
                    ),
                    Span::styled(
                        format!(
                            "  {:>10}  ",
                            if grows.abs() < 1e6 {
                                "—".to_string()
                            } else {
                                format!(
                                    "{}{}",
                                    if grows < 0.0 { "−" } else { "+" },
                                    crate::draw::gib(grows.abs() as u64)
                                )
                            }
                        ),
                        t.muted(),
                    ),
                    Span::styled(
                        full.clone(),
                        if full.ends_with("days") {
                            Style::default().fg(t.warn)
                        } else {
                            t.muted()
                        },
                    ),
                ]));
            }
        }
        s.push(Line::raw(""));
        s.push(Line::from(label("headlines", t)));
        for h in r.headlines.iter().take(6) {
            let (icon, c) = match h.tone {
                reeve_observer::report::Tone::Bad => ("●", t.bad),
                reeve_observer::report::Tone::Warn => ("▲", t.warn),
                reeve_observer::report::Tone::Info => ("◆", t.teal),
                reeve_observer::report::Tone::Good => ("✓", t.good),
            };
            s.push(Line::from(vec![
                Span::styled(format!("{icon} "), Style::default().fg(c)),
                Span::styled(truncate(&h.text, sw.saturating_sub(2)), t.text()),
            ]));
        }
    }
    f.render_widget(Paragraph::new(s), side);
    f.render_widget(
        Paragraph::new(keys(
            &[
                ("←→", "24h · 7d · 30d"),
                ("r", "the full report page"),
                ("?", "ask about it"),
            ],
            t,
        )),
        foot,
    );
}
