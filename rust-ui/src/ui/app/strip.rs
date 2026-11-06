//! The clip view's activity source: gathers every progress/error/log source of the open clip into
//! `activity::Model`. It is shown as the status-bar Activity popover (`activity_popover`) or as the old fixed
//! strip (`activity_strip`); both paint the same details list and carry out the same buttons.
use super::*;
use crate::ui::activity::{self, Act, Cancel, Level, Step};

#[derive(Default)]
pub struct StripState {
    pub open: bool,
    /// Steps injected by tests (the real sources need a worker or a decode failure).
    pub extra: Vec<Step>,
    pub rect: Option<egui::Rect>,
}

/// Everything the Activity popover and the strip show, gathered once for the open clip.
pub(crate) struct ActivitySnapshot {
    pub clip: Clip,
    pub model: activity::Model,
    /// Other clips with unfinished work.
    pub others: usize,
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
            note("lock", Some("A hint lock on this clip needs a decision: open Details.".into()));
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
        let (steps, others) = self.activity_inputs(&clip);
        Some(ActivitySnapshot { model: activity::build(steps), clip, others })
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
    pub(crate) fn activity_is_open(&self) -> bool {
        self.strip.open
    }

    /// The Activity popover above `anchor` (the status-bar item). Callable from any page, Focus included.
    /// Escape or a click outside it (and outside `anchor`, whose own click toggles it) closes it.
    pub(crate) fn activity_popover(&mut self, ui: &mut Ui, anchor: egui::Rect) {
        if !self.strip.open {
            return;
        }
        if ui.ctx().input(|i| i.key_pressed(egui::Key::Escape)) {
            self.strip.open = false;
            return;
        }
        let Some(snap) = self.activity_snapshot() else { return };
        let (act, rect) = self.activity_details(ui.ctx(), anchor, anchor.width().max(360.0), &snap);
        self.apply_activity(&act);
        // A press on another Foreground layer (a combo list opened from a row) is not outside.
        let outside = ui.ctx().input(|i| i.pointer.any_pressed().then(|| i.pointer.interact_pos()).flatten()).is_some_and(|p| {
            !rect.contains(p) && !anchor.contains(p) && ui.ctx().layer_id_at(p).is_none_or(|l| l.order != egui::Order::Foreground)
        });
        if outside {
            self.strip.open = false;
        }
    }

    /// Carries out the buttons drawn this frame by the line buttons or the details list.
    fn apply_activity(&mut self, act: &Act) {
        match &act.cancel {
            Some(Cancel::Job(id)) => self.ai.cancel(id),
            Some(Cancel::Ram) => {
                if let Some(v) = &mut self.viewer {
                    v.pause();
                }
            }
            _ => {}
        }
        if act.open_queue {
            self.back_requested = true;
        }
    }

    /// The old fixed strip (inside its fixed bottom panel); its Details open upward from the strip.
    pub(super) fn activity_strip(&mut self, ui: &mut Ui) {
        let Some(snap) = self.activity_snapshot() else { return };
        let act = activity::paint(ui, &snap.model, self.strip.open, snap.others);
        self.apply_activity(&act);
        if act.toggle_details {
            self.strip.open = !self.strip.open;
        }
        if self.strip.open {
            if let Some(r) = self.strip.rect {
                let (d, _) = self.activity_details(ui.ctx(), r, (r.width() - 24.0).max(100.0), &snap);
                self.apply_activity(&d);
            }
        }
    }

    /// Details list in a popup opening upward from `anchor`: capped, scrolling, one row per step id.
    /// Returns the buttons pressed in it (job rows act on their own) and the popup's rect.
    #[allow(deprecated)] // egui 0.37 wants IdSalt for areas; a fixed salt string is all this needs
    fn activity_details(&mut self, ctx: &egui::Context, anchor: egui::Rect, width: f32, snap: &ActivitySnapshot) -> (Act, egui::Rect) {
        let mut act = Act::default();
        let jobs = self.ai.job_ids();
        let room = (anchor.min.y - 60.0).max(80.0);
        let area = egui::Area::new(egui::Id::new("activity_details"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(anchor.left_top())
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(width);
                    egui::ScrollArea::vertical().id_salt("activity_details_scroll").max_height(activity::DETAILS_MAX_H.min(room)).auto_shrink([false, true]).show(ui, |ui| {
                        if snap.others > 0 && ui.link(RichText::new(format!("{} other clip{} running · Queue", snap.others, if snap.others == 1 { "" } else { "s" })).size(12.0)).clicked() {
                            act.open_queue = true;
                        }
                        let notice = self.imp.notice_text().map(str::to_owned);
                        if let Some(t) = &notice {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(t).color(theme::warn(ui)).size(12.0));
                                if ui.small_button("Dismiss").clicked() {
                                    self.imp.dismiss_notice();
                                }
                            });
                        }
                        if snap.model.list.is_empty() && notice.is_none() {
                            ui.label(RichText::new("Nothing has run on this clip yet.").color(theme::text_dim(ui)));
                        }
                        for s in &snap.model.list {
                            if jobs.contains(&s.id) {
                                self.ai.activity_row(ui, &s.id);
                            } else {
                                let col = match s.level {
                                    Level::Error => theme::error(ui),
                                    Level::Warn => theme::warn(ui),
                                    _ => theme::text_dim(ui),
                                };
                                let mut t = s.stage.clone();
                                for p in [&s.pc, &s.phase, &s.message] {
                                    if !p.is_empty() {
                                        t.push_str(" · ");
                                        t.push_str(p);
                                    }
                                }
                                // Same progress wording as the old strip (activity.rs progress row).
                                if s.total > 0 {
                                    t.push_str(&format!(" · {}/{}", s.done.min(s.total), s.total));
                                } else if s.level == Level::Active {
                                    t.push_str(" · working…");
                                }
                                ui.horizontal_wrapped(|ui| {
                                    ui.add(egui::Label::new(RichText::new(t).color(col).size(12.0)).wrap());
                                    if s.cancel == Cancel::Ram && s.level == Level::Active && ui.small_button("Cancel").clicked() {
                                        act.cancel = Some(Cancel::Ram);
                                    }
                                    if s.id == "decode" && ui.small_button("Retry").clicked() {
                                        if let Some(v) = self.viewer.as_mut() {
                                            v.retry_failed();
                                        }
                                    }
                                });
                            }
                        }
                        self.ai.hint_lock_ui(ui, &snap.clip);
                        if let Some(g) = self.ai.gate_result() {
                            self.ai.output_lock_recovery_ui(ui, &g);
                            if let Some(n) = self.ai.notice.clone().filter(|_| g.main.is_err()) {
                                ui.label(RichText::new(n).color(theme::warn(ui)).size(12.0));
                            }
                        }
                    });
                });
            });
        (act, area.response.rect)
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
