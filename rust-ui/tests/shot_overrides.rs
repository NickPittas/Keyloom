//! Preset base -> explicit shot overrides. A hand edit made through the Key controller (Temporal window Off) survives Back,
//! save/reload and every Batch preset kind; untouched shots inherit the preset; the queued job is a frozen
//! snapshot. Headless egui + fake worker; run through tools/isolated-test.sh.
mod common;
use ck::hints::HintCfg;
use ck::presets::{Pipeline, PipelineExport};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::ui::{App, AppOptions, Tab};
use ck::workflow::{OutputCfg, Params};
use ck::ui::ai::{KeyCmd, KeyEdit};
use ck::ui::shot_page::{Act, DeliverAct, PropAct};
use eframe::egui::{self, Event, RawInput, Rect, Vec2};
use serde_json::json;
use std::fs;
use std::path::{Path, PathBuf};
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
    frames(ctx, app, 6);
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}
fn env(tag: &str, clips: &[&str]) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    for name in clips {
        let c = project.join("clips").join(name);
        fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..3u8 {
            let mut e = png::Encoder::new(fs::File::create(c.join(format!("Frames/f_{}.png", 1101 + i as u32))).unwrap(), 4, 4);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[i; 48]).unwrap();
        }
        fs::write(c.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
        fs::write(c.join("clip.json"), "{}").unwrap();
    }
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = common::make_repo(&base, "repo", json!({}));
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0}]}).to_string()).unwrap();
    Env { base, project, settings }
}
fn start(e: &Env, open: Option<&str>) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: open.map(Into::into), tab: Some(Tab::Key), ..Default::default() },
    );
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample
    frames(&ctx, &mut app, 20);
    (ctx, app)
}
fn start_tab(e: &Env, open: Option<&str>, tab: Tab) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: open.map(Into::into), tab: Some(tab), ..Default::default() },
    );
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into()); // Automatic waits for a live GPU sample
    frames(&ctx, &mut app, 20);
    (ctx, app)
}
fn pipeline(window: &str) -> Pipeline {
    let o = OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() };
    Pipeline {
        hint: HintCfg::default(),
        params: Params { temporal_window: window.into(), ..Params::default() },
        export: PipelineExport::for_outputs(&o),
        outputs: o,
        location: None,
    }
}
fn batch(ctx: &egui::Context, app: &mut App, e: &Env, window: &str) {
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let picks: Vec<usize> = (0..proj.clips.len()).collect();
    app.ai_mut().begin_batch_pipeline(ctx, &proj, &Settings::default(), &picks, &pipeline(window));
    pump(ctx, app, "review", |a| a.ai().batch_ready());
}
fn queued_windows(app: &App) -> Vec<(String, String)> {
    let mut v: Vec<_> = app
        .ai()
        .scheduler()
        .snapshot()
        .iter()
        .map(|j| (j.job.clip_root.file_name().unwrap().to_string_lossy().into_owned(), j.job.params["temporal_window"].as_str().unwrap().to_string()))
        .collect();
    v.sort();
    v
}

/// Choose Temporal window "Off" on shot a through the Key controller (`KeyCmd::Edit`, what the dropdown sends).
fn edit_temporal_off(ctx: &egui::Context, app: &mut App, e: &Env) {
    let (proj, clip) = fresh_clip(e, "a");
    app.ai_mut().key_cmd(&clip, &proj, &Settings::default(), ctx, KeyCmd::Edit(KeyEdit::TemporalWindow("1/1".into()))).expect("edit applied");
    frames(ctx, app, 3);
}

#[test]
fn shot_edit_off_survives_back_reload_and_every_batch_preset() {
    let e = env("ov-main", &["a", "b"]);
    let (ctx, mut app) = start(&e, Some("a"));
    assert!(app.ai().shot_overrides(&app.clip_for_test()).is_empty(), "opening alone marks nothing");
    edit_temporal_off(&ctx, &mut app, &e);
    let d = app.ai().draft_of("a").unwrap();
    assert_eq!(d.params.temporal_window, "1/1");
    assert_eq!(d.marked(), ["temporal_window"], "only the edited field is marked");
    assert_eq!(d.explicit["temporal_window"].as_ref().unwrap().value, "auto", "the base is the value before the edit");
    frames(&ctx, &mut app, 6);
    assert_eq!(app.ai().draft_of("a").unwrap().explicit.len(), 1, "idle frames add no markers");
    app.save_project().unwrap();
    drop(app);

    // Fresh session straight to the Board: the unopened shots' drafts come from disk.
    let (ctx, mut app) = start(&e, None);
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(e.project.join("clips/a/clip.json")).unwrap()).unwrap();
    assert_eq!(raw["ck_native"]["draft"]["explicit"], json!({"temporal_window": {"base": "auto", "preset": null}}));
    assert!(fs::read_to_string(e.project.join("clips/b/clip.json")).unwrap().find("explicit").is_none());
    batch(&ctx, &mut app, &e, "24/12");
    let tw = |f: &str| app.ai().batch_row_settings(f).unwrap().0.temporal_window;
    assert_eq!((tw("a"), tw("b")), ("1/1".into(), "24/12".into()), "row preview: shot override kept, other shot inherits");
    assert!(app.ai().batch_summary().join("\n").contains("shot overrides: temporal_window"));
    // Choosing another preset leaves the override.
    batch(&ctx, &mut app, &e, "80/40");
    let tw = |f: &str| app.ai().batch_row_settings(f).unwrap().0.temporal_window;
    assert_eq!((tw("a"), tw("b")), ("1/1".into(), "80/40".into()));

    app.ai_mut().confirm_batch();
    assert_eq!(queued_windows(&app), [("a".into(), "1/1".into()), ("b".into(), "80/40".into())], "queued snapshot");
    assert_eq!(app.ai().draft_of("a").unwrap().explicit.len(), 1, "confirm keeps the marker");
    let frozen = queued_windows(&app);
    batch(&ctx, &mut app, &e, "8/4"); // a later preset review does not touch what is queued
    assert_eq!(queued_windows(&app), frozen);
    app.ai_mut().cancel_batch();

    // Confirm + save + reload preserve the marker and the choice.
    app.save_project().unwrap();
    drop(app);
    let (ctx, mut app) = start(&e, None);
    batch(&ctx, &mut app, &e, "24/12");
    assert_eq!(app.ai().batch_row_settings("a").unwrap().0.temporal_window, "1/1");

    // Reset restores the base and future inheritance.
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let clip = proj.clips.iter().find(|c| c.folder == "a").unwrap().clone();
    app.ai_mut().cancel_batch();
    app.ai_mut().reset_overrides(&clip, &proj, &Settings::default());
    assert!(app.ai().draft_of("a").unwrap().explicit.is_empty());
    // The confirmed 80/40 queue became the shot's base: Reset returns to it, not to the global default.
    assert_eq!(app.ai().draft_of("a").unwrap().params.temporal_window, "80/40");
    batch(&ctx, &mut app, &e, "24/12");
    assert_eq!(app.ai().batch_row_settings("a").unwrap().0.temporal_window, "24/12");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn legacy_batch_preset_follows_the_same_rules() {
    let e = env("ov-legacy", &["a", "b"]);
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: json!({"params": {"temporal_window": "8/4", "despill_strength": 0.2}}) };
    ck::presets::save(&e.base.join("presets.json"), &[legacy.clone()]).unwrap();
    let (ctx, mut app) = start(&e, Some("a"));
    edit_temporal_off(&ctx, &mut app, &e);
    app.save_project().unwrap();
    drop(app);
    // Fresh session, both shots reviewed with the legacy preset chosen (what the Board's preset selector + Key send).
    let (ctx, mut app) = start(&e, None);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    app.ai_mut().batch_dialog_set_preset(&ctx, Some("old".into()));
    app.ai_mut().begin_board_batch(&ctx, &proj, &Settings::default(), &[0, 1]); // shot a keeps its own value, so the review opens
    pump(&ctx, &mut app, "legacy rows", |a| a.ai().batch_ready() && a.ai().batch_row_settings("b").is_some_and(|r| r.0.temporal_window == "8/4"));
    let (pa, pb) = (app.ai().batch_row_settings("a").unwrap().0, app.ai().batch_row_settings("b").unwrap().0);
    assert_eq!((pa.temporal_window.as_str(), pb.temporal_window.as_str()), ("1/1", "8/4"));
    assert_eq!((pa.despill_strength, pb.despill_strength), (0.2, 0.2), "unmarked fields still take the legacy preset");
    // Deliberate single-shot Apply (what the Deliver preset button sends) of the legacy preset replaces only the fields
    // it carries: temporal_window follows the preset and loses its marker; an unrelated hand edit keeps both.
    app.ai_mut().cancel_batch();
    drop(app);
    let (ctx, mut app) = start(&e, Some("a"));
    let (proj, clip) = fresh_clip(&e, "a");
    app.ai_mut().draft_mut(&clip, &proj, &Settings::default()).explicit.insert("vram_profile".into(), None);
    assert!(app.ai().shot_overrides(&clip).contains(&"temporal_window".to_string()));
    app.deliver_preset_for_test(&ctx, "apply", "old").expect("apply");
    frames(&ctx, &mut app, 3);
    let d = app.ai().draft_of("a").unwrap();
    assert_eq!(d.params.temporal_window, "8/4");
    assert_eq!(d.marked(), ["vram_profile"], "carried fields lose the marker, others keep it");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn explicit_hint_pick_wins_but_a_preset_applied_mode_does_not() {
    let e = env("ov-hint", &["a", "b"]);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let (a, b) = (&proj.clips[0], &proj.clips[1]);
    ck::persist::save_hint_mode(a, Some("none")).unwrap(); // the user's own pick
    ck::persist::save_preset_hint_mode(b, "none").unwrap(); // only records what a preset resolved to
    let (ctx, mut app) = start(&e, None);
    let mut p = pipeline("auto");
    p.params.hint_mode = "full".into();
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    app.ai_mut().begin_batch_pipeline(&ctx, &proj, &Settings::default(), &[0, 1], &p);
    pump(&ctx, &mut app, "review", |a| a.ai().batch_ready());
    let (ra, rb) = (app.ai().batch_row_settings("a").unwrap().0, app.ai().batch_row_settings("b").unwrap().0);
    assert_eq!((ra.hint_mode.as_str(), ra.use_alpha_hint), ("none", false), "explicit pick kept");
    assert_eq!((rb.hint_mode.as_str(), rb.use_alpha_hint), ("full", true), "preset-recorded mode follows the new preset");
    assert!(ck::persist::explicit_hint_mode(&proj.clips[0]).is_some() && ck::persist::explicit_hint_mode(&proj.clips[1]).is_none());
    drop(app);
    let _ = fs::remove_dir_all(e.base);
    let _ = Path::new("");
}

fn confirm(app: &mut App) {
    app.ai_mut().confirm_batch();
}
fn fresh_clip(e: &Env, folder: &str) -> (ck::project::Project, ck::project::Clip) {
    let p = load_project(&e.project, &LoadOptions::default()).unwrap();
    let c = p.clips.iter().find(|c| c.folder == folder).unwrap().clone();
    (p, c)
}

#[test]
fn reset_returns_to_the_base_recorded_at_edit_time_with_an_honest_label() {
    let e = env("ov-base", &["a"]);
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: json!({"params": {"temporal_window": "8/4"}}) };
    ck::presets::save(&e.base.join("presets.json"), &[legacy]).unwrap();
    let (ctx, mut app) = start(&e, Some("a"));
    edit_temporal_off(&ctx, &mut app, &e);
    let (proj, clip) = fresh_clip(&e, "a");
    assert_eq!(app.ai().reset_label(&clip), "Reset to values before editing");
    // A confirmed queue of the legacy preset makes it the edited field's Reset base (the single-shot Apply
    // route to the same base is `reset_after_a_single_shot_apply_returns_to_that_preset`).
    app.ai_mut().batch_dialog_set_preset(&ctx, Some("old".into()));
    app.ai_mut().begin_board_batch(&ctx, &proj, &Settings::default(), &[0]);
    pump(&ctx, &mut app, "review or queued", |a| a.ai().batch_ready() || !a.ai().scheduler().snapshot().is_empty());
    confirm(&mut app);
    let b = app.ai().draft_of("a").unwrap().explicit["temporal_window"].clone().unwrap();
    assert_eq!((b.value, b.preset.as_deref()), (json!("8/4"), Some("old")));
    assert_eq!(app.ai().reset_label(&clip), "Reset to preset “old”");
    app.ai_mut().key_cmd(&clip, &proj, &Settings::default(), &ctx, KeyCmd::ResetOverrides).unwrap();
    let d = app.ai().draft_of("a").unwrap();
    assert_eq!((d.params.temporal_window.as_str(), d.explicit.len()), ("8/4", 0));
    assert_eq!(d.from_preset["temporal_window"], "old", "provenance survives the reset");
    // A marker saved without a base (first build) can only fall back to defaults and says so.
    app.ai_mut().draft_mut(&clip, &proj, &Settings::default()).explicit.insert("vram_profile".into(), None);
    assert_eq!(app.ai().reset_label(&clip), "Reset to defaults");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn reset_after_a_single_shot_apply_returns_to_that_preset() {
    let e = env("ov-base-apply", &["a"]);
    let legacy = ck::presets::Preset { name: "old".into(), kind: "workflow".into(), data: json!({"params": {"temporal_window": "8/4"}}) };
    ck::presets::save(&e.base.join("presets.json"), &[legacy]).unwrap();
    let (ctx, mut app) = start(&e, Some("a"));
    app.deliver_preset_for_test(&ctx, "apply", "old").expect("apply"); // deliberate Apply: the preset is now the base
    frames(&ctx, &mut app, 3);
    assert_eq!(app.ai().draft_of("a").unwrap().params.temporal_window, "8/4");
    edit_temporal_off(&ctx, &mut app, &e);
    let (proj, clip) = fresh_clip(&e, "a");
    let b = app.ai().draft_of("a").unwrap().explicit["temporal_window"].clone().unwrap();
    assert_eq!((b.value, b.preset.as_deref()), (json!("8/4"), Some("old")));
    assert_eq!(app.ai().reset_label(&clip), "Reset to preset “old”");
    app.ai_mut().key_cmd(&clip, &proj, &Settings::default(), &ctx, KeyCmd::ResetOverrides).unwrap();
    assert_eq!(app.ai().draft_of("a").unwrap().params.temporal_window, "8/4");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

fn full_pipeline(mode: &str) -> Pipeline {
    let mut p = pipeline("auto");
    p.params.hint_mode = mode.into();
    p
}

#[test]
fn confirmed_pipeline_keeps_an_explicit_hint_pick_across_reload_and_later_presets() {
    let e = env("ov-hint2", &["a", "b"]);
    let (_, a) = fresh_clip(&e, "a");
    ck::persist::save_hint_pick(&a, "none", ("keyframes", false)).unwrap();
    let (ctx, mut app) = start(&e, None);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    app.ai_mut().begin_batch_pipeline(&ctx, &proj, &Settings::default(), &[0, 1], &full_pipeline("full"));
    pump(&ctx, &mut app, "review", |a| a.ai().batch_ready());
    confirm(&mut app);
    drop(app);
    let (_, a) = fresh_clip(&e, "a");
    let (_, b) = fresh_clip(&e, "b");
    assert_eq!(ck::persist::explicit_hint_mode(&a).as_deref(), Some("none"), "the pick is still the pick after the preset ran");
    assert_eq!(ck::persist::hint_mode_base(&a), Some(("full".into(), true)), "its Reset base follows the preset");
    assert_eq!(ck::persist::clip_hint_mode(&b).as_deref(), Some("full"));
    assert!(ck::persist::explicit_hint_mode(&b).is_none(), "an applied preset is not a pick");
    // A different preset next time: the pick wins, the other shot follows.
    let (ctx, mut app) = start(&e, None);
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    app.ai_mut().begin_batch_pipeline(&ctx, &proj, &Settings::default(), &[0, 1], &full_pipeline("keyframes"));
    pump(&ctx, &mut app, "review2", |a| a.ai().batch_ready());
    assert_eq!(app.ai().batch_row_settings("a").unwrap().0.hint_mode, "none");
    assert_eq!(app.ai().batch_row_settings("b").unwrap().0.hint_mode, "keyframes");
    // Reset gives the pick up: the mode returns to the preset's result, recorded as such.
    app.ai_mut().cancel_batch();
    let (proj, a) = fresh_clip(&e, "a");
    app.ai_mut().reset_overrides(&a, &proj, &Settings::default());
    let (_, a) = fresh_clip(&e, "a");
    assert_eq!((ck::persist::clip_hint_mode(&a).as_deref(), ck::persist::explicit_hint_mode(&a)), (Some("full"), None));
    assert_eq!(app.ai().draft_of("a").unwrap().params.hint_mode, "full");
    // A pick whose base was never a preset leaves no recorded mode at all (inheritance resumes).
    ck::persist::save_hint_pick(&a, "none", ("keyframes", false)).unwrap();
    let (proj, a) = fresh_clip(&e, "a");
    app.ai_mut().reset_overrides(&a, &proj, &Settings::default());
    let (_, a) = fresh_clip(&e, "a");
    assert!(ck::persist::clip_hint_mode(&a).is_none() && ck::persist::hint_mode_base(&a).is_none());
    assert_eq!(app.ai().draft_of("a").unwrap().params.hint_mode, "keyframes");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_real_deliver_format_edit_is_an_override_like_the_key_panel_one() {
    let e = env("ov-deliver", &["a", "b"]);
    let (ctx, mut app) = start_tab(&e, Some("a"), Tab::Deliver);
    // Output 0 is FG; SetSeqFormat is what the FG PNG choice sends.
    app.act_for_test(&ctx, Act::Props(PropAct::Deliver(DeliverAct::SetSeqFormat { output: 0, exr: false })));
    frames(&ctx, &mut app, 3);
    let d = app.ai().draft_of("a").unwrap();
    assert_eq!(d.out.fg_format, "png");
    assert_eq!(d.marked(), ["fg_format"]);
    assert_eq!(d.explicit["fg_format"].as_ref().unwrap().value, "exr");
    app.save_project().unwrap();
    drop(app);
    let (ctx, mut app) = start(&e, None);
    batch(&ctx, &mut app, &e, "auto"); // preset: fg_format exr
    assert_eq!(app.ai().batch_row_settings("a").unwrap().1.fg_format, "png");
    assert_eq!(app.ai().batch_row_settings("b").unwrap().1.fg_format, "exr");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn batch_use_preset_values_on_an_unopened_shot_is_saved_and_survives_reopen() {
    let e = env("ov-unopened", &["a", "b"]);
    let (ctx, mut app) = start(&e, Some("a"));
    edit_temporal_off(&ctx, &mut app, &e);
    app.save_project().unwrap();
    drop(app);
    let (ctx, mut app) = start(&e, None);
    batch(&ctx, &mut app, &e, "24/12");
    app.ai_mut().batch_dialog_reset_to_preset(&ctx, "a"); // "Use preset values" on shot a
    pump(&ctx, &mut app, "rebuilt", |a| a.ai().batch_ready() && a.ai().batch_row_settings("a").is_some_and(|r| r.0.temporal_window == "24/12"));
    assert!(!app.ai().batch_summary().join("").contains("shot overrides"), "no override left");
    app.ai_mut().cancel_batch();
    frames(&ctx, &mut app, 3);
    assert!(app.unsaved(), "the reset is an unsaved change the app knows about");
    app.save_project().unwrap();
    drop(app);
    let raw: serde_json::Value = serde_json::from_slice(&fs::read(e.project.join("clips/a/clip.json")).unwrap()).unwrap();
    assert_eq!(raw["ck_native"]["draft"]["params"]["temporal_window"], "24/12");
    assert!(raw["ck_native"]["draft"].get("explicit").is_none());
    let (_ctx, mut app) = start(&e, None);
    let (proj, a) = fresh_clip(&e, "a");
    let d = app.ai_mut().draft_mut(&a, &proj, &Settings::default());
    assert_eq!((d.params.temporal_window.as_str(), d.explicit.len()), ("24/12", 0));
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}
