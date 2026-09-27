//! `~/.reeve/settings.toml`: choices made inside the TUI (default
//! connection, a model per connection, connections added from
//! `/providers`). Layered over `config.toml`, which Reeve never rewrites.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::{Config, ConnectionConfig};
use crate::error::{Error, Result};

/// What the TUI remembers.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Connection to use by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_connection: Option<String>,
    /// Chosen model per connection.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    /// Connections added in the TUI.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub connections: BTreeMap<String, ConnectionConfig>,
    /// The drafter, as set in `/observer`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drafter: Option<crate::config::DrafterConfig>,
}

impl Settings {
    /// Path under `home`.
    pub fn path(home: &Path) -> PathBuf {
        home.join("settings.toml")
    }

    /// Read, or empty when missing.
    pub fn load(home: &Path) -> Result<Self> {
        let path = Self::path(home);
        match fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }

    /// Write atomically (temp file, then rename). Never holds a key: keys
    /// live in `keys/`.
    pub fn save(&self, home: &Path) -> Result<()> {
        fs::create_dir_all(home)?;
        let mut clean = self.clone();
        for c in clean.connections.values_mut() {
            c.api_key = None;
        }
        let text = toml::to_string_pretty(&clean).map_err(|e| Error::Config(e.to_string()))?;
        let path = Self::path(home);
        let tmp = path.with_extension("toml.tmp");
        fs::write(
            &tmp,
            format!("# Written by Reeve's TUI. Edit config.toml instead.\n{text}"),
        )?;
        fs::rename(tmp, path)?;
        Ok(())
    }

    /// Apply these choices on top of `cfg`.
    pub fn apply(&self, cfg: &mut Config) {
        for (name, conn) in &self.connections {
            cfg.connections.entry(name.clone()).or_insert_with(|| {
                let mut c = conn.clone();
                c.api_key = None;
                c
            });
        }
        if let Some(d) = &self.default_connection {
            if cfg.connections.contains_key(d) {
                cfg.default_connection = d.clone();
            }
        }
        for (name, model) in &self.models {
            if let Some(c) = cfg.connections.get_mut(name) {
                c.default_model = Some(model.clone());
            }
        }
        if let Some(d) = &self.drafter {
            cfg.observer.drafter = d.clone();
        }
        // A model picked in the TUI for the active connection beats a
        // global `model =` from config.toml, or the pick would do nothing.
        if self.models.contains_key(&cfg.default_connection) {
            cfg.model = None;
        }
    }
}

/// Connection names become file names under `keys/`.
pub fn valid_connection_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 40
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_in_the_tui_win_over_config() {
        let home = tempfile::tempdir().unwrap();
        let mut s = Settings::default();
        s.connections.insert(
            "groq".into(),
            ConnectionConfig {
                kind: "openai".into(),
                base_url: "https://api.groq.com/openai/v1".into(),
                env_key: None,
                api_key: Some("never-saved".into()),
                default_model: None,
            },
        );
        s.default_connection = Some("groq".into());
        s.models.insert("groq".into(), "llama-4".into());
        s.save(home.path()).unwrap();
        let text = fs::read_to_string(Settings::path(home.path())).unwrap();
        assert!(!text.contains("never-saved"), "{text}");

        let mut cfg = Config {
            model: Some("from-config".into()),
            ..Config::default()
        };
        Settings::load(home.path()).unwrap().apply(&mut cfg);
        let (name, _, model) = cfg.route().unwrap();
        assert_eq!((name.as_str(), model.as_str()), ("groq", "llama-4"));
    }

    #[test]
    fn names_are_file_safe() {
        assert!(valid_connection_name("my-box_2"));
        for bad in ["", "../x", "a b", ".hidden", "a/b"] {
            assert!(!valid_connection_name(bad), "{bad}");
        }
    }
}
