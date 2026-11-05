//! Reference section "8.9 · Node graph" at its original x, shifted by dy = -5850 px (Light PNG has the same layout).
use crate::ui::{
    controls::{
        buttons::{icon_button, IconButtonSize},
        line, paint_line,
        node::{graph_canvas_forced, Forced, GraphView, NodeKind, NodeSpec, NodeState, Wire},
        tabs::{Tab, TabKind, TabStrip},
    },
    icons::Icon,
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, pos2, vec2, Align, Rect, Stroke, StrokeKind, Ui, UiBuilder};

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let dy = -5850.0;
    let r = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(x, y + dy), vec2(w, h));
    let p = ui.painter().clone();
    p.rect_filled(r(388.0, 5862.0, 1500.0, 286.0), 4.0, t.bg_panel);
    p.rect_stroke(r(388.0, 5862.0, 1500.0, 286.0), 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    for (i, s) in ["8.9 · Node", "graph"].into_iter().enumerate() {
        paint_line(&p, r(405.0, 5879.0 + 20.0 * i as f32, 90.0, 20.0), line(&p, s, ty::title(), 20.0, f32::INFINITY), 20.0, Align::Min, t.text_primary, 0.0);
    }
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = 1372.0;
    job.append("real size, 100 % · dock body 176 at 1920×1080 · five nodes in one row, 920 wide: Media 56 · Pre-matte 74 · Hint 110 · Key 110 · Deliver 128 · optional Roto + SAM row 2 (74) at +24 (content 250 > 176: the dock scrolls, scrollbar shown) · Media → Key runs in the lane between the rows · running footer 16 · text 11 px, never smaller · selected / running · model missing · no \"Out of date\" on nodes", 0.0, ty::format(ty::small(), t.text_tertiary, 14.0));
    p.galley(pos2(499.28, 5883.0 + dy), p.layout_job(job), t.text_tertiary);
    // Dock tab strip: 1 px divider top and sides, tabs left, graph tools right.
    let strip = r(405.0, 5931.0, 1466.0, 24.0);
    p.rect_stroke(strip, 0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    p.rect_filled(Rect::from_min_size(strip.left_bottom() - vec2(0.0, 1.0), vec2(strip.width(), 1.0)), 0, t.bg_panel);
    let mut active = 1;
    TabStrip::new(TabKind::Panel, &mut active, &[Tab::new("Actions"), Tab::new("Node graph").icon(Icon::Node)]).at(strip.min + vec2(1.0, 1.0)).show(ui);
    for (x, icon, tip) in [(1758.0, Icon::Fit, "Fit"), (1782.0, Icon::Minus, "Zoom out"), (1844.0, Icon::Plus, "Zoom in")] {
        let mut c = ui.new_child(UiBuilder::new().max_rect(r(x, 5932.0, 22.0, 22.0)));
        c.add(icon_button(icon, IconButtonSize::Normal22, tip));
    }
    paint_line(&p, r(1810.8, 5936.5, 26.4, 14.0), line(&p, "100%", ty::mono(), 14.0, f32::INFINITY), 14.0, Align::Min, t.text_secondary, 0.0);
    // The graph: 960 × 176 with a 1 px divider; canvas inside.
    let graph = r(405.0, 5955.0, 960.0, 176.0);
    p.rect_stroke(graph, 0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let mut canvas = ui.new_child(UiBuilder::new().max_rect(graph.shrink(1.0)).layout(egui::Layout::top_down(Align::Min)));
    canvas.set_clip_rect(graph.shrink(1.0));
    let n = |id, kind, title, icon, i: &[(&'static str, bool, bool)], o: &[(&'static str, bool, bool)], x: f32, y: f32, state, selected| NodeSpec {
        id, kind, title, icon, ports_in: i.to_vec(), ports_out: o.to_vec(), pos: pos2(x, y), state, selected,
    };
    let (img, pre, mask, hint, res) = (("image", false, true), ("pre-matte", true, true), ("mask", true, true), ("hint", false, true), ("result", false, true));
    let nodes = [
        n(1, NodeKind::Source, "Media", Icon::Film, &[], &[("image", false, true)], 12.0, 12.0, NodeState::Idle, false),
        n(2, NodeKind::Mask, "Pre-matte (GVM)", Icon::Gvm, &[img], &[("pre-matte", false, true)], 204.0, 12.0, NodeState::Missing, false),
        n(3, NodeKind::Mask, "Hint", Icon::Hint, &[img, pre, mask], &[("hint", false, true)], 396.0, 12.0, NodeState::Idle, false),
        n(4, NodeKind::Key, "Key", Icon::Key, &[img, hint, ("mask", true, true)], &[("result", false, true)], 588.0, 12.0, NodeState::Running(0.34), true),
        n(5, NodeKind::Output, "Deliver", Icon::Deliver, &[res], &[("FG", false, false), ("Matte", false, false), ("Comp", false, false), ("Processed", false, false)], 780.0, 12.0, NodeState::Idle, false),
        n(6, NodeKind::Mask, "Roto", Icon::Pen, &[img], &[("matte", false, true)], 12.0, 164.0, NodeState::Idle, false),
        n(7, NodeKind::Mask, "SAM", Icon::Sam, &[img], &[("mask", false, true)], 396.0, 164.0, NodeState::Idle, false),
    ];
    let w = |from, to, matte, optional| Wire { from, to, matte, optional };
    let wires = [
        w((1, 0), (2, 0), false, false), w((1, 0), (3, 0), false, false), w((1, 0), (4, 0), false, false), w((2, 0), (3, 1), true, true),
        w((3, 0), (4, 1), true, false), w((4, 0), (5, 0), false, false), w((1, 0), (6, 0), false, false), w((1, 0), (7, 0), false, false),
        w((6, 0), (3, 2), true, true), w((7, 0), (4, 2), true, true),
    ];
    let mut view = GraphView::default();
    graph_canvas_forced(&mut canvas, "node-specimen", &nodes, &wires, &mut view, Forced { reduced_motion: true, ..Default::default() });
}
