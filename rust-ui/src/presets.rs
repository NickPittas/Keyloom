//! Workflow and export presets (`presets.json` next to the preferences). A preset is a field merge: applying
//! one changes only the fields it carries and never source-bound state (In/Out, input colour, prompts, marks,
//! output locations, file names). Unknown keys in the file survive a save; an unreadable file is kept aside.
use crate::hints::HintCfg;
use crate::workflow::{OutputCfg, Params};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    /// `workflow` or `export`.
    pub kind: String,
    pub data: Value,
}

pub fn file_beside(settings_path: Option<&Path>) -> Option<PathBuf> {
    settings_path
        .and_then(Path::parent)
        .map(|d| d.join("presets.json"))
}

pub fn load(path: &Path) -> (Vec<Preset>, Option<String>) {
    let b = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], None),
        Err(e) => return (vec![], Some(format!("{}: {e}", path.display()))),
    };
    let v: Option<Value> = serde_json::from_slice(&b).ok();
    match v
        .as_ref()
        .and_then(|v| v.get("presets"))
        .and_then(|p| serde_json::from_value::<Vec<Preset>>(p.clone()).ok())
    {
        Some(p) => (p, None),
        None => {
            let kept = path.with_extension("json.unreadable");
            let _ = std::fs::rename(path, &kept);
            (
                vec![],
                Some(format!(
                    "presets file unreadable; kept as {}",
                    kept.display()
                )),
            )
        }
    }
}

pub fn save(path: &Path, presets: &[Preset]) -> Result<(), String> {
    let mut obj: Map<String, Value> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    obj.insert("version".into(), json!(1));
    obj.insert(
        "presets".into(),
        serde_json::to_value(presets).map_err(|e| e.to_string())?,
    );
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&Value::Object(obj)).unwrap(),
    )
    .and_then(|()| std::fs::rename(&tmp, path))
    .map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// What a workflow preset captures: engine parameters and the four outputs; NOT the input colour.
pub fn workflow_data(p: &Params, o: &OutputCfg) -> Value {
    let mut params = serde_json::to_value(p).unwrap();
    params.as_object_mut().unwrap().remove("input_is_linear");
    json!({"params": params, "outputs": o})
}

/// Field-merge a workflow preset into a draft. Validated before anything is changed.
pub fn apply_workflow(data: &Value, p: &mut Params, o: &mut OutputCfg) -> Result<(), String> {
    let merge = |base: Value, over: Option<&Value>| -> Value {
        let mut b = base;
        if let (Some(bo), Some(Value::Object(oo))) = (b.as_object_mut(), over) {
            for (k, v) in oo {
                bo.insert(k.clone(), v.clone());
            }
        }
        b
    };
    let mut over = data.get("params").cloned();
    if let Some(Value::Object(m)) = &mut over {
        m.remove("input_is_linear"); // never source-bound
    }
    let np: Params =
        serde_json::from_value(merge(serde_json::to_value(&*p).unwrap(), over.as_ref()))
            .map_err(|e| e.to_string())?;
    let no: OutputCfg = serde_json::from_value(merge(
        serde_json::to_value(&*o).unwrap(),
        data.get("outputs"),
    ))
    .map_err(|e| e.to_string())?;
    np.validate()?;
    no.validate()?;
    (*p, *o) = (np, no);
    Ok(())
}



#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityKind {
    Crf,
    Qscale,
}
/// One selectable video codec: `container` is a `hints::CONTAINERS` id, quality is CRF (lower = better) or qscale.
#[derive(Debug, Clone, Copy)]
pub struct VideoCodec {
    pub container: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub quality: QualityKind,
    pub min: u32,
    pub max: u32,
    pub default: u32,
    pub alpha: bool,
}
pub const VIDEO_CODECS: [VideoCodec; 5] = [
    VideoCodec { container: "mp4", id: "h264", label: "H.264", quality: QualityKind::Crf, min: 0, max: 51, default: 18, alpha: false },
    VideoCodec { container: "mp4", id: "hevc", label: "HEVC (H.265)", quality: QualityKind::Crf, min: 0, max: 51, default: 23, alpha: false },
    VideoCodec { container: "mov_prores4444", id: "prores4444", label: "ProRes 4444 (alpha)", quality: QualityKind::Qscale, min: 1, max: 31, default: 9, alpha: true },
    VideoCodec { container: "mov_prores4444", id: "prores422", label: "ProRes 422", quality: QualityKind::Qscale, min: 1, max: 31, default: 9, alpha: false },
    VideoCodec { container: "webm_alpha", id: "vp9", label: "VP9 (alpha)", quality: QualityKind::Crf, min: 0, max: 63, default: 18, alpha: true },
];
pub fn codecs_for(container: &str) -> Vec<&'static VideoCodec> {
    VIDEO_CODECS.iter().filter(|c| c.container == container).collect()
}
pub fn codec(container: &str, id: &str) -> Option<&'static VideoCodec> {
    VIDEO_CODECS.iter().find(|c| c.container == container && c.id == id)
}

/// How one enabled output is delivered: its frame sequence (format/EXR compression come from `OutputCfg`) or a
/// video made from it after Key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Delivery {
    Sequence,
    Video(VideoDelivery),
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VideoDelivery {
    pub container: String,
    pub codec: String,
    pub quality: u32,
}
impl VideoDelivery {
    pub fn default_for(container: &str) -> Self {
        let c = codecs_for(container).into_iter().next().unwrap_or(&VIDEO_CODECS[0]);
        Self { container: c.container.into(), codec: c.id.into(), quality: c.default }
    }
    pub fn validate(&self) -> Result<(), String> {
        let c = codec(&self.container, &self.codec)
            .ok_or_else(|| format!("unknown video codec {:?} for {:?}", self.codec, self.container))?;
        if !(c.min..=c.max).contains(&self.quality) {
            return Err(format!("{} quality must be {}-{}", c.label, c.min, c.max));
        }
        Ok(())
    }
}

/// Delivery step of a pipeline. `outputs` has exactly one entry per enabled output (`fg|matte|comp|processed`).
/// Video deliveries are queued after Key whenever any exists (`has_video`). `root: None` = each clip's `<project>/Exports/<clip folder>`;
/// `fps: None` = each clip's own fps. File names are never stored: they are built per clip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PipelineExport {
    pub fps: Option<f64>,
    pub allow_partial: bool,
    pub root: Option<PathBuf>,
    pub outputs: BTreeMap<String, Delivery>,
}
impl Default for PipelineExport {
    fn default() -> Self {
        Self::for_outputs(&OutputCfg::default())
    }
}
impl PipelineExport {
    pub fn for_outputs(o: &OutputCfg) -> Self {
        let mut e = Self { fps: None, allow_partial: false, root: None, outputs: BTreeMap::new() };
        e.sync_outputs(o);
        e
    }
    /// Follow the enabled outputs: new ones deliver as a sequence, disabled ones are dropped, video choices stay.
    pub fn sync_outputs(&mut self, o: &OutputCfg) {
        let on: Vec<String> = o.kinds().iter().map(|k| k.0.token().to_lowercase()).collect();
        self.outputs.retain(|k, _| on.contains(k));
        for k in on {
            self.outputs.entry(k).or_insert(Delivery::Sequence);
        }
    }
    pub fn has_video(&self) -> bool {
        self.outputs.values().any(|d| matches!(d, Delivery::Video(_)))
    }
    /// Video deliveries in manifest order (fg, matte, comp, processed).
    pub fn videos(&self) -> Vec<(&str, &VideoDelivery)> {
        ["fg", "matte", "comp", "processed"]
            .into_iter()
            .filter_map(|k| match self.outputs.get(k) {
                Some(Delivery::Video(v)) => Some((k, v)),
                _ => None,
            })
            .collect()
    }
    /// fps and folder rules only (the per-output rules need the `OutputCfg`, see `validate`).
    pub fn validate_videos(&self) -> Result<(), String> {
        for (k, v) in self.videos() {
            v.validate().map_err(|e| format!("{k}: {e}"))?;
        }
        if self.fps.is_some_and(|f| !(f.is_finite() && f > 0.0 && f <= 1000.0)) {
            return Err("export fps must be 0-1000".into());
        }
        if self.root.as_ref().is_some_and(|r| !r.is_absolute()) {
            return Err("export folder must be an absolute path".into());
        }
        Ok(())
    }
    pub fn validate(&self, o: &OutputCfg) -> Result<(), String> {
        let on: Vec<String> = o.kinds().iter().map(|k| k.0.token().to_lowercase()).collect();
        for k in self.outputs.keys() {
            if !on.contains(k) {
                return Err(format!("delivery for {k:?}, which is not an enabled output"));
            }
        }
        for k in &on {
            match self.outputs.get(k) {
                None => return Err(format!("no delivery chosen for output {k:?}")),
                Some(_) => {}
            }
        }
        self.validate_videos()
    }
}

/// A full pre-matte -> Key -> export preset. Never carries source-bound state (In/Out, input colour, prompts,
/// marks, masks, client, file names). `location` is `None` when the project had no saved output location, so
/// each clip keeps its own default instead of inheriting a sample clip's path.
#[derive(Debug, Clone, PartialEq)]
pub struct Pipeline {
    pub hint: HintCfg,
    pub params: Params,
    pub outputs: OutputCfg,
    pub location: Option<Value>,
    pub export: PipelineExport,
}

pub const PIPELINE_SCHEMA: &str = "pipeline";

impl Pipeline {
    pub fn validate(&self) -> Result<(), String> {
        self.hint.validate()?;
        self.params.validate()?;
        self.outputs.validate()?;
        self.export.validate(&self.outputs)?;
        if let Some(l) = &self.location {
            let loc: crate::contracts::OutputLocation = serde_json::from_value(l.clone())
                .map_err(|e| format!("output location: {e}"))?;
            crate::contracts::validate_templates(&loc, self.outputs.kinds().len().max(1))
                .map_err(|e| format!("output location: {e:?}"))?;
        }
        Ok(())
    }

    /// `Preset.data`. A superset of the legacy workflow shape (`params`, `outputs`), so older readers still merge those.
    pub fn to_value(&self) -> Value {
        let mut data = workflow_data(&self.params, &self.outputs);
        let o = data.as_object_mut().unwrap();
        o.insert("schema".into(), json!(PIPELINE_SCHEMA));
        o.insert("version".into(), json!(3));
        o.insert("hint".into(), serde_json::to_value(&self.hint).unwrap());
        o.insert("location".into(), self.location.clone().unwrap_or(Value::Null));
        o.insert("export".into(), serde_json::to_value(&self.export).unwrap());
        data
    }

    /// Strict: legacy/partial presets (no `schema: pipeline`, or missing a part) are refused with the reason.
    /// Version 2 (one `product` + `container` video) is still read: that product becomes a Video delivery with the
    /// container's default codec/quality, every other enabled output a Sequence.
    pub fn from_value(data: &Value) -> Result<Self, String> {
        if data.get("schema").and_then(Value::as_str) != Some(PIPELINE_SCHEMA) {
            return Err("legacy preset: only parameters/outputs, not a full pipeline".into());
        }
        let version = data.get("version").and_then(Value::as_u64);
        if !matches!(version, Some(2 | 3)) {
            return Err("unsupported pipeline preset version".into());
        }
        if data.get("location").is_none() {
            return Err("pipeline preset is missing \"location\"".into());
        }
        let part = |k: &str| data.get(k).ok_or_else(|| format!("pipeline preset is missing {k:?}"));
        // Every serialized key must be present: a missing one would silently take its default.
        let mut exp_default = serde_json::to_value(PipelineExport::default()).unwrap();
        let m = exp_default.as_object_mut().unwrap();
        // The required delivery set depends on the enabled outputs; `validate` checks it.
        m.insert("outputs".into(), json!({}));
        if version == Some(2) {
            m.remove("outputs");
            m.insert("enabled".into(), json!(false));
            m.insert("product".into(), json!(""));
            m.insert("container".into(), json!(""));
        }
        let defaults = [
            ("params", serde_json::to_value(Params::default()).unwrap()),
            ("outputs", serde_json::to_value(OutputCfg::default()).unwrap()),
            ("hint", serde_json::to_value(HintCfg::default()).unwrap()),
            ("export", exp_default),
        ];
        for (k, d) in &defaults {
            require_keys(d, part(k)?, k)?;
        }
        let mut params = part("params")?.clone();
        if let Some(m) = params.as_object_mut() {
            m.remove("input_is_linear");
        }
        let outputs: OutputCfg = serde_json::from_value(part("outputs")?.clone()).map_err(|e| format!("outputs: {e}"))?;
        let export = if version == Some(2) {
            #[derive(Deserialize)]
            struct V2 {
                enabled: bool,
                product: String,
                container: String,
                fps: Option<f64>,
                allow_partial: bool,
                root: Option<PathBuf>,
            }
            let v: V2 = serde_json::from_value(part("export")?.clone()).map_err(|e| format!("export: {e}"))?;
            let mut e = PipelineExport { fps: v.fps, allow_partial: v.allow_partial, root: v.root, ..PipelineExport::for_outputs(&outputs) };
            if codecs_for(&v.container).is_empty() {
                return Err(format!("unknown export container {:?}", v.container));
            }
            if v.enabled {
                // Only the selected product was ever exported.
                if !e.outputs.contains_key(&v.product) {
                    return Err(format!("export product {} is not an enabled output", v.product));
                }
                e.outputs.insert(v.product, Delivery::Video(VideoDelivery::default_for(&v.container)));
            }
            e
        } else {
            serde_json::from_value(part("export")?.clone()).map_err(|e| format!("export: {e}"))?
        };
        let p = Pipeline {
            hint: HintCfg::from_value_strict(part("hint")?)?,
            params: serde_json::from_value(params).map_err(|e| format!("params: {e}"))?,
            outputs,
            location: data.get("location").filter(|l| !l.is_null()).cloned(),
            export,
        };
        p.validate()?;
        Ok(p)
    }

    /// Replace the draft with this pipeline's settings; the source-bound input colour stays.
    pub fn apply(&self, hint: &mut HintCfg, params: &mut Params, outputs: &mut OutputCfg) {
        *hint = self.hint.clone();
        *outputs = self.outputs.clone();
        *params = Params { input_is_linear: params.input_is_linear, ..self.params.clone() };
    }
}

/// Every key of `default` (recursively through objects) must exist in `got`; unknown extra keys are fine.
/// `params.input_is_linear` is source-bound and never stored.
fn require_keys(default: &Value, got: &Value, path: &str) -> Result<(), String> {
    let Some(d) = default.as_object() else { return Ok(()) };
    let g = got.as_object().ok_or_else(|| format!("pipeline preset: {path} must be an object"))?;
    for (k, v) in d {
        let p = format!("{path}.{k}");
        if p == "params.input_is_linear" {
            continue;
        }
        match g.get(k) {
            Some(x) => require_keys(v, x, &p)?,
            None => return Err(format!("pipeline preset is missing {p:?}")),
        }
    }
    Ok(())
}

/// Full pipeline vs. a legacy workflow preset (params/outputs only) vs. export preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetClass {
    Pipeline,
    LegacyWorkflow,
    Export,
}
pub fn class_of(p: &Preset) -> PresetClass {
    match (p.kind.as_str(), p.data.get("schema").and_then(Value::as_str)) {
        ("workflow", Some(PIPELINE_SCHEMA)) => PresetClass::Pipeline,
        ("workflow", _) => PresetClass::LegacyWorkflow,
        _ => PresetClass::Export,
    }
}
/// Name as shown in a list: legacy presets are labelled so they are not mistaken for full pipelines.
pub fn label(p: &Preset) -> String {
    match class_of(p) {
        PresetClass::LegacyWorkflow => format!("{} (legacy: parameters/outputs only)", p.name),
        _ => p.name.clone(),
    }
}
/// New full-pipeline preset (`kind` stays `workflow`, so existing lists and files keep working).
pub fn pipeline_preset(name: &str, p: &Pipeline) -> Preset {
    Preset { name: name.into(), kind: "workflow".into(), data: p.to_value() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_preset_merges_fields_and_never_touches_colour() {
        let mut p = Params {
            input_is_linear: true,
            despill_strength: 0.9,
            ..Params::default()
        };
        let mut o = OutputCfg::default();
        let saved = workflow_data(
            &Params {
                input_is_linear: false,
                hint_mode: "full".into(),
                ..Params::default()
            },
            &OutputCfg {
                fg_enabled: false,
                ..OutputCfg::default()
            },
        );
        assert!(saved["params"].get("input_is_linear").is_none());
        apply_workflow(&saved, &mut p, &mut o).unwrap();
        assert!(p.input_is_linear, "source-bound colour is kept");
        assert_eq!(p.hint_mode, "full");
        assert!(!o.fg_enabled);
        // A partial preset changes only what it names.
        let mut p2 = p.clone();
        apply_workflow(
            &json!({"params": {"despill_strength": 0.2}}),
            &mut p2,
            &mut o,
        )
        .unwrap();
        assert_eq!((p2.hint_mode.as_str(), p2.despill_strength), ("full", 0.2));
        // Invalid content changes nothing.
        let mut p3 = p.clone();
        assert!(
            apply_workflow(&json!({"params": {"hint_mode": "bogus"}}), &mut p3, &mut o).is_err()
        );
        assert_eq!(p3, p);
    }

    #[test]
    fn file_keeps_unknown_keys_and_sets_aside_garbage() {
        let d = std::env::temp_dir().join(format!("ck-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("presets.json");
        std::fs::write(&f, r#"{"version":1,"presets":[],"future":{"a":1}}"#).unwrap();
        let ps = vec![Preset {
            name: "x".into(),
            kind: "export".into(),
            data: json!({"container": "mp4"}),
        }];
        save(&f, &ps).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(raw["future"]["a"], 1);
        assert_eq!(load(&f).0, ps);
        std::fs::write(&f, "garbage").unwrap();
        let (got, err) = load(&f);
        assert!(got.is_empty() && err.is_some() && d.join("presets.json.unreadable").is_file());
        let _ = std::fs::remove_dir_all(d);
    }
}
