//! Which Linux this is, and how to speak to its package manager.
//!
//! Fedora first (dnf5, with `dnf history undo`), then Arch (pacman, an AUR
//! helper if present). Image-based systems (Silverblue, Kinoite, …) are
//! recognized and package changes are refused: they need rpm-ostree.

use std::path::Path;

/// A distribution family.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    /// Fedora and relatives (RHEL, CentOS Stream, Nobara…).
    Fedora {
        /// `dnf5` or `dnf`.
        dnf: String,
        /// rpm-ostree based: packages are layered, not installed.
        atomic: bool,
    },
    /// Arch and relatives (Omarchy, EndeavourOS, Manjaro…).
    Arch {
        /// `paru` or `yay`, when installed.
        aur: Option<String>,
    },
    /// Anything else: package tools refuse, `shell` still works.
    Other(String),
}

fn has(bin: &str) -> bool {
    std::env::var("PATH")
        .unwrap_or_else(|_| "/usr/bin:/bin".into())
        .split(':')
        .any(|d| Path::new(d).join(bin).is_file())
}

impl Distro {
    /// From `/etc/os-release` and what's installed.
    pub fn detect() -> Self {
        let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        Self::from_os_release(&os, has, Path::new("/run/ostree-booted").exists())
    }

    fn from_os_release(os: &str, has: impl Fn(&str) -> bool, ostree: bool) -> Self {
        let field = |k: &str| {
            os.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
                .map(|v| v.trim_matches('"').to_ascii_lowercase())
                .unwrap_or_default()
        };
        let id = field("ID");
        let like = field("ID_LIKE");
        let family = |f: &str| id == f || like.split_whitespace().any(|l| l == f);
        if family("fedora") || family("rhel") {
            Self::Fedora {
                dnf: if has("dnf5") {
                    "dnf5".into()
                } else {
                    "dnf".into()
                },
                atomic: ostree,
            }
        } else if family("arch") {
            Self::Arch {
                aur: ["paru", "yay"]
                    .into_iter()
                    .find(|b| has(b))
                    .map(String::from),
            }
        } else {
            Self::Other(if id.is_empty() { "unknown".into() } else { id })
        }
    }

    /// One word for prompts and errors.
    pub fn name(&self) -> String {
        match self {
            Self::Fedora { atomic: true, .. } => "Fedora (atomic/rpm-ostree)".into(),
            Self::Fedora { .. } => "Fedora".into(),
            Self::Arch { .. } => "Arch".into(),
            Self::Other(id) => id.clone(),
        }
    }

    /// Why package tools won't work here, if they won't.
    pub fn package_block(&self) -> Option<String> {
        match self {
            Self::Fedora { atomic: true, .. } => Some(
                "this is an image-based (rpm-ostree) system: packages are layered with rpm-ostree, \
                 and Reeve's package tools don't do that yet. Prefer Flatpak or toolbox, or use shell."
                    .into(),
            ),
            Self::Other(id) => Some(format!("Reeve's package tools don't support {id} yet; use shell")),
            _ => None,
        }
    }

    /// Search by name and summary.
    pub fn search(&self, q: &str) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("{dnf} -q search {}", quote(q)),
            Self::Arch { .. } => format!("pacman -Ss {}", quote(q)),
            Self::Other(_) => String::new(),
        }
    }

    /// Details for one package.
    pub fn info(&self, name: &str) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("{dnf} -q info {}", quote(name)),
            Self::Arch { .. } => format!(
                "pacman -Qi {n} 2>/dev/null || pacman -Si {n}",
                n = quote(name)
            ),
            Self::Other(_) => String::new(),
        }
    }

    /// Installed packages (optionally filtered), user-installed ones, or pending updates.
    pub fn list(&self, which: &str, filter: Option<&str>) -> String {
        let grep = filter
            .map(|f| format!(" | grep -i -- {}", quote(f)))
            .unwrap_or_default();
        let base = match (self, which) {
            (Self::Fedora { dnf, .. }, "updates") => format!("{dnf} -q check-upgrade; true"),
            (Self::Fedora { dnf, .. }, "user") => {
                format!("{dnf} -q repoquery --userinstalled --qf '%{{name}}\\n'")
            }
            (Self::Fedora { dnf, .. }, "leaves") => format!("{dnf} -q leaves"),
            (Self::Fedora { .. }, _) => {
                "rpm -qa --qf '%{NAME} %{VERSION}-%{RELEASE} %{SIZE}\\n' | sort".into()
            }
            (Self::Arch { .. }, "updates") => "checkupdates 2>/dev/null || pacman -Qu; true".into(),
            (Self::Arch { .. }, "user") => "pacman -Qe".into(),
            (Self::Arch { .. }, "leaves") => "pacman -Qdtq; true".into(),
            (Self::Arch { .. }, _) => "pacman -Q".into(),
            (Self::Other(_), _) => String::new(),
        };
        format!("{base}{grep}")
    }

    /// Install (root).
    pub fn install(&self, pkgs: &[String]) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("sudo {dnf} install -y {}", quote_all(pkgs)),
            Self::Arch { .. } => format!("sudo pacman -S --needed --noconfirm {}", quote_all(pkgs)),
            Self::Other(_) => String::new(),
        }
    }

    /// Remove (root).
    pub fn remove(&self, pkgs: &[String]) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("sudo {dnf} remove -y {}", quote_all(pkgs)),
            Self::Arch { .. } => format!("sudo pacman -Rs --noconfirm {}", quote_all(pkgs)),
            Self::Other(_) => String::new(),
        }
    }

    /// Upgrade everything, or the named packages (root).
    pub fn upgrade(&self, pkgs: &[String]) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("sudo {dnf} upgrade -y {}", quote_all(pkgs))
                .trim_end()
                .to_string(),
            Self::Arch { .. } if pkgs.is_empty() => "sudo pacman -Syu --noconfirm".into(),
            Self::Arch { .. } => format!("sudo pacman -S --noconfirm {}", quote_all(pkgs)),
            Self::Other(_) => String::new(),
        }
    }

    /// Recent package transactions.
    pub fn history(&self, limit: usize) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("{dnf} history list | head -n {}", limit + 1),
            Self::Arch { .. } => format!(
                "grep -E '\\[ALPM\\] (installed|removed|upgraded)' /var/log/pacman.log | tail -n {limit}"
            ),
            Self::Other(_) => String::new(),
        }
    }

    /// The newest transaction id (for undo), as a command whose stdout is the id.
    pub fn last_transaction(&self) -> Option<String> {
        match self {
            Self::Fedora { dnf, .. } => Some(format!(
                "{dnf} history list 2>/dev/null | awk 'NR==2 {{print $1}}'"
            )),
            _ => None,
        }
    }

    /// Roll a transaction back (root).
    pub fn undo_transaction(&self, id: u64) -> Option<String> {
        match self {
            Self::Fedora { dnf, .. } => Some(format!("sudo {dnf} history undo -y {id}")),
            _ => None,
        }
    }
}

/// Single-quote for bash.
pub fn quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.:/+=@%,".contains(c))
    {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn quote_all(v: &[String]) -> String {
    v.iter().map(|s| quote(s)).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families() {
        let fedora = "NAME=\"Fedora Linux\"\nID=fedora\nVERSION_ID=44\n";
        assert_eq!(
            Distro::from_os_release(fedora, |b| b == "dnf5", false),
            Distro::Fedora {
                dnf: "dnf5".into(),
                atomic: false
            }
        );
        let omarchy = "NAME=\"Arch Linux\"\nID=arch\n";
        assert_eq!(
            Distro::from_os_release(omarchy, |b| b == "yay", false),
            Distro::Arch {
                aur: Some("yay".into())
            }
        );
        let endeavour = "ID=endeavouros\nID_LIKE=arch\n";
        assert!(matches!(
            Distro::from_os_release(endeavour, |_| false, false),
            Distro::Arch { .. }
        ));
        let silverblue = "ID=fedora\nVARIANT_ID=silverblue\n";
        assert!(
            Distro::from_os_release(silverblue, |_| true, true)
                .package_block()
                .is_some()
        );
    }

    #[test]
    fn commands_quote_their_arguments() {
        let d = Distro::Fedora {
            dnf: "dnf5".into(),
            atomic: false,
        };
        assert_eq!(
            d.install(&["htop".into(), "a b".into()]),
            "sudo dnf5 install -y htop 'a b'"
        );
        assert_eq!(quote("it's"), "'it'\\''s'");
        assert_eq!(
            d.undo_transaction(74).unwrap(),
            "sudo dnf5 history undo -y 74"
        );
    }
}
