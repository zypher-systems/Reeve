//! The first-run survey: read-only commands that write the machine's basic
//! facts (`survey-*` notes). Refreshed weekly. A survey fact the owner has
//! edited (its source is no longer `survey`) is left alone.

use chrono::{Duration, Utc};

use super::{Layer, Memory, Note};
use crate::tools::{ToolCtx, shell_exec};

/// (id, title, tags, command). Every command is a plain read.
const CHECKS: &[(&str, &str, &[&str], &str)] = &[
    (
        "survey-os",
        "Operating system",
        &["os", "distro", "kernel"],
        "grep -E '^PRETTY_NAME=' /etc/os-release | cut -d= -f2 | tr -d '\"'; echo \"kernel $(uname -r) ($(uname -m))\"; getenforce 2>/dev/null | sed 's/^/SELinux: /'; test -e /run/ostree-booted && echo 'image-based (rpm-ostree)'",
    ),
    (
        "survey-cpu-memory",
        "CPU and memory",
        &["hardware", "cpu", "memory", "ram"],
        "lscpu | grep -E '^(Model name|CPU\\(s\\)):' | sed 's/  */ /g'; free -h | awk '/^Mem/ {print \"RAM \" $2} /^Swap/ {print \"swap \" $2}'; zramctl --noheadings 2>/dev/null | awk '{print \"zram \" $1 \" \" $3}'",
    ),
    (
        "survey-gpu",
        "Graphics",
        &["hardware", "gpu", "graphics", "nvidia", "amd", "intel"],
        "lspci -k 2>/dev/null | grep -EA3 'VGA|3D|Display' | grep -E 'VGA|3D|Display|in use' | sed 's/^\\s*//'",
    ),
    (
        "survey-storage",
        "Disks and filesystems",
        &["storage", "disk", "filesystem", "btrfs", "mount"],
        "lsblk -e7 -o NAME,SIZE,TYPE,FSTYPE,MOUNTPOINTS 2>/dev/null | head -n 30",
    ),
    (
        "survey-boot",
        "Boot",
        &["boot", "bootloader", "grub", "efi", "secureboot"],
        "test -d /sys/firmware/efi && echo 'UEFI' || echo 'BIOS'; (bootctl is-installed 2>/dev/null | grep -q yes && echo 'systemd-boot') || (test -e /boot/grub2/grub.cfg -o -e /boot/grub/grub.cfg && echo 'GRUB'); mokutil --sb-state 2>/dev/null",
    ),
    (
        "survey-desktop",
        "Desktop session",
        &["desktop", "wayland", "x11", "gnome", "kde", "hyprland"],
        "echo \"desktop: ${XDG_CURRENT_DESKTOP:-unknown} (${XDG_SESSION_TYPE:-unknown})\"; systemctl status display-manager --no-pager 2>/dev/null | head -n 1 | sed 's/^● //'",
    ),
    (
        "survey-packages",
        "Packages",
        &["packages", "dnf", "pacman", "flatpak", "repos"],
        "command -v rpm >/dev/null && echo \"rpm packages: $(rpm -qa | wc -l)\"; command -v pacman >/dev/null && echo \"pacman packages: $(pacman -Q | wc -l)\"; command -v flatpak >/dev/null && echo \"flatpak apps: $(flatpak list --app 2>/dev/null | wc -l)\"; command -v dnf5 >/dev/null && dnf5 -q repolist 2>/dev/null | tail -n +2 | awk '{print \"repo \" $1}' | head -n 15",
    ),
    (
        "survey-services",
        "Enabled services",
        &["services", "systemd", "units"],
        "systemctl list-unit-files --type=service --state=enabled --no-legend --no-pager 2>/dev/null | awk '{print $1}' | tr '\\n' ' '",
    ),
    (
        "survey-network",
        "Network",
        &["network", "wifi", "networkmanager", "dns"],
        "nmcli -t -f DEVICE,TYPE,STATE,CONNECTION device 2>/dev/null | grep -v ':unmanaged' | head -n 10; ip route show default 2>/dev/null | head -n 1",
    ),
    (
        "survey-snapshots",
        "Snapshots and backups",
        &["snapper", "snapshots", "backup", "timeshift"],
        "command -v snapper >/dev/null && echo \"snapper configs: $(snapper --csvout list-configs 2>/dev/null | tail -n +2 | tr '\\n' ' ')\" || echo 'snapper not installed'; command -v timeshift >/dev/null && echo 'timeshift installed'; true",
    ),
    (
        "survey-virt",
        "Virtualization and containers",
        &["virtualization", "vm", "docker", "podman", "libvirt"],
        "echo \"running in: $(systemd-detect-virt 2>/dev/null || echo none)\"; for b in docker podman virsh distrobox toolbox; do command -v $b >/dev/null && echo \"$b installed\"; done; true",
    ),
];

/// Whether the survey is missing or older than a week.
pub fn due(mem: &Memory) -> bool {
    match mem.get("survey-os") {
        Some(n) => Utc::now() - n.observed > Duration::days(7),
        None => true,
    }
}

/// Run the survey. Returns how many facts were written.
pub async fn run(ctx: &ToolCtx, mem: &Memory, os: &str) -> usize {
    let mut n = 0;
    for (id, title, tags, cmd) in CHECKS {
        if let Some(existing) = mem.get(id) {
            if existing.source != "survey" {
                continue;
            }
        }
        // Checks end in `test … && echo`, which exits 1 when false: the
        // output still counts.
        let Some(out) = shell_exec(ctx, &format!("{cmd}; true"), false).await else {
            continue;
        };
        let body: String = out.trim().chars().take(2000).collect();
        if body.is_empty() {
            continue;
        }
        let mut note = Note::new(Layer::Facts, title, &body, "survey");
        note.id = (*id).into();
        note.tags = tags.iter().map(|t| (*t).to_string()).collect();
        note.os = (!os.is_empty()).then(|| os.to_string());
        note.confidence = 0.95;
        if mem.put(&mut note, true).is_ok() {
            n += 1;
        }
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_survey_writes_facts_and_respects_edits() {
        let home = tempfile::tempdir().unwrap();
        let mem = Memory::new(home.path());
        let ctx = ToolCtx::new(home.path().to_path_buf(), vec![]);
        assert!(due(&mem));
        // The owner rewrote the GPU fact: the survey must keep their version.
        let mut mine = Note::new(
            Layer::Facts,
            "Graphics",
            "eGPU on Thunderbolt, see notes",
            "user",
        );
        mine.id = "survey-gpu".into();
        mem.put(&mut mine, true).unwrap();
        let n = run(&ctx, &mem, "Test OS").await;
        assert!(n >= 3, "{n}");
        assert!(!due(&mem));
        assert_eq!(
            mem.get("survey-gpu").unwrap().body,
            "eGPU on Thunderbolt, see notes"
        );
        assert!(mem.get("survey-os").unwrap().body.contains("kernel"));
    }
}
