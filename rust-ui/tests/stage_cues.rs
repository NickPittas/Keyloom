//! Recompute ("Update required") cues after real completions through the fake worker: a successful run of a stage
//! promotes exactly what it ran with; failures, other outputs' successes and edits made while it ran do not
//! clear anything. Covers pre-matte/hint (pipeline runs), Key, and Deliver (export-only, multi-output).
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{Delivery, Pipeline, PipelineExport, VideoDelivery};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
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
}
fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
    frames(ctx, app, 3);
}
fn video(container: &str, codec: &str, quality: u32) -> Delivery {
    Delivery::Video(VideoDelivery { container: container.into(), codec: codec.into(), quality })
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}
/// One clip `a` (3 frames); `key_result` writes a complete Key result for matte + processed.
fn env(tag: &str, repo_cfg: Value, key_result: bool) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    let clip = project.join("clips/a");
    fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..3u32 {
        let mut e = png::Encoder::new(fs::File::create(clip.join(format!("Frames/f_{}.png", 1101 + i))).unwrap(), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
    }
    fs::write(clip.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    let other = project.join("clips/b");
    fs::create_dir_all(other.join("Frames")).unwrap();
    for i in 0..3u32 {
        let mut e = png::Encoder::new(fs::File::create(other.join(format!("Frames/f_{}.png", 1101 + i))).unwrap(), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
    }
    fs::write(other.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    fs::write(other.join("clip.json"), "{}").unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    if key_result {
        let out = clip.join("Output/a_v001");
        let stems: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
        let mut files = serde_json::Map::new();
        for (dir, name) in [("Matte", "matte"), ("Processed", "processed")] {
            fs::create_dir_all(out.join(dir)).unwrap();
            let list: Vec<String> = (0..3).map(|i| format!("{dir}/f_{}.png", 1101 + i)).collect();
            for f in &list {
                fs::write(out.join(f), b"x").unwrap();
            }
            files.insert(name.into(), json!(list));
        }
        let d = Params::default();
        fs::write(
            out.join(".corridorkey_manifest.json"),
            json!({"version": 3, "engine": "corridorkey-v2", "clip_id": "clip-a", "range": {"in_point": 0, "out_point": 2, "in_stem": "f_1101", "out_stem": "f_1103"},
                   "enabled_outputs": ["matte", "processed"], "formats": {"matte": "png", "processed": "exr"}, "exr_compression": "dwab", "params": d,
                   "source_stems": stems, "files": files, "complete": true})
            .to_string(),
        )
        .unwrap();
        fs::write(clip.join("clip.json"), json!({"clip_id": "clip-a", "key_result": {"manifest_dir": out}}).to_string()).unwrap();
    } else {
        fs::write(clip.join("clip.json"), "{}").unwrap();
    }
    let repo = common::make_repo(&base, "repo", repo_cfg);
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0}]}).to_string()).unwrap();
    Env { base, project, settings }
}
fn open(e: &Env, tab: Tab) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: Some("a".into()), tab: Some(tab), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    pump(&ctx, &mut app, "worker caps", |a| a.ai().local_caps().is_some());
    frames(&ctx, &mut app, 12);
    (ctx, app)
}
/// The Key outputs the fixture's manifest was made with: only matte (png) and processed (exr).
fn match_manifest(app: &mut App) {
    app.draft_for_test(|_, o| {
        o.fg_enabled = false;
        o.comp_enabled = false;
        o.matte_format = "png".into();
        o.processed_format = "exr".into();
    });
}
fn press(ctx: &egui::Context, app: &mut App, key: &str) {
    let at = app.deliver_rects().iter().find(|r| r.0 == key).unwrap_or_else(|| panic!("{key} not drawn")).1.center();
    click(ctx, app, at);
}
fn tasks_done(app: &App) -> usize {
    app.ai().scheduler().task_snapshot().iter().filter(|t| t.state.is_terminal()).count()
}

// ------------------------------------------------------------------------------------------------ Deliver

#[test]
fn export_cue_tracks_each_output_with_its_frozen_settings() {
    let e = env("cue-export", json!({"task_delay": 0.5}), true);
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    frames(&ctx, &mut app, 8);
    assert!(app.deliver_pending_for_test().is_empty(), "image sequences leave nothing for Deliver to encode");
    assert!(app.ai().stale_deliver.is_none() && app.ai().stale_key.is_none(), "nothing is stale just for being shown");
    // Two video outputs are chosen: neither has been encoded yet.
    app.set_export_for_test(&[("matte", video("mp4", "hevc", 23)), ("processed", video("mov_prores4444", "prores4444", 12))], None, false, None);
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte", "processed"]);
    assert!(app.ai().stale_deliver.is_some());
    press(&ctx, &mut app, "export_existing");
    assert_eq!(app.ai().scheduler().task_snapshot().len(), 2);
    // The first success clears only its own output.
    pump(&ctx, &mut app, "first export done", |a| tasks_done(a) >= 1);
    frames(&ctx, &mut app, 3);
    assert_eq!(tasks_done(&app), 1, "the second export is still running");
    assert_eq!(app.deliver_pending_for_test(), ["processed"], "matte is done, processed is still pending");
    assert!(app.ai().stale_deliver.is_some());
    // An edit made while the second export runs is not covered by it.
    app.set_export_for_test(&[("matte", video("mp4", "hevc", 30)), ("processed", video("mov_prores4444", "prores4444", 12))], None, false, None);
    pump(&ctx, &mut app, "both exports done", |a| tasks_done(a) >= 2);
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "processed was encoded as configured; the later matte edit is still pending");
    // Exporting again at the current settings finishes the job and the cue goes out.
    press(&ctx, &mut app, "export_existing");
    pump(&ctx, &mut app, "third export done", |a| tasks_done(a) >= 3 || a.deliver_pending_for_test().is_empty());
    frames(&ctx, &mut app, 4);
    assert!(app.deliver_pending_for_test().is_empty());
    assert!(!app.stage_pending_for_test("deliver") && app.ai().stale_deliver.is_none(), "no outline once everything is current");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn failed_exports_never_clear_the_cue_and_sequence_choices_are_never_pending() {
    let e = env("cue-export-fail", json!({"modes": {"export_video": "fail"}}), true);
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 12);
    app.set_export_for_test(&[("matte", video("mp4", "hevc", 23))], None, false, None);
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte"]);
    press(&ctx, &mut app, "export_existing");
    pump(&ctx, &mut app, "export failed", |a| tasks_done(a) >= 1);
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "a failed export clears nothing");
    // Switching the output to an image sequence leaves nothing for Deliver to render: it is not stuck pending.
    app.set_export_for_test(&[], None, false, None);
    frames(&ctx, &mut app, 6);
    assert!(app.deliver_pending_for_test().is_empty() && !app.stage_pending_for_test("deliver"));
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

// ------------------------------------------------------------------------------------------------ pre-matte / hint

fn route(guide: Producer, finish: Final) -> HintCfg {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (guide, finish);
    h
}
fn pipeline(hint: HintCfg) -> Pipeline {
    let o = OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() };
    Pipeline { hint, params: Params { hint_mode: "full".into(), ..Params::default() }, export: PipelineExport::for_outputs(&o), outputs: o, location: None }
}
fn run_pipeline(ctx: &egui::Context, app: &mut App, e: &Env, p: &Pipeline) {
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    app.ai_mut().queue_pipeline(ctx, &proj, &Settings::default(), 0, p);
    pump(ctx, app, "pipeline queued", |a| !a.ai().scheduler().snapshot().is_empty() || a.ai().pipeline_block_reason().is_some());
}
fn route_changed() -> HintCfg {
    route(Producer::Birefnet, Final::Matanyone2)
}

#[test]
fn a_pipeline_run_of_pre_matte_and_hint_clears_exactly_what_it_ran() {
    let e = env("cue-pipe", json!({"task_delay": 0.4}), false);
    let (ctx, mut app) = open(&e, Tab::Hint);
    assert!(!app.stage_pending_for_test("pre") && !app.stage_pending_for_test("hint"), "nothing configured yet");
    app.hint_apply_cfg_for_test(route_changed());
    frames(&ctx, &mut app, 4);
    assert!(app.stage_pending_for_test("pre") && app.stage_pending_for_test("hint"), "a changed route needs both stages");
    run_pipeline(&ctx, &mut app, &e, &pipeline(route_changed()));
    pump(&ctx, &mut app, "stages done", |a| tasks_done(a) >= 2);
    frames(&ctx, &mut app, 6);
    assert!(!app.stage_pending_for_test("pre"), "the pre-matte it ran is no longer pending");
    assert!(!app.stage_pending_for_test("hint"), "the hint it ran is no longer pending");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn an_edit_during_the_run_and_a_failed_run_keep_the_cue() {
    let e = env("cue-pipe-edit", json!({"task_delay": 0.4}), false);
    let (ctx, mut app) = open(&e, Tab::Hint);
    app.hint_apply_cfg_for_test(route_changed());
    frames(&ctx, &mut app, 4);
    run_pipeline(&ctx, &mut app, &e, &pipeline(route_changed()));
    // Changed while the stages run: the run used the old settings.
    let mut edited = route_changed();
    edited.mask_cleanup.grow += 3;
    app.hint_apply_cfg_for_test(edited);
    pump(&ctx, &mut app, "stages done", |a| tasks_done(a) >= 2);
    frames(&ctx, &mut app, 6);
    assert!(app.stage_pending_for_test("pre") && app.stage_pending_for_test("hint"), "the later edit is still pending");
    drop(app);
    let _ = fs::remove_dir_all(e.base);

    let e = env("cue-pipe-fail", json!({"modes": {"birefnet": "fail", "matanyone2": "fail"}}), false);
    let (ctx, mut app) = open(&e, Tab::Hint);
    app.hint_apply_cfg_for_test(route_changed());
    frames(&ctx, &mut app, 4);
    run_pipeline(&ctx, &mut app, &e, &pipeline(route_changed()));
    pump(&ctx, &mut app, "stage failed", |a| tasks_done(a) >= 1);
    frames(&ctx, &mut app, 6);
    assert!(app.stage_pending_for_test("pre"), "a failed stage clears nothing");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

// ------------------------------------------------------------------------------------------------ Key

fn run_key(ctx: &egui::Context, app: &mut App) {
    app.switch_tab(Tab::Key);
    frames(ctx, app, 6);
    pump(ctx, app, "key gate", |a| a.ai().gate_current() && a.ai().key_rect.is_some());
    let at = app.ai().key_rect.unwrap().center();
    click(ctx, app, at);
    pump(ctx, app, "key queued", |a| !a.ai().scheduler().snapshot().is_empty());
}

#[test]
fn key_cue_clears_after_its_own_success_but_not_after_failure_or_a_later_edit() {
    let e = env("cue-key", json!({"manifest": true}), false);
    let (ctx, mut app) = open(&e, Tab::Key);
    assert!(app.stage_pending_for_test("key"), "no Key result yet");
    run_key(&ctx, &mut app);
    pump(&ctx, &mut app, "result read back", |a| !a.stage_pending_for_test("key") && a.ai().scheduler().snapshot().iter().all(|j| j.state.is_terminal()));
    assert!(app.ai().stale_key.is_none(), "no outline after its own success");
    // A setting changed afterwards makes it pending again.
    app.draft_for_test(|p, _| p.despill_strength = 0.9);
    frames(&ctx, &mut app, 4);
    assert!(app.stage_pending_for_test("key"));
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn key_failure_and_an_edit_made_while_it_runs_keep_the_cue() {
    let e = env("cue-key-fail", json!({"manifest": true, "modes": {"key": "fail"}}), false);
    let (ctx, mut app) = open(&e, Tab::Key);
    run_key(&ctx, &mut app);
    pump(&ctx, &mut app, "key failed", |a| a.ai().scheduler().snapshot().iter().all(|j| j.state.is_terminal()));
    frames(&ctx, &mut app, 6);
    assert!(app.stage_pending_for_test("key"), "a failed Key clears nothing");
    drop(app);
    let _ = fs::remove_dir_all(e.base);

    let e = env("cue-key-edit", json!({"manifest": true, "key_delay": 0.3}), false);
    let (ctx, mut app) = open(&e, Tab::Key);
    run_key(&ctx, &mut app);
    app.draft_for_test(|p, _| p.despill_strength = 0.9); // while it runs
    pump(&ctx, &mut app, "key finished", |a| a.ai().scheduler().snapshot().iter().all(|j| j.state.is_terminal()));
    frames(&ctx, &mut app, 8);
    assert!(app.stage_pending_for_test("key"), "the result was made with the older despill: still pending");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_saved_video_delivery_is_the_baseline_at_open_not_pending() {
    let e = env("cue-export-saved", json!({}), true);
    let mut ex = PipelineExport::default();
    ex.outputs.insert("matte".into(), video("mp4", "h264", 18));
    let cj = e.project.join("clips/a/clip.json");
    let mut v: Value = serde_json::from_slice(&fs::read(&cj).unwrap()).unwrap();
    v["ck_native"] = json!({"delivery": serde_json::to_value(&ex).unwrap()});
    fs::write(&cj, v.to_string()).unwrap();
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    frames(&ctx, &mut app, 8);
    assert!(app.export_state_for_test().0.get("matte").is_some_and(|d| matches!(d, Delivery::Video(_))), "the saved choice was loaded");
    assert!(app.deliver_pending_for_test().is_empty() && app.ai().stale_deliver.is_none());
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

fn all_settled(a: &App) -> bool {
    a.ai().scheduler().snapshot().iter().all(|j| j.state.is_terminal()) && a.ai().scheduler().task_snapshot().iter().all(|t| t.state.is_terminal())
}
fn export_pipeline(hint: HintCfg) -> Pipeline {
    let mut p = pipeline(hint);
    p.export.outputs.insert("matte".into(), video("mp4", "h264", 18));
    p
}

#[test]
fn a_chained_hint_key_export_run_leaves_every_cue_clear_and_it_survives_switching_clips() {
    let e = env("cue-chain", json!({"manifest": true, "task_delay": 0.1}), false);
    let (ctx, mut app) = open(&e, Tab::Hint);
    app.hint_apply_cfg_for_test(route_changed());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 4);
    assert!(app.stage_pending_for_test("pre") && app.stage_pending_for_test("hint") && app.stage_pending_for_test("key"));
    run_pipeline(&ctx, &mut app, &e, &export_pipeline(route_changed()));
    pump(&ctx, &mut app, "chain finished", |a| all_settled(a) && a.ai().scheduler().task_snapshot().len() >= 3);
    pump(&ctx, &mut app, "cues settle", |a| !a.stage_pending_for_test("key"));
    frames(&ctx, &mut app, 6);
    for st in ["pre", "hint", "key", "deliver"] {
        assert!(!app.stage_pending_for_test(st), "{st} is still pending after its own successful run");
    }
    assert!(app.ai().stale_key.is_none(), "no amber on the Key cards");
    // Leave the shot and come back (the cue state is rebuilt from the scheduler's record of this session).
    app.open_clip(&ctx, 1);
    frames(&ctx, &mut app, 8);
    app.open_clip(&ctx, 0);
    frames(&ctx, &mut app, 12);
    for st in ["pre", "hint", "key"] {
        assert!(!app.stage_pending_for_test(st), "{st} came back pending after reopening the shot");
    }
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_video_encoded_from_an_older_key_result_is_pending_again_after_a_new_key() {
    let e = env("cue-replace", json!({"manifest": true, "task_delay": 0.05}), true);
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 6);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "chosen after open and never encoded");
    press(&ctx, &mut app, "export_existing");
    pump(&ctx, &mut app, "export done", all_settled);
    frames(&ctx, &mut app, 6);
    assert!(app.deliver_pending_for_test().is_empty(), "encoded from the current Key result at these settings");
    // A range change, and a partial-result change, make the same video pending again.
    app.viewer_mut_for_test(|v| v.range = Some((0, 1)));
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "another In/Out is another video");
    app.viewer_mut_for_test(|v| v.range = None);
    frames(&ctx, &mut app, 4);
    assert!(app.deliver_pending_for_test().is_empty());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, true, None);
    frames(&ctx, &mut app, 4);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "the partial-result choice is part of the video");
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 4);
    assert!(app.deliver_pending_for_test().is_empty());
    // A new Key result (as a new version) finishes after the export: the video no longer matches it.
    run_key(&ctx, &mut app);
    pump(&ctx, &mut app, "new key done", all_settled);
    app.switch_tab(Tab::Deliver);
    pump(&ctx, &mut app, "new result read", |a| a.stage_pending_for_test("deliver"));
    assert_eq!(app.deliver_pending_for_test(), ["matte"]);
    // Re-encoding from it makes the video current again; switching shots and back keeps that.
    press(&ctx, &mut app, "export_existing");
    frames(&ctx, &mut app, 20);
    pump(&ctx, &mut app, "re-export done", |a| all_settled(a) && !a.stage_pending_for_test("deliver"));
    app.open_clip(&ctx, 1);
    frames(&ctx, &mut app, 6);
    app.open_clip(&ctx, 0);
    frames(&ctx, &mut app, 12);
    assert!(app.deliver_pending_for_test().is_empty(), "replayed from the scheduler record: still current");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn applying_ai_targets_after_a_completed_chain_does_not_turn_the_cues_amber() {
    let e = env("cue-targets", json!({"manifest": true, "task_delay": 0.1}), false);
    let (ctx, mut app) = open(&e, Tab::Hint);
    app.hint_apply_cfg_for_test(route_changed());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 4);
    run_pipeline(&ctx, &mut app, &e, &export_pipeline(route_changed()));
    pump(&ctx, &mut app, "chain finished", |a| all_settled(a) && a.ai().scheduler().task_snapshot().len() >= 3);
    pump(&ctx, &mut app, "cues settle", |a| !a.stage_pending_for_test("key"));
    frames(&ctx, &mut app, 6);
    // The scheduler is rebuilt (the tracked jobs are forgotten); the shot's results are what they were.
    app.ai_mut().set_targets(&Settings::default(), None);
    pump(&ctx, &mut app, "scheduler replaced", |a| a.ai().scheduler().snapshot().is_empty() && !a.ai().targets_pending());
    frames(&ctx, &mut app, 12);
    for st in ["pre", "hint", "key", "deliver"] {
        assert!(!app.stage_pending_for_test(st), "{st} went pending after applying targets");
    }
    app.open_clip(&ctx, 1);
    frames(&ctx, &mut app, 8);
    app.open_clip(&ctx, 0);
    frames(&ctx, &mut app, 12);
    for st in ["pre", "hint", "key"] {
        assert!(!app.stage_pending_for_test(st), "{st} pending after switching shots following the rebuild");
    }
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_new_key_this_session_makes_a_video_configured_at_open_pending() {
    let e = env("cue-open-key", json!({"manifest": true, "task_delay": 0.05}), true);
    let mut ex = PipelineExport::default();
    ex.outputs.insert("matte".into(), video("mp4", "h264", 18));
    let cj = e.project.join("clips/a/clip.json");
    let mut v: Value = serde_json::from_slice(&fs::read(&cj).unwrap()).unwrap();
    v["ck_native"] = json!({"delivery": serde_json::to_value(&ex).unwrap()});
    fs::write(&cj, v.to_string()).unwrap();
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    frames(&ctx, &mut app, 8);
    assert_eq!(app.deliver_unverified_for_test(), ["matte"], "configured at open, no Key seen: unverified, no cue");
    assert!(app.deliver_pending_for_test().is_empty());
    run_key(&ctx, &mut app);
    pump(&ctx, &mut app, "new key done", all_settled);
    app.switch_tab(Tab::Deliver);
    pump(&ctx, &mut app, "new result read", |a| a.stage_pending_for_test("deliver"));
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "a Key made after the app opened is newer than anything verified");
    assert!(app.deliver_unverified_for_test().is_empty());
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_video_is_current_only_for_the_folder_it_was_actually_written_to() {
    let e = env("cue-folder", json!({"task_delay": 0.05}), true);
    let (ctx, mut app) = open(&e, Tab::Deliver);
    match_manifest(&mut app);
    let custom = e.base.join("custom exports");
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, Some(custom.clone()));
    frames(&ctx, &mut app, 6);
    press(&ctx, &mut app, "export_existing");
    pump(&ctx, &mut app, "custom export done", all_settled);
    frames(&ctx, &mut app, 6);
    assert!(app.deliver_pending_for_test().is_empty(), "written to the chosen folder");
    // Back to the project default: that is a different destination.
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx, &mut app, 6);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "custom -> project default is not current");
    press(&ctx, &mut app, "export_existing");
    pump(&ctx, &mut app, "default export done", |a| all_settled(a) && a.deliver_pending_for_test().is_empty());
    // And the other way round.
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, Some(custom));
    frames(&ctx, &mut app, 6);
    assert_eq!(app.deliver_pending_for_test(), ["matte"], "the latest video went to the default folder, not this one");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}
