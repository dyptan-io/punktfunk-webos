# UI/menu rendering pipeline — review and plan

Static review of `runtime/ui_flow.rs`, `app/render/{prepare,compose,tile,key}.rs`,
`ui/{cache,text,painter,animation}.rs`, `platform/webos/compositor.rs`.
Nothing here is measured — no frame-time instrumentation exists. Phase 0 fixes that
first so the rest is driven by numbers, not by reading.

## Verdict

The architecture is sound and unusually well documented: version-hashed tile cache,
GPU composition of pre-rasterized tiles, per-tile alpha/colour mod caching, texture
pooling, premultiplied blend probed once with a table-driven CPU fallback. The gaps
are not in the design; they are per-frame work that scales with library size, a few
unbounded caches, and no way to see any of it at runtime.

---

## Gaps

### G1. No frame-time instrumentation (blocking everything else)
`ui_flow.rs:483` paces off `tick_start`. If a tick's work exceeds `TICK_BUDGET`
(16ms) the sleep is skipped and the loop silently drops to 30fps. Nothing logs it.
Any "the menu feels laggy" report is currently unfalsifiable.

### G2. Per-frame work is O(library), not O(visible)
- `compose_grid` (`compose.rs:303`) iterates `0..count` over the whole library every
  frame purely to cull. Rows are uniform height; first/last visible index is arithmetic.
- `prepare_grid` (`prepare.rs:167`, `prepare.rs:202`) scans `0..count` twice more —
  once to evict, once to collect build candidates — plus a `Vec` alloc, a
  `sort_by_key`, and an `id.to_string()` per candidate.

At 60fps with a few hundred games this is tens of thousands of iterations per second
for work whose answer changes only when `grid_scroll` moves.

### G3. Every family prepares every frame, on every screen
`prepare_tiles` (`prepare.rs:1002`) unconditionally calls sidebar, grid, hero, status,
modal, dropdown, scroll. With Settings open over Home, the entire grid path still
runs. Cheap per call, but it is the whole of G2 paid while nothing on screen can change.

### G4. Cache keys allocate every frame
`ModalShellKey::Wake { name: w.name.clone() }`, `Pairing { status: clone }`,
`ModalFocusKey::MenuRow(_, row.label.clone(), _)`, `SpeedTestButton(_, String)` —
built, hashed, dropped, once per frame. The keys are only ever hashed immediately;
they never need to own anything.

### G5. Unbounded caches with no accounting
- `ui::text::TextCache` has no eviction by design, on the stated assumption that
  distinct strings are bounded by app content. Speed-test status lines, pairing
  status, and reachability strings are dynamic and violate it slowly. Nothing reports
  the entry count.
- `TileStore` is only pruned via the grid's explicit eviction window. No global budget.

### G6. Overlay tiles rebuild their text cache from scratch
`render_stats_overlay_tile` / `render_log_overlay_tile` each construct a throwaway
`TextCache::new()` per build (2Hz). Correct — it is what stops the log wall poisoning
the shared cache — but it also means zero reuse between consecutive rebuilds of
near-identical text, plus a `HashMap` alloc per build.

### G7. `ease_scroll` is frame-rate coupled
`ui/animation.rs:22` steps 35% of remaining distance *per tick*. Every other animation
is `Instant`-based. Scroll speed is therefore a function of achieved frame rate — the
one place where a slow frame changes motion, not just smoothness. It also makes
`TICK_BUDGET` unadjustable without a second edit.

### G8. `upload_raw` silently no-ops on an existing tile
`compositor.rs:216`. Callers must `drop_tile` first; `HERO` does. A future caller that
forgets gets a stale texture and no error.

### G9. `Compositor::present` leaks canvas blend state
A `DrawCmd::Fill` sets `BlendMode::Blend` on the canvas and never restores it. Today
the caller sets `None` before each `present`, so it is latent, not live.

---

## Refactoring / restructuring (Rust conventions)

### R1. Kill the `use crate::app::*;` globs
Five files (`render/prepare.rs`, `render/compose.rs`, `render/geometry.rs`,
`pointer.rs`, `press.rs`). They exist only because `App`'s fields and the module's
tuning constants are all reachable that way. Replace with explicit `use` lists.

### R2. Decompose the `App` god-struct
138 `pub(crate)` items in `app/mod.rs` (1131 lines). No encapsulation, which is what
forces the globs (R1) and the split `impl App` blocks across `state/`, `view/`,
`render/`. Group into owned sub-structs — `GridState`, `ModalState`, `SpinnerState`,
`ScrollState` — each with its own `impl` and private fields. Secondary win: disjoint
field borrows, removing the copy-layout-by-value workarounds in `prepare_grid`.

### R3. Introduce a render context, delete the arg lists
Nine `#[allow(clippy::too_many_arguments)]` in the tree; `prepare_tiles` takes 8 args,
`prepare_grid` 9, `run_ui_flow` 12. Introduce:

```rust
struct RenderCtx<'a> {
    tiles: &'a mut TileStore,
    text: &'a mut TextCache,
    fonts: &'a Fonts<'a>,
    screen: Size,
    updated: Vec<TileId>,
}
```

Every `prepare_*` becomes `fn(&mut self, ctx: &mut RenderCtx<'_>) -> Result<()>`.

### R4. Borrowed cache keys
Give `ModalShellKey` / `ModalFocusKey` / `ScrollContentKey` a lifetime and hold `&str`.
Fixes G4 mechanically and makes the "these are hashed, never stored" contract explicit
in the type.

### R5. One drawing idiom
`ui/tiles.rs`'s free `render_*_tile` functions and the `Widget`/`StatefulWidget` traits
in `ui/widget.rs` are two parallel ways to say the same thing. Pick the trait; port the
free functions to it, or vice versa. Currently a reader must know both.

### R6. Screen dispatch as a trait (optional, larger)
CLAUDE.md documents eight `Screen` match sites for adding a screen. A
`trait ScreenView { fn prepare(&mut self, ..); fn compose(&self, ..); fn handle(&mut self, ..) }`
with `App` holding the active view collapses them to one. Real cost: the state machine's
cross-screen transitions get less obvious. Propose, do not assume.

### R7. Lints and edition
Add `clippy::needless_pass_by_value` and `clippy::redundant_clone` to `[lints.clippy]`
— both target exactly the classes above. Separately, this crate is edition 2021 while
`punktfunk-core` is 2024; the migration is mechanical and independent of everything here.

---

## Plan

### Phase 0 — measure (do this alone, ship it, read the logs)
1. Record `tick_start.elapsed()` at `ui_flow.rs:483`; log at WARN when it exceeds
   `TICK_BUDGET`, with the tile count rebuilt that frame and the current screen.
2. Per-frame breakdown at DEBUG: `prepare_tiles` / uploads / `draw_list` / `present`.
3. Add `TextCache::len()` and `TileStore::len()` to the stats overlay.

Deploy with `task deploy TELEMETRY=auto`, scroll a full library, open every modal.
**Everything below is contingent on what this shows.** Do not start Phase 1 blind.

### Phase 1 — per-frame cost (G2, G3)
4. Compute the visible index range arithmetically; iterate only it in `compose_grid`.
5. Same in `prepare_grid`'s evict and build scans — evict from a tracked resident set,
   not from a full rescan.
6. Gate `prepare_grid`/`prepare_sidebar` on the grid actually being visible.
7. Drop the per-candidate `to_string()`; the `sort_by_key` can become a stable partition.

### Phase 2 — allocation and cache hygiene (G4, G5, G6)
8. R4 (borrowed keys) — this *is* the G4 fix.
9. Bound `TextCache` with a simple LRU or a generation sweep; expose the count.
10. Hoist the overlays' throwaway `TextCache` into a long-lived one owned by the
    overlay, separate from the menu's.

### Phase 3 — correctness and robustness (G7, G8, G9)
11. Make `ease_scroll` time-based (take `dt`), decoupling motion from frame rate and
    unblocking any future `TICK_BUDGET` change.
12. Make `upload_raw` replace rather than no-op, or return an explicit error.
13. Restore canvas blend state in `present`, or set it once per frame.

### Phase 4 — structure (R1, R2, R3, R5, R7)
14. R3 (`RenderCtx`) first — it touches the most signatures and makes R2 tractable.
15. R2 (decompose `App`), one sub-struct per commit, `task docker:check` between each.
16. R1 falls out of R2 nearly for free.
17. R5, then R7's lints (expect a cleanup pass as they fire).

### Phase 5 — optional
18. R6 (screen trait), if Phase 4 makes the shape obvious. Decide then, not now.

Phases 1-3 are independent of Phase 4 and can ship first. Phase 4 is a pure refactor:
no behaviour change, verified on device per the project's usual practice.

### Explicitly not planned
- Lowering `TICK_BUDGET`. Already at the 60Hz panel's ceiling; `eglSwapBuffers` gates
  presentation regardless. Reviewed separately — the fix for judder is Phase 1, not a
  shorter tick.
- Damage/dirty-rect rendering. Double-buffered presentation means a partial-scene
  redraw would need the previous frame's contents preserved; not worth it against a
  draw list of tens of quads.

---

## Status

Landed (static review only — nothing measured on device yet):

- **Phase 0** — `runtime/frame_timer.rs`: per-tick `FrameTimer`, stages `prepare`/`upload`/
  `compose`/`present`, DEBUG breakdown per frame and a WARN past `TICK_BUDGET` carrying the
  screen, tiles rebuilt, `TileStore::len()` and `TextCache::len()`. The two counts go into that
  report rather than the in-stream stats overlay: the overlay only exists while streaming, where
  neither cache is alive.
- **Phase 1** — `view::home::visible_cards` computes the on-screen index range arithmetically
  (property-tested against a brute-force cull); `compose_grid` iterates it. `prepare_grid`'s
  windows are index ranges, its eviction runs off the resident set (`CardIds`) rather than a
  library scan, candidates are two lists instead of a sort, and no id is copied for a candidate
  that may not be built. The whole grid pass is skipped while a modal owns the screen and
  nothing has invalidated a card behind it.
- **Phase 2** — keys borrow (`ModalShellKey<'_>`/`ModalFocusKey<'_>`); the shell key is no
  longer stored, only its hash (`App::modal_shell_version`), which is what makes borrowing
  possible. `TextCache` is bounded by a second-chance sweep and reports its length. The debug
  overlays now share one long-lived cache per loop instead of building a throwaway per rebuild.
- **Phase 3** — `ease_scroll` takes `dt`; `upload_raw` replaces instead of no-opping (the hero's
  `drop_tile` dance is gone); `Compositor::present` restores the canvas blend mode it found.
- **Phase 4** — R3 (`app::render::ctx::RenderCtx`), R1 (globs gone from all five files, and the
  names now come from their real homes), R7 (both lints on, tree clean under `-D warnings`).

- **Phase 4 (rest)** — **R5**: one drawing idiom. `ui::TileWidget` (a `Widget` that also names its
  own surface size) plus `ui::rasterize`, and every tile in the tree is now one — the ~28 free
  `render_*_tile` functions that each re-spelled measure/allocate/wrap-in-a-canvas are gone, and
  `ui/tiles.rs` is a `ui/tiles/` directory split by family (`card`, `cardmenu`, `text`, `overlay`,
  `confirm`). Three tiles that built a throwaway `TextCache` per rebuild (notification, confirm
  shell) now draw through a long-lived one, finishing what Phase 2 started for the overlays.
- **Phase 4 (rest)** — **R2**: `App` decomposed. `app::grid::GridState` (tiles, pop clocks, eased
  scroll, dirty flags, plus `GridLayout`/`GridCard` and the card tuning constants),
  `app::modal::ModalState` (the nine `modal_*` fields), `app::spinner::GridReveal` (the reveal flag
  and its two clocks, with the reveal decision as a method). `app/mod.rs` 1131 → 936 lines with the
  three new files carrying real behaviour rather than just fields.

  `ScrollState` (the plan's fourth sub-struct) was **not** done, deliberately: `scroll`,
  `settings_scroll` and `content_window` are already well-typed values, and the only thing to
  encapsulate is a two-line stash used at one call site each — wrapping them would add a hop at
  ~30 `self.scroll.*` sites and buy nothing.

## Large-library hardening (not in the original plan)

Phase 1 windowed the *render* path but left three interaction paths scaling with library size:

- `App::hit_test_grid_card` scanned every card in the library on every pointer motion.
  `view::home::card_at_point` inverts the layout instead: two divisions (one per section band) for
  a candidate row, then the ≤5 rects in that row tested against the same
  `unscrolled_card_rect` the painter used — so gutters and heading bands still match nothing.
- `home_focus_map` emitted one `FocusMap` item per card on every d-pad press, then scanned the
  whole list several times to answer a question about the immediate neighbours.
  `view::home::focus_window` restricts it to the on-screen band widened by two rows, unioned with
  the current focus (which must be in the map or `navigate` finds no origin).
- `replay_reorder_pop` put one `String` per game into `card_pop` on every pin toggle — a map
  `tick_animations` scans every frame, and which eviction never reaches for a card it holds no tile
  for. It now re-arms only resident cards; a non-resident one has no pop on screen to replay and
  gets its clock when `prepare_grid` builds it.

Plus two build-rate changes:

- `CARD_BUILD_BUDGET` is a 6ms *time* budget (checked after each card, so one always gets built)
  with a `CARD_BUILD_BURST` ceiling of 8, replacing the flat one-card-per-frame. One card per frame
  meant a 5x4 viewport took twenty frames to fill on every device; on armv7 softfloat this degrades
  back to exactly that one card, and on anything faster the window fills at the rate the hardware
  allows.
- `TileStore::bytes()` feeds the frame-overrun report, so "the grid's cards are held to the scroll
  window" is checkable rather than argued (G5's remaining half).

## R6 — recommendation: do not do it

Measured rather than assumed, as the plan asked. `Screen` has 20 variants across 46
match-on-screen sites and ~200 `Screen::` mentions. A `ScreenView` trait would collapse the eight
add-a-screen sites CLAUDE.md names, but:

- Most of those matches are not "dispatch to this screen's behaviour" — they are cross-cutting
  policy (is this a confirm dialog, does it scroll, what scope is it editing). A trait does not
  remove them; it scatters one readable table across 20 impls.
- Cross-screen transitions hand state between screens (Settings → About stashes the scroll window,
  Wake → Home carries a host, CardMenu → GameSettings carries a scratch copy). Today those are
  plain field moves on `App`; behind a trait object each needs an explicit handoff protocol.
- The eight dispatch sites are the mechanical cost of *adding* a screen, and the compiler finds all
  of them at once. That exhaustiveness is worth more than the collapse — trading it for a
  `Box<dyn ScreenView>` is a downgrade.

Not done: on-device verification of any of it. Everything above compiles clean for armv7 under
`task docker:lint` (`-D warnings`) and `task docker:build`, and nothing has run on a TV.
