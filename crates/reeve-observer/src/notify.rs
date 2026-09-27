//! Desktop notifications through `notify-send` (libnotify), which every
//! Linux desktop ships. Rate-limited by the daemon.

use reeve_core::findings::{Finding, Severity};

/// Tell the desktop about findings. One finding gets its own notification;
/// several are summed up in one.
pub fn send(findings: &[&Finding]) {
    let Some(worst) = findings.iter().map(|f| f.severity).max() else {
        return;
    };
    let (summary, body) = match findings {
        [one] => (
            format!("Reeve: {}", one.title),
            format!(
                "{}\nOpen Reeve to review (/findings).",
                first_line(&one.detail)
            ),
        ),
        many => (
            format!("Reeve: {} things need a look", many.len()),
            many.iter()
                .take(5)
                .map(|f| format!("• {}", f.title))
                .collect::<Vec<_>>()
                .join("\n")
                + "\nOpen Reeve to review (/findings).",
        ),
    };
    let urgency = match worst {
        Severity::Critical => "critical",
        Severity::Warning => "normal",
        Severity::Info => "low",
    };
    let icon = match worst {
        Severity::Critical => "dialog-error",
        Severity::Warning => "dialog-warning",
        Severity::Info => "dialog-information",
    };
    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=Reeve",
            &format!("--urgency={urgency}"),
            &format!("--icon={icon}"),
            &summary,
            &body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or("")
}

/// A plain notification (standing orders reporting back).
pub fn plain(summary: &str, body: &str, urgent: bool) {
    let _ = std::process::Command::new("notify-send")
        .args([
            "--app-name=Reeve",
            if urgent {
                "--urgency=critical"
            } else {
                "--urgency=normal"
            },
            "--icon=dialog-information",
            summary,
            body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}
