//! Reference x retained, constant dy = -8560 (rows at y=59/119/179).
use crate::ui::{
    controls::{line, machine_row::*, paint_line},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, Align, Rect, Stroke, StrokeKind, Ui, UiBuilder};
fn rows() -> [MachineRowData; 3] {
    [
        MachineRowData {
            name: "This machine".into(),
            kind: MachineKind::Local,
            online: true,
            offline_since: None,
            job: Some(("V1-0062".into(), 34.0)),
            meters: Some(Meters {
                gpu: 21.0,
                vram_pct: 15.0,
                vram_gb: (3.6, 24.0),
                cpu: 7.0,
                ram_pct: 28.0,
                ram_gb: (34.7, 123.5),
            }),
        },
        MachineRowData {
            name: "endor-omarchy".into(),
            kind: MachineKind::Network,
            online: true,
            offline_since: None,
            job: None,
            meters: Some(Meters {
                gpu: 0.0,
                vram_pct: 4.0,
                vram_gb: (1.0, 24.0),
                cpu: 3.0,
                ram_pct: 12.0,
                ram_gb: (15.0, 123.5),
            }),
        },
        MachineRowData {
            name: "render-node-07-with-a-long-name".into(),
            kind: MachineKind::Network,
            online: false,
            offline_since: Some("2 minutes ago".into()),
            job: None,
            meters: None,
        },
    ]
}
pub fn show(ui: &mut Ui) {
    let t = *tokens(ui);
    ui.painter().rect_filled(ui.max_rect(), 0, t.bg_panel);
    ui.painter().rect_stroke(
        Rect::from_min_size(egui::pos2(32.0, 9.0), egui::vec2(566.59375, 282.0)),
        4,
        Stroke::new(1.0, t.border_default),
        StrokeKind::Inside,
    );
    let panel = Rect::from_min_size(egui::pos2(49.0, 58.0), egui::vec2(300.0, 182.0));
    ui.painter().rect_stroke(
        panel,
        0,
        Stroke::new(1.0, t.border_divider),
        StrokeKind::Inside,
    );
    for (i, data) in rows().iter().enumerate() {
        let r = Rect::from_min_size(
            egui::pos2(58.0, 59.0 + i as f32 * 60.0),
            egui::vec2(282.0, 60.0),
        );
        let mut child = ui.new_child(UiBuilder::new().max_rect(r));
        machine_row(&mut child, data, 298.0);
    }
    ui.painter().rect_filled(
        Rect::from_min_size(egui::pos2(58.0, 238.0), egui::vec2(282.0, 1.0)),
        0,
        t.bg_panel,
    );
    let g = line(
        ui.painter(),
        "7.3 · Queue › Machines rows",
        egui::FontId::new(14.0, egui::FontFamily::Name("SemiBold".into())),
        20.0,
        540.0,
    );
    paint_line(
        ui.painter(),
        Rect::from_min_size(egui::pos2(49.0, 26.0), egui::vec2(540.0, 20.0)),
        g,
        20.0,
        Align::Min,
        t.text_primary,
        0.0,
    );
    let g = line(
        ui.painter(),
        "60 high · 2 × 2 meters · memory in %, GB in tooltip · panel 300",
        ty::small(),
        14.0,
        340.0,
    );
    paint_line(
        ui.painter(),
        Rect::from_min_size(egui::pos2(264.40625, 30.0), egui::vec2(340.0, 14.0)),
        g,
        14.0,
        Align::Min,
        t.text_tertiary,
        0.0,
    );
    for (i, s) in [
        "Meter 100 = label 36 + value 36 + 4 + bar 24. Columns 128",
        "at 300, 108 at 260, 98 at 240.",
    ]
    .iter()
    .enumerate()
    {
        let g = line(ui.painter(), s, ty::small(), 14.0, 300.0);
        paint_line(
            ui.painter(),
            Rect::from_min_size(
                egui::pos2(49.0, 245.0 + i as f32 * 14.0),
                egui::vec2(300.0, 14.0),
            ),
            g,
            14.0,
            Align::Min,
            t.text_tertiary,
            0.0,
        );
    }
    for (x, width, state) in [
        (
            390.0,
            300.0,
            RowState {
                hovered: true,
                selected: false,
            },
        ),
        (
            720.0,
            260.0,
            RowState {
                selected: true,
                hovered: false,
            },
        ),
        (1010.0, 240.0, RowState::default()),
        (1280.0, 220.0, RowState::default()),
    ] {
        let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(
            egui::pos2(x, 59.0),
            egui::vec2(width, 60.0),
        )));
        machine_row_forced(&mut child, &rows()[0], width, state);
    }
}
