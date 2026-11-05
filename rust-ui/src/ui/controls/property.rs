//! Properties panel building blocks (bible §7.2, §8.2, §8.3, §4, §11, §10.2). Layout only: the inner controls are put
//! into a row's control column by the caller. Fixed geometry: header 28, section header 24, rows 24 (48 for stacked rows,
//! 18 / 32 for message rows), label column 112 (panel ≥ 320) or 96, 8 px gutters, control column fills.
use super::action_bar::{action_bar_in, ActionBar, ActionBarOut, BAR_HEIGHT};
use super::buttons::{icon_button, IconButtonSize, Vis};
use super::{keyboard_focus, line, paint_line};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, accesskit::Role, text::LayoutJob, Align, Color32, Id, Layout, Rect, Response, ScrollArea, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, Vec2, WidgetInfo,
};

/// Property panel header (§7.2).
const HEADER_H: f32 = 28.0;
/// Section header and property row (§8.2, §8.3).
const SECTION_H: f32 = 24.0;
const ROW_H: f32 = 24.0;
/// Section body padding: the sheet's 2 above the first row, §8.3's 4 below the last.
const BODY_TOP: f32 = 2.0;
const BODY_BOTTOM: f32 = 4.0;
/// Collapse / expand (§11): 100 ms, linear.
const COLLAPSE_SECS: f32 = 0.1;

/// Label column width: 112 at a panel ≥ 320 wide, 96 below (§4, §8.2).
pub fn label_width(panel_w: f32) -> f32 { if panel_w >= 320.0 { 112.0 } else { 96.0 } }

/// Message rows and section badges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Warning, Error }

/// A section header's status badge (`warning` "Out of date" on Deliver › Outputs, or `error`).
#[derive(Clone, Copy)]
pub struct Badge<'a> { pub status: Status, pub text: &'a str }

type MenuFn<'a> = Box<dyn FnOnce(&mut Ui) + 'a>;

/// Panel-level options.
#[derive(Default)]
pub struct PanelOpts<'a> {
    /// The panel header's ⋯ menu (right end of the header).
    pub header_menu: Option<MenuFn<'a>>,
    /// Settings › Appearance › Reduce motion: collapse animation time 0.
    pub reduced_motion: bool,
}

/// `properties_panel(ui, "Properties", "Key", 320.0, |body| …, Some(bar))`: header 28 "Properties · <stage>", one
/// scroll body, the action bar pinned at the bottom (72). Fills `ui`'s remaining height.
pub fn properties_panel<'a>(ui: &mut Ui, title: &str, stage: &str, width: f32, body: impl FnOnce(&mut PropBody), bar: Option<ActionBar<'a>>) -> Option<ActionBarOut> {
    properties_panel_with(ui, title, stage, width, PanelOpts::default(), body, bar)
}

pub fn properties_panel_with<'a>(
    ui: &mut Ui, title: &str, stage: &str, width: f32, opts: PanelOpts<'_>, body: impl FnOnce(&mut PropBody), bar: Option<ActionBar<'a>>,
) -> Option<ActionBarOut> {
    let t = tokens(ui);
    let h = ui.available_height().max(HEADER_H + if bar.is_some() { BAR_HEIGHT } else { 0.0 });
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, h), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.bg_panel);

    // Header: "Properties" SemiBold, "· <stage>" secondary, optional ⋯ at the right; 1 px divider as its last row.
    let head = Rect::from_min_size(rect.min, Vec2::new(width, HEADER_H));
    p.rect_filled(Rect::from_min_size(egui::pos2(rect.min.x, head.max.y - 1.0), Vec2::new(width, 1.0)), 0.0, t.border_divider);
    let tr = Rect::from_min_max(egui::pos2(head.min.x + 8.0, head.min.y), egui::pos2(head.max.x - 8.0, head.max.y - 1.0));
    let g = line(&p, title, ty::panel_header(), 16.0, f32::INFINITY);
    let tw = g.size().x;
    paint_line(&p, tr, g, 16.0, Align::Min, t.text_primary, 0.0);
    let st = Rect::from_min_max(egui::pos2(tr.min.x + tw + 6.0, tr.min.y), tr.max);
    paint_line(&p, st, line(&p, &format!("· {stage}"), ty::label(), 16.0, st.width().max(1.0)), 16.0, Align::Min, t.text_secondary, 0.0);
    if let Some(menu) = opts.header_menu {
        let b = Rect::from_min_size(egui::pos2(head.max.x - 8.0 - 18.0, head.min.y + 5.0), Vec2::splat(18.0));
        let r = ui.add(icon_button(Icon::More, IconButtonSize::Small18, "Panel actions").in_rect(b));
        egui::Popup::menu(&r).show(menu);
    }

    // Body: one scroll area between header and bar.
    let bar_h = if bar.is_some() { BAR_HEIGHT } else { 0.0 };
    let body_rect = Rect::from_min_max(egui::pos2(rect.min.x, head.max.y), egui::pos2(rect.max.x, rect.max.y - bar_h));
    let mut bui = ui.new_child(UiBuilder::new().max_rect(body_rect).layout(Layout::top_down(Align::Min)).id_salt(("prop-body", title, stage)));
    bui.set_clip_rect(body_rect.intersect(ui.clip_rect()));
    let reduced = opts.reduced_motion;
    ScrollArea::vertical().id_salt("prop-scroll").auto_shrink([false, false]).show(&mut bui, |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let mut pb = PropBody { w: ui.available_width(), panel_w: width, ui, reduced_motion: reduced, n: 0, force_header: None };
        body(&mut pb);
    });

    bar.map(|b| {
        let r = Rect::from_min_max(egui::pos2(rect.min.x, rect.max.y - BAR_HEIGHT), rect.max);
        action_bar_in(ui, r, b)
    })
}

/// The scroll body: stacked sections.
pub struct PropBody<'u> {
    ui: &'u mut Ui,
    w: f32,
    panel_w: f32,
    reduced_motion: bool,
    n: usize,
    force_header: Option<(Vis, bool)>,
}

impl<'u> PropBody<'u> {
    pub fn ui(&mut self) -> &mut Ui { self.ui }
    /// Specimen / test only: draw the next section header in a forced pointer state (and keyboard focus ring).
    pub fn force_next_header(&mut self, vis: Vis, focus: bool) { self.force_header = Some((vis, focus)); }

    /// One section: header (24, whole header toggles `collapsed`, optional badge and ⋯ menu), body, 1 px divider.
    pub fn section(&mut self, title: &str, badge: Option<Badge<'_>>, menu: Option<MenuFn<'_>>, collapsed: &mut bool, rows: impl FnOnce(&mut PropRows)) -> Response {
        let t = tokens(&*self.ui);
        self.ui.spacing_mut().item_spacing = Vec2::ZERO; // sections stack edge to edge
        self.n += 1;
        let id = self.ui.scope_id().with(("prop-section", self.n, title));
        let (rect, _) = self.ui.allocate_exact_size(Vec2::new(self.w, SECTION_H), Sense::hover());
        let resp = self.ui.interact(rect, id, Sense::click());
        if resp.clicked() { *collapsed = !*collapsed; }
        let (vis, focus) = match self.force_header.take() {
            Some(f) => f,
            None => (if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest }, resp.has_focus() && keyboard_focus(self.ui.ctx())),
        };
        let expanded = !*collapsed;
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, title));
        self.ui.ctx().accesskit_node_builder(resp.id, |n| n.set_expanded(expanded));
        let p = self.ui.painter().clone();
        p.rect_filled(rect, 0.0, if vis == Vis::Rest { t.bg_raised } else { t.bg_hover });
        if focus { p.rect_stroke(rect, 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
        let chev = Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.center().y - 6.0), Vec2::splat(12.0));
        icons::paint(&p, chev, if expanded { Icon::ChevronDown } else { Icon::ChevronRight }, IconSize::S12, t.text_secondary);
        // Right side, right to left: ⋯ menu button, then the status badge.
        let mut right = rect.max.x - 8.0;
        let mut menu_resp = None;
        if let Some(m) = menu {
            let b = Rect::from_min_size(egui::pos2(right - 18.0, rect.min.y + 3.0), Vec2::splat(18.0));
            let r = self.ui.add(icon_button(Icon::More, IconButtonSize::Small18, "Section actions").in_rect(b));
            egui::Popup::menu(&r).show(m);
            menu_resp = Some(r);
            right = b.min.x - 6.0;
        }
        if let Some(b) = badge {
            let col = status_color(t, b.status);
            let g = line(&p, b.text, ty::small(), 14.0, f32::INFINITY);
            let tx = right - g.size().x;
            p.circle_filled(egui::pos2(tx - 4.0 - 3.0, rect.center().y), 3.0, col);
            paint_line(&p, Rect::from_min_max(egui::pos2(tx, rect.min.y), egui::pos2(right, rect.max.y)), g, 14.0, Align::Min, col, 0.0);
            right = tx - 4.0 - 6.0;
        }
        let tr = Rect::from_min_max(egui::pos2(rect.min.x + 24.0, rect.min.y), egui::pos2(right.max(rect.min.x + 25.0), rect.max.y));
        paint_line(&p, tr, line(&p, title, ty::section_header(), 16.0, f32::INFINITY), 16.0, Align::Min, t.text_primary, 0.0);
        let _ = menu_resp;

        let (w, panel_w, reduced) = (self.w, self.panel_w, self.reduced_motion);
        animated(self.ui, id.with("body"), expanded, reduced, BODY_TOP, BODY_BOTTOM, |ui| {
            rows(&mut PropRows { ui, w, panel_w, reduced_motion: reduced, n: 0 });
        });
        let (line_rect, _) = self.ui.allocate_exact_size(Vec2::new(self.w, 1.0), Sense::hover());
        p.rect_filled(line_rect, 0.0, t.border_divider);
        resp
    }
}

fn status_color(t: &crate::ui::tokens::Tokens, s: Status) -> Color32 { match s { Status::Warning => t.warning, Status::Error => t.error } }

/// Run `body` in an animated-height clip: height 0 → full over 100 ms (linear), `top` / `bottom` padding inside. Nothing
/// is drawn (or made interactive) at height 0.
fn animated(ui: &mut Ui, id: Id, open: bool, reduced: bool, top: f32, bottom: f32, body: impl FnOnce(&mut Ui)) {
    let ctx = ui.ctx().clone();
    let k = ctx.animate_bool_with_time(id.with("open"), open, if reduced { 0.0 } else { COLLAPSE_SECS });
    if k <= 0.0 { return; }
    let w = ui.max_rect().width();
    let pos = ui.cursor().min;
    let prev: Option<f32> = ctx.data(|d| d.get_temp(id.with("h")));
    let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(pos, Vec2::new(w, 1.0e5))).layout(Layout::top_down(Align::Min)).id_salt(id));
    child.spacing_mut().item_spacing = Vec2::ZERO;
    if let (true, Some(h)) = (k < 1.0, prev) {
        child.set_clip_rect(Rect::from_min_size(pos, Vec2::new(w, h * k)).intersect(ui.clip_rect()));
    }
    child.add_space(top);
    body(&mut child);
    child.add_space(bottom);
    let full = child.min_rect().max.y - pos.y;
    ctx.data_mut(|d| d.insert_temp(id.with("h"), full));
    ui.allocate_space(Vec2::new(w, if k >= 1.0 { full } else { full * k }));
}

/// Column rects of one row (all inside `row`).
#[derive(Clone, Copy, Debug)]
pub struct Cols {
    pub row: Rect,
    pub label: Rect,
    pub control: Rect,
}

/// Rows of one section body (or Details).
pub struct PropRows<'u> {
    ui: &'u mut Ui,
    w: f32,
    panel_w: f32,
    reduced_motion: bool,
    n: usize,
}

impl<'u> PropRows<'u> {
    pub fn label_width(&self) -> f32 { label_width(self.panel_w) }
    /// Width of the control column in a single-line row (184 at 320, 160 at 280).
    pub fn control_width(&self) -> f32 { self.w - 16.0 - self.label_width() - 8.0 }

    fn cols(&self, row: Rect, stacked: bool) -> Cols {
        let lw = self.label_width();
        let label = Rect::from_min_size(egui::pos2(row.min.x + 8.0, row.min.y), Vec2::new(lw, ROW_H));
        let control = if stacked {
            Rect::from_min_max(egui::pos2(row.min.x + 8.0, row.min.y + ROW_H), egui::pos2(row.max.x - 8.0, row.max.y))
        } else {
            Rect::from_min_max(egui::pos2(label.max.x + 8.0, row.min.y), egui::pos2(row.max.x - 8.0, row.max.y))
        };
        Cols { row, label, control }
    }

    fn alloc(&mut self, h: f32) -> Rect {
        self.n += 1;
        self.ui.allocate_exact_size(Vec2::new(self.w, h), Sense::hover()).0
    }

    fn control_ui(&mut self, rect: Rect) -> Ui {
        let mut c = self.ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::left_to_right(Align::Center)).id_salt(("prop-control", self.n)));
        c.set_clip_rect(rect.intersect(self.ui.clip_rect()));
        c.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
        c
    }

    /// Label text in its column: 12 Regular secondary, never truncated; the tooltip is the full name plus its help.
    fn paint_label(&mut self, label_rect: Rect, label: &str, tooltip: &str) {
        let t = tokens(&*self.ui);
        let p = self.ui.painter().clone();
        let g = line(&p, label, ty::label(), 16.0, f32::INFINITY);
        debug_assert!(g.size().x <= label_rect.width() + 0.5, "property label {label:?} does not fit its column");
        paint_line(&p, label_rect, g, 16.0, Align::Min, t.text_secondary, 0.0);
        let tip = if tooltip.is_empty() { label } else { tooltip };
        self.ui.interact(label_rect, self.ui.scope_id().with(("prop-label", self.n)), Sense::hover()).on_hover_text(tip);
    }

    /// The general row: fills the row (selected = `accent.tint` + 2 px `accent.bright` left bar, §8.4), then calls `f`
    /// to paint the label column and the control column. `stacked` makes the 48 high two-line row.
    pub fn row_full<R>(&mut self, selected: bool, stacked: bool, f: impl FnOnce(&mut Ui, Cols) -> R) -> R {
        let t = tokens(&*self.ui);
        let row = self.alloc(if stacked { 2.0 * ROW_H } else { ROW_H });
        if selected {
            let p = self.ui.painter();
            p.rect_filled(row, 0.0, t.accent_tint);
            p.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row.height())), 0.0, t.accent_bright);
        }
        let cols = self.cols(row, stacked);
        let mut c = self.control_ui(cols.control);
        c.set_clip_rect(row.intersect(self.ui.clip_rect())); // the closure paints the label column too
        f(&mut c, cols)
    }

    /// Label + control: `[8][label][8][control fills][8]`, height 24.
    pub fn row<R>(&mut self, label: &str, tooltip: &str, f: impl FnOnce(&mut Ui) -> R) -> R {
        let row = self.alloc(ROW_H);
        let cols = self.cols(row, false);
        self.paint_label(cols.label, label, tooltip);
        let mut c = self.control_ui(cols.control);
        f(&mut c)
    }

    /// Two-line row (48): the label on the first 24 line, the control across the full width on the second.
    pub fn row48<R>(&mut self, label: &str, f: impl FnOnce(&mut Ui) -> R) -> R { self.row48_tip(label, "", f) }

    pub fn row48_tip<R>(&mut self, label: &str, tooltip: &str, f: impl FnOnce(&mut Ui) -> R) -> R {
        let row = self.alloc(2.0 * ROW_H);
        let cols = self.cols(row, true);
        self.paint_label(cols.label, label, tooltip);
        let mut c = self.control_ui(cols.control);
        f(&mut c)
    }

    /// §8.3 "File type" fit rule: a normal row when the control column holds `needed` px, else the 48 high stacked row.
    pub fn row_or_stacked<R>(&mut self, label: &str, tooltip: &str, needed: f32, f: impl FnOnce(&mut Ui) -> R) -> R {
        if self.control_width() >= needed { self.row(label, tooltip, f) } else { self.row48_tip(label, tooltip, f) }
    }

    /// Read-only value: 12 primary in the control column (end-truncated with the full text in a tooltip).
    pub fn readonly(&mut self, label: &str, value: &str) {
        let t = tokens(&*self.ui);
        let row = self.alloc(ROW_H);
        let cols = self.cols(row, false);
        self.paint_label(cols.label, label, "");
        let p = self.ui.painter().clone();
        let g = line(&p, value, ty::value(), 16.0, cols.control.width());
        let cut = g.elided;
        paint_line(&p, cols.control, g, 16.0, Align::Min, t.text_primary, 0.0);
        if cut { self.ui.interact(cols.control, self.ui.scope_id().with(("prop-value", self.n)), Sense::hover()).on_hover_text(value); }
    }

    /// Message row under a field (§8.2): 14 px status icon + 11/14 text in the control column, wrapped, never truncated:
    /// 18 high for one line, 32 for two (4 + 14 per line).
    pub fn message(&mut self, status: Status, text: &str) -> Response {
        let t = tokens(&*self.ui);
        let p = self.ui.painter().clone();
        let lw = self.label_width();
        let x = 8.0 + lw + 8.0;
        let text_w = (self.w - x - 8.0 - 14.0 - 6.0).max(20.0);
        let g = wrapped(&p, text, text_w);
        let h = 4.0 + 14.0 * g.rows.len().max(1) as f32;
        let row = self.alloc(h);
        let col = status_color(t, status);
        let icon = Rect::from_min_size(egui::pos2(row.min.x + x, row.min.y + 2.0), Vec2::splat(14.0));
        icons::paint(&p, icon, if status == Status::Error { Icon::Error } else { Icon::Warning }, IconSize::S14, col);
        p.galley(egui::pos2(icon.max.x + 6.0, row.min.y + 2.0), g, col);
        let resp = self.ui.interact(row, self.ui.scope_id().with(("prop-msg", self.n)), Sense::hover());
        resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
        self.ui.ctx().accesskit_node_builder(resp.id, |n| n.set_role(Role::Status));
        resp
    }

    /// Path row (§8.3): label, path (11 mono secondary, middle-truncated to its last segments, full path in the tooltip),
    /// copy 18, reveal 18. Copy puts the full path on the clipboard; `reveal` is the caller's.
    pub fn path(&mut self, label: &str, path: &str) -> PathOut {
        let t = tokens(&*self.ui);
        let row = self.alloc(ROW_H);
        let cols = self.cols(row, false);
        self.paint_label(cols.label, label, "");
        let p = self.ui.painter().clone();
        let y = cols.control.min.y + 3.0;
        let rv = Rect::from_min_size(egui::pos2(cols.control.max.x - 18.0, y), Vec2::splat(18.0));
        let cp = Rect::from_min_size(egui::pos2(rv.min.x - 4.0 - 18.0, y), Vec2::splat(18.0));
        let tr = Rect::from_min_max(cols.control.min, egui::pos2(cp.min.x - 4.0, cols.control.max.y));
        let shown = middle_truncate(path, |s| line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x <= tr.width());
        paint_line(&p, tr, line(&p, &shown, ty::mono(), 14.0, f32::INFINITY), 14.0, Align::Min, t.text_secondary, 0.0);
        self.ui.interact(tr, self.ui.scope_id().with(("prop-path", self.n)), Sense::hover()).on_hover_text(path);
        let copy = self.ui.add(icon_button(Icon::Copy, IconButtonSize::Small18, "Copy path").in_rect(cp));
        if copy.clicked() { self.ui.ctx().copy_text(path.to_owned()); }
        let reveal = self.ui.add(icon_button(Icon::Reveal, IconButtonSize::Small18, "Reveal").in_rect(rv));
        PathOut { copy, reveal }
    }

    /// "Details" disclosure (collapsed by default, state kept in memory): a row with a chevron, then `f`'s rows
    /// ([`PropRows::id_row`]).
    pub fn details(&mut self, f: impl FnOnce(&mut PropRows)) { self.details_state(false, f) }

    /// As [`PropRows::details`] with the state it starts in (specimen / tests).
    pub fn details_state(&mut self, initially_open: bool, f: impl FnOnce(&mut PropRows)) {
        let t = tokens(&*self.ui);
        let (w, panel_w, reduced) = (self.w, self.panel_w, self.reduced_motion);
        let row = self.alloc(ROW_H);
        let id = self.ui.scope_id().with(("prop-details", self.n));
        let open: bool = self.ui.ctx().data(|d| d.get_temp(id)).unwrap_or(initially_open);
        let resp = self.ui.interact(row, id, Sense::click());
        if resp.clicked() { self.ui.ctx().data_mut(|d| d.insert_temp(id, !open)); }
        let open = if resp.clicked() { !open } else { open };
        resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "Details"));
        self.ui.ctx().accesskit_node_builder(resp.id, |n| n.set_expanded(open));
        let p = self.ui.painter().clone();
        if resp.hovered() { p.rect_filled(row, 0.0, t.bg_hover); }
        if resp.has_focus() && keyboard_focus(self.ui.ctx()) { p.rect_stroke(row, 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
        let chev = Rect::from_min_size(egui::pos2(row.min.x + 8.0, row.center().y - 6.0), Vec2::splat(12.0));
        icons::paint(&p, chev, if open { Icon::ChevronDown } else { Icon::ChevronRight }, IconSize::S12, t.text_secondary);
        paint_line(&p, Rect::from_min_max(egui::pos2(row.min.x + 24.0, row.min.y), row.max), line(&p, "Details", ty::label(), 16.0, f32::INFINITY), 16.0, Align::Min, t.text_secondary, 0.0);
        animated(self.ui, id.with("body"), open, reduced, 0.0, 0.0, |ui| f(&mut PropRows { ui, w, panel_w, reduced_motion: reduced, n: 0 }));
    }

    /// One Details line: 11 label (tertiary), the id in 11 mono tertiary (middle-truncated, full value in the tooltip),
    /// copy icon 18.
    pub fn id_row(&mut self, label: &str, value: &str) -> Response {
        let t = tokens(&*self.ui);
        let row = self.alloc(ROW_H);
        let cols = self.cols(row, false);
        let p = self.ui.painter().clone();
        paint_line(&p, cols.label, line(&p, label, ty::small(), 14.0, f32::INFINITY), 14.0, Align::Min, t.text_tertiary, 0.0);
        let cp = Rect::from_min_size(egui::pos2(cols.control.max.x - 18.0, cols.control.min.y + 3.0), Vec2::splat(18.0));
        let tr = Rect::from_min_max(cols.control.min, egui::pos2(cp.min.x - 4.0, cols.control.max.y));
        let shown = middle_truncate(value, |s| line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x <= tr.width());
        paint_line(&p, tr, line(&p, &shown, ty::mono(), 14.0, f32::INFINITY), 14.0, Align::Min, t.text_tertiary, 0.0);
        self.ui.interact(tr, self.ui.scope_id().with(("prop-id", self.n)), Sense::hover()).on_hover_text(value);
        let copy = self.ui.add(icon_button(Icon::Copy, IconButtonSize::Small18, "Copy").in_rect(cp));
        if copy.clicked() { self.ui.ctx().copy_text(value.to_owned()); }
        copy
    }
}

pub struct PathOut {
    pub copy: Response,
    pub reveal: Response,
}

/// 11/14 text wrapped to `width`, no row limit (message rows never truncate).
fn wrapped(painter: &egui::Painter, text: &str, width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_width = width;
    job.append(text, 0.0, ty::format(ty::small(), Color32::PLACEHOLDER, 14.0));
    painter.layout_job(job)
}

/// Middle-truncate a path so it fits (`fits` measures): the whole path, else "…/" plus as many trailing segments as fit
/// (at least the last two), else "…" plus the tail of the last two segments.
pub fn middle_truncate(path: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(path) { return path.to_owned(); }
    let segs: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();
    for n in (2..segs.len()).rev() {
        let cand = format!("…/{}", segs[segs.len() - n..].join("/"));
        if fits(&cand) { return cand; }
    }
    let tail = if segs.len() >= 2 { segs[segs.len() - 2..].join("/") } else { path.to_owned() };
    let chars: Vec<char> = tail.chars().collect();
    for skip in 0..chars.len() {
        let cand = format!("…{}", chars[skip..].iter().collect::<String>());
        if fits(&cand) { return cand; }
    }
    "…".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, t: f64, events: Vec<egui::Event>, f: impl FnMut(&mut Ui)) {
        let raw = egui::RawInput { time: Some(t), events, screen_rect: Some(Rect::from_min_size(egui::pos2(0.0, 0.0), Vec2::new(400.0, 700.0))), ..Default::default() };
        ctx.run_ui(raw, f).textures_delta.clear();
    }

    /// A context with the embedded fonts installed (one frame so they take effect).
    fn ctx() -> egui::Context {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        frame(&ctx, 0.0, vec![], |_| {});
        ctx
    }

    /// §4, §8.2: label column 112 at 320 and 96 at 280; every sheet label is shorter than the column and is laid out
    /// without truncation; control column 184 / 160; rows are 24, stacked rows 48.
    #[test]
    fn property_label_never_truncates_and_width_by_panel() {
        let ctx = ctx();
        for (w, lw, cw) in [(320.0, 112.0, 184.0), (280.0, 96.0, 160.0)] {
            let mut seen = Vec::new();
            frame(&ctx, 0.0, vec![], |ui| {
                ui.set_max_width(w);
                let mut collapsed = false;
                let mut pb = PropBody { w, panel_w: w, ui, reduced_motion: true, n: 0, force_header: None };
                pb.section("Key engine", None, None, &mut collapsed, |rows| {
                    assert_eq!((rows.label_width(), rows.control_width()), (lw, cw));
                    let p = rows.ui.painter().clone();
                    for l in ["Machine", "Model", "VRAM profile", "Temporal", "Strength", "Compression", "Encoder value", "Output folder", "Frame rate", "Position", "Out of range", "Missing frames"] {
                        let g = line(&p, l, ty::label(), 16.0, f32::INFINITY);
                        assert!(!g.elided && g.size().x <= lw, "{l}: {} > {lw}", g.size().x);
                    }
                    rows.row("Machine", "Machine · where the key runs", |ui| seen.push(ui.max_rect()));
                    rows.row48("Range", |ui| seen.push(ui.max_rect()));
                    let r = rows.row_or_stacked("File type", "", 174.0, |ui| ui.max_rect());
                    seen.push(r);
                });
            });
            assert_eq!(seen[0].height(), 24.0);
            assert_eq!(seen[0].width(), cw);
            assert_eq!((seen[1].height(), seen[1].width()), (24.0, w - 16.0));
            // File type: 174 needed → one 24 line at 320 (184), the stacked 24 second line (264) at 280.
            if w == 320.0 { assert_eq!(seen[2].width(), 184.0) } else { assert_eq!(seen[2].width(), 264.0) }
        }
    }

    /// §4 / §8.2: a message row is 18 for one line, 32 for two, and wraps instead of truncating.
    #[test]
    fn message_row_18_or_32_never_truncates() {
        let ctx = ctx();
        for w in [320.0, 280.0] {
            let mut hs = Vec::new();
            frame(&ctx, 0.0, vec![], |ui| {
                ui.set_max_width(w);
                let mut collapsed = false;
                let mut pb = PropBody { w, panel_w: w, ui, reduced_motion: true, n: 0, force_header: None };
                pb.section("Format", None, None, &mut collapsed, |rows| {
                    for (s, text) in [(Status::Warning, "DWAB is lossy."), (Status::Warning, "DWAB is lossy: HDR values are not exact."), (Status::Error, "No encoder for this file type on this machine, pick another codec or file type.")] {
                        hs.push(rows.message(s, text).rect.height());
                    }
                });
            });
            assert_eq!(hs[0], 18.0);
            assert_eq!(hs[1], 32.0, "width {w}");
            assert!(hs[2] >= 32.0 && (hs[2] - 4.0) % 14.0 == 0.0, "long text grows by whole lines, never truncates: {}", hs[2]);
        }
    }

    /// §11: the header toggles the section; the body height animates linearly over 100 ms between full and 0.
    #[test]
    fn section_collapse_toggles_and_animates_100ms() {
        let ctx = ctx();
        let mut collapsed = false;
        let mut heights = Vec::new();
        let run = |t: f64, events: Vec<egui::Event>, collapsed: &mut bool| -> (f32, Rect) {
            let mut out = (0.0, Rect::NOTHING);
            frame(&ctx, t, events, |ui| {
                ui.set_max_width(320.0);
                let top = ui.cursor().min.y;
                let mut pb = PropBody { w: 320.0, panel_w: 320.0, ui, reduced_motion: false, n: 0, force_header: None };
                let r = pb.section("Matte", None, None, collapsed, |rows| { for _ in 0..3 { rows.readonly("Erode", "0"); } });
                out = (pb.ui.cursor().min.y - top, r.rect);
            });
            out
        };
        // Expanded: header 24 + (2 + 3 × 24 + 4) + divider 1.
        let (full, header) = run(0.0, vec![], &mut collapsed);
        assert_eq!(full, 24.0 + 78.0 + 1.0);
        // A click on the header collapses it.
        let pos = header.center();
        let ev = |pressed| vec![egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }];
        run(0.01, vec![egui::Event::PointerMoved(pos)], &mut collapsed);
        run(0.02, ev(true), &mut collapsed);
        run(0.03, ev(false), &mut collapsed);
        assert!(collapsed, "header click toggles");
        // Animate in 20 ms steps: strictly shrinking to 24 + 1, closed within 100 ms (+ one frame) of the click.
        let mut closed_at = None;
        for k in 1..=15 {
            let t = 0.03 + 0.02 * k as f64;
            let h = run(t, vec![], &mut collapsed).0;
            if closed_at.is_none() && h == 25.0 { closed_at = Some(t - 0.03); }
            heights.push(h);
        }
        assert!(heights[0] < full && heights[0] > 25.0, "{heights:?}");
        assert!(heights.windows(2).all(|w| w[1] <= w[0]), "monotonic {heights:?}");
        assert!(heights.iter().any(|h| *h > 30.0 && *h < full - 10.0), "intermediate heights {heights:?}");
        let c = closed_at.expect("closes");
        assert!((0.08..=0.141).contains(&c), "closes after ~100 ms, got {c}");
        // Reopen.
        collapsed = false;
        let mut back = 0.0;
        for k in 1..=10 { back = run(0.40 + 0.02 * k as f64, vec![], &mut collapsed).0; }
        assert_eq!(back, full);
    }

    #[test]
    fn middle_truncate_keeps_last_segments() {
        let fits = |n: usize| move |s: &str| s.chars().count() <= n;
        let p = "/home/npittas/shots/clips/V1-0062/Output";
        assert_eq!(middle_truncate(p, fits(60)), p);
        assert_eq!(middle_truncate(p, fits(22)), "…/clips/V1-0062/Output");
        assert_eq!(middle_truncate(p, fits(20)), "…/V1-0062/Output");
        assert_eq!(middle_truncate(p, fits(10)), "…62/Output");
    }
}
