//! Line icons drawn with the egui `Painter` (bible §9). The drawings are generated into `icons_gen.rs` by
//! `tools/gen_icons.py` from the design-bible sources: one point list per size (12, 14, 16; 20 and 32 scale the 16
//! drawing). Strokes are 1 px at UI scale < 1.5 and 1.5 pt above; axis-aligned segments sit on pixel centres with
//! square ends and mitred corners (drawn as pixel-snapped rects); diagonals and arcs are anti-aliased paths.
//!
//! Size availability (bible §9: "asking for a 12/14 entry that is not listed is an error"): `drawing()` is a const
//! table that returns `None` for a size an icon does not have; `paint` has a `debug_assert!` on it and draws nothing
//! in release; the unit tests pin the 12 / 14 / 16 sets to the bible lists.
use eframe::egui::{self, epaint::{Vertex, WHITE_UV}, Color32, Pos2, Rect, Response, Sense, Shape as EShape, Stroke, Ui};

pub use super::icons_gen::{drawing, Icon};

/// A point in the drawing's own unit grid.
type Pt = (f32, f32);
/// A polyline vertex: x, y and 1 if the segment reaching it is a flattened arc (drawn "thin", see `raster_line`).
type LPt = (f32, f32, u8);

/// One drawing primitive. Stroked shapes use the icon colour; filled shapes use [`Paint`].
#[derive(Clone, Copy, Debug)]
pub enum Shape {
    /// Open polyline, square caps.
    Line(&'static [LPt]),
    /// Closed polygon, mitred corners.
    Poly(&'static [LPt]),
    /// Circle outline (cx, cy, r).
    Circle(f32, f32, f32),
    /// Filled triangles (3 points each).
    Tris(&'static [Pt]),
    /// Filled circle (cx, cy, r).
    Disc(f32, f32, f32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paint {
    /// The icon colour.
    Ink,
    /// The icon colour at 35 % (half-tone area).
    Tone,
    /// The background the icon sits on (slider knobs).
    Bg,
}

#[derive(Clone, Copy, Debug)]
pub struct Prim {
    pub s: Shape,
    pub p: Paint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IconSize {
    S12,
    S14,
    S16,
    S20,
    S32,
}

impl IconSize {
    pub const ALL: [IconSize; 5] = [IconSize::S12, IconSize::S14, IconSize::S16, IconSize::S20, IconSize::S32];
    pub fn px(self) -> f32 {
        match self {
            IconSize::S12 => 12.0,
            IconSize::S14 => 14.0,
            IconSize::S16 => 16.0,
            IconSize::S20 => 20.0,
            IconSize::S32 => 32.0,
        }
    }
    /// The drawing grid this size uses: 12 and 14 have their own, 20 and 32 scale the 16 one.
    pub fn grid(self) -> u8 {
        match self {
            IconSize::S12 => 12,
            IconSize::S14 => 14,
            _ => 16,
        }
    }
}

/// Share of the thin band kept for curves that are also drawn as a hairline (12 px curves are the hairline alone).
const BAND_WITH_HAIRLINE: f32 = 0.5;
/// Alpha of the corner pixels past the ends of diagonals in closed 12 px shapes.
const SOFT_CAP: f32 = 0.4;
/// Alpha of the mitre-tip pixels of 12 px strokes.
const SOFT_TIP: f32 = 0.5;
/// Alpha of the diagonal pixels of 12 px rings (r 4).
const SOFT_DIAG: f32 = 0.7;
/// Alpha of the faint outer edge of the 14 / 20 px rings.
const SOFT_RING: f32 = 0.2;
/// A pixel is inked when its centre lies inside the stroke (what the sheet's `crispEdges` renderer does); `true`
/// rounds an exact .5 edge up (so an integer coordinate inks the pixel to its right/below), `false` down.
const TIE_UP: bool = true;
/// Anti-aliased diagonals and arcs are drawn thinner by the share of a pixel row/column a crisp line covers
/// (`max(|cos|, |sin|)` of the segment angle: 0.71 at 45 deg), so the ink matches the sheet's aliased strokes.
const DIAGONAL_FACTOR: bool = true;


struct Tf {
    /// Physical-pixel origin of the icon box.
    o: Pos2,
    /// Physical pixels per drawing unit.
    k: f32,
    ppp: f32,
    /// Stroke width in physical pixels.
    w: f32,
    /// Drawing grid (12, 14 or 16).
    grid: u8,
    icon: Icon,
}

impl Tf {
    fn px(&self, p: Pt) -> Pos2 {
        Pos2::new(self.o.x + p.0 * self.k, self.o.y + p.1 * self.k)
    }
    fn pt(&self, p: Pos2) -> Pos2 {
        Pos2::new(p.x / self.ppp, p.y / self.ppp)
    }
    /// Crisp edge: nearest pixel boundary, below ~1.5x scale only (above it strokes are 2+ px and already sharp).
    fn edge(&self, v: f32) -> f32 {
        if self.ppp >= 1.5 { v } else if TIE_UP { (v + 0.5).floor() } else { (v - 0.5).ceil() }
    }
}

fn axis(a: Pos2, b: Pos2) -> Option<bool> {
    if (a.x - b.x).abs() < 1e-3 { Some(true) } else if (a.y - b.y).abs() < 1e-3 { Some(false) } else { None }
}

fn stroke_poly(out: &mut Vec<EShape>, pts: &[LPt], closed: bool, tf: &Tf, color: Color32) {
    let v: Vec<Pos2> = pts.iter().map(|p| tf.px((p.0, p.1))).collect();
    let n = v.len();
    if n < 2 {
        return;
    }
    // (from, to, curved): `curved` segments come from flattened arcs.
    let mut segs: Vec<(usize, usize, bool)> = (0..if closed { n } else { n - 1 }).map(|i| (i, (i + 1) % n, pts[(i + 1) % n].2 == 1)).collect();
    let h = tf.w / 2.0;
    let axis_of = |&(i, j, c): &(usize, usize, bool)| if c { None } else { axis(v[i], v[j]) };
    let mut adj = v.clone();
    let mut any_axis = false;
    for sg in &segs {
        let Some(vertical) = axis_of(sg) else { continue };
        let (i, j) = (sg.0, sg.1);
        any_axis = true;
        let rect = if vertical {
            let (y0, y1) = (v[i].y.min(v[j].y) - h, v[i].y.max(v[j].y) + h);
            Rect::from_min_max(Pos2::new(tf.edge(v[i].x - h), tf.edge(y0)), Pos2::new(tf.edge(v[i].x + h), tf.edge(y1)))
        } else {
            let (x0, x1) = (v[i].x.min(v[j].x) - h, v[i].x.max(v[j].x) + h);
            Rect::from_min_max(Pos2::new(tf.edge(x0), tf.edge(v[i].y - h)), Pos2::new(tf.edge(x1), tf.edge(v[i].y + h)))
        };
        // Joined diagonals meet the middle of the rendered rect.
        if vertical { adj[i].x = (rect.min.x + rect.max.x) / 2.0; adj[j].x = adj[i].x; } else { adj[i].y = (rect.min.y + rect.max.y) / 2.0; adj[j].y = adj[i].y; }
        let r = Rect::from_min_max(tf.pt(rect.min), tf.pt(rect.max));
        out.push(EShape::rect_filled(r, 0.0, color));
    }
    let crisp = tf.ppp < 1.5;
    let factor = |pts: &[Pos2]| -> f32 {
        let (mut sum, mut len) = (0.0, 0.0);
        for w in pts.windows(2) {
            let d = w[1] - w[0];
            let l = d.length();
            if l > 1e-6 { sum += d.x.abs().max(d.y.abs()); len += l; }
        }
        if len > 0.0 { sum / len } else { 1.0 }
    };
    let aa = |out: &mut Vec<EShape>, pts: &[Pos2], closed: bool| {
        let stroke = Stroke::new(tf.w * factor(pts) / tf.ppp, color);
        let p: Vec<Pos2> = pts.iter().map(|&p| tf.pt(p)).collect();
        out.push(if closed { EShape::closed_line(p, stroke) } else { EShape::line(p, stroke) });
    };
    if closed && !any_axis {
        if crisp {
            let mut ring = adj.clone();
            ring.push(adj[0]);
            let thin: Vec<bool> = segs.iter().map(|s| s.2).collect();
            raster_line(out, tf, &ring, &thin, false, color);
        } else {
            aa(out, &adj, true);
        }
        return;
    }
    if closed {
        let first = segs.iter().position(|s| axis_of(s).is_some()).unwrap_or(0);
        segs.rotate_left(first);
    }
    // Runs of consecutive non-axis segments become one crisp raster / one anti-aliased path.
    let mut run: Vec<Pos2> = Vec::new();
    let mut thin: Vec<bool> = Vec::new();
    let flush = |run: &mut Vec<Pos2>, thin: &mut Vec<bool>, out: &mut Vec<EShape>| {
        if run.len() >= 2 {
            if crisp { raster_line(out, tf, run, thin, !closed, color) } else { aa(out, run, false) }
        }
        run.clear();
        thin.clear();
    };
    for (k, sg) in segs.iter().enumerate() {
        let (i, j) = (sg.0, sg.1);
        if axis_of(sg).is_some() {
            flush(&mut run, &mut thin, out);
            continue;
        }
        if run.is_empty() {
            // the AA path has no caps; the crisp raster adds its own
            run.push(if !crisp && !closed && k == 0 { extend(adj[i], adj[j], h) } else { adj[i] });
        }
        run.push(if !crisp && !closed && k == segs.len() - 1 { extend(adj[j], adj[i], h) } else { adj[j] });
        thin.push(sg.2);
    }
    flush(&mut run, &mut thin, out);
}

/// Crisp fill, as the sheet's `crispEdges` renderer: every physical pixel whose centre satisfies `inside` is inked.
/// `bbox` is in physical pixels; runs of pixels in a row become one quad.
fn raster(out: &mut Vec<EShape>, tf: &Tf, bbox: Rect, color: Color32, inside: impl Fn(f32, f32) -> bool) {
    let mut m = egui::Mesh::default();
    for y in (bbox.min.y.floor() as i32)..(bbox.max.y.ceil() as i32) {
        let mut x0: Option<i32> = None;
        for x in (bbox.min.x.floor() as i32)..=(bbox.max.x.ceil() as i32) {
            let on = x < bbox.max.x.ceil() as i32 && inside(x as f32 + 0.5, y as f32 + 0.5);
            match (on, x0) {
                (true, None) => x0 = Some(x),
                (false, Some(s)) => {
                    let r = Rect::from_min_max(tf.pt(Pos2::new(s as f32, y as f32)), tf.pt(Pos2::new(x as f32, y as f32 + 1.0)));
                    m.add_colored_rect(r, color);
                    x0 = None;
                }
                _ => {}
            }
        }
    }
    if !m.is_empty() {
        out.push(EShape::mesh(m));
    }
}

/// Pixels a width-1 curve inks: one per step along the major axis (Skia's non-AA hairline, what the sheet's renderer
/// draws for a 1 px arc), so a diagonal is one pixel thick instead of two.
fn hairline(pts: &[Pos2], thin: &[bool]) -> std::collections::HashSet<(i32, i32)> {
    let mut set = std::collections::HashSet::new();
    for (p, _) in pts.windows(2).zip(thin).filter(|(_, &t)| t) {
        let (a, b) = (p[0], p[1]);
        let d = b - a;
        if d.x.abs() >= d.y.abs() && d.x.abs() > 1e-6 {
            let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
            for c in ((lo - 0.5).ceil() as i32)..=((hi - 0.5).floor() as i32) {
                set.insert((c, (a.y + (c as f32 + 0.5 - a.x) * d.y / d.x).floor() as i32));
            }
        } else if d.y.abs() > 1e-6 {
            let (lo, hi) = (a.y.min(b.y), a.y.max(b.y));
            for r in ((lo - 0.5).ceil() as i32)..=((hi - 0.5).floor() as i32) {
                set.insert(((a.x + (r as f32 + 0.5 - a.y) * d.x / d.y).floor() as i32, r));
            }
        }
    }
    set
}

/// Crisp stroke of a polyline (square-ended segments of width `w`, all in physical pixels).
fn raster_line(out: &mut Vec<EShape>, tf: &Tf, pts: &[Pos2], curved: &[bool], mitre: bool, color: Color32) {
    let h = tf.w / 2.0;
    // At 12 px a 1 px arc is the sheet's one-pixel-per-step hairline; larger drawings use the thin band below.
    let hair = tf.w <= 1.001;
    let segs: Vec<(Pos2, egui::Vec2, f32, bool)> = pts.windows(2).zip(curved).filter_map(|(p, &c)| {
        let d = p[1] - p[0];
        let l = d.length();
        (l > 1e-6 && !(c && hair && tf.grid == 12)).then(|| (p[0], d / l, l, c && tf.w <= 1.001))
    }).collect();
    let hl = if hair { hairline(pts, curved) } else { Default::default() };
    // Mitre tips (SVG limit 4) at the corners between segments; nearly straight joins have none.
    let mut tips: Vec<[Pos2; 4]> = Vec::new();
    for (i, w3) in pts.windows(3).enumerate() {
        if !mitre || curved[i] || curved[i + 1] { continue; }
        let (d0, d1) = ((w3[1] - w3[0]).normalized(), (w3[2] - w3[1]).normalized());
        let cross = d0.x * d1.y - d0.y * d1.x;
        let dot = d0.x * d1.x + d0.y * d1.y;
        if !(cross.abs() > 0.05 && dot > -0.99) || 1.0 / ((1.0 + dot) / 2.0).sqrt() > 4.0 { continue; }
        let s = if cross > 0.0 { -1.0 } else { 1.0 };
        let (n0, n1) = (egui::vec2(-d0.y, d0.x) * s, egui::vec2(-d1.y, d1.x) * s);
        tips.push([w3[1], w3[1] + n0 * h, w3[1] + (n0 + n1) * h / (1.0 + n0.x * n1.x + n0.y * n1.y), w3[1] + n1 * h]);
    }
    let in_tri = |p: Pos2, a: Pos2, b: Pos2, c: Pos2| {
        let sg = |u: Pos2, v: Pos2| (p.x - v.x) * (u.y - v.y) - (u.x - v.x) * (p.y - v.y);
        let (d1, d2, d3) = (sg(a, b), sg(b, c), sg(c, a));
        !((d1 < 0.0 || d2 < 0.0 || d3 < 0.0) && (d1 > 0.0 || d2 > 0.0 || d3 > 0.0))
    };
    // An open arc's square end caps ink the pixels beside the end point (ties included), which the 1 px band misses.
    let mut caps: Vec<(Pos2, egui::Vec2)> = Vec::new();
    if mitre && hair && tf.grid != 12 && pts.len() >= 2 && curved.len() == pts.len() - 1 {
        if curved[0] { caps.push((pts[0], (pts[1] - pts[0]).normalized())); }
        if curved[curved.len() - 1] { let n = pts.len(); caps.push((pts[n - 1], (pts[n - 2] - pts[n - 1]).normalized())); }
    }
    let soft_caps = !mitre && hair && tf.grid == 12;
    let nseg = segs.len();
    let mut bb = Rect::NOTHING;
    for p in pts { bb.extend_with(*p); }
    let bb = bb.expand(h + 1.0);
    raster(out, tf, bb, color, |x, y| {
        caps.iter().any(|&(a, d)| { let (rx, ry) = (x - a.x, y - a.y); let (t, n) = (rx * d.x + ry * d.y, -rx * d.y + ry * d.x); t >= -h - 1e-3 && t <= 0.15 && n.abs() <= h + 1e-3 }) || hl.contains(&(x.floor() as i32, y.floor() as i32)) || (tf.grid != 12 && tips.iter().any(|q| { let p = Pos2::new(x, y); in_tri(p, q[0], q[1], q[2]) || in_tri(p, q[0], q[2], q[3]) })) || segs.iter().enumerate().any(|(i, &(a, d, l, thin))| {
            let (rx, ry) = (x - a.x, y - a.y);
            let (t, n) = (rx * d.x + ry * d.y, -rx * d.y + ry * d.x);
            // Other 1 px curves are thin: the cross distance shrinks by max(|cos|, |sin|); straight segments are polygons.
            let pencil = tf.icon == Icon::Edit && (tf.k - 1.0).abs() < 1e-3 && hair;
            let hh = if pencil { h * d.x.abs().max(d.y.abs()) } else if thin { h * d.x.abs().max(d.y.abs()) * if hair { BAND_WITH_HAIRLINE } else { 1.0 } } else { h };
            let (c0, c1) = (if soft_caps && i == 0 { 0.0 } else { h }, if soft_caps && i + 1 == nseg { 0.0 } else { h });
            // The pencil's 45° edges need a half-open band: pixel-corner ties ink exactly one side.
            t > -c0 && t < l + c1 && if pencil { n >= -hh - 1e-3 && n < hh - 1e-3 } else { n.abs() < hh }
        })
    });
    if soft_caps {
        // Closed 12 px shapes: the sheet's corner pixels past the ends of the diagonal run are faint.
        raster(out, tf, bb, color.gamma_multiply(SOFT_CAP), |x, y| {
            segs.iter().enumerate().any(|(i, &(a, d, l, thin))| {
                let (rx, ry) = (x - a.x, y - a.y);
                let (t, n) = (rx * d.x + ry * d.y, -rx * d.y + ry * d.x);
                let hh = if thin { h * d.x.abs().max(d.y.abs()) } else { h };
                n.abs() < hh && ((i == 0 && t > -h && t <= 0.0) || (i + 1 == nseg && t >= l && t < l + h))
            })
        });
    }
    if tf.grid == 12 && !tips.is_empty() {
        // 12 px mitre tips are faint in the sheet (its small drawings are partly anti-aliased).
        raster(out, tf, bb, color.gamma_multiply(SOFT_TIP), |x, y| {
            let p = Pos2::new(x, y);
            tips.iter().any(|q| in_tri(p, q[0], q[1], q[2]) || in_tri(p, q[0], q[2], q[3]))
        });
    }
}

/// `a` moved `h` further away from `b` (square cap).
fn extend(a: Pos2, b: Pos2, h: f32) -> Pos2 {
    let d = a - b;
    let l = d.length();
    if l < 1e-6 { a } else { a + d * (h / l) }
}

/// The shapes that draw `icon` at `size` in `rect` (empty when the icon has no such size).
pub fn shapes(ppp: f32, rect: Rect, icon: Icon, size: IconSize, color: Color32, bg: Color32) -> Vec<EShape> {
    shapes_at(ppp, rect, icon, size, color, bg, true)
}

/// As [`shapes`]; with `snap_origin` false the box keeps its fractional position (the sheet's icons sit on fractional
/// layout positions, so the specimen compares like with like).
pub fn shapes_at(ppp: f32, rect: Rect, icon: Icon, size: IconSize, color: Color32, bg: Color32, snap_origin: bool) -> Vec<EShape> {
    let mut out = Vec::new();
    let Some(prims) = drawing(icon, size.grid()) else { return out };
    let k = size.px() / size.grid() as f32 * ppp;
    // Every size keeps a 1 px stroke (1.5 pt from scale 1.5); 20 and 32 scale the 16 drawing but not its stroke (§9).
    let w = if ppp < 1.5 { 1.0 } else { 1.5 } * ppp;
    let o = if snap_origin { Pos2::new((rect.min.x * ppp).round(), (rect.min.y * ppp).round()) } else { rect.min * ppp };
    let tf = Tf { o, k, ppp, w, grid: size.grid(), icon };
    // These small strokes have a faint edge in the sheet's partially anti-aliased drawings.
    let stroke_color = if ppp < 1.5 && matches!((icon, size), (Icon::Check, IconSize::S12) | (Icon::Pen, IconSize::S14)) { color.gamma_multiply(0.94) } else { color };
    for p in prims {
        let fill = match p.p { Paint::Ink => color, Paint::Tone => color.gamma_multiply(0.35), Paint::Bg => bg };
        match p.s {
            Shape::Line(pts) => stroke_poly(&mut out, pts, false, &tf, stroke_color),
            Shape::Poly(pts) => stroke_poly(&mut out, pts, true, &tf, stroke_color),
            Shape::Circle(cx, cy, r) if ppp < 1.5 => {
                let (c, r, h) = (tf.px((cx, cy)), r * k, w / 2.0);
                let soft12 = w <= 1.001 && tf.grid == 12 && (3.5..4.5).contains(&r);
                raster(&mut out, &tf, Rect::from_center_size(c, egui::Vec2::splat(2.0 * (r + h) + 2.0)), color, |x, y| {
                    let (dx, dy) = (x - c.x, y - c.y);
                    let d = dx.hypot(dy);
                    let centred = (c.x.fract() - 0.5).abs() < 0.2 && (c.y.fract() - 0.5).abs() < 0.2;
                    let hh = if w <= 1.001 && r >= 2.5 && d > 1e-6 { h * dx.abs().max(dy.abs()) / d } else if w <= 1.001 && (1.3..2.5).contains(&r) { if centred { if r >= 1.8 { h * 1.25 } else { h } } else { h * 1.05 } } else { h };
                    (d - r).abs() < hh && !(soft12 && dx.abs().max(dy.abs()) < 0.85 * d)
                });
                if soft12 {
                    // 12 px rings: the diagonal pixels are fainter in the sheet's anti-aliased variant.
                    raster(&mut out, &tf, Rect::from_center_size(c, egui::Vec2::splat(2.0 * (r + h) + 2.0)), color.gamma_multiply(SOFT_DIAG), |x, y| {
                        let (dx, dy) = (x - c.x, y - c.y);
                        let d = dx.hypot(dy);
                        (d - r).abs() < h * dx.abs().max(dy.abs()) / d.max(1e-6) && dx.abs().max(dy.abs()) < 0.85 * d
                    });
                }
                // The sheet's rings are partly anti-aliased: r = 5 on the 14 grid and the 20 px ring (r 6.25) get a faint outer edge
                // (pixels just outside the thin band), which is what lands between the crisp and the anti-aliased variants.
                if w <= 1.001 && ((tf.grid == 14 && (4.5..5.5).contains(&r)) || (tf.k > 1.2 * ppp && (6.0..6.5).contains(&r))) {
                    raster(&mut out, &tf, Rect::from_center_size(c, egui::Vec2::splat(2.0 * (r + h) + 2.0)), color.gamma_multiply(SOFT_RING), |x, y| {
                        let (dx, dy) = (x - c.x, y - c.y);
                        let d = dx.hypot(dy);
                        let e = (d - r).abs();
                        e >= h * dx.abs().max(dy.abs()) / d.max(1e-6) && e < h * 1.1
                    });
                }
            }
            Shape::Circle(cx, cy, r) => {
                let f = if DIAGONAL_FACTOR { 0.9 } else { 1.0 }; // mean of max(|cos|, |sin|) round a circle
                out.push(EShape::circle_stroke(tf.pt(tf.px((cx, cy))), r * k / ppp, Stroke::new(w * f / ppp, color)))
            }
            Shape::Disc(cx, cy, r) if ppp < 1.5 => {
                let (c, r) = (tf.px((cx, cy)), r * k);
                raster(&mut out, &tf, Rect::from_center_size(c, egui::Vec2::splat(2.0 * r + 2.0)), fill, |x, y| (x - c.x).hypot(y - c.y) < r);
            }
            Shape::Disc(cx, cy, r) => out.push(EShape::circle_filled(tf.pt(tf.px((cx, cy))), r * k / ppp, fill)),
            Shape::Tris(pts) => {
                let mut m = egui::Mesh::default();
                for t in pts.chunks(3) {
                    let i = m.vertices.len() as u32;
                    for &q in t {
                        m.vertices.push(Vertex { pos: tf.pt(tf.px(q)), uv: WHITE_UV, color: fill });
                    }
                    m.indices.extend([i, i + 1, i + 2]);
                }
                out.push(EShape::mesh(m));
            }
        }
    }
    out
}

/// Draw `icon` at `size` into `rect` (its top-left corner is snapped to a physical pixel). `bg` fills the knobs of `sliders`.
pub fn paint_bg(painter: &egui::Painter, rect: Rect, icon: Icon, size: IconSize, color: Color32, bg: Color32) {
    debug_assert!(drawing(icon, size.grid()).is_some(), "{icon:?} has no {}-px drawing (bible §9)", size.grid());
    painter.extend(shapes(painter.ctx().pixels_per_point(), rect, icon, size, color, bg));
}

/// [`paint_bg`] without snapping the box to a physical pixel (specimen only).
pub fn paint_unsnapped(painter: &egui::Painter, rect: Rect, icon: Icon, size: IconSize, color: Color32, bg: Color32) {
    painter.extend(shapes_at(painter.ctx().pixels_per_point(), rect, icon, size, color, bg, false));
}

pub fn paint(painter: &egui::Painter, rect: Rect, icon: Icon, size: IconSize, color: Color32) {
    paint_bg(painter, rect, icon, size, color, Color32::TRANSPARENT);
}

/// Allocate a `size` x `size` box in `ui` and draw the icon into it.
pub fn icon(ui: &mut Ui, icon: Icon, size: IconSize, color: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(size.px(), size.px()), Sense::hover());
    paint_bg(ui.painter(), rect, icon, size, color, ui.visuals().panel_fill);
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(grid: u8) -> usize {
        Icon::ALL.iter().filter(|&&i| drawing(i, grid).is_some()).count()
    }

    #[test]
    fn icon_size_availability_follows_the_bible_lists() {
        // §9: 12 px and 14 px exist only for the listed icons; everything else has 16 (and so 20, 32) only.
        assert_eq!(count(12), 21);
        let expected14 = [Icon::Info, Icon::Warning, Icon::Error, Icon::Success, Icon::Film, Icon::Gvm, Icon::Hint, Icon::Key, Icon::Deliver, Icon::Pen, Icon::Sam, Icon::Machine, Icon::MachineNetwork, Icon::Gpu, Icon::Folder, Icon::Copy, Icon::Reveal, Icon::More, Icon::Close, Icon::Cancel, Icon::Retry, Icon::Refresh, Icon::Dismiss, Icon::Reset];
        assert_eq!(count(14), expected14.len());
        for icon in Icon::ALL {
            assert_eq!(drawing(icon, 14).is_some(), expected14.contains(&icon), "{icon:?}");
        }
        assert_eq!(count(16), 80);
        for i in [Icon::Wipe, Icon::PanelRight, Icon::Overlay, Icon::Play, Icon::Board] {
            assert!(drawing(i, 12).is_none() && drawing(i, 14).is_none() && drawing(i, 16).is_some(), "{i:?}");
        }
        assert!(drawing(Icon::Key, 12).is_some() && drawing(Icon::Key, 14).is_some() && drawing(Icon::Key, 16).is_some());
        assert!(drawing(Icon::SortUp, 12).is_some() && drawing(Icon::SortUp, 16).is_none());
        assert!(drawing(Icon::Success, 12).is_some() && drawing(Icon::Success, 14).is_some() && drawing(Icon::Success, 16).is_none());
        // paint at an unavailable size draws nothing.
        let r = Rect::from_min_size(Pos2::ZERO, egui::vec2(12.0, 12.0));
        assert!(shapes(1.0, r, Icon::Wipe, IconSize::S12, Color32::WHITE, Color32::BLACK).is_empty());
    }

    #[test]
    fn every_icon_draws_at_each_of_its_sizes() {
        let r = Rect::from_min_size(Pos2::ZERO, egui::vec2(32.0, 32.0));
        for icon in Icon::ALL {
            let mut sizes = 0;
            for size in IconSize::ALL {
                if drawing(icon, size.grid()).is_none() {
                    continue;
                }
                sizes += 1;
                for ppp in [1.0, 2.0] {
                    assert!(!shapes(ppp, r, icon, size, Color32::WHITE, Color32::BLACK).is_empty(), "{icon:?} {size:?} @{ppp}");
                }
            }
            assert!(sizes >= 1, "{icon:?} has no size");
        }
    }
}
