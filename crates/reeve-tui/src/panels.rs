//! Drawing the floating panels and the slash-command palette.

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use reeve_core::spend::{format_tokens, trim_rate};

use crate::draw::{pad, panel, truncate};
use crate::overlay::{AddForm, FIELDS, KINDS, KeyEntry, ModelPicker, Overlay, Providers, palette};
use crate::theme::Theme;
use crate::view::View;

/// The top panel, centered over everything.
pub fn draw_overlay(f: &mut Frame, v: &View, t: &Theme) {
    let Some(top) = v.overlays.last() else {
        return;
    };
    // Tiles draw as screens, not floating panels.
    if top.tile().is_some() {
        return;
    }
    let area = f.area();
    crate::board::dim(f, t);
    if let Overlay::Everything(e) = top {
        everything(f, area, e, t);
        return;
    }
    let (w, h) = match top {
        Overlay::Providers(p) => (100, p.rows.len() as u16 + 12),
        Overlay::Key(_) => (84, 11),
        Overlay::Models(_) => (104, area.height.saturating_sub(6).min(32)),
        Overlay::Add(_) => (92, 15),
        Overlay::Receipts(_) => (
            area.width.saturating_sub(6).min(130),
            area.height.saturating_sub(4),
        ),
        Overlay::Password(_) => (80, 13),
        Overlay::Memory(_) | Overlay::Findings(_) => (
            area.width.saturating_sub(6).min(130),
            area.height.saturating_sub(4),
        ),
        Overlay::Observer(_) => (92, 26),
        Overlay::Privacy(_) => (88, 28),
        Overlay::Orders(_) => (
            area.width.saturating_sub(6).min(130),
            area.height.saturating_sub(4),
        ),
        Overlay::Help => (84, 36),
        Overlay::Spend(_) | Overlay::System(_) | Overlay::Everything(_) | Overlay::Needs(_) => {
            (0, 0)
        }
    };
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let title = match top {
        Overlay::Providers(_) => "providers",
        Overlay::Key(_) => "api key",
        Overlay::Models(_) => "model",
        Overlay::Add(_) => "add a connection",
        Overlay::Receipts(_) => "receipts",
        Overlay::Password(_) => "sudo",
        Overlay::Memory(_) => "memory",
        Overlay::Findings(_) => "findings",
        Overlay::Observer(_) => "observer",
        Overlay::Privacy(_) => "privacy",
        Overlay::Orders(_) => "standing orders",
        Overlay::Help => "help",
        Overlay::Spend(_) => "spend",
        Overlay::System(_) => "system",
        Overlay::Everything(_) => "everything",
        Overlay::Needs(_) => "needs you",
    };
    let block = panel(title, t, true);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 2,
        width: inner.width.saturating_sub(4),
        y: inner.y + 1,
        height: inner.height.saturating_sub(1),
    };
    match top {
        Overlay::Providers(p) => providers(f, inner, p, t),
        Overlay::Key(k) => key_entry(f, inner, k, t),
        Overlay::Models(m) => models(f, inner, m, t),
        Overlay::Add(a) => add_form(f, inner, a, t),
        Overlay::Receipts(_) => {}
        Overlay::Password(p) => password(f, inner, p, t),
        Overlay::Memory(_) => {}
        Overlay::Findings(_) => {}
        Overlay::Observer(o) => observer(f, inner, o, t),
        Overlay::Privacy(p) => privacy(f, inner, p, t),
        Overlay::Orders(_) => {}
        Overlay::Help => help(f, inner, t),
        Overlay::Spend(_) | Overlay::System(_) | Overlay::Everything(_) | Overlay::Needs(_) => {}
    }
}

/// ⌃K: one search over everything, grouped by what you can do with it.
fn everything(f: &mut Frame, area: Rect, e: &crate::overlay::EverythingPanel, t: &Theme) {
    let hits = e.matches();
    let shown = hits.len().min(12);
    let w = area.width.saturating_sub(8).min(104);
    let h = (shown.max(1) as u16 + 7).min(area.height.saturating_sub(4));
    let r = Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height / 6).min(area.height.saturating_sub(h)),
        width: w,
        height: h,
    };
    f.render_widget(Clear, r);
    crate::board::shadow(f, r, t);
    let block = ratatui::widgets::Block::default()
        .borders(ratatui::widgets::Borders::ALL)
        .border_type(ratatui::widgets::BorderType::Rounded)
        .border_style(
            Style::default()
                .fg(t.mix(t.border, t.brass, 0.45))
                .bg(t.panel),
        )
        .style(Style::default().bg(t.panel));
    let inner = block.inner(r);
    f.render_widget(block, r);
    let inner = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let iw = inner.width as usize;
    let found = hits.len() - usize::from(!e.query.trim().is_empty());
    let mut lines = vec![crate::board::split(
        vec![
            Span::styled(
                "⌕ ",
                Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
            ),
            Span::styled(e.query.clone(), Style::default().fg(t.fg)),
            Span::styled(" ", Style::default().bg(t.brass)),
        ],
        vec![Span::styled(
            format!("{found} of {}", e.items.len()),
            t.ghost(),
        )],
        iw,
    )];
    lines.push(Line::from(Span::styled(
        "─".repeat(iw),
        Style::default().fg(t.border),
    )));
    let sel = e.sel.min(hits.len().saturating_sub(1));
    let start = sel.saturating_sub(shown.saturating_sub(1));
    for (i, hit) in hits.iter().enumerate().skip(start).take(shown) {
        let on = i == sel;
        let bg = if on { t.input } else { t.panel };
        let (verb, c) = match hit.group {
            "ASK" => ("ask", t.brass),
            "FIX" => ("fix", t.good),
            "SEE" => ("see", t.warn),
            "GO" => ("open", t.violet),
            "KNOW" => ("recall", t.dim),
            "KEEP" => ("order", t.dim),
            _ => ("do", t.dim),
        };
        // `F3 findings`: the key as a key.
        let mut place: Vec<Span<'static>> = match hit.place.split_once(' ') {
            Some((k, rest)) if k.starts_with('F') && k[1..].chars().all(|c| c.is_ascii_digit()) => {
                vec![
                    Span::styled(format!("{rest}  "), t.muted().bg(bg)),
                    crate::board::key(k, t),
                ]
            }
            _ => vec![Span::styled(hit.place.clone(), t.muted().bg(bg))],
        };
        if on && hit.group == "ASK" {
            place = vec![
                Span::styled(format!("{}  ", hit.place), t.muted().bg(bg)),
                crate::board::key("tab", t),
            ];
        }
        let place_w: usize = place.iter().map(|s| s.content.width()).sum();
        let title_w = iw.saturating_sub(1 + 9 + place_w + 2);
        let mut title = truncate(&hit.title, title_w);
        let rest = title_w.saturating_sub(title.width());
        let detail = if hit.detail.is_empty() || rest < 6 {
            String::new()
        } else {
            truncate(&format!(" · {}", hit.detail), rest)
        };
        title.push_str(&detail);
        let mut spans = vec![
            Span::styled(" ", Style::default().bg(bg)),
            Span::styled(format!(" {verb} "), t.pill(c)),
            Span::styled(
                " ".repeat(8usize.saturating_sub(verb.width() + 2) + 1),
                Style::default().bg(bg),
            ),
            Span::styled(
                pad(&title, title_w + 1),
                Style::default().fg(t.fg).bg(bg).add_modifier(if on {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
            ),
        ];
        let used: usize = spans.iter().map(|s| s.content.width()).sum();
        spans.push(Span::styled(
            " ".repeat(iw.saturating_sub(used + place_w)),
            Style::default().bg(bg),
        ));
        spans.extend(place);
        lines.push(Line::from(spans));
    }
    if hits.is_empty() {
        lines.push(Line::from(Span::styled(
            " type to search fixes, findings, orders, memory, receipts, and commands",
            Style::default().fg(t.dim),
        )));
    }
    let foot_at = (inner.height as usize).saturating_sub(1);
    while lines.len() < foot_at {
        lines.push(Line::raw(""));
    }
    lines.truncate(foot_at);
    lines.push(Line::from(crate::board::hints(
        &[
            ("↑↓", "move"),
            ("⏎", "go"),
            ("tab", "ask Reeve instead"),
            ("esc", "close"),
        ],
        t,
    )));
    f.render_widget(Paragraph::new(lines), inner);
}

/// The command list, just above the composer, while a `/word` is typed.
pub fn draw_palette(f: &mut Frame, composer: Rect, v: &View, t: &Theme) {
    let hits = palette(&v.input);
    if hits.is_empty() || !v.overlays.is_empty() {
        return;
    }
    let h = hits.len() as u16 + 2;
    let w = 64.min(composer.width);
    let r = Rect {
        x: composer.x + 1,
        y: composer.y.saturating_sub(h),
        width: w,
        height: h.min(composer.y),
    };
    f.render_widget(Clear, r);
    crate::board::shadow(f, r, t);
    let block = panel("commands", t, true);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let sel = v.palette_sel.min(hits.len() - 1);
    let lines: Vec<Line> = hits
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let on = i == sel;
            let bg = if on { t.input } else { t.panel };
            Line::from(vec![
                Span::styled(" ", Style::default().bg(bg)),
                Span::styled(
                    pad(c.name, 12),
                    Style::default()
                        .fg(t.brass)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    pad(c.about, inner.width.saturating_sub(13) as usize),
                    Style::default().fg(if on { t.fg } else { t.dim }).bg(bg),
                ),
            ])
        })
        .collect();
    f.render_widget(Paragraph::new(lines), inner);
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width.saturating_sub(2));
    let h = h.min(area.height.saturating_sub(2));
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

fn hints(pairs: &[(&str, &str)], t: &Theme) -> Line<'static> {
    Line::from(crate::board::hints(pairs, t))
}

fn kind_label(kind: &str) -> &'static str {
    KINDS
        .iter()
        .find(|(k, _)| *k == kind)
        .map_or("custom", |(_, l)| match *l {
            "local server (no key, $0)" => "local",
            other => other,
        })
}

fn providers(f: &mut Frame, r: Rect, p: &Providers, t: &Theme) {
    let w = r.width as usize;
    let mut lines = vec![
        Line::from(vec![
            Span::styled(pad("   connection", 18), t.ghost()),
            Span::styled(pad("kind", 20), t.ghost()),
            Span::styled(pad("key", 22), t.ghost()),
            Span::styled("model", t.ghost()),
        ]),
        Line::raw(""),
    ];
    for (i, row) in p.rows.iter().enumerate() {
        let on = i == p.sel;
        let bg = if on { t.input } else { t.panel };
        let st = |s: Style| s.bg(bg);
        let (key_mark, key_style) = match &row.key {
            Some(src) => (format!("✓ {src}"), Style::default().fg(t.good)),
            None => ("✗ not set".to_string(), Style::default().fg(t.bad)),
        };
        let used = 3 + 15 + 20 + 22;
        lines.push(Line::from(vec![
            Span::styled(
                if on { " ▸ " } else { "   " },
                st(Style::default().fg(t.brass)),
            ),
            Span::styled(
                if row.active { "● " } else { "○ " },
                st(Style::default().fg(if row.active { t.teal } else { t.faint })),
            ),
            Span::styled(
                pad(&truncate(&row.name, 13), 13),
                st(Style::default()
                    .fg(if on { t.amber } else { t.fg })
                    .add_modifier(Modifier::BOLD)),
            ),
            Span::styled(pad(kind_label(&row.kind), 20), st(t.muted())),
            Span::styled(pad(&truncate(&key_mark, 21), 22), st(key_style)),
            Span::styled(
                pad(
                    &truncate(row.model.as_deref().unwrap_or("—"), w.saturating_sub(used)),
                    w.saturating_sub(used),
                ),
                st(t.muted()),
            ),
        ]));
    }
    let add_on = p.sel == p.rows.len();
    let bg = if add_on { t.input } else { t.panel };
    lines.push(Line::from(vec![
        Span::styled(
            if add_on { " ▸ " } else { "   " },
            Style::default().fg(t.brass).bg(bg),
        ),
        Span::styled(
            pad("+ add a connection", w.saturating_sub(3)),
            Style::default()
                .fg(if add_on { t.amber } else { t.teal })
                .bg(bg),
        ),
    ]));
    lines.push(Line::raw(""));
    match p.selected() {
        Some(row) => {
            lines.push(Line::from(vec![
                Span::styled("   ", t.ghost()),
                Span::styled(truncate(&row.base_url, w.saturating_sub(3)), t.ghost()),
            ]));
            let status = match (&row.status, &row.key) {
                (Some(Ok(s)), _) => Span::styled(format!("✓ {s}"), Style::default().fg(t.good)),
                (Some(Err(e)), _) => Span::styled(format!("✗ {e}"), Style::default().fg(t.bad)),
                (None, None) if row.kind == "local" => Span::styled("no key needed", t.ghost()),
                (None, None) => Span::styled(
                    "⏎ to add a key — it's stored in ~/.reeve/keys (0600), never shown",
                    t.muted(),
                ),
                (None, Some(_)) => Span::styled("v to check the key", t.ghost()),
            };
            lines.push(Line::from(vec![Span::raw("   "), status]));
        }
        None => lines.push(Line::from(Span::styled(
            "   An OpenAI-compatible API, OpenRouter, or a model server on this machine.",
            t.ghost(),
        ))),
    }
    lines.push(Line::raw(""));
    lines.push(hints(
        &[
            ("⏎", "use"),
            ("s", "set key"),
            ("m", "model"),
            ("v", "verify"),
            ("x", "forget key"),
            ("a", "add"),
            ("esc", "close"),
        ],
        t,
    ));
    f.render_widget(Paragraph::new(lines), r);
}

fn key_entry(f: &mut Frame, r: Rect, k: &KeyEntry, t: &Theme) {
    let n = k.secret.chars().count();
    let dots: String = "•".repeat(n.min(r.width.saturating_sub(12) as usize));
    let lines = vec![
        Line::from(vec![
            Span::styled("API key for ", t.muted()),
            Span::styled(k.connection.clone(), t.accent()),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(" ❯ ", Style::default().fg(t.brass).bg(t.input)),
            Span::styled(
                pad(&dots, r.width.saturating_sub(3) as usize),
                Style::default().fg(t.amber).bg(t.input),
            ),
        ]),
        Line::raw(""),
        Line::from(Span::styled(
            if n == 0 {
                "Paste or type the key. It is never shown, logged, or sent to a model.".to_string()
            } else {
                format!("{n} characters")
            },
            t.ghost(),
        )),
        Line::from(Span::styled(
            format!("Saved to ~/.reeve/keys/{} with mode 0600.", k.connection),
            t.ghost(),
        )),
        Line::raw(""),
        hints(&[("⏎", "save"), ("^u", "clear"), ("esc", "cancel")], t),
    ];
    f.render_widget(Paragraph::new(lines), r);
    let col = 3 + dots.width() as u16;
    f.set_cursor_position(Position::new(
        r.x + col.min(r.width.saturating_sub(1)),
        r.y + 2,
    ));
}

fn models(f: &mut Frame, r: Rect, m: &ModelPicker, t: &Theme) {
    let w = r.width as usize;
    let hits = m.filtered();
    let count = if m.loading && m.models.is_empty() {
        "loading…".to_string()
    } else {
        format!("{} of {}", hits.len(), m.models.len())
    };
    let mut lines = vec![
        Line::from(vec![
            Span::styled(" ⌕ ", Style::default().fg(t.brass).bg(t.input)),
            Span::styled(
                pad(&m.query, w.saturating_sub(3 + count.width() + 1)),
                Style::default().fg(t.fg).bg(t.input),
            ),
            Span::styled(format!("{count} "), Style::default().fg(t.dim).bg(t.input)),
        ]),
        Line::from(vec![
            Span::styled(
                pad(&format!("   on {}", m.connection), w.saturating_sub(42)),
                t.ghost(),
            ),
            Span::styled(pad("in/out $/M", 14), t.ghost()),
            Span::styled(pad("cache r", 10), t.ghost()),
            Span::styled("ctx", t.ghost()),
        ]),
    ];
    let list_h = r.height.saturating_sub(4) as usize;
    if let Some(e) = &m.error {
        lines.push(Line::from(Span::styled(
            format!("   ✗ {e}"),
            Style::default().fg(t.bad),
        )));
        lines.push(Line::from(Span::styled(
            "   Type a model id and press ⏎ to use it anyway.",
            t.ghost(),
        )));
    } else if hits.is_empty() && !m.loading {
        let msg = if m.query.is_empty() {
            "   This server lists no models. Type an id and press ⏎.".to_string()
        } else if m.query.trim().contains(char::is_whitespace) {
            "   Nothing matches.".to_string()
        } else {
            format!(
                "   Nothing matches. ⏎ uses \"{}\" as the model id.",
                m.query.trim()
            )
        };
        lines.push(Line::from(Span::styled(msg, t.muted())));
    }
    let start = m.sel.saturating_sub(list_h.saturating_sub(1));
    for (i, model) in hits.iter().enumerate().skip(start).take(list_h) {
        let on = i == m.sel;
        let bg = if on { t.input } else { t.panel };
        let current = m.current.as_deref() == Some(model.id.as_str());
        let price = match (model.input_per_million, model.output_per_million) {
            (Some(a), Some(b)) if a == 0.0 && b == 0.0 => "free".to_string(),
            (Some(a), Some(b)) => format!("${}/${}", trim_rate(a), trim_rate(b)),
            _ => "$?.??".to_string(),
        };
        let cache = model
            .cache_read_per_million
            .map_or(String::new(), |c| format!("${}", trim_rate(c)));
        let ctx = model
            .context_length
            .map_or(String::new(), |c| format_tokens(c).replace(".0k", "k"));
        let why_not = model.unusable();
        let no_tools = why_not.is_some();
        let id_w = w.saturating_sub(42 + 5);
        let id_style = if no_tools {
            t.ghost()
        } else if on {
            Style::default().fg(t.amber).add_modifier(Modifier::BOLD)
        } else {
            t.text()
        };
        lines.push(Line::from(vec![
            Span::styled(
                if on { " ▸ " } else { "   " },
                Style::default().fg(t.brass).bg(bg),
            ),
            Span::styled(
                if current { "● " } else { "  " },
                Style::default().fg(t.teal).bg(bg),
            ),
            Span::styled(pad(&truncate(&model.id, id_w), id_w), id_style.bg(bg)),
            Span::styled(pad(&price, 14), Style::default().fg(t.amber).bg(bg)),
            Span::styled(pad(&cache, 10), Style::default().fg(t.teal).bg(bg)),
            Span::styled(
                pad(
                    &why_not
                        .map_or_else(|| ctx.clone(), |w| format!("{ctx} {w}").trim().to_string()),
                    18,
                ),
                Style::default().fg(t.dim).bg(bg),
            ),
        ]));
    }
    while lines.len() < r.height.saturating_sub(1) as usize {
        lines.push(Line::raw(""));
    }
    lines.truncate(r.height.saturating_sub(1) as usize);
    let hidden = m.hidden();
    let toggle = if m.show_all {
        "hide unusable".to_string()
    } else {
        format!("show {hidden} unusable")
    };
    let mut keys = vec![("type", "search"), ("↑↓", "move"), ("⏎", "use")];
    if hidden > 0 || m.show_all {
        keys.push(("tab", toggle.as_str()));
    }
    keys.push(("esc", "close"));
    lines.push(hints(&keys, t));
    f.render_widget(Paragraph::new(lines), r);
    f.set_cursor_position(Position::new(
        r.x + 3 + m.query.width().min(w.saturating_sub(4)) as u16,
        r.y,
    ));
}

fn add_form(f: &mut Frame, r: Rect, a: &AddForm, t: &Theme) {
    let w = r.width as usize;
    let mut lines = Vec::new();
    let mut cursor = None;
    for (i, label) in FIELDS.iter().enumerate() {
        let on = a.focus == i;
        let value = match i {
            0 => a.name.clone(),
            1 => format!("‹ {} ›", KINDS[a.kind].1),
            2 => a.base_url.clone(),
            _ => a.model.clone(),
        };
        let placeholder = match i {
            0 => "e.g. groq, box, work",
            3 => "optional; pick later with m",
            _ => "",
        };
        let bg = if on { t.input } else { t.panel };
        let field_w = w.saturating_sub(12);
        let shown = if value.is_empty() {
            Span::styled(
                pad(placeholder, field_w),
                t.ghost().bg(bg).add_modifier(Modifier::ITALIC),
            )
        } else {
            Span::styled(
                pad(&truncate(&value, field_w), field_w),
                Style::default()
                    .fg(if i == 1 { t.teal } else { t.fg })
                    .bg(bg),
            )
        };
        if on && i != 1 {
            cursor = Some((
                r.x + 12 + value.width().min(field_w) as u16,
                r.y + lines.len() as u16,
            ));
        }
        lines.push(Line::from(vec![
            Span::styled(if on { " ▸ " } else { "   " }, Style::default().fg(t.brass)),
            Span::styled(pad(label, 9), if on { t.accent() } else { t.muted() }),
            shown,
        ]));
        lines.push(Line::raw(""));
    }
    match &a.error {
        Some(e) => lines.push(Line::from(Span::styled(
            format!("   ✗ {e}"),
            Style::default().fg(t.bad),
        ))),
        None => lines.push(Line::from(Span::styled(
            "   Groq, Together, vLLM, LM Studio… anything that speaks /v1/chat/completions.",
            t.ghost(),
        ))),
    }
    lines.push(Line::raw(""));
    lines.push(hints(
        &[
            ("tab", "next"),
            ("←→", "kind"),
            ("⏎", "save"),
            ("esc", "cancel"),
        ],
        t,
    ));
    f.render_widget(Paragraph::new(lines), r);
    if let Some((x, y)) = cursor {
        f.set_cursor_position(Position::new(x, y));
    }
}

fn ago(t: chrono::DateTime<chrono::Utc>) -> String {
    let s = (chrono::Utc::now() - t).num_seconds().max(0);
    match s {
        0..60 => "<1m".into(),
        60..3600 => format!("{}m", s / 60),
        3600..86_400 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn privacy(f: &mut Frame, r: Rect, p: &crate::overlay::PrivacyPanel, t: &Theme) {
    use reeve_core::privacy::Level;
    let w = r.width as usize;
    let mut lines = Vec::new();
    let key = |k: &str| {
        Span::styled(
            format!(" {k} "),
            Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
        )
    };
    let level_line = |k: &str, label: &str, value: &str, about: &str| {
        let lvl = Level::parse(value);
        let c = match lvl {
            Level::Off => t.warn,
            Level::Standard => t.teal,
            Level::Strict => t.good,
        };
        Line::from(vec![
            key(k),
            Span::styled(pad(label, 13), t.muted()),
            Span::styled(
                pad(lvl.as_str(), 10),
                Style::default().fg(c).add_modifier(Modifier::BOLD),
            ),
            Span::styled(truncate(about, w.saturating_sub(27)), t.ghost()),
        ])
    };
    lines.push(Line::from(Span::styled(
        truncate(
            "Before anything leaves this machine, Reeve swaps these for placeholders, and back here.",
            w,
        ),
        t.ghost(),
    )));
    lines.push(Line::raw(""));
    if p.local {
        lines.push(Line::from(Span::styled(
            "  This connection is local: nothing leaves the machine, so nothing is masked.",
            Style::default().fg(t.good),
        )));
    }
    lines.push(level_line(
        "l",
        "chat",
        &p.cfg.level,
        "standard: secrets, emails, public IPs, user and host names",
    ));
    lines.push(level_line(
        "b",
        "background",
        &p.cfg.background,
        "the drafter and standing orders; strict adds private IPs, MACs, UUIDs",
    ));
    lines.push(Line::raw(""));
    let toggle = |k: &str, on: bool, label: &str, about: &str| {
        Line::from(vec![
            key(k),
            Span::styled(
                if on { "● " } else { "○ " },
                Style::default().fg(if on { t.good } else { t.dim }),
            ),
            Span::styled(pad(label, 25), Style::default().fg(t.fg)),
            Span::styled(truncate(about, w.saturating_sub(32)), t.ghost()),
        ])
    };
    lines.push(Line::from(vec![
        Span::styled("OpenRouter routing", t.accent()),
        Span::styled(
            if p.openrouter {
                ""
            } else {
                "   (not the connection in use)"
            },
            t.ghost(),
        ),
    ]));
    lines.push(toggle(
        "t",
        p.cfg.no_training,
        "no training on prompts",
        "only providers that don't store or train on them",
    ));
    lines.push(toggle(
        "z",
        p.cfg.zdr,
        "zero data retention",
        "only ZDR endpoints (fewer models)",
    ));
    if !p.cfg.terms.is_empty() {
        lines.push(Line::from(vec![
            Span::styled("   your terms  ", t.muted()),
            Span::styled(
                truncate(&p.cfg.terms.join(", "), w.saturating_sub(15)),
                Style::default().fg(t.fg),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    let entries = p
        .state
        .as_ref()
        .map(|s| s.entries.as_slice())
        .unwrap_or(&[]);
    lines.push(Line::from(vec![
        Span::styled("Masked this session  ", t.accent()),
        Span::styled(format!("{}", entries.len()), Style::default().fg(t.fg)),
    ]));
    if entries.is_empty() {
        lines.push(Line::from(Span::styled(
            "  nothing yet: the list fills as values turn up in what Reeve sends",
            t.ghost(),
        )));
    }
    let room = (r.height as usize).saturating_sub(lines.len() + 3);
    let start = p.scroll.min(entries.len().saturating_sub(room.max(1)));
    for e in entries.iter().skip(start).take(room) {
        lines.push(Line::from(vec![
            Span::styled(
                pad(&format!("  {}", e.placeholder), 14),
                Style::default().fg(t.code),
            ),
            Span::styled(pad(e.kind.label(), 13), t.ghost()),
            Span::styled(truncate(&e.preview(), w.saturating_sub(28)), t.muted()),
        ]));
    }
    lines.push(Line::raw(""));
    match &p.note {
        Some(Ok(n)) => lines.push(Line::from(Span::styled(
            n.clone(),
            Style::default().fg(t.good),
        ))),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            e.clone(),
            Style::default().fg(t.bad),
        ))),
        None => lines.push(Line::from(Span::styled(
            truncate(
                "Pattern matching, not a guarantee. Add your own terms in config.toml [privacy].",
                w,
            ),
            t.ghost(),
        ))),
    }
    f.render_widget(Paragraph::new(lines), r);
}

fn observer(f: &mut Frame, r: Rect, p: &crate::overlay::ObserverPanel, t: &Theme) {
    let w = r.width as usize;
    let now = chrono::Utc::now();
    let mut lines = Vec::new();
    let alive = p.status.as_ref().is_some_and(|s| s.alive(now));
    lines.push(Line::from(vec![
        Span::styled("reeved  ", t.accent()),
        Span::styled(
            if alive {
                "● running"
            } else {
                "○ not running"
            },
            Style::default().fg(if alive { t.good } else { t.dim }),
        ),
        Span::styled(format!("   service: {}", p.service), t.ghost()),
    ]));
    if let Some(s) = &p.status {
        lines.push(Line::from(Span::styled(
            format!(
                "  journal {} · last heartbeat {}",
                if s.journal {
                    "readable"
                } else {
                    "NOT readable (join the systemd-journal group)"
                },
                s.beat
                    .map_or("never".to_string(), |b| format!("{} ago", ago(b)))
            ),
            t.muted(),
        )));
        if let Some(e) = &s.last_error {
            lines.push(Line::from(Span::styled(
                format!("  last note: {}", truncate(e, w.saturating_sub(14))),
                Style::default().fg(t.warn),
            )));
        }
    }
    lines.push(Line::from(Span::styled(
        "  Watches with rules only: no model, no cost. It never changes the machine.",
        t.ghost(),
    )));
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("drafter  ", t.accent()),
        Span::styled(
            "drafts a proposed fix for each finding while you're away (read-only, own budget)",
            t.ghost(),
        ),
    ]));
    lines.push(Line::raw(""));
    let d = &p.drafter;
    let spent = p.status.as_ref().map_or(0.0, |s| s.drafter_usd_today);
    let drafts = p.status.as_ref().map_or(0, |s| s.drafts_today);
    let values = [
        (
            if d.enabled {
                "on".to_string()
            } else {
                "off".to_string()
            },
            if d.enabled { t.good } else { t.dim },
        ),
        (
            d.connection
                .clone()
                .unwrap_or_else(|| "main connection".into()),
            t.fg,
        ),
        (
            d.model
                .clone()
                .unwrap_or_else(|| format!("main model ({})", p.main_model)),
            t.fg,
        ),
        (
            format!("${:.2}/day   (${spent:.2} spent today)", d.daily_usd),
            t.amber,
        ),
        (format!("${:.2} per draft", d.per_draft_usd), t.amber),
        (
            format!("{} a day   ({drafts} today)", d.max_drafts_per_day),
            t.fg,
        ),
        (format!("{} and above", d.min_severity), t.fg),
    ];
    for (i, (label, (value, color))) in crate::overlay::DRAFTER_FIELDS
        .iter()
        .zip(values)
        .enumerate()
    {
        let on = i == p.field;
        let bg = if on { t.input } else { t.panel };
        let arrows = if on && i != 2 {
            "‹ › "
        } else if on {
            "⏎ pick "
        } else {
            ""
        };
        lines.push(Line::from(vec![
            Span::styled(
                if on { " ▸ " } else { "   " },
                Style::default().fg(t.brass).bg(bg),
            ),
            Span::styled(
                pad(label, 14),
                (if on { t.accent() } else { t.muted() }).bg(bg),
            ),
            Span::styled(
                pad(&format!("{arrows}{value}"), w.saturating_sub(17)),
                Style::default().fg(color).bg(bg),
            ),
        ]));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("  Its spend counts toward your global day and month caps too; whichever is hit first stops it.", t.ghost())));
    lines.push(Line::from(Span::styled(
        "  A model with no known price won't run while any budget is set.",
        t.ghost(),
    )));
    let footer_at = r.height.saturating_sub(2) as usize;
    while lines.len() < footer_at {
        lines.push(Line::raw(""));
    }
    lines.push(match &p.note {
        Some(Ok(m)) => Line::from(Span::styled(format!("✓ {m}"), Style::default().fg(t.good))),
        Some(Err(m)) => Line::from(Span::styled(format!("✗ {m}"), Style::default().fg(t.bad))),
        None => Line::raw(""),
    });
    lines.push(hints(
        &[
            ("↑↓", "field"),
            ("←→", "change"),
            ("space", "drafter on/off"),
            ("i", "install/start"),
            ("u", "remove"),
            ("esc", "close"),
        ],
        t,
    ));
    f.render_widget(Paragraph::new(lines), r);
}

fn password(f: &mut Frame, r: Rect, p: &crate::overlay::PasswordEntry, t: &Theme) {
    let n = p.secret.chars().count();
    let dots: String = "•".repeat(n.min(r.width.saturating_sub(12) as usize));
    let check = if p.remember { "☑" } else { "☐" };
    let lines = vec![
        Line::from(vec![
            Span::styled("◆ ", Style::default().fg(t.bad)),
            Span::styled("Root access for an approved action", t.accent()),
        ]),
        Line::from(Span::styled(
            format!(
                "  {}",
                truncate(&p.action, r.width.saturating_sub(4) as usize)
            ),
            Style::default().fg(t.code),
        )),
        Line::raw(""),
        Line::from(Span::styled(
            truncate(p.prompt.trim(), r.width as usize),
            t.muted(),
        )),
        Line::from(vec![
            Span::styled(" ❯ ", Style::default().fg(t.brass).bg(t.input)),
            Span::styled(
                pad(&dots, r.width.saturating_sub(3) as usize),
                Style::default().fg(t.amber).bg(t.input),
            ),
        ]),
        Line::raw(""),
        Line::from(vec![
            Span::styled(format!("{check} "), Style::default().fg(t.teal)),
            Span::styled(
                "remember for 5 minutes (in memory only, gone when Reeve quits)",
                t.muted(),
            ),
        ]),
        Line::from(Span::styled(
            "It goes to sudo only: never to the model, a log, or a receipt.",
            t.ghost(),
        )),
        Line::raw(""),
        hints(&[("⏎", "send"), ("tab", "remember"), ("esc", "refuse")], t),
    ];
    f.render_widget(Paragraph::new(lines), r);
    let col = 3 + dots.width() as u16;
    f.set_cursor_position(Position::new(
        r.x + col.min(r.width.saturating_sub(1)),
        r.y + 4,
    ));
}

fn help(f: &mut Frame, r: Rect, t: &Theme) {
    let row = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(pad(k, 16), Style::default().fg(t.brass)),
            Span::styled(d.to_string(), t.muted()),
        ])
    };
    let mut lines = vec![Line::from(Span::styled("Commands", t.accent()))];
    for c in crate::overlay::COMMANDS {
        lines.push(row(c.name, c.about));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("Keys", t.accent())));
    for (k, d) in [
        ("⏎ / alt+⏎", "send / newline"),
        (
            "F1…F8",
            "open a tile: needs you, health, findings, activity, spend, changed, orders, memory",
        ),
        (
            "tab / ⇧tab",
            "the board, then each tile in turn (or click one)",
        ),
        ("↑", "the chat, from the board (empty composer)"),
        ("esc", "stop the turn · back to where you were · the board"),
        ("?", "on a tile: ask Reeve about what's selected"),
        (
            "⏎ a s n",
            "an approval: yes · yes to the rest of this request · this kind of change all session · no",
        ),
        (
            "ctrl+k",
            "search everything: fixes, findings, orders, memory, receipts",
        ),
        ("ctrl+l", "redraw the screen (after the terminal clears it)"),
        ("$", "the spend statement (empty composer)"),
        ("^r", "activity: every receipt, undo, verify the chain"),
        ("^y", "YOLO on/off"),
        ("pgup pgdn", "scroll the chat"),
        ("^c", "stop, clear, then quit"),
    ] {
        lines.push(row(k, d));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled("any key closes", t.ghost())));
    f.render_widget(Paragraph::new(lines), r);
}

#[cfg(test)]
mod tests {
    use crate::draw::draw;
    use crate::overlay::{KeyEntry, ModelPicker, Overlay, ProviderRow, Providers};
    use crate::theme::Theme;
    use crate::view::View;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use reeve_core::llm::ModelInfo;
    use reeve_observer::HostInfo;

    fn render(v: &View) -> String {
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| draw(f, v, &Theme::brass())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..40)
            .map(|y| (0..140).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect()
    }

    #[test]
    fn providers_panel_lists_connections_and_key_state() {
        let mut v = View::new(HostInfo::default());
        v.overlays.push(Overlay::Providers(Providers {
            rows: vec![
                ProviderRow {
                    name: "openrouter".into(),
                    kind: "openrouter".into(),
                    base_url: "https://openrouter.ai/api/v1".into(),
                    key: None,
                    active: true,
                    model: Some("anthropic/claude-sonnet-5".into()),
                    status: None,
                },
                ProviderRow {
                    name: "box".into(),
                    kind: "local".into(),
                    base_url: "http://localhost:11434/v1".into(),
                    key: Some("none needed".into()),
                    active: false,
                    model: None,
                    status: Some(Ok("key works · 3 models".into())),
                },
            ],
            sel: 0,
        }));
        let s = render(&v);
        for needle in [
            "providers",
            "openrouter",
            "✗ not set",
            "claude-sonnet-5",
            "+ add a connection",
            "⏎ to add a key",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
    }

    #[test]
    fn the_key_panel_shows_dots_not_the_key() {
        let mut v = View::new(HostInfo::default());
        let mut k = KeyEntry::new("openrouter", true);
        k.secret = "sk-or-v1-supersecret".into();
        v.overlays.push(Overlay::Key(k));
        let s = render(&v);
        assert!(!s.contains("supersecret") && !s.contains("sk-or"));
        assert!(s.contains("••••") && s.contains("20 characters"));
    }

    #[test]
    fn the_model_picker_shows_prices() {
        let mut v = View::new(HostInfo::default());
        let mut p = ModelPicker::loading("openrouter", Some("a/b".into()), None);
        p.set_models(vec![ModelInfo {
            input_per_million: Some(3.0),
            output_per_million: Some(15.0),
            cache_read_per_million: Some(0.3),
            context_length: Some(200_000),
            ..ModelInfo::named("a/b")
        }]);
        v.overlays.push(Overlay::Models(p));
        let s = render(&v);
        assert!(
            s.contains("$3/$15") && s.contains("$0.3") && s.contains("200k"),
            "{s}"
        );
    }

    #[test]
    fn the_palette_appears_for_a_slash() {
        let mut v = View::new(HostInfo::default());
        v.insert("/pro");
        let s = render(&v);
        assert!(s.contains("/providers") && s.contains("connections and API keys"));
    }
}
