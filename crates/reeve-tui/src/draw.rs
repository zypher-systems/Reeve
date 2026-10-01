//! The frame: a top line, the board (or the chat, or an open tile under a
//! strip of the others), and the composer. Floating panels (providers,
//! the model picker, ⌃K) draw over everything.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::board;
use crate::theme::Theme;
use crate::view::{Screen, Tile, View};

pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draw one frame.
pub fn draw(f: &mut Frame, v: &View, t: &Theme) {
    let area = f.area();
    f.render_widget(Block::default().style(Style::default().bg(t.bg)), area);
    let a = board::areas(area, v);
    board::top_bar(f, a.top, v, t);
    if a.strip.height > 0 {
        board::strip(f, a.strip, v, t);
    }
    let screen = v.screen();
    match screen {
        Screen::Board => board::board(f, a.main, v, t),
        Screen::Chat => {
            let inner = board::surface(f, a.main, t.panel, t.bg, t);
            crate::ledger::draw(f, inner, v, t);
        }
        Screen::Tile(tile) => {
            let inner = board::surface(f, a.main, t.panel, t.bg, t);
            crate::screens::draw(f, inner, v, t, tile);
        }
    }
    match (&v.approval, screen) {
        (Some(p), Screen::Chat) => crate::cards::draw_approval(f, a.composer, v, p, t),
        _ => {
            board::composer(f, a.composer, v, t);
            crate::panels::draw_palette(f, a.composer, v, t);
        }
    }
    crate::panels::draw_overlay(f, v, t);
}

/// The tile a click at (`column`, `row`) lands on: a board tile, or a mini
/// tile in the strip.
pub fn click(area: Rect, v: &View, column: u16, row: u16) -> Option<Tile> {
    let a = board::areas(area, v);
    let at = Position::new(column, row);
    let rects = if v.screen() == Screen::Board {
        board::tile_rects(a.main)
    } else {
        board::strip_rects(a.strip)
    };
    rects
        .into_iter()
        .find(|(_, r)| r.contains(at))
        .map(|(tile, _)| tile)
}

// ── floating panels ─────────────────────────────────────────────────────────

pub(crate) fn panel<'a>(title: &'a str, t: &Theme, hot: bool) -> Block<'a> {
    let edge = if hot {
        t.mix(t.border, t.brass, 0.45)
    } else {
        t.border
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(edge).bg(t.panel))
        .title(Line::from(vec![Span::styled(
            format!(" {title} "),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        )]))
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
        spans.extend(board::meter(r, bar_w, board::level(r, t), t));
        spans.push(Span::styled(
            format!(" {}/{}", gib(s.mem_used), gib(s.mem_total)),
            t.muted(),
        ));
        out.push(Line::from(spans));
    }
    if s.swap_total > 0 {
        let r = s.swap_used as f64 / s.swap_total as f64;
        let mut spans = vec![lbl("swap")];
        spans.extend(board::meter(r, bar_w, board::level(r, t), t));
        spans.push(Span::styled(
            format!(" {}/{}", gib(s.swap_used), gib(s.swap_total)),
            t.ghost(),
        ));
        out.push(Line::from(spans));
    }
    for d in &s.disks {
        let r = d.ratio();
        let mut spans = vec![lbl(&truncate(&d.mount, label_w))];
        spans.extend(board::meter(r, bar_w, board::level(r, t), t));
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

/// A braille area chart: 2 samples per cell, 4 levels per row. Newest at
/// the right; rows shade from the accent (bottom) to violet (top).
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
            let c = t.gradient(
                &[t.violet, t.brass],
                row as f32 / (height.max(2) - 1) as f32,
            );
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
    fn the_header_says_when_a_newer_reeve_is_out() {
        use reeve_core::update::{Badge, Version};
        let mut v = busy_view();
        assert!(!render(&v, 160, 44).contains("0.5.0"));
        v.update = Some(Badge::Available(Version(0, 5, 0)));
        let s = render(&v, 160, 44);
        assert!(s.lines().next().unwrap().contains("↑ 0.5.0"), "{s}");
        v.update = Some(Badge::Restart(Version(0, 5, 0)));
        let s = render(&v, 160, 44);
        assert!(
            s.lines().next().unwrap().contains("restart for 0.5.0"),
            "{s}"
        );
    }

    #[test]
    fn home_is_the_board_with_every_tile() {
        let s = render(&busy_view(), 160, 44);
        for needle in [
            "◆ reeve",
            "nexus",
            "F1",
            "needs you",
            "health",
            "findings",
            "activity",
            "spend",
            "changed",
            "orders",
            "memory",
            "/boot 93%",
            "✗ 1 failed",
            "tiered",
            "ask Reeve anything",
            "the chat",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }

    #[test]
    fn the_chat_sits_under_the_strip() {
        let mut v = busy_view();
        v.chat = true;
        let s = render(&v, 160, 44);
        for needle in ["● you", "◆ reeve", "• remove", "needs you", "F8", "esc"] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }

    #[test]
    fn narrow_screens_keep_the_chat_without_money_columns() {
        let mut v = busy_view();
        v.chat = true;
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
    fn clicking_a_tile_opens_it() {
        let mut v = busy_view();
        let area = Rect::new(0, 0, 160, 44);
        let a = board::areas(area, &v);
        for (tile, r) in board::tile_rects(a.main) {
            assert_eq!(click(area, &v, r.x + 3, r.y + 1), Some(tile), "{tile:?}");
        }
        assert_eq!(click(area, &v, 0, 0), None);
        // Under the strip, a mini tile opens its tile too.
        v.chat = true;
        let a = board::areas(area, &v);
        for (tile, r) in board::strip_rects(a.strip) {
            assert_eq!(click(area, &v, r.x + 2, r.y + 1), Some(tile), "{tile:?}");
        }
    }

    #[test]
    fn yolo_is_impossible_to_miss() {
        let mut v = busy_view();
        v.yolo = true;
        let s = render(&v, 160, 44);
        assert!(s.contains("YOLO") && !s.contains("tiered"));
        let narrow = render(&v, 60, 20);
        assert!(narrow.lines().next().unwrap().contains("YOLO"), "{narrow}");
    }

    #[test]
    fn tiny_terminals_do_not_panic() {
        let mut v = busy_view();
        for chat in [false, true] {
            v.chat = chat;
            for (w, h) in [(20, 5), (40, 10), (1, 1), (110, 12), (70, 22), (100, 30)] {
                render(&v, w, h);
            }
        }
    }

    #[test]
    fn sixteen_colors_get_borders_not_fills() {
        let mut term = Terminal::new(TestBackend::new(160, 44)).unwrap();
        let t = Theme::slate().degrade(crate::theme::ColorMode::Ansi16);
        term.draw(|f| draw(f, &busy_view(), &t)).unwrap();
        let buf = term.backend().buffer().clone();
        let s: String = (0..44)
            .map(|y| (0..160).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect();
        assert!(s.contains('╭') && !s.contains('▄'), "{s}");
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
