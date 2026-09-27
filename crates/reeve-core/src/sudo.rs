//! Root, one approved action at a time: `sudo -A` with Reeve's own askpass.
//!
//! Commands run without a terminal, so sudo can't prompt on one. Instead,
//! a `sudo` wrapper at the front of the command's `PATH` adds `-A`, and
//! sudo runs `reeve-askpass` (a link to this binary) for the password. The
//! helper asks the running Reeve over a private socket, and Reeve asks the
//! person. The password goes from the person to sudo and nowhere else:
//! never to the model, a log, a receipt, or disk.
//!
//! The socket answers only while an approved root action is running
//! ("armed"), and only to a caller holding this session's token.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use crate::error::{Error, Result};

/// Name the askpass link is called by; `main` checks `argv[0]` for it.
pub const HELPER_NAME: &str = "reeve-askpass";

/// Where a password comes from.
#[async_trait]
pub trait PasswordSource: Send + Sync {
    /// Ask for the password sudo wants. `None` refuses.
    async fn password(&self, prompt: String) -> Option<String>;
}

/// A running askpass endpoint.
#[derive(Debug, Clone)]
pub struct Askpass {
    bin_dir: PathBuf,
    sock: PathBuf,
    token: String,
    armed: Arc<AtomicUsize>,
}

/// While alive, the socket will ask for a password.
pub struct Armed(Arc<AtomicUsize>);

impl Drop for Armed {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[derive(Serialize, Deserialize)]
struct Ask {
    token: String,
    prompt: String,
}

impl Askpass {
    /// Set up `bin/` (the helper link and the sudo wrapper) and start
    /// listening. Needs a tokio runtime.
    pub fn start(reeve_home: &Path, source: Arc<dyn PasswordSource>) -> Result<Self> {
        let bin_dir = reeve_home.join("bin");
        fs::create_dir_all(&bin_dir)?;
        set_mode(&bin_dir, 0o700)?;
        let exe = std::env::current_exe()?;
        let link = bin_dir.join(HELPER_NAME);
        if fs::read_link(&link).ok().as_deref() != Some(exe.as_path()) {
            let _ = fs::remove_file(&link);
            std::os::unix::fs::symlink(&exe, &link)?;
        }
        let real_sudo =
            find_sudo(&bin_dir).ok_or_else(|| Error::Io("sudo isn't installed".into()))?;
        let wrapper = bin_dir.join("sudo");
        let script = format!(
            "#!/bin/sh\n# Written by Reeve: sudo must use Reeve's askpass, since there is no terminal.\nexec {} -A \"$@\"\n",
            real_sudo.display()
        );
        if fs::read_to_string(&wrapper).ok().as_deref() != Some(script.as_str()) {
            fs::write(&wrapper, script)?;
        }
        set_mode(&wrapper, 0o700)?;

        let run_dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(|d| PathBuf::from(d).join("reeve"))
            .unwrap_or_else(|| reeve_home.join("run"));
        fs::create_dir_all(&run_dir)?;
        set_mode(&run_dir, 0o700)?;
        clean_stale(&run_dir);
        static N: AtomicUsize = AtomicUsize::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let sock = run_dir.join(format!("askpass-{}-{n}.sock", std::process::id()));
        let _ = fs::remove_file(&sock);
        let listener = tokio::net::UnixListener::bind(&sock)?;
        set_mode(&sock, 0o600)?;

        let token = random_token()?;
        let armed = Arc::new(AtomicUsize::new(0));
        let ap = Self {
            bin_dir,
            sock,
            token: token.clone(),
            armed: armed.clone(),
        };
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (source, token, armed) = (source.clone(), token.clone(), armed.clone());
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut line = String::new();
                    if tokio::io::BufReader::new(r)
                        .read_line(&mut line)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    let reply = match serde_json::from_str::<Ask>(&line) {
                        Ok(ask) if ask.token == token && armed.load(Ordering::SeqCst) > 0 => {
                            match source.password(ask.prompt).await {
                                Some(pw) => format!("OK {pw}\n"),
                                None => "NO\n".into(),
                            }
                        }
                        _ => "NO\n".into(),
                    };
                    let _ = w.write_all(reply.as_bytes()).await;
                });
            }
        });
        Ok(ap)
    }

    /// Allow password requests until the guard drops.
    pub fn arm(&self) -> Armed {
        self.armed.fetch_add(1, Ordering::SeqCst);
        Armed(self.armed.clone())
    }

    /// Environment for a command that may run sudo.
    pub fn env(&self) -> Vec<(String, String)> {
        let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/local/bin:/usr/bin:/bin".into());
        vec![
            ("PATH".into(), format!("{}:{path}", self.bin_dir.display())),
            (
                "SUDO_ASKPASS".into(),
                self.bin_dir.join(HELPER_NAME).display().to_string(),
            ),
            ("REEVE_ASKPASS_SOCK".into(), self.sock.display().to_string()),
            ("REEVE_ASKPASS_TOKEN".into(), self.token.clone()),
        ]
    }

    /// The real sudo, wrapped to use the askpass (for Reeve's own calls).
    pub fn sudo_path(&self) -> PathBuf {
        self.bin_dir.join("sudo")
    }
}

/// Remove sockets left by Reeve processes that are gone.
fn clean_stale(dir: &Path) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let pid = name
            .strip_prefix("askpass-")
            .and_then(|r| r.split('-').next())
            .and_then(|p| p.parse::<u32>().ok());
        if let Some(pid) = pid {
            if !Path::new(&format!("/proc/{pid}")).exists() {
                let _ = fs::remove_file(e.path());
            }
        }
    }
}

/// The helper side: sudo ran `reeve-askpass "<prompt>"`. Prints the
/// password and exits 0, or exits 1.
pub fn helper_main() -> i32 {
    let prompt = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Password:".into());
    let (Ok(sock), Ok(token)) = (
        std::env::var("REEVE_ASKPASS_SOCK"),
        std::env::var("REEVE_ASKPASS_TOKEN"),
    ) else {
        eprintln!("reeve-askpass: not started by Reeve");
        return 1;
    };
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&sock) else {
        eprintln!("reeve-askpass: Reeve isn't running");
        return 1;
    };
    let ask = serde_json::to_string(&Ask { token, prompt }).unwrap_or_default();
    if writeln!(stream, "{ask}").is_err() {
        return 1;
    }
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).is_err() {
        return 1;
    }
    match line.strip_prefix("OK ") {
        Some(pw) => {
            let mut out = std::io::stdout();
            let _ = out.write_all(pw.trim_end_matches('\n').as_bytes());
            let _ = out.write_all(b"\n");
            0
        }
        None => 1,
    }
}

fn find_sudo(exclude: &Path) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .map(PathBuf::from)
        .filter(|d| d != exclude)
        .map(|d| d.join("sudo"))
        .find(|p| p.is_file())
        .or_else(|| Some(PathBuf::from("/usr/bin/sudo")).filter(|p| p.is_file()))
}

fn random_token() -> Result<String> {
    let mut buf = [0u8; 24];
    let mut f = fs::File::open("/dev/urandom")?;
    std::io::Read::read_exact(&mut f, &mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn set_mode(p: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(p, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed(Option<&'static str>);

    #[async_trait]
    impl PasswordSource for Fixed {
        async fn password(&self, _prompt: String) -> Option<String> {
            self.0.map(String::from)
        }
    }

    async fn ask(ap: &Askpass, token: &str) -> String {
        let sock = ap.sock.clone();
        let token = token.to_string();
        tokio::task::spawn_blocking(move || {
            let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
            writeln!(
                s,
                "{}",
                serde_json::to_string(&Ask {
                    token,
                    prompt: "pw".into()
                })
                .unwrap()
            )
            .unwrap();
            let mut line = String::new();
            BufReader::new(&s).read_line(&mut line).unwrap();
            line
        })
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn answers_only_when_armed_and_with_the_token() {
        let home = tempfile::tempdir().unwrap();
        let ap = Askpass::start(home.path(), Arc::new(Fixed(Some("hunter2")))).unwrap();
        assert_eq!(ask(&ap, &ap.token).await, "NO\n", "not armed");
        let guard = ap.arm();
        assert_eq!(ask(&ap, "wrong").await, "NO\n", "bad token");
        assert_eq!(ask(&ap, &ap.token).await, "OK hunter2\n");
        drop(guard);
        assert_eq!(ask(&ap, &ap.token).await, "NO\n", "disarmed");
    }

    #[tokio::test]
    async fn the_socket_and_bin_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let ap = Askpass::start(home.path(), Arc::new(Fixed(None))).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&ap.sock), 0o600);
        assert_eq!(mode(&home.path().join("bin")), 0o700);
        let wrapper = fs::read_to_string(ap.sudo_path()).unwrap();
        assert!(wrapper.contains(" -A \"$@\""), "{wrapper}");
        assert!(
            ap.env().iter().any(|(k, v)| k == "PATH"
                && v.starts_with(&home.path().join("bin").display().to_string()))
        );
    }
}
