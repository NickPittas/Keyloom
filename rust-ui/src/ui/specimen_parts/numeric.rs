//! Specimen section for controls::numeric: 1:1 copy of the reference "Numeric field + slider (Nuke inline) in property
//! rows" block (5.5–5.8 sheet, x 985 y 2509) and the Gain / Gamma fields of "8.10 · Viewer toolbar" (x 1524 / 1598,
//! y 4706.5). Each block is shifted up by its own constant so both fit 1920×1080 (DY_ROWS, DY_TOOL).
use crate::ui::controls::numeric::{NumSpec, NumState, NumericField};
use crate::ui::controls::{line, paint_line};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{self, Align, Rect, Stroke, StrokeKind, Ui, UiBuilder};

pub const DY_ROWS: f32 = 40.0 - 2509.0;
pub const DY_TOOL: f32 = 420.0 - 4704.0;

fn r(x: f32, y: f32, w: f32, h: f32) -> Rect { Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, h)) }

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    let text = |rect: Rect, s: &str, font: egui::FontId, lh: f32, align: Align, c: egui::Color32| {
        let g = line(&p, s, font, lh, f32::INFINITY);
        paint_line(&p, rect, g, lh, align, c, 0.0);
    };
    // h3
    text(r(985.0, 2509.0 + DY_ROWS, 886.0, 16.0), "Numeric field + slider (Nuke inline) in property rows", ty::section_header(), 16.0, Align::Min, t.text_secondary);
    let bx = r(985.0, 2533.0 + DY_ROWS, 520.0, 194.0);
    p.rect_filled(bx, 0.0, t.bg_panel);
    p.rect_stroke(bx, 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);

    struct Row { label: &'static str, state: &'static str, st: NumState }
    let rows = [
        Row { label: "Despill", state: "rest", st: NumState::Rest },
        Row { label: "Hint quality", state: "hover", st: NumState::Hover },
        Row { label: "Grow / shrink", state: "dragging", st: NumState::Dragging },
        Row { label: "Erode", state: "editing", st: NumState::Editing },
        Row { label: "Gamma", state: "changed", st: NumState::Rest },
        Row { label: "Despill", state: "mixed", st: NumState::Rest },
        Row { label: "Feather", state: "disabled", st: NumState::Rest },
        Row { label: "Decode workers", state: "auto", st: NumState::Rest },
    ];
    let mut auto_on = true;
    let (mut v0, mut v1, mut v2, mut v3, mut v4, mut v5, mut v6, mut v7) = (0.5, 0.95, 3.0, 2.0, 1.2, 0.5, 0.0, 0.0);
    for (i, row) in rows.iter().enumerate() {
        let ry = 2534.0 + 24.0 * i as f32 + DY_ROWS;
        let disabled = row.state == "disabled";
        text(r(994.0, ry, 112.0, 24.0), row.label, ty::label(), 16.0, Align::Min, if disabled { t.text_disabled } else { t.text_secondary });
        text(r(1426.0, ry, 70.0, 24.0), row.state, ty::small(), 16.0, Align::Center, t.text_tertiary);
        let pc = r(1114.0, ry + 1.0, 304.0, 22.0);
        let small = |s: &str, x: f32| text(r(x, ry, 300.0, 24.0), s, ty::small(), 14.0, Align::Min, t.text_tertiary);
        ui.scope_builder(UiBuilder::new().max_rect(pc), |ui| {
            match i {
                0 => { NumericField::new("n0", &mut v0, &NumSpec::unit_float(0.5)).name("Despill").slider(true).forced(row.st).show(ui); }
                1 => { NumericField::new("n1", &mut v1, &NumSpec::new(0.95, 0.0, 1.0, 0.01, 2)).name("Hint quality").slider(true).default_tick(false).forced(row.st).show(ui); }
                2 => { NumericField::new("n2", &mut v2, &NumSpec::new(2.5, 0.0, 5.0, 1.0, 0)).name("Grow / shrink").slider(true).unit("px").forced(row.st).show(ui); }
                3 => { NumericField::new("n3", &mut v3, &NumSpec::pixels(0.0, 0.0, 100.0, false)).name("Erode").unit("px").forced(row.st).show(ui); small("click = type · Enter / Esc / Tab", 1190.375); }
                4 => { NumericField::new("n4", &mut v4, &NumSpec::new(1.0, 0.0, 2.0, 0.01, 3)).name("Gamma").slider(true).forced(row.st).show(ui); }
                5 => { NumericField::new("n5", &mut v5, &NumSpec::unit_float(0.5)).name("Despill").mixed(true).forced(row.st).show(ui); small("3 shots differ · drag sets all", 1174.0); }
                6 => { NumericField::new("n6", &mut v6, &NumSpec::pixels(0.0, 0.0, 100.0, true).soft(0.0, 100.0)).name("Feather").slider(true).unit("px").disabled("Not available for this shot").forced(row.st).show(ui); }
                _ => { NumericField::new("n7", &mut v7, &NumSpec::integer(0, 0, 64)).name("Decode workers").auto(&mut auto_on).forced(row.st).show(ui); small("blank = Auto, never \"0\"", 1174.0); }
            }
        });
    }

    // 8.10 viewer toolbar: Gain 1.50 (changed) and Gamma 1.000, 48 wide, at the reference x.
    let ty0 = 4704.0 + DY_TOOL;
    text(r(406.0, ty0 - 24.0, 900.0, 16.0), "8.10 · Viewer toolbar: Gain (2 decimals) and Gamma (3 decimals), 48 wide", ty::section_header(), 16.0, Align::Min, t.text_secondary);
    p.rect_filled(r(406.0, ty0, 1464.0, 28.0), 0.0, t.bg_panel);
    p.rect_filled(r(406.0, ty0 + 27.0, 1464.0, 1.0), 0.0, t.border_divider);
    let fy = (4706.5 + DY_TOOL + 0.5).floor();
    let (mut gain, mut gamma) = (1.5, 1.0);
    ui.scope_builder(UiBuilder::new().max_rect(r(1524.0, fy, 48.0, 22.0)), |ui| { NumericField::new("gain", &mut gain, &NumSpec::gain()).name("Gain").width(48.0).forced(NumState::Rest).show(ui); });
    ui.scope_builder(UiBuilder::new().max_rect(r(1598.0, fy, 48.0, 22.0)), |ui| { NumericField::new("gamma", &mut gamma, &NumSpec::gamma()).name("Gamma").width(48.0).forced(NumState::Rest).show(ui); });
}
