//! Full pipeline presets: Save captures every part, Apply restores it (UI-driven). Headless egui only; run through tools/isolated-test.sh.
mod common;
use ck::presets::{Delivery, VideoDelivery};
use ck::ui::{App, AppOptions, Tab};
use common::*;
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::json;

const W: f32 = 1440.0;

fn frame(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(W, 2400.0))),
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


fn rect(app: &App, k: &str) -> Rect {
    app.preset_rects().iter().find(|r| r.0 == k).map(|r| r.1).unwrap_or_else(|| panic!("{k} not drawn"))
}

fn press(ctx: &egui::Context, app: &mut App, k: &str) {
    let at = rect(app, k).center();
    frame(ctx, app, click(at));
    frames(ctx, app, 3);
}

fn name_it(ctx: &egui::Context, app: &mut App, n: &str) {
    let at = rect(app, "name").center();
    frame(ctx, app, click(at));
    frame(ctx, app, vec![Event::Key { key: egui::Key::A, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND }]);
    frame(ctx, app, vec![Event::Text(n.into())]);
    frames(ctx, app, 3);
}

#[test]
fn saving_then_applying_restores_every_pipeline_part() {
    let (ctx, mut app, _s) = open_app("pipe-full", Tab::Key);
    frames(&ctx, &mut app, 4);
    let out = std::env::temp_dir().join(format!("ck-pipe-out-{}", std::process::id()));
    let exp = std::env::temp_dir().join(format!("ck-pipe-exp-{}", std::process::id()));
    let loc = json!({"root": out, "folder_template": "{name}_{version}/{output}", "file_template": "{name}_####"});
    let mut cfg = ck::hints::HintCfg::default();
    cfg.guide = ck::hints::Producer::Sam;
    cfg.sam_model = "large".into();
    app.hint_apply_cfg_for_test(cfg.clone());
    app.draft_for_test(|p, o| {
        p.hint_mode = "full".into();
        p.despill_strength = 0.8;
        o.fg_enabled = false;
    });
    app.set_location_for_test(Some(loc.clone()));
    let mix = [
        ("matte", Delivery::Video(VideoDelivery { container: "mp4".into(), codec: "hevc".into(), quality: 20 })),
        ("comp", Delivery::Sequence),
        ("processed", Delivery::Video(VideoDelivery { container: "mov_prores4444".into(), codec: "prores422".into(), quality: 5 })),
    ];
    app.set_export_for_test(&mix, Some(30.0), true, Some(exp.clone()));
    frames(&ctx, &mut app, 2);
    let (p0, o0) = app.draft_snapshot_for_test().unwrap();
    name_it(&ctx, &mut app, "full");
    press(&ctx, &mut app, "save");
    // Change every part, then Apply.
    app.hint_apply_cfg_for_test(ck::hints::HintCfg::default());
    app.draft_for_test(|p, o| {
        p.hint_mode = "none".into();
        p.despill_strength = 0.1;
        o.fg_enabled = true;
    });
    app.set_location_for_test(None);
    app.set_export_for_test(&[], None, false, None);
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "apply");
    assert_eq!(app.hint_cfg_for_test().unwrap(), cfg);
    let (p1, o1) = app.draft_snapshot_for_test().unwrap();
    assert_eq!((p1.hint_mode.as_str(), p1.despill_strength, o1.fg_enabled), ("full", 0.8, false));
    assert_eq!((p1, o1), (p0, o0));
    let got = app.location_for_test().unwrap();
    assert_eq!((got["root"].clone(), got["folder_template"].clone()), (loc["root"].clone(), loc["folder_template"].clone()));
    let on_disk: serde_json::Value = serde_json::from_slice(&std::fs::read(app.project_root_for_test().join("project.json")).unwrap()).unwrap();
    assert_eq!(on_disk["output_location"]["file_template"], "{name}_####");
    let (d, fps, partial, dir) = app.export_state_for_test();
    assert_eq!((fps, partial, dir), (Some(30.0), true, Some(exp)));
    assert_eq!(d.len(), 3, "one delivery per enabled output, fg disabled");
    for (k, v) in &mix {
        assert_eq!(&d[*k], v);
    }
    assert_eq!(ck::persist::clip_hint_mode(&app.clip_for_test()).as_deref(), Some("full"), "explicit hint mode persisted");
}

#[test]
fn defaults_save_with_no_location_and_source_fps_and_roundtrip_as_such() {
    let (ctx, mut app, settings) = open_app("pipe-default", Tab::Key);
    frames(&ctx, &mut app, 4);
    name_it(&ctx, &mut app, "plain");
    press(&ctx, &mut app, "save");
    let (ps, err) = ck::presets::load(&ck::presets::file_beside(Some(&settings)).unwrap());
    assert!(err.is_none());
    let pl = ck::presets::Pipeline::from_value(&ps[0].data).expect("default pipeline saved");
    assert!(pl.location.is_none() && pl.export.fps.is_none() && pl.export.root.is_none() && !pl.export.has_video());
    assert!(pl.export.outputs.values().all(|d| *d == Delivery::Sequence));
    // Applying it clears an explicit location and fps override.
    app.set_location_for_test(Some(json!({"root": std::env::temp_dir(), "folder_template": "{name}", "file_template": "{name}_####"})));
    app.set_export_for_test(&[("processed", Delivery::Video(VideoDelivery::default_for("mp4")))], Some(12.0), false, Some(std::env::temp_dir()));
    frames(&ctx, &mut app, 2);
    press(&ctx, &mut app, "apply");
    assert!(app.location_for_test().is_none());
    let (d, fps, partial, dir) = app.export_state_for_test();
    assert_eq!((fps, partial, dir), (None, false, None));
    assert!(d.values().all(|x| *x == Delivery::Sequence));
}

#[test]
fn legacy_presets_are_labelled_and_apply_only_params() {
    let (ctx, mut app, settings) = open_app("pipe-legacy", Tab::Key);
    let f = ck::presets::file_beside(Some(&settings)).unwrap();
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: ck::presets::workflow_data(&ck::workflow::Params { despill_strength: 0.9, ..Default::default() }, &Default::default()) };
    assert!(ck::presets::label(&legacy).contains("legacy"));
    ck::presets::save(&f, &[legacy]).unwrap();
    app.reload_presets_for_test();
    app.set_export_for_test(&[], Some(12.0), false, None);
    let mut app2 = app;
    frames(&ctx, &mut app2, 4);
    press(&ctx, &mut app2, "apply");
    assert_eq!(app2.draft_snapshot_for_test().unwrap().0.despill_strength, 0.9);
    assert_eq!(app2.export_state_for_test().1, Some(12.0), "export untouched by a legacy preset");
}

#[test]
fn deliver_shows_every_output_with_its_own_choice_and_no_overlap() {
    let (ctx, mut app, _s) = open_app("pipe-cards", Tab::Deliver);
    frames(&ctx, &mut app, 4);
    let keys = ["fg", "matte", "comp", "processed"];
    let cards = app.deliver_rects().to_vec();
    for k in keys {
        assert!(cards.iter().any(|c| c.0 == k), "{k} card missing");
    }
    // Pick Video on matte through the UI: only that card changes.
    let at = cards.iter().find(|c| c.0 == "matte").unwrap().1;
    frame(&ctx, &mut app, click(egui::pos2(at.right() - 50.0, at.top() + 55.0)));
    frames(&ctx, &mut app, 3);
    let (d, ..) = app.export_state_for_test();
    assert!(matches!(d["matte"], Delivery::Video(_)), "matte is a video now: {d:?}");
    assert_eq!(d["fg"], Delivery::Sequence);
    assert_eq!(d["processed"], Delivery::Sequence);
    let cards = app.deliver_rects().to_vec();
    for (i, a) in cards.iter().enumerate() {
        assert!(a.1.max.x <= W && a.1.max.y > a.1.min.y, "{} inside 1440 wide", a.0);
        for b in &cards[i + 1..] {
            assert!(!a.1.intersects(b.1), "{} overlaps {}", a.0, b.0);
        }
    }
    assert!(cards.iter().any(|c| c.0 == "render"), "render action drawn once a video exists");
}
