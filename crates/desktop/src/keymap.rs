//! Physical keys → PC scan codes (set 1), the representation `KeyEvent` carries.
//!
//! The technician sends *positions*, not characters: the host's keyboard layout
//! decides which character a key produces, exactly like a local keyboard. Keys egui
//! cannot report physically (IME, some international keys) fall back to `TextInput`.

use egui::Key;

/// `(scancode, extended)` of a physical key.
pub fn scancode(key: Key) -> Option<(u32, bool)> {
    use Key::*;
    let plain = |c: u32| Some((c, false));
    let ext = |c: u32| Some((c, true));
    match key {
        Escape => plain(0x01),
        Num1 => plain(0x02),
        Num2 => plain(0x03),
        Num3 => plain(0x04),
        Num4 => plain(0x05),
        Num5 => plain(0x06),
        Num6 => plain(0x07),
        Num7 => plain(0x08),
        Num8 => plain(0x09),
        Num9 => plain(0x0A),
        Num0 => plain(0x0B),
        Minus => plain(0x0C),
        Equals => plain(0x0D),
        Backspace => plain(0x0E),
        Tab => plain(0x0F),
        Q => plain(0x10),
        W => plain(0x11),
        E => plain(0x12),
        R => plain(0x13),
        T => plain(0x14),
        Y => plain(0x15),
        U => plain(0x16),
        I => plain(0x17),
        O => plain(0x18),
        P => plain(0x19),
        OpenBracket => plain(0x1A),
        CloseBracket => plain(0x1B),
        Enter => plain(0x1C),
        A => plain(0x1E),
        S => plain(0x1F),
        D => plain(0x20),
        F => plain(0x21),
        G => plain(0x22),
        H => plain(0x23),
        J => plain(0x24),
        K => plain(0x25),
        L => plain(0x26),
        Semicolon => plain(0x27),
        Quote => plain(0x28),
        Backtick => plain(0x29),
        Backslash => plain(0x2B),
        Z => plain(0x2C),
        X => plain(0x2D),
        C => plain(0x2E),
        V => plain(0x2F),
        B => plain(0x30),
        N => plain(0x31),
        M => plain(0x32),
        Comma => plain(0x33),
        Period => plain(0x34),
        Slash => plain(0x35),
        Space => plain(0x39),
        F1 => plain(0x3B),
        F2 => plain(0x3C),
        F3 => plain(0x3D),
        F4 => plain(0x3E),
        F5 => plain(0x3F),
        F6 => plain(0x40),
        F7 => plain(0x41),
        F8 => plain(0x42),
        F9 => plain(0x43),
        F10 => plain(0x44),
        F11 => plain(0x57),
        F12 => plain(0x58),
        Home => ext(0x47),
        ArrowUp => ext(0x48),
        PageUp => ext(0x49),
        ArrowLeft => ext(0x4B),
        ArrowRight => ext(0x4D),
        End => ext(0x4F),
        ArrowDown => ext(0x50),
        PageDown => ext(0x51),
        Insert => ext(0x52),
        Delete => ext(0x53),
        _ => None,
    }
}

/// Modifier keys, sent as left-hand keys.
pub const SHIFT: (u32, bool) = (0x2A, false);
pub const CTRL: (u32, bool) = (0x1D, false);
pub const ALT: (u32, bool) = (0x38, false);

/// Keys handled through modifier tracking instead of key events.
pub fn is_modifier(key: Key) -> bool {
    matches!(
        key,
        Key::ShiftLeft | Key::ShiftRight | Key::ControlLeft | Key::ControlRight
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_keys_have_scancodes() {
        assert_eq!(scancode(Key::A), Some((0x1E, false)));
        assert_eq!(scancode(Key::Enter), Some((0x1C, false)));
        assert_eq!(scancode(Key::Delete), Some((0x53, true)));
        assert_eq!(scancode(Key::ArrowLeft), Some((0x4B, true)));
        assert_eq!(scancode(Key::F12), Some((0x58, false)));
        assert_eq!(scancode(Key::Copy), None);
    }
}
