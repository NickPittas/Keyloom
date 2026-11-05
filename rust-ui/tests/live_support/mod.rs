//! Shared helpers of the live suite (`tests/live.rs`). Everything here drives the REAL App (headless egui, synthetic clicks), the REAL
//! worker, the REAL GPU and the REAL models (read-only) on a temporary copy of a real clip. Only `tools/live-test.sh` sets the
//! environment these read; do not call them from ordinary tests.
#![allow(dead_code)]
use ck::scheduler::{JobState, TargetStatus};
use ck::ui::{App, AppOptions, Tab};
use eframe::egui::{self, epaint::Shape, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

/// Every live test starts here: `#[ignore]` keeps `cargo test` away, this keeps `--ignored` from running one without the runner's env.
pub fn require() {
    budget_end(); // starts the clock
    assert_eq!(std::env::var("KEYLOOM_LIVE").as_deref(), Ok("1"), "live test: run it with `bash rust-ui/tools/live-test.sh <name>` (sets KEYLOOM_LIVE=1 and the runtime/models env)");
}

/// One line of evidence; the runner prints these in its PASS/FAIL report.
pub fn ev(s: impl AsRef<str>) {
    println!("EVIDENCE {}", s.as_ref());
}

pub fn env(k: &str) -> PathBuf {
    PathBuf::from(std::env::var_os(k).unwrap_or_else(|| panic!("{k} is not set (tools/live-test.sh sets it)")))
}

/// Frames per temporary clip: `dflt` or `KEYLOOM_LIVE_FRAMES`, never more than 6 (each test must fit the 60 s limit).
pub fn frames_or(dflt: usize) -> usize {
    std::env::var("KEYLOOM_LIVE_FRAMES").ok().and_then(|v| v.parse().ok()).unwrap_or(dflt).min(6)
}

/// Every wait ends by this time so a slow step FAILs with its name before run_monitored's 60 s deadline kills the test.
const BUDGET_SECS: u64 = 52;
fn budget_end() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now) + Duration::from_secs(BUDGET_SECS)
}

// ------------------------------------------------------------------ processes and locks

/// `pid args` lines of the running AI workers (the sandbox shows only its own processes).
pub fn workers() -> Vec<String> {
    let o = Command::new("/usr/bin/ps").args(["-eo", "pid,args"]).output().unwrap();
    String::from_utf8_lossy(&o.stdout).lines().filter(|l| l.contains("backend.worker")).map(|l| l.trim().to_string()).collect()
}
pub fn pids() -> Vec<u32> {
    workers().iter().filter_map(|l| l.split_whitespace().next()?.parse().ok()).collect()
}
pub fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists() && std::fs::read_to_string(format!("/proc/{pid}/stat")).map(|s| !s.contains(") Z ")).unwrap_or(false)
}
/// Entries of the cross-process model lock folder (`backend/model_lock.py`: `<tmp>/keyloom-model-locks`).
pub fn locks() -> Vec<String> {
    let d = std::env::temp_dir().join("keyloom-model-locks");
    std::fs::read_dir(d).map(|r| r.flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect()).unwrap_or_default()
}
pub fn count_files(dir: &Path) -> usize {
    fn walk(d: &Path) -> usize {
        std::fs::read_dir(d)
            .map(|r| r.flatten().map(|e| if e.path().is_dir() { walk(&e.path()) } else { usize::from(!e.file_name().to_string_lossy().starts_with('.')) }).sum())
            .unwrap_or(0)
    }
    walk(dir)
}
pub fn kill9(pid: u32) {
    Command::new("/usr/bin/kill").args(["-9", &pid.to_string()]).status().unwrap();
}

// ------------------------------------------------------------------ the temporary project

pub struct Live {
    pub base: PathBuf,
    pub proj: PathBuf,
    pub settings: PathBuf,
    pub wlog: PathBuf,
    pub py: PathBuf,
    pub helper: PathBuf,
    pub frames: usize,
    owner: bool,
}

impl Live {
    /// A fresh project under `KEYLOOM_LIVE_TMP` with `clips` (each the first `frames` frames of the real clip), the install descriptor
    /// (so the App finds the installed runtime, the repo's Python as helper and the real models folder), a worker-stdin logging wrapper
    /// and an empty settings file. `CK_INSTALL` is exported for this process and its children.
    pub fn new(tag: &str, frames: usize, clips: &[&str]) -> Live {
        require();
        let base = env("KEYLOOM_LIVE_TMP").join(tag);
        let _ = std::fs::remove_dir_all(&base);
        let (proj, wlog, inst) = (base.join("proj"), base.join("wlog"), base.join("install"));
        for d in [&proj, &wlog, &inst, &base.join("cache/model-provision"), &base.join("scratch")] {
            std::fs::create_dir_all(d).unwrap();
        }
        let (py, helper) = (env("KEYLOOM_LIVE_PYTHON"), env("KEYLOOM_LIVE_HELPER"));
        let src = env("KEYLOOM_LIVE_CLIP");
        let mut names: Vec<_> = std::fs::read_dir(src.join("Frames")).unwrap().flatten().map(|e| e.path()).filter(|p| !p.file_name().unwrap().to_string_lossy().starts_with('.')).collect(); // not `.dwab_done`
        names.sort();
        assert!(names.len() >= frames, "the real clip has {} frames, {frames} wanted", names.len());
        let mut meta: Value = serde_json::from_str(&std::fs::read_to_string(src.join(".video_metadata.json")).unwrap()).unwrap();
        meta["frame_count"] = json!(frames);
        for c in clips {
            let cd = proj.join("clips").join(c);
            std::fs::create_dir_all(cd.join("Frames")).unwrap();
            for f in &names[..frames] {
                std::fs::copy(f, cd.join("Frames").join(f.file_name().unwrap())).unwrap();
            }
            std::fs::write(cd.join(".video_metadata.json"), meta.to_string()).unwrap();
            std::fs::write(cd.join("clip.json"), "{}").unwrap();
        }
        std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
        // The worker's python is a wrapper that tees everything the app sends on stdin (the `model_files` evidence).
        let wrap = base.join("pywrap.sh");
        std::fs::write(
            &wrap,
            format!("#!/bin/bash\ncase \"$*\" in\n *backend.worker*) exec {py:?} \"$@\" < <(/usr/bin/tee -a {wl:?}/stdin-$$.log);;\n *) exec {py:?} \"$@\";;\nesac\n", wl = wlog),
        )
        .unwrap();
        Command::new("/usr/bin/chmod").arg("+x").arg(&wrap).status().unwrap();
        std::fs::write(
            inst.join("keyloom-install.json"),
            json!({"schema":1,"app_name":"Keyloom","app_version":"2.5.0","roots":{"python":wrap,"helper":helper,"models":null,"cache":null,"scratch":base.join("scratch"),"user_data":null}}).to_string(),
        )
        .unwrap();
        let settings = base.join("settings.json");
        std::fs::write(&settings, r#"{"version":1}"#).unwrap();
        std::env::set_var("CK_INSTALL", &inst);
        Live { base, proj, settings, wlog, py, helper, frames, owner: true }
    }
    /// The same environment on another project folder (an imported one); does not delete the shared temp folder.
    pub fn for_project(&self, proj: PathBuf) -> Live {
        Live { base: self.base.clone(), proj, settings: self.settings.clone(), wlog: self.wlog.clone(), py: self.py.clone(), helper: self.helper.clone(), frames: 0, owner: false }
    }
    pub fn clip(&self, name: &str) -> PathBuf {
        self.proj.join("clips").join(name)
    }
    pub fn edit_clip_json(&self, name: &str, f: impl FnOnce(&mut Value)) {
        let p = self.clip(name).join("clip.json");
        let mut v: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
        f(&mut v);
        std::fs::write(p, v.to_string()).unwrap();
    }
    /// A prompt point in the middle of frame 0 for SAM.
    pub fn sam_prompt(&self, name: &str) {
        self.edit_clip_json(name, |v| v["ck_native"]["annotations"] = json!({"frames": {"frame_000000": {"points": [{"x": 1920.0, "y": 1080.0, "label": 1}]}}}));
    }
    /// Run the runtime's Python (OpenCV with EXR on) on a snippet; returns stdout+stderr.
    pub fn python(&self, code: &str, args: &[&str]) -> String {
        let o = Command::new(&self.py).env("OPENCV_IO_ENABLE_OPENEXR", "1").args(["-I", "-c", code]).args(args).output().unwrap();
        format!("{}{}", String::from_utf8_lossy(&o.stdout), String::from_utf8_lossy(&o.stderr))
    }
    /// Frames, size and mean of a mask folder (evidence only).
    pub fn stats(&self, dir: &Path) -> String {
        const SNIP: &str = "import sys,os,glob,cv2\nd=sys.argv[1]\nfs=sorted(f for f in glob.glob(d+'/**/*',recursive=True) if os.path.isfile(f) and not os.path.basename(f).startswith('.'))\nout='files=%d'%len(fs)\nif fs:\n im=cv2.imread(fs[len(fs)//2],cv2.IMREAD_UNCHANGED)\n if im is not None:\n  g=im if im.ndim==2 else im[...,-1] if im.shape[2]==4 else im.mean(axis=2)\n  out+=' %s shape=%s mean=%.3f'%(os.path.basename(fs[len(fs)//2]),im.shape,float(g.mean())/(255.0 if im.dtype.itemsize==1 else 1.0))\nprint(out)";
        self.python(SNIP, &[dir.to_str().unwrap()]).trim().replace('\n', " / ")
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // The runner removes the whole temp folder; this just keeps a manual run tidy.
        if self.owner {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }
}

// ------------------------------------------------------------------ the headless App

fn walk(s: &Shape, out: &mut Vec<(String, Rect)>) {
    match s {
        Shape::Text(t) => out.push((t.galley.text().to_string(), t.galley.rect.translate(t.pos.to_vec2()))),
        Shape::Vec(v) => v.iter().for_each(|x| walk(x, out)),
        _ => {}
    }
}

pub struct Drv {
    pub ctx: egui::Context,
    pub app: App,
    pub texts: Vec<(String, Rect)>,
    pub t0: Instant,
}

pub fn state_text(s: &JobState) -> String {
    match s {
        JobState::Queued => "Queued".into(),
        JobState::Dispatching { .. } => "Dispatching".into(),
        JobState::Running { phase, done, total, .. } => format!("Running {phase} {done}/{total}"),
        JobState::Done { info, .. } => format!("Done {:?} {}", info.outcome, info.message.clone().unwrap_or_default()),
        o => format!("{o:?}").chars().take(80).collect(),
    }
}

impl Drv {
    /// The App as the window opens it: real model check pending, no worker started by this code (the app itself starts the idle one).
    pub fn new(live: &Live, project: bool, open: Option<&str>, tab: Option<Tab>) -> Drv {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, AppOptions { project: project.then(|| live.proj.clone()), settings_path: Some(live.settings.clone()), open_clip: open.map(Into::into), tab, ..Default::default() });
        app.ai_mut().forget_models(); // as App::new does for the real window: wait for the first REAL model check
        Drv { ctx, app, texts: vec![], t0: Instant::now() }
    }
    /// Project + clip open on `tab`, models checked, local worker Ready.
    pub fn open_clip(live: &Live, name: &str, tab: Tab) -> Drv {
        let mut d = Drv::new(live, true, Some(name), Some(tab));
        d.warm();
        assert!(d.pump(180, "clip loaded + worker ready + models known", |d| d.app.in_workspace() && d.app.open_frame_count().is_some() && d.ready() && !d.app.ai().models_unknown()), "clip did not open: {}", d.targets());
        d.frames(10);
        d.log(format!("opened {name}: frames={:?} targets={}", d.app.open_frame_count(), d.targets()));
        d
    }
    pub fn log(&self, s: impl AsRef<str>) {
        println!("[{:7.1}s] {}", self.t0.elapsed().as_secs_f32(), s.as_ref());
    }
    pub fn frame(&mut self, events: Vec<Event>) {
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 2400.0))), events, max_texture_side: Some(16384), ..Default::default() };
        let (ctx, app) = (&self.ctx, &mut self.app);
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
        self.texts.clear();
        for cs in &out.shapes {
            walk(&cs.shape, &mut self.texts);
        }
    }
    pub fn frames(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(vec![]);
        }
    }
    /// Draw frames until `done` or `secs` pass; false on timeout.
    pub fn pump(&mut self, secs: u64, what: &str, mut done: impl FnMut(&mut Drv) -> bool) -> bool {
        let end = (Instant::now() + Duration::from_secs(secs)).min(budget_end());
        loop {
            self.frame(vec![]);
            if done(self) {
                return true;
            }
            if Instant::now() > end {
                self.log(format!("TIMEOUT waiting for {what}"));
                return false;
            }
            std::thread::sleep(Duration::from_millis(15));
        }
    }
    /// The window starts the model Catalog -> Check on open (not reachable from tests); the Models page does the same on first draw.
    pub fn warm(&mut self) {
        self.frames(3);
        if !self.has("Models") {
            return;
        }
        self.click("Models");
        self.frames(5);
        self.pump(180, "catalog+check", |d| d.has("Models & runtime") && !d.has("Waiting for the local model catalog…") && !d.screen().iter().any(|s| s.contains("running…")));
        self.pump(60, "model snapshot", |d| !d.app.ai().models_unknown());
        // Back to the page the test came from: the Shot page tab when a clip is open, else Board (the page tabs sit below the menu bar's "Shot").
        let label = if self.app.in_workspace() { "Shot" } else { "Board" };
        self.page_tab(label);
    }
    /// Click the page tab `label` (the lowest drawn match: the menu bar above it also has "Shot").
    pub fn page_tab(&mut self, label: &str) {
        if let Some(r) = self.texts.iter().filter(|t| t.0 == label).max_by(|a, b| a.1.min.y.total_cmp(&b.1.min.y)).map(|t| t.1) {
            self.log(format!("CLICK page tab '{label}'"));
            self.click_at(r.center());
        }
    }
    pub fn has(&self, label: &str) -> bool {
        self.texts.iter().any(|t| t.0 == label)
    }
    pub fn has_containing(&self, part: &str) -> bool {
        self.texts.iter().any(|t| t.0.contains(part))
    }
    pub fn find_containing(&self, part: &str) -> Option<String> {
        self.texts.iter().find(|t| t.0.contains(part)).map(|t| t.0.clone())
    }
    pub fn click_at(&mut self, at: egui::Pos2) {
        let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.frame(vec![Event::PointerMoved(at)]);
        self.frame(vec![b(true)]);
        self.frame(vec![b(false)]);
        self.frames(3);
    }
    /// Click the drawn text `label` (exact), or the first text starting with `label` minus a trailing `*`.
    pub fn click(&mut self, label: &str) {
        let r = match label.strip_suffix('*') {
            Some(p) => self.texts.iter().find(|t| t.0.starts_with(p)),
            None => self.texts.iter().find(|t| t.0 == label),
        }
        .unwrap_or_else(|| panic!("button '{label}' is not drawn; screen: {:?}", self.screen()))
        .1;
        self.log(format!("CLICK '{label}'"));
        self.click_at(r.center());
    }
    pub fn press(&mut self, key: egui::Key) {
        let e = |pressed| Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE };
        self.frame(vec![e(true)]);
        self.frame(vec![e(false)]);
        self.frames(2);
    }
    pub fn screen(&self) -> Vec<String> {
        self.texts.iter().map(|t| t.0.clone()).filter(|s| !s.trim().is_empty() && s.len() < 160).collect()
    }
    pub fn tasks(&self) -> Vec<String> {
        self.app.ai().scheduler().task_snapshot().iter().map(|t| format!("{}:{}:{}", t.job.job_id, t.job.kind.wire(), state_text(&t.state))).collect()
    }
    pub fn keys(&self) -> Vec<String> {
        self.app.ai().scheduler().snapshot().iter().map(|t| format!("{}:{}", t.job.job_id, state_text(&t.state))).collect()
    }
    pub fn targets(&self) -> String {
        self.app.ai().scheduler().targets().iter().map(|t| format!("{} {:?} busy={:?}", t.id, t.status, t.busy_with)).collect::<Vec<_>>().join(" | ")
    }
    pub fn ready(&self) -> bool {
        self.app.ai().scheduler().targets().iter().any(|t| matches!(t.status, TargetStatus::Ready))
    }
    pub fn idle_ready(&self) -> bool {
        self.app.ai().scheduler().targets().iter().any(|t| matches!(t.status, TargetStatus::Ready) && t.busy_with.is_none())
    }
    pub fn all_ids(&self) -> Vec<String> {
        let mut v: Vec<String> = self.app.ai().scheduler().task_snapshot().iter().map(|t| t.job.job_id.clone()).collect();
        v.extend(self.app.ai().scheduler().snapshot().iter().map(|t| t.job.job_id.clone()));
        v
    }
    /// (job id, kind, state text, ended) of every task and Key job.
    pub fn jobs(&self) -> Vec<(String, String, String, bool)> {
        let mut v: Vec<_> = self.app.ai().scheduler().task_snapshot().iter().map(|t| (t.job.job_id.clone(), t.job.kind.wire().to_string(), state_text(&t.state), matches!(t.state, JobState::Done { .. }))).collect();
        v.extend(self.app.ai().scheduler().snapshot().iter().map(|t| (t.job.job_id.clone(), "key".to_string(), state_text(&t.state), matches!(t.state, JobState::Done { .. }))));
        v
    }
}

// ------------------------------------------------------------------ running one action

/// What a clicked action did.
pub struct JobRun {
    pub name: String,
    pub queued: bool,
    pub finished: bool,
    pub secs: f32,
    pub ids: Vec<String>,
    pub states: Vec<String>,
    pub pids_before: Vec<u32>,
    pub pids_after: Vec<u32>,
    /// `model_files` of every command sent to the worker for these jobs: (kind, object).
    pub model_files: Vec<(String, Value)>,
}

impl JobRun {
    /// Every job this click queued ended `Complete`.
    pub fn complete(&self) -> bool {
        self.queued && self.finished && !self.states.is_empty() && self.states.iter().all(|s| s.contains("Done Complete"))
    }
    pub fn assert_complete(&self) {
        assert!(self.complete(), "[{}] not complete: queued={} finished={} after {:.1}s; jobs={:?}", self.name, self.queued, self.finished, self.secs, self.states);
    }
    pub fn evidence(&self) {
        ev(format!("[{}] jobs {:?} finished in {:.1}s; worker pids {:?} -> {:?}", self.name, self.states, self.secs, self.pids_before, self.pids_after));
        for (k, m) in &self.model_files {
            ev(format!("[{}] model_files sent for {k}: {m}", self.name));
        }
    }
}

fn stdin_lines(wlog: &Path) -> Vec<String> {
    let mut v = vec![];
    for e in std::fs::read_dir(wlog).into_iter().flatten().flatten() {
        let n = e.file_name().to_string_lossy().to_string();
        for (i, l) in std::fs::read_to_string(e.path()).unwrap_or_default().lines().enumerate() {
            v.push(format!("{n}#{i}\t{l}"));
        }
    }
    v
}

fn find_model_files(v: &Value, ids: &[String], out: &mut Vec<(String, Value)>) {
    match v {
        Value::Object(m) => {
            if let Some(f) = m.get("model_files").filter(|f| f.as_object().is_some_and(|o| !o.is_empty())) {
                let id = m.get("job_id").and_then(Value::as_str).unwrap_or("");
                if ids.is_empty() || id.is_empty() || ids.iter().any(|i| i == id) {
                    let kind = m.get("kind").or(m.get("cmd")).and_then(Value::as_str).unwrap_or("?");
                    out.push((kind.to_string(), f.clone()));
                }
            }
            m.values().for_each(|x| find_model_files(x, ids, out));
        }
        Value::Array(a) => a.iter().for_each(|x| find_model_files(x, ids, out)),
        _ => {}
    }
}

/// Click the button `label` and wait up to `max` seconds for every job it queued to end. Records the worker pids and what was sent.
pub fn run_job(d: &mut Drv, live: &Live, name: &str, label: &str, max: u64) -> JobRun {
    let label = label.to_string();
    run_with(d, live, name, max, move |d| d.click(&label))
}

pub fn run_with(d: &mut Drv, live: &Live, name: &str, max: u64, click: impl FnOnce(&mut Drv)) -> JobRun {
    let known = d.all_ids();
    let before: std::collections::HashSet<String> = stdin_lines(&live.wlog).into_iter().collect();
    let pids_before = pids();
    let t = Instant::now();
    click(d);
    d.pump(15, "job queued", |d| d.all_ids().len() > known.len());
    let ids: Vec<String> = d.all_ids().into_iter().filter(|i| !known.contains(i)).collect();
    let mut r = JobRun { name: name.into(), queued: !ids.is_empty(), finished: false, secs: 0.0, ids: ids.clone(), states: vec![], pids_before, pids_after: vec![], model_files: vec![] };
    if ids.is_empty() {
        d.log(format!("[{name}] NO JOB QUEUED; notice={:?}; screen={:?}", d.app.ai().notice, d.screen()));
        return r;
    }
    let mine = move |d: &Drv| -> Vec<String> { d.jobs().into_iter().filter(|j| ids.contains(&j.0)).map(|j| format!("{}:{}", j.1, j.2)).collect() };
    r.finished = d.pump(max, name, |d| {
        let all: Vec<_> = d.jobs().into_iter().filter(|j| r.ids.contains(&j.0)).collect();
        !all.is_empty() && all.iter().all(|j| j.3)
    });
    r.secs = t.elapsed().as_secs_f32();
    r.states = mine(d);
    r.pids_after = pids();
    d.log(format!("[{name}] finished={} in {:.1}s; jobs={:?}; notice={:?}", r.finished, r.secs, r.states, d.app.ai().notice));
    std::thread::sleep(Duration::from_millis(300));
    for l in stdin_lines(&live.wlog).into_iter().filter(|l| !before.contains(l)) {
        if let Ok(v) = serde_json::from_str::<Value>(l.split_once('\t').map(|x| x.1).unwrap_or("")) {
            find_model_files(&v, &r.ids, &mut r.model_files);
        }
    }
    r
}

/// Set the Hint page route exactly as a graph rewire does.
pub fn cfg(d: &mut Drv, g: ck::hints::Producer, f: ck::hints::Final) {
    let mut c = d.app.hint_cfg_for_test().expect("a clip is open");
    c.guide = g;
    c.finish = f;
    d.app.hint_apply_cfg_for_test(c);
    d.frames(6);
}

/// Start a Key on the Key tab (range = first `n` frames) and return once the click is done.
pub fn key_click(d: &mut Drv, n: usize) {
    d.app.viewer_mut_for_test(|v| v.range = Some((0, n - 1)));
    d.frames(8);
    d.pump(60, "Key button ready", |d| (d.has("Key shot") || d.has("Key again…")) && !d.has_containing("Checking the output folder"));
    let label = if d.has("Key shot") { "Key shot" } else { "Key again…" };
    d.log(format!("Key screen: {:?}", d.screen()));
    d.click(label);
}

/// Find `*.ext` files under `dir`.
pub fn find_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = vec![];
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(find_ext(&p, ext));
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// A pipeline preset named `name` for the Board, written where the settings file's presets live.
pub fn write_preset(live: &Live, name: &str, guide: ck::hints::Producer, finish: ck::hints::Final, hint_mode: &str) {
    use ck::presets::{pipeline_preset, Pipeline, PipelineExport};
    let mut hint = ck::hints::HintCfg::default();
    (hint.guide, hint.finish) = (guide, finish);
    let outputs = ck::workflow::OutputCfg::default();
    let p = Pipeline { hint, params: ck::workflow::Params { hint_mode: hint_mode.into(), ..Default::default() }, outputs: outputs.clone(), location: None, export: PipelineExport::for_outputs(&outputs) };
    ck::presets::save(&live.base.join("presets.json"), &[pipeline_preset(name, &p)]).unwrap();
}

/// Board: select `folders`, pick the saved preset `preset`, click "Key selected". Leaves the batch review (or queued Key) on screen.
pub fn board_key_selected(d: &mut Drv, folders: &[&str], preset: &str) {
    d.app.ai_mut().placement = ck::scheduler::Placement::Target("local-0".into());
    d.app.reload_presets_for_test();
    d.frames(6);
    d.app.select_folders(folders);
    d.frames(4);
    d.app.ai_mut().batch_dialog_set_preset(&d.ctx, Some(preset.into()));
    d.frames(4);
    d.log("Board: Key selected");
    d.app.key_selected(&d.ctx);
    d.pump(30, "review or queue", |d| d.app.ai().batch_ready() || !d.app.ai().scheduler().snapshot().is_empty());
    d.frames(12);
}

/// Cancel every job that is not finished and wait for the scheduler to settle.
pub fn cancel_all(d: &mut Drv) {
    for j in d.jobs().into_iter().filter(|j| !j.3) {
        d.app.ai_mut().scheduler_mut().cancel(&j.0);
    }
    d.pump(60, "cancelled jobs settle", |d| d.jobs().iter().all(|j| j.3));
}
