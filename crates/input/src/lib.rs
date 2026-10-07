//! Remote input injection.
//!
//! Keys are injected by **scan code** so the remote keyboard layout decides which
//! character is produced (a Brazilian ABNT2 technician controlling a US machine types
//! what the US machine expects for those physical keys). Text that has no physical key
//! (IME, emoji) uses Unicode injection.
//!
//! Every pressed key and button is tracked so [`InputInjector::release_all`] can undo
//! them when the viewer loses focus, disconnects or the session ends: a stuck Ctrl or
//! mouse button on the remote machine is never acceptable.

use std::collections::BTreeSet;

#[cfg(windows)]
mod windows_impl;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
}

/// Rectangle of the controlled display in desktop coordinates (physical pixels).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, thiserror::Error)]
pub enum InputError {
    #[error("input injection blocked (secure desktop or higher integrity window)")]
    Blocked,
    #[error("platform error: {0}")]
    Platform(String),
}

/// Keys and buttons currently held by the remote viewer.
#[derive(Debug, Default)]
pub struct HeldState {
    keys: BTreeSet<(u32, bool)>,
    buttons: BTreeSet<MouseButton>,
}

impl HeldState {
    pub fn key(&mut self, scancode: u32, extended: bool, down: bool) {
        if down {
            self.keys.insert((scancode, extended));
        } else {
            self.keys.remove(&(scancode, extended));
        }
    }

    pub fn button(&mut self, button: MouseButton, down: bool) {
        if down {
            self.buttons.insert(button);
        } else {
            self.buttons.remove(&button);
        }
    }

    /// Drains everything currently held.
    pub fn take(&mut self) -> (Vec<(u32, bool)>, Vec<MouseButton>) {
        (
            std::mem::take(&mut self.keys).into_iter().collect(),
            std::mem::take(&mut self.buttons).into_iter().collect(),
        )
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }
}

/// Injects input into the interactive desktop of the current session.
pub struct InputInjector {
    display: DisplayRect,
    held: HeldState,
}

impl InputInjector {
    pub fn new(display: DisplayRect) -> Self {
        Self {
            display,
            held: HeldState::default(),
        }
    }

    /// Changes the display that mouse coordinates refer to (monitor switch).
    pub fn set_display(&mut self, display: DisplayRect) {
        self.display = display;
    }

    pub fn display(&self) -> DisplayRect {
        self.display
    }

    /// Clamps display-relative coordinates into the display.
    fn clamp(&self, x: i32, y: i32) -> (i32, i32) {
        (
            x.clamp(0, self.display.width.saturating_sub(1) as i32),
            y.clamp(0, self.display.height.saturating_sub(1) as i32),
        )
    }

    pub fn mouse_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        let (x, y) = self.clamp(x, y);
        self.platform_move(self.display.x + x, self.display.y + y)
    }

    pub fn mouse_button(
        &mut self,
        button: MouseButton,
        down: bool,
        x: i32,
        y: i32,
    ) -> Result<(), InputError> {
        self.mouse_move(x, y)?;
        self.held.button(button, down);
        self.platform_button(button, down)
    }

    /// Wheel deltas in WHEEL_DELTA units (120 per notch).
    pub fn mouse_wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        self.platform_wheel(dx, dy)
    }

    pub fn key(&mut self, scancode: u32, extended: bool, down: bool) -> Result<(), InputError> {
        if scancode == 0 || scancode > 0xFF {
            return Ok(());
        }
        self.held.key(scancode, extended, down);
        self.platform_key(scancode, extended, down)
    }

    pub fn text(&mut self, text: &str) -> Result<(), InputError> {
        self.platform_text(text)
    }

    /// Releases every key and button held by this viewer.
    pub fn release_all(&mut self) -> Result<(), InputError> {
        let (keys, buttons) = self.held.take();
        for (sc, ext) in keys.into_iter().rev() {
            let _ = self.platform_key(sc, ext, false);
        }
        for b in buttons {
            let _ = self.platform_button(b, false);
        }
        Ok(())
    }
}

impl Drop for InputInjector {
    fn drop(&mut self) {
        let _ = self.release_all();
    }
}

#[cfg(not(windows))]
impl InputInjector {
    fn platform_move(&mut self, _x: i32, _y: i32) -> Result<(), InputError> {
        Err(InputError::Platform("unsupported platform".into()))
    }
    fn platform_button(&mut self, _b: MouseButton, _down: bool) -> Result<(), InputError> {
        Err(InputError::Platform("unsupported platform".into()))
    }
    fn platform_wheel(&mut self, _dx: i32, _dy: i32) -> Result<(), InputError> {
        Err(InputError::Platform("unsupported platform".into()))
    }
    fn platform_key(&mut self, _sc: u32, _ext: bool, _down: bool) -> Result<(), InputError> {
        Err(InputError::Platform("unsupported platform".into()))
    }
    fn platform_text(&mut self, _t: &str) -> Result<(), InputError> {
        Err(InputError::Platform("unsupported platform".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_state_tracks_and_drains() {
        let mut h = HeldState::default();
        h.key(0x1D, false, true);
        h.key(0x38, false, true);
        h.key(0x38, false, false);
        h.button(MouseButton::Left, true);
        let (keys, buttons) = h.take();
        assert_eq!(keys, vec![(0x1D, false)]);
        assert_eq!(buttons, vec![MouseButton::Left]);
        assert!(h.is_empty());
    }

    #[test]
    fn coordinates_are_clamped_to_display() {
        let inj = InputInjector {
            display: DisplayRect {
                x: 1920,
                y: 0,
                width: 1280,
                height: 1024,
            },
            held: HeldState::default(),
        };
        assert_eq!(inj.clamp(-5, 5000), (0, 1023));
        assert_eq!(inj.clamp(100, 100), (100, 100));
        std::mem::forget(inj);
    }
}
