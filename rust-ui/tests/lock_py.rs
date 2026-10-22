//! The Rust-created import lock, judged by the REAL Python worker code: a live import must read as alive and
//! its release must be refused (both reasons), otherwise a concurrent hint writer could be let in.
use ck::relink::ImportLock;
use std::process::Command;

#[test]
fn live_rust_import_lock_is_alive_to_the_python_worker_and_cannot_be_cleared() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let py = ck::import::python(&repo);
    assert!(py.is_file(), "repo python missing: {}", py.display());
    let root = std::env::temp_dir().join(format!("ck-lockpy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let lock = ImportLock::acquire(&root).unwrap();
    let script = r#"
import json, sys
from backend.service.task_job import inspect_hint_lock, clear_hint_lock
root = sys.argv[1]
info = inspect_hint_lock(root)
exp = {"job_id": info["job_id"], "host": info["host"], "pid": info["pid"]}
out = {"info": {k: info.get(k) for k in ("same_host", "same_pid_namespace", "local_dead", "safe_to_clear", "readable")}}
for reason in ("observed_exit", "user_confirmed_orphan"):
    r = clear_hint_lock(root, exp, reason, "req-" + reason)
    out[reason] = {"cleared": r["cleared"], "code": r.get("code")}
print("RESULT " + json.dumps(out))
"#;
    let o = Command::new(&py)
        .args(["-c", script])
        .arg(&root)
        .current_dir(&repo)
        .env("PYTHONPATH", &repo)
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .env("OPENCV_IO_ENABLE_OPENEXR", "1")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&o.stdout);
    let line = text.lines().find_map(|l| l.strip_prefix("RESULT ")).unwrap_or_else(|| panic!("no result: {text}{}", String::from_utf8_lossy(&o.stderr)));
    let v: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(v["info"]["readable"], true);
    assert_eq!(v["info"]["same_host"], true, "{v}");
    assert_eq!(v["info"]["same_pid_namespace"], true, "{v}");
    assert_eq!(v["info"]["local_dead"], false, "a live import must not look dead or unknown: {v}");
    assert_eq!(v["info"]["safe_to_clear"], false);
    for reason in ["observed_exit", "user_confirmed_orphan"] {
        assert_eq!(v[reason]["cleared"], false, "{reason}: {v}");
        assert_eq!(v[reason]["code"], "alive", "{reason}: {v}");
    }
    assert!(root.join(".corridorkey_hint.lock").exists(), "the live lock is untouched");
    drop(lock);
    let _ = std::fs::remove_dir_all(root);
}
