//! Media-side recovery and import: relink a vanished movie (probe + identity check, then validated frame
//! extraction through the worker) and import a companion alpha/mask IMAGE SEQUENCE (validated, no overwrite).
use super::*;
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
            self.ai.submit_task(&clip, job, vec![], 0).map(|_| ())
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

    pub(super) fn media_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let caps = self.ai.local_caps();
        let prep_ok = caps
            .as_ref()
            .is_some_and(|c| c.tasks.get("prepare_source").copied().unwrap_or(false));
        let movie_gone = clip.error.is_some()
            && matches!(clip.source, Source::Missing)
            && stored_filename(clip).is_some();
        if movie_gone {
            ui.separator();
            ui.label(RichText::new("Source movie missing").strong());
            if self.media.movie.is_none() && ui.button("Relink movie…").on_hover_text("Choose the movie again: its size, frame rate and frame count are checked against what this clip recorded before anything is extracted").clicked() {
                let ctx = ui.ctx().clone();
                self.request_pick(&ctx, PickKind::RelinkMovie);
            }
            let (mut go, mut cancel_relink) = (false, false);
            if let Some(m) = &mut self.media.movie {
                ui.label(
                    RichText::new(m.path.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                match (&m.verdict, &m.probed) {
                    (None, _) => {
                        ui.label(RichText::new("Checking the movie…").color(DIM));
                    }
                    (Some(Err(e)), _) => {
                        ui.colored_label(
                            WARN,
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
                                .color(DIM)
                                .size(12.0),
                            );
                        } else {
                            ui.colored_label(WARN, "The clip kept no frame count to compare, so this match is partial (size and rate only).");
                            theme::checkbox(
                                ui,
                                &mut m.confirm,
                                "I confirm this is the same footage",
                            );
                        }
                        let ok = prep_ok && (*verified || m.confirm);
                        go = ui
                            .add_enabled(ok, egui::Button::new("Extract frames from this movie"))
                            .on_disabled_hover_text(if prep_ok {
                                "Confirm first"
                            } else {
                                "The worker cannot extract frames (FFmpeg)"
                            })
                            .clicked();
                    }
                }
                cancel_relink = ui.button("Cancel relink").clicked();
            }
            if go {
                let _ = self.extract_relinked_movie();
            }
            if cancel_relink {
                self.media.movie = None;
            }
            if let Some(r) = self.ai.latest_task(&clip.folder, TaskKind::PrepareSource) {
                self.ai.task_status_row(ui, &r);
            }
        }
        // Companion alpha / mask image sequence.
        if !clip.input_files.is_empty() {
            ui.separator();
            ui.label(RichText::new("Companion alpha / mask frames").strong());
            if self.media.seq.is_none() && !self.ai.external_writer(&clip.root) && ui.button("Import alpha or mask image sequence…").on_hover_text("PNG/EXR frames named like the source frames; checked for size and coverage of In/Out, existing hint frames are never replaced").clicked() {
                let ctx = ui.ctx().clone();
                self.request_pick(&ctx, PickKind::HintSequence);
            }
            let range = self
                .ai_open_range()
                .unwrap_or((0, clip.input_files.len() - 1));
            let mut start = false;
            let mut cancel = false;
            if let Some(s) = self
                .media
                .seq
                .as_mut()
                .filter(|s| s.clip_folder == clip.folder)
            {
                ui.label(
                    RichText::new(s.folder.display().to_string())
                        .monospace()
                        .size(11.0),
                );
                let before = s.mask;
                ui.radio_value(&mut s.mask, false, "Alpha hint (AlphaHint)");
                ui.radio_value(&mut s.mask, true, "Mask (VideoMamaMaskHint)");
                if s.mask != before && s.run.is_none() {
                    s.plan = relink::plan_sequence(clip, &s.folder, s.mask, range);
                }
                match &s.plan {
                    Err(e) => {
                        ui.colored_label(WARN, format!("Cannot import: {e}"));
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
                ui.horizontal(|ui| {
                    if s.run.is_some() {
                        ui.label("Importing…");
                        if ui.button("Stop").clicked() {
                            if let Some((_, c)) = &s.run {
                                c.store(true, std::sync::atomic::Ordering::Relaxed);
                            }
                        }
                    } else {
                        start = ui
                            .add_enabled(s.plan.is_ok(), egui::Button::new("Import"))
                            .clicked();
                        cancel = ui.button("Cancel").clicked();
                    }
                });
            }
            if start {
                self.start_sequence_import(ui.ctx());
            }
            if cancel {
                self.media.seq = None;
            }
        }
        if let Some(n) = &self.media.note {
            ui.label(RichText::new(n).color(WARN).size(12.0));
        }
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
