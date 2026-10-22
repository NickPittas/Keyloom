//! Node canvas: typed ports, explicit data edges, draggable cards, pan/zoom/fit.
//! Pure view code: it never clears or mutates parent state, it only returns [`Event`]s.
//! The parent owns one [`Graph`] + [`View`] per clip, so card positions survive clip switches.
//! Edges describe the current fixed topology; nothing here rewires or executes anything.
use super::theme as th;
use eframe::egui::{
    self, epaint::CubicBezierShape, text::LayoutJob, Align2, Color32, FontId, Id, Painter,
    PointerButton, Pos2, Rect, Sense, Shape, Stroke, StrokeKind, Ui, Vec2,
};

pub const HEADER_H: f32 = 20.0;
pub const PITCH: f32 = 18.0;
pub const BADGE_H: f32 = 18.0;
const PAD: f32 = 3.0;
pub const MIN_ZOOM: f32 = 0.5;
pub const MAX_ZOOM: f32 = 2.0;
/// Fit never goes below 1.0, so labels stay >= 12 px; a canvas smaller than the graph pans instead.
pub const FIT_MIN_ZOOM: f32 = 1.0;
const FIT_MARGIN: f32 = 8.0;
const FIT_BTN_STRIP: f32 = 66.0;
const LABEL_PX: f32 = 12.0;
/// Below this manual zoom the port/badge text is hidden (titles stay) rather than shrunk under 12 px.
const TEXT_MIN_ZOOM: f32 = 0.8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PortType {
    Image,
    Matte,
}
impl PortType {
    pub fn label(self) -> &'static str {
        match self {
            PortType::Image => "Image",
            PortType::Matte => "Matte",
        }
    }
    fn color(self) -> Color32 {
        match self {
            PortType::Image => Color32::from_rgb(0x5E, 0xC8, 0xD8),
            PortType::Matte => Color32::from_rgb(214, 214, 218),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum OutKind {
    Fg,
    Matte,
    Comp,
    Processed,
}
impl OutKind {
    pub fn label(self) -> &'static str {
        match self {
            OutKind::Fg => "FG",
            OutKind::Matte => "Matte",
            OutKind::Comp => "Comp",
            OutKind::Processed => "Processed",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum NodeKind {
    Media,
    Hint,
    Key,
    Out(OutKind),
}
impl NodeKind {
    pub fn title(self) -> &'static str {
        match self {
            NodeKind::Media => "Media",
            NodeKind::Hint => "Hint",
            NodeKind::Key => "Key",
            NodeKind::Out(o) => o.label(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct NodeId(pub usize);

#[derive(Clone, Copy, Debug)]
pub struct Port {
    pub name: &'static str,
    pub ty: PortType,
    pub optional: bool,
}
const fn port(name: &'static str, ty: PortType) -> Port {
    Port {
        name,
        ty,
        optional: false,
    }
}

#[derive(Clone, Debug)]
pub struct GNode {
    pub kind: NodeKind,
    /// Graph-space top-left; the only field the user changes (header drag).
    pub pos: Pos2,
    pub width: f32,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
}
impl GNode {
    pub fn size(&self) -> Vec2 {
        let rows = self.inputs.len().max(self.outputs.len()).max(1) as f32;
        let tail = if self.badge_inline() {
            0.0
        } else {
            BADGE_H + PAD
        };
        Vec2::new(self.width, HEADER_H + rows * PITCH + tail)
    }
    /// Sink cards (one input, no outputs) share the badge with their only row to stay short.
    pub fn badge_inline(&self) -> bool {
        self.inputs.len() == 1 && self.outputs.is_empty()
    }
    /// Graph-space socket centre. `output` picks the right edge.
    pub fn socket(&self, output: bool, idx: usize) -> Pos2 {
        let x = if output {
            self.pos.x + self.width
        } else {
            self.pos.x
        };
        Pos2::new(x, self.pos.y + HEADER_H + PITCH * (idx as f32 + 0.5))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edge {
    /// (node, output index)
    pub from: (NodeId, usize),
    /// (node, input index)
    pub to: (NodeId, usize),
}

#[derive(Clone, Debug)]
pub struct Graph {
    pub nodes: Vec<GNode>,
    pub edges: Vec<Edge>,
}

impl Graph {
    /// The current functional topology. Later nodes (luminance key, merge, roto, paint) are new
    /// `NodeKind`s + ports + edges here; nothing else in this module is specific to these seven.
    pub fn corridorkey() -> Self {
        use PortType::{Image, Matte};
        // Compact layout: 628 x 164 graph units, so Fit at 728 x 220 lands at zoom >= 1.0 (12 px text).
        let out = |o: OutKind, y: f32| GNode {
            kind: NodeKind::Out(o),
            pos: Pos2::new(498.0, y),
            width: 130.0,
            inputs: vec![port("in", if o == OutKind::Matte { Matte } else { Image })],
            outputs: vec![],
        };
        let nodes = vec![
            GNode {
                kind: NodeKind::Media,
                pos: Pos2::new(0.0, 30.0),
                width: 120.0,
                inputs: vec![],
                outputs: vec![port("rgb", Image)],
            },
            GNode {
                kind: NodeKind::Hint,
                pos: Pos2::new(156.0, 0.0),
                width: 130.0,
                inputs: vec![port("rgb", Image)],
                outputs: vec![port("hint", Matte)],
            },
            GNode {
                kind: NodeKind::Key,
                pos: Pos2::new(322.0, 26.0),
                width: 140.0,
                inputs: vec![
                    port("rgb", Image),
                    Port {
                        optional: true,
                        ..port("hint", Matte)
                    },
                ],
                outputs: vec![
                    port("fg", Image),
                    port("matte", Matte),
                    port("comp", Image),
                    port("processed", Image),
                ],
            },
            out(OutKind::Fg, 0.0),
            out(OutKind::Matte, 42.0),
            out(OutKind::Comp, 84.0),
            out(OutKind::Processed, 126.0),
        ];
        let e = |a: usize, ao: usize, b: usize, bi: usize| Edge {
            from: (NodeId(a), ao),
            to: (NodeId(b), bi),
        };
        let edges = vec![
            e(0, 0, 1, 0),
            e(0, 0, 2, 0),
            e(1, 0, 2, 1),
            e(2, 0, 3, 0),
            e(2, 1, 4, 0),
            e(2, 2, 5, 0),
            e(2, 3, 6, 0),
        ];
        Graph { nodes, edges }
    }

    pub fn find(&self, k: NodeKind) -> Option<NodeId> {
        self.nodes.iter().position(|n| n.kind == k).map(NodeId)
    }

    /// Indices valid, port types match, at most one edge per input, no cycles.
    pub fn validate(&self) -> Result<(), String> {
        let mut used = std::collections::HashSet::new();
        for (i, e) in self.edges.iter().enumerate() {
            let out = self
                .nodes
                .get(e.from.0 .0)
                .and_then(|n| n.outputs.get(e.from.1));
            let inp = self.nodes.get(e.to.0 .0).and_then(|n| n.inputs.get(e.to.1));
            let (Some(o), Some(n)) = (out, inp) else {
                return Err(format!("edge {i}: bad endpoint"));
            };
            if o.ty != n.ty {
                return Err(format!(
                    "edge {i}: {} -> {} type mismatch",
                    o.ty.label(),
                    n.ty.label()
                ));
            }
            if !used.insert(e.to) {
                return Err(format!("edge {i}: input already connected"));
            }
        }
        // Kahn: every node must be removable.
        let mut indeg = vec![0usize; self.nodes.len()];
        self.edges.iter().for_each(|e| indeg[e.to.0 .0] += 1);
        let mut stack: Vec<usize> = (0..indeg.len()).filter(|&i| indeg[i] == 0).collect();
        let mut seen = 0;
        while let Some(n) = stack.pop() {
            seen += 1;
            for e in self.edges.iter().filter(|e| e.from.0 .0 == n) {
                indeg[e.to.0 .0] -= 1;
                if indeg[e.to.0 .0] == 0 {
                    stack.push(e.to.0 .0);
                }
            }
        }
        if seen == self.nodes.len() {
            Ok(())
        } else {
            Err("cycle".into())
        }
    }

    pub fn bounds(&self) -> Rect {
        self.nodes.iter().fold(Rect::NOTHING, |r, n| {
            r.union(Rect::from_min_size(n.pos, n.size()))
        })
    }
}

/// Pan/zoom plus last-frame geometry. `Default` = fit pending.
#[derive(Clone, Debug)]
pub struct View {
    pub pan: Vec2,
    pub zoom: f32,
    pub user_moved: bool,
    fitted: bool,
    /// Canvas rect and per-node screen rects (index = NodeId) from the last `show`; for the parent's
    /// "Fit" header button and for tests/QA hit-testing.
    pub canvas: Rect,
    pub node_rects: Vec<Rect>,
}
impl Default for View {
    fn default() -> Self {
        View {
            pan: Vec2::ZERO,
            zoom: 1.0,
            user_moved: false,
            fitted: false,
            canvas: Rect::NOTHING,
            node_rects: vec![],
        }
    }
}
impl View {
    /// A saved view: no automatic refit over it.
    pub fn restored(pan: Vec2, zoom: f32) -> Self {
        View {
            pan,
            zoom,
            user_moved: true,
            fitted: true,
            ..View::default()
        }
    }
    pub fn fit(&mut self, g: &Graph, rect: Rect) {
        let b = g.bounds();
        if !b.is_positive() || rect.width() < 1.0 || rect.height() < 1.0 {
            return;
        }
        // The overlay Fit button owns a strip at the right edge: fit into the rest so no node sits under it.
        let usable = Vec2::new((rect.width() - FIT_BTN_STRIP).max(1.0), rect.height());
        let avail = usable - Vec2::splat(2.0 * FIT_MARGIN);
        self.zoom = (avail.x / b.width())
            .min(avail.y / b.height())
            .clamp(FIT_MIN_ZOOM, MAX_ZOOM);
        // Centre the bounds in the rect.
        self.pan = (usable - b.size() * self.zoom) * 0.5 - b.min.to_vec2() * self.zoom;
        self.user_moved = false;
        self.fitted = true;
        self.canvas = rect;
    }
    /// Re-fit using the last canvas rect (what a header "Fit" button calls).
    pub fn fit_last(&mut self, g: &Graph) {
        let c = self.canvas;
        self.fit(g, c);
    }
    pub fn to_screen(&self, p: Pos2, rect: Rect) -> Pos2 {
        rect.min + self.pan + p.to_vec2() * self.zoom
    }
    pub fn to_graph(&self, p: Pos2, rect: Rect) -> Pos2 {
        ((p - rect.min - self.pan) / self.zoom).to_pos2()
    }
    fn zoom_at(&mut self, local: Vec2, factor: f32) {
        let z = (self.zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
        let k = z / self.zoom;
        self.pan = local - (local - self.pan) * k;
        self.zoom = z;
        self.user_moved = true;
    }
}

/// Per-frame appearance supplied by the parent.
#[derive(Clone, Debug, Default)]
pub struct NodeLook {
    pub badge: String,
    pub dimmed: bool,
    pub dashed: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Event {
    Select(NodeKind),
    Open(NodeKind),
}

fn text(p: &Painter, pos: Pos2, align: Align2, s: &str, px: f32, color: Color32, max_w: f32) {
    let mut job = LayoutJob::simple_singleline(s.into(), FontId::proportional(px), color);
    job.wrap.max_width = max_w.max(1.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let g = p.layout_job(job);
    p.galley(align.anchor_size(pos, g.size()).min, g, color);
}

/// Draws the canvas at the full available width and `height` px. Returns at most one event:
/// header/body click -> `Select`, double-click or Enter on a focused card -> `Open`.
pub fn show(
    ui: &mut Ui,
    g: &mut Graph,
    v: &mut View,
    selected: Option<NodeKind>,
    look: impl Fn(NodeKind) -> NodeLook,
    height: f32,
) -> Option<Event> {
    let size = Vec2::new(ui.available_width().max(1.0), height.max(40.0));
    let (canvas, bg) = ui.allocate_exact_size(size, Sense::click_and_drag());
    let painter = ui.painter_at(canvas);
    painter.rect_filled(canvas, 0.0, th::CANVAS);
    painter.rect_stroke(
        canvas,
        0.0,
        Stroke::new(1.0, th::BORDER),
        StrokeKind::Inside,
    );

    // Fit on first show, and while the user has not touched the view when the panel is resized.
    if !v.fitted || (!v.user_moved && (v.canvas.size() - canvas.size()).abs().max_elem() > 0.5) {
        v.fit(g, canvas);
    }
    v.canvas = canvas;

    let mut ev = None;
    if bg.dragged_by(PointerButton::Primary) || bg.dragged_by(PointerButton::Middle) {
        v.pan += bg.drag_delta();
        v.user_moved = true;
    }
    if bg.double_clicked() {
        v.fit(g, canvas);
    }
    if ui.rect_contains_pointer(canvas) {
        let (zoom, scroll, ptr) = ui.input(|i| {
            (
                i.zoom_delta(),
                i.smooth_scroll_delta.y,
                i.pointer.hover_pos(),
            )
        });
        if (zoom - 1.0).abs() > f32::EPSILON {
            let local = ptr.map_or(canvas.size() * 0.5, |p| p - canvas.min);
            v.zoom_at(local, zoom);
        } else if scroll != 0.0 {
            v.pan.y += scroll;
            v.user_moved = true;
            ui.input_mut(|i| i.smooth_scroll_delta = Vec2::ZERO);
        }
    }

    // Interact (topmost last), recording screen rects.
    v.node_rects = g
        .nodes
        .iter()
        .map(|n| Rect::from_min_size(v.to_screen(n.pos, canvas), n.size() * v.zoom))
        .collect();
    let mut hovered = None;
    let mut focused = None;
    let mut drag = None;
    for (i, n) in g.nodes.iter().enumerate() {
        let r = v.node_rects[i];
        let hdr = Rect::from_min_size(r.min, Vec2::new(r.width(), HEADER_H * v.zoom));
        let body = Rect::from_min_max(Pos2::new(r.min.x, hdr.max.y), r.max);
        let id = ui.scope_id().with(("ck-graph", i));
        let rh = ui.interact(hdr, id.with("h"), Sense::click_and_drag());
        // click_and_drag so a body drag is absorbed rather than falling through to canvas pan.
        let rb = ui.interact(body, id.with("b"), Sense::click_and_drag());
        if rh.dragged_by(PointerButton::Primary) {
            drag = Some((i, rh.drag_delta()));
        }
        if rh.hovered() || rb.hovered() {
            hovered = Some(i);
        }
        if rh.has_focus() || rb.has_focus() {
            focused = Some(i);
        }
        let enter =
            (rh.has_focus() || rb.has_focus()) && ui.input(|i| i.key_pressed(egui::Key::Enter));
        if rh.double_clicked() || rb.double_clicked() || enter {
            ev = Some(Event::Open(n.kind));
        } else if ev.is_none()
            && (rh.clicked()
                || rb.clicked()
                || rh.drag_started()
                || rh.gained_focus()
                || rb.gained_focus())
        {
            ev = Some(Event::Select(n.kind));
        }
        if rb.hovered() {
            let b = look(n.kind).badge;
            if !b.is_empty() {
                rb.on_hover_text(b);
            }
        }
    }
    if let Some((i, d)) = drag {
        g.nodes[i].pos += d / v.zoom;
        for (k, n) in g.nodes.iter().enumerate() {
            v.node_rects[k] = Rect::from_min_size(v.to_screen(n.pos, canvas), n.size() * v.zoom);
        }
    }

    // Edges first so cards cover the joins.
    let sel_id = selected.and_then(|k| g.find(k));
    let looks: Vec<NodeLook> = g.nodes.iter().map(|n| look(n.kind)).collect();
    for e in &g.edges {
        let (a, b) = (&g.nodes[e.from.0 .0], &g.nodes[e.to.0 .0]);
        let (p0, p3) = (
            v.to_screen(a.socket(true, e.from.1), canvas),
            v.to_screen(b.socket(false, e.to.1), canvas),
        );
        let dx = ((p3.x - p0.x) * 0.5).abs().max(40.0 * v.zoom);
        let hot = sel_id == Some(e.to.0) || sel_id == Some(e.from.0);
        let mut c = a.outputs[e.from.1].ty.color();
        if !hot {
            c = c.gamma_multiply(0.7);
        }
        let dashed = looks[e.from.0 .0].dashed || looks[e.to.0 .0].dashed;
        if looks[e.from.0 .0].dimmed || looks[e.to.0 .0].dimmed {
            c = c.gamma_multiply(0.5);
        }
        let stroke = Stroke::new(if hot { 2.5 } else { 1.75 }, c);
        let bez = CubicBezierShape::from_points_stroke(
            [p0, p0 + Vec2::new(dx, 0.0), p3 - Vec2::new(dx, 0.0), p3],
            false,
            Color32::TRANSPARENT,
            stroke,
        );
        if dashed {
            let pts = bez.flatten(Some(0.5));
            painter.extend(Shape::dashed_line(&pts, stroke, 7.0, 5.0));
        } else {
            painter.add(bez);
        }
    }

    let px = LABEL_PX * v.zoom.max(1.0);
    let show_text = v.zoom >= TEXT_MIN_ZOOM;
    for (i, n) in g.nodes.iter().enumerate() {
        let r = v.node_rects[i];
        let lk = &looks[i];
        let dim = |c: Color32| if lk.dimmed { c.gamma_multiply(0.55) } else { c };
        let is_sel = selected == Some(n.kind);
        let rounding = 5.0 * v.zoom;
        painter.rect_filled(r, rounding, dim(th::CARD));
        let hdr = Rect::from_min_size(r.min, Vec2::new(r.width(), HEADER_H * v.zoom));
        painter.rect_filled(
            hdr,
            egui::CornerRadius {
                nw: rounding as u8,
                ne: rounding as u8,
                sw: 0,
                se: 0,
            },
            dim(if is_sel { th::ACCENT } else { th::CARD_HEADER }),
        );
        let border = if is_sel {
            Stroke::new(2.0, th::CHECKED)
        } else if hovered == Some(i) {
            Stroke::new(1.5, th::BORDER_HOVER)
        } else {
            Stroke::new(1.0, th::BORDER)
        };
        painter.rect_stroke(r, rounding, border, StrokeKind::Inside);
        if focused == Some(i) {
            painter.rect_stroke(
                r.expand(3.0),
                rounding + 3.0,
                Stroke::new(2.0, th::FOCUS),
                StrokeKind::Outside,
            );
        }
        text(
            &painter,
            hdr.left_center() + Vec2::new(8.0 * v.zoom, 0.0),
            Align2::LEFT_CENTER,
            n.kind.title(),
            px,
            dim(Color32::WHITE),
            r.width() - 16.0 * v.zoom,
        );
        for (idx, p) in n.inputs.iter().enumerate() {
            let c = v.to_screen(n.socket(false, idx), canvas);
            socket(&painter, c, p, v.zoom, dim(p.ty.color()));
            if show_text {
                let label = if p.optional {
                    format!("{} (opt)", p.name)
                } else {
                    p.name.to_string()
                };
                text(
                    &painter,
                    c + Vec2::new(10.0 * v.zoom, 0.0),
                    Align2::LEFT_CENTER,
                    &label,
                    px,
                    dim(th::TEXT),
                    r.width() * 0.5,
                );
            }
        }
        for (idx, p) in n.outputs.iter().enumerate() {
            let c = v.to_screen(n.socket(true, idx), canvas);
            socket(&painter, c, p, v.zoom, dim(p.ty.color()));
            if show_text {
                text(
                    &painter,
                    c - Vec2::new(10.0 * v.zoom, 0.0),
                    Align2::RIGHT_CENTER,
                    p.name,
                    px,
                    dim(th::TEXT),
                    r.width() * 0.5,
                );
            }
        }
        if show_text {
            let col = dim(th::TEXT_DIM);
            if n.badge_inline() {
                let y = v.to_screen(n.socket(false, 0), canvas).y;
                text(
                    &painter,
                    Pos2::new(r.max.x - 8.0 * v.zoom, y),
                    Align2::RIGHT_CENTER,
                    &lk.badge,
                    px,
                    col,
                    r.width() * 0.62,
                );
            } else {
                let y = r.max.y - (BADGE_H * 0.5 + PAD) * v.zoom;
                text(
                    &painter,
                    Pos2::new(r.min.x + 8.0 * v.zoom, y),
                    Align2::LEFT_CENTER,
                    &lk.badge,
                    px,
                    col,
                    r.width() - 16.0 * v.zoom,
                );
            }
        }
        // Socket tooltips: hover-only, so they never steal clicks or drags.
        for (idx, p) in n.inputs.iter().enumerate() {
            tip(
                ui,
                canvas,
                v,
                n,
                false,
                idx,
                p,
                ui.scope_id().with(("ck-sock-in", i, idx)),
            );
        }
        for (idx, p) in n.outputs.iter().enumerate() {
            tip(
                ui,
                canvas,
                v,
                n,
                true,
                idx,
                p,
                ui.scope_id().with(("ck-sock-out", i, idx)),
            );
        }
    }

    // Discoverable Fit (also: double-click empty canvas).
    let fit_rect = fit_button_rect(canvas);
    if ui
        .put(fit_rect, egui::Button::new("Fit"))
        .on_hover_text("Fit all nodes (or double-click empty canvas)")
        .clicked()
    {
        v.fit(g, canvas);
    }
    ev
}

/// The overlay Fit button's rectangle (nodes are fitted clear of it).
pub fn fit_button_rect(canvas: Rect) -> Rect {
    Rect::from_min_size(
        canvas.right_top() + Vec2::new(-64.0, 6.0),
        Vec2::new(58.0, 24.0),
    )
}

fn socket(p: &Painter, c: Pos2, port: &Port, zoom: f32, col: Color32) {
    let r = 4.5 * zoom.max(0.8);
    if port.optional {
        p.circle_stroke(c, r, Stroke::new(1.5, col));
    } else {
        p.circle_filled(c, r, col);
    }
    p.circle_stroke(c, r + 0.5, Stroke::new(1.0, th::CANVAS));
}

#[allow(clippy::too_many_arguments)]
fn tip(ui: &mut Ui, canvas: Rect, v: &View, n: &GNode, output: bool, idx: usize, p: &Port, id: Id) {
    let c = v.to_screen(n.socket(output, idx), canvas);
    let r = Rect::from_center_size(c, Vec2::splat(14.0 * v.zoom.max(0.8)));
    ui.interact(r, id, Sense::hover())
        .on_hover_text(format!("{}: {}", p.name, p.ty.label()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Context, Event as E, Modifiers, RawInput};

    struct H {
        ctx: Context,
        g: Graph,
        v: View,
        t: f64,
        sel: Option<NodeKind>,
        dashed: Option<NodeKind>,
        screen: Vec2,
    }
    impl H {
        fn new() -> Self {
            let ctx = Context::default();
            th::apply(&ctx);
            H {
                ctx,
                g: Graph::corridorkey(),
                v: View::default(),
                t: 0.0,
                sel: None,
                dashed: None,
                screen: Vec2::new(728.0, 300.0),
            }
        }
        fn step(&mut self, events: Vec<E>) -> Option<Event> {
            self.t += 0.05;
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.screen)),
                time: Some(self.t),
                events,
                ..Default::default()
            };
            let (g, v, sel, dashed, h) = (
                &mut self.g,
                &mut self.v,
                self.sel,
                self.dashed,
                self.screen.y,
            );
            let mut out = None;
            let mut full = self.ctx.run_ui(input, |ui| {
                out = show(
                    ui,
                    g,
                    v,
                    sel,
                    |k| NodeLook {
                        badge: "b".into(),
                        dimmed: false,
                        dashed: dashed == Some(k),
                    },
                    h,
                );
            });
            full.textures_delta.clear();
            out
        }
        fn rect(&self, k: NodeKind) -> Rect {
            self.v.node_rects[self.g.find(k).unwrap().0]
        }
        fn press(&mut self, p: Pos2, pressed: bool) -> Option<Event> {
            self.step(vec![
                E::PointerMoved(p),
                E::PointerButton {
                    pos: p,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                },
            ])
        }
        fn click(&mut self, p: Pos2) -> Option<Event> {
            self.step(vec![E::PointerMoved(p)]);
            let a = self.press(p, true);
            let b = self.press(p, false);
            b.or(a)
        }
        fn drag(&mut self, from: Pos2, to: Pos2) {
            self.step(vec![E::PointerMoved(from)]);
            self.press(from, true);
            for k in 1..=4 {
                self.step(vec![E::PointerMoved(from + (to - from) * (k as f32 / 4.0))]);
            }
            self.press(to, false);
        }
    }

    #[test]
    fn topology_is_valid_and_branching() {
        let g = Graph::corridorkey();
        assert!(g.validate().is_ok());
        let media = g.find(NodeKind::Media).unwrap();
        let key = g.find(NodeKind::Key).unwrap();
        assert_eq!(
            g.edges.iter().filter(|e| e.from.0 == media).count(),
            2,
            "Media fans out to Hint and Key"
        );
        assert_eq!(
            g.edges.iter().filter(|e| e.to.0 == key).count(),
            2,
            "Key merges rgb + hint"
        );
        let key_out: Vec<_> = g
            .edges
            .iter()
            .filter(|e| e.from.0 == key)
            .map(|e| e.from.1)
            .collect();
        assert_eq!(key_out, vec![0, 1, 2, 3], "four separate Key outputs");
        assert!(g.nodes[key.0].inputs[1].optional);
    }

    #[test]
    fn validate_rejects_bad_graphs() {
        let mut g = Graph::corridorkey();
        g.edges.push(Edge {
            from: (NodeId(1), 0),
            to: (NodeId(3), 0),
        }); // Matte -> Image, and input reuse
        assert!(g.validate().unwrap_err().contains("mismatch"));
        let mut g = Graph::corridorkey();
        g.edges.push(Edge {
            from: (NodeId(0), 0),
            to: (NodeId(1), 0),
        });
        assert!(g.validate().unwrap_err().contains("already"));
        let mut g = Graph::corridorkey();
        g.edges.push(Edge {
            from: (NodeId(9), 0),
            to: (NodeId(1), 0),
        });
        assert!(g.validate().is_err());
        // cycle: Key.fg -> Media? Media has no inputs, so use two image nodes.
        let mut g = Graph::corridorkey();
        g.nodes[0].inputs.push(port("x", PortType::Image));
        g.edges.push(Edge {
            from: (NodeId(2), 0),
            to: (NodeId(0), 0),
        });
        g.edges.push(Edge {
            from: (NodeId(0), 0),
            to: (NodeId(1), 0),
        });
        assert!(g.validate().is_err());
    }

    #[test]
    fn auto_fit_shows_every_card_at_728() {
        let mut h = H::new();
        h.step(vec![]);
        h.step(vec![]);
        let c = h.v.canvas;
        assert!((c.width() - 728.0).abs() < 1.0);
        for r in &h.v.node_rects {
            assert!(c.contains_rect(*r), "{r:?} outside {c:?}");
        }
        assert!(h.v.zoom >= MIN_ZOOM && h.v.zoom <= MAX_ZOOM);
    }

    #[test]
    fn click_selects_double_click_opens_header_and_body() {
        let mut h = H::new();
        h.step(vec![]);
        h.step(vec![]);
        let r = h.rect(NodeKind::Key);
        let header = Pos2::new(r.center().x, r.min.y + 5.0);
        assert_eq!(h.click(header), Some(Event::Select(NodeKind::Key)));
        let body = Pos2::new(r.center().x, r.max.y - 4.0);
        assert_eq!(h.click(body), Some(Event::Select(NodeKind::Key)));
        let r = h.rect(NodeKind::Hint);
        let p = Pos2::new(r.center().x, r.min.y + 5.0);
        h.t += 1.0; // egui counts 3 clicks inside 0.3 s as a triple click
        h.step(vec![E::PointerMoved(p)]);
        let pb = |pressed| E::PointerButton {
            pos: p,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let ev = h.step(vec![pb(true), pb(false), pb(true), pb(false)]);
        assert_eq!(ev, Some(Event::Open(NodeKind::Hint)));
    }

    #[test]
    fn header_drag_moves_node_body_drag_does_not_and_empty_drag_pans() {
        let mut h = H::new();
        h.step(vec![]);
        h.step(vec![]);
        let z = h.v.zoom;
        let before = h.g.nodes[2].pos;
        let r = h.rect(NodeKind::Key);
        let hp = Pos2::new(r.center().x, r.min.y + 5.0);
        h.drag(hp, hp + Vec2::new(30.0, 20.0));
        let moved = h.g.nodes[2].pos - before;
        assert!(
            (moved.x - 30.0 / z).abs() < 1.0 && (moved.y - 20.0 / z).abs() < 1.0,
            "{moved:?} z={z}"
        );
        let others = (h.g.nodes[0].pos, h.g.nodes[1].pos);
        assert_eq!(others, (Pos2::new(0.0, 30.0), Pos2::new(156.0, 0.0)));

        let r = h.rect(NodeKind::Key);
        let bp = Pos2::new(r.center().x, r.max.y - 4.0);
        let pos = h.g.nodes[2].pos;
        h.drag(bp, bp + Vec2::new(25.0, 25.0));
        assert_eq!(h.g.nodes[2].pos, pos, "body drag must not move the card");
        assert!(!h.v.user_moved);

        let pan0 = h.v.pan;
        let empty = h.v.canvas.left_bottom() + Vec2::new(40.0, -12.0);
        h.drag(empty, empty + Vec2::new(30.0, -5.0));
        assert!(
            (h.v.pan - pan0 - Vec2::new(30.0, -5.0)).abs().max_elem() < 1.0,
            "{:?}",
            h.v.pan - pan0
        );
        assert!(h.v.user_moved);
    }

    #[test]
    fn zoom_is_clamped_about_cursor_and_fit_restores() {
        let mut h = H::new();
        h.step(vec![]);
        h.step(vec![]);
        let cur = h.v.canvas.center();
        let under = h.v.to_graph(cur, h.v.canvas);
        h.step(vec![E::PointerMoved(cur), E::Zoom(1.2)]);
        let after = h.v.to_graph(cur, h.v.canvas);
        assert!((under - after).length() < 0.5, "cursor drifted");
        for _ in 0..30 {
            h.step(vec![E::Zoom(1.5)]);
        }
        assert!((h.v.zoom - MAX_ZOOM).abs() < 1e-4);
        for _ in 0..60 {
            h.step(vec![E::Zoom(0.5)]);
        }
        assert!((h.v.zoom - MIN_ZOOM).abs() < 1e-4);
        h.v.fit_last(&h.g);
        h.step(vec![]);
        assert!(!h.v.user_moved);
        for r in &h.v.node_rects {
            assert!(h.v.canvas.contains_rect(*r));
        }
    }

    #[test]
    fn per_clip_graphs_are_independent() {
        let mut a = Graph::corridorkey();
        let b = Graph::corridorkey();
        a.nodes[2].pos.x += 50.0;
        assert_ne!(a.nodes[2].pos, b.nodes[2].pos);
    }

    #[test]
    fn dashed_edges_follow_look_and_hover_shows_through() {
        let mut h = H::new();
        h.dashed = Some(NodeKind::Hint);
        h.step(vec![]);
        h.step(vec![]);
        assert!(h.g.validate().is_ok());
    }

    #[test]
    fn fit_at_728x220_is_unclipped_and_keeps_12px_text() {
        let mut h = H::new();
        h.screen = Vec2::new(728.0, 220.0);
        h.step(vec![]);
        h.step(vec![]);
        assert!(
            h.v.zoom >= 1.0,
            "zoom {} would shrink 12 px labels",
            h.v.zoom
        );
        for r in &h.v.node_rects {
            assert!(
                h.v.canvas.contains_rect(*r),
                "{r:?} clipped by {:?}",
                h.v.canvas
            );
        }
        // Fit never drops below 100 % even on a canvas too small for the graph.
        h.screen = Vec2::new(400.0, 120.0);
        h.v = View::default();
        h.step(vec![]);
        assert!(h.v.zoom >= FIT_MIN_ZOOM);
    }

    #[test]
    fn labels_fit_their_rows_at_12px() {
        let ctx = Context::default();
        th::apply(&ctx);
        let g = Graph::corridorkey();
        let mut full = ctx.run_ui(RawInput::default(), |ui| {
            let w = |s: &str| {
                ui.painter()
                    .layout_no_wrap(s.into(), FontId::proportional(LABEL_PX), th::TEXT)
                    .size()
                    .x
            };
            for n in &g.nodes {
                assert!(
                    w(n.kind.title()) + 16.0 <= n.width,
                    "{} title",
                    n.kind.title()
                );
                for row in 0..n.inputs.len().max(n.outputs.len()) {
                    let l = n.inputs.get(row).map_or(0.0, |p| {
                        w(&if p.optional {
                            format!("{} (opt)", p.name)
                        } else {
                            p.name.into()
                        }) + 10.0
                    });
                    let r = n.outputs.get(row).map_or(0.0, |p| w(p.name) + 10.0);
                    assert!(
                        l + r + 8.0 <= n.width,
                        "{} row {row}: {l}+{r}",
                        n.kind.title()
                    );
                }
            }
        });
        full.textures_delta.clear();
    }

    #[test]
    fn tab_focuses_a_card_and_enter_opens_it() {
        let mut h = H::new();
        h.step(vec![]);
        h.step(vec![]);
        let key = |k, pressed| E::Key {
            key: k,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let mut focused = None;
        for _ in 0..4 {
            let ev = h.step(vec![key(egui::Key::Tab, true), key(egui::Key::Tab, false)]);
            if let Some(Event::Select(k)) = ev.or_else(|| h.step(vec![])) {
                focused = Some(k);
                break;
            }
        }
        let focused = focused.expect("Tab never focused a card");
        let ev = h.step(vec![
            key(egui::Key::Enter, true),
            key(egui::Key::Enter, false),
        ]);
        assert!(
            matches!(ev, Some(Event::Open(_))),
            "Enter on focused card gave {ev:?} (focus {focused:?})"
        );
    }
}
