//! One exclusive lock per shared file, held for as long as the handle
//! lives. It blocks until the lock is free and works across processes
//! (reeved, the TUI and `reeve` commands all write the same files), so a
//! read-modify-write done under it never drops another writer's change.

use std::fs;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use crate::error::{Error, Result};

/// Hold the lock at `path` until the returned file is dropped.
pub fn hold(path: &Path) -> Result<fs::File> {
    if let Some(d) = path.parent() {
        fs::create_dir_all(d)?;
    }
    let f = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(path)?;
    rustix::fs::flock(&f, rustix::fs::FlockOperation::LockExclusive)
        .map_err(|e| Error::Io(format!("{}: lock: {e}", path.display())))?;
    Ok(f)
}
