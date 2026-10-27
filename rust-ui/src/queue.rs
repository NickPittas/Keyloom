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
            self.active() || matches!(self.state.as_str(), "held" | "lost")
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
            let _ = rename_free(p, &kept); // an earlier unreadable file is never overwritten
            (
                vec![],
                Some(format!(
                    "the queue history was unreadable; kept as {} (or the next free .N name)",
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

/// Keep every unfinished entry and the newest `KEEP` finished ones (by `at`), in order.
fn trim(all: Vec<Entry>) -> Vec<Entry> {
    let mut ats: Vec<u64> = all.iter().filter(|e| !e.unfinished()).map(|e| e.at).collect();
    ats.sort_unstable_by(|a, b| b.cmp(a));
    let Some(&cut) = ats.get(KEEP.saturating_sub(1)).filter(|_| ats.len() > KEEP) else { return all };
    // ties at the cut: keep only as many as fit
    let mut at_cut = KEEP - ats.iter().filter(|&&a| a > cut).count();
    all.into_iter()
        .rev()
        .filter(|e| {
            e.unfinished()
                || e.at > cut
                || (e.at == cut && at_cut > 0 && {
                    at_cut -= 1;
                    true
                })
        })
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
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
        #[cfg(windows)]
        crate::remote::protect_private(&tmp, false).map_err(std::io::Error::other)?;
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
            kind: "local".into(),
            owner: "c".into(),
        }
    }

    /// A fresh temp folder per test; never the real settings folder.
    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-queue-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trip_keeps_entries_as_stored() {
        let p = dir("round").join(FILE);
        let mut sent = e("s", "succeeded");
        sent.kind = "sent".into();
        sent.owner = "conn-1".into();
        let all = vec![e("a", "complete"), sent];
        write(&p, all.clone()).unwrap();
        let (got, err) = read(&p);
        assert!(err.is_none());
        assert_eq!(got, all);
    }

    #[test]
    fn active_entries_come_back_held_and_history_is_bounded() {
        let p = dir("held").join(FILE);
        let mut all: Vec<Entry> = (0..250)
            .map(|i| {
                let mut x = e(&format!("old{i}"), "complete");
                x.at = i + 2;
                x
            })
            .collect();
        all.push(e("run", "running"));
        all.push(e("q", "queued"));
        write(&p, all).unwrap();
        let (got, err) = load(&p);
        assert!(err.is_none());
        // the newest 200 finished entries plus the 2 unfinished ones
        assert_eq!(got.len(), 202);
        assert!(got.iter().any(|x| x.id == "old249"));
        assert!(!got.iter().any(|x| x.id == "old0"));
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
    }

    #[test]
    fn unreadable_history_is_kept_aside() {
        let p = dir("bad").join(FILE);
        std::fs::write(&p, "{not json").unwrap();
        let (got, err) = load(&p);
        assert!(got.is_empty() && err.is_some());
        assert_eq!(
            std::fs::read(p.with_extension("json.unreadable")).unwrap(),
            b"{not json"
        );
    }

    #[test]
    fn two_owners_do_not_lose_each_other() {
        let p = dir("owners").join(FILE);
        let sent = |id: &str, owner: &str| {
            let mut x = e(id, "succeeded");
            x.kind = "sent".into();
            x.owner = owner.into();
            x
        };
        replace(&p, |x| x.owner == "a", vec![sent("a1", "a"), sent("a2", "a")]).unwrap();
        replace(&p, |x| x.owner == "b", vec![sent("b1", "b")]).unwrap();
        // owner a rewrites its own entries; b's entry must survive
        replace(&p, |x| x.owner == "a", vec![sent("a2", "a")]).unwrap();
        let ids: Vec<_> = read(&p).0.into_iter().map(|x| x.id).collect();
        assert_eq!(ids, ["a2", "b1"]);
    }
}
