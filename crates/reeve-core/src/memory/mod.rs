//! What Reeve knows about this machine: plain Markdown files with a short
//! header, in `~/.reeve/memory/<layer>/<id>.md`, so the owner can read,
//! edit, or delete any of it.
//!
//! | layer | holds | written by |
//! | --- | --- | --- |
//! | facts | hardware, OS, layout, services, quirks | the survey; the agent; reflection |
//! | baselines | what "normal" looks like | the observer (M4) |
//! | runbooks | problem → steps → how often it worked | the agent after a verified fix; reflection |
//! | preferences | the owner's rules | the owner; proposed by the agent, confirmed by the owner |
//!
//! Search is keyword and tag scoring, no embeddings: small, inspectable,
//! and good enough at this scale.

pub mod reflect;
pub mod survey;

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Which kind of memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// Things that are true about the machine.
    Facts,
    /// What normal looks like.
    Baselines,
    /// Fixes, with their track record.
    Runbooks,
    /// The owner's rules.
    Preferences,
}

impl Layer {
    /// All four, in display order.
    pub const ALL: [Layer; 4] = [
        Layer::Facts,
        Layer::Runbooks,
        Layer::Preferences,
        Layer::Baselines,
    ];

    /// Directory name.
    pub fn dir(self) -> &'static str {
        match self {
            Self::Facts => "facts",
            Self::Baselines => "baselines",
            Self::Runbooks => "runbooks",
            Self::Preferences => "preferences",
        }
    }

    /// Parse `fact`, `facts`, `runbook`, …
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().trim_end_matches('s') {
            "fact" => Some(Self::Facts),
            "baseline" => Some(Self::Baselines),
            "runbook" => Some(Self::Runbooks),
            "preference" => Some(Self::Preferences),
            _ => None,
        }
    }
}

/// Where a note stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteStatus {
    /// In use.
    Active,
    /// Written by Reeve, in use, not yet looked at by the owner.
    New,
    /// A proposed preference: not in effect until the owner says yes.
    Pending,
    /// Kept for the record, no longer used.
    Retired,
}

impl NoteStatus {
    fn parse(s: &str) -> Self {
        match s.trim() {
            "new" => Self::New,
            "pending" => Self::Pending,
            "retired" => Self::Retired,
            _ => Self::Active,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::New => "new",
            Self::Pending => "pending",
            Self::Retired => "retired",
        }
    }

    /// Whether Reeve acts on it.
    pub fn in_use(self) -> bool {
        matches!(self, Self::Active | Self::New)
    }
}

/// One memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Note {
    /// File stem, unique within its layer.
    pub id: String,
    /// Layer.
    pub layer: Layer,
    /// One line.
    pub title: String,
    /// Lowercase keywords.
    pub tags: Vec<String>,
    /// `survey`, `user`, `agent:<session>`, `reflect:<session>`, `observer`.
    pub source: String,
    /// When it was last confirmed true.
    pub observed: DateTime<Utc>,
    /// 0–1.
    pub confidence: f32,
    /// The OS it was true on (`Fedora 44`).
    pub os: Option<String>,
    /// Status.
    pub status: NoteStatus,
    /// Runbooks: times it fixed the problem.
    pub successes: u32,
    /// Runbooks: times it didn't.
    pub failures: u32,
    /// Preferences: a rule the policy enforces (`deny-path: ~/.config/hypr/**`,
    /// `deny-command: docker restart*`).
    pub rule: Option<String>,
    /// Markdown.
    pub body: String,
}

impl Note {
    /// A new note with defaults.
    pub fn new(layer: Layer, title: &str, body: &str, source: &str) -> Self {
        Self {
            id: slug(title),
            layer,
            title: title.trim().to_string(),
            tags: Vec::new(),
            source: source.into(),
            observed: Utc::now(),
            confidence: 0.8,
            os: None,
            status: NoteStatus::Active,
            successes: 0,
            failures: 0,
            rule: None,
            body: body.trim().to_string(),
        }
    }

    /// Runbook track record, 0–1 (Laplace-smoothed).
    pub fn track(&self) -> f32 {
        (self.successes as f32 + 1.0) / (self.successes as f32 + self.failures as f32 + 2.0)
    }

    /// The file's text: header, blank line, body.
    pub fn render(&self) -> String {
        let mut h = format!(
            "---\nid: {}\nlayer: {}\ntitle: {}\ntags: [{}]\nsource: {}\nobserved: {}\nconfidence: {:.2}\nstatus: {}\n",
            self.id,
            self.layer.dir(),
            one_line(&self.title),
            self.tags.join(", "),
            one_line(&self.source),
            self.observed.to_rfc3339(),
            self.confidence,
            self.status.as_str(),
        );
        if let Some(os) = &self.os {
            h.push_str(&format!("os: {}\n", one_line(os)));
        }
        if self.layer == Layer::Runbooks {
            h.push_str(&format!(
                "successes: {}\nfailures: {}\n",
                self.successes, self.failures
            ));
        }
        if let Some(r) = &self.rule {
            h.push_str(&format!("rule: {}\n", one_line(r)));
        }
        format!("{h}---\n\n{}\n", self.body.trim_end())
    }

    /// Parse a file's text. Unknown header keys are ignored; a missing
    /// header makes the whole file the body.
    pub fn parse(layer: Layer, id: &str, text: &str) -> Self {
        let mut n = Note::new(layer, id, "", "user");
        n.id = id.into();
        let rest = match text
            .strip_prefix("---\n")
            .and_then(|t| t.split_once("\n---"))
        {
            Some((head, body)) => {
                for line in head.lines() {
                    let Some((k, v)) = line.split_once(':') else {
                        continue;
                    };
                    let v = v.trim();
                    match k.trim() {
                        "title" => n.title = v.into(),
                        "tags" => {
                            n.tags = v
                                .trim_matches(['[', ']'])
                                .split(',')
                                .map(|t| t.trim().to_ascii_lowercase())
                                .filter(|t| !t.is_empty())
                                .collect();
                        }
                        "source" => n.source = v.into(),
                        "observed" => {
                            if let Ok(t) = DateTime::parse_from_rfc3339(v) {
                                n.observed = t.with_timezone(&Utc);
                            }
                        }
                        "confidence" => n.confidence = v.parse().unwrap_or(0.8),
                        "status" => n.status = NoteStatus::parse(v),
                        "os" => n.os = Some(v.into()),
                        "successes" => n.successes = v.parse().unwrap_or(0),
                        "failures" => n.failures = v.parse().unwrap_or(0),
                        "rule" => n.rule = (!v.is_empty()).then(|| v.into()),
                        _ => {}
                    }
                }
                body.trim_start_matches('-').trim_start_matches('\n')
            }
            None => text,
        };
        n.body = rest.trim().to_string();
        if n.title.is_empty() || n.title == id {
            n.title = n
                .body
                .lines()
                .next()
                .unwrap_or(id)
                .trim_start_matches('#')
                .trim()
                .to_string();
        }
        n
    }
}

fn one_line(s: &str) -> String {
    s.replace(['\n', '\r'], " ")
}

/// `NVIDIA driver (akmods)` → `nvidia-driver-akmods`.
pub fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 60 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "note".into() } else { out }
}

/// A search hit.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    /// Score (higher is better).
    pub score: f32,
    /// The note.
    pub note: Note,
}

/// A rule compiled from a preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// Never change anything at or under this path (glob).
    DenyPath {
        /// The glob, `~` expanded.
        glob: String,
        /// The preference's title.
        because: String,
    },
    /// Never run a command matching this glob.
    DenyCommand {
        /// The glob.
        glob: String,
        /// The preference's title.
        because: String,
    },
}

/// The memory directory.
#[derive(Debug, Clone)]
pub struct Memory {
    dir: PathBuf,
}

const STOP: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "to", "in", "on", "for", "is", "it", "my", "with", "why",
    "how", "what", "does", "do", "i", "me", "this", "that", "be", "are", "was", "at", "from",
    "not",
];

fn terms(q: &str) -> Vec<String> {
    q.to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '-' && c != '.' && c != '_')
        .filter(|w| w.len() >= 2 && !STOP.contains(w))
        .map(String::from)
        .collect()
}

impl Memory {
    /// Memory under `reeve_home/memory`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("memory"),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A note's file.
    pub fn path(&self, layer: Layer, id: &str) -> PathBuf {
        self.dir.join(layer.dir()).join(format!("{id}.md"))
    }

    /// Every note in a layer, newest first.
    pub fn list(&self, layer: Layer) -> Vec<Note> {
        let Ok(rd) = fs::read_dir(self.dir.join(layer.dir())) else {
            return Vec::new();
        };
        let mut notes: Vec<Note> = rd
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                let id = p.file_stem()?.to_string_lossy().into_owned();
                (p.extension()? == "md").then_some(())?;
                Some(Note::parse(layer, &id, &fs::read_to_string(&p).ok()?))
            })
            .collect();
        notes.sort_by(|a, b| b.observed.cmp(&a.observed));
        notes
    }

    /// Every note.
    pub fn all(&self) -> Vec<Note> {
        Layer::ALL.iter().flat_map(|l| self.list(*l)).collect()
    }

    /// One note, by id, in any layer.
    pub fn get(&self, id: &str) -> Option<Note> {
        Layer::ALL.iter().find_map(|l| {
            let p = self.path(*l, id);
            fs::read_to_string(p).ok().map(|t| Note::parse(*l, id, &t))
        })
    }

    /// Write a note (atomically). A new note whose id is taken gets `-2`, `-3`, …
    pub fn put(&self, note: &mut Note, replace: bool) -> Result<PathBuf> {
        if note.id.is_empty()
            || !note
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(Error::Io(format!("bad note id {:?}", note.id)));
        }
        let dir = self.dir.join(note.layer.dir());
        fs::create_dir_all(&dir)?;
        if !replace {
            let base = note.id.clone();
            let mut n = 2;
            while self.path(note.layer, &note.id).exists() {
                note.id = format!("{base}-{n}");
                n += 1;
            }
        }
        let path = self.path(note.layer, &note.id);
        crate::undo::write_atomic(&path, note.render().as_bytes(), Some(0o600))?;
        Ok(path)
    }

    /// Remove a note's file.
    pub fn delete(&self, layer: Layer, id: &str) -> Result<()> {
        Ok(fs::remove_file(self.path(layer, id))?)
    }

    /// Notes matching `query`, best first. Retired and pending notes are left
    /// out. Runbooks rank by track record; notes from another OS version rank lower.
    pub fn search(
        &self,
        query: &str,
        layer: Option<Layer>,
        limit: usize,
        os: Option<&str>,
    ) -> Vec<Hit> {
        let words = terms(query);
        let pool: Vec<Note> = match layer {
            Some(l) => self.list(l),
            None => self.all(),
        };
        let mut hits: Vec<Hit> = pool
            .into_iter()
            .filter(|n| n.status.in_use())
            .filter_map(|n| {
                let title = n.title.to_ascii_lowercase();
                let body = n.body.to_ascii_lowercase();
                let mut score = 0.0;
                for w in &words {
                    if n.tags.iter().any(|t| t == w) {
                        score += 4.0;
                    }
                    if title.contains(w.as_str()) {
                        score += 3.0;
                    }
                    let c = body.matches(w.as_str()).count().min(5);
                    score += c as f32 * 0.6;
                }
                if words.is_empty() {
                    score = 0.1;
                }
                if score == 0.0 {
                    return None;
                }
                score *= 0.5 + n.confidence.clamp(0.0, 1.0) / 2.0;
                if n.layer == Layer::Runbooks {
                    score *= 0.5 + n.track();
                }
                if let (Some(now), Some(then)) = (os, &n.os) {
                    if now != then {
                        score *= 0.6;
                    }
                }
                Some(Hit { score, note: n })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        hits
    }

    /// The short version that goes into every system prompt: key facts,
    /// the owner's preferences in effect, and what else can be searched.
    pub fn profile(&self, max_chars: usize) -> String {
        let mut out = String::new();
        let facts: Vec<Note> = self
            .list(Layer::Facts)
            .into_iter()
            .filter(|n| n.status.in_use())
            .collect();
        let prefs: Vec<Note> = self
            .list(Layer::Preferences)
            .into_iter()
            .filter(|n| n.status.in_use())
            .collect();
        let runbooks = self
            .list(Layer::Runbooks)
            .into_iter()
            .filter(|n| n.status.in_use())
            .count();
        if !prefs.is_empty() {
            out.push_str("The owner's preferences (follow them):\n");
            for p in &prefs {
                let first = p.body.lines().next().unwrap_or("").trim();
                let rule = p
                    .rule
                    .as_ref()
                    .map(|r| format!(" [enforced: {r}]"))
                    .unwrap_or_default();
                out.push_str(&format!(
                    "- {}{}{rule}\n",
                    p.title,
                    if first.is_empty() || first == p.title {
                        String::new()
                    } else {
                        format!(": {first}")
                    }
                ));
            }
        }
        if !facts.is_empty() {
            out.push_str("Known about this machine (from memory; re-check before relying on anything that may have changed):\n");
            let mut sorted = facts;
            sorted.sort_by(|a, b| a.id.cmp(&b.id));
            for f in &sorted {
                let first = f
                    .body
                    .lines()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("")
                    .trim();
                let line = format!(
                    "- {}: {}\n",
                    f.title,
                    first.chars().take(140).collect::<String>()
                );
                if out.len() + line.len() > max_chars {
                    out.push_str("- … more facts: memory_search\n");
                    break;
                }
                out.push_str(&line);
            }
        }
        if runbooks > 0 {
            out.push_str(&format!("{runbooks} runbooks of past fixes: memory_search before diagnosing a problem from scratch.\n"));
        }
        if out.is_empty() {
            out.push_str("Memory is empty so far.\n");
        }
        out
    }

    /// Rules compiled from preferences in effect.
    pub fn rules(&self, home: &Path) -> Vec<Rule> {
        self.list(Layer::Preferences)
            .into_iter()
            .filter(|n| n.status.in_use())
            .filter_map(|n| {
                let r = n.rule?;
                let (kind, glob) = r.split_once(':')?;
                let glob = glob.trim().to_string();
                let expand = |g: &str| match g.strip_prefix("~/") {
                    Some(rest) => format!("{}/{rest}", home.display()),
                    None => g.to_string(),
                };
                match kind.trim() {
                    "deny-path" => Some(Rule::DenyPath {
                        glob: expand(&glob),
                        because: n.title,
                    }),
                    "deny-command" => Some(Rule::DenyCommand {
                        glob,
                        because: n.title,
                    }),
                    _ => None,
                }
            })
            .collect()
    }

    /// Counts for the header: (in use, new or pending).
    pub fn counts(&self) -> (usize, usize) {
        let all = self.all();
        let used = all.iter().filter(|n| n.status.in_use()).count();
        let fresh = all
            .iter()
            .filter(|n| matches!(n.status, NoteStatus::New | NoteStatus::Pending))
            .count();
        (used, fresh)
    }
}

/// `*` (any run, including `/`) and `?` glob.
pub fn glob(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    go(&p, &t)
}

impl Rule {
    /// Whether a path (resolved) or a command line trips this rule.
    pub fn blocks(&self, paths: &[PathBuf], command: Option<&str>) -> Option<String> {
        match self {
            Rule::DenyPath { glob: g, because } => {
                let base = g.trim_end_matches("/**").trim_end_matches("/*");
                let hit_path = paths.iter().any(|p| {
                    let s = p.to_string_lossy();
                    glob(g, &s) || s == base || s.starts_with(&format!("{base}/"))
                });
                let hit_cmd = command.is_some_and(|c| c.contains(base));
                (hit_path || hit_cmd)
                    .then(|| format!("the owner's preference \"{because}\" forbids changing {g}"))
            }
            Rule::DenyCommand { glob: g, because } => command
                .filter(|c| glob(g, c.trim()) || glob(&format!("*{g}*"), c))
                .map(|_| format!("the owner's preference \"{because}\" forbids `{g}`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> (tempfile::TempDir, Memory) {
        let d = tempfile::tempdir().unwrap();
        let m = Memory::new(d.path());
        (d, m)
    }

    #[test]
    fn notes_round_trip_through_their_files() {
        let (_d, m) = mem();
        let mut n = Note::new(
            Layer::Runbooks,
            "Bluetooth drops after suspend",
            "1. `systemctl restart bluetooth`\n2. check `rfkill`",
            "agent:s1",
        );
        n.tags = vec!["bluetooth".into(), "suspend".into()];
        n.successes = 3;
        n.failures = 1;
        n.os = Some("Fedora 44".into());
        m.put(&mut n, false).unwrap();
        let back = m.get("bluetooth-drops-after-suspend").unwrap();
        assert_eq!(back.title, n.title);
        assert_eq!(back.tags, n.tags);
        assert_eq!((back.successes, back.failures), (3, 1));
        assert_eq!(back.body, n.body);
        // A second note with the same title gets its own file.
        let mut dup = Note::new(
            Layer::Runbooks,
            "Bluetooth drops after suspend",
            "x",
            "user",
        );
        m.put(&mut dup, false).unwrap();
        assert_eq!(dup.id, "bluetooth-drops-after-suspend-2");
    }

    #[test]
    fn a_hand_written_file_without_a_header_still_works() {
        let n = Note::parse(
            Layer::Preferences,
            "hypr",
            "# Don't touch my Hyprland config\nI tune it by hand.",
        );
        assert_eq!(n.title, "Don't touch my Hyprland config");
        assert_eq!(n.status, NoteStatus::Active);
    }

    #[test]
    fn search_ranks_tags_titles_and_track_record() {
        let (_d, m) = mem();
        let mut good = Note::new(
            Layer::Runbooks,
            "Fix bluetooth after suspend",
            "restart the service",
            "a",
        );
        good.tags = vec!["bluetooth".into()];
        good.successes = 5;
        let mut bad = Note::new(
            Layer::Runbooks,
            "Bluetooth: reinstall bluez",
            "dnf reinstall bluez",
            "a",
        );
        bad.failures = 4;
        let mut retired = Note::new(Layer::Facts, "bluetooth adapter", "old", "a");
        retired.status = NoteStatus::Retired;
        let mut other = Note::new(Layer::Facts, "GPU", "AMD Radeon", "survey");
        for n in [&mut good, &mut bad, &mut retired, &mut other] {
            m.put(n, false).unwrap();
        }
        let hits = m.search("why does bluetooth stop working", None, 10, None);
        assert_eq!(hits[0].note.id, good.id);
        assert!(hits.iter().all(|h| h.note.status != NoteStatus::Retired));
        assert!(!hits.iter().any(|h| h.note.id == other.id));
    }

    #[test]
    fn preferences_compile_to_rules_only_once_confirmed() {
        let (_d, m) = mem();
        let mut p = Note::new(
            Layer::Preferences,
            "Never touch my Hyprland config",
            "",
            "agent",
        );
        p.rule = Some("deny-path: ~/.config/hypr/**".into());
        p.status = NoteStatus::Pending;
        m.put(&mut p, false).unwrap();
        assert!(
            m.rules(Path::new("/home/u")).is_empty(),
            "pending isn't in effect"
        );
        p.status = NoteStatus::Active;
        m.put(&mut p, true).unwrap();
        let rules = m.rules(Path::new("/home/u"));
        assert_eq!(rules.len(), 1);
        let hypr = PathBuf::from("/home/u/.config/hypr/hyprland.conf");
        assert!(rules[0].blocks(&[hypr], None).is_some());
        assert!(
            rules[0]
                .blocks(&[], Some("sed -i s/a/b/ /home/u/.config/hypr/x"))
                .is_some()
        );
        assert!(
            rules[0]
                .blocks(&[PathBuf::from("/home/u/.bashrc")], None)
                .is_none()
        );
        let cmd = Rule::DenyCommand {
            glob: "docker restart*".into(),
            because: "x".into(),
        };
        assert!(cmd.blocks(&[], Some("sudo docker restart web")).is_some());
    }

    #[test]
    fn the_profile_carries_preferences_and_facts() {
        let (_d, m) = mem();
        let mut p = Note::new(
            Layer::Preferences,
            "Prefer Flatpak for desktop apps",
            "",
            "user",
        );
        let mut f = Note::new(
            Layer::Facts,
            "GPU",
            "AMD Radeon RX 7800 XT (amdgpu)",
            "survey",
        );
        let mut pending = Note::new(Layer::Preferences, "Don't restart docker", "", "agent");
        pending.status = NoteStatus::Pending;
        for n in [&mut p, &mut f, &mut pending] {
            m.put(n, false).unwrap();
        }
        let prof = m.profile(4000);
        assert!(prof.contains("Prefer Flatpak") && prof.contains("GPU: AMD Radeon"));
        assert!(
            !prof.contains("docker"),
            "pending preferences aren't in effect: {prof}"
        );
        assert_eq!(m.counts(), (2, 1));
    }
}
