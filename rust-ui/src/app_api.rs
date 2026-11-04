//! The app's network endpoint, running INSIDE the app process.
//!
//! The pinned runtime's Python shared library is loaded at run time (`dlopen` / `LoadLibrary`, nothing linked at build
//! time, so a missing runtime never stops the app or any test binary from starting) and `backend.app_api.AppEndpoint`
//! (HTTPS listener, discovery, durable inbox) runs on Python's own threads in this PID. No service, no child process.
//!
//! * Contract (ARCH-PLAN section 8): library = `<home>/lib/libpython3.N.so.1.0` / `<home>\python3NN.dll`; `PYTHONHOME` = `<home>`;
//!   import roots = the app folder only; isolated like `-I` (no environment, no user site, no cwd on `sys.path`).
//! * The interpreter lock is released while Rust is not inside a call (`PyEval_SaveThread` after start) and by Python
//!   itself while its threads wait on sockets or the database. Every call from Rust takes it only for that call, so
//!   [`call`] and everything built on it must run on a background thread, never on the UI thread or under a UI/scheduler lock.
//! * Commit events (a pushed shot, a cancel) reach Rust through a native callback and wake the app driver; nothing polls.
//! * The interpreter is never finalized: stopping the endpoint closes the socket, the process exit ends the rest.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void, CString};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

type Obj = *mut c_void;

/// The installer looks for this exact text inside the UI binary: only a binary that starts the endpoint in-process BY DEFAULT lets it
/// retire the old background service (packaging `bundle.ENDPOINT_MARKER`). `AppApi::start` reads it, so the linker keeps it.
#[used]
pub static ENDPOINT_MARKER: [u8; 31] = *b"KEYLOOM_APP_ENDPOINT_DEFAULT_V1";

/// This PC's network state folder for a settings file (the same folder the old network service used, so identity, joins and inbox carry over).
pub fn state_dir(settings_path: &Path) -> PathBuf {
    settings_path.parent().map_or_else(|| PathBuf::from("network/server"), |d| d.join("network").join("server"))
}

/// Where the embedded interpreter comes from (see `runtime::embed_paths`).
#[derive(Clone, Debug, PartialEq)]
pub struct Paths {
    pub library: PathBuf,
    pub home: PathBuf,
    pub roots: Vec<PathBuf>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallError {
    pub code: String,
    pub message: String,
}

impl CallError {
    fn new(code: &str, message: impl Into<String>) -> Self {
        Self { code: code.into(), message: message.into() }
    }
}

#[repr(C)]
struct MethodDef {
    name: *const c_char,
    meth: unsafe extern "C" fn(Obj, Obj) -> Obj,
    flags: c_int,
    doc: *const c_char,
}

struct Api {
    _lib: libloading::Library,
    ensure: unsafe extern "C" fn() -> c_int,
    release: unsafe extern "C" fn(c_int),
    incref: unsafe extern "C" fn(Obj),
    decref: unsafe extern "C" fn(Obj),
    call_one: unsafe extern "C" fn(Obj, Obj) -> Obj,
    from_utf8: unsafe extern "C" fn(*const c_char, isize) -> Obj,
    as_utf8: unsafe extern "C" fn(Obj, *mut isize) -> *const c_char,
    to_str: unsafe extern "C" fn(Obj) -> Obj,
    err_fetch: unsafe extern "C" fn(*mut Obj, *mut Obj, *mut Obj),
    err_clear: unsafe extern "C" fn(),
    none: Obj,
    shim_op: Obj,
}

// The raw pointers are an immortal function table and Python objects only touched while holding the interpreter lock.
unsafe impl Send for Api {}
unsafe impl Sync for Api {}

static API: OnceLock<Result<Api, String>> = OnceLock::new();

/// True while this process's endpoint answers (set when it comes up, cleared the moment a stop begins).
static RUNNING: AtomicBool = AtomicBool::new(false);

pub fn running() -> bool {
    RUNNING.load(Ordering::SeqCst)
}

/// The Python half: one dispatcher over a fixed list of operations. Returns/raises nothing: every answer is JSON text.
const SHIM: &str = r#"
import json, os, sys, threading
for _r in json.loads(ROOTS):
    if _r not in sys.path:
        sys.path.insert(0, _r)
_ep = None
_bridges = {}
_nb = [0]
_lock = threading.Lock()
_INBOX = {"pending", "held", "begin", "report", "cancel_requested", "abandon", "foreign", "forwarded"}

class _E(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message

def _commit(kind, job_id):
    _emit(json.dumps([kind, job_id]))

def _need():
    if _ep is None:
        raise _E("not_running", "the app endpoint is not running")
    return _ep

def _start(a):
    global _ep
    with _lock:
        if _ep is not None:
            raise _E("already_running", "the app endpoint is already running")
        from backend import app_api
        ep = app_api.AppEndpoint(a["state_dir"], on_commit=_commit).start()
        _ep = ep
    return _info()

def _info():
    ep = _need()
    return {**ep.info(), "port": ep.port, "pid": os.getpid(), "discovery_error": ep.discovery_error}

def _stop(a):
    global _ep
    with _lock:
        ep, _ep = _ep, None
    if ep is not None:
        ep.stop()
    return {}

def _inbox(a):
    ep = _need()
    m = a.get("method")
    if m not in _INBOX:
        raise _E("bad_request", "unknown inbox method")
    kw = {k: v for k, v in a.items() if k != "method"}
    if m in ("pending", "held", "begin", "report", "foreign") and "host_id" not in kw:
        kw["host_id"] = ep.live["iid"]
    if m == "abandon":  # the user settles a shot left uncertain by a restart
        return ep.store.abandon(**kw)
    return getattr(ep.store, "inbox_" + m)(**kw)

def _estimate(a):
    from backend import netlink
    return netlink.Bridge().op_estimate(a)

def _bridge_open(a):
    from backend import netlink
    with _lock:
        _nb[0] += 1
        _bridges[_nb[0]] = netlink.Bridge(a.get("timeout", 15))
        return {"id": _nb[0]}

def _bridge_line(a):
    b = _bridges.get(a["id"])
    if b is None:
        raise _E("link_ended", "the in-app network link is closed")
    resp, stop = b.handle(a["line"].encode("utf-8"))
    try:
        text = json.dumps(resp, separators=(",", ":"), ensure_ascii=False)
    except UnicodeEncodeError:
        text = json.dumps({"id": resp.get("id"), "ok": False, "error": {"code": "internal", "message": "invalid Unicode in the reply"}})
    return {"line": text, "stop": stop}

def _bridge_close(a):
    with _lock:
        b = _bridges.pop(a["id"], None)
    if b is not None:
        b.release()
    return {}

_OPS = {
    "ping": lambda a: {"pid": os.getpid()},
    "check_imports": lambda a: {m: bool(__import__(m)) for m in a["modules"]},
    "endpoint_start": _start, "endpoint_stop": _stop, "endpoint_info": lambda a: _info(),
    "endpoint_refresh": lambda a: (_need().refresh(), {})[1],
    "registered": lambda a: _need().registered(),
    "retire": lambda a: _need().retire(a["installation_id"]),
    "inbox": _inbox,
    "publish": lambda a: (_need().publish(a.get("name"), a.get("gpu"), a.get("stats"), a.get("avail")), {})[1],
    "destinations": lambda a: _need().destinations(),
    "destination_offer": lambda a: _need().destination_offer(a["credentials_path"]),
    "destination_remove": lambda a: (_need().remove_destination(a["pc_id"]), {})[1],
    "configure": lambda a: (_need().configure(a.get("allow_join"), a.get("name"), a.get("receive")), {})[1],
    "estimate": _estimate,
    "bridge_open": _bridge_open, "bridge_line": _bridge_line, "bridge_close": _bridge_close,
}

def op(text):
    try:
        m = json.loads(text)
        return json.dumps({"ok": True, "result": _OPS[m["op"]](m.get("args") or {})}, default=str)
    except BaseException as e:
        return json.dumps({"ok": False, "code": str(getattr(e, "code", None) or "internal"),
                           "message": str(getattr(e, "message", None) or e or type(e).__name__)[:500]})
"#;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Event {
    /// A shot was committed to the inbox (`kind` "submit") or a cancel was recorded ("cancel").
    Commit { kind: String, job_id: String },
}

#[derive(Default)]
struct Sink {
    events: Mutex<VecDeque<Event>>,
    wake: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

fn sink() -> &'static Sink {
    static S: OnceLock<Sink> = OnceLock::new();
    S.get_or_init(Sink::default)
}

/// Events that arrived since the last call, whoever started the endpoint (the app driver drains them; the wake already ran).
pub fn take_events() -> Vec<Event> {
    sink().events.lock().unwrap().drain(..).collect()
}

/// Called by Python (interpreter lock held) for every committed event. Quick: queue + wake, nothing else.
unsafe extern "C" fn emit(_: Obj, arg: Obj) -> Obj {
    let Some(Ok(api)) = API.get() else { return std::ptr::null_mut() };
    let mut n: isize = 0;
    let p = (api.as_utf8)(arg, &mut n);
    if !p.is_null() {
        let bytes = std::slice::from_raw_parts(p as *const u8, n as usize);
        if let Ok(v) = serde_json::from_slice::<Vec<String>>(bytes) {
            if let [kind, job_id] = &v[..] {
                let s = sink();
                s.events.lock().unwrap().push_back(Event::Commit { kind: kind.clone(), job_id: job_id.clone() });
                let w = s.wake.lock().unwrap().clone();
                if let Some(w) = w {
                    w();
                }
            }
        }
    }
    (api.incref)(api.none);
    api.none
}

#[cfg(unix)]
fn open_library(p: &Path) -> Result<libloading::Library, String> {
    use libloading::os::unix::{Library, RTLD_GLOBAL, RTLD_NOW};
    // Global so the extension modules (_ssl, _sqlite3, ...) resolve their Python symbols from it.
    unsafe { Library::open(Some(p), RTLD_NOW | RTLD_GLOBAL) }.map(Into::into).map_err(|e| e.to_string())
}

#[cfg(windows)]
fn open_library(p: &Path) -> Result<libloading::Library, String> {
    use libloading::os::windows::{Library, LOAD_WITH_ALTERED_SEARCH_PATH};
    unsafe { Library::load_with_flags(p, LOAD_WITH_ALTERED_SEARCH_PATH) }.map(Into::into).map_err(|e| e.to_string())
}

#[cfg(unix)]
fn wide(p: &Path) -> Vec<u32> {
    use std::os::unix::ffi::OsStrExt;
    let mut v: Vec<u32> = String::from_utf8_lossy(p.as_os_str().as_bytes()).chars().map(|c| c as u32).collect();
    v.push(0);
    v
}

#[cfg(windows)]
fn wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    let mut v: Vec<u16> = p.as_os_str().encode_wide().collect();
    v.push(0);
    v
}

/// Refuse a damaged runtime BEFORE initializing: with part of the standard library missing, `Py_InitializeEx` aborts the whole
/// process with no message. (The status-returning `PyConfig` path needs version-specific struct offsets; this check is the cheap stand-in.)
pub fn check_runtime(paths: &Paths) -> Result<(), String> {
    if !paths.library.is_file() {
        return Err(format!("the Python runtime library was not found: {}", paths.library.display()));
    }
    if !paths.home.is_dir() {
        return Err(format!("the Python runtime folder was not found: {}", paths.home.display()));
    }
    let libs: Vec<PathBuf> = if cfg!(windows) {
        vec![paths.home.join("Lib")]
    } else {
        std::fs::read_dir(paths.home.join("lib")).into_iter().flatten().filter_map(Result::ok).map(|e| e.path()).filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("python3.")) && p.is_dir()).collect()
    };
    if !libs.iter().any(|l| ["os.py", "site.py", "encodings/__init__.py"].iter().all(|f| l.join(f).is_file())) {
        return Err(format!("the Python runtime is incomplete (its standard library is damaged or missing): {}", paths.home.display()));
    }
    Ok(())
}

fn load(paths: &Paths) -> Result<Api, String> {
    check_runtime(paths)?;
    let lib = open_library(&paths.library).map_err(|e| format!("cannot load {}: {e}", paths.library.display()))?;
    unsafe {
        macro_rules! sym {
            ($n:literal, $t:ty) => {
                *lib.get::<$t>($n).map_err(|e| format!("{} lacks {}: {e}", paths.library.display(), String::from_utf8_lossy(&$n[..$n.len() - 1])))?
            };
        }
        // Isolated, like `-I` (the isolated flag also drops the user site): nothing from the host environment, the user site or the working folder reaches `sys.path`.
        for flag in [&b"Py_IgnoreEnvironmentFlag\0"[..], b"Py_IsolatedFlag\0"] {
            let f = *lib.get::<*mut c_int>(flag).map_err(|e| e.to_string())?;
            *f = 1;
        }
        let set_home: unsafe extern "C" fn(*const c_void) = sym!(b"Py_SetPythonHome\0", unsafe extern "C" fn(*const c_void));
        let set_name: unsafe extern "C" fn(*const c_void) = sym!(b"Py_SetProgramName\0", unsafe extern "C" fn(*const c_void));
        let init: unsafe extern "C" fn(c_int) = sym!(b"Py_InitializeEx\0", unsafe extern "C" fn(c_int));
        let is_init: unsafe extern "C" fn() -> c_int = sym!(b"Py_IsInitialized\0", unsafe extern "C" fn() -> c_int);
        let save: unsafe extern "C" fn() -> *mut c_void = sym!(b"PyEval_SaveThread\0", unsafe extern "C" fn() -> *mut c_void);
        let run_string: unsafe extern "C" fn(*const c_char, c_int, Obj, Obj) -> Obj = sym!(b"PyRun_String\0", unsafe extern "C" fn(*const c_char, c_int, Obj, Obj) -> Obj);
        let add_module: unsafe extern "C" fn(*const c_char) -> Obj = sym!(b"PyImport_AddModule\0", unsafe extern "C" fn(*const c_char) -> Obj);
        let module_dict: unsafe extern "C" fn(Obj) -> Obj = sym!(b"PyModule_GetDict\0", unsafe extern "C" fn(Obj) -> Obj);
        let dict_get: unsafe extern "C" fn(Obj, *const c_char) -> Obj = sym!(b"PyDict_GetItemString\0", unsafe extern "C" fn(Obj, *const c_char) -> Obj);
        let dict_set: unsafe extern "C" fn(Obj, *const c_char, Obj) -> c_int = sym!(b"PyDict_SetItemString\0", unsafe extern "C" fn(Obj, *const c_char, Obj) -> c_int);
        let cfunc: unsafe extern "C" fn(*mut MethodDef, Obj, Obj) -> Obj = sym!(b"PyCFunction_NewEx\0", unsafe extern "C" fn(*mut MethodDef, Obj, Obj) -> Obj);
        let err_print: unsafe extern "C" fn() = sym!(b"PyErr_Print\0", unsafe extern "C" fn());
        let none: Obj = *lib.get::<*mut c_void>(b"_Py_NoneStruct\0").map_err(|e| e.to_string())?;
        let api = Api {
            ensure: sym!(b"PyGILState_Ensure\0", unsafe extern "C" fn() -> c_int),
            release: sym!(b"PyGILState_Release\0", unsafe extern "C" fn(c_int)),
            incref: sym!(b"Py_IncRef\0", unsafe extern "C" fn(Obj)),
            decref: sym!(b"Py_DecRef\0", unsafe extern "C" fn(Obj)),
            call_one: sym!(b"PyObject_CallOneArg\0", unsafe extern "C" fn(Obj, Obj) -> Obj),
            from_utf8: sym!(b"PyUnicode_FromStringAndSize\0", unsafe extern "C" fn(*const c_char, isize) -> Obj),
            as_utf8: sym!(b"PyUnicode_AsUTF8AndSize\0", unsafe extern "C" fn(Obj, *mut isize) -> *const c_char),
            to_str: sym!(b"PyObject_Str\0", unsafe extern "C" fn(Obj) -> Obj),
            err_fetch: sym!(b"PyErr_Fetch\0", unsafe extern "C" fn(*mut Obj, *mut Obj, *mut Obj)),
            err_clear: sym!(b"PyErr_Clear\0", unsafe extern "C" fn()),
            none,
            shim_op: std::ptr::null_mut(),
            _lib: lib,
        };
        // These buffers must outlive the interpreter (never finalized): leak them on purpose.
        let home: &'static [_] = Box::leak(wide(&paths.home).into_boxed_slice());
        let name: &'static [_] = Box::leak(wide(&paths.home.join(if cfg!(windows) { "python.exe" } else { "bin/python3" })).into_boxed_slice());
        set_home(home.as_ptr() as *const c_void);
        set_name(name.as_ptr() as *const c_void);
        init(0); // 0: leave the app's signal handlers alone
        if is_init() == 0 {
            return Err("the Python runtime did not initialize".into());
        }
        // Initialization left this thread holding the interpreter lock; give it up so every thread takes it only per call.
        let _ = save();
        let g = (api.ensure)();
        let result = (|| {
            let main = add_module(c"__ck_host__".as_ptr());
            let dict = module_dict(main);
            let def: &'static mut MethodDef = Box::leak(Box::new(MethodDef { name: c"_emit".as_ptr(), meth: emit, flags: 0x8, doc: std::ptr::null() })); // METH_O
            let f = cfunc(def, std::ptr::null_mut(), std::ptr::null_mut());
            if f.is_null() {
                return Err("cannot create the event callback".to_string());
            }
            dict_set(dict, c"_emit".as_ptr(), f);
            let roots = serde_json::to_string(&paths.roots.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>()).unwrap();
            let r = (api.from_utf8)(roots.as_ptr() as *const c_char, roots.len() as isize);
            dict_set(dict, c"ROOTS".as_ptr(), r);
            (api.decref)(r);
            let code = CString::new(SHIM).unwrap();
            let res = run_string(code.as_ptr(), 257, dict, dict); // Py_file_input
            if res.is_null() {
                err_print();
                return Err("the embedded Python helper failed to load".to_string());
            }
            (api.decref)(res);
            let op = dict_get(dict, c"op".as_ptr()); // borrowed
            if op.is_null() {
                return Err("the embedded Python helper has no entry point".to_string());
            }
            (api.incref)(op);
            Ok(op)
        })();
        (api.release)(g);
        let shim_op = result?;
        Ok(Api { shim_op, ..api })
    }
}

/// Load and start the interpreter once per process. Safe to call from any background thread; later calls return the first outcome.
pub fn init(paths: &Paths) -> Result<(), String> {
    API.get_or_init(|| load(paths)).as_ref().map(|_| ()).map_err(Clone::clone)
}

/// One operation of the embedded helper. **Blocks** for the interpreter lock and the operation: call it from a background
/// thread only, never under a UI/scheduler lock.
pub fn call(op: &str, args: Value) -> Result<Value, CallError> {
    let api = match API.get() {
        Some(Ok(a)) => a,
        Some(Err(e)) => return Err(CallError::new("runtime_unavailable", e.clone())),
        None => return Err(CallError::new("runtime_unavailable", "the embedded runtime was not started")),
    };
    let req = serde_json::to_string(&json!({"op": op, "args": args})).map_err(|e| CallError::new("bad_request", e.to_string()))?;
    let text = unsafe {
        let g = (api.ensure)();
        let arg = (api.from_utf8)(req.as_ptr() as *const c_char, req.len() as isize);
        let r = (api.call_one)(api.shim_op, arg);
        (api.decref)(arg);
        let out = if r.is_null() {
            let (mut t, mut v, mut tb) = (std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut());
            (api.err_fetch)(&mut t, &mut v, &mut tb);
            let mut msg = String::from("the embedded helper raised");
            if !v.is_null() {
                let s = (api.to_str)(v);
                if !s.is_null() {
                    let mut n = 0isize;
                    let p = (api.as_utf8)(s, &mut n);
                    if !p.is_null() {
                        msg = String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, n as usize)).into_owned();
                    }
                    (api.decref)(s);
                }
            }
            for o in [t, v, tb] {
                if !o.is_null() {
                    (api.decref)(o);
                }
            }
            (api.err_clear)();
            Err(msg)
        } else {
            let mut n = 0isize;
            let p = (api.as_utf8)(r, &mut n);
            let s = if p.is_null() { Err("the embedded helper returned no text".to_string()) } else { Ok(String::from_utf8_lossy(std::slice::from_raw_parts(p as *const u8, n as usize)).into_owned()) };
            (api.decref)(r);
            s
        };
        (api.release)(g);
        out
    }
    .map_err(|m| CallError::new("internal", m))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| CallError::new("internal", format!("unreadable helper answer: {e}")))?;
    if v.get("ok") == Some(&Value::Bool(true)) {
        Ok(v.get("result").cloned().unwrap_or(Value::Null))
    } else {
        Err(CallError::new(v.get("code").and_then(Value::as_str).unwrap_or("internal"), v.get("message").and_then(Value::as_str).unwrap_or("")))
    }
}

/// Run `call` on its own short-lived thread; the answer arrives on the channel (the caller never waits on Python).
pub fn spawn_call(op: &'static str, args: Value) -> Receiver<Result<Value, CallError>> {
    let (tx, rx) = channel();
    let _ = std::thread::Builder::new().name("ck-app-call".into()).spawn(move || {
        let _ = tx.send(call(op, args));
    });
    rx
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Starting,
    /// The endpoint answers: `port`, `fingerprint_sha256`, `network_id`, `name`, `pid`, ... (the helper's `info`).
    Running(Value),
    /// The old network service still owns this state: this app stays passive and says so.
    Passive { code: String, message: String },
    Failed { code: String, message: String },
    Stopped,
}

struct Shared {
    status: Mutex<Status>,
    wake: Arc<dyn Fn() + Send + Sync>,
    stopping: AtomicBool,
    start: Mutex<Option<std::thread::JoinHandle<()>>>,
    paths: Paths,
    state_dir: PathBuf,
}

/// Bring the endpoint up on a background thread; the outcome lands in `status`.
fn spawn_start(shared: &Arc<Shared>) {
    let s2 = shared.clone();
    let h = std::thread::Builder::new().name("ck-app-api-start".into()).spawn(move || {
        let fail = |code: &str, message: String| Status::Failed { code: code.into(), message };
        let st = match init(&s2.paths) {
            Err(e) => fail("runtime_unavailable", e),
            Ok(()) => match call("endpoint_start", json!({"state_dir": s2.state_dir})) {
                Ok(info) => Status::Running(info),
                Err(e) if e.code == "service_conflict" => Status::Passive { code: e.code, message: e.message },
                Err(e) => fail(&e.code, e.message),
            },
        };
        RUNNING.store(matches!(st, Status::Running(_)), Ordering::SeqCst);
        *s2.status.lock().unwrap() = st; // a Quit during start is handled by the stop thread, which joins this one first
        (s2.wake)();
    });
    *shared.start.lock().unwrap() = h.ok();
}

/// The endpoint's lifetime handle: `start` returns at once, the listener comes up on a background thread.
pub struct AppApi {
    shared: Arc<Shared>,
}

impl AppApi {
    pub fn start(paths: Paths, state_dir: PathBuf, wake: Arc<dyn Fn() + Send + Sync>) -> Self {
        std::hint::black_box(&ENDPOINT_MARKER);
        *sink().wake.lock().unwrap() = Some(wake.clone());
        let shared = Arc::new(Shared { status: Mutex::new(Status::Starting), wake, stopping: AtomicBool::new(false), start: Mutex::new(None), paths, state_dir });
        spawn_start(&shared);
        Self { shared }
    }

    /// Try the endpoint again after it stayed off (the old service held the state, or a port was taken). Does nothing unless it is
    /// Passive or Failed and no stop has begun. Called by the Retry button and when the Clients page opens; never on a timer.
    pub fn retry(&self) -> bool {
        if self.shared.stopping.load(Ordering::SeqCst) {
            return false;
        }
        {
            let mut st = self.shared.status.lock().unwrap();
            if !matches!(&*st, Status::Passive { .. } | Status::Failed { .. }) {
                return false;
            }
            *st = Status::Starting;
        }
        spawn_start(&self.shared);
        true
    }

    pub fn status(&self) -> Status {
        self.shared.status.lock().unwrap().clone()
    }

    /// Events that arrived since the last call (the app driver is woken for each).
    pub fn take_events(&self) -> Vec<Event> {
        take_events()
    }

    /// Close the listener, discovery and inbox. Idempotent. Waits at most `deadline` for the close to finish (never under a
    /// UI lock); returns whether it did. The close itself continues on its thread if the deadline passes.
    pub fn stop(&self, deadline: Duration) -> bool {
        if self.shared.stopping.swap(true, Ordering::SeqCst) {
            return matches!(self.status(), Status::Stopped);
        }
        RUNNING.store(false, Ordering::SeqCst); // nothing new is read from or written to the inbox once a stop begins
        let (tx, rx) = channel();
        let sh = self.shared.clone();
        let started = std::thread::Builder::new().name("ck-app-api-stop".into()).spawn(move || {
            let h = sh.start.lock().unwrap().take();
            if let Some(h) = h {
                let _ = h.join(); // a start still in flight finishes first, so its socket is the one we close
            }
            if matches!(API.get(), Some(Ok(_))) {
                let _ = call("endpoint_stop", json!({}));
            }
            *sh.status.lock().unwrap() = Status::Stopped;
            let _ = tx.send(());
            (sh.wake)();
        });
        started.is_ok() && rx.recv_timeout(deadline).is_ok()
    }
}

impl Drop for AppApi {
    fn drop(&mut self) {
        self.stop(Duration::ZERO);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runtime whose `encodings` package is gone would abort the process in `Py_InitializeEx`: it must come back as the notice instead.
    /// (An empty file stands in for the library: the check runs before anything is loaded.)
    #[test]
    fn damaged_runtime_is_a_notice_not_an_abort() {
        let home = std::env::temp_dir().join(format!("ck-app-api-damaged-{}", std::process::id()));
        let std_lib = if cfg!(windows) { home.join("Lib") } else { home.join("lib/python3.11") };
        std::fs::create_dir_all(&std_lib).unwrap();
        for f in ["os.py", "site.py"] {
            std::fs::write(std_lib.join(f), "").unwrap();
        }
        let library = home.join("libpython3.11.so.1.0");
        std::fs::write(&library, "").unwrap();
        let paths = Paths { library, home: home.clone(), roots: vec![] };
        let err = check_runtime(&paths).unwrap_err();
        assert!(err.contains("incomplete"), "{err}");
        let api = AppApi::start(paths, home.join("state"), Arc::new(|| {}));
        let end = std::time::Instant::now() + Duration::from_secs(10);
        while api.status() == Status::Starting && std::time::Instant::now() < end {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(matches!(api.status(), Status::Failed { code, message } if code == "runtime_unavailable" && message.contains("incomplete")));
        std::fs::remove_dir_all(&home).unwrap();
    }
}
