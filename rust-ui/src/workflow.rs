//! Key workflow (`ai-implementation-brief.md` §1): editable inference params and outputs, their
//! persistence in `clip.json`/`project.json`, and the readiness gate that freezes a `KeyJob` from the clip
//! on disk. Pure core (no UI types); never fabricates assets and never writes except in `commit`/`save_location`.
use crate::contracts::*;
use crate::jobs::{
    common_dir, leftover_temps, read_lock, resolve_outputs, JobMode, KeyJob, LockInfo, Require,
    PROTOCOL,
};
use crate::project::*;
use crate::settings::{Settings, HINT_MODES, TEMPORAL_WINDOWS, VRAM_PROFILES};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const OUTPUT_FORMATS: [&str; 2] = ["exr", "png"];
pub const EXR_COMPRESSIONS: [&str; 4] = ["dwab", "piz", "zip", "none"];
pub const MODEL: &str = "corridorkey_v2";

/// Two decimals, as an f64 that prints exactly (`0.95`, not `0.949999988…`): equality with the engine's dict.
pub fn r2(x: f64) -> f64 {
    (x * 100.0).round() / 100.0
}

/// `InferenceParams.to_dict()` (same names and defaults as `backend/service/core.py`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Params {
    pub input_is_linear: bool,
    pub use_alpha_hint: bool,
    pub hint_quality: f64,
    pub carry_hint: bool,
    pub carry_hint_quality: f64,
    pub vram_profile: String,
    pub despill_strength: f64,
    pub cutout_linear: bool,
    pub hint_mode: String,
    pub temporal_window: String,
}
impl Default for Params {
    fn default() -> Self {
        Self {
            input_is_linear: false,
            use_alpha_hint: false,
            hint_quality: 0.95,
            carry_hint: true,
            carry_hint_quality: 0.95,
            vram_profile: "auto".into(),
            despill_strength: 0.5,
            cutout_linear: false,
            hint_mode: "none".into(),
            temporal_window: "auto".into(),
        }
    }
}
impl Params {
    pub fn validate(&self) -> Result<(), String> {
        let one = |n: &str, v: &str, ok: &[&str]| {
            ok.contains(&v)
                .then_some(())
                .ok_or(format!("{n} must be one of {ok:?}"))
        };
        one("hint_mode", &self.hint_mode, &HINT_MODES)?;
        one("temporal_window", &self.temporal_window, &TEMPORAL_WINDOWS)?;
        one("vram_profile", &self.vram_profile, &VRAM_PROFILES)?;
        for (n, v) in [
            ("hint_quality", self.hint_quality),
            ("carry_hint_quality", self.carry_hint_quality),
            ("despill_strength", self.despill_strength),
        ] {
            if !(0.0..=1.0).contains(&v) {
                return Err(format!("{n} must be in [0, 1]"));
            }
        }
        Ok(())
    }
    /// Dict sent to the worker; `use_alpha_hint` always follows `hint_mode` like `InferenceParams.__post_init__`.
    pub fn to_value(&self) -> Value {
        let mut p = self.clone();
        p.use_alpha_hint = p.hint_mode != "none";
        serde_json::to_value(p).unwrap()
    }
}

/// `OutputConfig.to_dict()`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct OutputCfg {
    pub fg_enabled: bool,
    pub fg_format: String,
    pub matte_enabled: bool,
    pub matte_format: String,
    pub comp_enabled: bool,
    pub comp_format: String,
    pub processed_enabled: bool,
    pub processed_format: String,
    pub exr_compression: String,
}
impl Default for OutputCfg {
    fn default() -> Self {
        Self {
            fg_enabled: true,
            fg_format: "exr".into(),
            matte_enabled: true,
            matte_format: "exr".into(),
            comp_enabled: true,
            comp_format: "png".into(),
            processed_enabled: true,
            processed_format: "exr".into(),
            exr_compression: "dwab".into(),
        }
    }
}
impl OutputCfg {
    pub fn validate(&self) -> Result<(), String> {
        for f in [
            &self.fg_format,
            &self.matte_format,
            &self.comp_format,
            &self.processed_format,
        ] {
            if !OUTPUT_FORMATS.contains(&f.as_str()) {
                return Err(format!("output format must be exr or png, got {f:?}"));
            }
        }
        if !EXR_COMPRESSIONS.contains(&self.exr_compression.as_str()) {
            return Err(format!(
                "exr_compression must be one of {EXR_COMPRESSIONS:?}"
            ));
        }
        if self.kinds().is_empty() {
            return Err("No outputs selected; enable at least one output".into());
        }
        Ok(())
    }
    /// Enabled outputs with their file extension, in manifest order.
    pub fn kinds(&self) -> Vec<(OutputKind, &str)> {
        [
            (OutputKind::Fg, self.fg_enabled, &self.fg_format),
            (OutputKind::Matte, self.matte_enabled, &self.matte_format),
            (OutputKind::Comp, self.comp_enabled, &self.comp_format),
            (
                OutputKind::Processed,
                self.processed_enabled,
                &self.processed_format,
            ),
        ]
        .into_iter()
        .filter(|k| k.1)
        .map(|k| (k.0, k.2.as_str()))
        .collect()
    }
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap()
    }
}

/// Overlay `over`'s keys that `base` already has (unknown keys never leak into the engine dicts).
fn overlay(base: &mut Value, over: &Value) {
    if let (Some(b), Some(o)) = (base.as_object_mut(), over.as_object()) {
        for (k, v) in o {
            if b.contains_key(k) {
                b.insert(k.clone(), v.clone());
            }
        }
    }
}

/// Params + outputs for a clip. Precedence: the clip's own `workflow`, then the Qt session values, then the
/// saved AI defaults. `input_is_linear` is left to the caller (the clip's interpretation). Notes say what
/// was unusable and ignored.
pub fn seed(clip: &Clip, project: &Project, st: &Settings) -> (Params, OutputCfg, Vec<String>) {
    let a = &st.ai;
    let mut p = Params {
        vram_profile: a.vram_profile.clone(),
        hint_mode: a.hint_mode.clone(),
        hint_quality: r2(a.hint_quality as f64),
        carry_hint: a.carry_hint,
        carry_hint_quality: r2(a.carry_hint_quality as f64),
        despill_strength: r2(a.despill_strength as f64),
        cutout_linear: a.cutout_linear,
        temporal_window: a.temporal_window.clone(),
        ..Default::default()
    };
    p.use_alpha_hint = p.hint_mode != "none";
    let (mut pv, mut ov) = (p.to_value(), OutputCfg::default().to_value());
    let mut notes = vec![];
    let from_workflow = clip.workflow.as_ref().and_then(|w| w.get("params"));
    for src in [project.session_params.as_ref(), from_workflow]
        .into_iter()
        .flatten()
    {
        let mut src = src.clone();
        if let Some(o) = src.as_object_mut() {
            // Qt sessions written before hint_mode existed only have use_alpha_hint.
            if !o.contains_key("hint_mode") {
                if let Some(b) = o.get("use_alpha_hint").and_then(Value::as_bool) {
                    o.insert("hint_mode".into(), json!(if b { "full" } else { "none" }));
                }
            }
        }
        overlay(&mut pv, &src);
    }
    for src in [
        project.session_output_config.as_ref(),
        clip.workflow.as_ref().and_then(|w| w.get("output_config")),
    ]
    .into_iter()
    .flatten()
    {
        overlay(&mut ov, src);
    }
    let params = serde_json::from_value::<Params>(pv)
        .ok()
        .filter(|p| p.validate().is_ok())
        .unwrap_or_else(|| {
            notes.push("Stored Key settings were invalid and ignored; defaults are shown.".into());
            p
        });
    let out = serde_json::from_value::<OutputCfg>(ov)
        .ok()
        .filter(|o| o.validate().is_ok())
        .unwrap_or_else(|| {
            notes.push(
                "Stored output settings were invalid and ignored; defaults are shown.".into(),
            );
            OutputCfg::default()
        });
    (params, out, notes)
}

// ---------------------------------------------------------------- output location

pub const DATE_FORMAT: &str = "DD-MM-YYYY";

/// `project.json["output_location"]`; `None` = legacy layout.
pub fn read_location(project: &Project) -> Result<Option<OutputLocation>, String> {
    parse_location(project.output_location.as_ref())
}

pub fn parse_location(v: Option<&Value>) -> Result<Option<OutputLocation>, String> {
    match v {
        None | Some(Value::Null) => Ok(None),
        Some(v) => serde_json::from_value(v.clone())
            .map(Some)
            .map_err(|e| format!("output_location in project.json is invalid: {e}")),
    }
}

/// Contract templates used when the project has no saved output location: under the clip's own output
/// folder, so no machine-specific path is ever assumed. `None` = the clip has no output folder.
pub fn default_location(clip: &Clip) -> Option<OutputLocation> {
    Some(OutputLocation {
        root: abs(clip.output_dir.as_ref()?),
        folder_template: "{name}_{version}/{output}".into(),
        file_template: "{name}_####".into(),
    })
}

/// Atomic write of `output_location` into `project.json`, other keys preserved.
pub fn save_location(project_root: &Path, loc: &OutputLocation) -> Result<Value, String> {
    validate_templates(loc, 1).map_err(|e| format!("{e:?}"))?;
    let v = json!({"root": loc.root, "folder_template": loc.folder_template, "file_template": loc.file_template, "date_format": DATE_FORMAT});
    update_json(&project_root.join("project.json"), |m| {
        m.insert("output_location".into(), v.clone());
    })?;
    Ok(v)
}

/// Back to the legacy layout (`<output>/FG|Matte|Comp|Processed/<stem>.<ext>`).
pub fn clear_location(project_root: &Path) -> Result<(), String> {
    update_json(&project_root.join("project.json"), |m| {
        m.remove("output_location");
    })
}

/// Today as `DD-MM-YYYY` (UTC; Hinnant's civil-from-days).
pub fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let z = (secs / 86400) as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z.rem_euclid(146097);
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let (y, doy) = (yoe + era * 400, doe - (365 * yoe + yoe / 4 - yoe / 100));
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{d:02}-{m:02}-{:04}", if m <= 2 { y + 1 } else { y })
}

/// Random 128-bit id, uuid-shaped. Not cryptographic: it only has to be unique and stable.
pub fn new_id() -> String {
    use std::hash::{BuildHasher, Hasher};
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mut s = String::new();
    for _ in 0..2 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(t);
        h.write_u32(std::process::id());
        h.write_u64(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        s += &format!("{:016x}", h.finish());
    }
    format!(
        "{}-{}-{}-{}-{}",
        &s[..8],
        &s[8..12],
        &s[12..16],
        &s[16..20],
        &s[20..32]
    )
}

// ---------------------------------------------------------------- the gate

pub struct Inputs<'a> {
    pub clip: &'a Clip,
    /// Only what the gate needs from the project, so it can run on a background thread with owned copies.
    pub project_name: &'a str,
    /// `project.json["output_location"]`; absent = the default templates under the clip's output folder.
    pub location: Option<&'a Value>,
    pub params: &'a Params,
    pub out: &'a OutputCfg,
    /// Inclusive indices into `clip.input_files`.
    pub range: (usize, usize),
    /// Reference digest of the model (`None` = unknown; the scheduler decides what that means).
    pub digest: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    New,
    /// Continue the previous run in the same folder (only if its manifest is compatible).
    Resume,
    /// Same settings, next free `{version}`.
    NewVersion,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub job: KeyJob,
    /// Stable id for the clip (existing or freshly generated; `commit` stores it).
    pub clip_id: String,
    pub clip_id_is_new: bool,
    /// `output_values` to store.
    pub values: Value,
    pub values_changed: bool,
    /// Human summary: "AI will process frames 1101–1300 (200 of 412)…".
    pub summary: String,
}

#[derive(Debug, Default)]
pub struct Existing {
    pub reason: String,
    /// The previous run can be continued as is.
    pub resume: bool,
    /// Next free version when the templates have `{version}`; `None` = cannot write a new version.
    pub next_version: Option<u32>,
}

fn abs(p: &Path) -> PathBuf {
    std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf())
}

/// One line for a lock the core found (what the file says, nothing inferred).
pub fn describe_lock(l: &LockInfo) -> String {
    format!(
        "job {} on {}{} (last heartbeat {})",
        l.job_id.as_deref().unwrap_or("?"),
        l.host.as_deref().unwrap_or("?"),
        l.pid.map_or(String::new(), |p| format!(", pid {p}")),
        l.heartbeat_age
            .map_or("unknown".into(), |d| format!("{} s ago", d.as_secs()))
    )
}

#[derive(Debug, Default)]
pub struct Blocked {
    pub reasons: Vec<String>,
    pub existing: Option<Existing>,
    /// A lock file holds the folder (e.g. a run from before a restart). Only an explicit writer-gone
    /// confirmation (`jobs::recover_orphan`) can release it; age never does.
    pub lock: Option<LockInfo>,
    /// The folder the lock is in.
    pub lock_dir: Option<PathBuf>,
    /// That run's temp files left in the folder (exact core-recognised names), listed for review.
    pub leftovers: Vec<PathBuf>,
}
#[allow(clippy::result_large_err)] // `Blocked` is a rare, user-facing verdict, never in a hot path
fn blocked<T>(r: impl Into<String>) -> Result<T, Blocked> {
    Err(Blocked {
        reasons: vec![r.into()],
        ..Default::default()
    })
}

/// Why `manifest` cannot be continued by `job` (`None` = compatible). Rust port of `key_job.resume_problem`.
pub fn resume_problem(m: &Value, job: &KeyJob) -> Option<String> {
    if m.get("version").and_then(Value::as_u64) != Some(3) {
        return Some("the existing result is not a v3 manifest".into());
    }
    if m.get("engine").and_then(Value::as_str) != Some("corridorkey-v2")
        || m.get("clip_id").and_then(Value::as_str) != Some(&job.clip_id)
    {
        return Some("the existing result belongs to another clip".into());
    }
    if m.get("range") != serde_json::to_value(&job.range).ok().as_ref() {
        return Some("In/Out changed; a range change is never resumable".into());
    }
    let mut recorded = m.get("params").cloned().unwrap_or(Value::Null);
    if let Some(o) = recorded.as_object_mut() {
        let full = o.get("use_alpha_hint").and_then(Value::as_bool) == Some(true);
        o.entry("hint_mode")
            .or_insert(json!(if full { "full" } else { "none" }));
        o.entry("temporal_window").or_insert(json!("auto"));
    }
    let cfg: OutputCfg = serde_json::from_value(job.output_config.clone()).unwrap_or_default();
    let enabled: Vec<&str> = job.outputs.keys().map(String::as_str).collect();
    let formats = json!({"fg": cfg.fg_format, "matte": cfg.matte_format, "comp": cfg.comp_format, "processed": cfg.processed_format});
    let files: BTreeMap<&String, Vec<String>> = job
        .outputs
        .iter()
        .map(|(k, v)| {
            (
                k,
                v.iter()
                    .map(|p| {
                        p.strip_prefix(&job.manifest_dir)
                            .unwrap_or(p)
                            .to_string_lossy()
                            .into_owned()
                    })
                    .collect(),
            )
        })
        .collect();
    let marks = job
        .hint_keyframes_abs
        .as_ref()
        .map_or(Value::Null, |k| json!(k));
    let checks = [
        (recorded == job.params, "settings changed"),
        (
            m.get("enabled_outputs")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>())
                .is_some_and(|a| {
                    let mut a = a;
                    a.sort();
                    let mut e = enabled.clone();
                    e.sort();
                    a == e
                }),
            "enabled outputs changed",
        ),
        (m.get("formats") == Some(&formats), "output formats changed"),
        (
            m.get("exr_compression").and_then(Value::as_str) == Some(&cfg.exr_compression),
            "EXR compression changed",
        ),
        (
            m.get("hint_keyframes").unwrap_or(&Value::Null) == &marks,
            "keyframe marks changed",
        ),
        (
            m.get("files").is_some_and(|f| *f == json!(files)),
            "output paths changed",
        ),
        (
            m.get("effective_profile").is_some_and(Value::is_object),
            "no recorded temporal profile",
        ),
    ];
    checks.into_iter().find(|c| !c.0).map(|c| c.1.to_string())
}

fn read_manifest_value(dir: &Path) -> Option<Value> {
    let p = dir.join(MANIFEST_NAME);
    std::fs::symlink_metadata(&p).ok()?;
    Some(
        std::fs::read(&p)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or(Value::Null),
    )
}

type Resolved = (BTreeMap<String, Vec<PathBuf>>, PathBuf);

/// Build the frozen job, or say exactly why not. Reads the disk (existence only), never writes.
#[allow(clippy::result_large_err)]
pub fn plan(i: &Inputs, intent: Intent) -> Result<Plan, Blocked> {
    let clip = i.clip;
    let (a, b) = i.range;
    if let Some(e) = &clip.error {
        return blocked(e.clone());
    }
    if !matches!(clip.source, Source::Sequence { .. }) {
        return blocked("Range keying needs an image sequence; this clip is a video (extraction is not integrated yet)");
    }
    let stems: Vec<String> = clip.input_files.iter().map(|f| f.0.clone()).collect();
    let range = RangeIdentity::from_inclusive(&stems, a, b).map_err(|e| Blocked {
        reasons: vec![format!("Source changed: re-set In/Out ({e:?})")],
        ..Default::default()
    })?;
    range.validate(&stems).map_err(|_| Blocked {
        reasons: vec!["Source changed: re-set In/Out".into()],
        ..Default::default()
    })?;
    let range_stems = &stems[a..=b];

    let mut reasons = vec![];
    if let Err(e) = i.params.validate() {
        reasons.push(e);
    }
    if let Err(e) = i.out.validate() {
        reasons.push(e);
    }
    // Hint readiness, by stem (the legacy ordinal rule stays as a fallback).
    let mut marks = None;
    if i.params.hint_mode != "none" && list_images(&clip.root.join("AlphaHint")).is_empty() {
        let has_video = std::fs::read_dir(clip.root.join("AlphaHint"))
            .map(|d| {
                d.flatten()
                    .any(|e| VIDEO_EXTS.contains(&ext_of(&e.path()).as_str()))
            })
            .unwrap_or(false);
        if has_video {
            reasons.push("The AlphaHint is a video; range keying needs an image sequence. Extract it to frames or regenerate the hint as a sequence.".into());
        }
    }
    match i.params.hint_mode.as_str() {
        "full" => {
            let have: std::collections::BTreeSet<String> =
                list_images(&clip.root.join("AlphaHint"))
                    .into_iter()
                    .map(|h| h.0)
                    .collect();
            let missing: Vec<&String> = range_stems.iter().filter(|s| !have.contains(*s)).collect();
            if !missing.is_empty() && have.len() != stems.len() {
                let list: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
                reasons.push(format!(
                    "Needs hint (range changed): no hint for {} frame(s), e.g. {}",
                    missing.len(),
                    list.join(", ")
                ));
            }
        }
        "keyframes" => {
            if clip.alpha_frames == 0 {
                reasons.push("Needs hint: this clip has no AlphaHint".into());
            }
            let inside: Vec<usize> = clip
                .keyframes
                .iter()
                .copied()
                .filter(|k| (a..=b).contains(k))
                .collect();
            if inside.is_empty() {
                reasons.push("Needs keyframe marks (none inside In/Out)".into());
            }
            marks = Some(inside);
        }
        _ => {}
    }
    if !reasons.is_empty() {
        return Err(Blocked {
            reasons,
            ..Default::default()
        });
    }

    let kinds = i.out.kinds();
    let loc = parse_location(i.location).map_err(|e| Blocked {
        reasons: vec![e],
        ..Default::default()
    })?;
    let (res, date) = clip
        .sample_size
        .map(|(w, h)| resolution_label(w, h))
        .map_or(("clip".to_string(), today()), |r| (r, today()));
    let stored = clip.output_values.as_ref();
    let sv = |k: &str| {
        stored
            .and_then(|v| v.get(k))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let (resolution, date) = (sv("resolution").unwrap_or(res), sv("date").unwrap_or(date));
    let stored_version = stored
        .and_then(|v| v.get("version"))
        .and_then(Value::as_u64)
        .map_or(1, |v| v.clamp(1, 999) as u32);
    let values_for = |version: u32| ClipOutputValues {
        project: i.project_name.to_owned(),
        name: clip.name.clone(),
        resolution: resolution.clone(),
        date: date.clone(),
        version,
    };
    let Some(loc) = loc.or_else(|| default_location(clip)).map(|mut l| {
        l.root = abs(&l.root);
        l
    }) else {
        return blocked(
            "This clip has no output folder: set an output location in the Deliver tab",
        );
    };
    let resolve = |version: u32| -> Result<Resolved, String> {
        let o =
            resolve_outputs(&loc, &values_for(version), &kinds, range_stems).map_err(
                |e| match e {
                    crate::jobs::ResolveError::Location(e) => format!("Output location: {e:?}"),
                    crate::jobs::ResolveError::NoFrameNumber(s) => {
                        format!(
                            "Frame name {s:?} has no trailing number for the #### file template"
                        )
                    }
                },
            )?;
        let d = common_dir(&o).ok_or("no output files")?;
        Ok((o, d))
    };
    let (mut version, mut sel_intent) = (stored_version, intent);
    let (mut outputs, mut mdir) = resolve(version).map_err(|e| Blocked {
        reasons: vec![e],
        ..Default::default()
    })?;
    let exists = |o: &BTreeMap<String, Vec<PathBuf>>, d: &Path| {
        std::fs::symlink_metadata(d.join(MANIFEST_NAME)).is_ok()
            || std::fs::symlink_metadata(d.join(crate::jobs::LOCK_NAME)).is_ok()
            || o.values()
                .flatten()
                .any(|p| std::fs::symlink_metadata(p).is_ok())
    };

    let clip_id = clip.clip_id.clone().unwrap_or_else(new_id);
    let mut mode = JobMode::New;
    let mut resume_json = None;
    if intent == Intent::NewVersion {
        let first = mdir.clone();
        let mut found = None;
        for v in stored_version + 1..=999 {
            let (o, d) = resolve(v).map_err(|e| Blocked {
                reasons: vec![e],
                ..Default::default()
            })?;
            if d == first {
                break; // no {version} in the templates: bumping cannot change anything
            }
            if !exists(&o, &d) {
                found = Some((v, o, d));
                break;
            }
        }
        let Some((v, o, d)) = found else {
            return blocked("No new version is possible: add {version} to the folder template (or free a version)");
        };
        (version, outputs, mdir, sel_intent) = (v, o, d, Intent::New);
    }
    match read_lock(&mdir) {
        Ok(None) => {}
        Err(e) => return blocked(format!("The folder's lock file cannot be trusted: {e}")),
        Ok(Some(lock)) => {
            let leftovers = lock
                .job_id
                .as_deref()
                .map(|id| leftover_temps(&mdir, id))
                .unwrap_or_default();
            let why = if lock.job_id.is_some() {
                "If that process is gone, confirm it below; nothing is released automatically."
            } else {
                "The lock does not name a job, so it cannot be released from here."
            };
            return Err(Blocked {
                reasons: vec![format!(
                    "This folder is locked by {}. {why}",
                    describe_lock(&lock)
                )],
                lock: Some(lock),
                lock_dir: Some(mdir),
                leftovers,
                ..Default::default()
            });
        }
    }
    if exists(&outputs, &mdir) {
        resume_json = read_manifest_value(&mdir);
        let complete = resume_json
            .as_ref()
            .and_then(|m| m.get("complete"))
            .and_then(Value::as_bool)
            == Some(true);
        let next = {
            let mut n = None;
            for v in stored_version + 1..=999 {
                match resolve(v) {
                    Ok((o, d)) if d != mdir && !exists(&o, &d) => {
                        n = Some(v);
                        break;
                    }
                    Ok((_, d)) if d == mdir => break,
                    _ => {}
                }
            }
            n
        };
        if sel_intent != Intent::Resume {
            let probe = build(
                i,
                &clip_id,
                range.clone(),
                marks.clone(),
                outputs.clone(),
                mdir.clone(),
                JobMode::Resume,
            );
            let resume = !complete
                && resume_json
                    .as_ref()
                    .is_some_and(|m| resume_problem(m, &probe).is_none());
            let reason = format!(
                "A previous result{} already exists in {}",
                if complete { " (complete)" } else { "" },
                mdir.display()
            );
            return Err(Blocked {
                reasons: vec![],
                existing: Some(Existing {
                    reason,
                    resume,
                    next_version: next,
                }),
                ..Default::default()
            });
        }
        mode = JobMode::Resume;
    } else if sel_intent == Intent::Resume {
        return blocked("Nothing to resume: no previous result in the output folder");
    }
    let job = build(i, &clip_id, range, marks, outputs, mdir, mode);
    if mode == JobMode::Resume {
        if let Some(why) = resume_json
            .as_ref()
            .and_then(|m| resume_problem(m, &job))
            .or_else(|| {
                resume_json
                    .is_none()
                    .then(|| "the existing manifest is unreadable".into())
            })
        {
            return blocked(format!("Cannot resume: {why}. Write as a new version."));
        }
    }
    job.validate().map_err(|e| Blocked {
        reasons: vec![e.to_string()],
        ..Default::default()
    })?;
    job.check_disk().map_err(|e| Blocked {
        reasons: vec![e.to_string()],
        ..Default::default()
    })?;
    let values = json!({"resolution": resolution, "date": date, "version": version});
    let values_changed = stored != Some(&values);
    let n = range_stems.len();
    let names: Vec<String> = kinds
        .iter()
        .map(|(k, e)| format!("{} ({})", k.token(), e.to_uppercase()))
        .collect();
    let summary = format!(
        "AI will process frames {}–{} ({n} of {}). Temporal context stays inside this range. Writes {} to {}{}",
        job.range.in_stem,
        job.range.out_stem,
        stems.len(),
        names.join(", "),
        job.manifest_dir.display(),
        if mode == JobMode::Resume { " (resuming a previous run)" } else { "" }
    );
    Ok(Plan {
        clip_id_is_new: clip.clip_id.is_none(),
        clip_id,
        values,
        values_changed,
        summary,
        job,
    })
}

fn build(
    i: &Inputs,
    clip_id: &str,
    range: RangeIdentity,
    marks: Option<Vec<usize>>,
    outputs: BTreeMap<String, Vec<PathBuf>>,
    mdir: PathBuf,
    mode: JobMode,
) -> KeyJob {
    let mut params = i.params.clone();
    params.use_alpha_hint = params.hint_mode != "none";
    KeyJob {
        job_id: new_id(),
        clip_id: clip_id.into(),
        clip_root: abs(&i.clip.root),
        range,
        params: params.to_value(),
        output_config: i.out.to_value(),
        hint_keyframes_abs: marks,
        outputs,
        manifest_dir: mdir,
        mode,
        publish: "final".into(),
        path_map: vec![],
        require: Require {
            protocol: PROTOCOL,
            models: [(MODEL.to_string(), i.digest.map(str::to_owned))].into(),
        },
    }
}

/// Persist what a submitted job used: `clip_id` (once), `workflow`, `output_values` and the result folder.
/// Atomic, other keys preserved. Call before `Scheduler::submit`; on `Err` nothing was queued.
pub fn commit(clip: &Clip, plan: &Plan, params: &Params, out: &OutputCfg) -> Result<(), String> {
    update_json(&clip.meta_path, |m: &mut Map<String, Value>| {
        if plan.clip_id_is_new || !m.contains_key("clip_id") {
            m.insert("clip_id".into(), json!(plan.clip_id));
        }
        m.insert(
            "workflow".into(),
            json!({"version": 1, "params": params.to_value(), "output_config": out.to_value()}),
        );
        if plan.values_changed {
            m.insert("output_values".into(), plan.values.clone());
        }
        m.insert(
            "key_result".into(),
            json!({"manifest_dir": plan.job.manifest_dir}),
        );
    })
}
