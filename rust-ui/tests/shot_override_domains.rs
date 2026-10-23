//! Shot overrides for the hint route and the video delivery: a hand edit marks the shot (with the value before it),
//! Batch presets of either kind leave it alone and queue the shot's own, untouched shots inherit, confirmed queues are
//! frozen, Reset returns to the recorded base (per shot, and from Batch for a shot that was never opened), and a
//! deliberate single-shot Apply replaces it. Fake worker, headless egui; run through tools/isolated-test.sh.
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{pipeline_preset, Delivery, Pipeline, PipelineExport, VideoDelivery};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::tasks::TaskKind;
use ck::ui::{App, AppOptions, Tab};
use ck::workflow::{OutputCfg, Params};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 2400.0))), events, ..Default::default() };
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
}
fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        run(ctx, app, vec![]);
    }
}
fn pump(ctx: &egui::Context, app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(25);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        run(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(15));
    }
    frames(ctx, app, 4);
}
fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
    frames(ctx, app, 3);
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}
fn env(tag: &str) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    for name in ["a", "b"] {
        let c = project.join("clips").join(name);
        fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..3u32 {
            let mut e = png::Encoder::new(fs::File::create(c.join(format!("Frames/f_{}.png", 1101 + i))).unwrap(), 4, 4);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
        }
        fs::write(c.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
        fs::write(c.join("clip.json"), "{}").unwrap();
    }
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = common::make_repo(&base, "repo", json!({}));
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0}]}).to_string()).unwrap();
    Env { base, project, settings }
}
fn open(e: &Env, clip: Option<&str>, tab: Tab) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: clip.map(Into::into), tab: Some(tab), ..Default::default() });
    if clip.is_some() {
        pump(&ctx, &mut app, "worker caps", |a| a.ai().local_caps().is_some());
    }
    frames(&ctx, &mut app, 8);
    (ctx, app)
}
fn native(e: &Env, clip: &str) -> Value {
    let v: Value = serde_json::from_slice(&fs::read(e.project.join("clips").join(clip).join("clip.json")).unwrap()).unwrap();
    v["ck_native"].clone()
}
fn video(codec: &str, quality: u32) -> Delivery {
    Delivery::Video(VideoDelivery { container: "mp4".into(), codec: codec.into(), quality })
}
fn preset(hint: HintCfg, matte: Delivery) -> Pipeline {
    let o = OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() };
    let mut export = PipelineExport::for_outputs(&o);
    export.outputs.insert("matte".into(), matte);
    Pipeline { hint, params: Params { hint_mode: "full".into(), ..Params::default() }, export, outputs: o, location: None }
}
fn route(guide: Producer, finish: Final) -> HintCfg {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (guide, finish);
    h
}
/// Leave the open shot (the real Back button) and review both shots with a saved preset chosen on the Board: the
/// app's own in-memory project, no reload, no explicit Save.
fn board_batch(ctx: &egui::Context, app: &mut App, preset: &str) {
    if app.in_workspace() {
        let back = app.back_rect().expect("Back to batch drawn").center();
        click(ctx, app, back);
    }
    app.select_folders(&["a", "b"]);
    frames(ctx, app, 3);
    let at = app.ai().board_preset_rect.expect("Board preset selector").center();
    click(ctx, app, at);
    let item = app.ai().board_preset_items.iter().find(|i| i.0 == preset).unwrap_or_else(|| panic!("preset {preset} listed")).1.center();
    click(ctx, app, item);
    let key = app.ai().board_key_rect.expect("Key button drawn").center();
    click(ctx, app, key);
    pump(ctx, app, "review", |a| a.ai().batch_ready());
}
/// Open a shot from the Board (its card's open button is not exposed: the app API opens it by index).
fn open_shot(ctx: &egui::Context, app: &mut App, i: usize, tab: Tab) {
    app.open_clip(ctx, i);
    app.switch_tab(tab);
    frames(ctx, app, 8);
}
fn save_presets(e: &Env, list: &[(&str, Pipeline)]) {
    let v: Vec<_> = list.iter().map(|(n, p)| pipeline_preset(n, p)).collect();
    ck::presets::save(&e.base.join("presets.json"), &v).unwrap();
}
fn confirm(ctx: &egui::Context, app: &mut App) {
    let at = app.ai().batch_confirm_rect.expect("Queue drawn").center();
    click(ctx, app, at);
}
fn tasks_of(app: &App, clip: &str, kind: TaskKind) -> Vec<Value> {
    app.ai()
        .scheduler()
        .task_snapshot()
        .iter()
        .filter(|t| t.job.kind == kind && t.job.clip_root.ends_with(clip))
        .map(|t| t.job.params.clone())
        .collect()
}


/// A pipeline preset: BiRefNet + MatAnyone2, mask grow 5, matte (and optionally processed) delivered as HEVC at 30 fps.
fn preset_p(processed: bool, matte_on: bool) -> Pipeline {
    let mut hint = route(Producer::Birefnet, Final::Matanyone2);
    hint.mask_cleanup.grow = 5;
    let o = OutputCfg { fg_enabled: false, comp_enabled: false, matte_enabled: matte_on, processed_enabled: processed, matte_format: "png".into(), processed_format: "exr".into(), ..OutputCfg::default() };
    let mut export = PipelineExport::for_outputs(&o);
    if matte_on {
        export.outputs.insert("matte".into(), video("hevc", 23));
    }
    if processed {
        export.outputs.insert("processed".into(), Delivery::Video(VideoDelivery { container: "mov_prores4444".into(), codec: "prores4444".into(), quality: 9 }));
    }
    export.fps = Some(30.0);
    Pipeline { hint, params: Params { hint_mode: "full".into(), ..Params::default() }, export, outputs: o, location: None }
}
fn export_params(app: &App, clip: &str) -> Vec<Value> {
    tasks_of(app, clip, TaskKind::ExportVideo)
}

// -------------------------------------------------------------------------------------------------- route

#[test]
fn a_real_bypass_click_then_the_board_batch_keeps_the_bypass_and_the_rest_follows_the_preset() {
    let e = env("dom-route");
    save_presets(&e, &[("p", preset_p(false, true))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Hint);
    assert!(app.override_state_for_test().0.is_empty(), "opening the page marks nothing");
    let at = app.preset_rects().iter().find(|r| r.0 == "bypass").expect("bypass drawn").1.center();
    click(&ctx, &mut app, at); // the real control
    assert_eq!(app.override_state_for_test().0, ["bypass"], "only the field that changed is marked");
    assert_eq!(native(&e, "a")["route_override"]["fields"]["bypass"]["base"], false);
    // Back to the Board and review with the preset: the app's own project, no reload, no Save.
    board_batch(&ctx, &mut app, "p");
    let sum = app.ai().batch_summary();
    assert!(sum.iter().any(|l| l.starts_with("a:") && l.contains("hint route: bypass")), "{sum:?}");
    assert!(!sum.iter().any(|l| l.starts_with("b:") && l.contains("hint route")), "the untouched shot inherits");
    confirm(&ctx, &mut app);
    assert!(tasks_of(&app, "a", TaskKind::Birefnet).is_empty() && tasks_of(&app, "a", TaskKind::Matanyone2).is_empty(), "a's bypass was kept: no hint stage");
    assert_eq!(tasks_of(&app, "b", TaskKind::Birefnet).len(), 1, "b follows the preset");
    let n = native(&e, "a");
    assert_eq!(n["hint"]["bypass"], true, "confirming did not clobber the shot's own field");
    assert_eq!(n["route_override"]["fields"]["bypass"]["base"], false);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn only_the_edited_route_fields_are_the_shots_the_others_inherit_the_preset_and_the_queue_is_frozen() {
    let e = env("dom-route-fields");
    save_presets(&e, &[("p", preset_p(false, true))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Hint);
    let mut own = app.hint_cfg_for_test().unwrap();
    own.mask_cleanup.grow = 9; // one independent field, set the way a graph rewire sets it
    app.hint_apply_cfg_for_test(own);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.override_state_for_test().0, ["mask_cleanup.grow"]);
    board_batch(&ctx, &mut app, "p");
    confirm(&ctx, &mut app);
    let (ta, tb) = (tasks_of(&app, "a", TaskKind::Birefnet), tasks_of(&app, "b", TaskKind::Birefnet));
    assert_eq!((ta.len(), tb.len()), (1, 1), "the producer is inherited by both (only the grow field is a's own)");
    assert_eq!((ta[0]["mask_cleanup"]["grow"].as_i64(), tb[0]["mask_cleanup"]["grow"].as_i64()), (Some(9), Some(5)));
    let n = native(&e, "a");
    assert_eq!(n["hint"]["mask_cleanup"]["grow"], 9);
    assert_eq!(n["hint"]["guide"], json!(Producer::Birefnet), "the preset's producer was applied to the shot");
    assert_eq!(n["route_override"]["fields"]["mask_cleanup.grow"]["base"], 5, "its Reset base now follows the confirmed preset");
    // Later edits do not touch what is already queued.
    let frozen = tasks_of(&app, "a", TaskKind::Birefnet);
    open_shot(&ctx, &mut app, 0, Tab::Hint);
    let mut c = app.hint_cfg_for_test().unwrap();
    c.mask_cleanup.grow = 20;
    app.hint_apply_cfg_for_test(c);
    frames(&ctx, &mut app, 3);
    assert_eq!(tasks_of(&app, "a", TaskKind::Birefnet), frozen);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn the_eyedropper_colour_is_a_marked_route_edit_and_reset_restores_the_field() {
    let e = env("dom-eyedropper");
    let (ctx, mut app) = open(&e, Some("a"), Tab::Hint);
    app.pick_colour_for_test([10, 200, 30]).unwrap();
    frames(&ctx, &mut app, 3);
    assert_eq!(app.override_state_for_test().0, ["chroma.screen_color"]);
    let n = native(&e, "a");
    assert_eq!(n["hint"]["chroma"]["screen_color"], json!([10, 200, 30]));
    assert_eq!(n["route_override"]["fields"]["chroma.screen_color"]["base"], Value::Null);
    app.reset_route_for_test();
    frames(&ctx, &mut app, 3);
    assert!(app.override_state_for_test().0.is_empty() && app.hint_cfg_for_test().unwrap().chroma.screen_color.is_none());
    let n = native(&e, "a");
    assert!(n.get("route_override").is_none() && n["hint"]["chroma"]["screen_color"].is_null(), "{n}");
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn route_reset_by_a_deliberate_apply_and_from_batch_for_a_never_opened_shot() {
    let e = env("dom-route-reset");
    save_presets(&e, &[("looks", preset_p(false, true))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Hint);
    let mut c = app.hint_cfg_for_test().unwrap();
    c.mask_cleanup.grow += 4;
    app.hint_apply_cfg_for_test(c);
    frames(&ctx, &mut app, 3);
    app.reset_route_for_test();
    assert_eq!(app.hint_cfg_for_test().unwrap(), HintCfg::default(), "back to the value before the edit");
    // Edit again, then a deliberate Apply replaces the field and records the preset as the source.
    let mut c = app.hint_cfg_for_test().unwrap();
    c.mask_cleanup.grow += 4;
    app.hint_apply_cfg_for_test(c);
    frames(&ctx, &mut app, 3);
    app.switch_tab(Tab::Key);
    frames(&ctx, &mut app, 8);
    let at = app.preset_rects().iter().find(|r| r.0 == "apply").expect("Apply drawn").1.center();
    click(&ctx, &mut app, at);
    assert!(app.override_state_for_test().0.is_empty(), "Apply replaced the shot's own fields");
    let n = native(&e, "a");
    assert!(n.get("route_override").is_none() && n["route_override_src"] == "looks", "{n}");
    // Batch "Use preset values": a field of shot b left from an earlier session, never opened now.
    let mut hb = HintCfg::default();
    hb.mask_cleanup.grow = 7;
    drop(app);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    hb.save(&proj.clips[1]).unwrap();
    let cj = e.project.join("clips/b/clip.json");
    let mut v: Value = serde_json::from_slice(&fs::read(&cj).unwrap()).unwrap();
    v["ck_native"]["route_override"] = json!({"fields": {"mask_cleanup.grow": {"base": 0, "preset": null}}});
    fs::write(&cj, v.to_string()).unwrap();
    let (ctx, mut app) = open(&e, None, Tab::Hint);
    board_batch(&ctx, &mut app, "looks");
    assert!(app.ai().batch_summary().iter().any(|l| l.starts_with("b:") && l.contains("hint route: mask_cleanup.grow")));
    let at = app.ai().batch_reset_rects.iter().find(|r| r.0 == "b").expect("Use preset values drawn").1.center();
    click(&ctx, &mut app, at);
    pump(&ctx, &mut app, "rebuilt", |a| a.ai().batch_ready() && !a.ai().batch_summary().iter().any(|l| l.starts_with("b:") && l.contains("hint route")));
    app.ai_mut().cancel_batch();
    let n = native(&e, "b");
    assert!(n.get("route_override").is_none());
    assert_eq!(n["hint"]["mask_cleanup"]["grow"], 5, "the SELECTED preset's value was written back");
    assert_eq!(n["route_override_src"], "looks");
    let _ = fs::remove_dir_all(e.base);
}

// -------------------------------------------------------------------------------------------------- delivery

#[test]
fn one_outputs_delivery_is_the_shots_own_and_survives_other_shots_the_board_and_a_batch_without_save() {
    let e = env("dom-delivery");
    save_presets(&e, &[("p", preset_p(true, true))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Deliver);
    assert!(app.override_state_for_test().1.is_empty(), "showing the page marks nothing");
    let at = app.deliver_format_rects().iter().find(|r| r.0 == "matte_video").expect("Video file drawn").1.center();
    click(&ctx, &mut app, at); // the real control: matte as a video (MP4 H.264 default)
    assert_eq!(app.override_state_for_test().1, ["matte"], "only that output is the shot's own");
    let n = native(&e, "a"); // written without Save
    assert_eq!(n["delivery_override"]["fields"]["matte"]["base"], json!(Delivery::Sequence));
    assert!(n["delivery"]["outputs"]["matte"].to_string().contains("h264"), "{n}");
    // Board -> shot b -> Board -> shot a again: nothing was lost.
    app.back_rect().map(|r| r.center()).map(|at| click(&ctx, &mut app, at));
    open_shot(&ctx, &mut app, 1, Tab::Deliver);
    assert!(app.override_state_for_test().1.is_empty(), "b has none");
    app.back_rect().map(|r| r.center()).map(|at| click(&ctx, &mut app, at));
    open_shot(&ctx, &mut app, 0, Tab::Deliver);
    assert_eq!(app.override_state_for_test().1, ["matte"]);
    assert!(matches!(app.export_state_for_test().0["matte"], Delivery::Video(_)), "the edit is still shown");
    // Batch: a keeps its matte H.264, inherits processed and fps from the preset; b inherits everything.
    board_batch(&ctx, &mut app, "p");
    assert!(app.ai().batch_summary().iter().any(|l| l.starts_with("a:") && l.contains("delivery: matte") && !l.contains("delivery: fps")));
    confirm(&ctx, &mut app);
    pump(&ctx, &mut app, "exports queued", |a| a.ai().scheduler().task_snapshot().len() >= 4);
    let (ea, eb) = (export_params(&app, "a"), export_params(&app, "b"));
    let codec = |v: &[Value], product: &str| v.iter().find(|p| p["product"] == product).and_then(|p| p["codec"].as_str().map(str::to_owned));
    assert_eq!((codec(&ea, "matte").as_deref(), codec(&ea, "processed").as_deref()), (Some("h264"), Some("prores4444")));
    assert_eq!((codec(&eb, "matte").as_deref(), codec(&eb, "processed").as_deref()), (Some("hevc"), Some("prores4444")));
    assert!(ea.iter().chain(&eb).all(|p| p["fps"] == 30.0), "fps was never edited: the preset's applies to both");
    // The queued exports are frozen.
    let frozen: Vec<Value> = app.ai().scheduler().task_snapshot().iter().map(|t| t.job.params.clone()).collect();
    open_shot(&ctx, &mut app, 0, Tab::Deliver);
    app.reset_delivery_for_test();
    frames(&ctx, &mut app, 3);
    assert_eq!(app.ai().scheduler().task_snapshot().iter().map(|t| t.job.params.clone()).collect::<Vec<_>>(), frozen);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn delivery_reset_is_saved_and_reloads_and_a_batch_reset_uses_the_selected_presets_values() {
    let e = env("dom-delivery-reset");
    save_presets(&e, &[("p", preset_p(false, true))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Deliver);
    let at = app.deliver_format_rects().iter().find(|r| r.0 == "matte_video").unwrap().1.center();
    click(&ctx, &mut app, at);
    app.reset_delivery_for_test();
    frames(&ctx, &mut app, 3);
    assert!(matches!(app.export_state_for_test().0["matte"], Delivery::Sequence));
    let n = native(&e, "a");
    assert!(n.get("delivery_override").is_none() && !n["delivery"]["outputs"]["matte"].to_string().contains("h264"), "{n}");
    drop(app);
    let (_ctx, app) = open(&e, Some("a"), Tab::Deliver);
    assert!(app.override_state_for_test().1.is_empty() && matches!(app.export_state_for_test().0["matte"], Delivery::Sequence), "reloaded as reset");
    drop(app);
    // Edit again; Batch "Use preset values" on the unopened shot writes the preset's delivery back.
    let (ctx, mut app) = open(&e, Some("a"), Tab::Deliver);
    let at = app.deliver_format_rects().iter().find(|r| r.0 == "matte_video").unwrap().1.center();
    click(&ctx, &mut app, at);
    board_batch(&ctx, &mut app, "p");
    let at = app.ai().batch_reset_rects.iter().find(|r| r.0 == "a").expect("Use preset values drawn").1.center();
    click(&ctx, &mut app, at);
    pump(&ctx, &mut app, "rebuilt", |a| a.ai().batch_ready() && !a.ai().batch_summary().iter().any(|l| l.starts_with("a:") && l.contains("delivery:")));
    app.ai_mut().cancel_batch();
    let n = native(&e, "a");
    assert!(n.get("delivery_override").is_none());
    assert!(n["delivery"]["outputs"]["matte"].to_string().contains("hevc"), "the preset's HEVC, not the shot's H.264 or a default: {n}");
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_kept_delivery_the_presets_outputs_cannot_write_is_a_clear_refusal_not_a_silent_drop() {
    let e = env("dom-incompat");
    save_presets(&e, &[("processed_only", preset_p(true, false))]);
    let (ctx, mut app) = open(&e, Some("a"), Tab::Deliver);
    let at = app.deliver_format_rects().iter().find(|r| r.0 == "matte_video").unwrap().1.center();
    click(&ctx, &mut app, at); // a wants a matte video; the preset writes no matte
    board_batch(&ctx, &mut app, "processed_only");
    let sum = app.ai().batch_summary();
    assert!(sum.iter().any(|l| l.starts_with("a:") && l.contains("export blocked") && l.contains("does not write that output")), "{sum:?}");
    confirm(&ctx, &mut app);
    assert!(app.ai().scheduler().snapshot().iter().all(|j| !j.job.clip_root.ends_with("a")), "a was refused, not queued without its video");
    assert!(app.ai().scheduler().snapshot().iter().any(|j| j.job.clip_root.ends_with("b")), "b is unaffected");
    let _ = fs::remove_dir_all(e.base);
}
