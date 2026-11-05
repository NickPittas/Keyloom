//! Dropdown / combo box (bible §5.5, §5.10 list rules, §10.2, §12): a fixed 22 px box painted by hand plus an open list
//! (an `egui::Area` painted with the same tokens). One Tab stop; Space / Enter / Alt+↓ open, ↑/↓/Home/End move, type-ahead
//! jumps, Enter picks, Esc closes unchanged. AccessKit: ComboBox (value = current item), list rows are ListBoxOptions.
use super::{keyboard_focus, line, paint_line, Vis, LABEL_DX};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens, DARK};
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, accesskit::Role, epaint::Shadow, Align, Area, Color32, CornerRadius, EventFilter, Id, IdSalt, Key, Modifiers, Order, Painter, Pos2, Rect, Response,
    ScrollArea, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo,
};

const ROW: f32 = 22.0;
/// Rows visible before the list scrolls (§5.5).
const MAX_ROWS: usize = 12;
/// Gap between anchor and list.
const GAP: f32 = 2.0;
/// Clearance kept to the window edge (§5.10 list rules).
const EDGE: f32 = 8.0;

/// One list entry. `icon` / `dot` also show in the closed box when the item is selected; `shortcut` is right-aligned mono
/// 11; `note` is a right-aligned 11 hint (e.g. "online"); `disabled` carries the reason (short suffix + tooltip).
#[derive(Clone, Copy, Default)]
pub struct Item<'a> {
    pub label: &'a str,
    pub icon: Option<Icon>,
    pub dot: Option<Color32>,
    pub shortcut: Option<&'a str>,
    pub note: Option<&'a str>,
    pub disabled: Option<&'a str>,
}

impl<'a> Item<'a> {
    pub fn new(label: &'a str) -> Self { Item { label, ..Default::default() } }
    pub fn icon(mut self, icon: Icon) -> Self { self.icon = Some(icon); self }
    pub fn dot(mut self, c: Color32) -> Self { self.dot = Some(c); self }
    pub fn shortcut(mut self, s: &'a str) -> Self { self.shortcut = Some(s); self }
    pub fn note(mut self, s: &'a str) -> Self { self.note = Some(s); self }
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
}

/// Font of the closed box's value (lists always use the label font).
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub enum ValueFont { #[default] Value, Small, Mono }

/// Paints a 14 px swatch for item `i`; `dim` = the dropdown is disabled.
pub type SwatchFn<'a> = &'a dyn Fn(&Painter, Rect, usize, bool);

#[derive(Clone, Default)]
struct St {
    open: bool,
    hl: usize,
    ta: String,
    ta_t: f64,
    list: Option<Rect>,
    above: bool,
}

/// `dropdown(ui, id, &mut selected, &items, width)`: `selected >= items.len()` means "nothing" (use `.placeholder`).
/// Returns the box's response; `changed()` when the user picked another entry.
pub fn dropdown(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, selected: &mut usize, items: &[Item<'_>], width: f32) -> Response {
    Dropdown::new(id, selected, items, width).show(ui)
}

pub struct Dropdown<'a> {
    salt: IdSalt,
    selected: &'a mut usize,
    items: &'a [Item<'a>],
    width: f32,
    name: Option<&'a str>,
    disabled: Option<&'a str>,
    placeholder: Option<&'a str>,
    value_color: Option<Color32>,
    icon: Option<Icon>,
    dot: Option<Color32>,
    at: Option<Pos2>,
    list_width: Option<f32>,
    list_offset: Vec2,
    force: Option<(Vis, bool, bool)>,
    highlight: Option<usize>,
    compact: bool,
    display: Option<&'a str>,
    font: ValueFont,
    tip: Option<&'a str>,
    swatch: Option<SwatchFn<'a>>,
    repick: bool,
}

impl<'a> Dropdown<'a> {
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, selected: &'a mut usize, items: &'a [Item<'a>], width: f32) -> Self {
        Dropdown {
            salt: IdSalt::new(id), selected, items, width, name: None, disabled: None, placeholder: None, value_color: None, icon: None, dot: None,
            at: None, list_width: None, list_offset: Vec2::new(0.0, GAP), force: None, highlight: None,
            compact: false, display: None, font: ValueFont::Value, tip: None, swatch: None, repick: false,
        }
    }
    /// Picking the already selected entry also counts as `changed()` (actions, not values).
    pub fn repick(mut self) -> Self { self.repick = true; self }
    /// Narrow form (viewer toolbar, §7.2): no 96 px minimum width, text 7 px in, chevron 17 px from the right.
    pub fn compact(mut self) -> Self { self.compact = true; self }
    /// Closed-box text instead of the selected item's label (it is then never the placeholder look).
    pub fn display(mut self, t: &'a str) -> Self { self.display = Some(t); self }
    /// Font of the closed-box value (default: the value font).
    pub fn font(mut self, f: ValueFont) -> Self { self.font = f; self }
    /// Hover text while the list is closed and the box is enabled (also the AccessKit description).
    pub fn tip(mut self, t: &'a str) -> Self { self.tip = Some(t); self }
    /// A swatch box: the closed box shows only the selected item's swatch and the chevron; list rows lead with a swatch.
    /// Implies the compact padding.
    pub fn swatch(mut self, f: SwatchFn<'a>) -> Self { self.swatch = Some(f); self.compact = true; self }
    /// Accessible name (the visible property label).
    pub fn name(mut self, n: &'a str) -> Self { self.name = Some(n); self }
    /// Disabled with the reason as tooltip and AccessKit description.
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    pub fn disabled_if(self, cond: bool, reason: &'a str) -> Self { if cond { self.disabled(reason) } else { self } }
    /// Text in `text.tertiary` while nothing is selected ("Assign…").
    pub fn placeholder(mut self, t: &'a str) -> Self { self.placeholder = Some(t); self }
    /// Value colour override (`text.link` for a non-default value).
    pub fn value_color(mut self, c: Color32) -> Self { self.value_color = Some(c); self }
    /// Leading 14 px icon (default: the selected item's icon).
    pub fn icon(mut self, i: Icon) -> Self { self.icon = Some(i); self }
    /// Trailing 6 px status dot before the chevron (default: the selected item's dot).
    pub fn dot(mut self, c: Color32) -> Self { self.dot = Some(c); self }
    /// Absolute position instead of the layout flow (specimen pages).
    pub fn at(mut self, p: Pos2) -> Self { self.at = Some(p); self }
    /// List width (default: the box width) and offset of its top-left from the box's bottom-left (default 0, 2).
    pub fn list_width(mut self, w: f32) -> Self { self.list_width = Some(w); self }
    pub fn list_offset(mut self, o: Vec2) -> Self { self.list_offset = o; self }
    /// Specimen / test only: pointer look, list open, keyboard focus ring; drawn through the same paint path.
    pub fn forced(mut self, vis: Vis, open: bool, keyboard_focus: bool) -> Self { self.force = Some((vis, open, keyboard_focus)); self }
    /// Specimen only: the highlighted row of a forced-open list.
    pub fn highlight(mut self, i: usize) -> Self { self.highlight = Some(i); self }
}

/// Where the open list goes (§5.10): below the anchor, flipped above when that leaves more room; height = min(content,
/// 12 rows, space to the window edge − 8). Returns the rect and whether it is above.
pub fn list_placement(anchor: Rect, screen: Rect, size: Vec2, dx: f32, gap: f32) -> (Rect, bool) {
    let below = (screen.max.y - EDGE - (anchor.max.y + gap)).max(0.0);
    let above = (anchor.min.y - gap - EDGE - screen.min.y).max(0.0);
    let x = (anchor.min.x + dx).min(screen.max.x - EDGE - size.x).max(screen.min.x + EDGE);
    if size.y <= below || below >= above {
        (Rect::from_min_size(Pos2::new(x, anchor.max.y + gap), Vec2::new(size.x, size.y.min(below))), false)
    } else {
        let h = size.y.min(above);
        (Rect::from_min_size(Pos2::new(x, anchor.min.y - gap - h), Vec2::new(size.x, h)), true)
    }
}

/// Next enabled index from `from` in direction `dir` (no wrap); stays when there is none.
fn step(enabled: &[bool], from: usize, dir: i32) -> usize {
    let mut i = from as i32 + dir;
    while i >= 0 && (i as usize) < enabled.len() {
        if enabled[i as usize] { return i as usize }
        i += dir;
    }
    from
}

fn edge(enabled: &[bool], last: bool) -> usize {
    let mut it = (0..enabled.len()).filter(|&i| enabled[i]);
    (if last { it.next_back() } else { it.next() }).unwrap_or(0)
}

/// Type-ahead: a repeated single letter cycles, a longer prefix narrows. Wraps; skips disabled items.
fn type_ahead(items: &[Item<'_>], enabled: &[bool], hl: usize, buf: &str) -> Option<usize> {
    let n = items.len();
    let start = if buf.chars().count() == 1 { hl + 1 } else { hl };
    (0..n).map(|k| (start + k) % n.max(1)).find(|&i| enabled[i] && items[i].label.to_lowercase().starts_with(buf))
}

fn snap(r: Rect) -> Rect {
    Rect::from_min_max(Pos2::new(r.min.x.round(), r.min.y.round()), Pos2::new(r.max.x.round(), r.max.y.round()))
}

impl<'a> Dropdown<'a> {
    pub fn show(self, ui: &mut Ui) -> Response {
        let t = tokens(ui);
        let ctx = ui.ctx().clone();
        let items = self.items;
        let n = items.len();
        let en = ui.is_enabled() && self.disabled.is_none();
        let size = Vec2::new(if self.compact { self.width } else { self.width.max(96.0) }, 22.0);
        let rect0 = match self.at {
            Some(p) => Rect::from_min_size(p, size),
            None => ui.allocate_exact_size(size, Sense::hover()).0,
        };
        let rect = snap(rect0);
        let id = ui.make_persistent_id(self.salt);
        let mut resp = ui.interact(rect, id, if en { Sense::click() } else { Sense::hover() });
        let enabled: Vec<bool> = items.iter().map(|it| en && it.disabled.is_none()).collect();
        let first_hl = |sel: usize| if sel < n && enabled[sel] { sel } else { edge(&enabled, false) };

        let mut st: St = if self.force.is_some() { St::default() } else { ctx.data(|d| d.get_temp(id)).unwrap_or_default() };
        let mut picked: Option<usize> = None;
        let mut moved = false;
        if self.force.is_none() {
            if !en {
                st.open = false;
            } else {
                let mut opened_now = false;
                // Enter / Space on the focused box arrive as a click: while the list is open they pick (handled below).
                let kb_activate = ctx.input(|i| i.key_pressed(Key::Enter) || i.key_pressed(Key::Space));
                if resp.clicked() && !(st.open && kb_activate) {
                    st.open = !st.open;
                    opened_now = st.open;
                } else if !st.open && resp.has_focus() && ctx.input_mut(|i| i.consume_key(Modifiers::ALT, Key::ArrowDown)) {
                    st.open = true;
                    opened_now = true;
                }
                if opened_now {
                    st.hl = first_hl(*self.selected);
                    st.ta.clear();
                    moved = true;
                }
                if st.open {
                    ctx.memory_mut(|m| m.set_focus_lock_filter(id, EventFilter { vertical_arrows: true, escape: true, ..Default::default() }));
                }
                if st.open && !opened_now {
                    let key = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));
                    if key(Key::Escape) {
                        st.open = false;
                    } else {
                        if key(Key::ArrowDown) { st.hl = step(&enabled, st.hl, 1); moved = true }
                        if key(Key::ArrowUp) { st.hl = step(&enabled, st.hl, -1); moved = true }
                        if key(Key::Home) { st.hl = edge(&enabled, false); moved = true }
                        if key(Key::End) { st.hl = edge(&enabled, true); moved = true }
                        let typed: String = ctx.input(|i| {
                            i.events.iter().filter_map(|e| if let egui::Event::Text(s) = e { Some(s.as_str()) } else { None }).collect()
                        });
                        let typed: String = typed.chars().filter(|c| !c.is_whitespace() && !c.is_control()).collect();
                        if !typed.is_empty() {
                            let now = ctx.input(|i| i.time);
                            if now - st.ta_t > 1.0 { st.ta.clear() }
                            st.ta_t = now;
                            st.ta.push_str(&typed.to_lowercase());
                            if let Some(i) = type_ahead(items, &enabled, st.hl, &st.ta) { st.hl = i; moved = true }
                        }
                        if key(Key::Enter) || (st.ta.is_empty() && key(Key::Space)) {
                            if st.hl < n && enabled[st.hl] { picked = Some(st.hl) }
                        }
                        if ctx.input(|i| i.key_pressed(Key::Tab)) { st.open = false }
                    }
                }
            }
        }

        // ---- the box ------------------------------------------------------------------------------------------------
        let list_open = match self.force { Some((_, o, _)) => o, None => st.open };
        let (vis, kb) = match self.force {
            Some((v, _, k)) => (v, k),
            None => (
                if !en { Vis::Rest } else if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest },
                en && resp.has_focus() && keyboard_focus(&ctx),
            ),
        };
        let sel_item = items.get(*self.selected);
        let value = self.display.or(sel_item.map(|i| i.label)).or(self.placeholder).unwrap_or("");
        let is_ph = sel_item.is_none() && self.display.is_none();
        let name = self.name.unwrap_or(value);
        let mut elided = false;
        if ui.is_rect_visible(rect) {
            let p = ui.painter();
            let radius = CornerRadius::same(3);
            let (fill, border) = if !en {
                (t.bg_panel, t.border_divider)
            } else if list_open {
                (t.bg_sunken, t.accent_bright)
            } else if vis != Vis::Rest {
                (t.bg_sunken, t.border_hover)
            } else {
                (t.bg_sunken, t.border_control)
            };
            p.rect_filled(rect, radius, fill);
            if kb && en && !list_open {
                p.rect_stroke(rect, radius, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
            } else {
                p.rect_stroke(rect, radius, Stroke::new(1.0, border), StrokeKind::Inside);
            }
            let chrome = if en { t.text_secondary } else { t.text_disabled };
            let chev_x = match (&self.swatch, self.compact) {
                (Some(_), _) => rect.min.x + 20.0,
                (None, true) => rect.max.x - 17.0,
                (None, false) => (rect0.max.x - 7.0 - 12.0).round(),
            };
            let chev = Rect::from_min_size(Pos2::new(chev_x, rect.min.y + 5.0), Vec2::splat(12.0));
            icons::paint(p, chev, Icon::ChevronDown, IconSize::S12, chrome);
            if let Some(sw) = self.swatch {
                sw(p, Rect::from_min_size(rect.min + Vec2::splat(4.0), Vec2::splat(14.0)), *self.selected, !en);
            }
            let mut x0 = rect0.min.x + if self.compact { 7.0 } else { 9.0 };
            if self.swatch.is_some() {
                // swatch box: nothing but the swatch and the chevron
            } else if let Some(ic) = self.icon.or(sel_item.and_then(|i| i.icon)) {
                let r = Rect::from_min_size(Pos2::new((rect0.min.x + 9.0).round(), rect.min.y + 4.0), Vec2::splat(14.0));
                icons::paint(p, r, ic, IconSize::S14, chrome);
                x0 += 14.0 + 6.0;
            }
            let mut right = chev.min.x - if self.compact { 2.0 } else { 6.0 };
            if self.swatch.is_none() {
                if let Some(c) = self.dot.or(sel_item.and_then(|i| i.dot)) {
                    p.circle_filled(Pos2::new(chev.min.x - 6.0 - 3.0, rect.min.y + 11.0), 3.0, c);
                    right -= 6.0 + 6.0;
                }
                let color = if !en { t.text_disabled } else if is_ph { t.text_tertiary } else { self.value_color.unwrap_or(t.text_primary) };
                let (font, lh) = match self.font { ValueFont::Value => (ty::value(), 16.0), ValueFont::Small => (ty::small(), 14.0), ValueFont::Mono => (ty::mono(), 14.0) };
                let g = line(p, value, font, lh, (right - x0).max(1.0));
                elided = g.elided;
                // paint_line floors the x of a truncated label: start it on the rounded pixel instead of the biased one.
                let dx = if g.elided { x0.round() - x0 } else if self.font == ValueFont::Value { LABEL_DX } else { 0.0 };
                paint_line(p, Rect::from_min_max(Pos2::new(x0 + dx, rect.min.y), Pos2::new(right, rect.max.y)), g, lh, Align::Min, color, 0.0);
            }
        }

        // ---- AccessKit + tooltips -----------------------------------------------------------------------------------
        resp.widget_info(|| WidgetInfo { current_text_value: Some(value.to_string()), ..WidgetInfo::labeled(Role::ComboBox, en, name) });
        ctx.accesskit_node_builder(id, |nb| {
            nb.set_expanded(list_open);
            nb.set_value(value);
            if let Some(r) = self.disabled.or(self.tip) { nb.set_description(r) }
        });
        if let Some(r) = self.disabled {
            resp = resp.on_hover_text(r);
        } else if !list_open {
            if let Some(tip) = self.tip {
                resp = resp.on_hover_text(tip);
            } else if elided {
                resp = resp.on_hover_text(value);
            }
        }

        // ---- the list -----------------------------------------------------------------------------------------------
        if list_open {
            let mut hl = self.highlight.unwrap_or(st.hl);
            let interactive = self.force.is_none();
            let (click_pick, lrect, above) = self.show_list(&ctx, t, id, rect, &enabled, &mut hl, moved, interactive);
            st.list = Some(lrect);
            st.above = above;
            if interactive {
                st.hl = hl;
                if click_pick.is_some() { picked = click_pick }
                let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !lrect.contains(p) && !rect.contains(p)));
                if outside { st.open = false }
            }
        } else {
            st.list = None;
        }
        if let Some(i) = picked {
            if i != *self.selected || self.repick { *self.selected = i; resp.mark_changed(); }
            st.open = false;
        }
        if self.force.is_none() { ctx.data_mut(|d| d.insert_temp(id, st)); }
        resp
    }

    #[allow(clippy::too_many_arguments)]
    fn show_list(&self, ctx: &egui::Context, t: &Tokens, id: Id, anchor: Rect, enabled: &[bool], hl: &mut usize, scroll_to_hl: bool, interactive: bool) -> (Option<usize>, Rect, bool) {
        let items = self.items;
        let n = items.len();
        let lw = self.list_width.unwrap_or(anchor.width());
        let full_h = n.min(MAX_ROWS) as f32 * ROW + 8.0 + 2.0;
        let (lr, above) = list_placement(anchor, ctx.content_rect(), Vec2::new(lw, full_h), self.list_offset.x, self.list_offset.y);
        let shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if t.bg_panel == DARK.bg_panel { 115 } else { 46 }) };
        let mut picked = None;
        Area::new(id.with("dd-list")).order(Order::Foreground).fixed_pos(lr.min).constrain(false).show(ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(lr.size(), Sense::hover());
            let p = ui.painter().clone();
            p.add(shadow.as_shape(r, CornerRadius::same(4)));
            p.rect_filled(r, CornerRadius::same(4), t.bg_overlay);
            let inner = r.shrink(1.0);
            ui.scope_builder(UiBuilder::new().max_rect(inner), |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                ScrollArea::vertical().id_salt("dd-scroll").max_height(inner.height()).auto_shrink([false, false]).show(ui, |ui| {
                    ui.add_space(4.0);
                    for (i, it) in items.iter().enumerate() {
                        let en = enabled[i];
                        let (row, rr) = ui.allocate_exact_size(Vec2::new(ui.available_width(), ROW), if en && interactive { Sense::click() } else { Sense::hover() });
                        if en && interactive && rr.hovered() && ui.input(|i| i.pointer.delta() != Vec2::ZERO) { *hl = i }
                        if en && interactive && rr.clicked() { picked = Some(i) }
                        if scroll_to_hl && i == *hl { rr.scroll_to_me(None) }
                        let is_sel = i == *self.selected;
                        rr.widget_info(|| WidgetInfo::labeled(Role::ListBoxOption, en, it.label));
                        ui.ctx().accesskit_node_builder(rr.id, |nb| {
                            nb.set_selected(is_sel);
                            if let Some(d) = it.disabled { nb.set_description(d) }
                        });
                        if let Some(d) = it.disabled { let _ = rr.on_hover_text(d); }
                        if !ui.is_rect_visible(row) { continue }
                        let rw = row.shrink2(Vec2::new(4.0, 0.0));
                        let hot = en && *hl == i;
                        let p = ui.painter();
                        if hot { p.rect_filled(rw, CornerRadius::same(3), t.bg_menu_hover); }
                        let text = if !en { t.text_disabled } else { t.text_primary };
                        let sub = if hot { t.text_primary } else { t.text_tertiary };
                        if is_sel {
                            let r16 = Rect::from_min_size(Pos2::new((rw.min.x + 8.0).round(), rw.center().y.round() - 8.0), Vec2::splat(16.0));
                            icons::paint(p, r16, Icon::Check, IconSize::S16, if en { t.text_link } else { t.text_disabled });
                        }
                        let mut lx = rw.min.x + 8.0 + 16.0 + 8.0;
                        if let Some(sw) = self.swatch {
                            sw(p, Rect::from_min_size(Pos2::new(lx.round(), rw.center().y.round() - 7.0), Vec2::splat(14.0)), i, !en);
                            lx += 14.0 + 6.0;
                        } else if let Some(ic) = it.icon {
                            let r14 = Rect::from_min_size(Pos2::new(lx.round(), rw.center().y.round() - 7.0), Vec2::splat(14.0));
                            icons::paint(p, r14, ic, IconSize::S14, if en { t.text_secondary } else { t.text_disabled });
                            lx += 14.0 + 6.0;
                        }
                        let mut rx = rw.max.x - 8.0;
                        if let Some(c) = it.dot {
                            p.circle_filled(Pos2::new(rx - 3.0, rw.center().y), 3.0, c);
                            rx -= 6.0 + 8.0;
                        }
                        let reason = it.disabled.filter(|d| d.chars().count() <= 16);
                        for (s, font, lh, col) in [
                            (it.shortcut, ty::mono(), 14.0, sub),
                            (it.note.or(reason), ty::small(), 14.0, if en { sub } else { t.text_tertiary }),
                        ] {
                            if let Some(s) = s {
                                let g = line(p, s, font, lh, f32::INFINITY);
                                let w = g.size().x;
                                paint_line(p, Rect::from_min_max(Pos2::new(rx - w, rw.min.y), Pos2::new(rx, rw.max.y)), g, lh, Align::Max, col, 0.0);
                                rx -= w + 8.0;
                            }
                        }
                        let g = line(p, it.label, ty::label(), 16.0, (rx - lx).max(1.0));
                        let dx = if g.elided { lx.round() - lx } else { LABEL_DX };
                        paint_line(p, Rect::from_min_max(Pos2::new(lx + dx, rw.min.y), Pos2::new(rx, rw.max.y)), g, 16.0, Align::Min, text, 0.0);
                    }
                    ui.add_space(4.0);
                });
            });
            p.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border_overlay), StrokeKind::Inside);
        });
        (picked, lr, above)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, Event, RawInput};

    fn ctx() -> Context {
        let ctx = Context::default();
        crate::ui::fonts::install(&ctx);
        ctx
    }

    fn key(k: Key, m: Modifiers) -> Event {
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }
    }

    fn items() -> Vec<Item<'static>> {
        vec![Item::new("Input"), Item::new("Hint"), Item::new("Mask").disabled("No mask"), Item::new("FG"), Item::new("Matte")]
    }

    /// One frame at 400×300 with the box at `at`; returns the widget's stored state.
    fn frame(ctx: &Context, events: Vec<Event>, _modifiers: Modifiers, sel: &mut usize, at: Pos2) -> St {
        let its = items();
        let mut wid = Id::NULL;
        let mut out = ctx.run_ui(
            RawInput { events, screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0))), ..Default::default() },
            |ui| {
                let r = Dropdown::new("dd", sel, &its, 120.0).at(at).show(ui);
                wid = r.id;
                if !r.has_focus() { ui.memory_mut(|m| m.request_focus(r.id)) }
            },
        );
        out.textures_delta.clear();
        ctx.data(|d| d.get_temp::<St>(wid)).unwrap_or_default()
    }

    /// Alt+↓ opens, ↓ moves (skipping the disabled "Mask"), Enter picks; a second round with Esc leaves the value alone.
    #[test]
    fn dropdown_keys_open_move_pick_escape() {
        let ctx = ctx();
        let mut sel = 0usize;
        let at = Pos2::new(10.0, 10.0);
        frame(&ctx, vec![], Modifiers::NONE, &mut sel, at); // registers the box, takes focus
        frame(&ctx, vec![], Modifiers::NONE, &mut sel, at);
        let s = frame(&ctx, vec![key(Key::ArrowDown, Modifiers::ALT)], Modifiers::ALT, &mut sel, at);
        assert!(s.open, "Alt+Down opens");
        assert_eq!(s.hl, 0, "highlight starts at the selected item");
        frame(&ctx, vec![key(Key::ArrowDown, Modifiers::NONE)], Modifiers::NONE, &mut sel, at);
        let s = frame(&ctx, vec![key(Key::ArrowDown, Modifiers::NONE)], Modifiers::NONE, &mut sel, at);
        assert_eq!(s.hl, 3, "Down moves, skipping the disabled item");
        assert_eq!(sel, 0, "moving does not pick");
        let s = frame(&ctx, vec![key(Key::Enter, Modifiers::NONE)], Modifiers::NONE, &mut sel, at);
        assert_eq!(sel, 3, "Enter picks");
        assert!(!s.open, "pick closes");
        // Esc: open again, move, Esc → unchanged and closed.
        assert!(frame(&ctx, vec![key(Key::ArrowDown, Modifiers::ALT)], Modifiers::ALT, &mut sel, at).open);
        assert_eq!(frame(&ctx, vec![key(Key::ArrowDown, Modifiers::NONE)], Modifiers::NONE, &mut sel, at).hl, 4);
        let s = frame(&ctx, vec![key(Key::Escape, Modifiers::NONE)], Modifiers::NONE, &mut sel, at);
        assert_eq!(sel, 3, "Esc leaves the value unchanged");
        assert!(!s.open, "Esc closes");
    }

    /// Near the window bottom the list flips above its box; near the top it opens below; either way it stays 8 px inside.
    #[test]
    fn dropdown_list_flips_above_when_no_room() {
        let screen = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 300.0));
        let size = Vec2::new(120.0, 7.0 * ROW + 10.0);
        let low = Rect::from_min_size(Pos2::new(10.0, 270.0), Vec2::new(120.0, 22.0));
        let (r, above) = list_placement(low, screen, size, 0.0, GAP);
        assert!(above && (r.max.y - (low.min.y - GAP)).abs() < 0.01 && r.height() == size.y, "flipped above, full height: {r:?}");
        let high = Rect::from_min_size(Pos2::new(10.0, 10.0), Vec2::new(120.0, 22.0));
        let (r, above) = list_placement(high, screen, size, 0.0, GAP);
        assert!(!above && (r.min.y - (high.max.y + GAP)).abs() < 0.01, "below when there is room");
        // Neither side fits: the larger side wins and the list is clamped to it (then it scrolls).
        let tiny = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 100.0));
        let mid = Rect::from_min_size(Pos2::new(10.0, 60.0), Vec2::new(120.0, 22.0));
        let (r, above) = list_placement(mid, tiny, size, 0.0, GAP);
        assert!(above && r.min.y >= 0.0 && r.height() <= 60.0 - GAP - EDGE + 0.01, "clamped above: {r:?}");
        // The real widget does the same through a frame.
        let ctx = ctx();
        let mut sel = 0usize;
        frame(&ctx, vec![], Modifiers::NONE, &mut sel, Pos2::new(10.0, 270.0));
        frame(&ctx, vec![], Modifiers::NONE, &mut sel, Pos2::new(10.0, 270.0));
        let s = frame(&ctx, vec![key(Key::ArrowDown, Modifiers::ALT)], Modifiers::ALT, &mut sel, Pos2::new(10.0, 270.0));
        assert!(s.open && s.above && s.list.is_some_and(|l| l.max.y <= 270.0), "open list sits above the box: {:?}", s.list);
    }
}
