//! Specimen section for controls::viewer_toolbar: 1:1 copies of specimen.html "8.10" (the viewer toolbar row with the open View
//! list, the viewer plate and the pixel readout), the Compact toolbar and the bottom of its viewer (900×640 frame), "7.2"
//! (inspection states and both readout positions), plus a Medium row with the Gain/Gamma popover open. Reference
//! coordinates, shifted up by one constant per part (x unchanged): DY_A 8.10, DY_C1 Compact toolbar, DY_C2 Compact viewer
//! bottom, DY_S 7.2; the Medium row has no reference and sits at its own y.
use crate::ui::controls::viewer_toolbar::{
    paint_background, pixel_readout, viewer_toolbar_forced, Background, Channel, Forced, Layout, View, ViewerState,
};
use crate::ui::controls::{line, paint_line};
use crate::ui::tokens::{tokens, Tokens};
use crate::ui::type_scale as ty;
use eframe::egui::{
    epaint::Mesh, Align, Color32, CornerRadius, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2,
};

/// Section 8.10 starts at y 4654 in the reference; here at y 16.
pub const DY_A: f32 = 16.0 - 4654.0;
/// The Compact toolbar (ref y 5168) sits at y 356.
pub const DY_C1: f32 = 356.0 - 5168.0;
/// The bottom 70 px of the Compact viewer (ref y 5544) sit at y 470.
pub const DY_C2: f32 = 470.0 - 5544.0;
/// Section 7.2 starts at y 9941 in the reference; here at y 548.
pub const DY_S: f32 = 548.0 - 9941.0;

fn rgb(r: u8, g: u8, b: u8) -> Color32 { Color32::from_rgb(r, g, b) }

/// Soft-edged keyed placeholder (7.2 `fig`): a 80×120 ellipse fading from 66 % to its edge plus four hair strands, over `bg`.
fn fig(ui: &Ui, rect: Rect, bg: Option<Background>, solid: Color32, c: Color32) {
    let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    match bg {
        Some(b) => paint_background(ui, rect, b),
        None => { p.rect_filled(rect, 0.0, solid); }
    }
    let centre = rect.min + Vec2::new(70.0 + 40.0, 22.0 + 60.0);
    let n = 48;
    let mut m = Mesh::default();
    m.colored_vertex(centre, c);
    for k in 0..n {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        m.colored_vertex(centre + Vec2::new(a.cos() * 40.0 * 0.66, a.sin() * 60.0 * 0.66), c);
    }
    for k in 0..n {
        let a = k as f32 / n as f32 * std::f32::consts::TAU;
        m.colored_vertex(centre + Vec2::new(a.cos() * 40.0, a.sin() * 60.0), Color32::TRANSPARENT);
    }
    for k in 0..n as u32 {
        let k1 = (k + 1) % n as u32;
        m.add_triangle(0, 1 + k, 1 + k1);
        m.add_triangle(1 + k, 1 + n as u32 + k, 1 + n as u32 + k1);
        m.add_triangle(1 + k, 1 + n as u32 + k1, 1 + k1);
    }
    p.add(Shape::mesh(m));
    for (i, deg) in [-14.0_f32, -4.0, 6.0, 16.0].into_iter().enumerate() {
        let b = rect.min + Vec2::new(104.0 + i as f32 * 4.0 + 0.5, 8.0 + 26.0);
        let (s, co) = deg.to_radians().sin_cos();
        let top = b + Vec2::new(26.0 * s, -26.0 * co);
        let w = Vec2::new(co, s) * 0.5;
        let mut m = Mesh::default();
        m.colored_vertex(top - w, Color32::TRANSPARENT);
        m.colored_vertex(top + w, Color32::TRANSPARENT);
        m.colored_vertex(b + w, c);
        m.colored_vertex(b - w, c);
        m.add_triangle(0, 1, 2);
        m.add_triangle(0, 2, 3);
        p.add(Shape::mesh(m));
    }
}

fn sw12(p: &Painter, t: &Tokens, r: Rect, bg: Background, dim: bool) {
    let a = if dim { 0.4 } else { 1.0 };
    let inner = r.shrink(1.0);
    let q = p.with_clip_rect(inner);
    match bg {
        Background::Black => { q.rect_filled(inner, 0.0, Color32::BLACK.gamma_multiply(a)); }
        Background::Grey => { q.rect_filled(inner, 0.0, t.viewer_surround.gamma_multiply(a)); }
        Background::White => { q.rect_filled(inner, 0.0, Color32::WHITE.gamma_multiply(a)); }
        Background::Checker => {
            q.rect_filled(inner, 0.0, rgb(0x66, 0x66, 0x66).gamma_multiply(a));
            for cy in 0..3 {
                for cx in 0..3 {
                    if (cx + cy) % 2 == 0 {
                        q.rect_filled(Rect::from_min_size(inner.min + Vec2::new(cx as f32 * 4.0, cy as f32 * 4.0), Vec2::splat(4.0)), 0.0, rgb(0x99, 0x99, 0x99).gamma_multiply(a));
                    }
                }
            }
        }
    }
    p.rect_stroke(r, 0.0, Stroke::new(1.0, t.border_control.gamma_multiply(a)), StrokeKind::Inside);
}

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let o = ui.max_rect().min;
    let p = ui.painter().clone();
    let at = |x: f32, y: f32, dy: f32| Pos2::new(o.x + x, o.y + y + dy);
    let text = |pos: Pos2, s: &str, font, lh: f32, c: Color32| {
        let g = line(&p, s, font, lh, f32::INFINITY);
        paint_line(&p, Rect::from_min_size(pos, Vec2::new(g.size().x, lh)), g, lh, Align::Min, c, 0.0);
    };
    let sheet = |r: Rect| {
        p.rect_filled(r, CornerRadius::same(4), t.bg_panel);
        p.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    };
    let far = Some(Pos2::new(o.x + 1700.0, o.y + 60.0));

    // ---- 8.10 ------------------------------------------------------------------------------------------------------------
    sheet(Rect::from_min_size(at(388.0, 4654.0, DY_A), Vec2::new(1500.0, 330.0)));
    text(at(405.0, 4671.0, DY_A), "8.10 · Viewer toolbar, transport bar and timeline", ty::title(), 20.0, t.text_primary);
    text(at(765.6, 4671.0, DY_A), "anchors kept in Focus mode", ty::title(), 20.0, t.text_tertiary);
    p.rect_stroke(Rect::from_min_size(at(405.5, 4703.5, DY_A), Vec2::new(1465.0, 259.0)), 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let viewer = Rect::from_min_size(at(406.0, 4732.0, DY_A), Vec2::new(1464.0, 230.0));
    p.rect_filled(viewer, 0.0, t.viewer_surround);
    let plate = Rect::from_min_size(at(953.0, 4743.0, DY_A), Vec2::new(370.0, 208.0));
    p.rect_filled(plate, 0.0, rgb(0x2F, 0x6F, 0x3A));
    for (l, h, w, r) in [(150.0, 120.0, 34.0, 17u8), (192.0, 126.0, 38.0, 19u8)] {
        let f = Rect::from_min_size(Pos2::new(plate.min.x + l, plate.max.y - 14.0 - h), Vec2::new(w, h));
        p.rect_filled(f, CornerRadius { nw: r, ne: r, sw: 4, se: 4 }, rgb(0x1E, 0x24, 0x20));
    }
    let row = Rect::from_min_size(at(406.0, 4704.0, DY_A), Vec2::new(1464.0, 28.0));
    let mut st = ViewerState { view: View::Comp, background: Background::Checker, gain: 1.5, zoom: 42.0, ..Default::default() };
    ui.scope_builder(UiBuilder::new().id_salt("vt-wide").max_rect(row), |ui| {
        let forced = Forced { view_list: Some(4), list_offset: Vec2::new(4.0, 3.0), sliders_open: false };
        viewer_toolbar_forced(ui, &mut st, Layout::Wide, true, "EXR · half · 3200×1800", false, Some(forced));
    });
    pixel_readout(ui, viewer, plate, far, [0.512, 0.430, 0.118, 1.0], (1204, 388), false);

    // ---- Medium (no reference): Format → info, Gain/Gamma → sliders popover ----------------------------------------------------
    text(Pos2::new(o.x + 406.0, o.y + 388.0), "Medium · Format to info, Gain/Gamma to sliders popover (open)", ty::small(), 16.0, t.text_tertiary);
    let mut sm = ViewerState { view: View::Comp, gain: 1.5, zoom: 42.0, ..Default::default() };
    ui.scope_builder(UiBuilder::new().id_salt("vt-medium").max_rect(Rect::from_min_size(Pos2::new(o.x + 406.0, o.y + 406.0), Vec2::new(1159.0, 28.0))), |ui| {
        viewer_toolbar_forced(ui, &mut sm, Layout::Medium, true, "EXR · half · 3200×1800", true, Some(Forced { sliders_open: true, ..Default::default() }));
    });

    // ---- Compact toolbar (900×640 frame) -----------------------------------------------------------------------------------
    text(at(406.0, 5150.0, DY_C1), "Compact · View/Overlay dropdowns, 1:1 in the zoom list", ty::small(), 16.0, t.text_tertiary);
    let mut sc = ViewerState { view: View::Comp, gain: 1.5, zoom: 19.0, ..Default::default() };
    ui.scope_builder(UiBuilder::new().id_salt("vt-compact").max_rect(Rect::from_min_size(at(406.0, 5168.0, DY_C1), Vec2::new(619.0, 28.0))), |ui| {
        viewer_toolbar_forced(ui, &mut sc, Layout::Compact, true, "EXR · half · 3200×1800", false, Some(Forced::default()));
    });

    // ---- Compact viewer, bottom 70 px: readout without x/y ----------------------------------------------------------------------
    let cview = Rect::from_min_size(at(406.0, 5196.0, DY_C2), Vec2::new(619.0, 418.0));
    ui.scope_builder(UiBuilder::new().max_rect(cview), |ui| {
        ui.set_clip_rect(Rect::from_min_size(at(406.0, 5544.0, DY_C2), Vec2::new(619.0, 70.0)));
        let p = ui.painter().clone();
        p.rect_filled(cview, 0.0, t.viewer_surround);
        let pic = Rect::from_min_size(at(406.0, 5231.0, DY_C2), Vec2::new(619.0, 348.0));
        p.rect_filled(pic, 0.0, rgb(0x2A, 0x3B, 0x33));
        let g = line(&p, "619×348 · 37.4 %", ty::small(), 16.0, f32::INFINITY);
        paint_line(&p, Rect::from_min_size(at(414.0, 5557.0, DY_C2), Vec2::new(96.0, 16.0)), g, 16.0, Align::Min, Color32::WHITE.gamma_multiply(0.8), 0.0);
        pixel_readout(ui, cview, pic, Some(Pos2::new(o.x + 800.0, o.y + 480.0)), [0.512, 0.430, 0.118, 1.0], (0, 0), true);
    });

    // ---- 7.2 ---------------------------------------------------------------------------------------------------------------------
    sheet(Rect::from_min_size(at(33.0, 9941.0, DY_S), Vec2::new(1204.0, 470.0)));
    text(at(49.0, 9958.0, DY_S), "7.2 · Viewer inspection and pixel readout", ty::title(), 20.0, t.text_primary);
    let ck = Some(Background::Checker);
    // (x, view, channel, swatch, figure background, figure colour, caption, no-transparency note)
    let states: [(f32, &str, &str, Background, Option<Background>, Color32, Color32, &str, bool); 5] = [
        (49.0, "Comp", "RGB", Background::Black, Some(Background::Black), Color32::BLACK, rgb(214, 170, 132), "Comp over Black", false),
        (281.0, "Comp", "RGB", Background::White, Some(Background::White), Color32::WHITE, rgb(214, 170, 132), "Comp over White", false),
        (513.0, "Comp", "RGB", Background::Checker, ck, Color32::BLACK, rgb(214, 170, 132), "Comp over Checkerboard", false),
        (745.0, "Comp", "A", Background::Black, Some(Background::Black), Color32::BLACK, Color32::WHITE, "Channel A · matte, soft edge", true),
        (985.83, "Input", "G", Background::Black, None, rgb(74, 74, 74), rgb(150, 150, 150), "Channel G on Input", true),
    ];
    for (x, v, ch, swb, fbg, solid, c, cap, dim) in states {
        let mut cx = x;
        let seg = |cx: &mut f32, s: &str, col: Color32| {
            let g = line(&p, s, ty::mono(), 14.0, f32::INFINITY);
            let w = g.size().x;
            paint_line(&p, Rect::from_min_size(at(*cx, 9990.0, DY_S), Vec2::new(w, 14.0)), g, 14.0, Align::Min, col, 0.0);
            *cx += w + 8.0;
        };
        seg(&mut cx, &format!("View {v} ·"), t.text_secondary);
        seg(&mut cx, ch, t.text_primary);
        seg(&mut cx, "·", t.text_secondary);
        sw12(&p, t, Rect::from_min_size(at(cx, 9991.0, DY_S), Vec2::splat(12.0)), swb, dim);
        cx += 12.0 + 8.0;
        if dim { seg(&mut cx, "no transparency", t.text_tertiary) }
        fig(ui, Rect::from_min_size(at(x, 10008.0, DY_S), Vec2::new(220.0, 124.0)), fbg, solid, c);
        text(at(x, 10140.0, DY_S), cap, ty::small(), 16.0, t.text_tertiary);
    }
    let _ = Channel::Rgb;
    // Readout positions: far pointer keeps the strip bottom-left; a pointer within 48 px flips it to the bottom-right.
    for (x, vals, ptr, cap) in [
        (49.0, (1204, 388), (300.0, 90.0), "Readout bottom-left, 4 px in (pointer far away)"),
        (581.0, (212, 1702), (40.0, 178.0), "Flipped bottom-right: pointer within 48 px of the left strip"),
    ] {
        let boxr = Rect::from_min_size(at(x, 10168.0, DY_S), Vec2::new(520.0, 200.0));
        p.rect_filled(boxr, 0.0, rgb(0x2A, 0x2A, 0x2A));
        let figr = Rect::from_min_size(at(x + 150.0, 10168.0 + 38.0, DY_S), Vec2::new(220.0, 162.0));
        fig(ui, figr, None, rgb(0x3A, 0x3A, 0x3A), rgb(214, 170, 132));
        let ptr = boxr.min + Vec2::new(ptr.0, ptr.1);
        pixel_readout(ui, boxr, figr, Some(ptr), [0.512, 0.430, 0.118, 1.0], vals, false);
        p.rect_filled(Rect::from_min_size(Pos2::new(ptr.x, ptr.y - 5.0), Vec2::new(1.0, 11.0)), 0.0, t.text_primary);
        p.rect_filled(Rect::from_min_size(Pos2::new(ptr.x - 5.0, ptr.y), Vec2::new(11.0, 1.0)), 0.0, t.text_primary);
        text(at(x, 10376.0, DY_S), cap, ty::small(), 16.0, t.text_tertiary);
    }
}
