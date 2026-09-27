//! Sessions on disk: `~/.reeve/sessions/<uuidv7>/` holding `meta.json`,
//! `transcript.jsonl`, and `spend.jsonl`. JSONL is the source of truth.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::ledger::{SpendRecord, append_jsonl};
use crate::llm::Message;

/// Index row for a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Meta {
    /// UUIDv7 (sorts by time).
    pub id: String,
    /// When it began.
    pub started: DateTime<Utc>,
    /// Connection at start.
    pub connection: String,
    /// Model at start.
    pub model: String,
    /// When reflection read this session (so it isn't read twice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reflected: Option<DateTime<Utc>>,
}

/// An open session.
#[derive(Debug)]
pub struct Session {
    /// Directory holding the files.
    pub dir: PathBuf,
    /// Index.
    pub meta: Meta,
}

impl Session {
    /// Create `home/sessions/<id>/`.
    pub fn create(home: &Path, connection: &str, model: &str) -> Result<Self> {
        let id = uuid::Uuid::now_v7().to_string();
        let dir = home.join("sessions").join(&id);
        fs::create_dir_all(&dir)?;
        let meta = Meta {
            id,
            started: Utc::now(),
            connection: connection.into(),
            model: model.into(),
            reflected: None,
        };
        let json = serde_json::to_string_pretty(&meta).map_err(|e| Error::Io(e.to_string()))?;
        fs::write(dir.join("meta.json"), json)?;
        Ok(Self { dir, meta })
    }

    /// Append a message to the transcript.
    pub fn append(&self, m: &Message) -> Result<()> {
        append_jsonl(&self.dir.join("transcript.jsonl"), m)
    }

    /// Append a spend row to this session's log.
    pub fn record_spend(&self, rec: &SpendRecord) -> Result<()> {
        append_jsonl(&self.dir.join("spend.jsonl"), rec)
    }
}

/// Read a session's index.
pub fn load_meta(dir: &Path) -> Option<Meta> {
    serde_json::from_str(&fs::read_to_string(dir.join("meta.json")).ok()?).ok()
}

/// Rewrite a session's index (temp file, then rename).
pub fn save_meta(dir: &Path, meta: &Meta) -> Result<()> {
    let json = serde_json::to_string_pretty(meta).map_err(|e| Error::Io(e.to_string()))?;
    crate::undo::write_atomic(&dir.join("meta.json"), json.as_bytes(), None)
}

/// A session's transcript. Torn lines are skipped.
pub fn load_transcript(dir: &Path) -> Vec<Message> {
    fs::read_to_string(dir.join("transcript.jsonl"))
        .map(|t| {
            t.lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Session directories, newest first.
pub fn list(home: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(home.join("sessions"))
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.join("meta.json").exists())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs.reverse();
    dirs
}
