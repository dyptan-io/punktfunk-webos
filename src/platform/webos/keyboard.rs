//! Maps SDL2 keyboard events to punktfunk wire `InputEvents`.
//! Magic Remote 5-way pad → arrow keys. USB keyboards → QWERTY-positional VKs.
use punktfunk_core::input::{InputEvent, InputKind};
use sdl2::keyboard::Scancode;

/// SDL2 scancode → Windows VK code (`vk_to_evdev` on the host side).
/// `None` for keys not in that table.
fn vk_code(sc: Scancode) -> Option<u32> {
    Some(match sc {
        // ── Navigation / editing / whitespace ────────────────────────────────
        Scancode::Backspace => 0x08,                  // VK_BACK
        Scancode::Tab => 0x09,                        // VK_TAB
        Scancode::Return | Scancode::KpEnter => 0x0D, // VK_RETURN (numpad enter too)
        Scancode::Pause => 0x13,                      // VK_PAUSE
        Scancode::CapsLock => 0x14,                   // VK_CAPITAL
        Scancode::Escape => 0x1B,                     // VK_ESCAPE
        Scancode::Space => 0x20,                      // VK_SPACE
        Scancode::PageUp => 0x21,                     // VK_PRIOR
        Scancode::PageDown => 0x22,                   // VK_NEXT
        Scancode::End => 0x23,                        // VK_END
        Scancode::Home => 0x24,                       // VK_HOME
        Scancode::Left => 0x25,                       // VK_LEFT
        Scancode::Up => 0x26,                         // VK_UP
        Scancode::Right => 0x27,                      // VK_RIGHT
        Scancode::Down => 0x28,                       // VK_DOWN
        Scancode::PrintScreen => 0x2C,                // VK_SNAPSHOT
        Scancode::Insert => 0x2D,                     // VK_INSERT
        Scancode::Delete => 0x2E,                     // VK_DELETE

        // ── Digit row ─────────────────────────────────────────────────────────
        Scancode::Num0 => 0x30, // VK_0
        Scancode::Num1 => 0x31, // VK_1
        Scancode::Num2 => 0x32, // VK_2
        Scancode::Num3 => 0x33, // VK_3
        Scancode::Num4 => 0x34, // VK_4
        Scancode::Num5 => 0x35, // VK_5
        Scancode::Num6 => 0x36, // VK_6
        Scancode::Num7 => 0x37, // VK_7
        Scancode::Num8 => 0x38, // VK_8
        Scancode::Num9 => 0x39, // VK_9

        // ── Letters A–Z (QWERTY positional) ──────────────────────────────────
        Scancode::A => 0x41,
        Scancode::B => 0x42,
        Scancode::C => 0x43,
        Scancode::D => 0x44,
        Scancode::E => 0x45,
        Scancode::F => 0x46,
        Scancode::G => 0x47,
        Scancode::H => 0x48,
        Scancode::I => 0x49,
        Scancode::J => 0x4A,
        Scancode::K => 0x4B,
        Scancode::L => 0x4C,
        Scancode::M => 0x4D,
        Scancode::N => 0x4E,
        Scancode::O => 0x4F,
        Scancode::P => 0x50,
        Scancode::Q => 0x51,
        Scancode::R => 0x52,
        Scancode::S => 0x53,
        Scancode::T => 0x54,
        Scancode::U => 0x55,
        Scancode::V => 0x56,
        Scancode::W => 0x57,
        Scancode::X => 0x58,
        Scancode::Y => 0x59,
        Scancode::Z => 0x5A,

        // ── Meta / context-menu ───────────────────────────────────────────────
        Scancode::LGui => 0x5B,        // VK_LWIN
        Scancode::RGui => 0x5C,        // VK_RWIN
        Scancode::Application => 0x5D, // VK_APPS

        // ── Numpad ────────────────────────────────────────────────────────────
        Scancode::Kp0 => 0x60,        // VK_NUMPAD0
        Scancode::Kp1 => 0x61,        // VK_NUMPAD1
        Scancode::Kp2 => 0x62,        // VK_NUMPAD2
        Scancode::Kp3 => 0x63,        // VK_NUMPAD3
        Scancode::Kp4 => 0x64,        // VK_NUMPAD4
        Scancode::Kp5 => 0x65,        // VK_NUMPAD5
        Scancode::Kp6 => 0x66,        // VK_NUMPAD6
        Scancode::Kp7 => 0x67,        // VK_NUMPAD7
        Scancode::Kp8 => 0x68,        // VK_NUMPAD8
        Scancode::Kp9 => 0x69,        // VK_NUMPAD9
        Scancode::KpMultiply => 0x6A, // VK_MULTIPLY
        Scancode::KpPlus => 0x6B,     // VK_ADD
        Scancode::KpMinus => 0x6D,    // VK_SUBTRACT
        Scancode::KpPeriod => 0x6E,   // VK_DECIMAL
        Scancode::KpDivide => 0x6F,   // VK_DIVIDE

        // ── Function keys ─────────────────────────────────────────────────────
        Scancode::F1 => 0x70,
        Scancode::F2 => 0x71,
        Scancode::F3 => 0x72,
        Scancode::F4 => 0x73,
        Scancode::F5 => 0x74,
        Scancode::F6 => 0x75,
        Scancode::F7 => 0x76,
        Scancode::F8 => 0x77,
        Scancode::F9 => 0x78,
        Scancode::F10 => 0x79,
        Scancode::F11 => 0x7A,
        Scancode::F12 => 0x7B,

        // ── Lock keys ─────────────────────────────────────────────────────────
        Scancode::NumLockClear => 0x90, // VK_NUMLOCK
        Scancode::ScrollLock => 0x91,   // VK_SCROLL

        // ── Sided modifiers ───────────────────────────────────────────────────
        Scancode::LShift => 0xA0, // VK_LSHIFT
        Scancode::RShift => 0xA1, // VK_RSHIFT
        Scancode::LCtrl => 0xA2,  // VK_LCONTROL
        Scancode::RCtrl => 0xA3,  // VK_RCONTROL
        Scancode::LAlt => 0xA4,   // VK_LMENU
        Scancode::RAlt => 0xA5,   // VK_RMENU

        // ── OEM punctuation (US layout positions) ─────────────────────────────
        Scancode::Semicolon => 0xBA,      // VK_OEM_1      ;:
        Scancode::Equals => 0xBB,         // VK_OEM_PLUS   =+
        Scancode::Comma => 0xBC,          // VK_OEM_COMMA  ,<
        Scancode::Minus => 0xBD,          // VK_OEM_MINUS  -_
        Scancode::Period => 0xBE,         // VK_OEM_PERIOD .>
        Scancode::Slash => 0xBF,          // VK_OEM_2      /?
        Scancode::Grave => 0xC0,          // VK_OEM_3      `~
        Scancode::LeftBracket => 0xDB,    // VK_OEM_4      [{
        Scancode::Backslash => 0xDC,      // VK_OEM_5      \|
        Scancode::RightBracket => 0xDD,   // VK_OEM_6      ]}
        Scancode::Apostrophe => 0xDE,     // VK_OEM_7      '"
        Scancode::NonUsBackslash => 0xE2, // VK_OEM_102    ISO extra key

        _ => return None,
    })
}

/// Linux evdev `KEY_*` → the same Windows VK `vk_code` produces from an SDL scancode.
/// `None` for keys the host has no mapping for (media-only, TV-remote, unmapped).
pub fn vk_from_evdev(code: u16) -> Option<u32> {
    Some(match code {
        1 => 0x1B,   // KEY_ESC
        2 => 0x31,   // KEY_1
        3 => 0x32,   // KEY_2
        4 => 0x33,   // KEY_3
        5 => 0x34,   // KEY_4
        6 => 0x35,   // KEY_5
        7 => 0x36,   // KEY_6
        8 => 0x37,   // KEY_7
        9 => 0x38,   // KEY_8
        10 => 0x39,  // KEY_9
        11 => 0x30,  // KEY_0
        12 => 0xBD,  // KEY_MINUS
        13 => 0xBB,  // KEY_EQUAL
        14 => 0x08,  // KEY_BACKSPACE
        15 => 0x09,  // KEY_TAB
        16 => 0x51,  // KEY_Q
        17 => 0x57,  // KEY_W
        18 => 0x45,  // KEY_E
        19 => 0x52,  // KEY_R
        20 => 0x54,  // KEY_T
        21 => 0x59,  // KEY_Y
        22 => 0x55,  // KEY_U
        23 => 0x49,  // KEY_I
        24 => 0x4F,  // KEY_O
        25 => 0x50,  // KEY_P
        26 => 0xDB,  // KEY_LEFTBRACE
        27 => 0xDD,  // KEY_RIGHTBRACE
        28 | 96 => 0x0D, // KEY_ENTER / KEY_KPENTER
        29 => 0xA2,  // KEY_LEFTCTRL
        30 => 0x41,  // KEY_A
        31 => 0x53,  // KEY_S
        32 => 0x44,  // KEY_D
        33 => 0x46,  // KEY_F
        34 => 0x47,  // KEY_G
        35 => 0x48,  // KEY_H
        36 => 0x4A,  // KEY_J
        37 => 0x4B,  // KEY_K
        38 => 0x4C,  // KEY_L
        39 => 0xBA,  // KEY_SEMICOLON
        40 => 0xDE,  // KEY_APOSTROPHE
        41 => 0xC0,  // KEY_GRAVE
        42 => 0xA0,  // KEY_LEFTSHIFT
        43 => 0xDC,  // KEY_BACKSLASH
        44 => 0x5A,  // KEY_Z
        45 => 0x58,  // KEY_X
        46 => 0x43,  // KEY_C
        47 => 0x56,  // KEY_V
        48 => 0x42,  // KEY_B
        49 => 0x4E,  // KEY_N
        50 => 0x4D,  // KEY_M
        51 => 0xBC,  // KEY_COMMA
        52 => 0xBE,  // KEY_DOT
        53 => 0xBF,  // KEY_SLASH
        54 => 0xA1,  // KEY_RIGHTSHIFT
        55 => 0x6A,  // KEY_KPASTERISK
        56 => 0xA4,  // KEY_LEFTALT
        57 => 0x20,  // KEY_SPACE
        58 => 0x14,  // KEY_CAPSLOCK
        59 => 0x70,  // KEY_F1
        60 => 0x71,  // KEY_F2
        61 => 0x72,  // KEY_F3
        62 => 0x73,  // KEY_F4
        63 => 0x74,  // KEY_F5
        64 => 0x75,  // KEY_F6
        65 => 0x76,  // KEY_F7
        66 => 0x77,  // KEY_F8
        67 => 0x78,  // KEY_F9
        68 => 0x79,  // KEY_F10
        69 => 0x90,  // KEY_NUMLOCK
        70 => 0x91,  // KEY_SCROLLLOCK
        71 => 0x67,  // KEY_KP7
        72 => 0x68,  // KEY_KP8
        73 => 0x69,  // KEY_KP9
        74 => 0x6D,  // KEY_KPMINUS
        75 => 0x64,  // KEY_KP4
        76 => 0x65,  // KEY_KP5
        77 => 0x66,  // KEY_KP6
        78 => 0x6B,  // KEY_KPPLUS
        79 => 0x61,  // KEY_KP1
        80 => 0x62,  // KEY_KP2
        81 => 0x63,  // KEY_KP3
        82 => 0x60,  // KEY_KP0
        83 => 0x6E,  // KEY_KPDOT
        86 => 0xE2,  // KEY_102ND
        87 => 0x7A,  // KEY_F11
        88 => 0x7B,  // KEY_F12
        97 => 0xA3,  // KEY_RIGHTCTRL
        98 => 0x6F,  // KEY_KPSLASH
        99 => 0x2C,  // KEY_SYSRQ
        100 => 0xA5, // KEY_RIGHTALT
        102 => 0x24, // KEY_HOME
        103 => 0x26, // KEY_UP
        104 => 0x21, // KEY_PAGEUP
        105 => 0x25, // KEY_LEFT
        106 => 0x27, // KEY_RIGHT
        107 => 0x23, // KEY_END
        108 => 0x28, // KEY_DOWN
        109 => 0x22, // KEY_PAGEDOWN
        110 => 0x2D, // KEY_INSERT
        111 => 0x2E, // KEY_DELETE
        119 => 0x13, // KEY_PAUSE
        125 => 0x5B, // KEY_LEFTMETA
        126 => 0x5C, // KEY_RIGHTMETA
        127 => 0x5D, // KEY_COMPOSE / KEY_MENU
        _ => return None,
    })
}

pub fn key_event(scancode: Scancode, pressed: bool) -> Option<InputEvent> {
    Some(raw_key_event(vk_code(scancode)?, pressed))
}

/// Same wire event from an already-mapped VK — for [`super::evmouse`], whose keys come
/// off evdev and never pass through an SDL scancode.
pub fn raw_key_event(code: u32, pressed: bool) -> InputEvent {
    InputEvent {
        kind: if pressed { InputKind::KeyDown } else { InputKind::KeyUp },
        _pad: [0; 3],
        code,
        x: 0,
        y: 0,
        flags: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::vk_from_evdev;

    #[test]
    fn modifiers_and_letters_match_windows_vks() {
        assert_eq!(vk_from_evdev(29), Some(0xA2)); // KEY_LEFTCTRL → VK_LCONTROL
        assert_eq!(vk_from_evdev(42), Some(0xA0)); // KEY_LEFTSHIFT
        assert_eq!(vk_from_evdev(56), Some(0xA4)); // KEY_LEFTALT
        assert_eq!(vk_from_evdev(97), Some(0xA3)); // KEY_RIGHTCTRL
        assert_eq!(vk_from_evdev(30), Some(0x41)); // KEY_A
        assert_eq!(vk_from_evdev(57), Some(0x20)); // KEY_SPACE
        assert_eq!(vk_from_evdev(1), Some(0x1B)); // KEY_ESC
        assert_eq!(vk_from_evdev(0x110), None); // BTN_LEFT is not a key
    }
}
