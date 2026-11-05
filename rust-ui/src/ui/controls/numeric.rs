//! Numeric field + optional slider (bible §5.7, §7.2 item 4 Gain / Gamma, §12 "Numeric + slider"): one custom widget,
//! not `DragValue`. 56×22 mono field (any width via `.width`), Nuke-style drag with a 3 px click threshold, 1 step per
//! 4 px, Alt ÷10 / Shift ×10; click enters text editing; Esc reverts; Mixed "—"; Auto; AccessKit SpinButton.
//! Pointer lock only where `pointer_lock_ok()` (Windows / macOS); otherwise a visible no-grab drag, never warped.
use super::{keyboard_focus, line, paint_line};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{
    self, viewport::CursorGrab, Align, Context, CornerRadius, CursorIcon, Id, Key, Layout, Modifiers, Pos2, Rect, Response, Sense,
    Stroke, StrokeKind, Ui, UiBuilder, ViewportCommand, WidgetInfo,
};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// Hard limits, step table and display of one property (§5.7 "Step per kind").
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NumSpec {
    pub default: f64,
    pub min: f64,
    pub max: f64,
    /// One drag step (per 4 px); `fine` with Alt, `coarse` with Shift.
    pub step: f64,
    pub fine: f64,
    pub coarse: f64,
    pub decimals: usize,
    /// Slider range when narrower than the hard range (never clamps typed or dragged values).
    pub soft: Option<(f64, f64)>,
}

impl NumSpec {
    /// Custom: fine = step / 10, coarse = step × 10.
    pub fn new(default: f64, min: f64, max: f64, step: f64, decimals: usize) -> Self {
        NumSpec { default, min, max, step, fine: step / 10.0, coarse: step * 10.0, decimals, soft: None }
    }
    /// Counts, iterations, frames: 1 / 1 / 10, shown `12`.
    pub fn integer(default: i64, min: i64, max: i64) -> Self {
        NumSpec { default: default as f64, min: min as f64, max: max as f64, step: 1.0, fine: 1.0, coarse: 10.0, decimals: 0, soft: None }
    }
    pub fn frames(default: i64, min: i64, max: i64) -> Self { Self::integer(default, min, max) }
    /// Strength, opacity, threshold: 0–1, 0.01 / 0.001 / 0.1, shown `0.350`.
    pub fn unit_float(default: f64) -> Self { Self::new(default, 0.0, 1.0, 0.01, 3) }
    /// Pixels: 1 / (0.1 if fractions else 1) / 10, shown `4` or `2.5`.
    pub fn pixels(default: f64, min: f64, max: f64, fractions: bool) -> Self {
        NumSpec { default, min, max, step: 1.0, fine: if fractions { 0.1 } else { 1.0 }, coarse: 10.0, decimals: fractions as usize, soft: None }
    }
    /// Viewer Gain (§7.2): 0.06–16, 0.1 / 0.01 / 1, two decimals.
    pub fn gain() -> Self { NumSpec { default: 1.0, min: 0.06, max: 16.0, step: 0.1, fine: 0.01, coarse: 1.0, decimals: 2, soft: None } }
    /// Viewer Gamma (§7.2): 0.2–5, 0.01 / 0.001 / 0.1, three decimals.
    pub fn gamma() -> Self { NumSpec { default: 1.0, min: 0.2, max: 5.0, step: 0.01, fine: 0.001, coarse: 0.1, decimals: 3, soft: None } }
    pub fn soft(mut self, lo: f64, hi: f64) -> Self { self.soft = Some((lo, hi)); self }

    fn range(&self) -> (f64, f64) { self.soft.unwrap_or((self.min, self.max)) }
    fn step_for(&self, m: Modifiers) -> f64 { if m.alt { self.fine } else if m.shift { self.coarse } else { self.step } }
    /// Values live on the finest step grid (clean of float noise) and inside the hard range.
    fn snap(&self, x: f64) -> f64 {
        let q = self.fine.min(self.step).max(1e-9);
        (((x / q).round() * q) * 1e9).round() / 1e9
    }
    fn clamp(&self, x: f64) -> f64 { x.clamp(self.min, self.max) }
    pub fn is_default(&self, v: f64) -> bool { (v - self.default).abs() < 0.5 * 10f64.powi(-(self.decimals as i32)) }
    pub fn format(&self, v: f64) -> String {
        let s = format!("{:.*}", self.decimals, v);
        if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') { s[1..].to_string() } else { s }
    }
}

/// Start-up capability for pointer lock (§12): Windows and macOS true; X11, Wayland, unknown false (winit returns
/// `NotSupported` for `Locked` on X11; eframe reports no result, so it is never inferred from motion).
pub fn pointer_lock_supported(os: &str) -> bool { matches!(os, "windows" | "macos") }

static POINTER_LOCK: OnceLock<bool> = OnceLock::new();
/// The integration may set the flag once at start-up; otherwise it comes from the platform.
pub fn set_pointer_lock_ok(ok: bool) { let _ = POINTER_LOCK.set(ok); }
pub fn pointer_lock_ok() -> bool { *POINTER_LOCK.get_or_init(|| pointer_lock_supported(std::env::consts::OS)) }

/// Look to force in the specimen (the same paint path as the live states).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NumState { Rest, Hover, Dragging, Editing, Focus, Error }

#[derive(Clone, Default)]
struct St {
    pressed: bool,
    dragging: bool,
    locked: bool,
    swallowed: bool,
    moved: f32,
    start: f64,
    raw: f64,
    editing: bool,
    select_all: bool,
    /// Frames left to claim focus for the text editor (egui's click handling re-focuses the field on release).
    grace: u8,
    text: String,
    flash_until: f64,
    mixed_cleared: bool,
    sl_drag: bool,
    committed: bool,
}

const ACTIVE: &str = "keyloom-numeric-active";

/// The field currently dragging: its id, the last pass it was drawn in, and whether the pointer is locked.
#[derive(Clone, Default)]
struct Active { id: Option<Id>, pass: u64, locked: bool }

fn release_grab(ctx: &Context) {
    ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::None));
    ctx.send_viewport_cmd(ViewportCommand::CursorVisible(true));
}

/// Registered once per context: a drag whose widget vanished (not drawn this pass) releases the grab the same frame.
fn ensure_guard(ctx: &Context) {
    let k = Id::NULL.with("keyloom-numeric-guard");
    if ctx.data(|d| d.get_temp::<bool>(k)).is_some() { return }
    ctx.data_mut(|d| d.insert_temp(k, true));
    ctx.on_end_pass("keyloom-numeric", Arc::new(|ui: &mut Ui| {
        let ctx = ui.ctx().clone();
        let a = ctx.data(|d| d.get_temp::<Active>(Id::NULL.with(ACTIVE))).unwrap_or_default();
        if let (Some(id), true) = (a.id, a.pass != ctx.cumulative_pass_nr()) {
            if a.locked { release_grab(&ctx) }
            ctx.data_mut(|d| { d.insert_temp(Id::NULL.with(ACTIVE), Active::default()); d.remove_temp::<St>(id); });
        }
    }));
}

/// True in the frame a drag was released or an edit committed (the moment to apply a Mixed result to all shots).
pub fn committed(ctx: &Context, id: Id) -> bool { ctx.data(|d| d.get_temp::<St>(id)).is_some_and(|s| s.committed) }

/// Double-click on the property label resets to the default (§5.7). Returns true when it did.
pub fn reset_on_double_click(label: &Response, v: &mut f64, spec: &NumSpec) -> bool {
    if label.double_clicked() && *v != spec.default { *v = spec.default; true } else { false }
}

pub struct NumericField<'a> {
    salt: Id,
    v: &'a mut f64,
    spec: &'a NumSpec,
    slider: bool,
    width: f32,
    mixed: bool,
    disabled: Option<&'a str>,
    unit: Option<&'a str>,
    name: &'a str,
    auto: Option<&'a mut bool>,
    tick: bool,
    lock: Option<bool>,
    force: Option<NumState>,
}

/// `numeric_field(ui, id, &mut value, &spec)`: a 56×22 field in flow layout; returns the field's response
/// (`changed()` while the value changes, also live during a drag).
pub fn numeric_field(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, v: &mut f64, spec: &NumSpec) -> Response { NumericField::new(id, v, spec).show(ui) }

impl<'a> NumericField<'a> {
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, v: &'a mut f64, spec: &'a NumSpec) -> Self {
        NumericField { salt: Id::NULL.with(id), v, spec, slider: false, width: 56.0, mixed: false, disabled: None, unit: None, name: "", auto: None, tick: true, lock: None, force: None }
    }
    /// Slider after the field (4 px gap), filling the available width minus the unit.
    pub fn slider(mut self, on: bool) -> Self { self.slider = on; self }
    /// Field width (56 default; 48 for the viewer Gain / Gamma).
    pub fn width(mut self, w: f32) -> Self { self.width = w; self }
    /// Several shots differ: shows "—"; a drag starts from `*v` (the first selected shot's value).
    pub fn mixed(mut self, on: bool) -> Self { self.mixed = on; self }
    /// Disabled, with the reason as tooltip / AccessKit description.
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    pub fn unit(mut self, unit: &'a str) -> Self { self.unit = Some(unit); self }
    /// Accessible name (the visible property label).
    pub fn name(mut self, name: &'a str) -> Self { self.name = name; self }
    /// Value can be Auto: `*auto` true shows "Auto"; typing a number clears it, an emptied field sets it.
    pub fn auto(mut self, auto: &'a mut bool) -> Self { self.auto = Some(auto); self }
    /// Draw the default-value tick under the slider (default true).
    pub fn default_tick(mut self, on: bool) -> Self { self.tick = on; self }
    /// Override `pointer_lock_ok()` for this field (tests).
    pub fn pointer_lock(mut self, ok: bool) -> Self { self.lock = Some(ok); self }
    /// Specimen / test only: draw this look and skip all interaction.
    pub fn forced(mut self, s: NumState) -> Self { self.force = Some(s); self }

    pub fn show(self, ui: &mut Ui) -> Response {
        let NumericField { salt, v, spec, slider, width, mixed, disabled, unit, name, auto, tick, lock, force } = self;
        let t = tokens(ui);
        let ctx = ui.ctx().clone();
        ensure_guard(&ctx);
        let enabled = ui.is_enabled() && disabled.is_none();
        let id = ui.scope_id().with(salt);
        let mut st: St = ctx.data(|d| d.get_temp(id)).unwrap_or_default();
        st.committed = false;
        let auto_on = auto.as_deref().copied().unwrap_or(false);

        // Layout: [field width][4][slider][4][unit]
        let uw = unit.map_or(0.0, |u| line(ui.painter(), u, ty::small(), 14.0, f32::INFINITY).size().x + 1.0 / 64.0);
        let uextra = if unit.is_some() { 4.0 + uw } else { 0.0 };
        let sl_w = if slider { (ui.available_width() - width - 4.0 - uextra).max(24.0) } else { 0.0 };
        let total = width + if slider { 4.0 + sl_w } else { 0.0 } + uextra;
        let (all, _) = ui.allocate_exact_size(egui::vec2(total, 22.0), Sense::hover());
        let field = Rect::from_min_size(all.min.round(), egui::vec2(width, 22.0));
        let srect = Rect::from_min_size(egui::pos2(all.min.x + width + 4.0, all.min.y), egui::vec2(sl_w, 22.0));

        let resp = ui.interact(field, id, if enabled && force.is_none() { Sense::click_and_drag() } else { Sense::hover() });
        let mut changed = false;
        let lock_ok = lock.unwrap_or_else(pointer_lock_ok);

        if enabled && force.is_none() {
            let now = ctx.input(|i| i.time);
            let (down, pressed, released, mods, delta, motion, esc_p, unfocused) = ctx.input(|i| (
                i.pointer.primary_down(), i.pointer.primary_pressed(), i.pointer.primary_released(), i.modifiers,
                i.pointer.delta(), i.pointer.motion(), i.key_pressed(Key::Escape), i.viewport().focused == Some(false),
            ));
            let end_drag = |ctx: &Context, st: &mut St| {
                if st.locked { release_grab(ctx) }
                st.locked = false;
                st.dragging = false;
                ctx.data_mut(|d| d.insert_temp(Id::NULL.with(ACTIVE), Active::default()));
            };
            if !st.editing {
                // Esc reverts a drag (field or slider) to the value at press; the rest of that press is ignored.
                if (st.dragging || st.sl_drag) && esc_p {
                    ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
                    *v = st.start;
                    st.mixed_cleared = false;
                    st.sl_drag = false;
                    st.pressed = false;
                    st.swallowed = true;
                    end_drag(&ctx, &mut st);
                    changed = true;
                }
                if pressed && resp.hovered() { st = St { pressed: true, start: *v, raw: *v, mixed_cleared: st.mixed_cleared, flash_until: st.flash_until, ..St::default() }; }
                if st.pressed && down && !st.swallowed {
                    let dx = if st.locked { motion.map_or(delta.x, |m| m.x) } else { delta.x };
                    let px = if st.dragging { dx } else {
                        st.moved += dx;
                        if st.moved.abs() >= 3.0 {
                            st.dragging = true;
                            st.mixed_cleared = true;
                            if lock_ok {
                                ctx.send_viewport_cmd(ViewportCommand::CursorGrab(CursorGrab::Locked));
                                ctx.send_viewport_cmd(ViewportCommand::CursorVisible(false));
                                st.locked = true;
                            }
                            st.moved
                        } else { 0.0 }
                    };
                    if st.dragging && px != 0.0 {
                        st.raw = spec.clamp(st.raw + px as f64 / 4.0 * spec.step_for(mods));
                        let nv = spec.snap(st.raw);
                        if nv != *v { *v = nv; changed = true }
                    }
                    if st.dragging { let pass = ctx.cumulative_pass_nr(); ctx.data_mut(|d| d.insert_temp(Id::NULL.with(ACTIVE), Active { id: Some(id), pass, locked: st.locked })); }
                    // Focus loss commits what is shown and ends the drag.
                    if st.dragging && unfocused { st.committed = true; st.pressed = false; end_drag(&ctx, &mut st); }
                }
                if st.pressed && (released || !down) {
                    if st.dragging { st.committed = true; end_drag(&ctx, &mut st) }
                    else if !st.swallowed && resp.hovered() {
                        st.editing = true;
                        st.select_all = true;
                        st.grace = 3;
                        st.start = *v;
                        st.text = if (mixed && !st.mixed_cleared) || auto_on { String::new() } else { spec.format(*v) };
                    }
                    st.pressed = false;
                }
                if !down { st.swallowed = false }
                // Keyboard path: Enter on the focused field starts typing.
                if resp.has_focus() && !st.pressed && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                    st.editing = true;
                    st.select_all = true;
                        st.grace = 3;
                    st.start = *v;
                    st.text = if (mixed && !st.mixed_cleared) || auto_on { String::new() } else { spec.format(*v) };
                }
                if resp.hovered() || st.dragging { ctx.set_cursor_icon(CursorIcon::ResizeHorizontal) }
            }
            if st.editing {
                let text_id = id.with("edit");
                if ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                    st.editing = false;
                    ctx.memory_mut(|m| m.request_focus(id));
                } else {
                    if st.select_all {
                        let mut s = egui::text_edit::TextEditState::load(&ctx, text_id).unwrap_or_default();
                        let n = st.text.chars().count();
                        s.cursor.set_char_range(Some(egui::text::CCursorRange::two(egui::text::CCursor::new(0), egui::text::CCursor::new(n))));
                        s.store(&ctx, text_id);
                        ctx.memory_mut(|m| m.request_focus(text_id));
                        st.select_all = false;
                    }
                    if st.grace > 0 && ctx.memory(|m| m.focused()) != Some(text_id) { ctx.memory_mut(|m| m.request_focus(text_id)); st.grace -= 1; }
                    let inner = field.shrink2(egui::vec2(7.0, 1.0));
                    let r = ui.scope_builder(UiBuilder::new().max_rect(inner).layout(Layout::left_to_right(Align::Center)), |ui| {
                        ui.visuals_mut().selection.bg_fill = t.text_selection;
                        ui.visuals_mut().selection.stroke = Stroke::NONE;
                        ui.visuals_mut().text_cursor.stroke = Stroke::new(1.0, t.text_primary);
                        egui::TextEdit::singleline(&mut st.text)
                            .id(text_id)
                            .font(ty::frame_field())
                            .text_color(t.text_primary)
                            .frame(egui::Frame::NONE)
                            .margin(egui::Margin::ZERO)
                            .desired_width(inner.width())
                            .align(egui::Align2::LEFT_CENTER)
                            .show(ui)
                            .response
                    }).inner;
                    if r.lost_focus() {
                        st.editing = false;
                        let s = st.text.trim().replace(',', ".");
                        if s.is_empty() {
                            if let Some(a) = auto { if !*a { *a = true; changed = true } }
                            else if !(mixed && !st.mixed_cleared) { st.flash_until = now + 0.6; ctx.request_repaint_after(Duration::from_millis(600)); }
                        } else if let Some(x) = s.parse::<f64>().ok().filter(|x| x.is_finite()) {
                            let x = spec.clamp(x);
                            if x != *v || (mixed && !st.mixed_cleared) || auto_on { *v = x; changed = true }
                            if let Some(a) = auto { *a = false }
                            st.mixed_cleared = true;
                            st.committed = true;
                        } else {
                            // Invalid text reverts (the value never changed) and flashes the error border for 600 ms.
                            st.flash_until = now + 0.6;
                            ctx.request_repaint_after(Duration::from_millis(600));
                        }
                        if ctx.input(|i| i.key_pressed(Key::Enter)) { ctx.memory_mut(|m| m.request_focus(id)) }
                    }
                }
            }
        }

        // Slider: absolute position -> value on the soft range; Esc reverts; never a Tab stop.
        let (lo, hi) = spec.range();
        let frac_of = |x: f64| if hi > lo { ((x - lo) / (hi - lo)).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let mut sl_hover = false;
        if slider && sl_w > 0.0 && enabled && force.is_none() {
            let sresp = ui.interact(srect, id.with("slider"), Sense::CLICK | Sense::DRAG);
            sl_hover = sresp.hovered() || st.sl_drag;
            let (pressed, down, pos) = ctx.input(|i| (i.pointer.primary_pressed(), i.pointer.primary_down(), i.pointer.latest_pos()));
            if pressed && sresp.hovered() && !st.editing { st.sl_drag = true; st.start = *v; st.mixed_cleared = true; }
            if st.sl_drag && down {
                if let Some(p) = pos {
                    let f = ((p.x - srect.min.x) / srect.width()).clamp(0.0, 1.0) as f64;
                    let nv = spec.clamp(spec.snap(lo + f * (hi - lo)));
                    if nv != *v { *v = nv; changed = true }
                }
            }
            if st.sl_drag && !down { st.sl_drag = false; st.committed = true }
        }

        // ---- paint (the same path for live and forced looks) ----
        let live = if st.dragging { NumState::Dragging } else if st.editing { NumState::Editing } else if enabled && resp.has_focus() && keyboard_focus(&ctx) { NumState::Focus }
            else if enabled && resp.hovered() { NumState::Hover } else { NumState::Rest };
        let look = force.unwrap_or(live);
        let flashing = look != NumState::Editing && ctx.input(|i| i.time) + 0.01 < st.flash_until;
        let show_mixed = mixed && !st.mixed_cleared;
        if ui.is_rect_visible(all) {
            let p = ui.painter();
            let rad = CornerRadius::same(3);
            let (fill, border) = if !enabled { (t.bg_panel, t.border_divider) } else {
                match look {
                    NumState::Hover => (t.bg_sunken, t.border_hover),
                    NumState::Dragging => (t.bg_hover, t.accent_bright),
                    _ => (t.bg_sunken, t.border_control),
                }
            };
            p.rect_filled(field, rad, fill);
            if flashing || look == NumState::Error { p.rect_stroke(field, rad, Stroke::new(1.0, t.error), StrokeKind::Inside); }
            else if matches!(look, NumState::Editing | NumState::Focus) && enabled { p.rect_stroke(field, rad, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
            else { p.rect_stroke(field, rad, Stroke::new(1.0, border), StrokeKind::Inside); }

            let editing_live = st.editing && force.is_none();
            if !editing_live {
                let inner = field.shrink2(egui::vec2(7.0, 1.0));
                let changed_from_default = !spec.is_default(*v) && !show_mixed && !auto_on;
                let primary_look = matches!(look, NumState::Dragging | NumState::Editing);
                if show_mixed {
                    let g = line(p, "—", ty::frame_field(), 16.0, f32::INFINITY);
                    paint_line(p, inner, g, 16.0, Align::Center, if enabled { t.text_tertiary } else { t.text_disabled }, 0.0);
                } else if auto_on && look != NumState::Editing {
                    let g = line(p, "Auto", ty::value(), 16.0, inner.width());
                    paint_line(p, inner, g, 16.0, Align::Min, if enabled { t.text_tertiary } else { t.text_disabled }, 0.0);
                } else {
                    let s = spec.format(*v);
                    let color = if !enabled { t.text_disabled } else if changed_from_default && !primary_look { t.text_link } else { t.text_primary };
                    let g = line(p, &s, ty::frame_field(), 16.0, f32::INFINITY);
                    if look == NumState::Editing {
                        // Forced look: value selected (select-all on entry) with a caret after it.
                        let w = g.size().x;
                        let sel = Rect::from_min_size(egui::pos2(inner.min.x, field.min.y + 3.0), egui::vec2(w, 15.0));
                        p.rect_filled(sel, 0.0, t.text_selection);
                        p.rect_filled(Rect::from_min_size(egui::pos2((inner.min.x + w).round(), field.min.y + 3.0), egui::vec2(1.0, 15.0)), 0.0, t.text_primary);
                        paint_line(p, inner, g, 16.0, Align::Min, color, 0.0);
                    } else {
                        // A value wider than the padded box (Gamma 1.000 at 48) keeps its left pad and overflows to the right pad.
                        paint_line(p, inner, g.clone(), 16.0, if g.size().x > inner.width() { Align::Min } else { Align::Max }, color, 0.0);
                    }
                }
            }

            if slider && sl_w > 0.0 {
                let hov = sl_hover || look == NumState::Hover;
                let drag = st.sl_drag || look == NumState::Dragging;
                let f = frac_of(*v);
                let trk = Rect::from_min_size(egui::pos2(srect.min.x, srect.min.y + 9.0), egui::vec2(sl_w, 4.0));
                p.rect_filled(trk, 2.0, t.bg_sunken);
                p.rect_stroke(trk, 2.0, Stroke::new(1.0, if enabled { t.border_control } else { t.border_divider }), StrokeKind::Inside);
                if f > 0.0 || !enabled {
                    let fill_c = if !enabled { t.text_disabled } else if drag { t.accent_bright } else { t.text_tertiary };
                    p.rect_filled(Rect::from_min_size(trk.min, egui::vec2(sl_w * f, 4.0)), 2.0, fill_c);
                }
                if tick && enabled && (lo..=hi).contains(&spec.default) {
                    let x = (srect.min.x + sl_w * frac_of(spec.default)).round();
                    p.rect_filled(Rect::from_min_size(egui::pos2(x, srect.min.y + 15.0), egui::vec2(1.0, 6.0)), 0.0, t.text_tertiary);
                }
                let (hw, hh, top) = if hov && enabled { (10.0, 16.0, 3.0) } else { (8.0, 14.0, 4.0) };
                let hx = (srect.min.x + sl_w * f - hw / 2.0).clamp(srect.min.x, srect.max.x - hw);
                let h = Rect::from_min_size(egui::pos2(hx, srect.min.y + top), egui::vec2(hw, hh));
                p.rect_filled(h, 2.0, if enabled { t.text_primary } else { t.text_disabled });
                p.rect_stroke(h, 2.0, Stroke::new(1.0, t.bg_app), StrokeKind::Inside);
            }
            if let Some(u) = unit {
                let g = line(p, u, ty::small(), 16.0, f32::INFINITY);
                paint_line(p, Rect::from_min_size(egui::pos2(all.max.x - uw, all.min.y), egui::vec2(uw, 22.0)), g, 16.0, Align::Min, t.text_tertiary, 0.0);
            }
        }

        // AccessKit: one SpinButton (value, min, max, step); the slider rail is not a second focus stop.
        let shown = *v;
        let (mn, mx, stp) = (spec.min, spec.max, spec.step);
        resp.widget_info(|| WidgetInfo { label: Some(name.to_string()), ..WidgetInfo::drag_value(enabled, shown) });
        ctx.accesskit_node_builder(resp.id, |n| {
            n.set_min_numeric_value(mn);
            n.set_max_numeric_value(mx);
            n.set_numeric_value_step(stp);
            if let Some(r) = disabled { n.set_description(r) }
        });
        ctx.data_mut(|d| d.insert_temp(id, st));
        let _: Option<Pos2> = None;
        let mut resp = resp;
        if let Some(r) = disabled { resp = resp.on_hover_text(r).on_disabled_hover_text(r) }
        if changed { resp.mark_changed() }
        resp
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, RawInput};

    struct H { ctx: Context, t: f64, v: f64, spec: NumSpec, lock: bool, cmds: Vec<ViewportCommand>, delay: Duration, id: Id, show: bool }

    impl H {
        fn new(v: f64, spec: NumSpec) -> Self {
            let ctx = Context::default();
            crate::ui::fonts::install(&ctx);
            H { ctx, t: 0.0, v, spec, lock: false, cmds: vec![], delay: Duration::MAX, id: Id::NULL, show: true }
        }
        fn frame(&mut self, mut events: Vec<Event>, modifiers: Modifiers) {
            self.t += 0.05;
            events.insert(0, Event::ModifiersChanged(modifiers));
            let raw = RawInput {
                time: Some(self.t),
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, egui::vec2(300.0, 100.0))),
                events,
                ..Default::default()
            };
            let (v, spec, lock, show) = (&mut self.v, self.spec, self.lock, self.show);
            let mut id = self.id;
            let mut full = self.ctx.run_ui(raw, |ui| {
                if !show { return }
                ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(200.0, 22.0))), |ui| {
                    id = NumericField::new("n", v, &spec).pointer_lock(lock).show(ui).id;
                });
            });
            full.textures_delta.clear();
            self.id = id;
            self.delay = full.viewport_output.get(&egui::ViewportId::ROOT).map_or(Duration::MAX, |r| r.repaint_delay);
            if let Some(r) = full.viewport_output.get(&egui::ViewportId::ROOT) { self.cmds.extend(r.commands.iter().cloned()) }
        }
        fn mv(&mut self, x: f32, m: Modifiers) { self.frame(vec![Event::PointerMoved(egui::pos2(x, 21.0))], m) }
        fn btn(&mut self, pressed: bool, m: Modifiers) {
            self.frame(vec![Event::PointerButton { pos: egui::pos2(30.0, 21.0), button: PointerButton::Primary, pressed, modifiers: m }], m)
        }
        fn key(&mut self, k: Key) { self.frame(vec![Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }], Modifiers::NONE) }
        fn text(&mut self, s: &str) { self.frame(vec![Event::Text(s.into())], Modifiers::NONE) }
        fn idle(&mut self) { self.frame(vec![], Modifiers::NONE) }
        fn press(&mut self, m: Modifiers) { self.mv(30.0, m); self.mv(30.0, m); self.btn(true, m); }
        fn st(&self) -> St { self.ctx.data(|d| d.get_temp::<St>(self.id)).unwrap_or_default() }
    }

    fn drag(spec: NumSpec, start: f64, px: f32, m: Modifiers) -> (f64, bool) {
        let mut h = H::new(start, spec);
        h.press(m);
        h.mv(30.0 + px, m);
        h.mv(30.0 + px, m);
        h.btn(false, m);
        h.idle();
        (h.v, h.st().editing)
    }

    #[test]
    fn numeric_drag_threshold_and_steps() {
        let s = NumSpec::unit_float(0.5);
        // 2 px = a click (enters editing, value untouched); 8 px = 2 steps of 0.01.
        assert_eq!(drag(s, 0.5, 2.0, Modifiers::NONE), (0.5, true));
        assert_eq!(drag(s, 0.5, 8.0, Modifiers::NONE), (0.52, false));
        assert_eq!(drag(s, 0.5, -8.0, Modifiers::NONE), (0.48, false));
        // Alt = fine (0.001), Shift = coarse (0.1); clamped to the hard range.
        assert_eq!(drag(s, 0.5, 8.0, Modifiers::ALT), (0.502, false));
        assert_eq!(drag(s, 0.5, 8.0, Modifiers::SHIFT), (0.7, false));
        assert_eq!(drag(s, 0.9, 40.0, Modifiers::SHIFT), (1.0, false));
        // Integer: Alt is no finer than 1.
        assert_eq!(drag(NumSpec::integer(5, 0, 100), 5.0, 8.0, Modifiers::ALT), (7.0, false));
        assert_eq!(drag(NumSpec::integer(5, 0, 100), 5.0, 8.0, Modifiers::SHIFT), (25.0, false));
        // Pixels with fractions: Alt = 0.1.
        assert_eq!(drag(NumSpec::pixels(2.0, 0.0, 100.0, true), 2.0, 8.0, Modifiers::ALT), (2.2, false));
    }

    #[test]
    fn numeric_esc_reverts_drag_and_edit() {
        let s = NumSpec::unit_float(0.5);
        // Esc during a drag restores the value at press; releasing afterwards does not start editing.
        let mut h = H::new(0.5, s);
        h.press(Modifiers::NONE);
        h.mv(38.0, Modifiers::NONE);
        assert_eq!(h.v, 0.52);
        h.key(Key::Escape);
        assert_eq!(h.v, 0.5);
        h.mv(60.0, Modifiers::NONE);
        assert_eq!(h.v, 0.5, "motion after Esc is ignored until release");
        h.btn(false, Modifiers::NONE);
        h.idle();
        assert!(!h.st().editing);
        // Esc in the text editor leaves the value alone.
        let mut h = H::new(0.5, s);
        h.press(Modifiers::NONE);
        h.btn(false, Modifiers::NONE);
        h.idle();
        assert!(h.st().editing);
        h.text("0.9");
        h.key(Key::Escape);
        h.idle();
        assert!(!h.st().editing);
        assert_eq!(h.v, 0.5);
    }

    #[test]
    fn numeric_invalid_enter_flashes_and_reverts() {
        let s = NumSpec::unit_float(0.5);
        let mut h = H::new(0.5, s);
        h.press(Modifiers::NONE);
        h.btn(false, Modifiers::NONE);
        h.idle();
        h.text("abc");
        assert_eq!(h.st().text, "abc");
        h.key(Key::Enter);
        assert!(!h.st().editing);
        assert_eq!(h.v, 0.5);
        let until = h.st().flash_until;
        assert!((until - h.t).abs() <= 0.65 && until > h.t, "flash for 600 ms from the commit");
        assert!(h.delay <= Duration::from_millis(600), "its own one-shot repaint is scheduled: {:?}", h.delay);
        // After the flash a plain frame is drawn (time passes the flash end).
        h.t += 0.7;
        h.idle();
        assert!(h.ctx.input(|i| i.time) >= until);
        // A valid entry commits and clamps.
        let mut h = H::new(0.5, s);
        h.press(Modifiers::NONE);
        h.btn(false, Modifiers::NONE);
        h.idle();
        h.text("7");
        h.key(Key::Enter);
        assert_eq!(h.v, 1.0);
    }

    #[test]
    fn numeric_pointer_lock_only_when_supported() {
        for (os, ok) in [("windows", true), ("macos", true), ("linux", false), ("freebsd", false), ("", false)] {
            assert_eq!(pointer_lock_supported(os), ok, "{os}");
        }
        let grabs = |h: &H| h.cmds.iter().filter(|c| matches!(c, ViewportCommand::CursorGrab(_) | ViewportCommand::CursorVisible(_))).count();
        // No-grab mode: delta drives the value, no grab or hide command is ever sent.
        let mut h = H::new(0.5, NumSpec::unit_float(0.5));
        h.press(Modifiers::NONE);
        h.mv(38.0, Modifiers::NONE);
        h.btn(false, Modifiers::NONE);
        assert_eq!(h.v, 0.52);
        assert_eq!(grabs(&h), 0);
        // Lock mode: Locked then hidden after 3 px, driven by raw motion, released with the cursor shown on release.
        let mut h = H::new(0.5, NumSpec::unit_float(0.5));
        h.lock = true;
        h.press(Modifiers::NONE);
        h.mv(38.0, Modifiers::NONE);
        assert!(h.cmds.contains(&ViewportCommand::CursorGrab(CursorGrab::Locked)) && h.cmds.contains(&ViewportCommand::CursorVisible(false)));
        h.frame(vec![Event::MouseMoved(egui::vec2(4.0, 0.0))], Modifiers::NONE);
        assert_eq!(h.v, 0.53);
        h.cmds.clear();
        h.btn(false, Modifiers::NONE);
        assert!(h.cmds.contains(&ViewportCommand::CursorGrab(CursorGrab::None)) && h.cmds.contains(&ViewportCommand::CursorVisible(true)));
        // The widget disappearing mid-drag releases the grab and shows the cursor.
        let mut h = H::new(0.5, NumSpec::unit_float(0.5));
        h.lock = true;
        h.press(Modifiers::NONE);
        h.mv(38.0, Modifiers::NONE);
        h.cmds.clear();
        h.show = false;
        h.idle();
        assert!(h.cmds.contains(&ViewportCommand::CursorGrab(CursorGrab::None)) && h.cmds.contains(&ViewportCommand::CursorVisible(true)));
    }
}
