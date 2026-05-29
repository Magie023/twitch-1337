//! End-to-end integration tests for the schedule orchestrator.
//!
//! Two tests at different depths:
//!
//! 1. `settings_save_signals_change_notify` (variant c) — proves save→notify
//!    fires and the new typed `Schedule` round-trips through `SettingsStore`
//!    without spawning the full bot.
//!
//! 2. `schedule_fires_after_clock_advance` (variant a) — spawns the full bot
//!    via `TestBotBuilder` with a `FakeClock`, pre-seeds an interval schedule,
//!    advances the fake clock past the first fire point, and asserts the
//!    scheduled message appears on the transport + telemetry is recorded.

mod common;

use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, NaiveTime, TimeZone};
use chrono_tz::Europe::Berlin;
use common::TestBotBuilder;
use twitch_1337_core::schedule::{Schedule, Trigger, WeekdaySet};
use twitch_1337_core::settings::{Actor, MemoryAuditLog, SettingsOverrides, SettingsStore};

// ---------------------------------------------------------------------------
// Test 1: save → change_notify fires within 500ms; schedule round-trips
// ---------------------------------------------------------------------------

/// Prove that `SettingsStore::apply` fires `change_notify` and the new typed
/// schedule is visible in the handle immediately after apply returns.
#[tokio::test]
async fn settings_save_signals_change_notify() {
    let dir = tempfile::tempdir().expect("tmp");
    let audit = Arc::new(MemoryAuditLog::new());
    let (store, settings_handle) =
        SettingsStore::open(dir.path(), audit, "main").expect("open settings");
    let change_notify = store.change_notify();

    // Subscribe BEFORE we apply, to avoid the lost-wakeup race.
    let mut waiter = Box::pin(change_notify.notified());
    waiter.as_mut().enable();

    store
        .apply(
            SettingsOverrides {
                schedules: Some(vec![Schedule {
                    name: "morning".into(),
                    message: "gm everyone!".into(),
                    trigger: Trigger::Calendar {
                        days: WeekdaySet::default(),
                        at: NaiveTime::from_hms_opt(9, 0, 0).unwrap(),
                    },
                    start_date: None,
                    end_date: None,
                    enabled: true,
                }]),
                ..Default::default()
            },
            Actor {
                user_id: "1".into(),
                user_login: "tester".into(),
            },
        )
        .await
        .expect("apply");

    tokio::time::timeout(Duration::from_millis(500), &mut waiter)
        .await
        .expect("change_notify must fire within 500ms of apply()");

    let s = settings_handle.load();
    assert_eq!(s.schedules.len(), 1, "schedule count after apply");
    assert_eq!(s.schedules[0].name, "morning");
    assert_eq!(s.schedules[0].message, "gm everyone!");
    assert!(s.schedules[0].enabled);
}

// ---------------------------------------------------------------------------
// Test 2: full bot — pre-seeded interval schedule fires after clock advance
// ---------------------------------------------------------------------------

/// Spawn a full bot with a 1-minute interval schedule already in settings,
/// advance the fake clock past the first fire point, and assert:
///   - the scheduled message appears on the IRC transport, AND
///   - telemetry records ≥ 1 fire for that schedule (checked in-memory, no
///     file I/O required so the debounced flush window doesn't matter).
#[tokio::test]
async fn schedule_fires_after_clock_advance() {
    // Start at 11:00:00 Berlin so the first interval-aligned fire
    // (anchored to midnight, every 60 s) is ~1 minute away at 11:01:00.
    let start_utc = Berlin
        .with_ymd_and_hms(2026, 4, 18, 11, 0, 0)
        .unwrap()
        .with_timezone(&chrono::Utc);

    let mut bot = TestBotBuilder::new()
        .at(start_utc)
        .with_settings(|o| {
            o.schedules = Some(vec![Schedule {
                name: "hello-world".into(),
                message: "scheduled-hello".into(),
                trigger: Trigger::Interval {
                    every: std::time::Duration::from_secs(60),
                    days: WeekdaySet::default(),
                    active_from: None,
                    active_to: None,
                },
                start_date: None,
                end_date: None,
                enabled: true,
            }]);
        })
        .spawn()
        .await;

    // The orchestrator performs an initial reconcile on startup and spawns the
    // per-schedule task immediately. The task calls clock.sleep_until() for
    // the first interval-aligned fire (11:01 Berlin = +60s from start).
    //
    // Give the spawned task a moment to register its sleep_until waiter.
    tokio::time::sleep(Duration::from_millis(50)).await;

    // Advance 2 minutes: moves clock past 11:01, waking the sleeper.
    bot.clock.advance(ChronoDuration::minutes(2));

    // Give the woken task time to call sender.say() and record telemetry.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let msg = bot.expect_say(Duration::from_secs(2)).await;
    assert!(
        msg.contains("scheduled-hello"),
        "expected 'scheduled-hello' in message, got: {msg:?}"
    );

    // Check telemetry in-memory (avoids depending on the debounced disk flush).
    let snap = bot.telemetry.snapshot().await;
    let runtime = snap
        .get("hello-world")
        .expect("telemetry entry for 'hello-world' must exist");
    assert!(
        runtime.fires_today >= 1,
        "expected at least 1 fire_today, got {}",
        runtime.fires_today
    );

    bot.shutdown().await;
}
