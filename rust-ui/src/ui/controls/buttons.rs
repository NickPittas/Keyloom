//! The button family (bible §5.1, §5.2, §11, §12): primary, secondary, ghost, destructive (text / confirm), icon and
//! toggle buttons, all painted by one code path from [`look`] so rest / hover / pressed / focus / disabled stay in step.
use super::{keyboard_focus, line, paint_line};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::{tokens, Tokens, DARK};
use crate::ui::type_scale as ty;
use eframe::egui::{self, accesskit::Role, Align, Color32, CornerRadius, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, Vec2, Widget, WidgetInfo};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind { Primary, Secondary, Ghost, DestructiveText, DestructiveConfirm, Icon, Toggle }

/// Pointer-driven look of a button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Vis { Rest, Hover, Pressed }

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconButtonSize { Normal22, Small18 }

/// What one state of one kind draws (§5.1 table, §5.2).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    pub fill: Option<Color32>,
    /// 1 px outside border.
    pub border: Option<Color32>,
    /// 1 px inner border of the hover state of primary / danger.
    pub inner: Option<Color32>,
    pub text: Color32,
    pub icon: Color32,
}

/// `accent.tint.hover` (§5.2): one step lighter (Dark) / darker (Light) than `accent.tint`.
fn tint_hover(t: &Tokens) -> Color32 {
    if t.bg_panel == DARK.bg_panel { Color32::from_rgb(0x24, 0x42, 0x6A) } else { Color32::from_rgb(0xAF, 0xC9, 0xF2) }
}

/// The §5.1 / §5.2 table as a function: the single source for every state.
pub fn look(t: &Tokens, kind: Kind, vis: Vis, on: bool, enabled: bool) -> Look {
    use Kind::*;
    use Vis::*;
    let mk = |fill, border, inner, text| Look { fill, border, inner, text, icon: text };
    let sec_disabled = mk(None, Some(t.border_divider), None, t.text_disabled);
    let secondary = |text: Color32| match vis {
        Rest => mk(Some(t.bg_raised), Some(t.border_control), None, text),
        Hover => mk(Some(t.bg_hover), Some(t.border_hover), None, text),
        Pressed => mk(Some(t.bg_pressed), Some(t.text_secondary), None, text),
    };
    let ghost = |rest: Color32| match vis {
        Rest => mk(None, None, None, rest),
        Hover => mk(Some(t.bg_hover), Some(t.border_control), None, t.text_primary),
        Pressed => mk(Some(t.bg_pressed), Some(t.border_hover), None, t.text_primary),
    };
    if !enabled {
        return match kind {
            Primary => mk(Some(t.bg_raised), Some(t.border_divider), None, t.text_disabled),
            Ghost | Icon | Toggle => mk(None, None, None, t.text_disabled),
            _ => sec_disabled,
        };
    }
    match kind {
        Primary => match vis {
            Rest => mk(Some(t.accent), None, None, t.text_on_accent),
            Hover => mk(Some(t.accent_hover), None, Some(t.focus_ring), t.text_on_accent),
            Pressed => mk(Some(t.accent_pressed), None, None, t.text_on_accent),
        },
        Secondary => secondary(t.text_primary),
        DestructiveText => secondary(t.error),
        DestructiveConfirm => match vis {
            Rest => mk(Some(t.danger_fill), None, None, t.text_on_danger),
            Hover => mk(Some(t.danger_fill_hover), None, Some(Color32::from_white_alpha(102)), t.text_on_danger),
            Pressed => mk(Some(t.danger_fill_pressed), None, None, t.text_on_danger),
        },
        Ghost | Icon => ghost(t.text_secondary),
        Toggle if !on => ghost(t.text_secondary),
        Toggle => {
            let mut l = match vis {
                Rest => mk(Some(t.accent_tint), None, None, t.text_primary),
                Hover => mk(Some(tint_hover(t)), Some(t.accent_bright), None, t.text_primary),
                Pressed => mk(Some(t.bg_pressed), Some(t.border_hover), None, t.text_primary),
            };
            l.icon = t.text_link;
            if vis == Pressed { l.icon = t.text_primary }
            l
        }
    }
}

/// Custom spinner (§11, §12): 270° arc, one turn per second, repaints at ≤ 30 fps only while visible; `reduced_motion`
/// draws it still (gap to the right) and schedules nothing.
pub fn spinner(ui: &Ui, rect: Rect, color: Color32, reduced_motion: bool) {
    if !ui.is_rect_visible(rect) { return }
    let (c, r) = (rect.center(), (rect.width() - 1.5) / 2.0);
    let start = std::f32::consts::FRAC_PI_4 + if reduced_motion { 0.0 } else { (ui.input(|i| i.time) % 1.0) as f32 * std::f32::consts::TAU };
    let pts = (0..=32).map(|k| c + Vec2::angled(start + std::f32::consts::PI * 1.5 * k as f32 / 32.0) * r).collect();
    ui.painter().add(Shape::line(pts, Stroke::new(1.5, color)));
    if !reduced_motion { ui.ctx().request_repaint_after(std::time::Duration::from_millis(33)) }
}

/// A button. Build with the `*_button` constructors, add with `ui.add(..)`.
pub struct Btn<'a> {
    kind: Kind,
    label: Option<&'a str>,
    icon: Option<Icon>,
    height: f32,
    width: Option<f32>,
    min_width: Option<f32>,
    small: bool,
    disabled: Option<&'a str>,
    busy: Option<&'a str>,
    tooltip: Option<&'a str>,
    reduced_motion: bool,
    force: Option<(Vis, bool)>,
    on: Option<&'a mut bool>,
    at: Option<Pos2>,
    rect: Option<Rect>,
    alert: bool,
}

impl<'a> Btn<'a> {
    fn new(kind: Kind, label: Option<&'a str>) -> Self {
        Btn { kind, label, icon: None, height: 22.0, width: None, min_width: None, small: false, disabled: None, busy: None, tooltip: None,
              reduced_motion: false, force: None, on: None, at: None, rect: None, alert: false }
    }
    /// 26 px high (action bar, dialog footer, Settings footer).
    pub fn action_height(mut self) -> Self { self.height = 26.0; self }
    pub fn height(mut self, h: f32) -> Self { self.height = h; self }
    /// Fixed width (the label truncates, the button never grows).
    pub fn width(mut self, w: f32) -> Self { self.width = Some(w); self }
    pub fn min_width(mut self, w: f32) -> Self { self.min_width = Some(w); self }
    /// Disabled with the reason shown as tooltip and AccessKit description.
    pub fn disabled(mut self, reason: &'a str) -> Self { self.disabled = Some(reason); self }
    /// Disabled only when `cond` (see [`Btn::disabled`]).
    pub fn disabled_if(self, cond: bool, reason: &'a str) -> Self { if cond { self.disabled(reason) } else { self } }
    /// Busy: the label becomes `progress` with a 12 px spinner at the left; width unchanged when the button has a fixed width.
    pub fn busy(mut self, progress: &'a str) -> Self { self.busy = Some(progress); self }
    pub fn busy_if(self, progress: Option<&'a str>) -> Self { match progress { Some(p) => self.busy(p), None => self } }
    pub fn tooltip(mut self, tip: &'a str) -> Self { self.tooltip = Some(tip); self }
    /// Settings › Appearance › Reduce motion: the spinner draws a still arc.
    pub fn reduced_motion(mut self, on: bool) -> Self { self.reduced_motion = on; self }
    /// Missing state (§5.13): 6 px `error` dot instead of the icon, label in `error`.
    pub fn alert(mut self) -> Self { self.alert = true; self.icon = None; self }
    /// Leading icon (toggle).
    pub fn with_icon(mut self, icon: Icon) -> Self { self.icon = Some(icon); self }
    /// Specimen / test only: draw in a forced state (and keyboard focus) through the same painting path.
    pub fn forced(mut self, vis: Vis, keyboard_focus: bool) -> Self { self.force = Some((vis, keyboard_focus)); self }
    /// Place at an absolute position instead of the layout flow (specimen pages).
    pub fn at(mut self, pos: Pos2) -> Self { self.at = Some(pos); self }
    /// Place in exactly this rect (action bar slots): width and height come from the rect.
    pub fn in_rect(mut self, rect: Rect) -> Self { self.rect = Some(rect); self }

    fn pad(&self) -> f32 {
        match self.kind { Kind::Primary => 12.0, Kind::Secondary | Kind::DestructiveText | Kind::DestructiveConfirm => 10.0, _ => 8.0 }
    }
    /// Width this button would take for its label (action-bar primary slot).
    pub fn natural_width(&self, ui: &Ui) -> f32 {
        line(ui.painter(), self.label.unwrap_or(""), ty::button(), 16.0, f32::INFINITY).size().x + 2.0 * (self.pad() + 1.0)
    }
    fn min_w(&self) -> f32 {
        self.min_width.unwrap_or(match self.kind { Kind::Primary => 72.0, Kind::Secondary | Kind::DestructiveText | Kind::DestructiveConfirm => 64.0, _ => 32.0 })
    }
}

macro_rules! ctor {
    ($(#[$m:meta])* $name:ident, $kind:expr) => {
        $(#[$m])* pub fn $name(label: &str) -> Btn<'_> { Btn::new($kind, Some(label)) }
    };
}
ctor!(/** Primary (one per area): accent fill, Medium label. */ primary_button, Kind::Primary);
ctor!(secondary_button, Kind::Secondary);
ctor!(ghost_button, Kind::Ghost);
ctor!(/** Destructive outside a confirmation: a secondary button with `error` text. */ destructive_button, Kind::DestructiveText);
ctor!(/** Destructive inside the confirmation dialog: `danger.fill`, white text. */ destructive_confirm_button, Kind::DestructiveConfirm);

/// Icon-only ghost button 22 × 22 (icon 16) or 18 × 18 (icon 14); `tooltip` names it (AccessKit label too).
pub fn icon_button(icon: Icon, size: IconButtonSize, tooltip: &str) -> Btn<'_> {
    let mut b = Btn::new(Kind::Icon, None);
    b.icon = Some(icon);
    b.small = size == IconButtonSize::Small18;
    b.height = if b.small { 18.0 } else { 22.0 };
    b.tooltip = Some(tooltip);
    b
}

/// Toggle button (§5.2): 22 high, ghost geometry, optional label; flips `on` when clicked.
pub fn toggle_button<'a>(icon: Icon, label: Option<&'a str>, on: &'a mut bool, tooltip: &'a str) -> Btn<'a> {
    let mut b = Btn::new(Kind::Toggle, label);
    b.icon = Some(icon);
    b.on = Some(on);
    b.tooltip = Some(tooltip);
    b
}

const RADIUS: u8 = 3;

impl Widget for Btn<'_> {
    fn ui(mut self, ui: &mut Ui) -> Response {
        let t = tokens(ui);
        let icon_only = self.label.is_none();
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let text = self.busy.or(self.label);
        let (pad, icon_px) = (self.pad(), if self.small { IconSize::S14 } else { IconSize::S16 });
        let spin = self.busy.is_some();
        // Leading element: spinner (12, gap 6) or icon (16, gap 6); an icon-only button has just the icon.
        let (lead_w, lead_gap) = if spin { (12.0, 6.0) } else if self.alert { (6.0, 6.0) } else if self.icon.is_some() { (icon_px.px(), 6.0) } else { (0.0, 0.0) };
        let painter = ui.painter().clone();
        let inner_w = |w: f32| w - 2.0 * (pad + 1.0);
        let fixed_w = self.rect.map(|r| r.width()).or(self.width).or(if icon_only { Some(self.height) } else { None });
        let probe = text.map(|s| line(&painter, s, ty::button(), 16.0, f32::INFINITY));
        let nat = probe.as_ref().map_or(0.0, |g| g.size().x) + if text.is_some() { lead_w + lead_gap } else { lead_w };
        let w = fixed_w.unwrap_or_else(|| (nat + 2.0 * (pad + 1.0)).max(self.min_w()).round());
        let h = self.rect.map_or(self.height, |r| r.height());
        let galley = text.map(|s| line(&painter, s, ty::button(), 16.0, if icon_only { f32::INFINITY } else { (inner_w(w) - lead_w - lead_gap).max(1.0) }));

        let name = self.label.or(self.tooltip).unwrap_or("").to_owned();
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let resp = if let Some(r) = self.rect {
            ui.interact(r, ui.scope_id().with(("btn", name.as_str(), r.min.x as i32, r.min.y as i32)), sense)
        } else if let Some(p) = self.at {
            ui.interact(Rect::from_min_size(p, egui::vec2(w, h)), ui.scope_id().with(("btn", name.as_str(), p.x as i32, p.y as i32)), sense)
        } else {
            ui.allocate_exact_size(egui::vec2(w, h), sense).1
        };
        let rect = resp.rect;
        if enabled && resp.clicked() { if let Some(on) = self.on.as_deref_mut() { *on = !*on } }
        let on = self.on.as_deref().copied().unwrap_or(false);

        let (vis, focus) = match self.force {
            Some((v, f)) => (v, f),
            None => (
                if !enabled { Vis::Rest } else if resp.is_pointer_button_down_on() { Vis::Pressed } else if resp.hovered() { Vis::Hover } else { Vis::Rest },
                enabled && resp.has_focus() && keyboard_focus(ui.ctx()),
            ),
        };
        let mut l = look(t, self.kind, vis, on, enabled);
        if self.alert && enabled { l.text = t.error }
        // Busy (mockup 58): grey busy.fill with dark text and spinner; no accent fill, border or hover ring.
        if spin && enabled { l.fill = Some(t.busy_fill); l.text = t.badge_rendering_text; l.icon = t.badge_rendering_text; l.border = None; l.inner = None }
        if ui.is_rect_visible(rect) {
            let (p, r) = (ui.painter(), CornerRadius::same(RADIUS));
            if let Some(f) = l.fill { p.rect_filled(rect, r, f); }
            if focus {
                p.rect_stroke(rect, r, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
            } else {
                if let Some(b) = l.border { p.rect_stroke(rect, r, Stroke::new(1.0, b), StrokeKind::Inside); }
                // The hover "inner border" sits one pixel in from the edge, as in the sheet (its fill shows outside it).
                if let Some(b) = l.inner { p.rect_stroke(rect.shrink(1.0), CornerRadius::same(RADIUS - 1), Stroke::new(1.0, b), StrokeKind::Inside); }
            }
            // Pressed: the label of the text kinds moves 1 px down (§5.1); icons and toggles stay.
            let shift = if vis == Vis::Pressed && enabled && !matches!(self.kind, Kind::Icon | Kind::Toggle) { 1.0 } else { 0.0 };
            let tw = galley.as_ref().map_or(0.0, |g| g.size().x);
            let total = if galley.is_some() { lead_w + lead_gap + tw } else { lead_w };
            let x0 = (rect.center().x - total / 2.0).round();
            let lead = Rect::from_min_size(egui::pos2(x0, (rect.center().y - lead_w / 2.0).round()), Vec2::splat(lead_w));
            if spin {
                spinner(ui, lead, l.text, self.reduced_motion);
            } else if self.alert {
                p.circle_filled(lead.center(), 3.0, t.error);
            } else if let Some(ic) = self.icon {
                icons::paint(p, lead, ic, icon_px, l.icon);
            }
            if let Some(g) = galley {
                let tr = Rect::from_min_max(egui::pos2(x0 + lead_w + lead_gap, rect.min.y), egui::pos2(rect.max.x, rect.max.y));
                paint_line(p, tr, g, 16.0, Align::Min, l.text, shift);
            }
        }

        let tip = if enabled { self.tooltip } else { self.disabled.or(self.tooltip) };
        let name = if name.is_empty() { text.unwrap_or("").to_owned() } else { name };
        let busy_name = self.busy.map(str::to_owned);
        let is_toggle = self.kind == Kind::Toggle;
        resp.widget_info(|| {
            let n = busy_name.clone().unwrap_or_else(|| name.clone());
            if is_toggle { WidgetInfo::selected(Role::Switch, enabled, on, n) } else { WidgetInfo::labeled(Role::Button, enabled, n) }
        });
        if let Some(reason) = self.disabled {
            ui.ctx().accesskit_node_builder(resp.id, |n| n.set_description(reason));
        }
        match tip {
            Some(tip) => resp.on_hover_text(tip).on_disabled_hover_text(tip),
            None => resp,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tokens::LIGHT;

    const KINDS: [Kind; 7] = [Kind::Primary, Kind::Secondary, Kind::Ghost, Kind::DestructiveText, Kind::DestructiveConfirm, Kind::Icon, Kind::Toggle];

    /// State rule (§5.1): rest, hover and pressed differ in fill, and hover differs from rest in border (outer or
    /// inner); kinds that have an outer border change it at every step.
    #[test]
    fn rest_hover_pressed_differ_for_every_kind() {
        for t in [&DARK, &LIGHT] {
            for kind in KINDS {
                for on in [false, true] {
                    if on && kind != Kind::Toggle { continue }
                    let [r, h, p] = [Vis::Rest, Vis::Hover, Vis::Pressed].map(|v| look(t, kind, v, on, true));
                    let bits = |l: &Look| (l.border, l.inner);
                    let what = format!("{kind:?} on={on}");
                    assert!(r.fill != h.fill && h.fill != p.fill && r.fill != p.fill, "{what}: fills");
                    assert!(bits(&r) != bits(&h), "{what}: rest vs hover border");
                    if !matches!(kind, Kind::Primary | Kind::DestructiveConfirm) {
                        assert!(r.border != p.border && h.border != p.border, "{what}: borders");
                    }
                }
            }
        }
    }

    #[test]
    fn disabled_is_flat_and_never_accent() {
        for t in [&DARK, &LIGHT] {
            for kind in KINDS {
                let l = look(t, kind, Vis::Hover, true, false);
                assert_eq!(l.text, t.text_disabled);
                assert!(l.fill != Some(t.accent) && l.inner.is_none());
            }
        }
    }

    #[test]
    fn toggle_accesskit_role_is_switch() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut on = true;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            ui.add(toggle_button(Icon::Loop, Some("Loop"), &mut on, "Loop playback"));
            ui.add(primary_button("Key shot").disabled("Why not"));
        });
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.expect("accesskit tree");
        let find = |role: Role| update.nodes.iter().find(|(_, n)| n.role() == role).map(|(_, n)| n.clone());
        let sw = find(Role::Switch).expect("toggle is a Switch");
        assert_eq!(sw.toggled(), Some(accesskit_toggled(true)));
        let btn = find(Role::Button).expect("primary is a Button");
        assert_eq!(btn.description(), Some("Why not"));
    }

    fn accesskit_toggled(on: bool) -> egui::accesskit::Toggled { if on { egui::accesskit::Toggled::True } else { egui::accesskit::Toggled::False } }
}
