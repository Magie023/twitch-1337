# twitch-1337

[![CI](https://github.com/Chronophylos/twitch-1337/actions/workflows/ci.yml/badge.svg)](https://github.com/Chronophylos/twitch-1337/actions/workflows/ci.yml)
[![Docker](https://github.com/Chronophylos/twitch-1337/actions/workflows/docker.yml/badge.svg)](https://github.com/Chronophylos/twitch-1337/actions/workflows/docker.yml)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](#license)
[![Rust Edition 2024](https://img.shields.io/badge/rust-2024-orange.svg)](https://doc.rust-lang.org/edition-guide/rust-2024/)

A personal Twitch chat bot for a single channel, with a mind of its own.
Written in Rust, it ships as a ~6 MB static binary.

The name is from its oldest job: every day at **13:37** Berlin time, chat races
to type `1337`, and the bot keeps score.

## What it does

- **The 13:37 ritual.** For one minute a day it watches for `1337`/`DANKIES` and
  records who typed it fastest, down to the millisecond. `!lb` shows the
  all-time record.
- **Aviation toys.** What's flying overhead right now (`!up`), a random but
  plausible flight plan (`!fl`), and live tracking of one aircraft from takeoff
  to landing (`!track`).
- **An AI that remembers.** `!ai` talks to an OpenAI-compatible or local model.
  It keeps a character sheet on the regulars and a sense of the channel's lore,
  then rewrites those memories each night from the day's transcript.
- **Chat plumbing.** Community ping groups people can join (`!p`), scheduled
  messages, and feedback (`!fb`).

## How it's built

One persistent IRC connection feeds a broadcast channel. Each feature is an
independent handler task that reads from it and acts on its own, so one can lag
or fail without taking the others down.

Runtime state like the leaderboard, pings, tracked flights, and AI memory lives
under a data directory as RON and markdown files. Most of what you'd tune day to
day (permissions, schedules, AI behavior) is set from a web dashboard backed by
`settings.ron` and applies without a restart. Only secrets and a few bootstrap
fields stay in `config.toml`.

The release build is a static musl binary with no dynamic dependencies, so the
Docker image can start from `scratch`.

## Running it

```bash
cp config.toml.example config.toml   # add your Twitch credentials
cargo run
```

`config.toml.example` documents the bootstrap fields. Everything else is set
from the dashboard once the bot is running.

Docker, via the Justfile:

```bash
just build     # build the image
just deploy    # build, push, and restart on the remote host
just logs      # tail remote logs
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this project by you, as defined in the Apache-2.0 license,
shall be dual licensed as above, without any additional terms or conditions.
