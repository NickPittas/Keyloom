//! Import UI state: plan → confirm → run → progress/cancel/resume. The only code that starts the import helper.
//! The UI thread never blocks: it polls the job channel once per frame.
use super::picker::PickKind;
use super::viewer::{DIM, WARN};
use crate::import::{Event, Job, PlanItem, Request, Sources};
use crate::project::{Clip, Source};
use eframe::egui::{self, Color32, RichText, Ui};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

const ERR: Color32 = Color32::from_rgb(235, 120, 120);

/// Where an import writes. Always shown on the confirm bar; nothing is written until the user confirms it.
#[derive(Clone, Debug, PartialEq)]
pub enum Dest {
    /// Add clips to this existing project.
    Add(PathBuf),
    /// Create a new project folder inside this (chosen) parent folder; no extra level is inserted.
    NewIn(PathBuf),
}

struct Pending {
    src: Sources,
    items: Vec<PlanItem>,
    name: String,
    /// `None` until the user chooses (a drop on the empty landing page).
    dest: Option<Dest>,
    /// The project that was open when the sources arrived (offered as a destination).
    current: Option<PathBuf>,
}

/// A dry-run plan in flight. `prev` is restored if re-planning for a new destination fails.
struct Planning {
    job: Job,
    p: Pending,
    prev: Option<Pending>,
}

struct Running {
    job: Job,
    stage: String,
    created: Vec<String>,
    failed: usize,
    ended: bool,
}

/// What the Board should do after polling.
#[derive(Default, Debug)]
pub struct Outcome {
    /// Reload the project from this folder (new project created, or a clip changed).
    pub reload: Option<PathBuf>,
    /// Open this clip folder on Media (a single clip was imported successfully).
    pub open_clip: Option<String>,
}

pub enum CardState {
    Idle,
    Waiting,
    Busy(String, f32),
    Failed(String),
    NeedsFrames,
}

pub struct ImportUi {
    pub data_dir: PathBuf,
    repo: Option<PathBuf>,
    planning: Option<Planning>,
    pending: Option<Pending>,
    running: Option<Running>,
    progress: BTreeMap<String, (String, u64, u64)>,
    errors: BTreeMap<String, String>,
    /// (text, is_error, details)
    notice: Option<(String, bool, String)>,
    project: Option<PathBuf>,
    /// Set by the confirm bar's destination buttons (`Location` / `AddTo`); the App opens the dialog (never blocks here).
    pub dest_pick_requested: Option<PickKind>,
    /// A file dialog is open or its answer is not applied yet: the waiting plan must not be confirmed or
    /// cancelled (the answer decides its destination). Set by the App every frame.
    pub hold: bool,
}

fn default_name(src: &Sources) -> String {
    let p = src.videos.first().or(src.folders.first());
    p.and_then(|p| p.file_stem().or(p.file_name()))
        .map(|s| s.to_string_lossy().replace('_', " "))
        .unwrap_or_else(|| "New project".into())
}

impl ImportUi {
    pub fn new(data_dir: PathBuf, repo: Option<PathBuf>) -> Self {
        ImportUi {
            data_dir,
            repo,
            planning: None,
            pending: None,
            running: None,
            progress: BTreeMap::new(),
            errors: BTreeMap::new(),
            notice: None,
            project: None,
            dest_pick_requested: None,
            hold: false,
        }
    }

    /// A message for the user on the Board (not an error with details).
    pub fn notify(&mut self, msg: &str) {
        self.notice = Some((msg.into(), false, String::new()));
    }

    pub fn dismiss_notice(&mut self) {
        self.notice = None;
    }

    /// The user picked a destination for the waiting plan: re-plan against it (clip names depend on it).
    /// Nothing is written. Returns false (and says why) when no plan is waiting.
    pub fn set_dest(&mut self, ctx: &egui::Context, dest: Dest) -> bool {
        let Some(p) = self.pending.take() else {
            self.notify("Destination choice ignored: no import is waiting for one.");
            return false;
        };
        let (src, name, current) = (p.src.clone(), p.name.clone(), p.current.clone());
        let next = Pending {
            src,
            items: vec![],
            name,
            dest: Some(dest),
            current,
        };
        match Job::spawn(&Self::request(&next), &next.src, true, self.repo.as_deref()) {
            Ok(job) => {
                self.planning = Some(Planning {
                    job,
                    p: next,
                    prev: Some(p),
                });
            }
            Err(e) => {
                self.pending = Some(p);
                self.fail(e, String::new());
            }
        }
        ctx.request_repaint();
        true
    }

    /// The helper request for a pending import. A plan that has no destination yet is only probed (no
    /// destination exists; it is never started without a chosen one).
    fn request(p: &Pending) -> Request {
        match &p.dest {
            Some(Dest::Add(project)) => Request::Add {
                project: project.clone(),
            },
            Some(Dest::NewIn(parent)) => Request::NewIn {
                name: p.name.trim().to_owned(),
                parent: parent.clone(),
            },
            None => Request::Probe,
        }
    }

    pub fn busy(&self) -> bool {
        self.planning.is_some() || self.running.is_some()
    }
    /// Why `busy()` is true, for user-facing text: probing the files is not a running import.
    pub fn busy_what(&self) -> &'static str {
        if self.running.is_some() {
            "an import is running"
        } else {
            "the selected files are still being checked"
        }
    }
    /// The open project changed (or failed to load): a waiting plan that was made against another one is
    /// forgotten, so it can never be confirmed into a project that is no longer the open one.
    pub fn project_changed(&mut self, now: Option<&Path>) {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| p.current.as_deref() != now)
        {
            self.pending = None;
        }
    }
    /// True once a plan is waiting for the user's confirmation.
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }
    /// The destination of the waiting plan (tests).
    pub fn pending_dest(&self) -> Option<&Dest> {
        self.pending.as_ref().and_then(|p| p.dest.as_ref())
    }
    pub fn notice_text(&self) -> Option<&str> {
        self.notice.as_ref().map(|n| n.0.as_str())
    }

    fn fail(&mut self, msg: impl Into<String>, details: String) {
        self.notice = Some((msg.into(), true, details));
    }

    /// Start planning (background probe) for `src`. `project` = the open project, offered as the destination
    /// (pre-selected, shown on the confirm bar). With none open the user must choose a folder before importing.
    /// A fresh call drops any waiting plan and its destination: sources and destination never mix.
    pub fn begin(&mut self, ctx: &egui::Context, project: Option<&Path>, src: Sources) {
        self.begin_with(ctx, project, src, None);
    }

    /// QA (`--data-dir`/`--import`): with no open project the destination is preset to `<data-dir>/Projects`.
    pub fn begin_qa(&mut self, ctx: &egui::Context, project: Option<&Path>, src: Sources) {
        let preset = Dest::NewIn(self.data_dir.join("Projects"));
        self.begin_with(ctx, project, src, Some(preset));
    }

    fn begin_with(
        &mut self,
        ctx: &egui::Context,
        project: Option<&Path>,
        src: Sources,
        preset: Option<Dest>,
    ) {
        if self.busy() || (src.videos.is_empty() && src.folders.is_empty()) {
            return;
        }
        self.notice = None;
        self.pending = None;
        let p = Pending {
            name: default_name(&src),
            dest: project.map(|p| Dest::Add(p.into())).or(preset),
            current: project.map(Into::into),
            items: vec![],
            src,
        };
        match Job::spawn(&Self::request(&p), &p.src, true, self.repo.as_deref()) {
            Ok(job) => self.planning = Some(Planning { job, p, prev: None }),
            Err(e) => self.fail(e, String::new()),
        }
        ctx.request_repaint();
    }

    /// Re-run extraction for one clip folder (cancelled, crashed or failed import).
    pub fn resume(&mut self, ctx: &egui::Context, project: &Path, clip: &Clip) {
        if self.busy() {
            return;
        }
        self.notice = None;
        self.errors.remove(&clip.folder);
        match Job::spawn(
            &Request::Resume {
                clip_root: clip.root.clone(),
            },
            &Sources::default(),
            false,
            self.repo.as_deref(),
        ) {
            Ok(job) => {
                self.project = Some(project.into());
                self.progress
                    .insert(clip.folder.clone(), ("extract".into(), 0, 0));
                self.running = Some(Running {
                    job,
                    stage: "Resuming extraction".into(),
                    created: vec![],
                    failed: 0,
                    ended: false,
                });
            }
            Err(e) => self.fail(e, String::new()),
        }
        ctx.request_repaint();
    }

    /// QA only: accept the confirm bar without a click.
    pub fn confirm_now(&mut self, ctx: &egui::Context) {
        if !self.hold
            && self
                .pending
                .as_ref()
                .is_some_and(|p| p.dest.is_some() && p.items.iter().any(|i| i.ok))
        {
            self.start(ctx);
        }
    }

    fn start(&mut self, ctx: &egui::Context) {
        if self.hold {
            return; // a destination answer is still on its way: the old destination is not the user's choice
        }
        let Some(p) = self.pending.take() else { return };
        if p.dest.is_none() {
            self.pending = Some(p); // never write to a destination nobody chose
            return;
        }
        let req = Self::request(&p);
        if let Request::Add { project } = &req {
            self.project = Some(project.clone());
        }
        match Job::spawn(&req, &p.src, false, self.repo.as_deref()) {
            Ok(job) => {
                self.running = Some(Running {
                    job,
                    stage: "Starting".into(),
                    created: vec![],
                    failed: 0,
                    ended: false,
                })
            }
            Err(e) => self.fail(e, String::new()),
        }
        ctx.request_repaint();
    }

    /// Cancel on the confirm bar: forget the plan, its sources and its destination. Nothing was written.
    pub fn discard_pending(&mut self) {
        if !self.hold {
            self.pending = None;
        }
    }

    pub fn cancel(&mut self) {
        if let Some(r) = &mut self.running {
            r.job.cancel();
        }
    }

    pub fn card_state(&self, c: &Clip) -> CardState {
        if let Some((phase, d, t)) = self.progress.get(&c.folder) {
            let f = if *t > 0 { *d as f32 / *t as f32 } else { 0.0 };
            let verb = if phase == "compress" {
                "Compressing"
            } else {
                "Extracting"
            };
            return CardState::Busy(
                if *t > 0 {
                    format!("{verb} {d}/{t}")
                } else {
                    format!("{verb}…")
                },
                f,
            );
        }
        if let Some(e) = self.errors.get(&c.folder) {
            return CardState::Failed(e.clone());
        }
        if needs_frames(c) {
            if self.running.is_some() {
                CardState::Waiting
            } else {
                CardState::NeedsFrames
            }
        } else {
            CardState::Idle
        }
    }

    /// Poll the helper; call once per frame. Keeps repainting while anything is running.
    pub fn poll(&mut self, ctx: &egui::Context) -> Outcome {
        let mut out = Outcome::default();
        if let Some(pl) = &mut self.planning {
            let mut plan = None;
            let mut dead = None;
            while let Some(e) = pl.job.try_recv() {
                match e {
                    Event::Plan(items) => plan = Some(items),
                    Event::Error(m) => dead = Some((m, String::new())),
                    Event::Exited { stderr, .. } if plan.is_none() && dead.is_none() => {
                        dead = Some((
                            "The import helper stopped before it produced a plan.".into(),
                            stderr,
                        ))
                    }
                    _ => {}
                }
            }
            if plan.is_some() || dead.is_some() {
                let Planning { mut p, prev, .. } = self.planning.take().unwrap();
                match (plan, dead) {
                    (Some(items), _) => {
                        p.items = items;
                        self.pending = Some(p);
                    }
                    (_, Some((m, d))) => {
                        self.pending = prev; // a bad destination keeps the sources and the old choice
                        self.fail(m, d)
                    }
                    _ => {}
                }
            }
        }
        let mut finished_run = None;
        if let Some(r) = &mut self.running {
            while let Some(e) = r.job.try_recv() {
                match e {
                    Event::Stage(s) => r.stage = s,
                    Event::Created { project, clips } => {
                        for (n, _, kind) in &clips {
                            if kind == "video" {
                                self.progress.insert(n.clone(), ("extract".into(), 0, 0));
                            }
                        }
                        r.created = clips.into_iter().map(|c| c.0).collect();
                        self.project = Some(project.clone());
                        out.reload = Some(project);
                    }
                    Event::Progress {
                        clip,
                        phase,
                        done,
                        total,
                    } => {
                        self.progress.insert(clip, (phase, done, total));
                    }
                    Event::ClipDone { clip, .. } => {
                        self.progress.remove(&clip);
                        out.reload = self.project.clone();
                    }
                    Event::ClipError { clip, message } => {
                        self.progress.remove(&clip);
                        self.errors.insert(clip, message);
                        r.failed += 1;
                    }
                    Event::Cancelled => {
                        r.ended = true;
                        self.notice = Some((
                            "Import stopped. Movies that were not finished keep their source copy and show Resume.".into(),
                            false,
                            String::new(),
                        ));
                    }
                    Event::Finished => r.ended = true,
                    Event::Error(m) => {
                        r.ended = true;
                        self.notice = Some((m, true, String::new()));
                    }
                    Event::Exited { code, stderr } => {
                        if !r.ended {
                            self.notice = Some((
                                format!("The import helper stopped unexpectedly (exit {code:?}). Clips already created stay in the project."),
                                true,
                                stderr.clone(),
                            ));
                        } else if let Some(n) = &mut self.notice {
                            if n.1 {
                                n.2 = stderr.clone();
                            }
                        }
                        finished_run = Some(());
                        break;
                    }
                    Event::Plan(_) => {}
                }
            }
            if finished_run.is_some() {
                let ok_single = r.created.len() == 1 && r.failed == 0 && self.notice.is_none();
                if ok_single {
                    out.open_clip = r.created.first().cloned();
                }
                out.reload = self.project.clone();
            }
        }
        if finished_run.is_some() {
            self.running = None;
            self.progress.clear();
        }
        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        out
    }

    /// Notice, plan/confirm bar and running strip. Returns nothing: it starts/cancels jobs itself.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if let Some((msg, is_err, details)) = &self.notice {
            let (msg, is_err, details) = (msg.clone(), *is_err, details.clone());
            let mut dismiss = false;
            ui.horizontal_wrapped(|ui| {
                ui.colored_label(if is_err { ERR } else { WARN }, msg);
                if ui.button("Dismiss").clicked() {
                    dismiss = true;
                }
            });
            if !details.trim().is_empty() {
                ui.collapsing("Details", |ui| {
                    ui.label(RichText::new(details).monospace().size(11.5));
                });
            }
            if dismiss {
                self.notice = None;
            }
        }
        if self.planning.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("Checking the selected files (reading movie headers)…");
            });
        }
        if let Some(r) = &self.running {
            let stopping = r.job.cancel_sent;
            let stage = r.stage.clone();
            let mut cancel = false;
            ui.horizontal(|ui| {
                ui.spinner();
                if stopping {
                    ui.label("Stopping after the current step (a file copy already under way finishes first)…");
                } else {
                    ui.label(format!("Importing — {stage}"));
                    cancel = ui.button("Cancel import").clicked();
                }
            });
            if cancel {
                self.cancel();
            }
        }
        let mut start = false;
        let mut drop_pending = false;
        let mut act = None;
        let hold = self.hold;
        if let Some(p) = &mut self.pending {
            let ok = p.items.iter().filter(|i| i.ok).count();
            egui::Frame::group(ui.style()).show(ui, |ui| {
                ui.set_width(ui.available_width());
                dest_ui(ui, p, hold, &mut act);
                for i in &p.items {
                    let label = Path::new(&i.source)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| i.source.clone());
                    ui.horizontal_wrapped(|ui| {
                        if i.ok {
                            let how = if i.kind == "video" {
                                "copied into the project; frames extracted to EXR"
                            } else {
                                "referenced in place (source folder is not modified)"
                            };
                            ui.label(format!("Import {label}"));
                            let mut t = format!("as clip “{}”", i.clip.clone().unwrap_or_default());
                            if let Some(o) = &i.renamed_from {
                                t += &format!(" (“{o}” was taken)");
                            }
                            ui.label(RichText::new(t).color(if i.renamed_from.is_some() {
                                WARN
                            } else {
                                DIM
                            }));
                            if !i.detail.is_empty() {
                                ui.label(RichText::new(format!("· {}", i.detail)).color(DIM));
                            }
                            ui.label(RichText::new(format!("· {how}")).color(DIM));
                        } else {
                            ui.colored_label(
                                ERR,
                                format!(
                                    "Not imported: {label} — {}",
                                    i.reason.clone().unwrap_or_default()
                                ),
                            );
                        }
                    });
                }
                ui.horizontal(|ui| {
                    let named =
                        !matches!(p.dest, Some(Dest::NewIn(_))) || !p.name.trim().is_empty();
                    let can = ok > 0 && p.dest.is_some() && named && !hold;
                    if ui
                        .add_enabled(
                            can,
                            egui::Button::new(format!(
                                "Import {ok} clip{}",
                                if ok == 1 { "" } else { "s" }
                            )),
                        )
                        .on_disabled_hover_text(if hold {
                            "A file dialog is open: finish choosing the destination first"
                        } else if ok == 0 {
                            "Nothing here can be imported"
                        } else if p.dest.is_none() {
                            "Choose where to save the project first"
                        } else {
                            "Give the project a name"
                        })
                        .clicked()
                    {
                        start = true;
                    }
                    if ui
                        .add_enabled(!hold, egui::Button::new("Cancel"))
                        .on_disabled_hover_text(
                            "A file dialog is open: finish choosing the destination first",
                        )
                        .clicked()
                    {
                        drop_pending = true;
                    }
                });
            });
        }
        match act {
            Some(Act::Pick(k)) => self.dest_pick_requested = Some(k),
            Some(Act::Set(d)) => {
                self.set_dest(&ctx, d);
            }
            None => {}
        }
        if drop_pending {
            self.discard_pending();
        }
        if start {
            self.start(&ctx);
        }
    }
}

/// A video-sourced clip with no extracted frames yet.
pub fn needs_frames(c: &Clip) -> bool {
    matches!(c.source, Source::Video { .. }) && c.input_files.is_empty()
}

/// Folder name the backend gives a new project: `<yymmdd_hhmmss>_<name>` with the name sanitised.
pub(crate) fn folder_stem(name: &str) -> String {
    let mut out = String::new();
    for c in name.trim().chars() {
        let c = if c.is_alphanumeric() || c == '_' || c == '-' {
            c
        } else {
            '_'
        };
        if c != '_' || !out.ends_with('_') {
            out.push(c);
        }
    }
    out.trim_matches('_').chars().take(60).collect()
}

enum Act {
    Pick(PickKind),
    Set(Dest),
}

/// Destination section of the confirm bar: the choice, and a literal preview of what gets written where.
fn dest_ui(ui: &mut Ui, p: &mut Pending, hold: bool, act: &mut Option<Act>) -> Vec<egui::Response> {
    let mut controls = vec![];
    // While a destination dialog is open its answer decides the destination: the choice buttons are off.
    ui.add_enabled_ui(!hold, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Destination").strong());
            if let Some(cur) = p.current.clone() {
                let on = p.dest == Some(Dest::Add(cur.clone()));
                let r = ui.selectable_label(on, "Add to the open project");
                if r.clicked() && !on {
                    *act = Some(Act::Set(Dest::Add(cur)));
                }
                controls.push(r);
            }
            let r = ui.button("Add to another project…");
            if r.clicked() {
                *act = Some(Act::Pick(PickKind::AddTo));
            }
            controls.push(r);
            let new = matches!(p.dest, Some(Dest::NewIn(_)));
            let label = if new {
                "New project in another folder…"
            } else {
                "New project in folder…"
            };
            let r = ui.button(label);
            if r.clicked() {
                *act = Some(Act::Pick(PickKind::Location));
            }
            controls.push(r);
        });
    });
    match p.dest.clone() {
        None => {
            ui.colored_label(WARN, "Choose where to save before importing: a folder for a new project, or an existing project to add to.");
        }
        Some(Dest::Add(project)) => {
            ui.label(
                RichText::new(format!(
                    "Clips will be added to the existing project {}",
                    project.display()
                ))
                .strong(),
            );
        }
        Some(Dest::NewIn(parent)) => {
            ui.horizontal_wrapped(|ui| {
                ui.label("Project name");
                ui.add(
                    egui::TextEdit::singleline(&mut p.name)
                        .desired_width(240.0)
                        .hint_text("Project name"),
                );
            });
            ui.label(RichText::new(format!("Chosen folder: {}", parent.display())).color(DIM));
            ui.label(
                RichText::new(format!(
                    "A new project folder will be created inside it: {}",
                    parent
                        .join(format!("<yymmdd_hhmmss>_{}", folder_stem(&p.name)))
                        .display()
                ))
                .strong(),
            );
        }
    }
    controls
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, PointerButton, Pos2, RawInput};

    /// One frame of the destination row; returns each control's rect/enabled state and any choice made.
    fn frame(
        ctx: &egui::Context,
        p: &mut Pending,
        hold: bool,
        ev: Vec<Event>,
    ) -> (Vec<(egui::Rect, bool)>, bool) {
        let raw = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                Pos2::ZERO,
                egui::vec2(1400.0, 80.0),
            )),
            events: ev,
            ..Default::default()
        };
        let (mut rects, mut chose) = (vec![], false);
        ctx.run_ui(raw, |ui| {
            let mut act = None;
            let c = dest_ui(ui, p, hold, &mut act);
            rects = c.iter().map(|r| (r.rect, r.enabled())).collect();
            chose = act.is_some();
        })
        .textures_delta
        .clear();
        (rects, chose)
    }

    /// Click the centre of each real control (its captured rect) and report whether any click chose something.
    fn click_each(hold: bool) -> (Vec<bool>, bool) {
        let ctx = egui::Context::default();
        let mut p = Pending {
            src: Sources::default(),
            items: vec![],
            name: "n".into(),
            dest: Some(Dest::Add("/a".into())),
            current: Some("/a".into()),
        };
        let (rects, _) = frame(&ctx, &mut p, hold, vec![]);
        assert_eq!(rects.len(), 3, "open project, another project, new folder");
        let mut chose = false;
        for (r, _) in &rects {
            let pos = r.center();
            let btn = |pressed| Event::PointerButton {
                pos,
                button: PointerButton::Primary,
                pressed,
                modifiers: Default::default(),
            };
            frame(&ctx, &mut p, hold, vec![Event::PointerMoved(pos)]);
            chose |= frame(&ctx, &mut p, hold, vec![btn(true)]).1;
            chose |= frame(&ctx, &mut p, hold, vec![btn(false)]).1;
        }
        (rects.iter().map(|r| r.1).collect(), chose)
    }

    #[test]
    fn destination_controls_do_nothing_while_a_dialog_holds_the_plan() {
        let (enabled, chose) = click_each(false);
        assert!(
            enabled.iter().all(|e| *e) && chose,
            "sanity: the controls work"
        );
        let (enabled, chose) = click_each(true);
        assert!(enabled.iter().all(|e| !*e), "{enabled:?}");
        assert!(!chose, "a click on a held control chose a destination");
    }
}
