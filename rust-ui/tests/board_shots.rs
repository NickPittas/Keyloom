//! Board shots: drag a card to a new place (order saved in project.json, queued work follows it, running work does
//! not move) and "Remove shot…" (confirmation, refusal while queued, files kept unless the unticked option is
//! ticked, never outside the clip folder). Headless egui, real pointer events, temp projects only; run through
//! tools/isolated-test.sh.
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
    fs::write(project.join("project.json"), json!({"version": 2, "clips": clips}).to_string()).unwrap();
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
fn open(e: &Env, pref: &str) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() });
    theme::apply_preference(&ctx, pref);
    app.ai_mut().placement = ck::scheduler::Placement::Target("l0".into());
    frames(&ctx, &mut app, 12);
    (ctx, app)
}

fn btn(p: Pos2, pressed: bool) -> Vec<Event> {
    vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() }]
}
fn mv(p: Pos2) -> Vec<Event> {
    vec![Event::PointerMoved(p)]
}
fn click(ctx: &egui::Context, app: &mut App, p: Pos2) {
    frame_with(ctx, app, mv(p));
    frame_with(ctx, app, btn(p, true));
    frame_with(ctx, app, btn(p, false));
    frames(ctx, app, 3);
}
fn drag(ctx: &egui::Context, app: &mut App, from: Pos2, to: Pos2) {
    frame_with(ctx, app, mv(from));
    frame_with(ctx, app, mv(from));
    frame_with(ctx, app, btn(from, true));
    for k in 1..=8 {
        frame_with(ctx, app, mv(from + (to - from) * (k as f32 / 8.0)));
    }
    frame_with(ctx, app, btn(to, false));
    frames(ctx, app, 3);
}
fn texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(Rect, String)> {
    fn go(s: &Shape, out: &mut Vec<(Rect, String)>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| go(x, out)),
            Shape::Text(t) => out.push((t.galley.rect.translate(t.pos.to_vec2()), t.galley.job.text.clone())),
            _ => {}
        }
    }
    let mut out = vec![];
    shapes.iter().for_each(|c| go(&c.shape, &mut out));
    out
}
fn text_rect(shapes: &[egui::epaint::ClippedShape], t: &str) -> Rect {
    texts(shapes).into_iter().find(|(_, s)| s == t).unwrap_or_else(|| panic!("no text {t:?} on screen")).0
}
fn project_json(e: &Env) -> Value {
    serde_json::from_str(&fs::read_to_string(e.project.join("project.json")).unwrap()).unwrap()
}
fn order(app: &App) -> Vec<String> {
    app.board_folders()
}
fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn pipeline() -> Pipeline {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (Producer::Gvm, Final::Matanyone2);
    let mut outputs = OutputCfg::default();
    outputs.fg_enabled = false;
    outputs.comp_enabled = false;
    outputs.matte_format = "png".into();
    let export = PipelineExport::for_outputs(&outputs);
    Pipeline { hint: h, params: Params { hint_mode: "full".into(), ..Params::default() }, outputs, location: None, export }
}
fn queue(ctx: &egui::Context, app: &mut App, e: &Env, folder: &str) {
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    let i = proj.clips.iter().position(|c| c.folder == folder).unwrap();
    let before = app.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>();
    app.ai_mut().queue_pipeline(ctx, &proj, &Settings::default(), i, &pipeline());
    pump(ctx, app, "queued", |a| a.ai().pipeline_block_reason().is_some() || a.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>() > before);
    assert_eq!(app.ai().pipeline_block_reason(), None, "{folder}");
}

/// Drag card `from` onto the left half of card `onto`.
fn drag_card(ctx: &egui::Context, app: &mut App, from: usize, onto: usize) {
    let cards = app.board_cards().to_vec();
    let a = cards.iter().find(|c| c.0 == from).unwrap().2.center();
    let t = cards.iter().find(|c| c.0 == onto).unwrap().1;
    drag(ctx, app, a, Pos2::new(t.min.x + 12.0, t.center().y));
}

#[test]
fn dragging_the_third_shot_to_first_saves_the_order_and_it_reloads() {
    for pref in ["Dark", "Light"] {
        let e = env(&format!("bs-order-{pref}"), &["a", "b", "c"], json!({}));
        let (ctx, mut app) = open(&e, pref);
        assert_eq!(order(&app), strs(&["a", "b", "c"]));
        app.select_folders(&["a"]);
        frames(&ctx, &mut app, 2);
        assert_eq!(app.board_cards().len(), 3, "{pref}: three cards drawn");
        drag_card(&ctx, &mut app, 2, 0);
        assert_eq!(order(&app), strs(&["c", "a", "b"]), "{pref}: card 3 is now first");
        assert_eq!(app.selected_folders(), strs(&["a"]), "{pref}: the selection follows the shot, not the position");
        assert_eq!(project_json(&e)["clip_order"], json!(["c", "a", "b"]), "{pref}: saved in project.json");
        assert_eq!(project_json(&e)["clips"], json!(["a", "b", "c"]), "{pref}: nothing else in project.json is touched");
        // The cards are drawn in the new order too: the first card sits where the old first one did.
        let cards = app.board_cards().to_vec();
        assert!(cards[0].1.min.x < cards[1].1.min.x && cards[1].1.min.x < cards[2].1.min.x, "{pref}: drawn left to right in order");
        // A fresh app (and the plain loader) read the same order back.
        let p = load_project(&e.project, &LoadOptions::default()).unwrap();
        assert_eq!(p.clips.iter().map(|c| c.folder.as_str()).collect::<Vec<_>>(), ["c", "a", "b"]);
        drop(app);
        let (_ctx2, app2) = open(&e, pref);
        assert_eq!(order(&app2), strs(&["c", "a", "b"]), "{pref}: reloaded");
        // A shot imported later (not in clip_order) goes after the listed ones.
        let n = e.project.join("clips/z");
        fs::create_dir_all(n.join("Frames")).unwrap();
        write_png(&n.join("Frames/f_1101.png"));
        fs::write(n.join("clip.json"), "{}").unwrap();
        let p = load_project(&e.project, &LoadOptions::default()).unwrap();
        assert_eq!(p.clips.iter().map(|c| c.folder.as_str()).collect::<Vec<_>>(), ["c", "a", "b", "z"]);
        let _ = fs::remove_dir_all(e.base);
    }
}

#[test]
fn a_drop_on_the_same_place_changes_nothing_and_the_drop_line_shows_while_dragging() {
    let e = env("bs-noop", &["a", "b", "c"], json!({}));
    let (ctx, mut app) = open(&e, "Dark");
    let cards = app.board_cards().to_vec();
    let from = cards[1].2.center();
    // Mid-drag over card 0: a vertical drop line is painted in the gap before it, and the pointer carries the name.
    frame_with(&ctx, &mut app, mv(from));
    frame_with(&ctx, &mut app, btn(from, true));
    let over = Pos2::new(cards[0].1.min.x + 12.0, cards[0].1.center().y);
    let mut shapes = vec![];
    for k in 1..=6 {
        shapes = frame_with(&ctx, &mut app, mv(from + (over - from) * (k as f32 / 6.0)));
    }
    let mut lines = vec![];
    fn go(s: &Shape, out: &mut Vec<(Pos2, Pos2, f32)>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| go(x, out)),
            Shape::LineSegment { points, stroke } if (points[0].x - points[1].x).abs() < 0.01 => out.push((points[0], points[1], stroke.width)),
            _ => {}
        }
    }
    shapes.iter().for_each(|c| go(&c.shape, &mut lines));
    let gap = cards[0].1.min.x - 5.0;
    assert!(lines.iter().any(|(a, b, w)| (a.x - gap).abs() <= 1.0 && *w >= 2.5 && (b.y - a.y).abs() >= cards[0].1.height() - 1.0), "drop line before card 0 at x={gap}: {lines:?}");
    assert!(texts(&shapes).iter().any(|(_, t)| t == "b"), "the dragged shot's name follows the pointer");
    // Back onto its own place and release: no change, nothing written.
    let own = Pos2::new(cards[1].1.min.x + 12.0, cards[1].1.center().y);
    frame_with(&ctx, &mut app, mv(own));
    frame_with(&ctx, &mut app, btn(own, false));
    frames(&ctx, &mut app, 3);
    assert_eq!(order(&app), strs(&["a", "b", "c"]));
    assert!(project_json(&e).get("clip_order").is_none(), "an unchanged order is never written");
    let _ = fs::remove_dir_all(e.base);
}

/// Clip roots of the not-yet-started tasks, in the order the scheduler will start them.
fn queued_order(app: &App) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for t in app.ai().scheduler().task_snapshot().iter().filter(|t| t.state == ck::scheduler::JobState::Queued) {
        let n = t.job.clip_root.file_name().unwrap().to_string_lossy().into_owned();
        if out.last() != Some(&n) {
            out.push(n);
        }
    }
    out
}

#[test]
fn a_queued_batch_runs_in_the_new_order_and_running_work_never_moves() {
    let e = env("bs-queue", &["a", "b", "c"], json!({"gvm": "hold"}));
    let (ctx, mut app) = open(&e, "Dark");
    for f in ["a", "b", "c"] {
        queue(&ctx, &mut app, &e, f);
    }
    pump(&ctx, &mut app, "a running", |a| a.ai().queue_view()[0].runs[0].rows[0].cell.text.ends_with('%'));
    let running = |app: &App| -> Vec<String> {
        app.ai().scheduler().task_snapshot().iter().filter(|t| t.state != ck::scheduler::JobState::Queued && !t.state.is_terminal()).map(|t| t.job.clip_root.file_name().unwrap().to_string_lossy().into_owned()).collect()
    };
    assert_eq!(running(&app), ["a"], "a holds the only worker");
    assert_eq!(queued_order(&app), ["a", "b", "c"], "queued in the order they were sent (a's own later steps wait behind its running one)");
    drag_card(&ctx, &mut app, 2, 0); // c to the front of the Board
    assert_eq!(order(&app), strs(&["c", "a", "b"]));
    assert_eq!(queued_order(&app), ["a", "c", "b"], "the shots that have not started now follow the Board; a's own steps keep their place");
    assert_eq!(running(&app), ["a"], "the running shot is untouched");
    // The same work as the scheduler's own snapshot: a's running task is still the very first entry.
    let first = app.ai().scheduler().task_snapshot()[0].job.clip_root.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(first, "a");
    // Dragging the running shot itself changes only the Board, never the queue.
    drag_card(&ctx, &mut app, 1, 0); // a to the front
    assert_eq!(order(&app), strs(&["a", "c", "b"]));
    assert_eq!(queued_order(&app), ["a", "c", "b"]);
    assert_eq!(running(&app), ["a"]);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

/// A clip that has everything a removal could touch: frames extracted from a movie, hint folders, renders, and
/// files outside the clip that must never go.
fn rich_clip(e: &Env, name: &str) -> PathBuf {
    let c = e.project.join("clips").join(name);
    fs::create_dir_all(c.join("Source")).unwrap();
    fs::write(c.join("Source/movie.mp4"), b"original movie").unwrap();
    for d in ["AlphaHint", "VideoMamaMaskHint", "Output/Matte"] {
        fs::create_dir_all(c.join(d)).unwrap();
        write_png(&c.join(d).join("f_1101.png"));
    }
    c
}

#[test]
fn remove_shot_keeps_every_file_by_default_and_the_option_deletes_only_inside_the_clip() {
    for (pref, tick) in [("Dark", false), ("Light", true)] {
        let e = env(&format!("bs-remove-{pref}"), &["a", "b", "c"], json!({}));
        let b = rich_clip(&e, "b");
        // Outside the clip: another clip, a sequence the clip merely references, and a link out of the clip.
        let outside = e.base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("keep.txt"), "mine").unwrap();
        fs::write(b.join("clip.json"), json!({"source": {"type": "sequence", "original_path": outside}}).to_string()).unwrap();
        fs::remove_dir_all(b.join("VideoMamaMaskHint")).unwrap();
        std::os::unix::fs::symlink(&outside, b.join("VideoMamaMaskHint")).unwrap();
        let (ctx, mut app) = open(&e, pref);
        assert_eq!(order(&app), strs(&["a", "b", "c"]));
        // Remove is in the card menu on the new Board.
        frames(&ctx, &mut app, 2);
        let p = app.board_cards()[1].2.center();
        frame_with(&ctx, &mut app, mv(p));
        frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Default::default() }]);
        frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers: Default::default() }]);
        let menu = frames(&ctx, &mut app, 2);
        click(&ctx, &mut app, text_rect(&menu, "Remove shot…").center());
        assert!(app.remove_shot_open(), "{pref}: the confirmation opened");
        let shapes = frames(&ctx, &mut app, 3);
        assert!(texts(&shapes).iter().any(|(_, t)| t.starts_with("Remove b from this project")), "{pref}: names the shot");
        let opt = text_rect(&shapes, "also delete this shot's extracted frames, masks and renders");
        if tick {
            click(&ctx, &mut app, opt.center());
        }
        let ok = text_rect(&frames(&ctx, &mut app, 2), "Remove");
        click(&ctx, &mut app, ok.center());
        assert!(!app.remove_shot_open());
        assert_eq!(order(&app), strs(&["a", "c"]), "{pref}: gone from the list");
        let pj = project_json(&e);
        assert_eq!(pj["removed_clips"], json!(["b"]), "{pref}: recorded in project.json");
        assert_eq!(pj["clips"], json!(["a", "c"]), "{pref}: and off its clips list");
        let reload = load_project(&e.project, &LoadOptions::default()).unwrap();
        assert_eq!(reload.clips.iter().map(|c| c.folder.as_str()).collect::<Vec<_>>(), ["a", "c"], "{pref}: stays gone after a reload");
        // Files.
        let kept = |p: &str| b.join(p).exists();
        assert!(kept("clip.json") && kept("Source/movie.mp4"), "{pref}: clip.json and the original movie always stay");
        assert!(outside.join("keep.txt").exists(), "{pref}: nothing outside the project is touched, links are not followed");
        assert!(e.project.join("clips/a/Frames/f_1101.png").exists() && e.project.join("clips/c/Frames/f_1101.png").exists());
        if tick {
            assert!(!kept("AlphaHint") && !kept("Output") && !kept("Frames"), "{pref}: frames, masks and renders deleted");
            assert!(std::fs::symlink_metadata(b.join("VideoMamaMaskHint")).unwrap().file_type().is_symlink(), "{pref}: a link out of the clip is left alone");
        } else {
            assert!(kept("AlphaHint/f_1101.png") && kept("Output/Matte/f_1101.png") && kept("Frames/f_1101.png"), "{pref}: every file kept by default");
        }
        drop(app);
        let _ = fs::remove_dir_all(e.base);
    }
}

#[test]
fn remove_shot_is_refused_with_the_reason_while_the_shot_is_queued() {
    let e = env("bs-refuse", &["a", "b"], json!({"gvm": "hold"}));
    let (ctx, mut app) = open(&e, "Dark");
    queue(&ctx, &mut app, &e, "b");
    app.ask_remove_shot(1);
    let shapes = frames(&ctx, &mut app, 3);
    assert!(app.remove_shot_open());
    let t = texts(&shapes);
    assert!(t.iter().any(|(_, s)| s.contains("b is rendering or queued")), "the reason is shown: {t:?}");
    assert!(!t.iter().any(|(_, s)| s == "Remove"), "no Remove button while it is refused");
    click(&ctx, &mut app, text_rect(&shapes, "OK").center());
    assert!(!app.remove_shot_open());
    assert_eq!(order(&app), strs(&["a", "b"]));
    assert!(project_json(&e).get("removed_clips").is_none(), "nothing was written");
    // The same shot with nothing queued can be removed (a is idle).
    app.ask_remove_shot(0);
    frames(&ctx, &mut app, 3);
    let ok = text_rect(&frames(&ctx, &mut app, 2), "Remove");
    click(&ctx, &mut app, ok.center());
    assert_eq!(order(&app), strs(&["b"]));
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn shot_context_menu_shows_remove_and_disabled_reveal_entries() {
    let e = env("bs-context", &["a"], json!({}));
    let (ctx, mut app) = open(&e, "Dark");
    let card = app.board_cards()[0].2;
    let p = card.center();
    frame_with(&ctx, &mut app, mv(p));
    frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: true, modifiers: Default::default() }]);
    frame_with(&ctx, &mut app, vec![Event::PointerButton { pos: p, button: PointerButton::Secondary, pressed: false, modifiers: Default::default() }]);
    let menu = frames(&ctx, &mut app, 2);
    let labels = texts(&menu).into_iter().map(|(_, t)| t).collect::<Vec<_>>();
    for label in ["Remove shot…", "Reveal source", "Reveal exports"] { assert!(labels.iter().any(|t| t == label), "missing {label}: {labels:?}"); }
    click(&ctx, &mut app, text_rect(&menu, "Remove shot…").center());
    assert!(app.remove_shot_open());
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn key_selected_opens_the_real_batch_rows_and_cancel_clears_them() {
    let e = env("bs-batch-dialog", &["a", "b"], json!({}));
    let (ctx, mut app) = open(&e, "Dark");
    app.select_folders(&["a", "b"]);
    let board = frames(&ctx, &mut app, 2);
    let key = texts(&board).into_iter().find(|(_, text)| text.starts_with("Key selected (2)"))
        .unwrap_or_else(|| panic!("Key selected is not drawn: {:?}", texts(&board))).0;
    click(&ctx, &mut app, key.center());
    assert!(app.ai().batch_open(), "Key selected opens the production dialog");
    pump(&ctx, &mut app, "two planned Board rows", |a| a.ai().batch_ready());
    let dialog = app.ai_mut().batch_dialog().expect("the real batch is open");
    assert_eq!(dialog.rows.iter().map(|r| r.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
    let shapes = frames(&ctx, &mut app, 2);
    for shot in ["a", "b"] { assert!(texts(&shapes).iter().any(|(_, text)| text == shot), "real row {shot} is visible"); }
    let cancel = texts(&shapes).into_iter().find(|(_, text)| text.trim() == "Cancel")
        .unwrap_or_else(|| panic!("Cancel is not drawn: {:?}", texts(&shapes))).0;
    click(&ctx, &mut app, cancel.center());
    assert!(!app.ai().batch_open(), "Cancel clears the pending batch");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn key_selected_queues_the_valid_subset_when_another_shot_has_an_unknown_network_target() {
    let e = env("bs-batch-unknown-target", &["hold-a", "b"], json!({}));
    let (ctx, mut app) = open(&e, "Dark");
    let project = load_project(&e.project, &LoadOptions::default()).unwrap();
    let bad = project.clips.iter().find(|c| c.folder == "b").unwrap();
    app.ai_mut().set_clip_client(bad, Some("network:removed/pc")).unwrap();
    app.select_folders(&["hold-a", "b"]);
    let board = frames(&ctx, &mut app, 2);
    let key = texts(&board).into_iter().find(|(_, text)| text.starts_with("Key selected (2)"))
        .unwrap_or_else(|| panic!("Key selected is not drawn: {:?}", texts(&board))).0;
    click(&ctx, &mut app, key.center());
    pump(&ctx, &mut app, "two planned Board rows", |a| a.ai().batch_ready());

    let rows = app.ai_mut().batch_dialog().unwrap().rows;
    assert!(rows.iter().find(|r| r.name == "hold-a").unwrap().included);
    assert!(!rows.iter().find(|r| r.name == "b").unwrap().included);
    let bad = rows.iter().position(|r| r.name == "b").unwrap();
    app.ai_mut().batch_dialog_set_machine(bad, Some("l0"));
    app.ai_mut().batch_dialog_set_included(bad, true);
    assert!(app.ai_mut().batch_dialog().unwrap().rows[bad].included, "a valid reassignment can opt the row back in");
    app.ai_mut().batch_dialog_set_included(bad, false);
    let dialog = frames(&ctx, &mut app, 2);
    let queue = texts(&dialog).into_iter().find(|(_, text)| text.trim() == "Key 1 shots")
        .unwrap_or_else(|| panic!("Key 1 shots is not drawn: {:?}", texts(&dialog))).0;
    click(&ctx, &mut app, queue.center());

    let sections = app.ai().queue_view();
    assert_eq!(sections.len(), 1, "only the included shot entered the queue: {sections:?}");
    assert_eq!(sections[0].root.file_name().unwrap(), "hold-a");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}
