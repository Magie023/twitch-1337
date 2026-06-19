# Flight Tracking Design/QA Document

This document describes the expected results for the chat commands `!track`, `!untrack`, `!flight`, `!flights`, and the `/flights` web page across important flight-tracking scenarios.

## Relevant timing and polling constants

The acceptance criteria are based on the current tracker constants:

| Constant | Value | QA meaning |
| --- | ---: | --- |
| `TRACKING_LOST_THRESHOLD` | 300 seconds (5 minutes) | After this much time since the last ADS-B visibility, a previously visible flight is considered signal-lost for debug/status evaluation, but it is not removed yet. |
| `TRACKING_LOST_REMOVAL` | 1800 seconds (30 minutes) | After this much time since the last ADS-B visibility, the flight is automatically removed and the localized signal-lost removal message is announced. |
| `POLL_FAST` | 30 seconds | Live polling for new/unstable phases and for Takeoff, Approach, and Landing. |
| `POLL_NORMAL` | 60 seconds | Live polling for Climb and Descent after the initial fast phase. |
| `POLL_SLOW` | 120 seconds | Live polling for Cruise/Ground/Unknown after the initial fast phase. |
| `POLL_TIMEOUT` | 10 seconds | Maximum duration of one live ADS-B lookup. |

For the web flights page, the snapshot request to the tracker must also answer within 500 ms; otherwise the page renders without flights and marks the tracker as busy.

## Global expectations

- `!track <callsign/hex>` validates the input, starts tracking, persists state to `flights.ron`, and replies with the localized tracking-started confirmation when the flight is accepted.
- `!untrack <callsign/hex>` removes an existing flight when the requesting user is authorized or originally tracked that flight; moderators/broadcasters may remove flights tracked by other users.
- `!flight <callsign/hex>` shows exactly one matching flight by identifier, resolved callsign alias, or hex; if there is no match, it replies with the localized not-found reply.
- `!flights` shows all currently tracked flights or the localized no-flights reply.
- `/flights` shows the same snapshot as a read-only list; deleting from the web page uses the same tracker state and must not block the page when the tracker is slow.

## Scenarios and acceptance criteria

### 1. Track a flight by ICAO callsign, for example `DLH1234`

**Setup:** A user sends `!track DLH1234`.

**Expected result:**

- `DLH1234` is stored as a callsign, not as a hex identifier.
- AviationStack may provide supplemental metadata, but metadata is not mandatory for tracking to start.
- If ADS-B immediately returns a callsign-confirmed aircraft, callsign, hex, phase, position, altitude, speed, and `last_visible_at` are initialized.
- Chat reply: the localized tracking-started confirmation for `DLH1234`; when the hex is known, the ADS-B link should be included.
- `!flight DLH1234`, `!flights`, and `/flights` show the flight immediately after successful tracking.
- After a successful sighting, live polling uses `POLL_FAST` (30 s) while fewer than five polls have occurred without a phase change or while the flight is in Takeoff/Approach/Landing; after that it uses `POLL_NORMAL` (60 s) for Climb/Descent or `POLL_SLOW` (120 s) for other phases.

### 2. Track a flight by IATA number, for example `LH1234`, including resolution to ICAO

**Setup:** A user sends `!track LH1234`.

**Expected result:**

- `LH1234` is treated as an IATA flight number/callsign input even if it could resemble a six-character hex value.
- The tracker attempts to resolve it to the operating ICAO callsign, for example `DLH1234`.
- IATA and ICAO values are kept as aliases so `!flight LH1234`, `!flight DLH1234`, `!untrack LH1234`, `!untrack DLH1234`, `!flights`, and `/flights` consistently find the same flight.
- When AviationStack resolution succeeds, the track reply includes metadata such as route, aircraft type, or scheduled times when available.
- Duplicate tracking through both IATA and ICAO identifiers must be prevented; a second track attempt replies with a message equivalent to the localized already-tracked reply.

### 3. Track a flight by ICAO24 hex

**Setup:** A user sends `!track 3C6589` or another six-character hex identifier.

**Expected result:**

- The input is stored as `FlightIdentifier::Hex` and marked as a user-provided hex value.
- Polling is performed by hex; an ADS-B hit may confirm the target aircraft even without a callsign.
- If a callsign becomes visible later, it is stored as the `callsign`/alias; after that, `!flight <hex>`, `!flight <callsign>`, `!untrack <hex>`, and `!untrack <callsign>` work.
- If the initial hex lookup returns no hit, an error, or a timeout, no silent pending track is created without AviationStack fallback; the chat reply is the localized ADS-B not-found reply, the localized ADS-B request-failed reply, or the localized ADS-B timeout reply.

### 4. Flight has not departed yet and ADS-B is still empty

**Setup:** A callsign/IATA flight is tracked, AviationStack knows a scheduled departure, and ADS-B returns no hit yet.

**Expected result:**

- The flight remains stored as `Pending` when AviationStack is enabled for callsign/IATA tracking.
- `!flight` shows the flight as `Unknown` or not yet ADS-B-confirmed; `/flights` shows available metadata but no live position/altitude.
- The tracker avoids premature ADS-B load: if scheduled departure is more than 3 hours in the future, the initial ADS-B lookup is skipped.
- Around scheduled departure, the pending flight is polled more frequently; after the first ADS-B visibility, the live intervals `POLL_FAST`/`POLL_NORMAL`/`POLL_SLOW` apply again.
- A pending flight without ADS-B visibility must not be removed because of `TRACKING_LOST_THRESHOLD` or `TRACKING_LOST_REMOVAL`; those values apply only after `last_visible_at` exists.

### 5. AviationStack provides metadata, but ADS-B does not yet

**Setup:** `!track LH1234` or `!track DLH1234`; AviationStack returns route/times/type/hex, and ADS-B returns `None`.

**Expected result:**

- Tracking still starts and replies with the localized tracking-started confirmation plus AviationStack information.
- Route, scheduled departure, aircraft type, ICAO/IATA aliases, and optionally AviationStack hex are stored.
- If an AviationStack hex is known, the tracker may later search by hex as a supplement; without actual ADS-B visibility, the flight remains pending.
- `!flight`, `!flights`, and `/flights` show metadata but do not invent a live position.
- ADS-B backend errors during this scenario must not prevent tracking as long as the pending track was accepted through AviationStack/callsign fallback; the individual lookup may block for at most `POLL_TIMEOUT` (10 s).

### 6. ADS-B returns a hit without a callsign

**Setup:** ADS-B returns an aircraft with hex/position but without `flight`/callsign.

**Expected result:**

- For hex tracking, the hit confirms the target aircraft; telemetry and `last_visible_at` are updated.
- For callsign tracking, a callsign-less hit may be used only when the hex is sufficiently established, for example through a user-provided hex or consistent metadata/time window; otherwise the flight stays pending or the hit is ignored.
- `!flight` and `/flights` may show the localized aircraft-visible-but-target-unconfirmed status when only the aircraft is visible but the specific target callsign has not yet been confirmed.
- As soon as a callsign becomes visible later and matches, the flight becomes target-confirmed and is polled normally using the live intervals.

### 7. Callsign changes or only becomes visible later

**Setup:** A flight was tracked by IATA, an old callsign, or hex; ADS-B later shows a different or first-time callsign.

**Expected result:**

- The tracker adopts a later visible callsign when it matches the target or is plausibly confirmed through hex/metadata.
- New callsigns are added as aliases so old and new queries work for `!flight` and `!untrack`.
- A non-matching callsign on the same hex must not immediately retarget an already confirmed flight to the wrong flight; the existing target confirmation remains sticky while the hex is visible.
- Status output may show the localized current ADS-B callsign detail for a not-yet-confirmed target flight without treating that as a target confirmation.

### 8. Backend returns 429, 403, 5xx, timeout, or invalid JSON

**Setup:** AviationStack, the ADS-B aggregator, or the route backend responds with a rate-limit/auth/server error, timeout, or invalid JSON.

**Expected result:**

- Individual live ADS-B lookups stop no later than `POLL_TIMEOUT` (10 s).
- For callsign/IATA tracking with AviationStack fallback, a pending track can still be created; the reply may include the localized AviationStack-unavailable/ADS-B-only note when metadata is unavailable.
- For hex tracking without an initial ADS-B hit, no blind track is created; users receive a clear error or timeout reply.
- Existing flights are not removed because of temporary errors, and their last known position remains unchanged.
- Automatic removal may happen only after real ADS-B non-visibility since `last_visible_at`, not because of HTTP errors, 429/403/5xx responses, or invalid JSON.
- Errors are recorded in the debug journal/tracing as distinguishable Error/Timeout/Miss outcomes so rate limits are not misinterpreted as “flight disappeared”.

### 9. Tracker process restarts and loads `flights.ron`

**Setup:** Tracked flights exist in `flights.ron` before restart.

**Expected result:**

- On startup, the tracker reads `flights.ron`; if the file is missing, it starts with an empty state.
- If RON is invalid, the tracker starts fresh, logs a warning, and must not crash.
- Load-time migrations run: pending callsign hexes without `last_seen` are cleared, older confirmed flights receive `target_confirmation`, aliases are reseeded, and `last_visible_at` is backfilled from `last_adsb_poll_at`/`last_seen`.
- After restart, `!flights`, `!flight`, and `/flights` show the same loaded state.
- Polling continues based on the loaded timestamps; an old visible flight may be automatically removed only after `TRACKING_LOST_REMOVAL` (30 min) since `last_visible_at`.

### 10. Flight lands, is lost, or is automatically removed

**Setup:** A confirmed flight transitions to Landing/Ground or disappears from ADS-B.

**Expected result:**

- On detected landing, the localized landing announcement with flight time is announced; the phase is set to Ground and `takeoff_at` is reset.
- Landing alone does not necessarily mean immediate removal; the flight remains visible until it is manually removed or the automatic lost logic applies.
- If no ADS-B hit has arrived for at least `TRACKING_LOST_THRESHOLD` (5 min) since `last_visible_at`, the signal is considered lost, but the flight remains visible in `!flight`, `!flights`, and `/flights` for now.
- If no ADS-B hit has arrived for at least `TRACKING_LOST_REMOVAL` (30 min) since `last_visible_at`, the flight is automatically removed and chat announces the localized signal-lost removal announcement.
- After removal, `!flight <id>` replies with the localized not-found reply; `!flights` and `/flights` no longer list the flight.

### 11. Snapshot for `crates/web/src/routes/flights.rs` should answer quickly

**Setup:** `/flights` is loaded while the tracker is working normally or currently polling slow backends.

**Expected result:**

- The route sends `TrackerCommand::Snapshot` to the tracker and waits at most 500 ms for the reply.
- The tracker prioritizes latency-sensitive commands such as Snapshot and Web Delete, so a normal snapshot returns well below 500 ms and renders the current `TrackedFlightView` list.
- If the tracker is unavailable, `/flights` renders empty with `aviation_disabled = true` instead of hanging.
- If the tracker is busy and the 500 ms limit is exceeded, `/flights` renders empty with `tracker_busy = true`; this is degraded but acceptable behavior.
- QA should verify that slow ADS-B/AviationStack backends and `POLL_TIMEOUT` (10 s) do not block a web request until the backend timeout completes.

## Minimal regression checklist

- `!track DLH1234` starts an ICAO callsign track and is visible in `!flight`, `!flights`, and `/flights`.
- `!track LH1234` resolves/aliases IATA to ICAO and prevents duplicates.
- `!track <hex>` tracks only with an initially confirmed ADS-B hex or reports the appropriate error.
- Pending flights with metadata remain stored until ADS-B appears or the pending expiry applies.
- HTTP errors and invalid JSON do not remove existing flights.
- Restart robustly loads `flights.ron` and runs migrations.
- Lost removal happens no earlier than 30 minutes after the last ADS-B visibility.
- `/flights` responds with the busy state within 500 ms during tracker congestion instead of blocking the request.
