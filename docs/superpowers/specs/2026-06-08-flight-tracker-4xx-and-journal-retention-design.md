# Flight tracker: 4xx handling + debug-journal retention

Date: 2026-06-08
Branch: `feature/flight-tracker-debug-recording` (PR #276 follow-up)

## Context

PR #276 added a sanitized JSONL flight-tracker debug journal and quieted ADS-B /
aviation API 4xx responses. A code review surfaced three behavior/robustness
issues that were left for a follow-up decision (findings #2, #3, #12). This spec
covers fixing those three. It does **not** address finding #1 (the journal not
recording provider-refusal status) — that is a separate, larger change and stays
out of scope here.

## Changes

### 1. Streak-park non-403 4xx ADS-B responses (`crates/core/src/aviation/client.rs`)

**Problem.** In `fetch_adsb_merged`, a 403 parks the backend immediately and a
5xx feeds the 3-strike error-streak park, but every other 4xx
(400/404/410/451/422) does nothing — no park, no streak. Pre-PR, all non-success
responses fed the streak. A backend persistently returning a non-403 4xx is now
re-queried on every poll forever. For the readsb-v2 aggregators "no aircraft"
returns `200` with an empty array, so a 4xx is genuinely abnormal (bad path,
geofence, unsupported endpoint shape).

**Fix.** In the catch-all `Err(AdsbFetchError::ProviderRefused { status })` arm
(the non-403 branch), add `self.record_retryable_error(idx);`. This restores the
pre-PR behavior: the backend parks after `ERROR_STREAK_PARK` (3) consecutive
failures, and any single success calls `record_success` which resets the streak.
A query-specific 404 on an otherwise-healthy backend therefore never parks it.
The existing `debug!` log and `last_provider_outcome` string are unchanged. The
403 arm keeps its immediate `park_backend`.

**Tests.** Add a unit test: a backend returning 404 on every hex poll parks after
3 calls; a 404-then-200 backend stays live (streak resets).

### 2. Warn on aviationstack auth/quota 4xx (`crates/core/src/aviation/client.rs`)

**Problem.** `get_aviationstack_flight_metadata` maps every 4xx to `Ok(None)` at
`debug!` level. A `404`/`422` (no data) is common noise and quieting it is the
PR's intent, but `401` (bad key), `403` (plan limit), and `429` (quota
exhausted) are now invisible at default log level — the bot silently stops
enriching with no operator signal.

**Fix.** When `status.is_client_error()`: if status is **401, 403, or 429**, log
at `warn!` (provider, endpoint_kind, status; no body); otherwise keep the current
`debug!`. Either way still return `Ok(None)` so enrichment degrades gracefully.

**Tests.** Repoint the existing `aviationstack_4xx_is_quiet_miss_without_body`
test to use `404` for the quiet-miss case; both the quiet (404) and warned
(401/403/429) paths still assert `Ok(None)` and no body leak. Log level is not
unit-assertable here, so coverage stays behavior-level.

### 3. Bound the debug journal to the newest N files (`debug_journal.rs` + `loop_run.rs`)

**Problem.** The journal writes one `<YYYY-MM-DD>.jsonl` per day under
`$DATA_DIR/flight-tracker-debug/` and never prunes — the only persistent artifact
in the repo with no cap (transcripts rotate nightly, memory is byte-capped, GHCR
keeps the last 30). Growth is episodic (only while flights are tracked) but
unbounded on the prod `/data` volume.

**Fix.**
- New `pub(crate) async fn prune_debug_journals(data_dir: &Path, keep: usize)` in
  `debug_journal.rs`: list `*.jsonl` in the journal dir, sort by filename
  (`YYYY-MM-DD` sorts chronologically), delete all but the newest `keep`.
  Best-effort — errors `warn!`-and-continue, matching `append_debug_event`.
- New constant `DEBUG_JOURNAL_KEEP_FILES = 30` (≈one month), documented in
  CLAUDE.md "Key constants".
- Triggers in `run_flight_tracker`:
  - **Startup:** prune once after `load_tracker_state`.
  - **Date-rollover:** a loop-local `last_journal_date`; in the active (polling)
    branch, when `now.date_naive()` differs from the tracked value, prune and
    update. This fires only while flights are tracked — exactly when the dir is
    growing — and the first poll after an idle gap catches up any backlog.

**Tests.** Unit test: create `N + k` dated `.jsonl` files, run prune with
`keep = N`, assert the newest `N` survive and the oldest `k` are deleted.

## Out of scope

- Finding #1 (journal records flat `miss`/`error`, never the 403/429/status). A
  typed-outcome refactor that threads provider-refusal detail through the
  metadata/poll paths into the journal is deferred.
- Per-event journal I/O batching (finding #11).
- Any dashboard/settings.ron surface for these knobs; `DEBUG_JOURNAL_KEEP_FILES`
  is a compile-time constant.

## Defaults (confirmed)

- `DEBUG_JOURNAL_KEEP_FILES = 30`.
- aviationstack warn set: `{401, 403, 429}`.
