# Multi-backend ADS-B: parallel query + signal-ranked merge

**Issue:** #4 — Use multiple ADS-B backends for improved coverage
**Date:** 2026-05-29
**Status:** Design approved, pending spec review

## Problem

`AviationClient` queries ADS-B aggregators (adsb.lol, airplanes.live,
adsb.fi, ADSB.One) **sequentially**: the first backend that returns a
successful response wins, the rest are never queried (`fetch_adsb_response`,
`client.rs:227`). This means:

- Coverage is capped at whatever the *first reachable* backend sees. A
  plane that only one of the later backends can hear is invisible.
- Worst-case latency stacks: 4 backends × 2s timeout = up to 8s before
  giving up.

Issue #4 asks to query multiple backends and combine them for better
worldwide coverage, switching by signal strength.

## Goal

Query **all** non-parked backends in parallel, merge their aircraft into
a coverage superset, and for any aircraft seen by more than one backend
keep the single most-reliable copy (freshest position, strongest signal).

## Non-goals

- No change to the adsbdb route lookup (`get_flight_route`),
  adsbdb airline lookup, Nominatim geocoding, or Aviationstack enrichment.
  Those are single-source and out of scope.
- No new dashboard/settings knobs. Tuning values are compile-time consts.
- No proactive rate limiting / token bucket. Reactive cooldown only.

## Decisions (locked during brainstorming)

| Question | Decision |
|---|---|
| Merge model | Union + dedup by hex, keep best-signal copy. Maximizes coverage. |
| Scope | Parallel everywhere (commands **and** tracker poll loop) + rate-limit guard. |
| Rank metric | Freshest position first (`seen_pos`), rssi tiebreak. |
| Freshness equivalence | Quantize `seen_pos` into `FRESH_WINDOW`-wide buckets, then rssi within a bucket. |
| `FRESH_WINDOW` | 5s (healthy receivers cluster sub-2s; fading ones at 10s+). |
| Rate-limit guard | Reactive cooldown: park a backend on 429 / repeated errors for `ADSB_COOLDOWN`, auto-restore. |
| `ADSB_COOLDOWN` | 60s. |
| Config | None. Both values are consts. |

## Architecture

### 1. Capture signal fields — `aviation/types.rs`

`NearbyAircraft` (currently `types.rs:19`) deserializes no reliability
signal. Add two fields (readsb v2 returns both as JSON numbers):

```rust
pub rssi: Option<f64>,     // average signal strength, dBm; higher (less negative) = stronger
pub seen_pos: Option<f64>, // seconds since this receiver's last *position* message; lower = fresher
```

Purely additive. Both `Option` — a backend may omit either. `serde`
`#[derive(Deserialize)]` already present; missing fields default to `None`.

`NearbyAircraft` currently derives only `Debug, Deserialize`. The merge
consumes aircraft by value (no clone needed), so no new derive is
required.

### 2. Parallel fan-out — `aviation/client.rs`

Replace the sequential `fetch_adsb_response` loop with a parallel
fan-out + merge. New private method (name: `fetch_adsb_merged`):

1. For each backend **not currently parked** (see §4), build a future
   calling the existing `fetch_adsb_response_once(aggregator, &url)`.
   Futures borrow `&self` — drive them with
   `futures_util::future::join_all` (no `tokio::spawn`, so no `'static`
   / clone requirement). `futures-util` is already a workspace dep.
2. Each future is bounded by the existing per-backend
   `adsb_aggregator_timeout` (2s). Wall-clock for the whole batch ≈ the
   slowest single backend ≤ 2s — a latency win over today's sequential
   worst case.
3. Classify each result. Provenance is kept as a per-backend
   `(name, Vec<NearbyAircraft>)` pair at collection time — used only for
   the `debug!`/`warn!` log line — **not** stored on `NearbyAircraft`
   (that struct stays additive per §1, no provenance field).
   - `Ok(resp)` → keep `(aggregator.name, resp.aircraft)` for the merge.
   - `Err(RateLimited)` (HTTP 429) → park the backend (§4), log `warn!`.
   - `Err(Retryable)` **other than timeout** (5xx, parse failure,
     connect-refused) → count toward the error streak (§4), log `warn!`.
   - `Err(Retryable)` **timeout** → log `warn!` only; does **not** count
     toward parking (see §4 rationale — a slow backend with unique
     coverage must not be parked).
   - `Err(Fatal)` → log; does **not** abort the batch (other backends may
     still succeed). This is a change from today, where Fatal
     short-circuits the whole call (`client.rs:259`).

   **Error-type change required.** Today `AdsbFetchError` is
   `Retryable(Report) | Fatal(Report)` (`client.rs:121`), and a 429 is
   flattened into the `Retryable` message string (`client.rs:291`) — there
   is no way to branch on status. Add a distinct rate-limit case so the
   classification above is implementable, e.g.:

   ```rust
   enum AdsbFetchError {
       RateLimited,           // HTTP 429
       Timeout(eyre::Report), // is_timeout(): retryable, but NOT counted toward parking
       Retryable(eyre::Report),
       Fatal(eyre::Report),
   }
   ```

   `fetch_adsb_response_once` (`client.rs:269`) maps `status == 429` →
   `RateLimited`, `e.is_timeout()` → `Timeout`, `e.is_connect()` +
   non-2xx + parse failure → `Retryable`, other send errors → `Fatal`.
   (Note: in current code a parse failure is already `Retryable`, not
   `Fatal`.)
4. Feed all collected aircraft into the merge (§3) and return the merged
   `AdsbAircraftResponse`.

Callers are unchanged in shape:
- `get_aircraft_nearby` (point) → returns the merged `Vec`.
- `get_aircraft_by_hex` / `get_aircraft_by_callsign` → `.next()` on the
  merged vec (now the best copy across all backends, not just the first
  backend's).

### 3. Merge — union + dedup, signal-ranked

Given the flattened aircraft from every successful backend:

1. Partition into those **with** a `hex` and those without.
2. Hex-less aircraft (rare; a position with no Mode-S address) are kept
   as-is — no dedup key, so no merging.
3. Group hex-bearing aircraft by a **lowercased copy of `hex` used as the
   grouping key only** — never mutate the stored `hex` field. (readsb
   emits lowercase hex, but the tracker matches case-insensitively and
   tests assert the original uppercase value, e.g. `tracker/mod.rs`; a
   lowercased *stored* hex would break those.) For each group, keep the
   single winner.

   **Freshness bucket** (guard against garbage f64):

   ```
   fn freshness_bucket(seen_pos: Option<f64>) -> u64 {
       match seen_pos {
           Some(s) if s.is_finite() && s >= 0.0 => (s / FRESH_WINDOW).floor() as u64,
           _ => u64::MAX,   // missing, negative, or non-finite → sorts last
       }
   }
   ```

   A bare `(s / FRESH_WINDOW).floor() as u64` is unsafe: a negative or
   `NaN` f64 cast `as u64` saturates to `0` — the *freshest* bucket — so a
   backend reporting `seen_pos: -1.0`/`NaN` would wrongly win. `seen_pos`
   is `Option<f64>` from arbitrary upstream JSON, so this is reachable;
   the guard above forces such values to the last bucket.

   **Winner selection** — `f64` is not `Ord`, so a `(u64, Reverse<f64>)`
   sort key does **not** compile. Use an explicit comparator with
   `f64::total_cmp` via `min_by` (lower is better):

   ```rust
   group.into_iter().min_by(|a, b| {
       freshness_bucket(a.seen_pos)
           .cmp(&freshness_bucket(b.seen_pos))                        // fresher bucket first
           .then_with(|| {
               // higher rssi wins → compare b vs a (descending)
               b.rssi.unwrap_or(f64::MIN).total_cmp(&a.rssi.unwrap_or(f64::MIN))
           })
   })
   ```

   - Same `FRESH_WINDOW` bucket → **higher rssi wins**.
   - Different buckets → **fresher bucket wins, rssi ignored**.
   - Missing/garbage `seen_pos` → `u64::MAX` bucket → always loses.
   - All copies equal (e.g. all `seen_pos`/`rssi` = `None`) → `min_by`
     keeps the **first** element in iteration order; iterate groups in
     `adsb_aggregators` order so the highest-priority backend wins ties
     deterministically.
   - Quantizing (not a fuzzy `|a-b| < window` compare) keeps the ordering
     **transitive**, avoiding the `Ord`-contract violation fuzzy
     comparison would cause (A~B, B~C, A≠C).

4. Result = deduped hex aircraft (winners) ++ hex-less aircraft. This is
   the coverage superset.

`FRESH_WINDOW: f64 = 5.0` (seconds), module const in `client.rs`.

> Boundary note: the bucket edge is hard — `4.9s` and `5.1s` fall in
> different buckets despite being ~equal. Accepted: at the 5s scale both
> positions are "live enough"; the only alternative (relative
> clustering) reintroduces non-transitivity.

### 4. Cooldown guard — per-backend parking

`AviationClient` is `Clone` and shared across handler tasks, so the
parked state needs interior mutability. Use a single
`Arc<Mutex<Vec<BackendHealth>>>` (`std::sync::Mutex`), index-aligned with
`adsb_aggregators` — one lock, one allocation, both fields together:

```rust
struct BackendHealth {
    parked_until: Instant,  // parked_until <= now ⇒ available
    error_streak: u8,       // consecutive non-timeout retryable errors
}
```

- Init each entry `{ parked_until: now_at_construction, error_streak: 0 }`
  so every backend is available on the first query.
- Add `adsb_cooldown: Duration` field, default `ADSB_COOLDOWN`
  (`const ADSB_COOLDOWN: Duration = Duration::from_secs(60)`), overridable
  in tests like the existing `with_adsb_aggregator_timeout`.
- **Before** the fan-out: lock, snapshot which backends satisfy
  `parked_until <= now`, unlock; query only those. The comparison **must
  be `<=`** (not `<`): it makes a backend available on its first query
  (init equals construction `Instant`) and the instant its cooldown
  expires — the "cooldown expiry restores" test depends on it. Never hold
  a `std::sync::Mutex` across `.await`.
- **On result, lock briefly to update health:**
  - success → `error_streak = 0`.
  - `RateLimited` (429) → `parked_until = now + adsb_cooldown`,
    `error_streak = 0`.
  - non-timeout `Retryable` (5xx, parse, connect-refused) →
    `error_streak += 1`; if it reaches `ERROR_STREAK_PARK` (const, =3) →
    `parked_until = now + adsb_cooldown`, `error_streak = 0`.
  - `Timeout` → **no health change.** Counting timeouts toward parking
    would let a backend that is reachable but consistently slightly slower
    than the 2s timeout get parked for 60s — and if that backend is the
    only one hearing planes in a sparse region, parking it punches exactly
    the coverage hole this feature exists to close. Timeout is a latency
    signal, not a broken-backend signal.
  - `Fatal` → no health change (logged, batch continues per §2).
- Auto-restore is implicit: once `now >= parked_until`, the backend is
  queried again. No background task.

`Instant` (not the project clock abstraction) is fine here: this is
relative monotonic timing, not Berlin wall-clock logic, and stays
testable via the configurable `adsb_cooldown`.

### 5. Failure semantics

- ≥1 backend returns a usable response → merged result (even if others
  errored or are parked).
- Every backend parked or errored → `Err`, preserving today's contract
  (`"All ADS-B aggregators failed"` / `"No ADS-B aggregators configured"`).

### Data flow

```
caller (command / tracker)
  └─ get_aircraft_nearby / by_hex / by_callsign
       └─ fetch_adsb_merged(endpoint)
            ├─ filter out parked backends            (lock parked, copy, unlock)
            ├─ join_all([fetch_adsb_response_once(b, url) for b in live])
            │     each ≤ 2s timeout, runs concurrently
            ├─ on 429 / error-streak ≥ N → park backend   (lock, set parked_until)
            ├─ flatten successes, tag provenance
            └─ merge: group by hex → (freshness bucket, rssi) winner
                 → AdsbAircraftResponse { aircraft: superset }
```

## Testing — `client.rs` `#[cfg(test)]`, wiremock

Determinism note: every dedup test below must set **both** `seen_pos`
**and** `rssi` on each mocked aircraft. Two copies that both leave them
`None` produce equal rank keys, so the winner falls back to backend
iteration order — fine for the existing fallback tests (only one backend
succeeds) but a nondeterminism trap for any test that expects a specific
copy to win. The `aircraft_response` test helper (`client.rs:570`) must
gain `seen_pos`/`rssi` params.

New:
- **union/coverage:** backend A returns plane X only, backend B returns
  plane Y only → merged result contains both X and Y.
- **dedup freshness:** both return plane X, A `seen_pos=1.0`,
  B `seen_pos=30.0` (both `rssi` set) → result keeps A's copy.
- **rssi tiebreak:** both return X with `seen_pos` in the same bucket
  (e.g. 1.0 and 2.0), A `rssi=-30`, B `rssi=-5` → result keeps B's copy.
- **bucket beats rssi:** A `seen_pos=1.0 rssi=-30`, B `seen_pos=20.0
  rssi=-1` → fresher A wins despite weaker signal.
- **missing seen_pos loses:** A `seen_pos=None`, B `seen_pos=10.0` → B wins.
- **garbage seen_pos loses:** A `seen_pos=-1.0` (or non-finite),
  B `seen_pos=10.0` → B wins (guard forces A to the last bucket; without
  the guard the `as u64` cast would saturate A to bucket 0 and wrongly
  win).
- **tie → first backend:** both return X with identical `seen_pos`/`rssi`
  → the copy from the earlier `adsb_aggregators` entry wins (pin the
  documented tie-break).
- **429 parks backend:** first call gets 429 from backend A; a second
  call within the cooldown window does **not** hit A (assert A request
  count stays 1), still returns B's data.
- **timeout does NOT park:** backend A times out 3× across three calls;
  the fourth call **still queries A** (assert A is hit on the 4th) — proves
  timeouts are excluded from the error streak.
- **cooldown expiry restores:** with a short test `adsb_cooldown`, a
  third call after expiry hits A again.
- **all-fail → error:** every backend 503 → `Err`.

Existing tests (`adsb_falls_back_from_5xx_to_next_aggregator`,
`adsb_falls_back_from_timeout_to_next_aggregator`) stay green: under
parallel fan-out both servers are queried, the failing one is ignored,
the healthy one's data is returned. Semantics shift from "stop at first
success" to "use the union" but the asserted outcome (backup's aircraft
returned, each server hit once) still holds. Update their doc comments
to reflect parallel semantics.

## Risks / tradeoffs

- **Request volume:** every query now fans out to all live backends —
  ~4× the request count. The tracker fans multiple flights concurrently
  too, so per-poll volume becomes (tracked flights) × (live backends) —
  e.g. 12 × 4 = 48 requests every 30–120s. The cooldown guard only reacts
  *after* a 429 (parks 60s), it does not pre-throttle. Free aggregators
  (adsb.lol et al.) have generous limits; if one starts 429ing it
  self-removes until it recovers. Watch for sustained 429s in the logs
  after rollout — if they appear, the proactive token-bucket option
  (deferred in brainstorming) becomes the follow-up.
- **Fatal-error semantics change:** today a `Fatal` error aborts the whole
  lookup (`client.rs:259`). Under fan-out it's logged and the batch
  continues, so one backend's failure can't blind us to the others. Note
  the only `Fatal` path is a non-timeout/non-connect send error — HTTP
  status errors and JSON parse failures are already `Retryable` in the
  current code, not `Fatal`.
- **Bucket boundary jitter:** see §3 note. Accepted.

## Touched files

- `crates/core/src/aviation/types.rs` — add `rssi`, `seen_pos` to
  `NearbyAircraft`.
- `crates/core/src/aviation/client.rs` — extend `AdsbFetchError`
  (`RateLimited` + `Timeout` variants); `fetch_adsb_merged` + merge +
  `BackendHealth` cooldown state; consts `FRESH_WINDOW`,
  `ADSB_COOLDOWN`, `ERROR_STREAK_PARK`; update tests + helper signature.
- (Possibly) `crates/core/src/aviation/mod.rs` — only if a merge helper
  warrants its own item; default is to keep it inside `client.rs`.

No changes to commands, tracker, settings, config, or embedded data.
