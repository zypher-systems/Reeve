//! Configuration and secrets.
//!
//! `~/.reeve/config.toml` is yours; Reeve never rewrites it. Keys resolve in
//! Ryter's order: inline `api_key` (file must be 0600) → `env_key` →
//! `~/.reeve/keys/<connection>` → the kind's default variable.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Everything Reeve reads from `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Connection used when none is named.
    pub default_connection: String,
    /// Model used when none is named. Falls back to the connection's default.
    pub model: Option<String>,
    /// Reasoning effort sent to OpenRouter: `low`, `medium`, `high`, or `default` (send nothing).
    pub reasoning: String,
    /// Named connections.
    pub connections: BTreeMap<String, ConnectionConfig>,
    /// `[pricing."<model>"]` overrides, USD per million tokens.
    pub pricing: BTreeMap<String, PriceOverride>,
    /// Spending caps.
    pub spend: SpendConfig,
    /// Approval behavior.
    pub approvals: ApprovalConfig,
    /// Filesystem snapshots around root changes.
    pub snapshots: SnapshotConfig,
    /// Memory: survey and reflection.
    pub memory: MemoryConfig,
    /// The background observer (`reeved`).
    pub observer: ObserverConfig,
    /// TUI presentation.
    pub ui: UiConfig,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            default_connection: "openrouter".into(),
            model: None,
            reasoning: "medium".into(),
            connections: builtin_connections(),
            pricing: BTreeMap::new(),
            spend: SpendConfig::default(),
            approvals: ApprovalConfig::default(),
            snapshots: SnapshotConfig::default(),
            memory: MemoryConfig::default(),
            observer: ObserverConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

/// One model endpoint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionConfig {
    /// `openrouter`, `openai` (any OpenAI-compatible endpoint), or `local`
    /// (a model server on this machine: no key, no API cost).
    pub kind: String,
    /// API base URL, up to and including `/v1`.
    pub base_url: String,
    /// Environment variable holding the key.
    #[serde(default)]
    pub env_key: Option<String>,
    /// Raw key. Never logged. The file containing it must be mode 0600.
    #[serde(default)]
    pub api_key: Option<String>,
    /// Default model id on this connection.
    #[serde(default)]
    pub default_model: Option<String>,
}

impl ConnectionConfig {
    /// A model server on this machine: no key required, no API cost.
    pub fn is_local(&self) -> bool {
        self.kind == "local"
    }
}

/// `[pricing."<model>"]`. Values are USD per million tokens.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PriceOverride {
    /// Fresh input.
    #[serde(default)]
    pub input_per_million: Option<f64>,
    /// Output.
    #[serde(default)]
    pub output_per_million: Option<f64>,
    /// Cache reads.
    #[serde(default)]
    pub cache_read_per_million: Option<f64>,
    /// Cache writes.
    #[serde(default)]
    pub cache_write_per_million: Option<f64>,
}

/// `[spend]`. A cap of 0 is off.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpendConfig {
    /// Stop a session once it has spent this much.
    pub session_usd: f64,
    /// Stop all Reeve work (TUI and daemon) for the rest of the day.
    pub daily_usd: f64,
    /// Stop all Reeve work for the rest of the month.
    pub monthly_usd: f64,
    /// Warn in the Spend panel once a session passes this.
    pub warn_usd: f64,
}

impl Default for SpendConfig {
    fn default() -> Self {
        Self {
            session_usd: 0.0,
            daily_usd: 0.0,
            monthly_usd: 0.0,
            warn_usd: 1.0,
        }
    }
}

/// `[approvals]`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ApprovalConfig {
    /// Start every session in YOLO mode (auto-approve T0–T2). The safeguard
    /// floor still asks.
    pub yolo: bool,
}

/// `[snapshots]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SnapshotConfig {
    /// Take a snapper pre/post pair around each root action, when snapper
    /// has a config for `/`.
    pub enabled: bool,
}

impl Default for SnapshotConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// `[memory]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Survey the machine on first run and weekly after (read-only commands).
    pub survey: bool,
    /// Reflect on a session when it ends (`/new`) or, after a restart, on
    /// recent sessions that weren't. One model call each.
    pub auto_reflect: bool,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            survey: true,
            auto_reflect: true,
        }
    }
}

/// `[observer]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ObserverConfig {
    /// Desktop notifications for findings.
    pub notify: bool,
    /// Disk use (0–1) that raises a warning; +0.07 is critical.
    pub disk_warn: f64,
    /// CPU temperature (°C) that raises a warning when sustained.
    pub temp_warn: f32,
    /// Pre-drafts proposals for findings (off by default).
    pub drafter: DrafterConfig,
}

impl Default for ObserverConfig {
    fn default() -> Self {
        Self {
            notify: true,
            disk_warn: 0.90,
            temp_warn: 90.0,
            drafter: DrafterConfig::default(),
        }
    }
}

/// `[observer.drafter]`: a separate role with its own model and budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DrafterConfig {
    /// Draft proposals in the background.
    pub enabled: bool,
    /// Connection (default: the main one).
    pub connection: Option<String>,
    /// Model (default: the main one).
    pub model: Option<String>,
    /// Its own cap, USD per day.
    pub daily_usd: f64,
    /// Stop one draft past this, USD.
    pub per_draft_usd: f64,
    /// Most drafts per day.
    pub max_drafts_per_day: u32,
    /// `info`, `warning`, or `critical`: the least severe finding worth a draft.
    pub min_severity: String,
}

impl Default for DrafterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            connection: None,
            model: None,
            daily_usd: 0.25,
            per_draft_usd: 0.05,
            max_drafts_per_day: 10,
            min_severity: "warning".into(),
        }
    }
}

/// `[ui]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// `brass`, or a name from `~/.reeve/themes/<name>.toml`.
    pub theme: String,
    /// `auto`, `truecolor`, `256`, or `16`. `NO_COLOR` always wins.
    pub colors: String,
    /// Mouse capture.
    pub mouse: bool,
    /// Animations (pulses, gradient shimmer). Off for slow terminals.
    pub animate: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            theme: "brass".into(),
            colors: "auto".into(),
            mouse: true,
            animate: true,
        }
    }
}

impl Config {
    /// The connection and model to use when nothing more specific is chosen.
    pub fn route(&self) -> Result<(String, &ConnectionConfig, String)> {
        let name = self.default_connection.clone();
        let conn = self
            .connections
            .get(&name)
            .ok_or_else(|| Error::Config(format!("default_connection {name:?} is not defined")))?;
        let model = self
            .model
            .clone()
            .or_else(|| conn.default_model.clone())
            .ok_or_else(|| {
                Error::Config(format!(
                    "no model chosen: set `model = \"…\"` or connections.{name}.default_model"
                ))
            })?;
        Ok((name, conn, model))
    }

    /// Reasoning effort to send, or `None` for "let the provider decide".
    pub fn reasoning_effort(&self) -> Option<String> {
        match self.reasoning.trim().to_ascii_lowercase().as_str() {
            e @ ("low" | "medium" | "high") => Some(e.to_string()),
            _ => None,
        }
    }
}

fn builtin_connections() -> BTreeMap<String, ConnectionConfig> {
    let mut m = BTreeMap::new();
    for kind in ["openrouter", "openai"] {
        if let Ok(c) = connection_template(kind) {
            m.insert(kind.to_string(), c);
        }
    }
    m
}

/// Starter row for a connection kind.
pub fn connection_template(kind: &str) -> Result<ConnectionConfig> {
    let c = |kind: &str, url: &str, env: Option<&str>, model: Option<&str>| ConnectionConfig {
        kind: kind.into(),
        base_url: url.into(),
        env_key: env.map(Into::into),
        api_key: None,
        default_model: model.map(Into::into),
    };
    Ok(match kind {
        "openrouter" => c(
            "openrouter",
            "https://openrouter.ai/api/v1",
            Some("OPENROUTER_API_KEY"),
            Some("anthropic/claude-sonnet-5"),
        ),
        "openai" | "openai_compat" => c(
            "openai",
            "https://api.openai.com/v1",
            Some("OPENAI_API_KEY"),
            Some("gpt-5-mini"),
        ),
        "local" | "ollama" => c("local", "http://localhost:11434/v1", None, None),
        "lmstudio" => c("local", "http://localhost:1234/v1", None, None),
        "llamacpp" => c("local", "http://localhost:8080/v1", None, None),
        other => {
            return Err(Error::Config(format!(
                "unknown kind {other:?} (openrouter, openai, local, ollama, lmstudio, llamacpp)"
            )));
        }
    })
}

/// Where Reeve keeps its state (`REEVE_HOME` or `~/.reeve`).
pub fn home_dir() -> PathBuf {
    if let Ok(p) = std::env::var("REEVE_HOME") {
        return PathBuf::from(p);
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".reeve")
}

/// Defaults, then `home/config.toml`, then the TUI's `settings.toml`.
/// User connections merge over the built-in ones by name.
pub fn load_at(home: &Path) -> Result<Config> {
    let mut cfg = load_config_file(home)?;
    crate::settings::Settings::load(home)?.apply(&mut cfg);
    Ok(cfg)
}

fn load_config_file(home: &Path) -> Result<Config> {
    let path = home.join("config.toml");
    let mut cfg = Config::default();
    if !path.exists() {
        return Ok(cfg);
    }
    let text = fs::read_to_string(&path).map_err(|e| Error::Config(format!("{path:?}: {e}")))?;
    let user: Config =
        toml::from_str(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
    let builtin = std::mem::take(&mut cfg.connections);
    cfg = user;
    for (name, conn) in builtin {
        cfg.connections.entry(name).or_insert(conn);
    }
    if cfg.connections.values().any(|c| c.api_key.is_some()) {
        check_mode_0600(&path)?;
    }
    Ok(cfg)
}

/// [`load_at`] on [`home_dir`].
pub fn load() -> Result<Config> {
    load_at(&home_dir())
}

#[cfg(unix)]
fn check_mode_0600(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(path)?.permissions().mode() & 0o777;
    if mode & 0o077 != 0 {
        return Err(Error::Config(format!(
            "{} contains api_key and is mode {mode:o}; chmod 600 it or use `reeve key set`",
            path.display()
        )));
    }
    Ok(())
}

#[cfg(not(unix))]
fn check_mode_0600(_path: &Path) -> Result<()> {
    Ok(())
}

/// Resolve the key for `connection` without logging it.
pub fn resolve_secret(cfg: &Config, home: &Path, connection: &str) -> Result<String> {
    resolve_secret_with(cfg, home, connection, |k| std::env::var(k).ok())
}

/// [`resolve_secret`] with `env` in place of the process environment (tests).
pub fn resolve_secret_with(
    cfg: &Config,
    home: &Path,
    connection: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Result<String> {
    let conn = cfg
        .connections
        .get(connection)
        .ok_or_else(|| Error::Config(format!("unknown connection {connection:?}")))?;
    let clean = |s: String| {
        let t = s.trim().to_string();
        (!t.is_empty()).then_some(t)
    };
    if let Some(k) = conn.api_key.clone().and_then(clean) {
        return Ok(k);
    }
    if let Some(k) = conn.env_key.as_deref().and_then(&env).and_then(clean) {
        return Ok(k);
    }
    if let Some(k) = fs::read_to_string(key_path(home, connection))
        .ok()
        .and_then(clean)
    {
        return Ok(k);
    }
    if conn.is_local() {
        return Ok(String::new());
    }
    let fallback = match conn.kind.as_str() {
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    };
    if let Some(k) = fallback.and_then(&env).and_then(clean) {
        return Ok(k);
    }
    Err(Error::Config(format!(
        "no key for connection {connection:?}; run `reeve key set {connection}`{}",
        fallback
            .map(|v| format!(" or export {v}"))
            .unwrap_or_default()
    )))
}

/// Where a connection's key would come from, without reading it out:
/// `config.toml`, `$VAR`, or `keys/<name>`. `None` when there is none.
pub fn secret_source(cfg: &Config, home: &Path, connection: &str) -> Option<String> {
    secret_source_with(cfg, home, connection, |k| std::env::var(k).ok())
}

fn secret_source_with(
    cfg: &Config,
    home: &Path,
    connection: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let conn = cfg.connections.get(connection)?;
    let set = |v: Option<String>| v.is_some_and(|s| !s.trim().is_empty());
    if set(conn.api_key.clone()) {
        return Some("config.toml".into());
    }
    if let Some(var) = conn.env_key.as_deref() {
        if set(env(var)) {
            return Some(format!("${var}"));
        }
    }
    if set(fs::read_to_string(key_path(home, connection)).ok()) {
        return Some(format!("keys/{connection}"));
    }
    if conn.is_local() {
        return Some("none needed".into());
    }
    let fallback = match conn.kind.as_str() {
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "openai" => Some("OPENAI_API_KEY"),
        _ => None,
    }?;
    set(env(fallback)).then(|| format!("${fallback}"))
}

/// Forget a stored key. Keys from the environment or `config.toml` are
/// not Reeve's to remove.
pub fn remove_secret_at(home: &Path, connection: &str) -> Result<bool> {
    if !crate::settings::valid_connection_name(connection) {
        return Err(Error::Config(format!("bad connection name {connection:?}")));
    }
    match fs::remove_file(key_path(home, connection)) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e.into()),
    }
}

/// Whether a connection has a key, without returning it.
pub fn has_secret(cfg: &Config, home: &Path, connection: &str) -> bool {
    resolve_secret(cfg, home, connection).is_ok()
}

fn key_path(home: &Path, connection: &str) -> PathBuf {
    home.join("keys").join(connection)
}

/// Store a key at `home/keys/<connection>`, created at 0600 (never widened
/// then narrowed: a write followed by chmod leaves it readable meanwhile).
pub fn store_secret_at(home: &Path, connection: &str, secret: &str) -> Result<PathBuf> {
    let secret = secret.trim();
    if secret.is_empty() {
        return Err(Error::Config("empty API key".into()));
    }
    if !crate::settings::valid_connection_name(connection) {
        return Err(Error::Config(format!("bad connection name {connection:?}")));
    }
    let dir = home.join("keys");
    fs::create_dir_all(&dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    }
    let path = key_path(home, connection);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)?;
        f.write_all(secret.as_bytes())?;
    }
    #[cfg(not(unix))]
    fs::write(&path, secret)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_connections_merge_over_builtins() {
        let home = tempfile::tempdir().unwrap();
        fs::write(
            home.path().join("config.toml"),
            r#"
default_connection = "box"
model = "qwen3"
[connections.box]
kind = "local"
base_url = "http://10.0.0.5:8080/v1"
[spend]
daily_usd = 2.0
"#,
        )
        .unwrap();
        let cfg = load_at(home.path()).unwrap();
        assert!(cfg.connections.contains_key("openrouter"));
        let (name, conn, model) = cfg.route().unwrap();
        assert_eq!((name.as_str(), model.as_str()), ("box", "qwen3"));
        assert!(conn.is_local());
        assert!((cfg.spend.daily_usd - 2.0).abs() < 1e-9);
        assert_eq!(cfg.ui.theme, "brass");
    }

    #[test]
    fn keys_resolve_in_order_and_never_empty() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::default();
        let no_env = |_: &str| None;
        assert!(resolve_secret_with(&cfg, home.path(), "openrouter", no_env).is_err());
        let fallback = |k: &str| (k == "OPENROUTER_API_KEY").then(|| "env-key".to_string());
        assert_eq!(
            resolve_secret_with(&cfg, home.path(), "openrouter", fallback).unwrap(),
            "env-key"
        );
        store_secret_at(home.path(), "openrouter", "  file-key\n").unwrap();
        assert_eq!(
            resolve_secret_with(&cfg, home.path(), "openrouter", no_env).unwrap(),
            "file-key"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(home.path().join("keys/openrouter"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(store_secret_at(home.path(), "../x", "k").is_err());
        assert_eq!(
            secret_source_with(&cfg, home.path(), "openrouter", no_env).as_deref(),
            Some("keys/openrouter")
        );
        assert!(remove_secret_at(home.path(), "openrouter").unwrap());
        assert_eq!(
            secret_source_with(&cfg, home.path(), "openrouter", no_env),
            None
        );
        assert_eq!(
            secret_source_with(&cfg, home.path(), "openrouter", fallback).as_deref(),
            Some("$OPENROUTER_API_KEY")
        );
    }

    #[cfg(unix)]
    #[test]
    fn inline_key_in_a_readable_file_is_refused() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("config.toml");
        fs::write(
            &path,
            "[connections.openrouter]\nkind=\"openrouter\"\nbase_url=\"x\"\napi_key=\"sk\"\n",
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(load_at(home.path()).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(load_at(home.path()).is_ok());
    }
}
