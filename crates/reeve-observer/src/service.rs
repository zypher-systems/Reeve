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

/// Write the unit and start it.
pub fn install(exe: &Path, reeve_home: Option<&Path>) -> Result<String, String> {
    let p = unit_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&p, unit_text(exe, reeve_home)).map_err(|e| e.to_string())?;
    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", "--now", "reeved.service"])?;
    Ok(format!("reeved installed ({}) and started", p.display()))
}

/// Stop, disable, and remove the unit.
pub fn uninstall() -> Result<String, String> {
    let _ = systemctl(&["disable", "--now", "reeved.service"]);
    let p = unit_path();
    if p.exists() {
        std::fs::remove_file(&p).map_err(|e| e.to_string())?;
    }
    let _ = systemctl(&["daemon-reload"]);
    Ok("reeved stopped and removed".into())
}

/// `active`, `inactive`, `failed`, or `not installed`.
pub fn state() -> String {
    if !unit_path().exists() {
        return "not installed".into();
    }
    systemctl(&["is-active", "reeved.service"])
        .unwrap_or_else(|e| if e.is_empty() { "inactive".into() } else { e })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_runs_the_daemon_quietly() {
        let u = unit_text(Path::new("/usr/local/bin/reeve"), Some(Path::new("/tmp/h")));
        assert!(u.contains("ExecStart=/usr/local/bin/reeve daemon run"));
        assert!(u.contains("Environment=REEVE_HOME=/tmp/h") && u.contains("Nice=10"));
    }
}
