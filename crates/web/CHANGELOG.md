# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/Chronophylos/twitch-1337/releases/tag/twitch-1337-web-v0.1.0) - 2026-05-24

### Added

- migrate remaining config knobs to dashboard settings (v3) ([#224](https://github.com/Chronophylos/twitch-1337/pull/224))
- OpenRouter service tiers (flex / default / priority) ([#209](https://github.com/Chronophylos/twitch-1337/pull/209))
- *(web)* grouped settings sidebar + bytes-input control ([#208](https://github.com/Chronophylos/twitch-1337/pull/208))
- *(ai)* scope memory + clarify speaker identity in !ai prompt ([#206](https://github.com/Chronophylos/twitch-1337/pull/206))
- hoist AI config from config.toml to dashboard settings ([#203](https://github.com/Chronophylos/twitch-1337/pull/203))
- *(döner)* Dönerpreis Command ([#190](https://github.com/Chronophylos/twitch-1337/pull/190))
- *(web)* redesign settings page in "Quiet terminal" style ([#193](https://github.com/Chronophylos/twitch-1337/pull/193))
- *(settings)* dashboard-managed runtime settings + owner tier ([#192](https://github.com/Chronophylos/twitch-1337/pull/192))
- *(web)* static viewer allowlist; drop Helix follower gate ([#189](https://github.com/Chronophylos/twitch-1337/pull/189))
- *(web)* follower-gated viewer tier for dashboard ([#188](https://github.com/Chronophylos/twitch-1337/pull/188))
- *(web)* web-dev bin for worktree-side dashboard runs ([#177](https://github.com/Chronophylos/twitch-1337/pull/177))
- *(web)* add dev-login Cargo feature for local dashboard testing ([#175](https://github.com/Chronophylos/twitch-1337/pull/175))
- *(web)* redesign dashboard — quiet terminal ([#172](https://github.com/Chronophylos/twitch-1337/pull/172))
- *(web)* editable memory frontmatter + ping member CRUD ([#171](https://github.com/Chronophylos/twitch-1337/pull/171))
- *(web)* v2 dashboard — signed cookies, real bundles, sidebar, ?next=, form re-render ([#160](https://github.com/Chronophylos/twitch-1337/pull/160))
- *(web)* v1 dashboard — pings CRUD + AI memory editor ([#159](https://github.com/Chronophylos/twitch-1337/pull/159))

### Fixed

- *(release-plz)* unblock CI workflow ([#225](https://github.com/Chronophylos/twitch-1337/pull/225))
- *(web)* show OpenRouter tiers for default base URL ([#213](https://github.com/Chronophylos/twitch-1337/pull/213))
- *(web)* settings dirty tracking + polished text/range rows ([#205](https://github.com/Chronophylos/twitch-1337/pull/205))
- *(web)* dashboard polish pack — Dark Reader, breadcrumbs, version stamp, log cleanup ([#180](https://github.com/Chronophylos/twitch-1337/pull/180))
- *(web)* dashboard polish — avatar hue, breadcrumb, cancel URLs, sidebar link ([#179](https://github.com/Chronophylos/twitch-1337/pull/179))
- *(web)* pings empty-state blue tint + broken row delete ([#176](https://github.com/Chronophylos/twitch-1337/pull/176))
- *(web)* post-redesign dashboard fixes (cache-bust, hue, logout, fonts) ([#174](https://github.com/Chronophylos/twitch-1337/pull/174))
- *(web)* use moderated_channels for OAuth mod check ([#170](https://github.com/Chronophylos/twitch-1337/pull/170))
- *(web)* handle Twitch scope array + redact tokens in error logs ([#169](https://github.com/Chronophylos/twitch-1337/pull/169))
- *(web)* send OAuth client creds in body, not Basic auth header ([#168](https://github.com/Chronophylos/twitch-1337/pull/168))
- *(web)* friendly retry page on OAuth callback failure ([#167](https://github.com/Chronophylos/twitch-1337/pull/167))
- *(web)* improve OAuth callback error logging + bump askama/tokio ([#166](https://github.com/Chronophylos/twitch-1337/pull/166))

### Other

- *(ping)* replace Arc<RwLock<PingManager>> with mpsc actor + delete sync atomic_save_ron ([#220](https://github.com/Chronophylos/twitch-1337/pull/220))
- split aviation/tracker.rs and drop memory.rs validate_slug ([#219](https://github.com/Chronophylos/twitch-1337/pull/219))
- *(web)* per-card SaveForm structs + tri-state helpers ([#218](https://github.com/Chronophylos/twitch-1337/pull/218))
- *(deps)* bump tower-livereload from 0.9.6 to 0.10.3 ([#196](https://github.com/Chronophylos/twitch-1337/pull/196))
- More dashboard tweaks + dev iteration QoL ([#191](https://github.com/Chronophylos/twitch-1337/pull/191))
