//! The askpass end to end: the built `reeve` binary, run the way sudo runs
//! its askpass helper, reaches the Askpass socket and names the process
//! that asked. That is what tells one sudo from the next, so the remembered
//! password answers every new sudo and never the same one twice.

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use reeve_core::sudo::{Answer, Askpass, HELPER_NAME, PasswordSource};

/// The person: types a new password each time they're asked.
struct Person(AtomicUsize);

#[async_trait::async_trait]
impl PasswordSource for Person {
    async fn password(&self, _prompt: String) -> Option<Answer> {
        let n = self.0.fetch_add(1, Ordering::SeqCst);
        Some(Answer {
            secret: format!("typed{n}"),
            remember: true,
        })
    }
}

#[tokio::test]
async fn the_helper_names_the_sudo_that_asks() {
    let home = tempfile::tempdir().unwrap();
    let person = Arc::new(Person(AtomicUsize::new(0)));
    let ap = Askpass::start(home.path(), person.clone(), Duration::from_secs(300)).unwrap();
    // Askpass links the running binary as the helper; here that is the test
    // runner, so point the link at the real `reeve`.
    let link = home.path().join("bin").join(HELPER_NAME);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_reeve"), &link).unwrap();

    // Stands in for sudo: runs the askpass `$1` times, as sudo does once
    // per password try, each time as its own child.
    let sudo = home.path().join("sudo-stand-in");
    std::fs::write(
        &sudo,
        "#!/bin/sh\nn=$1\nwhile [ \"$n\" -gt 0 ]; do\n  \"$SUDO_ASKPASS\" '[sudo] password for me:'\n  n=$((n-1))\ndone\nexit 0\n",
    )
    .unwrap();
    std::fs::set_permissions(&sudo, std::fs::Permissions::from_mode(0o700)).unwrap();
    let env = ap.env();
    let run = |tries: u32| {
        let (sudo, env) = (sudo.clone(), env.clone());
        tokio::task::spawn_blocking(move || {
            let out = std::process::Command::new(sudo)
                .arg(tries.to_string())
                .envs(env)
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout).into_owned()
        })
    };
    let asked = || person.0.load(Ordering::SeqCst);

    assert_eq!(run(1).await.unwrap(), "", "not armed: the helper gets no");
    assert_eq!(asked(), 0);

    let armed = ap.arm();
    assert_eq!(run(1).await.unwrap(), "typed0\n");
    assert_eq!(
        run(1).await.unwrap(),
        "typed0\n",
        "the next sudo: from memory"
    );
    assert_eq!(asked(), 1);
    // One sudo asking twice refused the first answer: the person is asked.
    assert_eq!(run(2).await.unwrap(), "typed0\ntyped1\n");
    assert_eq!(asked(), 2);
    assert_eq!(
        run(1).await.unwrap(),
        "typed1\n",
        "what they typed is remembered"
    );
    assert_eq!(asked(), 2);
    drop(armed);
    assert_eq!(run(1).await.unwrap(), "", "disarmed again");
}
