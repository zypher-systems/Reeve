//! Risk tiers: what an action can break decides who has to say yes.
//!
//! | tier | meaning | default |
//! | --- | --- | --- |
//! | T0 | observe: reads, no side effects | runs |
//! | T1 | reversible changes as the user | asks; can be allowed for the session |
//! | T2 | system state, or anything needing root | asks every time |
//! | T3 | the floor: could destroy the system or leak secrets | typed `yes`, even in YOLO |
//!
//! Some actions are refused outright ([`Assessment::deny`]): reading Reeve's
//! own keys, or rewriting its receipts.

pub mod paths;
pub mod shell;

use serde::{Deserialize, Serialize};

pub use paths::{PathClass, PathCtx};

/// How risky an action is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Tier {
    /// Observe.
    T0,
    /// User-level change.
    T1,
    /// System change.
    T2,
    /// The safeguard floor.
    T3,
}

impl Tier {
    /// `T0`…`T3`.
    pub fn label(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
        }
    }

    /// One word for the tier.
    pub fn name(self) -> &'static str {
        match self {
            Self::T0 => "observe",
            Self::T1 => "user change",
            Self::T2 => "system change",
            Self::T3 => "floor",
        }
    }
}

/// The policy's verdict on one action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Assessment {
    /// Tier (the most restrictive of every part of the action).
    pub tier: Tier,
    /// Why, in a few words each. Shown on the approval card and kept in the receipt.
    pub reasons: Vec<String>,
    /// Refused outright, with the reason.
    pub deny: Option<String>,
    /// The action asks for root (`sudo`).
    pub sudo: bool,
    /// What its T1 parts do, as keys "allow for this session" can remember
    /// (`write:~/Documents`, `delete:/tmp`, `run:flatpak`).
    #[serde(default)]
    pub keys: Vec<String>,
    /// A T1 part has no key: it can only be allowed as this exact action.
    #[serde(default)]
    pub unkeyed: bool,
    /// It writes, but only where writing needs no yes (the session's
    /// scratch folder, a new file in /tmp). Unattended runs still count it.
    #[serde(default)]
    pub quiet_write: bool,
    /// Only the owner, asked each time, can say yes: no YOLO, no "yes to
    /// the rest", no session rule, and never an unattended run. For what
    /// Reeve would do on its own later (standing orders).
    #[serde(default)]
    pub owner_only: bool,
}

impl Assessment {
    /// A tier with nothing to say.
    pub fn new(tier: Tier) -> Self {
        Self {
            tier,
            reasons: Vec::new(),
            deny: None,
            sudo: false,
            keys: Vec::new(),
            unkeyed: false,
            quiet_write: false,
            owner_only: false,
        }
    }

    /// Raise to at least `tier`, noting why. A T1 raise this way has no
    /// key, so the action can't be allowed for the session by what it does.
    pub fn raise(&mut self, tier: Tier, why: impl Into<String>) {
        if tier == Tier::T1 {
            self.unkeyed = true;
        }
        self.lift(tier, why.into());
    }

    /// Raise to at least `tier`, noting why, with the key a T1 yes for the
    /// session is remembered by.
    pub fn raise_keyed(&mut self, tier: Tier, why: impl Into<String>, key: impl Into<String>) {
        if tier == Tier::T1 {
            let key = key.into();
            if !self.keys.contains(&key) {
                self.keys.push(key);
            }
        }
        self.lift(tier, why.into());
    }

    fn lift(&mut self, tier: Tier, why: String) {
        if tier > self.tier {
            self.tier = tier;
        }
        if tier > Tier::T0 && !why.is_empty() && !self.reasons.contains(&why) {
            self.reasons.push(why);
        }
    }

    /// Rename keys starting `from` to start `to` instead (`write:` →
    /// `delete:` for a removal).
    pub fn rekey(&mut self, from: &str, to: &str) {
        for k in &mut self.keys {
            if let Some(rest) = k.strip_prefix(from) {
                *k = format!("{to}{rest}");
            }
        }
    }

    /// What "allow for this session" remembers for this action: its keys,
    /// when it's T1 and every part of it has one.
    pub fn session_keys(&self) -> Option<Vec<String>> {
        (self.tier == Tier::T1 && !self.unkeyed && !self.keys.is_empty() && self.deny.is_none())
            .then(|| self.keys.clone())
    }

    /// Refuse, keeping the first reason given.
    pub fn refuse(&mut self, why: impl Into<String>) {
        if self.deny.is_none() {
            self.deny = Some(why.into());
        }
    }

    /// Fold another assessment in (compound commands).
    pub fn merge(&mut self, other: Assessment) {
        for r in other.reasons {
            if !self.reasons.contains(&r) {
                self.reasons.push(r);
            }
        }
        for k in other.keys {
            if !self.keys.contains(&k) {
                self.keys.push(k);
            }
        }
        self.unkeyed |= other.unkeyed;
        self.quiet_write |= other.quiet_write;
        self.tier = self.tier.max(other.tier);
        self.sudo |= other.sudo;
        if self.deny.is_none() {
            self.deny = other.deny;
        }
    }
}

/// Assess a read of `path`.
pub fn read(ctx: &PathCtx, path: &str) -> Assessment {
    let mut a = Assessment::new(Tier::T0);
    let p = ctx.resolve(path);
    match ctx.classify(&p) {
        PathClass::ReeveKeys => a.refuse("Reeve's own API keys are never readable by tools"),
        PathClass::Sensitive(what) => a.raise(
            Tier::T3,
            format!("reads {what}, which would be sent to the model provider"),
        ),
        _ => {}
    }
    a
}

/// Assess a write, move target, or delete of `path`.
pub fn write(ctx: &PathCtx, path: &str) -> Assessment {
    let mut a = Assessment::new(Tier::T0);
    let p = ctx.resolve(path);
    match ctx.classify(&p) {
        PathClass::ReeveKeys | PathClass::ReeveAudit => {
            a.raise(Tier::T1, "");
            a.refuse("Reeve's keys, receipts, and undo store can't be changed by tools");
        }
        PathClass::FloorFile(what) => a.raise(
            Tier::T3,
            format!("changes {what}; a mistake here can stop the system booting or lock you out"),
        ),
        PathClass::FloorDir(what) => a.raise(Tier::T3, format!("targets {what} itself")),
        PathClass::Sensitive(what) => a.raise(Tier::T2, format!("changes {what}")),
        // Reeve's own scratch folder for the session: nothing of yours is there.
        PathClass::Scratch => a.quiet_write = true,
        // A file in /tmp that doesn't exist yet can't clobber anything.
        PathClass::Temp if !p.exists() => a.quiet_write = true,
        PathClass::Home | PathClass::Temp => a.raise_keyed(
            Tier::T1,
            "changes your files",
            format!("write:{}", ctx.show_dir(&p)),
        ),
        PathClass::System => a.raise(
            Tier::T2,
            format!("changes system files ({})", short_dir(&p)),
        ),
    }
    a
}

/// What a session key means, for the approval card: `writes in ~/notes`.
pub fn describe_key(key: &str) -> String {
    match key.split_once(':') {
        Some(("write", dir)) => format!("writes in {dir}"),
        Some(("delete", dir)) => format!("deletes in {dir}"),
        Some(("perms", dir)) => format!("permission changes in {dir}"),
        Some(("run", prog)) => format!("runs `{prog}`"),
        Some(("shell", _)) => "this exact command".into(),
        _ => key.to_string(),
    }
}

/// Assess a recursive delete (or recursive chmod/chown) rooted at `path`.
pub fn recursive(ctx: &PathCtx, path: &str) -> Assessment {
    let mut a = write(ctx, path);
    let p = ctx.resolve(path);
    if ctx.is_floor_root(&p) {
        a.raise(Tier::T3, format!("recursive change of {}", p.display()));
    }
    a
}

fn short_dir(p: &std::path::Path) -> String {
    let mut comps = p.components();
    let first = comps
        .nth(1)
        .map(|c| c.as_os_str().to_string_lossy().into_owned());
    match first {
        Some(f) => format!("/{f}"),
        None => "/".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_order_and_merge_to_the_worst() {
        assert!(Tier::T3 > Tier::T2 && Tier::T1 > Tier::T0);
        let mut a = Assessment::new(Tier::T0);
        let mut b = Assessment::new(Tier::T0);
        b.raise(Tier::T2, "installs packages");
        a.merge(b);
        assert_eq!(a.tier, Tier::T2);
        assert_eq!(a.reasons, vec!["installs packages"]);
        a.raise(Tier::T1, "lower");
        assert_eq!(a.tier, Tier::T2, "raise never lowers");
    }

    #[test]
    fn reads_writes_and_refusals() {
        let ctx = PathCtx::for_tests();
        assert_eq!(read(&ctx, "/etc/fstab").tier, Tier::T0);
        assert_eq!(read(&ctx, "~/.ssh/id_ed25519").tier, Tier::T3);
        assert_eq!(read(&ctx, "~/.ssh/id_ed25519.pub").tier, Tier::T0);
        assert!(read(&ctx, "/home/u/.reeve/keys/openrouter").deny.is_some());
        assert_eq!(write(&ctx, "~/.bashrc").tier, Tier::T1);
        assert_eq!(write(&ctx, "~/.bashrc").keys, vec!["write:~"]);
        // A new file in /tmp can't clobber anything; an existing one can.
        assert_eq!(
            write(&ctx, "/tmp/reeve-test-surely-not-there-3f9a").tier,
            Tier::T0
        );
        assert_eq!(write(&ctx, "/etc/hosts").tier, Tier::T2);
        assert_eq!(write(&ctx, "/etc/fstab").tier, Tier::T3);
        assert!(
            write(&ctx, "~/.reeve/receipts/2026-09.jsonl")
                .deny
                .is_some()
        );
        assert_eq!(recursive(&ctx, "~").tier, Tier::T3);
        assert_eq!(recursive(&ctx, "/usr").tier, Tier::T3);
        assert_eq!(recursive(&ctx, "~/Downloads/old").tier, Tier::T1);
    }

    #[test]
    fn the_session_scratch_folder_never_asks() {
        let ctx = PathCtx::for_tests();
        for p in [
            "/home/u/.reeve/scratch/test/list.tsv",
            "$REEVE_SCRATCH/list.tsv",
            "${REEVE_SCRATCH}/out/a.md",
        ] {
            assert_eq!(write(&ctx, p).tier, Tier::T0, "{p}");
        }
        assert_eq!(recursive(&ctx, "$REEVE_SCRATCH/out").tier, Tier::T0);
        // Other sessions' scratch, and Reeve's own files, are not scratch.
        assert_eq!(write(&ctx, "~/.reeve/scratch/other/x").tier, Tier::T1);
    }

    #[test]
    fn keys_merge_and_say_what_they_allow() {
        let mut a = Assessment::new(Tier::T0);
        a.raise_keyed(Tier::T1, "changes your files", "write:~/notes");
        let mut b = Assessment::new(Tier::T0);
        b.raise_keyed(Tier::T1, "runs flatpak", "run:flatpak");
        a.merge(b);
        assert_eq!(
            a.session_keys(),
            Some(vec!["write:~/notes".to_string(), "run:flatpak".to_string()])
        );
        a.raise(Tier::T1, "something with no key");
        assert_eq!(a.session_keys(), None);
        assert_eq!(describe_key("write:~/notes"), "writes in ~/notes");
        assert_eq!(describe_key("run:flatpak"), "runs `flatpak`");
    }
}
