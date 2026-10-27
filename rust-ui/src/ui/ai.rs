//! AI Key panel state: one `Scheduler` (one worker per target), per-clip editable workflow drafts, the
//! readiness gate display, job tracking and the Board batch. Core rules live in `workflow`/`scheduler`;
//! this module only edits, asks, submits and reports. Nothing here fabricates results: files appear only
//! when a worker publishes them and the manifest says so.
use super::theme;
use super::viewer::{DIM, WARN};
use crate::contracts::OutputKind;
use crate::hints::HintCfg;
use crate::presets::{self, Pipeline, Preset, PresetClass};
use crate::jobs::JobMode;
use crate::project::{Clip, Project};
use crate::scheduler::*;
use crate::settings::{Settings, HINT_MODES, TEMPORAL_WINDOWS, VRAM_PROFILES};
use crate::worker::{Target, TargetKind};
use crate::shot_domains::{self, Domain};
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
    /// Key/output fields the user changed by hand on this shot (`workflow::is_overridable` names). A Batch
    /// preset is the base; these win over it. Set only by real control edits, never by comparing to defaults.
    /// The value is what the field held before its first hand edit (`None` = a marker saved without one).
    pub explicit: BTreeMap<String, Option<Base>>,
    /// Field -> preset that last set it (deliberate Apply or confirmed queue); the source of a future `Base`.
    pub from_preset: BTreeMap<String, String>,
}

/// The inherited value an overridden field returns to on Reset, and the preset it came from (if known).
#[derive(Clone, Debug, PartialEq)]
pub struct Base {
    pub value: Value,
    pub preset: Option<String>,
}

impl Draft {
    /// The `ck_native.draft` form: `explicit` / `from_preset` are written only when non-empty, so old files
    /// compare equal.
    pub fn to_value(&self) -> Value {
        let mut v = serde_json::json!({"params": self.params, "out": self.out});
        if !self.explicit.is_empty() {
            let m: serde_json::Map<String, Value> = self
                .explicit
                .iter()
                .map(|(k, b)| (k.clone(), b.as_ref().map_or(Value::Null, |b| serde_json::json!({"base": b.value, "preset": b.preset}))))
                .collect();
            v["explicit"] = Value::Object(m);
        }
        if !self.from_preset.is_empty() {
            v["from_preset"] = serde_json::json!(self.from_preset);
        }
        v
    }
    /// Markers saved with a draft (the first build wrote a bare name list); unknown or non-overridable names are dropped.
    pub fn explicit_of(saved: Option<&Value>) -> BTreeMap<String, Option<Base>> {
        match saved.and_then(|d| d.get("explicit")) {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).filter(|k| is_overridable(k)).map(|k| (k.to_owned(), None)).collect(),
            Some(Value::Object(m)) => m
                .iter()
                .filter(|(k, _)| is_overridable(k))
                .map(|(k, v)| {
                    let b = v.get("base").filter(|b| !b.is_null()).map(|b| Base {
                        value: b.clone(),
                        preset: v.get("preset").and_then(Value::as_str).map(str::to_owned),
                    });
                    (k.clone(), b)
                })
                .collect(),
            _ => BTreeMap::new(),
        }
    }
    pub fn from_preset_of(saved: Option<&Value>) -> BTreeMap<String, String> {
        saved
            .and_then(|d| d.get("from_preset"))
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter(|(k, _)| is_overridable(k))
            .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
            .collect()
    }
    /// Keys a Reset can name (sorted).
    pub fn marked(&self) -> Vec<String> {
        self.explicit.keys().cloned().collect()
    }
    pub(crate) fn snapshot(&self) -> (Params, OutputCfg) {
        (self.params.clone(), self.out.clone())
    }
    /// Mark exactly the fields a control just changed relative to `before`; a field's first mark records the
    /// value it had (and its preset, if one set it) as its Reset base.
    pub(crate) fn mark_edits(&mut self, before: &(Params, OutputCfg)) {
        let (mut b, mut a) = (before.0.to_value(), self.params.to_value());
        let (bo, ao) = (before.1.to_value(), self.out.to_value());
        for (k, v) in bo.as_object().into_iter().flatten() {
            b[k.as_str()] = v.clone();
        }
        for (k, v) in ao.as_object().into_iter().flatten() {
            a[k.as_str()] = v.clone();
        }
        for (k, v) in a.as_object().into_iter().flatten() {
            if is_overridable(k) && b.get(k.as_str()) != Some(v) && !self.explicit.contains_key(k.as_str()) {
                let base = Base { value: b[k.as_str()].clone(), preset: self.from_preset.remove(k.as_str()) };
                self.explicit.insert(k.clone(), Some(base));
            }
        }
    }
    /// A deliberate preset Apply carried these fields: their markers go and the preset becomes their source.
    pub(crate) fn applied(&mut self, name: &str, keys: Option<&std::collections::BTreeSet<String>>) {
        let all: Vec<String> = [self.params.to_value(), self.out.to_value()]
            .iter()
            .flat_map(|v| v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default())
            .filter(|k| is_overridable(k))
            .collect();
        for k in all.into_iter().filter(|k| keys.is_none_or(|ks| ks.contains(k))) {
            self.explicit.remove(&k);
            self.from_preset.insert(k, name.to_owned());
        }
    }
    /// A confirmed queue used `base`: overridden fields it speaks about get it as their new Reset base, the
    /// others record it as their source.
    fn confirmed(&mut self, base: &RowBase) {
        let (bp, bo) = (base.params.to_value(), base.out.to_value());
        let speaks = |k: &str| base.carried.as_ref().is_none_or(|c| c.contains(k));
        for (k, b) in self.explicit.iter_mut().filter(|(k, _)| speaks(k)) {
            if let Some(v) = bp.get(k.as_str()).or_else(|| bo.get(k.as_str())) {
                *b = Some(Base { value: v.clone(), preset: base.name.clone() });
            }
        }
        let keys: std::collections::BTreeSet<String> = base.carried.clone().unwrap_or_else(|| {
            [&bp, &bo].iter().flat_map(|v| v.as_object().map(|m| m.keys().cloned().collect::<Vec<_>>()).unwrap_or_default()).collect()
        });
        for k in keys.into_iter().filter(|k| is_overridable(k) && !self.explicit.contains_key(k)) {
            match &base.name {
                Some(n) => self.from_preset.insert(k, n.clone()),
                None => self.from_preset.remove(&k),
            };
        }
    }
    /// Write each marked field back to its stored base (`fallback` supplies it when none was stored) and drop
    /// the markers; returns the preset names the bases came from.
    fn restore_bases(&mut self, fallback: &(Params, OutputCfg)) -> Vec<Option<String>> {
        let marked = std::mem::take(&mut self.explicit);
        let (mut pv, mut ov) = (self.params.to_value(), self.out.to_value());
        let (fp, fo) = (fallback.0.to_value(), fallback.1.to_value());
        let mut sources = vec![];
        for (k, b) in marked {
            let v = b.as_ref().map(|b| b.value.clone()).or_else(|| fp.get(&k).or_else(|| fo.get(&k)).cloned());
            for dst in [&mut pv, &mut ov] {
                if let (Some(d), Some(v)) = (dst.get_mut(k.as_str()), &v) {
                    *d = v.clone();
                }
            }
            if let Some(p) = b.as_ref().and_then(|b| b.preset.clone()) {
                self.from_preset.insert(k.clone(), p);
            }
            sources.push(b.and_then(|b| b.preset));
        }
        if let Ok(x) = serde_json::from_value(pv) {
            self.params = x;
        }
        if let Ok(x) = serde_json::from_value(ov) {
            self.out = x;
        }
        sources
    }
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

#[derive(Clone)]
#[allow(clippy::large_enum_variant)] // a handful of short-lived review rows
enum RowState {
    Ready(Plan),
    Existing {
        reason: String,
        newver: Option<Plan>,
        resume: Option<Plan>,
        replace: Option<Plan>,
    },
    Blocked(String),
}

/// A confirmed Automatic shot with everything its submit needs once a PC is free.
#[derive(Clone)]
struct AutoShot {
    row: BatchRow,
    plan: Plan,
    project: Project,
    st: Settings,
    preset_name: Option<String>,
    /// PCs whose run of this shot failed (`LOCAL_PC` or a `network:` id): never given to them again.
    failed_on: Vec<String>,
    /// Held in the waiting list by the user; not given to any PC until resumed.
    paused: bool,
    /// The PC Automatic gave it to while it runs there (`None` for a manual send).
    on: Option<String>,
}

/// Automatic's key for this computer in `AutoShot::failed_on`/`on`.
const LOCAL_PC: &str = "local";

/// Why a chain's deciding step failed, if Automatic should try another PC. A cancel is the user's choice, and a
/// lost run whose writer is not proven stopped may still be writing to the clip: neither goes to another PC.
fn failure_of(st: &JobState) -> Option<String> {
    match st {
        JobState::Failed { reason } => Some(reason.clone()),
        JobState::Rejected { message, .. } => Some(message.clone()),
        JobState::Lost { reason, writer_stopped: true, .. } => Some(reason.clone()),
        JobState::Done { info, .. } if info.outcome == crate::worker::Outcome::Failed => Some(info.message.clone().unwrap_or_else(|| "failed".into())),
        _ => None,
    }
}

#[derive(Clone)]
struct BatchRow {
    /// Hint-generation steps queued ahead of Key (each depends on the one before; Key on the last).
    steps: Vec<crate::tasks::TaskJob>,
    clip: Clip,
    params: Params,
    out: OutputCfg,
    range: (usize, usize),
    state: RowState,
    choice: Choice,
    /// The client the plan queued while this review was built (per clip: `Automatic` is a policy, not a machine).
    placement: Placement,
    /// Pipeline preset rows: the hint settings saved on the clip once queued.
    hint: Option<HintCfg>,
    /// Why the configured hint producer is not run ("Key only: ..."), or what the hint step uses.
    note: Option<String>,
    export: Option<RowExport>,
    /// Fields this shot keeps from its own edits instead of the chosen preset.
    overrides: Vec<String>,
    /// What the chosen preset resolved to for this shot before its overrides (`None` = no preset chosen).
    base: Option<RowBase>,
    /// The output location this row was planned with (`None` = the clip's default), for a network submission.
    location: Option<Value>,
}

/// The selected preset's resolved values for one shot: what "Use preset values" returns to.
#[derive(Clone)]
struct RowBase {
    params: Params,
    out: OutputCfg,
    /// Fields the preset speaks about (`None` = all; a legacy preset carries only some).
    carried: Option<std::collections::BTreeSet<String>>,
    name: Option<String>,
    /// The pipeline preset's own hint route and video export (what Reset returns those two to).
    hint: Option<HintCfg>,
    export: Option<presets::PipelineExport>,
}

/// The video export that follows Key for this clip (built from the chosen Key plan at confirmation).
#[derive(Clone)]
struct RowExport {
    cfg: presets::PipelineExport,
    /// `(product, file)` per Video delivery, in the order they run after Key.
    dests: Vec<(String, PathBuf)>,
    /// Static reason the export cannot be queued.
    why: Option<String>,
}

/// The preset a name stands for in the saved list (`None` = current settings).
fn resolve_preset(presets: &[Preset], name: &Option<String>) -> Result<Option<BatchPreset>, String> {
    let Some(n) = name else { return Ok(None) };
    Ok(match presets.iter().find(|p| &p.name == n) {
        Some(p) if presets::class_of(p) == PresetClass::Pipeline => Some(BatchPreset::Pipeline(
            Pipeline::from_value(&p.data).map_err(|e| format!("Preset {n} is not usable: {e}"))?,
        )),
        Some(p) => Some(BatchPreset::Legacy(p.data.clone())),
        None => None,
    })
}

/// What a batch review applies to every selected clip.
#[derive(Clone)]
pub enum BatchPreset {
    Pipeline(Pipeline),
    /// Legacy workflow preset: a field merge of parameters/outputs only (no hint route, no export).
    Legacy(Value),
}

/// Why this row's export cannot run on the client it is placed on: it runs on this computer and must read the
/// Key result from the local path, so a remote client cannot run it (`export_block` reports the stored reason).
fn network_target(p: &Placement) -> Option<&str> {
    match p { Placement::Target(id) => id.strip_prefix("network:"), Placement::Auto => None }
}

/// A network PC's picker label: ids are `network id/PC id`.
fn net_label(targets: &[(String, String)], id: &str) -> Option<String> {
    targets.iter().find(|t| t.0 == id).map(|t| t.1.clone())
}

fn export_block(r: &BatchRow) -> Option<String> {
    let x = r.export.as_ref()?;
    if let Some(w) = &x.why {
        return Some(w.clone());
    }
    if x.dests.is_empty() {
        return None;
    }
    None
}

/// What keeps a clip from queueing without a decision, and whether the user may accept it (`OK`).
/// `None` = clean.
fn row_problem(r: &BatchRow, net: &[(String, String)]) -> Option<(String, bool)> {
    match &r.state {
        RowState::Blocked(why) => return Some((format!("not queued: {why}"), false)),
        RowState::Existing { newver: Some(p), .. } => {
            let v = p.values["version"].as_u64().unwrap_or(0);
            return Some((format!("a result exists: OK writes v{v:03}"), true));
        }
        RowState::Existing { reason, newver: None, .. } => return Some((format!("a result exists, no free version: {reason}"), false)),
        RowState::Ready(_) => {}
    }
    if let Some(w) = export_block(r) {
        return Some((format!("export not queued: {w}"), false));
    }
    if let Some(n) = &r.note {
        return Some((n.clone(), true));
    }
    if network_target(&r.placement).is_some_and(|t| net_label(net, t).is_none()) {
        return Some(("assigned PC is not known on any joined network".into(), false));
    }
    if !r.overrides.is_empty() {
        return Some((format!("keeps this clip's own values: {}", r.overrides.join(", ")), true));
    }
    None
}

impl BatchRow {
    /// Hints are regenerated ahead of Key: frames of an interrupted run were keyed with the OLD hint, so
    /// continuing it would mix old and new hints in one result.
    fn regenerates(&self) -> bool {
        self.hint.is_some() && !self.steps.is_empty()
    }
    fn plan(&self) -> Option<&Plan> {
        match (&self.state, self.choice) {
            (RowState::Ready(p), Choice::Queue) => Some(p),
            (RowState::Existing { newver, .. }, Choice::NewVersion) => newver.as_ref(),
            (RowState::Existing { resume, .. }, Choice::Resume) if !self.regenerates() => resume.as_ref(),
            (RowState::Existing { replace, .. }, Choice::Replace) => replace.as_ref(),
            _ => None,
        }
    }
}

/// The batch review: computed in the background, then confirmed by the user. The plans are the frozen jobs.
struct Batch {
    rows: Vec<BatchRow>,
    pending: Option<Receiver<Vec<BatchRow>>>,
    /// Everything needed to recompute the rows when another preset is chosen.
    project: Project,
    st: Settings,
    picks: Vec<usize>,
    /// Presets on disk (workflow kind), the chosen name and its applied content.
    presets: Vec<Preset>,
    preset_name: Option<String>,
    /// `queue_pipeline`: queue as soon as the plan is ready instead of waiting for a review click.
    auto: bool,
    /// The Board button started this: queue straight away when no clip needs a decision.
    quick: bool,
}

mod queue_view;
mod tasks;
pub use queue_view::{pass_name, Cell, Phase, QueueRow, QueueRun, QueueSection, Stage};
pub use tasks::{sig_of, PreviewResult, TaskDone};

/// One shot handed to the network controller (`remote::Remote`): the frozen spec plus what to show for it.
pub struct NetSubmit {
    pub submission_id: String,
    pub spec: crate::submission::JobSpec,
    pub target: Option<String>,
    pub folder: String,
    pub name: String,
}

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
    /// Job ids of finished runs removed with "Clear finished" (hidden from the queue and its log, still tracked).
    cleared: Vec<String>,
    /// Key job id -> the manual SAM/Chroma pre-matte it was queued on top of (display only: no job exists for it).
    precomputed: BTreeMap<String, String>,
    pub placement: Placement,
    /// Draft for the Board's "Assign selected to" control (applied to every selected clip on click).
    assignment: Placement,
    /// Per-clip clients changed in this session, keyed by the clip's ABSOLUTE folder (`None` = Automatic), so
    /// two projects that both have a `shot-01` clip never share an assignment.
    clients: BTreeMap<PathBuf, Option<String>>,
    pub notice: Option<String>,
    /// Recompute cues for the Key / delivery cards: `Some(pulse 0..1)` = stale (set each frame by the Hint feedback tick).
    pub stale_key: Option<f32>,
    pub stale_deliver: Option<f32>,
    /// Why the last `queue_pipeline` queued nothing (blocked row, missing pre-matte, export refused...).
    block_reason: Option<String>,
    /// Bumped when files a gate result depends on may have changed (job ended, submitted, recovered).
    disk_gen: u64,
    status_sig: String,
    gate: Gate,
    batch: Option<Batch>,
    /// `presets.json` beside the preferences (set by the host); the batch preset list is read from it.
    presets_file: Option<PathBuf>,
    /// Delete a recovered run's leftover temp files too (default: keep them).
    lock_remove_temps: bool,
    /// Saved worker targets waiting for the running/unresolved jobs to finish (applied by `tick`).
    pending_targets: Option<Vec<Target>>,
    /// Model assets are being installed or repaired; do not start workers against a changing tree.
    model_mutation_active: bool,
    /// Screen rectangles from the last frame (click tests).
    pub key_rect: Option<egui::Rect>,
    pub batch_confirm_rect: Option<egui::Rect>,
    /// Batch preset dropdown and its open entries (name, rect) from the last frame (click tests).
    pub batch_preset_rect: Option<egui::Rect>,
    /// Key panel Temporal window dropdown and its open items from the last frame (click tests).
    pub temporal_rect: Option<egui::Rect>,
    /// "Use preset values" buttons of the open review by shot folder (click tests).
    pub batch_reset_rects: Vec<(String, egui::Rect)>,
    touched: std::collections::BTreeSet<String>,
    /// Network Batch: the chosen destination, the paired clients to choose from `(installation id, label)`, whether
    /// the coordinator link is online, and the shots waiting to be sent.
    pub net_targets: Vec<(String, String)>,
    pub net_online: bool,
    net_outbox: Vec<NetSubmit>,
    /// Confirmed Automatic shots waiting for a free PC, in order.
    // ponytail: dropped on quit, not saved; the user confirms them again.
    auto_pending: std::collections::VecDeque<AutoShot>,
    /// Each network send's shot by submission id, while a connection follows it: "Send again" puts it back in Automatic.
    sent_shots: std::collections::HashMap<String, AutoShot>,
    /// Automatic shots running on this computer, by the id of their chain's last job.
    auto_live: Vec<(String, AutoShot)>,
    /// Why the last Automatic dispatch left shots waiting (shown after "Waiting for a free PC").
    auto_wait: Option<String>,
    /// How many clients are Automatic candidates (Connect + Accept work, online); set before each dispatch.
    pub auto_clients: usize,
    /// Shots whose clip.json was rewritten here: the App re-reads them (route/markers/bases) on its next tick.
    refresh_clips: Vec<String>,
    /// Re-reads the driver found necessary (published/ended jobs), drained by the UI.
    refreshes: Vec<Refresh>,
    pub temporal_items: Vec<(String, egui::Rect)>,
    /// Board selector (before Review): the chosen saved preset (session only) and its drawn rects (tests).
    board_preset: Option<String>,
    pub board_preset_rect: Option<egui::Rect>,
    pub board_key_rect: Option<egui::Rect>,
    pub board_preset_items: Vec<(String, egui::Rect)>,
    pub batch_preset_items: Vec<(String, egui::Rect)>,
}

fn trunc(s: &str) -> String {
    let t: String = s.chars().take(300).collect();
    if t.len() < s.len() {
        format!("{t}…")
    } else {
        t
    }
}

pub const NO_REPO: &str = "No worker is configured: no Keyloom installation or checkout was found next to the program or in the working folder, and none is set. Add a local target in Settings > AI worker targets, or install/repair the packaged runtime.";

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
        let TargetKind::Local { repo, python } = &t.kind;
        let py = python
            .clone()
            .unwrap_or_else(|| crate::import::python(repo));
        let why = if !repo.join("backend/worker/__main__.py").is_file() {
            Some(format!("{}: {} is not a Keyloom checkout (no backend/worker). Fix the repo folder in Settings > AI worker targets.", t.id, repo.display()))
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
            cleared: vec![],
            precomputed: BTreeMap::new(),
            placement: Placement::Auto,
            assignment: Placement::Auto,
            clients: BTreeMap::new(),
            notice,
            stale_key: None,
            stale_deliver: None,
            disk_gen: 0,
            status_sig: String::new(),
            gate: Gate::default(),
            batch: None,
            presets_file: None,
            lock_remove_temps: false,
            pending_targets: None,
            model_mutation_active: false,
            key_rect: None,
            batch_confirm_rect: None,
            batch_preset_rect: None,
            temporal_rect: None,
            batch_reset_rects: vec![],
            touched: Default::default(),
            net_targets: vec![],
            net_online: false,
            net_outbox: vec![],
            auto_pending: Default::default(),
            sent_shots: Default::default(),
            auto_live: vec![],
            auto_wait: None,
            auto_clients: 0,
            refresh_clips: vec![],
            refreshes: vec![],
            temporal_items: vec![],
            board_preset: None,
            board_preset_rect: None,
            board_key_rect: None,
            board_preset_items: vec![],
            block_reason: None,
            batch_preset_items: vec![],
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
                self.cleared.clear();
                self.notice = None;
                self.disk_gen += 1;
            }
            Err(e) => self.notice = Some(format!("AI targets not applied: {e}")),
        }
    }

    /// Another project is loading: per-clip drafts and the review batch belong to the old one.
    pub fn forget_project(&mut self) {
        self.drafts.clear();
        self.batch = None;
    }

    pub fn draft(&mut self, clip: &Clip, project: &Project, st: &Settings) -> &mut Draft {
        self.drafts.entry(clip.folder.clone()).or_insert_with(|| {
            let (params, out, notes) = seed(clip, project, st);
            let saved = clip.native.get("draft");
            Draft { params, out, notes, explicit: Draft::explicit_of(saved), from_preset: Draft::from_preset_of(saved) }
        })
    }

    /// Review-free full pipeline for ONE clip (hint steps -> Key -> every Video delivery): the same gate, plan and
    /// no-overwrite rules as the batch, queued automatically once ready. Async: resolved in `tick`; when nothing
    /// can be queued the reason is in `pipeline_block_reason` and nothing was queued.
    pub fn queue_pipeline(&mut self, ctx: &egui::Context, project: &Project, st: &Settings, pick: usize, p: &Pipeline) {
        self.block_reason = None;
        self.begin_batch_pipeline(ctx, project, st, &[pick], p);
        match &mut self.batch {
            Some(b) if b.pending.is_some() => b.auto = true,
            _ => self.block_reason = self.notice.take().or(Some("Pipeline could not be started".into())),
        }
    }
    pub fn pipeline_block_reason(&self) -> Option<String> {
        self.block_reason.clone()
    }

    fn resolve_auto_batch(&mut self) {
        let Some(b) = self.batch.as_mut().filter(|b| b.auto) else { return };
        if let Some(rx) = &b.pending {
            match rx.try_recv() {
                Ok(rows) => {
                    b.rows = rows;
                    b.pending = None;
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.batch = None;
                    self.block_reason = Some("Pipeline check was interrupted".into());
                    return;
                }
            }
        }
        let why = b.rows.iter().find_map(|r| match &r.state {
            _ if r.plan().is_some() => export_block(r),
            RowState::Blocked(w) => Some(w.clone()),
            RowState::Existing { reason, .. } => Some(format!("{}: a result already exists ({reason})", r.clip.display_name)),
            RowState::Ready(_) => None,
        });
        match why {
            Some(w) => {
                self.batch = None;
                self.block_reason = Some(w);
            }
            None => self.confirm_batch(),
        }
    }

    /// Poll workers. Publication and completion come back as `Refresh` so the host re-reads the LOCAL manifest.
    /// Everything that advances work, with no egui involved: called by the runtime driver thread (and by `tick`).
    /// Clip folders the UI must re-read are queued for `take_refreshes`.
    pub fn exec_tick(&mut self) {
        self.try_apply_targets();
        self.resolve_auto_batch();
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
                    ..
                } => {
                    self.on_preview(job_id, frame_stem, width, height, mask_png);
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
        for r in out {
            match self.refreshes.iter_mut().find(|x| x.folder == r.folder) {
                Some(x) => x.live &= r.live,
                None => self.refreshes.push(r),
            }
        }
        self.queue_sync(); // durable queue history follows terminal states even while no frame is drawn
    }

    /// `exec_tick` for hosts that drive it from their own loop; returns the pending refreshes.
    pub fn tick(&mut self) -> Vec<Refresh> {
        self.exec_tick();
        self.take_refreshes()
    }

    pub fn has_refreshes(&self) -> bool {
        !self.refreshes.is_empty()
    }

    pub fn take_refreshes(&mut self) -> Vec<Refresh> {
        std::mem::take(&mut self.refreshes)
    }

    /// A clip folder changed on disk outside a scheduler event (a received job finished): the UI re-reads it.
    pub fn push_refresh(&mut self, folder: String) {
        match self.refreshes.iter_mut().find(|x| x.folder == folder) {
            Some(x) => x.live = false,
            None => self.refreshes.push(Refresh { folder, live: false }),
        }
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

    fn placement_text(&self, p: &Placement) -> String {
        match p {
            Placement::Auto => "Automatic (scheduler)".into(),
            Placement::Target(t) => network_target(p).and_then(|id| net_label(&self.net_targets, id)).unwrap_or_else(|| t.clone()),
        }
    }

    /// The clip's own client, if it has one: the in-session value wins over the file, so a change shows and
    /// queues immediately without reloading the project.
    pub fn own_client(&self, clip: &Clip) -> Option<String> {
        match self.clients.get(&clip.root) {
            Some(v) => v.clone(),
            None => crate::persist::clip_client(clip),
        }
    }

    /// The client a clip is assigned to: its own choice, else the session default.
    pub fn clip_placement(&self, clip: &Clip) -> Placement {
        match self.own_client(clip) {
            Some(id) => Placement::Target(id),
            None => self.placement.clone(),
        }
    }

    /// Persist a clip's own client (`None` = follow the session default) and keep the session's copy in step.
    pub fn set_clip_client(&mut self, clip: &Clip, id: Option<&str>) -> Result<(), String> {
        crate::persist::save_client(clip, id)?;
        self.clients.insert(clip.root.clone(), id.map(str::to_owned));
        self.disk_gen += 1;
        Ok(())
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
                placement: self.clip_placement(clip),
                after,
                min_vram_gb: None,
            })
            .map_err(|e| self.submit_error_text(clip, &e))?;
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

    pub fn set_presets_file(&mut self, f: Option<PathBuf>) {
        self.presets_file = f;
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
        self.begin_with(ctx, project, st, picks, None, None);
    }

    /// Batch review for a full pipeline preset (pre-matte -> Key -> optional export), applied per clip.
    pub fn begin_batch_pipeline(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        st: &Settings,
        picks: &[usize],
        p: &Pipeline,
    ) {
        if let Err(e) = p.validate() {
            self.notice = Some(format!("Preset not usable: {e}"));
            return;
        }
        self.begin_with(ctx, project, st, picks, Some(BatchPreset::Pipeline(p.clone())), None);
    }

    fn begin_with(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        st: &Settings,
        picks: &[usize],
        preset: Option<BatchPreset>,
        preset_name: Option<String>,
    ) {
        if self.model_mutation_active {
            self.notice = Some(
                "Model installation or repair is active; wait before starting a batch.".into(),
            );
            return;
        }
        let presets = self.batch_presets();
        let caps_block = match &preset {
            Some(BatchPreset::Pipeline(p)) if p.export.has_video() => self
                .local_caps()
                .filter(|c| !c.tasks.get("export_video").copied().unwrap_or(false))
                .map(|_| "FFmpeg is not ready on the worker: video export unavailable".to_string()),
            _ => None,
        };
        let mut reqs = vec![];
        // The client each clip is assigned to, read once: the row shows it and queues exactly it.
        let mut places = vec![];
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
            let (mut params, mut out) = (d.params.clone(), d.out.clone());
            let shot = (d.params.clone(), d.out.clone(), d.explicit.clone());
            let mut location = project.output_location.clone();
            let (mut hint, mut export, mut note, mut perr) = (None, None, None, None);
            let (mut route_kept, mut delivery_kept) = (false, false);
            match &preset {
                Some(BatchPreset::Pipeline(p)) => {
                    (params, out) = (p.params.clone(), p.outputs.clone());
                    // None = each clip's default location, never this project's custom one.
                    location = p.location.clone();
                    hint = Some(p.hint.clone());
                    export = p.export.has_video().then(|| p.export.clone());
                    // The shot's own hint route / video delivery (hand-edited on the shot) win over the preset's.
                    // Only the fields marked on the shot: everything else is the preset's.
                    let own_route = Domain::Route.read(&c.native);
                    if !own_route.is_empty() {
                        hint = Some(shot_domains::route_merge(&p.hint, &HintCfg::load(c), &own_route));
                        route_kept = true;
                    }
                    let own_delivery = Domain::Delivery.read(&c.native);
                    if !own_delivery.is_empty() {
                        let own = c.native.get("delivery").and_then(|v| serde_json::from_value::<presets::PipelineExport>(v.clone()).ok());
                        if let Some(own) = own {
                            export = Some(shot_domains::export_merge(&p.export, &own, &own_delivery)).filter(|e| e.has_video());
                            delivery_kept = true;
                        }
                    }
                    // Bypassing the hint means Key runs without one (the stored preset keeps its mode).
                    if hint.as_ref().is_some_and(|h| h.bypass) {
                        params.hint_mode = "none".into();
                    }
                }
                Some(BatchPreset::Legacy(v)) => {
                    if let Err(e) = presets::apply_workflow(v, &mut params, &mut out) {
                        perr = Some(format!("preset: {e}"));
                    }
                    note = Some("legacy preset: parameters/outputs only".to_string());
                }
                None => {}
            }
            // Preset base -> the shot's explicit overrides (hand-edited fields, its own hint-mode pick) win over
            // either preset kind. A shot with no recorded edits simply takes the preset.
            let mut overrides: Vec<String> = vec![];
            let mut base = None;
            if let Some(pr) = &preset {
                let carried = match pr {
                    BatchPreset::Legacy(v) => Some(
                        ["params", "outputs"]
                            .iter()
                            .flat_map(|k| v.get(k).and_then(Value::as_object).into_iter().flat_map(|m| m.keys().cloned()))
                            .collect(),
                    ),
                    BatchPreset::Pipeline(_) => None,
                };
                let (bh, be) = match pr {
                    BatchPreset::Pipeline(p) => (Some(p.hint.clone()), Some(p.export.clone())),
                    BatchPreset::Legacy(_) => (None, None),
                };
                base = Some(RowBase { params: params.clone(), out: out.clone(), carried, name: preset_name.clone(), hint: bh, export: be });
                let keys: std::collections::BTreeSet<String> = shot.2.keys().cloned().collect();
                copy_fields(&mut params, &mut out, &shot.0, &shot.1, &keys);
                overrides = shot.2.keys().cloned().collect();
                if route_kept {
                    overrides.extend(Domain::Route.read(&c.native).keys().map(|k| format!("hint route: {k}")));
                }
                if delivery_kept {
                    overrides.extend(Domain::Delivery.read(&c.native).keys().map(|k| format!("delivery: {k}")));
                }
                if let Some(m) = crate::persist::explicit_hint_mode(c) {
                    params.use_alpha_hint = m != "none";
                    params.hint_mode = m;
                    overrides.push("hint_mode".into());
                }
            }
            // The clip's own interpretation always wins; a preset never carries the input colour.
            params.input_is_linear = c.input_is_linear;
            let (range, steps) = match (range, perr) {
                (_, Some(e)) => (Err(e), vec![]),
                (Ok(r), None) => match &hint {
                    // Pipeline: the configured producer/refiner re-runs for every clip.
                    Some(h) => match crate::pipeline::hint_steps(
                        c,
                        &project.root,
                        h,
                        &params.hint_mode,
                        r,
                        c.input_is_linear,
                        new_id,
                    ) {
                        Ok(hp) => {
                            note = hp.note;
                            (Ok(r), hp.steps)
                        }
                        Err(e) => (Err(e), vec![]),
                    },
                    // Hint steps the saved route needs before Key (none when bypassed or already covering).
                    None => match crate::hints::plan_steps(
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
                },
                (e, None) => (e, vec![]),
            };
            reqs.push((
                GateReq {
                    sig: String::new(),
                    clip: c.clone(),
                    project_name: project.display_name.clone(),
                    location: location.clone(),
                    params,
                    out,
                    range,
                    steps,
                },
                hint,
                note,
                export,
                overrides,
                base,
                location,
            ));
            places.push(self.clip_placement(c));
        }
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        let root = project.root.clone();
        std::thread::spawn(move || {
            let mut reserved = std::collections::BTreeSet::new();
            let mut rows: Vec<BatchRow> = reqs
                .into_iter()
                .zip(places)
                .map(|((r, hint, note, export, overrides, base, location), placement)| {
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
                                    },
                                    c,
                                )
                            }
                            None => (RowState::Blocked(b.reasons.join("; ")), Choice::Skip),
                        },
                    };
                    let enabled: Vec<String> = r.out.kinds().iter().map(|(k, _)| k.token().to_lowercase()).collect();
                    let export = export.zip(r.range.clone().ok()).map(|(cfg, rg)| {
                        // A video of an output this review does not write (the shot's own delivery against a preset
                        // that disables that output) is refused loudly, never dropped.
                        let missing: Vec<&str> = cfg.videos().iter().map(|(p, _)| *p).filter(|p| !enabled.iter().any(|e| e == p)).collect();
                        if !missing.is_empty() {
                            let why = format!("the delivery wants a video of {} but this review does not write that output (the shot's own delivery against the preset's outputs): fix the delivery or the outputs", missing.join(", "));
                            return RowExport { cfg, dests: vec![], why: Some(why) };
                        }
                        match crate::pipeline::export_dests(&r.clip, &root, &cfg, rg, &mut reserved) {
                            Ok(d) => RowExport { cfg, dests: d, why: caps_block.clone() },
                            Err(e) => RowExport { cfg, dests: vec![], why: Some(e) },
                        }
                    });
                    BatchRow {
                        steps: r.steps.clone(),
                        range: r.range.clone().unwrap_or((0, 0)),
                        clip: r.clip,
                        params: r.params,
                        out: r.out,
                        state,
                        choice,
                        placement,
                        hint,
                        note,
                        export,
                        overrides,
                        base,
                        location,
                    }
                })
                .collect();
            // Two clips writing the same files/folder: the later one is held back, named.
            let hits = crate::pipeline::collisions(
                rows.iter()
                    .filter_map(|r| r.plan().map(|p| (r.clip.display_name.as_str(), &p.job))),
            );
            let live: Vec<usize> = (0..rows.len()).filter(|i| rows[*i].plan().is_some()).collect();
            for (k, msg) in hits {
                let r = &mut rows[live[k]];
                r.state = RowState::Blocked(msg);
                r.choice = Choice::Skip;
            }
            let _ = tx.send(rows);
            ctx.request_repaint();
        });
        self.batch = Some(Batch {
            rows: vec![],
            pending: Some(rx),
            project: project.clone(),
            st: st.clone(),
            picks: picks.to_vec(),
            presets,
            preset_name,
            auto: false,
            quick: false,
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
                let mut tail = String::new();
                if let Some(x) = &r.note {
                    tail += &format!(" [{x}]");
                }
                if !r.overrides.is_empty() {
                    tail += &format!(" [shot overrides: {}]", r.overrides.join(", "));
                }
                if let Some(x) = &r.export {
                    tail += &match &x.why {
                        Some(w) => format!(" [export blocked: {w}]"),
                        None => x.dests.iter().map(|d| format!(" [export {} -> {}]", d.0, d.1.display())).collect(),
                    };
                }
                let line = match &r.state {
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
                };
                line + &tail
            })
            .collect()
    }
    /// Key settings and outputs one review row will queue (tests).
    pub fn batch_row_settings(&self, folder: &str) -> Option<(Params, OutputCfg)> {
        let r = self.batch.as_ref()?.rows.iter().find(|r| r.clip.folder == folder)?;
        Some((r.params.clone(), r.out.clone()))
    }
    /// Mutable draft for a shot (tests).
    pub fn draft_mut(&mut self, clip: &Clip, project: &Project, st: &Settings) -> &mut Draft {
        self.draft(clip, project, st)
    }
    /// A shot's draft (tests / diagnostics).
    pub fn draft_of(&self, folder: &str) -> Option<&Draft> {
        self.drafts.get(folder)
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

    /// The open clip's render client. The value lives in the clip (`ck_native.client`), so Board, Key and the
    /// batch row always show the same assignment; `Automatic` is a scheduling policy, not a second local worker.
    pub fn client_combo(&mut self, ui: &mut Ui, clip: &Clip) {
        // Board cards stay compact: one line, the effective value, no prose (the Key panel explains it).
        let own = self.own_client(clip);
        let effective = self.clip_placement(clip);
        let tv = self.sched.targets();
        let folder = clip.folder.clone();
        // Short enough for a 200 px card; the tooltip says whether it is the clip's or the session's.
        let session = self.placement.clone();
        let shown = own.clone().map_or(Placement::Auto, Placement::Target);
        let mut want: Option<Option<String>> = None;
        let live = match &effective {
            Placement::Auto => "automatic".to_string(),
            Placement::Target(id) => tv.iter().find(|t| &t.id == id).map_or_else(
                || if id.starts_with("network:") { "network client".to_string() } else { "not configured".to_string() },
                |t| target_state_word(&t.status, t.busy_with.is_some()).to_string(),
            ),
        };
        let hint = match &own {
            Some(id) => format!("This clip renders on {id} ({live})."),
            None => format!("No assignment on this clip: {live}. The session default applies."),
        };
        ui.scope(|ui| {
            client_picker(ui, ("clip_client", &folder), &session, &shown, &tv, &self.net_targets, |_, w| want = Some(w));
        })
        .response
        .on_hover_text(hint);
        if let Some(w) = want {
            if let Err(e) = self.set_clip_client(clip, w.as_deref()) {
                self.notice = Some(format!("Client not saved: {e}"));
            }
        }
    }

    /// Render client row for the Key and Deliver panels: what the clip's own choice is, or the effective
    /// session default when it has none, plus the live state of that machine.
    pub fn render_client_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        let own = self.own_client(clip);
        let tv = self.sched.targets();
        let folder = clip.folder.clone();
        let session = self.placement.clone();
        let shown = own.clone().map_or(Placement::Auto, Placement::Target);
        let mut want: Option<Option<String>> = None;
        ui.label(RichText::new("Render client").strong());
        client_picker(ui, ("clip_client", &folder), &session, &shown, &tv, &self.net_targets, |_, w| want = Some(w));
        match &own {
            Some(id) => {
                let live = tv.iter().find(|t| &t.id == id).map_or_else(
                    || if id.starts_with("network:") { "network client".to_string() } else { "not configured any more".to_string() },
                    |t| target_state_word(&t.status, t.busy_with.is_some()).to_string(),
                );
                let label = self.placement_text(&Placement::Target(id.clone()));
                ui.label(
                    RichText::new(format!("This clip · {label} · {live}"))
                        .color(DIM)
                        .size(11.5),
                );
            }
            None => {
                let live = match &self.placement {
                    Placement::Auto => "Automatic: the next free PC (nothing rendering, GPU and VRAM under 50%)".to_string(),
                    Placement::Target(id) => {
                        let st = tv.iter().find(|t| t.id == *id).map_or_else(
                            || "not configured any more".to_string(),
                            |t| target_state_word(&t.status, t.busy_with.is_some()).to_string(),
                        );
                        format!("Session default · {id} · {st}")
                    }
                };
                ui.label(
                    RichText::new(format!("No assignment on this clip. {live}"))
                        .color(DIM)
                        .size(11.5),
                );
            }
        }
        if let Some(w) = want {
            if let Err(e) = self.set_clip_client(clip, w.as_deref()) {
                self.notice = Some(format!("Client not saved: {e}"));
            }
        }
    }

    /// A submit refusal in the user's terms: which clip and folder, which other job holds it, on which client,
    /// and whether that writer is still alive. Never a raw enum dump.
    fn submit_error_text(&self, clip: &Clip, e: &SubmitError) -> String {
        let clip_name = &clip.display_name;
        match e {
            SubmitError::Collision { with } => {
                let (other, stage, state) = match self.sched.job(with) {
                    Some(j) => (
                        j.job.clip_id.rsplit('/').next().unwrap_or(&j.job.clip_id).to_string(),
                        "Key".to_string(),
                        j.state.clone(),
                    ),
                    None => match self.sched.task(with) {
                        Some(t) => (
                            t.job.clip_id.rsplit('/').next().unwrap_or(&t.job.clip_id).to_string(),
                            t.job.kind.label().to_string(),
                            t.state.clone(),
                        ),
                        None => (with.clone(), "another job".into(), JobState::Queued),
                    },
                };
                let live = match &state {
                    JobState::Queued => "queued",
                    JobState::Dispatching { .. } => "starting",
                    JobState::Running { .. } => "running now",
                    JobState::Lost { writer_stopped: true, .. } => "finished, folder not yet cleaned up",
                    JobState::Lost { .. } => "lost: its worker may still be writing",
                    _ => "finished",
                };
                let where_ = match &state {
                    JobState::Dispatching { target } | JobState::Running { target, .. } | JobState::Done { target, .. } | JobState::Rejected { target, .. } | JobState::Lost { target, .. } => {
                        format!(" on {target}")
                    }
                    _ => String::new(),
                };
                let folder = clip
                    .output_dir
                    .as_ref()
                    .map_or_else(|| "the output folder".into(), |p| p.display().to_string());
                format!(
                    "Not queued: {clip_name} writes into {folder}, which {other} · {stage} ({with}) already owns{where_} — that job is {live}. Nothing was started twice. Wait for it to end, or give this clip another output location in Deliver."
                )
            }
            SubmitError::DuplicateId => format!(
                "Not queued: this exact job for {clip_name} is already in the queue. Nothing was started twice."
            ),
            SubmitError::UnknownTarget(id) => format!(
                "Not queued: the client “{id}” assigned to {clip_name} is no longer configured. Pick another client in Properties > Key, or fix it in Clients."
            ),
            SubmitError::UnknownDependency(id) => format!(
                "Not queued: a step before {clip_name} ({id}) is no longer in the queue, so the chain is incomplete."
            ),
            SubmitError::Invalid(e) => format!("Not queued: {clip_name} has an invalid job ({e})."),
        }
    }

    /// A received stage the scheduler refused, in words for the submitting side's job message.
    fn net_submit_error(&self, stage: &str, e: &SubmitError) -> String {
        match e {
            SubmitError::Collision { with } => format!("the {stage} stage writes into a folder that {with} already owns on this computer; nothing was started"),
            SubmitError::DuplicateId => format!("the {stage} stage is already queued on this computer; nothing was started twice"),
            SubmitError::UnknownTarget(id) => format!("this computer's client “{id}” is no longer configured"),
            SubmitError::UnknownDependency(id) => format!("the {stage} stage depends on {id}, which is no longer queued"),
            SubmitError::Invalid(e) => format!("the {stage} stage is invalid on this computer ({e})"),
        }
    }

    /// A hint/pre-matte run just queued for `clip`: if the user never chose a consumption mode for it, Key must
    /// read what was just generated instead of silently staying on the out-of-box `none`.
    pub fn note_generated_hints(&mut self, clip: &Clip, marks_only: bool) {
        if crate::persist::clip_hint_mode(clip).is_some() {
            return; // an explicit choice (including "none") is never overridden; a queued job's frozen "none" is only a snapshot
        }
        let mode = if marks_only { "keyframes" } else { "full" };
        if let Some(d) = self.drafts.get_mut(&clip.folder) {
            d.params.hint_mode = mode.to_string();
            d.notes.push(format!(
                "Hint mode {mode}: set because this clip's hints were just generated. Change it here; choosing none keeps Key from reading them."
            ));
        }
    }

    /// Board "Assign to": one client for every chosen clip, saved per clip. `all` = every clip in the project is chosen.
    pub fn assign_selected_ui(&mut self, ui: &mut Ui, clips: &[&Clip], all: bool) {
        let tv = self.sched.targets();
        let cur = self.assignment.clone();
        let mut want: Option<Option<String>> = None;
        let n = clips.len();
        let label = if all { format!("Assign all {n} clips to") } else { format!("Assign {n} selected to") };
        ui.label(RichText::new(label).color(DIM).size(12.0));
        let session = self.placement.clone();
        client_picker(ui, "assign_selected", &session, &cur, &tv, &self.net_targets, |_, w| want = Some(w));
        if let Some(w) = want {
            self.assignment = match &w {
                None => Placement::Auto,
                Some(id) => Placement::Target(id.clone()),
            };
        }
        if ui
            .add_enabled(n > 0, egui::Button::new(format!("Apply to {n}")))
            .on_disabled_hover_text("Select clips first")
            .on_hover_text("Save this client on every selected clip (each row keeps its own value)")
            .clicked()
        {
            let id = match &self.assignment {
                Placement::Auto => None,
                Placement::Target(id) => Some(id.clone()),
            };
            let mut failed = vec![];
            for c in clips {
                if let Err(e) = self.set_clip_client(c, id.as_deref()) {
                    failed.push(format!("{}: {e}", c.display_name));
                }
            }
            let to = self.placement_text(&self.assignment);
            self.notice = Some(if failed.is_empty() {
                format!("Assigned {n} clip(s) to {to}.")
            } else {
                format!("Assigned {n} clip(s) to {to}; not saved: {}", failed.join(" · "))
            });
        }
    }

    /// True when the pick was saved, so the caller must re-read the clip (in-memory snapshots lack the new mode).
    /// Ids of this clip's tracked tasks (to find the ones a submit just added).
    pub fn task_ids(&self, folder: &str) -> Vec<String> {
        self.tracked.iter().filter(|t| t.folder == folder && !t.label.is_empty()).map(|t| t.job_id.clone()).collect()
    }

    /// The scheduler's last-event stamp of a task: for a finished task, when it finished.
    pub fn task_stamp(&self, id: &str) -> Option<Instant> {
        self.sched.task(id)?.last_activity
    }

    /// A tracked task's frozen job as it was queued (what a finished run actually used).
    pub fn task_job(&self, id: &str) -> Option<Arc<crate::tasks::TaskJob>> {
        Some(self.sched.task(id)?.job.clone())
    }

    /// Tasks of this clip that ended `Complete`: (job id, kind, publish folder name).
    pub fn completed_tasks(&self, folder: &str) -> Vec<(String, crate::tasks::TaskKind, String)> {
        self.tracked
            .iter()
            .filter(|t| t.folder == folder)
            .filter_map(|t| {
                let e = self.sched.task(&t.job_id)?;
                matches!(&e.state, JobState::Done { info, .. } if info.outcome == crate::worker::Outcome::Complete).then(|| {
                    let dir = e.job.publish_dir.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                    (t.job_id.clone(), e.job.kind, dir)
                })
            })
            .collect()
    }

    pub fn hint_controls(&mut self, ui: &mut Ui, clip: &Clip, project: &Project, st: &Settings) -> bool {
        let stale = self.stale_key;
        let d = self.draft(clip, project, st);
        let before = d.params.hint_mode.clone();
        let snap = d.snapshot();
        let p = &mut d.params;
        card(ui, "Hint consumption", stale, |ui| {
        pair(ui, "Hint mode", |ui| choice(ui, "hint_mode", &mut p.hint_mode, &HINT_MODES));
        ui.label(
            RichText::new(match p.hint_mode.as_str() {
                "none" => "Key ignores AlphaHint (bypass = none). Hints may still exist; they are not read.",
                "full" => "Key reads AlphaHint over the whole range.",
                _ => "Key reads AlphaHint on the marked frames and propagates between them.",
            })
            .color(DIM)
            .size(11.5),
        );
        let r = |ui: &mut Ui, l: &str, v: &mut f64| {
            let mut x = *v as f32;
            let mut moved = false;
            pair(ui, l, |ui| {
                fill_slider(ui);
                moved = ui.add(egui::Slider::new(&mut x, 0.0..=1.0).step_by(0.01)).changed();
            });
            if moved {
                *v = r2(x as f64); // only a real drag rewrites (and so marks) the value
            }
        };
        r(ui, "Hint quality", &mut p.hint_quality);
        pair(ui, "Carry hint", |ui| { theme::checkbox(ui, &mut p.carry_hint, "between windows"); });
        r(ui, "Carry quality", &mut p.carry_hint_quality);
        });
        let after = p.hint_mode.clone();
        d.mark_edits(&snap);
        notes(ui, &d.notes);
        // A pick here is the clip's own decision: saved, and it wins over defaults and inferences.
        if after != before {
            let from_preset = clip.native.get("hint_mode_src").and_then(Value::as_str) == Some("preset");
            match crate::persist::save_hint_pick(clip, &after, (&before, from_preset)) {
                Ok(()) => {
                    self.disk_gen += 1;
                    return true;
                }
                Err(e) => self.notice = Some(format!("Hint mode not saved: {e}")),
            }
        }
        false
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
        let stale = self.stale_key;
        let d = self.draft(clip, project, st);
        let snap = d.snapshot();
        let p = &mut d.params;
        let (mut temporal_rect, mut temporal_items) = (None, vec![]);
        card(ui, "Key engine", stale, |ui| {
            temporal_rect = Some(pair(ui, "Temporal window", |ui| temporal_choice_rects(ui, "temporal_window", &mut p.temporal_window, &mut temporal_items)));
            ui.label(
                RichText::new(crate::settings::temporal_explain(
                    &p.temporal_window,
                    range_frames,
                ))
                .color(DIM)
                .size(12.0),
            );
            pair(ui, "VRAM profile", |ui| choice(ui, "vram_profile", &mut p.vram_profile, &VRAM_PROFILES));
            let mut x = p.despill_strength as f32;
            let mut moved = false;
            pair(ui, "Despill", |ui| {
                fill_slider(ui);
                moved = ui.add(egui::Slider::new(&mut x, 0.0..=1.0).step_by(0.1)).changed();
            });
            if moved {
                p.despill_strength = r2(x as f64);
            }
            ui.label(
                RichText::new(format!(
                    "Input interpretation: {} (from Media; the job freezes it)",
                    if linear { "Linear" } else { "sRGB" }
                ))
                .color(DIM)
                .size(12.0),
            );
        });
        d.mark_edits(&snap);
        notes(ui, &d.notes);
        (self.temporal_rect, self.temporal_items) = (temporal_rect, temporal_items);
        let marked = self.shot_overrides(clip);
        if !marked.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Shot overrides (win over any Batch preset): {}", marked.join(", "))).color(DIM).size(12.0));
                if ui.small_button(self.reset_label(clip)).clicked() {
                    self.reset_overrides(clip, project, st);
                }
            });
        }
    }

    /// Names of the fields this shot overrides by hand (sorted; the hint-mode pick counts), for display.
    pub fn shot_overrides(&self, clip: &Clip) -> Vec<String> {
        let mut v: Vec<String> = self.drafts.get(&clip.folder).map(Draft::marked).unwrap_or_default();
        if crate::persist::explicit_hint_mode(clip).is_some() {
            v.push("hint_mode".into());
        }
        v
    }

    /// What "Reset" will honestly return this shot to, from the bases recorded at edit time.
    pub fn reset_label(&self, clip: &Clip) -> String {
        let bases: Vec<Option<&Base>> = self.drafts.get(&clip.folder).map(|d| d.explicit.values().map(Option::as_ref).collect()).unwrap_or_default();
        let hint = crate::persist::explicit_hint_mode(clip).map(|_| crate::persist::hint_mode_base(clip));
        let names: std::collections::BTreeSet<Option<String>> =
            bases.iter().map(|b| b.map(|b| b.preset.clone())).chain(hint.iter().map(|h| h.as_ref().map(|h| h.1.then(|| "preset".to_string())))).map(|o| o.flatten()).collect();
        let known = bases.iter().all(Option::is_some) && hint.as_ref().is_none_or(Option::is_some);
        match (known, names.iter().collect::<Vec<_>>().as_slice()) {
            (true, [Some(p)]) if p != "preset" => format!("Reset to preset “{p}”"),
            (true, [Some(_)]) => "Reset to the preset values".into(),
            (true, _) => "Reset to values before editing".into(),
            _ => "Reset to defaults".into(),
        }
    }

    /// Forget this shot's hand edits: each field returns to the value (and preset) it had before its first
    /// edit; a marker saved without one falls back to the clip's seeded defaults. Future Batch presets
    /// inherit again.
    pub fn reset_overrides(&mut self, clip: &Clip, project: &Project, st: &Settings) {
        let pick = crate::persist::explicit_hint_mode(clip).is_some();
        let mut base = clip.clone();
        base.workflow = None; // what Key last ran with is not a base: it holds the very values being reset
        if let Some(n) = base.native.as_object_mut() {
            n.remove("draft");
            if pick {
                n.remove("hint_mode");
            }
        }
        let (sp, so, _) = seed(&base, project, st);
        let d = self.draft(clip, project, st);
        d.restore_bases(&(sp.clone(), so));
        if pick {
            let (value, preset) = crate::persist::hint_mode_base(clip).unwrap_or((sp.hint_mode.clone(), false));
            self.return_hint_pick(clip, &value, preset);
            let d = self.draft(clip, project, st);
            d.params.use_alpha_hint = value != "none";
            d.params.hint_mode = value;
        }
        self.touched.insert(clip.folder.clone());
    }

    /// Give up a hint-mode pick: the mode goes back to `value`, recorded as a preset's result or (not) at all.
    fn return_hint_pick(&mut self, clip: &Clip, value: &str, preset: bool) {
        let saved = if preset { crate::persist::save_preset_hint_mode(clip, value) } else { crate::persist::save_hint_mode(clip, None) };
        if let Err(e) = saved {
            self.notice = Some(format!("Hint mode not reset for {}: {e}", clip.display_name));
            return;
        }
        self.disk_gen += 1;
        // The open review (if any) was built from copies of the clip: keep them in step.
        if let Some(n) = self.batch.as_mut().and_then(|b| b.project.clips.iter_mut().find(|c| c.folder == clip.folder)).and_then(|c| c.native.as_object_mut()) {
            n.remove("hint_mode_base");
            n.remove("hint_mode_src");
            n.remove("hint_mode");
            if preset {
                n.insert("hint_mode".into(), serde_json::json!(value));
                n.insert("hint_mode_src".into(), serde_json::json!("preset"));
            }
        }
    }

    /// Batch "Use preset values" for one shot: its overridden fields (and hint pick) take what the SELECTED
    /// preset resolved to. The review is rebuilt by the caller.
    fn reset_to_row_base(&mut self, folder: &str) {
        let Some((row, project, st)) = self.batch.as_ref().and_then(|b| Some((b.rows.iter().find(|r| r.clip.folder == folder)?, b.project.clone(), b.st.clone()))) else { return };
        let (Some(base), clip) = (row.base.clone(), row.clip.clone()) else { return };
        let d = self.draft(&clip, &project, &st);
        d.confirmed(&base);
        d.restore_bases(&(base.params.clone(), base.out.clone()));
        if crate::persist::explicit_hint_mode(&clip).is_some() {
            let speaks = base.carried.as_ref().is_none_or(|c| c.contains("hint_mode"));
            let value = if speaks { base.params.hint_mode.clone() } else { crate::persist::hint_mode_base(&clip).map_or_else(|| base.params.hint_mode.clone(), |b| b.0) };
            self.return_hint_pick(&clip, &value, true);
            let d = self.draft(&clip, &project, &st);
            d.params.use_alpha_hint = value != "none";
            d.params.hint_mode = value;
        }
        self.reset_route_and_delivery(&clip, &base);
        self.touched.insert(folder.to_owned());
    }

    /// Batch "Use preset values", hint route and video delivery: the shot's own FIELDS take the SELECTED preset's
    /// values (written back to the clip so the shot, when opened, shows them), their markers go, and the open
    /// review's copy of the clip follows. Errors are reported, never silent.
    fn reset_route_and_delivery(&mut self, clip: &Clip, base: &RowBase) {
        let meta = clip.meta_path.clone();
        let mut native = serde_json::Map::new();
        let mut res: Result<(), String> = Ok(());
        let own_route = Domain::Route.read(&clip.native);
        if let (false, Some(ph)) = (own_route.is_empty(), &base.hint) {
            let vals: Vec<(String, Value)> = own_route.keys().map(|k| (k.clone(), shot_domains::route_get(ph, k))).collect();
            let cfg = shot_domains::route_set(&HintCfg::load(clip), &vals);
            res = res.and_then(|()| shot_domains::write_domain(&meta, Domain::Route, &Default::default(), Some(base.name.as_deref()), |n| cfg.write_into(n)));
            native.insert("hint".into(), serde_json::to_value(&cfg).unwrap_or_default());
            native.insert("route_override".into(), Value::Null);
        }
        let own_delivery = Domain::Delivery.read(&clip.native);
        if let (false, Some(pe)) = (own_delivery.is_empty(), &base.export) {
            let own = clip.native.get("delivery").and_then(|v| serde_json::from_value::<presets::PipelineExport>(v.clone()).ok()).unwrap_or_default();
            let vals: Vec<(String, Value)> = own_delivery.keys().map(|k| (k.clone(), shot_domains::export_get(pe, k))).collect();
            let v = serde_json::to_value(shot_domains::export_set_all(&own, &vals)).unwrap_or_default();
            let w = v.clone();
            res = res.and_then(|()| shot_domains::write_domain(&meta, Domain::Delivery, &Default::default(), Some(base.name.as_deref()), |n| {
                n.insert("delivery".into(), w);
            }));
            native.insert("delivery".into(), v);
            native.insert("delivery_override".into(), Value::Null);
        }
        match res {
            Ok(()) => {
                if let Some(n) = self.batch.as_mut().and_then(|b| b.project.clips.iter_mut().find(|c| c.folder == clip.folder)).and_then(|c| c.native.as_object_mut()) {
                    for (k, v) in native {
                        if v.is_null() {
                            n.remove(&k);
                        } else {
                            n.insert(k, v);
                        }
                    }
                }
                self.refresh_clips.push(clip.folder.clone());
                self.disk_gen += 1;
            }
            Err(e) => self.notice = Some(format!("Route/delivery not reset for {}: {e}", clip.display_name)),
        }
    }

    /// Shots whose draft was changed by a deliberate action (reset, confirmed queue with overrides): the host
    /// adopts them so Save writes them even when the shot was never opened.
    pub fn take_refresh(&mut self) -> Vec<String> {
        let mut v = std::mem::take(&mut self.refresh_clips);
        v.sort();
        v.dedup();
        v
    }
    pub fn take_touched(&mut self) -> Vec<String> {
        std::mem::take(&mut self.touched).into_iter().collect()
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
        let stale = self.stale_deliver;
        let d = self.draft(clip, project, st);
        let snap = d.snapshot();
        let o = &mut d.out;
        let row = |ui: &mut Ui, k: OutputKind, en: &mut bool, f: &mut String| {
            if only.is_none_or(|x| x == k) {
                pair(ui, k.token(), |ui| {
                    theme::checkbox(ui, en, "Write");
                    choice(ui, &format!("fmt_{}", k.token()), f, &["exr", "png"]);
                });
            }
        };
        card(ui, "Outputs", stale, |ui| {
            row(ui, OutputKind::Fg, &mut o.fg_enabled, &mut o.fg_format);
            row(ui, OutputKind::Matte, &mut o.matte_enabled, &mut o.matte_format);
            row(ui, OutputKind::Comp, &mut o.comp_enabled, &mut o.comp_format);
            row(ui, OutputKind::Processed, &mut o.processed_enabled, &mut o.processed_format);
            pair(ui, "EXR compression", |ui| {
                egui::ComboBox::from_id_salt("exr_comp")
                    .selected_text(if o.exr_compression == "dwab" { "dwab (lossy)" } else { o.exr_compression.as_str() })
                    .show_ui(ui, |ui| {
                        for c in EXR_COMPRESSIONS {
                            ui.selectable_value(&mut o.exr_compression, c.to_string(), if c == "dwab" { "dwab (lossy)" } else { c });
                        }
                    });
            });
            if o.exr_compression == "dwab" {
                ui.colored_label(WARN, "DWAB is lossy: HDR values are not bit-exact. Choose piz, zip or none for exact data.");
            }
            ui.label(RichText::new("Shared by all EXR exports. The engine writes ZIP EXRs; other compressions are re-encoded and the real value is recorded in the manifest.").color(DIM).size(12.0));
        });
        d.mark_edits(&snap);
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
    fn setup_blockers(&self, placement: &Placement) -> Vec<String> {
        let enabled: Vec<&str> = self
            .targets
            .iter()
            .filter(|t| t.enabled)
            .map(|t| t.id.as_str())
            .collect();
        let relevant: Vec<&str> = match placement {
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
        if all_bad || matches!(placement, Placement::Target(_)) {
            relevant.iter().flat_map(|id| bad(id)).collect()
        } else {
            vec![]
        }
    }

    pub fn worker_controls(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Workers").strong());
            ui.vertical(|ui| {
                // The client this clip renders on is its own setting (above); the machine list and the
                // session default live on the Clients page.
                ui.label(RichText::new("Clients").color(DIM).size(11.5));
                if ui.add_enabled(
                    !self.model_mutation_active,
                    egui::Button::new("Check workers"),
                ).on_hover_text("Start the worker(s) now to see GPU and model identity; disabled during model installation or repair").clicked() {
                    self.sched.probe(None);
                }
            });
            for t in self.sched.targets() {
                ui.horizontal(|ui| {
                    ui.add(egui::Label::new(RichText::new(target_line(&t)).color(DIM).size(12.0)).truncate()).on_hover_text(target_line(&t));
                    let failed = matches!(t.status, TargetStatus::Down(_) | TargetStatus::Refused(_) | TargetStatus::Stopped);
                    if failed && t.busy_with.is_none() && ui.add_enabled(
                        !self.model_mutation_active,
                        egui::Button::new("Retry").small(),
                    ).on_hover_text("Forget this verdict and start the worker again; disabled during model installation or repair").clicked() {
                        self.sched.reset_target(&t.id);
                    }
                });
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
        action: &str,
    ) {
        let g = self.gate_poll(&ui.ctx().clone(), clip, project, st, range, linear);
        let running_here = g.as_ref().and_then(|g| g.main.as_ref().err())
            .and_then(|b| b.lock.as_ref()).and_then(|l| l.job_id.as_deref())
            .filter(|id| matches!(self.sched.state_of(id),
                Some(JobState::Dispatching { .. } | JobState::Running { .. })))
            .map(str::to_owned);
        if let Some(id) = running_here {
            ui.label("Processing this clip…");
            self.job_row(ui, &id, false);
            let go = ui.add_enabled(false, egui::Button::new(action)
                .min_size(egui::Vec2::new(ui.available_width(), 30.0)));
            self.key_rect = Some(go.rect);
            return;
        }
        let placement = self.clip_placement(clip);
        let network = network_target(&placement).is_some();
        let setup = if network { vec![] } else { self.setup_blockers(&placement) };
        let ready = g
            .as_ref()
            .and_then(|g| g.main.as_ref().ok())
            .map(|p| self.sched.readiness(&placement, &p.job, None));
        let mut intent = None;
        let mut recover = None;
        let (mut can, mut why) = if self.model_mutation_active {
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
                _ => (false, "Review output & result actions below".into()),
            }
        };
        if network && g.as_ref().is_some_and(|g| g.main.is_ok() || g.newver.is_some()) && !self.model_mutation_active {
            can = self.net_online;
            why = if can { format!("Send to {}", self.placement_text(&placement)) } else { "Connect to the coordinator in Clients first".into() };
        }
        let new_version = g.as_ref().and_then(|g| g.main.as_ref().err()).and_then(|b| b.existing.as_ref()).and_then(|e| e.next_version)
            .filter(|_| g.as_ref().is_some_and(|g| g.newver.is_some()) && setup.is_empty() && !self.model_mutation_active);
        if new_version.is_some() && !network { can = true; why = "Keeps the existing result in place".into(); }
        let primary_label = new_version.map_or_else(|| action.to_string(), |v| format!("Key as v{v:03}"));
        ui.label(
            RichText::new(why)
                .color(if can { DIM } else { BAD })
                .size(12.0),
        );
        // Centered in the action group, like the Deliver buttons.
        let go = ui
            .vertical_centered(|ui| {
                ui.add_enabled(
                    can,
                    egui::Button::new(RichText::new(primary_label).color(Color32::from_rgb(21, 36, 26))).fill(theme::ACCENT).min_size(egui::Vec2::new(180.0, 34.0)),
                )
            })
            .inner;
        self.key_rect = Some(go.rect);
        if let Some(pulse) = self.stale_key {
            ui.painter().rect_stroke(go.rect.expand(1.0), 4.0, egui::Stroke::new(2.0 + pulse * 1.5, theme::FOCUS), egui::StrokeKind::Outside);
        }
        if go.clicked() { intent = Some(if new_version.is_some() { Intent::NewVersion } else { Intent::New }); }

        // Details scroll; the verdict and the Key button below stay pinned and always reachable.
        let needs_fix = g.as_ref().is_some_and(|g| g.main.as_ref().err().is_some_and(|b| b.existing.is_none()));
        egui::CollapsingHeader::new("Output & result actions").default_open(needs_fix).show(ui, |ui| {
            match &g {
                None => {
                    ui.label(RichText::new("Checking the output folder…").color(DIM));
                }
                Some(g) => {
                    if let Ok(p) = &g.main {
                        ui.label(format!("Frames {}–{} · {} frames", p.job.range.in_stem, p.job.range.out_stem, p.job.range.out_point - p.job.range.in_point + 1));
                        ui.label("Output folder");
                        let path = p.job.manifest_dir.display().to_string();
                        ui.horizontal(|ui| {
                            if ui.small_button("Copy").clicked() { ui.ctx().copy_text(path.clone()); }
                            ui.add(egui::Label::new(&path).truncate()).on_hover_text(&path);
                        });
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
                            if let Some((message, path)) = e.reason.split_once(" exists in ") {
                                ui.colored_label(WARN, format!("{message} exists."));
                                ui.horizontal(|ui| {
                                    if ui.small_button("Copy path").clicked() { ui.ctx().copy_text(path.to_owned()); }
                                    ui.add(egui::Label::new(path).truncate()).on_hover_text(path);
                                });
                            } else {
                                ui.colored_label(WARN, &e.reason);
                            }
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
                                let rep_ready = g.replace.as_ref().map(|p| self.sched.readiness(&placement, &p.job, None));
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
            if let Some(n) = self.notice.clone() {
                ui.colored_label(WARN, n);
            }
            if let Some(n) = self.pending_note() {
                ui.colored_label(WARN, n);
            }
            self.unresolved_ui(ui);
            self.job_status(ui, &clip.folder);
        });
        if let Some((dir, id)) = recover {
            self.notice = Some(match self.release_lock(&dir, &id) {
                Ok(m) | Err(m) => m,
            });
            self.disk_gen += 1;
        }
        if let Some(i) = intent {
            self.replace_confirm = false;
            if network {
                if matches!(i, Intent::Resume | Intent::Replace) {
                    self.notice = Some("Resume/Replace stay local. Choose a new result for the network client.".into());
                } else if let Some(pick) = project.clips.iter().position(|c| c.root == clip.root) {
                    self.begin_batch(&ui.ctx().clone(), project, st, &[pick]);
                }
            } else {
                self.notice = self.submit_current(clip, i).err();
            }
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
            let stage = self
                .tracked
                .iter()
                .find(|t| t.job_id == id)
                .map(|t| t.label.clone())
                .filter(|l| !l.is_empty())
                .unwrap_or_else(|| "Key".to_string());
            ui.label(RichText::new(format!("· {stage}")).color(DIM).size(11.0));
            match &e.state {
                JobState::Queued => {
                    let client = match &e.placement {
                        Placement::Auto => "Automatic".to_string(),
                        Placement::Target(t) => t.clone(),
                    };
                    ui.label(format!("Queued · client {client}"));
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Dispatching { target } => {
                    ui.label(format!("Starting on {target}…"));
                    cancel = ui.small_button("Cancel").clicked();
                }
                JobState::Running { target, phase, done, total } => {
                    ui.label(format!("{target}: {}", trunc(&queue_view::pass_name(phase))));
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
        // Warnings stay out of the way until asked for: one collapsed line per job.
        let warnings = self
            .tracked
            .iter()
            .find(|t| t.job_id == id)
            .map(|t| t.warnings.clone())
            .unwrap_or_default();
        if !warnings.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(format!("{} warning{}", warnings.len(), if warnings.len() == 1 { "" } else { "s" }))
                    .color(WARN)
                    .size(11.0),
            )
            .id_salt(("warnings", id))
            .default_open(false)
            .show(ui, |ui| {
                for w in &warnings {
                    ui.label(RichText::new(trunc(w)).color(WARN).size(11.0));
                }
            });
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

    /// Saved presets a batch can start from (the export-class ones are excluded).
    fn batch_presets(&self) -> Vec<Preset> {
        self.presets_file
            .as_deref()
            .map(|f| presets::load(f).0)
            .unwrap_or_default()
            .into_iter()
            .filter(|p| presets::class_of(p) != PresetClass::Export)
            .collect()
    }

    /// Board preset dropdown: the choice "Key" starts the review from (the same saved file the review lists).
    pub fn board_preset_ui(&mut self, ui: &mut Ui) {
        let saved = self.batch_presets();
        if self.board_preset.as_ref().is_some_and(|n| !saved.iter().any(|p| &p.name == n)) {
            self.board_preset = None;
        }
        self.board_preset_items.clear();
        self.board_preset_rect = None;
        if !saved.is_empty() && self.batch.is_none() {
            let cur = self
                .board_preset
                .as_ref()
                .and_then(|n| saved.iter().find(|p| &p.name == n))
                .map_or("Current settings (no preset)".to_string(), presets::label);
            ui.horizontal(|ui| {
                ui.label("Preset");
                let mut items = vec![];
                let mut pick: Option<Option<String>> = None;
                let cb = egui::ComboBox::from_id_salt("board_preset").selected_text(cur).width(320.0).show_ui(ui, |ui| {
                    let r = ui.selectable_label(self.board_preset.is_none(), "Current settings (no preset)");
                    if r.clicked() {
                        pick = Some(None);
                    }
                    items.push((String::new(), r.rect));
                    for p in &saved {
                        let r = ui.selectable_label(self.board_preset.as_deref() == Some(&p.name), presets::label(p));
                        if r.clicked() {
                            pick = Some(Some(p.name.clone()));
                        }
                        items.push((p.name.clone(), r.rect));
                    }
                });
                self.board_preset_rect = Some(cb.response.rect);
                self.board_preset_items = items;
                if let Some(x) = pick {
                    self.board_preset = x;
                }
            });
        }
    }

    /// Board batch controls: "Key selected N clips" starts the review (nothing is queued yet).
    pub fn batch_ui(&mut self, ui: &mut Ui, project: &Project, st: &Settings, picks: &[usize]) {
        let label = format!("Key selected {} clip{}…", picks.len(), if picks.len() == 1 { "" } else { "s" });
        let enabled = !picks.is_empty() && self.batch.is_none() && !self.model_mutation_active;
        let r = ui.add_enabled_ui(enabled, |ui| theme::primary_button(ui, label)).inner;
        self.board_key_rect = Some(r.rect);
        if r.clicked() {
            let saved = self.batch_presets();
            let name = self.board_preset.clone();
            match resolve_preset(&saved, &name) {
                Ok(preset) => {
                    self.begin_with(&ui.ctx().clone(), project, st, picks, preset, name);
                    if let Some(b) = &mut self.batch {
                        b.quick = true;
                    }
                }
                Err(e) => self.notice = Some(e),
            }
        }
    }

    /// The review: per clip the frozen range, the resolved example path and what will happen; existing results
    /// offer the next free version (never a replace). Confirm queues exactly these jobs on the chosen target.
    pub fn batch_review(&mut self, ui: &mut Ui) {
        self.batch_confirm_rect = None; // a stale rect must never be clicked
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
        let Some(b) = &self.batch else { return };
        if b.quick && !b.rows.is_empty() && b.rows.iter().all(|r| row_problem(r, &self.net_targets).is_none()) {
            self.confirm_batch();
            return;
        }
        let mut confirm = false;
        let mut cancel = false;
        let mut reset_shot: Option<Clip> = None;
        let mut reset_rects: Vec<(String, egui::Rect)> = vec![];
        ui.group(|ui| {
            ui.label(RichText::new("Review batch").strong());
            let net = self.net_targets.clone();
            let Some(b) = self.batch.as_mut() else { return };
            let total = b.rows.len();
            let need = b.rows.iter().filter(|r| row_problem(r, &net).is_some()).count();
            ui.label(RichText::new(format!("{} clip(s) ready; {need} need a decision.", total - need)).color(DIM));
            // The rows scroll; the verdict and the Queue/Cancel buttons stay reachable below them.
            let max_h = (ui.available_height() * 0.5).max(140.0);
            egui::ScrollArea::vertical().max_height(max_h).auto_shrink([false, true]).show(ui, |ui| {
            for r in &mut b.rows {
                let Some((text, ok_allowed)) = row_problem(r, &net) else { continue };
                if !ok_allowed && r.choice != Choice::Skip {
                    r.choice = Choice::Skip; // a problem the user cannot accept never queues
                }
                let ok = if matches!(r.state, RowState::Existing { .. }) { Choice::NewVersion } else { Choice::Queue };
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new(&r.clip.display_name).strong());
                    ui.colored_label(if ok_allowed { WARN } else { BAD }, trunc(&text));
                    if ok_allowed {
                        ui.selectable_value(&mut r.choice, ok, "OK");
                    }
                    ui.selectable_value(&mut r.choice, Choice::Skip, "Skip");
                    if !r.overrides.is_empty() {
                        let b = ui.small_button("Use preset values");
                        reset_rects.push((r.clip.folder.clone(), b.rect));
                        if b.clicked() {
                            reset_shot = Some(r.clip.clone());
                        }
                    }
                });
            }
            });
            ui.horizontal_wrapped(|ui| {
                let n = self
                    .batch
                    .as_ref()
                    .map_or(0, |b| b.rows.iter().filter(|r| r.plan().is_some() && export_block(r).is_none()).count());
                let b = ui.add_enabled(
                    n > 0 && !self.model_mutation_active,
                    egui::Button::new(format!(
                        "Queue {n} clip{}",
                        if n == 1 { "" } else { "s" },
                    )),
                );
                self.batch_confirm_rect = Some(b.rect);
                confirm = b.clicked();
                cancel = ui.button("Cancel").clicked();
            });
            ui.label(RichText::new("Clips queue on their assigned client.").color(DIM).size(12.0));
        });
        self.batch_reset_rects = reset_rects;
        if let Some(c) = reset_shot {
            // Explicit user action: the shot's hand edits are dropped, then the rows are rebuilt from the preset.
            if let Some(name) = self.batch.as_ref().map(|b| b.preset_name.clone()) {
                self.reset_to_row_base(&c.folder);
                self.reload_batch(ui.ctx(), name);
            }
        } else if confirm {
            self.confirm_batch();
        } else if cancel {
            self.batch = None;
        }
    }

    /// Another preset was chosen in the review: recompute every row from that preset (nothing is queued).
    fn reload_batch(&mut self, ctx: &egui::Context, name: Option<String>) {
        let Some(b) = self.batch.take() else { return };
        let preset = match resolve_preset(&b.presets, &name) {
            Ok(p) => p,
            Err(e) => {
                self.notice = Some(e);
                self.batch = Some(b);
                return;
            }
        };
        self.begin_with(ctx, &b.project, &b.st, &b.picks, preset, name);
        if let Some(nb) = &mut self.batch {
            nb.quick = b.quick;
        }
    }

    /// Queue the confirmed review: per clip its hint steps, Key and the optional export, each depending on the
    /// one before. Cross-clip conflicts and unreadable exports are refused before anything is queued.
    pub fn confirm_batch(&mut self) {
        let Some(b) = &self.batch else { return };
        let live = |r: &&BatchRow| r.plan().is_some() && export_block(r).is_none();
        let hits = crate::pipeline::collisions(
            b.rows.iter().filter(live).map(|r| (r.clip.display_name.as_str(), &r.plan().unwrap().job)),
        );
        if !hits.is_empty() {
            self.notice = Some(format!(
                "Nothing queued: {}",
                hits.into_iter().map(|h| h.1).collect::<Vec<_>>().join("; ")
            ));
            if let Some(b) = &mut self.batch {
                b.quick = false; // the review shows its Queue and Cancel buttons instead of refusing again
            }
            return;
        }
        // A PC this computer cannot route to keeps the whole Batch for the user to fix; nothing is consumed.
        let unroutable: Vec<String> = b.rows.iter().filter(|r| r.plan().is_some()).filter_map(|r| {
            let t = network_target(&r.placement)?;
            net_label(&self.net_targets, t).is_none().then(|| format!("{} (assigned PC is not known on any joined network)", r.clip.display_name))
        }).collect();
        if !unroutable.is_empty() {
            self.notice = Some(format!("Nothing queued, the review is kept: pick another client for {}.", unroutable.join("; ")));
            if let Some(b) = &mut self.batch {
                b.quick = false;
            }
            return;
        }
        let Some(b) = self.batch.take() else { return };
        let rows = b.rows;
        let (mut queued, mut waiting, mut errs) = (0, 0, vec![]);
        let mut per_client: BTreeMap<String, usize> = BTreeMap::new();
        for r in rows {
            let Some(p) = r.plan().cloned() else { continue };
            if let Some(w) = export_block(&r) {
                errs.push(format!("{}: export: {w}", r.clip.display_name));
                continue;
            }
            if matches!(r.placement, Placement::Auto) {
                self.auto_pending.push_back(AutoShot { row: r, plan: p, project: b.project.clone(), st: b.st.clone(), preset_name: b.preset_name.clone(), failed_on: vec![], paused: false, on: None });
                waiting += 1;
                continue;
            }
            match self.submit_row(&b.project, &b.st, b.preset_name.as_deref(), &r, p) {
                Ok(_) => {
                    queued += 1;
                    *per_client.entry(self.placement_text(&r.placement)).or_default() += 1;
                }
                Err(e) => errs.push(format!("{}: {e}", r.clip.display_name)),
            }
        }
        let on: Vec<String> = per_client.iter().map(|(k, n)| format!("{n} on {k}")).collect();
        let mut m = format!(
            "Queued {queued} clip(s){}{}.",
            if on.is_empty() { "" } else { ": " },
            on.join(", ")
        );
        if waiting > 0 {
            m += &format!(" {waiting} waiting for a free PC.");
        }
        if !errs.is_empty() {
            m += &format!(" Not queued: {}", errs.join(" "));
        }
        self.notice = Some(m);
    }

    /// Route one planned shot by its placement (network PC, hint pipeline, single plan or chain) and remember its edits.
    /// Returns the id that follows it: the chain's last local job, or the network submission.
    fn submit_row(&mut self, project: &Project, st: &Settings, preset_name: Option<&str>, r: &BatchRow, p: Plan) -> Result<String, String> {
        let res = if let Some(target) = network_target(&r.placement) {
            self.network_row(project, st, preset_name, r, &p, Some(target.to_owned())).map(|n| {
                let id = n.submission_id.clone();
                let mut row = r.clone();
                row.placement = Placement::Auto;
                self.sent_shots.insert(id.clone(), AutoShot { row, plan: p.clone(), project: project.clone(), st: st.clone(), preset_name: preset_name.map(str::to_owned), failed_on: vec![], paused: false, on: None });
                self.net_outbox.push(n);
                id
            })
        } else if r.hint.is_some() {
            self.submit_pipeline_row(r, p)
        } else if r.steps.is_empty() {
            self.submit_plan(&r.clip, p, &r.params, &r.out)
        } else {
            self.submit_chain(&r.clip, r.steps.clone(), Some((p, r.params.clone(), r.out.clone())))
        };
        let id = res?;
        if preset_name.is_some() || r.hint.is_some() {
            // The baked values equal the shot's overrides where it has any: keep their markers.
            let old = self.drafts.get(&r.clip.folder);
            let (explicit, from_preset) = old.map(|d| (d.explicit.clone(), d.from_preset.clone())).unwrap_or_default();
            let mut nd = Draft { params: r.params.clone(), out: r.out.clone(), notes: vec![], explicit, from_preset };
            if let Some(b) = &r.base {
                nd.confirmed(b);
            }
            if !nd.explicit.is_empty() {
                self.touched.insert(r.clip.folder.clone());
            }
            self.drafts.insert(r.clip.folder.clone(), nd);
        }
        Ok(id)
    }

    /// Hand ONE waiting Automatic shot to each free PC: remote ones in `free_remote` (`network:` ids), then this computer.
    /// A PC given a shot is busy at once (a remote job counts from "sending", a local job from its submit), and the
    /// list is never split up front: a finished PC just takes the next shot.
    pub fn dispatch_auto(&mut self, free_remote: &[String], local: &crate::metrics::View) {
        self.auto_wait = None;
        self.settle_auto_local();
        if self.auto_pending.is_empty() || self.model_mutation_active {
            return;
        }
        // Unknown load (no live sample, GPU unreadable) is NOT free.
        let local_free = !self.sched.running() && matches!(local, crate::metrics::View::Live(s) if crate::remote::lightly_loaded(s));
        let pcs = local_free.then_some(None).into_iter().chain(free_remote.iter().map(|id| Some(id.clone())));
        for pc in pcs {
            let key = pc.clone().unwrap_or_else(|| LOCAL_PC.into());
            // The first waiting shot this PC may take: not paused, and never one that already failed on it.
            let Some(i) = self.auto_pending.iter().position(|a| !a.paused && !a.failed_on.contains(&key)) else { continue };
            let Some(mut a) = self.auto_pending.remove(i) else { break };
            let automatic = a.row.placement.clone();
            if let Some(id) = &pc {
                a.row.placement = Placement::Target(id.clone());
            }
            let sent = self.submit_row(&a.project, &a.st, a.preset_name.as_deref(), &a.row, a.plan.clone());
            if let Ok(id) = &sent {
                // Followed to its end: a failure brings it back for another PC.
                a.row.placement = automatic.clone();
                a.on = Some(key.clone());
                if pc.is_none() {
                    self.auto_live.push((id.clone(), a));
                } else if let Some(s) = self.sent_shots.get_mut(id) {
                    s.on = Some(key);
                    s.failed_on = a.failed_on;
                }
                continue;
            }
            if let Err(e) = sent {
                // A refused PC (this one included) is not necessarily the shot's fault: it waits, and the next PC in
                // this tick gets it. The error text can't tell a bad plan from a busy PC, so it is never dropped silently.
                a.row.placement = automatic;
                if pc.is_none() {
                    self.notice = Some(format!("{}: not queued on this PC yet, {e}", a.row.clip.display_name));
                }
                // To the back, so one failing shot never blocks the others.
                self.auto_pending.push_back(a);
            }
        }
        self.auto_wait = self.auto_pending.iter().any(|a| !a.paused).then(|| {
            let clients = if self.auto_clients > 0 { "every client is busy or over 50%" } else { "no client with Connect + Accept work is online" };
            [self.local_wait(local), Some(clients.to_owned())].into_iter().flatten().collect::<Vec<_>>().join("; ")
        });
    }

    /// Why this computer is not free for an Automatic shot, in the order it is checked; None = free.
    fn local_wait(&self, local: &crate::metrics::View) -> Option<String> {
        use crate::metrics::View;
        if self.sched.running() {
            return Some("this PC is busy".into());
        }
        Some(match local {
            View::Live(s) if crate::remote::lightly_loaded(s) => return None,
            View::Live(_) => "this PC is over 50% GPU or VRAM".into(),
            View::Unavailable(s) => format!("this PC's GPU load is unknown: {s}"),
            View::Starting => "this PC's GPU load is unknown: starting".into(),
            View::Stale { .. } => "this PC's GPU load is unknown: the reading is stale".into(),
        })
    }

    /// A send its PC never accepted goes to the end of the Automatic list; false if this session did not send it.
    pub fn requeue_sent(&mut self, submission_id: &str) -> bool {
        let Some(mut a) = self.sent_shots.remove(submission_id) else { return false };
        a.on = None;
        self.notice = Some(format!("{}: back in Automatic, it goes to the next free PC.", a.row.clip.display_name));
        self.auto_pending.push_back(a);
        true
    }

    /// Local Automatic shots whose chain ended: a failure goes back for another PC, anything else is done.
    fn settle_auto_local(&mut self) {
        for (id, a) in std::mem::take(&mut self.auto_live) {
            match self.sched.chain_outcome(&id) {
                None if self.sched.state_of(&id).is_some() => self.auto_live.push((id, a)),
                Some(Err(st)) => match failure_of(&st) {
                    Some(why) => self.auto_failed(a, &why),
                    None => {}
                },
                _ => {}
            }
        }
    }

    /// Network Automatic shots: `outcome(sid)` is the connection's view (see `Remote::sent_outcome`). A failed one
    /// goes back for another PC; a finished, cancelled or no longer followed one is forgotten.
    pub fn settle_sent(&mut self, outcome: impl Fn(&str) -> Option<Result<(), Option<String>>>) {
        let ids: Vec<String> = self.sent_shots.keys().cloned().collect();
        for sid in ids {
            match outcome(&sid) {
                Some(Ok(())) => {}
                Some(Err(Some(why))) => {
                    let a = self.sent_shots.remove(&sid).expect("listed");
                    if a.on.is_some() { self.auto_failed(a, &why) }
                }
                _ => { self.sent_shots.remove(&sid); }
            }
        }
    }

    /// A shot's run failed on `a.on`: to the end of the list, never to that PC again. It stays in Automatic
    /// waiting for another suitable PC; only the user's cancel removes it.
    fn auto_failed(&mut self, mut a: AutoShot, why: &str) {
        let pc = a.on.take().unwrap_or_else(|| LOCAL_PC.into());
        let name = a.row.clip.display_name.clone();
        let label = self.pc_label(&pc);
        a.failed_on.push(pc);
        a.row.placement = Placement::Auto;
        self.notice = Some(format!("{name}: failed on {label} ({why}); back in Automatic for another PC."));
        self.auto_pending.push_back(a);
    }

    /// A PC as people know it: "this PC", or the client's name for a `network:` id.
    fn pc_label(&self, pc: &str) -> String {
        if pc == LOCAL_PC {
            return "this PC".into();
        }
        let id = pc.strip_prefix("network:").unwrap_or(pc);
        self.net_targets.iter().find(|(t, _)| t == id).map(|(_, n)| n.split(" · ").next().unwrap_or(n).trim_end_matches(" (offline)").to_owned()).unwrap_or_else(|| id.to_owned())
    }

    /// Drop the Automatic shots still waiting for a free PC.
    pub fn cancel_auto(&mut self) {
        self.auto_pending.clear();
    }

    fn network_row(&mut self, project: &Project, st: &Settings, preset_name: Option<&str>, r: &BatchRow, plan: &Plan, target: Option<String>) -> Result<NetSubmit, String> {
        if !self.net_online { return Err("The coordinator is not connected; nothing was sent".into()); }
        if plan.job.mode != JobMode::New { return Err("Resume/Replace are local only".into()); }
        if target.as_deref().is_some_and(|t| net_label(&self.net_targets, t).is_none()) {
            return Err("that PC is not known on any joined network; pick it again".into());
        }
        let loc = parse_location(r.location.as_ref())?
            .or_else(|| default_location(&r.clip))
            .map(|mut l| {
                l.root = abs(&l.root);
                l
            })
            .ok_or("this shot has no output folder")?;
        let sv = |k: &str| plan.values.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let values = crate::contracts::ClipOutputValues {
            project: project.display_name.clone(),
            name: r.clip.name.clone(),
            resolution: sv("resolution"),
            date: sv("date"),
            version: plan.values.get("version").and_then(Value::as_u64).unwrap_or(1) as u32,
        };
        let spec = crate::submission::JobSpec::workflow(&crate::submission::WorkflowParts {
            roots: &st.shared_roots,
            clip: &r.clip,
            clip_id: &plan.clip_id,
            manifest_dir: &plan.job.manifest_dir,
            range: plan.job.range.clone(),
            params: &r.params,
            out: &r.out,
            hint_keyframes_abs: plan.job.hint_keyframes_abs.clone(),
            loc: &loc,
            values: &values,
            require: plan.job.require.clone(),
            steps: &r.steps,
            hint: r.hint.as_ref(),
            export: r.export.as_ref().map(|x| (&x.cfg, x.dests.as_slice())),
            preset_name,
        })?;
        commit(&r.clip, &plan, &r.params, &r.out)?; // clip id / output values, as a local queue does
        self.persist_row_settings(&r);
        Ok(NetSubmit { submission_id: format!("sub_{}", new_id()), spec, target: target.clone(), folder: r.clip.folder.clone(), name: r.clip.display_name.clone() })
    }

    /// Shots confirmed for the network, for the controller to send.
    pub fn take_net_outbox(&mut self) -> Vec<NetSubmit> {
        std::mem::take(&mut self.net_outbox)
    }

    /// Hold this computer's GPU for a received attempt (claim → preflight → start acknowledgement → finish).
    /// False when local work is queued or running: the claim must then be declined. Nothing dispatches locally
    /// meanwhile except this attempt's own stages.
    pub fn reserve_network_slot(&mut self, attempt: &str) -> bool {
        !self.model_mutation_active && self.sched.reserve_net(attempt)
    }
    pub fn release_network_slot(&mut self, attempt: &str) {
        self.sched.release_net(attempt);
        self.disk_gen += 1;
    }

    /// Queue a received attempt's stages (hint tasks, Key, export tasks in execution order) on the local client,
    /// each depending on the one before. All-or-nothing: on any refusal everything queued so far is cancelled and
    /// nothing has run. The attempt must hold the GPU (`reserve_network_slot`).
    pub fn submit_network_plan(&mut self, attempt: &str, stages: Vec<crate::submission::Stage>) -> Result<(), String> {
        use crate::submission::Stage;
        if self.model_mutation_active {
            return Err("model installation or repair is active".into());
        }
        let Some(local) = self.sched.targets().iter().find(|t| t.local && t.enabled).map(|t| t.id.clone()) else {
            return Err("no local client is configured".into());
        };
        let ids: Vec<String> = stages
            .iter()
            .map(|s| match s {
                Stage::Task(t) => t.job_id.clone(),
                Stage::Key(k) => k.job_id.clone(),
            })
            .collect();
        if !self.sched.allow_net(attempt, &ids) {
            return Err("this computer's GPU is not reserved for the attempt".into());
        }
        let mut done: Vec<String> = vec![];
        let run = || -> Result<(), String> {
            for st in stages {
                let after: Vec<String> = done.last().cloned().into_iter().collect();
                let (id, folder, label) = match &st {
                    Stage::Task(t) => (t.job_id.clone(), t.clip_root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), t.kind.label().to_string()),
                    Stage::Key(k) => (k.job_id.clone(), k.clip_root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), String::new()),
                };
                match st {
                    Stage::Task(t) => self.sched.submit_task(t, after).map_err(|e| self.net_submit_error(&label, &e))?,
                    Stage::Key(k) => self
                        .sched
                        .submit(Submit { job: k, placement: Placement::Target(local.clone()), after, min_vram_gb: None })
                        .map_err(|e| self.net_submit_error("Key", &e))?,
                }
                self.tracked.push(Tracked { warnings: vec![], job_id: id.clone(), folder: folder.clone(), name: format!("{folder} (network)"), label });
                done.push(id);
            }
            Ok(())
        };
        if let Err(e) = run() {
            for id in &done {
                self.sched.cancel(id);
            }
            return Err(e);
        }
        self.tk.dirty = true;
        self.disk_gen += 1;
        Ok(())
    }

    /// State of a job in this computer's scheduler (network receiver progress).
    pub fn job_state(&self, id: &str) -> Option<JobState> {
        self.sched.state_of(id).cloned()
    }

    /// Persist what a queued shot used (hint settings, route/delivery with their rebased markers, hint mode) once its
    /// work is accepted; local queueing and network submission share it so both leave the shot showing what was queued.
    fn persist_row_settings(&mut self, r: &BatchRow) {
        if let Some(h) = &r.hint {
            if r.base.is_none() {
                if let Err(e) = h.save(&r.clip) {
                    self.notice = Some(format!("Hint settings not saved for {}: {e}", r.clip.display_name));
                }
            }
            // Route and delivery as queued (the preset merged with the shot's own fields) are written together with
            // their rebased markers and source preset, so the shot shows exactly what was queued.
            if let Some(b) = &r.base {
                let meta = &r.clip.meta_path;
                let res = (|| -> Result<(), String> {
                    let mut own = Domain::Route.read(&r.clip.native);
                    for (k, f) in own.iter_mut() {
                        if let Some(h) = &b.hint {
                            *f = shot_domains::Field { base: shot_domains::route_get(h, k), preset: b.name.clone() };
                        }
                    }
                    shot_domains::write_domain(meta, Domain::Route, &own, Some(b.name.as_deref()), |n| h.write_into(n))?;
                    if let Some(x) = &r.export {
                        let mut own = Domain::Delivery.read(&r.clip.native);
                        for (k, f) in own.iter_mut() {
                            if let Some(e) = &b.export {
                                *f = shot_domains::Field { base: shot_domains::export_get(e, k), preset: b.name.clone() };
                            }
                        }
                        let v = serde_json::to_value(&x.cfg).map_err(|e| e.to_string())?;
                        shot_domains::write_domain(meta, Domain::Delivery, &own, Some(b.name.as_deref()), |n| {
                            n.insert("delivery".into(), v);
                        })?;
                    }
                    Ok(())
                })();
                match res {
                    Ok(()) => self.refresh_clips.push(r.clip.folder.clone()),
                    Err(e) => self.notice = Some(format!("Override bases not updated for {}: {e}", r.clip.display_name)),
                }
            }
            // The preset's mode is recorded for reload (a stored `none` must not win), but the shot's own pick stays
            // its pick: only its Reset base follows the preset.
            let saved = if crate::persist::explicit_hint_mode(&r.clip).is_some() {
                crate::persist::set_hint_base(&r.clip, r.base.as_ref().map_or(&r.params.hint_mode, |b| &b.params.hint_mode), true)
            } else {
                crate::persist::save_preset_hint_mode(&r.clip, &r.params.hint_mode)
            };
            if let Err(e) = saved {
                self.notice = Some(format!("Hint mode not saved for {}: {e}", r.clip.display_name));
            }
        }
    }

    /// One pipeline row: hint steps > Key > export, all-or-nothing (an earlier job is cancelled again if a later
    /// one is refused). The clip's hint settings are saved only once everything is queued.
    fn submit_pipeline_row(&mut self, r: &BatchRow, plan: Plan) -> Result<String, String> {
        let exports = match &r.export {
            Some(x) => crate::pipeline::export_jobs(
                &r.clip,
                &plan.job.clip_id,
                r.range,
                &x.cfg,
                &plan.job.manifest_dir,
                &plan.job.outputs.keys().cloned().collect::<Vec<_>>(),
                &x.dests,
                new_id,
            )?,
            None => vec![],
        };
        let mut ids: Vec<String> = vec![];
        let run = || -> Result<(), String> {
            for s in r.steps.clone() {
                let after = ids.last().cloned().into_iter().collect();
                let id = self.submit_task(&r.clip, s, after, 0)?;
                ids.push(id);
            }
            let after = ids.last().cloned().into_iter().collect();
            let id = self.submit_plan_after(&r.clip, plan, &r.params, &r.out, after)?;
            ids.push(id.clone());
            // Sequential: the scheduler forbids sibling writers on one clip unless they are ordered.
            let mut prev = id;
            for j in exports {
                prev = self.submit_task(&r.clip, j, vec![prev], 0)?;
                ids.push(prev.clone());
            }
            Ok(())
        };
        if let Err(e) = run() {
            for id in &ids {
                self.sched.cancel(id);
            }
            return Err(e);
        }
        if let Some(h) = &r.hint {
            if !r.steps.is_empty() && matches!(h.guide, crate::hints::Producer::Sam | crate::hints::Producer::Chroma) {
                self.precomputed.insert(ids[r.steps.len()].clone(), h.guide.label().to_string());
            }
        }
        self.persist_row_settings(r);
        ids.pop().ok_or_else(|| "nothing to queue".into())
    }

    /// Deliver an EXISTING Key result without a new Key run: freezes the clip's current manifest (v3, this
    /// clip's own, complete for the range unless `allow_partial`) and queues one export per Video delivery,
    /// strictly in sequence, to never-overwritten files. Sequence outputs are the files already on disk.
    /// Returns the queued job ids.
    pub fn submit_existing_deliveries(
        &mut self,
        clip: &Clip,
        project_root: &Path,
        range: (usize, usize),
        cfg: &presets::PipelineExport,
    ) -> Result<Vec<String>, String> {
        let (Some(m), Some(dir)) = (&clip.manifest, &clip.manifest_dir) else {
            return Err("this clip has no Key result to export yet".into());
        };
        if m.engine != "corridorkey-v2" || m.version < 3 || m.files.is_none() {
            return Err("the existing result is not a verifiable v3 result: run Key again".into());
        }
        let Some(clip_id) = clip.clip_id.clone().filter(|id| m.clip_id.as_ref() == Some(id)) else {
            return Err("the existing result belongs to another clip".into());
        };
        if !m.complete && !cfg.allow_partial {
            return Err("the existing Key result is incomplete: tick 'Export the partial result' to export it".into());
        }
        let stems = m.source_stems.clone().unwrap_or_default();
        let want: Vec<&String> = clip.input_files.get(range.0..=range.1).ok_or("range outside the clip")?.iter().map(|f| &f.0).collect();
        if !want.iter().all(|w| stems.contains(w)) {
            return Err("the existing Key result does not cover this range: choose a range inside the result, or run Key for it".into());
        }
        for (product, _) in cfg.videos() {
            let files = m.files.as_ref().and_then(|f| f.get(product));
            if !m.enabled.iter().any(|e| e.eq_ignore_ascii_case(product)) || files.is_none_or(|f| f.len() != stems.len()) {
                return Err(format!("the existing result has no complete {product} output"));
            }
        }
        if !cfg.has_video() {
            return Err("no video delivery is selected".into());
        }
        let mut reserved = std::collections::BTreeSet::new();
        let dests = crate::pipeline::export_dests(clip, project_root, cfg, range, &mut reserved)?;
        let products: Vec<String> = m.enabled.iter().map(|e| e.to_lowercase()).collect();
        let jobs = crate::pipeline::export_jobs(clip, &clip_id, range, cfg, dir, &products, &dests, new_id)?;
        let mut ids: Vec<String> = vec![];
        for j in jobs {
            let after = ids.last().cloned().into_iter().collect();
            match self.submit_task(clip, j, after, 0) {
                Ok(id) => ids.push(id),
                Err(e) => {
                    for id in &ids {
                        self.sched.cancel(id);
                    }
                    return Err(e);
                }
            }
        }
        Ok(ids)
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
}

fn target_state_word(status: &TargetStatus, busy: bool) -> &'static str {
    match status {
        TargetStatus::Fresh => "not started",
        TargetStatus::Starting => "starting",
        TargetStatus::Ready if busy => "busy",
        TargetStatus::Ready => "ready",
        TargetStatus::Refused(_) => "refused",
        TargetStatus::Down(_) => "down",
        TargetStatus::Stopping => "stopping",
        TargetStatus::Stopped => "stopped",
    }
}

/// The "no client of its own" entry. A clip cannot force Automatic over a named session default (its client key
/// is only present or absent), so with a named default this entry says it follows that client.
pub fn inherit_text(session: &Placement) -> String {
    match session {
        Placement::Auto => "Automatic".into(),
        Placement::Target(id) => format!("Session default ({id})"),
    }
}

/// One client picker: the clip's inherited choice (`Automatic` or the named session default) or a configured
/// machine, with its live state. `cur` is `Auto` while the clip follows the session. `pick` gets `None` for the
/// inherited choice and `Some(id)` for a machine.
fn client_picker(
    ui: &mut Ui,
    salt: impl std::hash::Hash + std::fmt::Debug,
    session: &Placement,
    cur: &Placement,
    tv: &[TargetView],
    network: &[(String, String)],
    mut pick: impl FnMut(&mut Ui, Option<String>),
) {
    let auto_text = inherit_text(session);
    let note = match session {
        Placement::Auto => "the next free PC (nothing rendering, GPU and VRAM under 50%)".to_string(),
        Placement::Target(id) => format!("this clip renders on {id} until it has its own"),
    };
    let text = match cur {
        Placement::Auto => auto_text.clone(),
        Placement::Target(id) => tv
            .iter()
            .find(|t| &t.id == id)
            .map_or_else(|| id.strip_prefix("network:").and_then(|n| net_label(network, n))
                .map_or_else(|| format!("{id} (unavailable)"), |name| name), |t| t.id.clone()),
    };
    egui::ComboBox::from_id_salt(salt)
        .selected_text(text)
        .show_ui(ui, |ui| {
            if ui
                .selectable_label(
                    matches!(cur, Placement::Auto),
                    format!("{auto_text} — {note}"),
                )
                .clicked()
            {
                pick(ui, None);
                ui.close();
            }
            for (id, name) in network {
                let value = format!("network:{id}");
                if ui.selectable_label(matches!(cur, Placement::Target(id) if id == &value), format!("{name} · network")).clicked() {
                    pick(ui, Some(value));
                    ui.close();
                }
            }
            for t in tv {
                let live = if t.enabled {
                    target_state_word(&t.status, t.busy_with.is_some())
                } else {
                    "disabled"
                };
                let on = matches!(cur, Placement::Target(id) if id == &t.id);
                let label = format!(
                    "{} — {} · {live}",
                    t.id,
                    "this computer"
                );
                if ui.selectable_label(on, label).clicked() {
                    pick(ui, Some(t.id.clone()));
                    ui.close();
                }
            }
        });
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
    format!("{} (local): {st}{hw} · {trust}", t.id)
}

fn notes(ui: &mut Ui, n: &[String]) {
    let info: Vec<_> = n.iter().filter(|n| n.starts_with("Hint mode ")).collect();
    for t in n.iter().filter(|n| !n.starts_with("Hint mode ")) { ui.colored_label(WARN, t); }
    if !info.is_empty() {
        egui::CollapsingHeader::new("Hint source details").show(ui, |ui| { for t in info { ui.label(RichText::new(t).color(DIM).size(12.0)); } });
    }
}

/// The temporal-window combo with plain-language entries; the stored/wire value is the exact preset string.
pub fn temporal_choice(ui: &mut Ui, id: &str, v: &mut String) {
    temporal_choice_rects(ui, id, v, &mut vec![]);
}

/// `temporal_choice` that also reports the closed control's rect and each open item's rect.
pub fn temporal_choice_rects(ui: &mut Ui, id: &str, v: &mut String, items: &mut Vec<(String, egui::Rect)>) -> egui::Rect {
    let r = egui::ComboBox::from_id_salt(id)
        .width(ui.available_width().min(200.0)).truncate()
        .selected_text(if v == "auto" { "Auto".to_string() } else { crate::settings::temporal_label(v) })
        .show_ui(ui, |ui| {
            for o in TEMPORAL_WINDOWS {
                let it = ui.selectable_value(v, o.to_string(), crate::settings::temporal_label(o));
                items.push((o.to_string(), it.rect));
            }
        });
    r.response.rect
}

/// Width of the label column in inspector rows: every label/control pair lines up on it.
pub const LABEL_W: f32 = 118.0;

/// Titled inspector card. `stale = Some(pulse)` draws the persistent amber "needs recompute" outline; `pulse`
/// (0..1) briefly brightens it right after a setting changed.
pub fn card<R>(ui: &mut Ui, title: &str, stale: Option<f32>, add: impl FnOnce(&mut Ui) -> R) -> R {
    // Border width stays fixed (Frame margins include it, so a changing width shifts everything below);
    // the stale amber outline is painted inside the finished frame rect instead.
    let out = egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(77, 94, 82)))
        .corner_radius(6.0)
        .inner_margin(egui::Margin::same(10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(title).strong());
                if stale.is_some() {
                    ui.label(RichText::new("Update required").color(theme::FOCUS).size(11.5));
                }
            });
            ui.add_space(2.0);
            add(ui)
        });
    if let Some(p) = stale {
        let stroke = egui::Stroke::new(2.0 + p * 1.5, theme::FOCUS.lerp_to_gamma(Color32::WHITE, p * 0.55));
        ui.painter().rect_stroke(out.response.rect, 6.0, stroke, egui::StrokeKind::Inside);
    }
    out.inner
}

/// One aligned row: fixed-width label, then the control(s).
pub fn pair<R>(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(LABEL_W, ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| { ui.set_width(LABEL_W); ui.add(egui::Label::new(label).wrap()); },
        );
        add(ui)
    })
    .inner
}

/// Let the next slider take the rest of the row.
pub fn fill_slider(ui: &mut Ui) {
    ui.spacing_mut().slider_width = (ui.available_width() - 64.0).max(80.0);
}

fn choice(ui: &mut Ui, id: &str, v: &mut String, opts: &[&str]) {
    egui::ComboBox::from_id_salt(id)
        .width(ui.available_width().min(200.0)).truncate()
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{load_project, LoadOptions};

    fn plan_for(clip: &Clip) -> Plan {
        let job: crate::jobs::KeyJob = serde_json::from_value(serde_json::json!({
            "job_id": "j", "clip_id": "c", "clip_root": clip.root,
            "range": {"in_point": 0, "out_point": 0, "in_stem": "f", "out_stem": "f"},
            "params": {}, "output_config": {}, "hint_keyframes_abs": null, "outputs": {},
            "manifest_dir": clip.root.join("o"), "mode": "resume", "publish": "final",
            "require": {"protocol": 1, "models": {}}
        }))
        .unwrap();
        Plan { job, clip_id: "c".into(), clip_id_is_new: false, values: Value::Null, values_changed: false, summary: String::new() }
    }

    #[test]
    fn resume_is_refused_when_the_hint_is_regenerated_first() {
        let d = std::env::temp_dir().join(format!("ck-resume-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("clips/a/Frames")).unwrap();
        std::fs::write(d.join("clips/a/Frames/f_1101.png"), b"x").unwrap();
        std::fs::write(d.join("project.json"), r#"{"version":2}"#).unwrap();
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let clip = p.clips[0].clone();
        let row = |hint: Option<HintCfg>, steps: Vec<crate::tasks::TaskJob>| BatchRow {
            steps,
            clip: clip.clone(),
            params: Params::default(),
            out: OutputCfg::default(),
            range: (0, 0),
            state: RowState::Existing {
                reason: "x".into(),
                newver: None,
                resume: Some(plan_for(&clip)),
                replace: None,
            },
            choice: Choice::Resume,
            placement: Placement::Auto,
            hint,
            note: None,
            export: None,
            overrides: vec![],
            base: None,
            location: None,
        };
        let step = crate::tasks::TaskJob {
            job_id: "t".into(),
            kind: crate::tasks::TaskKind::Gvm,
            clip_id: "c".into(),
            clip_root: clip.root.clone(),
            range: Some(crate::contracts::RangeIdentity { in_point: 0, out_point: 0, in_stem: "f".into(), out_stem: "f".into() }),
            params: serde_json::json!({}),
            staging_dir: Some(d.join("stg")),
            publish_dir: Some(clip.root.join("AlphaHint")),
            require: crate::jobs::Require { protocol: 1, models: BTreeMap::new() },
        };
        assert!(row(Some(HintCfg::default()), vec![]).plan().is_some(), "no regeneration: resume is fine");
        assert!(row(None, vec![step.clone()]).plan().is_some(), "non-pipeline rows keep their behaviour");
        assert!(row(Some(HintCfg::default()), vec![step]).plan().is_none(), "regenerated hint: never resume");
        let _ = std::fs::remove_dir_all(d);
    }
    fn auto_fixture(tag: &str, n: usize) -> (AiUi, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("ck-auto-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        for i in 0..n {
            std::fs::create_dir_all(d.join(format!("clips/c{i}/Frames"))).unwrap();
            std::fs::write(d.join(format!("clips/c{i}/Frames/f_1101.png")), b"x").unwrap();
        }
        std::fs::write(d.join("project.json"), r#"{"version":2}"#).unwrap();
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let mut st = Settings::default();
        st.shared_roots = vec![crate::shared_paths::SharedRoot { name: "NAS".into(), local: d.display().to_string(), extra: Default::default() }];
        let mut ai = AiUi::new(&st, None);
        for clip in &p.clips {
            let mut plan = plan_for(clip);
            plan.job.mode = crate::jobs::JobMode::New;
            plan.job.require.models.insert("corridorkey_v2".into(), None);
            plan.values = serde_json::json!({"resolution": "1920x1080", "date": "2026-10-27", "version": 1});
            let row = BatchRow {
                steps: vec![],
                clip: clip.clone(),
                params: Params::default(),
                out: OutputCfg::default(),
                range: (0, 0),
                state: RowState::Ready(plan.clone()),
                choice: Choice::Resume,
                placement: Placement::Auto,
                hint: None,
                note: None,
                export: None,
                overrides: vec![],
                base: None,
                location: None,
            };
            ai.auto_pending.push_back(AutoShot { row, plan, project: p.clone(), st: st.clone(), preset_name: None, failed_on: vec![], paused: false, on: None });
        }
        (ai, d)
    }

    fn busy_view() -> crate::metrics::View {
        crate::metrics::View::Unavailable("no sample".into())
    }

    #[test]
    fn auto_sends_one_shot_to_each_free_pc() {
        let (mut ai, d) = auto_fixture("two", 3);
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into()), ("n/b".into(), "B".into())];
        ai.dispatch_auto(&["network:n/a".into(), "network:n/b".into()], &busy_view());
        let sent = ai.take_net_outbox();
        assert_eq!(sent.len(), 2, "one shot per free PC, the third waits");
        assert_eq!(ai.auto_pending.len(), 1);
        let mut to: Vec<_> = sent.iter().map(|n| n.target.clone().unwrap()).collect();
        to.sort();
        assert_eq!(to, ["n/a", "n/b"]);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn send_again_puts_the_shot_at_the_end_of_automatic_and_the_next_free_pc_gets_it() {
        let (mut ai, d) = auto_fixture("again", 2);
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into()), ("n/b".into(), "B".into())];
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        let sent = ai.take_net_outbox().remove(0);
        assert_eq!(ai.auto_pending.len(), 1);
        assert!(ai.requeue_sent(&sent.submission_id));
        assert_eq!(ai.auto_pending.len(), 2, "back in the list");
        assert_eq!(ai.auto_pending.back().unwrap().row.clip.display_name, sent.name, "at the end");
        assert!(matches!(ai.auto_pending.back().unwrap().row.placement, Placement::Auto));
        assert!(!ai.requeue_sent(&sent.submission_id), "taken back once");
        ai.auto_pending.pop_front();
        ai.dispatch_auto(&["network:n/b".into()], &busy_view());
        let again = ai.take_net_outbox();
        assert_eq!((again.len(), again[0].target.as_deref()), (1, Some("n/b")), "resent to the next free PC");
        assert_ne!(again[0].submission_id, sent.submission_id, "a new send, not the old one");
        ai.settle_sent(|_| None);
        assert!(!ai.requeue_sent(&again[0].submission_id), "no longer followed: forgotten");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_failed_shot_goes_to_the_end_and_never_back_to_the_pc_it_failed_on() {
        let (mut ai, d) = auto_fixture("refail", 2);
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into()), ("n/b".into(), "B".into())];
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        let sent = ai.take_net_outbox().remove(0);
        ai.settle_sent(|sid| (sid == sent.submission_id).then_some(Err(Some("CUDA out of memory".into()))));
        assert_eq!(ai.auto_pending.len(), 2);
        let back = ai.auto_pending.back().unwrap();
        assert_eq!((back.row.clip.display_name.as_str(), back.failed_on.as_slice()), (sent.name.as_str(), &["network:n/a".to_owned()][..]), "at the end, marked");
        assert!(ai.notice.as_deref().unwrap().contains("failed on A (CUDA out of memory); back in Automatic"), "{:?}", ai.notice);
        ai.auto_pending.pop_front(); // only the failed shot is left
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert!(ai.take_net_outbox().is_empty(), "never back to the PC it failed on: it waits");
        assert_eq!(ai.auto_pending.len(), 1);
        ai.dispatch_auto(&["network:n/a".into(), "network:n/b".into()], &busy_view());
        let again = ai.take_net_outbox();
        assert_eq!((again.len(), again[0].target.as_deref()), (1, Some("n/b")), "another PC takes it");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_shot_that_failed_on_every_pc_stays_waiting_in_automatic() {
        let (mut ai, d) = auto_fixture("exhaust", 1);
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into())];
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        let sent = ai.take_net_outbox().remove(0);
        ai.settle_sent(|sid| (sid == sent.submission_id).then_some(Err(Some("boom".into()))));
        assert_eq!(ai.auto_pending[0].failed_on, ["network:n/a".to_owned()], "marked on the only PC");
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert!(ai.take_net_outbox().is_empty(), "never back to the failed PC");
        assert_eq!(ai.auto_pending.len(), 1, "failed on every PC and still waiting for another suitable one");
        assert_eq!(ai.auto_pending[0].row.clip.display_name, sent.name);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn a_cancelled_or_finished_send_is_not_requeued_and_a_paused_shot_waits() {
        let (mut ai, d) = auto_fixture("pause", 2);
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into())];
        ai.auto_pending[0].paused = true;
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        let sent = ai.take_net_outbox();
        assert_eq!(sent.len(), 1, "the paused first shot is skipped, the next one goes");
        assert!(ai.auto_pending[0].paused && ai.auto_pending.len() == 1);
        ai.settle_sent(|_| Some(Err(None))); // cancelled by the user
        assert_eq!(ai.auto_pending.len(), 1, "a cancel never comes back");
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert!(ai.take_net_outbox().is_empty(), "still paused");
        ai.auto_pending[0].paused = false;
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert_eq!(ai.take_net_outbox().len(), 1, "resumed");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn auto_waits_without_free_pc_and_when_models_change() {
        let (mut ai, d) = auto_fixture("none", 2);
        ai.net_online = true;
        ai.dispatch_auto(&[], &busy_view());
        assert_eq!((ai.auto_pending.len(), ai.take_net_outbox().len()), (2, 0));
        ai.model_mutation_active = true;
        ai.net_targets = vec![("n/a".into(), "A".into())];
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert_eq!((ai.auto_pending.len(), ai.take_net_outbox().len()), (2, 0));
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn refused_submission_keeps_the_shot_waiting_across_many_refusals() {
        let (mut ai, d) = auto_fixture("fail", 2);
        ai.net_online = false; // every remote send is refused
        // Several free PCs, all refusing in one tick: the shot is requeued, never dropped.
        let free = ["network:n/a".to_owned(), "network:n/b".to_owned(), "network:n/c".to_owned()];
        for _ in 0..10 {
            ai.dispatch_auto(&free, &busy_view());
        }
        assert_eq!(ai.auto_pending.len(), 2, "refused shots keep waiting in Automatic");
        assert!(matches!(ai.auto_pending.back().unwrap().row.placement, Placement::Auto), "Automatic placement restored");
        assert!(ai.auto_pending.iter().all(|a| a.failed_on.is_empty()), "a send refusal is not a render failure: no PC is excluded");
        // The local PC being busy is also no reason to give up: the shot waits for a remote one.
        ai.net_online = true;
        ai.net_targets = vec![("n/a".into(), "A".into())];
        ai.dispatch_auto(&["network:n/a".into()], &busy_view());
        assert_eq!(ai.take_net_outbox().len(), 1, "the busy local PC does not stop a remote send");
        assert_eq!(ai.auto_pending.len(), 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn requeued_first_shot_goes_to_a_later_pc_in_the_list() {
        let (mut ai, d) = auto_fixture("fail-later", 1);
        ai.net_targets = vec![("n/a".into(), "A".into()), ("n/b".into(), "B".into())];
        ai.net_online = false;
        let a = ["network:n/a".to_owned()];
        let b = ["network:n/b".to_owned()];
        ai.dispatch_auto(&a, &busy_view());
        assert_eq!(ai.auto_pending.len(), 1, "kept after the refusal");
        ai.net_online = true;
        ai.dispatch_auto(&b, &busy_view());
        assert_eq!(ai.take_net_outbox().len(), 1, "the same waiting shot reaches the next free PC");
        let _ = std::fs::remove_dir_all(d);
    }
}
