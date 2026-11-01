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
    /// Put pacman's changes back from its package cache (pacman has no
    /// transaction ids; see `crate::pacman`).
    Pacman {
        /// What the transaction changed, as pacman logged it.
        changes: Vec<crate::pacman::PkgChange>,
    },
    /// Put apt's changes back from its package cache, as one `apt-get`
    /// (see `crate::apt`).
    Apt {
        /// What the run changed, as dpkg logged it.
        changes: Vec<crate::pacman::PkgChange>,
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
        /// What an undo put back at `from` after moving the file away (the
        /// `replaced` of the move it reversed). Reverting this record redoes
        /// the move: it checks that file is still there unchanged and moves
        /// over it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restored: Option<Blob>,
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

    /// How `path` differs from `left`, what Reeve left there (`None`:
    /// nothing), in words; `None` when it still holds exactly that: the
    /// same content, mode, and kind (file or link).
    fn changed_since(path: &Path, left: Option<&Blob>) -> Result<Option<String>> {
        let meta = match fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(left.map(|_| "it is gone".to_string()));
            }
            Err(e) => return Err(e.into()),
        };
        let Some(b) = left else {
            return Ok(Some("something is there now".into()));
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            if b.link.is_none() {
                return Ok(Some("it is a link now".into()));
            }
            let target = fs::read_link(path)?.to_string_lossy().into_owned();
            if sha256_hex(target.as_bytes()) != b.sha256 {
                return Ok(Some("it points elsewhere".into()));
            }
        } else if kind.is_file() {
            if b.link.is_some() {
                return Ok(Some("it is a file now, not a link".into()));
            }
            if sha256_hex(&fs::read(path)?) != b.sha256 {
                return Ok(Some("its content changed".into()));
            }
        } else if kind.is_dir() {
            return Ok(Some("it is a directory now".into()));
        } else {
            return Ok(Some("it is not a regular file now".into()));
        }
        let (now, was) = (mode_of(&meta), b.mode & 0o7777);
        if now != was {
            return Ok(Some(format!("its mode is {now:04o}, was {was:04o}")));
        }
        Ok(None)
    }

    /// Put things back. Refuses (changing nothing) if any path no longer
    /// holds what Reeve left there: the same content, mode, and kind.
    /// Returns the inverse, so an undo can itself be undone.
    pub fn revert(&self, undo: &Undo) -> Result<(Undo, String)> {
        match undo {
            Undo::Files { changes } => {
                for c in changes {
                    if let Some(how) = Self::changed_since(Path::new(&c.path), c.post.as_ref())? {
                        return Err(Error::Io(format!(
                            "{} has changed since Reeve touched it ({how}); not undoing, to keep the newer version",
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
            Undo::Packages { .. } | Undo::Pacman { .. } | Undo::Apt { .. } | Undo::Unit { .. } => Err(Error::Io(

                "package and service changes are undone through their own tools, not the file store".into(),
            )),
            Undo::Move {
                from,
                to,
                replaced,
                restored,
            } => {
                let back = Path::new(from);
                match restored {
                    // Redoing a move: the file an undo put back here goes,
                    // if it's still that file.
                    Some(b) => {
                        if let Some(how) = Self::changed_since(back, Some(b))? {
                            return Err(Error::Io(format!(
                                "{from} has changed since Reeve put it back ({how}); not moving over it"
                            )));
                        }
                    }
                    None => {
                        if fs::symlink_metadata(back).is_ok() {
                            return Err(Error::Io(format!(
                                "{from} exists again; not moving over it"
                            )));
                        }
                    }
                }
                if matches!(fs::symlink_metadata(to), Err(e) if e.kind() == std::io::ErrorKind::NotFound)
                {
                    return Err(Error::Io(format!("{to} is gone; nothing to move back")));
                }
                // rename replaces a file at `from` in one step.
                fs::rename(to, from)?;
                if let Some(b) = replaced {
                    self.restore(Path::new(to), Some(b))?;
                }
                Ok((
                    Undo::Move {
                        from: to.clone(),
                        to: from.clone(),
                        replaced: restored.clone(),
                        restored: replaced.clone(),
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
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let dir = path.parent().unwrap_or(Path::new("/"));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    // A random name, created exclusively: a symlink or file planted under
    // a guessable name can't redirect the write.
    let mut opened = None;
    for _ in 0..16 {
        let tmp = dir.join(format!(".{name}.reeve-{}", random_hex(6)));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode.map_or(0o666, |m| m & 0o7777))
            .open(&tmp)
        {
            Ok(f) => {
                opened = Some((tmp, f));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let Some((tmp, mut f)) = opened else {
        return Err(Error::Io(format!(
            "couldn't create a temporary file next to {}",
            path.display()
        )));
    };
    let written = f.write_all(bytes).and_then(|()| f.sync_all());
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    if let Some(m) = mode {
        // The open above was masked by the umask; this is the exact mode,
        // set on the open file, not on a name that could have been swapped.
        if let Err(e) = f.set_permissions(fs::Permissions::from_mode(m & 0o7777)) {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
    }
    // In a directory others can write to, the name may no longer be this
    // file. Only this file is put in place.
    let same = (|| {
        use std::os::unix::fs::MetadataExt;
        let (ours, named) = (f.metadata()?, fs::symlink_metadata(&tmp)?);
        Ok::<_, std::io::Error>(ours.dev() == named.dev() && ours.ino() == named.ino())
    })();
    match same {
        Ok(true) => {}
        Ok(false) => {
            return Err(Error::Io(format!(
                "{} was swapped while being written; nothing was put in place",
                tmp.display()
            )));
        }
        Err(e) => {
            let _ = fs::remove_file(&tmp);
            return Err(e.into());
        }
    }
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}

/// `n` random bytes as hex, from the kernel.
pub(crate) fn random_hex(n: usize) -> String {
    use std::io::Read;
    let mut buf = vec![0u8; n];
    if fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut buf))
        .is_err()
    {
        // Never happens on Linux; still never a fixed name.
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        return format!("{:x}{:x}", std::process::id(), t);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
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
    fn write_atomic_never_writes_through_a_planted_name() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let victim = d.path().join("victim");
        fs::write(&victim, "keep").unwrap();
        let target = d.path().join("target");
        // The name an earlier Reeve used, planted as a link to the victim.
        let planted = d
            .path()
            .join(format!(".target.reeve-{}", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        write_atomic(&target, b"new", Some(0o600)).unwrap();
        assert_eq!(fs::read_to_string(&victim).unwrap(), "keep");
        assert_eq!(fs::read_to_string(&target).unwrap(), "new");
        let meta = fs::symlink_metadata(&target).unwrap();
        assert!(
            meta.file_type().is_file(),
            "the target is a file, not a link"
        );
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        let left: Vec<String> = fs::read_dir(d.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with(".target.reeve-"))
            .collect();
        assert_eq!(left.len(), 1, "only the plant remains: {left:?}");
        // Without a mode, the file follows the umask like any new file.
        write_atomic(&d.path().join("plain"), b"x", None).unwrap();
        assert!(d.path().join("plain").is_file());
    }

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

    #[test]
    fn a_later_chmod_blocks_the_undo() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let f = home.path().join("conf");
        fs::write(&f, "old\n").unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
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
        // The owner locks the file down after Reeve's edit.
        fs::set_permissions(&f, fs::Permissions::from_mode(0o600)).unwrap();
        let e = store.revert(&undo).unwrap_err().to_string();
        assert!(e.contains("its mode is 0600, was 0644"), "{e}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "new\n");
        assert_eq!(
            fs::metadata(&f).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // Put back as it was, the undo goes through and restores the old mode too.
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        store.revert(&undo).unwrap();
        assert_eq!(fs::read_to_string(&f).unwrap(), "old\n");
    }

    #[test]
    fn a_file_that_became_a_link_blocks_the_undo() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let f = home.path().join("conf");
        let undo = Undo::Files {
            changes: vec![FileChange {
                path: f.to_string_lossy().into(),
                pre: None,
                post: {
                    fs::write(&f, "target").unwrap();
                    store.snapshot(&f).unwrap()
                },
                root: false,
            }],
        };
        // A link whose target reads the same as the file's content: the
        // same hash, a different kind of thing.
        fs::remove_file(&f).unwrap();
        std::os::unix::fs::symlink("target", &f).unwrap();
        let e = store.revert(&undo).unwrap_err().to_string();
        assert!(e.contains("it is a link now"), "{e}");
        assert!(fs::symlink_metadata(&f).unwrap().file_type().is_symlink());
    }

    #[test]
    fn an_undone_overwriting_move_can_be_redone() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::new(home.path());
        let (a, b) = (home.path().join("a"), home.path().join("b"));
        fs::write(&a, "A\n").unwrap();
        fs::write(&b, "B\n").unwrap();
        let replaced = store.snapshot(&b).unwrap();
        fs::rename(&a, &b).unwrap();
        let undo = Undo::Move {
            from: a.to_string_lossy().into(),
            to: b.to_string_lossy().into(),
            replaced,
            restored: None,
        };
        let (redo, _) = store.revert(&undo).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "A\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "B\n");
        // The replaced file was edited since the undo put it back: no redo over it.
        fs::write(&b, "B2\n").unwrap();
        let e = store.revert(&redo).unwrap_err().to_string();
        assert!(
            e.contains("since Reeve put it back (its content changed)"),
            "{e}"
        );
        fs::write(&b, "B\n").unwrap();
        let (undo_again, _) = store.revert(&redo).unwrap();
        assert!(!a.exists(), "moved away again");
        assert_eq!(fs::read_to_string(&b).unwrap(), "A\n");
        assert_eq!(undo_again, undo, "the redo's inverse is the original undo");
        store.revert(&undo_again).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "A\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "B\n");
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
