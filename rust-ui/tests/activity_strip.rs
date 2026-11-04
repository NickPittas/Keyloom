//! The clip view's one activity strip: every stage reaches it, it is the only progress host, errors stay, it
//! survives tab/focus/collapse switches and fits 900x640 and 2400x1400. Headless egui only.
mod common;
use ck::contracts::RangeIdentity;
use ck::jobs::Require;
use ck::tasks::{TaskJob, TaskKind};
use ck::ui::activity::{Cancel, Level, Step, STRIP_H};
use ck::ui::{App, AppOptions, Tab};
use common::*;
use eframe::egui::{self, epaint::ClippedShape, Event, Key, Modifiers, RawInput, Rect, Shape, Vec2};
use serde_json::json;

fn open_app(tag: &str, tab: Tab) -> (egui::Context, App) {
    let base = tmp(tag);
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..4u8 {
        let f = std::fs::File::create(clip.join(format!("Frames/f_{i}.png"))).unwrap();
        let mut e = png::Encoder::new(f, 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[i; 48]).unwrap();
    }
    std::fs::write(clip.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"local-0","kind":"local","repo":repo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions { project: Some(proj), settings_path: Some(settings), open_clip: Some("c1".into()), tab: Some(tab), ..Default::default() },
    );
    for _ in 0..12 {
        frame(&ctx, &mut app, Vec2::new(1440.0, 900.0), vec![]);
    }
    (ctx, app)
}

fn frame(ctx: &egui::Context, app: &mut App, size: Vec2, events: Vec<Event>) -> Vec<ClippedShape> {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)), events, ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    out.shapes
}

/// (text, painted bounds, clip) of every text painted.
fn texts(shapes: &[ClippedShape]) -> Vec<(String, Rect, Rect)> {
    fn flat(s: &Shape, clip: Rect, out: &mut Vec<(String, Rect, Rect)>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| flat(x, clip, out)),
            Shape::Text(t) => out.push((t.galley.job.text.clone(), t.galley.rect.translate(t.pos.to_vec2()), clip)),
            _ => {}
        }
    }
    let mut out = vec![];
    for c in shapes {
        flat(&c.shape, c.clip_rect, &mut out);
    }
    out
}

fn all_text(shapes: &[ClippedShape]) -> String {
    texts(shapes).into_iter().map(|t| t.0).collect::<Vec<_>>().join("\n")
}

fn settle(ctx: &egui::Context, app: &mut App, size: Vec2) -> Vec<ClippedShape> {
    let mut last = vec![];
    for _ in 0..6 {
        last = frame(ctx, app, size, vec![]);
    }
    last
}

fn key(k: Key) -> Vec<Event> {
    vec![Event::Key { key: k, physical_key: Some(k), pressed: true, repeat: false, modifiers: Modifiers::NONE }]
}

fn step(id: &str, stage: &str, level: Level) -> Step {
    Step::new(id, stage, level)
}

fn task(root: &std::path::Path, id: &str, kind: TaskKind, fake: &str) -> TaskJob {
    let root = root.to_path_buf();
    TaskJob {
        job_id: id.into(),
        kind,
        clip_id: "c1".into(),
        clip_root: root.clone(),
        range: kind.ranged().then(|| RangeIdentity { in_point: 0, out_point: 3, in_stem: "f_0".into(), out_stem: "f_3".into() }),
        params: json!({"fake": fake, "dest": root.join("out.mp4"), "tracker_model": "base_plus", "variant": "Matting"}),
        staging_dir: kind.needs_staging().then(|| std::env::temp_dir().join(format!("ck-act-stg-{}", std::process::id())).join(id)),
        publish_dir: kind.writes_hint().then(|| root.join(if kind == TaskKind::SamTrack { "VideoMamaMaskHint" } else { "AlphaHint" })),
        require: Require { protocol: 1, models: Default::default() },
    }
}

#[test]
fn every_scheduler_stage_and_the_viewer_and_import_sources_reach_the_strip() {
    let (ctx, mut app) = open_app("act-stages", Tab::Hint);
    let clip = app.clip_for_test();
    let mut prev: Option<String> = None;
    let kinds = [TaskKind::SamTrack, TaskKind::Matanyone2, TaskKind::Videomama, TaskKind::Chroma, TaskKind::SamPreview, TaskKind::ExportVideo, TaskKind::PrepareSource];
    for (n, k) in kinds.into_iter().enumerate() {
        let after = prev.iter().cloned().collect();
        let id = format!("s{n}");
        let fake = if n == 0 { "hold" } else { "ok" };
        app.ai_mut().submit_task(&clip, task(&clip.root, &id, k, fake), after, 0).unwrap_or_else(|e| panic!("{k:?}: {e}"));
        prev = Some(id);
    }
    // The viewer and import sources have no worker here; they use the same injection point as their real feeders.
    app.activity_inject_for_test(vec![
        step("import", "Extract frames", Level::Active),
        step("ram", "RAM preview", Level::Active),
        step("decode", "Decoding", Level::Error),
        step("key", "Key", Level::Active),
    ]);
    app.activity_open_for_test(true);
    let shapes = settle(&ctx, &mut app, Vec2::new(1440.0, 900.0));
    let t = all_text(&shapes);
    for k in kinds {
        assert!(t.contains(k.label()), "{} missing from the strip:\n{t}", k.label());
    }
    for s in ["Extract frames", "RAM preview", "Decoding", "Key"] {
        assert!(t.contains(s), "{s} missing:\n{t}");
    }
}

#[test]
fn the_strip_is_the_only_progress_host_and_shows_this_computer_not_the_target_id() {
    let (ctx, mut app) = open_app("act-one", Tab::Key);
    let mut s = step("j1", "Key", Level::Active);
    (s.pc, s.phase, s.done, s.total) = ("This computer".into(), "stage 1, windows".into(), 20, 30);
    app.activity_inject_for_test(vec![s]);
    let shapes = settle(&ctx, &mut app, Vec2::new(1440.0, 900.0));
    let found: Vec<_> = texts(&shapes).into_iter().filter(|t| t.0.contains("20/30")).collect();
    assert_eq!(found.len(), 1, "progress must be painted by exactly one host: {found:?}");
    let strip = app.activity_rect().unwrap();
    assert!(strip.contains_rect(found[0].1), "the one progress host is outside the strip: {:?} vs {strip:?}", found[0].1);
    let t = all_text(&shapes);
    assert!(t.contains("This computer"), "{t}");
    assert!(!t.contains("local-0"), "raw target id leaked:\n{t}");
    assert!(!t.contains("Processing this clip"), "the old Keying progress box is still drawn");
}

#[test]
fn unknown_total_shows_activity_without_a_percentage() {
    let (ctx, mut app) = open_app("act-unknown", Tab::Media);
    let mut s = step("import", "Extract frames", Level::Active);
    s.pc = "This computer".into();
    app.activity_inject_for_test(vec![s]);
    let t = all_text(&settle(&ctx, &mut app, Vec2::new(1440.0, 900.0)));
    assert!(t.contains("working"), "{t}");
    assert!(!t.contains('%') && !t.contains("0/0"), "{t}");
}

#[test]
fn a_failed_key_stays_after_a_successful_preview() {
    let (ctx, mut app) = open_app("act-error", Tab::Key);
    let mut key = step("k1", "Key", Level::Error);
    key.message = "out of memory on the GPU".into();
    let mut prev = step("p1", "SAM preview", Level::Done);
    (prev.preview, prev.message) = (true, "Complete: 0 written".into());
    app.activity_inject_for_test(vec![key, prev]);
    let t = all_text(&settle(&ctx, &mut app, Vec2::new(1440.0, 900.0)));
    assert!(t.contains("out of memory on the GPU"), "error was replaced by the later preview:\n{t}");
    // The next action on the clip (any non-preview step) clears it.
    let mut again = step("k2", "Key", Level::Queued);
    again.pc = "This computer".into();
    let mut old = step("k1", "Key", Level::Error);
    old.message = "out of memory on the GPU".into();
    app.activity_inject_for_test(vec![old, again]);
    let t = all_text(&settle(&ctx, &mut app, Vec2::new(1440.0, 900.0)));
    assert!(!t.contains("out of memory on the GPU"), "the error outlived the next action:\n{t}");
}

#[test]
fn tab_focus_and_graph_collapse_switches_keep_the_strip_in_the_same_place() {
    let (ctx, mut app) = open_app("act-switch", Tab::Media);
    let size = Vec2::new(1440.0, 900.0);
    let mut s = step("j1", "Key", Level::Active);
    (s.pc, s.done, s.total) = ("This computer".into(), 3, 10);
    app.activity_inject_for_test(vec![s]);
    settle(&ctx, &mut app, size);
    let base = app.activity_rect().unwrap();
    for tab in [Tab::Media, Tab::Hint, Tab::Key, Tab::Deliver] {
        app.switch_tab(tab);
        let t = all_text(&settle(&ctx, &mut app, size));
        assert!(t.contains("3/10"), "{tab:?}: strip lost its progress:\n{t}");
        assert_eq!(app.activity_rect().unwrap(), base, "{tab:?}: strip moved");
    }
    frame(&ctx, &mut app, size, key(Key::F));
    assert!(app.focus_mode());
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(t.contains("3/10"), "Focus hid the strip");
    assert_eq!(app.activity_rect().unwrap(), base, "Focus moved the strip");
    frame(&ctx, &mut app, size, key(Key::F));
    frame(&ctx, &mut app, size, key(Key::G));
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(t.contains("3/10"), "collapsed graph hid the strip");
    assert_eq!(app.activity_rect().unwrap(), base, "collapsing the graph moved the strip");
}

#[test]
fn it_fits_small_and_large_windows_with_nothing_cut_off() {
    for (w, h) in [(900.0, 640.0), (2400.0, 1400.0)] {
        let (ctx, mut app) = open_app(&format!("act-fit-{w}"), Tab::Key);
        let size = Vec2::new(w, h);
        let mut s = step("j1", "Key", Level::Active);
        (s.pc, s.phase, s.done, s.total) = ("This computer".into(), "stage 1, windows".into(), 20, 30);
        s.cancel = Cancel::Job("x".into());
        s.message = "writing frames to a very long destination path that has to be shortened".repeat(2);
        app.activity_inject_for_test(vec![s]);
        let shapes = settle(&ctx, &mut app, size);
        let strip = app.activity_rect().unwrap();
        assert!((strip.height() - STRIP_H).abs() < 3.0, "{w}x{h}: strip height {}", strip.height());
        assert!(strip.min.x >= 0.0 && strip.max.x <= w + 0.5 && strip.max.y <= h, "{w}x{h}: {strip:?}");
        for (text, r, clip) in texts(&shapes) {
            // Only what the strip panel itself paints (the viewer above is clipped to its own panel).
            if clip.min.y >= strip.min.y - 1.0 && r.center().y < strip.max.y {
                assert!(strip.expand(1.0).contains_rect(r), "{w}x{h}: text {text:?} {r:?} leaves the strip {strip:?}");
            }
        }
        let t = all_text(&shapes);
        for need in ["Details", "queued", "Cancel", "20/30"] {
            assert!(t.contains(need), "{w}x{h}: {need} missing from the strip:\n{t}");
        }
    }
}

#[test]
fn cancel_on_the_strip_cancels_the_running_job() {
    let (ctx, mut app) = open_app("act-cancel", Tab::Hint);
    let clip = app.clip_for_test();
    app.ai_mut().submit_task(&clip, task(&clip.root, "c0", TaskKind::SamTrack, "hold"), vec![], 0).unwrap();
    let size = Vec2::new(1440.0, 900.0);
    let mut cancel_at = None;
    for _ in 0..400 {
        let shapes = frame(&ctx, &mut app, size, vec![]);
        let strip = app.activity_rect().unwrap();
        if let Some((_, r, _)) = texts(&shapes).into_iter().find(|t| t.0 == "Cancel" && strip.contains_rect(t.1)) {
            cancel_at = Some(r.center());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let at = cancel_at.expect("no Cancel button on the strip");
    let b = |pressed| Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    frame(&ctx, &mut app, size, vec![Event::PointerMoved(at)]);
    frame(&ctx, &mut app, size, vec![b(true)]);
    frame(&ctx, &mut app, size, vec![b(false)]);
    let mut t = String::new();
    for _ in 0..400 {
        t = all_text(&frame(&ctx, &mut app, size, vec![]));
        if t.contains("Cancelled") || t.contains("Cancel requested") {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    panic!("Cancel from the strip did not reach the job:\n{t}");
}
