//! The one activity strip of the clip view: every progress, error and log of the open clip in a fixed
//! two-line place above the system metrics. This file is the pure model (what the strip says) and its painter;
//! `app/strip.rs` gathers the inputs from the scheduler, import, viewer and notes.
use super::theme;
use eframe::egui::{self, RichText, Ui};

/// Fixed strip height: two text lines plus margins. The same on every tab, focus and graph state.
pub const STRIP_H: f32 = 54.0;
/// Details list height cap (it scrolls beyond).
pub const DETAILS_MAX_H: f32 = 260.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Level {
    Active,
    Queued,
    Done,
    Warn,
    Error,
}

/// What the Cancel button of a step does.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Cancel {
    None,
    /// Scheduler job or task.
    Job(String),
    /// RAM-preview buffering in the viewer.
    Ram,
}

#[derive(Clone, Debug)]
pub struct Step {
    /// Job id, or a fixed id for non-job sources ("import", "ram", "decode", "note", "lock").
    pub id: String,
    pub stage: String,
    /// Friendly PC name ("This computer" or the remote PC's name); empty when no PC is involved.
    pub pc: String,
    pub phase: String,
    pub done: u64,
    pub total: u64,
    pub level: Level,
    pub message: String,
    /// A SAM preview: it never clears an earlier error.
    pub preview: bool,
    pub cancel: Cancel,
}

impl Step {
    pub fn new(id: &str, stage: &str, level: Level) -> Self {
        Step { id: id.into(), stage: stage.into(), pc: String::new(), phase: String::new(), done: 0, total: 0, level, message: String::new(), preview: false, cancel: Cancel::None }
    }
}

#[derive(Default, Debug)]
pub struct Model {
    /// The step line 1 describes.
    pub head: Option<Step>,
    /// Error that stays until the next action on the clip (shown on line 2 while something else runs).
    pub error: Option<Step>,
    /// Active first, then queued, then finished newest first; one entry per id.
    pub list: Vec<Step>,
    pub queued: usize,
    /// Latest free-standing note (ids starting "note:"); never the head, shown on line 2.
    pub note: Option<String>,
}

/// `steps` are oldest first. Later entries replace earlier ones with the same id.
pub fn build(steps: Vec<Step>) -> Model {
    let mut uniq: Vec<Step> = Vec::new();
    for s in steps {
        uniq.retain(|o| o.id != s.id);
        uniq.push(s);
    }
    let (notes, uniq): (Vec<Step>, Vec<Step>) = uniq.into_iter().partition(|s| s.id.starts_with("note:"));
    // The error stands until a newer non-preview step exists (the next action on the clip).
    // Trailing previews are skipped: a preview that succeeded afterwards does not clear it.
    let error = uniq.iter().rev().find(|s| !s.preview).filter(|s| s.level == Level::Error).cloned();
    let queued = uniq.iter().filter(|s| s.level == Level::Queued).count();
    let head = uniq
        .iter()
        .find(|s| s.level == Level::Active)
        .cloned()
        .or_else(|| error.clone())
        .or_else(|| uniq.iter().find(|s| s.level == Level::Queued).cloned())
        .or_else(|| uniq.last().cloned());
    let mut list: Vec<Step> = uniq.iter().filter(|s| s.level == Level::Active).cloned().collect();
    list.extend(uniq.iter().filter(|s| s.level == Level::Queued).cloned());
    list.extend(uniq.iter().rev().filter(|s| !matches!(s.level, Level::Active | Level::Queued)).cloned());
    let note = notes.last().map(|n| n.message.clone());
    list.extend(notes);
    Model { head, error, list, queued, note }
}

impl Model {
    /// Line 2 text: the standing error while something else runs, else the head's own message.
    pub fn message(&self) -> Option<(String, Level)> {
        if let Some(e) = self.error.as_ref().filter(|e| self.head.as_ref().is_some_and(|h| h.id != e.id)) {
            return Some((format!("{}: {}", e.stage, e.message), Level::Error));
        }
        if let Some(n) = &self.note {
            return Some((n.clone(), Level::Warn));
        }
        let h = self.head.as_ref()?;
        Some((h.message.clone(), h.level)).filter(|(m, _)| !m.is_empty())
    }
}

/// What the user did on the strip this frame.
#[derive(Default)]
pub struct Act {
    pub cancel: Option<Cancel>,
    pub toggle_details: bool,
    pub open_queue: bool,
}

fn colour(ui: &Ui, l: Level) -> egui::Color32 {
    match l {
        Level::Error => theme::error(ui),
        Level::Warn => theme::warn(ui),
        _ => theme::text_dim(ui),
    }
}

/// Paint the two lines into `ui` (already sized to the strip). `others` = other clips with unfinished work.
pub fn paint(ui: &mut Ui, m: &Model, open: bool, others: usize) -> Act {
    let mut act = Act::default();
    let w = ui.available_width();
    ui.spacing_mut().item_spacing.y = 2.0;
    let right_w = (w * 0.45).clamp(150.0, 300.0);
    // ---- line 1
    ui.horizontal(|ui| {
        ui.set_height(22.0);
        let left_w = (w - right_w - 8.0).max(40.0);
        ui.allocate_ui_with_layout(egui::vec2(left_w, 22.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
            let Some(h) = &m.head else {
                ui.label(RichText::new("No activity yet").color(theme::text_dim(ui)));
                return;
            };
            let mut t = h.stage.clone();
            for p in [&h.pc, &h.phase] {
                if !p.is_empty() {
                    t.push_str(" · ");
                    t.push_str(p);
                }
            }
            ui.add(egui::Label::new(RichText::new(t).strong()).truncate());
        });
        ui.allocate_ui_with_layout(egui::vec2(right_w, 22.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let Some(h) = &m.head else { return };
            if h.cancel != Cancel::None && matches!(h.level, Level::Active | Level::Queued) && ui.small_button("Cancel").clicked() {
                act.cancel = Some(h.cancel.clone());
            }
            if h.level == Level::Active {
                if h.total > 0 {
                    let d = h.done.min(h.total);
                    ui.add(egui::ProgressBar::new(d as f32 / h.total as f32).desired_width((right_w - 70.0).max(60.0)).text(format!("{d}/{}", h.total)));
                } else {
                    // Unknown total: show life, never a made-up percentage.
                    ui.spinner();
                    ui.label(RichText::new("working…").color(theme::text_dim(ui)).size(12.0));
                }
            }
        });
    });
    // ---- line 2
    ui.horizontal(|ui| {
        ui.set_height(20.0);
        let left_w = (w - right_w - 8.0).max(40.0);
        ui.allocate_ui_with_layout(egui::vec2(left_w, 20.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
            if let Some((msg, l)) = m.message() {
                ui.add(egui::Label::new(RichText::new(msg).color(colour(ui, l)).size(12.0)).truncate());
            }
        });
        ui.allocate_ui_with_layout(egui::vec2(right_w, 20.0), egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button(if open { "Details ▾" } else { "Details ▴" }).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                act.toggle_details = true;
            }
            ui.label(RichText::new(format!("{} queued", m.queued)).color(theme::text_dim(ui)).size(12.0));
            if others > 0 && ui.link(RichText::new(format!("{others} other clip{} running · Queue", if others == 1 { "" } else { "s" })).size(12.0)).clicked() {
                act.open_queue = true;
            }
        });
    });
    act
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, l: Level, preview: bool) -> Step {
        let mut x = Step::new(id, id, l);
        x.preview = preview;
        x
    }

    #[test]
    fn error_survives_a_later_successful_preview_and_dies_with_the_next_action() {
        let m = build(vec![s("key", Level::Error, false), s("prev", Level::Done, true)]);
        assert_eq!(m.head.as_ref().unwrap().id, "key");
        let m = build(vec![s("key", Level::Error, false), s("prev", Level::Done, true), s("hint", Level::Queued, false)]);
        assert!(m.error.is_none());
        assert_eq!(m.queued, 1);
    }

    #[test]
    fn same_id_is_listed_once_and_active_comes_first() {
        let m = build(vec![s("a", Level::Queued, false), s("b", Level::Active, false), s("a", Level::Active, false)]);
        assert_eq!(m.list.len(), 2);
        assert_eq!(m.head.as_ref().unwrap().id, "b");
    }
}
