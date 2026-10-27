//! Full pipeline presets: schema strictness and the batch chain (pre-matte -> Key -> export) through the real
//! `App` + scheduler with a fake worker process. Temp projects only; run through tools/isolated-test.sh.
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{class_of, Delivery, Pipeline, PipelineExport, PresetClass, VideoDelivery};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::tasks::TaskKind;
use ck::ui::{App, AppOptions};
use ck::workflow::{OutputCfg, Params};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn write_png(p: &Path) {
    let f = fs::File::create(p).unwrap();
    let mut e = png::Encoder::new(f, 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&[7; 48]).unwrap();
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}

/// `clips` = (folder, frames, fps, in_out). Frames are `f_1101..`.
fn env(tag: &str, clips: &[(&str, u32, f64, Option<(usize, usize)>)], project_json: Value) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    for (name, n, fps, io) in clips {
        let c = project.join("clips").join(name);
        fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..*n {
            write_png(&c.join(format!("Frames/f_{}.png", 1101 + i)));
        }
        fs::write(c.join(".video_metadata.json"), json!({"fps": fps, "width": 4, "height": 4}).to_string()).unwrap();
        let mut cj = json!({});
        if let Some((a, b)) = io {
            cj["in_out_range"] = json!({"in_point": a, "out_point": b});
        }
        fs::write(c.join("clip.json"), cj.to_string()).unwrap();
    }
    fs::write(project.join("project.json"), project_json.to_string()).unwrap();
    let repo = common::make_repo(&base, "repo", json!({}));
    let settings = base.join("settings.json");
    fs::write(
        &settings,
        json!({"version": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": repo, "python": "/usr/bin/python3", "gpu": 0}]}).to_string(),
    )
    .unwrap();
    Env { base, project, settings }
}

fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
}
fn pump(ctx: &egui::Context, app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        run(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn click_on(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
}

fn start(e: &Env, folders: &[&str], p: &Pipeline) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() },
    );
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let picks: Vec<usize> = proj
        .clips
        .iter()
        .enumerate()
        .filter(|(_, c)| folders.contains(&c.folder.as_str()))
        .map(|(i, _)| i)
        .collect();
    app.ai_mut().begin_batch_pipeline(&ctx, &proj, &Settings::default(), &picks, p);
    pump(&ctx, &mut app, "review", |a| a.ai().batch_ready());
    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    (ctx, app)
}
fn confirm(ctx: &egui::Context, app: &mut App) {
    let r = app.ai().batch_confirm_rect.expect("Queue button drawn");
    click_on(ctx, app, r.center());
}

/// Only the matte is enabled, as PNG.
fn matte_only() -> OutputCfg {
    OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() }
}
fn pipeline(hint: HintCfg, mode: &str, export: PipelineExport) -> Pipeline {
    Pipeline { hint, params: Params { hint_mode: mode.into(), ..Params::default() }, outputs: matte_only(), location: None, export }
}
/// Matte output, sequence only.
fn seq() -> PipelineExport {
    PipelineExport::for_outputs(&matte_only())
}
fn video(container: &str, codec: &str, quality: u32) -> Delivery {
    Delivery::Video(VideoDelivery { container: container.into(), codec: codec.into(), quality })
}
fn export_on() -> PipelineExport {
    let mut e = seq();
    e.outputs.insert("matte".into(), video("mp4", "h264", 18));
    e
}
/// `HintCfg` has a private legacy field, so struct-update from its default is not possible outside the crate.
fn route(guide: Producer, finish: Final) -> HintCfg {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (guide, finish);
    h
}
fn birefnet_matanyone() -> HintCfg {
    route(Producer::Birefnet, Final::Matanyone2)
}

#[test]
fn schema_is_strict_and_legacy_presets_are_labelled_not_pretended() {
    let p = pipeline(birefnet_matanyone(), "full", export_on());
    let v = p.to_value();
    assert_eq!(Pipeline::from_value(&v).unwrap(), p);
    assert!(v["location"].is_null() && v["params"].get("input_is_linear").is_none());
    // An invalid enum must not silently fall back to the default (GVM) route.
    let mut bad = v.clone();
    bad["hint"]["guide"] = json!("nonsense");
    assert!(Pipeline::from_value(&bad).unwrap_err().contains("hint"));
    let mut no_loc = v.clone();
    no_loc.as_object_mut().unwrap().remove("location");
    assert!(Pipeline::from_value(&no_loc).is_err());
    let mut v1 = v.clone();
    v1["version"] = json!(1);
    assert!(Pipeline::from_value(&v1).is_err());
    // A missing serialized key would silently default (e.g. to the GVM route): refuse, naming it.
    for (part, key) in [("hint", "guide"), ("hint", "chroma"), ("params", "despill_strength"), ("outputs", "exr_compression"), ("export", "root")] {
        let mut m = v.clone();
        m[part].as_object_mut().unwrap().remove(key);
        let e = Pipeline::from_value(&m).unwrap_err();
        assert!(e.contains(&format!("{part}.{key}")), "{e}");
    }
    let mut nested = v.clone();
    nested["hint"]["chroma"].as_object_mut().unwrap().remove("strength");
    assert!(Pipeline::from_value(&nested).is_err());
    let mut extra = v.clone();
    extra["hint"]["future"] = json!(1);
    assert!(Pipeline::from_value(&extra).is_ok(), "unknown keys are fine");
    // Legacy workflow preset: readable by the merge, refused as a pipeline, and labelled.
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: json!({"params": {}, "outputs": {}}) };
    assert_eq!(class_of(&legacy), PresetClass::LegacyWorkflow);
    assert!(ck::presets::label(&legacy).contains("legacy"));
    assert!(Pipeline::from_value(&legacy.data).unwrap_err().contains("legacy"));
    assert_eq!(class_of(&ck::presets::pipeline_preset("n", &p)), PresetClass::Pipeline);
    // Per-output delivery rules: exactly the enabled outputs, valid codec and quality.
    let mut bad = p.clone();
    bad.export.outputs.insert("fg".into(), Delivery::Sequence);
    assert!(bad.validate().is_err(), "delivery for a disabled output");
    let mut bad = p.clone();
    bad.export.outputs.clear();
    assert!(bad.validate().is_err(), "enabled output without a delivery");
    for (c, k, q) in [("mp4", "prores422", 9), ("mp4", "h264", 52), ("mov_prores4444", "prores422", 0), ("webm_alpha", "vp9", 64), ("avi", "h264", 18)] {
        let mut bad = p.clone();
        bad.export.outputs.insert("matte".into(), video(c, k, q));
        assert!(bad.validate().is_err(), "{c} {k} {q}");
    }
    let mut ok = p.clone();
    ok.export.outputs.insert("matte".into(), video("mov_prores4444", "prores422", 5));
    assert!(ok.validate().is_ok(), "alpha containers are allowed on every product");
    // Strict v3: a stored preset missing a delivery or naming an unknown part is refused, not defaulted.
    let mut m = v.clone();
    m["export"]["outputs"] = json!({});
    assert!(Pipeline::from_value(&m).unwrap_err().contains("no delivery"));
    let mut m = v.clone();
    m["export"]["outputs"]["fg"] = json!({"kind": "sequence"});
    assert!(Pipeline::from_value(&m).is_err());
    let mut m = v.clone();
    m["export"]["outputs"]["matte"] = json!({"kind": "video", "container": "mp4", "codec": "h264"});
    assert!(Pipeline::from_value(&m).is_err(), "missing quality");
}

#[test]
fn legacy_v2_single_export_presets_still_read_and_unknown_parts_are_refused() {
    let mut full = pipeline(birefnet_matanyone(), "full", seq());
    full.outputs = OutputCfg::default(); // fg, matte, comp, processed all enabled
    full.export = PipelineExport::for_outputs(&full.outputs);
    let mut v = full.to_value();
    v["version"] = json!(2);
    v["export"] = json!({"enabled": true, "product": "processed", "container": "mov_prores4444", "fps": null, "allow_partial": false, "root": null});
    let p = Pipeline::from_value(&v).unwrap();
    assert_eq!(p.export.outputs["processed"], video("mov_prores4444", "prores4444", 9), "selected product only");
    assert!(["fg", "matte", "comp"].iter().all(|k| p.export.outputs[*k] == Delivery::Sequence));
    v["export"]["container"] = json!("avi");
    assert!(Pipeline::from_value(&v).unwrap_err().contains("unknown export container"));
    v["export"]["enabled"] = json!(false);
    assert!(Pipeline::from_value(&v).unwrap_err().contains("unknown export container"), "also when export is off");
    v["export"]["enabled"] = json!(true);
    v["export"]["container"] = json!("mp4");
    v["export"]["product"] = json!("bogus");
    assert!(Pipeline::from_value(&v).is_err());
    v["export"]["enabled"] = json!(false);
    assert!(Pipeline::from_value(&v).unwrap().export.outputs.values().all(|d| *d == Delivery::Sequence), "export off = sequences");
}

#[test]
fn every_scalar_setting_and_per_output_delivery_roundtrips() {
    let mut hint = route(Producer::Sam, Final::Videomama);
    hint.videomama_chunk = 8;
    hint.mask_cleanup.grow = -3;
    hint.mask_cleanup.fill_holes = true;
    hint.mask_cleanup.feather = 2.5;
    let outputs = OutputCfg { fg_format: "png".into(), exr_compression: "zip".into(), ..OutputCfg::default() };
    let mut export = PipelineExport::for_outputs(&outputs);
    export.fps = Some(23.976);
    export.allow_partial = true;
    export.root = Some("/abs/out".into());
    export.outputs.insert("matte".into(), video("mp4", "hevc", 20));
    export.outputs.insert("processed".into(), video("webm_alpha", "vp9", 30));
    let p = Pipeline {
        hint,
        params: Params { hint_mode: "keyframes".into(), hint_quality: 0.7, carry_hint: false, carry_hint_quality: 0.6, vram_profile: "low".into(), despill_strength: 0.25, temporal_window: "auto".into(), ..Params::default() },
        outputs,
        location: None,
        export,
    };
    assert_eq!(Pipeline::from_value(&p.to_value()).unwrap(), p);
    assert_eq!(p.export.videos().iter().map(|v| v.0).collect::<Vec<_>>(), ["matte", "processed"]);
    // A subset of enabled outputs is a valid v3 preset.
    let q = pipeline(HintCfg::default(), "none", export_on());
    assert_eq!(Pipeline::from_value(&q.to_value()).unwrap(), q);
}

#[test]
fn two_clips_chain_producer_refiner_key_export_with_their_own_range_fps_and_paths() {
    let custom = json!({"root": "/nonexistent-custom", "folder_template": "x_{version}", "file_template": "x_####"});
    let e = env(
        "chain",
        &[("a", 4, 24.0, Some((1, 3))), ("b", 6, 30.0, Some((2, 5)))],
        json!({"version": 2, "output_location": custom}),
    );
    // A file already at a's default export name: the export must pick the next free name.
    let exports = e.project.join("Exports/a");
    fs::create_dir_all(&exports).unwrap();
    fs::write(exports.join("a_matte_f_1102-f_1104.mp4"), b"keep").unwrap();
    let hint = birefnet_matanyone();
    let (ctx, mut app) = start(&e, &["a", "b"], &pipeline(hint.clone(), "full", export_on()));
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("a_matte_f_1102-f_1104_v2.mp4") && sum.contains("b_matte_f_1103-f_1106.mp4"), "{sum}");
    assert!(sum.contains("clips/a/Output/a_v001"), "None location = per-clip default, not the project's custom one: {sum}");
    assert!(app.ai().scheduler().task_snapshot().is_empty(), "review queues nothing");
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let sched = app.ai().scheduler();
    let tasks = sched.task_snapshot();
    let keys = sched.snapshot();
    assert_eq!((tasks.len(), keys.len()), (6, 2), "guide+refine+export per clip, one Key per clip");
    assert!(tasks.iter().filter(|t| t.job.kind == TaskKind::ExportVideo).all(|t| t.job.params["codec"] == "h264" && t.job.params["quality"] == 18));
    for (folder, (a, b), fps, name) in [("a", (1, 3), 24.0, "f_1102-f_1104_v2"), ("b", (2, 5), 30.0, "f_1103-f_1106")] {
        let mine: Vec<_> = tasks.iter().filter(|t| t.job.clip_root.ends_with(format!("clips/{folder}"))).collect();
        let kinds: Vec<TaskKind> = mine.iter().map(|t| t.job.kind).collect();
        assert_eq!(kinds, [TaskKind::Birefnet, TaskKind::Matanyone2, TaskKind::ExportVideo], "{folder}");
        let key = keys.iter().find(|k| k.job.clip_root.ends_with(format!("clips/{folder}"))).unwrap();
        // Ordered dependencies: guide <- refine <- Key <- export.
        assert!(mine[0].after.is_empty());
        assert_eq!(mine[1].after, [mine[0].job.job_id.clone()]);
        assert_eq!(key.after, [mine[1].job.job_id.clone()]);
        assert_eq!(mine[2].after, [key.job.job_id.clone()]);
        // Own range on every step; the guide covers MatAnyone's In frame only.
        let r = |t: &ck::scheduler::TaskEntry| {
            let r = t.job.range.as_ref().unwrap();
            (r.in_point, r.out_point)
        };
        assert_eq!((r(mine[0]), r(mine[1]), r(mine[2])), ((a, a), (a, b), (a, b)));
        assert_eq!((key.job.range.in_point, key.job.range.out_point), (a, b));
        assert!(mine[0].job.publish_dir.as_ref().unwrap().ends_with("VideoMamaMaskHint"));
        assert!(mine[1].job.publish_dir.as_ref().unwrap().ends_with("AlphaHint"));
        // Export reads the Key plan's frozen manifest folder and writes the clip's own file, at its own fps.
        let p = &mine[2].job.params;
        assert_eq!(p["manifest_dir"], key.job.manifest_dir.display().to_string());
        assert_eq!(p["fps"], fps);
        let dest = p["dest"].as_str().unwrap();
        assert!(dest.ends_with(&format!("Exports/{folder}/{folder}_matte_{name}.mp4")), "{dest}");
        assert_eq!(p["container"], "mp4");
        assert!(key.job.manifest_dir.starts_with(e.project.join(format!("clips/{folder}/Output/{folder}_v001"))), "{:?}", key.job.manifest_dir);
    }
    // The clip carries the preset's route, so the graph/Hint page reflect it after the job.
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    for c in &proj.clips {
        assert_eq!(HintCfg::load(c), hint, "{}", c.folder);
    }
    assert_eq!(fs::read(exports.join("a_matte_f_1102-f_1104.mp4")).unwrap(), b"keep");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn manual_sam_guides_are_never_generated_or_copied_across_a_batch() {
    let e = env("sam", &[("a", 4, 24.0, None), ("b", 4, 24.0, None)], json!({"version": 2}));
    // Only clip a has its own prepared guide (MatAnyone seeds from the In frame).
    let g = e.project.join("clips/a/VideoMamaMaskHint");
    fs::create_dir_all(&g).unwrap();
    write_png(&g.join("f_1101.png"));
    let sam = route(Producer::Sam, Final::Matanyone2);
    let (ctx, mut app) = start(&e, &["a", "b"], &pipeline(sam, "full", seq()));
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("a: new") && sum.contains("SAM: using this clip's own precomputed pre-matte"), "{sum}");
    assert!(sum.contains("b: blocked: SAM pre-matte missing") && sum.contains("precompute this clip's SAM pre-matte first"), "{sum}");
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let tasks = app.ai().scheduler().task_snapshot();
    assert!(tasks.iter().all(|t| t.job.kind == TaskKind::Matanyone2 && t.job.clip_root.ends_with("clips/a")), "{tasks:?}");
    assert!(!e.project.join("clips/b/VideoMamaMaskHint").exists());
    assert_eq!(app.ai().scheduler().snapshot().len(), 1, "only a is keyed");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn hint_mode_none_runs_key_only_and_says_so() {
    let e = env("none", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let (ctx, mut app) = start(&e, &["a"], &pipeline(HintCfg::default(), "none", seq()));
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("Key only: hints disabled (mode none)") && sum.contains("GVM"), "{sum}");
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert!(app.ai().scheduler().task_snapshot().is_empty());
    assert_eq!(app.ai().scheduler().snapshot().len(), 1);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn clips_writing_the_same_result_are_held_back_before_queueing() {
    let e = env("collide", &[("a", 3, 24.0, None), ("b", 3, 24.0, None)], json!({"version": 2}));
    let mut p = pipeline(HintCfg::default(), "none", seq());
    p.location = Some(json!({"root": e.base.join("shared"), "folder_template": "same", "file_template": "f_####"}));
    let (ctx, mut app) = start(&e, &["a", "b"], &p);
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("a: new") && sum.contains("b: blocked") && sum.contains("same result folder"), "{sum}");
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert_eq!(app.ai().scheduler().snapshot().len(), 1);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn bypassed_hint_keys_without_a_hint_even_when_the_preset_mode_is_full() {
    let e = env("bypass", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let mut h = birefnet_matanyone();
    h.bypass = true;
    let p = pipeline(h, "full", seq());
    assert_eq!(p.params.hint_mode, "full", "the stored preset keeps its mode");
    let (ctx, mut app) = start(&e, &["a"], &p);
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("a: new") && sum.contains("Key only: hint step bypassed"), "{sum}");
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert!(app.ai().scheduler().task_snapshot().is_empty());
    let keys = app.ai().scheduler().snapshot();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].job.params["hint_mode"], "none");
    assert_eq!(keys[0].job.params["use_alpha_hint"], false);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn choosing_a_saved_preset_in_the_batch_dropdown_recomputes_the_rows_and_chain() {
    let e = env("dropdown", &[("a", 4, 24.0, Some((1, 3)))], json!({"version": 2}));
    let full = pipeline(birefnet_matanyone(), "full", export_on());
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: json!({"params": {"despill_strength": 0.2}}) };
    ck::presets::save(&e.base.join("presets.json"), &[ck::presets::pipeline_preset("full", &full), legacy]).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() },
    );
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    app.select_folders(&["a"]);
    app.key_selected(&ctx);
    pump(&ctx, &mut app, "plain review", |a| a.ai().batch_ready());
    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    let plain = app.ai().batch_summary().join("\n");
    assert!(plain.contains("a: new") && !plain.contains("export"), "{plain}");
    // The review has no preset choice: the Board's selector picks it, and a clean Key queues at once.
    app.ai_mut().cancel_batch();
    for _ in 0..4 {
        run(&ctx, &mut app, vec![]);
    }
    let at = app.ai().board_preset_rect.expect("Board preset selector drawn").center();
    click_on(&ctx, &mut app, at);
    for _ in 0..4 {
        run(&ctx, &mut app, vec![]);
    }
    let items = app.ai().board_preset_items.clone();
    let names: Vec<&str> = items.iter().map(|i| i.0.as_str()).collect();
    assert_eq!(names, ["", "full", "old"], "disk presets listed, legacy included");
    let full_at = items.iter().find(|i| i.0 == "full").unwrap().1.center();
    click_on(&ctx, &mut app, full_at);
    for _ in 0..4 {
        run(&ctx, &mut app, vec![]);
    }
    assert!(app.ai().scheduler().task_snapshot().is_empty(), "choosing queues nothing");
    let key = app.ai().board_key_rect.expect("Key button drawn").center();
    click_on(&ctx, &mut app, key);
    pump(&ctx, &mut app, "queued", |a| a.ai().scheduler().task_snapshot().len() == 3);
    let kinds: Vec<TaskKind> = app.ai().scheduler().task_snapshot().iter().map(|t| t.job.kind).collect();
    assert_eq!(kinds, [TaskKind::Birefnet, TaskKind::Matanyone2, TaskKind::ExportVideo]);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

/// Run a one-clip review, confirm it and return (non-Key task kinds in order, Key count, task snapshot).
fn queue_one(e: &Env, p: &Pipeline) -> (Vec<TaskKind>, usize, Vec<ck::scheduler::TaskEntry>) {
    let (ctx, mut app) = start(e, &["a"], p);
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let tasks = app.ai().scheduler().task_snapshot();
    let keys = app.ai().scheduler().snapshot().len();
    drop(app);
    (tasks.iter().map(|t| t.job.kind).collect(), keys, tasks)
}

#[test]
fn prepared_manual_sam_or_chroma_direct_promotes_the_prematte_and_missing_ones_block_with_instructions() {
    for guide in [Producer::Sam, Producer::Chroma] {
        let e = env("direct", &[("a", 4, 24.0, None)], json!({"version": 2}));
        let p = pipeline(route(guide, Final::Direct), "full", seq());
        let (ctx, app) = start(&e, &["a"], &p);
        let sum = app.ai().batch_summary().join("\n");
        assert!(sum.contains("blocked") && sum.contains("precompute this clip's") && sum.contains("pre-matte"), "{guide:?}: {sum}");
        assert!(app.ai().scheduler().task_snapshot().is_empty());
        drop((ctx, app));
        // Partially covering the range still blocks: direct/prematte needs every frame.
        let g = e.project.join("clips/a/VideoMamaMaskHint");
        fs::create_dir_all(&g).unwrap();
        write_png(&g.join("f_1101.png"));
        let (ctx, app) = start(&e, &["a"], &p);
        assert!(app.ai().batch_summary().join("").contains("blocked"));
        drop((ctx, app));
        for i in 1..4 {
            write_png(&g.join(format!("f_{}.png", 1101 + i)));
        }
        let (kinds, keys, tasks) = queue_one(&e, &p);
        assert_eq!((kinds, keys), (vec![TaskKind::PrematteHint], 1), "{guide:?}");
        assert!(tasks[0].job.publish_dir.as_ref().unwrap().ends_with("AlphaHint"));
        let _ = fs::remove_dir_all(e.base);
    }
}

#[test]
fn automatic_gvm_and_birefnet_chains_end_in_prematte_promotion_or_the_refiner() {
    for (guide, fin, want) in [
        (Producer::Gvm, Final::Direct, vec![TaskKind::Gvm, TaskKind::PrematteHint]),
        (Producer::Birefnet, Final::Direct, vec![TaskKind::Birefnet, TaskKind::PrematteHint]),
        (Producer::Gvm, Final::Videomama, vec![TaskKind::Gvm, TaskKind::Videomama]),
        (Producer::Birefnet, Final::Matanyone2, vec![TaskKind::Birefnet, TaskKind::Matanyone2]),
    ] {
        let e = env("auto", &[("a", 4, 24.0, Some((1, 3)))], json!({"version": 2}));
        let (kinds, keys, tasks) = queue_one(&e, &pipeline(route(guide, fin), "full", export_on()));
        let mut want = want;
        want.push(TaskKind::ExportVideo);
        assert_eq!((kinds, keys), (want, 1), "{guide:?} {fin:?}");
        assert!(tasks[0].job.publish_dir.as_ref().unwrap().ends_with("VideoMamaMaskHint"), "automatic pre-matte lands in the guide folder");
        assert!(tasks[1].job.publish_dir.as_ref().unwrap().ends_with("AlphaHint"));
        let _ = fs::remove_dir_all(e.base);
    }
}

#[test]
fn no_hint_keys_then_delivers_every_enabled_video_in_order_without_overwriting() {
    let e = env("multi", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let mut p = pipeline(HintCfg::default(), "none", seq());
    p.outputs = OutputCfg { fg_format: "png".into(), ..OutputCfg::default() };
    p.export = PipelineExport::for_outputs(&p.outputs);
    p.export.outputs.insert("matte".into(), video("mp4", "hevc", 22));
    p.export.outputs.insert("processed".into(), video("mov_prores4444", "prores422", 7));
    // A file already at the matte name: the next free name is used, the file stays.
    let ex = e.project.join("Exports/a");
    fs::create_dir_all(&ex).unwrap();
    fs::write(ex.join("a_matte_f_1101-f_1103.mp4"), b"keep").unwrap();
    let (ctx, mut app) = start(&e, &["a"], &p);
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let tasks = app.ai().scheduler().task_snapshot();
    let key = app.ai().scheduler().snapshot().remove(0);
    assert_eq!(tasks.iter().map(|t| t.job.kind).collect::<Vec<_>>(), [TaskKind::ExportVideo, TaskKind::ExportVideo]);
    let (m, pr) = (&tasks[0], &tasks[1]);
    assert_eq!((m.job.params["product"].as_str(), pr.job.params["product"].as_str()), (Some("matte"), Some("processed")));
    assert_eq!((m.job.params["codec"].as_str(), m.job.params["quality"].as_u64()), (Some("hevc"), Some(22)));
    assert_eq!((pr.job.params["container"].as_str(), pr.job.params["codec"].as_str(), pr.job.params["quality"].as_u64()), (Some("mov_prores4444"), Some("prores422"), Some(7)));
    assert!(m.job.params["dest"].as_str().unwrap().ends_with("a_matte_f_1101-f_1103_v2.mp4"));
    assert!(pr.job.params["dest"].as_str().unwrap().ends_with("a_processed_f_1101-f_1103.mov"));
    // Strictly sequential after Key: Key <- matte <- processed (no sibling writers on one clip).
    assert_eq!(m.after, [key.job.job_id.clone()]);
    assert_eq!(pr.after, [m.job.job_id.clone()]);
    assert_eq!(m.job.params["manifest_dir"], key.job.manifest_dir.display().to_string());
    assert_eq!(fs::read(ex.join("a_matte_f_1101-f_1103.mp4")).unwrap(), b"keep");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn queue_pipeline_runs_the_open_clip_without_a_review_and_reports_why_it_cannot() {
    let e = env("oneoff", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    // Missing manual pre-matte: nothing queued, instruction text available.
    let blocked = pipeline(route(Producer::Sam, Final::Direct), "full", seq());
    app.ai_mut().queue_pipeline(&ctx, &proj, &Settings::default(), 0, &blocked);
    pump(&ctx, &mut app, "blocked", |a| a.ai().pipeline_block_reason().is_some());
    assert!(app.ai().pipeline_block_reason().unwrap().contains("precompute"));
    assert!(app.ai().scheduler().snapshot().is_empty());
    app.ai_mut().queue_pipeline(&ctx, &proj, &Settings::default(), 0, &pipeline(HintCfg::default(), "none", export_on()));
    pump(&ctx, &mut app, "queued", |a| !a.ai().scheduler().snapshot().is_empty());
    assert!(app.ai().pipeline_block_reason().is_none());
    assert_eq!(app.ai().scheduler().task_snapshot().len(), 1, "the video follows Key");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

fn open_app(e: &Env) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    for _ in 0..12 {
        run(&ctx, &mut app, vec![]);
    }
    (ctx, app)
}

#[test]
fn board_preset_selector_is_visible_before_review_and_drives_the_plan() {
    let e = env("board", &[("a", 4, 24.0, Some((1, 3)))], json!({"version": 2}));
    let full = pipeline(birefnet_matanyone(), "full", export_on());
    ck::presets::save(&e.base.join("presets.json"), &[ck::presets::pipeline_preset("full", &full)]).unwrap();
    let (ctx, mut app) = open_app(&e);
    app.select_folders(&["a"]);
    run(&ctx, &mut app, vec![]);
    // The selector is on the Board before any review exists, listing the same saved file.
    let at = app.ai().board_preset_rect.expect("Board preset selector drawn").center();
    click_on(&ctx, &mut app, at);
    for _ in 0..4 {
        run(&ctx, &mut app, vec![]);
    }
    let items = app.ai().board_preset_items.clone();
    assert_eq!(items.iter().map(|i| i.0.as_str()).collect::<Vec<_>>(), ["", "full"]);
    click_on(&ctx, &mut app, items[1].1.center());
    for _ in 0..4 {
        run(&ctx, &mut app, vec![]);
    }
    let key = app.ai().board_key_rect.expect("Key button drawn").center();
    click_on(&ctx, &mut app, key);
    pump(&ctx, &mut app, "queued", |a| !a.ai().scheduler().task_snapshot().is_empty());
    let kinds: Vec<TaskKind> = app.ai().scheduler().task_snapshot().iter().map(|t| t.job.kind).collect();
    assert!(kinds.contains(&TaskKind::ExportVideo), "the chosen preset's pipeline is planned: {kinds:?}");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn queued_preset_persists_its_effective_hint_mode_over_an_explicit_none() {
    let e = env("mode", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let cj = e.project.join("clips/a/clip.json");
    fs::write(&cj, json!({"hint_mode": "none"}).to_string()).unwrap();
    let p = pipeline(HintCfg::default(), "full", seq());
    let (ctx, mut app) = start(&e, &["a"], &p);
    confirm(&ctx, &mut app);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    drop(app);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    assert_eq!(ck::persist::clip_hint_mode(&proj.clips[0]).as_deref(), Some("full"), "reload shows full, not the old none");
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn existing_key_result_is_delivered_without_a_new_key_in_sequence_and_never_overwrites() {
    let e = env("existing", &[("a", 3, 24.0, None)], json!({"version": 2}));
    let out = e.project.join("clips/a/Output/a_v001");
    fs::create_dir_all(out.join("Matte")).unwrap();
    let files: Vec<String> = (0..3).map(|i| format!("Matte/f_{}.png", 1101 + i)).collect();
    for f in &files {
        write_png(&out.join(f));
    }
    let stems: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
    fs::write(
        out.join(".corridorkey_manifest.json"),
        json!({"version": 3, "engine": "corridorkey-v2", "clip_id": "clip-a", "range": {"in_point": 0, "out_point": 2, "in_stem": "f_1101", "out_stem": "f_1103"}, "enabled_outputs": ["matte"], "formats": {"matte": "png"},
               "source_stems": stems, "files": {"matte": files}, "complete": true})
        .to_string(),
    )
    .unwrap();
    fs::write(e.project.join("clips/a/clip.json"), json!({"clip_id": "clip-a", "key_result": {"manifest_dir": out}}).to_string()).unwrap();
    let (_ctx, mut app) = open_app(&e);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let clip = &proj.clips[0];
    let mut cfg = seq();
    cfg.outputs.insert("matte".into(), video("mp4", "h264", 18));
    let ids = app.ai_mut().submit_existing_deliveries(clip, &e.project, (0, 2), &cfg).unwrap();
    assert_eq!(ids.len(), 1);
    assert!(app.ai().scheduler().snapshot().is_empty(), "no Key was queued");
    let t = app.ai().scheduler().task_snapshot();
    assert_eq!(t[0].job.params["manifest_dir"], out.display().to_string());
    // Refuse invalid existing results before queueing, even when partial exports are allowed.
    let mut incomplete = clip.clone();
    incomplete.manifest.as_mut().unwrap().complete = false;
    assert!(app.ai_mut().submit_existing_deliveries(&incomplete, &e.project, (0, 2), &cfg).unwrap_err().contains("incomplete"));
    let mut outside = clip.clone();
    outside.manifest.as_mut().unwrap().source_stems = Some(vec!["f_1102".into()]);
    let mut partial = cfg.clone();
    partial.allow_partial = true;
    assert!(app.ai_mut().submit_existing_deliveries(&outside, &e.project, (0, 2), &partial).unwrap_err().contains("does not cover"));
    let mut missing = clip.clone();
    missing.manifest.as_mut().unwrap().files.as_mut().unwrap().remove("matte");
    assert!(app.ai_mut().submit_existing_deliveries(&missing, &e.project, (0, 2), &cfg).unwrap_err().contains("matte"));
    let mut wrong = clip.clone();
    wrong.manifest.as_mut().unwrap().clip_id = None;
    assert!(app.ai_mut().submit_existing_deliveries(&wrong, &e.project, (0, 2), &cfg).is_err());
    assert_eq!(app.ai().scheduler().task_snapshot().len(), 1, "refused cases queued nothing");
    let mut other = cfg.clone();
    other.outputs.insert("matte".into(), Delivery::Sequence);
    assert!(app.ai_mut().submit_existing_deliveries(clip, &e.project, (0, 2), &other).is_err(), "no video selected");
    drop(app);
    // The actual Deliver button uses the same existing-result path, with no Key job.
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: Some("a".into()), tab: Some(ck::ui::Tab::Deliver), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    pump(&ctx, &mut app, "local capabilities", |a| a.ai().local_caps().is_some());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    for _ in 0..12 { run(&ctx, &mut app, vec![]); }
    let r = app.deliver_rects().iter().find(|r| r.0 == "export_existing").unwrap().1;
    let right = app.tools_rects().unwrap()[2];
    assert!((r.center().x - right.center().x).abs() < 1.0, "export button centred in the action region");
    click_on(&ctx, &mut app, r.center());
    assert_eq!(app.ai().scheduler().task_snapshot().len(), 1);
    assert!(app.ai().scheduler().snapshot().is_empty(), "clicking Export existing queues no Key");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}
