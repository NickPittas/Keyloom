//! Window-system choice. winit 0.30 delivers file drops only on X11, so under Wayland (Omarchy/Hyprland) we
//! run through XWayland when its socket is really there, and stay on native Wayland otherwise.
use std::path::{Path, PathBuf};

/// Unix socket of an X display string such as `:0` or `:1.0`; remote/TCP displays have none.
pub fn x11_socket(display: &str) -> Option<PathBuf> {
    let n = display.strip_prefix(':')?.split('.').next()?;
    n.parse::<u32>()
        .ok()
        .map(|n| PathBuf::from(format!("/tmp/.X11-unix/X{n}")))
}

/// True when a Wayland session also offers a live X server (then Wayland is hidden from winit).
pub fn use_xwayland(
    wayland: bool,
    display: Option<&str>,
    keep_wayland: bool,
    socket_exists: impl Fn(&Path) -> bool,
) -> bool {
    wayland
        && !keep_wayland
        && display
            .and_then(x11_socket)
            .is_some_and(|s| socket_exists(&s))
}

/// Call before the event loop is created (single-threaded at that point).
pub fn prefer_xwayland() {
    let wayland = ["WAYLAND_DISPLAY", "WAYLAND_SOCKET"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()));
    let display = std::env::var("DISPLAY").ok();
    // CK_WAYLAND=1 keeps native Wayland (no drag and drop there).
    let keep = std::env::var_os("CK_WAYLAND").is_some_and(|v| v == "1");
    if use_xwayland(wayland, display.as_deref(), keep, Path::exists) {
        std::env::remove_var("WAYLAND_DISPLAY");
        std::env::remove_var("WAYLAND_SOCKET");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn xwayland_only_when_wayland_and_a_live_unix_socket() {
        let live = |p: &Path| p == Path::new("/tmp/.X11-unix/X1");
        assert!(use_xwayland(true, Some(":1"), false, live));
        assert!(use_xwayland(true, Some(":1.0"), false, live));
        assert!(
            !use_xwayland(true, Some(":0"), false, live),
            "stale DISPLAY, no socket"
        );
        assert!(!use_xwayland(true, None, false, live));
        assert!(
            !use_xwayland(true, Some("host:10"), false, |_| true),
            "TCP display"
        );
        assert!(
            !use_xwayland(false, Some(":1"), false, live),
            "pure X11 stays as is"
        );
        assert!(
            !use_xwayland(true, Some(":1"), true, live),
            "CK_WAYLAND=1 override"
        );
    }
}
