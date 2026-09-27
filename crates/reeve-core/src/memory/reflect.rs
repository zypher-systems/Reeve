//! Reflection: after a session, one model call reads what happened and
//! proposes memories. Facts must come from tool output, runbooks only
//! from fixes that were checked, and preferences only from the owner's
//! own words. Facts and runbooks are written as `new`; preferences wait as
//! `pending` until the owner says yes.

use futures_util::StreamExt;
use serde::Deserialize;

use super::{Layer, Memory, Note, NoteStatus};
use crate::error::{Error, Result};
use crate::llm::{CompletionRequest, Message, Provider, StreamDelta};
use crate::receipts::{Receipt, Status};
use crate::spend::Usage;

/// Longest session log sent for reflection (the newest part is kept).
const MAX_LOG: usize = 40_000;

const INSTRUCTIONS: &str = "You maintain the long-term memory of Reeve, an agent that manages one Linux \
computer for its owner. Read the session log and propose memories worth keeping for future sessions.\n\n\
Rules:\n\
- facts: things observed to be true about THIS machine in tool output (hardware, versions, layout, \
config choices, quirks). Not guesses, not general Linux knowledge, never secrets, keys, tokens, or \
passwords. Prefer updating an existing fact (give its id in `update`) to adding a near-duplicate.\n\
- runbooks: only for a problem that was actually worked on. `outcome` is \"worked\" only if the log \
shows the symptom was checked afterwards and was gone; \"failed\" if it was checked and wasn't; \
otherwise \"unverified\". Steps must be concrete commands or tool calls. Update an existing runbook \
when it's the same problem.\n\
- preferences: only from the owner's explicit words about how they want the machine handled (\"never \
touch X\", \"I prefer Y\"). Quote them. A `rule` is optional and must be exactly `deny-path: <glob>` or \
`deny-command: <glob>`, and only when the owner clearly forbade something.\n\
- Propose nothing rather than something weak. Most sessions yield 0–3 memories.\n\n\
Answer with only a JSON object:\n\
{\"facts\":[{\"title\":\"\",\"body\":\"\",\"tags\":[],\"update\":null}],\
\"runbooks\":[{\"title\":\"\",\"problem\":\"\",\"steps\":\"\",\"tags\":[],\"outcome\":\"worked\",\"update\":null}],\
\"preferences\":[{\"title\":\"\",\"body\":\"\",\"quote\":\"\",\"rule\":null}]}";

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct Proposals {
    facts: Vec<FactP>,
    runbooks: Vec<RunbookP>,
    preferences: Vec<PrefP>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct FactP {
    title: String,
    body: String,
    tags: Vec<String>,
    update: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RunbookP {
    title: String,
    problem: String,
    steps: String,
    tags: Vec<String>,
    outcome: String,
    update: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct PrefP {
    title: String,
    body: String,
    quote: String,
    rule: Option<String>,
}

/// What a reflection wrote.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Reflected {
    /// Facts added or updated.
    pub facts: usize,
    /// Runbooks added or updated.
    pub runbooks: usize,
    /// Preferences proposed.
    pub preferences: usize,
    /// Tokens used.
    pub usage: Usage,
    /// Provider-reported cost.
    pub reported_usd: Option<f64>,
}

impl Reflected {
    /// `2 facts, 1 runbook, 1 preference to confirm`.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        let plural = |n: usize, w: &str| format!("{n} {w}{}", if n == 1 { "" } else { "s" });
        if self.facts > 0 {
            parts.push(plural(self.facts, "fact"));
        }
        if self.runbooks > 0 {
            parts.push(plural(self.runbooks, "runbook"));
        }
        if self.preferences > 0 {
            parts.push(format!(
                "{} to confirm",
                plural(self.preferences, "preference")
            ));
        }
        if parts.is_empty() {
            "nothing new worth keeping".into()
        } else {
            parts.join(", ")
        }
    }
}

/// The session as plain text, newest part kept.
pub fn session_log(transcript: &[Message], receipts: &[Receipt]) -> String {
    let clip = |s: &str, n: usize| {
        if s.chars().count() > n {
            format!("{}…", s.chars().take(n).collect::<String>())
        } else {
            s.to_string()
        }
    };
    let mut log = String::new();
    for m in transcript {
        match m.role.as_str() {
            "user" => log.push_str(&format!("\nOWNER: {}\n", clip(&m.content, 2000))),
            "assistant" => {
                if !m.content.trim().is_empty() {
                    log.push_str(&format!("REEVE: {}\n", clip(&m.content, 1500)));
                }
                for c in m.tool_calls.iter().flatten() {
                    log.push_str(&format!("CALL {} {}\n", c.name, clip(&c.arguments, 300)));
                }
            }
            "tool" => log.push_str(&format!("RESULT: {}\n", clip(&m.content, 700))),
            _ => {}
        }
    }
    if !receipts.is_empty() {
        log.push_str("\nRECEIPTS:\n");
        for r in receipts {
            let status = match r.outcome.status {
                Status::Ok => "ok",
                Status::Error => "failed",
                Status::Denied => "declined by owner",
                Status::Refused => "refused by policy",
            };
            log.push_str(&format!(
                "#{} {} {} {}: {} — {}\n",
                r.seq,
                r.tier.label(),
                r.tool,
                clip(&r.target(), 120),
                status,
                clip(&r.outcome.summary, 160)
            ));
        }
    }
    if log.len() > MAX_LOG {
        let mut cut = log.len() - MAX_LOG;
        while !log.is_char_boundary(cut) {
            cut += 1;
        }
        log = format!("[earlier part omitted]\n{}", &log[cut..]);
    }
    log
}

fn index(mem: &Memory) -> String {
    let mut s = String::from("Existing memory (id: title):\n");
    for l in [Layer::Facts, Layer::Runbooks, Layer::Preferences] {
        for n in mem
            .list(l)
            .into_iter()
            .filter(|n| n.status != NoteStatus::Retired)
        {
            s.push_str(&format!("- [{}] {}: {}\n", l.dir(), n.id, n.title));
        }
    }
    s
}

/// Reflect on one session and write what's worth keeping.
pub async fn reflect(
    provider: &dyn Provider,
    model: &str,
    transcript: &[Message],
    receipts: &[Receipt],
    mem: &Memory,
    session: &str,
    os: &str,
) -> Result<Reflected> {
    let log = session_log(transcript, receipts);
    let req = CompletionRequest {
        model: model.into(),
        system: Some(INSTRUCTIONS.into()),
        messages: vec![Message::new(
            "user",
            format!("{}\n\nSESSION LOG:\n{log}", index(mem)),
        )],
        tools: Vec::new(),
        max_tokens: Some(2000),
        reasoning: Some("low".into()),
    };
    let mut stream = provider.stream(req).await?;
    let mut text = String::new();
    let mut out = Reflected::default();
    while let Some(d) = stream.next().await {
        match d? {
            StreamDelta::Text(t) => text.push_str(&t),
            StreamDelta::Usage(u) => out.usage = out.usage.merge(u),
            StreamDelta::ReportedCost(c) => out.reported_usd = Some(c),
            StreamDelta::Done => break,
            _ => {}
        }
    }
    let props = parse(&text)?;
    apply(mem, props, session, os, &mut out);
    Ok(out)
}

fn parse(text: &str) -> Result<Proposals> {
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else {
        return Err(Error::Provider("reflection didn't return JSON".into()));
    };
    serde_json::from_str(&text[a..=b]).map_err(|e| Error::Provider(format!("reflection JSON: {e}")))
}

/// Notes Reeve itself wrote may be updated by reflection; the owner's and
/// the survey's are left alone (a new note is added instead).
fn ours(n: &Note) -> bool {
    n.source.starts_with("reflect:") || n.source.starts_with("agent:")
}

fn tags(v: Vec<String>) -> Vec<String> {
    v.into_iter()
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty())
        .take(8)
        .collect()
}

fn valid_rule(r: &str) -> bool {
    let r = r.trim();
    (r.starts_with("deny-path:") || r.starts_with("deny-command:"))
        && r.split_once(':').is_some_and(|(_, g)| g.trim().len() >= 2)
}

fn apply(mem: &Memory, p: Proposals, session: &str, os: &str, out: &mut Reflected) {
    let source = format!("reflect:{session}");
    let os = (!os.is_empty()).then(|| os.to_string());
    for f in p.facts.into_iter().take(8) {
        if f.title.trim().is_empty() || f.body.trim().is_empty() {
            continue;
        }
        let existing = f
            .update
            .as_deref()
            .and_then(|id| mem.get(id))
            .filter(|n| n.layer == Layer::Facts && ours(n));
        let mut note = match existing {
            Some(mut n) => {
                n.body = f.body.trim().into();
                n.observed = chrono::Utc::now();
                n.status = NoteStatus::New;
                n
            }
            None => {
                let mut n = Note::new(Layer::Facts, &f.title, &f.body, &source);
                n.status = NoteStatus::New;
                n
            }
        };
        note.tags = tags(f.tags);
        note.os.clone_from(&os);
        let replace = mem
            .get(&note.id)
            .is_some_and(|n| n.layer == Layer::Facts && ours(&n));
        if mem.put(&mut note, replace).is_ok() {
            out.facts += 1;
        }
    }
    for r in p.runbooks.into_iter().take(5) {
        let worked = r.outcome == "worked";
        let failed = r.outcome == "failed";
        if !(worked || failed) || r.title.trim().is_empty() {
            continue;
        }
        let existing = r
            .update
            .as_deref()
            .and_then(|id| mem.get(id))
            .filter(|n| n.layer == Layer::Runbooks);
        let mut note = match existing {
            Some(n) => n,
            None => {
                let body = format!(
                    "## Problem\n{}\n\n## Steps\n{}\n",
                    r.problem.trim(),
                    r.steps.trim()
                );
                let mut n = Note::new(Layer::Runbooks, &r.title, &body, &source);
                n.tags = tags(r.tags);
                n.status = NoteStatus::New;
                n
            }
        };
        if worked {
            note.successes += 1;
        } else {
            note.failures += 1;
        }
        note.observed = chrono::Utc::now();
        note.os.clone_from(&os);
        let replace = mem
            .get(&note.id)
            .is_some_and(|n| n.layer == Layer::Runbooks);
        if mem.put(&mut note, replace).is_ok() {
            out.runbooks += 1;
        }
    }
    for pr in p.preferences.into_iter().take(3) {
        if pr.title.trim().is_empty() || pr.quote.trim().is_empty() {
            continue;
        }
        let body = format!("{}\n\n> {}", pr.body.trim(), pr.quote.trim());
        let mut note = Note::new(Layer::Preferences, &pr.title, &body, &source);
        note.status = NoteStatus::Pending;
        note.rule = pr
            .rule
            .filter(|r| valid_rule(r))
            .map(|r| r.trim().to_string());
        if mem.put(&mut note, false).is_ok() {
            out.preferences += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ReplayProvider;

    #[tokio::test]
    async fn proposals_land_with_the_right_status() {
        let home = tempfile::tempdir().unwrap();
        let mem = Memory::new(home.path());
        let json = r#"Here you go: {"facts":[{"title":"Swap is zram","body":"8G zram0, no disk swap","tags":["swap","zram"]}],
          "runbooks":[{"title":"Clear drkonqi failed units","problem":"drkonqi-coredump-processor units fail","steps":"systemctl reset-failed","tags":["systemd"],"outcome":"worked"},
                      {"title":"Guess","problem":"x","steps":"y","outcome":"unverified"}],
          "preferences":[{"title":"Never touch Hyprland config","body":"hands off","quote":"don't ever edit my hypr config","rule":"deny-path: ~/.config/hypr/**"},
                         {"title":"Made up","body":"x","quote":""}]}"#;
        let p = ReplayProvider::new(vec![StreamDelta::Text(json.into()), StreamDelta::Done]);
        let out = reflect(
            &p,
            "m",
            &[Message::new("user", "hi")],
            &[],
            &mem,
            "s1",
            "Fedora 44",
        )
        .await
        .unwrap();
        assert_eq!(
            (out.facts, out.runbooks, out.preferences),
            (1, 1, 1),
            "{out:?}"
        );
        let fact = mem.get("swap-is-zram").unwrap();
        assert_eq!(fact.status, NoteStatus::New);
        let rb = mem.get("clear-drkonqi-failed-units").unwrap();
        assert_eq!((rb.successes, rb.status), (1, NoteStatus::New));
        let pref = mem.get("never-touch-hyprland-config").unwrap();
        assert_eq!(pref.status, NoteStatus::Pending);
        assert_eq!(pref.rule.as_deref(), Some("deny-path: ~/.config/hypr/**"));
        assert!(
            mem.rules(home.path()).is_empty(),
            "not in effect until confirmed"
        );
        assert!(out.summary().contains("1 preference to confirm"));
    }

    #[test]
    fn the_log_keeps_the_newest_part() {
        let t: Vec<Message> = (0..2000)
            .map(|i| Message::new("user", format!("message {i} {}", "x".repeat(40))))
            .collect();
        let log = session_log(&t, &[]);
        assert!(
            log.len() <= MAX_LOG + 40
                && log.contains("message 1999")
                && !log.contains("message 3 ")
        );
    }

    #[test]
    fn owner_notes_are_never_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let mem = Memory::new(home.path());
        let mut mine = Note::new(Layer::Facts, "GPU", "my words", "user");
        mem.put(&mut mine, false).unwrap();
        let mut out = Reflected::default();
        apply(
            &mem,
            Proposals {
                facts: vec![FactP {
                    title: "GPU".into(),
                    body: "model's words".into(),
                    tags: vec![],
                    update: Some("gpu".into()),
                }],
                ..Default::default()
            },
            "s",
            "",
            &mut out,
        );
        assert_eq!(mem.get("gpu").unwrap().body, "my words");
        assert_eq!(mem.get("gpu-2").unwrap().body, "model's words");
    }
}
