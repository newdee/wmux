//! Logical keys: tmux-style names (`C-b`, `M-x`, `S-Up`, `F5`), parsing,
//! and conversion from raw Windows key records.

use crate::ipc::KeyRecord;
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum KeyCode {
    Char(char),
    Enter,
    Escape,
    Tab,
    BSpace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PPage,
    NPage,
    IC,
    DC,
    F(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    /// Only meaningful for non-character keys; for characters shift is folded
    /// into the character itself.
    pub shift: bool,
}

impl Key {
    pub const fn plain(code: KeyCode) -> Self {
        Key { code, ctrl: false, alt: false, shift: false }
    }
    pub const fn ch(c: char) -> Self {
        Key::plain(KeyCode::Char(c))
    }
    pub const fn ctrl(c: char) -> Self {
        Key { code: KeyCode::Char(c), ctrl: true, alt: false, shift: false }
    }
    pub const fn alt(c: char) -> Self {
        Key { code: KeyCode::Char(c), ctrl: false, alt: true, shift: false }
    }
    pub const fn with_ctrl(code: KeyCode) -> Self {
        Key { code, ctrl: true, alt: false, shift: false }
    }
    pub const fn with_alt(code: KeyCode) -> Self {
        Key { code, ctrl: false, alt: true, shift: false }
    }
    pub const fn with_shift(code: KeyCode) -> Self {
        Key { code, ctrl: false, alt: false, shift: true }
    }

    /// Parse a tmux-style key name.
    pub fn parse(s: &str) -> Option<Key> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut rest = s;
        loop {
            if let Some(r) = rest.strip_prefix("C-") {
                ctrl = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("M-") {
                alt = true;
                rest = r;
            } else if let Some(r) = rest.strip_prefix("S-") {
                shift = true;
                rest = r;
            } else {
                break;
            }
            if rest.is_empty() {
                // "C--" style: the key itself is '-'
                return None;
            }
        }
        // Allow "C--" meaning ctrl + '-'.
        if rest.is_empty() && s.ends_with("--") {
            return Some(Key { code: KeyCode::Char('-'), ctrl, alt, shift: false });
        }
        let code = match rest {
            "Enter" | "Return" | "CR" => KeyCode::Enter,
            "Escape" | "Esc" => KeyCode::Escape,
            "Tab" => KeyCode::Tab,
            "BTab" => {
                shift = true;
                KeyCode::Tab
            }
            "BSpace" | "Backspace" | "BS" => KeyCode::BSpace,
            "Space" => KeyCode::Char(' '),
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "PPage" | "PageUp" | "PgUp" => KeyCode::PPage,
            "NPage" | "PageDown" | "PgDn" => KeyCode::NPage,
            "IC" | "Insert" => KeyCode::IC,
            "DC" | "Delete" => KeyCode::DC,
            _ => {
                if let Some(n) = rest.strip_prefix('F')
                    && let Ok(n) = n.parse::<u8>()
                    && (1..=24).contains(&n)
                {
                    return Some(Key { code: KeyCode::F(n), ctrl, alt, shift });
                }
                let mut it = rest.chars();
                let c = it.next()?;
                if it.next().is_some() {
                    return None;
                }
                KeyCode::Char(c)
            }
        };
        let shift = if matches!(code, KeyCode::Char(_)) { false } else { shift };
        let code = match code {
            // Fold ctrl-letter case: C-B == C-b.
            KeyCode::Char(c) if ctrl && c.is_ascii_uppercase() => KeyCode::Char(c.to_ascii_lowercase()),
            other => other,
        };
        Some(Key { code, ctrl, alt, shift })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("C-")?;
        }
        if self.alt {
            f.write_str("M-")?;
        }
        if self.shift {
            f.write_str("S-")?;
        }
        match self.code {
            KeyCode::Char(' ') => f.write_str("Space"),
            KeyCode::Char(c) => write!(f, "{c}"),
            KeyCode::Enter => f.write_str("Enter"),
            KeyCode::Escape => f.write_str("Escape"),
            KeyCode::Tab => f.write_str("Tab"),
            KeyCode::BSpace => f.write_str("BSpace"),
            KeyCode::Up => f.write_str("Up"),
            KeyCode::Down => f.write_str("Down"),
            KeyCode::Left => f.write_str("Left"),
            KeyCode::Right => f.write_str("Right"),
            KeyCode::Home => f.write_str("Home"),
            KeyCode::End => f.write_str("End"),
            KeyCode::PPage => f.write_str("PPage"),
            KeyCode::NPage => f.write_str("NPage"),
            KeyCode::IC => f.write_str("IC"),
            KeyCode::DC => f.write_str("DC"),
            KeyCode::F(n) => write!(f, "F{n}"),
        }
    }
}

// Windows virtual key codes and control-key-state bits (winuser.h / wincon.h).
pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_SHIFT: u16 = 0x10;
pub const VK_CONTROL: u16 = 0x11;
pub const VK_MENU: u16 = 0x12;
pub const VK_PAUSE: u16 = 0x13;
pub const VK_CAPITAL: u16 = 0x14;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_PRIOR: u16 = 0x21;
pub const VK_NEXT: u16 = 0x22;
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_LWIN: u16 = 0x5B;
pub const VK_RWIN: u16 = 0x5C;
pub const VK_APPS: u16 = 0x5D;
pub const VK_F1: u16 = 0x70;
pub const VK_F24: u16 = 0x87;
pub const VK_NUMLOCK: u16 = 0x90;
pub const VK_SCROLL: u16 = 0x91;
pub const VK_LSHIFT: u16 = 0xA0;
pub const VK_RMENU: u16 = 0xA5;

pub const RIGHT_ALT_PRESSED: u32 = 0x0001;
pub const LEFT_ALT_PRESSED: u32 = 0x0002;
pub const RIGHT_CTRL_PRESSED: u32 = 0x0004;
pub const LEFT_CTRL_PRESSED: u32 = 0x0008;
pub const SHIFT_PRESSED: u32 = 0x0010;
pub const ENHANCED_KEY: u32 = 0x0100;

/// Character produced by a virtual key on a US layout when the console does
/// not supply one (typically when Ctrl is held).
fn vk_to_char(vk: u16, shift: bool) -> Option<char> {
    let c = match vk {
        0x30..=0x39 => {
            let d = (vk - 0x30) as u8;
            if shift { b")!@#$%^&*("[d as usize] as char } else { (b'0' + d) as char }
        }
        0x41..=0x5A => ((vk - 0x41) as u8 + b'a') as char,
        VK_SPACE => ' ',
        0xBA => {
            if shift {
                ':'
            } else {
                ';'
            }
        }
        0xBB => {
            if shift {
                '+'
            } else {
                '='
            }
        }
        0xBC => {
            if shift {
                '<'
            } else {
                ','
            }
        }
        0xBD => {
            if shift {
                '_'
            } else {
                '-'
            }
        }
        0xBE => {
            if shift {
                '>'
            } else {
                '.'
            }
        }
        0xBF => {
            if shift {
                '?'
            } else {
                '/'
            }
        }
        0xC0 => {
            if shift {
                '~'
            } else {
                '`'
            }
        }
        0xDB => {
            if shift {
                '{'
            } else {
                '['
            }
        }
        0xDC => {
            if shift {
                '|'
            } else {
                '\\'
            }
        }
        0xDD => {
            if shift {
                '}'
            } else {
                ']'
            }
        }
        0xDE => {
            if shift {
                '"'
            } else {
                '\''
            }
        }
        _ => return None,
    };
    Some(c)
}

/// Convert a raw key-down record into a logical key, if it represents one.
/// Modifier-only presses, key-ups and lone surrogate halves yield `None`.
pub fn key_from_record(r: &KeyRecord) -> Option<Key> {
    if !r.down {
        return None;
    }
    let cs = r.ctrl;
    let lalt = cs & LEFT_ALT_PRESSED != 0;
    let ralt = cs & RIGHT_ALT_PRESSED != 0;
    let lctrl = cs & LEFT_CTRL_PRESSED != 0;
    let rctrl = cs & RIGHT_CTRL_PRESSED != 0;
    let shift = cs & SHIFT_PRESSED != 0;
    // AltGr shows up as LeftCtrl+RightAlt and produces a real character.
    let altgr = ralt && lctrl && r.ch != 0;
    let alt = (lalt || ralt) && !altgr;
    let ctrl = (lctrl || rctrl) && !altgr;

    let special = match r.vk {
        VK_RETURN => Some(KeyCode::Enter),
        VK_ESCAPE => Some(KeyCode::Escape),
        VK_TAB => Some(KeyCode::Tab),
        VK_BACK => Some(KeyCode::BSpace),
        VK_UP => Some(KeyCode::Up),
        VK_DOWN => Some(KeyCode::Down),
        VK_LEFT => Some(KeyCode::Left),
        VK_RIGHT => Some(KeyCode::Right),
        VK_HOME => Some(KeyCode::Home),
        VK_END => Some(KeyCode::End),
        VK_PRIOR => Some(KeyCode::PPage),
        VK_NEXT => Some(KeyCode::NPage),
        VK_INSERT => Some(KeyCode::IC),
        VK_DELETE => Some(KeyCode::DC),
        VK_F1..=VK_F24 => Some(KeyCode::F((r.vk - VK_F1 + 1) as u8)),
        VK_SHIFT
        | VK_CONTROL
        | VK_MENU
        | VK_CAPITAL
        | VK_NUMLOCK
        | VK_SCROLL
        | VK_PAUSE
        | VK_LWIN
        | VK_RWIN
        | VK_APPS
        | VK_LSHIFT..=VK_RMENU => return None,
        _ => None,
    };
    if let Some(code) = special {
        return Some(Key { code, ctrl, alt, shift });
    }

    let c = if ctrl {
        // The console gives control characters (or nothing) here; recover the
        // logical key from the virtual key code instead.
        match vk_to_char(r.vk, shift) {
            Some(c) => c,
            None => {
                if r.ch == 0 || (0xD800..=0xDFFF).contains(&r.ch) {
                    return None;
                }
                char::from_u32(r.ch as u32)?
            }
        }
    } else {
        if r.ch == 0 {
            // Alt+key without a character (e.g. Alt+arrow handled above) or
            // a dead key: derive from the virtual key code if possible.
            return vk_to_char(r.vk, shift).map(|c| Key { code: KeyCode::Char(c), ctrl, alt, shift: false });
        }
        if (0xD800..=0xDFFF).contains(&r.ch) {
            return None;
        }
        let c = char::from_u32(r.ch as u32)?;
        if c.is_control() {
            return None;
        }
        c
    };
    Some(Key { code: KeyCode::Char(c), ctrl, alt, shift: false })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(vk: u16, ch: u16, ctrl: u32) -> KeyRecord {
        KeyRecord { down: true, repeat: 1, vk, sc: 0, ch, ctrl }
    }

    #[test]
    fn parse_names() {
        assert_eq!(Key::parse("C-b"), Some(Key::ctrl('b')));
        assert_eq!(Key::parse("C-B"), Some(Key::ctrl('b')));
        assert_eq!(Key::parse("M-x"), Some(Key::alt('x')));
        assert_eq!(Key::parse("S-Up"), Some(Key::with_shift(KeyCode::Up)));
        assert_eq!(Key::parse("C-M-Left"), Some(Key { code: KeyCode::Left, ctrl: true, alt: true, shift: false }));
        assert_eq!(Key::parse("F12"), Some(Key::plain(KeyCode::F(12))));
        assert_eq!(Key::parse("Space"), Some(Key::ch(' ')));
        assert_eq!(Key::parse("%"), Some(Key::ch('%')));
        assert_eq!(Key::parse("\""), Some(Key::ch('"')));
        assert_eq!(Key::parse("C--"), Some(Key::ctrl('-')));
        assert_eq!(Key::parse("F99"), None);
        assert_eq!(Key::parse("abc"), None);
        assert_eq!(Key::parse(""), None);
    }

    #[test]
    fn display_roundtrip() {
        for name in ["C-b", "M-x", "S-Up", "F5", "Space", "Enter", "C-M-S-Left", "x", "PPage"] {
            let k = Key::parse(name).unwrap();
            assert_eq!(k.to_string(), name);
            assert_eq!(Key::parse(&k.to_string()), Some(k));
        }
    }

    #[test]
    fn records_to_keys() {
        // Ctrl+B: console reports ch=0x02.
        assert_eq!(key_from_record(&rec(0x42, 0x02, LEFT_CTRL_PRESSED)), Some(Key::ctrl('b')));
        // Plain 'B' with shift is the character 'B'.
        assert_eq!(key_from_record(&rec(0x42, b'B' as u16, SHIFT_PRESSED)), Some(Key::ch('B')));
        // Enter.
        assert_eq!(key_from_record(&rec(VK_RETURN, 13, 0)), Some(Key::plain(KeyCode::Enter)));
        // Shift+Enter keeps its shift flag.
        assert_eq!(key_from_record(&rec(VK_RETURN, 13, SHIFT_PRESSED)), Some(Key::with_shift(KeyCode::Enter)));
        // Modifier alone.
        assert_eq!(key_from_record(&rec(VK_CONTROL, 0, LEFT_CTRL_PRESSED)), None);
        // Key up.
        let mut up = rec(0x42, b'b' as u16, 0);
        up.down = false;
        assert_eq!(key_from_record(&up), None);
        // AltGr+2 on a German layout gives '²' with LCTRL|RALT: not ctrl, not alt.
        assert_eq!(key_from_record(&rec(0x32, 0xB2, LEFT_CTRL_PRESSED | RIGHT_ALT_PRESSED)), Some(Key::ch('²')));
        // Alt+x.
        assert_eq!(key_from_record(&rec(0x58, b'x' as u16, LEFT_ALT_PRESSED)), Some(Key::alt('x')));
        // Ctrl+Space.
        assert_eq!(key_from_record(&rec(VK_SPACE, 0x20, LEFT_CTRL_PRESSED)), Some(Key::ctrl(' ')));
        // F1.
        assert_eq!(key_from_record(&rec(VK_F1, 0, 0)), Some(Key::plain(KeyCode::F(1))));
        // Lone surrogate is not a logical key.
        assert_eq!(key_from_record(&rec(0, 0xD83D, 0)), None);
    }
}
