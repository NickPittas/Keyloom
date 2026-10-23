//! The bridge client lifecycle through a fake bridge (and the real `python -m backend.netlink` for the ready
//! handshake): correlation, error codes, unreadable/overlong output, EOF, close, drop and no-retry behaviour.
//! Local subprocesses only: no credentials, network, GPU or display.
mod common;
use ck::netlink::{bridge_command, Link, Op, State};
use serde_json::json;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fake(mode: &str) -> Command {
    let mut c = Command::new(common::PY);
    c.args(["-I", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_netlink.py"), mode]);
    c
}
fn wake() -> (Arc<AtomicUsize>, Arc<dyn Fn() + Send + Sync>) {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    (n, Arc::new(move || {
        n2.fetch_add(1, Ordering::SeqCst);
    }))
}
fn until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}
fn ready(mode: &str) -> Link {
    let (_, w) = wake();
    let l = Link::start(fake(mode), w).unwrap();
    until("ready", || l.state() == State::Ready);
    l
}
fn collect(l: &mut Link, n: usize) -> Vec<ck::netlink::Reply> {
    let mut all = vec![];
    until("replies", || {
        all.extend(l.poll());
        all.len() >= n
    });
    all
}

#[test]
fn replies_are_correlated_to_their_requests_and_wake_the_ui() {
    let (n, w) = wake();
    let mut l = Link::start(fake("echo"), w).unwrap();
    until("ready", || l.state() == State::Ready);
    let a = l.request(Op::Submit, json!({"k": 1})).unwrap();
    let b = l.request(Op::Stats, json!({"seq": 2})).unwrap();
    assert_ne!(a, b);
    assert_eq!(l.pending(), 2);
    let r = collect(&mut l, 2);
    assert_eq!(r[0].id.as_deref(), Some(a.as_str()));
    assert_eq!(r[0].outcome, Ok(json!({"op": "submit", "args": {"k": 1}})));
    assert_eq!(r[1].outcome.as_ref().unwrap()["op"], "stats");
    assert_eq!(l.pending(), 0);
    assert!(n.load(Ordering::SeqCst) >= 3, "woken by ready and by replies, not polled per frame");
}

#[test]
fn error_codes_pass_through_untouched() {
    let mut l = ready("echo");
    l.request(Op::Job, json!({"job_id": "job_1"})).unwrap();
    let e = collect(&mut l, 1).remove(0).outcome.unwrap_err();
    assert_eq!(e.code, "busy");
    assert!(e.is_busy());
    assert!(e.message.contains("credentials"));
}

#[test]
fn bad_arguments_and_oversize_requests_are_refused_locally_and_nothing_is_queued() {
    let mut l = ready("echo");
    assert_eq!(l.request(Op::Job, json!([1])).unwrap_err().code, "bad_request");
    assert_eq!(l.request(Op::Submit, json!({"big": "x".repeat(1 << 20)})).unwrap_err().code, "too_large");
    assert_eq!(l.pending(), 0);
    l.request(Op::Installations, json!({})).unwrap();
    collect(&mut l, 1);
}

#[test]
fn pending_requests_are_bounded() {
    let mut l = ready("hang");
    for _ in 0..ck::netlink::MAX_PENDING {
        l.request(Op::Installations, json!({})).unwrap();
    }
    assert_eq!(l.request(Op::Installations, json!({})).unwrap_err().code, "busy_link");
}

#[test]
fn eof_ends_the_link_and_each_waiting_request_gets_one_explicit_answer() {
    let (_, w) = wake();
    let mut l = Link::start(fake("eof"), w).unwrap();
    until("ended", || matches!(l.state(), State::Ended(_)));
    assert_eq!(l.request(Op::Installations, json!({})).unwrap_err().code, "link_ended");
    let mut l = ready("hang");
    let ids: Vec<_> = (0..3).map(|_| l.request(Op::Submit, json!({})).unwrap()).collect();
    let pid = l.pid();
    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    let mut got = vec![];
    until("link_ended replies", || {
        got.extend(l.poll());
        got.len() >= 3
    });
    assert_eq!(got.iter().map(|r| r.id.clone().unwrap()).collect::<Vec<_>>(), ids, "in request order, none lost");
    assert!(got.iter().all(|r| r.outcome.as_ref().unwrap_err().code == "link_ended"));
    assert!(got[0].outcome.as_ref().unwrap_err().message.contains("may have been processed"));
    assert!(l.poll().is_empty(), "answered exactly once; nothing is retried");
    assert!(!alive(pid));
}

#[test]
fn unreadable_overlong_stray_and_wrong_protocol_output_is_a_visible_failure() {
    for mode in ["garbage", "oversize", "badproto"] {
        let (_, w) = wake();
        let mut l = Link::start(fake(mode), w).unwrap();
        until(mode, || matches!(l.state(), State::Ended(_)));
        let State::Ended(why) = l.state() else { unreachable!() };
        assert!(!why.is_empty(), "{mode}");
        let pid = l.pid();
        l.poll();
        assert!(!alive(pid), "{mode}: an ended link is killed and reaped");
    }
    let mut l = ready("stray");
    until("stray counted", || l.stray_replies() == 1);
    assert!(l.poll().is_empty(), "a reply nobody asked for is never delivered");
}

#[test]
fn stderr_is_bounded_and_never_blocks_the_bridge() {
    let mut l = ready("stderr");
    l.request(Op::Installations, json!({})).unwrap();
    collect(&mut l, 1);
    assert!(l.stderr_tail().len() <= 2048);
}

#[test]
fn close_is_graceful_and_reaps_and_drop_kills() {
    let mut l = ready("echo");
    let pid = l.pid();
    l.close();
    l.close();
    assert!(!alive(pid));
    assert_eq!(l.request(Op::Installations, json!({})).unwrap_err().code, "link_ended");
    let l = ready("hang");
    let pid = l.pid();
    drop(l);
    assert!(!alive(pid), "dropped link leaves no child");
    let mut l = ready("ignore-eof");
    let pid = l.pid();
    l.close(); // does not exit on close/EOF: killed after the grace period
    assert!(!alive(pid));
}

#[test]
fn the_link_never_resubmits_after_it_ends() {
    let mut l = ready("hang");
    l.request(Op::Submit, json!({"submission_id": "s"})).unwrap();
    let pid = l.pid();
    l.close();
    assert!(!alive(pid));
    let r = l.poll();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].outcome.as_ref().unwrap_err().code, "link_ended");
}

#[test]
fn the_real_bridge_says_ready_answers_in_order_and_closes() {
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let py = repo.join(".venv/bin/python");
    if !py.is_file() {
        eprintln!("skipped: no .venv python");
        return;
    }
    let (_, w) = wake();
    let mut l = Link::start(bridge_command(repo, Some(&py)), w).unwrap();
    until("real bridge ready", || l.state() == State::Ready);
    l.request(Op::Installations, json!({})).unwrap();
    l.request(Op::Connect, json!({"credentials_path": "/nonexistent/ck-creds.json"})).unwrap();
    l.request(Op::Job, json!({"job_id": "../etc"})).unwrap();
    let r = collect(&mut l, 3);
    let code = |i: usize| r[i].outcome.as_ref().unwrap_err().code.clone();
    assert_eq!((code(0).as_str(), code(1).as_str(), code(2).as_str()), ("not_connected", "io", "not_connected"));
    assert!(!r.iter().any(|x| format!("{x:?}").to_lowercase().contains("token")));
    let pid = l.pid();
    l.close();
    assert!(!alive(pid));
}

#[test]
fn multibyte_stderr_cut_at_the_limit_never_panics_or_poisons() {
    let mut l = ready("stderr-utf8"); // 'a' + 1500 two-byte characters: the 2048-byte cut lands INSIDE a character
    until("stderr drained", || l.stderr_tail().len() >= 2040);
    let tail = l.stderr_tail();
    assert!(tail.len() <= 2048 && tail.starts_with('a') && !tail.contains('\u{FFFD}'), "{}", tail.len());
    l.request(Op::Installations, json!({})).unwrap(); // mutex still usable: state/poll/request/drop all work
    assert!(matches!(l.state(), State::Ready));
    collect(&mut l, 1);
    drop(l);
}

#[test]
fn replies_that_arrive_out_of_order_are_matched_by_id() {
    let mut l = ready("reverse");
    let first = l.request(Op::Installations, json!({})).unwrap(); // the fake answers this one after the next request
    let second = l.request(Op::Submit, json!({"n": 2})).unwrap();
    let r = collect(&mut l, 2);
    assert_eq!(r[0].id.as_deref(), Some(second.as_str()), "answered first");
    assert_eq!(r[1].id.as_deref(), Some(first.as_str()));
    assert_eq!(r[1].outcome.as_ref().unwrap()["late"], true);
    assert_eq!(l.pending(), 0);
}

#[test]
fn a_silent_child_fails_the_ready_check_by_injected_clock_not_by_waiting() {
    let (_, w) = wake();
    let mut l = Link::start(fake("silent"), w).unwrap();
    assert_eq!(l.state(), State::Starting);
    let id = l.request(Op::Installations, json!({})).unwrap();
    let later = Instant::now() + Duration::from_secs(60);
    assert!(matches!(l.state_at(later), State::Ended(_)));
    let r = l.poll_at(later);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].id.as_deref(), Some(id.as_str()));
    assert_eq!(r[0].outcome.as_ref().unwrap_err().code, "link_ended");
    assert!(!alive(l.pid()), "killed and reaped");
}
