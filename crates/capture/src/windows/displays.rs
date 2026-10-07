use std::collections::HashMap;

use windows::Win32::{
    Devices::Display::{
        DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
        DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME,
        DISPLAYCONFIG_TARGET_DEVICE_NAME, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes,
        QDC_ONLY_ACTIVE_PATHS, QueryDisplayConfig,
    },
    Foundation::ERROR_SUCCESS,
    Graphics::Gdi::HMONITOR,
    Graphics::Dxgi::{
        Common::{DXGI_MODE_ROTATION_ROTATE90, DXGI_MODE_ROTATION_ROTATE180, DXGI_MODE_ROTATION_ROTATE270},
        CreateDXGIFactory1, DXGI_ERROR_NOT_FOUND,
        IDXGIAdapter1, IDXGIFactory1,
        IDXGIOutput,
    },
};

use crate::{CaptureError, DisplayInfo};

/// A display together with the DXGI objects needed to duplicate it.
pub(crate) struct DxgiDisplay {
    pub info: DisplayInfo,
    pub adapter: IDXGIAdapter1,
    pub output: IDXGIOutput,
    pub monitor: HMONITOR,
}

pub(crate) fn wide_to_string(w: &[u16]) -> String {
    let end = w.iter().position(|&c| c == 0).unwrap_or(w.len());
    String::from_utf16_lossy(&w[..end])
}

/// Lists the displays attached to the desktop, primary first.
pub fn enumerate_displays() -> Result<Vec<DisplayInfo>, CaptureError> {
    Ok(enumerate_dxgi()?.into_iter().map(|d| d.info).collect())
}

pub(crate) fn enumerate_dxgi() -> Result<Vec<DxgiDisplay>, CaptureError> {
    let friendly = friendly_names();
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut out = Vec::new();
    let mut adapter_index = 0;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        adapter_index += 1;
        let mut output_index = 0;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            output_index += 1;
            let desc = unsafe { output.GetDesc()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let r = desc.DesktopCoordinates;
            let device_name = wide_to_string(&desc.DeviceName);
            let rotation = match desc.Rotation {
                DXGI_MODE_ROTATION_ROTATE90 => 90,
                DXGI_MODE_ROTATION_ROTATE180 => 180,
                DXGI_MODE_ROTATION_ROTATE270 => 270,
                _ => 0,
            };
            out.push(DxgiDisplay {
                info: DisplayInfo {
                    id: 0,
                    name: friendly.get(&device_name).cloned().unwrap_or_else(|| device_name.clone()),
                    device_name,
                    x: r.left,
                    y: r.top,
                    width: (r.right - r.left) as u32,
                    height: (r.bottom - r.top) as u32,
                    // The primary display is the one at the desktop origin.
                    primary: r.left == 0 && r.top == 0,
                    rotation,
                },
                adapter: adapter.clone(),
                monitor: desc.Monitor,
                output,
            });
        }
    }
    // Stable ordering: primary first, then left-to-right, top-to-bottom.
    out.sort_by_key(|d| (!d.info.primary, d.info.x, d.info.y));
    for (i, d) in out.iter_mut().enumerate() {
        d.info.id = i as u32 + 1;
    }
    Ok(out)
}

/// Maps GDI device names (`\\.\DISPLAY1`) to monitor names (`DELL U2720Q`).
fn friendly_names() -> HashMap<String, String> {
    let mut map = HashMap::new();
    unsafe {
        let (mut n_paths, mut n_modes) = (0u32, 0u32);
        if GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut n_paths, &mut n_modes) != ERROR_SUCCESS {
            return map;
        }
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); n_paths as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); n_modes as usize];
        if QueryDisplayConfig(
            QDC_ONLY_ACTIVE_PATHS,
            &mut n_paths,
            paths.as_mut_ptr(),
            &mut n_modes,
            modes.as_mut_ptr(),
            None,
        ) != ERROR_SUCCESS
        {
            return map;
        }
        for path in paths.iter().take(n_paths as usize) {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME::default();
            source.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME;
            source.header.size = std::mem::size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32;
            source.header.adapterId = path.sourceInfo.adapterId;
            source.header.id = path.sourceInfo.id;
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 {
                continue;
            }
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME::default();
            target.header.r#type = DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME;
            target.header.size = std::mem::size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32;
            target.header.adapterId = path.targetInfo.adapterId;
            target.header.id = path.targetInfo.id;
            if DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                continue;
            }
            let name = wide_to_string(&target.monitorFriendlyDeviceName);
            if !name.is_empty() {
                map.insert(wide_to_string(&source.viewGdiDeviceName), name);
            }
        }
    }
    map
}
