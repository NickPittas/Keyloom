//! Keyloom palette on neutral grey (DESIGN-BIBLE.md §2, §12). The colours live in `tokens.rs`; this module maps the
//! helpers the screens already call onto them and installs the egui visuals. Every control border holds >= 3:1
//! against the panel; text holds >= 4.5:1. Tests measure the visuals `apply` actually installs.
use super::tokens::{tokens, DARK, LIGHT};
use eframe::egui::{self, Color32, Stroke};

// redesign: dark token; theme-aware replacement in a later step
pub const PANEL: Color32 = DARK.bg_panel;
// redesign: dark token; theme-aware replacement in a later step
pub const CANVAS: Color32 = DARK.bg_app;
// redesign: dark token; theme-aware replacement in a later step
pub const FIELD: Color32 = DARK.bg_sunken;
// redesign: dark token; theme-aware replacement in a later step
pub const BUTTON: Color32 = DARK.bg_raised;
// redesign: dark token; theme-aware replacement in a later step
pub const BUTTON_HOVER: Color32 = DARK.bg_hover;
// redesign: dark token; theme-aware replacement in a later step
pub const BORDER: Color32 = DARK.border_control;
// redesign: dark token; theme-aware replacement in a later step
pub const BORDER_HOVER: Color32 = DARK.border_hover;
// redesign: dark token; theme-aware replacement in a later step
pub const ACCENT: Color32 = DARK.accent;
// redesign: dark token; theme-aware replacement in a later step
pub const CHECKED: Color32 = DARK.accent;
// redesign: dark token; theme-aware replacement in a later step
pub const FOCUS: Color32 = DARK.focus_ring;
// redesign: dark token; theme-aware replacement in a later step
pub const TEXT: Color32 = DARK.text_primary;
// redesign: dark token; theme-aware replacement in a later step
pub const TEXT_DIM: Color32 = DARK.text_secondary;
// redesign: dark token; theme-aware replacement in a later step
pub const CARD: Color32 = DARK.bg_raised;
// redesign: dark token; theme-aware replacement in a later step
pub const CARD_HEADER: Color32 = DARK.bg_raised;
/// The tick on a checked box (white on `accent`).
#[cfg(test)]
const CHECK_MARK: Color32 = DARK.text_on_accent;

/// A processing action, distinct from settings and secondary controls.
pub fn primary_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let edge = tokens(ui).accent_bright;
    accent(ui, ui.is_enabled(), |ui| ui.add(egui::Button::new(text.into()).stroke(Stroke::new(1.0, edge))))
}

/// Raised card fill/outline for the current palette.
pub fn card_fill(ui: &egui::Ui) -> Color32 { tokens(ui).bg_raised }
pub fn card_stroke(ui: &egui::Ui) -> Stroke { Stroke::new(1.0, tokens(ui).border_default) }
/// Side-rail background (the window background grey).
pub fn canvas_fill(ui: &egui::Ui) -> Color32 { tokens(ui).bg_app }
/// Secondary text that holds >= 4.5:1 on the panel and card of the current palette.
pub fn text_dim(ui: &egui::Ui) -> Color32 { tokens(ui).text_secondary }
/// Warning (amber) and error (red) text for the current palette.
pub fn warn(ui: &egui::Ui) -> Color32 { tokens(ui).warning }
pub fn error(ui: &egui::Ui) -> Color32 { tokens(ui).error }

/// Success (green) text for the current palette.
pub fn ok(ui: &egui::Ui) -> Color32 { tokens(ui).success }
/// Green status dot / accent mark that holds >= 3:1 on the panel and canvas of the current palette.
pub fn accent_mark(ui: &egui::Ui) -> Color32 { tokens(ui).success }
/// Board stage identity: the card's edge colour, and the stage chip's fill and text (bible §2.6).
/// Masked = Mask, Complete = Completed, Error = Failed; Extracting (not in the bible) is neutral `info`.
pub struct StageColors { pub edge: Color32, pub chip: Color32, pub text: Color32 }
pub fn stage(ui: &egui::Ui, s: crate::project::ClipState) -> StageColors {
    use crate::project::ClipState::*;
    let t = tokens(ui);
    let (chip, text, edge) = match s {
        Raw => (t.stage_raw_chip_fill, t.stage_raw_chip_text, t.stage_raw_bar),
        Masked => (t.stage_mask_chip_fill, t.stage_mask_chip_text, t.stage_mask_bar),
        Ready => (t.stage_ready_chip_fill, t.stage_ready_chip_text, t.stage_ready_bar),
        Complete => (t.stage_completed_chip_fill, t.stage_completed_chip_text, t.stage_completed_bar),
        Extracting => (t.info_tint, t.info, t.busy_queued),
        Error => (t.stage_failed_chip_fill, t.stage_failed_chip_text, t.stage_failed_bar),
    };
    StageColors { edge, chip, text }
}
/// Outline of big panel frames.
pub fn panel_edge(ui: &egui::Ui) -> Color32 { tokens(ui).border_divider }

/// Runs `add` with the accent fill on stock buttons: the fill steps lighter on hover and darker when pressed
/// (a plain `.fill(ACCENT)` would paint every state the same). White text, so disabled look and focus stay egui's own.
pub fn accent<R>(ui: &mut egui::Ui, enabled: bool, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let t = tokens(ui);
    ui.scope(|ui| {
        if enabled {
            let w = &mut ui.visuals_mut().widgets;
            for (s, fill) in [(&mut w.inactive, t.accent), (&mut w.hovered, t.accent_hover), (&mut w.active, t.accent_pressed)] {
                s.weak_bg_fill = fill;
                s.fg_stroke = Stroke::new(1.0, t.text_on_accent);
            }
        }
        // Already-disabled parents are not faded a second time.
        if enabled || !ui.is_enabled() { add(ui) } else { ui.add_enabled_ui(false, add).inner }
    }).inner
}
// redesign: dark token; theme-aware replacement in a later step
pub const ACCENT_TEXT: Color32 = DARK.text_on_accent;

/// Scope helper for scroll areas: the scroll handle is painted with `widgets.*.bg_fill`, so a scope sets those
/// to the §5.14 thumb tokens (rest, hover, drag). Not applied to the screens yet.
pub fn scroll_thumb_scope<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> egui::InnerResponse<R> {
    let t = tokens(ui);
    ui.scope(|ui| {
        let w = &mut ui.visuals_mut().widgets;
        w.inactive.bg_fill = t.scroll_thumb;
        w.hovered.bg_fill = t.scroll_thumb_hover;
        w.active.bg_fill = t.scroll_thumb_drag;
        add(ui)
    })
}

/// Shared raised parameter group; every pane uses the same padding and title alignment.
pub fn section<R>(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let (fill, stroke, dim) = (card_fill(ui), card_stroke(ui), text_dim(ui));
    let out = egui::Frame::new().fill(fill).stroke(stroke)
        .corner_radius(5).inner_margin(12).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(12.0).color(dim).strong());
            ui.add_space(8.0);
            body(ui)
        }).inner;
    ui.add_space(10.0);
    out
}

fn lin(c: u8) -> f32 {
    let c = c as f32 / 255.0;
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG contrast ratio (1.0..=21.0) between two opaque colours.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    let l = |c: Color32| 0.2126 * lin(c.r()) + 0.7152 * lin(c.g()) + 0.0722 * lin(c.b());
    let (la, lb) = (l(a), l(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Checkbox with the bible's states: empty = sunken box with a control border; checked = `accent` fill + white
/// 1.5 px mark. Wraps the stock widget (scoped visuals only), so focus, hover, disabled state and
/// accessibility info (checkbox role + checked flag) are egui's own.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    let on = *checked;
    let t = tokens(ui);
    let label = label.into().color(ui.visuals().widgets.noninteractive.fg_stroke.color);
    ui.scope(|ui| {
        if on {
            let w = &mut ui.visuals_mut().widgets;
            for (s, fill) in [(&mut w.inactive, t.accent), (&mut w.hovered, t.accent_hover), (&mut w.active, t.accent_pressed)] {
                s.bg_fill = fill;
                s.fg_stroke = Stroke::new(1.5, t.text_on_accent);
            }
        }
        ui.checkbox(checked, label)
    })
    .inner
}

pub fn apply(ctx: &egui::Context) { apply_preference(ctx, "System"); }

/// Bible §12: the egui `Visuals` of one theme.
fn palette(light: bool) -> egui::Visuals {
    let t = if light { &LIGHT } else { &DARK };
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    // Glyph weight vs the Chromium reference (ink mass within 0.95-1.05 at 11-12 px): Dark needs a lighter ramp than egui's
    // default 2c-c^2 (which draws 20-35 % too heavy), Light is right with the raw coverage (Off).
    v.text_options.color_transfer_function = if light {
        egui::epaint::FontColorTransferFunction::Off // Light: coverage as rasterised, matches Chromium
    } else {
        egui::epaint::FontColorTransferFunction::Gamma(0.95) // Dark: alpha = coverage^0.95, matches Chromium
    };
    v.panel_fill = t.bg_panel;
    v.window_fill = t.bg_overlay;
    v.extreme_bg_color = t.bg_sunken;
    v.faint_bg_color = t.bg_raised;
    v.window_stroke = Stroke::new(1.0, t.border_overlay);
    let shade = |dy: i8, blur: u8, a: u8| egui::epaint::Shadow { offset: [0, dy], blur, spread: 0, color: Color32::from_black_alpha(a) };
    v.window_shadow = shade(8, 24, if light { 51 } else { 128 }); // dialog: 0 8 24 @ 50% (Light 20%)
    v.popup_shadow = shade(4, 16, if light { 46 } else { 115 }); // menu: 0 4 16 @ 45% (Light 18%)
    v.window_corner_radius = egui::CornerRadius::same(6);
    v.menu_corner_radius = egui::CornerRadius::same(4);
    v.selection.bg_fill = t.text_selection;
    v.selection.stroke = Stroke::new(1.0, t.text_primary); // egui draws selected-label text in this colour: text.primary on text.selection
    v.hyperlink_color = t.text_link;
    v.warn_fg_color = t.warning;
    v.error_fg_color = t.error;
    v.disabled_alpha = 1.0; // the explicit disabled tokens are drawn exactly, never dimmed twice
    let w = &mut v.widgets;
    let r = egui::CornerRadius::same(3);
    w.noninteractive.bg_fill = t.bg_panel; w.noninteractive.weak_bg_fill = t.bg_panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, t.border_divider);
    w.noninteractive.fg_stroke = Stroke::new(1.0, t.text_primary);
    // bg_fill is also the slider rail and the checkbox box: bg.sunken.
    w.inactive.weak_bg_fill = t.bg_raised; w.inactive.bg_fill = t.bg_sunken;
    w.inactive.bg_stroke = Stroke::new(1.0, t.border_control); w.inactive.fg_stroke = Stroke::new(1.0, t.text_primary);
    w.hovered.weak_bg_fill = t.bg_hover; w.hovered.bg_fill = t.bg_sunken;
    w.hovered.bg_stroke = Stroke::new(1.0, t.border_hover); w.hovered.fg_stroke = Stroke::new(1.0, t.text_primary);
    w.active.weak_bg_fill = t.bg_pressed; w.active.bg_fill = t.bg_sunken;
    w.active.bg_stroke = Stroke::new(1.0, t.text_secondary); w.active.fg_stroke = Stroke::new(1.0, t.text_primary);
    w.open = w.active;
    for s in [&mut w.noninteractive, &mut w.inactive, &mut w.hovered, &mut w.active, &mut w.open] {
        s.corner_radius = r;
        s.expansion = 0.0;
    }
    v
}

pub fn apply_preference(ctx: &egui::Context, preference: &str) {
    let pref = match preference { "Dark" => egui::ThemePreference::Dark, "Light" => egui::ThemePreference::Light, _ => egui::ThemePreference::System };
    // Both egui themes get our palette so System follows the OS live without falling back to stock egui.
    ctx.set_visuals_of(egui::Theme::Dark, palette(false));
    ctx.set_visuals_of(egui::Theme::Light, palette(true));
    ctx.all_styles_mut(|s| {
        use egui::{FontId, TextStyle::*};
        s.text_styles.insert(Body, FontId::proportional(13.5));
        s.text_styles.insert(Button, FontId::proportional(13.5));
        s.text_styles.insert(Small, FontId::proportional(12.0));
        s.text_styles.insert(Heading, FontId::proportional(18.0));
        s.text_styles.insert(Monospace, FontId::monospace(12.5));
        s.spacing.interact_size.y = 22.0;
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
        s.spacing.combo_width = 120.0;
        s.spacing.text_edit_width = 160.0;
        s.spacing.icon_width = 14.0;
        s.spacing.icon_spacing = 6.0;
        s.spacing.menu_margin = egui::Margin::same(4);
        s.spacing.tooltip_width = 320.0;
        s.spacing.scroll = egui::style::ScrollStyle { bar_width: 6.0, bar_inner_margin: 1.0, bar_outer_margin: 1.0, handle_min_length: 24.0, ..egui::style::ScrollStyle::solid() };
        s.interaction.tooltip_delay = 0.5;
        s.interaction.show_tooltips_only_when_still = true;
    });
    ctx.set_theme(pref);
}

/// Test support: every text painted in `shapes` against the fills painted before it (over `base`).
/// Text over images is skipped (image pixels are not ours); disabled/faded text needs 3:1, the rest `min`.
#[doc(hidden)]
pub fn text_contrast_report(shapes: &[egui::epaint::ClippedShape], base: Color32, min: f32) -> (usize, Vec<String>) {
    use egui::Shape;
    enum Ev { Fill(egui::Rect, Color32), Image(egui::Rect), Text(egui::Rect, Vec<Color32>, f32, String) }
    fn flat(s: &Shape, clip: egui::Rect, out: &mut Vec<Ev>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| flat(x, clip, out)),
            Shape::Rect(r) if r.fill.a() > 0 => out.push(Ev::Fill(r.rect.intersect(clip), r.fill)),
            Shape::Circle(c) if c.fill.a() > 0 => out.push(Ev::Fill(egui::Rect::from_center_size(c.center, egui::vec2(c.radius * 2.0, c.radius * 2.0)).intersect(clip), c.fill)),
            Shape::Mesh(m) if !m.vertices.is_empty() => out.push(Ev::Image(m.calc_bounds().intersect(clip))),
            Shape::Text(t) => {
                let cols = t.galley.job.sections.iter().map(|sec| {
                    let c = sec.format.color;
                    if c == Color32::PLACEHOLDER { t.override_text_color.unwrap_or(t.fallback_color) } else { t.override_text_color.unwrap_or(c) }
                }).collect();
                out.push(Ev::Text(t.galley.rect.translate(t.pos.to_vec2()).intersect(clip), cols, t.opacity_factor, t.galley.job.text.clone()));
            }
            _ => {}
        }
    }
    // Colour32 is premultiplied: result = fg + bg * (1 - alpha), with `k` the extra opacity.
    let over = |fg: Color32, bg: Color32, k: f32| {
        let a = fg.a() as f32 / 255.0 * k;
        let ch = |f: u8, b: u8| (f as f32 * k + b as f32 * (1.0 - a)).round().min(255.0) as u8;
        Color32::from_rgb(ch(fg.r(), bg.r()), ch(fg.g(), bg.g()), ch(fg.b(), bg.b()))
    };
    let mut evs = vec![];
    for cs in shapes { flat(&cs.shape, cs.clip_rect, &mut evs); }
    let (mut seen, mut bad, mut checked) = (Vec::<&Ev>::new(), vec![], 0);
    for e in &evs {
        if let Ev::Text(r, cols, k, text) = e {
            let at = r.center();
            if r.is_positive() && !seen.iter().any(|s| matches!(s, Ev::Image(i) if i.contains(at))) {
                let mut bg = base;
                for s in &seen { if let Ev::Fill(f, c) = s { if f.contains(at) { bg = over(*c, bg, 1.0); } } }
                for c in cols.iter().filter(|c| c.a() > 0) {
                    checked += 1;
                    // The disabled token (either theme) is exempt from the 4.5:1 text rule: bible §2.3 asks 3:1 for disabled text.
                    let disabled = *c == DARK.text_disabled || *c == LIGHT.text_disabled;
                    let want = if *k < 1.0 || c.a() < 255 || disabled { 3.0 } else { min };
                    let got = contrast(over(*c, bg, *k), bg);
                    if got < want { bad.push(format!("{text:?} {c:?} on {bg:?}: {got:.2} < {want}")); }
                }
            }
        }
        seen.push(e);
    }
    (checked, bad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, RawInput};

    fn styled() -> (Context, egui::style::Widgets) {
        let ctx = Context::default();
        apply(&ctx);
        let w = ctx.global_style().visuals.widgets.clone();
        (ctx, w)
    }

    #[test]
    fn installed_visuals_meet_contrast() {
      for preference in ["Dark", "Light"] {
        let ctx = Context::default();
        apply_preference(&ctx, preference);
        let w = ctx.global_style().visuals.widgets.clone();
        let v = ctx.global_style().visuals.clone();
        // Component boundaries vs the panel they sit on.
        for (n, s) in [
            ("inactive", w.inactive.bg_stroke),
            ("hovered", w.hovered.bg_stroke),
            ("active", w.active.bg_stroke),
        ] {
            assert!(
                contrast(s.color, v.panel_fill) >= 3.0,
                "{n} border {}",
                contrast(s.color, v.panel_fill)
            );
        }
        assert!(contrast(w.inactive.bg_stroke.color, w.inactive.bg_fill) >= 3.0);
        // Text on the fills it is painted over.
        for (n, fg, bg) in [
            (
                "button",
                w.inactive.fg_stroke.color,
                w.inactive.weak_bg_fill,
            ),
            (
                "button hover",
                w.hovered.fg_stroke.color,
                w.hovered.weak_bg_fill,
            ),
            (
                "button pressed",
                w.active.fg_stroke.color,
                w.active.weak_bg_fill,
            ),
            ("label", w.noninteractive.fg_stroke.color, v.panel_fill),
            ("checkmark", w.inactive.fg_stroke.color, w.inactive.bg_fill),
        ] {
            assert!(contrast(fg, bg) >= 4.5, "{n} {}", contrast(fg, bg));
        }
        assert!(contrast(v.selection.stroke.color, v.panel_fill) >= 3.0);
        // Bible §12: explicit disabled tokens are drawn exactly, so egui adds no second dimming; text stays >= 3:1.
        assert_eq!(v.disabled_alpha, 1.0, "{preference} disabled alpha");
        let blend = |fg: Color32, bg: Color32| {
            let m = |a: u8, b: u8| (a as f32 * v.disabled_alpha + b as f32 * (1.0 - v.disabled_alpha)).round() as u8;
            Color32::from_rgb(m(fg.r(), bg.r()), m(fg.g(), bg.g()), m(fg.b(), bg.b()))
        };
        for (n, fg, bg) in [
            ("disabled label", w.noninteractive.fg_stroke.color, v.panel_fill),
            ("disabled button", w.inactive.fg_stroke.color, w.inactive.weak_bg_fill),
        ] {
            let c = contrast(blend(fg, bg), blend(bg, v.panel_fill));
            assert!(c >= 3.0, "{preference} {n} {c}");
        }
        assert!(contrast(FOCUS, PANEL) >= 3.0 && contrast(CHECKED, PANEL) >= 3.0);
        assert!(contrast(v.widgets.active.bg_stroke.color, v.panel_fill) >= 3.0);
        assert_ne!(w.inactive.weak_bg_fill, w.hovered.weak_bg_fill, "{preference} hover fill");
      }
    }

    /// System follows the OS live and always lands on OUR palette, never stock egui.
    #[test]
    fn system_theme_follows_os_with_our_palette() {
        let ctx = Context::default();
        apply_preference(&ctx, "System");
        for (os, light) in [(egui::Theme::Light, true), (egui::Theme::Dark, false), (egui::Theme::Light, true)] {
            let raw = RawInput { system_theme: Some(os), ..Default::default() };
            let mut out = ctx.run_ui(raw, |_| {});
            out.textures_delta.clear();
            assert_eq!(ctx.global_style().visuals.panel_fill, palette(light).panel_fill, "{os:?}");
            assert_eq!(ctx.global_style().spacing.interact_size.y, 22.0, "{os:?} spacing");
        }
    }

    #[test]
    fn button_and_checkbox_differ_from_panel_and_between_states() {
        let (_, w) = styled();
        assert_ne!(w.inactive.weak_bg_fill, PANEL);
        assert_ne!(w.inactive.bg_fill, PANEL);
        assert!(w.inactive.bg_stroke.width >= 1.0);
        assert_ne!(w.inactive.weak_bg_fill, w.hovered.weak_bg_fill);
        assert_ne!(w.hovered.weak_bg_fill, w.active.weak_bg_fill);
        assert_ne!(w.inactive.bg_stroke.color, w.hovered.bg_stroke.color);
    }

    /// Paints a real checkbox pair and checks the pixels-as-shapes: the box is a bordered rect distinct
    /// from the panel, and only the checked one carries a checkmark polyline (stock egui does not
    /// change the box fill when checked).
    #[test]
    fn stock_checkbox_paints_box_and_mark() {
        let ctx = Context::default();
        apply(&ctx);
        let shapes_for = |checked: bool| {
            let mut c = checked;
            let mut out = ctx.run_ui(RawInput::default(), |ui| {
                ui.checkbox(&mut c, "x");
            });
            out.textures_delta.clear();
            let mut rects = vec![];
            let mut lines = 0;
            for cs in &out.shapes {
                walk(&cs.shape, &mut |s| match s {
                    egui::Shape::Rect(r) if r.stroke.width > 0.0 => {
                        rects.push((r.fill, r.stroke.color))
                    }
                    egui::Shape::LineSegment { .. } | egui::Shape::Path(_) => lines += 1,
                    _ => {}
                });
            }
            (rects, lines)
        };
        let (r0, l0) = shapes_for(false);
        let (r1, l1) = shapes_for(true);
        let boxed = r0.iter().find(|(f, s)| *f == FIELD && *s == BORDER);
        assert!(boxed.is_some(), "empty box not bordered: {r0:?}");
        assert!(r1.iter().any(|(f, s)| *f == FIELD && *s == BORDER));
        assert!(l1 > l0, "checked has no extra mark ({l0} vs {l1})");
    }

    fn walk(s: &egui::Shape, f: &mut impl FnMut(&egui::Shape)) {
        f(s);
        if let egui::Shape::Vec(v) = s {
            v.iter().for_each(|x| walk(x, f));
        }
    }

    /// Real paint of `theme::checkbox`: empty = dark bordered box, no mark; checked = CHECKED fill +
    /// dark mark. Also confirms contrast of what was actually painted.
    #[test]
    fn themed_checkbox_paints_distinct_states() {
        let ctx = Context::default();
        apply(&ctx);
        let paint = |checked: bool| {
            let mut c = checked;
            let mut out = ctx.run_ui(RawInput::default(), |ui| {
                checkbox(ui, &mut c, "x");
            });
            out.textures_delta.clear();
            let (mut boxes, mut marks) = (vec![], vec![]);
            for cs in &out.shapes {
                walk(&cs.shape, &mut |s| match s {
                    egui::Shape::Rect(r) if r.stroke.width > 0.0 => {
                        boxes.push((r.fill, r.stroke.color))
                    }
                    egui::Shape::Path(p) => {
                        if let egui::epaint::ColorMode::Solid(c) = p.stroke.color {
                            marks.push(c)
                        }
                    }
                    _ => {}
                });
            }
            (boxes, marks)
        };
        let (b0, m0) = paint(false);
        let (b1, m1) = paint(true);
        assert!(
            b0.contains(&(FIELD, BORDER)) && m0.is_empty(),
            "{b0:?} {m0:?}"
        );
        assert!(b1.iter().any(|(f, _)| *f == CHECKED), "{b1:?}");
        assert!(m1.contains(&CHECK_MARK), "{m1:?}");
        assert!(contrast(CHECKED, PANEL) >= 3.0 && contrast(CHECK_MARK, CHECKED) >= 3.0, "the tick must be readable on the fill");
        assert!(contrast(BORDER, FIELD) >= 3.0 && contrast(FIELD, PANEL) > 1.0);
    }
}
