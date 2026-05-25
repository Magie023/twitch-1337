# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0](https://github.com/Chronophylos/twitch-1337/releases/tag/v0.1.0) - 2026-05-24

### Added

- migrate remaining config knobs to dashboard settings (v3) ([#224](https://github.com/Chronophylos/twitch-1337/pull/224))
- *(ai)* scope memory + clarify speaker identity in !ai prompt ([#206](https://github.com/Chronophylos/twitch-1337/pull/206))
- hoist AI config from config.toml to dashboard settings ([#203](https://github.com/Chronophylos/twitch-1337/pull/203))
- *(döner)* Dönerpreis Command ([#190](https://github.com/Chronophylos/twitch-1337/pull/190))
- *(settings)* dashboard-managed runtime settings + owner tier ([#192](https://github.com/Chronophylos/twitch-1337/pull/192))
- *(web)* static viewer allowlist; drop Helix follower gate ([#189](https://github.com/Chronophylos/twitch-1337/pull/189))
- *(web)* follower-gated viewer tier for dashboard ([#188](https://github.com/Chronophylos/twitch-1337/pull/188))
- dönerpreisindex command + AI tool ([#178](https://github.com/Chronophylos/twitch-1337/pull/178)) ([#187](https://github.com/Chronophylos/twitch-1337/pull/187))
- *(ai)* prompt + memory hygiene overhaul ([#181](https://github.com/Chronophylos/twitch-1337/pull/181))
- *(web)* add dev-login Cargo feature for local dashboard testing ([#175](https://github.com/Chronophylos/twitch-1337/pull/175))
- *(web)* v2 dashboard — signed cookies, real bundles, sidebar, ?next=, form re-render ([#160](https://github.com/Chronophylos/twitch-1337/pull/160))
- *(web)* v1 dashboard — pings CRUD + AI memory editor ([#159](https://github.com/Chronophylos/twitch-1337/pull/159))
- feat(ai) improve AI emote timing ([#156](https://github.com/Chronophylos/twitch-1337/pull/156))
- *(ai)* forward Twitch user and session id to Langfuse ([#153](https://github.com/Chronophylos/twitch-1337/pull/153))
- *(ai)* replace fetch_url with read_url multimodal tool ([#152](https://github.com/Chronophylos/twitch-1337/pull/152))
- *(1337)* add !pb command and clarify PB stats wording ([#141](https://github.com/Chronophylos/twitch-1337/pull/141))
- *(ai)* identity-aware memory markers + wire web tools in v2 path ([#135](https://github.com/Chronophylos/twitch-1337/pull/135))
- *(up)* include all aircraft, not only commercial ([#134](https://github.com/Chronophylos/twitch-1337/pull/134))
- *(ai)* document web tools in \!ai system prompt ([#133](https://github.com/Chronophylos/twitch-1337/pull/133))
- *(ai)* per-channel chat history for ai_channel ([#131](https://github.com/Chronophylos/twitch-1337/pull/131))

### Fixed

- *(release-plz)* unblock CI workflow ([#225](https://github.com/Chronophylos/twitch-1337/pull/225))
- *(ai)* trust configured SearXNG host in SSRF guard ([#200](https://github.com/Chronophylos/twitch-1337/pull/200))
- *(ai)* log web_search/fetch_url failures + surface full eyre chain ([#139](https://github.com/Chronophylos/twitch-1337/pull/139))

### Other

- *(ping)* replace Arc<RwLock<PingManager>> with mpsc actor + delete sync atomic_save_ron ([#220](https://github.com/Chronophylos/twitch-1337/pull/220))
- More dashboard tweaks + dev iteration QoL ([#191](https://github.com/Chronophylos/twitch-1337/pull/191))
- *(ai)* bake 7TV emote glossary into binary ([#157](https://github.com/Chronophylos/twitch-1337/pull/157))
- [codex] improve pending flight tracking before ADS-B visibility ([#155](https://github.com/Chronophylos/twitch-1337/pull/155))
- fork twitch-irc, drop vendored copy ([#154](https://github.com/Chronophylos/twitch-1337/pull/154))
- security + reliability fixes (#21 #22 #39 #40 #100 #129 #130) ([#142](https://github.com/Chronophylos/twitch-1337/pull/142))
- *(ai)* drop legacy !ai path; v2 memory is the only path ([#140](https://github.com/Chronophylos/twitch-1337/pull/140))
- *(ai)* split recent chat into user message + emit mention table ([#138](https://github.com/Chronophylos/twitch-1337/pull/138))
- *(ai)* drop say tool, send agent final text as chat reply ([#137](https://github.com/Chronophylos/twitch-1337/pull/137))
- *(data)* refresh CSVs ([#132](https://github.com/Chronophylos/twitch-1337/pull/132))
- AI memory rework v2 — people-and-chat prose memory ([#102](https://github.com/Chronophylos/twitch-1337/pull/102)) ([#128](https://github.com/Chronophylos/twitch-1337/pull/128))
- AI memory rework v2 — spec + prompt files ([#118](https://github.com/Chronophylos/twitch-1337/pull/118))
- *(llm)* adjacent cleanups (model field, dead rebuild, empty content) ([#127](https://github.com/Chronophylos/twitch-1337/pull/127))
- *(ai+memory)* migrate tool loops to run_agent ([#126](https://github.com/Chronophylos/twitch-1337/pull/126))
- *(llm)* type module foundation for agent API ([#124](https://github.com/Chronophylos/twitch-1337/pull/124))
- extract llm crate ([#123](https://github.com/Chronophylos/twitch-1337/pull/123))
- convert repo into Cargo workspace ([#121](https://github.com/Chronophylos/twitch-1337/pull/121))
