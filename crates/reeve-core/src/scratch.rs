//! Each session's scratch folder, `~/.reeve/scratch/<session>`: where
//! Reeve keeps intermediate files (a package list before it becomes a
//! report). Writing there never asks, though it's receipted like everything
//! else; folders untouched for a week are cleared when an agent starts.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How long an untouched scratch folder is kept.
const KEEP: Duration = Duration::from_secs(7 * 86_400);

/// This session's scratch folder, made if missing (the parent is 0700).
pub fn ensure(reeve_home: &Path, session: &str) -> std::io::Result<PathBuf> {
    let root = reeve_home.join("scratch");
    let dir = root.join(session);
    std::fs::create_dir_all(&dir)?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(dir)
}

/// Clear scratch folders untouched for a week.
pub fn prune(reeve_home: &Path) {
    let Ok(entries) = std::fs::read_dir(reeve_home.join("scratch")) else {
        return;
    };
    let now = SystemTime::now();
    for e in entries.flatten() {
        let stale = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > KEEP);
        if stale {
            let _ = std::fs::remove_dir_all(e.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_gets_a_private_folder_and_old_ones_go() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let dir = ensure(home.path(), "s1").unwrap();
        assert!(dir.is_dir() && dir.ends_with("scratch/s1"));
        let mode = std::fs::metadata(home.path().join("scratch"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
        // A fresh folder survives pruning.
        prune(home.path());
        assert!(dir.is_dir());
    }
}
