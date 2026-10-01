//! Filesystem snapshots around system changes: a snapper pre/post pair per
//! root action, described with what Reeve did, so a bad change can be
//! rolled back with `snapper undochange` even when Reeve's own undo can't.
//!
//! Only when snapper has a config for `/`. Reeve never creates one without
//! being asked.
//!
//! On Arch with snap-pac, pacman already takes a pair around every
//! transaction, so Reeve doesn't add its own around pacman: it records the
//! pair snap-pac took.

use serde::{Deserialize, Serialize};

use crate::tools::ToolCtx;
use crate::tools::shell_exec as exec;

/// The snapshots taken around one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapPair {
    /// Snapper config (usually `root`).
    pub config: String,
    /// Snapshot number before.
    pub pre: u64,
    /// Snapshot number after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post: Option<u64>,
}

/// The snapper config whose subvolume is `/`, looked up once per run.
pub async fn root_config(ctx: &ToolCtx) -> Option<String> {
    ctx.snapper
        .get_or_init(|| async {
            let out = exec(ctx, "command -v snapper >/dev/null && snapper --csvout list-configs --columns config,subvolume", false).await?;
            parse_configs(&out)
        })
        .await
        .clone()
}

fn parse_configs(csv: &str) -> Option<String> {
    csv.lines()
        .skip(1)
        .filter_map(|l| l.split_once(','))
        .find(|(_, sub)| sub.trim() == "/")
        .map(|(c, _)| c.trim().to_string())
}

fn clean(desc: &str) -> String {
    let d: String = desc
        .chars()
        .filter(|c| !c.is_control() && *c != '\'')
        .take(120)
        .collect();
    format!("reeve: {d}")
}

/// Take the "pre" snapshot. `None` when snapper isn't set up or it failed.
pub async fn pre(ctx: &ToolCtx, config: &str, desc: &str) -> Option<u64> {
    let cmd = format!(
        "sudo snapper -c {} create --type pre --print-number --cleanup-algorithm number --userdata reeve=1 --description '{}'",
        crate::distro::quote(config),
        clean(desc)
    );
    exec(ctx, &cmd, true).await?.trim().parse().ok()
}

/// Take the matching "post" snapshot.
pub async fn post(ctx: &ToolCtx, config: &str, pre: u64, desc: &str) -> Option<u64> {
    let cmd = format!(
        "sudo snapper -c {} create --type post --pre-number {pre} --print-number --cleanup-algorithm number --userdata reeve=1 --description '{}'",
        crate::distro::quote(config),
        clean(desc)
    );
    exec(ctx, &cmd, true).await?.trim().parse().ok()
}

/// snap-pac is installed and its hooks aren't masked in `/etc/pacman.d/hooks`.
pub fn snap_pac() -> bool {
    snap_pac_in(
        std::path::Path::new("/usr/share/libalpm/hooks"),
        std::path::Path::new("/etc/pacman.d/hooks"),
    )
}

fn snap_pac_in(hooks: &std::path::Path, overrides: &std::path::Path) -> bool {
    let masked = |name: &std::ffi::OsStr| {
        std::fs::read_link(overrides.join(name))
            .is_ok_and(|t| t == std::path::Path::new("/dev/null"))
    };
    std::fs::read_dir(hooks).is_ok_and(|rd| {
        rd.flatten().any(|e| {
            let n = e.file_name();
            n.to_string_lossy().contains("snap-pac") && !masked(&n)
        })
    })
}

/// The newest snapshot number in `config`, so snap-pac's new pair can be
/// told apart afterwards.
pub async fn newest(ctx: &ToolCtx, config: &str) -> Option<u64> {
    let cmd = format!(
        "sudo snapper -c {} --csvout list --columns number",
        crate::distro::quote(config)
    );
    let out = exec(ctx, &cmd, true).await?;
    out.lines()
        .skip(1)
        .filter_map(|l| l.trim().parse().ok())
        .max()
}

/// The snapshots snap-pac took after snapshot `after`: the first pre, and
/// the last post of the transactions the command ran (a repo install and
/// an AUR build are two), so `snapper undochange pre..post` covers them all.
pub async fn snap_pac_pair(ctx: &ToolCtx, config: &str, after: u64) -> Option<SnapPair> {
    let cmd = format!(
        "sudo snapper -c {} --csvout list --columns number,type,pre-number",
        crate::distro::quote(config)
    );
    let out = exec(ctx, &cmd, true).await?;
    parse_pair(&out, after).map(|(pre, post)| SnapPair {
        config: config.to_string(),
        pre,
        post,
    })
}

fn parse_pair(csv: &str, after: u64) -> Option<(u64, Option<u64>)> {
    let rows: Vec<(u64, String, Option<u64>)> = csv
        .lines()
        .skip(1)
        .filter_map(|l| {
            let mut f = l.split(',');
            let n = f.next()?.trim().parse().ok()?;
            let kind = f.next()?.trim().to_string();
            let pre = f.next().and_then(|x| x.trim().parse().ok());
            Some((n, kind, pre))
        })
        .collect();
    let pres: Vec<u64> = rows
        .iter()
        .filter(|(n, kind, _)| *n > after && kind == "pre")
        .map(|(n, ..)| *n)
        .collect();
    let pre = *pres.iter().min()?;
    let post = rows
        .iter()
        .filter(|(_, kind, p)| kind == "post" && p.is_some_and(|p| pres.contains(&p)))
        .map(|(n, ..)| *n)
        .max();
    Some((pre, post))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snap_pacs_pair_is_found_after_the_mark() {
        let csv = "number,type,pre-number\n0,single,\n40,pre,\n41,post,40\n42,pre,\n43,post,42\n44,pre,\n45,post,44\n";
        // Two transactions after the mark: from the first pre to the last post.
        assert_eq!(parse_pair(csv, 41), Some((42, Some(45))));
        assert_eq!(parse_pair(csv, 43), Some((44, Some(45))));
        assert_eq!(parse_pair(csv, 45), None);
        assert_eq!(
            parse_pair("number,type,pre-number\n50,pre,\n", 45),
            Some((50, None))
        );
    }

    #[test]
    fn a_masked_snap_pac_hook_doesnt_count() {
        let d = tempfile::tempdir().unwrap();
        let (hooks, over) = (d.path().join("hooks"), d.path().join("over"));
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::create_dir_all(&over).unwrap();
        assert!(!snap_pac_in(&hooks, &over));
        std::fs::write(hooks.join("05-snap-pac-pre.hook"), "").unwrap();
        assert!(snap_pac_in(&hooks, &over));
        std::os::unix::fs::symlink("/dev/null", over.join("05-snap-pac-pre.hook")).unwrap();
        assert!(!snap_pac_in(&hooks, &over));
    }

    #[test]
    fn finds_the_root_config() {
        assert_eq!(
            parse_configs("config,subvolume\nhome,/home\nroot,/\n"),
            Some("root".into())
        );
        assert_eq!(parse_configs("config,subvolume\n"), None);
        assert_eq!(clean("it's\nfine"), "reeve: itsfine");
    }
}
