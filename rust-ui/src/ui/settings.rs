//! Settings page: a draft form over `settings::Store`. Edits stay in the draft until Apply; Cancel
//! discards them; Reset only rewrites the draft. Narrow-width friendly: one control per row,
//! wrapped help text, scrollable body, buttons pinned at the bottom.
use super::theme;
use crate::settings::{self as cfg, Settings, Store};
use crate::worker::{RemoteOs, Target, TargetKind};
use eframe::egui::{self, Color32, RichText, Ui};
use std::path::PathBuf;

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

const PENDING: &str = "Saved as a default for the native app. Not used until native inference / mask tools are integrated.";
const AI_SEED: &str = "Seeds a clip's Key settings until that clip (or the Qt session) has its own values. \"Frame input is scene-linear\" is not used: each job freezes the clip's own interpretation in Media.";

pub struct Page {
    store: Store,
    draft: Settings,
    /// Last Apply failure; cleared on the next successful Apply, Cancel or reopening the page.
    error: Option<String>,
    models_rect: Option<egui::Rect>,
    apply_rect: Option<egui::Rect>,
    cancel_rect: Option<egui::Rect>,
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
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }
    /// Re-sync the form with saved settings (call when the page is opened).
    pub fn open(&mut self) {
        self.draft = self.store.saved().clone();
        self.error = None;
    }

    pub fn apply(&mut self) -> bool {
        match self.store.save(self.draft.clone()) {
            Ok(()) => {
                self.error = None;
                true
            }
            Err(e) => {
                self.error = Some(e);
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
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                let r = ui.add_enabled(self.can_apply(), egui::Button::new("Apply"));
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
            });
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| self.body(ui, &mut ev));
        });
        ev
    }

    fn body(&mut self, ui: &mut Ui, ev: &mut Event) {
        if let Some(p) = &self.store.problem {
            note(ui, p, theme::FOCUS);
        }
        if let Some(e) = &self.error {
            note(
                ui,
                &format!("Not saved: {e}"),
                Color32::from_rgb(240, 120, 110),
            );
        }
        if let Some(p) = self.store.path() {
            dim(ui, &format!("Stored in {}", p.display()));
        }
        let models = ui.button("Models & runtime…");
        self.models_rect = Some(models.rect);
        if models.clicked() {
            *ev = Event::OpenModels;
        }
        let d = &mut self.draft;

        ui.heading("Playback");
        dim(ui, "RAM budget and decode workers apply when the next clip opens; the open clip keeps its In/Out and playhead. A --cache-mb launch option overrides the saved budget.");
        ui.label("Decoded-frame RAM budget (MiB, 0 = Auto)");
        ui.horizontal_wrapped(|ui| {
            ui.add(
                egui::DragValue::new(&mut d.playback.cache_mib)
                    .range(0..=*cfg::CACHE_MIB.end())
                    .speed(16.0),
            );
            if d.playback.cache_mib == 0 {
                dim(
                    ui,
                    &format!(
                        "Auto = {} MiB (half the free RAM, 1–8 GiB)",
                        crate::frames::default_cache_mb()
                    ),
                );
            }
        });
        ui.label("Decode workers (0 = Auto)");
        ui.horizontal_wrapped(|ui| {
            ui.add(egui::DragValue::new(&mut d.playback.decode_workers).range(cfg::WORKERS));
            if d.playback.decode_workers == 0 {
                dim(
                    ui,
                    &format!(
                        "Auto = {} on this machine",
                        crate::frames::default_workers()
                    ),
                );
            }
        });
        theme::checkbox(
            ui,
            &mut d.playback.loop_default,
            "Loop playback within In/Out by default",
        );

        ui.separator();
        ui.heading("Interface");
        dim(ui, "UI scale applies as soon as you press Apply.");
        ui.add(
            egui::Slider::new(&mut d.interface.ui_scale, cfg::UI_SCALE)
                .step_by(0.05)
                .text("UI scale"),
        );

        ui.separator();
        ui.heading("AI defaults");
        dim(ui, AI_SEED);
        choice(
            ui,
            "VRAM profile",
            &mut d.ai.vram_profile,
            &cfg::VRAM_PROFILES,
        );
        dim(ui, "Auto lets the engine pick; Low selects its conservative low-VRAM path. Neither caps memory.");
        choice(ui, "Hint mode", &mut d.ai.hint_mode, &cfg::HINT_MODES);
        ui.horizontal(|ui| {
            ui.label("Temporal awareness");
            super::ai::temporal_choice(ui, "settings_temporal", &mut d.ai.temporal_window);
        });
        dim(ui, &cfg::temporal_explain(&d.ai.temporal_window, None));
        unit(ui, "Hint quality", &mut d.ai.hint_quality);
        theme::checkbox(ui, &mut d.ai.carry_hint, "Carry hint between windows");
        unit(ui, "Carry hint quality", &mut d.ai.carry_hint_quality);
        unit(ui, "Despill strength", &mut d.ai.despill_strength);
        theme::checkbox(ui, &mut d.ai.input_is_linear, "Frame input is scene-linear");
        theme::checkbox(ui, &mut d.ai.cutout_linear, "Linear cutout");

        ui.separator();
        targets_ui(ui, &mut d.ai_targets);

        ui.separator();
        ui.heading("Masks and hints");
        dim(ui, PENDING);
        ui.label("SAM2 tracker model");
        let cur = cfg::TRACKER_MODELS
            .iter()
            .find(|m| m.2 == d.masks.tracker_model)
            .map_or("Unknown", |m| m.0);
        egui::ComboBox::from_id_salt("tracker_model")
            .selected_text(cur)
            .show_ui(ui, |ui| {
                for (label, size, id) in cfg::TRACKER_MODELS {
                    ui.selectable_value(
                        &mut d.masks.tracker_model,
                        id.to_string(),
                        format!("{label} ({size})"),
                    );
                }
            });

        ui.separator();
        dim(ui, "Defaults seed new work. Settings already chosen on a project or clip are never replaced.");
    }
}

fn text(ui: &mut Ui, label: &str, v: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(v).desired_width(f32::INFINITY));
}

/// Optional text: empty = `None`.
fn opt_text(ui: &mut Ui, label: &str, v: &mut Option<String>) {
    let mut t = v.clone().unwrap_or_default();
    text(ui, label, &mut t);
    *v = (!t.trim().is_empty()).then_some(t.trim().to_owned());
}

/// AI worker machines. Empty list = one implicit local worker (GPU 0). All machines are assumed to see the
/// same shared storage; `path_map` translates this machine's paths to the worker's (POSIX path, drive letter
/// or UNC). Results are always read back from the local path.
fn targets_ui(ui: &mut Ui, targets: &mut Vec<Target>) {
    ui.heading("AI worker targets");
    dim(ui, "No targets = one local worker on GPU 0. One worker runs one job at a time; jobs are placed by Auto (local first) or on a chosen target. Applied after running jobs finish.");
    dim(ui, "Synthetic local-GPU, Linux SSH, and Windows local-disk SSH smoke checks passed. Windows NAS/service deployment, packaged UI, and full workflows remain unverified.");
    dim(ui, "Windows SSH workers may not see desktop mapped drives.");
    let mut remove = None;
    for (i, t) in targets.iter_mut().enumerate() {
        egui::CollapsingHeader::new(format!("{} ({})", t.id, if t.is_local() { "local" } else { "SSH" })).id_salt(("target", i)).show(ui, |ui| {
            text(ui, "Name", &mut t.id);
            theme::checkbox(ui, &mut t.enabled, "Enabled");
            ui.add(egui::DragValue::new(&mut t.gpu).range(0..=15).prefix("GPU index "));
            let local = t.is_local();
            let mut want_local = local;
            ui.horizontal(|ui| {
                ui.selectable_value(&mut want_local, true, "This machine");
                ui.selectable_value(&mut want_local, false, "Remote (SSH)");
            });
            if want_local != local {
                t.kind = if want_local {
                    TargetKind::Local { repo: PathBuf::new(), python: None }
                } else {
                    TargetKind::Ssh { os: RemoteOs::Linux, host: String::new(), user: None, port: None, python: String::new(), repo: String::new(), ssh: None }
                };
            }
            match &mut t.kind {
                TargetKind::Local { repo, python } => {
                    let mut r = repo.display().to_string();
                    text(ui, "Repo folder", &mut r);
                    *repo = r.into();
                    let mut p = python.as_ref().map(|p| p.display().to_string());
                    opt_text(ui, "Python (empty = <repo>/.venv)", &mut p);
                    *python = p.map(Into::into);
                }
                TargetKind::Ssh { os, host, user, port, python, repo, .. } => {
                    ui.horizontal(|ui| {
                        ui.label("Remote OS");
                        ui.selectable_value(os, RemoteOs::Linux, "Linux");
                        ui.selectable_value(os, RemoteOs::Windows, "Windows (local-disk smoke tested)");
                    });
                    text(ui, "Host (ssh name or address)", host);
                    opt_text(ui, "User (empty = ssh config)", user);
                    let mut pt = port.unwrap_or(0);
                    ui.add(egui::DragValue::new(&mut pt).range(0..=65535).prefix("Port (0 = default) "));
                    *port = (pt != 0).then_some(pt);
                    let hint = if *os == RemoteOs::Windows { r"absolute path, e.g. C:\CorridorKey" } else { "absolute path, e.g. /opt/CorridorKey" };
                    text(ui, &format!("Repo folder on the worker ({hint})"), repo);
                    text(ui, "Python on the worker (absolute path)", python);
                }
            }
            ui.label("Path map: this machine's NAS path -> the worker's path");
            dim(ui, if matches!(t.kind, TargetKind::Ssh { os: RemoteOs::Windows, .. }) { r"e.g. /mnt/NAS  ->  \\nas\share  (preferred; a drive letter only if the SSH session has it)" } else { "e.g. /mnt/NAS  ->  /mnt/NAS" });
            let mut rm = None;
            for (j, (l, r)) in t.path_map.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    ui.add(egui::TextEdit::singleline(l).desired_width(120.0));
                    ui.label("->");
                    ui.add(egui::TextEdit::singleline(r).desired_width(120.0));
                    if ui.small_button("Remove").clicked() {
                        rm = Some(j);
                    }
                });
            }
            if let Some(j) = rm {
                t.path_map.remove(j);
            }
            if ui.button("Add path mapping").clicked() {
                t.path_map.push((String::new(), String::new()));
            }
            if ui.button("Remove this target").clicked() {
                remove = Some(i);
            }
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
            storage: Default::default(),
            path_map: vec![],
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
    ui.label(label);
    egui::ComboBox::from_id_salt(label)
        .selected_text(v.as_str())
        .show_ui(ui, |ui| {
            for o in opts {
                ui.selectable_value(v, o.to_string(), *o);
            }
        });
}
fn unit(ui: &mut Ui, label: &str, v: &mut f32) {
    ui.add(egui::Slider::new(v, 0.0..=1.0).step_by(0.01).text(label));
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
