//! Specimen section for controls::dropdown: a 1:1 copy of specimen.html "5.5–5.8" (first row, the dropdown cells and the
//! open list) and the "View" dropdown + open list of "8.10". Reference coordinates, shifted up by one constant per section
//! (DY1 / DY2) so both fit 1920×1080 (the reference is 1920 wide, x is unchanged).
use crate::ui::controls::dropdown::{Dropdown, Item};
use crate::ui::controls::{paint_line, line, Vis};
use crate::ui::icons::Icon;
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{Align, Color32, CornerRadius, Pos2, Rect, Stroke, StrokeKind, Ui, Vec2};

/// Section 5.5 starts at y 2076 in the reference; here at y 16.
pub const DY1: f32 = 16.0 - 2076.0;
/// Section 8.10 starts at y 4654 in the reference; here at y 232.
pub const DY2: f32 = 232.0 - 4654.0;

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let o = ui.max_rect().min;
    let at1 = |x: f32, y: f32| Pos2::new(o.x + x, o.y + y + DY1);
    let at2 = |x: f32, y: f32| Pos2::new(o.x + x, o.y + y + DY2);
    let p = ui.painter().clone();
    let text = |pos: Pos2, s: &str, font, lh: f32, c: Color32| {
        let g = line(&p, s, font, lh, f32::INFINITY);
        paint_line(&p, Rect::from_min_size(pos, Vec2::new(g.size().x, lh)), g, lh, Align::Min, c, 0.0);
    };
    let sheet = |r: Rect| {
        p.rect_filled(r, CornerRadius::same(4), t.bg_panel);
        p.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    };

    // ---- 5.5 -------------------------------------------------------------------------------------------------------
    sheet(Rect::from_min_size(at1(968.0, 2076.0), Vec2::new(920.0, 210.0)));
    text(at1(985.0, 2093.0), "5.5–5.8 · Dropdown, text field, numeric + slider, checkbox, radio", ty::title(), 20.0, t.text_primary);
    let cap = |x: f32, y: f32, s: &str| text(at1(x, y), s, ty::small(), 16.0, t.text_tertiary);

    let plain = [Item::new("Auto"), Item::new("Manual")];
    let (mut a, mut b) = (0usize, 0usize);
    Dropdown::new("dd-rest", &mut a, &plain, 150.0).at(at1(985.0, 2125.0)).name("Mode").show(ui);
    cap(985.0, 2153.0, "rest");
    Dropdown::new("dd-hover", &mut b, &plain, 150.0).at(at1(1147.0, 2125.0)).name("Mode").forced(Vis::Hover, false, false).show(ui);
    cap(1147.0, 2153.0, "hover");

    let long = [Item::new("render-node-07-with-a-long-name")];
    let mut c = 0usize;
    Dropdown::new("dd-long", &mut c, &long, 170.0).at(at1(1309.0, 2125.0)).name("Machine").icon(Icon::Machine).dot(t.error).show(ui);
    cap(1309.0, 2153.0, "long machine name truncates · offline dot");

    let gb = [Item::new("8 GB")];
    let mut d = 0usize;
    Dropdown::new("dd-dis", &mut d, &gb, 120.0).at(at1(1537.3, 2125.0)).name("Memory").disabled("Memory is set by the model").show(ui);
    cap(1537.3, 2153.0, "disabled");

    let machines = [
        Item::new("Automatic"),
        Item::new("This machine"),
        Item::new("endor-omarchy").note("online"),
        Item::new("render-node-07…").disabled("offline"),
    ];
    let mut e = 1usize;
    Dropdown::new("dd-open", &mut e, &machines, 200.0)
        .at(at1(1669.3, 2125.0))
        .name("Machine")
        .icon(Icon::Machine)
        .dot(t.success)
        .list_offset(Vec2::new(0.0, 4.0))
        .forced(Vis::Rest, true, false)
        .highlight(2)
        .show(ui);
    cap(1669.3, 2255.0, "open");

    // ---- 8.10 ------------------------------------------------------------------------------------------------------
    sheet(Rect::from_min_size(at2(388.0, 4654.0), Vec2::new(1500.0, 330.0)));
    text(at2(405.0, 4671.0), "8.10 · Viewer toolbar, transport bar and timeline", ty::title(), 20.0, t.text_primary);
    text(at2(765.6, 4671.0), "anchors kept in Focus mode", ty::title(), 20.0, t.text_tertiary);
    p.rect_stroke(Rect::from_min_size(at2(405.5, 4703.5), Vec2::new(1465.0, 259.0)), 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    p.rect_filled(Rect::from_min_size(at2(406.0, 4732.0), Vec2::new(1464.0, 230.0)), 0.0, t.viewer_surround);
    text(at2(414.0, 4709.5), "View", ty::small(), 16.0, t.text_tertiary);
    let views = [
        Item::new("Input").shortcut("Alt+1"),
        Item::new("Hint").shortcut("Alt+2"),
        Item::new("Mask").shortcut("Alt+3"),
        Item::new("FG").shortcut("Alt+4"),
        Item::new("Matte").shortcut("Alt+5"),
        Item::new("Comp").shortcut("Alt+6"),
        Item::new("Processed").shortcut("Alt+7"),
    ];
    let mut v = 5usize;
    Dropdown::new("dd-view", &mut v, &views, 96.0)
        .at(at2(442.08, 4706.5))
        .name("View")
        .list_width(180.0)
        .list_offset(Vec2::new(4.0, 3.0))
        .forced(Vis::Rest, true, false)
        .highlight(4)
        .show(ui);
}
