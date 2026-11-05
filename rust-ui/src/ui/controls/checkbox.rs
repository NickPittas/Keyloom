//! Checkbox, mixed checkbox and radio (§5.8). Custom paint: stock geometry/check paths differ from the bible.
use super::{keyboard_focus, line, paint_line, Vis};
use crate::ui::{icons::{self, Icon}, tokens::{tokens, Tokens}, type_scale as ty};
use eframe::egui::{self, accesskit::{Role, Toggled}, Align, Color32, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, Widget, WidgetInfo};

enum Value<'a> { Bool(&'a mut bool), Tri(&'a mut Option<bool>), Radio(bool) }
pub struct Check<'a> {
    value: Value<'a>,
    label: Option<&'a str>,
    disabled: Option<&'a str>,
    force: Option<(Vis, bool)>,
    at: Option<Pos2>,
}
impl<'a> Check<'a> {
    pub fn new(value: &'a mut bool, label: Option<&'a str>) -> Self { Self::make(Value::Bool(value), label) }
    pub fn mixed(value: &'a mut Option<bool>, label: Option<&'a str>) -> Self { Self::make(Value::Tri(value), label) }
    pub fn radio(selected: bool, label: Option<&'a str>) -> Self { Self::make(Value::Radio(selected), label) }
    fn make(value: Value<'a>, label: Option<&'a str>) -> Self { Self { value, label, disabled: None, force: None, at: None } }
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    /// Specimen states use the same paint path as live interaction.
    pub fn force(mut self, vis: Vis, focus: bool) -> Self { self.force = Some((vis, focus)); self }
    pub fn at(mut self, pos: Pos2) -> Self { self.at = Some(pos); self }
    fn state(&self) -> Option<bool> { match &self.value { Value::Bool(v) => Some(**v), Value::Tri(v) => **v, Value::Radio(v) => Some(*v) } }
}
#[derive(Clone, Copy)]
pub struct Look { pub fill: Color32, pub border: Color32, pub mark: Color32, pub text: Color32 }
pub fn look(t: &Tokens, radio: bool, state: Option<bool>, vis: Vis, enabled: bool) -> Look {
    if !enabled { return Look { fill: t.bg_panel, border: t.border_divider, mark: t.text_disabled, text: t.text_disabled }; }
    let on = state != Some(false);
    let accent = if vis == Vis::Hover { t.accent_hover } else { t.accent };
    Look {
        fill: if on && !radio { accent } else { t.bg_sunken },
        border: if on { accent } else if vis != Vis::Rest { t.border_hover } else { t.border_control },
        mark: if radio { accent } else { t.text_on_accent }, text: t.text_primary,
    }
}
impl Widget for Check<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        if self.disabled.is_some() && ui.is_enabled() { return ui.add_enabled(false, self); }
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let radio = matches!(self.value, Value::Radio(_));
        let t = tokens(ui);
        let galley = self.label.map(|s| line(ui.painter(), s, ty::label(), 16.0, f32::INFINITY));
        let size = egui::vec2(14.0 + galley.as_ref().map_or(0.0, |g| 6.0 + g.size().x), 22.0);
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let mut response = if let Some(pos) = self.at {
            ui.interact(Rect::from_min_size(pos, size), ui.scope_id().with(("check", pos.x.to_bits(), pos.y.to_bits())), sense)
        } else { ui.allocate_exact_size(size, sense).1 };
        if enabled && response.clicked() {
            match &mut self.value {
                Value::Bool(v) => { **v = !**v; response.mark_changed(); }
                Value::Tri(v) => { **v = Some(**v != Some(true)); response.mark_changed(); }
                Value::Radio(_) => {}
            }
        }
        let state = self.state();
        let keyboard = keyboard_focus(ui.ctx());
        let (vis, focus) = self.force.unwrap_or((
            if response.is_pointer_button_down_on() { Vis::Pressed } else if response.hovered() { Vis::Hover } else { Vis::Rest },
            response.has_focus() && keyboard,
        ));
        let l = look(t, radio, state, vis, enabled);
        let rect = response.rect;
        let box_rect = Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - 7.0), egui::vec2(14.0, 14.0));
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let stroke = Stroke::new(if focus && enabled { 2.0 } else { 1.0 }, if focus && enabled { t.focus_ring } else { l.border });
            if radio {
                p.rect(box_rect, 7, l.fill, stroke, StrokeKind::Inside);
                if state == Some(true) { p.rect_filled(Rect::from_center_size(box_rect.center(), egui::vec2(6.0, 6.0)), 3, l.mark); }
            } else {
                p.rect_filled(box_rect, 2, l.fill);
                p.rect_stroke(box_rect, 2, stroke, StrokeKind::Inside);
                match state {
                    Some(true) => {
                        // §5.8 overrides the general icon stroke: reuse the §9 drawing at 1.5 px.
                        for prim in icons::drawing(Icon::Check, 12).unwrap() {
                            if let icons::Shape::Line(points) = prim.s {
                                let points = points.iter().map(|&(x, y, _)| box_rect.min + egui::vec2(1.0 + x, 1.0 + y)).collect();
                                p.add(egui::Shape::line(points, Stroke::new(1.5, l.mark)));
                            }
                        }
                    }
                    // Whole-pixel dash at box top + 6 (dialog mockup: one solid white row); a 1.5 px bar blends to grey.
                    None => { p.rect_filled(Rect::from_min_size(egui::pos2(box_rect.center().x - 4.0, (box_rect.top() + 6.0).round()), egui::vec2(8.0, 1.0)), 0, l.mark); }
                    _ => {}
                }
            }
            if let Some(g) = galley {
                let label_rect = Rect::from_min_max(egui::pos2(box_rect.right() + 6.0, rect.top()), rect.max);
                paint_line(p, label_rect, g, 16.0, Align::Min, l.text, 0.0);
            }
        }
        response.widget_info(|| WidgetInfo::labeled(if radio { Role::RadioButton } else { Role::CheckBox }, enabled, self.label.unwrap_or("")));
        ui.ctx().accesskit_node_builder(response.id, |n| {
            n.set_toggled(match state { Some(true) => Toggled::True, Some(false) => Toggled::False, None => Toggled::Mixed });
            if !enabled { n.set_disabled(); }
            if let Some(reason) = self.disabled { n.set_description(reason); }
        });
        if let Some(reason) = self.disabled { response.on_hover_text(reason).on_disabled_hover_text(reason) } else { response }
    }
}
pub fn checkbox(ui: &mut Ui, value: &mut bool, label: Option<&str>) -> Response { ui.add(Check::new(value, label)) }
pub fn tri_checkbox(ui: &mut Ui, value: &mut Option<bool>, label: Option<&str>) -> Response { ui.add(Check::mixed(value, label)) }
pub fn radio(ui: &mut Ui, selected: bool, label: Option<&str>) -> Response { ui.add(Check::radio(selected, label)) }

#[cfg(test)]
mod tests {
    use super::*;
    fn click_event(pos: Pos2, pressed: bool) -> egui::Event { egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() } }
    #[test]
    fn checkbox_toggles_on_click_and_space() {
        let ctx = egui::Context::default();
        let mut value = false;
        let mut response = None;
        let mut draw = |events| { let mut out = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| { response = Some(checkbox(ui, &mut value, Some("Fill holes"))); }); out.textures_delta.clear(); response.clone().unwrap() };
        let r = draw(vec![]);
        let pos = r.rect.right_center() - egui::vec2(2.0, 0.0);
        draw(vec![egui::Event::PointerMoved(pos), click_event(pos, true)]);
        let r = draw(vec![click_event(pos, false)]);
        assert!(r.changed());
        ctx.memory_mut(|m| m.request_focus(r.id));
        let r = draw(vec![egui::Event::Key { key: egui::Key::Space, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }]);
        assert!(r.changed());
        assert!(!value);
    }
    #[test]
    fn tri_checkbox_mixed_click_sets_on() {
        let ctx = egui::Context::default();
        let mut value = None;
        let mut response = None;
        let mut draw = |events| { let mut out = ctx.run_ui(egui::RawInput { events, ..Default::default() }, |ui| { response = Some(tri_checkbox(ui, &mut value, Some("Fill holes"))); }); out.textures_delta.clear(); response.clone().unwrap() };
        let pos = draw(vec![]).rect.center();
        draw(vec![egui::Event::PointerMoved(pos), click_event(pos, true)]);
        assert!(draw(vec![click_event(pos, false)]).changed());
        assert_eq!(value, Some(true));
    }
    #[test]
    fn radio_accesskit_role() {
        let ctx = egui::Context::default(); ctx.enable_accesskit();
        let mut out = ctx.run_ui(Default::default(), |ui| { radio(ui, true, Some("Linear")); let mut v = None; tri_checkbox(ui, &mut v, Some("Mixed")); ui.add(Check::radio(false, Some("Disabled")).disabled("Unavailable")); });
        out.textures_delta.clear();
        let tree = out.platform_output.accesskit_update.unwrap();
        let n = tree.nodes.iter().map(|(_, n)| n).find(|n| n.role() == Role::RadioButton && n.label() == Some("Linear")).unwrap();
        assert_eq!(n.toggled(), Some(Toggled::True));
        let n = tree.nodes.iter().map(|(_, n)| n).find(|n| n.role() == Role::CheckBox).unwrap();
        assert_eq!(n.toggled(), Some(Toggled::Mixed));
        let n = tree.nodes.iter().map(|(_, n)| n).find(|n| n.description() == Some("Unavailable")).unwrap();
        assert!(n.is_disabled());
    }
}
