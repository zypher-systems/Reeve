//! The board. Home is every tile at a glance: what needs you, the machine's
//! health, findings, activity, spend, what changed, standing orders, and
//! memory. Opening a tile (F1–F8) or the chat gives it the main area while
//! the rest fold into a strip of live numbers along the top, so nothing
//! useful is ever a screen away. The composer is always at the bottom.
//!
//! Tiles are filled surfaces with half-block edges (half a row of padding,
//! softened corners); with 16 colors or none they get a rounded border.

use chrono::Local;
use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Paragraph};
use unicode_width::UnicodeWidthStr;

use reeve_core::findings::{Finding, Severity};
use reeve_core::policy::Tier;
use reeve_core::receipts::Status;
use reeve_core::spend::format_usd;

use crate::draw::{
    SPINNER, braille, char_wrap, cursor_pos, gib, pad, rate, short_model, truncate, uptime,
};
use crate::theme::Theme;
use crate::view::{Screen, Tile, View};

/// Wide enough for the strip's eight mini tiles; below it, one row of pills.
pub const STRIP_FULL: u16 = 112;
/// Wide enough for the four-column board.
const WIDE: u16 = 110;
/// Wide enough for the two-column board.
const MEDIUM: u16 = 64;

// ── what the board reads ────────────────────────────────────────────────────

/// Read what the board shows from disk: today's spend by role and hour,
/// the month, standing orders, and memory. (The day's report comes from
/// the worker; it takes a second or two.)
pub fn read(home: &std::path::Path, memory: &reeve_core::memory::Memory) -> crate::view::Board {
    use reeve_core::ledger;
    use reeve_core::memory::{Layer, NoteStatus};
    let mut b = crate::view::Board::default();
    let today = ledger::since(home, crate::overlay::SpendRange::Day.since());
    for l in ledger::statement(&today) {
        match b.roles.iter_mut().find(|r| r.0 == l.role) {
            Some(r) => {
                r.1 += l.usd;
                r.2 += l.calls;
            }
            None => b.roles.push((l.role.clone(), l.usd, l.calls)),
        }
    }
    b.roles.sort_by(|a, b| b.1.total_cmp(&a.1));
    b.hours = vec![0.0; 24];
    for r in &today {
        let h = r.ts.with_timezone(&Local).format("%H").to_string();
        if let Ok(h) = h.parse::<usize>() {
            b.hours[h.min(23)] += r.usd.unwrap_or(0.0);
        }
    }
    let month = ledger::since(home, crate::overlay::SpendRange::Month.since());
    let input: u64 = month.iter().map(|r| r.usage.input_tokens).sum();
    let cached: u64 = month.iter().map(|r| r.usage.cached_tokens).sum();
    b.month = (month.iter().filter_map(|r| r.usd).sum(), month.len() as u64);
    b.cached = (input > 0).then(|| cached as f64 / input as f64);
    b.orders = reeve_core::orders::Orders::new(home)
        .load()
        .0
        .iter()
        .map(|o| {
            let mut when = Vec::new();
            if let Some(s) = &o.trigger.schedule {
                when.push(s.clone());
            }
            if !o.trigger.findings.is_empty() {
                when.push("on findings".to_string());
            }
            (o.name.clone(), o.enabled, when.join(" · "))
        })
        .collect();
    let all = memory.all();
    let mut notes: Vec<&reeve_core::memory::Note> = Vec::new();
    for n in &all {
        if n.status == NoteStatus::Retired {
            continue;
        }
        let i = match n.layer {
            Layer::Facts => 0,
            Layer::Runbooks => 1,
            Layer::Preferences => 2,
            Layer::Baselines => 3,
        };
        b.layers[i] += 1;
        if n.layer != Layer::Baselines {
            notes.push(n);
        }
    }
    notes.sort_by(|a, b| b.observed.cmp(&a.observed));
    b.notes = notes
        .into_iter()
        .take(8)
        .map(|n| {
            (
                n.title.clone(),
                matches!(n.status, NoteStatus::New | NoteStatus::Pending),
            )
        })
        .collect();
    b
}

// ── surfaces ────────────────────────────────────────────────────────────────

/// Draw a filled surface over `under` and return its inside: two columns
/// in, one row down. The edges are half blocks, so the padding reads as
/// half a row and the corners are softened; without enough colors, a
/// rounded border instead, with the same inside.
pub fn surface(f: &mut Frame, area: Rect, fill: Color, under: Color, t: &Theme) -> Rect {
    if area.width < 5 || area.height < 3 {
        return Rect { height: 0, ..area };
    }
    let w = area.width as usize;
    if t.filled() {
        let buf = f.buffer_mut();
        let edge = Style::default().fg(fill).bg(under);
        buf.set_string(area.x, area.y, format!("▗{}▖", "▄".repeat(w - 2)), edge);
        for y in area.y + 1..area.bottom() - 1 {
            buf.set_string(area.x, y, " ".repeat(w), Style::default().bg(fill));
        }
        buf.set_string(
            area.x,
            area.bottom() - 1,
            format!("▝{}▘", "▀".repeat(w - 2)),
            edge,
        );
    } else {
        f.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_type(BorderType::Rounded)
                .border_style(Style::default().fg(t.border)),
            area,
        );
    }
    inside(area)
}

/// Fade everything drawn so far toward the ground, so a panel on top
/// stands out (with fewer colors, the terminal's dim attribute).
pub fn dim(f: &mut Frame, t: &Theme) {
    let area = f.area();
    let buf = f.buffer_mut();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let c = &mut buf[(x, y)];
            if t.mode == crate::theme::ColorMode::TrueColor {
                c.fg = t.mix(c.fg, t.bg, 0.62);
                c.bg = t.mix(c.bg, t.bg, 0.55);
            } else {
                c.modifier.insert(Modifier::DIM);
            }
        }
    }
}

/// A soft shadow under the right and bottom edges of `r`.
pub fn shadow(f: &mut Frame, r: Rect, t: &Theme) {
    if t.mode != crate::theme::ColorMode::TrueColor {
        return;
    }
    let area = f.area();
    let dark = t.mix(t.bg, Color::Rgb(0, 0, 0), 0.55);
    let buf = f.buffer_mut();
    for y in r.top() + 1..=r.bottom() {
        for x in [r.right(), r.right() + 1] {
            if x < area.right() && y < area.bottom() {
                buf[(x, y)].set_bg(dark);
            }
        }
    }
    if r.bottom() < area.bottom() {
        for x in r.left() + 2..(r.right() + 2).min(area.right()) {
            buf[(x, r.bottom())].set_bg(dark);
        }
    }
}

/// The inside of a surface drawn at `area`.
pub fn inside(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 1,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(2),
    }
}

/// A key as the hints show it.
pub(crate) fn key(k: &str, t: &Theme) -> Span<'static> {
    Span::styled(format!(" {k} "), t.key())
}

/// Key hints: `⏎ send   esc back`.
pub(crate) fn hints(pairs: &[(&str, &str)], t: &Theme) -> Vec<Span<'static>> {
    let mut out = Vec::new();
    for (i, (k, l)) in pairs.iter().enumerate() {
        if i > 0 {
            out.push(Span::raw("  "));
        }
        out.push(key(k, t));
        out.push(Span::styled(format!(" {l}"), t.ghost()));
    }
    out
}

fn width(spans: &[Span]) -> usize {
    spans.iter().map(|s| s.content.width()).sum()
}

/// Left spans, and right spans flush right, fitted to `w`: the right side
/// gives way first.
pub(crate) fn split(
    left: Vec<Span<'static>>,
    right: Vec<Span<'static>>,
    w: usize,
) -> Line<'static> {
    let lw = width(&left);
    let rw = width(&right);
    let mut spans = left;
    if lw + rw < w {
        spans.push(Span::raw(" ".repeat(w - lw - rw)));
        spans.extend(right);
    }
    Line::from(spans)
}

/// A severity's mark and color.
pub(crate) fn severity(sev: Severity, t: &Theme) -> (&'static str, Color) {
    match sev {
        Severity::Critical => ("●", t.bad),
        Severity::Warning => ("▲", t.warn),
        Severity::Info => ("◆", t.teal),
    }
}

/// A tier as a pill.
pub(crate) fn tier_pill(tier: Tier, t: &Theme) -> Span<'static> {
    let c = if tier == Tier::T0 {
        t.dim
    } else {
        t.tier(tier)
    };
    Span::styled(format!(" {} ", tier.label()), t.pill(c))
}

/// A meter: `ratio` of `w` cells filled in eighths, on a track.
pub(crate) fn meter(ratio: f64, w: usize, c: Color, t: &Theme) -> Vec<Span<'static>> {
    const PART: [&str; 8] = ["", "▏", "▎", "▍", "▌", "▋", "▊", "▉"];
    let eighths = (ratio.clamp(0.0, 1.0) * w as f64 * 8.0).round() as usize;
    let (full, rem) = (eighths / 8, eighths % 8);
    // The track: a filled cell in the raised color, or with few colors a
    // dotted line (a block in the terminal's own color would read as full).
    let (track_ch, track) = if t.filled() {
        ("█", Style::default().fg(t.input))
    } else {
        ("·", Style::default().fg(t.faint))
    };
    let mut out = vec![Span::styled(
        "█".repeat(full.min(w)),
        Style::default().fg(c),
    )];
    if full < w {
        if rem > 0 {
            out.push(Span::styled(PART[rem], Style::default().fg(c).bg(t.input)));
            out.push(Span::styled(track_ch.repeat(w - full - 1), track));
        } else {
            out.push(Span::styled(track_ch.repeat(w - full), track));
        }
    }
    out
}

/// The color a level reads as: accent when calm, amber, red when full.
pub(crate) fn level(ratio: f64, t: &Theme) -> Color {
    if ratio >= 0.9 {
        t.bad
    } else if ratio >= 0.75 {
        t.warn
    } else {
        t.brass
    }
}

/// A one-row sparkline of `data` squeezed into `w` cells (each cell the
/// highest sample it covers).
pub(crate) fn spark(data: &[f64], w: usize, max: f64) -> String {
    const BARS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if data.is_empty() || w == 0 {
        return String::new();
    }
    let max = max.max(1e-9);
    (0..w)
        .map(|i| {
            let a = i * data.len() / w;
            let b = ((i + 1) * data.len() / w).max(a + 1).min(data.len());
            let v = data[a..b].iter().copied().fold(0.0, f64::max);
            let l = (v / max * 8.0).round().clamp(0.0, 8.0) as usize;
            BARS[if v > 0.0 { l.max(1) } else { 0 }]
        })
        .collect()
}

// ── the frame ───────────────────────────────────────────────────────────────

/// Where everything goes.
#[derive(Debug, Clone, Copy)]
pub struct Areas {
    /// The top line.
    pub top: Rect,
    /// The strip of mini tiles (empty on the board).
    pub strip: Rect,
    /// The board, the chat, or an open tile.
    pub main: Rect,
    /// The composer (or the approval card, in the chat).
    pub composer: Rect,
}

/// The composer's text width inside a frame this wide.
pub fn text_width(frame_w: u16) -> usize {
    (frame_w as usize).saturating_sub(2 + 4 + 2).max(10)
}

/// Lay the frame out.
pub fn areas(area: Rect, v: &View) -> Areas {
    let area = Rect {
        x: area.x + 1,
        width: area.width.saturating_sub(2),
        ..area
    };
    let screen = v.screen();
    let top = Rect { height: 1, ..area };
    let rows = char_wrap(&v.input, text_width(area.width + 2))
        .len()
        .clamp(1, 6) as u16;
    let composer_h = match (&v.approval, screen) {
        (Some(p), Screen::Chat) => {
            crate::cards::approval_height(p, area.width).min(area.height.saturating_sub(8).max(6))
        }
        (_, Screen::Board) if last_reeve(v).is_some() => rows + 3,
        _ => rows + 2,
    };
    let strip_h = match screen {
        Screen::Board => 0,
        _ if area.width >= STRIP_FULL && area.height >= 24 => 4,
        _ => 1,
    };
    let body_y = area.y + 1;
    let body_h = area.height.saturating_sub(1 + composer_h).max(strip_h + 3);
    let strip = Rect {
        y: body_y,
        height: strip_h,
        ..area
    };
    let main = Rect {
        y: body_y + strip_h,
        height: body_h.saturating_sub(strip_h),
        ..area
    };
    let composer = Rect {
        y: main.bottom(),
        height: area.bottom().saturating_sub(main.bottom()),
        ..area
    };
    Areas {
        top,
        strip,
        main,
        composer,
    }
}

/// The top line: who and where on the left; model, mode, reeved, and the
/// time on the right.
pub fn top_bar(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let mut left = vec![Span::styled("◆ reeve", t.accent()), Span::raw("  ")];
    if !v.host.hostname.is_empty() {
        left.push(Span::styled(
            v.host.hostname.to_lowercase(),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ));
    }
    let mut facts = Vec::new();
    if !v.host.os_short.is_empty() {
        facts.push(v.host.os_short.clone());
    }
    if v.snap.uptime_secs > 0 {
        facts.push(format!("up {}", uptime(v.snap.uptime_secs)));
    }
    if !facts.is_empty() {
        left.push(Span::styled(format!(" · {}", facts.join(" · ")), t.muted()));
    }
    let mut right = Vec::new();
    if v.busy {
        let spin = SPINNER[(v.frame / 2) as usize % SPINNER.len()];
        right.push(Span::styled(
            format!("{spin} working "),
            Style::default().fg(t.amber),
        ));
        right.push(Span::raw(" "));
    }
    if !v.model.is_empty() {
        right.push(Span::styled(
            format!(" {} ", short_model(&v.model)),
            t.pill(t.dim),
        ));
        right.push(Span::raw(" "));
    }
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
    } else if v.auto_undo {
        // Changes Reeve can undo run without asking.
        right.push(Span::styled(" tiered · ↶ auto ", t.pill(t.dim)));
    } else {
        right.push(Span::styled(" tiered ", t.pill(t.dim)));
    }
    right.push(Span::raw(" "));
    if let Some(b) = v.update {
        use reeve_core::update::Badge;
        let text = match b {
            Badge::Available(ver) => format!(" ↑ {ver} "),
            Badge::Restart(ver) => format!(" restart for {ver} "),
        };
        right.push(Span::styled(text, t.pill(t.amber)));
        right.push(Span::raw(" "));
    }
    right.push(if v.observer_alive {
        Span::styled(" reeved ● ", t.pill(t.good))
    } else {
        Span::styled(" reeved ○ ", t.pill(t.faint))
    });
    if let Some(p) = &v.privacy {
        if p.level != reeve_core::privacy::Level::Off {
            right.push(Span::raw(" "));
            right.push(Span::styled(
                format!(" ▣ {} masked ", p.entries.len()),
                t.pill(t.dim),
            ));
        }
    }
    right.push(Span::styled(
        format!("  {}", Local::now().format("%H:%M")),
        Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
    ));
    // The facts give way first, then the pills from the left.
    let avail = area.width as usize;
    while left.len() > 2 && width(&left) + width(&right) + 1 > avail {
        left.pop();
    }
    while right.len() > 1 && width(&left) + width(&right) + 1 > avail {
        right.remove(0);
    }
    f.render_widget(Paragraph::new(split(left, right, avail)), area);
}

// ── the board ───────────────────────────────────────────────────────────────

/// Where each tile sits on the board, by width: four columns, two, or a
/// single column of short tiles. Tiles that don't fit are left off (F1–F8
/// still open them).
pub fn tile_rects(area: Rect) -> Vec<(Tile, Rect)> {
    let (x, y, w, h) = (area.x, area.y, area.width, area.height);
    let mut out = Vec::new();
    let cols = |n: u16| -> Vec<(u16, u16)> {
        // (x, width) of n columns with a one-cell gap.
        let cw = (w.saturating_sub(n - 1)) / n;
        (0..n)
            .map(|i| {
                let cx = x + i * (cw + 1);
                let last = i == n - 1;
                (cx, if last { x + w - cx } else { cw })
            })
            .collect()
    };
    if w >= WIDE && h >= 24 {
        let c = cols(4);
        let span2 = |i: usize| (c[i].0, c[i + 1].0 + c[i + 1].1 - c[i].0);
        let top_h = (h * 36 / 100).clamp(10, 14);
        let low_h = if h >= 34 { 8 } else { 6 };
        let mid_h = h.saturating_sub(top_h + low_h);
        let (a, b) = (span2(0), span2(2));
        out.push((Tile::Needs, Rect::new(a.0, y, a.1, top_h)));
        out.push((Tile::Health, Rect::new(b.0, y, b.1, top_h)));
        let my = y + top_h;
        for (i, tile) in [Tile::Findings, Tile::Activity, Tile::Spend, Tile::Changed]
            .into_iter()
            .enumerate()
        {
            out.push((tile, Rect::new(c[i].0, my, c[i].1, mid_h)));
        }
        let ly = my + mid_h;
        out.push((Tile::Orders, Rect::new(a.0, ly, a.1, low_h)));
        out.push((Tile::Memory, Rect::new(b.0, ly, b.1, low_h)));
    } else if w >= MEDIUM && h >= 16 {
        let c = cols(2);
        let rows: [(&[Tile], u16); 5] = [
            (&[Tile::Needs], 8),
            (&[Tile::Health, Tile::Findings], 9),
            (&[Tile::Spend, Tile::Activity], 7),
            (&[Tile::Changed, Tile::Orders], 6),
            (&[Tile::Memory], 6),
        ];
        // The rows that fit at five high or more, then the height left
        // over goes to health and findings, and the needs row.
        let mut chosen: Vec<(&[Tile], u16)> = Vec::new();
        let mut used = 0u16;
        for (tiles, want) in rows {
            if used + 5 > h {
                break;
            }
            let rh = want.min(h - used);
            chosen.push((tiles, rh));
            used += rh;
        }
        let mut extra = h.saturating_sub(used);
        for i in [1usize, 0, 2] {
            if extra == 0 {
                break;
            }
            if let Some(row) = chosen.get_mut(i) {
                let add = extra.min(4);
                row.1 += add;
                extra -= add;
            }
        }
        if let Some(row) = chosen.get_mut(1) {
            row.1 += extra;
        }
        let mut ry = y;
        for (tiles, rh) in chosen {
            if tiles.len() == 1 {
                out.push((tiles[0], Rect::new(x, ry, w, rh)));
            } else {
                for (i, tile) in tiles.iter().enumerate() {
                    out.push((*tile, Rect::new(c[i].0, ry, c[i].1, rh)));
                }
            }
            ry += rh;
        }
    } else {
        let mut ry = y;
        for tile in Tile::ALL {
            if y + h < ry + 4 {
                break;
            }
            out.push((tile, Rect::new(x, ry, w, 4)));
            ry += 4;
        }
    }
    out
}

/// Draw the board.
pub fn board(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    for (tile, r) in tile_rects(area) {
        let inner = surface(f, r, t.panel, t.bg, t);
        if inner.height == 0 {
            continue;
        }
        let (right, body) = head(tile, v, t);
        f.render_widget(
            Paragraph::new(split(title(tile, false, t), right, inner.width as usize)),
            Rect { height: 1, ..inner },
        );
        let body_r = Rect {
            y: inner.y + 1,
            height: inner.height.saturating_sub(1),
            ..inner
        };
        if body_r.height == 0 {
            continue;
        }
        match body {
            Body::Needs => needs(f, body_r, v, t),
            Body::Lines(lines) => f.render_widget(Paragraph::new(lines(body_r, v, t)), body_r),
        }
    }
}

/// A tile's title: its key and name.
fn title(tile: Tile, open: bool, t: &Theme) -> Vec<Span<'static>> {
    let k = if open {
        Style::default()
            .fg(t.bg)
            .bg(t.brass)
            .add_modifier(Modifier::BOLD)
    } else {
        t.key()
    };
    vec![
        Span::styled(format!(" {} ", tile.fkey()), k),
        Span::styled(
            format!(" {}", tile.label()),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
    ]
}

type LinesFn = fn(Rect, &View, &Theme) -> Vec<Line<'static>>;

enum Body {
    Needs,
    Lines(LinesFn),
}

/// A tile's header meta (flush right) and how its body draws.
fn head(tile: Tile, v: &View, t: &Theme) -> (Vec<Span<'static>>, Body) {
    match tile {
        Tile::Needs => {
            let fixes = fixes(v).len();
            let mut r = Vec::new();
            if v.approval.is_some() {
                r.push(Span::styled(" 1 approval ", t.pill(t.warn)));
            }
            if fixes > 0 {
                r.push(Span::styled(
                    format!("  {fixes} fix{} ready", if fixes == 1 { "" } else { "es" }),
                    t.muted(),
                ));
            }
            (r, Body::Needs)
        }
        Tile::Health => (
            vec![Span::styled(
                if day_report(v).is_some() {
                    "24h"
                } else {
                    "live"
                },
                Style::default().fg(t.brass),
            )],
            Body::Lines(health),
        ),
        Tile::Findings => (
            vec![Span::styled(
                format!("{} open", v.findings.len()),
                t.muted(),
            )],
            Body::Lines(findings),
        ),
        Tile::Activity => (
            vec![Span::styled("receipts", t.muted())],
            Body::Lines(activity),
        ),
        Tile::Spend => (vec![Span::styled("today", t.muted())], Body::Lines(spend)),
        Tile::Changed => (vec![Span::styled("24h", t.muted())], Body::Lines(changed)),
        Tile::Orders => {
            let on = v.board.orders.iter().filter(|o| o.1).count();
            let n = v.board.orders.len();
            (
                vec![Span::styled(
                    if n == 0 {
                        "none yet".to_string()
                    } else if on == 0 {
                        format!("{n} · all off")
                    } else {
                        format!("{on} of {n} on")
                    },
                    t.muted(),
                )],
                Body::Lines(orders),
            )
        }
        Tile::Memory => {
            let new = v.board.notes.iter().filter(|n| n.1).count();
            (
                vec![Span::styled(
                    format!(
                        "{} facts{}",
                        v.board.layers[0],
                        if new > 0 {
                            format!(" · {new} new")
                        } else {
                            String::new()
                        }
                    ),
                    t.muted(),
                )],
                Body::Lines(memory),
            )
        }
    }
}

/// Live findings with a drafted fix.
pub(crate) fn fixes(v: &View) -> Vec<&Finding> {
    v.findings
        .iter()
        .filter(|f| f.is_live() && f.proposal.is_some())
        .collect()
}

/// Findings, worst first, then the most frequent.
pub(crate) fn worst_first(v: &View) -> Vec<&Finding> {
    let mut out: Vec<&Finding> = v.findings.iter().filter(|f| f.is_live()).collect();
    out.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.count.cmp(&a.count)));
    out
}

fn count(n: u64) -> String {
    if n > 1 {
        format!("{n}×")
    } else {
        String::new()
    }
}

/// F1 on the board: the approval asking now, in a raised box with its
/// keys, then the drafted fixes.
fn needs(f: &mut Frame, r: Rect, v: &View, t: &Theme) {
    let w = r.width as usize;
    let mut y = r.y;
    if let Some(p) = &v.approval {
        let box_h = 5.min(r.height);
        let b = Rect {
            x: r.x.saturating_sub(1),
            y,
            width: r.width + 2,
            height: box_h,
        };
        let inner = surface(f, b, t.input, t.panel, t);
        let iw = inner.width as usize;
        let req = &p.req;
        let mut first = vec![
            Span::styled(
                "◐ approve  ",
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ),
            tier_pill(req.tier, t),
            Span::raw("  "),
        ];
        let room = iw.saturating_sub(width(&first));
        first.push(Span::styled(
            truncate(&req.summary, room),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ));
        let second = match (&req.txn, &req.why) {
            (Some(txn), _) => vec![
                Span::styled("verified change ", t.muted()),
                Span::styled(truncate(&txn.goal, iw.saturating_sub(16)), t.text()),
            ],
            (None, Some(why)) => vec![Span::styled(
                truncate(why, iw),
                t.muted().add_modifier(Modifier::ITALIC),
            )],
            (None, None) => vec![Span::styled(
                truncate(&req.reasons.join(" · "), iw),
                t.muted(),
            )],
        };
        let keys = if req.tier == Tier::T3 {
            vec![
                Span::styled("type ", t.muted()),
                Span::styled(
                    "yes",
                    Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
                ),
                Span::styled(" and ⏎   ", t.muted()),
                Span::styled(
                    format!(" {:<4}", p.typed),
                    Style::default().fg(t.fg).bg(t.inset),
                ),
            ]
        } else {
            let mut k = vec![("⏎", "yes")];
            if req.txn.is_some() || req.can_allow_turn {
                k.push(("a", "yes to the rest"));
            }
            if req.can_allow_session {
                k.push(("s", "this session"));
            }
            k.push(("n", "no"));
            k.push(("F1", "details"));
            hints(&k, t)
        };
        f.render_widget(
            Paragraph::new(vec![
                Line::from(first),
                Line::from(second),
                Line::from(keys),
            ]),
            inner,
        );
        y += box_h;
    }
    let mut lines = Vec::new();
    let fx = fixes(v);
    for x in &fx {
        let (icon, c) = severity(x.severity, t);
        let n = count(x.count);
        let right = vec![
            Span::styled(format!("{n}  "), t.muted()),
            Span::styled(" fix ready ", t.pill(t.good)),
        ];
        let room = w.saturating_sub(2 + width(&right) + 1);
        lines.push(split(
            vec![
                Span::styled(format!("{icon} "), Style::default().fg(c)),
                Span::styled(truncate(&x.title, room), t.text()),
            ],
            right,
            w,
        ));
    }
    if v.approval.is_none() && fx.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("✓ ", Style::default().fg(t.good)),
            Span::styled("nothing needs you", t.text()),
        ]));
        lines.push(Line::from(Span::styled(
            "approvals and drafted fixes land here",
            t.ghost(),
        )));
    }
    let rest = Rect {
        y,
        height: r.bottom().saturating_sub(y),
        ..r
    };
    f.render_widget(Paragraph::new(lines), rest);
}

/// The last day's report, when reeved recorded some of it.
fn day_report(v: &View) -> Option<&reeve_observer::report::Report> {
    v.board.report.as_deref().filter(|r| r.coverage > 0.0)
}

/// The last day's samples of one reading, from the report.
fn day_series(v: &View, pick: fn(&reeve_observer::report::Point) -> f64) -> Vec<f64> {
    day_report(v).map_or_else(Vec::new, |r| {
        r.points
            .iter()
            .map(|(_, p)| p.as_ref().map_or(0.0, pick))
            .collect()
    })
}

fn health(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let s = &v.snap;
    let w = r.width as usize;
    let mut out = Vec::new();
    // CPU: a braille chart of the last day (or the last minutes, live).
    let day = day_series(v, |p| p.cpu);
    let (data, max): (Vec<f32>, f32) = if day.iter().any(|x| *x > 0.0) {
        let peak = day.iter().copied().fold(0.0, f64::max) as f32;
        (
            day.iter().map(|x| *x as f32).collect(),
            (peak * 1.2).max(10.0),
        )
    } else {
        (v.cpu_hist.iter().copied().collect(), 100.0)
    };
    let chart_w = w.saturating_sub(13).max(8);
    let chart_h = if r.height >= 8 { 3 } else { 2 };
    let n = chart_w * 2;
    let sampled: Vec<f32> = (0..n)
        .map(|i| {
            if data.is_empty() {
                return 0.0;
            }
            let a = i * data.len() / n;
            let b = ((i + 1) * data.len() / n).max(a + 1).min(data.len());
            data[a..b].iter().copied().fold(0.0, f32::max)
        })
        .collect();
    let chart = braille(&sampled, max, chart_w, chart_h, t);
    let (avg, peak) = day_report(v).map_or((None, None), |r| (Some(r.cpu.avg), Some(r.cpu.max)));
    let side: [Vec<Span<'static>>; 3] = [
        vec![
            Span::styled("cpu  ", t.muted()),
            Span::styled(
                s.cpu_pct.map_or("  …".into(), |c| format!("{c:>3.0}%")),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ),
        ],
        vec![Span::styled(
            match (avg, peak) {
                (Some(a), Some(p)) => format!("avg {a:.0} · {p:.0}"),
                _ => String::new(),
            },
            t.ghost(),
        )],
        vec![Span::styled(format!("load {:.2}", s.load[0]), t.ghost())],
    ];
    for (i, row) in chart.into_iter().enumerate() {
        let mut spans = side.get(i).cloned().unwrap_or_default();
        let pad_w = 13usize.saturating_sub(width(&spans));
        spans.push(Span::raw(" ".repeat(pad_w)));
        spans.extend(row);
        out.push(Line::from(spans));
    }
    if r.height >= 9 {
        out.push(Line::raw(""));
    }
    // Memory and swap, temperature and network: two columns.
    let half = w.saturating_sub(3) / 2;
    let paired = w >= 70;
    let bar_w = if paired {
        half.saturating_sub(5 + 5 + 9)
    } else {
        w.saturating_sub(5 + 5 + 12)
    }
    .max(4);
    let mut mem = vec![Span::styled("mem  ", t.muted())];
    let mut swap = vec![Span::styled("swap ", t.muted())];
    if s.mem_total > 0 {
        let rm = s.mem_used as f64 / s.mem_total as f64;
        mem.push(Span::styled(
            format!("{:>3.0}% ", rm * 100.0),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ));
        mem.extend(meter(rm, bar_w, level(rm, t), t));
        mem.push(Span::styled(
            format!(" {}/{}", gib(s.mem_used), gib(s.mem_total)),
            t.muted(),
        ));
    }
    if s.swap_total > 0 {
        let rs = s.swap_used as f64 / s.swap_total as f64;
        swap.push(Span::styled(
            format!("{:>3.0}% ", rs * 100.0),
            Style::default()
                .fg(level(rs, t))
                .add_modifier(Modifier::BOLD),
        ));
        swap.extend(meter(rs, bar_w, level(rs, t), t));
        swap.push(Span::styled(
            format!(" {}/{}", gib(s.swap_used), gib(s.swap_total)),
            t.muted(),
        ));
    } else {
        swap.push(Span::styled("none", t.ghost()));
    }
    let spark_w = half.saturating_sub(5 + 5 + 8).max(4);
    let temps = day_series(v, |p| p.temp.unwrap_or(0.0));
    let mut temp = vec![Span::styled("temp ", t.muted())];
    match s.temp_c {
        Some(c) => {
            temp.push(Span::styled(
                format!("{c:>3.0}° "),
                Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
            ));
            temp.push(Span::styled(
                spark(&temps, spark_w, 100.0),
                Style::default().fg(t.violet),
            ));
            if let Some(tp) = day_report(v).and_then(|r| r.temp) {
                temp.push(Span::styled(format!(" pk {:.0}°", tp.max), t.muted()));
            }
        }
        None => temp.push(Span::styled("—", t.ghost())),
    }
    let rx = day_series(v, |p| p.rx);
    let mut net = vec![Span::styled("net  ", t.muted())];
    if let (Some(down), Some(up)) = (s.net_rx_bps, s.net_tx_bps) {
        net.push(Span::styled(
            format!("↓{} ", rate(down)),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ));
        let top = rx.iter().copied().fold(0.0, f64::max);
        net.push(Span::styled(
            spark(&rx, spark_w.saturating_sub(3), top),
            Style::default().fg(t.brass),
        ));
        net.push(Span::styled(format!(" ↑{}", rate(up)), t.muted()));
    }
    let pair = |a: Vec<Span<'static>>, b: Vec<Span<'static>>| {
        let mut spans = a;
        let aw = width(&spans);
        spans.push(Span::raw(" ".repeat((half + 3).saturating_sub(aw))));
        spans.extend(b);
        Line::from(spans)
    };
    if paired {
        out.push(pair(mem, temp));
        out.push(pair(swap, net));
    } else {
        out.push(Line::from(mem));
        out.push(Line::from(swap));
    }
    if r.height >= 9 {
        out.push(Line::raw(""));
    }
    // Disks, on one line: as many as fit, fullest first when they don't.
    let mut disks = vec![Span::styled("disks  ", t.muted())];
    let failed = v.failed.as_ref().map_or(0, Vec::len);
    let tail = if failed > 0 {
        vec![Span::styled(
            format!("✗ {failed} failed"),
            Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
        )]
    } else {
        Vec::new()
    };
    let mut order: Vec<&reeve_observer::Disk> = s.disks.iter().collect();
    let all_w: usize = order.iter().map(|d| d.mount.width() + 16).sum();
    if all_w + 7 + width(&tail) > w {
        order.sort_by(|a, b| b.ratio().total_cmp(&a.ratio()));
    }
    for d in order {
        let ratio = d.ratio();
        let item = vec![
            Span::styled(format!("{} ", d.mount), t.text()),
            Span::styled(
                format!("{:.0}%", ratio * 100.0),
                Style::default().fg(if ratio >= 0.9 {
                    t.bad
                } else if ratio >= 0.75 {
                    t.warn
                } else {
                    t.good
                }),
            ),
            Span::styled(format!(" of {}    ", gib(d.total)), t.ghost()),
        ];
        if width(&disks) + width(&item) + width(&tail) > w {
            break;
        }
        disks.extend(item);
    }
    disks.extend(tail);
    out.push(Line::from(disks));
    out
}

fn findings(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    let list = worst_first(v);
    let h = r.height as usize;
    let mut out = Vec::new();
    if list.is_empty() {
        out.push(Line::from(vec![
            Span::styled("✓ ", Style::default().fg(t.good)),
            Span::styled("nothing open", t.text()),
        ]));
        if !v.observer_alive {
            out.push(Line::from(Span::styled(
                "reeved isn't running: /observer",
                t.ghost(),
            )));
        }
        return out;
    }
    let shown = if list.len() > h {
        h.saturating_sub(1)
    } else {
        list.len()
    };
    for x in list.iter().take(shown) {
        let (icon, c) = severity(x.severity, t);
        let mut right = Vec::new();
        if x.proposal.is_some() {
            right.push(Span::styled("fix ", Style::default().fg(t.good)));
        }
        right.push(Span::styled(count(x.count), t.muted()));
        let room = w.saturating_sub(2 + width(&right) + 1);
        out.push(split(
            vec![
                Span::styled(format!("{icon} "), Style::default().fg(c)),
                Span::styled(truncate(&x.title, room), t.text()),
            ],
            right,
            w,
        ));
    }
    if list.len() > shown {
        out.push(Line::from(Span::styled(
            format!("  + {} more", list.len() - shown),
            t.ghost(),
        )));
    }
    out
}

/// A receipt in a few words: its tool and target.
pub(crate) fn receipt_title(r: &reeve_core::receipts::Receipt) -> String {
    let target = r.target();
    if target.is_empty() {
        r.tool.clone()
    } else {
        format!("{} {target}", r.tool)
    }
}

fn activity(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    if v.receipts.is_empty() {
        return vec![Line::from(Span::styled(
            "nothing yet this session",
            t.ghost(),
        ))];
    }
    v.receipts
        .iter()
        .take(r.height as usize)
        .map(|rc| {
            let (icon, c) = match rc.outcome.status {
                Status::Ok => ("✓", t.good),
                Status::Error => ("✗", t.bad),
                Status::Denied => ("⊘", t.warn),
                Status::Refused => ("⊗", t.bad),
            };
            let undo = rc.undo.is_some() && rc.outcome.status == Status::Ok && !v.is_undone(rc.seq);
            let right = if undo {
                vec![Span::styled("↶", Style::default().fg(t.brass))]
            } else {
                Vec::new()
            };
            let time = rc.ts.with_timezone(&Local).format("%H:%M ").to_string();
            let room = w.saturating_sub(6 + 2 + 2);
            split(
                vec![
                    Span::styled(time, t.ghost()),
                    Span::styled(format!("{icon} "), Style::default().fg(c)),
                    Span::styled(
                        truncate(&receipt_title(rc), room),
                        if rc.tier == Tier::T0 {
                            t.muted()
                        } else {
                            t.text()
                        },
                    ),
                ],
                right,
                w,
            )
        })
        .collect()
}

/// A role's color in spend bars.
pub(crate) fn role_color(role: &str, t: &Theme) -> Color {
    match role {
        "chat" => t.brass,
        "drafter" => t.violet,
        "reflect" => t.good,
        "orders" => t.amber,
        _ => t.dim,
    }
}

fn spend(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    let today = &v.totals.today;
    let mut out = vec![Line::from(vec![
        Span::styled(
            format_usd(Some(today.usd)),
            Style::default().fg(t.fg).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {} calls", today.calls), t.muted()),
    ])];
    let hours = &v.board.hours;
    if !hours.is_empty() && r.height >= 6 {
        let now = Local::now()
            .format("%H")
            .to_string()
            .parse::<usize>()
            .unwrap_or(23);
        let top = hours.iter().copied().fold(0.0, f64::max);
        let so_far = spark(&hours[..=now.min(hours.len() - 1)], (now + 1).min(w), top);
        out.push(Line::from(vec![
            Span::styled(so_far, Style::default().fg(t.brass)),
            Span::styled(
                "·".repeat(23usize.saturating_sub(now).min(w.saturating_sub(now + 1))),
                t.ghost(),
            ),
        ]));
    }
    if r.height >= 7 {
        out.push(Line::raw(""));
    }
    let top = v
        .board
        .roles
        .iter()
        .map(|x| x.1)
        .fold(0.0, f64::max)
        .max(1e-9);
    let bar_w = w.saturating_sub(8 + 7).max(3);
    for (role, usd, _) in v
        .board
        .roles
        .iter()
        .take(r.height.saturating_sub(3) as usize)
    {
        let mut spans = vec![Span::styled(pad(role, 8), t.muted())];
        spans.extend(meter(usd / top, bar_w, role_color(role, t), t));
        spans.push(Span::styled(format!(" {:>5}", short_usd(*usd)), t.muted()));
        out.push(Line::from(spans));
    }
    let (month, _) = v.board.month;
    let mut foot = format!("month {}", format_usd(Some(month)));
    if let Some(c) = v.board.cached {
        foot.push_str(&format!(" · {:.0}% cached", c * 100.0));
    }
    if (out.len() as u16) < r.height {
        out.push(Line::from(Span::styled(truncate(&foot, w), t.ghost())));
    }
    out
}

/// `$0.83` → `.83`; `$12.40` → `12.4`: fits a narrow column.
fn short_usd(usd: f64) -> String {
    if usd < 1.0 {
        format!("{usd:.2}").trim_start_matches('0').to_string()
    } else if usd < 100.0 {
        format!("{usd:.1}")
    } else {
        format!("{usd:.0}")
    }
}

fn changed(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    let Some(rep) = &v.board.report else {
        return vec![Line::from(Span::styled("gathering…", t.ghost()))];
    };
    let d = &rep.drift;
    let mut out = Vec::new();
    let row = |icon: &str, c: Color, head: String, rest: String| {
        Line::from(vec![
            Span::styled(format!("{icon} "), Style::default().fg(c)),
            Span::styled(head.clone(), t.text()),
            Span::styled(
                truncate(&rest, w.saturating_sub(2 + head.width())),
                t.muted(),
            ),
        ])
    };
    let pk = d.installed.len() + d.upgraded.len() + d.touched.len();
    if pk > 0 {
        out.push(row(
            "↑",
            t.good,
            format!("{pk} package{}", if pk == 1 { "" } else { "s" }),
            if d.removed.is_empty() {
                String::new()
            } else {
                format!(" · {} removed", d.removed.len())
            },
        ));
    }
    if let Some(k) = &d.reboot_for {
        out.push(row("◆", t.brass, format!("kernel {k}"), " · reboot".into()));
    }
    if d.etc_total > 0 {
        let mine = d.etc.iter().filter(|e| e.by_reeve.is_some()).count();
        out.push(row(
            "✎",
            t.dim,
            format!("{} in /etc", d.etc_total),
            if mine > 0 {
                format!(" · {mine} by Reeve")
            } else {
                " · none by Reeve".into()
            },
        ));
    }
    if d.baseline.is_some() {
        let n = d.units_enabled.len() + d.units_disabled.len();
        if n > 0 {
            out.push(row(
                "○",
                t.dim,
                format!("{n} services"),
                " enabled or disabled".into(),
            ));
        }
    } else {
        out.push(row("○", t.faint, "services".into(), ": tomorrow".into()));
    }
    if out.is_empty() {
        out.push(Line::from(vec![
            Span::styled("✓ ", Style::default().fg(t.good)),
            Span::styled("nothing changed", t.text()),
        ]));
    }
    out
}

fn orders(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    if v.board.orders.is_empty() {
        return vec![
            Line::from(Span::styled("no standing orders", t.text())),
            Line::from(Span::styled(
                "F7, then n: work Reeve does on its own",
                t.ghost(),
            )),
        ];
    }
    v.board
        .orders
        .iter()
        .take(r.height as usize)
        .map(|(name, on, when)| {
            let right = vec![Span::styled(when.clone(), t.muted())];
            let room = w.saturating_sub(2 + width(&right) + 2);
            split(
                vec![
                    Span::styled(
                        if *on { "● " } else { "○ " },
                        Style::default().fg(if *on { t.good } else { t.faint }),
                    ),
                    Span::styled(truncate(name, room), t.text()),
                ],
                right,
                w,
            )
        })
        .collect()
}

fn memory(r: Rect, v: &View, t: &Theme) -> Vec<Line<'static>> {
    let w = r.width as usize;
    let h = r.height as usize;
    let mut out: Vec<Line<'static>> = v
        .board
        .notes
        .iter()
        .take(h.saturating_sub(1).max(1))
        .map(|(title, new)| {
            Line::from(vec![
                Span::styled(
                    if *new { "● " } else { "✓ " },
                    Style::default().fg(if *new { t.brass } else { t.faint }),
                ),
                Span::styled(
                    truncate(title, w.saturating_sub(2)),
                    if *new { t.text() } else { t.muted() },
                ),
            ])
        })
        .collect();
    if out.is_empty() {
        out.push(Line::from(Span::styled("nothing learned yet", t.ghost())));
    }
    let [_, runbooks, prefs, baselines] = v.board.layers;
    if out.len() < h {
        out.push(Line::from(Span::styled(
            truncate(
                &format!("runbooks {runbooks} · preferences {prefs} · baselines {baselines}"),
                w,
            ),
            t.ghost(),
        )));
    }
    out
}

// ── the strip ───────────────────────────────────────────────────────────────

/// Where each mini tile sits in the strip.
pub fn strip_rects(area: Rect) -> Vec<(Tile, Rect)> {
    if area.height >= 4 {
        let n = Tile::ALL.len() as u16;
        let cw = area.width.saturating_sub(n - 1) / n;
        Tile::ALL
            .iter()
            .enumerate()
            .map(|(i, tile)| {
                let x = area.x + i as u16 * (cw + 1);
                let wd = if i as u16 == n - 1 {
                    area.right() - x
                } else {
                    cw
                };
                (*tile, Rect::new(x, area.y, wd, 4))
            })
            .collect()
    } else {
        // One row: `F1 ◐ approve  F2 swap 100%  …`, each as wide as it is.
        let mut x = area.x;
        let mut out = Vec::new();
        for tile in Tile::ALL {
            let w = (tile.fkey().width() + 2 + 1 + tile.label().width()) as u16;
            if x + w > area.right() {
                break;
            }
            out.push((tile, Rect::new(x, area.y, w, 1)));
            x += w + 2;
        }
        out
    }
}

/// The live number a mini tile shows.
fn mini(tile: Tile, v: &View, t: &Theme) -> Vec<Span<'static>> {
    let s = &v.snap;
    match tile {
        Tile::Needs => {
            if v.approval.is_some() {
                vec![Span::styled(
                    "◐ 1 approval",
                    Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
                )]
            } else {
                let n = fixes(v).len();
                if n > 0 {
                    vec![Span::styled(
                        format!("{n} fix{} ready", if n == 1 { "" } else { "es" }),
                        Style::default().fg(t.good),
                    )]
                } else {
                    vec![Span::styled("✓ nothing now", t.ghost())]
                }
            }
        }
        Tile::Health => {
            let swap = (s.swap_total > 0).then(|| s.swap_used as f64 / s.swap_total as f64);
            let cpu = s.cpu_pct.map_or(String::new(), |c| format!("cpu {c:.0}%"));
            match swap {
                Some(r) if r >= 0.9 => vec![
                    Span::styled(
                        format!("swap {:.0}%", r * 100.0),
                        Style::default().fg(t.bad),
                    ),
                    Span::styled(format!(" {cpu}"), t.muted()),
                ],
                _ => {
                    let mem = if s.mem_total > 0 {
                        format!(
                            " mem {:.0}%",
                            s.mem_used as f64 / s.mem_total as f64 * 100.0
                        )
                    } else {
                        String::new()
                    };
                    vec![Span::styled(format!("{cpu}{mem}"), t.muted())]
                }
            }
        }
        Tile::Findings => {
            let n = v.findings.len();
            let f = fixes(v).len();
            let mut out = vec![
                Span::styled(n.to_string(), t.text()),
                Span::styled(" open", t.muted()),
            ];
            if f > 0 {
                out.push(Span::styled(" · ", t.ghost()));
                out.push(Span::styled(
                    format!("{f} fix"),
                    Style::default().fg(t.good),
                ));
            }
            out
        }
        Tile::Activity => match v.receipts.first() {
            Some(r) => vec![
                Span::styled(format!("#{} ", r.seq), t.ghost()),
                Span::styled(receipt_title(r), t.muted()),
            ],
            None => vec![Span::styled("nothing yet", t.ghost())],
        },
        Tile::Spend => {
            let top = v.board.hours.iter().copied().fold(0.0, f64::max);
            vec![
                Span::styled(format_usd(Some(v.totals.today.usd)), t.text()),
                Span::styled(
                    format!(" {}", spark(&v.board.hours, 8, top)),
                    Style::default().fg(t.brass),
                ),
            ]
        }
        Tile::Changed => match &v.board.report {
            Some(r) => {
                let d = &r.drift;
                let pk = d.installed.len() + d.upgraded.len() + d.touched.len();
                let mut out = Vec::new();
                if pk > 0 {
                    out.push(Span::styled(format!("{pk} pkgs"), t.muted()));
                }
                if d.reboot_for.is_some() {
                    if !out.is_empty() {
                        out.push(Span::styled(" · ", t.ghost()));
                    }
                    out.push(Span::styled("reboot", Style::default().fg(t.brass)));
                }
                if out.is_empty() {
                    out.push(Span::styled("quiet", t.ghost()));
                }
                out
            }
            None => vec![Span::styled("…", t.ghost())],
        },
        Tile::Orders => {
            let on = v.board.orders.iter().filter(|o| o.1).count();
            let n = v.board.orders.len();
            vec![Span::styled(
                if n == 0 {
                    "none".into()
                } else if on == 0 {
                    format!("{n} · all off")
                } else {
                    format!("{on} of {n} on")
                },
                t.muted(),
            )]
        }
        Tile::Memory => {
            let new = v.board.notes.iter().filter(|n| n.1).count();
            vec![Span::styled(
                if new > 0 {
                    format!("{new} new facts")
                } else {
                    format!("{} facts", v.board.layers[0])
                },
                t.muted(),
            )]
        }
    }
}

/// The strip: every tile but the open one's number, live.
pub fn strip(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let open = match v.screen() {
        Screen::Tile(tile) => Some(tile),
        _ => None,
    };
    let full = area.height >= 4;
    for (tile, r) in strip_rects(area) {
        let on = open == Some(tile);
        if full {
            let fill = if on { t.tint(t.brass) } else { t.panel };
            let inner = surface(f, r, fill, t.bg, t);
            let iw = inner.width as usize;
            let mut label = title(tile, on, t);
            if label.iter().map(|s| s.content.width()).sum::<usize>() > iw {
                label.truncate(1);
            }
            let mut value = mini(tile, v, t);
            // Keep the value to the width.
            let mut room = iw;
            value.retain_mut(|s| {
                let sw = s.content.width();
                if room == 0 {
                    return false;
                }
                if sw > room {
                    s.content = truncate(&s.content, room).into();
                }
                room = room.saturating_sub(sw);
                true
            });
            f.render_widget(
                Paragraph::new(vec![Line::from(label), Line::from(value)]),
                inner,
            );
        } else {
            let k = if on {
                Style::default()
                    .fg(t.bg)
                    .bg(t.brass)
                    .add_modifier(Modifier::BOLD)
            } else {
                t.key()
            };
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(format!(" {} ", tile.fkey()), k),
                    Span::styled(
                        format!(" {}", tile.label()),
                        if on {
                            Style::default().fg(t.fg).add_modifier(Modifier::BOLD)
                        } else {
                            t.muted()
                        },
                    ),
                ])),
                r,
            );
        }
    }
}

// ── the composer ────────────────────────────────────────────────────────────

/// Reeve's last words this session, for the board's composer.
fn last_reeve(v: &View) -> Option<&crate::view::Entry> {
    v.entries
        .iter()
        .rev()
        .find(|e| e.who == crate::view::Speaker::Reeve && !e.text.trim().is_empty())
}

/// What a question typed on a tile's screen is about.
pub fn context(v: &View) -> Option<String> {
    use crate::overlay::{Need, Overlay};
    let top = v.overlays.first()?;
    Some(match top {
        Overlay::Findings(p) => format!("the finding \"{}\"", p.selected()?.title),
        Overlay::Needs(p) => match p.selected()? {
            Need::Approval => format!("the approval \"{}\"", v.approval.as_ref()?.req.summary),
            Need::Fix(i) => format!("the drafted fix for \"{}\"", p.fixes.get(i)?.title),
            Need::Reboot => format!("rebooting into kernel {}", p.reboot.clone()?),
        },
        Overlay::Receipts(p) => {
            let r = p.selected()?;
            format!("receipt #{} ({})", r.seq, receipt_title(r))
        }
        Overlay::Spend(p) => format!("spending this {}", p.range.label()),
        Overlay::System(p) if p.changed => {
            format!("what changed on this machine in the last {} day(s)", p.days)
        }
        Overlay::System(_) => "this machine's health".into(),
        Overlay::Orders(p) => format!("the standing order \"{}\"", p.selected()?.name),
        Overlay::Memory(p) => format!("the memory note \"{}\"", p.selected()?.title),
        _ => return None,
    })
}

/// The composer's context chip: a short `about:`.
fn chip(v: &View) -> Option<String> {
    use crate::overlay::{Need, Overlay};
    let top = v.overlays.first()?;
    Some(match top {
        Overlay::Findings(p) => p.selected()?.title.clone(),
        Overlay::Needs(p) => match p.selected()? {
            Need::Approval => "this approval".into(),
            Need::Fix(i) => p.fixes.get(i)?.title.clone(),
            Need::Reboot => "the reboot".into(),
        },
        Overlay::Receipts(p) => format!("receipt #{}", p.selected()?.seq),
        Overlay::Spend(_) => "spend".into(),
        Overlay::System(p) if p.changed => "what changed".into(),
        Overlay::System(_) => "health".into(),
        Overlay::Orders(p) => p.selected()?.name.clone(),
        Overlay::Memory(p) => p.selected()?.title.clone(),
        _ => return None,
    })
}

/// Draw the composer: Reeve's last words (on the board), then the prompt.
pub fn composer(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let inner = surface(f, area, t.panel, t.bg, t);
    if inner.height == 0 {
        return;
    }
    let w = inner.width as usize;
    let screen = v.screen();
    let mut y = inner.y;
    if screen == Screen::Board {
        if let Some(e) = last_reeve(v) {
            let right = vec![
                Span::styled(format!("session {}  ", v.session.label()), t.ghost()),
                key("↑", t),
                Span::styled(" the chat", t.ghost()),
            ];
            let first = e
                .text
                .lines()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("")
                .replace("**", "");
            let room = w.saturating_sub(8 + 7 + width(&right) + 2);
            let line = split(
                vec![
                    Span::styled(
                        "◆ reeve ",
                        Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(e.at.format("%H:%M  ").to_string(), t.ghost()),
                    Span::styled(truncate(&first, room), t.muted()),
                ],
                right,
                w,
            );
            f.render_widget(
                Paragraph::new(line),
                Rect {
                    y,
                    height: 1,
                    ..inner
                },
            );
            y += 1;
        }
    }
    let body = Rect {
        y,
        height: inner.bottom().saturating_sub(y),
        ..inner
    };
    let tile_open = matches!(screen, Screen::Tile(_));
    let typing = !tile_open || v.composing;
    let prompt = Span::styled(
        "› ",
        Style::default()
            .fg(if v.busy { t.faint } else { t.brass })
            .add_modifier(Modifier::BOLD),
    );
    let mut lead = vec![prompt];
    if tile_open {
        if let Some(c) = chip(v) {
            lead.push(Span::styled(
                format!(" about: {} ", truncate(&c, 32)),
                t.pill(t.dim),
            ));
            lead.push(Span::raw(" "));
        }
    }
    let lead_w = width(&lead);
    let text_w = w.saturating_sub(lead_w).max(4);
    let right = match screen {
        _ if v.busy => hints(&[("esc", "stop")], t),
        Screen::Board if v.approval.is_some() => {
            hints(&[("⏎", "yes"), ("n", "no"), ("F1", "details")], t)
        }
        Screen::Board => hints(
            &[
                ("F1–F8", "open a tile"),
                ("↑", "the chat"),
                ("ctrl+k", "search"),
            ],
            t,
        ),
        Screen::Chat => hints(
            &[
                ("esc", "the board"),
                ("F1–F8", "a tile"),
                ("ctrl+k", "search"),
            ],
            t,
        ),
        Screen::Tile(_) if v.composing => hints(&[("⏎", "ask"), ("esc", "cancel")], t),
        Screen::Tile(_) => hints(
            &[("esc", "back"), ("F1–F8", "a tile"), ("ctrl+k", "search")],
            t,
        ),
    };
    let rows = char_wrap(&v.input, text_w);
    let mut lines: Vec<Line> = Vec::new();
    if v.input.is_empty() || !typing {
        let hint = if v.busy {
            "Reeve is working…"
        } else if !v.ready {
            "Type /providers to add an API key and pick a model."
        } else if tile_open && !v.composing {
            "? ask about this · / commands"
        } else if screen == Screen::Board && v.approval.is_some() {
            "answer the approval above first"
        } else {
            "ask Reeve anything · / commands"
        };
        let mut left = lead.clone();
        left.push(Span::styled(hint, t.ghost()));
        lines.push(split(left, right, w));
    } else {
        for (i, row) in rows.iter().enumerate() {
            let mut spans = if i == 0 {
                lead.clone()
            } else {
                vec![Span::raw(" ".repeat(lead_w))]
            };
            spans.push(Span::styled(row.clone(), t.text()));
            lines.push(Line::from(spans));
        }
    }
    let visible = body.height as usize;
    let skip = lines.len().saturating_sub(visible);
    f.render_widget(
        Paragraph::new(lines.into_iter().skip(skip).collect::<Vec<_>>()),
        body,
    );
    let floating = v.overlays.iter().any(|o| o.tile().is_none());
    if typing && !v.busy && !floating && (v.approval.is_none() || screen != Screen::Board) {
        let (row, col) = cursor_pos(&v.input[..v.cursor], text_w);
        let row = row.saturating_sub(skip);
        f.set_cursor_position(Position::new(
            body.x + (lead_w + col) as u16,
            body.y + row.min(visible.saturating_sub(1)) as u16,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_board_lays_out_every_tile_when_wide() {
        let r = tile_rects(Rect::new(0, 0, 158, 38));
        assert_eq!(r.len(), 8);
        for (_, a) in &r {
            assert!(a.right() <= 158 && a.bottom() <= 38, "{a:?}");
        }
        // Nothing overlaps.
        for (i, (_, a)) in r.iter().enumerate() {
            for (_, b) in &r[i + 1..] {
                assert!(a.intersection(*b).is_empty(), "{a:?} {b:?}");
            }
        }
    }

    #[test]
    fn narrow_boards_keep_what_fits() {
        let r = tile_rects(Rect::new(0, 0, 98, 26));
        assert!(r.iter().any(|(t, _)| *t == Tile::Needs));
        assert!(r.iter().all(|(_, a)| a.bottom() <= 26));
        // The medium board uses the whole height.
        assert_eq!(r.iter().map(|(_, a)| a.bottom()).max(), Some(26));
        let r = tile_rects(Rect::new(0, 0, 50, 20));
        assert_eq!(r.len(), 5);
    }

    #[test]
    fn the_strip_folds_to_one_row_of_keys() {
        assert_eq!(strip_rects(Rect::new(0, 0, 158, 4)).len(), 8);
        let one = strip_rects(Rect::new(0, 0, 100, 1));
        assert!(!one.is_empty() && one.iter().all(|(_, r)| r.height == 1 && r.right() <= 100));
    }

    #[test]
    fn sparks_and_meters_fit() {
        assert_eq!(spark(&[0.0, 1.0, 2.0, 4.0], 4, 4.0).chars().count(), 4);
        let t = Theme::slate();
        let m = meter(0.27, 16, t.brass, &t);
        assert_eq!(m.iter().map(|s| s.content.width()).sum::<usize>(), 16);
        let m = meter(1.0, 16, t.bad, &t);
        assert_eq!(m.iter().map(|s| s.content.width()).sum::<usize>(), 16);
    }
}
