mod common;

use ck::scheduler::{Placement, Submit};
use ck::ui::tray::{Tray, TrayCmd};
use ck::ui::{App, AppOptions, Tab};
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2, ViewportCommand, ViewportEvent, ViewportId};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug)]
struct Pass {
    commands: Vec<ViewportCommand>,
    texts: Vec<(String, Rect)>,
}

fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let p = std::env::temp_dir().join(format!("ck-close-{}-{tag}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&p).unwrap();
    p
}

fn write_png(path: &Path) {
    let mut encoder = png::Encoder::new(fs::File::create(path).unwrap(), 2, 2);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.write_header().unwrap().write_image_data(&[7; 12]).unwrap();
}

fn project(base: &Path) -> PathBuf {
    let root = base.join("project");
    let clip = root.join("clips/shot_01");
    fs::create_dir_all(clip.join("Frames")).unwrap();
    write_png(&clip.join("Frames/f_1101.png"));
    write_png(&clip.join("Frames/f_1102.png"));
    fs::write(clip.join(".video_metadata.json"), r#"{"fps":24.0,"width":2,"height":2}"#).unwrap();
    fs::write(clip.join("clip.json"), "{}").unwrap();
    fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    root
}

fn settings(base: &Path, background: bool) -> PathBuf {
    let p = base.join("settings.json");
    fs::write(&p, json!({"version": 1, "interface": {"background": background}}).to_string()).unwrap();
    p
}

fn raw(close: bool, events: Vec<Event>) -> RawInput {
    let mut input = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        events,
        ..Default::default()
    };
    if close {
        input.viewports.entry(ViewportId::ROOT).or_default().events.push(ViewportEvent::Close);
    }
    input
}

fn walk(shape: &egui::epaint::Shape, texts: &mut Vec<(String, Rect)>) {
    match shape {
        egui::epaint::Shape::Text(t) => texts.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
        egui::epaint::Shape::Vec(shapes) => shapes.iter().for_each(|shape| walk(shape, texts)),
        _ => {}
    }
}

fn frame(ctx: &egui::Context, app: &mut App, close: bool, events: Vec<Event>) -> Pass {
    let mut native_frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(raw(close, events), |ui| {
        eframe::App::logic(app, ui.ctx(), &mut native_frame);
        app.frame(ui);
    });
    let commands = output.viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default();
    let mut texts = vec![];
    for shape in &output.shapes {
        walk(&shape.shape, &mut texts);
    }
    output.textures_delta.clear();
    Pass { commands, texts }
}

fn logic_only(ctx: &egui::Context, app: &mut App, close: bool) -> Vec<ViewportCommand> {
    let mut native_frame = eframe::Frame::_new_kittest();
    ctx.run_logic(&raw(close, vec![]), |ctx| eframe::App::logic(app, ctx, &mut native_frame))
        .viewport_commands.get(&ViewportId::ROOT).cloned().unwrap_or_default()
}

fn has(commands: &[ViewportCommand], wanted: impl Fn(&ViewportCommand) -> bool) -> bool {
    commands.iter().any(wanted)
}

fn finish_close(ctx: &egui::Context, app: &mut App, mut last: Pass) -> Pass {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if has(&last.commands, |c| matches!(c, ViewportCommand::Close)) {
            last = frame(ctx, app, true, vec![]);
            // egui applies commands in order: the close is accepted unless the last close command of the pass is CancelClose.
            let verdict = last.commands.iter().rev().find(|c| matches!(c, ViewportCommand::Close | ViewportCommand::CancelClose));
            if !matches!(verdict, Some(ViewportCommand::CancelClose)) {
                last.commands.retain(|c| !matches!(c, ViewportCommand::CancelClose)); // superseded by the later Close
                return last;
            }
        }
        if Instant::now() >= deadline {
            panic!("no accepted close before deadline: commands={:?}, texts={:?}", last.commands, last.texts);
        }
        std::thread::sleep(Duration::from_millis(20));
        last = frame(ctx, app, false, vec![]);
    }
}

fn click(ctx: &egui::Context, app: &mut App, at: Pos2) -> Pass {
    let button = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    frame(ctx, app, false, vec![Event::PointerMoved(at)]);
    frame(ctx, app, false, vec![button(true)]);
    let mut pass = frame(ctx, app, false, vec![button(false)]);
    for _ in 0..3 {
        let settled = frame(ctx, app, false, vec![]);
        pass.commands.extend(settled.commands);
        pass.texts = settled.texts;
    }
    pass
}

fn ctrl_q() -> Event {
    Event::Key { key: Key::Q, physical_key: Some(Key::Q), pressed: true, repeat: false, modifiers: Modifiers::COMMAND }
}

#[test]
fn normal_quit_accepts_a_fresh_authorized_close() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions::default());
    let quit = frame(&ctx, &mut app, false, vec![ctrl_q()]);
    let accepted = finish_close(&ctx, &mut app, quit);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
}

#[test]
fn tray_quit_uses_the_normal_flow_and_accepts_the_final_close() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions::default());
    let (tray, tx) = Tray::channel();
    app.set_tray(Some(tray));
    tx.send(TrayCmd::Quit).unwrap();
    let quit = frame(&ctx, &mut app, false, vec![]);
    assert!(has(&quit.commands, |c| matches!(c, ViewportCommand::Visible(true))), "{quit:?}");
    let accepted = finish_close(&ctx, &mut app, quit);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
}

#[test]
fn x_hides_only_with_saved_background_preference_and_a_tray() {
    let base = tmp("tray-hide");
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { settings_path: Some(settings(&base, true)), ..Default::default() });
    let (tray, _) = Tray::channel();
    app.set_tray(Some(tray));
    fs::write(base.join("tray-note-shown"), "").unwrap();
    let close = frame(&ctx, &mut app, true, vec![]);
    assert!(has(&close.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{close:?}");
    assert!(has(&close.commands, |c| matches!(c, ViewportCommand::Visible(false))), "{close:?}");
    assert!(!has(&close.commands, |c| matches!(c, ViewportCommand::Close)), "{close:?}");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn x_with_a_tray_but_background_disabled_quits() {
    let base = tmp("tray-no-background");
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { settings_path: Some(settings(&base, false)), ..Default::default() });
    let (tray, _) = Tray::channel();
    app.set_tray(Some(tray));
    let close = frame(&ctx, &mut app, true, vec![]);
    assert!(!has(&close.commands, |c| matches!(c, ViewportCommand::Visible(false))), "{close:?}");
    let accepted = finish_close(&ctx, &mut app, close);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn x_without_a_tray_accepts_the_authorized_close() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions::default());
    let close = frame(&ctx, &mut app, true, vec![]);
    let accepted = finish_close(&ctx, &mut app, close);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
}

#[test]
fn minimized_os_close_is_retained_until_the_next_rendered_frame() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions::default());
    let logic_commands = logic_only(&ctx, &mut app, true);
    assert!(has(&logic_commands, |c| matches!(c, ViewportCommand::CancelClose)), "{logic_commands:?}");
    assert!(has(&logic_commands, |c| matches!(c, ViewportCommand::Visible(true))), "{logic_commands:?}");
    let _ = logic_only(&ctx, &mut app, false);
    let rendered = frame(&ctx, &mut app, false, vec![]);
    let accepted = finish_close(&ctx, &mut app, rendered);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
}

#[test]
fn close_during_delete_dialog_stays_open() {
    let base = tmp("delete");
    let root = project(&base);
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions {
        project: Some(root),
        open_clip: Some("shot_01".into()),
        tab: Some(Tab::Hint),
        ..Default::default()
    });
    let first = frame(&ctx, &mut app, false, vec![]);
    let button = first.texts.iter().find(|(text, _)| text == "Delete frames…").map(|(_, rect)| rect.center()).expect("Hint page delete button");
    let opened = click(&ctx, &mut app, button);
    assert!(opened.texts.iter().any(|(text, _)| text.contains("Delete generated frames")), "delete dialog did not open: {:?}", opened.texts);
    let close = frame(&ctx, &mut app, true, vec![]);
    assert!(has(&close.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{close:?}");
    assert!(!has(&close.commands, |c| matches!(c, ViewportCommand::Close | ViewportCommand::Visible(false))), "{close:?}");
    let _ = fs::remove_dir_all(base);
}

#[test]
fn active_job_prompts_and_quit_anyway_accepts_the_final_close() {
    let base = common::tmp("close-anyway");
    let repo = common::make_repo(&base, "repo", json!({"modes": {"key": "hold"}}));
    let settings_path = base.join("settings.json");
    fs::write(&settings_path, json!({"version": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": repo, "python": common::PY, "gpu": 0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { repo: Some(repo.clone()), settings_path: Some(settings_path), ..Default::default() });
    app.ai_mut().set_models(ck::models::Snapshot::for_tests());
    app.ai_mut().start_workers();
    app.ai_mut().scheduler_mut().submit(Submit {
        job: common::job(&base, "closejob", 2, "hold"),
        placement: Placement::Target("l0".into()),
        after: vec![],
        min_vram_gb: None,
    }).unwrap();
    let end = Instant::now() + Duration::from_secs(20);
    while !app.ai().running_jobs() {
        assert!(Instant::now() < end, "fake worker did not start");
        frame(&ctx, &mut app, false, vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    let close = frame(&ctx, &mut app, true, vec![]);
    assert!(has(&close.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{close:?}");
    assert_eq!(app.close_state(), ck::ui::CloseState::Prompt);
    let prompt = frame(&ctx, &mut app, false, vec![]);
    let quit_anyway = prompt.texts.iter().find(|(text, _)| text == "Quit anyway").expect("Quit anyway button").1.center();
    let requested = click(&ctx, &mut app, quit_anyway);
    let accepted = finish_close(&ctx, &mut app, requested);
    assert!(!has(&accepted.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{accepted:?}");
    app.ai_mut().begin_stop();
    let end = Instant::now() + Duration::from_secs(20);
    while !app.ai().all_stopped() {
        assert!(Instant::now() < end, "fake worker did not stop");
        frame(&ctx, &mut app, false, vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    let _ = fs::remove_dir_all(base);
}

#[test]
fn close_save_failure_vetoes_os_close_and_shows_the_error() {
    let base = tmp("save-failure");
    let root = project(&base);
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(root.clone()), open_clip: Some("shot_01".into()), ..Default::default() });
    app.viewer_mut_for_test(|viewer| viewer.range = Some((0, 1)));
    let clip_file = root.join("clips/shot_01/clip.json");
    fs::remove_file(&clip_file).unwrap();
    fs::create_dir(&clip_file).unwrap();
    let close = frame(&ctx, &mut app, true, vec![]);
    assert!(has(&close.commands, |c| matches!(c, ViewportCommand::CancelClose)), "{close:?}");
    assert!(!has(&close.commands, |c| matches!(c, ViewportCommand::Close | ViewportCommand::Visible(false))), "{close:?}");
    assert!(app.ai().notice.as_deref().is_some_and(|notice| notice.starts_with("Not closing:")), "{:?}", app.ai().notice);
    let _ = fs::remove_dir_all(base);
}

#[test]
fn background_startup_without_a_tray_stays_visible() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { start_background: true, ..Default::default() });
    let startup = frame(&ctx, &mut app, false, vec![]);
    assert!(!has(&startup.commands, |c| matches!(c, ViewportCommand::Visible(false))), "{startup:?}");
}
