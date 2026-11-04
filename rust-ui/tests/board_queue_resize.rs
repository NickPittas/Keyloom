//! The Board's Queue panel is resized by dragging its edge (headless egui, real queued work). Run through tools/isolated-test.sh.
//! Each drag must stick, the single separator must sit on the panel edge, every Cancel button and text must lie
//! inside the panel, with long shot names and the Details expanded, in Dark and Light.
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{Pipeline, PipelineExport};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::ui::{theme, App, AppOptions};
use ck::workflow::{OutputCfg, Params};
use eframe::egui::{self, epaint::Shape, Event, PointerButton, Pos2, RawInput, Rect, Vec2};
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

/// Press at `from`, move to `to` in steps over several frames, release, then let the layout settle.
fn drag(ctx: &egui::Context, app: &mut App, from: Pos2, to: Pos2) {
    let mv = |p| vec![Event::PointerMoved(p)];
    let btn = |p, pressed| vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() }];
    frame_with(ctx, app, mv(from));
    frame_with(ctx, app, mv(from));
    frame_with(ctx, app, btn(from, true));
    for k in 1..=8 {
        let t = k as f32 / 8.0;
        frame_with(ctx, app, mv(from + (to - from) * t));
    }
    frame_with(ctx, app, btn(to, false));
    frames(ctx, app, 3);
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
fn vlines(s: &Shape, out: &mut Vec<(f32, f32, f32)>) {
    match s {
        Shape::Vec(v) => v.iter().for_each(|x| vlines(x, out)),
        Shape::LineSegment { points, .. } if (points[0].x - points[1].x).abs() < 0.01 => out.push((points[0].x, points[0].y.min(points[1].y), points[0].y.max(points[1].y))),
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

/// Everything about the drawn panel that must hold after each drag.
fn check(ctx: &egui::Context, app: &mut App, want_w: f32, what: &str) {
    let shapes = frames(ctx, app, 6);
    let panel = app.queue_panel_rect().expect("queue panel drawn");
    assert!((panel.width() - want_w).abs() <= 2.0, "{what}: width {} want {want_w}", panel.width());
    // The panel's saved width follows what is drawn: more frames never change it.
    let again = frames(ctx, app, 5);
    assert_eq!(app.queue_panel_rect().unwrap(), panel, "{what}: the width snapped back");
    drop(again);
    // One separator, on the panel's own edge; none through the panel.
    let mut lines = vec![];
    shapes.iter().for_each(|c| vlines(&c.shape, &mut lines));
    let tall = |l: &&(f32, f32, f32)| l.2 - l.1 >= panel.height() * 0.9;
    let on_edge: Vec<_> = lines.iter().filter(tall).filter(|l| (l.0 - panel.min.x).abs() <= 3.0).collect();
    assert_eq!(on_edge.len(), 1, "{what}: want one separator on the panel edge {}, got {lines:?}", panel.min.x);
    let inside: Vec<_> = lines.iter().filter(tall).filter(|l| l.0 > panel.min.x + 3.0 && l.0 < panel.max.x - 3.0).collect();
    assert!(inside.is_empty(), "{what}: a divider runs through the panel: {inside:?}");
    // Cards stay on their side; every Cancel and every text of the Queue lies inside the panel.
    for (i, card, _) in app.board_cards() {
        assert!(card.max.x <= panel.min.x + 1.0, "{what}: card {i} {card:?} reaches under the panel {panel:?}");
    }
    let cancels = app.ai().cancel_rects.clone();
    assert!(!cancels.is_empty(), "{what}: no inline Cancel drawn");
    for r in &cancels {
        assert!(panel.contains_rect(*r), "{what}: Cancel {r:?} outside the panel {panel:?}");
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
fn queue_panel_resizes_and_keeps_every_control_inside_in_dark_and_light() {
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
        // Expand every Details by clicking its header.
        for _ in 0..2 {
            let shapes = frames(&ctx, &mut app, 2);
            let mut all = vec![];
            shapes.iter().for_each(|c| texts(&c.shape, c.clip_rect, &mut all));
            if let Some((r, _, _)) = all.iter().find(|(_, _, t)| t == "Details") {
                let p = r.center();
                frame_with(&ctx, &mut app, vec![Event::PointerMoved(p)]);
                frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
                frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
            }
        }
        let shapes = frames(&ctx, &mut app, 3);
        let mut all = vec![];
        shapes.iter().for_each(|c| texts(&c.shape, c.clip_rect, &mut all));
        assert!(all.iter().any(|(_, _, t)| t.contains("Starting on") || t.contains("l0:") || t.contains("Queued")), "{pref}: Details did not expand");

        let panel = app.queue_panel_rect().unwrap();
        if panel.width() != 540.0 {
            let mut all = vec![];
            frames(&ctx, &mut app, 2).iter().for_each(|c| texts(&c.shape, c.clip_rect, &mut all));
            all.sort_by(|a, b| b.0.max.x.total_cmp(&a.0.max.x));
            for (r, _, t) in all.iter().take(8) { eprintln!("WIDE {r:?} {t:?}"); }
        }
        assert_eq!(panel.width(), 540.0, "{pref}: default width");
        check(&ctx, &mut app, 540.0, &format!("{pref} start"));
        // 540 -> 340 -> 540 -> 900 -> 340, grabbing the edge where it is each time.
        for (k, w) in [340.0f32, 540.0, 900.0, 340.0].into_iter().enumerate() {
            let edge = app.queue_panel_rect().unwrap();
            let y = edge.center().y;
            drag(&ctx, &mut app, Pos2::new(edge.min.x + 1.0, y), Pos2::new(edge.max.x - w, y));
            check(&ctx, &mut app, w, &format!("{pref} drag {k} to {w}"));
        }
        drop(app);
        let _ = fs::remove_dir_all(e.base);
    }
}
