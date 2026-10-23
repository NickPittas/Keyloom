//! Frozen Key job (`ai-implementation-brief.md` §3.3): serde mirror of the worker protocol's `KeyJob`,
//! validation, output-path resolution (via `contracts`), cross-job conflict test and local→remote path
//! mapping. Pure data: paths are checked as strings so a POSIX job validates the same on Windows.
use crate::contracts::{
    resolve_path, ClipOutputValues, LocationError, OutputKind, OutputLocation, RangeIdentity,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

pub const PROTOCOL: u32 = 1;
/// Wire keys of `KeyJob::outputs`.
pub const OUTPUT_KEYS: [&str; 4] = ["fg", "matte", "comp", "processed"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobMode {
    New,
    Resume,
    /// Replace the files of an existing app-owned result of the same shape (backend `key_modes` has `replace`).
    Replace,
}

/// What the worker must be. `None` digest = the caller has no reference digest (identity unverifiable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Require {
    pub protocol: u32,
    pub models: BTreeMap<String, Option<String>>,
}

/// The job as submitted. Built once by the UI (ids come from the caller), never mutated afterwards: the
/// scheduler only hands out `Arc<KeyJob>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KeyJob {
    pub job_id: String,
    pub clip_id: String,
    pub clip_root: PathBuf,
    pub range: RangeIdentity,
    /// `InferenceParams.to_dict()`, opaque here.
    pub params: Value,
    /// `OutputConfig.to_dict()`, opaque here.
    pub output_config: Value,
    pub hint_keyframes_abs: Option<Vec<usize>>,
    /// Only enabled outputs, one resolved path per range frame, in range order.
    pub outputs: BTreeMap<String, Vec<PathBuf>>,
    pub manifest_dir: PathBuf,
    pub mode: JobMode,
    pub publish: String,
    #[serde(default)]
    pub path_map: Vec<(String, String)>,
    pub require: Require,
    /// Network receiver only: `[(path clip.json records, folder this PC resolved through its own mapping)]`, at most
    /// one. Lets the worker read an external source without ever using the sender's path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_map: Vec<(String, String)>,
    /// Network receiver only: a pinned temporal window that does not fit the free GPU memory fails instead of warning.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enforce_fixed_window: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobError {
    pub field: &'static str,
    pub message: String,
}
impl std::fmt::Display for JobError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}
fn err<T>(field: &'static str, message: impl Into<String>) -> Result<T, JobError> {
    Err(JobError {
        field,
        message: message.into(),
    })
}

/// Absolute in either POSIX or Windows (drive / UNC) syntax.
pub fn is_abs(s: &str) -> bool {
    root_len(s).is_some()
}
/// Length of the root prefix: "/", "\" or "X:/" "X:\".
fn root_len(s: &str) -> Option<usize> {
    let b = s.as_bytes();
    if s.starts_with("\\\\") {
        Some(2) // UNC \\host\share
    } else if s.starts_with('/') || s.starts_with('\\') {
        Some(1)
    } else if b.len() >= 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && matches!(b[2], b'/' | b'\\')
    {
        Some(3)
    } else {
        None
    }
}
fn comps(s: &str) -> Vec<&str> {
    s.split(['/', '\\']).filter(|c| !c.is_empty()).collect()
}
fn lower(c: &[&str]) -> Vec<String> {
    c.iter().map(|s| s.to_lowercase()).collect()
}
/// Python `_clean_abs`: absolute and already normalised (no empty, `.` or `..` segments, no trailing
/// separator, no mixed separators, no NUL/control characters).
fn check_path<'a>(field: &'static str, p: &'a Path) -> Result<&'a str, JobError> {
    let Some(s) = p.to_str() else {
        return err(field, "path is not valid UTF-8");
    };
    let Some(root) = root_len(s) else {
        return err(field, format!("path is not absolute: {s}"));
    };
    let rest = &s[root..];
    let bad = s.chars().any(char::is_control)
        || (s.contains('/') && s.contains('\\'))
        || (!rest.is_empty()
            && rest
                .split(['/', '\\'])
                .any(|c| c.is_empty() || c == "." || c == ".."));
    if bad {
        return err(field, format!("path is not clean/normalised: {s}"));
    }
    Ok(s)
}

pub const MANIFEST_NAME: &str = ".corridorkey_manifest.json";
pub const LOCK_NAME: &str = ".corridorkey_job.lock";
const SETTINGS_NAME: &str = "corridorkey_v2_settings.txt";

/// Enabled outputs and their file extension from `OutputConfig.to_dict()`, with the worker's rules: all nine
/// keys, bool flags, `exr`/`png` formats, known EXR compression, at least one output.
fn enabled_outputs(cfg: &Value) -> Result<Vec<(&'static str, String)>, JobError> {
    let o = cfg.as_object().filter(|o| o.len() == 9);
    let Some(o) = o else {
        return err(
            "output_config",
            "must be an object with exactly the 9 OutputConfig fields",
        );
    };
    if !o
        .get("exr_compression")
        .and_then(Value::as_str)
        .is_some_and(|c| ["dwab", "piz", "zip", "none"].contains(&c))
    {
        return err(
            "output_config",
            "exr_compression must be dwab, piz, zip or none",
        );
    }
    let mut on = vec![];
    for k in OUTPUT_KEYS {
        let fmt = o
            .get(&format!("{k}_format"))
            .and_then(Value::as_str)
            .filter(|f| *f == "exr" || *f == "png");
        let (Some(en), Some(fmt)) = (o.get(&format!("{k}_enabled")).and_then(Value::as_bool), fmt)
        else {
            return err(
                "output_config",
                format!("{k}_enabled must be a bool and {k}_format exr or png"),
            );
        };
        if en {
            on.push((k, fmt.to_owned()));
        }
    }
    if on.is_empty() {
        return err("output_config", "no output enabled");
    }
    Ok(on)
}

impl KeyJob {
    /// Mirrors the worker's structural checks (`KeyJob.from_dict`) so a bad job fails before a process is
    /// involved. Full `InferenceParams` canonicalisation stays with the worker (it answers `rejected: invalid`).
    pub fn validate(&self) -> Result<(), JobError> {
        let id_ok = !self.job_id.is_empty()
            && self.job_id.len() <= 64
            && self
                .job_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !id_ok {
            return err("job_id", "must match [A-Za-z0-9._-]{1,64}");
        }
        if self.clip_id.is_empty() || self.clip_id.chars().count() > 512 {
            return err("clip_id", "must be 1-512 characters");
        }
        let r = &self.range;
        if r.in_point > r.out_point || r.in_stem.is_empty() || r.out_stem.is_empty() {
            return err("range", "needs in <= out and non-empty stems");
        }
        let n = r.frame_count();
        let on = enabled_outputs(&self.output_config)?;
        let hint = match self
            .params
            .as_object()
            .and_then(|p| p.get("hint_mode"))
            .and_then(Value::as_str)
        {
            Some(h @ ("none" | "full" | "keyframes")) => h,
            _ => {
                return err(
                    "params",
                    "params must be an object with hint_mode none, full or keyframes",
                )
            }
        };
        match (&self.hint_keyframes_abs, hint) {
            (Some(k), "keyframes") => {
                if k.is_empty()
                    || k.windows(2).any(|w| w[0] >= w[1])
                    || k.iter().any(|m| *m < r.in_point || *m > r.out_point)
                {
                    return err(
                        "hint_keyframes_abs",
                        "keyframes mode needs sorted unique marks inside In/Out",
                    );
                }
            }
            (None, "keyframes") => return err("hint_keyframes_abs", "keyframes mode needs marks"),
            (Some(_), _) => {
                return err(
                    "hint_keyframes_abs",
                    "marks are only valid in keyframes mode",
                )
            }
            (None, _) => {}
        }
        if self.publish != "final" {
            return err("publish", "only \"final\" is supported");
        }
        if !self.path_map.is_empty() {
            // Mapping is the scheduler's business per target; the wire job always carries [].
            return err("path_map", "must be empty (targets own their path maps)");
        }
        if self.source_map.len() > 1 || self.source_map.iter().any(|(r, l)| r.is_empty() || !is_abs(l)) {
            return err("source_map", "at most one [recorded path, absolute local folder] entry");
        }
        if self.require.protocol != PROTOCOL || self.require.models.is_empty() {
            return err("require", "needs protocol 1 and at least one model");
        }
        let root = check_path("clip_root", &self.clip_root)?;
        let mdir = comps(check_path("manifest_dir", &self.manifest_dir)?);
        if comps(root).len() >= mdir.len() && lower(&comps(root)[..mdir.len()]) == lower(&mdir) {
            return err("manifest_dir", "would contain the source clip");
        }
        let enabled: Vec<&str> = on.iter().map(|(k, _)| *k).collect();
        if self
            .outputs
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .len()
            != enabled.len()
            || !enabled.iter().all(|k| self.outputs.contains_key(*k))
        {
            return err(
                "outputs",
                format!("must be exactly the enabled outputs {enabled:?}"),
            );
        }
        let mut seen = HashSet::new();
        for (key, ext) in &on {
            let paths = &self.outputs[*key];
            if paths.len() != n {
                return err(
                    "outputs",
                    format!("{key}: {} paths for {n} frames", paths.len()),
                );
            }
            for p in paths {
                let s = check_path("outputs", p)?;
                let c = comps(s);
                let name = p.file_name().and_then(|f| f.to_str()).unwrap_or("");
                if c.len() <= mdir.len() || c[..mdir.len()] != mdir[..] {
                    return err("outputs", format!("outside manifest_dir: {s}"));
                }
                if p.extension().and_then(|e| e.to_str()) != Some(ext.as_str()) {
                    return err("outputs", format!("{key} must be .{ext}: {s}"));
                }
                if name.starts_with('.') {
                    return err("outputs", format!("hidden file name: {s}"));
                }
                if !seen.insert(s.to_lowercase()) {
                    return err("outputs", format!("two frames write the same file: {s}"));
                }
            }
        }
        Ok(())
    }

    /// One manifest folder has one writer: nested or equal manifest folders conflict. (Every output sits
    /// under its manifest folder, so this also covers identical files.) Case-insensitive on purpose.
    pub fn conflicts_with(&self, other: &KeyJob) -> bool {
        let (a, b) = (
            lower(&comps(self.manifest_dir.to_str().unwrap_or(""))),
            lower(&comps(other.manifest_dir.to_str().unwrap_or(""))),
        );
        let n = a.len().min(b.len());
        a[..n] == b[..n]
    }

    /// Copy with every absolute path rewritten through `map` (longest prefix wins, whole components only).
    /// Err = the first path no entry covers. The copy is re-validated so a mapping cannot break containment.
    pub fn mapped(&self, map: &[(String, String)]) -> Result<KeyJob, String> {
        let m = |p: &Path| -> Result<PathBuf, String> {
            let s = p.to_str().unwrap_or("");
            let c = comps(s);
            let best = map
                .iter()
                .filter(|(l, _)| {
                    let lc = comps(l);
                    c.len() >= lc.len() && c[..lc.len()] == lc[..]
                })
                .max_by_key(|(l, _)| comps(l).len())
                .ok_or_else(|| format!("no path_map entry covers {s}"))?;
            let rest = &c[comps(&best.0).len()..];
            let sep = if best.1.contains('\\') && !best.1.contains('/') {
                "\\"
            } else {
                "/"
            };
            let mut out = best.1.trim_end_matches(['/', '\\']).to_owned();
            if out.is_empty() && !is_abs(&best.1) {
                return Err(format!("bad path_map target {}", best.1));
            }
            for r in rest {
                out.push_str(sep);
                out.push_str(r);
            }
            Ok(out.into())
        };
        let mut j = self.clone();
        j.clip_root = m(&self.clip_root)?;
        j.manifest_dir = m(&self.manifest_dir)?;
        for (k, v) in &self.outputs {
            j.outputs
                .insert(k.clone(), v.iter().map(|p| m(p)).collect::<Result<_, _>>()?);
        }
        j.path_map = vec![]; // already applied; the worker rejects a non-empty map
        j.validate().map_err(|e| format!("after path_map: {e}"))?;
        Ok(j)
    }

    /// Local filesystem check (symlinks): every output, resolved through existing ancestors, stays under the
    /// resolved manifest folder, and that folder never contains the source clip. Only for local targets.
    pub fn check_disk(&self) -> Result<(), JobError> {
        note_disk_check(&self.manifest_dir);
        let root = canon_existing(&self.manifest_dir).map_err(|e| JobError {
            field: "manifest_dir",
            message: e,
        })?;
        let clip = canon_existing(&self.clip_root).map_err(|e| JobError {
            field: "clip_root",
            message: e,
        })?;
        if clip.starts_with(&root) {
            return err(
                "manifest_dir",
                "resolves to a folder containing the source clip",
            );
        }
        for p in self.outputs.values().flatten() {
            let c = canon_existing(p).map_err(|e| JobError {
                field: "outputs",
                message: e,
            })?;
            if !c.starts_with(&root) {
                return err(
                    "outputs",
                    format!("{} resolves outside the manifest folder", p.display()),
                );
            }
        }
        Ok(())
    }
}

/// What `KeyJob::recover_writer_gone` did. Never touches anything that is not provably this job's.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Recovery {
    pub lock_removed: bool,
    /// Lock held by a different job (or unreadable): left in place.
    pub lock_left: Option<String>,
    pub temps_removed: Vec<PathBuf>,
    /// `recover_orphan` without `remove_temps`: this job's leftovers, reported but not touched.
    pub temps_left: Vec<PathBuf>,
    pub errors: Vec<String>,
}

/// A `.corridorkey_job.lock` found on disk (e.g. after an app restart, when no job object exists any more).
/// All fields are what the lock file says; nothing is inferred and no age ever authorises anything.
#[derive(Debug, Clone, PartialEq)]
pub struct LockInfo {
    pub path: PathBuf,
    pub job_id: Option<String>,
    pub host: Option<String>,
    pub platform: Option<String>,
    pub pid: Option<u64>,
    pub started: Option<f64>,
    /// Age of the lock file's mtime (the worker touches it as its heartbeat). Informational only.
    pub heartbeat_age: Option<std::time::Duration>,
}

/// Read the lock in `manifest_dir`. `Ok(None)` = no lock. A symlink or non-regular file is an error: it is
/// never followed or trusted. An unreadable/garbled lock yields `Ok(Some)` with `job_id: None`.
pub fn read_lock(manifest_dir: &Path) -> Result<Option<LockInfo>, String> {
    let path = manifest_dir.join(LOCK_NAME);
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if !meta.file_type().is_file() {
        return Err(format!(
            "{} is not a regular file; not touching it",
            path.display()
        ));
    }
    let v: Value = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or(Value::Null);
    let text = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    Ok(Some(LockInfo {
        job_id: text("job_id"),
        host: text("host"),
        platform: text("platform"),
        pid: v.get("pid").and_then(Value::as_u64),
        started: v.get("started").and_then(Value::as_f64),
        heartbeat_age: meta.modified().ok().and_then(|m| m.elapsed().ok()),
        path,
    }))
}

/// Files directly in `manifest_dir` that carry this exact job id in the worker's temp naming: output tmp
/// `.ck-<id>-<12 hex>.exr|png`, probes `.ck-probe-<id>-a|b`, `<manifest>.tmp.<id>`, `<settings>.tmp.<id>`.
/// Exact structure, never a prefix match (`abc` does not match `abc-2`). Output sub-folders are not searched:
/// without the original paths their tags cannot be verified.
pub fn leftover_temps(manifest_dir: &Path, job_id: &str) -> Vec<PathBuf> {
    let hex12 = |t: &str| {
        t.len() == 12
            && t.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    };
    let mut out: Vec<PathBuf> = std::fs::read_dir(manifest_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file())) // symlinks are not regular files here
        .filter(|e| {
            let n = e.file_name();
            let n = n.to_string_lossy();
            n == format!("{MANIFEST_NAME}.tmp.{job_id}")
                || n == format!("{SETTINGS_NAME}.tmp.{job_id}")
                || n == format!(".ck-probe-{job_id}-a")
                || n == format!(".ck-probe-{job_id}-b")
                || n.strip_prefix(&format!(".ck-{job_id}-"))
                    .and_then(|r| r.strip_suffix(".exr").or_else(|| r.strip_suffix(".png")))
                    .is_some_and(hex12)
        })
        .map(|e| e.path())
        .collect();
    out.sort();
    out
}

/// Restart recovery for a folder whose lock belongs to a job this app no longer knows. The CALLER must have
/// obtained the user's explicit statement that the writer is gone; age is never a reason. Refuses unless the
/// lock names exactly `job_id`. Removes the lock; this job's top-level temp files are removed only with
/// `remove_temps`, otherwise listed in `temps_left`.
pub fn recover_orphan(manifest_dir: &Path, job_id: &str, remove_temps: bool) -> Recovery {
    let mut r = Recovery::default();
    match read_lock(manifest_dir) {
        Ok(None) => {}
        Err(e) => {
            r.errors.push(e);
            return r;
        }
        Ok(Some(l)) if l.job_id.as_deref() == Some(job_id) => match std::fs::remove_file(&l.path) {
            Ok(()) => r.lock_removed = true,
            Err(e) => r.errors.push(format!("lock: {e}")),
        },
        Ok(Some(l)) => {
            r.lock_left = Some(format!("held by job {:?}, not {job_id}", l.job_id));
            return r;
        }
    }
    for t in leftover_temps(manifest_dir, job_id) {
        if remove_temps {
            match std::fs::remove_file(&t) {
                Ok(()) => r.temps_removed.push(t),
                Err(e) => r.errors.push(format!("{}: {e}", t.display())),
            }
        } else {
            r.temps_left.push(t);
        }
    }
    r
}

impl KeyJob {
    /// The local manifest path, the only one the app reads. A path a worker reports in `done` is never used.
    pub fn manifest_path(&self) -> PathBuf {
        self.manifest_dir.join(MANIFEST_NAME)
    }

    /// Every temp/aux path this job's worker may have created, exact names only (see `recover_as_sent`):
    /// output tmp `.ck-<id>-<sha1(final)[:12]><ext>` next to each final file, link probes `.ck-probe-<id>-a|b`
    /// in every distinct output folder and the manifest folder, `<manifest>.tmp.<id>` and `<settings>.tmp.<id>`.
    /// The tag hashes the final path as the worker normalised it (`worker_path_form` of the path in `sent`);
    /// the location is this job's local path. Matches `tests/fixtures/worker_protocol/temp_names.json`.
    pub fn owned_temps(&self, sent: &KeyJob) -> Vec<PathBuf> {
        let id = &self.job_id;
        let mut temps: Vec<PathBuf> = vec![
            self.manifest_dir.join(format!("{MANIFEST_NAME}.tmp.{id}")),
            self.manifest_dir.join(format!("{SETTINGS_NAME}.tmp.{id}")),
        ];
        let mut dirs: Vec<&Path> = vec![&self.manifest_dir];
        for (key, paths) in &self.outputs {
            for (i, p) in paths.iter().enumerate() {
                let Some(dir) = p.parent() else { continue };
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
                let wire = sent
                    .outputs
                    .get(key)
                    .and_then(|v| v.get(i))
                    .and_then(|w| w.to_str());
                let ext = p
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{e}"))
                    .unwrap_or_default();
                if let Some(w) = wire {
                    temps.push(dir.join(format!(
                        ".ck-{id}-{}{ext}",
                        &sha1_hex(worker_path_form(w).as_bytes())[..12]
                    )));
                }
            }
        }
        for d in dirs {
            temps.push(d.join(format!(".ck-probe-{id}-a")));
            temps.push(d.join(format!(".ck-probe-{id}-b")));
        }
        temps
    }

    /// Clean up after a writer that is PROVEN gone (local child reaped, or the user confirmed a remote one).
    /// Removes `.corridorkey_job.lock` only if it names this exact job id, plus this job's own temp files
    /// (`.ck-<job_id>-<tag><ext>`, `<manifest>.tmp.<job_id>`, `<settings>.tmp.<job_id>`), full id, exact
    /// names, regular files only. Anything else (another job's lease or temps, symlinks) stays. Re-checks
    /// containment first, so a path swapped for a symlink since dispatch makes this a no-op with an error.
    pub fn recover_writer_gone(&self) -> Recovery {
        self.recover_as_sent(self)
    }

    /// Like `recover_writer_gone` for a job that was sent to a worker with remapped paths (`sent`, same shape
    /// as `self`): temp-file tags come from the paths the worker used, cleanup happens at the local paths.
    pub fn recover_as_sent(&self, sent: &KeyJob) -> Recovery {
        let mut r = Recovery::default();
        if let Err(e) = self.check_disk() {
            r.errors.push(format!("not touching the folder: {e}"));
            return r;
        }
        let regular = |p: &Path| std::fs::symlink_metadata(p).map(|m| m.file_type().is_file());
        let lock = self.manifest_dir.join(LOCK_NAME);
        match regular(&lock) {
            Ok(true) => match std::fs::read(&lock)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            {
                Some(v)
                    if v.get("job_id").and_then(Value::as_str) == Some(self.job_id.as_str()) =>
                {
                    match std::fs::remove_file(&lock) {
                        Ok(()) => r.lock_removed = true,
                        Err(e) => r.errors.push(format!("lock: {e}")),
                    }
                }
                Some(v) => {
                    r.lock_left = Some(format!(
                        "held by job {}",
                        v.get("job_id").unwrap_or(&Value::Null)
                    ))
                }
                None => r.lock_left = Some("unreadable lock".into()),
            },
            Ok(false) => r.lock_left = Some("lock is not a regular file".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => r.errors.push(format!("lock: {e}")),
        }
        let temps = self.owned_temps(sent);
        for t in temps {
            if regular(&t).unwrap_or(false) {
                match std::fs::remove_file(&t) {
                    Ok(()) => r.temps_removed.push(t),
                    Err(e) => r.errors.push(format!("{}: {e}", t.display())),
                }
            }
        }
        r
    }
}

static DISK_CHECKS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
fn note_disk_check(dir: &Path) {
    let mut v = DISK_CHECKS.lock().unwrap_or_else(|e| e.into_inner());
    if v.len() < 4096 {
        v.push(dir.to_path_buf());
    }
}
/// How many times `check_disk` ran for this manifest folder. Instrumentation for tests that prove the UI
/// polling path (`readiness`, `tick`) does no filesystem work.
pub fn disk_checks_for(dir: &Path) -> usize {
    DISK_CHECKS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|d| *d == dir)
        .count()
}

/// Canonicalise the deepest existing ancestor (dangling symlinks are errors) and re-append the missing tail.
fn canon_existing(p: &Path) -> Result<PathBuf, String> {
    let (mut cur, mut tail) = (p, vec![]);
    loop {
        match std::fs::symlink_metadata(cur) {
            Ok(_) => {
                let base = cur
                    .canonicalize()
                    .map_err(|e| format!("{}: {e}", cur.display()))?;
                return Ok(tail.iter().rev().fold(base, |a, t| a.join(t)));
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
                    return Err(format!("{}: no existing ancestor", p.display()));
                };
                tail.push(name.to_owned());
                cur = parent;
            }
            Err(e) => return Err(format!("{}: {e}", cur.display())),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ResolveError {
    Location(LocationError),
    /// The stem has no trailing number for the `####` run.
    NoFrameNumber(String),
}

/// Trailing decimal run of a source stem ("clip_name_1101" → 1101).
pub fn stem_frame_number(stem: &str) -> Option<u64> {
    let digits = stem.len() - stem.trim_end_matches(|c: char| c.is_ascii_digit()).len();
    stem[stem.len() - digits..].parse().ok()
}

fn key(kind: OutputKind) -> String {
    kind.token().to_ascii_lowercase()
}

/// Templated layout: one resolved path per range stem and enabled output, using the source frame numbers.
pub fn resolve_outputs(
    loc: &OutputLocation,
    values: &ClipOutputValues,
    kinds: &[(OutputKind, &str)],
    range_stems: &[String],
) -> Result<BTreeMap<String, Vec<PathBuf>>, ResolveError> {
    let mut out = BTreeMap::new();
    for (kind, ext) in kinds {
        let mut v = vec![];
        for s in range_stems {
            let n = stem_frame_number(s).ok_or_else(|| ResolveError::NoFrameNumber(s.clone()))?;
            v.push(
                resolve_path(loc, values, *kind, kinds.len(), n, ext)
                    .map_err(ResolveError::Location)?,
            );
        }
        out.insert(key(*kind), v);
    }
    Ok(out)
}

/// Legacy layout: `<output_dir>/FG|Matte|Comp|Processed/<stem>.<ext>`.
pub fn legacy_outputs(
    output_dir: &Path,
    kinds: &[(OutputKind, &str)],
    range_stems: &[String],
) -> BTreeMap<String, Vec<PathBuf>> {
    kinds
        .iter()
        .map(|(k, ext)| {
            let d = output_dir.join(k.token());
            (
                key(*k),
                range_stems
                    .iter()
                    .map(|s| d.join(format!("{s}.{ext}")))
                    .collect(),
            )
        })
        .collect()
}

/// Deepest folder common to every output (the manifest folder for templated layouts).
pub fn common_dir(outputs: &BTreeMap<String, Vec<PathBuf>>) -> Option<PathBuf> {
    let mut dirs = outputs.values().flatten().filter_map(|p| p.parent());
    let mut common: Vec<_> = dirs.next()?.components().collect();
    for d in dirs {
        let n = common
            .iter()
            .zip(d.components())
            .take_while(|(a, b)| *a == b)
            .count();
        common.truncate(n);
    }
    Some(common.into_iter().collect())
}

/// A path string as the worker hashes it: a Windows worker (drive, UNC or backslash root) normalises either
/// slash to `\\` (Python `ntpath.normpath`); a POSIX path is used as is.
pub fn worker_path_form(s: &str) -> String {
    let b = s.as_bytes();
    let win = s.starts_with("\\\\")
        || s.starts_with("//")
        || (b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'));
    if win {
        s.replace('/', "\\")
    } else {
        s.to_owned()
    }
}

/// SHA-1 (RFC 3174), only to reproduce the worker's temp-file tag; not used for anything security related.
pub fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, b) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}
