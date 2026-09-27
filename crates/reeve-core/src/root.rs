//! Changing root-owned files, with undo.
//!
//! The user-side Reeve runs `sudo reeve root` (this same binary) with one
//! operation as JSON on stdin. Running as root, it snapshots the file into
//! `/var/lib/reeve/undo` (root-only), changes it in place so its owner,
//! mode, and SELinux label stay as they were, and answers with the undo
//! record. Copies of root files never land in the user's home.
//!
//! `/etc/sudoers*` and `/etc/fstab` are checked with `visudo -c` and
//! `findmnt --verify` before they're written; a failed check writes nothing.

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::undo::{FileChange, Undo, UndoStore};

/// Where root-owned undo copies live.
pub const ROOT_STORE: &str = "/var/lib/reeve";

/// One root operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum RootOp {
    /// Create or replace a whole file.
    Write {
        /// Absolute path.
        path: String,
        /// New contents.
        content: String,
        /// Make missing parent directories.
        create_dirs: bool,
    },
    /// Replace exact text.
    Edit {
        /// Absolute path.
        path: String,
        /// Text to find.
        old: String,
        /// Replacement.
        new: String,
        /// Replace every occurrence.
        replace_all: bool,
    },
    /// Delete one file.
    Delete {
        /// Absolute path.
        path: String,
    },
    /// Put files back.
    Revert {
        /// From the receipt.
        undo: Undo,
    },
}

/// The answer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RootReply {
    /// It worked.
    pub ok: bool,
    /// Why not.
    pub error: Option<String>,
    /// One line.
    pub summary: String,
    /// How to reverse it.
    pub undo: Option<Undo>,
    /// Text before (for the diff), when it was text.
    pub old_text: Option<String>,
    /// Text after.
    pub new_text: Option<String>,
}

/// `reeve root`: read one op from stdin, print the reply. Exit 0 either way;
/// the reply says whether it worked.
pub fn root_main() -> i32 {
    let reply = run_stdin().unwrap_or_else(|e| RootReply {
        error: Some(e),
        ..RootReply::default()
    });
    println!(
        "{}",
        serde_json::to_string(&reply).unwrap_or_else(|_| "{}".into())
    );
    0
}

fn run_stdin() -> Result<RootReply, String> {
    if !rustix::process::geteuid().is_root() {
        return Err("`reeve root` must run as root (through sudo)".into());
    }
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .map_err(|e| e.to_string())?;
    let op: RootOp = serde_json::from_str(&input).map_err(|e| format!("bad request: {e}"))?;
    apply(&UndoStore::new(Path::new(ROOT_STORE)), op)
}

fn text_of(p: &Path) -> Option<String> {
    let bytes = fs::read(p).ok()?;
    (bytes.len() < 1 << 20 && !bytes.contains(&0))
        .then(|| String::from_utf8_lossy(&bytes).into_owned())
}

/// Do one operation with `store` for the copies. Split out from
/// [`root_main`] so it can be tested without root.
pub fn apply(store: &UndoStore, op: RootOp) -> Result<RootReply, String> {
    match op {
        RootOp::Write {
            path,
            content,
            create_dirs,
        } => {
            let p = PathBuf::from(&path);
            let old = text_of(&p);
            write_checked(store, &p, content.as_bytes(), create_dirs).map(|undo| RootReply {
                ok: true,
                summary: if old.is_some() {
                    format!("rewrote {path}")
                } else {
                    format!("created {path}")
                },
                undo: Some(undo),
                old_text: old,
                new_text: Some(content),
                error: None,
            })
        }
        RootOp::Edit {
            path,
            old,
            new,
            replace_all,
        } => {
            let p = PathBuf::from(&path);
            let before = fs::read_to_string(&p).map_err(|e| format!("{path}: {e}"))?;
            let n = before.matches(&old).count();
            let after = match n {
                0 => {
                    return Err(
                        "`old` text not found in the file; read it and copy the text exactly"
                            .into(),
                    );
                }
                1 => before.replacen(&old, &new, 1),
                _ if replace_all => before.replace(&old, &new),
                _ => {
                    return Err(format!(
                        "`old` appears {n} times; include more context, or set replace_all"
                    ));
                }
            };
            write_checked(store, &p, after.as_bytes(), false).map(|undo| RootReply {
                ok: true,
                summary: format!("edited {path}"),
                undo: Some(undo),
                old_text: Some(before),
                new_text: Some(after),
                error: None,
            })
        }
        RootOp::Delete { path } => {
            let p = PathBuf::from(&path);
            if p.is_dir() {
                return Err(format!("{path} is a directory; delete folders with shell"));
            }
            let pre = store.snapshot(&p).map_err(|e| e.to_string())?;
            let old = text_of(&p);
            fs::remove_file(&p).map_err(|e| format!("{path}: {e}"))?;
            Ok(RootReply {
                ok: true,
                summary: format!("deleted {path}"),
                undo: Some(Undo::Files {
                    changes: vec![FileChange {
                        path,
                        pre,
                        post: None,
                        root: true,
                    }],
                }),
                old_text: old,
                new_text: Some(String::new()),
                error: None,
            })
        }
        RootOp::Revert { undo } => {
            let (inverse, summary) = store.revert(&undo).map_err(|e| e.to_string())?;
            let inverse = mark_root(inverse);
            Ok(RootReply {
                ok: true,
                summary,
                undo: Some(inverse),
                ..RootReply::default()
            })
        }
    }
}

fn mark_root(u: Undo) -> Undo {
    match u {
        Undo::Files { changes } => Undo::Files {
            changes: changes
                .into_iter()
                .map(|c| FileChange { root: true, ..c })
                .collect(),
        },
        other => other,
    }
}

/// Validate (for the floor files), snapshot, write in place, snapshot again.
fn write_checked(
    store: &UndoStore,
    p: &Path,
    bytes: &[u8],
    create_dirs: bool,
) -> Result<Undo, String> {
    validate(p, bytes)?;
    let pre = store.snapshot(p).map_err(|e| e.to_string())?;
    let existed = pre.is_some();
    if let Some(parent) = p.parent() {
        if !parent.exists() {
            if !create_dirs {
                return Err(format!(
                    "{} doesn't exist; set create_dirs",
                    parent.display()
                ));
            }
            fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
    }
    // In place: the inode keeps its owner, mode, and SELinux label.
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(p)
        .map_err(|e| format!("{}: {e}", p.display()))?;
    f.write_all(bytes).map_err(|e| e.to_string())?;
    f.sync_all().map_err(|e| e.to_string())?;
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o644));
        // A new file gets its directory's default label, which restorecon fixes.
        let _ = Command::new("restorecon").arg("-F").arg(p).status();
    }
    let post = store.snapshot(p).map_err(|e| e.to_string())?;
    Ok(Undo::Files {
        changes: vec![FileChange {
            path: p.to_string_lossy().into_owned(),
            pre,
            post,
            root: true,
        }],
    })
}

/// The validators for files a typo can lock you out with.
fn validate(p: &Path, bytes: &[u8]) -> Result<(), String> {
    let s = p.to_string_lossy();
    let check: Option<Vec<String>> = if s == "/etc/sudoers" || s.starts_with("/etc/sudoers.d/") {
        Some(vec!["visudo".into(), "-cf".into()])
    } else if s == "/etc/fstab" {
        Some(vec![
            "findmnt".into(),
            "--verify".into(),
            "--tab-file".into(),
        ])
    } else {
        None
    };
    let Some(mut argv) = check else { return Ok(()) };
    let tmp = std::env::temp_dir().join(format!("reeve-check-{}", std::process::id()));
    fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
    argv.push(tmp.to_string_lossy().into_owned());
    let out = Command::new(&argv[0]).args(&argv[1..]).output();
    let _ = fs::remove_file(&tmp);
    match out {
        Ok(o) if o.status.success() => Ok(()),
        Ok(o) => Err(format!(
            "{} rejected the new {s}, so nothing was written: {}{}",
            argv[0],
            String::from_utf8_lossy(&o.stdout).trim(),
            String::from_utf8_lossy(&o.stderr).trim()
        )),
        Err(e) => Err(format!(
            "couldn't run {} to check {s}, so nothing was written: {e}",
            argv[0]
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_and_revert_through_the_same_code_root_uses() {
        let d = tempfile::tempdir().unwrap();
        let store = UndoStore::new(&d.path().join("store"));
        let f = d.path().join("hosts");
        fs::write(&f, "127.0.0.1 localhost\n").unwrap();
        let r = apply(
            &store,
            RootOp::Edit {
                path: f.to_string_lossy().into(),
                old: "localhost".into(),
                new: "localhost nexus".into(),
                replace_all: false,
            },
        )
        .unwrap();
        assert!(r.ok && r.old_text.unwrap().contains("localhost\n"));
        let undo = r.undo.unwrap();
        assert!(matches!(&undo, Undo::Files { changes } if changes[0].root));
        let back = apply(&store, RootOp::Revert { undo }).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "127.0.0.1 localhost\n");
        assert!(matches!(back.undo, Some(Undo::Files { changes }) if changes[0].root));
    }

    #[test]
    fn a_bad_edit_says_why() {
        let d = tempfile::tempdir().unwrap();
        let store = UndoStore::new(d.path());
        let f = d.path().join("x");
        fs::write(&f, "a").unwrap();
        let e = apply(
            &store,
            RootOp::Edit {
                path: f.to_string_lossy().into(),
                old: "zzz".into(),
                new: "b".into(),
                replace_all: false,
            },
        )
        .unwrap_err();
        assert!(e.contains("not found"));
    }

    #[test]
    fn root_main_refuses_without_root() {
        if !rustix::process::geteuid().is_root() {
            assert!(run_stdin().unwrap_err().contains("must run as root"));
        }
    }
}
