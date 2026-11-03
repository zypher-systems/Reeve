//! Budget totals across a DST change at a month boundary. The checks run
//! in a child copy of this test binary with `TZ` pinned to a DST rule, so
//! chrono's local zone is known without touching this process's environment.

use chrono::{Local, TimeZone, Utc};
use reeve_core::ledger::{SpendRecord, record, role_today, totals};
use reeve_core::spend::Usage;

const INNER: &str = "REEVE_LEDGER_DST_INNER";

#[test]
fn a_record_stays_in_the_day_and_month_it_was_filed_under_across_dst() {
    if std::env::var_os(INNER).is_some() {
        return inner();
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "a_record_stays_in_the_day_and_month_it_was_filed_under_across_dst",
            "--nocapture",
        ])
        .env("TZ", "EST5EDT,M3.2.0/2,M11.1.0/2")
        .env(INNER, "1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn inner() {
    let home = tempfile::tempdir().unwrap();
    // 00:30 EDT on November 1, 2026: filed under 2026-11.jsonl. The clocks
    // go back at 02:00 that morning.
    let rec = SpendRecord {
        ts: Utc.with_ymd_and_hms(2026, 11, 1, 4, 30, 0).unwrap(),
        session: "s".into(),
        connection: "openrouter".into(),
        model: "m".into(),
        usage: Usage::default(),
        usd: Some(12.0),
        priced_by: Some("provider".into()),
        role: Some("drafter".into()),
    };
    record(home.path(), &rec).unwrap();
    let at = |d: u32, h: u32, m: u32| {
        Local
            .with_ymd_and_hms(2026, 11, d, h, m, 0)
            .single()
            .unwrap()
    };
    assert_eq!(
        at(1, 0, 45).offset().local_minus_utc(),
        -4 * 3600,
        "TZ took"
    );
    assert_eq!(at(1, 12, 0).offset().local_minus_utc(), -5 * 3600);
    // Before the change (EDT), after it the same day (EST), and days on.
    for now in [at(1, 0, 45), at(1, 12, 0), at(3, 12, 0)] {
        assert_eq!(
            totals(home.path(), now).month.usd,
            12.0,
            "month total at {now}"
        );
    }
    for now in [at(1, 0, 45), at(1, 12, 0)] {
        assert_eq!(totals(home.path(), now).today.usd, 12.0, "today at {now}");
        assert_eq!(
            role_today(home.path(), "drafter", now).usd,
            12.0,
            "drafter at {now}"
        );
    }
    assert_eq!(totals(home.path(), at(3, 12, 0)).today.usd, 0.0);
    assert_eq!(role_today(home.path(), "drafter", at(3, 12, 0)).usd, 0.0);
}
