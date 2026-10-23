//! Stale-card feedback must not move content: only the painted outline may change. Headless egui; run through tools/isolated-test.sh.
use ck::ui::ai::card;
use eframe::egui::{self, Pos2, RawInput, Rect, Shape, Vec2};

/// (content min, content size, next widget y, amber outline painted) for one frame of `card`.
fn layout(stale: Option<f32>) -> (Pos2, Vec2, f32, bool) {
    let ctx = egui::Context::default();
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 600.0))), ..Default::default() };
    let mut seen = None;
    let mut out = ctx.run_ui(raw, |ui| {
        let r = card(ui, "Key", stale, |ui| {
            let r = ui.label("body").rect;
            (r.min, r.size())
        });
        seen = Some((r, ui.label("next").rect.min.y));
    });
    out.textures_delta.clear();
    let ((min, size), next_y) = seen.unwrap();
    let outline = out.shapes.iter().any(|s| matches!(&s.shape, Shape::Rect(r) if r.stroke.width > 1.5));
    (min, size, next_y, outline)
}

#[test]
fn border_feedback_never_shifts_layout() {
    let base = layout(None);
    assert!(!base.3, "fresh card has no amber outline");
    for p in [0.0, 0.5, 1.0] {
        let got = layout(Some(p));
        assert!(got.3, "stale card paints its outline at pulse {p}");
        assert_eq!((got.0, got.1, got.2), (base.0, base.1, base.2), "pulse {p}");
    }
}
