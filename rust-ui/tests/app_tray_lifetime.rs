//! A13: tray lifetime. Real `App::frame` + fake worker (stdlib Python, no GPU), a channel-fed tray instead of a desktop tray.
//! Close hides to the tray while the driver keeps working; Show restores; both Quit routes ask first and stop the endpoint, then the
//! workers; a failed tray leaves Close as a normal quit. Run through tools/isolated-test.sh, one test per command.
use ck::scheduler::JobState;
use ck::ui::tray::{Tray, TrayCmd};
use ck::ui::{App, AppOptions};
use ck::worker::Outcome;
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, RawInput, Rect, Vec2, ViewportCommand, ViewportEvent, ViewportId};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/ai_fake_worker.py");

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "ck-ai-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn write_png(p: &Path) {
    let f = fs::File::create(p).unwrap();
    let mut e = png::Encoder::new(f, 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .unwrap()
        .write_image_data(&[7; 48])
        .unwrap();
}

#[allow(dead_code)]
struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
    log: PathBuf,
}

fn env(tag: &str, clips: &[(&str, u32, Value)], project_json: Value) -> Env {
    let base = tmp(tag);
    let project = base.join("proj");
    for (name, n, extra) in clips {
        let c = project.join("clips").join(name);
        fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..*n {
            write_png(&c.join(format!("Frames/f_{}.png", 1101 + i)));
        }
        fs::write(
            c.join(".video_metadata.json"),
            r#"{"fps":24.0,"width":4,"height":4}"#,
        )
        .unwrap();
        fs::write(c.join("clip.json"), extra.to_string()).unwrap();
    }
    fs::write(project.join("project.json"), project_json.to_string()).unwrap();
    // A fake "repo": `python -m backend.worker` runs the fake worker (log path through the environment).
    let repo = base.join("repo");
    fs::create_dir_all(repo.join("backend/worker")).unwrap();
    fs::write(repo.join("backend/__init__.py"), "").unwrap();
    fs::write(repo.join("backend/worker/__init__.py"), "").unwrap();
    let log = base.join("worker.log");
    fs::write(
        repo.join("backend/worker/__main__.py"),
        format!("import os, runpy\nos.environ['AI_FAKE_LOG'] = {:?}\nrunpy.run_path({FAKE:?}, run_name='__main__')\n", log),
    )
    .unwrap();
    let settings = base.join("settings.json");
    fs::write(
        &settings,
        json!({"version": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": repo, "python": "/usr/bin/python3", "gpu": 0}]}).to_string(),
    )
    .unwrap();
    Env {
        base,
        project,
        settings,
        log,
    }
}

fn matte_png() -> Value {
    json!({"workflow": {"version": 1, "params": {}, "output_config": {"fg_enabled": false, "matte_enabled": true, "matte_format": "png", "comp_enabled": false, "processed_enabled": false}}})
}

fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(1440.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
}

fn settle(ctx: &egui::Context, app: &mut App) {
    for _ in 0..12 {
        run(ctx, app, vec![]);
    }
}

fn click_on(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
}

fn pump(ctx: &egui::Context, app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        run(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn confirm_batch(ctx: &egui::Context, app: &mut App) {
    // Automatic waits for a free PC with a live GPU sample; these tests run on the one local target.
    let session = std::mem::replace(&mut app.ai_mut().placement, ck::scheduler::Placement::Target("l0".into()));
    app.key_selected(ctx);
    pump(ctx, app, "batch review", |a| a.ai().batch_ready());
    settle(ctx, app);
    let r = app.ai().batch_confirm_rect.expect("Queue button drawn");
    click_on(ctx, app, r.center());
    app.ai_mut().placement = session;
}

fn states(app: &App) -> Vec<JobState> {
    app.ai()
        .scheduler()
        .snapshot()
        .into_iter()
        .map(|e| e.state)
        .collect()
}

fn all_terminal(app: &App) -> bool {
    let s = states(app);
    !s.is_empty() && s.iter().all(JobState::is_terminal)
}

fn complete(s: &JobState) -> bool {
    matches!(s, JobState::Done { info, .. } if info.outcome == Outcome::Complete)
}

fn app_for(e: &Env, ctx: &egui::Context) -> App {
    App::with_ctx(ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() })
}

/// One frame; `close` injects the window's close request. Returns the viewport commands the app sent.
fn frame(ctx: &egui::Context, app: &mut App, close: bool, events: Vec<Event>) -> Vec<ViewportCommand> {
    let mut raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 900.0))), events, ..Default::default() };
    if close {
        raw.viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
    }
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    out.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

fn running(a: &App) -> bool {
    states(a).iter().any(|s| matches!(s, JobState::Running { .. }))
}

fn with_job(tag: &str, clip: &str, frames: u32) -> (Env, egui::Context, App) {
    let e = env(tag, &[(clip, frames, matte_png())], json!({"version": 2}));
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx);
    app.select_folders(&[clip]);
    (e, ctx, app)
}

#[test]
fn close_hides_to_the_tray_while_the_driver_finishes_the_job_and_show_restores() {
    let (e, ctx, mut app) = with_job("hide", "ok1", 2);
    let (tray, tx) = Tray::channel();
    app.set_tray(Some(tray));
    fs::write(e.base.join("tray-note-shown"), "").unwrap(); // the first-time note was already seen
    app.start_driver(&ctx);
    confirm_batch(&ctx, &mut app);
    let cmds = frame(&ctx, &mut app, true, vec![]);
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::CancelClose)), "{cmds:?}");
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(false))), "{cmds:?}");
    // Hidden: no frame is drawn at all. The driver thread alone must finish the job.
    app.release_shared();
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        let done = all_terminal(&app);
        app.release_shared();
        if done {
            break;
        }
        assert!(Instant::now() < end, "the job did not finish while the window was hidden");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(states(&app).iter().all(complete), "{:?}", states(&app));
    app.release_shared();
    // Show: the command goes out and the next frame draws the finished result.
    tx.send(TrayCmd::Show).unwrap();
    app.tray_tick(&ctx);
    let cmds = frame(&ctx, &mut app, false, vec![]);
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(true))), "{cmds:?}");
    for _ in 0..5 {
        frame(&ctx, &mut app, false, vec![]);
    }
    assert!(states(&app).iter().all(complete));
}

#[test]
fn first_close_shows_the_tray_note_before_hiding() {
    let (e, ctx, mut app) = with_job("note", "ok1", 1);
    let (tray, _tx) = Tray::channel();
    app.set_tray(Some(tray));
    let cmds = frame(&ctx, &mut app, true, vec![]);
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::CancelClose)), "{cmds:?}");
    assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(false))), "hid before the note: {cmds:?}");
    assert!(app.tray_note_open());
    assert!(!e.base.join("tray-note-shown").exists());
}

/// Quit while a job runs: the question appears; "Stop work, then quit" stops the endpoint, then the workers, then the window closes.
fn quit_route(tag: &str, via_tray: bool) {
    let (_e, ctx, mut app) = with_job(tag, "slow1", 5);
    let (tray, tx) = Tray::channel();
    app.set_tray(Some(tray));
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", running);
    if via_tray {
        tx.send(TrayCmd::Quit).unwrap();
        app.tray_tick(&ctx);
        let cmds = frame(&ctx, &mut app, false, vec![]);
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(true))), "the question needs a visible window: {cmds:?}");
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Close)), "{cmds:?}");
    } else {
        let key = Event::Key { key: Key::Q, physical_key: Some(Key::Q), pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        let cmds = frame(&ctx, &mut app, false, vec![key]);
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Close)), "{cmds:?}");
    }
    assert!(app.shutdown_steps().is_empty(), "nothing stops before the user answers");
    // The window turns Close into a close request: the app cancels it and asks, even though a tray exists.
    let cmds = frame(&ctx, &mut app, true, vec![]);
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::CancelClose)), "{cmds:?}");
    assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(false))), "quit must not hide: {cmds:?}");
    assert_eq!(app.close_state(), ck::ui::CloseState::Prompt);
    for _ in 0..3 {
        frame(&ctx, &mut app, false, vec![]);
    }
    let [stop, _keep, _quit] = app.close_rects()[..] else { panic!("three buttons: {:?}", app.close_rects()) };
    click_on(&ctx, &mut app, stop.center());
    assert_eq!(app.close_state(), ck::ui::CloseState::Stopping);
    assert_eq!(app.shutdown_steps(), ["endpoint", "workers"]);
    let end = Instant::now() + Duration::from_secs(20);
    loop {
        if frame(&ctx, &mut app, false, vec![]).iter().any(|c| matches!(c, ViewportCommand::Close)) {
            break;
        }
        assert!(Instant::now() < end, "never closed: {:?}", app.ai().stopping_lines());
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[test]
fn tray_quit_asks_then_stops_endpoint_then_workers() {
    quit_route("quit-tray", true);
}

#[test]
fn file_quit_asks_then_stops_endpoint_then_workers() {
    quit_route("quit-file", false);
}

#[test]
fn without_a_tray_close_quits_normally_after_the_same_question() {
    let (_e, ctx, mut app) = with_job("notray", "slow1", 5);
    assert!(!app.has_tray());
    // A job running: the same question as Quit, never a hidden window.
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", running);
    let cmds = frame(&ctx, &mut app, true, vec![]);
    assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::CancelClose)), "{cmds:?}");
    assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(false))), "{cmds:?}");
    assert_eq!(app.close_state(), ck::ui::CloseState::Prompt);
}

#[test]
fn a_tray_that_goes_away_brings_the_window_back_and_close_quits() {
    let (_e, ctx, mut app) = with_job("lost", "ok1", 1);
    let (tray, tx) = Tray::channel();
    app.set_tray(Some(tray));
    tx.send(TrayCmd::Lost).unwrap();
    app.tray_tick(&ctx);
    assert!(!app.has_tray());
    let cmds = frame(&ctx, &mut app, true, vec![]);
    assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Visible(false))), "{cmds:?}");
}
