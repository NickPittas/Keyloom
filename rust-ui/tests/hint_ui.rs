//! Every page renders with the new Hint/Deliver controls, and Media edits are saved as they happen.
mod common;
use ck::ui::{App, AppOptions, Tab};
use common::*;
use eframe::egui::{self, RawInput, Rect, Vec2};
use serde_json::{json, Value};

fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
    }
}

fn png(p: &std::path::Path) {
    let f = std::fs::File::create(p).unwrap();
    let mut e = png::Encoder::new(f, 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header()
        .unwrap()
        .write_image_data(&[7; 48])
        .unwrap();
}

#[test]
fn an_owned_running_hint_lock_is_not_presented_as_a_recovery_warning() {
    let base = tmp("owned-hint-lock");
    let root = base.join("proj");
    let clip_root = root.join("clips/c");
    std::fs::create_dir_all(clip_root.join("Frames")).unwrap();
    png(&clip_root.join("Frames/f_1.png"));
    std::fs::write(clip_root.join("clip.json"), "{}").unwrap();
    std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = make_repo(&base, "worker", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"local","kind":"local","repo":repo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions {
        project: Some(root), settings_path: Some(settings), open_clip: Some("c".into()), tab: Some(Tab::Key), ..Default::default()
    });
    let clip = app.clip_for_test();
    let cfg = ck::hints::HintCfg::default();
    let mut job = ck::hints::Ctx {
        clip: &clip, range: (0, 0), job_id: "owned-hint".into(), staging: base.join("stage")
    }.producer(&cfg, &ck::hints::Annotations::default(), false, ck::hints::GUIDE_DIR).unwrap();
    job.params["fake"] = json!("hold");
    app.ai_mut().submit_task(&clip, job, vec![], 0).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !matches!(app.ai().scheduler().state_of("owned-hint"), Some(ck::scheduler::JobState::Running { .. })) {
        frames(&ctx, &mut app, 1);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let lock = clip_root.join(".corridorkey_hint.lock");
    std::fs::write(&lock, json!({"job_id":"owned-hint","pid":123,"host":"fakehost"}).to_string()).unwrap();
    let mut out = ctx.run_ui(RawInput::default(), |ui| app.ai_mut().hint_lock_ui(ui, &clip));
    fn text(shape: &egui::Shape, output: &mut Vec<String>) {
        match shape {
            egui::Shape::Text(t) => output.push(t.galley.text().to_string()),
            egui::Shape::Vec(v) => v.iter().for_each(|s| text(s, output)),
            _ => {}
        }
    }
    let mut labels = vec![];
    for s in &out.shapes { text(&s.shape, &mut labels); }
    out.textures_delta.clear();
    assert!(!labels.iter().any(|s| s.contains("Hint lock") || s.contains("Confirm")), "{labels:?}");
    assert!(lock.exists(), "a normal running lock must stay intact");
    app.ai_mut().cancel("owned-hint");
    while app.ai().scheduler().state_of("owned-hint").is_some_and(|s| !s.is_terminal()) {
        frames(&ctx, &mut app, 1);
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

#[test]
fn pages_render_and_in_out_and_colour_are_saved_immediately() {
    let base = tmp("ui");
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..6 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), r#"{"keep":{"x":1}}"#).unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(settings),
            open_clip: Some("c1".into()),
            tab: Some(Tab::Hint),
            ..Default::default()
        },
    );
    frames(&ctx, &mut app, 20);
    // Edit In/Out and colour on the open viewer: both reach clip.json / the session file without a Save step.
    app.viewer_mut_for_test(|v| {
        v.range = Some((1, 4));
        v.linear = true;
    });
    frames(&ctx, &mut app, 5);
    let raw: Value =
        serde_json::from_slice(&std::fs::read(clip.join("clip.json")).unwrap()).unwrap();
    assert_eq!(raw["in_out_range"], json!({"in_point": 1, "out_point": 4}));
    assert_eq!(raw["ck_native"]["range_stems"]["in"], "f_1102");
    assert_eq!(raw["keep"]["x"], 1, "unknown keys survive");
    let sess: Value =
        serde_json::from_slice(&std::fs::read(proj.join(".corridorkey_session.json")).unwrap())
            .unwrap();
    assert_eq!(sess["clip_input_is_linear"]["c1"], true);
    for t in Tab::ALL {
        app.switch_tab(t);
        frames(&ctx, &mut app, 6);
    }
    // A fresh app on the same project restores both.
    let mut again = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(base.join("settings.json")),
            open_clip: Some("c1".into()),
            ..Default::default()
        },
    );
    frames(&ctx, &mut again, 10);
    let mut seen = None;
    again.viewer_mut_for_test(|v| seen = Some((v.range, v.linear)));
    assert_eq!(seen, Some((Some((1, 4)), true)));
}

#[test]
fn workspace_is_saved_and_restored_against_the_clips_that_exist_now() {
    let base = tmp("ws");
    let proj = base.join("proj");
    for name in ["c1", "c2"] {
        let clip = proj.join("clips").join(name);
        std::fs::create_dir_all(clip.join("Frames")).unwrap();
        for i in 0..6 {
            png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
        }
        std::fs::write(
            clip.join(".video_metadata.json"),
            r#"{"fps":24.0,"width":4,"height":4}"#,
        )
        .unwrap();
        std::fs::write(clip.join("clip.json"), "{}").unwrap();
    }
    std::fs::write(proj.join("project.json"), r#"{"version":2,"keep":1}"#).unwrap();
    let settings = base.join("settings.json");
    std::fs::write(&settings, r#"{"version":1}"#).unwrap();
    let opts = || AppOptions {
        project: Some(proj.clone()),
        settings_path: Some(settings.clone()),
        ..Default::default()
    };
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, opts());
    frames(&ctx, &mut app, 5);
    app.open_clip(&ctx, 1);
    app.switch_tab(Tab::Key);
    app.viewer_mut_for_test(|v| v.set_playhead(3));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    frames(&ctx, &mut app, 5);
    let raw: Value =
        serde_json::from_slice(&std::fs::read(proj.join("project.json")).unwrap()).unwrap();
    assert_eq!(raw["keep"], 1);
    assert_eq!(raw["ck_native"]["workspace"]["open"], "c2");
    assert_eq!(raw["ck_native"]["workspace"]["tab"], "key");
    assert_eq!(raw["ck_native"]["workspace"]["frame"], "f_1104");

    let mut again = App::with_ctx(&ctx, opts());
    frames(&ctx, &mut again, 8);
    assert_eq!(again.open_frame_count(), Some(6));
    let mut at = None;
    again.viewer_mut_for_test(|v| at = Some(v.playhead));
    assert_eq!(at, Some(3));
    assert!(again.in_workspace());

    // The saved clip no longer exists: the project opens on the Board, nothing is applied blindly.
    std::fs::remove_dir_all(proj.join("clips/c2")).unwrap();
    let mut gone = App::with_ctx(&ctx, opts());
    frames(&ctx, &mut gone, 8);
    assert!(!gone.in_workspace());
}

#[test]
fn companion_sequence_import_through_the_real_picker_and_import_path() {
    use ck::ui::picker::PickKind;
    let base = tmp("seqimp");
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    let incoming = base.join("matte");
    std::fs::create_dir_all(&incoming).unwrap();
    for i in 0..4 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
        png(&incoming.join(format!("f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let settings = base.join("settings.json");
    std::fs::write(&settings, r#"{"version":1}"#).unwrap();
    let ctx = egui::Context::default();
    let pick = incoming.clone();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(settings),
            open_clip: Some("c1".into()),
            tab: Some(Tab::Media),
            ..Default::default()
        },
        std::sync::Arc::new(move |k, _| {
            if k == PickKind::HintSequence {
                vec![pick.clone()]
            } else {
                vec![]
            }
        }),
    );
    frames(&ctx, &mut app, 5);
    assert!(app.request_pick(&ctx, PickKind::HintSequence));
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.sequence_import_plan().is_none() && std::time::Instant::now() < end {
        frames(&ctx, &mut app, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.sequence_import_plan(), Some(Ok(4)));
    app.start_sequence_import(&ctx);
    while app.sequence_import_plan().is_some() && std::time::Instant::now() < end {
        frames(&ctx, &mut app, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(
        app.media_note().is_some_and(|n| n.contains("Imported 4")),
        "{:?}",
        app.media_note()
    );
    assert!(clip.join("AlphaHint/f_1104.png").is_file());
    assert!(
        !std::fs::read_dir(clip.join("AlphaHint")).unwrap().any(|e| e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with('.'))
    );
}

#[test]
fn movie_relink_checks_identity_then_queues_validated_extraction_through_the_worker() {
    use ck::ui::picker::PickKind;
    let base = tmp("movie");
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Source")).unwrap(); // the copied movie is gone: no video, no Frames
    std::fs::write(
        clip.join("clip.json"),
        r#"{"source":{"filename":"a.mov","original_path":"/gone/a.mov"},"keep":1}"#,
    )
    .unwrap();
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4,"frame_count":6}"#,
    )
    .unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    // Import helper repo whose script answers probes (data only: run by /usr/bin/python3, never exec'd).
    let irepo = base.join("irepo");
    std::fs::create_dir_all(irepo.join("rust-ui/tools")).unwrap();
    std::fs::create_dir_all(irepo.join("backend/worker")).unwrap();
    std::fs::write(irepo.join("backend/worker/__main__.py"), "").unwrap();
    std::fs::write(
        irepo.join("rust-ui/tools/ck_import.py"),
        r#"import json, sys
src = [a.split("=", 1)[1] for a in sys.argv if a.startswith("--video=")][0]
w = 8 if "wrong" in src else 4
print(json.dumps({"event": "plan", "items": [{"source": src, "kind": "video", "status": "ok", "clip": "x", "info": {"fps": 24.0, "width": w, "height": 4, "frame_count": 6}}]}))
"#,
    )
    .unwrap();
    std::env::set_var("CK_PYTHON", PY);
    let wrepo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":wrepo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let good = base.join("new_a.mov");
    let wrong = base.join("wrong_a.mov");
    std::fs::write(&good, b"x").unwrap();
    std::fs::write(&wrong, b"x").unwrap();
    let choice = std::sync::Arc::new(std::sync::Mutex::new(wrong.clone()));
    let c2 = choice.clone();
    let ctx = egui::Context::default();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(settings),
            repo: Some(irepo),
            open_clip: Some("c1".into()),
            tab: Some(Tab::Media),
            ..Default::default()
        },
        std::sync::Arc::new(move |k, _| {
            if k == PickKind::RelinkMovie {
                vec![c2.lock().unwrap().clone()]
            } else {
                vec![]
            }
        }),
    );
    frames(&ctx, &mut app, 5);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let wait = |app: &mut App, f: &dyn Fn(&App) -> bool| {
        while !f(app) && std::time::Instant::now() < end {
            frames(&ctx, app, 1);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    };
    // A different movie (wrong size) is refused and nothing is written.
    assert!(app.request_pick(&ctx, PickKind::RelinkMovie));
    wait(&mut app, &|a| a.movie_relink_verdict().is_some());
    assert!(matches!(app.movie_relink_verdict(), Some(Err(e)) if e.contains("different source")));
    assert!(app.extract_relinked_movie().is_err());
    let raw: Value =
        serde_json::from_slice(&std::fs::read(clip.join("clip.json")).unwrap()).unwrap();
    assert_eq!(raw["source"]["original_path"], "/gone/a.mov");
    // The right movie: full identity match, then extraction is queued through the worker with an absolute path.
    *choice.lock().unwrap() = good.clone();
    wait(&mut app, &|a| !a.picking());
    // (starting a new pick replaces the old relink)
    assert!(app.request_pick(&ctx, PickKind::RelinkMovie));
    wait(&mut app, &|a| {
        matches!(a.movie_relink_verdict(), Some(Ok(_)))
    });
    assert_eq!(app.movie_relink_verdict(), Some(Ok(true)));
    wait(&mut app, &|a| {
        a.ai()
            .scheduler()
            .targets()
            .iter()
            .any(|t| t.hello.is_some())
    });
    app.extract_relinked_movie().unwrap();
    wait(&mut app, &|a| {
        a.ai()
            .scheduler()
            .task_snapshot()
            .iter()
            .all(|t| t.state.is_terminal())
            && !a.ai().scheduler().task_snapshot().is_empty()
    });
    let t = &app.ai().scheduler().task_snapshot()[0];
    assert_eq!(t.job.kind, ck::tasks::TaskKind::PrepareSource);
    assert_eq!(t.job.params["source_path"], good.to_str().unwrap());
    let raw: Value =
        serde_json::from_slice(&std::fs::read(clip.join("clip.json")).unwrap()).unwrap();
    assert_eq!(raw["source"]["original_path"], good.to_str().unwrap());
    assert_eq!(raw["keep"], 1);
}

#[test]
fn import_owns_the_hint_lock_and_gates_hint_jobs_and_a_movie_check_never_follows_to_another_clip() {
    use ck::ui::picker::PickKind;
    let base = tmp("gate");
    let proj = base.join("proj");
    for n in ["a", "b"] {
        let clip = proj.join("clips").join(n);
        std::fs::create_dir_all(clip.join("Source")).unwrap();
        std::fs::write(
            clip.join("clip.json"),
            r#"{"source":{"filename":"x.mov","original_path":"/gone/x.mov"}}"#,
        )
        .unwrap();
        std::fs::write(
            clip.join(".video_metadata.json"),
            r#"{"fps":24.0,"width":4,"height":4,"frame_count":6}"#,
        )
        .unwrap();
    }
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let irepo = base.join("irepo");
    std::fs::create_dir_all(irepo.join("rust-ui/tools")).unwrap();
    std::fs::create_dir_all(irepo.join("backend/worker")).unwrap();
    std::fs::write(irepo.join("backend/worker/__main__.py"), "").unwrap();
    std::fs::write(
        irepo.join("rust-ui/tools/ck_import.py"),
        r#"import json, sys
src = [a.split("=", 1)[1] for a in sys.argv if a.startswith("--video=")][0]
print(json.dumps({"event": "plan", "items": [{"source": src, "kind": "video", "status": "ok", "clip": "x", "info": {"fps": 24.0, "width": 4, "height": 4, "frame_count": 6}}]}))
"#,
    )
    .unwrap();
    std::env::set_var("CK_PYTHON", PY);
    let wrepo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":wrepo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let movie = base.join("m.mov");
    std::fs::write(&movie, b"x").unwrap();
    let ctx = egui::Context::default();
    let m2 = movie.clone();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(settings),
            repo: Some(irepo),
            open_clip: Some("a".into()),
            tab: Some(Tab::Media),
            ..Default::default()
        },
        std::sync::Arc::new(move |k, _| {
            if k == PickKind::RelinkMovie {
                vec![m2.clone()]
            } else {
                vec![]
            }
        }),
    );
    frames(&ctx, &mut app, 5);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    assert!(app.request_pick(&ctx, PickKind::RelinkMovie));
    while !matches!(app.movie_relink_verdict(), Some(Ok(_))) && std::time::Instant::now() < end {
        frames(&ctx, &mut app, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(app.movie_relink_verdict(), Some(Ok(true)));
    // Switch to clip B (same geometry): the check does not follow, Extract refuses, B's data is untouched.
    app.open_clip(&ctx, 1);
    frames(&ctx, &mut app, 3);
    assert_eq!(
        app.movie_relink_verdict(),
        None,
        "state is dropped when the clip changes"
    );
    assert!(app.extract_relinked_movie().is_err());
    for n in ["a", "b"] {
        let raw: Value = serde_json::from_slice(
            &std::fs::read(proj.join("clips").join(n).join("clip.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            raw["source"]["original_path"], "/gone/x.mov",
            "{n} untouched"
        );
    }
    assert!(app.ai().scheduler().task_snapshot().is_empty());
}

#[test]
fn import_refuses_while_a_hint_lock_exists_and_blocks_hint_jobs_while_it_runs() {
    use ck::ui::picker::PickKind;
    let base = tmp("lockgate");
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    let incoming = base.join("matte");
    std::fs::create_dir_all(&incoming).unwrap();
    for i in 0..3 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
        png(&incoming.join(format!("f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let wrepo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":wrepo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let pick = incoming.clone();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(proj.clone()),
            settings_path: Some(settings),
            open_clip: Some("c1".into()),
            tab: Some(Tab::Media),
            ..Default::default()
        },
        std::sync::Arc::new(move |k, _| {
            if k == PickKind::HintSequence {
                vec![pick.clone()]
            } else {
                vec![]
            }
        }),
    );
    frames(&ctx, &mut app, 5);
    assert!(app.request_pick(&ctx, PickKind::HintSequence));
    let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.sequence_import_plan().is_none() && std::time::Instant::now() < end {
        frames(&ctx, &mut app, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    // A queued/running Key job of the same clip blocks the import (task_busy alone would not see it).
    let mut k = job(&base, "k1", 2, "hold");
    k.clip_root = clip.clone();
    app.ai_mut().scheduler_mut().submit(ck::scheduler::Submit { job: k, placement: ck::scheduler::Placement::Auto, after: vec![], min_vram_gb: None }).unwrap();
    app.start_sequence_import(&ctx);
    assert!(app.media_note().is_some_and(|n| n.contains("running for this clip")), "{:?}", app.media_note());
    assert!(!clip.join("AlphaHint").exists() && !clip.join(".corridorkey_hint.lock").exists());
    app.ai_mut().scheduler_mut().cancel("k1");
    // A foreign lock (a hint job, possibly on another machine): the import must not start or write anything.
    std::fs::write(
        clip.join(".corridorkey_hint.lock"),
        r#"{"job_id":"job-x","host":"h","pid":1}"#,
    )
    .unwrap();
    app.start_sequence_import(&ctx);
    assert!(
        app.media_note().is_some_and(|n| n.contains("Not imported")),
        "{:?}",
        app.media_note()
    );
    assert!(!clip.join("AlphaHint").exists());
    assert_eq!(
        std::fs::read(clip.join(".corridorkey_hint.lock")).unwrap(),
        br#"{"job_id":"job-x","host":"h","pid":1}"#.to_vec()
    );
    std::fs::remove_file(clip.join(".corridorkey_hint.lock")).unwrap();
    // Running import: the clip is fenced against new hint jobs and Key until it ends.
    app.start_sequence_import(&ctx);
    assert!(app.ai().external_writer(&clip));
    let job = ck::hints::Ctx {
        clip: &ck::project::load_project(&proj, &ck::project::LoadOptions::default())
            .unwrap()
            .clips[0],
        range: (0, 2),
        job_id: "g1".into(),
        staging: base.join("stg"),
    }
    .producer(&ck::hints::HintCfg::default(), &ck::hints::Annotations::default(), false, "AlphaHint")
    .unwrap();
    let c = ck::project::load_project(&proj, &ck::project::LoadOptions::default())
        .unwrap()
        .clips
        .remove(0);
    assert!(app.ai_mut().submit_task(&c, job, vec![], 0).is_err());
    while app.ai().external_writer(&clip) && std::time::Instant::now() < end {
        frames(&ctx, &mut app, 1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(!app.ai().external_writer(&clip));
    assert!(!clip.join(".corridorkey_hint.lock").exists());
    assert!(clip.join("AlphaHint/f_1103.png").is_file());
}
