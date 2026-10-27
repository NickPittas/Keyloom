//! Deliver "Run full workflow": with results already on disk it must not rerun pre-matte/hint/Key without an
//! explicit, informed choice; "Export existing Key result" stays encode-only. Matte MP4 + Processed MOV ProRes 4444.
//! Fake worker, headless egui; run through tools/isolated-test.sh.
mod common;
use ck::presets::{Delivery, VideoDelivery};
use ck::tasks::TaskKind;
use ck::ui::{App, AppOptions, Tab};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::json;
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
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        run(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn press(ctx: &egui::Context, app: &mut App, key: &str) {
    let at = app.deliver_rects().iter().find(|r| r.0 == key).unwrap_or_else(|| panic!("{key} not drawn")).1.center();
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
    frames(ctx, app, 4);
}
fn video(container: &str, codec: &str, quality: u32) -> Delivery {
    Delivery::Video(VideoDelivery { container: container.into(), codec: codec.into(), quality })
}

/// Which stage results the fixture shot already has.
#[derive(Clone, Copy, PartialEq)]
enum Have {
    KeyResult,
    PreMatteOnly,
    HintOnly,
}

fn open(tag: &str) -> (egui::Context, App, PathBuf) {
    open_with(tag, Have::KeyResult)
}

/// A shot with the given existing results and the two video deliveries chosen.
fn open_with(tag: &str, have: Have) -> (egui::Context, App, PathBuf) {
    let base = common::tmp(tag);
    let project = base.join("proj");
    let clip = project.join("clips/a");
    fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..3u8 {
        let mut e = png::Encoder::new(fs::File::create(clip.join(format!("Frames/f_{}.png", 1101 + i as u32))).unwrap(), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[i; 48]).unwrap();
    }
    fs::write(clip.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    if have != Have::KeyResult {
        let dir = if have == Have::PreMatteOnly { "VideoMamaMaskHint" } else { "AlphaHint" };
        fs::create_dir_all(clip.join(dir)).unwrap();
        for i in 0..3u32 {
            let mut e = png::Encoder::new(fs::File::create(clip.join(format!("{dir}/f_{}.png", 1101 + i))).unwrap(), 4, 4);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[255; 16]).unwrap();
        }
        fs::write(clip.join("clip.json"), "{}").unwrap();
    }
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
    fs::write(
        out.join(".corridorkey_manifest.json"),
        json!({"version": 3, "engine": "corridorkey-v2", "clip_id": "clip-a", "range": {"in_point": 0, "out_point": 2, "in_stem": "f_1101", "out_stem": "f_1103"},
               "enabled_outputs": ["matte", "processed"], "formats": {"matte": "png", "processed": "png"}, "source_stems": stems, "files": files, "complete": true})
        .to_string(),
    )
    .unwrap();
    if have == Have::KeyResult {
        fs::write(clip.join("clip.json"), json!({"clip_id": "clip-a", "key_result": {"manifest_dir": out}}).to_string()).unwrap();
    } else {
        fs::remove_dir_all(clip.join("Output")).unwrap();
    }
    let repo = common::make_repo(&base, "repo", json!({}));
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(project), settings_path: Some(settings), open_clip: Some("a".into()), tab: Some(Tab::Deliver), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    pump(&ctx, &mut app, "local capabilities", |a| a.ai().local_caps().is_some());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18)), ("processed", video("mov_prores4444", "prores4444", 9))], None, false, None);
    frames(&ctx, &mut app, 12);
    (ctx, app, base)
}
fn nothing_queued(app: &App) -> bool {
    app.ai().scheduler().snapshot().is_empty() && app.ai().scheduler().task_snapshot().is_empty()
}

#[test]
fn render_with_existing_results_asks_first_and_cancel_queues_nothing() {
    let (ctx, mut app, base) = open("rc-cancel");
    press(&ctx, &mut app, "render");
    frames(&ctx, &mut app, 20); // a queued pipeline would resolve within a few frames
    assert!(nothing_queued(&app), "the first click must not queue anything");
    let said = app.render_confirm_text().expect("a confirmation is shown");
    assert!(said.contains("complete Key result"), "{said}");
    press(&ctx, &mut app, "confirm_cancel");
    frames(&ctx, &mut app, 20);
    assert!(app.render_confirm_text().is_none());
    assert!(nothing_queued(&app), "cancel queues nothing");
    drop(app);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn only_the_explicit_full_workflow_choice_runs_the_pipeline() {
    let (ctx, mut app, base) = open("rc-run");
    press(&ctx, &mut app, "render");
    assert!(app.render_confirm_text().is_some() && nothing_queued(&app));
    press(&ctx, &mut app, "confirm_run");
    pump(&ctx, &mut app, "full workflow queued", |a| !a.ai().scheduler().snapshot().is_empty() || a.ai().pipeline_block_reason().is_some());
    assert!(app.render_confirm_text().is_none());
    let keys = app.ai().scheduler().snapshot().len();
    let kinds: Vec<TaskKind> = app.ai().scheduler().task_snapshot().iter().map(|t| t.job.kind).collect();
    assert_eq!((keys, kinds), (1, vec![TaskKind::ExportVideo, TaskKind::ExportVideo]), "Key runs again, then both encodes");
    drop(app);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn export_existing_is_encode_only_both_from_the_button_and_from_the_confirmation() {
    let (ctx, mut app, base) = open("rc-export");
    press(&ctx, &mut app, "export_existing");
    assert!(app.ai().scheduler().snapshot().is_empty(), "no Key job");
    let kinds: Vec<TaskKind> = app.ai().scheduler().task_snapshot().iter().map(|t| t.job.kind).collect();
    assert_eq!(kinds, [TaskKind::ExportVideo, TaskKind::ExportVideo], "matte MP4 and processed ProRes only");
    drop(app);
    let _ = fs::remove_dir_all(base);

    let (ctx, mut app, base) = open("rc-export2");
    press(&ctx, &mut app, "render");
    assert!(app.render_confirm_text().is_some());
    press(&ctx, &mut app, "confirm_export");
    assert!(app.render_confirm_text().is_none());
    assert!(app.ai().scheduler().snapshot().is_empty(), "the confirmation's export choice runs no Key either");
    assert_eq!(app.ai().scheduler().task_snapshot().len(), 2);
    drop(app);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn the_confirmation_closes_when_the_settings_it_was_opened_for_change() {
    let (ctx, mut app, base) = open("rc-guard");
    press(&ctx, &mut app, "render");
    assert!(app.render_confirm_text().is_some());
    // The captured workflow is frozen; a different Deliver choice is not silently run or silently adopted.
    app.set_export_for_test(&[("matte", video("mp4", "hevc", 23)), ("processed", video("mov_prores4444", "prores4444", 9))], None, false, None);
    frames(&ctx, &mut app, 4);
    assert!(app.render_confirm_text().is_none(), "closed, not executed");
    assert!(nothing_queued(&app));
    drop(app);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn a_shot_without_results_still_runs_directly() {
    let (ctx, mut app, base) = open("rc-fresh");
    let c = base.join("proj/clips/a");
    fs::write(c.join("clip.json"), "{}").unwrap();
    drop(app);
    let ctx2 = egui::Context::default();
    let mut app = App::with_ctx(&ctx2, AppOptions { project: Some(base.join("proj")), settings_path: Some(base.join("settings.json")), open_clip: Some("a".into()), tab: Some(Tab::Deliver), ..Default::default() });
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample

    pump(&ctx2, &mut app, "caps", |a| a.ai().local_caps().is_some());
    app.set_export_for_test(&[("matte", video("mp4", "h264", 18))], None, false, None);
    frames(&ctx2, &mut app, 12);
    press(&ctx2, &mut app, "render");
    assert!(app.render_confirm_text().is_none(), "nothing exists, so nothing to confirm");
    let _ = ctx;
    drop(app);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn pre_matte_only_and_hint_only_results_also_ask_before_a_full_run() {
    for (tag, have, says) in [("rc-pre", Have::PreMatteOnly, "3 pre-matte frame(s)"), ("rc-hint", Have::HintOnly, "3 hint (AlphaHint) frame(s)")] {
        let (ctx, mut app, base) = open_with(tag, have);
        press(&ctx, &mut app, "render");
        frames(&ctx, &mut app, 20);
        let text = app.render_confirm_text().unwrap_or_else(|| panic!("{tag}: no confirmation"));
        assert_eq!(text, says);
        assert!(nothing_queued(&app), "{tag}: the first click queues nothing");
        assert!(app.deliver_rects().iter().all(|r| r.0 != "export_existing"), "no duplicate export button while asking");
        drop(app);
        let _ = fs::remove_dir_all(base);
    }
}
