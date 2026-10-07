//! Input desktop tracking.
//!
//! Windows shows the lock screen, the Ctrl+Alt+Del screen and UAC prompts on the
//! *Winlogon* desktop. A thread can only capture and inject into the desktop it is
//! attached to, so hosts re-attach their media/input threads whenever the input desktop
//! changes. Only a SYSTEM process running in the user's session (the Agent's session
//! host) may open the Winlogon desktop; Quick Support reports it as unavailable.

use windows::{
    Win32::{
        Foundation::HANDLE,
        System::StationsAndDesktops::{
            CloseDesktop, DESKTOP_ACCESS_FLAGS, DESKTOP_CONTROL_FLAGS, GetUserObjectInformationW,
            HDESK, OpenInputDesktop, SetThreadDesktop, UOI_NAME,
        },
    },
};

use crate::PlatformError;

/// `GENERIC_ALL` is required to capture and inject on the secure desktop.
const DESKTOP_GENERIC_ALL: u32 = 0x1000_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputDesktop {
    /// The normal user desktop.
    Default,
    /// Secure desktop: lock screen, UAC, Ctrl+Alt+Del.
    Winlogon,
    /// Screen saver or another named desktop.
    Other(String),
}

impl InputDesktop {
    pub fn is_secure(&self) -> bool {
        matches!(self, Self::Winlogon)
    }

    fn from_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "default" => Self::Default,
            "winlogon" => Self::Winlogon,
            _ => Self::Other(name.to_owned()),
        }
    }
}

fn desktop_name(desk: HDESK) -> Option<String> {
    let mut buf = [0u16; 128];
    let mut needed = 0u32;
    unsafe {
        GetUserObjectInformationW(
            HANDLE(desk.0),
            UOI_NAME,
            Some(buf.as_mut_ptr().cast()),
            (buf.len() * 2) as u32,
            Some(&mut needed),
        )
        .ok()?;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Returns the desktop currently receiving user input, or `AccessDenied` when this
/// process may not see it (a non-SYSTEM process while the secure desktop is active).
pub fn current_input_desktop() -> Result<InputDesktop, PlatformError> {
    let desk = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(0x0001)) }
        .map_err(|_| PlatformError::AccessDenied)?;
    let name = desktop_name(desk);
    unsafe {
        let _ = CloseDesktop(desk);
    }
    Ok(InputDesktop::from_name(&name.unwrap_or_default()))
}

/// Attaches the calling thread to the current input desktop. The thread must not own
/// windows or hooks. Returns which desktop it is now attached to.
pub fn attach_thread_to_input_desktop() -> Result<InputDesktop, PlatformError> {
    let desk = unsafe { OpenInputDesktop(DESKTOP_CONTROL_FLAGS(0), false, DESKTOP_ACCESS_FLAGS(DESKTOP_GENERIC_ALL)) }
        .map_err(|_| PlatformError::AccessDenied)?;
    let name = desktop_name(desk).unwrap_or_default();
    let result = unsafe { SetThreadDesktop(desk) };
    // The thread keeps the desktop referenced while attached; our handle can go.
    unsafe {
        let _ = CloseDesktop(desk);
    }
    result.map_err(PlatformError::from)?;
    Ok(InputDesktop::from_name(&name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_desktop_names() {
        assert_eq!(InputDesktop::from_name("Default"), InputDesktop::Default);
        assert!(InputDesktop::from_name("Winlogon").is_secure());
        assert_eq!(InputDesktop::from_name("Screen-saver"), InputDesktop::Other("Screen-saver".into()));
    }
}
