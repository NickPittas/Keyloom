//! Hint page: route/model selection, SAM/paint annotation editing, preview/track/generate jobs, keyframe marks,
//! companion alpha/mask movies, frame extraction and the Deliver video export. Everything persistent lives in
//! `clip.json["ck_native"]` (see `persist`/`hints`); every job is built from a frozen snapshot of that state and
//! submitted to the shared scheduler. Nothing here talks to a model: the worker does, or the control stays off.
use super::*;
use crate::hints::{
    self, Annotations, Ctx, ExportCfg, HintCfg, Route, BIREFNET_VARIANTS, CONTAINERS, SAM_MODELS,
};
use crate::tasks::TaskKind;
use crate::ui::ai::sig_of;
use crate::ui::annotate::{self, Ann, Tool};
use crate::workflow::new_id;

#[derive(Default)]
pub struct State {
    folder: Option<String>,
    cfg: HintCfg,
    ann: Annotations,
    undo: Vec<Annotations>,
    holdout: hints::Holdout,
    hold_undo: Vec<hints::Holdout>,
    note: Option<String>,
    /// Picked companion movie waiting for its kind/channel to be confirmed.
    movie: Option<PathBuf>,
    movie_mask: bool,
    movie_alpha_channel: bool,
    export_product: String,
    export_container: usize,
    export_fps: Option<f64>,
    export_partial: bool,
    export_dir: Option<PathBuf>,
    presets: Vec<crate::presets::Preset>,
    presets_loaded: bool,
    preset_name: String,
    preset_sel: String,
}

/// What a preview depends on: the frame, its prompts, the tracker model and the colour reading.
fn psig(h: &State, stem: &str, linear: bool) -> u64 {
    let f = h
        .ann
        .frames
        .get(stem)
        .map(|f| serde_json::to_string(f).unwrap_or_default())
        .unwrap_or_default();
    sig_of(&[stem, &h.cfg.sam_model, &f, &linear.to_string()])
}

fn stems_of(c: &Clip) -> Vec<String> {
    c.input_files.iter().map(|f| f.0.clone()).collect()
}

impl App {
    fn hint_note(&mut self, m: impl Into<String>) {
        self.hint.note = Some(m.into());
    }

    /// Called once per frame: loads this clip's saved hint state, applies viewer edits and feeds the overlay.
    pub(super) fn hint_sync(&mut self) {
        let (Some(p), Some(i), Some(v)) = (self.project.as_ref(), self.open, self.viewer.as_mut())
        else {
            self.hint.folder = None;
            return;
        };
        let clip = &p.clips[i];
        if self.tab != Tab::Hint {
            v.ann.tool = Tool::None;
        }
        if self.hint.folder.as_deref() != Some(clip.folder.as_str()) {
            self.hint = State {
                folder: Some(clip.folder.clone()),
                ..Default::default()
            };
            self.hint.cfg = HintCfg::load(clip);
            match Annotations::load(clip) {
                Ok(a) => self.hint.ann = a,
                Err(e) => self.hint.note = Some(e),
            }
            match hints::Holdout::load(clip) {
                Ok(h) => self.hint.holdout = h,
                Err(e) => self.hint.note = Some(e),
            }
            let left = hints::interrupted_publish(&clip.root);
            if !left.is_empty() {
                self.hint.note = Some(format!("An interrupted hint publication left {} aside; running any hint job on this clip (or clearing its lock) restores it.", left.join(", ")));
            }
            self.ai.probe_workers();
        }
        let src = stems_of(clip);
        let stem = v
            .index
            .stems
            .get(v.playhead)
            .filter(|s| src.contains(s))
            .cloned();
        // Marks as timeline ticks.
        v.marks = clip
            .keyframes
            .iter()
            .filter_map(|k| src.get(*k).and_then(|s| v.index.index_of(s)))
            .collect();
        let events = std::mem::take(&mut v.ann.events);
        if !events.is_empty() {
            let Some(stem) = stem.clone() else {
                self.hint.note =
                    Some("This frame is not a source frame; move to one to annotate.".into());
                return;
            };
            let before = self.hint.ann.clone();
            let (mut changed, mut size_err) = (false, None);
            let mut hold_before: Option<hints::Holdout> = None;
            for e in events {
                match e {
                    Ann::Pick { x, y } => {
                        let (lin, st) = (v.linear, v.straight);
                        match v.index.availability(View::Input, v.playhead, None) {
                            Availability::Present(path) => match crate::decode::decode_display(
                                &path,
                                crate::frames::view_interp(View::Input, &path, lin, st),
                            ) {
                                Ok(f) => {
                                    let (px, py) = (
                                        (x as usize).min(f.width - 1),
                                        (y as usize).min(f.height - 1),
                                    );
                                    let o = (py * f.width + px) * 4;
                                    self.hint.cfg.chroma.screen_color =
                                        Some([f.rgba[o], f.rgba[o + 1], f.rgba[o + 2]]);
                                    let cfg = self.hint.cfg.clone();
                                    if let Err(e) = cfg.save(clip) {
                                        size_err = Some(format!("not saved: {e}"));
                                    }
                                }
                                Err(e) => size_err = Some(format!("could not read the frame: {e}")),
                            },
                            _ => size_err = Some("no source frame on screen".into()),
                        }
                    }
                    Ann::Hold(st) => {
                        hold_before.get_or_insert_with(|| self.hint.holdout.clone());
                        self.hint.holdout.strokes.push(st);
                    }
                    Ann::HoldErase { x, y, r } => {
                        let before = self.hint.holdout.clone();
                        if self.hint.holdout.erase(x, y, r) {
                            hold_before.get_or_insert(before);
                        }
                    }
                    other => {
                        if self.hint.ann.size.is_none() {
                            self.hint.ann.size = clip.sample_size;
                        }
                        changed |= annotate::apply(self.hint.ann.frame_mut(&stem), &other);
                    }
                }
            }
            if let Some(b) = hold_before {
                self.hint.hold_undo.push(b);
                self.hint.hold_undo.truncate(50);
                if let Err(e) = self.hint.holdout.save(clip) {
                    size_err = Some(format!("Holdout not saved: {e}"));
                }
            }
            if changed {
                self.hint.undo.push(before);
                self.hint.undo.truncate(50);
                match self.hint.ann.save(clip) {
                    Ok(()) => self.hint.note = None,
                    Err(e) => self.hint.note = Some(format!("Annotations not saved: {e}")),
                }
            }
            if let Some(e) = size_err {
                self.hint.note = Some(e);
            }
        }
        v.ann.holdout = self.hint.holdout.strokes.clone();
        v.ann.overlay = stem
            .as_ref()
            .and_then(|s| self.hint.ann.frames.get(s).cloned())
            .unwrap_or_default();
        v.ann.preview = stem.as_ref().and_then(|s| {
            let sig = psig(&self.hint, s, v.linear);
            self.ai
                .current_preview(&clip.folder, s, sig)
                .map(|r| r.mask_png.clone())
        });
    }

    fn preview_sig(&self, stem: &str) -> u64 {
        psig(
            &self.hint,
            stem,
            self.viewer.as_ref().is_some_and(|v| v.linear),
        )
    }
}

impl App {
    pub(super) fn open_clip_snapshot(&self) -> Option<Clip> {
        self.clip().cloned()
    }

    fn submit_task_job(
        &mut self,
        clip: &Clip,
        job: Result<crate::tasks::TaskJob, String>,
        sig: u64,
        what: &str,
    ) {
        match job.and_then(|j| self.ai.submit_task(clip, j, vec![], sig)) {
            Ok(_) => self.hint.note = Some(format!("{what} queued.")),
            Err(e) => self.hint_note(format!("{what} not queued: {e}")),
        }
    }

    fn hint_preview(&mut self) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        let Some(stem) = self
            .viewer
            .as_ref()
            .and_then(|v| v.index.stems.get(v.playhead).cloned())
        else {
            return;
        };
        let stems = stems_of(&clip);
        if !stems[range.0..=range.1].contains(&stem) {
            return self.hint_note("Preview a frame inside In/Out (or clear In/Out).");
        }
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        let sig = self.preview_sig(&stem);
        let job = Ctx {
            clip: &clip,
            range,
            job_id: new_id(),
            staging: hints::staging_root(
                &self.project.as_ref().unwrap().root,
                &clip.root,
                &clip.folder,
            ),
        }
        .sam_preview(&self.hint.cfg, &self.hint.ann, &stem, lin);
        self.submit_task_job(&clip, job, sig, "SAM preview");
    }

    fn hint_track(&mut self) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        let job = Ctx {
            clip: &clip,
            range,
            job_id: new_id(),
            staging: hints::staging_root(
                &self.project.as_ref().unwrap().root,
                &clip.root,
                &clip.folder,
            ),
        }
        .sam_track(&self.hint.cfg, &self.hint.ann, lin);
        self.submit_task_job(&clip, job, 0, "SAM tracking");
    }

    /// Generate the selected route's hint (mask routes first track SAM prompts unless a mask already covers the
    /// range). Replaces only the frames of In/Out; the previous hint stays until the new set is validated.
    fn hint_generate(&mut self) {
        let (Some(clip), Ok(range)) = (self.open_clip_snapshot(), self.ai_open_range()) else {
            return self.hint_note("Open a clip with source frames first.");
        };
        // An explicit Generate uses (and saves) the route shown, even before any setting was touched.
        if clip.native.get("hint").is_none() {
            if let Err(e) = self
                .hint
                .cfg
                .validate()
                .and_then(|()| self.hint.cfg.save(&clip))
            {
                return self.hint_note(format!("Hint settings not saved: {e}"));
            }
        }
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        let root = self.project.as_ref().unwrap().root.clone();
        let steps = hints::plan_steps(
            &clip,
            &self.hint.cfg,
            "full",
            range,
            lin,
            true,
            &hints::staging_root(&root, &clip.root, &clip.folder),
            new_id,
        );
        match steps.and_then(|s| self.ai.submit_chain(&clip, s, None)) {
            Ok(_) => {
                self.hint.note = Some(format!(
                    "{} queued for frames {}–{}.",
                    self.hint.cfg.route.label(),
                    range.0,
                    range.1
                ))
            }
            Err(e) => self.hint_note(format!("Not queued: {e}")),
        }
    }

    fn hint_marks(&mut self, add: Option<bool>) {
        let (Some(p), Some(i), Some(v)) = (self.project.as_ref(), self.open, self.viewer.as_ref())
        else {
            return;
        };
        let clip = &p.clips[i];
        let stems = stems_of(clip);
        let Some(idx) = v
            .index
            .stems
            .get(v.playhead)
            .and_then(|s| stems.iter().position(|x| x == s))
        else {
            return self.hint_note("This frame is not a source frame.");
        };
        let mut marks = clip.keyframes.clone();
        match add {
            Some(true) => marks.push(idx),
            Some(false) => marks.retain(|m| *m != idx),
            None => marks.clear(),
        }
        let folder = clip.folder.clone();
        match crate::persist::save_keyframes(&clip.root, &marks, &stems) {
            Ok(()) => {
                self.hint.note = None;
                let ctx = self.ctx_handle();
                self.refresh_clip(&ctx, &folder);
            }
            Err(e) => self.hint_note(format!("Marks not saved: {e}")),
        }
    }

    fn ctx_handle(&self) -> egui::Context {
        self.last_ctx.clone().unwrap_or_default()
    }

    /// A dialog result: `movie` = companion alpha/mask movie, else the export folder.
    pub(super) fn hint_picked(&mut self, path: PathBuf, movie: bool) {
        if movie {
            self.hint.movie = Some(path);
        } else {
            self.hint.export_dir = Some(path);
        }
    }

    fn source_movie(clip: &Clip) -> Option<PathBuf> {
        match &clip.source {
            Source::Video { path } => Some(path.clone()),
            _ => std::fs::read_dir(clip.root.join("Source"))
                .ok()?
                .flatten()
                .map(|e| e.path())
                .find(|p| crate::project::VIDEO_EXTS.contains(&ext_of(p).as_str())),
        }
    }

    fn undo_annotation(&mut self) {
        if let (Some(prev), Some(c)) = (self.hint.undo.pop(), self.open_clip_snapshot()) {
            self.hint.ann = prev;
            if let Err(e) = self.hint.ann.save(&c) {
                self.hint_note(format!("Annotations not saved: {e}"));
            }
        }
    }

    // ------------------------------------------------------------------- page bar (under the viewer)

    /// Page bar under the viewer. Order is by importance and everything is visible without scrolling at
    /// 1440x900: (1) the actions, (2) the tools, (3) brush/undo/clears; status lines follow below.
    pub(super) fn hint_tools(&mut self, ui: &mut Ui, clip: &Clip) {
        let local = self.ai.local_caps();
        let route_ready = |caps: &Option<crate::worker::Capabilities>, k: TaskKind| {
            caps.as_ref()
                .is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false))
        };
        let busy = self.ai.task_busy(&clip.folder);
        let sam_ok =
            route_ready(&local, TaskKind::SamPreview) && route_ready(&local, TaskKind::SamTrack);
        let stem_ann = self
            .viewer
            .as_ref()
            .and_then(|v| v.index.stems.get(v.playhead))
            .and_then(|s| self.hint.ann.frames.get(s))
            .is_some_and(|f| !f.is_empty());
        let route = self.hint.cfg.route;
        let ready = route.task().is_some_and(|k| route_ready(&local, k));
        // Row 1: the actions.
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(sam_ok && stem_ann && !busy, egui::Button::new("Preview SAM"))
                .on_hover_text("Run SAM on this frame's prompts (nothing is written to the hint)")
                .on_disabled_hover_text(if !sam_ok {
                    reason(&local, "SAM")
                } else if busy {
                    "A hint job is running for this clip".into()
                } else {
                    "Add a point, box or paint on this frame first".into()
                })
                .clicked()
            {
                self.hint_preview();
            }
            if ui
                .add_enabled(sam_ok && !busy, egui::Button::new("Track mask"))
                .on_hover_text("Track the saved prompts over In/Out into a dense mask hint")
                .on_disabled_hover_text(if busy { "A hint job is running for this clip".into() } else { reason(&local, "SAM") })
                .clicked()
            {
                self.hint_track();
            }
            if ui
                .add_enabled(
                    ready && !busy && !self.hint.cfg.bypass,
                    egui::Button::new(format!("Generate hint: {}", route.label())),
                )
                .on_hover_text("Generate the selected route's hint for In/Out; the previous hint stays until the new set is validated")
                .on_disabled_hover_text(if self.hint.cfg.bypass {
                    "Bypass is on".to_string()
                } else if busy {
                    "A hint job is running for this clip".to_string()
                } else if route == Route::Import {
                    "Import alpha has nothing to generate: use Import alpha / mask movie in Properties".to_string()
                } else {
                    reason(&local, route.label())
                })
                .clicked()
            {
                self.hint_generate();
            }
            ui.separator();
            if ui.button("Mark frame").on_hover_text("Add a sparse keyframe mark here").clicked() {
                self.hint_marks(Some(true));
            }
            if ui.button("Unmark").clicked() {
                self.hint_marks(Some(false));
            }
            if ui
                .button("Clear marks")
                .on_hover_text("Removes only the keyframe marks, never hint frames")
                .clicked()
            {
                self.hint_marks(None);
            }
        });
        // Row 2: tools (full names on hover).
        let Some(v) = self.viewer.as_mut() else {
            return;
        };
        ui.horizontal_wrapped(|ui| {
            for t in Tool::ALL {
                if ui
                    .selectable_label(v.ann.tool == t, t.short())
                    .on_hover_text(t.label())
                    .clicked()
                {
                    v.ann.tool = t;
                    if t != Tool::None {
                        v.view = View::Input; // annotations are in source pixels of the Input frame
                        v.ab = false;
                    }
                }
            }
        });
        // Row 3: brush, undo and the clears (each names exactly what it clears).
        let (mut undo, mut hold_undo, mut clr_frame, mut clr_all, mut clr_hold) =
            (false, false, false, false, false);
        ui.horizontal_wrapped(|ui| {
            let mut b = v.ann.brush as f32;
            ui.add(egui::Slider::new(&mut b, 2.0..=120.0).text("brush px").show_value(true));
            v.ann.brush = b as f64;
            undo = ui.button("Undo prompt").on_hover_text("Undo the last SAM/paint prompt edit").clicked();
            hold_undo = ui.button("Undo holdout").clicked();
            clr_frame = ui.button("Clear frame prompts").on_hover_text("SAM/paint prompts of this frame only").clicked();
            clr_all = ui
                .button("Clear all prompts")
                .on_hover_text("All SAM/paint prompts; source, hints, holdout and outputs are untouched")
                .clicked();
            clr_hold = ui
                .button("Clear holdout")
                .on_hover_text("The chroma holdout strokes only (holdout_strokes.json); prompts and hints are untouched")
                .clicked();
        });
        let stem_now = v.index.stems.get(v.playhead).cloned();
        if undo {
            self.undo_annotation();
        }
        if hold_undo || clr_hold {
            if let Some(c) = self.open_clip_snapshot() {
                let prev = if clr_hold {
                    Some(hints::Holdout::default())
                } else {
                    self.hint.hold_undo.pop()
                };
                if let Some(p) = prev {
                    if clr_hold {
                        self.hint.hold_undo.push(self.hint.holdout.clone());
                    }
                    self.hint.holdout = p;
                    if let Err(e) = self.hint.holdout.save(&c) {
                        self.hint_note(format!("Holdout not saved: {e}"));
                    }
                }
            }
        }
        if clr_frame || clr_all {
            if let Some(c) = self.open_clip_snapshot() {
                self.hint.undo.push(self.hint.ann.clone());
                match (clr_all, stem_now) {
                    (true, _) => self.hint.ann.frames.clear(),
                    (false, Some(s)) => {
                        self.hint.ann.frames.remove(&s);
                    }
                    _ => {}
                }
                if let Err(e) = self.hint.ann.save(&c) {
                    self.hint_note(format!("Annotations not saved: {e}"));
                }
            }
        }
        // Status (scrolls if long).
        ui.label(
            RichText::new(format!(
                "AlphaHint {} frames, mask hint {} frames, marks {}, holdout {} strokes",
                clip.alpha_frames,
                clip.mask_frames,
                if clip.keyframes.is_empty() {
                    "none".into()
                } else {
                    format!("{:?}", clip.keyframes)
                },
                self.hint.holdout.strokes.len()
            ))
            .color(DIM)
            .size(12.0),
        );
        self.ai.hint_lock_ui(ui, clip);
        if let Some(r) = self.ai.latest_task(&clip.folder, TaskKind::SamPreview) {
            ui.label(RichText::new("Last preview:").color(DIM).size(12.0));
            self.ai.task_status_row(ui, &r);
        }
        for k in [
            TaskKind::SamTrack,
            TaskKind::Gvm,
            TaskKind::Birefnet,
            TaskKind::Chroma,
            TaskKind::Matanyone2,
            TaskKind::Videomama,
            TaskKind::ImportHint,
            TaskKind::PrepareSource,
        ] {
            if let Some(r) = self.ai.latest_task(&clip.folder, k) {
                self.ai.task_status_row(ui, &r);
            }
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(WARN).size(12.0));
        }
    }

    // ------------------------------------------------------------------- properties

    pub(super) fn hint_props(&mut self, ui: &mut Ui, clip: &Clip) {
        let caps = self.ai.local_caps();
        let cfg_before = self.hint.cfg.clone();
        ui.label(RichText::new("Hint route").strong());
        theme::checkbox(
            ui,
            &mut self.hint.cfg.bypass,
            "Bypass hint (keeps every setting and asset)",
        );
        ui.add_enabled_ui(!self.hint.cfg.bypass, |ui| {
            egui::ComboBox::from_id_salt("hint_route").selected_text(self.hint.cfg.route.label()).show_ui(ui, |ui| {
                for r in Route::ALL {
                    let ok = r.task().is_none_or(|k| caps.as_ref().is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false)));
                    let l = if ok { r.label().to_string() } else { format!("{} (not ready)", r.label()) };
                    ui.selectable_value(&mut self.hint.cfg.route, r, l);
                }
            });
            match self.hint.cfg.route {
                Route::Birefnet => {
                    let have = caps.as_ref().map(|c| c.birefnet_variants.clone()).unwrap_or_default();
                    egui::ComboBox::from_id_salt("birefnet_variant").selected_text(self.hint.cfg.birefnet_variant.clone()).show_ui(ui, |ui| {
                        for v in BIREFNET_VARIANTS {
                            let l = if have.iter().any(|h| h == v) { v.to_string() } else { format!("{v} (not installed)") };
                            ui.selectable_value(&mut self.hint.cfg.birefnet_variant, v.to_string(), l);
                        }
                    });
                }
                Route::Chroma => self.chroma_controls(ui),
                Route::Videomama => {
                    let mut c = self.hint.cfg.videomama_chunk as i32;
                    ui.add(egui::Slider::new(&mut c, 1..=64).text("chunk size"));
                    self.hint.cfg.videomama_chunk = c as u32;
                    ui.label(RichText::new("Needs a dense mask over In/Out: Track mask or import a mask movie first.").color(DIM).size(12.0));
                }
                Route::Matanyone2 => {
                    ui.label(RichText::new("Seeds from the mask on the In frame (Track mask, paint or import one).").color(DIM).size(12.0));
                }
                Route::Import | Route::Gvm => {}
            }
        });
        ui.separator();
        ui.label(RichText::new("SAM tracker model").strong());
        let sam = caps
            .as_ref()
            .map(|c| c.sam_models.clone())
            .unwrap_or_default();
        egui::ComboBox::from_id_salt("sam_model")
            .selected_text(self.hint.cfg.sam_model.clone())
            .show_ui(ui, |ui| {
                for (id, l) in SAM_MODELS {
                    let t = if sam.iter().any(|s| s == id) {
                        l.to_string()
                    } else {
                        format!("{l} (not installed)")
                    };
                    ui.selectable_value(&mut self.hint.cfg.sam_model, id.to_string(), t);
                }
            });
        if self.hint.cfg != cfg_before {
            let c = self.hint.cfg.clone();
            if let Err(e) = c.validate().and_then(|()| c.save(clip)) {
                self.hint_note(format!("Hint settings not saved: {e}"));
            }
        }
        ui.separator();
        ui.label(RichText::new("Source / companion movies").strong());
        if let Some(src) =
            Self::source_movie(clip).filter(|_| !matches!(clip.source, Source::Sequence { .. }))
        {
            let ok = caps
                .as_ref()
                .is_some_and(|c| c.tasks.get("prepare_source").copied().unwrap_or(false));
            if ui.add_enabled(ok && !self.ai.task_busy(&clip.folder), egui::Button::new("Extract frames (validated)")).on_hover_text("Copies Source/ movie into a verified Frames/ sequence; cancellable and resumable").clicked() {
                let job = Ctx { clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.prepare_source(&src);
                let c = clip.clone();
                self.submit_task_job(&c, Ok(job), 0, "Frame extraction");
            }
        }
        ui.horizontal_wrapped(|ui| {
            if ui.button("Import alpha / mask movie…").clicked() {
                let ctx = ui.ctx().clone();
                self.request_pick(&ctx, PickKind::HintMovie);
            }
        });
        if let Some(m) = self.hint.movie.clone() {
            ui.label(
                RichText::new(m.display().to_string())
                    .monospace()
                    .size(11.0),
            );
            ui.radio_value(&mut self.hint.movie_mask, false, "Alpha hint (AlphaHint)");
            ui.radio_value(
                &mut self.hint.movie_mask,
                true,
                "Mask (VideoMamaMaskHint, for MatAnyone2/VideoMaMa)",
            );
            theme::checkbox(
                ui,
                &mut self.hint.movie_alpha_channel,
                "Matte is in the alpha channel (else gray)",
            );
            let src = Self::source_movie(clip);
            let ok = caps
                .as_ref()
                .is_some_and(|c| c.tasks.get("import_hint").copied().unwrap_or(false));
            ui.horizontal(|ui| {
                let go = ui.add_enabled(ok && src.is_some(), egui::Button::new("Import")).on_disabled_hover_text(if src.is_none() { "Companion movies need a movie source (Source/); import PNG/EXR sequences by placing them in AlphaHint" } else { "the worker cannot import movies (FFmpeg)" }).clicked();
                if go {
                    let job = Ctx { clip, range: (0, 0), job_id: new_id(), staging: PathBuf::new() }.import_hint(&m, src.as_ref().unwrap(), self.hint.movie_mask, self.hint.movie_alpha_channel);
                    let c = clip.clone();
                    self.submit_task_job(&c, Ok(job), 0, "Hint movie import");
                    self.hint.movie = None;
                }
                if ui.button("Cancel").clicked() {
                    self.hint.movie = None;
                }
            });
        }
        ui.separator();
        if !self.hint.cfg.bypass {
            ui.label(
                RichText::new("Consumption mode and quality are below (applied by Key).")
                    .color(DIM)
                    .size(12.0),
            );
        }
    }

    fn chroma_controls(&mut self, ui: &mut Ui) {
        let c = &mut self.hint.cfg.chroma;
        ui.horizontal(|ui| {
            ui.selectable_value(&mut c.screen_type, "green".to_string(), "Green");
            ui.selectable_value(&mut c.screen_type, "blue".to_string(), "Blue");
        });
        match c.screen_color {
            Some(rgb) => {
                ui.horizontal(|ui| {
                    ui.label(format!("Screen colour {:?}", rgb));
                    if ui.small_button("auto").clicked() {
                        c.screen_color = None;
                    }
                });
            }
            None => {
                ui.label(RichText::new("Screen colour: automatic. Use the “Pick chroma colour” tool on the frame to sample one.").color(DIM).size(12.0));
            }
        }
        ui.add(egui::Slider::new(&mut c.strength, 0.1..=10.0).text("strength"));
        ui.add(egui::Slider::new(&mut c.clip_black, 0.0..=1.0).text("clip black"));
        ui.add(egui::Slider::new(&mut c.clip_white, 0.0..=1.0).text("clip white"));
        ui.add(egui::Slider::new(&mut c.shrink_grow, -100..=100).text("shrink/grow px"));
        ui.add(egui::Slider::new(&mut c.edge_blur, 0..=100).text("edge blur px"));
    }

    // ------------------------------------------------------------------- Deliver: video export

    pub(super) fn export_ui(&mut self, ui: &mut Ui, clip: &Clip) {
        ui.label(RichText::new("Video export").strong());
        egui::CollapsingHeader::new("Export presets")
            .default_open(false)
            .show(ui, |ui| self.preset_ui(ui, clip, true));
        let caps = self.ai.local_caps();
        let ok = caps
            .as_ref()
            .is_some_and(|c| c.tasks.get("export_video").copied().unwrap_or(false));
        let (Some(m), Some(dir)) = (&clip.manifest, &clip.manifest_dir) else {
            ui.label(
                RichText::new("No Key result with a manifest yet: export needs one.").color(DIM),
            );
            return;
        };
        let products: Vec<String> = m.enabled.clone();
        if products.is_empty() {
            ui.label(RichText::new("The result lists no outputs.").color(DIM));
            return;
        }
        if !products.contains(&self.hint.export_product) {
            self.hint.export_product = products[0].clone();
        }
        egui::ComboBox::from_id_salt("exp_product")
            .selected_text(self.hint.export_product.clone())
            .show_ui(ui, |ui| {
                for p in &products {
                    ui.selectable_value(&mut self.hint.export_product, p.clone(), p);
                }
            });
        let alpha_ok = self.hint.export_product == "processed";
        if !alpha_ok && CONTAINERS[self.hint.export_container].0 != "mp4" {
            self.hint.export_container = 0; // alpha containers only carry the processed product
        }
        egui::ComboBox::from_id_salt("exp_container")
            .selected_text(CONTAINERS[self.hint.export_container].1)
            .show_ui(ui, |ui| {
                for (i, (id, l, _)) in CONTAINERS.iter().enumerate() {
                    let on = *id == "mp4" || alpha_ok;
                    ui.add_enabled_ui(on, |ui| {
                        ui.selectable_value(&mut self.hint.export_container, i, *l)
                    });
                }
            });
        ui.label(RichText::new(if alpha_ok { "Alpha formats carry the Processed (RGBA) product; EXR is encoded with the sRGB transfer." } else { "MP4 has no alpha channel. Alpha MOV/WebM need the Processed product." }).color(DIM).size(12.0));
        let mut fps = self.hint.export_fps.unwrap_or(clip.fps);
        ui.add(
            egui::DragValue::new(&mut fps)
                .speed(0.01)
                .range(0.001..=1000.0)
                .suffix(" fps"),
        );
        self.hint.export_fps = Some(fps);
        ui.label(
            RichText::new("Audio is not included in exported videos.")
                .color(DIM)
                .size(12.0),
        );
        if !m.complete || m.partial_range {
            theme::checkbox(
                ui,
                &mut self.hint.export_partial,
                "Export the partial result (explicitly chosen range, no gaps)",
            );
        }
        let default_dir = self
            .project
            .as_ref()
            .map(|p| p.root.join("Exports").join(&clip.folder));
        let dest_dir = self.hint.export_dir.clone().or(default_dir);
        ui.horizontal_wrapped(|ui| {
            ui.label(format!(
                "Folder: {}",
                dest_dir
                    .as_ref()
                    .map_or("?".into(), |d| d.display().to_string())
            ));
            if ui.button("Choose…").clicked() {
                let ctx = ui.ctx().clone();
                self.request_pick(&ctx, PickKind::ExportFolder);
            }
        });
        let range = self.ai_open_range();
        let stems = stems_of(clip);
        let name = range.as_ref().ok().map(|(a, b)| {
            format!(
                "{}_{}_{}-{}",
                clip.name, self.hint.export_product, stems[*a], stems[*b]
            )
        });
        let ext = CONTAINERS[self.hint.export_container].2;
        let dest = dest_dir.zip(name).map(|(d, n)| unique(&d, &n, ext));
        if let Some(d) = &dest {
            ui.label(
                RichText::new(d.display().to_string())
                    .monospace()
                    .size(11.0),
            );
        }
        let busy = self.ai.task_busy(&clip.folder);
        if ui
            .add_enabled(
                ok && dest.is_some() && range.is_ok() && !busy,
                egui::Button::new("Export video"),
            )
            .on_disabled_hover_text(if ok {
                "Set a valid range and folder"
            } else {
                "FFmpeg is not ready on the worker"
            })
            .clicked()
        {
            let cfg = ExportCfg {
                product: self.hint.export_product.clone(),
                manifest_dir: dir.display().to_string(),
                dest: dest.unwrap().display().to_string(),
                container: CONTAINERS[self.hint.export_container].0.into(),
                fps,
                allow_partial: self.hint.export_partial,
            };
            let r = range.unwrap();
            let job = Ctx {
                clip,
                range: r,
                job_id: new_id(),
                staging: PathBuf::new(),
            }
            .export(&cfg);
            let c = clip.clone();
            self.submit_task_job(&c, job, 0, "Video export");
        }
        if let Some(r) = self.ai.latest_task(&clip.folder, TaskKind::ExportVideo) {
            self.ai.task_status_row(ui, &r);
        }
        if let Some(n) = &self.hint.note {
            ui.label(RichText::new(n).color(WARN).size(12.0));
        }
    }
}

fn reason(caps: &Option<crate::worker::Capabilities>, what: &str) -> String {
    match caps {
        None => "The worker is not ready yet (starting or unavailable).".into(),
        Some(c) if c.task_unavailable.is_empty() => {
            format!("{what} is not installed or verified on the worker: see Settings > Models.")
        }
        Some(c) => c
            .task_unavailable
            .values()
            .cloned()
            .collect::<Vec<_>>()
            .join("; "),
    }
}

/// `<dir>/<name>.<ext>`, or `_v2`... when something is already there: an export never replaces a file.
fn unique(dir: &std::path::Path, name: &str, ext: &str) -> PathBuf {
    let first = dir.join(format!("{name}.{ext}"));
    if !first.exists() {
        return first;
    }
    (2..1000)
        .map(|v| dir.join(format!("{name}_v{v}.{ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

impl App {
    fn presets_file(&self) -> Option<PathBuf> {
        crate::presets::file_beside(self.settings.store_path())
    }

    fn ensure_presets(&mut self) {
        if self.hint.presets_loaded {
            return;
        }
        self.hint.presets_loaded = true;
        if let Some(f) = self.presets_file() {
            let (p, err) = crate::presets::load(&f);
            self.hint.presets = p;
            if err.is_some() {
                self.hint.note = err;
            }
        }
    }

    fn store_presets(&mut self) {
        match self.presets_file() {
            Some(f) => {
                if let Err(e) = crate::presets::save(&f, &self.hint.presets) {
                    self.hint_note(format!("Presets not saved: {e}"));
                }
            }
            None => self.hint_note("Presets need a preferences location; none is available."),
        }
    }

    /// Workflow presets (`export == false`: engine params + outputs, applied to this clip's draft) or export
    /// presets (container + product). Source-bound values are never copied (see `presets`).
    pub(super) fn preset_ui(&mut self, ui: &mut Ui, clip: &Clip, export: bool) {
        self.ensure_presets();
        let kind = if export { "export" } else { "workflow" };
        ui.label(
            RichText::new(if export {
                "Export presets"
            } else {
                "Workflow presets"
            })
            .strong(),
        );
        let names: Vec<String> = self
            .hint
            .presets
            .iter()
            .filter(|p| p.kind == kind)
            .map(|p| p.name.clone())
            .collect();
        if !names.contains(&self.hint.preset_sel) {
            self.hint.preset_sel = names.first().cloned().unwrap_or_default();
        }
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt(("preset", kind)).selected_text(if self.hint.preset_sel.is_empty() { "(none saved)".to_string() } else { self.hint.preset_sel.clone() }).show_ui(ui, |ui| {
                for n in &names {
                    ui.selectable_value(&mut self.hint.preset_sel, n.clone(), n);
                }
            });
            let have = !self.hint.preset_sel.is_empty();
            if ui.add_enabled(have, egui::Button::new("Apply")).clicked() {
                let data = self.hint.presets.iter().find(|p| p.kind == kind && p.name == self.hint.preset_sel).map(|p| p.data.clone());
                if let (Some(d), Some(pr)) = (data, self.project.as_ref()) {
                    if export {
                        if let Some(c) = d.get("container").and_then(Value::as_str).and_then(|c| CONTAINERS.iter().position(|x| x.0 == c)) {
                            self.hint.export_container = c;
                        }
                        if let Some(p) = d.get("product").and_then(Value::as_str) {
                            self.hint.export_product = p.to_string();
                        }
                    } else {
                        let dr = self.ai.draft(clip, pr, self.settings.saved());
                        match crate::presets::apply_workflow(&d, &mut dr.params, &mut dr.out) {
                            Ok(()) => self.hint.note = Some(format!("Applied “{}”: only the fields it carries changed; range, colour, prompts and output locations are untouched.", self.hint.preset_sel)),
                            Err(e) => self.hint.note = Some(format!("Preset not applied: {e}")),
                        }
                    }
                }
            }
            if ui.add_enabled(have, egui::Button::new("Delete")).clicked() {
                let sel = self.hint.preset_sel.clone();
                self.hint.presets.retain(|p| !(p.kind == kind && p.name == sel));
                self.store_presets();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.hint.preset_name).hint_text("name").desired_width(140.0));
            let n = self.hint.preset_name.trim().to_string();
            if ui.add_enabled(!n.is_empty(), egui::Button::new("Save current as preset")).clicked() {
                let data = if export {
                    Some(serde_json::json!({"container": CONTAINERS[self.hint.export_container].0, "product": self.hint.export_product}))
                } else {
                    self.project.as_ref().map(|pr| {
                        let d = self.ai.draft(clip, pr, self.settings.saved());
                        crate::presets::workflow_data(&d.params, &d.out)
                    })
                };
                if let Some(data) = data {
                    self.hint.presets.retain(|p| !(p.kind == kind && p.name == n));
                    self.hint.presets.push(crate::presets::Preset { name: n.clone(), kind: kind.into(), data });
                    self.hint.preset_sel = n;
                    self.store_presets();
                }
            }
        });
    }
}

impl App {
    pub(super) fn relink_picked(&mut self, ctx: &egui::Context, folder: PathBuf) {
        let Some(clip) = self.open_clip_snapshot() else {
            return;
        };
        match crate::persist::relink_sequence(&clip, &folder) {
            Ok(note) => {
                self.hint.note = Some(note);
                let f = clip.folder.clone();
                self.refresh_clip(ctx, &f);
            }
            Err(e) => self.hint.note = Some(format!("Not relinked: {e}")),
        }
    }
}
