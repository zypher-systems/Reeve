//! The tabs besides the ledger: findings, orders, spend, memory, system.
//! Each is a full screen under the tab bar, with its keys along the bottom.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use chrono::{DateTime, Local, Utc};
use reeve_core::findings::{Finding, FindingStatus, Severity};
use reeve_core::spend::{format_tokens, format_usd};

use crate::draw::{braille, markdown, pad, plain_wrap, system_lines, truncate};
use crate::overlay::{FindingsPanel, Overlay, SpendPanel, SystemPanel};
use crate::theme::Theme;
use crate::view::{Tab, View};

/// Draw the tab's screen.
pub fn draw(f: &mut Frame, area: Rect, v: &View, t: &Theme, tab: Tab) {
    let Some(top) = v.overlays.first() else {
        return;
    };
    match (tab, top) {
        (Tab::Findings, Overlay::Findings(p)) => findings(f, area, p, t),
        (Tab::Orders, Overlay::Orders(p)) => crate::panels::orders(f, area, p, t),
        (Tab::Memory, Overlay::Memory(p)) => crate::panels::memory(f, area, p, t),
        (Tab::Spend, Overlay::Spend(p)) => spend(f, area, v, p, t),
        (Tab::System, Overlay::System(p)) => system(f, area, v, p, t),
        _ => {}
    }
}

fn label(s: &str, t: &Theme) -> Span<'static> {
    Span::styled(
        s.to_uppercase(),
        Style::default().fg(t.faint).add_modifier(Modifier::BOLD),
    )
}

fn keys(pairs: &[(&str, &str)], t: &Theme) -> Line<'static> {
    let mut spans = Vec::new();
    for (k, l) in pairs {
        spans.push(Span::styled(
            (*k).to_string(),
            Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            format!(" {l}    "),
            Style::default().fg(t.dim),
        ));
    }
    Line::from(spans)
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
    match sev {
        Severity::Critical => ("●", t.bad),
        Severity::Warning => ("▲", t.warn),
        Severity::Info => ("◆", t.teal),
    }
}

// ── findings ────────────────────────────────────────────────────────────────

fn findings(f: &mut Frame, area: Rect, p: &FindingsPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let list_w = (body.width * 38 / 100).clamp(34, 60);
    let [list, _gap, detail] = Layout::horizontal([
        Constraint::Length(list_w),
        Constraint::Length(3),
        Constraint::Min(20),
    ])
    .areas(body);
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
    lines.push(Line::from(label(&format!("open · {}", live.len()), t)));
    if p.items.is_empty() {
        lines.push(Line::from(Span::styled(
            "Nothing found. reeved reports here; /observer starts it.",
            Style::default().fg(t.dim),
        )));
    }
    let row = |i: usize, x: &Finding| {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.bg };
        let (icon, c) = severity(x.severity, t);
        let count = if x.count > 1 {
            format!("{}×", x.count)
        } else {
            String::new()
        };
        let fix = if x.proposal.is_some() { " fix" } else { "" };
        let title_w = w.saturating_sub(4 + count.width() + fix.width() + 1);
        let title_style = if !x.is_live() {
            Style::default().fg(t.faint)
        } else if on {
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
        } else {
            t.text()
        };
        Line::from(vec![
            Span::styled(
                if on { "▸ " } else { "  " },
                Style::default().fg(t.brass).bg(bg),
            ),
            Span::styled(
                format!("{icon} "),
                Style::default()
                    .fg(if x.is_live() { c } else { t.faint })
                    .bg(bg),
            ),
            Span::styled(
                pad(&truncate(&x.title, title_w), title_w),
                title_style.bg(bg),
            ),
            Span::styled(fix, Style::default().fg(t.teal).bg(bg)),
            Span::styled(format!(" {count}"), Style::default().fg(t.dim).bg(bg)),
        ])
    };
    for (i, x) in &live {
        lines.push(row(*i, x));
    }
    if !rest.is_empty() {
        lines.push(Line::raw(""));
        lines.push(Line::from(label(&format!("earlier · {}", rest.len()), t)));
        for (i, x) in rest.iter().take(40) {
            lines.push(row(*i, x));
        }
    }
    // Keep the selection in view.
    let h = list.height as usize;
    let sel_line = lines
        .iter()
        .position(|l| l.spans.first().is_some_and(|s| s.content.starts_with('▸')))
        .unwrap_or(0);
    let skip = sel_line.saturating_sub(h.saturating_sub(2));
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        list,
    );
    // A rule between the panes.
    let rule: Vec<Line> = (0..body.height)
        .map(|_| Line::from(Span::styled(" │", Style::default().fg(t.border))))
        .collect();
    f.render_widget(
        Paragraph::new(rule),
        Rect {
            x: list.right(),
            width: 3,
            ..body
        },
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
                out.push(Line::from(vec![
                    Span::styled("▎ ", Style::default().fg(t.border)),
                    Span::styled(
                        truncate(e, dw.saturating_sub(2)),
                        Style::default().fg(t.dim),
                    ),
                ]));
            }
        }
        out.push(Line::raw(""));
        match &x.proposal {
            Some(pr) => {
                out.push(Line::from(vec![
                    label("drafted fix", t),
                    Span::styled(
                        format!(
                            "  by the drafter ({}) · read-only{}",
                            pr.model.rsplit('/').next().unwrap_or(&pr.model),
                            pr.usd.map_or(String::new(), |u| format!(
                                " · {} of its own budget",
                                format_usd(Some(u))
                            ))
                        ),
                        Style::default().fg(t.teal),
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
    let mut hints = vec![("⏎", "ask about it")];
    if p.selected().is_some_and(|x| x.proposal.is_some()) {
        hints.insert(0, ("p", "run the fix in the ledger"));
    }
    hints.extend([
        ("a", "acknowledge"),
        ("x", "dismiss"),
        ("o", "reopen"),
        ("pgup/pgdn", "detail"),
    ]);
    f.render_widget(Paragraph::new(keys(&hints, t)), foot);
}

// ── spend ───────────────────────────────────────────────────────────────────

fn spend(f: &mut Frame, area: Rect, v: &View, p: &SpendPanel, t: &Theme) {
    let w = area.width as usize;
    let lines_data = p.lines();
    let total: f64 = lines_data.iter().map(|l| l.usd).sum();
    let calls: u64 = lines_data.iter().map(|l| l.calls).sum();
    let unpriced: u64 = lines_data.iter().map(|l| l.unpriced).sum();
    let month: f64 = p.month.iter().filter_map(|r| r.usd).sum();
    let input: u64 = p.month.iter().map(|r| r.usage.input_tokens).sum();
    let cached: u64 = p.month.iter().map(|r| r.usage.cached_tokens).sum();
    let mut out: Vec<Line<'static>> = Vec::new();

    // Range selector.
    let mut range = vec![];
    for r in [
        crate::overlay::SpendRange::Day,
        crate::overlay::SpendRange::Week,
        crate::overlay::SpendRange::Month,
    ] {
        let on = r == p.range;
        range.push(Span::styled(
            r.label(),
            if on {
                Style::default()
                    .fg(t.fg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(t.dim)
            },
        ));
        range.push(Span::styled("  ", Style::default()));
    }
    range.push(Span::styled("←→", Style::default().fg(t.brass)));
    out.push(Line::from(range));
    out.push(Line::raw(""));
    let big = |s: String| Span::styled(s, Style::default().fg(t.fg).add_modifier(Modifier::BOLD));
    out.push(Line::from(vec![
        label(p.range.label(), t),
        Span::raw("  "),
        big(format_usd(Some(total))),
        Span::styled(
            format!("  {calls} calls · {unpriced} unpriced"),
            Style::default().fg(t.dim),
        ),
        Span::raw("      "),
        label("month", t),
        Span::raw("  "),
        big(format_usd(Some(month))),
        Span::styled(
            format!("  {} calls", p.month.len()),
            Style::default().fg(t.dim),
        ),
        Span::raw("      "),
        label("cached", t),
        Span::raw("  "),
        Span::styled(
            if input > 0 {
                format!("{:.0}%", cached as f64 / input as f64 * 100.0)
            } else {
                "—".into()
            },
            Style::default().fg(t.teal).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" of input tokens this month", Style::default().fg(t.dim)),
    ]));
    let (daily, session, monthly, warn) = p.caps;
    let caps = if daily + session + monthly == 0.0 {
        "no caps set".to_string()
    } else {
        let c = |v: f64| {
            if v > 0.0 {
                format!("${v:.2}")
            } else {
                "off".into()
            }
        };
        format!(
            "session {} · day {} · month {}",
            c(session),
            c(daily),
            c(monthly)
        )
    };
    let mut lim = vec![
        label("limits", t),
        Span::styled(format!("  {caps}"), t.text()),
        Span::styled(
            format!(" · warn at ${warn:.2}"),
            Style::default().fg(t.faint),
        ),
    ];
    if let (Some(cap), Some((used, _))) = (p.drafter_cap, v.drafter) {
        let n = 16usize;
        let on = ((used / cap.max(1e-9)).clamp(0.0, 1.0) * n as f64).round() as usize;
        lim.push(Span::styled(
            format!("   drafter {} of ${cap:.2} ", format_usd(Some(used))),
            Style::default().fg(t.dim),
        ));
        lim.push(Span::styled("█".repeat(on), Style::default().fg(t.teal)));
        lim.push(Span::styled(
            "▁".repeat(n - on),
            Style::default().fg(t.faint),
        ));
    }
    out.push(Line::from(lim));
    out.push(Line::raw(""));

    // By role, and by model this month.
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
    let half = w / 2;
    let bar_w = half.saturating_sub(9 + 10 + 10 + 4).max(4);
    let top = roles.iter().map(|r| r.1).fold(0.0, f64::max).max(1e-9);
    let role_c = |r: &str| match r {
        "chat" => t.fg,
        "drafter" => t.teal,
        "reflect" => t.copper,
        "orders" => t.user,
        _ => t.dim,
    };
    let mut left = vec![Line::from(label(
        &format!("by role · {}", p.range.label()),
        t,
    ))];
    for (r, usd, n) in &roles {
        let cells = ((usd / top) * bar_w as f64).round() as usize;
        left.push(Line::from(vec![
            Span::styled(pad(r, 9), Style::default().fg(role_c(r))),
            Span::styled(
                pad(&"█".repeat(cells.max(1)), bar_w),
                Style::default().fg(role_c(r)),
            ),
            Span::styled(format!("{:>10}", format_usd(Some(*usd))), t.text()),
            Span::styled(
                format!("{:>10}", format!("{n} calls")),
                Style::default().fg(t.dim),
            ),
        ]));
    }
    if roles.is_empty() {
        left.push(Line::from(Span::styled(
            "nothing spent",
            Style::default().fg(t.dim),
        )));
    }
    let mut right = vec![Line::from(label("by model · month", t))];
    for (m, usd, n) in models.iter().take(5) {
        right.push(Line::from(vec![
            Span::styled(
                pad(
                    &truncate(m, half.saturating_sub(22)),
                    half.saturating_sub(22),
                ),
                t.text(),
            ),
            Span::styled(format!("{:>10}", format_usd(Some(*usd))), t.text()),
            Span::styled(
                format!("{:>10}", format!("{n} calls")),
                Style::default().fg(t.dim),
            ),
        ]));
    }
    let rows = left.len().max(right.len());
    for i in 0..rows {
        let mut spans: Vec<Span<'static>> =
            left.get(i).map(|l| l.spans.clone()).unwrap_or_default();
        let lw: usize = spans.iter().map(|s| s.content.width()).sum();
        spans.push(Span::raw(" ".repeat(half.saturating_sub(lw))));
        spans.extend(right.get(i).map(|l| l.spans.clone()).unwrap_or_default());
        out.push(Line::from(spans));
    }
    out.push(Line::raw(""));

    // The statement.
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
        "TIME", "WHO", "WHAT", "CALLS", "IN", "CACHED", "OUT", "COST", "RUNNING",
    ];
    let mut head_s = String::new();
    for (i, h) in head.iter().enumerate() {
        let cw = if i == 2 { what_w } else { cols[i] };
        head_s.push_str(&cell((*h).to_string(), cw, i >= 3));
    }
    out.push(Line::from(Span::styled(
        head_s,
        Style::default().fg(t.faint).add_modifier(Modifier::BOLD),
    )));
    let table_top = out.len();
    let mut running = 0.0;
    let sel = p.sel.min(lines_data.len().saturating_sub(1));
    for (i, l) in lines_data.iter().enumerate() {
        running += l.usd;
        let on = i == sel;
        let bg = if on { t.input } else { t.bg };
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
            (l.role.clone(), role_c(&l.role)),
            (what, t.fg),
            (l.calls.to_string(), t.dim),
            (format_tokens(l.usage.input_tokens), t.fg),
            (format_tokens(l.usage.cached_tokens), t.teal),
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
        let spans: Vec<Span<'static>> = cells
            .into_iter()
            .enumerate()
            .map(|(k, (s, c))| {
                let cw = if k == 2 { what_w } else { cols[k] };
                Span::styled(cell(s, cw, k >= 3), Style::default().fg(c).bg(bg))
            })
            .collect();
        out.push(Line::from(spans));
    }
    if lines_data.is_empty() {
        out.push(Line::from(Span::styled(
            "  no model calls in this range",
            Style::default().fg(t.dim),
        )));
    }
    // Keep the selected statement row on screen: the table scrolls, the
    // summary above it stays.
    let h = area.height.saturating_sub(1) as usize;
    let room = h.saturating_sub(table_top).max(1);
    let start = (sel + 1).saturating_sub(room);
    let shown: Vec<Line> = out[..table_top]
        .iter()
        .cloned()
        .chain(out[table_top..].iter().skip(start).cloned())
        .collect();
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    f.render_widget(Paragraph::new(shown), body);
    let note = match &p.note {
        Some(Ok(n)) => Some(Span::styled(n.clone(), Style::default().fg(t.good))),
        Some(Err(n)) => Some(Span::styled(n.clone(), Style::default().fg(t.bad))),
        None => None,
    };
    let mut foot_line = keys(
        &[
            ("↑↓", "rows"),
            ("⏎", "open"),
            ("e", "export CSV"),
            ("←→", "day · week · month"),
        ],
        t,
    );
    if let Some(n) = note {
        foot_line.spans.push(n);
    }
    f.render_widget(Paragraph::new(foot_line), foot);
}

// ── system ──────────────────────────────────────────────────────────────────

fn system(f: &mut Frame, area: Rect, v: &View, p: &SystemPanel, t: &Theme) {
    let [body, foot] = Layout::vertical([Constraint::Min(3), Constraint::Length(1)]).areas(area);
    let side_w = (body.width * 34 / 100).clamp(36, 56);
    let [charts, _gap, side] = Layout::horizontal([
        Constraint::Min(30),
        Constraint::Length(3),
        Constraint::Length(side_w),
    ])
    .areas(body);

    // Left: the window's charts, two by two.
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut range = vec![];
    for d in [1u32, 7, 30] {
        let on = d == p.days;
        let label = match d {
            1 => "24h",
            7 => "7d",
            _ => "30d",
        };
        range.push(Span::styled(
            label,
            if on {
                Style::default()
                    .fg(t.fg)
                    .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            } else {
                Style::default().fg(t.dim)
            },
        ));
        range.push(Span::raw("  "));
    }
    range.push(Span::styled("←→", Style::default().fg(t.brass)));
    if p.loading {
        range.push(Span::styled("   gathering…", Style::default().fg(t.amber)));
    }
    out.push(Line::from(range));
    out.push(Line::raw(""));
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
    let rule: Vec<Line> = (0..body.height)
        .map(|_| Line::from(Span::styled(" │", Style::default().fg(t.border))))
        .collect();
    f.render_widget(
        Paragraph::new(rule),
        Rect {
            x: charts.right(),
            width: 3,
            ..body
        },
    );

    // Right: now, what changed, headlines.
    let sw = side.width as usize;
    let mut s: Vec<Line<'static>> = vec![Line::from(label("now", t))];
    s.extend(system_lines(v, t, sw));
    if let Some(r) = p.report.as_deref() {
        let d = &r.drift;
        s.push(Line::raw(""));
        s.push(Line::from(label(
            &format!(
                "what changed · {}",
                match p.days {
                    1 => "24h".to_string(),
                    n => format!("{n} days"),
                }
            ),
            t,
        )));
        let pk = d.installed.len() + d.upgraded.len() + d.touched.len();
        let mut change = |c: ratatui::style::Color, head: String, rest: String| {
            s.push(Line::from(vec![
                Span::styled(head, Style::default().fg(c).add_modifier(Modifier::BOLD)),
                Span::styled(
                    format!(" {}", truncate(&rest, sw.saturating_sub(8))),
                    t.text(),
                ),
            ]));
        };
        if pk > 0 {
            let what = if d.touched.is_empty() {
                format!(
                    "packages: {} installed, {} upgraded",
                    d.installed.len(),
                    d.upgraded.len()
                )
            } else if pk == 1 {
                "package installed or upgraded".into()
            } else {
                "packages installed or upgraded".into()
            };
            change(t.teal, format!("↑{pk}"), what);
        }
        if !d.removed.is_empty() {
            change(
                t.bad,
                format!("−{}", d.removed.len()),
                "packages removed".into(),
            );
        }
        if let Some(k) = &d.reboot_for {
            change(t.warn, "↻".into(), format!("kernel {k} waits for a reboot"));
        } else if !d.kernels.is_empty() {
            change(
                t.dim,
                "·".into(),
                format!("kernel {} · running it", d.kernels.join(", ")),
            );
        }
        if d.etc_total > 0 {
            let mine = d.etc.iter().filter(|e| e.by_reeve.is_some()).count();
            change(
                t.fg,
                d.etc_total.to_string(),
                format!(
                    "files changed in /etc{}",
                    if mine > 0 {
                        format!(" · {mine} by Reeve")
                    } else {
                        " · none by Reeve".into()
                    }
                ),
            );
        }
        if d.baseline.is_some() {
            if d.units_enabled.is_empty() && d.units_disabled.is_empty() {
                change(t.dim, "·".into(), "no services enabled or disabled".into());
            } else {
                change(
                    t.fg,
                    format!("{}", d.units_enabled.len() + d.units_disabled.len()),
                    "services enabled or disabled".into(),
                );
            }
        } else {
            change(
                t.faint,
                "—".into(),
                "services: needs a snapshot from before".into(),
            );
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
            &[("←→", "24h · 7d · 30d"), ("r", "the full report page")],
            t,
        )),
        foot,
    );
}
