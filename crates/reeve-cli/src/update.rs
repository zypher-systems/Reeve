//! `reeve update`: install a newer Reeve the way this one was installed, with
//! a receipt.
//!
//! - Copied by install.sh: download the release tarball, check it against the
//!   release's `SHA256SUMS`, and run *that release's* installer over this copy.
//! - An RPM: download the release RPM, check it, and hand it to dnf.
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
    /// The AUR helper.
    Aur { helper: String, package: String },
}

struct Plan {
    /// For the receipt: `install.sh (/usr/local)`, `rpm`, `aur`.
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
        let mut st = UpdateState::load(home);
        st.record(chrono::Utc::now(), answer.clone());
        let _ = st.save(home);
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
    let me = PathBuf::from(std::env::var("HOME").unwrap_or_default());
    let install = update::classify(&bin, &me, update::package_owner);
    let plan = plan(&install, &me, running, to, latest)?;
    println!("{}", plan.describe);
    if let Some(u) = &notes {
        println!("What's new: {u}");
    }
    if !opts.yes && !crate::confirm(&format!("Install Reeve {to}? [y/N] "))? {
        return Ok(());
    }

    let dir = home.join("update");
    let result = rt.block_on(execute(&plan, to, &dir)).and_then(|summary| {
        match (version_of(&bin), &plan.kind) {
            (Some(v), _) if v == to => Ok(summary),
            // The AUR's package may trail the release.
            (Some(v), Kind::Aur { .. }) => Ok(format!("{summary}; the AUR package is {v}")),
            (v, _) => Err(format!(
                "the install finished, but {} reports {}",
                bin.display(),
                v.map_or("no version".into(), |v| v.to_string())
            )),
        }
    });
    let _ = fs::remove_dir_all(&dir);

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
    let summary = result.map_err(|e| format!("{e} (receipt #{})", receipt.seq))?;

    let installed = version_of(&bin).unwrap_or(to);
    let mut st = UpdateState::load(home);
    st.previous = Some(running.to_string());
    st.installed = Some(installed.to_string());
    let _ = st.save(home);
    println!("{summary} (receipt #{})", receipt.seq);
    restart_reeved(&bin);
    println!(
        "`reeve update --rollback` goes back to {running}. Open Reeve windows keep running {running} until you restart them."
    );
    Ok(())
}

fn plan(
    install: &Install,
    me: &Path,
    running: Version,
    to: Version,
    latest: bool,
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
        Install::Rpm { package } => {
            let dnf = match Distro::detect() {
                Distro::Fedora { dnf, .. } => dnf,
                _ => "dnf".into(),
            };
            let downgrade = to < running;
            let verb = if downgrade { "downgrade" } else { "upgrade" };
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
        Install::Pacman { package } => {
            if !latest {
                return Err(format!(
                    "{package} comes from pacman, which installs a chosen version from its cache: ls /var/cache/pacman/pkg/{package}-*, then sudo pacman -U <file>"
                ));
            }
            let helper = match Distro::detect() {
                Distro::Arch { aur } => aur,
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
        Kind::Aur { helper, package } => {
            status(Command::new(helper).args(["-S", package.as_str()]), helper)?;
            Ok(format!("{package} reinstalled with {helper}"))
        }
    }
}

/// Download `name` and the release's `SHA256SUMS` into `dir`, and refuse the
/// file unless it matches.
async fn fetch_checked(dir: &Path, to: Version, name: &str) -> Result<PathBuf, String> {
    let _ = fs::remove_dir_all(dir);
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let base = update::download_base(to);
    let sums = dir.join("SHA256SUMS");
    let file = dir.join(name);
    println!("downloading {name}…");
    update::download(&format!("{base}/SHA256SUMS"), &sums).await?;
    update::download(&format!("{base}/{name}"), &file).await?;
    let listed = fs::read_to_string(&sums).map_err(|e| e.to_string())?;
    update::verify(&file, &listed)?;
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
