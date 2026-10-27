//! Network client controller: one coordinator connection per installation, owned by the App and driven by its
//! frame tick (never blocking). Submitter side: sends a confirmed Batch's frozen `JobSpec`s and follows their jobs.
//! Receiver side (opt-in): when this computer's verified GPU is idle it claims one job, runs the receiver
//! preflight on its own mappings, asks the coordinator to start the attempt and only then runs the job on its own
//! local scheduler client (the sole executor), reporting progress and the result. Durable local state records the
//! one started attempt, so a restart never claims or runs anything else while ownership is unresolved, and nothing
//! here ever releases a writer on a timeout. Credentials stay in their private file; only their path is a setting.
use crate::metrics::{ClientTelemetry, Sample, View};
use crate::netlink::{Link, LinkError, Op, Source, State};
use crate::network_setup::{ep_kind, host_of};
use crate::scheduler::JobState;
use crate::settings::Settings;
use crate::shared_paths::SharedRoot;
use crate::submission::{JobSpec, Preflight, Stage};
use crate::ui::ai::{AiUi, NetSubmit};
use crate::worker::Outcome;
use eframe::egui::{self, RichText, Ui};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const STATS_EVERY: Duration = Duration::from_secs(5);
const INSTALLS_EVERY: Duration = Duration::from_secs(4);
const CLAIM_EVERY: Duration = Duration::from_secs(3);
const JOB_POLL_EVERY: Duration = Duration::from_secs(3);
const PROGRESS_EVERY: Duration = Duration::from_millis(1500);
const RESTART_AFTER: Duration = Duration::from_secs(15);
const DIM: egui::Color32 = egui::Color32::from_rgb(169, 187, 175);

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    Off,
    Starting,
    Connecting,
    Online,
    Failed(String),
}

/// What a request was for (replies come back by id).
enum Pend {
    Connect,
    Register,
    Stats,
    Installs,
    Claim,
    Start(String),
    Reject,
    Events,
    Submit(NetJob),
    Job(String),
    JobEvents(String),
    Cancel(String),
    /// An empty event batch: asks the coordinator for the attempt's last accepted sequence.
    Resync,
    Estimate(String),
    /// Replaces the connected client's endpoint list (token and session are kept).
    Endpoints(Vec<String>, String),
    /// This PC retires its own login on the host (Remove).
    Leave,
}

/// How a Remove's leave request ended; the app finishes the local removal either way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    /// The host retired this PC's login, or it was already gone.
    Retired,
    /// The host runs an older Keyloom without the leave route.
    TooOld,
    /// Offline or another error: the host still lists this PC.
    Failed,
}

/// GPU load and VRAM use both under 50% on every GPU. A value that is missing or unknown counts as NOT free, never as free.
pub(crate) fn lightly_loaded(s: &Sample) -> bool {
    s.gpus.as_ref().is_some_and(|g| !g.is_empty() && g.iter().all(|g| {
        g.util.is_some_and(|u| u < 50.0) && matches!((g.vram_used, g.vram_total), (Some(u), Some(t)) if t > 0 && u * 2 < t)
    }))
}

#[derive(Clone, Debug)]
pub struct Install {
    pub id: String,
    pub name: String,
    pub gpu: Option<String>,
    pub connected: bool,
    pub last_seen_s: Option<f64>,
    pub stats: Value,
    received: Instant,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct NetStage {
    label: String,
    done: u64,
    total: u64,
    completed: bool,
    /// The worker pass the running PC reports (Key counts each pass from 0); absent from older PCs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    phase: Option<String>,
}

fn stage_list(spec: &JobSpec) -> Vec<NetStage> {
    let mut labels: Vec<String> = spec.hint_stages.as_ref().into_iter().flatten().map(|s| match s.as_str() {
        "gvm" => "Pre-matte · GVM".into(),
        "birefnet" => "Pre-matte · BiRefNet".into(),
        "chroma" => "Pre-matte · Chroma".into(),
        "matanyone2" => "Hint · MatAnyone2".into(),
        "videomama" => "Hint · VideoMaMa".into(),
        _ => format!("Hint · {s}"),
    }).collect();
    labels.push("Key".into());
    if let Some(e) = &spec.export {
        labels.extend(e.files.keys().map(|p| format!("Deliver · {p}")));
    }
    labels.into_iter().map(|label| NetStage { label, done: 0, total: 0, completed: false, phase: None }).collect()
}

/// A job this installation submitted, as the coordinator last reported it.
#[derive(Clone, Debug)]
pub struct NetJob {
    pub submission_id: String,
    submission: Option<Value>,
    installation: Option<String>,
    stages: Vec<NetStage>,
    active_stage: Option<usize>,
    cancel_requested: bool,
    events_done: bool,
    pub job_id: Option<String>,
    pub folder: String,
    pub name: String,
    pub state: String,
    pub on: Option<String>,
    pub done: u64,
    pub total: u64,
    pub reason: Option<String>,
    /// Stage the receiver last announced (Hint / Key / Deliver).
    pub stage: Option<String>,
    cursor: u64,
    refreshed: bool,
    last_poll: Option<Instant>,
    submission_attempted: bool,
    inflight: bool,
}

impl NetJob {
    fn terminal(&self) -> bool {
        matches!(self.state.as_str(), "succeeded" | "failed" | "cancelled" | "not sent")
    }

    fn can_take_unsent(&self) -> bool {
        self.job_id.is_none() && self.submission.is_some() && !self.submission_attempted && !self.inflight && self.last_poll.is_none() && !self.terminal()
    }
}

fn reconcile_submit(j: &mut NetJob, r: Result<Value, LinkError>, now: Instant) -> bool {
    match r {
        Ok(v) => {
            j.submission = None;
            j.inflight = false;
            j.job_id = v.get("job_id").and_then(Value::as_str).map(str::to_owned);
            j.state = v.get("state").and_then(Value::as_str).unwrap_or("queued").to_owned();
            j.reason = None;
            true
        }
        Err(e) if matches!(e.code.as_str(), "transport" | "not_connected") => {
            j.inflight = false;
            j.last_poll = Some(now);
            j.reason = Some("Waiting to resend the same submission after reconnect".into());
            true
        }
        Err(e) => {
            j.submission = None;
            j.inflight = false;
            j.state = "not sent".into();
            j.reason = Some(err_text(&e));
            false
        }
    }
}

struct Recv {
    /// The worker pass last announced (sent as a "pass: …" note: every host accepts notes, but rejects unknown
    /// fields in progress events).
    last_phase: String,
    attempt: String,
    job_id: String,
    /// manifest_dir / clip_root / exports of the attempt, kept in its durable file for restart reconciliation.
    meta: Value,
    /// Scheduler job ids in execution order with the stage's label.
    stages: Vec<(String, &'static str)>,
    announced: usize,
    outbox: Vec<Value>,
    seq: u64,
    last_done: u64,
    last_total: u64,
    last_report: Instant,
    last_progress: Instant,
    inflight: bool,
    retry_at: Option<Instant>,
    cancel_sent: bool,
    terminal: bool,
    report_saved: bool,
}

/// What the storage says about an attempt left by an earlier run.
enum Recon {
    /// Manifest complete, no writer lock, all deliveries present.
    Complete,
    Terminal(&'static str, String),
    /// No lock anywhere and the result is incomplete: nothing is writing, the run was cut short.
    Interrupted(String),
    /// A lock is present or the state cannot be read: a writer may still exist. Never released automatically.
    Fenced(String),
}

enum Rx {
    Idle,
    /// The receiver preflight runs on a thread (it scans the shared storage).
    Preflight { attempt: String, rx: Receiver<Result<Preflight, Vec<String>>> },
    /// A pinned temporal window is being checked against this GPU's free memory (engine estimator) before the start.
    Estimating { attempt: String, pf: Box<Preflight>, plan: Value, free_mb: u64 },
    Starting { attempt: String, pf: Box<Preflight> },
    Running(Recv),
    /// A previous run started an attempt this installation still owns: nothing else is claimed.
    Owned(String),
    /// Restart: the attempt's storage is inspected on a thread to learn whether anything can still be writing.
    Reconcile { attempt: String, meta: Value, rx: Receiver<Recon> },
    /// Restart: waiting for the coordinator's last sequence before reporting the reconciled end of the attempt.
    Resync { attempt: String, state: &'static str, message: String },
}

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    Enable { creds: String, on: bool },
    Receive { creds: String, on: bool },
    DefaultSubmit(String),
    Remove(String),
    CancelJob { creds: String, job_id: String },
    /// A send the PC never accepted: take it back and put the shot at the end of the Automatic list.
    Resend { creds: String, submission_id: String },
    ForgetAttempt(String),
    Prefer { creds: String, url: String },
}

/// One connection's settings for a tick (`Settings::network` lists many; each `Remote` runs exactly one).
#[derive(Clone, Debug, Default)]
pub struct Conn {
    pub creds: Option<PathBuf>,
    pub enabled: bool,
    pub receive: bool,
    /// This PC's name on the network.
    pub name: String,
    /// The set's round robin allows this connection to claim now (offers are serialized across networks).
    pub may_claim: bool,
    /// Known https addresses of the host, sent on connect and refreshed while online.
    pub endpoints: Vec<String>,
    /// The user's pick, already first in `endpoints` (empty = automatic).
    pub preferred: String,
    /// Exact host installation this connection targets (empty = every member, as before).
    pub target_host: String,
    /// This PC's queue receives work; other PCs are selected through their own host connection.
    pub local_host: bool,
}

pub struct Env<'a> {
    pub settings: &'a Settings,
    pub settings_path: Option<&'a Path>,
    pub ai: &'a mut AiUi,
    pub metrics: &'a View,
    pub repo: Option<&'a Path>,
    pub python: Option<&'a Path>,
    pub wake: &'a Arc<dyn Fn() + Send + Sync>,
    /// The connection being ticked (set by `RemoteSet` before each `Remote::tick`).
    pub cfg: Conn,
}

pub struct Remote {
    /// Stable identity: the credentials path (also the namespace of this connection's GPU-slot keys).
    pub(crate) key: String,
    source: Source,
    link: Option<Link>,
    pub phase: Phase,
    pend: HashMap<String, Pend>,
    leaving: bool,
    left: Option<Leave>,
    retry_at: Option<Instant>,
    me: Option<String>,
    network_url: Option<String>,
    network_id: Option<String>,
    registered: bool,
    /// Name sent with the last registration, whether the bridge/host rejected a name, and the user-facing note.
    reg_name: String,
    reg_sent_name: bool,
    name_unsupported: bool,
    name_note: Option<String>,
    jobs_loaded: bool,
    /// The network id remembered from an earlier connect (a PC's assignments stay valid while offline).
    cached_ns: Option<String>,
    register_note: Option<String>,
    reg_inflight: bool,
    stats_seq: u64,
    last_stats: Option<Instant>,
    last_installs: Option<Instant>,
    last_claim: Option<Instant>,
    last_probe: Option<Instant>,
    claim_inflight: bool,
    stats_inflight: bool,
    installs_inflight: bool,
    pub installs: Vec<Install>,
    pub jobs: Vec<NetJob>,
    rx: Rx,
    /// Attempt id whose GPU slot this installation holds (empty = none).
    held: String,
    pub receive_note: Option<String>,
    finished: Vec<String>,
    pub status: Option<String>,
    /// Shutdown began: nothing new is claimed.
    closing: bool,
    wake_hint: Option<Arc<dyn Fn() + Send + Sync>>,
    sent_endpoints: Vec<String>,
    sent_prefer: String,
    endpoints_inflight: bool,
    target_host: String,
    local_host: bool,
}

pub(crate) fn default_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok().filter(|s| !s.is_empty())
        // HOSTNAME is a shell variable, not exported to GUI launches; the kernel value matches the service's own name
        .or_else(|| std::fs::read_to_string("/proc/sys/kernel/hostname").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| "Keyloom PC".into())
}

fn attempt_meta(pf: &Preflight) -> Value {
    json!({
        "manifest_dir": pf.job.manifest_dir.display().to_string(),
        "clip_root": pf.job.clip_root.display().to_string(),
        "exports": pf.stages.iter().filter_map(|s| match s { Stage::Task(t) if t.kind == crate::tasks::TaskKind::ExportVideo => t.params.get("dest").and_then(Value::as_str).map(str::to_owned), _ => None }).collect::<Vec<_>>(),
    })
}

fn attempt_file(attempt: &str, job_id: &str, seq: u64, stages: &[(String, &'static str)], meta: &Value) -> Value {
    json!({"attempt_id": attempt, "job_id": job_id, "seq": seq, "stages": stages.iter().map(|s| &s.0).collect::<Vec<_>>(), "meta": meta})
}

/// A sent job's saved record as a history entry, keyed by connection and submission.
fn sent_entry(owner: &str, rec: Value) -> crate::queue::Entry {
    let text = |k: &str| rec[k].as_str().unwrap_or("").to_owned();
    crate::queue::Entry {
        id: format!("sent:{owner}:{}", text("submission_id")),
        folder: text("folder"),
        label: "Sent".into(),
        state: rec["state"].as_str().unwrap_or("queued").into(),
        detail: text("reason"),
        at: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        kind: "sent".into(),
        owner: owner.into(),
        job: Some(rec),
    }
}

fn side_file(creds: &Path, ext: &str) -> PathBuf {
    let mut n = creds.as_os_str().to_owned();
    n.push(ext);
    PathBuf::from(n)
}

/// Windows: remove inherited access and leave only the current user (the same `icacls` grant the Python credential
/// writer uses, `backend/coordinator/pki.py::protect_private`), then verify the list holds exactly that one entry.
/// Fails closed. Other platforms rely on the 0600/0700 modes set at creation.
#[cfg(windows)]
pub(crate) fn protect_private(path: &Path, dir: bool) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let run = |prog: &str, args: &[&str]| -> Result<String, String> {
        let o = std::process::Command::new(prog).args(args).creation_flags(0x0800_0000).output().map_err(|e| format!("{prog}: {e}"))?;
        if o.status.success() { Ok(String::from_utf8_lossy(&o.stdout).into_owned()) } else { Err(format!("{prog} failed for {}", path.display())) }
    };
    let who = run("whoami", &[])?.trim().to_owned();
    let p = path.display().to_string();
    let grant = if dir { format!("{who}:(OI)(CI)F") } else { format!("{who}:F") };
    run("icacls", &[&p, "/inheritance:r", "/grant:r", &grant])?;
    let out = run("icacls", &[&p])?;
    let aces: Vec<&str> = out.lines().filter(|l| l.contains(":(")).collect();
    let head = aces.first().map(|l| l.split(":(").next().unwrap_or("").trim_end().to_lowercase()).unwrap_or_default();
    let w = who.to_lowercase();
    if aces.len() == 1 && (head == w || (head.ends_with(&w) && head[..head.len() - w.len()].ends_with(char::is_whitespace))) {
        Ok(())
    } else {
        Err(format!("{} is still accessible to other users", path.display()))
    }
}
#[cfg(not(windows))]
fn protect_private(_path: &Path, _dir: bool) -> Result<(), String> {
    Ok(())
}

/// The folder credentials live in: created owner-only (0700 / a current-user-only ACL that new files inherit). An
/// existing folder is left as the user made it.
pub(crate) fn private_dir(d: &Path) -> Result<(), String> {
    if d.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d, std::fs::Permissions::from_mode(0o700)).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    protect_private(d, true)
}

/// Durable private write: temp file, fsync, rename. Every failure is returned; ownership is never assumed written.
pub(crate) fn write_private(path: &Path, v: &Value) -> Result<(), String> {
    use std::io::Write;
    let tmp = side_file(path, ".tmp");
    if let Some(d) = path.parent() {
        private_dir(d)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(v.to_string().as_bytes()).and_then(|()| f.sync_all()).map_err(|e| format!("{}: {e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))?;
    #[cfg(unix)]
    if let Some(d) = path.parent() {
        let _ = std::fs::File::open(d).and_then(|d| d.sync_all()); // make the rename itself durable
    }
    Ok(())
}

/// Inspect an earlier run's storage (on a helper thread): files only, nothing inferred from a pid or an age, nothing
/// written or removed.
fn reconcile(meta: &Value) -> Recon {
    let dir = |k: &str| meta.get(k).and_then(Value::as_str).map(PathBuf::from);
    let (Some(mdir), Some(clip)) = (dir("manifest_dir"), dir("clip_root")) else { return Recon::Fenced("the saved attempt has no storage paths".into()) };
    for lock in [mdir.join(crate::jobs::LOCK_NAME), clip.join(crate::hints::HINT_LOCK)] {
        match std::fs::symlink_metadata(&lock) {
            Ok(_) => return Recon::Fenced(format!("{} exists: a writer may still be running (or died without cleaning up).", lock.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Recon::Fenced(format!("{} cannot be inspected ({e}).", lock.display())),
        }
    }
    if let Some(t) = meta.get("terminal") {
        let state = match t.get("state").and_then(Value::as_str) {
            Some("succeeded") => Some("succeeded"),
            Some("failed") => Some("failed"),
            Some("cancelled") => Some("cancelled"),
            _ => None,
        };
        if let Some(state) = state {
            return Recon::Terminal(state, t.get("message").and_then(Value::as_str).unwrap_or("finished before disconnect").to_owned());
        }
    }
    let manifest = std::fs::read(mdir.join(crate::project::MANIFEST_NAME)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok());
    let complete = manifest.as_ref().and_then(|m| m.get("complete")).and_then(Value::as_bool) == Some(true);
    let exports: Vec<PathBuf> = meta.get("exports").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect();
    let missing: Vec<String> = exports.iter().filter(|p| !p.is_file()).map(|p| p.display().to_string()).collect();
    match (complete, missing.is_empty()) {
        (true, true) => Recon::Complete,
        (true, false) => Recon::Interrupted(format!("the Key result is complete but {} delivery file(s) are missing, e.g. {}", missing.len(), missing[0])),
        (false, _) if manifest.is_none() => Recon::Interrupted("no result was written before the app closed".into()),
        (false, _) => Recon::Interrupted("the result is incomplete (partial frames are kept in the result folder; Resume it locally)".into()),
    }
}

/// Bounded, secret-free diagnostics on stderr (phase changes and every claim/offer/start/report step).
pub(crate) fn nlog(msg: &str) {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    let t = T0.get_or_init(Instant::now).elapsed().as_secs_f32();
    let line = format!("[net {t:7.1}s] {}", truncate(msg, 240));
    eprintln!("{line}");
    if let Some(p) = LOG_FILE.get() {
        use std::io::Write;
        // Best effort, bounded: past 1 MiB the file is moved to .old (one generation) and started again.
        if std::fs::metadata(p).map_or(false, |m| m.len() > 1 << 20) {
            let _ = std::fs::rename(p, p.with_extension("log.old"));
        }
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{} {line}", chrono_free_stamp());
        }
    }
}

static LOG_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Persist `nlog` lines to this file (first call wins; the parent folder is created).
pub(crate) fn set_log_file(p: PathBuf) {
    if let Some(d) = p.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = LOG_FILE.set(p);
}

/// Seconds since the Unix epoch: enough to order log lines without a date crate.
fn chrono_free_stamp() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn truncate(s: &str, n: usize) -> String {
    s.chars().filter(|c| !c.is_control()).take(n).collect()
}

pub(crate) fn err_text(e: &LinkError) -> String {
    format!("{}: {}", e.code, truncate(&e.message, 300))
}

/// Whole-job percent for a collapsed node. Capped below 100 until the job succeeds; a state word when nothing moved.
fn net_overall(stages: &[NetStage], state: &str) -> String {
    if state == "succeeded" { return "100%".into(); }
    if stages.is_empty() || matches!(state, "failed" | "cancelled" | "not sent") || stages.iter().all(|s| !s.completed && s.done == 0) { return state.into(); }
    let f = stages.iter().map(|s| if s.completed { 1.0 } else if s.total > 0 { (s.done as f32 / s.total as f32).min(0.99) } else { 0.0 }).sum::<f32>() / stages.len() as f32;
    format!("{}%", (f * 100.0).floor() as u32)
}

impl Remote {
    /// Labels are meaningful before the first connection: the URL comes from the private credentials file (read for that
    /// one field only, never logged) and the network id from the persisted side file.
    pub fn new(source: Source, key: String) -> Self {
        let mut r = Self::blank(source, key);
        let creds = PathBuf::from(&r.key);
        r.network_url = std::fs::read(&creds).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["url"].as_str().map(str::to_owned));
        r.cached_ns = std::fs::read(side_file(&creds, ".network.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["network_id"].as_str().map(str::to_owned));
        r
    }

    fn blank(source: Source, key: String) -> Self {
        Self {
            key,
            source,
            link: None,
            phase: Phase::Off,
            pend: HashMap::new(),
            leaving: false,
            left: None,
            retry_at: None,
            me: None,
            network_url: None,
            network_id: None,
            registered: false,
            reg_name: String::new(),
            reg_sent_name: false,
            name_unsupported: false,
            name_note: None,
            jobs_loaded: false,
            cached_ns: None,
            register_note: None,
            reg_inflight: false,
            stats_seq: 0,
            last_stats: None,
            last_installs: None,
            last_claim: None,
            last_probe: None,
            claim_inflight: false,
            stats_inflight: false,
            installs_inflight: false,
            installs: vec![],
            jobs: vec![],
            rx: Rx::Idle,
            held: String::new(),
            receive_note: None,
            finished: vec![],
            status: None,
            closing: false,
            wake_hint: None,
            sent_endpoints: vec![],
            sent_prefer: String::new(),
            endpoints_inflight: false,
            target_host: String::new(),
            local_host: false,
        }
    }

    pub fn available(&self) -> bool {
        self.source != Source::Off
    }
    pub fn online(&self) -> bool {
        self.phase == Phase::Online
    }
    pub fn accepts_work(&self, cfg: &Conn) -> bool {
        !self.leaving && !cfg.local_host && cfg.enabled && cfg.receive && self.online()
    }
    pub fn registered(&self) -> bool {
        self.registered
    }
    /// Shots whose outputs just finished (the App re-reads them).
    pub fn take_finished(&mut self) -> Vec<String> {
        std::mem::take(&mut self.finished)
    }
    /// Why this computer may not drop this connection now (`None` = nothing would be orphaned).
    pub fn remove_blocker(&self, creds: Option<&Path>) -> Option<String> {
        if self.jobs.iter().any(|j| !j.terminal() || j.submission.is_some()) {
            return Some("jobs sent to the current network are still running".into());
        }
        if !matches!(self.rx, Rx::Idle) || !self.held.is_empty() {
            return Some("this computer is working on, or holding, a network job".into());
        }
        if let Some(c) = creds {
            if side_file(c, ".attempt.json").exists() {
                return Some("an unresolved network attempt is recorded on this computer".into());
            }
            if self.jobs.is_empty() {
                let pending = |jobs: &[Value]| jobs.iter().any(|j| !matches!(j["state"].as_str(), Some("succeeded" | "failed" | "cancelled" | "not sent")) || j.get("submission").is_some_and(|v| !v.is_null()));
                let unreadable = || Some("the saved network job list cannot be read".to_string());
                let (saved, err) = crate::queue::history_path().map(|p| crate::queue::read(&p)).unwrap_or_default();
                if err.is_some() {
                    return unreadable();
                }
                let owner = c.to_string_lossy();
                let mine: Vec<Value> = saved.into_iter().filter(|e| e.kind == "sent" && e.owner == owner).filter_map(|e| e.job).collect();
                if pending(&mine) {
                    return Some("saved jobs on the current network need to be reconnected and finished first".into());
                }
                // Not yet imported from the old per-connection file.
                match std::fs::read(side_file(c, ".jobs.json")) {
                    Ok(bytes) => match serde_json::from_slice::<Vec<Value>>(&bytes) {
                        Ok(jobs) if pending(&jobs) => {
                            return Some("saved jobs on the current network need to be reconnected and finished first".into());
                        }
                        Err(_) => return unreadable(),
                        _ => {}
                    },
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return unreadable(),
                    _ => {}
                }
            }
        }
        None
    }
    pub fn forget_attempt(&mut self, creds: Option<&Path>) {
        if let Some(c) = creds {
            let _ = std::fs::remove_file(side_file(c, ".attempt.json"));
        }
        self.rx = Rx::Idle; // the next tick releases the GPU slot
    }
    fn forget_file(env: &Env, yes: bool) {
        if let (true, Some(c)) = (yes, &env.cfg.creds) {
            let _ = std::fs::remove_file(side_file(c, ".attempt.json"));
        }
    }
    /// Begin shutdown: no further offers are claimed.
    pub fn shutdown_requested(&mut self) {
        self.closing = true;
    }
    /// A held attempt has no local work; its durable ownership survives closing.
    pub fn quiescent(&self) -> bool {
        matches!(self.rx, Rx::Idle | Rx::Owned(_)) || (self.closing && !self.online() && matches!(&self.rx, Rx::Running(r) if r.terminal && r.report_saved))
    }
    /// Another claim/offer/attempt is in progress on this connection (the set never overlaps two networks' work).
    pub(crate) fn claim_busy(&self) -> bool {
        self.claim_inflight || !matches!(self.rx, Rx::Idle) || !self.held.is_empty()
    }
    pub(crate) fn claim_ready(&self, now: Instant, cfg: &Conn) -> bool {
        cfg.local_host && cfg.receive && self.registered && self.online() && !self.closing && !self.claim_busy() && self.last_claim.is_none_or(|t| now.saturating_duration_since(t) >= CLAIM_EVERY)
    }
    pub(crate) fn claiming(&self) -> bool {
        self.claim_inflight
    }
    /// The coordinator's network id (remembered across restarts), the namespace of this connection's PCs.
    pub fn ns(&self) -> Option<String> {
        self.network_id.clone().or_else(|| self.cached_ns.clone())
    }
    /// This computer's own installation id on this network (from its last registration).
    pub fn me(&self) -> Option<&str> {
        self.me.as_deref()
    }
    /// The GPU-slot key of an attempt: namespaced by connection, so two networks' attempt ids can never alias.
    fn slot(&self, attempt: &str) -> String {
        format!("{}/{attempt}", self.key)
    }
    /// A connection that names its host targets only that exact installation (and never this PC itself);
    /// Older peer connections without metadata keep every member; our own queue supplies no remote targets.
    fn host_only(&self, id: &str) -> bool {
        !self.local_host && (self.target_host.is_empty() || (id == self.target_host && Some(id) != self.me.as_deref()))
    }
    /// The coordinator address from the credentials file (labels and discovery's known addresses).
    pub fn url(&self) -> Option<String> {
        self.network_url.clone()
    }
    pub fn label(&self) -> String {
        self.network_url.as_deref().map(|u| u.trim_start_matches("https://").trim_start_matches("http://").to_owned()).or_else(|| self.ns()).unwrap_or_else(|| Path::new(&self.key).parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "network".into()))
    }
    /// Remove: ask the host to retire this PC's login. False = not sent (offline), so the caller removes locally.
    pub fn leave(&mut self) -> bool {
        if self.local_host {
            return false; // this PC never retires its own login on its own service
        }
        if self.leaving {
            return true;
        }
        if !self.online() {
            self.leaving = false;
            return false;
        }
        self.leaving = true;
        if self.request_leave_if_ready() {
            true
        } else {
            self.leaving = false;
            false
        }
    }
    pub fn leaving(&self) -> bool {
        self.leaving
    }
    pub fn take_left(&mut self) -> Option<Leave> {
        self.left.take()
    }
    pub fn cancel_job(&mut self, job_id: &str) {
        if let Some(l) = &mut self.link {
            if let Ok(id) = l.request(Op::Cancel, json!({"job_id": job_id})) {
                self.pend.insert(id, Pend::Cancel(job_id.to_owned()));
                if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id.as_deref() == Some(job_id)) { j.cancel_requested = true; }
            }
        }
    }
    /// Take back a send this PC never accepted (no job id yet); false if it was accepted meanwhile.
    pub fn take_unsent(&mut self, submission_id: &str) -> bool {
        let Some(i) = self.jobs.iter().position(|j| j.submission_id == submission_id && j.can_take_unsent()) else { return false };
        self.jobs.remove(i);
        let key = self.key.clone();
        self.write_jobs(Path::new(&key));
        true
    }
    /// A send of ours this connection follows: `Ok(())` while it runs, `Err(None)` once it ended without failing
    /// (done or cancelled), `Err(Some(why))` once it failed or the host refused it. `None` = not followed here.
    pub(crate) fn sent_outcome(&self, submission_id: &str) -> Option<Result<(), Option<String>>> {
        let j = self.jobs.iter().find(|j| j.submission_id == submission_id)?;
        Some(if !j.terminal() { Ok(()) } else if j.state == "failed" || j.state == "not sent" { Err(Some(j.reason.clone().unwrap_or_else(|| j.state.clone()))) } else { Err(None) })
    }
    pub fn receiving(&self) -> Option<String> {
        match &self.rx {
            Rx::Idle => None,
            Rx::Preflight { attempt, .. } => Some(format!("checking offer {attempt}")),
            Rx::Starting { attempt, .. } => Some(format!("starting {attempt}")),
            Rx::Estimating { attempt, .. } => Some(format!("checking GPU memory for {attempt}")),
            Rx::Running(r) => Some(format!("running {}", r.job_id)),
            Rx::Owned(m) => Some(m.clone()),
            Rx::Reconcile { attempt, .. } | Rx::Resync { attempt, .. } => Some(format!("reconciling {attempt} after a restart")),
        }
    }

    /// Accept a Batch submission for this network now (kept and sent when it is online).
    pub(crate) fn enqueue(&mut self, n: NetSubmit, env: &Env) {
        self.load_jobs(env.cfg.creds.as_deref());
        self.send(n, env);
    }

    fn stop(&mut self) {
        if let Rx::Running(r) = &mut self.rx {
            r.inflight = false; // the request died with the link; the outbox replays after reconnect
        }
        if matches!(self.rx, Rx::Preflight { .. } | Rx::Estimating { .. } | Rx::Starting { .. }) {
            // (unstarted offers) — and a restart reconciliation in flight simply restarts from the saved attempt file on reconnect
            self.rx = Rx::Idle;
        }
        if matches!(self.rx, Rx::Reconcile { .. } | Rx::Resync { .. }) {
            self.held.clear();
            self.rx = Rx::Idle; // unstarted: the coordinator offer is revoked/expired, the slot is released next tick
        }
        self.link = None; // Drop kills and reaps the bridge; the coordinator keeps ownership of anything started
        self.pend.clear();
        if std::mem::take(&mut self.leaving) {
            self.left = Some(Leave::Failed); // the link died before the host answered
        }
        for j in &mut self.jobs {
            j.inflight = false;
        }
        self.registered = false;
        self.reg_inflight = false;
        self.name_unsupported = false;
        self.claim_inflight = false;
        self.stats_inflight = false;
        self.installs_inflight = false;
        self.me = None;
        self.last_stats = None;
        self.last_installs = None;
        self.endpoints_inflight = false;
    }

    fn req(&mut self, op: Op, args: Value, p: Pend) -> bool {
        let Some(l) = &mut self.link else { return false };
        match l.request(op, args) {
            Ok(id) => {
                self.pend.insert(id, p);
                true
            }
            Err(_) => false,
        }
    }

    // Keep credentials until a pending Submit can no longer reveal an accepted job.
    fn leave_ready(&self) -> bool {
        self.jobs_loaded
            && !self.pend.values().any(|p| matches!(p, Pend::Submit(_)))
            && self.jobs.iter().all(|j| j.terminal() && j.submission.is_none())
    }

    fn request_leave_if_ready(&mut self) -> bool {
        if !self.leaving
            || self.pend.values().any(|p| matches!(p, Pend::Leave))
            || !self.leave_ready()
        {
            return true;
        }
        self.req(Op::Leave, json!({}), Pend::Leave)
    }

    fn submits_after_marker_save(
        &mut self,
        first: Vec<(NetJob, Value)>,
        mut retries: Vec<(NetJob, Value)>,
        saved: bool,
    ) -> Vec<(NetJob, Value)> {
        if saved {
            retries.extend(first);
        } else {
            for (submitted, _) in first {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.submission_id == submitted.submission_id) {
                    j.submission_attempted = false;
                    j.inflight = false;
                    j.last_poll = None;
                }
            }
        }
        retries
    }

    // ---- the frame tick --------------------------------------------------------------------------------------------

    pub fn tick(&mut self, now: Instant, env: &mut Env) {
        self.wake_hint = Some(env.wake.clone());
        self.target_host.clone_from(&env.cfg.target_host);
        self.local_host = env.cfg.local_host;
        if !self.held.is_empty() && matches!(self.rx, Rx::Idle) {
            env.ai.release_network_slot(&self.held);
            self.held.clear();
        }
        let creds = env.cfg.creds.clone();
        self.load_jobs(creds.as_deref());
        if !self.online() && matches!(self.rx, Rx::Running(_)) {
            self.run_attempt(now, env, creds.as_deref());
        }
        let enabled = env.cfg.enabled && creds.as_ref().is_some_and(|c| c.exists());
        if !enabled {
            if self.link.is_some() || self.phase != Phase::Off {
                self.stop();
                self.phase = if env.cfg.enabled { Phase::Failed("not joined yet: Join this PC on the Clients page".into()) } else { Phase::Off };
            }
            return;
        }
        if self.link.is_none() {
            if self.retry_at.is_some_and(|t| now < t) {
                return;
            }
            match self.source.command(env.repo, env.python) {
                Ok(Some(cmd)) => match Link::start(cmd, env.wake.clone()) {
                    Ok(l) => {
                        self.link = Some(l);
                        nlog("bridge started");
                        self.phase = Phase::Starting;
                    }
                    Err(e) => self.fail(now, e),
                },
                Ok(None) => self.phase = Phase::Off,
                Err(e) => self.fail(now, e),
            }
            if self.link.is_none() {
                return;
            }
        }
        let state = self.link.as_ref().map(Link::state);
        match state {
            Some(State::Ended(why)) => {
                // An answer that arrived before the drop still counts (a Remove answered identity_revoked is Retired).
                for r in self.link.as_mut().map(Link::poll).unwrap_or_default() {
                    if let Some(Pend::Leave) = r.id.and_then(|id| self.pend.remove(&id)) {
                        self.on_reply(Pend::Leave, r.outcome, now, env);
                    }
                }
                self.stop();
                self.fail(now, why);
                return;
            }
            Some(State::Starting) | None => return,
            Some(State::Ready) => {
                if self.phase == Phase::Starting && !self.pend.contains_key("_ready_logged") {
                    nlog("bridge ready");
                }
            }
        }
        let replies = self.link.as_mut().map(Link::poll).unwrap_or_default();
        for r in replies {
            if let Some(id) = &r.id {
                if let Some(p) = self.pend.remove(id) {
                    self.on_reply(p, r.outcome, now, env);
                }
            }
        }
        if self.link.is_none() {
            return;
        }
        match self.phase.clone() {
            Phase::Starting => {
                if let Some(c) = &creds {
                    let mut args = json!({"credentials_path": c.display().to_string()});
                    let eps: Vec<String> = env.cfg.endpoints.iter().take(16).cloned().collect();
                    if !eps.is_empty() {
                        args["endpoints"] = json!(eps);
                    }
                    if !env.cfg.preferred.is_empty() {
                        args["prefer"] = json!(env.cfg.preferred);
                    }
                    if self.req(Op::Connect, args, Pend::Connect) {
                        self.sent_endpoints = eps;
                        self.sent_prefer = env.cfg.preferred.clone();
                        self.phase = Phase::Connecting;
                    }
                }
            }
            Phase::Online => {
                let want: Vec<String> = env.cfg.endpoints.iter().take(16).cloned().collect();
                if !self.endpoints_inflight && (want != self.sent_endpoints || env.cfg.preferred != self.sent_prefer) {
                    let mut a = json!({"urls": want});
                    if !env.cfg.preferred.is_empty() {
                        a["prefer"] = json!(env.cfg.preferred);
                    }
                    self.endpoints_inflight = self.req(Op::Endpoints, a, Pend::Endpoints(want, env.cfg.preferred.clone()));
                }
                self.drive(now, env, creds.as_deref())
            }
            _ => {}
        }
    }

    fn fail(&mut self, now: Instant, why: String) {
        nlog(&format!("link failed: {why}"));
        self.retry_at = Some(now + RESTART_AFTER);
        self.phase = Phase::Failed(truncate(&why, 300));
    }

    // ---- replies ---------------------------------------------------------------------------------------------------

    fn on_reply(&mut self, p: Pend, out: Result<Value, LinkError>, now: Instant, env: &mut Env) {
        match (&p, &out) {
            (Pend::Stats | Pend::Installs | Pend::Job(_) | Pend::JobEvents(_), Ok(_)) => {}
            (_, Ok(v)) => nlog(&format!("{} ok {}", Self::pname(&p), v.to_string().chars().take(120).collect::<String>())),
            (_, Err(e)) => nlog(&format!("{} ERR {}", Self::pname(&p), err_text(e))),
        }
        match (p, out) {
            (Pend::Endpoints(sent, prefer), r) => {
                self.endpoints_inflight = false;
                self.sent_endpoints = sent; // retry only when addresses or the preferred one change, or the link reconnects
                self.sent_prefer = prefer;
                if let Err(e) = r {
                    self.status = Some(format!("Could not update host addresses: {}", err_text(&e)));
                }
            }
            (Pend::Leave, r) => {
                self.leaving = false;
                self.left = Some(match r {
                    Ok(_) => Leave::Retired,
                    Err(e) if e.code == "host_too_old" => Leave::TooOld,
                    Err(e) if matches!(e.code.as_str(), "identity_revoked" | "unknown_installation" | "unauthorized") => Leave::Retired,
                    Err(_) => Leave::Failed,
                });
            }
            (Pend::Connect, Ok(v)) => {
                self.network_url = v.get("url").and_then(Value::as_str).map(str::to_owned);
                self.network_id = v.get("network_id").and_then(Value::as_str).map(str::to_owned);
                self.me = v.get("installation_id").and_then(Value::as_str).map(str::to_owned);
                self.phase = Phase::Online;
                self.retry_at = None;
                if let (Some(c), Some(n)) = (&env.cfg.creds, &self.network_id) {
                    if self.cached_ns.as_ref() != Some(n) {
                        self.cached_ns = Some(n.clone());
                        let _ = write_private(&side_file(c, ".network.json"), &json!({"network_id": n}));
                    }
                }
                self.load_state(env);
            }
            (Pend::Connect, Err(e)) => {
                self.stop();
                self.fail(now, format!("could not connect ({})", err_text(&e)));
            }
            (Pend::Register, r) => {
                self.reg_inflight = false;
                match r {
                    Ok(_) => {
                        self.registered = true;
                        self.register_note = None;
                        self.name_note = (self.name_unsupported).then(|| "This network's host is too old to sync the PC name: update Keyloom on the host.".into());
                    }
                    // An older host may reject the name field: register again without it (the next tick), rendering unchanged.
                    Err(e) if self.reg_sent_name && e.code == "invalid" && e.message.contains("unknown keys") && e.message.contains("'name'") => {
                        self.name_unsupported = true;
                        self.register_note = Some(format!("registering without the PC name ({})", err_text(&e)));
                    }
                    Err(e) => self.register_note = Some(format!("not registered ({})", err_text(&e))),
                }
            }
            (Pend::Stats, Ok(v)) => {
                self.stats_inflight = false;
                // Reopening keeps the session, so resume after its last accepted sample.
                self.stats_seq = self.stats_seq.max(v.get("seq").and_then(Value::as_u64).unwrap_or(0));
            }
            (Pend::Stats, Err(_)) => self.stats_inflight = false,
            (Pend::Installs, Ok(v)) => {
                self.installs_inflight = false;
                self.set_installs(&v);
            }
            (Pend::Installs, Err(e)) => {
                self.installs_inflight = false;
                // A failed membership refresh cannot establish a healthy connection (including TLS or revoked credentials).
                self.stop();
                self.fail(now, format!("connection lost ({})", err_text(&e)));
            }
            (Pend::Claim, r) => {
                self.claim_inflight = false;
                match r {
                    Ok(v) => self.on_claim(v, env),
                    Err(e) => self.receive_note = Some(format!("claim failed ({})", err_text(&e))),
                }
            }
            (Pend::Start(attempt), r) => self.on_start(attempt, r, env),
            (Pend::Reject, _) => {}
            (Pend::Resync, r) => self.on_resync(r, env),
            (Pend::Estimate(a), r) => self.on_estimate(a, r, env),
            (Pend::Events, r) => self.on_events(r, env),
            (Pend::Submit(submitted), r) => {
                self.on_submit_reply(submitted, r, now);
                self.save_jobs(env);
            }
            (Pend::Job(jid), r) => {
                let installs = self.installs.clone();
                if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id.as_deref() == Some(jid.as_str())) {
                    j.inflight = false;
                    if let Ok(v) = r {
                        j.state = v.get("state").and_then(Value::as_str).unwrap_or(&j.state).to_owned();
                        j.cancel_requested = v.get("cancel_requested").and_then(Value::as_bool).unwrap_or(j.cancel_requested);
                        j.reason = v.get("reason").and_then(Value::as_str).map(|s| truncate(s, 300));
                        if j.state == "queued" {
                            let why: Vec<String> = v.get("ineligible").and_then(Value::as_array).into_iter().flatten().filter_map(|x| Some(format!("{}: {}", x.get("name")?.as_str()?, x.get("reason")?.as_str()?))).collect();
                            if !why.is_empty() {
                                j.reason = Some(truncate(&format!("waiting; not eligible on {}", why.join("; ")), 300));
                            }
                        }
                        if let Some(id) = v.get("installation_id").and_then(Value::as_str) {
                            j.installation = Some(id.to_owned());
                            j.on = installs.iter().find(|x| x.id == id).map(|x| x.name.clone());
                        }
                        if j.state == "succeeded" { j.done = j.total; for s in &mut j.stages { s.completed = true; } }
                        if j.state == "succeeded" && !j.refreshed {
                            j.refreshed = true;
                            self.finished.push(j.folder.clone());
                        }
                    }
                }
                self.save_jobs(env);
            }
            (Pend::JobEvents(jid), Ok(v)) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id.as_deref() == Some(jid.as_str())) {
                    j.inflight = false;
                    j.events_done = j.terminal() && v.get("events").and_then(Value::as_array).is_some_and(|e| e.len() < 50);
                    for e in v.get("events").and_then(Value::as_array).into_iter().flatten() {
                        if e.get("type").and_then(Value::as_str) == Some("note") {
                            if let Some(p) = e["data"]["message"].as_str().and_then(|m| m.strip_prefix("pass: ")) {
                                if let Some(stage) = j.active_stage.and_then(|i| j.stages.get_mut(i)) { stage.phase = Some(p.to_owned()); }
                                continue;
                            }
                            j.stage = e["data"]["message"].as_str().map(|m| truncate(m, 80));
                            if let Some(index) = j.stage.as_deref().and_then(|m| m.strip_prefix("stage ")).and_then(|m| m.split_once(" of ")).and_then(|(n, _)| n.parse::<usize>().ok()).and_then(|n| n.checked_sub(1)).filter(|n| *n < j.stages.len()) {
                                j.active_stage = Some(index);
                                for stage in j.stages.iter_mut().take(index) { stage.completed = true; }
                            }
                        }
                        if e.get("type").and_then(Value::as_str) == Some("progress") {
                            let done = e["data"]["done"].as_u64().unwrap_or(0);
                            let total = e["data"]["total"].as_u64().unwrap_or(0);
                            if let Some(stage) = j.active_stage.and_then(|i| j.stages.get_mut(i)) {
                                stage.done = done;
                                stage.total = total;

                                if stage.label == "Key" { j.done = done; j.total = total; }
                            } else { j.done = done; j.total = total; }
                        }
                    }
                    j.cursor = v.get("cursor").and_then(Value::as_u64).unwrap_or(j.cursor);
                    if j.state == "succeeded" { j.done = j.total; for s in &mut j.stages { s.completed = true; } }
                }
                self.save_jobs(env);
            }
            (Pend::Cancel(jid), Err(e)) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id.as_deref() == Some(jid.as_str())) {
                    j.cancel_requested = false;
                    j.reason = Some(format!("Cancel failed: {}", err_text(&e)));
                }
            }
            (Pend::JobEvents(jid), Err(_)) => {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.job_id.as_deref() == Some(jid.as_str())) { j.inflight = false; }
            }
            (Pend::Cancel(_), Ok(_)) => {}
        }
    }

    fn on_submit_reply(&mut self, mut submitted: NetJob, r: Result<Value, LinkError>, now: Instant) {
        let existing = self.jobs.iter().position(|j| j.submission_id == submitted.submission_id);
        if let Some(i) = existing {
            reconcile_submit(&mut self.jobs[i], r, now);
        } else if reconcile_submit(&mut submitted, r, now) {
            self.jobs.push(submitted);
        }
    }

    fn pname(p: &Pend) -> &'static str {
        match p {
            Pend::Endpoints(..) => "endpoints",
            Pend::Connect => "connect",
            Pend::Register => "register",
            Pend::Stats => "stats",
            Pend::Installs => "installations",
            Pend::Claim => "claim",
            Pend::Start(_) => "start",
            Pend::Reject => "reject",
            Pend::Events => "events",
            Pend::Submit(_) => "submit",
            Pend::Job(_) => "job",
            Pend::JobEvents(_) => "job_events",
            Pend::Cancel(_) => "cancel",
            Pend::Resync => "resync",
            Pend::Estimate(_) => "estimate",
            Pend::Leave => "leave",
        }
    }

    fn set_installs(&mut self, v: &Value) {
        self.installs = v
            .get("installations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|i| {
                Some(Install {
                    id: i.get("installation_id")?.as_str()?.to_owned(),
                    name: i.get("name").and_then(Value::as_str).unwrap_or("?").to_owned(),
                    gpu: i.get("gpu_name").and_then(Value::as_str).map(str::to_owned),
                    connected: i.get("connected").and_then(Value::as_bool).unwrap_or(false),
                    last_seen_s: i.get("last_seen_age_s").and_then(Value::as_f64),
                    stats: i.get("stats").cloned().unwrap_or(Value::Null),
                    received: Instant::now(),
                })
            })
            .collect();
    }
    /// This network's PCs with a GPU, as picker entries `network id/PC id` (stable across hosts and restarts).
    pub fn targets(&self, label: Option<&str>) -> Vec<(String, String)> {
        let Some(ns) = self.ns() else { return vec![] };
        self.installs.iter().filter(|i| i.gpu.is_some() && self.host_only(&i.id)).map(|i| (format!("{ns}/{}", i.id), format!("{}{}{}", i.name, if i.connected && self.online() { "" } else { " (offline)" }, label.map_or(String::new(), |l| format!(" · {l}"))))).collect()
    }

    // ---- driving ---------------------------------------------------------------------------------------------------

    fn drive(&mut self, now: Instant, env: &mut Env, creds: Option<&Path>) {
        let due = |last: Option<Instant>, every: Duration| last.is_none_or(|t| now.saturating_duration_since(t) >= every);
        // 1. This computer's verified GPU (needed to register, report stats and receive).
        let local = env.ai.scheduler().targets().into_iter().find(|t| t.local && t.enabled);
        let hello_gpu = local.as_ref().and_then(|t| t.hello.as_ref()).and_then(|h| h.gpu.clone());
        let sample = if let View::Live(s) = env.metrics { Some(s.clone()) } else { None };
        if env.cfg.local_host && hello_gpu.is_none() && due(self.last_probe, Duration::from_secs(60)) {
            self.last_probe = Some(now);
            env.ai.probe_workers(); // start the local client so its GPU identity is known
        }
        // A renamed PC registers again right away; a host that cannot take names keeps the old registration.
        if self.registered && !self.reg_inflight && !self.name_unsupported && self.reg_name != truncate(env.cfg.name.trim(), 64) {
            self.registered = false;
        }
        if env.cfg.local_host && !self.registered && !self.reg_inflight {
            match (&hello_gpu, &sample) {
                (Some(g), Some(s)) => match g.verified_uuid() {
                    Ok(u) => {
                        let ram_mb = s.ram_total.map(|b| b / (1 << 20));
                        if let Some(ram) = ram_mb.filter(|r| *r > 0) {
                            let mut args = json!({"build": format!("keyloom-ui {}", env!("CARGO_PKG_VERSION")), "gpu": {"uuid": u, "name": truncate(&g.name, 120), "vram_total_mb": ((g.vram_gb * 1024.0) as u64).max(1), "ram_total_mb": ram}});
                            let name = truncate(env.cfg.name.trim(), 64); // the host accepts 1-64 characters
                            self.reg_sent_name = !name.is_empty() && !self.name_unsupported;
                            if self.reg_sent_name {
                                args["name"] = json!(name);
                            }
                            self.reg_name = if self.reg_sent_name { name } else { String::new() };
                            self.reg_inflight = self.req(Op::Register, args, Pend::Register);
                        }
                    }
                    Err(e) => self.register_note = Some(format!("this computer cannot take network work: {e}")),
                },
                (None, _) => self.register_note = Some("waiting for the local client's GPU identity (Clients > Check workers)".into()),
                (_, None) => self.register_note = Some("waiting for system metrics".into()),
            }
        }
        // 2. Stats / clients.
        if self.registered && !self.stats_inflight && due(self.last_stats, STATS_EVERY) {
            if let Some(s) = &sample {
                self.last_stats = Some(now);
                self.stats_seq += 1;
                let mut o = serde_json::Map::new();
                o.insert("seq".into(), json!(self.stats_seq));
                o.insert("measured_age_ms".into(), json!(0));
                if let Some(c) = s.cpu_percent {
                    o.insert("cpu_percent".into(), json!(c));
                }
                if let (Some(u), Some(t)) = (s.ram_used, s.ram_total) {
                    o.insert("ram_used_bytes".into(), json!(u));
                    o.insert("ram_total_bytes".into(), json!(t));
                }
                if let Some(g) = &s.gpus {
                    let list: Vec<Value> = g
                        .iter()
                        .filter(|x| x.uuid.len() >= 8)
                        .take(8)
                        .map(|x| {
                            let mut m = serde_json::Map::new();
                            m.insert("uuid".into(), json!(x.uuid));
                            m.insert("index".into(), json!(x.index.unwrap_or(0)));
                            m.insert("name".into(), json!(x.name.clone().unwrap_or_else(|| "GPU".into())));
                            if let Some(u) = x.util {
                                m.insert("utilization_percent".into(), json!(u));
                            }
                            if let (Some(u), Some(t)) = (x.vram_used, x.vram_total) {
                                m.insert("vram_used_bytes".into(), json!(u));
                                m.insert("vram_total_bytes".into(), json!(t));
                            }
                            Value::Object(m)
                        })
                        .collect();
                    o.insert("gpus".into(), json!(list));
                }
                self.stats_inflight = self.req(Op::Stats, Value::Object(o), Pend::Stats);
            }
        }
        if !self.installs_inflight && due(self.last_installs, INSTALLS_EVERY) {
            self.last_installs = Some(now);
            self.installs_inflight = self.req(Op::Installations, json!({}), Pend::Installs);
        }
        // 3. Submitter: send what the Batch confirmed, follow what was sent.
        self.poll_jobs(now);
        // 4. Receiver.
        self.receive(now, env, creds, local.as_ref().map(|t| (t.status.clone(), t.busy_with.is_some())), hello_gpu.as_ref().and_then(|g| g.verified_uuid().ok().map(str::to_owned)), sample.as_ref());
        if self.leaving && !self.request_leave_if_ready() {
            self.leaving = false;
            self.left = Some(Leave::Failed);
        }
    }

    fn send(&mut self, n: NetSubmit, env: &Env) {
        let stages = stage_list(&n.spec);
        let installation = n.target.clone();
        let body = json!({"submission_id": n.submission_id, "spec": serde_json::to_value(&n.spec).unwrap_or_default(), "target": n.target});
        self.jobs.push(NetJob { submission_id: n.submission_id.clone(), submission: Some(body), installation, stages, active_stage: None, cancel_requested: false, events_done: false, job_id: None, folder: n.folder, name: n.name, state: "sending".into(), on: None, done: 0, total: 0, reason: None, stage: None, cursor: 0, refreshed: false, last_poll: None, submission_attempted: false, inflight: false });
        self.save_jobs(env);
    }

    fn poll_jobs(&mut self, now: Instant) {
        let mut retries = vec![];
        let mut first = vec![];
        for j in &mut self.jobs {
            if let Some(body) = &j.submission {
                if !j.inflight && j.last_poll.is_none_or(|t| now.saturating_duration_since(t) >= JOB_POLL_EVERY) {
                    j.inflight = true;
                    j.last_poll = Some(now);
                    if !j.submission_attempted {
                        j.submission_attempted = true;
                        first.push((j.clone(), body.clone()));
                    } else {
                        retries.push((j.clone(), body.clone()));
                    }
                }
            }
        }
        if !first.is_empty() {
            let key = self.key.clone();
            let saved = self.write_jobs(Path::new(&key));
            retries = self.submits_after_marker_save(first, retries, saved);
        }
        for (submitted, body) in retries {
            let sid = submitted.submission_id.clone();
            if !self.req(Op::Submit, body, Pend::Submit(submitted)) {
                if let Some(j) = self.jobs.iter_mut().find(|j| j.submission_id == sid) {
                    j.inflight = false;
                }
            }
        }
        let mut todo = vec![];
        for j in &mut self.jobs {
            if (j.terminal() && j.events_done) || j.inflight || j.job_id.is_none() || j.last_poll.is_some_and(|t| now.saturating_duration_since(t) < JOB_POLL_EVERY) {
                continue;
            }
            j.last_poll = Some(now);
            j.inflight = true;
            todo.push((j.job_id.clone().unwrap(), j.cursor, j.terminal()));
        }
        for (jid, after, terminal) in todo {
            if !terminal { self.req(Op::Job, json!({"job_id": jid}), Pend::Job(jid.clone())); }
            self.req(Op::JobEvents, json!({"job_id": jid, "after": after, "limit": 50}), Pend::JobEvents(jid));
        }
    }

    // ---- receiver --------------------------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn receive(&mut self, now: Instant, env: &mut Env, creds: Option<&Path>, local: Option<(crate::scheduler::TargetStatus, bool)>, uuid: Option<String>, sample: Option<&Sample>) {
        // Preflight finished?
        if let Rx::Preflight { attempt, rx } = &self.rx {
            match rx.try_recv() {
                Ok(Ok(pf)) => {
                    let attempt = attempt.clone();
                    nlog(&format!("offer {attempt}: preflight passed"));
                    let plan = json!({"mode": "new", "gpu": uuid, "local_client": true, "temporal_window": pf.job.params.get("temporal_window"), "stages": pf.stages.iter().map(Stage::label).collect::<Vec<_>>(), "free_vram_mb": sample.and_then(|s| Self::free_vram(s, uuid.as_deref()))});
                    let free_mb = sample.and_then(|s| Self::free_vram(s, uuid.as_deref()));
                    let pinned = pf.job.params.get("temporal_window").and_then(Value::as_str).filter(|w| *w != "auto").map(str::to_owned);
                    match (pinned, free_mb) {
                        // Fixed window: the engine's own estimate against the memory this GPU has now, before anything is started.
                        (Some(w), Some(free)) if free > 0 => {
                            let args = json!({"first_frame": pf.first_frame.display().to_string(), "frames": pf.job.range.out_point - pf.job.range.in_point + 1, "temporal_window": w, "vram_profile": pf.job.params.get("vram_profile").and_then(Value::as_str).unwrap_or("auto"), "free_vram_mb": free});
                            if self.req(Op::Estimate, args, Pend::Estimate(attempt.clone())) {
                                self.rx = Rx::Estimating { attempt, pf: Box::new(pf), plan, free_mb: free };
                            } else {
                                self.rx = Rx::Idle;
                            }
                        }
                        _ => self.send_start(attempt, pf, plan, env),
                    }
                }
                Ok(Err(problems)) => {
                    let attempt = attempt.clone();
                    nlog(&format!("offer {attempt}: preflight refused: {}", problems.join("; ")));
                    let code = if problems.iter().any(|p| p.contains("no path mapping")) { "missing_map" } else { "other" };
                    self.receive_note = Some(format!("offer {attempt} refused: {}", truncate(&problems.join("; "), 300)));
                    self.req(Op::Reject, json!({"attempt_id": attempt, "code": code, "message": truncate(&problems.join("; "), 1000)}), Pend::Reject);
                    self.rx = Rx::Idle;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => {
                    let attempt = attempt.clone();
                    self.req(Op::Reject, json!({"attempt_id": attempt, "code": "other", "message": "the receiver preflight stopped unexpectedly"}), Pend::Reject);
                    self.rx = Rx::Idle;
                }
            }
        }
        if let Rx::Reconcile { attempt, meta, rx } = &self.rx {
            match rx.try_recv() {
                Ok(Recon::Terminal(state, message)) => self.resync(attempt.clone(), state, message),
                Ok(Recon::Complete) => self.resync(attempt.clone(), "succeeded", "complete (reconciled after a restart: manifest complete, no writer lock)".into()),
                Ok(Recon::Interrupted(m)) => self.resync(attempt.clone(), "failed", format!("interrupted by a restart of the app: {m}")),
                Ok(Recon::Fenced(m)) => {
                    let _ = meta;
                    self.rx = Rx::Owned(format!("attempt {attempt} is still owned by this installation: {m} Nothing new is claimed. Check this computer and the storage; the server operator resolves it with `abandon`."));
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.rx = Rx::Owned(format!("attempt {attempt} could not be inspected after the restart; it stays owned.")),
            }
        }
        if let Rx::Running(_) = &self.rx {
            self.run_attempt(now, env, creds);
            return;
        }
        // Claim only when opted in, registered, nothing is in progress, the verified local GPU is idle and the app is not closing.
        let idle = local.as_ref().is_some_and(|(st, busy)| *st == crate::scheduler::TargetStatus::Ready && !busy) && !env.ai.scheduler().running() && !env.ai.targets_pending();
        let ok = env.cfg.local_host && env.cfg.receive && env.cfg.may_claim && !self.closing && self.registered && matches!(self.rx, Rx::Idle) && self.held.is_empty() && uuid.is_some() && idle;
        if ok && !self.claim_inflight && self.last_claim.is_none_or(|t| now.saturating_duration_since(t) >= CLAIM_EVERY) {
            self.last_claim = Some(now);
            let maps: Vec<String> = env.settings.shared_roots.iter().map(|r| r.name.clone()).collect();
            let models: serde_json::Map<String, Value> = env
                .ai
                .scheduler()
                .targets()
                .into_iter()
                .find(|t| t.local && t.enabled)
                .and_then(|t| t.hello)
                .map(|h| h.models.iter().map(|(k, m)| (k.clone(), json!(m.sha256))).collect())
                .unwrap_or_default();
            let resources = sample.and_then(|s| Self::free_vram(s, uuid.as_deref()).zip(s.ram_total.zip(s.ram_used))).map(|(v, (t, u))| {
                json!({"sampled_at": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs()), "free_vram_mb": v, "free_ram_mb": t.saturating_sub(u) / (1 << 20)})
            });
            let claim = format!("clm_{}", crate::workflow::new_id());
            let args = json!({"claim_id": claim, "capabilities": {"protocol": 1, "kinds": ["key", "workflow"], "maps": maps, "models": models}, "resources": resources});
            nlog("claiming");
            self.claim_inflight = self.req(Op::Claim, args, Pend::Claim);
        }
    }

    fn free_vram(s: &Sample, uuid: Option<&str>) -> Option<u64> {
        let g = s.gpus.as_ref()?.iter().find(|g| Some(g.uuid.as_str()) == uuid)?;
        Some(g.vram_total?.saturating_sub(g.vram_used?) / (1 << 20))
    }

    fn on_claim(&mut self, v: Value, env: &mut Env) {
        let Some(offer) = v.get("offer").filter(|o| o.is_object()) else {
            self.receive_note = None;
            return;
        };
        let attempt = offer.get("attempt_id").and_then(Value::as_str).unwrap_or("").to_owned();
        if offer.get("state").and_then(Value::as_str) != Some("offered") || attempt.is_empty() || !matches!(self.rx, Rx::Idle) || self.closing {
            return; // a claim retry that shows `revoked` (or anything but offered): never run
        }
        let spec: Result<JobSpec, _> = serde_json::from_value(offer.get("spec").cloned().unwrap_or_default());
        let spec = match spec {
            Ok(s) => s,
            Err(e) => {
                self.req(Op::Reject, json!({"attempt_id": attempt, "code": "incompatible_protocol", "message": truncate(&format!("the job spec is not understood by this build: {e}"), 500)}), Pend::Reject);
                return;
            }
        };
        // The GPU slot is reserved BEFORE anything else is acknowledged, so a local Queue cannot race the start.
        if !env.ai.reserve_network_slot(&self.slot(&attempt)) {
            self.req(Op::Reject, json!({"attempt_id": attempt, "code": "busy", "message": "this computer's GPU has local work"}), Pend::Reject);
            self.receive_note = Some(format!("offer {attempt} declined: local work holds the GPU"));
            return;
        }
        self.held = self.slot(&attempt);
        let roots: Vec<SharedRoot> = env.settings.shared_roots.clone();
        let (tx, rx) = channel();
        let (a2, wake) = (attempt.clone(), env.wake.clone());
        nlog(&format!("offer {attempt}: preflight"));
        let started = std::thread::Builder::new().name("net-preflight".into()).spawn(move || {
            let _ = tx.send(spec.preflight(&roots, &a2));
            wake();
        });
        if started.is_ok() {
            self.receive_note = Some(format!("checking offer {attempt} against this computer's shared storage"));
            self.rx = Rx::Preflight { attempt, rx };
        }
    }

    /// Ownership is written BEFORE the start is requested, then the coordinator is asked to start the attempt.
    fn send_start(&mut self, attempt: String, pf: Preflight, plan: Value, env: &mut Env) {
        let stages: Vec<(String, &'static str)> = pf.stages.iter().map(|s| (s.job_id().to_owned(), s.label())).collect();
        let saved = match env.cfg.creds.clone() {
            Some(c) => write_private(&side_file(&c, ".attempt.json"), &attempt_file(&attempt, &pf.job.job_id, 0, &stages, &attempt_meta(&pf))),
            None => Err("no credentials folder to keep ownership in".into()),
        };
        if let Err(e) = saved {
            self.receive_note = Some(format!("offer {attempt} not started: ownership could not be saved ({e})"));
            self.req(Op::Reject, json!({"attempt_id": attempt, "code": "other", "message": truncate(&format!("receiver cannot record ownership: {e}"), 500)}), Pend::Reject);
            self.rx = Rx::Idle;
        } else if self.req(Op::Start, json!({"attempt_id": attempt, "effective_plan": plan}), Pend::Start(attempt.clone())) {
            self.rx = Rx::Starting { attempt, pf: Box::new(pf) };
        } else {
            Self::forget_file(env, true);
            self.rx = Rx::Idle;
        }
    }

    /// The engine's verdict on a pinned temporal window. Over budget: the offer is declined with the concrete figures
    /// (this receiver is then excluded for this job and nothing was started). No answer: the worker still enforces it.
    fn on_estimate(&mut self, attempt: String, r: Result<Value, LinkError>, env: &mut Env) {
        let Rx::Estimating { attempt: a, pf, plan, free_mb } = std::mem::replace(&mut self.rx, Rx::Idle) else { return };
        if a != attempt {
            return;
        }
        match r {
            Ok(v) if v.get("over_budget").and_then(Value::as_bool) == Some(true) => {
                let g = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                let w = pf.job.params.get("temporal_window").and_then(Value::as_str).unwrap_or("?");
                let msg = format!("pinned temporal window {w} ({} VRAM) needs ~{:.1} GiB (peak {:.1} GiB) but this GPU has ~{:.1} GiB free; a fixed window is never downgraded", if v.get("low_vram").and_then(Value::as_bool) == Some(true) { "low" } else { "normal" }, g("required_gib"), g("peak_gib"), g("budget_gib"));
                nlog(&format!("offer {attempt}: declined: {msg}"));
                self.receive_note = Some(format!("offer {attempt} declined: {msg}"));
                self.req(Op::Reject, json!({"attempt_id": attempt, "code": "insufficient_resources", "message": truncate(&msg, 900)}), Pend::Reject);
            }
            Ok(v) => {
                let mut plan = plan;
                plan["estimate"] = json!({"required_gib": v.get("required_gib"), "budget_gib": v.get("budget_gib"), "free_vram_mb": free_mb});
                self.send_start(attempt, *pf, plan, env);
            }
            Err(e) => {
                nlog(&format!("offer {attempt}: estimate unavailable ({}); the worker enforces the window at launch", err_text(&e)));
                self.send_start(attempt, *pf, plan, env);
            }
        }
    }

    fn on_start(&mut self, attempt: String, r: Result<Value, LinkError>, env: &mut Env) {
        let Rx::Starting { attempt: a, pf } = std::mem::replace(&mut self.rx, Rx::Idle) else { return };
        if a != attempt {
            return;
        }
        let v = match r {
            Ok(v) if v.get("state").and_then(Value::as_str) == Some("started") => v,
            Ok(v) => {
                self.receive_note = Some(format!("offer {attempt} was not started by the coordinator ({}); not run", v.get("state").and_then(Value::as_str).unwrap_or("?")));
                Self::forget_file(env, true);
                return;
            }
            Err(e) => {
                self.receive_note = Some(format!("offer {attempt} could not be started ({})", err_text(&e)));
                // A coordinator answer that says the attempt cannot be started settles it; a transport failure may not.
                Self::forget_file(env, e.code.starts_with("attempt_") || e.code == "unknown_attempt");
                return;
            }
        };
        let stages: Vec<(String, &'static str)> = pf.stages.iter().map(|s| (s.job_id().to_owned(), s.label())).collect();
        let job_id = pf.job.job_id.clone();
        // Ownership is durable BEFORE anything runs. If it cannot be written the attempt stays fenced (the coordinator
        // already considers it started): nothing is launched and the operator resolves it.
        let meta = attempt_meta(&pf);
        let persisted = match env.cfg.creds.clone() {
            Some(c) => write_private(&side_file(&c, ".attempt.json"), &attempt_file(&attempt, &job_id, 0, &stages, &meta)),
            None => Err("no credentials folder to keep ownership in".into()),
        };
        if let Err(e) = persisted {
            nlog(&format!("attempt {attempt}: ownership not persisted: {e}"));
            self.rx = Rx::Owned(format!("attempt {attempt} was started by the coordinator but this computer could not record it ({e}). Nothing was launched and the attempt stays held. Fix the storage, then ask the server operator to resolve it with `abandon`."));
            self.receive_note = Some(format!("not launched: ownership could not be saved ({e})"));
            return;
        }
        let mut rv = Recv { last_phase: String::new(), attempt: attempt.clone(), job_id: job_id.clone(), meta: meta.clone(), stages: stages.clone(), announced: 0, outbox: vec![], seq: 0, last_done: 0, last_total: 0, last_report: Instant::now(), last_progress: Instant::now(), inflight: false, retry_at: None, cancel_sent: false, terminal: false, report_saved: false };
        nlog(&format!("attempt {attempt} started; running {} stage(s) for {job_id} on the local client", stages.len()));
        match env.ai.submit_network_plan(&self.slot(&attempt), pf.stages) {
            Ok(()) => {
                self.receive_note = Some(format!("running network job {job_id}"));
                if v.get("cancel_requested").and_then(Value::as_bool) == Some(true) {
                    Self::cancel_all(&mut rv, env);
                }
            }
            Err(e) => Self::push_terminal(&mut rv, "failed", &format!("this computer could not start it: {e}")),
        }
        self.rx = Rx::Running(rv);
    }

    fn cancel_all(r: &mut Recv, env: &mut Env) {
        r.cancel_sent = true;
        for (id, _) in &r.stages {
            env.ai.cancel(id);
        }
    }

    /// Ask the coordinator where the attempt's event stream stands, then report the reconciled end.
    fn resync(&mut self, attempt: String, state: &'static str, message: String) {
        if self.req(Op::Events, json!({"attempt_id": attempt, "events": []}), Pend::Resync) {
            self.rx = Rx::Resync { attempt, state, message };
        } else {
            self.rx = Rx::Owned(format!("attempt {attempt}: the coordinator could not be asked to close it; it stays owned."));
        }
    }

    fn on_resync(&mut self, r: Result<Value, LinkError>, env: &mut Env) {
        let Rx::Resync { attempt, state, message } = std::mem::replace(&mut self.rx, Rx::Idle) else { return };
        match r {
            Ok(v) => {
                let mut rv = Recv { last_phase: String::new(), attempt: attempt.clone(), job_id: attempt.clone(), meta: Value::Null, stages: vec![], announced: 0, outbox: vec![], seq: v.get("last_seq").and_then(Value::as_u64).unwrap_or(0), last_done: 0, last_total: 0, last_report: Instant::now(), last_progress: Instant::now(), inflight: false, retry_at: None, cancel_sent: false, terminal: false, report_saved: false };
                Self::push_terminal(&mut rv, state, &message);
                self.receive_note = Some(format!("attempt {attempt} reconciled after a restart: {state}"));
                self.rx = Rx::Running(rv);
            }
            Err(e) if e.code == "attempt_terminal" || e.code == "unknown_attempt" || e.code == "attempt_not_started" => {
                if let Some(c) = env.cfg.creds.clone() {
                    let _ = std::fs::remove_file(side_file(&c, ".attempt.json"));
                }
                self.receive_note = Some(format!("attempt {attempt} was already resolved by the coordinator operator ({}); local ownership cleared", e.code));
            }
            Err(e) => self.rx = Rx::Owned(format!("attempt {attempt}: the coordinator could not be reached to close it ({}); it stays owned.", err_text(&e))),
        }
    }

    fn push_event(r: &mut Recv, t: &str, data: Value) {
        r.seq += 1;
        r.outbox.push(json!({"seq": r.seq, "type": t, "data": data}));
    }
    fn push_terminal(r: &mut Recv, state: &str, msg: &str) {
        if r.terminal {
            return;
        }
        r.terminal = true;
        if let Some(meta) = r.meta.as_object_mut() {
            meta.insert("terminal".into(), json!({"state": state, "message": truncate(msg, 900)}));
        }
        Self::push_event(r, "terminal", json!({"state": state, "message": truncate(msg, 900)}));
    }

    fn lost_verdict(label: &str, reason: &str, writer_stopped: bool) -> Option<(&'static str, String)> {
        writer_stopped.then(|| ("failed", format!("{label} stage was lost: {reason}")))
    }

    fn run_attempt(&mut self, now: Instant, env: &mut Env, creds: Option<&Path>) {
        let Rx::Running(r) = &mut self.rx else { return };
        if !r.terminal {
            // The first stage that is not Complete decides (a failed one fails everything after it).
            let mut verdict = None;
            let mut active = r.stages.len();
            let mut progress = None;
            for (i, (id, label)) in r.stages.iter().enumerate() {
                match env.ai.job_state(id) {
                    Some(JobState::Done { info, .. }) => match info.outcome {
                        Outcome::Complete => continue,
                        Outcome::Cancelled => verdict = Some(("cancelled", format!("{label} stage cancelled"))),
                        Outcome::Failed => verdict = Some(("failed", format!("{label} stage failed: {}", info.message.as_deref().unwrap_or("the render failed")))),
                    },
                    Some(JobState::Rejected { message, .. }) => verdict = Some(("failed", format!("{label} stage refused by the local client: {message}"))),
                    Some(JobState::Failed { reason }) => verdict = Some(("failed", format!("{label} stage: {reason}"))),
                    Some(JobState::Cancelled) => verdict = Some(("cancelled", format!("{label} stage cancelled"))),
                    Some(JobState::Lost { reason, writer_stopped, .. }) => {
                        // A lost stage is terminal only after the scheduler proves its writer has stopped.
                        self.receive_note = Some(format!("the {label} stage was lost ({reason}); writer {}. Inspect this computer and the storage; the coordinator keeps ownership.", if writer_stopped { "stopped" } else { "NOT proven stopped" }));
                        if let Some((state, msg)) = Self::lost_verdict(label, &reason, writer_stopped) {
                            verdict = Some((state, msg));
                        } else {
                            active = i;
                        }
                        break;
                    }
                    Some(JobState::Running { done, total, phase, .. }) => {
                        active = i;
                        progress = Some((done, total.max(done), phase));
                        break;
                    }
                    _ => {
                        active = i;
                        break;
                    }
                }
                break;
            }
            if let Some((state, msg)) = verdict {
                Self::push_terminal(r, state, &msg);
            } else if active == r.stages.len() {
                Self::push_terminal(r, "succeeded", "complete");
            } else if active + 1 > r.announced {
                r.announced = active + 1;
                let msg = format!("stage {} of {}: {}", active + 1, r.stages.len(), r.stages[active].1);
                Self::push_event(r, "note", json!({"message": msg}));
                r.last_done = u64::MAX;
                r.last_total = 0;
                Self::push_event(r, "progress", json!({"done": 0, "total": 0}));
            }
            if let Some((done, total, phase)) = progress {
                if (done != r.last_done || total != r.last_total) && (r.last_done == u64::MAX || now.saturating_duration_since(r.last_progress) >= PROGRESS_EVERY) {
                    r.last_done = done;
                    r.last_total = total;
                    r.last_progress = now;
                    if phase != r.last_phase && !phase.is_empty() {
                        Self::push_event(r, "note", json!({"message": format!("pass: {phase}")}));
                        r.last_phase = phase;
                    }
                    Self::push_event(r, "progress", json!({"done": done, "total": total}));
                }
            }
        }
        let Rx::Running(r) = &mut self.rx else { return };
        if (!r.outbox.is_empty() || (!r.terminal && now.saturating_duration_since(r.last_report) >= CLAIM_EVERY)) && !r.inflight && r.retry_at.is_none_or(|t| now >= t) {
            let evs: Vec<Value> = r.outbox.iter().take(256).cloned().collect();
            let (attempt, last, job_id) = (r.attempt.clone(), r.seq, r.job_id.clone());
            let (stages, meta) = (r.stages.clone(), r.meta.clone());
            r.inflight = true;
            r.last_report = now;
            let sent = self.phase == Phase::Online && self.link.as_mut().is_some_and(|l| match l.request(Op::Events, json!({"attempt_id": attempt, "events": evs})) {
                Ok(id) => {
                    self.pend.insert(id, Pend::Events);
                    true
                }
                Err(_) => false,
            });
            if let Rx::Running(r) = &mut self.rx {
                if !sent {
                    r.inflight = false;
                    r.retry_at = Some(now + Duration::from_secs(5));
                }
            }
            if let Some(c) = creds {
                match write_private(&side_file(c, ".attempt.json"), &attempt_file(&attempt, &job_id, last, &stages, &meta)) {
                    Ok(()) => {
                        if let Rx::Running(r) = &mut self.rx {
                            r.report_saved = r.terminal && meta.get("terminal").is_some();
                        }
                    }
                    Err(e) => self.receive_note = Some(format!("could not update the saved attempt state ({e}); the attempt stays held")),
                }
            }
        }
    }

    fn on_events(&mut self, r: Result<Value, LinkError>, env: &mut Env) {
        let Rx::Running(rv) = &mut self.rx else { return };
        rv.inflight = false;
        match r {
            Ok(v) => {
                let last = v.get("last_seq").and_then(Value::as_u64).unwrap_or(0);
                rv.outbox.retain(|e| e["seq"].as_u64().unwrap_or(0) > last);
                if v.get("cancel_requested").and_then(Value::as_bool) == Some(true) && !rv.cancel_sent {
                    Self::cancel_all(rv, env);
                }
                if rv.terminal && rv.outbox.is_empty() {
                    let job_id = rv.job_id.clone();
                    if let Some(c) = env.cfg.creds.clone() {
                        let _ = std::fs::remove_file(side_file(&c, ".attempt.json"));
                    }
                    self.receive_note = Some(format!("network job {job_id} finished and reported"));
                    if let Some(f) = Self::folder_of(env.ai, &job_id) {
                        self.finished.push(f);
                    }
                    self.rx = Rx::Idle; // the next tick releases the GPU slot
                }
            }
            Err(e) => {
                rv.retry_at = Some(Instant::now() + Duration::from_secs(5));
                self.receive_note = Some(format!("could not report progress ({}); retrying", err_text(&e)));
            }
        }
    }

    fn folder_of(ai: &AiUi, job_id: &str) -> Option<String> {
        ai.scheduler().snapshot().iter().find(|e| e.job.job_id == job_id).and_then(|e| e.job.clip_root.file_name().map(|n| n.to_string_lossy().into_owned()))
    }

    // ---- durable state ---------------------------------------------------------------------------------------------

    fn load_state(&mut self, env: &Env) {
        let Some(c) = env.cfg.creds.clone() else { return };
        let idle = matches!(self.rx, Rx::Idle);
        if let Some(v) = std::fs::read(side_file(&c, ".attempt.json")).ok().filter(|_| idle).and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
            let attempt = v.get("attempt_id").and_then(Value::as_str).unwrap_or("?").to_owned();
            self.held = self.slot(&attempt);
            let meta = v.get("meta").cloned().unwrap_or(Value::Null);
            let owned = |why: &str| Rx::Owned(format!("attempt {attempt} (job {}) was started by an earlier run of this app and is still owned by this installation: {why} Nothing new is claimed. Check this computer and the storage; the server operator resolves it with `abandon`.", v.get("job_id").and_then(Value::as_str).unwrap_or("?")));
            self.rx = if meta.get("manifest_dir").is_some() {
                let (tx, rx) = channel();
                let (m2, wake) = (meta.clone(), self.wake_hint.clone());
                let spawned = std::thread::Builder::new().name("net-reconcile".into()).spawn(move || {
                    let _ = tx.send(reconcile(&m2));
                    if let Some(w) = wake {
                        w();
                    }
                });
                if spawned.is_ok() { Rx::Reconcile { attempt: attempt.clone(), meta, rx } } else { owned("its storage could not be inspected.") }
            } else {
                owned("it predates restart reconciliation.")
            };
        }
    }

    /// Read the saved list of sent jobs once (before anything is added to it, so a save can never drop them).
    fn load_jobs(&mut self, creds: Option<&Path>) {
        let Some(c) = creds else { return };
        if std::mem::replace(&mut self.jobs_loaded, true) {
            return;
        }
        if let Some(n) = std::fs::read(side_file(c, ".network.json")).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).and_then(|v| v["network_id"].as_str().map(str::to_owned)) {
            self.cached_ns.get_or_insert(n);
        }
        let owner = c.to_string_lossy().into_owned();
        let hp = crate::queue::history_path();
        if let Some(hp) = &hp {
            // One-time import of the old per-connection file; renamed, never deleted.
            let old = side_file(c, ".jobs.json");
            if let Some(a) = std::fs::read(&old).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) {
                let es: Vec<_> = a.as_array().into_iter().flatten().cloned().map(|j| sent_entry(&owner, j)).collect();
                if let Err(e) = crate::queue::merge_old(hp, es).and_then(|()| crate::queue::rename_free(&old, &side_file(c, ".jobs.json.imported")).map_err(|e| e.to_string())) {
                    self.status = Some(format!("Could not import the old list of sent jobs ({e})."));
                }
            }
        }
        let (saved, err) = hp.map(|p| crate::queue::read(&p)).unwrap_or_default();
        if err.is_some() {
            self.status = err;
        }
        let saved: Vec<Value> = saved.into_iter().filter(|e| e.kind == "sent" && e.owner == owner).filter_map(|e| e.job).collect();
        {
            {
                for j in &saved {
                    self.jobs.push(NetJob {
                        submission_id: j["submission_id"].as_str().unwrap_or("").into(),
                        submission: j.get("submission").filter(|v| v.is_object()).cloned(),
                        installation: j.get("installation").and_then(Value::as_str).map(str::to_owned),
                        stages: j.get("stages").cloned().and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default(),
                        active_stage: j.get("active_stage").and_then(Value::as_u64).map(|n| n as usize),
                        cancel_requested: j.get("cancel_requested").and_then(Value::as_bool).unwrap_or(false),
                        events_done: false,
                        job_id: j["job_id"].as_str().map(str::to_owned),
                        folder: j["folder"].as_str().unwrap_or("").into(),
                        name: j["name"].as_str().unwrap_or("").into(),
                        state: j["state"].as_str().unwrap_or("queued").into(),
                        on: j.get("on").and_then(Value::as_str).map(str::to_owned),
                        done: j.get("done").and_then(Value::as_u64).unwrap_or(0),
                        total: j.get("total").and_then(Value::as_u64).unwrap_or(0),
                        reason: j.get("reason").and_then(Value::as_str).map(str::to_owned),
                        stage: None,
                        cursor: j.get("cursor").and_then(Value::as_u64).unwrap_or(0),
                        refreshed: false,
                        last_poll: None,
                        submission_attempted: j.get("submission_attempted").and_then(Value::as_bool).unwrap_or_else(|| j.get("submission").is_some_and(|v| v.is_object())),
                        inflight: false,
                    });
                }
            }
        }
    }

    /// "Clear finished": drop finished jobs from the queue and the saved list; running ones stay.
    pub fn clear_finished(&mut self) {
        if !self.jobs_loaded {
            return; // the saved list is not read yet: a write now would drop it
        }
        self.jobs.retain(|j| !j.terminal());
        self.write_jobs(&PathBuf::from(&self.key));
    }

    fn save_jobs(&mut self, env: &Env) {
        if let Some(c) = env.cfg.creds.clone() {
            self.write_jobs(&c);
        }
    }

    fn write_jobs(&mut self, c: &Path) -> bool {
        let Some(hp) = crate::queue::history_path() else {
            self.status = Some("Could not save the list of sent jobs (no settings folder); they cannot be followed after a restart.".into());
            return false;
        };
        let owner = c.to_string_lossy().into_owned();
        let add = self.jobs.iter().map(|j| sent_entry(&owner, json!({"submission_id": j.submission_id, "submission": j.submission, "submission_attempted": j.submission_attempted, "installation": j.installation, "stages": j.stages, "cursor": j.cursor, "active_stage": j.active_stage, "cancel_requested": j.cancel_requested, "on": j.on, "done": j.done, "total": j.total, "reason": j.reason, "job_id": j.job_id, "folder": j.folder, "name": j.name, "state": j.state}))).collect();
        match crate::queue::replace(&hp, |e| e.kind == "sent" && e.owner == owner, add) {
            Ok(()) => true,
            Err(e) => {
                self.status = Some(format!("Could not save the list of sent jobs ({e}); they cannot be followed after a restart."));
                false
            }
        }
    }

    // ---- UI --------------------------------------------------------------------------------------------------------

    pub fn status_line(&self, cfg: &Conn) -> (String, bool) {
        match &self.phase {
            Phase::Online if !cfg.local_host => ("Connected".into(), true),
            Phase::Online if self.registered => (format!("Connected{}", if cfg.receive { ", taking network work when the GPU is idle" } else { "" }), true),
            Phase::Online => (format!("Connected; {}", self.register_note.clone().unwrap_or_else(|| "registering this computer…".into())), true),
            Phase::Off if cfg.enabled => ("Not connected".into(), false),
            Phase::Off => ("Network client is off".into(), false),
            Phase::Starting | Phase::Connecting => ("Connecting…".into(), false),
            Phase::Failed(w) => (format!("Not connected: {w}"), false),
        }
    }

    /// The installations this connection offers, minus this computer's own (`own`: every network's own id and this PC's service id).
    pub fn other_installations(&self, own: &[String]) -> Vec<Install> {
        self.installs.iter().filter(|i| !own.contains(&i.id) && Some(&i.id) != self.me.as_ref() && self.host_only(&i.id)).cloned().map(|mut i| {
            i.connected &= self.online();
            i
        }).collect()
    }

    pub fn has_queue(&self) -> bool { !self.jobs.is_empty() }

    /// Any job of ours on this PC that has not finished.
    pub(crate) fn has_unfinished(&self, iid: &str) -> bool {
        self.jobs.iter().any(|j| j.installation.as_deref() == Some(iid) && !j.terminal())
    }

    /// Free for Automatic: none of our jobs queued or running on it, and its last reported load is under 50%.
    /// The PC must have answered just now: the sample's age counts from its measurement to this moment, so a list kept
    /// from before a dropped link (or a link that connects but never answers) goes stale and is not free.
    pub(crate) fn is_free(&self, iid: &str) -> bool {
        !self.leaving && self.online() && !self.has_unfinished(iid)
            && self.installs.iter().find(|i| i.id == iid).is_some_and(|i| {
                i.connected && matches!(Sample::from_api(&i.stats), Ok(Some((s, Some(age))))
                    if age + i.received.elapsed().as_secs_f64() <= crate::metrics::REMOTE_STALE_S && lightly_loaded(&s))
            })
    }

    fn telemetry(&self, id: &str) -> ClientTelemetry {
        let Some(i) = self.installs.iter().find(|i| i.id == id) else { return ClientTelemetry::Unreported("Waiting for PC statistics".into()) };
        if !i.connected || !self.online() { return ClientTelemetry::NotConnected; }
        match Sample::from_api(&i.stats) {
            Ok(Some((s, age))) => ClientTelemetry::reported_at(s, age, i.stats.get("received_age_s").and_then(Value::as_f64), i.received),
            Ok(None) => ClientTelemetry::Unreported("Waiting for statistics".into()),
            Err(e) => ClientTelemetry::Unreported(e),
        }
    }

    /// Submitted work belongs in Batch, grouped by the PC that owns it.
    pub fn queue_ui(&mut self, ui: &mut Ui, net_label: Option<&str>) -> Vec<Action> {
        let mut actions = vec![];
        let mut groups: Vec<Option<String>> = vec![];
        for j in &self.jobs {
            if !groups.contains(&j.installation) { groups.push(j.installation.clone()); }
        }
        // Only the PC this connection's Clients row stands for; finished jobs of other PCs on its network stay hidden.
        let listed = |id: &str| if self.local_host { Some(id) == self.me.as_deref() } else { self.target_host.is_empty() || id == self.target_host };
        for group in groups {
            if !self.jobs.iter().any(|j| j.installation == group && !j.terminal()) && !group.as_deref().is_some_and(listed) {
                continue;
            }
            let pc = group.as_deref().and_then(|id| self.installs.iter().find(|i| i.id == id));
            let name = pc.map(|i| i.name.as_str()).unwrap_or_else(|| group.as_deref().unwrap_or("Waiting for a PC"));
            let connected = pc.is_some_and(|i| i.connected) && self.online();
            let jobs: Vec<&NetJob> = self.jobs.iter().filter(|j| j.installation == group).collect();
            let current: Vec<&NetJob> = jobs.iter().copied().filter(|j| !j.terminal()).collect();
            let earlier: Vec<&NetJob> = jobs.iter().copied().filter(|j| !current.iter().any(|c| std::ptr::eq(*c, *j))).collect();
            let id = ui.make_persistent_id(("netnode", &self.key, &group));
            let st = egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), id, true);
            let open = st.is_open();
            egui::Frame::group(ui.style()).inner_margin(10.0).show(ui, |ui| {
                ui.set_width(ui.available_width());
                st.show_header(ui, |ui| {
                    let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.0, if connected { crate::ui::theme::ACCENT } else { DIM });
                    // Only the LAST widget may truncate (it takes the rest of the line); one before others pushes
                    // them past the edge and the resizable panel widens every frame.
                    ui.label(RichText::new(name).strong());
                    if let Some(l) = net_label { ui.label(RichText::new(l).color(DIM).size(11.0)); }
                    if !connected { ui.label(RichText::new("Offline").color(DIM).size(11.0)); }
                    if let (false, Some(c)) = (open, current.first()) {
                        ui.add(egui::Label::new(RichText::new(format!("{} · {}", c.name, net_overall(&c.stages, &c.state))).color(DIM).size(11.0)).truncate());
                    }
                }).body(|ui| {
                if let Some(id) = group.as_deref() {
                    crate::ui::metrics_bar::machine_meters(ui, &self.telemetry(id), &[]);
                }
                for j in current {
                    ui.add_space(8.0);
                    // Right to left: the buttons and state first, the name truncates into what is left (never wider).
                    // One line high: an unbounded right-to-left layout takes all the remaining height.
                    ui.allocate_ui_with_layout(egui::vec2(ui.available_width(), 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let (Some(id), false) = (&j.job_id, j.terminal()) {
                            if ui.add_enabled(!j.cancel_requested, egui::Button::new(if j.cancel_requested { "Stopping" } else { "Cancel" }).small()).clicked() { actions.push(Action::CancelJob { creds: self.key.clone(), job_id: id.clone() }); }
                        } else if j.can_take_unsent() && ui.small_button("Send again").on_hover_text("This PC has not accepted it yet: take it back and give it to the next free PC (end of the Automatic list).").clicked() {
                            actions.push(Action::Resend { creds: self.key.clone(), submission_id: j.submission_id.clone() });
                        } else if !j.terminal() {
                            ui.label("Reconciling submission");
                        }
                        ui.colored_label(if j.state == "failed" { egui::Color32::from_rgb(225, 110, 110) } else { DIM }, RichText::new(&j.state).size(11.0));
                        ui.add(egui::Label::new(RichText::new(&j.name).strong().size(12.0)).truncate()).on_hover_text(&j.name);
                    });
                    if j.stages.is_empty() { ui.label(RichText::new(&j.state).color(DIM)); }
                    for (index, stage) in j.stages.iter().enumerate() {
                        let current = j.active_stage == Some(index);
                        let failed = current && matches!(j.state.as_str(), "failed" | "cancelled");
                        let (frac, text) = if stage.completed || j.state == "succeeded" {
                            (1.0, "Complete".to_string())
                        } else if failed { (0.0, j.state.clone())
                        } else if current && stage.total > 0 {
                            let f = (stage.done as f32 / stage.total as f32).min(0.99);
                            let pass = stage.phase.as_deref().map(|p| format!("{} · ", crate::ui::ai::pass_name(p))).unwrap_or_default();
                            (f, format!("{pass}{}% · {}/{}", (f * 100.0).floor() as u32, stage.done, stage.total))
                        } else if current && !j.terminal() { (0.0, "Running".into())
                        } else if j.terminal() { (0.0, "Not run".into())
                        } else if index == 0 { (0.0, j.state.clone())
                        } else { (0.0, "Waiting".into()) };
                        ui.horizontal(|ui| {
                            ui.add_sized([125.0, 18.0], egui::Label::new(RichText::new(&stage.label).size(11.5)).truncate()).on_hover_text(&stage.label);
                            ui.add(egui::ProgressBar::new(frac).desired_width(ui.available_width()).desired_height(17.0)
                                .fill(if failed { egui::Color32::from_rgb(190, 95, 85) } else { crate::ui::theme::ACCENT })
                                .text(RichText::new(text).size(11.0)));
                        });
                    }
                    if let Some(reason) = &j.reason {
                        if reason != "complete" { ui.add(egui::Label::new(RichText::new(reason).color(DIM).size(11.0)).wrap()); }
                    }
                }
                if !earlier.is_empty() {
                    egui::CollapsingHeader::new(RichText::new(format!("Earlier ({})", earlier.len())).color(DIM).size(11.0))
                        .id_salt(("netlog", &self.key, &group)).default_open(false).show(ui, |ui| {
                        for j in earlier.iter().rev().take(12) {
                            let why = j.reason.as_deref().filter(|r| *r != "complete").unwrap_or(j.state.as_str());
                            ui.label(RichText::new(format!("{} · {}", j.name, why)).color(DIM).size(11.0));
                        }
                    });
                }
                });
            });
            ui.add_space(8.0);
        }
        actions
    }

    /// The connection's controls, one grid cell each: ID, Connect, Accept work, Default for Batch, Remove.
    pub fn controls(&self, ui: &mut Ui, cfg: &Conn, is_default: bool, blocker: Option<String>) -> Vec<Action> {
        let mut acts = vec![];
        let creds = self.key.clone();
        ui.label(RichText::new(self.ns().unwrap_or_default()).color(DIM).size(11.5));
        let mut en = cfg.enabled;
        if ui.add_enabled_ui(!self.leaving, |ui| crate::ui::theme::checkbox(ui, &mut en, "")).inner.changed() {
            acts.push(Action::Enable { creds: creds.clone(), on: en });
        }
        let mut rc = cfg.receive;
        let accept = ui.add_enabled_ui(!self.leaving, |ui| crate::ui::theme::checkbox(ui, &mut rc, "")).inner.on_hover_text(if cfg.local_host { "This PC takes jobs that other PCs send to it" } else { "Automatic may send shots to this PC" });
        if accept.changed() {
            acts.push(Action::Receive { creds: creds.clone(), on: rc });
        }
        let mut def = is_default;
        if ui.add_enabled_ui(!self.leaving, |ui| crate::ui::theme::checkbox(ui, &mut def, "")).inner.changed() && def {
            acts.push(Action::DefaultSubmit(creds.clone()));
        }
        if !cfg.local_host {
            let remove = ui.add_enabled(blocker.is_none() && !self.leaving, egui::Button::new(if self.leaving { "Removing…" } else { "Remove" }));
            let remove = match &blocker { Some(b) => remove.on_disabled_hover_text(format!("Cannot remove now: {b}")), None => remove };
            if remove.clicked() {
                acts.push(Action::Remove(creds));
            }
        }
        acts
    }

    /// The Status cell's text: the connection's status line (hover: its notes) and, when an attempt is owned here, its reason and Forget.
    pub fn status_cell(&self, ui: &mut Ui, cfg: &Conn) -> Vec<Action> {
        let mut acts = vec![];
        let (line, up) = self.status_line(cfg);
        let hover: Vec<&str> = [self.status.as_deref(), self.name_note.as_deref()].into_iter().flatten().collect();
        let status = ui.colored_label(if up { egui::Color32::from_rgb(150, 190, 150) } else { egui::Color32::from_rgb(224, 176, 80) }, line);
        if !hover.is_empty() {
            status.on_hover_text(hover.join("\n"));
        }
        if let Rx::Owned(m) = &self.rx {
            ui.colored_label(egui::Color32::from_rgb(225, 110, 110), m);
            if ui.small_button("Forget attempt").on_hover_text("I checked this computer: forget this attempt locally").clicked() {
                acts.push(Action::ForgetAttempt(self.key.clone()));
            }
        }
        acts
    }

    /// One connection as a table row: Client, Kind, Address, Status, GPU, then `controls`. The caller ends the row.
    pub fn row(&self, ui: &mut Ui, cfg: &Conn, is_default: bool, blocker: Option<String>, own: &[String]) -> Vec<Action> {
        let mut acts = vec![];
        let creds = self.key.clone();
        let installs = self.other_installations(own);
        let names: Vec<String> = installs.iter().map(|i| i.name.clone()).collect();
        ui.label(if names.is_empty() { self.label() } else { names.join(", ") });
        let url = self.url().unwrap_or_default();
        let shown = if cfg.preferred.is_empty() { url.as_str() } else { cfg.preferred.as_str() };
        ui.label(ep_kind(shown));
        let mut cur = cfg.preferred.clone();
        let mut opts = cfg.endpoints.clone();
        if !url.is_empty() && !opts.contains(&url) {
            opts.push(url.clone());
        }
        let label = |u: &str| format!("{} ({})", host_of(u), ep_kind(u));
        let text = if cur.is_empty() { "Automatic".to_owned() } else { label(&cur) };
        egui::ComboBox::from_id_salt(("conn_ep", &self.key)).selected_text(text).show_ui(ui, |ui| {
            ui.selectable_value(&mut cur, String::new(), "Automatic");
            for u in &opts {
                ui.selectable_value(&mut cur, u.clone(), label(u));
            }
        });
        if cur != cfg.preferred {
            acts.push(Action::Prefer { creds: creds.clone(), url: cur });
        }
        ui.vertical(|ui| acts.extend(self.status_cell(ui, cfg)));
        let gpus: Vec<String> = installs.iter().filter_map(|i| i.gpu.clone()).collect();
        ui.label(if gpus.is_empty() { "Not reported".to_owned() } else { gpus.join(", ") });
        acts.extend(self.controls(ui, cfg, is_default, blocker));
        acts
    }
}

#[cfg(test)]
mod discovery_tests {
    use super::*;

    #[test]
    fn own_queue_does_not_offer_submitters_as_render_targets() {
        let mut r = Remote::blank(Source::Off, String::new());
        r.me = Some("this-pc".into());
        r.local_host = true;
        assert!(!r.host_only("submitter"));
        assert!(!r.host_only("this-pc"));
        r.local_host = false;
        r.target_host = "selected-host".into();
        assert!(r.host_only("selected-host"));
        assert!(!r.host_only("submitter"));
        assert!(!r.host_only("this-pc"));
    }

    #[test]
    fn net_overall_is_capped_and_words() {
        let st = |done, total, completed| NetStage { label: String::new(), done, total, completed, phase: None };
        assert_eq!(net_overall(&[st(0, 2, false)], "queued"), "queued");
        assert_eq!(net_overall(&[st(0, 0, true), st(1, 2, false)], "running"), "75%");
        assert_eq!(net_overall(&[st(0, 0, false)], "succeeded"), "100%");
        assert_eq!(net_overall(&[st(1, 2, false)], "failed"), "failed");
    }

    #[test]
    fn own_ids_hide_this_pc() {
        let mut r = Remote::blank(Source::Off, String::new());
        r.me = Some("X1".into());
        let inst = |id: &str| Install { id: id.into(), name: id.into(), gpu: None, connected: true, last_seen_s: None, stats: Value::Null, received: Instant::now() };
        r.installs = vec![inst("X1"), inst("X2"), inst("B")];
        let left: Vec<String> = r.other_installations(&["X2".to_owned()]).into_iter().map(|i| i.id).collect();
        assert_eq!(left, ["B"]);
    }
    fn gpu(util: Option<f64>, used: u64, total: u64) -> Value {
        let mut g = json!({"uuid": "GPU-1", "vram_used_bytes": used, "vram_total_bytes": total});
        if let Some(u) = util {
            g["utilization_percent"] = json!(u);
        }
        g
    }
    fn sample(gpus: Value) -> Sample {
        Sample::from_api(&json!({"gpus": gpus})).unwrap().unwrap().0
    }

    #[test]
    fn lightly_loaded_needs_both_gpu_load_and_vram_under_half() {
        assert!(lightly_loaded(&sample(json!([gpu(Some(49.0), 49, 100)]))));
        assert!(!lightly_loaded(&sample(json!([gpu(Some(50.0), 49, 100)]))), "load at 50%");
        assert!(!lightly_loaded(&sample(json!([gpu(Some(49.0), 50, 100)]))), "VRAM at 50%");
        assert!(!lightly_loaded(&sample(json!([gpu(None, 10, 100)]))), "unknown load");
        assert!(!lightly_loaded(&sample(json!([{"uuid": "GPU-1", "utilization_percent": 1.0}]))), "unknown VRAM");
        assert!(!lightly_loaded(&sample(json!([gpu(Some(1.0), 1, 100), gpu(Some(80.0), 1, 100)]))), "one busy GPU");
        assert!(!lightly_loaded(&sample(json!([]))), "no GPU present");
        assert!(!lightly_loaded(&Sample::from_api(&json!({})).unwrap().unwrap().0), "GPUs unreadable");
    }

    fn free_remote(age: f64, connected: bool) -> Remote {
        let mut r = Remote::blank(Source::Off, String::new());
        let stats = json!({"gpus": [gpu(Some(10.0), 10, 100)], "measured_age_s": age});
        r.installs = vec![Install { id: "X".into(), name: "X".into(), gpu: None, connected, last_seen_s: None, stats, received: Instant::now() }];
        r.phase = Phase::Online;
        r
    }
    fn job_on(iid: &str, state: &str) -> NetJob {
        NetJob { submission_id: "s".into(), submission: None, installation: Some(iid.into()), stages: vec![], active_stage: None, cancel_requested: false, events_done: false, job_id: None, folder: String::new(), name: String::new(), state: state.into(), on: None, done: 0, total: 0, reason: None, stage: None, cursor: 0, refreshed: false, last_poll: None, submission_attempted: false, inflight: false }
    }

    #[test]
    fn an_async_host_refusal_reports_its_reason_and_terminal_sends_stay_done() {
        let mut refused = job_on("X", "not sent");
        refused.submission_id = "refused".into();
        refused.reason = Some("model not available on that PC".into());
        let mut cancelled = job_on("X", "cancelled");
        cancelled.submission_id = "cancelled".into();
        let mut running = job_on("X", "running");
        running.submission_id = "running".into();
        let r = Remote { jobs: vec![refused, cancelled, running], ..Remote::blank(Source::Off, String::new()) };
        assert_eq!(r.sent_outcome("missing"), None, "not followed here");
        assert_eq!(r.sent_outcome("refused"), Some(Err(Some("model not available on that PC".into()))), "a definite refusal keeps its reason, like a failure");
        assert_eq!(r.sent_outcome("cancelled"), Some(Err(None)), "cancelled stays ended-without-failing");
        assert_eq!(r.sent_outcome("running"), Some(Ok(())), "still running: followed");
    }

    fn recv() -> Recv {
        Recv { last_phase: String::new(), attempt: "attempt".into(), job_id: "job".into(), meta: json!({}), stages: vec![], announced: 0, outbox: vec![], seq: 0, last_done: 0, last_total: 0, last_report: Instant::now(), last_progress: Instant::now(), inflight: false, retry_at: None, cancel_sent: false, terminal: false, report_saved: false }
    }

    #[test]
    fn lost_terminal_requires_stopped_writer_and_is_idempotent() {
        let mut stopped = recv();
        let (state, msg) = Remote::lost_verdict("Key", "worker exited", true).unwrap();
        Remote::push_terminal(&mut stopped, state, &msg);
        Remote::push_terminal(&mut stopped, state, &msg);
        assert_eq!(stopped.outbox.len(), 1);
        assert_eq!(stopped.outbox[0]["type"], "terminal");
        assert_eq!(stopped.outbox[0]["data"]["state"], "failed");
        assert!(Remote::lost_verdict("Key", "worker exited", false).is_none());
    }

    #[test]
    fn send_again_takes_only_a_submission_that_has_never_been_attempted() {
        let mut r = Remote::blank(Source::Off, String::new());
        let mut j = job_on("X", "sending");
        j.submission = Some(json!({"submission_id": "s"}));
        j.inflight = true;
        j.last_poll = Some(Instant::now());
        r.jobs.push(j);
        assert!(!r.take_unsent("s"), "in-flight submission stays tracked");
        r.jobs[0].inflight = false;
        assert!(!r.take_unsent("s"), "uncertain submission stays tracked");
        r.jobs[0].last_poll = None;
        r.jobs[0].submission_attempted = true;
        assert!(!r.take_unsent("s"), "persisted uncertainty stays tracked after restart");
        r.jobs[0].submission_attempted = false;
        assert!(r.take_unsent("s"), "a never-attempted submission can be taken back");
    }

    #[test]
    fn failed_marker_save_blocks_first_submit_but_keeps_same_id_retry() {
        let mut r = Remote::blank(Source::Off, String::new());
        let now = Instant::now();
        let body = |id: &str| json!({"submission_id": id});
        let mut first = job_on("X", "sending");
        first.submission_id = "first-id".into();
        first.submission_attempted = true;
        first.inflight = true;
        first.last_poll = Some(now);
        r.jobs.push(first.clone());
        let mut retry = job_on("X", "sending");
        retry.submission_id = "retry-id".into();
        retry.submission_attempted = true;
        retry.inflight = true;
        retry.last_poll = Some(now);
        r.jobs.push(retry.clone());
        r.status = Some("Could not save the list of sent jobs (test failure).".into());
        let sendable = r.submits_after_marker_save(vec![(first, body("first-id"))], vec![(retry, body("retry-id"))], false);
        assert_eq!(sendable.len(), 1);
        assert_eq!(sendable[0].0.submission_id, "retry-id");
        assert_eq!(sendable[0].1["submission_id"], "retry-id");
        assert!(!r.jobs[0].submission_attempted);
        assert!(!r.jobs[0].inflight);
        assert!(r.jobs[0].last_poll.is_none());
        assert_eq!(r.status.as_deref(), Some("Could not save the list of sent jobs (test failure)."));
    }

    #[test]
    fn poll_jobs_does_not_send_first_submit_when_marker_save_fails() {
        crate::queue::set_settings_path(None);
        let mut r = Remote::blank(Source::Off, String::new());
        let now = Instant::now();
        let mut first = job_on("X", "sending");
        first.submission_id = "first-id".into();
        first.submission = Some(json!({"submission_id": "first-id"}));
        r.jobs.push(first);
        let mut retry = job_on("X", "sending");
        retry.submission_id = "retry-id".into();
        retry.submission = Some(json!({"submission_id": "retry-id"}));
        retry.submission_attempted = true;
        r.jobs.push(retry);
        r.poll_jobs(now);
        assert!(!r.jobs[0].submission_attempted);
        assert!(!r.jobs[0].inflight);
        assert!(r.jobs[0].last_poll.is_none());
        assert!(r.jobs[1].submission_attempted);
        assert_eq!(r.jobs[1].submission.as_ref().unwrap()["submission_id"], "retry-id");
        assert_eq!(r.jobs[1].last_poll, Some(now));
        assert!(r.status.as_deref().unwrap().contains("no settings folder"));
    }

    #[test]
    fn late_accepted_submit_reply_restores_a_tracked_job() {
        let mut r = Remote::blank(Source::Off, String::new());
        let now = Instant::now();
        let mut submitted = job_on("X", "sending");
        submitted.submission = Some(json!({"submission_id": "s"}));
        submitted.inflight = true;
        submitted.submission_attempted = true;
        submitted.last_poll = Some(now);
        r.on_submit_reply(submitted, Ok(json!({"job_id": "job_1", "state": "queued"})), now);
        assert_eq!(r.jobs.len(), 1);
        assert_eq!(r.jobs[0].job_id.as_deref(), Some("job_1"));
        assert_eq!(r.jobs[0].state, "queued");
        assert!(r.jobs[0].submission.is_none());
    }

    #[test]
    fn leave_blocks_new_work_and_waits_for_submit_reconciliation() {
        let mut r = free_remote(1.0, true);
        let cfg = Conn { enabled: true, receive: true, ..Conn::default() };
        assert!(r.accepts_work(&cfg));
        assert!(r.is_free("X"));
        let now = Instant::now();
        let mut submitted = job_on("X", "sending");
        submitted.submission = Some(json!({"submission_id": "s"}));
        submitted.submission_attempted = true;
        submitted.inflight = true;
        r.leaving = true;
        r.jobs_loaded = true;
        assert!(!r.accepts_work(&cfg));
        assert!(!r.is_free("X"));
        r.jobs.push(submitted.clone());
        assert!(r.remove_blocker(None).is_some(), "an in-flight Submit blocks Remove");
        r.jobs.clear();
        r.pend.insert("submit".into(), Pend::Submit(submitted.clone()));
        assert!(!r.leave_ready(), "Leave waits for the in-flight Submit reply");
        r.pend.remove("submit");
        r.on_submit_reply(submitted, Ok(json!({"job_id": "job_1", "state": "queued"})), now);
        assert_eq!(r.jobs[0].job_id.as_deref(), Some("job_1"));
        assert!(!r.leave_ready(), "a late acceptance remains protected until the job finishes");
        r.jobs[0].state = "succeeded".into();
        assert!(r.leave_ready());
    }

    #[test]
    fn a_client_with_one_running_job_is_drawn_compactly() {
        let mut r = free_remote(1.0, true);
        let mut j = job_on("X", "started");
        j.job_id = Some("job_1".into());
        j.name = "V1-0040_A001C040_261012_R4LF".into();
        j.stages = vec![NetStage { label: "Pre-matte · GVM".into(), done: 1, total: 1, completed: true, phase: None }];
        r.jobs.push(j);
        let ctx = egui::Context::default();
        let mut used = 0.0;
        for _ in 0..3 {
            let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(600.0, 2000.0))), ..Default::default() };
            let mut out = ctx.run_ui(input, |ui| {
                let top = ui.cursor().top();
                r.queue_ui(ui, None);
                used = ui.cursor().top() - top;
            });
            out.textures_delta.clear();
        }
        // Header, meters, one job line and one stage bar: a few hundred pixels at most, never the whole panel.
        assert!(used < 300.0, "client block is {used} px tall in a 2000 px panel");
    }

    #[test]
    fn is_free_needs_a_fresh_sample_and_no_unfinished_job() {
        assert!(free_remote(1.0, true).is_free("X"));
        assert!(free_remote(crate::metrics::REMOTE_STALE_S - 0.5, true).is_free("X"), "just under the limit is still fresh");
        assert!(!free_remote(crate::metrics::REMOTE_STALE_S + 0.1, true).is_free("X"), "stale");
        assert!(!free_remote(1.0, false).is_free("X"), "not connected");
        let mut r = free_remote(1.0, true);
        r.installs[0].received = Instant::now() - Duration::from_secs(20);
        assert!(!r.is_free("X"), "a list kept from before the link dropped is stale, however fresh it was then");
        let mut r = free_remote(1.0, true);
        r.phase = Phase::Connecting;
        assert!(!r.is_free("X"), "offline connection");
        assert!(!free_remote(1.0, true).is_free("other"), "unknown PC");
        let mut r = free_remote(1.0, true);
        r.installs[0].stats = json!({"gpus": [gpu(Some(10.0), 10, 100)]});
        assert!(!r.is_free("X"), "no age means unknown, not free");
        let mut r = free_remote(1.0, true);
        r.jobs.push(job_on("X", "sending"));
        assert!(!r.is_free("X") && r.has_unfinished("X"), "our job is queued there");
        r.jobs[0].state = "succeeded".into();
        assert!(r.is_free("X"), "a finished job frees the PC");
    }
}
