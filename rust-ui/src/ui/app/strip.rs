//! The clip view's activity strip: gathers every progress/error/log source of the open clip into
//! `activity::Model`, paints the fixed two-line strip and the upward Details list, and carries out its buttons.
use super::*;
use crate::ui::activity::{self, Cancel, Level, Step};

#[derive(Default)]
pub struct StripState {
    pub open: bool,
    /// Steps injected by tests (the real sources need a worker or a decode failure).
    pub extra: Vec<Step>,
    pub rect: Option<egui::Rect>,
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
        let (sched, others) = self.ai.activity_steps(&clip.folder);
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

    /// The strip itself (inside its fixed bottom panel).
    pub(super) fn activity_strip(&mut self, ui: &mut Ui) {
        let Some(clip) = self.clip().cloned() else { return };
        let (steps, others) = self.activity_inputs(&clip);
        let model = activity::build(steps);
        let act = activity::paint(ui, &model, self.strip.open, others);
        match act.cancel {
            Some(Cancel::Job(id)) => self.ai.cancel(&id),
            Some(Cancel::Ram) => {
                if let Some(v) = &mut self.viewer {
                    v.pause();
                }
            }
            _ => {}
        }
        if act.toggle_details {
            self.strip.open = !self.strip.open;
        }
        if act.open_queue {
            self.back_requested = true;
        }
        if self.strip.open {
            self.activity_details(ui.ctx(), &clip, &model);
        }
    }

    /// Details open upward from the strip: capped, scrolling, one row per step id.
    #[allow(deprecated)] // egui 0.37 wants IdSalt for areas; a fixed salt string is all this needs
    fn activity_details(&mut self, ctx: &egui::Context, clip: &Clip, model: &activity::Model) {
        let Some(strip) = self.strip.rect else { return };
        let jobs = self.ai.job_ids();
        let room = (strip.min.y - 60.0).max(80.0);
        egui::Area::new(egui::Id::new("activity_details"))
            .order(egui::Order::Foreground)
            .pivot(egui::Align2::LEFT_BOTTOM)
            .fixed_pos(strip.left_top())
            .show(ctx, |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width((strip.width() - 24.0).max(100.0));
                    egui::ScrollArea::vertical().id_salt("activity_details_scroll").max_height(activity::DETAILS_MAX_H.min(room)).auto_shrink([false, true]).show(ui, |ui| {
                        if model.list.is_empty() {
                            ui.label(RichText::new("Nothing has run on this clip yet.").color(theme::text_dim(ui)));
                        }
                        for s in &model.list {
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
                                ui.label(RichText::new(t).color(col).size(12.0));
                            }
                        }
                        self.ai.hint_lock_ui(ui, clip);
                    });
                });
            });
    }

    /// Rectangle of the strip from the last frame (layout tests).
    pub fn activity_rect(&self) -> Option<egui::Rect> {
        self.strip.rect
    }
    pub fn activity_open_for_test(&mut self, open: bool) {
        self.strip.open = open;
    }
    pub fn activity_inject_for_test(&mut self, steps: Vec<Step>) {
        self.strip.extra = steps;
    }
}
