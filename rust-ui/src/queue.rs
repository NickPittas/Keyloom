//! The one durable job history of this PC (`history.json` in the settings folder). It holds local renders and
//! AI tasks (kind "local", owner = project folder) and jobs sent to clients (kind "sent", owner = connection key).
//! After a restart a local entry that was queued or running is shown as HELD ("interrupted"): it is never
//! restarted or assumed finished — the user re-queues from current settings through the normal gate, which
//! also detects a leftover lock or an existing result. Unreadable files are kept aside.
//! Every writer re-reads the file under one lock and replaces only its own entries, so writers never lose each other's.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const FILE: &str = "history.json";
/// The per-project file this history replaces; read once for import, then renamed.
pub const OLD_FILE: &str = ".ck_queue.json";
/// Finished entries kept; unfinished ones are always kept.
const KEEP: usize = 200;

static LOCK: Mutex<()> = Mutex::new(());
/// The settings file the app actually uses (set once at startup); history.json lives beside it.
static SETTINGS: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();

/// Called once when the app starts, with the settings path it loaded (None = session-only settings).
pub fn set_settings_path(p: Option<PathBuf>) {
    let _ = SETTINGS.set(p);
}

fn local() -> String {
    "local".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub folder: String,
    /// "Key" or the task kind label.
    pub label: String,
    /// queued | running | complete | failed | cancelled | lost | held (sent jobs: their own states)
    pub state: String,
    pub detail: String,
    /// Unix seconds of the last change.
    pub at: u64,
    /// The frozen job as submitted (diagnostics; never re-sent). For a sent job: its saved record.
    #[serde(default)]
    pub job: Option<Value>,
    /// "local" or "sent".
    #[serde(default = "local")]
    pub kind: String,
    /// Local: the project folder. Sent: the connection key.
    #[serde(default)]
    pub owner: String,
}

impl Entry {
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running")
    }

    fn unfinished(&self) -> bool {
        if self.kind == "sent" {
            !matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled" | "not sent")
        } else {
            self.active()
        }
    }
}

/// `<settings folder>/history.json`; `None` when the app runs with session-only settings.
// ponytail: the first App in a process sets the settings path; only tests start a second one, so no fallback to the default path.
pub fn history_path() -> Option<PathBuf> {
    SETTINGS.get().cloned().flatten().and_then(|p| p.parent().map(|d| d.join(FILE)))
}

/// Move `from` to `to`, or to the first free `to.2`, `to.3`, ... when `to` exists, so an earlier import is never overwritten.
// ponytail: the existence check and the rename are not atomic; one app process imports at a time.
pub fn rename_free(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut dest = to.to_path_buf();
    let mut n = 2;
    while dest.exists() {
        dest = PathBuf::from(format!("{}.{n}", to.display()));
        n += 1;
    }
    std::fs::rename(from, dest)
}

/// Missing file = empty history. A file that cannot be parsed is moved to `<name>.unreadable` (kept for the
/// user) and reported; the history then starts empty. Entries are returned as stored.
pub fn read(p: &Path) -> (Vec<Entry>, Option<String>) {
    let bytes = match std::fs::read(p) {
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
        Some(e) => (e, None),
        None => {
            let kept = p.with_extension("json.unreadable");
            let _ = std::fs::rename(p, &kept);
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

/// `read`, with local entries that were queued/running when the app closed turned into HELD.
pub fn load(p: &Path) -> (Vec<Entry>, Option<String>) {
    let (mut e, err) = read(p);
    for x in e.iter_mut().filter(|x| x.kind == "local" && x.active()) {
        x.detail = format!(
            "interrupted while {}: not restarted, not assumed finished",
            x.state
        );
        x.state = "held".into();
    }
    (e, err)
}

/// Keep every unfinished entry and the newest `KEEP` finished ones, in order.
fn trim(all: Vec<Entry>) -> Vec<Entry> {
    let mut finished = 0;
    let mut keep: Vec<Entry> = all
        .into_iter()
        .rev()
        .filter(|e| {
            e.unfinished() || {
                finished += 1;
                finished <= KEEP
            }
        })
        .collect();
    keep.reverse();
    keep
}

fn write(p: &Path, entries: Vec<Entry>) -> Result<(), String> {
    use std::io::Write;
    let v = serde_json::json!({"version": 1, "entries": trim(entries)});
    let tmp = p.with_extension(format!("json.tmp{}", std::process::id()));
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600); // sent jobs carry their submission
    }
    let r = (|| -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        let mut f = o.open(&tmp)?;
        f.write_all(&serde_json::to_vec_pretty(&v).map_err(std::io::Error::other)?)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, p)
    })();
    r.map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", p.display())
    })
}

/// Re-read the file, replace the entries `mine` selects with `add` (entries already there keep their place, new
/// ones go last), leave everyone else's entries untouched, and save atomically.
pub fn replace(p: &Path, mine: impl Fn(&Entry) -> bool, mut add: Vec<Entry>) -> Result<(), String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (cur, err) = read(p);
    if let Some(e) = err.filter(|_| p.exists()) {
        return Err(e); // unreadable and still in place: writing would replace it with only our entries
    }
    let mut out = Vec::with_capacity(cur.len() + add.len());
    for e in cur {
        if !mine(&e) {
            out.push(e);
        } else if let Some(i) = add.iter().position(|a| a.id == e.id) {
            out.push(add.remove(i));
        }
    }
    out.extend(add);
    write(p, out)
}

/// Add `old` entries whose id is not in the file yet (before the existing ones: they are older).
pub fn merge_old(p: &Path, old: Vec<Entry>) -> Result<(), String> {
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (cur, err) = read(p);
    if let Some(e) = err.filter(|_| p.exists()) {
        return Err(e); // unreadable and still in place: writing would replace it with only our entries
    }
    let mut out: Vec<Entry> = old
        .into_iter()
        .filter(|o| !cur.iter().any(|c| c.id == o.id))
        .collect();
    out.extend(cur);
    write(p, out)
}

/// Sent entries whose connection is no longer one of `live_owners` (its PC was removed), kept until the user clears them.
pub fn orphaned(live_owners: &[String]) -> Vec<Entry> {
    let Some(p) = history_path() else { return vec![] };
    read(&p).0.into_iter().filter(|e| e.kind == "sent" && !live_owners.contains(&e.owner)).collect()
}

/// Remove exactly the entries with these ids, under the same lock and atomic write as `replace`.
pub fn forget(ids: &[String]) -> Result<(), String> {
    let Some(p) = history_path() else { return Ok(()) };
    let _g = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (cur, err) = read(&p);
    if let Some(e) = err.filter(|_| p.exists()) {
        return Err(e); // unreadable and still in place: writing would replace it with only the kept entries
    }
    write(&p, cur.into_iter().filter(|e| !ids.contains(&e.id)).collect())
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
