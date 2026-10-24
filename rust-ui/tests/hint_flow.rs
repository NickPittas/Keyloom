//! Generate hint -> Key chain through the real AiUi/scheduler with the fake worker: saved route, frozen
//! prerequisites, dependency order, durable queue history and held-after-restart semantics.
mod common;
use ck::hints::{plan_steps, staging_root, HintCfg};
use ck::project::{load_project, LoadOptions};
use ck::queue;
use ck::settings::Settings;
use ck::ui::ai::AiUi;
use ck::workflow::{new_id, plan, Inputs, Intent, OutputCfg, Params};
use common::*;
use serde_json::json;

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
fn saved_route_generates_the_hint_then_keys_and_history_survives_a_restart() {
    let base = tmp("flow");
    let proj = base.join("proj");
    let clip_root = proj.join("clips/c1");
    std::fs::create_dir_all(clip_root.join("Frames")).unwrap();
    for i in 0..4 {
        png(&clip_root.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip_root.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip_root.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let load = || load_project(&proj, &LoadOptions::default()).unwrap();

    // Nothing saved yet: no implicit generation, the gate would say "Needs hint".
    let p = load();
    let c = &p.clips[0];
    let stg = staging_root(&p.root, &c.root, &c.folder);
    assert!(plan_steps(
        c,
        &HintCfg::default(),
        "full",
        (0, 3),
        false,
        false,
        &stg,
        new_id
    )
    .unwrap()
    .is_empty());
    HintCfg::default().save(c)
    .unwrap();

    let p = load();
    let c = p.clips[0].clone();
    let steps = plan_steps(
        &c,
        &HintCfg::load(&c),
        "full",
        (0, 3),
        false,
        false,
        &stg,
        new_id,
    )
    .unwrap();
    assert_eq!(steps.len(), 1);
    assert_eq!(
        steps[0].staging_dir.as_ref().unwrap().parent().unwrap(),
        stg
    );
    assert!(
        !steps[0].staging_dir.as_ref().unwrap().starts_with(&c.root),
        "staging is outside the clip folder"
    );

    let params = Params {
        hint_mode: "full".into(),
        ..Params::default()
    };
    let out = OutputCfg {
        fg_enabled: false,
        comp_enabled: false,
        processed_enabled: false,
        matte_format: "png".into(),
        ..OutputCfg::default()
    };
    let inp = Inputs {
        clip: &c,
        project_name: "P",
        location: None,
        params: &params,
        out: &out,
        range: (0, 3),
        digest: None,
        pending_hint: true,
    };
    let key = plan(&inp, Intent::New).unwrap();
    // Without the declared prerequisite the same plan would be blocked.
    assert!(plan(
        &Inputs {
            pending_hint: false,
            ..Inputs {
                clip: &c,
                project_name: "P",
                location: None,
                params: &params,
                out: &out,
                range: (0, 3),
                digest: None,
                pending_hint: false
            }
        },
        Intent::New
    )
    .is_err());

    let repo = make_repo(&base, "w0", json!({}));
    let mut st = Settings::default();
    st.ai_targets = vec![local("l0", 0, &repo)];
    let mut ai = AiUi::new(&st, None);
    ai.queue_open(Some(&proj));
    let last = ai
        .submit_chain(&c, steps, Some((key, params, out)))
        .unwrap();
    wait_until("chain done", || {
        ai.tick();
        ai.queue_sync();
        ai.scheduler()
            .state_of(&last)
            .is_some_and(|s| s.is_terminal())
    });
    assert!(
        clip_root.join("AlphaHint/f_1101.png").is_file(),
        "hint published by the worker"
    );
    let (hist, err) = queue::load(&proj);
    assert!(err.is_none());
    assert_eq!(
        hist.iter().filter(|e| e.state == "complete").count(),
        2,
        "{hist:?}"
    );

    // Restart semantics: an entry that was still running when the app died comes back HELD, never running.
    let mut e = hist[0].clone();
    e.id = "stuck".into();
    e.state = "running".into();
    let mut all = hist.clone();
    all.push(e);
    queue::save(&proj, &all).unwrap();
    let (again, _) = queue::load(&proj);
    assert_eq!(
        again.iter().find(|x| x.id == "stuck").unwrap().state,
        "held"
    );
}

#[test]
fn relative_project_path_yields_absolute_clip_paths_for_every_task_job() {
    let base = tmp("rel");
    let proj = base.join("proj");
    let clip = proj.join("clips/c1");
    std::fs::create_dir_all(clip.join("Frames")).unwrap();
    for i in 0..3 {
        png(&clip.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    // Load through a path relative to the process cwd, as `keyloom-ui target/.../project` does.
    let cwd = std::env::current_dir().unwrap();
    let rel = pathdiff(&proj, &cwd);
    let p = load_project(&rel, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    assert!(p.root.is_absolute() && c.root.is_absolute(), "{:?}", c.root);
    let stg = staging_root(&p.root, &c.root, &c.folder);
    let cfg = HintCfg::default();
    let steps = plan_steps(
        c,
        &cfg,
        "full",
        (0, 2),
        false,
        true,
        &stg,
        new_id,
    )
    .unwrap();
    for j in steps {
        assert!(
            j.clip_root.is_absolute()
                && j.publish_dir.as_ref().unwrap().is_absolute()
                && j.staging_dir.as_ref().unwrap().is_absolute()
        );
        j.validate().unwrap();
    }
}

/// `to` expressed relative to `from` using `..` segments (both absolute).
fn pathdiff(to: &std::path::Path, from: &std::path::Path) -> std::path::PathBuf {
    let (t, f): (Vec<_>, Vec<_>) = (to.components().collect(), from.components().collect());
    let n = t.iter().zip(&f).take_while(|(a, b)| a == b).count();
    let mut out = std::path::PathBuf::new();
    for _ in n..f.len() {
        out.push("..");
    }
    for c in &t[n..] {
        out.push(c);
    }
    out
}

/// MatAnyone2 seeds from the guide's In frame only, so a one-frame pre-matte satisfies it; VideoMaMa refines
/// the whole range and still needs every frame. The generated guide steps therefore differ in coverage.
#[test]
fn matanyone2_requires_only_the_in_guide_while_videomama_needs_the_range() {
    let base = tmp("guide-coverage");
    let proj = base.join("proj");
    let clip_root = proj.join("clips/c1");
    std::fs::create_dir_all(clip_root.join("Frames")).unwrap();
    for i in 0..3 {
        png(&clip_root.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(
        clip_root.join(".video_metadata.json"),
        r#"{"fps":24.0,"width":4,"height":4}"#,
    )
    .unwrap();
    std::fs::write(clip_root.join("clip.json"), "{}").unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    let load = || load_project(&proj, &LoadOptions::default()).unwrap();

    // A guide for the In frame only, as the Hint page's "generate pre-matte" on one frame would leave it.
    std::fs::create_dir_all(clip_root.join("VideoMamaMaskHint")).unwrap();
    png(&clip_root.join("VideoMamaMaskHint/f_1101.png"));

    let mut cfg = HintCfg::default();
    // Chroma needs no prompts, so an auto-generated guide step can actually be built here.
    cfg.guide = ck::hints::Producer::Chroma;
    cfg.finish = ck::hints::Final::Matanyone2;
    let p = load();
    cfg.save(&p.clips[0]).unwrap();

    let p = load();
    let c = p.clips[0].clone();
    let stg = staging_root(&p.root, &c.root, &c.folder);
    // MatAnyone2: nothing left to generate, the existing In guide is enough.
    let steps = ck::hints::Gen {
        clip: &c,
        cfg: &cfg,
        ann: None,
        linear: false,
        staging: &stg,
    }
    .hint((0, 2), false, &mut new_id)
    .expect("one-frame guide satisfies MatAnyone2");
    assert_eq!(steps.len(), 1, "only the refine step");
    assert_eq!(steps[0].kind, ck::tasks::TaskKind::Matanyone2);

    // VideoMaMa: the range is not covered, so without auto_guide it refuses and names the gap ...
    let mut vm = cfg.clone();
    vm.finish = ck::hints::Final::Videomama;
    let err = ck::hints::Gen {
        clip: &c,
        cfg: &vm,
        ann: None,
        linear: false,
        staging: &stg,
    }
    .hint((0, 2), false, &mut new_id)
    .expect_err("VideoMaMa needs the whole range");
    assert!(err.contains("frames 0-2"), "names the missing range: {err}");

    // ... and with auto_guide it generates exactly that, then refines.
    let steps = ck::hints::Gen {
        clip: &c,
        cfg: &vm,
        ann: None,
        linear: false,
        staging: &stg,
    }
    .hint((0, 2), true, &mut new_id)
    .unwrap();
    assert_eq!(steps.len(), 2, "guide then refine");
    assert_ne!(steps[0].kind, ck::tasks::TaskKind::Videomama, "the guide step comes first");
    let r = steps[0].range.as_ref().expect("guide step has a range");
    assert_eq!(
        (r.in_point, r.out_point),
        (0, 2),
        "VideoMaMa's guide covers the whole range"
    );
    assert_eq!(steps[1].kind, ck::tasks::TaskKind::Videomama);
}
