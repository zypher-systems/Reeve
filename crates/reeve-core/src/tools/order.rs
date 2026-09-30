//! Standing orders, written in a conversation: `order_save` makes or
//! changes one, `order_delete` removes one. The owner says yes to each,
//! every time: nothing (YOLO, "yes to the rest") answers for them, and
//! unattended runs can't use these at all.

use serde::Deserialize;

use super::{Executed, ToolCtx};
use crate::diff::FileDiff;
use crate::orders::{Expect, Order, Orders, limits_for, parse, render, update};
use crate::policy::{Assessment, Tier};
use crate::receipts::{Outcome, Status};
use crate::undo::{Undo, sha256_hex};

/// Tools that change things, which an order may be limited to.
pub const CHANGE_TOOLS: &[&str] = &[
    "shell",
    "fs_write",
    "fs_edit",
    "fs_move",
    "fs_delete",
    "pkg_install",
    "pkg_remove",
    "pkg_upgrade",
    "svc_control",
    "proc_signal",
];

/// `order_save` as the model sends it: every field optional, so a change
/// names only what changes.
#[derive(Debug, Clone, Default, Deserialize)]
pub(super) struct SaveArgs {
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    task: Option<String>,
    #[serde(default)]
    schedule: Option<String>,
    #[serde(default)]
    findings: Option<Vec<String>>,
    #[serde(default)]
    min_severity: Option<String>,
    #[serde(default)]
    max_tier: Option<String>,
    #[serde(default)]
    tools: Option<Vec<String>>,
    #[serde(default)]
    commands: Option<Vec<String>>,
    #[serde(default)]
    paths: Option<Vec<String>>,
    #[serde(default)]
    per_run_usd: Option<f64>,
    #[serde(default)]
    runs_per_day: Option<u32>,
    #[serde(default)]
    cooldown_hours: Option<f64>,
    #[serde(default)]
    notify: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

/// `order_delete`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct DeleteArgs {
    id: String,
}

/// What an approved call will write, worked out before asking.
#[derive(Debug, Clone)]
pub(super) struct Planned {
    id: String,
    /// The order after (`None`: deleted).
    order: Option<Order>,
    /// The file after (`None`: deleted).
    text: Option<String>,
    /// What the file must still hold when it's written.
    expect: Expect,
    /// The file before, for the diff.
    before: String,
}

/// Work out `order_save`: the order it makes, checked, and its file.
pub(super) fn prepare_save(ctx: &ToolCtx, a: &SaveArgs) -> Result<Planned, String> {
    let orders = Orders::new(&ctx.paths.reeve_home);
    let current = match &a.id {
        Some(id) => {
            let text = std::fs::read_to_string(orders.path(id))
                .map_err(|_| format!("there's no order {id}; leave id out to make a new one"))?;
            Some((id.clone(), text))
        }
        None => None,
    };
    let base = match &current {
        Some((id, text)) => Some(parse(id, text).map_err(|e| {
            format!("{id}.toml doesn't parse ({e}); the owner can fix it with E in Orders")
        })?),
        None => None,
    };
    let order = build(a, base.as_ref())?;
    let id = match &current {
        Some((id, _)) => id.clone(),
        None => orders.free_id(&order.name),
    };
    let order = Order {
        id: id.clone(),
        ..order
    };
    let (text, expect, before) = match (&current, &base) {
        (Some((_, text)), Some(base)) => {
            let up = update(text, base, &order)?;
            if up.changed.is_empty() {
                return Err(format!("{id} already says that; nothing to change"));
            }
            (
                up.text,
                Expect::Sha(sha256_hex(text.as_bytes())),
                text.clone(),
            )
        }
        _ => (render(&order), Expect::Absent, String::new()),
    };
    parse(&id, &text).map_err(|e| format!("that order isn't valid: {e}"))?;
    Ok(Planned {
        id,
        order: Some(order),
        text: Some(text),
        expect,
        before,
    })
}

/// Work out `order_delete`.
pub(super) fn prepare_delete(ctx: &ToolCtx, a: &DeleteArgs) -> Result<Planned, String> {
    let orders = Orders::new(&ctx.paths.reeve_home);
    let before = std::fs::read_to_string(orders.path(&a.id))
        .map_err(|_| format!("there's no order {}", a.id))?;
    Ok(Planned {
        id: a.id.clone(),
        order: parse(&a.id, &before).ok(),
        text: None,
        expect: Expect::Sha(sha256_hex(before.as_bytes())),
        before,
    })
}

/// The order `a` describes, on top of `base` when it changes one.
fn build(a: &SaveArgs, base: Option<&Order>) -> Result<Order, String> {
    let new = base.is_none();
    let mut o = match base {
        Some(b) => b.clone(),
        None => Order {
            id: String::new(),
            name: String::new(),
            task: String::new(),
            enabled: true,
            trigger: Default::default(),
            scope: Default::default(),
            budget: Default::default(),
            connection: None,
            model: None,
            notify: "never".into(),
        },
    };
    let text = |s: &Option<String>| s.as_deref().map(str::trim).map(String::from);
    let list = |v: &Option<Vec<String>>| {
        v.as_ref().map(|v| {
            v.iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
        })
    };
    if let Some(n) = text(&a.name) {
        o.name = n;
    }
    if let Some(t) = text(&a.task) {
        o.task = t;
    }
    if new && (o.name.is_empty() || o.task.is_empty()) {
        return Err("a new order needs a name and a task".into());
    }
    let schedule_changed = a.schedule.is_some();
    if let Some(s) = text(&a.schedule) {
        o.trigger.schedule = (!s.is_empty() && s != "none").then_some(s);
    }
    if let Some(f) = list(&a.findings) {
        o.trigger.findings = f;
    }
    if let Some(sev) = text(&a.min_severity) {
        if !["info", "warning", "critical"].contains(&sev.as_str()) {
            return Err("min_severity is info, warning, or critical".into());
        }
        o.trigger.min_severity = (sev != "info").then_some(sev);
    }
    if let Some(t) = list(&a.tools) {
        if let Some(bad) = t.iter().find(|t| !CHANGE_TOOLS.contains(&t.as_str())) {
            return Err(format!(
                "{bad} can't be an order's tool; use some of: {}",
                CHANGE_TOOLS.join(", ")
            ));
        }
        o.scope.tools = t;
    }
    if let Some(c) = list(&a.commands) {
        o.scope.commands = c;
    }
    if let Some(p) = list(&a.paths) {
        o.scope.paths = p;
    }
    o.scope.max_tier = match text(&a.max_tier).as_deref() {
        Some("T0") => Tier::T0,
        Some("T1") => Tier::T1,
        Some("T2") => Tier::T2,
        Some(other) => {
            return Err(format!(
                "max_tier {other:?}: T0 (look and report), T1 (your files, user services), or T2 (system, sudo); never T3"
            ));
        }
        // New: as much as its commands and files need, and no more.
        None if new => {
            if o.scope.commands.iter().any(|c| c.starts_with("sudo ")) {
                Tier::T2
            } else if o.scope.commands.is_empty() && o.scope.paths.is_empty() {
                Tier::T0
            } else {
                Tier::T1
            }
        }
        None => o.scope.max_tier,
    };
    if let Some(v) = a.per_run_usd {
        if !(v.is_finite() && v > 0.0) {
            return Err("per_run_usd must be more than 0".into());
        }
        o.budget.per_run_usd = v;
    }
    let limits_given = a.runs_per_day.is_some() || a.cooldown_hours.is_some();
    if let Some(n) = a.runs_per_day {
        o.budget.runs_per_day = n.max(1);
    }
    if let Some(h) = a.cooldown_hours {
        if !(h.is_finite() && h >= 0.0) {
            return Err("cooldown_hours can't be negative".into());
        }
        o.budget.cooldown_hours = h;
    }
    // Limits follow the schedule unless given: "every 30m" must be able to
    // run every 30 minutes.
    if !limits_given && (new || (schedule_changed && o.pace_warning().is_some())) {
        let (per_day, between) = limits_for(o.trigger.schedule.as_deref());
        o.budget.runs_per_day = per_day;
        o.budget.cooldown_hours = between;
    }
    if let (true, Some(w)) = (limits_given, o.pace_warning()) {
        return Err(format!(
            "{w}; leave runs_per_day and cooldown_hours out to fit them to the schedule"
        ));
    }
    if let Some(n) = text(&a.notify) {
        o.notify = n;
    }
    if let Some(on) = a.enabled {
        o.enabled = on;
    }
    Ok(o)
}

/// Every order change asks the owner: it's what Reeve does on its own.
pub(super) fn plan(p: &Planned) -> (Assessment, String, Option<FileDiff>, bool, Vec<String>) {
    let mut a = Assessment::new(Tier::T2);
    a.owner_only = true;
    let name = p.order.as_ref().map_or(p.id.as_str(), |o| o.name.as_str());
    let summary = match (&p.text, p.before.is_empty()) {
        (None, _) => {
            a.reasons
                .push("removes a standing order: reeved stops running it".into());
            format!("delete standing order “{name}”")
        }
        (Some(_), true) => {
            a.reasons
                .push("a standing order: Reeve does this on its own, unattended".into());
            format!("new standing order “{name}”")
        }
        (Some(_), false) => {
            a.reasons
                .push("changes what Reeve does on its own, unattended".into());
            format!("change standing order “{name}”")
        }
    };
    let after = p.text.as_deref().unwrap_or("");
    let preview = crate::diff::diff(&p.before, after, 400);
    (a, summary, Some(preview), true, Vec::new())
}

/// The order in plain words, for the approval card.
pub(super) fn details(p: &Planned) -> Vec<String> {
    match (&p.order, &p.text) {
        (Some(o), Some(_)) => o.describe(),
        (Some(o), None) => vec![format!("Stops: {}", o.describe().join(" "))],
        (None, _) => vec![format!("{}.toml doesn't parse; it stops either way.", p.id)],
    }
}

/// Write it.
pub(super) fn run(ctx: &ToolCtx, p: &Planned) -> Executed {
    let orders = Orders::new(&ctx.paths.reeve_home);
    let fail = |e: String| Executed {
        output: format!("nothing written: {e}"),
        outcome: Outcome {
            status: Status::Error,
            exit: None,
            summary: e,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    };
    let change = match orders.write_file(&p.id, p.text.as_deref().map(str::as_bytes), &p.expect) {
        Ok(c) => c,
        Err(e) => return fail(e.to_string()),
    };
    let diff = crate::diff::diff(&p.before, p.text.as_deref().unwrap_or(""), 400);
    let (summary, mut output) = match (&p.order, &p.text) {
        (Some(o), Some(_)) => {
            let verb = if p.before.is_empty() {
                "saved"
            } else {
                "changed"
            };
            (
                format!("{verb} standing order “{}”", o.name),
                format!(
                    "{verb} standing order `{}` ({}):\n{}\n",
                    p.id,
                    if o.enabled { "on" } else { "off" },
                    o.describe().join("\n")
                ),
            )
        }
        _ => (
            format!("deleted standing order {}", p.id),
            format!(
                "deleted standing order `{}`; reeved won't run it again.\n",
                p.id
            ),
        ),
    };
    if p.text.is_some() {
        let alive = crate::findings::ObserverStatus::load(&ctx.paths.reeve_home)
            .is_some_and(|s| s.alive(chrono::Utc::now()));
        output.push_str(if alive {
            "reeved is running and will run it when it's due. `reeve orders run <id>` runs it now.\n"
        } else {
            "reeved isn't running, so nothing will run it until it is: tell the owner to start it in /observer (or `reeve daemon install`).\n"
        });
    }
    output.push_str(
        "The owner can see and change it in Orders (F7); u there, or the receipt, undoes this.",
    );
    Executed {
        output,
        outcome: Outcome {
            status: Status::Ok,
            exit: None,
            summary,
            output_sha256: None,
        },
        undo: Some(Undo::Files {
            changes: vec![change],
        }),
        diff: Some(diff),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(v: serde_json::Value) -> SaveArgs {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_new_order_gets_limits_that_fit_its_schedule() {
        let o = build(
            &args(serde_json::json!({
                "name": "Check the backups",
                "task": "Check the last backup finished.",
                "schedule": "every 30m"
            })),
            None,
        )
        .unwrap();
        assert!(o.enabled);
        assert_eq!(o.scope.max_tier, Tier::T0, "no commands: look only");
        assert_eq!(o.budget.runs_per_day, 48);
        assert!(o.budget.cooldown_hours <= 0.5);
        assert!(o.pace_warning().is_none());
        // Limits that would slow it down are sent back.
        let e = build(
            &args(serde_json::json!({
                "name": "x", "task": "y", "schedule": "every 30m", "runs_per_day": 4
            })),
            None,
        )
        .unwrap_err();
        assert!(e.contains("less often"), "{e}");
    }

    #[test]
    fn a_new_order_takes_the_tier_its_commands_need() {
        let o = build(
            &args(serde_json::json!({
                "name": "Vacuum", "task": "t", "schedule": "weekly sun 03:00",
                "commands": ["sudo journalctl --vacuum-size=1G"]
            })),
            None,
        )
        .unwrap();
        assert_eq!(o.scope.max_tier, Tier::T2);
        assert!(o.describe().iter().any(|l| l.contains("sudoers")));
        assert!(
            build(
                &args(serde_json::json!({"name": "x", "task": "y", "schedule": "daily", "tools": ["order_save"]})),
                None
            )
            .is_err(),
            "an order can't write orders"
        );
        assert!(
            build(
                &args(serde_json::json!({"name": "x", "task": "y", "schedule": "daily", "max_tier": "T3"})),
                None
            )
            .is_err()
        );
    }

    #[test]
    fn a_change_names_only_what_changes() {
        let base = parse("keep-journal-small", crate::orders::EXAMPLES[1].1).unwrap();
        let o = build(&args(serde_json::json!({"enabled": true})), Some(&base)).unwrap();
        assert_eq!(
            o,
            Order {
                enabled: true,
                ..base.clone()
            }
        );
        // A faster schedule brings limits that let it run.
        let o = build(
            &args(serde_json::json!({"schedule": "every 1h"})),
            Some(&base),
        )
        .unwrap();
        assert!(o.pace_warning().is_none());
        assert_eq!(o.trigger.findings, base.trigger.findings);
    }
}
