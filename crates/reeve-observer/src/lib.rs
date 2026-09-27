//! Reading the machine's vital signs.
//!
//! M0 has only the [`Sampler`], which the TUI polls for its System rail.
//! In M4 the same sampler feeds `reeved`'s baselines and detectors.
//!
//! Linux-only for now, straight from `/proc` and `/sys`: no daemon, no root,
//! and no dependency heavier than `statvfs`.

#![forbid(unsafe_code)]

pub mod baselines;
pub mod daemon;
pub mod detect;
pub mod drafter;
pub mod journal;
pub mod notify;
pub mod service;

use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::Instant;

use serde::Serialize;

/// Facts that don't change between samples.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct HostInfo {
    /// `nexus`.
    pub hostname: String,
    /// `Fedora Linux 44 (KDE Plasma Desktop Edition)`.
    pub os_pretty: String,
    /// `Fedora 44`.
    pub os_short: String,
    /// `fedora`, `arch`, …
    pub os_id: String,
    /// Kernel release.
    pub kernel: String,
    /// CPU model.
    pub cpu_model: String,
    /// Logical CPUs.
    pub cpus: usize,
}

impl HostInfo {
    /// Read it once.
    pub fn read() -> Self {
        let os = fs::read_to_string("/etc/os-release").unwrap_or_default();
        let field = |k: &str| {
            os.lines()
                .find_map(|l| l.strip_prefix(k)?.strip_prefix('='))
                .map(|v| v.trim_matches('"').to_string())
                .unwrap_or_default()
        };
        let name = field("NAME");
        let short_name = name
            .split_whitespace()
            .next()
            .unwrap_or("Linux")
            .to_string();
        let version = field("VERSION_ID");
        let cpuinfo = fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
        Self {
            hostname: read_trim("/proc/sys/kernel/hostname"),
            os_pretty: field("PRETTY_NAME"),
            os_short: format!("{short_name} {version}").trim().to_string(),
            os_id: field("ID"),
            kernel: read_trim("/proc/sys/kernel/osrelease"),
            cpu_model: cpuinfo
                .lines()
                .find_map(|l| l.strip_prefix("model name")?.split_once(':'))
                .map(|(_, v)| v.trim().to_string())
                .unwrap_or_default(),
            cpus: cpuinfo
                .lines()
                .filter(|l| l.starts_with("processor"))
                .count(),
        }
    }

    /// Short machine profile for the system prompt.
    pub fn profile(&self) -> String {
        format!(
            "- host: {}\n- os: {}\n- kernel: {}\n- cpu: {} ({} threads)",
            self.hostname, self.os_pretty, self.kernel, self.cpu_model, self.cpus
        )
    }
}

/// One mounted filesystem.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Disk {
    /// Mount point.
    pub mount: String,
    /// Filesystem type.
    pub fs: String,
    /// Bytes.
    pub total: u64,
    /// Bytes.
    pub used: u64,
    /// Bytes available to unprivileged users.
    pub avail: u64,
}

impl Disk {
    /// `df`-style use: used / (used + avail).
    pub fn ratio(&self) -> f64 {
        let denom = self.used + self.avail;
        if denom == 0 {
            0.0
        } else {
            self.used as f64 / denom as f64
        }
    }
}

/// A moment's readings.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Snapshot {
    /// Seconds since boot.
    pub uptime_secs: u64,
    /// Whole-machine CPU busy share, 0–100. `None` on the first sample.
    pub cpu_pct: Option<f32>,
    /// 1, 5, 15 minute load.
    pub load: [f32; 3],
    /// Bytes.
    pub mem_total: u64,
    /// Bytes (total − available).
    pub mem_used: u64,
    /// Bytes.
    pub swap_total: u64,
    /// Bytes.
    pub swap_used: u64,
    /// Real filesystems, one per device.
    pub disks: Vec<Disk>,
    /// Bytes/s received on non-loopback interfaces.
    pub net_rx_bps: Option<f64>,
    /// Bytes/s sent.
    pub net_tx_bps: Option<f64>,
    /// Hottest CPU sensor, °C.
    pub temp_c: Option<f32>,
    /// Battery percent and status (`Charging`, `Discharging`, …).
    pub battery: Option<(u8, String)>,
}

/// Keeps the previous counters so rates can be computed.
#[derive(Debug, Default)]
pub struct Sampler {
    cpu: Option<(u64, u64)>,
    net: Option<(Instant, u64, u64)>,
}

impl Sampler {
    /// A sampler with no history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Read everything now.
    pub fn sample(&mut self) -> Snapshot {
        let mut s = Snapshot {
            uptime_secs: read_trim("/proc/uptime")
                .split_whitespace()
                .next()
                .and_then(|v| v.parse::<f64>().ok())
                .map_or(0, |v| v as u64),
            ..Snapshot::default()
        };
        if let Some((busy, total)) = cpu_counters() {
            if let Some((pb, pt)) = self.cpu {
                let dt = total.saturating_sub(pt);
                if dt > 0 {
                    s.cpu_pct =
                        Some((busy.saturating_sub(pb) as f32 / dt as f32 * 100.0).min(100.0));
                }
            }
            self.cpu = Some((busy, total));
        }
        let load = read_trim("/proc/loadavg");
        for (i, v) in load.split_whitespace().take(3).enumerate() {
            s.load[i] = v.parse().unwrap_or(0.0);
        }
        let mem = meminfo();
        s.mem_total = mem.0;
        s.mem_used = mem.0.saturating_sub(mem.1);
        s.swap_total = mem.2;
        s.swap_used = mem.2.saturating_sub(mem.3);
        s.disks = disks();
        if let Some((rx, tx)) = net_counters() {
            let now = Instant::now();
            if let Some((then, prx, ptx)) = self.net {
                let dt = now.duration_since(then).as_secs_f64();
                if dt > 0.0 {
                    s.net_rx_bps = Some(rx.saturating_sub(prx) as f64 / dt);
                    s.net_tx_bps = Some(tx.saturating_sub(ptx) as f64 / dt);
                }
            }
            self.net = Some((now, rx, tx));
        }
        s.temp_c = cpu_temp();
        s.battery = battery();
        s
    }
}

/// Failed systemd units, system and user. Runs `systemctl`, so poll it
/// seldom (the TUI does every 30 s).
pub fn failed_units() -> Vec<String> {
    let mut out = Vec::new();
    for scope in [None, Some("--user")] {
        let mut cmd = Command::new("systemctl");
        cmd.args(["--failed", "--no-legend", "--plain", "--no-pager"]);
        if let Some(s) = scope {
            cmd.arg(s);
        }
        let Ok(o) = cmd.output() else { continue };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if let Some(unit) = line.split_whitespace().next() {
                let unit = if scope.is_some() {
                    format!("{unit} (user)")
                } else {
                    unit.to_string()
                };
                out.push(unit);
            }
        }
    }
    out
}

fn read_trim(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path)
        .map(|s| s.trim().to_string())
        .unwrap_or_default()
}

/// (busy, total) jiffies from the first line of `/proc/stat`.
fn cpu_counters() -> Option<(u64, u64)> {
    let stat = fs::read_to_string("/proc/stat").ok()?;
    parse_cpu_line(stat.lines().next()?)
}

fn parse_cpu_line(line: &str) -> Option<(u64, u64)> {
    let mut parts = line.split_whitespace();
    if parts.next()? != "cpu" {
        return None;
    }
    // user nice system idle iowait irq softirq steal (guest is inside user).
    let v: Vec<u64> = parts.take(8).filter_map(|x| x.parse().ok()).collect();
    if v.len() < 4 {
        return None;
    }
    let total: u64 = v.iter().sum();
    let idle = v[3] + v.get(4).copied().unwrap_or(0);
    Some((total - idle, total))
}

/// (MemTotal, MemAvailable, SwapTotal, SwapFree) in bytes.
fn meminfo() -> (u64, u64, u64, u64) {
    let text = fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k)?.strip_prefix(':'))
            .and_then(|v| v.split_whitespace().next()?.parse::<u64>().ok())
            .unwrap_or(0)
            * 1024
    };
    (
        get("MemTotal"),
        get("MemAvailable"),
        get("SwapTotal"),
        get("SwapFree"),
    )
}

const REAL_FS: &[&str] = &[
    "ext4", "ext3", "ext2", "btrfs", "xfs", "f2fs", "vfat", "exfat", "ntfs", "ntfs3", "zfs",
    "bcachefs",
];

fn disks() -> Vec<Disk> {
    let mounts = fs::read_to_string("/proc/self/mounts").unwrap_or_default();
    let mut seen = std::collections::HashMap::<String, usize>::new();
    let mut out: Vec<Disk> = Vec::new();
    for line in mounts.lines() {
        let mut f = line.split_whitespace();
        let (Some(dev), Some(mnt), Some(fs)) = (f.next(), f.next(), f.next()) else {
            continue;
        };
        if !REAL_FS.contains(&fs) {
            continue;
        }
        let mount = unescape_mount(mnt);
        let Ok(st) = rustix::fs::statvfs(mount.as_str()) else {
            continue;
        };
        let frsize = st.f_frsize;
        let disk = Disk {
            mount,
            fs: fs.to_string(),
            total: st.f_blocks * frsize,
            used: st.f_blocks.saturating_sub(st.f_bfree) * frsize,
            avail: st.f_bavail * frsize,
        };
        if disk.total == 0 {
            continue;
        }
        // btrfs subvolumes (`/`, `/home`, …) share one device and one set of
        // numbers: keep the shortest mount point.
        match seen.get(dev) {
            Some(&i) if out[i].mount.len() <= disk.mount.len() => {}
            Some(&i) => out[i] = disk,
            None => {
                seen.insert(dev.to_string(), out.len());
                out.push(disk);
            }
        }
    }
    out.sort_by(|a, b| a.mount.cmp(&b.mount));
    out
}

/// `/proc/mounts` escapes space, tab, newline, and backslash as octal.
fn unescape_mount(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 4], 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// (rx, tx) bytes across non-loopback interfaces.
fn net_counters() -> Option<(u64, u64)> {
    let text = fs::read_to_string("/proc/net/dev").ok()?;
    let mut rx = 0;
    let mut tx = 0;
    for line in text.lines().skip(2) {
        let Some((iface, rest)) = line.split_once(':') else {
            continue;
        };
        let iface = iface.trim();
        if iface == "lo" || iface.starts_with("veth") || iface.starts_with("docker") {
            continue;
        }
        let v: Vec<u64> = rest
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .collect();
        if v.len() >= 9 {
            rx += v[0];
            tx += v[8];
        }
    }
    Some((rx, tx))
}

/// Hottest CPU package sensor, else hottest sensor of any kind.
fn cpu_temp() -> Option<f32> {
    let dir = fs::read_dir("/sys/class/hwmon").ok()?;
    let mut cpu: Option<f32> = None;
    let mut any: Option<f32> = None;
    for e in dir.flatten() {
        let p = e.path();
        let name = read_trim(p.join("name"));
        let is_cpu = matches!(
            name.as_str(),
            "coretemp" | "k10temp" | "zenpower" | "cpu_thermal" | "acpitz"
        );
        let Ok(files) = fs::read_dir(&p) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            if !(fname.starts_with("temp") && fname.ends_with("_input")) {
                continue;
            }
            let Ok(milli) = read_trim(f.path()).parse::<f32>() else {
                continue;
            };
            let c = milli / 1000.0;
            if !(0.0..150.0).contains(&c) {
                continue;
            }
            let slot = if is_cpu { &mut cpu } else { &mut any };
            *slot = Some(slot.map_or(c, |m| m.max(c)));
        }
    }
    cpu.or(any)
}

fn battery() -> Option<(u8, String)> {
    let dir = fs::read_dir("/sys/class/power_supply").ok()?;
    for e in dir.flatten() {
        let p = e.path();
        if read_trim(p.join("type")) != "Battery" {
            continue;
        }
        let pct = read_trim(p.join("capacity")).parse::<u8>().ok()?;
        return Some((pct, read_trim(p.join("status"))));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_line_counts_iowait_as_idle() {
        let (busy, total) = parse_cpu_line("cpu  100 0 50 800 50 0 0 0 0 0").unwrap();
        assert_eq!((busy, total), (150, 1000));
        assert!(parse_cpu_line("cpu0 1 2 3 4").is_none());
    }

    #[test]
    fn mount_points_are_unescaped() {
        assert_eq!(unescape_mount("/mnt/my\\040disk"), "/mnt/my disk");
        assert_eq!(unescape_mount("/plain"), "/plain");
        assert_eq!(unescape_mount("/trailing\\"), "/trailing\\");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_real_sample_is_sane() {
        let mut s = Sampler::new();
        let _ = s.sample();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let snap = s.sample();
        assert!(snap.mem_total > 0 && snap.mem_used <= snap.mem_total);
        assert!(snap.uptime_secs > 0);
        assert!(!HostInfo::read().kernel.is_empty());
    }
}
