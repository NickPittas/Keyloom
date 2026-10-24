//! The runtime driver: everything that advances work (scheduler dispatch, worker messages, preflight, locks,
//! task-end effects, queue history, network claims/progress/outbox, metrics helper) runs on its own thread, so a
//! covered or minimized window cannot stall it. The UI shares the same objects through `Held`: a frame takes
//! them for its duration, then hands them back; between frames (or while nothing is drawn) the driver owns them.
//! Lock order is always ai → remote → metrics.
use crate::ui::ai::AiUi;
use parking_lot::{lock_api::ArcMutexGuard, Mutex, RawMutex};
use std::cell::OnceCell;
use std::ops::{Deref, DerefMut};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

type Guard<T> = ArcMutexGuard<RawMutex, T>;

/// A value shared with the driver thread. Dereferencing locks it (once) until `release`.
pub struct Held<T> {
    arc: Arc<Mutex<T>>,
    guard: OnceCell<Guard<T>>,
}

impl<T> Held<T> {
    pub fn new(v: T) -> Self {
        Self { arc: Arc::new(Mutex::new(v)), guard: OnceCell::new() }
    }
    pub fn shared(&self) -> Arc<Mutex<T>> {
        self.arc.clone()
    }
    /// Take the lock now (frame start: fixes the lock order).
    pub fn hold(&self) {
        let _ = self.guard.get_or_init(|| self.arc.lock_arc());
    }
    pub fn is_held(&self) -> bool {
        self.guard.get().is_some()
    }
    /// Hand the lock to the driver (fairly: a waiting driver gets it before the next frame can re-take it).
    pub fn release(&mut self) {
        if let Some(g) = self.guard.take() {
            ArcMutexGuard::unlock_fair(g);
        }
    }
}

impl<T> Deref for Held<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.get_or_init(|| self.arc.lock_arc())
    }
}
impl<T> DerefMut for Held<T> {
    fn deref_mut(&mut self) -> &mut T {
        self.hold();
        self.guard.get_mut().expect("held")
    }
}

/// Settings as the UI last saved them, for the driver (it never touches the UI's page).
#[derive(Default)]
pub struct Snapshot {
    pub settings: crate::settings::Settings,
    pub path: Option<PathBuf>,
}

pub struct Driver {
    stop: Arc<AtomicBool>,
    thread: std::thread::Thread,
    pub snapshot: Arc<Mutex<Snapshot>>,
}

pub struct Parts {
    pub ai: Arc<Mutex<AiUi>>,
    pub remote: Arc<Mutex<crate::remote_set::RemoteSet>>,
    pub metrics: Arc<Mutex<crate::metrics::Supervisor>>,
    pub repo: Option<PathBuf>,
    pub python: Option<PathBuf>,
    pub ctx: eframe::egui::Context,
}

const PERIOD: Duration = Duration::from_millis(100);

impl Driver {
    pub fn start(p: Parts, first: Snapshot) -> std::io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let snapshot = Arc::new(Mutex::new(first));
        let (s2, snap2) = (stop.clone(), snapshot.clone());
        let h = std::thread::Builder::new().name("ck-driver".into()).spawn(move || {
            let ctx = p.ctx.clone();
            let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
            while !s2.load(Ordering::Relaxed) {
                let now = Instant::now();
                {
                    let mut ai = p.ai.lock();
                    let mut remote = p.remote.lock();
                    let mut metrics = p.metrics.lock();
                    metrics.tick(now, &wake);
                    ai.exec_tick();
                    if remote.available() {
                        let view = metrics.view(now);
                        let snap = snap2.lock();
                        let mut env = crate::remote::Env {
                            settings: &snap.settings,
                            settings_path: snap.path.as_deref(),
                            ai: &mut ai,
                            metrics: &view,
                            repo: p.repo.as_deref(),
                            python: p.python.as_deref(),
                            wake: &wake,
                            cfg: Default::default(),
                        };
                        remote.tick(now, &mut env);
                        for f in remote.take_finished() {
                            ai.push_refresh(f);
                        }
                    }
                    if ai.active() || ai.has_refreshes() {
                        p.ctx.request_repaint_after(PERIOD);
                    }
                }
                std::thread::park_timeout(PERIOD);
            }
        })?;
        Ok(Self { stop, thread: h.thread().clone(), snapshot })
    }

    /// The UI saved settings: the driver sees them on its next pass.
    pub fn publish(&self, s: &crate::settings::Settings, path: Option<&std::path::Path>) {
        let mut g = self.snapshot.lock();
        if g.settings != *s || g.path.as_deref() != path {
            g.settings = s.clone();
            g.path = path.map(PathBuf::from);
        }
    }

    /// Ask the thread to end (it owns its own handles; no join, so a held UI lock can never deadlock this).
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.unpark();
    }
}

impl Drop for Driver {
    fn drop(&mut self) {
        self.stop();
    }
}
