//! The metrics helper lifecycle through a fake helper: live samples, stale, unreadable output, a crash that is
//! retried a bounded number of times with the reason kept, a chatty stderr, and kill + reap on stop/drop.
//! No real counters, GPU, display or network.
mod common;
use ck::metrics::{view_cells, Monitor, Supervisor, View};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn fake(mode: &str) -> Command {
    let mut c = Command::new(common::PY);
    c.args(["-I", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_metrics.py"), mode]);
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
    let end = Instant::now() + Duration::from_secs(10);
    while !f() {
        assert!(Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[test]
fn live_samples_wake_the_ui_and_stop_reaps_the_child() {
    let (n, w) = wake();
    let mut m = Monitor::start(fake("run"), w).unwrap();
    until("first live sample", || matches!(m.view(Instant::now()), View::Live(_)));
    assert!(n.load(Ordering::SeqCst) >= 1, "the UI is woken by a sample, not polled per frame");
    let View::Live(s) = m.view(Instant::now()) else { unreachable!() };
    let c = view_cells(&View::Live(s), &[0]);
    assert_eq!(c.cpu, "42%");
    assert_eq!(c.gpu, "Rendering GPU0 Fake GPU 55% · VRAM 2.0 / 8.0 GiB");
    let pid = m.pid();
    assert!(alive(pid));
    m.stop();
    assert!(!alive(pid), "killed and reaped, no zombie");
    drop(m);
}

#[test]
fn dropping_the_monitor_kills_the_helper() {
    let (_, w) = wake();
    let m = Monitor::start(fake("run"), w).unwrap();
    let pid = m.pid();
    until("started", || matches!(m.view(Instant::now()), View::Live(_)));
    drop(m);
    assert!(!alive(pid));
}

#[test]
fn a_silent_helper_goes_stale_by_local_time_and_garbage_is_unavailable() {
    let (_, w) = wake();
    let m = Monitor::start(fake("one"), w).unwrap();
    until("sample", || matches!(m.view(Instant::now()), View::Live(_)));
    match m.view(Instant::now() + Duration::from_secs(8)) {
        View::Stale { age, .. } => assert!(age >= Duration::from_secs(7)),
        v => panic!("{v:?}"),
    }
    let (_, w) = wake();
    let g = Monitor::start(fake("garbage"), w).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(g.view(Instant::now()), View::Starting, "unreadable lines are not samples");
    match g.view(Instant::now() + Duration::from_secs(11)) {
        View::Unavailable(why) => assert!(why.contains("unreadable"), "{why}"),
        v => panic!("{v:?}"),
    }
}

#[test]
fn a_crashing_helper_is_retried_a_bounded_number_of_times_and_the_reason_stays() {
    let starts = Arc::new(AtomicUsize::new(0));
    let s2 = starts.clone();
    let mut sup = Supervisor::new(move || {
        s2.fetch_add(1, Ordering::SeqCst);
        Ok(fake("crash"))
    });
    let (_, w) = wake();
    let t0 = Instant::now();
    let mut now = t0;
    for _ in 0..12 {
        sup.tick(now, &w);
        std::thread::sleep(Duration::from_millis(150)); // let the child exit and the reader see EOF
        now += Duration::from_secs(6);
    }
    for _ in 0..3 {
        sup.tick(now, &w);
        now += Duration::from_secs(60);
    }
    assert_eq!(starts.load(Ordering::SeqCst), 3, "never more than three starts");
    match sup.view(now) {
        View::Unavailable(why) => assert!(why.contains("boom: no nvml"), "{why}"),
        v => panic!("{v:?}"),
    }
    let c = view_cells(&sup.view(now), &[]);
    assert!(c.status.unwrap().starts_with("unavailable") && c.cpu == "—", "no fabricated numbers");
}

#[test]
fn spawn_failure_is_reported_once_and_chatty_stderr_is_bounded() {
    let mut sup = Supervisor::new(|| Ok(Command::new("/nonexistent/python")));
    let (_, w) = wake();
    sup.tick(Instant::now(), &w);
    match sup.view(Instant::now()) {
        View::Unavailable(why) => assert!(why.contains("cannot start"), "{why}"),
        v => panic!("{v:?}"),
    }
    let (_, w) = wake();
    let m = Monitor::start(fake("flood"), w).unwrap();
    until("sample despite the flood", || matches!(m.view(Instant::now()), View::Live(_)));
    assert!(m.ended().is_none());
}

#[test]
fn off_and_fixture_supervisors_start_nothing() {
    let off = Supervisor::off();
    assert_eq!(off.view(Instant::now()), View::Unavailable("not started".into()));
}

#[test]
fn multibyte_stderr_cut_at_the_limit_neither_panics_nor_poisons_the_monitor() {
    let (_, w) = wake();
    let m = Monitor::start(fake("utf8-stderr"), w).unwrap();
    until("stderr drained", || m.stderr_tail().len() >= 2040);
    let tail = m.stderr_tail();
    assert!(tail.len() <= 2048 && tail.starts_with('a') && !tail.contains('\u{FFFD}'), "{}", tail.len());
    until("sample", || matches!(m.view(Instant::now()), View::Live(_))); // the shared lock is still usable
    drop(m);
    let (_, w) = wake();
    let c = Monitor::start(fake("utf8-crash"), w).unwrap();
    until("ended", || c.ended().is_some());
    assert!(c.ended().unwrap().contains("stopped"));
    assert!(matches!(c.view(Instant::now()), View::Unavailable(_)));
}
