# Custom collections

Pinned becomes an ordinary user collection; Library stays the dynamic remainder. Games move
between collections from the card menu. Collections are per host, in `settings.json`.

## Model

`core::model`:

```rust
pub const MAX_COLLECTIONS: usize = 20;
pub const MAX_COLLECTION_NAME: usize = 24;

pub struct Collection {
    pub name: String,
    /// Member ids (`GameEntry::id` or `DESKTOP_PIN_ID`), in user order. Unbounded — no
    /// per-collection card limit, for Pinned or any other.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub games: Vec<String>,
    /// The one dynamic entry: Library, holding whatever is in no other collection. Its
    /// `games` is always empty on disk. Exactly one per host.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dynamic: bool,
}

pub struct KnownHost {
    ...
    /// Grid order, Library included as the `dynamic` entry. `None` in the document means
    /// "never migrated" (see store::load).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    collections: Option<Vec<Collection>>,
}
```

One vector carries everything: grid order, names, and membership. Library being an entry rather
than an implicit tail is what makes ordering, renaming and "hidden while empty" uniform instead
of three special cases — the only thing special about it is `dynamic`, which forbids removal and
means its members are computed rather than stored.

Rules, enforced by `KnownHost` methods so no caller can break them:

- A game is in at most one collection. `move_to(id, Some(ci))` removes it from any other first;
  `move_to(id, None)` returns it to Library. Membership is the *only* ordering key — no `pin`.
- `collection_of(&self, id) -> Option<usize>`, `collection_names()`, `add_collection(name)`,
  `rename_collection(i, name)`, `remove_collection(i)` (members fall back to Library).
- `MAX_COLLECTIONS` counts the user's collections; the dynamic entry is not one of them.
- `remove_collection(i)` refuses `dynamic` (there is no Remove icon on that row either — the
  guard is not the UI's only defence).
- `reorder_collection(from, to)` moves an entry, `dynamic` included: this vector *is* the grid
  section order.
- Names: trimmed, non-empty, `MAX_COLLECTION_NAME` chars, unique case-insensitively.
  `can_name(&self, i: Option<usize>, name) -> bool` is what gates the rename/add confirm button.
- `prune_games(live)` also drops vanished ids from every collection (never `DESKTOP_PIN_ID`).
- `upsert_known_host` keeps the existing record's `collections`, exactly as it keeps `games`.

Simplifications this unlocks:

- `GamePrefs::pin`, `is_pinned`, `pinned_ids`, `pinned_count`, `can_toggle_pin`, `toggle_pin`,
  `pinned_only`, `MAX_PINNED_GAMES` all go. `GamePrefs` collapses to the override map:
  `games: BTreeMap<String, SettingsOverride>` (kills `is_empty`/`drop_if_empty` bookkeeping).
- `Screen::PinLimit`, `app/state/pinlimit.rs`, `app/view/pinlimit.rs`, `ScreenKey::PinLimit`
  and `PIN_LIMIT_MESSAGE` go — there is no per-collection cap, only 20 collections.
- `tile::PIN_BADGE` / `PIN_BADGE_SIZE` / `PIN_BADGE_MARGIN` go: the section heading already
  says which collection a card is in.

## Migration (no version field)

In `store::load`, after `stamp_version`, one pass per host:

- `collections == Some(_)` → nothing to do.
- `None` and the host has legacy `pin` values → one collection `"Pinned"`, members in old pin
  order (`pin` stays as a deserialize-only field: `#[serde(default, skip_serializing)] legacy_pin`).
- `None` and no pins → just the Library entry.

Every migrated and every new host gets the `dynamic` Library entry appended last. A *new* host
(add/pair flows, `new_host_games`) is bootstrapped with `"Pinned"` holding `DESKTOP_PIN_ID` ahead
of it, matching today's behaviour.

Idempotent, because the first save writes `collections` and stops serializing `pin`.
`seed_desktop_capture` reads `legacy_pin` (it only runs on a document with no version, i.e.
one that still has pins) — order it before the collection pass.

## Grid: N sections

Today `GridLayout` hardcodes pinned-block + rest and `view::home::GridSections` hardcodes two
headings. Generalize to a bounded list of *groups* (≤ 21: 20 collections + Library).

`app::library::Library` gains what `reorder_games_by_pin` used to encode in two fields. The
runs are precomputed once per change, so nothing on a per-frame path derives them:

```rust
/// One grid section, in grid order. Built by `regroup_games`, read by everything else.
pub(crate) struct Group {
    pub name: String,
    /// Cards in it — `library.games` members plus the Desktop card if it lives here.
    pub len: usize,
    /// First grid index (whole rows: a partial last row pads, as the pinned block does today).
    pub first_idx: usize,
    pub rows: usize,
    /// Heading + gap pixels stacked above this group's first row.
    pub y_offset: i32,
    /// This group holds `DESKTOP_PIN_ID`, and it heads the group.
    pub desktop: bool,
}
pub(crate) groups: Vec<Group>,
```

`regroup_games` (was `reorder_games_by_pin`) walks `KnownHost::collections` in order, laying out
`library.games` collection-by-collection in membership order — the `dynamic` entry taking
whatever is left over, wherever in the order it sits — and fills `groups`. `clear_grid_groups`
(was `clear_grid_pins`) empties it. Empty collections are **not** rendered in the grid (they stay
in the management modal) — a zero-row section has no place to hang a heading.

**Desktop gating.** `DESKTOP_PIN_ID` counts toward a group's `len` only while
`library.games_loaded`; otherwise the grid draws a heading over nothing (today's
`desktop_pinned = games_loaded && desktop_pin`). `regroup_games` therefore runs after a library
load and reads `games_loaded`, and the host-switch path clears the groups instead of leaving a
stale one. Worth a test: an unloaded library has no cards *and* no headings.

`GridLayout` keeps working the way it does today — a `Copy` value carrying counts, with the data
passed in per call (`card_at(&self, games, idx)`); the group runs join `games` as a borrowed
parameter rather than becoming an owned field:

```rust
GridLayout { columns: usize, total: usize, rows: usize }   // still Copy
fn card_at<'a>(&self, groups: &[Group], games: &'a [GameEntry], idx: usize) -> Option<GridCard<'a>>
```

- `card_at` / `pin_id_at` / `game_at` / `idx_for_pin_id` find the group by scanning ≤21 runs
  (`first_idx` is sorted, so it is a partition point) and then index inside it — O(groups),
  never O(library), and no allocation.
- Same for `GridSections`, which becomes a `Copy` newtype over the slice — `GridSections<'a>(&'a
  [Group])` — **not** a struct of `Vec`s. It is passed by value into `visible_cards`,
  `card_at_point`, `unscrolled_card_rect`, `focus_window` and `section_heading_rect`, all of
  which run per frame or per pointer motion; a `Vec` field there would allocate on every mouse
  move. `row_offset_at` reads the group's precomputed `y_offset`; `row_bands` returns an
  `impl Iterator` over the runs instead of `[_; 2]`.
- `PINNED_SECTION_GAP` becomes the gap above every heading after the first, folded into
  `y_offset` at build time.
- `total_extra` is the last group's `y_offset` (plus its gap) — `max_grid_scroll` and
  `grid_layer_height` keep working off it unchanged.

**Borrow discipline (this is what would otherwise fail to compile).** Today
`self.grid_layout(cols)` returns `Copy`, so callers mutate `self` afterwards freely
(`replay_reorder_pop` holds a layout and then calls `self.render.grid.arm_card_pop`). Anything
that borrows out of `self` breaks that. So the group-aware geometry accessors move from
`impl App` onto `impl Library` (`self.library.layout(cols)`, `self.library.pin_id_at(idx, cols)`),
which borrows one field and leaves `self.render` mutably available — exactly the disjointness
`app/library.rs`'s module doc already claims as its reason to exist. `App` keeps thin forwarding
methods for read-only call sites.

Tiles: `SECTION_PINNED`/`SECTION_LIBRARY` are replaced by a slot range, `tile::section(i)`
(21 slots) as `settings_row` does, keyed on `(label, width)` — they stop being
`ensure_static`, since the labels are now user text. `prepare_grid`'s two-entry table and
`compose_grid`'s two-entry table become loops over the visible groups (cull by band as today).

Focus/pop after a move: the existing pattern generalizes — latch the moved id, `regroup_games`,
`grid_idx_for_pin_id` to keep focus, and replay the pop for the moved card plus every card from
the first touched group onward (`replay_reorder_pop`, same shape, group-bounded).

`app/grid.rs`'s property tests carry the weight here: `arrangements()` becomes group splits
(counts either side of a row boundary, Desktop in the first / a middle / the last group, empty
groups interleaved, unloaded library), and `pin_id_round_trips_through_idx`,
`every_card_appears_exactly_once` and `holes_are_only_the_padding` stay as the acceptance
criteria for the whole rewrite. Same for `view::home`'s `the_window_always_contains_the_current_focus`.

## Card menu

`app/state/cardmenu.rs`: rows become dynamic (2 or 3), so `ROW_COUNT` gives way to a per-card
count derived from membership (the panel is baked on focus, before a menu exists — membership is
available there, so this is safe for the tile key and for `card_menu_rows_rect`).

- `Move to…` — keeps `icons().pin`; opens `Screen::Collections`.
- `Remove` (only when the card is in a collection) — `move_to(id, None)`, persist, regroup, close.
  No dialog.
- `Settings` — unchanged.

The panel's baked height comes from `ui::widgets::card_menu_strip_h(.., ROW_COUNT)` and its tile
key from the same constant, so both take the per-card count; `card_menu_rows_rect` and
`card_menu_row_at` divide the band by that count too. Like Settings, `Screen::Collections` opened
from the menu leaves the menu up behind it (a step *into* it), and the move that closes the modal
closes the menu as well.

## Screens

Three new screens, each joining an existing family, so the exhaustive tables do the reminding
(`ScreenKey::of`, `list_modal_rows`, `list_modal_row_count`, `dropdown_options`, `confirm_of`).

1. `Screen::Collections` — list-modal family. Rows: one per entry in `collections` order,
   Library included (`ICON_FOLDER`, name, muted "N games" hint), plus a final
   `+ Add collection` row (`ICON_ADD`), hidden at `MAX_COLLECTIONS`. Confirm on a row = move the
   target card into that collection (or, on Library, out of whatever holds it), persist, regroup,
   back to Home. Confirm on the add row opens the name dialog.
2. `Screen::RenameCollection` — text-entry modal (also used for Add; the difference is whether it
   starts from an existing name and which index it commits to).
3. `Screen::RemoveCollection` — confirm family: `Confirm::new(ICON_DELETE, "Remove", error,
   "Cancel", "… its N games return to Library.")`.

`ScreenSlots` gains `collections: CollectionsState { target: Option<String>, index: Option<usize> }`
— the card being moved and the collection a rename/remove is acting on, reset by `open_*`
(same shape as `host_menu_index`).

### Reordering (drag mode)

A third trailing icon (`ICON_REORDER`, `\u{E945}` "drag handle") on every row, Library included.
Confirm on it enters drag mode: `CollectionsState.dragging: Option<usize>`.

While dragging:

- Up/Down move the entry itself (`reorder_collection`), not the cursor — the cursor rides it, so
  the row and its focus travel together. No wrap: a drag stops at the ends.
- Any other input commits: Confirm, Back, Left/Right, Secondary, a click, a controller button.
  Only the d-pad/stick axis is consumed by the drag, exactly as asked.
- Commit = clear `dragging`, `persist()`, `regroup_games()`. Nothing else is written mid-drag, so
  a burst of Up/Down is one save and one regroup.
- The row is drawn lifted (the focus-row tile's own treatment plus a brighter handle) so drag mode
  is visibly distinct from a focused handle; the neighbours it displaces slide with the existing
  focus-band animation rather than a new mechanic.
- Drag mode is exited by leaving the screen too (`open_*`/Back reset it), so it cannot outlive the
  list it reorders.

This order is the grid's section order — the same vector `regroup_games` walks — so committing a
drag reorders the Home grid on the next regroup with no separate ordering state.

### Row trailing buttons

A collection row carries three icon buttons: Reorder, Rename, Remove. Library carries Reorder and
Rename only — no Remove icon at all, and `remove_collection` refuses it regardless. `FocusRow::menu: Option<bool>` (the
host row's single ⋯) generalizes to:

```rust
pub trailing: Vec<&'static str>,       // icons, right-aligned; per-row, so Library omits Remove
pub trailing_focused: Option<usize>,   // which one has focus, if any
pub trailing_active: Option<usize>,    // one held open — the handle in drag mode
```

The host menu's ⋯ becomes the one-icon case; `ScreenSlots::host_menu_dots` becomes
`row_button: Option<usize>`, and Right/Left step through a row's trailing buttons before leaving
the row. One mechanic, drawn once, hit-tested once (`pointer.rs` gets the per-icon rects from the
same layout function the painter uses).

### Text entry

`AddHostState` is IP-specific (octet separators, digit gating). Extract the shared part:

```rust
pub struct TextField { text: String, kind: FieldKind }
pub enum FieldKind { Ipv4Port, Name { max: usize } }
```

`Ipv4Port` keeps today's behaviour verbatim (`enter_digit`/`advance_field`/`is_complete`);
`Name` accepts printable chars, `backspace`, and `is_complete = can_name(...)`, which is what
greys the confirm button. `view::addhost` takes the title/subtitle/hint as parameters so the
rename modal reuses its layout. `runtime::input`: add the two entry screens to
`text_input_active` and to the `TextInput`/digit dispatch.

### Scrolling — bigger than one match arm

Correction to the first draft: a list modal cannot scroll by adding an arm to
`scroll_geometry_for`. Its header *and every row* are baked into `tile::MODAL`, and
`list_modal_card_rect` derives the card height from the row count — 21 rows makes a card taller
than the screen, and re-baking that whole card per scroll step is the 25-60ms armv7 raster the
Settings screen exists to avoid.

So `Screen::Collections` joins the **Settings** rendering pattern, not the plain list-modal one,
and that pattern gets generalized on the way:

- Shell tile without rows + one tile per row in a slot band + `tile::SCROLL_CONTENT` crop +
  `SCROLL_INDICATOR` + edge fades. All of it exists; the settings-specific names become shared:
  `tile::settings_row(i)` → `tile::list_row(i)` (`SETTINGS_ROW_SLOTS = 32` already covers 21+1),
  `evict_settings_rows_from` → `evict_list_rows_from`, `stitch_settings_body` (the closing-modal
  snapshot) → `stitch_list_body`, `view::settings::{visible_rows, PEEK, layout}` →
  a shared `view::scrolllist`.
- `scroll_geometry_for` / `scroll_stride_for` / the `sync_modal_scroll` peek bias then take one
  arm for the scroll-list family (`Screen::Settings(_) | Screen::Collections`) instead of a
  fourth copy.
- **Pointer**: `modal_list_row_at` uses fixed row rects with no scroll offset, while
  `settings_row_at` uses `focus_row_rect_at_px(content, r, scroll_px)`. A scrolling list must use
  the px form, or a click lands on the wrong row once scrolled — the two hit tests collapse into
  one that takes the screen's scroll offset (0 for the non-scrolling modals).
- Card height is capped by the viewport (`visible_rows`), so the modal stops growing at ~8 rows
  and scrolls beyond that. The non-scrolling list modals (host menu, wake settings, …) keep the
  cheap single-tile path untouched.

`ModalShellKey`/`ModalFocusKey` gain variants for the three new screens. The collections shell key
carries the names and counts; the **focus** key carries the focused row, which trailing button is
focused, and whether the row is being dragged — a lifted row that is not in the focus key never
re-rasters.

## UX, beyond the mechanics

Each of these is cheap because the machinery already exists; the note says which.

**Do these — they remove a step or a dead end**

- *Create-and-move in one go.* `+ Add collection` from a card's `Move to…` names the collection,
  creates it, moves the card into it and closes. Otherwise the user names a collection, lands back
  on the list, and has to pick it — two decisions for one intent. The add dialog already knows the
  target card (`CollectionsState.target`), so this is one branch on commit.
- *Show where the card is now.* The row holding it gets `FocusRow::mark` (the override dot already
  drawn on settings rows) and the cursor opens on it. A move-to list that opens on row 0 makes the
  user find the current state themselves.
- *Name the current collection in the card menu.* `Move to…` carries it as the row's muted value,
  and Remove reads `Remove from Pinned`. `RowKind::Action` already draws a right-hand value.
- *Say why the confirm is dead.* Empty or duplicate name → `RowSubtext::caution` on the field
  ("Already used") instead of a greyed button with no explanation. Constructor exists.
- *Confirm the move on Home.* One toast, "Moved to Pinned" (`runtime::toast`). The card also
  animates into its new section and focus follows it, but the section may be off screen — scroll
  focus into view on the move, which `scroll_into_view` already does for the grid.
- *Count in the grid heading.* `Pinned · 6`. With up to 21 sections, the heading is the only
  orientation the grid has, and it is already rasterized per section with the label in its key.

**Do these for the drag**

- *Say what drag mode does while it is on.* The list modal's subtitle becomes
  "Up/Down to move, OK to drop" for the duration — the subtitle is already a per-frame string and
  its height already drives the card layout.
- *Nudge at the ends.* A drag that cannot move further plays the short reject animation rather
  than nothing, so "it stopped" and "it ignored me" stay distinguishable.
- *Don't strand a drag.* Committing on any non-d-pad input covers the remote's odd buttons, and
  leaving the screen commits too — never discards. A reorder the user watched happen must not
  silently un-happen.

**Reconsider two things in the current plan**

- *Empty collections hidden in the grid* is right for the grid but reads as a vanished collection.
  The modal row should say `0 games — hidden until you add one` (muted hint, same slot as the
  count). No new geometry, and the question never gets asked.
- *Where a moved card lands.* Appending to the end matches today's `toggle_pin` and is
  predictable, but the first slot is where a just-pinned card is looked for. Keep append, and let
  the pop animation point at it — the card can then be walked to where it belongs with the
  in-collection reorder above, which is the answer to this question rather than a guess at intent.

**Worth it later, not now**

- *Sticky section heading* while the grid scrolls under it — real orientation win in a 21-section
  grid, but it needs the heading composited outside the scrolled band.
- *Section skip* on the grid (jump heading to heading) for long libraries; needs a free button on
  the remote, so it wants deciding on hardware rather than here.
- *Discoverability.* Collections live behind a card hold, which nothing on screen reveals — reuse
  the one-shot `INTRO_HINT` mechanism (`store::Loaded::new_build`) to introduce them once on the
  first library load after the update.

## Card reordering inside a collection

While a card's slide-up menu is open, Left/Right move *that card* within its collection. Not
available in Library (its order is recency, see below) — there Left/Right keep closing the menu,
as they do today.

Mechanics, kept deliberately dumb:

- **Swap, don't insert.** Left/Right exchange the held card with its neighbour in the collection's
  member order. Every other card keeps its slot, so nothing reflows and the grid needs no new
  geometry — this is the whole reason the interaction is a swap.
- **In place, not through a regroup.** Both cards are inside one group, so the swap is two
  `Vec::swap`s: `library.games` (what the grid draws) and the collection's `games` (what persists).
  Group boundaries, `Library::groups` and every card tile are untouched — tiles are keyed by pin
  id, so a swapped card carries its own pixels with it. No rebuild, no O(library) work per press.
- **The menu travels with the card.** `CardMenu::idx` is re-latched to the new index after each
  swap (`grid_idx_for_pin_id`), so the panel, its band and its hit test stay on the card they
  belong to — the latch's existing staleness check then keeps holding.
- **Wrapping.** A swap at either end of the collection is a no-op with the reject nudge, not a
  wrap onto another row: the collection is a block, and wrapping across its edge reads as the card
  escaping.
- **Scroll.** If the swap moves the card onto another row, `scroll_into_view` brings it back —
  the same call the focus path already makes.
- **Committed on menu exit.** Closing the menu (any way) fixes the order: `persist()`, then the
  pin-toggle reorder animation replayed over the collection (`replay_reorder_pop`, scoped to the
  group) so "fixed" is visibly the same gesture as pinning is today. Nothing is written per press.
- **Unfixed state is visible.** While the menu is open *and* the card has been moved, every other
  card **in that collection** is dimmed to the modal scrim's level. Mechanism: scale that card's
  (and its shadow's) `DrawCmd::Tex` alpha by `1 - palette().scrim.a/255` in `compose_grid` — one
  multiply on a value already computed per card. Not a `DrawCmd::Fill` over each card: `Fill` is a
  square rect and would square off the card's rounded corners. Held card, its panel and
  everything outside the collection stay at full alpha. The dim goes away with the commit.

`app/state/cardmenu.rs`'s `_ => self.close_card_menu()` arm splits: Left/Right reorder when the
card is in a collection, close otherwise.

## Library order: recently played first

Library stops being "whatever the host listed" and becomes most-recently-played first, then the
never-played in the host's own order. Collections keep their user order — recency applies to the
dynamic group only.

Kept out of `settings.json`, which should not accumulate a timestamp per game:

- `services::recents` over `app_dir()/recents.json`:
  `{ "<host>:<port>": { "<game id>": <unix secs> } }`. Written through `services::atomic::write`
  like everything else, loaded once at startup into a `HashMap` on `App`.
- Recorded where the launch actually *takes* — `runtime::ui_flow`, once the connect succeeds —
  not in `confirm_grid_card`, which also fires for launches that bounce into the Wake dialog or
  fail to pair. A failed launch must not reorder Library.
- Written immediately (one small file, one launch) rather than through `StateWriter`: a stream
  follows, and a coalesced write is the one a crash eats.
- Returning from a stream regroups the Library, which can move the card that was just played.
  Re-latch focus by id after the regroup (`grid_idx_for_pin_id`), or focus lands on a stranger.
- `regroup_games` sorts the Library remainder by `recents` descending, unplayed last in host
  order. It already rebuilds the whole list on every change, so this is a comparator, not a pass.
- Pruned against the live library the same time `prune_games` runs, and a forgotten host drops its
  whole entry — otherwise the file grows forever with ids nothing can reach.
- Absent, unreadable or truncated file → empty map, i.e. today's ordering. It is a cache, and
  losing it must never be an error.

## Touch list

`core/model.rs`, `services/store/{mod,legacy}.rs`, `app/library.rs`, `app/grid.rs`,
`app/view/home.rs`, `app/render/{tile,prepare_grid,compose,key,geometry,prepare}.rs`,
`app/state/{home,cardmenu,addhost,edithost}.rs`, `app/view/{cardmenu,addhost,icons}.rs`,
`app/screens/{list,confirm,slots}.rs`, `app/nav.rs`, `core/screen.rs`, `app/pointer.rs`,
`runtime/{input,ui_flow}.rs`, `ui/widgets/{rows,listmodal,scroll}.rs`, `ui/scroll.rs`, new
`services/recents.rs` and `app/view/scrolllist.rs`, plus deleting the three pin-limit files.

Verified call sites of what is being removed — the whole list is short: `is_pinned` in
`compose.rs:604` (pin badge), `view/cardmenu.rs:18` (row label) and `store/mod.rs:111`
(`seed_desktop_capture`); `toggle_pin`/`can_toggle_pin` in `state/home.rs:264-274`;
`pinned_ids`/`pinned_count`/`desktop_pin` in `state/home.rs:326-354` and `app/library.rs`;
`new_host_games` in `state/addhost.rs:47` and `state/pairing.rs:152`.

## Phases

Ordered so nothing is built twice: the scroll-list generalization lands *before* the screen that
needs it, and the grid rewrite lands before anything can create a second collection.

1. **Model + migration + persistence.** No UI change — Pinned renders exactly as today through
   the generic group path. Device check: existing hosts keep their pinned block; a fresh install
   gets Pinned + Desktop.
2. **Grid → N groups.** `Library::groups`, the `Copy`-preserving `GridLayout`/`GridSections`, the
   heading slot tiles, the borrow move onto `impl Library`. Still one collection, so any visible
   change is a bug. Property tests are the gate; device check for scroll, focus, pointer, pops.
3. **Scroll-list family.** Generalize Settings' scrolling row list; Settings itself must look and
   behave identically afterwards (device check), because it is the only client so far.
4. **Collections screen** + card menu `Move to…` / `Remove`, built on 3.
5. **Add / rename / remove dialogs**, `MAX_COLLECTIONS`, the name rules. Verify the webOS
   on-screen keyboard on the TV early — text entry there has a history (see `docs/NOTES.md`).
6. **Drag-mode reordering** of collections.
7. **In-collection card reordering** (swap, scrim, commit-on-exit).
8. **`services::recents`** and Library's recency order.
9. **Cleanup:** delete the pin APIs, `PinLimit`, the pin badge, collapse `GamePrefs`.

Phases 1-3 are refactors with no new feature surface; 4-8 are each independently shippable.

## Risks

- **The grid rewrite is the whole risk.** Every geometry consumer (visible band, focus window,
  pointer hit test, heading placement, scroll extent) reads the same two-section shape today.
  Mitigation: the existing property tests, generalized first and kept passing at every step, plus
  the `focus_window` contains-focus test — that one is what silently freezes focus when broken.
- **Per-frame allocation.** The tempting `Vec<usize>`-per-call sections struct is a regression on
  armv7 softfloat, on paths that run per mouse move. Kept out by design above; worth re-checking
  during review that no `Vec`/`String` was introduced on a `&self` geometry path.
- **Raster budget.** 21 heading tiles and up to 22 row tiles is bounded and built once each, but
  the collections modal's first open should be measured on the TV; if it hitches, add it to the
  warm-up pass that already pre-renders Settings.
- **Migration is one-shot and irreversible** (the first save drops `pin`). Test it against a real
  pre-collections `settings.json` copied off a TV before shipping phase 1, not just a synthetic
  document.

## Assumptions worth a nod before coding

- One collection per game (no multi-membership). Membership lists carry order for free and the
  grid stays a partition of the library.
- Collection order is the `collections` vector, user-reorderable by drag; Library is an entry in
  it and may be moved and renamed, but never removed.
- No card limit in any collection, Pinned included.
- Empty collections are hidden in the grid, listed in the modal.
- The Desktop card is movable like any other card.
- Library's own order is recency, so cards there are not hand-orderable — the in-collection
  reorder is a no-op in Library, and the modal's Library row still reorders the *section*.
