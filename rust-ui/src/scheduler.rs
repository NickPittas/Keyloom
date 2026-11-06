//! Authoritative job queue (`ai-implementation-brief.md` §6, §7.5). One job per target (one worker per GPU), whole clips
//! only, Auto = local first then list order. The app starts the workers when it opens (`probe`); they stay alive and idle
//! between jobs, so several clips run one after another on each GPU. A worker only executes: every job carries the exact
//! model files taken from the app's readiness snapshot (`set_models`).
//! `tick()` never blocks: call it from the UI loop.
//!
//! Safety rules:
//! - A job whose worker vanished is `Lost` and keeps fencing its manifest folder until the writer is
//!   *provably* gone (local child reaped) or the user confirms it. Only then are that job's own lock and temp
//!   files cleaned (`KeyJob::recover_writer_gone`). No heartbeat-based retry or lock stealing exists.
//! - A running inference is never killed automatically: silence is reported (`stalled`), the user decides
//!   (`cancel`, then `force_stop`).
//! - Model state is the app's: a job whose model is not Verified in the snapshot never reaches a worker, and its
//!   files are taken from the snapshot at dispatch (queued jobs are re-checked against every new snapshot).
use crate::jobs::{JobError, KeyJob, Recovery, MANIFEST_NAME, PROTOCOL};
use crate::tasks::{TaskJob, TaskKind};
use crate::models::{ModelFiles, Snapshot};
use crate::worker::{Cmd, Done, Event, LockCmd, LockReq, Msg, Outcome, Ready, Target, WorkerConn};
use serde_json::Value;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Placement {
    Auto,
    Target(String),
}

pub struct Submit {
    pub job: KeyJob,
    pub placement: Placement,
    /// Job ids that must finish `Complete` first; if one does not, this job is `Failed` and never starts.
    pub after: Vec<String>,
    /// Peak-VRAM estimate; targets reporting less are ineligible.
    pub min_vram_gb: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobState {
    Queued,
    Dispatching {
        target: String,
    },
    Running {
        target: String,
        phase: String,
        done: u64,
        total: u64,
    },
    /// `info.manifest` is overwritten with the LOCAL manifest path (`KeyJob::manifest_path`).
    Done {
        target: String,
        info: Done,
    },
    /// Worker refused it; nothing was written ("Failed (not started)").
    Rejected {
        target: String,
        code: String,
        message: String,
    },
    /// Never started: no eligible target, unavailable target or failed dependency.
    Failed {
        reason: String,
    },
    Cancelled,
    /// Connection/process ended without `done`. `writer_stopped` = proof the writer is gone (local child
    /// reaped) or the user confirmed it; `false` keeps the folder fenced. `recovery` reports the cleanup
    /// done once the writer was known to be gone.
    Lost {
        target: String,
        reason: String,
        writer_stopped: bool,
        recovery: Option<Recovery>,
    },
}

impl JobState {
    pub fn is_terminal(&self) -> bool {
        !matches!(
            self,
            JobState::Queued | JobState::Dispatching { .. } | JobState::Running { .. }
        )
    }
    pub fn fences(&self) -> bool {
        !self.is_terminal()
            || matches!(
                self,
                JobState::Lost {
                    writer_stopped: false,
                    ..
                }
            )
    }
    pub fn completed(&self) -> bool {
        matches!(self, JobState::Done { info, .. } if info.outcome == Outcome::Complete)
    }
}

/// Immutable view of one job; the `KeyJob` itself is shared and never mutated after submit.
#[derive(Clone, Debug)]
pub struct JobEntry {
    pub job: Arc<KeyJob>,
    pub placement: Placement,
    pub after: Vec<String>,
    pub min_vram_gb: Option<f64>,
    pub state: JobState,
    pub cancel_requested: bool,
    pub dispatched_at: Option<Instant>,
    pub accepted_at: Option<Instant>,
    /// Last event from the worker for this job (or the dispatch time).
    pub last_activity: Option<Instant>,
    /// The job exactly as sent to the worker (remapped paths, pinned digest); `None` until dispatched.
    pub sent: Option<Arc<KeyJob>>,
}

impl JobEntry {
    /// One user-facing sentence for any non-success end state, else `None`. A worker's `rejected` text
    /// (including the hard-link preflight: "This filesystem cannot publish files without risking an
    /// overwrite ... choose another output folder", code `paths`) is passed through unchanged.
    pub fn failure_message(&self) -> Option<String> {
        Some(match &self.state {
            JobState::Rejected {
                target,
                code,
                message,
            } => format!("{target} refused the job ({code}): {message}"),
            JobState::Failed { reason } => format!("Not started: {reason}"),
            JobState::Lost {
                target,
                reason,
                writer_stopped,
                ..
            } => format!(
                "Lost on {target}: {reason}{}",
                if *writer_stopped {
                    ""
                } else {
                    ". The folder stays locked until you confirm that machine stopped writing."
                }
            ),
            JobState::Done { info, .. } if info.outcome == Outcome::Failed => {
                info.message.clone().unwrap_or_else(|| "Failed".into())
            }
            _ => return None,
        })
    }

    pub fn manifest_path(&self) -> std::path::PathBuf {
        self.job.manifest_path()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum TargetStatus {
    /// Not started yet; the app starts it when it opens (`probe`) or a job needs it.
    Fresh,
    /// Process spawned, waiting for `ready` (bounded by `ready_timeout`).
    Starting,
    Ready,
    /// Protocol/GPU mismatch. Sticky until `reset_target`.
    Refused(String),
    /// Spawn failure, exit, ready timeout or protocol violation. Sticky until `reset_target`.
    Down(String),
    /// Stop requested (stdin closed); the process has not exited yet. Real termination = `Stopped`.
    Stopping,
    /// The process has exited after a requested stop or `force_stop`.
    Stopped,
}

struct Slot {
    target: Target,
    conn: Option<WorkerConn>,
    ready: Option<Ready>,
    status: TargetStatus,
    job: Option<String>,
    started: Instant,
    /// Manifest path exactly as the worker was told (to spot a `done` that names something else).
    sent_manifest: Option<String>,
    /// Local filesystem preflight running on a helper thread; the job is sent when it succeeds.
    preflight: Option<(Receiver<Result<(), JobError>>, KeyJob, ModelFiles)>,
    /// Job that was lost on this slot while the process was still dying; proof is added when it exits.
    lost_job: Option<String>,
    /// Stop was requested (`stop_*`/`force_stop`): the final status is `Stopped` once the process exits.
    stopping: bool,
    /// When this slot's worker died on its own (not stopped by the user): a crash restarts it, at most
    /// `CRASH_RESTARTS` times in `CRASH_WINDOW`, so queued work waits for it instead of failing.
    crashes: Vec<Instant>,
}

const CRASH_RESTARTS: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(600);

/// A lost job's writer counts as stopped only with proof: a local worker whose process has exited. On Windows
/// the venv `python.exe` is a launcher that starts the real interpreter, so the launcher exiting proves nothing
/// about the process tree: local Windows workers stay fenced until the user confirms.
pub fn writer_proven(windows_host: bool) -> bool {
    !windows_host
}

#[derive(Clone, Debug)]
pub struct TargetView {
    pub id: String,
    pub local: bool,
    pub enabled: bool,
    pub status: TargetStatus,
    pub busy_with: Option<String>,
    pub ready: Option<Ready>,
    /// Time since spawn while `Starting` (show "Starting AI worker… 42 s").
    pub starting_for: Option<Duration>,
    /// Start-up budget the target gets before it is declared `Down`.
    pub startup_budget: Duration,
}

#[derive(Debug, PartialEq)]
pub enum SubmitError {
    Invalid(JobError),
    DuplicateId,
    UnknownTarget(String),
    UnknownDependency(String),
    /// Same or nested manifest folder as an active/fenced job.
    Collision {
        with: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub enum SchedEvent {
    Changed(String),
    Published {
        job_id: String,
        stems: Vec<String>,
    },
    Warning {
        job_id: String,
        message: String,
    },
    Profile {
        job_id: String,
        hann_chunk: u32,
        hann_stride: u32,
        low_vram: bool,
    },
    /// Reply to a hint-lock request (`Scheduler::lock_request`).
    HintLock {
        request_id: String,
        outcome: LockOutcome,
    },
    /// SAM preview mask for `frame_stem` (a PNG under the task's staging folder).
    Preview {
        job_id: String,
        frame_stem: String,
        width: u32,
        height: u32,
        mask_png: String,
        score: Option<f64>,
    },
}

/// Can this job run, per the current target knowledge? For the UI's Submit gate; call `probe` first.
#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    Ready {
        target: String,
        busy: bool,
    },
    /// Some target may still become eligible (starting, or the app has not read the model state yet).
    Pending(String),
    Blocked(Vec<String>),
}

/// A dispatched job that has been silent for a while. Informational: nothing is killed.
#[derive(Clone, Debug, PartialEq)]
pub struct Stalled {
    pub job_id: String,
    pub target: String,
    pub accepted: bool,
    pub idle: Duration,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LockOutcome {
    /// The worker's facts about the lock (`None` = no lock): job_id, host, pid, same_host, local_dead,
    /// safe_to_clear, readable, problem...
    Inspected(Option<serde_json::Value>),
    Cleared {
        recovered: Vec<String>,
        warning: Option<String>,
    },
    Refused {
        code: String,
        message: String,
        lock: Option<serde_json::Value>,
    },
}

struct LockEntry {
    req: LockReq,
    /// The app-side helper's answer, once it was started (`None` = still waiting for the workers to be idle).
    result: Option<Receiver<LockOutcome>>,
}

/// What the app knows about the installed models. Jobs wait while it is `Unknown`, fail when it is `Unavailable`.
#[derive(Clone)]
enum ModelState {
    Unknown,
    Known(Arc<Snapshot>),
    Unavailable(String),
}

/// A queued/running non-Key job. Shares the slots (one GPU job per worker) with Key jobs.
#[derive(Clone, Debug)]
pub struct TaskEntry {
    pub job: Arc<TaskJob>,
    pub after: Vec<String>,
    pub state: JobState,
    pub cancel_requested: bool,
    pub last_activity: Option<Instant>,
}

impl TaskEntry {
    pub fn state_message(&self) -> Option<String> {
        match &self.state {
            JobState::Done { info, .. } if info.outcome == Outcome::Failed => info.message.clone(),
            _ => None,
        }
    }
}

pub struct Scheduler {
    locks: Vec<LockEntry>,
    lock_seq: u64,
    tasks: Vec<TaskEntry>,
    jobs: Vec<JobEntry>,
    slots: Vec<Slot>,
    /// Auto order: local targets first, then list order.
    order: Vec<usize>,
    models: ModelState,
    models_settling: bool,
    /// A model download/repair/delete is running: `Some(ids)` holds only jobs whose model files include one of those
    /// catalog ids (an empty list = every job that loads a model). Jobs that need no model never wait on it.
    models_changing: Option<Vec<String>>,
    /// First start imports torch and initialises CUDA, which can take minutes on a NAS or under antivirus. A
    /// spawned worker that has not said `ready` by then is killed (no job is involved yet)
    /// and the target is `Down` with that reason until `reset_target`: never respawned automatically. Default
    /// 600 s; `TargetView::starting_for` shows the elapsed startup time. Expose it in settings if needed.
    pub ready_timeout: Duration,
    /// Test hook: while set to `true` the preflight helper thread waits, so a test can hold a preflight pending.
    #[doc(hidden)]
    pub preflight_hold: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The physical GPU is held for one received attempt (claim → preflight → start → finish): until released,
    /// only the jobs/tasks registered for it may be dispatched, so a local Queue entry cannot slip in between.
    net_hold: Option<(String, std::collections::BTreeSet<String>)>,
}

/// Outcome of testing one target for one job.
enum Cand {
    /// `wire` is the job as it will be sent; built only when asked (`deep`), never for a busy target.
    Go {
        wire: Option<Box<KeyJob>>,
        /// The exact model files it carries (from the app's snapshot).
        files: ModelFiles,
    },
    Wait(String),
    No(String),
}

impl Scheduler {
    pub fn new(targets: Vec<Target>) -> Result<Self, String> {
        for (i, t) in targets.iter().enumerate() {
            t.validate()?;
            if targets[..i].iter().any(|o| o.id == t.id) {
                return Err(format!("duplicate target id {}", t.id));
            }
        }
        let order: Vec<usize> = (0..targets.len()).collect();
        let slots = targets
            .into_iter()
            .map(|target| Slot {
                target,
                conn: None,
                ready: None,
                status: TargetStatus::Fresh,
                job: None,
                started: Instant::now(),
                sent_manifest: None,
                preflight: None,
                lost_job: None,
                crashes: vec![],
                stopping: false,
            })
            .collect();
        Ok(Self {
            locks: vec![],
            lock_seq: 0,
            tasks: vec![],
            jobs: vec![],
            slots,
            order,
            models: ModelState::Unknown,
            models_settling: false,
            models_changing: None,
            ready_timeout: Duration::from_secs(600),
            preflight_hold: None,
            net_hold: None,
        })
    }

    /// `after` plus everything those ids transitively wait for: all of them finish before the new job starts.
    fn ancestors(&self, after: &[String]) -> Vec<String> {
        let mut seen: Vec<String> = vec![];
        let mut todo: Vec<String> = after.to_vec();
        while let Some(id) = todo.pop() {
            if seen.contains(&id) {
                continue;
            }
            let deps = self
                .jobs
                .iter()
                .find(|e| e.job.job_id == id)
                .map(|e| &e.after)
                .or_else(|| self.tasks.iter().find(|t| t.job.job_id == id).map(|t| &t.after));
            todo.extend(deps.into_iter().flatten().cloned());
            seen.push(id);
        }
        seen
    }

    pub fn submit(&mut self, s: Submit) -> Result<(), SubmitError> {
        s.job.validate().map_err(SubmitError::Invalid)?;
        if self.jobs.iter().any(|e| e.job.job_id == s.job.job_id) {
            return Err(SubmitError::DuplicateId);
        }
        if let Placement::Target(id) = &s.placement {
            if !self.slots.iter().any(|t| &t.target.id == id) {
                return Err(SubmitError::UnknownTarget(id.clone()));
            }
        }
        for d in &s.after {
            if self.state_of(d).is_none() {
                return Err(SubmitError::UnknownDependency(d.clone()));
            }
        }
        if let Some(e) = self
            .jobs
            .iter()
            .find(|e| e.state.fences() && e.job.conflicts_with(&s.job))
        {
            return Err(SubmitError::Collision {
                with: e.job.job_id.clone(),
            });
        }
        // A hint writer on the same clip may replace what Key reads: only a declared dependency may run with it.
        let before = self.ancestors(&s.after);
        if let Some(t) = self.tasks.iter().find(|t| {
            t.state.fences()
                && t.job.kind.writes_clip()
                && t.job.clip_root == s.job.clip_root
                && !before.contains(&t.job.job_id)
        }) {
            return Err(SubmitError::Collision {
                with: t.job.job_id.clone(),
            });
        }
        self.jobs.push(JobEntry {
            job: Arc::new(s.job),
            placement: s.placement,
            after: s.after,
            min_vram_gb: s.min_vram_gb,
            state: JobState::Queued,
            cancel_requested: false,
            dispatched_at: None,
            accepted_at: None,
            last_activity: None,
            sent: None,
        });
        Ok(())
    }

    pub fn snapshot(&self) -> Vec<JobEntry> {
        self.jobs.clone()
    }

    /// Board order for work that has not started: the Queued jobs and tasks of clips with nothing dispatched or
    /// running swap places among themselves so the clips in `roots` come first-to-last (stable: one clip's own steps
    /// keep their order; clips not listed go last). A clip that already started keeps its queued steps where they
    /// are, and dispatched, running and finished entries never move.
    pub fn reorder_queued(&mut self, roots: &[std::path::PathBuf]) {
        let rank = |r: &std::path::Path| roots.iter().position(|x| x == r).unwrap_or(usize::MAX);
        let live = |s: &JobState| !s.is_terminal() && *s != JobState::Queued;
        let started: Vec<std::path::PathBuf> = self.jobs.iter().filter(|e| live(&e.state)).map(|e| e.job.clip_root.clone())
            .chain(self.tasks.iter().filter(|e| live(&e.state)).map(|e| e.job.clip_root.clone())).collect();
        let at: Vec<usize> = (0..self.jobs.len()).filter(|i| self.jobs[*i].state == JobState::Queued && !started.contains(&self.jobs[*i].job.clip_root)).collect();
        let mut q: Vec<JobEntry> = at.iter().map(|i| self.jobs[*i].clone()).collect();
        q.sort_by_key(|e| rank(&e.job.clip_root));
        for (i, e) in at.into_iter().zip(q) {
            self.jobs[i] = e;
        }
        let at: Vec<usize> = (0..self.tasks.len()).filter(|i| self.tasks[*i].state == JobState::Queued && !started.contains(&self.tasks[*i].job.clip_root)).collect();
        let mut q: Vec<TaskEntry> = at.iter().map(|i| self.tasks[*i].clone()).collect();
        q.sort_by_key(|e| rank(&e.job.clip_root));
        for (i, e) in at.into_iter().zip(q) {
            self.tasks[i] = e;
        }
    }

    /// Inspect/clear a clip's hint lock through the app-side helper (`backend/hint_lock_cli.py`, which keeps the
    /// exact writer checks). The answer arrives as `SchedEvent::HintLock`. The request waits until no job runs on
    /// any worker (a running job may be the lock's owner: never race it) and no job starts while it runs.
    pub fn lock_request(&mut self, cmd: LockCmd) -> Result<String, String> {
        if !self
            .slots
            .iter()
            .any(|s| s.target.enabled)
        {
            return Err("no enabled local worker".into());
        }
        self.lock_seq += 1;
        let request_id = format!("lk-{}-{}", std::process::id(), self.lock_seq);
        self.locks.push(LockEntry {
            req: LockReq {
                request_id: request_id.clone(),
                cmd,
            },
            result: None,
        });
        Ok(request_id)
    }
    pub fn lock_pending(&self) -> bool {
        !self.locks.is_empty()
    }
    /// The first enabled local worker's target: where the app-side helpers run (mask deletion).
    pub fn local_target(&self) -> Option<Target> {
        self.order.iter().map(|c| &self.slots[*c].target).find(|t| t.enabled).cloned()
    }

    fn pump_locks(&mut self, ev: &mut Vec<SchedEvent>) {
        // Collect finished helpers.
        let mut l = 0;
        while l < self.locks.len() {
            let done = match self.locks[l].result.as_ref().map(|r| r.try_recv()) {
                Some(Ok(outcome)) => Some(outcome),
                Some(Err(TryRecvError::Disconnected)) => Some(LockOutcome::Refused {
                    code: "unavailable".into(),
                    message: "the lock helper ended without an answer; nothing was changed".into(),
                    lock: None,
                }),
                _ => None,
            };
            match done {
                Some(outcome) => {
                    let e = self.locks.remove(l);
                    ev.push(SchedEvent::HintLock { request_id: e.req.request_id, outcome });
                }
                None => l += 1,
            }
        }
        // Start waiting ones, one at a time, only while nothing runs anywhere.
        if self.net_hold.is_some() || self.slots.iter().any(|s| s.job.is_some()) || self.locks.iter().any(|l| l.result.is_some()) {
            return;
        }
        let Some(target) = self.order.iter().map(|c| &self.slots[*c].target).find(|t| t.enabled).cloned() else {
            return;
        };
        if let Some(l) = self.locks.iter_mut().find(|l| l.result.is_none()) {
            let (tx, rx) = channel();
            let request = l.req.helper_request();
            std::thread::spawn(move || {
                let _ = tx.send(run_lock_helper(&target, &request));
            });
            l.result = Some(rx);
        }
    }

    /// Hold the GPU for a received attempt. False (nothing held) while any local job, task or lock is active
    /// or queued, or while another attempt holds it; the same attempt may call it again.
    pub fn reserve_net(&mut self, attempt: &str) -> bool {
        if let Some((a, _)) = &self.net_hold {
            return a == attempt;
        }
        if self.running() || self.busy() || self.slots.iter().any(|s| s.job.is_some()) || !self.locks.is_empty() {
            return false;
        }
        self.net_hold = Some((attempt.to_owned(), Default::default()));
        true
    }
    /// Let a held attempt's jobs/tasks dispatch. False when `attempt` is not the holder.
    pub fn allow_net(&mut self, attempt: &str, ids: &[String]) -> bool {
        match &mut self.net_hold {
            Some((a, set)) if a == attempt => {
                set.extend(ids.iter().cloned());
                true
            }
            _ => false,
        }
    }
    pub fn release_net(&mut self, attempt: &str) {
        if self.net_hold.as_ref().is_some_and(|(a, _)| a == attempt) {
            self.net_hold = None;
        }
    }
    /// A held GPU only serves its attempt's own jobs.
    fn held_out(&self, id: &str) -> bool {
        self.net_hold.as_ref().is_some_and(|(_, ids)| !ids.contains(id))
    }

    pub fn running(&self) -> bool {
        self.jobs.iter().any(|e| !e.state.is_terminal())
            || self.tasks.iter().any(|t| !t.state.is_terminal())
    }
    /// Running, or a Lost writer still fences its folder.
    pub fn busy(&self) -> bool {
        self.jobs.iter().any(|e| e.state.fences()) || self.tasks.iter().any(|t| t.state.fences())
    }
    /// Catalog ids loaded by jobs that are queued, running or still fenced.
    pub fn active_model_usage(&self) -> (Vec<String>, bool) {
        let mut ids = std::collections::BTreeSet::new();
        let mut unknown = false;
        for _ in self.jobs.iter().filter(|e| e.state.fences()) {
            match &self.models {
                ModelState::Known(models) => match models.key_files() {
                    Ok(files) => ids.extend(files.into_keys()),
                    Err(_) => unknown = true,
                },
                _ => unknown = true,
            }
        }
        for task in self.tasks.iter().filter(|e| e.state.fences()) {
            match self.model_task_files(&task.job) {
                Some(files) => ids.extend(files.into_keys()),
                None => unknown = true,
            }
        }
        (ids.into_iter().collect(), unknown)
    }
    /// Lost tasks whose writer is not proven stopped.
    pub fn task_attention(&self) -> Vec<TaskEntry> {
        self.tasks
            .iter()
            .filter(|t| {
                matches!(
                    t.state,
                    JobState::Lost {
                        writer_stopped: false,
                        ..
                    }
                )
            })
            .cloned()
            .collect()
    }

    pub fn task_snapshot(&self) -> Vec<TaskEntry> {
        self.tasks.clone()
    }
    pub fn task(&self, id: &str) -> Option<&TaskEntry> {
        self.tasks.iter().find(|t| t.job.job_id == id)
    }

    /// State of a Key job or task by id.
    /// Why a step never started: the step it waited on, by name, and that step's own failure text (not its id).
    fn dependency_reason(&self, d: &str) -> String {
        let (what, st) = match (self.jobs.iter().find(|e| e.job.job_id == d), self.tasks.iter().find(|t| t.job.job_id == d)) {
            (Some(e), _) => ("Key", &e.state),
            (_, Some(t)) => (t.job.kind.label(), &t.state),
            _ => return format!("dependency {d} did not complete"),
        };
        let why = match st {
            JobState::Cancelled => "was cancelled".to_owned(),
            JobState::Failed { reason } => format!("did not start: {reason}"),
            JobState::Rejected { message, .. } => format!("was refused: {message}"),
            JobState::Lost { reason, .. } => format!("was lost: {reason}"),
            JobState::Done { info, .. } => format!("failed: {}", info.message.as_deref().unwrap_or("no reason given")),
            _ => "did not complete".to_owned(),
        };
        format!("{what} {why}")
    }

    /// How the chain ending in `id` ended: `None` while it runs (or the id is unknown), `Ok` when every step
    /// completed, else the state of the EARLIEST step that did not (a cancel shows as `Cancelled`, not as the
    /// "did not start" of the steps after it).
    pub fn chain_outcome(&self, id: &str) -> Option<Result<(), JobState>> {
        let st = self.state_of(id)?;
        if !st.is_terminal() {
            return None;
        }
        if st.completed() {
            return Some(Ok(()));
        }
        let (mut cur, mut root) = (id.to_owned(), st.clone());
        let after = |i: &str| -> Vec<String> {
            self.jobs.iter().find(|e| e.job.job_id == i).map(|e| e.after.clone())
                .or_else(|| self.tasks.iter().find(|t| t.job.job_id == i).map(|t| t.after.clone()))
                .unwrap_or_default()
        };
        // Bounded: a chain is acyclic and no longer than the jobs known.
        for _ in 0..=self.jobs.len() + self.tasks.len() {
            let Some(d) = after(&cur).into_iter().find(|d| self.state_of(d).is_some_and(|s| !s.completed())) else { break };
            root = self.state_of(&d).cloned().unwrap_or(JobState::Cancelled);
            cur = d;
        }
        Some(Err(root))
    }

    pub fn state_of(&self, id: &str) -> Option<&JobState> {
        self.jobs
            .iter()
            .find(|e| e.job.job_id == id)
            .map(|e| &e.state)
            .or_else(|| {
                self.tasks
                    .iter()
                    .find(|t| t.job.job_id == id)
                    .map(|t| &t.state)
            })
    }

    /// Queue a task on the local worker. `after` may name Key jobs or other tasks.
    pub fn submit_task(&mut self, job: TaskJob, after: Vec<String>) -> Result<(), SubmitError> {
        job.validate().map_err(|m| {
            SubmitError::Invalid(JobError {
                field: "task",
                message: m,
            })
        })?;
        if self.state_of(&job.job_id).is_some() {
            return Err(SubmitError::DuplicateId);
        }
        for d in &after {
            if self.state_of(d).is_none() {
                return Err(SubmitError::UnknownDependency(d.clone()));
            }
        }
        let fence = job.fence();
        let before = self.ancestors(&after);
        if let Some(t) = self.tasks.iter().find(|t| {
            t.state.fences() && t.job.fence() == fence && !before.contains(&t.job.job_id)
        })
        {
            return Err(SubmitError::Collision {
                with: t.job.job_id.clone(),
            });
        }
        // A Key still waiting in the queue is not a conflict: it consumes this writer's result, so it is made to
        // wait for the new step (below) and the hint goes first. A Key that already started does collide.
        let mut keys_after: Vec<String> = vec![];
        if job.kind.writes_clip() {
            if let Some(e) = self.jobs.iter().find(|e| {
                e.state.fences()
                    && e.state != JobState::Queued
                    && e.job.clip_root == job.clip_root
                    && !before.contains(&e.job.job_id)
            }) {
                return Err(SubmitError::Collision {
                    with: e.job.job_id.clone(),
                });
            }
            keys_after = self
                .jobs
                .iter()
                .filter(|e| e.state == JobState::Queued && e.job.clip_root == job.clip_root && !before.contains(&e.job.job_id))
                .map(|e| e.job.job_id.clone())
                .collect();
        }
        let id = job.job_id.clone();
        self.tasks.push(TaskEntry {
            job: Arc::new(job),
            after,
            state: JobState::Queued,
            cancel_requested: false,
            last_activity: None,
        });
        for k in keys_after {
            self.add_after(&k, &id);
        }
        Ok(())
    }
    pub fn job(&self, id: &str) -> Option<&JobEntry> {
        self.jobs.iter().find(|e| e.job.job_id == id)
    }
    /// Make a still-queued Key job wait for `dep` too (a hint queued after it, which Key must consume). Refused,
    /// returning false, when it would form a cycle or either job is unknown/started.
    pub fn add_after(&mut self, id: &str, dep: &str) -> bool {
        if id == dep || self.state_of(dep).is_none() || self.ancestors(&[dep.to_owned()]).iter().any(|a| a == id) {
            return false;
        }
        match self.jobs.iter_mut().find(|e| e.job.job_id == id && e.state == JobState::Queued) {
            Some(e) => {
                if !e.after.iter().any(|a| a == dep) {
                    e.after.push(dep.to_owned());
                }
                true
            }
            None => false,
        }
    }
    /// The app's readiness snapshot changed (startup, Check/Verify, root change, finished model operation). Queued
    /// jobs are judged against it at dispatch; running ones keep the files they were sent.
    pub fn set_models(&mut self, snapshot: Arc<Snapshot>) {
        self.models = ModelState::Known(snapshot);
    }
    /// The app cannot read the model state at all (no usable runtime): queued model jobs fail with this reason.
    pub fn set_models_unavailable(&mut self, why: String) {
        self.models = ModelState::Unavailable(why);
    }
    pub fn forget_models(&mut self) {
        self.models = ModelState::Unknown;
    }
    /// Files may be changing (model operation) or the snapshot is about to be replaced: dispatch pauses.
    pub fn set_models_settling(&mut self, settling: bool) {
        self.models_settling = settling;
    }
    /// A rebuilt scheduler (Settings > Apply) starts with what the app already knows about the models.
    pub fn inherit_models(&mut self, old: &Scheduler) {
        self.models = old.models.clone();
        self.models_settling = old.models_settling;
        self.models_changing = old.models_changing.clone();
    }
    pub fn set_models_changing(&mut self, changing: Option<Vec<String>>) {
        self.models_changing = changing;
    }
    pub fn models_changing(&self) -> Option<&Vec<String>> {
        self.models_changing.as_ref()
    }
    /// A job carrying these model files must wait for the model operation that is running.
    fn model_held(&self, files: &ModelFiles) -> bool {
        !files.is_empty()
            && self.models_changing.as_ref().is_some_and(|ids| ids.is_empty() || ids.iter().any(|i| files.contains_key(i)))
    }
    /// Why a queued task waits only for the model operation in progress (None = not held by it).
    pub fn task_hold_reason(&self, id: &str) -> Option<String> {
        let t = self.task(id)?;
        let files = self.model_task_files(&t.job)?;
        self.model_held(&files).then(|| "its model is being changed on the Models page".to_string())
    }
    fn model_task_files(&self, job: &TaskJob) -> Option<ModelFiles> {
        match &self.models {
            ModelState::Known(m) => m.task_files(job.kind, &job.params).ok(),
            _ => None,
        }
    }
    pub fn models(&self) -> Option<&Arc<Snapshot>> {
        match &self.models {
            ModelState::Known(s) => Some(s),
            _ => None,
        }
    }

    pub fn targets(&self) -> Vec<TargetView> {
        self.slots
            .iter()
            .map(|s| TargetView {
                id: s.target.id.clone(),
                local: true,
                enabled: s.target.enabled,
                status: s.status.clone(),
                busy_with: s.job.clone(),
                ready: s.ready.clone(),
                starting_for: (s.status == TargetStatus::Starting).then(|| s.started.elapsed()),
                startup_budget: self.ready_timeout,
            })
            .collect()
    }

    /// A GPU appeared: add its worker slot (never removes or reorders existing ones; the id must be new).
    pub fn add_target(&mut self, target: Target) -> Result<(), String> {
        target.validate()?;
        if self.slots.iter().any(|s| s.target.id == target.id) {
            return Err(format!("duplicate target id {}", target.id));
        }
        self.slots.push(Slot {
            target,
            conn: None,
            ready: None,
            status: TargetStatus::Fresh,
            job: None,
            started: Instant::now(),
            sent_manifest: None,
            preflight: None,
            lost_job: None,
            crashes: vec![],
            stopping: false,
        });
        self.order.push(self.slots.len() - 1);
        Ok(())
    }

    /// Start (not wait for) workers: the app calls this when it opens and keeps one idle worker per GPU, whatever
    /// the project is doing. `None` = every enabled Fresh target. Safe to call repeatedly.
    pub fn probe(&mut self, target: Option<&str>) {
        for c in 0..self.slots.len() {
            if target.is_none_or(|t| t == self.slots[c].target.id) {
                self.spawn_if_fresh(c);
            }
        }
    }

    fn spawn_if_fresh(&mut self, c: usize) {
        let s = &mut self.slots[c];
        if s.status != TargetStatus::Fresh || !s.target.enabled {
            return;
        }
        s.started = Instant::now();
        match WorkerConn::spawn(&s.target) {
            Ok(conn) => {
                s.conn = Some(conn);
                s.status = TargetStatus::Starting;
            }
            Err(e) => s.status = TargetStatus::Down(e),
        }
    }

    /// Queued → `Cancelled` now. Dispatched/running → ask the worker; the state changes on its `done`.
    pub fn cancel(&mut self, id: &str) {
        let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) else {
            return self.cancel_task(id);
        };
        match self.jobs[j].state {
            JobState::Queued => self.jobs[j].state = JobState::Cancelled,
            JobState::Dispatching { .. } | JobState::Running { .. } => {
                self.jobs[j].cancel_requested = true;
                if let Some(s) = self
                    .slots
                    .iter_mut()
                    .find(|s| s.job.as_deref() == Some(id) && s.preflight.is_some())
                {
                    // Not sent yet: nothing to ask the worker, just drop the job.
                    s.preflight = None;
                    s.job = None;
                    self.jobs[j].state = JobState::Cancelled;
                    return;
                }
                if let Some(c) = self
                    .slots
                    .iter_mut()
                    .find(|s| s.job.as_deref() == Some(id))
                    .and_then(|s| s.conn.as_mut())
                {
                    let _ = c.cancel(id);
                }
            }
            _ => {}
        }
    }

    fn cancel_task(&mut self, id: &str) {
        let Some(t) = self.tasks.iter().position(|t| t.job.job_id == id) else {
            return;
        };
        match self.tasks[t].state {
            JobState::Queued => self.tasks[t].state = JobState::Cancelled,
            JobState::Dispatching { .. } | JobState::Running { .. } => {
                self.tasks[t].cancel_requested = true;
                if let Some(c) = self
                    .slots
                    .iter_mut()
                    .find(|s| s.job.as_deref() == Some(id))
                    .and_then(|s| s.conn.as_mut())
                {
                    let _ = c.cancel(id);
                }
            }
            _ => {}
        }
    }

    /// Explicit recovery: the user states the lost job's writer is gone. Releases its folder fence and cleans
    /// that job's own lock/temp files (the folder must be reachable locally, e.g. shared storage).
    pub fn confirm_writer_stopped(&mut self, id: &str) -> Option<Recovery> {
        if let Some(t) = self.tasks.iter_mut().find(|t| t.job.job_id == id) {
            if let JobState::Lost { writer_stopped, .. } = &mut t.state {
                *writer_stopped = true;
            }
            return None;
        }
        let e = self.jobs.iter_mut().find(|e| e.job.job_id == id)?;
        let JobState::Lost {
            writer_stopped,
            recovery,
            ..
        } = &mut e.state
        else {
            return None;
        };
        if *writer_stopped {
            return None;
        }
        *writer_stopped = true;
        *recovery = Some(e.job.recover_as_sent(e.sent.as_deref().unwrap_or(&e.job)));
        recovery.clone()
    }

    /// Forget a Down/Refused/Stopped verdict so the target can be spawned again (idle targets only).
    pub fn reset_target(&mut self, id: &str) {
        let Some(c) = self.slots.iter().position(|s| s.target.id == id) else {
            return;
        };
        let s = &mut self.slots[c];
        if s.job.is_some() || s.conn.is_some() {
            return;
        }
        s.ready = None;
        s.stopping = false;
        s.status = TargetStatus::Fresh;
    }

    /// A job still in its preflight was never sent: drop it as `Cancelled` (nothing was written, no worker saw
    /// it), instead of letting a later send hit a closed pipe and look like a lost writer.
    fn cancel_pending_preflight(&mut self, c: usize, ev: &mut Vec<SchedEvent>) {
        if self.slots[c].preflight.take().is_some() {
            if let Some(id) = self.slots[c].job.take() {
                if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) {
                    self.jobs[j].cancel_requested = true;
                    self.set(j, JobState::Cancelled, ev);
                }
            }
        }
    }

    /// Graceful stop of one target: its worker is told to cancel (stdin EOF), finishes its `done` and exits.
    /// The status is `Stopping` until the process has really exited (`Stopped`). Nothing is killed.
    pub fn stop_target(&mut self, id: &str) {
        if let Some(c) = self.slots.iter().position(|s| s.target.id == id) {
            self.cancel_pending_preflight(c, &mut vec![]);
        }
        if let Some(s) = self.slots.iter_mut().find(|s| s.target.id == id) {
            if let Some(c) = s.conn.as_mut() {
                c.request_stop();
                s.stopping = true;
                s.status = TargetStatus::Stopping;
            } else if s.status == TargetStatus::Fresh {
                s.status = TargetStatus::Stopped;
            }
        }
    }

    /// Graceful stop of everything; queued jobs are cancelled. Poll `tick` until `all_stopped`.
    pub fn stop_all(&mut self) {
        for j in 0..self.jobs.len() {
            if self.jobs[j].state == JobState::Queued {
                self.jobs[j].state = JobState::Cancelled;
            }
        }
        for t in self
            .tasks
            .iter_mut()
            .filter(|t| t.state == JobState::Queued)
        {
            t.state = JobState::Cancelled;
        }
        let ids: Vec<String> = self.slots.iter().map(|s| s.target.id.clone()).collect();
        ids.iter().for_each(|i| self.stop_target(i));
    }

    /// No worker process is alive.
    pub fn all_stopped(&self) -> bool {
        self.slots.iter().all(|s| s.conn.is_none())
    }

    /// Explicit hard stop of one target. Non-blocking: the process is signalled, the target shows `Stopping`
    /// and a running job becomes `Lost` with `writer_stopped: false`. When the exit is observed (a later
    /// `tick`) the target becomes `Stopped` and, where `writer_proven`, the job's proof and recovery appear.
    pub fn force_stop(&mut self, id: &str) {
        let Some(c) = self.slots.iter().position(|s| s.target.id == id) else {
            return;
        };
        let mut ev = vec![];
        self.slots[c].stopping = true;
        self.lose(c, "stopped by the user (force stop)".into(), &mut ev);
        if self.slots[c].conn.is_some() {
            self.slots[c].status = TargetStatus::Stopping;
        }
    }

    /// Can `job` run now, later, or never, on `placement`? Mirrors dispatch exactly.
    pub fn readiness(
        &self,
        placement: &Placement,
        job: &KeyJob,
        min_vram_gb: Option<f64>,
    ) -> Readiness {
        let e = JobEntry {
            job: Arc::new(job.clone()),
            placement: placement.clone(),
            after: vec![],
            min_vram_gb,
            state: JobState::Queued,
            cancel_requested: false,
            dispatched_at: None,
            accepted_at: None,
            last_activity: None,
            sent: None,
        };
        let (mut wait, mut reasons) = (None, vec![]);
        for c in self.candidates(placement) {
            match self.candidate(&e, c, true) {
                Cand::Go { .. } => {
                    let s = &self.slots[c];
                    return Readiness::Ready {
                        target: s.target.id.clone(),
                        busy: s.job.is_some(),
                    };
                }
                Cand::Wait(w) => wait = Some(w),
                Cand::No(r) => reasons.push(format!("{}: {r}", self.slots[c].target.id)),
            }
        }
        match wait {
            Some(w) => Readiness::Pending(w),
            None if reasons.is_empty() => Readiness::Blocked(vec!["no enabled target".into()]),
            None => Readiness::Blocked(reasons),
        }
    }

    /// Every Lost job that still needs a human: writer not proven stopped, or cleanup reported a problem or
    /// left a foreign lock. Poll this to be sure none is unreachable in the UI.
    pub fn needs_attention(&self) -> Vec<JobEntry> {
        self.jobs
            .iter()
            .filter(|e| match &e.state {
                JobState::Lost {
                    writer_stopped,
                    recovery,
                    ..
                } => {
                    !writer_stopped
                        || recovery
                            .as_ref()
                            .is_some_and(|r| !r.errors.is_empty() || r.lock_left.is_some())
                }
                _ => false,
            })
            .cloned()
            .collect()
    }

    /// Dispatched jobs with no worker event for at least `threshold`.
    pub fn stalled(&self, threshold: Duration) -> Vec<Stalled> {
        self.jobs
            .iter()
            .filter_map(|e| {
                let target = match &e.state {
                    JobState::Dispatching { target } | JobState::Running { target, .. } => {
                        target.clone()
                    }
                    _ => return None,
                };
                let idle = e.last_activity?.elapsed();
                (idle >= threshold).then(|| Stalled {
                    job_id: e.job.job_id.clone(),
                    target,
                    accepted: e.accepted_at.is_some(),
                    idle,
                })
            })
            .collect()
    }

    pub fn tick(&mut self) -> Vec<SchedEvent> {
        let mut ev = vec![];
        for i in 0..self.slots.len() {
            while let Some(m) = self.slots[i].conn.as_mut().and_then(|c| c.try_recv()) {
                self.handle(i, m, &mut ev);
            }
            let s = &self.slots[i];
            if s.status == TargetStatus::Starting && s.started.elapsed() > self.ready_timeout {
                // No job is involved before ready, so killing here cannot interrupt inference.
                let why = format!("no ready acknowledgment within {} s (torch and CUDA start-up can be slow on network storage); press Retry to start it again or raise the start-up timeout", self.ready_timeout.as_secs());
                self.lose(i, why, &mut ev);
            }
        }
        self.poll_preflight(&mut ev);
        self.dispatch(&mut ev);
        self.dispatch_tasks(&mut ev);
        self.pump_locks(&mut ev);
        ev
    }

    fn set(&mut self, j: usize, st: JobState, ev: &mut Vec<SchedEvent>) {
        self.jobs[j].state = st;
        ev.push(SchedEvent::Changed(self.jobs[j].job.job_id.clone()));
    }

    /// Worker unusable: signal it to die (non-blocking). Its current job becomes `Lost` UNPROVEN; proof and
    /// recovery are added by `finish_exit` when the exit is actually observed.
    fn lose(&mut self, i: usize, reason: String, ev: &mut Vec<SchedEvent>) {
        let s = &mut self.slots[i];
        if let Some(c) = s.conn.as_mut() {
            c.kill();
        }
        s.ready = None;
        if !matches!(
            s.status,
            TargetStatus::Refused(_)
                | TargetStatus::Down(_)
                | TargetStatus::Stopping
                | TargetStatus::Stopped
        ) {
            s.status = TargetStatus::Down(reason.clone());
        }
        let target = s.target.id.clone();
        let unsent = s.preflight.is_some();
        s.preflight = None;
        if unsent {
            if let Some(id) = s.job.take() {
                // Never sent to the worker: nothing can have been written, so this is not a lost writer.
                if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) {
                    self.set(
                        j,
                        JobState::Failed {
                            reason: format!("not started: {target} became unavailable ({reason})"),
                        },
                        ev,
                    );
                }
            }
        }
        if let Some(id) = self.slots[i].job.clone() {
            if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) {
                self.slots[i].job = None;
                self.slots[i].lost_job = Some(id);
                self.set(
                    j,
                    JobState::Lost {
                        target,
                        reason: reason.clone(),
                        writer_stopped: false,
                        recovery: None,
                    },
                    ev,
                );
            }
        }
        if let Some(id) = self.slots[i].job.take() {
            if let Some(t) = self.tasks.iter().position(|t| t.job.job_id == id) {
                self.slots[i].lost_job = Some(id);
                self.tasks[t].state = JobState::Lost {
                    target: self.slots[i].target.id.clone(),
                    reason: reason.clone(),
                    writer_stopped: false,
                    recovery: None,
                };
                ev.push(SchedEvent::Changed(self.tasks[t].job.job_id.clone()));
            }
        }
        if self.slots[i].conn.is_none() {
            self.finish_exit(i, ev);
        }
    }

    /// The worker process has exited (reaped by the waiter thread): that is the only proof we accept.
    fn finish_exit(&mut self, i: usize, ev: &mut Vec<SchedEvent>) {
        let s = &mut self.slots[i];
        s.conn = None;
        if s.stopping {
            s.status = TargetStatus::Stopped;
        }
        let proven = writer_proven(cfg!(windows));
        if let (Some(id), true) = (s.lost_job.clone(), proven) {
            if let Some(t) = self.tasks.iter().position(|t| t.job.job_id == id) {
                s.lost_job = None;
                if let JobState::Lost { writer_stopped, .. } = &mut self.tasks[t].state {
                    *writer_stopped = true;
                }
                ev.push(SchedEvent::Changed(id));
                return;
            }
        }
        let s = &mut self.slots[i];
        if let (Some(id), true) = (s.lost_job.take(), proven) {
            if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) {
                let e = &self.jobs[j];
                let rec = e.job.recover_as_sent(e.sent.as_deref().unwrap_or(&e.job));
                if let JobState::Lost {
                    writer_stopped,
                    recovery,
                    ..
                } = &mut self.jobs[j].state
                {
                    *writer_stopped = true;
                    *recovery = Some(rec);
                }
                ev.push(SchedEvent::Changed(id));
            }
        }
    }

    fn handle(&mut self, i: usize, m: Msg, ev: &mut Vec<SchedEvent>) {
        let event = match m {
            Msg::Protocol(e) => {
                self.lose(i, format!("protocol error: {e}"), ev);
                return;
            }
            Msg::Exited { code, stderr } => {
                let why = match code {
                    Some(c) if c < 0 => format!("worker killed by signal {}", -c),
                    Some(c) => format!("worker exited with code {c}"),
                    None => "worker terminated by signal".into(),
                };
                // Mark the connection gone first so `lose` finalises the exit immediately.
                let stopping = self.slots[i].stopping;
                self.slots[i].conn = None;
                self.lose(i, format!("{why}: {stderr}"), ev);
                // A crash (not a stop or a refusal) restarts the worker: the queued work waits for it.
                let now = Instant::now();
                let s = &mut self.slots[i];
                s.crashes.retain(|t| now.duration_since(*t) < CRASH_WINDOW);
                if !stopping && matches!(s.status, TargetStatus::Down(_)) {
                    if s.crashes.len() < CRASH_RESTARTS {
                        s.crashes.push(now);
                        s.job = None;
                        s.ready = None;
                        s.status = TargetStatus::Fresh;
                    } else if let TargetStatus::Down(reason) = &s.status {
                        let reason = reason.clone();
                        s.status = TargetStatus::Down(format!(
                            "automatic restart limit ({CRASH_RESTARTS} restarts in {} s) reached: {reason}",
                            CRASH_WINDOW.as_secs()
                        ));
                    }
                }
                return;
            }
            Msg::Event(e) => e,
        };
        if let Event::Ready(r) = event {
            return self.ready(i, r, ev);
        }
        let want = self.slots[i].job.clone();
        let (Some(id), Some(got)) = (want, event.job_id().map(str::to_owned)) else {
            self.lose(i, "event with no job running".into(), ev);
            return;
        };
        if got != id {
            self.lose(i, format!("event for job {got} while running {id}"), ev);
            return;
        }
        let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) else {
            return self.handle_task(i, &id, event, ev);
        };
        self.jobs[j].last_activity = Some(Instant::now());
        let target = self.slots[i].target.id.clone();
        let st = self.jobs[j].state.clone();
        let accepted = matches!(st, JobState::Running { .. });
        match event {
            Event::Accepted { .. } if matches!(st, JobState::Dispatching { .. }) => {
                self.jobs[j].accepted_at = Some(Instant::now());
                self.set(
                    j,
                    JobState::Running {
                        target,
                        phase: "accepted".into(),
                        done: 0,
                        total: 0,
                    },
                    ev,
                )
            }
            Event::Rejected { code, message, .. } if matches!(st, JobState::Dispatching { .. }) => {
                self.slots[i].job = None;
                self.set(
                    j,
                    JobState::Rejected {
                        target,
                        code,
                        message,
                    },
                    ev,
                )
            }
            Event::Progress {
                phase, done, total, ..
            } if accepted => self.set(
                j,
                JobState::Running {
                    target,
                    phase,
                    done,
                    total,
                },
                ev,
            ),
            Event::Status { message, .. } if accepted => {
                if let JobState::Running { phase, .. } = &mut self.jobs[j].state {
                    *phase = message;
                }
                ev.push(SchedEvent::Changed(id))
            }
            Event::Warning { message, .. } if accepted => ev.push(SchedEvent::Warning {
                job_id: id,
                message,
            }),
            Event::Published { stems, .. } if accepted => {
                ev.push(SchedEvent::Published { job_id: id, stems })
            }
            Event::Profile {
                hann_chunk,
                hann_stride,
                low_vram,
                ..
            } if accepted => ev.push(SchedEvent::Profile {
                job_id: id,
                hann_chunk,
                hann_stride,
                low_vram,
            }),
            Event::Done(mut info) if accepted => {
                // The local manifest is the only authority; a path the worker reports is its own view.
                let sent = self.slots[i].sent_manifest.take().unwrap_or_default();
                let norm = |s: &str| s.replace('\\', "/");
                if info
                    .manifest
                    .as_deref()
                    .is_some_and(|m| norm(m) != norm(&sent))
                {
                    ev.push(SchedEvent::Warning {
                        job_id: id.clone(),
                        message: format!("worker reported manifest {:?}, expected {sent:?}; using the local path", info.manifest),
                    });
                }
                info.manifest = info.manifest.map(|_| {
                    self.jobs[j]
                        .job
                        .manifest_path()
                        .to_string_lossy()
                        .into_owned()
                });
                self.slots[i].job = None;
                self.set(j, JobState::Done { target, info }, ev)
            }
            other => {
                self.lose(i, format!("unexpected {other:?} in state {st:?}"), ev);
            }
        }
    }

    fn ready(&mut self, i: usize, r: Ready, ev: &mut Vec<SchedEvent>) {
        if !matches!(
            self.slots[i].status,
            TargetStatus::Starting | TargetStatus::Ready
        ) {
            return; // a dying/refused/stopped worker's acknowledgment means nothing
        }
        if self.slots[i].ready.is_some() {
            self.lose(i, "duplicate ready".into(), ev);
            return;
        }
        let s = &mut self.slots[i];
        let bad = if r.protocol != PROTOCOL {
            Some(format!("worker speaks protocol {}", r.protocol))
        } else {
            r.gpu.as_ref().filter(|g| g.index != s.target.gpu).map(|g| {
                format!(
                    "worker reports GPU {}, target is GPU {}",
                    g.index, s.target.gpu
                )
            })
        };
        if let Some(why) = bad {
            if let Some(c) = s.conn.as_mut() {
                c.kill(); // the process lingers in `conn` until its exit is observed
            }
            s.status = TargetStatus::Refused(why);
            return;
        }
        s.ready = Some(r);
        if s.status == TargetStatus::Starting {
            s.status = TargetStatus::Ready;
        }
    }

    fn candidates(&self, p: &Placement) -> Vec<usize> {
        match p {
            Placement::Auto => self.order.clone(),
            Placement::Target(id) => self
                .slots
                .iter()
                .position(|s| &s.target.id == id)
                .into_iter()
                .collect(),
        }
    }

    /// Everything that decides whether `e` may run on slot `c`, and the exact job to send there.
    fn candidate(&self, e: &JobEntry, c: usize, deep: bool) -> Cand {
        let s = &self.slots[c];
        if !s.target.enabled {
            return Cand::No("disabled".into());
        }
        match &s.status {
            TargetStatus::Refused(r) => return Cand::No(r.clone()),
            TargetStatus::Down(_) if s.conn.is_some() => {
                return Cand::Wait(format!("{} is restarting", s.target.id));
            }
            TargetStatus::Down(r) => return Cand::No(r.clone()),
            TargetStatus::Stopping | TargetStatus::Stopped => return Cand::No("stopped".into()),
            TargetStatus::Fresh | TargetStatus::Starting => {
                return Cand::Wait(format!("{} is starting", s.target.id))
            }
            TargetStatus::Ready => {}
        }
        let Some(r) = s.ready.as_ref() else {
            return Cand::Wait("worker not ready".into());
        };
        // A Key job needs a GPU, whatever the VRAM estimate says.
        let Some(gpu) = r.gpu.as_ref() else {
            return Cand::No("worker reports no GPU".into());
        };
        if let Some(need) = e.min_vram_gb.filter(|n| gpu.vram_gb < *n) {
            return Cand::No(format!(
                "{:.1} GB VRAM, job needs {need:.1} GB",
                gpu.vram_gb
            ));
        }
        // The app's own model state decides, never the worker: the exact files travel with the job.
        let files = match &self.models {
            ModelState::Unknown => return Cand::Wait("the app has not read the installed models yet".into()),
            ModelState::Unavailable(why) => return Cand::No(why.clone()),
            ModelState::Known(m) => match m.key_files() {
                Ok(f) => f,
                Err(why) => return Cand::No(why),
            },
        };
        if self.model_held(&files) {
            return Cand::Wait("its model is being changed on the Models page".into());
        }
        // Pure in-memory from here on: the local filesystem preflight happens at dispatch, off the UI thread.
        let wire = deep.then(|| Box::new((*e.job).clone()));
        Cand::Go { wire, files }
    }

    fn dispatch(&mut self, ev: &mut Vec<SchedEvent>) {
        if self.models_settling || self.locks.iter().any(|l| l.result.is_some()) {
            return; // model files may be changing, or a hint-lock helper is running: nothing may start under it
        }
        // Queued jobs are judged against the CURRENT model snapshot: a model that became unusable fails them here.
        let mut reserved: Vec<usize> = vec![];
        for j in 0..self.jobs.len() {
            if self.jobs[j].state != JobState::Queued || self.held_out(&self.jobs[j].job.job_id) {
                continue;
            }
            let mut wait = false;
            for d in self.jobs[j].after.clone() {
                let st = self.state_of(&d).cloned().unwrap_or(JobState::Cancelled);
                if st.completed() {
                    continue;
                }
                if st.is_terminal() {
                    let reason = self.dependency_reason(&d);
                    self.set(j, JobState::Failed { reason }, ev);
                    break;
                }
                wait = true;
            }
            if wait || self.jobs[j].state != JobState::Queued {
                continue;
            }
            let (mut pending, mut reasons, mut spawn) = (false, vec![], None);
            let mut chosen = None;
            for c in self.candidates(&self.jobs[j].placement.clone()) {
                // A busy target only needs the cheap eligibility verdict, not a rebuilt wire job.
                let idle = self.slots[c].job.is_none();
                match self.candidate(&self.jobs[j], c, idle) {
                    Cand::Go {
                        wire: Some(wire),
                        files,
                    } if idle => {
                        chosen = Some((c, *wire, files));
                        break;
                    }
                    Cand::Go { .. } => pending = true,
                    Cand::No(r) => reasons.push(format!("{}: {r}", self.slots[c].target.id)),
                    Cand::Wait(_) => {
                        pending = true;
                        match self.slots[c].status {
                            TargetStatus::Fresh => spawn = spawn.or(Some(c)),
                            TargetStatus::Starting if !reserved.contains(&c) && spawn.is_none() => {
                                reserved.push(c);
                                spawn = Some(usize::MAX);
                            }
                            _ => {}
                        }
                    }
                }
            }
            if let Some((c, job, files)) = chosen {
                self.start(j, c, job, files, ev);
            } else if let Some(c) = spawn.filter(|c| *c != usize::MAX) {
                reserved.push(c);
                self.spawn_if_fresh(c);
            } else if !pending {
                let reason = if reasons.is_empty() {
                    "no enabled target".into()
                } else {
                    format!("no eligible target ({})", reasons.join("; "))
                };
                self.set(j, JobState::Failed { reason }, ev);
            }
        }
    }

    fn start(
        &mut self,
        j: usize,
        c: usize,
        job: KeyJob,
        files: ModelFiles,
        ev: &mut Vec<SchedEvent>,
    ) {
        let target = self.slots[c].target.id.clone();
        self.slots[c].job = Some(job.job_id.clone());
        self.slots[c].sent_manifest = Some(
            job.manifest_dir
                .join(MANIFEST_NAME)
                .to_string_lossy()
                .into_owned(),
        );
        self.jobs[j].sent = Some(Arc::new(job.clone()));
        self.jobs[j].dispatched_at = Some(Instant::now());
        self.jobs[j].last_activity = Some(Instant::now());
        self.set(j, JobState::Dispatching { target }, ev);
        // Symlink/containment checks touch the filesystem (slow on a NAS): helper thread, result in `tick`.
        let (tx, rx) = channel();
        let local = self.jobs[j].job.clone();
        let hold = self.preflight_hold.clone();
        std::thread::spawn(move || {
            while hold
                .as_ref()
                .is_some_and(|h| h.load(std::sync::atomic::Ordering::SeqCst))
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(local.check_disk());
        });
        self.slots[c].preflight = Some((rx, job, files));
    }

    fn send_job(&mut self, c: usize, job: &KeyJob, files: &ModelFiles, ev: &mut Vec<SchedEvent>) {
        let sent = self.slots[c]
            .conn
            .as_mut()
            .map(|conn| conn.send(&Cmd::Key(job, files)));
        if !matches!(sent, Some(Ok(()))) {
            self.lose(c, "could not send the job to the worker".into(), ev);
        }
    }

    /// Tasks run on the local worker only. `Err` = never eligible, `Ok(None)` = not ready yet. The slot comes with
    /// the exact model files the task carries (from the app's snapshot; empty for kinds that load no model).
    fn task_slot(&self, job: &TaskJob) -> Result<Option<(usize, ModelFiles)>, String> {
        let kind = job.kind;
        // These kinds load a model (and need the GPU); the others (Chroma, pre-matte, extract, import, export) never
        // look at the model state, so a failed or unread model check cannot stop them.
        let needs_gpu = matches!(kind, TaskKind::Gvm | TaskKind::Birefnet | TaskKind::SamPreview | TaskKind::SamTrack | TaskKind::Matanyone2 | TaskKind::Videomama);
        let files = if !needs_gpu {
            ModelFiles::new()
        } else {
            match &self.models {
                ModelState::Unknown => return Ok(None),
                ModelState::Unavailable(why) => return Err(why.clone()),
                ModelState::Known(m) => m.task_files(kind, &job.params)?,
            }
        };
        if self.model_held(&files) {
            return Ok(None);
        }
        let mut pending = None;
        let mut reasons = vec![];
        for &c in &self.order {
            let s = &self.slots[c];
            if !s.target.enabled {
                continue;
            }
            match &s.status {
                TargetStatus::Down(_) if s.conn.is_some() => pending = Some(c),
                TargetStatus::Refused(r) | TargetStatus::Down(r) => reasons.push(r.clone()),
                TargetStatus::Stopping | TargetStatus::Stopped => reasons.push("stopped".into()),
                TargetStatus::Fresh | TargetStatus::Starting => pending = Some(c),
                TargetStatus::Ready => {
                    let Some(r) = s.ready.as_ref() else {
                        pending = Some(c);
                        continue;
                    };
                    if needs_gpu && r.gpu.is_none() {
                        reasons.push(format!("{} reports no usable GPU", s.target.id));
                    } else if s.job.is_none() {
                        return Ok(Some((c, files)));
                    } else {
                        pending = Some(c);
                    }
                }
            }
        }
        match pending {
            Some(c)
                if self.slots[c].job.is_none() && self.slots[c].status == TargetStatus::Ready =>
            {
                Ok(Some((c, files)))
            }
            Some(_) => Ok(None),
            None if reasons.is_empty() => Err("no enabled local worker".into()),
            None => Err(reasons.join("; ")),
        }
    }

    fn dispatch_tasks(&mut self, ev: &mut Vec<SchedEvent>) {
        if self.models_settling || self.locks.iter().any(|l| l.result.is_some()) {
            return;
        }
        for t in 0..self.tasks.len() {
            if self.tasks[t].state != JobState::Queued || self.held_out(&self.tasks[t].job.job_id) {
                continue;
            }
            let mut wait = false;
            for d in self.tasks[t].after.clone() {
                let st = self.state_of(&d).cloned().unwrap_or(JobState::Cancelled);
                if st.completed() {
                    continue;
                }
                if st.is_terminal() {
                    self.tasks[t].state = JobState::Failed {
                        reason: self.dependency_reason(&d),
                    };
                    ev.push(SchedEvent::Changed(self.tasks[t].job.job_id.clone()));
                    break;
                }
                wait = true;
            }
            if wait || self.tasks[t].state != JobState::Queued {
                continue;
            }
            let jobc = self.tasks[t].job.clone();
            match self.task_slot(&jobc) {
                Err(reason) => {
                    self.tasks[t].state = JobState::Failed { reason };
                    ev.push(SchedEvent::Changed(self.tasks[t].job.job_id.clone()));
                }
                Ok(None) => {
                    if let Some(c) = self.order.iter().copied().find(|c| {
                        self.slots[*c].status == TargetStatus::Fresh
                    }) {
                        self.spawn_if_fresh(c);
                    }
                }
                Ok(Some((c, files))) => {
                    let job = self.tasks[t].job.clone();
                    self.slots[c].job = Some(job.job_id.clone());
                    self.tasks[t].last_activity = Some(Instant::now());
                    self.tasks[t].state = JobState::Dispatching {
                        target: self.slots[c].target.id.clone(),
                    };
                    ev.push(SchedEvent::Changed(job.job_id.clone()));
                    let sent = self.slots[c]
                        .conn
                        .as_mut()
                        .map(|conn| conn.send(&Cmd::Task(&job, &files)));
                    if !matches!(sent, Some(Ok(()))) {
                        self.lose(c, "could not send the task to the worker".into(), ev);
                    }
                }
            }
        }
    }

    fn handle_task(&mut self, i: usize, id: &str, event: Event, ev: &mut Vec<SchedEvent>) {
        let Some(t) = self.tasks.iter().position(|t| t.job.job_id == id) else {
            self.lose(i, format!("event for unknown job {id}"), ev);
            return;
        };
        self.tasks[t].last_activity = Some(Instant::now());
        let target = self.slots[i].target.id.clone();
        let st = self.tasks[t].state.clone();
        let accepted = matches!(st, JobState::Running { .. });
        let mut set = |me: &mut Self, s: JobState| {
            me.tasks[t].state = s;
            ev.push(SchedEvent::Changed(id.to_owned()));
        };
        match event {
            Event::Accepted { .. } if matches!(st, JobState::Dispatching { .. }) => set(
                self,
                JobState::Running {
                    target,
                    phase: "accepted".into(),
                    done: 0,
                    total: 0,
                },
            ),
            Event::Rejected { code, message, .. } if matches!(st, JobState::Dispatching { .. }) => {
                self.slots[i].job = None;
                set(
                    self,
                    JobState::Rejected {
                        target,
                        code,
                        message,
                    },
                )
            }
            Event::Progress {
                phase, done, total, ..
            } if accepted => set(
                self,
                JobState::Running {
                    target,
                    phase,
                    done,
                    total,
                },
            ),
            Event::Status { message, .. } if accepted => {
                if let JobState::Running { phase, .. } = &mut self.tasks[t].state {
                    *phase = message;
                }
                ev.push(SchedEvent::Changed(id.to_owned()))
            }
            Event::Warning { message, .. } if accepted => ev.push(SchedEvent::Warning {
                job_id: id.to_owned(),
                message,
            }),
            Event::Preview {
                frame_stem,
                width,
                height,
                mask_png,
                score,
                ..
            } if accepted => ev.push(SchedEvent::Preview {
                job_id: id.to_owned(),
                frame_stem,
                width,
                height,
                mask_png,
                score,
            }),
            Event::Done(info) if accepted => {
                self.slots[i].job = None;
                set(self, JobState::Done { target, info })
            }
            other => self.lose(i, format!("unexpected {other:?} in state {st:?}"), ev),
        }
    }

    /// Collect finished preflights: send the job, or fail it without ever having touched a worker.
    fn poll_preflight(&mut self, ev: &mut Vec<SchedEvent>) {
        for c in 0..self.slots.len() {
            let Some((rx, ..)) = &self.slots[c].preflight else {
                continue;
            };
            let res = match rx.try_recv() {
                Ok(r) => r.map_err(|e| format!("preflight: {e}")),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err("preflight: checker thread died".into()),
            };
            let (_, job, files) = self.slots[c].preflight.take().unwrap();
            match res {
                Ok(()) => self.send_job(c, &job, &files, ev),
                Err(reason) => {
                    self.slots[c].job = None;
                    if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == job.job_id) {
                        self.set(j, JobState::Failed { reason }, ev);
                    }
                }
            }
        }
    }
}
/// A stalled share must not stop the queue: the helper gets this long to answer.
const LOCK_HELPER_LIMIT: Duration = Duration::from_secs(10);

/// stdout of `cmd`, or an error when it does not finish within `limit` (it is killed) or cannot start.
fn output_within(cmd: &mut std::process::Command, limit: Duration) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not start the lock helper: {e}"))?;
    let mut pipe = child.stdout.take().ok_or("the lock helper has no output")?;
    let reader = std::thread::spawn(move || {
        let mut b = vec![];
        let _ = pipe.read_to_end(&mut b);
        b
    });
    let end = std::time::Instant::now() + limit;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(reader.join().unwrap_or_default()),
            Ok(None) if std::time::Instant::now() < end => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                // A helper stuck on a hard-mounted share cannot die until the share answers: reap it off this thread.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return Err("the clip's folder did not answer; nothing was changed".into());
            }
            Err(e) => return Err(format!("could not wait for the lock helper: {e}")),
        }
    }
}

/// Run one hint-lock request through the app-side helper. Blocking: call it from a background thread only.
fn run_lock_helper(target: &Target, request: &str) -> LockOutcome {
    let refused = |m: String| LockOutcome::Refused { code: "unavailable".into(), message: m, lock: None };
    let mut cmd = match target.one_shot("backend.hint_lock_cli", request) {
        Ok(c) => c,
        Err(e) => return refused(e),
    };
    let out = match output_within(&mut cmd, LOCK_HELPER_LIMIT) {
        Ok(o) => o,
        Err(e) => return refused(e),
    };
    let text = String::from_utf8_lossy(&out);
    let Some(v) = text.lines().rev().find_map(|l| serde_json::from_str::<Value>(l).ok()) else {
        return refused("the lock helper gave no answer; nothing was changed".into());
    };
    let text_of = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned();
    match v.get("event").and_then(Value::as_str) {
        Some("hint_lock") => LockOutcome::Inspected(v.get("lock").filter(|l| !l.is_null()).cloned()),
        Some("hint_lock_cleared") => LockOutcome::Cleared {
            recovered: v.get("recovered").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect(),
            warning: v.get("warning").and_then(Value::as_str).map(str::to_owned),
        },
        Some("hint_lock_refused") => LockOutcome::Refused { code: text_of("code"), message: text_of("message"), lock: v.get("lock").filter(|l| !l.is_null()).cloned() },
        Some("rejected") => LockOutcome::Refused { code: text_of("code"), message: text_of("message"), lock: None },
        _ => refused("the lock helper gave an unknown answer; nothing was changed".into()),
    }
}

/// Run one generated-mask plan/delete request (`backend/mask_delete_cli.py`, 10 s limit) on a background thread.
/// `Ok` = the helper's `mask_plan` / `masks_deleted` event; `Err` = why nothing was changed.
pub fn spawn_mask_helper(target: Target, request: String) -> Receiver<Result<Value, String>> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let _ = tx.send(run_mask_helper(&target, &request));
    });
    rx
}

fn run_mask_helper(target: &Target, request: &str) -> Result<Value, String> {
    let mut cmd = target.one_shot("backend.mask_delete_cli", request)?;
    let out = output_within(&mut cmd, LOCK_HELPER_LIMIT)?;
    let text = String::from_utf8_lossy(&out);
    let v = text
        .lines()
        .rev()
        .find_map(|l| serde_json::from_str::<Value>(l).ok())
        .ok_or("the delete helper gave no answer; nothing was changed")?;
    match v.get("event").and_then(Value::as_str) {
        Some("mask_plan" | "masks_deleted") => Ok(v),
        Some("rejected") => Err(v.get("message").and_then(Value::as_str).unwrap_or("refused").to_owned()),
        _ => Err("the delete helper gave an unknown answer; nothing was changed".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stalled_lock_helper_is_stopped_after_its_time_limit_and_a_quick_one_answers() {
        let started = Instant::now();
        let mut slow = std::process::Command::new("sleep");
        slow.arg("5");
        let err = output_within(&mut slow, Duration::from_millis(300)).unwrap_err();
        assert!(err.contains("did not answer"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(3), "killed at the limit, not after the sleep");
        // A helper that ignores TERM and whose child keeps the pipe open still gives the answer at the limit
        // (the kill and the reaping never block the caller; a D-state process cannot be simulated here).
        let started = Instant::now();
        let mut stuck = std::process::Command::new("sh");
        stuck.args(["-c", "trap '' TERM; sleep 30 & sleep 30; wait"]);
        assert!(output_within(&mut stuck, Duration::from_millis(300)).unwrap_err().contains("did not answer"));
        assert!(started.elapsed() < Duration::from_secs(3), "returned at the limit");
        let mut quick = std::process::Command::new("echo");
        quick.arg("hello");
        assert_eq!(output_within(&mut quick, Duration::from_secs(5)).unwrap(), b"hello\n");
    }
}
