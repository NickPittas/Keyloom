//! The one table (bible §8.4, §5.14, §10.2, §12): header 24 (pinned vertically, scrolls horizontally with the body),
//! rows 24, columns that size to content with a minimum, 6 px resize edges with widths remembered per table, and the
//! two-axis overflow (8 px bars, 8×8 corner). A custom grid (no `egui_extras`): rows are painted at absolute positions
//! and only the visible ones are built. Cells are drawn by the caller through the [`RowUi`] helpers:
//!
//! ```ignore
//! let mut sel = BTreeSet::new();
//! let out = Table::new("machines", &COLS).rows(n, |row, i| { row.text(0, name(i)); row.dash(4); }).selected(&mut sel).show(ui);
//! ```
use super::buttons::{icon_button, Btn, IconButtonSize};
use super::{keyboard_focus, line, paint_line};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, accesskit::{self, Role}, Align, Color32, CursorIcon, EventFilter, FontId, Id, Key, Modifiers, Painter, Pos2, Rect, Response,
    Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};
use std::collections::BTreeSet;

pub const HEADER_H: f32 = 24.0;
pub const ROW_H: f32 = 24.0;
/// Scrollbar track thickness (§5.14).
pub const BAR: f32 = 8.0;
/// Width of the drag zone on each header's right edge (§8.4).
pub const EDGE: f32 = 6.0;
/// Narrowest a column can be dragged (or the column's own `min`, whichever is larger).
pub const MIN_COL: f32 = 48.0;
const PAD: f32 = 8.0;

/// One column. `min` is the content minimum; `flexible`: the last such column takes the width the others leave;
/// `numeric`: cells are right-aligned; `sortable`: a header click sorts.
#[derive(Clone, Copy, Debug)]
pub struct Column {
    pub title: &'static str,
    pub min: f32,
    pub flexible: bool,
    pub numeric: bool,
    pub sortable: bool,
}

impl Column {
    pub const fn new(title: &'static str, min: f32) -> Self { Column { title, min, flexible: false, numeric: false, sortable: false } }
    pub const fn flexible(mut self) -> Self { self.flexible = true; self }
    pub const fn numeric(mut self) -> Self { self.numeric = true; self }
    pub const fn sortable(mut self) -> Self { self.sortable = true; self }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sort {
    pub col: usize,
    pub ascending: bool,
}

/// Specimen / test only: states drawn through the same paint path as the real ones.
#[derive(Clone, Copy, Debug, Default)]
pub struct Force {
    pub hover_row: Option<usize>,
    /// Draw the resize line of this column's right edge as if hovered.
    pub resize_hover: Option<usize>,
    /// Scroll offsets (x, y), replacing the stored ones every frame.
    pub scroll: Option<Vec2>,
    /// Row that has keyboard focus (ring + inline actions).
    pub focus_row: Option<usize>,
}

/// What happened this frame, plus the geometry the tests and the page code need.
#[derive(Clone, Debug)]
pub struct TableOut {
    pub rect: Rect,
    /// Header cells in screen space (scroll applied, not clipped).
    pub header_rects: Vec<Rect>,
    /// Drawn rows (index, screen rect, scroll applied).
    pub row_rects: Vec<(usize, Rect)>,
    /// Final column widths (keep them in the settings file if they should survive a restart).
    pub widths: Vec<f32>,
    pub offset: Vec2,
    pub clicked: Option<usize>,
    /// Enter or double click.
    pub opened: Option<usize>,
    /// Shift+F10 or right click.
    pub menu: Option<usize>,
    pub sorted: Option<Sort>,
    pub selection_changed: bool,
    pub focus_row: Option<usize>,
    pub hovered_row: Option<usize>,
    /// Column whose width changed this frame (drag or double click).
    pub resized: Option<usize>,
}

#[derive(Clone, Default)]
struct State {
    widths: Vec<Option<f32>>,
    /// Widest cell seen per column (grows only; a double click on an edge goes back to it).
    content: Vec<f32>,
    offset: Vec2,
    focus: Option<usize>,
    anchor: Option<usize>,
    /// Column, pointer x and width at the press.
    drag: Option<(usize, f32, f32)>,
    seeded: bool,
}

/// Chrome's layout snapping: a box edge lands on the nearest pixel.
fn snap(v: f32) -> f32 { (v + 0.5).floor() }
fn sr(r: Rect) -> Rect { Rect::from_min_max(egui::pos2(snap(r.min.x), snap(r.min.y)), egui::pos2(snap(r.max.x), snap(r.max.y))) }
fn fill(p: &Painter, r: Rect, c: Color32) { p.rect_filled(sr(r), 0.0, c); }

type RowFn<'a> = Box<dyn FnMut(&mut RowUi, usize) + 'a>;

pub struct Table<'a> {
    id: Id,
    cols: &'a [Column],
    n: usize,
    row_fn: Option<RowFn<'a>>,
    selected: Option<&'a mut BTreeSet<usize>>,
    sort: Option<&'a mut Option<Sort>>,
    size: Option<Vec2>,
    widths: Option<&'a [f32]>,
    overlay: bool,
    force: Force,
    disabled: Option<&'a dyn Fn(usize) -> bool>,
    label: &'a str,
}

impl<'a> Table<'a> {
    pub fn new(id: impl egui::AsIdSalt, cols: &'a [Column]) -> Self {
        Table { id: Id::NULL.with(id), cols, n: 0, row_fn: None, selected: None, sort: None, size: None, widths: None, overlay: false,
                force: Force::default(), disabled: None, label: "Table" }
    }
    /// `n` rows; `f` draws the cells of row `i` with the [`RowUi`] helpers (called for the visible rows only).
    pub fn rows(mut self, n: usize, f: impl FnMut(&mut RowUi, usize) + 'a) -> Self { self.n = n; self.row_fn = Some(Box::new(f)); self }
    pub fn selected(mut self, sel: &'a mut BTreeSet<usize>) -> Self { self.selected = Some(sel); self }
    pub fn sort(mut self, sort: &'a mut Option<Sort>) -> Self { self.sort = Some(sort); self }
    /// Fixed outer size (the bars appear inside it); without it the table is as wide as the parent and as high as its rows.
    pub fn size(mut self, size: Vec2) -> Self { self.size = Some(size); self }
    /// Initial widths (first frame only; the user's drags win afterwards).
    pub fn widths(mut self, w: &'a [f32]) -> Self { self.widths = Some(w); self }
    /// Inside a popover or dialog: the bars sit on `bg.overlay` with `border.overlay`.
    pub fn on_overlay(mut self) -> Self { self.overlay = true; self }
    pub fn forced(mut self, f: Force) -> Self { self.force = f; self }
    pub fn disabled_rows(mut self, f: &'a dyn Fn(usize) -> bool) -> Self { self.disabled = Some(f); self }
    /// Accessible name of the table.
    pub fn label(mut self, l: &'a str) -> Self { self.label = l; self }

    pub fn show(mut self, ui: &mut Ui) -> TableOut {
        let t = tokens(ui);
        let (cols, n_cols, n, size) = (self.cols, self.cols.len(), self.n, self.size);
        let id = ui.make_persistent_id(self.id);
        let mut st: State = ui.data(|d| d.get_temp(id)).unwrap_or_default();
        st.widths.resize(n_cols, None);
        st.content.resize(n_cols, 0.0);
        if let (Some(w), false) = (self.widths, st.seeded) {
            for (i, &w) in w.iter().enumerate().take(n_cols) { st.widths[i] = Some(w) }
        }
        st.seeded = true;
        let hdr_need: Vec<f32> = cols.iter().map(|c| {
            line(ui.painter(), c.title, ty::chip(), 14.0, f32::INFINITY).size().x + 2.0 * PAD + if c.sortable { 16.0 } else { 0.0 }
        }).collect();
        let base = |st: &State, i: usize| -> f32 {
            match st.widths[i] {
                Some(w) => w.max(MIN_COL.max(cols[i].min)),
                None => cols[i].min.max(hdr_need[i]).max(st.content[i]),
            }
        };
        let rows_h = n as f32 * ROW_H;
        let outer_w = size.map_or(ui.available_width(), |s| s.x);
        let avail_h = ui.available_height();
        let sum0: f32 = (0..n_cols).map(|i| base(&st, i)).sum();
        // Which bars: the vertical one narrows the viewport, the horizontal one shortens it.
        let outer_h = |h: bool| size.map_or((HEADER_H + rows_h + if h { BAR } else { 0.0 }).min(avail_h.max(HEADER_H)), |s| s.y);
        let (mut v, mut h) = (false, false);
        for _ in 0..2 {
            h = sum0 > outer_w - if v { BAR } else { 0.0 } + 0.01;
            v = rows_h > outer_h(h) - HEADER_H - if h { BAR } else { 0.0 } + 0.01;
        }
        let oh = outer_h(h);
        let (vw, body_h) = (outer_w - if v { BAR } else { 0.0 }, (oh - HEADER_H - if h { BAR } else { 0.0 }).max(0.0));
        let (rect, _) = ui.allocate_exact_size(egui::vec2(outer_w, oh), Sense::hover());
        let mut tui = ui.new_child(UiBuilder::new().max_rect(rect));
        tui.set_clip_rect(sr(rect).intersect(ui.clip_rect()));
        let ui = &mut tui;
        let widths = |st: &State| -> Vec<f32> {
            let mut ws: Vec<f32> = (0..n_cols).map(|i| base(st, i)).collect();
            if let Some(f) = cols.iter().rposition(|c| c.flexible) {
                let others: f32 = ws.iter().sum::<f32>() - ws[f];
                ws[f] = ws[f].max(vw - others);
            }
            ws
        };
        let xs_of = |ws: &[f32], off: f32| -> Vec<f32> {
            let mut x = rect.min.x - off;
            ws.iter().map(|w| { let r = x; x += w; r }).collect()
        };
        let mut ws = widths(&st);
        let limits = |ws: &[f32]| ((ws.iter().sum::<f32>() - vw).max(0.0), (rows_h - body_h).max(0.0));
        let clamp = |st: &mut State, ws: &[f32]| {
            let (mx, my) = limits(ws);
            st.offset = egui::vec2(st.offset.x.clamp(0.0, mx), st.offset.y.clamp(0.0, my));
        };
        if let Some(s) = self.force.scroll { st.offset = s }
        clamp(&mut st, &ws);

        let header_vp = Rect::from_min_size(rect.min, egui::vec2(vw, HEADER_H));
        let body_vp = Rect::from_min_size(rect.min + egui::vec2(0.0, HEADER_H), egui::vec2(vw, body_h));
        let hbar = Rect::from_min_size(egui::pos2(rect.min.x, body_vp.max.y), egui::vec2(vw, BAR));
        let vbar = Rect::from_min_size(egui::pos2(rect.max.x - BAR, body_vp.min.y), egui::vec2(BAR, body_h));
        let mut out = TableOut {
            rect, header_rects: vec![], row_rects: vec![], widths: vec![], offset: st.offset, clicked: None, opened: None, menu: None,
            sorted: None, selection_changed: false, focus_row: None, hovered_row: None, resized: None,
        };

        // ---- header interaction: sort cells first, then the resize edges (later widgets win), then the drag ----
        let mut hdr = ui.new_child(UiBuilder::new().max_rect(header_vp));
        hdr.set_clip_rect(sr(header_vp).intersect(ui.clip_rect()));
        let mut cur_sort = self.sort.as_deref().copied().flatten();
        let xs = xs_of(&ws, st.offset.x);
        let cells: Vec<Response> = (0..n_cols).map(|i| {
            let cell = Rect::from_min_size(egui::pos2(xs[i], rect.min.y), egui::vec2(ws[i], HEADER_H));
            hdr.interact(cell, id.with(("hdr", i)), if cols[i].sortable { Sense::click() } else { Sense::hover() })
        }).collect();
        for (i, r) in cells.iter().enumerate() {
            if cols[i].sortable && r.clicked() {
                let s = Sort { col: i, ascending: cur_sort.map_or(true, |c| c.col != i || !c.ascending) };
                cur_sort = Some(s);
                out.sorted = Some(s);
            }
        }
        let mut edge_hot = vec![false; n_cols];
        for i in 0..n_cols.saturating_sub(1) {
            let right = xs[i] + ws[i];
            let er = Rect::from_min_max(egui::pos2(right - EDGE, rect.min.y), egui::pos2(right, rect.min.y + HEADER_H));
            let r = hdr.interact(er, id.with(("edge", i)), Sense::click_and_drag());
            if r.double_clicked() {
                st.widths[i] = None;
                st.drag = None;
                out.resized = Some(i);
            } else if r.is_pointer_button_down_on() && st.drag.is_none() {
                if let Some(p) = hdr.input(|inp| inp.pointer.interact_pos()) { st.drag = Some((i, p.x, ws[i])) }
            }
            edge_hot[i] = r.hovered() || r.dragged() || matches!(st.drag, Some((c, ..)) if c == i);
            if r.hovered() || r.dragged() { hdr.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal) }
        }
        if let Some((c, x0, w0)) = st.drag {
            if hdr.input(|inp| inp.pointer.primary_down()) {
                if let Some(p) = hdr.input(|inp| inp.pointer.interact_pos()) {
                    if (p.x - x0).abs() > 0.5 || st.widths[c].is_some() {
                        st.widths[c] = Some((w0 + p.x - x0).max(MIN_COL.max(cols[c].min)));
                        out.resized = Some(c);
                    }
                }
            } else {
                st.drag = None;
            }
        }
        ws = widths(&st);
        clamp(&mut st, &ws);
        let xs = xs_of(&ws, st.offset.x);
        let total_w = ws.iter().sum::<f32>().max(vw);

        // ---- keyboard (before the body widget exists, so Space / Enter are ours), wheel, clicks ----
        let body_id = id.with("body");
        let mut sel_now: BTreeSet<usize> = self.selected.as_deref().cloned().unwrap_or_default();
        let before = sel_now.clone();
        let has_focus = ui.memory(|m| m.has_focus(body_id));
        let mut kb_click = false;
        if has_focus && n > 0 {
            ui.memory_mut(|m| m.set_focus_lock_filter(body_id, EventFilter { vertical_arrows: true, ..Default::default() }));
            let page = ((body_h / ROW_H) as usize).max(1);
            let cur = st.focus.unwrap_or_else(|| sel_now.iter().next().copied().unwrap_or(0)).min(n - 1);
            let (target, extend, space, enter, menu) = ui.input_mut(|i| {
                let (mut tgt, mut ext) = (None, false);
                for (m, e) in [(Modifiers::SHIFT, true), (Modifiers::NONE, false)] {
                    if i.consume_key(m, Key::ArrowDown) { tgt = Some((cur + 1).min(n - 1)) }
                    else if i.consume_key(m, Key::ArrowUp) { tgt = Some(cur.saturating_sub(1)) }
                    else if i.consume_key(m, Key::PageDown) { tgt = Some((cur + page).min(n - 1)) }
                    else if i.consume_key(m, Key::PageUp) { tgt = Some(cur.saturating_sub(page)) }
                    else if i.consume_key(m, Key::Home) { tgt = Some(0) }
                    else if i.consume_key(m, Key::End) { tgt = Some(n - 1) }
                    if tgt.is_some() { ext = e; break }
                }
                (tgt, ext, i.consume_key(Modifiers::NONE, Key::Space), i.consume_key(Modifiers::NONE, Key::Enter), i.consume_key(Modifiers::SHIFT, Key::F10))
            });
            kb_click = space || enter;
            st.focus = Some(cur);
            if let Some(tgt) = target {
                if extend {
                    let a = *st.anchor.get_or_insert(cur);
                    sel_now = (a.min(tgt)..=a.max(tgt)).collect();
                }
                st.focus = Some(tgt);
                let top = tgt as f32 * ROW_H;
                if top < st.offset.y { st.offset.y = top } else if top + ROW_H > st.offset.y + body_h { st.offset.y = top + ROW_H - body_h }
            }
            let cur = st.focus.unwrap_or(cur);
            if space { if !sel_now.remove(&cur) { sel_now.insert(cur); } st.anchor = Some(cur) }
            if enter { out.opened = Some(cur) }
            if menu { out.menu = Some(cur) }
            clamp(&mut st, &ws);
        }
        let body_resp = ui.interact(body_vp, body_id, Sense::click());
        let over = ui.rect_contains_pointer(rect);
        if over {
            let d = ui.input(|i| i.smooth_scroll_delta);
            let (mx, my) = limits(&ws);
            let (dx, dy) = (if mx > 0.0 { d.x } else { 0.0 }, if my > 0.0 { d.y } else { 0.0 });
            if dx != 0.0 || dy != 0.0 {
                st.offset -= egui::vec2(dx, dy);
                ui.input_mut(|i| { if dx != 0.0 { i.smooth_scroll_delta.x = 0.0 } if dy != 0.0 { i.smooth_scroll_delta.y = 0.0 } });
            }
        }
        let pointer = ui.input(|i| i.pointer.hover_pos());
        let mods = ui.input(|i| i.modifiers);
        let row_at = |p: Pos2, off_y: f32| -> Option<usize> {
            let r = ((p.y - body_vp.min.y + off_y) / ROW_H).floor();
            (body_vp.contains(p) && r >= 0.0 && (r as usize) < n).then_some(r as usize)
        };
        if (body_resp.clicked() && !kb_click) || body_resp.secondary_clicked() {
            if let Some(r) = pointer.and_then(|p| row_at(p, st.offset.y)) {
                if body_resp.secondary_clicked() {
                    if !sel_now.contains(&r) { sel_now = BTreeSet::from([r]); st.anchor = Some(r) }
                    out.menu = Some(r);
                } else {
                    if mods.command { if !sel_now.remove(&r) { sel_now.insert(r); } st.anchor = Some(r) }
                    else if mods.shift { let a = st.anchor.unwrap_or(r); sel_now = (a.min(r)..=a.max(r)).collect() }
                    else { sel_now = BTreeSet::from([r]); st.anchor = Some(r) }
                    out.clicked = Some(r);
                }
                st.focus = Some(r);
            }
        }
        if body_resp.double_clicked() { out.opened = pointer.and_then(|p| row_at(p, st.offset.y)) }
        out.selection_changed = sel_now != before;
        if let Some(s) = self.selected.as_deref_mut() { *s = sel_now.clone() }

        // ---- scrollbars (they move the offset, so they run before the rows are painted) ----
        let (track, track_border) = if self.overlay { (t.bg_overlay, t.border_overlay) } else { (t.bg_panel, t.border_divider) };
        let (mx, my) = limits(&ws);
        if v {
            fill(ui.painter(), vbar, track);
            fill(ui.painter(), Rect::from_min_size(vbar.min, egui::vec2(1.0, vbar.height())), track_border);
            bar(ui, t, id.with("v"), vbar, true, body_h, rows_h, &mut st.offset.y);
        }
        if h {
            fill(ui.painter(), hbar, track);
            fill(ui.painter(), Rect::from_min_size(hbar.min, egui::vec2(hbar.width(), 1.0)), track_border);
            bar(ui, t, id.with("h"), hbar, false, vw, total_w, &mut st.offset.x);
        }
        if v && h { fill(ui.painter(), Rect::from_min_size(egui::pos2(vbar.min.x, hbar.min.y), egui::vec2(BAR, BAR)), track) }
        st.offset = egui::vec2(st.offset.x.clamp(0.0, mx), st.offset.y.clamp(0.0, my));
        let xs = if st.offset.x == xs[0] - rect.min.x { xs } else { xs_of(&ws, st.offset.x) };
        out.offset = st.offset;

        // ---- header ----
        let p = ui.painter().clone();
        fill(&p, Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H)), t.bg_raised);
        fill(&p, Rect::from_min_size(egui::pos2(rect.min.x, rect.min.y + HEADER_H - 1.0), egui::vec2(rect.width(), 1.0)), t.border_default);
        let hp = hdr.painter().clone();
        let kbf = keyboard_focus(ui.ctx());
        for (i, c) in cols.iter().enumerate() {
            let cell = Rect::from_min_size(egui::pos2(xs[i], rect.min.y), egui::vec2(ws[i], HEADER_H));
            out.header_rects.push(cell);
            if cell.max.x < header_vp.min.x || cell.min.x > header_vp.max.x { continue }
            let sorted_here = cur_sort.filter(|s| s.col == i);
            let color = if c.sortable && cells[i].hovered() { t.text_primary } else { t.text_secondary };
            // A title wider than its cell is clipped at the cell edge (never an ellipsis, as in the sheet).
            let cp = hp.with_clip_rect(sr(cell).intersect(sr(header_vp)));
            let g = line(&cp, c.title, ty::chip(), 14.0, f32::INFINITY);
            let text_rect = Rect::from_min_max(egui::pos2(cell.min.x + PAD, cell.min.y), egui::pos2(cell.max.x - PAD, cell.max.y - 1.0));
            paint_line(&cp, text_rect, g, 14.0, if c.numeric { Align::Max } else { Align::Min }, color, 0.0);
            if let Some(s) = sorted_here {
                let ic = sr(Rect::from_min_size(egui::pos2(cell.max.x - 22.0, cell.min.y + 6.0), egui::vec2(12.0, 12.0)));
                icons::paint(&hp, ic, if s.ascending { Icon::SortUp } else { Icon::SortDown }, IconSize::S12, color);
            }
            if i + 1 < n_cols && (edge_hot[i] || self.force.resize_hover == Some(i)) {
                fill(&hp, Rect::from_min_size(egui::pos2(cell.max.x - 1.0, cell.min.y), egui::vec2(1.0, HEADER_H - 1.0)), t.border_hover);
            }
            if cells[i].has_focus() && kbf {
                hp.rect_stroke(sr(cell), 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
            }
            ui.ctx().accesskit_node_builder(cells[i].id, |nb| {
                nb.set_role(Role::ColumnHeader);
                nb.set_label(c.title);
                nb.set_column_index(i);
                if let Some(s) = sorted_here {
                    nb.set_sort_direction(if s.ascending { accesskit::SortDirection::Ascending } else { accesskit::SortDirection::Descending });
                }
            });
        }

        // ---- rows ----
        let focus_row = self.force.focus_row.or(if has_focus && keyboard_focus(ui.ctx()) && n > 0 {
            Some(st.focus.unwrap_or_else(|| sel_now.iter().next().copied().unwrap_or(0)).min(n - 1))
        } else { None });
        let hover_row = self.force.hover_row.or(if over { pointer.and_then(|p| row_at(p, st.offset.y)) } else { None });
        out.focus_row = focus_row;
        out.hovered_row = hover_row;
        let mut body = ui.new_child(UiBuilder::new().max_rect(body_vp));
        body.set_clip_rect(sr(body_vp).intersect(ui.clip_rect()));
        let bp = body.painter().clone();
        let first = (st.offset.y / ROW_H).floor() as usize;
        let last = (((st.offset.y + body_h) / ROW_H).ceil() as usize).min(n);
        let mut row_fn = self.row_fn.take();
        let mut need = st.content.clone();
        let mut focus_name = String::new();
        for r in first..last {
            let y = body_vp.min.y + r as f32 * ROW_H - st.offset.y;
            let row = Rect::from_min_size(egui::pos2(rect.min.x - st.offset.x, y), egui::vec2(total_w, ROW_H));
            let vis = Rect::from_min_size(egui::pos2(body_vp.min.x, y), egui::vec2(vw, ROW_H));
            let (is_sel, is_hov) = (sel_now.contains(&r), hover_row == Some(r));
            if is_sel { fill(&bp, vis, t.accent_tint) } else if is_hov { fill(&bp, vis, t.bg_hover) }
            let mut ru = RowUi {
                ui: &mut body, t, id: id.with("cells"), idx: r, row, xs: &xs, ws: &ws, cols, need: &mut need, hovered: is_hov, selected: is_sel,
                disabled: self.disabled.map_or(false, |d| d(r)), focused: focus_row == Some(r), name: String::new(),
            };
            if let Some(f) = row_fn.as_mut() { f(&mut ru, r) }
            // The selection bar stays at the viewport edge (over the cells), so a scrolled-away first column still shows the selection.
            if is_sel { fill(&bp, Rect::from_min_size(vis.min, egui::vec2(2.0, ROW_H)), t.accent_bright) }
            if focus_row == Some(r) {
                focus_name = std::mem::take(&mut ru.name);
                bp.rect_stroke(sr(vis), 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
            }
            out.row_rects.push((r, row));
        }
        if need != st.content { st.content = need; ui.ctx().request_repaint() }

        // ---- accessibility: Table with header cells; the body is the one focus stop and names the focused row ----
        ui.ctx().accesskit_node_builder(ui.unique_id(), |nb| {
            nb.set_role(Role::Table);
            nb.set_label(self.label);
            nb.set_row_count(n + 1);
            nb.set_column_count(n_cols);
        });
        ui.ctx().accesskit_node_builder(body_resp.id, |nb| {
            nb.set_role(Role::Row);
            nb.set_label(focus_name.clone());
            if let Some(f) = focus_row {
                nb.set_row_index(f + 1);
                nb.set_selected(sel_now.contains(&f));
            }
        });

        out.widths = ws;
        if let (Some(s), Some(dst)) = (cur_sort, self.sort.as_deref_mut()) { *dst = Some(s) }
        ui.data_mut(|d| d.insert_temp(id, st));
        out
    }
}

/// One scrollbar: track input (click centres the thumb), thumb drag, and the thumb itself (§5.14: 6 wide, inset 1,
/// radius 3, min length 24).
#[allow(clippy::too_many_arguments)]
fn bar(ui: &mut Ui, t: &Tokens, id: Id, track: Rect, vertical: bool, view: f32, content: f32, off: &mut f32) {
    let max = (content - view).max(0.0);
    let len = if vertical { track.height() } else { track.width() };
    let thumb_len = (len * view / content.max(1.0)).clamp(24.0_f32.min(len), len);
    let free = (len - thumb_len).max(1.0);
    let tr = ui.interact(track, id.with("track"), Sense::click());
    if let (true, Some(p)) = (tr.clicked(), tr.interact_pointer_pos()) {
        let c = if vertical { p.y - track.min.y } else { p.x - track.min.x };
        *off = ((c - thumb_len / 2.0) / free * max).clamp(0.0, max);
    }
    let place = |off: f32| {
        let pos = off / max.max(1.0) * free;
        if vertical { Rect::from_min_size(egui::pos2(track.min.x + 1.0, track.min.y + pos), egui::vec2(6.0, thumb_len)) }
        else { Rect::from_min_size(egui::pos2(track.min.x + pos, track.min.y + 1.0), egui::vec2(thumb_len, 6.0)) }
    };
    let r = ui.interact(place(*off), id.with("thumb"), Sense::drag());
    if r.dragged() {
        let d = if vertical { r.drag_delta().y } else { r.drag_delta().x };
        *off = (*off + d / free * max).clamp(0.0, max);
    }
    let color = if r.dragged() { t.scroll_thumb_drag } else if r.hovered() { t.scroll_thumb_hover } else { t.scroll_thumb };
    ui.painter().rect_filled(sr(place(*off)), 3.0, color);
}

/// The painted 14 px checkbox of the sheet (the checkbox control proper is built elsewhere).
pub fn paint_box(p: &Painter, t: &Tokens, rect: Rect, checked: bool, enabled: bool, hover: bool) {
    let rect = sr(rect);
    let (fill_c, border, check) = if !enabled {
        (t.bg_panel, t.border_divider, t.text_disabled)
    } else if checked {
        let c = if hover { t.accent_hover } else { t.accent };
        (c, c, t.text_on_accent)
    } else {
        (t.bg_sunken, if hover { t.border_hover } else { t.border_control }, t.text_on_accent)
    };
    p.rect_filled(rect, 2.0, fill_c);
    p.rect_stroke(rect, 2.0, Stroke::new(1.0, border), StrokeKind::Inside);
    if checked {
        // §5.8: the 1.5 px check, hand-snapped to the pixel grid (the 12 px `check` icon is a 1 px glyph): (x, y) from the box corner.
        // (row, first pixel, last pixel): drawn as 1 px horizontal runs.
        const CHECK: [(f32, f32, f32); 10] = [
            (3.0, 10.0, 10.0), (4.0, 9.0, 11.0), (5.0, 8.0, 10.0), (6.0, 3.0, 3.0), (6.0, 7.0, 9.0), (7.0, 2.0, 4.0), (7.0, 6.0, 8.0),
            (8.0, 3.0, 7.0), (9.0, 4.0, 6.0), (10.0, 5.0, 5.0),
        ];
        for (y, a, b) in CHECK {
            let (l, r) = (rect.min + egui::vec2(a, y + 0.5), rect.min + egui::vec2(b + 1.0, y + 0.5));
            p.line_segment([l, r], Stroke::new(1.0, check));
        }
    }
}

/// One row being built: cell helpers over the table's column geometry. Text is elided to the cell (padded 8 h), the
/// widest natural cell width is fed back so the column can size to content.
pub struct RowUi<'a> {
    ui: &'a mut Ui,
    t: &'static Tokens,
    id: Id,
    idx: usize,
    row: Rect,
    xs: &'a [f32],
    ws: &'a [f32],
    cols: &'a [Column],
    need: &'a mut Vec<f32>,
    pub hovered: bool,
    pub selected: bool,
    pub disabled: bool,
    pub focused: bool,
    name: String,
}

impl RowUi<'_> {
    pub fn index(&self) -> usize { self.idx }
    /// Accessible name of the row (used for the focus stop and the cell controls).
    pub fn name(&mut self, s: &str) { self.name = s.to_owned() }
    /// Inline row actions show on hover, selection and keyboard focus (§8.4).
    pub fn show_actions(&self) -> bool { self.hovered || self.selected || self.focused }
    pub fn cell_rect(&self, col: usize) -> Rect { Rect::from_min_size(egui::pos2(self.xs[col], self.row.min.y), egui::vec2(self.ws[col], ROW_H)) }
    fn visible(&self, cell: Rect) -> bool { cell.max.x >= self.ui.clip_rect().min.x && cell.min.x <= self.ui.clip_rect().max.x }

    fn put(&mut self, col: usize, s: &str, font: FontId, lh: f32, color: Color32) {
        let cell = self.cell_rect(col);
        let p = self.ui.painter().clone();
        let nat = line(&p, s, font.clone(), lh, f32::INFINITY);
        self.need[col] = self.need[col].max(nat.size().x + 2.0 * PAD);
        if !self.visible(cell) { return }
        let room = (cell.width() - 2.0 * PAD).max(1.0);
        let g = if nat.size().x > room { line(&p, s, font, lh, room) } else { nat };
        let r = Rect::from_min_max(egui::pos2(cell.min.x + PAD, cell.min.y), egui::pos2(cell.max.x - PAD, cell.max.y));
        paint_line(&p, r, g, lh, if self.cols[col].numeric { Align::Max } else { Align::Min }, color, 0.0);
    }
    fn ink(&self, tertiary: bool) -> Color32 {
        if self.disabled { self.t.text_disabled } else if tertiary { self.t.text_tertiary } else { self.t.text_primary }
    }
    /// 12 primary text.
    pub fn text(&mut self, col: usize, s: &str) { let c = self.ink(false); self.put(col, s, ty::value(), 16.0, c) }
    /// 12 tertiary text (unknown / not reported).
    pub fn muted(&mut self, col: usize, s: &str) { let c = self.ink(true); self.put(col, s, ty::value(), 16.0, c) }
    /// Mono 11 (addresses, ids, numbers; right-aligned in numeric columns).
    pub fn mono(&mut self, col: usize, s: &str) { let c = self.ink(false); self.put(col, s, ty::mono(), 14.0, c) }
    /// The empty cell: "—" in tertiary.
    pub fn dash(&mut self, col: usize) { self.muted(col, "—") }

    /// 14 px checkbox slot; returns true when the user clicked it (the caller flips its value). A disabled box ignores input.
    pub fn checkbox(&mut self, col: usize, checked: bool, enabled: bool) -> bool {
        let cell = self.cell_rect(col);
        self.need[col] = self.need[col].max(14.0 + 2.0 * PAD);
        let b = Rect::from_min_size(egui::pos2(cell.min.x + PAD, cell.min.y + 5.0), egui::vec2(14.0, 14.0));
        if !self.visible(cell) { return false }
        let enabled = enabled && !self.disabled;
        let r = self.ui.interact(sr(b), self.id.with(("cb", self.idx, col)), if enabled { Sense::click() } else { Sense::hover() });
        paint_box(self.ui.painter(), self.t, b, checked, enabled, enabled && r.hovered());
        let name = format!("{}, {}", self.cols[col].title, self.name);
        r.widget_info(|| egui::WidgetInfo::selected(Role::CheckBox, enabled, checked, name.clone()));
        enabled && r.clicked()
    }
    /// Any button in the cell (22 high, e.g. Join), 1 px below the row top as in the sheet.
    pub fn button(&mut self, col: usize, btn: Btn<'_>) -> Response {
        let cell = self.cell_rect(col);
        let w = btn.natural_width(self.ui).max(0.0);
        self.need[col] = self.need[col].max(w + 2.0 * PAD);
        let pos = egui::pos2(cell.min.x + PAD, cell.min.y + 1.0);
        self.ui.add(btn.at(pos))
    }
    /// Inline icon action (18, icon 14) shown only while [`RowUi::show_actions`]; true when clicked.
    pub fn action(&mut self, col: usize, icon: Icon, tooltip: &str) -> bool {
        let cell = self.cell_rect(col);
        self.need[col] = self.need[col].max(18.0 + 2.0 * PAD);
        if !self.show_actions() || !self.visible(cell) { return false }
        let pos = egui::pos2(snap(cell.min.x + PAD), cell.min.y + 2.0);
        self.ui.add(icon_button(icon, IconButtonSize::Small18, tooltip).at(pos)).clicked()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, Event, PointerButton, RawInput};

    const COLS: [Column; 3] = [Column::new("Name", 40.0), Column::new("Kind", 70.0), Column::new("Rest", 60.0).flexible()];

    fn ctx() -> Context {
        let ctx = Context::default();
        crate::ui::fonts::install(&ctx);
        ctx
    }
    fn press(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE }
    }
    /// One frame drawing the same 5-row table (fixed outer size) with `events`.
    fn frame(ctx: &Context, id: &str, events: Vec<Event>, force: Force) -> TableOut {
        let mut res = None;
        let mut o = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
            res = Some(Table::new(id, &COLS).rows(5, |r, i| { r.text(0, &format!("row {i}")); r.mono(1, "192.168.1.2") })
                .size(egui::vec2(400.0, 100.0)).forced(force).show(ui));
        });
        o.textures_delta.clear();
        res.unwrap()
    }
    fn settle(ctx: &Context, id: &str) -> TableOut {
        let mut o = frame(ctx, id, vec![], Force::default());
        for _ in 0..2 { o = frame(ctx, id, vec![], Force::default()) }
        o
    }
    /// Press on the right edge of column `col`, move the pointer by `dx`, release.
    fn drag(ctx: &Context, id: &str, col: usize, dx: f32) -> TableOut {
        let o = settle(ctx, id);
        let e = egui::pos2(o.header_rects[col].max.x - 3.0, o.header_rects[col].center().y);
        frame(ctx, id, vec![Event::PointerMoved(e)], Force::default());
        frame(ctx, id, vec![press(e, true)], Force::default());
        frame(ctx, id, vec![Event::PointerMoved(e + egui::vec2(dx, 0.0))], Force::default());
        frame(ctx, id, vec![Event::PointerMoved(e + egui::vec2(dx, 0.0))], Force::default());
        frame(ctx, id, vec![press(e + egui::vec2(dx, 0.0), false)], Force::default());
        frame(ctx, id, vec![], Force::default())
    }

    #[test]
    fn table_resize_drag_min_and_double_click_reset() {
        let ctx = ctx();
        let w0 = settle(&ctx, "t1").widths.clone();
        // Dragging the first edge right widens that column only; the ones to its right keep their widths and shift.
        let o = drag(&ctx, "t1", 0, 30.0);
        assert!((o.widths[0] - (w0[0] + 30.0)).abs() < 0.6, "{:?} vs {:?}", o.widths, w0);
        assert!((o.widths[1] - w0[1]).abs() < 0.01);
        // Far left: stops at max(48, column min) = 48 for column 0, at the column min 70 for column 1.
        assert_eq!(drag(&ctx, "t1", 0, -500.0).widths[0], 48.0);
        assert_eq!(drag(&ctx, "t1", 1, -500.0).widths[1], 70.0);
        // Double click on the edge: back to content width.
        let o = settle(&ctx, "t1");
        let e = egui::pos2(o.header_rects[0].max.x - 3.0, o.header_rects[0].center().y);
        frame(&ctx, "t1", vec![Event::PointerMoved(e)], Force::default());
        for _ in 0..2 {
            frame(&ctx, "t1", vec![press(e, true)], Force::default());
            frame(&ctx, "t1", vec![press(e, false)], Force::default());
        }
        let o = settle(&ctx, "t1");
        assert!((o.widths[0] - w0[0]).abs() < 0.01, "reset to content: {:?} vs {:?}", o.widths, w0);
    }

    #[test]
    fn table_widths_remembered() {
        let ctx = ctx();
        let w0 = settle(&ctx, "ta").widths.clone();
        let dragged = drag(&ctx, "ta", 0, 25.0).widths[0];
        assert!(dragged > w0[0] + 20.0);
        // A later frame (a new `Table` value, same id) keeps it; another table is unaffected.
        assert_eq!(settle(&ctx, "ta").widths[0], dragged);
        assert_eq!(settle(&ctx, "tb").widths[0], w0[0]);
    }

    #[test]
    fn table_header_scrolls_with_body_horizontally_only() {
        let ctx = ctx();
        let cols = [Column::new("A", 200.0), Column::new("B", 200.0), Column::new("C", 200.0)];
        let run = |force: Force| {
            let mut res = None;
            let mut o = ctx.run_ui(RawInput::default(), |ui| {
                res = Some(Table::new("ts", &cols).rows(20, |r, i| r.text(0, &format!("r{i}"))).size(egui::vec2(300.0, 120.0)).forced(force).show(ui));
            });
            o.textures_delta.clear();
            res.unwrap()
        };
        run(Force::default());
        let a = run(Force::default());
        let b = run(Force { scroll: Some(egui::vec2(100.0, 48.0)), ..Default::default() });
        assert_eq!(b.offset, egui::vec2(100.0, 48.0));
        // Header: moves left with the body, never vertically.
        assert_eq!(b.header_rects[0].min.x, a.header_rects[0].min.x - 100.0);
        assert_eq!(b.header_rects[0].min.y, a.header_rects[0].min.y);
        // Body row 2 (the first fully visible one at offset 48): same x shift, 48 px up.
        let row = |o: &TableOut| o.row_rects.iter().find(|r| r.0 == 2).unwrap().1;
        assert_eq!(row(&b).min.x, row(&a).min.x - 100.0);
        assert_eq!(row(&b).min.y, row(&a).min.y - 48.0);
        // The viewport is clamped: 600 wide content in a 300 − 8 wide view scrolls at most 308.
        let c = run(Force { scroll: Some(egui::vec2(9999.0, 0.0)), ..Default::default() });
        assert_eq!(c.offset.x, 600.0 - (300.0 - BAR));
    }
}
