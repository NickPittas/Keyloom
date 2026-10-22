//! AI Key integration: real `App::frame` + real scheduler + a fake worker process (stdlib Python, no GPU, no
//! network, no SSH). Temp projects/settings only. Run through tools/isolated-test.sh.
use ck::project::{load_project, ClipState, LoadOptions, View};
use ck::scheduler::JobState;
use ck::ui::{App, AppOptions, Tab};
use ck::worker::Outcome;
use ck::workflow::*;
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const FAKE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/ai_fake_worker.py"
);

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

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
    log: PathBuf,
}

/// `clips` = (folder, frames, clip.json extras). Project root holds `project.json`.
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

fn app_for(e: &Env, ctx: &egui::Context, open: Option<&str>) -> App {
    App::with_ctx(
        ctx,
        AppOptions {
            project: Some(e.project.clone()),
            settings_path: Some(e.settings.clone()),
            open_clip: open.map(Into::into),
            ..Default::default()
        },
    )
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

/// Panels size themselves over a few frames; clicks come after the layout has settled.
fn settle(ctx: &egui::Context, app: &mut App) {
    for _ in 0..12 {
        run(ctx, app, vec![]);
    }
}

/// Wait for the background gate to match the current inputs, then let the layout settle.
fn gate_ready(ctx: &egui::Context, app: &mut App) {
    pump(ctx, app, "gate", |a| a.ai().gate_current());
    settle(ctx, app);
}

/// Hover one frame, then press and release: the way a real pointer reaches a widget inside a scroll area.
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

/// Open the batch review for the selected clips and confirm it (real click on the Queue button).
fn confirm_batch(ctx: &egui::Context, app: &mut App) {
    app.key_selected(ctx);
    pump(ctx, app, "batch review", |a| a.ai().batch_ready());
    settle(ctx, app);
    let r = app.ai().batch_confirm_rect.expect("Queue button drawn");
    click_on(ctx, app, r.center());
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
fn log(e: &Env) -> Vec<Value> {
    fs::read_to_string(&e.log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}
fn clip_json(e: &Env, c: &str) -> Value {
    serde_json::from_slice(&fs::read(e.project.join("clips").join(c).join("clip.json")).unwrap())
        .unwrap()
}

#[test]
fn key_button_runs_the_fake_worker_and_the_viewer_keeps_its_state() {
    let mut extra = matte_png();
    extra["future_field"] = json!({"keep": [1, 2]});
    let e = env(
        "click",
        &[("c", 6, extra)],
        json!({"version": 2, "unknown_project_key": true}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, Some("c"));
    app.switch_tab(Tab::Key);
    app.viewer_mut_for_test(|v| {
        v.range = Some((1, 4));
        v.set_playhead(2);
        v.zoom = 2.0;
        v.ab = true;
        v.b_view = View::Input;
    });
    gate_ready(&ctx, &mut app);
    let rect = app.ai().key_rect.expect("Key clip button drawn");
    click_on(&ctx, &mut app, rect.center());
    pump(&ctx, &mut app, "job done", all_terminal);
    // Let the final refresh land.
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let st = states(&app);
    assert_eq!(st.len(), 1);
    assert!(complete(&st[0]), "{st:?}");
    // Real fake worker process ran exactly this job, for the frozen range 1..=4 only.
    let l = log(&e);
    assert_eq!(l.iter().filter(|x| x["event"] == "job").count(), 1);
    // Default contract templates: <output>/{name}_{version}/{output}/{name}_#### , numbered by source frame.
    let out = e.project.join("clips/c/Output/c_v001/Matte");
    assert!(out.join("c_1102.png").is_file() && out.join("c_1105.png").is_file());
    assert!(!out.join("c_1101.png").exists() && !out.join("c_1106.png").exists());
    // Viewer: results indexed from the manifest; playhead, range, zoom and A/B untouched.
    let v = app.viewer().unwrap();
    assert_eq!(v.index.view_frames(View::Matte), 4);
    assert_eq!(
        (v.playhead, v.range, v.zoom, v.ab, v.b_view),
        (2, Some((1, 4)), 2.0, true, View::Input)
    );
    // Persistence: ids, workflow, output values, result folder; unknown fields preserved; atomic (no temp left).
    let cj = clip_json(&e, "c");
    assert!(
        cj["clip_id"].as_str().is_some_and(|s| s.len() >= 32),
        "{cj}"
    );
    assert_eq!(cj["workflow"]["version"], 1);
    assert_eq!(cj["workflow"]["output_config"]["matte_format"], "png");
    assert_eq!(cj["future_field"]["keep"][1], 2);
    assert_eq!(cj["output_values"]["version"], 1);
    assert!(cj["key_result"]["manifest_dir"]
        .as_str()
        .unwrap()
        .ends_with("clips/c/Output/c_v001/Matte"));
    let pj: Value =
        serde_json::from_slice(&fs::read(e.project.join("project.json")).unwrap()).unwrap();
    assert_eq!(pj["unknown_project_key"], true);
    let leftovers: Vec<_> = fs::read_dir(e.project.join("clips/c"))
        .unwrap()
        .flatten()
        .filter(|d| d.file_name().to_string_lossy().contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    // A fresh load sees a result that is complete for its own 4-frame range.
    let p = load_project(&e.project, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    assert_eq!(c.state, ClipState::Complete);
    assert!(
        c.result_range_text().unwrap().contains("f_1102–f_1105"),
        "{:?}",
        c.result_range_text()
    );
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn live_publication_appears_before_the_job_finishes() {
    let e = env("live", &[("c", 6, matte_png())], json!({"version": 2}));
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, Some("c"));
    app.viewer_mut_for_test(|v| {
        v.range = Some((0, 5));
        v.set_playhead(3);
    });
    app.switch_tab(Tab::Key);
    gate_ready(&ctx, &mut app);
    let r = app.ai().key_rect.unwrap();
    click_on(&ctx, &mut app, r.center());
    let mut partial = false;
    pump(&ctx, &mut app, "done", |a| {
        let n = a.viewer().unwrap().index.view_frames(View::Matte);
        partial |= n > 0 && n < 6;
        all_terminal(a)
    });
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert_eq!(app.viewer().unwrap().index.view_frames(View::Matte), 6);
    assert_eq!(app.viewer().unwrap().playhead, 3);
    assert!(
        partial,
        "frames became visible while the worker was still running"
    );
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn batch_review_shows_paths_then_places_clips_and_offers_a_new_version_for_existing_results() {
    let mut blocked = matte_png();
    blocked["workflow"]["params"] = json!({"hint_mode": "full"}); // no AlphaHint on this clip
    let e = env(
        "batch",
        &[
            ("a", 3, matte_png()),
            ("b", 4, matte_png()),
            ("z", 2, blocked),
        ],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["a", "b", "z"]);
    app.key_selected(&ctx);
    pump(&ctx, &mut app, "review", |a| a.ai().batch_ready());
    // Reviewed BEFORE anything is queued: resolved destination per clip, the blocked one named.
    let sum = app.ai().batch_summary().join("\n");
    assert!(
        sum.contains("a: new") && sum.contains("clips/a/Output/a_v001/Matte"),
        "{sum}"
    );
    assert!(sum.contains("z: blocked: Needs hint"), "{sum}");
    assert!(states(&app).is_empty(), "review queues nothing");
    settle(&ctx, &mut app);
    let r = app.ai().batch_confirm_rect.unwrap();
    click_on(&ctx, &mut app, r.center());
    pump(&ctx, &mut app, "batch", all_terminal);
    let st = states(&app);
    assert_eq!(st.len(), 2);
    assert!(st.iter().all(complete), "{st:?}");
    let pngs = |c: &str, v: &str| {
        fs::read_dir(
            e.project
                .join("clips")
                .join(c)
                .join(format!("Output/{c}_{v}/Matte")),
        )
        .unwrap()
        .flatten()
        .filter(|f| f.file_name().to_string_lossy().ends_with(".png"))
        .count()
    };
    assert_eq!((pngs("a", "v001"), pngs("b", "v001")), (3, 4));
    assert!(
        !e.project.join("clips/z/Output").exists(),
        "a blocked clip writes nothing"
    );
    // Run it again: existing results are offered the next free version, never replaced or skipped silently.
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    app.select_folders(&["a", "b"]);
    app.key_selected(&ctx);
    pump(&ctx, &mut app, "review 2", |a| a.ai().batch_ready());
    let sum = app.ai().batch_summary().join("\n");
    assert!(sum.contains("existing") && sum.contains("a_v002"), "{sum}");
    let before = fs::read(e.project.join("clips/a/Output/a_v001/Matte/a_1101.png")).unwrap();
    settle(&ctx, &mut app);
    let r = app.ai().batch_confirm_rect.unwrap();
    click_on(&ctx, &mut app, r.center());
    pump(&ctx, &mut app, "batch 2", |a| {
        states(a).len() == 4 && all_terminal(a)
    });
    assert_eq!((pngs("a", "v002"), pngs("b", "v002")), (3, 4));
    assert_eq!(
        fs::read(e.project.join("clips/a/Output/a_v001/Matte/a_1101.png")).unwrap(),
        before
    );
    assert_eq!(clip_json(&e, "a")["output_values"]["version"], 2);
    // One scheduler, one worker process for the target.
    assert_eq!(log(&e).iter().filter(|x| x["event"] == "job").count(), 4);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn cancel_keeps_partial_frames_and_a_crashed_worker_is_lost() {
    let e = env(
        "cancel",
        &[("hold1", 5, matte_png()), ("crash1", 5, matte_png())],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["hold1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", |a| {
        states(a)
            .iter()
            .any(|s| matches!(s, JobState::Running { .. }))
    });
    let id = app.ai().job_ids()[0].clone();
    app.ai_mut().cancel(&id);
    pump(&ctx, &mut app, "cancelled", all_terminal);
    assert!(
        matches!(&states(&app)[0], JobState::Done { info, .. } if info.outcome == Outcome::Cancelled && !info.manifest_complete)
    );
    // Partial result: manifest incomplete, frame 1 on disk, clip is not Complete.
    let p = load_project(&e.project, &LoadOptions::default()).unwrap();
    let h = p.clips.iter().find(|c| c.folder == "hold1").unwrap();
    assert_ne!(h.state, ClipState::Complete);
    assert_eq!(h.manifest.as_ref().map(|m| m.complete), Some(false));
    assert!(e
        .project
        .join("clips/hold1/Output/hold1_v001/Matte/hold1_1101.png")
        .is_file());
    // Crash: the job is Lost; the local child is reaped so the writer is provably stopped.
    app.select_folders(&["crash1"]);
    confirm_batch(&ctx, &mut app);
    // Lost first (unproven, rendered with the waiting/confirm text), then the reaped child proves the writer gone.
    pump(&ctx, &mut app, "lost", |a| {
        states(a).iter().any(|s| matches!(s, JobState::Lost { .. }))
    });
    pump(&ctx, &mut app, "writer proven stopped", |a| {
        states(a).iter().any(|s| {
            matches!(
                s,
                JobState::Lost {
                    writer_stopped: true,
                    ..
                }
            )
        })
    });
    fs::remove_dir_all(e.base).unwrap();
}

// ---------------------------------------------------------------- gate (pure core, no workers)

fn load(e: &Env) -> ck::project::Project {
    load_project(&e.project, &LoadOptions::default()).unwrap()
}
fn inputs<'a>(
    p: &'a ck::project::Project,
    params: &'a Params,
    out: &'a OutputCfg,
    range: (usize, usize),
) -> Inputs<'a> {
    Inputs {
        clip: &p.clips[0],
        project_name: &p.display_name,
        location: p.output_location.as_ref(),
        params,
        out,
        range,
        digest: None,
    }
}
fn matte_only() -> OutputCfg {
    OutputCfg {
        fg_enabled: false,
        comp_enabled: false,
        processed_enabled: false,
        matte_format: "png".into(),
        ..Default::default()
    }
}
fn reasons(r: Result<Plan, Blocked>) -> String {
    match r {
        Ok(_) => "OK".into(),
        Err(b) => format!("{} {:?}", b.reasons.join("|"), b.existing),
    }
}

#[test]
fn gate_refuses_bad_range_outputs_and_hints_naming_the_problem() {
    let e = env("gate", &[("c", 6, json!({}))], json!({"version": 2}));
    let p = load(&e);
    let (params, out) = (Params::default(), matte_only());
    assert!(plan(&inputs(&p, &params, &out, (1, 4)), Intent::New).is_ok());
    assert!(
        reasons(plan(&inputs(&p, &params, &out, (4, 1)), Intent::New)).contains("re-set In/Out")
    );
    assert!(
        reasons(plan(&inputs(&p, &params, &out, (0, 9)), Intent::New)).contains("re-set In/Out")
    );
    let none = OutputCfg {
        matte_enabled: false,
        ..matte_only()
    };
    assert!(
        reasons(plan(&inputs(&p, &params, &none, (0, 1)), Intent::New))
            .contains("No outputs selected")
    );
    let full = Params {
        hint_mode: "full".into(),
        ..Default::default()
    };
    assert!(reasons(plan(&inputs(&p, &full, &out, (0, 1)), Intent::New)).contains("Needs hint"));
    let marks = Params {
        hint_mode: "keyframes".into(),
        ..Default::default()
    };
    assert!(
        reasons(plan(&inputs(&p, &marks, &out, (0, 1)), Intent::New))
            .contains("Needs keyframe marks")
    );
    let bad = Params {
        despill_strength: 2.0,
        ..Default::default()
    };
    assert!(
        reasons(plan(&inputs(&p, &bad, &out, (0, 1)), Intent::New)).contains("despill_strength")
    );
    // Full hint covering only part of the clip: the stems missing from the range are named.
    let hint = e.project.join("clips/c/AlphaHint");
    fs::create_dir_all(&hint).unwrap();
    for n in 1101..=1103 {
        write_png(&hint.join(format!("f_{n}.png")));
    }
    let p = load(&e);
    assert!(
        plan(&inputs(&p, &full, &out, (0, 2)), Intent::New).is_ok(),
        "hint covers the range even though the clip has more frames"
    );
    assert!(reasons(plan(&inputs(&p, &full, &out, (0, 4)), Intent::New)).contains("f_1104"));
    // A video AlphaHint cannot be range-staged: say what to do instead of failing in the worker.
    fs::remove_dir_all(&hint).unwrap();
    fs::create_dir_all(&hint).unwrap();
    fs::write(hint.join("hint.mp4"), b"x").unwrap();
    let p = load(&e);
    assert!(reasons(plan(&inputs(&p, &full, &out, (0, 2)), Intent::New))
        .contains("AlphaHint is a video"));
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn job_is_frozen_from_range_interpretation_and_templated_outputs() {
    let loc = json!({"root": "", "folder_template": "{name}_{resolution}_{date}_{version}", "file_template": "{name}_####", "date_format": "DD-MM-YYYY"});
    let e = env("frozen", &[("c", 6, json!({}))], json!({"version": 2}));
    let mut loc = loc;
    loc["root"] = json!(e.base.join("Masks"));
    let mut pj: Value =
        serde_json::from_slice(&fs::read(e.project.join("project.json")).unwrap()).unwrap();
    pj["output_location"] = loc;
    fs::write(e.project.join("project.json"), pj.to_string()).unwrap();
    let p = load(&e);
    let params = Params {
        input_is_linear: true,
        ..Default::default()
    };
    let out = matte_only();
    let pl = plan(&inputs(&p, &params, &out, (2, 4)), Intent::New).unwrap();
    let j = &pl.job;
    assert_eq!(
        (
            j.range.in_point,
            j.range.out_point,
            j.range.in_stem.as_str(),
            j.range.out_stem.as_str()
        ),
        (2, 4, "f_1103", "f_1105")
    );
    assert_eq!(j.params["input_is_linear"], true);
    let names: Vec<_> = j.outputs["matte"]
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["c_1103.png", "c_1104.png", "c_1105.png"]);
    let md = j.manifest_dir.to_string_lossy().into_owned();
    assert!(md.contains("Masks/c_4p_") && md.ends_with("_v001"), "{md}");
    assert_eq!(j.mode, ck::jobs::JobMode::New);
    // Changing the live inputs afterwards cannot change an already built job.
    let again = plan(&inputs(&p, &params, &out, (0, 5)), Intent::New).unwrap();
    assert_eq!(j.range.in_point, 2);
    assert_eq!(again.job.range.frame_count(), 6);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn existing_results_offer_resume_or_an_explicit_new_version_never_replace() {
    let e = env("exist", &[("c", 4, json!({}))], json!({"version": 2}));
    let mut pj: Value =
        serde_json::from_slice(&fs::read(e.project.join("project.json")).unwrap()).unwrap();
    pj["output_location"] = json!({"root": e.base.join("Masks"), "folder_template": "{name}_{version}", "file_template": "{name}_####"});
    fs::write(e.project.join("project.json"), pj.to_string()).unwrap();
    // Give the clip a stable id so a hand-written manifest can belong to it.
    fs::write(e.project.join("clips/c/clip.json"), r#"{"clip_id":"id-1"}"#).unwrap();
    let p = load(&e);
    let (params, out) = (Params::default(), matte_only());
    let first = plan(&inputs(&p, &params, &out, (0, 3)), Intent::New).unwrap();
    // Pretend a previous run: manifest + one file.
    let j = &first.job;
    fs::create_dir_all(&j.manifest_dir).unwrap();
    let files: Vec<String> = j.outputs["matte"]
        .iter()
        .map(|p| {
            p.strip_prefix(&j.manifest_dir)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let manifest = json!({"version": 3, "engine": "corridorkey-v2", "clip_id": "id-1", "enabled_outputs": ["matte"],
        "formats": {"fg": "exr", "matte": "png", "comp": "png", "processed": "exr"}, "exr_compression": "dwab",
        "params": j.params, "range": j.range, "files": {"matte": files}, "effective_profile": {"hann_chunk": 80}, "complete": false});
    fs::write(
        j.manifest_dir.join(".corridorkey_manifest.json"),
        manifest.to_string(),
    )
    .unwrap();
    write_png(&j.outputs["matte"][0]);
    let before = fs::read(&j.outputs["matte"][0]).unwrap();

    let blocked = plan(&inputs(&p, &params, &out, (0, 3)), Intent::New)
        .err()
        .unwrap();
    let ex = blocked
        .existing
        .expect("existing result is a choice, not a silent overwrite");
    assert!(ex.resume, "compatible manifest can resume");
    assert_eq!(ex.next_version, Some(2));
    let r = plan(&inputs(&p, &params, &out, (0, 3)), Intent::Resume).unwrap();
    assert_eq!(r.job.mode, ck::jobs::JobMode::Resume);
    // Different settings: not resumable, but a new version is still offered.
    let other = Params {
        despill_strength: 0.9,
        ..Default::default()
    };
    let ex = plan(&inputs(&p, &other, &out, (0, 3)), Intent::New)
        .err()
        .unwrap()
        .existing
        .unwrap();
    assert!(!ex.resume);
    assert!(
        reasons(plan(&inputs(&p, &other, &out, (0, 3)), Intent::Resume)).contains("Cannot resume")
    );
    let v2 = plan(&inputs(&p, &other, &out, (0, 3)), Intent::NewVersion).unwrap();
    assert!(
        v2.job.manifest_dir.to_string_lossy().ends_with("c_v002"),
        "{:?}",
        v2.job.manifest_dir
    );
    assert_eq!(v2.values["version"], 2);
    assert_eq!(
        fs::read(&j.outputs["matte"][0]).unwrap(),
        before,
        "planning never touches existing results"
    );
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn legacy_results_stay_and_the_default_location_writes_beside_them_as_a_new_version_folder() {
    let e = env("legacy", &[("c", 3, json!({}))], json!({"version": 2}));
    let out_dir = e.project.join("clips/c/Output");
    fs::create_dir_all(out_dir.join("Matte")).unwrap();
    fs::write(
        out_dir.join(".corridorkey_manifest.json"),
        r#"{"version":2,"engine":"corridorkey-v2"}"#,
    )
    .unwrap();
    fs::write(out_dir.join("Matte/f_1101.png"), b"old").unwrap();
    let p = load(&e);
    let (params, out) = (Params::default(), matte_only());
    let pl = plan(&inputs(&p, &params, &out, (0, 2)), Intent::New).unwrap();
    assert!(
        pl.job.manifest_dir.ends_with("Output/c_v001/Matte"),
        "{:?}",
        pl.job.manifest_dir
    );
    assert_eq!(fs::read(out_dir.join("Matte/f_1101.png")).unwrap(), b"old");
    // Nothing is assumed about the user's storage: the default root is the clip's own output folder.
    let d = default_location(&p.clips[0]).unwrap();
    assert!(d.root.ends_with("clips/c/Output") && !d.root.starts_with("/mnt"));
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn a_lock_from_an_earlier_session_blocks_and_only_explicit_recovery_releases_it() {
    let e = env("lock", &[("c", 3, json!({}))], json!({"version": 2}));
    let p = load(&e);
    let (params, out) = (Params::default(), matte_only());
    let first = plan(&inputs(&p, &params, &out, (0, 2)), Intent::New).unwrap();
    let dir = first.job.manifest_dir.clone();
    fs::create_dir_all(&dir).unwrap();
    let lock = dir.join(".corridorkey_job.lock");
    fs::write(
        &lock,
        json!({"job_id": "old-job", "host": "render9", "pid": 4242}).to_string(),
    )
    .unwrap();
    let temp = dir.join(".corridorkey_manifest.json.tmp.old-job");
    fs::write(&temp, b"half").unwrap();
    let b = plan(&inputs(&p, &params, &out, (0, 2)), Intent::New)
        .err()
        .unwrap();
    let l = b.lock.clone().expect("lock owner is surfaced");
    assert_eq!(
        (l.job_id.as_deref(), l.host.as_deref(), l.pid),
        (Some("old-job"), Some("render9"), Some(4242))
    );
    assert!(l.heartbeat_age.is_some());
    assert_eq!(
        b.leftovers,
        vec![temp.clone()],
        "the run's temp file is listed for review"
    );
    assert!(
        b.reasons[0].contains("render9") && !b.reasons[0].to_lowercase().contains("delete"),
        "{:?}",
        b.reasons
    );
    // Age never frees it: nothing changes by itself, however often the gate runs.
    assert!(plan(&inputs(&p, &params, &out, (0, 2)), Intent::New).is_err() && lock.exists());
    // A lock naming another job than the one confirmed is left alone.
    let r = ck::jobs::recover_orphan(&dir, "someone-else", true);
    assert!(!r.lock_removed && lock.exists() && temp.exists(), "{r:?}");
    // Explicit confirmation of the lock's own job: lock removed, temps KEPT unless asked.
    let r = ck::jobs::recover_orphan(&dir, "old-job", false);
    assert!(
        r.lock_removed && r.temps_left == vec![temp.clone()] && temp.exists(),
        "{r:?}"
    );
    assert!(plan(&inputs(&p, &params, &out, (0, 2)), Intent::New).is_ok());
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn editing_settings_never_blocks_the_ui_and_stale_gate_results_are_discarded() {
    let e = env("gate-bg", &[("c", 20, matte_png())], json!({"version": 2}));
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, Some("c"));
    app.switch_tab(Tab::Key);
    settle(&ctx, &mut app);
    pump(&ctx, &mut app, "first gate", |a| a.ai().gate_current());
    // Edit params: the old result is no longer current at once; frames stay fast while the check runs behind.
    let slow = |app: &mut App| {
        let t = Instant::now();
        run(&ctx, app, vec![]);
        t.elapsed()
    };
    app.viewer_mut_for_test(|v| v.range = Some((2, 9)));
    let t = slow(&mut app);
    assert!(
        !app.ai().gate_current(),
        "inputs changed, old result must not be shown as current"
    );
    assert!(t < Duration::from_millis(500), "UI frame took {t:?}");
    app.viewer_mut_for_test(|v| v.range = Some((3, 12))); // a newer edit while the first may still run
    gate_ready(&ctx, &mut app);
    // The shown job is for the LATEST range snapshot, never the discarded one.
    let rect = app.ai().key_rect.unwrap();
    click_on(&ctx, &mut app, rect.center());
    let id = app.ai().job_ids()[0].clone();
    let job = app.ai().scheduler().job(&id).unwrap().job.clone();
    assert_eq!((job.range.in_point, job.range.out_point), (3, 12));
    pump(&ctx, &mut app, "done", all_terminal);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn closing_with_a_running_job_asks_then_stops_gracefully_keeping_the_window_alive() {
    use eframe::egui::{ViewportCommand, ViewportEvent, ViewportId};
    let e = env("close", &[("slow1", 5, matte_png())], json!({"version": 2}));
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["slow1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", |a| {
        states(a)
            .iter()
            .any(|s| matches!(s, JobState::Running { .. }))
    });
    let frame = |app: &mut App, close: bool| -> Vec<ViewportCommand> {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1440.0, 900.0),
            )),
            ..Default::default()
        };
        if close {
            raw.viewports
                .entry(ViewportId::ROOT)
                .or_default()
                .events
                .push(ViewportEvent::Close);
        }
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
        out.viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    };
    // 1. Window close is cancelled and the user is asked.
    let cmds = frame(&mut app, true);
    assert!(
        cmds.iter()
            .any(|c| matches!(c, ViewportCommand::CancelClose)),
        "{cmds:?}"
    );
    assert_eq!(app.close_state(), ck::ui::CloseState::Prompt);
    for _ in 0..3 {
        frame(&mut app, false);
    }
    let [stop, keep, quit] = app.close_rects()[..] else {
        panic!("three buttons: {:?}", app.close_rects())
    };
    assert!(keep.width() > 0.0 && quit.width() > 0.0);
    // 2. "Stop workers, then quit": the window stays, shows Stopping, and every frame stays responsive while the
    //    fake worker takes 1.5 s to answer the stop.
    click_on(&ctx, &mut app, stop.center());
    assert_eq!(app.close_state(), ck::ui::CloseState::Stopping);
    let (mut worst, mut closed, t0) = (Duration::ZERO, false, Instant::now());
    let mut saw_stopping = false;
    while !closed {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "never closed: {:?} {:?} {:?}",
            app.ai().stopping_lines(),
            states(&app),
            app.close_state()
        );
        let t = Instant::now();
        let cmds = frame(&mut app, false);
        worst = worst.max(t.elapsed());
        saw_stopping |= app
            .ai()
            .stopping_lines()
            .iter()
            .any(|l| l.contains("stopping"));
        closed = cmds.iter().any(|c| matches!(c, ViewportCommand::Close));
        assert!(
            !closed || app.ai().all_stopped(),
            "closed while a worker was still alive"
        );
        std::thread::sleep(Duration::from_millis(15));
    }
    assert!(saw_stopping, "per-target Stopping status was visible");
    assert!(
        worst < Duration::from_millis(500),
        "a frame blocked for {worst:?}"
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(1000),
        "the window waited for the worker, not for a timer"
    );
    assert!(states(&app).iter().all(JobState::is_terminal));
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn missing_worker_setup_is_reported_with_the_fix_and_keying_is_refused() {
    let e = env("setup", &[("c", 3, matte_png())], json!({"version": 2}));
    fs::write(
        &e.settings,
        json!({"version": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": e.base.join("nope"), "python": null, "gpu": 0}]}).to_string(),
    )
    .unwrap();
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, Some("c"));
    app.switch_tab(Tab::Key);
    settle(&ctx, &mut app);
    pump(&ctx, &mut app, "gate", |a| a.ai().gate_current());
    settle(&ctx, &mut app);
    let t = app.ai().setup_text().join(" ");
    assert!(
        t.contains("not a CorridorKey checkout") && t.contains("Settings"),
        "{t}"
    );
    let r = app.ai().key_rect.unwrap();
    click_on(&ctx, &mut app, r.center());
    assert!(states(&app).is_empty(), "disabled: nothing queued");
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn v3_manifest_files_map_drives_index_and_completion_for_its_range() {
    let e = env("v3", &[("c", 5, json!({}))], json!({"version": 2}));
    let res = e.base.join("elsewhere/run_v001");
    fs::create_dir_all(&res).unwrap();
    for n in 1102..=1104 {
        write_png(&res.join(format!("shot_{n}.png"))); // names do not match the source stems
    }
    let files: Vec<String> = (1102..=1104).map(|n| format!("shot_{n}.png")).collect();
    let manifest = |complete: bool| {
        json!({"version": 3, "engine": "corridorkey-v2", "clip_id": "x", "enabled_outputs": ["matte"], "formats": {"matte": "png"},
        "range": {"in_point": 1, "out_point": 3, "in_stem": "f_1102", "out_stem": "f_1104"},
        "source_stems": ["f_1102", "f_1103", "f_1104"], "files": {"matte": files}, "complete": complete})
    };
    fs::write(
        e.project.join("clips/c/clip.json"),
        json!({"key_result": {"manifest_dir": res}}).to_string(),
    )
    .unwrap();
    fs::write(
        res.join(".corridorkey_manifest.json"),
        manifest(false).to_string(),
    )
    .unwrap();
    let p = load(&e);
    assert_ne!(
        p.clips[0].state,
        ClipState::Complete,
        "incomplete run is never Complete, whatever is on disk"
    );
    fs::write(
        res.join(".corridorkey_manifest.json"),
        manifest(true).to_string(),
    )
    .unwrap();
    let p = load(&e);
    let c = &p.clips[0];
    assert_eq!(c.state, ClipState::Complete);
    assert_eq!(c.completed_stems().len(), 3);
    let idx = ck::frames::FrameIndex::build(c);
    assert_eq!(idx.view_frames(View::Matte), 3);
    assert!(idx.view_has(View::Matte, "f_1103") && !idx.view_has(View::Matte, "f_1101"));
    assert_eq!(
        ck::project::readiness(c),
        "Complete for frames f_1102–f_1104 (3 of 5)"
    );
    // A missing file breaks completion (never decided by counting).
    fs::remove_file(res.join("shot_1103.png")).unwrap();
    assert_ne!(load(&e).clips[0].state, ClipState::Complete);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn ai_targets_roundtrip_in_settings_and_keep_unknown_keys() {
    use ck::settings::Store;
    let d = tmp("targets");
    let p = d.join("s.json");
    fs::write(&p, json!({"version": 1, "future": 1, "ai_targets": [{"id": "l0", "kind": "local", "repo": "/r", "gpu": 0}]}).to_string()).unwrap();
    let mut st = Store::load(Some(p.clone()));
    assert!(st.problem.is_none(), "{:?}", st.problem);
    let mut n = st.saved().clone();
    assert_eq!(n.ai_targets.len(), 1);
    n.ai_targets.push(n.ai_targets[0].clone());
    assert!(st.save(n.clone()).is_err(), "duplicate ids are refused");
    n.ai_targets.pop();
    n.playback.decode_workers = 2;
    st.save(n).unwrap();
    assert_eq!(st.saved().ai_targets.len(), 1);
    // A Windows SSH target with drive/UNC mappings is accepted; POSIX paths for a Windows worker are not.
    let win = |repo: &str| -> ck::worker::Target {
        serde_json::from_value(json!({"id": "w", "kind": "ssh", "os": "windows", "host": "win1", "user": null, "port": null,
            "python": r"C:\\CK\\.venv\\Scripts\\python.exe", "repo": repo, "ssh": null, "gpu": 0,
            "path_map": [["/mnt/NAS", r"Z:\\"], ["/mnt/NAS2", r"\\\\nas\\share"]]}))
        .unwrap()
    };
    let mut n = st.saved().clone();
    n.ai_targets.push(win("/opt/ck"));
    assert!(st.save(n.clone()).is_err());
    n.ai_targets[1] = win(r"C:\\CK");
    st.save(n).unwrap();
    assert_eq!(Store::load(Some(p.clone())).saved().ai_targets.len(), 2);
    let v: Value = serde_json::from_slice(&fs::read(&p).unwrap()).unwrap();
    assert_eq!(v["future"], 1);
    assert_eq!(v["ai_targets"][0]["id"], "l0");
    let _ = fs::remove_dir_all(d);
}

const FAKE_SSH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_ssh.sh");

fn manifest_dir_of(app: &App, id: &str) -> PathBuf {
    app.ai()
        .scheduler()
        .job(id)
        .unwrap()
        .job
        .manifest_dir
        .clone()
}

#[test]
fn a_lock_owned_by_an_active_job_of_this_session_cannot_be_released_from_the_gate() {
    let e = env(
        "lock-active",
        &[("hold1", 5, matte_png())],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["hold1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", |a| {
        states(a)
            .iter()
            .any(|s| matches!(s, JobState::Running { .. }))
    });
    let id = app.ai().job_ids()[0].clone();
    let dir = manifest_dir_of(&app, &id);
    let lock = dir.join(".corridorkey_job.lock");
    fs::write(
        &lock,
        json!({"job_id": id, "host": "fakehost", "pid": 1}).to_string(),
    )
    .unwrap();
    assert_eq!(app.ai().lock_owner(&id), ck::ui::ai::LockOwner::Session);
    // Even if a stale gate frame offered the button, the handler refuses a session-owned id.
    let r = app.ai_mut().release_lock(&dir, &id);
    assert!(r.is_err() && lock.exists(), "{r:?}");
    // A true orphan (unknown id) can still be released after the explicit confirmation.
    let ghost = e.base.join("ghost");
    fs::create_dir_all(&ghost).unwrap();
    fs::write(
        ghost.join(".corridorkey_job.lock"),
        json!({"job_id": "ghost-job", "host": "h"}).to_string(),
    )
    .unwrap();
    assert_eq!(
        app.ai().lock_owner("ghost-job"),
        ck::ui::ai::LockOwner::Unknown
    );
    assert!(app.ai_mut().release_lock(&ghost, "ghost-job").is_ok());
    assert!(!ghost.join(".corridorkey_job.lock").exists());
    app.ai_mut().cancel(&id);
    pump(&ctx, &mut app, "cancelled", all_terminal);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn a_lost_unproven_job_is_released_only_through_the_scheduler_confirm() {
    let e = env(
        "lock-lost",
        &[("crash1", 5, matte_png())],
        json!({"version": 2}),
    );
    let repo = e.base.join("repo");
    fs::write(
        &e.settings,
        json!({"version": 1, "ai_targets": [
            {"id": "l0", "kind": "local", "repo": repo, "python": "/usr/bin/python3", "gpu": 0},
            {"id": "r0", "kind": "ssh", "os": "linux", "host": "fakehost", "user": null, "port": null,
             "python": "/usr/bin/python3", "repo": repo, "ssh": FAKE_SSH, "gpu": 0, "path_map": [[e.base, e.base]]}]})
        .to_string(),
    )
    .unwrap();
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    // The local worker is the trusted model reference; only then may the (fake-ssh) remote run the job.
    app.ai_mut().scheduler_mut().probe(Some("l0"));
    pump(&ctx, &mut app, "local ready", |a| {
        a.ai().scheduler().reference().is_some()
    });
    app.ai_mut().placement = ck::scheduler::Placement::Target("r0".into());
    app.select_folders(&["crash1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "lost", |a| {
        states(a).iter().any(|s| matches!(s, JobState::Lost { .. }))
    });
    let id = app.ai().job_ids()[0].clone();
    assert!(
        matches!(
            &states(&app)[0],
            JobState::Lost {
                writer_stopped: false,
                ..
            }
        ),
        "an ssh client dying proves nothing: {:?}",
        states(&app)
    );
    let dir = manifest_dir_of(&app, &id);
    let lock = dir.join(".corridorkey_job.lock");
    fs::write(
        &lock,
        json!({"job_id": id, "host": "fakehost", "pid": 1}).to_string(),
    )
    .unwrap();
    assert_eq!(app.ai().lock_owner(&id), ck::ui::ai::LockOwner::Session);
    assert!(
        app.ai_mut().release_lock(&dir, &id).is_err() && lock.exists(),
        "no bypass of the dedicated confirm"
    );
    assert_eq!(
        app.ai().scheduler().needs_attention().len(),
        1,
        "it stays reachable"
    );
    // The dedicated confirm keeps fence and proof coherent and does the cleanup.
    let rec = app
        .ai_mut()
        .scheduler_mut()
        .confirm_writer_stopped(&id)
        .expect("recovery runs");
    assert!(rec.lock_removed && !lock.exists(), "{rec:?}");
    assert_eq!(app.ai().lock_owner(&id), ck::ui::ai::LockOwner::Ended);
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn saved_targets_wait_for_running_jobs_then_apply_by_themselves() {
    let e = env(
        "pending-targets",
        &[("hold1", 5, matte_png())],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["hold1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "running", |a| {
        states(a)
            .iter()
            .any(|s| matches!(s, JobState::Running { .. }))
    });
    // Save a different target list while the job runs (what Apply in Settings does).
    let mut st = app.settings_page().saved().clone();
    let mut t = st.ai_targets[0].clone();
    t.id = "l1".into();
    st.ai_targets.push(t);
    app.settings_page().draft_mut().ai_targets = st.ai_targets.clone();
    assert!(app.settings_page().apply(), "settings saved");
    app.ai_mut().set_targets(&st, None);
    assert!(
        app.ai().targets_pending() && app.ai().scheduler().targets().len() == 1,
        "not applied under a running job"
    );
    assert!(app
        .ai()
        .pending_note()
        .unwrap()
        .contains("apply automatically"));
    let id = app.ai().job_ids()[0].clone();
    app.ai_mut().cancel(&id);
    pump(&ctx, &mut app, "applied", |a| {
        a.ai().scheduler().targets().len() == 2
    });
    assert!(!app.ai().targets_pending());
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn a_cancelled_review_is_discarded_and_an_invalid_stored_in_out_blocks_its_row() {
    let mut bad = matte_png();
    bad["in_out_range"] = json!({"in_point": 2, "out_point": 99});
    let e = env(
        "batch-bad",
        &[("a", 4, matte_png()), ("z", 4, bad)],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["a", "z"]);
    app.key_selected(&ctx);
    app.ai_mut().cancel_batch(); // before the background answer arrives
    for _ in 0..30 {
        run(&ctx, &mut app, vec![]);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !app.ai().batch_open() && states(&app).is_empty(),
        "late result must not resurrect the review"
    );
    app.key_selected(&ctx);
    pump(&ctx, &mut app, "review", |a| a.ai().batch_ready());
    let sum = app.ai().batch_summary().join("\n");
    assert!(
        sum.contains("z: blocked") && sum.contains("does not fit"),
        "never silently the whole clip: {sum}"
    );
    assert!(sum.contains("a: new"));
    fs::remove_dir_all(e.base).unwrap();
}

/// Core invariant for the target swap: while ANY old worker is still stopping, the old scheduler stays.
fn assert_old_scheduler_while_stopping(app: &App) {
    let t = app.ai().scheduler().targets();
    assert!(
        !t.iter()
            .any(|x| x.status == ck::scheduler::TargetStatus::Stopping)
            || t.len() == 1,
        "targets swapped while an old worker was still stopping: {t:?}"
    );
}

fn with_second_target(app: &mut App) -> ck::settings::Settings {
    let mut st = app.settings_page().saved().clone();
    let mut t = st.ai_targets[0].clone();
    t.id = "l1".into();
    st.ai_targets.push(t);
    st
}

#[test]
fn pending_targets_and_close_wait_for_the_observed_exit_of_a_slow_worker() {
    use eframe::egui::{ViewportCommand, ViewportEvent, ViewportId};
    let e = env(
        "late-exit",
        &[("late1", 3, matte_png())],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["late1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "job done", all_terminal); // `done` is sent; the process is still alive
    let st = with_second_target(&mut app);
    app.ai_mut().set_targets(&st, None);
    assert!(app.ai().targets_pending());
    assert_eq!(
        app.ai().scheduler().targets().len(),
        1,
        "direct path must not swap under a live worker either"
    );
    let frame = |app: &mut App, close: bool| -> Vec<ViewportCommand> {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1440.0, 900.0),
            )),
            ..Default::default()
        };
        if close {
            raw.viewports
                .entry(ViewportId::ROOT)
                .or_default()
                .events
                .push(ViewportEvent::Close);
        }
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
        out.viewport_output
            .get(&ViewportId::ROOT)
            .map(|v| v.commands.clone())
            .unwrap_or_default()
    };
    let t0 = Instant::now();
    let mut first = frame(&mut app, true); // the user closes while the old worker is winding down
    let (mut closed, mut stopping_seen) = (false, false);
    loop {
        assert!(t0.elapsed() < Duration::from_secs(20), "never closed");
        assert_old_scheduler_while_stopping(&app);
        stopping_seen |= app
            .ai()
            .scheduler()
            .targets()
            .iter()
            .any(|t| t.status == ck::scheduler::TargetStatus::Stopping);
        if first
            .iter()
            .chain(std::iter::empty())
            .any(|c| matches!(c, ViewportCommand::Close))
        {
            closed = true;
        }
        if closed {
            break;
        }
        std::thread::sleep(Duration::from_millis(15));
        first = frame(&mut app, false);
    }
    assert!(
        stopping_seen,
        "the old worker's Stopping status was visible"
    );
    assert!(
        t0.elapsed() >= Duration::from_millis(1000),
        "the window waited for the real exit, not for a timer: {:?}",
        t0.elapsed()
    );
    fs::remove_dir_all(e.base).unwrap();
}

#[test]
fn a_force_stopped_idle_worker_blocks_the_target_swap_until_it_has_exited() {
    let e = env(
        "force-idle",
        &[("late1", 3, matte_png())],
        json!({"version": 2}),
    );
    let ctx = egui::Context::default();
    let mut app = app_for(&e, &ctx, None);
    app.select_folders(&["late1"]);
    confirm_batch(&ctx, &mut app);
    pump(&ctx, &mut app, "job done", all_terminal);
    let st = with_second_target(&mut app);
    app.ai_mut().scheduler_mut().force_stop("l0"); // no job; the process is dying
    app.ai_mut().set_targets(&st, None);
    pump(&ctx, &mut app, "swapped after exit", |a| {
        assert_old_scheduler_while_stopping(a);
        a.ai().scheduler().targets().len() == 2
    });
    assert!(!app.ai().targets_pending());
    fs::remove_dir_all(e.base).unwrap();
}
