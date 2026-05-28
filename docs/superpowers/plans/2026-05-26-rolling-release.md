# Rolling Release Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace tag-based release with rolling release. Every merge to `main` builds + pushes an image; the homelab pulls `:latest` via GHCR webhook.

**Architecture:**
- `docker.yml` re-triggered on push to `main` (not `v*` tags) with paths-ignore for docs and `cancel-in-progress: true`.
- Images tagged `latest` + `b<run_number>`.
- A `BUILD_NUM` env var flows from CI → Dockerfile ARG → web crate's `build.rs` → compile-time `env!("BUILD_NUM")` consumed in sidebar template, lib.rs startup log, and a renamed health constant.
- Workspace version pinned permanently to `0.0.0`; per-crate CHANGELOG files deleted.
- New weekly `ghcr-retention.yml` prunes old GHCR versions.

**Tech Stack:** GitHub Actions, Docker BuildKit, Rust 2024 (workspace with crates `core`, `llm`, `twitch-1337`, `web`), Askama templates, axum.

**Scope deviations from spec:**

1. **`APP_USER_AGENT` keeps `CARGO_PKG_VERSION`.** The `concat!` macro forces compile-time resolution and `BUILD_NUM` is only emitted by `crates/web/build.rs` (the crate that needs it). Adding a `build.rs` to `core` solely to mirror the var, or switching `APP_USER_AGENT` to a runtime `LazyLock<String>` (which would break every `&str` caller), is not justified — UA strings are seen only by upstream HTTP servers, never by the operator. UA becomes `twitch-1337/0.0.0` and stays that way.
2. **`/healthz` stays a plain status code; no JSON extension.** The spec proposed extending `/health` to return `{build, sha, uptime_secs}`, but the existing endpoint is `/healthz` and returns only `200`/`503` — it's also the target of the Docker `HEALTHCHECK`. Changing the contract risks the container healthcheck and adds a new route + serde struct + integration test for a feature that has only one consumer (the operator). The sidebar + the `Build info` startup log already surface `BUILD_NUM` + `GIT_SHA`; that's enough provenance at solo scale.

**Branch:** Work continues on `feature/rolling-release` (already created and holds the design spec commit). Open a PR after Task 8.

---

## File Structure

**Modified:**
- `.github/workflows/docker.yml` — trigger, concurrency, tags, build-args
- `Dockerfile` — add `BUILD_NUM` ARG/ENV
- `crates/web/build.rs` — emit `BUILD_NUM` rustc-env with `"dev"` fallback
- `crates/web/templates/sidebar.html` — swap version display
- `crates/web/src/routes/health.rs` — rename `PKG_VERSION` → `BUILD_NUM`, switch source
- `crates/web/src/lib.rs` — startup log field rename
- `Cargo.toml` — pin workspace version
- `CLAUDE.md` — rewrite release section, drop CHANGELOG mentions, add retention action to SHA-pin list

**Created:**
- `.github/workflows/ghcr-retention.yml` — weekly prune

**Deleted:**
- `crates/twitch-1337/CHANGELOG.md`
- `crates/web/CHANGELOG.md`
- `crates/core/CHANGELOG.md`
- `crates/llm/CHANGELOG.md`

**Untouched:**
- `crates/core/src/util/mod.rs::APP_USER_AGENT` — see scope deviation above
- `crates/web/tests/healthz.rs` — existing tests stay green
- `Justfile` — no release recipe per design

---

## Task 1: Plumb BUILD_NUM through Dockerfile and web build.rs

**Files:**
- Modify: `Dockerfile:34-35`
- Modify: `crates/web/build.rs:41-55`

- [ ] **Step 1: Add BUILD_NUM ARG/ENV to Dockerfile**

Edit `Dockerfile`. In the **builder stage** (line ~34), right after the existing `GIT_SHA` ARG/ENV pair, add a `BUILD_NUM` pair. Final shape of that block:

```dockerfile
# Build-arg: short commit SHA of the source tree. Required because
# .dockerignore strips .git/, so the web crate's build.rs cannot derive
# it itself. Defaults to "unknown" if the caller does not pass one.
ARG GIT_SHA=unknown
ENV GIT_SHA=${GIT_SHA}

# Build-arg: monotonic build number (GitHub Actions run number). Surfaced
# in the dashboard sidebar and startup log so the operator can see which
# CI run produced the running image. Defaults to "dev" for local builds.
ARG BUILD_NUM=dev
ENV BUILD_NUM=${BUILD_NUM}
```

- [ ] **Step 2: Emit BUILD_NUM from web build.rs**

Edit `crates/web/build.rs`. Add a `build_num()` helper mirroring `git_sha()`'s shape, and emit the rustc-env. Final shape of the relevant section:

```rust
/// Resolve the build's CI run number. Docker builds get it via the
/// `BUILD_NUM` build-arg; local cargo runs fall back to "dev".
fn build_num() -> String {
    env::var("BUILD_NUM")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "dev".to_owned())
}

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    for name in ["app.css", "app.js", "htmx.min.js", "favicon.svg"] {
        let path = Path::new(&manifest).join("assets").join(name);
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let key = name.replace(['.', '-'], "_").to_uppercase();
        println!("cargo:rustc-env=ASSET_V_{key}={:016x}", fnv1a64(&bytes));
        println!("cargo:rerun-if-changed=assets/{name}");
    }

    println!("cargo:rustc-env=GIT_SHA_SHORT={}", git_sha());
    println!("cargo:rerun-if-env-changed=GIT_SHA");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs/heads");

    println!("cargo:rustc-env=BUILD_NUM={}", build_num());
    println!("cargo:rerun-if-env-changed=BUILD_NUM");
}
```

- [ ] **Step 3: Verify cargo check passes**

Run: `cargo check --workspace`
Expected: PASS, no warnings. (BUILD_NUM is emitted but not yet consumed; that's fine.)

- [ ] **Step 4: Verify the env var is honored**

Run: `BUILD_NUM=b9999 cargo build -p twitch-1337-web --bin web-dev`
Expected: PASS. The binary will surface "dev" still since no consumer was switched yet — verified in Task 2.

- [ ] **Step 5: Commit**

```bash
git add Dockerfile crates/web/build.rs
git commit -m "$(cat <<'EOF'
build: plumb BUILD_NUM through Dockerfile + web build.rs

Mirror the GIT_SHA pattern: CI passes BUILD_NUM as a Docker build-arg,
Dockerfile re-exports it as an env var visible to cargo, and the web
crate's build.rs emits it as a rustc-env (with "dev" fallback). No
consumers switched yet; that lands in the next commit.
EOF
)"
```

---

## Task 2: Swap consumers from CARGO_PKG_VERSION to BUILD_NUM

**Files:**
- Modify: `crates/web/src/routes/health.rs:9`
- Modify: `crates/web/src/lib.rs:64`
- Modify: `crates/web/templates/sidebar.html:14`

- [ ] **Step 1: Rename PKG_VERSION → BUILD_NUM in health.rs**

Edit `crates/web/src/routes/health.rs`. Replace line 9:

```rust
pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");
```

with:

```rust
pub const BUILD_NUM: &str = env!("BUILD_NUM");
```

- [ ] **Step 2: Update the lib.rs startup log field**

Edit `crates/web/src/lib.rs:62-67`. Replace the `info!` block's `version = ...` line so the section reads:

```rust
    info!(
        target: "twitch_1337_web",
        build = routes::health::BUILD_NUM,
        sha = routes::health::GIT_SHA,
        "Build info",
    );
```

- [ ] **Step 3: Update the sidebar template**

Edit `crates/web/templates/sidebar.html:14`. Replace `v{{ env!("CARGO_PKG_VERSION") }}` with `{{ env!("BUILD_NUM") }}`. The full line becomes:

```html
      <span class="brand-meta">{{ env!("BUILD_NUM") }} · <a class="brand-sha" href="https://github.com/Chronophylos/twitch-1337/commit/{{ env!("GIT_SHA_SHORT") }}" target="_blank" rel="noopener noreferrer" title="commit {{ env!("GIT_SHA_SHORT") }}">{{ env!("GIT_SHA_SHORT") }}</a></span>
```

- [ ] **Step 4: Run clippy + tests**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: PASS, no warnings (`PKG_VERSION` removal must not orphan any reference).

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: all tests pass. `healthz_returns_200_when_irc_connected` + `healthz_returns_503_when_irc_disconnected` still green (they don't touch `PKG_VERSION`).

- [ ] **Step 5: Smoke-test the local fallback**

Run: `cargo run -p twitch-1337-web --bin web-dev --features dev-login 2>&1 | head -5 &`
Wait 2s, then: `curl -s http://127.0.0.1:8761/healthz; pkill -f 'target/debug/web-dev'`
Expected: startup log contains `build="dev" sha=<something>`. `curl` returns 200 (after dev-login session set) or 503 depending on IRC. Status code is incidental — the assertion is that the log line carries `build="dev"`.

- [ ] **Step 6: Smoke-test the env-var path**

Run: `BUILD_NUM=b9999 cargo run -p twitch-1337-web --bin web-dev --features dev-login 2>&1 | head -5 &`
Wait 2s, then: `pkill -f 'target/debug/web-dev'`
Expected: startup log contains `build="b9999"`.

- [ ] **Step 7: Commit**

```bash
git add crates/web/src/routes/health.rs crates/web/src/lib.rs crates/web/templates/sidebar.html
git commit -m "$(cat <<'EOF'
feat(web): surface BUILD_NUM instead of CARGO_PKG_VERSION

Rolling release pins workspace.package.version to 0.0.0, so v0.1.0 in
the sidebar would be a lie forever. Swap to BUILD_NUM (CI run number,
"dev" locally) in the sidebar, startup log, and the health::BUILD_NUM
constant. APP_USER_AGENT stays on CARGO_PKG_VERSION — upstream-only,
not worth a build.rs in core.
EOF
)"
```

---

## Task 3: Rewire docker.yml for rolling release

**Files:**
- Modify: `.github/workflows/docker.yml`

- [ ] **Step 1: Read the current workflow**

Run: `cat .github/workflows/docker.yml`
Expected: sees `on: push: tags: ['v*']`, `cancel-in-progress: false`, `type=semver` patterns, no `BUILD_NUM` build-arg.

- [ ] **Step 2: Replace the workflow content**

Write `.github/workflows/docker.yml` with this exact content (pinned action SHAs preserved from the existing file — do **not** change them in this task):

```yaml
name: Docker

on:
  push:
    branches: [main]
    paths-ignore:
      - '**/*.md'
      - 'docs/**'
      - '.github/ISSUE_TEMPLATE/**'

concurrency:
  group: docker-${{ github.ref }}
  cancel-in-progress: true

jobs:
  build:
    name: build and push
    runs-on: ubuntu-latest
    permissions:
      contents: read
      packages: write
    steps:
      - uses: actions/checkout@de0fac2e4500dabe0009e67214ff5f5447ce83dd # v6
        with:
          persist-credentials: false

      - uses: docker/setup-buildx-action@4d04d5d9486b7bd6fa91e7baf45bbb4f8b9deedd # v4

      - uses: docker/login-action@4907a6ddec9925e35a0a9e82d7399ccc52663121 # v4
        with:
          registry: ghcr.io
          username: ${{ github.actor }}
          password: ${{ secrets.GITHUB_TOKEN }}

      - id: meta
        uses: docker/metadata-action@030e881283bb7a6894de51c315a6bfe6a94e05cf # v6
        with:
          images: ghcr.io/chronophylos/twitch-1337
          tags: |
            type=raw,value=latest
            type=raw,value=b${{ github.run_number }}

      - uses: docker/build-push-action@bcafcacb16a39f128d818304e6c9c0c18556b85f # v7
        env:
          GIT_SHA: ${{ github.sha }}
          BUILD_NUM: b${{ github.run_number }}
        with:
          context: .
          push: true
          tags: ${{ steps.meta.outputs.tags }}
          labels: ${{ steps.meta.outputs.labels }}
          build-args: |
            GIT_SHA=${{ env.GIT_SHA }}
            BUILD_NUM=${{ env.BUILD_NUM }}
          cache-from: type=gha
          cache-to: type=gha,mode=max
```

- [ ] **Step 3: Lint with actionlint**

Run: `actionlint .github/workflows/docker.yml`
(If actionlint is not installed locally, skip — CI's `actionlint (workflows)` job covers it.)
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/docker.yml
git commit -m "$(cat <<'EOF'
build(ci): rolling release trigger on docker.yml

Trigger on push to main with paths-ignore for docs-only changes. Flip
concurrency cancel-in-progress to true so a faster follow-up commit
abandons the stale build instead of queueing behind it. Image tags
become latest + b<run_number>; semver patterns dropped. Pass BUILD_NUM
to the builder so the binary can surface it.
EOF
)"
```

---

## Task 4: Add ghcr-retention.yml weekly prune

**Files:**
- Create: `.github/workflows/ghcr-retention.yml`

- [ ] **Step 1: Look up the latest pinned SHA for actions/delete-package-versions**

The repo policy SHA-pins security-critical third-party actions. Resolve the latest stable tag and its commit SHA before authoring the file:

```bash
gh api -H "Accept: application/vnd.github+json" \
  repos/actions/delete-package-versions/releases/latest \
  --jq '{tag: .tag_name, sha: .target_commitish}'
```

If `target_commitish` is a branch name (e.g. `main`), resolve it to a commit SHA:

```bash
gh api -H "Accept: application/vnd.github+json" \
  repos/actions/delete-package-versions/git/refs/tags/<tag-from-above> \
  --jq '.object.sha'
```

Record the resulting `<sha>` and `<tag>` (e.g. `v5.0.0`) for use in Step 2.

- [ ] **Step 2: Write the retention workflow**

Create `.github/workflows/ghcr-retention.yml` with the resolved SHA and tag (substitute `<SHA>` and `<TAG>` from Step 1):

```yaml
name: GHCR retention

on:
  schedule:
    - cron: '0 4 * * 0'  # Sundays 04:00 UTC, after data-refresh at 03:00
  workflow_dispatch:

permissions:
  contents: read

jobs:
  prune:
    runs-on: ubuntu-latest
    permissions:
      packages: write
    steps:
      - uses: actions/delete-package-versions@<SHA>  # <TAG>
        with:
          package-name: twitch-1337
          package-type: container
          min-versions-to-keep: 30
          delete-only-untagged-versions: false
          ignore-versions: '^latest$'
```

- [ ] **Step 3: Lint with actionlint + zizmor**

Run: `actionlint .github/workflows/ghcr-retention.yml`
Expected: no errors.

(zizmor runs in CI; if installed locally: `zizmor .github/workflows/ghcr-retention.yml`. The pinned SHA + minimal `permissions:` block should satisfy it. If zizmor flags `unpinned-uses` for the `actions/delete-package-versions` line, double-check Step 1's SHA was applied.)

- [ ] **Step 4: Dry-run via workflow_dispatch is deferred**

A live `workflow_dispatch` against GHCR would actually delete packages. Skip the dry-run; trust the `min-versions-to-keep: 30` + `ignore-versions: '^latest$'` guardrails on first scheduled run. Document the manual test step in Task 8's PR body so the operator can decide.

- [ ] **Step 5: Commit**

```bash
git add .github/workflows/ghcr-retention.yml
git commit -m "$(cat <<'EOF'
build(ci): weekly GHCR retention prune

Rolling release pushes one image per main merge; over a year that is
hundreds of versions. Prune to last 30 plus latest every Sunday at
04:00 UTC. delete-only-untagged-versions=false lets it touch the
b<run_number> tags; ignore-versions guards latest from accidental
removal.
EOF
)"
```

---

## Task 5: Pin workspace version to 0.0.0 and delete CHANGELOG files

**Files:**
- Modify: `Cargo.toml:5-9`
- Delete: `crates/twitch-1337/CHANGELOG.md`
- Delete: `crates/web/CHANGELOG.md`
- Delete: `crates/core/CHANGELOG.md`
- Delete: `crates/llm/CHANGELOG.md`

- [ ] **Step 1: Pin workspace.package.version**

Edit `Cargo.toml`. Replace the `[workspace.package]` block (lines 5–9) so it reads:

```toml
[workspace.package]
# Pinned: this project uses a rolling release model (see
# docs/superpowers/specs/2026-05-26-rolling-release-design.md). Version
# bumps serve no audience — the running build identifies itself via
# BUILD_NUM + GIT_SHA in the dashboard sidebar and startup log. Do not
# bump this field.
version = "0.0.0"
edition = "2024"
license = "MIT OR Apache-2.0"
publish = false
```

- [ ] **Step 2: Refresh Cargo.lock**

Run: `cargo update --workspace --offline`
Expected: PASS. Updates the four `twitch-1337*` entries in `Cargo.lock` to `0.0.0`. If `--offline` fails because the lockfile is fully synced already, run plain `cargo check` instead — it rewrites the lock to match the manifest.

- [ ] **Step 3: Delete the four per-crate CHANGELOG files**

Run:

```bash
git rm crates/twitch-1337/CHANGELOG.md \
       crates/web/CHANGELOG.md \
       crates/core/CHANGELOG.md \
       crates/llm/CHANGELOG.md
```

- [ ] **Step 4: Verify build + tests still pass**

Run: `cargo nextest run --show-progress=none --cargo-quiet --status-level=fail`
Expected: all tests pass. `APP_USER_AGENT` (unchanged from spec deviation) now embeds `0.0.0` — any test that asserts a specific UA string would need updating. Search first:

```bash
rg -n 'APP_USER_AGENT|"twitch-1337/' crates/ --type rust
```

If a test pins the old `0.1.0`, update it to `0.0.0` and re-run nextest before committing.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock
git commit -m "$(cat <<'EOF'
build: pin workspace version to 0.0.0 and drop per-crate CHANGELOGs

Rolling release: nothing consumes the workspace version. Pin to 0.0.0
with a comment so a future contributor does not bump it expecting
effect. Delete the four per-crate CHANGELOG.md files (release-plz
artifacts); git log is the changelog now.
EOF
)"
```

---

## Task 6: Update CLAUDE.md release section

**Files:**
- Modify: `CLAUDE.md` — Release flow section, Action pinning paragraph, status-checks table

- [ ] **Step 1: Replace the Release flow section**

Edit `CLAUDE.md`. Find the `**Release flow (manual):**` block (5 numbered items, ends with the "Rollback" bullet) and replace the entire block with:

```markdown
**Release flow (rolling):**
1. Merge a PR into `main`. `docker.yml` triggers on push to main with
   `paths-ignore` for docs/spec-only changes (no needless rebuilds).
2. CI builds the musl static binary and pushes the image to
   `ghcr.io/chronophylos/twitch-1337` with tags `latest` and
   `b<github.run_number>` (e.g. `b1234`).
3. The homelab webhook fires on GHCR push; the server pulls `:latest`
   and `docker compose up -d` restarts the bot.
4. The dashboard sidebar and the startup log line `Build info` show
   `BUILD_NUM` + `GIT_SHA` — that is the source of truth for "what is
   running right now".
5. Rollback: `ssh docker.homelab`, edit the compose file to pin
   `:b<N-1>`, `docker compose up -d`. The previous image is still
   present until the weekly GHCR retention prune (keeps last 30).

There are no `vX.Y.Z` tags, no GitHub Releases, no `CHANGELOG.md`. `git
log` on `main` is the release history. The workspace version stays at
`0.0.0` permanently.
```

- [ ] **Step 2: Update the Docker row in the status-checks table**

The `Docker` paragraph just under the required-status-checks table currently reads:

```
`Docker` (build + push `ghcr.io/chronophylos/twitch-1337:vX.Y.Z`, `X.Y`, `latest`)
triggers on `v*` tag push. Tags are created manually — no per-commit images.
Not a required check.
```

Replace with:

```
`Docker` (build + push `ghcr.io/chronophylos/twitch-1337:latest` and
`:b<run_number>`) triggers on push to `main` with paths-ignore for
docs. Not a required check.
`GHCR retention` runs Sundays 04:00 UTC; prunes old image versions
keeping the last 30 plus `:latest`. Not a required check.
```

- [ ] **Step 3: Add the retention action to the SHA-pin list**

Find the `**Action pinning:**` paragraph. Append `actions/delete-package-versions` to the SHA-pinned action list. Final paragraph:

```
**Action pinning:** security-critical actions pinned to **commit SHA** with version
comment: `rustsec/audit-check`, `gitleaks/gitleaks-action`, `zizmorcore/zizmor-action`,
`actions/delete-package-versions`, `aquasecurity/trivy-action` (Mar 2026 supply-chain
incident — always SHA-pin trivy). Others pinned to major tags; Dependabot keeps them
current.
```

- [ ] **Step 4: Verify the file**

Run: `rg -n "release-plz|CHANGELOG|v\\*.*tag" CLAUDE.md`
Expected: no matches. (CHANGELOG references should be gone; release-plz already removed in PR #229; no `v*` tag mentions in release context.)

- [ ] **Step 5: Commit**

```bash
git add CLAUDE.md
git commit -m "$(cat <<'EOF'
docs(claude-md): rolling release flow

Drop the manual tag+bump flow. Describe the new shape: merge to main
triggers docker.yml, image tagged latest + b<run_number>, server pulls
via webhook. BUILD_NUM + GIT_SHA in sidebar identify running build.
Document the new GHCR retention workflow and SHA-pin the action it
uses.
EOF
)"
```

---

## Task 7: Final verification before PR

- [ ] **Step 1: Run the full CI gate locally**

Run each in order; bail on first failure:

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo nextest run --show-progress=none --cargo-quiet --status-level=fail
cargo audit
```

Expected: all pass.

- [ ] **Step 2: Sanity-check the workflow trigger**

Run: `rg -n "tags:|branches:" .github/workflows/docker.yml`
Expected: shows `branches: [main]`, no `tags:` entry.

- [ ] **Step 3: Sanity-check the version constants**

Run: `rg -n "CARGO_PKG_VERSION|PKG_VERSION" crates/ --type rust`
Expected: only `crates/core/src/util/mod.rs:11` (APP_USER_AGENT — intentionally untouched). Sidebar template, health route, and lib.rs no longer reference it.

- [ ] **Step 4: Confirm CHANGELOG files are gone**

Run: `fd CHANGELOG.md`
Expected: no output.

- [ ] **Step 5: Review the commit chain**

Run: `git log --oneline main..feature/rolling-release`
Expected: 7 commits — design spec (already present) + 6 implementation commits in order:
1. `docs: rolling release design spec`
2. `build: plumb BUILD_NUM through Dockerfile + web build.rs`
3. `feat(web): surface BUILD_NUM instead of CARGO_PKG_VERSION`
4. `build(ci): rolling release trigger on docker.yml`
5. `build(ci): weekly GHCR retention prune`
6. `build: pin workspace version to 0.0.0 and drop per-crate CHANGELOGs`
7. `docs(claude-md): rolling release flow`

---

## Task 8: Push branch and open PR

- [ ] **Step 1: Push the branch**

Run: `git push -u origin feature/rolling-release`
Expected: branch published.

- [ ] **Step 2: Open the PR**

Run:

```bash
gh pr create --title "feat: rolling release model" --body "$(cat <<'EOF'
## Summary

Replace tag-based release with a rolling release model. Every merge to `main` triggers `docker.yml`; image is tagged `latest` + `b<run_number>` and the homelab pulls via GHCR webhook.

- `docker.yml` triggers on push to main (paths-ignore docs), `cancel-in-progress: true`, tags `latest` + `b<run_number>`, passes `BUILD_NUM` to the builder.
- `BUILD_NUM` flows through `Dockerfile` ARG/ENV → `crates/web/build.rs` rustc-env → sidebar template + startup log + `health::BUILD_NUM` constant. Local fallback: `"dev"`.
- `workspace.package.version` pinned to `0.0.0` with a comment. Per-crate `CHANGELOG.md` files deleted (release-plz artifacts).
- New weekly `ghcr-retention.yml` prunes GHCR versions, keeping last 30 + `latest`.
- `CLAUDE.md` rewritten: rolling flow, new Docker workflow description, retention action added to SHA-pin list.

Spec: `docs/superpowers/specs/2026-05-26-rolling-release-design.md`.

## Scope deviation from spec

`crates/core/src/util/mod.rs::APP_USER_AGENT` keeps `CARGO_PKG_VERSION`. The `concat!` macro forces compile-time resolution and `BUILD_NUM` is only emitted by `crates/web/build.rs`. Adding a build.rs to `core` or switching `APP_USER_AGENT` to a runtime `LazyLock<String>` isn't justified — UA strings are seen only by upstream HTTP servers, never the operator.

## Test plan

- [ ] CI green: fmt, clippy, test, audit, hadolint, trivy, actionlint, zizmor, gitleaks
- [ ] After merge: observe `docker.yml` fires on push to main, image lands on GHCR with `:latest` + `:b<N>`
- [ ] Homelab webhook pulls the new image; sidebar shows `b<N> · <sha>`
- [ ] Optional: trigger `GHCR retention` via `gh workflow run ghcr-retention.yml` once a handful of builds exist; verify only the oldest beyond 30 are pruned

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

Expected: PR URL printed.

- [ ] **Step 3: Wait for CI**

Run: `gh pr checks --watch`
Expected: all 9 required checks pass. If `Docker` build runs against this branch via the new trigger, it will succeed but image push goes to GHCR with `:b<N>` for the branch — accept this; it'll be superseded by the merge build.

---

## Notes

- **Do not merge if `Docker` job is queued.** With `branches: [main]` the workflow only fires on main pushes, so the PR itself won't trigger a build. The first rolling build runs at squash-merge time.
- **First rolling build sanity check.** After merge, watch `gh run watch` and confirm `:latest` is overwritten + `:b<N>` lands on GHCR. Pull on the homelab manually if the webhook isn't wired yet.
- **Rollback playbook.** If the first rolling build deploys broken code:
  1. `ssh docker.homelab`
  2. Edit the compose file's `image:` line to pin `ghcr.io/chronophylos/twitch-1337:b<previous-N>` (look it up at https://github.com/Chronophylos/twitch-1337/pkgs/container/twitch-1337)
  3. `docker compose up -d`
  4. File a `fix/` PR; merge re-triggers the rolling build with the correction.
