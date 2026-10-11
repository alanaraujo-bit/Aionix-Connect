//! Per-user installation (no administrator prompt): files under
//! `%LOCALAPPDATA%\Programs\<product>`, Start Menu / Desktop shortcuts and the
//! "Apps & features" entry under HKCU. App data (`%LOCALAPPDATA%\Aionix Connect`, which
//! holds the device identity and therefore the 9-digit ID) is never touched by install
//! or update, and only removed on uninstall when the user explicitly asks.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// Static description of the installable product.
#[derive(Debug, Clone, Copy)]
pub struct ProductInfo {
    /// Name shown everywhere.
    pub display: &'static str,
    /// Install folder name under `%LOCALAPPDATA%\Programs`.
    pub dir_name: &'static str,
    /// Executable file name inside the install folder.
    pub exe_name: &'static str,
    /// HKCU\...\Uninstall\<key>.
    pub reg_key: &'static str,
    /// Tagline on the installer.
    pub tagline: &'static str,
}

/// Aionix Connect: one app to receive and to give remote support.
pub const APP: ProductInfo = ProductInfo {
    display: "Aionix Connect",
    dir_name: "Aionix Connect",
    exe_name: "Aionix Connect.exe",
    reg_key: "AionixConnect",
    tagline: "Receba e dê suporte remoto com segurança. Ninguém acessa um computador sem a permissão de quem está nele.",
};

pub const UNINSTALLER: &str = "Desinstalar.exe";

pub fn product() -> ProductInfo {
    APP
}

pub fn payload() -> &'static [u8] {
    include_bytes!(concat!(env!("OUT_DIR"), "/payload.bin"))
}

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn local_appdata() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

pub fn default_install_dir(p: &ProductInfo) -> PathBuf {
    local_appdata().join("Programs").join(p.dir_name)
}

/// `%LOCALAPPDATA%\Aionix Connect`: the receiving identity (the 9-digit ID), the
/// stored login, settings, logs and downloaded updates.
pub fn data_dir(_p: &ProductInfo) -> PathBuf {
    local_appdata().join(&aionix_brand::brand().identifiers.data_dir_name)
}

/// What is currently installed, if anything.
#[derive(Debug, Clone)]
pub struct Installed {
    pub dir: PathBuf,
    pub version: String,
}

/// Options chosen on the welcome page.
#[derive(Debug, Clone)]
pub struct Options {
    pub dir: PathBuf,
    pub desktop_shortcut: bool,
    /// Start the app (minimized) when the user signs in to Windows, so this computer
    /// is reachable by its ID after a restart. Turning it off in Task Manager
    /// (Inicializar) is respected: that lives in StartupApproved, which is not touched.
    pub start_with_windows: bool,
}

/// Progress callback: (fraction 0..1, step description).
pub type Progress<'a> = &'a mut dyn FnMut(f32, &str);

pub fn install(p: &ProductInfo, opts: &Options, progress: Progress<'_>) -> Result<PathBuf, String> {
    let payload = payload();
    if payload.is_empty() {
        return Err(
            "Este instalador foi gerado sem o aplicativo (build de desenvolvimento).".into(),
        );
    }
    progress(0.08, "Preparando a pasta de instalação");
    std::fs::create_dir_all(&opts.dir)
        .map_err(|e| format!("Não foi possível criar {}: {e}", opts.dir.display()))?;
    cleanup_old(&opts.dir);

    progress(0.25, "Copiando o aplicativo");
    let exe = opts.dir.join(p.exe_name);
    replace_file(&exe, payload)?;

    progress(0.55, "Instalando o desinstalador");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    let uninstaller = opts.dir.join(UNINSTALLER);
    if !same_file(&me, &uninstaller) {
        let bytes = std::fs::read(&me).map_err(|e| e.to_string())?;
        replace_file(&uninstaller, &bytes)?;
    }

    progress(0.7, "Criando atalhos");
    #[cfg(windows)]
    {
        let menu = start_menu_dir();
        let _ = std::fs::create_dir_all(&menu);
        win::shortcut(&menu.join(format!("{}.lnk", p.display)), &exe, p.display)?;
        // Only added when asked; an existing one is left alone (updates keep it).
        if opts.desktop_shortcut
            && let Some(d) = win::desktop_dir()
        {
            win::shortcut(&d.join(format!("{}.lnk", p.display)), &exe, p.display)?;
        }
    }

    progress(0.85, "Registrando no Windows");
    #[cfg(windows)]
    {
        win::register(p, &opts.dir, &exe, &uninstaller, dir_size(&opts.dir))?;
        if opts.start_with_windows {
            win::set_autostart(p, &exe)?;
        } else {
            win::remove_autostart(p);
        }
    }

    progress(1.0, "Concluído");
    Ok(exe)
}

pub fn uninstall(p: &ProductInfo, remove_data: bool, progress: Progress<'_>) -> Result<(), String> {
    let dir = installed(p)
        .map(|i| i.dir)
        .unwrap_or_else(|| default_install_dir(p));
    let exe = dir.join(p.exe_name);
    progress(0.15, "Fechando o aplicativo");
    #[cfg(windows)]
    win::close_running(&exe);

    // The app goes first: if it cannot be removed, nothing else has been touched and the
    // "Apps & features" entry is still there to try again.
    progress(0.3, "Removendo o aplicativo");
    if exe.exists() {
        std::fs::remove_file(&exe).map_err(|_| {
            format!(
                "Feche o {} e tente novamente (o aplicativo ainda está aberto).",
                p.display
            )
        })?;
    }

    progress(0.45, "Removendo atalhos");
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(start_menu_dir().join(format!("{}.lnk", p.display)));
        if let Some(d) = win::desktop_dir() {
            let _ = std::fs::remove_file(d.join(format!("{}.lnk", p.display)));
        }
        let menu = start_menu_dir();
        if std::fs::read_dir(&menu)
            .map(|mut r| r.next().is_none())
            .unwrap_or(false)
        {
            let _ = std::fs::remove_dir(&menu);
        }
    }

    progress(0.6, "Removendo o registro do Windows");
    #[cfg(windows)]
    {
        win::unregister(p);
        win::remove_autostart(p);
    }

    progress(0.75, "Removendo arquivos");
    if remove_data {
        let _ = std::fs::remove_dir_all(data_dir(p));
    }
    cleanup_old(&dir);
    // This uninstaller runs from the install folder: it is deleted after the process
    // exits, then the folder only if nothing else is left in it.
    #[cfg(windows)]
    win::delete_after_exit(&dir.join(UNINSTALLER), &dir);
    progress(1.0, "Concluído");
    Ok(())
}

/// Writes `bytes` to `path`; a running/locked executable is renamed aside first
/// (Windows allows renaming a running .exe), the leftover is cleaned on the next run.
fn replace_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let tmp = path.with_extension("new");
    std::fs::write(&tmp, bytes).map_err(|e| format!("Falha ao gravar {}: {e}", tmp.display()))?;
    let mut aside = None;
    if path.exists() && std::fs::remove_file(path).is_err() {
        let a = path.with_extension(format!("old-{}", std::process::id()));
        std::fs::rename(path, &a).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!(
                "O arquivo {} está em uso e não pôde ser substituído: {e}",
                path.display()
            )
        })?;
        aside = Some(a);
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        // Put the previous version back so the app keeps working.
        if let Some(a) = &aside {
            let _ = std::fs::rename(a, path);
        }
        let _ = std::fs::remove_file(&tmp);
        format!("Falha ao instalar {}: {e}", path.display())
    })
}

fn cleanup_old(dir: &Path) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.contains(".old-") || n.ends_with(".new") {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

pub fn start_menu_dir() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(r"Microsoft\Windows\Start Menu\Programs")
}

pub fn installed(p: &ProductInfo) -> Option<Installed> {
    #[cfg(windows)]
    {
        win::installed(p)
    }
    #[cfg(not(windows))]
    {
        let _ = p;
        None
    }
}

/// Waits (up to `timeout`) for the process that asked for the update to exit.
pub fn wait_for_pid(pid: u32, timeout: Duration) {
    #[cfg(windows)]
    win::wait_for_pid(pid, timeout);
    #[cfg(not(windows))]
    let _ = (pid, timeout);
}

/// Starts the installed app detached from the setup.
pub fn launch(exe: &Path, minimized: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    if minimized {
        cmd.arg("--minimized");
    }
    cmd
        .current_dir(exe.parent().unwrap_or(Path::new(".")))
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Não foi possível abrir o aplicativo: {e}"))
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Com::{
        CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        CoTaskMemFree, IPersistFile,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };
    use windows::Win32::UI::Shell::{
        FOLDERID_Desktop, IShellLinkW, KF_FLAG_DEFAULT, SHGetKnownFolderPath, ShellLink,
    };
    use windows::core::{HSTRING, Interface};
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, KEY_READ, KEY_WRITE};

    const UNINSTALL: &str = r"Software\Microsoft\Windows\CurrentVersion\Uninstall";

    pub fn desktop_dir() -> Option<PathBuf> {
        unsafe {
            let p = SHGetKnownFolderPath(&FOLDERID_Desktop, KF_FLAG_DEFAULT, None).ok()?;
            let s = p.to_string().ok();
            CoTaskMemFree(Some(p.0 as *const _));
            s.map(PathBuf::from)
        }
    }

    pub fn shortcut(lnk: &Path, target: &Path, description: &str) -> Result<(), String> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| format!("atalho: {e}"))?;
            link.SetPath(&HSTRING::from(target.as_os_str()))
                .map_err(|e| e.to_string())?;
            if let Some(dir) = target.parent() {
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                    .map_err(|e| e.to_string())?;
            }
            link.SetIconLocation(&HSTRING::from(target.as_os_str()), 0)
                .map_err(|e| e.to_string())?;
            link.SetDescription(&HSTRING::from(description))
                .map_err(|e| e.to_string())?;
            let file: IPersistFile = link.cast().map_err(|e| e.to_string())?;
            file.Save(&HSTRING::from(lnk.as_os_str()), true)
                .map_err(|e| format!("Não foi possível criar o atalho {}: {e}", lnk.display()))
        }
    }

    pub fn register(
        p: &ProductInfo,
        dir: &Path,
        exe: &Path,
        uninstaller: &Path,
        size_bytes: u64,
    ) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(format!(r"{UNINSTALL}\{}", p.reg_key))
            .map_err(|e| format!("registro: {e}"))?;
        let q = |p: &Path| format!("\"{}\"", p.display());
        let set = |name: &str, v: &str| key.set_value(name, &v.to_string());
        (|| -> std::io::Result<()> {
            set("DisplayName", p.display)?;
            set("DisplayVersion", VERSION)?;
            set("Publisher", &aionix_brand::brand().company_name)?;
            set("DisplayIcon", &format!("{},0", exe.display()))?;
            set("InstallLocation", &dir.display().to_string())?;
            set(
                "UninstallString",
                &format!("{} --uninstall", q(uninstaller)),
            )?;
            set(
                "QuietUninstallString",
                &format!("{} --uninstall --quiet", q(uninstaller)),
            )?;
            key.set_value("EstimatedSize", &((size_bytes / 1024) as u32))?;
            key.set_value("NoModify", &1u32)?;
            key.set_value("NoRepair", &1u32)?;
            Ok(())
        })()
        .map_err(|e| format!("registro: {e}"))
    }

    const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

    /// Starts `exe --minimized` at sign-in (HKCU Run, per user, no administrator).
    pub fn set_autostart(p: &ProductInfo, exe: &Path) -> Result<(), String> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let (key, _) = hkcu
            .create_subkey(RUN)
            .map_err(|e| format!("inicialização: {e}"))?;
        key.set_value(p.display, &format!("\"{}\" --minimized", exe.display()))
            .map_err(|e| format!("inicialização: {e}"))
    }

    pub fn remove_autostart(p: &ProductInfo) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(k) = hkcu.open_subkey_with_flags(RUN, KEY_WRITE) {
            let _ = k.delete_value(p.display);
        }
    }

    pub fn unregister(p: &ProductInfo) {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if let Ok(k) = hkcu.open_subkey_with_flags(UNINSTALL, KEY_WRITE) {
            let _ = k.delete_subkey_all(p.reg_key);
        }
    }

    pub fn installed(p: &ProductInfo) -> Option<Installed> {
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        let k = hkcu
            .open_subkey_with_flags(format!(r"{UNINSTALL}\{}", p.reg_key), KEY_READ)
            .ok()?;
        let dir: String = k.get_value("InstallLocation").ok()?;
        let version: String = k.get_value("DisplayVersion").unwrap_or_default();
        Some(Installed {
            dir: PathBuf::from(dir),
            version,
        })
    }

    pub fn wait_for_pid(pid: u32, timeout: Duration) {
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                let _ = WaitForSingleObject(h, timeout.as_millis() as u32);
                let _ = CloseHandle(h);
            }
        }
    }

    /// Asks a running copy of `exe` to close (window close, like the user would), gives
    /// it a few seconds, then ends it if it is still holding the file.
    pub fn close_running(exe: &Path) {
        let name = exe
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() {
            return;
        }
        let released = || {
            for _ in 0..30 {
                if std::fs::OpenOptions::new().write(true).open(exe).is_ok() || !exe.exists() {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(200));
            }
            false
        };
        let taskkill = |force: bool| {
            let mut cmd = std::process::Command::new("taskkill");
            if force {
                cmd.arg("/F");
            }
            let _ = cmd
                .args(["/IM", &name])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        };
        taskkill(false);
        if !released() {
            taskkill(true);
            released();
        }
    }

    /// Deletes `file` (this running uninstaller) once the process has exited, then
    /// `dir` with a plain `rmdir`, which refuses non-empty folders: nothing the setup did
    /// not create can be removed, whatever `InstallLocation` says.
    pub fn delete_after_exit(file: &Path, dir: &Path) {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let script = format!(
            "ping -n 3 127.0.0.1 >nul & del /f /q \"{}\" & rmdir \"{}\"",
            file.display(),
            dir.display()
        );
        // raw_arg: `args` would escape the inner quotes as \" (MSVCRT rules), which
        // cmd.exe does not understand, so paths with spaces were never deleted.
        let _ = std::process::Command::new("cmd")
            .raw_arg(format!("/d /s /c \"{script}\""))
            .current_dir(std::env::temp_dir())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn();
    }
}
