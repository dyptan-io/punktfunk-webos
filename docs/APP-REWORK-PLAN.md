# `src/app` rework plan

Status: phases 0-7 landed; see the per-phase notes below. Scope: `src/app` (10.9k LOC, 56 files), plus the seams it forces open in
`src/core/screen.rs`, `src/ui/screen.rs` and `src/runtime`.

## 1. What is actually wrong

The module is well documented and already half-refactored (`grid`, `modal`, `hero` are grouped;
`view::*` is free functions; `ui::ModalScreen` exists). The remaining problems are structural,
not cosmetic.

### P1 — `App` is a 75-field god object

`app/mod.rs:104-299`. One struct owns the screen enum, every screen's cursor, every background
channel, all art/library caches, the settings document, the render clocks and the dirty flags.
`App::new` is 118 lines of field initialization. Consequences:

- No borrow splitting. Any `&mut self` method conflicts with any `&self` geometry query, which
  is why `prepare_modal` destructures `RenderCtx` by hand and why `host_menu_actions()` is built
  eagerly at `render/prepare.rs:786` just to dodge a borrow.
- Screen state outlives its screen. `pin_digits`, `add_host`, `speed_test_name`,
  `send_logs_focused` are live on Home. Nothing resets them but the `open_*` functions, by
  convention (14 of them).
- Every field is a candidate dependency for every function, so staleness reasoning is manual
  (see the hand-written key structs in `render/key.rs`).

### P2 — nine per-screen focus cursors, and four match tables over them

`settings_focused`, `host_menu_focused`, `menu_focused`, `wake_settings_focused`,
`diagnostics_focused`, `experimental_focused`, `cursor_settings_focused`, `send_logs_focused`,
`speed_test_focused`. They exist so a nested menu keeps its place across a round trip — a real
requirement, solved by nine fields plus `list_modal_focused`, `list_modal_focused_mut`,
`confirm_focused`, `set_confirm_focused` (`render/geometry.rs:249-360`), each a `match
self.screen` that must name the same field as the other three.

### P3 — the screen match is smeared across 26 sites

`grep 'match self.screen'` → 8 in `render/prepare.rs`, 12 in `render/geometry.rs`, 3 in
`pointer.rs`, 2 in `press.rs`, 1 in `mod.rs`. CLAUDE.md claims "~8 `Screen` match sites"; that
was true when written. Adding a screen now means finding all 26 — the compiler finds the
exhaustive ones, but the `_ =>` arms in `address_copy`, `dropdown_options_len`,
`prepare_dropdown` and `scroll_stride_for` silently absorb a new variant into the wrong
behaviour.

It already has. **`Screen::WakeSettings` is missing from `hover_focus_at`'s list-modal arm**
(`pointer.rs:158`) and falls into the `_ => false` catch-all, whose comment then miscategorizes
it as a "single-card info/entry modal". It is a list modal with a toggle row. `handle_mouse_click`
compensates with a hardcoded `focus_row_rect(content, 0)` special case (`pointer.rs:490`) that
is only correct because `view::wakesettings::ROW_COUNT == 1`. Two divergent behaviours for one
screen, in the two functions whose module doc says they measure against the same geometry so
they cannot drift.

Related symptom: four `expect(...)`/`unreachable!(...)` in `prepare_modal` (`prepare.rs:848`,
`:851`, `:936`, `:953`) all say the same thing — "this arm is only reached when another match
elsewhere returned `Some`". That invariant should be carried by a type, not a panic message.

### P4 — two screen families are copy-paste

- **Plain list modal** (HostMenu, WakeSettings, Diagnostics, Experimental, CursorSettings). Each
  handler is: `rows()` → `list_nav` → `focus_anim = now()` → match `(cursor, ev)` → toggle +
  `switch_anim` → `Back` → `persist()`. `state/wakesettings.rs`, `state/experimental.rs` and
  `state/diagnostics.rs` differ only in the row table and the toggle targets. The render side
  repeats it again: `render/prepare.rs:925-955` is a `match self.screen` picking a row table,
  then one identical `FocusRowTile`.
- **Two-button confirm** (Wake, ForgetHost, SendLogs, SpeedTest). Six separate match tables
  over the same four variants: `confirm_subtitle`, `confirm_focused`, `set_confirm_focused`,
  `modal_focus_rect`, and two inside `prepare_modal` (`render/prepare.rs:843-870`).

### P5 — eight ad-hoc background channels

Seven `Option<mpsc::Receiver<_>>` fields on `App` plus `Discovery`, with eight hand-written
`drain_*` methods each returning `bool` "something changed", called one by one at
`runtime/ui_flow.rs:192-200`. Cancellation is "drop the receiver", documented per site.

### P6 — allocation on the per-frame and per-pointer-motion paths

Worst first:

1. `with_modal_screen` builds `host_menu_rows()` — a `Vec<FocusRow>` of owned `String`s — on
   **every Magic Remote `MouseMotion`** (`geometry.rs:407`), purely so a hit test can ask for a
   rect. `host_menu_actions()` already got a manual hoist in `prepare_modal` for exactly this
   reason; the geometry path did not.
2. `settings_rows()` returns an owned `Vec<FocusRow>` and is called at `prepare.rs:824`, `:1118`,
   `:1202` — up to three times in one frame.
3. `editing_override()` returns an owned `SettingsOverride`, called five times per frame
   (`:681`, `:711`, `:752`, `:933`, `geometry.rs:430`).
4. Three list handlers call `view::<screen>::rows(..).len()` — allocating a row list with its
   `String`s — to obtain a count that is a compile-time constant
   (`state/wakesettings.rs:24`, `state/diagnostics.rs:44`, and `ROW_COUNT` right beside it).
5. `prepare_grid` allocates four collections per unfrozen frame: a `HashSet<&str>` keep set, a
   `Vec<String>` of dropped ids, and `ready`/`waiting` index vectors (`prepare.rs:203-243`).
   These are window-bounded so they are not a scaling bug, but they are per-frame churn on a
   softfloat armv7 allocator, and `layout.pin_id_at` is called three times per windowed index.
6. `menu::audio_channel_options()` allocates a `Vec` to answer questions two of its three
   callers ask by count — `audio_option_count()` already exists as the workaround, and
   `row_lock` calls it on every geometry query.

### P7 — settings rows are `usize`, not an enum

`menu.rs:52-97` declares fifteen `pub const ROW_*: usize` across **three separate index spaces**
(the settings logical space, `EXP_ROW_*`, `CURSOR_ROW_*`), hand-mapped between by
`cursor_logical_row` and `settings_logical_row`, and policed by `debug_assert!`. Twelve tables
match on that bare `usize` with a `_ =>` fallback: `row_shown`, `row_lock`, `exp_row_lock`,
`toggle_value`, `row_fields`, `dropdown_options`, `dropdown_option_count`,
`dropdown_current_index`, `apply_dropdown_choice`, `adjust_setting`, plus
`handle_settings_event`'s `match menu::settings_logical_row(..)`.

Adding a row is currently: bump the constant, bump `SETTINGS_ROW_COUNT`, and remember all twelve.
The compiler checks none of it. This is the single largest "not canonical Rust" item in the
module and the cheapest to fix.

### P8 — zero tests

`src/app` has 10,951 lines and **no `#[test]` anywhere** (the whole crate has 38, all in
`core::model`, `platform::webos::*` and `session::sink`). Everything this plan proposes to move
is pure arithmetic with no I/O: `GridLayout::{card_at, pin_id_at, idx_for_pin_id, sections}`,
`max_scroll_px`, `clamped_scroll_px`, `clip_tile`, `settings_logical_row`, `cycle_index`,
`set_bitrate_fraction`, `home_focus_map` navigation. Refactoring this without a characterization
suite means the only regression detector is a human with a TV remote.

### P9 — `App`'s fields are the runtime's API

`runtime/ui_flow.rs` writes `app.detected_gamepad_type`, `app.home_status`,
`app.home_status_sticky` and `app.launch_anim` directly, and reads `app.screen`,
`app.home_focus`, `app.launch_ready`, `app.hero`. `pub` vs `pub(crate)` on the 75 fields tracks
nothing but history — `screen`, `games`, `art`, `settings`, `pin_digits`, `wake` are `pub` while
neighbours doing the same job are `pub(crate)`.

### P10 — the tile caches use the wrong containers, and throw away reusable buffers

Separate from P6 (which is about allocations the app makes); this is about the cache layer
underneath it. All of it is on the per-frame compose path, which runs at ~60fps for the whole
length of every grid scroll and every card pop.

**O1 — `TileStore` is a `HashMap<TileId, Entry>` over a dense `u32`.** `TileId` is
`pub struct TileId(pub u32)` (`ui/render.rs:126`) and `app/render/tile.rs` hands out ids in four
dense bands: 0-30 fixed, 32-63 settings rows, 64+ spinner frames, 256+ interned cards. Every
`get`/`contains`/`is_fresh` therefore pays a SipHash over a `u32` plus a probe, where a
`Vec<Option<Entry>>` indexed by `id.0` would be a bounds check. `compose_grid` alone does one
per visible card, and `draw_list` does one per draw command.

**O2 — card tiles are looked up by string, twice per card per frame.** `CardIds` is
`HashMap<String, TileId>` (`tile.rs:122`) and `GridState::card_pop` is
`HashMap<String, Instant>`. For each visible card, `compose_grid` calls `card_ids.get(pin_id)`
and then `card_pop_frac(pin_id)` — two SipHashes over the same game-id string — before the
`tiles.get` in O1 makes it three hash lookups per card per frame. `prepare_grid` does the same
walk again. Interning the pin id once per frame into a small `CardSlot(u16)` (or simply resolving
both maps in the single windowed pass that already exists) removes all of it. Note that keying by
identity rather than by grid position is load-bearing (pinning reorders the grid) — the fix is to
resolve the identity once, not to abandon it.

**O3 — card pixmaps are freed and immediately reallocated at the same size.**
`TileStore::remove` drops its `Painter` (`cache.rs:142`), and `prepare_grid` evicts before it
builds specifically so "a long scroll frees textures in the same frame it needs new ones"
(`prepare.rs:196`). At 1080p the grid is 5 columns of 260x346 cards, so each card pixmap is
~360KB; with `CARD_BUILD_BURST = 8` a fast scroll churns ~3MB of allocate-and-free per frame
through the armv7 allocator, for buffers that are all exactly the same size. Add
`TileStore::take(id) -> Option<Painter>` and a card-sized free list on `GridState`: the card size
is uniform and already stored (`grid.card_size`), so a recycled buffer never needs resizing. The
decoded cover `Pixmap` dropped alongside it (`prepare.rs:214`) is a second, larger instance of the
same churn.

**O4 — `prepare_scroll` rebuilds the whole settings row list every frame Settings is open.**
`prepare.rs:1118` calls `self.settings_rows()` — a `Vec<FocusRow>` of owned `String`s — and only
*then* checks each row's freshness (`:1125`), so a pure animation frame with nothing stale still
pays the full list build plus one `FocusRow::key()` hash per row. Gate the whole block on a
list-level version (the same value `modal_shell_version` already computes) and the steady-state
cost drops to one comparison.

**O5 — `modal_painter` allocates a fresh pixmap per modal rebuild.**
`geometry.rs:441` does `Painter::new(region.w, region.h)` every time. For the keyless screens
(`AddHost`), the modal rebuilds on *every* `content_dirty` tick — that is one card-sized
allocation per keystroke. `TileStore::ensure_in_place` (`cache.rs:100`) exists for exactly this
and is currently used only by the full-screen tiles.

**O6 — `TextCache` hashes its key twice.** It stores `HashMap<u64, Entry>` where the `u64` is
already `cache::version(&(text, color, font))` (`ui/text.rs:53,84`), so every lookup SipHashes a
value that is itself a hash. A pass-through `BuildHasher` removes the second hash from every
text draw.

**O7 — `tick_animations` scans `card_pop` every frame** (`mod.rs:829`) to ask whether any card is
still popping. Bounded by the scroll window, but it is a map walk per frame for a question a
single `card_pop_deadline: Option<Instant>` answers in one comparison.

**O8 — `persist()` deep-clones every known host.** `mod.rs:795` clones
`known_hosts: Vec<KnownHost>` — each with its per-game override map — plus `selected_host`, on
every settings Back, every pin toggle and every wake-setting flip. Not per-frame, so not urgent,
but the `StateWriter` could take an `Arc<Persisted>` and the clone would disappear.

**O9 — `cache::version` is SipHash.** `DefaultHasher` (`cache.rs:33`) is called dozens of times
per frame, including over whole `Settings` and `SettingsOverride` structs in
`modal_shell_version`/`modal_focus_version`. An FxHash-style multiply-xor `Hasher` over the same
`Hash` impls is several times cheaper and needs no change at any call site, because these values
are compared and never persisted.

> Caveat, and the reason O9 is last: `TextCache::key` uses `cache::version` as an *identity*
> key, not as a change detector — a collision there renders the wrong glyph run, and the code
> says so explicitly. Either keep SipHash for that one caller or widen its key to the full tuple
> before touching the hasher.

## 2. Target shape

```
app/
  mod.rs          App: ~8 fields, wiring only
  nav.rs          Screen stack + per-screen cursors      (P2, P3)
  jobs.rs         all background work + one drain        (P5)
  library.rs      games, art, pins, selected host        (was: 12 App fields)
  settingsui.rs   settings doc + scope + dropdown + scroll
  row.rs          SettingsRow enum + its tables          (P7)
  screens/
    list.rs       ListScreen descriptor + one handler    (P4a)
    confirm.rs    ConfirmScreen descriptor + one handler (P4b)
  state/ view/ render/   unchanged layout
```

`App` becomes:

```rust
pub struct App {
    nav: Nav,                 // screen, history, cursors, focus anim
    hosts: HostsState,        // known_hosts, entries, discovery, reachable, selection
    library: Library,         // games, art, pins, loaded flag
    settings_ui: SettingsUi,  // Settings, scope, game override, dropdown, scroll
    screens: ScreenSlots,     // pairing / addhost / wake / speedtest / sendlogs payloads
    render: RenderState,      // grid, modal, hero, press, dirty flags, evicted tiles
    jobs: Jobs,               // every channel, one drain
    identity: (String, String),
}
```

Rust-canonical, not Java-OOP: no inheritance, no `dyn` state objects. Composition, plus one
small trait per *measurably repeating* family, plus enums where the code currently uses bare
integers.

## 3. Phases

Each phase compiles, passes `task docker:lint` (`-D warnings`), and is independently
revertable. Ordered so the safety net and the cheap wins land before anything risky.

### Phase 0 — hygiene (no behaviour change) — DONE

1. Fix the `WakeSettings` hover gap (P3): add it to `hover_focus_at`'s list-modal arm and drop
   the hardcoded row-0 click special case. This is a bug fix, not a refactor — land it alone so
   it is bisectable.
2. Delete the dangling doc references. `docs/UI-PIPELINE-PLAN.md` (cited by CLAUDE.md for R6,
   the rejected `ScreenView` trait) and `docs/REFACTOR_PLAN.md` (cited by `state/mod.rs:2`) do
   not exist in the tree. Fold R6's rationale into §5 here and repoint both citations.
3. Fix CLAUDE.md's "~8 `Screen` match sites" to the real count; re-fix it after Phase 3.

### Phase 0.5 — characterization tests (P8) — DONE (23 tests; `task docker:test`)

The prerequisite for everything below. Pure functions only; no SDL, no fonts, no TV.

- `grid.rs`: `GridLayout` round-trips — `pin_id_at(idx_for_pin_id(id)) == id` for every pin
  arrangement (desktop pinned / in rest / absent, partial pinned rows, empty library).
- `render/geometry.rs`: `max_scroll_px`, `clamped_scroll_px`, `clip_tile` boundaries.
- `menu.rs`: `settings_logical_row` ∘ `settings_visible_logical_rows` is a bijection over the
  visible range in both scopes; `cycle_index` wraps; `set_bitrate_fraction` snaps to the
  `Automatic` sentinel exactly at the documented notch.
- `state/home.rs`: `home_focus_map` navigation reaches every card and never leaves
  `focus_window` (the invariant that silently freezes focus when broken).

Target: ~40 tests, all `cargo test` on the host, no cross-toolchain needed.

### Phase 1 — hot-path allocations and cache containers (P6, P10) — DONE except 3 and 7

Item 3 (`editing_override` → `Cow`) was dropped: `SettingsOverride` is `Copy`, so the call
never allocated. Item 7 (the known-hosts index) is deferred to Phase 6, which is where
`HostsState` gains the single mutation funnel an index can be maintained from.

Still unmeasured: the exit criteria below want `frame_timer` numbers from the TV, which
this work has not been checked against.

Pure optimization, no API churn. Measurable on armv7 softfloat, where a `FocusRow` list is
`String` formatting per row.

1. Give `ui::ModalScreen` a `metrics()` returning only what geometry needs (row count, title
   width), so `with_modal_screen` never builds `String` rows for a hit test. **The single
   hottest fix in the module** — it is on every pointer motion.
2. Add a frame scratch on `RenderCtx`: `rows: Option<Vec<FocusRow>>`, built at most once per
   frame and invalidated by the same version the shell key uses. Replaces the three
   `settings_rows()` calls.
3. `editing_override()` → `Cow<'_, SettingsOverride>`; it is a clone of a stored value in the
   game scope and `Default` otherwise, so neither case needs a per-call allocation.
4. Replace the three `rows(..).len()` count calls with the `ROW_COUNT` constants that already
   sit next to them (Phase 3a deletes the call sites entirely, but this is a one-line win now).
5. `menu::audio_channel_options()` → return a `&'static [(u8, &str)]` slice prefix; deletes
   `audio_option_count`'s reason to exist.
6. `prepare_grid`: hoist the four per-frame collections into `GridState` as reused buffers
   (`clear()` + refill), and call `layout.pin_id_at` once per index instead of three times.
7. `HostsState` gains a `HashMap<(String,u16), usize>` index, built in `set_entries` — the one
   mutation funnel that already exists — so `known_host`/`known_host_mut`/`host_listed` stop
   linear-scanning from render paths.
8. O1: `TileStore` → `Vec<Option<Entry>>` indexed by `TileId.0`. The four id bands are already
   dense; cap the vector at the top of the card band and fall back to push-on-grow.
9. O2: resolve each visible card's `TileId` and pop clock once per frame, in the windowed pass
   that already walks the same indices, instead of re-hashing its pin id in three places.
10. O3: `TileStore::take` + a card-sized `Painter` free list on `GridState`. Biggest single win
    during scroll; measure with the frame timer before and after.
11. O4: gate `prepare_scroll`'s settings block on a list-level version before building rows.
12. O5: route the modal tile through `ensure_in_place`.
13. O6: pass-through hasher for `TextCache`.
14. O7: replace the `card_pop` scan with a stored deadline.

O8 (persist clone) and O9 (the hasher) are deliberately left out of this phase: O8 is not on a
frame path, and O9 needs the `TextCache` identity-key question settled first. Both are one-file
changes that can land any time.

Exit criteria: zero allocations on `handle_mouse_motion`; at most one `Vec<FocusRow>` per frame
in `prepare_tiles`; no pixmap allocation on a steady-state scroll frame. Verify with
`runtime::frame_timer` on the TV, on a library large enough to scroll — this is the one phase
whose whole point is measurable, so measure it rather than assuming.

### Phase 2 — `SettingsRow` enum (P7) — DONE

```rust
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsRow {
    Resolution, Framerate, Bitrate, VideoBackend, Codec, Hdr, Audio, Gamepad,
    Cursor, Experimental, Diagnostics, About,
    CursorCapture, CursorGestures,   // sub-screen rows, on no list
    Reset,                            // per-game list only
}
```

Mechanical and entirely compiler-checked:

- Twelve `match row: usize { .. _ => }` tables become exhaustive matches. A new row fails to
  compile until every table answers for it — which is the whole point.
- `SETTINGS_ROW_COUNT`, `EXP_ROW_COUNT`, `CURSOR_ROW_COUNT` and the `debug_assert!` range checks
  go away; the display↔logical mapping becomes `&[SettingsRow]` slices per scope.
- `cursor_logical_row` disappears: the sub-screen's rows *are* `SettingsRow::CursorCapture` /
  `CursorGestures`, so the two index spaces become one.
- Keep display position as a distinct `usize` newtype (`DisplayRow`) so `settings_logical_row`'s
  two arguments can never be swapped — that confusion is live today in `DropdownState.row`,
  which is a display index that three call sites have to remember to convert.

Do `ExpRow` the same way, or fold Experimental's two rows into `SettingsRow` — they already
share the lock/toggle machinery.

Risk: low. Large diff, no logic change, every site compiler-found.

Landed as designed, with two notes. `settings_logical_row` returns `Option<SettingsRow>`
rather than falling back to the display index — with rows as a type there is no "the position
itself" to return, which is what let an out-of-range focus address a row. `DisplayRow` was
**not** introduced: it would have to cross into `ui::widgets::list_nav` and `FocusRowsState`,
which are library types shared with screens that have no logical rows at all. The confusion it
was meant to prevent (`DropdownState.row` being a display index) is now contained instead —
the only conversions left are inside `App::dropdown_options`/`dropdown_len`.

### Phase 3 — `Nav`: screen + cursors (P2) — DONE

```rust
pub struct Nav {
    screen: Screen,
    last_screen: Screen,
    /// Focus cursor per screen, surviving a round trip through a nested menu.
    cursors: [usize; ScreenKey::COUNT],
    focus_anim: Option<Instant>,
}
```

`ScreenKey` is `Screen` without payloads (`fn key(&self) -> ScreenKey`); the array is indexed by
a `const fn`, no new dependency.

- Deletes nine `App` fields.
- `list_modal_focused`, `list_modal_focused_mut`, `confirm_focused`, `set_confirm_focused`
  collapse to `nav.cursor(screen)` / `nav.cursor_mut(screen)` — four match tables gone, and the
  "these four must name the same field" hazard with them.
- `open_*` no longer each remember to zero their cursor: `Nav::enter(screen)` resets by default,
  `Nav::resume(screen)` for the ones that must keep their place.
- `Wake`'s cursor stays in `WakeState` (it is part of a payload that is `None` off-screen);
  `Nav` exposes it through the same accessor so callers do not care.

Landed. `focus_anim` stayed on `ModalState` rather than moving into `Nav` — it is a modal
animation clock, and every other clock beside it is there. The four match tables became
`nav.cursor(ScreenKey::of(screen))` plus one predicate per family (`is_confirm`,
`is_list_modal`), both exhaustive over `Screen`, which is what makes the P3 bug class
non-recurring rather than merely fixed.

### Phase 4 — the two screen families (P4, P3) — DONE

**4a. `ListScreen`.** One descriptor per plain list modal, in `screens/list.rs`:

```rust
pub(crate) trait ListScreen {
    const ROWS: usize;
    fn rows(&self, app: &App) -> Vec<FocusRow>;
    fn activate(&self, app: &mut App, row: usize, ev: MenuEvent) -> Handled;
    fn back(&self, app: &mut App);
}
```

One `handle_list_event` does nav + `focus_anim` + `switch_anim` + persist-on-back; the
descriptor supplies only what differs. `state/wakesettings.rs`, `state/experimental.rs`,
`state/diagnostics.rs` and `state/cursorsettings.rs` shrink to their row tables and toggles.
`prepare_modal`'s `WakeSettings | Diagnostics | Experimental | CursorSettings(_)` arm
(`prepare.rs:925-955`) becomes one arm with no inner match, and both `pointer.rs` arms become
one — which is what makes the P3 bug structurally impossible to reintroduce.

**4b. `ConfirmScreen`.** A value, not a trait — the four dialogs differ only in data:

```rust
pub(crate) struct Confirm<'a> { subtitle: String, buttons: [&'a str; 2] }
fn confirm_of(&self) -> Option<Confirm<'_>>;
```

Replaces six match tables with one, and deletes all four `expect`/`unreachable!` from
`prepare_modal`: the descriptor being `Some` is what proves the arm is reachable.

**4c.** Fold `dropdown_options_len`, `prepare_dropdown`'s option table and `address_copy` into
`ModalScreen` so their `_ =>` arms become exhaustive. This is what actually closes P3: after
4a-4c the count drops from 26 to ~10, and every survivor is exhaustive.

Explicitly **not** doing (R6): a full `ScreenView` trait owning state, input and rendering per screen
(R6). It forces `Box<dyn>` on the pointer path, hides the state machine behind dynamic dispatch,
and buys nothing for Home — the one screen with real complexity and no peer to share with.

Risk: medium. Behaviour-preserving but touches every list screen. Land 4a, 4b, 4c as separate
commits and verify each on the TV (`task deploy TELEMETRY=auto`) before the next — per
`docs/NOTES.md`, code-only confidence about this device is usually misplaced.

Landed as two commits (4b, then 4a+4c), **not yet verified on the TV** — that check is still
owed, and it is the one this plan says not to skip.

`ListScreen` is not a trait. The parts the five list screens actually shared were the row
table, the row count, the nav preamble and the switch-anim tail, and those are free methods on
`App` in `app::screens::list`; what differs (which rows, what a press means) stayed in each
screen's own `state`/`view` module, where a descriptor would only have re-indirected it. The
structural result the trait was for is there: one `prepare_modal` arm for all five, one
`pointer.rs` arm each for hover and click, one table naming the family.

`Confirm` is a value as designed (`app::screens::confirm`), and the shells read their buttons
from it — so the predicate deciding whether buttons are up is the same one in the shell, the
focused-button tile and the pointer. Forget and SendLogs lost their own `Modal` for a shared
`view::confirm` one. All four `expect`/`unreachable!` in `prepare_modal` are gone: its
focused-widget match yields `Option<Painter>`.

Count after 4c: 22 `match self.nav.screen` sites, and the fallback arms that remain are over
row indices or animation state rather than over `Screen`.

### Phase 5 — `Jobs` (P5) — DONE

```rust
pub(crate) struct Jobs {
    discovery: Option<Discovery>,
    games: Option<Receiver<GamesLoaded>>,
    art: Option<ArtLoader>,
    pairing: Option<Receiver<PairingOutcome>>,
    reach: Option<Receiver<Reachability>>,
    speed_test: Option<Receiver<SpeedTestMsg>>,
    send_logs: Option<Receiver<SendLogsMsg>>,
    rooted: Option<Receiver<bool>>,
}
```

Each `drain_*` keeps its body (they apply results to `App`, which is correct) but the runtime
calls one `App::drain_jobs() -> bool` instead of eight. Cancellation stays "drop the receiver",
now via `jobs.cancel_speed_test()` rather than raw field writes scattered across `state/*`.

Risk: low, mechanical.

Landed as designed. Field names lost their `_rx` suffix (`jobs.games`, `jobs.rooted`), the eight
`App` fields became one `jobs: Jobs`, and `drain_jobs` also absorbed `tick_reachability` since it
only exists to feed `drain_reachability`. Three cancel helpers: `cancel_pairing`,
`cancel_speed_test`, `cancel_library` (games + art together — a stale fetch landing after a host
switch would start art for the wrong library). `WakeState::probe_rx` stayed put: it is part of a
payload that is `None` off-screen, like `Wake`'s cursor in Phase 3.

### Phase 6 — split `App` (P1, P9) — DONE

Only after 2-5, which have already emptied most of it. Move the surviving fields into `Nav`,
`HostsState`, `Library`, `SettingsUi`, `ScreenSlots`, `RenderState` as sketched in §2. Prefer
`&mut self.render` / `&self.library` disjoint borrows over whole-`App` methods; that is what
lets `prepare_*` stop destructuring `RenderCtx` by hand, and what kills the eager
`host_menu_actions()` hoist.

Close P9 in the same pass: every field becomes private, and the four the runtime writes get
named setters (`app.set_gamepad_type(..)`, `app.set_home_status(..)`). `App::new`'s 118 lines
become each sub-struct's `Default`/`new`.

Risk: high churn, low logic risk — the compiler finds every site. One commit per sub-struct so
a bisect stays useful.

Landed as five commits (`Library`, `HostsState`, `SettingsUi`, `ScreenSlots`, `RenderState`) plus
one for P9. `App` is **19 fields**, all `pub(crate)` or tighter; the runtime writes through
`set_gamepad_type`, `set_keyboard_shown`, `set_home_status` and `end_slider_drag`. Two departures
from §2's sketch: `ScreenSlots` lives in `app::screens::slots` (next to the two families it holds
payloads for) and `RenderState` in `app::render::state`, rather than at the top of `app`. The
`HostsState` index deferred from Phase 1.7 was **not** done — the premise was wrong: `set_entries`
funnels `entries`, not `known_hosts`, which is mutated by `retain`/push from six `state/*`
modules. Funnelling those first is its own change.

`App::new` is 68 lines, from 118. The remaining fields are genuinely app-level: nav, the six
sub-structs, Home's focus and status, the launch handoff, the card menu, the state writer, the
gamepad type, the keyboard flag and the identity.

### Phase 7 — file size — DONE

`render/prepare.rs` (1291) and `render/compose.rs` (722) stay large but are cohesive once
Phase 4 removes their inner matches. Re-measure then. The two genuine outliers are
`prepare_grid` (395 lines, `prepare.rs:115`) and `compose_grid` (296 lines, `compose.rs:311`);
both are the O(visible) window logic. Split `prepare_grid` along its existing internal
boundaries only — evict / build-window / card-menu tiles / reveal / shared tiles are five
self-contained passes — and do not restructure the windowing arithmetic while doing it.

Landed. `prepare_grid` (430 lines by then) moved to its own `render/prepare_grid.rs` and became
seven methods: `release_stale_cards`, `evict_cards_outside`, `build_card_window`,
`prepare_focused_card_tiles`, `prepare_grid_shared_tiles`, `advance_grid_reveal` and
`prepare_no_host_tile`. The windowing arithmetic stays in `prepare_grid` itself, which is now
the only thing in the module that computes an index range. Two consequences worth knowing:
`art_ready` is a free function taking `&Library` (a closure could not survive `&mut self` in a
sibling pass), and the reveal now runs *after* the shared tiles rather than between them —
neither reads the other, both only ensure tiles and record what they rebuilt.

`compose_grid` split twice — `compose_focused_card` (the card's own glow/shadow/zoom/outline/
badge stack) and `compose_card_strip` (the title strip, or the submenu panel a hold grows out
of it) — and `compose_modal` once, into `compose_modal_card`. The layer functions now hold only
what is genuinely shared: `compose_grid` the window arithmetic, `compose_modal` the cross-fade
between an entering and a leaving card plus the scrim that belongs to neither.

Re-measured after: no function in `src/app` over 184 lines, and the two the plan named are 97
(`compose_grid`) and 73 (`prepare_grid`). `prepare.rs` is 880, `compose.rs` 763. The survivors
over 150 (`compose_modal_card`, `prepare_modal`, `prepare_scroll`) are each one screen family's
paint order end to end, which is the thing being described — splitting those would hide an
ordering the reader needs to see all at once.

## 4. Invariants this rework must not break

Carried from CLAUDE.md and verified against the code; treat as the regression checklist, and
encode as much of it as possible in Phase 0.5's tests.

1. **The grid stays O(visible)**, not O(library) — at build, draw, focus and hit-test. No new
   path may walk `self.games` per frame, per keypress or per pointer motion. Phase 6's `Library`
   split is the risk point: `pinned_count`/`desktop_pin` must stay precomputed by
   `reorder_games_by_pin`, never derived on demand.
   (Note `GridLayout::idx_for_pin_id` *is* an O(library) `position()` scan — acceptable only
   because it runs on pin toggles, not per frame. Keep it that way.)
2. `focus_window` always contains the current focus, or `FocusMap::navigate` finds no origin and
   focus freezes silently.
3. `GridState`/`CardIds` keep their hand-written `Default` — a derived one starts the id counter
   at 0 and hands the first card a `TileId` a fixed tile owns. Phase 6 must not
   `#[derive(Default)]` these.
4. Focus movement must not invalidate a modal shell — the `ModalShellKey`/`ModalFocusKey` split
   is what keeps a keypress from re-rasterizing the whole card. Phase 1's row cache must key off
   the shell version, not `content_dirty`.
5. Settings rows stay one tile each, keyed by `FocusRow::key`. Do not "simplify" back to one
   strip keyed on `Settings` — that regression is already on record.
6. Card art eviction must keep dropping the decoded `Pixmap` with the tile (`prepare.rs:214`);
   the cover is several times the tile's size.
7. `core::caps` has three readers that must agree. Nothing here touches it, so nothing here may
   start touching it — including Phase 2, which moves `row_lock` but must not move the
   `video_caps()` calls inside it.

## 5. Sequencing and verification

| Phase | Reversible | Verify |
| --- | --- | --- |
| 0 hygiene + WakeSettings fix | trivially | hover + click a Wake settings row on the TV |
| 0.5 tests | n/a | `cargo test` green, and red when an invariant is deliberately broken |
| 1 allocations + caches | yes | `frame_timer` before/after on a long scroll; hover feel |
| 2 SettingsRow enum | yes | every settings row, both scopes, plus both sub-screens — **still owed** |
| 3 Nav | yes | nested-menu round trip keeps its cursor (HostMenu → WakeSettings → Back) — **still owed** |
| 4 families | per-commit | every list screen + every confirm dialog, on the TV — **still owed** |
| 5 Jobs | yes | discovery, pairing, speed test, send-logs, root probe all still land — **still owed** |
| 6 App split | per-sub-struct | full menu pass + one stream launch/return — **still owed** |
| 7 file size | yes | lint only — done, plus `cargo test` |

Phases 1, 2 and 5 are independent and can land in any order after 0.5. Phase 4 wants 3 done
first (its handlers read `nav.cursor`) and is much cheaper after 2. Phase 6 wants everything
else done first.

Suggested first slice, if only one thing gets done: **Phase 0's WakeSettings fix + Phase 1.1**
(the `ModalScreen::metrics` hit-test allocation). One is a live bug, the other is the hottest
path in the module, and neither depends on anything else in this document.
