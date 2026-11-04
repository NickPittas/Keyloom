mod common;
use ck::contracts::RangeIdentity;
use ck::jobs::Require;
use ck::scheduler::*;
use ck::tasks::{TaskJob, TaskKind};
use common::*;
use serde_json::json;
use std::path::Path;

fn task(base: &Path, id: &str, kind: TaskKind, fake: &str) -> TaskJob {
    let root = base.join(format!("{id}_clip"));
    std::fs::create_dir_all(&root).unwrap();
    TaskJob {
        job_id: id.into(),
        kind,
        clip_id: format!("clip-{id}"),
        clip_root: root.clone(),
        range: kind.ranged().then(|| RangeIdentity {
            in_point: 0,
            out_point: 3,
            in_stem: "a".into(),
            out_stem: "d".into(),
        }),
        params: json!({"fake": fake, "dest": base.join(format!("{id}.mp4")), "tracker_model": "base_plus", "variant": "Matting"}),
        staging_dir: kind.needs_staging().then(|| base.join("stg").join(id)),
        publish_dir: kind.writes_hint().then(|| root.join("AlphaHint")),
        require: Require {
            protocol: 1,
            models: Default::default(),
        },
    }
}
fn st(s: &Scheduler, id: &str) -> JobState {
    s.state_of(id).unwrap().clone()
}
fn settle(s: &mut Scheduler, ids: &[&str]) -> Vec<SchedEvent> {
    let mut all = vec![];
    wait_until(&format!("{ids:?} terminal"), || {
        all.extend(s.tick());
        ids.iter().all(|i| st(s, i).is_terminal())
    });
    all
}
fn sched(name: &str, cfg: serde_json::Value) -> (std::path::PathBuf, Scheduler) {
    let base = tmp(name);
    let repo = make_repo(&base, "w0", cfg);
    let mut s = Scheduler::new(vec![local("l0", 0, &repo)]).unwrap();
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::for_tests()));
    (base, s)
}
fn ok(s: &JobState) -> bool {
    matches!(s, JobState::Done { info, .. } if info.outcome == ck::worker::Outcome::Complete)
}

#[test]
fn hint_task_then_key_chain_runs_in_order_and_reports_result() {
    let (base, mut s) = sched("chain", json!({}));
    s.submit_task(task(&base, "h1", TaskKind::Gvm, "ok"), vec![])
        .unwrap();
    // Key on the same clip after the hint writer: dependency declared, so no collision.
    let mut k = job(&base, "k1", 2, "ok");
    k.clip_root = base.join("h1_clip");
    s.submit(Submit {
        job: k,
        placement: Placement::Auto,
        after: vec!["h1".into()],
        min_vram_gb: None,
    })
    .unwrap();
    settle(&mut s, &["h1", "k1"]);
    assert!(ok(&st(&s, "h1")) && ok(&st(&s, "k1")));
    let JobState::Done { info, .. } = st(&s, "h1") else {
        panic!()
    };
    assert_eq!(info.kind.as_deref(), Some("gvm"));
    assert_eq!(info.result.unwrap()["published"], 1);
    assert!(base.join("h1_clip/AlphaHint/a.png").is_file());
}

#[test]
fn key_on_same_clip_without_dependency_collides_with_hint_writer() {
    let (base, mut s) = sched("coll", json!({}));
    s.submit_task(task(&base, "h1", TaskKind::Birefnet, "hold"), vec![])
        .unwrap();
    let mut k = job(&base, "k1", 2, "ok");
    k.clip_root = base.join("h1_clip");
    let r = s.submit(Submit {
        job: k,
        placement: Placement::Auto,
        after: vec![],
        min_vram_gb: None,
    });
    assert!(matches!(r, Err(SubmitError::Collision { .. })));
    let mut t2 = task(&base, "h2", TaskKind::Videomama, "ok");
    t2.clip_root = base.join("h1_clip");
    t2.staging_dir = Some(base.join("stg/h2"));
    t2.publish_dir = Some(base.join("h1_clip/AlphaHint"));
    assert!(matches!(
        s.submit_task(t2, vec![]),
        Err(SubmitError::Collision { .. })
    ));
}

#[test]
fn failed_or_cancelled_prerequisite_blocks_dependents_and_cancel_reaches_worker() {
    let (base, mut s) = sched("fail", json!({}));
    s.submit_task(task(&base, "h1", TaskKind::Gvm, "fail"), vec![])
        .unwrap();
    s.submit_task(task(&base, "h2", TaskKind::Chroma, "ok"), vec!["h1".into()])
        .unwrap();
    settle(&mut s, &["h1", "h2"]);
    assert!(matches!(st(&s, "h2"), JobState::Failed { .. }));
    s.submit_task(task(&base, "h3", TaskKind::SamTrack, "hold"), vec![])
        .unwrap();
    wait_until("running", || {
        s.tick();
        matches!(st(&s, "h3"), JobState::Running { .. })
    });
    s.cancel("h3");
    settle(&mut s, &["h3"]);
    assert!(
        matches!(st(&s, "h3"), JobState::Done { info, .. } if info.outcome == ck::worker::Outcome::Cancelled)
    );
}

#[test]
fn preview_event_surfaces_and_unready_route_is_refused_without_a_worker_call() {
    let (base, mut s) = sched("prev", json!({}));
    // GVM is missing; SAM is present.
    let gvm_only_unverified = json!({"models": {
        "gvm": {"gvm": {"ready": false, "state": "missing", "paths": {"gvm": "/m/gvm"}}},
        "sam2": {"sam2-base-plus": {"ready": true, "state": "present", "paths": {"sam2-base-plus": "/m/sam.pt"}}}},
        "sam2": {"base_plus": "sam2-base-plus"}});
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::from_json(&gvm_only_unverified).unwrap()));
    let mut t = task(&base, "p1", TaskKind::SamPreview, "ok");
    t.params["frame_stem"] = json!("a");
    s.submit_task(t, vec![]).unwrap();
    s.submit_task(task(&base, "h1", TaskKind::Gvm, "ok"), vec![])
        .unwrap();
    let ev = settle(&mut s, &["p1", "h1"]);
    assert!(ev
        .iter()
        .any(|e| matches!(e, SchedEvent::Preview { frame_stem, .. } if frame_stem == "a")));
    assert!(matches!(st(&s, "h1"), JobState::Failed { reason } if reason == "GVM is missing — download it on the Models page"));
    assert!(log_lines(&base, "w0").iter().all(|l| l["job_id"] != "h1"));
}

fn lock_file(root: &Path, job: &str) {
    std::fs::write(
        root.join(".corridorkey_hint.lock"),
        format!(r#"{{"job_id":"{job}","host":"h","pid":7}}"#),
    )
    .unwrap();
}
fn lock_outcome(s: &mut Scheduler, rid: &str) -> LockOutcome {
    let mut out = None;
    wait_until("lock reply", || {
        for e in s.tick() {
            if let SchedEvent::HintLock {
                request_id,
                outcome,
            } = e
            {
                if request_id == rid {
                    out = Some(outcome);
                }
            }
        }
        out.is_some()
    });
    out.unwrap()
}

#[test]
fn lock_requests_go_through_the_app_side_helper_with_exact_identity_and_never_race_a_job() {
    use ck::worker::LockCmd;
    let (base, mut s) = sched("lock", json!({}));
    let root = base.join("c1");
    std::fs::create_dir_all(&root).unwrap();
    lock_file(&root, "old-job");
    let rid = s
        .lock_request(LockCmd::Inspect {
            clip_root: root.clone(),
        })
        .unwrap();
    let LockOutcome::Inspected(Some(info)) = lock_outcome(&mut s, &rid) else {
        panic!()
    };
    assert_eq!(
        (info["job_id"].as_str(), info["pid"].as_u64()),
        (Some("old-job"), Some(7))
    );
    // Wrong expectation is refused by the worker and the file stays.
    let bad = s
        .lock_request(LockCmd::Clear {
            clip_root: root.clone(),
            job_id: "other".into(),
            host: "h".into(),
            pid: 7,
            reason: "user_confirmed_orphan",
        })
        .unwrap();
    assert!(
        matches!(lock_outcome(&mut s, &bad), LockOutcome::Refused { code, .. } if code == "mismatch")
    );
    assert!(root.join(".corridorkey_hint.lock").exists());
    let ok = s
        .lock_request(LockCmd::Clear {
            clip_root: root.clone(),
            job_id: "old-job".into(),
            host: "h".into(),
            pid: 7,
            reason: "observed_exit",
        })
        .unwrap();
    assert!(matches!(
        lock_outcome(&mut s, &ok),
        LockOutcome::Cleared { .. }
    ));
    assert!(!root.join(".corridorkey_hint.lock").exists());
    let sent: Vec<_> = log_lines(&base, "w0")
        .into_iter()
        .filter(|l| l["event"] == "lock_cmd")
        .collect();
    assert_eq!(sent.len(), 3);
    assert!(log_lines(&base, "w0").iter().filter(|l| l["event"] == "start").count() <= 1, "no lock command reached a worker");
    assert_eq!(
        sent[2]["expect"],
        json!({"job_id":"old-job","host":"h","pid":7})
    );
    assert_eq!(sent[2]["reason"], "observed_exit");
    // A running job holds the slot: the request waits and is answered after the job ended.
    s.submit_task(task(&base, "h1", TaskKind::Gvm, "hold"), vec![])
        .unwrap();
    wait_until("running", || {
        s.tick();
        matches!(st(&s, "h1"), JobState::Running { .. })
    });
    lock_file(&root, "old-job");
    let late = s
        .lock_request(LockCmd::Inspect { clip_root: root })
        .unwrap();
    for _ in 0..20 {
        s.tick();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.lock_pending(), "not sent while a job runs");
    s.cancel("h1");
    assert!(matches!(
        lock_outcome(&mut s, &late),
        LockOutcome::Inspected(Some(_))
    ));
}

#[test]
fn hint_writer_chained_after_another_writer_on_same_clip_is_accepted() {
    let (base, mut s) = sched("chain2", json!({}));
    s.submit_task(task(&base, "h1", TaskKind::Birefnet, "hold"), vec![])
        .unwrap();
    let mut t2 = task(&base, "h2", TaskKind::Videomama, "ok");
    t2.clip_root = base.join("h1_clip");
    t2.staging_dir = Some(base.join("stg/h2"));
    t2.publish_dir = Some(base.join("h1_clip/AlphaHint"));
    // Declared dependency on the writer: it cannot run concurrently, so it must not collide.
    assert_eq!(s.submit_task(t2, vec!["h1".into()]), Ok(()));
}

#[test]
fn key_after_two_chained_writers_is_accepted_but_a_stranger_still_collides() {
    let (base, mut s) = sched("chain3", json!({}));
    s.submit_task(task(&base, "h1", TaskKind::Birefnet, "hold"), vec![])
        .unwrap();
    let mut t2 = task(&base, "h2", TaskKind::Matanyone2, "ok");
    t2.clip_root = base.join("h1_clip");
    t2.staging_dir = Some(base.join("stg/h2"));
    s.submit_task(t2, vec!["h1".into()]).unwrap();
    let key = |id: &str, after: Vec<String>| {
        let mut k = job(&base, id, 2, "ok");
        k.clip_root = base.join("h1_clip");
        Submit { job: k, placement: Placement::Auto, after, min_vram_gb: None }
    };
    assert!(matches!(s.submit(key("k0", vec![])), Err(SubmitError::Collision { .. })));
    assert_eq!(s.submit(key("k1", vec!["h2".into()])), Ok(()));
}

#[test]
fn model_changes_need_no_worker_restart_and_every_variant_gets_the_supplied_paths() {
    let (base, mut s) = sched("models", json!({}));
    s.probe(None);
    wait_until("idle worker ready", || {
        s.tick();
        s.targets().iter().any(|t| t.ready.is_some())
    });
    // GVM missing: refused with the app's reason; the idle worker is untouched.
    let unverified = json!({"models": {"gvm": {"gvm": {"ready": false, "state": "missing", "paths": {"gvm": "/m/gvm"}}}}});
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::from_json(&unverified).unwrap()));
    s.submit_task(task(&base, "r0", TaskKind::Gvm, "ok"), vec![]).unwrap();
    settle(&mut s, &["r0"]);
    assert!(matches!(st(&s, "r0"), JobState::Failed { .. }));
    // Verify/Repair finished: the app's new snapshot is all the next job needs.
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::for_tests()));
    let cases = [
        (TaskKind::Gvm, "gvm", json!({"gvm": "/models/gvm"})),
        (TaskKind::Birefnet, "birefnet", json!({"birefnet-BiRefNet-matting": "/models/birefnet-BiRefNet-matting"})),
        (TaskKind::SamPreview, "sam_preview", json!({"sam2-base-plus": "/models/sam2-base-plus"})),
        (TaskKind::SamTrack, "sam_track", json!({"sam2-base-plus": "/models/sam2-base-plus"})),
        (TaskKind::Matanyone2, "matanyone2", json!({"matanyone2": "/models/matanyone2"})),
        (TaskKind::Videomama, "videomama", json!({"videomama": "/models/videomama"})),
        (TaskKind::Chroma, "chroma", json!(null)),
        (TaskKind::ExportVideo, "export_video", json!(null)),
    ];
    for (i, (kind, _, _)) in cases.iter().enumerate() {
        let mut t = task(&base, &format!("r{}", i + 1), *kind, "ok");
        t.clip_root = base.join(format!("clip{i}")); // one clip each: no collisions
        t.staging_dir = t.staging_dir.map(|_| base.join(format!("stg/{i}")));
        t.publish_dir = t.publish_dir.map(|_| t.clip_root.join("AlphaHint"));
        s.submit_task(t, vec![]).unwrap();
    }
    let ids: Vec<String> = (1..=cases.len()).map(|i| format!("r{i}")).collect();
    settle(&mut s, &ids.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(ids.iter().all(|i| ok(&st(&s, i))), "{:?}", ids.iter().map(|i| st(&s, i)).collect::<Vec<_>>());
    let sent: Vec<_> = log_lines(&base, "w0").into_iter().filter(|l| l["event"] == "task").collect();
    for (i, (_, wire, files)) in cases.iter().enumerate() {
        let got = sent.iter().find(|l| l["job_id"] == format!("r{}", i + 1)).unwrap();
        assert_eq!((got["kind"].as_str(), &got["model_files"]), (Some(*wire), files), "{wire}");
    }
    let starts = log_lines(&base, "w0").iter().filter(|l| l["event"] == "start").count();
    assert_eq!(starts, 1, "the same worker process served all of it: no restart");
}

#[test]
fn jobs_that_need_no_model_run_when_the_model_check_failed_and_a_download_holds_only_its_own_model() {
    let (base, mut s) = sched("nomodel", json!({}));
    s.set_models_unavailable("Model status is unavailable: the catalog could not be read".into());
    s.submit_task(task(&base, "c1", TaskKind::Chroma, "ok"), vec![]).unwrap();
    s.submit_task(task(&base, "g1", TaskKind::Gvm, "ok"), vec![]).unwrap();
    settle(&mut s, &["c1", "g1"]);
    assert!(ok(&st(&s, "c1")), "Chroma needs no model: the failed check does not touch it");
    assert!(matches!(st(&s, "g1"), JobState::Failed { reason } if reason.contains("unavailable")));
    // A download of GVM holds GVM jobs only: Chroma and a BiRefNet job go on.
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::for_tests()));
    s.set_models_changing(Some(vec!["gvm".into()]));
    s.submit_task(task(&base, "g2", TaskKind::Gvm, "ok"), vec![]).unwrap();
    s.submit_task(task(&base, "c2", TaskKind::Chroma, "ok"), vec![]).unwrap();
    s.submit_task(task(&base, "b2", TaskKind::Birefnet, "ok"), vec![]).unwrap();
    settle(&mut s, &["c2", "b2"]);
    assert!(ok(&st(&s, "c2")) && ok(&st(&s, "b2")));
    assert_eq!(st(&s, "g2"), JobState::Queued, "its own model is being downloaded");
    s.set_models_changing(None);
    settle(&mut s, &["g2"]);
    assert!(ok(&st(&s, "g2")));
}

#[test]
fn during_a_birefnet_download_chroma_export_and_key_run_and_only_birefnet_jobs_are_held_with_the_reason() {
    let (base, mut s) = sched("bidl", json!({}));
    s.set_models_changing(Some(vec!["birefnet-BiRefNet-matting".into()]));
    s.submit_task(task(&base, "c1", TaskKind::Chroma, "ok"), vec![]).unwrap();
    s.submit_task(task(&base, "e1", TaskKind::ExportVideo, "ok"), vec![]).unwrap();
    s.submit_task(task(&base, "b1", TaskKind::Birefnet, "ok"), vec![]).unwrap();
    s.submit(Submit { job: job(&base, "k1", 2, "ok"), placement: Placement::Auto, after: vec![], min_vram_gb: None }).unwrap();
    settle(&mut s, &["c1", "e1", "k1"]);
    assert!(ok(&st(&s, "c1")) && ok(&st(&s, "e1")) && ok(&st(&s, "k1")));
    assert_eq!(st(&s, "b1"), JobState::Queued, "BiRefNet is the model being downloaded");
    assert_eq!(s.task_hold_reason("b1").as_deref(), Some("its model is being changed on the Models page"));
    assert_eq!(s.task_hold_reason("c1"), None);
    s.set_models_changing(None);
    settle(&mut s, &["b1"]);
    assert!(ok(&st(&s, "b1")));
}
