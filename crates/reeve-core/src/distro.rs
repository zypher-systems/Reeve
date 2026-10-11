//! Which Linux this is, and how to speak to its package manager.
//!
//! Fedora first (dnf5, with `dnf history undo`), then Arch (pacman, an AUR
//! helper if present), then Debian and its family (apt-get and dpkg, with
//! undo from dpkg's log and apt's cache). Image-based systems (Silverblue,
//! Kinoite, …) are recognized and package changes are refused: they need
//! rpm-ostree.

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
    /// Debian and relatives (Ubuntu, Mint, Raspberry Pi OS…): apt and dpkg.
    Debian {
        /// `/etc/os-release`'s `ID`: `debian`, `ubuntu`, `linuxmint`…
        id: String,
    },
    /// Anything else: package tools refuse, `shell` still works.
    Other(String),
}

/// apt-get, told to keep a conffile the owner edited rather than stop at a
/// prompt nobody can answer (commands run without a terminal).
const APT_CONFOLD: &str = "-o Dpkg::Options::=--force-confold";
/// Refresh the lists first, so an install doesn't fetch a version the mirror
/// has since replaced. Quiet, and a mirror being down doesn't stop the rest.
const APT_UPDATE: &str = "sudo apt-get update -qq;";

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
        } else if family("debian") || family("ubuntu") {
            Self::Debian {
                id: if id.is_empty() { "debian".into() } else { id },
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
            Self::Debian { id } if id == "debian" => "Debian".into(),
            Self::Debian { id } => {
                let mut c = id.chars();
                let cap: String = c
                    .next()
                    .map(|f| f.to_uppercase().chain(c).collect())
                    .unwrap_or_default();
                format!("{cap} (Debian-based)")
            }
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
            Self::Arch { aur: Some(h) } => format!("{h} -Ss {}", quote(q)),
            Self::Arch { .. } => format!("pacman -Ss {}", quote(q)),
            Self::Debian { .. } => format!("apt-cache search {}", quote(q)),
            Self::Other(_) => String::new(),
        }
    }

    /// Details for one package.
    pub fn info(&self, name: &str) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("{dnf} -q info {}", quote(name)),
            Self::Arch { aur: Some(h) } => format!(
                "pacman -Qi {n} 2>/dev/null || pacman -Si {n} 2>/dev/null || {h} -Si {n}",
                n = quote(name)
            ),
            Self::Arch { .. } => format!(
                "pacman -Qi {n} 2>/dev/null || pacman -Si {n}",
                n = quote(name)
            ),
            // Installed and candidate versions with where they come from,
            // then the newest version's record (`show` lists every version).
            Self::Debian { .. } => format!(
                "apt-cache policy {n}; apt-cache show {n} 2>/dev/null | sed -n '1,/^$/p'",
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
            // AUR updates are listed apart: each is upgraded on its own card.
            (Self::Arch { aur: Some(h) }, "updates") => format!(
                "(checkupdates 2>/dev/null || pacman -Qu); {h} -Qua 2>/dev/null | sed 's/^/aur: /'; true"
            ),
            (Self::Arch { .. }, "updates") => "checkupdates 2>/dev/null || pacman -Qu; true".into(),
            (Self::Arch { .. }, "user") => "pacman -Qe".into(),
            (Self::Arch { .. }, "leaves") => "pacman -Qdtq; true".into(),
            (Self::Arch { .. }, _) => "pacman -Q".into(),
            // What the lists know; only root can refresh them, and the
            // upgrade and install commands do.
            (Self::Debian { .. }, "updates") => {
                "echo '(as of the last apt-get update; pkg_upgrade refreshes the lists first)'; \
                 apt list --upgradable 2>/dev/null | tail -n +2; true"
                    .into()
            }
            (Self::Debian { .. }, "user") => "apt-mark showmanual | sort".into(),
            (Self::Debian { .. }, "leaves") => {
                "apt-get -s autoremove 2>/dev/null | awk '/^Remv/ {print $2}'; true".into()
            }
            (Self::Debian { .. }, _) => {
                "dpkg-query -W -f='${db:Status-Status} ${binary:Package} ${Version} ${Installed-Size}\\n' \
                 | awk '$1==\"installed\" {print $2, $3, $4 \"K\"}' | sort"
                    .into()
            }
            (Self::Other(_), _) => String::new(),
        };
        format!("{base}{grep}")
    }

    /// Install (root).
    pub fn install(&self, pkgs: &[String]) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("sudo {dnf} install -y {}", quote_all(pkgs)),
            Self::Arch { .. } => format!("sudo pacman -S --needed --noconfirm {}", quote_all(pkgs)),
            Self::Debian { .. } => format!(
                "{APT_UPDATE} sudo apt-get install -y {APT_CONFOLD} {}",
                quote_all(pkgs)
            ),
            Self::Other(_) => String::new(),
        }
    }

    /// Remove (root).
    pub fn remove(&self, pkgs: &[String]) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("sudo {dnf} remove -y {}", quote_all(pkgs)),
            Self::Arch { .. } => format!("sudo pacman -Rs --noconfirm {}", quote_all(pkgs)),
            // Config files stay (`remove`, not `purge`), and nothing else
            // goes: `autoremove` would also sweep up packages orphaned
            // earlier, which the card never said.
            Self::Debian { .. } => format!("sudo apt-get remove -y {}", quote_all(pkgs)),
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
            // `upgrade --with-new-pkgs` (what `apt upgrade` does): new
            // dependencies and new kernel packages come in, nothing is
            // removed. What would need a removal is held back and listed.
            Self::Debian { .. } if pkgs.is_empty() => {
                format!("{APT_UPDATE} sudo apt-get upgrade -y --with-new-pkgs {APT_CONFOLD}")
            }
            Self::Debian { .. } => format!(
                "{APT_UPDATE} sudo apt-get install -y --only-upgrade {APT_CONFOLD} {}",
                quote_all(pkgs)
            ),
            Self::Other(_) => String::new(),
        }
    }

    /// Build and install AUR packages with the helper, asking nothing (the
    /// owner already read the PKGBUILDs on the card). The helper runs as
    /// you and calls sudo itself, which reaches Reeve's askpass.
    pub fn aur_install(&self, pkgs: &[String]) -> Option<String> {
        let Self::Arch { aur: Some(h) } = self else {
            return None;
        };
        let flags = if h == "paru" {
            "--needed --noconfirm --skipreview"
        } else {
            "--needed --noconfirm --answerclean None --answerdiff None --answeredit None"
        };
        Some(format!("{h} -S {flags} {}", quote_all(pkgs)))
    }

    /// Recent package transactions.
    pub fn history(&self, limit: usize) -> String {
        match self {
            Self::Fedora { dnf, .. } => format!("{dnf} history list | head -n {}", limit + 1),
            Self::Arch { .. } => format!(
                "grep -E '\\[ALPM\\] (installed|removed|upgraded)' /var/log/pacman.log | tail -n {limit}"
            ),
            // apt's history: one block per run (command, what it installed,
            // upgraded, removed), the last `limit` of them.
            Self::Debian { .. } => format!(
                "awk -v RS= -v n={limit} '{{b[NR]=$0}} END {{for (i=(NR>n?NR-n+1:1); i<=NR; i++) print b[i] \"\\n\"}}' \
                 /var/log/apt/history.log 2>/dev/null \
                 | grep -E '^(Start-Date|Commandline|Requested-By|Install|Upgrade|Remove|Purge|Downgrade|Error):|^$'; true"
            ),
            Self::Other(_) => String::new(),
        }
    }

    /// Package changes can be undone: dnf by transaction id, pacman and apt
    /// from their logs and caches (`crate::pacman`, `crate::apt`).
    pub fn can_undo_packages(&self) -> bool {
        matches!(
            self,
            Self::Fedora { .. } | Self::Arch { .. } | Self::Debian { .. }
        )
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

/// `Fedora 44`, `Arch`: the distribution and major version, for memories.
pub fn os_label() -> String {
    let os = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let field = |k: &str| {
        os.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
            .map(|v| v.trim_matches('"').to_string())
            .unwrap_or_default()
    };
    let name = field("NAME");
    let short = name.split_whitespace().next().unwrap_or("Linux");
    format!("{short} {}", field("VERSION_ID"))
        .trim()
        .to_string()
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
        let debian =
            "PRETTY_NAME=\"Debian GNU/Linux 12 (bookworm)\"\nID=debian\nVERSION_ID=\"12\"\n";
        let d = Distro::from_os_release(debian, |_| false, false);
        assert_eq!(
            d,
            Distro::Debian {
                id: "debian".into()
            }
        );
        assert_eq!(d.name(), "Debian");
        assert!(d.package_block().is_none() && d.can_undo_packages());
        let ubuntu = "ID=ubuntu\nID_LIKE=debian\nVERSION_ID=\"24.04\"\n";
        let u = Distro::from_os_release(ubuntu, |_| false, false);
        assert_eq!(
            u,
            Distro::Debian {
                id: "ubuntu".into()
            }
        );
        assert_eq!(u.name(), "Ubuntu (Debian-based)");
        let mint = "ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n";
        assert!(matches!(
            Distro::from_os_release(mint, |_| false, false),
            Distro::Debian { .. }
        ));
        let pi = "ID=raspbian\nID_LIKE=debian\n";
        assert!(matches!(
            Distro::from_os_release(pi, |_| false, false),
            Distro::Debian { .. }
        ));
    }

    #[test]
    fn debian_speaks_apt() {
        let d = Distro::Debian {
            id: "debian".into(),
        };
        // Installs and upgrades refresh the lists first and never stop at
        // a conffile prompt; removals keep config files and touch nothing else.
        assert_eq!(
            d.install(&["htop".into()]),
            "sudo apt-get update -qq; sudo apt-get install -y -o Dpkg::Options::=--force-confold htop"
        );
        assert_eq!(d.remove(&["htop".into()]), "sudo apt-get remove -y htop");
        assert_eq!(
            d.upgrade(&[]),
            "sudo apt-get update -qq; sudo apt-get upgrade -y --with-new-pkgs -o Dpkg::Options::=--force-confold"
        );
        assert_eq!(
            d.upgrade(&["htop".into()]),
            "sudo apt-get update -qq; sudo apt-get install -y --only-upgrade -o Dpkg::Options::=--force-confold htop"
        );
        assert_eq!(d.search("it's"), "apt-cache search 'it'\\''s'");
        assert!(
            d.info("htop")
                .starts_with("apt-cache policy htop; apt-cache show htop")
        );
        assert!(
            d.list("installed", Some("lib"))
                .ends_with("| sort | grep -i -- lib")
        );
        assert!(d.list("updates", None).contains("apt list --upgradable"));
        assert_eq!(d.list("user", None), "apt-mark showmanual | sort");
        assert!(d.list("leaves", None).starts_with("apt-get -s autoremove"));
        assert!(d.history(5).contains("-v n=5") && d.history(5).contains("history.log"));
        assert!(d.last_transaction().is_none() && d.undo_transaction(1).is_none());
        assert!(d.aur_install(&["x".into()]).is_none());
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
