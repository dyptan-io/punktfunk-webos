# `ui/` hardening: purity, structure, compute

Follow-on to `docs/UI-Framework-Adaptation.md` (phases 1, 2, 4 landed). That work fixed the
*API shape*. This one fixes what the API is still made of: `ui/` names this app's screens,
its settings types and its brand, and the render path still hangs off `App`. Separately, the
hot paths carry costs that only show up on the target (armv7 / cortex-a73, softfp+NEON,
1080p panel, GLES2).

Three phases. **A** removes app knowledge from `ui/`. **B** restructures the render path and
proves purity by compiling `ui/` alone. **C** is compute. A and C are independent — C can be
done first if a frame-time problem is pressing.

The success test for A+B: `ui/` builds as its own crate with zero `crate::core` / `crate::app`
paths, and a `examples/gallery.rs` renders every widget with no `App` in the process.

---

## Phase A — evict app knowledge from `ui/`

### A1. `TileId` is an app enum living in the library

`ui/render.rs:100` — 24 variants named after this app's screens (`NoHost`, `PinBadge`,
`Hero`, `DisconnectDialog`), two of them carrying `core::screen::Screen`, two carrying
`String`. The library cannot be reused, and the `String` variants cost a heap clone plus a
SipHash of the string on every draw command every frame (see C2).

Replace with an opaque handle:

```rust
// ui/render.rs
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileId(pub u32);
```

The app owns the numbering. Give `app` a `TileKey` enum (exactly today's variants, moved
verbatim) plus an interner that maps a key to a dense `u32`:

```rust
// app/render/keys.rs
pub enum TileKey { Sidebar, Card(String), ScrollContent(Screen), … }

pub struct TileKeys { map: FxHashMap<TileKey, TileId>, next: u32 }
impl TileKeys { pub fn id(&mut self, k: TileKey) -> TileId }
```

`prepare_tiles` interns once when a tile is (re)built; `draw_list` emits the cached `u32`.
The compositor's `HashMap<TileId, Texture>` becomes `Vec<Option<Texture>>` indexed by id
(C2). Note the earlier doc's "reworking `TileId` into hashed keys — not worth it": that
judgement was about *present pain* only; here it is the same edit that removes the leak and
the per-frame hashing, so it pays twice.

Interner eviction: card tiles are already evicted by id; give `TileKeys` a `retire(&TileKey)`
so the slot is reused instead of growing per library refresh.

### A2. `ui/tiles.rs` imports `core::model`

`tiles.rs:4` pulls `Settings`, `GamepadType`, `LogLevelOverride`; `ModalFocusKey` and
`ScrollContentKey` enumerate this app's screens; `TileCache`'s fields are named
`nohost_tile`, `status_tile`, `pin_badge_tile`. The file's own comment already says the keys
"still name this app's screens, which is the next thing to make opaque".

Split the file three ways:

| Goes to | What |
| --- | --- |
| `ui/tiles.rs` (stays) | `Painter`-producing primitives with no app types: `render_focus_ring_tile`, `render_card_outline_tile`, `render_text_tile`, `render_wrapped_text_tile`, `CARD_TILE_PAD`/`FOCUS_RING_PAD`/`ROW_TILE_PAD` |
| `ui/cache.rs` (new) | A generic cache: `TileStore<K> { map: FxHashMap<TileId, (K, Painter)> }` with `get_or_build(id, key, || Painter)` returning "was rebuilt". Replaces all 16 hand-written `Option<(Key, Painter)>` slots and their staleness `if`s |
| `app/render/tiles.rs` | `ModalFocusKey`, `ScrollContentKey`, `render_stats_overlay_tile`, `render_log_overlay_tile`, `render_pin_badge_tile`, `confirm_dialog_*`, the spinner GIF |

`TileStore<K>` is the real win: today every slot repeats
`if cache.x.as_ref().map(|(k, _)| k) != Some(&key) { rebuild }` by hand, sixteen times, and
each one is a place to forget a key field.

### A3. App assets embedded in `ui/`

`ui/text.rs:10-37` `include_bytes!`s Geist, the Material subset and `logo-sidebar.png`;
`ui/tiles.rs:38` embeds the spinner GIF. A library should not carry an app's brand.

- `Fonts` already holds a `&dyn TextRaster`; add the font *bytes* to it as
  `FontSource { regular: &'static [u8], medium: …, semibold: …, icons: … }` supplied by
  `app`. `ui` keeps the `FontId`/`FontWeight` vocabulary and nothing else.
- `logo_pixmap()` and the spinner move to `app` (`app/assets.rs`). `SidebarRow` grows a
  `logo: Option<&Pixmap>` or the app draws it; the widget must not know the brand.

### A4. `ui/style.rs` is this app's theme, as bare consts

26 `ICON_*` constants (`ICON_GAMEPAD`, `ICON_TOUCH`, `ICON_POWER`) and a purple brand
palette. Turn it into a value the app supplies:

```rust
// ui/style.rs
#[derive(Clone, Copy)]
pub struct Theme {
    pub bg: Color, pub surface: Color, pub accent: Color, pub accent_bright: Color,
    pub text: Color, pub muted: Color, pub warning: Color, pub caution: Color,
    pub error: Color, pub ok: Color, pub scrim: Color, pub rule: Color,
}
impl Theme { pub const DARK_PURPLE: Theme = …; }   // today's values, as the default
```

`Canvas` gains `pub theme: &'a Theme`. Icons are already `&'static str` glyphs passed in by
callers (`FocusRow::action(icon, …)`), so the `ICON_*` block moves wholesale to
`app/view/icons.rs` — no widget signature changes. This is the "optionally `Style`" item the
previous doc deferred; it stops being optional once the palette is the last app fact in the
module. Skip `.patch()` merging — nothing here needs cascade.

### A5. Input types in a draw library

`ui/mod.rs:39` re-exports `core::event::MenuEvent` into the `ui` prelude for exactly one
function: `widgets/listmodal.rs:99 list_nav(focused, len, ev)`. Replace with the direction
the library already owns:

```rust
pub fn list_nav(focused: &mut usize, len: usize, dir: Option<Dir>) -> bool  // ui::focus::Dir
```

The app maps `MenuEvent → Dir` at its own boundary (it already has that mapping for
`FocusMap::navigate`). Drop `MenuEvent` from the prelude.

### A6. Policy constants that are not the library's

`ui/animation.rs` holds `HERO_PAN`, `HERO_FADE`, `HERO_MIN_SHOW`, `HERO_ART_GRACE`,
`FIRST_FRAME_WAIT` (a *stream* timeout), `HERO_LOADING_MAX`, `HERO_SCRIM_ALPHA`, plus
`hero_pan_dst`. The hero is a screen, and `FIRST_FRAME_WAIT` belongs to `session`. Keep
`ease`, `anim_frac`, `anim_frac_in`, `zoom_rect`, `pop_in_rect`, `FOCUS_POP`, `ModalFade` in
`ui`; move the rest to `app/view/hero.rs` and `session`.

Same audit for `widgets/sidebar.rs` (`SIDEBAR_W = 460` is this app's layout, not a widget
property → make it a field on `SidebarRow`'s caller, i.e. `app/view/sidebar.rs`) and the
`Settings`/`HostMenu` references in `scroll.rs` docs.

### A7. `ModalScreen` and `Fonts` in trait signatures

`ui/screen.rs` is fine conceptually but `card_rect(&self, w, h, fonts)` threads `Fonts`
through every implementor for measurement alone. Once `Canvas` carries the theme, make the
measurement argument a `Measure<'_>` view (`&dyn TextRaster` + `&Fonts`) so the trait does
not widen again the next time a screen needs the theme to size itself.

---

## Phase B — structure: get the renderer off `App`

`src/app/mod.rs` is ~3,500 lines / 165 KB. Inside it: `prepare_sidebar`, `prepare_grid`,
`prepare_hero`, `prepare_modal` (370 lines), `prepare_dropdown`, `prepare_scroll`,
`prepare_tiles`, `compose_modal` (340 lines), `compose_sidebar_focus`, `compose_grid`,
`draw_list`, `compose_hero` — the entire render path, reading `self` directly. `RenderInput`
(`app/render_input.rs`) is the half-finished fix; it currently carries five fields.

### B1. Finish `RenderInput`, one family per commit

Order, smallest blast radius first: sidebar → grid → hero → dropdown → scroll → modal. Each
step moves the fields that family reads out of `self` and into `RenderInput`, and changes
its `&self` methods to free functions taking `(&RenderInput, &mut TileStore, &mut Canvas)`.
When a family's methods no longer mention `self`, they move to `app/render/<family>.rs`.

The forcing function: once a `prepare_*` is a free function, it can be called from a test
harness with a synthetic `RenderInput` and its output `Painter` dumped to PNG. That is the
first thing in this tree that can regression-test rendering without a TV.

### B2. `Frame`: one owner for painter + tiles + draw list

Today `prepare_tiles` and `draw_list` are separate passes over the same state, each
re-deriving geometry (`grid_columns`, `SIDEBAR_W`, focus rects) and `draw_list` allocating a
fresh `Vec<DrawCmd>` per frame.

```rust
// ui/frame.rs
pub struct Frame<'a> {
    pub canvas: Canvas<'a, 'a>,
    pub tiles: &'a mut TileStore,
    cmds: DrawList,        // retained across frames, cleared not freed
    updated: Vec<TileId>,  // ditto
}
impl Frame<'_> {
    pub fn tex(&mut self, id: TileId, dst: Rect, alpha: u8);
    pub fn tex_cropped(&mut self, id: TileId, src: Rect, dst: Rect, alpha: u8);
    pub fn fill(&mut self, rect: Rect, color: Color);
}
```

`runtime` owns one `Frame` for the process. Both allocations disappear (C1) and the two
passes get a single place to share the geometry they both compute.

### B3. Extract `ui/` to a workspace crate

Once A is done: `crates/pf-ui/` with `Cargo.toml` depending only on `tiny_skia`, `anyhow`
and `image` (or nothing, if A3 lands). The main crate depends on it. This is not cosmetic —
it is the only mechanism that keeps `core::` from creeping back in, and it cuts rebuild time
for UI-only edits (today every `ui/` edit relinks the whole app under fat LTO).

If a workspace split is too disruptive, the cheap 80%: a `#![deny]`-style guard test that
greps `src/ui/**` for `crate::core` / `crate::app` and fails CI. Do that first regardless —
it locks in A as each piece lands.

### B4. Split what remains of `app/mod.rs`

After B1, what is left is the state machine plus hit-testing (`hover_focus_at`,
`handle_mouse_click` — 200 lines each, both giant `match screen` blocks). Both are the same
"which widget is at (x, y)" question the `FocusMap` already answers from layout rects. Fold
them into `FocusMap::hit(x, y) -> Option<K>` per screen, built by the same `Layout::split`
the painter reads. That is the payoff Phase 2 of the previous doc set up and only cashed for
the sidebar.

---

## Phase C — compute, for cortex-a73 / softfp+NEON / 1080p

Ordered by expected win. Measure before and after with the existing telemetry path
(`task deploy … TELEMETRY=auto`); add a `render_ms` / `upload_ms` counter to the stats
overlay first, otherwise this is all speculation.

### C1. Per-frame heap traffic in `draw_list`

`app/mod.rs:3365` allocates a `Vec<DrawCmd>` every frame, and every `TileId::Card(String)` /
`Hero(String)` command clones a `String` into it. At 60 Hz with a full grid that is dozens of
allocations and string copies per frame, all of it dead on arrival.

Fix: A1 (`TileId` is `Copy`) + B2 (retained `DrawList`). Zero allocations per frame in the
steady state. This is the single clearest win and it is the same edit as the purity fix.

### C2. Texture lookup hashes a `String` per command

`compositor.rs:28` — `HashMap<TileId, Texture>` with std's SipHash, keyed by an enum that
contains `String`. Every draw command pays a string hash. After A1, replace with
`Vec<Option<Texture>>` indexed by `TileId(u32)` — an array index instead of a hash. Use
`rustc-hash`/`FxHashMap` for the remaining keyed maps (`TileKeys`, `TextCache`), never SipHash
on this CPU.

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

`ui/text.rs:76-88`: `key()` does `text.to_string()` on every call — including cache *hits* —
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

| Step | Depends on | Why now |
| --- | --- | --- |
| B3's CI grep guard | — | Locks in every A step as it lands |
| A1 + C1 + C2 | guard | One edit; removes the biggest leak *and* the per-frame allocations |
| C3 | — | Independent, largest single compute win, ~40 lines |
| A2 (`TileStore<K>`) | A1 | Deletes 16 hand-written staleness checks |
| A4, A5, A6, A3 | A2 | Mechanical, one commit each |
| C4-C8 | — | Independent, measure first |
| B1 | A2 | The long one; one family per commit |
| B2, B4 | B1 | |
| B3 crate split | A complete | The proof |
| A7, C9 | everything | Only if still warranted |

Each row compiles and ships on its own. Verify on device (`task deploy … TELEMETRY=auto`),
not with `cargo test` — the frame-time claims above are only true on the panel.
