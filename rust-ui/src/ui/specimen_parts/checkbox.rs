//! Reference x coordinates, dy = -2660, from the §5.8 sub-block of specimen.html.
use eframe::egui::{self, Align, Rect, Ui};
use crate::ui::{controls::{checkbox::Check, line, paint_line, Vis}, tokens::tokens, type_scale as ty};
pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    ui.painter().rect_filled(Rect::from_min_max(egui::pos2(968.0, 60.0), egui::pos2(1888.0, 160.0)), 4, t.bg_panel);
    let rect = Rect::from_min_size(egui::pos2(985.0, 79.0), egui::vec2(886.0, 16.0));
    let g = line(ui.painter(), "Checkbox and radio", ty::section_header(), 16.0, rect.width());
    paint_line(ui.painter(), rect, g, 16.0, Align::Min, t.text_secondary, 0.0);
    for (x, label, value, vis, focus, disabled) in [
        (985.0, "Off", Some(false), Vis::Rest, false, false),
        (1038.640625, "Hover", Some(false), Vis::Hover, false, false),
        (1108.375, "On", Some(true), Vis::Rest, false, false),
        (1161.171875, "On hover", Some(true), Vis::Hover, false, false),
        (1249.34375, "Mixed", None, Vis::Rest, false, false),
        (1319.59375, "Focus", Some(true), Vis::Rest, true, false),
        (1388.015625, "Disabled", Some(false), Vis::Rest, false, true),
        (1472.984375, "Disabled on", Some(true), Vis::Rest, false, true),
    ] {
        let mut value = value;
        let mut check = Check::mixed(&mut value, Some(label)).at(egui::pos2(x, 100.0)).force(vis, focus);
        if disabled { check = check.disabled("Unavailable in this state"); }
        ui.add(check);
    }
    for (x, label, selected, vis, focus, disabled) in [
        (985.0, "sRGB", false, Vis::Rest, false, false),
        (1050.75, "Linear", true, Vis::Rest, false, false),
        (1122.015625, "Hover", false, Vis::Hover, false, false),
        (1191.75, "Focus", true, Vis::Rest, true, false),
        (1260.171875, "Disabled", true, Vis::Rest, false, true),
    ] {
        let mut radio = Check::radio(selected, Some(label)).at(egui::pos2(x, 124.0)).force(vis, focus);
        if disabled { radio = radio.disabled("Unavailable in this state"); }
        ui.add(radio);
    }
}
