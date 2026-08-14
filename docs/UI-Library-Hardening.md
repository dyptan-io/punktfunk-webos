# `ui/` hardening: purity, structure, compute

Follow-on to `docs/UI-Framework-Adaptation.md` (phases 1, 2, 4 landed). That work fixed the
*API shape*. This one fixed what the API was made of.

**Status: phases A and B are done** (three commits — see below). **Phase C, compute, is not
started.**

---

## Phase A — evict app knowledge from `ui/` — **done**

`ui/` no longer contains a single `crate::core`/`crate::app` path or a single
`include_bytes!`.

| Was | Now |
| --- | --- |
| `ui::render::TileId`, a 24-variant enum naming this app's screens, two variants carrying `core::screen::Screen`, two carrying a `String` | `TileId(u32)`, `Copy`. `app::render::tile` owns the numbering in three bands: singletons, one slot per spinner frame, and an interned band for grid cards (`CardIds`, keyed by pin id so a pin/unpin reorder still rebuilds nothing) |
| `ui::tiles::TileCache` — 16 `Option<(Key, Painter)>` fields, each with its own hand-written `key != stored` check, keys typed on `Settings`/`GamepadType`/`Screen` | `ui::cache::TileStore` — one map, one rule: fresh while its `u64` version matches. `app::render::key` holds the `#[derive(Hash)]` keys (`ModalFocusKey`, `ScrollContentKey`, `ModalShellKey`, now together) |
| Geist, the Material subset, the sidebar logo and the spinner GIF `include_bytes!`d into `ui::text`/`ui::tiles` | `crate::assets`; `text_sdl` loads them. `ui` keeps the `FontId`/`FontWeight` roles |
| 13 bare palette consts + 26 `ICON_*` | `style::Theme`, installed once by `app::view::icons::install_style`, with a neutral `Theme::DEFAULT` so a `ui`-only harness draws without setup. `style::Icons` covers the four glyphs the library's own chrome needs; the other 26 are the app's vocabulary |
| `list_nav(…, ev: MenuEvent)`, `MenuEvent` in the `ui` prelude | `list_nav(…, dir: Option<focus::Dir>)`; `menu::nav_dir` maps the app's events once, at the app's boundary |
| `HERO_PAN`/`HERO_FADE`/`HERO_MIN_SHOW`/`HERO_ART_GRACE`/`HERO_LOADING_MAX`/`LAUNCH_FADE`/`FIRST_FRAME_WAIT` in `ui::animation` | `app::hero`. `ui::animation` keeps curves and rect math |
| `Compositor::upload` matching `TileId::Sidebar` to decide opacity | an `opaque: bool` the caller declares |

Deviation worth knowing about: the theme is **installed once into a `OnceLock`**, not carried
as a `Canvas` field as this document originally proposed. A `Canvas` field means a sixth
argument on every tile builder — the exact parameter-passing the previous document's phase 1
removed. A theme is process-wide; `OnceLock` says so.

Two smaller wins fell out of the same edit: draw commands no longer clone a `String` or hash
one (C1/C2 below are partly paid), and the sidebar strip rebuilds into its own pixmap
(`TileStore::ensure_in_place`) instead of reallocating a full-height surface per host-list
change.

Not done, and deliberately: `ModalScreen`'s `Fonts` argument (A7) — worth revisiting only
when a screen needs the theme to size itself, which none does.

## Phase B — structure — **done, differently**

`src/app/mod.rs` was 3422 lines holding the state machine, both halves of the render path and
every hit test. It is 1062. The rest:

- `app::render::prepare` (975) — rasterization, one `prepare_*` per family
- `app::render::compose` (639) — the draw list, pure texture-copy bookkeeping
- `app::render::geometry` (275) — what both halves measure against, so they cannot disagree
- `app::pointer` (546) — hover and click, against the same `app::view` rects the painter uses

**B1 was not done as written.** It had these become free functions over a completed
`RenderInput`. `RenderInput` is five fields after several families; finishing it means a
~40-field mirror of `App`, and its stated payoff was a PNG-diff test harness this project
does not use — it verifies on device. The file split is the part that was worth having, and
`RenderInput` stays for the families already on it.

**B4's hit-test premise turned out to be wrong.** The claim was that `hover_focus_at`/
`handle_mouse_click` re-derive geometry a third time. They do not — the previous document's
phase 2 already pointed them at the same `view::*` rect helpers the painter reads. What was
left was long `match screen` dispatch, which is now its own module rather than folded into
`FocusMap`.

**B3, the crate split, is still open** and is the only real proof of purity. The grep guard
this document proposed as its cheap stand-in was written and then removed at the author's
request; the check it performed (`crate::core`/`crate::app`/`include_bytes!` under `src/ui`)
now passes, and `crates/pf-ui` is the way to keep it passing.

**B2, `ui::Frame`, is not done** — it is mostly a compute change (a retained `DrawList`
buffer) and belongs with C1 below.

---

## Phase C — compute, for cortex-a73 / softfp+NEON / 1080p

Ordered by expected win. Measure before and after with the existing telemetry path
(`task deploy … TELEMETRY=auto`); add a `render_ms` / `upload_ms` counter to the stats
overlay first, otherwise this is all speculation.

### C1. Per-frame heap traffic in `draw_list` — half paid

The `String` clone per card/hero draw command is gone with A1. What remains:
`app::render::compose::draw_list` still allocates a fresh `Vec<DrawCmd>` every frame and
drops it at the end of the same frame.

Fix: B2's `ui::Frame`, owning a `DrawList` that is `clear()`ed rather than freed, plus a
`updated: Vec<TileId>` on the same footing. Two allocations per frame to zero.

### C2. Texture lookup still hashes, now cheaply — finish it

The string hash per draw command is gone with A1; `compositor.rs`'s
`HashMap<TileId, Texture>` is now keyed by a `u32`, but still through std's SipHash.

`app::render::tile`'s numbering is dense and small by construction, so replace it with
`Vec<Option<Texture>>` indexed by `TileId(u32)` — an array index instead of a hash. Same for
`ui::cache::TileStore`'s own map. `ui::text::TextCache` keeps a real hash map; give it
`rustc-hash`/`FxHashMap`, never SipHash on this CPU.

### C3. The un-premultiply pass on every upload — the big one

`compositor.rs:88-108`: for every non-opaque tile, per pixel, three integer divides by a
runtime `a`. The sidebar alone is 460×1080 ≈ 497k pixels ≈ 1.5M divides; a full-screen modal
tile is 1920×1080 ≈ 6M divides. ARM has no vectorised integer divide, so NEON cannot help
and each is a multi-cycle scalar `udiv`.

Two fixes, in order of preference:

1. **Delete the pass.** SDL2 supports premultiplied blending via
   `SDL_ComposeCustomBlendMode(SDL_BLENDFACTOR_ONE, SDL_BLENDFACTOR_ONE_MINUS_SRC_ALPHA, …)`.
   `tiny_skia` already produces premultiplied pixels. Set that blend mode on non-opaque
   textures and `tex.update(None, pm.data(), pitch)` directly — no staging buffer, no
   conversion, no second 4 MB copy. Verify on-device that the GLES2 renderer accepts the
   custom blend mode (it is supported in the GL/GLES2 backends; check the SDL-webOS fork's
   version) and that `alpha_mod` still composes correctly for fades. Fall back to (2) if not.
2. **256-entry reciprocal LUT.** `static RECIP: [u32; 256]` where `RECIP[a] = (255 << 16) / a`;
   the per-pixel op becomes `(c * RECIP[a] + 0x8000) >> 16` — a multiply and a shift.
   ~4× on the pass, and it vectorises.

Either way, keep the existing `a == 0 || a == 255` fast path — most tile pixels are one or
the other.

### C4. `TextCache` allocates a `String` per lookup and hashes twice

`ui/text.rs`: `key()` does `text.to_string()` on every call — including cache *hits* —
then `contains_key` + `insert(key.clone())` + `get`, i.e. up to three hashes and two
allocations for one glyph run. `Canvas::text` is called for every label on every tile
rebuild.

```rust
// key by hash, no allocation on the hit path
fn key(font: FontId, text: &str, color: Color) -> u64  // FxHasher over (text, color, font)
entries: FxHashMap<u64, Pixmap>
```

Collision risk is acceptable for a glyph cache of this size; if not, keep the tuple key and
use `raw_entry_mut` (or `hashbrown`'s `entry_ref`) so the `String` is built only on a miss.
Either way one hash, one probe.

Also: `render_stats_overlay_tile` and `render_log_overlay_tile` each do `TextCache::new()`
internally and throw it away — the log overlay rebuilds at 2 Hz, re-rasterizing every glyph
run from freetype each time. Thread the shared cache in (they take `&Fonts` already; give
them `&mut TextCache`). The About screen's `text_uncached` path is correct as-is and should
stay.

### C5. Blur allocates two buffers per call and blurs alpha needlessly

`ui/painter.rs:304` — `vec![0u8; w*h*4]` + `vec![0u8; w*h]` per `blur_rect`, then four
channel passes. Move both buffers onto `Painter` as reusable scratch (`Vec` kept, `clear()`ed).
Check whether the alpha channel pass is observable for the frosted panel; if the region is
opaque, three passes instead of four is a free 25%.

`fill_shadow`/`fill_glow` already cache by `ShadowKey`/`GlowKey` — good, leave them. Confirm
the caches are process-lifetime and not per-`Painter`; if per-`Painter`, every tile rebuild
re-blurs its shadow.

### C6. `spinner_frame_at` re-sums the frame table every frame

`ui/tiles.rs:87` sums every frame's `delay` and then walks the list, on every call, while the
spinner is on screen. Precompute `(cumulative_ns: Vec<u128>, total_ns: u128)` in the same
`OnceLock` and binary-search. Small, but it runs at 60 Hz on the loading path.

### C7. `Layout::split` returns a `Vec<Rect>`

Every hit-test and every paint call allocates. Layouts here are ≤ 12 slots. Either return a
`SmallVec<[Rect; 8]>`, or add `split_into(&self, area, out: &mut [Rect])` and let callers own
an array. `nav_rows` is called from the painter, two hit tests and the focus map — four
allocations per interaction today.

### C8. Text measurement allocates `Vec<String>`

`wrap_text` (`text.rs:242`) returns `Vec<String>`; it is called for measurement (the probe
pattern, `total_length()`) *and* again for drawing, so every wrapped subtitle is wrapped
twice and allocates a `String` per line both times. Return `Vec<Range<usize>>` into the input
and have the draw path slice. `confirm_dialog_card`'s doc already notes the double-wrap it
tries to dodge — this removes the reason to dodge it.

### C9. `Painter::new` allocates a `Pixmap` per tile rebuild

Full-screen modal tiles are ~8 MB each. Pool them: `PainterPool` keyed by `(w, h)` handing
out cleared pixmaps. Only worth it if C1-C4 land and rebuild churn still shows in the
profile — measure first.

### C10. Build and loop settings

- `[profile.release]` already has `lto = "fat"` + `codegen-units = 1`, and
  `.cargo/config.toml` sets `+neon,+vfp3,-soft-float` and `target-cpu=cortex-a73`. Nothing to
  change there.
- Add `panic = "abort"` to `profile.release` — smaller binary, no unwind tables, and the app
  has no `catch_unwind` recovery path.
- Confirm the menu loop is `SDL_WaitEventTimeout`-driven when nothing is animating rather
  than spinning at 60 Hz on a static screen. `tick_animations` returning `false` should mean
  the loop sleeps until the next event. This is the cheapest power/thermal win on a TV that
  sits on the host list for minutes.
- Audit for `f64` in the render path (`as f64`, `powf`, `sqrt`) — softfp gives real VFP for
  `f32`; `f64` on cortex-a73 is fine but wider, and `tiny_skia` is `f32` throughout. Keep the
  boundary `f32`.

---

## Sequencing

A and B are in. What is left, in order:

| Step | Why now |
| --- | --- |
| A stats-overlay `render_ms` / `upload_ms` counter | Everything below is a guess without it |
| C3 | Independent, largest single win, ~40 lines |
| C1 + B2 (`ui::Frame`) | One edit; kills the last per-frame allocations |
| C2 | Falls out of the dense id space A1 established |
| C4-C8 | Independent of each other; measure first |
| B3 (`crates/pf-ui`) | The proof that A stays true, and it cuts UI-only rebuild time under fat LTO |
| C9, A7 | Only if still warranted |

Each row compiles and ships on its own. Verify on device (`task deploy … TELEMETRY=auto`),
not with `cargo test` — the frame-time claims above are only true on the panel.
