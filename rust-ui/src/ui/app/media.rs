//! Media-side recovery and import: relink a vanished movie (probe + identity check, then validated frame
//! extraction through the worker) and import a companion alpha/mask IMAGE SEQUENCE (validated, no overwrite).
use super::*;
use crate::actions::Decision;
use crate::relink::{self, Probed, SeqPlan};
use crate::tasks::TaskKind;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

pub struct MovieRelink {
    /// The clip (folder) this check belongs to: never applied to another clip.
    pub folder: String,
    pub path: PathBuf,
    job: Option<crate::import::Job>,
    pub probed: Option<Probed>,
    pub verdict: Option<Result<bool, String>>,
    pub confirm: bool,
}

pub struct SeqImport {
    /// The clip the plan was made for.
    pub clip_folder: String,
    pub clip_root: PathBuf,
    pub folder: PathBuf,
    pub mask: bool,
    pub plan: Result<SeqPlan, String>,
    run: Option<(Receiver<Result<usize, String>>, Arc<AtomicBool>)>,
}

#[derive(Default)]
pub struct MediaState {
    pub movie: Option<MovieRelink>,
    pub seq: Option<SeqImport>,
    pub note: Option<String>,
}

/// Read-only snapshot of the open clip's Media state, built from cached state only (the Shot page reads it;
/// every change goes through `media_cmd`).
#[derive(Debug, Clone)]
pub(crate) struct MediaView {
    pub source: MediaSource,
    pub source_path: Option<PathBuf>,
    pub resolution: Option<String>,
    pub source_frames: usize,
    /// Frames on the timeline (the viewer's index); `None` when no viewer is open on this clip.
    pub timeline_frames: Option<usize>,
    pub fps: f64,
    pub fps_assumed: bool,
    pub input_linear: bool,
    pub stored_linear: bool,
    pub colour_save_failed: bool,
    pub colour_origin: &'static str,
    pub paths: Vec<PathRow>,
    pub result_range: Option<String>,
    /// The source movie is gone and its stored file name is known: a relink can be offered.
    pub missing: bool,
    pub relink: Option<RelinkView>,
    /// Extract readiness and the reason when it is blocked (only while a movie check is for this clip).
    pub extract: Option<Decision>,
    pub can_choose_companion: bool,
    pub companion: Option<CompanionView>,
    pub note: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaSource {
    Sequence { external: bool },
    Movie,
    Missing,
}

#[derive(Debug, Clone)]
pub(crate) struct PathRow {
    pub label: &'static str,
    pub path: PathBuf,
    /// Frames the folder holds, when it is a frame folder.
    pub frames: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelinkState {
    Checking,
    Failed,
    Partial,
    Verified,
}

#[derive(Debug, Clone)]
pub(crate) struct RelinkView {
    pub path: PathBuf,
    pub state: RelinkState,
    /// The failure text when `state` is `Failed`.
    pub failure: Option<String>,
    pub probed: Option<Probed>,
    /// The same-footage checkbox (only meaningful for `Partial`).
    pub confirmed: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CompanionView {
    pub folder: PathBuf,
    /// false = AlphaHint, true = VideoMamaMaskHint.
    pub mask: bool,
    pub plan: Result<CompanionPlan, String>,
    /// A copy is running; a proposed import has `running == false`.
    pub running: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct CompanionPlan {
    pub dest: PathBuf,
    /// Frames that will be written.
    pub copy: usize,
    /// Existing frames that are kept, never replaced.
    pub kept: usize,
}

/// Every Media action the old panel and the Shot page can trigger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MediaCmd {
    /// Input colour interpretation (Linear when true); saved through `persist_media`.
    SetInputLinear(bool),
    /// Choose a replacement movie (file dialog, `PickKind::RelinkMovie`).
    Relink,
    CancelRelink,
    ConfirmMovieMatch(bool),
    ExtractRelinked,
    /// Choose a companion image folder (file dialog, `PickKind::HintSequence`).
    ChooseCompanion,
    /// Alpha (false) or mask (true) target; re-plans a proposed import.
    ImportAs { mask: bool },
    StartSequenceImport,
    /// Drop a proposed (not yet running) import.
    CancelSequenceImport,
    /// Ask a running import to stop.
    StopSequenceImport,
}

impl App {
    pub(super) fn movie_picked(&mut self, path: PathBuf) {
        let Some(clip) = self.open_clip_snapshot() else {
            return;
        };
        let job = crate::import::Job::spawn(
            &crate::import::Request::Probe,
            &crate::import::Sources {
                videos: vec![path.clone()],
                folders: vec![],
            },
            true,
            self.opts.repo.as_deref(),
        );
        self.media.note = None;
        match job {
            Ok(j) => {
                self.media.movie = Some(MovieRelink {
                    folder: clip.folder.clone(),
                    path,
                    job: Some(j),
                    probed: None,
                    verdict: None,
                    confirm: false,
                });
            }
            Err(e) => self.media.note = Some(format!("Cannot check the movie: {e}")),
        }
    }

    pub(super) fn sequence_picked(&mut self, folder: PathBuf) {
        let Some(clip) = self.open_clip_snapshot() else {
            return;
        };
        let range = self
            .ai_open_range()
            .unwrap_or((0, clip.input_files.len().saturating_sub(1)));
        // Mask or alpha: decided by what the user chooses next; plan for alpha first and re-plan on toggle.
        let plan = relink::plan_sequence(&clip, &folder, false, range);
        self.media.seq = Some(SeqImport {
            clip_folder: clip.folder.clone(),
            clip_root: clip.root.clone(),
            folder,
            mask: false,
            plan,
            run: None,
        });
    }

    /// Poll the movie probe and the sequence copy (called every frame).
    pub(super) fn media_poll(&mut self, ctx: &egui::Context) {
        // Pending checks and plans belong to the clip they were made for: switching clips drops them (a running
        // copy keeps going and finishes for ITS clip).
        let open_folder = self.open_clip_snapshot().map(|c| c.folder);
        if self
            .media
            .movie
            .as_ref()
            .is_some_and(|m| Some(&m.folder) != open_folder.as_ref())
        {
            self.media.movie = None;
        }
        if self
            .media
            .seq
            .as_ref()
            .is_some_and(|s| s.run.is_none() && Some(&s.clip_folder) != open_folder.as_ref())
        {
            self.media.seq = None;
        }
        if let Some(m) = self.media.movie.as_mut() {
            if let Some(j) = m.job.as_mut() {
                let mut finished = false;
                while let Some(ev) = j.try_recv() {
                    match ev {
                        crate::import::Event::Plan(items) => {
                            m.probed =
                                items
                                    .first()
                                    .and_then(|i| i.probed)
                                    .map(|(w, h, f, n)| Probed {
                                        width: w,
                                        height: h,
                                        fps: f,
                                        frames: n,
                                    });
                            if m.probed.is_none() {
                                m.verdict = Some(Err(items
                                    .first()
                                    .and_then(|i| i.reason.clone())
                                    .unwrap_or_else(|| "the movie could not be probed".into())));
                            }
                        }
                        crate::import::Event::Error(e) => m.verdict = Some(Err(e)),
                        crate::import::Event::Exited { .. } => finished = true,
                        _ => {}
                    }
                }
                if finished {
                    m.job = None;
                    if m.verdict.is_none() {
                        m.verdict = Some(
                            match (
                                &m.probed,
                                self.project
                                    .as_ref()
                                    .and_then(|p| p.clips.iter().find(|c| c.folder == m.folder))
                                    .map(|c| c.root.clone()),
                            ) {
                                (Some(p), Some(root)) => match relink::stored_identity(&root) {
                                    Some(id) => relink::compare(&id, p),
                                    None => Ok(false),
                                },
                                _ => Err("the movie could not be probed".into()),
                            },
                        );
                    }
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(100));
                }
            }
        }
        if let Some(s) = self.media.seq.as_mut() {
            if let Some((rx, _)) = &s.run {
                match rx.try_recv() {
                    Ok(r) => {
                        s.run = None;
                        let folder = Some(s.clip_folder.clone());
                        self.media.note = Some(match r {
                            Ok(n) => format!(
                                "Imported {n} frame(s) into {}.",
                                s.plan
                                    .as_ref()
                                    .map(|p| p.dest.display().to_string())
                                    .unwrap_or_default()
                            ),
                            Err(e) => format!("Import stopped: {e}"),
                        });
                        let done_root = s.clip_root.clone();
                        self.media.seq = None;
                        self.ai.set_external_writer(&done_root, false);
                        if let Some(f) = folder {
                            self.refresh_clip(ctx, &f);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        ctx.request_repaint_after(std::time::Duration::from_millis(100))
                    }
                    Err(_) => {
                        s.run = None;
                        let r = s.clip_root.clone();
                        self.ai.set_external_writer(&r, false);
                    }
                }
            }
        }
    }

    /// The probe verdict of the picked replacement movie: `Ok(true)` full identity match, `Ok(false)` partial,
    /// `Err` a different movie (tests / QA).
    pub fn movie_relink_verdict(&self) -> Option<Result<bool, String>> {
        self.media.movie.as_ref().and_then(|m| m.verdict.clone())
    }

    /// Point the clip at the checked movie and queue validated extraction (the Extract button). A partial
    /// match needs `confirm` (the checkbox); a mismatch never gets here.
    pub fn extract_relinked_movie(&mut self) -> Result<(), String> {
        let open = self.open_clip_snapshot().ok_or("no clip open")?;
        let (path, verdict, confirm, bound) = match &self.media.movie {
            Some(m) => (
                m.path.clone(),
                m.verdict.clone(),
                m.confirm,
                m.folder.clone(),
            ),
            None => return Err("no movie chosen".into()),
        };
        if open.folder != bound {
            self.media.movie = None;
            return Err(
                "the movie was checked for another clip; pick it again for this one".into(),
            );
        }
        let clip = open;
        let r = match verdict {
            Some(Ok(true)) => Ok(()),
            Some(Ok(false)) if confirm => Ok(()),
            Some(Ok(false)) => Err("confirm that this is the same footage first".to_string()),
            Some(Err(e)) => Err(e),
            None => Err("the movie is still being checked".to_string()),
        };
        let res = r.and_then(|()| {
            relink::point_at_movie(&clip, &path)?;
            let job = crate::hints::Ctx {
                clip: &clip,
                range: (0, 0),
                job_id: crate::workflow::new_id(),
                staging: PathBuf::new(),
            }
            .prepare_source(&crate::project::clean_abs(&path));
            let after = crate::actions::waits_for(self.ai.scheduler(), &clip.root, crate::actions::Action::Extract);
            self.ai.submit_task(&clip, job, after, 0).map(|_| ())
        });
        self.media.note = Some(match &res {
            Ok(()) => {
                self.media.movie = None;
                "Extraction queued; the clip reopens with its frames when it finishes.".into()
            }
            Err(e) => format!("Not relinked: {e}"),
        });
        res
    }

    /// Start copying the planned companion frames on a worker thread (the Import button).
    pub fn start_sequence_import(&mut self, ctx: &egui::Context) {
        let Some(s) = self.media.seq.as_ref() else {
            return;
        };
        if s.run.is_some() || s.plan.is_err() {
            return;
        }
        let root = s.clip_root.clone();
        // The import writes the hint folders outside the queue, so it can't wait behind a same-clip job: refuse instead.
        if self.ai.clip_active(&root) || self.ai.external_writer(&root) {
            self.media.note = Some(
                "A hint job or another import is running for this clip; wait for it to finish."
                    .into(),
            );
            return;
        }
        // Exclusive on-disk ownership for the whole copy: worker hint jobs are refused, and so is a second
        // importer, even from another process.
        let lock = match relink::ImportLock::acquire(&root) {
            Ok(l) => l,
            Err(e) => {
                self.media.note = Some(format!("Not imported: {e}"));
                return;
            }
        };
        self.ai.set_external_writer(&root, true);
        let Some(s) = self.media.seq.as_mut() else {
            return;
        };
        let Ok(p) = &s.plan else { return };
        let (tx, rx) = channel();
        let flag = Arc::new(AtomicBool::new(false));
        let plan = SeqPlan {
            dest: p.dest.clone(),
            copy: p.copy.clone(),
            kept: p.kept.clone(),
        };
        let (f, ctx) = (flag.clone(), ctx.clone());
        std::thread::spawn(move || {
            let r = relink::run_sequence(&plan, &f);
            drop(lock); // released before the UI hears the result
            let _ = tx.send(r);
            ctx.request_repaint();
        });
        s.run = Some((rx, flag));
    }

    /// The pending companion-sequence import: `Ok(frames to write)` or why it cannot run (tests / QA).
    pub fn sequence_import_plan(&self) -> Option<Result<usize, String>> {
        self.media
            .seq
            .as_ref()
            .map(|s| s.plan.as_ref().map(|p| p.copy.len()).map_err(Clone::clone))
    }
    pub fn media_note(&self) -> Option<&str> {
        self.media.note.as_deref()
    }

    /// Extract readiness for a movie check of this clip: the same decision the Extract button shows.
    fn extract_decision(&self, clip: &Clip, m: &MovieRelink) -> Decision {
        let ready = prepare_source_worker_ok(self.ai.models_unknown(), self.ai.local_caps().as_ref().is_some_and(|c| c.tasks.get("prepare_source").copied().unwrap_or(false)))
            && (matches!(m.verdict, Some(Ok(true))) || matches!(m.verdict, Some(Ok(false))) && m.confirm);
        crate::actions::decide(self.ai.scheduler(), &clip.root, crate::actions::Action::Extract, "Extract frames", if !ready { Some("FFmpeg or a verified movie is required".into()) } else if self.ai.external_writer(&clip.root) { Some("Another computer is importing into this clip. Wait for it to finish.".into()) } else { None })
    }

    /// Snapshot of the open clip's Media state. Cached state only; the one file read is the stored movie name,
    /// and only when the source is missing (the same check the old panel makes).
    pub(crate) fn media_view(&self) -> Option<MediaView> {
        let clip = self.clip()?;
        let v = self.viewer.as_ref();
        let missing = clip.error.is_some() && matches!(clip.source, Source::Missing) && stored_filename(clip).is_some();
        let (source, source_path) = match &clip.source {
            Source::Sequence { dir, external } => (MediaSource::Sequence { external: *external }, Some(dir.clone())),
            Source::Video { path } => (MediaSource::Movie, Some(path.clone())),
            Source::Missing => (MediaSource::Missing, None),
        };
        let (input_linear, stored_linear, colour_save_failed) = match v {
            Some(v) => (v.linear, v.linear_stored, self.persist_failed == Some((v.range, v.linear))),
            None => (clip.input_is_linear, clip.input_is_linear, false),
        };
        let mut paths = Vec::new();
        if let Some(p) = &source_path {
            paths.push(PathRow { label: "Source", path: p.clone(), frames: None });
        }
        if let Some(p) = clip.view_dir(View::Guide) {
            paths.push(PathRow { label: "Pre-matte guide folder", path: p, frames: Some(clip.mask_frames) });
        }
        if let Some(p) = clip.view_dir(View::Hint) {
            paths.push(PathRow { label: "Final hint folder", path: p, frames: Some(clip.alpha_frames) });
        }
        if let Some(p) = &clip.output_dir {
            paths.push(PathRow { label: "Output folder", path: p.clone(), frames: None });
        }
        if let Some(p) = &clip.manifest_dir {
            paths.push(PathRow { label: "Last run wrote to", path: p.clone(), frames: None });
        }
        Some(MediaView {
            source,
            source_path,
            resolution: clip.sample_size.map(|(w, h)| crate::contracts::resolution_label(w, h)),
            source_frames: clip.input_files.len(),
            timeline_frames: v.map(|v| v.index.stems.len()),
            fps: clip.fps,
            fps_assumed: clip.fps_assumed,
            input_linear,
            stored_linear,
            colour_save_failed,
            colour_origin: color_origin_text(clip),
            paths,
            result_range: clip.result_range_text(),
            missing,
            relink: self.media.movie.as_ref().map(|m| RelinkView {
                path: m.path.clone(),
                state: match &m.verdict {
                    None => RelinkState::Checking,
                    Some(Err(_)) => RelinkState::Failed,
                    Some(Ok(true)) => RelinkState::Verified,
                    Some(Ok(false)) => RelinkState::Partial,
                },
                failure: match &m.verdict {
                    Some(Err(e)) => Some(e.clone()),
                    _ => None,
                },
                probed: m.probed.clone(),
                confirmed: m.confirm,
            }),
            extract: self.media.movie.as_ref().filter(|m| m.folder == clip.folder).map(|m| self.extract_decision(clip, m)),
            can_choose_companion: !clip.input_files.is_empty() && self.media.seq.is_none() && !self.ai.external_writer(&clip.root),
            companion: self.media.seq.as_ref().filter(|s| s.clip_folder == clip.folder).map(|s| CompanionView {
                folder: s.folder.clone(),
                mask: s.mask,
                running: s.run.is_some(),
                plan: match &s.plan {
                    Ok(p) => Ok(CompanionPlan { dest: p.dest.clone(), copy: p.copy.len(), kept: p.kept.len() }),
                    Err(e) => Err(e.clone()),
                },
            }),
            note: self.media.note.clone(),
            error: clip.error.clone(),
        })
    }

    /// Run one Media action. The old panel and the Shot page both call this; `Err` is a refusal or a failed
    /// extraction, shown as the Media note.
    pub(crate) fn media_cmd(&mut self, ctx: &egui::Context, c: MediaCmd) -> Result<(), String> {
        match c {
            MediaCmd::SetInputLinear(linear) => {
                self.viewer.as_mut().ok_or("no clip open")?.linear = linear;
                self.persist_media();
            }
            MediaCmd::Relink => {
                if self.media.movie.is_none() {
                    self.request_pick(ctx, PickKind::RelinkMovie);
                }
            }
            MediaCmd::CancelRelink => self.media.movie = None,
            MediaCmd::ConfirmMovieMatch(on) => self.media.movie.as_mut().ok_or("no movie chosen")?.confirm = on,
            MediaCmd::ExtractRelinked => self.extract_relinked_movie()?,
            MediaCmd::ChooseCompanion => {
                let root = self.clip().ok_or("no clip open")?.root.clone();
                if self.media.seq.is_none() && !self.ai.external_writer(&root) {
                    self.request_pick(ctx, PickKind::HintSequence);
                }
            }
            MediaCmd::ImportAs { mask } => {
                let clip = self.open_clip_snapshot().ok_or("no clip open")?;
                let range = self
                    .ai_open_range()
                    .unwrap_or((0, clip.input_files.len().saturating_sub(1)));
                let s = self.media.seq.as_mut().ok_or("no companion sequence chosen")?;
                if s.clip_folder != clip.folder {
                    return Err("the companion sequence was chosen for another clip".into());
                }
                s.mask = mask;
                if s.run.is_none() {
                    s.plan = relink::plan_sequence(&clip, &s.folder, mask, range);
                }
            }
            MediaCmd::StartSequenceImport => self.start_sequence_import(ctx),
            MediaCmd::CancelSequenceImport => {
                let s = self.media.seq.as_ref().ok_or("no companion sequence chosen")?;
                if s.run.is_some() {
                    return Err("the import is running; use Stop import".into());
                }
                self.media.seq = None;
            }
            MediaCmd::StopSequenceImport => {
                let (_, stop) = self
                    .media
                    .seq
                    .as_ref()
                    .and_then(|s| s.run.as_ref())
                    .ok_or("no import is running")?;
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        Ok(())
    }

    pub(super) fn media_has_actions(&self, clip: &Clip) -> bool {
        self.media.movie.as_ref().is_some_and(|m| m.folder == clip.folder)
            || self.media.seq.as_ref().is_some_and(|s| s.clip_folder == clip.folder)
    }

    pub(super) fn media_actions(&mut self, ui: &mut Ui, clip: &Clip) {
        let mut cmds = Vec::new();
        theme::section(ui, "Media preparation", |ui| {
            if let Some(m) = self.media.movie.as_ref().filter(|m| m.folder == clip.folder) {
                let d = self.extract_decision(clip, m);
                let response = ui.add_enabled(d.enabled, egui::Button::new(&d.label));
                if response.on_disabled_hover_text(d.reason).clicked() {
                    cmds.push(MediaCmd::ExtractRelinked);
                }
            }
            if let Some(s) = self.media.seq.as_ref().filter(|s| s.clip_folder == clip.folder) {
                ui.horizontal(|ui| {
                    if s.run.is_some() {
                        ui.label("Importing frames…");
                        if ui.button("Stop import").clicked() { cmds.push(MediaCmd::StopSequenceImport); }
                    } else {
                        if theme::accent(ui, s.plan.is_ok(), |ui| ui.add(egui::Button::new("Import mask frames"))).clicked() { cmds.push(MediaCmd::StartSequenceImport); }
                        if ui.button("Cancel import").clicked() { cmds.push(MediaCmd::CancelSequenceImport); }
                    }
                });
            }
        });
        let ctx = ui.ctx().clone();
        for c in cmds {
            if let Err(e) = self.media_cmd(&ctx, c) { self.media.note = Some(e); }
        }
    }

    pub(super) fn media_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let mut cmds = Vec::new();
        let movie_gone = clip.error.is_some()
            && matches!(clip.source, Source::Missing)
            && stored_filename(clip).is_some();
        if movie_gone {
            ui.separator();
            ui.label(RichText::new("Source movie missing").strong());
            if let Some(e) = &clip.error {
                ui.colored_label(theme::error(ui), e);
            }
            if self.media.movie.is_none() && ui.button("Relink movie…").on_hover_text("Choose the movie again: its size, frame rate and frame count are checked against what this clip recorded before anything is extracted").clicked() {
                cmds.push(MediaCmd::Relink);
            }
            if let Some(m) = &self.media.movie {
                ui.label(
                    RichText::new(m.path.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                match (&m.verdict, &m.probed) {
                    (None, _) => {
                        ui.label(RichText::new("Checking the movie…").color(theme::text_dim(ui)));
                    }
                    (Some(Err(e)), _) => {
                        ui.colored_label(
                            theme::warn(ui),
                            format!("Not the same footage: {e}. Nothing was changed."),
                        );
                    }
                    (Some(Ok(verified)), p) => {
                        if let Some(p) = p {
                            ui.label(format!(
                                "{}x{} · {:.3} fps{}",
                                p.width,
                                p.height,
                                p.fps,
                                p.frames.map_or(String::new(), |n| format!(" · {n} frames"))
                            ));
                        }
                        if *verified {
                            ui.label(
                                RichText::new(
                                    "Size, frame rate and frame count match the clip's records.",
                                )
                                .color(theme::text_dim(ui))
                                .size(12.0),
                            );
                        } else {
                            ui.colored_label(theme::warn(ui), "The clip kept no frame count to compare, so this match is partial (size and rate only).");
                            let mut confirm = m.confirm;
                            theme::checkbox(
                                ui,
                                &mut confirm,
                                "I confirm this is the same footage",
                            );
                            if confirm != m.confirm {
                                cmds.push(MediaCmd::ConfirmMovieMatch(confirm));
                            }
                        }

                    }
                }
                if ui.button("Cancel relink").clicked() {
                    cmds.push(MediaCmd::CancelRelink);
                }
            }
            if let Some(r) = self.ai.latest_task(&clip.root, TaskKind::PrepareSource) {
                self.ai.task_status_row(ui, &r);
            }
        }
        // Companion alpha / mask image sequence.
        if !clip.input_files.is_empty() {
            ui.separator();
            ui.label(RichText::new("Companion alpha / mask frames").strong());
            if self.media.seq.is_none() && !self.ai.external_writer(&clip.root) && ui.button("Import alpha or mask image sequence…").on_hover_text("PNG/EXR frames named like the source frames; checked for size and coverage of In/Out, existing hint frames are never replaced").clicked() {
                cmds.push(MediaCmd::ChooseCompanion);
            }
            if let Some(s) = self
                .media
                .seq
                .as_ref()
                .filter(|s| s.clip_folder == clip.folder)
            {
                ui.label(
                    RichText::new(s.folder.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                let mut mask = s.mask;
                ui.radio_value(&mut mask, false, "Alpha hint (AlphaHint)");
                ui.radio_value(&mut mask, true, "Mask (VideoMamaMaskHint)");
                if mask != s.mask {
                    cmds.push(MediaCmd::ImportAs { mask });
                }
                match &s.plan {
                    Err(e) => {
                        ui.colored_label(theme::warn(ui), format!("Cannot import: {e}"));
                    }
                    Ok(p) => {
                        ui.label(format!(
                            "{} frame(s) will be written to {}{}",
                            p.copy.len(),
                            p.dest.display(),
                            if p.kept.is_empty() {
                                String::new()
                            } else {
                                format!(
                                    "; {} existing frame(s) are kept, never replaced",
                                    p.kept.len()
                                )
                            }
                        ));
                    }
                }
                ui.label(RichText::new("Use the import action above the node graph.").color(theme::text_dim(ui)).size(12.0));
            }

        }
        let ctx = ui.ctx().clone();
        for c in cmds {
            if let Err(e) = self.media_cmd(&ctx, c) { self.media.note = Some(e); }
        }
        // The note (relink/extraction/import result) is shown in the activity strip.
    }
}

fn stored_filename(c: &Clip) -> Option<String> {
    let v: Value = std::fs::read(&c.meta_path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())?;
    v.get("source")?
        .get("filename")?
        .as_str()
        .map(str::to_owned)
}

/// Media page summary (bottom bar): the source, its span and interpretation, and one line about everything
/// already on disk. Every row is a real path from the loaded clip; nothing is guessed.
impl App {
    pub(super) fn media_details(&mut self, ui: &mut Ui, clip: &Clip) {
        let row = |ui: &mut Ui, label: &str, text: &str, copy: Option<&str>| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(label).color(theme::text_dim(ui)).size(11.5));
                ui.add(egui::Label::new(RichText::new(text).monospace().size(11.0)).truncate())
                    .on_hover_text(text);
                if let Some(c) = copy {
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(c.to_owned());
                    }
                }
            });
        };
        match &clip.source {
            Source::Sequence { dir, external } => row(
                ui,
                if *external {
                    "Source · external sequence"
                } else {
                    "Source · sequence"
                },
                &dir.display().to_string(),
                Some(&dir.display().to_string()),
            ),
            Source::Video { path } => row(
                ui,
                "Source · movie",
                &path.display().to_string(),
                Some(&path.display().to_string()),
            ),
            Source::Missing => {
                ui.label(RichText::new("Source · missing (relink in Properties)").color(theme::warn(ui)).size(11.5));
            }
        }
        let stems = &clip.input_files;
        let span = match (stems.first(), stems.last()) {
            (Some(a), Some(b)) if stems.len() > 1 => format!("{} … {}", a.0, b.0),
            (Some(a), _) => a.0.clone(),
            _ => "none".into(),
        };
        let size = clip.sample_size.map_or_else(
            || format!("{:.3} fps{}", clip.fps, if clip.fps_assumed { " (assumed)" } else { "" }),
            |(w, h)| {
                format!(
                    "{w}×{h} · {:.3} fps{}",
                    clip.fps,
                    if clip.fps_assumed { " (assumed)" } else { "" }
                )
            },
        );
        row(ui, "Frames", &format!("{} · {span}", stems.len()), None);
        row(
            ui,
            "Format",
            &format!(
                "{size} · {} · {}",
                if clip.input_is_linear { "linear" } else { "sRGB" },
                color_origin_text(clip)
            ),
            None,
        );
    }

    /// Full path inventory for the Media properties panel: every folder this clip reads or writes.
    pub(super) fn media_paths(&mut self, ui: &mut Ui, clip: &Clip) {
        let row = |ui: &mut Ui, label: &str, text: &str| {
            ui.label(RichText::new(label).color(theme::text_dim(ui)).size(11.5));
            ui.horizontal_wrapped(|ui| {
                ui.add(egui::Label::new(RichText::new(text).monospace().size(11.0)).truncate())
                    .on_hover_text(text);
                if ui.small_button("Copy").clicked() {
                    ui.ctx().copy_text(text.to_owned());
                }
            });
        };
        for (label, view, n) in [
            ("Pre-matte guide folder", View::Guide, clip.mask_frames),
            ("Final hint folder", View::Hint, clip.alpha_frames),
        ] {
            if let Some(p) = clip.view_dir(view) {
                row(ui, &format!("{label} ({n} frames)"), &p.display().to_string());
            }
        }
        if let Some(d) = &clip.output_dir {
            row(ui, "Output folder", &d.display().to_string());
        }
        if let Some(m) = &clip.manifest_dir {
            row(ui, "Last run wrote to", &m.display().to_string());
        }
        if let Some(r) = clip.result_range_text() {
            ui.label(RichText::new(format!("Result covers {r}")).color(theme::text_dim(ui)).size(11.5));
        }
    }
}

/// An unstarted worker is fine: the job queues and the scheduler starts it.
fn prepare_source_worker_ok(unstarted: bool, capable: bool) -> bool {
    unstarted || capable
}

#[cfg(test)]
mod prepare_source_tests {
    use super::prepare_source_worker_ok as ok;
    #[test]
    fn available_before_the_worker_starts() {
        assert!(ok(true, false));
        assert!(ok(false, true));
        assert!(!ok(false, false));
    }
}
