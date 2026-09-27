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
    let area = f.area();
    let (w, h) = match top {
        Overlay::Providers(p) => (100, p.rows.len() as u16 + 12),
        Overlay::Key(_) => (84, 11),
        Overlay::Models(_) => (104, area.height.saturating_sub(6).min(32)),
        Overlay::Add(_) => (92, 15),
        Overlay::Help => (72, 24),
    };
    let r = centered(area, w, h);
    f.render_widget(Clear, r);
    let title = match top {
        Overlay::Providers(_) => "providers",
        Overlay::Key(_) => "api key",
        Overlay::Models(_) => "model",
        Overlay::Add(_) => "add a connection",
        Overlay::Help => "help",
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
        Overlay::Help => help(f, inner, t),
    }
}

/// The command list, just above the composer, while a `/word` is typed.
pub fn draw_palette(f: &mut Frame, composer: Rect, v: &View, t: &Theme) {
    let hits = palette(&v.input);
    if hits.is_empty() || !v.overlays.is_empty() {
        return;
    }
    let h = hits.len() as u16 + 2;
    let w = 58.min(composer.width);
    let r = Rect {
        x: composer.x,
        y: composer.y.saturating_sub(h),
        width: w,
        height: h.min(composer.y),
    };
    f.render_widget(Clear, r);
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
                Span::styled(
                    if on { " ▸ " } else { "   " },
                    Style::default().fg(t.brass).bg(bg),
                ),
                Span::styled(
                    pad(c.name, 12),
                    Style::default()
                        .fg(if on { t.amber } else { t.fg })
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    pad(c.about, inner.width.saturating_sub(15) as usize),
                    Style::default().fg(t.dim).bg(bg),
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
    let mut spans = Vec::new();
    for (k, l) in pairs {
        spans.push(Span::styled((*k).to_string(), Style::default().fg(t.brass)));
        spans.push(Span::styled(format!(" {l}   "), t.ghost()));
    }
    Line::from(spans)
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
                pad(&format!("   on {}", m.connection), w.saturating_sub(34)),
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
        } else {
            if m.query.trim().contains(char::is_whitespace) {
                "   Nothing matches.".to_string()
            } else {
                format!(
                    "   Nothing matches. ⏎ uses \"{}\" as the model id.",
                    m.query.trim()
                )
            }
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
        let no_tools = model.tools == Some(false);
        let id_w = w.saturating_sub(34 + 5);
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
                    &format!("{ctx}{}", if no_tools { " no tools" } else { "" }),
                    10,
                ),
                Style::default().fg(t.dim).bg(bg),
            ),
        ]));
    }
    while lines.len() < r.height.saturating_sub(1) as usize {
        lines.push(Line::raw(""));
    }
    lines.truncate(r.height.saturating_sub(1) as usize);
    lines.push(hints(
        &[
            ("type", "search"),
            ("↑↓", "move"),
            ("⏎", "use"),
            ("esc", "close"),
        ],
        t,
    ));
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
        ("esc", "stop the turn, or clear the composer"),
        ("^y", "YOLO on/off"),
        ("^b", "chat ⇄ rail on narrow terminals"),
        ("pgup pgdn", "scroll the conversation"),
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
