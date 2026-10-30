//! `reeve orders run` on a state file that doesn't parse: the run doesn't
//! start, and the file is left as it is rather than replaced with a state
//! that remembers nothing.

use std::process::Command;

#[test]
fn orders_run_refuses_on_a_state_file_it_cannot_read() {
    let home = tempfile::tempdir().unwrap();
    let reeve = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_reeve"))
            .args(args)
            .env("REEVE_HOME", home.path())
            .env("HOME", home.path())
            .output()
            .unwrap()
    };
    let out = reeve(&["orders", "examples"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let state = home.path().join("orders").join(".state.json");
    std::fs::write(&state, "{ not json").unwrap();

    let out = reeve(&["orders", "run", "reset-crash-units"]);
    assert!(!out.status.success(), "the run started");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains(".state.json") && err.contains("nothing runs until it's fixed"),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(&state).unwrap(),
        "{ not json",
        "the file was left alone"
    );

    // Listing still shows the orders, and says why reeved starts nothing.
    let out = reeve(&["orders", "list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warning:") && text.contains("reset-crash-units"),
        "{text}"
    );
}
