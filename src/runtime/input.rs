//! Pre-stream menu input plumbing: hold/chord gestures, the confirm dialog, and the
//! SDL-event → `MenuEvent` routing shared by the `ui_flow` and `stream` loops.
//!
//! Split out of `runtime/mod.rs` (which keeps run/connect/signals/log-overlay). Re-exported
//! there via `use input::*` so the sibling loop modules pick these up through `use super::*`.

use super::*;

/// How long a controller shortcut ([`DisconnectChord`]) must be held before its dialog
/// opens — the in-stream disconnect dialog while streaming, the quit dialog in the menu.
/// Every button in these shortcuts is also real game input, so a hold — not a press — is
/// the only safe trigger (L1+R1 in particular is a common in-game bind); the hold window
/// is the margin against a stream dying mid-play. Shared by both loops so the remote's
/// held-Back EXIT gesture and the controller chord feel the same in either context —
/// 1s to match webOS's own long-press threshold on the EXIT gesture.
pub(super) const EXIT_HOLD: Duration = Duration::from_millis(1000);

/// How long OK must be held on a focused Home game card to pin/unpin it instead
/// of launching it — see `card_hold_gate`.
pub(super) const CARD_HOLD: Duration = crate::core::event::LONG_PRESS;

/// An in-flight hold-to-pin gesture: OK is down on a pinnable Home card. The
/// toggle fires the moment `CARD_HOLD` elapses (so the pin visibly lands under
/// the still-held button), and `fired` then makes the release a no-op instead
/// of the launch a quick tap would have dispatched.
pub(super) struct CardHold {
    pub(super) since: Instant,
    pub(super) focus: HomeFocus,
    pub(super) fired: bool,
}

/// The gamepad routes to the disconnect dialog (streaming) or quit dialog (menu): Guide,
/// both shoulders, or Start+Back, each held for [`EXIT_HOLD`].
///
/// Tracked as button state rather than read back from SDL because SDL only reports
/// transitions here — and a chord needs to know what is down *now*, not what changed
/// last. Three shortcuts share one timer: the gesture is "some disconnect chord has been
/// complete for long enough", so sliding from one chord into another (releasing Start
/// while both shoulders stay down) is one continuous hold rather than a restart.
#[derive(Default)]
pub(super) struct DisconnectChord {
    guide: bool,
    left_shoulder: bool,
    right_shoulder: bool,
    start: bool,
    back: bool,
    /// When the currently-held chord became complete; `None` when none is.
    since: Option<Instant>,
}

impl DisconnectChord {
    /// Records one button transition and arms or disarms the hold timer.
    pub(super) fn set(&mut self, button: sdl2::controller::Button, down: bool) {
        use sdl2::controller::Button;
        match button {
            Button::Guide => self.guide = down,
            Button::LeftShoulder => self.left_shoulder = down,
            Button::RightShoulder => self.right_shoulder = down,
            Button::Start => self.start = down,
            Button::Back => self.back = down,
            _ => return,
        }
        // Re-derived after every transition, so releasing any part of a chord restarts
        // the hold instead of leaving a stale deadline armed.
        self.since = match (self.complete(), self.since) {
            (true, Some(t)) => Some(t),
            (true, None) => Some(Instant::now()),
            (false, _) => None,
        };
    }

    fn complete(&self) -> bool {
        self.guide || (self.left_shoulder && self.right_shoulder) || (self.start && self.back)
    }

    /// Whether a chord has now been held long enough to fire.
    pub(super) fn held_for(&self, hold: Duration) -> bool {
        self.since.is_some_and(|t| t.elapsed() >= hold)
    }

    /// Forgets all held buttons.
    ///
    /// Called when the chord fires and when the pad disconnects, because in both cases
    /// the releases that follow never reach [`set`](Self::set) — the open dialog swallows
    /// controller events, and an unplugged pad sends none. Without this the buttons would
    /// stay "down" forever and the dialog would reopen the instant it was dismissed.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }
}

/// What feeding an event to an open [`ConfirmDialog`] resolved to. `Confirmed`
/// leaves the dialog open — the caller runs its action and dismisses (or exits).
pub(super) enum ConfirmAction {
    /// Primary (index 0) button activated.
    Confirmed,
    /// Cancel/Back — the close-fade has been started.
    Dismissed,
    /// Focus moved between buttons.
    Navigated,
}

/// A two-button confirm dialog (stop-streaming mid-stream, quit-app in the menu) —
/// same open/close fade as pre-stream modals. Rendered as a compositor overlay via
/// `tile::DISCONNECT_DIALOG` + `tile::DISCONNECT_FOCUS_BUTTON`; the menu and stream
/// never show one at the same time, so they share those two tile slots.
pub(super) struct ConfirmDialog {
    title: &'static str,
    subtitle: &'static str,
    buttons: [crate::ui::widgets::ConfirmButton<'static>; 2],
    focus: Option<usize>,
    fade: crate::ui::fade::ModalFade<usize>,
    /// Re-render only on open; focused button is its own tile.
    shell_dirty: bool,
    focus_dirty: bool,
    focus_anim: Option<Instant>,
    /// The focused button's press dip, playing out over the close fade it starts.
    press: crate::ui::animation::Press,
    tc: crate::ui::text::TextCache,
    /// The `ui::theme` epoch the shell tile was baked at, so picking a Theme with the dialog
    /// up rebuilds it. This tile is hand-cached rather than going through `ui::cache`, which
    /// folds the epoch in for everything else.
    styled_at: u64,
}

impl ConfirmDialog {
    pub(super) fn new(
        title: &'static str,
        subtitle: &'static str,
        buttons: [crate::ui::widgets::ConfirmButton<'static>; 2],
    ) -> Self {
        Self {
            title,
            subtitle,
            buttons,
            focus: None,
            fade: crate::ui::fade::ModalFade::modal(),
            shell_dirty: false,
            focus_dirty: false,
            focus_anim: None,
            press: crate::ui::animation::Press::default(),
            tc: crate::ui::text::TextCache::new(),
            styled_at: 0,
        }
    }

    pub(super) fn is_open(&self) -> bool {
        self.focus.is_some()
    }

    /// Opens (or reopens) with `focus` focused, under a subtitle chosen at open time — what
    /// Quit does depends on the selected host, which is not known when the dialog is built.
    pub(super) fn open_with(&mut self, focus: usize, subtitle: &'static str) {
        self.subtitle = subtitle;
        self.open(focus);
    }

    /// Opens (or reopens) with `focus` focused.
    pub(super) fn open(&mut self, focus: usize) {
        self.focus = Some(focus);
        self.press = crate::ui::animation::Press::default();
        self.fade.reopen();
        self.shell_dirty = true;
        self.focus_dirty = true;
        self.focus_anim = Some(Instant::now());
    }

    /// Moves focus between the two buttons (Left/Right while open).
    fn set_focus(&mut self, focus: usize) {
        self.focus = Some(focus);
        self.focus_dirty = true;
        self.focus_anim = Some(Instant::now());
    }

    /// Dips the focused button, over whatever the press starts (close fade, teardown).
    fn press(&mut self) {
        self.press.arm();
    }

    /// Starts close-fade with the focused button.
    pub(super) fn dismiss(&mut self) {
        if let Some(focus) = self.focus.take() {
            self.fade.close(focus);
        }
    }

    /// Returns `(focus, alpha, is_closing)` to draw, or `None` if nothing to show.
    pub(super) fn frame(&self) -> Option<(usize, f32, bool)> {
        if let Some((alpha, focus)) = self.fade.closing_frame() {
            return Some((focus, alpha, true));
        }
        self.focus.map(|focus| (focus, self.fade.open_alpha(), false))
    }

    /// Advances the fade; `true` while either direction is still in flight.
    pub(super) fn tick(&mut self) -> bool {
        self.fade.tick()
    }

    /// Feeds one SDL event to the open dialog. Fresh presses only, so an
    /// auto-repeating held key can't run an action twice. `None` when the event
    /// isn't the dialog's; `Confirmed` doesn't dismiss — the caller decides.
    pub(super) fn handle_event(
        &mut self,
        event: &sdl2::event::Event,
        w: u32,
        h: u32,
        fonts: &crate::ui::text::Fonts,
    ) -> Option<ConfirmAction> {
        use sdl2::event::Event;
        let focus = self.focus?;
        // Magic Remote pointer: hovering a button focuses it, a click acts on it —
        // the same absolute button rects the dialog is drawn with, so it lines up
        // with what's on screen. `content` is a plain Rect (captured by copy), so
        // the closure holds no borrow of `self` that `set_focus` would collide with.
        let (_, content) = crate::ui::tiles::confirm_dialog_layout(w, h, fonts, self.subtitle);
        let button_at = |x: i32, y: i32| crate::ui::tiles::confirm_button_at(content, x, y);
        match *event {
            Event::MouseMotion { x, y, .. } => {
                return match button_at(x, y) {
                    Some(i) if i != focus => {
                        self.set_focus(i);
                        Some(ConfirmAction::Navigated)
                    }
                    _ => None,
                };
            }
            // Act on the button under the click; a click off both buttons is ignored
            // (the dialog stays open) rather than dismissing on a stray tap.
            Event::MouseButtonDown {
                mouse_btn: sdl2::mouse::MouseButton::Left,
                x,
                y,
                ..
            } => {
                let i = button_at(x, y)?;
                self.press();
                return Some(if i == 0 {
                    ConfirmAction::Confirmed
                } else {
                    self.dismiss();
                    ConfirmAction::Dismissed
                });
            }
            _ => {}
        }
        let nav = match event {
            Event::KeyDown {
                keycode: Some(k),
                repeat: false,
                ..
            } => crate::platform::webos::input::menu_event_for_key(*k),
            Event::ControllerButtonDown { button, .. } => crate::platform::webos::input::menu_event_for_button(*button),
            _ => None,
        };
        match nav {
            Some(MenuEvent::Left | MenuEvent::Right) => {
                self.set_focus(1 - focus);
                Some(ConfirmAction::Navigated)
            }
            Some(MenuEvent::Confirm) if focus == 0 => {
                self.press();
                Some(ConfirmAction::Confirmed)
            }
            Some(ev @ (MenuEvent::Confirm | MenuEvent::Back)) => {
                // Back is a dismissal, not a press — only the pressed button dips.
                if ev == MenuEvent::Confirm {
                    self.press();
                }
                self.dismiss();
                Some(ConfirmAction::Dismissed)
            }
            _ => None,
        }
    }

    /// Uploads any dirty tiles and appends this dialog's overlay (scrim + shell +
    /// popped focus button) for the current fade frame. No-op when nothing shows.
    ///
    /// `blurrable` says whether this loop's backdrop is in the framebuffer at all: the menu
    /// passes `true`, the streaming loop `false`, since NDL video lives on a hardware plane
    /// *below* the SDL surface. Whether the card is then actually frosted is the theme's
    /// answer, not the caller's.
    pub(super) fn draw(
        &mut self,
        compositor: &mut Compositor,
        texture_creator: &sdl2::render::TextureCreator<sdl2::video::WindowContext>,
        fonts: &crate::ui::text::Fonts<'_>,
        screen: crate::ui::render::Size,
        blurrable: bool,
        cmds: &mut Vec<DrawCmd>,
    ) -> Result<()> {
        let Some((focus, m, _closing)) = self.frame() else {
            return Ok(());
        };
        let (w, h) = (screen.w, screen.h);
        let full = crate::ui::render::Rect::new(0, 0, w, h);
        // The theme's glass, but only where there is a framebuffer backdrop to blur.
        let glass = blurrable.then(crate::ui::theme::glass).flatten();
        let styled_at = crate::ui::theme::epoch();
        self.shell_dirty |= std::mem::replace(&mut self.styled_at, styled_at) != styled_at;
        if self.shell_dirty {
            self.shell_dirty = false;
            let shell = crate::ui::rasterize(
                crate::ui::tiles::ConfirmDialogShellTile {
                    screen_w: w,
                    screen_h: h,
                    title: self.title,
                    subtitle: self.subtitle,
                    buttons: &self.buttons,
                    glass: glass.is_some(),
                },
                &mut self.tc,
                fonts,
            )?;
            compositor.upload(texture_creator, tile::DISCONNECT_DIALOG, &shell, false)?;
        }
        let (card, content) = crate::ui::tiles::confirm_dialog_layout(w, h, fonts, self.subtitle);
        let btn_rect = crate::ui::widgets::confirm_button_rect(content, focus);
        if self.focus_dirty {
            self.focus_dirty = false;
            let tile = crate::ui::rasterize(
                crate::ui::widgets::ConfirmButtonTile {
                    button: &self.buttons[focus],
                    w: btn_rect.width(),
                    h: btn_rect.height(),
                },
                &mut self.tc,
                fonts,
            )?;
            compositor.upload(texture_creator, tile::DISCONNECT_FOCUS_BUTTON, &tile, false)?;
        }
        // Same open/close motion as the `App`'s `Screen` modals (see `compose_modal`):
        // the shared rise, same curve in both directions, no scale.
        let dy = crate::ui::animation::modal_rise(m);
        let pad = crate::ui::tiles::ROW_TILE_PAD;
        let base = crate::ui::render::Rect::new(
            btn_rect.x() - pad,
            btn_rect.y() - pad + dy,
            btn_rect.width() + 2 * pad as u32,
            btn_rect.height() + 2 * pad as u32,
        );
        let shell_dst = crate::ui::render::Rect::new(0, dy, w, h);
        // Pane first, scrim second, shell tile third — the same order `App::compose_modal`
        // keeps, and for the same reason: the compositor captures its blur source at the
        // frame's first pane, so a scrim pushed ahead of one would be in some frames' blur and
        // not others. The shell tile above supplies the tint and the border.
        if let Some(g) = glass {
            cmds.push(DrawCmd::Frost(Box::new(crate::ui::render::FrostPane::whole(
                card.offset(0, dy),
                crate::ui::render::FrostMask {
                    radius: crate::ui::widgets::MODAL_RADIUS,
                    corners: crate::ui::render::Corners::All,
                },
                g.blur,
                (255.0 * m) as u8,
                Some(crate::ui::theme::palette().panel),
            ))));
        }
        cmds.push(DrawCmd::Fill {
            rect: full,
            color: crate::ui::render::Color::RGBA(0, 0, 0, (f32::from(crate::ui::theme::palette().scrim.a) * m) as u8),
        });
        cmds.push(DrawCmd::Tex {
            tile: tile::DISCONNECT_DIALOG,
            dst: shell_dst,
            alpha: (255.0 * m) as u8,
        });
        cmds.push(DrawCmd::Tex {
            tile: tile::DISCONNECT_FOCUS_BUTTON,
            dst: crate::ui::animation::focus_tile_rect(base, self.focus_anim, self.press),
            alpha: (255.0 * m) as u8,
        });
        Ok(())
    }
}

/// Edge-trigger bookkeeping shared by every scancode/keycode poll in both loops: `down` (with
/// whatever gating — a dialog owning input, say — the caller wants folded in already) becomes
/// `fired` exactly once per physical press, `prev` carrying state across calls. Split from
/// [`scancode_rising_edge`] so a caller that needs to gate `down` on something besides the raw
/// key state (the streaming loop's colour buttons, skipped while its disconnect dialog is open)
/// isn't left re-deriving this bookkeeping itself.
pub(super) fn rising_edge(down: bool, prev: &mut bool) -> bool {
    let fired = down && !*prev;
    *prev = down;
    fired
}

/// Rising-edge detect on a raw webOS scancode (polled since these sit outside
/// rust-sdl2's `Scancode` enum). `prev` carries the last-frame state across calls.
fn scancode_rising_edge(scancode: i32, prev: &mut bool) -> bool {
    rising_edge(crate::platform::webos::input::webos_scancode_down(scancode), prev)
}

/// Wraps webOS's on-screen keyboard (`SDL_StartTextInput`/`Stop`/`SetTextInputRect`, driving
/// `zwp_text_input_v3` — see `text_input_screen`'s doc) for the two loops that raise it:
/// `run_ui_flow` declaratively, from which screen is open, and `stream` from a button press
/// with no equivalent "wants text" screen state to read. One place for the SDL toggle-semantics
/// workaround `raise` needs, instead of two copies free to drift apart.
pub(super) struct TextInputController {
    util: sdl2::keyboard::TextInputUtil,
    /// This app's own belief about whether it last asked for text input — not necessarily
    /// what the compositor is showing right now; webOS's IME can dismiss the panel (Back)
    /// without this app hearing about it. Only [`Self::set_active`] treats this as trustworthy
    /// (it owns every off transition); [`Self::raise`] deliberately never reads it.
    active: bool,
}

impl TextInputController {
    pub(super) fn new(util: sdl2::keyboard::TextInputUtil) -> Self {
        Self { util, active: false }
    }

    pub(super) fn has_screen_keyboard_support(&self) -> bool {
        self.util.has_screen_keyboard_support()
    }

    pub(super) fn is_shown(&self, window: &sdl2::video::Window) -> bool {
        self.util.is_screen_keyboard_shown(window)
    }

    /// Declarative form (`run_ui_flow`'s): active exactly while `want` is true, a no-op unless
    /// `want` changed since the last call. `rect`, when given, anchors the panel to a text
    /// field on the way up — `run_ui_flow` skips it when the field itself isn't on screen yet.
    pub(super) fn set_active(&mut self, want: bool, rect: Option<sdl2::rect::Rect>) {
        if want == self.active {
            return;
        }
        self.active = want;
        if want {
            if let Some(r) = rect {
                self.util.set_rect(r);
            }
            self.util.start();
        } else {
            self.util.stop();
        }
        tracing::debug!("text input requested: {want}");
    }

    /// Button-triggered form (`stream`'s): unconditionally (re)raises the panel at `rect`,
    /// regardless of what this app currently believes. SDL's own idea of text input stays
    /// "started" from the first raise onward for the whole loop, even across a Back dismissal
    /// this app never hears about — so a bare `start()` on a later press is a no-op against
    /// state SDL already considers unchanged, and the panel never re-shows. `stop()`
    /// immediately before it forces a real disable→enable transition every single press.
    pub(super) fn raise(&mut self, rect: sdl2::rect::Rect) {
        self.util.set_rect(rect);
        self.util.stop();
        self.util.start();
        self.active = true;
    }

    /// Cleanup at loop exit — harmless if already stopped.
    pub(super) fn stop(&mut self) {
        self.util.stop();
        self.active = false;
    }
}

/// The webOS EXIT gesture (a held Back, delivered as `WEBOS_EXIT_SCANCODE`).
pub(super) fn exit_gesture_fired(prev: &mut bool) -> bool {
    scancode_rising_edge(crate::platform::webos::input::WEBOS_EXIT_SCANCODE, prev)
}

/// The webOS Home key (`WEBOS_HOME_SCANCODE`) — captured, so callers re-open the
/// launcher themselves via `luna::launch_home`. Distinct from EXIT, so a long Back
/// never trips it.
pub(super) fn home_key_fired(prev: &mut bool) -> bool {
    scancode_rising_edge(crate::platform::webos::input::WEBOS_HOME_SCANCODE, prev)
}

/// webOS ships a real on-screen keyboard, and the SDL fork this app links wires it
/// up (`SDL_waylandwebos_osk.c` in `webosbrew/SDL-webOS`, driving `zwp_text_input_v3`)
/// — but only for an app that actually asks for text input. Nothing here ever called
/// `SDL_StartTextInput`, so the keyboard simply never appeared on the add-host screen
/// and the only way to enter an address was the remote's number pad.
///
/// `run_ui_flow` now starts text input whenever a screen that edits text is open and
/// stops it on the way out, and `SDL_SetTextInputRect` tells webOS where the field is
/// so the panel doesn't cover it. Committed text arrives as `Event::TextInput`.
pub(super) fn text_input_screen(screen: Screen) -> bool {
    matches!(screen, Screen::AddHost | Screen::EditHost | Screen::RenameCollection)
}

/// Edge-triggers Back off `held`: a repeat/OS-resent press while already held
/// produces nothing, so a single physical press dispatches Back exactly once no
/// matter how SDL reports (or misreports) repeats for it — e.g. a *held* Back
/// would otherwise cascade through every level of menu navigation in one go
/// (closing a dropdown, then the very next repeat exiting the screen it was on)
/// instead of stopping at the first. Shared by the menu loop's keyboard and
/// controller arms, which debounce identically.
fn edge_trigger_back(ev: Option<MenuEvent>, held: &mut bool) -> Option<MenuEvent> {
    if ev != Some(MenuEvent::Back) {
        return ev;
    }
    if *held {
        None
    } else {
        *held = true;
        ev
    }
}

/// The UI loop's input state that outlives a single event: the Back debounce,
/// an in-flight hold-to-pin, and analogue-stick nav.
#[derive(Default)]
pub(super) struct UiInput {
    /// Whether a Back-mapped key/button is currently held, per the
    /// keyboard/gamepad event stream — edge-detected so a single physical press
    /// dispatches Back exactly once no matter how SDL reports (or misreports)
    /// repeats for it.
    menu_back_down: bool,
    /// Hold-to-pin on Home (see `CARD_HOLD`), while OK is held on a pinnable card.
    pub(super) card_held: Option<CardHold>,
    stick_nav: crate::platform::webos::input::StickMenuNav,
    /// When the last wheel detent arrived, for the [`WHEEL_MOTION_GUARD`] window.
    wheel_at: Option<Instant>,
}

/// How long the wheel owns focus after a detent, pointer motion ignored. Scrolling
/// emits motion at the same time — the Magic Remote keeps moving while its wheel
/// turns, and a hand on a HID mouse never holds still — which would hand focus back
/// to whatever row is under the cursor and undo the scroll. Distance can't separate
/// the two (a real mouse move is arbitrarily large), so the wheel just wins for long
/// enough to cover the pause between detents; a click ends the window early.
const WHEEL_MOTION_GUARD: Duration = Duration::from_millis(500);

/// What the UI loop should do with the event `handle_ui_event` just consumed.
pub(super) enum EventAction {
    /// Handled — carry on with the next event.
    Next,
    /// A launch is under way; leave the UI flow.
    Launch,
}

/// Starts a hold on the focused grid card, if the focus is on one. Both the pointer press
/// and the OK press arm through here so the two gestures can never disagree about what a
/// hold is; `screen_w` is the full screen width, the sidebar taken off inside.
fn arm_card_hold(input: &mut UiInput, app: &App, screen_w: u32) -> bool {
    let columns = crate::app::view::home::grid_columns_for_screen(screen_w);
    if app.focused_pin_id(columns).is_none() {
        return false;
    }
    input.card_held = Some(CardHold {
        since: Instant::now(),
        focus: app.home_focus,
        fired: false,
    });
    true
}

/// Hold-to-pin arbitration (see `CARD_HOLD`). `MenuEvent` has no press/release
/// notion, so the gesture works off raw SDL events: OK down on a pinnable Home
/// card starts the hold and is swallowed, and the launch can only ever come
/// from the release. `Some` means the event was the gesture's and goes no
/// further.
fn card_hold_gate(
    app: &mut App,
    event: &sdl2::event::Event,
    input: &mut UiInput,
    display_mode: sdl2::video::DisplayMode,
    fonts: &crate::ui::text::Fonts,
    dirty: &mut bool,
) -> Option<EventAction> {
    use sdl2::event::Event;
    let (w, h) = (display_mode.w as u32, display_mode.h as u32);
    // The Magic Remote's pointer delivers OK as a left mouse button, so give it the same
    // hold gesture the D-pad's Confirm has: a press on a hovered Home card starts the hold
    // and is swallowed (the card's menu opens on the hold-elapsed tick, same as `CARD_HOLD`
    // above), and the tap/launch comes only from the release. A press on anything else falls
    // through to the normal click path.
    if let Event::MouseButtonDown {
        mouse_btn: sdl2::mouse::MouseButton::Left,
        x,
        y,
        ..
    } = *event
    {
        // A press while the menu is up belongs to the menu, not to a fresh gesture — the
        // hold fires on elapsed (see `ui_flow`'s `CARD_HOLD` check), so the panel is already
        // open with OK still down, and re-arming here would re-open it from row 0.
        if !matches!(app.nav.screen, Screen::Home) || app.card_menu.is_some() {
            return None;
        }
        // Land hover focus on the press point first — a button press can jostle the
        // remote off the last motion position.
        *dirty |= app.handle_mouse_motion(x, y, w, h, fonts);
        if input.card_held.is_some() {
            return Some(EventAction::Next);
        }
        if arm_card_hold(input, app, w) {
            return Some(EventAction::Next);
        }
        return None;
    }
    // Release of a pointer OK: resolve whatever the matching press started. A fired hold has
    // already opened the card's menu (swallow); a quick tap confirms whatever's under the
    // pointer now, exactly as an immediate click would have.
    if let Event::MouseButtonUp {
        mouse_btn: sdl2::mouse::MouseButton::Left,
        x,
        y,
        ..
    } = *event
    {
        let hold = input.card_held.take()?;
        *dirty = true;
        if hold.fired {
            return Some(EventAction::Next);
        }
        return Some(if app.handle_mouse_click(x, y, w, h, fonts).is_some() {
            EventAction::Launch
        } else {
            EventAction::Next
        });
    }
    // No `repeat: false` filter, deliberately — OS auto-repeats while OK is held
    // have to be caught here too, not dispatched as fresh presses.
    let confirm_down = matches!(
        *event,
        Event::KeyDown { keycode: Some(k), .. }
            if crate::platform::webos::input::menu_event_for_key(k) == Some(MenuEvent::Confirm)
    ) || matches!(
        *event,
        Event::ControllerButtonDown { button, .. }
            if crate::platform::webos::input::menu_event_for_button(button) == Some(MenuEvent::Confirm)
    );
    if confirm_down {
        // OK stays the gesture's until released, whatever the hold put on screen: the
        // card menu opens *under the still-held button*, and the next auto-repeat KeyDown
        // would otherwise dispatch Confirm straight into it.
        if input.card_held.is_some() {
            return Some(EventAction::Next);
        }
        if matches!(app.nav.screen, Screen::Home)
            && app.card_menu.is_none()
            && arm_card_hold(input, app, display_mode.w as u32)
        {
            return Some(EventAction::Next);
        }
        return None;
    }
    let ends_hold = matches!(
        *event,
        Event::KeyUp { keycode: Some(k), .. }
            if crate::platform::webos::input::menu_event_for_key(k) == Some(MenuEvent::Confirm)
    ) || matches!(
        *event,
        Event::ControllerButtonUp { button, .. }
            if crate::platform::webos::input::menu_event_for_button(button) == Some(MenuEvent::Confirm)
    );
    // This press was ours (tap or hold) — swallow the release.
    let hold = ends_hold.then(|| input.card_held.take()).flatten()?;
    *dirty = true;
    // A quick tap: the press never dispatched, so do it now. A hold that already opened its
    // menu, or one whose screen/focus moved out from under it, resolves to nothing.
    let tapped = !hold.fired && matches!(app.nav.screen, Screen::Home) && hold.focus == app.home_focus;
    let launched = tapped && app.press(display_mode.w as u32, display_mode.h as u32, fonts).is_some();
    Some(if launched {
        EventAction::Launch
    } else {
        EventAction::Next
    })
}

/// Feeds a resolved `MenuEvent` to the app, translating what it returns into this
/// loop's terms. The per-screen routing is `App::handle_menu_event`.
fn dispatch_menu_event(
    app: &mut App,
    menu_ev: MenuEvent,
    display_mode: sdl2::video::DisplayMode,
    fonts: &crate::ui::text::Fonts,
) -> EventAction {
    let (w, h) = (display_mode.w as u32, display_mode.h as u32);
    if menu_ev == MenuEvent::Back {
        return if app.back(w, h, fonts).is_some() {
            EventAction::Launch
        } else {
            EventAction::Next
        };
    }
    // Confirm goes through the press animation; everything else dispatches straight.
    let launched = if menu_ev == MenuEvent::Confirm {
        app.press(w, h, fonts)
    } else {
        app.handle_menu_event(menu_ev, w, h, fonts)
    };
    if launched.is_some() {
        return EventAction::Launch;
    }
    EventAction::Next
}

/// One SDL event from the pre-stream UI's pump, routed into `app`. `dirty` is
/// set whenever the event can have changed what's on screen. Device-level
/// events (quit, controller hotplug) are the caller's and never arrive here.
// Takes the event by value: it comes straight off `poll_iter`, and the `match` arms below read
// cleaner destructuring an owned event than reborrowing every payload out of a reference.
#[allow(clippy::needless_pass_by_value)]
pub(super) fn handle_ui_event(
    app: &mut App,
    event: sdl2::event::Event,
    input: &mut UiInput,
    display_mode: sdl2::video::DisplayMode,
    fonts: &crate::ui::text::Fonts,
    dirty: &mut bool,
) -> EventAction {
    use sdl2::event::Event;
    let (w, h) = (display_mode.w as u32, display_mode.h as u32);
    // The Magic Remote's pointer mode surfaces as a plain SDL2 MouseMotion
    // event fired continuously while the remote is moving — unlike every other
    // event handled below, redraw only if the motion actually changed the
    // focused/hovered element, not on every no-op tick.
    if let Event::MouseMotion { x, y, .. } = event {
        if !input.wheel_at.is_some_and(|t| t.elapsed() < WHEEL_MOTION_GUARD) {
            *dirty |= app.handle_mouse_motion(x, y, w, h, fonts);
        }
        return EventAction::Next;
    }
    // The Magic Remote's scroll wheel — scrolls the game grid on Home (wheel
    // y > 0 = "scroll up" = content moves down). Like motion above, only
    // redraws when the offset actually moved (a wheel tick at either clamp
    // edge is a no-op).
    if let Event::MouseWheel { y: wheel_y, .. } = event {
        input.wheel_at = Some(Instant::now());
        match app.nav.screen {
            Screen::About => {
                /// Licence-wall px per wheel detent — a few lines at a time.
                const ABOUT_WHEEL_STEP: i32 = 90;
                *dirty |= app.scroll_about_by(-wheel_y * ABOUT_WHEEL_STEP, w, h, fonts);
            }
            Screen::Home => {
                /// Grid px scrolled per wheel detent — about a third of a card
                /// row, so a few ticks walk one row.
                const WHEEL_STEP: i32 = 120;
                *dirty |= app.scroll_grid_by(-wheel_y * WHEEL_STEP, w, h);
            }
            // List-modal screens (row-per-page, not pixel scroll): one detent
            // moves focus exactly one row, same as an Up/Down key press.
            Screen::Settings(_)
            | Screen::Collections
            | Screen::HostMenu
            | Screen::HostPower
            | Screen::Diagnostics
            | Screen::Experimental
            | Screen::CursorSettings(_)
                if wheel_y != 0 =>
            {
                let menu_ev = if wheel_y > 0 { MenuEvent::Up } else { MenuEvent::Down };
                dispatch_menu_event(app, menu_ev, display_mode, fonts);
            }
            _ => {}
        }
        return EventAction::Next;
    }
    // A click is deliberate input: it takes focus at the press point regardless of a
    // recent wheel detent, so drop the guard before either press path resolves hover focus.
    if matches!(event, Event::MouseButtonDown { .. }) {
        input.wheel_at = None;
    }
    if let Some(action) = card_hold_gate(app, &event, input, display_mode, fonts, dirty) {
        return action;
    }
    // Any other event might change what's on screen (focus/hover, a typed
    // digit, a screen transition) — simplest to mark dirty for all of them
    // rather than re-litigate that per event kind.
    *dirty = true;
    match event {
        // The Magic Remote's pointer delivers OK as a plain mouse click.
        // Dispatch it on press: there is no hold gesture to disambiguate any
        // more (per-host actions have their own ⋯ button — see
        // `ui::widgets::sidebar_menu_button_rect`), so nothing needs to wait for the
        // release.
        Event::MouseButtonDown {
            mouse_btn: sdl2::mouse::MouseButton::Left,
            x,
            y,
            ..
        } => {
            // A grid-card click resolves via `confirm_grid_card`'s async check,
            // same as a remote Confirm — never a target directly here.
            return if app.handle_mouse_click(x, y, w, h, fonts).is_some() {
                EventAction::Launch
            } else {
                EventAction::Next
            };
        }
        // Ends a Bitrate drag armed in `handle_mouse_click` — without this the slider
        // would keep tracking the pointer (or a stale last position) past the release.
        Event::MouseButtonUp {
            mouse_btn: sdl2::mouse::MouseButton::Left,
            ..
        } => {
            app.end_slider_drag();
        }
        // Direct digit entry via the remote's number buttons — PIN entry on the
        // pairing screen, IP entry on the add/edit-host screens.
        Event::KeyDown { keycode: Some(k), .. }
            if matches!(
                app.nav.screen,
                Screen::Pairing | Screen::AddHost | Screen::EditHost | Screen::RenameCollection
            ) =>
        {
            if let Some(digit) = crate::platform::webos::input::digit_key_value(k) {
                match app.nav.screen {
                    Screen::Pairing => app.enter_pin_digit(digit),
                    Screen::AddHost | Screen::EditHost => app.enter_add_host_digit(digit),
                    // A digit is an ordinary character in a name.
                    Screen::RenameCollection => {
                        app.enter_collection_name_char((b'0' + digit) as char);
                    }
                    _ => unreachable!(),
                }
                return EventAction::Next;
            }
            // Backspace is a *text* key here, not navigation: `menu_event_for_key` maps it to
            // Back (a remote whose Back arrives as Backspace still has to work), which would
            // close the modal on every attempt to correct a typo — from a USB keyboard and
            // from webOS's on-screen keyboard alike, since the OSK's erase key is delivered
            // as a synthetic Backspace rather than as `TextInput`. Consumed only when the
            // screen had something to erase, so on such a remote Backspace still leaves an
            // empty field the way Back does.
            if k == sdl2::keyboard::Keycode::Backspace && app.erase_text_entry() {
                return EventAction::Next;
            }
        }
        // Text committed by webOS's on-screen keyboard (see `SOFTWARE_KEYBOARD`
        // in this module): the OSK delivers whole strings via SDL_TEXTINPUT, not
        // synthetic key events, so it has to be consumed separately from the
        // number-pad path above. Each character is fed through the same entry
        // state machine, so typing "192.168.1.5" on the keyboard and tapping it
        // out on the remote produce identical results.
        Event::TextInput { ref text, .. } => {
            match app.nav.screen {
                Screen::Pairing => {
                    for d in text.chars().filter_map(|c| c.to_digit(10)) {
                        app.enter_pin_digit(d as u8);
                    }
                }
                Screen::AddHost | Screen::EditHost => {
                    for c in text.chars() {
                        app.enter_host_address_char(c);
                    }
                }
                Screen::RenameCollection => {
                    for c in text.chars() {
                        app.enter_collection_name_char(c);
                    }
                }
                _ => {}
            }
            return EventAction::Next;
        }
        _ => {}
    }
    let menu_ev = match event {
        Event::KeyDown { keycode: Some(k), .. } => edge_trigger_back(
            crate::platform::webos::input::menu_event_for_key(k),
            &mut input.menu_back_down,
        ),
        Event::KeyUp { keycode: Some(k), .. } => {
            if crate::platform::webos::input::menu_event_for_key(k) == Some(MenuEvent::Back) {
                input.menu_back_down = false;
            }
            None
        }
        Event::ControllerButtonDown { button, .. } => edge_trigger_back(
            crate::platform::webos::input::menu_event_for_button(button),
            &mut input.menu_back_down,
        ),
        Event::ControllerButtonUp { button, .. } => {
            if crate::platform::webos::input::menu_event_for_button(button) == Some(MenuEvent::Back) {
                input.menu_back_down = false;
            }
            None
        }
        Event::ControllerAxisMotion { axis, value, .. } => input.stick_nav.axis_event(axis, value),
        _ => None,
    };
    let Some(menu_ev) = menu_ev else {
        return EventAction::Next;
    };
    dispatch_menu_event(app, menu_ev, display_mode, fonts)
}
