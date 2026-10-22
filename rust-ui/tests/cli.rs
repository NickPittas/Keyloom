//! Headless flags used by installers: no window, deterministic output.
use std::process::Command;

#[test]
fn version_and_self_check_never_open_a_window() {
    let exe = env!("CARGO_BIN_EXE_corridorkey-ui");
    let v = Command::new(exe).arg("--version").output().unwrap();
    assert!(v.status.success());
    assert!(String::from_utf8_lossy(&v.stdout).starts_with("corridorkey-ui "));
    // Bogus descriptor: the self-check must fail loudly (exit 1 + JSON problem), not fall back to a GUI.
    let bad = Command::new(exe)
        .arg("--self-check")
        .env("CK_INSTALL", "/nonexistent/corridorkey-install.json")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    let line = String::from_utf8_lossy(&bad.stdout);
    let j: serde_json::Value = serde_json::from_str(line.trim()).expect("one JSON line");
    assert_eq!(j["ok"], false);
    assert!(!bad.status.success());
}
