//! Undo before do: every file Reeve changes is copied into a content-addressed
//! store first (`~/.reeve/undo/objects/<sha256>`, mode 0600), and the receipt
//! records how to put it back.
//!
//! Reverting checks that the file still holds exactly what Reeve left there,
//! so an undo never clobbers a later edit.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{Error, Result};

/// Largest file Reeve will snapshot. Bigger files can't be changed through
/// the file tools, because the change couldn't be undone.
pub const MAX_SNAPSHOT: u64 = 64 * 1024 * 1024;
/// Most files one recursive delete may snapshot.
pub const MAX_FILES: usize = 5000;

/// A stored copy of one file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Blob {
    /// Content hash (the object's name). For a symlink, of its target.
    pub sha256: String,
    /// Permission bits.
    pub mode: u32,
    /// Set when this was a symlink: its target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// One path's before and after.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Absolute path.
    pub path: String,
    /// What was there before (`None`: nothing).
    pub pre: Option<Blob>,
    /// What Reeve left (`None`: deleted).
    pub post: Option<Blob>,
    /// A root-owned file: its copies are in `/var/lib/reeve`, and putting
    /// it back goes through `sudo reeve root`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub root: bool,
}

/// How to reverse an action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Undo {
    /// Restore these paths to their `pre` state.
    Files {
        /// Changes, in the order they were made.
        changes: Vec<FileChange>,
    },
    /// Roll a package transaction back (`dnf history undo`).
    Packages {
        /// `dnf5` or `dnf`.
        manager: String,
        /// Transaction id.
        transaction: u64,
    },
    /// Put a unit back the way it was.
    Unit {
        /// Unit name.
        unit: String,
        /// A user unit (`systemctl --user`).
        user: bool,
        /// `is-enabled` before: enabled, disabled, masked, static…
        enabled: String,
        /// Was it running.
        active: bool,
    },
    /// Move `to` back to `from`; `replaced` is what `to` overwrote.
    Move {
        /// Original location.
        from: String,
        /// Where it went.
        to: String,
        /// What was at `to` before.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replaced: Option<Blob>,
    },
}

/// `sha256` in hex.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let d = Sha256::digest(bytes);
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// The object store.
#[derive(Debug, Clone)]
pub struct UndoStore {
    dir: PathBuf,
}

impl UndoStore {
    /// Store under `reeve_home/undo/objects`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("undo").join("objects"),
        }
    }

    /// Keep `bytes`; returns their hash. Already-stored content is not rewritten.
    pub fn put(&self, bytes: &[u8]) -> Result<String> {
        let sha = sha256_hex(bytes);
        let path = self.dir.join(&sha);
        if path.exists() {
            return Ok(sha);
        }
        fs::create_dir_all(&self.dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(
                self.dir.parent().unwrap_or(&self.dir),
                fs::Permissions::from_mode(0o700),
            );
        }
        let tmp = self.dir.join(format!(".{sha}.tmp"));
        {
            #[cfg(unix)]
            use std::os::unix::fs::OpenOptionsExt;
            let mut opts = fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            opts.mode(0o600);
            let mut f = opts.open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
        }
        fs::rename(tmp, path)?;
        Ok(sha)
    }

    /// Read stored content back.
    pub fn get(&self, sha: &str) -> Result<Vec<u8>> {
        if sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Io(format!("bad object id {sha:?}")));
        }
        Ok(fs::read(self.dir.join(sha))?)
    }

    /// Copy what's at `path` now. `None` when nothing is there.
    pub fn snapshot(&self, path: &Path) -> Result<Option<Blob>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let mode = mode_of(&meta);
        if meta.file_type().is_symlink() {
            let target = fs::read_link(path)?.to_string_lossy().into_owned();
            let sha = self.put(target.as_bytes())?;
            return Ok(Some(Blob {
                sha256: sha,
                mode,
                link: Some(target),
            }));
        }
        if !meta.is_file() {
            return Err(Error::Io(format!(
                "{} is not a regular file",
                path.display()
            )));
        }
        if meta.len() > MAX_SNAPSHOT {
            return Err(Error::Io(format!(
                "{} is {} MB, too large to keep an undo copy of (limit {} MB)",
                path.display(),
                meta.len() / (1024 * 1024),
                MAX_SNAPSHOT / (1024 * 1024)
            )));
        }
        let bytes = fs::read(path)?;
        Ok(Some(Blob {
            sha256: self.put(&bytes)?,
            mode,
            link: None,
        }))
    }

    /// Snapshot every file under `dir` (for a recursive delete).
    pub fn snapshot_tree(&self, dir: &Path) -> Result<Vec<FileChange>> {
        let mut out = Vec::new();
        let mut total = 0u64;
        for entry in walkdir::WalkDir::new(dir).follow_links(false) {
            let entry = entry.map_err(|e| Error::Io(e.to_string()))?;
            if entry.file_type().is_dir() {
                continue;
            }
            total += entry.metadata().map(|m| m.len()).unwrap_or(0);
            if out.len() >= MAX_FILES || total > MAX_SNAPSHOT {
                return Err(Error::Io(format!(
                    "{} holds more than {MAX_FILES} files or {} MB; too much to keep an undo copy of",
                    dir.display(),
                    MAX_SNAPSHOT / (1024 * 1024)
                )));
            }
            out.push(FileChange {
                path: entry.path().to_string_lossy().into_owned(),
                pre: self.snapshot(entry.path())?,
                post: None,
                root: false,
            });
        }
        Ok(out)
    }

    /// What `path` holds now, as a blob hash (without storing it).
    pub fn current(path: &Path) -> Result<Option<String>> {
        match fs::symlink_metadata(path) {
            Ok(m) if m.file_type().is_symlink() => Ok(Some(sha256_hex(
                fs::read_link(path)?.to_string_lossy().as_bytes(),
            ))),
            Ok(m) if m.is_file() => Ok(Some(sha256_hex(&fs::read(path)?))),
            Ok(_) => Ok(Some("<directory>".into())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Put things back. Refuses (changing nothing) if any path no longer
    /// holds what Reeve left there. Returns the inverse, so an undo can
    /// itself be undone.
    pub fn revert(&self, undo: &Undo) -> Result<(Undo, String)> {
        match undo {
            Undo::Files { changes } => {
                for c in changes {
                    let now = Self::current(Path::new(&c.path))?;
                    let left = c.post.as_ref().map(|b| b.sha256.clone());
                    if now != left {
                        return Err(Error::Io(format!(
                            "{} has changed since Reeve touched it; not undoing, to keep the newer version",
                            c.path
                        )));
                    }
                }
                let mut inverse = Vec::new();
                for c in changes.iter().rev() {
                    let path = Path::new(&c.path);
                    self.restore(path, c.pre.as_ref())?;
                    inverse.push(FileChange {
                        path: c.path.clone(),
                        pre: c.post.clone(),
                        post: c.pre.clone(),
                        root: c.root,
                    });
                }
                let summary = match changes.len() {
                    1 => format!("restored {}", changes[0].path),
                    n => format!("restored {n} files"),
                };
                Ok((Undo::Files { changes: inverse }, summary))
            }
            Undo::Packages { .. } | Undo::Unit { .. } => Err(Error::Io(
                "package and service changes are undone through their own tools, not the file store".into(),
            )),
            Undo::Move { from, to, replaced } => {
                if Path::new(from).exists() {
                    return Err(Error::Io(format!(
                        "{from} exists again; not moving over it"
                    )));
                }
                fs::rename(to, from)?;
                if let Some(b) = replaced {
                    self.restore(Path::new(to), Some(b))?;
                }
                Ok((
                    Undo::Move {
                        from: to.clone(),
                        to: from.clone(),
                        replaced: None,
                    },
                    format!("moved {to} back to {from}"),
                ))
            }
        }
    }

    fn restore(&self, path: &Path, blob: Option<&Blob>) -> Result<()> {
        let Some(b) = blob else {
            return match fs::symlink_metadata(path) {
                Ok(_) => Ok(fs::remove_file(path)?),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            };
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Some(target) = &b.link {
            let _ = fs::remove_file(path);
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, path)?;
            return Ok(());
        }
        let bytes = self.get(&b.sha256)?;
        write_atomic(path, &bytes, Some(b.mode))
    }
}

/// Write via a temp file in the same directory, then rename over.
pub fn write_atomic(path: &Path, bytes: &[u8], mode: Option<u32>) -> Result<()> {
    let dir = path.parent().unwrap_or(Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.reeve-{}", std::process::id()));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    #[cfg(unix)]
    if let Some(m) = mode {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(m & 0o7777))?;
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

fn mode_of(meta: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        0o644
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_write_can_be_undone_and_redone() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let f = home.path().join("conf");
        fs::write(&f, "old\n").unwrap();
        let pre = store.snapshot(&f).unwrap();
        fs::write(&f, "new\n").unwrap();
        let post = store.snapshot(&f).unwrap();
        let undo = Undo::Files {
            changes: vec![FileChange {
                path: f.to_string_lossy().into(),
                pre,
                post,
                root: false,
            }],
        };
        let (redo, _) = store.revert(&undo).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "old\n");
        store.revert(&redo).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "new\n");
    }

    #[test]
    fn a_later_edit_blocks_the_undo() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let f = home.path().join("conf");
        let undo = Undo::Files {
            changes: vec![FileChange {
                path: f.to_string_lossy().into(),
                pre: None,
                post: {
                    fs::write(&f, "reeve wrote this").unwrap();
                    store.snapshot(&f).unwrap()
                },
                root: false,
            }],
        };
        fs::write(&f, "then the user edited it").unwrap();
        assert!(store.revert(&undo).is_err());
        assert_eq!(fs::read_to_string(&f).unwrap(), "then the user edited it");
    }

    #[test]
    fn a_deleted_tree_comes_back() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(&home.path().join(".reeve"));
        let dir = home.path().join("old");
        fs::create_dir_all(dir.join("sub")).unwrap();
        fs::write(dir.join("a"), "1").unwrap();
        fs::write(dir.join("sub/b"), "2").unwrap();
        let changes = store.snapshot_tree(&dir).unwrap();
        fs::remove_dir_all(&dir).unwrap();
        store.revert(&Undo::Files { changes }).unwrap();
        assert_eq!(fs::read_to_string(dir.join("sub/b")).unwrap(), "2");
    }

    #[cfg(unix)]
    #[test]
    fn objects_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let sha = store.put(b"secret-ish").unwrap();
        let mode = fs::metadata(home.path().join("undo/objects").join(&sha))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(store.get("../../etc/passwd").is_err());
    }
}
