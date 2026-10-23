//! Read-only loader for existing CorridorKey projects (`native-input-map.md`).
//! Nothing in this module writes to disk; unknown JSON keys are ignored and corrupt metadata becomes
//! `notes` text on the owning clip/project, never a panic.
use crate::contracts::RangeIdentity;
use crate::natural_sort::natural_cmp;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

pub const IMAGE_EXTS: [&str; 8] = ["png", "jpg", "jpeg", "exr", "tif", "tiff", "bmp", "dpx"];
pub const VIDEO_EXTS: [&str; 8] = ["mp4", "mov", "avi", "mkv", "mxf", "webm", "m4v", "gif"];

pub fn ext_of(p: &Path) -> String {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash, PartialOrd, Ord)]
pub enum View {
    Input,
    /// Final hint (`AlphaHint`).
    Hint,
    /// Guide mask (`VideoMamaMaskHint`).
    Guide,
    Fg,
    Matte,
    Comp,
    Processed,
}

impl View {
    pub const ALL: [View; 7] = [
        View::Input,
        View::Hint,
        View::Guide,
        View::Fg,
        View::Matte,
        View::Comp,
        View::Processed,
    ];
    pub fn label(self) -> &'static str {
        match self {
            View::Input => "Input",
            View::Hint => "Hint",
            View::Guide => "Guide",
            View::Fg => "FG",
            View::Matte => "Matte",
            View::Comp => "Comp",
            View::Processed => "Processed",
        }
    }
    /// Manifest key / output sub-folder, for the four outputs.
    fn output(self) -> Option<(&'static str, &'static str)> {
        match self {
            View::Fg => Some(("fg", "FG")),
            View::Matte => Some(("matte", "Matte")),
            View::Comp => Some(("comp", "Comp")),
            View::Processed => Some(("processed", "Processed")),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ClipState {
    Extracting,
    Raw,
    Masked,
    Ready,
    Complete,
    /// Python ERROR: e.g. the external source folder is gone. The clip stays listed with the message.
    Error,
}

impl ClipState {
    pub fn label(self) -> &'static str {
        match self {
            ClipState::Extracting => "Extracting (video source, no frames)",
            ClipState::Raw => "Raw",
            ClipState::Masked => "Masked",
            ClipState::Ready => "Ready",
            ClipState::Complete => "Complete",
            ClipState::Error => "Error",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    /// Directory of numbered frames; `external` = referenced by `clip.json`, not inside the project.
    Sequence {
        dir: PathBuf,
        external: bool,
    },
    Video {
        path: PathBuf,
    },
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColorOrigin {
    SessionOverride,
    DefaultLinearExr,
    VideoMetadataLinear,
    DefaultNotLinear,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    pub engine: String,
    pub version: u64,
    pub complete: bool,
    pub partial_range: bool,
    pub enabled: Vec<String>,
    pub formats: BTreeMap<String, String>,
    pub source_stems: Option<Vec<String>>,
    pub processed_exr_convention: Option<String>,
    /// v3: owner clip, recorded range and explicit per-output files (relative to the manifest folder, in
    /// `source_stems` order). With `files` present, stems never come from a directory listing.
    pub clip_id: Option<String>,
    pub range: Option<RangeIdentity>,
    pub files: Option<BTreeMap<String, Vec<String>>>,
    pub exr_compression: Option<String>,
    /// The manifest as read (resume checks compare against it).
    pub raw: Value,
}

/// A v3 manifest with its explicit output files and source stems.
type V3Files<'a> = (
    &'a Manifest,
    &'a BTreeMap<String, Vec<String>>,
    &'a Vec<String>,
);

pub const MANIFEST_NAME: &str = ".corridorkey_manifest.json";

#[derive(Clone, Debug)]
pub struct Clip {
    /// Folder basename: the stable identity.
    pub folder: String,
    /// Effective name (display name when set, like `ClipEntry.name`): used for output joins and the session colour map.
    pub name: String,
    pub error: Option<String>,
    pub display_name: String,
    pub root: PathBuf,
    pub source: Source,
    pub input_files: Vec<(String, PathBuf)>,
    /// Zero-based inclusive indices into `input_files`, as stored in `clip.json`.
    pub in_out: Option<(usize, usize)>,
    pub state: ClipState,
    pub input_is_linear: bool,
    pub color_origin: ColorOrigin,
    pub fps: f64,
    pub fps_assumed: bool,
    pub output_dir: Option<PathBuf>,
    pub output_dir_note: String,
    /// Folder the manifest was read from (v3 results may live outside `output_dir`).
    pub manifest_dir: Option<PathBuf>,
    /// File holding this clip's metadata (`clip.json`, or `project.json` for a v1 project).
    pub meta_path: PathBuf,
    /// Stable random id written on first Key.
    pub clip_id: Option<String>,
    /// `clip.json["workflow"]`: `{version, params, output_config}` as stored.
    pub workflow: Option<Value>,
    /// `clip.json["output_values"]`: `{resolution, date, version}`.
    pub output_values: Option<Value>,
    pub alpha_frames: usize,
    pub mask_frames: usize,
    pub manifest: Option<Manifest>,
    pub keyframes: Vec<usize>,
    pub notes: Vec<String>,
    pub sample_size: Option<(u32, u32)>,
    /// `clip.json["ck_native"]` as stored: native-UI state (hint route, annotations, range stems...).
    /// Written key by key (`persist::update_native`), so unknown keys survive.
    pub native: Value,
}

#[derive(Clone, Debug)]
pub struct Project {
    pub root: PathBuf,
    pub display_name: String,
    pub is_v2: bool,
    pub clips: Vec<Clip>,
    pub session_params: Option<Value>,
    pub session_output_config: Option<Value>,
    /// `project.json["output_location"]`; absent = legacy layout.
    pub output_location: Option<Value>,
    pub notes: Vec<String>,
    ctx: LoadCtx,
}

/// What `load_clip` needs besides the clip folder, kept so one clip can be re-read in place.
#[derive(Clone, Debug, Default)]
struct LoadCtx {
    project_output: Option<PathBuf>,
    color_map: BTreeMap<String, bool>,
    opts: LoadOptions,
}

#[derive(Default, Clone, Debug)]
pub struct LoadOptions {
    pub global_output_dir: Option<PathBuf>,
}

fn read_json(p: &Path) -> Result<Option<Value>, String> {
    match fs::read(p) {
        Ok(b) => serde_json::from_slice(&b)
            .map(Some)
            .map_err(|e| format!("{}: {e}", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", p.display())),
    }
}

/// Image files in `dir` as (stem, path), natural sorted. Non-image sidecars are ignored.
pub fn list_images(dir: &Path) -> Vec<(String, PathBuf)> {
    let Ok(rd) = fs::read_dir(dir) else {
        return vec![];
    };
    let mut v: Vec<(String, PathBuf)> = rd
        .flatten()
        .filter(|e| e.path().is_file())
        .filter(|e| IMAGE_EXTS.contains(&ext_of(&e.path()).as_str()))
        .map(|e| {
            let p = e.path();
            (
                p.file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
                p,
            )
        })
        .collect();
    v.sort_by(|a, b| natural_cmp(&a.0, &b.0).then_with(|| a.1.cmp(&b.1)));
    v
}

fn count_images(dir: &Path) -> usize {
    list_images(dir).len()
}

fn first_video(dir: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && VIDEO_EXTS.contains(&ext_of(p).as_str()))
        .collect();
    v.sort();
    v.into_iter().next()
}

fn root_input_video(cr: &Path) -> Option<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(cr)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && VIDEO_EXTS.contains(&ext_of(p).as_str())
                && p.file_stem()
                    .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("input"))
        })
        .collect();
    v.sort();
    v.into_iter().next()
}

/// Absolute and lexically clean (`.`/`..` folded, no symlink resolution): job paths must not contain `..`.
pub fn clean_abs(p: &Path) -> PathBuf {
    use std::path::Component;
    let a = std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf());
    let mut out = PathBuf::new();
    for c in a.components() {
        match c {
            Component::ParentDir => {
                if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_)) | None
                ) {
                    out.pop();
                }
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

pub fn load_project(root: &Path, opts: &LoadOptions) -> Result<Project, String> {
    // Every path derived from the project (clip roots, staging, outputs) must be valid in a worker with a
    // different working directory: absolute once, here. Symlinks are not resolved.
    let abs_root = clean_abs(root);
    let root = abs_root.as_path();
    if !root.is_dir() {
        return Err(format!("Not a folder: {}", root.display()));
    }
    let mut notes = Vec::new();
    let pj = match read_json(&root.join("project.json")) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("project.json unreadable: {e}"));
            None
        }
    };
    let clips_dir = root.join("clips");
    let is_v2 = clips_dir.is_dir();
    let display_name = pj
        .as_ref()
        .and_then(|j| j.get("display_name"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            root.file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned()
        });
    let removed: BTreeSet<String> = pj
        .as_ref()
        .and_then(|j| j.get("removed_clips"))
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let project_output = pj
        .as_ref()
        .and_then(|j| j.get("output_dir"))
        .and_then(Value::as_str)
        .map(PathBuf::from);

    // Session file lives in the project root (observed) or in the clips dir (native-input-map).
    let mut session = None;
    for p in [
        root.join(".corridorkey_session.json"),
        clips_dir.join(".corridorkey_session.json"),
    ] {
        match read_json(&p) {
            Ok(Some(v)) => {
                session = Some(v);
                break;
            }
            Ok(None) => {}
            Err(e) => notes.push(format!("session unreadable: {e}")),
        }
    }
    let color_map: BTreeMap<String, bool> = session
        .as_ref()
        .and_then(|s| s.get("clip_input_is_linear"))
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_bool().map(|b| (k.clone(), b)))
                .collect()
        })
        .unwrap_or_default();

    let clip_roots: Vec<PathBuf> = if is_v2 {
        let mut names: Vec<String> = fs::read_dir(&clips_dir)
            .map_err(|e| e.to_string())?
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| !n.starts_with('.') && !n.starts_with('_') && !removed.contains(n))
            .collect();
        names.sort();
        names.into_iter().map(|n| clips_dir.join(n)).collect()
    } else {
        vec![root.to_path_buf()]
    };

    let project_folder = root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let clips = clip_roots
        .iter()
        .filter_map(|cr| {
            match load_clip(
                cr,
                is_v2,
                root,
                &project_folder,
                project_output.as_deref(),
                &color_map,
                opts,
            ) {
                Ok(c) => Some(c),
                Err(why) => {
                    // Python raises ClipScanError and does not list the clip; say so instead of hiding it.
                    notes.push(format!("Clip not listed (same as the Qt app): {why}"));
                    None
                }
            }
        })
        .collect();
    Ok(Project {
        root: root.to_path_buf(),
        display_name,
        is_v2,
        clips,
        session_params: session.as_ref().and_then(|s| s.get("params")).cloned(),
        session_output_config: session
            .as_ref()
            .and_then(|s| s.get("output_config"))
            .cloned(),
        output_location: pj.as_ref().and_then(|j| j.get("output_location")).cloned(),
        notes,
        ctx: LoadCtx {
            project_output,
            color_map,
            opts: opts.clone(),
        },
    })
}

impl Project {
    /// Re-read one clip from disk and replace it in place (list order and every other clip untouched).
    /// Used after a job writes results. `Err` leaves the old clip.
    pub fn reload_clip(&mut self, folder: &str) -> Result<(), String> {
        let i = self
            .clips
            .iter()
            .position(|c| c.folder == folder)
            .ok_or("clip not in the project")?;
        let root = self.clips[i].root.clone();
        let pf = self
            .root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.clips[i] = load_clip(
            &root,
            self.is_v2,
            &self.root,
            &pf,
            self.ctx.project_output.as_deref(),
            &self.ctx.color_map,
            &self.ctx.opts,
        )?;
        Ok(())
    }
}

/// Atomic, unknown-field-preserving update of a JSON object file (`clip.json`, `project.json`). A missing
/// file starts from `{}`; a file that is not a JSON object is an error and is never replaced.
pub fn update_json(
    path: &Path,
    f: impl FnOnce(&mut serde_json::Map<String, Value>),
) -> Result<(), String> {
    let mut v = match read_json(path)? {
        Some(Value::Object(m)) => m,
        Some(_) => {
            return Err(format!(
                "{} is not a JSON object; left untouched",
                path.display()
            ))
        }
        None => serde_json::Map::new(),
    };
    f(&mut v);
    let dir = path.parent().ok_or("no parent folder")?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.tmp{}", std::process::id()));
    let write = || -> std::io::Result<()> {
        use std::io::Write;
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&Value::Object(v.clone()))?)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("cannot write {}: {e}", path.display())
    })
}

/// A new, empty v2 project: the NEW folder `parent/name` (never an existing one, so nothing is overwritten).
/// `project.json` is renamed into place last, so it is written whole or not at all. Returns the project folder.
pub fn create_empty_project(parent: &Path, name: &str) -> Result<PathBuf, String> {
    let name = name.trim();
    // A single plain component: no separators, no `.`/`..`, no Windows drive/prefix forms like `D:proj`.
    if name.is_empty() || name.contains(['/', '\\', '\0']) || Path::new(name).file_name() != Some(std::ffi::OsStr::new(name)) {
        return Err("Enter a project name that is a single folder name".into());
    }
    let dir = clean_abs(parent).join(name);
    // Exclusive mkdir: an existing folder or file, even one made a moment ago, is refused untouched.
    fs::create_dir(&dir).map_err(|e| match e.kind() {
        std::io::ErrorKind::AlreadyExists => format!("{} already exists; choose another name", dir.display()),
        _ => format!("cannot create {}: {e}", dir.display()),
    })?;
    let meta = serde_json::json!({"version": 2, "display_name": name, "clips": []});
    let (tmp, clips) = (dir.join(format!(".project.json.tmp{}", std::process::id())), dir.join("clips"));
    let (mut tmp_made, mut clips_made) = (false, false);
    let made = (|| -> std::io::Result<()> {
        use std::io::Write;
        let mut f = fs::File::create_new(&tmp)?;
        tmp_made = true;
        f.write_all(&serde_json::to_vec_pretty(&meta)?)?;
        f.sync_all()?;
        fs::create_dir(&clips)?;
        clips_made = true;
        fs::rename(&tmp, dir.join("project.json")) // last step: nothing of ours is left to undo once it lands
    })();
    made.map(|()| dir.clone()).map_err(|e| {
        // Undo only what this call made, non-recursively: anything foreign keeps the folder in place.
        if tmp_made {
            let _ = fs::remove_file(&tmp);
        }
        if clips_made {
            let _ = fs::remove_dir(&clips);
        }
        let _ = fs::remove_dir(&dir);
        format!("cannot create the project in {}: {e}", dir.display())
    })
}

fn load_clip(
    cr: &Path,
    is_v2: bool,
    project_root: &Path,
    project_folder: &str,
    project_output: Option<&Path>,
    color_map: &BTreeMap<String, bool>,
    opts: &LoadOptions,
) -> Result<Clip, String> {
    let folder = cr
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let mut name = folder.clone();
    let mut notes = Vec::new();
    // v1: project.json doubles as clip metadata.
    let meta_path = if is_v2 {
        cr.join("clip.json")
    } else {
        project_root.join("project.json")
    };
    let meta = match read_json(&meta_path) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("clip metadata unreadable: {e}"));
            None
        }
    };
    let get = |k: &str| meta.as_ref().and_then(|m| m.get(k));
    let display_name = get("display_name")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(&name)
        .to_owned();

    // Source, same order as `ClipEntry.find_assets`: Frames → Input → Source/ video (or clip.json original
    // file) → external sequence (ERROR if gone) → root Input.* video. Anything else is not listed.
    let src_meta = get("source");
    let src_type = src_meta.and_then(|s| s.get("type")).and_then(Value::as_str);
    let orig = src_meta
        .and_then(|s| s.get("original_path"))
        .and_then(Value::as_str)
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let nonempty = |d: &Path| fs::read_dir(d).is_ok_and(|mut r| r.next().is_some());
    let mut error = None;
    let source = if nonempty(&cr.join("Frames")) {
        Source::Sequence {
            dir: cr.join("Frames"),
            external: false,
        }
    } else if nonempty(&cr.join("Input")) {
        Source::Sequence {
            dir: cr.join("Input"),
            external: false,
        }
    } else if cr.join("Source").is_dir() {
        match first_video(&cr.join("Source")).or_else(|| orig.clone().filter(|p| p.is_file())) {
            Some(path) => Source::Video { path },
            // A movie clip whose movie vanished stays listed (error state) so it can be relinked; the Qt app
            // does not list it. Anything else without a source is still not listed.
            None if src_meta.and_then(|m| m.get("filename")).is_some() => {
                error = Some(format!(
                    "Source movie missing: nothing in {} (relink the movie to extract frames again)",
                    cr.join("Source").display()
                ));
                Source::Missing
            }
            None => return Err(format!("Clip '{folder}': 'Source' dir has no video.")),
        }
    } else if let (Some("sequence"), Some(p)) = (src_type, &orig) {
        if p.is_dir() {
            Source::Sequence {
                dir: p.clone(),
                external: true,
            }
        } else {
            error = Some(format!("Source folder missing: {}", p.display()));
            Source::Missing
        }
    } else if let Some(v) = root_input_video(cr) {
        Source::Video { path: v }
    } else if cr.join("Input").is_dir() {
        return Err(format!(
            "Clip '{folder}': Input dir is empty — no image files."
        ));
    } else {
        return Err(format!("Clip '{folder}': no Input found."));
    };
    // Python applies the display name last, and not at all for the early ERROR return.
    if error.is_none() && display_name != folder {
        name = display_name.clone();
    }
    let input_files = match &source {
        Source::Sequence { dir, .. } => list_images(dir),
        _ => vec![],
    };

    let in_out = get("in_out_range").and_then(|r| {
        let a = r.get("in_point")?.as_u64()? as usize;
        let b = r.get("out_point")?.as_u64()? as usize;
        Some((a, b))
    });
    let mut in_out = in_out;
    if let Some((a, b)) = in_out {
        if !input_files.is_empty() && (a > b || b >= input_files.len()) {
            notes.push(format!(
                "Stored In/Out {a}–{b} is outside the {} source frames",
                input_files.len()
            ));
        }
        // The native UI also records the boundary stems: a changed source never shifts the range silently.
        let stems = get("ck_native").and_then(|n| n.get("range_stems"));
        let want = |k: &str| stems.and_then(|s| s.get(k)).and_then(Value::as_str);
        if let (Some(i), Some(o)) = (want("in"), want("out")) {
            let at = |n: usize| input_files.get(n).map(|f| f.0.as_str());
            if !input_files.is_empty() && (at(a) != Some(i) || at(b) != Some(o)) {
                notes.push(
                    "Source changed: the stored In/Out no longer matches its frames; re-set In/Out"
                        .into(),
                );
                in_out = None;
            }
        }
    }

    // Video metadata / fps.
    let vm = match read_json(&cr.join(".video_metadata.json")) {
        Ok(v) => v,
        Err(e) => {
            notes.push(format!("video metadata unreadable: {e}"));
            None
        }
    };
    let (fps, fps_assumed) = match vm
        .as_ref()
        .and_then(|v| v.get("fps"))
        .and_then(Value::as_f64)
        .filter(|f| *f > 0.0)
    {
        Some(f) => (f, false),
        None => (24.0, true),
    };
    let vm_linear = vm
        .as_ref()
        .and_then(|v| v.get("source_probe"))
        .and_then(|p| p.get("color_transfer"))
        .and_then(Value::as_str)
        == Some("linear");
    let first_is_exr = input_files.first().is_some_and(|(_, p)| ext_of(p) == "exr");
    let (input_is_linear, color_origin) = if let Some(b) = color_map.get(&name) {
        (*b, ColorOrigin::SessionOverride)
    } else if vm_linear {
        (true, ColorOrigin::VideoMetadataLinear)
    } else if first_is_exr && vm.is_none() {
        (true, ColorOrigin::DefaultLinearExr)
    } else {
        (false, ColorOrigin::DefaultNotLinear)
    };

    // Output directory precedence.
    let clip_out = get("output_dir")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let (output_dir, output_dir_note) = if let Some(p) = clip_out {
        (Some(p), "clip.json output_dir".to_string())
    } else if let Some(p) = project_output.filter(|_| is_v2) {
        (
            Some(p.join(&name)),
            "project.json output_dir / clip".to_string(),
        )
    } else if let Some(g) = &opts.global_output_dir {
        // v1: the project folder is the clip folder; Python joins <global>/<project basename>/<name> either way.
        (
            Some(g.join(project_folder).join(&name)),
            "--global-output-dir".to_string(),
        )
    } else if is_v2 || !cr.as_os_str().is_empty() {
        // The Qt global default (QSettings) is machine-local and not read here: honest, not silent.
        (
            Some(cr.join("Output")),
            "default <clip>/Output (Qt global output preference not read)".to_string(),
        )
    } else {
        (None, String::new())
    };

    let alpha_frames = count_images(&cr.join("AlphaHint"));
    let mask_frames = count_images(&cr.join("VideoMamaMaskHint"));

    // The folder of the last Key job (`key_result`) wins over the legacy output folder.
    let result_dir = get("key_result")
        .and_then(|r| r.get("manifest_dir"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let mut manifest = None;
    let mut manifest_dir = None;
    for d in result_dir.iter().chain(output_dir.iter()) {
        match read_json(&d.join(MANIFEST_NAME)) {
            Ok(Some(m)) => {
                manifest = Some(parse_manifest(&m));
                manifest_dir = Some(d.clone());
                break;
            }
            Ok(None) => {}
            Err(e) => notes.push(format!("manifest unreadable: {e}")),
        }
    }

    let keyframes = match read_json(&cr.join(".corridorkey_hint_keyframes.json")) {
        Ok(Some(v)) => match &source {
            Source::Sequence { .. } => {
                let stems: Vec<String> = input_files.iter().map(|f| f.0.clone()).collect();
                match parse_keyframes(&v, &stems) {
                    Ok(k) => k,
                    Err(e) => {
                        notes.push(format!("Hint keyframes ignored: {e}"));
                        vec![]
                    }
                }
            }
            _ => {
                notes.push("Hint keyframe sidecar present but the source frames are unavailable to validate it against".into());
                vec![]
            }
        },
        Ok(None) => vec![],
        Err(e) => {
            notes.push(format!("keyframe sidecar unreadable: {e}"));
            vec![]
        }
    };

    let mut clip = Clip {
        folder,
        name,
        error,
        display_name,
        root: cr.to_path_buf(),
        source,
        input_files,
        in_out,
        state: ClipState::Raw,
        input_is_linear,
        color_origin,
        fps,
        fps_assumed,
        output_dir,
        output_dir_note,
        manifest_dir,
        meta_path: meta_path.clone(),
        clip_id: get("clip_id").and_then(Value::as_str).map(str::to_owned),
        workflow: get("workflow").cloned(),
        output_values: get("output_values").cloned(),
        alpha_frames,
        mask_frames,
        manifest,
        keyframes,
        notes,
        native: get("ck_native").cloned().unwrap_or(Value::Null),
        sample_size: vm.as_ref().and_then(|v| {
            Some((
                v.get("width")?.as_u64()? as u32,
                v.get("height")?.as_u64()? as u32,
            ))
        }),
    };
    clip.state = resolve_state(&clip);
    Ok(clip)
}

fn parse_manifest(m: &Value) -> Manifest {
    let strs = |k: &str| -> Vec<String> {
        m.get(k)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    Manifest {
        version: m.get("version").and_then(Value::as_u64).unwrap_or(0),
        clip_id: m.get("clip_id").and_then(Value::as_str).map(str::to_owned),
        range: m
            .get("range")
            .and_then(|r| serde_json::from_value(r.clone()).ok()),
        files: m
            .get("files")
            .and_then(|f| serde_json::from_value(f.clone()).ok()),
        exr_compression: m
            .get("exr_compression")
            .and_then(Value::as_str)
            .map(str::to_owned),
        raw: m.clone(),
        engine: m
            .get("engine")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
        complete: m.get("complete").and_then(Value::as_bool) == Some(true),
        partial_range: m.get("frame_range").is_some_and(|v| !v.is_null()),
        enabled: strs("enabled_outputs"),
        formats: m
            .get("formats")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_lowercase())))
                    .collect()
            })
            .unwrap_or_default(),
        source_stems: m.get("source_stems").and_then(Value::as_array).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        }),
        processed_exr_convention: m
            .get("alpha_convention")
            .and_then(|a| a.get("processed_exr"))
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

/// Port of `hint_keyframes._parse`: exact schema, version 1, `{index, stem}` int/str records, in range, no
/// duplicates, sorted by index, and every stem equal to the source stem at that index. Never repaired.
pub fn parse_keyframes(v: &Value, stems: &[String]) -> Result<Vec<usize>, String> {
    let keys = |o: &serde_json::Map<String, Value>, k: [&str; 2]| {
        o.len() == 2 && k.iter().all(|k| o.contains_key(*k))
    };
    let int = |v: &Value| v.is_i64() || v.is_u64();
    let o = v
        .as_object()
        .filter(|o| keys(o, ["version", "keyframes"]))
        .ok_or("Invalid keyframe sidecar schema")?;
    if !int(&o["version"]) || o["version"].as_i64() != Some(1) {
        return Err(format!(
            "Unsupported keyframe sidecar version: {}",
            o["version"]
        ));
    }
    let records = o["keyframes"]
        .as_array()
        .ok_or("'keyframes' must be a list")?;
    let (mut out, mut seen_stems): (Vec<usize>, BTreeSet<&str>) = (vec![], BTreeSet::new());
    for r in records {
        let rec = r
            .as_object()
            .filter(|o| keys(o, ["index", "stem"]))
            .ok_or_else(|| format!("Invalid keyframe record: {r}"))?;
        let (idx, stem) = (&rec["index"], rec["stem"].as_str());
        let (true, Some(stem)) = (int(idx), stem) else {
            return Err(format!("Invalid keyframe record types: {r}"));
        };
        if !seen_stems.insert(stem) {
            return Err(format!("Duplicate keyframe stem {stem:?}"));
        }
        let i = idx
            .as_i64()
            .ok_or_else(|| format!("Keyframe index {idx} out of range"))?;
        if i < 0 || i as usize >= stems.len() {
            return Err(format!(
                "Keyframe index {i} out of range 0..{}",
                stems.len() as i64 - 1
            ));
        }
        let i = i as usize;
        if out.contains(&i) {
            return Err(format!("Duplicate keyframe index {i}"));
        }
        if stems[i] != stem {
            return Err(format!(
                "Stale keyframe {i}: sidecar stem {stem:?} != source stem {:?}",
                stems[i]
            ));
        }
        out.push(i);
    }
    if out.windows(2).any(|w| w[0] > w[1]) {
        return Err("Keyframes not sorted by index".into());
    }
    Ok(out)
}

impl Clip {
    /// Directory holding a view's frames, if the view has one.
    pub fn view_dir(&self, view: View) -> Option<PathBuf> {
        match view {
            View::Input => match &self.source {
                Source::Sequence { dir, .. } => Some(dir.clone()),
                _ => None,
            },
            View::Hint => Some(self.root.join("AlphaHint")),
            View::Guide => Some(self.root.join("VideoMamaMaskHint")),
            v => {
                if self.v3_files().is_some() {
                    return self.manifest_dir.clone();
                }
                v.output()
                    .and_then(|(_, sub)| self.output_dir.as_ref().map(|o| o.join(sub)))
            }
        }
    }

    /// The v3 manifest when it carries explicit files + stems (then it, not a folder listing, defines results).
    fn v3_files(&self) -> Option<V3Files<'_>> {
        let m = self
            .manifest
            .as_ref()
            .filter(|m| m.engine == "corridorkey-v2" && m.version >= 3)?;
        Some((m, m.files.as_ref()?, m.source_stems.as_ref()?))
    }

    /// stem → existing file for one output view, straight from the v3 manifest. `None` = not a v3 result
    /// (use the folder listing). Names that would leave the manifest folder are ignored.
    pub fn manifest_view_files(&self, view: View) -> Option<BTreeMap<String, PathBuf>> {
        let (key, _) = view.output()?;
        let (_, files, stems) = self.v3_files()?;
        let dir = self.manifest_dir.as_ref()?;
        let mut out = BTreeMap::new();
        for (stem, rel) in stems.iter().zip(files.get(key).into_iter().flatten()) {
            let rel = Path::new(rel);
            if rel.is_absolute()
                || rel
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                continue;
            }
            let p = dir.join(rel);
            if p.is_file() {
                out.insert(stem.clone(), p);
            }
        }
        Some(out)
    }

    /// Stems whose enabled outputs all exist (manifest-aware, like `completed_stems`).
    pub fn completed_stems(&self) -> BTreeSet<String> {
        let (Some(m), Some(out)) = (&self.manifest, &self.output_dir) else {
            return BTreeSet::new();
        };
        if m.engine != "corridorkey-v2" {
            return BTreeSet::new();
        }
        if self.v3_files().is_some() {
            let mut sets = m.enabled.iter().filter_map(|o| {
                let v = View::ALL
                    .into_iter()
                    .find(|v| v.output().is_some_and(|(k, _)| k == o))?;
                self.manifest_view_files(v)
                    .map(|f| f.into_keys().collect::<BTreeSet<_>>())
            });
            let Some(first) = sets.next() else {
                return BTreeSet::new();
            };
            return sets.fold(first, |a, b| a.intersection(&b).cloned().collect());
        }
        let mut sets: Vec<BTreeSet<String>> = vec![];
        for o in &m.enabled {
            let sub = match o.as_str() {
                "fg" => "FG",
                "matte" => "Matte",
                "comp" => "Comp",
                "processed" => "Processed",
                _ => return BTreeSet::new(),
            };
            let d = out.join(sub);
            let ext = m
                .formats
                .get(o)
                .map(|s| s.trim_start_matches('.').to_lowercase())
                .unwrap_or_default();
            if !d.is_dir() || ext.is_empty() {
                return BTreeSet::new();
            }
            sets.push(
                list_images(&d)
                    .into_iter()
                    .filter(|(_, p)| ext_of(p) == ext)
                    .map(|(s, _)| s)
                    .collect(),
            );
        }
        let Some(first) = sets.first().cloned() else {
            return BTreeSet::new();
        };
        let mut r = sets
            .iter()
            .skip(1)
            .fold(first, |a, b| a.intersection(b).cloned().collect());
        if let Some(ss) = &m.source_stems {
            let ss: BTreeSet<&String> = ss.iter().collect();
            r.retain(|s| ss.contains(s));
        }
        r
    }

    /// "frames 1101–1300" when a v3 result covers only part of the clip.
    pub fn result_range_text(&self) -> Option<String> {
        let (m, _, stems) = self.v3_files()?;
        (stems.len() < self.input_files.len()).then(|| {
            let r = m.range.as_ref();
            format!(
                "frames {}–{} ({} of {})",
                r.map_or("?", |r| r.in_stem.as_str()),
                r.map_or("?", |r| r.out_stem.as_str()),
                stems.len(),
                self.input_files.len()
            )
        })
    }

    pub fn range_len(&self) -> usize {
        match self.in_out {
            Some((a, b)) if b >= a => b - a + 1,
            _ => self.input_files.len(),
        }
    }
}

/// Mirrors `ClipEntry._resolve_state`.
fn resolve_state(c: &Clip) -> ClipState {
    if c.error.is_some() {
        return ClipState::Error;
    }
    let n_in = c.input_files.len();
    if n_in > 0 {
        if let (Some(m), Some((_, _, ss))) = (&c.manifest, c.v3_files()) {
            // v3: complete for its own recorded range, decided by the explicit files, never a count.
            let done = c.completed_stems();
            if m.complete && !ss.is_empty() && ss.iter().all(|s| done.contains(s)) {
                return ClipState::Complete;
            }
        } else if let Some(m) = &c.manifest {
            if m.engine == "corridorkey-v2" && m.complete && !m.partial_range {
                let done = c.completed_stems();
                if !done.is_empty() && done.len() >= n_in {
                    return ClipState::Complete;
                }
            }
        }
    }
    if c.alpha_frames > 0 {
        let required = if n_in > 0 { c.range_len() } else { 0 };
        if n_in == 0 || c.alpha_frames >= required {
            return ClipState::Ready;
        }
    }
    if c.mask_frames > 0 {
        ClipState::Masked
    } else if matches!(c.source, Source::Video { .. }) {
        ClipState::Extracting
    } else {
        ClipState::Raw
    }
}

/// Read-only readiness text for the Board. Only states derivable from disk; never implies a job can run.
pub fn readiness(c: &Clip) -> String {
    if let Some(e) = &c.error {
        return e.clone();
    }
    match c.state {
        ClipState::Complete => match c.result_range_text() {
            Some(r) => format!("Complete for {r}"),
            None => "Complete (existing outputs)".into(),
        },
        ClipState::Ready => "Hint present".into(),
        ClipState::Error => "Error".into(),
        ClipState::Masked => "Mask hint only".into(),
        ClipState::Extracting => {
            "Video source: native movie decode not integrated yet (staged)".into()
        }
        ClipState::Raw => {
            if c.alpha_frames > 0 {
                format!(
                    "Partial hint: {} of {} frames",
                    c.alpha_frames,
                    c.range_len()
                )
            } else if c.input_files.is_empty() {
                "No source frames found".into()
            } else {
                "No hint yet".into()
            }
        }
    }
}
