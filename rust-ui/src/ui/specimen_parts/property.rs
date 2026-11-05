//! Specimen section 8.2–8.3 · Properties (`--specimen-part property`). The sheet's six panels (Key 320, Deliver 320, the
//! two deliver-video panels, Review 320, Key 280) side by side, each 326 px apart at x = 4 + 326·i and shifted so its
//! border box top is at y 16 (reference tops 4693, 5219, 5616, 6265, 6494, 6909 → dy = 16 − top; dx = x_i − 43).
//! Dropdown / numeric / checkbox / slider are painted stand-ins with the sheet's geometry; the other agents own the real ones.
use crate::ui::controls::property::{properties_panel_with, Badge, PanelOpts, PropRows, Status};
use crate::ui::controls::Vis;
use crate::ui::controls::{ghost_button, primary_button, secondary_button, segmented, ActionBar, Blocker, Seg, SegWidth};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{self, Align2, Color32, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2};

/// Border-box heights of the sheet panels.
const HEIGHTS: [f32; 6] = [454.0, 387.0, 507.0, 157.0, 405.0, 339.0];
/// Pitch between panels in this part.
pub const PITCH: f32 = 326.0;
pub const X0: f32 = 4.0;
pub const TOP: f32 = 16.0;

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    for i in 0..6 {
        let w = if i == 5 { 280.0 } else { 320.0 };
        let outer = Rect::from_min_size(egui::pos2(X0 + PITCH * i as f32, TOP), Vec2::new(w + 2.0, HEIGHTS[i]));
        ui.painter().rect_stroke(outer, 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
        let inner = outer.shrink(1.0);
        ui.scope_builder(UiBuilder::new().max_rect(inner).id_salt(("prop-specimen", i)), |ui| panel(ui, i, w));
    }
    // Rows the sheet has no sample of (bible only): message rows, forced header states, error badge, collapsed section,
    // path row at 280, Details open, stacked File type row. Under the Comp panel (320) and the Key 280 panel.
    for (x, y, w, h) in [(X0 + PITCH * 3.0, 190.0, 320.0, 560.0), (X0 + PITCH * 5.0, 372.0, 280.0, 560.0)] {
        let outer = Rect::from_min_size(egui::pos2(x, y), Vec2::new(w + 2.0, h));
        ui.painter().rect_stroke(outer, 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
        ui.scope_builder(UiBuilder::new().max_rect(outer.shrink(1.0)).id_salt(("prop-extras", w as i32)), |ui| extras(ui, w));
    }
}

fn extras(ui: &mut Ui, w: f32) {
    let none = || -> Option<Box<dyn FnOnce(&mut Ui)>> { None };
    properties_panel_with(ui, "Properties", "Deliver", w, PanelOpts { header_menu: None, reduced_motion: true }, |b| {
        let (mut a, mut c, mut d, mut e) = (false, true, false, false);
        b.force_next_header(Vis::Hover, false);
        b.section("Format · Matte", None, noop_menu(), &mut a, |r| {
            file_type(r, 0);
            r.row("Compression", "Compression", |ui| dd(ui, None, None, "DWAB"));
            r.message(Status::Warning, "DWAB is lossy: HDR values are not exact.");
            r.row("Codec", "Codec", |ui| dd(ui, None, None, "ProRes"));
            r.message(Status::Error, "No ProRes encoder on this machine.");
            r.message(Status::Warning, "Lossy.");
        });
        b.section("Outputs", Some(Badge { status: Status::Error, text: "Failed" }), none(), &mut c, |_r| {});
        b.force_next_header(Vis::Rest, true);
        b.section("Destination", Some(Badge { status: Status::Warning, text: "Out of date" }), noop_menu(), &mut d, |r| {
            r.path("Output folder", "/home/npittas/shots/clips/V1-0062/Output");
            r.readonly("Machine", "endor-omarchy-with-a-very-long-name");
            r.details_state(true, |dd_| {
                dd_.id_row("Shot id", "3f2a9c1e-7b44-4d0a-9a51-2c6e0d8b1f77");
                dd_.id_row("Machine id", "m-4be1");
            });
            r.details(|_| {});
        });
        b.section("Despill", None, none(), &mut e, |r| { r.row("Despill", "Despill strength", |ui| { num(ui, "0.500", false); slider(ui) }); });
    }, None);
}

fn noop_menu() -> Option<Box<dyn FnOnce(&mut Ui)>> { Some(Box::new(|_ui: &mut Ui| {})) }

fn key_bar<'a>() -> ActionBar<'a> {
    ActionBar {
        more_menu: noop_menu(),
        secondary: None,
        primary: primary_button("Key shot"),
        blocker: Some(Blocker { text: "Can't key: endor-omarchy is offline.", fix_button: Some("Assign machine") }),
        status_text: None,
        busy: None,
        reduced_motion: true,
    }
}

fn export_bar<'a>() -> ActionBar<'a> {
    ActionBar { more_menu: noop_menu(), secondary: None, primary: primary_button("Export"), blocker: None, status_text: None, busy: None, reduced_motion: true }
}

fn panel(ui: &mut Ui, i: usize, w: f32) {
    let opts = |menu: bool| PanelOpts { header_menu: if menu { noop_menu() } else { None }, reduced_motion: true };
    let none = || -> Option<Box<dyn FnOnce(&mut Ui)>> { None };
    let more = || -> Option<Box<dyn FnOnce(&mut Ui)>> { noop_menu() };
    let mut open = false; // every section expanded (collapsed = false)
    let mut closed = true;
    match i {
        0 | 5 => {
            properties_panel_with(ui, "Properties", "Key", w, opts(true), |b| {
                b.section("Render", None, none(), &mut open, |r| {
                    r.row("Machine", "Machine · where this shot is keyed", |ui| dd(ui, Some(Icon::MachineNetwork), Some(true), "endor-omarchy"));
                });
                let mut o2 = false;
                b.section("Key engine", None, none(), &mut o2, |r| {
                    r.row("Model", "Model · the key engine", |ui| dd(ui, None, None, "CorridorKey v2"));
                    r.row("VRAM profile", "VRAM profile", |ui| dd(ui, None, None, "Auto"));
                    r.row("Temporal", "Temporal window", |ui| dd(ui, None, None, "Auto"));
                });
                let mut o3 = false;
                b.section("Matte", None, none(), &mut o3, |r| {
                    r.row("Strength", "Matte strength", |ui| { num(ui, "0.500", false); slider(ui) });
                    if i == 0 {
                        r.row("Erode", "Erode", |ui| { num(ui, "0", false); unit(ui, "px") });
                    }
                    r.row("Blur", "Blur", |ui| { num(ui, "1.5", true); unit(ui, "px") });
                    if i == 0 {
                        r.row("Fill holes", "Fill holes", |ui| checkbox(ui, true, None));
                    }
                });
                if i == 0 {
                    let mut o4 = false;
                    b.section("Despill", None, none(), &mut o4, |r| {
                        r.row("Despill", "Despill strength", |ui| { num(ui, "0.500", false); slider(ui) });
                    });
                }
            }, Some(key_bar()));
        }
        1 => {
            properties_panel_with(ui, "Properties", "Deliver", w, opts(false), |b| {
                b.section("Outputs", Some(Badge { status: Status::Warning, text: "Out of date" }), none(), &mut open, |r| {
                    output_row(r, "FG", false, false, "EXR · half · ZIP");
                    output_row(r, "Matte", true, true, "EXR · half · DWAB");
                    output_row(r, "Comp", false, false, "PNG · 16-bit");
                    output_row(r, "Processed", true, false, "MOV · ProRes 4444");
                });
                let mut o2 = false;
                b.section("Format · Matte", None, more(), &mut o2, |r| {
                    file_type(r, 0);
                    r.row("Bit depth", "Bit depth", |ui| dd(ui, None, None, "Half float"));
                    r.row("Compression", "Compression · DWAB is lossy: HDR values are not exact.", |ui| dd(ui, None, None, "DWAB"));
                });
                let mut o3 = false;
                b.section("Destination", None, none(), &mut o3, |r| {
                    r.path("Output folder", "/home/npittas/shots/clips/V1-0062/Output");
                });
            }, Some(export_bar()));
        }
        2 => {
            properties_panel_with(ui, "Properties", "Deliver", w, opts(false), |b| {
                b.section("Outputs", None, more(), &mut open, |r| {
                    output_row(r, "FG", false, false, "EXR · half · ZIP");
                    output_row(r, "Matte", true, false, "EXR · half · DWAB");
                    output_row(r, "Comp", false, false, "PNG · 16-bit");
                    output_row(r, "Processed", true, true, "MOV · ProRes 4444");
                });
                let mut o2 = false;
                b.section("Format · Processed", None, more(), &mut o2, |r| {
                    file_type(r, 3);
                    r.row("Codec", "Codec", |ui| dd(ui, None, None, "ProRes"));
                    r.row("Quality", "ProRes profile 4444 · Best 4444 XQ · High 4444 · Medium HQ · Small LT · Custom", |ui| dd(ui, None, None, "High · 4444"));
                    r.row("Encoder value", "ProRes qscale 1–31 · empty = encoder default", |ui| { num(ui, "9", true); unit(ui, "qscale 1–31") });
                    r.row("Frame rate", "Frame rate", |ui| dd(ui, None, None, "Source"));
                    r.row48("Range", |ui| {
                        let mut v = 2;
                        let o = [Seg::new(0, "In/Out"), Seg::new(1, "All frames"), Seg::new(2, "Done frames")];
                        let tw = ui.available_width();
                        segmented_in(ui, &mut v, &o, SegWidth::Equal, tw);
                    });
                });
                let mut o3 = false;
                b.section("Destination", None, none(), &mut o3, |r| {
                    r.path("Output folder", "/home/npittas/shots/clips/V1-0062/Output");
                    r.row_full(false, false, |ui, cols| {
                        let mut c = Rect::from_min_size(cols.row.min + Vec2::new(8.0, 0.0), Vec2::new(cols.row.width() - 16.0, 24.0));
                        c.min.y = cols.row.min.y;
                        checkbox_at(ui, c, true, Some("Keep partial results"), true);
                    });
                });
            }, Some(export_bar()));
        }
        3 => {
            properties_panel_with(ui, "Properties", "Deliver", w, opts(false), |b| {
                b.section("Format · Comp", None, more(), &mut open, |r| {
                    file_type(r, 2);
                    r.row("Codec", "Codec", |ui| dd(ui, None, None, "H.264"));
                    r.row("Quality", "H.264 CRF · Best 14 · High 18 · Medium 23 · Small 28 · Custom", |ui| dd(ui, None, None, "Custom (CRF 21)"));
                    r.row("Encoder value", "H.264 CRF 0–51 · stored value kept exactly", |ui| { num(ui, "21", true); unit(ui, "CRF 0–51") });
                });
            }, None);
        }
        _ => {
            properties_panel_with(ui, "Properties", "Review", w, opts(false), |b| {
                b.section("Result", None, none(), &mut open, |r| {
                    r.readonly("Keyed", "Today 14:20");
                    r.readonly("Engine", "CorridorKey v2");
                    r.readonly("VRAM profile", "High (24 GB)");
                    r.readonly("Machine", "endor-omarchy");
                    r.readonly("Frames", "108 of 108");
                });
                let mut o2 = false;
                b.section("Compare", None, none(), &mut o2, |r| {
                    r.row("A", "A · left view", |ui| dd(ui, None, None, "Comp"));
                    r.row("B", "B · right view", |ui| dd(ui, None, None, "Input"));
                    r.row("Wipe", "Wipe direction", |ui| {
                        let mut v = 0;
                        let o = [Seg::new(0, "Vertical"), Seg::new(1, "Horizontal")];
                        segmented_in(ui, &mut v, &o, SegWidth::Natural, 0.0);
                    });
                    r.row("Position", "Wipe position", |ui| { num(ui, "50", false); unit(ui, "%") });
                });
                b.section("Checks", None, none(), &mut closed, |r| { r.readonly("Missing frames", "None"); });
            }, Some(ActionBar {
                more_menu: noop_menu(),
                secondary: Some(secondary_button("Go to Deliver")),
                primary: ghost_button(""),
                blocker: None,
                status_text: None,
                busy: None,
                reduced_motion: true,
            }));
        }
    }
}

/// Outputs row: checkbox + name in the label column, the format summary in 11 mono (secondary; primary when selected).
fn output_row(r: &mut PropRows, name: &str, checked: bool, selected: bool, summary: &str) {
    r.row_full(selected, false, |ui, cols| {
        let t = tokens(ui);
        checkbox_at(ui, cols.label, checked, Some(name), false);
        let p = ui.painter().clone();
        let g = crate::ui::controls::line(&p, summary, ty::mono(), 14.0, f32::INFINITY);
        crate::ui::controls::paint_line(&p, cols.control, g, 14.0, egui::Align::Min, if selected { t.text_primary } else { t.text_secondary }, 0.0);
    });
}

/// The File type row: five segments, label width + 4 each, spare shared out (§8.3); stacked 48 when 174 does not fit.
fn file_type(r: &mut PropRows, sel: usize) {
    let labels = ["EXR", "PNG", "MP4", "MOV", "WebM"];
    r.row_or_stacked("File type", "File type", 174.0, |ui| {
        let mut v = sel;
        let o: Vec<Seg<usize>> = labels.iter().enumerate().map(|(i, l)| Seg::new(i, l)).collect();
        let tw = ui.available_width();
        segmented_in(ui, &mut v, &o, SegWidth::FitVariable, tw);
    });
}

fn segmented_in(ui: &mut Ui, v: &mut usize, o: &[Seg<usize>], width: SegWidth, track_w: f32) {
    if width == SegWidth::Natural { segmented(ui, v, o, width); } else { crate::ui::controls::Segmented::new(v, o).width(width).track_width(track_w).show(ui); }
}

// ---- painted stand-ins (the real controls come from other agents) ----

/// Dropdown field: 22 high, sunken, 1 px `border.control`, padding 8 / 6, gap 6, value 12, chevron 12.
fn dd(ui: &mut Ui, icon: Option<Icon>, red_dot: Option<bool>, value: &str) {
    let t = tokens(ui);
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(r, CornerRadius::same(3), t.bg_sunken);
    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.border_control), StrokeKind::Inside);
    let mut x = r.min.x + 9.0;
    if let Some(ic) = icon {
        icons::paint(&p, Rect::from_min_size(egui::pos2(x, (r.center().y - 7.0).round()), Vec2::splat(14.0)), ic, IconSize::S14, t.text_secondary);
        x += 14.0 + 6.0;
    }
    let chev = Rect::from_min_size(egui::pos2(r.max.x - 1.0 - 6.0 - 12.0, (r.center().y - 6.0).round()), Vec2::splat(12.0));
    icons::paint(&p, chev, Icon::ChevronDown, IconSize::S12, t.text_secondary);
    let mut right = chev.min.x - 6.0;
    if red_dot.is_some() {
        p.circle_filled(egui::pos2(right - 3.0, r.center().y), 3.0, t.error);
        right -= 6.0 + 6.0;
    }
    let tr = Rect::from_min_max(egui::pos2(x, r.min.y), egui::pos2(right, r.max.y));
    let g = crate::ui::controls::line(&p, value, ty::value(), 16.0, tr.width());
    crate::ui::controls::paint_line(&p, tr, g, 16.0, egui::Align::Min, t.text_primary, 0.0);
}

/// Numeric field: 56 × 22, mono 12 right-aligned, padding 6; changed value in `text.link`.
fn num(ui: &mut Ui, v: &str, changed: bool) {
    let t = tokens(ui);
    let (r, _) = ui.allocate_exact_size(Vec2::new(56.0, 22.0), Sense::hover());
    let p = ui.painter().clone();
    p.rect_filled(r, CornerRadius::same(3), t.bg_sunken);
    p.rect_stroke(r, CornerRadius::same(3), Stroke::new(1.0, t.border_control), StrokeKind::Inside);
    let g = crate::ui::controls::line(&p, v, ty::frame_field(), 16.0, f32::INFINITY);
    crate::ui::controls::paint_line(&p, Rect::from_min_max(r.min, egui::pos2(r.max.x - 7.0, r.max.y)), g, 16.0, egui::Align::Max, if changed { t.text_link } else { t.text_primary }, 0.0);
}

fn unit(ui: &mut Ui, s: &str) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    let g = crate::ui::controls::line(&p, s, ty::small(), 14.0, f32::INFINITY);
    let (r, _) = ui.allocate_exact_size(Vec2::new(g.size().x, 22.0), Sense::hover());
    crate::ui::controls::paint_line(&p, r, g, 14.0, egui::Align::Min, t.text_tertiary, 0.0);
}

/// Slider rail: 4 px sunken rail with `border.control`, fill and tick `text.tertiary`, 8 × 14 handle at 50 %.
fn slider(ui: &mut Ui) {
    let t = tokens(ui);
    let w = ui.available_width();
    let (r, _) = ui.allocate_exact_size(Vec2::new(w, 22.0), Sense::hover());
    let p = ui.painter().clone();
    let rail = Rect::from_min_size(egui::pos2(r.min.x, r.min.y + 9.0), Vec2::new(w, 6.0));
    p.rect_filled(rail, CornerRadius::same(2), t.bg_sunken);
    p.rect_stroke(rail, CornerRadius::same(2), Stroke::new(1.0, t.border_control), StrokeKind::Inside);
    let mid = (r.min.x + w * 0.5).round();
    p.rect_filled(Rect::from_min_size(egui::pos2(r.min.x, r.min.y + 9.0), Vec2::new(mid - r.min.x, 4.0)), CornerRadius::same(2), t.text_tertiary);
    p.rect_filled(Rect::from_min_size(egui::pos2(mid, r.min.y + 15.0), Vec2::new(1.0, 6.0)), 0.0, t.text_tertiary);
    let hd = Rect::from_min_size(egui::pos2(mid - 4.0, r.min.y + 4.0), Vec2::new(8.0, 14.0));
    p.rect_filled(hd, CornerRadius::same(2), t.text_primary);
    p.rect_stroke(hd, CornerRadius::same(2), Stroke::new(1.0, t.bg_app), StrokeKind::Inside);
}

fn checkbox(ui: &mut Ui, on: bool, label: Option<&str>) {
    let (r, _) = ui.allocate_exact_size(Vec2::new(14.0, 22.0), Sense::hover());
    checkbox_at(ui, r, on, label, true);
}

/// 14 px box at the left of `rect` (vertically centred), optional label after a 6 px gap.
fn checkbox_at(ui: &mut Ui, rect: Rect, on: bool, label: Option<&str>, primary_text: bool) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    let b = Rect::from_min_size(egui::pos2(rect.min.x, (rect.center().y - 7.0).round()), Vec2::splat(14.0));
    p.rect_filled(b, CornerRadius::same(2), if on { t.accent } else { t.bg_sunken });
    p.rect_stroke(b, CornerRadius::same(2), Stroke::new(1.0, if on { t.accent } else { t.border_control }), StrokeKind::Inside);
    if on { icons::paint(&p, Rect::from_min_size(b.min + Vec2::splat(1.0), Vec2::splat(12.0)), Icon::Check, IconSize::S12, Color32::WHITE); }
    if let Some(l) = label {
        let g = crate::ui::controls::line(&p, l, ty::label(), 16.0, f32::INFINITY);
        let tr = Rect::from_min_max(egui::pos2(b.max.x + 6.0, rect.min.y), rect.max);
        crate::ui::controls::paint_line(&p, tr, g, 16.0, egui::Align::Min, if primary_text { t.text_primary } else { t.text_secondary }, 0.0);
    }
    let _ = Align2::LEFT_TOP;
}
