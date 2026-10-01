//! New releases: whether one is out, and how this copy got onto the machine,
//! so `reeve update` can install the next one the same way.
//!
//! The only question asked is GitHub's "latest release" for this repo, with
//! `reeve/<version>` as the user agent. The answer is kept in
//! `~/.reeve/observer/update.json`, which reeved and the TUI share.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Where releases come from.
pub const REPO: &str = "zypher-systems/reeve";

/// This build's version.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// How often reeved asks.
pub const EVERY: Duration = Duration::from_secs(12 * 3600);

/// How old an answer gets before the TUI asks itself (when reeved is down).
pub const STALE_HOURS: i64 = 24;

/// MAJOR.MINOR.PATCH. Pre-releases and anything else don't parse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `0.5.0` or `v0.5.0`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.strip_prefix('v').unwrap_or(s);
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        let n = |p: &str| -> Option<u32> {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        Some(Self(n(parts[0])?, n(parts[1])?, n(parts[2])?))
    }

    /// This build.
    pub fn current() -> Self {
        Self::parse(CURRENT).unwrap_or(Self(0, 0, 0))
    }

    /// The release tag, `v0.5.0`.
    pub fn tag(self) -> String {
        format!("v{self}")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// A published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// Its version.
    pub version: Version,
    /// Its page, with the notes.
    pub url: String,
}

/// Read GitHub's answer for the latest release. Drafts and pre-releases
/// never count.
pub fn parse_release(json: &str) -> std::result::Result<Release, String> {
    #[derive(Deserialize)]
    struct Gh {
        tag_name: String,
        #[serde(default)]
        html_url: String,
        #[serde(default)]
        draft: bool,
        #[serde(default)]
        prerelease: bool,
    }
    let gh: Gh =
        serde_json::from_str(json).map_err(|e| format!("unreadable answer from GitHub: {e}"))?;
    if gh.draft || gh.prerelease {
        return Err(format!("{} isn't a published release", gh.tag_name));
    }
    let version =
        Version::parse(&gh.tag_name).ok_or_else(|| format!("{:?} isn't a version", gh.tag_name))?;
    Ok(Release {
        version,
        url: gh.html_url,
    })
}

/// GitHub's latest-release endpoint, or `REEVE_RELEASES_API`.
fn api_url() -> String {
    std::env::var("REEVE_RELEASES_API")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("https://api.github.com/repos/{REPO}/releases/latest"))
}

/// Where a release's files are: `REEVE_DOWNLOAD_URL` (a mirror, as for
/// install.sh) or the release on GitHub.
pub fn download_base(v: Version) -> String {
    match std::env::var("REEVE_DOWNLOAD_URL") {
        Ok(u) if !u.is_empty() => u.trim_end_matches('/').to_string(),
        _ => format!("https://github.com/{REPO}/releases/download/{}", v.tag()),
    }
}

fn client(timeout: Duration) -> std::result::Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(format!("reeve/{CURRENT}"))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Ask GitHub for the newest release.
pub async fn latest() -> std::result::Result<Release, String> {
    let r = client(Duration::from_secs(30))?
        .get(api_url())
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("couldn't reach GitHub: {e}"))?;
    let status = r.status();
    let body = r.text().await.map_err(|e| e.to_string())?;
    if status.as_u16() == 404 {
        return Err("no published release yet".into());
    }
    if !status.is_success() {
        return Err(format!("GitHub answered {status}"));
    }
    parse_release(&body)
}

/// Download `url` to `dest`.
pub async fn download(url: &str, dest: &Path) -> std::result::Result<(), String> {
    let r = client(Duration::from_secs(600))?
        .get(url)
        .send()
        .await
        .map_err(|e| format!("couldn't download {url}: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("couldn't download {url}: {}", r.status()));
    }
    let bytes = r.bytes().await.map_err(|e| e.to_string())?;
    fs::write(dest, &bytes).map_err(|e| format!("{}: {e}", dest.display()))
}

/// The hash `SHA256SUMS` lists for `name` (`<hex>  name` or `<hex> *name`).
pub fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|l| {
        let (hash, rest) = l.split_once(char::is_whitespace)?;
        let file = rest.trim_start().trim_start_matches('*').trim_end();
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| hash.to_ascii_lowercase())
    })
}

/// Check a downloaded file against `SHA256SUMS`. A file that isn't listed
/// is refused, never waved through.
pub fn verify(file: &Path, sums: &str) -> std::result::Result<(), String> {
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let want =
        listed_sum(sums, &name).ok_or_else(|| format!("{name} isn't listed in SHA256SUMS"))?;
    let got = crate::undo::sha256_hex(&fs::read(file).map_err(|e| format!("{name}: {e}"))?);
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch for {name}: expected {want}, got {got}"
        ))
    }
}

/// What the TUI shows about updates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// A newer release is out.
    Available(Version),
    /// A newer version is already installed; this process predates it.
    Restart(Version),
}

/// The last answer, in `observer/update.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UpdateState {
    /// When it last asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked_at: Option<DateTime<Utc>>,
    /// The newest release it found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// That release's page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Why the last check failed. The last good answer stays.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The version on disk: what reeved runs, and what `reeve update` put there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    /// The version `reeve update` replaced, for `--rollback`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
}

impl UpdateState {
    /// The file.
    pub fn path(reeve_home: &Path) -> PathBuf {
        reeve_home.join("observer").join("update.json")
    }

    /// Read it (empty when there's none yet).
    pub fn load(reeve_home: &Path) -> Self {
        fs::read_to_string(Self::path(reeve_home))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    /// Read, change, and write it under a lock, so reeved's check and
    /// `reeve update` never drop each other's fields (the rollback target
    /// above all).
    pub fn modify(reeve_home: &Path, f: impl FnOnce(&mut Self)) -> Result<Self> {
        let dir = reeve_home.join("observer");
        fs::create_dir_all(&dir)?;
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(".update.lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(|e| Error::Io(format!("update lock: {e}")))?;
        let mut s = Self::load(reeve_home);
        f(&mut s);
        s.save(reeve_home)?;
        drop(lock);
        Ok(s)
    }

    /// Write it. Use [`UpdateState::modify`] to change what's there.
    pub fn save(&self, reeve_home: &Path) -> Result<()> {
        let p = Self::path(reeve_home);
        if let Some(d) = p.parent() {
            fs::create_dir_all(d)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| Error::Io(e.to_string()))?;
        crate::undo::write_atomic(&p, json.as_bytes(), Some(0o600))
    }

    /// The last answer is older than `max_age`, or there's none.
    pub fn due(&self, now: DateTime<Utc>, max_age: chrono::Duration) -> bool {
        self.checked_at.is_none_or(|t| now - t >= max_age)
    }

    /// Take one check's answer. A failure keeps the last good one.
    pub fn record(&mut self, now: DateTime<Utc>, answer: std::result::Result<Release, String>) {
        self.checked_at = Some(now);
        match answer {
            Ok(r) => {
                self.latest = Some(r.version.to_string());
                self.url = (!r.url.is_empty()).then_some(r.url);
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    /// What a process of version `running` should show.
    pub fn badge(&self, running: Version) -> Option<Badge> {
        let latest = self.latest.as_deref().and_then(Version::parse);
        let installed = self.installed.as_deref().and_then(Version::parse);
        // Already on disk, and nothing newer out: only a restart is missing.
        if let Some(i) = installed {
            if i > running && latest.is_none_or(|l| i >= l) {
                return Some(Badge::Restart(i));
            }
        }
        latest.filter(|l| *l > running).map(Badge::Available)
    }
}

/// Ask GitHub and record the answer. A failure is recorded, not returned.
pub async fn check(reeve_home: &Path) -> UpdateState {
    let answer = latest().await;
    UpdateState::modify(reeve_home, |s| s.record(Utc::now(), answer))
        .unwrap_or_else(|_| UpdateState::load(reeve_home))
}

/// The person's home directory, as the rest of Reeve finds it (from passwd
/// when `HOME` isn't set). `None` rather than an empty path.
pub fn user_home() -> Option<PathBuf> {
    dirs::home_dir().filter(|p| !p.as_os_str().is_empty())
}

/// How this copy of Reeve got onto the machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Install {
    /// Copied into `<prefix>/bin` by install.sh (or by hand). The release's
    /// own installer updates it.
    Copied {
        /// `<prefix>/bin/reeve`.
        bin: PathBuf,
        /// `<prefix>`.
        prefix: PathBuf,
        /// Under your home: no sudo, and the unit sits in your own systemd dir.
        user: bool,
    },
    /// Owned by an RPM.
    Rpm {
        /// Its name.
        package: String,
    },
    /// Owned by a pacman package.
    Pacman {
        /// Its name.
        package: String,
    },
    /// Built from source with cargo.
    Source(PathBuf),
    /// Somewhere no installer puts it.
    Unknown(PathBuf),
}

/// The package manager that owns a path, and the package.
pub type Owner = Option<(&'static str, String)>;

/// Work out how `bin` was installed. `home` is the person's home directory
/// (empty when there's none: then nothing counts as a home install);
/// `owner` asks the package managers.
pub fn classify(bin: &Path, home: &Path, owner: impl Fn(&Path) -> Owner) -> Install {
    let s = bin.to_string_lossy();
    // Every path starts with an empty one.
    let in_home = |p: &Path| !home.as_os_str().is_empty() && p.starts_with(home);
    if s.contains("/target/debug/")
        || s.contains("/target/release/")
        || in_home(bin) && bin.starts_with(home.join(".cargo"))
    {
        return Install::Source(bin.into());
    }
    if let Some((pm, package)) = owner(bin) {
        return if pm == "rpm" {
            Install::Rpm { package }
        } else {
            Install::Pacman { package }
        };
    }
    let Some(dir) = bin.parent() else {
        return Install::Unknown(bin.into());
    };
    if dir.file_name().is_none_or(|n| n != "bin") {
        return Install::Unknown(bin.into());
    }
    Install::Copied {
        bin: bin.into(),
        prefix: dir.parent().unwrap_or(Path::new("/")).to_path_buf(),
        user: in_home(bin),
    }
}

/// Ask rpm, then pacman, who owns `path`.
pub fn package_owner(path: &Path) -> Owner {
    let ask = |prog: &str, args: &[&str]| -> Option<String> {
        let o = std::process::Command::new(prog)
            .args(args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let out = String::from_utf8_lossy(&o.stdout);
        let first = out.lines().next()?.trim();
        (o.status.success() && !first.is_empty()).then(|| first.to_string())
    };
    if let Some(p) = ask("rpm", &["-qf", "--qf", "%{NAME}\\n"]) {
        return Some(("rpm", p));
    }
    ask("pacman", &["-Qoq"]).map(|p| ("pacman", p))
}

/// The unit install.sh writes for an install under `prefix` (its own rule).
pub fn unit_path(prefix: &Path, user: bool, data_home: &Path) -> PathBuf {
    if user {
        data_home.join("systemd/user/reeved.service")
    } else if prefix == Path::new("/usr/local") || prefix == Path::new("/usr") {
        prefix.join("lib/systemd/user/reeved.service")
    } else {
        PathBuf::from("/etc/systemd/user/reeved.service")
    }
}

/// install.sh's arguments to put `tarball` where this copy is. The unit is
/// refreshed only where install.sh put one before, and the installer never
/// starts reeved: `reeve update` restarts it if it was running.
pub fn installer_args(tarball: &Path, prefix: &Path, user: bool, has_unit: bool) -> Vec<String> {
    let mut a = vec![
        "--from".to_string(),
        tarball.display().to_string(),
        "--prefix".into(),
        prefix.display().to_string(),
        "--no-start".into(),
    ];
    if user {
        a.push("--user".into());
    }
    if !has_unit {
        a.push("--no-service".into());
    }
    a
}

/// This machine's release target, such as `x86_64-unknown-linux-musl`.
pub fn target() -> Option<&'static str> {
    match std::env::consts::ARCH {
        "x86_64" => Some("x86_64-unknown-linux-musl"),
        "aarch64" => Some("aarch64-unknown-linux-musl"),
        _ => None,
    }
}

/// The release tarball for `target`.
pub fn tarball_name(target: &str) -> String {
    format!("reeve-{target}.tar.gz")
}

/// The release RPM for this machine.
pub fn rpm_name(v: Version) -> String {
    format!("reeve-{v}-1.{}.rpm", std::env::consts::ARCH)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_three_numbers() {
        assert_eq!(Version::parse("0.5.0"), Some(Version(0, 5, 0)));
        assert_eq!(Version::parse("v1.10.2"), Some(Version(1, 10, 2)));
        for bad in [
            "",
            "1.2",
            "1.2.3.4",
            "0.5.0-rc1",
            "v",
            "a.b.c",
            "1..2",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert!(Version(1, 0, 0) > Version(0, 99, 99));
        assert_eq!(Version(0, 5, 0).tag(), "v0.5.0");
        assert!(
            Version::parse(CURRENT).is_some(),
            "the build's own version parses"
        );
    }

    #[test]
    fn only_published_releases_count() {
        let ok = r#"{"tag_name":"v0.5.0","html_url":"https://github.com/zypher-systems/reeve/releases/tag/v0.5.0","draft":false,"prerelease":false}"#;
        let r = parse_release(ok).unwrap();
        assert_eq!(r.version, Version(0, 5, 0));
        assert!(r.url.ends_with("/v0.5.0"));
        assert!(parse_release(r#"{"tag_name":"v0.6.0","prerelease":true}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"v0.6.0","draft":true}"#).is_err());
        assert!(parse_release(r#"{"tag_name":"nightly"}"#).is_err());
        assert!(parse_release("<html>").is_err());
    }

    #[test]
    fn checksums_must_match_and_be_listed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("reeve-x86_64-unknown-linux-musl.tar.gz");
        fs::write(&f, b"payload").unwrap();
        let sum = crate::undo::sha256_hex(b"payload");
        let sums = format!(
            "{sum}  reeve-x86_64-unknown-linux-musl.tar.gz\n{}  install.sh\n",
            "0".repeat(64)
        );
        assert_eq!(verify(&f, &sums), Ok(()));
        let star = format!("{sum} *reeve-x86_64-unknown-linux-musl.tar.gz\n");
        assert_eq!(verify(&f, &star), Ok(()));
        let wrong = format!(
            "{}  reeve-x86_64-unknown-linux-musl.tar.gz\n",
            "a".repeat(64)
        );
        assert!(verify(&f, &wrong).unwrap_err().contains("mismatch"));
        assert!(verify(&f, "").unwrap_err().contains("isn't listed"));
        // A name that only ends the same doesn't count.
        let other = format!("{sum}  old-reeve-x86_64-unknown-linux-musl.tar.gz\n");
        assert!(verify(&f, &other).is_err());
    }

    #[test]
    fn the_badge_says_update_or_restart() {
        let running = Version(0, 4, 0);
        let mut s = UpdateState::default();
        assert_eq!(s.badge(running), None);
        s.latest = Some("0.4.0".into());
        assert_eq!(s.badge(running), None, "same version: nothing");
        s.latest = Some("0.5.0".into());
        assert_eq!(s.badge(running), Some(Badge::Available(Version(0, 5, 0))));
        // Updated on disk while this window ran: restart.
        s.installed = Some("0.5.0".into());
        assert_eq!(s.badge(running), Some(Badge::Restart(Version(0, 5, 0))));
        // Installed is newer than this window, but still not the newest.
        s.latest = Some("0.6.0".into());
        assert_eq!(s.badge(running), Some(Badge::Available(Version(0, 6, 0))));
        // An older running reeved recorded what's installed; this is newer.
        s.installed = Some("0.3.0".into());
        s.latest = Some("0.4.0".into());
        assert_eq!(s.badge(running), None);
    }

    #[test]
    fn a_failed_check_keeps_the_last_answer() {
        let now = Utc::now();
        let mut s = UpdateState::default();
        assert!(s.due(now, chrono::Duration::hours(24)));
        s.record(
            now,
            Ok(Release {
                version: Version(0, 5, 0),
                url: "u".into(),
            }),
        );
        assert!(!s.due(now, chrono::Duration::hours(24)));
        assert!(s.due(
            now + chrono::Duration::hours(25),
            chrono::Duration::hours(24)
        ));
        s.record(now, Err("offline".into()));
        assert_eq!(s.latest.as_deref(), Some("0.5.0"));
        assert_eq!(s.error.as_deref(), Some("offline"));

        let d = tempfile::tempdir().unwrap();
        s.save(d.path()).unwrap();
        assert_eq!(UpdateState::load(d.path()), s);
        assert_eq!(
            UpdateState::load(&d.path().join("nowhere")),
            UpdateState::default()
        );
    }

    #[test]
    fn the_install_method_follows_the_path() {
        let home = Path::new("/home/me");
        let none = |_: &Path| None;
        assert_eq!(
            classify(Path::new("/usr/local/bin/reeve"), home, none),
            Install::Copied {
                bin: "/usr/local/bin/reeve".into(),
                prefix: "/usr/local".into(),
                user: false
            }
        );
        assert_eq!(
            classify(Path::new("/home/me/.local/bin/reeve"), home, none),
            Install::Copied {
                bin: "/home/me/.local/bin/reeve".into(),
                prefix: "/home/me/.local".into(),
                user: true
            }
        );
        let rpm = |_: &Path| Some(("rpm", "reeve".to_string()));
        assert_eq!(
            classify(Path::new("/usr/bin/reeve"), home, rpm),
            Install::Rpm {
                package: "reeve".into()
            }
        );
        let pac = |_: &Path| Some(("pacman", "reeve-bin".to_string()));
        assert_eq!(
            classify(Path::new("/usr/bin/reeve"), home, pac),
            Install::Pacman {
                package: "reeve-bin".into()
            }
        );
        // Source builds win even if something claims to own them.
        for p in [
            "/home/me/src/reeve/target/release/reeve",
            "/home/me/.cargo/bin/reeve",
        ] {
            assert_eq!(classify(Path::new(p), home, rpm), Install::Source(p.into()));
        }
        assert_eq!(
            classify(Path::new("/opt/tools/reeve"), home, none),
            Install::Unknown("/opt/tools/reeve".into())
        );
        // No home known: a system install stays a system install.
        assert_eq!(
            classify(Path::new("/usr/local/bin/reeve"), Path::new(""), none),
            Install::Copied {
                bin: "/usr/local/bin/reeve".into(),
                prefix: "/usr/local".into(),
                user: false
            }
        );
    }

    #[test]
    fn changes_to_the_state_keep_each_others_fields() {
        let d = tempfile::tempdir().unwrap();
        UpdateState::modify(d.path(), |s| s.previous = Some("0.4.0".into())).unwrap();
        let now = Utc::now();
        let after = UpdateState::modify(d.path(), |s| {
            s.record(
                now,
                Ok(Release {
                    version: Version(0, 6, 0),
                    url: String::new(),
                }),
            )
        })
        .unwrap();
        assert_eq!(after.previous.as_deref(), Some("0.4.0"));
        assert_eq!(after.latest.as_deref(), Some("0.6.0"));
        assert_eq!(UpdateState::load(d.path()), after);
    }

    #[test]
    fn the_installer_matches_the_install() {
        let data = Path::new("/home/me/.local/share");
        assert_eq!(
            unit_path(Path::new("/usr/local"), false, data),
            Path::new("/usr/local/lib/systemd/user/reeved.service")
        );
        assert_eq!(
            unit_path(Path::new("/home/me/.local"), true, data),
            Path::new("/home/me/.local/share/systemd/user/reeved.service")
        );
        assert_eq!(
            unit_path(Path::new("/opt/reeve"), false, data),
            Path::new("/etc/systemd/user/reeved.service")
        );
        let a = installer_args(
            Path::new("/t/r.tar.gz"),
            Path::new("/home/me/.local"),
            true,
            false,
        );
        assert_eq!(
            a,
            [
                "--from",
                "/t/r.tar.gz",
                "--prefix",
                "/home/me/.local",
                "--no-start",
                "--user",
                "--no-service"
            ]
        );
        let a = installer_args(
            Path::new("/t/r.tar.gz"),
            Path::new("/usr/local"),
            false,
            true,
        );
        assert_eq!(
            a,
            [
                "--from",
                "/t/r.tar.gz",
                "--prefix",
                "/usr/local",
                "--no-start"
            ]
        );
    }
}
