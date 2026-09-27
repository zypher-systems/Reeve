//! Tool rows in the chat, the approval card, and the receipts rail.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use unicode_width::UnicodeWidthStr;

use reeve_core::diff::{DiffKind, FileDiff};
use reeve_core::policy::Tier;
use reeve_core::receipts::Status;

use crate::draw::{pad, truncate};
use crate::theme::Theme;
use crate::view::{Pending, ToolView, View};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
/// Diff rows shown under a finished tool call.
const CHAT_DIFF_ROWS: usize = 12;
/// Diff rows on an approval card.
const CARD_DIFF_ROWS: usize = 10;

fn badge(tier: Tier, t: &Theme) -> Span<'static> {
    Span::styled(
        format!(" {} ", tier.label()),
        Style::default()
            .fg(t.bg)
            .bg(t.tier(tier))
            .add_modifier(Modifier::BOLD),
    )
}

fn status_icon(status: Option<Status>, frame: u64, t: &Theme) -> Span<'static> {
    match status {
        None => Span::styled(
            SPINNER[(frame / 2) as usize % SPINNER.len()],
            Style::default().fg(t.amber),
        ),
        Some(Status::Ok) => Span::styled("✓", Style::default().fg(t.good)),
        Some(Status::Error) => Span::styled("✗", Style::default().fg(t.bad)),
        Some(Status::Denied) => Span::styled("⊘", Style::default().fg(t.warn)),
        Some(Status::Refused) => Span::styled("⊗", Style::default().fg(t.bad)),
    }
}

/// A tool call in the conversation: icon, tier, what, receipt; then what
/// happened, and the diff for file changes.
pub fn tool_lines(tv: &ToolView, width: usize, frame: u64, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let receipt = match (tv.seq, tv.undoable) {
        (Some(n), true) => format!(" #{n} ↶"),
        (Some(n), false) => format!(" #{n}"),
        _ => String::new(),
    };
    let head_w = width.saturating_sub(7 + receipt.width());
    let summary_style = match tv.status {
        Some(Status::Denied | Status::Refused) => t.muted().add_modifier(Modifier::CROSSED_OUT),
        _ => Style::default().fg(t.fg),
    };
    out.push(Line::from(vec![
        status_icon(tv.status, frame, t),
        Span::raw(" "),
        badge(tv.tier, t),
        Span::raw(" "),
        Span::styled(pad(&truncate(&tv.summary, head_w), head_w), summary_style),
        Span::styled(receipt, t.ghost()),
    ]));
    let note = match tv.status {
        None => Some(if tv.tier == Tier::T0 {
            "running…".to_string()
        } else {
            "waiting for you…".to_string()
        }),
        Some(Status::Denied) => Some("you declined".to_string()),
        Some(_) if !tv.result.is_empty() => Some(tv.result.clone()),
        _ => None,
    };
    if let Some(note) = note {
        let by = match tv.approved_by.as_deref() {
            Some("yolo") => "  · yolo",
            Some("session-rule") => "  · allowed for session",
            _ => "",
        };
        let color = match tv.status {
            Some(Status::Error | Status::Refused) => t.bad,
            _ => t.dim,
        };
        out.push(Line::from(vec![
            Span::styled("  └ ", t.ghost()),
            Span::styled(
                truncate(&note, width.saturating_sub(6 + by.width())),
                Style::default().fg(color),
            ),
            Span::styled(
                by.to_string(),
                Style::default().fg(if by.contains("yolo") { t.bad } else { t.teal }),
            ),
        ]));
    }
    if let Some(d) = &tv.diff {
        out.extend(diff_lines(d, width, CHAT_DIFF_ROWS, t));
    }
    out
}

/// Numbered, tinted diff rows.
pub fn diff_lines(d: &FileDiff, width: usize, max: usize, t: &Theme) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    let text_w = width.saturating_sub(10);
    for l in d.lines.iter().take(max) {
        let (mark, fg, bg) = match l.kind {
            DiffKind::Add => ("+", t.good, t.add_bg),
            DiffKind::Remove => ("−", t.bad, t.del_bg),
            DiffKind::Context => (" ", t.dim, t.panel),
            DiffKind::Gap => {
                out.push(Line::from(Span::styled("    ┊", t.ghost())));
                continue;
            }
        };
        let num = l.line.map_or(String::new(), |n| n.to_string());
        let body = truncate(&l.text.replace('\t', "    "), text_w);
        out.push(Line::from(vec![
            Span::styled(format!("  {num:>4} "), t.ghost().bg(bg)),
            Span::styled(
                format!("{mark} "),
                Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                pad(&body, text_w),
                Style::default()
                    .fg(if l.kind == DiffKind::Context {
                        t.dim
                    } else {
                        t.fg
                    })
                    .bg(bg),
            ),
        ]));
    }
    let hidden = d.lines.len().saturating_sub(max) + usize::from(d.truncated);
    if hidden > 0 {
        out.push(Line::from(Span::styled(
            format!("       … more changes (+{} −{} in all)", d.added, d.removed),
            t.ghost(),
        )));
    }
    out
}

/// The card's rows (so its height is known before drawing).
fn card_lines(p: &Pending, width: usize, t: &Theme) -> Vec<Line<'static>> {
    let r = &p.req;
    let mut out = Vec::new();
    let code = Style::default().fg(t.code).bg(t.code_bg);
    out.push(Line::from(vec![
        Span::styled(r.tool.clone(), t.accent()),
        Span::styled(
            if r.sudo { "  as root" } else { "" },
            Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
        ),
    ]));
    for chunk in wrap_hard(&r.summary, width.saturating_sub(4)) {
        out.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(format!(" {chunk} "), code),
        ]));
    }
    if let Some(why) = &r.why {
        out.push(Line::from(vec![
            Span::styled("why  ", t.ghost()),
            Span::styled(
                truncate(why, width.saturating_sub(5)),
                t.muted().add_modifier(Modifier::ITALIC),
            ),
        ]));
    }
    if !r.reasons.is_empty() {
        let mut spans = vec![Span::styled("risk ", t.ghost())];
        for (i, reason) in r.reasons.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" · ", t.ghost()));
            }
            spans.push(Span::styled(
                reason.clone(),
                Style::default().fg(t.tier(r.tier)),
            ));
        }
        out.push(Line::from(spans));
    }
    if let Some(d) = &r.preview {
        out.push(Line::from(Span::styled(
            format!("change  +{} −{}", d.added, d.removed),
            t.ghost(),
        )));
        out.extend(diff_lines(d, width, CARD_DIFF_ROWS, t));
    }
    out.push(Line::from(if r.undoable {
        vec![
            Span::styled("↶ ", Style::default().fg(t.teal)),
            Span::styled(
                "a copy is kept: you can undo this from its receipt",
                t.ghost(),
            ),
        ]
    } else {
        vec![
            Span::styled("! ", Style::default().fg(t.warn)),
            Span::styled(
                "Reeve can't undo this; the receipt records what ran",
                t.ghost(),
            ),
        ]
    }));
    out.push(Line::raw(""));
    let key = |k: &str| {
        Span::styled(
            k.to_string(),
            Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
        )
    };
    let label = |l: &str| Span::styled(format!(" {l}    "), t.muted());
    if r.tier == Tier::T3 {
        out.push(Line::from(vec![
            Span::styled("type ", t.muted()),
            Span::styled(
                "yes",
                Style::default().fg(t.bad).add_modifier(Modifier::BOLD),
            ),
            Span::styled(" and press ⏎ to allow it   ", t.muted()),
            Span::styled(
                format!(" {:<4}", p.typed),
                Style::default().fg(t.fg).bg(t.input),
            ),
            Span::styled("   esc deny", t.ghost()),
        ]));
    } else {
        let mut spans = vec![key("⏎"), label("approve")];
        if r.can_allow_session {
            spans.push(key("a"));
            spans.push(label("allow for this session"));
        }
        spans.push(key("n"));
        spans.push(label("deny"));
        out.push(Line::from(spans));
    }
    out
}

/// Rows the card needs at `width`.
pub fn approval_height(p: &Pending, width: u16) -> u16 {
    card_lines(p, width.saturating_sub(4) as usize, &Theme::brass()).len() as u16 + 2
}

/// The approval card, in place of the composer.
pub fn draw_approval(f: &mut Frame, area: Rect, v: &View, p: &Pending, t: &Theme) {
    let tier_c = t.tier(p.req.tier);
    let pulse = if v.animate {
        ((v.frame as f32 / 7.0).sin() + 1.0) / 2.0
    } else {
        1.0
    };
    let edge: Color = t.mix(t.border, tier_c, 0.45 + 0.55 * pulse);
    let title = Line::from(vec![
        Span::styled(
            " ◆ approve? ",
            Style::default().fg(tier_c).add_modifier(Modifier::BOLD),
        ),
        badge(p.req.tier, t),
        Span::styled(
            format!(" {} ", p.req.tier.name()),
            Style::default().fg(tier_c),
        ),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(if p.req.tier == Tier::T3 {
            BorderType::Double
        } else {
            BorderType::Rounded
        })
        .border_style(Style::default().fg(edge))
        .title(title)
        .style(Style::default().bg(t.panel));
    let inner = block.inner(area);
    f.render_widget(Clear, area);
    f.render_widget(block, area);
    let body = Rect {
        x: inner.x + 1,
        width: inner.width.saturating_sub(2),
        ..inner
    };
    let lines = card_lines(p, body.width as usize, t);
    // Keep the keys visible if the card is taller than the room.
    let skip = lines.len().saturating_sub(body.height as usize);
    let lines: Vec<Line> = if skip > 0 {
        let keys = lines.len() - 1;
        lines[..keys.saturating_sub(skip)]
            .iter()
            .cloned()
            .chain(std::iter::once(lines[keys].clone()))
            .collect()
    } else {
        lines
    };
    f.render_widget(Paragraph::new(lines), body);
}

/// The rail's receipt feed.
pub fn receipt_lines(v: &View, width: usize, t: &Theme) -> Vec<Line<'static>> {
    if v.receipts.is_empty() {
        return vec![
            Line::from(vec![
                Span::styled("◌ ", Style::default().fg(t.faint)),
                Span::styled("No actions yet.", t.muted()),
            ]),
            Line::from(Span::styled(
                "  Every change Reeve makes lands here",
                t.ghost(),
            )),
            Line::from(Span::styled(
                "  with its tier, approval, and undo.",
                t.ghost(),
            )),
            Line::raw(""),
            Line::from(vec![
                Span::styled("  T0 ", Style::default().fg(t.dim)),
                Span::styled("observe  ", t.ghost()),
                Span::styled("T1 ", Style::default().fg(t.teal)),
                Span::styled("user  ", t.ghost()),
                Span::styled("T2 ", Style::default().fg(t.amber)),
                Span::styled("system  ", t.ghost()),
                Span::styled("T3 ", Style::default().fg(t.bad)),
                Span::styled("floor", t.ghost()),
            ]),
        ];
    }
    let mut out = Vec::new();
    for r in &v.receipts {
        let icon = status_icon(Some(r.outcome.status), 0, t);
        let undone = v.is_undone(r.seq);
        let mark = if r.undoes.is_some() {
            Span::styled(" ↺", Style::default().fg(t.teal))
        } else if r.undo.is_some() && !undone && r.outcome.status == Status::Ok {
            Span::styled(" ↶", Style::default().fg(t.teal))
        } else {
            Span::raw("")
        };
        let target = if r.tool == "undo" {
            format!("undo {}", r.target())
        } else {
            r.target()
        };
        let name_w = width.saturating_sub(7 + 4 + 8 + 3);
        let target_style = if undone {
            t.ghost().add_modifier(Modifier::CROSSED_OUT)
        } else {
            t.muted()
        };
        out.push(Line::from(vec![
            Span::styled(format!("{:>4} ", format!("#{}", r.seq)), t.ghost()),
            icon,
            Span::raw(" "),
            Span::styled(
                r.tier.label().to_string(),
                Style::default().fg(t.tier(r.tier)),
            ),
            Span::raw(" "),
            Span::styled(
                pad(&truncate(&short_tool(&r.tool), 8), 8),
                Style::default().fg(t.fg),
            ),
            Span::styled(truncate(&target, name_w), target_style),
            mark,
        ]));
    }
    out
}

fn short_tool(tool: &str) -> String {
    tool.strip_prefix("fs_").unwrap_or(tool).to_string()
}

fn wrap_hard(s: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut out = Vec::new();
    for line in s.lines() {
        let chars: Vec<char> = line.chars().collect();
        if chars.is_empty() {
            continue;
        }
        for chunk in chars.chunks(width) {
            out.push(chunk.iter().collect());
        }
        if out.len() >= 6 {
            out.truncate(6);
            out.push("…".into());
            break;
        }
    }
    if out.is_empty() {
        out.push(String::new());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draw::draw;
    use crate::view::{Speaker, View};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use reeve_core::agent::{AgentEvent, ApprovalRequest};
    use reeve_core::diff::diff;
    use reeve_observer::HostInfo;

    fn render(v: &View) -> String {
        let mut term = Terminal::new(TestBackend::new(150, 44)).unwrap();
        term.draw(|f| draw(f, v, &Theme::brass())).unwrap();
        let buf = term.backend().buffer().clone();
        (0..44)
            .map(|y| (0..150).map(|x| buf[(x, y)].symbol()).collect::<String>() + "\n")
            .collect()
    }

    fn req(tier: Tier) -> ApprovalRequest {
        ApprovalRequest {
            tool: "fs_edit".into(),
            summary: "edit ~/.bashrc".into(),
            tier,
            reasons: vec!["changes your files".into()],
            sudo: false,
            why: Some("add an alias".into()),
            preview: Some(diff("a\nb\n", "a\nc\n", 20)),
            undoable: true,
            can_allow_session: tier == Tier::T1,
        }
    }

    #[test]
    fn the_card_replaces_the_composer_and_shows_the_change() {
        let mut v = View::new(HostInfo::default());
        v.push(Speaker::User, "add an alias");
        v.approval = Some(Pending {
            req: req(Tier::T1),
            typed: String::new(),
        });
        let s = render(&v);
        for needle in [
            "approve?",
            "T1",
            "edit ~/.bashrc",
            "add an alias",
            "+ c",
            "− b",
            "allow for this session",
            "a copy is kept",
        ] {
            assert!(s.contains(needle), "missing {needle:?}\n{s}");
        }
        assert!(!s.contains("Ask Reeve"), "the composer should be hidden");
    }

    #[test]
    fn the_floor_asks_for_a_typed_yes() {
        let mut v = View::new(HostInfo::default());
        v.push(Speaker::User, "x");
        let mut r = req(Tier::T3);
        r.summary = "rm -rf ~".into();
        v.approval = Some(Pending {
            req: r,
            typed: "ye".into(),
        });
        let s = render(&v);
        assert!(
            s.contains("type yes") && s.contains("ye") && !s.contains("allow for this session"),
            "{s}"
        );
    }

    #[test]
    fn tool_rows_show_status_tier_and_receipt() {
        let mut v = View::new(HostInfo::default());
        v.push(Speaker::User, "check");
        v.apply(AgentEvent::TurnStarted);
        v.apply(AgentEvent::ToolStarted {
            id: "1".into(),
            tool: "shell".into(),
            tier: Tier::T0,
            summary: "systemctl --failed".into(),
        });
        let args =
            reeve_core::receipts::Receipt::draft("s", "x", Default::default(), Tier::T0).args;
        let mut receipt = reeve_core::receipts::Receipt::draft("s", "shell", args, Tier::T0);
        receipt.seq = 7;
        receipt.approved_by = "policy".into();
        v.apply(AgentEvent::ToolFinished {
            id: "1".into(),
            status: Status::Ok,
            summary: "exit 0 · 5 units failed".into(),
            diff: None,
            receipt: Some(Box::new(receipt)),
        });
        let s = render(&v);
        assert!(
            s.contains("✓")
                && s.contains("systemctl --failed")
                && s.contains("#7")
                && s.contains("5 units failed"),
            "{s}"
        );
        assert_eq!(v.receipts.len(), 1);
    }
}
