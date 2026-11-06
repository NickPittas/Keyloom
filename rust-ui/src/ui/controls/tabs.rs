//! The three tab kinds of bible §5.4 (page, stage, panel), one look family, painted by hand. Each strip is one Tab
//! stop (only the active tab is focusable), ←/→ switch tab at once, AccessKit TabList of Tabs (§10.2, §12).
use super::nav::{child_id, child_sense, composite_nav};
use super::{keyboard_focus, line, paint_line, Vis, LABEL_DX};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{self, accesskit::Role, Align, Color32, CornerRadius, Pos2, Rect, Response, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo};

/// Status dot after a stage tab's label (never text): an out-of-date output, or a blocked stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StageStatus { OutOfDate, Blocked }

/// One tab. Page and panel tabs may carry a 16 px icon; stage tabs are numbered by position; panel tabs a count badge.
pub struct Tab<'a> {
    pub label: &'a str,
    pub icon: Option<Icon>,
    /// Disabled with the reason as tooltip (page tab "Shot": "Open a shot from the Board").
    pub disabled: Option<&'a str>,
    pub status: Option<StageStatus>,
    pub badge: Option<u32>,
    /// Hover tooltip of an enabled tab (name, then its shortcut).
    pub tip: Option<&'a str>,
}

impl<'a> Tab<'a> {
    pub fn new(label: &'a str) -> Self { Tab { label, icon: None, disabled: None, status: None, badge: None, tip: None } }
    pub fn icon(mut self, icon: Icon) -> Self { self.icon = Some(icon); self }
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    pub fn status(mut self, s: StageStatus) -> Self { self.status = Some(s); self }
    pub fn badge(mut self, n: u32) -> Self { self.badge = Some(n); self }
    pub fn tip(mut self, t: &'a str) -> Self { self.tip = Some(t); self }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TabKind { Page, Stage, Panel }

pub struct TabStrip<'a> {
    kind: TabKind,
    active: &'a mut usize,
    tabs: &'a [Tab<'a>],
    compact: bool,
    name: &'a str,
    at: Option<Pos2>,
    force: Option<(Vec<Vis>, Option<usize>)>,
    whole: bool,
    rects_out: Option<&'a mut Vec<Rect>>,
}

/// Toolbar page tabs (28 high): Board · Shot · Machines · Models · Settings.
pub fn page_tabs(ui: &mut Ui, active: &mut usize, tabs: &[Tab<'_>]) -> Response {
    TabStrip::new(TabKind::Page, active, tabs).show(ui)
}
/// Shot-header stage tabs (28 high). `compact`: numbered circle only, the active tab keeps its label (§7.6).
pub fn stage_tabs(ui: &mut Ui, active: &mut usize, tabs: &[Tab<'_>], compact: bool) -> Response {
    TabStrip::new(TabKind::Stage, active, tabs).compact(compact).show(ui)
}
/// Dock / queue / activity panel tabs (24 high, browser-like).
pub fn panel_tabs(ui: &mut Ui, active: &mut usize, tabs: &[Tab<'_>]) -> Response {
    TabStrip::new(TabKind::Panel, active, tabs).show(ui)
}

impl<'a> TabStrip<'a> {
    pub fn new(kind: TabKind, active: &'a mut usize, tabs: &'a [Tab<'a>]) -> Self {
        TabStrip { kind, active, tabs, compact: false, name: "", at: None, force: None, whole: false, rects_out: None }
    }
    pub fn compact(mut self, on: bool) -> Self { self.compact = on; self }
    /// The strip's accessible name ("Pages", "Stages").
    pub fn name(mut self, name: &'a str) -> Self { self.name = name; self }
    pub fn at(mut self, pos: Pos2) -> Self { self.at = Some(pos); self }
    /// Specimen / test only: pointer look per tab and the tab drawn with the keyboard focus ring.
    pub fn forced(mut self, vis: Vec<Vis>, focus: Option<usize>) -> Self { self.force = Some((vis, focus)); self }
    /// Each tab width rounded up to a whole pixel (app toolbar, Start mockups fix 4); the sheet keeps fractional widths.
    pub fn whole_px(mut self, on: bool) -> Self { self.whole = on; self }
    /// Receives each tab's rect after layout (tests, anchors).
    pub fn rects_out(mut self, out: &'a mut Vec<Rect>) -> Self { self.rects_out = Some(out); self }

    fn height(&self) -> f32 { if self.kind == TabKind::Panel { 24.0 } else { 28.0 } }
    fn gap(&self) -> f32 { if self.kind == TabKind::Panel { 0.0 } else { 2.0 } }

    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens(ui);
        let n = self.tabs.len();
        let painter = ui.painter().clone();
        let active_now = *self.active;
        let badge_w = |tab: &Tab| tab.badge.map(|b| (line(&painter, &b.to_string(), ty::chip(), 14.0, f32::INFINITY).size().x + 10.0).max(16.0));
        // Width of each tab (unsnapped), per kind.
        let widths: Vec<f32> = self.tabs.iter().enumerate().map(|(i, tab)| {
            // Chrome rounds each text width up to 1/64 px and egui to 1/16, so the sheet's strips drift by ~0.016 px per label.
            let tw = line(&painter, tab.label, ty::button(), 16.0, f32::INFINITY).size().x + 1.0 / 64.0;
            match self.kind {
                // Compact page tabs (§7.5): icon only, 28 × 28; the active tab keeps its label.
                TabKind::Page if self.compact && tab.icon.is_some() && i != active_now => 28.0 + badge_w(tab).map_or(0.0, |b| 6.0 + b),
                TabKind::Page => 12.0 + if tab.icon.is_some() { 22.0 } else { 0.0 } + tw + tab.status.map_or(0.0, |_| 15.0) + badge_w(tab).map_or(0.0, |b| 6.0 + b) + 12.0,
                TabKind::Stage => {
                    // Shot mockup: 12 px pads, or 6 px at Compact, where inactive tabs are circle-only.
                    let pad = if self.compact { 6.0 } else { 12.0 };
                    let label = if self.compact && i != active_now { 0.0 } else { 6.0 + tw };
                    pad + 16.0 + label + if tab.status.is_some() { 12.0 } else { 0.0 } + pad
                }
                TabKind::Panel => 12.0 + if tab.icon.is_some() { 22.0 } else { 0.0 } + tw + badge_w(tab).map_or(0.0, |b| 6.0 + b) + 12.0 + 1.0,
            }
        }).map(|w| if self.whole { w.ceil() } else { w }).collect();
        let total = widths.iter().sum::<f32>() + self.gap() * n.saturating_sub(1) as f32;
        let h = self.height();
        let strip = match self.at {
            Some(p) => Rect::from_min_size(p, egui::vec2(total, h)),
            None => ui.allocate_exact_size(egui::vec2(total, h), egui::Sense::hover()).0,
        };

        let id = ui.scope_id().with(("tabs", self.kind as u8, self.name, self.tabs.iter().map(|x| x.label).collect::<Vec<_>>(), self.at.map(|p| (p.x as i32, p.y as i32))));
        let enabled: Vec<bool> = self.tabs.iter().map(|x| ui.is_enabled() && x.disabled.is_none()).collect();
        let mut changed = false;
        if self.force.is_none() && composite_nav(ui, id, &enabled, self.active) { changed = true; }
        let current = *self.active;

        let mut group = ui.interact(strip, id, egui::Sense::hover());
        group.widget_info(|| WidgetInfo::labeled(Role::TabList, ui.is_enabled(), self.name));

        let snap = |v: f32| v.round();
        let mut rects = Vec::with_capacity(n);
        let mut x = strip.min.x;
        for (i, w) in widths.iter().enumerate() {
            let hh = if self.kind == TabKind::Panel && i == current { h + 1.0 } else { h };
            rects.push(Rect::from_min_max(egui::pos2(snap(x), snap(strip.min.y)), egui::pos2(snap(x + w), snap(strip.min.y) + hh)));
            x += w + self.gap();
        }

        let mut states = Vec::with_capacity(n);
        let mut clicked = None;
        ui.scope_builder(UiBuilder::new().max_rect(strip).accessibility_parent(id), |ui| {
            for (i, tab) in self.tabs.iter().enumerate() {
                let mut resp = ui.interact(rects[i], child_id(id, i), child_sense(&enabled, current, i));
                if enabled[i] { resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand); }
                if enabled[i] && resp.clicked() { clicked = Some(i); }
                let (vis, focus) = match &self.force {
                    Some((v, f)) => (v.get(i).copied().unwrap_or(Vis::Rest), *f == Some(i)),
                    None => (
                        if !enabled[i] { Vis::Rest } else if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest },
                        enabled[i] && resp.has_focus() && keyboard_focus(ui.ctx()),
                    ),
                };
                states.push((vis, focus));
                let name = match tab.status {
                    Some(StageStatus::OutOfDate) => format!("{}, out of date", tab.label.trim()),
                    Some(StageStatus::Blocked) => format!("{}, blocked", tab.label.trim()),
                    None => tab.label.trim().to_owned(),
                };
                let is_sel = i == current;
                resp.widget_info(|| WidgetInfo::labeled(Role::Tab, enabled[i], name.clone()));
                ui.ctx().accesskit_node_builder(resp.id, |nd| {
                    nd.set_selected(is_sel);
                    if let Some(r) = tab.disabled { nd.set_description(r); }
                    else if let Some(t) = tab.tip { nd.set_description(t); }
                });
                if let Some(r) = tab.disabled { let _ = resp.on_hover_text(r).on_disabled_hover_text(r); }
                else if let Some(t) = tab.tip { let _ = resp.on_hover_text(t); }
            }
        });
        if let Some(i) = clicked { if i != current { *self.active = i; changed = true; } }
        let current = *self.active;

        if ui.is_rect_visible(strip) {
            let mut x = strip.min.x;
            for (i, tab) in self.tabs.iter().enumerate() {
                let (vis, focus) = states[i];
                let r = rects[i];
                let pr = TabPaint { t, tab, r, number: i + 1, active: i == current, en: enabled[i], vis, focus, compact: self.compact, unsnapped_x: x, badge_w: badge_w(tab) };
                match self.kind {
                    TabKind::Page => pr.page(ui),
                    TabKind::Stage => pr.stage(ui),
                    TabKind::Panel => pr.panel(ui),
                }
                x += widths[i] + self.gap();
            }
        }
        if changed { group.mark_changed(); }
        if let Some(out) = self.rects_out { *out = rects; }
        group
    }
}

struct TabPaint<'a> {
    t: &'a Tokens,
    tab: &'a Tab<'a>,
    r: Rect,
    number: usize,
    active: bool,
    en: bool,
    vis: Vis,
    focus: bool,
    compact: bool,
    /// The tab's unsnapped left edge (text keeps the fractional x of the sheet; `paint_line` rounds it).
    unsnapped_x: f32,
    badge_w: Option<f32>,
}

impl TabPaint<'_> {
    fn label_color(&self) -> Color32 {
        if !self.en { self.t.text_disabled } else if self.active || self.vis != Vis::Rest { self.t.text_primary } else { self.t.text_secondary }
    }
    fn label(&self, ui: &Ui, x: f32, color: Color32) -> f32 {
        let g = line(ui.painter(), self.tab.label, ty::button(), 16.0, f32::INFINITY);
        let w = g.size().x;
        paint_line(ui.painter(), Rect::from_min_max(egui::pos2(x + LABEL_DX, self.r.min.y), egui::pos2(x + w + 1.0, self.r.max.y)), g, 16.0, Align::Min, color, 0.0);
        w
    }
    fn icon(&self, ui: &Ui, x: f32, color: Color32) {
        if let Some(ic) = self.tab.icon {
            let ir = Rect::from_min_size(egui::pos2(x.round(), (self.r.center().y - 8.0).round()), egui::vec2(16.0, 16.0));
            icons::paint(ui.painter(), ir, ic, IconSize::S16, color);
        }
    }
    fn ring(&self, ui: &Ui, radius: CornerRadius) {
        ui.painter().rect_stroke(self.r, radius, Stroke::new(2.0, self.t.focus_ring), StrokeKind::Inside);
    }

    fn page(&self, ui: &Ui) {
        let (t, r, p) = (self.t, self.r, ui.painter());
        let top = CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 };
        if self.en && self.active { p.rect_filled(r, top, t.bg_panel); } else if self.en && self.vis != Vis::Rest { p.rect_filled(r, top, t.bg_hover); }
        if self.active && self.en { p.rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 2.0), r.max), 0.0, t.accent_bright); }
        let c = self.label_color();
        if self.compact && self.tab.icon.is_some() && !self.active {
            self.icon(ui, r.min.x + 6.0, c);
            if let (Some(b), Some(bw)) = (self.tab.badge, self.badge_w) {
                self.count_badge(p, egui::pos2(r.max.x - bw - 3.0, r.center().y), b, bw);
            }
        } else {
            let mut x = self.unsnapped_x + 12.0;
            self.icon(ui, x, c);
            if self.tab.icon.is_some() { x += 22.0 }
            x += self.label(ui, x, c);
            if let Some(status) = self.tab.status {
                let color = if status == StageStatus::Blocked { t.error } else { t.warning };
                p.circle_filled(egui::pos2((x + 9.0).round(), r.center().y.round()), 3.0, color);
                x += 15.0;
            }
            if let (Some(b), Some(bw)) = (self.tab.badge, self.badge_w) {
                self.count_badge(p, egui::pos2((x + 6.0 + bw / 2.0).round(), r.center().y.round()), b, bw);
            }
        }
        if self.focus { self.ring(ui, top) }
    }

    fn stage(&self, ui: &Ui) {
        let (t, r, p) = (self.t, self.r, ui.painter());
        let c = self.label_color();
        let pad = if self.compact { 6.0 } else { 12.0 };
        let ccx = (self.unsnapped_x + pad + 8.0).round();
        let centre = egui::pos2(ccx, r.min.y + 14.0);
        let (num_color, text_color) = if self.active { (t.text_on_accent, c) } else { (c, c) };
        if self.active {
            p.circle_filled(centre, 8.0, t.accent);
            p.rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 2.0), r.max), 0.0, t.accent_bright);
        } else {
            p.circle_stroke(centre, 7.0, Stroke::new(1.0, t.border_control)); // egui strokes a circle outside its radius: ring 7..8
        }
        let ng = line(p, &self.number.to_string(), ty::chip(), 14.0, f32::INFINITY);
        // The number is centred on the circle's unsnapped box (the ring itself sits on whole pixels).
        paint_line(p, Rect::from_center_size(egui::pos2(self.unsnapped_x + pad + 8.0 + LABEL_DX, centre.y), egui::vec2(16.0, 16.0)), ng, 14.0, Align::Center, num_color, 0.0);
        let mut x = self.unsnapped_x + pad + 16.0;
        if !(self.compact && !self.active) {
            x += 6.0;
            x += self.label(ui, x, text_color);
        }
        if let Some(s) = self.tab.status {
            let col = if s == StageStatus::Blocked { t.error } else { t.warning };
            let d = egui::pos2((x + 6.0).round() + 3.0, r.min.y + 14.0 + 0.0);
            p.rect_filled(Rect::from_center_size(d, egui::vec2(6.0, 6.0)), CornerRadius::same(3), col);
        }
        if self.focus { self.ring(ui, CornerRadius::same(3)) }
    }

    fn panel(&self, ui: &Ui) {
        let (t, r, p) = (self.t, self.r, ui.painter());
        let fill = if self.en && self.active { t.bg_panel } else if self.en && self.vis != Vis::Rest { t.bg_hover } else { t.bg_app };
        p.rect_filled(r, 0.0, fill);
        if self.active && self.en { p.rect_filled(Rect::from_min_size(r.min, egui::vec2(r.width(), 2.0)), 0.0, t.accent_bright); }
        // The strip's 1 px bottom line runs under every tab except the active one, which joins the panel body.
        if !self.active { p.rect_filled(Rect::from_min_max(egui::pos2(r.min.x, r.max.y - 1.0), r.max), 0.0, t.border_divider); }
        p.rect_filled(Rect::from_min_max(egui::pos2(r.max.x - 1.0, r.min.y), r.max), 0.0, t.border_divider);
        // Hover only changes the fill; the label stays secondary until the tab is active.
        let c = if !self.en { t.text_disabled } else if self.active { t.text_primary } else { t.text_secondary };
        let mut x = self.unsnapped_x + 12.0;
        self.icon(ui, x, c);
        if self.tab.icon.is_some() { x += 22.0 }
        let w = self.label(ui, x, c);
        if let (Some(b), Some(bw)) = (self.tab.badge, self.badge_w) {
            self.count_badge(p, egui::pos2((x + w + 6.0 + bw / 2.0).round(), r.center().y.round()), b, bw);
        }
        if self.focus { self.ring(ui, CornerRadius::ZERO) }
    }

    fn count_badge(&self, p: &egui::Painter, center: egui::Pos2, count: u32, width: f32) {
        let rect = Rect::from_center_size(center, egui::vec2(width.round(), 16.0));
        p.rect_filled(rect, CornerRadius::same(8), self.t.bg_pressed);
        let g = line(p, &count.to_string(), ty::chip(), 14.0, f32::INFINITY);
        paint_line(p, rect, g, 14.0, Align::Center, self.t.text_primary, 0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rects(compact: bool, whole: bool) -> Vec<Rect> {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let tabs = [Tab::new("Board").icon(Icon::Board), Tab::new("Machines").icon(Icon::Machines), Tab::new("Settings").icon(Icon::Settings)];
        let mut out = Vec::new();
        let mut active = usize::MAX;
        let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
            TabStrip::new(TabKind::Page, &mut active, &tabs).compact(compact).whole_px(whole).at(egui::pos2(8.0, 31.0)).rects_out(&mut out).show(ui);
        });
        full.textures_delta.clear();
        out
    }

    /// Whole-pixel page tabs: integral widths, 2 px gaps, no drift (Start mockups fix 4); Compact: 28 x 28 icon tabs.
    #[test]
    fn page_tabs_whole_pixel_and_compact() {
        let w = rects(false, true);
        for (i, r) in w.iter().enumerate() {
            assert_eq!(r.width().fract(), 0.0, "{r:?}");
            if i > 0 { assert_eq!(r.min.x - w[i - 1].max.x, 2.0) }
        }
        let c = rects(true, true);
        assert!(c.iter().all(|r| r.size() == egui::vec2(28.0, 28.0)), "{c:?}");
        assert_eq!(c.iter().map(|r| r.min.x).collect::<Vec<_>>(), vec![8.0, 38.0, 68.0]);
    }

    #[test]
    fn g2_compact_models_badge_reserves_space_on_the_existing_tab() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::fonts::install(&ctx);
        let tabs = [
            Tab::new("Board").icon(Icon::Board),
            Tab::new("Models").icon(Icon::Models).badge(12).status(StageStatus::Blocked).tip("Models\n12 required models missing\nCtrl+Shift+4"),
        ];
        let mut active = 0;
        let mut rects = Vec::new();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            TabStrip::new(TabKind::Page, &mut active, &tabs).compact(true).whole_px(true).rects_out(&mut rects).show(ui);
        });
        out.textures_delta.clear();
        assert_eq!(rects[0].width(), 28.0);
        assert!(rects[1].width() > 28.0, "the cached count needs room beside the Models icon");
        let models = &out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_, n)| n.label() == Some("Models")).unwrap().1;
        assert_eq!(models.description(), Some("Models\n12 required models missing\nCtrl+Shift+4"));
    }
}
