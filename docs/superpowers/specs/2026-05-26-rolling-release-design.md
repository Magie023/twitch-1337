# Rolling Release Design

**Date:** 2026-05-26
**Status:** Approved, ready for implementation plan

## Context

Release-plz was removed in PR #231 because the automated release-PR flow misbehaved.
The bot is a solo-operated, single-binary deployment to one homelab host. There are no
external consumers of any crate. SemVer per crate or per workspace serves no audience.

This spec replaces the manual tag-and-release flow with a rolling release model.

## Goals

- Every merge to `main` produces a deployable image.
- Server pulls the new image automatically via registry webhook.
- Zero version bookkeeping. No `cargo set-version`, no CHANGELOG editing, no release PR.
- Surface enough provenance (build number, commit SHA) in the running bot to identify
  what is deployed.
- Keep storage bounded over years of pushes.

## Non-Goals

- Per-crate versioning. All four crates remain pinned to the same workspace version.
- Discrete tags or GitHub Releases.
- A `just release` recipe. Merging to main is the release.
- Webhook plumbing on the homelab. Out of scope; the user owns it.
- Smoke checks, deploy notifications, deploy kill switch, build-number race mitigation.
  Listed in brainstorming as #3, #4, #7, #8 — skipped for solo scale.

## Architecture

### Trigger

`.github/workflows/docker.yml` triggers on push to `main` instead of `v*` tag push.

```yaml
on:
  push:
    branches: [main]
    paths-ignore:
      - '**/*.md'
      - 'docs/**'
      - '.github/ISSUE_TEMPLATE/**'
```

### Image tags

Two tags per build, via `docker/metadata-action`:

- `latest` — every successful build.
- `b<github.run_number>` — monotonic per workflow run (e.g. `b1234`).

```yaml
tags: |
  type=raw,value=latest
  type=raw,value=b${{ github.run_number }}
```

`type=semver` patterns are removed.

### Concurrency

Flip `cancel-in-progress: false` to `true`. Rolling merges can queue; latest commit
should win and stale builds should be abandoned.

```yaml
concurrency:
  group: docker-${{ github.ref }}
  cancel-in-progress: true
```

### Build provenance in the binary

`docker/build-push-action` already passes `GIT_SHA`. Add `BUILD_NUM`:

```yaml
build-args: |
  GIT_SHA=${{ env.GIT_SHA }}
  BUILD_NUM=b${{ github.run_number }}
```

`Dockerfile`:

```dockerfile
ARG BUILD_NUM=dev
ENV BUILD_NUM=${BUILD_NUM}
```

`crates/web/build.rs` reads `BUILD_NUM` from env (falls back to `"dev"`), exposes it as
a compile-time env var alongside `GIT_SHA_SHORT`:

```rust
let build_num = env::var("BUILD_NUM").unwrap_or_else(|_| "dev".to_string());
println!("cargo:rustc-env=BUILD_NUM={build_num}");
println!("cargo:rerun-if-env-changed=BUILD_NUM");
```

### Version display

Replace `CARGO_PKG_VERSION` references in user-visible surfaces with `BUILD_NUM`:

| File | Line | Current | New |
|------|------|---------|-----|
| `crates/web/templates/sidebar.html` | 14 | `v{{ env!("CARGO_PKG_VERSION") }}` | `{{ env!("BUILD_NUM") }}` |
| `crates/web/src/routes/health.rs` | 9 | `pub const PKG_VERSION: &str = env!("CARGO_PKG_VERSION");` | `pub const BUILD_NUM: &str = env!("BUILD_NUM");` |
| `crates/core/src/util/mod.rs` | 11 | `concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"),)` | `concat!(env!("CARGO_PKG_NAME"), "/", env!("BUILD_NUM"),)` |

`crates/web/src/lib.rs` template context exposes `build` (replacing any `version` field
backed by `PKG_VERSION`).

### /health endpoint

Extend the JSON response to include `build` and keep `sha`:

```json
{
  "build": "b1234",
  "sha": "abc1234",
  "uptime_secs": 42
}
```

Implementation lives in `crates/web/src/routes/health.rs`. Use the existing
`GIT_SHA` constant; add `BUILD_NUM` constant; expose both in the response struct.

### Cargo.toml workspace version

Pin `workspace.package.version = "0.0.0"` permanently. Cargo requires the field for
`workspace.package`; the value is now meaningless. A short comment documents the
intent so a future contributor doesn't bump it expecting effect.

### CHANGELOG removal

Delete the four per-crate files:

- `crates/twitch-1337/CHANGELOG.md`
- `crates/web/CHANGELOG.md`
- `crates/core/CHANGELOG.md`
- `crates/llm/CHANGELOG.md`

No workspace-level `CHANGELOG.md` exists currently; none is added. Git log is the
changelog now.

### GHCR retention

Add `.github/workflows/ghcr-retention.yml`. Runs weekly (Sunday) and on `workflow_dispatch`.
Keeps the last 30 image versions plus any tagged `latest`. Deletes everything older.

Uses `actions/delete-package-versions` (pinned to commit SHA per repo policy):

```yaml
name: GHCR retention

on:
  schedule:
    - cron: '0 4 * * 0'  # Sundays 04:00 UTC, after data-refresh at 03:00
  workflow_dispatch:

jobs:
  prune:
    runs-on: ubuntu-latest
    permissions:
      packages: write
    steps:
      - uses: actions/delete-package-versions@<sha>  # vX.Y.Z
        with:
          package-name: twitch-1337
          package-type: container
          min-versions-to-keep: 30
          delete-only-untagged-versions: false
          ignore-versions: '^latest$'
```

### CLAUDE.md updates

Replace the "Release flow" section with:

```markdown
**Release flow (rolling):**
1. Merge a PR into `main`. `docker.yml` triggers on push to main (paths-ignore
   skips docs-only changes).
2. Image is built and pushed to `ghcr.io/chronophylos/twitch-1337` with tags
   `latest` and `b<run_number>`.
3. Homelab webhook fires on GHCR push; server pulls `:latest` and restarts.
4. Bot sidebar + `/health` show `BUILD_NUM` + `GIT_SHA` for verification.
5. Rollback: `ssh docker.homelab`, edit compose to pin `:b<N-1>`,
   `docker compose up -d`.

There are no version tags, no GitHub Releases, no CHANGELOG.md. `git log` is the
release history.
```

Drop the table row for `Docker` referencing tags. Drop the "manual version bumps"
guidance. Drop CHANGELOG-related text.

`Action pinning` paragraph: add `actions/delete-package-versions` to the SHA-pinned
list.

## Components

```
docker.yml (modified)
  ├── trigger: push to main (paths-ignore docs)
  ├── concurrency: cancel-in-progress: true
  ├── tags: latest + b<run_number>
  └── build-args: GIT_SHA + BUILD_NUM

ghcr-retention.yml (new)
  └── weekly prune, keep 30 + latest

Dockerfile (modified)
  └── ARG/ENV BUILD_NUM

crates/web/build.rs (modified)
  └── emit BUILD_NUM as rustc-env

crates/web/templates/sidebar.html (modified)
  └── show BUILD_NUM instead of CARGO_PKG_VERSION

crates/web/src/routes/health.rs (modified)
  └── BUILD_NUM constant + extended JSON

crates/core/src/util/mod.rs (modified)
  └── APP_USER_AGENT uses BUILD_NUM

Cargo.toml (modified)
  └── pin workspace version to 0.0.0 + comment

CLAUDE.md (modified)
  └── rolling release section

CHANGELOG.md files (deleted, 4 total)
```

## Data Flow

```
PR merged to main
  └─> docker.yml triggers
        ├─> cargo-chef cache restore
        ├─> musl static build (passes GIT_SHA + BUILD_NUM)
        ├─> docker buildx build
        └─> docker push :latest + :b<N>
              └─> GHCR webhook -> homelab
                    └─> docker pull :latest
                          └─> docker compose up -d
                                └─> bot starts, sidebar shows b<N> + sha

User visits /health
  └─> JSON { build: "b<N>", sha: "abc1234", uptime_secs: ... }
```

## Error Handling

- **CI build fails** — no image pushed, no webhook fires, server stays on previous
  image. Standard CI failure.
- **Webhook miss** — server keeps running prior image. Manual fix:
  `ssh docker.homelab; docker compose pull && docker compose up -d`.
- **Bad code merged** — rollback by pinning compose to `:b<N-1>`.
- **GHCR retention prune deletes wrong tag** — `min-versions-to-keep: 30` plus
  `ignore-versions: '^latest$'` guards against deleting `latest`. Worst case: a
  build from >30 deploys ago becomes unrecoverable; for solo scale that's fine.
- **Two PRs merge in quick succession** — `cancel-in-progress: true` kills the older
  workflow. The newer one's build wins. The older commit is still on main; its image
  just isn't built. Acceptable.

## Testing

- Build provenance: `cargo build` locally without `BUILD_NUM` set → sidebar shows
  `dev`. With `BUILD_NUM=b9999 cargo build` → sidebar shows `b9999`.
- `/health` route: integration test asserts `build` + `sha` fields present.
- `docker.yml` workflow: lint with `actionlint` (already a required check).
- `ghcr-retention.yml`: dry-run via `workflow_dispatch` on a fork first.

No new unit tests for version display beyond the `/health` integration.

## Migration

One-shot. No data migration. After this lands:

- Delete pre-existing `v0.1.0` git tag? No — leave it as history. Future tags simply
  won't be created.
- Existing `vX.Y.Z` images on GHCR stay until retention prunes them by age.

## Open Items

None. All open questions resolved during brainstorming.
