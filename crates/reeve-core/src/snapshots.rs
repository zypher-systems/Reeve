//! Filesystem snapshots around system changes: a snapper pre/post pair per
//! root action, described with what Reeve did, so a bad change can be
//! rolled back with `snapper undochange` even when Reeve's own undo can't.
//!
//! Only when snapper has a config for `/`. Reeve never creates one without
//! being asked.

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

#[cfg(test)]
mod tests {
    use super::*;

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
