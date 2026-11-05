//! Segmented control (bible §5.3, §8.3 "File type" fit, §10.2, §12): a 22 px track with 2–5 short options painted by
//! hand. One Tab stop (only the selected segment is focusable), ←/→ select, AccessKit RadioGroup of RadioButtons.
use super::nav::{child_id, child_sense, composite_nav};
use super::{keyboard_focus, line, paint_line, Vis, LABEL_DX};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{self, accesskit::{Role, Toggled}, Align, CornerRadius, Pos2, Rect, Response, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo};

/// How the segments share the track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegWidth {
    /// Each segment is its label (+ 14 px icon and 4 gap) plus 9 px each side; the track is as wide as the sum + 4.
    Natural,
    /// Equal segments filling the track width (Deliver Range row).
    Equal,
    /// Label width + 4 px each side, the spare track width shared out equally (File type row, §8.3).
    FitVariable,
}

/// One option: the value it sets, its short label (≤ 10 characters), an optional 14 px icon, and a reason when disabled.
pub struct Seg<'a, T> {
    pub value: T,
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub disabled: Option<&'a str>,
}

impl<'a, T> Seg<'a, T> {
    pub fn new(value: T, label: &'a str) -> Self { Seg { value, label, icon: None, disabled: None } }
    pub fn icon(mut self, icon: Icon) -> Self { self.icon = Some(icon); self }
    /// Disabled option; the reason is its tooltip and AccessKit description.
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
}

pub struct Segmented<'a, T> {
    value: &'a mut T,
    options: &'a [Seg<'a, T>],
    width: SegWidth,
    track_width: Option<f32>,
    name: Option<&'a str>,
    disabled: Option<&'a str>,
    at: Option<Pos2>,
    force: Option<(Vec<Vis>, Option<usize>)>,
}

/// `segmented(ui, &mut value, &[options], width)`: Natural width, flow layout; returns the track's response
/// (`changed()` when the value changed).
pub fn segmented<T: PartialEq + Clone>(ui: &mut Ui, value: &mut T, options: &[Seg<'_, T>], width: SegWidth) -> Response {
    Segmented::new(value, options).width(width).show(ui)
}

impl<'a, T: PartialEq + Clone> Segmented<'a, T> {
    pub fn new(value: &'a mut T, options: &'a [Seg<'a, T>]) -> Self {
        Segmented { value, options, width: SegWidth::Natural, track_width: None, name: None, disabled: None, at: None, force: None }
    }
    pub fn width(mut self, w: SegWidth) -> Self { self.width = w; self }
    /// Track width for `Equal` / `FitVariable` (default: the available width).
    pub fn track_width(mut self, w: f32) -> Self { self.track_width = Some(w); self }
    /// The group's accessible name (the visible group label, "Overlay").
    pub fn name(mut self, name: &'a str) -> Self { self.name = Some(name); self }
    /// Whole control disabled, with the reason as tooltip / description.
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    pub fn at(mut self, pos: Pos2) -> Self { self.at = Some(pos); self }
    /// Specimen / test only: pointer look per segment, and the segment drawn with the keyboard focus ring.
    pub fn forced(mut self, vis: Vec<Vis>, focus: Option<usize>) -> Self { self.force = Some((vis, focus)); self }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens(ui);
        let n = self.options.len();
        let whole_enabled = ui.is_enabled() && self.disabled.is_none();
        let painter = ui.painter().clone();
        let lead = |i: usize| if self.options[i].icon.is_some() { 14.0 + 4.0 } else { 0.0 };
        // + 1/64 px: Chrome rounds text widths up to that, egui to 1/16 (the sheet's fractional layout then matches).
        let natural: Vec<f32> = (0..n).map(|i| line(&painter, self.options[i].label, ty::button(), 16.0, f32::INFINITY).size().x + 1.0 / 64.0 + lead(i)).collect();
        let avail = self.track_width.unwrap_or_else(|| ui.available_width());
        let widths: Vec<f32> = match self.width {
            SegWidth::Natural => natural.iter().map(|w| w + 18.0).collect(),
            SegWidth::Equal => vec![(avail - 4.0) / n as f32; n],
            SegWidth::FitVariable => {
                let base: Vec<f32> = natural.iter().map(|w| w + 8.0).collect();
                let spare = ((avail - 4.0) - base.iter().sum::<f32>()).max(0.0) / n as f32;
                base.iter().map(|w| w + spare).collect()
            }
        };
        let total = if self.width == SegWidth::Natural { widths.iter().sum::<f32>() + 4.0 } else { avail };
        let track = match self.at {
            Some(p) => Rect::from_min_size(p, egui::vec2(total, 22.0)),
            None => ui.allocate_exact_size(egui::vec2(total, 22.0), egui::Sense::hover()).0,
        };

        let id = ui.scope_id().with(("segmented", self.name, self.options.iter().map(|o| o.label).collect::<Vec<_>>(), self.at.map(|p| (p.x as i32, p.y as i32))));
        let enabled: Vec<bool> = self.options.iter().map(|o| whole_enabled && o.disabled.is_none()).collect();
        let mut sel = self.options.iter().position(|o| o.value == *self.value);
        let mut changed = false;
        if self.force.is_none() {
            let mut idx = sel.unwrap_or(0);
            if composite_nav(ui, id, &enabled, &mut idx) { sel = Some(idx); *self.value = self.options[idx].value.clone(); changed = true; }
        }

        let mut group = ui.interact(track, id, egui::Sense::hover());
        group.widget_info(|| WidgetInfo::labeled(Role::RadioGroup, whole_enabled, self.name.unwrap_or("")));
        if let Some(reason) = self.disabled { ui.ctx().accesskit_node_builder(id, |n| n.set_description(reason)); }

        let snap = |r: Rect| Rect::from_min_max(egui::pos2(r.min.x.round(), r.min.y.round()), egui::pos2(r.max.x.round(), r.max.y.round()));
        let track_s = snap(track);
        let (y0, y1) = (track_s.min.y + 2.0, track_s.max.y - 2.0);
        let radius = CornerRadius::same(2);
        let mut clicked = None;
        let mut x = track.min.x + 2.0;
        let mut rects = Vec::with_capacity(n);
        for w in &widths { rects.push(snap(Rect::from_min_max(egui::pos2(x, y0), egui::pos2(x + w, y1)))); x += w; }

        let mut painted: Vec<(Rect, bool, Vis, bool)> = Vec::with_capacity(n);
        ui.scope_builder(UiBuilder::new().max_rect(track).accessibility_parent(id), |ui| {
            for (i, o) in self.options.iter().enumerate() {
                let resp = ui.interact(rects[i], child_id(id, i), child_sense(&enabled, sel.unwrap_or(0), i));
                if enabled[i] && resp.clicked() { clicked = Some(i); }
                let (vis, focus) = match &self.force {
                    Some((v, f)) => (v.get(i).copied().unwrap_or(Vis::Rest), *f == Some(i)),
                    None => (
                        if !enabled[i] { Vis::Rest } else if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest },
                        enabled[i] && resp.has_focus() && keyboard_focus(ui.ctx()),
                    ),
                };
                painted.push((rects[i], enabled[i], vis, focus));
                let is_sel = sel == Some(i);
                resp.widget_info(|| WidgetInfo::labeled(Role::RadioButton, enabled[i], o.label));
                ui.ctx().accesskit_node_builder(resp.id, |n| {
                    n.set_toggled(if is_sel { Toggled::True } else { Toggled::False });
                    if let Some(r) = o.disabled { n.set_description(r); }
                });
                if let Some(r) = o.disabled { let _ = resp.on_hover_text(r).on_disabled_hover_text(r); }
            }
        });
        if let Some(i) = clicked {
            if sel != Some(i) { *self.value = self.options[i].value.clone(); sel = Some(i); changed = true; }
        }

        if ui.is_rect_visible(track_s) {
            let p = ui.painter();
            p.rect_filled(track_s, CornerRadius::same(3), t.bg_sunken);
            p.rect_stroke(track_s, CornerRadius::same(3), Stroke::new(1.0, if whole_enabled { t.border_control } else { t.border_divider }), StrokeKind::Inside);
            for (i, o) in self.options.iter().enumerate() {
                let (r, en, vis, focus) = painted[i];
                let is_sel = sel == Some(i);
                // §5.3: selected = bg.selected + 1 px border.control; hover = bg.hover + border.hover; pressed = bg.pressed + text.secondary.
                let (fill, border) = if is_sel {
                    if whole_enabled { (Some(t.bg_selected), Some(t.border_control)) } else { (Some(t.bg_raised), Some(t.border_divider)) }
                } else {
                    match (en, vis) {
                        (true, Vis::Hover) => (Some(t.bg_hover), Some(t.border_hover)),
                        (true, Vis::Pressed) => (Some(t.bg_pressed), Some(t.text_secondary)),
                        _ => (None, None),
                    }
                };
                if let Some(f) = fill { p.rect_filled(r, radius, f); }
                if focus {
                    p.rect_stroke(r, radius, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
                } else if let Some(b) = border {
                    p.rect_stroke(r, radius, Stroke::new(1.0, b), StrokeKind::Inside);
                }
                // 1 px divider between segments that are neither selected nor next to the selected one.
                if i > 0 && !is_sel && sel != Some(i - 1) {
                    p.rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.min.y + 4.0), egui::pos2(r.min.x + 1.0, r.max.y - 4.0)), 0.0, t.border_divider);
                }
                let color = if !en { t.text_disabled } else if is_sel || vis != Vis::Rest { t.text_primary } else { t.text_secondary };
                let gw = widths[i] - if self.width == SegWidth::Natural { 18.0 } else { 8.0 } - lead(i);
                let g = line(p, o.label, ty::button(), 16.0, gw.max(1.0));
                let content = g.size().x + lead(i);
                // Centred in the segment's unsnapped box (the text keeps the sheet's fractional x; paint_line rounds it).
                let left = track.min.x + 2.0 + widths[..i].iter().sum::<f32>();
                let mut tx = left + widths[i] / 2.0 - content / 2.0;
                if let Some(ic) = o.icon {
                    let ir = Rect::from_min_size(egui::pos2(tx.round(), (r.center().y - 7.0).round()), egui::vec2(14.0, 14.0));
                    icons::paint(p, ir, ic, IconSize::S14, color);
                    tx += 18.0;
                }
                paint_line(p, Rect::from_min_max(egui::pos2(tx + LABEL_DX, r.min.y), egui::pos2(r.max.x, r.max.y)), g, 16.0, Align::Min, color, 0.0);
            }
        }
        if let Some(reason) = self.disabled { group = group.on_hover_text(reason).on_disabled_hover_text(reason); }
        if changed { group.mark_changed(); }
        group
    }
}
