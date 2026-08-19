# UI/menu render pipeline: instrumentation, per-frame cost, cache hygiene, structure

**Original request (session 1):** "please implement the UI pipeline plan. Focus on clean code, OOP
design, refactoring and simplicity."
**Session 2:** R2/R5/R6, plus "make sure a large game library doesn't blast the UI and doesn't take
forever to build textures", plus restructuring into files.
**Branch:** main (uncommitted working tree) — **Status:** code complete for the whole plan,
**not verified on device**

## Scope
`docs/UI-PIPELINE-PLAN.md` (untracked, in the tree) is the spec, and its "Status" section at the
bottom is the authoritative record of what landed. Everything in it is now done except the
deliberate omissions recorded there (`ScrollState`, and R6 which is answered with a
recommendation against).

## Done in session 2
- **Large-library hardening** (new work, see the plan's own section): `view::home::card_at_point`
  replaces the per-motion library scan in `App::hit_test_grid_card`;
  `view::home::focus_window` bounds `home_focus_map` to the visible band ± 2 rows;
  `replay_reorder_pop` re-arms `card_pop` only for resident cards. `CARD_BUILD_BUDGET` became a
  6ms time budget with a `CARD_BUILD_BURST` ceiling. `TileStore::bytes()` in the overrun report.
- **R5**: `ui::TileWidget` + `ui::rasterize` in `src/ui/widget.rs`; `src/ui/tiles.rs` became
  `src/ui/tiles/{mod,card,cardmenu,text,overlay,confirm}.rs`. Every tile in the tree is a
  `TileWidget` — including the ones that lived in `ui/widgets/*` and `app/view/*`. `padded_size`
  is the shared pad arithmetic. `runtime/toast.rs` (new) is the old `push_notification_cmd` as an
  owned type.
- **R2**: `src/app/grid.rs` (`GridState` + `GridLayout`/`GridCard` + card tuning),
  `src/app/modal.rs` (`ModalState`), `src/app/spinner.rs` (`GridReveal`).
- **R6**: recommendation written into the plan; not implemented.
- Removed the `visible_cards` property test session 1 added (per instruction: only tests that
  session added, and only where they carried no value). **Every pre-existing test is untouched** —
  an earlier over-broad deletion was reverted; `git diff HEAD` should show no test removals outside
  `src/app/view/home.rs`.

## Verify
1. `task deploy TELEMETRY=auto`, then read the frame report. Scroll a full library, open every
   modal. The plan's Phase 0 rule still holds: nothing here has been measured on hardware.
2. Behaviour-visible changes worth watching specifically:
   - **Pointer hit test** on the grid, especially the first library row under each section heading
     and the gutters between cards (`card_at_point` is a new inverse of the layout).
   - **D-pad navigation** at the top and bottom edges of the visible band, and entering the grid
     from the sidebar after scrolling far down (`focus_window`).
   - **Card fill rate** on first load and during a fast scroll (time-based build budget) — cards
     should now appear in visible batches rather than one per frame.
   - Grid cards appearing/evicting correctly behind an open modal (`grid_window_frozen`, session 1)
     and the hero image on launch (in-place `upload_raw`, session 1).
   - The pop animation after pinning/unpinning a card in a large library.

## Key decisions
- `GridState`/`CardIds` both hand-write `Default` rather than deriving it: a derived `CardIds`
  starts `next` at 0, handing the first card a `TileId` already owned by a fixed tile. Clippy's
  dead-code warning on `CardIds::new` is what surfaced this — do not "simplify" either back to a
  derive.
- `focus_window` must always contain the current focus index; `FocusMap::navigate` returns `None`
  when `from` names no item, which would freeze focus rather than fail loudly.
- `card_at_point` computes the candidate *row* arithmetically but confirms with the painter's own
  `unscrolled_card_rect`, so the gutters and heading bands keep matching nothing.
- `TileStore::bytes()` is called only on the overrun path — `FrameStats` holds `&TileStore` rather
  than two counts so a frame inside budget pays no scan.
- `ScrollState` skipped, R6 declined — reasons in the plan.

## Gotchas
- **Docker works now.** `task docker:check` is ~3s incremental, `docker:lint` and `docker:build`
  both pass. Session 1's handover claimed Docker was unusable (sign-in wall); that is no longer
  true, so use the real armv7 loop rather than the `hostcheck` scaffolding it described.
- CI lints with `-D warnings` and clippy is load-bearing here: it caught the `CardIds::default`
  bug and every dead accessor left behind by the extractions. Run `task docker:lint`, not just
  `check`.
- `cargo fmt` reformats beyond the edits; the tree is `cargo fmt --check` clean.
- `ui::prelude` glob-imports `tiles::*` into every `ui` module, so a new tile type's name must not
  collide with a widget's — hence the `*Tile` suffix on all of them.
