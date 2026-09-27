//! Installing `reeved` as a systemd user service.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The unit file's path.
pub fn unit_path() -> PathBuf {
    dirs_config().join("systemd/user/reeved.service")
}

fn dirs_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/".into())).join(".config")
        })
}

/// The unit, running `<exe> daemon run`.
pub fn unit_text(exe: &Path, reeve_home: Option<&Path>) -> String {
    let env = reeve_home
        .map(|h| format!("Environment=REEVE_HOME={}\n", h.display()))
        .unwrap_or_default();
    format!(
        "# Written by `reeve daemon install`.\n\
         [Unit]\n\
         Description=Reeve observer: watches this machine, never changes it on its own\n\
         After=graphical-session.target\n\n\
         [Service]\n\
         ExecStart={} daemon run\n\
         {env}Restart=on-failure\n\
         RestartSec=15\n\
         Nice=10\n\
         IOSchedulingClass=idle\n\n\
         [Install]\n\
         WantedBy=default.target\n",
        exe.display()
    )
}

fn systemctl(args: &[&str]) -> Result<String, String> {
    let o = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| format!("couldn't run systemctl: {e}"))?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// Where packages and `install.sh` put the unit.
pub const PACKAGED: &[&str] = &[
    "/usr/lib/systemd/user",
    "/usr/local/lib/systemd/user",
    "/etc/systemd/user",
];

/// A unit installed by a package or `install.sh` (not by `reeve daemon install`).
pub fn packaged_unit() -> Option<PathBuf> {
    let user_share = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("systemd/user");
    PACKAGED
        .iter()
        .map(PathBuf::from)
        .chain(std::iter::once(user_share))
        .map(|d| d.join("reeved.service"))
        .find(|p| p.is_file())
}

/// The binary a unit file runs.
pub fn unit_exec(unit: &Path) -> Option<PathBuf> {
    std::fs::read_to_string(unit)
        .ok()?
        .lines()
        .find_map(|l| l.strip_prefix("ExecStart="))
        .and_then(|l| l.split_whitespace().next())
        .map(PathBuf::from)
}

/// Enable and start the service: the packaged unit when there is one,
/// otherwise a unit written to `~/.config/systemd/user` for this binary.
pub fn install(exe: &Path, reeve_home: Option<&Path>) -> Result<String, String> {
    if let Some(unit) = packaged_unit() {
        if reeve_home.is_none() {
            systemctl(&["daemon-reload"])?;
            systemctl(&["enable", "--now", "reeved.service"])?;
            return Ok(format!(
                "reeved started from the packaged unit ({})",
                unit.display()
            ));
        }
    }
    let p = unit_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, unit_text(exe, reeve_home)).map_err(|e| e.to_string())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", "reeved.service"])?;
    Ok(format!("reeved installed ({}) and started", p.display()))
}

/// Stop and disable the service, and remove the unit if `reeve daemon
/// install` wrote it. A packaged unit stays (the package owns it).
pub fn uninstall() -> Result<String, String> {
    let _ = systemctl(&["disable", "--now", "reeved.service"]);
    let p = unit_path();
    let mut removed = false;
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
        removed = true;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok(if removed || packaged_unit().is_none() {
        "reeved stopped and removed".into()
    } else {
        "reeved stopped and disabled (the packaged unit stays; your package manager owns it)".into()
    })
}

/// `active`, `inactive`, `failed`, or `not installed`.
pub fn state() -> String {
    if !unit_path().exists() && packaged_unit().is_none() {
        return "not installed".into();
    }
    systemctl(&["is-active", "reeved.service"])
        .unwrap_or_else(|e| if e.is_empty() { "inactive".into() } else { e })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_packaged_unit_parses() {
        let dir = tempfile::tempdir().unwrap();
        let unit = dir.path().join("reeved.service");
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../packaging/systemd/reeved.service"
            ),
            &unit,
        )
        .unwrap();
        assert_eq!(unit_exec(&unit), Some(PathBuf::from("/usr/bin/reeve")));
        assert!(
            std::fs::read_to_string(&unit)
                .unwrap()
                .contains("Reeve observer"),
            "install.sh finds units by this mark"
        );
    }

    #[test]
    fn the_unit_runs_the_daemon_quietly() {
        let u = unit_text(Path::new("/usr/local/bin/reeve"), Some(Path::new("/tmp/h")));
        assert!(u.contains("ExecStart=/usr/local/bin/reeve daemon run"));
        assert!(u.contains("Environment=REEVE_HOME=/tmp/h") && u.contains("Nice=10"));
    }
}
