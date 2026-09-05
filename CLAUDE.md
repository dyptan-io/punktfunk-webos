# CLAUDE.md

Native LG webOS TV client for [punktfunk](https://git.unom.io/unom/punktfunk) — low-latency
desktop/game streaming. Targets webOS 5.x+, built on `punktfunk-core` (pinned git dep).
One target: Linux (webOS armv7 cross target, or a plain Linux box).

## Commands

[go-task](https://taskfile.dev), `task --list`. Bare targets run natively (how CI runs);
`docker:*` wraps the cross-toolchain, which is what you want locally.

| Task | What it does |
| --- | --- |
| `task docker:check` / `docker:build` | `cargo check` / release build |
| `task docker:lint` / `fmt` | clippy / `cargo fmt` |
| `task docker:test` | run the unit tests (the only task that RUNS them; `lint` only type-checks) |
| `task docker:package` | build + `dist/*.ipk` |
| `task docker:deploy` | run the app in a container over VNC — UI work needs no TV |
| `task deploy TELEMETRY=auto` | install to the TV, stream logs here (`TELEMETRY_LEVEL=debug\|info\|warn\|error`) |

CI lints with `-D warnings` and clippy is load-bearing — run `docker:lint`, not just `check`.
CI also runs `task test` on the host target; the cross build cannot execute tests.

## Architecture

Layered, deps point inward, acyclic:

`core` (pure domain: `Settings`, `Screen`, events, `caps`) ← `ui` (presentation, `tiny_skia`
only, **no sdl2**) and `services` (portable I/O: store, discovery, mTLS library, art, wol)
← `session` (streaming on `punktfunk-core`, **no sdl2**) and `platform/webos` (the SDL2 and
hardware boundary — compositor, input, NDL video, audio, evdev) ← `app` (the `App` state
machine) ← `runtime` (the two top-level loops).

- **`ui`** is namespaced after Ratatui: `render`/`canvas`/`painter`/`layout`/`widgets`/`tiles`/
  `text`. One drawing idiom: `Widget`/`StatefulWidget`, plus `TileWidget` (a widget that names
  its own surface size) which `ui::rasterize` turns into a tile. Every tile is one. Names stay
  flat inside `ui` via `ui::prelude`.
- **`app`** splits per screen by concern: `state::<screen>` (events, transitions) and
  `view::<screen>` (geometry, draw list). `app::render` holds `tile` (which tile is which),
  `key` (what its pixels depend on — hashed, never stored), `ctx` (`RenderCtx`, threaded
  through every `prepare_*`) and `prepare_grid` (the O(visible) card passes, one method each). `App` is 19 fields and owns almost nothing directly: `nav` (screen,
  previous screen, one focus cursor per screen), `jobs` (every background receiver, one
  `drain_jobs`), `library` (the selected host's games, art and pins), `hosts` (the known list,
  reachability, rooted), `settings_ui` (the document plus its dropdown/override/slider),
  `screens::slots` (per-screen payloads) and `render::state` (grid, modal, hero, press, scroll
  windows, dirty flags). `screens::{list,confirm}` hold what a whole family of screens shares.
  Every field is `pub(crate)`; `runtime` writes through named setters.
- **`console`** hosts the shared gamepad shell (`pf-console-ui`) on its own GL context over the
  same SDL window: `gl` (Skia over framebuffer 0), `model` (host rows, library, the command
  bus). armv7-only — it is the one target the prebuilt Skia archive exists for, so on every
  other target the module is absent and `runtime::console_flow` is a stub.
- **`runtime`** alternates two phases: a menu and `stream`, on `StreamOutcome::ReturnToMenu`
  vs `Quit`. The menu is one of two flows, picked per entry by `Settings::console_ui`:
  `ui_flow` (this client's own screens) or `console_flow` (the shared shell). Both reload the
  document on entry, which is what keeps the flip from showing one side's edits stale.

Rendering is a `tiny_skia` software framebuffer composited by SDL, redrawn on change.
Add a screen: build on `ui::widgets::ListModal` (copy `app/{state,view}/hostmenu.rs`) and say
which family it joins in `app::screens` — `list` (rows) or `confirm` (two buttons); both tables
are exhaustive over `Screen`, so the compiler asks. ~22 `Screen` match sites otherwise. A
`ScreenView` trait to collapse them was evaluated and rejected (R6) — see "Explicitly not
doing" under Phase 4 in `docs/APP-REWORK-PLAN.md`.

## Invariants worth knowing before you edit

- **The grid is O(visible), not O(library)**, at every layer: card tiles build in a scroll
  window on a time budget and evict outside a hysteresis window; the drawn range, the focus
  map and pointer hit-testing are all computed arithmetically rather than by scanning. A new
  path that walks `self.games` per frame, per keypress or per pointer motion is a regression.
- `focus_window` must always contain the current focus, or `FocusMap::navigate` finds no
  origin and focus silently freezes.
- `GridState`/`CardIds` hand-write `Default`: a derived one starts the id counter at 0 and
  hands the first card a `TileId` a fixed tile already owns.
- **NDL is `dlopen`'d, never linked** — a `DT_NEEDED` breaks webOS 4 startup before `main`.
- **`settings.json` stores a schema wider than `Settings`.** It is `pf_client_core::trust::
  Settings`, shared with every other client, and this one models a subset — so
  `Persisted::shared_base` carries the rest and `shared::to_shared` writes over it. Rebuilding
  the object from `Settings` alone resets the gamepad shell's own rows on the next save.
- **A test behind the arm gate never runs**: `task test` builds the host target, and an armv7
  test binary cannot execute on a runner. Real logic goes in `services::store::shared`
  (ungated, tested); only glue goes behind the gate.
- Video decodes through NDL DirectMedia (opaque decode+present, two generations picked by
  `device::ndl_generation()`); audio is client-side Opus.
  `core::caps` publishes the resulting limits and has three readers that must agree.

**Before any platform, perf or A/V work, read `docs/NOTES.md`** — soft-float, glibc shims, the
SDL fork, NDL's audio-plane pacing requirement, and a long list of measured blind alleys.
Debug real behaviour on the TV early; code-only theories about this hardware are usually wrong.

## Code comments

Only where necessary. Concise WHY comments — non-obvious invariants, platform workarounds,
subtle constraints. Never restate the code.
