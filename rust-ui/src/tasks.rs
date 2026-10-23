//! Non-Key worker jobs (`cmd:"task"`): automatic/guided hint generation, SAM preview/tracking and video
//! export. Same frozen-snapshot rules as `KeyJob`: built once, validated, never mutated. Hint kinds write only
//! into `staging_dir`; the worker publishes a validated set into `publish_dir` recoverably.
use crate::contracts::RangeIdentity;
use crate::jobs::{is_abs, Require, PROTOCOL};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Gvm,
    Birefnet,
    Chroma,
    SamPreview,
    SamTrack,
    Matanyone2,
    Videomama,
    /// Promotes the existing guide (`VideoMamaMaskHint`) to the final `AlphaHint` on the CPU: only the feather
    /// cleanup is applied (grow/fill were already applied to the guide). No model, no GPU.
    PrematteHint,
    ExportVideo,
    /// Whole-movie `Source/` -> validated `Frames/` (no range).
    PrepareSource,
    /// Companion alpha/mask movie -> aligned `AlphaHint`/`VideoMamaMaskHint` (no range).
    ImportHint,
}

impl TaskKind {
    pub const ALL: [TaskKind; 11] = [
        TaskKind::Gvm,
        TaskKind::Birefnet,
        TaskKind::Chroma,
        TaskKind::SamPreview,
        TaskKind::SamTrack,
        TaskKind::Matanyone2,
        TaskKind::Videomama,
        TaskKind::PrematteHint,
        TaskKind::ExportVideo,
        TaskKind::PrepareSource,
        TaskKind::ImportHint,
    ];
    pub fn wire(self) -> &'static str {
        match self {
            TaskKind::Gvm => "gvm",
            TaskKind::Birefnet => "birefnet",
            TaskKind::Chroma => "chroma",
            TaskKind::SamPreview => "sam_preview",
            TaskKind::SamTrack => "sam_track",
            TaskKind::Matanyone2 => "matanyone2",
            TaskKind::Videomama => "videomama",
            TaskKind::PrematteHint => "prematte_hint",
            TaskKind::ExportVideo => "export_video",
            TaskKind::PrepareSource => "prepare_source",
            TaskKind::ImportHint => "import_hint",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            TaskKind::Gvm => "GVM hint",
            TaskKind::Birefnet => "BiRefNet hint",
            TaskKind::Chroma => "Chroma-key hint",
            TaskKind::SamPreview => "SAM preview",
            TaskKind::SamTrack => "SAM track",
            TaskKind::Matanyone2 => "MatAnyone2 hint",
            TaskKind::Videomama => "VideoMaMa hint",
            TaskKind::PrematteHint => "Pre-matte hint",
            TaskKind::ExportVideo => "Video export",
            TaskKind::PrepareSource => "Extract frames",
            TaskKind::ImportHint => "Import hint movie",
        }
    }
    /// Publishes a hint set into `publish_dir` (replacing the range's frames).
    pub fn writes_hint(self) -> bool {
        !matches!(
            self,
            TaskKind::SamPreview
                | TaskKind::ExportVideo
                | TaskKind::PrepareSource
                | TaskKind::ImportHint
        )
    }
    /// Changes files inside the clip folder (so Key on that clip must wait for it).
    pub fn writes_clip(self) -> bool {
        self.writes_hint() || matches!(self, TaskKind::PrepareSource | TaskKind::ImportHint)
    }
    pub fn ranged(self) -> bool {
        !matches!(self, TaskKind::PrepareSource | TaskKind::ImportHint)
    }
    pub fn needs_staging(self) -> bool {
        self.writes_hint() || self == TaskKind::SamPreview
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskJob {
    pub job_id: String,
    pub kind: TaskKind,
    pub clip_id: String,
    pub clip_root: PathBuf,
    /// `None` exactly for the whole-movie kinds.
    pub range: Option<RangeIdentity>,
    /// Kind-specific parameters (see the wire contract); opaque to the scheduler.
    pub params: Value,
    /// App-owned, OUTSIDE the clip folder. Present for hint kinds and `sam_preview` only.
    pub staging_dir: Option<PathBuf>,
    /// Present for hint kinds only; exactly `<clip>/AlphaHint` (`VideoMamaMaskHint` for `sam_track`).
    pub publish_dir: Option<PathBuf>,
    pub require: Require,
}

impl TaskJob {
    pub fn validate(&self) -> Result<(), String> {
        let id_ok = !self.job_id.is_empty()
            && self.job_id.len() <= 64
            && self
                .job_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
        if !id_ok {
            return Err("job_id must match [A-Za-z0-9._-]{1,64}".into());
        }
        if self.clip_id.is_empty() {
            return Err("clip_id is empty".into());
        }
        match (&self.range, self.kind.ranged()) {
            (Some(r), true) => {
                if r.in_point > r.out_point || r.in_stem.is_empty() || r.out_stem.is_empty() {
                    return Err("range needs in <= out and non-empty stems".into());
                }
            }
            (None, false) => {}
            _ => {
                return Err(format!(
                    "{}: range presence is wrong for this kind",
                    self.kind.wire()
                ))
            }
        }
        if self.require.protocol != PROTOCOL {
            return Err("require.protocol must be 1".into());
        }
        if !self.params.is_object() {
            return Err("params must be an object".into());
        }
        let clean = |f: &str, p: &PathBuf| -> Result<(), String> {
            let s = p.to_str().ok_or_else(|| format!("{f} is not UTF-8"))?;
            if !is_abs(s)
                || s.chars().any(char::is_control)
                || s.split(['/', '\\']).any(|c| c == "..")
            {
                return Err(format!("{f} must be a clean absolute path: {s}"));
            }
            Ok(())
        };
        clean("clip_root", &self.clip_root)?;
        if self.staging_dir.is_some() != self.kind.needs_staging()
            || self.publish_dir.is_some() != self.kind.writes_hint()
        {
            return Err(format!(
                "{}: staging_dir/publish_dir presence is wrong for this kind",
                self.kind.wire()
            ));
        }
        for (f, p) in [
            ("staging_dir", &self.staging_dir),
            ("publish_dir", &self.publish_dir),
        ] {
            if let Some(p) = p {
                clean(f, p)?;
            }
        }
        if let Some(st) = &self.staging_dir {
            if st.starts_with(&self.clip_root) || self.clip_root.starts_with(st) {
                return Err("staging_dir must be outside the clip folder".into());
            }
        }
        if self.kind == TaskKind::ExportVideo {
            let dest = self
                .params
                .get("dest")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !is_abs(dest) {
                return Err("export_video needs an absolute params.dest".into());
            }
        }
        Ok(())
    }

    /// One writer per target: hint kinds fence their publish folder, previews their staging folder, exports
    /// their destination file. Distinct clips never conflict.
    pub fn fence(&self) -> String {
        let low = |p: &PathBuf| p.to_string_lossy().to_lowercase();
        match self.kind {
            TaskKind::ExportVideo => self
                .params
                .get("dest")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_lowercase(),
            k if k.writes_clip() => low(&self.clip_root),
            _ => self.staging_dir.as_ref().map(low).unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    pub fn sample(kind: TaskKind) -> TaskJob {
        TaskJob {
            job_id: "t1".into(),
            kind,
            clip_id: "c".into(),
            clip_root: "/p/clip".into(),
            range: Some(RangeIdentity {
                in_point: 0,
                out_point: 3,
                in_stem: "a".into(),
                out_stem: "d".into(),
            }),
            params: serde_json::json!({}),
            staging_dir: Some("/p/stg/t1".into()),
            publish_dir: Some("/p/clip/AlphaHint".into()),
            require: Require {
                protocol: 1,
                models: BTreeMap::new(),
            },
        }
    }

    #[test]
    fn wire_names_round_trip() {
        for k in TaskKind::ALL {
            let v = serde_json::to_value(k).unwrap();
            assert_eq!(v, k.wire());
            assert_eq!(serde_json::from_value::<TaskKind>(v).unwrap(), k);
        }
    }

    #[test]
    fn validation_follows_the_per_kind_shape_the_worker_enforces() {
        assert!(sample(TaskKind::Gvm).validate().is_ok());
        let mut j = sample(TaskKind::Gvm);
        j.staging_dir = Some("/p/clip/.in/t1".into()); // inside the clip folder
        assert!(j.validate().is_err());
        let mut j = sample(TaskKind::Gvm);
        j.publish_dir = None;
        assert!(j.validate().is_err());
        let mut j = sample(TaskKind::SamPreview);
        assert!(j.validate().is_err()); // preview has no publish_dir
        j.publish_dir = None;
        assert!(j.validate().is_ok());
        let j = sample(TaskKind::PrematteHint);
        assert!(j.validate().is_ok());
        assert_eq!(j.kind.wire(), "prematte_hint");
        assert!(j.kind.writes_hint() && j.kind.needs_staging() && j.kind.ranged());
        assert_eq!(j.fence(), "/p/clip");
        let mut j = sample(TaskKind::PrepareSource);
        assert!(j.validate().is_err()); // whole-movie kinds carry null range/staging/publish
        j.range = None;
        j.staging_dir = None;
        j.publish_dir = None;
        assert!(j.validate().is_ok());
        assert_eq!(j.fence(), "/p/clip");
        let mut j = sample(TaskKind::ExportVideo);
        j.staging_dir = None;
        j.publish_dir = None;
        assert!(j.validate().is_err()); // dest missing
        j.params = serde_json::json!({"dest":"/o/x.mp4"});
        assert!(j.validate().is_ok());
        assert_eq!(j.fence(), "/o/x.mp4");
        assert!(serde_json::to_value(&j).unwrap()["staging_dir"].is_null());
    }
}
