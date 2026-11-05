//! Specimen section for controls::table: 1:1 copy of the reference sheets "8.4 · Table rows" (bible specimen.html y 7371..7691,
//! left) and "8.4b · Table overflow, two axes" (right), reference x, shifted by DY so they fit a 1080 window.
//! Hover / selection / resize hover / scroll are forced through the same paint path as the real states.
use crate::ui::controls::table::{Column, Force, RowUi, Table};
use crate::ui::controls::{line, paint_line, secondary_button};
use crate::ui::icons::Icon;
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{self, Align, Align2, Color32, FontId, Rect, Stroke, StrokeKind, Ui, UiBuilder};
use std::collections::BTreeSet;

/// Reference y 7371 (top of both sheets) lands at y 16.
const DY: f32 = 16.0 - 7371.0;

#[derive(Clone, Copy)]
enum B { Dash, On, Off, Dis }
#[derive(Clone, Copy)]
enum Act { Nothing, More, Join, JoinOff }
struct M { name: &'static str, kind: &'static str, ip: &'static str, gpu: &'static str, status: Option<&'static str>, boxes: [B; 3], act: Act }

const fn m(name: &'static str, kind: &'static str, ip: &'static str, gpu: &'static str, status: Option<&'static str>, boxes: [B; 3], act: Act) -> M {
    M { name, kind, ip, gpu, status, boxes, act }
}

/// §7.4 Machines schema; mins as in the sheet's overflow table.
const COLS: [Column; 9] = [
    Column::new("Machine", 104.0), Column::new("Kind", 56.0), Column::new("Address", 88.0), Column::new("GPU", 124.0),
    Column::new("Status", 160.0).sortable(), Column::new("Connect", 56.0), Column::new("Accept work", 76.0),
    Column::new("Default for new shots", 128.0), Column::new("Actions", 88.0).flexible(),
];
/// The 8.4 sheet lets the browser spread 905 px over auto-sized columns; mins are lower there.
const COLS_ROWS: [Column; 9] = [
    Column::new("Machine", 96.0), Column::new("Kind", 56.0), Column::new("Address", 88.0), Column::new("GPU", 120.0),
    Column::new("Status", 72.0), Column::new("Connect", 56.0), Column::new("Accept work", 76.0),
    Column::new("Default for new shots", 120.0), Column::new("Actions", 64.0).flexible(),
];

fn machine_row(row: &mut RowUi, m: &M) {
    row.name(m.name);
    row.text(0, m.name);
    row.text(1, m.kind);
    row.mono(2, m.ip);
    if m.gpu == "Not reported" { row.muted(3, m.gpu) } else { row.text(3, m.gpu) }
    match m.status { Some(s) => row.text(4, s), None => row.dash(4) }
    for (i, b) in m.boxes.into_iter().enumerate() {
        match b {
            B::Dash => row.dash(5 + i),
            B::On => { row.checkbox(5 + i, true, true); }
            B::Off => { row.checkbox(5 + i, false, true); }
            B::Dis => { row.checkbox(5 + i, false, false); }
        }
    }
    match m.act {
        Act::Nothing => {}
        Act::More => { row.action(8, Icon::More, "More actions"); }
        Act::Join => { row.button(8, secondary_button("Join")); }
        Act::JoinOff => { row.button(8, secondary_button("Join").disabled("This machine does not allow joins")); }
    }
}

const ROWS_84: [M; 4] = [
    m("endor-omarchy", "Local", "192.168.1.20", "RTX 4090 · 24 GB", None, [B::Dash, B::On, B::Off], Act::More),
    m("kamino3", "Network", "192.168.1.31", "RTX 4090 · 24 GB +1", Some("Connected"), [B::On, B::On, B::On], Act::More),
    m("render-node-07-with-a-long-n…", "Network", "192.168.1.37", "Not reported", Some("Available"), [B::Dis, B::Dis, B::Dis], Act::Join),
    m("old-workstation", "Network", "192.168.1.12", "RTX 3090 · 24 GB", Some("Not used"), [B::Off, B::Off, B::Off], Act::Nothing),
];

const ROWS_84B: [M; 14] = [
    m("endor-omarchy", "Local", "192.168.1.20", "RTX 4090 · 24 GB", None, [B::Dash, B::On, B::Off], Act::Nothing),
    m("kamino3", "Network", "192.168.1.31", "RTX 4090 · 24 GB", Some("Connected"), [B::On, B::On, B::On], Act::Nothing),
    m("kamino-1", "Network", "192.168.1.32", "RTX 3090 · 24 GB", Some("Connected"), [B::On, B::On, B::Off], Act::More),
    m("kamino-2", "Network", "192.168.1.33", "RTX A6000 · 48 GB", Some("Enabled, not connected"), [B::Off, B::Off, B::Off], Act::Nothing),
    m("render-node-04", "Network", "192.168.1.34", "Not reported", Some("Available"), [B::Dis, B::Dis, B::Dis], Act::Join),
    m("render-node-05", "Network", "192.168.1.35", "RTX 3090 · 24 GB", Some("Join not allowed"), [B::Dis, B::Dis, B::Dis], Act::JoinOff),
    m("render-node-06", "Network", "192.168.1.36", "RTX 4090 · 24 GB", Some("Not used"), [B::Off, B::Off, B::Off], Act::Nothing),
    m("render-node-07", "Network", "192.168.1.37", "RTX A6000 · 48 GB", Some("Connected"), [B::On, B::On, B::Off], Act::Nothing),
    m("render-node-08", "Network", "192.168.1.38", "RTX 3090 · 24 GB", Some("Available"), [B::Dis, B::Dis, B::Dis], Act::Join),
    m("render-node-09", "Network", "192.168.1.39", "Not reported", Some("Enabled, not connected"), [B::Off, B::Off, B::Off], Act::Nothing),
    m("render-node-10", "Network", "192.168.1.40", "RTX 4090 · 24 GB", Some("Not used"), [B::Off, B::Off, B::Off], Act::Nothing),
    m("render-node-11", "Network", "192.168.1.41", "RTX 4090 · 24 GB", Some("Connected"), [B::On, B::On, B::Off], Act::Nothing),
    m("render-node-12", "Network", "192.168.1.42", "RTX A6000 · 48 GB", Some("Join not allowed"), [B::Dis, B::Dis, B::Dis], Act::JoinOff),
    m("render-node-13", "Network", "192.168.1.43", "RTX 3090 · 24 GB", Some("Available"), [B::Dis, B::Dis, B::Dis], Act::Join),
];

fn snap(v: f32) -> f32 { (v + 0.5).floor() }
fn snapped(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_max(egui::pos2(snap(x), snap(y)), egui::pos2(snap(x + w), snap(y + h)))
}
fn text(ui: &Ui, pos: egui::Pos2, s: &str, font: FontId, c: Color32) { ui.painter().text(pos, Align2::LEFT_TOP, s, font, c); }
/// Wrapped small text (line height 14) at a fixed top-left.
fn para(ui: &Ui, pos: egui::Pos2, s: &str, c: Color32, w: f32) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = w;
    job.append(s, 0.0, ty::format(ty::small(), c, 14.0));
    let g = ui.painter().layout_job(job);
    ui.painter().galley(pos, g, c);
}
fn sheet(ui: &Ui, t: &Tokens, x: f32, w: f32, title: &str) -> f32 {
    let r = snapped(x, 7371.0 + DY, w, 320.0);
    ui.painter().rect_filled(r, 4.0, t.bg_panel);
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let g = ui.painter().layout_no_wrap(title.into(), ty::title(), t.text_primary);
    let tw = g.size().x;
    ui.painter().galley(egui::pos2(x + 17.0, 7388.0 + DY), g, t.text_primary);
    x + 17.0 + tw + 12.0
}

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let _ = (line, paint_line, Align::Min);

    // 8.4 · Table rows
    let nx = sheet(ui, t, 32.0, 939.42, "8.4 · Table rows");
    text(ui, egui::pos2(nx, 7392.0 + DY), "header · rest · hover · selected · disabled", ty::small(), t.text_tertiary);
    let mut sel = BTreeSet::from([0usize]);
    let w84 = [192.81, 64.28, 95.20, 127.44, 76.69, 59.44, 80.62, 128.94];
    let pos = egui::pos2(49.0, 7420.0 + DY);
    ui.scope_builder(UiBuilder::new().max_rect(Rect::from_min_size(pos, egui::vec2(905.42, 120.0))), |ui| {
        Table::new("specimen-rows", &COLS_ROWS).label("Machines").widths(&w84).size(egui::vec2(905.42, 120.0))
            .rows(ROWS_84.len(), |row, i| machine_row(row, &ROWS_84[i]))
            .selected(&mut sel).forced(Force { hover_row: Some(1), ..Default::default() }).show(ui);
    });

    // 8.4b · Table overflow, two axes
    let nx = sheet(ui, t, 987.42, 900.58, "8.4b · Table overflow, two axes");
    para(ui, egui::pos2(nx, 7392.0 + DY), "560 × 200 · 9 columns · 14 rows · scrolled right 328 (to the end), down 1 row · Available / Join not allowed rows: checkboxes disabled until Join, Join button in Actions", t.text_tertiary, 673.84);
    let pos = egui::pos2(1004.42, 7440.0 + DY);
    let tov = Rect::from_min_size(pos, egui::vec2(560.0, 200.0));
    ui.painter().rect_stroke(snapped(pos.x, pos.y, 560.0, 200.0), 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Outside);
    ui.painter().rect_filled(snapped(pos.x, pos.y, 560.0, 200.0), 0.0, t.bg_panel);
    let w84b = [104.0, 56.0, 88.0, 124.0, 160.0, 56.0, 76.0, 128.0];
    let mut sel = BTreeSet::from([2usize]);
    let mut sort = Some(crate::ui::controls::table::Sort { col: 4, ascending: true });
    ui.scope_builder(UiBuilder::new().max_rect(tov), |ui| {
        Table::new("specimen-overflow", &COLS).label("Machines").widths(&w84b).size(egui::vec2(560.0, 200.0))
            .rows(ROWS_84B.len(), |row, i| machine_row(row, &ROWS_84B[i]))
            .selected(&mut sel).sort(&mut sort)
            .forced(Force { hover_row: Some(4), resize_hover: Some(4), scroll: Some(egui::vec2(328.0, 24.0)), focus_row: None }).show(ui);
    });
    para(ui, egui::pos2(1004.42, 7646.0 + DY), "Machines table · 9 columns, 14 rows · two-axis overflow: header scrolls sideways only; vertical bar starts below the header; 8×8 corner is bg.panel; 6 px resize zone on each header edge except the last (Status shown hovered)", t.text_tertiary, 866.58);
}
