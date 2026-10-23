//! Native file dialogs run on a worker thread so the UI never freezes; one dialog at a time, result via channel.
use crate::project::VIDEO_EXTS;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PickKind {
    Clips,
    Folder,
    Project,
    /// Parent folder for a new project (the project folder is created inside it).
    Location,
    /// An existing project to add clips to.
    AddTo,
    /// Existing parent folder in which File > New project creates the new project folder.
    NewProject,
    /// One companion alpha/mask movie for the open clip.
    HintMovie,
    /// The folder a missing external image sequence now lives in.
    Relink,
    /// A replacement movie for a clip whose movie is gone.
    RelinkMovie,
    /// A folder of alpha/mask frames to import into the open clip.
    HintSequence,
    /// Destination folder for an exported video.
    ExportFolder,
    /// Local folder for a Path Mapping row in Settings.
    MappingRoot,
}

/// Blocks until the user chooses; an empty list = cancelled. Tests inject a mock.
pub type Backend = Arc<dyn Fn(PickKind, Option<PathBuf>) -> Vec<PathBuf> + Send + Sync>;

pub fn rfd_backend() -> Backend {
    Arc::new(|kind, dir| {
        let mut d = rfd::FileDialog::new();
        if let Some(dir) = dir {
            d = d.set_directory(dir);
        }
        match kind {
            PickKind::Clips => d
                .set_title("Import clips")
                .add_filter("Movies", &VIDEO_EXTS)
                .pick_files()
                .unwrap_or_default(),
            PickKind::Folder => d
                .set_title("Import folder (movies and image sequences)")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::Project => d
                .set_title("Open a CorridorKey project folder")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::Location => d
                .set_title("Choose the folder to create the new project in")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::NewProject => d
                .set_title("Choose the folder to create the new project in")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::HintMovie => d
                .set_title("Choose the alpha or mask movie for this clip")
                .add_filter("Movies", &VIDEO_EXTS)
                .pick_file()
                .into_iter()
                .collect(),
            PickKind::Relink => d
                .set_title("Choose the folder that now holds this clip's image sequence")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::RelinkMovie => d
                .set_title("Choose the movie for this clip")
                .add_filter("Movies", &VIDEO_EXTS)
                .pick_file()
                .into_iter()
                .collect(),
            PickKind::HintSequence => d
                .set_title("Choose the folder of alpha or mask frames (PNG/EXR)")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::ExportFolder => d
                .set_title("Choose the folder for the exported video")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::MappingRoot => d
                .set_title("Choose the folder where this PC sees the shared location")
                .pick_folder()
                .into_iter()
                .collect(),
            PickKind::AddTo => d
                .set_title("Choose an existing CorridorKey project to add to")
                .pick_folder()
                .into_iter()
                .collect(),
        }
    })
}

pub struct Picker {
    backend: Backend,
    pending: Option<(PickKind, Receiver<Vec<PathBuf>>)>,
}

impl Picker {
    pub fn new(backend: Backend) -> Self {
        Self {
            backend,
            pending: None,
        }
    }
    pub fn pending(&self) -> Option<PickKind> {
        self.pending.as_ref().map(|p| p.0)
    }
    /// Open a dialog unless one is already outstanding. `wake` runs when the dialog closes.
    pub fn start(
        &mut self,
        kind: PickKind,
        dir: Option<PathBuf>,
        wake: impl Fn() + Send + 'static,
    ) -> bool {
        if self.pending.is_some() {
            return false;
        }
        let (tx, rx) = channel();
        let b = self.backend.clone();
        let spawned = std::thread::Builder::new()
            .name("file-dialog".into())
            .spawn(move || {
                // A dialog backend that panics (no portal) drops `tx`: reported as a cancel, not a hang.
                let _ = tx.send(b(kind, dir));
                wake();
            });
        if spawned.is_err() {
            return false;
        }
        self.pending = Some((kind, rx));
        true
    }
    pub fn poll(&mut self) -> Option<(PickKind, Vec<PathBuf>)> {
        let (kind, rx) = self.pending.as_ref()?;
        let kind = *kind;
        match rx.try_recv() {
            Ok(v) => {
                self.pending = None;
                Some((kind, v))
            }
            Err(TryRecvError::Disconnected) => {
                self.pending = None;
                Some((kind, vec![]))
            }
            Err(TryRecvError::Empty) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Condvar, Mutex};
    use std::time::{Duration, Instant};

    #[test]
    fn one_outstanding_dialog_and_start_never_blocks() {
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let g = gate.clone();
        let mut p = Picker::new(Arc::new(move |_, _| {
            let mut open = g.0.lock().unwrap();
            while !*open {
                open = g.1.wait(open).unwrap();
            }
            vec![PathBuf::from("/x")]
        }));
        let t = Instant::now();
        assert!(p.start(PickKind::Folder, None, || {}));
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "start must not block"
        );
        assert!(
            !p.start(PickKind::Clips, None, || {}),
            "second dialog refused"
        );
        assert_eq!(p.pending(), Some(PickKind::Folder));
        assert!(p.poll().is_none(), "still open");
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        let d = Instant::now() + Duration::from_secs(5);
        let r = loop {
            if let Some(r) = p.poll() {
                break r;
            }
            assert!(Instant::now() < d);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(r, (PickKind::Folder, vec![PathBuf::from("/x")]));
        assert!(p.pending().is_none());
        assert!(p.start(PickKind::Clips, None, || {}), "free again");
    }

    #[test]
    fn panicking_backend_counts_as_cancel() {
        let mut p = Picker::new(Arc::new(|_, _| panic!("no portal")));
        assert!(p.start(PickKind::Project, None, || {}));
        let d = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(r) = p.poll() {
                assert_eq!(r, (PickKind::Project, vec![]));
                break;
            }
            assert!(Instant::now() < d);
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}
