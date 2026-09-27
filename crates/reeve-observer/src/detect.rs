//! Detectors: plain rules over readings, baselines, and the journal. No
//! model is involved. Each returns [`Signal`]s with stable ids, and the
//! store turns them into findings.

use std::collections::BTreeMap;

use reeve_core::findings::{Severity, Signal};

use crate::Disk;
use crate::baselines::{Baselines, signed_bytes};

/// Kinds whose findings resolve as soon as a tick doesn't report them.
pub const STATE_KINDS: &[&str] = &[
    "disk-full",
    "disk-trend",
    "swap-full",
    "mem-pressure",
    "temp-high",
    "load-high",
    "unit-failed",
];

fn sig(
    id: String,
    severity: Severity,
    title: String,
    detail: String,
    evidence: Vec<String>,
) -> Signal {
    Signal {
        id,
        severity,
        title,
        detail,
        evidence,
        count: 1,
    }
}

fn gib(b: u64) -> String {
    let g = b as f64 / (1u64 << 30) as f64;
    if g >= 1.0 {
        format!("{g:.1}G")
    } else {
        format!("{:.0}M", b as f64 / (1u64 << 20) as f64)
    }
}

/// Full disks, and disks filling fast.
pub fn disks(disks: &[Disk], b: &Baselines, warn: f64) -> Vec<Signal> {
    let mut out = Vec::new();
    for d in disks {
        let r = d.ratio();
        if r >= warn {
            let sev = if r >= (warn + 0.07).min(0.99) {
                Severity::Critical
            } else {
                Severity::Warning
            };
            out.push(sig(
                format!("disk-full:{}", d.mount),
                sev,
                format!("{} is {:.0}% full", d.mount, r * 100.0),
                format!("{} free of {} ({}).", gib(d.avail), gib(d.total), d.fs),
                vec![],
            ));
        }
        if let Some(g) = b.growth(&d.mount) {
            // Only real growth, and only when "full" is days away, not months.
            if g > 50e6 && d.avail > 0 {
                let days = d.avail as f64 / g;
                if days < 3.0 {
                    out.push(sig(
                        format!("disk-trend:{}", d.mount),
                        if days < 1.0 {
                            Severity::Critical
                        } else {
                            Severity::Warning
                        },
                        format!(
                            "{} fills in about {:.0} hours at this rate",
                            d.mount,
                            days * 24.0
                        ),
                        format!(
                            "Growing {} per day (fitted over the last 3 days); {} left.",
                            signed_bytes(g),
                            gib(d.avail)
                        ),
                        vec![],
                    ));
                }
            }
        }
    }
    out
}

/// Swap nearly full: usually a leak, or zram sized too small.
pub fn swap(total: u64, used: u64, mem_ratio: f64) -> Option<Signal> {
    if total == 0 {
        return None;
    }
    let r = used as f64 / total as f64;
    (r >= 0.9).then(|| {
        let sev = if r >= 0.98 && mem_ratio >= 0.9 { Severity::Critical } else { Severity::Warning };
        sig(
            "swap-full".into(),
            sev,
            format!("Swap is {:.0}% full", r * 100.0),
            format!("{} of {} swap used, with memory at {:.0}%. Something may be leaking, or swap is too small for this workload.", gib(used), gib(total), mem_ratio * 100.0),
            vec![],
        )
    })
}

/// Memory above 95% for every sample of the last five minutes.
pub fn memory(recent: &[f64]) -> Option<Signal> {
    (recent.len() >= 60 && recent.iter().all(|r| *r >= 0.95)).then(|| {
        sig(
            "mem-pressure".into(),
            Severity::Warning,
            "Memory has been over 95% for 5 minutes".into(),
            format!(
                "Now at {:.0}%. Expect swapping, slowness, or the OOM killer.",
                recent.last().copied().unwrap_or(0.0) * 100.0
            ),
            vec![],
        )
    })
}

/// CPU hot for three minutes straight.
pub fn temp(recent: &[f32], warn: f32) -> Option<Signal> {
    (recent.len() >= 36 && recent.iter().all(|t| *t >= warn)).then(|| {
        let now = recent.last().copied().unwrap_or(0.0);
        sig(
            "temp-high".into(),
            if now >= warn + 8.0 {
                Severity::Critical
            } else {
                Severity::Warning
            },
            format!("CPU at {now:.0}°C for 3 minutes"),
            "Sustained heat: check fans, dust, or a runaway process.".into(),
            vec![],
        )
    })
}

/// Load above twice the CPU count for ten minutes.
pub fn load(recent: &[f32], cpus: usize) -> Option<Signal> {
    let limit = (cpus.max(1) * 2) as f32;
    (recent.len() >= 120 && recent.iter().all(|l| *l > limit)).then(|| {
        sig(
            "load-high".into(),
            Severity::Info,
            format!("Load has been over {limit:.0} for 10 minutes"),
            format!(
                "Load {:.1} on {cpus} CPUs.",
                recent.last().copied().unwrap_or(0.0)
            ),
            vec![],
        )
    })
}

/// `drkonqi-coredump-processor@97-49159-….service` → `drkonqi-coredump-processor@.service`:
/// one finding per template, not one per crash.
pub fn unit_family(unit: &str) -> String {
    let (name, user) = match unit.strip_suffix(" (user)") {
        Some(n) => (n, " (user)"),
        None => (unit, ""),
    };
    match name.split_once('@') {
        Some((base, rest)) => {
            let suffix = rest.rsplit_once('.').map(|(_, t)| t).unwrap_or("service");
            format!("{base}@.{suffix}{user}")
        }
        None => format!("{name}{user}"),
    }
}

/// Failed units, grouped by template.
pub fn failed_units(units: &[String]) -> Vec<Signal> {
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for u in units {
        groups.entry(unit_family(u)).or_default().push(u.clone());
    }
    groups
        .into_iter()
        .map(|(family, members)| {
            let title = if members.len() == 1 {
                format!("{} failed", members[0])
            } else {
                format!("{} {family} units failed", members.len())
            };
            let detail = if family.starts_with("drkonqi-coredump") {
                "These are KDE's crash-report processors: each one is an application crash that was recorded. The units themselves are harmless to reset; the crashes may be worth a look.".to_string()
            } else {
                format!("systemd reports {} as failed. `svc_status` and `logs_query` show why.", if members.len() == 1 { "it" } else { "them" })
            };
            let mut s = sig(format!("unit-failed:{family}"), Severity::Warning, title, detail, members.clone());
            s.count = members.len() as u64;
            s
        })
        .collect()
}

/// A unit logging far more warnings than it normally does.
pub fn journal_spike(
    unit: &str,
    last_10min: u64,
    per_hour_normal: f64,
    examples: &[String],
) -> Option<Signal> {
    let normal_10 = per_hour_normal / 6.0;
    let threshold = (normal_10 * 10.0).max(30.0);
    (last_10min as f64 >= threshold).then(|| {
        let mut s = sig(
            format!("journal-spike:{unit}"),
            Severity::Warning,
            format!("{unit} logged {last_10min} warnings in 10 minutes"),
            if per_hour_normal > 0.05 {
                format!("Normally about {per_hour_normal:.1} an hour.")
            } else {
                "Normally quiet.".into()
            },
            examples.to_vec(),
        );
        s.count = last_10min;
        s
    })
}

/// A critical, alert, or emergency message.
pub fn journal_critical(unit: &str, template: &str, message: &str) -> Signal {
    let hash = &reeve_core::undo::sha256_hex(template.as_bytes())[..8];
    sig(
        format!("journal-critical:{unit}:{hash}"),
        Severity::Critical,
        format!("{unit}: {}", message.chars().take(90).collect::<String>()),
        "Logged at critical priority or above.".into(),
        vec![message.chars().take(400).collect()],
    )
}

/// The running kernel is older than the newest installed one.
pub fn reboot_pending(running: &str, newest: &str) -> Option<Signal> {
    (!newest.is_empty() && !running.is_empty() && running != newest).then(|| {
        sig(
            "reboot-pending".into(),
            Severity::Info,
            "A newer kernel is installed: reboot to use it".into(),
            format!("Running {running}; newest installed is {newest}."),
            vec![],
        )
    })
}

/// Security updates waiting.
pub fn security_updates(n: usize, sample: &[String]) -> Option<Signal> {
    (n > 0).then(|| {
        sig(
            "updates-security".into(),
            Severity::Info,
            format!("{n} security update{} available", if n == 1 { "" } else { "s" }),
            "pkg_list updates shows them; pkg_upgrade installs them (with a snapshot, if snapper is set up).".into(),
            sample.iter().take(10).cloned().collect(),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(mount: &str, used_pct: u64) -> Disk {
        Disk {
            mount: mount.into(),
            fs: "ext4".into(),
            total: 100 << 30,
            used: used_pct << 30,
            avail: (100 - used_pct) << 30,
        }
    }

    #[test]
    fn disk_thresholds() {
        let b = Baselines::default();
        let s = disks(
            &[disk("/boot", 94), disk("/", 50), disk("/home", 98)],
            &b,
            0.90,
        );
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].severity, Severity::Warning);
        assert_eq!(s[1].severity, Severity::Critical);
    }

    #[test]
    fn swap_full_on_this_kind_of_machine() {
        let s = swap(8 << 30, 8 << 30, 0.26).unwrap();
        assert_eq!(
            (s.id.as_str(), s.severity),
            ("swap-full", Severity::Warning)
        );
        assert!(swap(8 << 30, 1 << 30, 0.5).is_none());
        assert!(swap(0, 0, 0.5).is_none());
    }

    #[test]
    fn crash_units_group_into_one_finding() {
        let units = vec![
            "drkonqi-coredump-processor@14-28-1.service".to_string(),
            "drkonqi-coredump-processor@97-49159-2.service".to_string(),
            "bluetooth.service".to_string(),
            "pipewire.service (user)".to_string(),
        ];
        let s = failed_units(&units);
        assert_eq!(s.len(), 3);
        let crash = s.iter().find(|x| x.id.contains("drkonqi")).unwrap();
        assert_eq!(
            (crash.id.as_str(), crash.count),
            ("unit-failed:drkonqi-coredump-processor@.service", 2)
        );
        assert!(
            s.iter()
                .any(|x| x.id == "unit-failed:pipewire.service (user)")
        );
    }

    #[test]
    fn sustained_means_every_sample() {
        let mut hot = vec![95.0f32; 36];
        assert!(temp(&hot, 90.0).is_some());
        hot[10] = 60.0;
        assert!(temp(&hot, 90.0).is_none());
        assert!(memory(&[0.99; 30]).is_none(), "not five minutes yet");
    }

    #[test]
    fn spikes_are_relative_to_normal() {
        assert!(journal_spike("x", 35, 0.0, &[]).is_some());
        assert!(
            journal_spike("chatty", 35, 60.0, &[]).is_none(),
            "60/hour is its normal"
        );
        assert!(journal_spike("chatty", 120, 60.0, &[]).is_some());
    }

    #[test]
    fn reboot_and_updates() {
        assert!(reboot_pending("6.17.4-200.fc44.x86_64", "6.17.5-200.fc44.x86_64").is_some());
        assert!(reboot_pending("a", "a").is_none());
        assert!(security_updates(0, &[]).is_none());
    }
}
