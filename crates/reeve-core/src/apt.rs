//! apt's side of package undo. apt has no transaction ids either, so Reeve
//! reads what its own command changed out of dpkg's log
//! (`/var/log/dpkg.log`: every install, upgrade, and removal, with
//! versions) and undoes it from apt's package cache: the old versions go
//! back from `/var/cache/apt/archives` and what was new comes out. Unlike
//! pacman, apt does both in one transaction (`apt-get install ./old.deb
//! new-`), and `apt-get -s` shows the whole plan before anything runs, so an
//! undo that would take something else with it is refused.
//!
//! A version the cache no longer has (`apt-get clean`, or `apt` rather than
//! `apt-get` installed it, which deletes the download) is asked for by
//! name and version instead; if the repos no longer carry it, apt says so
//! and nothing changes.

use std::fs;
use std::path::{Path, PathBuf};

use crate::distro::quote;
pub use crate::pacman::PkgChange;

/// Where dpkg logs, unless `/etc/dpkg/dpkg.cfg` says otherwise.
pub const LOG: &str = "/var/log/dpkg.log";
/// Where apt keeps downloaded packages, unless apt.conf says otherwise.
pub const CACHE: &str = "/var/cache/apt/archives";

/// The log file and the cache directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// dpkg's `log`.
    pub log: PathBuf,
    /// apt's `Dir::Cache::Archives`.
    pub cache: PathBuf,
}

impl Paths {
    /// Read dpkg's config and ask apt-config, falling back to the defaults.
    pub fn detect() -> Self {
        let dpkg = fs::read_to_string("/etc/dpkg/dpkg.cfg").unwrap_or_default();
        let archives = std::process::Command::new("apt-config")
            .args(["shell", "CACHE", "Dir::Cache::Archives/d"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        Self::from_conf(&dpkg, archives.as_deref())
    }

    /// `dpkg_cfg` is `/etc/dpkg/dpkg.cfg`; `archives` is what `apt-config
    /// shell CACHE Dir::Cache::Archives/d` printed (`CACHE='/var/…/'`).
    fn from_conf(dpkg_cfg: &str, archives: Option<&str>) -> Self {
        let log = dpkg_cfg
            .lines()
            .map(|l| l.split('#').next().unwrap_or("").trim())
            .find_map(|l| l.strip_prefix("log "))
            .map(|p| PathBuf::from(p.trim()))
            .unwrap_or_else(|| LOG.into());
        let cache = archives
            .and_then(|s| s.trim().strip_prefix("CACHE='"))
            .and_then(|s| s.strip_suffix('\''))
            .map(|s| s.trim_end_matches('/'))
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| CACHE.into());
        Self { log, cache }
    }
}

/// Where the log ends now; the next command's changes start here.
pub fn mark(log: &Path) -> u64 {
    crate::pacman::mark(log)
}

/// The package changes dpkg logged after `mark`. A log that got shorter
/// was rotated, so it's read from the start.
pub fn changes_since(log: &Path, mark: u64) -> Vec<PkgChange> {
    parse(&crate::pacman::text_since(log, mark))
}

/// Package changes in dpkg's log text. Names keep their `:arch`, as dpkg
/// writes them.
pub fn parse(text: &str) -> Vec<PkgChange> {
    text.lines().filter_map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<PkgChange> {
    // 2026-10-11 02:30:43 upgrade reeve-t-lib:all 1 2
    // 2026-10-11 02:30:43 install reeve-t-app:all <none> 1
    // 2026-10-11 02:30:43 remove reeve-t-app:all 1 <none>
    let mut f = line.split_whitespace();
    let (_date, _time) = (f.next()?, f.next()?);
    let action = f.next()?;
    let name = f.next()?;
    let from = f.next()?;
    let to = f.next()?;
    let version = |v: &str| (v != "<none>").then(|| v.to_string());
    let (action, from, to) = match action {
        // A package in the config-files state logs its old version here,
        // though it wasn't installed: an install is an install.
        "install" => ("installed", None, version(to)?),
        // dpkg calls every version change an upgrade; the versions say.
        "upgrade" | "downgrade" => {
            let (a, b) = (version(from)?, version(to)?);
            let action = if a == b {
                "reinstalled"
            } else if action == "downgrade" {
                "downgraded"
            } else {
                "upgraded"
            };
            (action, Some(a), b)
        }
        "remove" => ("removed", Some(version(from)?), String::new()),
        // A purge of an installed package logs `remove` first, which is
        // the change; a purge of config files alone changes no package.
        _ => return None,
    };
    Some(PkgChange {
        name: name.to_string(),
        action: action.to_string(),
        from,
        to: (!to.is_empty()).then_some(to),
    })
}

/// One package to put back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Restore {
    /// `name:arch`, as dpkg logs it.
    pub name: String,
    /// The version before.
    pub version: String,
    /// Its file in apt's cache, when the cache still has it.
    pub file: Option<PathBuf>,
}

impl Restore {
    /// What to give `apt-get install`: the cached file, or else the
    /// version by name (apt fetches it, if the repos still carry it).
    fn target(&self) -> String {
        match &self.file {
            Some(f) => quote(&f.display().to_string()),
            None => quote(&format!("{}={}", bare_if_all(&self.name), self.version)),
        }
    }
}

/// `name:all` → `name`: packages for every architecture are addressed
/// without one. Others keep theirs (a foreign-arch library stays exact).
fn bare_if_all(name: &str) -> &str {
    name.strip_suffix(":all").unwrap_or(name)
}

/// `name:arch` → `name`.
fn bare(name: &str) -> &str {
    name.split_once(':').map_or(name, |(n, _)| n)
}

/// How to undo some changes: one `apt-get install` that puts old versions
/// back and takes new packages out.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UndoPlan {
    /// Versions to put back.
    pub restore: Vec<Restore>,
    /// Packages the changes installed, to remove (`name:arch`).
    pub remove: Vec<String>,
}

/// Options every undo run passes: say yes, allow going back to an older
/// version, and keep the owner's config files.
const UNDO_OPTS: &str = "-y --allow-downgrades -o Dpkg::Options::=--force-confold";

impl UndoPlan {
    fn targets(&self) -> String {
        let mut t: Vec<String> = self.restore.iter().map(Restore::target).collect();
        t.extend(
            self.remove
                .iter()
                .map(|n| quote(&format!("{}-", bare_if_all(n)))),
        );
        t.join(" ")
    }

    /// The whole undo, as one transaction (root). `None` when there's
    /// nothing to do.
    pub fn command(&self) -> Option<String> {
        (!self.restore.is_empty() || !self.remove.is_empty())
            .then(|| format!("sudo apt-get install {UNDO_OPTS} {}", self.targets()))
    }

    /// The same, simulated: apt resolves it and prints what it would do
    /// (`Inst`, `Remv`), changing nothing and needing no root.
    pub fn check(&self) -> Option<String> {
        (!self.restore.is_empty() || !self.remove.is_empty())
            .then(|| format!("apt-get -s -q install {UNDO_OPTS} {}", self.targets()))
    }

    /// Versions the cache doesn't have: `name version`.
    pub fn missing(&self) -> Vec<String> {
        self.restore
            .iter()
            .filter(|r| r.file.is_none())
            .map(|r| format!("{} {}", r.name, r.version))
            .collect()
    }
}

/// Plan the undo of `changes`. `files` lists the cache directory's file
/// names.
pub fn undo_plan(
    changes: &[PkgChange],
    cache: &Path,
    files: impl Fn(&Path) -> Vec<String>,
) -> UndoPlan {
    let names = files(cache);
    let mut plan = UndoPlan::default();
    // The newest change to a package decides what it goes back to.
    let mut seen = std::collections::HashSet::new();
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
                plan.restore.push(Restore {
                    name: c.name.clone(),
                    version: before.to_string(),
                    file: cached(&names, &c.name, before).map(|f| cache.join(f)),
                });
            }
        }
    }
    plan.restore.sort_by(|a, b| a.name.cmp(&b.name));
    plan.remove.sort();
    plan
}

/// The cache file for `name:arch` at `version`:
/// `name_version_arch.deb`, with `:` in the version written `%3a`.
fn cached(names: &[String], name: &str, version: &str) -> Option<String> {
    let (pkg, arch) = name.split_once(':')?;
    let want = format!("{pkg}_{}_{arch}.deb", version.replace(':', "%3a"));
    names.iter().find(|f| **f == want).cloned()
}

/// What `apt-get -s` said it would do.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Simulation {
    /// Packages it would install or change (`Inst`), bare names.
    pub inst: Vec<String>,
    /// Packages it would remove (`Remv`), bare names.
    pub remv: Vec<String>,
    /// Its `E:` lines, when it refused.
    pub errors: Vec<String>,
}

/// Read a simulation's output.
pub fn simulated(out: &str) -> Simulation {
    let mut s = Simulation::default();
    for l in out.lines() {
        let l = l.trim();
        if let Some(rest) = l.strip_prefix("Inst ") {
            s.inst
                .push(bare(rest.split_whitespace().next().unwrap_or("")).to_string());
        } else if let Some(rest) = l.strip_prefix("Remv ") {
            s.remv
                .push(bare(rest.split_whitespace().next().unwrap_or("")).to_string());
        } else if let Some(e) = l.strip_prefix("E: ") {
            s.errors.push(e.to_string());
        }
    }
    s
}

/// Removals the simulation shows that the plan didn't ask for: apt would
/// take them along to satisfy the rest.
pub fn unplanned_removals(plan: &UndoPlan, sim: &Simulation) -> Vec<String> {
    let planned: Vec<&str> = plan.remove.iter().map(|n| bare(n)).collect();
    let mut extra: Vec<String> = sim
        .remv
        .iter()
        .filter(|n| !planned.contains(&n.as_str()))
        .cloned()
        .collect();
    extra.sort();
    extra.dedup();
    extra
}

/// A directory's file names.
pub fn list_dir(dir: &Path) -> Vec<String> {
    crate::pacman::list_dir(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG_TEXT: &str = "\
2026-10-11 02:28:21 startup archives unpack
2026-10-11 02:28:21 install libnl-genl-3-200:amd64 <none> 3.7.0-0.2+b1
2026-10-11 02:28:21 status half-installed libnl-genl-3-200:amd64 3.7.0-0.2+b1
2026-10-11 02:28:21 install htop:amd64 <none> 3.2.2-2
2026-10-11 02:28:21 upgrade libjpeg62-turbo:amd64 1:2.1.4-1 1:2.1.5-2
2026-10-11 02:28:21 upgrade reeve-t-lib:all 2 1
2026-10-11 02:28:21 upgrade bash:amd64 5.2.15-2+b8 5.2.15-2+b8
2026-10-11 02:28:21 configure htop:amd64 3.2.2-2 <none>
2026-10-11 02:28:22 remove nano:amd64 7.2-1+deb12u1 <none>
2026-10-11 02:28:22 purge nano:amd64 7.2-1+deb12u1 <none>
2026-10-11 02:28:23 purge old-config:amd64 1.0-1 <none>
2026-10-11 02:28:24 trigproc libc-bin:amd64 2.36-9+deb12u10 <none>
";

    fn cache() -> Vec<String> {
        [
            "libjpeg62-turbo_1%3a2.1.4-1_amd64.deb",
            "libjpeg62-turbo_1%3a2.1.5-2_amd64.deb",
            "reeve-t-lib_2_all.deb",
            "nano_7.2-1+deb12u1_amd64.deb",
            "nano_7.2-1+deb12u1_amd64.deb.part",
            "lock",
        ]
        .map(String::from)
        .to_vec()
    }

    #[test]
    fn the_log_reads_as_changes() {
        let c = parse(LOG_TEXT);
        let words: Vec<String> = c
            .iter()
            .map(|x| format!("{} {}", x.action, x.name))
            .collect();
        assert_eq!(
            words,
            [
                "installed libnl-genl-3-200:amd64",
                "installed htop:amd64",
                "upgraded libjpeg62-turbo:amd64",
                "upgraded reeve-t-lib:all",
                "reinstalled bash:amd64",
                "removed nano:amd64",
            ]
        );
        assert_eq!(c[2].from.as_deref(), Some("1:2.1.4-1"));
        assert_eq!(c[2].to.as_deref(), Some("1:2.1.5-2"));
        assert_eq!(c[5].to, None);
        assert_eq!(c[0].from, None);
        // A purge of config files alone isn't a package change; a purge
        // after a removal is the same removal.
        assert!(!c.iter().any(|x| x.name.starts_with("old-config")));
        assert_eq!(
            crate::pacman::summary(&c),
            "installed libnl-genl-3-200:amd64, htop:amd64; upgraded libjpeg62-turbo:amd64, reeve-t-lib:all; removed nano:amd64; reinstalled bash:amd64"
        );
    }

    #[test]
    fn an_undo_is_one_apt_get_with_old_files_and_new_names() {
        let p = undo_plan(
            &parse(LOG_TEXT),
            Path::new("/var/cache/apt/archives"),
            |_| cache(),
        );
        assert_eq!(p.remove, ["htop:amd64", "libnl-genl-3-200:amd64"]);
        assert_eq!(
            p.restore
                .iter()
                .map(|r| (r.name.as_str(), r.version.as_str(), r.file.is_some()))
                .collect::<Vec<_>>(),
            [
                ("libjpeg62-turbo:amd64", "1:2.1.4-1", true),
                ("nano:amd64", "7.2-1+deb12u1", true),
                ("reeve-t-lib:all", "2", true),
            ]
        );
        assert!(p.missing().is_empty());
        assert_eq!(
            p.command().unwrap(),
            "sudo apt-get install -y --allow-downgrades -o Dpkg::Options::=--force-confold \
             /var/cache/apt/archives/libjpeg62-turbo_1%3a2.1.4-1_amd64.deb \
             /var/cache/apt/archives/nano_7.2-1+deb12u1_amd64.deb \
             /var/cache/apt/archives/reeve-t-lib_2_all.deb \
             htop:amd64- libnl-genl-3-200:amd64-"
        );
        assert!(
            p.check()
                .unwrap()
                .starts_with("apt-get -s -q install -y --allow-downgrades")
        );
        assert_eq!(UndoPlan::default().command(), None);
        assert_eq!(UndoPlan::default().check(), None);
    }

    #[test]
    fn a_version_the_cache_lost_is_asked_for_by_name() {
        let p = undo_plan(&parse(LOG_TEXT), Path::new("/c"), |_| vec![]);
        assert_eq!(
            p.missing(),
            [
                "libjpeg62-turbo:amd64 1:2.1.4-1",
                "nano:amd64 7.2-1+deb12u1",
                "reeve-t-lib:all 2"
            ]
        );
        let cmd = p.command().unwrap();
        assert!(
            cmd.contains(" libjpeg62-turbo:amd64=1:2.1.4-1 ") && cmd.contains(" reeve-t-lib=2 "),
            "{cmd}"
        );
    }

    #[test]
    fn a_package_changed_twice_goes_back_to_where_it_started() {
        let text = "\
2026-10-11 02:00:00 upgrade foo:amd64 1.0-1 1.1-1
2026-10-11 02:00:01 upgrade foo:amd64 1.1-1 1.2-1
2026-10-11 02:00:02 install bar:amd64 <none> 2.0-1
2026-10-11 02:00:03 remove bar:amd64 2.0-1 <none>
2026-10-11 02:00:04 remove baz:amd64 3.0-1 <none>
2026-10-11 02:00:05 install baz:amd64 <none> 3.0-1
";
        let p = undo_plan(&parse(text), Path::new("/c"), |_| {
            vec!["foo_1.0-1_amd64.deb".to_string()]
        });
        assert_eq!(p.restore.len(), 1);
        assert_eq!(
            p.restore[0].file.as_deref(),
            Some(Path::new("/c/foo_1.0-1_amd64.deb"))
        );
        // bar came and went; baz went and came back as it was.
        assert!(p.remove.is_empty(), "{p:?}");
    }

    #[test]
    fn a_simulation_says_what_else_would_go() {
        let out = "\
NOTE: This is only a simulation!
      apt-get needs root privileges for real execution.
Reading package lists...
The following packages will be REMOVED:
  reeve-t-dep reeve-t-user
Inst reeve-t-lib [3] (2 local-deb [all])
Remv reeve-t-user [1]
Remv reeve-t-dep [1]
Remv libfoo:i386 [1]
Conf reeve-t-lib (2 local-deb [all])
";
        let s = simulated(out);
        assert_eq!(s.inst, ["reeve-t-lib"]);
        assert_eq!(s.remv, ["reeve-t-user", "reeve-t-dep", "libfoo"]);
        assert!(s.errors.is_empty());
        let plan = UndoPlan {
            restore: vec![],
            remove: vec!["reeve-t-dep:all".into(), "libfoo:i386".into()],
        };
        assert_eq!(unplanned_removals(&plan, &s), ["reeve-t-user"]);
        let refused =
            simulated("Reading package lists...\nE: Version '2' for 'reeve-t-lib' was not found\n");
        assert_eq!(
            refused.errors,
            ["Version '2' for 'reeve-t-lib' was not found"]
        );
    }

    #[test]
    fn paths_follow_dpkg_and_apt_config() {
        let p = Paths::from_conf(
            "# dpkg configuration\nno-debsig\nlog /var/log/my-dpkg.log  # here\n",
            Some("CACHE='/srv/apt/archives/'\n"),
        );
        assert_eq!(p.log, PathBuf::from("/var/log/my-dpkg.log"));
        assert_eq!(p.cache, PathBuf::from("/srv/apt/archives"));
        let d = Paths::from_conf("", None);
        assert_eq!((d.log.to_str(), d.cache.to_str()), (Some(LOG), Some(CACHE)));
    }

    #[test]
    fn only_what_came_after_the_mark_counts() {
        let d = tempfile::tempdir().unwrap();
        let log = d.path().join("dpkg.log");
        fs::write(&log, "2026-10-11 02:00:00 install old:amd64 <none> 1\n").unwrap();
        let m = mark(&log);
        let mut f = fs::OpenOptions::new().append(true).open(&log).unwrap();
        std::io::Write::write_all(&mut f, b"2026-10-11 02:00:01 install new:amd64 <none> 2\n")
            .unwrap();
        let c = changes_since(&log, m);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].name, "new:amd64");
        // A log that doesn't exist yet (a fresh system) has no changes.
        assert_eq!(mark(&d.path().join("none")), 0);
        assert!(changes_since(&d.path().join("none"), 0).is_empty());
    }
}
