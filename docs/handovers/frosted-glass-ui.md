# GPU frosted-glass UI: modals, grid cards, and one theme's worth of shared tokens

**Original request:** "how computationally expensive would it be if all modal dialogs have a semi
transparent background with a frosted effect (the same theme tint)? Are there ways to optimize the
algorithm so it is cheap to dynamically render?" Then, over two sessions: implement it, extend it
to the game card slide-up menu, unify colours/fades across menus, add an Experimental toggle, and
make that toggle actually reach the cost.
**Branch:** frosted-glass-ui-followups - **Status:** ready for review; **nothing verified on device**

## Scope
Frosted glass for the menu UI, done on the GPU. In-stream chrome (`ui/tiles/overlay.rs`, the
disconnect dialog) is deliberately excluded: NDL video sits on a hardware plane *below* the SDL
surface, so it is not in the framebuffer and cannot be blurred at all.

## Done
- `DrawCmd::Frost(Box<FrostPane>)` in `ui/render.rs`; implemented in `platform/webos/compositor.rs`.
  No readback: layers under the first pane render into an offscreen `backdrop`, that is minified
  down `FROST_STEPS` (divisors 4/8/16/32 - bilinear minification *is* the blur), blitted to the
  screen in their place, then magnified back per pane and cut to a mask. Boxed because a pane is
  far larger than any other variant's payload and `DrawCmd` sits in a per-frame `Vec` the stream
  path fills without ever frosting.
- Mask is applied with a composed blend (`mask_blend`, `dst.a *= src.a`) so the scratch stays in
  straight alpha and composites with plain `BlendMode::Blend`. Both it and render-target support
  are probed once via the shared `probe_blend_mode`; logs `frosted modals: <bool>` and falls back
  to flat rounded fills.
- Card title strip + submenu (`ui/tiles/card.rs`, `ui/tiles/cardmenu.rs`) moved off the baked CPU
  blur onto the same path. Deleted `Painter::blur_rect`, `clip_to_rounded_rect`, `BLUR_SCRATCH`,
  and the `art` fields on both tiles: a focus move no longer rescales a cover or blurs it.
- One material everywhere: `Theme::panel_glass` + `Theme::glass_edge` via `Painter::glass_panel`
  (modal card, dropdown popup, toast, quit dialog). `Painter::glass_face` is the same surface
  without the drop shadow, for a tile sized to the panel exactly.
- One fade curve: `painter::fade_step`, shared by `draw_pixmap_faded` (horizontal) and the
  compose path's vertical ramp. Added fades to `dropdown_option` and `modal_header`'s title.
- The menu's **quit dialog is frosted**. It shares `ConfirmDialog`/`ConfirmDialogShellTile` with
  the in-stream disconnect prompt, which is why it was opaque; both now take a `glass` flag, and
  `ui_flow` passes `settings.frosted` while `stream` passes `false`. `FROST_BLUR` moved to
  `ui::render` so `app` and `runtime` name the same figure.
- Card submenu stays open behind the per-game settings modal; closed in `state/settings.rs`'s
  `MenuEvent::Back` for `SettingsScope::Game`.
- Settings row **"Theme"** (`SettingsRow::Theme`, global list only, between Cursor and
  Experimental) governs **modals and cards**, applied live through `ui::style` +
  `App::restyle()`. It replaced the Experimental "Frosted theme" toggle.
  - `core::model::ThemeChoice { Default, DefaultGlossy }` replaces `Settings::frosted`.
    Serialized as `"default"` / `"default_glossy"`; `ThemeChoice::glossy()` is what every
    former `settings.frosted` reader calls now. **Default is `Default`** — the old `frosted`
    defaulted to on, so this flips the shipped look.
  - `Deserialize` is hand-written, through `serde_json::Value`: an unknown name (or any other
    JSON shape) yields `Default`. A derived impl errors, and since `Settings` loads with one
    `from_value` that error would discard the *whole* document over a cosmetic field.
  - It is an ordinary dropdown row — no custom input logic. `dropdown_options` /
    `dropdown_option_count` / `dropdown_current_index` / `apply_dropdown_choice` each carry a
    `Theme` arm, and the row is listed in `state/settings.rs`'s dropdown-opening `Confirm` arm
    so a click/OK raises the shared popup instead of cycling in place. `restyle()` is called
    at both apply sites (the popup's pick and Left/Right).

## Material (what "frosted" means here, beyond the blur)
- **Grain.** A pure minification chain is perfectly smooth, which reads as *defocus*, not as
  frost — real frosting scatters off a rough surface. `grain_tile` builds one 256px square of
  neutral LCG noise once and tiles it 1:1 into each pane's scratch, inside the same
  `with_texture_canvas` pass as the mask and **before** it (the mask writes alpha only, so
  anything after it paints outside the rounded shape). `GRAIN_ALPHA` is the one knob.
  256px, not 64: it trades texture bytes (256 KB) against blit count (~32 for a full-screen
  pane rather than ~500).
- **Tint** is `Theme::panel_glass`: `0x1c1c1cda` neutral / `0x1c1532da` brand.
- **`FROST_BLUR = 64`**, with `FROST_STEPS = [4, 2, 2, 2, 2]` (divisors 4/8/16/32/64). The
  fifth link costs one copy of a 34x17 texture and buys a visibly wider, smoother spread —
  successive 2x averages converge on a gaussian where one deep jump does not.

## Performance: glossy vs default
Static analysis (code + arithmetic at a 1080p surface), **not measured on device**.

The CPU delta is ~nil: with the default theme `card_glass()` simply returns an opaque fill, so
tiny-skia rasterizes the same tiles either way. The whole footprint is GPU memory, GPU fill and
render-target binds. And the menu only draws when `dirty || animating` (`runtime/ui_flow.rs`),
so **at rest both themes cost nothing** — everything below is per *drawn* frame.

Memory, allocated on the first focused card and held for the session: backdrop 7.9 MiB, five
chain levels 0.66 MiB, grain 0.25 MiB, up to four scratches + four masks ~5.5 MiB — **~14 MiB**,
against zero for the default theme. The surface is the display mode (`runtime/stream.rs`), so a
set that ever hands back a 4K mode triples that.

Fill, against a ~2.1 Mpx baseline menu frame:

| Frame | Extra fill | vs baseline |
| --- | --- | --- |
| Focused card strip only (300x50) | ~2.3 Mpx | +110% |
| Modal over a card (900x700 + strip) | ~4.8 Mpx | +230% |

**The fixed cost dominates**: capture + blit-back is ~2.07 Mpx and the chain ~0.17 Mpx whatever
the pane is, so a 300x50 title strip cost almost as much as a whole modal. Blur width and the
grain are cheap by comparison (the grain is ~13% of a modal frame's extra, one pane-area of
fill). Per pane the work is ~4x its area: magnify, grain, mask, blit.

Latency: 8 to 12 render-target binds per frosted frame (1 backdrop + 5 chain + 2 per pane)
against none. On a tile-based GPU each is a resolve/restore, and that — not fill — is the term
to watch. `present()` is a blocking vsync swap, so this eats headroom silently until it doesn't.

### The backdrop cache (the fix that came out of the above)
`Chain::backdrop_key` hashes the under-pane commands, the clear colour, the target size and a
`Compositor::tile_epoch` (bumped on every tile upload/drop/clear, so a tile whose *pixels*
changed under an unchanged draw list still invalidates). A frosted frame whose key matches the
last one **skips the capture and all five minifications**, leaving one full-screen blit as the
whole backdrop cost. That is the common case by a wide margin — the grid under an animating
card strip or a fading-in modal is identical frame to frame — and it removes ~6 of the binds
and the under-layer's redraw from those frames. The key is cleared *before* the capture, so an
error mid-capture cannot leave a key naming half a frame.

## The style epoch (what invalidates tiles on toggle)
`ui::style::STYLE_EPOCH` is an `AtomicU64` bumped by `set_frosted` when the value changes.
`cache::version` hashes it ahead of every key, and `cache::STATIC` became `cache::static_version()`,
which *is* the epoch. `ensure_static` lost its `contains(id)` short-circuit and goes through
`ensure` like everything else. One toggle therefore stales every baked tile at once.

This replaced a hand-maintained `GLASS_TILES` list plus a `render.restyle` bool and a
`drain_restyle` pass, all now deleted. The list had already gone stale (it listed the dropdown
popup, which paints a fixed `DROPDOWN_FILL`, and omitted the toast, which does read `glass_fill`),
and it could never have covered the grid's card tiles, which are keyed on content that does not
change when the theme does.

That last point is the whole reason the epoch exists: **`push_card_frost` is now gated on the
setting**, and `card_glass()` returns `glass_fill()` rather than a fixed `panel_glass`, so the
strip goes opaque in the same moment the blur goes away. Those two must move together, because a
translucent strip over cover art needs the blur to carry a title. Without per-tile invalidation
that the card tiles actually see, the gate would leave stale translucent strips over bare art.

## Left
- Deploy and look at it. Specifically: the `frosted modals:` probe line on a CX; whether
  `FROST_BLUR = 32` reads right on the card strip (~50px tall, so it is nearly a flat wash by
  design); whether the bottom submenu row's shadow doubles with the card's own `CARD_SHADOW`.
- **The unfrosted card strip is a look nobody has seen.** It is the same fallback a renderer
  without render targets gets, but that is reasoning, not observation.
- **One toggle press now re-rasters every visible tile** under the grid's existing time budget.
  Bounded, but worth watching for a hitch.
- `FADE_STEP` went 4 to 8, halving the edge fade's command count (~46 to ~24 per frame). Steps
  land ~32 alpha apart over 92px. Drop it back to 4 if banding shows on the panel.
- **Measure the grid's hot path on device** — the numbers above are arithmetic. Deploy with
  `task deploy TELEMETRY=auto` and compare the same navigation glossy vs default with the frame
  timer at TRACE.
- Confirm the backdrop cache actually hits during a card-strip wipe and a modal fade. If it
  misses, something under the pane is being re-emitted with a moving rect or re-uploaded every
  frame, and the hash will say so.
- The remaining fixed cost is the full-screen blit-back. Removing it means capturing only the
  pane's own region rather than the screen, which is the same change that would fix stacked
  panes (see "Known limits").
- `GRAIN_ALPHA` and the tint are unverified on a real panel — grain that reads as film grain
  rather than as a surface is the failure mode.

## Key decisions
- **Blur width is declared, never inferred from pane size.** `FrostPane.blur` maps through
  `level_for`. Sizing it to the pane made the card's title strip and the submenu land on
  different chain levels, so the frost visibly changed the instant the panel opened.
- **One capture per frame, at the first pane.** Same-depth panes are fine (modal cross-fade);
  a pane stacked on another's surface blurs the wrong layer. This is why the dropdown popup takes
  the glass fill but no frost. Documented on `FrostPane`. See "Known limits" below: this is no
  longer purely theoretical.
- **The scroll-edge fade dissolves the content, it is not a surface.** Painting a tint band over
  the outgoing row buries the blur (flat rectangle); re-laying blur and tint as two ramped layers
  under-tints the middle of the band. Dissolving the content adds no surface at all, so there is
  nothing to seam. This is why the reviewer-suggested "restore a stretched ramp overlay" is not
  an option: the step count is the only lever, hence `FADE_STEP`.
- Card glass at a thinner alpha than the modals' (`CARD_GLASS_ALPHA`) - user reverted it.
- A focus-pop zoom on the card menu's selection band - user reverted it; the slide is the effect.

## Known limits (accepted, not bugs to hunt)
- **Stacked panes blur the pre-capture layer.** The quit dialog over Home, and a modal over a
  focused card (normal flow, since the card submenu stays up behind per-game Settings), both sit
  at a later depth than the card's own pane. Their blur samples the frame before the card strip
  and before the scrim. Fixing it means capturing per pane.
- **The flat fallback fringes its corners.** `frost_mask` uploads premultiplied tiny-skia pixels
  and the fallback path draws them with straight-alpha `Blend` plus a colour mod, so antialiased
  corner texels darken. Only affects renderers with no render targets, which is a device none of
  us has.
- **`FROST_SLOTS = 4`, evicted FIFO not LRU.** Busiest frame is three shapes (a modal card over
  a card's title strip or its taller submenu panel), leaving one spare for a cross-fade. A fourth
  live shape would thrash.
- **The chain is allocated for the session on the first focused card**, not on the first modal.
  ~14 MB at 1080p all told. Only a stream that never returns to the menu avoids it.

## Dead ends
- `glReadPixels` / `SDL_RenderReadPixels` off the default framebuffer: pipeline stall, ~5-20ms.
- CPU box blur over the modal region: ~4M read-modify-writes on a soft-float core.
- A single large minification instead of stepped ones: bilinear only averages 2x2, so it
  point-samples and aliases.

## Gotchas
- `cargo check` on the mac host compiles almost nothing. Use `task -s docker:check` /
  `docker:lint`; CI lints with `-D warnings` and clippy is load-bearing (doc backticks, cast
  lossless, unused type params, arg count all bit during these sessions).
- Two crashes came from `Ord::clamp` with min > max when a card scrolls off-viewport. Both rect
  mappings now share `texel_rect`, which clamps the origin to `w - 1`. Any new rect mapping in
  the frost path needs the same care.
- `Corners::Bottom` and `ui::widgets::bottom_rounded` both grow a rect *upward* past the radius.
  Drawing such a shape second inside a shared buffer double-fills what is above it: that was the
  card menu's doubled selection darkening.
- A tile sized exactly to its panel throws away `card_shadow`'s entire 14px blur. Use
  `Painter::glass_face`. The toast was paying that per raster, and its width tracks the message,
  so it missed the shape cache every time.
