# Grid card focus: zoom, glow, title strip

Handover for the branch's card-focus work. Covers what the focused game card does now,
where each piece lives, and the traps hit on the way.

## What changed for the user

- The title strip is gone from unfocused cards. It belongs to the focused card only, and
  wipes up over its bottom edge as focus lands.
- A game with no cover gets a generated poster — the tinted card with its title set into
  it, wrapped and centred — instead of a single initial letter. Cards only; hero art is
  untouched.
- Focus zoom is bigger (2.8% -> 4.5%) and runs on smoothstep over 160ms.
- The glow is tighter against the card edge and brighter there, and follows the card's
  corner radius.
- The outline over the focused card's art follows the card's rounded shape rather than
  being a hard rectangle, and is composited last, over the title strip too, so the card
  ends on one unbroken lit line for the glow to end against.
- Cover art is rounded to `CARD_RADIUS`, like the placeholder poster and the glow.

## How it is built

Nothing per-frame rasterizes. Every effect is a GPU composite over tiles built once:

| Tile | Built in | Contents |
| --- | --- | --- |
| card (`CardIds` band) | `ui::tiles::render_card_tile` | shadow + art, always unfocused |
| `tile::RING` | `render_focus_ring_tile` | one glow at the current card size, shared |
| `tile::CARD_TITLE` | `render_card_title_tile` | the focused card's frosted title strip |
| `tile::CARD_OUTLINE` | `render_card_outline_tile` | the lit `CARD_RADIUS`-rounded edge, one card size, shared |

`app::render::prepare` decides which are stale; `app::render::compose` places them.

### Title strip

The strip needs real pixels under it or the frost has nothing to blur, so
`render_card_title_tile` redraws the card's art into a strip-tall canvas translated up by
everything above the strip — the strip's slice of the cover lands at y 0 and the rest
falls off the canvas, where tiny-skia clips it. Then `Canvas::poster_title_strip` frosts
and labels it.

The frost is rounded at its bottom corners only, so it sits inside the card's rounded art
instead of jutting out square. `Canvas::poster_title_strip` gets that from one rounded
rect grown upward by its own radius: the top pair of corners lands above the strip, off
the tile canvas, where tiny-skia clips it. The art underneath was already rounded to the
same shape, but the frost blur smears it back out into those corners, so the tile is
trimmed to the shape afterwards with `Painter::clip_to_rounded_rect`, which applies an
alpha mask over the whole tile. It has to be a mask: tiny-skia has no clip stack, and a
path fill only writes pixels *inside* the path, so it can never cut the corners away.

One slot, versioned on `(pin_id, card_w, card_h, art.is_some())`: only one card is ever
focused, `pin_id` because two games can share a title, `art.is_some()` so a late-arriving
cover rebuilds the strip that was baked over the placeholder.

Compose draws it as a **wipe, not a translation**: the bottom `shown` rows of the tile go
to the bottom `shown` rows of the card. The baked art therefore stays registered with the
art beneath it and only the frosted band's top edge moves. Sliding the whole tile (the
first attempt) dragged its fragment of cover art upward, which read as the card shifting
under the glass.

### Riding the card's transform

The card tile is scaled by the focus zoom and, on first appearance, the pop-in. Anything
composited on top has to fold in the same scale about the **card's** centre, not its own —
`ui::animation::scale_about(rect, pivot, scale)`, with `zoom_scale`/`pop_in_scale`
exposing the two factors. `zoom_rect`/`pop_in_rect` are the same call with the rect as its
own pivot.

### Glow

`render_glow_shape` blurs the shape, restores the sharp silhouette over the result (`max`
of the two coverages, so the lit body ends on the shape's own outline and corner curvature
rather than on the rounder, larger figure a blur leaves), then reshapes the alpha ramp
through a 256-entry LUT: `min(a * GLOW_EDGE_GAIN, 1)^GLOW_FALLOFF_GAMMA`. A plain blur is
half-strength exactly at the edge and spread evenly outward, which reads as haze; the gain
saturates the dense end into a collar on the edge and the gamma shortens the tail. Both
are monotonic, so it stays one continuous falloff.

Stacking a second, tighter blur pass was tried first and looked two-tone — the sum has a
visible knee where the inner pass ends. Don't go back to it.

The LUT matters: the input is the blurred u8 alpha, so there are only 256 possible
outputs, against ~80k `powf` calls over the padded buffer on softfloat armv7.

### Tuning dials

| Constant | Where | Effect |
| --- | --- | --- |
| `CARD_GROWTH` | `app/mod.rs` | zoom amount |
| `CARD_FOCUS_POP` | `ui/animation.rs` | duration of the whole focus animation — zoom, glow and title wipe share it, and it is **also when `focus_anim` is cleared** |
| `FOCUS_GLOW_BLUR` | `ui/widgets/cards.rs` | how far the halo reaches |
| `FOCUS_RING_PAD` | `ui/tiles.rs` | tile margin; must clear the blur or the halo clips |
| `GLOW_EDGE_GAIN` / `GLOW_FALLOFF_GAMMA` | `ui/painter.rs` | collar brightness / tail length; 1.0 and 1.0 give the plain blur back |

## Traps

**Animations that never start.** `focus_anim` was armed only by
`app::state::home::ensure_grid_visible`, i.e. the D-pad path. Moving focus with the Magic
Remote pointer (or a mouse in the X11/VNC preview) changed `home_focus` without arming any
clock, so every effect rendered at its end state and looked instant. It is now armed in
`App::set_home_focus`, which covers every pointer path. A third path that sets
`home_focus` directly would reintroduce the bug — route it through `set_home_focus`.

Diagnosing this: a debug line in `run_ui_flow` logging `focus_anim.elapsed()` per
presented frame showed *zero* lines while navigating, which is what identified the unarmed
clock rather than frame skipping. The loop itself was fine — `App::tick_animations`
returns `animating` while `focus_anim` is set and `run_ui_flow` renders every 16ms on it.
The probe has been removed; re-add it the same way if timing is ever suspect again.

**Rounded cover art costs a path fill.** `Painter::draw_pixmap_rounded` is a
pattern-shaded rounded-rect fill, not a blit. Fine here (tile build only, once per card),
not fine anywhere per-frame.

**Glow reshaping lives in the shared primitive.** `render_glow_shape` is generic but
currently carries the card-focus profile in two constants. `fill_glow` has exactly one
caller (`Painter::focus_ring`); a second glow user should take the profile as a parameter
rather than inherit this look.

## Verifying

`task docker:lint` for the build. Everything else is visual — deploy and move focus
around the grid, with and without covers:

```
task deploy TELEMETRY=auto TELEMETRY_LEVEL=debug
```

Check both input paths (D-pad and pointer), a card whose cover has not landed yet, and a
freshly loaded library where the card pop-in and the focus pop run at once.
