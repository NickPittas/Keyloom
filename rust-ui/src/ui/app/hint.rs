//! Hint page: route/model selection, SAM/paint annotation editing, preview/track/generate jobs, keyframe marks,
//! companion alpha/mask movies, frame extraction and the Deliver video export. Everything persistent lives in
//! `clip.json["ck_native"]` (see `persist`/`hints`); every job is built from a frozen snapshot of that state and
//! submitted to the shared scheduler. Nothing here talks to a model: the worker does, or the control stays off.
use super::*;
use crate::hints::{
    self, Annotations, Ctx, Final, HintCfg, Producer, BIREFNET_VARIANTS, SAM_MODELS,
};
use crate::presets::{Delivery, PipelineExport, QualityKind, VideoDelivery, VIDEO_CODECS};
use crate::tasks::TaskKind;
use crate::ui::ai::{card, fill_slider, pair, sig_of};
use crate::ui::annotate::{self, Ann, Tool};
use crate::workflow::new_id;
use crate::shot_domains::{self, Domain, Field};
use eframe::egui::Sense;
use std::path::Path;

#[derive(Default)]
pub struct State {
    folder: Option<String>,
    cfg: HintCfg,
    ann: Annotations,
    undo: Vec<Annotations>,
    /// Which generated mask is tinted over the Input frame.
    overlay: Overlay,
    note: Option<String>,
    /// Picked companion movie waiting for its kind/channel to be confirmed.
    movie: Option<PathBuf>,
    movie_mask: bool,
    movie_alpha_channel: bool,
    /// Per enabled output (fg|matte|comp|processed): image sequence or a video file with its own settings.
    deliveries: std::collections::BTreeMap<String, Delivery>,
    export_fps: Option<f64>,
    export_partial: bool,
    export_dir: Option<PathBuf>,
    presets: Vec<crate::presets::Preset>,
    presets_loaded: bool,
    preset_name: String,
    preset_sel: String,
    /// Selection the name field was last filled from: the field follows the selection only when it changes.
    preset_name_for: String,
    /// Preset controls drawn this frame (name field, Save, Rename, Delete), for layout/interaction tests.
    preset_rects: Vec<(&'static str, egui::Rect)>,
    /// Deliver cards drawn this frame (output key or `render`), for layout tests.
    deliver_rects: Vec<(String, egui::Rect)>,
    /// Per-output sequence format buttons (`<output>_exr` / `<output>_png`), inside the cards (click tests).
    deliver_fmt_rects: Vec<(String, egui::Rect)>,
    /// "Run full workflow" pressed while results exist: waits for an explicit choice, on frozen settings.
    render_confirm: Option<RenderConfirm>,
    /// Prompt tools drawn in the strip this frame (tool, screen rect). Tests read the layout from here.
    pub(super) strip_rects: Vec<(Tool, egui::Rect)>,
    rc: Recompute,
    /// Delivery config JSON / annotation signature as last loaded or flushed to `ck_native` (dirty = differs).
    saved_delivery: String,
    /// The shot's hand-edited hint-route / video-delivery FIELDS with the value each had before its first edit
    /// (`shot_domains`), and the preset the unmarked values were last applied from.
    route_ov: crate::shot_domains::Fields,
    route_src: Option<String>,
    delivery_ov: crate::shot_domains::Fields,
    delivery_src: Option<String>,
    /// The route as last persisted for this shot; an edit is "unsaved" while the shown route differs from it.
    route_saved: HintCfg,
    /// The shown route a save already failed for (reported once; retried on the next edit or Save).
    route_failed: Option<HintCfg>,
    /// The delivery a save already failed for (reported once; retried on the next edit or Save).
    delivery_failed: Option<String>,
    /// The route was saved while the pointer was down: the project's copy of the clip is refreshed once it settles.
    route_refresh: Option<String>,
    ann_saved: u64,
}


/// A pending "Run full workflow" confirmation. `pipeline` is the capture taken when the button was pressed:
/// it is what runs if confirmed, and the confirmation closes if anything it was based on changes.
struct RenderConfirm {
    folder: String,
    sig: String,
    pipeline: crate::presets::Pipeline,
    existing: Vec<String>,
}

/// Stage results already on disk for this shot (pre-matte / hint frames and any Key result), described for
/// the confirmation. Conservative: any of them, complete or not, counts.
fn existing_stage_results(c: &Clip) -> Vec<String> {
    let mut v = vec![];
    if c.alpha_frames > 0 {
        v.push(format!("{} hint (AlphaHint) frame(s)", c.alpha_frames));
    }
    if c.mask_frames > 0 {
        v.push(format!("{} pre-matte frame(s)", c.mask_frames));
    }
    match &c.manifest {
        Some(m) => v.push(format!("a {} Key result", if m.complete { "complete" } else { "partial" })),
        None if c.output_values.is_some() => v.push("a previous Key run".into()),
        None => {}
    }
    v
}

/// Everything the confirmation was based on besides the captured pipeline: the shot, what exists, the range.
fn result_sig(c: &Clip, range: &Result<(usize, usize), String>) -> String {
    format!(
        "{}|{}|{}|{:?}|{}|{:?}|{:?}",
        c.folder,
        c.alpha_frames,
        c.mask_frames,
        c.manifest.as_ref().map(|m| (m.complete, m.version)),
        c.output_values.is_some(),
        c.manifest_dir,
        range
    )
}

/// What a generated asset was made from: the hint settings and the SAM prompts.
#[derive(Clone, PartialEq)]
struct Snap {
    cfg: HintCfg,
    ann: u64,
}

/// Recompute cues. Baselines move only when a job that published (or a Key/export that finished) actually
/// completes; queued jobs carry the snapshot frozen at submit time in `inflight`.
#[derive(Default)]
struct Recompute {
    base_pre: Option<Snap>,
    base_hint: Option<Snap>,
    inflight: std::collections::HashMap<String, Snap>,
    seen: std::collections::HashSet<String>,
    prev: Option<(Snap, u64, u64)>,
    pulse_at: Option<f64>,
    key_manifest: Option<u64>,
    /// A hint was published after the last Key result.
    hint_after_key: bool,
    /// The scheduler's Done stamp of the latest hint/pre-matte job: a Key result consumed it only if that Key job
    /// was dispatched at or after it.
    hint_done_at: Option<std::time::Instant>,
    /// Per video output, what its last successful export in this session encoded (from the finished job itself)
    /// and when it finished.
    deliver_done: std::collections::BTreeMap<String, ExportDone>,
    /// The video outputs as configured when the clip was opened: configured, but never verified as encoded.
    deliver_open: std::collections::BTreeMap<String, ProductCfg>,
    deliver_seeded: bool,
    /// Pending outputs (amber cue) and unverified ones (shown, no cue), with the reason, for the action area.
    deliver_why: Vec<String>,
    deliver_unverified: Vec<String>,
    /// The current Key result's job as the scheduler last showed it: `(job id, dispatched, finished)`. Kept so a
    /// scheduler rebuild (AI targets applied) does not make a known Key result look unknown.
    key_seen: Option<(String, Option<std::time::Instant>, Option<std::time::Instant>)>,
    pre: bool,
    hint: bool,
    key: bool,
    deliver: bool,
}

/// The eyedropper's result: the chroma screen colour is part of the route and is saved (and marked as a hand edit)
/// by the same per-frame step as every other route edit.
fn pick_screen_colour(h: &mut State, rgb: [u8; 3]) {
    h.cfg.chroma.screen_color = Some(rgb);
}

fn p_clip_by_folder(p: Option<&Project>, folder: &str) -> Option<Clip> {
    p?.clips.iter().find(|c| c.folder == folder).cloned()
}

/// Is the latest hint newer than the Key result? Only a Key job dispatched at or after the hint's Done stamp
/// consumed it; an unknown (older-session) Key cannot have.
fn hint_unconsumed(hint_done: Option<std::time::Instant>, key_dispatched: Option<std::time::Instant>) -> bool {
    hint_done.is_some_and(|done| !key_dispatched.is_some_and(|d| d >= done))
}

#[cfg(test)]
mod cue_tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn a_hint_is_only_consumed_by_a_key_dispatched_at_or_after_it_finished() {
        let t = Instant::now();
        let (before, after) = (t - Duration::from_secs(2), t + Duration::from_secs(2));
        assert!(!hint_unconsumed(None, Some(t)), "no hint, nothing to consume");
        assert!(hint_unconsumed(Some(t), None), "a Key this session does not know cannot have consumed it");
        assert!(hint_unconsumed(Some(t), Some(before)), "a Key dispatched before the hint finished did not");
        assert!(!hint_unconsumed(Some(t), Some(t)) && !hint_unconsumed(Some(t), Some(after)));
    }

    #[test]
    fn a_pipeline_job_snapshot_keeps_the_baseline_annotations() {
        let job = |kind, params| crate::tasks::TaskJob {
            job_id: "j".into(),
            kind,
            clip_id: "c".into(),
            clip_root: "/c".into(),
            range: None,
            params,
            staging_dir: None,
            publish_dir: None,
            require: crate::jobs::Require { protocol: 1, models: Default::default() },
        };
        let base = Snap { cfg: HintCfg::default(), ann: 1 };
        let s = snap_from_job(&job(TaskKind::Birefnet, serde_json::json!({"variant": "Matting"})), &base, false);
        assert_eq!((s.ann, s.cfg.birefnet_variant.as_str()), (1, "Matting"));
        // Even a SAM job's fold leaves the annotations alone: nothing here can say what prompts it used.
        let s = snap_from_job(&job(TaskKind::SamTrack, serde_json::json!({"tracker_model": "base_plus"})), &base, false);
        assert_eq!((s.ann, s.cfg.guide), (1, Producer::Sam));
    }
}

/// A shot's route / delivery edits that could not be written when its page was left: kept in memory (and shown as
/// unsaved) until a write succeeds, never dropped.
#[derive(Clone, Default)]
pub struct Unsaved {
    route: Option<HintCfg>,
    delivery: Option<(crate::shot_domains::Fields, Option<String>, PipelineExport)>,
}

/// One video output's encode settings: container, codec, quality, fps in thousandths, partial-result flag and
/// the In/Out range it encodes.
#[derive(Clone, Debug, PartialEq)]
struct ProductCfg(String, String, u32, i64, bool, Option<(usize, usize)>);

/// A successful export: what it encoded, where it was written, and when it finished (scheduler stamp).
#[derive(Clone, Debug)]
struct ExportDone {
    cfg: ProductCfg,
    dest_dir: String,
    at: std::time::Instant,
}

/// The video outputs as configured now (sequence outputs have nothing for Deliver to render).
fn current_products(h: &State, fps: f64, range: Option<(usize, usize)>) -> std::collections::BTreeMap<String, ProductCfg> {
    let fps = (h.export_fps.unwrap_or(fps) * 1000.0).round() as i64;
    h.deliveries
        .iter()
        .filter_map(|(k, d)| match d {
            Delivery::Video(v) => Some((k.clone(), ProductCfg(v.container.clone(), v.codec.clone(), v.quality, fps, h.export_partial, range))),
            Delivery::Sequence => None,
        })
        .collect()
}

/// What a finished export job really encoded (its frozen parameters, not today's settings).
fn exported(j: &crate::tasks::TaskJob) -> Option<(String, ProductCfg, String)> {
    let p = &j.params;
    let dest = std::path::Path::new(p.get("dest")?.as_str()?);
    Some((
        p.get("product")?.as_str()?.to_owned(),
        ProductCfg(
            p.get("container")?.as_str()?.to_owned(),
            p.get("codec").and_then(Value::as_str).unwrap_or_default().to_owned(),
            p.get("quality").and_then(Value::as_u64).unwrap_or_default() as u32,
            (p.get("fps")?.as_f64()? * 1000.0).round() as i64,
            p.get("allow_partial").and_then(Value::as_bool).unwrap_or(false),
            j.range.as_ref().map(|r| (r.in_point, r.out_point)),
        ),
        dest.parent().map(|d| d.display().to_string()).unwrap_or_default(),
    ))
}

/// The settings a finished pre-matte/hint job ran with, folded from its own frozen parameters onto the stage's
/// previous baseline (the same fold the on-disk provenance receipt uses). Pipelines never run SAM (a SAM guide is
/// a hand-made, precomputed one), so the annotations are exactly the baseline's.
fn snap_from_job(j: &crate::tasks::TaskJob, base: &Snap, is_hint: bool) -> Snap {
    let mut cfg = base.cfg.clone();
    apply_prov(&mut cfg, &serde_json::json!({"kind": j.kind.wire(), "params": j.params}), is_hint);
    Snap { cfg, ann: base.ann }
}

fn delivery_json(h: &State) -> String {
    let e = PipelineExport { outputs: h.deliveries.clone(), fps: h.export_fps, allow_partial: h.export_partial, root: h.export_dir.clone() };
    serde_json::to_string(&e).unwrap_or_default()
}

fn ann_sig(a: &Annotations) -> u64 {
    sig_of(&[&serde_json::to_string(&a.frames).unwrap_or_default()])
}

/// Does the draft differ from what a Key result froze? Compares the manifest's `params` keys, enabled outputs,
/// formats and EXR compression (numbers with a small tolerance).
fn manifest_differs(m: &Value, p: &crate::workflow::Params, o: &crate::workflow::OutputCfg) -> bool {
    let cur = serde_json::to_value(p).unwrap_or_default();
    if let Some(saved) = m.get("params").and_then(Value::as_object) {
        for (k, v) in saved {
            match (cur.get(k), v) {
                (Some(Value::Number(a)), Value::Number(b)) => {
                    if (a.as_f64().unwrap_or(0.0) - b.as_f64().unwrap_or(0.0)).abs() > 1e-6 {
                        return true;
                    }
                }
                (Some(a), b) if a != b => return true,
                _ => {}
            }
        }
    }
    let on = |k: &str| match k {
        "fg" => o.fg_enabled,
        "matte" => o.matte_enabled,
        "comp" => o.comp_enabled,
        _ => o.processed_enabled,
    };
    let fmt = |k: &str| match k {
        "fg" => &o.fg_format,
        "matte" => &o.matte_format,
        "comp" => &o.comp_format,
        _ => &o.processed_format,
    };
    if let Some(en) = m.get("enabled_outputs").and_then(Value::as_array) {
        for k in ["fg", "matte", "comp", "processed"] {
            if en.iter().any(|x| x == k) != on(k) {
                return true;
            }
            if on(k) && m.get("formats").and_then(|f| f.get(k)).and_then(Value::as_str).is_some_and(|f| f != fmt(k)) {
                return true;
            }
        }
    }
    m.get("exr_compression").and_then(Value::as_str).is_some_and(|c| c != o.exr_compression)
}

/// Settings that change the pre-matte (guide) result.
fn pre_diff(a: &Snap, b: &Snap) -> bool {
    let (x, y) = (&a.cfg, &b.cfg);
    x.guide != y.guide
        || x.mask_cleanup.grow != y.mask_cleanup.grow
        || x.mask_cleanup.fill_holes != y.mask_cleanup.fill_holes
        || match x.guide {
            Producer::Sam => x.sam_model != y.sam_model || a.ann != b.ann,
            Producer::Birefnet => x.birefnet_variant != y.birefnet_variant,
            Producer::Chroma => x.chroma != y.chroma,
            Producer::Gvm => false,
        }
}

/// Fold a saved `.corridorkey_hint.json` (kind + params) into the settings it was made from.
fn apply_prov(cfg: &mut HintCfg, v: &Value, is_hint: bool) {
    let guide = match v.get("kind").and_then(Value::as_str) {
        Some("sam_track") => Some(Producer::Sam),
        Some("gvm") => Some(Producer::Gvm),
        Some("birefnet") => Some(Producer::Birefnet),
        Some("chroma") => Some(Producer::Chroma),
        _ => None,
    };
    match v.get("kind").and_then(Value::as_str) {
        Some("matanyone2") if is_hint => cfg.finish = Final::Matanyone2,
        Some("videomama") if is_hint => cfg.finish = Final::Videomama,
        _ => {}
    }
    if let Some(g) = guide {
        cfg.guide = g;
        if is_hint {
            cfg.finish = Final::Direct;
        }
    }
    if let Some(p) = v.get("params") {
        if let Some(m) = p.get("tracker_model").and_then(Value::as_str) {
            // The worker records the model by wire id ("base_plus"), the same id the settings use.
            cfg.sam_model = m.to_string();
        }
        if let Some(b) = p.get("variant").and_then(Value::as_str) {
            cfg.birefnet_variant = b.to_string();
        }
        if let Some(c) = p.get("chunk_size").and_then(Value::as_u64) {
            cfg.videomama_chunk = c as u32;
        }
        if guide == Some(Producer::Chroma) {
            let c = &mut cfg.chroma;
            if let Some(t) = p.get("screen_type").and_then(Value::as_str) {
                c.screen_type = t.to_string();
            }
            c.screen_color = match p.get("screen_color") {
                Some(Value::Array(a)) if a.len() == 3 => {
                    let g = |i: usize| a[i].as_u64().unwrap_or(0) as u8;
                    Some([g(0), g(1), g(2)])
                }
                Some(_) => None,
                None => c.screen_color,
            };
            for (k, f) in [("strength", &mut c.strength), ("clip_black", &mut c.clip_black), ("clip_white", &mut c.clip_white)] {
                if let Some(x) = p.get(k).and_then(Value::as_f64) {
                    *f = x;
                }
            }
            if let Some(x) = p.get("shrink_grow").and_then(Value::as_i64) {
                c.shrink_grow = x as _;
            }
            if let Some(x) = p.get("edge_blur").and_then(Value::as_i64) {
                c.edge_blur = x as _;
            }
        }
    }
    if let Some(m) = v.get("params").and_then(|p| p.get("mask_cleanup")) {
        let c = &mut cfg.mask_cleanup;
        if let Some(g) = m.get("grow").and_then(Value::as_i64) {
            c.grow = g;
        }
        if let Some(f) = m.get("fill_holes").and_then(Value::as_bool) {
            c.fill_holes = f;
        }
        if let Some(f) = m.get("feather").and_then(Value::as_f64) {
            c.feather = f;
        }
    }
}

/// Baselines from what is actually on disk: the publication provenance the worker wrote, else "as configured".
fn prov_base(clip: &Clip, cur: &Snap) -> (Snap, Snap) {
    let read = |d: &str| {
        std::fs::read(clip.root.join(d).join(".corridorkey_hint.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
    };
    let (mut pre, mut hint) = (cur.clone(), cur.clone());
    if let Some(v) = read(hints::GUIDE_DIR) {
        apply_prov(&mut pre.cfg, &v, false);
    }
    if let Some(v) = read(hints::HINT_DIR) {
        apply_prov(&mut hint.cfg, &v, true);
        if read(hints::GUIDE_DIR).is_none() {
            pre.cfg.guide = hint.cfg.guide;
        }
    }
    (pre, hint)
}

#[derive(Default, Clone, Copy, PartialEq)]
enum Overlay {
    #[default]
    Off,
    Guide,
    Hint,
}

/// Tools the guide producer can use: SAM takes prompts, Chroma a screen colour, the others are automatic.
fn tools_for(p: Producer) -> &'static [(Tool, &'static str, &'static str)] {
    match p {
        Producer::Sam => &[
            (Tool::FgPoint, "Point +", "Foreground point (click)"),
            (Tool::BgPoint, "Point −", "Background point (click)"),
            (Tool::PaintFg, "Paint +", "Paint foreground"),
            (Tool::PaintBg, "Paint −", "Paint background"),
            (Tool::Erase, "Erase", "Erase prompts under the brush"),
            (Tool::Box, "Box", "Drag a box (one per frame)"),
        ],
        Producer::Chroma => &[(Tool::Pick, "Pick colour", "Click the screen colour on the frame")],
        _ => &[],
    }
}

#[derive(Clone, Copy)]
enum Run {
    Guide,
    Hint,
    GuideMarks,
    HintMarks,
}

/// What a preview depends on: the frame, its prompts, the tracker model, mask cleanup and the colour reading.
fn psig(h: &State, stem: &str, linear: bool) -> u64 {
    let f = h
        .ann
        .frames
        .get(stem)
        .map(|f| serde_json::to_string(f).unwrap_or_default())
        .unwrap_or_default();
    let mc = serde_json::to_string(&h.cfg.mask_cleanup).unwrap_or_default();
    sig_of(&[stem, &h.cfg.sam_model, &f, &linear.to_string(), &mc])
}

fn stems_of(c: &Clip) -> Vec<String> {
    c.input_files.iter().map(|f| f.0.clone()).collect()
}

impl State {
    pub(super) fn deliveries_has_video(&self) -> bool {
        self.deliveries.values().any(|d| matches!(d, Delivery::Video(_)))
    }
}

impl App {
    fn hint_note(&mut self, m: impl Into<String>) {
        self.hint.note = Some(m.into());
    }

    /// The Hint page's config for the open clip (tests: the node graph and the page must be one value).
    pub fn preset_rects(&self) -> &[(&'static str, egui::Rect)] {
        &self.hint.preset_rects
    }

    pub fn deliver_format_rects(&self) -> &[(String, egui::Rect)] {
        &self.hint.deliver_fmt_rects
    }
    pub fn deliver_rects(&self) -> &[(String, egui::Rect)] {
        &self.hint.deliver_rects
    }

    pub fn preset_state(&self) -> (String, String) {
        (self.hint.preset_sel.clone(), self.hint.preset_name.clone())
    }

    #[allow(clippy::type_complexity)]
    pub fn export_state_for_test(&self) -> (std::collections::BTreeMap<String, Delivery>, Option<f64>, bool, Option<PathBuf>) {
        let h = &self.hint;
        (h.deliveries.clone(), h.export_fps, h.export_partial, h.export_dir.clone())
    }

    /// Replace the per-output choices (outputs not named stay/become image sequences on the next frame).
    pub fn set_export_for_test(&mut self, deliveries: &[(&str, Delivery)], fps: Option<f64>, partial: bool, dir: Option<PathBuf>) {
        let h = &mut self.hint;
        h.deliveries = deliveries.iter().map(|(k, d)| (k.to_string(), d.clone())).collect();
        (h.export_fps, h.export_partial, h.export_dir) = (fps, partial, dir);
    }

    /// Recompute cue of one stage of the open clip: `pre`, `hint`, `key` or `deliver` (tests).
    pub fn stage_pending_for_test(&self, stage: &str) -> bool {
        let rc = &self.hint.rc;
        match stage {
            "pre" => rc.pre,
            "hint" => rc.hint,
            "key" => rc.key,
            _ => rc.deliver,
        }
    }
    /// Video outputs whose last successful export differs from the configured one (tests).
    pub fn deliver_pending_for_test(&self) -> Vec<String> {
        self.hint.rc.deliver_why.iter().map(|w| w.split(" (").next().unwrap_or(w).to_owned()).collect()
    }
    /// Video outputs configured at open with no export seen in this session (tests).
    pub fn deliver_unverified_for_test(&self) -> Vec<String> {
        self.hint.rc.deliver_unverified.clone()
    }

    /// Reset the open shot's own hint route / delivery to their recorded bases (tests; the buttons call the same code).
    pub fn reset_route_for_test(&mut self) {
        if let Some(c) = self.clip().cloned() {
            self.reset_route(&c);
        }
    }
    pub fn reset_delivery_for_test(&mut self) {
        if let Some(c) = self.clip().cloned() {
            self.reset_delivery(&c);
        }
    }
    /// What the eyedropper does with a picked colour (tests; the viewer event path calls the same function).
    pub fn pick_colour_for_test(&mut self, rgb: [u8; 3]) -> Result<(), String> {
        self.clip().ok_or("no clip open")?;
        pick_screen_colour(&mut self.hint, rgb);
        self.hint_pending_refresh();
        Ok(())
    }

    /// The open shot's marked route / delivery fields (tests).
    pub fn override_state_for_test(&self) -> (Vec<String>, Vec<String>) {
        (self.hint.route_ov.keys().cloned().collect(), self.hint.delivery_ov.keys().cloned().collect())
    }

    /// The text of the open "Run full workflow" confirmation, if any (tests).
    pub fn render_confirm_text(&self) -> Option<String> {
        self.hint.render_confirm.as_ref().map(|c| c.existing.join(", "))
    }

    /// Edit the open clip's Key draft (tests).
    pub fn draft_for_test(&mut self, f: impl FnOnce(&mut crate::workflow::Params, &mut crate::workflow::OutputCfg)) {
        if let (Some(c), Some(p)) = (self.clip().cloned(), self.project.as_ref()) {
            let d = self.ai.draft(&c, p, self.settings.saved());
            f(&mut d.params, &mut d.out);
        }
    }

    pub fn draft_snapshot_for_test(&mut self) -> Option<(crate::workflow::Params, crate::workflow::OutputCfg)> {
        let mut r = None;
        self.draft_for_test(|p, o| r = Some((p.clone(), o.clone())));
        r
    }

    /// In-memory project location only (tests set the starting state; Apply is what must persist it).
    pub fn set_location_for_test(&mut self, v: Option<Value>) {
        if let Some(p) = &mut self.project {
            p.output_location = v;
        }
    }

    pub fn project_root_for_test(&self) -> PathBuf {
        self.project.as_ref().map(|p| p.root.clone()).unwrap_or_default()
    }

    /// Re-read presets.json on the next frame (tests that write the file directly).
    pub fn reload_presets_for_test(&mut self) {
        self.hint.presets_loaded = false;
    }

    pub fn location_for_test(&self) -> Option<Value> {
        self.project.as_ref().and_then(|p| p.output_location.clone())
    }

    pub fn hint_cfg_for_test(&self) -> Option<HintCfg> {
        self.clip().map(|c| self.hint_cfg_for(c))
    }

    /// Apply a config exactly as a graph rewire does (tests).
    pub fn hint_apply_cfg_for_test(&mut self, cfg: HintCfg) {
        if let Some(c) = self.clip().cloned() {
            self.hint_set_cfg(&c, cfg);
        }
    }

    /// The Hint page's in-memory config for this clip: it is the authority while that clip is open, so the
    /// graph and the page can never disagree.
    pub(super) fn hint_cfg_for(&self, clip: &Clip) -> HintCfg {
        if self.hint.folder.as_deref() == Some(clip.folder.as_str()) {
            self.hint.cfg.clone()
        } else {
            HintCfg::load(clip)
        }
    }

    /// Adopt a config changed elsewhere (the node graph): validate, persist, keep the page in step.
    pub(super) fn hint_set_cfg(&mut self, clip: &Clip, cfg: HintCfg) {
        if self.hint.folder.as_deref() == Some(clip.folder.as_str()) {
            self.hint.cfg = cfg; // the per-frame step saves it and marks exactly what changed
            let _ = self.save_route(clip);
        } else if let Err(e) = cfg.validate().and_then(|()| cfg.save(clip)) {
            self.ai.notice = Some(format!("Hint settings not saved: {e}"));
        }
    }

    /// Called once per frame: loads this clip's saved hint state, applies viewer edits and feeds the overlay.
    pub(super) fn hint_sync(&mut self) {
        let (Some(p), Some(i), Some(v)) = (self.project.as_ref(), self.open, self.viewer.as_mut())
        else {
            self.hint.folder = None;
            return;
        };
        let clip = &p.clips[i];
        if self.tab != Tab::Hint
            || !tools_for(self.hint.cfg.guide).iter().any(|t| t.0 == v.ann.tool)
        {
            v.ann.tool = Tool::None;
        }
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            self.hint = State {
                folder: Some(clip.folder.clone()),
                ..Default::default()
            };
            self.hint.cfg = HintCfg::load(clip);
            // A clip that never saved hint settings follows the user's default tracker model.
            let want = self.settings.saved().masks.tracker_model.clone();
            if clip.native.get("hint").is_none() {
                if let Some(m) = hints::canonical_sam_model(&want) {
                    self.hint.cfg.sam_model = m.to_string();
                }
            }
            if let Some(e) = clip.native.get("delivery").and_then(|v| serde_json::from_value::<PipelineExport>(v.clone()).ok()) {
                let h = &mut self.hint;
                (h.deliveries, h.export_fps, h.export_partial, h.export_dir) = (e.outputs, e.fps, e.allow_partial, e.root);
            }
            self.hint.saved_delivery = delivery_json(&self.hint);
            self.hint.route_saved = self.hint.cfg.clone();
            self.hint.route_ov = Domain::Route.read(&clip.native);
            self.hint.route_src = clip.native.get("route_override_src").and_then(Value::as_str).map(str::to_owned);
            self.hint.delivery_ov = Domain::Delivery.read(&clip.native);
            self.hint.delivery_src = clip.native.get("delivery_override_src").and_then(Value::as_str).map(str::to_owned);
            // Edits that could not be written when this shot was last left come back, still unsaved.
            if let Some(u) = self.hint_unsaved.get(&clip.folder).cloned() {
                if let Some(r) = u.route {
                    self.hint.cfg = r;
                }
                if let Some((f, src, e)) = u.delivery {
                    let h = &mut self.hint;
                    (h.delivery_ov, h.delivery_src) = (f, src);
                    (h.deliveries, h.export_fps, h.export_partial, h.export_dir) = (e.outputs, e.fps, e.allow_partial, e.root);
                }
            }
            match Annotations::load(clip) {
                Ok(a) => self.hint.ann = a,
                Err(e) => self.hint.note = Some(e),
            }
            self.hint.ann_saved = ann_sig(&self.hint.ann);
            let left = hints::interrupted_publish(&clip.root);
            if !left.is_empty() {
                self.hint.note = Some(format!("An interrupted hint publication left {} aside; running any hint job on this clip (or clearing its lock) restores it.", left.join(", ")));
            }
            self.ai.probe_workers();
        }
        let src = stems_of(clip);
        let stem = v
            .index
            .stems
            .get(v.playhead)
            .filter(|s| src.contains(s))
            .cloned();
        // Marks as timeline ticks.
        v.marks = clip
            .keyframes
            .iter()
            .filter_map(|k| src.get(*k).and_then(|s| v.index.index_of(s)))
            .collect();
        let events = std::mem::take(&mut v.ann.events);
        if !events.is_empty() {
            let Some(stem) = stem.clone() else {
                self.hint.note =
                    Some("This frame is not a source frame; move to one to annotate.".into());
                return;
            };
            let before = self.hint.ann.clone();
            let (mut changed, mut size_err) = (false, None);
            for e in events {
                match e {
                    Ann::Pick { x, y } => {
                        let (lin, st) = (v.linear, v.straight);
                        match v.index.availability(View::Input, v.playhead, None) {
                            Availability::Present(path) => match crate::decode::decode_display(
                                &path,
                                crate::frames::view_interp(View::Input, &path, lin, st),
                            ) {
                                Ok(f) => {
                                    let (px, py) = (
                                        (x as usize).min(f.width - 1),
                                        (y as usize).min(f.height - 1),
                                    );
                                    let o = (py * f.width + px) * 4;
                                    pick_screen_colour(&mut self.hint, [f.rgba[o], f.rgba[o + 1], f.rgba[o + 2]]);
                                }
                                Err(e) => size_err = Some(format!("could not read the frame: {e}")),
                            },
                            _ => size_err = Some("no source frame on screen".into()),
                        }
                    }
                    Ann::Hold(_) | Ann::HoldErase { .. } => {}
                    other => {
                        if self.hint.ann.size.is_none() {
                            self.hint.ann.size = clip.sample_size;
                        }
                        changed |= annotate::apply(self.hint.ann.frame_mut(&stem), &other);
                    }
                }
            }
            if changed {
                self.hint.undo.push(before);
                self.hint.undo.truncate(50);
                match self.hint.ann.save(clip) {
                    Ok(()) => self.hint.note = None,
                    Err(e) => self.hint.note = Some(format!("Annotations not saved: {e}")),
                }
            }
            if let Some(e) = size_err {
                self.hint.note = Some(e);
            }
        }
        v.ann.holdout.clear();
        let want = match self.hint.overlay {
            Overlay::Off => None,
            Overlay::Guide => Some((View::Guide, [40, 200, 255])),
            Overlay::Hint => Some((View::Hint, [255, 80, 200])),
        };
        v.ann.mask = want.and_then(|(w, tint)| {
            match v.index.availability(w, v.playhead, None) {
                Availability::Present(p) => Some((p, tint)),
                _ => None,
            }
        });
        v.ann.overlay = stem
            .as_ref()
            .and_then(|s| self.hint.ann.frames.get(s).cloned())
            .unwrap_or_default();
        v.ann.preview = stem.as_ref().and_then(|s| {
            let sig = psig(&self.hint, s, v.linear);
            self.ai
                .current_preview(&clip.folder, s, sig)
                .map(|r| r.mask_png.clone())
        });
    }

    fn preview_sig(&self, stem: &str) -> u64 {
        psig(
            &self.hint,
            stem,
            self.viewer.as_ref().is_some_and(|v| v.linear),
        )
    }
}


// ------------------------------------------------------------------- project save
impl App {
    /// Unsaved Hint-page state of the open clip: the delivery config or annotations changed since last flushed.
    pub(super) fn hint_project_dirty(&self) -> bool {
        let h = &self.hint;
        h.folder.is_some()
            && (!h.deliveries.is_empty() && delivery_json(h) != h.saved_delivery || ann_sig(&h.ann) != h.ann_saved || h.cfg != h.route_saved)
            || !self.hint_unsaved.is_empty()
    }

    /// Write the open clip's delivery config and annotations to `ck_native` (called by project Save).
    pub(super) fn hint_flush_project(&mut self) -> Result<(), String> {
        let other = self.flush_unsaved(); // shots whose edits could not be written when they were left
        let Some(clip) = self.clip().cloned() else { return other };
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            return other;
        }
        self.save_route(&clip)?; // retries a route write that failed
        if !self.hint.deliveries.is_empty() {
            let json = delivery_json(&self.hint);
            if json != self.hint.saved_delivery {
                self.persist_delivery(&clip)?;
            }
        }
        if ann_sig(&self.hint.ann) != self.hint.ann_saved {
            self.hint.ann.save(&clip)?;
            self.hint.ann_saved = ann_sig(&self.hint.ann);
        }
        other
    }
}

// ------------------------------------------------------------------- route / delivery overrides

/// The route as edited, written with the markers of exactly the fields that changed since `saved`, in ONE atomic
/// update. Nothing is written (so no marker without its value, no value without its marker) if it cannot all be.
fn route_commit(clip: &Clip, saved: &HintCfg, cfg: &HintCfg, fields: &shot_domains::Fields, src: &Option<String>) -> Result<shot_domains::Fields, String> {
    cfg.validate()?;
    let mut f = fields.clone();
    for k in shot_domains::route_diff(saved, cfg) {
        f.entry(k.clone()).or_insert_with(|| Field { base: shot_domains::route_get(saved, &k), preset: src.clone() });
    }
    shot_domains::write_domain(&clip.meta_path, Domain::Route, &f, None, |n| cfg.write_into(n))?;
    Ok(f)
}

/// The delivery with its markers and source preset, one atomic update.
pub(super) fn write_delivery(meta: &Path, fields: &shot_domains::Fields, src: Option<&str>, e: &PipelineExport) -> Result<(), String> {
    let v = serde_json::to_value(e).map_err(|e| e.to_string())?;
    shot_domains::write_domain(meta, Domain::Delivery, fields, Some(src), |n| {
        n.insert("delivery".into(), v);
    })
}

impl App {
    /// The saved route changed on disk: the project's copy of the clip must follow (Batch reads it). While the
    /// pointer is down (a slider drag) this waits until it settles.
    fn refresh_after_save(&mut self, folder: &str) {
        let ctx = self.ctx_handle();
        if ctx.input(|i| i.pointer.any_down()) {
            self.hint.route_refresh = Some(folder.to_owned());
        } else {
            self.hint.route_refresh = None;
            self.refresh_clip(&ctx, folder);
        }
    }

    /// A route or delivery write for `folder` succeeded: forget what was kept in memory for it.
    fn unsaved_done(&mut self, folder: &str, route: bool) {
        if let Some(u) = self.hint_unsaved.get_mut(folder) {
            if route {
                u.route = None;
            } else {
                u.delivery = None;
            }
            if u.route.is_none() && u.delivery.is_none() {
                self.hint_unsaved.remove(folder);
            }
        }
    }

    /// Persist the route as shown. On failure the edit stays in memory (shown as unsaved, retried on the next edit
    /// or Save) and nothing is marked: a failed write never leaves a marker pointing at a value that was not saved.
    fn save_route(&mut self, clip: &Clip) -> Result<(), String> {
        if self.hint.cfg == self.hint.route_saved {
            self.hint.route_failed = None;
            return Ok(());
        }
        let cfg = self.hint.cfg.clone();
        match route_commit(clip, &self.hint.route_saved, &cfg, &self.hint.route_ov, &self.hint.route_src) {
            Ok(f) => {
                (self.hint.route_ov, self.hint.route_saved, self.hint.route_failed) = (f, cfg, None);
                self.unsaved_done(&clip.folder, true);
                self.refresh_after_save(&clip.folder);
                Ok(())
            }
            Err(e) => {
                self.hint.route_failed = Some(cfg);
                self.hint_note(format!("Hint route not saved: {e}. It stays here and is retried on your next edit or Save."));
                Err(e)
            }
        }
    }

    /// Per frame, before `hint_sync`: Batch/queue refresh requests, a shot being left with edits not yet written,
    /// route edits made by something other than a control (graph, eyedropper), the delivery once the pointer has
    /// settled, and a refresh that waited for the pointer.
    pub(super) fn hint_pending_refresh(&mut self) {
        for f in self.ai.take_refresh() {
            let ctx = self.ctx_handle();
            self.refresh_clip(&ctx, &f);
        }
        let shown = self.clip().map(|c| c.folder.clone());
        if self.hint.folder.is_some() && self.hint.folder != shown {
            self.flush_on_leave();
        }
        if let Some(c) = self.clip().cloned() {
            if self.hint.folder.as_deref() == Some(c.folder.as_str()) {
                if self.hint.cfg != self.hint.route_saved && self.hint.route_failed.as_ref() != Some(&self.hint.cfg) {
                    let _ = self.save_route(&c);
                }
                let json = delivery_json(&self.hint);
                let settled = !self.ctx_handle().input(|i| i.pointer.any_down());
                if settled && !self.hint.delivery_ov.is_empty() && !self.hint.deliveries.is_empty() && json != self.hint.saved_delivery && self.hint.delivery_failed.as_ref() != Some(&json) {
                    match self.persist_delivery(&c) {
                        Ok(()) => self.refresh_after_save(&c.folder),
                        Err(e) => self.hint_note(format!("Delivery not saved: {e}. It stays here and is retried on your next edit or Save.")),
                    }
                }
            }
        }
        if let Some(f) = self.hint.route_refresh.clone() {
            let ctx = self.ctx_handle();
            if !ctx.input(|i| i.pointer.any_down()) {
                self.hint.route_refresh = None;
                self.refresh_clip(&ctx, &f);
            }
        }
    }

    /// The shown shot is being left: write its pending route/delivery now; whatever cannot be written is kept (by
    /// shot) and comes back, still unsaved, when the shot is opened again. Never dropped.
    fn flush_on_leave(&mut self) {
        let Some(old) = self.hint.folder.clone().and_then(|f| p_clip_by_folder(self.project.as_ref(), &f)) else { return };
        let mut un = self.hint_unsaved.remove(&old.folder).unwrap_or_default();
        if self.hint.cfg != self.hint.route_saved {
            un.route = self.save_route(&old).is_err().then(|| self.hint.cfg.clone());
        }
        if !self.hint.delivery_ov.is_empty() && delivery_json(&self.hint) != self.hint.saved_delivery {
            un.delivery = match self.persist_delivery(&old) {
                Ok(()) => None,
                Err(_) => Some((self.hint.delivery_ov.clone(), self.hint.delivery_src.clone(), self.current_export())),
            };
        }
        if un.route.is_some() || un.delivery.is_some() {
            self.ai.notice = Some(format!("{}: hint route / delivery edits could not be saved. They are kept and shown as unsaved; Save retries them.", old.display_name));
            self.hint_unsaved.insert(old.folder.clone(), un);
        }
    }

    /// Retry everything kept in memory for shots that are not open (the Save button).
    fn flush_unsaved(&mut self) -> Result<(), String> {
        let mut errs = vec![];
        for folder in self.hint_unsaved.keys().cloned().collect::<Vec<_>>() {
            let Some(clip) = p_clip_by_folder(self.project.as_ref(), &folder) else { continue };
            let u = self.hint_unsaved[&folder].clone();
            if let Some(cfg) = &u.route {
                let saved = HintCfg::load(&clip);
                let src = clip.native.get("route_override_src").and_then(Value::as_str).map(str::to_owned);
                match route_commit(&clip, &saved, cfg, &Domain::Route.read(&clip.native), &src) {
                    Ok(_) => self.unsaved_done(&folder, true),
                    Err(e) => errs.push(format!("{}: hint route: {e}", clip.display_name)),
                }
            }
            if let Some((f, src, e)) = &u.delivery {
                match write_delivery(&clip.meta_path, f, src.as_deref(), e) {
                    Ok(()) => self.unsaved_done(&folder, false),
                    Err(e) => errs.push(format!("{}: delivery: {e}", clip.display_name)),
                }
            }
            let ctx = self.ctx_handle();
            self.refresh_clip(&ctx, &folder);
        }
        if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
    }

    /// Give up the shot's own route fields: each goes back to the value before its first edit.
    fn reset_route(&mut self, clip: &Clip) {
        if self.hint.route_ov.is_empty() {
            return;
        }
        let vals: Vec<(String, Value)> = self.hint.route_ov.iter().map(|(k, f)| (k.clone(), f.base.clone())).collect();
        let cfg = shot_domains::route_set(&self.hint.cfg, &vals);
        let src = shot_domains::common_preset(&self.hint.route_ov);
        match shot_domains::write_domain(&clip.meta_path, Domain::Route, &Default::default(), Some(src.as_deref()), |n| cfg.write_into(n)) {
            Ok(()) => {
                (self.hint.cfg, self.hint.route_saved, self.hint.route_failed) = (cfg.clone(), cfg, None);
                (self.hint.route_ov, self.hint.route_src) = (Default::default(), src);
                self.unsaved_done(&clip.folder, true);
                self.refresh_after_save(&clip.folder);
            }
            Err(e) => self.hint_note(format!("Hint route not reset: {e}")),
        }
    }

    fn current_export(&self) -> PipelineExport {
        let h = &self.hint;
        PipelineExport { outputs: h.deliveries.clone(), fps: h.export_fps, allow_partial: h.export_partial, root: h.export_dir.clone() }
    }

    /// Write the delivery and its field markers in ONE atomic update. On failure `saved_delivery` is untouched, so the
    /// edit stays "unsaved" (and retried) until a write succeeds.
    fn persist_delivery(&mut self, clip: &Clip) -> Result<(), String> {
        let json = delivery_json(&self.hint);
        match write_delivery(&clip.meta_path, &self.hint.delivery_ov, self.hint.delivery_src.as_deref(), &self.current_export()) {
            Ok(()) => {
                self.hint.saved_delivery = json;
                self.hint.delivery_failed = None;
                self.unsaved_done(&clip.folder, false);
                Ok(())
            }
            Err(e) => {
                self.hint.delivery_failed = Some(json);
                Err(e)
            }
        }
    }

    /// Give up the shot's own delivery fields (each output, fps, partial, folder): back to the values before the edit.
    fn reset_delivery(&mut self, clip: &Clip) {
        if self.hint.delivery_ov.is_empty() {
            return;
        }
        let vals: Vec<(String, Value)> = self.hint.delivery_ov.iter().map(|(k, x)| (k.clone(), x.base.clone())).collect();
        let e = shot_domains::export_set_all(&self.current_export(), &vals);
        let src = shot_domains::common_preset(&self.hint.delivery_ov);
        match write_delivery(&clip.meta_path, &Default::default(), src.as_deref(), &e) {
            Ok(()) => {
                let h = &mut self.hint;
                (h.deliveries, h.export_fps, h.export_partial, h.export_dir) = (e.outputs, e.fps, e.allow_partial, e.root);
                (h.delivery_ov, h.delivery_src, h.delivery_failed) = (Default::default(), src, None);
                self.hint.saved_delivery = delivery_json(&self.hint);
                self.unsaved_done(&clip.folder, false);
                self.refresh_after_save(&clip.folder);
            }
            Err(e) => self.hint_note(format!("Delivery not reset: {e}")),
        }
    }

    /// Label for the Reset button of an override: which base it really returns to.
    pub(super) fn reset_label(what: &str, f: &shot_domains::Fields) -> String {
        match shot_domains::common_preset(f) {
            Some(p) => format!("Reset {what} to preset “{p}”"),
            None => format!("Reset {what} to its values before editing"),
        }
    }

    /// The Reset button texts of the open shot (tests), `None` = nothing overridden.
    pub fn reset_labels_for_test(&self) -> (Option<String>, Option<String>) {
        let l = |what, f: &shot_domains::Fields| (!f.is_empty()).then(|| Self::reset_label(what, f));
        (l("these route fields", &self.hint.route_ov), l("these delivery fields", &self.hint.delivery_ov))
    }

    /// "This shot's own hint route fields" line with its Reset (Hint page).
    pub(super) fn route_override_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        if self.hint.route_ov.is_empty() {
            return;
        }
        let names: Vec<String> = self.hint.route_ov.keys().cloned().collect();
        let label = Self::reset_label("these route fields", &self.hint.route_ov);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Hint route, this shot's own (win over a Batch preset): {}", names.join(", "))).color(DIM).size(12.0));
            if ui.small_button(label).clicked() {
                self.reset_route(clip);
            }
        });
    }
}

// ------------------------------------------------------------------- recompute cues
impl App {
    /// Per frame (parent): promote baselines on successful jobs, recompute the stale flags, pulse on change and
    /// hand the Key/delivery flags to the settings cards.
    pub(super) fn feedback_tick(&mut self) {
        let (Some(clip), Some(ctx)) = (self.clip().cloned(), self.last_ctx.clone()) else {
            return;
        };
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            return;
        }
        let cur = Snap { cfg: self.hint.cfg.clone(), ann: ann_sig(&self.hint.ann) };
        // Key is stale when the draft differs from the params/outputs the last Key result froze into its manifest
        // (survives a reload; settings edited while a Key job runs stay pending). No result yet = Key is required.
        let key_stale = match self.project.as_ref() {
            Some(p) => {
                let d = self.ai.draft(&clip, p, self.settings.saved());
                match &clip.manifest {
                    None => true,
                    Some(m) => manifest_differs(&m.raw, &d.params, &d.out),
                }
            }
            None => false,
        };
        let manifest = sig_of(&[&format!("{:?}", clip.manifest)]);
        let key_wants_hint = self.project.as_ref().is_some_and(|p| self.ai.draft(&clip, p, self.settings.saved()).params.hint_mode != "none");
        let first = self.hint.rc.base_pre.is_none();
        let range_now = self.ai_open_range().ok();
        let products = current_products(&self.hint, clip.fps, range_now);
        if first {
            let (p, h) = prov_base(&clip, &cur);
            let rc = &mut self.hint.rc;
            (rc.base_pre, rc.base_hint) = (Some(p), Some(h));
            rc.key_manifest = Some(manifest);
        }
        // The video outputs configured at open: known, but never verified as encoded.
        if !self.hint.rc.deliver_seeded && !self.hint.deliveries.is_empty() {
            self.hint.rc.deliver_seeded = true;
            self.hint.rc.deliver_open = products.clone();
        }
        let completed: Vec<_> = self
            .ai
            .completed_tasks(&clip.folder)
            .into_iter()
            .map(|(id, kind, dir)| {
                let job = self.ai.task_job(&id);
                let at = self.ai.task_stamp(&id);
                (id, kind, dir, job, at)
            })
            .collect();
        let delivery_sig = delivery_json(&self.hint);
        // The current Key result's job, as this session's scheduler knows it: when it was dispatched and finished.
        let key_id = clip.manifest.as_ref().and_then(|m| m.raw.get("job_id")?.as_str().map(str::to_owned));
        let live_key = key_id.as_ref().and_then(|id| {
            self.ai.scheduler().snapshot().into_iter().find(|e| &e.job.job_id == id).map(|e| (e.dispatched_at, e.last_activity))
        });
        let rc = &mut self.hint.rc;
        // Evidence is remembered per job id: a rebuilt scheduler forgets the job, the result is still that job's.
        if let (Some(id), Some((d, f))) = (&key_id, live_key) {
            rc.key_seen = Some((id.clone(), d, f));
        }
        let key_job = live_key.or_else(|| rc.key_seen.as_ref().filter(|k| Some(&k.0) == key_id.as_ref()).map(|k| (k.1, k.2)));
        for (id, kind, dir, job, at) in completed {
            if !rc.seen.insert(id.clone()) {
                continue;
            }
            if kind == TaskKind::ExportVideo {
                // Only the output this job encoded, with the settings it was frozen with: another output's
                // success, or an edit made while it ran, clears nothing else.
                if let (Some((product, cfg, dest_dir)), Some(at)) = (job.as_deref().and_then(exported), at) {
                    rc.deliver_done.insert(product, ExportDone { cfg, dest_dir, at });
                }
                continue;
            }
            let is_guide = dir == hints::GUIDE_DIR;
            let is_hint = dir == hints::HINT_DIR;
            if !is_guide && !is_hint {
                continue;
            }
            // Baselines advance to what the job ran with: the snapshot frozen at its submit (Hint page), else
            // its own parameters (pipeline / Batch runs), never to whatever the controls say now.
            // A hint was made from the pre-matte that existed then, so it starts from the pre-matte baseline.
            let from = rc.base_pre.clone();
            let s = rc.inflight.remove(&id).or_else(|| Some(snap_from_job(job.as_deref()?, from.as_ref()?, is_hint)));
            if let Some(s) = s {
                if is_guide {
                    rc.base_pre = Some(s);
                } else {
                    if !s.cfg.finish.needs_guide() {
                        rc.base_pre = Some(s.clone());
                    }
                    rc.base_hint = Some(s);
                }
            }
            // The scheduler's own Done stamp, not the time this frame noticed it (a chained Key may already have
            // been dispatched by then).
            rc.hint_done_at = rc.hint_done_at.max(at);
        }
        rc.key_manifest = Some(manifest);
        // A hint is newer than the Key result unless that result's job was dispatched at or after the hint
        // finished. A Key this session does not know (older) cannot have consumed it.
        rc.hint_after_key = hint_unconsumed(rc.hint_done_at, key_job.and_then(|(disp, _)| disp));
        let (bp, bh) = (rc.base_pre.clone().unwrap(), rc.base_hint.clone().unwrap());
        let live = !cur.cfg.bypass;
        rc.pre = live && pre_diff(&cur, &bp);
        let hint_own = pre_diff(&cur, &bh)
            || cur.cfg.finish != bh.cfg.finish
            || (cur.cfg.finish == Final::Videomama && cur.cfg.videomama_chunk != bh.cfg.videomama_chunk)
            || cur.cfg.mask_cleanup.feather != bh.cfg.mask_cleanup.feather;
        // Required-but-missing output is pending too, even when nothing was ever configured differently.
        let guide_missing = live && cur.cfg.finish.needs_guide() && clip.mask_frames == 0;
        let hint_missing = live && cur.cfg.finish != Final::Import && clip.alpha_frames == 0 && key_wants_hint;
        rc.pre = rc.pre || guide_missing;
        rc.hint = live && cur.cfg.finish != Final::Import && (rc.pre || hint_own) || hint_missing;
        rc.key = rc.pre || rc.hint || rc.hint_after_key || key_stale;
        // Per configured video output: current (exported here, at these settings, from the current Key result),
        // pending (changed, never encoded after a change, or its Key result is newer) or unverified (configured
        // at open, no export seen in this session: not claimed current, not nagged either).
        let key_done = key_job.and_then(|(_, done)| done);
        let (mut pending, mut unverified) = (vec![], vec![]);
        for (k, cur) in &products {
            match rc.deliver_done.get(k) {
                Some(d) => {
                    let want = self.project.as_ref().map(|p| crate::pipeline::export_dir(&clip, &p.root, self.hint.export_dir.as_deref()).display().to_string());
                    let root_ok = want.is_none_or(|w| w == d.dest_dir);
                    if d.cfg != *cur || !root_ok {
                        pending.push(format!("{k} (settings changed since it was encoded)"));
                    } else if key_done.is_some_and(|kd| d.at <= kd) {
                        pending.push(format!("{k} (the Key result is newer than this video)"));
                    }
                }
                // Configured at open and no Key run seen since: unverified. Once a Key result of this session exists,
                // a video nobody encoded from it is not current.
                None if rc.deliver_open.get(k) == Some(cur) && key_job.is_none() => unverified.push(k.clone()),
                None if rc.deliver_open.get(k) == Some(cur) => pending.push(format!("{k} (a Key result was made since it was last verified)")),
                None => pending.push(format!("{k} (not encoded yet)")),
            }
        }
        rc.deliver = !pending.is_empty();
        rc.deliver_why = pending;
        rc.deliver_unverified = unverified;
        let now = ctx.input(|i| i.time);
        let snap = (cur, key_stale as u64, sig_of(&[&delivery_sig, &format!("{:?}", rc.deliver_done)]));
        if rc.prev.as_ref().is_some_and(|p| *p != snap) {
            rc.pulse_at = Some(now);
        }
        rc.prev = Some(snap);
        let pulse = rc.pulse_at.map_or(0.0, |t| (1.0 - (now - t) / 1.2).clamp(0.0, 1.0) as f32);
        if pulse > 0.0 {
            ctx.request_repaint();
        }
        let (k, d) = (rc.key, rc.deliver);
        self.ai.stale_key = k.then_some(pulse);
        self.ai.stale_deliver = (d || k).then_some(pulse);
    }

    /// Does this graph stage hold output made from older settings? (File presence never answers this.)
    pub(super) fn stage_pending(&self, node: Node) -> bool {
        let rc = &self.hint.rc;
        match node {
            Node::Media => false,
            Node::PreMatte => rc.pre,
            Node::Hint => rc.hint,
            Node::Key => rc.key,
            Node::Out(_) => rc.deliver || rc.key,
        }
    }

    fn pulse(&self, ctx: &egui::Context) -> f32 {
        self.hint.rc.pulse_at.map_or(0.0, |t| (1.0 - (ctx.input(|i| i.time) - t) / 1.2).clamp(0.0, 1.0) as f32)
    }

    fn stale(&self, ctx: &egui::Context, node: Node) -> Option<f32> {
        self.stage_pending(node).then(|| self.pulse(ctx))
    }

    /// A processing button for `stage`: `primary` fills it with the accent; a stale stage gets the amber outline.
    pub(super) fn action_button(&self, ui: &mut Ui, label: &str, stage: Node, primary: bool, enabled: bool) -> egui::Response {
        let b = if primary {
            egui::Button::new(RichText::new(label).color(Color32::from_rgb(21, 36, 26))).fill(theme::ACCENT)
        } else {
            egui::Button::new(label).fill(theme::BUTTON)
        };
        let r = ui.add_enabled(enabled, b);
        if let Some(p) = self.stale(ui.ctx(), stage) {
            ui.painter().rect_stroke(r.rect.expand(1.0), 4.0, egui::Stroke::new(2.0 + p * 1.5, theme::FOCUS), egui::StrokeKind::Outside);
        }
        r
    }
}

impl App {
    pub(super) fn open_clip_snapshot(&self) -> Option<Clip> {
        self.clip().cloned()
    }

    fn submit_task_job(
        &mut self,
        clip: &Clip,
        job: Result<crate::tasks::TaskJob, String>,
        sig: u64,
        what: &str,
    ) {
        match job.and_then(|j| self.ai.submit_task(clip, j, vec![], sig)) {
            Ok(_) => self.hint.note = Some(format!("{what} queued.")),
            Err(e) => self.hint_note(format!("{what} not queued: {e}")),
        }
    }

    fn hint_preview(&mut self) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        let Some(stem) = self
            .viewer
            .as_ref()
            .and_then(|v| v.index.stems.get(v.playhead).cloned())
        else {
            return;
        };
        let stems = stems_of(&clip);
        if !stems[range.0..=range.1].contains(&stem) {
            return self.hint_note("Preview a frame inside In/Out (or clear In/Out).");
        }
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        let sig = self.preview_sig(&stem);
        let job = Ctx {
            clip: &clip,
            range,
            job_id: new_id(),
            staging: hints::staging_root(
                &self.project.as_ref().unwrap().root,
                &clip.root,
                &clip.folder,
            ),
        }
        .sam_preview(&self.hint.cfg, &self.hint.ann, &stem, lin);
        self.submit_task_job(&clip, job, sig, "SAM preview");
    }

    /// Queue a hint/pre-matte run from outside this page (the node graph's context menu), by node meaning:
    /// `pre_matte` = the guide producer, otherwise the final hint (`marks` = only the marked frames).
    pub(super) fn hint_run_from_graph(&mut self, pre_matte: bool, marks: bool) {
        let what = if pre_matte {
            if marks { Run::GuideMarks } else { Run::Guide }
        } else if marks {
            Run::HintMarks
        } else {
            Run::Hint
        };
        self.hint_run(what);
    }

    /// Which hint step to queue.
    fn hint_run(&mut self, what: Run) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        // An explicit action uses (and saves) the choices shown, even before any setting was touched.
        if clip.native.get("hint").is_none() {
            if let Err(e) = self
                .hint
                .cfg
                .validate()
                .and_then(|()| self.hint.cfg.save(&clip))
            {
                return self.hint_note(format!("Hint settings not saved: {e}"));
            }
        }
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        let root = self.project.as_ref().unwrap().root.clone();
        let staging = hints::staging_root(&root, &clip.root, &clip.folder);
        let g = hints::Gen {
            clip: &clip,
            cfg: &self.hint.cfg,
            ann: Some(&self.hint.ann),
            linear: lin,
            staging: &staging,
        };
        let marks: Vec<usize> = clip
            .keyframes
            .iter()
            .copied()
            .filter(|k| (range.0..=range.1).contains(k))
            .collect();
        let mut id = new_id;
        let (steps, label) = match what {
            Run::Guide => (g.guide(range, &mut id).map(|j| vec![j]), "Pre-matte"),
            Run::Hint => (g.hint(range, false, &mut id), "Hint"),
            Run::GuideMarks => (
                marks
                    .iter()
                    .map(|k| g.guide((*k, *k), &mut id))
                    .collect::<Result<Vec<_>, _>>()
                    .and_then(|v| {
                        if v.is_empty() {
                            Err("no keyframe marks inside In/Out: mark frames first".into())
                        } else {
                            Ok(v)
                        }
                    }),
                "Pre-matte on marked frames",
            ),
            Run::HintMarks => (g.keyframes(&marks, true, &mut id), "Hint on marked frames"),
        };
        let marks_only = matches!(what, Run::GuideMarks | Run::HintMarks);
        let before = self.ai.task_ids(&clip.folder);
        match steps.and_then(|s| self.ai.submit_chain(&clip, s, None)) {
            Ok(_) => {
                // Frozen settings of a whole-range run: they become the baseline only if the job completes.
                if !marks_only {
                    let snap = Snap { cfg: self.hint.cfg.clone(), ann: ann_sig(&self.hint.ann) };
                    for id in self.ai.task_ids(&clip.folder).into_iter().filter(|i| !before.contains(i)) {
                        self.hint.rc.inflight.insert(id, snap.clone());
                    }
                }
                // Generating is the intent to use what was generated: a clip that never chose a hint mode must
                // not go on to Key reading nothing.
                self.ai.note_generated_hints(&clip, marks_only);
                self.hint.note = Some(format!("{label} queued."));
            }
            Err(e) => self.hint_note(format!("Not queued: {e}")),
        }
    }

    fn hint_marks(&mut self, add: Option<bool>) {
        let (Some(p), Some(i), Some(v)) = (self.project.as_ref(), self.open, self.viewer.as_ref())
        else {
            return;
        };
        let clip = &p.clips[i];
        let stems = stems_of(clip);
        let Some(idx) = v
            .index
            .stems
            .get(v.playhead)
            .and_then(|s| stems.iter().position(|x| x == s))
        else {
            return self.hint_note("This frame is not a source frame.");
        };
        let mut marks = clip.keyframes.clone();
        match add {
            Some(true) => marks.push(idx),
            Some(false) => marks.retain(|m| *m != idx),
            None => marks.clear(),
        }
        let folder = clip.folder.clone();
        match crate::persist::save_keyframes(&clip.root, &marks, &stems) {
            Ok(()) => {
                self.hint.note = None;
                let ctx = self.ctx_handle();
                self.refresh_clip(&ctx, &folder);
            }
            Err(e) => self.hint_note(format!("Marks not saved: {e}")),
        }
    }

    fn ctx_handle(&self) -> egui::Context {
        self.last_ctx.clone().unwrap_or_default()
    }

    /// A dialog result: `movie` = companion alpha/mask movie, else the export folder.
    pub(super) fn hint_picked(&mut self, path: PathBuf, movie: bool) {
        if movie {
            self.hint.movie = Some(path);
        } else {
            self.hint.export_dir = Some(path);
        }
    }

    fn source_movie(clip: &Clip) -> Option<PathBuf> {
        match &clip.source {
            Source::Video { path } => Some(path.clone()),
            _ => std::fs::read_dir(clip.root.join("Source"))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| crate::project::VIDEO_EXTS.contains(&ext_of(p).as_str())),
        }
    }

    fn undo_annotation(&mut self) {
        if let (Some(prev), Some(c)) = (self.hint.undo.pop(), self.open_clip_snapshot()) {
            self.hint.ann = prev;
            if let Err(e) = self.hint.ann.save(&c) {
                self.hint_note(format!("Annotations not saved: {e}"));
            }
        }
    }

    // ------------------------------------------------------------------- tool strip (beside the viewer)

    /// Vertical, Photoshop-style tool strip left of the viewer (Hint page). Icons are drawn with primitives so
    /// nothing depends on an icon font; every button has a tooltip and a selected state. Prompts (points, paint,
    /// erase, box) sit in the first group, Undo/Clear in a second group. Processing (SAM Preview/Track) is in the bottom
    /// action area and the brush size in Properties. Chroma gets exactly one tool: pick the screen colour. The holdout tools are not part
    /// of SAM's prompt set and never appear here.
    pub(super) fn hint_tool_strip(&mut self, ui: &mut Ui) {
        let Some(clip) = self.open_clip_snapshot() else {
            return;
        };
        if self.tab != Tab::Hint {
            return;
        }
        let guide = self.hint.cfg.guide;
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        let stem_now = v.index.stems.get(v.playhead).cloned();
        let tools = tools_for(guide);
        self.hint.strip_rects.clear();
        if tools.is_empty() {
            return; // automatic producers take no prompts: no empty column beside the viewer
        }
        let (mut undo, mut clr_frame, mut clr_all) = (false, false, false);
        let strip_h = ui.available_height();
        egui::Frame::new()
            .fill(ui.visuals().faint_bg_color)
            .corner_radius(5.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(STRIP_W - 8.0);
                ui.set_min_height((strip_h - 8.0).max(0.0));
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.vertical_centered(|ui| {
                    for (t, _short, tip) in tools {
                        let icon = icon_for(*t);
                        let r = strip_button(ui, v.ann.tool == *t, icon, tip);
                        self.hint.strip_rects.push((*t, r.rect));
                        if r.clicked() {
                            v.ann.tool = if v.ann.tool == *t { Tool::None } else { *t };
                            if v.ann.tool != Tool::None {
                                v.view = View::Input; // prompts are stored in source pixels of Input
                                v.ab = false;
                            }
                        }
                    }
                    if guide == Producer::Sam {
                        ui.add_space(4.0);
                        ui.separator();
                        undo = strip_button(ui, false, Icon::Undo, "Undo the last prompt edit").clicked();
                        clr_frame = strip_button(ui, false, Icon::ClearFrame, "Clear this frame's prompts").clicked();
                        clr_all = strip_button(
                            ui,
                            false,
                            Icon::ClearAll,
                            "Clear all SAM prompts (source and generated masks are untouched)",
                        )
                        .clicked();
                    }
                });
            });
        if undo {
            self.undo_annotation();
        }
        if clr_frame || clr_all {
            self.hint.undo.push(self.hint.ann.clone());
            match (clr_all, stem_now) {
                (true, _) => self.hint.ann.frames.clear(),
                (false, Some(s)) => {
                    self.hint.ann.frames.remove(&s);
                }
                _ => {}
            }
            if let Err(e) = self.hint.ann.save(&clip) {
                self.hint_note(format!("Annotations not saved: {e}"));
            }
        }
    }

    // ------------------------------------------------------------------- page bar (under the viewer)

    /// The Hint page's processing actions, one group per module in the shared action area: Pre-matte | Final hint,
    /// then Source / Import when there is something to prepare. The inspector only chooses; every run starts here.
    pub(super) fn hint_actions(&mut self, ui: &mut Ui, clip: &Clip) {
        let local = self.ai.local_caps();
        let ready = |k: TaskKind| local.as_ref().is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false));
        let busy = self.ai.task_busy(&clip.folder);
        let (guide, fin) = (self.hint.cfg.guide, self.hint.cfg.finish);
        let guide_ready = ready(guide.task());
        let fin_ready = fin.task().map_or(fin == Final::Direct && guide_ready, ready);
        let bypass = self.hint.cfg.bypass;
        let why = |b: bool, what: &str| -> String {
            if bypass {
                "Bypass is on".into()
            } else if busy {
                "A hint job is running for this clip".into()
            } else if b {
                String::new()
            } else {
                reason(&local, what)
            }
        };
        let sam = guide == Producer::Sam;
        let tool_on = sam && self.viewer.as_ref().is_some_and(|v| v.ann.tool != Tool::None);
        let sam_ok = ready(TaskKind::SamPreview) && ready(TaskKind::SamTrack);
        let annotated = self
            .viewer
            .as_ref()
            .and_then(|v| v.index.stems.get(v.playhead))
            .and_then(|s| self.hint.ann.frames.get(s))
            .is_some_and(|f| !f.is_empty());
        let src = Self::source_movie(clip).filter(|_| !matches!(clip.source, Source::Sequence { .. }));
        let movie = self.hint.movie.clone();
        let import_ok = ready(TaskKind::ImportHint) && src.is_some();
        let ctx = ui.ctx().clone();
        let (mut run, mut preview, mut extract, mut import, mut cancel_import) = (None, false, false, false, false);
        let this = &*self;
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            let stale = this.stale(&ctx, Node::PreMatte);
            let r = ui.scope(|ui| {
                card(ui, &format!("Pre-matte · {}", guide.label()), stale, |ui| {
                    let on = guide_ready && !busy && !bypass;
                    ui.horizontal_wrapped(|ui| {
                        if sam {
                            // SAM preview/tracking belong to SAM only: Track is the pre-matte run, so no second Generate.
                            let tip = if !sam_ok {
                                reason(&local, "SAM")
                            } else if busy {
                                "A hint job is running for this clip".into()
                            } else if !annotated {
                                "Add a point, box or paint on this frame first".into()
                            } else {
                                "Run SAM on this frame's prompts (nothing is written)".into()
                            };
                            preview = this.action_button(ui, "Preview frame", Node::PreMatte, false, sam_ok && annotated && !busy).on_hover_text(tip.clone()).on_disabled_hover_text(tip).clicked();
                            let tip = if bypass { "Bypass is on".to_string() } else if busy { "A hint job is running for this clip".to_string() } else { reason(&local, "SAM") };
                            if this.action_button(ui, "Track prompts", Node::PreMatte, true, sam_ok && !busy && !bypass).on_hover_text("Track the saved prompts over In/Out into the pre-matte").on_disabled_hover_text(tip).clicked() {
                                run = Some(Run::Guide);
                            }
                        } else if this.action_button(ui, "Generate pre-matte", Node::PreMatte, true, on).on_hover_text("Run the Pre-matte over In/Out into VideoMamaMaskHint").on_disabled_hover_text(why(guide_ready, guide.label())).clicked() {
                            run = Some(Run::Guide);
                        }
                        ui.menu_button("More…", |ui| {
                            if ui.add_enabled(on, egui::Button::new("On marked frames")).on_hover_text("One single-frame Pre-matte per keyframe mark").on_disabled_hover_text(why(guide_ready, guide.label())).clicked() {
                                run = Some(Run::GuideMarks);
                                ui.close();
                            }
                        });
                    });
                    ui.label(RichText::new(format!("{} frames on disk", clip.mask_frames)).color(DIM).size(12.0));
                });
            });
            if tool_on {
                ui.painter().rect_stroke(r.response.rect, 6.0, egui::Stroke::new(2.0, theme::CHECKED), egui::StrokeKind::Outside);
            }
            let ui = &mut cols[1];
            let stale = this.stale(&ctx, Node::Hint);
            card(ui, &format!("Hint · {}", fin.label()), stale, |ui| {
                let on = fin_ready && !busy && !bypass && fin != Final::Import;
                let tip = if fin == Final::Import { "Imported alpha has nothing to generate: choose a movie in Properties".to_string() } else { why(fin_ready, fin.label()) };
                ui.horizontal_wrapped(|ui| {
                    if this.action_button(ui, "Generate hint", Node::Hint, true, on).on_hover_text(if fin.needs_guide() { "Refine the Pre-matte over In/Out into AlphaHint (needs the Pre-matte first)" } else { "Publish the Pre-matte straight into AlphaHint" }).on_disabled_hover_text(tip.clone()).clicked() {
                        run = Some(Run::Hint);
                    }
                    ui.menu_button("More…", |ui| {
                        if ui.add_enabled(on, egui::Button::new("On marked frames")).on_hover_text("Real hints on each keyframe mark, for Key's keyframe mode").on_disabled_hover_text(tip).clicked() {
                            run = Some(Run::HintMarks);
                            ui.close();
                        }
                    });
                });
                ui.label(RichText::new(format!("{} frames on disk", clip.alpha_frames)).color(DIM).size(12.0));
            });
        });
        if src.is_some() || movie.is_some() {
            card(ui, "Source / Import", None, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if movie.is_some() {
                        let tip = if src.is_none() { "Companion movies need a movie source (Source/); import PNG/EXR sequences by placing them in AlphaHint" } else { "the worker cannot import movies (FFmpeg)" };
                        import = ui.add_enabled(import_ok && !busy, egui::Button::new(RichText::new("Import movie").color(Color32::from_rgb(21, 36, 26))).fill(theme::ACCENT)).on_disabled_hover_text(tip).clicked();
                        cancel_import = ui.button("Cancel").clicked();
                        if let Some(m) = &movie {
                            ui.label(RichText::new(m.display().to_string()).monospace().size(11.0).color(DIM));
                        }
                    }
                    if src.is_some() {
                        extract = ui.add_enabled(ready(TaskKind::PrepareSource) && !busy, egui::Button::new("Extract frames (validated)")).on_hover_text("Copies Source/ movie into a verified Frames/ sequence; cancellable and resumable").clicked();
                    }
                });
            });
        }
        if preview {
            self.hint_preview();
        }
        if extract {
            if let Some(src) = src.clone() {
                let job = Ctx { clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.prepare_source(&src);
                let c = clip.clone();
                self.submit_task_job(&c, Ok(job), 0, "Frame extraction");
            }
        }
        if import {
            if let (Some(m), Some(src)) = (movie, src) {
                let job = Ctx { clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.import_hint(&m, &src, self.hint.movie_mask, self.hint.movie_alpha_channel);
                let c = clip.clone();
                self.submit_task_job(&c, Ok(job), 0, "Hint movie import");
                self.hint.movie = None;
            }
        }
        if cancel_import {
            self.hint.movie = None;
        }
        if let Some(r) = run {
            self.hint_run(r);
        }
    }

    /// Status region (left of the action region): job progress, marks, overlay and notes. The generate buttons
    /// live in the action region (`hint_actions`).
    pub(super) fn hint_tools(&mut self, ui: &mut Ui, clip: &Clip) {
        let guide = self.hint.cfg.guide;
        let mut marks = None;
        // A job shows once: the Pre-matte route lists the guide's task plus SAM tracking, which is the same
        // kind when the guide is SAM, and a job never appears under both headings.
        let mut seen = vec![];
        self.hint_progress(ui, clip, &[guide.task(), TaskKind::SamTrack], hints::GUIDE_DIR, &mut seen);
        self.hint_progress(
            ui,
            clip,
            &[
                TaskKind::Gvm,
                TaskKind::Birefnet,
                TaskKind::Chroma,
                TaskKind::SamTrack,
                TaskKind::Matanyone2,
                TaskKind::Videomama,
                TaskKind::ImportHint,
                TaskKind::PrepareSource,
                TaskKind::PrematteHint,
            ],
            hints::HINT_DIR,
            &mut seen,
        );
        ui.label(
            RichText::new("Pre-matte and hint generation run on this computer (the local worker). Key then runs on the client assigned to this clip.")
                .color(DIM)
                .size(11.5),
        );
        ui.horizontal_wrapped(|ui| {
            if ui.button("Mark frame").on_hover_text("Add a sparse keyframe mark here").clicked() {
                marks = Some(Some(true));
            }
            if ui.button("Unmark").clicked() {
                marks = Some(Some(false));
            }
            if ui.button("Clear marks").on_hover_text("Removes only the keyframe marks, never hint frames").clicked() {
                marks = Some(None);
            }
            ui.label(RichText::new(if clip.keyframes.is_empty() { "marks: none".into() } else { format!("marks {:?}", clip.keyframes) }).color(DIM).size(12.0));
            ui.separator();
            ui.label("Overlay");
            for (o, l) in [(Overlay::Off, "Off"), (Overlay::Guide, "Pre-matte"), (Overlay::Hint, "Hint")] {
                ui.selectable_value(&mut self.hint.overlay, o, l);
            }
            ui.label(RichText::new("grayscale: Guide/Hint view chips").color(DIM).size(12.0));
        });
        if let Some(m) = marks {
            self.hint_marks(m);
        }
        self.ai.hint_lock_ui(ui, clip);
        if let Some(r) = self.ai.latest_task(&clip.folder, TaskKind::SamPreview) {
            ui.label(RichText::new("Last preview:").color(DIM).size(12.0));
            self.ai.task_status_row(ui, &r);
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(WARN).size(12.0));
        }
    }

    /// Latest task of each kind, beside the control that started it.
    /// Progress of the jobs that actually publish into `dir`, whatever their kind: the Pre-matte column shows
    /// `VideoMamaMaskHint` jobs and the Hint column `AlphaHint` jobs, so a SAM tracking run that published the
    /// final hint never appears under the pre-matte heading (and the other way round).
    fn hint_progress(&mut self, ui: &mut Ui, clip: &Clip, kinds: &[TaskKind], dir: &str, seen: &mut Vec<String>) {
        for k in kinds {
            if let Some(r) = self.ai.latest_task_in(&clip.folder, *k, dir) {
                if !seen.contains(&r) {
                    self.ai.task_status_row(ui, &r);
                    seen.push(r);
                }
            }
        }
    }

    // ------------------------------------------------------------------- properties

    pub(super) fn hint_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let caps = self.ai.local_caps();
        let cfg_before = self.hint.cfg.clone();
        let ctx = ui.ctx().clone();
        let (pre_stale, hint_stale) = (self.stale(&ctx, Node::PreMatte), self.stale(&ctx, Node::Hint));
        let bypass = theme::checkbox(ui, &mut self.hint.cfg.bypass, "Bypass hint (keeps every setting and asset)");
        self.hint.preset_rects.push(("bypass", bypass.rect));
        ui.add_space(4.0);
        let ok = |k: TaskKind| caps.as_ref().is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false));
        ui.add_enabled_ui(!self.hint.cfg.bypass, |ui| {
            card(ui, "Pre-matte", pre_stale, |ui| {
                pair(ui, "Producer", |ui| {
                    egui::ComboBox::from_id_salt("hint_guide").selected_text(self.hint.cfg.guide.label()).show_ui(ui, |ui| {
                        for p in Producer::ALL {
                            let l = if ok(p.task()) { p.label().to_string() } else { format!("{} (not ready)", p.label()) };
                            ui.selectable_value(&mut self.hint.cfg.guide, p, l);
                        }
                    });
                });
                match self.hint.cfg.guide {
                    Producer::Birefnet => {
                        let have = caps.as_ref().map(|c| c.birefnet_variants.clone()).unwrap_or_default();
                        pair(ui, "Variant", |ui| {
                            egui::ComboBox::from_id_salt("birefnet_variant").selected_text(self.hint.cfg.birefnet_variant.clone()).show_ui(ui, |ui| {
                                for v in BIREFNET_VARIANTS {
                                    let l = if have.iter().any(|h| h == v) { v.to_string() } else { format!("{v} (not installed)") };
                                    ui.selectable_value(&mut self.hint.cfg.birefnet_variant, v.to_string(), l);
                                }
                            });
                        });
                    }
                    Producer::Chroma => self.chroma_controls(ui, clip),
                    Producer::Sam => {
                        let sam = caps.as_ref().map(|c| c.sam_models.clone()).unwrap_or_default();
                        pair(ui, "Tracker model", |ui| {
                            egui::ComboBox::from_id_salt("sam_model").selected_text(SAM_MODELS.iter().find(|m| m.0 == self.hint.cfg.sam_model).map_or("Select model", |m| m.1)).show_ui(ui, |ui| {
                                for (id, l) in SAM_MODELS {
                                    let t = if sam.iter().any(|s| s == id) { l.to_string() } else { format!("{l} (not installed)") };
                                    ui.selectable_value(&mut self.hint.cfg.sam_model, id.to_string(), t);
                                }
                            });
                        });
                    }
                    Producer::Gvm => {}
                }
            });
            ui.add_space(6.0);
            card(ui, "Final hint", hint_stale, |ui| {
                pair(ui, "Method", |ui| {
                    egui::ComboBox::from_id_salt("hint_final").selected_text(self.hint.cfg.finish.label()).show_ui(ui, |ui| {
                        for f in Final::ALL {
                            let l = if f.task().is_none_or(ok) { f.label().to_string() } else { format!("{} (not ready)", f.label()) };
                            ui.selectable_value(&mut self.hint.cfg.finish, f, l);
                        }
                    });
                });
                match self.hint.cfg.finish {
                    Final::Videomama => {
                        let mut c = self.hint.cfg.videomama_chunk as i32;
                        pair(ui, "Chunk size", |ui| {
                            fill_slider(ui);
                            ui.add(egui::Slider::new(&mut c, 1..=64));
                        });
                        self.hint.cfg.videomama_chunk = c as u32;
                        ui.label(RichText::new("Refines the Pre-matte: generate it over In/Out first.").color(DIM).size(12.0));
                    }
                    Final::Matanyone2 => {
                        ui.label(RichText::new("Refines the Pre-matte, seeded from its In frame.").color(DIM).size(12.0));
                    }
                    Final::Direct | Final::Import => {}
                }
            });
        });
        if self.hint.cfg != cfg_before {
            let _ = self.save_route(clip);
        }
        self.route_override_ui(ui, clip);
        ui.add_space(6.0);
        card(ui, "Import source", None, |ui| {
            pair(ui, "Companion movie", |ui| {
                if ui.button("Choose alpha / mask movie…").clicked() {
                    let ctx = ui.ctx().clone();
                    self.request_pick(&ctx, PickKind::HintMovie);
                }
            });
            if let Some(m) = self.hint.movie.clone() {
                ui.label(RichText::new(m.display().to_string()).monospace().size(11.0));
                pair(ui, "Imports as", |ui| {
                    ui.radio_value(&mut self.hint.movie_mask, false, "AlphaHint");
                    ui.radio_value(&mut self.hint.movie_mask, true, "Pre-matte");
                });
                pair(ui, "Matte channel", |ui| {
                    theme::checkbox(ui, &mut self.hint.movie_alpha_channel, "In the alpha channel (else gray)");
                });
                ui.label(RichText::new("Run Import from the Source / Import group in the action area.").color(DIM).size(12.0));
            }
        });
        if !self.hint.cfg.bypass {
            ui.add_space(4.0);
            ui.label(RichText::new("Consumption mode and quality are below (applied by Key).").color(DIM).size(12.0));
        }
    }

    /// Brush size and the mask cleanup shared by every producer/refiner (after the mode controls).
    pub(super) fn hint_cleanup_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let cfg_before = self.hint.cfg.clone();
        let stale = self.stale(&ui.ctx().clone(), Node::Hint);
        ui.add_space(6.0);
        if tools_for(self.hint.cfg.guide).iter().any(|t| !matches!(t.0, Tool::Pick | Tool::Box)) {
            if let Some(v) = self.viewer.as_mut() {
                card(ui, "Brush", None, |ui| {
                    let mut b = v.ann.brush as f32;
                    pair(ui, "Size", |ui| {
                        fill_slider(ui);
                        ui.add(egui::Slider::new(&mut b, 2.0..=120.0).suffix(" px")).on_hover_text("Size of points, paint and erase strokes");
                    });
                    v.ann.brush = b as f64;
                });
                ui.add_space(6.0);
            }
        }
        let cleanup_note = if self.hint.cfg.finish.needs_guide() {
            "Grow / fill holes: regenerate pre-matte. Feather: regenerate hint. Guides stay binary."
        } else {
            "Regenerate to apply changes. Feather softens the final hint; guides stay binary."
        };
        card(ui, "Mask cleanup", stale, |ui| {
            ui.label(RichText::new(cleanup_note).color(DIM).size(12.0));
            let mc = &mut self.hint.cfg.mask_cleanup;
            pair(ui, "Grow / shrink", |ui| {
                fill_slider(ui);
                ui.add(egui::Slider::new(&mut mc.grow, -100..=100).suffix(" px"));
            });
            pair(ui, "Fill holes", |ui| {
                theme::checkbox(ui, &mut mc.fill_holes, "Fill holes");
            });
            pair(ui, "Feather", |ui| {
                fill_slider(ui);
                ui.add(egui::Slider::new(&mut mc.feather, 0.0..=100.0).suffix(" px"));
            });
        });
        if self.hint.cfg != cfg_before {
            let _ = self.save_route(clip);
        }
    }

    fn chroma_controls(&mut self, ui: &mut Ui, clip: &Clip) {
        let c = &mut self.hint.cfg.chroma;
        pair(ui, "Screen", |ui| {
            ui.selectable_value(&mut c.screen_type, "green".to_string(), "Green");
            ui.selectable_value(&mut c.screen_type, "blue".to_string(), "Blue");
        });
        match c.screen_color {
            Some(rgb) => {
                pair(ui, "Screen colour", |ui| {
                    ui.label(format!("{:?}", rgb));
                    if ui.small_button("auto").clicked() {
                        c.screen_color = None;
                    }
                });
            }
            None => {
                ui.label(RichText::new("Screen colour: automatic. Use the “Pick colour” tool on the frame to sample one.").color(DIM).size(12.0));
            }
        }
        pair(ui, "Strength", |ui| {
            fill_slider(ui);
            ui.add(egui::Slider::new(&mut c.strength, 0.1..=10.0));
        });
        pair(ui, "Clip black", |ui| {
            fill_slider(ui);
            ui.add(egui::Slider::new(&mut c.clip_black, 0.0..=1.0));
        });
        pair(ui, "Clip white", |ui| {
            fill_slider(ui);
            ui.add(egui::Slider::new(&mut c.clip_white, 0.0..=1.0));
        });
        pair(ui, "Shrink / grow", |ui| {
            fill_slider(ui);
            ui.add(egui::Slider::new(&mut c.shrink_grow, -100..=100).suffix(" px"));
        });
        pair(ui, "Edge blur", |ui| {
            fill_slider(ui);
            ui.add(egui::Slider::new(&mut c.edge_blur, 0..=100).suffix(" px"));
        });
        // A holdout drawn in the old editor is still read by the generator. It is not a tool here (the user
        // wants only the picker), but it must not be invisible: show it and let it be cleared.
        match crate::hints::Holdout::load(clip) {
            Ok(h) if !h.strokes.is_empty() => {
                ui.separator();
                let (fg, bg) = (
                    h.strokes.iter().filter(|s| s.fg).count(),
                    h.strokes.iter().filter(|s| !s.fg).count(),
                );
                ui.label(
                    RichText::new(format!(
                        "Existing chroma holdout: {fg} foreground, {bg} background stroke(s) from the old editor ({})",
                        crate::hints::HOLDOUT_FILE
                    ))
                    .color(DIM)
                    .size(11.5),
                );
                if ui
                    .small_button("Clear existing holdout…")
                    .on_hover_text("Deletes the stored chroma holdout strokes for this clip. Hints and prompts are untouched.")
                    .clicked()
                {
                    match crate::hints::Holdout::default().save(clip) {
                        Ok(()) => {
                            self.hint_note("Chroma holdout cleared.");
                        }
                        Err(e) => self.hint_note(format!("Holdout not cleared: {e}")),
                    }
                }
            }
            Ok(_) => {}
            Err(e) => {
                ui.colored_label(
                    WARN,
                    RichText::new(format!("Existing chroma holdout unreadable: {e}"))
                        .size(11.5),
                );
            }
        }
    }

    // ------------------------------------------------------------------- Deliver: per-output delivery

    /// Keep one delivery per ENABLED output (new ones are sequences, disabled ones vanish, video choices stay).
    fn sync_deliveries(&mut self, clip: &Clip) {
        let Some(pr) = self.project.as_ref() else { return };
        let o = self.ai.draft(clip, pr, self.settings.saved()).out.clone();
        let mut e = PipelineExport { outputs: std::mem::take(&mut self.hint.deliveries), ..Default::default() };
        e.sync_outputs(&o);
        self.hint.deliveries = e.outputs;
    }

    /// One card per enabled output: sequence (format) or video (file type, codec, quality), chosen independently.
    pub(super) fn deliver_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        self.sync_deliveries(clip);
        let export_before = self.current_export();
        let stale = self.stale(&ui.ctx().clone(), Node::Out(View::Fg));
        let Some(pr) = self.project.as_ref() else { return };
        let dr = self.ai.draft(clip, pr, self.settings.saved());
        let snap = dr.snapshot();
        let o = &mut dr.out;
        if self.hint.deliveries.is_empty() {
            ui.label(RichText::new("No output is enabled in the Key outputs.").color(DIM));
            return;
        }
        self.hint.deliver_rects.clear();
        let mut fmt_rects: Vec<(String, egui::Rect)> = vec![];
        for key in ["fg", "matte", "comp", "processed"].map(str::to_string) {
            let Some(d) = self.hint.deliveries.get_mut(&key) else { continue };
            let title = match key.as_str() {
                "fg" => "FG (foreground)",
                "matte" => "Matte",
                "comp" => "Comp",
                _ => "Processed (RGBA)",
            };
            let fmt = match key.as_str() {
                "fg" => &mut o.fg_format,
                "matte" => &mut o.matte_format,
                "comp" => &mut o.comp_format,
                _ => &mut o.processed_format,
            };
            let card = ui.scope(|ui| card(ui, title, stale, |ui| {
                let is_video = matches!(d, Delivery::Video(_));
                pair(ui, "Deliver as", |ui| {
                    if ui.selectable_label(!is_video, "Image sequence").clicked() {
                        *d = Delivery::Sequence;
                    }
                    let v = ui.selectable_label(is_video, "Video file");
                    fmt_rects.push((format!("{key}_video"), v.rect));
                    if v.clicked() && !is_video {
                        *d = Delivery::Video(VideoDelivery::default_for("mp4"));
                    }
                });
                match d {
                    Delivery::Sequence => {
                        pair(ui, "Format", |ui| {
                            let e = ui.selectable_value(fmt, "exr".to_string(), "EXR");
                            let p = ui.selectable_value(fmt, "png".to_string(), "PNG");
                            fmt_rects.push((format!("{key}_exr"), e.rect));
                            fmt_rects.push((format!("{key}_png"), p.rect));
                        });
                    }
                    Delivery::Video(v) => {
                        if !VIDEO_CODECS.iter().any(|c| c.container == v.container && c.id == v.codec) {
                            *v = VideoDelivery::default_for("mp4");
                        }
                        pair(ui, "File type", |ui| {
                            for (id, l) in [("mp4", "MP4"), ("mov_prores4444", "MOV"), ("webm_alpha", "WebM")] {
                                if ui.selectable_label(v.container == id, l).clicked() && v.container != id {
                                    *v = VideoDelivery::default_for(id);
                                }
                            }
                        });
                        let cur = VIDEO_CODECS.iter().find(|c| c.container == v.container && c.id == v.codec).unwrap();
                        pair(ui, "Codec", |ui| {
                            egui::ComboBox::from_id_salt(("codec", key.as_str()))
                                .width(ui.available_width()).truncate()
                                .selected_text(codec_text(cur))
                                .show_ui(ui, |ui| {
                                    for c in VIDEO_CODECS.iter().filter(|c| c.container == v.container) {
                                        if ui.selectable_label(c.id == v.codec, codec_text(c)).clicked() && c.id != v.codec {
                                            (v.codec, v.quality) = (c.id.into(), c.default);
                                        }
                                    }
                                });
                        });
                        let name = match cur.quality {
                            QualityKind::Crf => "CRF",
                            QualityKind::Qscale => "qscale",
                        };
                        let note = format!("{name} {}-{}: lower = better quality, bigger file", cur.min, cur.max);
                        pair(ui, "Quality", |ui| {
                            ui.spacing_mut().slider_width = (ui.available_width() - 100.0).max(40.0);
                            ui.add(egui::Slider::new(&mut v.quality, cur.min..=cur.max).text(name)).on_hover_text(&note);
                        });
                        ui.label(RichText::new(note).color(DIM).size(11.5));
                        if key == "processed" && !cur.alpha {
                            ui.colored_label(WARN, "This codec has no alpha: transparency is not retained. Use MOV ProRes 4444 or WebM VP9 to keep it.");
                        }
                    }
                }
            }));
            self.hint.deliver_rects.push((key.clone(), card.response.rect));
            ui.add_space(6.0);
        }
        card(ui, "EXR", stale, |ui| {
            pair(ui, "Compression", |ui| {
                egui::ComboBox::from_id_salt("exr_comp_deliver")
                    .selected_text(o.exr_compression.clone())
                    .show_ui(ui, |ui| {
                        for c in crate::workflow::EXR_COMPRESSIONS {
                            ui.selectable_value(&mut o.exr_compression, c.to_string(), c);
                        }
                    });
            });
            ui.label(RichText::new("Applies to every EXR sequence (shared).").color(DIM).size(12.0));
            if o.exr_compression == "dwab" {
                ui.colored_label(WARN, "DWAB is lossy: HDR values are not bit-exact.");
            }
        });
        self.hint.deliver_fmt_rects = fmt_rects;
        // Formats and compression are the same draft fields as in the Key panel's Outputs: same override rule.
        dr.mark_edits(&snap);
        if self.hint.deliveries.values().any(|d| matches!(d, Delivery::Video(_))) {
            ui.add_space(6.0);
            card(ui, "Video options (all video outputs)", stale, |ui| {
                // `None` = each clip's own source fps; an override is stored only when chosen.
                let mut source_fps = self.hint.export_fps.is_none();
                pair(ui, "Frame rate", |ui| {
                    if theme::checkbox(ui, &mut source_fps, "Use source").changed() {
                        self.hint.export_fps = if source_fps { None } else { Some(clip.fps) };
                    }
                    if let Some(fps) = &mut self.hint.export_fps {
                        ui.add(egui::DragValue::new(fps).speed(0.01).range(0.001..=1000.0).suffix(" fps"));
                    }
                });
                pair(ui, "Range", |ui| {
                    ui.scope(|ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        theme::checkbox(ui, &mut self.hint.export_partial, "Export the partial result (explicit range, no gaps)");
                    });
                });
                let dest_dir = self.project.as_ref().map(|p| crate::pipeline::export_dir(&clip, &p.root, self.hint.export_dir.as_deref()));
                pair(ui, "Folder", |ui| {
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(dest_dir.as_ref().map_or("?".into(), |d| d.display().to_string())).wrap());
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Choose…").clicked() {
                                let ctx = ui.ctx().clone();
                                self.request_pick(&ctx, PickKind::ExportFolder);
                            }
                            if self.hint.export_dir.is_some() && ui.button("Project default").clicked() {
                                self.hint.export_dir = None;
                            }
                        });
                    });
                });
                ui.label(RichText::new("Audio is not included. Files: <clip>_<output>_<in>-<out>.<mp4|mov|webm>; an existing file is never replaced.").color(DIM).size(12.0));
            });
        }
        // Each field the user really changed (one output's delivery, fps, partial flag, folder) is the shot's own and
        // keeps winning over a Batch preset until Reset; the others stay inherited.
        let keys = shot_domains::export_diff(&export_before, &self.current_export());
        if !keys.is_empty() {
            let preset = self.hint.delivery_src.clone();
            for k in keys {
                self.hint.delivery_ov.entry(k.clone()).or_insert_with(|| Field { base: shot_domains::export_get(&export_before, &k), preset: preset.clone() });
            }
        }
        // (The write itself is the per-frame step in `hint_pending_refresh`: as soon as the pointer settles,
        // atomically with the markers, and kept and retried if it fails.)
        if !self.hint.delivery_ov.is_empty() {
            let names: Vec<String> = self.hint.delivery_ov.keys().cloned().collect();
            let label = Self::reset_label("these delivery fields", &self.hint.delivery_ov);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Delivery, this shot's own (win over a Batch preset): {}", names.join(", "))).color(DIM).size(12.0));
                if ui.small_button(label).clicked() {
                    self.reset_delivery(clip);
                }
            });
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(WARN).size(12.0));
        }
    }

    /// Deliver action when any output is a video: Key, then every video output (core queues the chain).
    pub(super) fn deliver_action(&mut self, ui: &mut Ui, clip: &Clip) {
        self.sync_deliveries(clip);
        let ffmpeg = self.ai.local_caps().is_some_and(|c| c.tasks.get("export_video").copied().unwrap_or(false));
        let busy = self.ai.task_busy(&clip.folder);
        let range = self.ai_open_range();
        let pl = self.capture_pipeline(clip);
        let why = match (&pl, &range) {
            (Err(e), _) | (_, Err(e)) => e.clone(),
            _ if !ffmpeg => reason(&self.ai.local_caps(), "FFmpeg"),
            _ if busy => "A job is running for this clip".into(),
            _ => String::new(),
        };
        // A confirmation only ever applies to the shot, results, range and settings it was opened for.
        let sig = result_sig(clip, &range);
        if let Some(c) = &self.hint.render_confirm {
            let same = c.folder == clip.folder && c.sig == sig && pl.as_ref().is_ok_and(|p| *p == c.pipeline) && why.is_empty();
            if !same {
                self.hint.render_confirm = None;
                self.hint_note("The confirmation was closed because the shot, its results or the Deliver settings changed. Press the button again.");
            }
        }
        let confirming = self.hint.render_confirm.is_some();
        let go = ui.vertical_centered(|ui| ui.add_enabled(why.is_empty() && !confirming, egui::Button::new("Run full workflow").min_size(egui::vec2(220.0, 30.0)))).inner;
        self.hint.deliver_rects.push(("render".into(), go.rect));
        let go = go.on_hover_text("Runs this shot's whole workflow: pre-matte, hint, Key, then every enabled video output. To encode from an existing Key result without running Key again, use “Export existing Key result”.").on_disabled_hover_text(why.clone());
        ui.vertical_centered(|ui| ui.label(RichText::new("Full workflow: pre-matte → hint → Key → export").color(DIM).size(11.5)));
        if go.clicked() {
            if let (Ok(pl), Some(i), Some(p)) = (&pl, self.open, self.project.as_ref()) {
                let existing = existing_stage_results(clip);
                if existing.is_empty() {
                    let ctx = ui.ctx().clone();
                    self.ai.queue_pipeline(&ctx, p, self.settings.saved(), i, pl);
                } else {
                    self.hint.render_confirm = Some(RenderConfirm { folder: clip.folder.clone(), sig: sig.clone(), pipeline: pl.clone(), existing });
                }
            }
        }
        let mut export_now = false;
        if let Some(c) = &self.hint.render_confirm {
            let existing = c.existing.join(", ");
            let can_export = clip.manifest.is_some() && ffmpeg && !busy && range.is_ok();
            let (mut run, mut cancel) = (false, false);
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(WARN, format!("This shot already has {existing}."));
                ui.label(RichText::new("Running the full workflow means pre-matte, hint and Key may run again, not only the video encoding. To encode the selected video outputs from the existing Key result, choose “Export existing Key result”.").size(12.0));
                ui.horizontal_wrapped(|ui| {
                    let a = ui.button("Cancel");
                    self.hint.deliver_rects.push(("confirm_cancel".into(), a.rect));
                    cancel = a.clicked();
                    let b = ui.button("Run full workflow again");
                    self.hint.deliver_rects.push(("confirm_run".into(), b.rect));
                    run = b.clicked();
                    let e = ui.add_enabled(can_export, egui::Button::new("Export existing Key result instead"));
                    self.hint.deliver_rects.push(("confirm_export".into(), e.rect));
                    export_now = e.clicked();
                });
            });
            if cancel || export_now {
                self.hint.render_confirm = None;
            } else if run {
                if let (Some(c), Some(i), Some(p)) = (self.hint.render_confirm.take(), self.open, self.project.as_ref()) {
                    let ctx = ui.ctx().clone();
                    self.ai.queue_pipeline(&ctx, p, self.settings.saved(), i, &c.pipeline);
                }
            }
        }
        // While the confirmation is open its own export choice is the only one (no duplicate export buttons).
        let mut export_click = export_now;
        if clip.manifest.is_some() && !confirming {
            let ready = ffmpeg && !busy && range.is_ok();
            let export = ui.vertical_centered(|ui| ui.add_enabled(ready, egui::Button::new("Export existing Key result").min_size(egui::vec2(220.0, 30.0)))).inner;
            self.hint.deliver_rects.push(("export_existing".into(), export.rect));
            export_click |= export.on_hover_text("Encode the selected video outputs from the current Key result, without running Key again.").clicked();
        }
        if export_click {
            if let (Ok(r), Some(p)) = (range, self.project.as_ref()) {
                let cfg = PipelineExport {
                    outputs: self.hint.deliveries.clone(),
                    fps: self.hint.export_fps,
                    allow_partial: self.hint.export_partial,
                    root: self.hint.export_dir.clone(),
                };
                self.hint.note = Some(match self.ai.submit_existing_deliveries(clip, &p.root, r, &cfg) {
                    Ok(ids) => format!("Queued {} video output(s) from the existing Key result.", ids.len()),
                    Err(e) => format!("Existing result not exported: {e}"),
                });
            }
        }
        if !why.is_empty() {
            ui.label(RichText::new(&why).color(DIM).size(12.0));
        }
        // Why the Deliver cue is on: each reason separately, never blended into one claim.
        let rc = &self.hint.rc;
        if rc.deliver {
            ui.label(RichText::new(format!("Needs encoding: {}.", rc.deliver_why.join("; "))).color(DIM).size(12.0));
        }
        if !rc.deliver_unverified.is_empty() {
            ui.label(RichText::new(format!("Not verified as encoded (no export seen in this session): {}.", rc.deliver_unverified.join(", "))).color(DIM).size(12.0));
        }
        if rc.key {
            ui.label(RichText::new("The Key result is older than the current pre-matte/hint/Key settings (or missing): outputs encoded from it would be outdated.").color(DIM).size(12.0));
        }
        if let Some(b) = self.ai.pipeline_block_reason() {
            ui.colored_label(WARN, b);
        }
        // Real pipeline progress: the Key job, then whichever pre-matte/hint step and export is queued or running.
        self.ai.job_status(ui, &clip.folder);
        if busy {
            for k in [TaskKind::Gvm, TaskKind::Birefnet, TaskKind::Chroma, TaskKind::SamTrack, TaskKind::Matanyone2, TaskKind::Videomama, TaskKind::PrematteHint] {
                if let Some(r) = self.ai.latest_task(&clip.folder, k) {
                    self.ai.task_status_row(ui, &r);
                }
            }
        }
        if let Some(r) = self.ai.latest_task(&clip.folder, TaskKind::ExportVideo) {
            self.ai.task_status_row(ui, &r);
        }
    }
}

/// "H.264 (no alpha)" / "ProRes 4444 (alpha)": the codec's name and whether it can carry transparency.
fn codec_text(c: &crate::presets::VideoCodec) -> String {
    let n = match c.id {
        "h264" => "H.264",
        "hevc" => "H.265 (HEVC)",
        "prores422" => "ProRes 422",
        "prores4444" => "ProRes 4444",
        "vp9" => "VP9",
        o => o,
    };
    format!("{n} ({})", if c.alpha { "alpha" } else { "no alpha" })
}

fn reason(caps: &Option<crate::worker::Capabilities>, what: &str) -> String {
    match caps {
        None => "The worker is not ready yet (starting or unavailable).".into(),
        Some(c) if c.task_unavailable.is_empty() => {
            format!("{what} is not installed or verified on the worker: see Settings > Models.")
        }
        Some(c) => c
            .task_unavailable
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("; "),
    }
}

impl App {
    fn presets_file(&self) -> Option<PathBuf> {
        crate::presets::file_beside(self.settings.store_path())
    }

    fn ensure_presets(&mut self) {
        if self.hint.presets_loaded {
            return;
        }
        self.hint.presets_loaded = true;
        if let Some(f) = self.presets_file() {
            let (p, err) = crate::presets::load(&f);
            self.hint.presets = p;
            if err.is_some() {
                self.hint.note = err;
            }
        }
    }

    /// True when the file was written; otherwise the note says so (the in-memory list still holds the preset).
    fn store_presets(&mut self) -> bool {
        match self.presets_file() {
            Some(f) => match crate::presets::save(&f, &self.hint.presets) {
                Ok(()) => true,
                Err(e) => {
                    self.hint_note(format!("Presets not saved: {e}"));
                    false
                }
            },
            None => {
                self.hint_note("Presets need a preferences location; none is available.");
                false
            }
        }
    }

    /// Everything a full pipeline preset carries, read from the live state. Source-bound state is not read.
    pub fn capture_pipeline(&mut self, clip: &Clip) -> Result<crate::presets::Pipeline, String> {
        self.sync_deliveries(clip);
        let Some(pr) = self.project.as_ref() else { return Err("no project open".into()) };
        let d = self.ai.draft(clip, pr, self.settings.saved());
        let (params, outputs) = (d.params.clone(), d.out.clone());
        let location = pr.output_location.clone().filter(|l| !l.is_null());
        let p = crate::presets::Pipeline {
            hint: self.hint_cfg_for(clip),
            params,
            outputs,
            location,
            export: PipelineExport {
                fps: self.hint.export_fps,
                allow_partial: self.hint.export_partial,
                root: self.hint.export_dir.clone(),
                outputs: self.hint.deliveries.clone(),
            },
        };
        p.validate()?;
        Ok(p)
    }

    /// Persist and adopt a validated pipeline. Each step reports its own failure; a step that did not save is
    /// not adopted in memory, and steps that did save stay saved.
    pub fn apply_pipeline(&mut self, ctx: &egui::Context, clip: &Clip, name: &str, pl: &crate::presets::Pipeline) -> Result<(), String> {
        pl.validate()?;
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else { return Err("no project open".into()) };
        let mut errs: Vec<String> = vec![];
        // The preset's route replaces the shot's own fields: ONE write for the route and the cleared markers, with
        // the preset recorded as their source.
        match shot_domains::write_domain(&clip.meta_path, Domain::Route, &Default::default(), Some(Some(name)), |n| pl.hint.write_into(n)) {
            Ok(()) => {
                if self.hint.folder.as_deref() == Some(clip.folder.as_str()) {
                    self.hint.cfg = pl.hint.clone();
                    (self.hint.route_saved, self.hint.route_failed) = (pl.hint.clone(), None);
                    (self.hint.route_ov, self.hint.route_src) = (Default::default(), Some(name.to_owned()));
                }
                self.unsaved_done(&clip.folder, true);
            }
            Err(e) => errs.push(format!("hint settings: {e}")),
        }
        let mode_saved = crate::persist::save_preset_hint_mode(clip, &pl.params.hint_mode);
        if let Err(e) = &mode_saved {
            errs.push(format!("hint mode: {e}"));
        }
        let loc = match &pl.location {
            Some(v) => serde_json::from_value::<crate::contracts::OutputLocation>(v.clone())
                .map_err(|e| e.to_string())
                .and_then(|l| crate::workflow::save_location(&root, &l).map(|v| (Some(v), l))),
            None => crate::workflow::clear_location(&root).map(|()| (None, crate::workflow::default_location(clip).unwrap_or_else(|| self.loc.clone()))),
        };
        match loc {
            Ok((v, l)) => {
                if let Some(p) = &mut self.project {
                    p.output_location = v;
                }
                self.loc = l;
            }
            Err(e) => errs.push(format!("output location: {e}")),
        }
        if let Some(pr) = self.project.as_ref() {
            let d = self.ai.draft(clip, pr, self.settings.saved());
            let mut h = HintCfg::default();
            pl.apply(&mut h, &mut d.params, &mut d.out);
            d.applied(name, None); // a deliberate Apply replaces every field a shot override could shadow; the preset is their source
            if mode_saved.is_err() {
                d.params.hint_mode = crate::persist::clip_hint_mode(clip).unwrap_or_else(|| d.params.hint_mode.clone());
            }
        }
        let e = &pl.export;
        (self.hint.delivery_ov, self.hint.delivery_src) = (Default::default(), Some(name.to_owned()));
        self.hint.deliveries = e.outputs.clone();
        self.hint.export_fps = e.fps;
        self.hint.export_partial = e.allow_partial;
        self.hint.export_dir = e.root.clone();
        // The preset's delivery replaces the shot's own: written now, with its markers cleared, so Batch and a
        // reopened shot see it without a Save.
        if let Err(e) = self.persist_delivery(clip) {
            errs.push(format!("delivery: {e}"));
        }
        let f = clip.folder.clone();
        self.refresh_clip(ctx, &f);
        if errs.is_empty() { Ok(()) } else { Err(errs.join("; ")) }
    }

    /// Workflow presets: the full pre-matte -> Key -> per-output delivery pipeline (Key and Deliver share the
    /// list). Source-bound values are never copied (see `presets`).
    pub(super) fn preset_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        self.ensure_presets();
        let kind = "workflow";
        ui.label(
            RichText::new("Workflow presets").strong(),
        );
        // (stable name, list label): legacy presets are labelled, never renamed.
        let names: Vec<(String, String)> = self
            .hint
            .presets
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| (p.name.clone(), crate::presets::label(p)))
            .collect();
        if !names.iter().any(|n| n.0 == self.hint.preset_sel) {
            self.hint.preset_sel = names.first().map(|n| n.0.clone()).unwrap_or_default();
        }
        let sel_label = names.iter().find(|n| n.0 == self.hint.preset_sel).map_or("(none saved)".to_string(), |n| n.1.clone());
        self.hint.preset_rects.clear();
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt(("preset", kind)).selected_text(sel_label).show_ui(ui, |ui| {
                for (n, l) in &names {
                    ui.selectable_value(&mut self.hint.preset_sel, n.clone(), l);
                }
            });
            let have = !self.hint.preset_sel.is_empty();
            let ap = ui.add_enabled(have, egui::Button::new("Apply"));
            self.hint.preset_rects.push(("apply", ap.rect));
            if ap.clicked() {
                let found = self.hint.presets.iter().find(|p| p.kind == kind && p.name == self.hint.preset_sel).cloned();
                if let (Some(pr), true) = (found, self.project.is_some()) {
                    let sel = pr.name.clone();
                    if crate::presets::class_of(&pr) == crate::presets::PresetClass::Pipeline {
                        let note = match crate::presets::Pipeline::from_value(&pr.data) {
                            Err(e) => format!("Preset not applied: {e}"),
                            Ok(pl) => match self.apply_pipeline(ui.ctx(), clip, &sel, &pl) {
                                Ok(()) => format!("Applied “{sel}”: pre-matte, hint, Key, outputs and export settings. {} Range, colour, prompts and marks are untouched.", if pl.location.is_none() { "Project output location reset to default." } else { "Project output location updated." }),
                                Err(e) => format!("Preset “{sel}” not fully applied: {e}"),
                            },
                        };
                        self.hint.note = Some(note);
                    } else if let Some(p) = self.project.as_ref() {
                        let dr = self.ai.draft(clip, p, self.settings.saved());
                        let res = crate::presets::apply_workflow(&pr.data, &mut dr.params, &mut dr.out);
                        if res.is_ok() {
                            // Only the fields this preset carries lose their override marker.
                            let carried: std::collections::BTreeSet<String> = ["params", "outputs"]
                                .iter()
                                .flat_map(|k| pr.data.get(k).and_then(|v| v.as_object()).into_iter().flat_map(|m| m.keys().cloned()))
                                .collect();
                            dr.applied(&sel, Some(&carried));
                            // A pick of the clip's own hint mode is one of those fields: it now follows the preset.
                            if crate::persist::explicit_hint_mode(clip).is_some() && pr.data["params"].get("hint_mode").is_some() {
                                let _ = crate::persist::save_preset_hint_mode(clip, &dr.params.hint_mode);
                            }
                        }
                        self.hint.note = Some(match res {
                            Ok(()) => format!("Applied legacy preset “{sel}”: parameters and outputs only (partial; pre-matte, hint, locations and export unchanged)."),
                            Err(e) => format!("Preset not applied: {e}"),
                        });
                    }
                }
            }
            let del = ui.add_enabled(have, egui::Button::new("Delete"));
            self.hint.preset_rects.push(("delete", del.rect));
            if del.clicked() {
                let sel = self.hint.preset_sel.clone();
                self.hint.presets.retain(|p| !(p.kind == kind && p.name == sel));
                self.store_presets();
            }
        });
        if self.hint.preset_name_for != self.hint.preset_sel {
            self.hint.preset_name_for = self.hint.preset_sel.clone();
            self.hint.preset_name = self.hint.preset_sel.clone();
        }
        ui.horizontal_wrapped(|ui| {
            ui.label("Name:");
            let r = ui.add(egui::TextEdit::singleline(&mut self.hint.preset_name)
                .id(egui::Id::unique(("preset_name", kind)))
                .desired_width(140.0)
                .min_size(egui::vec2(140.0, 24.0)));
            if r.clicked() {
                r.request_focus();
            }
            self.hint.preset_rects.push(("name", r.rect));
            let n = self.hint.preset_name.trim().to_string();
            let sv = ui.add_enabled(!n.is_empty(), egui::Button::new("Save current as preset"));
            self.hint.preset_rects.push(("save", sv.rect));
            if sv.clicked() {
                let data = self.capture_pipeline(clip).map(|p| p.to_value());
                match data {
                    Ok(data) => {
                        self.hint.presets.retain(|p| !(p.kind == kind && p.name == n));
                        self.hint.presets.push(crate::presets::Preset { name: n.clone(), kind: kind.into(), data });
                        self.hint.preset_name_for = n.clone();
                        self.hint.preset_sel = n.clone();
                        if self.store_presets() {
                            self.hint.note = Some(format!("Saved “{n}”."));
                        }
                    }
                    Err(e) => self.hint.note = Some(format!("Preset not saved: {e}")),
                }
            }
        });
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(DIM).size(12.0));
        }
    }
}

impl App {
    pub(super) fn relink_picked(&mut self, ctx: &egui::Context, folder: PathBuf) {
        let Some(clip) = self.open_clip_snapshot() else {
            return;
        };
        match crate::persist::relink_sequence(&clip, &folder) {
            Ok(note) => {
                self.hint.note = Some(note);
                let f = clip.folder.clone();
                self.refresh_clip(ctx, &f);
            }
            Err(e) => self.hint.note = Some(format!("Not relinked: {e}")),
        }
    }
}

// --------------------------------------------------------------------- strip icons

/// Width of the vertical tool strip beside the viewer.
/// One 28 px button plus the frame margin: the column is exactly as wide as its content.
const STRIP_W: f32 = 36.0;

#[derive(Clone, Copy)]
enum Icon {
    PointFg,
    PointBg,
    PaintFg,
    PaintBg,
    Erase,
    Box,
    Pick,
    Undo,
    ClearFrame,
    ClearAll,
}

fn icon_for(t: Tool) -> Icon {
    match t {
        Tool::FgPoint => Icon::PointFg,
        Tool::BgPoint => Icon::PointBg,
        Tool::PaintFg => Icon::PaintFg,
        Tool::PaintBg => Icon::PaintBg,
        Tool::Erase => Icon::Erase,
        Tool::Box => Icon::Box,
        _ => Icon::Pick,
    }
}

/// A square icon button with hover/selected backgrounds and a tooltip. `sel` = the active tool.
fn strip_button(ui: &mut Ui, sel: bool, icon: Icon, tip: &str) -> egui::Response {
    strip_button_disabled(ui, sel, icon, true, tip)
}

fn strip_button_disabled(
    ui: &mut Ui,
    sel: bool,
    icon: Icon,
    enabled: bool,
    tip: &str,
) -> egui::Response {
    let size = egui::vec2(STRIP_W - 8.0, 26.0);
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(size, sense);
    let vis = ui.visuals();
    let (fg, bg) = if !enabled {
        (vis.weak_text_color(), Color32::TRANSPARENT)
    } else if sel {
        (vis.selection.stroke.color, vis.selection.bg_fill)
    } else if resp.hovered() {
        (vis.strong_text_color(), vis.widgets.hovered.bg_fill)
    } else {
        // Readable at a glance: the icons are small, so they get the normal text colour, not the muted one.
        (vis.text_color(), vis.faint_bg_color)
    };
    if bg != Color32::TRANSPARENT {
        ui.painter().rect_filled(rect, 4.0, bg);
    }
    paint_icon(ui.painter(), rect.shrink(5.0), icon, fg);
    resp.on_hover_text(tip)
}

/// Vector icons: shapes only, so no glyph can render as a missing box.
fn paint_icon(p: &egui::Painter, r: egui::Rect, icon: Icon, col: Color32) {
    use egui::vec2;
    let c = r.center();
    let s = r.width().min(r.height()) * 0.5;
    let st = egui::Stroke::new(1.9, col);
    let plus = |at: egui::Pos2, d: f32, on: bool| {
        p.line_segment([at - vec2(d, 0.0), at + vec2(d, 0.0)], st);
        if on {
            p.line_segment([at - vec2(0.0, d), at + vec2(0.0, d)], st);
        }
    };
    match icon {
        Icon::PointFg | Icon::PointBg => {
            p.circle_stroke(c, s * 0.8, st);
            plus(c, s * 0.42, matches!(icon, Icon::PointFg));
        }
        Icon::PaintFg | Icon::PaintBg => {
            // a brush stroke, with the +/− that says which side of the matte it paints
            p.line_segment(
                [c + vec2(-s * 0.8, s * 0.6), c + vec2(s * 0.15, -s * 0.5)],
                egui::Stroke::new(3.2, col),
            );
            plus(c + vec2(s * 0.5, s * 0.45), s * 0.28, matches!(icon, Icon::PaintFg));
        }
        Icon::Erase => {
            let b = egui::Rect::from_center_size(c, vec2(s * 1.7, s * 1.15));
            p.rect_stroke(b, 2.0, st, egui::StrokeKind::Inside);
            p.line_segment([b.left_top(), b.right_bottom()], st);
        }
        Icon::Box => {
            p.rect_stroke(
                egui::Rect::from_center_size(c, vec2(s * 1.6, s * 1.15)),
                1.0,
                st,
                egui::StrokeKind::Inside,
            );
        }
        Icon::Pick => {
            p.line_segment([c + vec2(-s * 0.7, s * 0.7), c + vec2(s * 0.3, -s * 0.3)], st);
            p.circle_stroke(c + vec2(s * 0.5, -s * 0.5), s * 0.3, st);
        }
        Icon::Undo => {
            // an arc with an arrowhead pointing back
            let mut pts = vec![];
            for k in 0..=10 {
                let a = std::f32::consts::PI * (0.15 + 0.85 * k as f32 / 10.0);
                pts.push(c + vec2(a.cos() * s * 0.8, -a.sin() * s * 0.8) + vec2(0.0, s * 0.3));
            }
            p.add(egui::Shape::line(pts, st));
            let tip = c + vec2(-s * 0.77, s * 0.3 - s * 0.1);
            p.line_segment([tip, tip + vec2(-s * 0.35, -s * 0.35)], st);
            p.line_segment([tip, tip + vec2(s * 0.3, -s * 0.42)], st);
        }
        Icon::ClearFrame => {
            let d = s * 0.62;
            p.line_segment([c - vec2(d, d), c + vec2(d, d)], st);
            p.line_segment([c - vec2(d, -d), c + vec2(d, -d)], st);
        }
        Icon::ClearAll => {
            let d = s * 0.62;
            for dx in [-s * 0.45, s * 0.45] {
                let o = c + vec2(dx, 0.0);
                p.line_segment([o - vec2(d, d) * 0.7, o + vec2(d, d) * 0.7], st);
                p.line_segment(
                    [o - vec2(d, -d) * 0.7, o + vec2(d, -d) * 0.7],
                    st,
                );
            }
        }
    }
}
