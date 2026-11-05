//! Single-line field (§5.6). Call validation on a commit, then pass its result with `.error(..)`.
use crate::ui::{icons::{self, Icon, IconSize}, tokens::tokens, type_scale as ty};
use super::{keyboard_focus, line, paint_line};
use eframe::egui::{self, Id, Key, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State { Rest, Hover, Editing, Error, Disabled, Focus }

#[derive(Clone, Default)]
struct EditState { original: String, error: Option<String>, editing: bool }

/// Default field. For options use `TextField::new(id, value, width).… .show(ui)`.
pub fn text_field(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, value: &mut String, width: f32) -> Response {
    TextField::new(id, value, width).show(ui)
}

pub struct TextField<'a> {
    id: Id, value: &'a mut String, width: f32, placeholder: &'a str,
    disabled: Option<&'a str>, error: Option<&'a str>, commit: bool, mono: bool,
    validator: Option<&'a mut dyn FnMut(&str) -> Option<String>>,
    forced: Option<State>,
}
impl<'a> TextField<'a> {
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, value: &'a mut String, width: f32) -> Self {
        Self { id: Id::unique(id), value, width, placeholder: "", disabled: None,
            error: None, commit: false, mono: false, forced: None, validator: None }
    }
    pub fn placeholder(mut self, text: &'a str) -> Self { self.placeholder = text; self }
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    /// Validation result, sampled only on Enter or `.commit(true)`; retained until the next commit.
    /// Callers must run their validator only on these actions, never on draft changes.
    pub fn error(mut self, message: Option<&'a str>) -> Self { self.error = message; self }
    /// Apply / Save / Test (or another consuming action) was activated this frame.
    pub fn commit(mut self, commit: bool) -> Self { self.commit = commit; self }
    /// Runs only on a nonempty committed value, including Enter after TextEdit processes input.
    pub fn validate(mut self, validator: &'a mut dyn FnMut(&str) -> Option<String>) -> Self { self.validator = Some(validator); self }
    pub fn mono(mut self) -> Self { self.mono = true; self }
    pub fn forced(mut self, state: State) -> Self { self.forced = Some(state); self }
    pub fn show(self, ui: &mut Ui) -> Response { ui.add(self) }
}

impl Widget for TextField<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let t = tokens(ui);
        let enabled = ui.is_enabled() && self.disabled.is_none() && self.forced != Some(State::Disabled);
        let (rect, _) = ui.allocate_exact_size(egui::vec2(self.width.max(120.0), 22.0), Sense::hover());
        let mut state = ui.ctx().data_mut(|d| d.get_temp::<EditState>(self.id).unwrap_or_default());
        let focused = ui.memory(|m| m.has_focus(self.id));
        if !state.editing { state.original = self.value.clone(); }
        // egui clears focus for Escape before widgets run; retain the previous frame's edit session.
        let escape = enabled && (focused || state.editing) && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        if escape { *self.value = state.original.clone(); ui.memory_mut(|m| m.surrender_focus(self.id)); }
        let enter = enabled && focused && ui.input(|i| i.key_pressed(Key::Enter));
        let bg = ui.painter().add(egui::Shape::Noop);
        let font = if self.mono { ty::mono() } else { ty::value() };
        let color = if enabled { t.text_primary } else { t.text_disabled };
        let mut layouter = |ui: &Ui, text: &dyn egui::TextBuffer, _: f32| {
            line(ui.painter(), text.as_str(), font.clone(), 16.0, f32::INFINITY)
        };
        let mut response = ui.scope(|ui| {
            ui.visuals_mut().selection.bg_fill = t.text_selection;
            ui.visuals_mut().weak_text_color = Some(if enabled { t.text_tertiary } else { t.text_disabled });
            ui.visuals_mut().text_cursor.blink = false;
            ui.visuals_mut().text_cursor.preview = false;
            ui.add_enabled_ui(enabled, |ui| {
                ui.put(rect, egui::TextEdit::singleline(self.value).id(self.id)
                    .frame(egui::Frame::NONE.inner_margin(egui::Margin { left: 7, right: 7, top: 3, bottom: 3 }))
                    .font(font.clone()).text_color(color)
                    .hint_text(egui::RichText::new(self.placeholder).font(font.clone()))
                    .layouter(&mut layouter).align(egui::Align2::LEFT_CENTER))
            }).inner
        }).inner;
        if escape { response.mark_changed(); }
        if enabled && !escape && (enter || self.commit) {
            state.original = self.value.clone();
            state.error = if self.value.trim().is_empty() { None }
                else if let Some(validate) = self.validator { validate(self.value) }
                else { self.error.map(str::to_owned) };
        }
        state.editing = response.has_focus();
        let visual = self.forced.unwrap_or_else(|| {
            if !enabled { State::Disabled } else if state.error.is_some() { State::Error }
            else if response.has_focus() { State::Editing } else if response.hovered() { State::Hover } else { State::Rest }
        });
        let ring = enabled && (self.forced == Some(State::Focus) || self.forced == Some(State::Editing)
            || (self.forced.is_none() && response.has_focus() && keyboard_focus(ui.ctx())));
        let border = match visual {
            State::Disabled => t.border_divider, State::Error => t.error,
            State::Hover => t.border_hover, State::Editing | State::Focus => t.accent_bright,
            State::Rest => t.border_control,
        };
        ui.painter().set(bg, egui::Shape::rect_filled(rect, 3, if enabled { t.bg_sunken } else { t.bg_panel }));
        ui.painter().rect_stroke(rect, 3, Stroke::new(if ring { 2.0 } else { 1.0 }, if ring { t.focus_ring } else { border }), StrokeKind::Inside);
        if self.forced == Some(State::Editing) && !response.has_focus() {
            let g = line(ui.painter(), self.value, font.clone(), 16.0, f32::INFINITY);
            let x = (rect.left() + 7.0 + g.size().x).min(rect.right() - 7.0);
            let caret = line(ui.painter(), "|", font, 16.0, f32::INFINITY);
            paint_line(ui.painter(), Rect::from_min_max(egui::pos2(x,rect.top()),rect.max), caret,16.0,egui::Align::Min,t.text_primary,0.0);
        }
        let message = if self.forced == Some(State::Error) { self.error } else { state.error.as_deref().filter(|_| enabled) };
        if let Some(message) = message { message_row(ui, rect.width(), message); }
        response.widget_info(|| egui::WidgetInfo::text_edit(enabled, self.value.as_str(), self.value.as_str(), self.placeholder));
        if let Some(reason) = self.disabled.or(message) {
            ui.ctx().accesskit_node_builder(response.id, |n| n.set_description(reason));
        }
        ui.ctx().data_mut(|d| d.insert_temp(self.id, state));
        if let Some(reason) = self.disabled { response.on_disabled_hover_text(reason) } else { response }
    }
}

fn message_row(ui: &mut Ui, width: f32, message: &str) {
    let width = width.max(160.0);
    let t = tokens(ui);
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = width - 18.0;
    job.append(message, 0.0, ty::format(ty::small(), t.error, 14.0));
    let galley = ui.painter().layout_job(job);
    let height = if galley.rows.len() > 1 { 32.0 } else { 18.0 };
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), Sense::hover());
    icons::paint(ui.painter(), Rect::from_min_size(rect.min + egui::vec2(0.0, 2.0), egui::vec2(14.0,14.0)), Icon::Error, IconSize::S14, t.error);
    ui.painter().galley(rect.min + egui::vec2(18.0,2.0), galley, t.error);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(ctx: &egui::Context, value: &mut String, events: Vec<egui::Event>) -> usize {
        let mut calls = 0;
        let mut validator = |_: &str| { calls += 1; Some("Invalid name".to_owned()) };
        let mut output = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| {
            TextField::new("field", value, 200.0).validate(&mut validator).show(ui);
        });
        output.textures_delta.clear();
        calls
    }
    fn key(key: Key) -> egui::Event { egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE } }
    #[test]
    fn textfield_error_only_after_commit() {
        let ctx = egui::Context::default(); let mut value = "valid".to_owned();
        frame(&ctx, &mut value, vec![]);
        ctx.memory_mut(|m| m.request_focus(Id::unique("field")));
        assert_eq!(frame(&ctx, &mut value, vec![egui::Event::Text("!".into())]), 0);
        assert!(value.contains("!"));
        assert!(ctx.data(|d| d.get_temp::<EditState>(Id::unique("field")).unwrap().error.is_none()));
        assert_eq!(frame(&ctx, &mut value, vec![key(Key::Enter)]), 1);
        assert_eq!(ctx.data(|d| d.get_temp::<EditState>(Id::unique("field")).unwrap().error), Some("Invalid name".into()));
        ctx.memory_mut(|m| m.request_focus(Id::unique("field")));
        assert_eq!(frame(&ctx, &mut value, vec![egui::Event::Text("draft".into())]), 0);
        assert!(ctx.data(|d| d.get_temp::<EditState>(Id::unique("field")).unwrap().error.is_some()));
        value.clear();
        assert_eq!(frame(&ctx, &mut value, vec![key(Key::Enter)]), 0);
        assert!(ctx.data(|d| d.get_temp::<EditState>(Id::unique("field")).unwrap().error.is_none()));
    }
    #[test]
    fn textfield_esc_reverts() {
        let ctx = egui::Context::default(); let mut value = "original".to_owned();
        frame(&ctx, &mut value, vec![]);
        ctx.memory_mut(|m| m.request_focus(Id::unique("field")));
        frame(&ctx, &mut value, vec![egui::Event::Text("draft".into())]);
        assert_ne!(value, "original");
        frame(&ctx, &mut value, vec![key(Key::Escape)]);
        assert_eq!(value, "original");
        assert!(!ctx.memory(|m| m.has_focus(Id::unique("field"))));
    }
}
