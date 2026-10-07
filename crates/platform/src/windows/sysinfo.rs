//! Machine description for the inventory and the technician's device panel.

use windows::{
    Win32::System::{
        Registry::{HKEY, HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
        RemoteDesktop::{
            WTS_CURRENT_SERVER_HANDLE, WTSFreeMemory, WTSGetActiveConsoleSessionId,
            WTSQuerySessionInformationW, WTSUserName,
        },
        SystemInformation::{
            ComputerNamePhysicalDnsHostname, GetComputerNameExW, GlobalMemoryStatusEx,
            MEMORYSTATUSEX,
        },
    },
    core::{HSTRING, PWSTR},
};

use crate::SystemInfo;

fn reg_string(key: HKEY, path: &str, name: &str) -> Option<String> {
    let mut buf = [0u16; 512];
    let mut len = (buf.len() * 2) as u32;
    unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    let chars = (len as usize / 2).saturating_sub(1);
    let s = String::from_utf16_lossy(&buf[..chars.min(buf.len())])
        .trim()
        .to_owned();
    (!s.is_empty()).then_some(s)
}

fn reg_dword(key: HKEY, path: &str, name: &str) -> Option<u32> {
    let mut value = 0u32;
    let mut len = 4u32;
    unsafe {
        RegGetValueW(
            key,
            &HSTRING::from(path),
            &HSTRING::from(name),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut value as *mut u32).cast()),
            Some(&mut len),
        )
        .ok()
        .ok()?;
    }
    Some(value)
}

/// Human OS name. Windows 11 still reports "Windows 10" in `ProductName`, so the build
/// number decides.
pub(crate) fn os_name(product: &str, build: u32, edition: &str) -> String {
    let family = if build >= 22000 {
        "Windows 11"
    } else if build >= 10240 {
        "Windows 10"
    } else {
        "Windows"
    };
    if product.contains("Server") {
        return product.to_owned();
    }
    let edition = match edition {
        "Core" | "CoreSingleLanguage" | "CoreCountrySpecific" => "Home",
        "Professional" => "Pro",
        "ProfessionalWorkstation" => "Pro for Workstations",
        "Enterprise" => "Enterprise",
        "Education" => "Education",
        "IoTEnterprise" => "IoT Enterprise",
        other => other,
    };
    if edition.is_empty() {
        family.to_owned()
    } else {
        format!("{family} {edition}")
    }
}

/// Collects system information. Cheap enough to call on every session start.
pub fn system_info() -> SystemInfo {
    const NT: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
    let product = reg_string(HKEY_LOCAL_MACHINE, NT, "ProductName").unwrap_or_default();
    let build: u32 = reg_string(HKEY_LOCAL_MACHINE, NT, "CurrentBuild")
        .and_then(|b| b.parse().ok())
        .unwrap_or(0);
    let ubr = reg_dword(HKEY_LOCAL_MACHINE, NT, "UBR").unwrap_or(0);
    let display_version = reg_string(HKEY_LOCAL_MACHINE, NT, "DisplayVersion").unwrap_or_default();
    let edition = reg_string(HKEY_LOCAL_MACHINE, NT, "EditionID").unwrap_or_default();
    let cpu = reg_string(
        HKEY_LOCAL_MACHINE,
        r"HARDWARE\DESCRIPTION\System\CentralProcessor\0",
        "ProcessorNameString",
    )
    .unwrap_or_default();

    let mut mem = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    let memory_total_bytes = if unsafe { GlobalMemoryStatusEx(&mut mem) }.is_ok() {
        mem.ullTotalPhys
    } else {
        0
    };

    let os_version = if display_version.is_empty() {
        format!("{build}.{ubr}")
    } else {
        format!("{display_version} (build {build}.{ubr})")
    };

    SystemInfo {
        hostname: hostname(),
        os_name: os_name(&product, build, &edition),
        os_version,
        cpu,
        memory_total_bytes,
        username: console_user().unwrap_or_else(|| std::env::var("USERNAME").unwrap_or_default()),
    }
}

pub fn hostname() -> String {
    let mut buf = [0u16; 256];
    let mut len = buf.len() as u32;
    if unsafe {
        GetComputerNameExW(
            ComputerNamePhysicalDnsHostname,
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
    }
    .is_ok()
    {
        return String::from_utf16_lossy(&buf[..len as usize]);
    }
    std::env::var("COMPUTERNAME").unwrap_or_default()
}

/// User logged on at the physical console (works from services running as SYSTEM).
pub fn console_user() -> Option<String> {
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX {
        return None;
    }
    let mut ptr = PWSTR::null();
    let mut bytes = 0u32;
    unsafe {
        WTSQuerySessionInformationW(
            Some(WTS_CURRENT_SERVER_HANDLE),
            session,
            WTSUserName,
            &mut ptr,
            &mut bytes,
        )
    }
    .ok()?;
    let name = unsafe { ptr.to_string() }.ok();
    unsafe { WTSFreeMemory(ptr.0.cast()) };
    name.filter(|n| !n.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_11_is_detected_by_build() {
        assert_eq!(
            os_name("Windows 10 Pro", 22631, "Professional"),
            "Windows 11 Pro"
        );
        assert_eq!(os_name("Windows 10 Home", 19045, "Core"), "Windows 10 Home");
        assert_eq!(
            os_name("Windows Server 2022 Standard", 20348, "ServerStandard"),
            "Windows Server 2022 Standard"
        );
    }

    #[test]
    fn collects_this_machine() {
        let info = system_info();
        assert!(!info.hostname.is_empty());
        assert!(info.os_name.starts_with("Windows"));
        assert!(info.memory_total_bytes > 0);
    }
}
