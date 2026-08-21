# Custom collections: implement `docs/COLLECTIONS-PLAN.md` phase by phase

**Original request:** "Read collections plan and implement it step by step, commit big phases.
Don't skip worthy improvements and optimization as you go, don't be afraid to go out of plan
scope. Make sure changes are structurally correct to current layout, rust canonical, OOP style."
**Branch:** `collections-plan` — **Status:** in progress, phases 1-6 of 9 committed and green

## Scope
`docs/COLLECTIONS-PLAN.md` in full: pins become per-host, user-ordered collections with Library
as the dynamic remainder. Nine phases; the plan is the spec, not a suggestion — deviations are
noted under Key decisions. Out of scope: anything the plan files under "Worth it later".

## Done
- **Phase 1+2 (`100d416`)** — landed as one commit; see Key decisions.
  `core::model`: `Collection`, `KnownHost::collections` (private-ish, `pub(crate)` only so the
  add/pair struct literals can seed it), the whole editing API, `GamePrefs::pin` →
  `legacy_pin` (`skip_serializing`). `services::store::migrate_collections` runs after
  `stamp_version` (which is the last reader of `legacy_pin`). Grid rewritten to N sections:
  `app::grid::{Group, Placed, GridLayout<'a>}`, `Library::regroup`, `view::home::GridSections`
  deleted into `GridLayout`. Section headings are a tile slot band (`tile::section`).
  Pin badge, `MAX_PINNED_GAMES` and the whole `PinLimit` screen deleted.
- **Phase 3 (`c9685a2`)** — `app::view::scrolllist` owns the scrolling-list geometry, taking a
  row count instead of `SettingsScope`; `tile::list_row`, `evict_list_rows_from`,
  `stitch_list_body`; `geometry::is_scroll_list` joins the exhaustive family tables; the two
  pointer row hit tests collapse into `App::row_at(content, rows, scroll_px, x, y)`.
- **Phase 4 (`73a5aeb`)** — `Screen::Collections`, `app::{state,view}::collections`,
  `app::screens::scrolllist` as the family table (rows / count / layout / invalidation key /
  scroll-into-view). Card submenu rows are dynamic via `card_menu_row_kinds` →
  `CardMenuRow::{MoveTo, Remove, Settings}`.

- **Phase 5 (`0de4b3b`)** — `app::state::textfield::{TextField, FieldKind}` replaces
  `AddHostState` (slots' `add_host` is now a `TextField`); `view::addhost::Modal` gained
  `hint` and is built by one `App::text_form()` that replaced `address_copy`.
  `Screen::RenameCollection` serves add *and* rename (`CollectionsState.index`); the add row
  creates and moves in one go. Wired into `text_input_screen` and both entry dispatches.
- **Phase 6 (`6e35aa2`, `e6c0524`)** — `FocusRow::menu` → `trailing`/`trailing_focused`/
  `trailing_active` + `ui::widgets::{trailing_button_rect, trailing_width}` and
  `Canvas::row_button`; `host_menu_dots` → `ScreenSlots::row_button`, stepped by
  `App::step_row_button` (`app::screens::rowbuttons`). Collections rows carry
  reorder/rename/remove (Library: no remove). `Screen::RemoveCollection` joins the confirm
  family. Drag mode is `CollectionsState.dragging`, committed by any non-vertical input.

## Left
1. **Phase 7** — in-collection card swap. `KnownHost::swap_within_collection` already exists
   and is tested. Split `cardmenu`'s `_ => self.close_card_menu()` arm on Left/Right.
2. **Phase 8** — `services::recents` + Library recency order in `Library::regroup`.
3. **Phase 9** — remove the `#[allow(dead_code)]` (see Gotchas) once phases 5-7 consume it.
4. **Device check** — none of this has run on a TV yet. The plan wants a real pre-collections
   `settings.json` copied off a TV tested against the migration before shipping phase 1.

## Key decisions
- **Phases 1 and 2 shipped together.** Phase 1 alone would have needed throwaway shims
  (`pinned_ids`, `toggle_pin`) that phase 2 deletes, because the grid is the only consumer.
- **`GridLayout` derives its geometry per call rather than caching it.** `Group` holds only
  column-independent data; `placed()` walks ≤21 runs of integer adds with no allocation. A
  cached `first_idx`/`y_offset` would need invalidating on every column change and could go
  stale between an event and the next `advance_frame`. Per-frame allocation on a `&self`
  geometry path is the regression the plan calls out — re-check that in review.
- **Borrow discipline:** the geometry accessors live on `impl Library`, not `impl App`, so a
  held `GridLayout` borrows one field and leaves `self.render` mutable. In `prepare_grid` the
  layout is rebuilt inside each helper rather than hoisted, for the same reason.
- **`prune_games` prunes collection membership too**, not just the `games` map.
- **Phase 5 shipped without the rename/remove *entry points*** — they arrived in 6a with the
  trailing buttons that reach them. Wiring an unreachable `open_*` earlier would have been
  dead code, and CI lints `-D warnings`.
- **The drag's "nudge" at the ends is the press dip** (`render.press.arm()`); there is no
  separate reject animation in `ui::animation`.
- **Drag mode swaps the heading suffix** rather than adding a subtitle line: the scroll-list
  shell has a title + suffix and no subtitle slot, so this costs no geometry.

## Dead ends
- Hoisting one `GridLayout` across `prepare_grid`'s helpers — five E0502s; it borrows `self`
  where the old owned `Copy` layout did not.
- Caching column-dependent group geometry on `Library` — see above.

## Gotchas
- **`cargo check` on macOS proves nothing.** `app`/`platform`/`session`/`runtime` are
  `cfg(target_os = "linux")`. Use `task -s docker:check`, `docker:lint`, `docker:test`
  (76 tests, all passing). `rtk proxy touch src/main.rs` if cargo looks suspiciously cached.
- **One `#[allow(dead_code)]` is deliberate**, on the second `impl KnownHost` block in
  `core/model.rs` holding the collection editors phases 5-7 will call. It carries a comment
  saying so; delete it, not the methods. CI lints `-D warnings`, so each commit must be clean.
- `rustfmt` fails hard on trailing whitespace rather than fixing it (`task -s fmt`).
- **Found and fixed on the way:** `list_nav_event` counted rows with `list_modal_row_count`,
  which is 0 for the scrolling family — Up/Down on the collections list did nothing. It now
  branches on `is_scroll_list`.
- `README.md` / `packaging/description.html` carried pre-existing user edits about collections;
  they went into `100d416`.
