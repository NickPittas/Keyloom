//! Local model-manager subprocess client. The Python helper owns the catalog and path policy;
//! this module only passes resolved runtime roots and supervises its JSONL protocol.
use serde_json::Value;
use std::ffi::OsString;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

const STDERR_LINES: usize = 20;
const DRAIN_STDOUT: std::time::Duration = std::time::Duration::from_secs(2);
const DRAIN_STDERR: std::time::Duration = std::time::Duration::from_millis(300);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    Catalog,
    Check,
    VerifyAll,
    Verify(String),
    Download(String),
    DownloadProfile(Profile),
    Repair(String),
    Delete(String),
    Cancel,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Required,
    Full,
}

impl Action {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Check => "check",
            Self::VerifyAll | Self::Verify(_) => "verify",
            Self::Download(_) | Self::DownloadProfile(_) => "download",
            Self::Repair(_) => "repair",
            Self::Delete(_) => "delete",
            Self::Cancel => "cancel",
        }
    }

    pub fn requires_operation_id(&self) -> bool {
        matches!(
            self,
            Self::Download(_) | Self::DownloadProfile(_) | Self::Repair(_) | Self::Delete(_) | Self::Cancel
        )
    }

    fn arguments(&self) -> Vec<OsString> {
        match self {
            Self::Catalog => vec!["--catalog".into()],
            Self::Check => vec!["--check".into(), "--json".into()],
            Self::VerifyAll => vec!["--verify".into()],
            Self::Verify(id) => vec!["--verify".into(), "--target".into(), id.into()],
            Self::Download(id) => vec!["--download".into(), "--target".into(), id.into()],
            Self::DownloadProfile(profile) => vec![
                "--download".into(),
                "--profile".into(),
                match profile {
                    Profile::Required => "required",
                    Profile::Full => "full",
                }
                .into(),
            ],
            Self::Repair(id) => vec!["--repair".into(), "--target".into(), id.into()],
            Self::Delete(id) => vec!["--delete".into(), "--target".into(), id.into()],
            Self::Cancel => vec!["--cancel".into()],
        }
    }
}

/// Explicit overrides derived from the active descriptor or user environment. Missing roots stay
/// with the helper's documented platform defaults, so this client never reimplements them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootArgs(Vec<(OsString, OsString)>);

impl RootArgs {
    pub fn from_launch(launch: &crate::runtime::Launch) -> Self {
        let mut roots = Vec::new();
        let user_root = std::env::var_os("CORRIDORKEY_V2_MODELS_DIR")
            .filter(|p| !p.is_empty())
            .or_else(|| {
                launch
                    .install
                    .as_ref()
                    .and_then(|i| i.models.as_ref().map(|p| p.as_os_str().to_owned()))
            });
        if let Some(path) = user_root {
            roots.push(("--v2-models-dir".into(), path));
        }
        if let Some(path) = std::env::var_os("HF_HUB_CACHE").filter(|p| !p.is_empty()) {
            roots.push(("--hf-cache".into(), path));
        }
        if let Some(install) = &launch.install {
            if let Some(path) = &install.user_data {
                roots.push(("--data-dir".into(), path.as_os_str().to_owned()));
            }
        }
        Self(roots)
    }

    fn append_to(&self, args: &mut Vec<OsString>) {
        for (flag, path) in &self.0 {
            args.push(flag.clone());
            args.push(path.clone());
        }
    }
}

#[derive(Clone, Debug)]
pub struct Runtime {
    pub repo: PathBuf,
    python: PathBuf,
    helper: PathBuf,
    install_env: Option<(&'static str, PathBuf)>,
    /// An installation descriptor is active: scrub inherited Python variables from the child.
    scrub_python: bool,
    roots: RootArgs,
}

impl Runtime {
    pub fn resolve(repo_override: Option<&Path>) -> Result<Self, String> {
        let launch = match repo_override {
            Some(repo) => crate::runtime::launch_explicit(repo),
            None => crate::runtime::launch(None),
        };
        let repo = launch.repo.clone().ok_or_else(|| {
            launch
                .problem
                .clone()
                .map(|p| format!("Installed runtime is unusable: {p}"))
                .unwrap_or_else(|| {
                    "No Keyloom runtime was found; install the app or select a checkout.".into()
                })
        })?;
        let python = std::env::var_os("CK_PYTHON")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| launch.python.clone())
            .unwrap_or_else(|| crate::import::python(&repo));
        if !python.is_file() {
            return Err(format!(
                "Model helper Python was not found at {}. Repair the runtime or set CK_PYTHON.",
                python.display()
            ));
        }
        let helper = repo.join("scripts/setup_models.py");
        if !helper.is_file() {
            return Err(format!(
                "Model helper was not found at {}. Repair the installation.",
                helper.display()
            ));
        }
        Ok(Self {
            repo,
            python,
            helper,
            install_env: crate::runtime::child_env(launch.install.as_ref()),
            scrub_python: launch.install.is_some(),
            roots: RootArgs::from_launch(&launch),
        })
    }

    pub fn command_args(&self, action: &Action) -> Result<Vec<OsString>, String> {
        self.command_args_with_operation_id(action, None)
    }

    fn command_args_with_operation_id(
        &self,
        action: &Action,
        operation_id: Option<&str>,
    ) -> Result<Vec<OsString>, String> {
        if action.requires_operation_id() != operation_id.is_some() {
            return Err(if action.requires_operation_id() {
                "download, repair, and cancel require an operation ID".into()
            } else {
                "an operation ID is only valid for download, repair, and cancel".into()
            });
        }
        if operation_id.is_some_and(|id| !valid_operation_id(id)) {
            return Err("invalid model operation ID".into());
        }
        let mut args = vec![self.helper.as_os_str().to_owned()];
        args.extend(action.arguments());
        if let Some(id) = operation_id {
            args.push("--operation-id".into());
            args.push(id.into());
        }
        self.roots.append_to(&mut args);
        Ok(args)
    }

    pub fn spawn(&self, action: &Action) -> Result<Job, String> {
        self.spawn_with_operation_id(action, None)
    }

    pub fn spawn_with_operation_id(
        &self,
        action: &Action,
        operation_id: Option<&str>,
    ) -> Result<Job, String> {
        let mut command = Command::new(&self.python);
        command
            .current_dir(&self.repo)
            .args(self.command_args_with_operation_id(action, operation_id)?);
        if let Some((key, value)) = &self.install_env {
            command.env(key, value);
        }
        if self.scrub_python {
            for k in crate::runtime::POLLUTING_ENV {
                command.env_remove(k);
            }
            command.env("PYTHONNOUSERSITE", "1");
        }
        Job::spawn(command, &self.python)
    }
}

fn valid_operation_id(id: &str) -> bool {
    let mut chars = id.chars();
    id.len() <= 128
        && chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
}

#[derive(Debug)]
pub enum Event {
    Json(Value),
    Malformed(String),
    Exited { code: Option<i32>, stderr: String },
}

pub struct Job {
    rx: Receiver<Event>,
}

impl Job {
    fn spawn(mut command: Command, program: &Path) -> Result<Self, String> {
        crate::runtime::prepare_python_command(&mut command, None);
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start model helper {}: {e}", program.display()))?;
        let out = child.stdout.take().expect("stdout was piped");
        let err = child.stderr.take().expect("stderr was piped");
        let (tx, rx) = channel();
        let out_tx = tx.clone();
        let (out_done_tx, out_done) = channel::<()>();
        let out_reader = std::thread::spawn(move || {
            let _done = out_done_tx;
            for line in BufReader::new(out).lines() {
                match line {
                    Ok(line) if line.trim().is_empty() => continue,
                    Ok(line) => match serde_json::from_str(&line) {
                        Ok(v) => {
                            let _ = out_tx.send(Event::Json(v));
                        }
                        Err(e) => {
                            let _ = out_tx.send(Event::Malformed(format!(
                                "invalid helper JSONL: {e}: {}",
                                truncate(&line)
                            )));
                        }
                    },
                    Err(e) => {
                        let _ = out_tx.send(Event::Malformed(format!(
                            "could not read helper stdout: {e}"
                        )));
                        break;
                    }
                }
            }
        });
        let err_tail =
            std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        let (err_done_tx, err_done) = channel::<()>();
        let shared_tail = err_tail.clone();
        let err_reader = std::thread::spawn(move || {
            let _done = err_done_tx;
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                let mut tail = shared_tail.lock().unwrap_or_else(|e| e.into_inner());
                tail.push_back(truncate(&line));
                if tail.len() > STDERR_LINES {
                    tail.pop_front();
                }
            }
        });
        std::thread::spawn(move || {
            let code = loop {
                match child.try_wait() {
                    Ok(Some(status)) => break status.code(),
                    Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
                    Err(_) => break None,
                }
            };
            let _ = out_done.recv_timeout(DRAIN_STDOUT);
            let _ = err_done.recv_timeout(DRAIN_STDERR);
            let stderr = err_tail
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect::<Vec<_>>()
                .join("\n");
            drop(out_reader);
            drop(err_reader);
            let _ = tx.send(Event::Exited { code, stderr });
        });
        Ok(Self { rx })
    }

    pub fn try_recv(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(event) => Some(event),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn disconnected() -> Self {
        Self { rx: channel().1 }
    }

    #[cfg(test)]
    pub(crate) fn from_receiver(rx: Receiver<Event>) -> Self {
        Self { rx }
    }
}

fn truncate(line: &str) -> String {
    const MAX_CHARS: usize = 2000;
    if line.chars().count() <= MAX_CHARS {
        return line.to_owned();
    }
    let end = line
        .char_indices()
        .nth(MAX_CHARS)
        .map(|(i, _)| i)
        .unwrap_or(line.len());
    format!("{}…", &line[..end])
}

pub fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub fn number(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}

pub fn flag(v: &Value, key: &str) -> Option<bool> {
    v.get(key).and_then(Value::as_bool)
}

pub fn strings(v: &Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

// ---- the app's readiness snapshot ---------------------------------------------------------------------------------
//
// ONE record of what is installed, kept by the app: the Models page, the action buttons and the scheduler all read it.
// It is replaced when a check ends (startup, Check/Verify, root change, finished Install/Repair/Delete) and never hashes.
// The executor learns nothing from it: every job carries the exact files (`model_files`) taken from here.

/// Exact model files for one job: dependency id -> absolute path (what `backend/app_models.py` resolved).
pub type ModelFiles = std::collections::BTreeMap<String, String>;

/// What the UI may offer, derived from the snapshot (and the GPU the workers report). Never asks a worker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    pub key: bool,
    /// Per task kind (`TaskKind::wire`).
    pub tasks: std::collections::BTreeMap<String, bool>,
    /// Ready BiRefNet display variants and SAM tracker model ids.
    pub birefnet_variants: Vec<String>,
    pub sam_models: Vec<String>,
    /// Why a family is unavailable (`gpu`, `ffmpeg`, `models`): shown, never guessed.
    pub task_unavailable: std::collections::BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Binding {
    pub ready: bool,
    /// present | missing (a size mismatch is missing); never a hash verdict
    pub state: String,
    pub paths: ModelFiles,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Snapshot {
    /// task -> variant catalog id -> binding.
    models: std::collections::BTreeMap<String, std::collections::BTreeMap<String, Binding>>,
    ffmpeg: bool,
    /// SAM tracker model (`small`...) / BiRefNet display variant -> catalog id.
    sam2: std::collections::BTreeMap<String, String>,
    birefnet: std::collections::BTreeMap<String, String>,
}

impl Snapshot {
    /// The helper's `snapshot` event (`AppModels.document()`); `None` when it is not that shape.
    pub fn from_json(v: &Value) -> Option<Self> {
        let map = |k: &str| -> std::collections::BTreeMap<String, String> {
            v.get(k)
                .and_then(Value::as_object)
                .map(|o| o.iter().filter_map(|(a, b)| Some((a.clone(), b.as_str()?.to_owned()))).collect())
                .unwrap_or_default()
        };
        let mut models = std::collections::BTreeMap::new();
        for (task, variants) in v.get("models")?.as_object()? {
            let mut by_id = std::collections::BTreeMap::new();
            for (id, b) in variants.as_object()? {
                let paths = b.get("paths").and_then(Value::as_object)?;
                by_id.insert(
                    id.clone(),
                    Binding {
                        ready: b.get("ready").and_then(Value::as_bool)?,
                        state: string(b, "state").unwrap_or_default(),
                        paths: paths.iter().filter_map(|(k, p)| Some((k.clone(), p.as_str()?.to_owned()))).collect(),
                    },
                );
            }
            models.insert(task.clone(), by_id);
        }
        Some(Self { models, ffmpeg: flag(v, "ffmpeg").unwrap_or(false), sam2: map("sam2"), birefnet: map("birefnet") })
    }

    fn binding(&self, task: &str, id: &str) -> Option<&Binding> {
        self.models.get(task)?.get(id)
    }

    /// Default variant of a one-entry task (`key`, `gvm`, `matanyone2`, `videomama`).
    fn fixed(&self, task: &str) -> Option<&Binding> {
        self.models.get(task)?.values().next()
    }

    fn files_of(b: Option<&Binding>, what: &str) -> Result<ModelFiles, String> {
        match b {
            Some(b) if b.ready && !b.paths.is_empty() => Ok(b.paths.clone()),
            Some(_) => Err(format!("{what} is missing — download it on the Models page")),
            None => Err(format!("{what} is not in the model catalog")),
        }
    }

    /// The exact files a Key job needs, or why it cannot start.
    pub fn key_files(&self) -> Result<ModelFiles, String> {
        Self::files_of(self.fixed("key"), "The Key model")
    }

    /// The exact files a task needs (empty for kinds that load no model), or why it cannot start.
    pub fn task_files(&self, kind: crate::tasks::TaskKind, params: &Value) -> Result<ModelFiles, String> {
        use crate::tasks::TaskKind as K;
        let want = |k: &str| params.get(k).and_then(Value::as_str).unwrap_or_default();
        match kind {
            K::Gvm => Self::files_of(self.fixed("gvm"), "GVM"),
            K::Matanyone2 => Self::files_of(self.fixed("matanyone2"), "MatAnyone2"),
            K::Videomama => Self::files_of(self.fixed("videomama"), "VideoMaMa"),
            K::Birefnet => {
                let id = self.birefnet.get(want("variant")).ok_or_else(|| format!("BiRefNet variant {:?} is unknown", want("variant")))?;
                Self::files_of(self.binding("birefnet", id), &format!("BiRefNet {}", want("variant")))
            }
            K::SamPreview | K::SamTrack => {
                let id = self.sam2.get(want("tracker_model")).ok_or_else(|| format!("SAM model {:?} is unknown", want("tracker_model")))?;
                Self::files_of(self.binding("sam2", id), &format!("SAM {}", want("tracker_model")))
            }
            K::Chroma | K::PrematteHint | K::ExportVideo | K::PrepareSource | K::ImportHint => Ok(ModelFiles::new()),
        }
    }

    /// What the UI may offer. `gpu` = a usable GPU is (or may still turn out to be) present.
    pub fn capabilities(&self, gpu: bool) -> Capabilities {
        use crate::tasks::TaskKind as K;
        let empty = serde_json::json!({});
        let mut c = Capabilities { key: gpu && self.key_files().is_ok(), ..Default::default() };
        for (name, id) in &self.birefnet {
            if self.binding("birefnet", id).is_some_and(|b| b.ready) {
                c.birefnet_variants.push(name.clone());
            }
        }
        for (name, id) in &self.sam2 {
            if self.binding("sam2", id).is_some_and(|b| b.ready) {
                c.sam_models.push(name.clone());
            }
        }
        for k in K::ALL {
            let needs_gpu = matches!(k, K::Gvm | K::Birefnet | K::SamPreview | K::SamTrack | K::Matanyone2 | K::Videomama);
            let params = match k {
                K::Birefnet => c.birefnet_variants.first().map_or(empty.clone(), |v| serde_json::json!({"variant": v})),
                K::SamPreview | K::SamTrack => c.sam_models.first().map_or(empty.clone(), |v| serde_json::json!({"tracker_model": v})),
                _ => empty.clone(),
            };
            let models_ok = match k {
                K::Birefnet => !c.birefnet_variants.is_empty(),
                K::SamPreview | K::SamTrack => !c.sam_models.is_empty(),
                K::ExportVideo | K::PrepareSource | K::ImportHint => self.ffmpeg,
                _ => self.task_files(k, &params).is_ok(),
            };
            c.tasks.insert(k.wire().into(), models_ok && (gpu || !needs_gpu));
        }
        if !gpu {
            c.task_unavailable.insert("gpu".into(), "no usable CUDA GPU".into());
        }
        if !self.ffmpeg {
            c.task_unavailable.insert("ffmpeg".into(), "FFmpeg not installed or not usable".into());
        }
        c
    }

    /// Every model ready with made-up paths: for tests that need jobs to dispatch.
    #[doc(hidden)]
    pub fn for_tests() -> Self {
        let b = |id: &str| Binding { ready: true, state: "present".into(), paths: [(id.to_owned(), format!("/models/{id}"))].into() };
        let one = |task: &str, ids: &[&str]| (task.to_owned(), ids.iter().map(|i| (i.to_string(), b(i))).collect());
        Self {
            models: [
                one("key", &["corridorkey-weights"]),
                one("gvm", &["gvm"]),
                one("matanyone2", &["matanyone2"]),
                one("videomama", &["videomama"]),
                one("sam2", &["sam2-small", "sam2-base-plus", "sam2-large"]),
                one("birefnet", &["birefnet-BiRefNet-matting"]),
            ]
            .into(),
            ffmpeg: true,
            sam2: [("small", "sam2-small"), ("base_plus", "sam2-base-plus"), ("large", "sam2-large")]
                .map(|(a, b)| (a.to_owned(), b.to_owned()))
                .into(),
            birefnet: [("Matting".to_owned(), "birefnet-BiRefNet-matting".to_owned())].into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(ready: bool) -> Value {
        let b = |id: &str| serde_json::json!({"ready": ready, "state": if ready { "present" } else { "missing" }, "paths": {id: format!("/m/{id}")}});
        serde_json::json!({
            "models": {"key": {"corridorkey-weights": b("corridorkey-weights")}, "gvm": {"gvm": b("gvm")},
                       "matanyone2": {"matanyone2": b("matanyone2")}, "videomama": {"videomama": b("videomama")},
                       "sam2": {"sam2-small": b("sam2-small")}, "birefnet": {"birefnet-BiRefNet-matting": b("birefnet-BiRefNet-matting")}},
            "ffmpeg": true, "sam2": {"small": "sam2-small"}, "birefnet": {"Matting": "birefnet-BiRefNet-matting"}})
    }

    #[test]
    fn the_snapshot_maps_every_task_and_variant_to_its_exact_files() {
        use crate::tasks::TaskKind as K;
        let s = Snapshot::from_json(&doc(true)).unwrap();
        let p = |k: &str, v: &str| serde_json::json!({k: v});
        assert_eq!(s.key_files().unwrap()["corridorkey-weights"], "/m/corridorkey-weights");
        assert_eq!(s.task_files(K::Gvm, &Value::Null).unwrap()["gvm"], "/m/gvm");
        assert_eq!(s.task_files(K::Matanyone2, &Value::Null).unwrap()["matanyone2"], "/m/matanyone2");
        assert_eq!(s.task_files(K::Videomama, &Value::Null).unwrap()["videomama"], "/m/videomama");
        assert_eq!(s.task_files(K::SamTrack, &p("tracker_model", "small")).unwrap()["sam2-small"], "/m/sam2-small");
        assert_eq!(s.task_files(K::SamPreview, &p("tracker_model", "small")).unwrap()["sam2-small"], "/m/sam2-small");
        assert_eq!(s.task_files(K::Birefnet, &p("variant", "Matting")).unwrap()["birefnet-BiRefNet-matting"], "/m/birefnet-BiRefNet-matting");
        assert!(s.task_files(K::Birefnet, &p("variant", "Nope")).is_err());
        assert!(s.task_files(K::SamTrack, &p("tracker_model", "large")).is_err(), "a variant the snapshot does not know");
        for k in [K::Chroma, K::PrematteHint, K::ExportVideo, K::PrepareSource, K::ImportHint] {
            assert!(s.task_files(k, &Value::Null).unwrap().is_empty(), "{k:?} loads no model");
        }
    }

    #[test]
    fn a_missing_model_is_refused_with_the_apps_reason_and_buttons_follow() {
        use crate::tasks::TaskKind as K;
        let s = Snapshot::from_json(&doc(false)).unwrap();
        let why = s.task_files(K::Gvm, &Value::Null).unwrap_err();
        assert_eq!(why, "GVM is missing — download it on the Models page");
        assert!(s.key_files().is_err());
        let c = s.capabilities(true);
        assert!(!c.key && !c.tasks["gvm"] && !c.tasks["birefnet"] && c.birefnet_variants.is_empty() && c.sam_models.is_empty());
        assert!(c.tasks["chroma"] && c.tasks["prematte_hint"] && c.tasks["export_video"], "no model needed");
        let ready = Snapshot::from_json(&doc(true)).unwrap().capabilities(true);
        assert!(ready.key && ready.tasks["gvm"] && ready.tasks["sam_track"] && ready.birefnet_variants == ["Matting"]);
        // No GPU: GPU routes are off with the reason, CPU/FFmpeg routes stay.
        let nogpu = Snapshot::from_json(&doc(true)).unwrap().capabilities(false);
        assert!(!nogpu.key && !nogpu.tasks["gvm"] && nogpu.tasks["export_video"] && nogpu.task_unavailable.contains_key("gpu"));
    }

    #[test]
    fn key_runs_with_every_hint_model_missing_and_a_missing_gvm_disables_only_gvm() {
        use crate::tasks::TaskKind as K;
        let mut d = doc(true);
        for t in ["gvm", "matanyone2", "videomama", "sam2", "birefnet"] {
            for b in d["models"][t].as_object_mut().unwrap().values_mut() {
                b["ready"] = false.into();
                b["state"] = "missing".into();
            }
        }
        let c = Snapshot::from_json(&d).unwrap().capabilities(true);
        assert!(c.key && c.tasks["chroma"] && c.tasks["export_video"]);
        assert!(!c.tasks["gvm"] && !c.tasks["matanyone2"] && !c.tasks["videomama"] && !c.tasks["sam_track"] && !c.tasks["birefnet"]);
        let mut d = doc(true);
        d["models"]["gvm"]["gvm"]["ready"] = false.into();
        let s = Snapshot::from_json(&d).unwrap();
        assert_eq!(s.task_files(K::Gvm, &Value::Null).unwrap_err(), "GVM is missing \u{2014} download it on the Models page");
        let c = s.capabilities(true);
        assert!(c.key && !c.tasks["gvm"] && c.tasks["matanyone2"] && c.tasks["videomama"] && c.tasks["sam_track"] && c.tasks["birefnet"]);
    }

    #[test]
    fn a_malformed_snapshot_is_not_taken() {
        assert!(Snapshot::from_json(&serde_json::json!({"models": 5})).is_none());
        assert!(Snapshot::from_json(&serde_json::json!({"models": {"key": {"x": {"state": "missing"}}}})).is_none());
    }


    fn tmp(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("ck-models-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn actions_use_the_helper_cli_contract() {
        let roots = RootArgs(vec![
            ("--v2-models-dir".into(), "/same/models".into()),
            ("--data-dir".into(), "/same/data".into()),
            ("--hf-cache".into(), "/same/hf-cache".into()),
        ]);
        let runtime = Runtime {
            repo: PathBuf::from("/repo"),
            python: PathBuf::from("/python"),
            helper: PathBuf::from("/repo/scripts/setup_models.py"),
            install_env: None,
            scrub_python: false,
            roots,
        };
        let operation_id = crate::workflow::new_id();
        assert!(valid_operation_id(&operation_id));
        for action in [
            Action::Catalog,
            Action::Check,
            Action::VerifyAll,
            Action::Verify("gvm".into()),
            Action::Download("gvm".into()),
            Action::DownloadProfile(Profile::Full),
            Action::Repair("gvm".into()),
            Action::Cancel,
        ] {
            let operation_id = action
                .requires_operation_id()
                .then_some(operation_id.as_str());
            let args = runtime
                .command_args_with_operation_id(&action, operation_id)
                .unwrap();
            for (flag, path) in [
                ("--v2-models-dir", "/same/models"),
                ("--data-dir", "/same/data"),
                ("--hf-cache", "/same/hf-cache"),
            ] {
                assert!(
                    args.windows(2).any(|w| w[0] == flag && w[1] == path),
                    "{action:?}: {args:?}"
                );
            }
            assert!(
                !args.iter().any(|arg| arg == "--cache-dir"),
                "cache policy comes from CK_INSTALL"
            );
            if action.requires_operation_id() {
                assert!(args
                    .windows(2)
                    .any(|w| { w[0] == "--operation-id" && w[1] == operation_id.unwrap() }));
            }
        }
        assert!(runtime
            .command_args(&Action::Check)
            .unwrap()
            .windows(2)
            .any(|w| w[0] == "--check" && w[1] == "--json"));
        assert!(runtime
            .command_args(&Action::Download("gvm".into()))
            .is_err());
        assert!(runtime
            .command_args_with_operation_id(&Action::Download("gvm".into()), Some("bad/id"))
            .is_err());
        assert!(Action::DownloadProfile(Profile::Required)
            .arguments()
            .windows(2)
            .any(|w| w[0] == "--profile" && w[1] == "required"));
    }

    #[test]
    fn operation_id_validation_matches_the_helper_contract() {
        assert!(valid_operation_id("A"));
        assert!(valid_operation_id(&format!("a{}", "_-".repeat(63))));
        assert!(!valid_operation_id(""));
        assert!(!valid_operation_id("_starts-with-punctuation"));
        assert!(!valid_operation_id("bad/id"));
        assert!(!valid_operation_id(&"a".repeat(129)));
    }

    #[cfg(unix)]
    #[test]
    fn fake_helper_drains_both_streams_and_reports_real_exit() {
        use std::os::unix::fs::PermissionsExt;
        let root = tmp("fake-helper");
        let fake_python = root.join("fake-python");
        let helper = root.join("fake.py");
        std::fs::write(&helper, "").unwrap();
        std::fs::write(&fake_python, "#!/bin/sh\nprintf '%s\\n' '{\"event\":\"progress\",\"artifact\":\"gvm\",\"bytes\":5,\"bytes_total\":10}'\nfor i in $(seq 1 25); do printf 'stderr-%s\\n' \"$i\" >&2; done\nprintf '%s\\n' '{\"event\":\"op-done\",\"op\":\"download\",\"ok\":false,\"cancelled\":true}'\nexit 7\n").unwrap();
        let mut permissions = std::fs::metadata(&fake_python).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_python, permissions).unwrap();
        let mut command = Command::new(&fake_python);
        command.arg(helper);
        let job = Job::spawn(command, &fake_python).unwrap();
        let mut events = Vec::new();
        loop {
            match job
                .rx
                .recv_timeout(std::time::Duration::from_secs(3))
                .unwrap()
            {
                Event::Exited { code, stderr } => {
                    assert_eq!(code, Some(7));
                    assert!(stderr.contains("stderr-25"));
                    assert!(!stderr.contains("stderr-1\n"));
                    break;
                }
                e => events.push(e),
            }
        }
        assert!(
            matches!(&events[0], Event::Json(v) if string(v, "event").as_deref() == Some("progress") && number(v, "bytes_total") == Some(10))
        );
        assert!(
            matches!(events.last(), Some(Event::Json(v)) if string(v, "event").as_deref() == Some("op-done") && flag(v, "cancelled") == Some(true))
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn helper_exit_is_reported_when_descendant_keeps_pipes_open() {
        use std::os::unix::fs::PermissionsExt;

        struct KillPidFile(PathBuf);
        impl Drop for KillPidFile {
            fn drop(&mut self) {
                if let Ok(pid) = std::fs::read_to_string(&self.0) {
                    let _ = Command::new("kill").args(["-TERM", pid.trim()]).status();
                }
            }
        }

        let root = tmp("lingering-descendant");
        let fake_python = root.join("fake-python");
        let helper = root.join("fake.py");
        let pid_file = root.join("pipe-holder.pid");
        let cleanup = KillPidFile(pid_file.clone());
        std::fs::write(&helper, "").unwrap();
        std::fs::write(
            &fake_python,
            "#!/bin/sh\nsleep 30 &\necho $! > \"$PIPE_HOLDER_PID_FILE\"\nprintf '%s\\n' '{\"event\":\"op-done\",\"op\":\"check\",\"ok\":false}'\nexit 0\n",
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&fake_python).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&fake_python, permissions).unwrap();
        let mut command = Command::new(&fake_python);
        command.arg(helper).env("PIPE_HOLDER_PID_FILE", &pid_file);
        let job = Job::spawn(command, &fake_python).unwrap();
        let started = std::time::Instant::now();
        loop {
            match job
                .rx
                .recv_timeout(std::time::Duration::from_secs(4))
                .unwrap()
            {
                Event::Exited { code, .. } => {
                    assert_eq!(code, Some(0));
                    assert!(started.elapsed() < std::time::Duration::from_secs(3));
                    break;
                }
                _ => {}
            }
        }
        assert!(std::fs::read_to_string(&pid_file).is_ok());
        drop(cleanup);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn installed_helper_uses_explicit_roots_for_status_and_fixture_active_cancel() {
        use std::thread;
        use std::time::Instant;

        let root = tmp("descriptor-roots");
        let app = root.join("install");
        std::fs::create_dir_all(&app).unwrap();
        let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .to_path_buf();
        let models = root.join("models");
        let cache = root.join("cache");
        let user_data = root.join("user-data");
        let python = repo.join(".venv/bin/python");
        assert!(
            python.is_file(),
            "the offline helper contract test needs the checkout Python runtime"
        );
        let descriptor = app.join(crate::runtime::DESCRIPTOR_NAME);
        std::fs::write(
            &descriptor,
            serde_json::json!({"schema":1,"roots":{
                "python":python,"helper":repo,"models":models,"cache":cache,
                "scratch":root.join("scratch"),"user_data":user_data
            }})
            .to_string(),
        )
        .unwrap();
        let install = crate::runtime::load(&descriptor).unwrap();
        let launch = crate::runtime::Launch {
            repo: Some(install.helper.clone()),
            python: Some(install.python.clone()),
            install: Some(install.clone()),
            problem: None,
        };
        let expected_models = std::env::var_os("CORRIDORKEY_V2_MODELS_DIR")
            .filter(|p| !p.is_empty())
            .map(PathBuf::from)
            .or_else(|| install.models.clone())
            .unwrap();
        let hf_cache = root.join("hf-cache");
        let cache_root = install.cache.as_ref().unwrap().join("model-provision");
        let runtime = Runtime {
            repo: launch.repo.as_ref().unwrap().clone(),
            python: launch.python.clone().unwrap(),
            helper: launch
                .repo
                .as_ref()
                .unwrap()
                .join("scripts/setup_models.py"),
            install_env: Some((crate::runtime::ENV_INSTALL, install.descriptor.clone())),
            scrub_python: true,
            roots: RootArgs(vec![
                (
                    "--data-dir".into(),
                    install.user_data.as_ref().unwrap().as_os_str().into(),
                ),
                ("--v2-models-dir".into(), expected_models.as_os_str().into()),
                ("--hf-cache".into(), hf_cache.as_os_str().into()),
                ("--cache-dir".into(), cache_root.as_os_str().into()),
            ]),
        };
        let operation_id = "descriptor-active-download-123";
        let old_asset = user_data.join("fixture/weights/weights.bin");
        std::fs::create_dir_all(old_asset.parent().unwrap()).unwrap();
        std::fs::write(&old_asset, b"old-fixture-bytes").unwrap();
        let marker = cache_root
            .join("model-cancel")
            .join(format!("{operation_id}.cancel"));

        for action in [
            Action::Catalog,
            Action::Check,
            Action::VerifyAll,
            Action::DownloadProfile(Profile::Required),
            Action::Repair("gvm".into()),
            Action::Cancel,
        ] {
            let operation_id = action
                .requires_operation_id()
                .then_some("descriptor-op-123");
            let args = runtime
                .command_args_with_operation_id(&action, operation_id)
                .unwrap();
            for (flag, expected) in [
                ("--data-dir", install.user_data.as_ref().unwrap()),
                ("--v2-models-dir", &expected_models),
                ("--hf-cache", &hf_cache),
                ("--cache-dir", &cache_root),
            ] {
                assert!(
                    args.windows(2)
                        .any(|w| w[0] == flag && Path::new(&w[1]) == expected),
                    "{action:?}: {args:?}"
                );
            }
        }

        let check = runtime.spawn(&Action::Check).unwrap();
        let mut events = Vec::new();
        let (check_exit, check_stderr) = loop {
            match check
                .rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .unwrap()
            {
                Event::Json(v) => events.push(v),
                Event::Exited { code, stderr } => break (code, stderr),
                Event::Malformed(e) => panic!("helper emitted invalid JSONL: {e}"),
            }
        };
        assert_eq!(
            check_exit,
            Some(0),
            "stderr={check_stderr}; events={events:?}"
        );
        assert!(events.iter().any(
            |v| v.get("event").and_then(Value::as_str) == Some("op-done")
                && v.get("op").and_then(Value::as_str) == Some("check")
                && flag(v, "ok") == Some(false)
        ));
        let weights = events
            .iter()
            .find(|v| v.get("artifact").and_then(Value::as_str) == Some("corridorkey-weights"))
            .unwrap();
        assert_eq!(flag(weights, "present"), Some(false));
        assert_eq!(flag(weights, "verified"), Some(false));
        assert_eq!(flag(weights, "ready"), Some(false));
        assert!(events
            .iter()
            .any(
                |v| v.get("artifact").and_then(Value::as_str) == Some("corridorkey-weights")
                    && v.get("path").and_then(Value::as_str)
                        == Some(expected_models.join("weights").to_string_lossy().as_ref())
            ));
        assert!(events
            .iter()
            .any(
                |v| v.get("artifact").and_then(Value::as_str) == Some("birefnet-BiRefNet")
                    && v.get("path").and_then(Value::as_str)
                        == Some(
                            user_data
                                .join("modules/BiRefNetModule/checkpoints/BiRefNet")
                                .to_string_lossy()
                                .as_ref()
                        )
            ));

        let ready_file = cache_root.join("model-provision-fixture.ready");
        let mut fixture_runtime = runtime.clone();
        fixture_runtime.helper = repo.join("tests/fixtures/model_provision_cli.py");
        let download_args = fixture_runtime
            .command_args_with_operation_id(
                &Action::DownloadProfile(Profile::Required),
                Some(operation_id),
            )
            .unwrap();
        let mut download = Command::new(&python)
            .current_dir(&repo)
            .args(download_args)
            .env(crate::runtime::ENV_INSTALL, &install.descriptor)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(4);
        while !ready_file.is_file() && Instant::now() < deadline {
            if download.try_wait().unwrap().is_some() {
                break;
            }
            thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(
            ready_file.is_file(),
            "download helper never reached the local transport gate"
        );

        let cancel = fixture_runtime
            .spawn_with_operation_id(&Action::Cancel, Some(operation_id))
            .unwrap();
        let mut cancel_events = Vec::new();
        let cancel_exit = loop {
            match cancel
                .rx
                .recv_timeout(std::time::Duration::from_secs(8))
                .unwrap()
            {
                Event::Json(v) => cancel_events.push(v),
                Event::Exited { code, .. } => break code,
                Event::Malformed(e) => panic!("cancel emitted invalid JSONL: {e}"),
            }
        };
        assert_eq!(cancel_exit, Some(0));
        assert_eq!(
            cancel_events
                .iter()
                .filter_map(|v| string(v, "event"))
                .collect::<Vec<_>>(),
            ["op-start", "cancel-requested", "op-done"]
        );
        assert!(cancel_events
            .iter()
            .all(|v| string(v, "operation_id").as_deref() == Some(operation_id)));
        let request_id = string(&cancel_events[0], "request_id").unwrap();
        assert!(!request_id.is_empty());
        assert!(cancel_events
            .iter()
            .all(|v| string(v, "request_id").as_deref() == Some(&request_id)));
        let terminal = cancel_events.last().unwrap();
        assert_eq!(flag(terminal, "ok"), Some(true));
        assert_eq!(flag(terminal, "requested"), Some(true));
        let download_output = download.wait_with_output().unwrap();
        let download_stdout = String::from_utf8(download_output.stdout).unwrap();
        let download_events: Vec<Value> = download_stdout
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect();
        assert_eq!(
            download_output.status.code(),
            Some(1),
            "stderr={}; stdout={download_stdout}",
            String::from_utf8_lossy(&download_output.stderr)
        );
        assert!(download_events
            .iter()
            .all(|v| string(v, "operation_id").as_deref() == Some(operation_id)));
        assert!(download_events.iter().any(|v| {
            string(v, "event").as_deref() == Some("cancelled")
                && string(v, "op").as_deref() == Some("download")
        }));
        let terminal = download_events
            .iter()
            .find(|v| string(v, "event").as_deref() == Some("op-done"))
            .unwrap();
        assert_eq!(string(terminal, "op").as_deref(), Some("download"));
        assert_eq!(flag(terminal, "ok"), Some(false));
        assert_eq!(flag(terminal, "cancelled"), Some(true));
        assert_eq!(std::fs::read(&old_asset).unwrap(), b"old-fixture-bytes");
        assert!(!cache_root
            .join("model-operations")
            .join(format!("{operation_id}.active"))
            .exists());
        assert!(!marker.exists());
        let _ = std::fs::remove_dir_all(root);
    }
}
