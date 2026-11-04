//! Worker protocol v1 over stdio (`ai-implementation-brief.md` §3, §7): versioned events, a persistent
//! local worker process per target and the
//! identity gate. Bounded line reader: a malformed or oversized line is a fatal `Msg::Protocol`.
use crate::jobs::KeyJob;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const MAX_LINE: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Gpu {
    pub index: u32,
    pub name: String,
    pub vram_gb: f64,
    /// Canonical `GPU-<uuid>` of the CUDA device the worker opened. Only trust it when `uuid_status == "verified"`.
    /// Absent (older worker) or unverified means remote/network eligibility is unavailable; local use is unaffected
    /// and the index/name/memory are never promoted to an identity.
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub uuid_status: Option<String>,
    #[serde(default)]
    pub uuid_reason: Option<String>,
}

/// `GPU-` + lowercase 8-4-4-4-12 hex, exactly what the worker and the coordinator use.
pub fn is_canonical_uuid(u: &str) -> bool {
    let Some(rest) = u.strip_prefix("GPU-") else { return false };
    let parts: Vec<&str> = rest.split('-').collect();
    parts.len() == 5 && parts.iter().zip([8, 4, 4, 4, 12]).all(|(p, n)| p.len() == n && p.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

impl Gpu {
    /// The verified identity, or why there is none (shown to the user; a receiver must refuse network work without it).
    pub fn verified_uuid(&self) -> Result<&str, String> {
        match (self.uuid.as_deref(), self.uuid_status.as_deref()) {
            (Some(u), Some("verified")) if is_canonical_uuid(u) => Ok(u),
            (Some(_), Some("verified")) => Err("the reported GPU uuid is not in canonical GPU-<8-4-4-4-12 hex> form".into()),
            (_, Some(_)) => Err(self.uuid_reason.clone().unwrap_or_else(|| "the GPU identity is unverified".into())),
            (_, None) => Err("this worker predates GPU identity reporting".into()),
        }
    }
}
/// The worker's transport-only acknowledgment: protocol and start success, plus the device it opened. It carries no
/// models and no capabilities (the app owns model state and sends exact files with every job).
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Ready {
    pub protocol: u32,
    pub engine: String,
    pub host: String,
    /// `sys.platform` of the worker ("linux", "win32"); informational.
    #[serde(default)]
    pub platform: Option<String>,
    pub gpu: Option<Gpu>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Complete,
    Cancelled,
    Failed,
}
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Done {
    pub job_id: String,
    pub outcome: Outcome,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub error_kind: Option<String>,
    #[serde(default)]
    pub manifest: Option<String>,
    #[serde(default)]
    pub published: u64,
    #[serde(default)]
    pub skipped: u64,
    #[serde(default)]
    pub expected: u64,
    #[serde(default)]
    pub manifest_complete: bool,
    /// Task jobs: `kind` and the worker's `result` object (published/expected/hint_dir/complete/outputs).
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum Event {
    Ready(Ready),
    Accepted {
        job_id: String,
    },
    /// `job_id` is null when the command itself was unparseable (code `protocol`).
    Rejected {
        job_id: Option<String>,
        code: String,
        message: String,
    },
    Status {
        job_id: String,
        message: String,
    },
    Progress {
        job_id: String,
        phase: String,
        done: u64,
        total: u64,
    },
    Warning {
        job_id: String,
        message: String,
    },
    Published {
        job_id: String,
        stems: Vec<String>,
    },
    Profile {
        job_id: String,
        hann_chunk: u32,
        hann_stride: u32,
        low_vram: bool,
    },
    /// SAM preview result: a mask PNG under the job's `staging_dir`.
    Preview {
        job_id: String,
        frame_stem: String,
        width: u32,
        height: u32,
        mask_png: String,
        #[serde(default)]
        score: Option<f64>,
    },
    Done(Done),
}

impl Event {
    pub fn job_id(&self) -> Option<&str> {
        match self {
            Event::Ready(_) => None,
            Event::Rejected { job_id, .. } => job_id.as_deref(),
            Event::Accepted { job_id }
            | Event::Status { job_id, .. }
            | Event::Progress { job_id, .. }
            | Event::Warning { job_id, .. }
            | Event::Published { job_id, .. }
            | Event::Profile { job_id, .. }
            | Event::Preview { job_id, .. } => Some(job_id),
            Event::Done(d) => Some(&d.job_id),
        }
    }
}

/// One protocol line → event. Wrong/missing `"v"`, unknown event names and missing fields are all errors.
pub fn parse_line(line: &str) -> Result<Event, String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("not JSON: {e}"))?;
    match v.get("v").and_then(Value::as_u64) {
        Some(1) => {}
        other => return Err(format!("unsupported protocol version {other:?}")),
    }
    serde_json::from_value(v).map_err(|e| format!("bad event: {e}"))
}

/// Hint-lock recovery requests. The app runs them through its one-shot helper (`backend/hint_lock_cli.py`), never the
/// render worker; the writer checks (exact job/host/pid, liveness proof, `.prev` restore) stay in that helper.
#[derive(Debug, Clone, PartialEq)]
pub enum LockCmd {
    Inspect {
        clip_root: PathBuf,
    },
    Clear {
        clip_root: PathBuf,
        job_id: String,
        host: String,
        pid: u64,
        /// `observed_exit` (local exit proven) or `user_confirmed_orphan` (the user says it is gone).
        reason: &'static str,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct LockReq {
    pub request_id: String,
    pub cmd: LockCmd,
}

/// What the app tells a worker. Model jobs carry the exact model files (`model_files`) taken from the app's snapshot.
pub enum Cmd<'a> {
    Key(&'a KeyJob, &'a crate::models::ModelFiles),
    Task(&'a crate::tasks::TaskJob, &'a crate::models::ModelFiles),
    Cancel(&'a str),
    Shutdown,
}

fn with_files(job: &impl Serialize, files: &crate::models::ModelFiles) -> Value {
    let mut v = serde_json::to_value(job).unwrap_or(Value::Null);
    if let (Some(o), false) = (v.as_object_mut(), files.is_empty()) {
        o.insert("model_files".into(), json!(files));
    }
    v
}

impl LockReq {
    /// The helper's JSON request.
    pub fn helper_request(&self) -> String {
        match &self.cmd {
            LockCmd::Inspect { clip_root } => json!({"cmd":"inspect","request_id":self.request_id,"clip_root":clip_root}),
            LockCmd::Clear { clip_root, job_id, host, pid, reason } => json!({"cmd":"clear","request_id":self.request_id,"clip_root":clip_root,
                "expect":{"job_id":job_id,"host":host,"pid":pid},"reason":reason}),
        }
        .to_string()
    }
}

impl Cmd<'_> {
    pub fn line(&self) -> String {
        match self {
            Cmd::Key(j, f) => json!({"v":1,"cmd":"key","job":with_files(*j, f)}),
            Cmd::Task(j, f) => json!({"v":1,"cmd":"task","job":with_files(*j, f)}),
            Cmd::Cancel(id) => json!({"v":1,"cmd":"cancel","job_id":id}),
            Cmd::Shutdown => json!({"v":1,"cmd":"shutdown"}),
        }
        .to_string()
        .chars()
        .fold(String::new(), |mut o, c| {
            // Non-ASCII only occurs inside JSON strings, where \uXXXX is equivalent: keeps the stream immune
            // to remote code pages (Windows consoles).
            let mut b = [0u16; 2];
            if c.is_ascii() {
                o.push(c);
            } else {
                for u in c.encode_utf16(&mut b) {
                    o.push_str(&format!("\\u{u:04x}"));
                }
            }
            o
        })
    }
}

// ---- targets -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum TargetKind {
    /// `python: None` → `import::python(repo)`.
    Local {
        repo: PathBuf,
        python: Option<PathBuf>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Target {
    pub id: String,
    #[serde(flatten)]
    pub kind: TargetKind,
    pub gpu: u32,
    #[serde(default = "yes")]
    pub enabled: bool,
}
fn yes() -> bool {
    true
}

impl Target {
    pub fn is_local(&self) -> bool {
        true
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty() || self.id.chars().any(char::is_control) {
            return Err("target id must be non-empty text".into());
        }
        Ok(())
    }

    fn command(&self) -> Result<Command, String> {
        self.python_module(&["backend.worker", "--gpu", &self.gpu.to_string()])
    }

    /// `python -m <module> <arg>` with this target's interpreter and checkout, for the app's one-shot helpers.
    pub fn one_shot(&self, module: &str, arg: &str) -> Result<Command, String> {
        self.python_module(&[module, arg])
    }

    fn python_module(&self, args: &[&str]) -> Result<Command, String> {
        self.validate()?;
        Ok(match &self.kind {
            TargetKind::Local { repo, python } => {
                // No `-I`: isolated mode drops the cwd from sys.path and `-m backend.worker` needs it.
                let mut c = Command::new(
                    python
                        .clone()
                        .unwrap_or_else(|| crate::import::python(repo)),
                );
                c.arg("-m")
                    .args(args)
                    .current_dir(repo)
                    .env("PYTHONUNBUFFERED", "1");
                // Installed runs: descriptor handed over, foreign Python variables removed.
                crate::runtime::prepare_python_command(
                    &mut c,
                    crate::runtime::launch(None).install.as_ref(),
                );
                c
            }
        })
    }
}

// ---- connection ----------------------------------------------------------------------------------

#[derive(Debug)]
pub enum Msg {
    Event(Event),
    /// Malformed, oversized or wrong-version line. The reader has stopped: kill the worker.
    Protocol(String),
    /// stdout closed and the process was reaped. Always last unless `Protocol` came first.
    Exited {
        /// Exit code; on Unix a negative value is the signal number that ended the process.
        code: Option<i32>,
        stderr: String,
    },
}

/// `Ok(None)` EOF, `Ok(Some(false))` line over `MAX_LINE`, `Ok(Some(true))` line in `buf`.
fn read_line(r: &mut impl BufRead, buf: &mut Vec<u8>) -> io::Result<Option<bool>> {
    loop {
        let avail = r.fill_buf()?;
        if avail.is_empty() {
            return Ok((!buf.is_empty()).then_some(true));
        }
        let (take, done) = match avail.iter().position(|b| *b == b'\n') {
            Some(i) => (i, true),
            None => (avail.len(), false),
        };
        buf.extend_from_slice(&avail[..take]);
        r.consume(take + usize::from(done));
        if buf.len() > MAX_LINE {
            return Ok(Some(false));
        }
        if done {
            if buf.last() == Some(&b'\r') {
                buf.pop();
            }
            return Ok(Some(true));
        }
    }
}

/// How long after the process exits we still wait for its stdout/stderr readers before reporting it.
const DRAIN_STDOUT: Duration = Duration::from_secs(2);
const DRAIN_STDERR: Duration = Duration::from_millis(300);

/// One persistent worker process. Nothing here blocks the caller: a waiter thread owns exit detection and
/// `try_recv` only polls a channel. Stopping is explicit and graded: `request_stop` (EOF on stdin = cancel and
/// exit, the worker finishes cleanly) and `kill` (hard, reports whether the process was really reaped).
/// Dropping never kills; it requests a stop and lets the waiter thread reap the child.
pub struct WorkerConn {
    rx: Receiver<Msg>,
    child: Arc<Mutex<Child>>,
    stdin: Option<ChildStdin>,
}

fn lock(c: &Mutex<Child>) -> std::sync::MutexGuard<'_, Child> {
    c.lock().unwrap_or_else(|e| e.into_inner())
}

impl WorkerConn {
    pub fn spawn(target: &Target) -> Result<Self, String> {
        let mut c = target.command()?;
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("could not start worker for {}: {e}", target.id))?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let child = Arc::new(Mutex::new(child));
        let (tx, rx) = channel();
        // stderr is drained on its own thread so a chatty worker can never stall; only a bounded tail is kept.
        let tail = Arc::new(Mutex::new(VecDeque::<String>::new()));
        let (err_done_tx, err_done) = channel::<()>();
        let t2 = tail.clone();
        std::thread::spawn(move || {
            let _done = err_done_tx;
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                let mut t = t2.lock().unwrap_or_else(|e| e.into_inner());
                t.push_back(l);
                if t.len() > 20 {
                    t.pop_front();
                }
            }
        });
        let (out_done_tx, out_done) = channel::<()>();
        let txr = tx.clone();
        std::thread::spawn(move || {
            let _done = out_done_tx; // dropped on return: the waiter sees the reader has finished
            let (mut r, mut buf) = (BufReader::new(out), vec![]);
            loop {
                buf.clear();
                let res = match read_line(&mut r, &mut buf) {
                    Ok(None) | Err(_) => return,
                    Ok(Some(false)) => Err(format!("line longer than {MAX_LINE} bytes")),
                    Ok(Some(true)) if buf.is_empty() => continue,
                    Ok(Some(true)) => std::str::from_utf8(&buf)
                        .map_err(|e| e.to_string())
                        .and_then(parse_line),
                };
                match res {
                    Ok(e) => {
                        let _ = txr.send(Msg::Event(e));
                    }
                    Err(e) => {
                        let _ = txr.send(Msg::Protocol(e));
                        return;
                    }
                }
            }
        });
        // Waiter: reports the exit after stdout has been read to its end (so `done` is never overtaken by
        // `Exited`) but never waits on a descendant that keeps a pipe open longer than the grace periods.
        let w = child.clone();
        std::thread::spawn(move || {
            let code = loop {
                match lock(&w).try_wait() {
                    Ok(Some(s)) => {
                        #[cfg(unix)]
                        if let Some(sig) = std::os::unix::process::ExitStatusExt::signal(&s) {
                            break Some(-sig); // negative = the signal that ended it (see `Msg::Exited`)
                        }
                        break s.code();
                    }
                    Ok(None) => {}
                    Err(_) => break None,
                }
                std::thread::sleep(Duration::from_millis(25));
            };
            let _ = out_done.recv_timeout(DRAIN_STDOUT);
            let _ = err_done.recv_timeout(DRAIN_STDERR);
            let stderr = tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            let _ = tx.send(Msg::Exited { code, stderr });
        });
        Ok(Self { rx, child, stdin })
    }

    pub fn send(&mut self, cmd: &Cmd) -> io::Result<()> {
        let s = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))?;
        writeln!(s, "{}", cmd.line())?;
        s.flush()
    }

    pub fn cancel(&mut self, job_id: &str) -> io::Result<()> {
        self.send(&Cmd::Cancel(job_id))
    }

    /// Never blocks.
    pub fn try_recv(&mut self) -> Option<Msg> {
        self.rx.try_recv().ok()
    }

    /// Graceful: close stdin. The worker treats EOF as cancel-and-exit, finishes its `done`, then exits.
    pub fn request_stop(&mut self) {
        self.stdin = None;
    }

    /// Hard stop, non-blocking: closes stdin and signals the process. It does NOT prove anything; the process
    /// is known gone only when `Msg::Exited` arrives (the waiter thread reaps it).
    pub fn kill(&mut self) {
        self.stdin = None;
        let _ = lock(&self.child).kill();
    }
}

impl Drop for WorkerConn {
    /// Graceful only: EOF asks the worker to cancel and exit; the waiter thread reaps it whenever it does.
    /// Use `kill` first if a hard stop is wanted.
    fn drop(&mut self) {
        self.stdin = None;
    }
}
