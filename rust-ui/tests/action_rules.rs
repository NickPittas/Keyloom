//! Headless rules for the shared action decision: labels, reasons, duplicate clicks and same-clip ordering.
mod common;
use ck::actions::{decide, waiting_step_failed, waits_for, Action};
use ck::contracts::RangeIdentity;
use ck::jobs::Require;
use ck::scheduler::*;
use ck::tasks::{TaskJob, TaskKind};
use common::*;
use serde_json::json;
use std::path::{Path, PathBuf};

fn task(base: &Path, clip: &str, id: &str, kind: TaskKind, fake: &str) -> TaskJob {
    let root = base.join(clip);
    std::fs::create_dir_all(&root).unwrap();
    TaskJob {
        job_id: id.into(),
        kind,
        clip_id: format!("id-{clip}"),
        clip_root: root.clone(),
        range: kind.ranged().then(|| RangeIdentity { in_point: 0, out_point: 3, in_stem: "a".into(), out_stem: "d".into() }),
        params: json!({"fake": fake, "dest": base.join(format!("{id}.mp4")), "tracker_model": "base_plus", "variant": "Matting"}),
        staging_dir: kind.needs_staging().then(|| base.join("stg").join(id)),
        publish_dir: kind.writes_hint().then(|| root.join("AlphaHint")),
        require: Require { protocol: 1, models: Default::default() },
    }
}
fn key(base: &Path, clip: &str, id: &str, fake: &str) -> Submit {
    let mut k = job(base, id, 2, fake);
    k.clip_root = base.join(clip);
    Submit { job: k, placement: Placement::Auto, after: vec![], min_vram_gb: None }
}
fn sched(name: &str) -> (PathBuf, Scheduler) {
    let base = tmp(name);
    let repo = make_repo(&base, "w0", json!({}));
    let mut s = Scheduler::new(vec![local("l0", 0, &repo)]).unwrap();
    s.set_models(std::sync::Arc::new(ck::models::Snapshot::for_tests()));
    (base, s)
}
fn st(s: &Scheduler, id: &str) -> JobState {
    s.state_of(id).unwrap().clone()
}
fn ok(s: &JobState) -> bool {
    matches!(s, JobState::Done { info, .. } if info.outcome == ck::worker::Outcome::Complete)
}
fn settle(s: &mut Scheduler, ids: &[&str]) {
    wait_until(&format!("{ids:?} terminal"), || {
        s.tick();
        ids.iter().all(|i| st(s, i).is_terminal())
    });
}
/// Queue `a` for `clip` the way the panels do: wait for `waits_for`.
fn queue(s: &mut Scheduler, t: TaskJob) {
    let a = Action::of_task(&t).unwrap();
    let after = waits_for(s, &t.clip_root, a);
    s.submit_task(t, after).unwrap();
}

#[test]
fn another_clip_busy_says_queue_and_runs_after() {
    let (base, mut s) = sched("rules_other");
    let (a, b) = (base.join("clipA"), base.join("clipB"));
    assert_eq!(decide(&s, &b, Action::Hint, "Generate hint", None).label, "Generate hint");
    queue(&mut s, task(&base, "clipA", "ha", TaskKind::Gvm, "ok"));
    let d = decide(&s, &b, Action::Hint, "Generate hint", None);
    assert!(d.enabled && d.label == "Queue generate hint" && !d.reason.is_empty(), "{d:?}");
    assert!(waits_for(&s, &b, Action::Hint).is_empty(), "other clip: nothing to depend on, the single worker orders them");
    assert_eq!(waits_for(&s, &a, Action::Hint), ["ha"]);
    queue(&mut s, task(&base, "clipB", "hb", TaskKind::Gvm, "ok"));
    settle(&mut s, &["ha", "hb"]);
    assert!(ok(&st(&s, "ha")) && ok(&st(&s, "hb")));
}

#[test]
fn hint_queued_after_key_goes_first_and_key_waits_for_it() {
    let (base, mut s) = sched("rules_keyfirst");
    s.submit(key(&base, "clipA", "k1", "ok")).unwrap();
    // The button still asks for a hint; it does not wait for the queued Key.
    let root = base.join("clipA");
    assert!(waits_for(&s, &root, Action::Hint).is_empty());
    queue(&mut s, task(&base, "clipA", "h1", TaskKind::Gvm, "ok"));
    assert_eq!(s.job("k1").unwrap().after, ["h1"], "Key now consumes the hint");
    // Key never starts before the hint ended.
    wait_until("both done", || {
        s.tick();
        if !matches!(st(&s, "k1"), JobState::Queued) {
            assert!(ok(&st(&s, "h1")), "Key started before its hint finished: {:?}", st(&s, "h1"));
        }
        st(&s, "k1").is_terminal() && st(&s, "h1").is_terminal()
    });
    assert!(ok(&st(&s, "h1")) && ok(&st(&s, "k1")));
}

#[test]
fn hint_then_key_then_export_run_in_that_order() {
    let (base, mut s) = sched("rules_chain");
    let root = base.join("clipA");
    queue(&mut s, task(&base, "clipA", "h1", TaskKind::Gvm, "ok"));
    let after = waits_for(&s, &root, Action::Key);
    assert_eq!(after, ["h1"]);
    let mut k = key(&base, "clipA", "k1", "ok");
    k.after = after;
    s.submit(k).unwrap();
    let mut w = waits_for(&s, &root, Action::Export);
    w.sort();
    assert_eq!(w, ["h1", "k1"]);
    queue(&mut s, task(&base, "clipA", "e1", TaskKind::ExportVideo, "ok"));
    let e = s.task("e1").unwrap();
    assert!(e.after.contains(&"h1".to_string()) && e.after.contains(&"k1".to_string()), "{:?}", e.after);
    wait_until("all done", || {
        s.tick();
        let (h, k, e) = (st(&s, "h1"), st(&s, "k1"), st(&s, "e1"));
        if !matches!(k, JobState::Queued) {
            assert!(ok(&h), "Key before hint");
        }
        if !matches!(e, JobState::Queued) {
            assert!(ok(&k), "export before Key");
        }
        e.is_terminal()
    });
    assert!(ok(&st(&s, "h1")) && ok(&st(&s, "k1")) && ok(&st(&s, "e1")));
}

#[test]
fn missing_input_is_disabled_with_its_reason() {
    let (base, s) = sched("rules_missing");
    let root = base.join("clipA");
    let d = decide(&s, &root, Action::Track, "Track prompts", Some("SAM Base+ is missing — download it on the Models page".into()));
    assert!(!d.enabled);
    assert_eq!(d.label, "Track prompts");
    assert_eq!(d.reason, "SAM Base+ is missing — download it on the Models page");
    // A missing input wins over the queue state.
    let d = decide(&s, &root, Action::Key, "Key", Some("In is not marked".into()));
    assert!(!d.enabled && d.reason == "In is not marked");
}

#[test]
fn duplicate_click_says_already_queued() {
    let (base, mut s) = sched("rules_dup");
    let root = base.join("clipA");
    queue(&mut s, task(&base, "clipA", "h1", TaskKind::Gvm, "ok"));
    let d = decide(&s, &root, Action::Hint, "Generate hint", None);
    assert!(!d.enabled && d.label == "Already queued", "{d:?}");
    // Another action on that clip is a different step, not a duplicate.
    assert_eq!(decide(&s, &root, Action::Key, "Key", None).label, "Queue Key");
    s.submit(key(&base, "clipA", "k1", "ok")).unwrap_err(); // collides without the dependency: the panel passes it
    let mut k = key(&base, "clipA", "k1", "ok");
    k.after = waits_for(&s, &root, Action::Key);
    s.submit(k).unwrap();
    assert_eq!(decide(&s, &root, Action::Key, "Key", None).label, "Already queued");
}

#[test]
fn failed_predecessor_stops_the_dependent_job() {
    let (base, mut s) = sched("rules_fail");
    let root = base.join("clipA");
    queue(&mut s, task(&base, "clipA", "h1", TaskKind::Gvm, "fail"));
    let mut k = key(&base, "clipA", "k1", "ok");
    k.after = waits_for(&s, &root, Action::Key);
    s.submit(k).unwrap();
    settle(&mut s, &["h1", "k1"]);
    assert!(matches!(st(&s, "k1"), JobState::Failed { .. }), "{:?}", st(&s, "k1"));
    assert!(waiting_step_failed(&s, "k1"));
    assert!(!waiting_step_failed(&s, "h1"));
    assert!(!base.join("k1").exists(), "nothing was published by the dependent");
}

#[test]
fn queued_job_is_frozen_against_later_changes() {
    let (base, mut s) = sched("rules_frozen");
    let mut t = task(&base, "clipA", "h1", TaskKind::Gvm, "ok");
    queue(&mut s, t.clone());
    let queued = (*s.task("h1").unwrap().job).clone();
    // The user changes the settings afterwards: only a new job built from them differs; the queued one is untouched.
    t.params["variant"] = json!("Portrait");
    t.job_id = "h2".into();
    assert_eq!(*s.task("h1").unwrap().job, queued);
    assert_eq!(s.task("h1").unwrap().job.params["variant"], "Matting");
    settle(&mut s, &["h1"]);
    assert_eq!(s.task("h1").unwrap().job.params["variant"], "Matting");
}
