//! Board queue display: real pipelines queued through `App` + scheduler + fake worker, read back through the
//! grouped view and one drawn frame. Temp projects only; run through tools/isolated-test.sh.
mod common;
use ck::hints::{Final, HintCfg, Producer};
use ck::presets::{Delivery, Pipeline, PipelineExport, VideoDelivery};
use ck::project::{load_project, LoadOptions};
use ck::settings::Settings;
use ck::ui::ai::QueueSection;
use ck::ui::{App, AppOptions};
use ck::workflow::{OutputCfg, Params};
use eframe::egui::{self, epaint::Shape, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

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

/// `modes` = fake worker behaviour per task kind ("key" for Key jobs), e.g. `{"gvm": "hold"}`.
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

fn frame(ctx: &egui::Context, app: &mut App) -> egui::FullOutput {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 900.0))), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    out
}
fn pump(ctx: &egui::Context, app: &mut App, what: &str, mut done: impl FnMut(&App) -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !done(app) {
        assert!(Instant::now() < end, "timed out: {what}");
        frame(ctx, app);
        std::thread::sleep(Duration::from_millis(15));
    }
}
fn open(e: &Env) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), ..Default::default() });
    for _ in 0..12 {
        frame(&ctx, &mut app);
    }
    (ctx, app)
}

fn route(guide: Producer, finish: Final) -> HintCfg {
    let mut h = HintCfg::default();
    (h.guide, h.finish) = (guide, finish);
    h
}
fn two_videos() -> PipelineExport {
    let mut out = OutputCfg::default();
    out.fg_enabled = false;
    out.comp_enabled = false;
    let mut e = PipelineExport::for_outputs(&out);
    for p in ["matte", "processed"] {
        e.outputs.insert(p.into(), Delivery::Video(VideoDelivery { container: "mp4".into(), codec: "h264".into(), quality: 18 }));
    }
    e
}
fn pipeline(hint: HintCfg, mode: &str, export: PipelineExport) -> Pipeline {
    let mut outputs = OutputCfg::default();
    outputs.fg_enabled = false;
    outputs.comp_enabled = false;
    outputs.matte_format = "png".into();
    Pipeline { hint, params: Params { hint_mode: mode.into(), ..Params::default() }, outputs, location: None, export }
}
/// Queue the pipeline on `folders` straight away (no review click), one clip at a time like the Board does.
fn queue(ctx: &egui::Context, app: &mut App, e: &Env, folders: &[&str], p: &Pipeline) {
    let proj = load_project(&e.project, &LoadOptions::default()).unwrap();
    for f in folders {
        let i = proj.clips.iter().position(|c| c.folder == *f).unwrap();
        let before = app.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>();
        app.ai_mut().queue_pipeline(ctx, &proj, &Settings::default(), i, p);
        pump(ctx, app, "queued", |a| a.ai().pipeline_block_reason().is_some() || a.ai().queue_view().iter().map(|s| s.runs.len()).sum::<usize>() > before);
        assert_eq!(app.ai().pipeline_block_reason(), None, "{f}");
    }
}
/// "heading: status" lines of every run of a section.
fn lines(s: &QueueSection) -> Vec<Vec<String>> {
    s.runs.iter().map(|r| r.rows.iter().map(|x| format!("{}: {}", x.heading, x.cell.text)).collect()).collect()
}
fn screen_text(out: &egui::FullOutput) -> String {
    let mut t = String::new();
    for c in &out.shapes {
        if let Shape::Text(s) = &c.shape {
            t += &s.galley.text();
            t += "\n";
        }
    }
    t
}

#[test]
fn two_clip_batch_shows_each_title_once_with_ordered_stages_and_aggregate_export() {
    let e = env("gq-batch", &["a", "b"], json!({"gvm": "hold"}));
    let (ctx, mut app) = open(&e);
    queue(&ctx, &mut app, &e, &["a", "b"], &pipeline(route(Producer::Gvm, Final::Matanyone2), "full", two_videos()));
    pump(&ctx, &mut app, "gvm running", |a| a.ai().queue_view()[0].runs[0].rows[0].cell.text.ends_with('%'));
    let v = app.ai().queue_view();
    assert_eq!(v.iter().map(|s| s.title.as_str()).collect::<Vec<_>>(), ["a", "b"], "one title per clip, queue order");
    assert_eq!(
        lines(&v[0]),
        [["Pre-matte · GVM: 0%", "Hint · MatAnyone2: Waiting", "Keying: Waiting", "Export: Waiting"]]
    );
    // Clip b's first step queues behind a on the single worker; its later steps wait on b's own chain.
    assert_eq!(lines(&v[1]), [["Pre-matte · GVM: Queued", "Hint · MatAnyone2: Waiting", "Keying: Waiting", "Export: Waiting"]]);
    let export = &v[0].runs[0].rows[3];
    assert_eq!((export.ids.len(), export.products.iter().map(|p| p.0.as_str()).collect::<Vec<_>>()), (2, vec!["Matte", "Processed"]));
    // Drawn once: the title is a single label and the stage lines are on screen with an inline Cancel.
    let text = screen_text(&frame(&ctx, &mut app));
    // One stage line per clip, not per job (the Board card also shows the clip name, so titles are checked in the view).
    assert_eq!(text.lines().filter(|l| *l == "Export · Matte").count(), 0, "two exports aggregate into one Export row");
    assert_eq!(text.lines().filter(|l| *l == "Pre-matte · GVM").count(), 2, "{text}");
    for want in ["Hint · MatAnyone2", "Keying", "Export", "Cancel", "Details", "Matte Waiting · Processed Waiting"] {
        assert!(text.lines().any(|l| l == want), "{want} missing in:\n{text}");
    }
    assert!(!text.contains("Video export") && !text.contains("GVM hint"), "no raw interleaved task list:\n{text}");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn routes_show_only_their_real_stages() {
    let cases: Vec<(&str, Pipeline, Vec<&str>)> = vec![
        ("direct", pipeline(route(Producer::Birefnet, Final::Direct), "full", pipeline_seq()), vec!["Pre-matte · BiRefNet", "Hint · Use pre-matte", "Keying"]),
        ("refiner", pipeline(route(Producer::Birefnet, Final::Videomama), "full", pipeline_seq()), vec!["Pre-matte · BiRefNet", "Hint · VideoMaMa", "Keying"]),
        ("nohint", pipeline(route(Producer::Gvm, Final::Matanyone2), "none", two_videos()), vec!["Keying", "Export"]),
        ("oneexport", pipeline(route(Producer::Gvm, Final::Matanyone2), "none", one_video()), vec!["Keying", "Export · Matte"]),
    ];
    for (tag, p, want) in cases {
        let e = env(&format!("gq-{tag}"), &["a"], json!({"birefnet": "hold", "key": "hold"}));
        let (ctx, mut app) = open(&e);
        queue(&ctx, &mut app, &e, &["a"], &p);
        let v = app.ai().queue_view();
        let got: Vec<String> = v[0].runs[0].rows.iter().map(|r| r.heading.clone()).collect();
        assert_eq!(got, want, "{tag}");
        drop(app);
        let _ = fs::remove_dir_all(e.base);
    }
}
fn one_video() -> PipelineExport {
    let mut e = pipeline_seq();
    e.outputs.insert("matte".into(), Delivery::Video(VideoDelivery { container: "mp4".into(), codec: "h264".into(), quality: 18 }));
    e
}
fn pipeline_seq() -> PipelineExport {
    let mut out = OutputCfg::default();
    out.fg_enabled = false;
    out.comp_enabled = false;
    PipelineExport::for_outputs(&out)
}

#[test]
fn manual_prematte_is_precomputed_without_a_job_and_the_rest_runs() {
    let e = env("gq-manual", &["a"], json!({"key": "hold"}));
    let g = e.project.join("clips/a/VideoMamaMaskHint");
    fs::create_dir_all(&g).unwrap();
    write_png(&g.join("f_1101.png"));
    let (ctx, mut app) = open(&e);
    queue(&ctx, &mut app, &e, &["a"], &pipeline(route(Producer::Sam, Final::Matanyone2), "full", pipeline_seq()));
    let v = app.ai().queue_view();
    let rows = &v[0].runs[0].rows;
    assert_eq!(rows[0].heading, "Pre-matte · SAM");
    assert_eq!((rows[0].cell.text.as_str(), rows[0].ids.len(), rows[0].precomputed), ("Precomputed", 0, true));
    assert_eq!(rows.iter().map(|r| r.heading.as_str()).collect::<Vec<_>>(), ["Pre-matte · SAM", "Hint · MatAnyone2", "Keying"]);
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn a_new_run_of_the_same_clip_is_separate_from_the_cancelled_one_and_failures_are_not_complete() {
    let e = env("gq-runs", &["a"], json!({"gvm": "hold"}));
    let (ctx, mut app) = open(&e);
    let p = pipeline(route(Producer::Gvm, Final::Matanyone2), "full", pipeline_seq());
    queue(&ctx, &mut app, &e, &["a"], &p);
    pump(&ctx, &mut app, "running", |a| a.ai().queue_view()[0].runs[0].rows[0].cell.text.ends_with('%'));
    // Cancel through the drawn row's own Cancel path (the scheduler call it makes).
    let ids: Vec<String> = app.ai().queue_view()[0].runs[0].rows.iter().flat_map(|r| r.ids.clone()).collect();
    for id in &ids {
        app.ai_mut().cancel(id);
    }
    pump(&ctx, &mut app, "ended", |a| !a.ai().queue_view()[0].runs[0].active);
    let first = lines(&app.ai().queue_view()[0]);
    assert!(first[0][0].starts_with("Pre-matte · GVM: Cancelled"), "{first:?}");
    assert!(first[0].iter().all(|l| !l.ends_with("100%")), "nothing claimed complete: {first:?}");
    queue(&ctx, &mut app, &e, &["a"], &p);
    let v = app.ai().queue_view();
    assert_eq!((v.len(), v[0].runs.len()), (1, 2), "same clip root, two runs");
    assert!(v[0].runs[0].rows[0].cell.bad && !v[0].runs[1].rows[0].cell.bad, "the old run does not leak into the new one");
    assert!(v[0].runs[1].rows.iter().all(|r| r.cell.text != "Cancelled" && r.cell.text != "100%"));
    assert!(!v[0].runs[0].rows[0].attention, "a finished cancellation no longer forces Details open");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}

#[test]
fn same_folder_name_in_two_projects_stays_two_clip_groups() {
    let a = env("gq-root-a", &["c"], json!({"key": "hold"}));
    let b = env("gq-root-b", &["c"], json!({"key": "hold"}));
    let (ctx, mut app) = open(&a);
    let p = pipeline(HintCfg::default(), "none", pipeline_seq());
    queue(&ctx, &mut app, &a, &["c"], &p);
    app.open_project_for_test(&ctx, &b.project);
    for _ in 0..12 {
        frame(&ctx, &mut app);
    }
    queue(&ctx, &mut app, &b, &["c"], &p);
    let v = app.ai().queue_view();
    assert_eq!(v.len(), 2, "grouped by clip root, not by folder label");
    assert_ne!(v[0].root, v[1].root);
    drop(app);
    for e in [a, b] {
        let _ = fs::remove_dir_all(e.base);
    }
}

#[test]
fn a_failed_first_stage_fails_the_chain_and_never_reads_complete() {
    let e = env("gq-fail", &["a"], json!({"gvm": "fail"}));
    let (ctx, mut app) = open(&e);
    queue(&ctx, &mut app, &e, &["a"], &pipeline(route(Producer::Gvm, Final::Matanyone2), "full", two_videos()));
    pump(&ctx, &mut app, "ended", |a| !a.ai().queue_view()[0].runs[0].active);
    let rows = app.ai().queue_view()[0].runs[0].rows.clone();
    assert!(rows.iter().all(|r| r.cell.text == "Failed" && r.cell.bad && r.cell.frac < 1.0), "{rows:?}");
    let text = screen_text(&frame(&ctx, &mut app));
    assert!(text.lines().any(|l| l == "Details"), "{text}");
    drop(app);
    let _ = fs::remove_dir_all(e.base);
}
