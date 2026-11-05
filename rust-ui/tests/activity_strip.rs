//! The Activity popover (opened from the status bar) lists every stage that reaches the clip: scheduler jobs,
//! tasks, import, viewer and notes. It is the only progress host and it survives tab/focus/graph switches.
//! Headless egui only; the popover is checked through the text it paints, with the popover open.
mod common;
use ck::contracts::RangeIdentity;
use ck::jobs::Require;
use ck::tasks::{TaskJob, TaskKind};
use ck::ui::activity::{Level, Step};
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

/// The texts `shapes` paints beyond `base` (each base text cancels one identical text): what the open popover adds.
fn fresh(base: &[String], shapes: &[ClippedShape]) -> Vec<(String, Rect, Rect)> {
    let mut left = base.to_vec();
    texts(shapes)
        .into_iter()
        .filter(|t| match left.iter().position(|b| *b == t.0) {
            Some(i) => {
                left.swap_remove(i);
                false
            }
            None => true,
        })
        .collect()
}

/// Texts the Activity popover paints on its own: the popover is closed, then opened, and the difference is kept.
fn popover_only(ctx: &egui::Context, app: &mut App, size: Vec2) -> Vec<(String, Rect, Rect)> {
    app.activity_open_for_test(false);
    let base: Vec<String> = texts(&settle(ctx, app, size)).into_iter().map(|t| t.0).collect();
    app.activity_open_for_test(true);
    let shapes = settle(ctx, app, size);
    fresh(&base, &shapes)
}

/// Every text the open popover paints while its list scrolls from top to bottom (rows past 260 px are only seen this way).
fn popover_text_scrolled(ctx: &egui::Context, app: &mut App, size: Vec2) -> String {
    let strip = app.activity_rect().expect("the status strip is laid out");
    let at = egui::Pos2::new(strip.left() + 20.0, strip.top() - 40.0);
    let mut seen = all_text(&settle(ctx, app, size));
    for _ in 0..40 {
        let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -40.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown };
        for n in 0..3 {
            let events = if n == 0 { vec![Event::PointerMoved(at), wheel.clone()] } else { vec![Event::PointerMoved(at)] };
            seen.push('\n');
            seen.push_str(&all_text(&frame(ctx, app, size, events)));
        }
    }
    seen
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
fn every_scheduler_stage_and_the_viewer_and_import_sources_reach_the_popover() {
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
    // The list is taller than the 260 px popover, so every row is collected while it scrolls.
    let t = popover_text_scrolled(&ctx, &mut app, Vec2::new(1440.0, 900.0));
    for k in kinds {
        assert!(t.contains(k.label()), "{} missing from the popover:\n{t}", k.label());
    }
    for s in ["Extract frames", "RAM preview", "Decoding", "Key"] {
        assert!(t.contains(s), "{s} missing:\n{t}");
    }
}

#[test]
fn the_popover_is_the_only_progress_host_and_shows_this_computer_not_the_target_id() {
    let (ctx, mut app) = open_app("act-one", Tab::Key);
    let size = Vec2::new(1440.0, 900.0);
    let mut s = step("j1", "Key", Level::Active);
    (s.pc, s.phase, s.done, s.total) = ("This computer".into(), "stage 1, windows".into(), 20, 30);
    app.activity_inject_for_test(vec![s]);
    let added = popover_only(&ctx, &mut app, size);
    let found: Vec<_> = added.iter().filter(|t| t.0.contains("20/30")).collect();
    assert_eq!(found.len(), 1, "the popover must paint the progress exactly once: {added:?}");
    let shapes = settle(&ctx, &mut app, size);
    let n = texts(&shapes).iter().filter(|t| t.0.contains("20/30")).count();
    assert_eq!(n, 1, "progress painted {n} times on the page");
    let t = all_text(&shapes);
    assert!(t.contains("This computer"), "{t}");
    assert!(!t.contains("local-0"), "raw target id leaked:\n{t}");
    assert!(!t.contains("Processing this clip"), "the old Keying progress box is still drawn");
}

#[test]
fn unknown_total_shows_the_step_without_a_percentage() {
    let (ctx, mut app) = open_app("act-unknown", Tab::Media);
    let size = Vec2::new(1440.0, 900.0);
    let mut s = step("import", "Extract frames", Level::Active);
    s.pc = "This computer".into();
    app.activity_inject_for_test(vec![s]);
    let added: Vec<String> = popover_only(&ctx, &mut app, size).into_iter().map(|t| t.0).collect();
    assert!(added.iter().any(|t| t.contains("Extract frames")), "the step is not listed: {added:?}");
    assert!(added.iter().all(|t| !t.contains('%') && !t.contains("0/0")), "a made-up percentage: {added:?}");
}

#[test]
fn a_failed_key_stays_after_a_successful_preview() {
    let (ctx, mut app) = open_app("act-error", Tab::Key);
    let size = Vec2::new(1440.0, 900.0);
    let mut key = step("k1", "Key", Level::Error);
    key.message = "out of memory on the GPU".into();
    let mut prev = step("p1", "SAM preview", Level::Done);
    (prev.preview, prev.message) = (true, "Complete: 0 written".into());
    app.activity_inject_for_test(vec![key, prev]);
    app.activity_open_for_test(true);
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(t.contains("out of memory on the GPU"), "error was replaced by the later preview:\n{t}");
    // The next action on the clip (any non-preview step) clears it.
    let mut again = step("k2", "Key", Level::Queued);
    again.pc = "This computer".into();
    let mut old = step("k1", "Key", Level::Error);
    old.message = "out of memory on the GPU".into();
    app.activity_inject_for_test(vec![old, again]);
    // Line 2 paints the standing error as "stage: message"; the history row ("Key · …") may still list it.
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(!t.contains("Key: out of memory on the GPU"), "the error outlived the next action:\n{t}");
}

#[test]
fn progress_stays_in_the_popover_across_tab_focus_and_graph_collapse_switches() {
    let (ctx, mut app) = open_app("act-switch", Tab::Media);
    let size = Vec2::new(1440.0, 900.0);
    let mut s = step("j1", "Key", Level::Active);
    (s.pc, s.done, s.total) = ("This computer".into(), 3, 10);
    app.activity_inject_for_test(vec![s]);
    app.activity_open_for_test(true);
    settle(&ctx, &mut app, size);
    for tab in [Tab::Media, Tab::Hint, Tab::Key, Tab::Deliver] {
        app.switch_tab(tab);
        let t = all_text(&settle(&ctx, &mut app, size));
        assert!(t.contains("3/10"), "{tab:?}: the popover lost its progress:\n{t}");
    }
    frame(&ctx, &mut app, size, key(Key::F));
    assert!(app.focus_mode());
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(t.contains("3/10"), "Focus hid the popover's progress");
    frame(&ctx, &mut app, size, key(Key::F));
    frame(&ctx, &mut app, size, key(Key::G));
    let t = all_text(&settle(&ctx, &mut app, size));
    assert!(t.contains("3/10"), "collapsed graph hid the popover's progress");
}

#[test]
fn the_popover_fits_small_and_large_windows_with_nothing_outside_the_window() {
    for (w, h) in [(900.0, 640.0), (2400.0, 1400.0)] {
        let (ctx, mut app) = open_app(&format!("act-fit-{w}"), Tab::Key);
        let size = Vec2::new(w, h);
        let mut s = step("j1", "Key", Level::Active);
        (s.pc, s.phase, s.done, s.total) = ("This computer".into(), "stage 1, windows".into(), 20, 30);
        s.message = "writing frames to a very long destination path that has to be shortened".repeat(2);
        app.activity_inject_for_test(vec![s]);
        let added = popover_only(&ctx, &mut app, size);
        let window = Rect::from_min_size(egui::Pos2::ZERO, size);
        for (text, r, _) in &added {
            assert!(window.expand(0.5).contains_rect(*r), "{w}x{h}: text {text:?} {r:?} is outside the window");
        }
        let names: Vec<&str> = added.iter().map(|t| t.0.as_str()).collect();
        for need in ["This computer", "20/30"] {
            assert!(names.iter().any(|t| t.contains(need)), "{w}x{h}: {need} missing from the popover: {names:?}");
        }
    }
}

#[test]
fn cancel_in_the_popover_cancels_the_running_job() {
    let (ctx, mut app) = open_app("act-cancel", Tab::Hint);
    let clip = app.clip_for_test();
    app.ai_mut().submit_task(&clip, task(&clip.root, "c0", TaskKind::SamTrack, "hold"), vec![], 0).unwrap();
    let size = Vec2::new(1440.0, 900.0);
    // Closed-popover text is the baseline; the open popover's extra "Cancel" is the job row's button.
    app.activity_open_for_test(false);
    let base: Vec<String> = texts(&settle(&ctx, &mut app, size)).into_iter().map(|t| t.0).collect();
    app.activity_open_for_test(true);
    let mut cancel_at = None;
    for _ in 0..400 {
        let shapes = frame(&ctx, &mut app, size, vec![]);
        if let Some((_, r, _)) = fresh(&base, &shapes).into_iter().find(|t| t.0 == "Cancel") {
            cancel_at = Some(r.center());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let at = cancel_at.expect("no Cancel button in the Activity popover");
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
    panic!("Cancel from the popover did not reach the job:\n{t}");
}
