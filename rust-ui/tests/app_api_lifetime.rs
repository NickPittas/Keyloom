//! A3: the endpoint lives in the app PID. Loads the INSTALLED runtime's libpython in this test process (read-only), imports
//! ssl/sqlite3, starts the listener, takes an authenticated push into the inbox, and stops it again. Run only through
//! `rust-ui/tools/isolated-test.sh`; state dirs are temporary and the ports are picked at random.
use ck::app_api::{self, AppApi, Event, Paths, Status};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The interpreter these tests embed: found exactly the way the app finds it (installed runtime, else `CK_PYTHON` / the repo `.venv`).
const NO_RUNTIME: &str = "SKIPPED: no Keyloom runtime was found (no install, no CK_PYTHON, no repo .venv)";

/// One interpreter per process and one endpoint per interpreter: tests take turns.
static TURN: Mutex<()> = Mutex::new(());

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

fn paths() -> Option<Paths> {
    let p = ck::runtime::embed_paths(&ck::runtime::launch(Some(&repo()))).ok();
    if p.is_none() {
        eprintln!("{NO_RUNTIME}");
    }
    p
}

/// A Python that has the repo's dependencies (sender / lock-holder helper processes).
fn venv_python() -> PathBuf {
    ck::import::python(&repo())
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A state dir the way the old service left it: fixed port, loopback, Join allowed.
fn state_dir(port: u16) -> tempfile_dir::Dir {
    let d = tempfile_dir::Dir::new();
    let sd = d.path().join("server");
    std::fs::create_dir_all(&sd).unwrap();
    std::fs::write(sd.join("service.json"), json!({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": true}).to_string()).unwrap();
    d
}

mod tempfile_dir {
    use std::path::{Path, PathBuf};
    pub struct Dir(PathBuf);
    impl Dir {
        pub fn new() -> Self {
            let p = std::env::temp_dir().join(format!("ck-a3-{}-{}", std::process::id(), super::free_port()));
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
        pub fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
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

/// Listening sockets of this process on `port`: the socket inode appears in /proc/net/tcp{,6} and in our own fd table.
#[cfg(target_os = "linux")]
fn listener_is_ours(port: u16) -> bool {
    let want = format!(":{port:04X} ");
    let inodes: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .flat_map(|t| t.lines().skip(1).map(str::to_owned).collect::<Vec<_>>())
        .filter(|l| l.contains(&want) && l.split_whitespace().nth(3) == Some("0A"))
        .filter_map(|l| l.split_whitespace().nth(9).map(str::to_owned))
        .collect();
    std::fs::read_dir("/proc/self/fd").unwrap().filter_map(Result::ok).filter_map(|e| std::fs::read_link(e.path()).ok()).any(|l| inodes.iter().any(|i| l.to_string_lossy() == format!("socket:[{i}]")))
}

#[cfg(windows)]
fn listener_is_ours(port: u16) -> bool {
    // netstat on Kamino-3 omits the dual-stack `::` listener; Get-NetTCPConnection shows it (State is blank there,
    // so no State filter). A closed port is an empty answer, not an error.
    let me = std::process::id();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("Get-NetTCPConnection -LocalPort {port} -ErrorAction SilentlyContinue | Where-Object OwningProcess -eq {me} | Where-Object {{ -not $_.RemoteAddress -or $_.RemoteAddress -in '0.0.0.0','::' }} | Select-Object -ExpandProperty OwningProcess"),
        ])
        .output()
        .expect("query listening port owner");
    String::from_utf8_lossy(&out.stdout).lines().any(|l| l.trim() == me.to_string())
}

#[cfg(target_os = "linux")]
fn child_processes() -> Vec<String> {
    let mut out = vec![];
    for t in std::fs::read_dir("/proc/self/task").unwrap().filter_map(Result::ok) {
        if let Ok(c) = std::fs::read_to_string(t.path().join("children")) {
            out.extend(c.split_whitespace().map(str::to_owned));
        }
    }
    out
}

#[cfg(windows)]
fn child_processes() -> Vec<String> {
    let output = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!(
                "Get-CimInstance Win32_Process -Filter 'ParentProcessId={}' | Where-Object ProcessId -ne $PID | Select-Object -ExpandProperty ProcessId",
                std::process::id()
            ),
        ])
        .output()
        .expect("query child processes");
    assert!(output.status.success(), "Get-CimInstance failed");
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

fn reaches(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).is_ok()
}

/// FIRST GATE: the installed runtime's libpython, loaded in this process, imports ssl and sqlite3, starts and stops a listener,
/// and a thread standing in for the UI keeps ticking the whole time.
#[test]
fn gate_installed_libpython_in_process_listener_and_responsive_ui() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(paths) = paths() else { return };
    let beat = Arc::new(AtomicU64::new(0));
    let (stop_ui, b2) = (Arc::new(std::sync::atomic::AtomicBool::new(false)), beat.clone());
    let s2 = stop_ui.clone();
    let worst = Arc::new(Mutex::new(Duration::ZERO));
    let w2 = worst.clone();
    let ui = std::thread::spawn(move || {
        let mut last = Instant::now();
        while !s2.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(2));
            let now = Instant::now();
            let gap = now - last;
            last = now;
            let mut w = w2.lock().unwrap();
            *w = (*w).max(gap);
            b2.fetch_add(1, Ordering::Relaxed);
        }
    });

    let port = free_port();
    let dir = state_dir(port);
    let sd = dir.path().join("server");
    let woke = Arc::new(AtomicU64::new(0));
    let w3 = woke.clone();
    let api = AppApi::start(paths.clone(), sd.clone(), Arc::new(move || {
        w3.fetch_add(1, Ordering::Relaxed);
    }));
    assert_eq!(api.status(), Status::Starting, "start returns at once; the listener comes up on a background thread");
    let st = wait_status(&api, |s| !matches!(s, Status::Starting));
    let Status::Running(info) = st else { panic!("endpoint did not start: {st:?}") };
    assert_eq!(info["port"], port);
    assert_eq!(info["pid"], std::process::id(), "the listener belongs to this very process");
    assert!(woke.load(Ordering::Relaxed) >= 1, "the driver is woken when the endpoint settles");

    // The two modules the endpoint needs, from the bundled runtime, through the real call path.
    let imp = app_api::call("check_imports", json!({"modules": ["ssl", "sqlite3"]})).expect("import ssl and sqlite3");
    assert_eq!(imp, json!({"ssl": true, "sqlite3": true}));
    assert!(listener_is_ours(port), "the listening socket is in this process's fd table");
    assert!(reaches(port));
    assert!(child_processes().is_empty(), "no service, no netlink child: {:?}", child_processes());

    assert!(api.stop(Duration::from_secs(20)), "stop finishes in time");
    assert!(!reaches(port), "Quit closes the socket");
    assert!(!listener_is_ours(port));
    assert_eq!(api.status(), Status::Stopped);
    assert!(api.stop(Duration::from_secs(1)), "stop is idempotent");

    stop_ui.store(true, Ordering::Relaxed);
    ui.join().unwrap();
    let worst = *worst.lock().unwrap();
    assert!(beat.load(Ordering::Relaxed) > 20, "the UI thread ran throughout");
    assert!(worst < Duration::from_millis(400), "the UI thread was never held up: worst gap {worst:?}");
}

const CLIENT: &str = r#"
import sys, json
sys.path.insert(0, sys.argv[1])
from backend.coordinator import pki
from backend.coordinator.client import Client, join_to_file
port, state, creds = sys.argv[2], sys.argv[3], sys.argv[4]
join_to_file(f"https://127.0.0.1:{port}", creds, "sender", pem=open(state + "/server-cert.pem").read())
c = Client(pki.read_credentials(creds))
from tests.coordinator.specs import SPEC as spec
a = c.submit("a" * 32, spec)
b = c.submit("a" * 32, spec)
print(json.dumps({"job": a["job_id"], "again": b["job_id"], "state": c.job(a["job_id"])["state"]}))
"#;

#[test]
fn authenticated_push_reaches_the_inbox_and_wakes_the_driver() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(paths) = paths() else { return };
    let port = free_port();
    let dir = state_dir(port);
    let sd = dir.path().join("server");
    let woke = Arc::new(AtomicU64::new(0));
    let w3 = woke.clone();
    let api = AppApi::start(paths.clone(), sd.clone(), Arc::new(move || {
        w3.fetch_add(1, Ordering::Relaxed);
    }));
    assert!(matches!(wait_status(&api, |s| !matches!(s, Status::Starting)), Status::Running(_)));
    let before = woke.load(Ordering::Relaxed);

    // A sender PC: a separate process, joined with the pinned certificate, pushing one shot twice (the same submission id).
    let out = std::process::Command::new(venv_python())
        .arg("-c").arg(CLIENT).arg(repo()).arg(port.to_string()).arg(&sd).arg(dir.path().join("creds.json"))
        .output().expect("sender runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let got: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let job = got["job"].as_str().unwrap().to_owned();
    assert_eq!(got["again"], got["job"], "a repeated submission is the same job");
    assert_eq!(got["state"], "queued");

    // The commit woke the app driver and is waiting as an event; the repeat added nothing.
    let end = Instant::now() + Duration::from_secs(5);
    while woke.load(Ordering::Relaxed) == before && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(woke.load(Ordering::Relaxed) > before);
    assert_eq!(api.take_events(), vec![Event::Commit { kind: "submitted".into(), job_id: job.clone() }]);

    let pending = app_api::call("inbox", json!({"method": "pending"})).expect("inbox");
    assert_eq!(pending.as_array().unwrap().iter().map(|j| j["job_id"].as_str().unwrap()).collect::<Vec<_>>(), [job.as_str()]);
    let regs = app_api::call("registered", json!({})).unwrap();
    assert_eq!(regs.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect::<Vec<_>>(), ["sender"]);
    assert!(child_processes().is_empty(), "{:?}", child_processes());

    // Quit: the socket closes, the same state reopens with the shot still queued.
    assert!(api.stop(Duration::from_secs(20)));
    assert!(!reaches(port));
    let again = AppApi::start(paths.clone(), sd, Arc::new(|| {}));
    assert!(matches!(wait_status(&again, |s| !matches!(s, Status::Starting)), Status::Running(_)));
    let pending = app_api::call("inbox", json!({"method": "pending"})).unwrap();
    assert_eq!(pending[0]["job_id"], job.as_str());
    assert!(again.stop(Duration::from_secs(20)));
}

/// The old network service (or anything else) holding this PC's state: the app reports it and stays passive.
#[test]
fn old_service_holding_the_state_makes_the_app_passive() {
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(paths) = paths() else { return };
    let port = free_port();
    let dir = state_dir(port);
    let sd = dir.path().join("server");
    let mut holder = std::process::Command::new(venv_python())
        .args(["-c", "import sys; sys.path.insert(0, sys.argv[1]); from backend.coordinator import pki; l = pki.OwnerLock(sys.argv[2] + '/coordinator.lock', 'old service'); print('held', flush=True); sys.stdin.read()"])
        .arg(repo()).arg(&sd)
        .stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(holder.stdout.take().unwrap()), &mut line).unwrap();
    assert_eq!(line.trim(), "held");
    let api = AppApi::start(paths.clone(), sd.clone(), Arc::new(|| {}));
    let st = wait_status(&api, |s| !matches!(s, Status::Starting));
    assert!(matches!(&st, Status::Passive { code, .. } if code == "service_conflict"), "{st:?}");
    assert!(!reaches(port), "a passive app opens no listener");
    drop(holder.stdin.take());
    holder.wait().unwrap();
    assert!(api.stop(Duration::from_secs(20)));
}

/// The in-process bridge link speaks the netlink protocol: the same request/reply shape and error codes, no child process.
#[test]
fn embedded_link_answers_like_the_process_link() {
    use ck::netlink::{Link, Op, State};
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(paths) = paths() else { return };
    let mut link = Link::start_embedded(paths, Arc::new(|| {})).expect("link");
    let end = Instant::now() + Duration::from_secs(60);
    while link.state() == State::Starting && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(link.state(), State::Ready);
    assert_eq!(link.pid(), std::process::id());
    let id = link.request(Op::Job, json!({"job_id": "job_x"})).unwrap();
    let mut got = vec![];
    while got.is_empty() && Instant::now() < end {
        got = link.poll();
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].id.as_deref(), Some(id.as_str()));
    assert_eq!(got[0].outcome.as_ref().unwrap_err().code, "not_connected");
    assert!(child_processes().is_empty());
    link.close();
    assert!(matches!(link.state(), State::Ended(_)));
}

/// Endpoint on + a resolvable runtime gives the in-process link and starts no child; endpoint off keeps the child-process source.
#[test]
fn endpoint_on_opens_an_embedded_link_and_no_child() {
    use ck::netlink::{Source, State};
    let _t = TURN.lock().unwrap_or_else(|e| e.into_inner());
    let Some(p) = paths() else { return };
    assert_eq!(Source::for_app(false, Some(&repo())), Source::Local);
    let src = Source::for_app(true, Some(&repo()));
    assert_eq!(src, Source::Embedded(p));
    let mut link = src.open(Some(&repo()), None, Arc::new(|| {})).unwrap().expect("a link");
    let end = Instant::now() + Duration::from_secs(60);
    while link.state() == State::Starting && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(link.state(), State::Ready);
    assert_eq!(link.pid(), std::process::id());
    assert!(child_processes().is_empty());
    link.close();
}
