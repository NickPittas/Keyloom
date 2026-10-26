//! The frozen Key job as it crosses the network, and the receiving installation's preflight.
//!
//! `JobSpec` is the EXACT wire schema of the reviewed coordinator (`backend/coordinator/schema.py::validate_spec`):
//! protocol, kind "key", mode "new", clip_id, clip_root, range, params, output_config, hint_keyframes_abs, ONE output
//! location (root + templates + values, expanded by the receiver), manifest_dir, publish "final", require and an
//! optional descriptive `preset_name`. `Envelope` is the submit body (`submission_id` + spec + target). There are no
//! per-frame lists, no host path, no path map, and nothing the server schema does not know (shot overrides, frozen
//! hint routes and exports are NOT carried: this pilot is Key only, new only).
//!
//! The receiver resolves the three `shared:` references through ITS OWN mappings, scans ITS OWN copy of the clip
//! (self-contained mapped `Frames`/`Input` + `AlphaHint` only; any external or movie source is refused), checks the
//! range stems and the selected hint frames by name, expands the outputs with the same templates the app uses
//! (`jobs::resolve_outputs`), requires them to be new and writable, and builds the local `KeyJob`. Pure data + disk
//! checks: no network, no scheduler.
//!
//! Hint frames must carry exactly the source stems (the app's own `list_images` names). The worker may accept more
//! (case, suffix or ordinal fallbacks); this check is intentionally narrower, so it can refuse a job the worker
//! would have run, never the other way round.
//!
//! Confinement: every resolved path is also checked, symlinks followed, to stay inside the mapped location its own
//! reference names (clip and its frames/hints against the clip's location, outputs against the output root's, the
//! result folder against its own). This is a check at one moment: it cannot stop a symlink being swapped in after
//! preflight, and the worker has no roots to re-check with. The pilot therefore trusts paired clients and the
//! storage they share.
use crate::contracts::{resolve_path, ClipOutputValues, OutputLocation, RangeError, RangeIdentity};
use crate::hints::HintCfg;
use crate::presets::{Delivery, PipelineExport};
use crate::project::Clip;
use crate::tasks::{TaskJob, TaskKind};
use crate::jobs::{common_dir, resolve_outputs, stem_frame_number, JobMode, KeyJob, Require, LOCK_NAME, PROTOCOL};
use crate::project::{list_images, load_project, LoadOptions, Source, MANIFEST_NAME};
use crate::shared_paths::{self as sp, SharedRef, SharedRoot};
use crate::workflow::{OutputCfg, Params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The server rejects specs whose canonical JSON exceeds 64 KiB; stay safely under.
pub const MAX_SPEC_BYTES: usize = 60 * 1024;
const MAX_PROBLEMS: usize = 20;
const MAX_VERSION: u32 = 99_999;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecValues {
    pub project: String,
    pub name: String,
    pub resolution: String,
    pub date: String,
    pub version: u32,
}

/// The one output location: a reference for the root plus the templates and values the receiver expands.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecOutput {
    pub root: String,
    pub folder_template: String,
    pub file_template: String,
    pub values: SpecValues,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JobSpec {
    pub protocol: u32,
    pub kind: String,
    pub mode: String,
    pub clip_id: String,
    pub clip_root: String,
    pub range: RangeIdentity,
    pub params: Params,
    pub output_config: OutputCfg,
    pub hint_keyframes_abs: Option<Vec<usize>>,
    pub output: SpecOutput,
    pub manifest_dir: String,
    pub publish: String,
    pub require: Require,
    /// Descriptive only; the receiver never looks a preset up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_name: Option<String>,
    /// `kind == "workflow"` only: the frozen hint route. The receiver rebuilds the hint steps from it on ITS clip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<HintCfg>,
    /// `kind == "workflow"` only: the task kinds the sender's review showed, in order; the receiver's rebuild must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint_stages: Option<Vec<String>>,
    /// `kind == "workflow"` only: the external image-sequence folder the shot reads, as a reference. The receiver
    /// resolves it through ITS mapping and tells its worker (`source_map`); clip.json's recorded path is never used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// `kind == "workflow"` only: frozen video deliveries after Key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<SpecExport>,
}

/// Frozen Deliver stage: a destination FOLDER reference plus the file name of each video product.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SpecExport {
    pub fps: Option<f64>,
    pub allow_partial: bool,
    pub root: String,
    pub outputs: std::collections::BTreeMap<String, Delivery>,
    /// product -> file name (a single path component) inside `root`.
    pub files: std::collections::BTreeMap<String, String>,
}

/// One execution stage on the receiver, in dependency order (each waits for the one before).
#[derive(Debug, Clone)]
pub enum Stage {
    Task(TaskJob),
    Key(KeyJob),
}

impl Stage {
    pub fn job_id(&self) -> &str {
        match self {
            Stage::Task(t) => &t.job_id,
            Stage::Key(k) => &k.job_id,
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Stage::Task(t) if t.kind == TaskKind::ExportVideo => "Deliver",
            Stage::Task(_) => "Hint",
            Stage::Key(_) => "Key",
        }
    }
}

/// Everything the sender froze for one shot (see `JobSpec::workflow`).
pub struct WorkflowParts<'a> {
    pub roots: &'a [SharedRoot],
    pub clip: &'a Clip,
    /// The plan's clip id (the shot gets it written when queued).
    pub clip_id: &'a str,
    pub manifest_dir: &'a Path,
    pub range: RangeIdentity,
    pub params: &'a Params,
    pub out: &'a OutputCfg,
    pub hint_keyframes_abs: Option<Vec<usize>>,
    pub loc: &'a OutputLocation,
    pub values: &'a ClipOutputValues,
    /// The confirmed Key plan's requirement.
    pub require: Require,
    /// The review's hint steps (may be empty); only their kinds are frozen.
    pub steps: &'a [TaskJob],
    pub hint: Option<&'a HintCfg>,
    pub export: Option<(&'a PipelineExport, &'a [(String, PathBuf)])>,
    pub preset_name: Option<&'a str>,
}

/// The submit body (`POST /v1/jobs`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// 16-64 characters of A-Z a-z 0-9 _ - ; the idempotency key.
    pub submission_id: String,
    pub spec: JobSpec,
    /// An installation id, or `None` = automatic placement.
    pub target: Option<String>,
}

/// The receiver's own scan of the clip.
struct Scan {
    stems: Vec<String>,
    /// Source frame files in stem order.
    files: Vec<PathBuf>,
    source_dir: PathBuf,
    /// AlphaHint frames by stem.
    hint: std::collections::BTreeMap<String, PathBuf>,
    /// The clip's saved keyframe marks (absolute indices).
    keyframes: Vec<usize>,
    clip: Clip,
    project_root: PathBuf,
    /// `[(recorded path, local folder)]` for the worker when the source is external.
    source_map: Vec<(String, String)>,
}

pub enum SourceKind {
    /// An image sequence inside the shared clip folder (Frames or Input): the worker finds it by itself.
    Internal(PathBuf),
    /// An image sequence outside the clip that clip.json references: a receiver must resolve its own copy.
    External(PathBuf),
}

/// What the shot reads, as the SENDER sees it. A movie is only usable once its frames are extracted (then the clip's
/// Frames are the source and clip.json's recorded movie path is never followed); an unextracted movie has no frames
/// to range over and is refused by name.
pub fn source_kind(clip: &Clip, clip_root: &Path) -> Result<SourceKind, String> {
    if let Some(e) = &clip.error {
        return Err(format!("clip is not usable: {e}"));
    }
    let internal = |d: &Path| d == clip_root.join("Frames") || d == clip_root.join("Input");
    match &clip.source {
        Source::Sequence { dir, external: false } if internal(dir) => Ok(SourceKind::Internal(dir.clone())),
        Source::Sequence { dir, external: true } => Ok(SourceKind::External(dir.clone())),
        Source::Video { path } => Err(format!("movie source {}: extract its frames first (Extract frames); a movie has no frames to range over until then", path.display())),
        _ => Err("the source is not an image sequence".into()),
    }
}

/// What a successful preflight hands to the scheduler: the validated local job and what was checked.
#[derive(Debug, Clone)]
pub struct Preflight {
    pub job: KeyJob,
    /// Execution order: hint tasks, Key, deliver tasks (the Key stage equals `job`).
    pub stages: Vec<Stage>,
    /// First frame of the range as this PC reads it (for the pinned-window memory estimate).
    pub first_frame: PathBuf,
    /// Write probes by folder (kept so the receiver can report them).
    pub probes: Vec<(PathBuf, sp::Access)>,
}

const MAX_PROBLEMS_NOTE: &str = "…and more";

fn push(problems: &mut Vec<String>, p: String) {
    if problems.len() < MAX_PROBLEMS {
        problems.push(p);
    } else if problems.len() == MAX_PROBLEMS {
        problems.push(MAX_PROBLEMS_NOTE.into());
    }
}

fn confine(problems: &mut Vec<String>, roots: &[SharedRoot], loc: &str, p: &Path) {
    if let Err(e) = sp::confined(roots, loc, p) {
        push(problems, e);
    }
}

fn ref_of(roots: &[SharedRoot], p: &Path, what: &str) -> Result<String, String> {
    sp::reference_for(roots, p).map(|r| r.encode()).map_err(|e| format!("{what}: {e}"))
}

fn text_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.chars().count() <= max && !s.chars().any(char::is_control)
}

/// Bytes of this JSON as Python's `json.dumps(..., ensure_ascii=True)` writes it: one `\uXXXX` (6) per UTF-16 unit.
fn ascii_len(json: &str) -> usize {
    json.chars().map(|c| if c.is_ascii() { 1 } else { 6 * c.len_utf16() }).sum()
}

pub fn submission_id_ok(id: &str) -> bool {
    (16..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl JobSpec {
    /// Build the spec on the submitting PC. Each of the three locations must lie inside one of THIS installation's
    /// mapped locations, otherwise the receiver could not find it; refused here, naming which.
    #[allow(clippy::too_many_arguments)]
    pub fn from_parts(
        roots: &[SharedRoot],
        clip_root: &Path,
        manifest_dir: &Path,
        clip_id: &str,
        range: RangeIdentity,
        params: &Params,
        out: &OutputCfg,
        hint_keyframes_abs: Option<Vec<usize>>,
        loc: &OutputLocation,
        values: &ClipOutputValues,
        require: Require,
        preset_name: Option<&str>,
    ) -> Result<Self, String> {
        sp::validate_roots(roots)?;
        let mut params = params.clone();
        params.use_alpha_hint = params.hint_mode != "none"; // always follows hint_mode (the server checks it)
        let s = Self {
            protocol: PROTOCOL,
            kind: "key".into(),
            mode: "new".into(),
            clip_id: clip_id.into(),
            clip_root: ref_of(roots, clip_root, "clip folder")?,
            range,
            params,
            output_config: out.clone(),
            hint_keyframes_abs,
            output: SpecOutput {
                root: ref_of(roots, &loc.root, "output root")?,
                folder_template: loc.folder_template.clone(),
                file_template: loc.file_template.clone(),
                values: SpecValues {
                    project: values.project.clone(),
                    name: values.name.clone(),
                    resolution: values.resolution.clone(),
                    date: values.date.clone(),
                    version: values.version,
                },
            },
            manifest_dir: ref_of(roots, manifest_dir, "result folder")?,
            publish: "final".into(),
            require,
            preset_name: preset_name.map(str::to_owned),
            hint: None,
            hint_stages: None,
            source: None,
            export: None,
        };
        s.check_shape()?;
        Ok(s)
    }

    /// The submitter's frozen Hint -> Key -> Deliver contract. `kind` is "key" only when there is nothing but Key.
    /// A source the receiver cannot resolve logically (movie, external sequence) is refused here by name.
    pub fn workflow(w: &WorkflowParts) -> Result<Self, String> {
        sp::validate_roots(w.roots)?;
        let ext = match source_kind(w.clip, &w.clip.root)? {
            SourceKind::External(d) => Some(ref_of(w.roots, &d, "external source folder")?),
            SourceKind::Internal(_) => None,
        };
        let mut s = Self::from_parts(
            w.roots,
            &w.clip.root,
            w.manifest_dir,
            w.clip_id,
            w.range.clone(),
            w.params,
            w.out,
            w.hint_keyframes_abs.clone(),
            w.loc,
            w.values,
            w.require.clone(),
            w.preset_name,
        )?;
        if w.steps.is_empty() && w.export.is_none() && ext.is_none() {
            return Ok(s);
        }
        s.source = ext;
        let mut models = s.require.models.clone();
        if !w.steps.is_empty() {
            let h = w.hint.ok_or("hint steps need the frozen hint settings")?;
            for t in w.steps {
                if matches!(t.kind, TaskKind::PrepareSource | TaskKind::ImportHint | TaskKind::ExportVideo | TaskKind::SamPreview) {
                    return Err(format!("{} cannot be rebuilt on a receiver from the hint settings", t.kind.label()));
                }
                models.extend(t.require.models.clone());
            }
            s.hint = Some(h.clone());
            s.hint_stages = Some(w.steps.iter().map(|t| t.kind.wire().to_owned()).collect());
        }
        if let Some((e, dests)) = w.export {
            e.validate_videos()?;
            let dir = dests.first().and_then(|d| d.1.parent()).ok_or("no delivery destination")?;
            let mut files = std::collections::BTreeMap::new();
            for (product, d) in dests {
                if d.parent() != Some(dir) {
                    return Err("all video deliveries must go to one folder".into());
                }
                files.insert(product.clone(), d.file_name().and_then(|n| n.to_str()).ok_or("destination file name is not UTF-8")?.to_owned());
            }
            s.export = Some(SpecExport { fps: e.fps, allow_partial: e.allow_partial, root: ref_of(w.roots, dir, "delivery folder")?, outputs: e.outputs.clone(), files });
        }
        s.kind = "workflow".into();
        s.require.models = models;
        s.check_shape()?;
        Ok(s)
    }

    /// Everything the server checks that needs no disk or mapping, so a bad spec is refused before submit.
    pub fn check_shape(&self) -> Result<(), String> {
        if self.protocol != PROTOCOL || self.require.protocol != PROTOCOL {
            return Err(format!("unsupported protocol (this build speaks {PROTOCOL})"));
        }
        match self.kind.as_str() {
            "key" if self.hint.is_none() && self.hint_stages.is_none() && self.export.is_none() && self.source.is_none() => {}
            "key" => return Err("a \"key\" job carries no hint, export or external source; use kind \"workflow\"".into()),
            "workflow" => self.check_workflow()?,
            k => return Err(format!("unsupported job kind {k:?}")),
        }
        if self.mode != "new" {
            return Err("network jobs support mode \"new\" only".into());
        }
        if self.publish != "final" {
            return Err("publish must be \"final\"".into());
        }
        if !text_ok(&self.clip_id, 128) || self.preset_name.as_deref().is_some_and(|p| !text_ok(p, 128)) {
            return Err("clip_id / preset_name must be 1-128 characters without control characters".into());
        }
        let r = &self.range;
        if r.in_point > r.out_point || !text_ok(&r.in_stem, 255) || !text_ok(&r.out_stem, 255) {
            return Err("range needs In <= Out and stems of 1-255 characters".into());
        }
        self.params.validate()?;
        if self.params.use_alpha_hint != (self.params.hint_mode != "none") {
            return Err("params.use_alpha_hint disagrees with params.hint_mode".into());
        }
        self.output_config.validate()?;
        if self.output_config.kinds().is_empty() {
            return Err("output_config enables no output".into());
        }
        match (&self.hint_keyframes_abs, self.params.hint_mode == "keyframes") {
            (Some(k), true) => {
                if k.is_empty() || k.len() > 4096 || k.windows(2).any(|w| w[0] >= w[1]) || k.iter().any(|m| *m < r.in_point || *m > r.out_point) {
                    return Err("hint_keyframes_abs must be ascending, unique and inside the range (at most 4096)".into());
                }
            }
            (None, false) => {}
            _ => return Err("hint_keyframes_abs is required exactly when hint_mode is keyframes".into()),
        }
        for (what, s) in [("clip_root", &self.clip_root), ("manifest_dir", &self.manifest_dir), ("output.root", &self.output.root)] {
            SharedRef::decode(s).map_err(|e| format!("{what}: {e}"))?;
        }
        let v = &self.output.values;
        if ![&v.project, &v.name, &v.resolution, &v.date].iter().all(|s| text_ok(s, 128)) || v.version > MAX_VERSION {
            return Err("output.values need text of 1-128 characters and a version up to 99999".into());
        }
        if self.require.models.len() > 32 || self.require.models.is_empty() {
            return Err("require.models needs 1-32 models".into());
        }
        if self.require.models.iter().any(|(n, d)| !text_ok(n, 64) || d.as_deref().is_some_and(|d| !text_ok(d, 128))) {
            return Err("require.models names are 1-64 characters and digests 1-128".into());
        }
        // The app's own template expander (tokens, `#` run, collisions, no escape) on a stand-in root.
        let (kinds, v) = (self.output_config.kinds(), &self.output.values);
        let probe = OutputLocation { root: PathBuf::from(if cfg!(windows) { r"C:\shared" } else { "/shared" }), folder_template: self.output.folder_template.clone(), file_template: self.output.file_template.clone() };
        let values = ClipOutputValues { project: v.project.clone(), name: v.name.clone(), resolution: v.resolution.clone(), date: v.date.clone(), version: v.version };
        for (k, ext) in &kinds {
            resolve_path(&probe, &values, *k, kinds.len(), 1, ext).map_err(|e| format!("output templates: {e:?}"))?;
        }
        // The server measures canonical JSON with non-ASCII escaped (\uXXXX): count it that way.
        let size = ascii_len(&serde_json::to_string(self).map_err(|e| e.to_string())?);
        if size > MAX_SPEC_BYTES {
            return Err(format!("job spec is {size} bytes; the limit is {MAX_SPEC_BYTES}"));
        }
        Ok(())
    }

    fn check_workflow(&self) -> Result<(), String> {
        if let Some(r) = &self.source {
            SharedRef::decode(r).map_err(|x| format!("source: {x}"))?;
        }
        if let Some(h) = &self.hint {
            h.validate()?;
            let st = self.hint_stages.as_ref().ok_or("hint settings need hint_stages")?;
            if st.len() > 64 || st.iter().any(|k| !text_ok(k, 32)) {
                return Err("hint_stages is invalid".into());
            }
        } else if self.hint_stages.is_some() {
            return Err("hint_stages without hint settings".into());
        }
        if let Some(e) = &self.export {
            SharedRef::decode(&e.root).map_err(|x| format!("export.root: {x}"))?;
            let pe = PipelineExport { fps: e.fps, allow_partial: e.allow_partial, root: None, outputs: e.outputs.clone() };
            pe.validate_videos()?;
            let videos: Vec<&str> = pe.videos().into_iter().map(|v| v.0).collect();
            if videos.is_empty() || videos.len() != e.files.len() || videos.iter().any(|v| !e.files.contains_key(*v)) {
                return Err("export needs exactly one file name per video delivery".into());
            }
            let kinds: Vec<String> = self.output_config.kinds().iter().map(|k| k.0.token().to_lowercase()).collect();
            if e.outputs.keys().any(|k| !kinds.contains(k)) {
                return Err("export delivers an output that is not enabled".into());
            }
            for f in e.files.values() {
                SharedRef::new("x", &[f.as_str()]).map_err(|x| format!("export file name: {x}"))?;
            }
        }
        Ok(())
    }

    /// Resolve and check everything on THIS installation. `job_id` is the receiver's attempt id. Err = every problem
    /// found (bounded), each naming what and why, so the submitter can act on it before anything starts.
    pub fn preflight(&self, roots: &[SharedRoot], job_id: &str) -> Result<Preflight, Vec<String>> {
        self.check_shape().map_err(|e| vec![e])?;
        sp::validate_roots(roots).map_err(|e| vec![format!("this PC's path mapping is not usable: {e}")])?;
        let mut problems: Vec<String> = vec![];
        let mut res = |what: &str, r: &str| -> Option<PathBuf> {
            match SharedRef::decode(r).and_then(|x| sp::resolve(roots, &x)) {
                Ok(p) => Some(p),
                Err(e) => {
                    push(&mut problems, format!("{what}: {e}"));
                    None
                }
            }
        };
        let clip_root = res("clip folder", &self.clip_root);
        let out_root = res("output root", &self.output.root);
        let manifest_dir = res("result folder", &self.manifest_dir);
        drop(res);
        let (Some(clip_root), Some(out_root), Some(manifest_dir)) = (clip_root, out_root, manifest_dir) else {
            return Err(problems);
        };

        // The clip, scanned the way this app scans it: self-contained frames only.
        let scan = match self.scan_clip(roots, &clip_root) {
            Ok(x) => x,
            Err(e) => {
                push(&mut problems, e);
                return Err(problems);
            }
        };
        let stems = &scan.stems;
        let range_stems = match self.check_range(stems) {
            Ok(s) => s,
            Err(e) => {
                push(&mut problems, e);
                return Err(problems);
            }
        };
        // Confinement: each path against the mapped location its own reference names.
        let name = |r: &str| SharedRef::decode(r).map(|x| x.root().to_owned()).unwrap_or_default();
        let (clip_loc, out_loc, mdir_loc) = (name(&self.clip_root), name(&self.output.root), name(&self.manifest_dir));
        confine(&mut problems, roots, &clip_loc, &clip_root);
        let src_loc = self.source.as_deref().map(name).unwrap_or_else(|| clip_loc.clone());
        confine(&mut problems, roots, &src_loc, &scan.source_dir);
        if clip_root.join(crate::hints::HINT_DIR).exists() {
            confine(&mut problems, roots, &clip_loc, &clip_root.join(crate::hints::HINT_DIR));
        }
        let is_link = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
        for f in &scan.files[self.range.in_point..=self.range.out_point] {
            if is_link(f) {
                confine(&mut problems, roots, &src_loc, f); // a symlinked frame must still resolve inside the location
            }
        }
        // Hint stage: rebuilt on THIS clip with the app's own planner; it must be the same stages the sender reviewed.
        let mut ids = {
            let mut n = 0;
            let base = job_id.to_owned();
            move |tag: &str| {
                n += 1;
                format!("{base}-{tag}{n}")
            }
        };
        let steps: Vec<TaskJob> = match &self.hint {
            Some(h) => {
                let r = (self.range.in_point, self.range.out_point);
                match crate::pipeline::hint_steps(&scan.clip, &scan.project_root, h, &self.params.hint_mode, r, self.params.input_is_linear, || ids("h")) {
                    Ok(plan) => plan.steps,
                    Err(e) => {
                        push(&mut problems, format!("hint stage cannot be built on this PC: {e}"));
                        vec![]
                    }
                }
            }
            None => vec![],
        };
        if let Some(want) = &self.hint_stages {
            let got: Vec<&str> = steps.iter().map(|t| t.kind.wire()).collect();
            if got.iter().map(|g| g.to_string()).ne(want.iter().cloned()) && problems.is_empty() {
                push(&mut problems, format!("the hint stages differ on this PC: the job reviewed {want:?}, this PC's clip needs {got:?} (guide/hint files differ from the sender's)"));
            }
        }
        for t in &steps {
            if let Some(m) = t.require.models.keys().find(|m| !self.require.models.contains_key(*m)) {
                push(&mut problems, format!("hint stage needs model {m}, which the job did not require"));
            }
            if let Err(e) = t.validate() {
                push(&mut problems, format!("hint stage {}: {e}", t.kind.label()));
            }
        }
        if !steps.is_empty() {
            // Hint steps publish into the clip and stage next to it: both must be writable, inside their locations.
            for dir in [clip_root.clone(), steps[0].staging_dir.clone().and_then(|d| d.parent().map(Path::to_path_buf)).unwrap_or_else(|| scan.project_root.clone())] {
                if let Some(near) = dir.ancestors().find(|a| a.is_dir()) {
                    if let Err(e) = sp::confined(roots, &clip_loc, near) {
                        push(&mut problems, e);
                    } else if let Err(e) = sp::test_access(near).write {
                        push(&mut problems, format!("hint stage cannot write {}: {e}", near.display()));
                    }
                }
            }
        }
        let hint_have: BTreeSet<&String> = scan.hint.keys().collect();
        let needed: Vec<&String> = match self.params.hint_mode.as_str() {
            "full" => range_stems.iter().collect(),
            "keyframes" => self.hint_keyframes_abs.iter().flatten().map(|m| &stems[*m]).collect(),
            _ => vec![],
        };
        for s in needed.iter().filter(|_| steps.is_empty()) {
            if let Some(f) = scan.hint.get(*s).filter(|f| is_link(f)) {
                confine(&mut problems, roots, &clip_loc, f);
            }
        }
        match if steps.is_empty() { self.params.hint_mode.as_str() } else { "none" } {
            "full" => {
                let missing: Vec<&String> = range_stems.iter().filter(|s| !hint_have.contains(*s)).collect();
                if !missing.is_empty() {
                    let eg: Vec<&str> = missing.iter().take(5).map(|s| s.as_str()).collect();
                    push(&mut problems, format!("hint mode full: AlphaHint has no frame for {} of the {} selected frames, e.g. {} (the job carries no hint stage to produce them)", missing.len(), range_stems.len(), eg.join(", ")));
                }
            }
            "keyframes" => {
                let marks: Vec<usize> = self.hint_keyframes_abs.clone().unwrap_or_default();
                let mine: Vec<usize> = scan.keyframes.iter().copied().filter(|k| (self.range.in_point..=self.range.out_point).contains(k)).collect();
                if mine != marks {
                    push(&mut problems, format!("the keyframe marks differ on this PC: the job marks {marks:?}, this PC's clip has {mine:?} inside the range"));
                }
                for m in &marks {
                    let stem = &stems[*m];
                    if !hint_have.contains(stem) {
                        push(&mut problems, format!("hint mode keyframes: AlphaHint has no frame for marked frame {stem}"));
                    }
                }
            }
            _ => {}
        }

        // Outputs: the receiver's own expansion of the templates, which must land where the spec says.
        let loc = OutputLocation { root: out_root, folder_template: self.output.folder_template.clone(), file_template: self.output.file_template.clone() };
        let v = &self.output.values;
        let values = ClipOutputValues { project: v.project.clone(), name: v.name.clone(), resolution: v.resolution.clone(), date: v.date.clone(), version: v.version };
        let kinds = self.output_config.kinds();
        let outputs = match resolve_outputs(&loc, &values, &kinds, &range_stems) {
            Ok(o) => o,
            Err(crate::jobs::ResolveError::Location(e)) => {
                push(&mut problems, format!("output location: {e:?}"));
                return Err(problems);
            }
            Err(crate::jobs::ResolveError::NoFrameNumber(s)) => {
                push(&mut problems, format!("frame name {s:?} has no trailing number for the #### file template"));
                return Err(problems);
            }
        };
        if common_dir(&outputs).as_deref() != Some(manifest_dir.as_path()) {
            push(&mut problems, format!("result folder {} does not match where the output templates write ({:?})", manifest_dir.display(), common_dir(&outputs)));
        }
        // New-only: nothing exists where this job writes, and no other writer holds the folder.
        for name in [MANIFEST_NAME, LOCK_NAME] {
            if std::fs::symlink_metadata(manifest_dir.join(name)).is_ok() {
                push(&mut problems, format!("{} already exists in {}: network jobs are new-only (no Resume/Replace yet)", name, manifest_dir.display()));
            }
        }
        for (k, v) in &outputs {
            for (i, p) in v.iter().enumerate() {
                if std::fs::symlink_metadata(p).is_ok() {
                    push(&mut problems, format!("output {k} frame {} already exists: {}", i + 1, p.display()));
                }
            }
        }
        // Write access, proven by an owned probe on each distinct nearest existing output folder.
        let folders: BTreeSet<PathBuf> = outputs.values().flatten().filter_map(|p| p.ancestors().skip(1).find(|a| a.is_dir()).map(Path::to_path_buf)).collect();
        let mut probes: Vec<(PathBuf, sp::Access)> = vec![];
        let out_root = &loc.root;
        for (l, p) in [(&out_loc, out_root), (&mdir_loc, &manifest_dir)] {
            if let Err(e) = sp::confined(roots, l, p) {
                push(&mut problems, e);
            }
        }
        for f in folders {
            if let Err(e) = sp::confined(roots, &out_loc, &f) {
                push(&mut problems, e);
                continue; // never probe (write) outside the location
            }
            let a = sp::test_access(&f);
            if let Err(e) = &a.write {
                push(&mut problems, format!("cannot write outputs here: {e}"));
            }
            probes.push((f, a));
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let job = KeyJob {
            job_id: job_id.into(),
            clip_id: self.clip_id.clone(),
            clip_root,
            range: self.range.clone(),
            params: self.params.to_value(),
            output_config: self.output_config.to_value(),
            hint_keyframes_abs: self.hint_keyframes_abs.clone(),
            outputs,
            manifest_dir,
            mode: JobMode::New,
            publish: "final".into(),
            path_map: vec![],
            require: self.require.clone(),
            source_map: scan.source_map.clone(),
            enforce_fixed_window: true, // a pinned window that cannot fit fails with its reason, never a silent downgrade
        };
        job.validate().map_err(|e| vec![format!("the job is not valid on this PC: {e}")])?;
        job.check_disk().map_err(|e| vec![e.to_string()])?;
        let mut steps = steps;
        if let Some((rec, loc)) = scan.source_map.first() {
            for t in &mut steps {
                if let Some(o) = t.params.as_object_mut() {
                    o.insert("source_map".into(), serde_json::json!([[rec, loc]]));
                }
            }
        }
        let mut stages: Vec<Stage> = steps.into_iter().map(Stage::Task).collect();
        stages.push(Stage::Key(job.clone()));
        if let Some(e) = &self.export {
            stages.extend(self.deliver_stages(roots, &scan.clip, &job, e, &mut ids, &mut probes)?.into_iter().map(Stage::Task));
        }
        Ok(Preflight { job, stages, first_frame: scan.files[self.range.in_point].clone(), probes })
    }

    /// The frozen Deliver stage on this PC: the sender's destination folder through THIS PC's mapping, never-overwrite
    /// and write checks, then the app's own export builder.
    fn deliver_stages(&self, roots: &[SharedRoot], clip: &Clip, key: &KeyJob, e: &SpecExport, ids: &mut impl FnMut(&str) -> String, probes: &mut Vec<(PathBuf, sp::Access)>) -> Result<Vec<TaskJob>, Vec<String>> {
        let dir = SharedRef::decode(&e.root).and_then(|x| sp::resolve(roots, &x).map(|p| (x.root().to_owned(), p))).map_err(|x| vec![format!("delivery folder: {x}")])?;
        let (loc, dir) = dir;
        let mut problems = vec![];
        let pe = PipelineExport { fps: e.fps, allow_partial: e.allow_partial, root: Some(dir.clone()), outputs: e.outputs.clone() };
        let dests: Vec<(String, PathBuf)> = e.files.iter().map(|(k, f)| (k.clone(), dir.join(f))).collect();
        for (k, d) in &dests {
            if std::fs::symlink_metadata(d).is_ok() {
                problems.push(format!("delivery {k} already exists: {} (network deliveries never overwrite)", d.display()));
            }
        }
        if let Some(near) = dir.ancestors().find(|a| a.is_dir()) {
            match sp::confined(roots, &loc, near) {
                Err(x) => problems.push(x),
                Ok(()) => {
                    let a = sp::test_access(near);
                    if let Err(x) = &a.write {
                        problems.push(format!("cannot write deliveries here: {x}"));
                    }
                    probes.push((near.to_path_buf(), a));
                }
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let products: Vec<String> = key.outputs.keys().cloned().collect();
        crate::pipeline::export_jobs(clip, &key.clip_id, (self.range.in_point, self.range.out_point), &pe, &key.manifest_dir, &products, &dests, || ids("x")).map_err(|x| vec![format!("deliver stage: {x}")])
    }

    /// Scan the clip this PC has at `clip_root`: `<project>/clips/<folder>` with its own `Frames`/`Input` sequence.
    /// Returns the source stems in order and the stems that have an AlphaHint frame.
    fn scan_clip(&self, roots: &[SharedRoot], clip_root: &Path) -> Result<Scan, String> {
        let (Some(clips), Some(folder)) = (clip_root.parent(), clip_root.file_name()) else {
            return Err(format!("{} is not a clip folder", clip_root.display()));
        };
        let project_root = match (clips.file_name().and_then(|n| n.to_str()), clips.parent()) {
            (Some("clips"), Some(p)) => p,
            _ => return Err(format!("{} is not inside a <project>/clips folder: network jobs need a project clip", clip_root.display())),
        };
        let project = load_project(project_root, &LoadOptions::default()).map_err(|e| format!("project on this PC: {e}"))?;
        let folder = folder.to_string_lossy();
        let clip = project
            .clips
            .iter()
            .find(|c| c.folder == folder)
            .ok_or_else(|| format!("this PC's project has no clip folder named {folder:?} at {}", clip_root.display()))?;
        let mut clip = clip.clone();
        let mut source_map = vec![];
        let source_dir = match &self.source {
            None => {
                if let Some(e) = &clip.error {
                    return Err(format!("clip on this PC is not usable: {e}"));
                }
                match source_kind(&clip, clip_root)? {
                    SourceKind::Internal(d) => d,
                    SourceKind::External(d) => return Err(format!("this PC's clip reads an external folder ({}) but the job carries no source reference", d.display())),
                }
            }
            Some(r) => {
                // The sender's clip reads an external sequence. This PC finds its own copy through ITS mapping; the
                // recorded (sender) path in clip.json is only the string the worker must replace, never opened.
                let local = SharedRef::decode(r).and_then(|x| sp::resolve(roots, &x)).map_err(|e| format!("source folder: {e}"))?;
                if !local.is_dir() {
                    return Err(format!("source folder {} does not exist on this PC", local.display()));
                }
                if matches!(clip.source, Source::Sequence { external: false, .. }) {
                    return Err("this PC's clip has its own Frames/Input but the job reads an external source: the two clips differ".into());
                }
                let meta = std::fs::read(&clip.meta_path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
                let src = meta.as_ref().and_then(|m| m.get("source"));
                let recorded = src
                    .filter(|s| s.get("type").and_then(Value::as_str) == Some("sequence"))
                    .and_then(|s| s.get("original_path"))
                    .and_then(Value::as_str)
                    .filter(|p| !p.is_empty())
                    .ok_or("this PC's clip.json does not reference an external image sequence")?
                    .to_owned();
                source_map.push((recorded, local.display().to_string()));
                clip.source = Source::Sequence { dir: local.clone(), external: true };
                clip.input_files = list_images(&local);
                clip.error = None;
                local
            }
        };
        Ok(Scan {
            stems: clip.input_files.iter().map(|f| f.0.clone()).collect(),
            files: clip.input_files.iter().map(|f| f.1.clone()).collect(),
            source_dir,
            hint: list_images(&clip_root.join(crate::hints::HINT_DIR)).into_iter().collect(),
            keyframes: clip.keyframes.clone(),
            clip,
            project_root: project_root.to_path_buf(),
            source_map,
        })
    }

    /// The range must name this PC's own frames: same stems at the same positions, never shifted silently.
    fn check_range(&self, stems: &[String]) -> Result<Vec<String>, String> {
        let r = &self.range;
        match r.validate(stems) {
            Ok(()) => {}
            Err(RangeError::SourceChanged) => {
                let here = |i: usize| stems.get(i).map_or("(no such frame)", String::as_str);
                return Err(format!(
                    "range does not match this PC's frames: frame {} is {} here but the job says {:?}; frame {} is {} here but the job says {:?} (a different or reordered source?)",
                    r.in_point, here(r.in_point), r.in_stem, r.out_point, here(r.out_point), r.out_stem
                ));
            }
            Err(e) => return Err(format!("range does not fit this PC's {} frames: {e:?}", stems.len())),
        }
        let range_stems = stems[r.in_point..=r.out_point].to_vec();
        if let Some(bad) = range_stems.iter().find(|s| stem_frame_number(s).is_none()) {
            return Err(format!("frame name {bad:?} has no trailing number for the #### file template"));
        }
        Ok(range_stems)
    }
}

impl Envelope {
    pub fn check(&self) -> Result<(), String> {
        if !submission_id_ok(&self.submission_id) {
            return Err("submission_id must be 16-64 characters of A-Z a-z 0-9 _ -".into());
        }
        if self.target.as_deref().is_some_and(|t| !submission_id_ok(t)) {
            return Err("target must be an installation id".into());
        }
        self.spec.check_shape()
    }
}

#[cfg(test)]
mod tests {
    use super::ascii_len;

    #[test]
    fn size_is_counted_the_way_python_escapes_non_ascii() {
        assert_eq!(ascii_len("abc"), 3);
        assert_eq!(ascii_len("é"), 6, "one \\uXXXX");
        assert_eq!(ascii_len("😀"), 12, "a surrogate pair is two escapes");
        assert_eq!(ascii_len("a😀é"), 1 + 12 + 6);
    }
}
