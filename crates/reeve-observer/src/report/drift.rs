//! What changed on the machine: packages, enabled units, files in `/etc`,
//! and kernels.
//!
//! Some of it needs no history. rpm records when each package was
//! installed, pacman logs every transaction, and file times show `/etc`
//! edits. What those can't show (a package removed on Fedora, a unit
//! enabled or disabled) comes from comparing daily snapshots, which reeved
//! takes in `~/.reeve/observer/state/`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

use chrono::{DateTime, Local, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

use reeve_core::distro::Distro;
use reeve_core::policy::Tier;
use reeve_core::receipts::Receipt;

/// One package change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pkg {
    /// Name (with `.arch` on Fedora).
    pub name: String,
    /// Version before, when known.
    pub from: Option<String>,
    /// Version after.
    pub to: Option<String>,
    /// When, when known.
    pub at: Option<DateTime<Utc>>,
}

/// A file in `/etc` modified in the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EtcChange {
    /// Path.
    pub path: String,
    /// Modified.
    pub at: DateTime<Utc>,
    /// The receipt, when Reeve made the change.
    pub by_reeve: Option<u64>,
}

/// What changed in the window.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Drift {
    /// The snapshot compared against, if there is one.
    pub baseline: Option<DateTime<Utc>>,
    /// New packages.
    pub installed: Vec<Pkg>,
    /// New versions.
    pub upgraded: Vec<Pkg>,
    /// Gone.
    pub removed: Vec<Pkg>,
    /// Installed or upgraded: without a snapshot, rpm can't say which.
    pub touched: Vec<Pkg>,
    /// Kernel packages among the changes.
    pub kernels: Vec<String>,
    /// A newer kernel is installed than the one running.
    pub reboot_for: Option<String>,
    /// `/etc` files changed, newest first (at most 200).
    pub etc: Vec<EtcChange>,
    /// All `/etc` files changed.
    pub etc_total: usize,
    /// Units enabled since the snapshot.
    pub units_enabled: Vec<String>,
    /// Units disabled since the snapshot.
    pub units_disabled: Vec<String>,
    /// Where the package changes come from.
    pub source: String,
}

/// A day's snapshot.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct State {
    /// When.
    pub ts: DateTime<Utc>,
    /// Package → version (installonly packages like kernels: versions joined by `,`).
    pub packages: BTreeMap<String, String>,
    /// Enabled system units.
    pub units: BTreeSet<String>,
    /// Enabled user units.
    pub user_units: BTreeSet<String>,
}

fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .env("LC_ALL", "C")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Fedora: name.arch → (version, install time), from rpm.
fn rpm_packages() -> Vec<(String, String, Option<DateTime<Utc>>)> {
    run(
        "rpm",
        &[
            "-qa",
            "--qf",
            "%{NAME}.%{ARCH}\\t%{VERSION}-%{RELEASE}\\t%{INSTALLTIME}\\n",
        ],
    )
    .unwrap_or_default()
    .lines()
    .filter_map(|l| {
        let mut f = l.split('\t');
        let name = f.next()?;
        if name.starts_with("gpg-pubkey") {
            return None;
        }
        let ver = f.next()?;
        let at = f
            .next()
            .and_then(|t| t.trim().parse::<i64>().ok())
            .and_then(|t| DateTime::from_timestamp(t, 0));
        Some((name.to_string(), ver.to_string(), at))
    })
    .collect()
}

fn units(user: bool) -> BTreeSet<String> {
    let mut args = vec![
        "list-unit-files",
        "--state=enabled",
        "--no-legend",
        "--plain",
        "--no-pager",
    ];
    if user {
        args.insert(0, "--user");
    }
    run("systemctl", &args)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| l.split_whitespace().next().map(String::from))
        .collect()
}

/// Take a snapshot now.
pub fn take(distro: &Distro) -> State {
    let mut packages: BTreeMap<String, String> = BTreeMap::new();
    let mut add = |name: String, ver: String| {
        packages
            .entry(name)
            .and_modify(|v| {
                let mut all: Vec<&str> = v.split(',').chain([ver.as_str()]).collect();
                all.sort_unstable();
                all.dedup();
                *v = all.join(",");
            })
            .or_insert(ver);
    };
    match distro {
        Distro::Fedora { .. } => {
            for (n, v, _) in rpm_packages() {
                add(n, v);
            }
        }
        Distro::Arch { .. } => {
            for l in run("pacman", &["-Q"]).unwrap_or_default().lines() {
                if let Some((n, v)) = l.split_once(' ') {
                    add(n.to_string(), v.to_string());
                }
            }
        }
        Distro::Other(_) => {}
    }
    State {
        ts: Utc::now(),
        packages,
        units: units(false),
        user_units: units(true),
    }
}

fn state_dir(home: &Path) -> std::path::PathBuf {
    home.join("observer").join("state")
}

/// reeved, once a day: keep today's snapshot (and 60 days of them).
pub fn save_daily(home: &Path, distro: &Distro) {
    let dir = state_dir(home);
    let today = Local::now().date_naive();
    let path = dir.join(format!("{today}.json"));
    if path.exists() {
        return;
    }
    let _ = fs::create_dir_all(&dir);
    if let Ok(json) = serde_json::to_string(&take(distro)) {
        let _ = fs::write(&path, json);
    }
    let cutoff = today - chrono::Duration::days(60);
    for (day, p) in snapshots(home) {
        if day < cutoff {
            let _ = fs::remove_file(p);
        }
    }
}

fn snapshots(home: &Path) -> Vec<(NaiveDate, std::path::PathBuf)> {
    let mut v: Vec<(NaiveDate, std::path::PathBuf)> = fs::read_dir(state_dir(home))
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| {
                    let name = e.file_name().to_string_lossy().into_owned();
                    let day =
                        NaiveDate::parse_from_str(name.strip_suffix(".json")?, "%Y-%m-%d").ok()?;
                    Some((day, e.path()))
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// The snapshot to compare against: the last one from before the window,
/// or else the oldest one from before today.
fn baseline(home: &Path, since: DateTime<Utc>) -> Option<State> {
    let all = snapshots(home);
    let start = since.with_timezone(&Local).date_naive();
    let today = Local::now().date_naive();
    let pick = all
        .iter()
        .rev()
        .find(|(d, _)| *d <= start)
        .or_else(|| all.iter().find(|(d, _)| *d < today))?;
    serde_json::from_str(&fs::read_to_string(&pick.1).ok()?).ok()
}

fn is_kernel(name: &str) -> bool {
    let base = name.split('.').next().unwrap_or(name);
    matches!(
        base,
        "kernel" | "kernel-core" | "linux" | "linux-lts" | "linux-zen" | "linux-hardened"
    )
}

/// One line of pacman's log.
struct PacEvent {
    at: DateTime<Utc>,
    installed: bool,
    name: String,
    from: Option<String>,
    to: Option<String>,
}

/// pacman's log: `[2026-09-27T10:00:00-0400] [ALPM] upgraded foo (1.0-1 -> 1.1-1)`.
fn pacman_events(text: &str, since: DateTime<Utc>) -> Vec<PacEvent> {
    let mut out = Vec::new();
    for l in text.lines() {
        let Some(rest) = l.strip_prefix('[') else {
            continue;
        };
        let Some((ts, rest)) = rest.split_once("] [ALPM] ") else {
            continue;
        };
        let Ok(at) = DateTime::parse_from_str(ts, "%Y-%m-%dT%H:%M:%S%z") else {
            continue;
        };
        let at = at.with_timezone(&Utc);
        if at < since {
            continue;
        }
        let Some((verb, rest)) = rest.split_once(' ') else {
            continue;
        };
        let Some((name, vers)) = rest.split_once(" (") else {
            continue;
        };
        let vers = vers.trim_end_matches(')');
        let (from, to) = match verb {
            "installed" => (None, Some(vers.to_string())),
            "removed" => (Some(vers.to_string()), None),
            "upgraded" | "downgraded" => match vers.split_once(" -> ") {
                Some((a, b)) => (Some(a.to_string()), Some(b.to_string())),
                None => continue,
            },
            _ => continue,
        };
        out.push(PacEvent {
            at,
            installed: verb == "installed",
            name: name.to_string(),
            from,
            to,
        });
    }
    out
}

/// Each package's first and last event in the window, netted out:
/// installed, upgraded, removed (a package that came and went is neither).
fn net_changes(events: Vec<PacEvent>) -> (Vec<Pkg>, Vec<Pkg>, Vec<Pkg>) {
    // Per package: the change so far, and whether it was new in the window.
    let mut net: BTreeMap<String, (Pkg, bool)> = BTreeMap::new();
    for e in events {
        let entry = net.entry(e.name.clone()).or_insert((
            Pkg {
                name: e.name,
                from: e.from,
                to: None,
                at: None,
            },
            e.installed,
        ));
        entry.0.to = e.to;
        entry.0.at = Some(e.at);
    }
    let (mut installed, mut upgraded, mut removed) = (Vec::new(), Vec::new(), Vec::new());
    for (p, fresh) in net.into_values() {
        match (fresh, p.to.is_some()) {
            (true, true) => installed.push(Pkg { from: None, ..p }),
            (true, false) => {}
            (false, false) => removed.push(p),
            (false, true) if p.from != p.to => upgraded.push(p),
            _ => {}
        }
    }
    (installed, upgraded, removed)
}

/// What changed since `since`.
pub fn gather(home: &Path, distro: &Distro, since: DateTime<Utc>, receipts: &[Receipt]) -> Drift {
    let base = baseline(home, since);
    let now = take(distro);
    let mut d = Drift {
        baseline: base.as_ref().map(|b| b.ts),
        ..Default::default()
    };
    match distro {
        Distro::Fedora { .. } => {
            d.source = "rpm install times".into();
            let pkgs = rpm_packages();
            for (name, ver, at) in &pkgs {
                if at.is_none_or(|t| t < since) {
                    continue;
                }
                let p = Pkg {
                    name: name.clone(),
                    from: None,
                    to: Some(ver.clone()),
                    at: *at,
                };
                match base.as_ref().map(|b| b.packages.get(name)) {
                    Some(Some(old)) if old.split(',').any(|v| v == ver) => {}
                    Some(Some(old)) => d.upgraded.push(Pkg {
                        from: Some(old.clone()),
                        ..p
                    }),
                    Some(None) => d.installed.push(p),
                    None => d.touched.push(p),
                }
            }
            // The newest installed kernel, against the one running.
            let newest = pkgs
                .iter()
                .filter(|(n, _, _)| n.starts_with("kernel-core."))
                .max_by_key(|(_, _, at)| *at);
            if let Some((name, ver, _)) = newest {
                let arch = name.rsplit('.').next().unwrap_or("");
                let release = format!("{ver}.{arch}");
                let running = run("uname", &["-r"]).unwrap_or_default();
                if !running.trim().is_empty() && running.trim() != release {
                    d.reboot_for = Some(ver.clone());
                }
            }
        }
        Distro::Arch { .. } => {
            d.source = "pacman's log".into();
            let log = fs::read_to_string("/var/log/pacman.log").unwrap_or_default();
            let (installed, upgraded, removed) = net_changes(pacman_events(&log, since));
            d.installed = installed;
            d.upgraded = upgraded;
            d.removed = removed;
            // An upgraded kernel removes the running one's modules.
            let running = run("uname", &["-r"]).unwrap_or_default();
            let running = running.trim();
            if !running.is_empty() && !Path::new("/usr/lib/modules").join(running).exists() {
                d.reboot_for = now
                    .packages
                    .get("linux")
                    .cloned()
                    .or(Some("a newer kernel".into()));
            }
        }
        Distro::Other(_) => d.source = "daily snapshots".into(),
    }
    if let Some(b) = &base {
        // Removals only a snapshot can show (Fedora), and everything on
        // distros Reeve doesn't read logs for.
        if !matches!(distro, Distro::Arch { .. }) {
            for (name, ver) in &b.packages {
                if !now.packages.contains_key(name) {
                    d.removed.push(Pkg {
                        name: name.clone(),
                        from: Some(ver.clone()),
                        to: None,
                        at: None,
                    });
                }
            }
        }
        let tag = |u: &String, user: bool| {
            if user {
                format!("{u} (user)")
            } else {
                u.clone()
            }
        };
        for (old, new, user) in [
            (&b.units, &now.units, false),
            (&b.user_units, &now.user_units, true),
        ] {
            d.units_enabled
                .extend(new.difference(old).map(|u| tag(u, user)));
            d.units_disabled
                .extend(old.difference(new).map(|u| tag(u, user)));
        }
    }
    for list in [
        &mut d.installed,
        &mut d.upgraded,
        &mut d.removed,
        &mut d.touched,
    ] {
        list.sort_by(|a, b| b.at.cmp(&a.at).then(a.name.cmp(&b.name)));
    }
    d.kernels = d
        .installed
        .iter()
        .chain(&d.upgraded)
        .chain(&d.touched)
        .filter(|p| is_kernel(&p.name))
        .filter_map(|p| p.to.clone())
        .collect();
    d.kernels.sort();
    d.kernels.dedup();
    let (etc, total) = etc_changes(Path::new("/etc"), since, receipts);
    d.etc = etc;
    d.etc_total = total;
    d
}

/// Files under `root` modified since `since`, newest first, marking the
/// ones a receipt shows Reeve changed.
fn etc_changes(root: &Path, since: DateTime<Utc>, receipts: &[Receipt]) -> (Vec<EtcChange>, usize) {
    let mine: BTreeMap<String, u64> = receipts
        .iter()
        .filter(|r| r.tier >= Tier::T1 && r.undoes.is_none())
        .flat_map(|r| {
            ["path", "from", "to"]
                .into_iter()
                .filter_map(|k| {
                    r.args
                        .get(k)
                        .and_then(|v| v.as_str())
                        .map(|p| (p.to_string(), r.seq))
                })
                .collect::<Vec<_>>()
        })
        .collect();
    let mut all: Vec<EtcChange> = walkdir::WalkDir::new(root)
        .follow_links(false)
        .max_depth(8)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
        .filter_map(|e| {
            let at: DateTime<Utc> = e.metadata().ok()?.modified().ok()?.into();
            (at >= since).then(|| {
                let path = e.path().to_string_lossy().into_owned();
                EtcChange {
                    by_reeve: mine.get(&path).copied(),
                    path,
                    at,
                }
            })
        })
        .collect();
    all.sort_by(|a, b| b.at.cmp(&a.at));
    let total = all.len();
    all.truncate(200);
    (all, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn etc_changes_mark_reeves_own() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.conf");
        let b = dir.path().join("sub/b.conf");
        fs::create_dir_all(b.parent().unwrap()).unwrap();
        fs::write(&a, "x").unwrap();
        fs::write(&b, "y").unwrap();
        let mut r = Receipt::draft(
            "s",
            "fs_edit",
            serde_json::json!({"path": a.to_string_lossy()}),
            Tier::T2,
        );
        r.seq = 12;
        let (changes, total) =
            etc_changes(dir.path(), Utc::now() - chrono::Duration::hours(1), &[r]);
        assert_eq!(total, 2);
        let mine: Vec<_> = changes.iter().filter_map(|c| c.by_reeve).collect();
        assert_eq!(mine, [12]);
        let (none, _) = etc_changes(dir.path(), Utc::now() + chrono::Duration::hours(1), &[]);
        assert!(none.is_empty());
    }

    #[test]
    fn pacmans_log_nets_out() {
        let log = "[2026-09-20T10:00:00-0400] [ALPM] upgraded old (1-1 -> 2-1)\n\
                   [2026-09-27T10:00:00-0400] [ALPM] installed htop (3.3-1)\n\
                   [2026-09-27T10:01:00-0400] [ALPM] upgraded linux (6.17.7.arch1-1 -> 6.17.9.arch1-1)\n\
                   [2026-09-27T10:02:00-0400] [ALPM] upgraded linux (6.17.9.arch1-1 -> 6.17.10.arch1-1)\n\
                   [2026-09-27T10:03:00-0400] [ALPM] installed tmp (1-1)\n\
                   [2026-09-27T10:04:00-0400] [ALPM] removed tmp (1-1)\n\
                   [2026-09-27T10:05:00-0400] [ALPM] removed nano (8.0-1)\n\
                   [2026-09-27T10:05:00-0400] [ALPM] running 'systemd-update.hook'...\n";
        let since = DateTime::parse_from_rfc3339("2026-09-25T00:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (i, u, r) = net_changes(pacman_events(log, since));
        let names = |v: &[Pkg]| v.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&i), ["htop"]);
        assert_eq!(names(&u), ["linux"]);
        assert_eq!(u[0].from.as_deref(), Some("6.17.7.arch1-1"));
        assert_eq!(u[0].to.as_deref(), Some("6.17.10.arch1-1"));
        assert_eq!(names(&r), ["nano"]);
    }

    #[test]
    fn kernels_are_recognized() {
        assert!(is_kernel("kernel-core.x86_64") && is_kernel("linux-zen"));
        assert!(!is_kernel("kernel-headers.x86_64") && !is_kernel("linux-firmware"));
    }

    #[test]
    fn the_baseline_is_the_last_snapshot_before_the_window() {
        let home = tempfile::tempdir().unwrap();
        let dir = state_dir(home.path());
        fs::create_dir_all(&dir).unwrap();
        let today = Local::now().date_naive();
        for back in [10, 8, 3] {
            let s = State {
                ts: Utc::now() - chrono::Duration::days(back),
                packages: BTreeMap::from([("marker".into(), back.to_string())]),
                ..Default::default()
            };
            let day = today - chrono::Duration::days(back);
            fs::write(
                dir.join(format!("{day}.json")),
                serde_json::to_string(&s).unwrap(),
            )
            .unwrap();
        }
        let week = baseline(home.path(), Utc::now() - chrono::Duration::days(7)).unwrap();
        assert_eq!(week.packages["marker"], "8");
        let month = baseline(home.path(), Utc::now() - chrono::Duration::days(30)).unwrap();
        assert_eq!(
            month.packages["marker"], "10",
            "none older: the oldest there is"
        );
    }
}
