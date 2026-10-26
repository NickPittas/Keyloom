//! The one shared viewer: mode chips (top-right), A/B wipe (top-centre), icon transport (centred below),
//! scrubbable timeline with In/Out band. One instance per open clip, shared by all workspace tabs.
use crate::frames::*;
use crate::project::{Clip, View};
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
/// View, B view, input reading, alpha convention, In/Out bounds, looping.
type PreviewSig = (View, Option<View>, bool, bool, (usize, usize), bool);

fn mb(bytes: usize) -> String {
    format!("{:.1} GB", bytes as f64 / (1u64 << 30) as f64)
}

struct Tex {
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
}

fn icon_btn(ui: &mut Ui, kind: u8, active: bool, tip: &str) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(34.0, 28.0), Sense::click());
    let p = ui.painter();
    use super::theme as th;
    let (bg, edge) = if active {
        (th::CHECKED, th::BORDER_HOVER)
    } else if resp.hovered() {
        (th::BUTTON_HOVER, th::BORDER_HOVER)
    } else {
        (th::BUTTON, th::BORDER)
    };
    p.rect_filled(rect, 3.0, bg);
    p.rect_stroke(rect, 3.0, Stroke::new(1.0, edge), egui::StrokeKind::Inside);
    if resp.has_focus() {
        p.rect_stroke(
            rect.expand(1.0),
            3.0,
            Stroke::new(2.0, th::FOCUS),
            egui::StrokeKind::Outside,
        );
    }
    let c = th::TEXT;
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
    resp.on_hover_text(tip)
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
        if !self.tex.contains_key(key) {
            let Lookup::Ready(f) = self.store.lookup(key) else {
                return None;
            };
            let img = if f.has_alpha {
                egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba)
            } else {
                egui::ColorImage::from_rgba_premultiplied([f.width, f.height], &f.rgba)
            };
            let handle = ctx.load_texture("frame", img, TextureOptions::LINEAR);
            self.tex.insert(
                key.clone(),
                Tex {
                    handle,
                    size: [f.width, f.height],
                    has_alpha: f.has_alpha,
                    clipped: f.clipped_hdr,
                    info: f.info.clone(),
                },
            );
            self.tex_order.push(key.clone());
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

    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let full = ui.available_rect_before_wrap();
        let n = self.index.len();

        // ---- top bar: info (left) · A/B (centre) · mode chips (right). Narrow windows put A/B on its own
        // centred row so it can never overlap the chips or the info text.
        let two_rows = full.width() < 960.0;
        let bar = Rect::from_min_size(full.min, Vec2::new(full.width(), 30.0));
        let ab_bar = if two_rows {
            Rect::from_min_size(bar.left_bottom(), Vec2::new(full.width(), 30.0))
        } else {
            bar
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
        // Fixed widths (toggle 84 + gap 6 + combo 100) so the whole A/B group, not its left edge, is centred.
        let group_w = if self.ab { 84.0 + 6.0 + 85.0 } else { 84.0 };
        let cbar = Rect::from_center_size(ab_bar.center(), Vec2::new(group_w, 30.0));
        let mut mid = ui.new_child(
            UiBuilder::new()
                .max_rect(cbar)
                .layout(Layout::left_to_right(Align::Center)),
        );
        mid.spacing_mut().item_spacing.x = 6.0;
        let (mut view, mut ab, mut bv) = (self.view, self.ab, self.b_view);
        for v in View::ALL.into_iter().rev() {
            let en = self.view_enabled(v);
            let r = right.add_enabled(
                en.is_ok(),
                egui::Button::selectable(view == v, v.label()).min_size(Vec2::new(0.0, 24.0)),
            );
            if r.clicked() {
                view = v;
            }
            if let Err(why) = en {
                r.on_disabled_hover_text(why);
            }
        }
        if mid
            .add(egui::Button::selectable(ab, "A/B wipe").min_size(Vec2::new(84.0, 24.0)))
            .clicked()
        {
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
        let info = self
            .held
            .as_ref()
            .and_then(|k| self.tex.get(k))
            .map(|t| format!("{} · {}×{}", t.info, t.size[0], t.size[1]))
            .unwrap_or_default();
        // The format/size text would run under the chips when the viewer is squeezed (Properties sheet open).
        if full.width() >= 640.0 {
            left.add_space(4.0);
            left.label(egui::RichText::new(info).color(DIM).size(12.0));
        }
        (self.view, self.ab, self.b_view) = (view, ab, bv);
        ui.advance_cursor_after_rect(if two_rows { bar.union(ab_bar) } else { bar });

        // ---- image area
        let bottom_h = 36.0 + 38.0 + 22.0 + 6.0 + if full.width() < 864.0 { 30.0 } else { 0.0 };
        let img_h = (full.bottom() - ui.cursor().top() - bottom_h).max(120.0);
        let (rect, resp) =
            ui.allocate_exact_size(Vec2::new(full.width(), img_h), Sense::click_and_drag());
        ui.painter().rect_filled(rect, 2.0, Color32::from_gray(22));
        self.draw_image(&ctx, ui, rect, &resp);

        // ---- transport: the six icons are centred on the viewer by themselves; Set In / Set Out / Clear
        // live to their right on the same row when there is room, otherwise on a compact second row.
        ui.add_space(4.0);
        let icons_w = 34.0 * 6.0 + 5.0 * 4.0;
        let range_w = 92.0 + 100.0 + 104.0 + 78.0 + 3.0 * 4.0;
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
        ui.label(egui::RichText::new(s).size(12.0).color(DIM));
    }

    fn draw_image(&mut self, ctx: &egui::Context, ui: &mut Ui, rect: Rect, resp: &egui::Response) {
        self.draw_h = rect.height();
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
        if let Lookup::Failed(e) = self.store.lookup(&key_a) {
            let left = self.store.retries_left(&key_a);
            return msg(
                &format!(
                    "Decode failed: {e}{}",
                    if left > 0 {
                        format!("  ·  press R to retry ({left} left)")
                    } else {
                        "  ·  retries exhausted; reopen the clip".into()
                    }
                ),
                Color32::from_rgb(220, 100, 100),
            );
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
        let size = Vec2::new(asz[0] as f32, asz[1] as f32) * fit * self.zoom;
        // Keep at least 64 pt of the image inside the viewport; at fit there is nothing to pan.
        let lim = ((size + rect.size()) / 2.0 - Vec2::splat(64.0)).max(Vec2::ZERO);
        self.pan = if self.zoom <= 1.0 {
            Vec2::ZERO
        } else {
            self.pan.clamp(-lim, lim)
        };
        let img = Rect::from_center_size(rect.center() + self.pan, size);
        if aalpha || b_tex.is_some_and(|b| b.2) {
            // Only the cells inside the viewport: a deep zoom must not walk a million off-screen cells.
            let cell = 14.0;
            let vis = img.intersect(rect);
            if vis.is_positive() {
                let c0 = ((vis.left() - img.left()) / cell).floor() as i32;
                let r0 = ((vis.top() - img.top()) / cell).floor() as i32;
                let (mut y, mut r) = (img.top() + r0 as f32 * cell, r0);
                while y < vis.bottom() {
                    let (mut x, mut c) = (img.left() + c0 as f32 * cell, c0);
                    while x < vis.right() {
                        let col = if (r + c) % 2 == 0 {
                            Color32::from_gray(70)
                        } else {
                            Color32::from_gray(96)
                        };
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
            let sx = img.left() + img.width() * self.wipe;
            let split = (self.wipe).clamp(0.0, 1.0);
            p.image(
                aid,
                Rect::from_min_max(img.min, Pos2::new(sx, img.bottom())),
                Rect::from_min_max(Pos2::ZERO, Pos2::new(split, 1.0)),
                Color32::WHITE,
            );
            p.image(
                bid,
                Rect::from_min_max(Pos2::new(sx, img.top()), img.max),
                Rect::from_min_max(Pos2::new(split, 0.0), Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
            p.line_segment(
                [Pos2::new(sx, img.top()), Pos2::new(sx, img.bottom())],
                Stroke::new(2.0, ACCENT),
            );
            p.text(
                img.left_top() + Vec2::new(6.0, 4.0),
                Align2::LEFT_TOP,
                format!("A {}", self.view.label()),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            p.text(
                img.right_top() + Vec2::new(-6.0, 4.0),
                Align2::RIGHT_TOP,
                format!("B {}", self.b_view.label()),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
            if (resp.dragged_by(egui::PointerButton::Primary) || resp.clicked())
                && img.width() > 0.0
            {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.wipe = ((pos.x - img.left()) / img.width()).clamp(0.0, 1.0);
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
            notices.push("Alpha shown over checker".into());
        }
        if self.view == View::Input && self.linear {
            notices.push("Input interpreted as Linear, sRGB view".into());
        }
        for (k, n) in notices.iter().enumerate() {
            p.text(
                rect.left_bottom() + Vec2::new(8.0, -6.0 - 15.0 * k as f32),
                Align2::LEFT_BOTTOM,
                n,
                FontId::proportional(12.0),
                WARN,
            );
        }
        if let Some((done, total)) = self.buffering() {
            let bar = Rect::from_center_size(
                rect.center_bottom() - Vec2::new(0.0, 40.0),
                Vec2::new(280.0, 22.0),
            );
            p.rect_filled(bar.expand(6.0), 4.0, Color32::from_black_alpha(190));
            p.rect_filled(bar, 2.0, Color32::from_gray(50));
            let f = if total > 0 {
                done as f32 / total as f32
            } else {
                1.0
            };
            p.rect_filled(
                Rect::from_min_size(bar.min, Vec2::new(bar.width() * f, bar.height())),
                2.0,
                ACCENT,
            );
            p.text(
                bar.center(),
                Align2::CENTER_CENTER,
                format!("Buffering RAM preview {done}/{total} · Space cancels"),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
        }
    }

    fn timeline(&mut self, ui: &mut Ui, width: f32) {
        let n = self.index.len();
        let (rect, resp) = ui.allocate_exact_size(Vec2::new(width, 38.0), Sense::click_and_drag());
        let p = ui.painter_at(rect);
        let track = Rect::from_min_max(
            Pos2::new(rect.left() + 6.0, rect.top() + 14.0),
            Pos2::new(rect.right() - 6.0, rect.bottom() - 4.0),
        );
        p.rect_filled(track, 2.0, Color32::from_gray(38));
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
                ACCENT.linear_multiply(0.45),
            );
        }
        for m in &self.marks {
            p.line_segment(
                [
                    Pos2::new(x_of(*m), track.top() - 4.0),
                    Pos2::new(x_of(*m), track.bottom()),
                ],
                Stroke::new(2.0, Color32::from_rgb(120, 220, 140)),
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
                    Stroke::new(1.0, Color32::from_rgb(120, 60, 60)),
                );
            }
        }
        let label = |x: f32, i: usize, a: Align2| {
            p.text(
                Pos2::new(x, rect.top() + 1.0),
                a,
                self.index.frame_number(i).to_string(),
                FontId::monospace(11.0),
                DIM,
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
            Stroke::new(2.0, Color32::from_rgb(235, 200, 90)),
        );
        p.circle_filled(
            Pos2::new(px, rect.top() + 12.0),
            3.5,
            Color32::from_rgb(235, 200, 90),
        );
        if resp.dragged() || resp.clicked() {
            if let Some(pos) = resp.interact_pointer_pos() {
                let f = ((pos.x - track.left()) / track.width()).clamp(0.0, 1.0);
                self.set_playhead((f * (n - 1) as f32).round() as usize);
            }
        }
    }
}
