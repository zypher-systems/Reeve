//! pacman's side of package undo. pacman has no transaction ids, so Reeve
//! reads what its own command changed out of `/var/log/pacman.log` (every
//! install, upgrade, downgrade, and removal, with versions), and undoes it
//! from the package cache: the old versions go back with `pacman -U`, and
//! what was new comes out with `pacman -R`. Those are two transactions.
//! This module plans them; which goes first is the caller's choice, by
//! what pacman accepts (`tools::sys`), and an undo that stops between the
//! two is recorded so the half that ran can be taken back.
//!
//! An undo that needs a version the cache no longer has (`paccache` cleans
//! it) is refused before anything runs.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::distro::quote;

/// Where pacman logs, unless pacman.conf says otherwise.
pub const LOG: &str = "/var/log/pacman.log";
/// Where pacman keeps downloaded packages, unless pacman.conf says otherwise.
pub const CACHE: &str = "/var/cache/pacman/pkg";

/// One package change pacman logged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PkgChange {
    /// Package name.
    pub name: String,
    /// `installed`, `removed`, `upgraded`, `downgraded`, or `reinstalled`.
    pub action: String,
    /// The version before; none for an install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    /// The version after; none for a removal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<String>,
}

/// The log file and cache directories, from `/etc/pacman.conf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `LogFile`.
    pub log: PathBuf,
    /// `CacheDir` (pacman allows several).
    pub caches: Vec<PathBuf>,
}

impl Paths {
    /// Read `/etc/pacman.conf`, falling back to pacman's defaults.
    pub fn detect() -> Self {
        Self::from_conf(&fs::read_to_string("/etc/pacman.conf").unwrap_or_default())
    }

    fn from_conf(conf: &str) -> Self {
        let mut log = None;
        let mut caches = Vec::new();
        for line in conf.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            match k.trim() {
                "LogFile" => log = Some(PathBuf::from(v.trim())),
                "CacheDir" => caches.extend(v.split_whitespace().map(PathBuf::from)),
                _ => {}
            }
        }
        if caches.is_empty() {
            caches.push(CACHE.into());
        }
        Self {
            log: log.unwrap_or_else(|| LOG.into()),
            caches,
        }
    }
}

/// Where the log ends now; the next transaction starts here.
pub fn mark(log: &Path) -> u64 {
    fs::metadata(log).map_or(0, |m| m.len())
}

/// The package changes logged after `mark`. A log that got shorter was
/// rotated, so it's read from the start.
pub fn changes_since(log: &Path, mark: u64) -> Vec<PkgChange> {
    parse(&text_since(log, mark))
}

/// Whether pacman started after `mark`: it logs the command it was run
/// with before it does anything, in every locale. A pacman that started
/// and logged no change refused the transaction itself. One that never
/// started was never reached: sudo said no, in whatever language.
pub fn started_since(log: &Path, mark: u64) -> bool {
    started(&text_since(log, mark))
}

fn started(text: &str) -> bool {
    text.lines().any(|l| l.contains("] [PACMAN] Running '"))
}

fn text_since(log: &Path, mark: u64) -> String {
    let Ok(mut f) = fs::File::open(log) else {
        return String::new();
    };
    let len = f.metadata().map_or(0, |m| m.len());
    let start = if len < mark { 0 } else { mark };
    let mut text = String::new();
    if f.seek(SeekFrom::Start(start)).is_err() || f.read_to_string(&mut text).is_err() {
        return String::new();
    }
    text
}

/// Package changes in log text: the `[ALPM]` lines.
pub fn parse(text: &str) -> Vec<PkgChange> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<PkgChange> {
    // [2026-09-30T10:00:01-0400] [ALPM] upgraded foo (1.0-1 -> 1.1-1)
    let rest = line.split_once("] [ALPM] ")?.1.trim();
    let (action, rest) = rest.split_once(' ')?;
    let (name, rest) = rest.split_once(' ')?;
    let versions = rest.strip_prefix('(')?.strip_suffix(')')?;
    let (from, to) = match action {
        "installed" => (None, Some(versions.to_string())),
        "removed" => (Some(versions.to_string()), None),
        "reinstalled" => (Some(versions.to_string()), Some(versions.to_string())),
        "upgraded" | "downgraded" => {
            let (a, b) = versions.split_once(" -> ")?;
            (Some(a.to_string()), Some(b.to_string()))
        }
        _ => return None,
    };
    Some(PkgChange {
        name: name.to_string(),
        action: action.to_string(),
        from,
        to,
    })
}

/// Short words for a receipt: `installed htop, upgraded 2`.
pub fn summary(changes: &[PkgChange]) -> String {
    let mut parts = Vec::new();
    for action in [
        "installed",
        "upgraded",
        "downgraded",
        "removed",
        "reinstalled",
    ] {
        let names: Vec<&str> = changes
            .iter()
            .filter(|c| c.action == action)
            .map(|c| c.name.as_str())
            .collect();
        match names.len() {
            0 => {}
            1..=3 => parts.push(format!("{action} {}", names.join(", "))),
            n => parts.push(format!("{action} {n}")),
        }
    }
    parts.join("; ")
}

/// How to undo some changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoPlan {
    /// Cached packages to put back (the versions before).
    pub restore: Vec<PathBuf>,
    /// Packages the changes installed, to remove.
    pub remove: Vec<String>,
    /// Versions the cache doesn't have: `name version`.
    pub missing: Vec<String>,
}

impl UndoPlan {
    fn files(&self) -> String {
        let files: Vec<String> = self
            .restore
            .iter()
            .map(|p| quote(&p.display().to_string()))
            .collect();
        files.join(" ")
    }

    fn names(&self) -> String {
        let names: Vec<String> = self.remove.iter().map(|n| quote(n)).collect();
        names.join(" ")
    }

    /// Put the old versions back (root). `None` when there are none.
    pub fn restore_command(&self) -> Option<String> {
        (!self.restore.is_empty()).then(|| format!("sudo pacman -U --noconfirm {}", self.files()))
    }

    /// Remove what the changes installed (root). `None` when there's nothing.
    pub fn remove_command(&self) -> Option<String> {
        (!self.remove.is_empty()).then(|| format!("sudo pacman -R --noconfirm {}", self.names()))
    }

    /// Whether [`UndoPlan::restore_command`] would go through now: pacman
    /// resolves it and prints the targets, changing nothing (no root).
    pub fn restore_check(&self) -> Option<String> {
        (!self.restore.is_empty()).then(|| format!("pacman -U --print {}", self.files()))
    }

    /// Whether [`UndoPlan::remove_command`] would go through now.
    pub fn remove_check(&self) -> Option<String> {
        (!self.remove.is_empty()).then(|| format!("pacman -R --print {}", self.names()))
    }

    /// The whole undo as one line, in the order given. pacman can't put
    /// packages back and take others out in one transaction, and either
    /// half may need the other done first: an upgrade that pulled in a new
    /// dependency needs the old version back before the dependency can go,
    /// and an install that upgraded a library needs the new package gone
    /// before the library can go back. The caller picks by trying each
    /// (`*_check`). `None` when there's nothing to do.
    pub fn command(&self, remove_first: bool) -> Option<String> {
        let mut steps: Vec<String> = [self.restore_command(), self.remove_command()]
            .into_iter()
            .flatten()
            .collect();
        if remove_first {
            steps.reverse();
        }
        (!steps.is_empty()).then(|| steps.join(" && "))
    }
}

/// Plan the undo of `changes`. `files` lists a cache directory's file names.
pub fn undo_plan(
    changes: &[PkgChange],
    caches: &[PathBuf],
    files: impl Fn(&Path) -> Vec<String>,
) -> UndoPlan {
    let listed: Vec<(PathBuf, Vec<String>)> =
        caches.iter().map(|c| (c.clone(), files(c))).collect();
    let mut plan = UndoPlan::default();
    // The newest change to a package decides what it goes back to.
    let mut seen = std::collections::HashSet::new();
    let mut restore_names = Vec::new();
    for c in changes.iter().rev() {
        if !seen.insert(c.name.as_str()) {
            continue;
        }
        let first = changes.iter().find(|x| x.name == c.name).unwrap_or(c);
        match first.from.as_deref() {
            // It wasn't there before: remove it (unless it's gone again).
            None => {
                if c.to.is_some() {
                    plan.remove.push(c.name.clone());
                }
            }
            // Put the version from before back, unless it's that already.
            Some(before) => {
                if c.to.as_deref() == Some(before) {
                    continue;
                }
                match listed
                    .iter()
                    .find_map(|(dir, names)| cached(names, &c.name, before).map(|f| dir.join(f)))
                {
                    Some(p) => restore_names.push((c.name.clone(), p)),
                    None => plan.missing.push(format!("{} {before}", c.name)),
                }
            }
        }
    }
    restore_names.sort();
    plan.restore = restore_names.into_iter().map(|(_, p)| p).collect();
    plan.remove.sort();
    plan.missing.sort();
    plan
}

/// What a package file's name ends in. A `.sig` isn't one, and neither is
/// a download that never finished (`.part`).
const ARCHIVES: &[&str] = &[
    "pkg.tar",
    "pkg.tar.zst",
    "pkg.tar.xz",
    "pkg.tar.gz",
    "pkg.tar.bz2",
    "pkg.tar.lz4",
    "pkg.tar.lzo",
    "pkg.tar.lrz",
    "pkg.tar.lz",
    "pkg.tar.Z",
];

/// The cache file for `name` at `version`: `name-version-arch.pkg.tar.*`.
fn cached(names: &[String], name: &str, version: &str) -> Option<String> {
    let prefix = format!("{name}-{version}-");
    names
        .iter()
        .find(|f| {
            f.strip_prefix(&prefix).is_some_and(|rest| {
                let (arch, ext) = rest.split_once('.').unwrap_or((rest, ""));
                !arch.is_empty() && !arch.contains('-') && ARCHIVES.contains(&ext)
            })
        })
        .cloned()
}

/// Whether the repos have `name` (a package, a group, or something a
/// package provides). Reads pacman's local sync databases; nothing is
/// downloaded.
pub fn in_repos(name: &str) -> bool {
    std::process::Command::new("pacman")
        .args(["-Sp", "--print-format", "%n", "--"])
        .arg(name)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// An AUR package's PKGBUILD, as the helper fetches it (`-Gp`).
pub fn pkgbuild(helper: &str, name: &str) -> Result<String, String> {
    let o = std::process::Command::new("timeout")
        .args(["30", helper, "-Gp", "--", name])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("couldn't run {helper}: {e}"))?;
    let text = String::from_utf8_lossy(&o.stdout).into_owned();
    if o.status.success() && text.contains("pkgname") {
        Ok(text)
    } else {
        Err(format!("{name} isn't in the repos or the AUR"))
    }
}

/// Marks a detail line the card draws as code, not as a sentence.
pub const CODE: &str = "│ ";

/// Lines of PKGBUILD the approval card shows, per package.
const PKGBUILD_LINES: usize = 60;

/// What the approval card says about AUR packages: the warning, then each
/// PKGBUILD (long ones cut, with how to read the rest). PKGBUILD lines start
/// with [`CODE`], which the card draws as code.
pub fn aur_details(pkgbuilds: &[(String, String)], helper: &str) -> Vec<String> {
    let mut out = vec![
        "AUR: built here from PKGBUILDs anyone can submit. Arch doesn't review them; read them before you say yes.".to_string(),
    ];
    for (name, text) in pkgbuilds {
        out.push(format!("── PKGBUILD: {name} ──"));
        let lines: Vec<&str> = text.lines().collect();
        out.extend(
            lines
                .iter()
                .take(PKGBUILD_LINES)
                .map(|l| format!("{CODE}{l}")),
        );
        if lines.len() > PKGBUILD_LINES {
            out.push(format!(
                "… {} more lines: {helper} -Gp {name}",
                lines.len() - PKGBUILD_LINES
            ));
        }
        // `install=` names a script that runs as root; an empty one doesn't.
        let script = lines
            .iter()
            .find_map(|l| l.trim().strip_prefix("install="))
            .map(|s| s.trim().trim_matches(|c| c == '\'' || c == '"'))
            .filter(|s| !s.is_empty());
        if let Some(script) = script {
            out.push(format!(
                "{name} also runs {script} as root while it installs; that script isn't shown here."
            ));
        }
    }
    out
}

/// A cache directory's file names.
pub fn list_dir(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG_TEXT: &str = "\
[2026-09-30T10:00:00-0400] [PACMAN] Running 'pacman -S --needed --noconfirm htop'
[2026-09-30T10:00:01-0400] [ALPM] transaction started
[2026-09-30T10:00:01-0400] [ALPM] installed libnl (3.11.0-1)
[2026-09-30T10:00:01-0400] [ALPM] installed htop (3.4.1-1)
[2026-09-30T10:00:01-0400] [ALPM] upgraded ncurses (6.5-3 -> 6.5-4)
[2026-09-30T10:00:01-0400] [ALPM] downgraded zstd (1.5.7-2 -> 1.5.7-1)
[2026-09-30T10:00:01-0400] [ALPM] removed nano (8.4-1)
[2026-09-30T10:00:01-0400] [ALPM] reinstalled bash (5.3.3-1)
[2026-09-30T10:00:01-0400] [ALPM-SCRIPTLET] some hook output (not a change)
[2026-09-30T10:00:02-0400] [ALPM] transaction completed
";

    fn cache() -> Vec<String> {
        [
            "ncurses-6.5-3-x86_64.pkg.tar.zst",
            "ncurses-6.5-3-x86_64.pkg.tar.zst.sig",
            "ncurses-6.5-4-x86_64.pkg.tar.zst",
            "zstd-1.5.7-2-x86_64.pkg.tar.zst",
            "nano-8.4-1-x86_64.pkg.tar.zst",
            "nano-syntax-8.4-1-any.pkg.tar.zst",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn a_pacman_that_started_is_in_its_log() {
        // Refused in prepare (a conflict): the command is logged, no change.
        let refused = "[2026-10-01T13:04:00+0000] [PACMAN] Running 'pacman -U --noconfirm /var/cache/pacman/pkg/a-1-1-any.pkg.tar.zst'\n";
        assert!(started(refused));
        assert!(parse(refused).is_empty());
        // sudo never ran it: nothing is logged, whatever sudo printed.
        assert!(!started(""));
        // Only pacman's own line counts.
        assert!(!started(
            "[2026-10-01T13:04:00+0000] [ALPM] installed a (1-1)\n"
        ));
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("pacman.log");
        fs::write(&log, refused).unwrap();
        assert!(started_since(&log, 0));
        assert!(!started_since(&log, mark(&log)));
        assert!(!started_since(&d.path().join("none.log"), 0));
    }

    #[test]
    fn the_log_reads_as_changes() {
        let c = parse(LOG_TEXT);
        assert_eq!(c.len(), 6);
        assert_eq!(
            c[2],
            PkgChange {
                name: "ncurses".into(),
                action: "upgraded".into(),
                from: Some("6.5-3".into()),
                to: Some("6.5-4".into())
            }
        );
        assert_eq!(c[4].to, None);
        assert_eq!(c[0].from, None);
        assert_eq!(
            summary(&c),
            "installed libnl, htop; upgraded ncurses; downgraded zstd; removed nano; reinstalled bash"
        );
    }

    #[test]
    fn an_undo_puts_old_versions_back_then_removes_new_packages() {
        let caches = vec![PathBuf::from("/var/cache/pacman/pkg")];
        let p = undo_plan(&parse(LOG_TEXT), &caches, |_| cache());
        assert_eq!(p.missing, Vec::<String>::new());
        assert_eq!(p.remove, ["htop", "libnl"]);
        assert_eq!(
            p.restore,
            [
                "/var/cache/pacman/pkg/nano-8.4-1-x86_64.pkg.tar.zst",
                "/var/cache/pacman/pkg/ncurses-6.5-3-x86_64.pkg.tar.zst",
                "/var/cache/pacman/pkg/zstd-1.5.7-2-x86_64.pkg.tar.zst",
            ]
            .map(PathBuf::from)
        );
        let restore = "sudo pacman -U --noconfirm /var/cache/pacman/pkg/nano-8.4-1-x86_64.pkg.tar.zst \
                       /var/cache/pacman/pkg/ncurses-6.5-3-x86_64.pkg.tar.zst \
                       /var/cache/pacman/pkg/zstd-1.5.7-2-x86_64.pkg.tar.zst";
        let remove = "sudo pacman -R --noconfirm htop libnl";
        // Either order, for the caller to pick by what resolves.
        assert_eq!(p.command(false).unwrap(), format!("{restore} && {remove}"));
        assert_eq!(p.command(true).unwrap(), format!("{remove} && {restore}"));
        assert_eq!(p.remove_check().unwrap(), "pacman -R --print htop libnl");
        assert!(
            p.restore_check()
                .unwrap()
                .starts_with("pacman -U --print /var/cache")
        );
    }

    #[test]
    fn a_version_the_cache_lost_is_named() {
        let caches = vec![PathBuf::from("/c")];
        let p = undo_plan(&parse(LOG_TEXT), &caches, |_| vec![]);
        assert_eq!(p.missing, ["nano 8.4-1", "ncurses 6.5-3", "zstd 1.5.7-2"]);
    }

    #[test]
    fn a_package_changed_twice_goes_back_to_where_it_started() {
        let text = "\
[t] [ALPM] upgraded foo (1.0-1 -> 1.1-1)
[t] [ALPM] upgraded foo (1.1-1 -> 1.2-1)
[t] [ALPM] installed bar (2.0-1)
[t] [ALPM] removed bar (2.0-1)
[t] [ALPM] removed baz (3.0-1)
[t] [ALPM] installed baz (3.0-1)
";
        let caches = vec![PathBuf::from("/c")];
        let files = |_: &Path| vec!["foo-1.0-1-x86_64.pkg.tar.zst".to_string()];
        let p = undo_plan(&parse(text), &caches, files);
        assert_eq!(
            p.restore,
            [PathBuf::from("/c/foo-1.0-1-x86_64.pkg.tar.zst")]
        );
        // bar came and went; baz went and came back as it was.
        assert!(p.remove.is_empty(), "{p:?}");
        assert!(p.missing.is_empty(), "{p:?}");
        assert_eq!(UndoPlan::default().command(false), None);
        assert_eq!(UndoPlan::default().restore_check(), None);
    }

    #[test]
    fn epochs_and_similar_names_match_exactly() {
        let names = vec![
            "python-3.13.7-1-x86_64.pkg.tar.zst".to_string(),
            "python-pip-25.2-1-any.pkg.tar.zst".to_string(),
            "go-2:1.25.1-1-x86_64.pkg.tar.zst".to_string(),
        ];
        assert_eq!(
            cached(&names, "python", "3.13.7-1").as_deref(),
            Some("python-3.13.7-1-x86_64.pkg.tar.zst")
        );
        assert_eq!(cached(&names, "python", "25.2-1"), None);
        // A signature or an unfinished download is never the package.
        let junk = vec![
            "htop-3.5.2-1-x86_64.pkg.tar.zst.part".to_string(),
            "htop-3.5.2-1-x86_64.pkg.tar.zst.sig".to_string(),
        ];
        assert_eq!(cached(&junk, "htop", "3.5.2-1"), None);
        assert_eq!(
            cached(&names, "go", "2:1.25.1-1").as_deref(),
            Some("go-2:1.25.1-1-x86_64.pkg.tar.zst")
        );
    }

    #[test]
    fn the_card_shows_each_pkgbuild_and_its_install_script() {
        let short = "pkgname=foo\npkgver=1\ninstall=foo.install\n".to_string();
        let long: String = (0..70).map(|i| format!("line{i}\n")).collect();
        let d = aur_details(
            &[
                ("foo".into(), short),
                ("bar".into(), format!("pkgname=bar\n{long}")),
            ],
            "paru",
        );
        assert!(d[0].starts_with("AUR:"));
        assert!(d.contains(&"── PKGBUILD: foo ──".to_string()));
        assert!(d.contains(&"│ pkgver=1".to_string()), "{d:?}");
        assert!(d.iter().any(|l| l == "foo also runs foo.install as root while it installs; that script isn't shown here."));
        assert!(
            d.iter().any(|l| l == "… 11 more lines: paru -Gp bar"),
            "{d:?}"
        );
        // An empty install= names no script.
        let none = aur_details(&[("baz".into(), "pkgname=baz\ninstall=\n".into())], "yay");
        assert!(!none.iter().any(|l| l.contains("as root")), "{none:?}");
    }

    /// Real pacman, on an Arch box (a throwaway container), as a user with
    /// passwordless sudo: `REEVE_LIVE_PACMAN=1`, and `REEVE_LIVE_OLD_PKG` set
    /// to an archived older htop to upgrade from. It installs and removes
    /// packages, so it never runs on its own.
    #[test]
    #[ignore]
    fn live_pacman_round_trip() {
        use crate::distro::Distro;
        use std::process::Command;
        if std::env::var("REEVE_LIVE_PACMAN").is_err() {
            return;
        }
        let old_url = std::env::var("REEVE_LIVE_OLD_PKG").expect("REEVE_LIVE_OLD_PKG");
        let d = Distro::detect();
        assert!(matches!(d, Distro::Arch { .. }), "{d:?}");
        let paths = Paths::detect();
        let sh = |c: &str| {
            println!("$ {c}");
            Command::new("sh")
                .args(["-c", c])
                .status()
                .unwrap()
                .success()
        };
        let version = |p: &str| {
            let o = Command::new("pacman").args(["-Q", p]).output().unwrap();
            o.status.success().then(|| {
                String::from_utf8_lossy(&o.stdout)
                    .split_whitespace()
                    .nth(1)
                    .unwrap()
                    .to_string()
            })
        };
        // As revert_pacman does: try each order without changing anything,
        // and run the one that resolves. Returns whether removal went first.
        let undo = |changes: &[PkgChange]| -> bool {
            let plan = undo_plan(changes, &paths.caches, list_dir);
            assert!(plan.missing.is_empty(), "{plan:?}");
            let remove_first = match (plan.restore_check(), plan.remove_check()) {
                (Some(restore), Some(remove)) => {
                    if sh(&restore) {
                        false
                    } else {
                        assert!(sh(&remove), "neither order resolves: {plan:?}");
                        true
                    }
                }
                _ => false,
            };
            assert!(sh(&plan.command(remove_first).unwrap()));
            remove_first
        };
        let has = |c: &[PkgChange], name: &str, action: &str| {
            c.iter().any(|x| x.name == name && x.action == action)
        };

        // 1. Install; undo removes it, and the undo logs its own inverse.
        sh(
            "for p in htop pipes.sh; do sudo pacman -Rns --noconfirm $p >/dev/null 2>&1; done; true",
        );
        let m = mark(&paths.log);
        assert!(sh(&d.install(&["htop".into()])));
        let c = changes_since(&paths.log, m);
        assert!(has(&c, "htop", "installed"), "{c:?}");
        let m = mark(&paths.log);
        undo(&c);
        assert_eq!(version("htop"), None);
        assert!(has(&changes_since(&paths.log, m), "htop", "removed"));

        // 2. Upgrade from an older htop; undo puts the old one back from the cache.
        assert!(sh(&format!("sudo pacman -U --noconfirm {old_url}")));
        let old = version("htop").unwrap();
        let m = mark(&paths.log);
        assert!(sh(&d.upgrade(&["htop".into()])));
        assert_ne!(version("htop").as_deref(), Some(old.as_str()));
        let c = changes_since(&paths.log, m);
        assert!(has(&c, "htop", "upgraded"), "{c:?}");
        undo(&c);
        assert_eq!(version("htop").as_deref(), Some(old.as_str()));

        // 3. Remove; undo reinstalls the same version from the cache.
        let m = mark(&paths.log);
        assert!(sh(&d.remove(&["htop".into()])));
        undo(&changes_since(&paths.log, m));
        assert_eq!(version("htop").as_deref(), Some(old.as_str()));

        // 4. What's in the repos (a package, a group, a name a repo package
        // provides: pfetch-rs provides pfetch) and what isn't.
        assert!(in_repos("htop") && in_repos("base-devel") && in_repos("pfetch"));
        assert!(!in_repos("pipes.sh"));

        // 5. Undo is two transactions, and which goes first depends on the
        // change. Tiny local packages make both cases: `lib` 1, 2, and 3 (3
        // needs `dep`), `app` (needs lib>=2), and `dep`.
        let build = r#"set -eu
d=$(mktemp -d); cd "$d"
mk() { # name ver depends
  mkdir -p "$1-$2"; cat > "$1-$2/PKGBUILD" <<EOF
pkgname=reeve-t-$1
pkgver=$2
pkgrel=1
arch=(any)
depends=($3)
package() { install -d "\$pkgdir/usr/share/reeve-t"; echo $2 > "\$pkgdir/usr/share/reeve-t/$1"; }
EOF
  (cd "$1-$2" && makepkg -f --nodeps >/dev/null 2>&1)
  sudo cp "$1-$2"/reeve-t-$1-$2-1-any.pkg.tar.zst /var/cache/pacman/pkg/
}
mk lib 1 ""; mk lib 2 ""; mk lib 3 "reeve-t-dep"; mk app 1 "'reeve-t-lib>=2'"; mk dep 1 ""
for p in reeve-t-app reeve-t-lib reeve-t-dep; do sudo pacman -Rdd --noconfirm $p >/dev/null 2>&1 || true; done
"#;
        assert!(sh(build), "building the test packages");
        let pkg =
            |n: &str, v: u32| format!("/var/cache/pacman/pkg/reeve-t-{n}-{v}-1-any.pkg.tar.zst");
        assert!(sh(&format!("sudo pacman -U --noconfirm {}", pkg("lib", 1))));

        // An install that upgraded a library the new package needs: the old
        // library can't go back while the package is there, so it goes first.
        let m = mark(&paths.log);
        assert!(sh(&format!(
            "sudo pacman -U --noconfirm {} {}",
            pkg("lib", 2),
            pkg("app", 1)
        )));
        let c = changes_since(&paths.log, m);
        assert!(
            has(&c, "reeve-t-app", "installed") && has(&c, "reeve-t-lib", "upgraded"),
            "{c:?}"
        );
        assert!(undo(&c), "the removal has to go first");
        assert_eq!(version("reeve-t-app"), None);
        assert_eq!(version("reeve-t-lib").as_deref(), Some("1-1"));

        // An upgrade that pulled in a new dependency: the dependency can't
        // go while the new version needs it, so the old version goes back first.
        assert!(sh(&format!("sudo pacman -U --noconfirm {}", pkg("lib", 2))));
        let m = mark(&paths.log);
        assert!(sh(&format!(
            "sudo pacman -U --noconfirm {} {}",
            pkg("lib", 3),
            pkg("dep", 1)
        )));
        let c = changes_since(&paths.log, m);
        assert!(!undo(&c), "the old version has to go back first");
        assert_eq!(version("reeve-t-dep"), None);
        assert_eq!(version("reeve-t-lib").as_deref(), Some("2-1"));
        assert!(sh("sudo pacman -R --noconfirm reeve-t-lib"));

        // 6. The AUR, through the helper, without a prompt; undo removes it.
        if let Distro::Arch { aur: Some(h) } = &d {
            let text = pkgbuild(h, "pipes.sh").unwrap();
            assert!(text.contains("pkgname=pipes.sh"), "{text}");
            assert!(pkgbuild(h, "no-such-package-reeve-test").is_err());
            let m = mark(&paths.log);
            assert!(sh(&d.aur_install(&["pipes.sh".into()]).unwrap()));
            let c = changes_since(&paths.log, m);
            assert!(has(&c, "pipes.sh", "installed"), "{c:?}");
            undo(&c);
            assert_eq!(version("pipes.sh"), None);
        }
    }

    #[test]
    fn pacman_conf_can_move_the_log_and_the_cache() {
        let p = Paths::from_conf(
            "[options]\n#LogFile = /nope\nLogFile = /var/log/my.log\nCacheDir = /a/ /b/  # two\n",
        );
        assert_eq!(p.log, PathBuf::from("/var/log/my.log"));
        assert_eq!(p.caches, [PathBuf::from("/a/"), PathBuf::from("/b/")]);
        let d = Paths::from_conf("");
        assert_eq!((d.log.to_str(), d.caches.len()), (Some(LOG), 1));
    }

    #[test]
    fn only_what_came_after_the_mark_counts() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("pacman.log");
        fs::write(&log, "[t] [ALPM] installed old (1-1)\n").unwrap();
        let m = mark(&log);
        let mut f = fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"[t] [ALPM] installed new (2-1)\n").unwrap();
        let c = changes_since(&log, m);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "new");
        // Rotated (shorter than the mark): read from the start.
        fs::write(&log, "[t] [ALPM] removed x (1-1)\n").unwrap();
        assert_eq!(changes_since(&log, m + 1000)[0].name, "x");
        assert!(changes_since(&d.path().join("none"), 0).is_empty());
    }
}
