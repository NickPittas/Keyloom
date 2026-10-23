//! Board thumbnails. Each clip's first source frame is decoded through the same `decode_display` path the
//! viewer uses, off the UI thread, then box-downscaled once and kept as an egui texture. A path is decoded at
//! most once per session; failures are remembered so a broken frame is not retried every frame.
use crate::decode::{decode_display, DisplayFrame};
use crate::frames::{view_interp, Key, Loader};
use crate::project::Clip;
use crate::project::View;
use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// Thumbnail width in pixels; the card scales the texture to fit.
const W: usize = 256;

/// The frame a clip is represented by: its first source frame (the clip's own identity, not a result).
pub fn thumb_path(clip: &Clip) -> Option<PathBuf> {
    clip.input_files.first().map(|f| f.1.clone())
}

/// Box-filter `f` down to at most `W` pixels wide (aspect kept). Preview only: never used for measurements.
fn shrink(f: &DisplayFrame) -> DisplayFrame {
    if f.width <= W || f.height == 0 {
        return f.clone();
    }
    let h = (f.height * W / f.width).max(1);
    let (sx, sy) = (f.width as f32 / W as f32, f.height as f32 / h as f32);
    let mut rgba = Vec::with_capacity(W * h * 4);
    for y in 0..h {
        let y0 = (y as f32 * sy) as usize;
        let y1 = (((y + 1) as f32 * sy) as usize).min(f.height);
        for x in 0..W {
            let x0 = (x as f32 * sx) as usize;
            let x1 = (((x + 1) as f32 * sx) as usize).min(f.width);
            let (mut acc, mut n) = ([0u32; 4], 0u32);
            for yy in y0..y1.max(y0 + 1) {
                for xx in x0..x1.max(x0 + 1) {
                    let i = (yy * f.width + xx) * 4;
                    for (k, a) in acc.iter_mut().enumerate() {
                        *a += f.rgba[i + k] as u32;
                    }
                    n += 1;
                }
            }
            let n = n.max(1);
            rgba.extend(acc.iter().map(|a| (a / n) as u8));
        }
    }
    DisplayFrame {
        width: W,
        height: h,
        rgba,
        has_alpha: f.has_alpha,
        clipped_hdr: f.clipped_hdr,
        info: f.info.clone(),
    }
}

/// Identity of one source frame on disk: a re-extracted or replaced frame gets a new stamp, so its thumbnail
/// is decoded again instead of showing the old picture.
type Stamp = (PathBuf, u64, u64);

fn stamp(path: &PathBuf) -> Stamp {
    let (mut mtime, mut len) = (0u64, 0u64);
    if let Ok(m) = std::fs::metadata(path) {
        len = m.len();
        mtime = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_nanos() as u64);
    }
    (path.clone(), mtime, len)
}

pub struct Thumbs {
    loader: Loader,
    ready: HashMap<Stamp, Arc<DisplayFrame>>,
    tex: HashMap<Stamp, TextureHandle>,
    failed: HashMap<Stamp, String>,
}

impl Thumbs {
    pub fn new(ctx: &egui::Context) -> Self {
        let c = ctx.clone();
        let decode: crate::frames::DecodeFn =
            Arc::new(|k: &Key| decode_display(&k.path, k.interp).map(|f| shrink(&f)));
        Self {
            loader: Loader::new(2, decode, Some(Arc::new(move || c.request_repaint()))),
            ready: HashMap::new(),
            tex: HashMap::new(),
            failed: HashMap::new(),
        }
    }

    /// Put these clips' representative frames in the decode queue (first one first).
    pub fn request(&mut self, clips: &[&Clip]) {
        // Drop entries whose frame changed on disk before deciding what is still wanted.
        let live: Vec<Stamp> = clips
            .iter()
            .filter_map(|c| thumb_path(c).map(|p| stamp(&p)))
            .collect();
        self.tex.retain(|k, _| live.contains(k));
        self.ready.retain(|k, _| live.contains(k));
        self.failed.retain(|k, _| live.contains(k));
        let keys: Vec<Key> = clips
            .iter()
            .filter_map(|c| {
                let p = thumb_path(c)?;
                let st = stamp(&p);
                if self.tex.contains_key(&st) || self.failed.contains_key(&st) {
                    None
                } else {
                    Some(Key {
                        interp: view_interp(View::Input, &p, c.input_is_linear, false),
                        path: p,
                    })
                }
            })
            .collect();
        if !keys.is_empty() {
            self.loader.set_wanted(keys);
        }
    }

    /// Absorb finished decodes. Call once per frame before drawing cards.
    pub fn tick(&mut self) {
        while let Some(d) = self.loader.take() {
            let st = stamp(&d.key.path);
            match d.result {
                Ok(f) => {
                    self.ready.insert(st, f);
                }
                Err(e) => {
                    self.failed.insert(st, e);
                }
            }
        }
    }

    /// The texture for this clip's frame, once decoded. Uploads on first use.
    pub fn texture(&mut self, ctx: &egui::Context, clip: &Clip) -> Option<&TextureHandle> {
        let path = thumb_path(clip)?;
        let st = stamp(&path);
        if !self.tex.contains_key(&st) {
            let f = self.ready.remove(&st)?;
            let img = ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
            let t = ctx.load_texture(
                format!("thumb:{}:{}:{}", path.display(), st.1, st.2),
                img,
                TextureOptions::LINEAR,
            );
            self.tex.insert(st.clone(), t);
        }
        self.tex.get(&st)
    }

    /// Why this clip has no thumbnail (unreadable first frame), if that is known.
    pub fn failed(&self, clip: &Clip) -> Option<&str> {
        let path = thumb_path(clip)?;
        self.failed.get(&stamp(&path)).map(String::as_str)
    }
}
