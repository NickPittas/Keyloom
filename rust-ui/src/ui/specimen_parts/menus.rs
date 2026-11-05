//! Specimen: sheets "5.9b · Narrow menu bar" and "5.9–5.11 · Menu bar, menus, tooltip" (reference specimen.html y 2836
//! and 3088), one constant dy = -2836 so both fit a 1080 window. Forced states go through the real paint path.
use crate::ui::controls::menus::{paint_tooltip, Menu, MenuBar, MenuBarRight, MenuItem, MenuList};
use crate::ui::controls::Vis;
use crate::ui::icons::Icon;
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{self, pos2, vec2, Color32, FontId, Pos2, Rect, Stroke, StrokeKind, Ui};

const DY: f32 = -2836.0;
const LONG: &str = "V1_interview_finale_grade_master_v12_client_review_final_approved_director_cut_2026_11_05_export_hdr";

const RECENT: &[MenuItem<'static>] = &[MenuItem::new("V1 interview"), MenuItem::new("Title sequence")];
const FILE: &[MenuItem<'static>] = &[
    MenuItem::new("New project…").shortcut("Ctrl+N"),
    MenuItem::new("Open project…").shortcut("Ctrl+O"),
    MenuItem::new("Open recent").submenu(RECENT),
    MenuItem::sep(),
    MenuItem::new("Save project").shortcut("Ctrl+S").disabled(""),
    MenuItem::new("Project info…"),
    MenuItem::sep(),
    MenuItem::new("Import shots…").shortcut("Ctrl+I"),
    MenuItem::new("Import folder…"),
    MenuItem::sep(),
    MenuItem::new("Quit").shortcut("Ctrl+Q"),
];
const SHOT: &[MenuItem<'static>] = &[
    MenuItem::new("Open").icon(Icon::Open),
    MenuItem::new("Key this shot…").icon(Icon::Key),
    MenuItem::new("Reveal source").icon(Icon::Reveal),
    MenuItem::new("Reveal exports").disabled("nothing exported yet"),
    MenuItem::sep(),
    MenuItem::new("Remove shot…").icon(Icon::Trash).destructive(),
];
const EMPTY: &[MenuItem<'static>] = &[];
const MENUS: [Menu<'static>; 6] = [
    Menu { label: "File", items: FILE },
    Menu { label: "Edit", items: EMPTY },
    Menu { label: "View", items: EMPTY },
    Menu { label: "Shot", items: SHOT },
    Menu { label: "Window", items: EMPTY },
    Menu { label: "Help", items: EMPTY },
];

fn pt(x: f32, y: f32) -> Pos2 { pos2(x, y + DY) }

fn snap(v: f32) -> f32 { (v + 0.5).floor() }

fn sheet(ui: &Ui, t: &Tokens, x: f32, y: f32, w: f32, h: f32) {
    let r = Rect::from_min_max(pos2(snap(x), snap(y + DY)), pos2(snap(x + w), snap(y + DY + h)));
    ui.painter().rect_filled(r, 4.0, t.bg_panel);
    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
}

fn text(ui: &Ui, p: Pos2, s: &str, font: FontId, lh: f32, col: Color32) -> f32 {
    let mut job = egui::text::LayoutJob::default();
    job.append(s, 0.0, ty::format(font, col, lh));
    let g = ui.painter().layout_job(job);
    let w = g.size().x;
    ui.painter().galley(p.round(), g, col);
    w
}

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    // 5.9b · Narrow menu bar.
    sheet(ui, t, 32.0, 2836.0, 1856.0, 236.0);
    text(ui, pt(49.0, 2853.0), "5.9b · Narrow menu bar", ty::title(), 20.0, t.text_primary);
    text(ui, pt(233.22, 2857.0), "§5.9 overflow order · §7.6", ty::small(), 14.0, t.text_tertiary);
    let narrow = [
        (2893.0, 900.0, "900 · save state is only its 6 px dot (tooltip \"Unsaved changes\"); name truncates at the end with \"…\" (tooltip: full name)"),
        (2943.0, 569.0, "569 · the floor: 260 menus + 8 + 80 name + 8 + 6 dot + 8 + 1 + 8 + 190 credit; name is at its 80 px minimum"),
        (2993.0, 520.0, "below 569 · project name hidden (the window title still shows it); dot, menus and credit kept"),
    ];
    for (y, w, cap) in narrow {
        text(ui, pt(49.0, y), cap, ty::small(), 14.0, t.text_tertiary);
        MenuBar::new(&MENUS, MenuBarRight { project: LONG, unsaved: true }).at(pt(49.0, y + 18.0)).width(w).forced(vec![], None, None).show(ui);
    }
    text(ui, pt(49.0, 3039.0), "narrow menu bar · overflow order §5.9: save state → dot, project name → 80 px → hidden; menus and credit never truncate", ty::small(), 14.0, t.text_tertiary);

    // 5.9–5.11 · Menu bar, menus, tooltip.
    sheet(ui, t, 32.0, 3088.0, 766.59375, 884.0);
    text(ui, pt(49.0, 3105.0), "5.9–5.11 · Menu bar, menus, tooltip", ty::title(), 20.0, t.text_primary);
    MenuBar::new(&MENUS, MenuBarRight { project: "Spec", unsaved: true }).at(pt(49.0, 3137.0)).width(732.59375)
        .forced(vec![Vis::Rest, Vis::Hover], Some(0), None).show(ui);
    MenuList::new(FILE).at(pt(49.0, 3163.0)).width(240.0).highlight(Some(1)).show(ui);
    MenuList::new(SHOT).at(pt(305.0, 3163.0)).width(250.0).header("Shot V1-0046_A002C008…").highlight(Some(1)).show(ui);
    text(ui, pt(305.0, 3316.0), "context menu: target header, destructive last", ty::small(), 14.0, t.text_tertiary);
    paint_tooltip(
        ui, pt(49.0, 3388.0), 300.0, "", None,
        "Temporal window: how many neighbouring frames the key sees at once. More frames give steadier edges and use more memory.",
        Some("Tooltip · max 320 · 11/14"),
    );
    let _ = vec2(0.0, 0.0);
}
