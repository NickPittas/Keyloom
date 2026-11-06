//! Hint page: route/model selection, SAM/paint annotation editing, preview/track/generate jobs, keyframe marks,
//! companion alpha/mask movies, frame extraction and the Deliver video export. Everything persistent lives in
//! `clip.json["ck_native"]` (see `persist`/`hints`); every job is built from a frozen snapshot of that state and
//! submitted to the shared scheduler. Nothing here talks to a model: the worker does, or the control stays off.
use super::*;
use crate::hints::{
    self, Annotations, Ctx, Final, HintCfg, Producer, BIREFNET_VARIANTS, SAM_MODELS,
};
use crate::presets::{Delivery, PipelineExport, QualityKind, VideoDelivery, VIDEO_CODECS};
use crate::actions::{decide, Action};
use crate::tasks::TaskKind;
use crate::ui::ai::{card, fill_slider, pair, sig_of};
use crate::ui::annotate::{self, Ann, Tool};
use crate::workflow::new_id;
use crate::shot_domains::{self, Domain, Field};
use crate::ui::viewer::{MarkReq, Overlay};
use eframe::egui::Sense;
use std::path::Path;

#[derive(Default)]
pub struct State {
    folder: Option<String>,
    cfg: HintCfg,
    ann: Annotations,
    undo: Vec<Annotations>,
    note: Option<String>,
    /// The last `hint_note` error since `hint_cmd` started: what a command reports as its `Err`.
    cmd_err: Option<String>,
    /// The marked-frame choice could not be saved: new requests wait until it is (or is changed back).
    marks_err: Option<String>,
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
    /// A workflow preset waiting for its delete confirmation.
    preset_del: Option<String>,
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
    /// The open "Delete generated frames" dialog.
    dele: Option<Del>,
    /// Jobs known when generated frames were deleted: a SAM preview that finishes later is never shown.
    dead_previews: std::collections::HashSet<String>,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum DelScope {
    Current,
    Marked,
    All,
}

/// What the helper says it would delete (exact file names, movies only when asked).
#[derive(Default)]
struct DelPlan {
    files: Vec<String>,
    movies: Vec<String>,
    blocked: usize,
}

struct Del {
    folder: String,
    prematte: bool,
    scope: DelScope,
    movies: bool,
    /// What `plan` was asked for (asset|scope|movies|stems); a different choice asks again.
    plan_key: String,
    plan: Option<Result<DelPlan, String>>,
    /// The helper request in flight: (is the delete itself, answer).
    rx: Option<(bool, std::sync::mpsc::Receiver<Result<serde_json::Value, String>>)>,
    done: Option<String>,
}

/// Why generated frames of this clip cannot be deleted right now (None = they can). The helper checks the
/// publication lock and interrupted publishes again under the lock.
fn delete_refusal(active: bool, external: bool) -> Option<&'static str> {
    if active {
        Some("This clip has queued or running jobs that read or write its hint folders. Wait for them or cancel them.")
    } else if external {
        Some("Another computer is importing into this clip. Wait for it to finish.")
    } else {
        None
    }
}

/// The frames a scope names: `None` = every frame of the asset.
fn scope_stems(scope: DelScope, current: Option<&str>, marked: &[String]) -> Option<Vec<String>> {
    match scope {
        DelScope::All => None,
        DelScope::Current => Some(current.map(str::to_owned).into_iter().collect()),
        DelScope::Marked => Some(marked.to_vec()),
    }
}

fn marked_keyframe_stems(source: &[String], keyframes: &[usize]) -> Vec<String> {
    keyframes.iter().filter_map(|index| source.get(*index).cloned()).collect()
}

fn delete_request(cmd: &str, rid: &str, root: &Path, prematte: bool, stems: &Option<Vec<String>>, movies: bool) -> String {
    serde_json::json!({"cmd": cmd, "request_id": rid, "clip_root": root, "asset": if prematte { "prematte" } else { "hint" },
        "scope": if stems.is_some() { "frames" } else { "all" }, "stems": stems.clone().unwrap_or_default(), "movies": movies})
    .to_string()
}

fn strings(v: &serde_json::Value, k: &str) -> Vec<String> {
    v.get(k).and_then(|a| a.as_array()).into_iter().flatten().filter_map(|x| x.as_str()).map(str::to_owned).collect()
}

/// A SAM preview of a job that existed when frames were deleted must not come back.
fn preview_is_live(job_id: &str, dead: &std::collections::HashSet<String>) -> bool {
    !dead.contains(job_id)
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
    annotations: Option<Annotations>,
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
fn current_products(h: &State, exp: &PipelineExport, fps: f64, range: Option<(usize, usize)>) -> std::collections::BTreeMap<String, ProductCfg> {
    let fps = (h.export_fps.unwrap_or(fps) * 1000.0).round() as i64;
    exp.outputs
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

/// A tool stays selected when the producer offers it; only SAM prompt tools also need the SAM overlay on
/// (Chroma's colour pick must work with the overlay off, its chip is greyed out then).
fn tool_allowed(p: Producer, sam_overlay: bool, tool: Tool) -> bool {
    (sam_overlay || p != Producer::Sam) && tools_for(p).iter().any(|t| t.0 == tool)
}

#[cfg(test)]
mod missing_model_tests {
    use super::*;

    #[test]
    fn only_the_selected_producer_with_a_missing_model_is_named() {
        let mk = |gvm: bool, ffmpeg: bool| {
            let b = |ready: bool, id: &str| serde_json::json!({"ready": ready, "state": if ready {"present"} else {"missing"}, "paths": {id: "/m"}});
            crate::models::Snapshot::from_json(&serde_json::json!({
                "models": {"key": {"k": b(true, "k")}, "gvm": {"gvm": b(gvm, "gvm")}, "videomama": {"videomama": b(true, "videomama")},
                           "matanyone2": {"matanyone2": b(true, "matanyone2")}, "sam2": {"sam2-base-plus": b(true, "sam2-base-plus")},
                           "birefnet": {"birefnet-BiRefNet-matting": b(true, "birefnet-BiRefNet-matting")}},
                "ffmpeg": ffmpeg, "sam2": {"base_plus": "sam2-base-plus"}, "birefnet": {"Matting": "birefnet-BiRefNet-matting"}})).unwrap()
        };
        let snap = |gvm: bool| mk(gvm, true);
        let mut cfg = HintCfg::default();
        cfg.guide = Producer::Gvm;
        let caps = Some(snap(false).capabilities(true));
        assert_eq!(missing_guide(&caps, false, &cfg).as_deref(), Some("GVM"));
        assert_eq!(reason(&caps, "GVM"), "GVM is missing — download it on the Models page");
        assert_eq!(missing_final(&caps, false, Final::Videomama), None, "VideoMaMa is installed");
        // No FFmpeg on this PC: the hover still names the missing model, and FFmpeg only when asked about FFmpeg.
        let caps_ff = Some(mk(false, false).capabilities(true));
        assert_eq!(reason(&caps_ff, "GVM"), "GVM is missing — download it on the Models page");
        assert_eq!(reason(&caps_ff, "FFmpeg"), "FFmpeg not installed or not usable");
        cfg.guide = Producer::Birefnet;
        assert_eq!(missing_guide(&caps, false, &cfg), None, "BiRefNet Matting is installed");
        cfg.birefnet_variant = "Portrait".into();
        assert_eq!(missing_guide(&caps, false, &cfg).as_deref(), Some("BiRefNet Portrait"));
        cfg.guide = Producer::Chroma;
        assert_eq!(missing_guide(&caps, false, &cfg), None, "Chroma needs no model");
        cfg.guide = Producer::Gvm;
        assert_eq!(missing_guide(&caps, true, &cfg), None, "not read yet");
        assert_eq!(missing_guide(&Some(snap(false).capabilities(false)), false, &cfg), None, "no GPU is its own reason");
    }
}

#[cfg(test)]
mod tool_tests {
    use super::*;
    #[test]
    fn chroma_pick_survives_overlay_off_but_sam_tools_do_not() {
        assert!(tool_allowed(Producer::Chroma, false, Tool::Pick));
        assert!(!tool_allowed(Producer::Sam, false, Tool::FgPoint));
        assert!(tool_allowed(Producer::Sam, true, Tool::FgPoint));
        assert!(!tool_allowed(Producer::Chroma, true, Tool::FgPoint));
    }
}

#[derive(Clone, Copy)]
enum Run {
    Guide,
    Hint,
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
    pub(super) fn board_export_root(&self) -> Option<&Path> { self.export_dir.as_deref() }
    pub(super) fn deliveries_has_video(&self) -> bool {
        self.deliveries.values().any(|d| matches!(d, Delivery::Video(_)))
    }
}

impl App {
    fn hint_note(&mut self, m: impl Into<String>) {
        let m = m.into();
        self.hint.cmd_err = Some(m.clone());
        self.hint.note = Some(m);
    }

    /// True once the per-shot hint state is loaded for the open clip (test hook).
    #[doc(hidden)]
    pub fn hint_loaded_for_test(&self) -> bool {
        self.clip().is_some_and(|c| self.hint.folder.as_deref() == Some(c.folder.as_str()))
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
        if let Some(r) = self.viewer.as_mut().and_then(|v| v.mark_req.take()) {
            self.hint_marks(match r {
                MarkReq::Mark => Some(true),
                MarkReq::Unmark => Some(false),
                MarkReq::Clear => None,
            });
        }
        let (Some(p), Some(i), Some(v)) = (self.project.as_ref(), self.open, self.viewer.as_mut())
        else {
            self.hint.folder = None;
            return;
        };
        let clip = &p.clips[i];
        if self.tab != Tab::Hint
            || !tool_allowed(self.hint.cfg.guide, v.sam_overlay, v.ann.tool)
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
            if let Some(a) = self.hint_unsaved.get(&clip.folder).and_then(|u| u.annotations.clone()) {
                self.hint.ann = a;
                self.hint.note = Some("SAM prompts could not be saved earlier; the unsaved prompts were restored.".into());
            }
            let left = hints::interrupted_publish(&clip.root);
            if !left.is_empty() {
                self.hint.note = Some(format!("An interrupted hint publication left {} aside; running any hint job on this clip (or clearing its lock) restores it.", left.join(", ")));
            }
            // No worker probe here: it starts on demand when work is queued (an idle worker must never block anything).
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
        v.marks.sort_unstable();
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
                    Ok(()) => {
                        self.hint.ann_saved = ann_sig(&self.hint.ann);
                        let clear = if let Some(u) = self.hint_unsaved.get_mut(&clip.folder) {
                            u.annotations = None;
                            u.route.is_none() && u.delivery.is_none()
                        } else {
                            false
                        };
                        if clear {
                            self.hint_unsaved.remove(&clip.folder);
                        }
                        self.hint.note = None;
                    }
                    Err(e) => self.hint.note = Some(format!("Annotations not saved: {e}")),
                }
            }
            if let Some(e) = size_err {
                self.hint.note = Some(e);
            }
        }
        v.ann.holdout.clear();
        // The SAM layer (prompts and preview) exists only while SAM is the pre-matte producer and its toggle is on.
        let sam = self.hint.cfg.guide == Producer::Sam;
        v.sam_available = sam;
        let sam_on = sam && v.sam_overlay;
        let want = match v.overlay {
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
            .filter(|_| sam_on)
            .and_then(|s| self.hint.ann.frames.get(s).cloned())
            .unwrap_or_default();
        v.ann.preview = stem.as_ref().filter(|_| sam_on).and_then(|s| {
            let sig = psig(&self.hint, s, v.linear);
            self.ai
                .current_preview(&clip.folder, s, sig)
                .filter(|r| preview_is_live(&r.job_id, &self.hint.dead_previews))
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
            self.unsaved_annotations_done(&clip.folder);
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
            if u.route.is_none() && u.delivery.is_none() && u.annotations.is_none() {
                self.hint_unsaved.remove(folder);
            }
        }
    }

    fn unsaved_annotations_done(&mut self, folder: &str) {
        if let Some(u) = self.hint_unsaved.get_mut(folder) {
            u.annotations = None;
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

    /// The shown shot is being left: write its pending route/delivery/prompts now; whatever cannot be written is kept (by
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
        if ann_sig(&self.hint.ann) != self.hint.ann_saved {
            un.annotations = self.hint.ann.save(&old).err().map(|_| self.hint.ann.clone());
            if un.annotations.is_none() {
                self.hint.ann_saved = ann_sig(&self.hint.ann);
            }
        }
        if un.route.is_some() || un.delivery.is_some() || un.annotations.is_some() {
            self.ai.notice = Some(format!("{}: hint route, delivery or prompt edits could not be saved. They are kept and shown as unsaved; Save retries them.", old.display_name));
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
            if let Some(a) = &u.annotations {
                match a.save(&clip) {
                    Ok(()) => {
                        self.unsaved_annotations_done(&folder);
                        if self.hint.folder.as_deref() == Some(folder.as_str()) && ann_sig(&self.hint.ann) == ann_sig(a) {
                            self.hint.ann_saved = ann_sig(a);
                        }
                    }
                    Err(e) => errs.push(format!("{}: prompts: {e}", clip.display_name)),
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

    /// "This shot's own hint route fields" line with its Reset (Hint page).
    pub(super) fn route_override_ui(&mut self, ui: &mut Ui, _clip: &Clip) {
        if self.hint.route_ov.is_empty() {
            return;
        }
        let names: Vec<String> = self.hint.route_ov.keys().cloned().collect();
        let label = Self::reset_label("these route fields", &self.hint.route_ov);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(format!("Hint route, this shot's own (win over a Batch preset): {}", names.join(", "))).color(theme::text_dim(ui)).size(12.0));
            if ui.small_button(label).clicked() {
                let ctx = ui.ctx().clone();
                let _ = self.hint_cmd(&ctx, HintCmd::ResetRoute);
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
        // Read the enabled outputs through the synced clone (`export_now`); nothing is written to `hint.deliveries`.
        let exp = self.deliver_out_cfg(&clip).map(|o| self.export_now(&o)).unwrap_or_else(|| self.current_export());
        let products = current_products(&self.hint, &exp, clip.fps, range_now);
        if first {
            let (p, h) = prov_base(&clip, &cur);
            let rc = &mut self.hint.rc;
            (rc.base_pre, rc.base_hint) = (Some(p), Some(h));
            rc.key_manifest = Some(manifest);
        }
        // The video outputs configured at open: known, but never verified as encoded.
        if !self.hint.rc.deliver_seeded && !exp.outputs.is_empty() {
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
        let delivery_sig = serde_json::to_string(&exp).unwrap_or_default();
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
        let r = if primary { theme::accent(ui, enabled, |ui| ui.add(egui::Button::new(label))) } else { ui.add_enabled(enabled, egui::Button::new(label)) };
        if let Some(p) = self.stale(ui.ctx(), stage) {
            ui.painter().rect_stroke(r.rect.expand(1.0), 4.0, egui::Stroke::new(2.0 + p * 1.5, theme::warn(ui)), egui::StrokeKind::Outside);
        }
        r
    }
}

// ------------------------------------------------------------------- typed Hint state + commands (Shot page)

fn task_ok(caps: &Option<crate::models::Capabilities>, lazy: bool, k: TaskKind) -> bool {
    lazy || caps.as_ref().is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false))
}

fn cleanup_note(fin: Final) -> &'static str {
    if fin.needs_guide() {
        "Grow / fill holes: regenerate pre-matte. Feather: regenerate hint. Guides stay binary."
    } else {
        "Regenerate to apply changes. Feather softens the final hint; guides stay binary."
    }
}

/// A slider that reports a changed value instead of writing it (the caller sends it as a `HintEdit`).
fn slide<N: egui::emath::Numeric>(ui: &mut Ui, cur: N, range: std::ops::RangeInclusive<N>, suffix: &str) -> Option<N> {
    let mut v = cur;
    fill_slider(ui);
    ui.add(egui::Slider::new(&mut v, range).suffix(suffix));
    (v != cur).then_some(v)
}

/// One option of a dropdown: `ready` is false only when the worker reported the model missing (an unknown or
/// not-yet-started worker counts as ready, like the old panel).
#[derive(Clone)]
pub(crate) struct Choice<T> {
    pub value: T,
    pub label: String,
    pub ready: bool,
}

/// One action: its enabled state, label ("Queue ...", "Already queued") and the reason it is off or waits.
#[derive(Clone)]
pub(crate) struct HintAct {
    pub enabled: bool,
    pub label: String,
    pub reason: String,
}
impl From<&crate::actions::Decision> for HintAct {
    fn from(d: &crate::actions::Decision) -> Self {
        Self { enabled: d.enabled, label: d.label.clone(), reason: d.reason.clone() }
    }
}

/// Pre-matte or Hint card: frames on disk, stale flag and its actions.
#[derive(Clone)]
pub(crate) struct HintStage {
    pub frames_on_disk: usize,
    pub stale: bool,
    /// SAM single-frame preview (Pre-matte with SAM only).
    pub preview: Option<HintAct>,
    /// Track (SAM) / Generate pre-matte, or Generate hint.
    pub generate: HintAct,
    /// Always enabled: it opens the delete dialog, which states any refusal (`reason`).
    pub delete: HintAct,
}

#[derive(Clone)]
pub(crate) struct ChromaView {
    pub cfg: crate::hints::Chroma,
    /// Legacy holdout strokes (fg, bg), or the load error.
    pub holdout: Result<(usize, usize), String>,
}

#[derive(Clone)]
pub(crate) struct MarkedView {
    pub pre: bool,
    pub hint: bool,
    /// MatAnyone2: two rows (pre-matte, same marks as hints); otherwise one row "pre-matte and hint".
    pub two_rows: bool,
    pub show_hint_row: bool,
    /// MatAnyone2 with marked pre-matte but full-range hints: "Full-range hints use In-frame pre-matte."
    pub in_frame_note: bool,
    pub key_uses_marked: bool,
}

#[derive(Clone)]
pub(crate) struct MovieView {
    pub path: PathBuf,
    pub as_mask: bool,
    pub alpha_channel: bool,
}

#[derive(Clone)]
pub(crate) struct RouteView {
    /// The shot's own route fields (names), with the label of the Reset that gives them up.
    pub overrides: Vec<String>,
    pub reset_label: Option<String>,
    /// The shown route differs from the saved one / the last save failed (kept, retried on the next edit or Save).
    pub unsaved: bool,
    pub save_failed: bool,
}

/// Read-only snapshot of the Hint stage of the open shot (cached state only; nothing is probed or written).
#[derive(Clone)]
pub(crate) struct HintView {
    pub bypass: bool,
    /// The worker has not been asked yet: readiness is unknown, not missing.
    pub worker_unknown: bool,
    pub producer: Producer,
    pub producers: Vec<Choice<Producer>>,
    /// Model the selected producer needs when the app's check found it missing ("GVM", "SAM Base+").
    pub producer_missing: Option<String>,
    pub birefnet_variant: Option<String>,
    pub birefnet_variants: Vec<Choice<String>>,
    pub sam_model: Option<String>,
    pub sam_models: Vec<Choice<String>>,
    pub chroma: Option<ChromaView>,
    pub finish: Final,
    pub finishes: Vec<Choice<Final>>,
    pub finish_missing: Option<String>,
    pub videomama_chunk: Option<u32>,
    /// None for an imported alpha (nothing to generate).
    pub marked: Option<MarkedView>,
    pub marked_gate: Option<String>,
    pub marks_err: Option<String>,
    pub cleanup: crate::hints::MaskCleanup,
    pub cleanup_note: &'static str,
    pub movie: Option<MovieView>,
    pub tool: Tool,
    pub tools: Vec<(Tool, &'static str, &'static str)>,
    /// Some when the brush row applies (the producer has paint/point tools).
    pub brush: Option<f32>,
    pub prompt_frames: usize,
    pub frame_annotated: bool,
    pub can_undo: bool,
    pub prematte: HintStage,
    pub hint: HintStage,
    /// None when the source is not a movie.
    pub extract: Option<HintAct>,
    /// Some while a companion movie is picked.
    pub import: Option<HintAct>,
    pub route: RouteView,
    pub note: Option<String>,
}

/// Every editable Hint row (route rows are saved through `save_route`; the two movie rows are page state).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HintEdit {
    Bypass(bool),
    Producer(Producer),
    BirefnetVariant(String),
    SamModel(String),
    Finish(Final),
    VideomamaChunk(u32),
    ChromaScreen(String),
    ChromaStrength(f64),
    ChromaClipBlack(f64),
    ChromaClipWhite(f64),
    ChromaShrinkGrow(i64),
    ChromaEdgeBlur(i64),
    CleanupGrow(i64),
    CleanupFill(bool),
    CleanupFeather(f64),
    MovieAsMask(bool),
    MovieAlphaChannel(bool),
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum HintCmd {
    ChooseMovie,
    Edit(HintEdit),
    MarkedMode { pre: bool, hint: bool },
    ResetRoute,
    /// Selects the tool (selecting the active one again deselects it); `Tool::None` deselects.
    Tool(Tool),
    Brush(f32),
    UndoPrompt,
    ClearPrompts { all: bool },
    PreviewSam,
    /// `marked` must equal the shown Marked-frames choice (change that row first); it is not a second setting.
    GeneratePreMatte { marked: bool },
    GenerateHint { marked: bool },
    ExtractSource,
    ImportMovie,
    CancelMovie,
    RequestDelete { prematte: bool },
    ClearLegacyHoldout,
    ResetChromaColour,
}

fn finite_in(v: f64, lo: f64, hi: f64, what: &str) -> Result<f64, String> {
    if v.is_finite() { Ok(v.clamp(lo, hi)) } else { Err(format!("{what} must be a number")) }
}

impl HintEdit {
    /// The value as the old controls allowed it (same ranges); an impossible value is refused, not guessed.
    fn apply(self, cfg: &mut HintCfg) -> Result<(), String> {
        match self {
            HintEdit::Bypass(b) => cfg.bypass = b,
            HintEdit::Producer(p) => cfg.guide = p,
            HintEdit::BirefnetVariant(v) if BIREFNET_VARIANTS.contains(&v.as_str()) => cfg.birefnet_variant = v,
            HintEdit::BirefnetVariant(v) => return Err(format!("Unknown BiRefNet variant {v}")),
            HintEdit::SamModel(m) if SAM_MODELS.iter().any(|s| s.0 == m) => cfg.sam_model = m,
            HintEdit::SamModel(m) => return Err(format!("Unknown SAM model {m}")),
            HintEdit::Finish(f) => cfg.finish = f,
            HintEdit::VideomamaChunk(n) => cfg.videomama_chunk = n.clamp(1, 64),
            HintEdit::ChromaScreen(s) if s == "green" || s == "blue" => cfg.chroma.screen_type = s,
            HintEdit::ChromaScreen(s) => return Err(format!("Screen must be green or blue, not {s}")),
            HintEdit::ChromaStrength(v) => cfg.chroma.strength = finite_in(v, 0.1, 10.0, "Strength")?,
            HintEdit::ChromaClipBlack(v) => cfg.chroma.clip_black = finite_in(v, 0.0, 1.0, "Clip black")?,
            HintEdit::ChromaClipWhite(v) => cfg.chroma.clip_white = finite_in(v, 0.0, 1.0, "Clip white")?,
            HintEdit::ChromaShrinkGrow(v) => cfg.chroma.shrink_grow = v.clamp(-100, 100),
            HintEdit::ChromaEdgeBlur(v) => cfg.chroma.edge_blur = v.clamp(0, 100),
            HintEdit::CleanupGrow(v) => cfg.mask_cleanup.grow = v.clamp(-100, 100),
            HintEdit::CleanupFill(b) => cfg.mask_cleanup.fill_holes = b,
            HintEdit::CleanupFeather(v) => cfg.mask_cleanup.feather = finite_in(v, 0.0, 100.0, "Feather")?,
            HintEdit::MovieAsMask(_) | HintEdit::MovieAlphaChannel(_) => {}
        }
        Ok(())
    }
}

/// Availability of every Hint action: the one set of rules the action area, the graph menus and the Shot page use.
struct HintGates {
    local: Option<crate::models::Capabilities>,
    lazy: bool,
    miss_guide: Option<String>,
    miss_fin: Option<String>,
    src: Option<PathBuf>,
    /// Why a companion movie cannot be imported now (None = it can).
    import_reason: Option<String>,
    /// In-frame / marked-hint gate of the Hint run.
    gate: Option<String>,
    d_preview: crate::actions::Decision,
    d_track: crate::actions::Decision,
    d_guide: crate::actions::Decision,
    d_hint: crate::actions::Decision,
    d_extract: crate::actions::Decision,
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
        action: Action,
    ) {
        let waits = crate::actions::waits_for(self.ai.scheduler(), &clip.root, action);
        match job.and_then(|j| self.ai.submit_task(clip, j, waits, sig)) {
            Ok(_) => self.hint.note = Some(format!("{what} queued.")),
            Err(e) => self.hint_note(format!("{what} not queued: {e}")),
        }
    }

    fn hint_preview(&mut self) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        if let Some(e) = self.hint.marks_err.clone() {
            return self.hint_note(format!("Marked-frame choice not saved: {e}. Fix that (or change the checkbox back) first."));
        }
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
        self.submit_task_job(&clip, job, sig, "SAM preview", Action::SamPreview);
    }

    /// Queue a hint/pre-matte run from outside this page (the node graph's context menu), by node meaning:
    /// `pre_matte` = the guide producer, otherwise the final hint; the marked-frame checkboxes decide the frames.
    pub(super) fn hint_run_from_graph(&mut self, pre_matte: bool) {
        self.hint_run(if pre_matte { Run::Guide } else { Run::Hint });
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
        let (pre_marks, hint_marks) = (self.hint.cfg.pre_on_marks(), self.hint.cfg.hint_on_marks());
        let (steps, label) = match what {
            Run::Guide if pre_marks => (g.guide_marks(&marks, &mut id), "Pre-matte on marked frames"),
            Run::Guide => (g.guide(range, &mut id).map(|j| vec![j]), "Pre-matte"),
            Run::Hint if hint_marks => (g.hint_run(range, &marks, &mut id), "Hint on marked frames"),
            Run::Hint => (g.hint_run(range, &marks, &mut id), "Hint"),
        };
        let marks_only = match what {
            Run::Guide => pre_marks,
            Run::Hint => hint_marks,
        };
        let before = self.ai.task_ids(&clip.folder);
        // Settings and prompts are frozen in `steps` now; the run waits for the earlier same-clip steps.
        let action = match what {
            Run::Guide if self.hint.cfg.guide == Producer::Sam => Action::Track,
            Run::Guide => Action::PreMatte,
            Run::Hint => Action::Hint,
        };
        let waits = crate::actions::waits_for(self.ai.scheduler(), &clip.root, action);
        match steps.and_then(|s| self.ai.submit_chain_after(&clip, s, None, waits)) {
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

    /// The marked-frame checkboxes changed. Turning marked hints on saves Key's consumption as keyframes, turning
    /// them off saves full; the settings and the mode are written together (both or neither), and the Key draft
    /// follows at once. A failed write puts the checkboxes back, keeps the reason visible and holds new requests
    /// until a choice is saved. Never called by merely loading a clip.
    fn set_marked(&mut self, clip: &Clip, pre: bool, hint: bool) {
        let before = self.hint.cfg.clone();
        let mut cfg = before.clone();
        (cfg.marks_pre, cfg.marks_hint) = (pre, hint);
        let (was, now) = (before.hint_on_marks(), cfg.hint_on_marks());
        self.hint.cfg = cfg.clone();
        if was == now {
            self.hint.marks_err = None; // plain settings: saved with the others
            return;
        }
        let Some(pr) = self.project.as_ref() else { return };
        let cur = self.ai.draft(clip, pr, self.settings.saved()).params.hint_mode.clone();
        let from_preset = clip.native.get("hint_mode_src").and_then(Value::as_str) == Some("preset");
        let mode = if now { "keyframes" } else { "full" };
        match crate::persist::save_marked_hints(clip, &cfg, mode, (&cur, from_preset)) {
            Ok(()) => {
                self.hint.route_saved = cfg;
                self.hint.marks_err = None;
                self.hint.note = None;
                if let Some(pr) = self.project.as_ref() {
                    self.ai.follow_marked_mode(clip, pr, self.settings.saved(), mode);
                }
                self.refresh_after_save(&clip.folder);
            }
            Err(e) => {
                self.hint.cfg = before;
                self.hint_note(format!("Marked-frame choice not saved: {e}"));
                self.hint.marks_err = Some(e);
            }
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

    /// The movie to extract/import against: `project.rs` already resolves a legacy `Source/` copy first, then the
    /// original in place.
    fn source_movie(clip: &Clip) -> Option<PathBuf> {
        match &clip.source {
            Source::Video { path } => Some(path.clone()),
            _ => None,
        }
    }

    fn undo_annotation(&mut self) {
        if let (Some(prev), Some(c)) = (self.hint.undo.pop(), self.open_clip_snapshot()) {
            self.hint.ann = prev;
            match self.hint.ann.save(&c) {
                Ok(()) => {
                    self.hint.ann_saved = ann_sig(&self.hint.ann);
                    self.unsaved_annotations_done(&c.folder);
                }
                Err(e) => self.hint_note(format!("Annotations not saved: {e}")),
            }
        }
    }

    // ------------------------------------------------------------------- typed read + commands (Shot page)

    fn hint_gates(&self, clip: &Clip) -> HintGates {
        let local = self.ai.local_caps();
        // A worker that was never started reports nothing: actions queue and the worker starts with the job.
        let lazy = local.is_none() && self.ai.models_unknown();
        let ready = |k: TaskKind| task_ok(&local, lazy, k);
        // A busy clip no longer disables the buttons: they say "Queue ..." (actions::decide). Only a companion
        // import writing the hint folders still refuses new work.
        let ext = self.ai.external_writer(&clip.root).then(|| "A companion-sequence import is writing this clip's hints; wait for it to finish".to_string());
        let (guide, fin) = (self.hint.cfg.guide, self.hint.cfg.finish);
        // A producer whose model the app's check found missing is off, with the model named; the rest keeps working.
        let miss_guide = missing_guide(&local, lazy, &self.hint.cfg);
        let miss_fin = missing_final(&local, lazy, fin);
        let guide_ready = ready(guide.task()) && miss_guide.is_none();
        let fin_ready = fin.task().map_or(fin == Final::Direct && guide_ready, ready) && miss_fin.is_none();
        let guide_name = miss_guide.clone().unwrap_or_else(|| guide.label().to_string());
        let fin_name = miss_fin.clone().unwrap_or_else(|| fin.label().to_string());
        let bypass = self.hint.cfg.bypass;
        let sam_ok = ready(TaskKind::SamPreview) && ready(TaskKind::SamTrack) && miss_guide.is_none();
        let annotated = self.frame_annotated();
        let src = Self::source_movie(clip).filter(|_| !matches!(clip.source, Source::Sequence { .. }));
        let import_reason = if src.is_none() {
            Some("Companion movies need a movie source; import PNG/EXR sequences by placing them in AlphaHint".to_string())
        } else if !ready(TaskKind::ImportHint) {
            Some("the worker cannot import movies (FFmpeg)".to_string())
        } else {
            ext.clone()
        };
        let marks_in: Vec<usize> = match self.ai_open_range() {
            Ok(r) => clip.keyframes.iter().copied().filter(|k| (r.0..=r.1).contains(k)).collect(),
            Err(_) => vec![],
        };
        let gate = self.ai_open_range().ok().and_then(|r| hints::in_frame_gate(clip, &self.hint.cfg, r, &marks_in).err());
        // One decision per button (enabled, label, reason): the same rules the graph menus and Media tab use.
        let (sched, root) = (self.ai.scheduler(), &clip.root);
        let d_preview = decide(sched, root, Action::SamPreview, "Preview frame", if !sam_ok { Some(reason(&local, &guide_name)) } else if !annotated { Some("Add a point, box or paint on this frame first".into()) } else { ext.clone() });
        let d_track = decide(sched, root, Action::Track, "Track prompts", if bypass { Some("Bypass is on".into()) } else if !sam_ok { Some(reason(&local, &guide_name)) } else { ext.clone() });
        let d_guide = decide(sched, root, Action::PreMatte, "Generate pre-matte", if bypass { Some("Bypass is on".into()) } else if !guide_ready { Some(reason(&local, &guide_name)) } else { ext.clone() });
        let d_hint = decide(
            sched,
            root,
            Action::Hint,
            "Generate hint",
            if bypass {
                Some("Bypass is on".into())
            } else if fin == Final::Import {
                Some("Imported alpha has nothing to generate: choose a movie in Properties".into())
            } else if let Some(g) = &gate {
                Some(g.clone())
            } else if !fin_ready {
                Some(reason(&local, &fin_name))
            } else {
                ext.clone()
            },
        );
        let d_extract = decide(sched, root, Action::Extract, "Extract frames (validated)", if !ready(TaskKind::PrepareSource) { Some(reason(&local, "FFmpeg")) } else { ext.clone() });
        HintGates { local, lazy, miss_guide, miss_fin, src, import_reason, gate, d_preview, d_track, d_guide, d_hint, d_extract }
    }

    /// The playhead frame has prompts.
    fn frame_annotated(&self) -> bool {
        self.viewer
            .as_ref()
            .and_then(|v| v.index.stems.get(v.playhead))
            .and_then(|s| self.hint.ann.frames.get(s))
            .is_some_and(|f| !f.is_empty())
    }

    /// Typed snapshot of the Hint stage for the open shot; `None` while no shot is open or its hint state is
    /// not loaded yet. Reads cached state only.
    pub(crate) fn hint_view(&self) -> Option<HintView> {
        let clip = self.clip()?;
        let h = &self.hint;
        if h.folder.as_deref() != Some(clip.folder.as_str()) {
            return None;
        }
        let g = self.hint_gates(clip);
        let cfg = &h.cfg;
        let ok = |k: TaskKind| task_ok(&g.local, g.lazy, k);
        let (guide, fin) = (cfg.guide, cfg.finish);
        let sam = guide == Producer::Sam;
        let installed_variant = |v: &str| g.lazy || g.local.as_ref().is_some_and(|c| c.birefnet_variants.iter().any(|x| x.as_str() == v));
        let installed_sam = |id: &str| g.lazy || g.local.as_ref().is_some_and(|c| c.sam_models.iter().any(|x| x.as_str() == id));
        let tools = tools_for(guide);
        let stage = |frames: usize, node: Node, preview: Option<&crate::actions::Decision>, gen: &crate::actions::Decision| {
            let refusal = delete_refusal(self.ai.clip_active(&clip.root) || self.ai.task_busy(&clip.folder), self.ai.external_writer(&clip.root));
            HintStage {
                frames_on_disk: frames,
                stale: self.stage_pending(node),
                preview: preview.map(HintAct::from),
                generate: gen.into(),
                delete: HintAct { enabled: true, label: "Delete frames…".into(), reason: refusal.unwrap_or_default().into() },
            }
        };
        let marked = (fin != Final::Import).then(|| {
            let two = fin == Final::Matanyone2;
            MarkedView {
                pre: cfg.marks_pre,
                hint: cfg.marks_hint,
                two_rows: two,
                show_hint_row: two && cfg.marks_pre,
                in_frame_note: two && cfg.marks_pre && !cfg.marks_hint,
                key_uses_marked: cfg.hint_on_marks() && h.marks_err.is_none(),
            }
        });
        let movie = h.movie.clone().map(|path| MovieView { path, as_mask: h.movie_mask, alpha_channel: h.movie_alpha_channel });
        Some(HintView {
            bypass: cfg.bypass,
            worker_unknown: g.lazy,
            producer: guide,
            producers: Producer::ALL.iter().map(|&p| Choice { value: p, label: p.label().into(), ready: ok(p.task()) }).collect(),
            producer_missing: g.miss_guide.clone(),
            birefnet_variant: (guide == Producer::Birefnet).then(|| cfg.birefnet_variant.clone()),
            birefnet_variants: if guide == Producer::Birefnet { BIREFNET_VARIANTS.iter().map(|v| Choice { value: v.to_string(), label: v.to_string(), ready: installed_variant(*v) }).collect() } else { vec![] },
            sam_model: sam.then(|| cfg.sam_model.clone()),
            sam_models: if sam { SAM_MODELS.iter().map(|(id, l)| Choice { value: id.to_string(), label: l.to_string(), ready: installed_sam(*id) }).collect() } else { vec![] },
            chroma: (guide == Producer::Chroma).then(|| ChromaView {
                cfg: cfg.chroma.clone(),
                holdout: crate::hints::Holdout::load(clip).map(|ho| (ho.strokes.iter().filter(|s| s.fg).count(), ho.strokes.iter().filter(|s| !s.fg).count())),
            }),
            finish: fin,
            finishes: Final::ALL.iter().map(|&f| Choice { value: f, label: f.label().into(), ready: f.task().is_none_or(ok) }).collect(),
            finish_missing: g.miss_fin.clone(),
            videomama_chunk: (fin == Final::Videomama).then_some(cfg.videomama_chunk),
            marked_gate: marked.as_ref().and(g.gate.clone()),
            marked,
            marks_err: h.marks_err.clone(),
            cleanup: cfg.mask_cleanup.clone(),
            cleanup_note: cleanup_note(fin),
            movie,
            tool: self.viewer.as_ref().map_or(Tool::None, |v| v.ann.tool),
            tools: tools.to_vec(),
            brush: self.viewer.as_ref().filter(|_| tools.iter().any(|t| !matches!(t.0, Tool::Pick | Tool::Box))).map(|v| v.ann.brush as f32),
            prompt_frames: h.ann.frames.values().filter(|f| !f.is_empty()).count(),
            frame_annotated: self.frame_annotated(),
            can_undo: !h.undo.is_empty(),
            prematte: stage(clip.mask_frames, Node::PreMatte, sam.then_some(&g.d_preview), if sam { &g.d_track } else { &g.d_guide }),
            hint: stage(clip.alpha_frames, Node::Hint, None, &g.d_hint),
            extract: g.src.is_some().then(|| (&g.d_extract).into()),
            import: h.movie.is_some().then(|| HintAct { enabled: g.import_reason.is_none(), label: "Import movie".into(), reason: g.import_reason.clone().unwrap_or_default() }),
            route: RouteView {
                overrides: h.route_ov.keys().cloned().collect(),
                reset_label: (!h.route_ov.is_empty()).then(|| Self::reset_label("these route fields", &h.route_ov)),
                unsaved: h.cfg != h.route_saved,
                save_failed: h.route_failed.is_some(),
            },
            note: h.note.clone(),
        })
    }

    /// Run one Hint command through the same paths the old panel used. `Err` = refused or failed (the reason is
    /// also in the Hint note, as before); nothing is changed on refusal.
    pub(crate) fn hint_cmd(&mut self, ctx: &egui::Context, c: HintCmd) -> Result<(), String> {
        let Some(clip) = self.open_clip_snapshot() else {
            return Err("Open a shot first.".into());
        };
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            return Err("This shot's hint settings are still loading.".into());
        }
        self.hint.cmd_err = None;
        let refuse = |d: &crate::actions::Decision| if d.enabled { Ok(()) } else { Err(d.reason.clone()) };
        match c {
            HintCmd::Edit(e) => return self.hint_edit(&clip, e),
            HintCmd::MarkedMode { pre, hint } => {
                if self.hint.cfg.finish == Final::Import {
                    return Err("An imported alpha has no marked-frame generation.".into());
                }
                if self.hint.cfg.bypass {
                    return Err("Bypass is on".into());
                }
                // Only MatAnyone2 can have marked pre-matte with full-range hints; the others use the same marks.
                let hint = hint || self.hint.cfg.finish != Final::Matanyone2;
                self.set_marked(&clip, pre, hint);
                return self.hint.marks_err.clone().map_or(Ok(()), Err);
            }
            HintCmd::ResetRoute => self.reset_route(&clip),
            HintCmd::Tool(t) => {
                let guide = self.hint.cfg.guide;
                let Some(v) = self.viewer.as_mut() else { return Err("No frames are open.".into()) };
                if t != Tool::None && !tool_allowed(guide, v.sam_overlay, t) {
                    return Err("That tool is not available for this producer (SAM tools need the SAM overlay on).".into());
                }
                v.ann.tool = if v.ann.tool == t { Tool::None } else { t };
                if v.ann.tool != Tool::None {
                    v.view = View::Input; // prompts are stored in source pixels of Input
                    v.ab = false;
                }
            }
            HintCmd::Brush(b) => {
                if !b.is_finite() {
                    return Err("Brush size must be a number.".into());
                }
                let Some(v) = self.viewer.as_mut() else { return Err("No frames are open.".into()) };
                v.ann.brush = b.clamp(2.0, 120.0) as f64;
            }
            HintCmd::UndoPrompt => {
                if self.hint.undo.is_empty() {
                    return Err("Nothing to undo.".into());
                }
                self.undo_annotation();
            }
            HintCmd::ClearPrompts { all } => {
                if self.hint.cfg.guide != Producer::Sam {
                    return Err("Only SAM takes prompts.".into());
                }
                let stem = self.viewer.as_ref().and_then(|v| v.index.stems.get(v.playhead).cloned());
                if !all && stem.is_none() {
                    return Err("No frame is selected.".into());
                }
                self.hint.undo.push(self.hint.ann.clone());
                match (all, stem) {
                    (true, _) => self.hint.ann.frames.clear(),
                    (false, Some(s)) => {
                        self.hint.ann.frames.remove(&s);
                    }
                    _ => {}
                }
                match self.hint.ann.save(&clip) {
                    Ok(()) => {
                        self.hint.ann_saved = ann_sig(&self.hint.ann);
                        self.unsaved_annotations_done(&clip.folder);
                    }
                    Err(e) => self.hint_note(format!("Annotations not saved: {e}")),
                }
            }
            HintCmd::PreviewSam => {
                let g = self.hint_gates(&clip);
                refuse(&g.d_preview)?;
                self.hint_preview();
            }
            HintCmd::GeneratePreMatte { marked } => {
                if marked != self.hint.cfg.pre_on_marks() {
                    return Err("The Marked frames choice differs from the request: change that row first.".into());
                }
                let g = self.hint_gates(&clip);
                refuse(if self.hint.cfg.guide == Producer::Sam { &g.d_track } else { &g.d_guide })?;
                self.hint_run(Run::Guide);
            }
            HintCmd::GenerateHint { marked } => {
                if marked != self.hint.cfg.hint_on_marks() {
                    return Err("The Marked frames choice differs from the request: change that row first.".into());
                }
                let g = self.hint_gates(&clip);
                refuse(&g.d_hint)?;
                self.hint_run(Run::Hint);
            }
            HintCmd::ExtractSource => {
                let g = self.hint_gates(&clip);
                let Some(src) = g.src.clone() else { return Err("This shot has no movie source to extract.".into()) };
                refuse(&g.d_extract)?;
                let job = Ctx { clip: &clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.prepare_source(&src);
                self.submit_task_job(&clip, Ok(job), 0, "Frame extraction", Action::Extract);
            }
            HintCmd::ImportMovie => {
                let g = self.hint_gates(&clip);
                if let Some(why) = g.import_reason.clone() {
                    return Err(why);
                }
                let (Some(m), Some(src)) = (self.hint.movie.clone(), g.src.clone()) else { return Err("Choose a companion movie first.".into()) };
                let job = Ctx { clip: &clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.import_hint(&m, &src, self.hint.movie_mask, self.hint.movie_alpha_channel);
                self.submit_task_job(&clip, Ok(job), 0, "Hint movie import", Action::Hint);
                self.hint.movie = None;
            }
            HintCmd::ChooseMovie => { self.request_pick(ctx, crate::ui::picker::PickKind::HintMovie); }
            HintCmd::CancelMovie => self.hint.movie = None,
            // Opens the existing dialog (scope, exact file plan, refusals); never deletes by itself.
            HintCmd::RequestDelete { prematte } => {
                self.hint.dele = Some(Del { folder: clip.folder.clone(), prematte, scope: DelScope::All, movies: false, plan_key: String::new(), plan: None, rx: None, done: None });
            }
            HintCmd::ClearLegacyHoldout => match crate::hints::Holdout::default().save(&clip) {
                Ok(()) => self.hint.note = Some("Chroma holdout cleared.".into()),
                Err(e) => self.hint_note(format!("Holdout not cleared: {e}")),
            },
            HintCmd::ResetChromaColour => {
                self.hint.cfg.chroma.screen_color = None;
                return self.save_route(&clip);
            }
        }
        self.hint.cmd_err.take().map_or(Ok(()), Err)
    }

    fn hint_edit(&mut self, clip: &Clip, e: HintEdit) -> Result<(), String> {
        match e {
            HintEdit::MovieAsMask(b) => return Ok(self.hint.movie_mask = b),
            HintEdit::MovieAlphaChannel(b) => return Ok(self.hint.movie_alpha_channel = b),
            _ => {}
        }
        let mut cfg = self.hint.cfg.clone();
        e.apply(&mut cfg)?;
        if cfg == self.hint.cfg {
            return Ok(());
        }
        self.hint.cfg = cfg; // shown at once; on a failed save it stays as unsaved (save_route)
        self.save_route(clip)
    }

    // ------------------------------------------------------------------- tool strip (beside the viewer)

    /// Vertical, Photoshop-style tool strip left of the viewer (Hint page). Icons are drawn with primitives so
    /// nothing depends on an icon font; every button has a tooltip and a selected state. Prompts (points, paint,
    /// erase, box) sit in the first group, Undo/Clear in a second group. Processing (SAM Preview/Track) is in the bottom
    /// action area and the brush size in Properties. Chroma gets exactly one tool: pick the screen colour. The holdout tools are not part
    /// of SAM's prompt set and never appear here.
    pub(super) fn hint_tool_strip(&mut self, ui: &mut Ui) {
        let Some(_clip) = self.open_clip_snapshot() else {
            return;
        };
        if self.tab != Tab::Hint {
            return;
        }
        let guide = self.hint.cfg.guide;
        let Some(v) = self.viewer.as_ref() else {
            return;
        };
        let tools = tools_for(guide);
        self.hint.strip_rects.clear();
        if tools.is_empty() {
            return; // automatic producers take no prompts: no empty column beside the viewer
        }
        let mut cmds: Vec<HintCmd> = vec![];
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
                            cmds.push(HintCmd::Tool(*t));
                        }
                    }
                    if guide == Producer::Sam {
                        ui.add_space(4.0);
                        ui.separator();
                        if strip_button(ui, false, Icon::Undo, "Undo the last prompt edit").clicked() {
                            cmds.push(HintCmd::UndoPrompt);
                        }
                        if strip_button(ui, false, Icon::ClearFrame, "Clear this frame's prompts").clicked() {
                            cmds.push(HintCmd::ClearPrompts { all: false });
                        }
                        if strip_button(ui, false, Icon::ClearAll, "Clear all SAM prompts (source and generated masks are untouched)").clicked() {
                            cmds.push(HintCmd::ClearPrompts { all: true });
                        }
                    }
                });
            });
        let ctx = ui.ctx().clone();
        for c in cmds {
            let _ = self.hint_cmd(&ctx, c);
        }
    }

    // ------------------------------------------------------------------- page bar (under the viewer)

    /// The Hint page's processing actions, one group per module in the shared action area: Pre-matte | Final hint,
    /// then Source / Import when there is something to prepare. The inspector only chooses; every run starts here.
    pub(super) fn hint_actions(&mut self, ui: &mut Ui, clip: &Clip) {
        let g = self.hint_gates(clip);
        let (guide, fin) = (self.hint.cfg.guide, self.hint.cfg.finish);
        let bypass = self.hint.cfg.bypass;
        let sam = guide == Producer::Sam;
        let tool_on = sam && self.viewer.as_ref().is_some_and(|v| v.ann.tool != Tool::None);
        let src = g.src.clone();
        let movie = self.hint.movie.clone();
        let ctx = ui.ctx().clone();
        let (mut run, mut preview, mut extract, mut import, mut cancel_import) = (None, false, false, false, false);
        let mut del_open: Option<bool> = None;
        let gate = g.gate.clone();
        let import_ok = g.import_reason.is_none();
        let (d_preview, d_track, d_guide, d_hint, d_extract) = (&g.d_preview, &g.d_track, &g.d_guide, &g.d_hint, &g.d_extract);
        let this = &*self;
        ui.columns(2, |cols| {
            let ui = &mut cols[0];
            let stale = this.stale(&ctx, Node::PreMatte);
            let r = ui.scope(|ui| {
                card(ui, &format!("Pre-matte · {}", guide.label()), stale, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        if sam {
                            // SAM preview/tracking belong to SAM only: Track is the pre-matte run, so no second Generate.
                            preview = decided_button(this, ui, d_preview, Node::PreMatte, false, "Run SAM on this frame's prompts (nothing is written)");
                            if decided_button(this, ui, d_track, Node::PreMatte, true, "Track the saved prompts over In/Out into the pre-matte") {
                                run = Some(Run::Guide);
                            }
                        } else if decided_button(this, ui, d_guide, Node::PreMatte, true, "Run the Pre-matte over In/Out into VideoMamaMaskHint") {
                            run = Some(Run::Guide);
                        }
                    });
                    if sam {
                        reason_line(ui, &[d_preview, d_track]);
                    } else {
                        reason_line(ui, &[d_guide]);
                    }
                    ui.label(RichText::new(format!("{} frames on disk", clip.mask_frames)).color(theme::text_dim(ui)).size(12.0));
                    if ui.small_button("Delete frames…").on_hover_text("Delete generated pre-matte or hint frames from disk").clicked() {
                        del_open = Some(true);
                    }
                });
            });
            if tool_on {
                ui.painter().rect_stroke(r.response.rect, 6.0, egui::Stroke::new(2.0, theme::accent_mark(ui)), egui::StrokeKind::Outside);
            }
            let ui = &mut cols[1];
            let stale = this.stale(&ctx, Node::Hint);
            card(ui, &format!("Hint · {}", fin.label()), stale, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if decided_button(this, ui, d_hint, Node::Hint, true, if fin.needs_guide() { "Refine the Pre-matte over In/Out into AlphaHint (needs the Pre-matte first)" } else { "Publish the Pre-matte straight into AlphaHint" }) {
                        run = Some(Run::Hint);
                    }
                });
                reason_line(ui, &[d_hint]);
                ui.label(RichText::new(format!("{} frames on disk", clip.alpha_frames)).color(theme::text_dim(ui)).size(12.0));
                if ui.small_button("Delete frames…").on_hover_text("Delete generated pre-matte or hint frames from disk").clicked() {
                    del_open = Some(false);
                }
            });
        });
        if let Some(prematte) = del_open {
            let _ = self.hint_cmd(&ctx, HintCmd::RequestDelete { prematte });
        }
        self.hint_delete_ui(ui, clip);
        let mut marked = None;
        if fin != Final::Import {
            let (mut p, mut h) = (self.hint.cfg.marks_pre, self.hint.cfg.marks_hint);
            ui.add_enabled_ui(!bypass, |ui| {
                if fin == Final::Matanyone2 {
                    theme::checkbox(ui, &mut p, "Pre-matte on marked frames");
                    if p {
                        theme::checkbox(ui, &mut h, "Use the same marked frames as hints");
                        if !h {
                            ui.label(RichText::new("Full-range hints use In-frame pre-matte.").color(theme::text_dim(ui)).size(12.0));
                        }
                    }
                } else {
                    let mut c = p;
                    theme::checkbox(ui, &mut c, "Pre-matte and hint on marked frames");
                    if c != p {
                        (p, h) = (c, true);
                    }
                }
            });
            if let Some(g) = &gate {
                ui.label(RichText::new(g.as_str()).color(theme::warn(ui)).size(12.0));
            }
            if self.hint.cfg.hint_on_marks() && self.hint.marks_err.is_none() {
                ui.label(RichText::new("Key will use marked hints.").color(theme::text_dim(ui)).size(12.0));
            }
            if (p, h) != (self.hint.cfg.marks_pre, self.hint.cfg.marks_hint) {
                marked = Some((p, h));
            }
        }
        if let Some((pre, hint)) = marked {
            let _ = self.hint_cmd(&ctx, HintCmd::MarkedMode { pre, hint });
        }
        if src.is_some() || movie.is_some() {
            card(ui, "Source / Import", None, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if movie.is_some() {
                        let tip = g.import_reason.clone().unwrap_or_default();
                        import = theme::accent(ui, import_ok, |ui| ui.add(egui::Button::new("Import movie"))).on_disabled_hover_text(tip).clicked();
                        cancel_import = ui.button("Cancel").clicked();
                        if let Some(m) = &movie {
                            ui.label(RichText::new(m.display().to_string()).monospace().size(11.0).color(theme::text_dim(ui)));
                        }
                    }
                    if src.is_some() {
                        extract = decided_button(&*self, ui, d_extract, Node::Media, false, "Reads the source movie from where it is and writes a verified Frames/ sequence; cancellable and resumable");
                    }
                });
                reason_line(ui, &[d_extract]);
            });
        }
        let (pre_marks, hint_marks) = (self.hint.cfg.pre_on_marks(), self.hint.cfg.hint_on_marks());
        let mut cmds: Vec<HintCmd> = vec![];
        if preview {
            cmds.push(HintCmd::PreviewSam);
        }
        if extract {
            cmds.push(HintCmd::ExtractSource);
        }
        if import {
            cmds.push(HintCmd::ImportMovie);
        }
        if cancel_import {
            cmds.push(HintCmd::CancelMovie);
        }
        match run {
            Some(Run::Guide) => cmds.push(HintCmd::GeneratePreMatte { marked: pre_marks }),
            Some(Run::Hint) => cmds.push(HintCmd::GenerateHint { marked: hint_marks }),
            None => {}
        }
        for c in cmds {
            let _ = self.hint_cmd(&ctx, c);
        }
    }

    /// True while the generated-frames delete dialog is open (global shortcuts must not run then).
    pub(super) fn hint_delete_open(&self) -> bool {
        self.hint.dele.is_some()
    }

    /// The delete plan as a modal over the Shot page (the old panel drew it inline). Closing (X / Esc) is ignored while the helper runs.
    pub(super) fn hint_delete_dialog(&mut self, ctx: &egui::Context, clip: &Clip) {
        if self.hint.dele.is_none() {
            return;
        }
        let out = crate::ui::controls::dialog::Dialog::new("hint-delete-dialog", "Delete generated frames", crate::ui::controls::dialog::DialogSize::M)
            .body(|body| self.hint_delete_ui(body, clip))
            .show(ctx);
        if out.closed && self.hint.dele.as_ref().is_some_and(|d| d.rx.is_none()) {
            self.hint.dele = None;
        }
    }

    /// "Delete generated frames": Pre-matte or Hint, current / marked / all frames, the exact files and count,
    /// companion movies as a separate unticked choice. The deletion runs in the app-side helper, never the worker.
    fn hint_delete_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        let Some(mut d) = self.hint.dele.take() else { return };
        if d.folder != clip.folder {
            return;
        }
        let refusal = delete_refusal(self.ai.clip_active(&clip.root) || self.ai.task_busy(&clip.folder), self.ai.external_writer(&clip.root));
        let current = self.viewer.as_ref().and_then(|v| v.index.stems.get(v.playhead).cloned());
        let marked = marked_keyframe_stems(&stems_of(clip), &clip.keyframes);
        let stems = scope_stems(d.scope, current.as_deref(), &marked);
        let key = format!("{}|{:?}|{}|{:?}", d.prematte, d.scope, d.movies, stems);
        if let Some((is_delete, rx)) = &d.rx {
            match rx.try_recv() {
                Ok(r) => {
                    let is_delete = *is_delete;
                    d.rx = None;
                    if is_delete {
                        d.done = Some(self.mask_delete_done(ui.ctx(), clip, r));
                        d.plan_key.clear();
                        d.plan = None;
                    } else {
                        d.plan = Some(r.map(|v| DelPlan { files: strings(&v, "files"), movies: strings(&v, "movies"), blocked: v.get("blocked").and_then(|b| b.as_array()).map_or(0, Vec::len) }));
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(100)),
                Err(_) => {
                    d.rx = None;
                    d.plan = Some(Err("the delete helper ended without an answer; nothing was changed".into()));
                }
            }
        }
        let empty_scope = stems.as_ref().is_some_and(|s| s.is_empty());
        if d.rx.is_none() && d.done.is_none() && refusal.is_none() && !empty_scope && d.plan_key != key {
            d.plan_key = key.clone();
            d.plan = None;
            match self.ai.scheduler().local_target() {
                Some(t) => d.rx = Some((false, crate::scheduler::spawn_mask_helper(t, delete_request("plan", &new_id(), &clip.root, d.prematte, &stems, d.movies)))),
                None => d.plan = Some(Err("no enabled local worker to run the helper".into())),
            }
        }
        let (mut close, mut go) = (false, false);
        egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Delete generated frames").strong());
            if let Some(msg) = &d.done {
                ui.label(msg.as_str());
                close = ui.button("Close").clicked();
                return;
            }
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut d.prematte, true, "Pre-matte (VideoMamaMaskHint)");
                ui.radio_value(&mut d.prematte, false, "Hint (AlphaHint)");
            });
            ui.horizontal_wrapped(|ui| {
                ui.radio_value(&mut d.scope, DelScope::Current, "Current frame");
                ui.radio_value(&mut d.scope, DelScope::Marked, "Marked frames");
                ui.radio_value(&mut d.scope, DelScope::All, "All frames");
            });
            theme::checkbox(ui, &mut d.movies, "Also delete the companion movie(s) of this asset");
            ui.label(RichText::new("Prompts, marks, the other asset, renders, models and the prepared source frames are kept.").color(theme::text_dim(ui)).size(12.0));
            if let Some(r) = refusal {
                ui.colored_label(theme::warn(ui), r);
            } else if empty_scope {
                ui.colored_label(theme::warn(ui), if d.scope == DelScope::Current { "No frame is selected." } else { "No frames are marked." });
            }
            let mut can = false;
            match &d.plan {
                _ if refusal.is_some() || empty_scope => {}
                Some(Ok(p)) => {
                    ui.label(format!("{} frame file(s){}", p.files.len(), if p.movies.is_empty() { String::new() } else { format!(" and {} movie(s)", p.movies.len()) }));
                    egui::ScrollArea::vertical().max_height(110.0).id_salt("del_files").show(ui, |ui| {
                        for f in p.files.iter().chain(p.movies.iter()) {
                            ui.label(RichText::new(f.as_str()).monospace().size(11.0));
                        }
                    });
                    if p.blocked > 0 {
                        ui.colored_label(theme::warn(ui), format!("{} file(s) are links that leave the clip folder and will not be deleted.", p.blocked));
                    }
                    can = !p.files.is_empty() || !p.movies.is_empty();
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::warn(ui), e.as_str());
                }
                None => {
                    ui.label("Checking the files…");
                }
            }
            ui.horizontal_wrapped(|ui| {
                go = ui.add_enabled(can && d.rx.is_none(), egui::Button::new("Delete")).clicked();
                // Cancel stays off while the helper runs: dropping `d` here would lose its answer and the texture/clip refresh.
                close = ui.add_enabled(d.rx.is_none(), egui::Button::new("Cancel")).on_disabled_hover_text("Deleting… wait for it to finish").clicked();
            });
        });
        if go {
            match self.ai.scheduler().local_target() {
                Some(t) => d.rx = Some((true, crate::scheduler::spawn_mask_helper(t, delete_request("delete", &new_id(), &clip.root, d.prematte, &stems, d.movies)))),
                None => d.done = Some("Nothing was deleted: no enabled local worker to run the helper.".into()),
            }
        }
        if !close {
            self.hint.dele = Some(d);
        }
    }

    /// The helper answered the delete: forget everything built from the deleted files, rescan the clip (also after
    /// a partial failure) and say what happened.
    fn mask_delete_done(&mut self, ctx: &egui::Context, clip: &Clip, r: Result<serde_json::Value, String>) -> String {
        let (deleted, errors) = match &r {
            Ok(v) => (strings(v, "deleted"), v.get("errors").and_then(|e| e.as_array()).cloned().unwrap_or_default()),
            Err(e) => return format!("Nothing was deleted: {e}"),
        };
        let paths: Vec<PathBuf> = deleted.iter().map(PathBuf::from).collect();
        if let Some(v) = &mut self.viewer {
            v.forget_paths(&paths);
        }
        let ids = self.ai.forget_masks(&clip.folder);
        self.hint.dead_previews.extend(ids);
        self.refresh_clip(ctx, &clip.folder);
        let mut msg = format!("Deleted {} file(s).", deleted.len());
        for e in errors.iter().take(5) {
            msg.push_str(&format!("\nNot deleted: {} — {}", e["path"].as_str().unwrap_or("?"), e["message"].as_str().unwrap_or("?")));
        }
        msg
    }

    /// Status region (left of the action region). Job progress, notes and the hint lock live in the activity
    /// strip; the generate buttons live in the action region (`hint_actions`).
    pub(super) fn hint_tools(&mut self, ui: &mut Ui) {
        ui.label(
            RichText::new("Pre-matte and hint generation run on this computer (the local worker). Key then runs on the client assigned to this clip.")
                .color(theme::text_dim(ui))
                .size(11.5),
        );
    }

    pub(super) fn hint_note_text(&self) -> Option<&str> {
        self.hint.note.as_deref()
    }

    // ------------------------------------------------------------------- properties

    pub(super) fn hint_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let caps = self.ai.local_caps();
        let lazy = caps.is_none() && self.ai.models_unknown();
        let ctx = ui.ctx().clone();
        let mut edits: Vec<HintEdit> = vec![];
        let (pre_stale, hint_stale) = (self.stale(&ctx, Node::PreMatte), self.stale(&ctx, Node::Hint));
        let mut byp = self.hint.cfg.bypass;
        let bypass = theme::checkbox(ui, &mut byp, "Bypass hint (keeps every setting and asset)");
        self.hint.preset_rects.push(("bypass", bypass.rect));
        if byp != self.hint.cfg.bypass {
            let _ = self.hint_cmd(&ctx, HintCmd::Edit(HintEdit::Bypass(byp)));
        }
        ui.add_space(4.0);
        let ok = |k: TaskKind| task_ok(&caps, lazy, k);
        ui.add_enabled_ui(!self.hint.cfg.bypass, |ui| {
            card(ui, "Pre-matte", pre_stale, |ui| {
                pair(ui, "Producer", |ui| {
                    let mut g = self.hint.cfg.guide;
                    egui::ComboBox::from_id_salt("hint_guide").selected_text(g.label()).show_ui(ui, |ui| {
                        for p in Producer::ALL {
                            let l = if ok(p.task()) { p.label().to_string() } else { format!("{} (not ready)", p.label()) };
                            ui.selectable_value(&mut g, p, l);
                        }
                    });
                    if g != self.hint.cfg.guide {
                        edits.push(HintEdit::Producer(g));
                    }
                });
                if let Some(m) = missing_guide(&caps, lazy, &self.hint.cfg) {
                    ui.label(RichText::new(format!("{m} is missing — download it on the Models page")).color(theme::FOCUS).size(12.0));
                }
                match self.hint.cfg.guide {
                    Producer::Birefnet => {
                        let have = caps.as_ref().map(|c| c.birefnet_variants.clone()).unwrap_or_default();
                        pair(ui, "Variant", |ui| {
                            let mut sel = self.hint.cfg.birefnet_variant.clone();
                            egui::ComboBox::from_id_salt("birefnet_variant").selected_text(sel.clone()).show_ui(ui, |ui| {
                                for v in BIREFNET_VARIANTS {
                                    let l = if have.iter().any(|h| h == v) { v.to_string() } else { format!("{v} (not installed)") };
                                    ui.selectable_value(&mut sel, v.to_string(), l);
                                }
                            });
                            if sel != self.hint.cfg.birefnet_variant {
                                edits.push(HintEdit::BirefnetVariant(sel));
                            }
                        });
                    }
                    Producer::Chroma => self.chroma_controls(ui, clip),
                    Producer::Sam => {
                        let sam = caps.as_ref().map(|c| c.sam_models.clone()).unwrap_or_default();
                        pair(ui, "Tracker model", |ui| {
                            let mut sel = self.hint.cfg.sam_model.clone();
                            egui::ComboBox::from_id_salt("sam_model").selected_text(SAM_MODELS.iter().find(|m| m.0 == sel).map_or("Select model", |m| m.1)).show_ui(ui, |ui| {
                                for (id, l) in SAM_MODELS {
                                    let t = if sam.iter().any(|s| s == id) { l.to_string() } else { format!("{l} (not installed)") };
                                    ui.selectable_value(&mut sel, id.to_string(), t);
                                }
                            });
                            if sel != self.hint.cfg.sam_model {
                                edits.push(HintEdit::SamModel(sel));
                            }
                        });
                    }
                    Producer::Gvm => {}
                }
            });
            ui.add_space(6.0);
            card(ui, "Final hint", hint_stale, |ui| {
                pair(ui, "Method", |ui| {
                    let mut f = self.hint.cfg.finish;
                    egui::ComboBox::from_id_salt("hint_final").selected_text(f.label()).show_ui(ui, |ui| {
                        for x in Final::ALL {
                            let l = if x.task().is_none_or(ok) { x.label().to_string() } else { format!("{} (not ready)", x.label()) };
                            ui.selectable_value(&mut f, x, l);
                        }
                    });
                    if f != self.hint.cfg.finish {
                        edits.push(HintEdit::Finish(f));
                    }
                });
                if let Some(m) = missing_final(&caps, lazy, self.hint.cfg.finish) {
                    ui.label(RichText::new(format!("{m} is missing — download it on the Models page")).color(theme::FOCUS).size(12.0));
                }
                match self.hint.cfg.finish {
                    Final::Videomama => {
                        pair(ui, "Chunk size", |ui| edits.extend(slide(ui, self.hint.cfg.videomama_chunk, 1..=64, "").map(HintEdit::VideomamaChunk)));
                        ui.label(RichText::new("Refines the Pre-matte: generate it over In/Out first.").color(theme::text_dim(ui)).size(12.0));
                    }
                    Final::Matanyone2 => {
                        ui.label(RichText::new("Refines the Pre-matte, seeded from its In frame.").color(theme::text_dim(ui)).size(12.0));
                    }
                    Final::Direct | Final::Import => {}
                }
            });
        });
        for e in edits.drain(..) {
            let _ = self.hint_cmd(&ctx, HintCmd::Edit(e));
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
                    let mut mk = self.hint.movie_mask;
                    ui.radio_value(&mut mk, false, "AlphaHint");
                    ui.radio_value(&mut mk, true, "Pre-matte");
                    if mk != self.hint.movie_mask {
                        edits.push(HintEdit::MovieAsMask(mk));
                    }
                });
                pair(ui, "Matte channel", |ui| {
                    let mut a = self.hint.movie_alpha_channel;
                    theme::checkbox(ui, &mut a, "In the alpha channel (else gray)");
                    if a != self.hint.movie_alpha_channel {
                        edits.push(HintEdit::MovieAlphaChannel(a));
                    }
                });
                ui.label(RichText::new("Run Import from the Source / Import group in the action area.").color(theme::text_dim(ui)).size(12.0));
            }
        });
        for e in edits {
            let _ = self.hint_cmd(&ctx, HintCmd::Edit(e));
        }
        if !self.hint.cfg.bypass {
            ui.add_space(4.0);
            ui.label(RichText::new("Consumption mode and quality are below (applied by Key).").color(theme::text_dim(ui)).size(12.0));
        }
    }

    /// Brush size and the mask cleanup shared by every producer/refiner (after the mode controls).
    pub(super) fn hint_cleanup_props(&mut self, ui: &mut Ui, _clip: &Clip) {
        let ctx = ui.ctx().clone();
        let mut cmds: Vec<HintCmd> = vec![];
        let stale = self.stale(&ctx, Node::Hint);
        ui.add_space(6.0);
        if tools_for(self.hint.cfg.guide).iter().any(|t| !matches!(t.0, Tool::Pick | Tool::Box)) {
            if let Some(v) = self.viewer.as_ref() {
                card(ui, "Brush", None, |ui| {
                    let mut b = v.ann.brush as f32;
                    pair(ui, "Size", |ui| {
                        fill_slider(ui);
                        ui.add(egui::Slider::new(&mut b, 2.0..=120.0).suffix(" px")).on_hover_text("Size of points, paint and erase strokes");
                    });
                    if b as f64 != v.ann.brush {
                        cmds.push(HintCmd::Brush(b));
                    }
                });
                ui.add_space(6.0);
            }
        }
        let cleanup_note = cleanup_note(self.hint.cfg.finish);
        let mc = self.hint.cfg.mask_cleanup.clone();
        card(ui, "Mask cleanup", stale, |ui| {
            ui.label(RichText::new(cleanup_note).color(theme::text_dim(ui)).size(12.0));
            pair(ui, "Grow / shrink", |ui| cmds.extend(slide(ui, mc.grow, -100..=100, " px").map(|v| HintCmd::Edit(HintEdit::CleanupGrow(v)))));
            pair(ui, "Fill holes", |ui| {
                let mut f = mc.fill_holes;
                theme::checkbox(ui, &mut f, "Fill holes");
                if f != mc.fill_holes {
                    cmds.push(HintCmd::Edit(HintEdit::CleanupFill(f)));
                }
            });
            pair(ui, "Feather", |ui| cmds.extend(slide(ui, mc.feather, 0.0..=100.0, " px").map(|v| HintCmd::Edit(HintEdit::CleanupFeather(v)))));
        });
        for c in cmds {
            let _ = self.hint_cmd(&ctx, c);
        }
    }

    fn chroma_controls(&mut self, ui: &mut Ui, clip: &Clip) {
        let ctx = ui.ctx().clone();
        let c = self.hint.cfg.chroma.clone();
        let mut cmds: Vec<HintCmd> = vec![];
        pair(ui, "Screen", |ui| {
            let mut t = c.screen_type.clone();
            ui.selectable_value(&mut t, "green".to_string(), "Green");
            ui.selectable_value(&mut t, "blue".to_string(), "Blue");
            if t != c.screen_type {
                cmds.push(HintCmd::Edit(HintEdit::ChromaScreen(t)));
            }
        });
        match c.screen_color {
            Some(rgb) => {
                pair(ui, "Screen colour", |ui| {
                    ui.label(format!("{:?}", rgb));
                    if ui.small_button("auto").clicked() {
                        cmds.push(HintCmd::ResetChromaColour);
                    }
                });
            }
            None => {
                ui.label(RichText::new("Screen colour: automatic. Use the “Pick colour” tool on the frame to sample one.").color(theme::text_dim(ui)).size(12.0));
            }
        }
        pair(ui, "Strength", |ui| cmds.extend(slide(ui, c.strength, 0.1..=10.0, "").map(|v| HintCmd::Edit(HintEdit::ChromaStrength(v)))));
        pair(ui, "Clip black", |ui| cmds.extend(slide(ui, c.clip_black, 0.0..=1.0, "").map(|v| HintCmd::Edit(HintEdit::ChromaClipBlack(v)))));
        pair(ui, "Clip white", |ui| cmds.extend(slide(ui, c.clip_white, 0.0..=1.0, "").map(|v| HintCmd::Edit(HintEdit::ChromaClipWhite(v)))));
        pair(ui, "Shrink / grow", |ui| cmds.extend(slide(ui, c.shrink_grow, -100..=100, " px").map(|v| HintCmd::Edit(HintEdit::ChromaShrinkGrow(v)))));
        pair(ui, "Edge blur", |ui| cmds.extend(slide(ui, c.edge_blur, 0..=100, " px").map(|v| HintCmd::Edit(HintEdit::ChromaEdgeBlur(v)))));
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
                    .color(theme::text_dim(ui))
                    .size(11.5),
                );
                if ui
                    .small_button("Clear existing holdout…")
                    .on_hover_text("Deletes the stored chroma holdout strokes for this clip. Hints and prompts are untouched.")
                    .clicked()
                {
                    cmds.push(HintCmd::ClearLegacyHoldout);
                }
            }
            Ok(_) => {}
            Err(e) => {
                ui.colored_label(
                    theme::warn(ui),
                    RichText::new(format!("Existing chroma holdout unreadable: {e}"))
                        .size(11.5),
                );
            }
        }
        for c in cmds {
            let _ = self.hint_cmd(&ctx, c);
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
    /// Reads `deliver_view`; every control is a `DeliverCmd` run through `deliver_run` (the Shot page's path).
    pub(super) fn deliver_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        self.sync_deliveries(clip);
        let ctx = ui.ctx().clone();
        let stale = self.stale(&ctx, Node::Out(View::Fg));
        let Some(v) = self.deliver_view() else { return };
        if !v.outputs.iter().any(|o| o.enabled) {
            ui.label(RichText::new("No output is enabled in the Key outputs.").color(theme::text_dim(ui)));
            return;
        }
        let mut cmds: Vec<DeliverCmd> = vec![];
        let mut rects: Vec<(String, egui::Rect)> = vec![];
        let mut fmt_rects: Vec<(String, egui::Rect)> = vec![];
        for ov in v.outputs.iter().filter(|o| o.enabled) {
            let (key, output) = (ov.output.key(), ov.output);
            let card = ui.scope(|ui| card(ui, ov.title, stale, |ui| {
                pair(ui, "Deliver as", |ui| {
                    if ui.selectable_label(ov.video.is_none(), "Image sequence").clicked() {
                        cmds.push(DeliverCmd::SetKind { output, video: false });
                    }
                    let b = ui.selectable_label(ov.video.is_some(), "Video file");
                    fmt_rects.push((format!("{key}_video"), b.rect));
                    if b.clicked() && ov.video.is_none() {
                        cmds.push(DeliverCmd::SetKind { output, video: true });
                    }
                });
                let Some(vv) = &ov.video else {
                    pair(ui, "Format", |ui| {
                        let e = ui.selectable_label(ov.exr, "EXR");
                        let p = ui.selectable_label(!ov.exr, "PNG");
                        fmt_rects.push((format!("{key}_exr"), e.rect));
                        fmt_rects.push((format!("{key}_png"), p.rect));
                        if e.clicked() && !ov.exr {
                            cmds.push(DeliverCmd::SetSeqFormat { output, exr: true });
                        }
                        if p.clicked() && ov.exr {
                            cmds.push(DeliverCmd::SetSeqFormat { output, exr: false });
                        }
                    });
                    return;
                };
                if vv.custom {
                    // An unknown container/codec pair is reset to the default MP4 (as before).
                    cmds.push(DeliverCmd::SetContainer { output, container: "mp4".into() });
                    return;
                }
                pair(ui, "File type", |ui| {
                    for c in &vv.containers {
                        if ui.selectable_label(vv.container == c.value, c.label.as_str()).clicked() && vv.container != c.value {
                            cmds.push(DeliverCmd::SetContainer { output, container: c.value.clone() });
                        }
                    }
                });
                pair(ui, "Codec", |ui| {
                    egui::ComboBox::from_id_salt(("codec", key))
                        .width(ui.available_width()).truncate()
                        .selected_text(vv.codec_label.as_str())
                        .show_ui(ui, |ui| {
                            for c in &vv.codecs {
                                if ui.selectable_label(c.value == vv.codec, c.label.as_str()).clicked() && c.value != vv.codec {
                                    cmds.push(DeliverCmd::SetCodec { output, codec: c.value.clone() });
                                }
                            }
                        });
                });
                pair(ui, "Quality", |ui| {
                    ui.spacing_mut().slider_width = (ui.available_width() - 100.0).max(40.0);
                    let mut q = vv.quality;
                    ui.add(egui::Slider::new(&mut q, vv.min..=vv.max).text(vv.quality_name)).on_hover_text(&vv.quality_note);
                    if q != vv.quality {
                        cmds.push(DeliverCmd::SetQuality { output, quality: q as f32 });
                    }
                });
                ui.label(RichText::new(&vv.quality_note).color(theme::text_dim(ui)).size(11.5));
                if let (DeliverOutput::Processed, Some(w)) = (output, vv.alpha_warning) {
                    ui.colored_label(theme::warn(ui), w);
                }
            }));
            rects.push((key.to_string(), card.response.rect));
            ui.add_space(6.0);
        }
        card(ui, "EXR", stale, |ui| {
            pair(ui, "Compression", |ui| {
                let mut cur = v.compression.value.clone();
                egui::ComboBox::from_id_salt("exr_comp_deliver").selected_text(cur.clone()).show_ui(ui, |ui| {
                    for c in &v.compression.choices {
                        ui.selectable_value(&mut cur, c.to_string(), *c);
                    }
                });
                if cur != v.compression.value {
                    cmds.push(DeliverCmd::SetCompression(cur));
                }
            });
            ui.label(RichText::new(v.compression.shared_note).color(theme::text_dim(ui)).size(12.0));
            if let Some(n) = v.compression.lossy_note {
                ui.colored_label(theme::warn(ui), n);
            }
        });
        if v.has_video {
            ui.add_space(6.0);
            card(ui, "Video options (all video outputs)", stale, |ui| {
                // `None` = each clip's own source fps; an override is stored only when chosen.
                let mut source_fps = v.fps.stored.is_none();
                pair(ui, "Frame rate", |ui| {
                    if theme::checkbox(ui, &mut source_fps, "Use source").changed() {
                        cmds.push(if source_fps { DeliverCmd::UseSourceFps } else { DeliverCmd::SetFps(v.fps.source) });
                    }
                    if let Some(fps) = v.fps.stored {
                        let mut f = fps;
                        ui.add(egui::DragValue::new(&mut f).speed(0.01).range(0.001..=1000.0).suffix(" fps"));
                        if f != fps {
                            cmds.push(DeliverCmd::SetFps(f));
                        }
                    }
                });
                pair(ui, "Range", |ui| {
                    ui.scope(|ui| {
                        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);
                        let mut done = v.done_frames;
                        if theme::checkbox(ui, &mut done, "Export the partial result (explicit range, no gaps)").changed() {
                            cmds.push(DeliverCmd::SetRange { done_frames: done });
                        }
                    });
                });
                pair(ui, "Folder", |ui| {
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(v.destination.resolved.as_str()).wrap());
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("Choose…").clicked() {
                                cmds.push(DeliverCmd::ChooseExportDir);
                            }
                            if v.destination.dir.is_some() && ui.button("Project default").clicked() {
                                cmds.push(DeliverCmd::ResetExportDir);
                            }
                        });
                    });
                });
                ui.label(RichText::new("Audio is not included. Files: <clip>_<output>_<in>-<out>.<mp4|mov|webm>; an existing file is never replaced.").color(theme::text_dim(ui)).size(12.0));
            });
        }
        // Each field the user really changed is the shot's own and keeps winning over a Batch preset until Reset;
        // the write itself is the per-frame step in `hint_pending_refresh` (once the pointer settles).
        if let Some(label) = &v.reset_label {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Delivery, this shot's own (win over a Batch preset): {}", v.overridden.join(", "))).color(theme::text_dim(ui)).size(12.0));
                if ui.small_button(label).clicked() {
                    cmds.push(DeliverCmd::ResetDelivery);
                }
            });
        }
        (self.hint.deliver_rects, self.hint.deliver_fmt_rects) = (rects, fmt_rects);
        for c in cmds {
            let _ = self.deliver_run(&ctx, c);
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(theme::warn(ui)).size(12.0));
        }
    }

    /// Deliver action when any output is a video: Key, then every video output (core queues the chain).
    pub(super) fn deliver_action(&mut self, ui: &mut Ui, clip: &Clip) {
        self.sync_deliveries(clip);
        let ctx = ui.ctx().clone();
        // A confirmation only ever applies to the shot, results, range and settings it was opened for.
        let g = self.deliver_gates(clip);
        if self.hint.render_confirm.is_some() && !self.confirm_valid(clip, &g) {
            self.hint.render_confirm = None;
            self.hint_note("The confirmation was closed because the shot, its results or the Deliver settings changed. Press the button again.");
        }
        let Some(v) = self.deliver_view() else { return };
        let mut cmd: Option<DeliverCmd> = None;
        let go = ui.vertical_centered(|ui| ui.add_enabled(v.workflow.enabled, egui::Button::new("Run full workflow").min_size(egui::vec2(220.0, 30.0)))).inner;
        self.hint.deliver_rects.push(("render".into(), go.rect));
        let go = go.on_hover_text("Runs this shot's whole workflow: pre-matte, hint, Key, then every enabled video output. To encode from an existing Key result without running Key again, use “Export existing Key result”.").on_disabled_hover_text(v.workflow.reason.clone());
        ui.vertical_centered(|ui| ui.label(RichText::new("Full workflow: pre-matte → hint → Key → export").color(theme::text_dim(ui)).size(11.5)));
        if go.clicked() {
            cmd = Some(DeliverCmd::RunWorkflow);
        }
        if let Some(c) = &v.confirm {
            let existing = c.existing.join(", ");
            egui::Frame::group(ui.style()).inner_margin(8.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.colored_label(theme::warn(ui), format!("This shot already has {existing}."));
                ui.label(RichText::new("Running the full workflow means pre-matte, hint and Key may run again, not only the video encoding. To encode the selected video outputs from the existing Key result, choose “Export existing Key result”.").size(12.0));
                ui.horizontal_wrapped(|ui| {
                    let a = ui.button("Cancel");
                    self.hint.deliver_rects.push(("confirm_cancel".into(), a.rect));
                    if a.clicked() {
                        cmd = Some(DeliverCmd::CancelWorkflow);
                    }
                    let b = ui.button("Run full workflow again");
                    self.hint.deliver_rects.push(("confirm_run".into(), b.rect));
                    if b.clicked() {
                        cmd = Some(DeliverCmd::ConfirmWorkflow);
                    }
                    let e = ui.add_enabled(c.can_export_existing, egui::Button::new("Export existing Key result instead"));
                    self.hint.deliver_rects.push(("confirm_export".into(), e.rect));
                    if e.clicked() {
                        cmd = Some(DeliverCmd::ExportExisting);
                    }
                });
            });
        }
        // While the confirmation is open its own export choice is the only one (no duplicate export buttons).
        if clip.manifest.is_some() && v.confirm.is_none() {
            let x = &v.export_existing;
            let d = crate::actions::Decision { enabled: x.enabled, label: x.label.clone(), reason: x.reason.clone() };
            let export = ui.vertical_centered(|ui| ui.add_enabled(d.enabled, egui::Button::new(d.label.as_str()).min_size(egui::vec2(220.0, 30.0)))).inner;
            self.hint.deliver_rects.push(("export_existing".into(), export.rect));
            let tip = "Encode the selected video outputs from the current Key result, without running Key again.";
            let clicked = if d.enabled {
                export.on_hover_text(if d.reason.is_empty() { tip.to_string() } else { format!("{tip}\n{}", d.reason) }).clicked()
            } else {
                export.on_disabled_hover_text(d.reason.as_str()).clicked()
            };
            if clicked {
                cmd = Some(DeliverCmd::ExportExisting);
            }
            reason_line(ui, &[&d]);
        }
        if let Some(c) = cmd {
            let _ = self.deliver_run(&ctx, c);
        }
        if !v.workflow.reason.is_empty() {
            ui.label(RichText::new(&v.workflow.reason).color(theme::text_dim(ui)).size(12.0));
        }
        // Why the Deliver cue is on: each reason separately, never blended into one claim.
        let rc = &self.hint.rc;
        if rc.deliver {
            ui.label(RichText::new(format!("Needs encoding: {}.", rc.deliver_why.join("; "))).color(theme::text_dim(ui)).size(12.0));
        }
        if !rc.deliver_unverified.is_empty() {
            ui.label(RichText::new(format!("Not verified as encoded (no export seen in this session): {}.", rc.deliver_unverified.join(", "))).color(theme::text_dim(ui)).size(12.0));
        }
        if rc.key {
            ui.label(RichText::new("The Key result is older than the current pre-matte/hint/Key settings (or missing): outputs encoded from it would be outdated.").color(theme::text_dim(ui)).size(12.0));
        }
        if let Some(b) = &v.block {
            ui.colored_label(theme::warn(ui), b);
        }
    }
}

// ----------------------------------------------------------------------- Deliver: typed snapshot + commands

/// The four Key outputs, in the order Deliver shows them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DeliverOutput {
    Fg,
    Matte,
    Comp,
    Processed,
}

impl DeliverOutput {
    pub(crate) const ALL: [DeliverOutput; 4] = [Self::Fg, Self::Matte, Self::Comp, Self::Processed];
    /// The key used by `PipelineExport.outputs` and the shot's delivery markers.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Fg => "fg",
            Self::Matte => "matte",
            Self::Comp => "comp",
            Self::Processed => "processed",
        }
    }
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::Fg => "FG (foreground)",
            Self::Matte => "Matte",
            Self::Comp => "Comp",
            Self::Processed => "Processed (RGBA)",
        }
    }
    fn enabled_mut(self, o: &mut crate::workflow::OutputCfg) -> &mut bool {
        match self {
            Self::Fg => &mut o.fg_enabled,
            Self::Matte => &mut o.matte_enabled,
            Self::Comp => &mut o.comp_enabled,
            Self::Processed => &mut o.processed_enabled,
        }
    }
    fn format_mut(self, o: &mut crate::workflow::OutputCfg) -> &mut String {
        match self {
            Self::Fg => &mut o.fg_format,
            Self::Matte => &mut o.matte_format,
            Self::Comp => &mut o.comp_format,
            Self::Processed => &mut o.processed_format,
        }
    }
}

/// A video delivery with the exact stored numbers. `custom`: the container/codec pair is not one the export knows.
#[derive(Clone)]
pub(crate) struct DeliverVideo {
    pub container: String,
    pub container_label: &'static str,
    pub codec: String,
    pub codec_label: String,
    pub quality: u32,
    pub custom: bool,
    /// "CRF" / "qscale" (empty when custom).
    pub quality_name: &'static str,
    pub min: u32,
    pub max: u32,
    /// "CRF 0-51: lower = better quality, bigger file".
    pub quality_note: String,
    pub alpha_warning: Option<&'static str>,
    pub containers: Vec<Choice<String>>,
    /// The codecs of the current container.
    pub codecs: Vec<Choice<String>>,
}

#[derive(Clone)]
pub(crate) struct DeliverOutputView {
    pub output: DeliverOutput,
    pub title: &'static str,
    pub enabled: bool,
    /// Sequence format: EXR (true) or PNG.
    pub exr: bool,
    /// `Some` = delivered as a video file, `None` = image sequence.
    pub video: Option<DeliverVideo>,
    pub summary: String,
    pub enabled_overridden: bool,
    pub format_overridden: bool,
    pub delivery_overridden: bool,
}

/// The one EXR compression every EXR sequence shares.
#[derive(Clone)]
pub(crate) struct DeliverCompression {
    pub value: String,
    pub choices: Vec<&'static str>,
    pub shared_note: &'static str,
    pub lossy_note: Option<&'static str>,
    pub overridden: bool,
}

/// The one frame rate every video shares: `stored` is exact (None = each shot's own `source`).
#[derive(Clone)]
pub(crate) struct DeliverFps {
    pub stored: Option<f64>,
    pub source: f64,
    pub overridden: bool,
}

#[derive(Clone)]
pub(crate) struct DeliverDestination {
    /// The shot's own video folder (None = the project default).
    pub dir: Option<PathBuf>,
    pub resolved: String,
    pub overridden: bool,
}

/// Project-wide image output templates (`App.loc`); a project setting, not a per-shot one.
#[derive(Clone)]
pub(crate) struct DeliverLocation {
    pub root: String,
    pub folder: String,
    pub file: String,
    /// The first output's resolved path, or why it cannot be resolved.
    pub preview: Result<String, String>,
    pub saved: bool,
    pub note: Option<String>,
}

#[derive(Clone)]
pub(crate) struct DeliverPreset {
    pub name: String,
    pub label: String,
    /// Parameters/outputs only (partial apply).
    pub legacy: bool,
}

/// "Run full workflow" while results exist: waits on frozen settings for an explicit choice.
#[derive(Clone)]
pub(crate) struct DeliverConfirm {
    pub existing: Vec<String>,
    pub can_export_existing: bool,
}

/// Read-only snapshot of the Deliver stage of the open shot (cached state only; nothing is probed or written).
#[derive(Clone)]
pub(crate) struct DeliverView {
    pub outputs: Vec<DeliverOutputView>,
    pub compression: DeliverCompression,
    pub fps: DeliverFps,
    pub has_video: bool,
    /// In/Out frames (false) or the Done frames of a partial result (true).
    pub done_frames: bool,
    pub done_frames_overridden: bool,
    pub destination: DeliverDestination,
    pub location: DeliverLocation,
    /// Outputs badge: the written outputs are older than the settings. Reasons are listed separately below.
    pub out_of_date: bool,
    pub out_of_date_why: Vec<String>,
    /// Outputs with no export seen in this session: unverified, not out of date.
    pub unverified: Vec<String>,
    pub key_older: bool,
    /// Export primary: existing result for video, the Key render for sequence-only.
    pub export: HintAct,
    pub export_existing: HintAct,
    pub workflow: HintAct,
    pub confirm: Option<DeliverConfirm>,
    pub presets: Vec<DeliverPreset>,
    /// A preset waiting for delete confirmation.
    pub preset_delete: Option<String>,
    pub overridden: Vec<String>,
    pub reset_label: Option<String>,
    pub block: Option<String>,
    pub note: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum DeliverCmd {
    SetEnabled { output: DeliverOutput, on: bool },
    SetSeqFormat { output: DeliverOutput, exr: bool },
    SetKind { output: DeliverOutput, video: bool },
    SetContainer { output: DeliverOutput, container: String },
    SetCodec { output: DeliverOutput, codec: String },
    SetQuality { output: DeliverOutput, quality: f32 },
    SetCompression(String),
    SetFps(f64),
    /// Back to each shot's own frame rate.
    UseSourceFps,
    SetRange { done_frames: bool },
    ChooseExportDir,
    ResetExportDir,
    EditLocation { root: String, folder: String, file: String },
    SaveLocation,
    ResetLocation,
    ResetDelivery,
    Export,
    ExportExisting,
    RunWorkflow,
    ConfirmWorkflow,
    CancelWorkflow,
    SavePreset(String),
    ApplyPreset(String),
    RequestDeletePreset(String),
    CancelDeletePreset,
    ConfirmDeletePreset(String),
}

struct DeliverGates {
    ffmpeg: bool,
    range: Result<(usize, usize), String>,
    pl: Result<crate::presets::Pipeline, String>,
    /// Why the full workflow cannot start now (empty = it can).
    why: String,
}

fn video_view(v: &VideoDelivery) -> DeliverVideo {
    let cur = VIDEO_CODECS.iter().find(|c| c.container == v.container && c.id == v.codec);
    let name = cur.map_or("", |c| if matches!(c.quality, QualityKind::Crf) { "CRF" } else { "qscale" });
    let (min, max) = cur.map_or((0, 0), |c| (c.min, c.max));
    DeliverVideo {
        container: v.container.clone(),
        container_label: match v.container.as_str() {
            "mp4" => "MP4",
            "mov_prores4444" => "MOV",
            "webm_alpha" => "WebM",
            _ => "Custom",
        },
        codec: v.codec.clone(),
        codec_label: cur.map_or_else(|| "Custom".to_string(), codec_text),
        quality: v.quality,
        custom: cur.is_none(),
        quality_name: name,
        min,
        max,
        quality_note: if cur.is_some() { format!("{name} {min}-{max}: lower = better quality, bigger file") } else { String::new() },
        alpha_warning: cur.is_some_and(|c| !c.alpha).then_some("This codec has no alpha: transparency is not retained. Use MOV ProRes 4444 or WebM VP9 to keep it."),
        containers: [("mp4", "MP4"), ("mov_prores4444", "MOV"), ("webm_alpha", "WebM")]
            .iter()
            .map(|(id, l)| Choice { value: id.to_string(), label: l.to_string(), ready: true })
            .collect(),
        codecs: VIDEO_CODECS
            .iter()
            .filter(|c| c.container == v.container)
            .map(|c| Choice { value: c.id.to_string(), label: codec_text(c), ready: true })
            .collect(),
    }
}

impl App {
    /// Output settings of the shot without creating its draft.
    fn deliver_out_cfg(&self, clip: &Clip) -> Option<crate::workflow::OutputCfg> {
        let pr = self.project.as_ref()?;
        Some(self.ai.draft_of(&clip.folder).map_or_else(|| crate::workflow::seed(clip, pr, self.settings.saved()).1, |d| d.out.clone()))
    }

    /// The shot's export settings following the enabled outputs (what `sync_deliveries` stores), without writing.
    fn export_now(&self, o: &crate::workflow::OutputCfg) -> PipelineExport {
        let mut e = self.current_export();
        e.sync_outputs(o);
        e
    }

    /// The full pipeline from the live state, read-only (`capture_pipeline` is this plus the syncing).
    fn pipeline_now(&self, clip: &Clip) -> Result<crate::presets::Pipeline, String> {
        let pr = self.project.as_ref().ok_or("no project open")?;
        let (params, outputs) = self
            .ai
            .draft_of(&clip.folder)
            .map(|d| (d.params.clone(), d.out.clone()))
            .unwrap_or_else(|| {
                let (p, o, _) = crate::workflow::seed(clip, pr, self.settings.saved());
                (p, o)
            });
        let p = crate::presets::Pipeline {
            hint: self.hint_cfg_for(clip),
            params,
            location: pr.output_location.clone().filter(|l| !l.is_null()),
            export: self.export_now(&outputs),
            outputs,
        };
        p.validate()?;
        Ok(p)
    }

    fn deliver_gates(&self, clip: &Clip) -> DeliverGates {
        let ffmpeg = self.ai.models_unknown() || self.ai.local_caps().is_some_and(|c| c.tasks.get("export_video").copied().unwrap_or(false));
        let busy = self.ai.task_busy(&clip.folder);
        let range = self.ai_open_range();
        let pl = self.pipeline_now(clip);
        let why = match (&pl, &range) {
            (Err(e), _) | (_, Err(e)) => e.clone(),
            _ if !ffmpeg => reason(&self.ai.local_caps(), "FFmpeg"),
            _ if busy => "A job is running for this clip".into(),
            _ => String::new(),
        };
        DeliverGates { ffmpeg, range, pl, why }
    }

    /// The "Export existing Key result" decision; `None` = this shot has no Key result.
    fn export_existing_decision(&self, clip: &Clip, g: &DeliverGates) -> Option<crate::actions::Decision> {
        clip.manifest.as_ref()?;
        let blocked = if !g.ffmpeg { Some(reason(&self.ai.local_caps(), "FFmpeg")) } else { g.range.as_ref().err().cloned() };
        Some(decide(self.ai.scheduler(), &clip.root, Action::Export, "Export existing Key result", blocked))
    }

    /// A confirmation only ever applies to the shot, results, range and settings it was opened for.
    fn confirm_valid(&self, clip: &Clip, g: &DeliverGates) -> bool {
        self.hint.render_confirm.as_ref().is_some_and(|c| {
            c.folder == clip.folder && c.sig == result_sig(clip, &g.range) && g.pl.as_ref().is_ok_and(|p| *p == c.pipeline) && g.why.is_empty()
        })
    }

    /// Every frame while a shot is open: read-only (the gates read the live state through `export_now`, so nothing is
    /// synced into `hint.deliveries`, which would make a freshly opened shot unsaved); only closes a stale confirmation.
    pub(crate) fn deliver_tick(&mut self) {
        let Some(clip) = self.clip().cloned() else { return };
        if self.hint.folder.as_deref() == Some(clip.folder.as_str()) {
            let g = self.deliver_gates(&clip);
            if self.hint.render_confirm.is_some() && !self.confirm_valid(&clip, &g) {
                self.hint.render_confirm = None;
                self.hint_note("The confirmation was closed because the shot, its results or the Deliver settings changed. Press the button again.");
            }
        }
    }

    /// Once per frame before `deliver_view` (loads the presets file the first time; nothing else is read).
    pub(crate) fn deliver_prepare(&mut self) {
        self.ensure_presets();
    }

    pub(crate) fn deliver_view(&self) -> Option<DeliverView> {
        let clip = self.clip()?;
        let h = &self.hint;
        if h.folder.as_deref() != Some(clip.folder.as_str()) {
            return None;
        }
        let o = self.deliver_out_cfg(clip)?;
        let pr = self.project.as_ref()?;
        let exp = self.export_now(&o);
        let ex = self.ai.draft_of(&clip.folder).map(|d| d.explicit.clone()).unwrap_or_default();
        let outputs = DeliverOutput::ALL
            .map(|out| {
                let k = out.key();
                let enabled = exp.outputs.contains_key(k);
                let exr = match out {
                    DeliverOutput::Fg => &o.fg_format,
                    DeliverOutput::Matte => &o.matte_format,
                    DeliverOutput::Comp => &o.comp_format,
                    DeliverOutput::Processed => &o.processed_format,
                } == "exr";
                let video = match exp.outputs.get(k) {
                    Some(Delivery::Video(v)) => Some(video_view(v)),
                    _ => None,
                };
                let summary = match (&video, enabled) {
                    (_, false) => "Not written".to_string(),
                    (None, _) => format!("{} sequence", if exr { "EXR" } else { "PNG" }),
                    (Some(v), _) if v.custom => format!("Custom video ({} / {}, {})", v.container, v.codec, v.quality),
                    (Some(v), _) => format!("{} · {} · {} {}", v.container_label, v.codec_label, v.quality_name, v.quality),
                };
                DeliverOutputView {
                    output: out,
                    title: out.title(),
                    enabled,
                    exr,
                    video,
                    summary,
                    enabled_overridden: ex.contains_key(&format!("{k}_enabled")),
                    format_overridden: ex.contains_key(&format!("{k}_format")),
                    delivery_overridden: h.delivery_ov.contains_key(k),
                }
            })
            .to_vec();
        let g = self.deliver_gates(clip);
        let has_video = exp.has_video();
        let ex_dec = self.export_existing_decision(clip, &g);
        let export_existing = if !has_video {
            HintAct { enabled: false, label: "Export existing Key result".into(), reason: "No output is delivered as a video file.".into() }
        } else {
            ex_dec.as_ref().map_or_else(
                || HintAct { enabled: false, label: "Export existing Key result".into(), reason: "This shot has no Key result to export yet.".into() },
                HintAct::from,
            )
        };
        let export = if has_video {
            export_existing.clone()
        } else {
            let kv = self.ai.key_view(clip, pr, self.settings.saved());
            let reason = match (&kv.blocker, kv.primary_intent.is_none()) {
                (Some(b), _) => b.message.clone(),
                (None, true) => "A result already exists: choose what to do with it in Key / Render.".into(),
                _ => kv.status_line.clone().unwrap_or_default(),
            };
            HintAct { enabled: kv.primary_enabled && kv.primary_intent.is_some(), label: kv.primary_label.clone(), reason }
        };
        let confirming = self.confirm_valid(clip, &g);
        let location = {
            let preview = self.location_resolved(clip, &o);
            DeliverLocation {
                root: self.loc.root.display().to_string(),
                folder: self.loc.folder_template.clone(),
                file: self.loc.file_template.clone(),
                preview,
                saved: pr.output_location.as_ref().is_some_and(|v| !v.is_null()),
                note: self.loc_note.clone(),
            }
        };
        let rc = &h.rc;
        Some(DeliverView {
            outputs,
            compression: DeliverCompression {
                lossy_note: (o.exr_compression == "dwab").then_some("DWAB is lossy: HDR values are not bit-exact."),
                value: o.exr_compression.clone(),
                choices: crate::workflow::EXR_COMPRESSIONS.to_vec(),
                shared_note: "Applies to every EXR sequence (shared).",
                overridden: ex.contains_key("exr_compression"),
            },
            fps: DeliverFps { stored: exp.fps, source: clip.fps, overridden: h.delivery_ov.contains_key("fps") },
            has_video,
            done_frames: exp.allow_partial,
            done_frames_overridden: h.delivery_ov.contains_key("partial"),
            destination: DeliverDestination {
                resolved: crate::pipeline::export_dir(clip, &pr.root, exp.root.as_deref()).display().to_string(),
                dir: exp.root.clone(),
                overridden: h.delivery_ov.contains_key("root"),
            },
            location,
            out_of_date: self.stage_pending(Node::Out(View::Fg)),
            out_of_date_why: rc.deliver_why.clone(),
            unverified: rc.deliver_unverified.clone(),
            key_older: rc.key,
            export,
            export_existing,
            workflow: HintAct { enabled: g.why.is_empty() && !confirming, label: "Run full workflow".into(), reason: g.why.clone() },
            confirm: confirming
                .then(|| h.render_confirm.as_ref())
                .flatten()
                .map(|c| DeliverConfirm { existing: c.existing.clone(), can_export_existing: ex_dec.as_ref().is_some_and(|d| d.enabled) }),
            presets: h
                .presets
                .iter()
                .filter(|p| p.kind == "workflow")
                .map(|p| DeliverPreset { name: p.name.clone(), label: crate::presets::label(p), legacy: crate::presets::class_of(p) == crate::presets::PresetClass::LegacyWorkflow })
                .collect(),
            preset_delete: h.preset_del.clone().filter(|n| h.presets.iter().any(|p| p.kind == "workflow" && &p.name == n)),
            overridden: h.delivery_ov.keys().cloned().collect(),
            reset_label: (!h.delivery_ov.is_empty()).then(|| Self::reset_label("these delivery fields", &h.delivery_ov)),
            block: self.ai.pipeline_block_reason(),
            note: h.note.clone(),
        })
    }

    /// First output's resolved image path under the templates in `self.loc` (what the old editor showed).
    fn location_resolved(&self, clip: &Clip, o: &crate::workflow::OutputCfg) -> Result<String, String> {
        let (w, h) = clip.sample_size.unwrap_or((0, 0));
        let sv = |k: &str| clip.output_values.as_ref().and_then(|v| v.get(k)).and_then(Value::as_str).map(str::to_owned);
        let vals = ClipOutputValues {
            project: self.project.as_ref().map(|p| p.display_name.clone()).unwrap_or_default(),
            name: clip.name.clone(),
            resolution: sv("resolution").unwrap_or_else(|| resolution_label(w, h)),
            date: sv("date").unwrap_or_else(crate::workflow::today),
            version: clip.output_values.as_ref().and_then(|v| v.get("version")).and_then(Value::as_u64).map_or(1, |v| v as u32),
        };
        let kinds = o.kinds();
        let frame = self.viewer.as_ref().map_or(0, |v| v.index.frame_number(v.playhead));
        match kinds.first().map(|k| resolve_path(&self.loc, &vals, k.0, kinds.len(), frame, k.1)) {
            Some(Ok(p)) => Ok(display(&p)),
            Some(Err(e)) => Err(format!("{e:?}")),
            None => Err("No output enabled".into()),
        }
    }

    /// Give each field the user really changed the shot's own marker (value before the edit = its Reset base).
    fn mark_delivery_edits(&mut self, before: &PipelineExport) {
        let keys = shot_domains::export_diff(before, &self.current_export());
        if keys.is_empty() {
            return;
        }
        let preset = self.hint.delivery_src.clone();
        for k in keys {
            self.hint.delivery_ov.entry(k.clone()).or_insert_with(|| Field { base: shot_domains::export_get(before, &k), preset: preset.clone() });
        }
    }

    /// Edit the Key output draft (enabled/format/compression) with the Key panel's snapshot + `mark_edits` rule.
    fn deliver_draft_edit(&mut self, clip: &Clip, f: impl FnOnce(&mut crate::workflow::OutputCfg) -> Result<(), String>) -> Result<(), String> {
        let pr = self.project.as_ref().ok_or("no project open")?;
        let dr = self.ai.draft(clip, pr, self.settings.saved());
        let snap = dr.snapshot();
        let mut out = dr.out.clone();
        f(&mut out)?;
        out.validate()?;
        dr.out = out;
        dr.mark_edits(&snap);
        Ok(())
    }

    /// `deliver_cmd` without the immediate delivery write: the old panel drags sliders through this and lets
    /// `hint_pending_refresh` write once the pointer settles.
    pub(crate) fn deliver_run(&mut self, ctx: &egui::Context, c: DeliverCmd) -> Result<(), String> {
        let Some(clip) = self.open_clip_snapshot() else {
            return Err("Open a shot first.".into());
        };
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            return Err("This shot's delivery settings are still loading.".into());
        }
        self.hint.cmd_err = None;
        self.sync_deliveries(&clip);
        let before = self.current_export();
        // Only edits of the shot's own delivery fields get override markers (never a preset Apply / Reset / sync).
        let marks = matches!(c, DeliverCmd::SetKind { .. } | DeliverCmd::SetContainer { .. } | DeliverCmd::SetCodec { .. } | DeliverCmd::SetQuality { .. } | DeliverCmd::SetFps(_) | DeliverCmd::UseSourceFps | DeliverCmd::SetRange { .. } | DeliverCmd::ResetExportDir);
        let video_mut = |app: &mut App, o: DeliverOutput| -> Result<VideoDelivery, String> {
            match app.hint.deliveries.get(o.key()) {
                Some(Delivery::Video(v)) => Ok(v.clone()),
                Some(Delivery::Sequence) => Err(format!("{} is delivered as an image sequence: choose Video file first.", o.title())),
                None => Err(format!("{} is not enabled.", o.title())),
            }
        };
        match c {
            DeliverCmd::SetEnabled { output, on } => {
                self.deliver_draft_edit(&clip, |o| {
                    *output.enabled_mut(o) = on;
                    Ok(())
                })?;
                self.sync_deliveries(&clip);
            }
            DeliverCmd::SetSeqFormat { output, exr } => self.deliver_draft_edit(&clip, |o| {
                *output.format_mut(o) = if exr { "exr" } else { "png" }.into();
                Ok(())
            })?,
            DeliverCmd::SetKind { output, video } => {
                let Some(d) = self.hint.deliveries.get_mut(output.key()) else {
                    return Err(format!("{} is not enabled.", output.title()));
                };
                match (video, matches!(d, Delivery::Video(_))) {
                    (true, false) => *d = Delivery::Video(VideoDelivery::default_for("mp4")),
                    (false, true) => *d = Delivery::Sequence,
                    _ => {}
                }
            }
            DeliverCmd::SetContainer { output, container } => {
                if !VIDEO_CODECS.iter().any(|c| c.container == container) {
                    return Err(format!("Unknown video file type {container}"));
                }
                let v = video_mut(self, output)?;
                if v.container != container || crate::presets::codec(&v.container, &v.codec).is_none() {
                    self.hint.deliveries.insert(output.key().into(), Delivery::Video(VideoDelivery::default_for(&container)));
                }
            }
            DeliverCmd::SetCodec { output, codec } => {
                let v = video_mut(self, output)?;
                let Some(c) = crate::presets::codec(&v.container, &codec) else {
                    return Err(format!("{codec} is not a codec of this file type."));
                };
                if c.id != v.codec {
                    self.hint.deliveries.insert(output.key().into(), Delivery::Video(VideoDelivery { container: v.container, codec: c.id.into(), quality: c.default }));
                }
            }
            DeliverCmd::SetQuality { output, quality } => {
                let mut v = video_mut(self, output)?;
                let Some(c) = crate::presets::codec(&v.container, &v.codec) else {
                    return Err("Choose a file type and codec first.".into());
                };
                if !quality.is_finite() {
                    return Err("Quality must be a number.".into());
                }
                v.quality = (quality.round().max(0.0) as u32).clamp(c.min, c.max);
                self.hint.deliveries.insert(output.key().into(), Delivery::Video(v));
            }
            DeliverCmd::SetCompression(c) => {
                let c = crate::workflow::EXR_COMPRESSIONS.iter().find(|x| **x == c).ok_or_else(|| format!("Unknown EXR compression {c}"))?;
                self.deliver_draft_edit(&clip, |o| {
                    o.exr_compression = c.to_string();
                    Ok(())
                })?;
            }
            DeliverCmd::SetFps(f) => {
                if !f.is_finite() || !(0.001..=1000.0).contains(&f) {
                    return Err("Frame rate must be between 0.001 and 1000 fps.".into());
                }
                self.hint.export_fps = Some(f);
            }
            DeliverCmd::UseSourceFps => self.hint.export_fps = None,
            DeliverCmd::SetRange { done_frames } => self.hint.export_partial = done_frames,
            DeliverCmd::ChooseExportDir => {
                if !self.request_pick(ctx, PickKind::ExportFolder) {
                    return Err("Another dialog is open.".into());
                }
            }
            DeliverCmd::ResetExportDir => self.hint.export_dir = None,
            DeliverCmd::EditLocation { root, folder, file } => {
                self.loc.root = root.into();
                (self.loc.folder_template, self.loc.file_template) = (folder, file);
            }
            DeliverCmd::SaveLocation => {
                let root = self.project.as_ref().map(|p| p.root.clone()).ok_or("no project open")?;
                match crate::workflow::save_location(&root, &self.loc) {
                    Ok(v) => {
                        if let Some(p) = &mut self.project {
                            p.output_location = Some(v);
                        }
                        self.loc_note = Some("Saved to project.json".into());
                    }
                    Err(e) => {
                        self.loc_note = Some(format!("Not saved: {e}"));
                        return Err(e);
                    }
                }
            }
            DeliverCmd::ResetLocation => {
                let root = self.project.as_ref().map(|p| p.root.clone()).ok_or("no project open")?;
                match crate::workflow::clear_location(&root) {
                    Ok(()) => {
                        if let Some(p) = &mut self.project {
                            p.output_location = None;
                        }
                        if let Some(l) = crate::workflow::default_location(&clip) {
                            self.loc = l;
                        }
                        self.loc_note = Some("Default location restored".into());
                    }
                    Err(e) => {
                        self.loc_note = Some(format!("Not saved: {e}"));
                        return Err(e);
                    }
                }
            }
            DeliverCmd::ResetDelivery => {
                self.reset_delivery(&clip);
                return self.hint.cmd_err.take().map_or(Ok(()), Err);
            }
            DeliverCmd::Export if self.deliver_export_is_existing(&clip) => return self.deliver_export_existing(&clip),
            DeliverCmd::Export => {
                let (Some(pr), Some(_)) = (self.project.as_ref(), self.open) else { return Err("Open a shot first.".into()) };
                let kv = self.ai.key_view(&clip, pr, self.settings.saved());
                let Some(i) = kv.primary_intent.clone() else {
                    return Err("A result already exists: choose what to do with it in Key / Render.".into());
                };
                if !kv.primary_enabled {
                    return Err(kv.blocker.map(|b| b.message).or(kv.status_line).unwrap_or_else(|| "Key cannot start now.".into()));
                }
                return self.ai.key_cmd(&clip, pr, self.settings.saved(), ctx, crate::ui::ai::KeyCmd::Submit(i));
            }
            DeliverCmd::ExportExisting => return self.deliver_export_existing(&clip),
            DeliverCmd::RunWorkflow => {
                let g = self.deliver_gates(&clip);
                if !g.why.is_empty() {
                    return Err(g.why);
                }
                if self.hint.render_confirm.is_some() {
                    return Err("Answer the open confirmation first.".into());
                }
                let (Ok(pl), Some(i), Some(p)) = (&g.pl, self.open, self.project.as_ref()) else { return Err("Open a shot first.".into()) };
                let existing = existing_stage_results(&clip);
                if existing.is_empty() {
                    self.ai.queue_pipeline(ctx, p, self.settings.saved(), i, pl);
                } else {
                    self.hint.render_confirm = Some(RenderConfirm { folder: clip.folder.clone(), sig: result_sig(&clip, &g.range), pipeline: pl.clone(), existing });
                }
            }
            DeliverCmd::ConfirmWorkflow => {
                let g = self.deliver_gates(&clip);
                if self.hint.render_confirm.is_none() {
                    return Err("There is nothing to confirm.".into());
                }
                if !self.confirm_valid(&clip, &g) {
                    self.hint.render_confirm = None;
                    let m = "The confirmation was closed because the shot, its results or the Deliver settings changed. Press the button again.";
                    self.hint_note(m);
                    return Err(m.into());
                }
                if let (Some(c), Some(i), Some(p)) = (self.hint.render_confirm.take(), self.open, self.project.as_ref()) {
                    self.ai.queue_pipeline(ctx, p, self.settings.saved(), i, &c.pipeline);
                }
            }
            DeliverCmd::CancelWorkflow => self.hint.render_confirm = None,
            DeliverCmd::SavePreset(name) => {
                self.ensure_presets();
                let n = name.trim().to_string();
                if n.is_empty() {
                    return Err("Name the preset first.".into());
                }
                let data = match self.capture_pipeline(&clip) {
                    Ok(p) => p.to_value(),
                    Err(e) => {
                        let m = format!("Preset not saved: {e}");
                        self.hint_note(m.clone());
                        return Err(m);
                    }
                };
                self.hint.presets.retain(|p| !(p.kind == "workflow" && p.name == n));
                self.hint.presets.push(crate::presets::Preset { name: n.clone(), kind: "workflow".into(), data });
                (self.hint.preset_name_for, self.hint.preset_sel) = (n.clone(), n.clone());
                if !self.store_presets() {
                    return Err(self.hint.cmd_err.clone().unwrap_or_else(|| "Presets not saved".into()));
                }
                self.hint.note = Some(format!("Saved “{n}”."));
            }
            DeliverCmd::ApplyPreset(name) => {
                self.ensure_presets();
                let pr = self.hint.presets.iter().find(|p| p.kind == "workflow" && p.name == name).cloned().ok_or_else(|| format!("No preset named “{name}”."))?;
                self.hint.preset_sel = name.clone();
                let (note, ok) = self.apply_workflow_preset(ctx, &clip, &pr);
                self.hint.note = Some(note.clone());
                if !ok {
                    self.hint.cmd_err = Some(note.clone());
                    return Err(note);
                }
            }
            DeliverCmd::RequestDeletePreset(name) => {
                self.ensure_presets();
                if !self.hint.presets.iter().any(|p| p.kind == "workflow" && p.name == name) {
                    return Err(format!("No preset named “{name}”."));
                }
                self.hint.preset_del = Some(name);
            }
            DeliverCmd::CancelDeletePreset => self.hint.preset_del = None,
            DeliverCmd::ConfirmDeletePreset(name) => {
                if self.hint.preset_del.as_deref() != Some(name.as_str()) {
                    return Err("Delete needs your confirmation first.".into());
                }
                self.hint.preset_del = None;
                self.hint.presets.retain(|p| !(p.kind == "workflow" && p.name == name));
                if !self.store_presets() {
                    return Err(self.hint.cmd_err.clone().unwrap_or_else(|| "Presets not saved".into()));
                }
            }
        }
        if marks {
            self.mark_delivery_edits(&before);
        }
        Ok(())
    }

    /// One Deliver command, through the same paths as the old Deliver panel. A change to the shot's own delivery
    /// fields is written at once (atomically with its markers); `Err` = refused or failed, nothing is half-applied.
    pub(crate) fn deliver_cmd(&mut self, ctx: &egui::Context, c: DeliverCmd) -> Result<(), String> {
        self.deliver_run(ctx, c)?;
        let Some(clip) = self.open_clip_snapshot() else { return Ok(()) };
        let json = delivery_json(&self.hint);
        if !self.hint.delivery_ov.is_empty() && !self.hint.deliveries.is_empty() && json != self.hint.saved_delivery {
            if let Err(e) = self.persist_delivery(&clip) {
                let m = format!("Delivery not saved: {e}. It stays here and is retried on your next edit or Save.");
                self.hint_note(m.clone());
                return Err(m);
            }
            self.refresh_after_save(&clip.folder);
        }
        Ok(())
    }

    /// Export's primary is the existing result whenever any output is a video.
    fn deliver_export_is_existing(&self, clip: &Clip) -> bool {
        self.deliver_out_cfg(clip).is_some_and(|o| self.export_now(&o).has_video())
    }

    fn deliver_export_existing(&mut self, clip: &Clip) -> Result<(), String> {
        let g = self.deliver_gates(clip);
        if !self.deliver_export_is_existing(clip) {
            return Err("No output is delivered as a video file.".into());
        }
        let d = self.export_existing_decision(clip, &g).ok_or("This shot has no Key result to export yet.")?;
        if !d.enabled {
            return Err(d.reason);
        }
        // Choosing it inside the full-workflow confirmation closes that question.
        self.hint.render_confirm = None;
        let (r, Some(p)) = (g.range?, self.project.as_ref()) else { return Err("Open a shot first.".into()) };
        let cfg = self.current_export();
        match self.ai.submit_existing_deliveries(clip, &p.root, r, &cfg) {
            Ok(ids) => {
                self.hint.note = Some(format!("Queued {} video output(s) from the existing Key result.", ids.len()));
                Ok(())
            }
            Err(e) => {
                let m = format!("Existing result not exported: {e}");
                self.hint_note(m.clone());
                Err(m)
            }
        }
    }

    /// Apply a saved workflow preset (full pipeline, or legacy parameters/outputs only). (note, applied)
    fn apply_workflow_preset(&mut self, ctx: &egui::Context, clip: &Clip, pr: &crate::presets::Preset) -> (String, bool) {
        let sel = pr.name.clone();
        if crate::presets::class_of(pr) == crate::presets::PresetClass::Pipeline {
            return match crate::presets::Pipeline::from_value(&pr.data) {
                Err(e) => (format!("Preset not applied: {e}"), false),
                Ok(pl) => match self.apply_pipeline(ctx, clip, &sel, &pl) {
                    Ok(()) => (format!("Applied “{sel}”: pre-matte, hint, Key, outputs and export settings. {} Range, colour, prompts and marks are untouched.", if pl.location.is_none() { "Project output location reset to default." } else { "Project output location updated." }), true),
                    Err(e) => (format!("Preset “{sel}” not fully applied: {e}"), false),
                },
            };
        }
        let Some(p) = self.project.as_ref() else { return ("Preset not applied: no project open".into(), false) };
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
        match res {
            Ok(()) => (format!("Applied legacy preset “{sel}”: parameters and outputs only (partial; pre-matte, hint, locations and export unchanged)."), true),
            Err(e) => (format!("Preset not applied: {e}"), false),
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

/// A processing button driven by the shared action decision: its label, enabled state and hover text come from `d`.
/// Returns true when clicked.
fn decided_button(app: &App, ui: &mut Ui, d: &crate::actions::Decision, stage: Node, primary: bool, tip: &str) -> bool {
    let r = app.action_button(ui, &d.label, stage, primary, d.enabled);
    if d.enabled {
        r.on_hover_text(if d.reason.is_empty() { tip.to_string() } else { format!("{tip}\n{}", d.reason) }).clicked()
    } else {
        r.on_disabled_hover_text(d.reason.as_str()).clicked()
    }
}

/// The short line beside the buttons: why one is disabled (warning colour) or why it will wait (dim).
fn reason_line(ui: &mut Ui, ds: &[&crate::actions::Decision]) {
    let mut seen: Vec<&str> = vec![];
    for d in ds.iter().filter(|d| !d.reason.is_empty()) {
        if seen.contains(&d.reason.as_str()) {
            continue;
        }
        seen.push(&d.reason);
        let c = if d.enabled || d.label == "Already queued" { theme::text_dim(ui) } else { theme::warn(ui) };
        ui.label(RichText::new(d.reason.as_str()).color(c).size(12.0));
    }
}

/// The model the selected Pre-matte producer needs when the app's one check found it missing ("GVM", "BiRefNet
/// Matting", "SAM Base+"). `None` = nothing missing, nothing read yet, or the GPU is the reason instead.
pub(super) fn missing_guide(caps: &Option<crate::models::Capabilities>, lazy: bool, cfg: &HintCfg) -> Option<String> {
    let c = caps.as_ref().filter(|c| !lazy && !c.task_unavailable.contains_key("gpu"))?;
    match cfg.guide {
        Producer::Gvm => (!c.tasks.get(TaskKind::Gvm.wire()).copied().unwrap_or(false)).then(|| "GVM".into()),
        Producer::Birefnet => (!c.birefnet_variants.contains(&cfg.birefnet_variant)).then(|| format!("BiRefNet {}", cfg.birefnet_variant)),
        Producer::Sam => (!c.sam_models.contains(&cfg.sam_model))
            .then(|| format!("SAM {}", SAM_MODELS.iter().find(|m| m.0 == cfg.sam_model).map_or(cfg.sam_model.as_str(), |m| m.1))),
        Producer::Chroma => None,
    }
}

/// Same for the Final hint method (VideoMaMa, MatAnyone2).
fn missing_final(caps: &Option<crate::models::Capabilities>, lazy: bool, fin: Final) -> Option<String> {
    let c = caps.as_ref().filter(|c| !lazy && !c.task_unavailable.contains_key("gpu"))?;
    let (kind, name) = match fin {
        Final::Videomama => (TaskKind::Videomama, "VideoMaMa"),
        Final::Matanyone2 => (TaskKind::Matanyone2, "MatAnyone2"),
        _ => return None,
    };
    (!c.tasks.get(kind.wire()).copied().unwrap_or(false)).then(|| name.to_string())
}

fn reason(caps: &Option<crate::models::Capabilities>, what: &str) -> String {
    let Some(c) = caps else {
        return "The worker is starting or unavailable: see Settings > Clients.".into();
    };
    // A missing model comes first: FFmpeg is named only when FFmpeg is what the caller asks about.
    let blockers: Vec<&str> = c.task_unavailable.iter().filter(|(k, _)| what == "FFmpeg" || k.as_str() != "ffmpeg").map(|(_, v)| v.as_str()).collect();
    if blockers.is_empty() {
        format!("{what} is missing — download it on the Models page")
    } else {
        blockers.join("; ")
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
        self.ai.draft(clip, pr, self.settings.saved());
        self.pipeline_now(clip)
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
    /// list). Source-bound values are never copied (see `presets`). Every button is a `DeliverCmd`.
    pub(super) fn preset_ui(&mut self, ui: &mut Ui, _clip: &Clip) {
        self.ensure_presets();
        let ctx = ui.ctx().clone();
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
        let mut cmd: Option<DeliverCmd> = None;
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
                cmd = Some(DeliverCmd::ApplyPreset(self.hint.preset_sel.clone()));
            }
            let del = ui.add_enabled(have, egui::Button::new("Delete"));
            self.hint.preset_rects.push(("delete", del.rect));
            if del.clicked() {
                cmd = Some(DeliverCmd::RequestDeletePreset(self.hint.preset_sel.clone()));
            }
        });
        // Deleting a preset needs a second, explicit click.
        if let Some(n) = self.hint.preset_del.clone().filter(|n| names.iter().any(|x| &x.0 == n)) {
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(theme::warn(ui), format!("Delete preset “{n}”? This cannot be undone."));
                let y = ui.button("Delete preset");
                self.hint.preset_rects.push(("delete_confirm", y.rect));
                if y.clicked() {
                    cmd = Some(DeliverCmd::ConfirmDeletePreset(n.clone()));
                }
                let k = ui.button("Keep");
                self.hint.preset_rects.push(("delete_cancel", k.rect));
                if k.clicked() {
                    cmd = Some(DeliverCmd::CancelDeletePreset);
                }
            });
        }
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
                cmd = Some(DeliverCmd::SavePreset(n));
            }
        });
        if let Some(c) = cmd {
            let _ = self.deliver_run(&ctx, c);
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(theme::text_dim(ui)).size(12.0));
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
    let resp = if enabled { resp.on_hover_cursor(egui::CursorIcon::PointingHand) } else { resp };
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


#[cfg(test)]
mod delete_masks_tests {
    use super::*;

    #[test]
    fn queued_or_external_work_refuses_deletion() {
        assert!(delete_refusal(true, false).is_some_and(|m| m.contains("queued")));
        assert!(delete_refusal(false, true).is_some());
        assert!(delete_refusal(false, false).is_none());
    }

    #[test]
    fn scopes_name_exact_frames_and_all_means_the_whole_asset() {
        let marks = vec!["s_2".to_string(), "s_5".to_string()];
        assert_eq!(scope_stems(DelScope::Current, Some("s_3"), &marks), Some(vec!["s_3".into()]));
        assert_eq!(scope_stems(DelScope::Marked, Some("s_3"), &marks), Some(marks.clone()));
        assert_eq!(scope_stems(DelScope::All, Some("s_3"), &marks), None);
        let v: serde_json::Value = serde_json::from_str(&delete_request("delete", "r", Path::new("/c"), true, &None, false)).unwrap();
        assert_eq!((v["scope"].as_str(), v["asset"].as_str(), v["movies"].as_bool()), (Some("all"), Some("prematte"), Some(false)));
    }

    #[test]
    fn a_late_preview_of_a_job_known_at_deletion_is_not_shown() {
        let mut dead = std::collections::HashSet::new();
        assert!(preview_is_live("job-1", &dead));
        dead.insert("job-1".to_string());
        assert!(!preview_is_live("job-1", &dead));
        assert!(preview_is_live("job-2", &dead));
    }
}

#[cfg(test)]
mod annotation_leave_tests {
    use super::*;

    fn frame(ctx: &egui::Context, app: &mut crate::ui::App) {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1440.0, 900.0))),
            ..Default::default()
        };
        let mut output = ctx.run_ui(raw, |ui| app.frame(ui));
        output.textures_delta.clear();
    }

    fn png(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut encoder = png::Encoder::new(file, 4, 4);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
    }

    #[test]
    fn marked_delete_dialog_resolves_marks_against_source_frames() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("hint-marked-delete-{}-{nonce}", std::process::id()));
        let root = base.join("project");
        let frames = root.join("clips/c1/Frames");
        std::fs::create_dir_all(&frames).unwrap();
        png(&frames.join("f_1001.png"));
        std::fs::write(root.join("clips/c1/clip.json"), "{}").unwrap();
        std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();

        let ctx = egui::Context::default();
        let mut app = crate::ui::App::with_ctx(&ctx, crate::ui::AppOptions {
            project: Some(root.clone()),
            open_clip: Some("c1".into()),
            tab: Some(crate::ui::Tab::Hint),
            ..Default::default()
        });
        for _ in 0..3 { frame(&ctx, &mut app); }
        app.hint_sync();
        let clip_index = app.project.as_ref().unwrap().clips.iter().position(|c| c.folder == "c1").unwrap();
        app.project.as_mut().unwrap().clips[clip_index].keyframes = vec![0];
        let clip = app.project.as_ref().unwrap().clips[clip_index].clone();
        assert_eq!(stems_of(&clip), ["f_1001"]);
        app.viewer.as_mut().unwrap().index.stems = vec!["generated-frame".into()];

        let marked = Some(vec!["f_1001".to_owned()]);
        let plan_key = format!("{}|{:?}|{}|{:?}", false, DelScope::Marked, false, marked);
        app.hint.dele = Some(Del {
            folder: clip.folder.clone(), prematte: false, scope: DelScope::Marked, movies: false,
            plan_key: plan_key.clone(), plan: Some(Ok(DelPlan { files: vec!["f_1001".into()], ..Default::default() })),
            rx: None, done: None,
        });
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(700.0, 500.0))), ..Default::default() };
        let mut output = ctx.run_ui(raw, |ui| app.hint_delete_ui(ui, &clip));
        output.textures_delta.clear();
        let dialog = app.hint.dele.as_ref().unwrap();
        assert_eq!(dialog.plan_key, plan_key);
        assert!(matches!(&dialog.plan, Some(Ok(plan)) if plan.files == ["f_1001"]));
        std::fs::remove_dir_all(base).unwrap();
    }

    #[test]
    fn failed_prompt_clear_survives_leaving_and_reopening_the_shot() {
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let base = std::env::temp_dir().join(format!("hint-ann-leave-{}-{nonce}", std::process::id()));
        let root = base.join("project");
        for name in ["c1", "c2"] {
            let frames = root.join("clips").join(name).join("Frames");
            std::fs::create_dir_all(&frames).unwrap();
            png(&frames.join("f_1.png"));
            let mut saved = Annotations::default();
            saved.frame_mut("f_1").points.push(crate::hints::Pt { x: 0.5, y: 0.5, label: 1 });
            let metadata = if name == "c1" {
                serde_json::json!({"ck_native":{"annotations":saved}}).to_string()
            } else {
                "{}".into()
            };
            std::fs::write(root.join("clips").join(name).join("clip.json"), metadata).unwrap();
        }
        std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();

        let ctx = egui::Context::default();
        let mut app = crate::ui::App::with_ctx(&ctx, crate::ui::AppOptions {
            project: Some(root.clone()),
            open_clip: Some("c1".into()),
            tab: Some(crate::ui::Tab::Hint),
            ..Default::default()
        });
        for _ in 0..3 { frame(&ctx, &mut app); }
        app.hint_sync();
        app.hint.cfg.guide = Producer::Sam;

        let c2 = app.project.as_ref().unwrap().clips.iter().position(|c| c.folder == "c2").unwrap();
        let metadata = root.join("clips/c1/clip.json");
        let blocked_metadata = root.join("clips/c1/blocked-meta");
        std::fs::create_dir(&blocked_metadata).unwrap();
        app.project.as_mut().unwrap().clips.iter_mut().find(|c| c.folder == "c1").unwrap().meta_path = blocked_metadata;
        assert!(app.hint_cmd(&ctx, HintCmd::ClearPrompts { all: true }).is_err());
        app.open_clip(&ctx, c2);
        for _ in 0..3 { frame(&ctx, &mut app); }

        app.project.as_mut().unwrap().clips.iter_mut().find(|c| c.folder == "c1").unwrap().meta_path = metadata.clone();
        let c1 = app.project.as_ref().unwrap().clips.iter().position(|c| c.folder == "c1").unwrap();
        app.open_clip(&ctx, c1);
        for _ in 0..3 { frame(&ctx, &mut app); }
        assert!(app.hint.ann.frames.is_empty(), "the failed clear must be restored instead of persisted prompts");
        app.hint_flush_project().unwrap();

        let raw: serde_json::Value = serde_json::from_slice(&std::fs::read(&metadata).unwrap()).unwrap();
        assert!(raw["ck_native"]["annotations"]["frames"].as_object().is_some_and(|o| o.is_empty()));
        std::fs::remove_dir_all(base).unwrap();
    }
}
