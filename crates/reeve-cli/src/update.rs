//! `reeve update`: install a newer Reeve the way this one was installed, with
//! a receipt.
//!
//! - Copied by install.sh: download the release tarball, check it against the
//!   release's `SHA256SUMS`, and run *that release's* installer over this copy.
//! - An RPM or a .deb from a repository (a distribution's build, signed its
//!   way): updated through that repository, by version, so the release's own
//!   package never replaces it; if the repository doesn't have the version,
//!   nothing changes.
//! - An RPM or .deb installed by hand: download the release's, check it, and
//!   hand it to dnf or apt-get.
//! - A pacman package: hand off to the AUR helper.
//! - A cargo build: refuse, and say how to rebuild.
//!
//! Rolling back installs the version the last update replaced, from its own
//! release, checked the same way. Nothing kept in your home is ever copied
//! into a system directory.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use reeve_core::distro::Distro;
use reeve_core::policy::Tier;
use reeve_core::receipts::{Receipt, ReceiptBook, Status};
use reeve_core::update::{self, Install, UpdateState, Version};

/// What `reeve update` was asked to do.
pub struct Opts {
    /// Only say whether a newer release is out.
    pub check: bool,
    /// Install this version instead of the newest.
    pub version: Option<String>,
    /// Go back to the version the last update replaced.
    pub rollback: bool,
    /// Don't ask.
    pub yes: bool,
}

/// How the new version gets in.
enum Kind {
    /// The release's install.sh, from its verified tarball.
    Installer {
        prefix: PathBuf,
        user: bool,
        has_unit: bool,
    },
    /// The release RPM, through dnf.
    Rpm { dnf: String, downgrade: bool },
    /// The release .deb, through apt-get.
    Deb { downgrade: bool },
    /// The repository the package came from, through its package manager.
    Repo {
        /// `dnf5 upgrade -y reeve-0.8.1`, run with sudo.
        command: Vec<String>,
        repo: String,
    },
    /// The AUR helper.
    Aur { helper: String, package: String },
}

struct Plan {
    /// For the receipt: `install.sh (/usr/local)`, `rpm`, `rpm (kinetic)`,
    /// `deb`, `deb (repo.example.org)`, `aur`.
    method: String,
    tier: Tier,
    /// What will happen, in plain words, before asking.
    describe: String,
    kind: Kind,
}

pub fn run(home: &Path, opts: Opts) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    // /proc/self belongs to the effective user.
    if fs::metadata("/proc/self").is_ok_and(|m| m.uid() == 0) {
        return Err("run `reeve update` as yourself; it asks for sudo when it needs it".into());
    }
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let running = Version::current();

    // What to install.
    let latest = !opts.rollback && opts.version.is_none();
    let (to, notes) = if opts.rollback {
        let v = UpdateState::load(home)
            .previous
            .as_deref()
            .and_then(Version::parse)
            .ok_or("nothing to roll back to: `reeve update` hasn't replaced a version yet")?;
        (v, None)
    } else if let Some(s) = &opts.version {
        let v = Version::parse(s).ok_or_else(|| format!("{s:?} isn't a version like v0.4.0"))?;
        (v, None)
    } else {
        let answer = rt.block_on(update::latest());
        let seen = answer.clone();
        let _ = UpdateState::modify(home, |s| s.record(chrono::Utc::now(), seen));
        let r = answer?;
        if r.version <= running {
            println!("Reeve {running} is the newest release.");
            return Ok(());
        }
        (r.version, (!r.url.is_empty()).then_some(r.url))
    };
    if opts.check {
        println!("Reeve {to} is out; this is {running}. `reeve update` installs it.");
        if let Some(u) = &notes {
            println!("What's new: {u}");
        }
        return Ok(());
    }
    if to == running {
        println!("This is already Reeve {to}.");
        return Ok(());
    }

    let bin = std::env::current_exe()
        .and_then(|p| p.canonicalize())
        .map_err(|e| format!("can't tell where this reeve is: {e}"))?;
    let me = update::user_home().unwrap_or_default();
    let install = update::classify(&bin, &me, update::package_owner);
    let plan = plan(&install, &me, running, to, latest, &Distro::detect())?;
    println!("{}", plan.describe);
    if let Some(u) = &notes {
        println!("What's new: {u}");
    }
    if !opts.yes && !crate::confirm(&format!("Install Reeve {to}? [y/N] "))? {
        return Ok(());
    }

    let dir = home.join("update");
    let outcome = rt.block_on(execute(&plan, to, &dir));
    let _ = fs::remove_dir_all(&dir);

    // What's on disk decides what happened, whatever the installer said: a
    // failed install may already have replaced the binary, and then
    // --rollback must still know what to put back.
    let after = version_of(&bin);
    let changed = after.is_some_and(|v| v != running);
    if changed {
        let _ = UpdateState::modify(home, |s| {
            s.previous = Some(running.to_string());
            s.installed = after.map(|v| v.to_string());
        });
    }
    let aur = matches!(plan.kind, Kind::Aur { .. });
    let result = match (outcome, after) {
        (Err(e), _) => Err(e),
        (Ok(summary), Some(v)) if v == to => Ok(summary),
        // The AUR's package may trail the release.
        (Ok(_), Some(v)) if aur && v == running => Err(format!(
            "the AUR package is still {running}; it hasn't caught up with {to} yet"
        )),
        (Ok(summary), Some(v)) if aur => Ok(format!("{summary}; the AUR package is {v}")),
        (Ok(_), v) => Err(format!(
            "the install finished, but {} reports {}",
            bin.display(),
            v.map_or("no version".into(), |v| v.to_string())
        )),
    };
    let back = match &plan.kind {
        Kind::Aur { package, .. } => format!(
            "To go back, install {running} from the pacman cache: ls /var/cache/pacman/pkg/{package}-*, then sudo pacman -U <file>."
        ),
        _ => format!("`reeve update --rollback` goes back to {running}."),
    };

    let mut r = Receipt::draft(
        "cli",
        "reeve_update",
        serde_json::json!({
            "name": format!("reeve {running} → {to}"),
            "method": plan.method,
            "binary": bin.display().to_string(),
        }),
        plan.tier,
    );
    r.approved_by = "user".into();
    r.why = Some(String::from(if opts.rollback {
        "roll back the last update"
    } else {
        "update Reeve"
    }));
    match &result {
        Ok(s) => r.outcome.summary = s.clone(),
        Err(e) => {
            r.outcome.status = Status::Error;
            r.outcome.summary = e.clone();
        }
    }
    let receipt = ReceiptBook::new(home)
        .append(r)
        .map_err(|e| e.to_string())?;
    let summary = match result {
        Ok(s) => s,
        Err(e) => {
            if let (true, Some(v)) = (changed, after) {
                eprintln!("{} is now Reeve {v}. {back}", bin.display());
            }
            return Err(format!("{e} (receipt #{})", receipt.seq));
        }
    };
    println!("{summary} (receipt #{})", receipt.seq);
    restart_reeved(&bin);
    println!("{back} Open Reeve windows keep running {running} until you restart them.");
    Ok(())
}

fn plan(
    install: &Install,
    me: &Path,
    running: Version,
    to: Version,
    latest: bool,
    distro: &Distro,
) -> Result<Plan, String> {
    let head = format!("Reeve {running} → {to}");
    match install {
        Install::Source(p) => Err(format!(
            "{} was built from source; update the checkout and rebuild it (cargo install --path crates/reeve-cli)",
            p.display()
        )),
        Install::Unknown(p) => Err(format!(
            "{} isn't where an installer puts Reeve, so `reeve update` won't replace it. Reinstall with install.sh, or swap the binary yourself.",
            p.display()
        )),
        Install::Copied { bin, prefix, user } => {
            let data_home = std::env::var_os("XDG_DATA_HOME")
                .filter(|s| !s.is_empty())
                .map_or_else(|| me.join(".local/share"), PathBuf::from);
            let has_unit = update::unit_path(prefix, *user, &data_home).exists();
            let sudo = if *user {
                ""
            } else {
                ", using sudo to copy it into place"
            };
            Ok(Plan {
                method: format!("install.sh ({})", prefix.display()),
                tier: if *user { Tier::T1 } else { Tier::T2 },
                describe: format!(
                    "{head}\nDownload the release from {}, check it against SHA256SUMS, and run its installer over {}{sudo}.",
                    update::download_base(to),
                    bin.display()
                ),
                kind: Kind::Installer {
                    prefix: prefix.clone(),
                    user: *user,
                    has_unit,
                },
            })
        }
        Install::Rpm { package, repo } => {
            // Image-based Fedora layers packages with rpm-ostree; the package
            // tools refuse there, and so does this.
            if let Some(why) = distro.package_block() {
                return Err(format!("{package} is an RPM, but {why}"));
            }
            let dnf = match distro {
                Distro::Fedora { dnf, .. } => dnf.clone(),
                _ => "dnf".into(),
            };
            let downgrade = to < running;
            let verb = if downgrade { "downgrade" } else { "upgrade" };
            if let Some(repo) = repo {
                // A repository's build stays the one installed: the version
                // comes from the repository, signed its way, or nothing changes.
                let spec = format!("{package}-{to}");
                return Ok(Plan {
                    method: format!("rpm ({repo})"),
                    tier: Tier::T2,
                    describe: format!(
                        "{head} ({package} comes from the {repo} repository)\nRun: sudo {dnf} {verb} -y {spec}. If {repo} doesn't have {to} yet, nothing changes."
                    ),
                    kind: Kind::Repo {
                        command: vec![dnf, verb.into(), "-y".into(), spec],
                        repo: repo.clone(),
                    },
                });
            }
            Ok(Plan {
                method: "rpm".into(),
                tier: Tier::T2,
                describe: format!(
                    "{head} ({package} is an RPM)\nDownload {} from {}, check it against SHA256SUMS, then run: sudo {dnf} {verb} -y <that file>",
                    update::rpm_name(to),
                    update::download_base(to)
                ),
                kind: Kind::Rpm { dnf, downgrade },
            })
        }
        Install::Deb { package, repo } => {
            let downgrade = to < running;
            let allow = if downgrade { " --allow-downgrades" } else { "" };
            if let Some(repo) = repo {
                // apt installs an exact version: the repository's build of
                // `to`, whatever revision it gave it.
                let Some(version) = repo
                    .versions
                    .iter()
                    .find(|v| **v == to.to_string() || v.starts_with(&format!("{to}-")))
                else {
                    let has = if repo.versions.is_empty() {
                        "nothing listed".to_string()
                    } else {
                        repo.versions.join(", ")
                    };
                    return Err(format!(
                        "{package} comes from the {} repository, which doesn't have {to} in what apt last fetched ({has}); run `sudo apt-get update` and try again, or wait for the repository",
                        repo.name
                    ));
                };
                let spec = format!("{package}={version}");
                let mut command = vec!["apt-get".to_string(), "install".into(), "-y".into()];
                if downgrade {
                    command.push("--allow-downgrades".into());
                }
                command.push(spec.clone());
                return Ok(Plan {
                    method: format!("deb ({})", repo.name),
                    tier: Tier::T2,
                    describe: format!(
                        "{head} ({package} comes from the {} repository)\nRun: sudo apt-get install -y{allow} {spec}",
                        repo.name
                    ),
                    kind: Kind::Repo {
                        command,
                        repo: repo.name.clone(),
                    },
                });
            }
            Ok(Plan {
                method: "deb".into(),
                tier: Tier::T2,
                describe: format!(
                    "{head} ({package} is a Debian package)\nDownload {} from {}, check it against SHA256SUMS, then run: sudo apt-get install -y{allow} <that file>",
                    update::deb_name(to),
                    update::download_base(to)
                ),
                kind: Kind::Deb { downgrade },
            })
        }
        Install::Pacman { package } => {
            if !latest {
                return Err(format!(
                    "{package} comes from pacman, which installs a chosen version from its cache: ls /var/cache/pacman/pkg/{package}-*, then sudo pacman -U <file>"
                ));
            }
            let helper = match distro {
                Distro::Arch { aur } => aur.clone(),
                _ => None,
            };
            let Some(helper) = helper else {
                return Err(format!(
                    "{package} comes from pacman, and there's no AUR helper (paru or yay). Build the release's PKGBUILD with makepkg -si: {}/PKGBUILD",
                    update::download_base(to)
                ));
            };
            Ok(Plan {
                method: "aur".into(),
                tier: Tier::T2,
                describe: format!(
                    "{head} ({package} comes from pacman)\nRun: {helper} -S {package}"
                ),
                kind: Kind::Aur {
                    helper,
                    package: package.clone(),
                },
            })
        }
    }
}

async fn execute(plan: &Plan, to: Version, dir: &Path) -> Result<String, String> {
    match &plan.kind {
        Kind::Installer {
            prefix,
            user,
            has_unit,
        } => {
            let target = update::target().ok_or("there's no prebuilt Reeve for this CPU")?;
            let asset = update::tarball_name(target);
            let tar = fetch_checked(dir, to, &asset).await?;
            status(
                Command::new("tar").arg("-C").arg(dir).arg("-xzf").arg(&tar),
                "tar",
            )?;
            let src = dir.join(format!("reeve-{target}"));
            match version_of(&src.join("reeve")) {
                Some(v) if v == to => {}
                v => {
                    return Err(format!(
                        "the {to} release's archive holds {}",
                        v.map_or("no working reeve".into(), |v| format!("Reeve {v}"))
                    ));
                }
            }
            let args = update::installer_args(&tar, prefix, *user, *has_unit);
            status(
                Command::new("sh").arg(src.join("install.sh")).args(&args),
                "the installer",
            )?;
            Ok(format!(
                "Reeve {to} installed in {}",
                prefix.join("bin").display()
            ))
        }
        Kind::Rpm { dnf, downgrade } => {
            let rpm = fetch_checked(dir, to, &update::rpm_name(to)).await?;
            let verb = if *downgrade { "downgrade" } else { "upgrade" };
            status(
                Command::new("sudo")
                    .args([dnf.as_str(), verb, "-y"])
                    .arg(&rpm),
                dnf,
            )?;
            Ok(format!("Reeve {to} installed with {dnf}"))
        }
        Kind::Deb { downgrade } => {
            let deb = fetch_checked(dir, to, &update::deb_name(to)).await?;
            let mut cmd = Command::new("sudo");
            cmd.args(["apt-get", "install", "-y"]);
            if *downgrade {
                cmd.arg("--allow-downgrades");
            }
            status(cmd.arg(&deb), "apt-get")?;
            Ok(format!("Reeve {to} installed with apt-get"))
        }
        Kind::Repo { command, repo } => {
            let tool = command
                .first()
                .map(String::as_str)
                .unwrap_or("the package manager");
            let mut cmd = Command::new("sudo");
            cmd.args(command);
            status(&mut cmd, tool).map_err(|e| {
                format!(
                    "{e}; the {repo} repository may not have Reeve {to} yet, in which case nothing changed"
                )
            })?;
            Ok(format!(
                "Reeve {to} installed from the {repo} repository with {tool}"
            ))
        }
        Kind::Aur { helper, package } => {
            status(Command::new(helper).args(["-S", package.as_str()]), helper)?;
            Ok(format!("{package} reinstalled with {helper}"))
        }
    }
}

/// Download the release's `SHA256SUMS` and its signature into `dir`, refuse
/// them unless Reeve's release key signed them, then download `name` and
/// refuse it unless it matches.
async fn fetch_checked(dir: &Path, to: Version, name: &str) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let base = update::download_base(to);
    let sums = dir.join("SHA256SUMS");
    let sig = dir.join("SHA256SUMS.sig");
    let file = dir.join(name);
    println!("downloading {name}…");
    update::download(&format!("{base}/SHA256SUMS"), &sums).await?;
    update::download(&format!("{base}/SHA256SUMS.sig"), &sig)
        .await
        .map_err(|e| {
            format!(
                "the {to} release isn't signed ({e}); releases before 0.8.0 aren't, and nothing was installed"
            )
        })?;
    let listed = fs::read(&sums).map_err(|e| e.to_string())?;
    let sig = fs::read_to_string(&sig).map_err(|e| e.to_string())?;
    update::signed_sums(&listed, &sig)?;
    println!("SHA256SUMS is signed with Reeve's release key");
    update::download(&format!("{base}/{name}"), &file).await?;
    update::verify(&file, &String::from_utf8_lossy(&listed))?;
    println!("{name} matches the release's SHA256SUMS");
    Ok(file)
}

/// Run with the terminal attached (sudo and the installers talk to you).
fn status(cmd: &mut Command, what: &str) -> Result<(), String> {
    let st = cmd
        .status()
        .map_err(|e| format!("couldn't run {what}: {e}"))?;
    if st.success() {
        Ok(())
    } else {
        Err(format!("{what} failed (its output is above)"))
    }
}

/// What `<bin> --version` says.
fn version_of(bin: &Path) -> Option<Version> {
    let o = Command::new(bin)
        .arg("--version")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&o.stdout)
        .split_whitespace()
        .last()
        .and_then(Version::parse)
}

/// Restart reeved when it runs the binary that was just replaced. A reeved
/// running some other copy is left alone, and so is one that's stopped.
fn restart_reeved(bin: &Path) {
    let sc = |args: &[&str]| {
        Command::new("systemctl")
            .arg("--user")
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
    };
    let Some(o) = sc(&[
        "show",
        "reeved.service",
        "-p",
        "ActiveState",
        "-p",
        "ExecStart",
    ]) else {
        return;
    };
    let out = String::from_utf8_lossy(&o.stdout);
    if !out.lines().any(|l| l == "ActiveState=active") {
        return;
    }
    if !out.contains(&format!("path={} ", bin.display())) {
        println!("reeved runs another copy of Reeve, so it was left alone.");
        return;
    }
    let _ = sc(&["daemon-reload"]);
    match sc(&["restart", "reeved.service"]) {
        Some(o) if o.status.success() => println!("reeved restarted on the new version."),
        _ => println!("couldn't restart reeved; run: systemctl --user restart reeved"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reeve_core::update::AptRepo;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn fedora() -> Distro {
        Distro::Fedora {
            dnf: "dnf5".into(),
            atomic: false,
        }
    }

    #[test]
    fn a_package_from_a_repository_updates_through_it() {
        let me = Path::new("/home/me");
        let (running, to) = (v("0.8.0"), v("0.8.1"));
        let from_repo = Install::Rpm {
            package: "reeve".into(),
            repo: Some("kinetic".into()),
        };
        let p = plan(&from_repo, me, running, to, true, &fedora()).unwrap();
        assert_eq!(p.method, "rpm (kinetic)");
        assert!(p.describe.contains("kinetic repository"), "{}", p.describe);
        match &p.kind {
            Kind::Repo { command, repo } => {
                assert_eq!(command, &["dnf5", "upgrade", "-y", "reeve-0.8.1"]);
                assert_eq!(repo, "kinetic");
            }
            _ => panic!("not through the repository"),
        }
        // Going back goes back through the repository too.
        let back = plan(&from_repo, me, to, running, false, &fedora()).unwrap();
        assert!(
            matches!(&back.kind, Kind::Repo { command, .. } if command[1] == "downgrade" && command[3] == "reeve-0.8.0")
        );
        // A file installed by hand gets the release's own RPM, as before.
        let by_hand = Install::Rpm {
            package: "reeve".into(),
            repo: None,
        };
        let p = plan(&by_hand, me, running, to, true, &fedora()).unwrap();
        assert_eq!(p.method, "rpm");
        assert!(p.describe.contains(&update::rpm_name(to)), "{}", p.describe);
        assert!(matches!(p.kind, Kind::Rpm { .. }));

        let debian = Distro::Debian {
            id: "debian".into(),
        };
        let repo = AptRepo {
            name: "repo.example.org".into(),
            versions: vec!["0.8.1-1".into(), "0.7.2-1".into()],
        };
        let from_repo = Install::Deb {
            package: "reeve".into(),
            repo: Some(repo.clone()),
        };
        let p = plan(&from_repo, me, running, to, true, &debian).unwrap();
        assert_eq!(p.method, "deb (repo.example.org)");
        assert!(
            matches!(&p.kind, Kind::Repo { command, .. } if command == &["apt-get", "install", "-y", "reeve=0.8.1-1"])
        );
        // The repository's revision, whatever it is, and a downgrade allowed
        // only when going back.
        let back = plan(&from_repo, me, to, v("0.7.2"), false, &debian).unwrap();
        assert!(
            matches!(&back.kind, Kind::Repo { command, .. } if command == &["apt-get", "install", "-y", "--allow-downgrades", "reeve=0.7.2-1"])
        );
        // Not in the repository yet: nothing runs, and the reason says what is.
        let e = plan(&from_repo, me, running, v("0.9.0"), true, &debian)
            .err()
            .expect("not in the repository");
        assert!(
            e.contains("doesn't have 0.9.0") && e.contains("0.8.1-1, 0.7.2-1"),
            "{e}"
        );
        let by_hand = Install::Deb {
            package: "reeve".into(),
            repo: None,
        };
        let p = plan(&by_hand, me, running, to, true, &debian).unwrap();
        assert_eq!(p.method, "deb");
        assert!(p.describe.contains(&update::deb_name(to)), "{}", p.describe);
    }
}
