//! Reference x preserved. §8.8 dy=-10400; §8.10 Compact dy=-5200.
use crate::ui::{
    controls::{statusbar::*, Vis},
    tokens::tokens,
};
use eframe::egui::{self, Rect, Ui, UiBuilder};
fn at(
    ui: &mut Ui,
    x: f32,
    y: f32,
    w: f32,
    data: &StatusData<'_>,
    width: Width,
    force: ForcedState,
) {
    let mut child = ui.new_child(
        UiBuilder::new()
            .id_salt(("statusbar specimen", y as i32))
            .max_rect(Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, 22.0))),
    );
    status_bar_forced(&mut child, data, width, force);
}
pub fn show(ui: &mut Ui) {
    ui.painter()
        .rect_filled(ui.max_rect(), 0, tokens(ui).bg_panel);
    let mut data = StatusData {
        activity: Some((ActivityIcon::Spinner, "Keying V1-0062 · 34%")),
        notices: 1,
        failures: 0,
        queue_waiting: Some(3),
        start: false,
        meters: Some(Meters {
            cpu: 7.0,
            ram_used: 34.7,
            ram_total: 123.5,
            gpu_pct: 21.0,
            gpu_label: "RTX 4090",
            vram_used: 3.6,
            vram_total: 24.0,
        }),
    };
    let still = ForcedState {
        reduced_motion: true,
        ..Default::default()
    };
    at(ui, 49.0, 74.0, 1822.0, &data, Width::Wide, still);
    at(ui, 49.0, 146.0, 1390.0, &data, Width::Medium, still);
    for (y, visual, focus) in [
        (218.0, Vis::Hover, false),
        (290.0, Vis::Pressed, false),
        (362.0, Vis::Rest, true),
    ] {
        at(
            ui,
            49.0,
            y,
            1822.0,
            &data,
            Width::Wide,
            ForcedState {
                segment: Some(Segment::Gpu),
                visual: Some(visual),
                focus,
                ..still
            },
        );
    }
    data.activity = Some((ActivityIcon::Spinner, "Keying A003C001_261012… · 34%"));
    at(ui, 406.0, 494.0, 900.0, &data, Width::Compact, still);
    data.failures = 2;
    at(ui, 49.0, 602.0, 1822.0, &data, Width::Wide, still);
    at(
        ui,
        49.0,
        710.0,
        1822.0,
        &StatusData {
            notices: 1,
            ..Default::default()
        },
        Width::Wide,
        still,
    );
}
