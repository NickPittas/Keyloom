//! Authoritative job queue (`ai-implementation-brief.md` §6, §7.5). One job per target, whole clips only,
//! Auto = local first then list order. Workers are spawned lazily (or by `probe`) and stay alive across jobs.
//! `tick()` never blocks: call it from the UI loop.
//!
//! Safety rules:
//! - A job whose worker vanished is `Lost` and keeps fencing its manifest folder until the writer is
//!   *provably* gone (local child reaped) or the user confirms it. Only then are that job's own lock and temp
//!   files cleaned (`KeyJob::recover_writer_gone`). No heartbeat-based retry or lock stealing exists.
//! - A running inference is never killed automatically: silence is reported (`stalled`), the user decides
//!   (`cancel`, then `force_stop`).
//! - Model identity: the first local worker's hello digest is the trusted reference. Jobs that pin no digest
//!   run on local workers and, on remote workers, only if their digest equals the reference. Unknown digests
//!   need the explicit `allow_unverified` opt-in and are flagged on the job.
use crate::jobs::{JobError, KeyJob, Recovery, MANIFEST_NAME, PROTOCOL};
use crate::worker::{Cmd, Done, Event, Hello, Msg, Outcome, Target, WorkerConn};
use std::collections::BTreeMap;
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
    fn fences(&self) -> bool {
        !self.is_terminal()
            || matches!(
                self,
                JobState::Lost {
                    writer_stopped: false,
                    ..
                }
            )
    }
    fn completed(&self) -> bool {
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
    /// Running on a worker whose model digest could not be verified (show it).
    pub unverified_models: bool,
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
    /// Not started yet; spawned on demand or by `probe`.
    Fresh,
    /// Process spawned, waiting for `hello` (bounded by `hello_timeout`).
    Starting,
    Ready,
    /// Protocol/GPU mismatch. Sticky until `reset_target`.
    Refused(String),
    /// Spawn failure, exit, hello timeout or protocol violation. Sticky until `reset_target`.
    Down(String),
    /// Stop requested (stdin closed); the process has not exited yet. Real termination = `Stopped`.
    Stopping,
    /// The process has exited after a requested stop or `force_stop`.
    Stopped,
}

/// How a target's model digest relates to the trusted local reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// No hello yet.
    Unknown,
    /// This local worker is the reference.
    Reference,
    /// Digest equals the reference.
    Matches,
    /// Digest differs from the reference: jobs without a pinned digest are refused.
    Mismatch,
    /// Digest or reference missing: only runs with `allow_unverified`.
    Unverified,
}

struct Slot {
    target: Target,
    conn: Option<WorkerConn>,
    hello: Option<Hello>,
    status: TargetStatus,
    job: Option<String>,
    started: Instant,
    /// Manifest path exactly as the worker was told (to spot a `done` that names something else).
    sent_manifest: Option<String>,
    /// Local filesystem preflight running on a helper thread; the job is sent when it succeeds.
    preflight: Option<(Receiver<Result<(), JobError>>, KeyJob)>,
    /// Job that was lost on this slot while the process was still dying; proof is added when it exits.
    lost_job: Option<String>,
    /// Stop was requested (`stop_*`/`force_stop`): the final status is `Stopped` once the process exits.
    stopping: bool,
}

/// A lost job's writer counts as stopped only with proof: a local worker whose process has exited. On Windows
/// the venv `python.exe` is a launcher that starts the real interpreter, so the launcher exiting proves nothing
/// about the process tree: local Windows workers stay fenced until the user confirms. Remote: never proof.
pub fn writer_proven(target_is_local: bool, windows_host: bool) -> bool {
    target_is_local && !windows_host
}

#[derive(Clone, Debug)]
pub struct TargetView {
    pub id: String,
    pub local: bool,
    pub enabled: bool,
    pub status: TargetStatus,
    pub busy_with: Option<String>,
    pub hello: Option<Hello>,
    pub trust: Trust,
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
}

/// Can this job run, per the current target knowledge? For the UI's Submit gate; call `probe` first.
#[derive(Clone, Debug, PartialEq)]
pub enum Readiness {
    Ready {
        target: String,
        unverified: bool,
        busy: bool,
    },
    /// Some target may still become eligible (starting, or the local reference is not known yet).
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

type Digests = BTreeMap<String, Option<String>>;

pub struct Scheduler {
    jobs: Vec<JobEntry>,
    slots: Vec<Slot>,
    /// Auto order: local targets first, then list order.
    order: Vec<usize>,
    reference: Option<Digests>,
    reference_from: Option<String>,
    /// Accept workers whose model digests cannot be compared (never silently: see `unverified_models`).
    pub allow_unverified: bool,
    /// First start hashes the checkpoint, imports torch and initialises CUDA, which can take minutes on a NAS
    /// or under antivirus. A spawned worker that has not said hello by then is killed (no job is involved yet)
    /// and the target is `Down` with that reason until `reset_target`: never respawned automatically. Default
    /// 600 s; `TargetView::starting_for` shows the elapsed startup time. Expose it in settings if needed.
    pub hello_timeout: Duration,
    /// Test hook: while set to `true` the preflight helper thread waits, so a test can hold a preflight pending.
    #[doc(hidden)]
    pub preflight_hold: Option<Arc<std::sync::atomic::AtomicBool>>,
}

/// Outcome of testing one target for one job.
enum Cand {
    /// `wire` is the job as it will be sent; built only when asked (`deep`), never for a busy target.
    Go {
        wire: Option<Box<KeyJob>>,
        unverified: bool,
    },
    Wait(String),
    No(String),
}

fn digests(h: &Hello) -> Digests {
    h.models
        .iter()
        .map(|(k, m)| (k.clone(), m.sha256.clone()))
        .collect()
}

impl Scheduler {
    pub fn new(targets: Vec<Target>) -> Result<Self, String> {
        for (i, t) in targets.iter().enumerate() {
            t.validate()?;
            if targets[..i].iter().any(|o| o.id == t.id) {
                return Err(format!("duplicate target id {}", t.id));
            }
        }
        let mut order: Vec<usize> = (0..targets.len()).collect();
        order.sort_by_key(|i| !targets[*i].is_local());
        let slots = targets
            .into_iter()
            .map(|target| Slot {
                target,
                conn: None,
                hello: None,
                status: TargetStatus::Fresh,
                job: None,
                started: Instant::now(),
                sent_manifest: None,
                preflight: None,
                lost_job: None,
                stopping: false,
            })
            .collect();
        Ok(Self {
            jobs: vec![],
            slots,
            order,
            reference: None,
            reference_from: None,
            allow_unverified: false,
            hello_timeout: Duration::from_secs(600),
            preflight_hold: None,
        })
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
            if !self.jobs.iter().any(|e| &e.job.job_id == d) {
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
        self.jobs.push(JobEntry {
            job: Arc::new(s.job),
            placement: s.placement,
            after: s.after,
            min_vram_gb: s.min_vram_gb,
            state: JobState::Queued,
            unverified_models: false,
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
    pub fn job(&self, id: &str) -> Option<&JobEntry> {
        self.jobs.iter().find(|e| e.job.job_id == id)
    }
    /// Trusted model digests (from the first local worker that said hello), if any.
    pub fn reference(&self) -> Option<&BTreeMap<String, Option<String>>> {
        self.reference.as_ref()
    }

    pub fn targets(&self) -> Vec<TargetView> {
        self.slots
            .iter()
            .map(|s| TargetView {
                id: s.target.id.clone(),
                local: s.target.is_local(),
                enabled: s.target.enabled,
                status: s.status.clone(),
                busy_with: s.job.clone(),
                hello: s.hello.clone(),
                trust: self.trust(s),
                starting_for: (s.status == TargetStatus::Starting).then(|| s.started.elapsed()),
                startup_budget: self.hello_timeout,
            })
            .collect()
    }

    fn trust(&self, s: &Slot) -> Trust {
        let (Some(h), Some(r)) = (&s.hello, &self.reference) else {
            return Trust::Unknown;
        };
        if self.reference_from.as_deref() == Some(&s.target.id) {
            return Trust::Reference;
        }
        match (
            r.get("corridorkey_v2").cloned().flatten(),
            h.models
                .get("corridorkey_v2")
                .and_then(|m| m.sha256.clone()),
        ) {
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(&b) => Trust::Matches,
            (Some(_), Some(_)) => Trust::Mismatch,
            _ => Trust::Unverified,
        }
    }

    /// Start (not wait for) workers so hello, GPU and model digest are known before a job is submitted.
    /// `None` = every enabled Fresh target. Safe to call repeatedly.
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
            return;
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

    /// Explicit recovery: the user states the lost job's writer is gone. Releases its folder fence and cleans
    /// that job's own lock/temp files (the folder must be reachable locally, e.g. shared storage).
    pub fn confirm_writer_stopped(&mut self, id: &str) -> Option<Recovery> {
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
        s.hello = None;
        s.stopping = false;
        s.status = TargetStatus::Fresh;
        if self.reference_from.as_deref() == Some(id) {
            self.reference = None;
            self.reference_from = None;
        }
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
            unverified_models: false,
            cancel_requested: false,
            dispatched_at: None,
            accepted_at: None,
            last_activity: None,
            sent: None,
        };
        let (mut wait, mut reasons) = (None, vec![]);
        for c in self.candidates(placement) {
            match self.candidate(&e, c, true) {
                Cand::Go { unverified, .. } => {
                    let s = &self.slots[c];
                    return Readiness::Ready {
                        target: s.target.id.clone(),
                        unverified,
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
            if s.status == TargetStatus::Starting && s.started.elapsed() > self.hello_timeout {
                // No job is involved before hello, so killing here cannot interrupt inference.
                let why = format!("no hello within {} s (model hash, torch and CUDA start-up can be slow on network storage); press Retry to start it again or raise the start-up timeout", self.hello_timeout.as_secs());
                self.lose(i, why, &mut ev);
            }
        }
        self.poll_preflight(&mut ev);
        self.dispatch(&mut ev);
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
        s.hello = None;
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
        if let Some(id) = self.slots[i].job.take() {
            if let Some(j) = self.jobs.iter().position(|e| e.job.job_id == id) {
                self.slots[i].lost_job = Some(id);
                self.set(
                    j,
                    JobState::Lost {
                        target,
                        reason,
                        writer_stopped: false,
                        recovery: None,
                    },
                    ev,
                );
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
        let proven = writer_proven(s.target.is_local(), cfg!(windows));
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
                    Some(c) => format!("worker exited with code {c}"),
                    None => "worker terminated by signal".into(),
                };
                // Mark the connection gone first so `lose` finalises the exit immediately.
                self.slots[i].conn = None;
                self.lose(i, format!("{why}: {stderr}"), ev);
                return;
            }
            Msg::Event(e) => e,
        };
        if let Event::Hello(h) = event {
            return self.hello(i, h, ev);
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
        let j = self.jobs.iter().position(|e| e.job.job_id == id).unwrap();
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

    fn hello(&mut self, i: usize, h: Hello, ev: &mut Vec<SchedEvent>) {
        if !matches!(
            self.slots[i].status,
            TargetStatus::Starting | TargetStatus::Ready
        ) {
            return; // a dying/refused/stopped worker's hello means nothing
        }
        if self.slots[i].hello.is_some() {
            self.lose(i, "duplicate hello".into(), ev);
            return;
        }
        let s = &mut self.slots[i];
        let bad = if h.protocol != PROTOCOL {
            Some(format!("worker speaks protocol {}", h.protocol))
        } else {
            h.gpu.as_ref().filter(|g| g.index != s.target.gpu).map(|g| {
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
        if s.target.is_local() && self.reference.is_none() {
            self.reference = Some(digests(&h));
            self.reference_from = Some(s.target.id.clone());
        }
        s.hello = Some(h);
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
            TargetStatus::Refused(r) | TargetStatus::Down(r) => return Cand::No(r.clone()),
            TargetStatus::Stopping | TargetStatus::Stopped => return Cand::No("stopped".into()),
            TargetStatus::Fresh | TargetStatus::Starting => {
                return Cand::Wait(format!("{} is starting", s.target.id))
            }
            TargetStatus::Ready => {}
        }
        let Some(h) = s.hello.as_ref() else {
            return Cand::Wait("worker not ready".into());
        };
        // A Key job needs a GPU and the capability, whatever the VRAM estimate says.
        let Some(gpu) = h.gpu.as_ref() else {
            return Cand::No("worker reports no GPU".into());
        };
        if !h.capabilities.key || !h.capabilities.publish_modes.contains(&e.job.publish) {
            return Cand::No("worker cannot key with publish=final".into());
        }
        if let Some(need) = e.min_vram_gb.filter(|n| gpu.vram_gb < *n) {
            return Cand::No(format!(
                "{:.1} GB VRAM, job needs {need:.1} GB",
                gpu.vram_gb
            ));
        }
        // Identity. A pinned digest must match. An unpinned one means "the trusted local reference": local
        // workers are trusted even when the reference carries no digest; remote workers must match it.
        let mut sent = e.job.require.clone();
        let mut check = e.job.require.clone();
        for (name, want) in sent.models.iter_mut() {
            if want.is_none() {
                match &self.reference {
                    Some(r) => *want = r.get(name).cloned().flatten(),
                    None if !s.target.is_local()
                        && self.slots.iter().any(|l| {
                            l.target.is_local()
                                && l.target.enabled
                                && matches!(l.status, TargetStatus::Fresh | TargetStatus::Starting)
                        }) =>
                    {
                        return Cand::Wait("waiting for the local worker's model digest".into());
                    }
                    None => {}
                }
                if want.is_none() && s.target.is_local() && h.models.contains_key(name) {
                    check.models.remove(name); // trusted local, no digest anywhere
                    continue;
                }
            }
            check.models.insert(name.clone(), want.clone());
        }
        let unverified = match h.check_identity(&check, self.allow_unverified) {
            Ok(u) => u,
            Err(r) => return Cand::No(r),
        };
        // Pure in-memory from here on: the local filesystem preflight happens at dispatch, off the UI thread.
        let wire = if !deep {
            None
        } else if s.target.is_local() {
            let mut w = (*e.job).clone();
            w.require = sent;
            Some(Box::new(w))
        } else {
            match e.job.mapped(&s.target.path_map) {
                Ok(mut w) => {
                    w.require = sent;
                    Some(Box::new(w))
                }
                Err(r) => return Cand::No(r),
            }
        };
        Cand::Go { wire, unverified }
    }

    fn dispatch(&mut self, ev: &mut Vec<SchedEvent>) {
        if self.reference.is_none() && self.jobs.iter().any(|e| e.state == JobState::Queued) {
            // Remote jobs need the local digest as reference: bring up the first local worker.
            if let Some(c) = self
                .order
                .iter()
                .copied()
                .find(|c| self.slots[*c].target.is_local())
            {
                self.spawn_if_fresh(c);
            }
        }
        let mut reserved: Vec<usize> = vec![];
        for j in 0..self.jobs.len() {
            if self.jobs[j].state != JobState::Queued {
                continue;
            }
            let mut wait = false;
            for d in self.jobs[j].after.clone() {
                let st = &self.job(&d).unwrap().state;
                if st.completed() {
                    continue;
                }
                if st.is_terminal() {
                    let reason = format!("dependency {d} did not complete");
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
                        unverified,
                    } if idle => {
                        chosen = Some((c, *wire, unverified));
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
            if let Some((c, job, unverified)) = chosen {
                self.start(j, c, job, unverified, ev);
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
        unverified: bool,
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
        self.jobs[j].unverified_models = unverified;
        self.jobs[j].sent = Some(Arc::new(job.clone()));
        self.jobs[j].dispatched_at = Some(Instant::now());
        self.jobs[j].last_activity = Some(Instant::now());
        self.set(j, JobState::Dispatching { target }, ev);
        if self.slots[c].target.is_local() {
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
            self.slots[c].preflight = Some((rx, job));
        } else {
            self.send_job(c, &job, ev);
        }
    }

    fn send_job(&mut self, c: usize, job: &KeyJob, ev: &mut Vec<SchedEvent>) {
        let sent = self.slots[c]
            .conn
            .as_mut()
            .map(|conn| conn.send(&Cmd::Key(job)));
        if !matches!(sent, Some(Ok(()))) {
            self.lose(c, "could not send the job to the worker".into(), ev);
        }
    }

    /// Collect finished preflights: send the job, or fail it without ever having touched a worker.
    fn poll_preflight(&mut self, ev: &mut Vec<SchedEvent>) {
        for c in 0..self.slots.len() {
            let Some((rx, _)) = &self.slots[c].preflight else {
                continue;
            };
            let res = match rx.try_recv() {
                Ok(r) => r.map_err(|e| format!("preflight: {e}")),
                Err(TryRecvError::Empty) => continue,
                Err(TryRecvError::Disconnected) => Err("preflight: checker thread died".into()),
            };
            let (_, job) = self.slots[c].preflight.take().unwrap();
            match res {
                Ok(()) => self.send_job(c, &job, ev),
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
