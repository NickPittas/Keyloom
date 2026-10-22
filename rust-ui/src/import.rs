//! Import bridge: runs `tools/ck_import.py` (which reuses the backend project/FFmpeg code) and turns its
//! JSON lines into events. No UI types. The job keeps the helper's stdin open: closing it means "cancel".
use serde_json::Value;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, TryRecvError};

#[derive(Clone, Debug, PartialEq)]
pub struct PlanItem {
    pub source: String,
    pub kind: String,
    pub ok: bool,
    pub clip: Option<String>,
    pub renamed_from: Option<String>,
    pub reason: Option<String>,
    /// "320×180 · 24 fps · 48 frames" etc., only from what the probe really returned.
    pub detail: String,
    /// (width, height, fps, frame count) exactly as probed; `None` for sequences / failed probes.
    pub probed: Option<(u32, u32, f64, Option<u64>)>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Plan(Vec<PlanItem>),
    Stage(String),
    Created {
        project: PathBuf,
        clips: Vec<(String, PathBuf, String)>,
    },
    Progress {
        clip: String,
        phase: String,
        done: u64,
        total: u64,
    },
    ClipDone {
        clip: String,
        frames: u64,
    },
    ClipError {
        clip: String,
        message: String,
    },
    Cancelled,
    Finished,
    Error(String),
    /// Helper process ended; `stderr` is its last lines. Always the last event.
    Exited {
        code: Option<i32>,
        stderr: String,
    },
}

pub fn parse_event(line: &str) -> Option<Event> {
    let v: Value = serde_json::from_str(line).ok()?;
    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
    let n = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    Some(match v.get("event")?.as_str()? {
        "plan" => Event::Plan(
            v.get("items")?
                .as_array()?
                .iter()
                .map(|i| {
                    let g = |k: &str| i.get(k).and_then(Value::as_str).map(str::to_owned);
                    let info = i.get("info");
                    let num = |k: &str| info.and_then(|x| x.get(k)).and_then(Value::as_f64);
                    let mut d = vec![];
                    if let (Some(w), Some(h)) = (num("width"), num("height")) {
                        d.push(format!("{w:.0}×{h:.0}"));
                    }
                    if let Some(f) = num("fps") {
                        d.push(format!("{f:.3} fps"));
                    }
                    if let Some(c) = num("frame_count").filter(|c| *c > 0.0) {
                        d.push(format!("{c:.0} frames"));
                    }
                    PlanItem {
                        source: g("source").unwrap_or_default(),
                        kind: g("kind").unwrap_or_default(),
                        ok: g("status").as_deref() == Some("ok"),
                        clip: g("clip"),
                        renamed_from: g("renamed_from"),
                        reason: g("reason"),
                        detail: d.join(" · "),
                        probed: match (num("width"), num("height"), num("fps")) {
                            (Some(w), Some(h), Some(f)) => Some((
                                w as u32,
                                h as u32,
                                f,
                                num("frame_count").filter(|c| *c > 0.0).map(|c| c as u64),
                            )),
                            _ => None,
                        },
                    }
                })
                .collect(),
        ),
        "stage" => Event::Stage(s("message")),
        "created" => Event::Created {
            project: s("project").into(),
            clips: v
                .get("clips")?
                .as_array()?
                .iter()
                .map(|c| {
                    let g = |k: &str| c.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
                    (g("name"), g("root").into(), g("kind"))
                })
                .collect(),
        },
        "progress" => Event::Progress {
            clip: s("clip"),
            phase: s("phase"),
            done: n("done"),
            total: n("total"),
        },
        "clip_done" => Event::ClipDone {
            clip: s("clip"),
            frames: n("frames"),
        },
        "clip_error" => Event::ClipError {
            clip: s("clip"),
            message: s("message"),
        },
        "cancelled" => Event::Cancelled,
        "finished" => Event::Finished,
        "error" => Event::Error(s("message")),
        _ => return None,
    })
}

#[derive(Clone, Debug)]
pub enum Request {
    New {
        name: String,
        data_dir: PathBuf,
    },
    /// New project folder created literally inside `parent` (no `Projects` level is added).
    NewIn {
        name: String,
        parent: PathBuf,
    },
    Add {
        project: PathBuf,
    },
    /// Plan the sources with no destination at all (always a dry run; nothing can be written).
    Probe,
    /// Re-run extraction for one clip folder (interrupted or failed import).
    Resume {
        clip_root: PathBuf,
    },
}

#[derive(Clone, Debug, Default)]
pub struct Sources {
    pub videos: Vec<PathBuf>,
    pub folders: Vec<PathBuf>,
}

pub fn python(repo: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("CK_PYTHON") {
        return p.into();
    }
    if cfg!(windows) {
        repo.join(".venv/Scripts/python.exe")
    } else {
        repo.join(".venv/bin/python")
    }
}

pub struct Job {
    rx: Receiver<Event>,
    child: Child,
    stdin: Option<ChildStdin>,
    pub cancel_sent: bool,
}

impl Job {
    /// `dry_run` only probes and plans (no writes). Errors name the missing python/helper path.
    /// No build-time default: the explicit `repo` argument, then `CK_REPO`, then the install
    /// descriptor, then developer checkout discovery (`crate::runtime::launch`).
    pub fn spawn(
        req: &Request,
        src: &Sources,
        dry_run: bool,
        repo: Option<&Path>,
    ) -> Result<Job, String> {
        let launch = match repo {
            Some(r) => crate::runtime::launch_explicit(r),
            None => crate::runtime::launch(None),
        };
        let Some(repo) = launch.repo else {
            return Err(launch.problem.map(|p| format!("The installed runtime is unusable: {p}")).unwrap_or_else(|| {
                "No CorridorKey runtime was found: pass --repo, set CK_REPO or CK_INSTALL, install the packaged runtime, or run from a checkout.".into()
            }));
        };
        let helper = repo.join("rust-ui/tools/ck_import.py");
        let py = std::env::var_os("CK_PYTHON")
            .map(PathBuf::from)
            .or_else(|| launch.python.clone())
            .unwrap_or_else(|| python(&repo));
        if !py.is_file() {
            return Err(
                if launch.python.is_some() && std::env::var_os("CK_PYTHON").is_none() {
                    format!(
                    "Installed Python is missing at {}. Repair the installation, or set CK_PYTHON.",
                    py.display()
                )
                } else {
                    format!(
                        "Python not found at {} (set CK_PYTHON or create the repo .venv)",
                        py.display()
                    )
                },
            );
        }
        if !helper.is_file() {
            return Err(format!("Import helper not found at {}", helper.display()));
        }
        // A destination is never defaulted: an empty one is refused here, and again by the helper.
        let empty = match req {
            Request::New { data_dir: p, .. }
            | Request::NewIn { parent: p, .. }
            | Request::Add { project: p }
            | Request::Resume { clip_root: p } => p.as_os_str().is_empty(),
            Request::Probe => false,
        };
        if empty {
            return Err("No destination folder was given; choose one explicitly.".into());
        }
        let mut c = Command::new(&py);
        c.arg("-I").arg(&helper);
        if dry_run || matches!(req, Request::Probe) {
            c.arg("--dry-run");
        }
        match req {
            Request::New { name, data_dir } => {
                c.arg("new")
                    .arg("--name")
                    .arg(name)
                    .arg("--data-dir")
                    .arg(data_dir);
            }
            Request::NewIn { name, parent } => {
                c.arg("new")
                    .arg("--name")
                    .arg(name)
                    .arg("--parent-dir")
                    .arg(parent);
            }
            Request::Add { project } => {
                c.arg("add").arg("--project").arg(project);
            }
            Request::Probe => {
                c.arg("probe");
            }
            Request::Resume { clip_root } => {
                c.arg("extract").arg("--clip-root").arg(clip_root);
            }
        }
        // `=` form: a path starting with `-` can never be read as an option.
        for v in &src.videos {
            c.arg(format!("--video={}", v.display()));
        }
        for f in &src.folders {
            c.arg(format!("--folder={}", f.display()));
        }
        crate::runtime::prepare_python_command(&mut c, launch.install.as_ref());
        let mut child = c
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("Could not start {}: {e}", py.display()))?;
        let stdin = child.stdin.take();
        let out = child.stdout.take().unwrap();
        let err = child.stderr.take().unwrap();
        let (tx, rx) = channel();
        // stderr is drained on its own thread so a chatty FFmpeg can never fill the pipe and stall the helper.
        let errt = std::thread::spawn(move || {
            let mut tail: Vec<String> = vec![];
            for l in BufReader::new(err).lines().map_while(Result::ok) {
                tail.push(l);
                if tail.len() > 20 {
                    tail.remove(0);
                }
            }
            tail.join("\n")
        });
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if let Some(e) = parse_event(&l) {
                    let _ = tx.send(e);
                }
            }
            let stderr = errt.join().unwrap_or_default();
            let _ = tx.send(Event::Exited { code: None, stderr });
        });
        Ok(Job {
            rx,
            child,
            stdin,
            cancel_sent: false,
        })
    }

    /// Ask the helper to stop after its current step. Never kills: a copy or metadata write in flight completes.
    pub fn cancel(&mut self) {
        if let Some(s) = self.stdin.as_mut() {
            let _ = s.write_all(b"cancel\n");
            let _ = s.flush();
        }
        self.cancel_sent = true;
    }

    pub fn try_recv(&mut self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(Event::Exited { stderr, .. }) => {
                let code = self.child.wait().ok().and_then(|s| s.code());
                Some(Event::Exited { code, stderr })
            }
            Ok(e) => Some(e),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel();
        self.stdin = None; // EOF is also a cancel for the helper
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_events() {
        assert_eq!(
            parse_event(r#"{"event":"progress","clip":"a","phase":"extract","done":3,"total":9}"#),
            Some(Event::Progress {
                clip: "a".into(),
                phase: "extract".into(),
                done: 3,
                total: 9
            })
        );
        assert_eq!(parse_event("garbage"), None);
        let Some(Event::Plan(p)) = parse_event(
            r#"{"event":"plan","items":[{"source":"/x/a.mp4","kind":"video","status":"ok","clip":"a_2","renamed_from":"a","info":{"fps":24.0,"width":320,"height":180,"frame_count":48}},{"source":"/x/b","kind":"sequence","status":"skip","reason":"nope"}]}"#,
        ) else {
            panic!()
        };
        assert!(p[0].ok && p[0].detail == "320×180 · 24.000 fps · 48 frames");
        assert_eq!(p[0].renamed_from.as_deref(), Some("a"));
        assert!(!p[1].ok && p[1].reason.as_deref() == Some("nope"));
    }
}
