//! Reference x retained. Wide dy = -4882; Compact dy = -5354.
use crate::ui::{
    controls::{
        buttons::Vis,
        line, paint_line,
        transport::{self, Action, Forced, Layout, TimelineData, TransportState},
    },
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, pos2, vec2, Align, Rect, Ui, UiBuilder};

fn at(ui: &mut Ui, x: f32, y: f32, w: f32, paint: impl FnOnce(&mut Ui)) {
    let mut c = ui.new_child(
        UiBuilder::new()
            .id_salt((x as i32, y as i32))
            .max_rect(Rect::from_min_size(pos2(x, y), vec2(w, 100.0)))
            .layout(egui::Layout::top_down(Align::Min)),
    );
    c.spacing_mut().item_spacing = vec2(0.0, 0.0);
    paint(&mut c);
}
fn title(ui: &Ui, y: f32, text: &str) {
    let g = line(ui.painter(), text, ty::panel_header(), 16.0, 1464.0);
    paint_line(
        ui.painter(),
        Rect::from_min_size(pos2(406.0, y), vec2(1464.0, 24.0)),
        g,
        16.0,
        Align::Min,
        tokens(ui).text_secondary,
        0.0,
    );
}
pub fn show(ui: &mut Ui) {
    title(ui, 40.0, "Transport and timeline · Wide · sequence frames");
    let d = TimelineData {
        first: 0,
        last: 64,
        frame: 12,
        in_pt: Some(6),
        out_pt: Some(52),
        marks: &[14, 30],
        cached: &[(0, 19)],
        fps: 24.0,
    };
    for (y, w, layout) in [
        (80.0, 1464.0, Layout::Wide),
        (260.0, 619.0, Layout::Compact),
    ] {
        if layout == Layout::Compact {
            title(
                ui,
                220.0,
                "Compact · marks and In/Out in overflow · Loop stays",
            );
        }
        at(ui, 406.0, y, w, |ui| {
            let mut s = TransportState {
                first: 0,
                last: 64,
                frame: 12,
                loop_on: true,
                in_pt: Some(6),
                out_pt: Some(52),
                ..Default::default()
            };
            transport::transport(ui, &mut s, layout);
            transport::timeline(ui, &d);
            if layout == Layout::Wide {
                transport::timeline_meta(ui, &d);
            }
        });
    }
    title(
        ui,
        380.0,
        "Forced states · hover, pressed, keyboard focus · real paint path",
    );
    for (i, vis, focus) in [
        (0, Vis::Hover, false),
        (1, Vis::Pressed, false),
        (2, Vis::Rest, true),
    ] {
        at(ui, 406.0, 420.0 + i as f32 * 100.0, 619.0, |ui| {
            let mut s = TransportState {
                first: 1001,
                last: 1065,
                frame: 1012,
                playing: true,
                reverse: false,
                loop_on: true,
                marks: vec![1012],
                ..Default::default()
            };
            transport::transport_forced(
                ui,
                &mut s,
                Layout::Compact,
                Some(Forced {
                    action: Action::Loop,
                    vis,
                    focus,
                }),
            );
            let d = TimelineData {
                first: 1001,
                last: 1065,
                frame: 1012,
                in_pt: None,
                out_pt: None,
                marks: &[1012],
                cached: &[(1001, 1020)],
                fps: 24.0,
            };
            transport::timeline_forced(ui, &d, focus);
            transport::timeline_meta(ui, &d);
        });
    }
}
