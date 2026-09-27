//! Memory tools: search, read, and write Reeve's notes about this machine.
//! Writing memory changes nothing on the machine (T0), but every write is
//! receipted, and preferences only take effect once the owner confirms them.

use serde::Deserialize;

use super::{Executed, ToolCtx};
use crate::memory::{Layer, Note, NoteStatus};
use crate::receipts::{Outcome, Status};

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SearchArgs {
    query: String,
    #[serde(default)]
    layer: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ReadArgs {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct WriteArgs {
    layer: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    tags: Vec<String>,
    /// Update this note instead of adding one.
    #[serde(default)]
    id: Option<String>,
    /// Preferences only.
    #[serde(default)]
    rule: Option<String>,
    /// Runbooks only: `worked` or `failed` this time.
    #[serde(default)]
    outcome: Option<String>,
}

fn done(output: String, summary: String) -> Executed {
    Executed {
        output,
        outcome: Outcome {
            status: Status::Ok,
            exit: None,
            summary,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    }
}

fn fail(msg: String) -> Executed {
    Executed {
        output: format!("error: {msg}"),
        outcome: Outcome {
            status: Status::Error,
            exit: None,
            summary: msg,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    }
}

pub(super) fn search(ctx: &ToolCtx, a: &SearchArgs) -> Executed {
    let layer = a.layer.as_deref().and_then(Layer::parse);
    let hits = ctx.memory.search(&a.query, layer, 8, Some(&ctx.os));
    if hits.is_empty() {
        return done("no memories match".into(), "no matches".into());
    }
    let mut out = String::new();
    for h in &hits {
        let n = &h.note;
        let track = if n.layer == Layer::Runbooks {
            format!(" · worked {}×, failed {}×", n.successes, n.failures)
        } else {
            String::new()
        };
        let stale = match &n.os {
            Some(os) if *os != ctx.os => format!(" · written on {os}, check it still applies"),
            _ => String::new(),
        };
        out.push_str(&format!(
            "[{}] {} — {}{track}{stale}\n",
            n.layer.dir(),
            n.id,
            n.title
        ));
        for l in n.body.lines().filter(|l| !l.trim().is_empty()).take(3) {
            out.push_str(&format!(
                "    {}\n",
                l.chars().take(160).collect::<String>()
            ));
        }
    }
    out.push_str("(memory_read an id for the whole note)\n");
    done(out, format!("{} matches", hits.len()))
}

pub(super) fn read(ctx: &ToolCtx, a: &ReadArgs) -> Executed {
    match ctx.memory.get(&a.id) {
        Some(n) => done(n.render(), n.title),
        None => fail(format!("no memory with id {}", a.id)),
    }
}

pub(super) fn write(ctx: &ToolCtx, a: &WriteArgs) -> Executed {
    let Some(layer) = Layer::parse(&a.layer) else {
        return fail("layer must be fact, runbook, or preference".into());
    };
    if layer == Layer::Baselines {
        return fail("baselines are written by the observer from measurements, not by hand".into());
    }
    let source = format!("agent:{}", ctx.session);
    let existing =
        a.id.as_deref()
            .and_then(|id| ctx.memory.get(id))
            .filter(|n| n.layer == layer);
    let ours = existing
        .as_ref()
        .is_some_and(|n| n.source.starts_with("agent:") || n.source.starts_with("reflect:"));
    // Only the note being updated may be replaced; a new note never
    // overwrites another (the store gives it its own id instead).
    let (mut note, updating) = match existing {
        // Counting a runbook's outcome is fine on anyone's runbook.
        Some(n) if ours || (layer == Layer::Runbooks && a.body.is_empty()) => (n, true),
        _ => {
            if a.title.trim().is_empty() || a.body.trim().is_empty() {
                return fail("a new memory needs a title and a body".into());
            }
            (Note::new(layer, &a.title, &a.body, &source), false)
        }
    };
    if !a.body.trim().is_empty() {
        note.body = a.body.trim().to_string();
    }
    if !a.tags.is_empty() {
        note.tags = a
            .tags
            .iter()
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .collect();
    }
    note.observed = chrono::Utc::now();
    note.os = Some(ctx.os.clone());
    match layer {
        Layer::Preferences => {
            // Nothing the model writes becomes a rule without the owner.
            note.status = NoteStatus::Pending;
            note.rule = a
                .rule
                .as_ref()
                .map(|r| r.trim().to_string())
                .filter(|r| r.starts_with("deny-path:") || r.starts_with("deny-command:"));
        }
        Layer::Runbooks => {
            match a.outcome.as_deref() {
                Some("worked") => note.successes += 1,
                Some("failed") => note.failures += 1,
                _ => {}
            }
            if note.status == NoteStatus::Retired || !updating {
                note.status = NoteStatus::New;
            }
        }
        _ => {
            if !updating {
                note.status = NoteStatus::New;
            }
        }
    }
    match ctx.memory.put(&mut note, updating) {
        Ok(_) => {
            let what = if layer == Layer::Preferences {
                " (pending: the owner confirms it in /memory before it takes effect)"
            } else {
                ""
            };
            done(
                format!("saved [{}] {}{what}\n", layer.dir(), note.id),
                format!("{} {}", layer.dir(), note.id),
            )
        }
        Err(e) => fail(e.to_string()),
    }
}

impl SearchArgs {
    pub(super) fn query(&self) -> &str {
        &self.query
    }
}

impl ReadArgs {
    pub(super) fn id(&self) -> &str {
        &self.id
    }
}

impl WriteArgs {
    pub(super) fn summary(&self) -> String {
        match (&self.id, &self.outcome) {
            (Some(id), Some(o)) => format!("record runbook {id}: {o}"),
            (Some(id), None) => format!("update memory {id}"),
            _ => format!("remember {}: {}", self.layer, self.title),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::memory::{Layer, Note, NoteStatus, Rule};
    use crate::policy::Tier;
    use crate::tools::{ToolCtx, execute, prepare};
    use serde_json::json;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let d = tempfile::tempdir().unwrap();
        let mut c = ToolCtx::new(d.path().to_path_buf(), vec![]);
        c.paths.home = d.path().to_path_buf();
        c.session = "s1".into();
        (d, c)
    }

    async fn run(c: &ToolCtx, tool: &str, args: serde_json::Value) -> String {
        let p = prepare(c, tool, &args.to_string()).unwrap();
        assert_eq!(p.assessment.tier, Tier::T0);
        execute(c, &p).await.output
    }

    #[tokio::test]
    async fn write_search_read_and_count_outcomes() {
        let (_d, c) = ctx();
        let out = run(&c, "memory_write", json!({"layer": "runbook", "title": "Bluetooth after suspend", "body": "restart bluetooth", "tags": ["bluetooth"], "outcome": "worked"})).await;
        assert!(
            out.contains("saved [runbooks] bluetooth-after-suspend"),
            "{out}"
        );
        let n = c.memory.get("bluetooth-after-suspend").unwrap();
        assert_eq!((n.successes, n.status), (1, NoteStatus::New));
        run(
            &c,
            "memory_write",
            json!({"layer": "runbook", "id": "bluetooth-after-suspend", "outcome": "failed"}),
        )
        .await;
        assert_eq!(c.memory.get("bluetooth-after-suspend").unwrap().failures, 1);
        let hits = run(
            &c,
            "memory_search",
            json!({"query": "bluetooth keeps dropping"}),
        )
        .await;
        assert!(hits.contains("worked 1×, failed 1×"), "{hits}");
        assert!(
            run(&c, "memory_read", json!({"id": "bluetooth-after-suspend"}))
                .await
                .contains("restart bluetooth")
        );
    }

    #[tokio::test]
    async fn the_model_cant_overwrite_the_owners_notes_or_confirm_preferences() {
        let (_d, c) = ctx();
        let mut mine = Note::new(Layer::Facts, "GPU", "mine", "user");
        c.memory.put(&mut mine, false).unwrap();
        run(
            &c,
            "memory_write",
            json!({"layer": "fact", "id": "gpu", "title": "GPU", "body": "theirs"}),
        )
        .await;
        assert_eq!(c.memory.get("gpu").unwrap().body, "mine");
        run(
            &c,
            "memory_write",
            json!({"layer": "preference", "title": "Prefer flatpak", "body": "owner said so"}),
        )
        .await;
        assert_eq!(
            c.memory.get("prefer-flatpak").unwrap().status,
            NoteStatus::Pending
        );
    }

    #[test]
    fn confirmed_preferences_refuse_matching_changes() {
        let (d, mut c) = ctx();
        c.rules = vec![Rule::DenyPath {
            glob: format!("{}/.config/hypr/**", d.path().display()),
            because: "hands off hypr".into(),
        }];
        let p = prepare(
            &c,
            "fs_write",
            &json!({"path": "~/.config/hypr/hyprland.conf", "content": "x"}).to_string(),
        )
        .unwrap();
        assert!(
            p.assessment
                .deny
                .as_deref()
                .unwrap()
                .contains("hands off hypr")
        );
        let p = prepare(
            &c,
            "shell",
            &json!({"command": format!("rm {}/.config/hypr/x", d.path().display())}).to_string(),
        )
        .unwrap();
        assert!(p.assessment.deny.is_some());
        let read = prepare(
            &c,
            "fs_read",
            &json!({"path": "~/.config/hypr/hyprland.conf"}).to_string(),
        )
        .unwrap();
        assert!(read.assessment.deny.is_none(), "reading is fine");
    }
}
