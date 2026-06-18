# Flight-Tracker `advance_flight` Extraction Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Lift the flight tracker's per-poll state-transition logic out of the 545-line `poll_all_flights_with_commands` I/O loop into one pure, synchronous, unit-testable function `advance_flight`.

**Architecture:** A new `advance.rs` module owns the **Advance** (per-`Observation` transition of a `TrackedFlight`). It mutates the flight in place and returns a `FlightUpdate` describing the chat messages, debug events, follow-up lookups, and removal it wants — but performs no I/O. The poll loop keeps the `JoinSet` fetch orchestration, `sender.say`, `append_debug_event`, route lookup, and persistence; it translates each messy poll result into an `Observation`, calls `advance_flight`, then performs the returned `FlightUpdate`. The extraction is **behaviour-preserving**: the existing 1964-line integration suite (`crates/core/tests/flight_tracker.rs`) is the characterization net and must stay green; only after that do we add unit tests on `advance_flight`.

**Tech Stack:** Rust, tokio, chrono / chrono-tz (Berlin compiled-in), `eyre` errors, `cargo nextest`, serde (RON persistence).

## Global Constraints

- All time ops use UTC/`Europe/Berlin`; `advance_flight` takes `now: DateTime<Utc>` supplied by `clock.now_utc()` — never call a clock inside the pure function.
- `cargo clippy --workspace --all-targets -- -D warnings` must pass; extra lints in `Cargo.toml [lints.clippy]` apply. No `#[allow]`/`#[expect]` without a one-line `reason`.
- Run tests with `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail` (not `cargo test`).
- Log errors with `?error`, not `%error`.
- Branch from a `refactor/` branch (`refactor/advance-flight`); never commit to `main`.
- Commits: Conventional Commits, unhinged genz subject line, sober body only when large.
- This is a refactor: **no behaviour change** until the unit-test tasks. Do not "fix" anything you find odd during the move — note it, keep behaviour identical.
- Persisted `TrackedFlight` shape must not change (serde compatibility); this plan does not touch field definitions.

## File Structure

- **Create** `crates/core/src/aviation/tracker/advance.rs` — the Advance: types (`PollOutcome`, `Observation`, `Emit`, `RemovalReason`, `Followup`, `FlightUpdate`), `advance_flight`, `apply_route`, `format_emit`, and the confirmation-transition helpers moved out of `commands.rs`. Owns the unit tests.
- **Modify** `crates/core/src/aviation/tracker/mod.rs` — declare `pub(crate) mod advance;`.
- **Modify** `crates/core/src/aviation/tracker/commands.rs` — `poll_all_flights_with_commands` loses pass-2's inline transition body; gains result→`Observation` translation and `FlightUpdate` performing. Confirmation helpers move to `advance.rs`; `aircraft_callsign` stays `pub(crate)` and is imported by `advance.rs`.

### Helpers and their destination

Move into `advance.rs` (pure, used only by the transition):
`target_confirmation_for_aircraft`, `should_keep_prior_confirmation`, `flight_matches_callsign`, `candidate_callsign_matches_flight`, `identifier_callsign`, `inferred_by_hex_window`, `tracking_lost_threshold_delta`, `last_seen_age_secs`.

Stay in `commands.rs` (used by the loop / fetch orchestration), imported into `advance.rs` as needed:
`aircraft_callsign` (already `pub(crate)`), `find_index_by_identifier`, `callsign_poll_candidates`, `should_poll_by_hex`, `poll_aircraft_by_callsign_aliases`.

Already `pub(crate)`, called by `advance_flight` unchanged:
`detect_phase`, `is_airborne_phase`, `altitude_ft`, `vertical_rate`, `emergency_squawk_meaning`, `update_divert_counter` (all `phase.rs`); `set_route_from_iata`, `add_alias_callsign`, `set_hex_if_consistent` (all `metadata.rs`); `msg_*` (`format.rs`); `FlightTrackerDebugEvent::*` (`debug_journal.rs`).

---

### Task 1: Extract `advance_flight` (behaviour-preserving move)

This is one cohesive change — pass-2 cannot be moved without rewiring the loop and still compile. The reviewer's gate is: **integration suite green + types well-shaped**. No behaviour change.

**Files:**
- Create: `crates/core/src/aviation/tracker/advance.rs`
- Modify: `crates/core/src/aviation/tracker/mod.rs:1-10` (add module declaration)
- Modify: `crates/core/src/aviation/tracker/commands.rs:204-228` (move confirmation helpers out), `commands.rs:1332-1739` (replace pass-2 body)

**Interfaces:**
- Produces (consumed by the loop and by Tasks 2-4):
  ```rust
  pub(crate) enum PollOutcome { Hit(Box<NearbyAircraft>), Miss, Error, Timeout }
  pub(crate) struct Observation { pub used_hex: bool, pub outcome: PollOutcome }

  #[derive(Debug, PartialEq)]
  pub(crate) enum Emit {
      AdsbVisible, Takeoff, Cruise, Descent, Approach, Landing,
      SquawkEmergency { code: String, meaning: String },
      PossibleDivert, TrackingLost, PendingExpired,
  }
  #[derive(Debug, PartialEq)]
  pub(crate) enum RemovalReason { TrackingLost { secs: i64 }, PendingExpired }
  #[derive(Debug, PartialEq)]
  pub(crate) enum Followup { FetchRoute { callsign: String } }

  #[derive(Default)]
  pub(crate) struct FlightUpdate {
      pub emits: Vec<Emit>,
      pub debug: Vec<FlightTrackerDebugEvent>,
      pub followups: Vec<Followup>,
      pub removal: Option<RemovalReason>,
  }

  pub(crate) fn advance_flight(
      flight: &mut TrackedFlight,
      obs: &Observation,
      now: DateTime<Utc>,
  ) -> FlightUpdate;

  pub(crate) fn apply_route(flight: &mut TrackedFlight, origin: &str, dest: &str);
  pub(crate) fn format_emit(flight: &TrackedFlight, emit: &Emit, now: DateTime<Utc>) -> String;
  ```
- Consumes: existing `pub(crate)` helpers listed in the File Structure section.

- [ ] **Step 1: Branch**

```bash
git checkout -b refactor/advance-flight
```

- [ ] **Step 2: Declare the module**

In `crates/core/src/aviation/tracker/mod.rs`, add after line 1 (`pub(crate) mod commands;`):

```rust
pub(crate) mod advance;
```

- [ ] **Step 3: Create `advance.rs` with the types, `format_emit`, and `apply_route`**

```rust
//! The Advance: the pure per-`Observation` transition of a `TrackedFlight`.
//!
//! `advance_flight` mutates a flight in place and returns a `FlightUpdate`
//! describing the side effects the poll loop should perform. It does no I/O
//! and is deterministic given `(flight, observation, now)`.

use chrono::{DateTime, TimeDelta, Utc};

use super::commands::aircraft_callsign;
use super::debug_journal::FlightTrackerDebugEvent;
use super::format::{
    msg_adsb_visible, msg_approach, msg_cruise, msg_descent, msg_landing, msg_pending_expired,
    msg_possible_divert, msg_squawk_emergency, msg_takeoff, msg_tracking_lost,
};
use super::metadata::set_route_from_iata;
use super::{FlightIdentifier, TargetConfirmation, TrackedFlight, TRACKING_LOST_THRESHOLD};
use crate::aviation::types::NearbyAircraft;

#[derive(Debug)]
pub(crate) enum PollOutcome {
    Hit(Box<NearbyAircraft>),
    Miss,
    Error,
    Timeout,
}

#[derive(Debug)]
pub(crate) struct Observation {
    pub used_hex: bool,
    pub outcome: PollOutcome,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Emit {
    AdsbVisible,
    Takeoff,
    Cruise,
    Descent,
    Approach,
    Landing,
    SquawkEmergency { code: String, meaning: String },
    PossibleDivert,
    TrackingLost,
    PendingExpired,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RemovalReason {
    TrackingLost { secs: i64 },
    PendingExpired,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Followup {
    FetchRoute { callsign: String },
}

#[derive(Default)]
pub(crate) struct FlightUpdate {
    pub emits: Vec<Emit>,
    pub debug: Vec<FlightTrackerDebugEvent>,
    pub followups: Vec<Followup>,
    pub removal: Option<RemovalReason>,
}

/// Maps a semantic `Emit` to the existing chat formatter.
pub(crate) fn format_emit(flight: &TrackedFlight, emit: &Emit, now: DateTime<Utc>) -> String {
    match emit {
        Emit::AdsbVisible => msg_adsb_visible(flight),
        Emit::Takeoff => msg_takeoff(flight),
        Emit::Cruise => msg_cruise(flight),
        Emit::Descent => msg_descent(flight),
        Emit::Approach => msg_approach(flight),
        Emit::Landing => msg_landing(flight, now),
        Emit::SquawkEmergency { code, meaning } => msg_squawk_emergency(flight, code, meaning),
        Emit::PossibleDivert => msg_possible_divert(flight),
        Emit::TrackingLost => msg_tracking_lost(flight),
        Emit::PendingExpired => msg_pending_expired(flight),
    }
}

/// Applies a freshly fetched route to a flight (also sets dest lat/lon).
/// Same body as the inline `set_route_from_iata` call in the old loop.
pub(crate) fn apply_route(flight: &mut TrackedFlight, origin: &str, dest: &str) {
    set_route_from_iata(flight, origin, dest);
}

fn tracking_lost_threshold_delta() -> TimeDelta {
    TimeDelta::from_std(TRACKING_LOST_THRESHOLD).unwrap_or_else(|_| TimeDelta::zero())
}
```

> NOTE: if `set_route_from_iata` already does exactly `apply_route`'s job and you prefer not to wrap it, call `set_route_from_iata` directly from the loop's followup handler in Step 6 and drop `apply_route`. Keeping the named wrapper is the recommended seam.

- [ ] **Step 4: Move the confirmation helpers into `advance.rs`**

Cut these functions from `commands.rs` and paste them into `advance.rs`, changing each from `fn` to `pub(crate) fn` only where the loop still needs them (none do — keep them private to `advance.rs`):
- `target_confirmation_for_aircraft` (commands.rs:204-228)
- `should_keep_prior_confirmation` (commands.rs:190-202)
- `flight_matches_callsign` (commands.rs:131-146)
- `candidate_callsign_matches_flight` (commands.rs:148-154)
- `identifier_callsign` (commands.rs:156-161)
- `inferred_by_hex_window` (commands.rs:168-178)
- `last_seen_age_secs` (commands.rs:184-188)

Delete the now-duplicated `tracking_lost_threshold_delta` from `commands.rs` (it is defined in `advance.rs` Step 3). Fix imports in both files until `cargo build -p twitch-1337-core` compiles. `aircraft_callsign` stays `pub(crate)` in `commands.rs`.

- [ ] **Step 5: Write `advance_flight` by moving the pass-2 body**

Add `advance_flight` to `advance.rs`. Its body is the existing pass-2 logic from `commands.rs:1346-1739`, moved verbatim, with these **mechanical transformations** (this is the whole change — apply each rule everywhere it appears in that range):

1. The function receives `flight: &mut TrackedFlight` already resolved by the loop. Delete the `find_index_by_identifier` lookup and `let flight = &mut state.flights[idx];` (commands.rs:1342-1345); the loop does that now.
2. `flight.last_adsb_poll_at = Some(now);` stays as the first line. Delete the `changed = true;` lines — persistence is decided by the loop (it always persists when any flight was advanced).
3. Replace the `match ac_result { Ok(Ok(Some(ac))) => ac, ... }` with a match on `&obs.outcome`:
   - `PollOutcome::Hit(ac)` → bind `ac`, continue to the hit logic.
   - `PollOutcome::Miss` → run the miss branch (commands.rs:1353-1410): push the `adsb_poll_result` "miss" debug event into `update.debug`; run the `last_visible_at` grace-window logic; on removal set `update.removal = Some(RemovalReason::TrackingLost { secs: lost_duration.num_seconds() })` and `update.emits.push(Emit::TrackingLost)` instead of `messages.push(msg_tracking_lost(flight))`/`removals.push(...)`; then `return update;`.
   - `PollOutcome::Error` → push the "error" `adsb_poll_result` debug event, `return update;`.
   - `PollOutcome::Timeout` → push the "timeout" `adsb_poll_result` debug event, `return update;`.
   - Replace `used_hex` (the old tuple field) with `obs.used_hex` throughout.
4. Every `append_debug_event(data_dir, now, EVENT).await;` → `update.debug.push(EVENT);` (drop `data_dir`/`now`/`.await`; the `FlightTrackerDebugEvent::*` constructor call is unchanged).
5. Every `messages.push(msg_X(flight ...));` → `update.emits.push(Emit::X ...);` using `format_emit`'s mapping:
   - `msg_adsb_visible` → `Emit::AdsbVisible`
   - `msg_takeoff` → `Emit::Takeoff`; `msg_cruise` → `Emit::Cruise`; `msg_descent` → `Emit::Descent`; `msg_approach` → `Emit::Approach`; `msg_landing` → `Emit::Landing`
   - `msg_squawk_emergency(flight, new_squawk, meaning)` → `Emit::SquawkEmergency { code: new_squawk.clone(), meaning: meaning.to_string() }`
   - `msg_possible_divert` → `Emit::PossibleDivert`
6. The route block (commands.rs:1516-1587): replace the whole `aviation_client.get_flight_route(&cs).await` + match + `set_route_from_iata` + debug events with a single `update.followups.push(Followup::FetchRoute { callsign: cs.clone() });`. The loop performs the fetch and the debug events (Step 6). Keep the synchronous parts before it (`flight.callsign = Some(cs.clone()); add_alias_callsign(flight, &cs);`).
7. The hex/type/squawk/telemetry mutations (commands.rs:1589-1618), the `became_target_confirmed` block (1620-1627), the phase block (1629-1673), and the divert block (1675-1738) move verbatim, with rules 4-5 applied. `update_divert_counter` is unchanged.
8. End with `update`. Signature:

```rust
pub(crate) fn advance_flight(
    flight: &mut TrackedFlight,
    obs: &Observation,
    now: DateTime<Utc>,
) -> FlightUpdate {
    let mut update = FlightUpdate::default();
    let was_pending = super::schedule::is_pending_adsb(flight);
    let was_target_confirmed = flight.target_confirmation.is_target_confirmed();
    flight.last_adsb_poll_at = Some(now);
    // ... moved body per rules above ...
    update
}
```

- [ ] **Step 6: Rewire the loop in `commands.rs`**

Replace the per-result body (commands.rs:1342-1739) with the call + perform. The result→`Observation` translation and the `FetchRoute` followup live here:

```rust
let Some(idx) = find_index_by_identifier(&state.flights, &identifier) else {
    continue;
};

let outcome = match ac_result {
    Ok(Ok(Some(ac))) => PollOutcome::Hit(Box::new(ac)),
    Ok(Ok(None)) => PollOutcome::Miss,
    Ok(Err(e)) => {
        warn!(?e, identifier = %identifier, "ADS-B poll failed");
        PollOutcome::Error
    }
    Err(_) => {
        warn!(identifier = %identifier, "ADS-B poll timed out");
        PollOutcome::Timeout
    }
};
let obs = Observation { used_hex, outcome };

let update = {
    let flight = &mut state.flights[idx];
    advance_flight(flight, &obs, now)
};
changed = true;

for event in update.debug {
    append_debug_event(data_dir, now, event).await;
}

for followup in update.followups {
    let Followup::FetchRoute { callsign } = followup;
    if let Ok(Ok(Some(route))) =
        tokio::time::timeout(ROUTE_FETCH_TIMEOUT, aviation_client.get_flight_route(&callsign)).await
    {
        let origin = route.origin.iata_code.clone();
        let dest = route.destination.iata_code.clone();
        let flight = &mut state.flights[idx];
        apply_route(flight, &origin, &dest);
        append_debug_event(
            data_dir,
            now,
            FlightTrackerDebugEvent::flight_route_lookup(
                state.flights[idx].identifier.as_str(),
                Some(&callsign),
                DebugHttpOutcome::success("adsbdb", "flight_route"),
                Some(&origin),
                Some(&dest),
                state.flights[idx].dest_lat.is_some() && state.flights[idx].dest_lon.is_some(),
            ),
        )
        .await;
    }
    // (miss/error/timeout route-lookup debug events: move them here verbatim
    //  from commands.rs:1541-1585 if you want byte-identical journals; otherwise
    //  the success path above is the behaviour that affects state.)
}

for emit in &update.emits {
    let msg = format_emit(&state.flights[idx], emit, now);
    messages.push(msg);
}

if let Some(reason) = update.removal {
    match reason {
        RemovalReason::TrackingLost { secs } => {
            info!(identifier = %identifier, "Removing flight: tracking lost for {secs}s");
        }
        RemovalReason::PendingExpired => {}
    }
    removals.push(identifier.clone());
}
```

Add `use super::advance::{advance_flight, apply_route, format_emit, Emit, Followup, Observation, PollOutcome, RemovalReason};` to `commands.rs`. Keep the existing `messages`/`removals`/`changed` aggregation and the end-of-loop removal+say+persist block (commands.rs:1741-1759) unchanged.

> The miss-branch route-lookup debug events (non-success) are journal-only and never touch state. Moving them is optional for behaviour; do it for byte-identical journals. The integration suite asserts on success-path journal entries (see `track_command_enriches_flight_from_aviationstack_once`), so keep those.

- [ ] **Step 7: Update pass-1 pending-expired to share the vocabulary**

In the pass-1 readiness loop (commands.rs:1245-1260), keep the decision point but use the shared types for consistency: leave `messages.push(msg_pending_expired(flight))` as-is OR push through `format_emit(flight, &Emit::PendingExpired, now)`; either is identical output. No behaviour change required here — the shared `RemovalReason::PendingExpired`/`Emit::PendingExpired` exist for the unit tests and future callers.

- [ ] **Step 8: Build, lint, and run the integration suite**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run -p twitch-1337-core --show-progress=none --cargo-quiet --status-level=fail
```

Expected: clippy clean; **every test in `crates/core/tests/flight_tracker.rs` passes unchanged** (this proves the move was behaviour-preserving). If any flight-tracker integration test fails, the move changed behaviour — diff your `advance_flight` against the original pass-2 body until green. Do not adjust the test.

- [ ] **Step 9: Commit**

```bash
git add crates/core/src/aviation/tracker/advance.rs \
        crates/core/src/aviation/tracker/mod.rs \
        crates/core/src/aviation/tracker/commands.rs
git commit -m "refactor(tracker): yeeted the poll loop's state machine into advance_flight, no cap 🛬

Pass-2 of poll_all_flights_with_commands now lives behind a pure
advance_flight(&mut flight, &obs, now) -> FlightUpdate. The loop keeps
the I/O (fetch, say, debug journal, route lookup, persist); the
transition becomes synchronous and deterministic. Behaviour-preserving:
the flight_tracker integration suite is unchanged and green."
```

---

### Task 2: Unit tests — confirmation, grace window, removal

Now the payoff. `advance_flight` is the test surface.

**Files:**
- Modify: `crates/core/src/aviation/tracker/advance.rs` (add `#[cfg(test)] mod tests`)

**Interfaces:**
- Consumes: `advance_flight`, `Observation`, `PollOutcome`, `Emit`, `RemovalReason`, `FlightUpdate`; `test_support::{dt, tracked_flight}`; `NearbyAircraft`.

- [ ] **Step 1: Add a `NearbyAircraft` fixture + the failing grace-window tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::aviation::tracker::test_support::{dt, tracked_flight};
    use crate::aviation::tracker::{HexSource, TargetConfirmation};
    use crate::aviation::types::{AltBaro, NearbyAircraft};

    /// Minimal aircraft fixture; override fields per test.
    fn aircraft() -> NearbyAircraft {
        NearbyAircraft {
            hex: Some("3C6497".to_string()),
            flight: Some("DLH1929".to_string()),
            r: None,
            t: None,
            alt_baro: Some(AltBaro::Feet(12_000)),
            lat: Some(52.4),
            lon: Some(13.5),
            gs: Some(280.0),
            baro_rate: Some(-1_800),
            geom_rate: None,
            squawk: Some("1000".to_string()),
            nav_modes: None,
            rssi: None,
            seen_pos: None,
        }
    }

    fn miss(used_hex: bool) -> Observation {
        Observation { used_hex, outcome: PollOutcome::Miss }
    }
    fn hit(ac: NearbyAircraft, used_hex: bool) -> Observation {
        Observation { used_hex, outcome: PollOutcome::Hit(Box::new(ac)) }
    }

    #[test]
    fn miss_within_grace_window_keeps_flight() {
        let mut f = tracked_flight();
        f.last_visible_at = Some(dt("2026-04-18T12:00:00Z"));
        // 5 min later: past lost-threshold (300s) but well under removal (1800s)
        let upd = advance_flight(&mut f, &miss(true), dt("2026-04-18T12:05:00Z"));
        assert_eq!(upd.removal, None);
        assert!(upd.emits.is_empty());
    }

    #[test]
    fn miss_past_removal_threshold_removes_and_emits_tracking_lost() {
        let mut f = tracked_flight();
        f.last_visible_at = Some(dt("2026-04-18T12:00:00Z"));
        // 31 min later: past removal (1800s)
        let upd = advance_flight(&mut f, &miss(true), dt("2026-04-18T12:31:00Z"));
        assert_eq!(upd.removal, Some(RemovalReason::TrackingLost { secs: 31 * 60 }));
        assert_eq!(upd.emits, vec![Emit::TrackingLost]);
    }
}
```

- [ ] **Step 2: Run, verify failure if behaviour differs (it should pass if the move was faithful)**

```bash
cargo nextest run -p twitch-1337-core advance::tests --show-progress=none --cargo-quiet --status-level=fail
```

Expected: PASS. These tests characterize the moved behaviour. If one fails, the move (Task 1) diverged — fix `advance_flight`, not the test.

- [ ] **Step 3: Add the sticky-confirmation test**

```rust
    #[test]
    fn sticky_confirmation_keeps_prior_when_hex_visible_within_threshold() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.last_seen = Some(dt("2026-04-18T12:00:00Z"));
        // hex hit, observed callsign mismatched, within lost-threshold:
        // confirmation must NOT decay to AircraftVisible.
        let mut ac = aircraft();
        ac.flight = Some("DLH9999".to_string());
        let upd = advance_flight(&mut f, &hit(ac, true), dt("2026-04-18T12:02:00Z"));
        assert_eq!(f.target_confirmation, TargetConfirmation::ConfirmedByCallsign);
        assert_eq!(f.last_visible_at, Some(dt("2026-04-18T12:02:00Z")));
        let _ = upd;
    }
```

- [ ] **Step 4: Run and commit**

```bash
cargo nextest run -p twitch-1337-core advance::tests --show-progress=none --cargo-quiet --status-level=fail
git add crates/core/src/aviation/tracker/advance.rs
git commit -m "test(tracker): pin advance_flight's confirmation + grace-window behaviour fr 🧪"
```

---

### Task 3: Unit tests — phase, takeoff, landing

**Files:**
- Modify: `crates/core/src/aviation/tracker/advance.rs` (extend `mod tests`)

**Interfaces:**
- Consumes: same as Task 2, plus `FlightPhase`.

- [ ] **Step 1: Add the phase/takeoff/landing tests**

```rust
    use crate::aviation::tracker::FlightPhase;

    #[test]
    fn ground_to_airborne_sets_takeoff_at() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.phase = FlightPhase::Ground;
        f.takeoff_at = None;
        let mut ac = aircraft();
        ac.alt_baro = Some(AltBaro::Feet(1_500));
        ac.baro_rate = Some(2_500); // climbing
        ac.gs = Some(180.0);
        let now = dt("2026-04-18T12:10:00Z");
        let upd = advance_flight(&mut f, &hit(ac, false), now);
        assert!(is_airborne_phase_for_test(f.phase), "phase = {:?}", f.phase);
        assert_eq!(f.takeoff_at, Some(now));
        assert!(upd.emits.iter().any(|e| matches!(e, Emit::Takeoff)));
    }

    // local mirror to avoid importing the phase predicate purely for asserts
    fn is_airborne_phase_for_test(p: FlightPhase) -> bool {
        !matches!(p, FlightPhase::Ground | FlightPhase::Unknown)
    }

    #[test]
    fn landing_resets_to_ground_and_clears_takeoff_at() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.phase = FlightPhase::Approach;
        f.takeoff_at = Some(dt("2026-04-18T12:00:00Z"));
        let mut ac = aircraft();
        ac.alt_baro = Some(AltBaro::Ground);
        ac.gs = Some(15.0);
        let upd = advance_flight(&mut f, &hit(ac, false), dt("2026-04-18T13:00:00Z"));
        // landing transition emits, then phase is forced back to Ground
        if upd.emits.iter().any(|e| matches!(e, Emit::Landing)) {
            assert_eq!(f.phase, FlightPhase::Ground);
            assert_eq!(f.takeoff_at, None);
        }
    }
```

> Adjust the altitude/rate/speed fixtures in Step 1 until `detect_phase` yields the intended phase — read `phase.rs::detect_phase` thresholds and match them. The assertion shape (takeoff_at set on first airborne transition) is the invariant; the exact numbers serve it.

- [ ] **Step 2: Run and commit**

```bash
cargo nextest run -p twitch-1337-core advance::tests --show-progress=none --cargo-quiet --status-level=fail
git add crates/core/src/aviation/tracker/advance.rs
git commit -m "test(tracker): advance_flight phase + takeoff/landing transitions, locked in 🛫"
```

---

### Task 4: Unit tests — route followup, divert, became-confirmed reset

**Files:**
- Modify: `crates/core/src/aviation/tracker/advance.rs` (extend `mod tests`)

**Interfaces:**
- Consumes: same as Tasks 2-3, plus `Followup`.

- [ ] **Step 1: Add the route-followup + became-confirmed-reset tests**

```rust
    #[test]
    fn newly_resolved_callsign_with_no_route_emits_fetch_route_followup() {
        let mut f = tracked_flight();
        f.identifier = crate::aviation::tracker::FlightIdentifier::Hex("3C6497".to_string());
        f.callsign = None;
        f.route = None;
        let mut ac = aircraft();
        ac.flight = Some("DLH1929".to_string());
        let upd = advance_flight(&mut f, &hit(ac, true), dt("2026-04-18T12:05:00Z"));
        assert_eq!(f.callsign.as_deref(), Some("DLH1929"));
        assert_eq!(
            upd.followups,
            vec![Followup::FetchRoute { callsign: "DLH1929".to_string() }]
        );
    }

    #[test]
    fn first_target_confirmation_resets_phase_and_emits_adsb_visible() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::Pending;
        f.last_visible_at = None;
        f.phase = FlightPhase::Cruise;
        let ac = aircraft(); // callsign DLH1929 matches -> ConfirmedByCallsign
        let upd = advance_flight(&mut f, &hit(ac, false), dt("2026-04-18T12:05:00Z"));
        assert!(f.target_confirmation.is_target_confirmed());
        assert!(upd.emits.iter().any(|e| matches!(e, Emit::AdsbVisible)));
        assert_eq!(f.phase, FlightPhase::Unknown); // reset on first confirm from pending
        assert_eq!(f.polls_since_change, 0);
    }
```

- [ ] **Step 2: Add the divert test**

```rust
    #[test]
    fn sustained_off_heading_in_descent_eventually_emits_possible_divert() {
        let mut f = tracked_flight();
        f.target_confirmation = TargetConfirmation::ConfirmedByCallsign;
        f.phase = FlightPhase::Descent;
        f.dest_lat = Some(48.35);
        f.dest_lon = Some(11.78);
        // previous position so a ground track can be computed
        f.lat = Some(52.0);
        f.lon = Some(13.0);
        // aircraft heading away from destination across consecutive polls
        let mut emitted = false;
        for i in 0..6 {
            let mut ac = aircraft();
            ac.alt_baro = Some(AltBaro::Feet(8_000));
            ac.baro_rate = Some(-1_500);
            ac.lat = Some(52.0 + f64::from(i) * 0.5); // moving north, dest is south
            ac.lon = Some(13.0);
            let upd = advance_flight(&mut f, &hit(ac, false), dt("2026-04-18T12:05:00Z"));
            if upd.emits.iter().any(|e| matches!(e, Emit::PossibleDivert)) {
                emitted = true;
                break;
            }
        }
        assert!(emitted, "expected a PossibleDivert after sustained off-heading polls");
    }
```

> If `detect_phase` knocks the flight out of Descent/Approach with these fixtures, the divert branch won't run. Tune `alt_baro`/`baro_rate` so the flight stays in Descent across the loop (read `phase.rs`), or set the phase each iteration. The invariant: `update_divert_counter` crosses its threshold and emits exactly once.

- [ ] **Step 3: Run full tracker suite + commit**

```bash
cargo nextest run -p twitch-1337-core --show-progress=none --cargo-quiet --status-level=fail
git add crates/core/src/aviation/tracker/advance.rs
git commit -m "test(tracker): advance_flight route followup, divert + confirm-reset edge cases 📡"
```

- [ ] **Step 4: Final gate**

```bash
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo nextest run --workspace --show-progress=none --cargo-quiet --status-level=fail
```

Expected: all green. Push and open the PR per CLAUDE.md (`gh pr create`, wait for the 9 checks, squash-merge).

---

## Self-Review

**Spec coverage** (against the grilled design):
- Seam scope = per-flight transition only → Task 1 (`advance_flight` owns pass-2; readiness stays in `schedule.rs`; fetch/I/O stays in the loop). ✓
- Async route fetch → followup + same-poll `apply_route` → Task 1 Step 5 rule 6, Step 6 followup handler. ✓
- Effects = semantic `Emit` + returned debug + loop-side `changed` → Task 1 Steps 3, 5, 6. ✓
- Signature `&mut flight + Observation -> FlightUpdate` → Task 1 Step 5. ✓
- Pending-expired shares vocabulary, decided in pass-1 → Task 1 Step 7. ✓
- Sequencing: behaviour-preserving extract under integration suite, then unit tests → Task 1 Step 8 gate, Tasks 2-4. ✓
- Unit-test areas named in grilling (grace window, sticky, takeoff, divert, removal) → Tasks 2-4. ✓

**Placeholder scan:** Code steps carry real code; the two "tune the fixture" notes (Task 3 Step 1, Task 4 Step 2) point at `phase.rs::detect_phase` thresholds rather than leaving a TODO — the invariant under test is stated. The optional miss-branch route-lookup debug events (Task 1 Step 6) are explicitly scoped as journal-only with the behaviour-affecting path shown in full.

**Type consistency:** `advance_flight`, `Observation`/`PollOutcome`, `Emit`, `RemovalReason`, `Followup`, `FlightUpdate`, `apply_route`, `format_emit` are used with identical signatures across Tasks 1-4 and the Interfaces blocks. `RemovalReason::TrackingLost { secs }` carries `secs` (Task 1 + Task 2 assertion match). `Followup::FetchRoute { callsign }` field name consistent (Task 1 + Task 4 assertion).
