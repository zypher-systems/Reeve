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
//!
//! sudo's own five-minute cache is no help here: with no terminal it is
//! keyed on sudo's parent process, and that is a fresh shell for every
//! command. So Reeve remembers the typed password itself, in memory, for
//! `[approvals] password_minutes`, and answers each new sudo with it. The
//! helper names the sudo process asking (its parent). The same process
//! asking twice means sudo refused what it got: the memory is dropped and
//! the person is asked. Any other process asking is simply the next step.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::error::{Error, Result};

/// Name the askpass link is called by; `main` checks `argv[0]` for it.
pub const HELPER_NAME: &str = "reeve-askpass";

/// What the person typed.
pub struct Answer {
    pub secret: String,
    /// Keep it for the root steps that follow, within `password_minutes`.
    pub remember: bool,
}

/// Where a password comes from when Reeve doesn't remember one.
#[async_trait]
pub trait PasswordSource: Send + Sync {
    /// Ask the person for the password sudo wants. `None` refuses.
    async fn password(&self, prompt: String) -> Option<Answer>;
}

/// A running askpass endpoint.
#[derive(Debug, Clone)]
pub struct Askpass {
    bin_dir: PathBuf,
    sock: PathBuf,
    token: String,
    armed: Arc<AtomicUsize>,
    /// Counts `arm` calls: a sudo process is told apart by its pid and the
    /// run it asked in.
    runs: Arc<AtomicU64>,
}

/// While alive, the socket will ask for a password.
pub struct Armed(Arc<AtomicUsize>);

impl Drop for Armed {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// The remembered password. Memory only, wiped when dropped or replaced.
#[derive(Default)]
struct Remembered {
    /// The password and when it last answered sudo.
    secret: Option<(Zeroizing<String>, Instant)>,
    /// The sudo process (run, pid) that got the last answer.
    answered: Option<(u64, u32)>,
}

impl std::fmt::Debug for Remembered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Remembered")
            .field("secret", &self.secret.as_ref().map(|_| "<held>"))
            .field("answered", &self.answered)
            .finish()
    }
}

#[derive(Serialize, Deserialize)]
struct Ask {
    token: String,
    prompt: String,
    /// The sudo process asking: the helper's parent.
    #[serde(default)]
    caller: u32,
}

impl Askpass {
    /// Set up `bin/` (the helper link and the sudo wrapper) and start
    /// listening. A typed password is remembered for `remember_for` (zero:
    /// never). Needs a tokio runtime.
    pub fn start(
        reeve_home: &Path,
        source: Arc<dyn PasswordSource>,
        remember_for: Duration,
    ) -> Result<Self> {
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
        let runs = Arc::new(AtomicU64::new(0));
        let memory = Arc::new(Mutex::new(Remembered::default()));
        let ap = Self {
            bin_dir,
            sock,
            token: token.clone(),
            armed: armed.clone(),
            runs: runs.clone(),
        };
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let (source, token) = (source.clone(), token.clone());
                let (armed, runs, memory) = (armed.clone(), runs.clone(), memory.clone());
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
                            let run = runs.load(Ordering::SeqCst);
                            match answer(ask, &*source, &memory, run, remember_for).await {
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
        self.runs.fetch_add(1, Ordering::SeqCst);
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

/// The password for `ask`: the remembered one when a sudo that hasn't
/// asked yet wants it and the memory is fresh, otherwise the person's.
async fn answer(
    ask: Ask,
    source: &dyn PasswordSource,
    memory: &Mutex<Remembered>,
    run: u64,
    remember_for: Duration,
) -> Option<String> {
    let caller = (run, ask.caller);
    let remembered = {
        let mut m = memory.lock().unwrap_or_else(PoisonError::into_inner);
        if m.answered == Some(caller) {
            // The same sudo asks again: it refused what it got.
            m.secret = None;
            None
        } else {
            match m.secret.take() {
                Some((pw, at)) if at.elapsed() < remember_for => {
                    // Like sudo's own cache, the clock restarts on each use.
                    m.secret = Some((pw.clone(), Instant::now()));
                    m.answered = Some(caller);
                    Some(pw)
                }
                _ => None,
            }
        }
    };
    if let Some(pw) = remembered {
        return Some(pw.to_string());
    }
    let a = source.password(ask.prompt).await?;
    let mut m = memory.lock().unwrap_or_else(PoisonError::into_inner);
    m.secret = (a.remember && !remember_for.is_zero())
        .then(|| (Zeroizing::new(a.secret.clone()), Instant::now()));
    m.answered = Some(caller);
    Some(a.secret)
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
    let ask = Ask {
        token,
        prompt,
        // sudo runs the helper itself, so this is the sudo asking.
        caller: std::os::unix::process::parent_id(),
    };
    let ask = serde_json::to_string(&ask).unwrap_or_default();
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
    use std::collections::VecDeque;

    const FIVE_MINUTES: Duration = Duration::from_secs(300);

    /// Answers in order, then refuses. Counts how often it was asked.
    struct Script {
        answers: Mutex<VecDeque<(&'static str, bool)>>,
        asked: AtomicUsize,
    }

    fn script(answers: &[(&'static str, bool)]) -> Arc<Script> {
        Arc::new(Script {
            answers: Mutex::new(answers.iter().copied().collect()),
            asked: AtomicUsize::new(0),
        })
    }

    #[async_trait]
    impl PasswordSource for Script {
        async fn password(&self, _prompt: String) -> Option<Answer> {
            self.asked.fetch_add(1, Ordering::SeqCst);
            let (secret, remember) = self.answers.lock().unwrap().pop_front()?;
            Some(Answer {
                secret: secret.into(),
                remember,
            })
        }
    }

    /// What sudo process `caller` gets when it asks with `token`.
    async fn ask(ap: &Askpass, token: &str, caller: u32) -> String {
        let sock = ap.sock.clone();
        let token = token.to_string();
        tokio::task::spawn_blocking(move || {
            let mut s = std::os::unix::net::UnixStream::connect(sock).unwrap();
            writeln!(
                s,
                "{}",
                serde_json::to_string(&Ask {
                    token,
                    prompt: "pw".into(),
                    caller,
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
        let src = script(&[("hunter2", true)]);
        let ap = Askpass::start(home.path(), src.clone(), FIVE_MINUTES).unwrap();
        assert_eq!(ask(&ap, &ap.token, 1).await, "NO\n", "not armed");
        let guard = ap.arm();
        assert_eq!(ask(&ap, "wrong", 1).await, "NO\n", "bad token");
        assert_eq!(ask(&ap, &ap.token, 1).await, "OK hunter2\n");
        drop(guard);
        assert_eq!(ask(&ap, &ap.token, 2).await, "NO\n", "disarmed");
        assert_eq!(
            src.asked.load(Ordering::SeqCst),
            1,
            "refusals never reach the person"
        );
    }

    #[tokio::test]
    async fn a_remembered_password_answers_each_new_sudo_but_not_the_same_one_twice() {
        let home = tempfile::tempdir().unwrap();
        let src = script(&[("hunter2", true), ("letmein", true)]);
        let ap = Askpass::start(home.path(), src.clone(), FIVE_MINUTES).unwrap();
        let asked = || src.asked.load(Ordering::SeqCst);

        // One command: a pre-snapshot's sudo, then the action's.
        let run = ap.arm();
        assert_eq!(ask(&ap, &ap.token, 100).await, "OK hunter2\n");
        assert_eq!(ask(&ap, &ap.token, 200).await, "OK hunter2\n");
        assert_eq!(asked(), 1, "the second sudo is answered from memory");
        drop(run);

        // The next command, whose sudo happens to reuse a pid from before.
        let run = ap.arm();
        assert_eq!(ask(&ap, &ap.token, 100).await, "OK hunter2\n");
        assert_eq!(asked(), 1, "a new run is a new sudo, whatever its pid");

        // The same sudo again: it refused the password. The memory goes
        // and the person is asked; what they type is remembered instead.
        assert_eq!(ask(&ap, &ap.token, 100).await, "OK letmein\n");
        assert_eq!(asked(), 2);
        assert_eq!(ask(&ap, &ap.token, 300).await, "OK letmein\n");
        assert_eq!(asked(), 2);

        // Refused again, and the person gives nothing: nothing is kept.
        assert_eq!(ask(&ap, &ap.token, 300).await, "NO\n");
        assert_eq!(asked(), 3);
        assert_eq!(ask(&ap, &ap.token, 400).await, "NO\n");
        assert_eq!(asked(), 4, "no stale memory answers after a refusal");
        drop(run);
    }

    #[tokio::test]
    async fn not_kept_when_the_person_says_so_or_the_config_forbids_it() {
        let home = tempfile::tempdir().unwrap();
        let src = script(&[("a", false), ("b", true)]);
        let ap = Askpass::start(home.path(), src.clone(), FIVE_MINUTES).unwrap();
        let _run = ap.arm();
        assert_eq!(ask(&ap, &ap.token, 1).await, "OK a\n");
        assert_eq!(
            ask(&ap, &ap.token, 2).await,
            "OK b\n",
            "`a` wasn't to be remembered"
        );
        assert_eq!(src.asked.load(Ordering::SeqCst), 2);

        let home = tempfile::tempdir().unwrap();
        let src = script(&[("a", true), ("b", true)]);
        let ap = Askpass::start(home.path(), src.clone(), Duration::ZERO).unwrap();
        let _run = ap.arm();
        assert_eq!(ask(&ap, &ap.token, 1).await, "OK a\n");
        assert_eq!(
            ask(&ap, &ap.token, 2).await,
            "OK b\n",
            "password_minutes = 0"
        );
    }

    #[tokio::test]
    async fn the_memory_expires_and_each_use_restarts_the_clock() {
        let home = tempfile::tempdir().unwrap();
        let src = script(&[("a", true), ("b", true)]);
        let ap = Askpass::start(home.path(), src.clone(), Duration::from_millis(150)).unwrap();
        let _run = ap.arm();
        assert_eq!(ask(&ap, &ap.token, 1).await, "OK a\n");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(ask(&ap, &ap.token, 2).await, "OK a\n", "still fresh");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            ask(&ap, &ap.token, 3).await,
            "OK a\n",
            "the use restarted the clock"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(
            ask(&ap, &ap.token, 4).await,
            "OK b\n",
            "expired: asked again"
        );
        assert_eq!(src.asked.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn the_socket_and_bin_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let ap = Askpass::start(home.path(), script(&[]), FIVE_MINUTES).unwrap();
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
