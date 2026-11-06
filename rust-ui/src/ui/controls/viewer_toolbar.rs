//! Viewer toolbar and inspection overlays (bible §7.2 "Viewer toolbar (28)" / "Pixel readout" / "Viewer toolbar at smaller
//! widths", §7.5, §10.1 viewer shortcuts, §2.1 `viewer.surround`, §12). A 28 px row painted at fixed slots: left View +
//! Overlay, right-anchored groups Format · Channel · Background · Gain/Gamma · Zoom · Modes separated by 1 px dividers with
//! 8 px either side. Wide shows everything, Medium folds Format into an `info` icon and Gain/Gamma into a `sliders`
//! popover, Compact also turns View/Overlay into dropdowns and moves 1:1 into the zoom list. Reuses `Dropdown`, `Segmented`,
//! `NumericField`, the icon/toggle buttons and `neutral_badge` (the narrow dropdowns are `Dropdown::compact`); only the mono
//! "1:1" button is painted here.
use super::buttons::{icon_button, look, toggle_button, IconButtonSize, Kind, Vis};
use super::chips::neutral_badge;
use super::dropdown::{list_placement, Dropdown, Item, ValueFont};
use super::numeric::{NumSpec, NumericField};
use super::segmented::{Seg, SegWidth, Segmented};
use super::{keyboard_focus, line, paint_line};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens, DARK};
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, accesskit::Role, epaint::Shadow, text::LayoutJob, Align, Area, Color32, CornerRadius, Id, Key, Modifiers, Order, Painter, Pos2,
    Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo,
};

// ---- state ------------------------------------------------------------------------------------------------------------

/// The seven outputs of the View dropdown (Alt+1 … Alt+7, §10.1).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View { Input, Hint, Mask, Fg, Matte, Comp, Processed }
impl View {
    pub const ALL: [View; 7] = [View::Input, View::Hint, View::Mask, View::Fg, View::Matte, View::Comp, View::Processed];
    pub fn label(self) -> &'static str { ["Input", "Hint", "Mask", "FG", "Matte", "Comp", "Processed"][self as usize] }
    pub fn shortcut(self) -> &'static str { ["Alt+1", "Alt+2", "Alt+3", "Alt+4", "Alt+5", "Alt+6", "Alt+7"][self as usize] }
    /// FG, Comp and Processed carry transparency (the Background choice only applies there).
    pub fn has_alpha(self) -> bool { matches!(self, View::Fg | View::Comp | View::Processed) }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Overlay { Off, PreMatte, Hint, Sam }
impl Overlay {
    pub const ALL: [Overlay; 4] = [Overlay::Off, Overlay::PreMatte, Overlay::Hint, Overlay::Sam];
    pub fn label(self) -> &'static str { ["Off", "Pre-matte", "Hint", "SAM"][self as usize] }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Channel { Rgb, R, G, B, A, Luma }
impl Channel {
    pub const ALL: [Channel; 6] = [Channel::Rgb, Channel::R, Channel::G, Channel::B, Channel::A, Channel::Luma];
    pub fn label(self) -> &'static str { ["RGB", "R", "G", "B", "A", "Luma"][self as usize] }
    fn key(self) -> Option<Key> { [None, Some(Key::R), Some(Key::G), Some(Key::B), Some(Key::A), Some(Key::Y)][self as usize] }
    fn shortcut(self) -> Option<&'static str> { [None, Some("R"), Some("G"), Some("B"), Some("A"), Some("Y")][self as usize] }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Background { Black, Grey, White, Checker }
impl Background {
    pub const ALL: [Background; 4] = [Background::Black, Background::Grey, Background::White, Background::Checker];
    pub fn label(self) -> &'static str { ["Black", "Grey", "White", "Checkerboard"][self as usize] }
    /// K: Black → Grey → White → Checkerboard → Black.
    pub fn next(self) -> Background { Self::ALL[(self as usize + 1) % 4] }
}

/// Width class of the window (§7.5), decided by the caller.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout { Wide, Medium, Compact }

/// Everything the toolbar edits. `zoom` is the actual picture scale in percent (the readout shows it), `gain` / `gamma`
/// the display multipliers (display only, never written to any output).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct ViewerState {
    pub view: View,
    pub overlay: Overlay,
    pub channel: Channel,
    pub background: Background,
    pub gain: f64,
    pub gamma: f64,
    pub zoom: f64,
    pub wipe: bool,
    pub focus: bool,
}
impl Default for ViewerState {
    fn default() -> Self {
        ViewerState { view: View::Input, overlay: Overlay::Off, channel: Channel::Rgb, background: Background::Black, gain: 1.0, gamma: 1.0, zoom: 100.0, wipe: false, focus: false }
    }
}

/// What happened this frame. `changed`: any `ViewerState` field changed. `fit` / `one_to_one` / `zoom_to` ask the viewer to
/// move the picture (Fit button, H / Ctrl+0, Ctrl+1 and the zoom list); `zoom` in the state is already updated for `zoom_to`.
#[derive(Clone, Copy, Debug)]
pub struct ToolbarOut {
    pub changed: bool,
    pub fit: bool,
    pub one_to_one: bool,
    pub zoom_to: Option<f64>,
    pub rect: Rect,
}

impl Default for ToolbarOut {
    fn default() -> Self { ToolbarOut { changed: false, fit: false, one_to_one: false, zoom_to: None, rect: Rect::NOTHING } }
}

/// Specimen / test only: open lists drawn through the real paint path.
#[derive(Clone, Copy)]
pub struct Forced {
    /// View list open with this row highlighted.
    pub view_list: Option<usize>,
    /// Top-left of the View list from the box's bottom-left.
    pub list_offset: Vec2,
    /// Gain / Gamma popover open (Medium, Compact).
    pub sliders_open: bool,
}
impl Default for Forced {
    fn default() -> Self { Forced { view_list: None, list_offset: Vec2::new(0.0, 2.0), sliders_open: false } }
}

pub fn gain_spec() -> NumSpec { NumSpec::gain() }
pub fn gamma_spec() -> NumSpec { NumSpec::gamma() }

fn at_default(st: &ViewerState) -> bool { gain_spec().is_default(st.gain) && gamma_spec().is_default(st.gamma) }

// ---- layout -----------------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Debug)]
enum Slot { Badge(f32), Info, Div, Channel, Bg, GainIcon, Gain, GammaIcon, Gamma, Reset, Sliders, Zoom, Fit, One, Wipe, Focus, Gap }
impl Slot {
    fn w(self) -> f32 {
        match self {
            Slot::Badge(w) => w,
            Slot::Div => 17.0,
            Slot::Channel => 56.0,
            Slot::Bg => 36.0,
            Slot::Gain | Slot::Gamma => 48.0,
            Slot::Reset => 18.0,
            Slot::Zoom => 60.0,
            Slot::One => 26.0,
            Slot::Gap => 4.0,
            Slot::Info | Slot::GainIcon | Slot::GammaIcon | Slot::Sliders | Slot::Fit | Slot::Wipe | Slot::Focus => 22.0,
        }
    }
}

/// The right-anchored groups, left → right (§7.2 groups 1–6, `Div` between groups).
fn cluster(layout: Layout, badges: &[f32]) -> Vec<Slot> {
    let mut v = Vec::new();
    if layout == Layout::Wide {
        for (i, w) in badges.iter().enumerate() {
            if i > 0 { v.push(Slot::Gap) }
            v.push(Slot::Badge(*w));
        }
        if !badges.is_empty() { v.push(Slot::Div) }
        v.extend([Slot::Channel, Slot::Div, Slot::Bg, Slot::Div, Slot::GainIcon, Slot::Gain, Slot::Gap, Slot::GammaIcon, Slot::Gamma, Slot::Gap, Slot::Reset, Slot::Div]);
        v.extend([Slot::Zoom, Slot::Gap, Slot::Fit, Slot::One, Slot::Div, Slot::Wipe, Slot::Gap, Slot::Focus]);
    } else {
        v.extend([Slot::Info, Slot::Div, Slot::Channel, Slot::Div, Slot::Bg, Slot::Div, Slot::Sliders, Slot::Div, Slot::Zoom]);
        if layout == Layout::Medium { v.extend([Slot::Gap, Slot::Fit, Slot::One]) }
        v.extend([Slot::Div, Slot::Wipe, Slot::Gap, Slot::Focus]);
    }
    v
}
fn cluster_width(layout: Layout, badges: &[f32]) -> f32 { cluster(layout, badges).iter().map(|s| s.w()).sum() }

/// Compact budget (§7.2): pad 8 + View 96 + 8 + Overlay 96 + gap 16 + the right groups + pad 8 = 587.
pub fn compact_min_width() -> f32 { 8.0 + 96.0 + 8.0 + 96.0 + 16.0 + cluster_width(Layout::Compact, &[]) + 8.0 }

// ---- shortcuts (§10.1, scope Viewer) -------------------------------------------------------------------------------------

/// Viewer shortcuts: Alt+1…7 view, R G B A Y channel (the active key again → RGB), K background (only where there is
/// transparency), W wipe, F focus, H / Ctrl+0 fit, Ctrl+1 1:1. Single keys yield to field edits and open lists / menus.
pub fn handle_shortcuts(ctx: &egui::Context, st: &mut ViewerState, has_alpha: bool) -> ToolbarOut {
    let mut out = ToolbarOut::default();
    let key = |m: Modifiers, k: Key| ctx.input_mut(|i| i.consume_key(m, k));
    const NUM: [Key; 7] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7];
    for (i, k) in NUM.into_iter().enumerate() {
        if key(Modifiers::ALT, k) && st.view != View::ALL[i] { st.view = View::ALL[i]; out.changed = true }
    }
    if key(Modifiers::COMMAND, Key::Num0) { out.fit = true }
    if key(Modifiers::COMMAND, Key::Num1) { out.one_to_one = true }
    // Custom dropdowns, menus and popovers use foreground Areas rather than egui Popups.
    // The Activity details popover is a foreground Area too, but it must not swallow viewer keys.
    #[allow(deprecated)] // same fixed Area salt as app/strip.rs
    let activity = Id::new("activity_details");
    if ctx.text_edit_focused() || egui::Popup::is_any_open(ctx)
        || ctx.data(|d| d.iter_temp_by_type::<super::menus::Nav>().any(|nav| nav.focus || nav.open))
        || ctx.memory(|m| m.layer_ids().any(|l| l.order == Order::Foreground && l.id != activity && m.areas().is_visible(&l)))
    { return out }
    for c in Channel::ALL {
        if let Some(k) = c.key() {
            if key(Modifiers::NONE, k) {
                st.channel = if st.channel == c { Channel::Rgb } else { c };
                out.changed = true;
            }
        }
    }
    if key(Modifiers::NONE, Key::K) && has_alpha { st.background = st.background.next(); out.changed = true }
    if key(Modifiers::NONE, Key::W) { st.wipe = !st.wipe; out.changed = true }
    if key(Modifiers::NONE, Key::F) { st.focus = !st.focus; out.changed = true }
    if key(Modifiers::NONE, Key::H) { out.fit = true }
    out
}

// ---- backgrounds ------------------------------------------------------------------------------------------------------

const CK_DARK: Color32 = Color32::from_rgb(0x66, 0x66, 0x66);
const CK_LIGHT: Color32 = Color32::from_rgb(0x99, 0x99, 0x99);

fn paint_bg(p: &Painter, t: &Tokens, rect: Rect, bg: Background, cell: f32, alpha: f32) {
    let m = |c: Color32| c.gamma_multiply(alpha);
    match bg {
        Background::Black => { p.rect_filled(rect, 0.0, m(Color32::BLACK)); }
        Background::Grey => { p.rect_filled(rect, 0.0, m(t.viewer_surround)); }
        Background::White => { p.rect_filled(rect, 0.0, m(Color32::WHITE)); }
        Background::Checker => {
            p.rect_filled(rect, 0.0, m(CK_DARK));
            let (nx, ny) = ((rect.width() / cell).ceil() as i32, (rect.height() / cell).ceil() as i32);
            for cy in 0..ny {
                for cx in 0..nx {
                    if (cx + cy) % 2 == 0 {
                        let c = Rect::from_min_size(rect.min + Vec2::new(cx as f32 * cell, cy as f32 * cell), Vec2::splat(cell)).intersect(rect);
                        p.rect_filled(c, 0.0, m(CK_LIGHT));
                    }
                }
            }
        }
    }
}

/// The viewer background behind a transparent picture (§7.2 group 3): black / grey (`viewer.surround`) / white / 8 px
/// checkerboard `#666` / `#999`, the checks starting at the rect's top-left.
pub fn paint_background(ui: &Ui, rect: Rect, bg: Background) { paint_bg(&ui.painter().with_clip_rect(rect.intersect(ui.clip_rect())), tokens(ui), rect, bg, 8.0, 1.0) }

/// 14 px swatch with its 1 px `border.control`; the checkerboard uses 4 px checks here so 12 px still reads as a checkerboard.
fn swatch(p: &Painter, t: &Tokens, r: Rect, bg: Background, dim: bool) {
    let a = if dim { 0.4 } else { 1.0 };
    paint_bg(&p.with_clip_rect(r.shrink(1.0)), t, r.shrink(1.0), bg, 4.0, a);
    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.border_control.gamma_multiply(a)), StrokeKind::Inside);
}

// ---- pixel readout ------------------------------------------------------------------------------------------------------

const READOUT_H: f32 = 20.0;
const READOUT_PAD: f32 = 6.0;

/// Where the readout strip goes (§7.2): 4 px in from the bottom-left of the viewer region; in the surround when the space
/// below the fitted picture is ≥ 24 px; otherwise over the picture's bottom-left corner, flipping to the bottom-right when
/// the pointer is within 48 px of that corner strip.
pub fn readout_rect(viewer: Rect, picture: Rect, pointer: Option<Pos2>, width: f32) -> Rect {
    let y = viewer.max.y - 4.0 - READOUT_H;
    let left = Rect::from_min_size(Pos2::new(viewer.min.x + 4.0, y), Vec2::new(width, READOUT_H));
    if viewer.max.y - picture.max.y >= 24.0 { return left }
    match pointer {
        Some(p) if left.expand(48.0).contains(p) => Rect::from_min_size(Pos2::new(viewer.max.x - 4.0 - width, y), Vec2::new(width, READOUT_H)),
        _ => left,
    }
}

/// `x 1204  y 388   R 0.512  G 0.430  B 0.118  A 1.000` (mono 11 primary, the letters tertiary); Compact drops x / y.
fn readout_job(t: &Tokens, rgba: [f32; 4], xy: (i32, i32), compact: bool) -> LayoutJob {
    let mut job = LayoutJob::default();
    job.wrap.max_rows = 1;
    job.wrap.max_width = f32::INFINITY;
    let mut add = |s: &str, c: Color32| job.append(s, 0.0, ty::format(ty::mono(), c, 14.0));
    if !compact {
        add("x", t.text_tertiary);
        add(&format!(" {}  ", xy.0), t.text_primary);
        add("y", t.text_tertiary);
        add(&format!(" {}   ", xy.1), t.text_primary);
    }
    for (i, (l, v)) in ["R", "G", "B", "A"].iter().zip(rgba).enumerate() {
        add(l, t.text_tertiary);
        add(&format!(" {:.3}{}", v, if i < 3 { "  " } else { "" }), t.text_primary);
    }
    job
}

/// The pixel readout strip (20 high, `bg.overlay` @ 85 %). Drawn only while `pointer` (over the image) is `Some`; `rgba` is
/// the pixel before gain / gamma. Returns its rect when drawn.
pub fn pixel_readout(ui: &mut Ui, viewer_rect: Rect, picture_rect: Rect, pointer: Option<Pos2>, rgba: [f32; 4], xy: (i32, i32), compact: bool) -> Option<Rect> {
    pointer?;
    let t = tokens(ui);
    let g = ui.painter().layout_job(readout_job(t, rgba, xy, compact));
    let r = readout_rect(viewer_rect, picture_rect, pointer, g.size().x + 2.0 * READOUT_PAD);
    let rs = Rect::from_min_max(Pos2::new(r.min.x.round(), r.min.y.round()), Pos2::new(r.max.x.round(), r.max.y.round()));
    let p = ui.painter();
    p.rect_filled(rs, 0.0, t.bg_overlay.gamma_multiply(0.85));
    paint_line(p, Rect::from_min_max(Pos2::new(r.min.x + READOUT_PAD, r.min.y), r.max), g, 14.0, Align::Min, t.text_primary, 0.0);
    Some(rs)
}

// ---- small helpers ----------------------------------------------------------------------------------------------------

/// Run `f` in a fixed box at `pos` (flow widgets land on its top-left corner); `key` keeps widget ids stable.
fn place<R>(ui: &mut Ui, key: &str, pos: Pos2, size: Vec2, f: impl FnOnce(&mut Ui) -> R) -> R {
    ui.scope_builder(UiBuilder::new().id_salt(key).max_rect(Rect::from_min_size(pos, size)), |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        f(ui)
    })
    .inner
}

// ---- the toolbar ------------------------------------------------------------------------------------------------------

const VIEW_C: [&str; 7] = ["View: Input", "View: Hint", "View: Mask", "View: FG", "View: Matte", "View: Comp", "View: Processed"];
const OVERLAY_C: [&str; 4] = ["Overlay: Off", "Overlay: Pre-matte", "Overlay: Hint", "Overlay: SAM"];
const ZOOM_PCT: [f64; 4] = [25.0, 50.0, 100.0, 200.0];

pub fn viewer_toolbar(ui: &mut Ui, st: &mut ViewerState, layout: Layout, has_alpha: bool, format: &str, hdr: bool) -> ToolbarOut {
    viewer_toolbar_forced(ui, st, layout, has_alpha, format, hdr, None)
}

/// Gain / Gamma slots (Wide inline and the Medium popover): icon + numeric 48 each, the fixed `reset` slot always drawn.
fn gain_gamma_slot(ui: &mut Ui, st: &mut ViewerState, slot: Slot, x: f32, y: f32, out: &mut ToolbarOut) {
    let t = tokens(ui);
    let icon = |ui: &mut Ui, ic: Icon, name: &str| {
        let r = Rect::from_min_size(Pos2::new(x, y), Vec2::splat(22.0));
        let resp = ui.interact(r, ui.make_persistent_id(("vt-icon", name)), Sense::hover());
        icons::paint(ui.painter(), Rect::from_min_size(Pos2::new(x + 3.0, y + 3.0), Vec2::splat(16.0)), ic, IconSize::S16, t.text_secondary);
        resp.widget_info(|| WidgetInfo::labeled(Role::Label, true, name));
        let _ = resp.on_hover_text(name);
    };
    match slot {
        Slot::GainIcon => icon(ui, Icon::Gain, "Gain"),
        Slot::GammaIcon => icon(ui, Icon::Gamma, "Gamma"),
        Slot::Gain => {
            let spec = gain_spec();
            let r = place(ui, "vt-gain", Pos2::new(x, y), Vec2::new(48.0, 22.0), |ui| NumericField::new("vt-gain", &mut st.gain, &spec).width(48.0).name("Gain").show(ui));
            if r.changed() { out.changed = true }
        }
        Slot::Gamma => {
            let spec = gamma_spec();
            let r = place(ui, "vt-gamma", Pos2::new(x, y), Vec2::new(48.0, 22.0), |ui| NumericField::new("vt-gamma", &mut st.gamma, &spec).width(48.0).name("Gamma").show(ui));
            if r.changed() { out.changed = true }
        }
        Slot::Reset => {
            // 18 px ghost icon button with the 16 px drawing (`reset` has no 14 px version, so `IconButtonSize::Small18` cannot be used).
            let def = at_default(st);
            let r = Rect::from_min_size(Pos2::new(x, y + 2.0), Vec2::splat(18.0));
            let resp = ui.interact(r, ui.make_persistent_id(("vt-icon", "reset", r.min.x as i32)), if def { Sense::hover() } else { Sense::click() });
            let vis = if def { Vis::Rest } else if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest };
            let l = look(t, Kind::Icon, vis, false, !def);
            let p = ui.painter();
            if let Some(f) = l.fill { p.rect_filled(r, CornerRadius::same(3), f); }
            if !def && resp.has_focus() && keyboard_focus(ui.ctx()) {
                p.rect_stroke(r, CornerRadius::same(3), Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
            } else if let Some(b) = l.border {
                p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, b), StrokeKind::Inside);
            }
            icons::paint(p, Rect::from_min_size(r.min + Vec2::splat(1.0), Vec2::splat(16.0)), Icon::Reset, IconSize::S16, l.icon);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, !def, "Reset gain and gamma"));
            if def { ui.ctx().accesskit_node_builder(resp.id, |n| n.set_description("Gain and gamma are at default")); }
            let tip = if def { "Gain and gamma are at default" } else { "Reset gain and gamma" };
            if resp.on_hover_text(tip).clicked() && !def { st.gain = 1.0; st.gamma = 1.0; out.changed = true }
        }
        _ => {}
    }
}

/// `viewer_toolbar` plus specimen-only forced open lists.
pub fn viewer_toolbar_forced(ui: &mut Ui, st: &mut ViewerState, layout: Layout, has_alpha: bool, format: &str, hdr: bool, forced: Option<Forced>) -> ToolbarOut {
    let t = tokens(ui);
    let ctx = ui.ctx().clone();
    let mut out = if forced.is_none() { handle_shortcuts(&ctx, st, has_alpha) } else { ToolbarOut::default() };
    let fo = forced.unwrap_or_default();
    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 28.0), Sense::hover());
    out.rect = row;
    let p = ui.painter().clone();
    p.rect_filled(row, 0.0, t.bg_panel);
    p.rect_filled(Rect::from_min_max(Pos2::new(row.min.x, row.max.y - 1.0), row.max), 0.0, t.border_divider);
    let y = row.min.y + 3.0;
    let compact = layout == Layout::Compact;
    if st.view == View::Hint && st.overlay == Overlay::Hint { st.overlay = Overlay::Off; out.changed = true }

    // ---- left: View · Overlay -------------------------------------------------------------------------------------------
    let mut x = row.min.x + 8.0;
    let label = |ui: &mut Ui, x: f32, s: &str| -> f32 {
        let g = line(ui.painter(), s, ty::small(), 16.0, f32::INFINITY);
        let w = g.size().x;
        paint_line(ui.painter(), Rect::from_min_size(Pos2::new(x, row.min.y + 5.5), Vec2::new(w, 16.0)), g, 16.0, Align::Min, t.text_tertiary, 0.0);
        w + 4.0
    };
    if !compact { x += label(ui, x, "View") }
    let items: Vec<Item> = View::ALL.iter().map(|v| Item::new(v.label()).shortcut(v.shortcut())).collect();
    let hint_off = st.view == View::Hint;
    if compact {
        let closed = VIEW_C[st.view as usize];
        let mut vi = st.view as usize;
        let mut dd = Dropdown::new("vt-view-c", &mut vi, &items, 96.0).compact().at(Pos2::new(x, y)).name("View").display(closed).font(ValueFont::Small).tip(closed).list_width(180.0);
        if let Some(h) = fo.view_list { dd = dd.forced(Vis::Rest, true, false).highlight(h) }
        if dd.show(ui).changed() { st.view = View::ALL[vi]; out.changed = true }
        x += 96.0 + 8.0;
        let items: Vec<Item> = Overlay::ALL.iter().map(|o| {
            let it = Item::new(o.label());
            if *o == Overlay::Hint && hint_off { it.disabled("Already viewing the hint") } else { it }
        }).collect();
        let closed = OVERLAY_C[st.overlay as usize];
        let mut oi = st.overlay as usize;
        if Dropdown::new("vt-overlay-c", &mut oi, &items, 96.0).compact().at(Pos2::new(x, y)).name("Overlay").display(closed).font(ValueFont::Small).tip(closed).list_width(140.0).show(ui).changed() {
            st.overlay = Overlay::ALL[oi];
            out.changed = true;
        }
    } else {
        let mut vi = st.view as usize;
        let mut dd = Dropdown::new("vt-view", &mut vi, &items, 96.0).at(Pos2::new(x, y)).name("View").list_width(180.0).list_offset(fo.list_offset);
        if let Some(h) = fo.view_list { dd = dd.forced(Vis::Rest, true, false).highlight(h) }
        if dd.show(ui).changed() { st.view = View::ALL[vi]; out.changed = true }
        x += 96.0 + 16.0;
        x += label(ui, x, "Overlay");
        let segs: Vec<Seg<Overlay>> = Overlay::ALL.iter().map(|o| {
            let s = Seg::new(*o, o.label());
            if *o == Overlay::Hint && hint_off { s.disabled("Already viewing the hint") } else { s }
        }).collect();
        if Segmented::new(&mut st.overlay, &segs).width(SegWidth::Natural).name("Overlay").at(Pos2::new(x, y)).show(ui).changed() { out.changed = true }
    }

    // ---- right-anchored groups --------------------------------------------------------------------------------------------
    let mut badge_text: Vec<String> = Vec::new();
    if !format.is_empty() { badge_text.push(format.to_owned()) }
    if hdr { badge_text.push("HDR clipped display".to_owned()) }
    let badge_w: Vec<f32> = if layout == Layout::Wide {
        badge_text.iter().map(|s| (line(&p, s, ty::chip(), 14.0, f32::INFINITY).size().x + 12.0).round()).collect()
    } else {
        Vec::new()
    };
    let slots = cluster(layout, &badge_w);
    let mut x = row.max.x - 8.0 - slots.iter().map(|s| s.w()).sum::<f32>();
    let mut badge_i = 0;
    // Thin space, as the mockup's "38 %" readout: the normal mono space reads wider.
    let zoom_text = format!("{:.0}\u{2009}%", st.zoom);
    for slot in slots {
        let sx = x.round();
        match slot {
            Slot::Badge(w) => {
                let s = badge_text[badge_i].clone();
                badge_i += 1;
                place(ui, ("vt-badge-".to_owned() + &badge_i.to_string()).as_str(), Pos2::new(sx, row.min.y + 5.0), Vec2::new(w, 18.0), |ui| neutral_badge(ui, &s));
            }
            Slot::Info => {
                let tip = if badge_text.is_empty() && !hdr {
                    "Format".to_owned()
                } else {
                    let mut s = format.to_owned();
                    if hdr { s.push_str("\nHDR clipped display") }
                    s
                };
                let r = ui.add(icon_button(Icon::Info, IconButtonSize::Normal22, &tip).at(Pos2::new(sx, y)));
                if hdr { p.circle_filled(Pos2::new(r.rect.max.x - 2.0 - 3.0, r.rect.min.y + 2.0 + 3.0), 3.0, t.text_secondary); }
            }
            Slot::Div => { p.rect_filled(Rect::from_min_size(Pos2::new(sx + 8.0, row.min.y + 6.0), Vec2::new(1.0, 16.0)), 0.0, t.border_divider); }
            Slot::Channel => {
                let items: Vec<Item> = Channel::ALL.iter().map(|c| { let it = Item::new(c.label()); if let Some(s) = c.shortcut() { it.shortcut(s) } else { it } }).collect();
                let color = if st.channel == Channel::Rgb { t.text_primary } else { t.text_link };
                let mut ci = st.channel as usize;
                if Dropdown::new("vt-channel", &mut ci, &items, 56.0).compact().at(Pos2::new(x, y)).name("Channel").tip("Channel (R G B A Y)").value_color(color).list_width(120.0).show(ui).changed() {
                    st.channel = Channel::ALL[ci];
                    out.changed = true;
                }
            }
            Slot::Bg => {
                let items: Vec<Item> = Background::ALL.iter().map(|b| Item::new(b.label())).collect();
                let paint = |p: &Painter, r: Rect, i: usize, dim: bool| swatch(p, t, r, Background::ALL[i], dim);
                let mut bi = st.background as usize;
                let dd = Dropdown::new("vt-bg", &mut bi, &items, 36.0).at(Pos2::new(x, y)).name("Background").tip("Background (K)").swatch(&paint).list_width(150.0);
                if dd.disabled_if(!has_alpha, "No transparency in this view").show(ui).changed() { st.background = Background::ALL[bi]; out.changed = true }
            }
            Slot::GainIcon | Slot::Gain | Slot::GammaIcon | Slot::Gamma | Slot::Reset => gain_gamma_slot(ui, st, slot, sx, y, &mut out),
            Slot::Sliders => {
                let r = ui.add(icon_button(Icon::Sliders, IconButtonSize::Normal22, "Gain and gamma").at(Pos2::new(sx, y)));
                if !at_default(st) { p.circle_filled(Pos2::new(r.rect.max.x - 2.0 - 3.0, r.rect.min.y + 2.0 + 3.0), 3.0, t.text_link); }
                sliders_popover(ui, st, r, fo.sliders_open, forced.is_some(), &mut out);
            }
            Slot::Zoom => {
                let mut items = vec![Item::new("Fit").shortcut("H")];
                items.extend(["25 %", "50 %", "100 %", "200 %", "400 %"].map(Item::new));
                if compact { items.push(Item::new("1:1").shortcut("Ctrl+1")) }
                let sel = ZOOM_PCT.iter().chain([&400.0]).position(|z| (z - st.zoom).abs() < 0.5).map_or(usize::MAX, |i| i + 1);
                let mut zi = sel;
                // `repick`: choosing the current zoom again still re-applies it (re-centres the picture).
                let dd = Dropdown::new("vt-zoom", &mut zi, &items, 60.0).compact().repick().at(Pos2::new(x, y)).name("Zoom").tip("Zoom").display(&zoom_text).font(ValueFont::Mono).list_width(110.0);
                if dd.show(ui).changed() {
                    match zi {
                        0 => out.fit = true,
                        6 if compact => out.one_to_one = true,
                        i => { let z = [25.0, 50.0, 100.0, 200.0, 400.0][i - 1]; st.zoom = z; out.zoom_to = Some(z); out.changed = true }
                    }
                }
            }
            Slot::Fit => { if ui.add(icon_button(Icon::Fit, IconButtonSize::Normal22, "Fit (H or Ctrl+0)").at(Pos2::new(sx, y))).clicked() { out.fit = true } }
            Slot::One => {
                let r = Rect::from_min_size(Pos2::new(sx, y), Vec2::new(26.0, 22.0));
                let resp = ui.interact(r, ui.make_persistent_id("vt-one"), Sense::click());
                let vis = if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest };
                let l = look(t, Kind::Ghost, vis, false, true);
                if let Some(f) = l.fill { p.rect_filled(r, CornerRadius::same(3), f); }
                if resp.has_focus() && keyboard_focus(&ctx) {
                    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
                } else if let Some(b) = l.border {
                    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, b), StrokeKind::Inside);
                }
                let g = line(&p, "1:1", ty::mono(), 14.0, f32::INFINITY);
                paint_line(&p, r, g, 14.0, Align::Center, l.text, 0.0);
                resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, "1:1"));
                if resp.on_hover_text("1:1 (Ctrl+1)").clicked() { out.one_to_one = true }
            }
            Slot::Wipe => { if ui.add(toggle_button(Icon::Wipe, None, &mut st.wipe, "A/B wipe (W)").at(Pos2::new(sx, y))).clicked() { out.changed = true } }
            Slot::Focus => { if ui.add(toggle_button(Icon::Focus, None, &mut st.focus, "Focus (F)").at(Pos2::new(sx, y))).clicked() { out.changed = true } }
            Slot::Gap => {}
        }
        x += slot.w();
    }
    out
}

/// Medium / Compact: the `sliders` button opens a popover with both fields and Reset (the Wide group, in an overlay).
fn sliders_popover(ui: &mut Ui, st: &mut ViewerState, btn: Response, forced_open: bool, forced: bool, out: &mut ToolbarOut) {
    let t = tokens(ui);
    let ctx = ui.ctx().clone();
    let id = Id::NULL.with("vt-sliders-popover");
    let mut open = forced_open || ctx.data(|d| d.get_temp::<bool>(id)).unwrap_or(false);
    if !forced && btn.clicked() { open = !open }
    if !forced && open && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) { open = false }
    if open {
        let size = Vec2::new(8.0 + 22.0 + 48.0 + 4.0 + 22.0 + 48.0 + 4.0 + 18.0 + 8.0, 8.0 + 22.0 + 8.0);
        let (lr, _) = list_placement(btn.rect, ctx.content_rect(), size, btn.rect.width() - size.x, 2.0);
        let shadow = Shadow { offset: [0, 4], blur: 16, spread: 0, color: Color32::from_black_alpha(if t.bg_panel == DARK.bg_panel { 115 } else { 46 }) };
        Area::new(id.with("area")).order(Order::Foreground).fixed_pos(lr.min).constrain(false).show(&ctx, |ui| {
            let (r, _) = ui.allocate_exact_size(lr.size(), Sense::hover());
            let p = ui.painter().clone();
            p.add(shadow.as_shape(r, CornerRadius::same(4)));
            p.rect_filled(r, CornerRadius::same(4), t.bg_overlay);
            let (mut x, y) = (r.min.x + 8.0, r.min.y + 8.0);
            for slot in [Slot::GainIcon, Slot::Gain, Slot::Gap, Slot::GammaIcon, Slot::Gamma, Slot::Gap, Slot::Reset] {
                gain_gamma_slot(ui, st, slot, x, y, out);
                x += slot.w();
            }
            p.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border_overlay), StrokeKind::Inside);
        });
        if !forced {
            let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !lr.contains(p) && !btn.rect.contains(p)));
            if outside { open = false }
        }
    }
    if !forced { ctx.data_mut(|d| { d.insert_temp(id, open); }) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Context, RawInput};

    fn ctx() -> Context {
        let ctx = Context::default();
        crate::ui::fonts::install(&ctx);
        ctx
    }

    #[test]
    fn viewer_letters_ignored_while_list_open() {
        use egui::Event;
        let key = |key, modifiers| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers };
        for letter in [Key::R, Key::G, Key::B, Key::A, Key::Y, Key::K, Key::W, Key::F, Key::H] {
            for shortcuts_first in [true, false] {
                let ctx = ctx();
                let label = format!("{letter:?}");
                let items = [Item::new("Input"), Item::new(&label)];
                let mut selected = 0;
                let mut st = ViewerState::default();
                let mut frame = |events: Vec<Event>, check: bool| {
                    let mut output = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| {
                        if check && shortcuts_first {
                            let out = handle_shortcuts(&ctx, &mut st, true);
                            assert!(!out.changed && !out.fit, "{letter:?}");
                        }
                        let r = Dropdown::new("letters", &mut selected, &items, 120.0).show(ui);
                        if !r.has_focus() { r.request_focus(); }
                        if check && !shortcuts_first {
                            let out = handle_shortcuts(&ctx, &mut st, true);
                            assert!(!out.changed && !out.fit, "{letter:?}");
                        }
                        if check { assert!(ctx.input(|i| i.key_pressed(letter)), "letter belongs to the list"); }
                    });
                    output.textures_delta.clear();
                };
                frame(vec![], false);
                frame(vec![key(Key::ArrowDown, Modifiers::ALT)], false);
                frame(vec![key(letter, Modifiers::NONE), Event::Text(label.to_lowercase())], true);
                frame(vec![key(Key::Enter, Modifiers::NONE)], false);
                assert_eq!(selected, 1, "{letter:?} type-ahead still selects its row");
                assert_eq!(st, ViewerState::default());
            }
        }
    }

    #[test]
    fn viewer_letters_yield_to_menu_focus() {
        let ctx = ctx();
        let id = Id::unique("focused-menu");
        let mut nav = super::super::menus::Nav::default();
        nav.focus = true;
        ctx.data_mut(|d| d.insert_temp(id, nav));
        let mut st = ViewerState::default();
        for focused in [true, false] {
            if !focused { ctx.data_mut(|d| d.remove::<super::super::menus::Nav>(id)); }
            let mut output = ctx.run_ui(RawInput { events: vec![egui::Event::Key {
                key: Key::R, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE,
            }], ..Default::default() }, |_| {
                assert_eq!(handle_shortcuts(&ctx, &mut st, true).changed, !focused);
            });
            output.textures_delta.clear();
        }
        assert_eq!(st.channel, Channel::R, "shortcuts resume when the menu releases focus");
    }

    /// pad 8 + View 96 + 8 + Overlay 96 + gap 16 + Channel 56 + Background 36 + info 22 + sliders 22 + zoom 60 + 4 + Fit 22 +
    /// A/B 22 + 4 + Focus 22 + five dividers × 17 + pad 8 = 587 ≤ 619 (§7.2).
    #[test]
    fn compact_toolbar_fits_587_of_619() {
        assert_eq!(compact_min_width(), 587.0);
        assert!(compact_min_width() <= 619.0);
        let dividers = cluster(Layout::Compact, &[]).iter().filter(|s| **s == Slot::Div).count();
        assert_eq!(dividers, 5);
        // Laid out at 619: the right groups start 48 px (16 + 32 spare) after Overlay.
        let start = 619.0 - 8.0 - cluster_width(Layout::Compact, &[]);
        assert_eq!(start - (8.0 + 96.0 + 8.0 + 96.0), 48.0);
        // Nothing the Compact class must keep is dropped.
        let c = cluster(Layout::Compact, &[]);
        for s in [Slot::Channel, Slot::Bg, Slot::Zoom, Slot::Fit, Slot::Wipe, Slot::Focus, Slot::Info, Slot::Sliders] { assert!(c.contains(&s), "{s:?}") }
        assert!(!c.contains(&Slot::One) && !c.contains(&Slot::Gain));
    }

    fn accesskit_frame(has_alpha: bool) -> Vec<(String, bool, Option<String>)> {
        let ctx = ctx();
        ctx.enable_accesskit();
        let mut st = ViewerState::default();
        let mut out = ctx.run_ui(RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 200.0))), ..Default::default() }, |ui| {
            viewer_toolbar(ui, &mut st, Layout::Wide, has_alpha, "EXR · half · 3200×1800", false);
        });
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.expect("accesskit tree");
        update.nodes.iter().filter(|(_, n)| n.role() == Role::ComboBox).map(|(_, n)| (n.label().unwrap_or("").to_owned(), n.is_disabled(), n.description().map(str::to_owned))).collect()
    }

    /// Background is disabled with the reason where the view has no transparency, enabled where it has.
    #[test]
    fn background_disabled_without_alpha() {
        let none = accesskit_frame(false);
        let bg = none.iter().find(|n| n.0 == "Background").expect("Background combo");
        assert!(bg.1, "disabled without alpha");
        assert_eq!(bg.2.as_deref(), Some("No transparency in this view"));
        let some = accesskit_frame(true);
        assert!(!some.iter().find(|n| n.0 == "Background").unwrap().1);
        assert!(!some.iter().find(|n| n.0 == "Channel").unwrap().1, "Channel never disables");
        assert!(View::Comp.has_alpha() && View::Fg.has_alpha() && View::Processed.has_alpha() && !View::Input.has_alpha() && !View::Matte.has_alpha());
        assert_eq!(Background::Checker.next(), Background::Black);
    }

    #[test]
    fn readout_flips_when_pointer_near() {
        let viewer = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(520.0, 200.0));
        // Picture reaches the bottom (Wide 16:9): the strip overlays its corner at 4 px from the bottom-left …
        let picture = Rect::from_min_size(Pos2::new(250.0, 138.0), Vec2::new(220.0, 162.0));
        let far = readout_rect(viewer, picture, Some(Pos2::new(400.0, 190.0)), 342.0);
        assert_eq!((far.min.x, far.min.y, far.height()), (104.0, 276.0, 20.0));
        // … and flips to the bottom-right when the pointer is within 48 px of it.
        let near = readout_rect(viewer, picture, Some(Pos2::new(140.0, 260.0)), 342.0);
        assert_eq!((near.max.x, near.max.y), (616.0, 296.0));
        let edge = readout_rect(viewer, picture, Some(Pos2::new(104.0 + 342.0 + 47.0, 286.0)), 342.0);
        assert!(edge.min.x > 104.0, "47 px away flips");
        let out = readout_rect(viewer, picture, Some(Pos2::new(104.0 + 342.0 + 49.0, 286.0)), 342.0);
        assert_eq!(out.min.x, 104.0, "49 px away stays");
        // With ≥ 24 px of surround below the picture the strip sits in the surround and never flips.
        let short = Rect::from_min_size(Pos2::new(100.0, 100.0), Vec2::new(520.0, 150.0));
        assert_eq!(readout_rect(viewer, short, Some(Pos2::new(140.0, 280.0)), 342.0).min.x, 104.0);
        // Hidden without a pointer; Compact drops x / y.
        let ctx = ctx();
        let mut got = (Some(Rect::NOTHING), None, 0.0, 0.0);
        let mut out = ctx.run_ui(RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 400.0))), ..Default::default() }, |ui| {
            got.0 = pixel_readout(ui, viewer, picture, None, [0.0; 4], (0, 0), false);
            got.1 = pixel_readout(ui, viewer, picture, Some(Pos2::new(400.0, 190.0)), [0.5, 0.4, 0.1, 1.0], (1204, 388), false);
            got.2 = got.1.map_or(0.0, |r| r.width());
            got.3 = pixel_readout(ui, viewer, picture, Some(Pos2::new(400.0, 190.0)), [0.5, 0.4, 0.1, 1.0], (1204, 388), true).map_or(0.0, |r| r.width());
        });
        out.textures_delta.clear();
        assert!(got.0.is_none() && got.1.is_some());
        assert!(got.3 < got.2 - 50.0, "compact is narrower: {} vs {}", got.3, got.2);
    }

    #[test]
    fn gamma_shows_three_decimals_gain_two() {
        let (gain, gamma) = (gain_spec(), gamma_spec());
        assert_eq!(gain.format(1.0), "1.00");
        assert_eq!(gain.format(1.5), "1.50");
        assert_eq!(gamma.format(1.0), "1.000");
        assert_eq!(gamma.format(1.001), "1.001");
        assert_eq!((gain.min, gain.max, gain.default), (0.06, 16.0, 1.0));
        assert_eq!((gamma.min, gamma.max, gamma.default), (0.2, 5.0, 1.0));
        assert!(!at_default(&ViewerState { gamma: 1.001, ..Default::default() }) && at_default(&ViewerState::default()));
    }
}
