//! The Board's responsive Queue panel keeps real queued work inside its fixed-width area in both themes.
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{Pipeline, PipelineExport};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::ui::{theme, App, AppOptions};
use ck::workflow::{OutputCfg, Params};
use eframe::egui::{self, epaint::Shape, Event, Pos2, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SIZE: Vec2 = Vec2::new(1440.0, 900.0);
const LONG: [&str; 2] = [
    "a_really_long_shot_name_that_keeps_going_and_going_and_going_and_going_take_0001",
    "another_extremely_long_shot_name_for_the_second_clip_in_this_batch_take_0002",
];

fn write_png(p: &Path) {
    let mut e = png::Encoder::new(fs::File::create(p).unwrap(), 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&[7; 48]).unwrap();
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}

fn env(tag: &str, clips: &[&str], modes: Value) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    for name in clips {
        let c = project.join("clips").join(name);
        fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..3 {
            write_png(&c.join(format!("Frames/f_{}.png", 1101 + i)));
        }
        fs::write(c.join(".video_metadata.json"), json!({"fps": 24.0, "width": 4, "height": 4}).to_string()).unwrap();
        fs::write(c.join("clip.json"), "{}").unwrap();
    }
    fs::write(project.join("project.json"), json!({"version": 2}).to_string()).unwrap();
    let repo = common::make_repo(&base, "repo", json!({"modes": modes}));
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": repo, "python": "/usr/bin/python3", "gpu": 0}]}).to_string()).unwrap();
    Env { base, project, settings }
}

fn frame_with(ctx: &egui::Context, app: &mut App, events: Vec<Event>) -> Vec<egui::epaint::ClippedShape> {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)), events, ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    out.shapes
}
fn frames(ctx: &egui::Context, app: &mut App, n: usize) -> Vec<egui::epaint::ClippedShape> {
    let mut last = vec![];
    for _ in 0..n {
        last = frame_with(ctx, app, vec![]);
    }
    last
}
fn pump(ctx: &egui::Context, app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        frame_with(ctx, app, vec![]);
        std::thread::sleep(Duration::from_millis(15));
    }
}

fn texts(s: &Shape, clip: Rect, out: &mut Vec<(Rect, Rect, String)>) {
    match s {
        Shape::Vec(v) => v.iter().for_each(|x| texts(x, clip, out)),
        Shape::Text(t) => out.push((t.galley.rect.translate(t.pos.to_vec2()), clip, t.galley.job.text.clone())),
        _ => {}
    }
}
fn rects(s: &Shape, out: &mut Vec<Rect>) {
    match s {
        Shape::Vec(v) => v.iter().for_each(|x| rects(x, out)),
        Shape::Rect(r) => out.push(r.rect),
        _ => {}
    }
}
fn pipeline() -> Pipeline {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (Producer::Gvm, Final::Matanyone2);
    let mut outputs = OutputCfg::default();
    outputs.fg_enabled = false;
    outputs.comp_enabled = false;
    outputs.matte_format = "png".into();
    let o2 = outputs.clone();
    Pipeline { hint: h, params: Params { hint_mode: "full".into(), ..Params::default() }, outputs, location: None, export: PipelineExport::for_outputs(&o2) }
}

/// The responsive panel and its actual Queue data stay inside their assigned area.
fn check(ctx: &egui::Context, app: &mut App, want_w: f32, what: &str) {
    let shapes = frames(ctx, app, 6);
    let panel = app.queue_panel_rect().expect("queue panel drawn");
    assert!((panel.width() - want_w).abs() <= 2.0, "{what}: width {} want {want_w}", panel.width());
    // The layout is fixed at this viewport; more frames do not change its responsive width.
    frames(ctx, app, 5);
    assert_eq!(app.queue_panel_rect().unwrap(), panel, "{what}: responsive width changed");
    // Cards stay on their side; every Queue label and its text fit the panel.
    for (i, card, _) in app.board_cards() {
        assert!(card.max.x <= panel.min.x + 1.0, "{what}: card {i} {card:?} reaches under the panel {panel:?}");
    }
    // No painted box (card frames, bars, buttons) reaches past the panel's right edge either.
    let mut boxes = vec![];
    shapes.iter().for_each(|c| rects(&c.shape, &mut boxes));
    for r in boxes.iter().filter(|r| r.min.x >= panel.min.x && r.min.y > panel.min.y && r.min.y < panel.max.y - 20.0 && r.width() < panel.width()) {
        assert!(r.max.x <= panel.max.x + 0.5, "{what}: a box {r:?} runs past the panel's right edge {}", panel.max.x);
    }
    let mut all = vec![];
    shapes.iter().for_each(|c| texts(&c.shape, c.clip_rect, &mut all));
    for (r, _, t) in all.iter().filter(|(r, _, _)| r.min.x >= panel.min.x && r.min.y > panel.min.y) {
        assert!(r.max.x <= panel.max.x + 0.5, "{what}: text {t:?} {r:?} runs past the panel's right edge {}", panel.max.x);
    }
}

#[test]
fn queue_panel_uses_responsive_width_and_keeps_real_work_inside_in_dark_and_light() {
    for pref in ["Dark", "Light"] {
        let e = env(&format!("bqr-{pref}"), &LONG, json!({"gvm": "hold"}));
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() });
        theme::apply_preference(&ctx, pref);
        app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into());
        frames(&ctx, &mut app, 12);
        let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
        for i in 0..proj.clips.len() {
            let before = app.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>();
            app.ai_mut().queue_pipeline(&ctx, &proj, &Settings::default(), i, &pipeline());
            pump(&ctx, &mut app, "queued", |a| a.ai().pipeline_block_reason().is_some() || a.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>() > before);
            assert_eq!(app.ai().pipeline_block_reason(), None);
        }
        pump(&ctx, &mut app, "gvm running", |a| a.ai().queue_view()[0].runs[0].rows[0].cell.text.ends_with('%'));
        frames(&ctx, &mut app, 3);
        let shapes = frames(&ctx, &mut app, 3);
        let mut all = vec![];
        shapes.iter().for_each(|c| texts(&c.shape, c.clip_rect, &mut all));
        assert!(all.iter().any(|(_, _, t)| t.contains("track") || t.contains("Keying") || t.contains("Queued")), "{pref}: active work is not shown in Queue");
        for label in ["Queue", "Machines", "Waiting"] { assert!(all.iter().any(|(_, _, t)| t == label), "{pref}: missing {label}"); }
        check(&ctx, &mut app, 260.0, &format!("{pref} Queue"));
        drop(app);
        let _ = fs::remove_dir_all(e.base);
    }
}
