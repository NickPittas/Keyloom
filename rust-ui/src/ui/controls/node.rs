//! Node graph: nodes, wires, canvas (bible §8.9, §2.7, §5.14, §9, §10.2, §12). Drawing only: the caller owns the graph.
//! Node = 152 wide, 22 header + 18 per port row (inputs first, then outputs) + 16 footer, always reserved.
use super::{
    chips::{self, Progress},
    keyboard_focus, line,
    menus::{context_menu, tooltip, MenuItem},
    paint_line, scroll, spinner,
};
use crate::ui::{icons::{self, Icon, IconSize}, tokens::{tokens, Tokens}, type_scale as ty};
use eframe::egui::{
    self, accesskit::Role, pos2, text::LayoutJob, vec2, Align, Color32, FontFamily, FontId, Id, Key, Pos2, Rect,
    Sense, Shape, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo,
};
use std::sync::Arc;

pub const NODE_W: f32 = 152.0;
pub const HEADER: f32 = 22.0;
pub const ROW: f32 = 18.0;
pub const FOOTER: f32 = 16.0;
/// Margin around the nodes in the scrollable content (sheet: x 12, y 12).
pub const PAD: f32 = 12.0;
/// Gap between row 1's tallest node and the wire lane (lane = 12 below row 1, row 2 = 24 below).
const LANE_GAP: f32 = 12.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeKind { Source, Mask, Key, Output }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum NodeState { Idle, Running(f32), Missing, Selected, Bypassed }
/// (label, optional, connected)
pub type Port<'a> = (&'a str, bool, bool);

#[derive(Clone, Debug)]
pub struct NodeSpec<'a> {
    pub id: u32,
    pub kind: NodeKind,
    pub title: &'a str,
    pub icon: Icon,
    pub ports_in: Vec<Port<'a>>,
    pub ports_out: Vec<Port<'a>>,
    /// Top-left in graph units (zoom 1 = pixels), relative to the content origin.
    pub pos: Pos2,
    pub state: NodeState,
    /// Selection outline; independent of `state` (a selected node can be running). `NodeState::Selected` implies it.
    pub selected: bool,
}
/// `from` = (node id, output port index), `to` = (node id, input port index).
#[derive(Clone, Copy, Debug)]
pub struct Wire { pub from: (u32, usize), pub to: (u32, usize), pub matte: bool, pub optional: bool }
/// `zoom` 0.5..=2.0 (text only at >= 1); `offset` is the scroll offset reported by the canvas each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GraphView { pub zoom: f32, pub offset: Vec2 }
impl Default for GraphView { fn default() -> Self { GraphView { zoom: 1.0, offset: Vec2::ZERO } } }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeAction { OpenSettings, ShowInViewer, Bypass }
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct GraphOut { pub clicked: Option<u32>, pub context: Option<(u32, NodeAction)> }
/// Specimen / test only: pointer and keyboard looks, through the real paint path.
#[derive(Clone, Copy, Debug, Default)]
pub struct Forced { pub hover: Option<u32>, pub focus: Option<u32>, pub reduced_motion: bool, pub scroll: Option<scroll::State> }

// ---------------------------------------------------------------------------------------------------------------
// Geometry (graph units)
// ---------------------------------------------------------------------------------------------------------------

pub fn node_height(ports_in: usize, ports_out: usize) -> f32 { HEADER + ROW * (ports_in + ports_out) as f32 + FOOTER }
pub fn node_rect(n: &NodeSpec) -> Rect { Rect::from_min_size(n.pos, vec2(NODE_W, node_height(n.ports_in.len(), n.ports_out.len()))) }
/// Centre of a port dot: on the left edge (input) or right edge (output).
pub fn port_pos(n: &NodeSpec, input: bool, i: usize) -> Pos2 {
    let row = if input { i } else { n.ports_in.len() + i };
    pos2(n.pos.x + if input { 0.0 } else { NODE_W }, n.pos.y + HEADER + ROW * row as f32 + ROW / 2.0)
}
fn is_matte(label: &str) -> bool { ["hint", "matte", "mask"].iter().any(|k| label.contains(k)) }

enum Seg { L(Pos2, Pos2), C([Pos2; 4]) }
fn cubic(c: &[Pos2; 4], t: f32) -> Pos2 {
    let u = 1.0 - t;
    (c[0].to_vec2() * (u * u * u) + c[1].to_vec2() * (3.0 * u * u * t) + c[2].to_vec2() * (3.0 * u * t * t) + c[3].to_vec2() * (t * t * t)).to_pos2()
}
/// Polyline of a route; lines are subdivided to `step` px so overlap tests see every part.
fn flatten(segs: &[Seg], step: f32) -> Vec<Pos2> {
    let mut pts: Vec<Pos2> = Vec::new();
    for s in segs {
        match s {
            Seg::L(a, b) => {
                let n = ((*b - *a).length() / step).ceil().max(1.0) as usize;
                pts.extend((0..=n).map(|k| a.lerp(*b, k as f32 / n as f32)));
            }
            Seg::C(c) => pts.extend((0..=16).map(|k| cubic(c, k as f32 / 16.0))),
        }
    }
    pts.dedup();
    pts
}
/// Wire route (bible §8.9): a 1.5 px S-curve between neighbouring columns; anything that would cross a node, or span
/// more than one column, drops into the lane below row 1 (12 below its tallest node), runs along it and rises into
/// the target. Targets at or left of the source (Roto's image input) are entered from the left.
fn route(w: &Wire, nodes: &[NodeSpec]) -> Vec<Seg> {
    let (Some(a), Some(b)) = (nodes.iter().find(|n| n.id == w.from.0), nodes.iter().find(|n| n.id == w.to.0)) else { return vec![] };
    let (s, e) = (port_pos(a, false, w.from.1), port_pos(b, true, w.to.1));
    let direct = [s, s + vec2(20.0, 0.0), e - vec2(20.0, 0.0), e];
    let dx = e.x - s.x;
    if dx > 0.0 && dx <= 100.0 {
        let pts = flatten(&[Seg::C(direct)], 4.0);
        let clear = pts[1..pts.len() - 1].iter().all(|p| !nodes.iter().any(|n| node_rect(n).shrink(0.5).contains(*p)));
        if clear { return vec![Seg::C(direct)] }
    }
    let top = nodes.iter().map(|n| n.pos.y).fold(f32::INFINITY, f32::min);
    let lane = nodes.iter().filter(|n| n.pos.y <= top + 1.0).map(|n| node_rect(n).bottom()).fold(0.0, f32::max) + LANE_GAP;
    if dx >= 64.0 {
        vec![
            Seg::C([s, s + vec2(20.0, 0.0), pos2(s.x + 12.0, lane), pos2(s.x + 32.0, lane)]),
            Seg::L(pos2(s.x + 32.0, lane), pos2(e.x - 32.0, lane)),
            Seg::C([pos2(e.x - 32.0, lane), pos2(e.x - 12.0, lane), pos2(e.x - 20.0, e.y), e]),
        ]
    } else {
        vec![
            Seg::C([s, s + vec2(18.0, 0.0), pos2(s.x + 18.0, lane), pos2(s.x, lane)]),
            Seg::L(pos2(s.x, lane), pos2(e.x - 4.0, lane)),
            Seg::C([pos2(e.x - 4.0, lane), pos2(e.x - 12.0, lane), pos2(e.x - 12.0, e.y), pos2(e.x - 4.0, e.y)]),
            Seg::L(pos2(e.x - 4.0, e.y), e),
        ]
    }
}
/// Size of the scrollable content: nodes plus the margin on the right and bottom.
fn content_extent(nodes: &[NodeSpec]) -> Vec2 {
    nodes.iter().fold(Vec2::ZERO, |m, n| { let r = node_rect(n); vec2(m.x.max(r.right() + PAD), m.y.max(r.bottom() + PAD)) })
}

// ---------------------------------------------------------------------------------------------------------------
// Painting
// ---------------------------------------------------------------------------------------------------------------

fn kind_color(t: &Tokens, k: NodeKind) -> Color32 {
    match k { NodeKind::Source => t.node_kind_source, NodeKind::Mask => t.node_kind_mask, NodeKind::Key => t.node_kind_key, NodeKind::Output => t.node_kind_output }
}
fn wire_color(t: &Tokens, matte: bool) -> Color32 { if matte { t.wire_matte } else { t.wire_image } }
fn semibold() -> FontId { FontId::new(11.0, FontFamily::Name("SemiBold".into())) }
/// Label (secondary) with an optional "(opt)" suffix (tertiary) as one galley.
fn port_label(p: &egui::Painter, t: &Tokens, label: &str, optional: bool, max_w: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_rows = 1;
    job.wrap.max_width = max_w;
    job.wrap.overflow_character = Some('…');
    job.wrap.break_anywhere = true;
    job.append(label, 0.0, ty::format(ty::small(), t.text_secondary, 14.0));
    if optional { job.append(" (opt)", 0.0, ty::format(ty::small(), t.text_tertiary, 14.0)) }
    p.layout_job(job)
}
fn dot(p: &egui::Painter, t: &Tokens, c: Pos2, ring: Color32, optional: bool, connected: bool, z: f32) {
    let r = 4.0 * z.min(1.0).max(0.5);
    let fill = if connected { ring } else { t.node_fill };
    p.circle_filled(c, r, fill);
    let (rr, w) = (r - 0.75, 1.5);
    if optional {
        let pts: Vec<Pos2> = (0..=40).map(|k| c + Vec2::angled(k as f32 / 40.0 * std::f32::consts::TAU) * rr).collect();
        let len = std::f32::consts::TAU * rr;
        p.add(Shape::dashed_line(&pts, Stroke::new(w, ring), len / 6.0 * 0.5, len / 6.0 * 0.5));
    } else {
        p.circle_stroke(c, rr, Stroke::new(w, ring));
    }
}

/// One node at `origin` (screen position of graph (0, 0)). Returns its screen rect.
#[allow(clippy::too_many_arguments)]
fn paint_node(ui: &mut Ui, n: &NodeSpec, origin: Pos2, z: f32, hover: bool, focus: bool, reduced_motion: bool) -> Rect {
    let t = tokens(ui);
    let mut p = ui.painter().clone();
    let bypass = n.state == NodeState::Bypassed;
    if bypass { p.set_opacity(0.5) }
    let g = node_rect(n);
    let r = Rect::from_min_size(origin + g.min.to_vec2() * z, g.size() * z);
    let radius = (4.0 * z).max(2.0);
    let border = if hover { t.border_hover } else { t.node_border };
    p.rect_filled(r, radius, t.node_fill);
    if bypass {
        let c = r.shrink(0.5);
        for (a, b) in [(c.left_top(), c.right_top()), (c.right_top(), c.right_bottom()), (c.right_bottom(), c.left_bottom()), (c.left_bottom(), c.left_top())] {
            p.extend(Shape::dashed_line(&[a, b], Stroke::new(1.0, border), 4.0, 3.0));
        }
    } else {
        p.rect_stroke(r, radius, Stroke::new(1.0, border), StrokeKind::Inside);
    }
    let kind = kind_color(t, n.kind);
    p.with_clip_rect(Rect::from_min_size(r.min, vec2(r.width(), (3.0 * z).max(1.5)))).rect_filled(r, radius, kind);
    if n.selected || n.state == NodeState::Selected {
        p.rect_stroke(r.expand(2.0), radius + 2.0, Stroke::new(2.0, t.node_selected), StrokeKind::Inside);
    }
    if focus && keyboard_focus(ui.ctx()) {
        p.rect_stroke(r.shrink(1.0), radius, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
    }
    // Ports (dots stay on the edges at every zoom).
    let outputs_are_rows = n.kind == NodeKind::Output;
    for (i, &(label, optional, connected)) in n.ports_in.iter().enumerate() {
        let c = origin + port_pos(n, true, i).to_vec2() * z;
        dot(&p, t, c, wire_color(t, is_matte(label)), optional, connected, z);
    }
    for (i, &(label, optional, connected)) in n.ports_out.iter().enumerate() {
        if outputs_are_rows { continue } // Deliver's rows are deliverables, not wire ends
        let c = origin + port_pos(n, false, i).to_vec2() * z;
        dot(&p, t, c, wire_color(t, is_matte(label)), optional, connected, z);
    }
    // Header icons: kind icon 14 at x 9 / y 7; spinner 12 at the right while running.
    let (icon_size, icon_rect) = if z >= 1.0 {
        (IconSize::S14, Rect::from_min_size(r.min + vec2(9.0, 7.0), vec2(14.0, 14.0)))
    } else {
        (IconSize::S12, Rect::from_min_size(r.min + vec2(4.0, (HEADER * z - 12.0) / 2.0 + 1.0), vec2(12.0, 12.0)))
    };
    icons::paint(&p, icon_rect, n.icon, icon_size, t.text_primary);
    if let NodeState::Running(_) = n.state {
        let sr = if z >= 1.0 { Rect::from_min_size(pos2(r.right() - 21.0, r.top() + 8.0), vec2(12.0, 12.0)) } else { Rect::from_min_size(pos2(r.right() - 16.0, icon_rect.top()), vec2(12.0, 12.0)) };
        spinner(ui, sr, t.busy_fill, reduced_motion);
    }
    if z < 1.0 { return r } // text never below 11 px: below 100 % only kind bar and icons
    // Text
    let title_w = r.width() - 29.0 - 9.0 - if matches!(n.state, NodeState::Running(_)) { 18.0 } else { 0.0 };
    let tg = line(&p, n.title, semibold(), 14.0, title_w);
    paint_line(&p, Rect::from_min_size(r.min + vec2(29.0, 3.0), vec2(title_w, 19.0)), tg, 14.0, Align::Min, t.text_primary, 0.0);
    for (i, &(label, optional, _)) in n.ports_in.iter().enumerate() {
        let row = Rect::from_min_size(r.min + vec2(11.0, HEADER + ROW * i as f32), vec2(r.width() - 22.0, ROW));
        paint_line(&p, row, port_label(&p, t, label, optional, row.width()), 14.0, Align::Min, t.text_secondary, 0.0);
    }
    for (i, &(label, optional, _)) in n.ports_out.iter().enumerate() {
        let row = Rect::from_min_size(r.min + vec2(11.0, HEADER + ROW * (n.ports_in.len() + i) as f32), vec2(r.width() - 22.0, ROW));
        paint_line(&p, row, port_label(&p, t, label, optional, row.width()), 14.0, Align::Max, t.text_secondary, 0.0);
    }
    let foot = Rect::from_min_size(pos2(r.left(), r.bottom() - FOOTER), vec2(r.width(), FOOTER));
    match n.state {
        NodeState::Missing => {
            let fg = line(&p, "Model missing", ty::small(), 14.0, foot.width() - 18.0);
            paint_line(&p, Rect::from_min_size(foot.min + vec2(9.0, 0.0), vec2(foot.width() - 18.0, FOOTER)), fg, 14.0, Align::Min, t.error, 0.0);
        }
        NodeState::Running(v) => {
            let bar = Rect::from_min_size(pos2(foot.left() + 9.0, foot.top() + 6.0), vec2(foot.width() - 18.0, 3.0));
            chips::progress(ui, bar, Progress { value: Some(v), state: chips::State::Running, thin: true, label: n.title });
        }
        _ => {}
    }
    r
}

fn paint_grid(p: &egui::Painter, t: &Tokens, origin: Pos2, view: Rect) {
    // 16 px dots, 2×2 at (7, 7) of each tile, anchored at the content origin (scroll with it).
    let (x0, y0) = (((view.left() - origin.x) / 16.0).floor() as i32, ((view.top() - origin.y) / 16.0).floor() as i32);
    let (x1, y1) = (((view.right() - origin.x) / 16.0).ceil() as i32, ((view.bottom() - origin.y) / 16.0).ceil() as i32);
    let mut shapes = Vec::with_capacity(((x1 - x0) * (y1 - y0)).max(0) as usize);
    for j in y0..y1 { for i in x0..x1 {
        shapes.push(Shape::rect_filled(Rect::from_min_size(origin + vec2(i as f32 * 16.0 + 7.0, j as f32 * 16.0 + 7.0), vec2(2.0, 2.0)), 0, t.graph_grid));
    } }
    p.extend(shapes);
}

fn paint_wire(p: &egui::Painter, t: &Tokens, w: &Wire, nodes: &[NodeSpec], origin: Pos2, z: f32) {
    let pts: Vec<Pos2> = flatten(&route(w, nodes), 4.0).into_iter().map(|q| origin + q.to_vec2() * z).collect();
    let stroke = Stroke::new(1.5, wire_color(t, w.matte));
    if w.optional { p.extend(Shape::dashed_line(&pts, stroke, 4.0, 3.0)) } else { p.add(Shape::line(pts, stroke)); }
}

fn accessible_name(n: &NodeSpec) -> String {
    let mut s = format!("{} node", n.title);
    match n.state {
        NodeState::Running(v) => s += &format!(", running {:.0} %", v.clamp(0.0, 1.0) * 100.0),
        NodeState::Missing => s += ", model missing",
        NodeState::Bypassed => s += ", bypassed",
        _ => {}
    }
    s
}

pub fn graph_canvas(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, nodes: &[NodeSpec], wires: &[Wire], view: &mut GraphView) -> GraphOut {
    graph_canvas_forced(ui, id, nodes, wires, view, Forced::default())
}

/// The canvas fills the rest of `ui`. Ctrl + wheel zooms (0.5..2), the wheel and dragging empty canvas scroll; both
/// bars appear when the content is larger (never clipped silently, §5.14). Arrows move between nodes in layout order,
/// Enter selects (`clicked`), Shift+F10 / right-click opens the node menu (§10.2).
pub fn graph_canvas_forced(ui: &mut Ui, id: impl std::hash::Hash + std::fmt::Debug, nodes: &[NodeSpec], wires: &[Wire], view: &mut GraphView, forced: Forced) -> GraphOut {
    let t = tokens(ui);
    let id = Id::unique(id); // same hash as the old `Id::new`, so ids stay stable and identical
    let (rect, canvas) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
    ui.painter().rect_filled(rect, 0, t.graph_canvas);
    if canvas.hovered() {
        let zd = ui.input(|i| i.zoom_delta());
        if zd != 1.0 { view.zoom = (view.zoom * zd).clamp(0.5, 2.0) }
    }
    view.zoom = if view.zoom.is_finite() { view.zoom.clamp(0.5, 2.0) } else { 1.0 };
    let z = view.zoom;
    let mut out = GraphOut::default();
    let mut order: Vec<usize> = (0..nodes.len()).collect();
    order.sort_by(|&a, &b| (nodes[a].pos.x, nodes[a].pos.y).partial_cmp(&(nodes[b].pos.x, nodes[b].pos.y)).unwrap_or(std::cmp::Ordering::Equal));
    let stop_key = id.with("kb-stop");
    let mut stop: usize = ui.data(|d| d.get_temp(stop_key)).unwrap_or_else(|| nodes.iter().position(|n| n.selected || n.state == NodeState::Selected).unwrap_or(order.first().copied().unwrap_or(0)));
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(egui::Layout::top_down(Align::Min)));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let size = content_extent(nodes) * z;
    let scrolled = scroll::scroll_both(&mut child, id.with("scroll"), |ui| {
        let (content, _) = ui.allocate_exact_size(size, Sense::hover());
        let origin = content.min;
        paint_grid(&ui.painter().with_clip_rect(ui.clip_rect()), t, origin, ui.clip_rect());
        for w in wires { paint_wire(ui.painter(), t, w, nodes, origin, z) }
        for (k, n) in nodes.iter().enumerate() {
            let nid = id.with(("node", n.id));
            let g = node_rect(n);
            let r = Rect::from_min_size(origin + g.min.to_vec2() * z, g.size() * z);
            let sense = if k == stop { Sense::click() } else { Sense::CLICK };
            let mut resp = ui.interact(r, nid, sense);
            let name = accessible_name(n);
            resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, name.as_str()));
            ui.ctx().accesskit_node_builder(nid, |nd| nd.set_selected(n.selected || n.state == NodeState::Selected));
            if resp.has_focus() {
                ui.memory_mut(|m| m.set_focus_lock_filter(nid, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() }));
                let (next, prev) = ui.input_mut(|i| (
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowRight) | i.consume_key(egui::Modifiers::NONE, Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, Key::ArrowLeft) | i.consume_key(egui::Modifiers::NONE, Key::ArrowUp)));
                let pos = order.iter().position(|&o| o == k).unwrap_or(0);
                let target = if next { order.get(pos + 1) } else if prev { pos.checked_sub(1).and_then(|p| order.get(p)) } else { None };
                if let Some(&tk) = target {
                    stop = tk;
                    ui.memory_mut(|m| m.request_focus(id.with(("node", nodes[tk].id))));
                    ui.scroll_to_rect(Rect::from_min_size(origin + node_rect(&nodes[tk]).min.to_vec2() * z, g.size() * z), None);
                }
            }
            if resp.clicked() { out.clicked = Some(n.id); stop = k }
            let hover = forced.hover.map_or(resp.hovered(), |h| h == n.id);
            let focus = forced.focus.map_or(resp.has_focus(), |f| f == n.id);
            paint_node(ui, n, origin, z, hover, focus, forced.reduced_motion);
            if z < 1.0 { resp = tooltip(&resp, n.title, None, ""); }
            let items = [
                MenuItem::new("Open settings"),
                MenuItem::new("Show in viewer"),
                MenuItem::new("Bypass").checked(n.state == NodeState::Bypassed),
            ];
            if let Some(path) = context_menu(&resp, &format!("Node {}", n.title), &items) {
                out.context = Some((n.id, [NodeAction::OpenSettings, NodeAction::ShowInViewer, NodeAction::Bypass][path[0].min(2)]));
            }
        }
    });
    view.offset = scrolled.state.clamped_offset();
    ui.data_mut(|d| d.insert_temp(stop_key, stop));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn n<'a>(id: u32, kind: NodeKind, title: &'a str, x: f32, y: f32, ins: &[&'a str], outs: &[&'a str]) -> NodeSpec<'a> {
        NodeSpec { id, kind, title, icon: Icon::Key, ports_in: ins.iter().map(|l| (*l, false, true)).collect(), ports_out: outs.iter().map(|l| (*l, false, true)).collect(), pos: pos2(x, y), state: NodeState::Idle, selected: false }
    }
    /// The sheet's pipeline: five nodes in one row (+ Roto and SAM in row 2).
    fn pipeline() -> (Vec<NodeSpec<'static>>, Vec<Wire>) {
        let nodes = vec![
            n(1, NodeKind::Source, "Media", 12.0, 12.0, &[], &["image"]),
            n(2, NodeKind::Mask, "Pre-matte (GVM)", 204.0, 12.0, &["image"], &["pre-matte"]),
            n(3, NodeKind::Mask, "Hint", 396.0, 12.0, &["image", "pre-matte", "mask"], &["hint"]),
            n(4, NodeKind::Key, "Key", 588.0, 12.0, &["image", "hint", "mask"], &["result"]),
            n(5, NodeKind::Output, "Deliver", 780.0, 12.0, &["result"], &["FG", "Matte", "Comp", "Processed"]),
            n(6, NodeKind::Mask, "Roto", 12.0, 164.0, &["image"], &["matte"]),
            n(7, NodeKind::Mask, "SAM", 396.0, 164.0, &["image"], &["mask"]),
        ];
        let w = |a: (u32, usize), b: (u32, usize), matte| Wire { from: a, to: b, matte, optional: false };
        let wires = vec![
            w((1, 0), (2, 0), false), w((1, 0), (3, 0), false), w((1, 0), (4, 0), false), w((2, 0), (3, 1), true), w((3, 0), (4, 1), true),
            w((4, 0), (5, 0), false), w((1, 0), (6, 0), false), w((1, 0), (7, 0), false), w((6, 0), (3, 2), true), w((7, 0), (4, 2), true),
        ];
        (nodes, wires)
    }

    #[test]
    fn node_height_follows_port_rule() {
        let (nodes, _) = pipeline();
        let h: Vec<f32> = nodes.iter().map(|n| node_rect(n).height()).collect();
        assert_eq!(h, vec![56.0, 74.0, 110.0, 110.0, 128.0, 74.0, 74.0]); // Media, Pre-matte, Hint, Key, Deliver, Roto, SAM
        assert_eq!(node_height(1, 1), 74.0);
    }

    #[test]
    fn wires_never_pass_behind_nodes() {
        let (nodes, wires) = pipeline();
        for w in &wires {
            let pts = flatten(&route(w, &nodes), 2.0);
            assert!(pts.len() > 2, "wire {:?} has a route", w);
            for p in &pts[1..pts.len() - 1] {
                for nd in &nodes {
                    assert!(!node_rect(nd).shrink(0.5).contains(*p), "wire {:?} passes behind {} at {:?}", w, nd.title, p);
                }
            }
        }
        // Media -> Key image drops into the lane 12 below row 1 (Deliver ends at 140).
        let lane = flatten(&route(&wires[2], &nodes), 2.0);
        assert!(lane.iter().any(|p| (p.y - 152.0).abs() < 0.01));
    }

    #[test]
    fn node_text_hidden_below_100_percent() {
        let (nodes, wires) = pipeline();
        let texts = |zoom: f32| {
            let ctx = egui::Context::default();
            let mut view = GraphView { zoom, offset: Vec2::ZERO };
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1000.0, 300.0))), ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| { graph_canvas(ui, "g", &nodes, &wires, &mut view); });
            });
            out.textures_delta.clear();
            fn count(s: &Shape) -> usize { match s { Shape::Text(_) => 1, Shape::Vec(v) => v.iter().map(count).sum(), _ => 0 } }
            out.shapes.iter().map(|c| count(&c.shape)).sum::<usize>()
        };
        assert!(texts(1.0) > 20, "titles, port labels and footers at 100 %");
        assert_eq!(texts(0.99), 0);
        assert_eq!(texts(0.5), 0);
    }
}
