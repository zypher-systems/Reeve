//! The page: one self-contained HTML file.

use std::fmt::Write as _;

use chrono::{DateTime, Local, Utc};

use reeve_core::findings::Severity;
use reeve_core::policy::Tier;
use reeve_core::receipts::Status;

use super::svg::{self, Chart, Series, esc};
use super::{Report, Tone, bytes};

fn local(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local)
        .format("%a %-d %b %H:%M")
        .to_string()
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

fn uptime(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, secs % 86_400 / 3600, secs % 3600 / 60);
    if d > 0 {
        format!("{d}d {h}h")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn tier_pill(t: Tier) -> String {
    let c = match t {
        Tier::T0 => "t0",
        Tier::T1 => "t1",
        Tier::T2 => "t2",
        Tier::T3 => "t3",
    };
    format!(r#"<span class="pill {c}">{}</span>"#, t.label())
}

fn window(days: u32) -> &'static str {
    match days {
        1 => "the last 24 hours",
        7 => "the last 7 days",
        28..=31 => "the last 30 days",
        _ => "this period",
    }
}

fn tile(label: &str, value: &str, sub: &str, extra: &str, tone: &str) -> String {
    format!(
        r#"<div class="tile {tone}"><div class="label">{}</div><div class="value">{value}</div><div class="sub">{sub}</div>{extra}</div>"#,
        esc(label)
    )
}

fn card(title: &str, aside: &str, body: &str) -> String {
    format!(
        r#"<section class="card"><header><h2>{}</h2><span class="aside">{aside}</span></header>{body}</section>"#,
        esc(title)
    )
}

fn series(
    r: &Report,
    name: &str,
    color: &'static str,
    area: bool,
    f: impl Fn(&super::Point) -> Option<f64>,
) -> Series {
    Series {
        name: name.into(),
        color,
        values: r
            .points
            .iter()
            .map(|(_, p)| p.as_ref().and_then(&f))
            .collect(),
        area,
        faint: false,
    }
}

fn stat_line(items: &[(&str, String)]) -> String {
    let mut s = String::from(r#"<div class="stats">"#);
    for (k, v) in items {
        let _ = write!(s, r#"<span><em>{}</em>{}</span>"#, esc(k), esc(v));
    }
    s.push_str("</div>");
    s
}

fn legend(items: &[(&str, &str)]) -> String {
    let mut s = String::from(r#"<div class="legend">"#);
    for (name, color) in items {
        let _ = write!(
            s,
            r#"<span><i style="background:{color}"></i>{}</span>"#,
            esc(name)
        );
    }
    s.push_str("</div>");
    s
}

fn charts(r: &Report) -> String {
    let times: Vec<DateTime<Utc>> = r.points.iter().map(|(t, _)| *t).collect();
    let cfg = reeve_core::config::ObserverConfig::default();
    let mut out = String::new();
    if r.coverage == 0.0 {
        return card(
            "Vital signs",
            "",
            r#"<p class="empty">No readings yet for this window. reeved records a row every minute once it's running: <code>systemctl --user enable --now reeved</code>.</p>"#,
        );
    }
    let mut cpu_peak = series(r, "peak", "var(--brass)", false, |p| Some(p.cpu_max));
    cpu_peak.faint = true;
    let cpu = Chart {
        times: &times,
        series: vec![
            cpu_peak,
            series(r, "CPU", "var(--brass)", true, |p| Some(p.cpu)),
        ],
        y_max: Some(100.0),
        unit: "%",
        threshold: None,
        days: chart_days(r),
    };
    out.push_str(&card(
        "CPU",
        &format!("{} cores", r.host.cpus),
        &format!(
            "{}{}{}",
            svg::chart(&cpu),
            legend(&[
                ("average", "var(--brass)"),
                ("peak (faint)", "var(--faint)")
            ]),
            stat_line(&[
                ("avg", format!("{:.0}%", r.cpu.avg)),
                ("peak", format!("{:.0}%", r.cpu.max)),
                ("peak at", r.cpu.max_at.map_or("—".into(), local)),
            ])
        ),
    ));
    let mem = Chart {
        times: &times,
        series: vec![
            series(r, "swap", "var(--copper)", true, |p| Some(p.swap)),
            series(r, "memory", "var(--teal)", true, |p| Some(p.mem)),
        ],
        y_max: Some(100.0),
        unit: "%",
        threshold: None,
        days: chart_days(r),
    };
    out.push_str(&card(
        "Memory and swap",
        "",
        &format!(
            "{}{}{}",
            svg::chart(&mem),
            legend(&[("memory", "var(--teal)"), ("swap", "var(--copper)")]),
            stat_line(&[
                ("memory avg", format!("{:.0}%", r.mem.avg)),
                ("memory peak", format!("{:.0}%", r.mem.max)),
                ("swap avg", format!("{:.0}%", r.swap.avg)),
                (
                    "swap ≥95%",
                    format!("{:.0}% of the time", r.swap_full_share * 100.0)
                ),
            ])
        ),
    ));
    if let Some(t) = r.temp {
        let temp = Chart {
            times: &times,
            series: vec![series(r, "temperature", "var(--amber)", true, |p| p.temp)],
            y_max: Some(svg_top(t.max.max(f64::from(cfg.temp_warn)) + 5.0)),
            unit: "°C",
            threshold: Some((
                f64::from(cfg.temp_warn),
                format!("warn {:.0}°C", cfg.temp_warn),
            )),
            days: chart_days(r),
        };
        out.push_str(&card(
            "Temperature",
            "CPU package",
            &format!(
                "{}{}",
                svg::chart(&temp),
                stat_line(&[
                    ("avg", format!("{:.0}°C", t.avg)),
                    ("peak", format!("{:.0}°C", t.max)),
                    ("peak at", t.max_at.map_or("—".into(), local)),
                ])
            ),
        ));
    }
    let load = Chart {
        times: &times,
        series: vec![series(r, "load", "var(--blue)", true, |p| Some(p.load))],
        y_max: None,
        unit: "",
        threshold: (r.host.cpus > 0)
            .then(|| (r.host.cpus as f64, format!("{} cores", r.host.cpus))),
        days: chart_days(r),
    };
    out.push_str(&card(
        "Load",
        "1-minute average",
        &format!(
            "{}{}",
            svg::chart(&load),
            stat_line(&[
                ("avg", format!("{:.2}", r.load.avg)),
                ("peak", format!("{:.2}", r.load.max)),
                ("peak at", r.load.max_at.map_or("—".into(), local)),
            ])
        ),
    ));
    let net = Chart {
        times: &times,
        series: vec![
            series(r, "in", "var(--teal)", true, |p| Some(p.rx)),
            series(r, "out", "var(--blue)", false, |p| Some(p.tx)),
        ],
        y_max: None,
        unit: "B/s",
        threshold: None,
        days: chart_days(r),
    };
    let (rx, tx) = r
        .points
        .iter()
        .filter_map(|(_, p)| *p)
        .fold((0.0, 0.0), |a, p| (a.0 + p.rx, a.1 + p.tx));
    let n = r.points.iter().filter(|(_, p)| p.is_some()).count().max(1) as f64;
    out.push_str(&card(
        "Network",
        "",
        &format!(
            "{}{}{}",
            svg::chart(&net),
            legend(&[("in", "var(--teal)"), ("out", "var(--blue)")]),
            stat_line(&[
                ("in avg", format!("{}/s", bytes(rx / n))),
                ("out avg", format!("{}/s", bytes(tx / n))),
            ])
        ),
    ));
    format!(r#"<div class="grid">{out}</div>"#)
}

/// Days the charts span, for their axis.
fn chart_days(r: &Report) -> u32 {
    let span = Utc::now() - r.chart_since;
    (span.num_hours() as f64 / 24.0).ceil().max(1.0) as u32
}

fn svg_top(v: f64) -> f64 {
    (v / 10.0).ceil() * 10.0
}

fn disks(r: &Report) -> String {
    let warn = reeve_core::config::ObserverConfig::default().disk_warn;
    let mut rows = String::new();
    for d in &r.disks {
        let color = if d.now >= warn {
            "var(--bad)"
        } else if d.now >= warn - 0.15 {
            "var(--warn)"
        } else {
            "var(--teal)"
        };
        let trend: Vec<f64> = d.series.iter().map(|(_, v)| v * 100.0).collect();
        let growth = if d.series.len() < 20 {
            r#"<span class="muted">learning</span>"#.to_string()
        } else if d.per_day.abs() * (d.total as f64) < 1e6 {
            r#"<span class="muted">steady</span>"#.to_string()
        } else {
            format!(
                "{}{}/day",
                if d.per_day > 0.0 { "+" } else { "" },
                esc(&bytes(d.per_day * d.total as f64))
            )
        };
        let forecast = match d.days_to_full {
            Some(days) if days < 30.0 => {
                format!(r#"<span class="bad">full in ~{days:.0} days</span>"#)
            }
            Some(days) if days < 365.0 => {
                format!(r#"<span class="warn">full in ~{days:.0} days</span>"#)
            }
            _ => r#"<span class="muted">no end in sight</span>"#.to_string(),
        };
        let _ = write!(
            rows,
            r#"<div class="disk">{ring}<div class="disk-main"><div class="disk-head"><b>{mount}</b><span>{used} of {total}</span></div><div class="bar"><i style="width:{pct:.1}%;background:{color}"></i></div><div class="disk-foot"><span>{growth}</span><span>{forecast}</span></div></div><div class="disk-spark">{spark}</div></div>"#,
            ring = svg::ring(d.now, color, &format!("{:.0}%", d.now * 100.0)),
            mount = esc(&d.mount),
            used = esc(&bytes(d.used as f64)),
            total = esc(&bytes(d.total as f64)),
            pct = d.now * 100.0,
            spark = svg::spark_range(&trend, color, 160.0, 40.0, 2.0),
        );
    }
    if rows.is_empty() {
        rows = r#"<p class="empty">No disks found.</p>"#.into();
    }
    card(
        "Disks",
        "trend from hourly readings",
        &format!(r#"<div class="disks">{rows}</div>"#),
    )
}

fn pkg_list(title: &str, class: &str, sign: &str, pkgs: &[super::drift::Pkg]) -> String {
    if pkgs.is_empty() {
        return String::new();
    }
    let mut items = String::new();
    for p in pkgs.iter().take(400) {
        let ver = match (&p.from, &p.to) {
            (Some(a), Some(b)) => format!("{a} → {b}"),
            (None, Some(b)) => b.clone(),
            (Some(a), None) => a.clone(),
            _ => String::new(),
        };
        let _ = write!(
            items,
            r#"<li><span class="mono">{}</span><span class="muted">{}</span><span class="muted">{}</span></li>"#,
            esc(&p.name),
            esc(&ver),
            p.at.map_or(String::new(), local)
        );
    }
    let more = pkgs.len().saturating_sub(400);
    if more > 0 {
        let _ = write!(items, r#"<li class="muted">and {more} more</li>"#);
    }
    format!(
        r#"<details class="pkgs"><summary><span class="count {class}">{sign}{}</span> {}</summary><ul class="rows">{items}</ul></details>"#,
        pkgs.len(),
        esc(title)
    )
}

fn drift(r: &Report) -> String {
    let d = &r.drift;
    let mut body = String::new();
    if let Some(k) = &d.reboot_for {
        let _ = write!(
            body,
            r#"<div class="callout info"><b>Reboot pending.</b> Kernel {} is installed; the machine is still running {}.</div>"#,
            esc(k),
            esc(&r.host.kernel)
        );
    }
    let total = d.installed.len() + d.upgraded.len() + d.removed.len() + d.touched.len();
    let mut pk = String::new();
    pk.push_str(&pkg_list("installed", "good", "+", &d.installed));
    pk.push_str(&pkg_list("upgraded", "teal", "↑", &d.upgraded));
    pk.push_str(&pkg_list("installed or upgraded", "teal", "↑", &d.touched));
    pk.push_str(&pkg_list("removed", "bad", "−", &d.removed));
    if !d.kernels.is_empty() {
        let _ = write!(
            pk,
            r#"<p class="note">Kernels: {}</p>"#,
            esc(&d.kernels.join(", "))
        );
    }
    if total == 0 {
        pk.push_str(r#"<p class="empty">No package changes.</p>"#);
    }
    let baseline = match d.baseline {
        Some(t) => format!("compared with the snapshot from {}", local(t)),
        None => "reeved takes a daily snapshot; removals and unit changes show once there's one from before this window".into(),
    };
    let mut units = String::new();
    for u in &d.units_enabled {
        let _ = write!(
            units,
            r#"<li><span class="count good">on</span><span class="mono">{}</span></li>"#,
            esc(u)
        );
    }
    for u in &d.units_disabled {
        let _ = write!(
            units,
            r#"<li><span class="count bad">off</span><span class="mono">{}</span></li>"#,
            esc(u)
        );
    }
    if units.is_empty() {
        units = format!(
            r#"<li class="muted">{}</li>"#,
            if d.baseline.is_some() {
                "No units enabled or disabled."
            } else {
                "Needs a snapshot from before this window."
            }
        );
    }
    let mut etc = String::new();
    for e in d.etc.iter().take(60) {
        let who = e.by_reeve.map_or(String::new(), |n| {
            format!(r#"<span class="badge">Reeve #{n}</span>"#)
        });
        let _ = write!(
            etc,
            r#"<li><span class="mono">{}</span>{who}<span class="muted">{}</span></li>"#,
            esc(&e.path),
            ago(e.at)
        );
    }
    if d.etc_total > 60 {
        let _ = write!(
            etc,
            r#"<li class="muted">and {} more</li>"#,
            d.etc_total - 60
        );
    }
    if etc.is_empty() {
        etc = r#"<li class="muted">Nothing in /etc changed.</li>"#.into();
    }
    let by_reeve = d.etc.iter().filter(|e| e.by_reeve.is_some()).count();
    let _ = write!(
        body,
        r#"<div class="cols3"><div><h3>Packages <span class="muted">{}</span></h3>{pk}</div><div><h3>Services</h3><ul class="rows">{units}</ul></div><div><h3>/etc <span class="muted">{} files{}</span></h3><ul class="rows scroll">{etc}</ul></div></div><p class="note">From {}; {baseline}.</p>"#,
        total,
        d.etc_total,
        if by_reeve > 0 {
            format!(", {by_reeve} by Reeve")
        } else {
            String::new()
        },
        esc(&d.source),
    );
    card("What changed", window(r.days), &body)
}

fn findings(r: &Report) -> String {
    if r.findings.is_empty() {
        return card(
            "Findings",
            &format!("{} resolved", r.resolved),
            r#"<p class="empty good">Nothing open. reeved is watching.</p>"#,
        );
    }
    let mut rows = String::new();
    for f in r.findings.iter().take(30) {
        let (class, word) = match f.severity {
            Severity::Critical => ("bad", "critical"),
            Severity::Warning => ("warn", "warning"),
            Severity::Info => ("teal", "info"),
        };
        let fix = if f.proposal.is_some() {
            r#"<span class="badge">fix drafted</span>"#
        } else {
            ""
        };
        let _ = write!(
            rows,
            r#"<tr><td><span class="pill {class}">{word}</span></td><td><b>{}</b>{fix}<div class="muted small">{}</div></td><td class="num">{}×</td><td class="muted">{}</td><td class="muted">{}</td></tr>"#,
            esc(&f.title),
            esc(f.detail.lines().next().unwrap_or("")),
            f.count,
            ago(f.first_seen),
            ago(f.last_seen)
        );
    }
    card(
        "Findings",
        &format!("{} open · {} resolved", r.findings.len(), r.resolved),
        &format!(
            r#"<table class="findings"><colgroup><col class="sev"><col><col class="n"><col class="t"><col class="t"></colgroup><thead><tr><th></th><th>what</th><th class="num">seen</th><th>first</th><th>last</th></tr></thead><tbody>{rows}</tbody></table><p class="note">Open <code>/findings</code> in Reeve to look into one or use its drafted fix.</p>"#
        ),
    )
}

fn work(r: &Report) -> String {
    let changes: Vec<_> = super::changes(r).collect();
    let reads = r
        .receipts
        .iter()
        .filter(|x| x.tier == Tier::T0 && !x.tool.starts_with("change_"))
        .count();
    let verified = r
        .receipts
        .iter()
        .filter(|x| x.tool == "change_commit" && x.outcome.status == Status::Ok)
        .count();
    let rolled = r
        .receipts
        .iter()
        .filter(|x| x.tool == "change_commit" && x.outcome.status == Status::Error)
        .count();
    let undone = r
        .receipts
        .iter()
        .filter(|x| x.undoes.is_some() && x.outcome.status == Status::Ok)
        .count();
    let said_no = r
        .receipts
        .iter()
        .filter(|x| matches!(x.outcome.status, Status::Denied | Status::Refused))
        .count();
    let mut body = stat_line(&[
        (
            "changes",
            changes
                .iter()
                .filter(|x| !x.tool.starts_with("change_") && x.undoes.is_none())
                .count()
                .to_string(),
        ),
        ("verified", verified.to_string()),
        ("rolled back", rolled.to_string()),
        ("undone", undone.to_string()),
        ("declined or refused", said_no.to_string()),
        ("looked", plural(reads, "time", "times")),
    ]);
    if changes.is_empty() {
        body.push_str(r#"<p class="empty">Reeve changed nothing in this window.</p>"#);
    } else {
        body.push_str(r#"<ol class="timeline">"#);
        for x in changes.iter().rev().take(40) {
            let (icon, class) = match x.outcome.status {
                Status::Ok => ("✓", "good"),
                Status::Error => ("✗", "bad"),
                Status::Denied => ("⊘", "warn"),
                Status::Refused => ("⊗", "bad"),
            };
            let what = match x.tool.as_str() {
                "change_begin" => format!("verified change: {}", x.target()),
                "change_commit" => x.outcome.summary.clone(),
                "undo" => format!("undo {}", x.target()),
                _ => x.target(),
            };
            let by = match x.approved_by.as_str() {
                "-" => String::new(),
                b => format!(r#"<span class="muted small">{}</span>"#, esc(b)),
            };
            let _ = write!(
                body,
                r#"<li><span class="when">{}</span><span class="icon {class}">{icon}</span>{}<span class="tool">{}</span><span class="what">{}</span>{by}<span class="seq">#{}</span></li>"#,
                local(x.ts),
                tier_pill(x.tier),
                esc(&x.tool),
                esc(&what),
                x.seq
            );
        }
        body.push_str("</ol>");
    }
    card("Reeve's work", "from the receipts", &body)
}

fn spend(r: &Report) -> String {
    let roles: [(&str, &'static str); 4] = [
        ("chat", "var(--brass)"),
        ("drafter", "var(--teal)"),
        ("orders", "var(--blue)"),
        ("reflect", "var(--copper)"),
    ];
    let cols: Vec<(String, Vec<(f64, &'static str)>)> = r
        .spend
        .iter()
        .map(|d| {
            let label = if r.days <= 10 {
                d.day.format("%a").to_string()
            } else {
                d.day.format("%-d").to_string()
            };
            let parts = roles
                .iter()
                .map(|(role, color)| (d.by_role.get(*role).copied().unwrap_or(0.0), *color))
                .chain(std::iter::once((
                    d.by_role
                        .iter()
                        .filter(|(k, _)| !roles.iter().any(|(r, _)| r == k))
                        .map(|(_, v)| v)
                        .sum::<f64>(),
                    "var(--dim)",
                )))
                .collect();
            (label, parts)
        })
        .collect();
    let total: f64 = r.spend.iter().flat_map(|d| d.by_role.values()).sum();
    let unpriced: u32 = r.spend.iter().map(|d| d.unpriced).sum();
    let mut aside = format!("${total:.2} in {}", window(r.days));
    if unpriced > 0 {
        let _ = write!(aside, " · {unpriced} calls unpriced");
    }
    let chart = if total > 0.0 {
        svg::bars(&cols, (r.daily_cap > 0.0).then_some(r.daily_cap), "$")
    } else {
        r#"<p class="empty">No spend in this window.</p>"#.to_string()
    };
    card("Spend", &esc(&aside), &format!("{chart}{}", legend(&roles)))
}

fn memory(r: &Report) -> String {
    let m = &r.memory;
    let mut body = format!(
        r#"<div class="mem"><div><b>{}</b><span>facts</span></div><div><b>{}</b><span>runbooks</span></div><div><b>{}</b><span>preferences</span></div><div><b>{}</b><span>new</span></div></div>"#,
        m.facts, m.runbooks, m.preferences, m.new
    );
    if m.pending > 0 {
        let _ = write!(
            body,
            r#"<p class="note">{} waiting for you in <code>/memory</code>.</p>"#,
            plural(m.pending, "preference is", "preferences are")
        );
    }
    if !m.top_runbooks.is_empty() {
        body.push_str(r#"<h3>Runbooks that get used</h3><ul class="rows">"#);
        for (title, ok, bad) in &m.top_runbooks {
            let _ = write!(
                body,
                r#"<li><span>{}</span><span class="good">{ok} worked</span><span class="{}">{bad} failed</span></li>"#,
                esc(title),
                if *bad > 0 { "bad" } else { "muted" }
            );
        }
        body.push_str("</ul>");
    }
    card("What Reeve knows", "gets better as it goes", &body)
}

fn tiles(r: &Report) -> String {
    let pick = |f: fn(&super::Point) -> f64| -> Vec<f64> {
        r.points
            .iter()
            .filter_map(|(_, p)| p.as_ref().map(f))
            .collect()
    };
    let mut t = String::new();
    let has = r.coverage > 0.0;
    let dash = "—".to_string();
    t.push_str(&tile(
        "CPU",
        &if has {
            format!("{:.0}%", r.cpu.avg)
        } else {
            dash.clone()
        },
        &format!("average · peak {:.0}%", r.cpu.max),
        &svg::spark(&pick(|p| p.cpu), "var(--brass)", 120.0, 28.0, Some(100.0)),
        "",
    ));
    t.push_str(&tile(
        "Memory",
        &if has {
            format!("{:.0}%", r.mem.avg)
        } else {
            dash.clone()
        },
        &format!("average · peak {:.0}%", r.mem.max),
        &svg::spark(&pick(|p| p.mem), "var(--teal)", 120.0, 28.0, Some(100.0)),
        "",
    ));
    t.push_str(&tile(
        "Swap",
        &if has {
            format!("{:.0}%", r.swap.avg)
        } else {
            dash.clone()
        },
        &format!("≥95% for {:.0}% of the time", r.swap_full_share * 100.0),
        &svg::spark(&pick(|p| p.swap), "var(--copper)", 120.0, 28.0, Some(100.0)),
        if r.swap_full_share >= 0.5 { "warn" } else { "" },
    ));
    if let Some(temp) = r.temp {
        t.push_str(&tile(
            "Peak temperature",
            &format!("{:.0}°C", temp.max),
            &format!("average {:.0}°C", temp.avg),
            &svg::spark(
                &r.points
                    .iter()
                    .filter_map(|(_, p)| p.and_then(|p| p.temp))
                    .collect::<Vec<_>>(),
                "var(--amber)",
                120.0,
                28.0,
                None,
            ),
            if temp.max >= 90.0 { "warn" } else { "" },
        ));
    }
    let worst = r.disks.iter().max_by(|a, b| a.now.total_cmp(&b.now));
    if let Some(d) = worst {
        t.push_str(&tile(
            "Fullest disk",
            &format!("{:.0}%", d.now * 100.0),
            &esc(&format!(
                "{} · {} free",
                d.mount,
                bytes(d.total.saturating_sub(d.used) as f64)
            )),
            "",
            if d.now >= 0.9 { "bad" } else { "" },
        ));
    }
    let crit = r
        .findings
        .iter()
        .filter(|f| f.severity == Severity::Critical)
        .count();
    t.push_str(&tile(
        "Open findings",
        &r.findings.len().to_string(),
        &if crit > 0 {
            format!("{crit} critical")
        } else {
            format!("{} resolved", r.resolved)
        },
        "",
        if crit > 0 {
            "bad"
        } else if r.findings.is_empty() {
            "good"
        } else {
            "warn"
        },
    ));
    let verified = r
        .receipts
        .iter()
        .filter(|x| x.tool == "change_commit" && x.outcome.status == Status::Ok)
        .count();
    t.push_str(&tile(
        "Reeve's changes",
        &super::changes(r)
            .filter(|x| {
                !x.tool.starts_with("change_")
                    && x.undoes.is_none()
                    && x.outcome.status == Status::Ok
            })
            .count()
            .to_string(),
        &format!("{verified} verified"),
        "",
        "",
    ));
    let total: f64 = r.spend.iter().flat_map(|d| d.by_role.values()).sum();
    t.push_str(&tile(
        "Spend",
        &format!("${total:.2}"),
        window(r.days),
        "",
        "",
    ));
    format!(r#"<div class="tiles">{t}</div>"#)
}

fn headlines(r: &Report) -> String {
    let mut s = String::from(r#"<ul class="headlines">"#);
    for h in &r.headlines {
        let (class, icon) = match h.tone {
            Tone::Bad => ("bad", "●"),
            Tone::Warn => ("warn", "▲"),
            Tone::Info => ("info", "◆"),
            Tone::Good => ("good", "✓"),
        };
        let _ = write!(
            s,
            r#"<li class="{class}"><i>{icon}</i>{}</li>"#,
            esc(&h.text)
        );
    }
    s.push_str("</ul>");
    s
}

/// The whole page.
pub fn render(r: &Report) -> String {
    let host = if r.host.hostname.is_empty() {
        "this machine"
    } else {
        &r.host.hostname
    };
    let facts: Vec<String> = [
        r.host.os_pretty.clone(),
        if r.host.kernel.is_empty() {
            String::new()
        } else {
            format!("kernel {}", r.host.kernel)
        },
        if r.host.cpu_model.is_empty() {
            String::new()
        } else {
            format!("{} × {}", r.host.cpu_model, r.host.cpus)
        },
        format!("up {}", uptime(r.uptime_secs)),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect();
    let observer = match &r.observer {
        Some(s) if s.alive(Utc::now()) => r#"<span class="live">● reeved live</span>"#.to_string(),
        _ => r#"<span class="muted">○ reeved not running</span>"#.to_string(),
    };
    format!(
        r##"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>{host} · Reeve</title>
<style>{CSS}</style></head>
<body>
<main>
<header class="hero">
  <div class="brand"><span class="mark">◆</span> R E E V E <span class="muted">· state of the machine</span></div>
  <div class="hero-row">
    <div><h1>{host_e}</h1><p class="facts">{facts}</p></div>
    <div class="when"><div>{window}</div><div class="muted">made {made} · {observer}</div></div>
  </div>
</header>
{headlines}
{tiles}
{charts}
{disks}
{drift}
<div class="grid">{findings}{work}</div>
<div class="grid">{spend}{memory}</div>
<footer>Drawn by Reeve {ver} on this machine from <code>~/.reeve</code>: reeved's minute readings ({cov:.0}% of the window), findings, receipts, the spend ledger, memory, rpm/pacman, systemd, and /etc. No model was asked and nothing on this page left the machine.</footer>
</main>
<script>{JS}</script>
</body></html>"##,
        host = esc(host),
        host_e = esc(host),
        facts = esc(&facts.join(" · ")),
        window = esc(&format!(
            "Last {}",
            match r.days {
                1 => "24 hours".to_string(),
                d => format!("{d} days"),
            }
        )),
        made = esc(&r.generated.format("%a %-d %b %Y, %H:%M").to_string()),
        headlines = headlines(r),
        tiles = tiles(r),
        charts = charts(r),
        disks = disks(r),
        drift = drift(r),
        findings = findings(r),
        work = work(r),
        spend = spend(r),
        memory = memory(r),
        ver = esc(&r.version),
        cov = r.coverage * 100.0,
    )
}

const CSS: &str = r#"
:root{--bg:#0b1020;--bg2:#10172a;--panel:#121929;--panel2:#172033;--border:#263149;--fg:#dce1ea;--dim:#8290a8;--faint:#46516a;
--brass:#d6ab52;--amber:#f2b94b;--copper:#dd7a50;--teal:#4cc3b1;--blue:#8db8ff;--good:#7fd18b;--warn:#f0a04b;--bad:#ec5f6b;
--grid:rgba(130,144,168,.14);--shadow:0 1px 0 rgba(255,255,255,.03) inset,0 10px 30px rgba(0,0,0,.25)}
@media (prefers-color-scheme:light){:root{--bg:#f5f1e8;--bg2:#efe9dc;--panel:#fffdf8;--panel2:#f3eee3;--border:#e1d8c6;--fg:#1d2433;--dim:#5d6678;--faint:#b3b8c2;
--brass:#9a7426;--amber:#b7841d;--copper:#b3552d;--teal:#16877a;--blue:#3767c4;--good:#2f8a45;--warn:#b8660f;--bad:#c43645;--grid:rgba(29,36,51,.09);--shadow:0 8px 24px rgba(60,48,20,.08)}}
*{box-sizing:border-box}
body{margin:0;background:radial-gradient(1200px 600px at 10% -10%,var(--bg2),var(--bg));background-attachment:fixed;color:var(--fg);
font:15px/1.5 system-ui,-apple-system,"Segoe UI",Inter,Roboto,sans-serif;font-variant-numeric:tabular-nums}
main{max-width:1240px;margin:0 auto;padding:32px 28px 48px}
code,.mono{font-family:ui-monospace,"JetBrains Mono","Cascadia Code",Menlo,monospace;font-size:.88em}
code{background:var(--panel2);padding:1px 6px;border-radius:5px}
.muted{color:var(--dim)}.small{font-size:12.5px}.good{color:var(--good)}.warn{color:var(--warn)}.bad{color:var(--bad)}.teal{color:var(--teal)}
.hero{margin-bottom:22px}
.brand{font:600 12px/1 ui-monospace,Menlo,monospace;letter-spacing:.14em;color:var(--brass);margin-bottom:14px}
.brand .mark{color:var(--amber)}
.hero-row{display:flex;justify-content:space-between;align-items:flex-end;gap:24px;flex-wrap:wrap}
h1{margin:0;font-size:44px;line-height:1.05;letter-spacing:-.02em;font-weight:700}
.facts{margin:8px 0 0;color:var(--dim)}
.when{text-align:right;font-size:14px;margin-left:auto;white-space:nowrap}.when>div:first-child{font-weight:600;font-size:16px}
.live{color:var(--good)}
.headlines{list-style:none;margin:0 0 18px;padding:0;display:grid;gap:8px}
.headlines li{display:flex;gap:12px;align-items:center;padding:11px 16px;border-radius:12px;background:var(--panel);border:1px solid var(--border);box-shadow:var(--shadow)}
.headlines i{font-style:normal;width:18px;text-align:center}
.headlines .bad{border-color:color-mix(in srgb,var(--bad) 45%,var(--border))}.headlines .bad i{color:var(--bad)}
.headlines .warn i{color:var(--warn)}.headlines .info i{color:var(--teal)}.headlines .good i{color:var(--good)}
.tiles{display:grid;grid-template-columns:repeat(auto-fit,minmax(134px,1fr));gap:12px;margin-bottom:18px}
.tile{position:relative;padding:14px 16px 12px;border-radius:14px;background:var(--panel);border:1px solid var(--border);box-shadow:var(--shadow);overflow:hidden}
.tile .label{font:600 11px/1 ui-monospace,Menlo,monospace;letter-spacing:.1em;text-transform:uppercase;color:var(--dim)}
.tile .value{font-size:28px;font-weight:700;margin-top:8px;letter-spacing:-.01em}
.tile .sub{color:var(--dim);font-size:12.5px}
.tile .spark{display:block;width:100%;height:28px;margin-top:8px}
.tile.warn .value{color:var(--warn)}.tile.bad .value{color:var(--bad)}.tile.good .value{color:var(--good)}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(520px,1fr));gap:14px;margin-bottom:14px}
.card{background:var(--panel);border:1px solid var(--border);border-radius:16px;padding:16px 18px 14px;box-shadow:var(--shadow);margin-bottom:14px;min-width:0}
.grid>.card{margin-bottom:0}
.card>header{display:flex;justify-content:space-between;align-items:baseline;gap:12px;margin-bottom:10px}
h2{margin:0;font:600 12px/1 ui-monospace,Menlo,monospace;letter-spacing:.14em;text-transform:uppercase;color:var(--brass)}
h3{margin:4px 0 8px;font-size:14px}
.aside{color:var(--dim);font-size:13px}
.plot{position:relative}
svg.chart{display:block;width:100%;height:auto;overflow:visible}
.grid line.grid,svg line.grid{stroke:var(--grid);stroke-width:1}svg line.grid.soft{stroke-dasharray:2 4}
svg .axis{fill:var(--dim);font:12.5px ui-monospace,Menlo,monospace}svg .axis.warn{fill:var(--warn)}svg .axis.strong{fill:var(--fg);font-weight:600}
svg .line{stroke-width:1.8;stroke-linejoin:round}svg .line.faint{stroke-width:1;opacity:.45}
svg .area{opacity:.16}
svg .threshold{stroke:var(--warn);stroke-dasharray:5 4;opacity:.7}
svg .cursor{stroke:var(--dim);stroke-width:1;opacity:.6}
svg.spark path{stroke-width:1.6}
.tip{position:absolute;top:6px;pointer-events:none;background:var(--panel2);border:1px solid var(--border);border-radius:9px;padding:7px 10px;font-size:12.5px;white-space:nowrap;box-shadow:var(--shadow)}
.tip b{display:block;font-weight:600;margin-bottom:2px}.tip i{display:inline-block;width:8px;height:8px;border-radius:2px;margin-right:6px}
.legend{display:flex;gap:14px;margin-top:6px;color:var(--dim);font-size:12.5px}.legend i{display:inline-block;width:10px;height:10px;border-radius:3px;margin-right:6px;vertical-align:-1px}
.stats{display:flex;flex-wrap:wrap;gap:6px 18px;margin-top:8px;font-size:13.5px}.stats em{font-style:normal;color:var(--dim);margin-right:6px}
.disks{display:grid;gap:12px}
.disk{display:grid;grid-template-columns:64px 1fr 170px;gap:16px;align-items:center;padding:6px 0;border-top:1px solid var(--border)}
.disk:first-child{border-top:0}
svg.ring{width:64px;height:64px}.ring-bg{fill:none;stroke:var(--panel2);stroke-width:7}.ring-label{fill:var(--fg);font:700 13px system-ui,sans-serif}
.disk-head,.disk-foot{display:flex;justify-content:space-between;gap:12px;font-size:13.5px}.disk-head b{font-size:15px}
.bar{height:8px;border-radius:5px;background:var(--panel2);margin:7px 0;overflow:hidden}.bar i{display:block;height:100%;border-radius:5px}
.disk-spark svg{width:170px;height:40px;display:block}
.callout{padding:10px 14px;border-radius:10px;margin-bottom:12px;background:color-mix(in srgb,var(--teal) 12%,transparent);border:1px solid color-mix(in srgb,var(--teal) 35%,var(--border))}
.cols3{display:grid;grid-template-columns:1.2fr 1fr 1.4fr;gap:22px}
ul.rows{list-style:none;margin:0;padding:0}ul.rows li{display:flex;gap:10px;justify-content:space-between;align-items:baseline;padding:5px 0;border-bottom:1px dashed var(--border);font-size:13.5px}
ul.rows li>:first-child{overflow:hidden;text-overflow:ellipsis;white-space:nowrap;min-width:0}ul.rows li>:last-child{white-space:nowrap}
ul.rows.scroll{max-height:320px;overflow:auto}
details.pkgs{margin-bottom:6px}details.pkgs summary{cursor:pointer;padding:6px 0;list-style:none}details.pkgs summary::-webkit-details-marker{display:none}
details.pkgs ul{max-height:280px;overflow:auto;margin:4px 0 10px}
.count{display:inline-block;min-width:44px;text-align:center;font-weight:700;padding:1px 8px;border-radius:7px;background:var(--panel2);margin-right:6px}
.count.good{color:var(--good)}.count.bad{color:var(--bad)}.count.teal{color:var(--teal)}
.badge{font-size:11px;font-weight:600;color:var(--amber);border:1px solid color-mix(in srgb,var(--amber) 45%,var(--border));border-radius:6px;padding:0 6px;margin-left:8px;white-space:nowrap}
.note{color:var(--dim);font-size:12.5px;margin:10px 0 0}
.empty{color:var(--dim);margin:6px 0}.empty.good{color:var(--good)}
table{width:100%;border-collapse:collapse;font-size:13.5px}th{text-align:left;color:var(--dim);font-weight:500;font-size:12px;padding:4px 8px}
td{padding:8px;border-top:1px solid var(--border);vertical-align:top;overflow-wrap:anywhere}
table.findings{table-layout:fixed}table.findings col.sev{width:84px}table.findings col.n{width:64px}table.findings col.t{width:76px}td.num,th.num{text-align:right}
.pill{display:inline-block;font:600 10.5px/1.6 ui-monospace,Menlo,monospace;letter-spacing:.04em;padding:0 7px;border-radius:6px;background:var(--panel2);color:var(--dim)}
.pill.bad,.pill.t3{color:var(--bad)}.pill.warn,.pill.t2{color:var(--amber)}.pill.teal,.pill.t1{color:var(--teal)}
ol.timeline{list-style:none;margin:10px 0 0;padding:0;max-height:420px;overflow:auto}
ol.timeline li{display:grid;grid-template-columns:120px 18px 34px 96px 1fr auto auto;gap:8px;align-items:baseline;padding:6px 0;border-top:1px solid var(--border);font-size:13.5px}
ol.timeline .when{color:var(--dim);font-size:12.5px;text-align:left}ol.timeline .tool{font-family:ui-monospace,Menlo,monospace;font-size:12px;color:var(--dim)}
ol.timeline .what{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}ol.timeline .seq{color:var(--faint);font-size:12px}
.mem{display:grid;grid-template-columns:repeat(4,1fr);gap:10px;margin-bottom:6px}.mem div{background:var(--panel2);border-radius:12px;padding:10px 12px}
.mem b{display:block;font-size:26px}.mem span{color:var(--dim);font-size:12.5px}
footer{color:var(--dim);font-size:12.5px;margin-top:26px;text-align:center;line-height:1.6}
@media (max-width:760px){main{padding:20px 14px}.when{white-space:normal}.grid{grid-template-columns:1fr}.cols3{grid-template-columns:1fr}h1{font-size:34px}
.disk{grid-template-columns:56px 1fr}.disk-spark{display:none}ol.timeline li{grid-template-columns:18px 34px 1fr auto}ol.timeline .when,ol.timeline .tool,ol.timeline .muted{display:none}.when{text-align:left}}
@media print{body{background:#fff}.card,.tile,.headlines li{box-shadow:none}}
"#;

const JS: &str = r#"
document.querySelectorAll('.plot').forEach(function(p){
  var d; try{d=JSON.parse(p.dataset.series)}catch(e){return}
  var svg=p.querySelector('svg'),cur=svg.querySelector('.cursor'),tip=p.querySelector('.tip'),n=d.t.length;
  var fmt=function(v){if(v==null)return'—';var u=d.unit;
    if(u==='B/s'){var s=['B','KB','MB','GB'],i=0;while(v>=1000&&i<3){v/=1000;i++}return(v>=100||i==0?v.toFixed(0):v.toFixed(1))+' '+s[i]+'/s'}
    if(u==='%')return v.toFixed(0)+'%';if(u==='°C')return v.toFixed(0)+'°C';return v.toFixed(v>=10?0:2)};
  svg.addEventListener('mousemove',function(e){
    var r=svg.getBoundingClientRect(),x=(e.clientX-r.left)/r.width*d.w;
    var f=(x-d.l)/(d.w-d.l-d.r);if(f<0||f>1){cur.setAttribute('visibility','hidden');tip.hidden=true;return}
    var i=Math.min(n-1,Math.max(0,Math.round(f*(n-1)))),cx=d.l+(d.w-d.l-d.r)*i/(n-1);
    cur.setAttribute('x1',cx);cur.setAttribute('x2',cx);cur.setAttribute('visibility','visible');
    var t=new Date(d.t[i]*1000),h='<b>'+t.toLocaleString([], {weekday:'short',day:'numeric',month:'short',hour:'2-digit',minute:'2-digit'})+'</b>';
    d.s.forEach(function(s){h+='<div><i style="background:'+s.c+'"></i>'+s.n+' '+fmt(s.v[i])+'</div>'});
    tip.innerHTML=h;tip.hidden=false;var px=cx/d.w*r.width;
    tip.style.left=(px>r.width/2?px-tip.offsetWidth-12:px+12)+'px'});
  svg.addEventListener('mouseleave',function(){cur.setAttribute('visibility','hidden');tip.hidden=true});
});
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untrusted_text_cannot_break_out() {
        let mut f = reeve_core::findings::Finding {
            id: "x:y".into(),
            severity: Severity::Critical,
            title: "<script>alert(1)</script>".into(),
            detail: "\"><img src=x onerror=alert(1)>".into(),
            evidence: vec![],
            first_seen: Utc::now(),
            last_seen: Utc::now(),
            count: 1,
            status: reeve_core::findings::FindingStatus::Open,
            resolved_at: None,
            notified_at: None,
            notified_severity: None,
            proposal: None,
            draft_note: None,
        };
        f.count = 3;
        let r = super::super::tests_support::report_with(vec![f]);
        let page = render(&r);
        assert!(!page.contains("<script>alert"), "title escaped");
        assert!(!page.contains("<img src=x"), "detail escaped");
        assert_eq!(
            page.matches("<script>").count(),
            1,
            "only Reeve's own script"
        );
    }
}
