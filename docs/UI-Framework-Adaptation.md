# Adapting `ui/` to established framework design

`ui/` stays bespoke; what changed is the shape of its API, borrowed from libraries that
already settled these questions.

**Status: phases 1, 2 and 4 are done** (see "Sizing"). Phase 3 was conditional and is not
needed — see its section.

## Why not replace it

Evaluated egui and iced against this target. Both were rejected, for reasons that also
explain what is worth borrowing:

- **Focus.** Neither has spatial d-pad navigation. egui's focus is tab-order; iced's
  `focusable` operations don't even cover `button`. Every tile, row and sidebar entry here
  is a d-pad target. That work is ours regardless of framework — see `ui/focus.rs`.
- **Event loop and window.** iced is winit-bound; we are SDL2 end to end (the
  `webosbrew/SDL-webOS` fork's `dlopen`'d entry points, and an SDL window the NDL video
  plane sits behind). Driving iced headless via `iced_runtime::UserInterface` means
  skipping the framework and keeping the widget library.
- **Redraw model — the decisive one.** This app is *hybrid* software/GPU (NOTES.md:34):
  `tiny_skia` rasterizes each widget **once** into a tile, `Compositor::upload` hands it to
  a GPU texture, and every frame after is texture copies — position, scroll, focus-pop
  scale and fades are all `copy`/`copy_f`/`set_alpha_mod` parameters. Only widgets that
  actually changed are re-rasterized.

  iced has no equivalent of that per-`TileId` cache at any layer; every renderer produces a
  fresh scene per repaint. `iced_tiny_skia` would rasterize the whole scene into one buffer
  per frame and upload it whole — exactly the CPU-compositor architecture this project
  measured at ~25-45 ms/frame and abandoned (see `platform/webos/compositor.rs:1`).
  `iced_wgpu` is out separately: wgpu's GL backend wants GLES 3.0 and the SDL renderer here
  is `opengles2`.
- **Transparent overlay.** `runtime/stream.rs` paints stats/log overlays at ~2Hz onto a
  transparent window over the NDL plane. Frameworks that own and clear the surface fight this.

Net: they cost the SDL integration, the video-plane compositing and the redraw model, in
exchange for a layout engine. `ui/` is ~2.6k lines and target-fit. Keep it.

## What is actually wrong today

Measured, not asserted:

| Symptom | Count |
| --- | --- |
| Free fns still taking `painter: &mut Painter` | 31 (`rows` 10, `cards` 7, `modal` 5, `text` 5, `sidebar` 3, `listmodal` 1) |
| `Canvas` inherent methods (the intended destination) | 18 |
| `*_rect` fns shadowing a draw fn | 32 |
| Public names re-exported flat into `ui::` | 199 |

Three structural problems behind those numbers:

1. **`Canvas` is a half-finished migration.** `ui/canvas.rs`'s own doc comment gives the
   rationale (the painter/cache/fonts trio pushes every call past `too_many_arguments`).
   31 functions never followed.
2. **Geometry is computed twice.** Every `*_rect` fn must agree with the draw fn beside it.
   `list_modal_content_rect` and `render_list_modal` both recompute the header offset.
3. **The measure pass is hand-rolled.** The "probe trick" — measure against a zero-height
   rect at final width, then place — appears in `tiles.rs:307`, `listmodal.rs:18-31`,
   `modal.rs:110`, `text.rs:314`. Already half-abstracted into a closure, never named.

## Where to borrow from

**Ratatui**, for everything except the measure pass — its *API design*, which is orthogonal
to how pixels reach the panel. What transfers is the immediate-mode widget contract (draw
yourself into a given region), constraint layout, no reactive graph and no retained widget
tree. Six years of iteration (via tui-rs) means those decisions are settled.

What does **not** transfer is its `Buffer`: Ratatui has one flat cell grid, we have a keyed
tile cache backed by GPU textures. Borrow the traits and the layout solver, not the
rendering model.

**Druid/Masonry**, for `BoxConstraints` → `Size` only. Ratatui has no measure phase because
terminal cells make text measurement trivial. We wrap real glyphs and need one.

Copy the design, not the crates: Ratatui's `Layout` is `u16` terminal cells and pulls in
cassowary; ours is `i32` pixels over 1-D stacks.

## Phase 1 — finish `Canvas`, add the widget traits — **done**

Every free fn that took a `&mut Painter` is gone. The split landed on which state a draw
call actually needs:

- needs only the surface (card shadow, focus ring, switch, slider, rule, popup panel,
  modal card) → a `Painter` method, in the widget module it belongs to. A tile builder
  holding no fonts (`render_focus_ring_tile`) can still call it.
- needs the painter/cache/fonts trio → a `Canvas` method, in an `impl Canvas` block next
  to the widget's own geometry rather than in `canvas.rs`. `canvas.rs` is now the struct
  and its two constructors; the forwarding layer that duplicated every signature is gone.
- tile builders take `(&mut TextCache, &Fonts, …)` and open a `Canvas::tile` over their own
  painter, so no caller passes a raster and a `FontId` pair around any more.

Then the traits:

```rust
pub trait Widget {
    fn render(self, area: Rect, c: &mut Canvas) -> Result<()>;
}

pub trait StatefulWidget {
    type State;
    fn render(self, area: Rect, c: &mut Canvas, state: &mut Self::State) -> Result<()>;
}
```

`self` by value (as Ratatui does) is what makes builder-style config work without lifetime
friction. `StatefulWidget` is the home for what currently rides as loose arguments: a focus
index, a `ScrollWindow`. `draw_focus_rows(…, focused: usize, …)` becomes
`FocusRows::new(rows).render(area, c, &mut state)`.

Mechanical, no behavior change, and it finishes a decision already made in the tree.

Shipped widgets: `SidebarRow` (builder — `focused`/`selected`/`reserve_right`), `FocusRows`
+ `FocusRowsState` (`StatefulWidget`), `ListModal`, `ConfirmButtons`, `DropdownOverlay`.
`UNFOCUSED` names the "nothing here is focused" index the shell tiles render with — the
focused row/button is always its own tile, composited on top, so the widgets that draw a
list draw it unfocused and the single-item `Canvas` methods (`focus_row`, `confirm_button`,
`dropdown_option`) draw the focused copy.

## Phase 2 — `Layout` + `Constraint` — **done**

Highest value, and it compounds with `ui/focus.rs`. `ui/layout.rs`, ~130 lines:

```rust
pub enum Constraint { Length(u32), Min(u32), Percentage(u16), Fill(u16) }

Layout::vertical([Constraint::Length(HEADER_H), Constraint::Fill(1), Constraint::Length(FOOTER_H)])
    .gap(ROW_GAP)
    .split(area) -> Vec<Rect>
```

This collapses the 32 parallel `*_rect` fns: the split happens once and both painter and
caller read the same `Vec<Rect>`.

The payoff already paid for: **those rects feed `FocusMap`.** One geometry source for
layout, paint, hit-test and focus. `app::state::home::home_focus_map`'s hand-built rect
enumeration becomes a read of the layout result instead of a third derivation.

No cassowary. Layouts here are 1-D stacks: resolve the fixed slots, distribute the
remainder across `Fill` (and `Min`, whose baseline is a floor rather than a size) by
weight, and on overflow shrink from the most elastic slot to the least. `Max` was dropped
until a screen needs one — an unused variant is not a design, it is dead code.

Where it landed:

- `modal_card_rect` — "centred" is `[Fill(1), Percentage(frac), Fill(1)]` across and
  `[Min(min_top), Length(h), Min(0)]` down, which also expresses the keyboard-lifted
  card's minimum top inset instead of a `max()` on a hand-computed `y`.
- list modals, confirm dialogs, the settings card, About's body — each is one vertical
  stack read twice: `total_length()` gives the card its height (the probe pattern, now
  named), and `split()` gives the content rect. The two can no longer disagree.
- `confirm_button_rect` — `[Fill(1), Fill(1)]` with a `gap`.
- `view::sidebar::nav_rows` — the whole nav column in one split, the bottom-pinned
  Settings row expressed as a `Fill(1)` spacer before it. The painter, both hit tests and
  `home_focus_map` all read that same `Vec<Rect>`; `row_rect`/`settings_row_rect` and
  their index arithmetic are gone.

## Phase 3 — a real measure pass — **not needed**

Conditional on Phase 2 exposing text-height cases the probe trick was papering over. It
did not: every measured height in the tree is a modal header's wrapped subtitle, and
`modal_header_end_y(fonts, card, subtitle)` (now `Fonts`-taking rather than three loose
font arguments) feeds it into a `Constraint::Length` slot. The probe itself is
`Layout::total_length`, which is the naming the section below asked for. Kept here as the
design to reach for when a widget's own height first becomes text-dependent.

```rust
pub struct Limits { pub min: Size, pub max: Size }

pub trait Measure {
    fn measure(&self, limits: Limits, c: &Canvas) -> Size;
}
```

`&Canvas` (not `&mut`) reaches `fonts.raster` for metrics. `modal_header_end_y` becomes
`ModalHeader::measure`; `list_modal_card_rect`'s zero-height probe becomes an ordinary
measure call. Do Phase 2 with fixed heights first — this is what makes `Constraint::Min`
and `Fill` honest for wrapped text.

## Phase 4 — namespacing, and optionally `Style` — **done**

`ui::` is now `layout`, `widgets`, `style`, `text` (Ratatui's own division) plus `render`,
`painter`, `tiles`, `focus`, `scroll`, `animation`/`fade`; the flat `pub use *` glob is
gone. Only `Canvas`, `Painter`, `ModalScreen` and the two widget traits sit at the root.

Inside `ui` the names stay flat, through a crate-internal `ui::prelude`: a widget reaches
for the theme, the text cache, the layout solver and two neighbouring widgets in the same
function, and qualifying each of those buys nothing in the library that draws them all.
`MenuEvent` is no longer re-exported — callers take it from `core::event`, where it lives.

Optional: fold `theme.rs`'s bare consts into `Style { fg, bg, weight }` with `.patch()`
merging. Low value — the theme is small and the consts read fine.

## Deliberately not borrowed

- **egui's `Response`/`Sense`** — couples input into the draw path. The `app::state` /
  `app::view` split is deliberate and better for a d-pad UI.
- **Retained widget tree with `WidgetId`** (Masonry/Xilem) — the `TileId` texture cache
  already retains at the granularity that matters.
- **Reactive properties/bindings** (Slint) — needs a DSL and a compiler. Redraw-on-dirty is
  simpler and correct for a UI with no continuous animation.
- **Reworking `TileId` into hashed keys** — touches `platform/webos/compositor.rs` for no
  present pain. The 24-variant enum is not hurting anything.

## Sizing

Phases 1, 2, 4 were the plan worth running, and all three are in: the traits and the
layout solver are ~170 new lines, against ~300 lines of duplicated signatures, forwarding
methods and parallel `*_rect` arithmetic deleted. Phase 3 stayed conditional and did not
trigger. Each phase compiles and ships independently.
