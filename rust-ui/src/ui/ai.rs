//! AI Key panel state: one `Scheduler` (one worker per target), per-clip editable workflow drafts, the
//! readiness gate display, job tracking and the Board batch. Core rules live in `workflow`/`scheduler`;
//! this module only edits, asks, submits and reports. Nothing here fabricates results: files appear only
//! when a worker publishes them and the manifest says so.
use super::theme;
use super::viewer::{DIM, WARN};
use crate::contracts::OutputKind;
use crate::jobs::JobMode;
use crate::project::{Clip, Project};
use crate::scheduler::*;
use crate::settings::{Settings, HINT_MODES, TEMPORAL_WINDOWS, VRAM_PROFILES};
use crate::worker::{Target, TargetKind};
use crate::workflow::*;
use eframe::egui::{self, Color32, RichText, Ui};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

const BAD: Color32 = Color32::from_rgb(225, 110, 110);
const STALL: Duration = Duration::from_secs(120);

pub struct Draft {
    pub params: Params,
    pub out: OutputCfg,
    pub notes: Vec<String>,
}

struct Tracked {
    /// Worker warnings for this job (kept visible, e.g. "DWAB is lossy").
    warnings: Vec<String>,
    job_id: String,
    folder: String,
    name: String,
    /// Task kind label ("" = a Key job).
    label: String,
}

/// What the host must do after `AiUi::tick`.
#[derive(Debug, PartialEq, Eq)]
pub struct Refresh {
    pub folder: String,
    /// Mid-run publication (the manifest may be incomplete); `false` = the job ended.
    pub live: bool,
}

/// One gate request: owned copies, so the check (disk stats on a NAS) runs off the UI thread.
#[derive(Clone)]
struct GateReq {
    sig: String,
    clip: Clip,
    project_name: String,
    location: Option<Value>,
    params: Params,
    out: OutputCfg,
    /// The single range snapshot this check (and the job it yields) is built from.
    range: Result<(usize, usize), String>,
    /// Hint steps queued before Key (the plan assumes they provide the hint).
    steps: Vec<crate::tasks::TaskJob>,
}

pub struct GateOut {
    sig: String,
    main: Result<Plan, Blocked>,
    resume: Option<Plan>,
    newver: Option<Plan>,
    replace: Option<Plan>,
}

fn run_gate(r: &GateReq) -> GateOut {
    let none = |e: String| GateOut {
        sig: r.sig.clone(),
        main: Err(Blocked {
            reasons: vec![e],
            ..Default::default()
        }),
        resume: None,
        newver: None,
        replace: None,
    };
    let range = match &r.range {
        Ok(x) => *x,
        Err(e) => return none(e.clone()),
    };
    let inp = Inputs {
        clip: &r.clip,
        project_name: &r.project_name,
        location: r.location.as_ref(),
        params: &r.params,
        out: &r.out,
        range,
        digest: None,
        pending_hint: !r.steps.is_empty(),
    };
    let main = plan(&inp, Intent::New);
    let (mut resume, mut newver, mut replace) = (None, None, None);
    if let Err(Blocked {
        existing: Some(e), ..
    }) = &main
    {
        if e.resume {
            resume = plan(&inp, Intent::Resume).ok();
        }
        if e.replace {
            replace = plan(&inp, Intent::Replace).ok();
        }
        if e.next_version.is_some() {
            newver = plan(&inp, Intent::NewVersion).ok();
        }
    }
    GateOut {
        sig: r.sig.clone(),
        main,
        resume,
        newver,
        replace,
    }
}

/// Latest finished gate result, the request in flight, and the newest wanted request (debounced).
#[derive(Default)]
struct Gate {
    shown: Option<Arc<GateOut>>,
    inflight: Option<Receiver<GateOut>>,
    want: Option<(Instant, GateReq)>,
}

/// Who a lock's job id belongs to, as far as this session knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockOwner {
    /// Not a job of this session: a run from before a restart. Only the explicit orphan flow applies.
    Unknown,
    /// A job of this session whose writer may be alive (queued, starting, running, stopping, or Lost unproven).
    /// Its own controls (Cancel, Force stop, Confirm writer stopped) are the only way.
    Session,
    /// A job of this session that has ended with its writer proven gone; the scheduler already did the cleanup.
    Ended,
}

/// What the user decided for one clip in the batch review.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    Skip,
    Queue,
    NewVersion,
    Resume,
    /// Replace the files of the existing owned result (needs the batch's explicit confirmation).
    Replace,
}

#[allow(clippy::large_enum_variant)] // a handful of short-lived review rows
enum RowState {
    Ready(Plan),
    Existing {
        reason: String,
        newver: Option<Plan>,
        resume: Option<Plan>,
        replace: Option<Plan>,
        /// Why Replace is unavailable (shown as the disabled reason).
        replace_why: Option<String>,
    },
    Blocked(String),
}

struct BatchRow {
    /// Hint-generation steps queued ahead of Key (each depends on the one before; Key on the last).
    steps: Vec<crate::tasks::TaskJob>,
    clip: Clip,
    params: Params,
    out: OutputCfg,
    range: (usize, usize),
    state: RowState,
    choice: Choice,
}

impl BatchRow {
    fn plan(&self) -> Option<&Plan> {
        match (&self.state, self.choice) {
            (RowState::Ready(p), Choice::Queue) => Some(p),
            (RowState::Existing { newver, .. }, Choice::NewVersion) => newver.as_ref(),
            (RowState::Existing { resume, .. }, Choice::Resume) => resume.as_ref(),
            (RowState::Existing { replace, .. }, Choice::Replace) => replace.as_ref(),
            _ => None,
        }
    }
}

/// The batch review: computed in the background, then confirmed by the user. The plans are the frozen jobs.
struct Batch {
    /// The user confirmed that rows set to Replace overwrite their owned result files.
    confirm_replace: bool,
    rows: Vec<BatchRow>,
    pending: Option<Receiver<Vec<BatchRow>>>,
}

mod tasks;
pub use tasks::{sig_of, PreviewResult, TaskDone};

pub struct AiUi {
    /// The Key panel's inline Replace confirmation is open.
    replace_confirm: bool,
    /// Task-side state (previews, finished results, held chains): see `tasks`.
    pub tk: tasks::TaskState,
    sched: Scheduler,
    targets: Vec<Target>,
    /// Worker setup problems per target (`None` = no target at all), checked when targets change.
    setup: Vec<(Option<String>, String)>,
    drafts: BTreeMap<String, Draft>,
    tracked: Vec<Tracked>,
    pub placement: Placement,
    pub notice: Option<String>,
    /// Bumped when files a gate result depends on may have changed (job ended, submitted, recovered).
    disk_gen: u64,
    status_sig: String,
    gate: Gate,
    batch: Option<Batch>,
    /// Delete a recovered run's leftover temp files too (default: keep them).
    lock_remove_temps: bool,
    /// Saved worker targets waiting for the running/unresolved jobs to finish (applied by `tick`).
    pending_targets: Option<Vec<Target>>,
    /// Model assets are being installed or repaired; do not start workers against a changing tree.
    model_mutation_active: bool,
    /// Screen rectangles from the last frame (click tests).
    pub key_rect: Option<egui::Rect>,
    pub batch_confirm_rect: Option<egui::Rect>,
}

fn trunc(s: &str) -> String {
    let t: String = s.chars().take(300).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

pub const NO_REPO: &str = "No worker is configured: no CorridorKey installation or checkout was found next to the program or in the working folder, and none is set. Add a local target in Settings > AI worker targets, or install/repair the packaged runtime.";

/// Saved targets, or one local GPU-0 worker on `repo` (explicit option) or the resolved runtime —
/// `CK_REPO`, then the install descriptor, then developer checkout discovery (`crate::runtime::launch`).
/// Nothing found = no implicit target (see `NO_REPO`).
pub fn effective_targets(st: &Settings, repo: Option<&Path>) -> Vec<Target> {
    if !st.ai_targets.is_empty() {
        return st.ai_targets.clone();
    }
    let l = crate::runtime::launch(repo);
    let Some(repo) = l.repo else {
        return vec![];
    };
    vec![Target {
        id: "local-0".into(),
        kind: TargetKind::Local {
            repo,
            // CK_PYTHON (user override) beats the descriptor's interpreter.
            python: std::env::var_os("CK_PYTHON")
                .map(PathBuf::from)
                .or(l.python),
        },
        gpu: 0,
        enabled: true,
        storage: Default::default(),
        path_map: vec![],
    }]
}

fn setup_problems(
    targets: &[Target],
    runtime_problem: Option<&str>,
) -> Vec<(Option<String>, String)> {
    let mut v = vec![];
    if let Some(p) = runtime_problem {
        v.push((
            None,
            format!("The installed runtime descriptor is unusable: {p} Repair or reinstall the installation (or unset CK_INSTALL to use a developer checkout)."),
        ));
    }
    if targets.iter().all(|t| !t.enabled) && v.is_empty() {
        v.push((
            None,
            if targets.is_empty() {
                NO_REPO.to_string()
            } else {
                "Every worker target is disabled.".into()
            },
        ));
    }
    for t in targets.iter().filter(|t| t.enabled) {
        if let TargetKind::Local { repo, python } = &t.kind {
            let py = python
                .clone()
                .unwrap_or_else(|| crate::import::python(repo));
            let why = if !repo.join("backend/worker/__main__.py").is_file() {
                Some(format!("{}: {} is not a CorridorKey checkout (no backend/worker). Fix the repo folder in Settings > AI worker targets.", t.id, repo.display()))
            } else if !py.is_file() {
                Some(if python.is_some() {
                    format!("{}: the configured Python does not exist at {}. Fix Python in Settings > AI worker targets, or repair the installation.", t.id, py.display())
                } else {
                    format!("{}: no Python at {}. Create the repo's .venv or set Python in Settings > AI worker targets.", t.id, py.display())
                })
            } else {
                None
            };
            if let Some(w) = why {
                v.push((Some(t.id.clone()), w));
            }
        }
    }
    v
}

impl AiUi {
    pub fn new(st: &Settings, repo: Option<&Path>) -> Self {
        let targets = effective_targets(st, repo);
        let problem = crate::runtime::launch(repo).problem;
        let (sched, notice) = match Scheduler::new(targets.clone()) {
            Ok(s) => (s, None),
            Err(e) => (
                Scheduler::new(vec![]).unwrap(),
                Some(format!("AI targets are invalid: {e}")),
            ),
        };
        Self {
            replace_confirm: false,
            tk: Default::default(),
            sched,
            setup: setup_problems(&targets, problem.as_deref()),
            targets,
            drafts: BTreeMap::new(),
            tracked: vec![],
            placement: Placement::Auto,
            notice,
            disk_gen: 0,
            status_sig: String::new(),
            gate: Gate::default(),
            batch: None,
            lock_remove_temps: false,
            pending_targets: None,
            model_mutation_active: false,
            key_rect: None,
            batch_confirm_rect: None,
        }
    }

    pub fn scheduler(&self) -> &Scheduler {
        &self.sched
    }
    pub fn scheduler_mut(&mut self) -> &mut Scheduler {
        self.disk_gen += 1;
        &mut self.sched
    }
    /// A job is queued, starting or running, or a Lost writer is still fencing its folder.
    pub fn busy(&self) -> bool {
        self.sched.busy()
    }
    pub fn job_ids(&self) -> Vec<String> {
        self.tracked.iter().map(|t| t.job_id.clone()).collect()
    }

    /// New target list (Settings applied). Refused while jobs are active: workers would be dropped under them.
    pub fn set_targets(&mut self, st: &Settings, repo: Option<&Path>) {
        let t = effective_targets(st, repo);
        if t == self.targets {
            self.pending_targets = None;
            return;
        }
        // Remember the newest saved list; `tick` applies it as soon as no job needs the current workers.
        self.pending_targets = Some(t);
        self.try_apply_targets();
    }

    /// Saved targets are waiting for jobs to finish.
    pub fn targets_pending(&self) -> bool {
        self.pending_targets.is_some()
    }
    pub fn pending_note(&self) -> Option<&'static str> {
        self.pending_targets.as_ref().map(|_| "New worker targets are saved. They apply automatically when the running jobs finish and every Lost job is resolved (see the queue).")
    }

    /// Swap in the pending targets, but only once NO old worker process exists any more: a worker still
    /// stopping (graceful, or after a force stop) may hold the GPU, and a close must not see "all stopped"
    /// on a fresh scheduler while the real process lives. Idle old workers are stopped first and awaited.
    fn try_apply_targets(&mut self) {
        if self.pending_targets.is_none() || self.busy() {
            return;
        }
        if !self.sched.all_stopped() {
            if !self
                .sched
                .targets()
                .iter()
                .any(|t| t.status == TargetStatus::Stopping)
            {
                self.sched.stop_all(); // idle workers: EOF; the swap waits for their observed exit
            }
            return;
        }
        let Some(t) = self.pending_targets.take() else {
            return;
        };
        match Scheduler::new(t.clone()) {
            Ok(mut s) => {
                std::mem::swap(&mut self.sched, &mut s);
                s.stop_all(); // the old workers are idle: they get EOF
                self.setup = setup_problems(&t, crate::runtime::launch(None).problem.as_deref());
                self.targets = t;
                self.tracked.clear();
                self.notice = None;
                self.disk_gen += 1;
            }
            Err(e) => self.notice = Some(format!("AI targets not applied: {e}")),
        }
    }

    pub fn draft(&mut self, clip: &Clip, project: &Project, st: &Settings) -> &mut Draft {
        self.drafts.entry(clip.folder.clone()).or_insert_with(|| {
            let (params, out, notes) = seed(clip, project, st);
            Draft { params, out, notes }
        })
    }

    /// Poll workers. Publication and completion come back as `Refresh` so the host re-reads the LOCAL manifest.
    pub fn tick(&mut self) -> Vec<Refresh> {
        self.try_apply_targets();
        let mut out: Vec<Refresh> = vec![];
        for e in self.sched.tick() {
            let (id, live) = match e {
                SchedEvent::Published { job_id, .. } => (job_id, true),
                // Only the end of a job needs a re-read besides publication.
                SchedEvent::Changed(id)
                    if self.sched.state_of(&id).is_some_and(|s| s.is_terminal()) =>
                {
                    self.on_task_end(&id);
                    (id, false)
                }
                SchedEvent::Preview {
                    job_id,
                    frame_stem,
                    width,
                    height,
                    mask_png,
                    score,
                } => {
                    self.on_preview(job_id, frame_stem, width, height, mask_png, score);
                    continue;
                }
                SchedEvent::HintLock {
                    request_id,
                    outcome,
                } => {
                    if let Some(folder) = self.on_lock_reply(request_id, outcome) {
                        out.push(Refresh {
                            folder,
                            live: false,
                        });
                    }
                    continue;
                }
                SchedEvent::Warning { job_id, message } => {
                    if let Some(t) = self.tracked.iter_mut().find(|t| t.job_id == job_id) {
                        if t.warnings.len() < 5 && !t.warnings.contains(&message) {
                            t.warnings.push(message);
                        }
                    }
                    continue;
                }
                _ => continue,
            };
            let Some(t) = self.tracked.iter().find(|t| t.job_id == id) else {
                continue;
            };
            match out.iter_mut().find(|r| r.folder == t.folder) {
                Some(r) => r.live &= live,
                None => out.push(Refresh {
                    folder: t.folder.clone(),
                    live,
                }),
            }
        }
        let sig: String = self
            .sched
            .targets()
            .iter()
            .map(|t| format!("{}={:?}/{:?};", t.id, t.status, t.trust))
            .collect();
        // Mid-run publication must not re-run the gate (it stats every output path, slow on a NAS).
        if sig != self.status_sig || out.iter().any(|r| !r.live) {
            self.status_sig = sig;
            self.disk_gen += 1;
        }
        out
    }

    pub fn active(&self) -> bool {
        self.busy()
            || self
                .sched
                .targets()
                .iter()
                .any(|t| t.status == TargetStatus::Starting || t.status == TargetStatus::Stopping)
    }

    // -------------------------------------------------------------- submit

    fn placement_text(&self) -> String {
        match &self.placement {
            Placement::Auto => "Auto".into(),
            Placement::Target(t) => t.clone(),
        }
    }

    /// Persist what the job uses, then queue the already frozen plan on the chosen placement. `Err` = nothing queued.
    fn submit_plan(
        &mut self,
        clip: &Clip,
        plan: Plan,
        params: &Params,
        out: &OutputCfg,
    ) -> Result<String, String> {
        self.submit_plan_after(clip, plan, params, out, vec![])
    }

    fn submit_plan_after(
        &mut self,
        clip: &Clip,
        plan: Plan,
        params: &Params,
        out: &OutputCfg,
        after: Vec<String>,
    ) -> Result<String, String> {
        if self.model_mutation_active {
            return Err("Model installation or repair is active; wait for it to finish before starting Key work.".into());
        }
        if self.external_writer(&clip.root) {
            return Err(
                "a companion-sequence import is writing this clip's hints; wait for it to finish"
                    .into(),
            );
        }
        commit(clip, &plan, params, out)?;
        let id = plan.job.job_id.clone();
        self.sched
            .submit(Submit {
                job: plan.job,
                placement: self.placement.clone(),
                after,
                min_vram_gb: None,
            })
            .map_err(|e| format!("not queued: {e:?}"))?;
        self.tracked.push(Tracked {
            warnings: vec![],
            job_id: id.clone(),
            folder: clip.folder.clone(),
            name: clip.display_name.clone(),
            label: String::new(),
        });
        self.disk_gen += 1;
        Ok(id)
    }

    /// Queue the open clip's job from the current gate result (narrow layout button). Never recomputes: the job
    /// is exactly the one the gate froze from the viewer's range at that moment.
    pub fn submit_current(&mut self, clip: &Clip, intent: Intent) -> Result<String, String> {
        let g = self
            .gate
            .shown
            .clone()
            .filter(|g| self.gate.want.as_ref().is_some_and(|w| w.1.sig == g.sig));
        let Some(g) = g else {
            return Err("Still checking the output folder; try again in a moment".into());
        };
        let plan = match intent {
            Intent::New => g.main.as_ref().ok(),
            Intent::Resume => g.resume.as_ref(),
            Intent::NewVersion => g.newver.as_ref(),
            Intent::Replace => g.replace.as_ref(),
        };
        let Some(plan) = plan else {
            return Err(match &g.main {
                Err(b) => b
                    .reasons
                    .first()
                    .cloned()
                    .or_else(|| b.existing.as_ref().map(|e| e.reason.clone()))
                    .unwrap_or_default(),
                Ok(_) => "Nothing to queue".into(),
            });
        };
        let (params, out) = {
            let w = &self.gate.want.as_ref().unwrap().1;
            (w.params.clone(), w.out.clone())
        };
        self.submit_plan(clip, plan.clone(), &params, &out)
    }

    /// Start the batch review for these clips: each clip uses its stored In/Out (else the whole clip), its own
    /// interpretation and workflow. Nothing is queued until the user confirms.
    pub fn begin_batch(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        st: &Settings,
        picks: &[usize],
    ) {
        if self.model_mutation_active {
            self.notice = Some(
                "Model installation or repair is active; wait before starting a batch.".into(),
            );
            return;
        }
        let mut reqs = vec![];
        for &i in picks {
            let Some(c) = project.clips.get(i) else {
                continue;
            };
            let n = c.input_files.len();
            let range = match c.in_out {
                Some((a, b)) if a <= b && b < n => Ok((a, b)),
                Some((a, b)) => Err(format!("The stored In/Out {a}–{b} does not fit this clip's {n} frames: open the clip and set In/Out again")),
                None if n > 0 => Ok((0, n - 1)),
                None => Err("No source frames".to_string()),
            };
            let d = self.draft(c, project, st);
            let (mut params, out) = (d.params.clone(), d.out.clone());
            params.input_is_linear = c.input_is_linear;
            // Hint steps the saved route needs before Key (none when bypassed or already covering).
            let (range, steps) = match range {
                Ok(r) => match crate::hints::plan_steps(
                    c,
                    &crate::hints::HintCfg::load(c),
                    &params.hint_mode,
                    r,
                    c.input_is_linear,
                    false,
                    &crate::hints::staging_root(&project.root, &c.root, &c.folder),
                    new_id,
                ) {
                    Ok(s) => (Ok(r), s),
                    Err(e) => (Err(e), vec![]),
                },
                e => (e, vec![]),
            };
            reqs.push(GateReq {
                sig: String::new(),
                clip: c.clone(),
                project_name: project.display_name.clone(),
                location: project.output_location.clone(),
                params,
                out,
                range,
                steps,
            });
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let rows: Vec<BatchRow> = reqs
                .into_iter()
                .map(|r| {
                    let g = run_gate(&r);
                    let (state, choice) = match g.main {
                        Ok(p) => (RowState::Ready(p), Choice::Queue),
                        Err(b) => match b.existing {
                            Some(e) => {
                                let c = if g.newver.is_some() {
                                    Choice::NewVersion
                                } else {
                                    Choice::Skip
                                };
                                (
                                    RowState::Existing {
                                        reason: e.reason,
                                        newver: g.newver,
                                        resume: g.resume,
                                        replace: g.replace,
                                        replace_why: e.replace_why,
                                    },
                                    c,
                                )
                            }
                            None => (RowState::Blocked(b.reasons.join("; ")), Choice::Skip),
                        },
                    };
                    BatchRow {
                        steps: r.steps.clone(),
                        range: r.range.clone().unwrap_or((0, 0)),
                        clip: r.clip,
                        params: r.params,
                        out: r.out,
                        state,
                        choice,
                    }
                })
                .collect();
            let _ = tx.send(rows);
            ctx.request_repaint();
        });
        self.batch = Some(Batch {
            confirm_replace: false,
            rows: vec![],
            pending: Some(rx),
        });
    }

    /// The batch review is computed and waiting for confirmation (tests).
    pub fn batch_ready(&self) -> bool {
        self.batch.as_ref().is_some_and(|b| b.pending.is_none())
    }
    /// One line per clip of the open review (tests / diagnostics).
    pub fn batch_summary(&self) -> Vec<String> {
        let Some(b) = &self.batch else { return vec![] };
        b.rows
            .iter()
            .map(|r| {
                let n = &r.clip.display_name;
                match &r.state {
                    RowState::Ready(p) => format!("{n}: new -> {}", p.job.manifest_dir.display()),
                    RowState::Existing { reason, newver, .. } => format!(
                        "{n}: existing ({reason}); new version {}",
                        newver.as_ref().map_or("unavailable".into(), |p| p
                            .job
                            .manifest_dir
                            .display()
                            .to_string())
                    ),
                    RowState::Blocked(w) => format!("{n}: blocked: {w}"),
                }
            })
            .collect()
    }
    /// The gate result matches the current inputs (tests).
    pub fn gate_current(&self) -> bool {
        self.gate
            .shown
            .as_ref()
            .zip(self.gate.want.as_ref())
            .is_some_and(|(g, w)| g.sig == w.1.sig)
    }
    /// Worker setup problems currently shown (tests).
    pub fn setup_text(&self) -> Vec<String> {
        self.setup.iter().map(|s| s.1.clone()).collect()
    }
    /// Discard the review, including one still being computed.
    pub fn cancel_batch(&mut self) {
        self.batch = None;
    }
    pub fn batch_open(&self) -> bool {
        self.batch.is_some()
    }

    pub fn cancel(&mut self, id: &str) {
        self.sched.cancel(id);
        self.disk_gen += 1;
    }

    fn latest(&self, folder: &str) -> Option<&Tracked> {
        self.tracked.iter().rev().find(|t| t.folder == folder)
    }

    // -------------------------------------------------------------- controls

    pub fn placement_combo(&mut self, ui: &mut Ui) {
        let tv = self.sched.targets();
        let label = |t: &TargetView| {
            let s = match &t.status {
                _ if !t.enabled => "disabled".to_string(),
                TargetStatus::Fresh => "not started".into(),
                TargetStatus::Starting => "starting".into(),
                TargetStatus::Ready => t
                    .busy_with
                    .as_ref()
                    .map_or("ready".into(), |_| "busy".into()),
                TargetStatus::Refused(_) => "refused".into(),
                TargetStatus::Down(_) => "down".into(),
                TargetStatus::Stopping => "stopping".into(),
                TargetStatus::Stopped => "stopped".into(),
            };
            format!(
                "{} ({}, {s})",
                t.id,
                if t.local { "local" } else { "remote" }
            )
        };
        let cur = match &self.placement {
            Placement::Auto => "Auto (local first)".to_string(),
            Placement::Target(id) => tv.iter().find(|t| &t.id == id).map_or(id.clone(), label),
        };
        egui::ComboBox::from_id_salt("ai_placement")
            .selected_text(cur)
            .show_ui(ui, |ui| {
                if ui
                    .selectable_value(&mut self.placement, Placement::Auto, "Auto (local first)")
                    .clicked()
                {
                    self.disk_gen += 1;
                }
                for t in &tv {
                    if ui
                        .selectable_value(
                            &mut self.placement,
                            Placement::Target(t.id.clone()),
                            label(t),
                        )
                        .clicked()
                    {
                        self.disk_gen += 1;
                    }
                }
            });
    }

    pub fn hint_controls(&mut self, ui: &mut Ui, clip: &Clip, project: &Project, st: &Settings) {
        let d = self.draft(clip, project, st);
        let p = &mut d.params;
        ui.label(RichText::new("Hint mode (Bypass = none)").strong());
        choice(ui, "hint_mode", &mut p.hint_mode, &HINT_MODES);
        let r = |ui: &mut Ui, l: &str, v: &mut f64| {
            let mut x = *v as f32;
            ui.add(egui::Slider::new(&mut x, 0.0..=1.0).step_by(0.01).text(l));
            *v = r2(x as f64);
        };
        r(ui, "Hint quality", &mut p.hint_quality);
        theme::checkbox(ui, &mut p.carry_hint, "Carry hint between windows");
        r(ui, "Carry hint quality", &mut p.carry_hint_quality);
        notes(ui, &d.notes);
    }

    pub fn key_controls(
        &mut self,
        ui: &mut Ui,
        clip: &Clip,
        project: &Project,
        st: &Settings,
        linear: bool,
        range_frames: Option<usize>,
    ) {
        let d = self.draft(clip, project, st);
        let p = &mut d.params;
        ui.label(RichText::new("Temporal awareness (frames seen at once)").strong());
        temporal_choice(ui, "temporal_window", &mut p.temporal_window);
        ui.label(
            RichText::new(crate::settings::temporal_explain(
                &p.temporal_window,
                range_frames,
            ))
            .color(DIM)
            .size(12.0),
        );
        ui.label(RichText::new("VRAM profile (engine policy, not a cap)").strong());
        choice(ui, "vram_profile", &mut p.vram_profile, &VRAM_PROFILES);
        let mut x = p.despill_strength as f32;
        ui.add(
            egui::Slider::new(&mut x, 0.0..=1.0)
                .step_by(0.1)
                .text("Despill"),
        );
        p.despill_strength = r2(x as f64);
        ui.label(
            RichText::new(format!(
                "Input interpretation: {} (from Media; the job freezes it)",
                if linear { "Linear" } else { "sRGB" }
            ))
            .color(DIM)
            .size(12.0),
        );
        notes(ui, &d.notes);
    }

    /// Enabled + format of one output (or all four), and the EXR compression shared by every EXR export.
    pub fn output_controls(
        &mut self,
        ui: &mut Ui,
        clip: &Clip,
        project: &Project,
        st: &Settings,
        only: Option<OutputKind>,
    ) {
        let o = &mut self.draft(clip, project, st).out;
        let row = |ui: &mut Ui, k: OutputKind, en: &mut bool, f: &mut String| {
            if only.is_none_or(|x| x == k) {
                ui.horizontal(|ui| {
                    theme::checkbox(ui, en, k.token());
                    choice(ui, &format!("fmt_{}", k.token()), f, &["exr", "png"]);
                });
            }
        };
        row(ui, OutputKind::Fg, &mut o.fg_enabled, &mut o.fg_format);
        row(
            ui,
            OutputKind::Matte,
            &mut o.matte_enabled,
            &mut o.matte_format,
        );
        row(
            ui,
            OutputKind::Comp,
            &mut o.comp_enabled,
            &mut o.comp_format,
        );
        row(
            ui,
            OutputKind::Processed,
            &mut o.processed_enabled,
            &mut o.processed_format,
        );
        ui.label("EXR compression (shared by all EXR exports)");
        egui::ComboBox::from_id_salt("exr_comp")
            .selected_text(if o.exr_compression == "dwab" {
                "dwab (lossy)"
            } else {
                o.exr_compression.as_str()
            })
            .show_ui(ui, |ui| {
                for c in EXR_COMPRESSIONS {
                    ui.selectable_value(
                        &mut o.exr_compression,
                        c.to_string(),
                        if c == "dwab" { "dwab (lossy)" } else { c },
                    );
                }
            });
        if o.exr_compression == "dwab" {
            ui.colored_label(WARN, "DWAB is lossy: HDR values are not bit-exact. Choose piz, zip or none for exact data.");
        }
        ui.label(RichText::new("The engine writes ZIP EXRs; other compressions are re-encoded and the real value is recorded in the manifest.").color(DIM).size(12.0));
    }

    // -------------------------------------------------------------- Key footer

    /// Ask for (and pick up) the gate result of the open clip. Runs the disk checks on one background thread,
    /// debounced and latest-wins, so editing sliders never touches the NAS from the UI thread. `Some` only when
    /// the result matches the current inputs.
    #[allow(clippy::too_many_arguments)]
    fn gate_poll(
        &mut self,
        ctx: &egui::Context,
        clip: &Clip,
        project: &Project,
        st: &Settings,
        range: Result<(usize, usize), String>,
        linear: bool,
    ) -> Option<Arc<GateOut>> {
        let d = self.draft(clip, project, st);
        let (mut params, out) = (d.params.clone(), d.out.clone());
        params.input_is_linear = linear;
        let sig = format!(
            "{}|{params:?}|{out:?}|{range:?}|{:?}|{:?}|{:?}|{}|{}|{}",
            clip.folder,
            project.output_location,
            clip.output_values,
            clip.clip_id,
            self.disk_gen,
            clip.alpha_frames,
            clip.manifest.as_ref().map_or(0, |m| m.complete as u8 + 1)
        );
        if self.gate.want.as_ref().is_none_or(|w| w.1.sig != sig) {
            let req = GateReq {
                sig,
                clip: clip.clone(),
                project_name: project.display_name.clone(),
                location: project.output_location.clone(),
                params,
                out,
                range,
                steps: vec![],
            };
            self.gate.want = Some((Instant::now(), req));
        }
        if let Some(rx) = &self.gate.inflight {
            match rx.try_recv() {
                Ok(o) => {
                    self.gate.shown = Some(Arc::new(o));
                    self.gate.inflight = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.gate.inflight = None,
            }
        }
        let want = self.gate.want.as_ref().unwrap();
        let current = self
            .gate
            .shown
            .as_ref()
            .is_some_and(|g| g.sig == want.1.sig);
        if !current {
            if self.gate.inflight.is_none() {
                if want.0.elapsed() >= Duration::from_millis(250) {
                    let (tx, rx) = channel();
                    let (req, ctx2) = (want.1.clone(), ctx.clone());
                    std::thread::spawn(move || {
                        let _ = tx.send(run_gate(&req));
                        ctx2.request_repaint();
                    });
                    self.gate.inflight = Some(rx);
                } else {
                    ctx.request_repaint_after(Duration::from_millis(100));
                }
            } else {
                ctx.request_repaint_after(Duration::from_millis(50));
            }
        }
        self.gate
            .shown
            .clone()
            .filter(|g| g.sig == self.gate.want.as_ref().unwrap().1.sig)
    }

    /// Poll the gate without drawing it (the narrow layout's button uses the result).
    pub fn watch_gate(
        &mut self,
        ctx: &egui::Context,
        clip: &Clip,
        project: &Project,
        st: &Settings,
        range: Result<(usize, usize), String>,
        linear: bool,
    ) {
        let _ = self.gate_poll(ctx, clip, project, st, range, linear);
    }

    pub fn lock_owner(&self, job_id: &str) -> LockOwner {
        match self.sched.job(job_id).map(|e| &e.state) {
            None => LockOwner::Unknown,
            Some(JobState::Lost {
                writer_stopped: true,
                ..
            })
            | Some(JobState::Done { .. })
            | Some(JobState::Rejected { .. })
            | Some(JobState::Failed { .. })
            | Some(JobState::Cancelled) => LockOwner::Ended,
            Some(_) => LockOwner::Session,
        }
    }

    /// The explicit "that process is gone" release for a lock from BEFORE this session. Refuses any job this
    /// session knows (its writer may be alive; the scheduler's own confirm keeps fence and proof coherent), no
    /// matter what the gate showed when it was drawn.
    pub fn release_lock(&mut self, dir: &Path, id: &str) -> Result<String, String> {
        if self.lock_owner(id) != LockOwner::Unknown {
            return Err(format!("Job {id} belongs to this session: use its own Cancel / Force stop / Confirm writer stopped. Nothing was released."));
        }
        let r = crate::jobs::recover_orphan(dir, id, self.lock_remove_temps);
        self.disk_gen += 1;
        let left = if r.temps_left.is_empty() {
            String::new()
        } else {
            format!("; {} temp file(s) kept", r.temps_left.len())
        };
        let what = if r.lock_removed {
            "lock removed".to_string()
        } else {
            r.lock_left
                .or(r.errors.first().cloned())
                .unwrap_or_else(|| "nothing removed".into())
        };
        Ok(format!("Lock release for job {id}: {what}{left}"))
    }

    /// Setup problems that stop the chosen placement from ever starting a worker.
    fn setup_blockers(&self) -> Vec<String> {
        let enabled: Vec<&str> = self
            .targets
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.id.as_str())
            .collect();
        let relevant: Vec<&str> = match &self.placement {
            Placement::Auto => enabled,
            Placement::Target(id) => vec![id.as_str()],
        };
        let bad = |id: &str| -> Vec<String> {
            self.setup
                .iter()
                .filter(|(t, _)| t.as_deref() == Some(id))
                .map(|(_, w)| w.clone())
                .collect()
        };
        if self.setup.iter().any(|(t, _)| t.is_none()) {
            return self
                .setup
                .iter()
                .filter(|(t, _)| t.is_none())
                .map(|(_, w)| w.clone())
                .collect();
        }
        // Auto can still use any target without a problem.
        let all_bad = !relevant.is_empty() && relevant.iter().all(|id| !bad(id).is_empty());
        if all_bad || matches!(self.placement, Placement::Target(_)) {
            relevant.iter().flat_map(|id| bad(id)).collect()
        } else {
            vec![]
        }
    }

    /// The Key panel body: summary, blockers with their fix actions, target, button, job status.
    /// `range` is the inclusive source-frame range of the open viewer (or why it cannot be built).
    pub fn key_panel(
        &mut self,
        ui: &mut Ui,
        clip: &Clip,
        project: &Project,
        st: &Settings,
        range: Result<(usize, usize), String>,
        linear: bool,
    ) {
        let g = self.gate_poll(&ui.ctx().clone(), clip, project, st, range, linear);
        let setup = self.setup_blockers();
        let ready = g
            .as_ref()
            .and_then(|g| g.main.as_ref().ok())
            .map(|p| self.sched.readiness(&self.placement, &p.job, None));
        let mut intent = None;
        let mut recover = None;
        // Details scroll; the verdict and the Key button below stay pinned and always reachable.
        egui::ScrollArea::vertical().id_salt("key_panel_scroll").max_height(190.0).auto_shrink([true, true]).show(ui, |ui| {
            match &g {
                None => {
                    ui.label(RichText::new("Checking the output folder…").color(DIM));
                }
                Some(g) => {
                    if let Ok(p) = &g.main {
                        ui.label(&p.summary);
                    }
                    if let Err(b) = &g.main {
                        for r in &b.reasons {
                            ui.colored_label(WARN, format!("• {r}"));
                        }
                        if let (Some(l), Some(dir)) = (&b.lock, &b.lock_dir) {
                            if let Some(id) = &l.job_id {
                                match self.lock_owner(id) {
                                    LockOwner::Session => {
                                        ui.label(RichText::new("This lock belongs to a job of this session whose worker may still be running. Use that job's own controls (Cancel, Force stop, or Confirm writer stopped); it cannot be released from here.").color(DIM).size(12.0));
                                    }
                                    LockOwner::Ended => {
                                        ui.label(RichText::new("This lock belongs to a finished job of this session; the cleanup result is shown with that job. It is not released from here.").color(DIM).size(12.0));
                                    }
                                    LockOwner::Unknown => {
                                        ui.label(RichText::new("The lock names a run from before this session. Releasing it is only safe if that process is really gone.").color(DIM).size(12.0));
                                        if !b.leftovers.is_empty() {
                                            ui.label(RichText::new(format!("That run left {} temp file(s) in the folder:", b.leftovers.len())).size(12.0));
                                            for t in b.leftovers.iter().take(5) {
                                                ui.label(RichText::new(t.display().to_string()).monospace().size(11.0).color(DIM));
                                            }
                                            theme::checkbox(ui, &mut self.lock_remove_temps, "Also delete these temp files (otherwise they are kept)");
                                        }
                                        if ui.button("I confirm that process is gone: release the lock").clicked() {
                                            recover = Some((dir.clone(), id.clone()));
                                        }
                                    }
                                }
                            }
                        }
                        if let Some(e) = &b.existing {
                            ui.colored_label(WARN, &e.reason);
                            ui.label(RichText::new("Nothing is replaced unless you choose Replace and confirm it.").color(DIM).size(12.0));
                            if self.replace_confirm && g.replace.is_some() {
                                // Buttons first (always visible); the folder is already shown in the line above.
                                ui.horizontal(|ui| {
                                    if ui.button("Yes, replace").clicked() {
                                        intent = Some(Intent::Replace);
                                    }
                                    if ui.button("Cancel").clicked() {
                                        self.replace_confirm = false;
                                    }
                                });
                                ui.colored_label(WARN, format!("Replace {} owned file(s)? Old files stay until the new ones are verified; unknown files are never touched; cancel or failure keeps the previous result.", e.replace_files));
                            }
                            ui.horizontal_wrapped(|ui| {
                                if g.resume.is_some()
                                    && ui.add_enabled(!self.model_mutation_active, egui::Button::new("Resume previous run")).clicked()
                                {
                                    intent = Some(Intent::Resume);
                                }
                                // Replace: owned files only, explicit confirmation, disabled with its reason.
                                let rep_ready = g.replace.as_ref().map(|p| self.sched.readiness(&self.placement, &p.job, None));
                                let rep_block = match (&g.replace, &rep_ready) {
                                    (None, _) => Some(e.replace_why.clone().unwrap_or_else(|| "Replace is not available for this result".into())),
                                    (Some(_), Some(Readiness::Blocked(r))) => Some(r.join("; ")),
                                    _ if self.model_mutation_active => Some("Model installation or repair is active".into()),
                                    _ => None,
                                };
                                let label = format!("Replace result ({} files)", e.replace_files);
                                match rep_block {
                                    Some(why) => {
                                        ui.add_enabled(false, egui::Button::new(label)).on_disabled_hover_text(why);
                                    }
                                    None => {
                                        if ui.button(label).on_hover_text("Overwrites the files this result owns, with the settings above").clicked() {
                                            self.replace_confirm = !self.replace_confirm;
                                        }
                                    }
                                }
                                match (e.next_version, &g.newver) {
                                    (Some(v), Some(_)) => {
                                        if ui.add_enabled(
                                            !self.model_mutation_active,
                                            egui::Button::new(format!("Write as v{v:03}")),
                                        ).on_hover_text("Same settings, next free version folder").clicked() {
                                            intent = Some(Intent::NewVersion);
                                        }
                                    }
                                    _ => {
                                        ui.add_enabled(false, egui::Button::new("Write as new version"))
                                            .on_disabled_hover_text("Needs an output location with {version} in the folder template (Deliver tab)");
                                    }
                                }
                            });
                        }
                    }
                }
            }
            for w in &setup {
                ui.colored_label(BAD, format!("• {w}"));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Target");
                self.placement_combo(ui);
                if ui.add_enabled(
                    !self.model_mutation_active,
                    egui::Button::new("Check workers"),
                ).on_hover_text("Start the worker(s) now to see GPU and model identity; disabled during model installation or repair").clicked() {
                    self.sched.probe(None);
                }
            });
            for t in self.sched.targets() {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(target_line(&t)).color(DIM).size(12.0));
                    let failed = matches!(t.status, TargetStatus::Down(_) | TargetStatus::Refused(_) | TargetStatus::Stopped);
                    if failed && t.busy_with.is_none() && ui.add_enabled(
                        !self.model_mutation_active,
                        egui::Button::new("Retry").small(),
                    ).on_hover_text("Forget this verdict and start the worker again; disabled during model installation or repair").clicked() {
                        self.sched.reset_target(&t.id);
                    }
                });
            }
            if self.sched.targets().iter().any(|t| !t.local && t.enabled) && self.sched.reference().is_none() {
                ui.colored_label(WARN, "Remote workers need a verified local model digest first: run Check workers and get the local worker Ready. If the local worker is Down, remote jobs stay blocked.");
            }
            if let Some(n) = self.notice.clone() {
                ui.colored_label(WARN, n);
            }
            if let Some(n) = self.pending_note() {
                ui.colored_label(WARN, n);
            }
            self.unresolved_ui(ui);
            self.job_status(ui, &clip.folder);
        });
        let (can, why) = if self.model_mutation_active {
            (
                false,
                "Model installation or repair is active; Key workers are paused until it finishes."
                    .into(),
            )
        } else {
            match (&ready, &g) {
                (_, None) => (false, "Checking the output folder…".to_string()),
                _ if !setup.is_empty() => (false, "Worker setup needed (see above)".into()),
                (
                    Some(Readiness::Ready {
                        unverified,
                        busy,
                        target,
                    }),
                    _,
                ) => {
                    let mut s = format!(
                        "Runs on {target}{}",
                        if *busy { " (busy: it will wait)" } else { "" }
                    );
                    if *unverified {
                        s += " · model identity unverified";
                    }
                    (true, s)
                }
                (Some(Readiness::Pending(w)), _) => {
                    (true, format!("Worker not ready yet: {}", trunc(w)))
                }
                (Some(Readiness::Blocked(r)), _) => (
                    false,
                    format!("No target can run this job: {}", trunc(&r.join("; "))),
                ),
                _ => (false, "Fix the items above first".into()),
            }
        };
        ui.label(
            RichText::new(why)
                .color(if can { DIM } else { BAD })
                .size(12.0),
        );
        let go = ui.add_enabled(
            can,
            egui::Button::new("Key clip").min_size(egui::Vec2::new(ui.available_width(), 30.0)),
        );
        self.key_rect = Some(go.rect);
        if go.clicked() {
            intent = Some(Intent::New);
        }
        if let Some((dir, id)) = recover {
            self.notice = Some(match self.release_lock(&dir, &id) {
                Ok(m) | Err(m) => m,
            });
            self.disk_gen += 1;
        }
        if let Some(i) = intent {
            self.replace_confirm = false;
            self.notice = match self.submit_current(clip, i) {
                Ok(_) => None,
                Err(e) => Some(format!("Not queued: {}", trunc(&e))),
            };
        }
    }

    /// Latest job of this clip: phase, bounded progress, Cancel, lost/stalled recovery.
    pub fn job_status(&mut self, ui: &mut Ui, folder: &str) {
        let Some(id) = self.latest(folder).map(|t| t.job_id.clone()) else {
            return;
        };
        self.job_row(ui, &id, false);
    }

    fn job_row(&mut self, ui: &mut Ui, id: &str, with_name: bool) {
        let Some(e) = self.sched.job(id).cloned() else {
            return self.task_row(ui, id, with_name);
        };
        let name = self
            .tracked
            .iter()
            .find(|t| t.job_id == id)
            .map_or("", |t| t.name.as_str())
            .to_owned();
        let stalled = self
            .sched
            .stalled(STALL)
            .into_iter()
            .find(|s| s.job_id == id);
        let mut cancel = false;
        let mut confirm = false;
        let mut force = None;
        ui.horizontal_wrapped(|ui| {
            if with_name {
                ui.label(RichText::new(&name).strong());
            }
            match &e.state {
                JobState::Queued => {
                    ui.label("Queued");
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Dispatching { target } => {
                    ui.label(format!("Starting on {target}…"));
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Running { target, phase, done, total } => {
                    ui.label(format!("{target}: {}", trunc(phase)));
                    if *total > 0 {
                        let f = (*done).min(*total) as f32 / *total as f32;
                        ui.add(egui::ProgressBar::new(f).desired_width(120.0).text(format!("{}/{}", (*done).min(*total), total)));
                    }
                    if e.cancel_requested {
                        ui.label(RichText::new("cancel requested; waiting for the worker").color(WARN));
                    } else {
                        cancel = ui.small_button("Cancel").clicked();
                    }
                    if e.cancel_requested {
                        if let Some(s) = &stalled {
                            if ui.small_button("Force stop worker").on_hover_text("Kills the worker process. Frames already written stay; the job becomes Lost.").clicked() {
                                force = Some(s.target.clone());
                            }
                        }
                    }
                }
                JobState::Done { target, info } => {
                    let word = match info.outcome {
                        crate::worker::Outcome::Complete => "Complete",
                        crate::worker::Outcome::Cancelled => "Cancelled (partial results kept)",
                        crate::worker::Outcome::Failed => "Failed",
                    };
                    ui.label(format!("{word} on {target}: {} of {} published{}", info.published, info.expected, if info.skipped > 0 { format!(", {} skipped", info.skipped) } else { String::new() }));
                    if let Some(m) = info.message.as_ref().filter(|_| info.outcome == crate::worker::Outcome::Failed) {
                        ui.colored_label(BAD, trunc(m));
                    }
                }
                JobState::Rejected { target, code, message } => {
                    ui.colored_label(BAD, format!("Failed (not started) on {target}: {code}: {}", trunc(message)));
                }
                JobState::Failed { reason } => {
                    ui.colored_label(BAD, format!("Failed (not started): {}", trunc(reason)));
                }
                JobState::Cancelled => {
                    ui.label("Cancelled before it started");
                }
                JobState::Lost { target, reason, writer_stopped, recovery } => {
                    ui.colored_label(BAD, format!("Lost ({target}): {}", trunc(reason)));
                    if *writer_stopped {
                        ui.label(format!("Writer stopped{}. Files already written stay; re-run to resume.", recovery.as_ref().map_or(String::new(), |r| if r.errors.is_empty() { " and cleaned up".into() } else { format!(" (cleanup problems: {})", trunc(&r.errors.join("; "))) })));
                    } else {
                        // Only a reaped local non-Windows child is proof; that arrives on a later tick.
                        let waiting = !cfg!(windows) && self.sched.targets().iter().any(|t| &t.id == target && t.local);
                        ui.colored_label(WARN, if waiting { "Waiting for the local worker process to exit; the folder stays locked until it is proven stopped." } else { "The worker is not proven stopped and may still be writing: the folder stays locked." });
                        confirm = ui.small_button("I confirm the worker is stopped").clicked();
                    }
                }
            }
        });
        if let Some(s) = stalled {
            if !e.state.is_terminal() {
                ui.label(
                    RichText::new(format!(
                        "No news from {} for {} s{}. Nothing is killed automatically.",
                        s.target,
                        s.idle.as_secs(),
                        if s.accepted {
                            ""
                        } else {
                            " (not accepted yet)"
                        }
                    ))
                    .color(WARN)
                    .size(12.0),
                );
            }
        }
        for w in self
            .tracked
            .iter()
            .find(|t| t.job_id == id)
            .map(|t| t.warnings.clone())
            .unwrap_or_default()
        {
            ui.label(
                RichText::new(format!("Worker warning: {}", trunc(&w)))
                    .color(WARN)
                    .size(12.0),
            );
        }
        if e.unverified_models {
            ui.label(
                RichText::new("Model identity unverified on this worker.")
                    .color(WARN)
                    .size(12.0),
            );
        }
        if cancel {
            self.cancel(id);
        }
        if confirm {
            self.sched.confirm_writer_stopped(id);
            self.disk_gen += 1;
        }
        if let Some(t) = force {
            self.sched.force_stop(&t);
            self.disk_gen += 1;
        }
    }

    /// Every Lost job that still needs a human decision or cleanup (core's `needs_attention`), tracked or not.
    fn unresolved(&self) -> Vec<String> {
        self.sched
            .needs_attention()
            .into_iter()
            .map(|e| e.job.job_id.clone())
            .chain(
                self.sched
                    .task_attention()
                    .into_iter()
                    .map(|t| t.job.job_id.clone()),
            )
            .collect()
    }

    /// Every unresolved Lost job, wherever it came from: always reachable (Board and Key panel).
    pub fn unresolved_ui(&mut self, ui: &mut Ui) {
        let ids = self.unresolved();
        if ids.is_empty() {
            return;
        }
        ui.label(
            RichText::new(format!(
                "{} job(s) need your decision (writer not proven stopped)",
                ids.len()
            ))
            .color(WARN)
            .strong(),
        );
        for id in ids {
            self.job_row(ui, &id, true);
        }
    }

    /// Board queue: running/queued/unresolved jobs always, then the latest finished ones.
    pub fn queue_ui(&mut self, ui: &mut Ui) {
        if self.tracked.is_empty() && self.tk.history.is_empty() {
            ui.label(RichText::new("No jobs").color(DIM));
        }
        let unresolved = self.unresolved();
        let mut ids: Vec<String> = self
            .tracked
            .iter()
            .rev()
            .filter(|t| {
                self.sched
                    .state_of(&t.job_id)
                    .is_some_and(|s| !s.is_terminal())
                    || unresolved.contains(&t.job_id)
            })
            .map(|t| t.job_id.clone())
            .collect();
        let done: Vec<String> = self
            .tracked
            .iter()
            .rev()
            .filter(|t| !ids.contains(&t.job_id))
            .take(12)
            .map(|t| t.job_id.clone())
            .collect();
        ids.extend(done);
        for id in ids {
            self.job_row(ui, &id, true);
        }
    }

    /// Board batch controls: "Key selected N clips" starts the review (nothing is queued yet).
    pub fn batch_ui(&mut self, ui: &mut Ui, project: &Project, st: &Settings, picks: &[usize]) {
        let r = ui.add_enabled(
            !picks.is_empty() && self.batch.is_none() && !self.model_mutation_active,
            egui::Button::new(format!(
                "Key selected {} clip{}…",
                picks.len(),
                if picks.len() == 1 { "" } else { "s" }
            )),
        );
        if r.clicked() {
            self.begin_batch(&ui.ctx().clone(), project, st, picks);
        }
    }

    /// The review: per clip the frozen range, the resolved example path and what will happen; existing results
    /// offer the next free version (never a replace). Confirm queues exactly these jobs on the chosen target.
    pub fn batch_review(&mut self, ui: &mut Ui) {
        if let Some(b) = &mut self.batch {
            if let Some(rx) = &b.pending {
                match rx.try_recv() {
                    Ok(rows) => {
                        b.rows = rows;
                        b.pending = None;
                    }
                    Err(TryRecvError::Empty) => {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Checking the selected clips…").color(DIM));
                            if ui.button("Cancel").clicked() {
                                self.batch = None; // the thread's late answer finds no receiver and is dropped
                            }
                        });
                        ui.ctx().request_repaint_after(Duration::from_millis(50));
                        return;
                    }
                    Err(TryRecvError::Disconnected) => {
                        self.batch = None;
                        return;
                    }
                }
            }
        }
        if self.batch.is_none() {
            return;
        }
        let mut confirm = false;
        let mut cancel = false;
        ui.group(|ui| {
            ui.label(RichText::new("Review batch").strong());
            let Some(b) = self.batch.as_mut() else { return };
            for r in &mut b.rows {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&r.clip.display_name).strong());
                    ui.label(
                        RichText::new(format!("frames {}–{}", r.range.0, r.range.1)).color(DIM),
                    );
                    match &r.state {
                        RowState::Ready(_) => {
                            ui.label("new result");
                            let mut on = r.choice == Choice::Queue;
                            if theme::checkbox(ui, &mut on, "queue").changed() {
                                r.choice = if on { Choice::Queue } else { Choice::Skip };
                            }
                        }
                        RowState::Existing {
                            reason,
                            newver,
                            resume,
                            replace,
                            replace_why,
                        } => {
                            ui.colored_label(WARN, trunc(reason));
                            ui.selectable_value(&mut r.choice, Choice::Skip, "skip");
                            if let Some(p) = newver {
                                let v = p.values["version"].as_u64().unwrap_or(0);
                                ui.selectable_value(
                                    &mut r.choice,
                                    Choice::NewVersion,
                                    format!("write as v{v:03}"),
                                );
                            }
                            if resume.is_some() {
                                ui.selectable_value(&mut r.choice, Choice::Resume, "resume");
                            }
                            match replace {
                                Some(p) => {
                                    ui.selectable_value(
                                        &mut r.choice,
                                        Choice::Replace,
                                        format!("replace {} owned files", p.job.outputs.values().map(Vec::len).sum::<usize>()),
                                    );
                                }
                                None => {
                                    ui.add_enabled(false, egui::Button::new("replace"))
                                        .on_disabled_hover_text(replace_why.clone().unwrap_or_else(|| "Replace is not available for this result".into()));
                                    if let Some(w) = replace_why {
                                        ui.label(RichText::new(format!("replace unavailable: {}", trunc(w))).color(DIM).size(11.0));
                                    }
                                }
                            }
                        }
                        RowState::Blocked(why) => {
                            ui.colored_label(BAD, format!("not queued: {}", trunc(why)));
                        }
                    }
                });
                if !r.steps.is_empty() && r.plan().is_some() {
                    let chain: Vec<&str> = r.steps.iter().map(|s| s.kind.label()).collect();
                    ui.label(
                        RichText::new(format!("first: {} > Key", chain.join(" > ")))
                            .color(DIM)
                            .size(12.0),
                    );
                }
                if let Some(p) = r.plan() {
                    let first = p.job.outputs.values().next().and_then(|v| v.first());
                    ui.label(
                        RichText::new(format!(
                            "to {}",
                            first.map_or(String::new(), |f| f.display().to_string())
                        ))
                        .monospace()
                        .size(11.0)
                        .color(DIM),
                    );
                }
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("Target");
                self.placement_combo(ui);
                let n = self
                    .batch
                    .as_ref()
                    .map_or(0, |b| b.rows.iter().filter(|r| r.plan().is_some()).count());
                let n_replace = self.batch.as_ref().map_or(0, |b| {
                    b.rows.iter().filter(|r| r.choice == Choice::Replace && r.plan().is_some()).count()
                });
                let confirmed = n_replace == 0 || self.batch.as_ref().is_some_and(|b| b.confirm_replace);
                if n_replace > 0 {
                    if let Some(b) = self.batch.as_mut() {
                        theme::checkbox(
                            ui,
                            &mut b.confirm_replace,
                            format!("I confirm: replace the owned result files of {n_replace} clip(s). The worker keeps the old files until the new ones are verified; other files in those folders are untouched."),
                        );
                    }
                }
                let b = ui.add_enabled(
                    n > 0 && confirmed && !self.model_mutation_active,
                    egui::Button::new(format!(
                        "Queue {n} clip{} on {}",
                        if n == 1 { "" } else { "s" },
                        self.placement_text()
                    )),
                );
                self.batch_confirm_rect = Some(b.rect);
                confirm = b.clicked();
                cancel = ui.button("Cancel").clicked();
            });
            ui.label(
                RichText::new("Whole clips are distributed over the targets. Existing results are only replaced where you chose and confirmed it.")
                    .color(DIM)
                    .size(12.0),
            );
        });
        if confirm {
            let rows = self.batch.take().map(|b| b.rows).unwrap_or_default();
            let (mut queued, mut errs) = (0, vec![]);
            for r in rows {
                if let Some(p) = r.plan().cloned() {
                    let res = if r.steps.is_empty() {
                        self.submit_plan(&r.clip, p, &r.params, &r.out)
                    } else {
                        self.submit_chain(
                            &r.clip,
                            r.steps.clone(),
                            Some((p, r.params.clone(), r.out.clone())),
                        )
                    };
                    match res {
                        Ok(_) => queued += 1,
                        Err(e) => errs.push(format!("{}: {}", r.clip.display_name, trunc(&e))),
                    }
                }
            }
            let mut m = format!("Queued {queued} clip(s) on {}.", self.placement_text());
            if !errs.is_empty() {
                m += &format!(" Not queued: {}", errs.join(" · "));
            }
            self.notice = Some(m);
        } else if cancel {
            self.batch = None;
        }
    }

    // -------------------------------------------------------------- closing

    /// A job is queued, starting or running (not merely fenced).
    pub fn running_jobs(&self) -> bool {
        self.sched.running()
    }
    pub fn set_model_mutation_active(&mut self, active: bool) {
        self.model_mutation_active = active;
    }
    /// Model assets are safe to change only after observed worker exits and resolved job fences.
    pub fn model_asset_mutation_safe(&self) -> bool {
        self.sched.all_stopped()
            && self.sched.needs_attention().is_empty()
            && self.sched.task_attention().is_empty()
            && !self.sched.running()
    }
    pub fn workers_need_stop_for_model_change(&self) -> bool {
        self.running_jobs() || !self.sched.all_stopped()
    }
    pub fn unresolved_model_fences(&self) -> bool {
        !self.unresolved().is_empty()
    }
    /// A worker process may be alive (starting, idle or stopping).
    pub fn workers_alive(&self) -> bool {
        !self.sched.all_stopped()
    }
    /// Graceful stop of everything: queued jobs are cancelled, workers get EOF and finish their `done`.
    pub fn begin_stop(&mut self) {
        self.sched.stop_all();
        self.disk_gen += 1;
    }
    pub fn all_stopped(&self) -> bool {
        self.sched.all_stopped()
    }
    pub fn stopping_lines(&self) -> Vec<String> {
        self.sched
            .targets()
            .iter()
            .filter(|t| t.enabled)
            .map(target_line)
            .collect()
    }

    /// For tests/QA: mode of a tracked job.
    pub fn job_mode(&self, id: &str) -> Option<JobMode> {
        self.sched.job(id).map(|e| e.job.mode)
    }
}

fn target_line(t: &TargetView) -> String {
    let st = match &t.status {
        _ if !t.enabled => "disabled".to_string(),
        TargetStatus::Fresh => "not started (press Check workers)".into(),
        TargetStatus::Starting => match t.starting_for {
            Some(e) => format!(
                "starting… {} s of {} s allowed (model load on a NAS can be slow)",
                e.as_secs(),
                t.startup_budget.as_secs()
            ),
            None => "starting…".into(),
        },
        TargetStatus::Ready => "ready".into(),
        TargetStatus::Refused(r) => format!("refused: {}", trunc(r)),
        TargetStatus::Down(r) => format!("down: {}", trunc(r)),
        TargetStatus::Stopping => "stopping".into(),
        TargetStatus::Stopped => "stopped".into(),
    };
    let hw = t.hello.as_ref().map_or(String::new(), |h| {
        format!(
            " · {}",
            h.gpu.as_ref().map_or("no GPU".into(), |g| format!(
                "{} {:.0} GB",
                g.name, g.vram_gb
            ))
        )
    });
    let trust = match t.trust {
        Trust::Unknown => "model identity: verifying",
        Trust::Reference => "model identity: reference",
        Trust::Matches => "model identity: matches local",
        Trust::Mismatch => "model identity: DIFFERS from local",
        Trust::Unverified => "model identity: unverifiable",
    };
    format!(
        "{} ({}): {st}{hw} · {trust}",
        t.id,
        if t.local { "local" } else { "remote" }
    )
}

fn notes(ui: &mut Ui, n: &[String]) {
    for t in n {
        ui.colored_label(WARN, t);
    }
}

/// The temporal-window combo with plain-language entries; the stored/wire value is the exact preset string.
pub fn temporal_choice(ui: &mut Ui, id: &str, v: &mut String) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(crate::settings::temporal_label(v))
        .show_ui(ui, |ui| {
            for o in TEMPORAL_WINDOWS {
                ui.selectable_value(v, o.to_string(), crate::settings::temporal_label(o));
            }
        });
}

fn choice(ui: &mut Ui, id: &str, v: &mut String, opts: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .selected_text(v.as_str())
        .show_ui(ui, |ui| {
            for o in opts {
                ui.selectable_value(v, o.to_string(), *o);
            }
        });
}

impl Drop for AiUi {
    /// Graceful: workers are told to stop (stdin EOF = cancel + exit); nothing is killed here.
    /// Safety net only: the app closes through the stop flow (`begin_stop`, tick until `all_stopped`). This
    /// sends the stop and returns at once; dropping the connections is EOF.
    fn drop(&mut self) {
        self.sched.stop_all();
    }
}
