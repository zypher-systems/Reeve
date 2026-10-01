//! Skills: jobs the owner wants done a particular way, by name. Each is a
//! Markdown file in `~/.reeve/skills/<id>.md` with a short header:
//!
//! ```text
//! ---
//! name: Tidy Downloads
//! description: Sort ~/Downloads into folders by kind and clear out old installers
//! ---
//! 1. List ~/Downloads …
//! ```
//!
//! Every system prompt lists the names and descriptions. Reeve reads a
//! skill's steps (the `skill` tool) when the owner names it, or when a
//! request clearly matches its description. A skill grants nothing: each
//! step still goes through the same tiers and approvals.
//!
//! Skills aren't runbooks (fixes Reeve learned, found by search) or
//! standing orders (when Reeve works unattended; an order's task can say
//! "use the skill …"). Writing one is the owner's call every time, and a
//! raw write into the folder is on the floor: a skill is text Reeve will
//! trust later.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::orders::Expect;
use crate::undo::{FileChange, UndoStore, write_atomic};

/// One skill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skill {
    /// File name without `.md`: what `/id` and the `skill` tool use.
    pub id: String,
    /// Its name, for people.
    pub name: String,
    /// One line: what it does, and so when to use it.
    pub description: String,
    /// The steps.
    pub body: String,
}

/// The skills on disk.
#[derive(Debug, Clone)]
pub struct Skills {
    dir: PathBuf,
    home: PathBuf,
}

/// The longest description the prompt carries, in characters.
const DESCRIPTION_MAX: usize = 200;

impl Skills {
    /// Skills under `reeve_home/skills`.
    pub fn new(reeve_home: &Path) -> Self {
        Self {
            dir: reeve_home.join("skills"),
            home: reeve_home.to_path_buf(),
        }
    }

    /// The directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A skill's file.
    pub fn path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.md"))
    }

    /// Every skill, by id, with any that don't parse reported separately.
    pub fn load(&self) -> (Vec<Skill>, Vec<(String, String)>) {
        let mut ok = Vec::new();
        let mut bad = Vec::new();
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return (ok, bad);
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let Some(id) = name.strip_suffix(".md") else {
                continue;
            };
            if !valid_id(id) {
                bad.push((
                    id.to_string(),
                    "the file name isn't a skill id (a-z, 0-9, -)".into(),
                ));
                continue;
            }
            match fs::read_to_string(e.path())
                .map_err(|e| e.to_string())
                .and_then(|t| parse(id, &t))
            {
                Ok(s) => ok.push(s),
                Err(why) => bad.push((id.to_string(), why)),
            }
        }
        ok.sort_by(|a, b| a.id.cmp(&b.id));
        bad.sort();
        (ok, bad)
    }

    /// One skill, by id or by name (any case).
    pub fn find(&self, which: &str) -> Option<Skill> {
        let want = which.trim().trim_start_matches('/');
        let (all, _) = self.load();
        all.iter()
            .find(|s| s.id == want)
            .or_else(|| all.iter().find(|s| s.name.eq_ignore_ascii_case(want)))
            .or_else(|| all.iter().find(|s| s.id == slug(want)))
            .cloned()
    }

    /// Write a skill's file (or, with `None`, remove it) when it holds what
    /// `expect` says, keeping what was there in the undo store.
    pub fn write_file(
        &self,
        id: &str,
        bytes: Option<&[u8]>,
        expect: &Expect,
    ) -> Result<FileChange> {
        let path = self.path(id);
        let now = UndoStore::current(&path)?;
        match (expect, &now) {
            (Expect::Absent, Some(_)) => {
                return Err(Error::Io(format!(
                    "{id}.md already exists; not writing over it"
                )));
            }
            (Expect::Sha(want), now) if now.as_deref() != Some(want.as_str()) => {
                return Err(Error::Io(format!(
                    "{id}.md changed while this was being saved; nothing written"
                )));
            }
            _ => {}
        }
        let store = UndoStore::new(&self.home);
        let pre = store.snapshot(&path)?;
        fs::create_dir_all(&self.dir)?;
        match bytes {
            Some(b) => write_atomic(&path, b, Some(0o600))?,
            None => fs::remove_file(&path)?,
        }
        Ok(FileChange {
            path: path.display().to_string(),
            pre,
            post: store.snapshot(&path)?,
            root: false,
        })
    }

    /// Write the starter skills, once: ones the owner deleted stay deleted.
    pub fn seed_examples_once(&self) -> Result<usize> {
        let mark = self.dir.join(".examples");
        if mark.exists() {
            return Ok(0);
        }
        fs::create_dir_all(&self.dir)?;
        let mut n = 0;
        for (id, text) in EXAMPLES {
            if !self.path(id).exists() {
                write_atomic(&self.path(id), text.as_bytes(), Some(0o600))?;
                n += 1;
            }
        }
        fs::write(
            &mark,
            "Reeve wrote its starter skills once. Delete this file to get them back.\n",
        )?;
        Ok(n)
    }
}

/// A skill id: lowercase letters, digits, and dashes, as `/id` types.
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 48
        && !id.starts_with('-')
        && !id.ends_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The id a name gets: `Tidy Downloads` is `tidy-downloads`. `None` when
/// the name has no letters or digits.
pub fn id_for(name: &str) -> Option<String> {
    let id = slug(name);
    valid_id(&id).then_some(id)
}

fn slug(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.chars().take(48).collect();
    out.trim_end_matches('-').to_string()
}

/// Read a skill file. Without a header, the id is the name and the first
/// line the description.
pub fn parse(id: &str, text: &str) -> std::result::Result<Skill, String> {
    let (header, body) = match text.strip_prefix("---") {
        Some(rest) => match rest.split_once("\n---") {
            Some((h, b)) => (
                h,
                b.trim_start_matches('-').trim_start_matches(['\r', '\n']),
            ),
            None => return Err("the header isn't closed: add a line with --- after it".into()),
        },
        None => ("", text),
    };
    let field = |key: &str| {
        header.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == key).then(|| v.trim().trim_matches('"').to_string())
        })
    };
    let body = body.trim().to_string();
    if body.is_empty() {
        return Err("it has no steps: write what to do under the header".into());
    }
    let name = field("name")
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| id.to_string());
    let description = field("description")
        .filter(|d| !d.is_empty())
        .or_else(|| {
            // No header: the first line says what it is.
            header.is_empty().then(|| {
                body.lines()
                    .next()
                    .unwrap_or("")
                    .trim_start_matches(['#', ' '])
                    .to_string()
            })
        })
        .filter(|d| !d.is_empty())
        .ok_or("`description` is empty: one line on what it does, so Reeve knows when to use it")?;
    Ok(Skill {
        id: id.to_string(),
        name,
        description: description.chars().take(DESCRIPTION_MAX).collect(),
        body,
    })
}

/// A skill's file.
pub fn render(name: &str, description: &str, body: &str) -> String {
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    format!(
        "---\nname: {}\ndescription: {}\n---\n{}\n",
        one_line(name),
        one_line(description),
        body.trim()
    )
}

/// The part of the system prompt that lists the skills. Empty when there
/// are none.
pub fn prompt(skills: &[Skill]) -> String {
    if skills.is_empty() {
        return String::new();
    }
    let mut out = String::from("The owner's skills (read one with the skill tool):\n");
    for s in skills {
        out.push_str(&format!("- {}: {}\n", s.id, s.description));
    }
    out
}

/// The starter skills, written once.
pub const EXAMPLES: &[(&str, &str)] = &[
    (
        "tidy-downloads",
        "---\n\
name: Tidy Downloads\n\
description: Sort ~/Downloads into folders by kind and clear out old installers\n\
---\n\
1. List ~/Downloads. Leave anything changed in the last day alone: it may still be in use.\n\
2. Move files (never folders) into folders by kind, making them as needed: Documents (pdf, docx, odt, txt, md), Images (png, jpg, gif, webp, svg), Archives (zip, tar.*, 7z), Installers (rpm, deb, AppImage, pkg.tar.*, iso), and Other for the rest.\n\
3. In Installers, list what is older than 30 days with its size, and ask before deleting any of it.\n\
4. Finish with one line per folder: how many files moved, and how much space was freed.\n",
    ),
    (
        "update-everything",
        "---\n\
name: Update everything\n\
description: Update system packages and Flatpaks, with a check before and after\n\
---\n\
1. Open a verified change (change_begin). Its checks: the units that are failed now are the only ones failed afterwards, and the root disk stays under 90%.\n\
2. Show what would update (pkg_list updates). If a kernel or the graphics stack is in it, say so before going on.\n\
3. Upgrade the system packages (pkg_upgrade with no names). On Arch, list the AUR updates apart and ask about each one.\n\
4. If Flatpak is installed, run `flatpak update -y`.\n\
5. Commit the change (change_commit) and report: what was upgraded, whether a reboot is needed, and anything that failed.\n",
    ),
    (
        "why-slow",
        "---\n\
name: Why is it slow\n\
description: Find out why the machine feels slow right now, without changing anything\n\
---\n\
Look only; change nothing.\n\n\
1. Read the load, memory, swap, and temperatures (sys_info), then the busiest processes by CPU and by memory (proc_list).\n\
2. If swap is busy or memory is nearly full, name the processes holding it.\n\
3. If the load is high while the CPU is idle, look for disk waits (`iostat -x 1 3` when it's installed) and for a full disk (sys_disk).\n\
4. Read the journal's last 15 minutes at warning and above (logs_query), and the failed units (svc_list).\n\
5. Search memory for a runbook that matches what you found (memory_search).\n\
6. Say what the cause most likely is, what the evidence is, and what you would do about it. Ask before doing any of it.\n",
    ),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_skill_file_reads_back() {
        let text = render("Tidy  Downloads", "Sort it\nout", "1. List\n2. Move\n");
        assert_eq!(
            text,
            "---\nname: Tidy Downloads\ndescription: Sort it out\n---\n1. List\n2. Move\n"
        );
        let s = parse("tidy-downloads", &text).unwrap();
        assert_eq!(
            (s.name.as_str(), s.description.as_str(), s.body.as_str()),
            ("Tidy Downloads", "Sort it out", "1. List\n2. Move")
        );
        // No header: the id names it and the first line describes it.
        let bare = parse("notes", "# Rotate the logs\n1. Do it\n").unwrap();
        assert_eq!(
            (bare.name.as_str(), bare.description.as_str()),
            ("notes", "Rotate the logs")
        );
    }

    #[test]
    fn a_skill_needs_steps_and_a_description() {
        assert!(
            parse("x", "---\nname: X\ndescription: d\n---\n")
                .unwrap_err()
                .contains("no steps")
        );
        assert!(
            parse("x", "---\nname: X\n---\n1. step\n")
                .unwrap_err()
                .contains("description")
        );
        assert!(
            parse("x", "---\nname: X\n1. step\n")
                .unwrap_err()
                .contains("isn't closed")
        );
    }

    #[test]
    fn ids_are_what_a_slash_can_type() {
        assert_eq!(id_for("Tidy Downloads!").as_deref(), Some("tidy-downloads"));
        assert_eq!(id_for("  Why is it slow? "), Some("why-is-it-slow".into()));
        assert_eq!(id_for("!!!"), None);
        for bad in ["", "Caps", "a b", "../x", "-a", "a-", "a/b"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_starter_skills_are_valid_and_written_once() {
        for (id, text) in EXAMPLES {
            let s = parse(id, text).unwrap();
            assert_eq!(id_for(&s.id).as_deref(), Some(*id));
            assert!(s.body.contains("1."), "{id}");
        }
        let d = tempfile::tempdir().unwrap();
        let skills = Skills::new(d.path());
        assert_eq!(skills.seed_examples_once().unwrap(), EXAMPLES.len());
        let (all, bad) = skills.load();
        assert_eq!((all.len(), bad.len()), (EXAMPLES.len(), 0));
        // Deleted ones stay deleted.
        fs::remove_file(skills.path("why-slow")).unwrap();
        assert_eq!(skills.seed_examples_once().unwrap(), 0);
        assert_eq!(skills.load().0.len(), EXAMPLES.len() - 1);
    }

    #[test]
    fn skills_are_found_by_id_or_name_and_listed_for_the_prompt() {
        let d = tempfile::tempdir().unwrap();
        let skills = Skills::new(d.path());
        skills.seed_examples_once().unwrap();
        assert_eq!(
            skills.find("/tidy-downloads").unwrap().name,
            "Tidy Downloads"
        );
        assert_eq!(
            skills.find("update everything").unwrap().id,
            "update-everything"
        );
        assert!(skills.find("nope").is_none());
        let p = prompt(&skills.load().0);
        assert!(
            p.contains("- why-slow: Find out why the machine feels slow"),
            "{p}"
        );
        assert_eq!(prompt(&[]), "");
        // A file that doesn't parse is reported, not listed.
        fs::write(skills.path("broken"), "---\nname: x\n").unwrap();
        fs::write(d.path().join("skills/Not An Id.md"), "x").unwrap();
        let (_, bad) = skills.load();
        assert_eq!(bad.len(), 2, "{bad:?}");
    }

    #[test]
    fn a_save_never_replaces_what_it_didnt_expect() {
        let d = tempfile::tempdir().unwrap();
        let skills = Skills::new(d.path());
        let text = render("A", "does a", "1. a");
        skills
            .write_file("a", Some(text.as_bytes()), &Expect::Absent)
            .unwrap();
        assert!(skills.path("a").exists());
        assert!(skills.write_file("a", Some(b"x"), &Expect::Absent).is_err());
        let sha = crate::undo::sha256_hex(text.as_bytes());
        assert!(
            skills
                .write_file("a", Some(b"y"), &Expect::Sha("0".repeat(64)))
                .is_err()
        );
        skills.write_file("a", None, &Expect::Sha(sha)).unwrap();
        assert!(!skills.path("a").exists());
    }
}
