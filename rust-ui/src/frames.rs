//! Frame identity (stem index per view), bounded cache, off-thread decode, and the playback clock.
use crate::decode::{decode_display, DisplayFrame, Interp};
use crate::natural_sort::natural_cmp;
use crate::project::{ext_of, list_images, Clip, View};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

/// Per-view stem → file maps and the unified timeline (natural-sorted union of stems).
#[derive(Debug, Default)]
pub struct FrameIndex {
    pub stems: Vec<String>,
    views: HashMap<View, HashMap<String, PathBuf>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Availability {
    Present(PathBuf),
    /// Frame is inside the timeline but this view has no file for the stem.
    NoFile,
    OutsideRange,
    ViewEmpty,
}

impl FrameIndex {
    pub fn build(clip: &Clip) -> Self {
        let mut views = HashMap::new();
        for v in View::ALL {
            // v3 manifest: its explicit files map is the truth (templated names do not match source stems).
            if let Some(f) = clip.manifest_view_files(v) {
                if !f.is_empty() {
                    views.insert(v, f.into_iter().collect());
                }
                continue;
            }
            let Some(dir) = clip.view_dir(v) else {
                continue;
            };
            let want_ext: Option<String> = clip.manifest.as_ref().and_then(|m| {
                let k = match v {
                    View::Fg => "fg",
                    View::Matte => "matte",
                    View::Comp => "comp",
                    View::Processed => "processed",
                    _ => return None,
                };
                m.formats.get(k).cloned()
            });
            let mut files = list_images(&dir);
            // Prefer the manifest's format when both exist for a stem; otherwise take what is on disk.
            if let Some(e) = want_ext.filter(|e| files.iter().any(|(_, p)| ext_of(p) == *e)) {
                files.retain(|(_, p)| ext_of(p) == e);
            }
            let m: HashMap<String, PathBuf> = files.into_iter().collect();
            if !m.is_empty() {
                views.insert(v, m);
            }
        }
        Self::from_views(views)
    }

    pub fn from_views(views: HashMap<View, HashMap<String, PathBuf>>) -> Self {
        let set: BTreeSet<&String> = views.values().flat_map(|m| m.keys()).collect();
        let mut stems: Vec<String> = set.into_iter().cloned().collect();
        stems.sort_by(|a, b| natural_cmp(a, b));
        Self { stems, views }
    }

    pub fn len(&self) -> usize {
        self.stems.len()
    }
    pub fn is_empty(&self) -> bool {
        self.stems.is_empty()
    }
    pub fn view_frames(&self, v: View) -> usize {
        self.views.get(&v).map_or(0, |m| m.len())
    }
    /// The view has a file for `stem`.
    pub fn view_has(&self, v: View, stem: &str) -> bool {
        self.views.get(&v).is_some_and(|m| m.contains_key(stem))
    }
    /// `range` = inclusive timeline indices of the processed range, if any.
    pub fn availability(&self, v: View, i: usize, range: Option<(usize, usize)>) -> Availability {
        let Some(m) = self.views.get(&v) else {
            return Availability::ViewEmpty;
        };
        let Some(stem) = self.stems.get(i) else {
            return Availability::NoFile;
        };
        if v != View::Input && v != View::Hint {
            if let Some((a, b)) = range {
                if i < a || i > b {
                    return Availability::OutsideRange;
                }
            }
        }
        match m.get(stem) {
            Some(p) => Availability::Present(p.clone()),
            None => Availability::NoFile,
        }
    }
    /// Timeline index of the first stem equal to `stem`.
    pub fn index_of(&self, stem: &str) -> Option<usize> {
        self.stems.iter().position(|s| s == stem)
    }
    /// Source frame number: the stem's trailing digits, else the 1-based position.
    pub fn frame_number(&self, i: usize) -> u64 {
        let s = self.stems.get(i).map(String::as_str).unwrap_or("");
        let d: String = s
            .chars()
            .rev()
            .take_while(|c| c.is_ascii_digit())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        d.parse().unwrap_or(i as u64 + 1)
    }
}

/// Convert a clip's stored In/Out (indices into its input files) to inclusive timeline indices via stems,
/// so extra output-only stems never shift the range.
pub fn clip_range_to_timeline(clip: &Clip, idx: &FrameIndex) -> Option<(usize, usize)> {
    let (a, b) = clip.in_out?;
    let (sa, sb) = (&clip.input_files.get(a)?.0, &clip.input_files.get(b)?.0);
    Some((idx.index_of(sa)?, idx.index_of(sb)?))
}

/// `straight_alpha`: the manifest says Processed EXR is straight-linear. Legacy/manifest-less Processed EXR is
/// premultiplied linear in the Python writer, so it is un-premultiplied before the sRGB encode.
pub fn view_interp(
    v: View,
    path: &std::path::Path,
    input_is_linear: bool,
    straight_alpha: bool,
) -> Interp {
    match v {
        View::Hint | View::Matte => Interp::Matte,
        _ if ext_of(path) == "png" => Interp::Passthrough,
        View::Input if !input_is_linear => Interp::Passthrough,
        View::Processed if !straight_alpha => Interp::LinearToSrgbPremul,
        _ => Interp::LinearToSrgb,
    }
}

/// True when the clip's manifest declares Processed EXR as straight-linear (v2 writer).
pub fn processed_is_straight(clip: &Clip) -> bool {
    clip.manifest
        .as_ref()
        .and_then(|m| m.processed_exr_convention.as_deref())
        == Some("straight-linear")
}

// ---------------------------------------------------------------- cache

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub path: PathBuf,
    pub interp: Interp,
}

/// Byte-budgeted LRU. A frame larger than the whole budget is not retained.
pub struct Cache {
    budget: usize,
    bytes: usize,
    tick: u64,
    map: HashMap<Key, (Arc<DisplayFrame>, u64)>,
    pub peak_bytes: usize,
}

impl Cache {
    pub fn new(budget: usize) -> Self {
        Self {
            budget,
            bytes: 0,
            tick: 0,
            map: HashMap::new(),
            peak_bytes: 0,
        }
    }
    pub fn bytes(&self) -> usize {
        self.bytes
    }
    pub fn budget(&self) -> usize {
        self.budget
    }
    /// Bytes held for these keys (absent ones count 0) and how many of them are present.
    pub fn held<'a>(&self, keys: impl IntoIterator<Item = &'a Key>) -> (usize, usize) {
        keys.into_iter()
            .filter_map(|k| self.map.get(k))
            .fold((0, 0), |(b, n), e| (b + e.0.bytes(), n + 1))
    }
    pub fn len(&self) -> usize {
        self.map.len()
    }
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
    pub fn get(&mut self, k: &Key) -> Option<Arc<DisplayFrame>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|e| {
            e.1 = t;
            e.0.clone()
        })
    }
    pub fn contains(&self, k: &Key) -> bool {
        self.map.contains_key(k)
    }
    /// Drop every interpretation of `path` (a resumed job may publish a path the viewer saw as missing).
    pub fn remove_path(&mut self, path: &std::path::Path) {
        let dead: Vec<Key> = self
            .map
            .keys()
            .filter(|k| k.path == path)
            .cloned()
            .collect();
        for k in dead {
            if let Some((f, _)) = self.map.remove(&k) {
                self.bytes -= f.bytes();
            }
        }
    }
    pub fn insert(&mut self, k: Key, f: Arc<DisplayFrame>) {
        let sz = f.bytes();
        if sz > self.budget {
            return;
        }
        self.tick += 1;
        if let Some(old) = self.map.insert(k, (f, self.tick)) {
            self.bytes -= old.0.bytes();
        }
        self.bytes += sz;
        while self.bytes > self.budget {
            let victim = self
                .map
                .iter()
                .min_by_key(|(_, v)| v.1)
                .map(|(k, _)| k.clone())
                .unwrap();
            let (f, _) = self.map.remove(&victim).unwrap();
            self.bytes -= f.bytes();
        }
        self.peak_bytes = self.peak_bytes.max(self.bytes);
    }
}

// ---------------------------------------------------------------- loader

pub type DecodeFn = Arc<dyn Fn(&Key) -> Result<DisplayFrame, String> + Send + Sync>;

pub struct Done {
    pub key: Key,
    pub result: Result<Arc<DisplayFrame>, String>,
    pub ms: f64,
}

struct Queue {
    wanted: Vec<Key>,
    inflight: HashSet<Key>,
    shutdown: bool,
}
struct Shared {
    q: Mutex<Queue>,
    cv: Condvar,
}

pub struct Loader {
    shared: Arc<Shared>,
    rx: Receiver<Done>,
    handles: Vec<std::thread::JoinHandle<()>>,
}

/// Frame-cache budget when none is chosen: half of `MemAvailable` (MiB), 1–8 GiB (a 129-frame 1080p range is ~1.1 GB).
pub fn cache_mb_from_meminfo(meminfo: &str) -> usize {
    let avail_kb = meminfo
        .lines()
        .find_map(|l| l.strip_prefix("MemAvailable:"))
        .and_then(|l| l.split_whitespace().next()?.parse::<usize>().ok())
        .unwrap_or(0);
    (avail_kb / 2048).clamp(1024, 8192)
}

pub fn default_cache_mb() -> usize {
    cache_mb_from_meminfo(&std::fs::read_to_string("/proc/meminfo").unwrap_or_default())
}

pub fn default_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(2, |n| n.get().saturating_sub(1))
        .clamp(1, 4)
}

impl Loader {
    /// Running decode threads.
    pub fn workers(&self) -> usize {
        self.handles.len()
    }

    pub fn new(
        workers: usize,
        decode: DecodeFn,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        let shared = Arc::new(Shared {
            q: Mutex::new(Queue {
                wanted: vec![],
                inflight: HashSet::new(),
                shutdown: false,
            }),
            cv: Condvar::new(),
        });
        let (tx, rx) = channel();
        let handles = (0..workers.max(1))
            .map(|_| {
                let (sh, tx, decode, wake): (Arc<Shared>, Sender<Done>, DecodeFn, _) =
                    (shared.clone(), tx.clone(), decode.clone(), wake.clone());
                std::thread::spawn(move || loop {
                    let key = {
                        let mut q = sh.q.lock().unwrap();
                        loop {
                            if q.shutdown {
                                return;
                            }
                            if let Some(pos) = q.wanted.iter().position(|k| !q.inflight.contains(k))
                            {
                                let k = q.wanted.remove(pos);
                                q.inflight.insert(k.clone());
                                break k;
                            }
                            q = sh.cv.wait(q).unwrap();
                        }
                    };
                    let t = Instant::now();
                    // A panicking decoder must not kill the worker and strand the key in `inflight`.
                    let result =
                        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| decode(&key)))
                            .unwrap_or_else(|_| {
                                Err(format!("{}: decoder panicked", key.path.display()))
                            })
                            .map(Arc::new);
                    let ms = t.elapsed().as_secs_f64() * 1000.0;
                    // The key stays in `inflight` until the UI absorbs this result (`ack`), so a re-request in the
                    // meantime cannot decode it again.
                    let _ = tx.send(Done { key, result, ms });
                    if let Some(w) = &wake {
                        w();
                    }
                })
            })
            .collect();
        Self {
            shared,
            rx,
            handles,
        }
    }

    /// Replace the pending list (highest priority first). Anything queued but not listed is dropped.
    pub fn set_wanted(&self, keys: Vec<Key>) {
        self.shared.q.lock().unwrap().wanted = keys;
        self.shared.cv.notify_all();
    }

    /// The UI has taken `k`'s result: it may be requested again, and stale queued copies are dropped.
    fn ack(&self, k: &Key) {
        let mut q = self.shared.q.lock().unwrap();
        q.inflight.remove(k);
        q.wanted.retain(|w| w != k);
    }

    pub fn try_recv(&self) -> Option<Done> {
        self.rx.try_recv().ok()
    }
    pub fn recv_timeout(&self, d: std::time::Duration) -> Option<Done> {
        self.rx.recv_timeout(d).ok()
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.q.lock().unwrap().shutdown = true;
        self.shared.cv.notify_all();
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

pub fn real_decode() -> DecodeFn {
    Arc::new(|k: &Key| decode_display(&k.path, k.interp))
}

pub const MAX_RETRIES: u32 = 3;

pub enum Lookup {
    Ready(Arc<DisplayFrame>),
    Loading,
    Failed(String),
}

/// Cache + loader + the one current target. Results for any other key only warm the cache.
pub struct FrameStore {
    pub cache: Cache,
    loader: Loader,
    pub target: Option<Key>,
    errors: HashMap<Key, String>,
    attempts: HashMap<Key, u32>,
    pub decode_ms: Vec<f64>,
}

impl FrameStore {
    pub fn new(
        budget: usize,
        workers: usize,
        decode: DecodeFn,
        wake: Option<Arc<dyn Fn() + Send + Sync>>,
    ) -> Self {
        Self {
            cache: Cache::new(budget),
            loader: Loader::new(workers, decode, wake),
            target: None,
            errors: HashMap::new(),
            attempts: HashMap::new(),
            decode_ms: vec![],
        }
    }

    pub fn workers(&self) -> usize {
        self.loader.workers()
    }

    /// Make `target` the current frame and queue it first, then `prefetch` in order.
    pub fn request(&mut self, target: Key, prefetch: Vec<Key>) {
        let mut wanted = vec![target.clone()];
        wanted.extend(prefetch);
        wanted.retain(|k| !self.cache.contains(k) && !self.errors.contains_key(k));
        self.target = Some(target);
        self.loader.set_wanted(wanted);
    }

    pub fn poll(&mut self) -> usize {
        let mut n = 0;
        while let Some(d) = self.loader.try_recv() {
            self.absorb(d);
            n += 1;
        }
        n
    }

    pub fn wait_one(&mut self, d: std::time::Duration) -> bool {
        match self.loader.recv_timeout(d) {
            Some(x) => {
                self.absorb(x);
                true
            }
            None => false,
        }
    }

    fn absorb(&mut self, d: Done) {
        self.decode_ms.push(d.ms);
        self.loader.ack(&d.key);
        match d.result {
            Ok(f) if f.bytes() > self.cache.budget() => {
                // Refused by the cache: record it so the frame is not decoded again and the UI can say why.
                let e = format!(
                    "Frame is {:.0} MiB, larger than the {:.0} MiB cache budget — raise the RAM budget in Settings",
                    f.bytes() as f64 / 1048576.0,
                    self.cache.budget() as f64 / 1048576.0
                );
                self.errors.insert(d.key, e);
            }
            Ok(f) => self.cache.insert(d.key, f),
            Err(e) => {
                self.errors.insert(d.key, e);
            }
        }
    }

    /// Explicit user retry of a failed frame (e.g. it was still being written). At most `MAX_RETRIES`
    /// retries per frame, only on request, never automatic: no retry storms.
    pub fn retry(&mut self, k: &Key) -> bool {
        let n = self.attempts.entry(k.clone()).or_insert(0);
        if !self.errors.contains_key(k) || *n >= MAX_RETRIES {
            return false;
        }
        *n += 1;
        self.errors.remove(k);
        true
    }

    /// Forget everything known about `path` (cached pixels, recorded failures and retry counts).
    pub fn forget_path(&mut self, path: &std::path::Path) {
        self.cache.remove_path(path);
        self.errors.retain(|k, _| k.path != path);
        self.attempts.retain(|k, _| k.path != path);
    }

    pub fn failed(&self, k: &Key) -> bool {
        self.errors.contains_key(k)
    }

    pub fn retries_left(&self, k: &Key) -> u32 {
        MAX_RETRIES.saturating_sub(self.attempts.get(k).copied().unwrap_or(0))
    }

    pub fn lookup(&mut self, k: &Key) -> Lookup {
        if let Some(f) = self.cache.get(k) {
            Lookup::Ready(f)
        } else if let Some(e) = self.errors.get(k) {
            Lookup::Failed(e.clone())
        } else {
            Lookup::Loading
        }
    }
}

// ---------------------------------------------------------------- clock

/// Deterministic playback clock: frame index advanced from wall time at the source fps.
pub struct Clock {
    pub fps: f64,
    pub dir: isize,
    t0: Instant,
    f0: usize,
}
impl Clock {
    pub fn start(fps: f64, at: usize, dir: isize) -> Self {
        Self::start_at(fps, at, dir, Instant::now())
    }
    pub fn start_at(fps: f64, at: usize, dir: isize, t0: Instant) -> Self {
        Self {
            fps,
            dir,
            t0,
            f0: at,
        }
    }
    /// Unclamped frame index (may be negative when playing in reverse).
    pub fn frame_at(&self, now: Instant) -> isize {
        self.f0 as isize
            + self.dir * (now.duration_since(self.t0).as_secs_f64() * self.fps).floor() as isize
    }
}

/// Frame under a looping clock: `raw` (unbounded, may be negative) folded into `[lo, hi]`.
pub fn wrap_frame(raw: isize, lo: usize, hi: usize) -> usize {
    let n = (hi - lo + 1) as isize;
    lo + (raw - lo as isize).rem_euclid(n) as usize
}

/// Frames visited going from `from` to `to` in direction `dir` inside a loop over `[lo, hi]` (both ends included).
pub fn loop_span(from: usize, to: usize, dir: isize, lo: usize, hi: usize) -> Vec<usize> {
    let n = hi - lo + 1;
    let mut v = vec![from];
    let mut c = from;
    while c != to && v.len() <= n {
        c = wrap_frame(c as isize + dir, lo, hi);
        v.push(c);
    }
    v
}

/// Step within `[lo, hi]` clamped (no wrap).
pub fn step(cur: usize, delta: isize, lo: usize, hi: usize) -> usize {
    (cur as isize + delta).clamp(lo as isize, hi as isize) as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn frame(bytes: usize) -> Arc<DisplayFrame> {
        Arc::new(DisplayFrame {
            width: bytes / 4,
            height: 1,
            rgba: vec![0; bytes],
            has_alpha: false,
            clipped_hdr: false,
            info: String::new(),
        })
    }
    fn key(n: usize) -> Key {
        Key {
            path: PathBuf::from(format!("/x/{n}.png")),
            interp: Interp::Passthrough,
        }
    }

    #[test]
    fn default_cache_is_half_of_available_ram_clamped() {
        let m = |kb: u64| format!("MemTotal: 99 kB\nMemAvailable:   {kb} kB\n");
        assert_eq!(cache_mb_from_meminfo(&m(8 * 1024 * 1024)), 4096);
        assert_eq!(cache_mb_from_meminfo(&m(512 * 1024)), 1024);
        assert_eq!(cache_mb_from_meminfo(&m(64 * 1024 * 1024)), 8192);
        assert_eq!(cache_mb_from_meminfo(""), 1024);
    }

    #[test]
    fn cache_never_exceeds_budget_and_evicts_lru() {
        let mut c = Cache::new(1000);
        for i in 0..50 {
            c.insert(key(i), frame(400));
            assert!(c.bytes() <= 1000);
            c.get(&key(0)); // keep 0 hot
        }
        assert!(c.contains(&key(0)));
        assert!(c.peak_bytes <= 1000);
        c.insert(key(999), frame(5000)); // larger than budget: not retained
        assert!(!c.contains(&key(999)));
    }

    fn views(files: &[(View, &[&str])]) -> FrameIndex {
        FrameIndex::from_views(
            files
                .iter()
                .map(|(v, s)| {
                    (
                        *v,
                        s.iter()
                            .map(|x| (x.to_string(), PathBuf::from(format!("/{v:?}/{x}.exr"))))
                            .collect(),
                    )
                })
                .collect(),
        )
    }

    #[test]
    fn holes_and_range_by_stem_not_position() {
        let idx = views(&[
            (View::Input, &["f_1", "f_2", "f_10", "f_3"]),
            (View::Matte, &["f_1", "f_10"]),
        ]);
        assert_eq!(idx.stems, ["f_1", "f_2", "f_3", "f_10"]);
        assert!(matches!(
            idx.availability(View::Matte, 0, None),
            Availability::Present(_)
        ));
        assert_eq!(idx.availability(View::Matte, 1, None), Availability::NoFile); // hole stays at f_2
        assert!(matches!(
            idx.availability(View::Matte, 3, None),
            Availability::Present(_)
        ));
        assert_eq!(
            idx.availability(View::Matte, 3, Some((0, 1))),
            Availability::OutsideRange
        );
        assert!(matches!(
            idx.availability(View::Input, 3, Some((0, 1))),
            Availability::Present(_)
        ));
        assert_eq!(
            idx.availability(View::Comp, 0, None),
            Availability::ViewEmpty
        );
        assert_eq!(idx.frame_number(3), 10);
    }

    #[test]
    fn loop_wraps_forward_and_reverse_inside_the_range() {
        assert_eq!(wrap_frame(8, 3, 7), 3);
        assert_eq!(wrap_frame(2, 3, 7), 7);
        assert_eq!(wrap_frame(-1, 3, 7), 4);
        assert_eq!(wrap_frame(5, 5, 5), 5);
        assert_eq!(loop_span(6, 4, 1, 3, 7), [6, 7, 3, 4]);
        assert_eq!(loop_span(4, 6, -1, 3, 7), [4, 3, 7, 6]);
        assert_eq!(loop_span(5, 5, 1, 3, 7), [5]);
    }

    #[test]
    fn step_is_clamped_and_deterministic() {
        assert_eq!(step(5, -1, 3, 9), 4);
        assert_eq!(step(3, -1, 3, 9), 3);
        assert_eq!(step(9, 1, 3, 9), 9);
        assert_eq!(step(0, 100, 0, 9), 9);
    }

    fn px(info: &str) -> DisplayFrame {
        DisplayFrame {
            width: 1,
            height: 1,
            rgba: vec![0; 4],
            has_alpha: false,
            clipped_hdr: false,
            info: info.into(),
        }
    }

    fn wait_ready(s: &mut FrameStore, k: &Key) -> Arc<DisplayFrame> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            s.poll();
            if let Lookup::Ready(f) = s.lookup(k) {
                return f;
            }
            assert!(Instant::now() < deadline, "{k:?} never decoded");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[test]
    fn seek_storm_trims_the_queue_and_the_last_target_wins() {
        let slow: DecodeFn = Arc::new(|k: &Key| {
            std::thread::sleep(Duration::from_millis(2));
            Ok(px(&k.path.display().to_string()))
        });
        let mut s = FrameStore::new(10_000_000, 3, slow, None);
        let (mut seed, mut last) = (12345u64, 0);
        for _ in 0..1000 {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            last = (seed >> 33) as usize % 500;
            s.request(key(last), vec![key(last + 1)]);
            s.poll();
        }
        let f = wait_ready(&mut s, &key(last));
        assert_eq!(f.info, key(last).path.display().to_string());
        // Pending work for superseded targets is dropped, not decoded: far fewer than 2000 decodes ran.
        assert!(
            s.decode_ms.len() < 1000,
            "decoded {} frames",
            s.decode_ms.len()
        );
    }

    #[test]
    fn late_completion_of_a_superseded_frame_never_becomes_the_target() {
        // Frame A's decode is held open by a gate; the user seeks to B meanwhile. When A finally completes it
        // may warm the cache, but the store's target stays B and B is what lookup(target) resolves to.
        let gate = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let g2 = gate.clone();
        let (a, b) = (key(1), key(2));
        let a2 = a.clone();
        let dec: DecodeFn = Arc::new(move |k: &Key| {
            if *k == a2 {
                let (m, cv) = &*g2;
                let mut open = m.lock().unwrap();
                while !*open {
                    open = cv.wait(open).unwrap();
                }
            }
            Ok(px(&k.path.display().to_string()))
        });
        let mut s = FrameStore::new(10_000_000, 2, dec, None);
        s.request(a.clone(), vec![]);
        std::thread::sleep(Duration::from_millis(30)); // A is now in flight and blocked
        s.request(b.clone(), vec![]);
        let fb = wait_ready(&mut s, &b);
        assert_eq!(fb.info, b.path.display().to_string());
        assert!(
            matches!(s.lookup(&a), Lookup::Loading),
            "A has not completed yet"
        );
        assert_eq!(s.target.as_ref(), Some(&b));
        // Release A: it completes late.
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let fa = wait_ready(&mut s, &a);
        assert_eq!(fa.info, a.path.display().to_string());
        assert_eq!(
            s.target.as_ref(),
            Some(&b),
            "a late result must not retarget the store"
        );
        assert!(
            matches!(s.lookup(s.target.clone().as_ref().unwrap()), Lookup::Ready(f) if f.info == b.path.display().to_string())
        );
    }

    #[test]
    fn explicit_retry_is_bounded_and_a_panicking_decoder_does_not_kill_the_worker() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = calls.clone();
        let dec: DecodeFn = Arc::new(move |k: &Key| {
            let n = c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match (k.path.to_string_lossy().ends_with("/9.png"), n) {
                (true, 0) => panic!("hostile file"),
                (true, 1) => Err("still being written".into()),
                _ => Ok(px("ok")),
            }
        });
        let mut s = FrameStore::new(10_000_000, 1, dec, None); // one worker: a dead worker would hang everything
        let k = key(9);
        s.request(k.clone(), vec![]);
        assert!(s.wait_one(Duration::from_secs(2)));
        assert!(matches!(s.lookup(&k), Lookup::Failed(m) if m.contains("panicked")));
        // No automatic retry: re-requesting does not start another decode.
        s.request(k.clone(), vec![]);
        assert!(!s.wait_one(Duration::from_millis(100)));
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Explicit retry works (error 2), and a worker that survived the panic serves other frames.
        assert!(s.retry(&k));
        s.request(k.clone(), vec![]);
        assert!(s.wait_one(Duration::from_secs(2)));
        assert!(matches!(s.lookup(&k), Lookup::Failed(m) if m.contains("being written")));
        assert!(s.retry(&k));
        s.request(k.clone(), vec![]);
        wait_ready(&mut s, &k);
        // Retry budget: a success needs no more; exhausted budget refuses further retries.
        let bad: DecodeFn = Arc::new(|_| Err("nope".into()));
        let mut s2 = FrameStore::new(1000, 1, bad, None);
        let k2 = key(5);
        for round in 0..MAX_RETRIES {
            s2.request(k2.clone(), vec![]);
            assert!(s2.wait_one(Duration::from_secs(2)), "round {round}");
            assert!(s2.retry(&k2), "retry {round} should be allowed");
        }
        s2.request(k2.clone(), vec![]);
        assert!(s2.wait_one(Duration::from_secs(2)));
        assert!(!s2.retry(&k2), "budget exhausted");
        assert_eq!(s2.retries_left(&k2), 0);
    }

    #[test]
    fn oversized_frame_fails_with_budget_message_and_is_decoded_once() {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = n.clone();
        let big: DecodeFn = Arc::new(move |_| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(DisplayFrame {
                width: 100,
                height: 100,
                rgba: vec![0; 40_000],
                has_alpha: false,
                clipped_hdr: false,
                info: String::new(),
            })
        });
        let mut s = FrameStore::new(1000, 1, big, None);
        for _ in 0..5 {
            s.request(key(1), vec![key(1)]);
            s.wait_one(Duration::from_millis(100));
        }
        assert!(matches!(s.lookup(&key(1)), Lookup::Failed(m) if m.contains("cache budget")));
        assert_eq!(n.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn unabsorbed_completion_is_not_decoded_again_by_a_re_request() {
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c = n.clone();
        let dec: DecodeFn = Arc::new(move |k| {
            c.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(px(&k.path.display().to_string()))
        });
        let mut s = FrameStore::new(10_000, 1, dec, None);
        s.request(key(1), vec![]);
        while n.load(std::sync::atomic::Ordering::SeqCst) == 0 {
            std::thread::sleep(Duration::from_millis(1));
        }
        // Slow receiver: the result is sent but not yet polled while the UI keeps asking for the key.
        for _ in 0..20 {
            s.request(key(1), vec![]);
            std::thread::sleep(Duration::from_millis(2));
        }
        wait_ready(&mut s, &key(1));
        s.request(key(1), vec![]);
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(n.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn failed_decode_is_reported_not_retried_forever() {
        let bad: DecodeFn = Arc::new(|_| Err("boom".into()));
        let mut s = FrameStore::new(1000, 1, bad, None);
        s.request(key(1), vec![]);
        assert!(s.wait_one(Duration::from_secs(2)));
        assert!(matches!(s.lookup(&key(1)), Lookup::Failed(m) if m == "boom"));
    }
}

#[cfg(test)]
mod clock_tests {
    use super::*;
    #[test]
    fn clock_direction() {
        let c = Clock::start(24.0, 10, -1);
        assert_eq!(c.frame_at(c.t0), 10);
        assert!(c.frame_at(c.t0 + std::time::Duration::from_millis(500)) == 10 - 12);
        let c = Clock::start(24.0, 10, 1);
        assert_eq!(
            c.frame_at(c.t0 + std::time::Duration::from_millis(1000)),
            34
        );
    }
}
