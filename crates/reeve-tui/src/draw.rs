//! Mission control: conversation on the left, a live rail on the right
//! (System, Spend, Receipts), a gradient header, and key hints below.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use reeve_core::spend::{format_tokens, format_usd};

use crate::theme::Theme;
use crate::view::{Speaker, View};

/// Below this width the rail and the chat take turns (`^b`).
pub const WIDE: u16 = 110;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draw one frame.
pub fn draw(f: &mut Frame, v: &View, t: &Theme) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(t.bg)), area);
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(area);
    draw_header(f, header, v, t);
    draw_footer(f, footer, v, t);

    let body = body.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    if body.width >= WIDE {
        let rail_w = (body.width * 38 / 100).clamp(40, 58);
        let [chat, rail] =
            Layout::horizontal([Constraint::Min(40), Constraint::Length(rail_w)]).areas(body);
        draw_chat(f, chat, v, t);
        draw_rail(f, rail, v, t);
    } else if v.rail_only {
        draw_rail(f, body, v, t);
    } else {
        draw_chat(f, body, v, t);
    }
    crate::panels::draw_overlay(f, v, t);
}

// ── header & footer ─────────────────────────────────────────────────────────

fn draw_header(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let mut left = vec![Span::raw(" ")];
    // The wordmark: a brass → copper → teal gradient that drifts slowly.
    let mark = " ◆ R E E V E ";
    let n = mark.chars().count().max(1) as f32;
    let drift = if v.animate {
        (v.frame as f32 / 90.0).sin() * 0.15
    } else {
        0.0
    };
    for (i, ch) in mark.chars().enumerate() {
        let c = t.gradient(&[t.brass, t.amber, t.copper, t.teal], i as f32 / n + drift);
        left.push(Span::styled(
            ch.to_string(),
            Style::default().fg(c).add_modifier(Modifier::BOLD),
        ));
    }
    let host = &v.host;
    let mut facts = Vec::new();
    if !host.hostname.is_empty() {
        facts.push(host.hostname.clone());
    }
    if !host.os_short.is_empty() {
        facts.push(host.os_short.clone());
    }
    if v.snap.uptime_secs > 0 {
        facts.push(format!("up {}", uptime(v.snap.uptime_secs)));
    }
    let facts = format!("  {}", facts.join(" · "));

    let mut right = Vec::new();
    if !v.model.is_empty() {
        right.push(Span::styled(short_model(&v.model), t.muted()));
        right.push(Span::styled(format!(" via {}  ", v.connection), t.ghost()));
    }
    right.push(Span::styled("observer ", t.ghost()));
    right.push(Span::styled("○ local  ", Style::default().fg(t.dim)));
    if v.yolo {
        // The badge breathes so nobody forgets that nothing is being asked.
        let pulse = if v.animate {
            ((v.frame as f32 / 8.0).sin() + 1.0) / 2.0
        } else {
            1.0
        };
        let bg = t.mix(t.copper, t.bad, pulse);
        right.push(Span::styled(
            " YOLO ",
            Style::default()
                .fg(Color::Black)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        right.push(Span::styled(
            " TIERED ",
            Style::default()
                .fg(t.bg)
                .bg(t.teal)
                .add_modifier(Modifier::BOLD),
        ));
    }
    right.push(Span::raw(" "));
    // The mode badge must always show; host facts, then the model, give way.
    let width = |v: &[Span]| v.iter().map(|s| s.content.width()).sum::<usize>();
    let avail = area.width as usize;
    while right.len() > 3 && width(&left) + width(&right) > avail {
        right.remove(0);
    }
    let room = avail.saturating_sub(width(&left) + width(&right) + 1);
    left.push(Span::styled(truncate(&facts, room), t.muted()));
    put_split(f, area, left, right, t.bg);
}

fn draw_footer(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let key = |k: &str| Span::styled(k.to_string(), Style::default().fg(t.brass));
    let label = |l: &str| Span::styled(format!(" {l}   "), t.ghost());
    let mut spans = vec![Span::raw(" ")];
    let floor = v
        .approval
        .as_ref()
        .is_some_and(|p| p.req.tier == reeve_core::policy::Tier::T3);
    let hints: &[(&str, &str)] = if floor {
        &[("yes ⏎", "allow"), ("esc", "deny"), ("^c", "stop the turn")]
    } else if v.approval.is_some() {
        &[("⏎", "approve"), ("n", "deny"), ("^c", "stop the turn")]
    } else if v.busy {
        &[("esc", "stop"), ("pgup/pgdn", "scroll")]
    } else {
        &[
            ("⏎", "send"),
            ("/", "commands"),
            ("alt+⏎", "newline"),
            ("^y", "yolo"),
            ("^b", "rail"),
            ("pgup/pgdn", "scroll"),
            ("^c", "quit"),
        ]
    };
    for (k, l) in hints {
        spans.push(key(k));
        spans.push(label(l));
    }
    let right = vec![Span::styled(
        format!("session {} ", v.session.label()),
        Style::default().fg(t.dim),
    )];
    put_split(f, area, spans, right, t.bg);
}

/// Left spans, right spans flush right, on one row.
fn put_split(f: &mut Frame, area: Rect, left: Vec<Span>, right: Vec<Span>, bg: Color) {
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

// ── conversation ────────────────────────────────────────────────────────────

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

fn draw_chat(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let block = panel("conversation", t, true);
    let inner = block.inner(area);
    f.render_widget(block, area);

    let text_w = inner.width.saturating_sub(4).max(10) as usize;
    let composer_lines = char_wrap(&v.input, text_w).len().clamp(1, 6) as u16;
    // An approval takes the composer's place until it's answered.
    let bottom_h = match &v.approval {
        Some(p) => {
            crate::cards::approval_height(p, inner.width).min(inner.height.saturating_sub(3).max(6))
        }
        None => composer_lines + 2,
    };
    let [log, composer] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(bottom_h)]).areas(inner);

    // Until the conversation starts, the wordmark sits above any notices.
    let talking = v
        .entries
        .iter()
        .any(|e| matches!(e.who, Speaker::User | Speaker::Reeve));
    let lines = if talking {
        transcript(v, t, log.width.saturating_sub(2) as usize)
    } else {
        let mut l = welcome(log, v, t);
        if !v.entries.is_empty() {
            l.push(Line::raw(""));
            l.push(Line::raw(""));
            l.extend(transcript(v, t, log.width.saturating_sub(2) as usize));
        }
        l
    };
    let h = log.height as usize;
    let max_scroll = lines.len().saturating_sub(h);
    let scroll = v.scroll.min(max_scroll);
    let start = lines.len().saturating_sub(h + scroll);
    let shown: Vec<Line> = lines.into_iter().skip(start).take(h).collect();
    let log_area = log.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(Paragraph::new(shown), log_area);
    if scroll > 0 {
        let tag = format!(" ↑ {scroll} more below ");
        let w = tag.width() as u16;
        let r = Rect::new(
            log.right().saturating_sub(w + 1),
            log.bottom().saturating_sub(1),
            w,
            1,
        );
        f.render_widget(
            Paragraph::new(Span::styled(tag, Style::default().fg(t.bg).bg(t.brass))),
            r,
        );
    }
    match &v.approval {
        Some(p) => crate::cards::draw_approval(f, composer, v, p, t),
        None => {
            draw_composer(f, composer, v, t, text_w);
            crate::panels::draw_palette(f, composer, v, t);
        }
    }
}

fn draw_composer(f: &mut Frame, area: Rect, v: &View, t: &Theme, text_w: usize) {
    let edge = if v.busy {
        t.border
    } else {
        t.mix(t.border, t.brass, 0.55)
    };
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(edge))
        .style(Style::default().bg(t.input));
    let inner = block.inner(area);
    f.render_widget(block, area);
    let prompt = Span::styled(
        "❯ ",
        Style::default()
            .fg(if v.busy { t.faint } else { t.brass })
            .add_modifier(Modifier::BOLD),
    );
    let rows = char_wrap(&v.input, text_w);
    let mut lines = Vec::new();
    if v.input.is_empty() {
        let hint = if v.busy {
            "Reeve is working…  esc to stop"
        } else if v.ready {
            "Ask Reeve to check, explain, or plan something on this machine…"
        } else {
            "Type /providers to add an API key and pick a model."
        };
        lines.push(Line::from(vec![
            prompt,
            Span::styled(hint, t.ghost().add_modifier(Modifier::ITALIC)),
        ]));
    } else {
        for (i, row) in rows.iter().enumerate() {
            let lead = if i == 0 {
                prompt.clone()
            } else {
                Span::raw("  ")
            };
            lines.push(Line::from(vec![lead, Span::styled(row.clone(), t.text())]));
        }
    }
    let visible = inner.height as usize;
    let skip = lines.len().saturating_sub(visible);
    let body = inner.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        body,
    );
    if !v.busy && v.overlays.is_empty() {
        let (row, col) = cursor_pos(&v.input[..v.cursor], text_w);
        let row = row.saturating_sub(skip);
        f.set_cursor_position(Position::new(
            body.x + 2 + col as u16,
            body.y + row.min(visible.saturating_sub(1)) as u16,
        ));
    }
}

fn welcome(area: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let h = area.height as usize;
    let notices: usize = v.entries.iter().map(|e| e.text.lines().count() + 2).sum();
    let top = h.saturating_sub(9 + notices) / 2;
    out.extend((0..top).map(|_| Line::raw("")));
    let w = area.width.saturating_sub(2) as usize;
    let center = |s: &str| " ".repeat(w.saturating_sub(s.width()) / 2);
    let mark = "◆  R  E  E  V  E  ◆";
    let n = mark.chars().count() as f32;
    let mut spans = vec![Span::raw(center(mark))];
    let phase = if v.animate {
        v.frame as f32 / 40.0
    } else {
        0.0
    };
    for (i, ch) in mark.chars().enumerate() {
        let x = (i as f32 / n + phase).fract();
        let c = t.gradient(&[t.brass, t.amber, t.copper, t.teal, t.brass], x);
        spans.push(Span::styled(
            ch.to_string(),
            Style::default().fg(c).add_modifier(Modifier::BOLD),
        ));
    }
    out.push(Line::from(spans));
    let tag = "the steward of this machine";
    out.push(Line::from(vec![
        Span::raw(center(tag)),
        Span::styled(tag, t.muted().add_modifier(Modifier::ITALIC)),
    ]));
    out.push(Line::raw(""));
    let rule = "─".repeat(w.min(44));
    out.push(Line::from(vec![
        Span::raw(center(&rule)),
        Span::styled(rule.clone(), t.ghost()),
    ]));
    out.push(Line::raw(""));
    for s in [
        "why is my disk filling up?",
        "what should I check after a kernel update?",
        "plan a cleanup of old flatpak runtimes",
    ] {
        let line = format!("›  {s}");
        out.push(Line::from(vec![
            Span::raw(center(&line)),
            Span::styled("›  ", Style::default().fg(t.brass)),
            Span::styled(s.to_string(), t.muted()),
        ]));
    }
    out
}

fn transcript(v: &View, t: &Theme, width: usize) -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let body_w = width.saturating_sub(2).max(8);
    for (i, e) in v.entries.iter().enumerate() {
        let time = Span::styled(format!("  {}", e.at.format("%H:%M")), t.ghost());
        match e.who {
            Speaker::Tool => {
                if let Some(tv) = &e.tool {
                    for mut l in crate::cards::tool_lines(tv, body_w, v.frame, t) {
                        l.spans.insert(0, Span::raw("  "));
                        out.push(l);
                    }
                }
                // Consecutive tool calls sit together.
                if v.entries.get(i + 1).is_some_and(|n| n.who == Speaker::Tool) {
                    continue;
                }
            }
            Speaker::User => {
                out.push(Line::from(vec![
                    Span::styled("▍", Style::default().fg(t.user)),
                    Span::styled(
                        "you",
                        Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                    ),
                    time,
                ]));
                for l in plain_wrap(&e.text, body_w) {
                    out.push(Line::from(vec![Span::raw("  "), Span::styled(l, t.text())]));
                }
            }
            Speaker::Reeve => {
                out.push(Line::from(vec![
                    Span::styled("◆ ", Style::default().fg(t.brass)),
                    Span::styled("reeve", t.accent()),
                    time,
                ]));
                for mut l in markdown(&e.text, body_w, t) {
                    l.spans.insert(0, Span::raw("  "));
                    out.push(l);
                }
            }
            Speaker::System => {
                for (i, l) in plain_wrap(&e.text, body_w).into_iter().enumerate() {
                    let lead = if i == 0 { "· " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(lead, Style::default().fg(t.teal)),
                        Span::styled(l, t.muted()),
                    ]));
                }
            }
            Speaker::Error => {
                for (i, l) in plain_wrap(&e.text, body_w).into_iter().enumerate() {
                    let lead = if i == 0 { "✗ " } else { "  " };
                    out.push(Line::from(vec![
                        Span::styled(lead, Style::default().fg(t.bad)),
                        Span::styled(l, Style::default().fg(t.bad)),
                    ]));
                }
            }
        }
        out.push(Line::raw(""));
    }
    let running_tool = v
        .entries
        .last()
        .and_then(|e| e.tool.as_ref())
        .is_some_and(|t| t.status.is_none());
    if v.busy && !running_tool {
        let spin = SPINNER[(v.frame / 2) as usize % SPINNER.len()];
        let label = if v.thinking.is_empty() {
            "working"
        } else {
            "thinking"
        };
        let mut line = vec![
            Span::styled(format!("{spin} "), Style::default().fg(t.amber)),
            Span::styled(label, Style::default().fg(t.amber)),
        ];
        if !v.thinking.is_empty() {
            let words = v.thinking.split_whitespace().count();
            line.push(Span::styled(format!("  {words} words"), t.ghost()));
        }
        out.push(Line::from(line));
        if let Some(tail) = v.thinking.lines().rev().find(|l| !l.trim().is_empty()) {
            let tail = truncate(tail.trim(), body_w);
            out.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(tail, t.ghost().add_modifier(Modifier::ITALIC)),
            ]));
        }
    }
    out
}

// ── the rail ────────────────────────────────────────────────────────────────

fn draw_rail(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let inner_w = area.width.saturating_sub(4) as usize;
    let sys = system_lines(v, t, inner_w);
    let spend = spend_lines(v, t, inner_w);
    let sys_h = (sys.len() as u16 + 2).min(area.height / 2);
    let spend_h = (spend.len() as u16 + 2).min(area.height.saturating_sub(sys_h + 3));
    let [a, b, c] = Layout::vertical([
        Constraint::Length(sys_h),
        Constraint::Length(spend_h),
        Constraint::Min(3),
    ])
    .areas(area);
    put_panel(f, a, "system", sys, t);
    put_panel(f, b, "spend", spend, t);
    put_panel(
        f,
        c,
        "receipts",
        crate::cards::receipt_lines(v, inner_w, t),
        t,
    );
}

fn put_panel(f: &mut Frame, area: Rect, title: &str, lines: Vec<Line<'static>>, t: &Theme) {
    let block = panel(title, t, false);
    let inner = block.inner(area).inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 0,
    });
    f.render_widget(block, area);
    f.render_widget(Paragraph::new(lines), inner);
}

fn system_lines(v: &View, t: &Theme, w: usize) -> Vec<Line<'static>> {
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

fn spend_lines(v: &View, t: &Theme, w: usize) -> Vec<Line<'static>> {
    let lbl = |x: &str| Span::styled(pad(x, 9), t.muted());
    let money =
        |s: String| Span::styled(s, Style::default().fg(t.amber).add_modifier(Modifier::BOLD));
    let mut out = Vec::new();

    let u = v.session.usage;
    out.push(Line::from(vec![
        lbl("session"),
        money(v.session.label()),
        Span::styled(
            format!(
                "  {} tok · {} call{}",
                format_tokens(u.total()),
                v.session.calls,
                if v.session.calls == 1 { "" } else { "s" }
            ),
            t.ghost(),
        ),
    ]));
    if v.caps.session > 0.0 {
        out.push(capped("", v.session.usd, v.caps.session, w, t));
    }
    out.push(capped_line(
        "today",
        &v.totals.today.label(),
        v.totals.today.usd,
        v.caps.daily,
        w,
        t,
    ));
    out.push(capped_line(
        "month",
        &v.totals.month.label(),
        v.totals.month.usd,
        v.caps.monthly,
        w,
        t,
    ));

    if v.totals.by_day.iter().any(|d| *d > 0.0) {
        let mut spans = vec![lbl("by day")];
        spans.extend(sparkline(&v.totals.by_day, w.saturating_sub(9), t));
        out.push(Line::from(spans));
    }

    match v.last {
        Some(last) => {
            let hit = last.usage.cache_hit_ratio();
            let mut spans = vec![
                lbl("last"),
                Span::styled(format_usd(last.usd), t.text()),
                Span::styled(
                    format!(
                        "  {} in · {} out",
                        format_tokens(last.usage.input_tokens),
                        format_tokens(last.usage.output_tokens)
                    ),
                    t.ghost(),
                ),
            ];
            if last.usage.cached_tokens > 0 {
                spans.push(Span::styled(
                    format!(" · cache {:.0}%", hit * 100.0),
                    Style::default().fg(t.teal),
                ));
            }
            out.push(Line::from(spans));
        }
        None => out.push(Line::from(vec![
            lbl("last"),
            Span::styled("no calls yet", t.ghost()),
        ])),
    }
    if !v.model.is_empty() {
        out.push(Line::from(vec![
            lbl("model"),
            Span::styled(truncate(&v.model, w.saturating_sub(9)), t.text()),
        ]));
        out.push(Line::from(vec![
            lbl(""),
            Span::styled(truncate(&v.rates, w.saturating_sub(9)), t.ghost()),
        ]));
    }
    out
}

fn capped_line(name: &str, label: &str, used: f64, cap: f64, w: usize, t: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(pad(name, 9), t.muted()),
        Span::styled(
            pad(label, 9),
            Style::default().fg(t.amber).add_modifier(Modifier::BOLD),
        ),
    ];
    if cap > 0.0 {
        let bar_w = w.saturating_sub(9 + 9 + 9).max(4);
        spans.extend(bar(used / cap, bar_w, t));
        spans.push(Span::styled(format!(" / ${cap:.2}"), t.ghost()));
    } else {
        spans.push(Span::styled("no cap", t.ghost()));
    }
    Line::from(spans)
}

fn capped(name: &str, used: f64, cap: f64, w: usize, t: &Theme) -> Line<'static> {
    let mut spans = vec![Span::styled(pad(name, 9), t.muted())];
    let bar_w = w.saturating_sub(9 + 9).max(4);
    spans.extend(bar(used / cap, bar_w, t));
    spans.push(Span::styled(format!(" / ${cap:.2}"), t.ghost()));
    Line::from(spans)
}

// ── little instruments ──────────────────────────────────────────────────────

/// A smooth bar in eighths, colored along teal → amber → red by position.
fn bar(ratio: f64, width: usize, t: &Theme) -> Vec<Span<'static>> {
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
fn braille(
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
                        Style::default().fg(t.faint).bg(t.panel)
                    } else {
                        Style::default().fg(c)
                    };
                    Span::styled(ch.to_string(), style)
                })
                .collect()
        })
        .collect()
}

/// One-row block sparkline, newest at the right.
fn sparkline(data: &[f64], width: usize, t: &Theme) -> Vec<Span<'static>> {
    const BARS: [&str; 8] = ["▁", "▂", "▃", "▄", "▅", "▆", "▇", "█"];
    let take = width.min(data.len());
    let tail = &data[data.len() - take..];
    let max = tail.iter().copied().fold(0.0_f64, f64::max);
    tail.iter()
        .map(|v| {
            if *v <= 0.0 || max <= 0.0 {
                Span::styled("·", t.ghost())
            } else {
                let i = ((v / max) * 7.0).round() as usize;
                Span::styled(
                    BARS[i.min(7)],
                    Style::default().fg(t.gradient(&[t.teal, t.amber], (v / max) as f32)),
                )
            }
        })
        .collect()
}

// ── text ────────────────────────────────────────────────────────────────────

/// Markdown, the parts a chat reply uses: headings, bullets, quotes, rules,
/// fenced code, `inline code`, and **bold**.
fn markdown(text: &str, width: usize, t: &Theme) -> Vec<Line<'static>> {
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
fn inline(s: &str, t: &Theme) -> Vec<Span<'static>> {
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
fn wrap_spans(
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

fn plain_wrap(text: &str, width: usize) -> Vec<String> {
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
fn char_wrap(text: &str, width: usize) -> Vec<String> {
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
fn cursor_pos(before: &str, width: usize) -> (usize, usize) {
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

fn gib(bytes: u64) -> String {
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

fn rate(bps: f64) -> String {
    if bps >= 1e6 {
        format!("{:.1}M/s", bps / 1e6)
    } else if bps >= 1e3 {
        format!("{:.0}K/s", bps / 1e3)
    } else {
        format!("{bps:.0}B/s")
    }
}

fn uptime(secs: u64) -> String {
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
    fn wide_layout_shows_every_panel() {
        let s = render(&busy_view(), 160, 44);
        for needle in [
            "R E E V E",
            "conversation",
            "system",
            "spend",
            "receipts",
            "nexus",
            "Fedora 44",
            "TIERED",
            "1 failed",
            "/boot",
            "• remove",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }

    #[test]
    fn narrow_layout_keeps_the_chat() {
        let s = render(&busy_view(), 80, 24);
        assert!(s.contains("conversation"));
        assert!(!s.contains("receipts"));
        let mut v = busy_view();
        v.rail_only = true;
        assert!(render(&v, 80, 24).contains("receipts"));
    }

    #[test]
    fn yolo_is_impossible_to_miss() {
        let mut v = busy_view();
        v.yolo = true;
        let s = render(&v, 160, 44);
        assert!(s.contains("YOLO") && !s.contains("TIERED"));
        let narrow = render(&v, 60, 20);
        assert!(narrow.lines().next().unwrap().contains("YOLO"), "{narrow}");
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
