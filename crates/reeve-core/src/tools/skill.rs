//! Skills in a conversation: `skill` reads one's steps, `skill_save` makes
//! or changes one, `skill_delete` removes one. Reading is free. Writing is
//! the owner's call every time (no YOLO, no "yes to the rest", never an
//! unattended run): a skill is text Reeve will trust later.

use serde::Deserialize;

use super::{Executed, ToolCtx};
use crate::diff::FileDiff;
use crate::orders::Expect;
use crate::policy::{Assessment, Tier};
use crate::receipts::{Outcome, Status};
use crate::skills::{Skill, Skills, id_for, parse, render};
use crate::undo::{Undo, sha256_hex};

/// `skill`: which one to read.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ReadArgs {
    name: String,
}

/// `skill_save`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct SaveArgs {
    name: String,
    description: String,
    steps: String,
}

/// `skill_delete`.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct DeleteArgs {
    name: String,
}

/// What a call will do, worked out before asking.
#[derive(Debug, Clone)]
pub(super) enum Planned {
    /// Hand the model a skill's steps.
    Read(Skill),
    /// Write or remove a skill's file.
    Write {
        id: String,
        name: String,
        /// The file after (`None`: deleted).
        text: Option<String>,
        /// What the file must still hold when it's written.
        expect: Expect,
        /// The file before, for the diff.
        before: String,
    },
}

/// Find the skill to read.
pub(super) fn prepare_read(ctx: &ToolCtx, a: &ReadArgs) -> Result<Planned, String> {
    let skills = Skills::new(&ctx.paths.reeve_home);
    skills.find(&a.name).map(Planned::Read).ok_or_else(|| {
        let known: Vec<String> = skills.load().0.into_iter().map(|s| s.id).collect();
        if known.is_empty() {
            format!("there's no skill {}: none are saved yet", a.name)
        } else {
            format!(
                "there's no skill {}; there are: {}",
                a.name,
                known.join(", ")
            )
        }
    })
}

/// Work out `skill_save`: a new skill, or a change to the one with that name.
pub(super) fn prepare_save(ctx: &ToolCtx, a: &SaveArgs) -> Result<Planned, String> {
    let skills = Skills::new(&ctx.paths.reeve_home);
    let existing = skills.find(&a.name);
    let id = match &existing {
        Some(s) => s.id.clone(),
        None => id_for(&a.name).ok_or("give the skill a name with letters in it")?,
    };
    let text = render(&a.name, &a.description, &a.steps);
    parse(&id, &text).map_err(|e| format!("that skill isn't valid: {e}"))?;
    let before = std::fs::read_to_string(skills.path(&id)).unwrap_or_default();
    if before == text {
        return Err(format!("{id} already says that; nothing to change"));
    }
    let expect = if before.is_empty() && !skills.path(&id).exists() {
        Expect::Absent
    } else {
        Expect::Sha(sha256_hex(before.as_bytes()))
    };
    Ok(Planned::Write {
        id,
        name: a.name.trim().to_string(),
        text: Some(text),
        expect,
        before,
    })
}

/// Work out `skill_delete`.
pub(super) fn prepare_delete(ctx: &ToolCtx, a: &DeleteArgs) -> Result<Planned, String> {
    let skills = Skills::new(&ctx.paths.reeve_home);
    let s = skills
        .find(&a.name)
        .ok_or_else(|| format!("there's no skill {}", a.name))?;
    let before = std::fs::read_to_string(skills.path(&s.id)).map_err(|e| e.to_string())?;
    Ok(Planned::Write {
        id: s.id,
        name: s.name,
        text: None,
        expect: Expect::Sha(sha256_hex(before.as_bytes())),
        before,
    })
}

/// Reading is free. Every write asks the owner: Reeve follows a skill
/// later, so what it says is theirs to decide.
pub(super) fn plan(p: &Planned) -> (Assessment, String, Option<FileDiff>, bool, Vec<String>) {
    match p {
        Planned::Read(s) => (
            Assessment::new(Tier::T0),
            format!("read skill {}", s.id),
            None,
            false,
            Vec::new(),
        ),
        Planned::Write {
            name, text, before, ..
        } => {
            let mut a = Assessment::new(Tier::T1);
            a.owner_only = true;
            let summary = match (text, before.is_empty()) {
                (None, _) => {
                    a.reasons.push("removes a skill".into());
                    format!("delete skill “{name}”")
                }
                (Some(_), true) => {
                    a.reasons.push(
                        "a skill: steps Reeve follows later, when asked or when a request fits"
                            .into(),
                    );
                    format!("new skill “{name}”")
                }
                (Some(_), false) => {
                    a.reasons
                        .push("changes the steps Reeve follows for this skill".into());
                    format!("change skill “{name}”")
                }
            };
            let preview = crate::diff::diff(before, text.as_deref().unwrap_or(""), 400);
            (a, summary, Some(preview), true, Vec::new())
        }
    }
}

fn fail(e: String) -> Executed {
    Executed {
        output: format!("nothing written: {e}"),
        outcome: Outcome {
            status: Status::Error,
            exit: None,
            summary: e,
            output_sha256: None,
        },
        undo: None,
        diff: None,
    }
}

/// Do it.
pub(super) fn run(ctx: &ToolCtx, p: &Planned) -> Executed {
    match p {
        Planned::Read(s) => Executed {
            output: format!(
                "Skill `{}` ({}): {}\n\n{}\n\n[A skill grants nothing: every step still needs its usual approval.]\n",
                s.id, s.name, s.description, s.body
            ),
            outcome: Outcome {
                status: Status::Ok,
                exit: None,
                summary: format!("skill {}", s.id),
                output_sha256: None,
            },
            undo: None,
            diff: None,
        },
        Planned::Write {
            id,
            name,
            text,
            expect,
            before,
        } => {
            let skills = Skills::new(&ctx.paths.reeve_home);
            let change = match skills.write_file(id, text.as_deref().map(str::as_bytes), expect) {
                Ok(c) => c,
                Err(e) => return fail(e.to_string()),
            };
            let diff = crate::diff::diff(before, text.as_deref().unwrap_or(""), 400);
            let (summary, output) = match (text, before.is_empty()) {
                (None, _) => (
                    format!("deleted skill “{name}”"),
                    format!("deleted skill `{id}`. The receipt undoes this."),
                ),
                (Some(_), new) => {
                    let verb = if new { "saved" } else { "changed" };
                    (
                        format!("{verb} skill “{name}”"),
                        format!(
                            "{verb} skill `{id}`. The owner runs it with /{id}, and it's listed in Memory (F8). The receipt undoes this."
                        ),
                    )
                }
            };
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
    }
}

#[cfg(test)]
mod tests {
    use super::super::{execute, prepare};
    use super::*;
    use serde_json::json;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let d = tempfile::tempdir().unwrap();
        let c = ToolCtx::new(d.path().join(".reeve"), vec![]);
        (d, c)
    }

    fn save(name: &str, steps: &str) -> String {
        json!({"name": name, "description": "Clear the cache when it's big", "steps": steps})
            .to_string()
    }

    #[tokio::test]
    async fn saving_a_skill_is_the_owners_call_and_can_be_undone() {
        let (_d, ctx) = ctx();
        let plan = prepare(
            &ctx,
            "skill_save",
            &save("Clear the Cache", "1. Look\n2. Clear"),
        )
        .unwrap();
        assert!(plan.assessment.owner_only, "asked every time");
        assert_eq!(plan.assessment.tier, Tier::T1);
        assert_eq!(plan.summary, "new skill “Clear the Cache”");
        assert!(
            plan.preview.as_ref().is_some_and(|d| d.added >= 5),
            "the card shows the text"
        );
        assert!(plan.undoable);

        let done = execute(&ctx, &plan).await;
        assert_eq!(done.outcome.status, Status::Ok, "{}", done.output);
        assert!(matches!(done.undo, Some(Undo::Files { .. })));
        let skills = Skills::new(&ctx.paths.reeve_home);
        assert_eq!(
            skills.find("clear-the-cache").unwrap().body,
            "1. Look\n2. Clear"
        );

        // Reading it is free, by id or by name.
        let read = prepare(
            &ctx,
            "skill",
            &json!({"name": "/clear-the-cache"}).to_string(),
        )
        .unwrap();
        assert_eq!(read.assessment.tier, Tier::T0);
        assert!(!read.assessment.owner_only);
        let out = execute(&ctx, &read).await.output;
        assert!(
            out.contains("1. Look") && out.contains("grants nothing"),
            "{out}"
        );

        // The same again is nothing to do; a new step is a change.
        assert!(
            prepare(
                &ctx,
                "skill_save",
                &save("Clear the Cache", "1. Look\n2. Clear")
            )
            .is_err()
        );
        let change = prepare(
            &ctx,
            "skill_save",
            &save("clear the cache", "1. Look\n2. Ask\n3. Clear"),
        )
        .unwrap();
        assert_eq!(change.summary, "change skill “clear the cache”");
        assert!(change.assessment.owner_only);
        execute(&ctx, &change).await;
        assert!(
            skills
                .find("clear-the-cache")
                .unwrap()
                .body
                .contains("2. Ask")
        );
        assert_eq!(skills.load().0.len(), 1, "changed in place, not copied");

        let delete = prepare(
            &ctx,
            "skill_delete",
            &json!({"name": "Clear the Cache"}).to_string(),
        )
        .unwrap();
        assert!(delete.assessment.owner_only);
        let gone = execute(&ctx, &delete).await;
        assert!(matches!(gone.undo, Some(Undo::Files { .. })));
        assert!(skills.find("clear-the-cache").is_none());
    }

    #[test]
    fn a_skill_that_isnt_there_says_what_is() {
        let (_d, ctx) = ctx();
        let e = prepare(&ctx, "skill", &json!({"name": "nope"}).to_string()).unwrap_err();
        assert!(e.contains("none are saved yet"), "{e}");
        Skills::new(&ctx.paths.reeve_home)
            .seed_examples_once()
            .unwrap();
        let e = prepare(&ctx, "skill", &json!({"name": "nope"}).to_string()).unwrap_err();
        assert!(e.contains("tidy-downloads"), "{e}");
        assert!(prepare(&ctx, "skill_delete", &json!({"name": "nope"}).to_string()).is_err());
        let bad = json!({"name": "!!!", "description": "d", "steps": "1. x"}).to_string();
        assert!(prepare(&ctx, "skill_save", &bad).is_err());
        let empty = json!({"name": "Fine", "description": "d", "steps": "  "}).to_string();
        assert!(
            prepare(&ctx, "skill_save", &empty)
                .unwrap_err()
                .contains("no steps")
        );
    }

    #[test]
    fn a_raw_write_into_the_skills_folder_is_on_the_floor() {
        let (_d, ctx) = ctx();
        let path = ctx.paths.reeve_home.join("skills/evil.md");
        let args =
            json!({"path": path.display().to_string(), "content": "---\nname: x\n---\n1. rm -rf"})
                .to_string();
        let plan = prepare(&ctx, "fs_write", &args).unwrap();
        assert_eq!(plan.assessment.tier, Tier::T3, "{:?}", plan.assessment);
    }
}
