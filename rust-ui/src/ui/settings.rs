//! Settings page: a draft form over `settings::Store`. Edits stay in the draft until Apply; Cancel
//! discards them; Reset only rewrites the draft. Left category navigation with search, one selected
//! pane on the right in grouped cards, Apply/Cancel/Reset pinned at the bottom. Below ~480 px the
//! navigation collapses to a wrapped row above the pane.
use super::theme;
use crate::settings::{self as cfg, Settings, Store};
use crate::shared_paths::{self as sp, Access, SharedRoot};
use crate::worker::{Target, TargetKind};
use eframe::egui::{self, Color32, RichText, Ui};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, TryRecvError};

/// What the host must do after `Page::ui`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    None,
    OpenModels,
    /// `Page::saved()` changed and is on disk: apply UI scale now; RAM/workers take effect at the
    /// next clip open (do not rebuild the open viewer).
    Applied,
    /// Cancel pressed: leave the page, nothing changed.
    Closed,
}

const PENDING: &str = "Seeds the SAM tracker model of a clip whose Hint settings were never saved. A clip that already has hint settings always keeps them.";
const AI_SEED: &str = "Seeds a clip's Key settings until that clip (or the Qt session) has its own values. \"Frame input is scene-linear\" is not used: each job freezes the clip's own interpretation in Media.";
const LABEL_W: f32 = 190.0;
const WIDE: f32 = 480.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Cat {
    Playback,
    Interface,
    AiDefaults,
    PathMapping,
    Targets,
    Masks,
    Paths,
}

static CATS: [(Cat, &str, &str); 7] = [
        (Cat::Playback, "Playback", "ram cache decode workers loop"),
        (Cat::Interface, "Interface", "ui scale zoom"),
        (Cat::AiDefaults, "AI defaults", "vram hint temporal despill linear carry key"),
        (Cat::PathMapping, "Path mapping", "shared storage nas mount drive unc root location network client folder"),
        (Cat::Targets, "AI worker targets", "gpu python windows"),
        (Cat::Masks, "Masks and hints", "sam2 tracker model"),
        (Cat::Paths, "Paths and info", "preferences runtime repo python folder"),
];

pub struct Page {
    store: Store,
    draft: Settings,
    /// Last Apply failure; cleared on the next successful Apply, Cancel or reopening the page.
    error: Option<String>,
    models_rect: Option<egui::Rect>,
    apply_rect: Option<egui::Rect>,
    cancel_rect: Option<egui::Rect>,
    cat: Cat,
    search: String,
    /// Resolved lazily the first time the Paths pane is shown (discovery touches the disk).
    launch: Option<crate::runtime::Launch>,
    /// Path Mapping row whose Browse was pressed and not yet handed to the host's picker.
    browse_req: Option<usize>,
    /// Row waiting for a picker answer; row edits that shift indices are disabled meanwhile.
    browse_row: Option<usize>,
    /// Access test in flight (runs off the UI thread: a NAS can stall) and its latest answers.
    probe: Option<Receiver<(SharedRoot, Access)>>,
    probed: Vec<(SharedRoot, Access)>,
    /// ("browse" | "test", row, rect) from the last frame (click tests).
    map_rects: Vec<(&'static str, usize, egui::Rect)>,
}

impl Page {
    pub fn new(store: Store) -> Self {
        let draft = store.saved().clone();
        Self {
            store,
            draft,
            error: None,
            models_rect: None,
            apply_rect: None,
            cancel_rect: None,
            cat: Cat::Playback,
            search: String::new(),
            launch: None,
            browse_req: None,
            browse_row: None,
            probe: None,
            probed: vec![],
            map_rects: vec![],
        }
    }

    /// Apply / Cancel button rectangles from the last frame (layout and click tests).
    pub fn apply_rect(&self) -> Option<egui::Rect> {
        self.apply_rect
    }
    pub fn cancel_rect(&self) -> Option<egui::Rect> {
        self.cancel_rect
    }
    pub fn models_rect(&self) -> Option<egui::Rect> {
        self.models_rect
    }

    /// The preferences file (presets live beside it).
    pub fn store_path(&self) -> Option<&std::path::Path> {
        self.store.path()
    }
    pub fn saved(&self) -> &Settings {
        self.store.saved()
    }
    pub fn draft(&self) -> &Settings {
        &self.draft
    }
    pub fn draft_mut(&mut self) -> &mut Settings {
        &mut self.draft
    }
    pub fn dirty(&self) -> bool {
        self.draft != *self.store.saved()
    }
    /// Apply is offered when there are edits or an unusable file must be replaced by the defaults.
    pub fn can_apply(&self) -> bool {
        self.dirty() || self.store.needs_repair()
    }
    pub fn set_error(&mut self, e: String) {
        self.error = Some(e);
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// Re-sync the form with saved settings (call when the page is opened).
    pub fn open(&mut self) {
        self.draft = self.store.saved().clone();
        self.error = None;
        self.browse_row = None;
    }

    /// What the Path mapping pane reports about the draft rows (tests read the same text).
    pub fn mapping_problem(&self) -> Option<String> {
        self.draft.mapping_problem()
    }
    /// Show the Path mapping pane (tests).
    pub fn select_path_mapping(&mut self) {
        self.cat = Cat::PathMapping;
        self.search.clear();
    }
    pub fn map_rect(&self, what: &str, row: usize) -> Option<egui::Rect> {
        self.map_rects.iter().find(|r| r.0 == what && r.1 == row).map(|r| r.2)
    }
    /// Latest Test answer for a row of the draft (tests).
    pub fn access_of(&self, row: usize) -> Option<&Access> {
        let r = self.draft.shared_roots.get(row)?;
        self.probed.iter().find(|(p, _)| p == r).map(|(_, a)| a)
    }
    /// Browse pressed: the host opens a folder picker (starting at `Some(dir)`) and answers `browse_done`.
    pub fn take_browse(&mut self) -> Option<(usize, Option<PathBuf>)> {
        let i = self.browse_req.take()?;
        self.browse_row = Some(i);
        let dir = self.draft.shared_roots.get(i).map(|r| PathBuf::from(&r.local));
        Some((i, dir))
    }
    /// The picker answered (`None` = cancelled or could not open). Ignored if the page was reopened meanwhile.
    pub fn browse_done(&mut self, path: Option<PathBuf>) {
        let Some(i) = self.browse_row.take() else { return };
        let Some(p) = path else { return };
        match (p.to_str(), self.draft.shared_roots.get_mut(i)) {
            (Some(s), Some(r)) => r.local = s.to_owned(),
            (None, _) => self.error = Some(format!("{} is not valid UTF-8 and cannot be used", p.display())),
            _ => {}
        }
    }

    pub fn apply(&mut self) -> bool {
        trim_targets(&mut self.draft.ai_targets);
        match self.store.save(self.draft.clone()) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(format!("Not saved: {e}"));
                false
            }
        }
    }
    pub fn cancel(&mut self) {
        self.open();
    }
    /// Factory defaults into the form only; unknown keys (top-level and nested) survive.
    pub fn reset(&mut self) {
        self.draft.reset_keeping_unknown();
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Event {
        let mut ev = Event::None;
        egui::Panel::bottom("settings_buttons").show(ui, |ui| {
            ui.add_space(6.0);
            ui.horizontal(|ui| ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let apply = egui::Button::new(RichText::new("Apply settings").strong().color(Color32::from_rgb(21, 36, 26))).fill(theme::ACCENT);
                let r = ui.add_enabled(self.can_apply(), apply);
                self.apply_rect = Some(r.rect);
                if r.clicked() && self.apply() {
                    ev = Event::Applied;
                }
                let c = ui.button("Cancel");
                self.cancel_rect = Some(c.rect);
                if c.clicked() {
                    self.cancel();
                    ev = Event::Closed;
                }
                if ui.button("Reset defaults").clicked() {
                    self.reset();
                }
                if self.dirty() {
                    dim(ui, "Unapplied changes");
                }
            }));
            ui.add_space(6.0);
        });
        let wide = ui.available_width() >= WIDE;
        if wide {
            egui::Panel::left("settings_categories")
                .resizable(false)
                .exact_size(210.0)
                .frame(egui::Frame::new().fill(theme::CANVAS).inner_margin(12))
                .show(ui, |ui| {
                    self.nav(ui, &mut ev, true);
                });
        }
        egui::CentralPanel::default().frame(egui::Frame::new().fill(theme::PANEL).inner_margin(24)).show(ui, |ui| {
            if !wide {
                self.nav(ui, &mut ev, false);
                ui.separator();
            }
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| self.body(ui));
        });
        ev
    }

    fn matches(&self, keys: &str, name: &str) -> bool {
        let q = self.search.trim().to_lowercase();
        q.is_empty() || name.to_lowercase().contains(&q) || keys.contains(&q)
    }

    fn nav(&mut self, ui: &mut Ui, models: &mut Event, vertical: bool) {
        let hits: Vec<_> = CATS.iter().filter(|(_, n, k)| self.matches(k, n)).collect();
        ui.add(
            egui::TextEdit::singleline(&mut self.search)
                .hint_text("Search settings")
                .desired_width(if vertical { f32::INFINITY } else { 220.0 }),
        );
        if !hits.is_empty() && !hits.iter().any(|(c, ..)| *c == self.cat) {
            self.cat = hits[0].0;
        }
        let items = |ui: &mut Ui| {
            for (c, name, _) in &hits {
                if ui.add(egui::Button::selectable(self.cat == *c, "").left_text(*name).min_size(egui::vec2(ui.available_width(), 34.0))).clicked() {
                    self.cat = *c;
                }
            }
            if hits.is_empty() {
                dim(ui, "No matching settings");
            }
            let m = ui.button("Models & runtime…");
            self.models_rect = Some(m.rect);
            if m.clicked() {
                *models = Event::OpenModels;
            }
        };
        if vertical {
            ui.add_space(6.0);
            ui.vertical(items);
        } else {
            ui.horizontal_wrapped(items);
        }
    }

    fn body(&mut self, ui: &mut Ui) {
        if let Some(p) = &self.store.problem {
            note(ui, p, theme::FOCUS);
        }
        if let Some(e) = &self.error {
            note(ui, e, Color32::from_rgb(240, 120, 110));
        }
        let cat = self.cat;
        let name = CATS.iter().find(|c| c.0 == cat).map_or("", |c| c.1);
        ui.heading(name);
        ui.add_space(12.0);
        match cat {
            Cat::Playback => self.playback(ui),
            Cat::Interface => self.interface(ui),
            Cat::AiDefaults => self.ai_defaults(ui),
            Cat::PathMapping => self.path_mapping(ui),
            Cat::Targets => targets_ui(ui, &mut self.draft.ai_targets),
            Cat::Masks => self.masks(ui),
            Cat::Paths => self.paths(ui),
        }
        ui.add_space(4.0);
        dim(ui, "Defaults seed new work. Settings already chosen on a project or clip are never replaced.");
    }

    fn playback(&mut self, ui: &mut Ui) {
        let d = &mut self.draft;
        dim(ui, "RAM budget and decode workers apply when the next clip opens; the open clip keeps its In/Out and playhead. A --cache-mb launch option overrides the saved budget.");
        card(ui, |ui| {
            row(ui, "Decoded-frame RAM (MiB)", |ui| {
                ui.add(
                    egui::DragValue::new(&mut d.playback.cache_mib)
                        .range(0..=*cfg::CACHE_MIB.end())
                        .speed(16.0),
                );
                if d.playback.cache_mib == 0 {
                    dim(ui, &format!("Auto = {} MiB (half the free RAM, 1–8 GiB)", crate::frames::default_cache_mb()));
                }
            });
            row(ui, "Decode workers", |ui| {
                ui.add(egui::DragValue::new(&mut d.playback.decode_workers).range(cfg::WORKERS));
                if d.playback.decode_workers == 0 {
                    dim(ui, &format!("Auto = {} on this machine", crate::frames::default_workers()));
                }
            });
            row(ui, "Looping", |ui| {
                theme::checkbox(ui, &mut d.playback.loop_default, "Loop within In/Out by default");
            });
        });
    }

    fn interface(&mut self, ui: &mut Ui) {
        let d = &mut self.draft;
        dim(ui, "UI scale applies as soon as you press Apply.");
        card(ui, |ui| {
            row(ui, "UI scale", |ui| {
                ui.add(egui::Slider::new(&mut d.interface.ui_scale, cfg::UI_SCALE).step_by(0.05));
            });
            row(ui, "Background", |ui| {
                theme::checkbox(ui, &mut d.interface.background, "Keep running when the window is closed");
            });
            dim(ui, "Closing the window hides it so the network client stays available. File > Quit still quits. Starting Keyloom again shows the running window.");
            if !d.interface.background {
                d.interface.launch_at_login = false;
            }
            row(ui, "Sign-in", |ui| {
                ui.add_enabled_ui(d.interface.background, |ui| {
                    theme::checkbox(ui, &mut d.interface.launch_at_login, "Start in the background when I sign in to this computer");
                });
            });
            dim(ui, "Starts only after you sign in to this user account (not at boot), so your mapped drives are available. Turning it off removes only the entry Keyloom created.");
        });
    }

    fn ai_defaults(&mut self, ui: &mut Ui) {
        let d = &mut self.draft;
        dim(ui, AI_SEED);
        card(ui, |ui| {
            row(ui, "VRAM profile", |ui| choice(ui, "VRAM profile", &mut d.ai.vram_profile, &cfg::VRAM_PROFILES));
            dim(ui, "Auto lets the engine pick; Low selects its conservative low-VRAM path. Neither caps memory.");
            row(ui, "Hint mode", |ui| choice(ui, "Hint mode", &mut d.ai.hint_mode, &cfg::HINT_MODES));
            row(ui, "Temporal awareness", |ui| {
                super::ai::temporal_choice(ui, "settings_temporal", &mut d.ai.temporal_window)
            });
            dim(ui, &cfg::temporal_explain(&d.ai.temporal_window, None));
        });
        card(ui, |ui| {
            row(ui, "Hint quality", |ui| unit(ui, &mut d.ai.hint_quality));
            row(ui, "Carry hint", |ui| theme::checkbox(ui, &mut d.ai.carry_hint, "Carry hint between windows"));
            row(ui, "Carry hint quality", |ui| unit(ui, &mut d.ai.carry_hint_quality));
            row(ui, "Despill strength", |ui| unit(ui, &mut d.ai.despill_strength));
        });
        card(ui, |ui| {
            row(ui, "Frame input", |ui| theme::checkbox(ui, &mut d.ai.input_is_linear, "Frame input is scene-linear"));
            row(ui, "Cutout", |ui| theme::checkbox(ui, &mut d.ai.cutout_linear, "Linear cutout"));
        });
    }

    fn masks(&mut self, ui: &mut Ui) {
        let d = &mut self.draft;
        dim(ui, PENDING);
        card(ui, |ui| {
            row(ui, "SAM2 tracker model", |ui| {
                let cur = cfg::TRACKER_MODELS
                    .iter()
                    .find(|m| m.2 == d.masks.tracker_model)
                    .map_or("Unknown", |m| m.0);
                egui::ComboBox::from_id_salt("tracker_model").selected_text(cur).show_ui(ui, |ui| {
                    for (label, size, id) in cfg::TRACKER_MODELS {
                        ui.selectable_value(&mut d.masks.tracker_model, id.to_string(), format!("{label} ({size})"));
                    }
                });
            });
        });
    }

    fn path_mapping(&mut self, ui: &mut Ui) {
        match self.probe.as_ref().map(Receiver::try_recv) {
            Some(Ok(a)) => {
                self.probed.retain(|(r, _)| *r != a.0);
                self.probed.push(a);
                self.probe = None;
            }
            Some(Err(TryRecvError::Disconnected)) => self.probe = None,
            _ => {}
        }
        dim(ui, "Each PC names the shared storage it sees. Jobs refer to a shared location by name and a relative path; every PC resolves that under its own folder, drive or UNC share. Applies to network jobs.");
        self.map_rects.clear();
        let (mut remove, mut test) = (None, None);
        let busy = self.browse_row.is_some();
        let testing = self.probe.is_some();
        for (i, r) in self.draft.shared_roots.iter_mut().enumerate() {
            card(ui, |ui| {
                row(ui, "Shared location name", |ui| {
                    let n = ui.add(egui::TextEdit::singleline(&mut r.name).id(ui.make_persistent_id(("shared_root_name", i))).desired_width(ui.available_width().clamp(90.0, 200.0)).hint_text("e.g. NAS"));
                    self.map_rects.push(("name", i, n.rect));
                });
                row(ui, "Folder on this PC", |ui| {
                    // Leave room for Browse so the pair never overflows a narrow card.
                    let w = (ui.available_width() - 100.0).clamp(90.0, 260.0);
                    let l = ui.add(egui::TextEdit::singleline(&mut r.local).id(ui.make_persistent_id(("shared_root_local", i))).desired_width(w).hint_text(if cfg!(windows) { r"Z:\Shared or \\nas\share" } else { "/mnt/nas" }));
                    self.map_rects.push(("local", i, l.rect));
                    let b = ui.add_enabled(!busy, egui::Button::new("Browse…"));
                    self.map_rects.push(("browse", i, b.rect));
                    if b.clicked() {
                        self.browse_req = Some(i);
                    }
                });
                row(ui, "Access", |ui| {
                    let t = ui.add_enabled(!testing, egui::Button::new("Test read and write"));
                    self.map_rects.push(("test", i, t.rect));
                    if t.clicked() {
                        test = Some(i);
                    }
                    if ui.add_enabled(!busy, egui::Button::new("Remove")).clicked() {
                        remove = Some(i);
                    }
                });
                if let Some((_, a)) = self.probed.iter().find(|(p, _)| p == r) {
                    if let Ok(t) = sample_preview(r) {
                        dim(ui, &t);
                    }
                    for (what, res) in [("Read", &a.read), ("Write", &a.write)] {
                        match res {
                            Ok(()) => dim(ui, &format!("{what}: OK")),
                            Err(e) => note(ui, &format!("{what}: {e}"), Color32::from_rgb(240, 120, 110)),
                        }
                    }
                }
            });
        }
        // Checked only on Test (here) and Apply (`store.save` validates), never while typing.
        if let Some(e) = test.and_then(|i| sp::check_root(&self.draft.shared_roots[i]).err()) {
            self.error = Some(format!("Not usable on this PC: {e}"));
            test = None;
        }
        if let Some(i) = test {
            self.error = None;
            let (tx, rx) = channel();
            let (r, ctx) = (self.draft.shared_roots[i].clone(), ui.ctx().clone());
            std::thread::spawn(move || {
                let a = sp::test_access(std::path::Path::new(&r.local));
                let _ = tx.send((r, a));
                ctx.request_repaint();
            });
            self.probe = Some(rx);
        }
        if let Some(i) = remove {
            self.draft.shared_roots.remove(i);
        }
        let add = ui.add_enabled(!busy, egui::Button::new("Add shared location"));
        self.map_rects.push(("add", 0, add.rect));
        if add.clicked() {
            self.draft.shared_roots.push(SharedRoot::default());
        }
    }

    fn paths(&mut self, ui: &mut Ui) {
        let launch = self.launch.get_or_insert_with(|| crate::runtime::launch(None));
        dim(ui, "Read-only. Project output folders belong to the project; model roots are managed under Models & runtime.");
        card(ui, |ui| {
            let none = "not set";
            let show = |ui: &mut Ui, label: &str, v: String| row(ui, label, |ui| {
                ui.add(egui::Label::new(v).wrap());
            });
            show(ui, "Preferences file", self.store.path().map_or(none.into(), |p| p.display().to_string()));
            show(ui, "Runtime repo", launch.repo.as_ref().map_or(none.into(), |p| p.display().to_string()));
            show(ui, "Python", launch.python.as_ref().map_or("developer default (CK_PYTHON, then <repo>/.venv)".into(), |p| p.display().to_string()));
            if let Some(p) = &launch.problem {
                note(ui, p, theme::FOCUS);
            }
        });
    }
}

/// A real reference through the real resolver: what a job's sample file becomes on this PC.
fn sample_preview(r: &SharedRoot) -> Result<String, String> {
    let sample = sp::SharedRef::new(&r.name, &["Project", "shot 01", "clip.mov"])?;
    let local = sp::resolve(std::slice::from_ref(r), &sample)?;
    Ok(format!("{}  ->  {}", sample.encode(), local.display()))
}

/// Raised grouped card.
fn card(ui: &mut Ui, body: impl FnOnce(&mut Ui)) {
    egui::Frame::new()
        .fill(theme::CARD)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(77, 94, 82)))
        .corner_radius(6.0)
        .inner_margin(12.0)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui);
        });
    ui.add_space(8.0);
}

/// Label column + control; wraps under the label when narrow.
fn row<R>(ui: &mut Ui, label: &str, control: impl FnOnce(&mut Ui) -> R) {
    if ui.available_width() > LABEL_W + 140.0 {
        ui.horizontal(|ui| {
            ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 26.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.set_width(LABEL_W);
                ui.add(egui::Label::new(label).wrap());
            });
            control(ui);
        });
    } else {
        ui.label(label);
        ui.horizontal_wrapped(control);
    }
}

fn text(ui: &mut Ui, label: &str, v: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
}

/// Optional text: empty (or only spaces) = `None`. The text is kept as typed; `apply` trims the saved value.
fn opt_text(ui: &mut Ui, label: &str, v: &mut Option<String>) {
    let mut t = v.clone().unwrap_or_default();
    text(ui, label, &mut t);
    *v = opt_value(t);
}

fn opt_value(t: String) -> Option<String> {
    (!t.trim().is_empty()).then_some(t)
}

/// Trim the optional fields that `opt_text` stores untrimmed, so the saved file has no stray spaces.
fn trim_targets(targets: &mut [Target]) {
    for t in targets {
        match &mut t.kind {
            TargetKind::Local { python, .. } => {
                *python = python.take().and_then(|p| opt_value(p.display().to_string()).map(|s| s.trim().to_owned().into()));
            }
        }
    }
}

#[cfg(test)]
mod opt_text_tests {
    use super::opt_value;

    #[test]
    fn keeps_spaces_and_blank_means_none() {
        assert_eq!(opt_value(String::new()), None);
        assert_eq!(opt_value("   ".into()), None);
        assert_eq!(opt_value(r"C:\Program Files".into()), Some(r"C:\Program Files".into()));
        assert_eq!(opt_value(" a ".into()), Some(" a ".into()));
    }
}

/// AI worker machines. Empty list = one implicit local worker (GPU 0).
fn targets_ui(ui: &mut Ui, targets: &mut Vec<Target>) {
    dim(ui, "No targets = one local worker on GPU 0. One worker runs one job at a time; jobs are placed by Auto (local first) or on a chosen target. Applied after running jobs finish.");
    dim(ui, "Legacy: this installation's own shared-storage names live under Path mapping.");
    ui.add_space(4.0);
    let mut remove = None;
    for (i, t) in targets.iter_mut().enumerate() {
        card(ui, |ui| {
        egui::CollapsingHeader::new(format!("{} (local)", t.id)).id_salt(("target", i)).show(ui, |ui| {
            text(ui, "Name", &mut t.id);
            theme::checkbox(ui, &mut t.enabled, "Enabled");
            ui.add(egui::DragValue::new(&mut t.gpu).range(0..=15).prefix("GPU index "));
            match &mut t.kind {
                TargetKind::Local { repo, python } => {
                    let mut r = repo.display().to_string();
                    text(ui, "Repo folder", &mut r);
                    *repo = r.into();
                    let mut p = python.as_ref().map(|p| p.display().to_string());
                    opt_text(ui, "Python (empty = <repo>/.venv)", &mut p);
                    *python = p.map(Into::into);
                }
            }
            if ui.button("Remove this target").clicked() {
                remove = Some(i);
            }
        });
        });
    }
    if let Some(i) = remove {
        targets.remove(i);
    }
    if ui.button("Add target").clicked() {
        let n = targets.len();
        targets.push(Target {
            id: format!("target-{}", n + 1),
            kind: TargetKind::Local {
                repo: PathBuf::new(),
                python: None,
            },
            gpu: 0,
            enabled: true,
        });
    }
}

fn dim(ui: &mut Ui, t: &str) {
    ui.label(RichText::new(t).color(theme::TEXT_DIM).size(12.0));
}
fn note(ui: &mut Ui, t: &str, c: Color32) {
    ui.label(RichText::new(t).color(c));
}
fn choice(ui: &mut Ui, label: &str, v: &mut String, opts: &[&str]) {
    egui::ComboBox::from_id_salt(label)
        .selected_text(v.as_str())
        .show_ui(ui, |ui| {
            for o in opts {
                ui.selectable_value(v, o.to_string(), *o);
            }
        });
}
fn unit(ui: &mut Ui, v: &mut f32) {
    ui.add(egui::Slider::new(v, 0.0..=1.0).step_by(0.01));
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Pos2, RawInput, Rect, Vec2};

    fn page(name: &str, body: Option<&str>) -> (Page, std::path::PathBuf) {
        let d = std::env::temp_dir().join(format!("ck-settings-ui-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("s.json");
        if let Some(b) = body {
            std::fs::write(&p, b).unwrap();
        }
        (Page::new(Store::load(Some(p.clone()))), p)
    }

    fn render(page: &mut Page, w: f32, scale: f32) -> Event {
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        ctx.set_zoom_factor(scale);
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, 500.0))),
            ..Default::default()
        };
        let mut ev = Event::None;
        for _ in 0..2 {
            ctx.run_ui(raw.clone(), |ui| ev = page.ui(ui))
                .textures_delta
                .clear();
        }
        ev
    }

    #[test]
    fn renders_at_narrow_widths_and_scales() {
        let (mut p, _) = page("render", Some("{bad"));
        for (w, s) in [(280.0, 0.75), (320.0, 1.0), (480.0, 2.0)] {
            assert_eq!(render(&mut p, w, s), Event::None);
        }
        assert!(p.store.problem.is_some());
    }

    #[test]
    fn cancel_and_reset_only_touch_the_draft_until_apply() {
        let (mut p, path) = page("flow", None);
        p.draft_mut().playback.cache_mib = 2048;
        p.draft_mut().extra.insert("k".into(), 1.into());
        assert!(p.dirty());
        p.cancel();
        assert!(!p.dirty() && p.saved().playback.cache_mib == 0);
        assert!(!path.exists());

        p.draft_mut().playback.cache_mib = 2048;
        p.reset();
        assert_eq!(p.draft().playback.cache_mib, 0);
        assert!(!path.exists() && p.saved().playback.cache_mib == 0);

        p.draft_mut().playback.decode_workers = 2;
        p.draft_mut().interface.ui_scale = 1.5;
        assert!(p.apply());
        assert!(!p.dirty());
        let again = Store::load(Some(path));
        assert_eq!(
            (again.saved().workers(), again.saved().interface.ui_scale),
            (2, 1.5)
        );
    }

    #[test]
    fn invalid_apply_shows_error_and_keeps_saved() {
        let (mut p, _) = page("bad", None);
        p.draft_mut().ai.hint_quality = 3.0;
        assert!(!p.apply());
        assert!(p.error().unwrap().contains("hint_quality"));
        assert_eq!(p.saved().ai.hint_quality, 0.95);
        render(&mut p, 320.0, 1.0); // error banner renders
    }

    #[test]
    fn unusable_file_can_be_repaired_with_unchanged_defaults() {
        let (mut p, path) = page("repair", Some("{bad"));
        assert!(!p.dirty() && p.can_apply());
        render(&mut p, 320.0, 1.0);
        let c = p.apply_rect().unwrap().center();
        // Click the Apply button at its exact laid-out position, in one isolated egui context.
        let ctx = egui::Context::default();
        theme::apply(&ctx);
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(320.0, 500.0))),
            ..Default::default()
        };
        let btn = |pressed| egui::Event::PointerButton {
            pos: c,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        };
        let mut ev = Event::None;
        let mut frame = |events: Vec<egui::Event>| {
            let mut r = raw.clone();
            r.events = events;
            ctx.run_ui(r, |ui| {
                let e = p.ui(ui);
                if e != Event::None {
                    ev = e;
                }
            })
            .textures_delta
            .clear();
        };
        frame(vec![]);
        frame(vec![egui::Event::PointerMoved(c)]);
        frame(vec![btn(true)]);
        frame(vec![btn(false)]);
        assert_eq!(ev, Event::Applied);
        assert!(path.with_extension("json.unusable0").exists());
        assert!(!p.can_apply());
        assert_eq!(Store::load(Some(path)).saved(), &Settings::default());
    }

    #[test]
    fn preview_is_a_real_reference_resolved_natively() {
        let r = SharedRoot { name: "NAS".into(), local: if cfg!(windows) { r"Z:\Shared".into() } else { "/mnt/nas".into() }, ..Default::default() };
        let t = sample_preview(&r).unwrap();
        let native = std::path::Path::new(&r.local).join("Project").join("shot 01").join("clip.mov");
        assert_eq!(t, format!("shared:NAS/Project/shot 01/clip.mov  ->  {}", native.display()));
    }

    #[test]
    fn reset_keeps_nested_unknown_keys() {
        let (mut p, _) = page(
            "nested",
            Some(r#"{"version":1,"ai":{"x":1},"playback":{"cache_mib":2048}}"#),
        );
        p.draft_mut().ai.vram_profile = "low".into();
        p.reset();
        assert_eq!(p.draft().ai.vram_profile, "auto");
        assert_eq!(p.draft().ai.extra["x"], 1);
        assert_eq!(p.draft().playback.cache_mib, 0);
    }
}
