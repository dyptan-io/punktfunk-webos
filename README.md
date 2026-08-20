<div align="center">

<img src="assets/logo/logo-sidebar.png" alt="punktfunk" width="300">

<br>
<br>

[![Build](https://github.com/dyptan-io/punktfunk-webos/actions/workflows/build.yml/badge.svg)](https://github.com/dyptan-io/punktfunk-webos/actions/workflows/build.yml)
[![Release](https://img.shields.io/github/v/release/dyptan-io/punktfunk-webos?color=6c5bf3&label=release)](https://github.com/dyptan-io/punktfunk-webos/releases/latest)
[![Downloads](https://img.shields.io/github/downloads/dyptan-io/punktfunk-webos/latest/total?color=a79ff8&label=downloads)](https://github.com/dyptan-io/punktfunk-webos/releases/latest)
[![License](https://img.shields.io/badge/license-MIT%2FApache--2.0-d2c9fb)](#license)

**Native LG webOS TV client for [punktfunk](https://git.unom.io/unom/punktfunk) — low-latency desktop & game streaming.**

</div>

---

Targets webOS 5.x+ (developed and verified live on an **LG CX, webOS 5.6**), packaged as a homebrew
`.ipk`. Built directly on the upstream `punktfunk-core` crate (a pinned git dependency — see
`Cargo.toml`).

Built on the [punktfunk](https://git.unom.io/unom/punktfunk) project by **Enrico Bühler
([unom](https://unom.io))** — all credit for the protocol, FEC/crypto core, and host implementation
belongs there. This repo is only the webOS-specific client: an SDL2 UI, NDL DirectMedia hardware
video decode, and webOS packaging.

## Features

- **Video** — up to 4K120 with HDR. H.264 or HEVC, decoded by the TV's hardware media pipeline
  (NDL DirectMedia), with a fallback decode path for webOS 3.5–4.x.
- **Bitrate** — Automatic mode adjusts to the network, or set a fixed rate from 10 to 200 Mbps.
  A per-host network speed test measures over the real data plane and applies a recommended rate.
- **Audio** — stereo, 5.1 or 7.1, decoded on the TV.
- **Library** — the host's game library, alphabetically sorted, launched with one press. Pinned
  games appear in the top row.
- **Per-game overrides** — any game can override the global resolution, frame rate, bitrate, codec,
  HDR, audio or controller settings.
- **Input** — Magic Remote pointer, gamepads, USB keyboard and mouse. Pointer capture for games,
  absolute pointing for the desktop. D-pad navigation, number-pad PIN/IP entry, and the Red button
  as a Back/disconnect substitute.
- **DualSense** — adaptive triggers, lightbar, player LEDs, touchpad, gyro and rumble over
  Bluetooth (see the note below).
- **Hosts** — LAN discovery (mDNS) or add a host by IP; PIN pairing with persisted trust, a live
  reachability dot per host, and per-host actions behind a ⋯ button: connect, pair, speed test,
  wake, edit address, forget.
- **Game mode (rooted TVs)** — optional setting that switches picture and sound to Game mode while
  streaming and restores the previous settings on exit.

> **Controller feedback needs newer webOS version.** Rumble, DualSense triggers, haptics and lightbar all
> rely on the kernel's `hid-playstation` driver, which LG ships only on latest webOS versions 24+.
> On webOS 5.x (e.g. the CX) the pad still works as an *input*, but no feedback reaches it.

<details>
<summary><b>Screenshots</b></summary>

<p align="center">
  <img src="assets/screenshots/home.jpg" width="32%" alt="Home / game library">
  <img src="assets/screenshots/host-menu.jpg" width="32%" alt="Host menu">
  <img src="assets/screenshots/settings.jpg" width="32%" alt="Settings">
</p>

</details>

## Installing

**Via Homebrew Channel** (recommended — installs/updates from the TV, no laptop needed):

1. Install [Homebrew Channel](https://www.webosbrew.org/) on the TV.
2. Homebrew Channel → Configuration → Add repository →
   `https://raw.githubusercontent.com/dyptan-io/punktfunk-webos/main/repo.json`
3. punktfunk now appears in the Homebrew Channel app list.

Only published [GitHub Releases](https://github.com/dyptan-io/punktfunk-webos/releases) appear this way —
dev/CI builds don't.

**Directly onto a TV** (Developer Mode required): `task deploy TV_HOST=root@<tv-ip>`.

## Development

Everything is a [go-task](https://taskfile.dev) target. Bare targets run natively and need a
Linux-aarch64 host with Rust installed (that's how CI runs).

| Task | What it does |
| --- | --- |
| `task build` | Compile a release binary |
| `task package` | Build + package `dist/*.ipk` |
| `task deploy TV_HOST=root@<tv-ip>` | Build, package, install, and launch on a real TV (via [ares-cli-rs](https://github.com/webosbrew/ares-cli-rs)); add `TELEMETRY=auto` to stream logs here |

For local dev prefix any target with `docker:` (`task docker:build`, `task docker:package`, …) —
same logic in an ephemeral `docker run`, so only Docker is required, no local Rust/NDK install (the
cross-toolchain image is Linux-aarch64-only; works on amd64 hosts via QEMU). `task --list` shows
the rest.

`deploy` settings such as `TV_HOST` and `SSH_KEY` can be set in a local `.env` — see
[`.env.example`](.env.example). Architecture and on-device gotchas live in
[`docs/NOTES.md`](docs/NOTES.md) and `CLAUDE.md`.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE), matching upstream punktfunk,
at your option.
