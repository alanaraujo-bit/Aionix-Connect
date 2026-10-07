//! Visible consent prompt ("Permitir" / "Recusar") shown before a technician gets
//! access to this computer.
//!
//! A small top-most window on its own UI thread. It is language-agnostic: every
//! string comes from the caller. Safety properties:
//! * "Recusar" has the initial focus and is the default button; Enter, Esc, Alt+F4
//!   and the close box all refuse.
//! * "Permitir" stays disabled for `allow_delay` after the window appears, so a click
//!   or key press meant for something else cannot approve the request.
//! * After `timeout` the request is refused automatically; the remaining time is shown.
//! * Text is rendered without mnemonic processing (`&` is shown literally).

use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            COLOR_WINDOW, CreateFontIndirectW, DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK,
            DeleteObject, DrawTextW, GetDC, GetSysColor, GetSysColorBrush, HBRUSH, HDC, HFONT,
            HGDIOBJ, ReleaseDC, SelectObject, SetBkColor, SetBkMode, TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            HiDpi::GetDpiForSystem,
            Input::KeyboardAndMouse::{EnableWindow, IsWindowEnabled, SetFocus},
            WindowsAndMessaging::{
                AdjustWindowRectEx, BN_CLICKED, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CREATESTRUCTW,
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FLASHW_ALL,
                FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWLP_USERDATA, GetMessageW,
                GetWindowLongPtrW, HMENU, IDC_ARROW, IDCANCEL, IDI_SHIELD, IsDialogMessageW,
                KillTimer, LoadCursorW, LoadIconW, MSG, NONCLIENTMETRICSW, PostMessageW,
                PostQuitMessage, RegisterClassExW, SPI_GETNONCLIENTMETRICS, SPI_GETWORKAREA,
                SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SendMessageW, SetForegroundWindow,
                SetTimer, SetWindowLongPtrW, SetWindowTextW, ShowWindow, SystemParametersInfoW,
                TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
                WM_CTLCOLORSTATIC, WM_DESTROY, WM_NCCREATE, WM_SETFONT, WM_TIMER, WNDCLASSEXW,
                WS_CAPTION, WS_CHILD, WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST,
                WS_POPUP, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE,
            },
        },
    },
    core::{HSTRING, PCWSTR, w},
};

use tracing::info;

use crate::PlatformError;

/// Strings and timing of one prompt.
#[derive(Debug, Clone)]
pub struct ConsentPrompt {
    pub window_title: String,
    pub heading: String,
    pub body: String,
    pub allow_label: String,
    pub deny_label: String,
    /// Shown under the body; `{s}` is replaced by the remaining seconds.
    pub countdown_template: String,
    pub timeout: Duration,
    pub allow_delay: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConsentResult {
    /// The user clicked the allow button.
    Allowed,
    /// The user refused (button, Esc, Enter on the default button, close box).
    Denied,
    /// Nobody answered within the timeout.
    TimedOut,
    /// Withdrawn by the application ([`ConsentWindow::dismiss`]).
    Dismissed,
    /// The window could not be shown.
    Failed(String),
}

const CLASS_NAME: PCWSTR = w!("AionixConnectConsent");
const ID_ALLOW: i32 = 1001;
const ID_DENY: i32 = 1002;
const TIMER_ID: usize = 1;
const WM_APP_DISMISS: u32 = WM_APP + 1;
// Static control styles (winuser.h); defined here to avoid a large feature module.
const SS_LEFT: u32 = 0x0000;
const SS_NOPREFIX: u32 = 0x0080;

#[derive(Default)]
struct Shared {
    hwnd: isize,
    dismissed: bool,
}

/// Handle to a prompt running on its own thread. Dropping it withdraws the prompt.
pub struct ConsentWindow {
    shared: Arc<Mutex<Shared>>,
}

impl ConsentWindow {
    /// Shows `prompt`; `on_result` is called exactly once, from the UI thread.
    pub fn show(
        prompt: ConsentPrompt,
        on_result: impl FnOnce(ConsentResult) + Send + 'static,
    ) -> Result<Self, PlatformError> {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread_shared = shared.clone();
        std::thread::Builder::new()
            .name("aionix-consent".into())
            .spawn(move || {
                let result = run(prompt, &thread_shared)
                    .unwrap_or_else(|e| ConsentResult::Failed(e.to_string()));
                on_result(result);
            })
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        Ok(Self { shared })
    }

    /// Closes the prompt without an answer (reported as [`ConsentResult::Dismissed`]).
    pub fn dismiss(&self) {
        let mut s = self.shared.lock().unwrap();
        s.dismissed = true;
        if s.hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(s.hwnd as *mut c_void)),
                    WM_APP_DISMISS,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
        }
    }
}

impl Drop for ConsentWindow {
    fn drop(&mut self) {
        self.dismiss();
    }
}

struct DialogState {
    shown_at: Instant,
    deadline: Instant,
    allow_at: Instant,
    allow_enabled: bool,
    countdown_template: String,
    shown_seconds: u64,
    allow: HWND,
    countdown: HWND,
    result: Option<ConsentResult>,
}

struct Font(HFONT);

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

fn register_class(instance: HINSTANCE) -> Result<(), PlatformError> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: LoadIconW(None, IDI_SHIELD).unwrap_or_default(),
                hbrBackground: HBRUSH((COLOR_WINDOW.0 + 1) as usize as *mut c_void),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 {
                Err(windows::core::Error::from_thread().to_string())
            } else {
                Ok(())
            }
        })
        .clone()
        .map_err(PlatformError::Os)
}

fn fonts() -> Result<(Font, Font), PlatformError> {
    let mut ncm = NONCLIENTMETRICSW {
        cbSize: std::mem::size_of::<NONCLIENTMETRICSW>() as u32,
        ..Default::default()
    };
    unsafe {
        SystemParametersInfoW(
            SPI_GETNONCLIENTMETRICS,
            ncm.cbSize,
            Some(&mut ncm as *mut NONCLIENTMETRICSW as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )?;
        let body = Font(CreateFontIndirectW(&ncm.lfMessageFont));
        let mut lf = ncm.lfMessageFont;
        lf.lfWeight = 600;
        lf.lfHeight = lf.lfHeight * 13 / 10;
        let heading = Font(CreateFontIndirectW(&lf));
        Ok((heading, body))
    }
}

/// Converts `\n` to the `\r\n` static controls expect.
fn crlf(s: &str) -> String {
    s.replace("\r\n", "\n").replace('\n', "\r\n")
}

fn measure(text: &str, font: &Font, width: i32) -> i32 {
    let mut wide: Vec<u16> = crlf(text).encode_utf16().collect();
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    unsafe {
        let hdc = GetDC(None);
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        DrawTextW(
            hdc,
            &mut wide,
            &mut rc,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(hdc, old);
        ReleaseDC(None, hdc);
    }
    rc.bottom - rc.top
}

fn countdown_text(template: &str, seconds: u64) -> String {
    template.replace("{s}", &seconds.to_string())
}

#[allow(clippy::too_many_arguments)]
fn child(
    parent: HWND,
    instance: HINSTANCE,
    class: PCWSTR,
    text: &str,
    style: u32,
    id: i32,
    (x, y, w, h): (i32, i32, i32, i32),
    font: &Font,
) -> Result<HWND, PlatformError> {
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            class,
            &HSTRING::from(crlf(text)),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | style),
            x,
            y,
            w,
            h,
            Some(parent),
            Some(HMENU(id as isize as *mut c_void)),
            Some(instance),
            None,
        )?
    };
    unsafe {
        SendMessageW(
            hwnd,
            WM_SETFONT,
            Some(WPARAM(font.0.0 as usize)),
            Some(LPARAM(1)),
        );
    }
    Ok(hwnd)
}

fn run(prompt: ConsentPrompt, shared: &Mutex<Shared>) -> Result<ConsentResult, PlatformError> {
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    register_class(instance)?;
    let (heading_font, body_font) = fonts()?;

    let dpi = unsafe { GetDpiForSystem() }.max(96) as i32;
    let s = |v: i32| v * dpi / 96;
    let width = s(500);
    let pad = s(24);
    let inner = width - 2 * pad;
    let (btn_w, btn_h, gap) = (s(116), s(32), s(12));

    let total_secs = prompt.timeout.as_secs().max(1);
    let heading_h = measure(&prompt.heading, &heading_font, inner);
    let body_h = measure(&prompt.body, &body_font, inner);
    let countdown_h = measure(
        &countdown_text(&prompt.countdown_template, total_secs),
        &body_font,
        inner,
    );
    let heading_y = pad;
    let body_y = heading_y + heading_h + s(14);
    let countdown_y = body_y + body_h + s(14);
    let buttons_y = countdown_y + countdown_h + s(22);
    let client_h = buttons_y + btn_h + pad;

    let style = WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0);
    let ex_style = WINDOW_EX_STYLE(WS_EX_TOPMOST.0 | WS_EX_DLGMODALFRAME.0 | WS_EX_CONTROLPARENT.0);
    let mut outer = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: client_h,
    };
    unsafe { AdjustWindowRectEx(&mut outer, style, false, ex_style)? };
    let (ow, oh) = (outer.right - outer.left, outer.bottom - outer.top);
    let mut work = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut RECT as *mut c_void),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )?;
    }
    let x = work.left + ((work.right - work.left) - ow).max(0) / 2;
    let y = work.top + ((work.bottom - work.top) - oh).max(0) / 2;

    let now = Instant::now();
    let mut state = Box::new(DialogState {
        shown_at: now,
        deadline: now + prompt.timeout,
        allow_at: now + prompt.allow_delay,
        allow_enabled: prompt.allow_delay.is_zero(),
        countdown_template: prompt.countdown_template.clone(),
        shown_seconds: total_secs,
        allow: HWND::default(),
        countdown: HWND::default(),
        result: None,
    });
    let state_ptr: *mut DialogState = &mut *state;

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS_NAME,
            &HSTRING::from(prompt.window_title.as_str()),
            style,
            x,
            y,
            ow,
            oh,
            None,
            None,
            Some(instance),
            Some(state_ptr as *const c_void),
        )?
    };

    let static_style = SS_LEFT | SS_NOPREFIX;
    let button = w!("BUTTON");
    let label = w!("STATIC");
    let build = || -> Result<(HWND, HWND, HWND), PlatformError> {
        child(
            hwnd,
            instance,
            label,
            &prompt.heading,
            static_style,
            0,
            (pad, heading_y, inner, heading_h),
            &heading_font,
        )?;
        child(
            hwnd,
            instance,
            label,
            &prompt.body,
            static_style,
            0,
            (pad, body_y, inner, body_h),
            &body_font,
        )?;
        let countdown = child(
            hwnd,
            instance,
            label,
            &countdown_text(&prompt.countdown_template, total_secs),
            static_style,
            0,
            (pad, countdown_y, inner, countdown_h),
            &body_font,
        )?;
        let deny = child(
            hwnd,
            instance,
            button,
            &prompt.deny_label,
            WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
            ID_DENY,
            (width - pad - 2 * btn_w - gap, buttons_y, btn_w, btn_h),
            &body_font,
        )?;
        let allow = child(
            hwnd,
            instance,
            button,
            &prompt.allow_label,
            WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
            ID_ALLOW,
            (width - pad - btn_w, buttons_y, btn_w, btn_h),
            &body_font,
        )?;
        Ok((countdown, deny, allow))
    };
    let (countdown, deny, allow) = match build() {
        Ok(v) => v,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    state.countdown = countdown;
    state.allow = allow;
    unsafe {
        if !state.allow_enabled {
            let _ = EnableWindow(allow, false);
        }
    }

    {
        let mut sh = shared.lock().unwrap();
        if sh.dismissed {
            state.result = Some(ConsentResult::Dismissed);
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        } else {
            sh.hwnd = hwnd.0 as isize;
        }
    }

    if state.result.is_none() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            let _ = SetFocus(Some(deny));
            let _ = FlashWindowEx(&FLASHWINFO {
                cbSize: std::mem::size_of::<FLASHWINFO>() as u32,
                hwnd,
                dwFlags: FLASHW_ALL | FLASHW_TIMERNOFG,
                uCount: 0,
                dwTimeout: 0,
            });
            SetTimer(Some(hwnd), TIMER_ID, 200, None);
        }
        let mut msg = MSG::default();
        unsafe {
            while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
                if !IsDialogMessageW(hwnd, &msg).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
    shared.lock().unwrap().hwnd = 0;
    // The window is destroyed: no message can reach `state` any more.
    Ok(state.result.take().unwrap_or(ConsentResult::Denied))
}

enum Action {
    None,
    Finish(ConsentResult),
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        if msg == WM_NCCREATE {
            let cs = &*(lparam.0 as *const CREATESTRUCTW);
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, cs.lpCreateParams as isize);
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut DialogState;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wparam, lparam);
        }
        let action = {
            let state = &mut *ptr;
            match msg {
                WM_COMMAND => {
                    let id = (wparam.0 & 0xffff) as i32;
                    let code = ((wparam.0 >> 16) & 0xffff) as u32;
                    let from = lparam.0;
                    if code != BN_CLICKED {
                        Action::None
                    } else if id == ID_ALLOW {
                        // Only a real click on the enabled "allow" button counts.
                        if from == state.allow.0 as isize
                            && state.allow_enabled
                            && IsWindowEnabled(state.allow).as_bool()
                        {
                            Action::Finish(ConsentResult::Allowed)
                        } else {
                            Action::None
                        }
                    } else if id == ID_DENY || id == IDCANCEL.0 {
                        Action::Finish(ConsentResult::Denied)
                    } else {
                        Action::None
                    }
                }
                WM_CLOSE => Action::Finish(ConsentResult::Denied),
                WM_APP_DISMISS => Action::Finish(ConsentResult::Dismissed),
                WM_TIMER => {
                    let now = Instant::now();
                    if !state.allow_enabled && now >= state.allow_at {
                        state.allow_enabled = true;
                        let _ = EnableWindow(state.allow, true);
                    }
                    let remaining = state.deadline.saturating_duration_since(now);
                    if remaining.is_zero() {
                        Action::Finish(ConsentResult::TimedOut)
                    } else {
                        let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
                        if secs != state.shown_seconds {
                            state.shown_seconds = secs;
                            let text = countdown_text(&state.countdown_template, secs);
                            let _ = SetWindowTextW(state.countdown, &HSTRING::from(text));
                        }
                        Action::None
                    }
                }
                WM_CTLCOLORSTATIC => {
                    let hdc = HDC(wparam.0 as *mut c_void);
                    SetBkMode(hdc, TRANSPARENT);
                    SetBkColor(hdc, COLORREF(GetSysColor(COLOR_WINDOW)));
                    return LRESULT(GetSysColorBrush(COLOR_WINDOW).0 as isize);
                }
                WM_DESTROY => {
                    let _ = KillTimer(Some(hwnd), TIMER_ID);
                    PostQuitMessage(0);
                    return LRESULT(0);
                }
                _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
            }
        };
        if let Action::Finish(result) = action {
            let state = &mut *ptr;
            if state.result.is_none() {
                info!(
                    ?result,
                    after_ms = state.shown_at.elapsed().as_millis() as u64,
                    "consent prompt answered"
                );
                state.result = Some(result);
            }
            let _ = DestroyWindow(hwnd);
        }
        LRESULT(0)
    }
}

#[cfg(test)]
mod tests {
    //! These open a real window on the desktop for about a second, so they only run
    //! on demand: `cargo test -p aionix-platform consent -- --ignored`.
    use super::*;
    use std::sync::mpsc::channel;

    fn prompt(timeout: Duration) -> ConsentPrompt {
        ConsentPrompt {
            window_title: "Aionix Connect (teste)".into(),
            heading: "Teste automático".into(),
            body: "Esta janela fecha sozinha.".into(),
            allow_label: "Permitir".into(),
            deny_label: "Recusar".into(),
            countdown_template: "{s} s".into(),
            timeout,
            allow_delay: Duration::from_secs(5),
        }
    }

    #[test]
    #[ignore = "shows a window on the interactive desktop"]
    fn unanswered_prompt_times_out() {
        let (tx, rx) = channel();
        let _w = ConsentWindow::show(prompt(Duration::from_secs(1)), move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(r, ConsentResult::TimedOut);
    }

    #[test]
    #[ignore = "shows a window on the interactive desktop"]
    fn dismissed_prompt_is_not_an_approval() {
        let (tx, rx) = channel();
        let w = ConsentWindow::show(prompt(Duration::from_secs(30)), move |r| {
            let _ = tx.send(r);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(500));
        drop(w);
        let r = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(r, ConsentResult::Dismissed);
    }
}
