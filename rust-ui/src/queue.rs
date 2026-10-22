//! Durable local queue history (`<project>/.ck_queue.json`). A snapshot is written atomically whenever a job
//! changes. After a restart anything that was queued or running is shown as HELD ("interrupted"): it is never
//! restarted or assumed finished — the user re-queues from current settings through the normal gate, which
//! also detects a leftover lock or an existing result. Unknown fields and unreadable files are preserved.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const FILE: &str = ".ck_queue.json";
const KEEP: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub folder: String,
    /// "Key" or the task kind label.
    pub label: String,
    /// queued | running | complete | failed | cancelled | lost | held
    pub state: String,
    pub detail: String,
    /// Unix seconds of the last change.
    pub at: u64,
    /// The frozen job as submitted (diagnostics; never re-sent).
    #[serde(default)]
    pub job: Option<Value>,
}

impl Entry {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running")
    }
}

pub fn path(project_root: &Path) -> PathBuf {
    project_root.join(FILE)
}

/// Missing file = empty history. A file that cannot be parsed is moved to `.ck_queue.json.unreadable` (kept
/// for the user) and reported; the queue then starts empty.
pub fn load(project_root: &Path) -> (Vec<Entry>, Option<String>) {
    let p = path(project_root);
    let bytes = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], None),
        Err(e) => return (vec![], Some(format!("{}: {e}", p.display()))),
    };
    let parsed: Result<Value, _> = serde_json::from_slice(&bytes);
    let entries = parsed
        .as_ref()
        .ok()
        .and_then(|v| v.get("entries"))
        .and_then(|e| serde_json::from_value::<Vec<Entry>>(e.clone()).ok());
    match entries {
        Some(mut e) => {
            // Held = was queued/running when the app closed.
            for x in e.iter_mut().filter(|x| x.active()) {
                x.detail = format!(
                    "interrupted while {}: not restarted, not assumed finished",
                    x.state
                );
                x.state = "held".into();
            }
            (e, None)
        }
        None => {
            let kept = p.with_extension("json.unreadable");
            let _ = std::fs::rename(&p, &kept);
            (
                vec![],
                Some(format!(
                    "the queue history was unreadable; kept as {}",
                    kept.display()
                )),
            )
        }
    }
}

pub fn save(project_root: &Path, entries: &[Entry]) -> Result<(), String> {
    let p = path(project_root);
    let start = entries.len().saturating_sub(KEEP);
    let v = serde_json::json!({"version": 1, "entries": &entries[start..]});
    let tmp = p.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?,
    )
    .and_then(|()| std::fs::rename(&tmp, &p))
    .map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", p.display())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, state: &str) -> Entry {
        Entry {
            id: id.into(),
            folder: "c".into(),
            label: "Key".into(),
            state: state.into(),
            detail: String::new(),
            at: 1,
            job: None,
        }
    }

    #[test]
    fn active_entries_come_back_held_and_history_is_bounded() {
        let d = std::env::temp_dir().join(format!("ck-queue-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let mut all: Vec<Entry> = (0..150)
            .map(|i| e(&format!("old{i}"), "complete"))
            .collect();
        all.push(e("run", "running"));
        all.push(e("q", "queued"));
        save(&d, &all).unwrap();
        let (got, err) = load(&d);
        assert!(err.is_none());
        assert_eq!(got.len(), 100);
        let held: Vec<_> = got
            .iter()
            .filter(|x| x.state == "held")
            .map(|x| x.id.as_str())
            .collect();
        assert_eq!(held, ["run", "q"]);
        assert!(
            got.iter().all(|x| !x.active()),
            "nothing is ever restored as running"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn unreadable_history_is_kept_aside() {
        let d = std::env::temp_dir().join(format!("ck-queue-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(path(&d), "{not json").unwrap();
        let (got, err) = load(&d);
        assert!(got.is_empty() && err.is_some());
        assert_eq!(
            std::fs::read(d.join(".ck_queue.json.unreadable")).unwrap(),
            b"{not json"
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
