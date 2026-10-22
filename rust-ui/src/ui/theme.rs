//! Readable dark theme on neutral grey. Every button/checkbox/field has a border that holds >= 3:1
//! against the panel; text holds >= 4.5:1. Tests measure the visuals `apply` actually installs.
use eframe::egui::{self, Color32, CornerRadius, Stroke};

pub const PANEL: Color32 = Color32::from_rgb(44, 44, 46);
pub const CANVAS: Color32 = Color32::from_rgb(31, 31, 33);
pub const FIELD: Color32 = Color32::from_rgb(24, 24, 26);
pub const BUTTON: Color32 = Color32::from_rgb(72, 74, 78);
pub const BUTTON_HOVER: Color32 = Color32::from_rgb(87, 90, 95);
pub const BORDER: Color32 = Color32::from_rgb(128, 132, 138);
pub const BORDER_HOVER: Color32 = Color32::from_rgb(168, 172, 178);
pub const ACCENT: Color32 = Color32::from_rgb(62, 110, 160);
pub const CHECKED: Color32 = Color32::from_rgb(76, 139, 208);
pub const FOCUS: Color32 = Color32::from_rgb(224, 176, 80);
pub const TEXT: Color32 = Color32::from_rgb(220, 220, 220);
pub const TEXT_DIM: Color32 = Color32::from_rgb(190, 192, 198);
pub const CARD: Color32 = Color32::from_rgb(58, 60, 65);
pub const CARD_HEADER: Color32 = Color32::from_rgb(78, 80, 86);
const DISABLED_BORDER: Color32 = Color32::from_rgb(84, 84, 88);

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

/// Checkbox with the brief's states: empty = dark bordered box; checked = CHECKED fill + white 2 px
/// mark. Wraps the stock widget (scoped visuals only), so focus, hover, disabled state and
/// accessibility info (checkbox role + checked flag) are egui's own.
pub fn checkbox(
    ui: &mut egui::Ui,
    checked: &mut bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    let on = *checked;
    ui.scope(|ui| {
        if on {
            let w = &mut ui.visuals_mut().widgets;
            for (s, fill) in [
                (&mut w.inactive, CHECKED),
                (&mut w.hovered, Color32::from_rgb(98, 155, 218)),
                (&mut w.active, Color32::from_rgb(58, 118, 188)),
            ] {
                s.bg_fill = fill;
                s.fg_stroke = Stroke::new(2.0, Color32::WHITE);
            }
        }
        ui.checkbox(checked, label)
    })
    .inner
}

/// Replacement for the old `setup_style`.
pub fn setup_style(ctx: &egui::Context) {
    apply(ctx)
}

pub fn apply(ctx: &egui::Context) {
    let mut v = egui::Visuals::dark();
    v.panel_fill = PANEL;
    v.window_fill = Color32::from_rgb(50, 50, 52);
    v.extreme_bg_color = FIELD;
    v.faint_bg_color = Color32::from_rgb(50, 50, 52);
    v.hyperlink_color = Color32::from_rgb(130, 176, 224);
    v.window_stroke = Stroke::new(1.0, Color32::from_rgb(96, 98, 104));
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.5, Color32::WHITE);
    // Disabled widgets are drawn at disabled_alpha toward the background; keep them legible.
    v.disabled_alpha = 0.65;

    let w = &mut v.widgets;
    w.noninteractive.bg_fill = PANEL;
    w.noninteractive.weak_bg_fill = PANEL;
    w.noninteractive.bg_stroke = Stroke::new(1.0, DISABLED_BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    // Buttons use weak_bg_fill (raised); checkbox/slider/field use bg_fill (dark inset box).
    w.inactive.weak_bg_fill = BUTTON;
    w.inactive.bg_fill = FIELD;
    w.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    w.inactive.fg_stroke = Stroke::new(2.0, TEXT); // also the checkmark
    w.hovered.weak_bg_fill = BUTTON_HOVER;
    w.hovered.bg_fill = Color32::from_rgb(34, 34, 37);
    w.hovered.bg_stroke = Stroke::new(1.0, BORDER_HOVER);
    w.hovered.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    w.active.weak_bg_fill = ACCENT;
    w.active.bg_fill = Color32::from_rgb(40, 40, 44);
    w.active.bg_stroke = Stroke::new(2.0, FOCUS);
    w.active.fg_stroke = Stroke::new(2.0, Color32::WHITE);
    w.open.weak_bg_fill = BUTTON_HOVER;
    w.open.bg_stroke = Stroke::new(1.0, BORDER_HOVER);
    for s in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        s.corner_radius = CornerRadius::same(4);
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        use egui::{FontId, TextStyle::*};
        s.text_styles.insert(Body, FontId::proportional(13.5));
        s.text_styles.insert(Button, FontId::proportional(13.5));
        s.text_styles.insert(Small, FontId::proportional(12.0));
        s.text_styles.insert(Heading, FontId::proportional(18.0));
        s.text_styles.insert(Monospace, FontId::monospace(12.5));
        s.spacing.interact_size.y = 26.0;
        s.spacing.button_padding = egui::vec2(9.0, 4.0);
    });
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
        let (ctx, w) = styled();
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
        assert!(contrast(FOCUS, PANEL) >= 3.0 && contrast(CHECKED, PANEL) >= 3.0);
        // Disabled text (blended by disabled_alpha over the button) stays readable.
        let a = v.disabled_alpha;
        let m = |x: u8, y: u8| (x as f32 * a + y as f32 * (1.0 - a)).round() as u8;
        let (t, b) = (w.noninteractive.fg_stroke.color, PANEL);
        let d = Color32::from_rgb(m(t.r(), b.r()), m(t.g(), b.g()), m(t.b(), b.b()));
        assert!(
            contrast(d, b) >= 3.0 && contrast(d, b) < contrast(t, b),
            "disabled {}",
            contrast(d, b)
        );
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
    /// extra white mark. Also confirms contrast of what was actually painted.
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
        assert!(m1.contains(&Color32::WHITE), "{m1:?}");
        assert!(contrast(CHECKED, PANEL) >= 3.0 && contrast(Color32::WHITE, CHECKED) >= 3.0);
        assert!(contrast(BORDER, FIELD) >= 3.0 && contrast(FIELD, PANEL) > 1.0);
    }
}
