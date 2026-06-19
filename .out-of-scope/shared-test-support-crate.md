# Shared Test-Support Crate

This project does not maintain a dedicated `test-support` workspace crate (nor
a cross-crate shared fixture module) to deduplicate test helpers between
`crates/core` and `crates/web`. Test fixtures live next to the suite that uses
them: `crates/core/tests/common/` and `crates/web/tests/helpers/`.

## Why this is out of scope

The recurring ask is "the clock fixtures and state builders are reimplemented
in both crates — consolidate them into one shared crate." On inspection the
duplication is shallow and the two halves are deliberately different, so a
shared home buys very little and costs a new structural dependency.

### The two clock fixtures implement two intentionally-separate traits

There is no single `Clock` trait to share against. The crates define their own,
for different reasons:

```rust
// crates/web/src/clock.rs — sync, now()-only, by design
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

// crates/core/src/util/clock.rs — async, drives time-based schedulers
#[async_trait]
pub trait Clock: Send + Sync {
    fn now_utc(&self) -> DateTime<Utc>;
    async fn sleep_until(&self, target: DateTime<Utc>);
}
```

The web trait's own doc comment states the split is intentional: a `now()`-only
trait "so route tests can substitute a stub clock without dragging in the async
`sleep_until` half of `core::util::clock::Clock` (which exists for time-driven
schedulers — not for HTTP request timing)."

So `web`'s `FixedClock`/`StepClock` (~30 LOC) and `core`'s `FakeClock` (a waiter
queue for paused-tokio tests, 80 LOC) implement *different* traits and cannot be
unified without first merging the production traits — which the web crate
explicitly rejected. `StepClock` (advances one second per `now()` call) has no
core analogue at all.

### The state builders share almost nothing concrete

`web`'s `build_state*` assembles a `WebState`; `core`'s `TestBot` assembles a
live `run_bot` behind `FakeTransport` + `FakeLlm` + wiremock + tempdir. Different
target types, different crates. The only trait-agnostic shared piece is a
handful of pure helpers like `seed_leaderboard` — not enough to justify a crate.

### A shared crate is a poor structural trade

To host both fixture sets, a `test-support` crate would have to dev-depend on
**both** `web` and `core` (web fixtures need `WebState`; core fixtures need
`Services`/`run_bot`). The workspace already carries a `web → core` production
edge and a dev-only `core → web` back-edge (`web_smoke.rs`); a third node for
~30 lines of shallow duplication is not worth it. Core already has the in-tree
pattern for sharing test code where it *does* fit — a `testing` feature
(`#[cfg(any(test, feature = "testing"))]`) consumed via `dev-dependencies` — but
it can't host web-trait clocks without seeing web's trait.

Severity is low: no behavior gap, no coverage gap. The "drift" between the two
clock impls is correct divergence (different traits for different needs), not
accidental rot.

## If this is revisited

The only defensible narrow slice is moving pure, trait-free builders
(`seed_leaderboard` and similar) behind `core`'s existing `testing` feature so
`web` can reuse them. That leaves the clocks where they are and does not need a
new crate. Reconsider the full shared crate only if the two production `Clock`
traits are unified for an unrelated reason — at which point the fixtures could
follow.

## Prior requests

- #309 — "Share test fixtures (clock + state builders) across core and web test suites" (split out from #250 cluster 4)
