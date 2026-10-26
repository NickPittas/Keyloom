//! Task side of `AiUi`: hint generation, SAM preview/tracking and video export share the Key scheduler (one GPU
//! job per worker). Results are surfaced from the worker's own events; nothing is fabricated here. A preview is
//! tied to the prompt signature it was requested with, so an edit while it ran can never be shown as current.
use super::*;
use crate::tasks::{TaskJob, TaskKind};
use crate::worker::{LockCmd, Outcome};

#[derive(Clone, Debug)]
pub struct PreviewResult {
    pub job_id: String,
    pub folder: String,
    pub frame_stem: String,
    pub mask_png: PathBuf,
    pub width: u32,
    pub height: u32,
    /// Prompt/model signature when requested.
    pub sig: u64,
}

#[derive(Clone, Debug)]
pub struct TaskDone {
    pub job_id: String,
    pub folder: String,
    pub kind: TaskKind,
    pub outcome: Outcome,
    pub message: Option<String>,
    pub result: Option<Value>,
}

#[derive(Default)]
pub struct TaskState {
    /// Request signature per task id (previews).
    pub sigs: BTreeMap<String, u64>,
    pub previews: BTreeMap<String, PreviewResult>,
    pub done: Vec<TaskDone>,
    /// Something durable changed (queue snapshot to write).
    pub dirty: bool,
    /// Hint-lock cleanup outcome per task id ("lock removed", or why it was left).
    pub lock_notes: BTreeMap<String, String>,
    /// Lock files seen per clip folder, re-read at most once a second (a NAS stat is slow).
    locks: BTreeMap<String, (Instant, Result<Option<crate::hints::HintLock>, String>)>,
    /// Result of the last explicit orphan release (shown once).
    pub orphan_note: Option<String>,
    pub external: std::collections::BTreeSet<PathBuf>,
    /// Outstanding worker lock requests and what to do with the answer.
    pub purposes: BTreeMap<String, Purpose>,
    /// The worker's facts about each clip's lock.
    pub views: BTreeMap<String, LockView>,
    /// Durable queue history of the open project (older sessions' entries + this session's, see `queue`).
    pub history: Vec<crate::queue::Entry>,
    pub history_root: Option<PathBuf>,
    last_saved: String,
    stamps: BTreeMap<String, (String, u64)>,
    /// A held Key entry the user chose to re-queue: the host opens the normal review for this clip folder.
    pub requeue: Option<String>,
}

pub enum Purpose {
    View {
        folder: String,
    },
    Auto {
        task: String,
        folder: String,
        reason: &'static str,
    },
    Clear {
        folder: Option<String>,
        task: Option<String>,
    },
}

pub struct LockView {
    pub at: Instant,
    pub info: Option<Value>,
    pub pending: bool,
}

/// `(job_id, host, pid)` of a readable lock inspection.
fn expect_of(v: &Value) -> Option<(String, String, u64)> {
    if v.get("readable").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    Some((
        v.get("job_id")?.as_str()?.to_owned(),
        v.get("host")?.as_str()?.to_owned(),
        v.get("pid")?.as_u64()?,
    ))
}

impl AiUi {
    /// Queue one task on the local worker. `Err` = nothing was queued.
    pub fn submit_task(
        &mut self,
        clip: &Clip,
        job: TaskJob,
        after: Vec<String>,
        sig: u64,
    ) -> Result<String, String> {
        if self.model_mutation_active {
            return Err("Model installation or repair is active; wait for it to finish before starting work.".into());
        }
        if self.external_writer(&clip.root) {
            return Err(
                "a companion-sequence import is writing this clip's hints; wait for it to finish"
                    .into(),
            );
        }
        let (id, kind) = (job.job_id.clone(), job.kind);
        self.sched
            .submit_task(job, after)
            .map_err(|e| self.submit_error_text(clip, &e))?;
        self.tracked.push(Tracked {
            warnings: vec![],
            job_id: id.clone(),
            folder: clip.folder.clone(),
            name: clip.display_name.clone(),
            label: kind.label().into(),
        });
        self.tk.sigs.insert(id.clone(), sig);
        self.tk.dirty = true;
        self.disk_gen += 1;
        Ok(id)
    }

    /// Hint steps then an optional Key job, each depending on the one before. All-or-nothing: if a later
    /// step is refused the earlier ones are cancelled again before anything ran.
    pub fn submit_chain(
        &mut self,
        clip: &Clip,
        steps: Vec<TaskJob>,
        key: Option<(Plan, Params, OutputCfg)>,
    ) -> Result<String, String> {
        let mut ids: Vec<String> = vec![];
        let run = || -> Result<(), String> {
            for s in steps {
                let after = ids.last().cloned().into_iter().collect();
                let id = self.submit_task(clip, s, after, 0)?;
                ids.push(id);
            }
            if let Some((plan, params, out)) = key {
                let after: Vec<String> = ids.last().cloned().into_iter().collect();
                let id = self.submit_plan_after(clip, plan, &params, &out, after)?;
                ids.push(id);
            }
            Ok(())
        };
        match run() {
            Ok(()) => ids.pop().ok_or_else(|| "nothing to queue".into()),
            Err(e) => {
                for id in &ids {
                    self.sched.cancel(id);
                }
                Err(e)
            }
        }
    }

    /// What the local worker reports (`None` until its hello arrived).
    pub fn local_caps(&self) -> Option<crate::worker::Capabilities> {
        self.sched
            .targets()
            .into_iter()
            .find(|t| t.local && t.enabled && t.hello.is_some())
            .and_then(|t| t.hello.map(|h| h.capabilities))
    }
    /// Start the local worker now so capabilities are known before the user asks for work.
    pub fn probe_workers(&mut self) {
        if !self.model_mutation_active {
            self.sched.probe(None);
        }
    }

    pub fn task_status_row(&mut self, ui: &mut Ui, id: &str) {
        self.task_row(ui, id, false);
    }

    /// A non-worker writer (the companion-sequence import) owns this clip's hint folders: no hint job and no
    /// Key may be queued for it until it ends.
    /// Keyed by the clip's absolute root, so an import under one project never fences another project's
    /// clip of the same name, and a project switch cannot lose track of it.
    pub fn external_writer(&self, clip_root: &Path) -> bool {
        self.tk.external.contains(clip_root)
    }
    pub fn set_external_writer(&mut self, clip_root: &Path, on: bool) {
        if on {
            self.tk.external.insert(clip_root.to_path_buf());
        } else {
            self.tk.external.remove(clip_root);
        }
    }

    /// Any Key job OR task of this clip is queued or running (`task_busy` counts tasks only).
    pub fn clip_active(&self, clip_root: &Path) -> bool {
        self.sched.snapshot().iter().any(|e| e.job.clip_root == clip_root && e.state.fences())
            || self.sched.task_snapshot().iter().any(|t| t.job.clip_root == clip_root && t.state.fences())
    }

    pub fn task_busy(&self, folder: &str) -> bool {
        self.tracked.iter().any(|t| {
            t.folder == folder
                && !t.label.is_empty()
                && self
                    .sched
                    .state_of(&t.job_id)
                    .is_some_and(|s| !s.is_terminal())
        })
    }

    /// The newest finished/ongoing task of `kind` for this clip that publishes into `dir` (`AlphaHint` or
    /// `VideoMamaMaskHint`). One kind (SAM tracking, a producer) can publish into either, so the column a job
    /// belongs to is its actual `publish_dir`, not just its kind.
    pub fn latest_task_in(&self, folder: &str, kind: TaskKind, dir: &str) -> Option<String> {
        self.tracked
            .iter()
            .rev()
            .find(|t| {
                t.folder == folder
                    && self.sched.task(&t.job_id).is_some_and(|e| {
                        e.job.kind == kind
                            && e.job
                                .publish_dir
                                .as_deref()
                                .is_some_and(|p| p.file_name().is_some_and(|n| n == dir))
                    })
            })
            .map(|t| t.job_id.clone())
    }

    /// The newest finished/ongoing task of `kind` for this clip.
    pub fn latest_task(&self, folder: &str, kind: TaskKind) -> Option<String> {
        self.tracked
            .iter()
            .rev()
            .find(|t| {
                t.folder == folder
                    && self
                        .sched
                        .task(&t.job_id)
                        .is_some_and(|e| e.job.kind == kind)
            })
            .map(|t| t.job_id.clone())
    }

    /// Preview for `frame_stem` that matches the CURRENT prompt signature, else none (stale results are
    /// discarded, never shown as the current selection).
    pub fn current_preview(
        &self,
        folder: &str,
        frame_stem: &str,
        sig: u64,
    ) -> Option<&PreviewResult> {
        self.tk
            .previews
            .values()
            .filter(|p| p.folder == folder && p.frame_stem == frame_stem && p.sig == sig)
            .last()
    }

    pub(super) fn on_preview(
        &mut self,
        id: String,
        stem: String,
        w: u32,
        h: u32,
        png: String,
    ) {
        let Some(t) = self.tracked.iter().find(|t| t.job_id == id) else {
            return;
        };
        let sig = self.tk.sigs.get(&id).copied().unwrap_or(0);
        // A newer preview for this clip supersedes (and deletes the files of) the older ones.
        let folder = t.folder.clone();
        let old: Vec<String> = self
            .tk
            .previews
            .values()
            .filter(|p| p.folder == folder)
            .map(|p| p.job_id.clone())
            .collect();
        for o in old {
            if let Some(p) = self.tk.previews.remove(&o) {
                p.mask_png.parent().map(drop_staging);
            }
        }
        self.tk.previews.insert(
            id.clone(),
            PreviewResult {
                job_id: id,
                folder: t.folder.clone(),
                frame_stem: stem,
                mask_png: PathBuf::from(png),
                width: w,
                height: h,
                sig,
            },
        );
    }

    pub(super) fn on_task_end(&mut self, id: &str) {
        let (Some(e), Some(t)) = (
            self.sched.task(id),
            self.tracked.iter().find(|t| t.job_id == id),
        ) else {
            return;
        };
        let (outcome, message, result) = match &e.state {
            JobState::Done { info, .. } => {
                (info.outcome, info.message.clone(), info.result.clone())
            }
            JobState::Rejected { message, .. } => (Outcome::Failed, Some(message.clone()), None),
            JobState::Failed { reason } => (Outcome::Failed, Some(reason.clone()), None),
            JobState::Cancelled => (Outcome::Cancelled, None, None),
            JobState::Lost { reason, .. } => (Outcome::Failed, Some(reason.clone()), None),
            _ => return,
        };
        let d = TaskDone {
            job_id: id.to_owned(),
            folder: t.folder.clone(),
            kind: e.job.kind,
            outcome,
            message,
            result,
        };
        // The job's app-owned staging is garbage once it ended for good (previews keep theirs until superseded).
        let ended = !matches!(
            e.state,
            JobState::Lost {
                writer_stopped: false,
                ..
            }
        );
        if let (true, true, Some(st)) = (
            ended,
            e.job.kind != TaskKind::SamPreview,
            e.job.staging_dir.clone(),
        ) {
            drop_staging(&st);
        }
        self.tk.done.retain(|x| x.job_id != id);
        self.tk.done.push(d);
        self.tk.dirty = true;
        self.cleanup_task_lock(id, true);
    }

    /// A Lost hint writer that is now PROVEN gone (`observed`: its local process exit was seen; else the user
    /// confirmed it) leaves its `.corridorkey_hint.lock`. Ownership-safe release is the worker's job: inspect
    /// the lock, and only if it names exactly this job, ask the worker to clear it with that job/host/pid and
    /// the matching reason (it also restores an interrupted `.prev` publication). Nothing is unlinked here.
    pub(super) fn cleanup_task_lock(&mut self, id: &str, observed: bool) {
        let Some(e) = self.sched.task(id) else {
            return;
        };
        if !e.job.kind.writes_hint() {
            return;
        }
        let proven = matches!(
            e.state,
            JobState::Lost {
                writer_stopped: true,
                ..
            }
        );
        if !proven {
            return; // an ended job's own worker released the lock before `done`
        }
        let (root, folder) = (
            e.job.clip_root.clone(),
            self.tracked
                .iter()
                .find(|t| t.job_id == id)
                .map(|t| t.folder.clone())
                .unwrap_or_default(),
        );
        let reason = if observed {
            "observed_exit"
        } else {
            "user_confirmed_orphan"
        };
        match self
            .sched
            .lock_request(LockCmd::Inspect { clip_root: root })
        {
            Ok(rid) => {
                self.tk.purposes.insert(
                    rid,
                    Purpose::Auto {
                        task: id.to_owned(),
                        folder,
                        reason,
                    },
                );
                self.tk.lock_notes.insert(
                    id.to_owned(),
                    "releasing the hint lock through the worker…".into(),
                );
            }
            Err(m) => {
                self.tk
                    .lock_notes
                    .insert(id.to_owned(), format!("hint lock left in place: {m}"));
            }
        }
        self.tk.dirty = true;
        self.disk_gen += 1;
    }

    /// Worker answers to lock requests. Returns clip folders whose hint files may have changed (restored).
    pub(super) fn on_lock_reply(&mut self, rid: String, outcome: LockOutcome) -> Option<String> {
        let purpose = self.tk.purposes.remove(&rid)?;
        match (purpose, outcome) {
            (Purpose::View { folder }, LockOutcome::Inspected(info)) => {
                self.tk.views.insert(
                    folder,
                    LockView {
                        at: Instant::now(),
                        info,
                        pending: false,
                    },
                );
                None
            }
            (Purpose::View { folder }, LockOutcome::Refused { message, .. }) => {
                self.tk.views.insert(
                    folder,
                    LockView {
                        at: Instant::now(),
                        info: None,
                        pending: false,
                    },
                );
                self.tk.orphan_note = Some(format!("Lock inspection unavailable: {message}"));
                None
            }
            (
                Purpose::Auto {
                    task,
                    folder,
                    reason,
                },
                LockOutcome::Inspected(info),
            ) => {
                let note = match info.as_ref().and_then(expect_of) {
                    None if info.is_none() => Some("no hint lock to release".to_string()),
                    None => Some(
                        "hint lock is unreadable or names no writer; left in place".to_string(),
                    ),
                    Some((job, host, pid)) if job == task => {
                        let root = self.sched.task(&task).map(|t| t.job.clip_root.clone());
                        match root.map(|r| {
                            self.sched.lock_request(LockCmd::Clear {
                                clip_root: r,
                                job_id: job,
                                host,
                                pid,
                                reason,
                            })
                        }) {
                            Some(Ok(rid2)) => {
                                self.tk.purposes.insert(
                                    rid2,
                                    Purpose::Clear {
                                        folder: Some(folder),
                                        task: Some(task.clone()),
                                    },
                                );
                                None
                            }
                            _ => Some("hint lock left in place: worker unavailable".into()),
                        }
                    }
                    Some((job, ..)) => Some(format!(
                        "hint lock now belongs to {job}, not {task}; left in place"
                    )),
                };
                if let Some(n) = note {
                    self.tk.lock_notes.insert(task, n);
                }
                None
            }
            (Purpose::Clear { folder, task }, out) => {
                let msg = match &out {
                    LockOutcome::Cleared { recovered, warning } => format!(
                        "hint lock released by the worker{}{}",
                        if recovered.is_empty() {
                            String::new()
                        } else {
                            format!("; restored {} interrupted hint folder(s)", recovered.len())
                        },
                        warning
                            .as_ref()
                            .map_or(String::new(), |w| format!(" ({w})"))
                    ),
                    LockOutcome::Refused { code, message, .. } => {
                        format!("hint lock left in place ({code}): {message}")
                    }
                    LockOutcome::Inspected(_) => "unexpected reply".into(),
                };
                match task {
                    Some(t) => {
                        self.tk.lock_notes.insert(t, msg);
                    }
                    None => self.tk.orphan_note = Some(msg),
                }
                if let Some(f) = &folder {
                    self.tk.views.remove(f);
                }
                self.disk_gen += 1;
                folder.filter(|_| matches!(out, LockOutcome::Cleared { .. }))
            }
            _ => None,
        }
    }

    /// The hint lock of this clip. A lock file is only ever DETECTED here (read-only); facts come from the
    /// worker's inspection and release goes through its `clear_hint_lock` (exact job id + host + pid, liveness
    /// proof or the user's explicit orphan confirmation). A lock owned by a job of this session is released
    /// with that job's own proof instead.
    pub fn hint_lock_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        let now = Instant::now();
        let stale = self
            .tk
            .locks
            .get(&clip.folder)
            .is_none_or(|(t, _)| now.duration_since(*t) > Duration::from_secs(1));
        if stale {
            self.tk.locks.insert(
                clip.folder.clone(),
                (now, crate::hints::read_hint_lock(&clip.root)),
            );
        }
        let Some((_, state)) = self.tk.locks.get(&clip.folder).cloned() else {
            return;
        };
        let lock = match state {
            Ok(None) => {
                self.tk.views.remove(&clip.folder);
                return;
            }
            Err(e) => {
                ui.colored_label(BAD, format!("Hint lock problem: {e}. Nothing was touched; remove it by hand once you are sure no hint job is writing."));
                return;
            }
            Ok(Some(l)) => l,
        };
        let known = lock
            .job_id
            .as_deref()
            .and_then(|j| self.sched.state_of(j).cloned());
        if let (Some(j), Some(st)) = (&lock.job_id, known) {
            if matches!(st, JobState::Dispatching { .. } | JobState::Running { .. }) {
                return; // The normal task row already shows this writer's progress and Cancel control.
            }
            let t = if st.is_terminal()
                && !matches!(
                    st,
                    JobState::Lost {
                        writer_stopped: false,
                        ..
                    }
                ) {
                "that job has ended; its lock is released with its proof (see its row)"
            } else {
                "owned by a job of this session: use its Cancel / Confirm controls"
            };
            ui.label(
                RichText::new(format!("Hint lock held by job {j}: {t}."))
                    .color(WARN)
                    .size(12.0),
            );
            return;
        }
        // A lock this very app holds for a running companion-sequence import: never offered for release.
        if self.external_writer(&clip.root) {
            ui.label(RichText::new("Hint lock held by this app's running sequence import; it is released when the import ends.").color(WARN).size(12.0));
            return;
        }
        // Not a job of this session: ask the worker for the facts once, then offer explicit release.
        let need = self
            .tk
            .views
            .get(&clip.folder)
            .is_none_or(|v| !v.pending && now.duration_since(v.at) > Duration::from_secs(5));
        if need {
            if let Ok(rid) = self.sched.lock_request(LockCmd::Inspect {
                clip_root: clip.root.clone(),
            }) {
                self.tk.purposes.insert(
                    rid,
                    Purpose::View {
                        folder: clip.folder.clone(),
                    },
                );
                self.tk.views.insert(
                    clip.folder.clone(),
                    LockView {
                        at: now,
                        info: self.tk.views.get(&clip.folder).and_then(|v| v.info.clone()),
                        pending: true,
                    },
                );
            }
        }
        let info = self.tk.views.get(&clip.folder).and_then(|v| v.info.clone());
        let Some(info) = info else {
            ui.label(
                RichText::new("A hint lock exists; asking the worker who owns it…")
                    .color(WARN)
                    .size(12.0),
            );
            return;
        };
        let Some((job, host, pid)) = expect_of(&info) else {
            let why = info
                .get("problem")
                .and_then(Value::as_str)
                .unwrap_or("it names no writer");
            ui.colored_label(BAD, format!("{} is unusable ({why}); left in place. Remove it by hand once you are sure no hint job is writing.", lock.path.display()));
            return;
        };
        let b = |k: &str| info.get(k).and_then(Value::as_bool);
        ui.colored_label(WARN, format!(
            "A hint job {job} from before this session left a lock (host {host}, pid {pid}{}). It is released only after the worker verifies ownership.",
            match b("local_dead") { Some(true) => ", its process has exited on this machine", Some(false) => ", its process is STILL RUNNING on this machine", None => ", it ran on another machine or its state is unknown" }
        ));
        let root = clip.root.clone();
        let mut reason = None;
        if b("safe_to_clear") == Some(true)
            && ui
                .button(format!("Clear lock of exited job {job}"))
                .clicked()
        {
            reason = Some("observed_exit");
        }
        if b("local_dead") != Some(false)
            && ui
                .button(format!("I confirm job {job}'s writer is gone — clear lock"))
                .clicked()
        {
            reason = Some("user_confirmed_orphan");
        }
        if let Some(reason) = reason {
            match self.sched.lock_request(LockCmd::Clear {
                clip_root: root,
                job_id: job,
                host,
                pid,
                reason,
            }) {
                Ok(rid) => {
                    self.tk.purposes.insert(
                        rid,
                        Purpose::Clear {
                            folder: Some(clip.folder.clone()),
                            task: None,
                        },
                    );
                }
                Err(m) => self.tk.orphan_note = Some(format!("Lock left in place: {m}")),
            }
        }
        if let Some(n) = &self.tk.orphan_note {
            ui.label(RichText::new(n).size(12.0));
        }
    }

    pub(super) fn task_row(&mut self, ui: &mut Ui, id: &str, with_name: bool) {
        let Some(e) = self.sched.task(id).cloned() else {
            return;
        };
        let name = self
            .tracked
            .iter()
            .find(|t| t.job_id == id)
            .map_or(String::new(), |t| t.name.clone());
        let (mut cancel, mut confirm) = (false, false);
        ui.horizontal_wrapped(|ui| {
            if with_name {
                ui.label(RichText::new(&name).strong());
            }
            ui.label(RichText::new(e.job.kind.label()).color(DIM));
            match &e.state {
                JobState::Queued => {
                    ui.label(if e.after.is_empty() {
                        "Queued".to_string()
                    } else {
                        "Waiting for the previous step".to_string()
                    });
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Dispatching { target } => {
                    ui.label(format!("Starting on {target}…"));
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Running {
                    phase, done, total, ..
                } => {
                    ui.label(trunc(phase));
                    if *total > 0 {
                        let f = (*done).min(*total) as f32 / *total as f32;
                        ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!(
                            "{}/{}",
                            (*done).min(*total),
                            total
                        )));
                    }
                    if e.cancel_requested {
                        ui.label(RichText::new("cancel requested").color(WARN));
                    } else {
                        cancel = ui.small_button("Cancel").clicked();
                    }
                }
                JobState::Done { info, .. } => match info.outcome {
                    Outcome::Complete => {
                        let r = info.result.as_ref();
                        let n = r
                            .and_then(|r| r.get("published"))
                            .and_then(Value::as_u64)
                            .unwrap_or(info.published);
                        ui.label(format!("Complete: {n} written"));
                    }
                    Outcome::Cancelled => {
                        ui.label("Cancelled (previous result kept)");
                    }
                    Outcome::Failed => {
                        ui.colored_label(BAD, trunc(info.message.as_deref().unwrap_or("Failed")));
                    }
                },
                JobState::Rejected { code, message, .. } => {
                    ui.colored_label(BAD, format!("Not started ({code}): {}", trunc(message)));
                }
                JobState::Failed { reason } => {
                    ui.colored_label(BAD, format!("Not started: {}", trunc(reason)));
                }
                JobState::Cancelled => {
                    ui.label("Cancelled before it started");
                }
                JobState::Lost {
                    reason,
                    writer_stopped,
                    ..
                } => {
                    ui.colored_label(BAD, format!("Lost: {}", trunc(reason)));
                    if *writer_stopped {
                        ui.label("Writer stopped; the previous hint is unchanged.");
                    } else {
                        ui.colored_label(WARN, "The worker may still be writing.");
                        confirm = ui.small_button("I confirm the worker is stopped").clicked();
                    }
                }
            }
        });
        if cancel {
            self.cancel(id);
        }
        if confirm {
            self.sched.confirm_writer_stopped(id);
            self.cleanup_task_lock(id, false);
            self.disk_gen += 1;
        }
        if let Some(n) = self.tk.lock_notes.get(id) {
            ui.label(RichText::new(n).color(DIM).size(12.0));
        }
    }
}

/// Stable signature of anything that decides a preview's validity.
pub fn sig_of(parts: &[&str]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    parts.hash(&mut h);
    h.finish()
}

/// Remove one job's staging folder `<...>/.ck_staging/<clip>/<job>`; refuses anything that is not under a
/// `.ck_staging` folder (never user data) and never follows a symlink.
pub fn drop_staging(dir: &Path) {
    let under = dir
        .components()
        .any(|c| c.as_os_str().to_string_lossy().contains(".ck_staging"));
    let plain = std::fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir());
    if under && plain {
        let _ = std::fs::remove_dir_all(dir);
    }
}

fn now_s() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

impl AiUi {
    /// Load the durable history when a (different) project becomes active. Anything that was queued or running
    /// when the last session ended is HELD; nothing is restarted.
    pub fn queue_open(&mut self, root: Option<&Path>) {
        if self.tk.history_root.as_deref() == root {
            return;
        }
        self.tk.history_root = root.map(Path::to_path_buf);
        self.tk.last_saved.clear();
        self.tk.history = match root {
            Some(r) => {
                let (h, err) = crate::queue::load(r);
                if err.is_some() {
                    self.notice = err;
                }
                h
            }
            None => vec![],
        };
    }

    fn queue_entries(&mut self, root: &Path) -> Vec<crate::queue::Entry> {
        let mut out: Vec<crate::queue::Entry> = vec![];
        let ids: Vec<(String, String, String)> = self
            .tracked
            .iter()
            .map(|t| (t.job_id.clone(), t.folder.clone(), t.label.clone()))
            .collect();
        for (id, folder, label) in ids {
            let mine = self
                .sched
                .job(&id)
                .map(|e| e.job.clip_root.clone())
                .or_else(|| self.sched.task(&id).map(|e| e.job.clip_root.clone()));
            if !mine.is_some_and(|c| c.starts_with(root)) {
                continue; // another project's job
            }
            let (state, detail, job) = if let Some(e) = self.sched.job(&id) {
                let (s, d) = describe(&e.state, e.failure_message());
                (s, d, serde_json::to_value(&*e.job).ok())
            } else if let Some(e) = self.sched.task(&id) {
                let msg = e.state_message();
                let (s, d) = describe(&e.state, msg);
                (s, d, serde_json::to_value(&*e.job).ok())
            } else {
                continue;
            };
            let at = match self.tk.stamps.get(&id) {
                Some((st, at)) if *st == state => *at,
                _ => {
                    let n = now_s();
                    self.tk.stamps.insert(id.clone(), (state.clone(), n));
                    n
                }
            };
            out.push(crate::queue::Entry {
                id,
                folder,
                label: if label.is_empty() {
                    "Key".into()
                } else {
                    label
                },
                state,
                detail,
                at,
                job,
            });
        }
        out
    }

    /// Write the queue snapshot when anything changed. A failed write is reported, never ignored.
    pub fn queue_sync(&mut self) {
        let Some(root) = self.tk.history_root.clone() else {
            return;
        };
        let mine = self.queue_entries(&root);
        let mut all: Vec<crate::queue::Entry> = self
            .tk
            .history
            .iter()
            .filter(|h| !mine.iter().any(|m| m.id == h.id))
            .cloned()
            .collect();
        all.extend(mine);
        let sig = serde_json::to_string(
            &all.iter()
                .map(|e| (&e.id, &e.state, &e.detail))
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        if sig == self.tk.last_saved {
            return;
        }
        match crate::queue::save(&root, &all) {
            Ok(()) => self.tk.last_saved = sig,
            Err(e) => {
                self.tk.last_saved = sig;
                self.notice = Some(format!("Queue history not saved: {e}"));
            }
        }
    }

    /// "Clear finished": this session's finished runs leave the queue and its log; earlier sessions are dismissed.
    pub fn clear_finished(&mut self) {
        let done: Vec<String> = self.queue_view().into_iter().flat_map(|s| s.runs).filter(|r| !r.active).flat_map(|r| r.rows.into_iter().flat_map(|x| x.ids)).collect();
        self.cleared.extend(done);
        let mine: Vec<String> = self.tracked.iter().map(|t| t.job_id.clone()).collect();
        self.tk.history.retain(|h| mine.contains(&h.id));
        self.tk.last_saved.clear();
    }

    /// Earlier sessions' entries and held jobs.
    pub fn history_ui(&mut self, ui: &mut Ui) {
        let mine: Vec<String> = self.tracked.iter().map(|t| t.job_id.clone()).collect();
        let older: Vec<crate::queue::Entry> = self
            .tk
            .history
            .iter()
            .filter(|h| !mine.contains(&h.id))
            .rev()
            .take(20)
            .cloned()
            .collect();
        if older.is_empty() {
            return;
        }
        ui.separator();
        let mut dismiss = None;
        let mut dismiss_all = false;
        egui::CollapsingHeader::new(format!("Earlier sessions · {} jobs", older.len())).id_salt("earlier_sessions").default_open(false).show(ui, |ui| {
        for h in &older {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(&h.folder).strong());
                ui.label(RichText::new(&h.label).color(DIM));
                if h.state == "held" {
                    ui.colored_label(WARN, "held (interrupted)");
                } else {
                    ui.label(&h.state);
                }
                if !h.detail.is_empty() {
                    ui.label(RichText::new(trunc(&h.detail)).color(DIM).size(12.0));
                }
                if h.state == "held" {
                    if h.label == "Key" && ui.small_button("Review & re-queue").on_hover_text("Opens the normal review with the CURRENT settings; the old job is never re-sent").clicked() {
                        self.tk.requeue = Some(h.folder.clone());
                    }
                }
                if ui.small_button("Dismiss").clicked() {
                    dismiss = Some(h.id.clone());
                }
            });
        }
        if ui.small_button("Dismiss all").clicked() {
            dismiss_all = true;
        }
        });
        if let Some(id) = dismiss {
            self.tk.history.retain(|h| h.id != id);
            self.tk.last_saved.clear();
        }
        // Every earlier-session entry, not only the 20 shown; this session's tracked jobs stay.
        if dismiss_all {
            self.tk.history.retain(|h| mine.contains(&h.id));
            self.tk.last_saved.clear();
        }
    }
}

pub(super) fn describe(st: &JobState, msg: Option<String>) -> (String, String) {
    match st {
        JobState::Queued => ("queued".into(), String::new()),
        JobState::Dispatching { target } => ("running".into(), format!("starting on {target}")),
        JobState::Running {
            phase, done, total, ..
        } => ("running".into(), format!("{phase} {done}/{total}")),
        JobState::Done { info, .. } => match info.outcome {
            Outcome::Complete => ("complete".into(), format!("{} written", info.published)),
            Outcome::Cancelled => ("cancelled".into(), "partial results kept".into()),
            Outcome::Failed => ("failed".into(), info.message.clone().unwrap_or_default()),
        },
        JobState::Rejected { code, message, .. } => {
            ("failed".into(), format!("not started ({code}): {message}"))
        }
        JobState::Failed { reason } => ("failed".into(), format!("not started: {reason}")),
        JobState::Cancelled => ("cancelled".into(), "before it started".into()),
        JobState::Lost {
            reason,
            writer_stopped,
            ..
        } => (
            "lost".into(),
            format!(
                "{reason}{}",
                if *writer_stopped {
                    ""
                } else {
                    " (writer not proven stopped)"
                }
            ),
        ),
    }
    .into_msg(msg)
}

trait IntoMsg {
    fn into_msg(self, m: Option<String>) -> (String, String);
}
impl IntoMsg for (String, String) {
    fn into_msg(self, m: Option<String>) -> (String, String) {
        match (m, self.1.is_empty()) {
            (Some(m), true) => (self.0, m),
            _ => self,
        }
    }
}
