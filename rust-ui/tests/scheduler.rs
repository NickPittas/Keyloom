mod common;
use ck::jobs::{disk_checks_for, KeyJob};
use ck::scheduler::*;
use ck::worker::*;
use common::*;
use serde_json::json;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

fn sub(job: KeyJob, p: Placement) -> Submit {
    Submit {
        job,
        placement: p,
        after: vec![],
        min_vram_gb: None,
    }
}
fn state(s: &Scheduler, id: &str) -> JobState {
    s.job(id).unwrap().state.clone()
}
fn settle(s: &mut Scheduler, ids: &[&str]) -> Vec<SchedEvent> {
    let mut all = vec![];
    wait_until(&format!("{ids:?} terminal"), || {
        all.extend(s.tick());
        ids.iter().all(|i| state(s, i).is_terminal())
    });
    all
}
/// Wait until a Lost job's writer is proven stopped (the exit is observed asynchronously after the kill).
fn proven(s: &mut Scheduler, id: &str) {
    wait_until(&format!("{id} writer proven stopped"), || {
        s.tick();
        matches!(
            state(s, id),
            JobState::Lost {
                writer_stopped: true,
                ..
            }
        )
    });
}
fn one(name: &str, cfg: serde_json::Value) -> (std::path::PathBuf, Scheduler) {
    let base = tmp(name);
    let repo = make_repo(&base, "w0", cfg);
    (base, Scheduler::new(vec![local("l0", 0, &repo)]).unwrap())
}
fn complete(s: &JobState) -> bool {
    matches!(s, JobState::Done { info, .. } if info.outcome == Outcome::Complete)
}

#[test]
fn runs_a_job_with_progress_and_published_events_on_a_persistent_worker() {
    let (base, mut s) = one("ok", json!({}));
    let mut j = job(&base, "a", 4, "ok");
    j.params["delay"] = json!(0.03);
    s.submit(sub(j, Placement::Auto)).unwrap();
    assert_eq!(state(&s, "a"), JobState::Queued);
    let (mut saw, mut ev) = (vec![], vec![]);
    wait_until("done", || {
        ev.extend(s.tick());
        if let JobState::Running { done, total, .. } = state(&s, "a") {
            saw.push((done, total));
        }
        state(&s, "a").is_terminal()
    });
    assert!(complete(&state(&s, "a")));
    assert!(
        saw.iter().any(|(d, t)| *d > 0 && *d < *t),
        "intermediate progress seen: {saw:?}"
    );
    let stems: Vec<_> = ev
        .iter()
        .filter_map(|e| {
            if let SchedEvent::Published { stems, .. } = e {
                Some(stems.clone())
            } else {
                None
            }
        })
        .flatten()
        .collect();
    assert_eq!(stems, ["clip_1101", "clip_1102", "clip_1103", "clip_1104"]);
    assert!(!s.job("a").unwrap().unverified_models);
    // Second job reuses the same process (one `start` in the worker log).
    s.submit(sub(job(&base, "b", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["b"]);
    assert!(complete(&state(&s, "b")));
    assert_eq!(
        log_lines(&base, "w0")
            .iter()
            .filter(|l| l["event"] == "start")
            .count(),
        1
    );
    assert_eq!(s.targets()[0].status, TargetStatus::Ready);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn queued_snapshot_is_frozen_and_ids_must_be_unique() {
    let (base, mut s) = one("snap", json!({}));
    let j = job(&base, "a", 1, "ok");
    s.submit(sub(j.clone(), Placement::Auto)).unwrap();
    let snap = s.snapshot();
    assert_eq!(*snap[0].job, j);
    assert_eq!(
        s.submit(sub(j, Placement::Auto)).unwrap_err(),
        SubmitError::DuplicateId
    );
    let mut bad = job(&base, "z", 2, "ok");
    bad.outputs.get_mut("matte").unwrap().pop();
    assert!(matches!(
        s.submit(sub(bad, Placement::Auto)),
        Err(SubmitError::Invalid(_))
    ));
    assert_eq!(
        s.submit(sub(
            job(&base, "y", 1, "ok"),
            Placement::Target("nope".into())
        ))
        .unwrap_err(),
        SubmitError::UnknownTarget("nope".into())
    );
    settle(&mut s, &["a"]);
    assert_eq!(*snap[0].job, *s.job("a").unwrap().job); // later ticks never changed the frozen job
    assert_eq!(snap[0].state, JobState::Queued);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn collisions_with_queued_and_running_jobs_are_refused() {
    let (base, mut s) = one("coll", json!({}));
    s.submit(sub(job(&base, "a", 2, "hold"), Placement::Auto))
        .unwrap();
    let mut b = job(&base, "b", 2, "ok");
    b.manifest_dir = base.join("a/FG"); // nested in a's folder
    b.outputs.insert(
        "matte".into(),
        vec![base.join("a/FG/x_1101.png"), base.join("a/FG/x_1102.png")],
    );
    assert_eq!(
        s.submit(sub(b.clone(), Placement::Auto)).unwrap_err(),
        SubmitError::Collision { with: "a".into() }
    );
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    assert!(matches!(
        s.submit(sub(b.clone(), Placement::Auto)),
        Err(SubmitError::Collision { .. })
    ));
    s.cancel("a");
    settle(&mut s, &["a"]);
    s.submit(sub(b, Placement::Auto)).unwrap(); // finished jobs no longer fence
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn cancel_queued_and_running_and_worker_survives() {
    let (base, mut s) = one("cancel", json!({}));
    s.submit(sub(job(&base, "a", 2, "hold"), Placement::Auto))
        .unwrap();
    s.submit(sub(job(&base, "q", 1, "ok"), Placement::Auto))
        .unwrap();
    s.cancel("q");
    assert_eq!(state(&s, "q"), JobState::Cancelled);
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    s.cancel("a");
    settle(&mut s, &["a"]);
    assert!(
        matches!(state(&s, "a"), JobState::Done { info, .. } if info.outcome == Outcome::Cancelled && !info.manifest_complete)
    );
    assert!(s.job("a").unwrap().cancel_requested);
    s.submit(sub(job(&base, "n", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["n"]);
    assert!(complete(&state(&s, "n")));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn rejected_job_writes_nothing_and_worker_stays_usable() {
    let (base, mut s) = one("rej", json!({}));
    s.submit(sub(job(&base, "a", 2, "reject"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(matches!(state(&s, "a"), JobState::Rejected { code, .. } if code == "exists"));
    assert!(s
        .job("a")
        .unwrap()
        .failure_message()
        .unwrap()
        .contains("refused the job (exists): nope"));
    assert!(!base.join("a").exists());
    s.submit(sub(job(&base, "b", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["b"]);
    assert!(complete(&state(&s, "b")));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn crash_is_lost_with_proof_for_local_and_target_stays_down_until_reset() {
    let (base, mut s) = one("crash", json!({}));
    s.submit(sub(job(&base, "a", 3, "crash"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    proven(&mut s, "a");
    let JobState::Lost {
        reason,
        writer_stopped,
        ..
    } = state(&s, "a")
    else {
        panic!("{:?}", state(&s, "a"))
    };
    assert!(writer_stopped && reason.contains("code 3"), "{reason}");
    assert!(matches!(s.targets()[0].status, TargetStatus::Down(_)));
    // Proven stopped local writer: the folder can be reused (resume is a new job id).
    let mut again = job(&base, "a2", 3, "ok");
    again.manifest_dir = base.join("a");
    again.outputs = job(&base, "a", 3, "ok").outputs;
    s.submit(sub(again, Placement::Auto)).unwrap();
    settle(&mut s, &["a2"]);
    assert!(
        matches!(state(&s, "a2"), JobState::Failed { reason } if reason.contains("worker exited"))
    );
    s.reset_target("l0");
    s.submit(sub(job(&base, "c", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["c"]);
    assert!(complete(&state(&s, "c")));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn protocol_violations_kill_the_worker_and_lose_the_job() {
    for (mode, needle) in [
        ("malformed", "not JSON"),
        ("oversized", "longer than"),
        ("badversion", "version"),
        ("mismatch", "other-job"),
        ("exit_clean", "code 0"),
    ] {
        let (base, mut s) = one(mode, json!({}));
        s.submit(sub(job(&base, "a", 2, mode), Placement::Auto))
            .unwrap();
        settle(&mut s, &["a"]);
        proven(&mut s, "a");
        let JobState::Lost {
            reason,
            writer_stopped,
            ..
        } = state(&s, "a")
        else {
            panic!("{mode}: {:?}", state(&s, "a"))
        };
        assert!(
            reason.contains(needle) && writer_stopped,
            "{mode}: {reason}"
        );
        assert!(matches!(s.targets()[0].status, TargetStatus::Down(_)));
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn failed_done_is_reported_not_complete() {
    let (base, mut s) = one("fail", json!({}));
    s.submit(sub(job(&base, "a", 1, "fail"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(
        matches!(state(&s, "a"), JobState::Done { info, .. } if info.outcome == Outcome::Failed && info.error_kind.as_deref() == Some("engine"))
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn dependency_runs_after_success_and_fails_after_failure() {
    let (base, mut s) = one("dep", json!({}));
    let mut b = sub(job(&base, "b", 1, "ok"), Placement::Auto);
    b.after = vec!["a".into()];
    assert_eq!(
        s.submit(Submit {
            after: vec!["ghost".into()],
            ..sub(job(&base, "g", 1, "ok"), Placement::Auto)
        })
        .unwrap_err(),
        SubmitError::UnknownDependency("ghost".into())
    );
    s.submit(sub(job(&base, "a", 1, "ok"), Placement::Auto))
        .unwrap();
    s.submit(b).unwrap();
    settle(&mut s, &["a", "b"]);
    assert!(complete(&state(&s, "b")));
    // Failing prerequisite (reject, fail-outcome, cancelled) never lets the dependent start.
    for (i, mode) in ["reject", "fail"].iter().enumerate() {
        let (p, d) = (format!("p{i}"), format!("d{i}"));
        s.submit(sub(job(&base, &p, 1, mode), Placement::Auto))
            .unwrap();
        s.submit(Submit {
            after: vec![p.clone()],
            ..sub(job(&base, &d, 1, "ok"), Placement::Auto)
        })
        .unwrap();
        settle(&mut s, &[&p, &d]);
        // Names the failed step and its own reason, not the id.
        let want = if *mode == "reject" { "Key was refused: " } else { "Key failed: " };
        assert!(
            matches!(state(&s, &d), JobState::Failed { reason } if reason.starts_with(want)),
            "{mode}: {:?}", state(&s, &d)
        );
        assert!(!base.join(&d).exists());
        // The chain's outcome is the failed prerequisite itself, not the dependent's "did not start".
        let first = s.chain_outcome(&d).expect("ended").unwrap_err();
        assert!(!matches!(first, JobState::Failed { .. } | JobState::Cancelled), "{mode}: {first:?}");
    }
    assert_eq!(s.chain_outcome("b"), Some(Ok(())));
    s.submit(sub(job(&base, "pc", 1, "ok"), Placement::Auto))
        .unwrap();
    s.submit(Submit {
        after: vec!["pc".into()],
        ..sub(job(&base, "dc", 1, "ok"), Placement::Auto)
    })
    .unwrap();
    s.cancel("pc");
    settle(&mut s, &["dc"]);
    assert!(matches!(state(&s, "dc"), JobState::Failed { .. }));
    assert_eq!(s.chain_outcome("dc"), Some(Err(JobState::Cancelled)), "a cancel stays a cancel");
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn multiple_targets_run_jobs_in_parallel_and_explicit_placement_is_honored() {
    let base = tmp("multi");
    let (r0, r1) = (
        make_repo(&base, "w0", json!({})),
        make_repo(&base, "w1", json!({})),
    );
    let mut s = Scheduler::new(vec![local("g0", 0, &r0), local("g1", 1, &r1)]).unwrap();
    for id in ["a", "b", "c"] {
        s.submit(sub(job(&base, id, 2, "hold"), Placement::Auto))
            .unwrap();
    }
    wait_until("two running", || {
        s.tick();
        ["a", "b"]
            .iter()
            .all(|i| matches!(state(&s, i), JobState::Running { .. }))
    });
    let t = |i: &str| match state(&s, i) {
        JobState::Running { target, .. } => target,
        o => panic!("{o:?}"),
    };
    let (ta, tb) = (t("a"), t("b"));
    assert_ne!(ta, tb); // one job per target (which gets which depends on who said hello first)
    assert_eq!(state(&s, "c"), JobState::Queued);
    s.cancel("b");
    wait_until("c takes the target b freed", || {
        s.tick();
        matches!(state(&s, "c"), JobState::Running { ref target, .. } if *target == tb)
    });
    s.cancel("a");
    s.cancel("c");
    settle(&mut s, &["a", "b", "c"]);
    // Explicit target.
    s.submit(sub(
        job(&base, "e", 1, "ok"),
        Placement::Target("g1".into()),
    ))
    .unwrap();
    settle(&mut s, &["e"]);
    assert!(matches!(state(&s, "e"), JobState::Done { target, .. } if target == "g1"));
    assert!(log_lines(&base, "w0")
        .iter()
        .filter(|l| l["event"] == "job")
        .all(|l| l["job_id"] != "e"));
    // Only the targets that were needed were started: g0 for a, g1 for b.
    assert_eq!(
        log_lines(&base, "w0")
            .iter()
            .filter(|l| l["event"] == "start")
            .count(),
        1
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn identity_and_protocol_gates_refuse_targets() {
    // protocol 2 worker: target Refused (sticky), job Failed with the reason.
    let (base, mut s) = one("proto", json!({"protocol": 2}));
    s.submit(sub(job(&base, "a", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("protocol 2")));
    assert!(matches!(s.targets()[0].status, TargetStatus::Refused(_)));
    std::fs::remove_dir_all(base).unwrap();

    // Pinned digest: mismatch / worker without digest are refused, a match (any case) runs verified.
    let want = |sha: Option<&str>, base: &Path| {
        let mut j = job(base, "a", 1, "ok");
        j.require
            .models
            .insert("corridorkey_v2".into(), sha.map(String::from));
        j
    };
    for (worker, reference, needle) in [
        (json!("bb"), "aa", "differs"),
        (json!(null), "aa", "no digest"),
    ] {
        let (base, mut s) = one("sha", json!({"sha": worker}));
        s.allow_unverified = true; // must not rescue a pinned digest
        s.submit(sub(want(Some(reference), &base), Placement::Auto))
            .unwrap();
        settle(&mut s, &["a"]);
        assert!(
            matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains(needle)),
            "{needle}: {:?}",
            state(&s, "a")
        );
        std::fs::remove_dir_all(base).unwrap();
    }
    let (base, mut s) = one("sha-ok", json!({"sha": "aa"}));
    s.submit(sub(want(Some("AA"), &base), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(complete(&state(&s, "a")) && !s.job("a").unwrap().unverified_models);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn local_worker_is_the_trusted_reference_for_default_jobs_without_allow_unverified() {
    let base = tmp("ref");
    let (r0, r1) = (
        make_repo(&base, "w0", json!({"sha": "aa"})),
        make_repo(&base, "w1", json!({"sha": "bb"})),
    );
    let mut s = Scheduler::new(vec![local("g0", 0, &r0), local("g1", 1, &r1)]).unwrap();
    assert!(!s.allow_unverified && s.reference().is_none());
    s.probe(Some("g0"));
    wait_until("g0 hello", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Ready
    });
    assert_eq!(
        s.reference().unwrap()["corridorkey_v2"].as_deref(),
        Some("aa")
    );
    assert_eq!(s.targets()[0].trust, Trust::Reference);
    // A default job (no pinned digest) runs on the reference worker, verified, and the wire job pins its digest.
    let mut d = job(&base, "d", 1, "ok");
    d.require.models.insert("corridorkey_v2".into(), None);
    s.submit(sub(d, Placement::Auto)).unwrap();
    settle(&mut s, &["d"]);
    assert!(complete(&state(&s, "d")) && !s.job("d").unwrap().unverified_models);
    assert_eq!(
        log_lines(&base, "w0")
            .iter()
            .find(|l| l["event"] == "job")
            .unwrap()["require"]["models"]["corridorkey_v2"],
        "aa"
    );
    // A second local worker with a different digest is refused for default jobs (never silently accepted).
    s.probe(Some("g1"));
    wait_until("g1 hello", || {
        s.tick();
        s.targets()[1].status == TargetStatus::Ready
    });
    assert_eq!(s.targets()[1].trust, Trust::Mismatch);
    let mut e = job(&base, "e", 1, "ok");
    e.require.models.insert("corridorkey_v2".into(), None);
    assert!(
        matches!(s.readiness(&Placement::Target("g1".into()), &e, None), Readiness::Blocked(r) if r[0].contains("differs"))
    );
    assert!(
        matches!(s.readiness(&Placement::Auto, &e, None), Readiness::Ready { ref target, unverified: false, .. } if target == "g0")
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn key_needs_a_gpu_and_readiness_reports_it() {
    let (base, mut s) = one("nogpu", json!({"no_gpu": true}));
    s.probe(None);
    wait_until("hello", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Ready
    });
    let j = job(&base, "a", 1, "ok");
    assert!(
        matches!(s.readiness(&Placement::Auto, &j, None), Readiness::Blocked(r) if r[0].contains("no GPU"))
    );
    s.submit(sub(j, Placement::Auto)).unwrap();
    settle(&mut s, &["a"]);
    assert!(matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("no GPU")));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn readiness_is_pending_until_probe_finishes_then_ready() {
    let (base, mut s) = one("ready", json!({}));
    let j = job(&base, "a", 1, "ok");
    assert!(matches!(
        s.readiness(&Placement::Auto, &j, None),
        Readiness::Pending(_)
    ));
    s.probe(None);
    assert_eq!(s.targets()[0].status, TargetStatus::Starting);
    assert!(s.targets()[0].starting_for.is_some());
    wait_until("ready", || {
        s.tick();
        matches!(
            s.readiness(&Placement::Auto, &j, None),
            Readiness::Ready { .. }
        )
    });
    assert!(s.targets()[0].hello.as_ref().unwrap().gpu.is_some());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn hello_deadline_kills_a_silent_worker_that_has_no_job() {
    let (base, mut s) = one("hellodl", json!({"hello_delay": 30}));
    s.hello_timeout = std::time::Duration::from_millis(300);
    s.submit(sub(job(&base, "a", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(
        matches!(s.targets()[0].status, TargetStatus::Down(ref r) if r.contains("no hello") && r.contains("Retry"))
    );
    assert_eq!(
        s.targets()[0].startup_budget,
        std::time::Duration::from_millis(300)
    );
    assert_eq!(
        Scheduler::new(vec![]).unwrap().hello_timeout,
        std::time::Duration::from_secs(600)
    );
    s.tick(); // no automatic respawn
    assert!(matches!(s.targets()[0].status, TargetStatus::Down(_)));
    assert!(matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("no hello")));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn silent_running_job_is_reported_but_never_killed() {
    let (base, mut s) = one("stall", json!({}));
    s.submit(sub(job(&base, "a", 2, "hold"), Placement::Auto))
        .unwrap();
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    std::thread::sleep(std::time::Duration::from_millis(250));
    s.tick();
    let st = s.stalled(std::time::Duration::from_millis(200));
    assert_eq!(
        (st.len(), st[0].job_id.as_str(), st[0].accepted),
        (1, "a", true)
    );
    assert!(s.stalled(std::time::Duration::from_secs(60)).is_empty());
    assert!(
        matches!(state(&s, "a"), JobState::Running { .. })
            && s.targets()[0].status == TargetStatus::Ready
    );
    assert!(s.job("a").unwrap().accepted_at.is_some());
    s.cancel("a");
    settle(&mut s, &["a"]);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn graceful_stop_reports_stopping_then_stopped_and_running_job_ends_cancelled() {
    let (base, mut s) = one("stop", json!({}));
    s.submit(sub(job(&base, "a", 2, "hold"), Placement::Auto))
        .unwrap();
    s.submit(sub(job(&base, "q", 1, "ok"), Placement::Auto))
        .unwrap(); // queued behind
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    s.stop_all();
    assert_eq!(s.targets()[0].status, TargetStatus::Stopping);
    assert!(!s.all_stopped());
    assert_eq!(state(&s, "q"), JobState::Cancelled);
    wait_until("stopped", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Stopped
    });
    // EOF made the worker cancel cleanly: the job got its own `done`, it was not lost.
    assert!(
        matches!(state(&s, "a"), JobState::Done { info, .. } if info.outcome == Outcome::Cancelled)
    );
    assert!(s.all_stopped());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn force_stop_does_not_block_and_proof_arrives_when_the_exit_is_observed() {
    let (base, mut s) = one("force", json!({}));
    let j = job(&base, "a", 2, "hold");
    s.submit(sub(j.clone(), Placement::Auto)).unwrap();
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    // The worker's lease, as the Python side would have left it.
    std::fs::create_dir_all(&j.manifest_dir).unwrap();
    std::fs::write(
        j.manifest_dir.join(".corridorkey_job.lock"),
        json!({"job_id": "a"}).to_string(),
    )
    .unwrap();
    let t = std::time::Instant::now();
    s.force_stop("l0");
    assert!(
        t.elapsed() < std::time::Duration::from_millis(100),
        "force_stop blocked the caller"
    );
    // Nothing is claimed yet: Lost, writer not proven, lock untouched, target still Stopping, process still tracked.
    let JobState::Lost {
        writer_stopped,
        recovery,
        ..
    } = state(&s, "a")
    else {
        panic!()
    };
    assert!(!writer_stopped && recovery.is_none());
    assert!(j.manifest_dir.join(".corridorkey_job.lock").exists());
    assert_eq!(s.targets()[0].status, TargetStatus::Stopping);
    assert!(!s.all_stopped());
    // Then the exit is observed by a later tick: proof, recovery, Stopped.
    proven(&mut s, "a");
    let JobState::Lost { recovery, .. } = state(&s, "a") else {
        panic!()
    };
    assert!(recovery.unwrap().lock_removed);
    assert!(!j.manifest_dir.join(".corridorkey_job.lock").exists());
    wait_until("stopped", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Stopped
    });
    assert!(s.all_stopped());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn local_windows_workers_are_never_proven_by_the_launcher_exiting() {
    assert!(writer_proven(false)); // local Linux: child reaped
    assert!(!writer_proven(true)); // local Windows: venv launcher != process tree
}

#[test]
fn polling_queued_busy_jobs_does_no_filesystem_work_and_dispatch_preflights_once() {
    let (base, mut s) = one("nofs", json!({}));
    let a = job(&base, "a", 3, "hold");
    s.submit(sub(a.clone(), Placement::Auto)).unwrap();
    wait_until("running", || {
        s.tick();
        matches!(state(&s, "a"), JobState::Running { .. })
    });
    assert_eq!(disk_checks_for(&a.manifest_dir), 1); // exactly one preflight, at dispatch
    let queued: Vec<KeyJob> = ["b", "c", "d"]
        .iter()
        .map(|i| job(&base, i, 50, "ok"))
        .collect();
    for q in &queued {
        s.submit(sub(q.clone(), Placement::Auto)).unwrap();
    }
    for _ in 0..200 {
        s.tick();
        for q in &queued {
            assert!(matches!(
                s.readiness(&Placement::Auto, q, None),
                Readiness::Ready { busy: true, .. }
            ));
        }
    }
    for q in queued.iter().chain([&a]) {
        assert_eq!(
            disk_checks_for(&q.manifest_dir),
            usize::from(q.job_id == "a"),
            "{}: UI polling touched the filesystem",
            q.job_id
        );
    }
    assert!(queued
        .iter()
        .all(|q| state(&s, &q.job_id) == JobState::Queued));
    s.cancel("a");
    settle(
        &mut s,
        &queued.iter().map(|q| q.job_id.as_str()).collect::<Vec<_>>(),
    );
    for q in &queued {
        assert_eq!(
            disk_checks_for(&q.manifest_dir),
            1,
            "{}: one preflight when it was actually dispatched",
            q.job_id
        );
    }
    std::fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[test]
fn failed_preflight_fails_the_job_without_contacting_the_worker() {
    let (base, mut s) = one("pre", json!({}));
    let j = job(&base, "a", 2, "ok");
    let outside = base.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&j.manifest_dir).unwrap();
    std::os::unix::fs::symlink(&outside, j.manifest_dir.join("sub")).unwrap();
    let mut j = j;
    j.outputs.insert(
        "matte".into(),
        vec![
            j.manifest_dir.join("sub/clip_1101.png"),
            j.manifest_dir.join("sub/clip_1102.png"),
        ],
    );
    s.submit(sub(j, Placement::Auto)).unwrap();
    settle(&mut s, &["a"]);
    assert!(
        matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("preflight") && reason.contains("outside"))
    );
    assert!(!outside.join("clip_1101.png").exists());
    assert!(log_lines(&base, "w0").iter().all(|l| l["event"] != "job")); // the worker never saw it
    assert_eq!(s.targets()[0].busy_with, None);
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn local_crash_cleans_only_its_own_lock_and_temps() {
    let (base, mut s) = one("rec", json!({}));
    let j = job(&base, "crashjob-0001", 2, "lock");
    s.submit(sub(j.clone(), Placement::Auto)).unwrap();
    // A sibling job's temp (same 8-char prefix) in the same folder must survive.
    std::fs::create_dir_all(&j.manifest_dir).unwrap();
    let other = j.manifest_dir.join(".ck-crashjob-0001-2-974304f0bdd7.png"); // different job whose id starts with ours
    std::fs::write(&other, "x").unwrap();
    settle(&mut s, &["crashjob-0001"]);
    proven(&mut s, "crashjob-0001");
    let JobState::Lost {
        writer_stopped,
        recovery,
        ..
    } = state(&s, "crashjob-0001")
    else {
        panic!()
    };
    let r = recovery.unwrap();
    assert!(
        writer_stopped && r.lock_removed && r.temps_removed.len() == 2,
        "{r:?}"
    ); // output tmp + probe
    assert!(!j.manifest_dir.join(".corridorkey_job.lock").exists());
    assert!(other.exists() && j.manifest_dir.join(".tmp_marker").exists());
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn exit_is_reported_promptly_even_if_a_descendant_keeps_the_pipes_open() {
    let (base, mut s) = one("grand", json!({}));
    s.submit(sub(job(&base, "a", 1, "grandchild"), Placement::Auto))
        .unwrap();
    let t = std::time::Instant::now();
    settle(&mut s, &["a"]);
    // The orphaned `sleep 4` still holds stdout/stderr; the exit must not wait for it.
    proven(&mut s, "a");
    assert!(
        t.elapsed() < std::time::Duration::from_millis(3500),
        "{:?}",
        t.elapsed()
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn done_manifest_path_is_replaced_by_the_local_one_with_a_warning() {
    let (base, mut s) = one("manifest", json!({}));
    s.submit(sub(job(&base, "a", 1, "badmanifest"), Placement::Auto))
        .unwrap();
    let ev = settle(&mut s, &["a"]);
    let JobState::Done { info, .. } = state(&s, "a") else {
        panic!()
    };
    assert_eq!(
        info.manifest.as_deref(),
        Some(s.job("a").unwrap().manifest_path().to_str().unwrap())
    );
    assert!(ev.iter().any(
        |e| matches!(e, SchedEvent::Warning { message, .. } if message.contains("/etc/elsewhere"))
    ));
    std::fs::remove_dir_all(base).unwrap();
}

#[test]
fn vram_gate_and_gpu_index_mismatch() {
    let (base, mut s) = one("vram", json!({"vram": 8.0}));
    s.submit(Submit {
        min_vram_gb: Some(16.0),
        ..sub(job(&base, "a", 1, "ok"), Placement::Auto)
    })
    .unwrap();
    settle(&mut s, &["a"]);
    assert!(matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("8.0 GB")));
    std::fs::remove_dir_all(base).unwrap();
    let (base, mut s) = one("gpuidx", json!({"gpu": 5}));
    s.submit(sub(job(&base, "a", 1, "ok"), Placement::Auto))
        .unwrap();
    settle(&mut s, &["a"]);
    assert!(matches!(s.targets()[0].status, TargetStatus::Refused(ref r) if r.contains("GPU 5")));
    std::fs::remove_dir_all(base).unwrap();
}

fn held(name: &str) -> (std::path::PathBuf, Scheduler, Arc<AtomicBool>) {
    let (base, mut s) = one(name, json!({}));
    let hold = Arc::new(AtomicBool::new(true));
    s.preflight_hold = Some(hold.clone());
    (base, s, hold)
}

#[test]
fn stop_during_a_pending_preflight_cancels_the_job_instead_of_losing_it() {
    for graceful_all in [true, false] {
        let (base, mut s, hold) = held("prestop");
        s.probe(None);
        wait_until("ready", || {
            s.tick();
            s.targets()[0].status == TargetStatus::Ready
        });
        s.submit(sub(job(&base, "a", 2, "ok"), Placement::Auto))
            .unwrap();
        s.tick();
        assert!(
            matches!(state(&s, "a"), JobState::Dispatching { .. })
                && s.targets()[0].busy_with.is_some()
        ); // preflight pending
        if graceful_all {
            s.stop_all()
        } else {
            s.stop_target("l0")
        }
        assert_eq!(state(&s, "a"), JobState::Cancelled);
        assert_eq!(s.targets()[0].busy_with, None);
        hold.store(false, Ordering::SeqCst); // the helper thread finishes now; its result must be ignored
        wait_until("stopped", || {
            s.tick();
            s.targets()[0].status == TargetStatus::Stopped
        });
        for _ in 0..20 {
            s.tick();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(state(&s, "a"), JobState::Cancelled); // never Lost, never sent
        assert!(s.needs_attention().is_empty());
        assert!(log_lines(&base, "w0").iter().all(|l| l["event"] != "job"));
        std::fs::remove_dir_all(base).unwrap();
    }
}

#[test]
fn losing_the_worker_during_a_pending_preflight_fails_the_unsent_job() {
    let (base, mut s, hold) = held("prelose");
    s.probe(None);
    wait_until("ready", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Ready
    });
    s.submit(sub(job(&base, "a", 2, "ok"), Placement::Auto))
        .unwrap();
    s.tick();
    s.force_stop("l0");
    assert!(
        matches!(state(&s, "a"), JobState::Failed { reason } if reason.contains("not started"))
    );
    hold.store(false, Ordering::SeqCst);
    wait_until("stopped", || {
        s.tick();
        s.targets()[0].status == TargetStatus::Stopped
    });
    assert!(matches!(state(&s, "a"), JobState::Failed { .. }) && s.needs_attention().is_empty());
    std::fs::remove_dir_all(base).unwrap();
}
