//! The frame: tabs along the top, the tab's screen, and a status line.
//! The ledger (the conversation and everything Reeve did, as one
//! timeline) is home; findings, orders, spend, memory, and system are the
//! other tabs. Floating panels (providers, receipts, ⌃K) draw over them.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use reeve_core::findings::Severity;

use crate::theme::Theme;
use crate::view::{Tab, View};

pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draw one frame.
pub fn draw(f: &mut Frame, v: &View, t: &Theme) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(t.bg)), area);
    let [tabs, body, status] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_tabs(f, tabs, v, t);
    draw_status(f, status, v, t);
    let body = Rect {
        x: body.x + 2,
        width: body.width.saturating_sub(4),
        y: body.y + 1,
        height: body.height.saturating_sub(1),
    };
    match v.tab() {
        Tab::Ledger => crate::ledger::draw(f, body, v, t),
        tab => crate::screens::draw(f, body, v, t, tab),
    }
    crate::panels::draw_overlay(f, v, t);
}

// ── tabs & status ───────────────────────────────────────────────────────────

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

fn draw_tabs(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let active = v.tab();
    let mut left = vec![
        Span::raw(" "),
        Span::styled(
            "reeve",
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
    ];
    let mut mark = (0, 0);
    for tab in Tab::ALL {
        let on = tab == active;
        let start = width(&left);
        left.push(Span::styled(
            format!("{} ", tab.key()),
            Style::default().fg(t.faint),
        ));
        left.push(Span::styled(
            tab.label(),
            if on {
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(t.dim)
            },
        ));
        if tab == Tab::Findings {
            let live = v.findings.len();
            if live > 0 {
                let worst = v.findings.iter().map(|f| f.severity).max();
                let c = if worst == Some(Severity::Critical) {
                    t.bad
                } else {
                    t.warn
                };
                left.push(Span::styled(format!(" {live}"), Style::default().fg(c)));
            }
        }
        if on {
            mark = (start, width(&left) - start);
        }
        left.push(Span::raw("   "));
    }
    let right = status_sentence(v, t);
    let row = Rect { height: 1, ..area };
    put_split(f, row, left, right, t.bg);
    if area.height > 1 {
        let w = area.width as usize;
        let mut rule = vec![Span::styled(
            "─".repeat(mark.0.min(w)),
            Style::default().fg(t.border),
        )];
        rule.push(Span::styled(
            "━".repeat(mark.1.min(w.saturating_sub(mark.0))),
            Style::default().fg(t.brass),
        ));
        rule.push(Span::styled(
            "─".repeat(w.saturating_sub(mark.0 + mark.1)),
            Style::default().fg(t.border),
        ));
        f.render_widget(
            Paragraph::new(Line::from(rule)),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
    }
}

/// `nexus · all quiet, except swap 99%`: the one line that says how the
/// machine is.
pub(crate) fn status_sentence(v: &View, t: &Theme) -> Vec<Span<'static>> {
    let s = &v.snap;
    let crit = v
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    let warn = v
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();
    let mut issues: Vec<(String, Color)> = Vec::new();
    if crit > 0 {
        issues.push((
            format!("{crit} need{} you", if crit == 1 { "s" } else { "" }),
            t.bad,
        ));
    }
    if s.swap_total > 0 && s.swap_used as f64 / s.swap_total as f64 >= 0.9 {
        issues.push((
            format!(
                "swap {:.0}%",
                s.swap_used as f64 / s.swap_total as f64 * 100.0
            ),
            t.bad,
        ));
    }
    for d in s.disks.iter().filter(|d| d.ratio() >= 0.9) {
        issues.push((format!("{} {:.0}%", d.mount, d.ratio() * 100.0), t.bad));
    }
    if crit == 0 && warn > 0 {
        issues.push((
            format!("{warn} finding{}", if warn == 1 { "" } else { "s" }),
            t.warn,
        ));
    }
    let mut out = Vec::new();
    if !v.host.hostname.is_empty() {
        out.push(Span::styled(
            v.host.hostname.to_lowercase(),
            Style::default().fg(t.dim),
        ));
        out.push(Span::styled(" · ", Style::default().fg(t.faint)));
    }
    if issues.is_empty() {
        out.push(Span::styled("all quiet", Style::default().fg(t.dim)));
    } else {
        if crit == 0 {
            out.push(Span::styled(
                "all quiet, except ",
                Style::default().fg(t.dim),
            ));
        }
        for (i, (text, c)) in issues.into_iter().enumerate() {
            if i > 0 {
                out.push(Span::styled(", ", Style::default().fg(t.dim)));
            }
            out.push(Span::styled(text, Style::default().fg(c)));
        }
    }
    out.push(Span::raw(" "));
    out
}

fn draw_status(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let tab = v.tab();
    let mode = tab.label().to_uppercase();
    let mut left = vec![
        Span::raw(" "),
        Span::styled(
            mode,
            Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    let dim = |s: String| Span::styled(s, Style::default().fg(t.dim));
    if v.busy {
        let spin = SPINNER[(v.frame / 2) as usize % SPINNER.len()];
        left.push(Span::styled(
            format!("{spin} working  "),
            Style::default().fg(t.amber),
        ));
    }
    match tab {
        Tab::Ledger => {
            left.push(dim(format!("session {}", v.session.label())));
            left.push(dim(format!(" · today {}", v.totals.today.label())));
        }
        Tab::Findings => {
            let drafted = v.findings.iter().filter(|f| f.proposal.is_some()).count();
            left.push(dim(format!("{} open", v.findings.len())));
            if drafted > 0 {
                left.push(dim(format!(
                    " · {drafted} fix{} drafted",
                    if drafted == 1 { "" } else { "es" }
                )));
            }
        }
        Tab::Spend => left.push(dim(format!(
            "today {} · month {}",
            v.totals.today.label(),
            v.totals.month.label()
        ))),
        Tab::Memory => left.push(dim(format!("{} in use · {} new", v.memory.0, v.memory.1))),
        Tab::System => {
            let mut facts = vec![v.host.hostname.clone(), v.host.os_short.clone()];
            if v.snap.uptime_secs > 0 {
                facts.push(format!("up {}", uptime(v.snap.uptime_secs)));
            }
            facts.retain(|f| !f.is_empty());
            left.push(dim(facts.join(" · ")));
        }
        Tab::Orders => left.push(dim("what Reeve may do unattended".into())),
    }
    let mut right = Vec::new();
    if !v.model.is_empty() {
        right.push(dim(short_model(&v.model)));
        right.push(Span::styled(" · ", Style::default().fg(t.faint)));
    }
    if let Some(p) = &v.privacy {
        if p.level != reeve_core::privacy::Level::Off {
            right.push(dim(format!("▣ {} masked", p.entries.len())));
            right.push(Span::styled(" · ", Style::default().fg(t.faint)));
        }
    }
    right.push(dim("reeved ".into()));
    right.push(if v.observer_alive {
        Span::styled("●", Style::default().fg(t.good))
    } else {
        Span::styled("○", Style::default().fg(t.faint))
    });
    right.push(Span::raw("  "));
    if v.yolo {
        // The badge breathes so nobody forgets that nothing is being asked.
        let pulse = if v.animate {
            ((v.frame as f32 / 8.0).sin() + 1.0) / 2.0
        } else {
            1.0
        };
        right.push(Span::styled(
            " YOLO ",
            Style::default()
                .fg(Color::Black)
                .bg(t.mix(t.copper, t.bad, pulse))
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        right.push(Span::styled("TIERED", Style::default().fg(t.dim)));
    }
    right.push(Span::raw("   "));
    right.push(Span::styled(
        "⌃K",
        Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
    ));
    right.push(Span::styled(" everything ", Style::default().fg(t.dim)));
    // The mode badge must always show: context, then the model, give way.
    let avail = area.width as usize;
    while left.len() > 3 && width(&left) + width(&right) > avail {
        left.pop();
    }
    while right.len() > 4 && width(&left) + width(&right) > avail {
        right.remove(0);
    }
    put_split(f, area, left, right, t.bg);
}

/// Left spans, right spans flush right, on one row.
pub(crate) fn put_split(f: &mut Frame, area: Rect, left: Vec<Span>, right: Vec<Span>, bg: Color) {
    let rw: usize = right.iter().map(|s| s.content.width()).sum();
    let lw: usize = left.iter().map(|s| s.content.width()).sum();
    let mut spans = left;
    let gap = (area.width as usize).saturating_sub(lw + rw);
    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(right);
    }
    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(bg)),
        area,
    );
}

// ── floating panels ─────────────────────────────────────────────────────────

pub(crate) fn panel<'a>(title: &'a str, t: &Theme, hot: bool) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(if hot { t.border_hot } else { t.border }))
        .title(Line::from(vec![
            Span::styled(
                " ◇ ",
                Style::default().fg(if hot { t.brass } else { t.faint }),
            ),
            Span::styled(format!("{title} "), t.muted()),
        ]))
        .style(Style::default().bg(t.panel))
}

pub(crate) fn system_lines(v: &View, t: &Theme, w: usize) -> Vec<Line<'static>> {
    let s = &v.snap;
    let label_w = s
        .disks
        .iter()
        .map(|d| d.mount.width())
        .max()
        .unwrap_or(0)
        .clamp(4, 10);
    let lbl = |x: &str| Span::styled(pad(x, label_w + 1), t.muted());
    let value_w = 11;
    let bar_w = w.saturating_sub(label_w + 1 + value_w + 1).max(4);
    let mut out = Vec::new();

    // CPU: a two-row braille area chart of the last minutes.
    let cpu_now = s
        .cpu_pct
        .map_or("  …".to_string(), |c| format!("{c:>3.0}%"));
    let hist: Vec<f32> = v.cpu_hist.iter().copied().collect();
    let chart = braille(&hist, 100.0, bar_w, 2, t);
    for (i, row) in chart.into_iter().enumerate() {
        let mut spans = vec![lbl(if i == 0 { "cpu" } else { "" })];
        spans.extend(row);
        if i == 0 {
            spans.push(Span::styled(
                format!(" {cpu_now}"),
                Style::default().fg(t.level(f64::from(s.cpu_pct.unwrap_or(0.0)) / 100.0)),
            ));
        } else {
            spans.push(Span::styled(format!(" {:.2}", s.load[0]), t.ghost()));
        }
        out.push(Line::from(spans));
    }

    if s.mem_total > 0 {
        let r = s.mem_used as f64 / s.mem_total as f64;
        let mut spans = vec![lbl("mem")];
        spans.extend(bar(r, bar_w, t));
        spans.push(Span::styled(
            format!(" {}/{}", gib(s.mem_used), gib(s.mem_total)),
            t.muted(),
        ));
        out.push(Line::from(spans));
    }
    if s.swap_total > 0 {
        let r = s.swap_used as f64 / s.swap_total as f64;
        let mut spans = vec![lbl("swap")];
        spans.extend(bar(r, bar_w, t));
        spans.push(Span::styled(
            format!(" {}/{}", gib(s.swap_used), gib(s.swap_total)),
            t.ghost(),
        ));
        out.push(Line::from(spans));
    }
    for d in &s.disks {
        let r = d.ratio();
        let mut spans = vec![lbl(&truncate(&d.mount, label_w))];
        spans.extend(bar(r, bar_w, t));
        spans.push(Span::styled(
            format!(" {:>3.0}%", r * 100.0),
            Style::default().fg(t.level(r)),
        ));
        spans.push(Span::styled(format!(" {}", gib(d.avail)), t.ghost()));
        if r >= 0.9 {
            spans.push(Span::styled(" ⚠", Style::default().fg(t.bad)));
        }
        out.push(Line::from(spans));
    }

    let mut facts = vec![lbl(if s.temp_c.is_some() { "temp" } else { "net" })];
    if let Some(temp) = s.temp_c {
        let r = f64::from(temp) / 100.0;
        facts.push(Span::styled(
            format!("{temp:.0}°C"),
            Style::default().fg(t.level(r)),
        ));
        facts.push(Span::styled("  ", t.ghost()));
    }
    if let (Some(rx), Some(tx)) = (s.net_rx_bps, s.net_tx_bps) {
        facts.push(Span::styled("↓", Style::default().fg(t.teal)));
        facts.push(Span::styled(format!("{} ", rate(rx)), t.muted()));
        facts.push(Span::styled("↑", Style::default().fg(t.brass)));
        facts.push(Span::styled(rate(tx), t.muted()));
    }
    if let Some((pct, status)) = &s.battery {
        let icon = if status == "Charging" { "⚡" } else { "▮" };
        facts.push(Span::styled(format!("  {icon}{pct}%"), t.muted()));
    }
    out.push(Line::from(facts));

    let units = match &v.failed {
        None => vec![lbl("units"), Span::styled("checking…", t.ghost())],
        Some(f) if f.is_empty() => vec![
            lbl("units"),
            Span::styled("✓ ", Style::default().fg(t.good)),
            Span::styled("all healthy", t.muted()),
        ],
        Some(f) => {
            let names = truncate(&f.join(", "), w.saturating_sub(label_w + 12));
            vec![
                lbl("units"),
                Span::styled(
                    format!("✗ {} failed ", f.len()),
                    Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
                ),
                Span::styled(names, t.muted()),
            ]
        }
    };
    out.push(Line::from(units));
    out
}

// ── little instruments ──────────────────────────────────────────────────────

/// A smooth bar in eighths, colored along teal → amber → red by position.
pub(crate) fn bar(ratio: f64, width: usize, t: &Theme) -> Vec<Span<'static>> {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let ratio = ratio.clamp(0.0, 1.0);
    let eighths = (ratio * width as f64 * 8.0).round() as usize;
    let full = eighths / 8;
    let rem = eighths % 8;
    let mut out = Vec::with_capacity(width);
    for i in 0..width {
        let pos = i as f32 / width.max(1) as f32;
        let c = t.gradient(&[t.teal, t.teal, t.amber, t.bad], pos);
        if i < full {
            out.push(Span::styled("█", Style::default().fg(c)));
        } else if i == full && rem > 0 {
            out.push(Span::styled(PART[rem], Style::default().fg(c).bg(t.input)));
        } else {
            out.push(Span::styled(" ", Style::default().bg(t.input)));
        }
    }
    out
}

/// A braille area chart: 2 samples per cell, 4 levels per row. Newest at
/// the right; rows shade from teal (bottom) to amber (top).
pub(crate) fn braille(
    data: &[f32],
    max: f32,
    width: usize,
    height: usize,
    t: &Theme,
) -> Vec<Vec<Span<'static>>> {
    const LEFT: [u32; 4] = [0x01, 0x02, 0x04, 0x40];
    const RIGHT: [u32; 4] = [0x08, 0x10, 0x20, 0x80];
    let levels = height * 4;
    let take = (width * 2).min(data.len());
    let tail = &data[data.len() - take..];
    let pad_cols = width * 2 - take;
    let level = |i: usize| -> usize {
        if i < pad_cols {
            return 0;
        }
        let v = tail[i - pad_cols];
        let l = (v / max * levels as f32).round() as usize;
        // Any activity at all shows one dot.
        if v > 0.5 { l.max(1) } else { l }.min(levels)
    };
    (0..height)
        .map(|row| {
            let c = t.gradient(&[t.amber, t.teal], row as f32 / (height.max(2) - 1) as f32);
            (0..width)
                .map(|col| {
                    let (a, b) = (level(col * 2), level(col * 2 + 1));
                    let mut bits = 0x2800;
                    for k in 0..4 {
                        // Level of this dot counted from the chart's bottom.
                        let dot = (height - 1 - row) * 4 + (4 - k);
                        if a >= dot {
                            bits |= LEFT[k];
                        }
                        if b >= dot {
                            bits |= RIGHT[k];
                        }
                    }
                    let ch = char::from_u32(bits).unwrap_or(' ');
                    let style = if bits == 0x2800 {
                        Style::default().fg(t.faint)
                    } else {
                        Style::default().fg(c)
                    };
                    Span::styled(ch.to_string(), style)
                })
                .collect()
        })
        .collect()
}

// ── text ────────────────────────────────────────────────────────────────────

/// Markdown, the parts a chat reply uses: headings, bullets, quotes, rules,
/// fenced code, `inline code`, and **bold**.
pub(crate) fn markdown(text: &str, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let mut in_code = false;
    for raw in text.lines() {
        let line = raw.trim_end();
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            if in_code {
                let lang = line.trim_start().trim_start_matches('`').trim();
                out.push(Line::from(Span::styled(
                    format!("╭─ {}", if lang.is_empty() { "code" } else { lang }),
                    t.ghost(),
                )));
            } else {
                out.push(Line::from(Span::styled("╰─", t.ghost())));
            }
            continue;
        }
        if in_code {
            for row in char_wrap(line, width.saturating_sub(2)) {
                out.push(Line::from(vec![
                    Span::styled("│ ", t.ghost()),
                    Span::styled(row, Style::default().fg(t.code).bg(t.code_bg)),
                ]));
            }
            continue;
        }
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.is_empty() {
            out.push(Line::raw(""));
        } else if let Some(h) = trimmed.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            let style = Style::default().fg(t.brass).add_modifier(Modifier::BOLD);
            for l in wrap_spans(vec![Span::styled(h.to_string(), style)], width, "", "") {
                out.push(l);
            }
        } else if trimmed == "---" || trimmed == "***" {
            out.push(Line::from(Span::styled(
                "─".repeat(width.min(40)),
                t.ghost(),
            )));
        } else if let Some(rest) = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "))
        {
            let lead = format!("{}• ", " ".repeat(indent.min(6)));
            let hang = " ".repeat(lead.width());
            let mut lines = wrap_spans(inline(rest, t), width, &lead, &hang);
            if let Some(first) = lines.first_mut() {
                if let Some(s) = first.spans.first_mut() {
                    s.style = Style::default().fg(t.teal);
                }
            }
            out.extend(lines);
        } else if let Some(rest) = trimmed.strip_prefix("> ") {
            for mut l in wrap_spans(inline(rest, t), width.saturating_sub(2), "", "") {
                l.spans
                    .insert(0, Span::styled("▎ ", Style::default().fg(t.faint)));
                for s in &mut l.spans[1..] {
                    s.style = s.style.fg(t.dim).add_modifier(Modifier::ITALIC);
                }
                out.push(l);
            }
        } else if let Some((num, rest)) = numbered(trimmed) {
            let lead = format!("{}{num}. ", " ".repeat(indent.min(6)));
            let hang = " ".repeat(lead.width());
            let mut lines = wrap_spans(inline(rest, t), width, &lead, &hang);
            if let Some(first) = lines.first_mut() {
                if let Some(s) = first.spans.first_mut() {
                    s.style = Style::default().fg(t.brass);
                }
            }
            out.extend(lines);
        } else {
            out.extend(wrap_spans(inline(line, t), width, "", ""));
        }
    }
    out
}

fn numbered(s: &str) -> Option<(&str, &str)> {
    let (num, rest) = s.split_once(". ")?;
    (!num.is_empty() && num.len() <= 3 && num.chars().all(|c| c.is_ascii_digit()))
        .then_some((num, rest))
}

/// `inline code` and **bold** within a line.
pub(crate) fn inline(s: &str, t: &Theme) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    let mut rest = s;
    let plain = t.text();
    while !rest.is_empty() {
        let tick = rest.find('`');
        let star = rest.find("**");
        match (tick, star) {
            (Some(i), s2) if s2.is_none_or(|j| i < j) => {
                if let Some(end) = rest[i + 1..].find('`') {
                    out.push(Span::styled(rest[..i].to_string(), plain));
                    out.push(Span::styled(
                        rest[i + 1..i + 1 + end].to_string(),
                        Style::default().fg(t.code).bg(t.code_bg),
                    ));
                    rest = &rest[i + 2 + end..];
                    continue;
                }
                break;
            }
            (_, Some(j)) => {
                if let Some(end) = rest[j + 2..].find("**") {
                    out.push(Span::styled(rest[..j].to_string(), plain));
                    out.push(Span::styled(
                        rest[j + 2..j + 2 + end].to_string(),
                        plain.add_modifier(Modifier::BOLD).fg(t.amber),
                    ));
                    rest = &rest[j + 4 + end..];
                    continue;
                }
                break;
            }
            _ => break,
        }
    }
    if !rest.is_empty() {
        out.push(Span::styled(rest.to_string(), plain));
    }
    out.retain(|s| !s.content.is_empty());
    out
}

/// Word-wrap styled spans. `lead` starts the first line, `hang` the rest.
pub(crate) fn wrap_spans(
    spans: Vec<Span<'static>>,
    width: usize,
    lead: &str,
    hang: &str,
) -> Vec<Line<'static>> {
    let width = width.max(4);
    let mut lines = Vec::new();
    let mut cur: Vec<Span<'static>> = vec![Span::raw(lead.to_string())];
    let mut cur_w = lead.width();
    // Tokens keep their style; whitespace is its own token so it can be
    // dropped at line breaks.
    let mut tokens: Vec<(String, Style)> = Vec::new();
    for s in spans {
        let mut word = String::new();
        for ch in s.content.chars() {
            if ch == ' ' {
                if !word.is_empty() {
                    tokens.push((std::mem::take(&mut word), s.style));
                }
                tokens.push((" ".into(), s.style));
            } else {
                word.push(ch);
            }
        }
        if !word.is_empty() {
            tokens.push((word, s.style));
        }
    }
    for (tok, style) in tokens {
        let tw = tok.width();
        if tok == " " {
            if cur_w > hang.width() && cur_w < width {
                cur.push(Span::styled(tok, style));
                cur_w += 1;
            }
            continue;
        }
        if cur_w + tw > width && cur_w > hang.width().max(lead.width()) {
            // Drop trailing space before breaking.
            if cur.last().is_some_and(|s| s.content == " ") {
                cur.pop();
            }
            lines.push(Line::from(std::mem::take(&mut cur)));
            cur.push(Span::raw(hang.to_string()));
            cur_w = hang.width();
        }
        if tw > width.saturating_sub(cur_w) {
            // A word longer than the line: hard-split it.
            for piece in char_wrap(&tok, width.saturating_sub(cur_w).max(1)) {
                let pw = piece.width();
                if cur_w + pw > width {
                    lines.push(Line::from(std::mem::take(&mut cur)));
                    cur.push(Span::raw(hang.to_string()));
                    cur_w = hang.width();
                }
                cur.push(Span::styled(piece, style));
                cur_w += pw;
            }
        } else {
            cur.push(Span::styled(tok, style));
            cur_w += tw;
        }
    }
    lines.push(Line::from(cur));
    lines
}

pub(crate) fn plain_wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    for para in text.lines() {
        let lines = wrap_spans(vec![Span::raw(para.to_string())], width, "", "");
        for l in lines {
            out.push(l.spans.iter().map(|s| s.content.as_ref()).collect());
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

/// Hard wrap by display width, honoring newlines.
pub(crate) fn char_wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut out = Vec::new();
    for para in text.split('\n') {
        let mut cur = String::new();
        let mut w = 0;
        for ch in para.chars() {
            let cw = ch.width().unwrap_or(0);
            if w + cw > width {
                out.push(std::mem::take(&mut cur));
                w = 0;
            }
            cur.push(ch);
            w += cw;
        }
        out.push(cur);
    }
    out
}

/// Row and column of the end of `before` under [`char_wrap`].
pub(crate) fn cursor_pos(before: &str, width: usize) -> (usize, usize) {
    let rows = char_wrap(before, width);
    let last = rows.last().map_or(0, |r| r.width());
    let row = rows.len().saturating_sub(1);
    if last >= width {
        (row + 1, 0)
    } else {
        (row, last)
    }
}

pub(crate) fn pad(s: &str, w: usize) -> String {
    let sw = s.width();
    if sw >= w {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(w - sw))
    }
}

pub(crate) fn truncate(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(ch);
        used += cw;
    }
    out.push('…');
    out
}

pub(crate) fn gib(bytes: u64) -> String {
    let g = bytes as f64 / (1u64 << 30) as f64;
    if g >= 1000.0 {
        format!("{:.1}T", g / 1024.0)
    } else if g >= 100.0 {
        format!("{g:.0}G")
    } else if g >= 1.0 {
        format!("{g:.1}G")
    } else {
        format!("{:.0}M", bytes as f64 / (1u64 << 20) as f64)
    }
}

pub(crate) fn rate(bps: f64) -> String {
    if bps >= 1e6 {
        format!("{:.1}M/s", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.0}K/s", bps / 1e3)
    } else {
        format!("{bps:.0}B/s")
    }
}

pub(crate) fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs / 3600 % 24, secs / 60 % 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

/// `anthropic/claude-sonnet-4.6` → `claude-sonnet-4.6`.
pub(crate) fn short_model(m: &str) -> String {
    m.rsplit('/').next().unwrap_or(m).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::View;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use reeve_observer::{Disk, HostInfo, Snapshot};

    fn render(v: &View, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let t = Theme::brass();
        term.draw(|f| draw(f, v, &t)).unwrap();
        let buf = term.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..h {
            for x in 0..w {
                s.push_str(buf[(x, y)].symbol());
            }
            s.push('\n');
        }
        s
    }

    fn busy_view() -> View {
        let mut v = View::new(HostInfo {
            hostname: "nexus".into(),
            os_short: "Fedora 44".into(),
            ..HostInfo::default()
        });
        v.model = "anthropic/claude-sonnet-4.6".into();
        v.connection = "openrouter".into();
        v.ready = true;
        v.sample(Snapshot {
            uptime_secs: 3 * 86_400 + 4 * 3600,
            cpu_pct: Some(18.0),
            mem_total: 32 << 30,
            mem_used: 9 << 30,
            disks: vec![Disk {
                mount: "/boot".into(),
                fs: "ext4".into(),
                total: 1 << 30,
                used: 950 << 20,
                avail: 74 << 20,
            }],
            ..Snapshot::default()
        });
        for i in 0..200 {
            v.cpu_hist.push_back((i % 37) as f32 * 2.5);
        }
        v.failed = Some(vec!["bluetooth.service".into()]);
        v.push(crate::view::Speaker::User, "clean up old kernels");
        v.push(
            crate::view::Speaker::Reeve,
            "## Plan\n- remove `kernel-6.9.4`\n- keep the **running** kernel\n\n```sh\ndnf remove kernel-6.9.4\n```",
        );
        v
    }

    #[test]
    fn the_frame_is_tabs_a_ledger_and_a_status_line() {
        let s = render(&busy_view(), 160, 44);
        for needle in [
            "reeve",
            "1 ledger",
            "2 findings",
            "4 spend",
            "6 system",
            "nexus · all quiet, except /boot 93%",
            "━━━━━━━━",
            "● you",
            "◆ reeve",
            "• remove",
            "LEDGER",
            "TIERED",
            "⌃K everything",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }

    #[test]
    fn narrow_screens_keep_the_ledger_without_money_columns() {
        let mut v = busy_view();
        v.entries[1].cost = Some(crate::view::RoundCost {
            usd: Some(0.0123),
            usage: Default::default(),
            session: 0.0123,
        });
        let wide = render(&v, 160, 44);
        assert!(
            wide.contains("session") && wide.contains("$0.0123"),
            "{wide}"
        );
        let narrow = render(&v, 80, 24);
        assert!(
            narrow.contains("● you") && narrow.contains("$0.0123"),
            "{narrow}"
        );
        assert!(!narrow.contains("cost    session"), "{narrow}");
    }

    #[test]
    fn yolo_is_impossible_to_miss() {
        let mut v = busy_view();
        v.yolo = true;
        let s = render(&v, 160, 44);
        assert!(s.contains("YOLO") && !s.contains("TIERED"));
        let narrow = render(&v, 60, 20);
        assert!(narrow.lines().last().unwrap().contains("YOLO"), "{narrow}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let v = busy_view();
        for (w, h) in [(20, 5), (40, 10), (1, 1), (110, 12)] {
            render(&v, w, h);
        }
    }

    #[test]
    fn wrapping_keeps_width() {
        let t = Theme::brass();
        let lines = wrap_spans(
            inline("some `code` and **bold** words that wrap around", &t),
            12,
            "• ",
            "  ",
        );
        for l in &lines {
            let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
            assert!(w <= 12, "{l:?}");
        }
        assert!(lines.len() > 2);
        assert_eq!(char_wrap("abcdef", 4), vec!["abcd", "ef"]);
        assert_eq!(cursor_pos("abcd", 4), (1, 0));
    }

    #[test]
    fn braille_fills_from_the_bottom() {
        let t = Theme::brass();
        let rows = braille(&[100.0, 100.0], 100.0, 1, 2, &t);
        assert_eq!(rows[0][0].content, "⣿");
        assert_eq!(rows[1][0].content, "⣿");
        let rows = braille(&[0.0, 0.0], 100.0, 1, 2, &t);
        assert_eq!(rows[1][0].content, "⠀");
        let rows = braille(&[50.0, 0.0], 100.0, 1, 2, &t);
        assert_eq!(rows[0][0].content, "⠀");
        assert_eq!(rows[1][0].content, "⡇");
    }
}
