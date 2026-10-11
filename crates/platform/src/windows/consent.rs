//! Visible consent prompt ("Permitir" / "Recusar") shown before a technician gets
//! access to this computer.
//!
//! A small top-most window on its own UI thread. It is language-agnostic: every
//! string comes from the caller. Safety properties:
//! * "Recusar" has the initial focus and is the default button; Enter, Esc, Alt+F4
//!   and the close box all refuse.
//! * "Permitir" stays disabled for `allow_delay` after the window appears, so a click
//!   or key press meant for something else cannot approve the request.
//! * After `timeout` the request is refused automatically; the remaining time is shown,
//!   as text and as a bar that empties.
//! * Text is rendered without mnemonic processing (`&` is shown literally).
//!
//! Look: the Aionix Connect visual language (crates/desktop/src/theme.rs; the colors
//! below mirror its palettes, DESIGN.md documents both) in plain Win32: it follows the Windows light/dark setting, the caption takes the window color
//! (Windows 11), shapes are antialiased with GDI+, text is ClearType GDI, and both
//! buttons are owner-drawn `BUTTON` controls so focus, keyboard and click handling stay
//! the system's.

use std::{
    ffi::c_void,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use windows::{
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::{
            Dwm::{
                DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR,
                DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE, DWMWCP_ROUND,
                DwmSetWindowAttribute,
            },
            Gdi::{
                BeginPaint, BitBlt, CLEARTYPE_QUALITY, CreateCompatibleBitmap, CreateCompatibleDC,
                CreateFontIndirectW, CreateSolidBrush, DRAW_TEXT_FORMAT, DT_CALCRECT, DT_CENTER,
                DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, DT_WORDBREAK,
                DeleteDC, DeleteObject, DrawTextW, EndPaint, FillRect, GetDC, HBRUSH, HDC, HFONT,
                HGDIOBJ, InvalidateRect, PAINTSTRUCT, ReleaseDC, SRCCOPY, SelectObject,
                SetBkMode, SetTextColor, TRANSPARENT,
            },
            GdiPlus::{
                FillModeAlternate, GdipAddPathArc, GdipClosePathFigure, GdipCreateFromHDC,
                GdipCreatePath, GdipCreatePen1, GdipCreateSolidFill, GdipDeleteBrush,
                GdipDeleteGraphics, GdipDeletePath, GdipDeletePen, GdipDrawPath, GdipFillEllipse,
                GdipFillPath, GdipSetPixelOffsetMode, GdipSetSmoothingMode, GdiplusStartup,
                GdiplusStartupInput, GpBrush, GpGraphics, GpPath, PixelOffsetModeHalf,
                SmoothingModeAntiAlias, UnitPixel,
            },
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW},
        },
        UI::{
            Controls::{DRAWITEMSTRUCT, ODS_DISABLED, ODS_FOCUS, ODS_NOFOCUSRECT, ODS_SELECTED},
            HiDpi::GetDpiForSystem,
            Input::KeyboardAndMouse::{
                EnableWindow, IsWindowEnabled, SetFocus, TME_LEAVE, TRACKMOUSEEVENT,
                TrackMouseEvent,
            },
            WindowsAndMessaging::{
                AdjustWindowRectEx, BN_CLICKED, BS_OWNERDRAW, CREATESTRUCTW, CallWindowProcW,
                CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FLASHW_ALL,
                FLASHW_TIMERNOFG, FLASHWINFO, FlashWindowEx, GWLP_USERDATA, GWLP_WNDPROC,
                GetClientRect, GetMessageW, GetParent, GetWindowLongPtrW, GetWindowTextW,
                HMENU, IDC_ARROW, IDCANCEL, IDI_SHIELD, IDOK, IsDialogMessageW, KillTimer,
                LoadCursorW, LoadIconW, MSG, NONCLIENTMETRICSW, PostMessageW, PostQuitMessage,
                RegisterClassExW, SPI_GETNONCLIENTMETRICS, SPI_GETWORKAREA, SW_SHOW,
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetForegroundWindow, SetTimer,
                SetWindowLongPtrW, ShowWindow, SystemParametersInfoW, TranslateMessage,
                WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND, WM_DESTROY,
                WM_DRAWITEM, WM_ERASEBKGND, WM_MOUSEMOVE, WM_NCCREATE, WM_PAINT,
                WM_TIMER, WM_USER, WNDCLASSEXW, WNDPROC, WS_CAPTION, WS_CHILD,
                WS_EX_CONTROLPARENT, WS_EX_DLGMODALFRAME, WS_EX_TOPMOST, WS_POPUP, WS_SYSMENU,
                WS_TABSTOP, WS_VISIBLE,
            },
        },
    },
    core::{BOOL, HSTRING, PCWSTR, w},
};

use tracing::info;

use crate::PlatformError;

/// Strings and timing of one prompt.
#[derive(Debug, Clone, Default)]
pub struct ConsentPrompt {
    pub window_title: String,
    pub heading: String,
    /// Who is asking, shown with their initials. Empty: no identity block.
    pub requester: String,
    /// Under the name: e-mail and organization.
    pub requester_detail: String,
    /// What allowing means.
    pub body: String,
    /// Last line, emphasized ("Permita somente se…").
    pub caution: String,
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
/// Repaint period of the countdown bar.
const TICK_MS: u32 = 50;
const WM_APP_DISMISS: u32 = WM_APP + 1;
/// Dialog-manager query for the default button (winuser.h).
const DM_GETDEFID: u32 = WM_USER;
const DC_HASDEFID: usize = 0x534B;
/// Sent after `TrackMouseEvent(TME_LEAVE)` when the pointer leaves (winuser.h).
const WM_MOUSELEAVE: u32 = 0x02A3;
/// Resource ID of the application icon (crates/desktop/build.rs).
const APP_ICON_ID: usize = 1;

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

// ------------------------------------------------------------------ look

/// Colors of one theme, as 0xRRGGBB.
#[derive(Clone, Copy)]
struct Colors {
    bg: u32,
    panel: u32,
    text: u32,
    muted: u32,
    faint: u32,
    hairline: u32,
    accent: u32,
    accent_hover: u32,
    accent_press: u32,
    accent_soft: u32,
    accent_text: u32,
    secondary: u32,
    secondary_hover: u32,
    secondary_press: u32,
    secondary_line: u32,
    warning: u32,
    /// Disabled "Permitir": the app's disabled primary (pale accent, dimmed label).
    disabled_fill: u32,
    disabled_text: u32,
}

const LIGHT: Colors = Colors {
    bg: 0xffffff,
    panel: 0xf4f4f6,
    text: 0x18191c,
    muted: 0x55585f,
    faint: 0x686b72,
    hairline: 0xe6e7ea,
    accent: 0x0b63e5,
    accent_hover: 0x0a58cc,
    accent_press: 0x094db3,
    accent_soft: 0xe8f0fd,
    accent_text: 0x0b5cd6,
    secondary: 0xf0f1f3,
    secondary_hover: 0xe6e7ea,
    secondary_press: 0xdcdde1,
    secondary_line: 0xe2e3e7,
    warning: 0x9a6008,
    disabled_fill: 0xe8f0fd,
    disabled_text: 0x7fa5e6,
};

const DARK: Colors = Colors {
    bg: 0x28282c,
    panel: 0x313136,
    text: 0xededef,
    muted: 0xa6a8af,
    faint: 0x95979e,
    hairline: 0x3a3a40,
    accent: 0x226ce8,
    accent_hover: 0x2670ec,
    accent_press: 0x1f62d4,
    accent_soft: 0x222e45,
    accent_text: 0x6ca6ff,
    secondary: 0x38383e,
    secondary_hover: 0x414148,
    secondary_press: 0x4a4a52,
    secondary_line: 0x44444b,
    warning: 0xe6a23c,
    disabled_fill: 0x222e45,
    disabled_text: 0x5b7fb6,
};

/// GDI color (0x00BBGGRR).
fn cref(c: u32) -> COLORREF {
    COLORREF(((c & 0xff) << 16) | (c & 0xff00) | ((c >> 16) & 0xff))
}

/// GDI+ color (0xAARRGGBB), opaque.
fn argb(c: u32) -> u32 {
    0xff00_0000 | c
}

/// Windows "Escolha o modo de aplicativo padrão": Escuro.
fn system_dark() -> bool {
    let mut value: u32 = 1;
    let mut size = std::mem::size_of::<u32>() as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("AppsUseLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut c_void),
            Some(&mut size),
        )
    };
    r.is_ok() && value == 0
}

fn gdiplus_ready() -> bool {
    static STARTED: OnceLock<bool> = OnceLock::new();
    *STARTED.get_or_init(|| {
        let mut token = 0usize;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        unsafe { GdiplusStartup(&mut token, &input, std::ptr::null_mut()).0 == 0 }
    })
}

/// Antialiased shapes on a DC (GDI+). Every method is a no-op if GDI+ is missing.
struct Canvas(*mut GpGraphics);

impl Canvas {
    fn new(hdc: HDC) -> Self {
        let mut g: *mut GpGraphics = std::ptr::null_mut();
        if gdiplus_ready() {
            unsafe {
                if GdipCreateFromHDC(hdc, &mut g).0 == 0 {
                    GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
                    GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
                } else {
                    g = std::ptr::null_mut();
                }
            }
        }
        Self(g)
    }

    fn path(x: f32, y: f32, w: f32, h: f32, r: f32) -> *mut GpPath {
        let mut p: *mut GpPath = std::ptr::null_mut();
        let d = (2.0 * r).min(w).min(h).max(0.1);
        unsafe {
            GdipCreatePath(FillModeAlternate, &mut p);
            GdipAddPathArc(p, x, y, d, d, 180.0, 90.0);
            GdipAddPathArc(p, x + w - d, y, d, d, 270.0, 90.0);
            GdipAddPathArc(p, x + w - d, y + h - d, d, d, 0.0, 90.0);
            GdipAddPathArc(p, x, y + h - d, d, d, 90.0, 90.0);
            GdipClosePathFigure(p);
        }
        p
    }

    fn fill_round(&self, x: f32, y: f32, w: f32, h: f32, r: f32, color: u32) {
        if self.0.is_null() || w <= 0.0 || h <= 0.0 {
            return;
        }
        unsafe {
            let p = Self::path(x, y, w, h, r);
            let mut b = std::ptr::null_mut();
            GdipCreateSolidFill(argb(color), &mut b);
            GdipFillPath(self.0, b as *mut GpBrush, p);
            GdipDeleteBrush(b as *mut GpBrush);
            GdipDeletePath(p);
        }
    }

    /// Outlines the rounded rectangle `[x, y, w, h]`.
    fn stroke_round(&self, [x, y, w, h]: [f32; 4], r: f32, color: u32, width: f32) {
        if self.0.is_null() {
            return;
        }
        unsafe {
            let p = Self::path(x, y, w, h, r);
            let mut pen = std::ptr::null_mut();
            GdipCreatePen1(argb(color), width, UnitPixel, &mut pen);
            GdipDrawPath(self.0, pen, p);
            GdipDeletePen(pen);
            GdipDeletePath(p);
        }
    }

    fn fill_circle(&self, cx: f32, cy: f32, r: f32, color: u32) {
        if self.0.is_null() {
            return;
        }
        unsafe {
            let mut b = std::ptr::null_mut();
            GdipCreateSolidFill(argb(color), &mut b);
            GdipFillEllipse(self.0, b as *mut GpBrush, cx - r, cy - r, 2.0 * r, 2.0 * r);
            GdipDeleteBrush(b as *mut GpBrush);
        }
    }

    /// The Aionix Connect mark: two landscape screens that overlap (the front one on a
    /// stand), the shared region filled, on the blue tile (same geometry as
    /// infra/branding/make-icons.py).
    fn mark(&self, x: f32, y: f32, size: f32) {
        let s = size / 24.0;
        self.fill_round(x, y, size, size, size * 0.225, 0x1b6aeb);
        let w = if size <= 32.0 { 2.1 } else { 1.7 } * s;
        for (x0, y0, x1, y1) in [(3.0, 3.6, 15.6, 11.8), (8.4, 8.8, 21.0, 17.0)] {
            self.stroke_round(
                [
                    x + (x0 * s) + w / 2.0,
                    y + (y0 * s) + w / 2.0,
                    (x1 - x0) * s - w,
                    (y1 - y0) * s - w,
                ],
                (1.6 * s - w / 2.0).max(0.5),
                0xffffff,
                w,
            );
        }
        for (x0, y0, x1, y1, r) in [
            (8.4, 8.8, 15.6, 11.8, 0.5),
            (13.85, 17.0, 15.55, 19.4, 0.4),
            (11.6, 19.0, 17.8, 20.6, 0.6),
        ] {
            self.fill_round(x + x0 * s, y + y0 * s, (x1 - x0) * s, (y1 - y0) * s, r * s, 0xffffff);
        }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                GdipDeleteGraphics(self.0);
            }
        }
    }
}

struct Font(HFONT);

impl Drop for Font {
    fn drop(&mut self) {
        unsafe {
            let _ = DeleteObject(HGDIOBJ(self.0.0));
        }
    }
}

struct Fonts {
    heading: Font,
    name: Font,
    body: Font,
    strong: Font,
    small: Font,
    button: Font,
    initials: Font,
}

/// Segoe UI (the system message font) at pixel `size`, `weight` 400/600.
fn fonts(scale: impl Fn(f32) -> i32) -> Result<Fonts, PlatformError> {
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
    }
    let make = |size: f32, weight: i32| {
        let mut lf = ncm.lfMessageFont;
        lf.lfHeight = -scale(size);
        lf.lfWidth = 0;
        lf.lfWeight = weight;
        lf.lfQuality = CLEARTYPE_QUALITY;
        Font(unsafe { CreateFontIndirectW(&lf) })
    };
    Ok(Fonts {
        heading: make(18.0, 600),
        name: make(15.0, 600),
        body: make(14.0, 400),
        strong: make(14.0, 600),
        small: make(12.5, 400),
        button: make(14.0, 600),
        initials: make(14.0, 600),
    })
}

fn wide(s: &str) -> Vec<u16> {
    s.replace("\r\n", "\n").replace('\n', "\r\n").encode_utf16().collect()
}

/// Height of `text` wrapped at `width` (0 for empty text).
fn measure(text: &str, font: &Font, width: i32) -> i32 {
    if text.is_empty() {
        return 0;
    }
    let mut buf = wide(text);
    let mut rc = RECT {
        left: 0,
        top: 0,
        right: width,
        bottom: 0,
    };
    unsafe {
        let hdc = GetDC(None);
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        DrawTextW(hdc, &mut buf, &mut rc, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
        SelectObject(hdc, old);
        ReleaseDC(None, hdc);
    }
    rc.bottom - rc.top
}

/// Width of single-line `text`.
fn measure_width(text: &str, font: &Font) -> i32 {
    let mut buf = wide(text);
    let mut rc = RECT::default();
    unsafe {
        let hdc = GetDC(None);
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        DrawTextW(hdc, &mut buf, &mut rc, DT_CALCRECT | DT_SINGLELINE | DT_NOPREFIX);
        SelectObject(hdc, old);
        ReleaseDC(None, hdc);
    }
    rc.right - rc.left
}

fn draw_text(hdc: HDC, text: &str, rc: RECT, font: &Font, color: u32, flags: DRAW_TEXT_FORMAT) {
    let mut buf = wide(text);
    let mut rc = rc;
    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        SetTextColor(hdc, cref(color));
        let old = SelectObject(hdc, HGDIOBJ(font.0.0));
        DrawTextW(hdc, &mut buf, &mut rc, flags | DT_NOPREFIX);
        SelectObject(hdc, old);
    }
}

fn countdown_text(template: &str, seconds: u64) -> String {
    template.replace("{s}", &seconds.to_string())
}

fn initials(name: &str) -> String {
    name.split_whitespace()
        .filter_map(|w| w.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect()
}

fn rect(x: i32, y: i32, w: i32, h: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + w,
        bottom: y + h,
    }
}

/// Where everything goes, in client pixels.
#[derive(Default)]
struct Geometry {
    mark: RECT,
    heading: RECT,
    panel: RECT,
    avatar: RECT,
    name: RECT,
    detail: RECT,
    body: RECT,
    caution: RECT,
    countdown: RECT,
    bar: RECT,
}

// ------------------------------------------------------------------ window

struct DialogState {
    prompt: ConsentPrompt,
    shown_at: Instant,
    deadline: Instant,
    allow_at: Instant,
    allow_enabled: bool,
    shown_seconds: u64,
    allow: HWND,
    deny: HWND,
    /// Button under the mouse.
    hover: HWND,
    result: Option<ConsentResult>,
    colors: Colors,
    fonts: Fonts,
    geo: Geometry,
    initials: String,
}

fn register_class(instance: HINSTANCE) -> Result<(), PlatformError> {
    static REGISTERED: OnceLock<Result<(), String>> = OnceLock::new();
    REGISTERED
        .get_or_init(|| unsafe {
            let icon = LoadIconW(Some(instance), PCWSTR(APP_ICON_ID as *const u16))
                .or_else(|_| LoadIconW(None, IDI_SHIELD))
                .unwrap_or_default();
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(wndproc),
                hInstance: instance,
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hIcon: icon,
                hIconSm: icon,
                hbrBackground: HBRUSH::default(),
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

fn button(
    parent: HWND,
    instance: HINSTANCE,
    text: &str,
    id: i32,
    (x, y, w, h): (i32, i32, i32, i32),
) -> Result<HWND, PlatformError> {
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            &HSTRING::from(text),
            WINDOW_STYLE(WS_CHILD.0 | WS_VISIBLE.0 | WS_TABSTOP.0 | BS_OWNERDRAW as u32),
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
    // Hover tracking: our procedure in front of the system one, which stays in charge
    // of focus, keyboard and click handling.
    unsafe {
        let old = SetWindowLongPtrW(hwnd, GWLP_WNDPROC, button_proc as *const () as usize as isize);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, old);
    }
    Ok(hwnd)
}

/// Windows 11 frame in the prompt's colors; older systems ignore what they lack.
fn style_frame(hwnd: HWND, dark: bool, c: &Colors) {
    unsafe {
        let set = |attr, value: *const c_void, size: usize| {
            let _ = DwmSetWindowAttribute(hwnd, attr, value, size as u32);
        };
        let dark = BOOL::from(dark);
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, &dark as *const _ as _, std::mem::size_of::<BOOL>());
        let corner = DWMWCP_ROUND;
        set(DWMWA_WINDOW_CORNER_PREFERENCE, &corner as *const _ as _, std::mem::size_of_val(&corner));
        for (attr, color) in [
            (DWMWA_CAPTION_COLOR, c.bg),
            (DWMWA_TEXT_COLOR, c.text),
            (DWMWA_BORDER_COLOR, c.hairline),
        ] {
            let v = cref(color);
            set(attr, &v as *const _ as _, std::mem::size_of::<COLORREF>());
        }
    }
}

fn run(prompt: ConsentPrompt, shared: &Mutex<Shared>) -> Result<ConsentResult, PlatformError> {
    let instance: HINSTANCE = unsafe { GetModuleHandleW(None)? }.into();
    register_class(instance)?;

    let dpi = unsafe { GetDpiForSystem() }.max(96) as f32;
    let s = |v: f32| (v * dpi / 96.0).round() as i32;
    let fonts = fonts(s)?;
    let dark = system_dark();
    let colors = if dark { DARK } else { LIGHT };

    // ---- layout
    let width = s(460.0);
    let pad = s(28.0);
    let inner = width - 2 * pad;
    let mut geo = Geometry::default();
    let mut y = pad;
    let mark = s(40.0);
    let heading_x = pad + mark + s(14.0);
    let heading_w = width - pad - heading_x;
    let heading_h = measure(&prompt.heading, &fonts.heading, heading_w);
    let row = mark.max(heading_h);
    geo.mark = rect(pad, y + (row - mark) / 2, mark, mark);
    geo.heading = rect(heading_x, y + (row - heading_h) / 2, heading_w, heading_h);
    y += row + s(20.0);
    if !prompt.requester.is_empty() {
        let (ip, av, gap) = (s(14.0), s(36.0), s(12.0));
        let text_x = pad + ip + av + gap;
        let text_w = inner - (ip + av + gap + ip);
        let name_h = measure(&prompt.requester, &fonts.name, text_w);
        let detail_h = measure(&prompt.requester_detail, &fonts.small, text_w);
        let content = av.max(name_h + if detail_h > 0 { s(2.0) + detail_h } else { 0 });
        let panel_h = content + 2 * ip;
        geo.panel = rect(pad, y, inner, panel_h);
        geo.avatar = rect(pad + ip, y + (panel_h - av) / 2, av, av);
        let ty = y + (panel_h - (name_h + if detail_h > 0 { s(2.0) + detail_h } else { 0 })) / 2;
        geo.name = rect(text_x, ty, text_w, name_h);
        geo.detail = rect(text_x, ty + name_h + s(2.0), text_w, detail_h);
        y += panel_h + s(18.0);
    }
    let body_h = measure(&prompt.body, &fonts.body, inner);
    geo.body = rect(pad, y, inner, body_h);
    y += body_h + if body_h > 0 { s(8.0) } else { 0 };
    let caution_h = measure(&prompt.caution, &fonts.strong, inner);
    geo.caution = rect(pad, y, inner, caution_h);
    y += caution_h + s(22.0);
    let total_secs = prompt.timeout.as_secs().max(1);
    let countdown_h = measure(&countdown_text(&prompt.countdown_template, total_secs), &fonts.small, inner);
    geo.countdown = rect(pad, y, inner, countdown_h);
    y += countdown_h + s(8.0);
    geo.bar = rect(pad, y, inner, s(3.0).max(2));
    y += s(3.0) + s(24.0);
    let btn_h = s(36.0);
    let btn_w = |label: &str| (measure_width(label, &fonts.button) + s(40.0)).max(s(108.0));
    let (allow_w, deny_w) = (btn_w(&prompt.allow_label), btn_w(&prompt.deny_label));
    let gap = s(10.0);
    let buttons_y = y;
    let client_h = buttons_y + btn_h + s(24.0);

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
    let wy = work.top + ((work.bottom - work.top) - oh).max(0) / 2;

    let now = Instant::now();
    let initials = initials(&prompt.requester);
    let window_title = prompt.window_title.clone();
    let (allow_label, deny_label) = (prompt.allow_label.clone(), prompt.deny_label.clone());
    let mut state = Box::new(DialogState {
        shown_at: now,
        deadline: now + prompt.timeout,
        allow_at: now + prompt.allow_delay,
        allow_enabled: prompt.allow_delay.is_zero(),
        shown_seconds: total_secs,
        prompt,
        allow: HWND::default(),
        deny: HWND::default(),
        hover: HWND::default(),
        result: None,
        colors,
        fonts,
        geo,
        initials,
    });
    let state_ptr: *mut DialogState = &mut *state;

    let hwnd = unsafe {
        CreateWindowExW(
            ex_style,
            CLASS_NAME,
            &HSTRING::from(window_title.as_str()),
            style,
            x,
            wy,
            ow,
            oh,
            None,
            None,
            Some(instance),
            Some(state_ptr as *const c_void),
        )?
    };
    style_frame(hwnd, dark, &colors);

    let build = || -> Result<(HWND, HWND), PlatformError> {
        let deny = button(
            hwnd,
            instance,
            &deny_label,
            ID_DENY,
            (width - pad - allow_w - gap - deny_w, buttons_y, deny_w, btn_h),
        )?;
        let allow = button(
            hwnd,
            instance,
            &allow_label,
            ID_ALLOW,
            (width - pad - allow_w, buttons_y, allow_w, btn_h),
        )?;
        Ok((deny, allow))
    };
    let (deny, allow) = match build() {
        Ok(v) => v,
        Err(e) => {
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
            return Err(e);
        }
    };
    state.allow = allow;
    state.deny = deny;
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
            SetTimer(Some(hwnd), TIMER_ID, TICK_MS, None);
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

// ------------------------------------------------------------------ painting

/// Paints `f` into an off-screen bitmap the size of `rc`, then copies it to `hdc`.
fn buffered(hdc: HDC, rc: RECT, f: impl FnOnce(HDC)) {
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    if w <= 0 || h <= 0 {
        return;
    }
    unsafe {
        let mem = CreateCompatibleDC(Some(hdc));
        let bmp = CreateCompatibleBitmap(hdc, w, h);
        let old = SelectObject(mem, HGDIOBJ(bmp.0));
        f(mem);
        let _ = BitBlt(hdc, rc.left, rc.top, w, h, Some(mem), 0, 0, SRCCOPY);
        SelectObject(mem, old);
        let _ = DeleteObject(HGDIOBJ(bmp.0));
        let _ = DeleteDC(mem);
    }
}

fn fill(hdc: HDC, rc: RECT, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(cref(color));
        FillRect(hdc, &rc, brush);
        let _ = DeleteObject(HGDIOBJ(brush.0));
    }
}

fn paint_window(hdc: HDC, client: RECT, st: &DialogState) {
    let c = &st.colors;
    let g = &st.geo;
    let p = &st.prompt;
    fill(hdc, client, c.bg);
    {
        let canvas = Canvas::new(hdc);
        canvas.mark(g.mark.left as f32, g.mark.top as f32, (g.mark.right - g.mark.left) as f32);
        if !p.requester.is_empty() {
            let r = &g.panel;
            canvas.fill_round(
                r.left as f32,
                r.top as f32,
                (r.right - r.left) as f32,
                (r.bottom - r.top) as f32,
                ((g.avatar.bottom - g.avatar.top) as f32 * 0.3).max(6.0),
                c.panel,
            );
            let a = &g.avatar;
            let rad = (a.right - a.left) as f32 / 2.0;
            canvas.fill_circle(a.left as f32 + rad, a.top as f32 + rad, rad, c.accent_soft);
        }
        // Countdown bar: the time left, emptying towards the left.
        let b = &g.bar;
        let (bw, bh) = ((b.right - b.left) as f32, (b.bottom - b.top) as f32);
        canvas.fill_round(b.left as f32, b.top as f32, bw, bh, bh / 2.0, c.hairline);
        let total = p.timeout.as_secs_f32().max(0.001);
        let left = st.deadline.saturating_duration_since(Instant::now()).as_secs_f32();
        let frac = (left / total).clamp(0.0, 1.0);
        let urgent = st.shown_seconds <= 10;
        if frac > 0.0 {
            canvas.fill_round(
                b.left as f32,
                b.top as f32,
                (bw * frac).max(bh),
                bh,
                bh / 2.0,
                if urgent { c.warning } else { c.accent },
            );
        }
    }
    let wrap = DT_LEFT | DT_WORDBREAK;
    draw_text(hdc, &p.heading, g.heading, &st.fonts.heading, c.text, wrap);
    if !p.requester.is_empty() {
        draw_text(
            hdc,
            &st.initials,
            g.avatar,
            &st.fonts.initials,
            c.accent_text,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        draw_text(hdc, &p.requester, g.name, &st.fonts.name, c.text, wrap);
        draw_text(hdc, &p.requester_detail, g.detail, &st.fonts.small, c.muted, wrap);
    }
    draw_text(hdc, &p.body, g.body, &st.fonts.body, c.muted, wrap);
    draw_text(hdc, &p.caution, g.caution, &st.fonts.strong, c.text, wrap);
    let urgent = st.shown_seconds <= 10;
    draw_text(
        hdc,
        &countdown_text(&p.countdown_template, st.shown_seconds),
        g.countdown,
        &st.fonts.small,
        if urgent { c.warning } else { c.faint },
        DT_LEFT | DT_SINGLELINE | DT_END_ELLIPSIS,
    );
}

fn paint_button(dis: &DRAWITEMSTRUCT, st: &DialogState) {
    let c = &st.colors;
    let primary = dis.hwndItem == st.allow;
    let disabled = (dis.itemState.0 & ODS_DISABLED.0) != 0;
    let pressed = (dis.itemState.0 & ODS_SELECTED.0) != 0;
    let focused = (dis.itemState.0 & ODS_FOCUS.0) != 0 && (dis.itemState.0 & ODS_NOFOCUSRECT.0) == 0;
    let hovered = !disabled && dis.hwndItem == st.hover;
    let (fill_c, text_c) = match (primary, disabled) {
        (_, true) => (c.disabled_fill, c.disabled_text),
        (true, false) => (
            if pressed {
                c.accent_press
            } else if hovered {
                c.accent_hover
            } else {
                c.accent
            },
            0xffffff,
        ),
        (false, false) => (
            if pressed {
                c.secondary_press
            } else if hovered {
                c.secondary_hover
            } else {
                c.secondary
            },
            c.text,
        ),
    };
    let mut buf = [0u16; 64];
    let len = unsafe { GetWindowTextW(dis.hwndItem, &mut buf) }.max(0) as usize;
    let label = String::from_utf16_lossy(&buf[..len]);
    let rc = dis.rcItem;
    buffered(dis.hDC, rc, |hdc| {
        let local = RECT {
            left: 0,
            top: 0,
            right: rc.right - rc.left,
            bottom: rc.bottom - rc.top,
        };
        fill(hdc, local, c.bg);
        let (w, h) = (local.right as f32, local.bottom as f32);
        let radius = h * 0.24;
        {
            let canvas = Canvas::new(hdc);
            // Room for the focus ring inside the control.
            let inset = if focused { 3.0 } else { 0.0 };
            canvas.fill_round(inset, inset, w - 2.0 * inset, h - 2.0 * inset, radius - inset * 0.5, fill_c);
            if !primary && !disabled && !focused {
                canvas.stroke_round([0.5, 0.5, w - 1.0, h - 1.0], radius, c.secondary_line, 1.0);
            }
            if focused {
                // Neutral ring with a gap, so the focused "Recusar" never reads as a
                // second blue button.
                canvas.stroke_round([1.0, 1.0, w - 2.0, h - 2.0], radius, c.muted, 1.5);
            }
        }
        let shift = if pressed { 1 } else { 0 };
        let text_rc = RECT {
            top: local.top + shift,
            bottom: local.bottom + shift,
            ..local
        };
        draw_text(
            hdc,
            &label,
            text_rc,
            &st.fonts.button,
            text_c,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    });
}

// ------------------------------------------------------------------ messages

enum Action {
    None,
    Finish(ConsentResult),
}

/// Front of the system BUTTON procedure: tracks hover for painting only.
unsafe extern "system" fn button_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    unsafe {
        let parent = GetParent(hwnd).unwrap_or_default();
        let state = GetWindowLongPtrW(parent, GWLP_USERDATA) as *mut DialogState;
        if !state.is_null() {
            match msg {
                WM_MOUSEMOVE if (*state).hover != hwnd => {
                    let previous = (*state).hover;
                    (*state).hover = hwnd;
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    let _ = TrackMouseEvent(&mut track);
                    let _ = InvalidateRect(Some(hwnd), None, false);
                    if !previous.is_invalid() {
                        let _ = InvalidateRect(Some(previous), None, false);
                    }
                }
                WM_MOUSELEAVE if (*state).hover == hwnd => {
                    (*state).hover = HWND::default();
                    let _ = InvalidateRect(Some(hwnd), None, false);
                }
                _ => {}
            }
        }
        let old: WNDPROC = std::mem::transmute(GetWindowLongPtrW(hwnd, GWLP_USERDATA));
        CallWindowProcW(old, hwnd, msg, wparam, lparam)
    }
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
                    } else if id == ID_DENY || id == IDCANCEL.0 || id == IDOK.0 {
                        // IDOK: Enter with no default button resolved still refuses.
                        Action::Finish(ConsentResult::Denied)
                    } else {
                        Action::None
                    }
                }
                // The dialog manager asks which button Enter presses: "Recusar".
                DM_GETDEFID => return LRESULT(((DC_HASDEFID << 16) | ID_DENY as usize) as isize),
                WM_CLOSE => Action::Finish(ConsentResult::Denied),
                WM_APP_DISMISS => Action::Finish(ConsentResult::Dismissed),
                WM_TIMER => {
                    let now = Instant::now();
                    if !state.allow_enabled && now >= state.allow_at {
                        state.allow_enabled = true;
                        let _ = EnableWindow(state.allow, true);
                        let _ = InvalidateRect(Some(state.allow), None, false);
                    }
                    let remaining = state.deadline.saturating_duration_since(now);
                    if remaining.is_zero() {
                        Action::Finish(ConsentResult::TimedOut)
                    } else {
                        let secs = remaining.as_secs() + u64::from(remaining.subsec_nanos() > 0);
                        state.shown_seconds = secs;
                        let g = &state.geo;
                        let dirty = RECT {
                            left: g.countdown.left,
                            top: g.countdown.top,
                            right: g.bar.right,
                            bottom: g.bar.bottom + 1,
                        };
                        let _ = InvalidateRect(Some(hwnd), Some(&dirty), false);
                        Action::None
                    }
                }
                WM_ERASEBKGND => return LRESULT(1),
                WM_PAINT => {
                    let mut ps = PAINTSTRUCT::default();
                    let hdc = BeginPaint(hwnd, &mut ps);
                    let mut client = RECT::default();
                    let _ = GetClientRect(hwnd, &mut client);
                    buffered(hdc, client, |mem| paint_window(mem, client, state));
                    let _ = EndPaint(hwnd, &ps);
                    return LRESULT(0);
                }
                WM_DRAWITEM => {
                    let dis = &*(lparam.0 as *const DRAWITEMSTRUCT);
                    paint_button(dis, state);
                    return LRESULT(1);
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
            requester: "Ana Souza".into(),
            requester_detail: "ana@exemplo.com.br".into(),
            body: "Esta janela fecha sozinha.".into(),
            caution: String::new(),
            allow_label: "Permitir".into(),
            deny_label: "Recusar".into(),
            countdown_template: "{s} s".into(),
            timeout,
            allow_delay: Duration::from_secs(5),
        }
    }

    #[test]
    fn initials_skip_symbols() {
        assert_eq!(initials("Ana Souza"), "AS");
        assert_eq!(initials("  (ana) maria  luz"), "AM");
        assert_eq!(initials(""), "");
    }

    #[test]
    fn colors_convert_to_gdi_order() {
        assert_eq!(cref(0x112233).0, 0x00332211);
        assert_eq!(argb(0x112233), 0xff112233);
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
