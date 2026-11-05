//! The node-graph wheel. Headless egui only; run through tools/isolated-test.sh.
use ck::ui::graph::{self, Graph, NodeLook, View};
mod common;
use ck::ui::{App, AppOptions, Tab};
use common::*;
use eframe::egui::{self, Event, Modifiers, RawInput, Rect, Vec2};
use serde_json::json;

#[test]
fn graph_wheel_zooms_under_the_pointer_and_does_not_scroll_the_parent() {
    let ctx = egui::Context::default();
    let (mut g, mut v) = (Graph::new(), View::default());
    let mut offset = 0.0;
    let mut run = |events: Vec<Event>, v: &mut View, g: &mut Graph| {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 700.0))),
            events,
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| {
            let out = egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.allocate_space(Vec2::new(10.0, 40.0));
                graph::show(ui, g, v, None, |_| NodeLook::default(), 300.0);
                ui.allocate_space(Vec2::new(10.0, 1200.0));
            });
            offset = out.state.clamped_offset().y;
        })
        .textures_delta
        .clear();
    };
    run(vec![], &mut v, &mut g);
    run(vec![], &mut v, &mut g);
    let at = v.canvas.min + Vec2::new(300.0, 120.0);
    let under = v.to_graph(at, v.canvas);
    let z0 = v.zoom;
    run(vec![Event::PointerMoved(at)], &mut v, &mut g);
    run(
        vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, 120.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown }],
        &mut v,
        &mut g,
    );
    for _ in 0..10 {
        run(vec![], &mut v, &mut g);
    }
    assert!(v.zoom > z0 * 1.05, "wheel up did not zoom in: {} -> {}", z0, v.zoom);
    let still = v.to_graph(at, v.canvas);
    assert!((still - under).length() < 1.0, "point under the pointer moved: {under:?} -> {still:?}");
    assert_eq!(offset, 0.0, "parent scroll area moved");
}

// DELETED key_and_deliver_actions_are_centered_in_the_right_region_above_the_graph: old above-graph action-area geometry.
// DELETED sam_toolbar_is_a_compact_aligned_column_of_tools_only: old tool-strip rect geometry.
// DELETED preset_name_field_*, name_field_is_editable_*, blank_name_field_takes_focus_*: old preset text-field widget behaviour.

fn open_app(tag: &str, tab: Tab) -> (egui::Context, App, std::path::PathBuf) {
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
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(proj), settings_path: Some(settings.clone()), open_clip: Some("c1".into()), tab: Some(tab), ..Default::default() });
    for _ in 0..20 {
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 2400.0))), ..Default::default() };
        ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
    }
    (ctx, app, settings)
}

#[test]
fn deliver_video_selection_lands() {
    let (ctx, mut app, _s) = open_app("act-video", Tab::Deliver);
    app.deliver_set_video_for_test(&ctx, 0, true).expect("fg set to video");
    assert!(matches!(app.export_state_for_test().0["fg"], ck::presets::Delivery::Video(_)), "video selection must land");
    assert!(app.deliver_outputs_for_test().iter().any(|o| o.0 == "fg" && o.1));
}

#[test]
fn preset_saves_then_deletes_only_after_confirmation() {
    let (ctx, mut app, settings) = open_app("act-preset", Tab::Key);
    let f = ck::presets::file_beside(Some(&settings)).unwrap();
    let names = || ck::presets::load(&f).0.into_iter().map(|p| p.name).collect::<Vec<_>>();
    app.deliver_preset_for_test(&ctx, "save", "one").expect("saved");
    app.deliver_preset_for_test(&ctx, "save", "two").expect("saved a second");
    assert_eq!(names(), ["one", "two"]);
    assert_eq!(app.preset_state().0, "two", "the saved preset is selected");
    assert!(app.deliver_preset_for_test(&ctx, "confirm_delete", "one").is_err(), "delete needs RequestDelete first");
    assert_eq!(names(), ["one", "two"]);
    app.deliver_preset_for_test(&ctx, "request_delete", "one").unwrap();
    app.deliver_preset_for_test(&ctx, "confirm_delete", "one").expect("confirmed");
    assert_eq!(names(), ["two"]);
    assert!(app.deliver_preset_for_test(&ctx, "save", "  ").is_err(), "blank name refused");
}
