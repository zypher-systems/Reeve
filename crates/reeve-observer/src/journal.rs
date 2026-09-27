//! Following the systemd journal at warning level and above.

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

/// One journal entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// Unit, or the syslog identifier when there's no unit.
    pub unit: String,
    /// 0 (emerg) … 4 (warning).
    pub priority: u8,
    /// The message.
    pub message: String,
    /// When.
    pub ts: DateTime<Utc>,
}

/// Parse one `journalctl -o json` line.
pub fn parse(line: &str) -> Option<Entry> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let unit = s("_SYSTEMD_UNIT")
        .or_else(|| s("_SYSTEMD_USER_UNIT"))
        .or_else(|| s("SYSLOG_IDENTIFIER"))
        .unwrap_or_else(|| "kernel".into());
    let priority = s("PRIORITY").and_then(|p| p.parse().ok()).unwrap_or(6);
    let message = match v.get("MESSAGE") {
        Some(Value::String(m)) => m.clone(),
        // Binary messages come as a byte array.
        Some(Value::Array(bytes)) => String::from_utf8_lossy(
            &bytes
                .iter()
                .filter_map(|b| b.as_u64().map(|b| b as u8))
                .collect::<Vec<_>>(),
        )
        .into_owned(),
        _ => String::new(),
    };
    let ts = s("__REALTIME_TIMESTAMP")
        .and_then(|t| t.parse::<i64>().ok())
        .and_then(|us| Utc.timestamp_micros(us).single())
        .unwrap_or_else(Utc::now);
    Some(Entry {
        unit,
        priority,
        message,
        ts,
    })
}

/// The message with the parts that vary (numbers, hex ids, quoted values)
/// blanked, so repeats of one problem group together.
pub fn template(msg: &str) -> String {
    let mut out = String::new();
    let mut last_hash = false;
    for word in msg.split_whitespace() {
        let digits = word.chars().filter(char::is_ascii_digit).count();
        let hexish = word.len() >= 8
            && word
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == '-' || c == ':');
        let w = if hexish || (digits > 0 && digits * 2 >= word.len()) {
            "#"
        } else {
            word
        };
        if w == "#" && last_hash {
            continue;
        }
        last_hash = w == "#";
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(w);
        if out.len() > 120 {
            break;
        }
    }
    out
}

/// Run `journalctl -f` and send entries until it exits. Returns whether
/// the system journal was readable (the user is in `wheel`, `adm`, or
/// `systemd-journal`).
pub async fn follow(tx: mpsc::Sender<Entry>) -> Result<(), String> {
    let mut child = tokio::process::Command::new("journalctl")
        .args(["-f", "-o", "json", "-p", "warning", "-n", "0", "--no-pager"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("couldn't run journalctl: {e}"))?;
    let out = child.stdout.take().ok_or("no journal output")?;
    let mut lines = BufReader::new(out).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(e) = parse(&line) {
            if tx.send(e).await.is_err() {
                break;
            }
        }
    }
    let _ = child.kill().await;
    Err("journalctl stopped".into())
}

/// Whether the system journal is readable (not just the user's own).
pub async fn system_readable() -> bool {
    tokio::process::Command::new("journalctl")
        .args(["--system", "-n", "1", "-q", "--no-pager"])
        .stdin(std::process::Stdio::null())
        .output()
        .await
        .is_ok_and(|o| {
            o.status.success()
                && !String::from_utf8_lossy(&o.stderr).contains("insufficient permissions")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_units_priorities_and_binary_messages() {
        let e = parse(r#"{"_SYSTEMD_UNIT":"bluetooth.service","PRIORITY":"3","MESSAGE":"Failed to set mode","__REALTIME_TIMESTAMP":"1758950000000000"}"#).unwrap();
        assert_eq!((e.unit.as_str(), e.priority), ("bluetooth.service", 3));
        let b =
            parse(r#"{"SYSLOG_IDENTIFIER":"kernel","PRIORITY":"4","MESSAGE":[104,105]}"#).unwrap();
        assert_eq!((b.unit.as_str(), b.message.as_str()), ("kernel", "hi"));
    }

    #[test]
    fn templates_group_repeats() {
        assert_eq!(
            template("usb 3-2: device descriptor read/64, error -71"),
            template("usb 1-4: device descriptor read/64, error -110")
        );
        assert_ne!(template("disk full"), template("disk ok"));
    }
}
