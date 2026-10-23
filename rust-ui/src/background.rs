//! Optional background mode: closing the window hides it (the runtime driver keeps the network client
//! alive), a second launch shows the first window instead of starting a second receiver, and an opt-in
//! per-user login entry starts the app in the background. Per-user on purpose: a SYSTEM service would lose the
//! logged-in user's mapped drives (R:, Y:). Std only: an advisory file lock for ownership and a file the
//! second launch drops for the first to pick up (no network listener).
use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};

const OWNER: &str = "corridorkey-ui"; // show-request payload only
#[cfg(not(windows))]
const ENTRY: &str = "corridorkey.desktop";
#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const RUN_VALUE: &str = "CorridorKey";

pub enum Instance {
    /// This process owns the lock for as long as the value lives.
    Primary(File),
    /// Another instance is running and was asked to show its window.
    Other,
}

/// Take the per-user instance lock in `dir` (the settings folder). Any failure is an error the caller
/// must treat as fatal: running unguarded could start a second network receiver.
pub fn acquire(dir: &Path) -> Result<Instance, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let f = OpenOptions::new().create(true).write(true).truncate(false).open(dir.join("instance.lock")).map_err(|e| e.to_string())?;
    match f.try_lock() {
        Ok(()) => {
            let _ = std::fs::remove_file(dir.join("instance.show")); // stale request from a dead launch
            Ok(Instance::Primary(f))
        }
        Err(TryLockError::WouldBlock) => {
            std::fs::write(dir.join("instance.show"), OWNER).map_err(|e| format!("cannot ask the running copy to show itself: {e}"))?;
            Ok(Instance::Other)
        }
        Err(TryLockError::Error(e)) => Err(e.to_string()),
    }
}

pub fn show(ctx: &eframe::egui::Context) {
    use eframe::egui::ViewportCommand as C;
    ctx.send_viewport_cmd(C::Minimized(false));
    ctx.send_viewport_cmd(C::Visible(true));
    ctx.send_viewport_cmd(C::Focus);
    ctx.request_repaint();
}

/// A hidden Win32 window may receive no redraw events. Wake the native window directly,
/// rather than depending on an egui frame to process the request to show it again.
#[cfg(windows)]
pub fn watch_windows(cc: &eframe::CreationContext<'_>, dir: PathBuf, start_hidden: bool) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetWindowThreadProcessId, SetForegroundWindow, ShowWindowAsync, SW_HIDE, SW_RESTORE, SW_SHOW};
    let RawWindowHandle::Win32(h) = cc.window_handle().map_err(|e| e.to_string())?.as_raw() else {
        return Err("no Win32 window handle".into());
    };
    let handle = h.hwnd.get() as usize;
    let ctx = cc.egui_ctx.clone();
    std::thread::Builder::new().name("ck-instance".into()).spawn(move || {
        let mut hide = start_hidden;
        loop {
            std::thread::sleep(std::time::Duration::from_millis(500));
            let hwnd = handle as windows_sys::Win32::Foundation::HWND;
            let mut owner = 0;
            // The handle must still belong to this process before any native window operation.
            unsafe {
                GetWindowThreadProcessId(hwnd, &mut owner);
                if owner != windows_sys::Win32::System::Threading::GetCurrentProcessId() { break; }
            }
            if std::fs::remove_file(dir.join("instance.show")).is_ok() {
                hide = false;
                unsafe {
                    ShowWindowAsync(hwnd, SW_RESTORE);
                    ShowWindowAsync(hwnd, SW_SHOW);
                    SetForegroundWindow(hwnd);
                }
                ctx.request_repaint();
            } else if hide && ctx.cumulative_frame_nr() > 0 {
                hide = false;
                unsafe { ShowWindowAsync(hwnd, SW_HIDE); }
            }
        }
    }).map(|_| ()).map_err(|e| format!("background window watcher: {e}"))
}

/// The installed stable shim `PREFIX/bin/corridorkey[.cmd]` and its PREFIX: `CK_LAUNCHER` when the installer
/// exports it, else found above this exe. A source/dev run has none, and then no login entry is managed.
fn launcher() -> Result<(PathBuf, PathBuf), String> {
    let name = if cfg!(windows) { "corridorkey.cmd" } else { "corridorkey" };
    let shim = match std::env::var_os("CK_LAUNCHER").filter(|v| !v.is_empty()) {
        Some(p) => Some(PathBuf::from(p)),
        None => std::env::current_exe().ok().and_then(|e| e.ancestors().skip(1).map(|d| d.join("bin").join(name)).find(|c| c.is_file())),
    };
    let prefix = shim.as_ref().and_then(|s| s.parent()?.parent().map(Path::to_path_buf));
    shim.zip(prefix).ok_or_else(|| "this copy was not installed by the CorridorKey installer, so there is no stable launcher to start at sign-in".into())
}

/// Make the per-user login entry match `enabled`. Only the entry this app owns is created, rewritten or removed.
pub fn sync_login(enabled: bool) -> Result<(), String> {
    platform::sync(enabled)
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    fn entry() -> Result<PathBuf, String> {
        let base = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").filter(|v| !v.is_empty()).map(|h| PathBuf::from(h).join(".config")))
            .ok_or("no HOME or XDG_CONFIG_HOME: cannot place a login entry")?;
        Ok(base.join("autostart").join(ENTRY))
    }
    pub fn sync(enabled: bool) -> Result<(), String> {
        let path = entry()?;
        let (shim, prefix) = launcher()?;
        // Installer's owner id: first 16 hex of sha256 of the prefix path.
        let owner = &crate::sha256::hex(prefix.to_string_lossy().as_bytes())[..16];
        let marker = format!("X-CorridorKey-Owner={owner}");
        let ours = std::fs::read_to_string(&path).is_ok_and(|t| t.lines().any(|l| l == marker));
        if !enabled {
            return if ours { std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display())) } else { Ok(()) };
        }
        if path.exists() && !ours {
            return Err(format!("{} exists and is not managed by CorridorKey; not overwriting it", path.display()));
        }
        let exe = shim.to_str().ok_or("launcher path is not valid UTF-8")?;
        // Exec quoting (escape " \ $ `, then % as %%), then the key-file layer doubles every backslash.
        let quoted: String = exe.chars().flat_map(|c| match c {
            '"' | '\\' | '$' | '`' => vec!['\\', c],
            '%' => vec!['%', '%'],
            _ => vec![c],
        }).collect::<String>().replace('\\', "\\\\");
        let text = format!("[Desktop Entry]\nType=Application\nName=CorridorKey\nComment=Keeps the CorridorKey network client available\nExec=\"{quoted}\" --background\nTerminal=false\n{marker}\n");
        if std::fs::read_to_string(&path).is_ok_and(|t| t == text) {
            return Ok(());
        }
        std::fs::create_dir_all(path.parent().expect("has parent")).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    fn reg(args: &[&str]) -> Result<std::process::Output, String> {
        std::process::Command::new("reg").args(args).creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output().map_err(|e| format!("cannot run reg.exe: {e}"))
    }
    pub fn sync(enabled: bool) -> Result<(), String> {
        let (_, prefix) = launcher()?;
        let q = reg(&["query", RUN_KEY, "/v", RUN_VALUE])?;
        let old = String::from_utf8_lossy(&q.stdout).to_lowercase();
        let owned = ["bin", "versions"].iter().any(|d| old.contains(
            &format!("\"{}\\", prefix.join(d).display()).to_lowercase()
        ));
        if !enabled {
            // Remove only a value that points into this installation.
            if !q.status.success() || !owned {
                return Ok(());
            }
            let o = reg(&["delete", RUN_KEY, "/v", RUN_VALUE, "/f"])?;
            return if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()) };
        }
        if q.status.success() && !owned {
            return Err("The CorridorKey sign-in entry belongs to another installation; it was left unchanged".into());
        }
        // A GUI exe avoids a console at login. The installer rewrites this on activation/rollback.
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let data = format!("\"{}\" --background", exe.display());
        let o = reg(&["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d", &data, "/f"])?;
        if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_owned()) }
    }
}
