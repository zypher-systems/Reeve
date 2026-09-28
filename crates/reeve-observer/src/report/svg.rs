//! Inline SVG charts. Colors are CSS variables, so the page's light and
//! dark themes restyle them; a small script adds a hover readout, and the
//! charts read fine without it.

use std::fmt::Write as _;

use chrono::{DateTime, Duration, Local, TimeZone, Timelike, Utc};

/// Escape for text and attributes.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            _ => o.push(c),
        }
    }
    o
}

/// One series.
pub struct Series {
    /// Legend name.
    pub name: String,
    /// CSS color, e.g. `var(--teal)`.
    pub color: &'static str,
    /// One value per time; `None` is a gap.
    pub values: Vec<Option<f64>>,
    /// Fill under the line.
    pub area: bool,
    /// Draw thin and faint (a peak band's edge).
    pub faint: bool,
}

/// A time chart.
pub struct Chart<'a> {
    /// Bucket times.
    pub times: &'a [DateTime<Utc>],
    /// Series, back to front.
    pub series: Vec<Series>,
    /// Fixed top of the scale (100 for percentages).
    pub y_max: Option<f64>,
    /// Unit for labels and the readout: `%`, `°C`, `B/s`, ``.
    pub unit: &'static str,
    /// A dashed line with a label (a warning level).
    pub threshold: Option<(f64, String)>,
    /// Window in days, for the axis.
    pub days: u32,
}

const W: f64 = 720.0;
const H: f64 = 200.0;
const L: f64 = 46.0;
const R: f64 = 10.0;
const T: f64 = 10.0;
const B: f64 = 24.0;

/// A round number at or above `v`.
fn nice_ceiling(v: f64) -> f64 {
    if v <= 0.0 {
        return 1.0;
    }
    let mag = 10f64.powf(v.log10().floor());
    for m in [1.0, 2.0, 2.5, 5.0, 10.0] {
        if m * mag >= v {
            return m * mag;
        }
    }
    10.0 * mag
}

/// A value with its unit, short.
pub fn fmt(v: f64, unit: &str) -> String {
    match unit {
        "B/s" => format!("{}/s", super::bytes(v)),
        "%" => format!("{v:.0}%"),
        "°C" => format!("{v:.0}°C"),
        "$" => format!("${v:.2}"),
        _ if v >= 10.0 => format!("{v:.0}"),
        _ => format!("{v:.1}"),
    }
}

/// Local-time ticks across the window.
fn x_ticks(t0: DateTime<Utc>, t1: DateTime<Utc>, days: u32) -> Vec<(DateTime<Utc>, String)> {
    let local0 = t0.with_timezone(&Local);
    let mut out = Vec::new();
    if days <= 1 {
        // Every 3 hours, on the hour.
        let mut t = local0 + Duration::hours(1);
        t = t.with_minute(0).and_then(|t| t.with_second(0)).unwrap_or(t);
        while t.with_timezone(&Utc) < t1 {
            if t.hour() % 3 == 0 {
                out.push((t.with_timezone(&Utc), t.format("%H:%M").to_string()));
            }
            t += Duration::hours(1);
        }
    } else {
        let step = if days <= 10 { 1 } else { 5 };
        let mut d = local0.date_naive() + Duration::days(1);
        let mut i = 0;
        while let Some(t) = Local
            .from_local_datetime(&d.and_hms_opt(0, 0, 0).unwrap_or_default())
            .earliest()
        {
            let tu = t.with_timezone(&Utc);
            if tu >= t1 {
                break;
            }
            if i % step == 0 {
                let label = if days <= 10 {
                    t.format("%a %-d").to_string()
                } else {
                    t.format("%-d %b").to_string()
                };
                out.push((tu, label));
            }
            d += Duration::days(1);
            i += 1;
        }
    }
    out
}

/// Draw a time chart.
pub fn chart(c: &Chart) -> String {
    let n = c.times.len();
    if n < 2 {
        return String::new();
    }
    let (t0, t1) = (c.times[0], c.times[n - 1]);
    let span = (t1 - t0).num_seconds().max(1) as f64;
    let data_max = c
        .series
        .iter()
        .flat_map(|s| s.values.iter().flatten())
        .copied()
        .fold(0.0, f64::max);
    let top = c.y_max.unwrap_or_else(|| nice_ceiling(data_max * 1.1));
    let x = |t: DateTime<Utc>| L + (W - L - R) * ((t - t0).num_seconds() as f64 / span);
    let y = |v: f64| T + (H - T - B) * (1.0 - (v / top).clamp(0.0, 1.0));
    let mut s = String::new();
    let _ = write!(s, r#"<svg class="chart" viewBox="0 0 {W} {H}" role="img">"#);
    // Grid and y labels.
    for i in 0..=4 {
        let v = top * f64::from(i) / 4.0;
        let yy = y(v);
        let _ = write!(
            s,
            r#"<line class="grid" x1="{L}" x2="{}" y1="{yy:.1}" y2="{yy:.1}"/><text class="axis" x="{}" y="{:.1}" text-anchor="end">{}</text>"#,
            W - R,
            L - 6.0,
            yy + 3.5,
            esc(&fmt(v, c.unit))
        );
    }
    for (t, label) in x_ticks(t0, t1, c.days) {
        let xx = x(t);
        let _ = write!(
            s,
            r#"<line class="grid soft" x1="{xx:.1}" x2="{xx:.1}" y1="{T}" y2="{}"/><text class="axis" x="{xx:.1}" y="{}" text-anchor="middle">{}</text>"#,
            H - B,
            H - 7.0,
            esc(&label)
        );
    }
    if let Some((v, label)) = &c.threshold {
        if *v <= top {
            let yy = y(*v);
            let _ = write!(
                s,
                r#"<line class="threshold" x1="{L}" x2="{}" y1="{yy:.1}" y2="{yy:.1}"/><text class="axis warn" x="{}" y="{:.1}" text-anchor="end">{}</text>"#,
                W - R,
                W - R - 2.0,
                yy - 4.0,
                esc(label)
            );
        }
    }
    for (k, se) in c.series.iter().enumerate() {
        // Runs of values between gaps.
        let mut runs: Vec<Vec<(f64, f64)>> = Vec::new();
        let mut cur = Vec::new();
        for (t, v) in c.times.iter().zip(&se.values) {
            match v {
                Some(v) => cur.push((x(*t), y(*v))),
                None if !cur.is_empty() => runs.push(std::mem::take(&mut cur)),
                None => {}
            }
        }
        if !cur.is_empty() {
            runs.push(cur);
        }
        let mut line = String::new();
        let mut fill = String::new();
        for run in &runs {
            for (i, (px, py)) in run.iter().enumerate() {
                let _ = write!(line, "{}{px:.1},{py:.1}", if i == 0 { "M" } else { "L" });
            }
            if se.area {
                let (fx, _) = run[0];
                let (lx, _) = run[run.len() - 1];
                let _ = write!(fill, "M{fx:.1},{:.1}", H - B);
                for (px, py) in run {
                    let _ = write!(fill, "L{px:.1},{py:.1}");
                }
                let _ = write!(fill, "L{lx:.1},{:.1}Z", H - B);
            }
        }
        if se.area && !fill.is_empty() {
            let _ = write!(
                s,
                r#"<path d="{fill}" fill="{}" class="area" style="--i:{k}"/>"#,
                se.color
            );
        }
        if !line.is_empty() {
            let _ = write!(
                s,
                r#"<path d="{line}" fill="none" stroke="{}" class="{}" vector-effect="non-scaling-stroke"/>"#,
                se.color,
                if se.faint { "line faint" } else { "line" }
            );
        }
    }
    let _ = write!(
        s,
        r#"<line class="cursor" x1="0" x2="0" y1="{T}" y2="{}" visibility="hidden"/></svg>"#,
        H - B
    );
    // Data for the hover readout.
    let data = serde_json::json!({
        "t": c.times.iter().map(|t| t.timestamp()).collect::<Vec<_>>(),
        "unit": c.unit,
        "l": L, "r": R, "w": W,
        "s": c.series.iter().filter(|s| !s.faint).map(|s| serde_json::json!({
            "n": s.name,
            "c": s.color,
            "v": s.values.iter().map(|v| v.map(|v| (v * 100.0).round() / 100.0)).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    });
    format!(
        r#"<div class="plot" data-series="{}">{s}<div class="tip" hidden></div></div>"#,
        esc(&data.to_string())
    )
}

/// A tiny line, no axes.
pub fn spark(values: &[f64], color: &str, w: f64, h: f64, top: Option<f64>) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let max = top.unwrap_or_else(|| values.iter().copied().fold(f64::MIN, f64::max));
    let min = if top.is_some() {
        0.0
    } else {
        values.iter().copied().fold(f64::MAX, f64::min)
    };
    let range = (max - min).max(1e-9);
    let n = values.len() - 1;
    let mut d = String::new();
    for (i, v) in values.iter().enumerate() {
        let px = w * i as f64 / n as f64;
        let py = 2.0 + (h - 4.0) * (1.0 - (v - min) / range);
        let _ = write!(d, "{}{px:.1},{py:.1}", if i == 0 { "M" } else { "L" });
    }
    format!(
        r#"<svg class="spark" viewBox="0 0 {w} {h}" preserveAspectRatio="none"><path d="{d}" fill="none" stroke="{color}" vector-effect="non-scaling-stroke"/></svg>"#
    )
}

/// A tiny line whose scale spans at least `min_range`, so noise stays flat.
pub fn spark_range(values: &[f64], color: &str, w: f64, h: f64, min_range: f64) -> String {
    if values.len() < 2 {
        return String::new();
    }
    let lo = values.iter().copied().fold(f64::MAX, f64::min);
    let hi = values.iter().copied().fold(f64::MIN, f64::max);
    let mid = (lo + hi) / 2.0;
    let half = ((hi - lo) / 2.0).max(min_range / 2.0);
    let top = mid + half;
    let bottom = mid - half;
    let n = values.len() - 1;
    let mut d = String::new();
    for (i, v) in values.iter().enumerate() {
        let px = w * i as f64 / n as f64;
        let py = 2.0 + (h - 4.0) * (1.0 - (v - bottom) / (top - bottom));
        let _ = write!(d, "{}{px:.1},{py:.1}", if i == 0 { "M" } else { "L" });
    }
    format!(
        r#"<svg class="spark" viewBox="0 0 {w} {h}" preserveAspectRatio="none"><path d="{d}" fill="none" stroke="{color}" vector-effect="non-scaling-stroke"/></svg>"#
    )
}

/// A ring for a share, 0–1.
pub fn ring(share: f64, color: &str, label: &str) -> String {
    let r = 26.0;
    let c = 2.0 * std::f64::consts::PI * r;
    let on = c * share.clamp(0.0, 1.0);
    format!(
        r#"<svg class="ring" viewBox="0 0 64 64"><circle cx="32" cy="32" r="{r}" class="ring-bg"/><circle cx="32" cy="32" r="{r}" fill="none" stroke="{color}" stroke-width="7" stroke-linecap="round" stroke-dasharray="{on:.1} {c:.1}" transform="rotate(-90 32 32)"/><text x="32" y="36" text-anchor="middle" class="ring-label">{}</text></svg>"#,
        esc(label)
    )
}

/// Stacked bars, one per label: `(label, [(value, color)])`.
pub fn bars(
    cols: &[(String, Vec<(f64, &'static str)>)],
    cap: Option<f64>,
    unit: &'static str,
) -> String {
    if cols.is_empty() {
        return String::new();
    }
    let data_max = cols
        .iter()
        .map(|(_, v)| v.iter().map(|x| x.0).sum::<f64>())
        .fold(0.0, f64::max);
    let top = nice_ceiling(data_max.max(cap.unwrap_or(0.0)).max(0.01) * 1.1);
    let h = 160.0;
    let y = |v: f64| T + (h - T - B) * (1.0 - v / top);
    let slot = (W - L - R) / cols.len() as f64;
    let bw = (slot * 0.62).min(46.0);
    let mut s = format!(r#"<svg class="chart bars" viewBox="0 0 {W} {h}">"#);
    for i in 0..=3 {
        let v = top * f64::from(i) / 3.0;
        let _ = write!(
            s,
            r#"<line class="grid" x1="{L}" x2="{}" y1="{:.1}" y2="{:.1}"/><text class="axis" x="{}" y="{:.1}" text-anchor="end">{}</text>"#,
            W - R,
            y(v),
            y(v),
            L - 6.0,
            y(v) + 3.5,
            esc(&fmt(v, unit))
        );
    }
    if let Some(cap) = cap.filter(|c| *c > 0.0) {
        let _ = write!(
            s,
            r#"<line class="threshold" x1="{L}" x2="{}" y1="{:.1}" y2="{:.1}"/><text class="axis warn" x="{}" y="{:.1}" text-anchor="end">daily cap</text>"#,
            W - R,
            y(cap),
            y(cap),
            W - R - 2.0,
            y(cap) - 4.0
        );
    }
    let every = cols.len().div_ceil(10);
    for (i, (label, parts)) in cols.iter().enumerate() {
        let cx = L + slot * (i as f64 + 0.5);
        let mut base = 0.0;
        let total: f64 = parts.iter().map(|p| p.0).sum();
        for (v, color) in parts {
            if *v <= 0.0 {
                continue;
            }
            let _ = write!(
                s,
                r#"<rect x="{:.1}" y="{:.1}" width="{bw:.1}" height="{:.1}" rx="2" fill="{color}"><title>{} {}</title></rect>"#,
                cx - bw / 2.0,
                y(base + v),
                (y(base) - y(base + v)).max(0.5),
                esc(label),
                esc(&fmt(*v, unit))
            );
            base += v;
        }
        if i % every == 0 {
            let _ = write!(
                s,
                r#"<text class="axis" x="{cx:.1}" y="{}" text-anchor="middle">{}</text>"#,
                h - 7.0,
                esc(label)
            );
        }
        if total > 0.0 && cols.len() <= 10 {
            let _ = write!(
                s,
                r#"<text class="axis strong" x="{cx:.1}" y="{:.1}" text-anchor="middle">{}</text>"#,
                y(total) - 4.0,
                esc(&fmt(total, unit))
            );
        }
    }
    s.push_str("</svg>");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_are_round() {
        assert_eq!(nice_ceiling(7.3), 10.0);
        assert_eq!(nice_ceiling(0.21), 0.25);
        assert_eq!(nice_ceiling(130.0), 200.0);
    }

    #[test]
    fn gaps_break_the_line() {
        let now = Utc::now();
        let times: Vec<DateTime<Utc>> = (0..6).map(|i| now + Duration::minutes(i)).collect();
        let svg = chart(&Chart {
            times: &times,
            series: vec![Series {
                name: "CPU".into(),
                color: "var(--teal)",
                values: vec![Some(1.0), Some(2.0), None, None, Some(3.0), Some(4.0)],
                area: true,
                faint: false,
            }],
            y_max: Some(100.0),
            unit: "%",
            threshold: Some((90.0, "warn".into())),
            days: 1,
        });
        let line = svg.split("class=\"line\"").next().unwrap();
        let d = line.rsplit("d=\"").next().unwrap();
        assert_eq!(d.matches('M').count(), 2, "{d}");
        assert!(svg.contains("data-series") && svg.contains("threshold"));
        assert!(!svg.contains("<script"), "text is escaped");
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            esc("<a href='x'>&</a>"),
            "&lt;a href=&#39;x&#39;&gt;&amp;&lt;/a&gt;"
        );
    }
}
