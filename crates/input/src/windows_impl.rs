use windows::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBD_EVENT_FLAGS, KEYBDINPUT,
        KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP, KEYEVENTF_SCANCODE, KEYEVENTF_UNICODE,
        MOUSE_EVENT_FLAGS, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN,
        MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_MOVE,
        MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL,
        MOUSEEVENTF_XDOWN, MOUSEEVENTF_XUP, MOUSEINPUT, SendInput, VIRTUAL_KEY,
    },
    WindowsAndMessaging::{
        GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN,
        SM_YVIRTUALSCREEN, XBUTTON1, XBUTTON2,
    },
};

use crate::{InputError, InputInjector, MouseButton};

fn send(inputs: &[INPUT]) -> Result<(), InputError> {
    let sent = unsafe { SendInput(inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        // UIPI (elevated foreground window) or secure desktop.
        Err(InputError::Blocked)
    }
}

fn mouse(flags: MOUSE_EVENT_FLAGS, dx: i32, dy: i32, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT { dx, dy, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

fn keyboard(scan: u16, vk: u16, flags: KEYBD_EVENT_FLAGS) -> INPUT {
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: scan, dwFlags: flags, time: 0, dwExtraInfo: 0 },
        },
    }
}

/// Maps a desktop pixel to SendInput's normalized 0..=65535 virtual desktop space.
pub(crate) fn normalize(x: i32, y: i32, vx: i32, vy: i32, vw: i32, vh: i32) -> (i32, i32) {
    let nx = ((i64::from(x - vx) * 65535) / i64::from((vw - 1).max(1))) as i32;
    let ny = ((i64::from(y - vy) * 65535) / i64::from((vh - 1).max(1))) as i32;
    (nx.clamp(0, 65535), ny.clamp(0, 65535))
}

impl InputInjector {
    pub(crate) fn platform_move(&mut self, x: i32, y: i32) -> Result<(), InputError> {
        let (vx, vy, vw, vh) = unsafe {
            (
                GetSystemMetrics(SM_XVIRTUALSCREEN),
                GetSystemMetrics(SM_YVIRTUALSCREEN),
                GetSystemMetrics(SM_CXVIRTUALSCREEN),
                GetSystemMetrics(SM_CYVIRTUALSCREEN),
            )
        };
        let (nx, ny) = normalize(x, y, vx, vy, vw, vh);
        send(&[mouse(MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK, nx, ny, 0)])
    }

    pub(crate) fn platform_button(&mut self, button: MouseButton, down: bool) -> Result<(), InputError> {
        let (flags, data) = match (button, down) {
            (MouseButton::Left, true) => (MOUSEEVENTF_LEFTDOWN, 0),
            (MouseButton::Left, false) => (MOUSEEVENTF_LEFTUP, 0),
            (MouseButton::Right, true) => (MOUSEEVENTF_RIGHTDOWN, 0),
            (MouseButton::Right, false) => (MOUSEEVENTF_RIGHTUP, 0),
            (MouseButton::Middle, true) => (MOUSEEVENTF_MIDDLEDOWN, 0),
            (MouseButton::Middle, false) => (MOUSEEVENTF_MIDDLEUP, 0),
            (MouseButton::Back, true) => (MOUSEEVENTF_XDOWN, XBUTTON1 as i32),
            (MouseButton::Back, false) => (MOUSEEVENTF_XUP, XBUTTON1 as i32),
            (MouseButton::Forward, true) => (MOUSEEVENTF_XDOWN, XBUTTON2 as i32),
            (MouseButton::Forward, false) => (MOUSEEVENTF_XUP, XBUTTON2 as i32),
        };
        send(&[mouse(flags, 0, 0, data)])
    }

    pub(crate) fn platform_wheel(&mut self, dx: i32, dy: i32) -> Result<(), InputError> {
        let mut inputs = Vec::with_capacity(2);
        if dy != 0 {
            inputs.push(mouse(MOUSEEVENTF_WHEEL, 0, 0, dy));
        }
        if dx != 0 {
            inputs.push(mouse(MOUSEEVENTF_HWHEEL, 0, 0, dx));
        }
        if inputs.is_empty() { Ok(()) } else { send(&inputs) }
    }

    pub(crate) fn platform_key(&mut self, scancode: u32, extended: bool, down: bool) -> Result<(), InputError> {
        let mut flags = KEYEVENTF_SCANCODE;
        if extended {
            flags |= KEYEVENTF_EXTENDEDKEY;
        }
        if !down {
            flags |= KEYEVENTF_KEYUP;
        }
        send(&[keyboard(scancode as u16, 0, flags)])
    }

    pub(crate) fn platform_text(&mut self, text: &str) -> Result<(), InputError> {
        let mut inputs = Vec::with_capacity(text.len() * 2);
        for unit in text.encode_utf16() {
            inputs.push(keyboard(unit, 0, KEYEVENTF_UNICODE));
            inputs.push(keyboard(unit, 0, KEYEVENTF_UNICODE | KEYEVENTF_KEYUP));
        }
        if inputs.is_empty() { Ok(()) } else { send(&inputs) }
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn normalizes_corners_of_virtual_desktop() {
        // Two monitors: secondary at x=-1280.
        let (vx, vy, vw, vh) = (-1280, 0, 3200, 1080);
        assert_eq!(normalize(-1280, 0, vx, vy, vw, vh), (0, 0));
        assert_eq!(normalize(1919, 1079, vx, vy, vw, vh), (65535, 65535));
        let (nx, _) = normalize(0, 0, vx, vy, vw, vh);
        assert!((26200..26250).contains(&nx), "{nx}");
    }
}
