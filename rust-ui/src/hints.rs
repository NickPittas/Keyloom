//! Hint generation model: route/config and SAM/paint annotations (persisted per clip in `ck_native`), and the
//! frozen `TaskJob`s built from them. Annotations are stored in source-pixel coordinates keyed by source frame
//! stem, so zoom/pan never matter and a changed source is rejected instead of silently re-pointed.
use crate::contracts::RangeIdentity;
use crate::jobs::Require;
use crate::persist::update_native;
use crate::project::Clip;
use crate::tasks::{TaskJob, TaskKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const SAM_MODELS: [(&str, &str); 3] = [
    ("small", "Small"),
    ("base_plus", "Base+"),
    ("large", "Large"),
];
pub const BIREFNET_VARIANTS: [&str; 16] = [
    "Matting",
    "Matting HR",
    "Matting Lite",
    "Matting Dynamic",
    "General",
    "General HR",
    "General Lite",
    "General Lite 2K",
    "General Dynamic",
    "General 512",
    "Portrait",
    "DIS",
    "HRSOD",
    "COD",
    "DIS TR_TEs",
    "General Legacy",
];
pub const STAGING: &str = ".ck_staging";

/// App-owned staging root for a clip: OUTSIDE the clip folder (the worker refuses staging inside it).
pub fn staging_root(project_root: &Path, clip_root: &Path, folder: &str) -> PathBuf {
    if clip_root != project_root {
        project_root.join(STAGING).join(folder)
    } else {
        // v1 project: the clip is the project root, so stage next to it.
        let name = project_root
            .file_name()
            .map_or("project".into(), |n| n.to_string_lossy().into_owned());
        project_root
            .parent()
            .unwrap_or(project_root)
            .join(format!(".{name}{STAGING}"))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Import,
    Gvm,
    Birefnet,
    Chroma,
    Matanyone2,
    Videomama,
}

impl Route {
    pub const ALL: [Route; 6] = [
        Route::Import,
        Route::Gvm,
        Route::Birefnet,
        Route::Chroma,
        Route::Matanyone2,
        Route::Videomama,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Route::Import => "Import alpha",
            Route::Gvm => "GVM",
            Route::Birefnet => "BiRefNet",
            Route::Chroma => "Chroma key",
            Route::Matanyone2 => "MatAnyone2",
            Route::Videomama => "VideoMaMa",
        }
    }
    pub fn task(self) -> Option<TaskKind> {
        match self {
            Route::Import => None,
            Route::Gvm => Some(TaskKind::Gvm),
            Route::Birefnet => Some(TaskKind::Birefnet),
            Route::Chroma => Some(TaskKind::Chroma),
            Route::Matanyone2 => Some(TaskKind::Matanyone2),
            Route::Videomama => Some(TaskKind::Videomama),
        }
    }
    /// Needs a dense/initial mask hint (`VideoMamaMaskHint`) produced by SAM tracking, paint or import.
    pub fn needs_mask(self) -> bool {
        matches!(self, Route::Matanyone2 | Route::Videomama)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Chroma {
    pub screen_color: Option<[u8; 3]>,
    pub screen_type: String,
    pub strength: f64,
    pub clip_black: f64,
    pub clip_white: f64,
    pub shrink_grow: i64,
    pub edge_blur: i64,
}
impl Default for Chroma {
    fn default() -> Self {
        Self {
            screen_color: None,
            screen_type: "green".into(),
            strength: 1.0,
            clip_black: 0.0,
            clip_white: 1.0,
            shrink_grow: 0,
            edge_blur: 0,
        }
    }
}

/// Generation settings, separate from the consumption mode (`hint_mode` none/full/keyframes in the Key
/// workflow). `bypass` keeps every other field, so switching back restores the prior route.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HintCfg {
    pub route: Route,
    pub bypass: bool,
    pub birefnet_variant: String,
    pub sam_model: String,
    pub chroma: Chroma,
    pub videomama_chunk: u32,
}
impl Default for HintCfg {
    fn default() -> Self {
        Self {
            route: Route::Gvm,
            bypass: false,
            birefnet_variant: "Matting".into(),
            sam_model: "base_plus".into(),
            chroma: Chroma::default(),
            videomama_chunk: 16,
        }
    }
}

impl HintCfg {
    pub fn load(clip: &Clip) -> Self {
        clip.native
            .get("hint")
            .and_then(|v| serde_json::from_value(v.clone()).ok())
            .unwrap_or_default()
    }
    pub fn save(&self, clip: &Clip) -> Result<(), String> {
        let v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        update_native(&clip.meta_path, |n| {
            // Merge key by key: a newer version's extra keys inside `hint` survive.
            let e = n.entry("hint").or_insert_with(|| json!({}));
            if !e.is_object() {
                *e = json!({});
            }
            for (k, x) in v.as_object().unwrap() {
                e[k] = x.clone();
            }
        })
    }
    pub fn validate(&self) -> Result<(), String> {
        if !BIREFNET_VARIANTS.contains(&self.birefnet_variant.as_str()) {
            return Err(format!(
                "unknown BiRefNet variant {}",
                self.birefnet_variant
            ));
        }
        if !SAM_MODELS.iter().any(|m| m.0 == self.sam_model) {
            return Err(format!("unknown SAM model {}", self.sam_model));
        }
        if !(1..=64).contains(&self.videomama_chunk) {
            return Err("VideoMaMa chunk size must be 1-64".into());
        }
        let c = &self.chroma;
        if !["green", "blue"].contains(&c.screen_type.as_str()) || c.clip_black >= c.clip_white {
            return Err("chroma settings are invalid (screen type, black < white)".into());
        }
        Ok(())
    }
    pub fn params(&self, kind: TaskKind) -> Value {
        match kind {
            TaskKind::Birefnet => json!({"variant": self.birefnet_variant}),
            TaskKind::Chroma => serde_json::to_value(&self.chroma).unwrap(),
            TaskKind::Videomama => json!({"chunk_size": self.videomama_chunk}),
            _ => json!({}),
        }
    }
}

// ---- annotations ---------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Pt {
    pub x: f64,
    pub y: f64,
    /// 1 foreground, 0 background.
    pub label: u8,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    pub fg: bool,
    pub radius: f64,
    pub pts: Vec<[f64; 2]>,
}
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FrameAnn {
    pub points: Vec<Pt>,
    /// At most one box per frame (SAM contract).
    pub boxes: Vec<[f64; 4]>,
    pub strokes: Vec<Stroke>,
}
impl FrameAnn {
    pub fn is_empty(&self) -> bool {
        self.points.is_empty() && self.boxes.is_empty() && self.strokes.is_empty()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Annotations {
    /// Source frame size the coordinates belong to.
    pub size: Option<(u32, u32)>,
    pub frames: BTreeMap<String, FrameAnn>,
}

impl Annotations {
    /// Stored annotations for the clip; geometry that no longer matches the source is refused, not scaled.
    pub fn load(clip: &Clip) -> Result<Self, String> {
        let a: Annotations = match clip.native.get("annotations") {
            Some(v) => serde_json::from_value(v.clone())
                .map_err(|e| format!("annotations unreadable: {e}"))?,
            None => return Ok(Self::default()),
        };
        if let (Some(have), Some(now)) = (a.size, clip.sample_size) {
            if have != now {
                return Err(format!(
                    "annotations were made for {}×{} frames, the source is now {}×{}; clear them first",
                    have.0, have.1, now.0, now.1
                ));
            }
        }
        Ok(a)
    }
    pub fn save(&self, clip: &Clip) -> Result<(), String> {
        let mut a = self.clone();
        a.size = a.size.or(clip.sample_size);
        a.frames.retain(|_, f| !f.is_empty());
        let v = serde_json::to_value(&a).map_err(|e| e.to_string())?;
        update_native(&clip.meta_path, |n| {
            n.insert("annotations".into(), v);
        })
    }
    pub fn frame_mut(&mut self, stem: &str) -> &mut FrameAnn {
        self.frames.entry(stem.to_owned()).or_default()
    }
    /// Stems that carry prompts and lie inside the inclusive range.
    pub fn stems_in(&self, stems: &[String], range: (usize, usize)) -> Vec<String> {
        stems[range.0..=range.1]
            .iter()
            .filter(|s| self.frames.get(*s).is_some_and(|f| !f.is_empty()))
            .cloned()
            .collect()
    }
}

// ---- chroma holdout (static, per clip) --------------------------------------------------------------

pub const HOLDOUT_FILE: &str = "holdout_strokes.json";

/// Chroma holdout: strokes the chroma generator must treat as certain foreground (`fg`) or certain background
/// (`bg`). Static for the whole clip, stored exactly where the backend reads it (`<clip>/holdout_strokes.json`,
/// `{"0": [{points, brush_type, radius}]}`), and distinct from SAM prompts: background SAM paint is never a
/// holdout.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Holdout {
    pub strokes: Vec<Stroke>,
}

impl Holdout {
    /// Missing file = none. An unreadable file or one made for other frame geometry is an error, never replaced.
    pub fn load(clip: &Clip) -> Result<Self, String> {
        let path = clip.root.join(HOLDOUT_FILE);
        let v: Value = match std::fs::read(&path) {
            Ok(b) => serde_json::from_slice(&b)
                .map_err(|e| format!("{} is unreadable: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        let list = v
            .get("0")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut strokes = vec![];
        for s in list {
            let pts: Vec<[f64; 2]> = s
                .get("points")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|p| Some([p.get(0)?.as_f64()?, p.get(1)?.as_f64()?]))
                        .collect()
                })
                .unwrap_or_default();
            if pts.is_empty() {
                continue;
            }
            strokes.push(Stroke {
                fg: s.get("brush_type").and_then(Value::as_str).unwrap_or("fg") != "bg",
                radius: s.get("radius").and_then(Value::as_f64).unwrap_or(15.0),
                pts,
            });
        }
        let stored = clip
            .native
            .get("holdout_size")
            .and_then(|v| Some((v.get(0)?.as_u64()? as u32, v.get(1)?.as_u64()? as u32)));
        if let (Some(have), Some(now), false) = (stored, clip.sample_size, strokes.is_empty()) {
            if have != now {
                return Err(format!(
                    "the holdout was drawn for {}x{} frames, the source is now {}x{}; clear it first",
                    have.0, have.1, now.0, now.1
                ));
            }
        }
        Ok(Self { strokes })
    }

    /// Atomic; other frame keys of the file are kept; no strokes removes the `"0"` key (and the file if empty).
    pub fn save(&self, clip: &Clip) -> Result<(), String> {
        let path = clip.root.join(HOLDOUT_FILE);
        let mut obj = match std::fs::read(&path) {
            Ok(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Object(m)) => m,
                _ => {
                    return Err(format!(
                        "{} is not a holdout file; left untouched",
                        path.display()
                    ))
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        if self.strokes.is_empty() {
            obj.remove("0");
        } else {
            obj.insert(
                "0".into(),
                Value::Array(
                    self.strokes
                        .iter()
                        .map(|s| json!({"points": s.pts, "brush_type": if s.fg {"fg"} else {"bg"}, "radius": s.radius}))
                        .collect(),
                ),
            );
        }
        if obj.is_empty() {
            return match std::fs::remove_file(&path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("{}: {e}", path.display()))
                }
                _ => Ok(()),
            };
        }
        let tmp = clip
            .root
            .join(format!(".holdout_strokes.json.tmp{}", std::process::id()));
        std::fs::write(
            &tmp,
            serde_json::to_vec_pretty(&Value::Object(obj)).unwrap(),
        )
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })?;
        let size = clip.sample_size;
        update_native(&clip.meta_path, |n| match size {
            Some((w, h)) => {
                n.insert("holdout_size".into(), json!([w, h]));
            }
            None => {
                n.remove("holdout_size");
            }
        })
    }

    /// Remove strokes the eraser touches.
    pub fn erase(&mut self, x: f64, y: f64, r: f64) -> bool {
        let n = self.strokes.len();
        self.strokes.retain(|s| {
            !s.pts
                .iter()
                .any(|q| (q[0] - x).hypot(q[1] - y) <= r + s.radius)
        });
        self.strokes.len() != n
    }
}

/// Rasterise foreground strokes of one frame to an 8-bit mask (255 = prompt). Background strokes become
/// negative points instead (see `frame_prompts`).
pub fn rasterize(strokes: &[Stroke], w: u32, h: u32) -> Vec<u8> {
    let mut m = vec![0u8; w as usize * h as usize];
    let mut dot = |cx: f64, cy: f64, r: f64| {
        let (x0, x1) = (
            ((cx - r).floor().max(0.0)) as u32,
            ((cx + r).ceil().min(w as f64 - 1.0)) as u32,
        );
        let (y0, y1) = (
            ((cy - r).floor().max(0.0)) as u32,
            ((cy + r).ceil().min(h as f64 - 1.0)) as u32,
        );
        for y in y0..=y1.min(h.saturating_sub(1)) {
            for x in x0..=x1.min(w.saturating_sub(1)) {
                let (dx, dy) = (x as f64 + 0.5 - cx, y as f64 + 0.5 - cy);
                if dx * dx + dy * dy <= r * r {
                    m[y as usize * w as usize + x as usize] = 255;
                }
            }
        }
    };
    for s in strokes.iter().filter(|s| s.fg) {
        let r = s.radius.max(0.5);
        for (i, p) in s.pts.iter().enumerate() {
            dot(p[0], p[1], r);
            if let Some(q) = i.checked_sub(1).map(|j| s.pts[j]) {
                let d = ((p[0] - q[0]).powi(2) + (p[1] - q[1]).powi(2)).sqrt();
                let n = (d / (r * 0.5).max(0.5)).ceil() as usize;
                for k in 1..n {
                    let t = k as f64 / n as f64;
                    dot(q[0] + (p[0] - q[0]) * t, q[1] + (p[1] - q[1]) * t, r);
                }
            }
        }
    }
    m
}

pub fn write_mask_png(path: &Path, mask: &[u8], w: u32, h: u32) -> Result<(), String> {
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Grayscale);
    e.set_depth(png::BitDepth::Eight);
    let mut wr = e.write_header().map_err(|e| e.to_string())?;
    wr.write_image_data(mask).map_err(|e| e.to_string())
}

/// SAM prompt object for one frame; paint strokes are rasterised to `<staging>/prompt_<stem>.png` (relative
/// name in the job). Background strokes contribute up to 8 negative points.
pub fn frame_prompts(
    f: &FrameAnn,
    stem: &str,
    staging: &Path,
    size: (u32, u32),
) -> Result<Value, String> {
    let mut points: Vec<Value> = f
        .points
        .iter()
        .map(|p| json!({"x": p.x, "y": p.y, "label": p.label}))
        .collect();
    let bg: Vec<[f64; 2]> = f
        .strokes
        .iter()
        .filter(|s| !s.fg)
        .flat_map(|s| s.pts.clone())
        .collect();
    let step = (bg.len() / 8).max(1);
    points.extend(
        bg.iter()
            .step_by(step)
            .take(8)
            .map(|p| json!({"x": p[0], "y": p[1], "label": 0})),
    );
    let mut o = json!({"points": points, "boxes": f.boxes.iter().take(1).collect::<Vec<_>>()});
    if f.strokes.iter().any(|s| s.fg) {
        let inputs = staging.join("inputs");
        std::fs::create_dir_all(&inputs).map_err(|e| format!("{}: {e}", inputs.display()))?;
        let name = format!("prompt_{stem}.png");
        write_mask_png(
            &inputs.join(&name),
            &rasterize(&f.strokes, size.0, size.1),
            size.0,
            size.1,
        )?;
        o["mask_png"] = json!(name);
    }
    Ok(o)
}

// ---- job builders --------------------------------------------------------------------------------

pub struct Ctx<'a> {
    pub clip: &'a Clip,
    pub range: (usize, usize),
    pub job_id: String,
    /// `staging_root(..)` of this clip; each job stages in `<root>/<job_id>`.
    pub staging: PathBuf,
}

impl Ctx<'_> {
    fn stems(&self) -> Vec<String> {
        self.clip.input_files.iter().map(|f| f.0.clone()).collect()
    }
    fn base(&self, kind: TaskKind, params: Value) -> Result<TaskJob, String> {
        let stems = self.stems();
        let range = RangeIdentity::from_inclusive(&stems, self.range.0, self.range.1)
            .map_err(|e| format!("range: {e:?}"))?;
        let root = self.clip.root.clone();
        let publish = root.join(if kind == TaskKind::SamTrack {
            "VideoMamaMaskHint"
        } else {
            "AlphaHint"
        });
        Ok(TaskJob {
            job_id: self.job_id.clone(),
            kind,
            clip_id: self
                .clip
                .clip_id
                .clone()
                .unwrap_or_else(|| self.clip.folder.clone()),
            clip_root: root,
            range: Some(range),
            params,
            staging_dir: kind
                .needs_staging()
                .then(|| self.staging.join(&self.job_id)),
            publish_dir: kind.writes_hint().then_some(publish),
            require: Require {
                protocol: 1,
                models: BTreeMap::new(),
            },
        })
    }

    /// Whole-movie jobs: no range, no staging, no publish folder.
    fn whole(&self, kind: TaskKind, params: Value) -> TaskJob {
        TaskJob {
            job_id: self.job_id.clone(),
            kind,
            clip_id: self
                .clip
                .clip_id
                .clone()
                .unwrap_or_else(|| self.clip.folder.clone()),
            clip_root: self.clip.root.clone(),
            range: None,
            params,
            staging_dir: None,
            publish_dir: None,
            require: Require {
                protocol: 1,
                models: BTreeMap::new(),
            },
        }
    }

    /// `Source/` movie -> validated `Frames/`.
    pub fn prepare_source(&self, movie: &Path) -> TaskJob {
        self.whole(TaskKind::PrepareSource, json!({"source_path": movie}))
    }

    /// Companion alpha/mask movie -> the clip's aligned hint folder (`mask` = VideoMaMa mask hint).
    pub fn import_hint(
        &self,
        hint: &Path,
        source: &Path,
        mask: bool,
        alpha_channel: bool,
    ) -> TaskJob {
        self.whole(
            TaskKind::ImportHint,
            json!({"hint_path": hint, "source_path": source, "hint_kind": if mask {"mask"} else {"alpha"},
                   "channel": if alpha_channel {"alpha"} else {"gray"}}),
        )
    }

    /// Route job. MatAnyone2/VideoMaMa need the mask hint to exist and cover the range (checked by the
    /// worker too); `mask_stem` defaults to In.
    pub fn route(&self, cfg: &HintCfg) -> Result<TaskJob, String> {
        cfg.validate()?;
        let kind = cfg
            .route
            .task()
            .ok_or("Import alpha has nothing to generate: use Import alpha on the Hint page")?;
        let mut p = cfg.params(kind);
        if kind == TaskKind::Matanyone2 {
            p["mask_stem"] = json!(self.stems()[self.range.0]);
        }
        self.base(kind, p)
    }

    pub fn sam_preview(
        &self,
        cfg: &HintCfg,
        ann: &Annotations,
        stem: &str,
        linear: bool,
    ) -> Result<TaskJob, String> {
        let f = ann
            .frames
            .get(stem)
            .filter(|f| !f.is_empty())
            .ok_or("no prompt on this frame")?;
        let size = self.clip.sample_size.ok_or("frame size unknown")?;
        let mut j = self.base(TaskKind::SamPreview, json!({}))?;
        let prompts = frame_prompts(f, stem, j.staging_dir.as_ref().unwrap(), size)?;
        j.params = json!({"frame_stem": stem, "tracker_model": cfg.sam_model, "prompts": prompts, "input_is_linear": linear});
        Ok(j)
    }

    pub fn sam_track(
        &self,
        cfg: &HintCfg,
        ann: &Annotations,
        linear: bool,
    ) -> Result<TaskJob, String> {
        let stems = self.stems();
        let with = ann.stems_in(&stems, self.range);
        if with.is_empty() {
            return Err(
                "no prompts inside In/Out: add points, a box or paint on a frame in the range"
                    .into(),
            );
        }
        let size = self.clip.sample_size.ok_or("frame size unknown")?;
        let mut j = self.base(TaskKind::SamTrack, json!({}))?;
        let mut frames = serde_json::Map::new();
        for s in with {
            frames.insert(
                s.clone(),
                frame_prompts(&ann.frames[&s], &s, j.staging_dir.as_ref().unwrap(), size)?,
            );
        }
        j.params =
            json!({"tracker_model": cfg.sam_model, "frames": frames, "input_is_linear": linear});
        Ok(j)
    }

    pub fn export(&self, p: &ExportCfg) -> Result<TaskJob, String> {
        self.base(TaskKind::ExportVideo, p.to_params())
    }
}

// ---- hint lock and interrupted-publication recovery -------------------------------------------------

pub const HINT_LOCK: &str = ".corridorkey_hint.lock";
pub const HINT_DIRS: [&str; 2] = ["AlphaHint", "VideoMamaMaskHint"];

/// `<clip>/.corridorkey_hint.lock` as written by the worker. The pid/host are informational only: a lock on a
/// shared volume can belong to another machine and pids are reused, so nothing is ever inferred from them.
#[derive(Debug, Clone, PartialEq)]
pub struct HintLock {
    pub path: PathBuf,
    pub job_id: Option<String>,
    pub pid: Option<u64>,
    pub started: Option<f64>,
    pub host: Option<String>,
}

/// `Ok(None)` = no lock. A symlink / non-regular file is an error and is never followed or touched.
pub fn read_hint_lock(clip_root: &Path) -> Result<Option<HintLock>, String> {
    let path = clip_root.join(HINT_LOCK);
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
    Ok(Some(HintLock {
        job_id: v.get("job_id").and_then(Value::as_str).map(str::to_owned),
        pid: v.get("pid").and_then(Value::as_u64),
        started: v.get("started").and_then(Value::as_f64),
        host: v.get("host").and_then(Value::as_str).map(str::to_owned),
        path,
    }))
}

/// Leftovers of an interrupted publish (`<dir>.prev-*` with no `<dir>`), read-only. The worker restores them
/// under its hint lock (at the next hint task or lock clear); the UI never renames anything itself.
pub fn interrupted_publish(clip_root: &Path) -> Vec<String> {
    let mut out = vec![];
    for d in HINT_DIRS {
        if std::fs::symlink_metadata(clip_root.join(d)).is_ok() {
            continue;
        }
        let has_prev = std::fs::read_dir(clip_root)
            .into_iter()
            .flatten()
            .flatten()
            .any(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(&format!("{d}.prev-"))
                    && e.file_type().is_ok_and(|t| t.is_dir())
            });
        if has_prev {
            out.push(d.to_string());
        }
    }
    out
}

/// Does `dir` hold an image for every stem in the inclusive range?
pub fn covers(dir: &Path, stems: &[String], range: (usize, usize)) -> bool {
    let have: std::collections::BTreeSet<String> = crate::project::list_images(dir)
        .into_iter()
        .map(|h| h.0)
        .collect();
    stems[range.0..=range.1].iter().all(|s| have.contains(s))
}

/// What a Key run on `range` needs before it: the hint-generation steps (possibly none), in order.
/// `Ok(vec![])` = bypassed, not needed, or the existing hint already covers the range. Mask routes first track
/// SAM prompts into `VideoMamaMaskHint` unless a mask already covers the range.
pub fn plan_steps(
    clip: &Clip,
    cfg: &HintCfg,
    hint_mode: &str,
    range: (usize, usize),
    linear: bool,
    force: bool,
    staging: &Path,
    mut id: impl FnMut() -> String,
) -> Result<Vec<TaskJob>, String> {
    // Only a route the user chose (saved on the Hint page) is ever generated implicitly; otherwise the Key
    // gate reports "Needs hint" and nothing runs behind the user's back.
    if cfg.bypass || hint_mode == "none" || (!force && clip.native.get("hint").is_none()) {
        return Ok(vec![]);
    }
    let stems: Vec<String> = clip.input_files.iter().map(|f| f.0.clone()).collect();
    if stems.is_empty() {
        return Err("no source frames".into());
    }
    let hint_ok = covers(&clip.root.join("AlphaHint"), &stems, range);
    if cfg.route == Route::Import {
        return if hint_ok {
            Ok(vec![])
        } else {
            Err("Import alpha: the imported AlphaHint does not cover In/Out".into())
        };
    }
    if hint_ok && !force {
        return Ok(vec![]);
    }
    let mut steps = vec![];
    if cfg.route.needs_mask() && !covers(&clip.root.join("VideoMamaMaskHint"), &stems, range) {
        let ann = Annotations::load(clip)?;
        let cx = Ctx {
            clip,
            range,
            job_id: id(),
            staging: staging.to_path_buf(),
        };
        steps.push(cx.sam_track(cfg, &ann, linear).map_err(|e| {
            format!(
                "{} needs a mask over In/Out: {e} (or import a mask video)",
                cfg.route.label()
            )
        })?);
    }
    steps.push(
        Ctx {
            clip,
            range,
            job_id: id(),
            staging: staging.to_path_buf(),
        }
        .route(cfg)?,
    );
    Ok(steps)
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExportCfg {
    pub product: String,
    pub manifest_dir: String,
    pub dest: String,
    pub container: String,
    pub fps: f64,
    pub allow_partial: bool,
}
pub const CONTAINERS: [(&str, &str, &str); 3] = [
    ("mp4", "MP4 (H.264, no alpha)", "mp4"),
    ("mov_prores4444", "MOV ProRes 4444 (alpha)", "mov"),
    ("webm_alpha", "WebM VP9 (alpha)", "webm"),
];
impl ExportCfg {
    pub fn to_params(&self) -> Value {
        json!({"product": self.product, "manifest_dir": self.manifest_dir, "dest": self.dest,
               "container": self.container, "fps": self.fps, "allow_partial": self.allow_partial, "audio": "none"})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rasterize_paints_discs_along_segments_and_ignores_background() {
        let s = [
            Stroke {
                fg: true,
                radius: 2.0,
                pts: vec![[2.0, 2.0], [12.0, 2.0]],
            },
            Stroke {
                fg: false,
                radius: 5.0,
                pts: vec![[2.0, 8.0]],
            },
        ];
        let m = rasterize(&s, 16, 10);
        assert_eq!(m[2 * 16 + 7], 255); // between the two stroke points
        assert_eq!(m[8 * 16 + 2], 0); // background strokes never paint
        assert_eq!(m[9 * 16 + 15], 0);
    }

    #[test]
    fn hint_lock_is_read_only_here_and_never_followed_through_a_symlink() {
        let d = std::env::temp_dir().join(format!("ck-lock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        assert_eq!(read_hint_lock(&d).unwrap(), None);
        std::fs::write(
            d.join(HINT_LOCK),
            r#"{"job_id":"j1","pid":42,"started":1.5,"host":"h"}"#,
        )
        .unwrap();
        let l = read_hint_lock(&d).unwrap().unwrap();
        assert_eq!(
            (l.job_id.as_deref(), l.pid, l.host.as_deref()),
            (Some("j1"), Some(42), Some("h"))
        );
        std::fs::remove_file(d.join(HINT_LOCK)).unwrap();
        std::fs::write(d.join("target"), "x").unwrap();
        std::os::unix::fs::symlink(d.join("target"), d.join(HINT_LOCK)).unwrap();
        assert!(read_hint_lock(&d).is_err());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn interrupted_publication_is_reported_not_repaired() {
        let d = std::env::temp_dir().join(format!("ck-prev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("AlphaHint.prev-j1")).unwrap();
        assert_eq!(interrupted_publish(&d), vec!["AlphaHint".to_string()]);
        std::fs::create_dir_all(d.join("AlphaHint")).unwrap();
        assert!(interrupted_publish(&d).is_empty());
        assert!(
            d.join("AlphaHint.prev-j1").is_dir(),
            "nothing renamed or removed"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn holdout_file_matches_the_backend_format_and_keeps_other_frames() {
        let d = std::env::temp_dir().join(format!("ck-hold-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let root = d.join("clips/c1");
        std::fs::create_dir_all(root.join("Frames")).unwrap();
        std::fs::write(root.join("Frames/f1.png"), b"x").unwrap();
        std::fs::write(root.join("clip.json"), "{}").unwrap();
        std::fs::write(d.join("project.json"), r#"{"version":2}"#).unwrap();
        let mut c = crate::project::load_project(&d, &crate::project::LoadOptions::default())
            .unwrap()
            .clips
            .remove(0);
        let proj = d.clone();
        let d = root;
        c.sample_size = Some((8, 8));
        assert!(Holdout::load(&c).unwrap().strokes.is_empty());
        let h = Holdout {
            strokes: vec![
                Stroke {
                    fg: true,
                    radius: 4.0,
                    pts: vec![[1.0, 2.0], [3.0, 4.0]],
                },
                Stroke {
                    fg: false,
                    radius: 2.5,
                    pts: vec![[5.0, 5.0]],
                },
            ],
        };
        std::fs::write(
            d.join(HOLDOUT_FILE),
            r#"{"7":[{"points":[[1,1]],"brush_type":"fg","radius":3}]}"#,
        )
        .unwrap();
        h.save(&c).unwrap();
        let raw: Value =
            serde_json::from_slice(&std::fs::read(d.join(HOLDOUT_FILE)).unwrap()).unwrap();
        assert_eq!(raw["0"][0]["brush_type"], "fg");
        assert_eq!(raw["0"][1]["brush_type"], "bg");
        assert_eq!(raw["0"][0]["points"], json!([[1.0, 2.0], [3.0, 4.0]]));
        assert_eq!(raw["7"][0]["radius"], 3, "another frame's strokes survive");
        assert_eq!(Holdout::load(&c).unwrap(), h);
        // Different geometry is refused (the size was recorded with the save), an unreadable file is never
        // overwritten.
        let mut c = crate::project::load_project(&proj, &crate::project::LoadOptions::default())
            .unwrap()
            .clips
            .remove(0);
        c.sample_size = Some((16, 16));
        assert!(Holdout::load(&c).unwrap_err().contains("clear it first"));
        Holdout::default().save(&c).unwrap();
        let raw: Value =
            serde_json::from_slice(&std::fs::read(d.join(HOLDOUT_FILE)).unwrap()).unwrap();
        assert!(raw.get("0").is_none() && raw.get("7").is_some());
        std::fs::write(d.join(HOLDOUT_FILE), "garbage").unwrap();
        assert!(h.save(&c).is_err());
        assert_eq!(std::fs::read(d.join(HOLDOUT_FILE)).unwrap(), b"garbage");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn cfg_validates_and_round_trips_params() {
        let mut c = HintCfg::default();
        assert!(c.validate().is_ok());
        assert_eq!(c.params(TaskKind::Birefnet), json!({"variant": "Matting"}));
        c.videomama_chunk = 0;
        assert!(c.validate().is_err());
        c = HintCfg {
            birefnet_variant: "nope".into(),
            ..HintCfg::default()
        };
        assert!(c.validate().is_err());
    }

    #[test]
    fn prompts_cap_one_box_and_write_relative_mask() {
        let d = std::env::temp_dir().join(format!("ck-prompt-{}", std::process::id()));
        let f = FrameAnn {
            points: vec![Pt {
                x: 1.0,
                y: 2.0,
                label: 1,
            }],
            boxes: vec![[0.0, 0.0, 5.0, 5.0], [1.0, 1.0, 2.0, 2.0]],
            strokes: vec![Stroke {
                fg: true,
                radius: 1.5,
                pts: vec![[3.0, 3.0]],
            }],
        };
        let o = frame_prompts(&f, "f001", &d, (8, 8)).unwrap();
        assert_eq!(o["boxes"].as_array().unwrap().len(), 1);
        assert_eq!(o["mask_png"], "prompt_f001.png");
        assert!(d.join("inputs/prompt_f001.png").is_file());
        let _ = std::fs::remove_dir_all(d);
    }
}
