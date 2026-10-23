//! Interaction tests for the shared above-graph action area, the compact SAM toolbar, the preset name field and the
//! node-graph wheel. Headless egui only; run through tools/isolated-test.sh.
mod common;
use ck::ui::graph::{self, Graph, NodeLook, View};
use ck::ui::{App, AppOptions, Tab};
use common::*;
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::json;

const W: f32 = 1440.0;

fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    frame_at_height(ctx, app, events, 2400.0);
}

fn frame_at_height(ctx: &egui::Context, app: &mut App, events: Vec<Event>, height: f32) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(W, height))),
        events,
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
}

fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        frame(ctx, app, vec![]);
    }
}

fn click(at: egui::Pos2) -> Vec<Event> {
    let b = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(at), b(true), b(false)]
}

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
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(proj),
            settings_path: Some(settings.clone()),
            open_clip: Some("c1".into()),
            tab: Some(tab),
            ..Default::default()
        },
    );
    frames(&ctx, &mut app, 20);
    (ctx, app, settings)
}

/// Status left, centered action group right, both in one panel above the graph canvas.
fn assert_action_layout(app: &App, action: Rect) {
    let [panel, left, right] = app.tools_rects().expect("tools panel drawn");
    let (canvas, _) = app.graph_rects().expect("graph drawn");
    assert!(panel.max.y <= canvas.min.y, "tools panel not above the graph: {panel:?} vs {canvas:?}");
    assert!(left.max.x < right.min.x && left.min.y == right.min.y, "{left:?} {right:?}");
    assert!(right.contains_rect(action), "action {action:?} outside the right region {right:?}");
    assert!((action.center().x - right.center().x).abs() < 40.0, "action not centered: {action:?} in {right:?}");
}

#[test]
fn key_and_deliver_actions_are_centered_in_the_right_region_above_the_graph() {
    let (ctx, mut app, _) = open_app("act-key", Tab::Key);
    for _ in 0..12 { frame_at_height(&ctx, &mut app, vec![], 900.0); }
    let r = app.ai().key_rect.expect("Key clip button drawn");
    assert_action_layout(&app, r);
    // Deliver (image sequences) uses the very same region.
    app.switch_tab(Tab::Deliver);
    app.ai_mut().key_rect = None;
    for _ in 0..12 { frame_at_height(&ctx, &mut app, vec![], 900.0); }
    let d = app.ai().key_rect.expect("render button drawn");
    assert_action_layout(&app, d);
    assert!((d.center().x - r.center().x).abs() < 1.0, "{d:?} vs {r:?}");
}

#[test]
fn deliver_sidebar_stays_visible_after_selecting_video() {
    let (ctx, mut app, _) = open_app("deliver-sidebar", Tab::Deliver);
    for _ in 0..12 { frame_at_height(&ctx, &mut app, vec![], 900.0); }
    let card = app.deliver_rects().iter().find(|r| r.0 == "fg").unwrap().1;
    let sidebar_edge = app.tools_rects().unwrap()[0].right();
    let at = egui::pos2(card.right() - 50.0, card.top() + 55.0);
    for ev in [pointer(at, None), pointer(at, Some(true)), pointer(at, Some(false))] {
        frame_at_height(&ctx, &mut app, ev, 900.0);
    }
    assert!(matches!(app.export_state_for_test().0["fg"], ck::presets::Delivery::Video(_)), "video selection must land");
    for i in 0..120 {
        frame_at_height(&ctx, &mut app, vec![Event::PointerMoved(at)], 900.0);
        let r = app.deliver_rects().iter().find(|r| r.0 == "fg").unwrap().1;
        assert!(r.width() > 240.0 && r.min.x >= 900.0 && r.max.x <= W + 1.0, "frame {i}: {r:?}");
        assert!(app.tools_rects().unwrap()[0].right() <= sidebar_edge + 2.0, "sidebar clipped on frame {i}: {:?}", app.tools_rects());
    }
}

#[test]
fn sam_toolbar_is_a_compact_aligned_column_of_tools_only() {
    let (ctx, mut app, _) = open_app("act-sam", Tab::Hint);
    app.hint_apply_cfg_for_test({
        let mut c = app.hint_cfg_for_test().unwrap();
        c.guide = ck::hints::Producer::Sam;
        c
    });
    frames(&ctx, &mut app, 6);
    let rects: Vec<Rect> = app.tool_strip_rects().iter().map(|(_, r)| *r).collect();
    assert!(rects.len() >= 5, "{rects:?}");
    let x = rects[0].center().x;
    for r in &rects {
        assert!(r.width() <= 32.0 && r.width() >= 26.0, "button width {r:?}");
        assert!((r.center().x - x).abs() < 0.5, "icons not aligned: {rects:?}");
    }
    for w in rects.windows(2) {
        assert!(w[1].min.y >= w[0].max.y, "overlap {w:?}");
    }
    // The column starts at the very top: no header label above the first tool.
    let top = rects[0].min.y;
    assert!(top < 200.0, "{top}");
}

#[test]
fn preset_name_field_stays_editable_and_saves_a_second_preset() {
    let (ctx, mut app, settings) = open_app("act-preset", Tab::Key);
    frames(&ctx, &mut app, 4);
    let rect = |app: &App, k: &str| app.preset_rects().iter().find(|r| r.0 == k).map(|r| r.1).unwrap_or_else(|| panic!("{k} not drawn"));
    let at = rect(&app, "name").center();
    frame(&ctx, &mut app, click(at));
    frame(&ctx, &mut app, vec![Event::Text("look1".into())]);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.preset_state().1, "look1", "typed text survives later frames");
    let at = rect(&app, "save").center();
    frame(&ctx, &mut app, click(at));
    frames(&ctx, &mut app, 3);
    assert_eq!(app.preset_state(), ("look1".into(), "look1".into()));
    // Edit the name after the save, then Save again: a second preset, the first one untouched.
    let at = rect(&app, "name").center();
    frame(&ctx, &mut app, click(at));
    frame(&ctx, &mut app, vec![Event::Text("b".into())]);
    frames(&ctx, &mut app, 3);
    assert_eq!(app.preset_state().1, "look1b", "field stays editable after a save");
    let at = rect(&app, "save").center();
    frame(&ctx, &mut app, click(at));
    frames(&ctx, &mut app, 3);
    assert_eq!(app.preset_state(), ("look1b".into(), "look1b".into()));
    let saved = std::fs::read_to_string(ck::presets::file_beside(Some(&settings)).unwrap()).unwrap();
    assert!(saved.contains("\"look1\"") && saved.contains("\"look1b\""), "{saved}");
}

fn pointer(at: egui::Pos2, pressed: Option<bool>) -> Vec<Event> {
    let mut v = vec![Event::PointerMoved(at)];
    if let Some(pressed) = pressed {
        v.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
    }
    v
}

/// 1440x900, Properties scrolled until the control is on screen; hover, press and release on separate frames.
fn real_click(ctx: &egui::Context, app: &mut App, k: &str) {
    real_click_at(ctx, app, k, 0.5)
}

fn real_click_at(ctx: &egui::Context, app: &mut App, k: &str, fx: f32) {
    let rect = |app: &App| app.preset_rects().iter().find(|r| r.0 == k).map(|r| r.1).unwrap_or_else(|| panic!("{k} not drawn"));
    for _ in 0..40 {
        if rect(app).min.y > 60.0 && rect(app).max.y < 880.0 {
            break;
        }
        let ev = Event::MouseWheel { unit: egui::MouseWheelUnit::Point, delta: Vec2::new(0.0, -120.0), modifiers: Modifiers::NONE, phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel };
        frame_at_height(ctx, app, vec![Event::PointerMoved(egui::pos2(W - 100.0, 400.0)), ev], 900.0);
        frame_at_height(ctx, app, vec![], 900.0);
    }
    let r = rect(app);
    assert!(r.min.y > 60.0 && r.max.y < 880.0, "{k} not scrolled into view: {r:?}");
    let at = egui::pos2(r.min.x + r.width() * fx, r.center().y);
    for ev in [pointer(at, None), pointer(at, Some(true)), pointer(at, Some(false))] {
        frame_at_height(ctx, app, ev, 900.0);
    }
    for _ in 0..3 {
        frame_at_height(ctx, app, vec![], 900.0);
    }
}

fn type_name(ctx: &egui::Context, app: &mut App, name: &str) {
    type_name_at(ctx, app, name, 0.5)
}

fn type_name_at(ctx: &egui::Context, app: &mut App, name: &str, fx: f32) {
    real_click_at(ctx, app, "name", fx);
    for ch in name.chars() {
        let key = egui::Key::from_name(&ch.to_string());
        let mut ev = vec![];
        if let Some(key) = key {
            ev.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        }
        ev.push(Event::Text(ch.to_string()));
        frame_at_height(ctx, app, ev, 900.0);
    }
    frame_at_height(ctx, app, vec![], 900.0);
}

#[test]
fn name_field_is_editable_after_deleting_every_preset_and_after_clearing_it() {
    let (ctx, mut app, settings) = open_app("act-preset-del", Tab::Key);
    for _ in 0..4 { frame_at_height(&ctx, &mut app, vec![], 900.0); }
    type_name(&ctx, &mut app, "one");
    real_click(&ctx, &mut app, "save");
    assert_eq!(app.preset_state(), ("one".into(), "one".into()));
    real_click(&ctx, &mut app, "delete");
    assert_eq!(app.preset_state(), (String::new(), String::new()), "last preset deleted, field emptied");
    // Blank field after the delete: typing letter by letter must land and save.
    type_name(&ctx, &mut app, "two");
    assert_eq!(app.preset_state().1, "two", "empty name field accepts typing");
    real_click(&ctx, &mut app, "save");
    assert_eq!(app.preset_state(), ("two".into(), "two".into()));
    // Clear via select-all + Backspace, then type again.
    real_click(&ctx, &mut app, "name");
    let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
    frame_at_height(&ctx, &mut app, vec![key(egui::Key::A, Modifiers::COMMAND)], 900.0);
    frame_at_height(&ctx, &mut app, vec![key(egui::Key::Backspace, Modifiers::NONE)], 900.0);
    frame_at_height(&ctx, &mut app, vec![], 900.0);
    assert_eq!(app.preset_state().1, "", "cleared");
    type_name(&ctx, &mut app, "three");
    assert_eq!(app.preset_state().1, "three", "field editable after clearing");
    real_click(&ctx, &mut app, "save");
    let saved = std::fs::read_to_string(ck::presets::file_beside(Some(&settings)).unwrap()).unwrap();
    assert!(saved.contains("\"two\"") && saved.contains("\"three\"") && !saved.contains("\"one\""), "{saved}");
}

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

#[test]
fn blank_name_field_takes_focus_wherever_it_is_clicked() {
    for (i, fx) in [0.03, 0.2, 0.5, 0.97].into_iter().enumerate() {
        let (ctx, mut app, _) = open_app(&format!("act-preset-blank{i}"), Tab::Key);
        for _ in 0..4 { frame_at_height(&ctx, &mut app, vec![], 900.0); }
        type_name_at(&ctx, &mut app, "x", fx);
        assert_eq!(app.preset_state().1, "x", "blank field ignored a click at {fx} of its width");
    }
}
