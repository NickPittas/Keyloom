//! The clip view's activity source: gathers every progress/error/log source of the open clip into
//! `activity::Model`. It is shown as the status-bar Activity popover (`activity_popover`) or as the old fixed
//! strip (`activity_strip`); both paint the same details list and carry out the same buttons.
use super::*;
use crate::ui::activity::{self, Cancel, Level, Step};
use crate::ui::controls::activity::{self as control, Action, ActivityItem, ActivityState, ItemKind, JobState, NoticeLevel};

#[derive(Default)]
pub struct StripState {
    pub open: bool,
    /// Steps injected by tests (the real sources need a worker or a decode failure).
    pub extra: Vec<Step>,
    pub rect: Option<egui::Rect>,
    pub activity: ActivityState,
}

struct GlobalActivityItem {
    id: u64,
    kind: ItemKind,
    title: String,
    detail: String,
    actions: &'static [Action],
    job: Option<String>,
}

    /// The open clip's Activity model, used for its status-bar summary.
pub(crate) struct ActivitySnapshot {
    pub model: activity::Model,
}

impl App {
    /// Every source of the open clip, oldest first: notes, then scheduler steps, then import/viewer activity.
    fn activity_inputs(&mut self, clip: &Clip) -> (Vec<Step>, usize) {
        let mut steps: Vec<Step> = vec![];
        let mut note = |id: &str, text: Option<String>| {
            if let Some(t) = text.filter(|t| !t.is_empty()) {
                let mut s = Step::new(&format!("note:{id}"), "Note", Level::Warn);
                (s.message, s.preview) = (t, true);
                steps.push(s);
            }
        };
        note("ai", self.ai.notice.clone());
        note("hint", self.hint_note_text().map(str::to_owned));
        note("media", self.media.note.clone());
        if self.ai.lock_attention(clip) {
            note("lock", Some("A hint lock on this clip needs a decision: open Activity to release it.".into()));
        }
        let (sched, others) = self.ai.activity_steps(&clip.root);
        steps.extend(sched);
        if let Some((phase, done, total)) = self.imp.progress_of(clip) {
            let mut s = Step::new("import", if phase == "compress" { "Compress frames" } else { "Extract frames" }, Level::Active);
            (s.pc, s.phase, s.done, s.total) = ("This computer".into(), if phase == "compress" { "Compressing" } else { "Extracting" }.into(), done, total);
            steps.push(s);
        } else {
            match self.imp.card_state(clip) {
                CardState::Failed(m) => {
                    let mut s = Step::new("import", "Extract frames", Level::Error);
                    (s.phase, s.message) = ("Failed".into(), m);
                    steps.push(s);
                }
                CardState::Waiting => {
                    let mut s = Step::new("import", "Extract frames", Level::Queued);
                    (s.phase, s.message) = ("Waiting".into(), "Waiting for its turn to extract…".into());
                    steps.push(s);
                }
                _ => {}
            }
        }
        if let Some(v) = &mut self.viewer {
            if let Some((done, total)) = v.buffering() {
                let mut s = Step::new("ram", "RAM preview", Level::Active);
                (s.pc, s.phase, s.done, s.total, s.preview, s.cancel) = ("This computer".into(), "Buffering".into(), done as u64, total as u64, true, Cancel::Ram);
                s.message = "Space cancels".into();
                steps.push(s);
            }
            if let Some(e) = v.decode_error() {
                let mut s = Step::new("decode", "Decoding", Level::Error);
                (s.phase, s.message, s.preview) = ("Failed".into(), e, true);
                steps.push(s);
            }
        }
        steps.extend(self.strip.extra.iter().cloned());
        (steps, others)
    }

    /// The open clip's activity (None with no clip open). `&mut` because the lock and viewer reads refresh state.
    pub(crate) fn activity_snapshot(&mut self) -> Option<ActivitySnapshot> {
        let clip = self.clip().cloned()?;
        let (steps, _) = self.activity_inputs(&clip);
        Some(ActivitySnapshot { model: activity::build(steps) })
    }

    /// Status-bar Activity item: the head step's one-line text and its fraction when it reports a total.
    pub(crate) fn activity_summary(&mut self) -> Option<(String, Option<f32>)> {
        let h = self.activity_snapshot()?.model.head?;
        let mut t = h.stage.clone();
        for p in [&h.pc, &h.phase] {
            if !p.is_empty() {
                t.push_str(" · ");
                t.push_str(p);
            }
        }
        let frac = (h.level == Level::Active && h.total > 0).then(|| h.done.min(h.total) as f32 / h.total as f32);
        Some((t, frac))
    }

    /// Status-bar Activity item: open or close the popover.
    pub(crate) fn activity_toggle(&mut self) {
        self.strip.open = !self.strip.open;
    }
    pub(crate) fn activity_open_on(&mut self, tab: usize, anchor: egui::Rect) {
        self.activity_anchor = Some(anchor);
        self.strip.open = true;
        self.strip.activity.tab = tab.min(2);
    }
    pub(crate) fn activity_is_open(&self) -> bool {
        self.strip.open
    }

    pub(crate) fn route_status(&mut self, out: crate::ui::controls::statusbar::StatusOut) {
        let Some(segment) = out.clicked else { return };
        let anchor = out.segments.iter().find(|(s, _)| *s == segment).map(|(_, r)| *r);
        use crate::ui::controls::statusbar::Segment;
        match segment {
            Segment::Activity | Segment::Notices => {
                let tab = if segment == Segment::Notices { 2 } else { 0 };
                if self.strip.open && self.strip.activity.tab == tab { self.strip.open = false; }
                else if let Some(anchor) = anchor { self.activity_open_on(tab, anchor); }
            }
            Segment::Queue => {
                self.board_page.fx.qtab = "waiting".into();
                self.board_page.fx.drawer = 1;
                self.goto_page(0);
            }
            Segment::Cpu | Segment::Ram | Segment::Gpu | Segment::Vram => self.goto_page(2),
        }
    }

    fn global_activity_items(&self) -> Vec<GlobalActivityItem> {
        let mut rows = Vec::new();
        if let Some(project) = &self.project {
            for clip in &project.clips {
                let (steps, _) = self.ai.activity_steps(&clip.root);
                for step in steps {
                    let id = activity_id(&format!("job:{}:{}", clip.folder, step.id));
                    let kind = match step.level {
                        Level::Active => JobState::Running((step.total > 0).then(|| step.done.min(step.total) as f32 / step.total as f32)),
                        Level::Queued => JobState::Queued,
                        Level::Done | Level::Warn => JobState::Done,
                        Level::Error => JobState::Failed,
                    };
                    let mut detail = Vec::new();
                    for part in [&step.pc, &step.phase, &step.message] {
                        if !part.is_empty() { detail.push(part.as_str()); }
                    }
                    let job = match step.cancel { Cancel::Job(id) if kind != JobState::Done => Some(id), _ => None };
                    rows.push(GlobalActivityItem {
                        id,
                        kind: ItemKind::Job(kind),
                        title: format!("{} · {}", clip.display_name, step.stage),
                        detail: detail.join(" · "),
                        actions: if kind == JobState::Failed { &[Action::Dismiss] } else if job.is_some() { &[Action::Cancel] } else { &[] },
                        job,
                    });
                }
                if let Some((phase, done, total)) = self.imp.progress_of(clip) {
                    let state = if phase == "compress" { "Compress frames" } else { "Extract frames" };
                    rows.push(GlobalActivityItem {
                        id: activity_id(&format!("import:{}", clip.folder)),
                        kind: ItemKind::Job(JobState::Running((total > 0).then_some(done.min(total) as f32 / total as f32))),
                        title: format!("{} · {state}", clip.display_name),
                        detail: "This computer".into(),
                        actions: &[],
                        job: None,
                    });
                } else if let CardState::Failed(message) = self.imp.card_state(clip) {
                    rows.push(GlobalActivityItem {
                        id: activity_id(&format!("import:{}", clip.folder)),
                        kind: ItemKind::Job(JobState::Failed),
                        title: format!("{} · Extract frames", clip.display_name),
                        detail: message,
                        actions: &[Action::Dismiss],
                        job: None,
                    });
                } else if matches!(self.imp.card_state(clip), CardState::Waiting) {
                    rows.push(GlobalActivityItem {
                        id: activity_id(&format!("import:{}", clip.folder)),
                        kind: ItemKind::Job(JobState::Queued),
                        title: format!("{} · Extract frames", clip.display_name),
                        detail: "Waiting for its turn to extract…".into(),
                        actions: &[],
                        job: None,
                    });
                }
            }
            for (i, note) in project.notes.iter().enumerate() {
                rows.push(notice_row(&format!("project:{i}:{note}"), "Project note", note, NoticeLevel::Info));
            }
        }
        for (key, title, detail) in [
            ("ai", "AI", self.ai.notice.as_deref()),
            ("pending", "Notice", self.ai.pending_note()),
            ("load", "Project", self.load_error.as_deref()),
            ("startup", "Startup", self.opts.startup_notice.as_deref()),
            ("save", "Save", self.save_error.as_deref()),
            ("import", "Import", self.imp.notice_text()),
            ("hint", "Hint", self.hint_note_text()),
            ("media", "Media", self.media.note.as_deref()),
        ] {
            if let Some(detail) = detail.filter(|s| !s.is_empty()) {
                rows.push(notice_row(key, title, detail, NoticeLevel::Warning));
            }
        }
        rows.extend(self.strip.extra.iter().map(|step| {
            let kind = match step.level {
                Level::Active => JobState::Running((step.total > 0).then(|| step.done.min(step.total) as f32 / step.total as f32)),
                Level::Queued => JobState::Queued,
                Level::Done | Level::Warn => JobState::Done,
                Level::Error => JobState::Failed,
            };
            GlobalActivityItem { id: activity_id(&format!("test:{}", step.id)), kind: ItemKind::Job(kind), title: step.stage.clone(), detail: step.message.clone(), actions: &[], job: None }
        }));
        rows
    }

    pub(crate) fn notice_count(&self) -> u32 {
        self.active_notice_text().len() as u32
    }

    pub(crate) fn active_notice_text(&self) -> Vec<String> {
        self.global_activity_items().into_iter().filter(|row| matches!(row.kind, ItemKind::Notice(_)) && !self.strip.activity.is_acknowledged(row.id)).map(|row| row.detail).collect()
    }

    pub(crate) fn failure_count(&self) -> u32 {
        let rows = self.global_activity_items();
        let items: Vec<_> = rows.iter().map(GlobalActivityItem::as_item).collect();
        self.strip.activity.unacknowledged_failures(&items) as u32
    }

    /// Project-wide Activity popover above the status bar.
    pub(crate) fn activity_popover(&mut self, ui: &mut Ui, anchor: egui::Rect) {
        if !self.strip.open {
            return;
        }
        let clip = self.clip().cloned();
        let rows = self.global_activity_items();
        let items: Vec<_> = rows.iter().filter(|row| !matches!(row.kind, ItemKind::Notice(_)) || !self.strip.activity.is_acknowledged(row.id)).map(GlobalActivityItem::as_item).collect();
        let out = control::activity_popover(ui.ctx(), anchor, &mut self.strip.activity, &items);
        if let Some(clip) = clip.filter(|clip| self.ai.lock_attention(clip)) {
            let screen = ui.ctx().content_rect();
            let popover_x = anchor.left().min(screen.right() - 480.0).max(screen.left());
            let response = egui::Area::new(ui.scope_id().with("hint-lock-recovery"))
                .order(egui::Order::Foreground)
                .pivot(egui::Align2::LEFT_BOTTOM)
                .fixed_pos(egui::pos2(popover_x + 480.0 + 8.0, anchor.top()))
                .show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.set_max_width(360.0);
                        self.ai.hint_lock_ui(ui, &clip)
                    })
                });
            ui.ctx().move_to_top(response.response.layer_id);
        }
        if out.closed { self.strip.open = false; }
        if let Some((id, Action::Cancel)) = out.action {
            if let Some(job) = rows.iter().find(|row| row.id == id).and_then(|row| row.job.as_deref()) { self.ai.cancel(job); }
        }
    }

    /// Rectangle of the strip from the last frame (layout tests).
    pub fn activity_rect(&self) -> Option<egui::Rect> {
        self.strip.rect.or_else(|| self.shot_activity_anchor())
    }
    pub fn activity_open_for_test(&mut self, open: bool) {
        self.strip.open = open;
    }
    pub fn activity_inject_for_test(&mut self, steps: Vec<Step>) {
        self.strip.extra = steps;
    }
}

impl GlobalActivityItem {
    fn as_item(&self) -> ActivityItem<'_> {
        ActivityItem { id: self.id, kind: self.kind, title: &self.title, detail: &self.detail, time: "", actions: self.actions }
    }
}

fn activity_id(source: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    source.hash(&mut hasher);
    hasher.finish()
}

fn notice_row(source: &str, title: &str, detail: &str, level: NoticeLevel) -> GlobalActivityItem {
    GlobalActivityItem { id: activity_id(&format!("notice:{source}:{detail}")), kind: ItemKind::Notice(level), title: title.into(), detail: detail.into(), actions: &[Action::Dismiss], job: None }
}
