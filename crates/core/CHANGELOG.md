# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/Chronophylos/twitch-1337/releases/tag/twitch-1337-core-v0.1.0) - 2026-05-24

### Added

- migrate remaining config knobs to dashboard settings (v3) ([#224](https://github.com/Chronophylos/twitch-1337/pull/224))
- add !haiku command ([#212](https://github.com/Chronophylos/twitch-1337/pull/212))
- OpenRouter service tiers (flex / default / priority) ([#209](https://github.com/Chronophylos/twitch-1337/pull/209))
- *(ai)* scope memory + clarify speaker identity in !ai prompt ([#206](https://github.com/Chronophylos/twitch-1337/pull/206))
- hoist AI config from config.toml to dashboard settings ([#203](https://github.com/Chronophylos/twitch-1337/pull/203))
- *(döner)* Dönerpreis Command ([#190](https://github.com/Chronophylos/twitch-1337/pull/190))
- *(settings)* dashboard-managed runtime settings + owner tier ([#192](https://github.com/Chronophylos/twitch-1337/pull/192))
- *(web)* static viewer allowlist; drop Helix follower gate ([#189](https://github.com/Chronophylos/twitch-1337/pull/189))
- *(web)* follower-gated viewer tier for dashboard ([#188](https://github.com/Chronophylos/twitch-1337/pull/188))
- dönerpreisindex command + AI tool ([#178](https://github.com/Chronophylos/twitch-1337/pull/178)) ([#187](https://github.com/Chronophylos/twitch-1337/pull/187))
- *(ai)* prompt + memory hygiene overhaul ([#181](https://github.com/Chronophylos/twitch-1337/pull/181))
- *(web)* editable memory frontmatter + ping member CRUD ([#171](https://github.com/Chronophylos/twitch-1337/pull/171))
- *(web)* v2 dashboard — signed cookies, real bundles, sidebar, ?next=, form re-render ([#160](https://github.com/Chronophylos/twitch-1337/pull/160))
- *(web)* v1 dashboard — pings CRUD + AI memory editor ([#159](https://github.com/Chronophylos/twitch-1337/pull/159))

### Fixed

- *(release-plz)* unblock CI workflow ([#225](https://github.com/Chronophylos/twitch-1337/pull/225))
- *(tests)* poll flights.ron until expired flight clears ([#222](https://github.com/Chronophylos/twitch-1337/pull/222))
- *(haiku)* throttle failed LLM attempts ([#216](https://github.com/Chronophylos/twitch-1337/pull/216))
- *(commands)* align triggers, suspension, and live cooldowns ([#214](https://github.com/Chronophylos/twitch-1337/pull/214))
- *(ai)* trust configured SearXNG host in SSRF guard ([#200](https://github.com/Chronophylos/twitch-1337/pull/200))
- fix für falsche Dönerpreise bei !dpi ([#197](https://github.com/Chronophylos/twitch-1337/pull/197))
- *(aviation)* clear pending callsign hexes and improve callsign matching logic ([#186](https://github.com/Chronophylos/twitch-1337/pull/186))
- *(aviation)* keep pre-ADS-B tracked flights with stale schedules ([#183](https://github.com/Chronophylos/twitch-1337/pull/183))
- *(web)* dashboard polish pack — Dark Reader, breadcrumbs, version stamp, log cleanup ([#180](https://github.com/Chronophylos/twitch-1337/pull/180))
- *(web)* use moderated_channels for OAuth mod check ([#170](https://github.com/Chronophylos/twitch-1337/pull/170))

### Other

- *(data)* refresh CSVs ([#223](https://github.com/Chronophylos/twitch-1337/pull/223))
- *(ping)* replace Arc<RwLock<PingManager>> with mpsc actor + delete sync atomic_save_ron ([#220](https://github.com/Chronophylos/twitch-1337/pull/220))
- split aviation/tracker.rs and drop memory.rs validate_slug ([#219](https://github.com/Chronophylos/twitch-1337/pull/219))
- *(tests)* drop spawn lock, make FakeTransport instance-scoped ([#217](https://github.com/Chronophylos/twitch-1337/pull/217))
- *(chat)* route all chat sends through sanitizing ChatSender ([#215](https://github.com/Chronophylos/twitch-1337/pull/215))
- *(data)* refresh CSVs ([#207](https://github.com/Chronophylos/twitch-1337/pull/207))
- Döner Rechner mit richtigen Rechenoperationen ([#201](https://github.com/Chronophylos/twitch-1337/pull/201))
- Ignore null Preise bei Läden ([#199](https://github.com/Chronophylos/twitch-1337/pull/199))
- More dashboard tweaks + dev iteration QoL ([#191](https://github.com/Chronophylos/twitch-1337/pull/191))
