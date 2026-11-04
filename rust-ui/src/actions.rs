//! One decision for every primary action button (panels, node graph, Media tab): is it enabled, what does it
//! say, and why not. The prerequisites themselves (missing model, no input, In not marked, ...) stay the
//! caller's existing checks and arrive as `blocked`; this adds the queue state: "Queue ..." when the clip or the
//! PC is busy, "Already queued" for a duplicate, and the earlier same-clip steps a new job must wait for.
use crate::scheduler::{JobState, Scheduler};
use crate::tasks::{TaskJob, TaskKind};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    SamPreview,
    Track,
    PreMatte,
    Hint,
    Key,
    Export,
    Extract,
}

impl Action {
    /// "Queue track", "Queue generate hint", ... (what the button says while busy).
    pub fn queue_label(self) -> &'static str {
        match self {
            Action::SamPreview => "Queue preview",
            Action::Track => "Queue track",
            Action::PreMatte => "Queue generate pre-matte",
            Action::Hint => "Queue generate hint",
            Action::Key => "Queue Key",
            Action::Export => "Queue export",
            Action::Extract => "Queue extract",
        }
    }

    /// The button action a queued task stands for (`None` = no button of its own).
    pub fn of_task(job: &TaskJob) -> Option<Action> {
        Some(match job.kind {
            TaskKind::SamPreview => Action::SamPreview,
            TaskKind::SamTrack => Action::Track,
            TaskKind::ExportVideo => Action::Export,
            TaskKind::PrepareSource => Action::Extract,
            TaskKind::ImportHint => return None,
            _ if job.publish_dir.as_deref().and_then(Path::file_name).is_some_and(|n| n == "VideoMamaMaskHint") => Action::PreMatte,
            _ => Action::Hint,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Decision {
    pub enabled: bool,
    pub label: String,
    /// Shown next to a disabled button, and (also when enabled and queued) as its hover text.
    pub reason: String,
}

/// Ids of the earlier queued/running steps of THIS clip that `a` reads or writes the same folders as: a hint
/// writer replaces what Key reads, Key's result is what an export reads, so all of them run in order. A frame
/// preview only touches its own staging folder (and the source frames), so it waits just for previews and
/// frame extraction/import.
pub fn waits_for(s: &Scheduler, root: &Path, a: Action) -> Vec<String> {
    let preview_only = |k: TaskKind| matches!(k, TaskKind::SamPreview | TaskKind::PrepareSource | TaskKind::ImportHint);
    let mut ids: Vec<String> = vec![];
    if a != Action::SamPreview {
        ids.extend(s.snapshot().iter().filter(|e| e.state.fences() && e.job.clip_root == root).map(|e| e.job.job_id.clone()));
    }
    ids.extend(
        s.task_snapshot()
            .iter()
            .filter(|t| t.state.fences() && t.job.clip_root == root)
            .filter(|t| if a == Action::SamPreview { preview_only(t.job.kind) } else { t.job.kind != TaskKind::SamPreview })
            .map(|t| t.job.job_id.clone()),
    );
    // A hint written after a Key was queued goes first: that Key consumes it (the scheduler makes the Key wait
    // for the new step). So the hint does not wait for a still-queued Key, nor for anything queued behind one.
    if matches!(a, Action::Track | Action::PreMatte | Action::Hint | Action::Extract) {
        let after: std::collections::HashMap<String, Vec<String>> = s
            .snapshot()
            .iter()
            .map(|e| (e.job.job_id.clone(), e.after.clone()))
            .chain(s.task_snapshot().iter().map(|t| (t.job.job_id.clone(), t.after.clone())))
            .collect();
        let queued_keys: Vec<String> = s.snapshot().iter().filter(|e| e.state == JobState::Queued && e.job.clip_root == root).map(|e| e.job.job_id.clone()).collect();
        let behind_key = |id: &String| {
            let (mut todo, mut seen) = (vec![id.clone()], vec![]);
            while let Some(i) = todo.pop() {
                if queued_keys.contains(&i) {
                    return true;
                }
                if !seen.contains(&i) {
                    todo.extend(after.get(&i).cloned().unwrap_or_default());
                    seen.push(i);
                }
            }
            false
        };
        ids.retain(|i| !behind_key(i));
    }
    ids
}

/// The same action for the same clip is already waiting in the queue.
fn queued_same(s: &Scheduler, root: &Path, a: Action) -> bool {
    match a {
        Action::Key => s.snapshot().iter().any(|e| e.state == JobState::Queued && e.job.clip_root == root),
        _ => s.task_snapshot().iter().any(|t| t.state == JobState::Queued && t.job.clip_root == root && Action::of_task(&t.job) == Some(a)),
    }
}

/// `idle` = the label when nothing is in the way ("Track prompts"); `blocked` = the first failing prerequisite
/// from the caller's existing checks, already worded for the user.
pub fn decide(s: &Scheduler, root: &Path, a: Action, idle: &str, blocked: Option<String>) -> Decision {
    let d = |enabled, label: &str, reason: &str| Decision { enabled, label: label.into(), reason: reason.into() };
    if let Some(why) = blocked {
        return Decision { enabled: false, label: idle.into(), reason: why };
    }
    if queued_same(s, root, a) {
        return d(false, "Already queued", "This is already waiting in the queue for this clip.");
    }
    let n = waits_for(s, root, a).len();
    if n > 0 {
        return d(true, a.queue_label(), &format!("This clip has {n} earlier step{} queued or running: this runs after {}.", if n == 1 { "" } else { "s" }, if n == 1 { "it" } else { "them" }));
    }
    if s.running() {
        return d(true, a.queue_label(), "Another clip is processing: this runs after it.");
    }
    d(true, idle, "")
}

/// A queued step that never started because the step it waited on failed or was cancelled.
pub fn waiting_step_failed(s: &Scheduler, id: &str) -> bool {
    let after = s.job(id).map(|e| e.after.clone()).or_else(|| s.task(id).map(|t| t.after.clone())).unwrap_or_default();
    matches!(s.state_of(id), Some(JobState::Failed { .. })) && after.iter().any(|d| s.state_of(d).is_some_and(|st| st.is_terminal() && !st.completed()))
}
