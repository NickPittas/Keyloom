//! Menu bar, menu / context menu and tooltip (bible §5.9–§5.11, §10.2, §11, §12). Painted by hand with tokens only:
//! the bar is 24 high, a menu is an overlay card of 22 px rows, a context menu adds a 20 px target header, the tooltip
//! is the one home of "why" text. Menus are driven by one small key model (`Nav`): Alt / F10 focus the bar, ↑/↓ move,
//! → opens a submenu (or the next menu), ← closes it, Enter activates, Esc closes. Forced looks (`forced`, `highlight`,
//! `pressed`) go through the same paint path as the real states (specimen).
use super::{keyboard_focus, line, paint_line, Vis, LABEL_DX};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, accesskit::{Role, Toggled}, text::LayoutJob, vec2, Align, Area, Color32, Context, CornerRadius, FontId, Id, Key, Modifiers, Order,
    Painter, Pos2, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo,
};

/// "No row highlighted" inside a `Nav` path (a submenu opened by the pointer, nothing chosen in it yet).
const NONE: usize = usize::MAX;
const ROW: f32 = 22.0;
const HEAD: f32 = 20.0;
const SEP: f32 = 9.0;

// ---------------------------------------------------------------------------------------------------------------
// Data
// ---------------------------------------------------------------------------------------------------------------

/// One menu row (or a separator). `disabled` carries the reason (`Some("")` = disabled without a reason).
#[derive(Clone, Copy)]
pub struct MenuItem<'a> {
    pub label: &'a str,
    pub shortcut: Option<&'a str>,
    pub icon: Option<Icon>,
    pub checked: bool,
    pub disabled: Option<&'a str>,
    pub submenu: Option<&'a [MenuItem<'a>]>,
    pub separator: bool,
    pub destructive: bool,
}

impl<'a> MenuItem<'a> {
    pub const fn new(label: &'a str) -> Self {
        MenuItem { label, shortcut: None, icon: None, checked: false, disabled: None, submenu: None, separator: false, destructive: false }
    }
    pub const fn sep() -> Self { MenuItem { separator: true, ..MenuItem::new("") } }
    pub const fn shortcut(mut self, s: &'a str) -> Self { self.shortcut = Some(s); self }
    pub const fn icon(mut self, i: Icon) -> Self { self.icon = Some(i); self }
    pub const fn checked(mut self, on: bool) -> Self { self.checked = on; self }
    pub const fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    pub const fn submenu(mut self, items: &'a [MenuItem<'a>]) -> Self { self.submenu = Some(items); self }
    pub const fn destructive(mut self) -> Self { self.destructive = true; self }
    fn selectable(&self) -> bool { !self.separator && self.disabled.is_none() }
    fn height(&self) -> f32 { if self.separator { SEP } else { ROW } }
}

/// A bar menu: its name and its rows.
#[derive(Clone, Copy)]
pub struct Menu<'a> {
    pub label: &'a str,
    pub items: &'a [MenuItem<'a>],
}

/// The bar's right end: project name and save state.
#[derive(Clone, Copy)]
pub struct MenuBarRight<'a> {
    pub project: &'a str,
    pub unsaved: bool,
}

pub struct MenuBarOut {
    pub response: Response,
    /// The activated row as a path: menu index, then item index per submenu level.
    pub clicked: Option<Vec<usize>>,
    pub open: bool,
}

pub struct ListOut {
    pub rect: Rect,
    pub clicked: Option<usize>,
    pub hovered: Option<usize>,
    /// Row rect per item (separators too).
    pub rows: Vec<Rect>,
}

// ---------------------------------------------------------------------------------------------------------------
// Text helpers
// ---------------------------------------------------------------------------------------------------------------

fn txt(p: &Painter, rect: Rect, s: &str, font: FontId, lh: f32, align: Align, col: Color32) -> f32 {
    let g = line(p, s, font, lh, rect.width().max(1.0));
    let w = g.size().x;
    paint_line(p, rect, g, lh, align, col, 0.0);
    w
}

fn width_of(p: &Painter, s: &str, font: FontId, lh: f32) -> f32 { line(p, s, font, lh, f32::INFINITY).size().x }

fn snap(v: f32) -> f32 { (v + 0.5).floor() }

// ---------------------------------------------------------------------------------------------------------------
// Menu list (§5.10)
// ---------------------------------------------------------------------------------------------------------------

/// A menu card. `highlight` is the keyboard / forced-hover row, `pressed` the forced-pressed row.
pub struct MenuList<'a> {
    items: &'a [MenuItem<'a>],
    header: Option<&'a str>,
    width: Option<f32>,
    at: Option<Pos2>,
    highlight: Option<usize>,
    pressed: Option<usize>,
    name: &'a str,
    max_height: f32,
}

/// A menu card at the cursor (no keyboard model; rows hover with the pointer).
pub fn menu_list(ui: &mut Ui, items: &[MenuItem<'_>]) -> ListOut { MenuList::new(items).show(ui) }

impl<'a> MenuList<'a> {
    pub fn new(items: &'a [MenuItem<'a>]) -> Self {
        MenuList { items, header: None, width: None, at: None, highlight: None, pressed: None, name: "Menu", max_height: f32::INFINITY }
    }
    /// Context-menu target row ("Shot V1-0046…"): 20 high, non-interactive, 11 tertiary.
    pub fn header(mut self, h: &'a str) -> Self { self.header = Some(h); self }
    /// Fixed outer width; otherwise the natural width, 200..360.
    pub fn width(mut self, w: f32) -> Self { self.width = Some(w); self }
    pub fn at(mut self, p: Pos2) -> Self { self.at = Some(p); self }
    /// Row drawn hovered (keyboard highlight or forced hover).
    pub fn highlight(mut self, i: Option<usize>) -> Self { self.highlight = i; self }
    /// Row drawn pressed (forced).
    pub fn pressed(mut self, i: Option<usize>) -> Self { self.pressed = i; self }
    pub fn name(mut self, n: &'a str) -> Self { self.name = n; self }
    /// Outer height limit: more rows scroll.
    pub fn max_height(mut self, h: f32) -> Self { self.max_height = h; self }

    fn content_height(&self) -> f32 { self.header.map_or(0.0, |_| HEAD) + self.items.iter().map(|i| i.height()).sum::<f32>() }

    /// Outer size (width, height) of the card.
    pub fn measure(&self, p: &Painter) -> egui::Vec2 {
        let h = (self.content_height() + 10.0).min(self.max_height);
        let w = self.width.unwrap_or_else(|| self.natural_width(p));
        vec2(w, h)
    }

    fn natural_width(&self, p: &Painter) -> f32 {
        let mut w = 0.0f32;
        for it in self.items.iter().filter(|i| !i.separator) {
            let mut row = 32.0 + width_of(p, it.label, ty::label(), 16.0) + 8.0;
            if let Some(s) = it.shortcut { row += 16.0 + width_of(p, s, ty::mono(), 14.0) } else if it.submenu.is_some() { row += 16.0 + 12.0 }
            else if let Some(r) = it.disabled.filter(|r| !r.is_empty()) { row += 16.0 + width_of(p, r, ty::small(), 14.0) }
            w = w.max(row + 10.0);
        }
        if let Some(h) = self.header { w = w.max(width_of(p, h, ty::small(), 14.0) + 24.0 + 2.0) }
        w.clamp(200.0, 360.0)
    }

    pub fn show(self, ui: &mut Ui) -> ListOut {
        let t = tokens(ui);
        let p = ui.painter().clone();
        let size = self.measure(&p);
        let pos = self.at.unwrap_or_else(|| ui.cursor().min);
        let rect = Rect::from_min_size(pos, size);
        if self.at.is_none() { ui.allocate_rect(rect, Sense::hover()); }
        let id = ui.scope_id().with(("menu-list", self.name, pos.x as i32, pos.y as i32));
        p.add(ui.visuals().popup_shadow.as_shape(rect, CornerRadius::same(4)));
        p.rect_filled(rect, 4.0, t.bg_overlay);
        p.rect_stroke(rect, 4.0, Stroke::new(1.0, t.border_overlay), StrokeKind::Inside);
        let group = ui.interact(rect, id, Sense::hover());
        group.widget_info(|| WidgetInfo::labeled(Role::Menu, true, self.header.unwrap_or(self.name)));

        let inner = rect.shrink(1.0);
        let mut out = ListOut { rect, clicked: None, hovered: None, rows: Vec::new() };
        let full = self.content_height() + 8.0;
        if full > inner.height() {
            ui.scope_builder(UiBuilder::new().max_rect(inner).accessibility_parent(id), |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    let origin = ui.cursor().min;
                    ui.allocate_space(vec2(inner.width(), full));
                    self.rows(ui, t, id, origin, inner.width(), &mut out);
                });
            });
        } else {
            ui.scope_builder(UiBuilder::new().max_rect(inner).accessibility_parent(id), |ui| self.rows(ui, t, id, inner.min, inner.width(), &mut out));
        }
        out
    }

    fn rows(&self, ui: &mut Ui, t: &Tokens, id: Id, origin: Pos2, w: f32, out: &mut ListOut) {
        let p = ui.painter().clone();
        let mut y = origin.y + 4.0;
        let (x0, x1) = (origin.x + 4.0, origin.x + w - 4.0);
        if let Some(h) = self.header {
            let r = Rect::from_min_max(egui::pos2(origin.x, y), egui::pos2(origin.x + w, y + HEAD));
            txt(&p, Rect::from_min_max(egui::pos2(r.min.x + 12.0, r.min.y), egui::pos2(r.max.x - 12.0, r.max.y)), h, ty::small(), 14.0, Align::Min, t.text_tertiary);
            y += HEAD;
        }
        for (i, it) in self.items.iter().enumerate() {
            if it.separator {
                let r = Rect::from_min_size(egui::pos2(origin.x, y), vec2(w, SEP));
                p.rect_filled(Rect::from_min_size(egui::pos2(origin.x, y + 4.0), vec2(w, 1.0)), 0.0, t.border_default);
                out.rows.push(r);
                y += SEP;
                continue;
            }
            let row = Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + ROW));
            out.rows.push(row);
            y += ROW;
            let sel = it.selectable();
            let resp = ui.interact(row, id.with(("item", i)), if sel { Sense::CLICK } else { Sense::hover() });
            if sel && resp.hovered() { out.hovered = Some(i) }
            if sel && resp.clicked() { out.clicked = Some(i) }
            let down = self.pressed == Some(i) || (sel && resp.is_pointer_button_down_on());
            let hot = match self.highlight { Some(h) => h == i, None => sel && resp.hovered() };
            self.row(&p, t, row, it, hot, down);
            self.row_access(ui, &resp, it);
            if let Some(r) = it.disabled.filter(|r| r.len() > 28) { let _ = tooltip(&resp, "", None, r); }
        }
    }

    fn row(&self, p: &Painter, t: &Tokens, row: Rect, it: &MenuItem<'_>, hot: bool, down: bool) {
        if down { p.rect_filled(row, 3.0, t.bg_pressed); } else if hot { p.rect_filled(row, 3.0, t.bg_menu_hover); }
        let on = hot || down;
        let label = if it.disabled.is_some() { t.text_disabled } else if it.destructive { t.error } else { t.text_primary };
        let tert = if on { t.text_primary } else { t.text_tertiary };
        let icon_col = if it.disabled.is_some() { t.text_disabled } else { t.text_link };
        let glyph = if it.checked { Some(Icon::Check) } else { it.icon };
        if let Some(ic) = glyph {
            let ir = Rect::from_min_size(egui::pos2(row.min.x + 8.0, row.min.y + 3.0), vec2(16.0, 16.0));
            icons::paint(p, ir, ic, IconSize::S16, icon_col);
        }
        let mut right = row.max.x - 8.0;
        if it.submenu.is_some() {
            let ir = Rect::from_min_size(egui::pos2(right - 12.0, row.min.y + 5.0), vec2(12.0, 12.0));
            icons::paint(p, ir, Icon::ChevronRight, IconSize::S12, if it.disabled.is_some() { t.text_disabled } else { tert });
            right -= 12.0 + 8.0;
        } else if let Some(s) = it.shortcut {
            let w = txt(p, Rect::from_min_max(egui::pos2(row.min.x, row.min.y), egui::pos2(right, row.max.y)), s, ty::mono(), 14.0, Align::Max, tert);
            right -= w + 8.0;
        } else if let Some(r) = it.disabled.filter(|r| !r.is_empty() && r.len() <= 28) {
            let w = txt(p, Rect::from_min_max(egui::pos2(row.min.x, row.min.y), egui::pos2(right, row.max.y)), r, ty::small(), 14.0, Align::Max, t.text_tertiary);
            right -= w + 8.0;
        }
        let lr = Rect::from_min_max(egui::pos2(row.min.x + 32.0, row.min.y), egui::pos2(right.max(row.min.x + 40.0), row.max.y));
        txt(p, lr, it.label, ty::label(), 16.0, Align::Min, label);
    }

    fn row_access(&self, ui: &Ui, resp: &Response, it: &MenuItem<'_>) {
        let role = if it.checked { Role::MenuItemCheckBox } else { Role::MenuItem };
        resp.widget_info(|| WidgetInfo::labeled(role, it.selectable(), it.label));
        ui.ctx().accesskit_node_builder(resp.id, |nd| {
            if it.checked { nd.set_toggled(Toggled::True) }
            if let Some(s) = it.shortcut { nd.set_keyboard_shortcut(s) }
            if let Some(r) = it.disabled.filter(|r| !r.is_empty()) { nd.set_description(r) }
        });
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Key model (§10.2)
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NavKey { Alt, Left, Right, Up, Down, Enter, Esc }

/// Keyboard / pointer state of one menu system (the bar, or one context menu). `path` is the highlighted row per open
/// level: every entry but the last is an open submenu.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Nav {
    pub focus: bool,
    pub bar: usize,
    pub open: bool,
    pub path: Vec<usize>,
    alt_down: bool,
    alt_clean: bool,
}

fn step(items: &[MenuItem<'_>], cur: usize, dir: i32) -> usize {
    let n = items.len();
    let ok = |i: usize| items[i].selectable();
    if !(0..n).any(ok) { return NONE }
    let mut i = if cur == NONE { if dir > 0 { n - 1 } else { 0 } } else { cur };
    for _ in 0..n {
        i = (i as i32 + dir).rem_euclid(n as i32) as usize;
        if ok(i) { return i }
    }
    NONE
}

fn first_enabled(items: &[MenuItem<'_>]) -> usize { step(items, NONE, 1) }

impl Nav {
    fn items<'a>(&self, tops: &[&'a [MenuItem<'a>]], depth: usize) -> Option<&'a [MenuItem<'a>]> {
        let mut items = *tops.get(self.bar)?;
        for &i in self.path.get(..depth)? { items = items.get(i)?.submenu? }
        Some(items)
    }

    fn open_bar(&mut self, tops: &[&[MenuItem<'_>]]) {
        self.open = true;
        self.path = vec![tops.get(self.bar).map_or(NONE, |t| first_enabled(t))];
    }

    /// Feed one key. Returns the activated row path (with the bar menu index first when `bar_mode`).
    pub(crate) fn key(&mut self, k: NavKey, tops: &[&[MenuItem<'_>]], bar_mode: bool) -> Option<Vec<usize>> {
        let n = tops.len().max(1);
        if k == NavKey::Alt {
            if self.focus || self.open { *self = Nav::default() } else if bar_mode { self.focus = true; self.bar = 0 }
            return None;
        }
        if !self.open {
            if !self.focus { return None }
            match k {
                NavKey::Left => self.bar = (self.bar + n - 1) % n,
                NavKey::Right => self.bar = (self.bar + 1) % n,
                NavKey::Up | NavKey::Down | NavKey::Enter => self.open_bar(tops),
                NavKey::Esc => self.focus = false,
                NavKey::Alt => {}
            }
            return None;
        }
        let d = self.path.len().saturating_sub(1);
        let items = self.items(tops, d)?;
        let cur = *self.path.get(d)?;
        let sub_of = |c: usize| items.get(c).filter(|it| it.selectable()).and_then(|it| it.submenu).filter(|s| !s.is_empty());
        match k {
            NavKey::Down => self.path[d] = step(items, cur, 1),
            NavKey::Up => self.path[d] = step(items, cur, -1),
            NavKey::Right => {
                if let Some(s) = sub_of(cur) { self.path.push(first_enabled(s)) }
                else if bar_mode && d == 0 { self.bar = (self.bar + 1) % n; self.open_bar(tops) }
            }
            NavKey::Left => {
                if d > 0 { self.path.pop(); }
                else if bar_mode { self.bar = (self.bar + n - 1) % n; self.open_bar(tops) }
            }
            NavKey::Enter => {
                if let Some(s) = sub_of(cur) { self.path.push(first_enabled(s)) }
                else if items.get(cur).is_some_and(|it| it.selectable()) {
                    let mut p = self.path.clone();
                    if bar_mode { p.insert(0, self.bar) }
                    *self = Nav::default();
                    return Some(p);
                }
            }
            NavKey::Esc => {
                if d > 0 { self.path.pop(); } else { self.open = false; self.path.clear(); if !bar_mode { self.focus = false } }
            }
            NavKey::Alt => {}
        }
        None
    }

    /// Keys of this frame -> `key` (arrows, Enter and Esc are consumed only while the menus have the keyboard).
    fn feed(&mut self, ctx: &Context, tops: &[&[MenuItem<'_>]], bar_mode: bool) -> Option<Vec<usize>> {
        let mut result = None;
        let (f10, alt, other) = ctx.input_mut(|i| {
            let f10 = bar_mode && i.consume_key(Modifiers::NONE, Key::F10);
            let other = i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }));
            (f10, i.modifiers.alt, other)
        });
        if bar_mode {
            if alt && !self.alt_down { self.alt_down = true; self.alt_clean = true }
            if self.alt_down && other { self.alt_clean = false }
            if !alt && self.alt_down {
                self.alt_down = false;
                if self.alt_clean { self.key(NavKey::Alt, tops, true); }
            }
            if f10 { self.key(NavKey::Alt, tops, true); }
        }
        if self.focus || self.open {
            for (key, nk) in [(Key::ArrowLeft, NavKey::Left), (Key::ArrowRight, NavKey::Right), (Key::ArrowUp, NavKey::Up), (Key::ArrowDown, NavKey::Down), (Key::Enter, NavKey::Enter), (Key::Escape, NavKey::Esc)] {
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                    if let Some(r) = self.key(nk, tops, bar_mode) { result = Some(r) }
                }
            }
        }
        result
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Popup driver: the open menu and its submenus as foreground areas
// ---------------------------------------------------------------------------------------------------------------

struct Driven {
    clicked: Option<Vec<usize>>,
    rects: Vec<Rect>,
}

/// Show the open levels of `nav` for `items`; pointer hover moves the highlight, a click activates.
fn drive(ctx: &Context, id: Id, nav: &mut Nav, items: &[MenuItem<'_>], header: Option<&str>, pos: Pos2) -> Driven {
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(Order::Foreground, id));
    let mut out = Driven { clicked: None, rects: Vec::new() };
    let (mut level_items, mut k, mut anchor): (&[MenuItem<'_>], usize, Option<Rect>) = (items, 0, None);
    let mut changed = false;
    loop {
        let hi = nav.path.get(k).copied();
        let mut list = MenuList::new(level_items).name("Menu").highlight(Some(hi.unwrap_or(NONE)));
        if k == 0 { if let Some(h) = header { list = list.header(h) } }
        let room = (screen.height() - 8.0).max(120.0);
        list.max_height = room;
        let size = list.measure(&painter);
        let mut p = match anchor {
            None => pos,
            Some(parent) => egui::pos2(parent.max.x - 2.0, parent.min.y - 5.0),
        };
        if p.x + size.x > screen.right() - 4.0 {
            p.x = match anchor { Some(parent) => (parent.min.x - size.x + 2.0).max(screen.left() + 4.0), None => (screen.right() - 4.0 - size.x).max(screen.left() + 4.0) };
        }
        if p.y + size.y > screen.bottom() - 4.0 { p.y = (screen.bottom() - 4.0 - size.y).max(screen.top() + 4.0) }
        let list = list.at(p);
        let res = Area::new(id.with(("level", k))).order(Order::Foreground).fixed_pos(p).interactable(true).show(ctx, |ui| list.show(ui));
        let lo = res.inner;
        out.rects.push(lo.rect);
        if let Some(h) = lo.hovered.filter(|h| Some(*h) != hi) {
            nav.path.truncate(k);
            nav.path.push(h);
            if level_items[h].submenu.is_some() { nav.path.push(NONE) }
            changed = true;
        }
        if let Some(c) = lo.clicked {
            if let Some(s) = level_items[c].submenu { nav.path.truncate(k); nav.path.push(c); nav.path.push(first_enabled(s)); changed = true }
            else { let mut path = nav.path[..k].to_vec(); path.push(c); out.clicked = Some(path) }
        }
        let next = nav.path.get(k).copied().filter(|&i| i != NONE).and_then(|i| level_items.get(i)).and_then(|it| it.submenu).filter(|_| nav.path.len() > k + 1);
        match next {
            Some(s) => { anchor = lo.rows.get(nav.path[k]).copied(); level_items = s; k += 1 }
            None => break,
        }
    }
    if changed { ctx.request_repaint() }
    out
}

fn load<T: Clone + Send + Sync + 'static + Default>(ctx: &Context, id: Id) -> T { ctx.data(|d| d.get_temp::<T>(id)).unwrap_or_default() }

// ---------------------------------------------------------------------------------------------------------------
// Menu bar (§5.9)
// ---------------------------------------------------------------------------------------------------------------

/// Right end layout decision (§5.9 overflow order): the save state collapses to its dot first, then the project name
/// truncates down to 80, then it is hidden. Menu names and the credit never truncate.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Plan {
    /// Drawn width of the project name (`None` = hidden).
    pub name: Option<f32>,
    pub full_state: bool,
    /// Right padding inside the right group (8 normally; shrinks by what the 80 px name floor needs).
    pub pad: f32,
}

/// `avail` = bar width − 4 (bar padding) − end of the menus.
pub(crate) fn plan(avail: f32, name_nat: Option<f32>, state_full_w: f32, credit_w: f32) -> Plan {
    let base = credit_w + 8.0 + 1.0 + 8.0; // credit, divider and their gaps
    let Some(nat) = name_nat else {
        return Plan { name: None, full_state: base + state_full_w + 8.0 <= avail, pad: 8.0 };
    };
    if base + state_full_w + 8.0 + nat + 8.0 <= avail { return Plan { name: Some(nat), full_state: true, pad: 8.0 } }
    let room = avail - 8.0 - (base + 6.0 + 8.0);
    if nat <= room { return Plan { name: Some(nat), full_state: false, pad: 8.0 } }
    if room >= 80.0 { return Plan { name: Some(room), full_state: false, pad: 8.0 } }
    if room + 8.0 >= 80.0 { return Plan { name: Some(80.0), full_state: false, pad: 8.0 - (80.0 - room) } }
    Plan { name: None, full_state: base + state_full_w + 8.0 <= avail, pad: 8.0 }
}

pub struct MenuBar<'a> {
    menus: &'a [Menu<'a>],
    right: MenuBarRight<'a>,
    width: Option<f32>,
    at: Option<Pos2>,
    force: Option<(Vec<Vis>, Option<usize>, Option<usize>)>,
    dot_only: bool,
}

/// The 24 px menu bar across the available width.
pub fn menu_bar(ui: &mut Ui, menus: &[Menu<'_>], right: MenuBarRight<'_>) -> MenuBarOut { MenuBar::new(menus, right).show(ui) }

impl<'a> MenuBar<'a> {
    pub fn new(menus: &'a [Menu<'a>], right: MenuBarRight<'a>) -> Self { MenuBar { menus, right, width: None, at: None, force: None, dot_only: false } }
    /// Compact window (§7.5): the save state is its dot only, whatever room there is.
    pub fn dot_only(mut self, on: bool) -> Self { self.dot_only = on; self }
    pub fn width(mut self, w: f32) -> Self { self.width = Some(w); self }
    pub fn at(mut self, p: Pos2) -> Self { self.at = Some(p); self }
    /// Specimen / test only: pointer look per menu, the open menu and the menu drawn with the keyboard focus ring.
    pub fn forced(mut self, vis: Vec<Vis>, open: Option<usize>, focus: Option<usize>) -> Self { self.force = Some((vis, open, focus)); self }

    pub fn show(self, ui: &mut Ui) -> MenuBarOut {
        let t = tokens(ui);
        let ctx = ui.ctx().clone();
        let p = ui.painter().clone();
        let w = self.width.unwrap_or_else(|| ui.available_width());
        let bar = match self.at {
            Some(pos) => Rect::from_min_size(pos, vec2(w, 24.0)),
            None => ui.allocate_exact_size(vec2(w, 24.0), Sense::hover()).0,
        };
        let id = ui.scope_id().with(("menubar", bar.min.x as i32, bar.min.y as i32));
        let group = ui.interact(bar, id, Sense::hover());
        group.widget_info(|| WidgetInfo::labeled(Role::MenuBar, ui.is_enabled(), "Menu bar"));
        let tops: Vec<&[MenuItem<'_>]> = self.menus.iter().map(|m| m.items).collect();
        let forced = self.force.is_some();
        let mut nav: Nav = if forced { Nav::default() } else { load(&ctx, id) };
        let mut clicked = None;
        if !forced { clicked = nav.feed(&ctx, &tops, true) }

        // Item rects (unsnapped x kept for the text).
        let mut x = bar.min.x + 4.0;
        let mut items = Vec::with_capacity(self.menus.len());
        for m in self.menus {
            let tw = width_of(&p, m.label, ty::label(), 16.0) + 1.0 / 64.0;
            items.push((x, tw + 16.0));
            x += tw + 16.0;
        }
        let menus_end = x;
        let irect = |i: usize| Rect::from_min_max(egui::pos2(snap(items[i].0), bar.min.y + 1.0), egui::pos2(snap(items[i].0 + items[i].1), bar.min.y + 23.0));

        // Item interaction.
        let mut states = Vec::with_capacity(self.menus.len());
        for (i, m) in self.menus.iter().enumerate() {
            let r = irect(i);
            let resp = ui.interact(r, id.with(("menu", i)), Sense::CLICK);
            let open = match &self.force { Some((_, o, _)) => *o == Some(i), None => nav.open && nav.bar == i };
            if !forced {
                if resp.clicked() {
                    if open { nav = Nav { focus: true, bar: i, ..Nav::default() } } else { nav = Nav { focus: true, bar: i, open: true, path: vec![NONE], ..nav.clone() } }
                } else if resp.hovered() && nav.open && nav.bar != i {
                    nav.bar = i;
                    nav.path = vec![NONE];
                }
            }
            let (vis, ring) = match &self.force {
                Some((v, _, f)) => (v.get(i).copied().unwrap_or(Vis::Rest), *f == Some(i)),
                None => (
                    if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest },
                    nav.focus && !nav.open && nav.bar == i && keyboard_focus(&ctx),
                ),
            };
            resp.widget_info(|| WidgetInfo::labeled(Role::MenuItem, ui.is_enabled(), m.label));
            ctx.accesskit_node_builder(resp.id, |nd| {
                nd.set_has_popup(egui::accesskit::HasPopup::Menu);
                nd.set_expanded(open);
            });
            states.push((vis, open, ring));
        }

        // Right end.
        let tx = |s: &str, f: FontId, lh: f32| width_of(&p, s, f, lh);
        let (state_txt, state_full_w) = if self.right.unsaved { ("Unsaved changes", 6.0 + 4.0 + tx("Unsaved changes", ty::small(), 14.0)) } else { ("Saved", tx("Saved", ty::small(), 14.0)) };
        let (kw, pw) = (tx("Keyloom", ty::panel_header(), 16.0), tx("powered by CorridorKey", ty::small(), 14.0));
        let credit_w = kw + 8.0 + pw;
        let name_nat = (!self.right.project.is_empty()).then(|| tx(self.right.project, ty::button(), 16.0));
        let pl = plan(bar.max.x - 4.0 - menus_end, name_nat, if self.dot_only { 1.0e6 } else { state_full_w }, credit_w);

        // Paint: bar, items.
        p.rect_filled(bar, 0.0, t.bg_app);
        p.rect_filled(Rect::from_min_max(egui::pos2(bar.min.x, bar.max.y - 1.0), bar.max), 0.0, t.border_divider);
        let trow = |y0: f32| Rect::from_min_max(egui::pos2(0.0, bar.min.y + y0), egui::pos2(0.0, bar.min.y + y0 + 22.0));
        for (i, m) in self.menus.iter().enumerate() {
            let (vis, open, ring) = states[i];
            let r = irect(i);
            if open || vis == Vis::Pressed { p.rect_filled(r, 3.0, t.bg_pressed); } else if vis == Vis::Hover { p.rect_filled(r, 3.0, t.bg_menu_hover); }
            let g = line(&p, m.label, ty::label(), 16.0, f32::INFINITY);
            let tr = trow(1.0);
            paint_line(&p, Rect::from_min_max(egui::pos2(items[i].0 + 8.0 + LABEL_DX, tr.min.y), egui::pos2(items[i].0 + items[i].1, tr.max.y)), g, 16.0, Align::Min, t.text_primary, 0.0);
            if ring { p.rect_stroke(r, 3.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
        }
        let mut rx = bar.max.x - 4.0 - pl.pad;
        let tr = trow(1.0);
        let seg = |x_left: f32, x_right: f32| Rect::from_min_max(egui::pos2(x_left, tr.min.y), egui::pos2(x_right, tr.max.y));
        txt(&p, seg(rx - pw, rx + 1.0), "powered by CorridorKey", ty::small(), 14.0, Align::Min, t.text_tertiary);
        rx -= pw + 8.0;
        txt(&p, seg(rx - kw, rx + 1.0), "Keyloom", ty::panel_header(), 16.0, Align::Min, t.text_primary);
        rx -= kw + 8.0;
        // No project open (Start): no project name, no save state and so no divider (§5.9, Start mockups).
        if self.right.project.is_empty() {
            return self.finish(ui, &ctx, id, bar, nav, clicked, forced, group, &items);
        }
        let sx = snap(rx - 1.0);
        p.rect_filled(Rect::from_min_size(egui::pos2(sx, snap(bar.min.y + 5.5)), vec2(1.0, 12.0)), 0.0, t.border_divider);
        rx = rx - 1.0 - 8.0;
        let dot_col = if self.right.unsaved { t.warning } else { t.text_tertiary };
        let dot = |x: f32| { p.circle_filled(egui::pos2(snap(x) + 3.0, bar.min.y + 12.0), 3.0, dot_col); };
        let state_rect;
        if pl.full_state {
            let left = rx - state_full_w;
            if self.right.unsaved {
                dot(left);
                txt(&p, seg(left + 10.0, rx + 1.0), state_txt, ty::small(), 14.0, Align::Min, t.text_tertiary);
            } else {
                txt(&p, seg(left, rx + 1.0), state_txt, ty::small(), 14.0, Align::Min, t.text_tertiary);
            }
            state_rect = Rect::from_min_max(egui::pos2(left, bar.min.y + 5.0), egui::pos2(rx, bar.min.y + 19.0));
            rx = left - 8.0;
        } else {
            dot(rx - 6.0);
            state_rect = Rect::from_min_max(egui::pos2(rx - 6.0, bar.min.y + 5.0), egui::pos2(rx, bar.min.y + 19.0));
            rx = rx - 6.0 - 8.0;
        }
        let state_name = if self.right.unsaved { "Unsaved changes" } else { "Saved" };
        let sresp = ui.interact(state_rect, id.with("state"), Sense::hover());
        sresp.widget_info(|| WidgetInfo::labeled(Role::Label, true, state_name));
        if !pl.full_state { let _ = tooltip(&sresp, "", None, state_name); }
        if let Some(nw) = pl.name {
            let nr = seg(rx - nw, rx);
            let g = line(&p, self.right.project, ty::button(), 16.0, nw + 0.01);
            let trunc = g.elided;
            paint_line(&p, nr, g, 16.0, Align::Min, t.text_primary, 0.0);
            let nresp = ui.interact(Rect::from_min_max(egui::pos2(rx - nw, bar.min.y + 1.0), egui::pos2(rx, bar.min.y + 23.0)), id.with("name"), Sense::hover());
            nresp.widget_info(|| WidgetInfo::labeled(Role::Label, true, self.right.project));
            if trunc { let _ = tooltip(&nresp, "", None, self.right.project); }
        }

        self.finish(ui, &ctx, id, bar, nav, clicked, forced, group, &items)
    }

    /// Open menus (not while forced: the specimen paints its lists itself), then store the key model.
    #[allow(clippy::too_many_arguments)]
    fn finish(&self, ui: &Ui, ctx: &Context, id: Id, bar: Rect, mut nav: Nav, mut clicked: Option<Vec<usize>>, forced: bool, group: Response, items: &[(f32, f32)]) -> MenuBarOut {
        let irect = |i: usize| Rect::from_min_max(egui::pos2(snap(items[i].0), bar.min.y + 1.0), egui::pos2(snap(items[i].0 + items[i].1), bar.min.y + 23.0));
        let _ = ui;
        if !forced && nav.open {
            let ctx = ctx.clone();
            let r = irect(nav.bar.min(self.menus.len().saturating_sub(1)));
            let bi = nav.bar.min(self.menus.len() - 1);
            let d = drive(&ctx, id, &mut nav, self.menus[bi].items, None, egui::pos2(r.min.x, bar.max.y));
            if let Some(mut c) = d.clicked { c.insert(0, nav.bar); clicked = Some(c) }
            let press = ctx.input(|i| i.pointer.press_origin());
            if let Some(pp) = press {
                if !bar.contains(pp) && !d.rects.iter().any(|r| r.contains(pp)) { nav = Nav::default() }
            }
            if clicked.is_some() { nav = Nav::default() }
        }
        let open = nav.open;
        if !forced { ctx.data_mut(|d| { d.insert_temp(id, nav); }); }
        MenuBarOut { response: group, clicked, open }
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Context menu
// ---------------------------------------------------------------------------------------------------------------

#[derive(Clone, Default)]
struct CtxState {
    pos: Option<Pos2>,
    nav: Nav,
}

/// A context menu on `response` (secondary click, Shift+F10 while focused). `header` names the target (§5.10). Returns
/// the activated row path (item index per level).
pub fn context_menu(response: &Response, header: &str, items: &[MenuItem<'_>]) -> Option<Vec<usize>> {
    let ctx = &response.ctx;
    let sid = response.id.with("context-menu");
    let mut st: CtxState = load(ctx, sid);
    let tops = [items];
    if response.secondary_clicked() {
        st.pos = response.interact_pointer_pos();
        st.nav = Nav { open: true, path: vec![NONE], ..Nav::default() };
    } else if response.has_focus() && ctx.input_mut(|i| i.consume_key(Modifiers::SHIFT, Key::F10)) {
        st.pos = Some(response.rect.left_bottom());
        st.nav = Nav::default();
        st.nav.open_bar(&tops);
    }
    let mut clicked = None;
    if st.nav.open {
        clicked = st.nav.feed(ctx, &tops, false);
        if let Some(pos) = st.pos {
            let d = drive(ctx, sid, &mut st.nav, items, Some(header), pos);
            if d.clicked.is_some() { clicked = d.clicked }
            let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|pp| !d.rects.iter().any(|r| r.contains(pp))));
            if outside && !response.secondary_clicked() { st.nav = Nav::default() }
        }
        if clicked.is_some() { st.nav = Nav::default() }
    }
    if !st.nav.open { st.pos = None }
    ctx.data_mut(|d| d.insert_temp(sid, st));
    clicked
}

// ---------------------------------------------------------------------------------------------------------------
// Tooltip (§5.11)
// ---------------------------------------------------------------------------------------------------------------

/// Tooltip body: name (icon-only controls), description, then the shortcut (mono, tertiary). 8×6 padding, 11/14.
fn tip_body(ui: &mut Ui, name: &str, shortcut: Option<&str>, description: &str, note: Option<&str>, wrap: f32, min_w: f32) {
    let t = tokens(ui);
    egui::Frame::new().inner_margin(egui::Margin { left: 4, right: 4, top: 2, bottom: 2 }).show(ui, |ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        ui.set_min_width(min_w);
        let add = |ui: &mut Ui, s: &str, font: FontId, col: Color32| {
            let mut job = LayoutJob::default();
            job.wrap.max_width = wrap;
            job.append(s, 0.0, ty::format(font, col, 14.0));
            let g = ui.painter().layout_job(job);
            let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
            ui.painter().galley(r.min, g, col);
        };
        if !name.is_empty() { add(ui, name, ty::small(), t.text_primary) }
        if !description.is_empty() { add(ui, description, ty::small(), t.text_primary) }
        if let Some(n) = note { add(ui, n, ty::small(), t.text_tertiary) }
        if let Some(s) = shortcut { add(ui, s, ty::mono(), t.text_tertiary) }
    });
}

/// Hover tooltip on `response` (500 ms, only while the pointer is still: the theme's `tooltip_delay`). Also the
/// control's AccessKit description.
pub fn tooltip(response: &Response, name: &str, shortcut: Option<&str>, description: &str) -> Response {
    let desc = if description.is_empty() { name } else { description };
    response.ctx.accesskit_node_builder(response.id, |nd| nd.set_description(desc));
    response.clone().on_hover_ui(|ui| tip_body(ui, name, shortcut, description, None, 302.0, 0.0))
}

/// Specimen: a tooltip card painted at `pos` with a fixed outer `width` (through the same frame and body as the real one).
pub fn paint_tooltip(ui: &mut Ui, pos: Pos2, width: f32, name: &str, shortcut: Option<&str>, description: &str, note: Option<&str>) {
    let inner = width - 10.0;
    ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(pos, vec2(width, 400.0))), |ui| {
        egui::Frame::popup(ui.style()).show(ui, |ui| tip_body(ui, name, shortcut, description, note, inner - 8.0, inner - 8.0));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUB: &[MenuItem<'static>] = &[MenuItem::new("A"), MenuItem::new("B")];
    const FILE: &[MenuItem<'static>] = &[
        MenuItem::new("New"),
        MenuItem::sep(),
        MenuItem::new("Save").disabled(""),
        MenuItem::new("Recent").submenu(SUB),
        MenuItem::new("Quit"),
    ];
    const EDIT: &[MenuItem<'static>] = &[MenuItem::new("Undo")];

    /// §5.9 overflow order with the sheet's measured widths (menus end 264.22, credit 183.98, name 601.06).
    #[test]
    fn menu_bar_overflow_order() {
        let avail = |bar: f32| bar - 4.0 - 264.22;
        let (name, state, credit) = (601.06, 100.98, 183.98);
        let p900 = plan(avail(900.0), Some(name), state, credit);
        assert!(!p900.full_state, "state is its dot at 900");
        assert!(p900.name.is_some_and(|w| w < name && w > 300.0), "name truncates at 900: {p900:?}");
        let p569 = plan(avail(569.0), Some(name), state, credit);
        assert!(p569.name.is_some_and(|w| w >= 80.0), "name keeps 80 at 569: {p569:?}");
        assert_eq!(plan(avail(520.0), Some(name), state, credit).name, None, "hidden below 569");
        // Short name + wide bar: everything natural, full state.
        assert_eq!(plan(avail(1920.0), Some(30.0), state, credit), Plan { name: Some(30.0), full_state: true, pad: 8.0 });
    }

    /// Start (no project): the right end is only the credit; no save state, no name.
    #[test]
    fn menu_bar_without_project_has_no_save_state() {
        let labels = |project: &str| {
            let ctx = Context::default();
            ctx.enable_accesskit();
            crate::ui::fonts::install(&ctx);
            let menus = [Menu { label: "File", items: FILE }];
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1920.0, 200.0))), ..Default::default() }, |ui| {
                menu_bar(ui, &menus, MenuBarRight { project, unsaved: false });
            });
            out.textures_delta.clear();
            out.platform_output.accesskit_update.unwrap().nodes.iter().filter_map(|(_, n)| n.label().or(n.value()).map(str::to_owned)).collect::<Vec<_>>()
        };
        assert!(labels("Spec").iter().any(|l| l == "Saved"));
        let start = labels("");
        assert!(!start.iter().any(|l| l == "Saved" || l == "Unsaved changes"), "{start:?}");
    }

    #[test]
    fn menu_keys_open_navigate_close() {
        let tops: [&[MenuItem<'_>]; 2] = [FILE, EDIT];
        let mut n = Nav::default();
        assert_eq!(n.key(NavKey::Down, &tops, true), None);
        assert!(!n.focus && !n.open, "keys do nothing before Alt / F10");
        n.key(NavKey::Alt, &tops, true);
        assert!(n.focus && n.bar == 0 && !n.open);
        n.key(NavKey::Right, &tops, true);
        assert_eq!(n.bar, 1);
        n.key(NavKey::Left, &tops, true);
        n.key(NavKey::Down, &tops, true);
        assert!(n.open && n.path == vec![0], "Down opens with the first row");
        n.key(NavKey::Down, &tops, true);
        assert_eq!(n.path, vec![3], "separator and disabled row skipped");
        n.key(NavKey::Right, &tops, true);
        assert_eq!(n.path, vec![3, 0], "→ opens the submenu");
        n.key(NavKey::Left, &tops, true);
        assert_eq!(n.path, vec![3], "← closes it");
        n.key(NavKey::Esc, &tops, true);
        assert!(!n.open && n.focus, "Esc closes the menu, bar keeps focus");
        n.key(NavKey::Esc, &tops, true);
        assert!(!n.focus);
        n.key(NavKey::Alt, &tops, true);
        n.key(NavKey::Down, &tops, true);
        assert_eq!(n.key(NavKey::Enter, &tops, true), Some(vec![0, 0]));
        assert!(!n.open && !n.focus, "activating closes everything");
    }
}
