//! The one shared viewer: mode chips (top-right), A/B wipe (top-centre), icon transport (centred below),
//! scrubbable timeline with In/Out band. One instance per open clip, shared by all workspace tabs.
use crate::decode::DisplayFrame;
use crate::frames::*;
use crate::project::{Clip, View};
use super::theme;
use super::controls::viewer_toolbar::{Background, Channel};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, Sense, Shape, Stroke, TextureHandle,
    TextureOptions, Ui, UiBuilder, Vec2,
};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

pub const WARN: Color32 = Color32::from_rgb(214, 168, 90);
pub const DIM: Color32 = Color32::from_rgb(150, 154, 160);
const ACCENT: Color32 = Color32::from_rgb(110, 150, 190);

const MAX_ZOOM: f32 = 32.0;
/// Transport widths: the six icons, and Set In / Set Out / Clear In/Out / Loop.
const ICONS_W: f32 = 34.0 * 6.0 + 5.0 * 4.0;
const RANGE_W: f32 = 92.0 + 100.0 + 104.0 + 78.0 + 3.0 * 4.0;
/// Width reserved for the view-mode chips at the right of the top bar.
const CHIPS_W: f32 = 400.0;
/// View, B view, input reading, alpha convention, In/Out bounds, looping.
type PreviewSig = (View, Option<View>, bool, bool, (usize, usize), bool);

fn mb(bytes: usize) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

/// Which generated mask is tinted over the Input frame.
#[derive(Default, Clone, Copy, PartialEq, Debug)]
pub enum Overlay {
    #[default]
    Off,
    Guide,
    Hint,
}

/// A marked-frame request from the viewer's mark controls, carried out by the Hint page.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum MarkReq {
    Mark,
    Unmark,
    Clear,
}

/// What the Shot toolbar asks the picture to show. Default = untouched pixels over the checker (the old screen).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct DisplayOpts {
    pub channel: Channel,
    pub gain: f32,
    pub gamma: f32,
    pub background: Background,
    pub wipe_horizontal: bool,
}
impl Default for DisplayOpts {
    fn default() -> Self {
        Self { channel: Channel::Rgb, gain: 1.0, gamma: 1.0, background: Background::Checker, wipe_horizontal: false }
    }
}
impl DisplayOpts {
    /// Channel/gain/gamma leave the pixels as decoded (background and wipe direction never touch the texture).
    fn pixels_untouched(&self) -> bool {
        self.channel == Channel::Rgb && self.gain == 1.0 && self.gamma == 1.0
    }
}

/// The texture for `f` with channel/gain/gamma applied on the CPU. Straight alpha stays straight for alpha
/// frames; opaque frames keep the old premultiplied constructor (alpha is 255 there).
fn display_image(f: &DisplayFrame, o: &DisplayOpts) -> egui::ColorImage {
    let size = [f.width, f.height];
    if o.pixels_untouched() {
        return if f.has_alpha {
            egui::ColorImage::from_rgba_unmultiplied(size, &f.rgba)
        } else {
            egui::ColorImage::from_rgba_premultiplied(size, &f.rgba)
        };
    }
    // gain, then pow(1/gamma), per 8-bit value.
    let inv_g = 1.0 / (o.gamma.max(0.01) as f64);
    let lut: Vec<u8> = (0..256)
        .map(|v| (((v as f64 / 255.0) * o.gain as f64).max(0.0).powf(inv_g) * 255.0 + 0.5).clamp(0.0, 255.0) as u8)
        .collect();
    let mut out = Vec::with_capacity(f.rgba.len());
    for px in f.rgba.chunks_exact(4) {
        let (r, g, b, a) = (px[0], px[1], px[2], px[3]);
        let (grey, alpha) = match o.channel {
            Channel::Rgb => {
                out.extend_from_slice(&[lut[r as usize], lut[g as usize], lut[b as usize], a]);
                continue;
            }
            Channel::R => (r, a),
            Channel::G => (g, a),
            Channel::B => (b, a),
            Channel::A => (a, 255), // alpha as an opaque grey picture
            Channel::Luma => ((0.2126 * r as f32 + 0.7152 * g as f32 + 0.0722 * b as f32 + 0.5) as u8, a),
        };
        let v = lut[grey as usize];
        out.extend_from_slice(&[v, v, v, alpha]);
    }
    if f.has_alpha {
        egui::ColorImage::from_rgba_unmultiplied(size, &out)
    } else {
        egui::ColorImage::from_rgba_premultiplied(size, &out)
    }
}

struct Tex {
    /// The decoded frame this texture was built from (rebuilt from it when the display options change; also the
    /// pixel readout source).
    frame: Arc<DisplayFrame>,
    opts: DisplayOpts,
    handle: TextureHandle,
    size: [usize; 2],
    has_alpha: bool,
    clipped: bool,
    info: String,
}

pub struct Viewer {
    /// Hint-page annotation tools and overlay (source-pixel coordinates).
    pub ann: super::annotate::AnnState,
    /// Hint keyframe marks as timeline indices (drawn as ticks).
    pub marks: Vec<usize>,
    /// Mask tint (Off / Pre-matte / Hint), and the independent SAM prompt/preview layer; the SAM layer only
    /// exists while the pre-matte producer is SAM (`sam_available`, set by the Hint page).
    pub overlay: Overlay,
    pub sam_overlay: bool,
    pub sam_available: bool,
    /// Mark / Unmark / Clear marks pressed this frame.
    pub mark_req: Option<MarkReq>,
    /// Controls drawn this frame (name, rect), for layout tests.
    pub ui_rects: Vec<(&'static str, Rect)>,
    /// Widths the A/B and overlay chip groups really took last frame (their text sets the size, not our estimate).
    group_w: [f32; 2],
    pub clip: usize,
    pub index: FrameIndex,
    pub store: FrameStore,
    pub view: View,
    pub ab: bool,
    pub b_view: View,
    pub wipe: f32,
    pub playhead: usize,
    pub clock: Option<Clock>,
    /// Inclusive timeline indices. Edited in memory only.
    pub range: Option<(usize, usize)>,
    pub range_stored: Option<(usize, usize)>,
    /// In-memory input-colour interpretation (display only; never written).
    pub linear: bool,
    pub linear_stored: bool,
    pub fps: f64,
    pub fps_assumed: bool,
    /// Manifest says Processed EXR is straight-linear (else it is premultiplied, legacy).
    pub straight: bool,
    pub drops: u64,
    /// Playback wraps inside In/Out (or the whole clip) in both directions instead of stopping at the end.
    pub looping: bool,
    /// 1.0 = fit; `pan` is the image centre's offset from the viewport centre in screen points.
    pub zoom: f32,
    pub pan: Vec2,
    /// Direction of a RAM preview that is still filling the cache; the clock starts when it is complete.
    buffering: Option<isize>,
    /// Why the last Play streams instead of buffering (range over the RAM budget).
    pub preview_note: Option<String>,
    /// What the running preview was buffered for; a change (view, A/B, interpretation, range) re-buffers.
    preview_sig: Option<PreviewSig>,
    /// Bytes of one decoded frame as promised by the clip's metadata (0 = unknown until one is decoded).
    est_frame_bytes: usize,
    tex: HashMap<Key, Tex>,
    tex_order: Vec<Key>,
    held: Option<Key>,
    /// Height of the image area at the last draw (layout tests).
    pub draw_h: f32,
    /// Screen points per image pixel at Fit, from the last draw (0 = nothing drawn yet).
    fit_scale: f32,
    /// Toolbar display choices; `Tex::opts` older than this means the texture is rebuilt from its cached frame.
    disp: DisplayOpts,
    /// Set by `set_display` (the Shot page): the checkerboard then uses the design-bible 8 px #666/#999 cells.
    display_set: bool,
    /// Where the image was drawn last frame (screen rect), for `pixel_at`.
    img_rect: Rect,
    /// True when `image_ui` (the Shot page) draws: the pixel readout owns the bottom-left corner.
    readout: bool,
}

fn icon_btn(ui: &mut Ui, kind: u8, active: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 28.0), Sense::click());
    let p = ui.painter();
    use super::theme as th;
    let w = &ui.visuals().widgets;
    // Pressed-in (active) = accent fill with dark ink; otherwise the stock button states of the current palette.
    let (bg, edge, c) = if active {
        (if ui.visuals().dark_mode { th::CHECKED } else { Color32::from_rgb(157, 192, 165) }, w.hovered.bg_stroke.color, th::ACCENT_TEXT)
    } else if resp.hovered() {
        (w.hovered.weak_bg_fill, w.hovered.bg_stroke.color, w.hovered.fg_stroke.color)
    } else {
        (w.inactive.weak_bg_fill, w.inactive.bg_stroke.color, w.inactive.fg_stroke.color)
    };
    p.rect_filled(rect, 3.0, bg);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    if resp.has_focus() {
        p.rect_stroke(
            rect.expand(1.0),
            3.0,
            Stroke::new(2.0, th::warn(ui)),
            egui::StrokeKind::Outside,
        );
    }
    let m = rect.center();
    let tri = |dir: f32, x: f32| {
        Shape::convex_polygon(
            vec![
                Pos2::new(x + dir * 5.0, m.y),
                Pos2::new(x - dir * 4.0, m.y - 7.0),
                Pos2::new(x - dir * 4.0, m.y + 7.0),
            ],
            c,
            Stroke::NONE,
        )
    };
    let bar = |x: f32| {
        Shape::rect_filled(
            Rect::from_center_size(Pos2::new(x, m.y), Vec2::new(2.5, 14.0)),
            0.0,
            c,
        )
    };
    match kind {
        0 => {
            p.add(bar(m.x - 7.0));
            p.add(tri(-1.0, m.x + 1.0));
            p.add(tri(-1.0, m.x + 8.0));
        } // ⏮
        1 => {
            p.add(tri(-1.0, m.x - 2.0));
            p.add(bar(m.x + 6.0));
        } // ◀|
        2 => {
            p.add(tri(-1.0, m.x));
        } // ◀ reverse play
        3 => {
            p.add(tri(1.0, m.x));
        } // ▶ play
        4 => {
            p.add(bar(m.x - 6.0));
            p.add(tri(1.0, m.x + 2.0));
        } // |▶
        5 => {
            p.add(tri(1.0, m.x - 8.0));
            p.add(tri(1.0, m.x - 1.0));
            p.add(bar(m.x + 7.0));
        } // ⏭
        _ => {
            p.add(bar(m.x - 4.0));
            p.add(bar(m.x + 4.0));
        } // pause
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip)
}

impl Viewer {
    pub fn new(clip_idx: usize, clip: &Clip, cache_mb: usize, ctx: &egui::Context) -> Self {
        Self::new_with_workers(clip_idx, clip, cache_mb, default_workers(), ctx)
    }

    pub fn new_with_workers(
        clip_idx: usize,
        clip: &Clip,
        cache_mb: usize,
        workers: usize,
        ctx: &egui::Context,
    ) -> Self {
        let index = FrameIndex::build(clip);
        let c = ctx.clone();
        let store = FrameStore::new(
            cache_mb << 20,
            workers,
            real_decode(),
            Some(Arc::new(move || c.request_repaint())),
        );
        let range = clip_range_to_timeline(clip, &index);
        let first = [
            View::Input,
            View::Processed,
            View::Comp,
            View::Matte,
            View::Fg,
            View::Hint,
        ]
        .into_iter()
        .find(|v| index.view_frames(*v) > 0)
        .unwrap_or(View::Input);
        Self {
            ann: super::annotate::AnnState::new(),
            marks: vec![],
            overlay: Overlay::Off,
            sam_overlay: true,
            sam_available: false,
            mark_req: None,
            ui_rects: vec![],
            group_w: [0.0; 2],
            clip: clip_idx,
            index,
            store,
            view: first,
            ab: false,
            b_view: View::Input,
            wipe: 0.5,
            playhead: range.map_or(0, |r| r.0),
            clock: None,
            range,
            range_stored: range,
            linear: clip.input_is_linear,
            linear_stored: clip.input_is_linear,
            fps: clip.fps,
            fps_assumed: clip.fps_assumed,
            straight: processed_is_straight(clip),
            drops: 0,
            looping: true,
            zoom: 1.0,
            pan: Vec2::ZERO,
            buffering: None,
            preview_note: None,
            preview_sig: None,
            est_frame_bytes: clip
                .sample_size
                .map_or(0, |(w, h)| w as usize * h as usize * 4),
            tex: HashMap::new(),
            tex_order: vec![],
            held: None,
            draw_h: 0.0,
            fit_scale: 0.0,
            disp: DisplayOpts::default(),
            display_set: false,
            img_rect: Rect::NOTHING,
            readout: false,
        }
    }

    /// Source path of the Input frame at timeline index `i` (test/diagnostic helper).
    pub fn index_path(&self, i: usize) -> std::path::PathBuf {
        match self.index.availability(View::Input, i, None) {
            Availability::Present(p) => p,
            _ => std::path::PathBuf::new(),
        }
    }

    /// Adopt a re-read clip (job results appeared). The timeline is rebuilt, but playhead, In/Out (edited or
    /// stored), view, A/B, zoom and pan stay: positions are carried over by frame stem. Paths that newly
    /// exist are forgotten by the cache so earlier "missing"/failed verdicts cannot stick.
    pub fn refresh(&mut self, clip: &Clip) {
        let old = std::mem::replace(&mut self.index, FrameIndex::build(clip));
        let n = self.index.len();
        let remap = |i: usize| {
            old.stems
                .get(i)
                .and_then(|s| self.index.index_of(s))
                .unwrap_or(i.min(n.saturating_sub(1)))
        };
        self.playhead = remap(self.playhead);
        self.range = self.range.map(|(a, b)| (remap(a), remap(b)));
        self.range_stored = self.range_stored.map(|(a, b)| (remap(a), remap(b)));
        self.straight = processed_is_straight(clip);
        for v in View::ALL {
            for (i, stem) in self.index.stems.iter().enumerate() {
                // Hint/guide frames are rewritten in place by generation jobs: never keep their old pixels.
                if !old.view_has(v, stem) || matches!(v, View::Hint | View::Guide) {
                    if let Availability::Present(p) = self.index.availability(v, i, None) {
                        self.store.forget_path(&p);
                        self.tex.retain(|k, _| k.path != p);
                        self.tex_order.retain(|k| k.path != p);
                    }
                }
            }
        }
        // A running preview re-buffers on its own when the range/view signature changed.
    }

    /// Generated frames were deleted: forget their decoded pixels, failure records and textures (the paths are
    /// gone from the rebuilt index, so `refresh` can no longer find them), and the annotation overlay textures.
    pub fn forget_paths(&mut self, paths: &[std::path::PathBuf]) {
        for p in paths {
            self.store.forget_path(p);
            self.tex.retain(|k, _| k.path != *p);
            self.tex_order.retain(|k| k.path != *p);
        }
        self.ann.forget_masks();
    }

    pub fn range_dirty(&self) -> bool {
        self.range != self.range_stored
    }
    pub fn bounds(&self) -> (usize, usize) {
        self.range
            .unwrap_or((0, self.index.len().saturating_sub(1)))
    }

    fn key(&self, view: View, i: usize) -> Option<Key> {
        match self.index.availability(view, i, self.range) {
            Availability::Present(p) => Some(Key {
                interp: view_interp(view, &p, self.linear, self.straight),
                path: p,
            }),
            _ => None,
        }
    }

    pub fn set_playhead(&mut self, i: usize) {
        self.playhead = i.min(self.index.len().saturating_sub(1));
        self.clock = None;
        self.buffering = None;
    }
    /// Stop playback or RAM-preview buffering; playhead, In/Out and cache stay as they are.
    pub fn pause(&mut self) {
        self.clock = None;
        self.buffering = None;
        self.preview_sig = None;
    }
    /// Decode threads running in the frame loader, and its cache budget in bytes (tests).
    pub fn loader_config(&self) -> (usize, usize) {
        (self.store.workers(), self.store.cache.budget())
    }
    /// RAM preview progress `(frames in RAM, frames needed)` while Play waits for the buffer.
    pub fn buffering(&self) -> Option<(usize, usize)> {
        let dir = self.buffering?;
        let keys = self.preview_keys(dir);
        Some((self.done_count(&keys), keys.len()))
    }
    /// Why the frame under the playhead cannot be shown (decode failure), for the activity strip.
    pub fn decode_error(&mut self) -> Option<String> {
        let av = self.index.availability(self.view, self.playhead, self.range);
        let Availability::Present(path) = &av else { return None };
        let key = Key { interp: view_interp(self.view, path, self.linear, self.straight), path: path.clone() };
        match self.store.lookup(&key) {
            Lookup::Failed(e) => {
                let left = self.store.retries_left(&key);
                Some(format!("Decode failed: {e}{}", if left > 0 { format!(" · use Retry in Activity ({left} left)") } else { " · retries exhausted; reopen the clip".into() }))
            }
            _ => None,
        }
    }
    pub fn fit(&mut self) {
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }
    /// Zoom by factor `f` keeping the point `anchor` (offset from the viewport centre) fixed.
    pub fn zoom_by(&mut self, f: f32, anchor: Vec2) {
        let z = (self.zoom * f).clamp(1.0, MAX_ZOOM);
        let r = z / self.zoom;
        self.pan = anchor - (anchor - self.pan) * r;
        self.zoom = z;
        if z <= 1.0 {
            self.pan = Vec2::ZERO;
        }
    }
    pub fn step(&mut self, d: isize) {
        let n = self.index.len();
        if n > 0 {
            self.set_playhead(step(self.playhead, d, 0, n - 1));
        }
    }
    /// Play/pause. Play first fills RAM with the whole range (A and B views, current interpretation); the clock
    /// starts when it is complete, or at once, streaming, when the range cannot fit the budget.
    pub fn toggle_play(&mut self, dir: isize) {
        if self.clock.as_ref().is_some_and(|c| c.dir == dir) || self.buffering == Some(dir) {
            self.clock = None;
            self.buffering = None;
            self.preview_sig = None;
            return;
        }
        if self.index.is_empty() {
            return;
        }
        let (lo, hi) = self.bounds();
        // Restart from the edge if we are parked at the end we'd run into (or outside In/Out).
        let outside = self.playhead < lo || self.playhead > hi;
        if outside
            || (!self.looping
                && ((dir > 0 && self.playhead >= hi) || (dir < 0 && self.playhead <= lo)))
        {
            self.playhead = if dir > 0 { lo } else { hi };
        }
        self.clock = None;
        self.preview_note = None;
        self.drops = 0; // per run: a warm repeat starts from a clean count
        self.buffering = Some(dir);
        self.buffer_step(dir);
    }

    /// Loop button and L. Playback carries on from the current frame: the clock is rebased, so frames elapsed
    /// over earlier laps cannot make it jump to In/Out.
    pub fn toggle_loop(&mut self) {
        self.toggle_loop_at(Instant::now());
    }
    pub fn toggle_loop_at(&mut self, now: Instant) {
        self.looping = !self.looping;
        if let Some(c) = &mut self.clock {
            *c = Clock::start_at(c.fps, self.playhead, c.dir, now);
        }
    }

    fn sig(&self) -> PreviewSig {
        (
            self.view,
            self.ab.then_some(self.b_view),
            self.linear,
            self.straight,
            self.bounds(),
            self.looping, // non-loop buffers only the remaining direction: a toggle needs the whole range
        )
    }

    /// Cache keys a preview in direction `dir` needs: every present frame it will show, A first then B,
    /// nearest the playhead first, de-duplicated.
    fn preview_keys(&self, dir: isize) -> Vec<Key> {
        let (lo, hi) = self.bounds();
        let order: Vec<usize> = if self.looping {
            loop_span(
                self.playhead.clamp(lo, hi),
                wrap_frame(self.playhead as isize - dir, lo, hi),
                dir,
                lo,
                hi,
            )
        } else if dir > 0 {
            (self.playhead.clamp(lo, hi)..=hi).collect()
        } else {
            (lo..=self.playhead.clamp(lo, hi)).rev().collect()
        };
        let mut seen = HashSet::new();
        let mut keys = vec![];
        for i in order {
            for v in [Some(self.view), self.ab.then_some(self.b_view)]
                .into_iter()
                .flatten()
            {
                if let Some(k) = self.key(v, i).filter(|k| seen.insert(k.clone())) {
                    keys.push(k);
                }
            }
        }
        keys
    }

    /// Frames that will not change any more: in RAM, or failed to decode (reported, never waited on).
    fn done_count(&self, keys: &[Key]) -> usize {
        keys.iter()
            .filter(|k| self.store.cache.contains(k) || self.store.failed(k))
            .count()
    }

    fn buffer_step(&mut self, dir: isize) {
        // In/Out edited under a run may leave the playhead outside: start from the nearest valid frame.
        let (lo, hi) = self.bounds();
        self.playhead = self.playhead.clamp(lo, hi);
        let keys = self.preview_keys(dir);
        let done = self.done_count(&keys);
        let mut stream = None;
        if done < keys.len() {
            let (held, n) = self.store.cache.held(&keys);
            let fb = held.checked_div(n).unwrap_or(self.est_frame_bytes);
            let budget = self.store.cache.budget();
            if fb > 0 && (keys.len() * fb > budget || held + fb > budget) {
                stream = Some(format!(
                    "RAM preview needs {} for {} frames but the frame cache is {}: streaming, frames may drop. Narrow In/Out, close A/B, or raise --cache-mb.",
                    mb(keys.len() * fb),
                    keys.len(),
                    mb(budget)
                ));
            } else {
                // Size unknown: decode only the first frame, then decide.
                let first = keys.first().cloned().into_iter();
                let wanted = if fb == 0 && !keys.first().is_some_and(|k| self.store.failed(k)) {
                    first.collect()
                } else {
                    keys.clone()
                };
                if let Some(t) = wanted.first().cloned() {
                    self.store.request(t, wanted.into_iter().skip(1).collect());
                }
                return;
            }
        }
        self.buffering = None;
        let failed = keys.iter().filter(|k| self.store.failed(k)).count();
        self.preview_note = stream.or((failed > 0).then(|| {
            format!("{failed} of {} frames could not be loaded into RAM (see the frame error; Narrow In/Out or raise the RAM budget): playing without them.", keys.len())
        }));
        self.preview_sig = Some(self.sig());
        self.clock = Some(Clock::start(self.fps, self.playhead, dir));
    }
    pub fn set_in(&mut self) {
        let (_, hi) = self.bounds();
        let hi = if self.range.is_none() {
            self.index.len().saturating_sub(1)
        } else {
            hi
        };
        self.range = Some((self.playhead, hi.max(self.playhead)));
    }
    pub fn set_out(&mut self) {
        let (lo, _) = self.bounds();
        self.range = Some((lo.min(self.playhead), self.playhead));
    }
    pub fn clear_range(&mut self) {
        self.range = None;
    }

    fn ready(&mut self, i: usize) -> bool {
        let mut views = vec![self.view];
        if self.ab {
            views.push(self.b_view);
        }
        views.into_iter().all(|v| {
            // A frame that failed to decode is final too, or a failed Out/In frame would hold the clock forever.
            self.key(v, i)
                .is_none_or(|k| self.store.cache.contains(&k) || self.store.failed(&k))
        })
    }

    /// Advance playback and queue decodes. Call once per frame. Returns true while playing: the caller must
    /// keep scheduling repaints, because with every frame cached no decode completes to wake the UI.
    pub fn tick(&mut self) -> bool {
        self.store.poll();
        let n = self.index.len();
        if n == 0 {
            return false;
        }
        if let Some(c) = &self.clock {
            if self.preview_sig.is_some_and(|s| s != self.sig()) {
                // View, A/B, interpretation or range changed under a running preview: buffer again.
                let dir = c.dir;
                self.clock = None;
                self.buffering = Some(dir);
            }
        }
        if let Some(dir) = self.buffering {
            self.buffer_step(dir);
            if self.buffering.is_some() {
                return true;
            }
        }
        let mut wanted = self.playhead;
        if let Some(c) = &self.clock {
            let (lo, hi) = self.bounds();
            let dir = c.dir;
            let raw = c.frame_at(Instant::now());
            let target = if self.looping {
                wrap_frame(raw, lo, hi)
            } else {
                raw.clamp(lo as isize, hi as isize) as usize
            };
            wanted = target;
            // Newest ready frame between the playhead and the clock target, in play direction.
            let span: Vec<usize> = if self.looping {
                loop_span(self.playhead.clamp(lo, hi), target, dir, lo, hi)
            } else if dir > 0 {
                (self.playhead..=target).collect()
            } else {
                (target..=self.playhead).rev().collect()
            };
            let mut best = None;
            for (pos, i) in span.into_iter().enumerate() {
                if self.ready(i) {
                    best = Some((pos, i));
                }
            }
            if let Some((pos, b)) = best {
                if pos > 1 {
                    self.drops += pos as u64 - 1;
                }
                self.playhead = b;
            }
            if !self.looping && (raw < lo as isize || raw > hi as isize) && self.playhead == target
            {
                self.clock = None;
            }
        }
        let (dir, (lo, hi)) = (self.clock.as_ref().map_or(1, |c| c.dir), self.bounds());
        let mut keys = vec![];
        for v in [Some(self.view), self.ab.then_some(self.b_view)]
            .into_iter()
            .flatten()
        {
            for k in 0..=4isize {
                let j = wanted as isize + k * dir;
                let j = if self.looping && self.clock.is_some() {
                    wrap_frame(j, lo, hi) as isize
                } else {
                    j
                };
                if j >= 0 && (j as usize) < n {
                    keys.extend(self.key(v, j as usize));
                }
            }
        }
        if let Some(first) = keys.first().cloned() {
            self.store
                .request(first, keys.into_iter().skip(1).collect());
        }
        self.clock.is_some()
    }

    /// Explicit retry of the failed frame under the playhead (bounded by `MAX_RETRIES`).
    pub fn retry_failed(&mut self) -> bool {
        let Some(k) = self.key(self.view, self.playhead) else {
            return false;
        };
        self.store.retry(&k)
    }

    fn texture(&mut self, ctx: &egui::Context, key: &Key) -> Option<&Tex> {
        // Missing, or built with other display options: (re)build from the cached decoded frame. Runs once per
        // frame/option change, never per repaint.
        if self.tex.get(key).is_none_or(|t| t.opts != self.disp) {
            let f = match self.tex.get(key) {
                Some(t) => t.frame.clone(),
                None => {
                    let Lookup::Ready(f) = self.store.lookup(key) else {
                        return None;
                    };
                    f
                }
            };
            let img = display_image(&f, &self.disp);
            let handle = ctx.load_texture("frame", img, TextureOptions::LINEAR);
            let fresh = !self.tex.contains_key(key);
            self.tex.insert(
                key.clone(),
                Tex {
                    frame: f.clone(),
                    opts: self.disp,
                    handle,
                    size: [f.width, f.height],
                    has_alpha: f.has_alpha,
                    clipped: f.clipped_hdr,
                    info: f.info.clone(),
                },
            );
            if fresh {
                self.tex_order.push(key.clone());
            }
            while self.tex_order.len() > 6 {
                let old = self.tex_order.remove(0);
                if Some(&old) != self.held.as_ref() {
                    self.tex.remove(&old);
                } else {
                    self.tex_order.push(old);
                    break;
                }
            }
        }
        self.tex.get(key)
    }

    fn view_enabled(&self, v: View) -> Result<(), &'static str> {
        if self.index.view_frames(v) > 0 {
            Ok(())
        } else {
            Err(match v {
                View::Input => "No source frames",
                View::Hint => "No hint yet",
                View::Guide => "No guide yet",
                _ => "Output off / nothing on disk",
            })
        }
    }

    pub fn range_text(&self) -> String {
        let n = self.index.len();
        match self.range {
            Some((a, b)) => format!(
                "Range {}–{} · {} of {} frames",
                self.index.frame_number(a),
                self.index.frame_number(b),
                b - a + 1,
                n
            ),
            None => format!("Range: whole clip · {n} frames"),
        }
    }

    /// Only the image area in `rect`: backdrop, frame, A/B wipe, pan/zoom/wheel input, annotation and tint hooks,
    /// texture uploads. No toolbar, transport or timeline; does not advance `ui`'s cursor. Playback itself is
    /// driven by `tick()` (the caller keeps repainting while it returns true).
    /// Also draws the pixel readout (bottom-left) while the pointer is over the image.
    pub fn image_ui(&mut self, ui: &mut Ui, rect: Rect) -> egui::Response {
        let resp = ui.interact(rect, ui.scope_id().with("viewer_image"), Sense::click_and_drag());
        self.readout = true;
        self.image_body(ui, rect, &resp);
        if let Some((x, y, px)) = resp.hover_pos().and_then(|p| self.pixel_at(p)) {
            self.draw_readout(ui, rect, x, y, px);
        }
        resp
    }

    /// Toolbar choices (channel, gain, gamma, background, wipe direction). A change rebuilds the shown textures
    /// from their cached decoded frames on the next draw; the same options cost nothing.
    pub fn set_display(&mut self, o: DisplayOpts) {
        self.disp = o;
        self.display_set = true;
    }

    /// Source pixel under screen point `pos` and its decoded RGBA (before channel/gain/gamma); None outside the
    /// image or before a frame is drawn. With A/B on it reads the A frame.
    pub fn pixel_at(&self, pos: Pos2) -> Option<(u32, u32, [u8; 4])> {
        let t = self.held.as_ref().and_then(|k| self.tex.get(k))?;
        let r = self.img_rect;
        if !r.is_positive() || !r.contains(pos) {
            return None;
        }
        let x = (((pos.x - r.left()) / r.width() * t.size[0] as f32) as usize).min(t.size[0] - 1);
        let y = (((pos.y - r.top()) / r.height() * t.size[1] as f32) as usize).min(t.size[1] - 1);
        let i = (y * t.size[0] + x) * 4;
        let px = t.frame.rgba.get(i..i + 4)?;
        Some((x as u32, y as u32, [px[0], px[1], px[2], px[3]]))
    }

    /// (pixels below 0, pixels above 1) in the frame under `pixel_at`; None = unknown or an 8-bit source.
    pub fn out_of_range(&self) -> Option<(usize, usize)> {
        self.held.as_ref().and_then(|k| self.tex.get(k)).and_then(|t| t.frame.out_of_range)
    }

    fn draw_readout(&self, ui: &Ui, rect: Rect, x: u32, y: u32, px: [u8; 4]) {
        use egui::text::{LayoutJob, TextFormat};
        let t = super::tokens::tokens(ui);
        let font = super::type_scale::mono();
        let mut job = LayoutJob::default();
        let put = |job: &mut LayoutJob, label: &str, val: String| {
            job.append(label, 0.0, TextFormat::simple(font.clone(), t.text_tertiary));
            job.append(&format!(" {val}  "), 0.0, TextFormat::simple(font.clone(), t.text_primary));
        };
        put(&mut job, "x", x.to_string());
        put(&mut job, "y", y.to_string());
        job.append("  ", 0.0, TextFormat::simple(font.clone(), t.text_primary));
        let v = |i: usize| format!("{:.3}", px[i] as f32 / 255.0);
        let chans: &[(&str, usize)] = match self.disp.channel {
            Channel::R => &[("R", 0)],
            Channel::G => &[("G", 1)],
            Channel::B => &[("B", 2)],
            Channel::A => &[("A", 3)],
            _ => &[("R", 0), ("G", 1), ("B", 2), ("A", 3)],
        };
        for (l, i) in chans {
            put(&mut job, l, v(*i));
        }
        let p = ui.painter_at(rect);
        let galley = p.layout_job(job);
        let r = Rect::from_min_size(
            Pos2::new(rect.left() + 4.0, rect.bottom() - 4.0 - 20.0),
            Vec2::new(galley.size().x + 12.0, 20.0),
        );
        p.rect_filled(r, 0.0, t.bg_overlay.gamma_multiply(0.85));
        p.galley(Pos2::new(r.left() + 6.0, r.center().y - galley.size().y / 2.0), galley, t.text_primary);
    }

    fn image_body(&mut self, ui: &mut Ui, rect: Rect, resp: &egui::Response) {
        let ctx = ui.ctx().clone();
        ui.painter().rect_filled(rect, 2.0, Color32::from_gray(22));
        self.draw_image(&ctx, ui, rect, resp);
    }

    /// Decoded size of the frame on screen, in pixels.
    pub fn image_size(&self) -> Option<[usize; 2]> {
        self.held.as_ref().and_then(|k| self.tex.get(k)).map(|t| t.size)
    }
    /// Current zoom, 100 = one image pixel per screen point; None until a frame has been drawn.
    pub fn zoom_percent(&self) -> Option<f64> {
        (self.image_size().is_some() && self.fit_scale > 0.0).then(|| (self.fit_scale * self.zoom) as f64 * 100.0)
    }
    /// e.g. "EXR · half · 3840×2160"; "" while no frame is decoded.
    pub fn format_text(&self) -> String {
        self.held
            .as_ref()
            .and_then(|k| self.tex.get(k))
            .map(|t| format!("{} · {}×{}", t.info, t.size[0], t.size[1]))
            .unwrap_or_default()
    }
    /// Timeline length in frames; None for an empty clip.
    pub fn frame_count(&self) -> Option<i64> {
        (!self.index.is_empty()).then_some(self.index.len() as i64)
    }
    /// Frame numbers (from the file names) of the first and last timeline frame and of the playhead.
    pub fn first_frame(&self) -> i64 {
        self.index.frame_number(0) as i64
    }
    pub fn last_frame(&self) -> i64 {
        self.index.frame_number(self.index.len().saturating_sub(1)) as i64
    }
    pub fn current_frame(&self) -> i64 {
        self.index.frame_number(self.playhead) as i64
    }
    /// Playing or buffering forward / in reverse (buffering counts, as the pause icon does).
    pub fn playing(&self) -> bool {
        self.playing_dir() != 0
    }
    pub fn playing_reverse(&self) -> bool {
        self.playing_dir() < 0
    }
    fn playing_dir(&self) -> isize {
        self.clock.as_ref().map(|c| c.dir).or(self.buffering).unwrap_or(0)
    }
    pub fn is_looping(&self) -> bool {
        self.looping
    }
    pub fn go_first(&mut self) {
        self.set_playhead(0);
    }
    pub fn go_last(&mut self) {
        self.set_playhead(self.index.len().saturating_sub(1));
    }
    /// 100 % (one image pixel per point); never below Fit, which is the smallest zoom the viewer allows.
    pub fn one_to_one(&mut self) {
        self.set_zoom_percent(100.0);
    }
    pub fn set_zoom_percent(&mut self, p: f64) {
        if self.fit_scale > 0.0 {
            self.zoom = (p as f32 / 100.0 / self.fit_scale).clamp(1.0, MAX_ZOOM);
            if self.zoom <= 1.0 {
                self.pan = Vec2::ZERO;
            }
        }
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let full = ui.available_rect_before_wrap();
        let n = self.index.len();

        // ---- top bar: info (left) · A/B wipe + overlay (centre) · mode chips (right). Narrow windows move the
        // centre group to its own centred row (and the overlay chips to a third when even that is too wide), so
        // nothing can overlap the chips or the info text.
        // Measured last frame: the chips grow with the font and the enabled views, so a fixed width overlapped.
        let chips_w = self.ui_rects.iter().find(|(k, _)| *k == "chips").map_or(CHIPS_W, |(_, r)| r.width().max(CHIPS_W));
        let ab_w = (if self.ab { 84.0_f32 + 6.0 + 85.0 } else { 84.0 }).max(self.group_w[0]);
        let ov_w = (36.0_f32 + 72.0 + 42.0 + 44.0 + 3.0 * 4.0).max(self.group_w[1]);
        let both_w = ab_w + 12.0 + ov_w;
        self.ui_rects.clear();
        // The mode chips on the right are about 400 wide; the info text on the left gets 170 when shown.
        let info_w = if full.width() >= 640.0 { 170.0 } else { 0.0 };
        let one_row = full.width() >= info_w + both_w + 8.0 + chips_w;
        let split = !one_row && full.width() < both_w + 8.0;
        let row = |i: f32| Rect::from_min_size(full.min + Vec2::new(0.0, 30.0 * i), Vec2::new(full.width(), 30.0));
        let bar = row(0.0);
        let (ab_rect, ov_rect) = if split {
            (
                Rect::from_center_size(row(1.0).center(), Vec2::new(ab_w, 30.0)),
                Rect::from_center_size(row(2.0).center(), Vec2::new(ov_w, 30.0)),
            )
        } else {
            let c = if one_row { bar.center() } else { row(1.0).center() };
            let mut l = c.x - both_w / 2.0;
            if one_row {
                l = l.clamp(full.left() + info_w, full.right() - chips_w - 8.0 - both_w);
            }
            (
                Rect::from_min_size(Pos2::new(l, c.y - 15.0), Vec2::new(ab_w, 30.0)),
                Rect::from_min_size(Pos2::new(l + ab_w + 12.0, c.y - 15.0), Vec2::new(ov_w, 30.0)),
            )
        };
        let mut left = ui.new_child(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::left_to_right(Align::Center)),
        );
        let mut right = ui.new_child(
            UiBuilder::new()
                .max_rect(bar)
                .layout(Layout::right_to_left(Align::Center)),
        );
        let mut mid = ui.new_child(
            UiBuilder::new()
                .max_rect(ab_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        mid.spacing_mut().item_spacing.x = 6.0;
        let mut ovr = ui.new_child(
            UiBuilder::new()
                .max_rect(ov_rect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        ovr.spacing_mut().item_spacing.x = 4.0;
        let (mut view, mut ab, mut bv) = (self.view, self.ab, self.b_view);
        for v in View::ALL.into_iter().rev() {
            let en = self.view_enabled(v);
            let r = right.add_enabled(
                en.is_ok(),
                egui::Button::selectable(view == v, v.label()).min_size(Vec2::new(0.0, 24.0)),
            );
            let r = if en.is_ok() { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
            if r.clicked() {
                view = v;
            }
            if let Err(why) = en {
                r.on_disabled_hover_text(why);
            }
        }
        self.ui_rects.push(("chips", right.min_rect()));
        let r = mid.add(egui::Button::selectable(ab, "A/B wipe").min_size(Vec2::new(84.0, 24.0))).on_hover_cursor(egui::CursorIcon::PointingHand);
        self.ui_rects.push(("ab", r.rect));
        if r.clicked() {
            ab = !ab;
        }
        if ab {
            egui::ComboBox::from_id_salt("bview")
                .selected_text(format!("B: {}", bv.label()))
                .width(80.0)
                .show_ui(&mut mid, |ui| {
                    for v in View::ALL {
                        if self.index.view_frames(v) > 0 {
                            ui.selectable_value(&mut bv, v, v.label());
                        }
                    }
                });
        }
        for (o, l, w, name) in [
            (Overlay::Off, "Off", 36.0, "overlay_off"),
            (Overlay::Guide, "Pre-matte", 72.0, "overlay_guide"),
            (Overlay::Hint, "Hint", 42.0, "overlay_hint"),
        ] {
            let r = ovr
                .add(egui::Button::selectable(self.overlay == o, l).min_size(Vec2::new(w, 24.0)))
                .on_hover_cursor(egui::CursorIcon::PointingHand)
                .on_hover_text("Tint this mask over the Input frame");
            self.ui_rects.push((name, r.rect));
            if r.clicked() {
                self.overlay = o;
            }
        }
        let on = self.sam_overlay && self.sam_available;
        let r = ovr
            .add_enabled(self.sam_available, egui::Button::selectable(on, "SAM").min_size(Vec2::new(44.0, 24.0)))
            .on_hover_text("Show the SAM prompts and preview on the frame")
            .on_disabled_hover_text("The pre-matte producer is not SAM");
        let r = if self.sam_available { r.on_hover_cursor(egui::CursorIcon::PointingHand) } else { r };
        self.ui_rects.push(("sam", r.rect));
        if r.clicked() {
            self.sam_overlay = !self.sam_overlay;
        }
        self.group_w = [mid.min_rect().width(), ovr.min_rect().width()];
        let info = self
            .held
            .as_ref()
            .and_then(|k| self.tex.get(k))
            .map(|t| format!("{} · {}×{}", t.info, t.size[0], t.size[1]))
            .unwrap_or_default();
        // The format/size text would run under the chips when the viewer is squeezed (Properties sheet open).
        if full.width() >= 640.0 {
            left.add_space(4.0);
            left.label(egui::RichText::new(info).color(theme::text_dim(ui)).size(12.0));
        }
        (self.view, self.ab, self.b_view) = (view, ab, bv);
        ui.advance_cursor_after_rect(if one_row { bar } else if split { bar.union(row(2.0)) } else { bar.union(row(1.0)) });

        // ---- image area
        // Wide enough that Set In/Out sit on the icon row; then the mark buttons sit on its left and the marks list
        // shares the status line. Narrower: a compact second transport row and a marks row of their own.
        let inline_w = full.width() / 2.0 + ICONS_W / 2.0 + 16.0 + RANGE_W <= full.width();
        let bottom_h = 36.0 + 38.0 + 22.0 + 6.0 + if inline_w { 0.0 } else { 30.0 + 28.0 };
        let img_h = (full.bottom() - ui.cursor().top() - bottom_h).max(56.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(full.width(), img_h), Sense::click_and_drag());
        self.image_body(ui, rect, &resp);

        // ---- transport: the six icons are centred on the viewer by themselves; Set In / Set Out / Clear
        // live to their right on the same row when there is room, otherwise on a compact second row.
        ui.add_space(4.0);
        let (icons_w, range_w) = (ICONS_W, RANGE_W);
        let trow = ui.allocate_space(Vec2::new(full.width(), 30.0)).1;
        let crect = Rect::from_center_size(trow.center(), Vec2::new(icons_w, 30.0));
        let mut t = ui.new_child(
            UiBuilder::new()
                .max_rect(crect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        t.spacing_mut().item_spacing.x = 4.0;
        // Buffering shows the active pause icon too, so cancelling is discoverable.
        let playing_fwd =
            self.clock.as_ref().is_some_and(|c| c.dir > 0) || self.buffering == Some(1);
        let playing_rev =
            self.clock.as_ref().is_some_and(|c| c.dir < 0) || self.buffering == Some(-1);
        if icon_btn(&mut t, 0, false, "Go to start (Home)").clicked() {
            self.set_playhead(0);
        }
        if icon_btn(&mut t, 1, false, "Step back (Left)").clicked() {
            self.step(-1);
        }
        if icon_btn(
            &mut t,
            if playing_rev { 6 } else { 2 },
            playing_rev,
            "Play reverse",
        )
        .clicked()
        {
            self.toggle_play(-1);
        }
        if icon_btn(
            &mut t,
            if playing_fwd { 6 } else { 3 },
            playing_fwd,
            "Play / pause (Space)",
        )
        .clicked()
        {
            self.toggle_play(1);
        }
        if icon_btn(&mut t, 4, false, "Step forward (Right)").clicked() {
            self.step(1);
        }
        if icon_btn(&mut t, 5, false, "Go to end (End)").clicked() {
            self.set_playhead(n.saturating_sub(1));
        }
        let inline = crect.right() + 16.0 + range_w <= full.right();
        debug_assert_eq!(inline, inline_w);
        if inline {
            let area = Rect::from_min_max(trow.left_top(), Pos2::new(crect.left() - 8.0, trow.bottom()));
            let mut t = ui.new_child(UiBuilder::new().max_rect(area).layout(Layout::left_to_right(Align::Center)));
            t.spacing_mut().item_spacing.x = 4.0;
            self.mark_buttons(&mut t);
        }
        let rrect = if inline {
            Rect::from_min_size(
                Pos2::new(crect.right() + 16.0, trow.top()),
                Vec2::new(range_w, 30.0),
            )
        } else {
            ui.add_space(2.0);
            let r2 = ui.allocate_space(Vec2::new(full.width(), 28.0)).1;
            Rect::from_center_size(r2.center(), Vec2::new(range_w, 28.0))
        };
        let mut t = ui.new_child(
            UiBuilder::new()
                .max_rect(rrect)
                .layout(Layout::left_to_right(Align::Center)),
        );
        t.spacing_mut().item_spacing.x = 4.0;
        if t.add(egui::Button::new("Set In (I)").min_size(Vec2::new(92.0, 26.0)))
            .clicked()
        {
            self.set_in();
        }
        if t.add(egui::Button::new("Set Out (O)").min_size(Vec2::new(100.0, 26.0)))
            .clicked()
        {
            self.set_out();
        }
        if t.add_enabled(
            self.range.is_some(),
            egui::Button::new("Clear In/Out").min_size(Vec2::new(104.0, 26.0)),
        )
        .clicked()
        {
            self.clear_range();
        }
        if t.add(egui::Button::selectable(self.looping, "Loop (L)").min_size(Vec2::new(78.0, 26.0)))
            .on_hover_text("Wrap around at In/Out in both directions")
            .clicked()
        {
            self.toggle_loop();
        }

        // ---- timeline
        ui.add_space(4.0);
        self.timeline(ui, full.width());
        if !inline {
            let r1 = ui.allocate_space(Vec2::new(full.width(), 26.0)).1;
            let mut t = ui.new_child(UiBuilder::new().max_rect(r1).layout(Layout::left_to_right(Align::Center)));
            t.spacing_mut().item_spacing.x = 4.0;
            self.mark_buttons(&mut t);
            let area = Rect::from_min_max(Pos2::new(t.min_rect().right() + 8.0, r1.top()), r1.right_bottom());
            self.marks_list(ui, area, None);
        }

        // ---- status line
        let mut s = format!(
            "Frame {} ({}/{})  ·  {}",
            self.index.frame_number(self.playhead),
            self.playhead + 1,
            n,
            self.range_text()
        );
        if self.range_dirty() {
            s += "  ·  In/Out edited in memory — NOT saved to the project";
        }
        if self.fps_assumed {
            s += "  ·  fps assumed 24 (no metadata)";
        } else {
            s += &format!("  ·  {:.3} fps", self.fps);
        }
        if self.clock.is_some() || self.drops > 0 {
            s += &format!("  ·  dropped {}", self.drops);
        }
        if inline {
            let area = ui.allocate_space(Vec2::new(full.width(), 15.0)).1;
            self.marks_list(ui, area, Some(&s));
        } else {
            let s = format!("{s}  ·  {}", self.marks_text());
            ui.label(egui::RichText::new(s).size(12.0).color(theme::text_dim(ui)));
        }
    }

    fn draw_image(&mut self, ctx: &egui::Context, ui: &mut Ui, rect: Rect, resp: &egui::Response) {
        self.draw_h = rect.height();
        self.img_rect = Rect::NOTHING;
        let p = ui.painter_at(rect);
        let msg = |text: &str, c: Color32| {
            p.text(
                rect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(16.0),
                c,
            );
        };
        if self.index.is_empty() {
            return msg("No frames found for this clip", WARN);
        }
        let i = self.playhead;
        let av = self.index.availability(self.view, i, self.range);
        let key_a = match &av {
            Availability::Present(path) => Some(Key {
                interp: view_interp(self.view, path, self.linear, self.straight),
                path: path.clone(),
            }),
            Availability::NoFile => return msg("No file on disk", WARN),
            Availability::OutsideRange => return msg("Outside In/Out", DIM),
            Availability::ViewEmpty => {
                return msg(
                    &format!("{}: nothing on disk for this clip", self.view.label()),
                    WARN,
                )
            }
        };
        let key_a = key_a.unwrap();
        let key_b = if self.ab {
            self.key(self.b_view, i)
        } else {
            None
        };
        // Decode failure is reported, never hidden behind a stale frame.
        if let Lookup::Failed(_) = self.store.lookup(&key_a) {
            // The reason (and the retry hint) is in the activity strip.
            return msg("Frame could not be decoded (see the activity strip)", Color32::from_rgb(220, 100, 100));
        }
        let a_ready = self.texture(ctx, &key_a).is_some();
        if a_ready {
            self.held = Some(key_a.clone());
        }
        let show_key = if a_ready {
            key_a.clone()
        } else {
            match self.held.clone() {
                Some(h) => h,
                None => return msg("Decoding…", DIM),
            }
        };
        let _ = self.texture(ctx, &show_key);
        let b_tex = key_b.as_ref().filter(|_| a_ready).and_then(|k| {
            let _ = self.texture(ctx, k);
            self.tex
                .get(k)
                .map(|t| (t.handle.id(), t.size, t.has_alpha))
        });
        let Some(a) = self.tex.get(&show_key) else {
            return;
        };
        let (aid, asz, aalpha, aclip) = (a.handle.id(), a.size, a.has_alpha, a.clipped);
        // Fit to the frame's own aspect (never resampled into another one), then zoom/pan on top.
        // Wheel zooms about the cursor, middle-drag pans; primary-drag pans too unless it moves the A/B wipe.
        let wipe_active = self.ab; // A/B on: primary is the wipe even while B still decodes
        if resp.hovered() {
            let (scroll, pinch, at) = ctx.input(|i| {
                (
                    i.smooth_scroll_delta.y,
                    i.zoom_delta(),
                    i.pointer.hover_pos(),
                )
            });
            let f = pinch * (scroll * 0.003).exp();
            if f != 1.0 {
                self.zoom_by(f, at.map_or(Vec2::ZERO, |p| p - rect.center()));
            }
        }
        if resp.dragged_by(egui::PointerButton::Middle)
            || (!wipe_active && !self.ann.active() && resp.dragged_by(egui::PointerButton::Primary))
        {
            self.pan += resp.drag_delta();
        }
        let fit = (rect.width() / asz[0] as f32).min(rect.height() / asz[1] as f32);
        self.fit_scale = fit;
        let size = Vec2::new(asz[0] as f32, asz[1] as f32) * fit * self.zoom;
        // Keep at least 64 pt of the image inside the viewport; at fit there is nothing to pan.
        let lim = ((size + rect.size()) / 2.0 - Vec2::splat(64.0)).max(Vec2::ZERO);
        self.pan = if self.zoom <= 1.0 {
            Vec2::ZERO
        } else {
            self.pan.clamp(-lim, lim)
        };
        let img = Rect::from_center_size(rect.center() + self.pan, size);
        self.img_rect = img;
        let bg = self.disp.background;
        if (aalpha || b_tex.is_some_and(|b| b.2)) && bg != Background::Checker {
            let fill = match bg {
                Background::Black => Color32::from_gray(0),
                Background::Grey => super::tokens::tokens(&*ui).viewer_surround,
                _ => Color32::from_gray(255),
            };
            p.rect_filled(img.intersect(rect), 0.0, fill);
        } else if aalpha || b_tex.is_some_and(|b| b.2) {
            // Only the cells inside the viewport: a deep zoom must not walk a million off-screen cells.
            let (cell, c_even, c_odd) = if self.display_set {
                (8.0, Color32::from_gray(0x66), Color32::from_gray(0x99))
            } else {
                (14.0, Color32::from_gray(70), Color32::from_gray(96))
            };
            let vis = img.intersect(rect);
            if vis.is_positive() {
                let c0 = ((vis.left() - img.left()) / cell).floor() as i32;
                let r0 = ((vis.top() - img.top()) / cell).floor() as i32;
                let (mut y, mut r) = (img.top() + r0 as f32 * cell, r0);
                while y < vis.bottom() {
                    let (mut x, mut c) = (img.left() + c0 as f32 * cell, c0);
                    while x < vis.right() {
                        let col = if (r + c) % 2 == 0 { c_even } else { c_odd };
                        p.rect_filled(
                            Rect::from_min_max(
                                Pos2::new(x.max(vis.left()), y.max(vis.top())),
                                Pos2::new(
                                    (x + cell).min(vis.right()),
                                    (y + cell).min(vis.bottom()),
                                ),
                            ),
                            0.0,
                            col,
                        );
                        x += cell;
                        c += 1;
                    }
                    y += cell;
                    r += 1;
                }
            }
        }
        let full_uv = Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0));
        let mut notices: Vec<String> = vec![];
        if let Some((bid, bsz, _)) = b_tex {
            let split = (self.wipe).clamp(0.0, 1.0);
            let horiz = self.disp.wipe_horizontal;
            // Vertical wipe: A left of the line, B right. Horizontal: A above, B below (same fraction, y axis).
            let (a_r, a_uv, b_r, b_uv, line) = if horiz {
                let sy = img.top() + img.height() * split;
                (
                    Rect::from_min_max(img.min, Pos2::new(img.right(), sy)),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, split)),
                    Rect::from_min_max(Pos2::new(img.left(), sy), img.max),
                    Rect::from_min_max(Pos2::new(0.0, split), Pos2::new(1.0, 1.0)),
                    [Pos2::new(img.left(), sy), Pos2::new(img.right(), sy)],
                )
            } else {
                let sx = img.left() + img.width() * split;
                (
                    Rect::from_min_max(img.min, Pos2::new(sx, img.bottom())),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(split, 1.0)),
                    Rect::from_min_max(Pos2::new(sx, img.top()), img.max),
                    Rect::from_min_max(Pos2::new(split, 0.0), Pos2::new(1.0, 1.0)),
                    [Pos2::new(sx, img.top()), Pos2::new(sx, img.bottom())],
                )
            };
            p.image(aid, a_r, a_uv, Color32::WHITE);
            p.image(bid, b_r, b_uv, Color32::WHITE);
            p.line_segment(line, Stroke::new(2.0, ACCENT));
            p.text(
                img.left_top() + Vec2::new(6.0, 4.0),
                Align2::LEFT_TOP,
                format!("A {}", self.view.label()),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            p.text(
                if self.disp.wipe_horizontal { img.left_bottom() + Vec2::new(6.0, -4.0) } else { img.right_top() + Vec2::new(-6.0, 4.0) },
                if self.disp.wipe_horizontal { Align2::LEFT_BOTTOM } else { Align2::RIGHT_TOP },
                format!("B {}", self.b_view.label()),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            if (resp.dragged_by(egui::PointerButton::Primary) || resp.clicked())
                && img.width() > 0.0
            {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.wipe = if self.disp.wipe_horizontal {
                        (pos.y - img.top()) / img.height()
                    } else {
                        (pos.x - img.left()) / img.width()
                    }
                    .clamp(0.0, 1.0);
                }
            }
            if bsz != asz {
                notices.push(format!(
                    "Size differs: A {}×{}, B {}×{} (B is stretched into A's frame)",
                    asz[0], asz[1], bsz[0], bsz[1]
                ));
            }
        } else {
            p.image(aid, img, full_uv, Color32::WHITE);
            if self.ab {
                notices.push(format!("B ({}) has no frame here", self.b_view.label()));
            }
        }
        // Annotation overlay/tools live on the Input view only (source pixels = that frame's pixels).
        if self.view == View::Input && !wipe_active {
            self.ann.run(ctx, &p, resp, img, asz);
        }
        if !a_ready {
            notices.push("Decoding… (showing previous frame)".into());
        }
        if let Some(n) = &self.preview_note {
            notices.push(n.clone());
        }
        if self.zoom > 1.0 {
            notices.push(format!("Zoom {:.0}% · 0 fits", fit * self.zoom * 100.0));
        }
        if aclip {
            notices.push("HDR clipped display".into());
        }
        if aalpha {
            notices.push(format!(
                "Alpha shown over {}",
                match bg {
                    Background::Checker => "checker",
                    Background::Black => "black",
                    Background::Grey => "grey",
                    Background::White => "white",
                }
            ));
        }
        if self.view == View::Input && self.linear {
            notices.push("Input interpreted as Linear, sRGB view".into());
        }
        for (k, n) in notices.iter().enumerate() {
            p.text(
                rect.left_bottom() + Vec2::new(8.0, -6.0 - 15.0 * k as f32 - if self.readout { 24.0 } else { 0.0 }),
                Align2::LEFT_BOTTOM,
                n,
                FontId::proportional(12.0),
                WARN,
            );
        }
        // RAM-preview buffering progress is shown in the activity strip.
    }

    /// The current frame's name and how many marks are inside In/Out.
    fn marks_text(&self) -> String {
        let count = self.marks.iter().filter(|m| self.range.is_none_or(|(a, b)| (a..=b).contains(*m))).count();
        format!("{} · {count} marked within In/Out", self.index.stems.get(self.playhead).map_or("", String::as_str))
    }

    /// Mark / Unmark / Clear marks. Pressing one sets `mark_req`; the Hint page saves it.
    fn mark_buttons(&mut self, t: &mut Ui) {
        let here = self.marks.contains(&self.playhead);
        for (label, name, enabled, tip, req) in [
            ("Mark frame", "mark", !self.index.is_empty() && !here, "Mark this frame", MarkReq::Mark),
            ("Unmark", "unmark", here, "Remove the mark from this frame", MarkReq::Unmark),
            ("Clear marks", "clear_marks", !self.marks.is_empty(), "Removes only the marks, never hint frames", MarkReq::Clear),
        ] {
            let r = t.add_enabled(enabled, egui::Button::new(label).min_size(Vec2::new(0.0, 22.0))).on_hover_text(tip);
            self.ui_rects.push((name, r.rect));
            if r.clicked() {
                self.mark_req = Some(req);
            }
        }
    }

    /// In `area`: the current frame, how many marks are inside In/Out, and the marks themselves (click one to go
    /// there). `lead` is text drawn first (the status line). Marks outside In/Out stay listed, dimmed.
    fn marks_list(&mut self, ui: &mut Ui, area: Rect, lead: Option<&str>) {
        let inside = |m: usize| self.range.is_none_or(|(a, b)| (a..=b).contains(&m));
        let mut room = area;
        if let Some(lead) = lead {
            let text = format!("{lead}  ·  {}", self.marks_text());
            let mut row = ui.new_child(UiBuilder::new().max_rect(area).layout(Layout::left_to_right(Align::Center)));
            let l = row.label(egui::RichText::new(text).color(theme::text_dim(ui)).size(12.0));
            self.ui_rects.push(("marks_text", l.rect));
            room = Rect::from_min_max(Pos2::new(l.rect.right() + 8.0, area.top()), area.right_bottom());
        }
        if room.width() > 24.0 && !self.marks.is_empty() {
            let mut goto = None;
            let mut list = ui.new_child(UiBuilder::new().max_rect(room).layout(Layout::left_to_right(Align::Center)));
            egui::ScrollArea::horizontal().id_salt("marks_list").show(&mut list, |ui| {
                ui.spacing_mut().item_spacing.x = 3.0;
                for m in self.marks.clone() {
                    let b = egui::Button::selectable(self.playhead == m, self.index.frame_number(m).to_string())
                        .min_size(Vec2::new(0.0, 20.0));
                    let b = if inside(m) { b } else { b.fill(ui.visuals().extreme_bg_color) };
                    if ui.add(b).on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text("Go to this marked frame").clicked() {
                        goto = Some(m);
                    }
                }
            });
            if let Some(m) = goto {
                self.set_playhead(m);
            }
        }
    }

    fn timeline(&mut self, ui: &mut Ui, width: f32) {
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click_and_drag());
        self.timeline_body(ui, rect, &resp);
    }

    /// Only the frame timeline (track, In/Out band, marks, playhead, scrubbing) in `rect`; the caller owns the
    /// layout and draws the transport and properties itself. Does not advance `ui`'s cursor.
    pub fn timeline_ui(&mut self, ui: &mut Ui, rect: Rect) {
        let resp = ui.interact(rect, ui.scope_id().with("viewer_timeline"), Sense::click_and_drag());
        self.timeline_body(ui, rect, &resp);
    }

    fn timeline_body(&mut self, ui: &mut Ui, rect: Rect, resp: &egui::Response) {
        let n = self.index.len();
        let p = ui.painter_at(rect);
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 6.0, rect.top() + 14.0),
            Pos2::new(rect.right() - 6.0, rect.bottom() - 4.0),
        );
        let dark = ui.visuals().dark_mode;
        p.rect_filled(track, 2.0, if dark { Color32::from_gray(38) } else { Color32::from_rgb(208, 215, 209) });
        p.rect_stroke(track, 2.0, Stroke::new(1.0, ui.visuals().widgets.inactive.bg_stroke.color), egui::StrokeKind::Inside);
        if n == 0 {
            return;
        }
        let x_of = |i: usize| {
            track.left()
                + track.width()
                    * if n > 1 {
                        i as f32 / (n - 1) as f32
                    } else {
                        0.0
                    }
        };
        if let Some((a, b)) = self.range {
            p.rect_filled(
                Rect::from_min_max(
                    Pos2::new(x_of(a), track.top()),
                    Pos2::new(x_of(b).max(x_of(a) + 2.0), track.bottom()),
                ),
                0.0,
                theme::accent_mark(ui).linear_multiply(0.45),
            );
        }
        for m in &self.marks {
            p.line_segment(
                [
                    Pos2::new(x_of(*m), track.top() - 4.0),
                    Pos2::new(x_of(*m), track.bottom()),
                ],
                Stroke::new(2.0, theme::ok(ui)),
            );
        }
        // Holes in the current view: thin dark ticks, so missing frames stay visible.
        for i in 0..n {
            if matches!(
                self.index.availability(self.view, i, None),
                Availability::NoFile
            ) {
                p.line_segment(
                    [
                        Pos2::new(x_of(i), track.top()),
                        Pos2::new(x_of(i), track.bottom()),
                    ],
                    Stroke::new(1.0, theme::error(ui)),
                );
            }
        }
        let label = |x: f32, i: usize, a: Align2| {
            p.text(
                Pos2::new(x, rect.top() + 1.0),
                a,
                self.index.frame_number(i).to_string(),
                FontId::monospace(11.0),
                theme::text_dim(ui),
            )
        };
        label(track.left(), 0, Align2::LEFT_TOP);
        label(track.right(), n - 1, Align2::RIGHT_TOP);
        if let Some((a, b)) = self.range {
            if a > 0 {
                label(x_of(a), a, Align2::CENTER_TOP);
            }
            if b + 1 < n {
                label(x_of(b), b, Align2::CENTER_TOP);
            }
        }
        let px = x_of(self.playhead);
        p.line_segment(
            [
                Pos2::new(px, rect.top() + 12.0),
                Pos2::new(px, rect.bottom()),
            ],
            Stroke::new(2.0, theme::warn(ui)),
        );
        p.circle_filled(
            Pos2::new(px, rect.top() + 12.0),
            3.5,
            theme::warn(ui),
        );
        if resp.dragged() || resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
                self.set_playhead((f * (n - 1) as f32).round() as usize);
            }
        }
    }
}
