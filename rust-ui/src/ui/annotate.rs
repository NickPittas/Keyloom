//! Annotation tools on the viewer (Hint page): SAM points, one box, paint/holdout strokes, erase. Pointer
//! positions are converted to SOURCE pixels through the image rectangle of the current frame, so zoom and pan
//! never change what is stored. Tools take the primary button (the viewer's primary-drag pan is suspended
//! while one is active; middle-drag and the wheel still navigate).
use crate::hints::{FrameAnn, Pt, Stroke as PaintStroke};
use eframe::egui::{
    self, Color32, Painter, Pos2, Rect, Stroke, TextureHandle, TextureOptions, Vec2,
};
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Tool {
    #[default]
    None,
    FgPoint,
    BgPoint,
    Box,
    PaintFg,
    PaintBg,
    Erase,
    /// Click a screen colour for the chroma-key generator.
    Pick,
    /// Chroma holdout: certain foreground / certain background (static for the clip, not SAM prompts).
    HoldFg,
    HoldBg,
    EraseHold,
}

impl Tool {
    pub const ALL: [Tool; 11] = [
        Tool::None,
        Tool::FgPoint,
        Tool::BgPoint,
        Tool::Box,
        Tool::PaintFg,
        Tool::PaintBg,
        Tool::Erase,
        Tool::Pick,
        Tool::HoldFg,
        Tool::HoldBg,
        Tool::EraseHold,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tool::None => "Navigate",
            Tool::FgPoint => "Foreground point",
            Tool::BgPoint => "Background point",
            Tool::Box => "Box",
            Tool::PaintFg => "Paint foreground",
            Tool::PaintBg => "Paint background / holdout",
            Tool::Erase => "Erase",
            Tool::Pick => "Pick chroma colour",
            Tool::HoldFg => "Holdout: keep as foreground (chroma)",
            Tool::HoldBg => "Holdout: force background (chroma)",
            Tool::EraseHold => "Erase holdout strokes",
        }
    }
    /// Compact chip text; the full name is the hover text.
    pub fn short(self) -> &'static str {
        match self {
            Tool::None => "Navigate",
            Tool::FgPoint => "FG point",
            Tool::BgPoint => "BG point",
            Tool::Box => "Box",
            Tool::PaintFg => "Paint FG",
            Tool::PaintBg => "Paint BG",
            Tool::Erase => "Erase",
            Tool::Pick => "Pick colour",
            Tool::HoldFg => "Holdout FG",
            Tool::HoldBg => "Holdout BG",
            Tool::EraseHold => "Erase holdout",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ann {
    Point(Pt),
    Box([f64; 4]),
    Stroke(PaintStroke),
    Erase {
        x: f64,
        y: f64,
        r: f64,
    },
    /// Chroma colour pick at a source pixel (not an annotation).
    Pick {
        x: f64,
        y: f64,
    },
    /// Holdout stroke / eraser (the host stores these in `holdout_strokes.json`).
    Hold(PaintStroke),
    HoldErase {
        x: f64,
        y: f64,
        r: f64,
    },
}

/// Screen point -> source pixel, `None` outside the frame.
pub fn to_src(pos: Pos2, img: Rect, size: [usize; 2]) -> Option<[f64; 2]> {
    if !img.contains(pos) || img.width() <= 0.0 || img.height() <= 0.0 {
        return None;
    }
    Some([
        ((pos.x - img.left()) / img.width()) as f64 * size[0] as f64,
        ((pos.y - img.top()) / img.height()) as f64 * size[1] as f64,
    ])
}
pub fn to_screen(p: [f64; 2], img: Rect, size: [usize; 2]) -> Pos2 {
    Pos2::new(
        img.left() + (p[0] / size[0] as f64) as f32 * img.width(),
        img.top() + (p[1] / size[1] as f64) as f32 * img.height(),
    )
}

#[derive(Default)]
pub struct AnnState {
    pub tool: Tool,
    /// Brush radius in source pixels.
    pub brush: f64,
    /// Edits made since the host last drained them.
    pub events: Vec<Ann>,
    /// Annotations of the frame on screen (the host refreshes it every frame).
    pub overlay: FrameAnn,
    /// The clip's static chroma holdout strokes (drawn on every frame, in other colours).
    pub holdout: Vec<PaintStroke>,
    /// Preview mask PNG to tint over the frame (current prompt signature only; the host decides).
    pub preview: Option<PathBuf>,
    drag: Vec<[f64; 2]>,
    box_start: Option<[f64; 2]>,
    tex: Option<(PathBuf, Option<TextureHandle>)>,
}

impl AnnState {
    pub fn new() -> Self {
        Self {
            brush: 12.0,
            ..Default::default()
        }
    }
    pub fn active(&self) -> bool {
        self.tool != Tool::None
    }

    fn preview_tex(&mut self, ctx: &egui::Context) -> Option<&TextureHandle> {
        let want = self.preview.clone()?;
        if self.tex.as_ref().is_none_or(|(p, _)| *p != want) {
            let tex = (|| {
                let mut d =
                    png::Decoder::new(std::io::BufReader::new(std::fs::File::open(&want).ok()?));
                d.set_transformations(png::Transformations::EXPAND);
                let mut r = d.read_info().ok()?;
                let mut buf = vec![0; r.output_buffer_size()?];
                let info = r.next_frame(&mut buf).ok()?;
                let ch = info.color_type.samples();
                let px = (info.width * info.height) as usize;
                let rgba: Vec<u8> = (0..px)
                    .flat_map(|i| {
                        let on = buf[i * ch] > 127;
                        if on {
                            [60, 220, 120, 110]
                        } else {
                            [0, 0, 0, 0]
                        }
                    })
                    .collect();
                let img = egui::ColorImage::from_rgba_unmultiplied(
                    [info.width as usize, info.height as usize],
                    &rgba,
                );
                Some(ctx.load_texture("sam-preview", img, TextureOptions::LINEAR))
            })();
            self.tex = Some((want, tex));
        }
        self.tex.as_ref().and_then(|t| t.1.as_ref())
    }

    /// Draw the overlays and handle the active tool for one frame. `img` is the on-screen frame rectangle.
    pub fn run(
        &mut self,
        ctx: &egui::Context,
        p: &Painter,
        resp: &egui::Response,
        img: Rect,
        size: [usize; 2],
    ) {
        if let Some(t) = self.preview_tex(ctx) {
            p.image(
                t.id(),
                img,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }
        let scale = img.width() / size[0] as f32;
        for s in &self.overlay.strokes {
            let col = if s.fg {
                Color32::from_rgba_unmultiplied(80, 230, 120, 110)
            } else {
                Color32::from_rgba_unmultiplied(235, 90, 90, 110)
            };
            let pts: Vec<Pos2> = s.pts.iter().map(|q| to_screen(*q, img, size)).collect();
            let w = (s.radius as f32 * 2.0 * scale).max(2.0);
            if pts.len() == 1 {
                p.circle_filled(pts[0], w / 2.0, col);
            } else {
                p.add(egui::Shape::line(pts, Stroke::new(w, col)));
            }
        }
        for s in &self.holdout {
            let col = if s.fg {
                Color32::from_rgba_unmultiplied(255, 190, 60, 120)
            } else {
                Color32::from_rgba_unmultiplied(170, 90, 235, 120)
            };
            let pts: Vec<Pos2> = s.pts.iter().map(|q| to_screen(*q, img, size)).collect();
            let w = (s.radius as f32 * 2.0 * scale).max(2.0);
            if pts.len() == 1 {
                p.circle_filled(pts[0], w / 2.0, col);
            } else {
                p.add(egui::Shape::line(pts, Stroke::new(w, col)));
            }
        }
        for b in &self.overlay.boxes {
            p.rect_stroke(
                Rect::from_two_pos(
                    to_screen([b[0], b[1]], img, size),
                    to_screen([b[2], b[3]], img, size),
                ),
                0.0,
                Stroke::new(2.0, Color32::from_rgb(120, 190, 255)),
                egui::StrokeKind::Middle,
            );
        }
        for q in &self.overlay.points {
            let c = if q.label == 1 {
                Color32::from_rgb(80, 230, 120)
            } else {
                Color32::from_rgb(235, 90, 90)
            };
            let s = to_screen([q.x, q.y], img, size);
            p.circle_filled(s, 5.0, c);
            p.circle_stroke(s, 5.0, Stroke::new(1.5, Color32::BLACK));
        }
        if !self.active() {
            return;
        }
        let pos = resp.interact_pointer_pos().or(resp.hover_pos());
        let src = pos.and_then(|q| to_src(q, img, size));
        if matches!(
            self.tool,
            Tool::PaintFg
                | Tool::PaintBg
                | Tool::Erase
                | Tool::HoldFg
                | Tool::HoldBg
                | Tool::EraseHold
        ) {
            if let Some(q) = pos.filter(|q| img.contains(*q)) {
                p.circle_stroke(
                    q,
                    (self.brush as f32 * scale).max(2.0),
                    Stroke::new(1.5, Color32::WHITE),
                );
            }
        }
        let primary = resp.dragged_by(egui::PointerButton::Primary)
            || resp.clicked_by(egui::PointerButton::Primary);
        match self.tool {
            Tool::FgPoint | Tool::BgPoint => {
                if resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(s) = src {
                        self.events.push(Ann::Point(Pt {
                            x: s[0],
                            y: s[1],
                            label: (self.tool == Tool::FgPoint) as u8,
                        }));
                    }
                }
            }
            Tool::Box => {
                if resp.drag_started_by(egui::PointerButton::Primary) {
                    self.box_start = src;
                }
                if let (Some(a), Some(q)) = (self.box_start, pos) {
                    p.rect_stroke(
                        Rect::from_two_pos(to_screen(a, img, size), q),
                        0.0,
                        Stroke::new(2.0, Color32::from_rgb(255, 210, 90)),
                        egui::StrokeKind::Middle,
                    );
                }
                if resp.drag_stopped() {
                    if let (Some(a), Some(b)) =
                        (self.box_start.take(), pos.map(|q| clamp_src(q, img, size)))
                    {
                        let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
                        let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
                        if x1 - x0 >= 2.0 && y1 - y0 >= 2.0 {
                            self.events.push(Ann::Box([x0, y0, x1, y1]));
                        }
                    }
                }
            }
            Tool::PaintFg | Tool::PaintBg | Tool::HoldFg | Tool::HoldBg => {
                if resp.dragged_by(egui::PointerButton::Primary) {
                    if let Some(s) = src {
                        if self
                            .drag
                            .last()
                            .is_none_or(|l| (l[0] - s[0]).hypot(l[1] - s[1]) >= 1.0)
                        {
                            self.drag.push(s);
                        }
                    }
                }
                if self.drag.len() > 1 || resp.clicked_by(egui::PointerButton::Primary) {
                    if self.drag.is_empty() {
                        self.drag.extend(src);
                    }
                    let pts: Vec<Pos2> =
                        self.drag.iter().map(|q| to_screen(*q, img, size)).collect();
                    let col = match self.tool {
                        Tool::PaintFg => Color32::from_rgba_unmultiplied(80, 230, 120, 140),
                        Tool::PaintBg => Color32::from_rgba_unmultiplied(235, 90, 90, 140),
                        Tool::HoldFg => Color32::from_rgba_unmultiplied(255, 190, 60, 150),
                        _ => Color32::from_rgba_unmultiplied(170, 90, 235, 150),
                    };
                    if pts.len() > 1 {
                        p.add(egui::Shape::line(
                            pts,
                            Stroke::new(self.brush as f32 * 2.0 * scale, col),
                        ));
                    }
                }
                if resp.drag_stopped() || resp.clicked_by(egui::PointerButton::Primary) {
                    if !self.drag.is_empty() {
                        let stroke = PaintStroke {
                            fg: matches!(self.tool, Tool::PaintFg | Tool::HoldFg),
                            radius: self.brush,
                            pts: std::mem::take(&mut self.drag),
                        };
                        self.events
                            .push(if matches!(self.tool, Tool::HoldFg | Tool::HoldBg) {
                                Ann::Hold(stroke)
                            } else {
                                Ann::Stroke(stroke)
                            });
                    }
                }
            }
            Tool::EraseHold => {
                if primary {
                    if let Some(s) = src {
                        self.events.push(Ann::HoldErase {
                            x: s[0],
                            y: s[1],
                            r: self.brush,
                        });
                    }
                }
            }
            Tool::Erase => {
                if primary {
                    if let Some(s) = src {
                        self.events.push(Ann::Erase {
                            x: s[0],
                            y: s[1],
                            r: self.brush,
                        });
                    }
                }
            }
            Tool::Pick => {
                if resp.clicked_by(egui::PointerButton::Primary) {
                    if let Some(s) = src {
                        self.events.push(Ann::Pick { x: s[0], y: s[1] });
                    }
                }
            }
            Tool::None => {}
        }
        // `Vec2` keeps the import used on every cfg.
        let _ = Vec2::ZERO;
    }
}

fn clamp_src(pos: Pos2, img: Rect, size: [usize; 2]) -> [f64; 2] {
    let c = Pos2::new(
        pos.x.clamp(img.left(), img.right()),
        pos.y.clamp(img.top(), img.bottom()),
    );
    to_src(c, img, size).unwrap_or([0.0, 0.0])
}

/// Apply one edit to a frame's annotations. A second box replaces the first (SAM takes one per frame).
pub fn apply(f: &mut FrameAnn, a: &Ann) -> bool {
    match a {
        Ann::Point(p) => f.points.push(*p),
        Ann::Box(b) => f.boxes = vec![*b],
        Ann::Stroke(s) => f.strokes.push(s.clone()),
        Ann::Erase { x, y, r } => {
            let n = f.points.len() + f.boxes.len() + f.strokes.len();
            f.points.retain(|p| (p.x - x).hypot(p.y - y) > *r);
            f.boxes.retain(|b| {
                !(*x >= b[0] - r
                    && *x <= b[2] + r
                    && *y >= b[1] - r
                    && *y <= b[3] + r
                    && near_edge(b, *x, *y, *r))
            });
            f.strokes.retain(|s| {
                !s.pts
                    .iter()
                    .any(|q| (q[0] - x).hypot(q[1] - y) <= *r + s.radius)
            });
            return f.points.len() + f.boxes.len() + f.strokes.len() != n;
        }
        Ann::Pick { .. } | Ann::Hold(_) | Ann::HoldErase { .. } => return false,
    }
    true
}

fn near_edge(b: &[f64; 4], x: f64, y: f64, r: f64) -> bool {
    let inside_x = x >= b[0] && x <= b[2];
    let inside_y = y >= b[1] && y <= b[3];
    (inside_x && ((y - b[1]).abs() <= r || (y - b[3]).abs() <= r))
        || (inside_y && ((x - b[0]).abs() <= r || (x - b[2]).abs() <= r))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_coordinates_do_not_depend_on_zoom_or_pan() {
        let size = [1920usize, 1080];
        let fit = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(960.0, 540.0));
        // Same source pixel (480, 270): at fit it is at the centre-ish of the rectangle...
        let at_fit = to_screen([480.0, 270.0], fit, size);
        // ...zoomed 3x and panned, the rectangle changes but the mapped source pixel does not.
        let zoomed = Rect::from_min_size(Pos2::new(-400.0, -120.0), Vec2::new(2880.0, 1620.0));
        let at_zoom = to_screen([480.0, 270.0], zoomed, size);
        for (pt, r) in [(at_fit, fit), (at_zoom, zoomed)] {
            let s = to_src(pt, r, size).unwrap();
            assert!(
                (s[0] - 480.0).abs() < 0.01 && (s[1] - 270.0).abs() < 0.01,
                "{s:?}"
            );
        }
        assert!(to_src(Pos2::new(5.0, 5.0), fit, size).is_none());
    }

    #[test]
    fn edits_apply_one_box_and_erase_removes_hit_marks_only() {
        let mut f = FrameAnn::default();
        apply(
            &mut f,
            &Ann::Point(Pt {
                x: 10.0,
                y: 10.0,
                label: 1,
            }),
        );
        apply(
            &mut f,
            &Ann::Point(Pt {
                x: 200.0,
                y: 200.0,
                label: 0,
            }),
        );
        apply(&mut f, &Ann::Box([0.0, 0.0, 50.0, 50.0]));
        apply(&mut f, &Ann::Box([5.0, 5.0, 60.0, 60.0]));
        assert_eq!(f.boxes.len(), 1);
        assert!(apply(
            &mut f,
            &Ann::Erase {
                x: 11.0,
                y: 11.0,
                r: 4.0
            }
        ));
        assert_eq!(f.points.len(), 1);
        assert!(!apply(
            &mut f,
            &Ann::Erase {
                x: 500.0,
                y: 500.0,
                r: 4.0
            }
        ));
    }
}
