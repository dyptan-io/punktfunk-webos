# Menu rasterization performance: find and fix what blows the 16ms tick

**Original request:** analyse frame-overrun logs, identify why UI rendering exceeds the budget,
and apply reasonable measures (later: verify findings with agents first, apply only confirmed
ones; then remove all diagnostics and anything that didn't help).
**Branch:** fix/ndl-audio-ceiling-reskew - **Status:** ready for review, nothing committed

## Scope
Menu render path only (`prepare` stage on armv7). The streaming path was never touched. The
branch name is unrelated — this work landed on top of it.

## Done
- `src/ui/painter.rs` — `blit_src_over`: hand-written premultiplied source-over replacing
  `Pixmap::draw_pixmap`, which ran tiny-skia's pattern shader per pixel (~5.6 Mpx/s measured).
  `Painter::draw_pixmap`, `fill_shadow` and `fill_glow` all route through it. ~4x on every
  glyph, icon and cached shape blit. This was the single biggest win.
- `src/ui/widgets/modal.rs`, `src/app/render/{compose,prepare,tile}.rs` — modal card shadow
  moved to the GPU as a nine-slice: `modal_shadow_atlas()` (~122px, `ensure_static` into
  `tile::MODAL_SHADOW`), stretched by nine `DrawCmd::TexCropped` in `push_card_shadow`.
  `modal_card_glass` now uses `glass_face` (no baked shadow). Shell 260ms → ~55ms.
- `src/ui/text.rs` — `wrap_text` fast path: a line that already fits skips the per-word walk.
  About's 12,355-line wrap went 470ms → 49ms.
- `src/ui/painter.rs` — `blur_1d` replaced by `blur_line` over a caller-owned prefix buffer
  (was allocating per row *and* per column). Kept but never measured after the GPU move.
- Removed all measurement, per the final request: the per-family `prepare` timers I added, and
  the pre-existing `src/runtime/frame_timer.rs` (deleted; `mod` decl dropped, four `Stage`
  wrappers unwound in `ui_flow.rs`). `TileStore::len`/`bytes` and `TextCache::len` went with it
  — they existed only to feed the report.

Measured on device: Settings entry 350→130ms, Settings re-entry 289→90ms, Collections 277→87ms,
Experimental 261→91ms, About entry 558→190ms, About scroll bake 480→100ms, settings row 4.4→1ms.

## Left
1. `modal_focus` (focused-row tile) was 26-33ms and is predicted ~8-10ms, but **never confirmed
   on device** — the `fill_shadow`/`fill_glow` routing landed after the last deploy. Verify.
2. `modal_shell`'s remaining ~55-90ms is the card-sized anti-aliased `fill_rounded_rect`
   (~21 Mpx/s). `FrostPane` already carries a tint and a rounded mask, so the card face could
   plausibly move to the GPU the same way the shadow did.
3. `ModalShellKey::Collections` carries `collections_heading()`, which flips to a drag hint on
   grip-confirm and back on drop — two full shell re-rasters per card move. Fix is a separate
   heading tile (geometry is safe; card height depends only on row count), but it must also be
   composited into the `MODAL_PREV` close-snapshot or the leaving card loses its title mid-fade.
4. List modals (HostMenu, WakeSettings, Diagnostics, Experimental, CursorSettings) bake all rows
   into `tile::MODAL`, so any value change re-rasters the card. Confirmed but low payoff — entry
   cost is the same pixels either way; only in-place changes win. Five screens of plumbing.
5. `ABOUT_WINDOW_BUDGET` is still 80 lines, ~100ms per re-bake while scrolling. Shrinking it
   trades one stall for more frequent ones — measure before touching.
6. `blur_line` hoist is unmeasured; the user may want it dropped as "didn't help".

## Key decisions
- Measure, then cut. Two confident hypotheses were wrong (below), so nothing was optimized on
  reasoning alone. Re-measuring now requires re-adding timers — they were all removed.
- Nine-slice exactness depends on the corner slice covering `pad + radius + 3*(blur/2)`, plus a
  4px margin; a pixel of error tiles as a seam down every modal. Two tests guard it.
- `blit_src_over` blends in integers where tiny-skia uses floats, so a channel can land one step
  off. Tests assert `<= 1` against tiny-skia's own `draw_pixmap` as oracle, plus ten clipping
  placements. Do not loosen that tolerance.
- All 8 `modal_shell` callers render into the app's `tile::MODAL`; `main.rs`'s in-stream dialogs
  do not use it, which is why dropping the baked shadow there was safe.
- `FrameTimer` was also the loop's pacing clock, not just logging — replaced with a plain
  `Instant` so `TICK_BUDGET` behaviour is unchanged.

## Dead ends
- tiny-skia `simd` feature is off in `Cargo.toml`, but 0.12 has no `target_arch = "arm"` branch —
  a no-op on this target. Do not "fix" it expecting a gain.
- Icon downscale cache (128px glyph → 30px per draw): implemented, measured, no change. Reverted.
- "The AA rounded-rect fill is the modal shell's cost" — wrong; the shadow blit was 205ms of 260.
- Card art `draw_pixmap_rounded`: tiny-skia already downgrades Bilinear→Nearest at identity, and
  art size is not always card size. Not worth it.
- `CARD_BUILD_BUDGET` degrading to one card per frame is deliberate and documented (`grid.rs:23`).
- `hid_playstation_bound()` in the Settings rows hash: only runs on dirty frames, ~1ms/s.
- Off-thread tile rasterization: blocked by `!Send` SDL2_ttf fonts reaching into `TileWidget::size`,
  plus thread_local shadow/glow caches.

## Gotchas
- `task docker:test` intermittently dies on an apt/dpkg error during container setup — retry.
- Host `cargo check` cfg-gates out `app`/`platform`; only `task docker:check`/`docker:lint` prove
  it compiles. Formatting is `task fmt` (there is no `docker:fmt`).
- Current tree: `docker:lint` clean under `-D warnings`, 100 tests pass, `fmt` applied.
