//! Where a path points, as far as risk goes.
//!
//! Paths are resolved the way the kernel will see them: `~` and `$HOME`
//! expanded, relative paths joined to the working directory, `..` folded,
//! and the longest existing prefix canonicalized, so `~/link-to-etc/fstab`
//! is judged as `/etc/fstab`.

use std::path::{Component, Path, PathBuf};

/// What a path is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathClass {
    /// `~/.reeve/keys/…`: never readable, never writable.
    ReeveKeys,
    /// Reeve's receipts and undo store: readable, never writable by tools.
    ReeveAudit,
    /// A file whose damage can stop boot or lock the owner out.
    FloorFile(&'static str),
    /// A top-level directory itself (`/usr`, `/etc`, the home directory).
    FloorDir(&'static str),
    /// Secrets.
    Sensitive(&'static str),
    /// Scratch space (`/tmp`, `/var/tmp`, `/run/user/<uid>`).
    Temp,
    /// This session's own scratch folder (`~/.reeve/scratch/<session>`).
    Scratch,
    /// Under the user's home.
    Home,
    /// Anything else: system files.
    System,
}

/// What classification needs to know about the machine.
#[derive(Debug, Clone)]
pub struct PathCtx {
    /// The user's home directory.
    pub home: PathBuf,
    /// Reeve's state directory (`~/.reeve`).
    pub reeve_home: PathBuf,
    /// Where relative paths start.
    pub cwd: PathBuf,
    /// The user's uid, for `/run/user/<uid>`.
    pub uid: u32,
    /// `uname -r`, to recognize the running kernel's packages.
    pub kernel: String,
    /// This session's scratch folder, where writing never asks.
    pub scratch: Option<PathBuf>,
}

const FLOOR_ROOTS: &[&str] = &[
    "/",
    "/usr",
    "/etc",
    "/boot",
    "/boot/efi",
    "/var",
    "/bin",
    "/sbin",
    "/lib",
    "/lib64",
    "/opt",
    "/srv",
    "/home",
    "/root",
    "/dev",
    "/proc",
    "/sys",
    "/run",
];

impl PathCtx {
    /// This machine and user.
    pub fn current(reeve_home: PathBuf) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        Self {
            cwd: home.clone(),
            home,
            reeve_home,
            uid: rustix::process::getuid().as_raw(),
            kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease")
                .map(|s| s.trim().to_string())
                .unwrap_or_default(),
            scratch: None,
        }
    }

    /// A fixed user `/home/u` for tests (no filesystem lookups matter).
    pub fn for_tests() -> Self {
        Self {
            home: "/home/u".into(),
            reeve_home: "/home/u/.reeve".into(),
            cwd: "/home/u".into(),
            uid: 1000,
            kernel: "6.17.4-200.fc44.x86_64".into(),
            scratch: Some("/home/u/.reeve/scratch/test".into()),
        }
    }

    /// Expand `~` / `$HOME`, join to `cwd`, fold `..`, and canonicalize
    /// the longest prefix that exists.
    pub fn resolve(&self, raw: &str) -> PathBuf {
        let raw = raw.trim();
        let home = self.home.to_string_lossy();
        let expanded = if raw == "~" {
            home.to_string()
        } else if let Some(rest) = raw.strip_prefix("~/") {
            format!("{home}/{rest}")
        } else if let Some(rest) = raw
            .strip_prefix("$HOME")
            .or_else(|| raw.strip_prefix("${HOME}"))
        {
            format!("{home}{rest}")
        } else if let (Some(rest), Some(scratch)) = (
            raw.strip_prefix("$REEVE_SCRATCH")
                .or_else(|| raw.strip_prefix("${REEVE_SCRATCH}")),
            &self.scratch,
        ) {
            format!("{}{rest}", scratch.display())
        } else {
            raw.to_string()
        };
        let p = Path::new(&expanded);
        let joined = if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.cwd.join(p)
        };
        let lexical = normalize(&joined);
        canonical_prefix(&lexical)
    }

    /// Classify an already-resolved path for writing.
    pub fn classify(&self, p: &Path) -> PathClass {
        if p.starts_with(self.reeve_home.join("keys")) {
            return PathClass::ReeveKeys;
        }
        if p.starts_with(self.reeve_home.join("receipts"))
            || p.starts_with(self.reeve_home.join("undo"))
            || p.starts_with("/var/lib/reeve")
        {
            return PathClass::ReeveAudit;
        }
        // Standing orders grant unattended powers, and Reeve's config sets
        // YOLO and budgets: changing either needs the owner's typed yes.
        if p.starts_with(self.reeve_home.join("orders")) {
            return PathClass::FloorFile("a standing order (powers Reeve uses unattended)");
        }
        // A skill is text Reeve trusts later; skill_save shows it to the
        // owner, and nothing else should slip one in.
        if p.starts_with(self.reeve_home.join("skills")) {
            return PathClass::FloorFile("a skill (steps Reeve follows later)");
        }
        if p == self.reeve_home.join("config.toml") || p == self.reeve_home.join("settings.toml") {
            return PathClass::FloorFile("Reeve's own configuration (budgets, approvals)");
        }
        if let Some(what) = self.floor_file(p) {
            return PathClass::FloorFile(what);
        }
        if self.is_floor_root(p) {
            return PathClass::FloorDir(if p == self.home {
                "your home directory"
            } else {
                "a top-level system directory"
            });
        }
        if let Some(what) = self.sensitive(p) {
            return PathClass::Sensitive(what);
        }
        if self.scratch.as_ref().is_some_and(|s| p.starts_with(s)) {
            return PathClass::Scratch;
        }
        if p.starts_with(&self.home) {
            return PathClass::Home;
        }
        if self.is_temp(p) {
            return PathClass::Temp;
        }
        PathClass::System
    }

    /// A resolved path's directory as the owner reads it: `~/notes` under
    /// home, else absolute.
    pub fn show_dir(&self, p: &Path) -> String {
        let dir = p.parent().unwrap_or(p);
        match dir.strip_prefix(&self.home) {
            Ok(rel) if rel.as_os_str().is_empty() => "~".into(),
            Ok(rel) => format!("~/{}", rel.display()),
            Err(_) => dir.display().to_string(),
        }
    }

    /// `/`, a top-level system directory, the home directory, or one of its
    /// ancestors.
    pub fn is_floor_root(&self, p: &Path) -> bool {
        FLOOR_ROOTS.iter().any(|r| p == Path::new(r)) || self.home.starts_with(p)
    }

    fn is_temp(&self, p: &Path) -> bool {
        (p.starts_with("/tmp") && p != Path::new("/tmp"))
            || (p.starts_with("/var/tmp") && p != Path::new("/var/tmp"))
            || p.starts_with("/dev/shm/")
            || p.starts_with(format!("/run/user/{}/", self.uid))
    }

    /// Files whose damage can stop boot or lock the owner out.
    pub fn floor_file(&self, p: &Path) -> Option<&'static str> {
        let s = p.to_string_lossy();
        let exact: &[(&str, &str)] = &[
            ("/etc/fstab", "/etc/fstab"),
            ("/etc/crypttab", "/etc/crypttab"),
            ("/etc/sudoers", "sudo's rules"),
            ("/etc/passwd", "the user database"),
            ("/etc/shadow", "the password database"),
            ("/etc/group", "the group database"),
            ("/etc/gshadow", "the group password database"),
        ];
        if let Some((_, what)) = exact.iter().find(|(e, _)| s == *e) {
            return Some(what);
        }
        if s.starts_with("/etc/sudoers.d/") {
            return Some("sudo's rules");
        }
        if s.starts_with("/etc/pam.d/") {
            return Some("login (PAM) configuration");
        }
        if s.starts_with("/boot/") {
            return Some("the boot partition");
        }
        let dev = s.strip_prefix("/dev/").unwrap_or("");
        let disk = [
            "sd", "nvme", "mmcblk", "vd", "hd", "dm-", "md", "loop", "mapper/",
        ];
        if disk.iter().any(|d| dev.starts_with(d)) {
            return Some("a disk device");
        }
        None
    }

    /// Secrets that must not be sent to a model provider without a yes.
    pub fn sensitive(&self, p: &Path) -> Option<&'static str> {
        let rel = p.strip_prefix(&self.home).ok();
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(rel) = rel {
            let r = rel.to_string_lossy();
            if r.starts_with(".ssh/") {
                let public = name.ends_with(".pub")
                    || matches!(
                        name.as_str(),
                        "known_hosts" | "known_hosts.old" | "config" | "authorized_keys"
                    );
                if !public {
                    return Some("an SSH private key");
                }
            }
            let prefixes: &[(&str, &str)] = &[
                (".gnupg", "your GnuPG keys"),
                (".local/share/keyrings", "the login keyring"),
                (".password-store", "your password store"),
                (".ryter/keys", "Ryter's API keys"),
                (".aws/credentials", "AWS credentials"),
                (".config/gh/hosts.yml", "GitHub CLI tokens"),
                (".docker/config.json", "Docker registry credentials"),
                (".kube/config", "Kubernetes credentials"),
                (".netrc", "saved passwords (.netrc)"),
                (".git-credentials", "saved Git credentials"),
            ];
            if let Some((_, what)) = prefixes
                .iter()
                .find(|(pre, _)| r == *pre || r.starts_with(&format!("{pre}/")))
            {
                return Some(what);
            }
            let browser = r.starts_with(".mozilla/")
                || r.starts_with(".config/google-chrome/")
                || r.starts_with(".config/chromium/")
                || r.starts_with(".config/BraveSoftware/")
                || r.starts_with(".var/app/");
            let secret_file = matches!(
                name.as_str(),
                "logins.json"
                    | "key4.db"
                    | "cookies.sqlite"
                    | "Login Data"
                    | "Cookies"
                    | "Web Data"
            );
            if browser && secret_file {
                return Some("saved browser passwords or cookies");
            }
        }
        let s = p.to_string_lossy();
        if s == "/etc/shadow" || s == "/etc/gshadow" {
            return Some("the password database");
        }
        if s.starts_with("/etc/ssh/ssh_host_") && !name.ends_with(".pub") {
            return Some("an SSH host key");
        }
        if name == ".env" || name.starts_with(".env.") {
            return Some("an environment file (often holds secrets)");
        }
        let key_ext = [".pem", ".key", ".p12", ".pfx"]
            .iter()
            .any(|e| name.ends_with(e));
        let public_store = s.starts_with("/etc/pki/ca-trust")
            || s.starts_with("/usr/share/")
            || s.starts_with("/etc/ssl/certs");
        if key_ext && !public_store {
            return Some("a private key or certificate bundle");
        }
        None
    }
}

/// Fold `.` and `..` without touching the filesystem.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::from("/");
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(n) => out.push(n),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    out
}

/// Canonicalize the longest existing ancestor and re-append the rest, so a
/// symlink anywhere along the way is followed.
fn canonical_prefix(p: &Path) -> PathBuf {
    let mut existing = p.to_path_buf();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = std::fs::canonicalize(&existing) {
            let mut out = c;
            for part in rest.iter().rev() {
                out.push(part);
            }
            return out;
        }
        match (
            existing.file_name().map(|n| n.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return p.to_path_buf(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_like_the_kernel() {
        let ctx = PathCtx::for_tests();
        assert_eq!(ctx.resolve("~/a/../b"), PathBuf::from("/home/u/b"));
        assert_eq!(ctx.resolve("$HOME/x"), PathBuf::from("/home/u/x"));
        assert_eq!(ctx.resolve("notes.txt"), PathBuf::from("/home/u/notes.txt"));
        assert_eq!(
            ctx.resolve("/etc/../etc/./fstab"),
            PathBuf::from("/etc/fstab")
        );
        assert_eq!(ctx.resolve("../../.."), PathBuf::from("/"));
    }

    #[test]
    fn symlinks_are_followed() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("to-etc");
        std::os::unix::fs::symlink("/etc", &link).unwrap();
        let ctx = PathCtx {
            home: dir.path().to_path_buf(),
            reeve_home: dir.path().join(".reeve"),
            cwd: dir.path().to_path_buf(),
            uid: 1000,
            kernel: String::new(),
            scratch: None,
        };
        let p = ctx.resolve(&format!("{}/fstab", link.display()));
        assert_eq!(p, PathBuf::from("/etc/fstab"));
        assert_eq!(ctx.classify(&p), PathClass::FloorFile("/etc/fstab"));
    }

    #[test]
    fn classes() {
        let ctx = PathCtx::for_tests();
        let c = |s: &str| ctx.classify(&ctx.resolve(s));
        assert_eq!(c("~/.reeve/keys/openrouter"), PathClass::ReeveKeys);
        assert_eq!(c("~/.reeve/undo/objects/ab"), PathClass::ReeveAudit);
        assert!(matches!(
            c("~/.reeve/settings.toml"),
            PathClass::FloorFile(_)
        ));
        assert!(matches!(
            c("~/.reeve/orders/x.toml"),
            PathClass::FloorFile(_)
        ));
        assert!(matches!(
            c("~/.reeve/skills/tidy-downloads.md"),
            PathClass::FloorFile(_)
        ));
        assert_eq!(c("~/.reeve/memory/facts/gpu.md"), PathClass::Home);
        assert_eq!(
            c("/boot/grub2/grub.cfg"),
            PathClass::FloorFile("the boot partition")
        );
        assert_eq!(c("/dev/nvme0n1"), PathClass::FloorFile("a disk device"));
        assert_eq!(c("/dev/null"), PathClass::System);
        assert_eq!(
            c("/usr"),
            PathClass::FloorDir("a top-level system directory")
        );
        assert_eq!(c("~"), PathClass::FloorDir("your home directory"));
        assert_eq!(
            c("/home"),
            PathClass::FloorDir("a top-level system directory")
        );
        assert!(matches!(
            c("~/.gnupg/private-keys-v1.d/x.key"),
            PathClass::Sensitive(_)
        ));
        assert!(matches!(c("~/project/.env"), PathClass::Sensitive(_)));
        assert!(matches!(
            c("~/.mozilla/firefox/abc.default/logins.json"),
            PathClass::Sensitive(_)
        ));
        assert_eq!(c("~/.ssh/known_hosts"), PathClass::Home);
        assert_eq!(
            c("/etc/pki/ca-trust/source/anchors/x.pem"),
            PathClass::System
        );
        assert_eq!(c("/tmp/scratch"), PathClass::Temp);
        assert_eq!(
            c("/tmp"),
            PathClass::System,
            "/tmp itself isn't scratch; what's in it is"
        );
    }
}
