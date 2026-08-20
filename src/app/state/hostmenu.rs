//! The per-host actions menu — logic. Rendering lives in `app::view::hostmenu`.
//!
//! Adding an action here is deliberately two edits and nothing else: a row in
//! [`App::host_menu_actions`] and an arm in [`App::confirm_host_menu_row`]. Everything
//! else — card geometry, the unfocused shell, the focused-row tile, the focus pop — is
//! `ui::widgets::ListModal`'s, shared with any future list screen.
use crate::app::hosts::HostEntry;
use crate::app::App;
use crate::core::event::MenuEvent;
use crate::core::screen::Screen;
use crate::ui::widgets::FocusRow;
use std::time::Instant;

/// Host action (enum instead of bare index so conditional rows don't silently shift indices).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum HostAction {
    Connect,
    Pair,
    SpeedTest,
    Wake,
    Edit,
    Forget,
}

/// Every action the menu can offer — the capacity [`HostActions`] is sized to.
const MAX_HOST_ACTIONS: usize = 6;

/// The actions one host's menu offers, in display order.
///
/// A fixed array rather than a `Vec`: the row count is what the card's height is measured
/// from, so the pointer path asks for this on every Magic Remote `MouseMotion`, and an
/// allocation per motion event is what it cost when the labels came with it.
pub(crate) struct HostActions {
    items: [HostAction; MAX_HOST_ACTIONS],
    len: usize,
}

impl HostActions {
    fn push(&mut self, action: HostAction) {
        self.items[self.len] = action;
        self.len += 1;
    }
}

impl Default for HostActions {
    fn default() -> Self {
        Self {
            items: [HostAction::Connect; MAX_HOST_ACTIONS],
            len: 0,
        }
    }
}

impl std::ops::Deref for HostActions {
    type Target = [HostAction];

    fn deref(&self) -> &[HostAction] {
        &self.items[..self.len]
    }
}

/// One action's row. Split from [`App::host_menu_actions`] so the geometry path can have the
/// list's shape without its labels — every label here is an owned `String` in the `FocusRow`.
/// `paired` is the host's pairing state: the only thing a label varies on.
fn host_menu_row(action: HostAction, paired: bool) -> FocusRow {
    use crate::app::view::icons;
    match action {
        HostAction::Connect if paired => FocusRow::action(icons::ICON_TV, "Connect"),
        // The hint goes in the value column like every other Action row's, rather than
        // being parenthesised into the label.
        HostAction::Connect => FocusRow::action_with_value(icons::ICON_TV, "Connect", "pairs first"),
        HostAction::Pair => FocusRow::action(icons::ICON_LOCK, "Pair with PIN…"),
        HostAction::SpeedTest => FocusRow::action(icons::ICON_SIGNAL, "Test network speed…"),
        // The one row with a ⋯: Confirm wakes now, the button holds the per-host wake
        // settings (`Screen::WakeSettings`). Same affordance and the same Right-to-reach-it
        // gesture as a sidebar host row's. Always built *un*focused — whether the button is
        // lit is `host_menu_dots`, applied by the focused-row tile alone (see
        // `App::modal_focus_tile`), so the shell underneath can't bake in a highlight that
        // outlives it.
        HostAction::Wake => FocusRow::action(icons::ICON_POWER, "Wake host").with_menu(false),
        HostAction::Edit => FocusRow::action(icons::ICON_EDIT, "Edit address…"),
        HostAction::Forget => FocusRow::action(icons::ICON_DELETE, "Forget host").danger(),
    }
}

impl App {
    /// The action rows, as the view paints them.
    pub(crate) fn host_menu_rows(&self) -> Vec<FocusRow> {
        let paired = self.host_menu_paired();
        self.host_menu_actions()
            .iter()
            .map(|&a| host_menu_row(a, paired))
            .collect()
    }

    /// Whether the menu's host is paired — what half the actions are conditional on, and
    /// the one thing a row label varies on.
    pub(crate) fn host_menu_paired(&self) -> bool {
        self.host_menu_index
            .and_then(|i| self.entries.get(i))
            .is_some_and(HostEntry::is_paired)
    }

    /// Opens host menu for sidebar row `idx` (⋯ button, pointer, or Right key).
    pub(crate) fn open_host_menu(&mut self, idx: usize) {
        self.host_menu_index = Some(idx);
        self.menu_focused = 0;
        self.host_menu_dots = false;
        self.screen = Screen::HostMenu;
    }

    /// Whether focused row's ⋯ button exists (only "Wake host" has one).
    pub(crate) fn host_menu_row_has_dots(&self) -> bool {
        self.host_menu_actions().get(self.menu_focused) == Some(&HostAction::Wake)
    }

    /// The actions offered; conditional on host state (saved/discovered, has MAC).
    pub(crate) fn host_menu_actions(&self) -> HostActions {
        let mut actions = HostActions::default();
        let Some(entry) = self.host_menu_index.and_then(|i| self.entries.get(i)) else {
            return actions;
        };
        let saved = matches!(entry, HostEntry::Known(_));
        let paired = entry.is_paired();
        actions.push(HostAction::Connect);
        actions.push(HostAction::Pair);
        // Both this and "Wake host" below need a paired host: the probe runs over the real
        // data plane (so it needs the host to accept this client's certificate), and waking a
        // host we can't then connect to is a dead end.
        if paired {
            actions.push(HostAction::SpeedTest);
        }
        if paired && !entry.mac().is_empty() {
            actions.push(HostAction::Wake);
        }
        if saved {
            actions.push(HostAction::Edit);
            actions.push(HostAction::Forget);
        }
        actions
    }

    /// The host's name — the menu's title.
    pub(crate) fn host_menu_title(&self) -> String {
        self.host_menu_index
            .and_then(|i| self.entries.get(i))
            .map_or_else(String::new, |e| e.name().to_string())
    }

    /// `address:port` and the pairing state — the menu's subtitle.
    pub(crate) fn host_menu_subtitle(&self) -> String {
        self.host_menu_index
            .and_then(|i| self.entries.get(i))
            .map_or_else(String::new, |e| {
                let paired = if e.is_paired() { "paired" } else { "not paired" };
                format!("{}:{} · {paired}", e.host(), e.port())
            })
    }

    /// Handles host menu events.
    pub(crate) fn handle_host_menu_event(&mut self, ev: MenuEvent) {
        let len = self.host_menu_actions().len();
        if crate::ui::widgets::list_nav(&mut self.menu_focused, len, crate::app::menu::nav_dir(ev)) {
            // Vertical movement always lands on the row body — a ⋯ belongs to the row
            // it's on, so leaving that row leaves the button too.
            self.host_menu_dots = false;
            self.modal.focus_anim = Some(Instant::now());
            return;
        }
        match ev {
            // Right/Left move onto and off the focused row's ⋯, mirroring the sidebar's
            // `HomeFocus::SidebarMenu`; on a row without one they do nothing.
            MenuEvent::Right if !self.host_menu_dots && self.host_menu_row_has_dots() => {
                self.host_menu_dots = true;
                self.modal.focus_anim = Some(Instant::now());
            }
            MenuEvent::Left if self.host_menu_dots => {
                self.host_menu_dots = false;
                self.modal.focus_anim = Some(Instant::now());
            }
            MenuEvent::Confirm if self.host_menu_dots => self.open_wake_settings(),
            MenuEvent::Confirm => self.confirm_host_menu_row(),
            MenuEvent::Back => {
                self.host_menu_index = None;
                self.screen = Screen::Home;
            }
            MenuEvent::Up | MenuEvent::Down | MenuEvent::Left | MenuEvent::Right | MenuEvent::Secondary => {}
        }
    }

    /// Runs focused row's action; every arm navigates away or closes menu.
    pub(crate) fn confirm_host_menu_row(&mut self) {
        let actions = self.host_menu_actions();
        let Some(action) = actions.get(self.menu_focused) else {
            return;
        };
        let Some(idx) = self.host_menu_index else { return };
        match action {
            HostAction::Connect => {
                self.host_menu_index = None;
                self.screen = Screen::Home;
                self.confirm_sidebar_host(idx);
            }
            // Straight to the PIN ceremony, even for an already-paired host: re-pairing
            // is the documented recovery when a host's certificate has changed.
            HostAction::Pair => {
                self.host_menu_index = None;
                self.open_pairing(idx);
            }
            HostAction::SpeedTest => self.open_speed_test(idx),
            HostAction::Wake => {
                let Some(entry) = self.entries.get(idx) else { return };
                let (host, port) = (entry.host().to_string(), entry.port());
                let mac = entry.mac().to_vec();
                let name = entry.name().to_string();
                self.host_menu_index = None;
                self.screen = Screen::Home;
                self.start_wake(host, port, mac, format!("Waking {name}…"));
            }
            HostAction::Edit => self.open_edit_host(idx),
            HostAction::Forget => self.open_forget_host(idx),
        }
    }
}
