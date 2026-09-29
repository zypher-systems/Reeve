//! The ledger: the conversation and everything Reeve did, as one timeline.
//!
//! Each row is a time, a node on the spine, what happened, and (on a wide
//! screen) what it cost and the session's running total. Tool calls
//! branch off Reeve's rounds; a verified change is a bracket around its
//! steps; what reeved and the drafter did while you were here sits on the
//! same spine. Whatever needs you is at the bottom, where you type.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use unicode_width::UnicodeWidthStr;

use reeve_core::policy::Tier;
use reeve_core::receipts::Status;
use reeve_core::spend::{format_tokens, format_usd};

use crate::draw::{SPINNER, markdown, pad, plain_wrap, truncate};
use crate::theme::Theme;
use crate::view::{Entry, Speaker, ToolView, View};

/// Time column: `10:39 `.
const TIME_W: usize = 6;
/// Spine column: ` ◆ `.
const SPINE_W: usize = 3;
/// Money columns, when they fit.
const COST_W: usize = 10;
const BAL_W: usize = 11;
/// Below this width the money columns fold into the rows.
const MONEY_MIN: u16 = 96;

/// Draw the conversation (the composer and any approval are the frame's).
pub fn draw(f: &mut Frame, area: Rect, v: &View, t: &Theme) {
    let log = area;
    let money = area.width >= MONEY_MIN;
    let talking = v
        .entries
        .iter()
        .any(|e| matches!(e.who, Speaker::User | Speaker::Reeve));
    let log = if talking && money && v.entries.iter().any(|e| e.cost.is_some()) {
        // Heads over the money columns.
        let head = format!("{:>c$}{:>b$}", "cost", "session", c = COST_W, b = BAL_W);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::raw(pad("", (log.width as usize).saturating_sub(head.width()))),
                Span::styled(head, Style::default().fg(t.faint)),
            ])),
            Rect { height: 1, ..log },
        );
        Rect {
            y: log.y + 1,
            height: log.height.saturating_sub(1),
            ..log
        }
    } else {
        log
    };
    let lines = if talking {
        lines(v, t, log.width as usize, money)
    } else {
        let mut l = welcome(log, t);
        if !v.entries.is_empty() {
            l.push(Line::raw(""));
            l.extend(lines(v, t, log.width as usize, false));
        }
        l
    };
    let h = log.height as usize;
    let max_scroll = lines.len().saturating_sub(h);
    let scroll = v.scroll.min(max_scroll);
    let start = lines.len().saturating_sub(h + scroll);
    let shown: Vec<Line> = lines.into_iter().skip(start).take(h).collect();
    f.render_widget(Paragraph::new(shown), log);
    if scroll > 0 {
        let tag = format!(" ↓ {scroll} more below ");
        let w = tag.width() as u16;
        f.render_widget(
            Paragraph::new(Span::styled(tag, Style::default().fg(t.bg).bg(t.brass))),
            Rect::new(
                log.right().saturating_sub(w),
                log.bottom().saturating_sub(1),
                w,
                1,
            ),
        );
    }
}

fn welcome(area: Rect, t: &Theme) -> Vec<Line<'static>> {
    let w = area.width as usize;
    let center = |s: &str| " ".repeat(w.saturating_sub(s.width()) / 2);
    let mut out: Vec<Line<'static>> = (0..(area.height as usize).saturating_sub(10) / 2)
        .map(|_| Line::raw(""))
        .collect();
    let mark = "r e e v e";
    out.push(Line::from(vec![
        Span::raw(center(mark)),
        Span::styled(mark, Style::default().fg(t.fg).add_modifier(Modifier::BOLD)),
    ]));
    let tag = "the steward of this machine";
    out.push(Line::from(vec![
        Span::raw(center(tag)),
        Span::styled(
            tag,
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ),
    ]));
    out.push(Line::raw(""));
    for s in [
        "why is my disk filling up?",
        "what changed on this machine this week?",
        "is anything crashing?",
    ] {
        let line = format!("›  {s}");
        out.push(Line::from(vec![
            Span::raw(center(&line)),
            Span::styled("›  ", Style::default().fg(t.brass)),
            Span::styled(s.to_string(), Style::default().fg(t.dim)),
        ]));
    }
    out
}

/// One row before it's laid out.
struct Row {
    time: String,
    node: Span<'static>,
    content: Vec<Span<'static>>,
    cost: Option<Span<'static>>,
    bal: Option<Span<'static>>,
}

impl Row {
    fn filler(spine: Span<'static>, content: Vec<Span<'static>>) -> Self {
        Self {
            time: String::new(),
            node: spine,
            content,
            cost: None,
            bal: None,
        }
    }
}

/// The ledger's rows, laid out for `width`.
pub fn lines(v: &View, t: &Theme, width: usize, money: bool) -> Vec<Line<'static>> {
    let content_w = width
        .saturating_sub(TIME_W + SPINE_W + if money { COST_W + BAL_W } else { 0 })
        .max(8);
    let mut rows: Vec<Row> = Vec::new();
    let mut bracket = false;
    let entries = &v.entries;
    for (i, e) in entries.iter().enumerate() {
        let spine = |bracket: bool| {
            if bracket {
                Span::styled("┃", Style::default().fg(t.good))
            } else {
                Span::styled("│", Style::default().fg(t.border))
            }
        };
        let time = e.at.format("%H:%M").to_string();
        match e.who {
            Speaker::User => {
                let text = plain_wrap(&e.text, content_w.saturating_sub(5));
                let mut first = true;
                for l in text {
                    let content = if first {
                        vec![
                            Span::styled(
                                "you  ",
                                Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                            ),
                            Span::styled(l, t.text()),
                        ]
                    } else {
                        vec![Span::raw("     "), Span::styled(l, t.text())]
                    };
                    rows.push(if first {
                        Row {
                            time: time.clone(),
                            node: Span::styled(
                                "●",
                                Style::default().fg(t.user).add_modifier(Modifier::BOLD),
                            ),
                            content,
                            cost: None,
                            bal: None,
                        }
                    } else {
                        Row::filler(spine(bracket), content)
                    });
                    first = false;
                }
            }
            Speaker::Reeve => {
                let mut head = vec![Span::styled(
                    "reeve",
                    Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
                )];
                let (cost, bal) = money_cols(e, t, money, &mut head);
                rows.push(Row {
                    time: time.clone(),
                    node: Span::styled(
                        "◆",
                        Style::default().fg(t.brass).add_modifier(Modifier::BOLD),
                    ),
                    content: head,
                    cost,
                    bal,
                });
                if !e.text.trim().is_empty() {
                    for l in markdown(&e.text, content_w, t) {
                        rows.push(Row::filler(spine(bracket), l.spans));
                    }
                }
            }
            Speaker::Tool => {
                let Some(tv) = &e.tool else { continue };
                let last_of_group = entries.get(i + 1).is_none_or(|n| n.who != Speaker::Tool);
                match tv.tool.as_str() {
                    "change_begin" if tv.status != Some(Status::Error) => {
                        bracket = true;
                        let goal = tv
                            .summary
                            .trim_start_matches("verified change: ")
                            .to_string();
                        let mut content = vec![
                            Span::styled(
                                "verified change",
                                Style::default().fg(t.good).add_modifier(Modifier::BOLD),
                            ),
                            Span::raw("  "),
                            Span::styled(goal, t.text()),
                        ];
                        let (cost, bal) = money_cols(e, t, money, &mut content);
                        rows.push(Row {
                            time: time.clone(),
                            node: Span::styled("┏", Style::default().fg(t.good)),
                            content,
                            cost,
                            bal,
                        });
                        let checks = tv
                            .result
                            .trim_start_matches("will check: ")
                            .replace("; ", " · ");
                        if !checks.is_empty() {
                            rows.push(Row::filler(
                                spine(true),
                                vec![Span::styled(
                                    truncate(&format!("checks  {checks}"), content_w),
                                    Style::default().fg(t.dim),
                                )],
                            ));
                        }
                    }
                    "change_commit" if tv.status.is_some() => {
                        let ok = tv.status == Some(Status::Ok);
                        let c = if ok { t.good } else { t.bad };
                        let (word, rest) = if ok {
                            (
                                "verified · kept",
                                tv.result.trim_start_matches("verified: ").to_string(),
                            )
                        } else {
                            ("failed · rolled back", tv.result.clone())
                        };
                        let mut content = vec![
                            Span::styled(word, Style::default().fg(c).add_modifier(Modifier::BOLD)),
                            Span::raw("  "),
                            Span::styled(rest, Style::default().fg(t.dim)),
                        ];
                        if let Some(n) = tv.seq {
                            content.push(Span::styled(
                                format!("  #{n}"),
                                Style::default().fg(t.faint),
                            ));
                        }
                        let (cost, bal) = money_cols(e, t, money, &mut content);
                        rows.push(Row {
                            time: String::new(),
                            node: Span::styled("┗", Style::default().fg(c)),
                            content,
                            cost,
                            bal,
                        });
                        bracket = false;
                    }
                    _ => {
                        let branch = if bracket {
                            Span::styled("┃", Style::default().fg(t.good))
                        } else if last_of_group {
                            Span::styled("└", Style::default().fg(t.faint))
                        } else {
                            Span::styled("├", Style::default().fg(t.faint))
                        };
                        let mut tr =
                            tool_rows(tv, branch, spine(bracket), content_w, v.frame, t, money);
                        // The round that asked for this call, when it said nothing.
                        if let (Some(c), Some(first)) = (e.cost, tr.first_mut()) {
                            if money {
                                first.cost = Some(Span::styled(format_usd(c.usd), t.text()));
                                first.bal = Some(Span::styled(
                                    format_usd(Some(c.session)),
                                    Style::default().fg(t.dim),
                                ));
                            } else {
                                first.content.push(Span::styled(
                                    format!("  {}", format_usd(c.usd)),
                                    Style::default().fg(t.dim),
                                ));
                            }
                        }
                        rows.extend(tr);
                    }
                }
                if !last_of_group {
                    continue;
                }
            }
            Speaker::System | Speaker::Error => {
                let (node, c) = if e.who == Speaker::Error {
                    (Span::styled("✗", Style::default().fg(t.bad)), t.bad)
                } else {
                    (Span::styled("·", Style::default().fg(t.faint)), t.dim)
                };
                for (k, l) in plain_wrap(&e.text, content_w).into_iter().enumerate() {
                    let content = vec![Span::styled(l, Style::default().fg(c))];
                    rows.push(if k == 0 {
                        Row {
                            time: time.clone(),
                            node: node.clone(),
                            content,
                            cost: None,
                            bal: None,
                        }
                    } else {
                        Row::filler(spine(bracket), content)
                    });
                }
            }
            Speaker::Reeved | Speaker::Drafter => {
                let (name, c) = if e.who == Speaker::Reeved {
                    ("reeved", t.warn)
                } else {
                    ("drafter", t.violet)
                };
                let mut content = vec![
                    Span::styled(name, Style::default().fg(c)),
                    Span::raw("  "),
                    Span::styled(
                        truncate(&e.text, content_w.saturating_sub(10)),
                        Style::default().fg(t.dim),
                    ),
                ];
                let (cost, bal) = match e.cost.and_then(|c| c.usd) {
                    Some(usd) if money => (
                        Some(Span::styled(
                            format!("({})", format_usd(Some(usd))),
                            Style::default().fg(t.violet),
                        )),
                        Some(Span::styled("own budget", Style::default().fg(t.faint))),
                    ),
                    Some(usd) => {
                        content.push(Span::styled(
                            format!("  ({} own budget)", format_usd(Some(usd))),
                            Style::default().fg(t.faint),
                        ));
                        (None, None)
                    }
                    None => (None, None),
                };
                rows.push(Row {
                    time: time.clone(),
                    node: Span::styled("⚑", Style::default().fg(c)),
                    content,
                    cost,
                    bal,
                });
            }
        }
        rows.push(Row::filler(spine(bracket), Vec::new()));
    }
    let running = entries
        .last()
        .and_then(|e| e.tool.as_ref())
        .is_some_and(|t| t.status.is_none());
    if v.busy && !running {
        let spin = SPINNER[(v.frame / 2) as usize % SPINNER.len()];
        let mut content = vec![Span::styled(
            if v.thinking.is_empty() {
                "working"
            } else {
                "thinking"
            },
            Style::default().fg(t.amber),
        )];
        if !v.thinking.is_empty() {
            let words = v.thinking.split_whitespace().count();
            content.push(Span::styled(
                format!("  {words} words"),
                Style::default().fg(t.faint),
            ));
        }
        rows.push(Row {
            time: String::new(),
            node: Span::styled(spin, Style::default().fg(t.amber)),
            content,
            cost: None,
            bal: None,
        });
        if let Some(tail) = v.thinking.lines().rev().find(|l| !l.trim().is_empty()) {
            rows.push(Row::filler(
                Span::styled("┊", Style::default().fg(t.faint)),
                vec![Span::styled(
                    truncate(tail.trim(), content_w),
                    Style::default().fg(t.faint).add_modifier(Modifier::ITALIC),
                )],
            ));
        }
    }
    // Trailing empty spine rows add nothing.
    while rows
        .last()
        .is_some_and(|r| r.content.is_empty() && r.time.is_empty())
    {
        rows.pop();
    }
    rows.into_iter()
        .map(|r| lay_out(r, content_w, money, t))
        .collect()
}

/// The cost and running columns for a round; folded into `head` when narrow.
fn money_cols(
    e: &Entry,
    t: &Theme,
    money: bool,
    head: &mut Vec<Span<'static>>,
) -> (Option<Span<'static>>, Option<Span<'static>>) {
    let Some(c) = e.cost else {
        return (None, None);
    };
    let tokens = format!(
        "  {} in · {} out",
        format_tokens(c.usage.input_tokens),
        format_tokens(c.usage.output_tokens)
    );
    head.push(Span::styled(tokens, Style::default().fg(t.faint)));
    let usd = format_usd(c.usd);
    if money {
        (
            Some(Span::styled(usd, t.text())),
            Some(Span::styled(
                format_usd(Some(c.session)),
                Style::default().fg(t.dim),
            )),
        )
    } else {
        head.push(Span::styled(format!("  {usd}"), Style::default().fg(t.dim)));
        (None, None)
    }
}

fn tier_pill(tier: Tier, t: &Theme) -> Span<'static> {
    crate::board::tier_pill(tier, t)
}

/// A tool call: status, tier, what, what came of it, its receipt; then
/// the diff for a file change.
fn tool_rows(
    tv: &ToolView,
    branch: Span<'static>,
    spine: Span<'static>,
    width: usize,
    frame: u64,
    t: &Theme,
    money: bool,
) -> Vec<Row> {
    let icon = match tv.status {
        None => Span::styled(
            SPINNER[(frame / 2) as usize % SPINNER.len()],
            Style::default().fg(t.amber),
        ),
        Some(Status::Ok) => Span::styled("✓", Style::default().fg(t.good)),
        Some(Status::Error) => Span::styled("✗", Style::default().fg(t.bad)),
        Some(Status::Denied) => Span::styled("⊘", Style::default().fg(t.warn)),
        Some(Status::Refused) => Span::styled("⊗", Style::default().fg(t.bad)),
    };
    let mut tag = String::new();
    if let Some(n) = tv.seq {
        tag = format!("  #{n}");
        if tv.undoable {
            tag.push_str(" ↶");
        }
    }
    let by = match tv.approved_by.as_deref() {
        Some("yolo") => "  yolo",
        Some("session-rule") => "  allowed for session",
        Some("change-rule") => "  rest of the change",
        Some(b) if b.starts_with("order:") => "  by order",
        Some(b) if b.starts_with("txn:") => "  rollback",
        _ => "",
    };
    let note = match tv.status {
        None if tv.tier == Tier::T0 => "running…".to_string(),
        None => "waiting for you…".to_string(),
        Some(Status::Denied) => "you declined".to_string(),
        _ => tv.result.lines().next().unwrap_or("").to_string(),
    };
    let summary_style = match tv.status {
        Some(Status::Denied | Status::Refused) => Style::default()
            .fg(t.faint)
            .add_modifier(Modifier::CROSSED_OUT),
        _ => t.text(),
    };
    let fixed = 2 + 5 + tag.width() + by.width();
    let room = width.saturating_sub(fixed);
    let note_c = match tv.status {
        Some(Status::Error | Status::Refused) => t.bad,
        None => t.amber,
        _ => t.dim,
    };
    let mut content = vec![icon, Span::raw(" "), tier_pill(tv.tier, t), Span::raw(" ")];
    let mut second = None;
    let sw = tv.summary.width();
    if !note.is_empty() && sw + 4 + note.width() <= room {
        // Summary on the left, the result flush right, on one row.
        let gap = room.saturating_sub(sw + 2 + note.width());
        content.push(Span::styled(tv.summary.clone(), summary_style));
        content.push(Span::raw(format!(" {} ", " ".repeat(gap))));
        content.push(Span::styled(note, Style::default().fg(note_c)));
    } else {
        content.push(Span::styled(truncate(&tv.summary, room), summary_style));
        if !note.is_empty() {
            second = Some(Span::styled(
                truncate(&note, width.saturating_sub(4)),
                Style::default().fg(note_c),
            ));
        }
    }
    content.push(Span::styled(tag, Style::default().fg(t.faint)));
    if !by.is_empty() {
        content.push(Span::styled(
            by,
            Style::default().fg(if by.contains("yolo") { t.bad } else { t.teal }),
        ));
    }
    let mut rows = vec![Row {
        time: String::new(),
        node: branch,
        content,
        cost: money
            .then(|| Span::styled("—", Style::default().fg(t.faint)))
            .filter(|_| tv.status.is_some()),
        bal: None,
    }];
    if let Some(s) = second {
        rows.push(Row::filler(spine.clone(), vec![Span::raw("    "), s]));
    }
    if let Some(d) = &tv.diff {
        for l in crate::cards::diff_lines(d, width.saturating_sub(2), 8, t) {
            let mut spans = vec![Span::raw("  ")];
            spans.extend(l.spans);
            rows.push(Row::filler(spine.clone(), spans));
        }
    }
    rows
}

fn lay_out(r: Row, content_w: usize, money: bool, t: &Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled(format!("{:>5} ", r.time), Style::default().fg(t.faint)),
        Span::raw(" "),
        r.node,
        Span::raw(" "),
    ];
    let used: usize = r.content.iter().map(|s| s.content.width()).sum();
    if used > content_w {
        // Cut the row to fit, span by span.
        let mut left = content_w;
        for s in r.content {
            let w = s.content.width();
            if w <= left {
                left -= w;
                spans.push(s);
            } else {
                if left > 1 {
                    spans.push(Span::styled(truncate(&s.content, left), s.style));
                }
                left = 0;
            }
            if left == 0 {
                break;
            }
        }
        spans.push(Span::raw(" ".repeat(left)));
    } else {
        spans.extend(r.content);
        spans.push(Span::raw(" ".repeat(content_w - used)));
    }
    if money {
        let right = |s: Option<Span<'static>>, w: usize| match s {
            Some(s) => {
                let sw = s.content.width();
                vec![Span::raw(" ".repeat(w.saturating_sub(sw))), s]
            }
            None => vec![Span::raw(" ".repeat(w))],
        };
        spans.extend(right(r.cost, COST_W));
        spans.extend(right(r.bal, BAL_W));
    }
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::RoundCost;
    use reeve_core::agent::AgentEvent;
    use reeve_core::spend::{Tally, Usage};
    use reeve_observer::HostInfo;

    fn text(lines: &[Line]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
                    + "\n"
            })
            .collect()
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
    ) {
        v.apply(AgentEvent::ToolStarted {
            id: id.into(),
            tool: name.into(),
            tier,
            summary: summary.into(),
        });
        let mut r = reeve_core::receipts::Receipt::draft("s", name, Default::default(), tier);
        r.seq = seq;
        v.apply(AgentEvent::ToolFinished {
            id: id.into(),
            status,
            summary: result.into(),
            diff: None,
            receipt: Some(Box::new(r)),
        });
    }

    #[test]
    fn rounds_carry_their_cost_and_changes_are_bracketed() {
        let mut v = View::new(HostInfo::default());
        v.push(Speaker::User, "mailsync keeps crashing");
        v.apply(AgentEvent::TurnStarted);
        let spend = |usd: f64, session: f64| AgentEvent::Spend {
            usd: Some(usd),
            usage: Usage {
                input_tokens: 7900,
                output_tokens: 153,
                ..Usage::default()
            },
            session: Tally {
                usd: session,
                calls: 1,
                ..Tally::default()
            },
            totals: Box::default(),
        };
        // A round that only calls tools still gets a row with its cost.
        v.apply(spend(0.012, 0.012));
        tool(
            &mut v,
            "1",
            "shell",
            Tier::T0,
            "coredumpctl list mailsync",
            Status::Ok,
            "102 dumps",
            81,
        );
        v.apply(AgentEvent::Text("It dumps core every 15 minutes.".into()));
        v.apply(spend(0.006, 0.018));
        tool(
            &mut v,
            "2",
            "change_begin",
            Tier::T0,
            "verified change: stop mailsync crashing",
            Status::Ok,
            "will check: unit active; no coredumps",
            82,
        );
        tool(
            &mut v,
            "3",
            "svc_control",
            Tier::T1,
            "restart mailspring",
            Status::Ok,
            "restarted",
            83,
        );
        tool(
            &mut v,
            "4",
            "change_commit",
            Tier::T0,
            "check: stop mailsync crashing",
            Status::Ok,
            "verified: stop mailsync crashing (2 checks passed)",
            84,
        );
        let wide = text(&lines(&v, &crate::theme::Theme::ink(), 140, true));
        for needle in [
            "● you",
            "◆ reeve",
            "$0.0120",
            "$0.0180",
            "102 dumps",
            "┏ verified change",
            "checks  unit active · no coredumps",
            "┃ ✓  T1  restart mailspring",
            "┗ verified · kept",
            "#84",
        ] {
            assert!(wide.contains(needle), "missing {needle:?}\n{wide}");
        }
        assert!(v.entries.iter().filter(|e| e.cost.is_some()).count() == 2);
        // Round 1 said nothing: its cost rides on its first tool call.
        assert!(
            v.entries
                .iter()
                .find(|e| e.cost.is_some())
                .is_some_and(|e| e.who == Speaker::Tool)
        );
        assert_eq!(
            v.entries.iter().find_map(|e| e.cost),
            Some(RoundCost {
                usd: Some(0.012),
                usage: Usage {
                    input_tokens: 7900,
                    output_tokens: 153,
                    ..Usage::default()
                },
                session: 0.012,
            })
        );
        // Narrow: no money columns, the cost folds into the row.
        let narrow = text(&lines(&v, &crate::theme::Theme::ink(), 70, false));
        assert!(
            narrow.contains("$0.0060") && !narrow.contains("$0.0180"),
            "{narrow}"
        );
        for l in lines(&v, &crate::theme::Theme::ink(), 70, false) {
            let w: usize = l.spans.iter().map(|s| s.content.width()).sum();
            assert!(w <= 70, "{w}: {l:?}");
        }
    }
}
