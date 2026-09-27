//! What "normal" looks like on this machine, learned from minutes of
//! readings: exponentially weighted means and variances for the vital
//! signs, hourly disk-use points for growth rates, and each unit's usual
//! journal noise. Kept as JSON in `~/.reeve/observer/baselines.json`, and
//! summarized hourly into readable notes in `memory/baselines/`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use reeve_core::memory::{Layer, Memory, Note};
use serde::{Deserialize, Serialize};

/// A running mean and variance that forgets slowly.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct Ewma {
    /// Mean.
    pub mean: f64,
    /// Variance.
    pub var: f64,
    /// Samples seen.
    pub n: u64,
}

impl Ewma {
    /// Fold in `x`; `window` is roughly how many samples it remembers.
    pub fn add(&mut self, x: f64, window: f64) {
        self.n += 1;
        // Plain average until the window fills, so early values aren't overweighted.
        let alpha = (1.0 / self.n as f64).max(1.0 / window);
        let d = x - self.mean;
        self.mean += alpha * d;
        self.var = (1.0 - alpha) * (self.var + alpha * d * d);
    }

    /// Standard deviation.
    pub fn sd(&self) -> f64 {
        self.var.max(0.0).sqrt()
    }
}

/// Everything learned.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Baselines {
    /// Last update.
    pub updated: Option<DateTime<Utc>>,
    /// `cpu`, `mem`, `swap`, `load`, `temp`, per minute.
    pub metrics: BTreeMap<String, Ewma>,
    /// Per mount: (unix seconds, bytes used), hourly, two weeks.
    pub disks: BTreeMap<String, Vec<(i64, u64)>>,
    /// Per unit: journal warnings per hour.
    pub journal: BTreeMap<String, Ewma>,
    /// Minutes observed.
    pub minutes: u64,
}

/// Remember about a week of minutes.
const METRIC_WINDOW: f64 = 60.0 * 24.0 * 7.0;
/// And about two weeks of hours for journal noise.
const JOURNAL_WINDOW: f64 = 24.0 * 14.0;

impl Baselines {
    fn path(home: &Path) -> PathBuf {
        home.join("observer").join("baselines.json")
    }

    /// Load, or start empty.
    pub fn load(home: &Path) -> Self {
        fs::read_to_string(Self::path(home))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    /// Save.
    pub fn save(&self, home: &Path) -> std::io::Result<()> {
        let p = Self::path(home);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
        fs::write(p, serde_json::to_string(self).unwrap_or_default())
    }

    /// Fold in one minute of readings.
    pub fn minute(&mut self, readings: &[(&str, f64)], now: DateTime<Utc>) {
        for (k, v) in readings {
            self.metrics
                .entry((*k).to_string())
                .or_default()
                .add(*v, METRIC_WINDOW);
        }
        self.minutes += 1;
        self.updated = Some(now);
    }

    /// Fold in the hour's disk use and journal counts.
    pub fn hour(
        &mut self,
        disks: &[(String, u64)],
        journal: &BTreeMap<String, u64>,
        now: DateTime<Utc>,
    ) {
        let cutoff = now.timestamp() - 14 * 86_400;
        for (mount, used) in disks {
            let pts = self.disks.entry(mount.clone()).or_default();
            pts.push((now.timestamp(), *used));
            pts.retain(|(t, _)| *t >= cutoff);
        }
        // Units that were quiet this hour count as zero, so noise fades.
        let units: Vec<String> = self
            .journal
            .keys()
            .cloned()
            .chain(journal.keys().cloned())
            .collect();
        for u in units {
            let n = journal.get(&u).copied().unwrap_or(0) as f64;
            self.journal.entry(u).or_default().add(n, JOURNAL_WINDOW);
        }
        self.journal.retain(|_, e| e.mean > 0.05 || e.n < 48);
    }

    /// Bytes per day a mount grows, from a least-squares fit over the last
    /// three days. `None` until there are six hours of points.
    pub fn growth(&self, mount: &str) -> Option<f64> {
        let pts = self.disks.get(mount)?;
        let latest = pts.last()?.0;
        let recent: Vec<&(i64, u64)> = pts
            .iter()
            .filter(|(t, _)| latest - t <= 3 * 86_400)
            .collect();
        if recent.len() < 6 || latest - recent[0].0 < 6 * 3600 {
            return None;
        }
        let n = recent.len() as f64;
        let mx = recent.iter().map(|(t, _)| *t as f64).sum::<f64>() / n;
        let my = recent.iter().map(|(_, u)| *u as f64).sum::<f64>() / n;
        let (mut sxy, mut sxx) = (0.0, 0.0);
        for (t, u) in &recent {
            let dx = *t as f64 - mx;
            sxy += dx * (*u as f64 - my);
            sxx += dx * dx;
        }
        (sxx > 0.0).then(|| sxy / sxx * 86_400.0)
    }

    /// A unit's usual warnings per hour.
    pub fn journal_rate(&self, unit: &str) -> f64 {
        self.journal.get(unit).map_or(0.0, |e| e.mean)
    }

    /// Readable summaries in `memory/baselines/`, so the owner (and the
    /// model, through memory_search) can see what "normal" is.
    pub fn write_notes(&self, mem: &Memory, os: &str) {
        if self.minutes < 60 {
            return;
        }
        let hours = self.minutes / 60;
        let mut res = format!("Learned from {hours} hours of readings.\n\n");
        for (k, e) in &self.metrics {
            let (unit, scale) = match k.as_str() {
                "cpu" => ("% CPU", 1.0),
                "mem" | "swap" => ("% used", 100.0),
                "temp" => ("°C", 1.0),
                _ => ("", 1.0),
            };
            res.push_str(&format!(
                "- {k}: usually {:.1}{unit} (±{:.1})\n",
                e.mean * scale,
                e.sd() * scale
            ));
        }
        let mut disks = String::from("Growth fitted over the last 3 days of hourly readings.\n\n");
        for m in self.disks.keys() {
            match self.growth(m) {
                Some(g) => disks.push_str(&format!("- {m}: {}/day\n", signed_bytes(g))),
                None => disks.push_str(&format!("- {m}: not enough history yet\n")),
            }
        }
        let mut noisy: Vec<(&String, &Ewma)> =
            self.journal.iter().filter(|(_, e)| e.mean >= 0.1).collect();
        noisy.sort_by(|a, b| b.1.mean.total_cmp(&a.1.mean));
        let mut journal =
            String::from("Warnings and errors per hour that are normal for each unit.\n\n");
        for (u, e) in noisy.iter().take(20) {
            journal.push_str(&format!("- {u}: {:.1}/hour\n", e.mean));
        }
        if noisy.is_empty() {
            journal.push_str("- the journal is quiet\n");
        }
        for (id, title, body, tags) in [
            (
                "baseline-resources",
                "Normal resource use",
                res,
                vec!["cpu", "memory", "swap", "temperature", "load"],
            ),
            (
                "baseline-disks",
                "Disk growth",
                disks,
                vec!["disk", "storage", "growth"],
            ),
            (
                "baseline-journal",
                "Normal journal noise",
                journal,
                vec!["journal", "logs", "noise"],
            ),
        ] {
            let mut n = Note::new(Layer::Baselines, title, &body, "observer");
            n.id = id.into();
            n.tags = tags.into_iter().map(String::from).collect();
            n.os = Some(os.to_string());
            n.confidence = (self.minutes as f32 / (60.0 * 24.0 * 3.0)).min(0.95);
            let _ = mem.put(&mut n, true);
        }
    }
}

/// `+1.2G`, `−300M`.
pub fn signed_bytes(b: f64) -> String {
    let sign = if b < 0.0 { "−" } else { "+" };
    let a = b.abs();
    let s = if a >= 0.9995e9 {
        format!("{:.1}G", a / 1e9)
    } else if a >= 1e6 {
        format!("{:.0}M", a / 1e6)
    } else {
        format!("{:.0}K", a / 1e3)
    };
    format!("{sign}{s}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ewma_tracks_mean_and_spread() {
        let mut e = Ewma::default();
        for i in 0..1000 {
            e.add(if i % 2 == 0 { 10.0 } else { 20.0 }, 100.0);
        }
        assert!((e.mean - 15.0).abs() < 1.0, "{e:?}");
        assert!((e.sd() - 5.0).abs() < 1.0, "{e:?}");
    }

    #[test]
    fn growth_is_a_fitted_rate() {
        let mut b = Baselines::default();
        let t0 = Utc::now() - chrono::Duration::hours(12);
        for h in 0..12 {
            let t = t0 + chrono::Duration::hours(h);
            // 1 GB per day.
            b.hour(
                &[(
                    "/var".into(),
                    50_000_000_000 + (h as u64) * 1_000_000_000 / 24,
                )],
                &BTreeMap::new(),
                t,
            );
        }
        let g = b.growth("/var").unwrap();
        assert!((g - 1e9).abs() / 1e9 < 0.01, "{g}");
        assert!(b.growth("/nope").is_none());
        assert_eq!(signed_bytes(g), "+1.0G");
    }

    #[test]
    fn quiet_hours_bring_noise_down() {
        let mut b = Baselines::default();
        let mut counts = BTreeMap::new();
        counts.insert("bluetooth.service".to_string(), 30u64);
        b.hour(&[], &counts, Utc::now());
        for _ in 0..50 {
            b.hour(&[], &BTreeMap::new(), Utc::now());
        }
        assert!(b.journal_rate("bluetooth.service") < 2.0);
    }
}
