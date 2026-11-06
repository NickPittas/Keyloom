//! A7: app-to-app routing. Two PCs in one test process: PC A is a sender (`RemoteSet`, the sender-side status loop) and PC B is a
//! real app endpoint (`AppApi`, the embedded Python HTTPS API with its durable inbox on a temp state dir and a free port) with
//! the receiving half (`Inbound`) and its own scheduler running a fake worker. Nothing here touches the user's services, settings
//! or models; the state dirs are temporary and the ports are picked at random. Run each test alone, through
//! `rust-ui/tools/isolated-test.sh`.
mod common;
use ck::app_api::{self, AppApi, Paths, Status};
use ck::contracts::{ClipOutputValues, OutputLocation, RangeIdentity};
use ck::inbound::Inbound;
use ck::jobs::{common_dir, resolve_outputs, Require};
use ck::metrics::{Sample, View};
use ck::netlink::{Link, Op, Source, State};
use ck::remote::Env;
use ck::remote_set::RemoteSet;
use ck::scheduler::{Placement, Submit};
use ck::settings::{Connection, Settings};
use ck::submission::JobSpec;
use ck::ui::ai::{AiUi, NetSubmit};
use ck::workflow::{OutputCfg, Params};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// One interpreter per process and one endpoint per interpreter: tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A project on one PC: `<mount>/projects/Show/clips/shot_01` with 3 frames named f_1101.. and a full AlphaHint.
fn project(mount: &Path) {
    let clip = mount.join("projects/Show/clips/shot_01");
    for d in ["Frames", "AlphaHint"] {
        std::fs::create_dir_all(clip.join(d)).unwrap();
        for i in 0..3 {
            let mut e = png::Encoder::new(std::fs::File::create(clip.join(d).join(format!("f_{}.png", 1101 + i))).unwrap(), 2, 2);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[9; 4]).unwrap();
        }
    }
    std::fs::write(clip.join("clip.json"), "{}").unwrap();
    std::fs::write(mount.join("projects/Show/project.json"), r#"{"version":2}"#).unwrap();
    std::fs::create_dir_all(mount.join("out")).unwrap();
}

fn roots(m: &Path) -> Value {
    json!([{"name": "projects", "local": m.join("projects")}, {"name": "out", "local": m.join("out")}])
}

/// The frozen shot PC A sends: version `n` has its own output folder, so a second shot never collides with the first.
fn spec(mount_a: &Path, n: u32) -> JobSpec {
    let roots_a: Vec<ck::shared_paths::SharedRoot> = serde_json::from_value(roots(mount_a)).unwrap();
    let loc = OutputLocation { root: mount_a.join("out"), folder_template: "{project}/{name}_{version}/{output}".into(), file_template: "{name}_{output}_####".into() };
    let values = ClipOutputValues { project: "Show".into(), name: "shot_01".into(), resolution: "HD".into(), date: "2026-10-23".into(), version: n };
    let o = OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() };
    let stems: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
    let outputs = resolve_outputs(&loc, &values, &o.kinds(), &stems).unwrap();
    let mdir = common_dir(&outputs).unwrap();
    JobSpec::from_parts(
        &roots_a,
        &mount_a.join("projects/Show/clips/shot_01"),
        &mdir,
        "clip-1",
        RangeIdentity { in_point: 0, out_point: 2, in_stem: "f_1101".into(), out_stem: "f_1103".into() },
        &Params { hint_mode: "none".into(), temporal_window: "1/1".into(), ..Params::default() },
        &o,
        None,
        &loc,
        &values,
        Require { protocol: 1, models: [("corridorkey_v2".to_string(), Some("aa".to_string()))].into() },
        Some("Quality"),
    )
    .unwrap()
}

/// A live hardware sample of PC B with this GPU load (percent) and VRAM use (GiB of 24).
fn sample(util: f64, vram_gib: u64) -> View {
    let gib = 1u64 << 30;
    let s = json!({"cpu_percent": 5.0, "ram_used_bytes": 8 * gib, "ram_total_bytes": 64 * gib,
        "gpus": [{"uuid": "GPU-0a1b2c3d-0000-4000-8000-0123456789ab", "index": 0, "name": "Fake GPU", "utilization_percent": util, "vram_used_bytes": vram_gib * gib, "vram_total_bytes": 24 * gib}]});
    View::Live(Sample::from_api(&s).unwrap().unwrap().0)
}

struct World {
    base: PathBuf,
    mount_a: PathBuf,
    mount_b: PathBuf,
    sd: PathBuf,
    paths: Paths,
    api: Option<AppApi>,
    woke: Arc<AtomicU64>,
    host_iid: String,
    ns: String,
    creds: PathBuf,
    // PC B: the receiving app
    b_ai: AiUi,
    b_in: Inbound,
    b_settings: Settings,
    b_view: View,
    // PC A: the sender
    a_ai: AiUi,
    a_set: RemoteSet,
    a_settings: Settings,
    wake: Arc<dyn Fn() + Send + Sync>,
    // PC B as a sender too (the previous host sending to its registered PCs): off unless a test turns it on
    b_set: RemoteSet,
    b_side: bool,
}

fn wait_status(api: &AppApi, ok: impl Fn(&Status) -> bool) -> Status {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        let s = api.status();
        if ok(&s) {
            return s;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("status never settled: {:?}", api.status());
}

/// `worker`: the fake worker's behaviour (for example `{"modes": {"key": "hold"}}` keeps a Key job running until it is cancelled).
fn world(tag: &str, worker: Value) -> Option<World> {
    let paths = match ck::runtime::embed_paths(&ck::runtime::launch(Some(&repo()))) {
        Ok(p) => p,
        Err(_) => {
            eprintln!("SKIPPED: no Keyloom runtime was found (no install, no CK_PYTHON, no repo .venv)");
            return None;
        }
    };
    let base = common::tmp(tag);
    let (mount_a, mount_b) = (base.join("pcA/nas"), base.join("pcB/mnt/share"));
    project(&mount_a);
    project(&mount_b);
    ck::queue::set_settings_path(Some(base.join("settings.json")));

    // PC B's endpoint: temp state, a free port, address join allowed.
    let port = free_port();
    let sd = base.join("state/server");
    std::fs::create_dir_all(&sd).unwrap();
    std::fs::write(sd.join("service.json"), json!({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": true}).to_string()).unwrap();
    let woke = Arc::new(AtomicU64::new(0));
    let w2 = woke.clone();
    let api = AppApi::start(paths.clone(), sd.clone(), Arc::new(move || {
        w2.fetch_add(1, Ordering::Relaxed);
    }));
    let Status::Running(info) = wait_status(&api, |s| !matches!(s, Status::Starting)) else { panic!("endpoint did not start: {:?}", api.status()) };
    let host_iid = info["host_installation_id"].as_str().expect("the app registered itself").to_owned();
    let ns = info["network_id"].as_str().unwrap().to_owned();

    // PC A joins B by address (a separate process writes the credentials file, as the Clients page does through the link).
    let creds = base.join("creds.json");
    let join = "import sys; sys.path.insert(0, sys.argv[1]); from backend.coordinator.client import join_to_file; join_to_file(f'https://127.0.0.1:{sys.argv[2]}', sys.argv[4], 'sender-pc', pem=open(sys.argv[3]).read())";
    let out = std::process::Command::new(ck::import::python(&repo())).arg("-c").arg(join).arg(repo()).arg(port.to_string()).arg(sd.join("server-cert.pem")).arg(&creds).output().expect("join runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));

    // PC B's scheduler with a fake worker that reports a verified GPU.
    let mut cfg = worker;
    cfg["uuid"] = json!("GPU-0a1b2c3d-0000-4000-8000-0123456789ab");
    let frepo = common::make_repo(&base, "repo", cfg);
    let b_settings: Settings = serde_json::from_value(json!({"version": 1, "shared_roots": roots(&mount_b),
        "ai_targets": [{"id": "l0", "kind": "local", "repo": frepo, "python": common::PY, "gpu": 0}]})).unwrap();
    let mut b_ai = AiUi::new(&b_settings, Some(&frepo));
    b_ai.set_models(ck::models::Snapshot::for_tests());
    b_ai.start_workers();

    let a_settings = {
        let mut s: Settings = serde_json::from_value(json!({"version": 1, "shared_roots": roots(&mount_a)})).unwrap();
        s.network.connections = vec![Connection {
            credentials: creds.display().to_string(),
            enabled: true,
            receive: true,
            endpoints: vec![format!("https://127.0.0.1:{port}")],
            network_id: ns.clone(),
            host_installation_id: host_iid.clone(),
            host_name: "PC B".into(),
            ..Default::default()
        }];
        s
    };
    let paths_for_b = paths.clone();
    Some(World {
        base,
        mount_a,
        mount_b,
        sd,
        a_set: RemoteSet::new(Source::Embedded(paths.clone())),
        paths,
        api: Some(api),
        woke,
        host_iid,
        ns,
        creds,
        b_ai,
        b_in: Inbound::new(),
        b_settings,
        b_view: sample(5.0, 1),
        a_ai: AiUi::new(&Settings::default(), None),
        a_settings,
        wake: Arc::new(|| {}),
        b_set: RemoteSet::new(Source::Embedded(paths_for_b)),
        b_side: false,
    })
}

impl World {
    /// One pass of both PCs: B's scheduler and receiver, then A's sender loop.
    fn pump(&mut self, a: bool) {
        let now = Instant::now();
        self.b_ai.start_workers(); // the app does this every frame: one idle worker per GPU, started when the app opens
        self.b_ai.exec_tick();
        {
            let mut env = Env { settings: &self.b_settings, settings_path: None, ai: &mut self.b_ai, metrics: &self.b_view, repo: None, python: None, wake: &self.wake, cfg: Default::default() };
            self.b_in.tick(now, &mut env);
        }
        // The app's own wiring: a registered PC's endpoint becomes a connection; the open project re-reads received history on a change.
        if self.b_in.take_history_changed() {
            self.b_ai.queue_refresh_received();
        }
        if self.b_side {
            if self.b_in.take_destinations_changed() {
                let dests = self.b_in.destinations().to_vec();
                ck::inbound::add_destinations(&mut self.b_settings.network, &dests);
            }
            let mut env = Env { settings: &self.b_settings, settings_path: None, ai: &mut self.b_ai, metrics: &self.b_view, repo: None, python: None, wake: &self.wake, cfg: Default::default() };
            self.b_set.tick(now, &mut env);
            ck::inbound::pump_forwards(&mut self.b_in, &mut self.b_set, &mut env);
        }
        if a {
            let view = View::Unavailable(String::new());
            let mut env = Env { settings: &self.a_settings, settings_path: None, ai: &mut self.a_ai, metrics: &view, repo: None, python: None, wake: &self.wake, cfg: Default::default() };
            self.a_set.tick(now, &mut env);
        }
        std::thread::sleep(Duration::from_millis(15));
    }

    fn until(&mut self, what: &str, secs: u64, mut ok: impl FnMut(&mut World) -> bool) {
        let end = Instant::now() + Duration::from_secs(secs);
        while !ok(self) {
            assert!(Instant::now() < end, "timed out waiting for {what}: {}", self.diag());
            self.pump(true);
        }
    }

    /// Push shot `n` from A to B through A's sender loop. Returns the submission id.
    fn push(&mut self, n: u32) -> String {
        self.push_as(n, &self.host_iid.clone(), false)
    }

    /// Push shot `n` from A, aimed at the PC `pc` of B's network (B itself or a PC registered on B); `automatic` as Automatic sends it.
    fn push_as(&mut self, n: u32, pc: &str, automatic: bool) -> String {
        let submission_id = format!("sub_{n:0>20}");
        let target = Some(format!("{}/{}", self.ns, pc));
        let net = NetSubmit { submission_id: submission_id.clone(), spec: spec(&self.mount_a, n), target, folder: "shot_01".into(), name: format!("shot_01 v{n}"), automatic };
        let view = View::Unavailable(String::new());
        let mut env = Env { settings: &self.a_settings, settings_path: None, ai: &mut self.a_ai, metrics: &view, repo: None, python: None, wake: &self.wake, cfg: Default::default() };
        self.a_set.send(net, &mut env).expect("routed to B");
        submission_id
    }

    /// What A knows, for a failure message.
    fn diag(&self) -> String {
        let installs: Vec<String> = self.a_set.installs().iter().map(|i| format!("{} gpu={:?} connected={} stats={}", i.name, i.gpu, i.connected, i.stats)).collect();
        let gpu = self.b_ai.scheduler().targets().into_iter().find(|t| t.local).and_then(|t| t.ready).and_then(|r| r.gpu).map(|g| g.name);
        format!("b_note={:?} b_gpu={gpu:?} online={} installs={installs:?} free={:?} jobs={:?}", self.b_in.note, self.a_set.online(), self.free(), self.a_set.jobs().iter().map(|j| (j.state.clone(), j.reason.clone())).collect::<Vec<_>>())
    }

    fn state_of(&self, sid: &str) -> Option<String> {
        self.a_set.jobs().iter().find(|j| j.submission_id == sid).map(|j| j.state.clone())
    }

    fn free(&self) -> Vec<String> {
        self.a_set.free_candidates(&self.a_settings, None)
    }

    fn stop_b(&mut self) {
        assert!(self.api.take().expect("running").stop(Duration::from_secs(20)));
    }

    fn start_b(&mut self) {
        let api = AppApi::start(self.paths.clone(), self.sd.clone(), Arc::new(|| {}));
        assert!(matches!(wait_status(&api, |s| !matches!(s, Status::Starting)), Status::Running(_)));
        self.api = Some(api);
    }

    fn history(&self, job_id: &str) -> Option<ck::queue::Entry> {
        let p = ck::queue::history_path()?;
        ck::queue::read(&p).0.into_iter().find(|e| e.id == format!("received:{job_id}"))
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(api) = self.api.take() {
            api.stop(Duration::from_secs(20));
        }
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn job_id(w: &World, sid: &str) -> Option<String> {
    w.a_set.jobs().iter().find(|j| j.submission_id == sid).and_then(|j| j.job_id.clone())
}

fn child_processes() -> Vec<String> {
    let mut out = vec![];
    for t in std::fs::read_dir("/proc/self/task").unwrap().filter_map(Result::ok) {
        if let Ok(c) = std::fs::read_to_string(t.path().join("children")) {
            out.extend(c.split_whitespace().map(str::to_owned));
        }
    }
    out
}

// ------------------------------------------------------------------------------------------------ the proofs

/// A pushes a shot to B's app: it lands in B's inbox, B's own scheduler runs it, and A's status loop sees the progress and the result.
#[test]
fn a_push_lands_in_bs_inbox_runs_on_bs_scheduler_and_as_status_loop_sees_progress_and_result() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-push", json!({"key_delay": 1.5})) else { return };
    w.until("A to reach B", 40, |w| w.a_set.online());
    let sid = w.push(1);
    w.until("B's app to accept the shot", 30, |w| job_id(w, &sid).is_some());
    let job = job_id(&w, &sid).unwrap();
    let woke = w.woke.load(Ordering::Relaxed);
    assert!(woke >= 2, "the commit woke the app driver ({woke} wakes)");
    // B's receiver took it from the inbox into its scheduler.
    w.until("B's scheduler to take it", 30, |w| w.b_ai.scheduler().running());
    assert!(w.b_in.receiving().is_some(), "the receiver follows the shot it handed to the scheduler");
    // A's status loop shows it running with progress.
    w.until("A to see it started", 30, |w| w.state_of(&sid).as_deref() == Some("started"));
    // ... and its final result.
    w.until("A to see the result", 60, |w| w.state_of(&sid).as_deref() == Some("succeeded"));
    let j = w.a_set.jobs().into_iter().find(|j| j.submission_id == sid).unwrap().clone();
    assert!(j.total >= 1 && j.done == j.total, "progress {}/{}", j.done, j.total);
    assert!(w.mount_b.join("out/Show/shot_01_v001/Matte/shot_01_Matte_1101.png").is_file(), "the result is on B's shared storage");
    let h = w.history(&job).expect("history records the received shot");
    assert_eq!((h.kind.as_str(), h.state.as_str()), ("received", "succeeded"));
    let pending = app_api::call("inbox", json!({"method": "pending"})).unwrap();
    assert!(pending.as_array().unwrap().is_empty(), "nothing is left in the inbox");
    assert!(w.b_in.receiving().is_none() && !w.b_ai.scheduler().running());
    assert!(w.a_set.online() && w.a_set.installs().iter().any(|i| i.connected), "B shows connected after real contact");
    for c in child_processes() {
        let cmd = std::fs::read_to_string(format!("/proc/{c}/cmdline")).unwrap_or_default();
        assert!(!cmd.contains("netlink") && !cmd.contains("network_service"), "no service or bridge process: {cmd}");
    }
}

/// While B runs the first shot, a second one waits in B's queue (A sees it queued), and starts when the first ends.
#[test]
fn a_second_shot_queues_while_b_is_busy() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-busy", json!({"modes": {"key": "hold"}})) else { return };
    w.until("A to reach B", 40, |w| w.a_set.online());
    let first = w.push(1);
    w.until("the first shot to run on B", 40, |w| w.b_in.receiving().is_some() && w.state_of(&first).as_deref() == Some("started"));
    let second = w.push(2);
    w.until("the second shot to wait", 40, |w| w.state_of(&second).as_deref() == Some("queued") && w.b_in.waiting() == 1);
    let pending = app_api::call("inbox", json!({"method": "pending"})).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1, "the waiting shot stays in the inbox, not rejected");
    // The busy PC is not free for Automatic, but an explicit pick still queues.
    assert!(w.free().is_empty());
    let (creds, first_job) = (w.creds.display().to_string(), job_id(&w, &first).unwrap());
    w.a_set.cancel_job(&creds, &first_job);
    w.until("the first to end and the second to start", 60, |w| w.state_of(&first).as_deref() == Some("cancelled") && w.state_of(&second).as_deref() == Some("started"));
    let second_job = job_id(&w, &second).unwrap();
    w.a_set.cancel_job(&creds, &second_job);
    w.until("the second to end", 60, |w| w.state_of(&second).as_deref() == Some("cancelled"));
}

/// An offline B keeps its registration (and A keeps its saved connection); it shows online again after contact.
#[test]
fn an_offline_b_keeps_its_registration_and_shows_online_again_after_contact() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-offline", json!({})) else { return };
    w.until("A to reach B", 40, |w| w.a_set.online());
    let registered = |w: &World| -> Value { let _ = w; app_api::call("registered", json!({})).unwrap() };
    let before = registered(&w);
    assert!(before.as_array().unwrap().iter().any(|r| r["name"] == "sender-pc" && r["connected"] == true), "A registered on B and is connected: {before}");
    w.stop_b();
    w.until("A to notice B is gone", 40, |w| !w.a_set.online());
    assert!(w.free().is_empty(), "an offline PC is never free");
    assert_eq!(w.a_settings.network.connections.len(), 1, "A's saved connection is untouched");
    w.start_b();
    let after = registered(&w);
    assert!(after.as_array().unwrap().iter().any(|r| r["name"] == "sender-pc"), "the registration survived the outage: {after}");
    // A discovery answer from B's network reconnects at once (otherwise the retry timer would).
    let ns = w.ns.clone();
    w.a_set.reachable(&ns);
    w.until("A to be online again", 40, |w| w.a_set.online() && w.a_set.installs().iter().any(|i| i.id == w.host_iid && i.connected));
    let again = registered(&w);
    assert!(again.as_array().unwrap().iter().any(|r| r["name"] == "sender-pc" && r["connected"] == true), "online again from contact: {again}");
}

/// Cancel from A reaches B's app, then B's queue and worker.
#[test]
fn cancel_from_a_reaches_b() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-cancel", json!({"modes": {"key": "hold"}})) else { return };
    w.until("A to reach B", 40, |w| w.a_set.online());
    let sid = w.push(1);
    w.until("the shot to run on B", 40, |w| w.b_in.receiving().is_some() && w.state_of(&sid).as_deref() == Some("started"));
    let (creds, job) = (w.creds.display().to_string(), job_id(&w, &sid).unwrap());
    w.a_set.cancel_job(&creds, &job);
    w.until("B to stop it", 60, |w| w.state_of(&sid).as_deref() == Some("cancelled") && w.b_in.receiving().is_none());
    assert!(!w.b_ai.scheduler().running(), "B's worker is free again");
    assert_eq!(w.history(&job).expect("history").state, "cancelled");
}

/// A retry after an uncertain acknowledgment (the same submission id sent twice) creates no second shot.
#[test]
fn a_retry_after_an_uncertain_acknowledgment_creates_no_duplicate() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-retry", json!({"key_delay": 0.1})) else { return };
    let mut link = Link::start_embedded(w.paths.clone(), Arc::new(|| {})).unwrap();
    let end = Instant::now() + Duration::from_secs(60);
    while link.state() == State::Starting && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let id = link.request(Op::Connect, json!({"credentials_path": w.creds.display().to_string()})).unwrap();
    let mut replies = vec![];
    while replies.is_empty() && Instant::now() < end {
        replies = link.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(replies[0].id.as_deref(), Some(id.as_str()));
    assert!(replies[0].outcome.is_ok(), "{:?}", replies[0].outcome);
    // The first answer is "lost": the sender cannot know, so it sends the very same submission again.
    let body = json!({"submission_id": "sub_00000000000000000009", "spec": serde_json::to_value(spec(&w.mount_a, 9)).unwrap(), "target": null});
    link.request(Op::Submit, body.clone()).unwrap();
    link.request(Op::Submit, body).unwrap();
    let mut got = vec![];
    while got.len() < 2 && Instant::now() < end {
        got.extend(link.poll());
        std::thread::sleep(Duration::from_millis(10));
    }
    let jobs: Vec<String> = got.iter().map(|r| r.outcome.as_ref().expect("accepted")["job_id"].as_str().unwrap().to_owned()).collect();
    assert_eq!(jobs[0], jobs[1], "the same shot, not a second one");
    let pending = app_api::call("inbox", json!({"method": "pending"})).unwrap();
    assert_eq!(pending.as_array().unwrap().len(), 1, "one row in B's inbox");
    link.close(); // releases the credentials, so A's own loop can connect meanwhile
    // B runs it exactly once.
    w.until("B to finish it", 60, |w| w.history(&jobs[0]).is_some_and(|h| h.state == "succeeded"));
    let received = ck::queue::read(&ck::queue::history_path().unwrap()).0.into_iter().filter(|e| e.id == format!("received:{}", jobs[0])).count(); // this shot only: the history file is shared by the process
    assert_eq!(received, 1, "one history entry, one run");
}

/// Automatic only ever picks a free PC. B publishes its load every 5 s and A reads it every 4 s, so each change takes a few seconds.
fn free_b(w: &mut World) -> String {
    let want = format!("network:{}/{}", w.ns, w.host_iid);
    let w2 = want.clone();
    w.until("B to be free for Automatic", 25, move |w| w.free() == [w2.clone()]);
    assert_eq!(w.a_set.automatic_candidates(&w.a_settings, None).len(), 1);
    want
}

/// Not over 50% load: a PC that reports 90% is skipped, and it is picked again once it is light.
#[test]
fn automatic_skips_a_pc_over_half_load() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-auto-load", json!({})) else { return };
    let want = free_b(&mut w);
    w.b_view = sample(90.0, 20);
    w.until("B to report its load", 25, |w| w.free().is_empty());
    w.b_view = sample(5.0, 1);
    w.until("B to be free again", 25, |w| w.free() == [want.clone()]);
}

/// A PC busy with a shot of ours is not free, whatever its load; a finished shot frees it.
#[test]
fn automatic_skips_a_busy_pc() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-auto-busy", json!({"modes": {"key": "hold"}})) else { return };
    let want = free_b(&mut w);
    let sid = w.push(1);
    assert!(w.free().is_empty(), "sending marks the PC busy at once");
    w.until("the shot to run", 30, |w| w.state_of(&sid).as_deref() == Some("started"));
    assert!(w.free().is_empty(), "a PC with our unfinished shot is busy");
    let (creds, job) = (w.creds.display().to_string(), job_id(&w, &sid).unwrap());
    w.a_set.cancel_job(&creds, &job);
    w.until("the shot to end", 30, |w| w.state_of(&sid).as_deref() == Some("cancelled"));
    assert_eq!(w.free(), [want], "a finished shot frees the PC");
}

/// An offline PC is not a candidate at all.
#[test]
fn automatic_never_picks_an_offline_pc() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-auto-offline", json!({})) else { return };
    free_b(&mut w);
    w.stop_b();
    w.until("A to notice B is gone", 30, |w| !w.a_set.online());
    assert!(w.free().is_empty() && w.a_set.automatic_candidates(&w.a_settings, None).is_empty());
}

/// Nothing calls its own endpoint and no claim path remains: a connection to this PC's own old service is not driven, and the
/// claim / offer / session routes and operations are gone from the code.
#[test]
fn no_self_http_and_no_claim_calls_remain() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for name in ["remote.rs", "remote_set.rs", "network_setup.rs", "inbound.rs", "netlink.rs", "ui/app.rs"] {
        let text = std::fs::read_to_string(src.join(name)).unwrap();
        for dead in ["CLAIM_EVERY", "Op::Claim", "Op::Register", "Op::Stats", "claim_ready", "claim_busy", "on_claim", "send_start", "may_claim", "\"/v1/claim\"", "\"/v1/session\""] {
            assert!(!text.contains(dead), "{name} still has {dead}");
        }
    }
    let root = repo();
    for rel in ["backend/coordinator/server.py", "backend/coordinator/client.py", "backend/coordinator/store.py", "backend/netlink.py"] {
        let text = std::fs::read_to_string(root.join(rel)).unwrap();
        for dead in ["/v1/claim", "/v1/session", "/v1/attempts", "def claim", "def _offer", "def op_claim", "def op_register", "def reject", "def op_stats"] {
            assert!(!text.contains(dead), "{rel} still has {dead}");
        }
    }
    // A saved connection to this PC's own old service (an old settings file) is never driven: no link, no request to our own endpoint.
    let mut s = Settings::default();
    s.network.connections = vec![Connection { credentials: "/nonexistent/own-credentials.json".into(), enabled: true, receive: true, local_host: true, ..Default::default() }];
    assert!(s.network.receives(), "an old local connection that received keeps receiving");
    let mut set = RemoteSet::new(Source::Off);
    let mut ai = AiUi::new(&s, None);
    let view = View::Unavailable(String::new());
    let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
    let mut env = Env { settings: &s, settings_path: None, ai: &mut ai, metrics: &view, repo: None, python: None, wake: &wake, cfg: Default::default() };
    set.tick(Instant::now(), &mut env);
    assert!(set.infos().is_empty(), "the old local connection is not driven");
    s.network.connections[0].receive = false;
    assert!(!s.network.receives(), "and its Receive choice carries over");
}

// ---------------------------------------------------------------------------------------- B3 fixes

/// Local work on B (a hold job on its own scheduler), as the Board would queue it.
fn local_hold(w: &mut World, id: &str) {
    let job = common::job(&w.base, id, 3, "hold");
    w.b_ai.scheduler_mut().submit(Submit { job, placement: Placement::Auto, after: vec![], min_vram_gb: None }).expect("local work queued");
}

/// Automatic skips a PC that is busy with its OWN work (not ours): free again when that work ends.
#[test]
fn automatic_skips_a_pc_busy_with_its_own_local_work() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-auto-local", json!({})) else { return };
    let want = free_b(&mut w);
    local_hold(&mut w, "local-1");
    w.until("B to say it is busy", 25, |w| w.free().is_empty());
    assert!(w.a_set.installs().iter().any(|i| i.id == w.host_iid && i.busy == Some(true) && i.receiving == Some(true)));
    w.b_ai.cancel("local-1");
    w.until("B to be free again", 40, |w| w.free() == [want.clone()]);
}

/// Receive jobs off: the PC is not offered to Automatic (and an explicit pick is still refused by the endpoint, not stored).
#[test]
fn automatic_skips_a_pc_with_receive_jobs_off() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-auto-off", json!({})) else { return };
    let want = free_b(&mut w);
    w.b_settings.network.receive_jobs = Some(false);
    w.until("B to say Receive jobs is off", 25, |w| w.free().is_empty());
    assert!(w.a_set.installs().iter().any(|i| i.id == w.host_iid && i.receiving == Some(false)));
    w.b_settings.network.receive_jobs = Some(true);
    w.until("B to be free again", 25, |w| w.free() == [want.clone()]);
}

/// A PC holding a shot it cannot settle (left running by a restart) takes no new work, so Automatic skips it until the user settles it.
#[test]
fn automatic_skips_a_pc_with_a_held_shot() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-auto-held", json!({})) else { return };
    let want = free_b(&mut w);
    w.b_in.shutdown_requested(); // nothing is started: the shot stays where a restart would find it
    let sid = w.push(1);
    w.until("the shot to be in B's inbox", 30, |w| job_id(w, &sid).is_some());
    let job = job_id(&w, &sid).unwrap();
    app_api::call("inbox", json!({"method": "begin", "job_id": job, "slot": "old:slot", "plan": {}})).expect("started in the old run");
    w.b_in = Inbound::new(); // the app restarted: the shot is uncertain
    w.until("B to say it holds a shot", 25, |w| w.free().is_empty() && w.a_set.installs().iter().any(|i| i.id == w.host_iid && i.held == Some(true)));
    let attempt = held_attempts(&w).pop().expect("one held attempt");
    w.b_in.release_held(&attempt); // the user checked and released it
    w.until("B to be free again", 40, |w| w.free() == [want.clone()]);
}

fn held_attempts(_w: &World) -> Vec<String> {
    app_api::call("inbox", json!({"method": "held"})).unwrap().as_array().unwrap().iter().map(|h| h["attempt_id"].as_str().unwrap().to_owned()).collect()
}

/// Clear/Forget on one held shot releases only that attempt (the action carries its attempt id; the handler in the app is that one call).
#[test]
fn clear_on_one_held_attempt_leaves_the_other_held() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b4-clear-one", json!({})) else { return };
    free_b(&mut w);
    w.b_in.shutdown_requested(); // nothing is started: the shots stay where a restart would find them
    let (s1, s2) = (w.push(1), w.push(2));
    w.until("both shots in B's inbox", 30, |w| job_id(w, &s1).is_some() && job_id(w, &s2).is_some());
    for (s, slot) in [(&s1, "old:slot1"), (&s2, "old:slot2")] {
        app_api::call("inbox", json!({"method": "begin", "job_id": job_id(&w, s).unwrap(), "slot": slot, "plan": {}})).expect("started in the old run");
    }
    w.b_in = Inbound::new(); // the app restarted: both are uncertain
    w.until("B to hold both", 25, |w| w.b_in.busy_reason().is_some() && held_attempts(w).len() == 2);
    let (a1, a2) = { let h = held_attempts(&w); (h[0].clone(), h[1].clone()) };
    w.b_in.release_held(&a1);
    w.until("the first attempt to be released", 25, |w| !held_attempts(w).contains(&a1));
    for _ in 0..20 {
        w.pump(true);
    }
    assert_eq!(held_attempts(&w), [a2], "the other attempt is still held");
    assert!(w.b_in.busy_reason().is_some(), "and still blocks new work");
}

/// A shot this PC refuses (its input is missing here) still lands on the Board of the project it maps to, with its clip.
#[test]
fn a_refused_received_shot_keeps_its_project_and_clip_on_the_board() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b4-refused-history", json!({})) else { return };
    let project = w.mount_b.join("projects/Show");
    std::fs::remove_dir_all(project.join("clips/shot_01/Frames")).unwrap(); // B has no frames: the preflight refuses it
    w.b_ai.queue_open(Some(&project));
    w.until("A to reach B", 40, |w| w.a_set.online());
    let sid = w.push(1);
    w.until("the shot to be refused", 60, |w| w.state_of(&sid).as_deref() == Some("failed"));
    let job = job_id(&w, &sid).unwrap();
    let id = format!("received:{job}");
    let e = w.history(&job).expect("history");
    assert_eq!((e.kind.as_str(), e.state.as_str(), e.folder.as_str(), e.owner.as_str()), ("received", "failed", "shot_01", project.to_str().unwrap()));
    let mut after = AiUi::new(&w.b_settings, None); // the Board loader of the project, after a restart
    after.queue_open(Some(&project));
    assert!(after.earlier_entries().iter().any(|e| e.id == id && e.state == "failed" && e.folder == "shot_01"), "the refusal is on the Board");
}

/// The race: an Automatic shot arrives after the PC stopped being free (the sender's list is a few seconds old). It is refused with
/// `not_free` and nothing is stored; a PC picked explicitly still queues the shot even when busy.
#[test]
fn an_automatic_push_to_a_pc_that_is_no_longer_free_is_refused_not_free() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-not-free", json!({})) else { return };
    free_b(&mut w);
    local_hold(&mut w, "local-1");
    w.until("B to say it is busy", 25, |w| w.a_set.installs().iter().any(|i| i.id == w.host_iid && i.busy == Some(true)));
    let auto = w.push_as(1, &w.host_iid.clone(), true); // sent although it is busy now (A's own list was stale a moment ago)
    w.until("B to refuse it", 30, |w| matches!(w.a_set.sent_outcome(&auto), Some(Err(Some(why))) if why.starts_with("not_free")));
    assert!(app_api::call("inbox", json!({"method": "pending"})).unwrap().as_array().unwrap().is_empty(), "a refused shot is not stored");
    assert!(job_id(&w, &auto).is_none());
    let explicit = w.push_as(2, &w.host_iid.clone(), false); // chosen by the user: queued behind the busy work
    w.until("B to queue the explicit pick", 30, |w| job_id(w, &explicit).is_some() && w.state_of(&explicit).as_deref() == Some("queued"));
    assert_eq!(app_api::call("inbox", json!({"method": "pending"})).unwrap().as_array().unwrap().len(), 1);
    let (creds, job) = (w.creds.display().to_string(), job_id(&w, &explicit).unwrap());
    w.a_set.cancel_job(&creds, &job);
    w.b_ai.cancel("local-1");
    w.until("the queued shot to end", 40, |w| w.state_of(&explicit).as_deref() == Some("cancelled"));
}

/// The receiver answers 400 invalid to the first submit: the sender's record ends as "not sent" with the receiver's reason (nothing
/// stays "sending"), the PC is free for the next send, and the same shot sent again once it is valid is accepted.
#[test]
fn a_400_invalid_first_send_ends_not_sent_with_the_reason_and_a_second_send_is_accepted() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b5-invalid", json!({})) else { return };
    free_b(&mut w);
    let mut bad = spec(&w.mount_a, 1);
    bad.preset_name = Some(String::new()); // the receiver's schema refuses an empty preset name
    let sid = "sub_00000000000000000001".to_owned();
    let net = NetSubmit { submission_id: sid.clone(), spec: bad, target: Some(format!("{}/{}", w.ns, w.host_iid)), folder: "shot_01".into(), name: "shot_01 v1".into(), automatic: false };
    let view = View::Unavailable(String::new());
    let mut env = Env { settings: &w.a_settings, settings_path: None, ai: &mut w.a_ai, metrics: &view, repo: None, python: None, wake: &w.wake, cfg: Default::default() };
    w.a_set.send(net, &mut env).expect("routed to B");
    w.until("B to refuse it", 30, |w| w.state_of(&sid).as_deref() == Some("not sent"));
    let reason = w.a_set.jobs().iter().find(|j| j.submission_id == sid).and_then(|j| j.reason.clone()).unwrap();
    assert!(reason.starts_with("invalid"), "{reason}");
    assert!(job_id(&w, &sid).is_none());
    assert!(app_api::call("inbox", json!({"method": "pending"})).unwrap().as_array().unwrap().is_empty(), "nothing stored on B");
    let again = w.push(2);
    w.until("B to accept the valid shot", 30, |w| job_id(w, &again).is_some());
    let (creds, job) = (w.creds.display().to_string(), job_id(&w, &again).unwrap());
    w.a_set.cancel_job(&creds, &job);
    w.until("the shot to end", 40, |w| w.state_of(&again).as_deref() == Some("cancelled"));
}

/// A shot another PC sent here, finished: the history entry has the receiver's own project and clip, so a restarted app shows it on
/// the Board when the project opens - and an open project's list is updated when the entry changes (no polling, no reopen).
#[test]
fn received_history_reaches_the_board_while_open_and_after_a_restart() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-history", json!({"key_delay": 0.3})) else { return };
    let project = w.mount_b.join("projects/Show");
    w.b_ai.queue_open(Some(&project)); // the project is open on B while the shot runs
    w.until("A to reach B", 40, |w| w.a_set.online());
    let sid = w.push(1);
    w.until("the shot to finish", 60, |w| w.state_of(&sid).as_deref() == Some("succeeded"));
    let job = job_id(&w, &sid).unwrap();
    let id = format!("received:{job}");
    w.until("the open Board to list it", 10, |w| w.b_ai.earlier_entries().iter().any(|e| e.id == id && e.state == "succeeded"));
    let e = w.history(&job).expect("history");
    assert_eq!((e.kind.as_str(), e.folder.as_str(), e.owner.as_str()), ("received", "shot_01", project.to_str().unwrap()), "the receiver's real project and clip");
    // The app restarts: a fresh Board opens the project and the received run is there.
    let mut after = AiUi::new(&w.b_settings, None);
    after.queue_open(Some(&project));
    let got = after.earlier_entries().iter().find(|e| e.id == id).expect("the received shot is on the Board after a restart");
    assert_eq!((got.kind.as_str(), got.state.as_str(), got.folder.as_str()), ("received", "succeeded", "shot_01"));
    let mut other = AiUi::new(&w.b_settings, None);
    other.queue_open(Some(&w.mount_b.join("projects/Other")));
    assert!(other.earlier_entries().iter().all(|e| e.id != id), "another project does not list it");
}

fn creds_id(creds: &Path) -> String {
    let v: Value = serde_json::from_slice(&std::fs::read(creds).unwrap()).unwrap();
    v["installation_id"].as_str().unwrap().to_owned()
}

impl World {
    fn b_state(&self, sid: &str) -> Option<String> {
        self.b_set.jobs().iter().find(|j| j.submission_id == sid).map(|j| j.state.clone())
    }
    fn b_sends(&mut self, n: NetSubmit) -> Result<(), String> {
        let mut env = Env { settings: &self.b_settings, settings_path: None, ai: &mut self.b_ai, metrics: &self.b_view, repo: None, python: None, wake: &self.wake, cfg: Default::default() };
        self.b_set.send(n, &mut env)
    }
}

/// A PC joined to B (A) tells B its endpoint over that connection when A opens (A's own app talks to B here), and B then has the PC as
/// a destination under the SAME PC id: it is in B's send list and in Automatic, and B can push a shot to it (which it runs).
/// One process has one endpoint, so "A's endpoint" is the in-process one; the sender-pc registration is the PC that publishes it.
#[test]
fn a_joined_pc_tells_its_host_its_endpoint_and_the_host_can_send_to_it_and_use_it_for_automatic() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-reverse", json!({})) else { return };
    w.b_side = true;
    let reg = creds_id(&w.creds);
    let public = format!("{}/{}", w.ns, reg);
    w.until("B to learn the joined PC's endpoint", 40, |w| w.b_in.destinations().iter().any(|d| d.pc_id == reg));
    let d = w.b_in.destinations().iter().find(|d| d.pc_id == reg).unwrap().clone();
    assert_eq!((d.legacy_ns.as_str(), d.host_id.as_str(), d.name.is_empty()), (w.ns.as_str(), w.host_iid.as_str(), false), "the PC id is its registration; its assignments keep the old namespace");
    assert!(std::fs::metadata(&d.credentials).is_ok(), "the login B pushes with is a private file");
    assert!(w.b_settings.network.connections.iter().any(|c| c.legacy_id == reg && c.credentials == d.credentials), "saved like a join");
    w.until("the destination to be online", 40, |w| w.b_set.online());
    w.until("the PC in B's send list under its old id", 20, |w| w.b_ai.net_targets.iter().any(|(id, _)| *id == public));
    let auto = format!("network:{public}");
    assert!(w.b_set.automatic_candidates(&w.b_settings, None).iter().any(|c| c.0 == auto), "offered to Automatic");
    w.until("the PC to be free for Automatic", 40, |w| w.b_set.free_candidates(&w.b_settings, None).contains(&auto));
    // B pushes a shot to it (an Automatic pick) and it is run exactly once.
    let sid = format!("sub_{:0>20}", 50);
    let net = NetSubmit { submission_id: sid.clone(), spec: spec(&w.mount_a, 50), target: Some(public.clone()), folder: "shot_01".into(), name: "shot_01 v50".into(), automatic: true };
    w.b_sends(net).expect("routed to the destination");
    w.until("the shot to finish", 60, |w| w.b_state(&sid).as_deref() == Some("succeeded"));
    let job = w.b_set.accepted(&sid).unwrap();
    assert_eq!(w.history(&job).expect("history").state, "succeeded");
    assert_eq!(ck::queue::read(&ck::queue::history_path().unwrap()).0.into_iter().filter(|e| e.kind == "received").count(), 1);
}

/// Another PC's app, as a separate process: it joins B (so it is a registered PC there), runs its own endpoint, publishes itself as free,
/// and tells B its endpoint on command (what its own app does on open).
const KAMINO: &str = r#"
import sys, json, threading, time
from pathlib import Path
sys.path.insert(0, sys.argv[1])
from backend import app_api, discovery
from backend.coordinator import pki
from backend.coordinator.client import Client, join_to_file
class Quiet:
    def __init__(self, *a): pass
    def start(self): return None
    def stop(self): pass
discovery.Responder = Quiet
sd, port, creds, host_port, host_pem = Path(sys.argv[2]), int(sys.argv[3]), sys.argv[4], sys.argv[5], sys.argv[6]
sd.mkdir(mode=0o700, parents=True)
pki.write_private_new(str(sd / "service.json"), json.dumps({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": True}))
ep = app_api.AppEndpoint(sd).start()
gib = 1 << 30
U = "GPU-0b1b2c3d-0000-4000-8000-0123456789cd"
def publish(seq):
    ep.publish(name="kamino-3", gpu={"uuid": U, "name": "Kamino GPU", "vram_total_mb": 24576, "ram_total_mb": 65536},
        stats={"seq": seq, "measured_age_ms": 0, "cpu_percent": 5.0, "ram_used_bytes": 8 * gib, "ram_total_bytes": 64 * gib,
               "gpus": [{"uuid": U, "index": 0, "name": "Kamino GPU", "utilization_percent": 5.0, "vram_used_bytes": gib, "vram_total_bytes": 24 * gib}]},
        avail={"receiving": True, "busy": False, "held": False})
publish(1)
join_to_file(f"https://127.0.0.1:{host_port}", creds, "kamino-3", pem=open(host_pem).read())
print("joined", flush=True)
def beat():
    n = 1
    while True:
        time.sleep(1)
        n += 1
        publish(n)
threading.Thread(target=beat, daemon=True).start()
for line in sys.stdin:
    cmd = line.strip()
    if cmd == "publish-endpoint":
        Client(pki.read_credentials(creds)).destination(ep.destination_offer(creds))
        print("told", flush=True)
    elif cmd == "pending":
        print(json.dumps([j["job_id"] for j in ep.store.inbox_pending(ep.live["iid"])]), flush=True)
ep.stop()
"#;

struct Kamino {
    child: std::process::Child,
    out: std::io::BufReader<std::process::ChildStdout>,
}

impl Kamino {
    fn ask(&mut self, cmd: &str) -> String {
        use std::io::{BufRead, Write};
        writeln!(self.child.stdin.as_mut().unwrap(), "{cmd}").unwrap();
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        line.trim().to_owned()
    }
}

impl Drop for Kamino {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn kamino(w: &World, creds: &Path) -> Kamino {
    let host_port = serde_json::from_slice::<Value>(&std::fs::read(w.sd.join("service.json")).unwrap()).unwrap()["port"].as_u64().unwrap();
    let mut child = std::process::Command::new(ck::import::python(&repo()))
        .arg("-c").arg(KAMINO).arg(repo()).arg(w.base.join("kamino-state")).arg(free_port().to_string()).arg(creds).arg(host_port.to_string()).arg(w.sd.join("server-cert.pem"))
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().expect("the other PC's app starts");
    let mut out = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    std::io::BufRead::read_line(&mut out, &mut line).unwrap();
    assert_eq!(line.trim(), "joined", "the other PC joined B");
    Kamino { child, out }
}

/// The real upgrade: B (the previous host) has old shots queued for and started on a registered PC, and that PC's app now runs its own
/// endpoint. When that PC tells B its endpoint (it holds a connection to B), B sends the queued shots to it with their submission id and
/// closes them; the started one is held as "was running on <PC> under the old version" and never re-run. Opening again sends nothing twice.
#[test]
fn a_previous_host_moves_old_shots_to_a_registered_pc_and_holds_the_started_one() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b3-move", json!({})) else { return };
    w.b_side = true;
    let kam_creds = w.base.join("kamino-on-b.json");
    let mut kam = kamino(&w, &kam_creds);
    let reg = creds_id(&kam_creds);
    let public = format!("{}/{}", w.ns, reg);
    // What the old database holds, made while that PC was not a destination yet: one shot started on it, one queued for it.
    w.until("A to reach B", 40, |w| w.a_set.online());
    let old_started = w.push_as(10, &reg, false);
    w.until("B to store the old started shot", 30, |w| job_id(w, &old_started).is_some());
    let started_job = job_id(&w, &old_started).unwrap();
    app_api::call("inbox", json!({"method": "begin", "host_id": reg, "job_id": started_job, "slot": "old:slot", "plan": {}})).expect("started by the old run");
    let old_queued = w.push_as(11, &reg, false);
    w.until("B to store the old queued shot", 30, |w| job_id(w, &old_queued).is_some());
    let queued_job = job_id(&w, &old_queued).unwrap();
    w.b_in = Inbound::new(); // the upgraded app opens on that database
    for _ in 0..20 {
        w.pump(true);
    }
    assert!(w.b_set.accepted(&old_queued).is_none(), "nothing can be sent before that PC's endpoint is known; the shot waits");
    // The PC's app opens and tells B its endpoint.
    assert_eq!(kam.ask("publish-endpoint"), "told");
    w.until("B to learn the PC's endpoint", 40, |w| w.b_in.destinations().iter().any(|d| d.pc_id == reg));
    assert_eq!(w.b_in.destinations().iter().find(|d| d.pc_id == reg).unwrap().legacy_ns, w.ns, "its old assignments keep resolving");
    w.until("the destination to be online", 40, |w| w.b_set.online());
    w.until("the PC in B's send list under its old id", 20, |w| w.b_ai.net_targets.iter().any(|(id, name)| *id == public && name.starts_with("kamino-3")));
    assert!(w.b_set.automatic_candidates(&w.b_settings, None).iter().any(|c| c.0 == format!("network:{public}")));
    // The queued shot reached that PC's own app, as a shot there; this copy is closed.
    w.until("the PC's app to accept the old queued shot", 40, |w| w.b_set.accepted(&old_queued).is_some());
    let moved = w.b_set.accepted(&old_queued).unwrap();
    assert!(kam.ask("pending").contains(&moved), "it is in that PC's inbox");
    w.until("the old row to be closed", 20, |_| {
        let rows = app_api::call("inbox", json!({"method": "foreign"})).unwrap();
        rows.as_array().unwrap().iter().all(|r| r["job_id"] != queued_job.as_str())
    });
    // The started one is held with that PC's name and is never sent anywhere.
    let held = app_api::call("inbox", json!({"method": "held"})).unwrap();
    let row = held.as_array().unwrap().iter().find(|h| h["job_id"] == started_job.as_str()).expect("held").clone();
    assert_eq!((row["foreign"].clone(), row["pc"].as_str()), (json!(true), Some("kamino-3")));
    // The app opens again: nothing is sent a second time; the held shot does not stop this PC taking its own work.
    w.b_in = Inbound::new();
    for _ in 0..40 {
        w.pump(true);
    }
    assert_eq!(w.b_set.jobs().iter().filter(|j| j.submission_id == old_queued).count(), 1, "one send for the old shot, however many times the app opens");
    assert_eq!(kam.ask("pending").matches("job_").count(), 1, "one shot in that PC's inbox");
    let own = w.push(12);
    w.until("this PC to run its own shot while the other PC's old shot stays held", 60, |w| w.state_of(&own).as_deref() == Some("succeeded"));
    assert!(w.b_in.busy_reason().is_some(), "the old shot is still held, shown for the user");
    // Clear: failed for good, never run.
    app_api::call("inbox", json!({"method": "abandon", "attempt_id": row["attempt_id"], "reason": "cleared"})).unwrap();
    assert!(app_api::call("inbox", json!({"method": "held"})).unwrap().as_array().unwrap().iter().all(|h| h["job_id"] != started_job.as_str()));
    assert_eq!(ck::queue::read(&ck::queue::history_path().unwrap()).0.into_iter().filter(|e| e.kind == "received").count(), 1, "only B's own shot ran here");
}

/// Fake `systemctl` and a temp `XDG_CONFIG_HOME`, shared by the tests of this file. The embedded interpreter copies the environment
/// when it starts, so this runs before the first endpoint of the process. `fail-disable` in the folder makes `systemctl disable` fail.
fn fake_systemd() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = std::env::temp_dir().join(format!("ck-b5-fake-systemd-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        let script = format!("#!/bin/sh\necho \"systemctl $*\" >> \"{d}/calls.log\"\n[ \"$2\" = disable ] && [ -e \"{d}/fail-disable\" ] && {{ echo 'unit is masked' >&2; exit 1; }}\nexit 0\n", d = dir.display());
        std::fs::write(dir.join("bin/systemctl"), script).unwrap();
        std::fs::set_permissions(dir.join("bin/systemctl"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::env::set_var("PATH", format!("{}:{}", dir.join("bin").display(), std::env::var("PATH").unwrap_or_default()));
        std::env::set_var("XDG_CONFIG_HOME", dir.join("xdg"));
        dir
    })
}

/// The takeover tests need their own PATH/XDG_CONFIG_HOME before the process-wide embedded interpreter starts, so each runs in its own
/// child process (this test binary re-run on that one test). Returns true in the child, where the real body runs; in the parent it
/// runs the child, asserts it succeeded and returns false. The parent's environment is never changed.
fn in_own_process(name: &str) -> bool {
    if std::env::var_os("KEYLOOM_TAKEOVER_CHILD").is_some() {
        return true;
    }
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--test-threads=1", "--nocapture"]).env("KEYLOOM_TAKEOVER_CHILD", "1").output().unwrap();
    print!("{}", String::from_utf8_lossy(&out.stdout));
    eprint!("{}", String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "{name} failed in its own process: {}", out.status);
    false
}

/// The stand-in old service of this install: its registry and install record (as the installer wrote them), the systemd unit, and a
/// process holding the state lock until a `stop.request` appears (the old service's own graceful stop). Returns (process, prefix).
fn old_service(w: &World) -> (std::process::Child, PathBuf) {
    let fake = fake_systemd();
    let prefix = w.base.join("prefix");
    let unit = fake.join("xdg/systemd/user/keyloom-network.service");
    std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
    std::fs::write(&unit, format!("# X-Keyloom-State-Dir={}\n[Service]\nExecStart=/x\n", w.sd.display())).unwrap();
    let script = "import sys, os, time, json; sys.path.insert(0, sys.argv[1]); sys.path.insert(0, sys.argv[1] + '/packaging')\n\
        import legacy_network_cleanup as cl; from backend.coordinator import pki\n\
        sd, prefix = sys.argv[2], sys.argv[3]; os.makedirs(prefix, exist_ok=True)\n\
        open(prefix + '/network-service.json', 'w').write(json.dumps({'version': 1, 'state_dir': sd}))\n\
        open(sd + '/service-install.json', 'w').write(json.dumps({'version': 1, 'owner': cl.owner_id(prefix), 'mode': 'systemd'}))\n\
        lock = pki.OwnerLock(sd + '/coordinator.lock', 'old service'); print('held', flush=True)\n\
        while not os.path.exists(sd + '/stop.request'): time.sleep(0.05)\n\
        lock.close()";
    let mut child = std::process::Command::new(ck::import::python(&repo())).arg("-c").arg(script).arg(repo()).arg(&w.sd).arg(&prefix)
        .stdout(std::process::Stdio::piped()).spawn().expect("the stand-in old service starts");
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(child.stdout.take().unwrap()), &mut line).unwrap();
    assert_eq!(line.trim(), "held", "the old service holds the state");
    (child, prefix)
}

/// A shot another PC pushed into this PC's inbox while the app's endpoint was closed. Returns its job id.
fn queue_old_row(w: &World) -> String {
    let spec_file = w.base.join("spec.json");
    std::fs::write(&spec_file, serde_json::to_string(&spec(&w.mount_a, 1)).unwrap()).unwrap();
    let script = "import sys, json; sys.path.insert(0, sys.argv[1]); from backend.coordinator.store import Store\n\
        st = Store(sys.argv[2] + '/coordinator.sqlite3'); iid, tok = st.join('sender-pc'); inst = st.authenticate(f'{iid}.{tok}')\n\
        r = st.submit(inst, {'submission_id': 'sub_' + '1'.zfill(20), 'spec': json.load(open(sys.argv[3])), 'target': None}); print(r['job_id'], flush=True)";
    let out = std::process::Command::new(ck::import::python(&repo())).arg("-c").arg(script).arg(repo()).arg(&w.sd).arg(&spec_file).output().unwrap();
    let job = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    assert!(job.starts_with("job_"), "the shot was queued: {job:?} {}", String::from_utf8_lossy(&out.stderr));
    job
}

/// The old background service of this install still holds the state: the app retires it itself (stop, remove its unit), takes over,
/// and runs a shot pushed AFTER the takeover.
#[test]
fn the_app_retires_the_old_service_and_runs_a_shot_pushed_after_the_takeover() {
    if !in_own_process("the_app_retires_the_old_service_and_runs_a_shot_pushed_after_the_takeover") {
        return;
    }
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    fake_systemd();
    let Some(mut w) = world("b5-retire", json!({})) else { return };
    w.stop_b();
    let (mut old, prefix) = old_service(&w);
    let api = AppApi::start_in(w.paths.clone(), w.sd.clone(), Some(prefix.clone()), Arc::new(|| {}));
    assert!(matches!(wait_status(&api, |s| !matches!(s, Status::Starting)), Status::Running(_)), "{:?}", api.status());
    w.api = Some(api);
    old.wait().unwrap();
    assert!(!prefix.join("network-service.json").exists() && !fake_systemd().join("xdg/systemd/user/keyloom-network.service").exists(), "the old service is unregistered");
    assert!(std::fs::read_to_string(fake_systemd().join("calls.log")).unwrap().contains("systemctl --user disable"));
    w.until("A to reach the app", 40, |w| w.a_set.online());
    let sid = w.push(2);
    w.until("the shot pushed after the takeover to be accepted", 30, |w| job_id(w, &sid).is_some());
    let job = job_id(&w, &sid).unwrap();
    w.until("the shot to run and succeed", 60, |w| w.history(&job).is_some_and(|e| e.state == "succeeded"));
}

/// The old service cannot be removed: the app says why, stays Passive and dispatches nothing, even a shot already in the inbox. Retry
/// (once the cause is gone) takes over and the queued shot then runs.
#[test]
fn when_the_old_service_cannot_be_retired_the_banner_says_why_and_nothing_is_dispatched() {
    if !in_own_process("when_the_old_service_cannot_be_retired_the_banner_says_why_and_nothing_is_dispatched") {
        return;
    }
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let fail = fake_systemd().join("fail-disable");
    std::fs::write(&fail, "").unwrap();
    let Some(mut w) = world("b5-noretire", json!({})) else { return };
    w.stop_b();
    let job = queue_old_row(&w);
    let (mut old, prefix) = old_service(&w);
    let api = AppApi::start_in(w.paths.clone(), w.sd.clone(), Some(prefix), Arc::new(|| {}));
    let Status::Passive { message, .. } = wait_status(&api, |s| !matches!(s, Status::Starting)) else { panic!("not passive: {:?}", api.status()) };
    assert!(message.contains("systemctl --user disable failed") && message.contains("unit is masked"), "{message}");
    w.api = Some(api);
    old.wait().unwrap();
    for _ in 0..100 {
        w.pump(false);
    }
    assert!(w.history(&job).is_none() && !w.b_ai.scheduler().running(), "nothing runs while the network is not active");
    std::fs::remove_file(&fail).unwrap();
    assert!(w.api.as_ref().unwrap().retry());
    assert!(matches!(wait_status(w.api.as_ref().unwrap(), |s| !matches!(s, Status::Starting)), Status::Running(_)));
    w.until("the queued shot to run after Retry", 60, |w| w.history(&job).is_some_and(|e| e.state == "succeeded"));
}

/// Receive jobs off: a shot already in the inbox is not dispatched; turning it on lets it run.
#[test]
fn receive_jobs_off_does_not_dispatch_a_queued_inbox_row() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("b5-receive-off", json!({})) else { return };
    w.stop_b();
    let job = queue_old_row(&w);
    w.start_b();
    w.b_settings.network.receive_jobs = Some(false);
    for _ in 0..100 {
        w.pump(false);
    }
    assert!(w.history(&job).is_none() && !w.b_ai.scheduler().running() && w.b_in.receiving().is_none(), "the queued shot did not start");
    let pending = app_api::call("inbox", json!({"method": "pending"})).unwrap();
    assert!(pending.as_array().unwrap().iter().any(|p| p["job_id"] == job.as_str()), "it is still queued");
    w.b_settings.network.receive_jobs = Some(true);
    w.until("the shot to run once receiving is on", 60, |w| w.history(&job).is_some_and(|e| e.state == "succeeded"));
}

#[test]
fn graceful_shutdown_settles_received_job_before_stopping_endpoint() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(mut w) = world("a7-close-settle", json!({"modes": {"key": "hold"}})) else { return };
    w.until("A to reach B", 40, |w| w.a_set.online());
    let sid = w.push(1);
    w.until("the shot to run on B", 40, |w| w.b_in.receiving().is_some() && w.state_of(&sid).as_deref() == Some("started"));
    let job = job_id(&w, &sid).unwrap();

    w.b_in.shutdown_requested();
    w.b_ai.begin_stop();
    w.until("the received result and receive-disable to settle", 60, |w| {
        w.b_in.shutdown_settled() && w.state_of(&sid).is_some_and(|s| s == "cancelled" || s == "failed")
    });

    assert!(app_api::running(), "the inbox remains available until its cancellation is recorded");
    assert!(w.b_in.receiving().is_none());
    assert_eq!(w.history(&job).expect("durable receive history").state, "cancelled");
    assert!(!w.b_ai.scheduler().running());
    w.stop_b();
}
