//! The receiving half of the app. The app's own endpoint (`app_api`) takes a pushed shot into the durable inbox and wakes the
//! driver; this module then runs it on the app's own scheduler and writes progress and the result straight back into the inbox
//! (which is what the sending PC reads from this PC's API) and into the history. Nothing here asks any server for work:
//! pending shots are read once at startup and again when the endpoint announces a commit; a local shot never touches HTTP.
//!
//! * One received shot at a time holds this PC's GPU (the scheduler's network hold), so it queues behind local work and later
//!   shots wait in the inbox; "one shot = one PC" and the clip/stage ownership of the scheduler are unchanged.
//! * The receiver checks the shot against its own shared-folder mappings (`JobSpec::preflight`) and runs it with its OWN exact
//!   model files (attached by the scheduler at dispatch); nothing of the sender's paths or model locations is used.
//! * A shot that was running when the app closed stays uncertain: its storage is inspected, never replayed, and nothing is
//!   released on a timer. The user can release one that cannot be settled.
//! * This PC's own entry for the other PCs (name, GPU, load) is published in-process every 5 s: no registration handshake.
use crate::app_api::{self, CallError, Event};
use crate::metrics::{Sample, View};
use crate::remote::{nlog, truncate, Action, Env};
use crate::scheduler::JobState;
use crate::submission::{JobSpec, Preflight, Stage};
use crate::worker::Outcome;
use eframe::egui::{self, RichText, Ui};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

/// Set whenever a received shot's history entry was written; the app re-reads the open project's received entries once (no polling).
static HISTORY_CHANGED: AtomicBool = AtomicBool::new(false);

/// A PC that registered on this app and told it its own endpoint (reverse publication). Its PC id is the registration id.
#[derive(Clone, Debug, PartialEq)]
pub struct Dest {
    pub pc_id: String,
    pub name: String,
    pub host_id: String,
    pub network_id: String,
    /// This app's own network id: the namespace old assignments (`network:<ns>/<pc id>`) were written in.
    pub legacy_ns: String,
    pub credentials: String,
    pub endpoints: Vec<String>,
}

impl Dest {
    fn parse(v: &Value) -> Option<Self> {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            pc_id: s("pc_id")?,
            name: s("name")?,
            host_id: s("host_installation_id")?,
            network_id: s("network_id")?,
            legacy_ns: s("legacy_network_id")?,
            credentials: s("credentials")?,
            endpoints: v.get("endpoints").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
        })
    }
}

/// Put the destinations into the saved connections like a Join would, so the PC is sent to, listed and used by Automatic through the
/// same machinery. The old assignment namespace is kept on the connection. A PC this app already joined directly only gains it.
/// True when the list changed.
pub fn add_destinations(n: &mut crate::settings::Network, dests: &[Dest]) -> bool {
    let mut changed = false;
    for d in dests {
        // The network a direct connection reaches: the saved id, else the one a successful connection taught it (credentials side file).
        let same_net = |c: &crate::settings::Connection| {
            let known = if c.network_id.is_empty() { crate::remote::learned_network_id(&c.credentials).unwrap_or_default() } else { c.network_id.clone() };
            !c.local_host && !known.is_empty() && known.eq_ignore_ascii_case(&d.network_id) && c.credentials != d.credentials
        };
        if let Some(c) = n.connections.iter_mut().find(|c| c.credentials == d.credentials) {
            let before = c.clone();
            c.endpoints.clone_from(&d.endpoints);
            c.host_name.clone_from(&d.name);
            c.host_installation_id.clone_from(&d.host_id);
            c.network_id.clone_from(&d.network_id);
            (c.legacy_ns, c.legacy_id) = (d.legacy_ns.clone(), d.pc_id.clone());
            changed |= *c != before;
        } else if let Some(c) = n.connections.iter_mut().find(|c| same_net(c)) {
            // The direct namespace and PC id stay as they are; the old host's id is an extra alias for this connection. An older
            // build saved it as the connection's own id (legacy_*): that moves to the alias so the direct id resolves again.
            let before = c.clone();
            (c.legacy_ns, c.legacy_id) = Default::default();
            (c.alias_ns, c.alias_id) = (d.legacy_ns.clone(), d.pc_id.clone());
            changed |= *c != before;
        } else {
            n.connections.push(crate::settings::Connection {
                credentials: d.credentials.clone(),
                enabled: true,
                receive: true,
                endpoints: d.endpoints.clone(),
                network_id: d.network_id.clone(),
                host_installation_id: d.host_id.clone(),
                host_name: d.name.clone(),
                legacy_ns: d.legacy_ns.clone(),
                legacy_id: d.pc_id.clone(),
                ..Default::default()
            });
            changed = true;
        }
    }
    changed
}

/// A queued shot an old database holds for another registered PC, waiting to be sent to that PC's app.
struct Fwd {
    job_id: String,
    target: String,
    submission_id: String,
    spec: JobSpec,
}

const STATS_EVERY: Duration = Duration::from_secs(5);
const RETRY: Duration = Duration::from_secs(5);
const PROGRESS_EVERY: Duration = Duration::from_millis(1500);
/// The one inbox slot of this PC's GPU hold.
const SLOT: &str = "app:local";

type Reply = Receiver<Result<Value, CallError>>;

fn call(args: Value) -> Reply {
    app_api::spawn_call("inbox", args)
}

/// What the storage says about an attempt left by an earlier run.
pub(crate) enum Recon {
    /// Manifest complete, no writer lock, all deliveries present.
    Complete,
    /// No lock anywhere and the result is incomplete: nothing is writing, the run was cut short.
    Interrupted(String),
    /// A lock is present or the state cannot be read: a writer may still exist. Never released automatically.
    Fenced(String),
}

/// Inspect an earlier run's storage (on a helper thread): files only, nothing inferred from a pid or an age, nothing
/// written or removed.
pub(crate) fn reconcile(meta: &Value) -> Recon {
    let dir = |k: &str| meta.get(k).and_then(Value::as_str).map(PathBuf::from);
    let (Some(mdir), Some(clip)) = (dir("manifest_dir"), dir("clip_root")) else { return Recon::Fenced("the saved attempt has no storage paths".into()) };
    for lock in [mdir.join(crate::jobs::LOCK_NAME), clip.join(crate::hints::HINT_LOCK)] {
        match std::fs::symlink_metadata(&lock) {
            Ok(_) => return Recon::Fenced(format!("{} exists: a writer may still be running (or died without cleaning up).", lock.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Recon::Fenced(format!("{} cannot be inspected ({e}).", lock.display())),
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

/// manifest_dir / clip_root / exports of the attempt, kept in the inbox plan for the reconciliation after a restart.
fn attempt_meta(pf: &Preflight) -> Value {
    json!({
        "manifest_dir": pf.job.manifest_dir.display().to_string(),
        "clip_root": pf.job.clip_root.display().to_string(),
        "exports": pf.stages.iter().filter_map(|s| match s { Stage::Task(t) if t.kind == crate::tasks::TaskKind::ExportVideo => t.params.get("dest").and_then(Value::as_str).map(str::to_owned), _ => None }).collect::<Vec<_>>(),
    })
}

struct Item {
    id: String,
    spec: Result<JobSpec, String>,
    not_before: Option<Instant>,
}

/// A shot running on this PC.
struct Run {
    job_id: String,
    folder: String,
    /// The receiver's own project folder of the shot (owner of its history entry, so the Board shows it after a restart).
    project: String,
    /// Scheduler job ids in execution order with the stage's label.
    stages: Vec<(String, &'static str)>,
    announced: usize,
    outbox: Vec<Value>,
    seq: u64,
    /// The worker pass last announced (sent as a "pass: …" note: the sender shows it next to the stage).
    last_phase: String,
    last_done: u64,
    last_total: u64,
    last_progress: Instant,
    inflight: Option<Reply>,
    retry_at: Option<Instant>,
    cancel_sent: bool,
    terminal: Option<(&'static str, String)>,
}

enum Rx {
    Idle,
    /// The receiver preflight runs on a thread (it scans the shared storage).
    Preflight { id: String, rx: Receiver<Result<Preflight, Vec<String>>> },
    /// A pinned temporal window is being checked against this GPU's free memory (engine estimator) before the start.
    Estimating { id: String, pf: Box<Preflight>, plan: Value, rx: Reply },
    /// The inbox is recording the start. `pf` is `None` (with `fail`) when the shot cannot run here: it is recorded and ended failed.
    Beginning { id: String, pf: Option<Box<Preflight>>, fail: Option<String>, rx: Reply },
    Running(Run),
}

/// A shot that was running when the app last closed.
struct Held {
    job_id: String,
    attempt_id: String,
    last_seq: u64,
    state: HeldState,
    /// Another PC's started shot from an old database (the PC's name): it never blocks this PC and is never re-run.
    foreign: Option<String>,
}

enum HeldState {
    Inspect(Receiver<Recon>),
    Report { rx: Reply, state: &'static str, message: String },
    /// Could not be settled by looking at the storage: the user decides.
    Owned(String),
    Release(Reply),
}

pub struct Inbound {
    rx: Rx,
    queue: VecDeque<Item>,
    seen: HashSet<String>,
    cancels: HashSet<String>,
    held: Vec<Held>,
    held_loaded: bool,
    reload: bool,
    pending: Option<Reply>,
    held_call: Option<Reply>,
    loaded: bool,
    retry_load_at: Option<Instant>,
    closing: bool,
    finished: Vec<String>,
    pub note: Option<String>,
    /// Set by the app while its endpoint is not running ("Network not active: ..."); the Clients page and the Board show it with Retry.
    pub inactive: Option<String>,
    // publishing this PC's own entry
    stats_seq: u64,
    last_publish: Option<Instant>,
    last_avail: Option<(bool, bool, bool)>,
    publishing: Option<Reply>,
    sent_cfg: Option<(bool, bool, String)>,
    configuring: Option<Reply>,
    // PCs registered here that told this app their own endpoint, and old shots queued here for them
    dests: Vec<Dest>,
    dest_reload: bool,
    dest_call: Option<Reply>,
    dests_changed: bool,
    foreign_call: Option<Reply>,
    forward: Vec<Fwd>,
    /// Old job id -> the submission id it was handed to the sender with (never handed twice).
    handed: HashMap<String, String>,
    closing_fwd: Vec<(String, Reply)>,
    /// Shot id -> (clip folder, project) mapped through this PC's shared folders, known before any refusal so a refused shot keeps its place on the Board.
    homes: HashMap<String, (String, String)>,
}

impl Default for Inbound {
    fn default() -> Self {
        Self::new()
    }
}

impl Inbound {
    pub fn new() -> Self {
        Self {
            rx: Rx::Idle,
            queue: VecDeque::new(),
            seen: HashSet::new(),
            cancels: HashSet::new(),
            held: vec![],
            held_loaded: false,
            reload: true,
            pending: None,
            held_call: None,
            loaded: false,
            retry_load_at: None,
            closing: false,
            finished: vec![],
            note: None,
            inactive: None,
            stats_seq: 0,
            last_publish: None,
            last_avail: None,
            publishing: None,
            sent_cfg: None,
            configuring: None,
            dests: vec![],
            dest_reload: true,
            dest_call: None,
            dests_changed: false,
            foreign_call: None,
            forward: vec![],
            handed: HashMap::new(),
            closing_fwd: vec![],
            homes: HashMap::new(),
        }
    }

    /// True once after a received shot's history entry changed (the app re-reads the open project's received entries).
    pub fn take_history_changed(&mut self) -> bool {
        HISTORY_CHANGED.swap(false, Ordering::SeqCst)
    }
    /// The registered PCs that told this app their endpoint.
    pub fn destinations(&self) -> &[Dest] {
        &self.dests
    }
    /// True once after the destination list was (re)read: the app saves them as connections.
    pub fn take_destinations_changed(&mut self) -> bool {
        std::mem::take(&mut self.dests_changed)
    }
    /// Held shots that stop this PC from taking new ones (another PC's old shot never does).
    fn blocking_held(&self) -> bool {
        self.held.iter().any(|h| h.foreign.is_none())
    }
    /// Receive jobs is on and the endpoint runs (this struct only works while it does), nothing held, nothing running or waiting here.
    fn avail(&self, env: &Env) -> (bool, bool, bool) {
        let busy = env.ai.scheduler().running() || !self.queue.is_empty() || !matches!(self.rx, Rx::Idle);
        (env.settings.network.receives(), busy, self.blocking_held())
    }

    /// Shots whose outputs just finished (the App re-reads them).
    pub fn take_finished(&mut self) -> Vec<String> {
        std::mem::take(&mut self.finished)
    }
    /// Shots read from the inbox and not yet started, plus the one running (the Clients page shows it).
    pub fn waiting(&self) -> usize {
        self.queue.len()
    }
    pub fn receiving(&self) -> Option<String> {
        match &self.rx {
            Rx::Idle => None,
            Rx::Preflight { id, .. } => Some(format!("checking {id}")),
            Rx::Estimating { id, .. } => Some(format!("checking GPU memory for {id}")),
            Rx::Beginning { id, .. } => Some(format!("starting {id}")),
            Rx::Running(r) => Some(format!("running {}", r.job_id)),
        }
    }
    /// Begin shutdown: no further shot is started.
    pub fn shutdown_requested(&mut self) {
        self.closing = true;
    }
    /// Nothing is being started or run (an uncertain shot from an earlier run does not hold the app open).
    pub fn quiescent(&self) -> bool {
        matches!(self.rx, Rx::Idle)
    }
    /// Why this PC cannot be changed or removed now (a shot is running here, or one is uncertain).
    pub fn busy_reason(&self) -> Option<String> {
        if !matches!(self.rx, Rx::Idle) {
            return Some("this computer is working on a shot another PC sent".into());
        }
        self.held.iter().any(|h| !matches!(h.state, HeldState::Report { .. })).then(|| "a shot received before the last restart is not settled yet".into())
    }

    // ---- the per-pass entry -----------------------------------------------------------------------------------

    pub fn tick(&mut self, now: Instant, env: &mut Env) {
        if !app_api::running() {
            return;
        }
        for Event::Commit { kind, job_id } in app_api::take_events() {
            match kind.as_str() {
                "submitted" => self.reload = true,
                // A registered PC told this app its endpoint: read the list, and look again at shots queued here for it.
                "destination" => {
                    self.dest_reload = true;
                    self.reload = true;
                }
                "cancelled" => {
                    self.cancels.insert(job_id.clone());
                    if let Rx::Running(r) = &mut self.rx {
                        if r.job_id == job_id && !r.cancel_sent {
                            Self::cancel_all(r, env);
                        }
                    }
                }
                _ => {}
            }
        }
        self.configure(env);
        self.publish(now, env);
        self.load(now);
        self.settle_held();
        self.advance(now, env);
    }

    /// Receive jobs / join policy / PC name to the endpoint, whenever they differ from what it was last told.
    fn configure(&mut self, env: &Env) {
        if let Some(rx) = &self.configuring {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Err(e)) => {
                    self.sent_cfg = None;
                    self.note = Some(format!("The network settings could not be applied ({}).", e.message));
                }
                _ => {}
            }
            self.configuring = None;
        }
        let n = &env.settings.network;
        let want = (n.receives(), n.allow_join, truncate(n.name.trim(), 64));
        if self.sent_cfg.as_ref() != Some(&want) {
            let mut a = json!({"receive": want.0, "allow_join": want.1});
            if !want.2.is_empty() {
                a["name"] = json!(want.2);
            }
            self.configuring = Some(app_api::spawn_call("configure", a));
            self.sent_cfg = Some(want);
        }
    }

    /// This PC's name, GPU and load as the other PCs read them: written into this app's own endpoint every 5 s.
    fn publish(&mut self, now: Instant, env: &Env) {
        if let Some(rx) = &self.publishing {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => return,
                Ok(Err(e)) => self.note = Some(format!("This PC's entry for the other PCs could not be updated ({}).", e.message)),
                _ => {}
            }
            self.publishing = None;
        }
        // The usual 5 s cadence, and at once when Receive jobs / busy / held changed (an Automatic sender must not pick a PC that just got busy).
        let avail = self.avail(env);
        if self.last_publish.is_some_and(|t| now.saturating_duration_since(t) < STATS_EVERY) && self.last_avail == Some(avail) {
            return;
        }
        self.last_publish = Some(now);
        self.last_avail = Some(avail);
        let sample = if let View::Live(s) = env.metrics { Some(s) } else { None };
        let gpu = env.ai.scheduler().targets().into_iter().find(|t| t.local && t.enabled).and_then(|t| t.ready).and_then(|r| r.gpu);
        let name = truncate(env.settings.network.name.trim(), 64);
        let mut args = json!({"avail": {"receiving": avail.0, "busy": avail.1, "held": avail.2}});
        if !name.is_empty() {
            args["name"] = json!(name);
        }
        if let (Some(g), Some(s)) = (&gpu, sample) {
            match (g.verified_uuid(), s.ram_total.map(|b| b / (1 << 20)).filter(|r| *r > 0)) {
                (Ok(uuid), Some(ram)) => {
                    args["gpu"] = json!({"uuid": uuid, "name": truncate(&g.name, 120), "vram_total_mb": ((g.vram_gb * 1024.0) as u64).max(1), "ram_total_mb": ram});
                    self.stats_seq += 1;
                    args["stats"] = stats_args(s, self.stats_seq);
                }
                (Err(why), _) => self.note = Some(format!("This PC is not offered to other PCs: {why}")),
                _ => {}
            }
        }
        self.publishing = Some(app_api::spawn_call("publish", args));
    }

    fn load(&mut self, now: Instant) {
        if let Some(rx) = &self.pending {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                done => {
                    self.pending = None;
                    match done {
                        Ok(Ok(rows)) => {
                            self.loaded = true;
                            for r in rows.as_array().into_iter().flatten() {
                                let Some(id) = r.get("job_id").and_then(Value::as_str) else { continue };
                                if self.seen.insert(id.to_owned()) {
                                    let spec = serde_json::from_value::<JobSpec>(r.get("spec").cloned().unwrap_or_default()).map_err(|e| format!("this build does not understand the job spec: {e}"));
                                    self.queue.push_back(Item { id: id.to_owned(), spec, not_before: None });
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            self.note = Some(format!("The inbox could not be read ({}); trying again.", e.message));
                            self.retry_load_at = Some(now + RETRY);
                        }
                        _ => self.retry_load_at = Some(now + RETRY),
                    }
                }
            }
        }
        if let Some(rx) = &self.held_call {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                done => {
                    self.held_call = None;
                    match done {
                        Ok(Ok(rows)) => {
                            self.held_loaded = true;
                            for r in rows.as_array().into_iter().flatten() {
                                self.start_inspect(r);
                            }
                        }
                        _ => self.retry_load_at = Some(now + RETRY),
                    }
                }
            }
        }
        if let Some(rx) = &self.foreign_call {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                done => {
                    self.foreign_call = None;
                    if let Ok(Ok(rows)) = done {
                        for r in rows.as_array().into_iter().flatten() {
                            let (Some(job_id), Some(target)) = (r.get("job_id").and_then(Value::as_str), r.get("target").and_then(Value::as_str)) else { continue };
                            if self.handed.contains_key(job_id) || self.forward.iter().any(|f| f.job_id == job_id) {
                                continue;
                            }
                            match serde_json::from_value::<JobSpec>(r.get("spec").cloned().unwrap_or_default()) {
                                // The submission id the sender used is kept, so a retry can never make a second shot on the target PC.
                                Ok(spec) => self.forward.push(Fwd { job_id: job_id.to_owned(), target: target.to_owned(), submission_id: r.get("submission_id").and_then(Value::as_str).map_or_else(|| format!("fwd_{job_id}"), str::to_owned), spec }),
                                Err(e) => self.note = Some(format!("An old shot for another PC could not be read ({e}); it stays where it is.")),
                            }
                        }
                    }
                }
            }
        }
        if let Some(rx) = &self.dest_call {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                done => {
                    self.dest_call = None;
                    if let Ok(Ok(rows)) = done {
                        let v: Vec<Dest> = rows.as_array().into_iter().flatten().filter_map(Dest::parse).collect();
                        if v != self.dests {
                            self.dests = v;
                            self.dests_changed = true;
                        }
                    }
                }
            }
        }
        if self.dest_reload && self.dest_call.is_none() {
            self.dest_reload = false;
            self.dest_call = Some(app_api::spawn_call("destinations", json!({})));
        }
        let idle = self.pending.is_none() && self.held_call.is_none() && self.foreign_call.is_none();
        if (self.reload || !self.loaded || !self.held_loaded) && idle && self.retry_load_at.is_none_or(|t| now >= t) {
            self.reload = false;
            self.retry_load_at = None;
            self.held_call = (!self.held_loaded).then(|| call(json!({"method": "held"})));
            self.pending = Some(call(json!({"method": "pending"})));
            self.foreign_call = Some(call(json!({"method": "foreign"})));
        }
    }

    fn start_inspect(&mut self, row: &Value) {
        let (Some(job_id), Some(attempt_id)) = (row.get("job_id").and_then(Value::as_str), row.get("attempt_id").and_then(Value::as_str)) else { return };
        if self.held.iter().any(|h| h.attempt_id == attempt_id) {
            return;
        }
        self.seen.insert(job_id.to_owned());
        let meta = row.pointer("/plan/meta").cloned().unwrap_or(Value::Null);
        // An old database's started shot of ANOTHER PC: nothing here can see its storage or its writer. Held for the user, never re-run.
        if row.get("foreign").and_then(Value::as_bool) == Some(true) {
            let pc = row.get("pc").and_then(Value::as_str).unwrap_or("another PC").to_owned();
            let state = HeldState::Owned(format!("It was running on {pc} under the old version: check its output, then Clear."));
            self.held.push(Held { job_id: job_id.to_owned(), attempt_id: attempt_id.to_owned(), last_seq: 0, state, foreign: Some(pc) });
            return;
        }
        let state = if meta.get("manifest_dir").is_some() {
            let (tx, rx) = channel();
            let spawned = std::thread::Builder::new().name("net-reconcile".into()).spawn(move || {
                let _ = tx.send(reconcile(&meta));
            });
            if spawned.is_ok() { HeldState::Inspect(rx) } else { HeldState::Owned("its storage could not be inspected.".into()) }
        } else {
            HeldState::Owned("it predates restart reconciliation.".into())
        };
        self.held.push(Held { job_id: job_id.to_owned(), attempt_id: attempt_id.to_owned(), last_seq: row.get("last_seq").and_then(Value::as_u64).unwrap_or(0), state, foreign: None });
    }

    /// Shots left by an earlier run: look at the storage once, then close them in the inbox (or hand them to the user).
    fn settle_held(&mut self) {
        let mut done = vec![];
        for (i, h) in self.held.iter_mut().enumerate() {
            let next = match &mut h.state {
                HeldState::Inspect(rx) => match rx.try_recv() {
                    Ok(Recon::Complete) => Some(("succeeded", "complete (reconciled after a restart: manifest complete, no writer lock)".to_owned())),
                    Ok(Recon::Interrupted(m)) => Some(("failed", format!("interrupted by a restart of the app: {m}"))),
                    Ok(Recon::Fenced(m)) => {
                        h.state = HeldState::Owned(m);
                        None
                    }
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        h.state = HeldState::Owned("its storage could not be inspected.".into());
                        None
                    }
                },
                HeldState::Report { rx, state, message } => match rx.try_recv() {
                    Ok(Ok(_)) => {
                        history(&h.job_id, "", "", state, message, Value::Null);
                        done.push(i);
                        None
                    }
                    Ok(Err(e)) => {
                        h.state = HeldState::Owned(format!("the inbox could not close it ({})", e.message));
                        None
                    }
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        h.state = HeldState::Owned("the inbox could not close it".into());
                        None
                    }
                },
                HeldState::Release(rx) => match rx.try_recv() {
                    Ok(Ok(_)) => {
                        if h.foreign.is_none() {
                            history(&h.job_id, "", "", "failed", "released by the user on this PC (the writer was not proven stopped)", Value::Null);
                        }
                        done.push(i);
                        None
                    }
                    Ok(Err(e)) => {
                        h.state = HeldState::Owned(format!("it could not be released ({})", e.message));
                        None
                    }
                    Err(TryRecvError::Empty) => None,
                    Err(TryRecvError::Disconnected) => {
                        h.state = HeldState::Owned("it could not be released".into());
                        None
                    }
                },
                HeldState::Owned(_) => None,
            };
            if let Some((state, message)) = next {
                let ev = json!([{"seq": h.last_seq + 1, "type": "terminal", "data": {"state": state, "message": truncate(&message, 900)}}]);
                h.state = HeldState::Report { rx: call(json!({"method": "report", "job_id": h.job_id, "events": ev})), state, message };
            }
        }
        for i in done.into_iter().rev() {
            let h = self.held.remove(i);
            self.note = Some(format!("shot {} from before the restart is settled", h.job_id));
        }
    }

    /// "Forget attempt": the user checked this PC and its storage and releases shots that could not be settled by looking.
    pub fn release_held(&mut self, attempt_id: &str) {
        for h in self.held.iter_mut().filter(|h| h.attempt_id == attempt_id) {
            if matches!(h.state, HeldState::Owned(_)) {
                h.state = HeldState::Release(call(json!({"method": "abandon", "attempt_id": h.attempt_id, "reason": "released by the user on this PC"})));
            }
        }
    }

    // ---- one shot at a time -----------------------------------------------------------------------------------

    fn advance(&mut self, now: Instant, env: &mut Env) {
        match std::mem::replace(&mut self.rx, Rx::Idle) {
            Rx::Idle => self.dispatch(now, env),
            Rx::Preflight { id, rx } => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.rx = Rx::Preflight { id, rx },
                Ok(Ok(pf)) => self.preflight_passed(id, pf, now, env),
                Ok(Err(problems)) => {
                    nlog(&format!("shot {id}: preflight refused: {}", problems.join("; ")));
                    self.begin(id, None, Some(format!("refused by this PC: {}", truncate(&problems.join("; "), 700))), json!({"refused": true}));
                }
                Err(TryRecvError::Disconnected) => self.begin(id, None, Some("the receiver preflight stopped unexpectedly".into()), json!({"refused": true})),
            },
            Rx::Estimating { id, pf, plan, rx } => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.rx = Rx::Estimating { id, pf, plan, rx },
                Ok(Ok(v)) if v.get("over_budget").and_then(Value::as_bool) == Some(true) => {
                    let g = |k: &str| v.get(k).and_then(Value::as_f64).unwrap_or(0.0);
                    let w = pf.job.params.get("temporal_window").and_then(Value::as_str).unwrap_or("?");
                    let msg = format!("pinned temporal window {w} ({} VRAM) needs ~{:.1} GiB (peak {:.1} GiB) but this GPU has ~{:.1} GiB free; a fixed window is never downgraded", if v.get("low_vram").and_then(Value::as_bool) == Some(true) { "low" } else { "normal" }, g("required_gib"), g("peak_gib"), g("budget_gib"));
                    nlog(&format!("shot {id}: declined: {msg}"));
                    self.begin(id, None, Some(format!("refused by this PC: {msg}")), json!({"refused": true}));
                }
                Ok(Ok(v)) => {
                    let mut plan = plan;
                    plan["estimate"] = json!({"required_gib": v.get("required_gib"), "budget_gib": v.get("budget_gib")});
                    self.begin_prepared(id, pf, plan, now, env);
                }
                // No answer: the worker still enforces the window at launch.
                Ok(Err(_)) | Err(TryRecvError::Disconnected) => self.begin_prepared(id, pf, plan, now, env),
            },
            Rx::Beginning { id, pf, fail, rx } => match rx.try_recv() {
                Err(TryRecvError::Empty) => self.rx = Rx::Beginning { id, pf, fail, rx },
                Ok(Ok(_)) => self.begun(id, pf, fail, env),
                Ok(Err(e)) => {
                    env.ai.release_network_slot(SLOT);
                    self.began_badly(id, fail, e, now);
                }
                Err(TryRecvError::Disconnected) => env.ai.release_network_slot(SLOT),
            },
            Rx::Running(r) => self.run(r, now, env),
        }
    }

    /// Start the next waiting shot when this PC's GPU is free of local work (otherwise it simply waits in the queue).
    fn dispatch(&mut self, now: Instant, env: &mut Env) {
        // Receive jobs off: a shot already in the inbox waits there; nothing starts on this PC.
        if self.closing || self.blocking_held() || !env.settings.network.receives() {
            return;
        }
        let Some(pos) = self.queue.iter().position(|i| i.not_before.is_none_or(|t| now >= t)) else { return };
        if !env.ai.reserve_network_slot(SLOT) {
            return; // local work holds the GPU: the shot waits its turn
        }
        let Some(item) = self.queue.remove(pos) else { return };
        match item.spec {
            Err(why) => self.begin(item.id, None, Some(why), json!({"refused": true})),
            Ok(spec) => {
                let roots = env.settings.shared_roots.clone();
                self.homes.insert(item.id.clone(), home_of(&spec, &roots));
                let (tx, rx) = channel();
                let (id, wake) = (item.id.clone(), env.wake.clone());
                nlog(&format!("shot {id}: preflight"));
                let started = std::thread::Builder::new().name("net-preflight".into()).spawn(move || {
                    let _ = tx.send(spec.preflight(&roots, &id));
                    wake();
                });
                if started.is_ok() {
                    self.rx = Rx::Preflight { id: item.id, rx };
                } else {
                    env.ai.release_network_slot(SLOT);
                    self.queue.push_front(Item { id: item.id, spec: Err("the receiver preflight could not start".into()), not_before: Some(now + RETRY) });
                }
            }
        }
    }

    fn preflight_passed(&mut self, id: String, pf: Preflight, now: Instant, env: &mut Env) {
        nlog(&format!("shot {id}: preflight passed"));
        let sample = if let View::Live(s) = env.metrics { Some(s) } else { None };
        let uuid = env.ai.scheduler().targets().into_iter().find(|t| t.local && t.enabled).and_then(|t| t.ready).and_then(|r| r.gpu).and_then(|g| g.verified_uuid().ok().map(str::to_owned));
        let free_mb = sample.and_then(|s| free_vram(s, uuid.as_deref()));
        let mut plan = json!({"mode": "new", "gpu": uuid, "local_client": true, "temporal_window": pf.job.params.get("temporal_window"), "stages": pf.stages.iter().map(Stage::label).collect::<Vec<_>>(), "free_vram_mb": free_mb, "meta": attempt_meta(&pf)});
        let pinned = pf.job.params.get("temporal_window").and_then(Value::as_str).filter(|w| *w != "auto").map(str::to_owned);
        match (pinned, free_mb) {
            // Fixed window: the engine's own estimate against the memory this GPU has now, before anything is started.
            (Some(w), Some(free)) if free > 0 => {
                let args = json!({"first_frame": pf.first_frame.display().to_string(), "frames": pf.job.range.out_point - pf.job.range.in_point + 1, "temporal_window": w, "vram_profile": pf.job.params.get("vram_profile").and_then(Value::as_str).unwrap_or("auto"), "free_vram_mb": free});
                plan["estimate_asked"] = json!(true);
                self.rx = Rx::Estimating { id, pf: Box::new(pf), plan, rx: app_api::spawn_call("estimate", args) };
            }
            _ => self.begin_prepared(id, Box::new(pf), plan, now, env),
        }
    }

    fn begin_prepared(&mut self, id: String, pf: Box<Preflight>, plan: Value, now: Instant, env: &mut Env) {
        // Receive jobs may have been turned off while this shot was being prepared.
        if !env.settings.network.receives() {
            env.ai.release_network_slot(SLOT);
            self.seen.remove(&id);
            self.homes.remove(&id);
            self.reload = true;
            self.retry_load_at = Some(now + RETRY);
            return;
        }
        self.begin(id, Some(pf), None, plan);
    }

    /// Ask the inbox to record the start (durable BEFORE anything runs).
    fn begin(&mut self, id: String, pf: Option<Box<Preflight>>, fail: Option<String>, plan: Value) {
        let rx = call(json!({"method": "begin", "job_id": id, "slot": SLOT, "plan": plan}));
        self.rx = Rx::Beginning { id, pf, fail, rx };
    }

    fn began_badly(&mut self, id: String, fail: Option<String>, e: CallError, now: Instant) {
        match e.code.as_str() {
            // Cancelled or handled elsewhere meanwhile: nothing to run.
            "not_queued" | "unknown_job" => nlog(&format!("shot {id}: no longer queued ({})", e.code)),
            _ => {
                self.note = Some(format!("shot {id} could not be started yet ({}); it stays queued", e.message));
                match fail {
                    // A shot this PC cannot run is recorded as failed as soon as the inbox allows it.
                    Some(why) => self.queue.push_back(Item { id, spec: Err(why), not_before: Some(now + RETRY) }),
                    // A fine shot stays queued in the inbox: read it again instead of keeping a half-made copy here.
                    None => {
                        self.seen.remove(&id);
                        self.reload = true;
                        self.retry_load_at = Some(now + RETRY);
                    }
                }
            }
        }
    }

    fn begun(&mut self, id: String, pf: Option<Box<Preflight>>, fail: Option<String>, env: &mut Env) {
        let (folder, project) = self.homes.remove(&id).unwrap_or_default();
        let mut run = Run { job_id: id.clone(), folder, project, stages: vec![], announced: 0, outbox: vec![], seq: 0, last_phase: String::new(), last_done: 0, last_total: 0, last_progress: Instant::now(), inflight: None, retry_at: None, cancel_sent: false, terminal: None };
        match (pf, fail) {
            (Some(pf), _) => {
                run.stages = pf.stages.iter().map(|s| (s.job_id().to_owned(), s.label())).collect();
                run.folder = pf.job.clip_root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                // This PC's own project folder (<project>/clips/<clip>), mapped through this PC's shared folders: the Board reads it by project.
                run.project = project_of(&pf.job.clip_root);
                nlog(&format!("shot {id} started; running {} stage(s) on the local client", run.stages.len()));
                match env.ai.submit_network_plan(SLOT, pf.stages) {
                    Ok(()) => {
                        self.note = Some(format!("running network job {id}"));
                        history(&id, &run.folder, &run.project, "running", "", json!({"stages": run.stages.iter().map(|s| s.1).collect::<Vec<_>>()}));
                        if self.cancels.contains(&id) {
                            Self::cancel_all(&mut run, env);
                        }
                    }
                    Err(e) => Self::push_terminal(&mut run, "failed", &format!("this computer could not start it: {e}")),
                }
            }
            (None, why) => Self::push_terminal(&mut run, "failed", &why.unwrap_or_else(|| "refused by this PC".into())),
        }
        self.rx = Rx::Running(run);
    }

    fn cancel_all(r: &mut Run, env: &mut Env) {
        r.cancel_sent = true;
        for (id, _) in &r.stages {
            env.ai.cancel(id);
        }
    }

    fn push_event(r: &mut Run, t: &str, data: Value) {
        r.seq += 1;
        r.outbox.push(json!({"seq": r.seq, "type": t, "data": data}));
    }

    fn push_terminal(r: &mut Run, state: &'static str, msg: &str) {
        if r.terminal.is_some() {
            return;
        }
        r.terminal = Some((state, truncate(msg, 900)));
        Self::push_event(r, "terminal", json!({"state": state, "message": truncate(msg, 900)}));
    }

    fn lost_verdict(label: &str, reason: &str, writer_stopped: bool) -> Option<(&'static str, String)> {
        writer_stopped.then(|| ("failed", format!("{label} stage was lost: {reason}")))
    }

    fn run(&mut self, mut r: Run, now: Instant, env: &mut Env) {
        // The inbox's answer to the last report.
        if let Some(rx) = &r.inflight {
            match rx.try_recv() {
                Err(TryRecvError::Empty) => {}
                Ok(Ok(v)) => {
                    let last = v.get("last_seq").and_then(Value::as_u64).unwrap_or(0);
                    r.outbox.retain(|e| e["seq"].as_u64().unwrap_or(0) > last);
                    r.inflight = None;
                    if v.get("cancel_requested").and_then(Value::as_bool) == Some(true) && !r.cancel_sent {
                        Self::cancel_all(&mut r, env);
                    }
                }
                Ok(Err(e)) if e.code == "attempt_terminal" => {
                    // Settled by the user (released) while it ran: nothing more to report.
                    r.outbox.clear();
                    r.inflight = None;
                    r.terminal.get_or_insert(("failed", "released on this PC".into()));
                }
                Ok(Err(e)) => {
                    r.inflight = None;
                    r.retry_at = Some(now + RETRY);
                    self.note = Some(format!("could not record progress ({}); retrying", e.message));
                }
                Err(TryRecvError::Disconnected) => {
                    r.inflight = None;
                    r.retry_at = Some(now + RETRY);
                }
            }
        }
        if r.terminal.is_none() {
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
                        self.note = Some(format!("the {label} stage was lost ({reason}); writer {}. Inspect this computer and the storage.", if writer_stopped { "stopped" } else { "NOT proven stopped" }));
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
                Self::push_terminal(&mut r, state, &msg);
            } else if active == r.stages.len() {
                Self::push_terminal(&mut r, "succeeded", "complete");
            } else if active + 1 > r.announced {
                r.announced = active + 1;
                let msg = format!("stage {} of {}: {}", active + 1, r.stages.len(), r.stages[active].1);
                Self::push_event(&mut r, "note", json!({"message": msg}));
                r.last_done = u64::MAX;
                r.last_total = 0;
                Self::push_event(&mut r, "progress", json!({"done": 0, "total": 0}));
            }
            if let Some((done, total, phase)) = progress {
                if (done != r.last_done || total != r.last_total) && (r.last_done == u64::MAX || now.saturating_duration_since(r.last_progress) >= PROGRESS_EVERY) {
                    r.last_done = done;
                    r.last_total = total;
                    r.last_progress = now;
                    if phase != r.last_phase && !phase.is_empty() {
                        Self::push_event(&mut r, "note", json!({"message": format!("pass: {phase}")}));
                        r.last_phase = phase;
                    }
                    Self::push_event(&mut r, "progress", json!({"done": done, "total": total}));
                }
            }
        }
        if r.inflight.is_none() && !r.outbox.is_empty() && r.retry_at.is_none_or(|t| now >= t) {
            let evs: Vec<Value> = r.outbox.iter().take(256).cloned().collect();
            r.inflight = Some(call(json!({"method": "report", "job_id": r.job_id, "events": evs})));
        }
        if let (Some((state, message)), true, None) = (&r.terminal, r.outbox.is_empty(), &r.inflight) {
            history(&r.job_id, &r.folder, &r.project, state, message, Value::Null);
            self.note = Some(format!("network job {} finished and reported", r.job_id));
            if !r.folder.is_empty() {
                self.finished.push(r.folder.clone());
            }
            env.ai.release_network_slot(SLOT); // the GPU is the local queue's again
            return; // rx stays Idle
        }
        self.rx = Rx::Running(r);
    }

    // ---- UI -----------------------------------------------------------------------------------------------------

    /// This PC's receiving status for the Clients page (its row's Status cell).
    pub fn status_ui(&self, ui: &mut Ui) -> Vec<Action> {
        let mut acts = vec![];
        if let Some(why) = &self.inactive {
            ui.colored_label(egui::Color32::from_rgb(225, 110, 110), why);
            if ui.small_button("Retry").on_hover_text("Try to start this app's network endpoint again").clicked() {
                acts.push(Action::RetryEndpoint);
            }
        }
        if let Some(r) = self.receiving() {
            ui.label(RichText::new(format!("Receiving: {r}")).size(11.5));
        }
        if !self.queue.is_empty() {
            ui.label(RichText::new(format!("{} shot(s) from other PCs waiting", self.queue.len())).size(11.5));
        }
        for h in &self.held {
            let (msg, ask) = match &h.state {
                HeldState::Owned(m) if h.foreign.is_some() => (format!("Shot {} is held: {m}", h.job_id), true),
                HeldState::Owned(m) => (format!("Shot {} was running when the app closed and is still held: {m} Nothing new is received until it is settled. Check this computer and the storage.", h.job_id), true),
                HeldState::Inspect(_) => (format!("Shot {} was running when the app closed: checking its storage…", h.job_id), false),
                HeldState::Report { .. } | HeldState::Release(_) => (format!("Shot {}: closing it…", h.job_id), false),
            };
            ui.colored_label(egui::Color32::from_rgb(225, 110, 110), msg);
            let label = if h.foreign.is_some() { "Clear" } else { "Forget attempt" };
            if ask && ui.small_button(label).on_hover_text("I checked: release this shot (it is marked failed, and never run again)").clicked() {
                acts.push(Action::ReleaseHeld(h.attempt_id.clone()));
            }
        }
        if let Some(n) = &self.note {
            ui.label(RichText::new(n).color(egui::Color32::from_rgb(169, 187, 175)).size(11.0));
        }
        acts
    }
}

/// Old shots an earlier (shared) database queued for ANOTHER registered PC: send each to that PC's own app with the sender's original
/// submission id (so a retry can never make a second shot), and close it here once that app has accepted it. Needs the PC to have
/// told this app its endpoint (a destination); until then the shot waits where it is. Memory only: no asking, no timer.
pub fn pump_forwards(inbound: &mut Inbound, remote: &mut crate::remote_set::RemoteSet, env: &mut Env) {
    inbound.closing_fwd.retain_mut(|(_, rx)| !matches!(rx.try_recv(), Ok(_) | Err(TryRecvError::Disconnected)));
    let mut fwd = std::mem::take(&mut inbound.forward);
    fwd.retain(|f| {
        if let Some(sid) = inbound.handed.get(&f.job_id).cloned() {
            if let Some(new_job) = remote.accepted(&sid) {
                let pc = inbound.dests.iter().find(|d| d.pc_id == f.target).map_or("the other PC", |d| d.name.as_str());
                let note = format!("Sent to {pc} by the upgrade as job {new_job}: follow it on that PC. This copy is closed and never runs here.");
                inbound.closing_fwd.push((f.job_id.clone(), call(json!({"method": "forwarded", "job_id": f.job_id, "note": note}))));
                return false;
            }
            if let Some(Err(Some(why))) = remote.sent_outcome(&sid) {
                inbound.note = Some(format!("An old shot could not be sent to its PC ({why}); it stays queued here."));
                return false;
            }
            return true;
        }
        let Some(d) = inbound.dests.iter().find(|d| d.pc_id == f.target) else { return true };
        let folder = f.spec.clip_root.rsplit(['/', '\\']).next().unwrap_or("").to_owned();
        let n = crate::ui::ai::NetSubmit {
            submission_id: f.submission_id.clone(),
            spec: f.spec.clone(),
            target: Some(format!("{}/{}", d.legacy_ns, f.target)),
            folder: folder.clone(),
            name: format!("{folder} (moved by the upgrade)"),
            automatic: false,
        };
        if remote.send(n, env).is_ok() {
            inbound.handed.insert(f.job_id.clone(), f.submission_id.clone());
        }
        true
    });
    inbound.forward = fwd;
}

fn free_vram(s: &Sample, uuid: Option<&str>) -> Option<u64> {
    let g = s.gpus.as_ref()?.iter().find(|g| Some(g.uuid.as_str()) == uuid)?;
    Some(g.vram_total?.saturating_sub(g.vram_used?) / (1 << 20))
}

/// The load sample as the endpoint's stats schema wants it (every metric optional; unavailable is never 0).
pub(crate) fn stats_args(s: &Sample, seq: u64) -> Value {
    let mut o = serde_json::Map::new();
    o.insert("seq".into(), json!(seq));
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
    Value::Object(o)
}

/// The clip folder name and project folder this shot maps to on this PC (empty when its clip folder cannot be mapped).
fn home_of(spec: &JobSpec, roots: &[crate::shared_paths::SharedRoot]) -> (String, String) {
    match crate::shared_paths::SharedRef::decode(&spec.clip_root).and_then(|r| crate::shared_paths::resolve(roots, &r)) {
        Ok(p) => (p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), project_of(&p)),
        Err(_) => Default::default(),
    }
}

/// `<project>/clips/<clip>` -> `<project>`: the folder the Board opens a project by (empty when the path has no such shape).
fn project_of(clip_root: &std::path::Path) -> String {
    clip_root.parent().and_then(std::path::Path::parent).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default()
}

/// One received shot in the history (`received:<job id>`), replaced as it changes. `folder` and `owner` (the receiver's project
/// folder) are kept from the earlier entry when empty. The Board re-reads its received entries when this reports a change.
fn history(job_id: &str, folder: &str, owner: &str, state: &str, detail: &str, job: Value) {
    let Some(hp) = crate::queue::history_path() else { return };
    let id = format!("received:{job_id}");
    let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let prev = crate::queue::read(&hp).0.into_iter().find(|e| e.id == id);
    let entry = crate::queue::Entry {
        folder: if folder.is_empty() { prev.as_ref().map(|p| p.folder.clone()).unwrap_or_default() } else { folder.to_owned() },
        owner: if owner.is_empty() { prev.as_ref().map(|p| p.owner.clone()).unwrap_or_default() } else { owner.to_owned() },
        job: if job.is_null() { prev.and_then(|p| p.job) } else { Some(job) },
        id: id.clone(),
        label: "Received".into(),
        state: state.into(),
        detail: detail.into(),
        at,
        kind: "received".into(),
    };
    match crate::queue::replace(&hp, |e| e.id == id, vec![entry]) {
        Ok(()) => HISTORY_CHANGED.store(true, Ordering::SeqCst),
        Err(e) => nlog(&format!("history not updated for {job_id}: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lost_stage_ends_the_shot_only_with_the_writer_proven_stopped() {
        assert!(Inbound::lost_verdict("Key", "worker exited", false).is_none());
        let (state, msg) = Inbound::lost_verdict("Key", "worker exited", true).unwrap();
        assert_eq!(state, "failed");
        assert!(msg.contains("Key stage was lost"));
    }

    #[test]
    fn terminal_is_pushed_once() {
        let mut r = Run { job_id: "j".into(), folder: String::new(), project: String::new(), stages: vec![], announced: 0, outbox: vec![], seq: 0, last_phase: String::new(), last_done: 0, last_total: 0, last_progress: Instant::now(), inflight: None, retry_at: None, cancel_sent: false, terminal: None };
        Inbound::push_terminal(&mut r, "failed", "a");
        Inbound::push_terminal(&mut r, "succeeded", "b");
        assert_eq!(r.outbox.len(), 1);
        assert_eq!(r.outbox[0]["data"]["state"], "failed");
    }
}
