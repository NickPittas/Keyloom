//! Readable dark theme on neutral grey. Every button/checkbox/field has a border that holds >= 3:1
//! against the panel; text holds >= 4.5:1. Tests measure the visuals `apply` actually installs.
use eframe::egui::{self, Color32, Stroke};

pub const PANEL: Color32 = Color32::from_rgb(38, 46, 40);
pub const CANVAS: Color32 = Color32::from_rgb(23, 31, 25);
pub const FIELD: Color32 = Color32::from_rgb(30, 38, 32);
pub const BUTTON: Color32 = Color32::from_rgb(54, 64, 57);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(71, 84, 75);
pub const BORDER: Color32 = Color32::from_rgb(125, 145, 132);
pub const BORDER_HOVER: Color32 = Color32::from_rgb(166, 194, 175);
pub const ACCENT: Color32 = Color32::from_rgb(145, 178, 154);
pub const CHECKED: Color32 = Color32::from_rgb(145, 178, 154);
pub const FOCUS: Color32 = Color32::from_rgb(224, 176, 80);
pub const TEXT: Color32 = Color32::from_rgb(226, 232, 228);
pub const TEXT_DIM: Color32 = Color32::from_rgb(169, 187, 175);
pub const CARD: Color32 = Color32::from_rgb(47, 57, 50);
pub const CARD_HEADER: Color32 = Color32::from_rgb(65, 81, 69);
/// The tick on a checked box: dark on the light CHECKED fill (white on it would be under 3:1).
const CHECK_MARK: Color32 = Color32::from_rgb(21, 36, 26);
/// Fade of disabled widgets; blended text must still hold 3:1 (tested).
const DISABLED_ALPHA: f32 = 0.6;
const DISABLED_BORDER: Color32 = Color32::from_rgb(84, 84, 88);

/// A processing action, distinct from settings and secondary controls.
pub fn primary_button(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    let edge = if ui.visuals().dark_mode { Color32::from_rgb(174, 204, 183) } else { Color32::from_rgb(45, 103, 61) };
    ui.add(egui::Button::new(text.into().color(Color32::from_rgb(21, 36, 26))).fill(ACCENT).stroke(Stroke::new(1.0, edge)))
}

/// Raised card fill/outline for the current palette (the dark `CARD` constants are dark-mode only).
pub fn card_fill(ui: &egui::Ui) -> Color32 { ui.visuals().window_fill }
pub fn card_stroke(ui: &egui::Ui) -> Stroke {
    Stroke::new(1.0, if ui.visuals().dark_mode { Color32::from_rgb(77, 94, 82) } else { Color32::from_rgb(160, 176, 163) })
}
/// Side-rail background: darker than the panel in Dark, slightly darker in Light.
pub fn canvas_fill(ui: &egui::Ui) -> Color32 { if ui.visuals().dark_mode { CANVAS } else { Color32::from_rgb(232, 237, 232) } }
/// Secondary text that holds >= 4.5:1 on the panel and card of the current palette.
pub fn text_dim(ui: &egui::Ui) -> Color32 { if ui.visuals().dark_mode { TEXT_DIM } else { Color32::from_rgb(66, 82, 70) } }
/// Warning (amber) and error (red) text for the current palette.
pub fn warn(ui: &egui::Ui) -> Color32 { if ui.visuals().dark_mode { FOCUS } else { Color32::from_rgb(145, 91, 0) } }
pub fn error(ui: &egui::Ui) -> Color32 { if ui.visuals().dark_mode { Color32::from_rgb(240, 120, 110) } else { Color32::from_rgb(170, 30, 25) } }

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

/// Checkbox with the brief's states: empty = dark bordered box; checked = CHECKED fill + dark 2 px
/// mark. Wraps the stock widget (scoped visuals only), so focus, hover, disabled state and
/// accessibility info (checkbox role + checked flag) are egui's own.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    let on = *checked;
    let label = label.into().color(ui.visuals().widgets.noninteractive.fg_stroke.color);
    ui.scope(|ui| {
        if on {
            let w = &mut ui.visuals_mut().widgets;
            for (s, fill) in [
                (&mut w.inactive, CHECKED),
                (&mut w.hovered, Color32::from_rgb(168, 200, 178)),
                (&mut w.active, Color32::from_rgb(126, 160, 136)),
            ] {
                s.bg_fill = fill;
                s.fg_stroke = Stroke::new(2.0, CHECK_MARK);
            }
        }
        ui.checkbox(checked, label)
    })
    .inner
}

pub fn apply(ctx: &egui::Context) { apply_preference(ctx, "System"); }

fn palette(light: bool) -> egui::Visuals {
    let mut v = if light { egui::Visuals::light() } else { egui::Visuals::dark() };
    let (panel, card, field, button, hover, border, hover_border, text, focus) = if light {
        (Color32::from_rgb(244,246,244), Color32::WHITE, Color32::WHITE, Color32::from_rgb(220,228,221), Color32::from_rgb(185,211,190), Color32::from_rgb(83,105,88), Color32::from_rgb(45,103,61), Color32::from_rgb(28,38,30), Color32::from_rgb(145,91,0))
    } else { (PANEL, CARD, FIELD, BUTTON, BUTTON_HOVER, BORDER, BORDER_HOVER, TEXT, FOCUS) };
    v.panel_fill = panel; v.window_fill = card; v.extreme_bg_color = field;
    v.faint_bg_color = if light { Color32::from_rgb(235,239,235) } else { CARD };
    v.selection.bg_fill = if light { Color32::from_rgb(185,211,190) } else { Color32::from_rgb(65,88,74) };
    v.selection.stroke = Stroke::new(2.0, text);
    v.disabled_alpha = DISABLED_ALPHA;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = panel; w.noninteractive.weak_bg_fill = panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, if light { border } else { DISABLED_BORDER });
    w.noninteractive.fg_stroke = Stroke::new(1.0, text);
    w.inactive.weak_bg_fill = button; w.inactive.bg_fill = field;
    w.inactive.bg_stroke = Stroke::new(1.0, border); w.inactive.fg_stroke = Stroke::new(1.0, text);
    w.hovered.weak_bg_fill = hover; w.hovered.bg_fill = hover;
    w.hovered.bg_stroke = Stroke::new(2.0, hover_border); w.hovered.fg_stroke = Stroke::new(1.0, text);
    w.active.weak_bg_fill = if light { Color32::from_rgb(157,192,165) } else { Color32::from_rgb(65,88,74) };
    w.active.bg_fill = w.active.weak_bg_fill; w.active.bg_stroke = Stroke::new(2.0, focus);
    w.active.fg_stroke = Stroke::new(1.0, text); w.open = w.hovered;
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
        s.spacing.interact_size.y = 26.0;
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
    });
    ctx.set_theme(pref);
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
            ("selected", v.selection.stroke.color, v.selection.bg_fill),
            ("label", w.noninteractive.fg_stroke.color, v.panel_fill),
            ("checkmark", w.inactive.fg_stroke.color, w.inactive.bg_fill),
        ] {
            assert!(contrast(fg, bg) >= 4.5, "{n} {}", contrast(fg, bg));
        }
        assert!(contrast(v.selection.stroke.color, v.panel_fill) >= 3.0);
        // Disabled = the normal look at `disabled_alpha`: visibly different, text still >= 3:1.
        assert!(v.disabled_alpha < 0.8, "{preference} disabled looks like enabled");
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
            assert_eq!(ctx.global_style().spacing.interact_size.y, 26.0, "{os:?} spacing");
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
