//! Global chrome: File menu, Save with a Saved/Unsaved indicator, the project output folder, the Welcome
//! page and the Recent list. Everything here goes through the existing open/pick/close flows.
use super::*;
use serde_json::json;

/// Recently opened projects: user state in `recent_projects.json` beside the preferences (never in a
/// project). Only projects that actually loaded are recorded; unknown keys in the file are preserved.
#[derive(Default)]
pub struct Recent {
    file: Option<PathBuf>,
    list: Vec<PathBuf>,
}

const KEEP: usize = 8;

impl Recent {
    pub fn load(settings_path: Option<&std::path::Path>) -> Self {
        let file = settings_path
            .and_then(std::path::Path::parent)
            .map(|d| d.join("recent_projects.json"));
        let list = file
            .as_ref()
            .and_then(|f| std::fs::read(f).ok())
            .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
            .and_then(|v| {
                v.get("recent").and_then(Value::as_array).map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(PathBuf::from)
                        .collect()
                })
            })
            .unwrap_or_default();
        Self { file, list }
    }

    /// Move `root` to the front; the file is only rewritten when the order changed.
    fn record(&mut self, root: &std::path::Path) -> Result<(), String> {
        if self.list.first().map(PathBuf::as_path) == Some(root) {
            return Ok(());
        }
        let mut next: Vec<PathBuf> = self.list.iter().filter(|p| p.as_path() != root).cloned().collect();
        next.insert(0, root.to_path_buf());
        next.truncate(KEEP);
        if let Some(f) = &self.file {
            let v: Vec<String> = next.iter().map(|p| p.display().to_string()).collect();
            crate::project::update_json(f, |m| {
                m.insert("recent".into(), json!(v));
            })?;
        }
        self.list = next;
        Ok(())
    }

    pub fn list(&self) -> &[PathBuf] {
        &self.list
    }
}

impl App {
    /// Every unsaved edit the project could lose: In/Out, input colour (viewer), output location, or an
    /// earlier failed save.
    pub fn unsaved(&self) -> bool {
        self.save_error.is_some()
            || self.hint_project_dirty()
            || self.draft_dirty
            || self.persist_failed.is_some()
            || self
                .viewer
                .as_ref()
                .is_some_and(|v| v.range_dirty() || v.linear != v.linear_stored)
            || (self.project.is_some() && serde_json::to_value(&self.loc).ok() != self.loc_saved)
    }

    fn draft_value(&mut self, clip: &Clip) -> Option<Value> {
        let p = self.project.as_ref()?;
        Some(self.ai.draft(clip, p, self.settings.saved()).to_value())
    }

    /// First open of a clip after a load: the saved draft (`ck_native.draft`) replaces the seeded one, and
    /// the baseline for the Unsaved indicator is whatever disk holds.
    pub(super) fn adopt_draft(&mut self, clip: &Clip) {
        if !self.draft_clips.insert(clip.folder.clone()) {
            return;
        }
        if let (Some(v), Some(p)) = (clip.native.get("draft"), self.project.as_ref()) {
            let params = v.get("params").and_then(|x| serde_json::from_value(x.clone()).ok());
            let out = v.get("out").and_then(|x| serde_json::from_value(x.clone()).ok());
            let d = self.ai.draft(clip, p, self.settings.saved());
            d.explicit = crate::ui::ai::Draft::explicit_of(Some(v));
            d.from_preset = crate::ui::ai::Draft::from_preset_of(Some(v));
            if let Some(x) = params {
                d.params = x;
            }
            if let Some(x) = out {
                d.out = x;
            }
        }
        if let Some(v) = self.draft_value(clip) {
            self.draft_saved.insert(clip.folder.clone(), v);
        }
    }

    /// Per frame: does any adopted clip's draft (open or not) differ from what disk holds?
    fn refresh_draft_dirty(&mut self) {
        // Shots changed by a deliberate action (Batch reset, confirmed queue with overrides) are tracked like opened ones.
        for f in self.ai.take_touched() {
            self.draft_clips.insert(f);
        }
        let Some(p) = &self.project else {
            self.draft_dirty = false;
            return;
        };
        let (ai, st, saved, adopted) = (&mut self.ai, self.settings.saved(), &self.draft_saved, &self.draft_clips);
        self.draft_dirty = p.clips.iter().filter(|c| adopted.contains(&c.folder)).any(|c| {
            let d = ai.draft(c, p, st);
            saved.get(&c.folder) != Some(&d.to_value())
        });
    }

    /// Remember the on-disk output location as the clean state (project load, clip open, successful save).
    pub(super) fn mark_location_clean(&mut self) {
        self.loc_saved = serde_json::to_value(&self.loc).ok();
    }

    /// Project-level output location: the saved one, else the first clip's default.
    pub(super) fn init_location(&mut self) {
        let Some(p) = &self.project else { return };
        let l = crate::workflow::read_location(p)
            .ok()
            .flatten()
            .or_else(|| p.clips.first().and_then(crate::workflow::default_location));
        // A project with no saved location and no clip must not inherit the previous project's output folder.
        self.loc = l.unwrap_or_else(|| OutputLocation {
            root: PathBuf::new(),
            folder_template: "{name}_{version}/{output}".into(),
            file_template: "{name}_####".into(),
        });
        self.mark_location_clean();
    }

    /// Record a project that just loaded.
    pub(super) fn record_recent(&mut self) {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return;
        };
        if let Err(e) = self.recent.record(&root) {
            self.ai.notice = Some(format!("Recent projects not saved: {e}"));
        }
    }

    /// Write the output location to `project.json` (the same save the Deliver page uses).
    pub(super) fn save_output_location(&mut self) -> Result<(), String> {
        let root = self.project.as_ref().ok_or("no project")?.root.clone();
        let v = crate::workflow::save_location(&root, &self.loc)?;
        if let Some(p) = &mut self.project {
            p.output_location = Some(v);
        }
        self.mark_location_clean();
        Ok(())
    }

    /// Explicit Save: flush In/Out + colour, the open clip's hint settings, the output location and the
    /// workspace. Annotations and graph edits are written as they change. Never claims Saved after an error.
    pub fn save_project(&mut self) -> Result<(), String> {
        let Some(root) = self.project.as_ref().map(|p| p.root.clone()) else {
            return Err("No project is open".into());
        };
        let mut errs: Vec<String> = vec![];
        for f in self.ai.take_touched() {
            self.draft_clips.insert(f);
        }
        if let Err(e) = self.hint_flush_project() { errs.push(e); }
        self.persist_failed = None;
        self.persist_media();
        if self.persist_failed.is_some() {
            errs.push(self.ai.notice.clone().unwrap_or_else(|| "In/Out or colour not saved".into()));
        }
        if let Some(clip) = self.clip().cloned() {
            let cfg = self.hint_cfg_for(&clip);
            if let Err(e) = cfg.validate().and_then(|()| cfg.save(&clip)) {
                errs.push(format!("Hint settings not saved: {e}"));
            }
        }
        for f in self.draft_clips.clone() {
            let Some(clip) = self.project.as_ref().and_then(|p| p.clips.iter().find(|c| c.folder == f)).cloned() else { continue };
            let Some(v) = self.draft_value(&clip) else { continue };
            if self.draft_saved.get(&f) == Some(&v) {
                continue;
            }
            match crate::persist::update_native(&clip.meta_path, |n| {
                n.insert("draft".into(), v.clone());
            }) {
                Ok(()) => {
                    self.draft_saved.insert(f, v);
                }
                Err(e) => errs.push(format!("Key settings of {f} not saved: {e}")),
            }
        }
        let loc_dirty = serde_json::to_value(&self.loc).ok() != self.loc_saved;
        let has_saved = self.project.as_ref().is_some_and(|p| p.output_location.as_ref().is_some_and(|v| !v.is_null()));
        if loc_dirty || has_saved {
            if let Err(e) = self.save_output_location() {
                errs.push(format!("Output location not saved: {e}"));
            }
        }
        if let Some(w) = self.workspace_current() {
            if let Err(e) = w.save(&root) {
                errs.push(format!("Workspace not saved: {e}"));
            }
        }
        self.save_error = (!errs.is_empty()).then(|| errs.join("; "));
        match &self.save_error {
            None => Ok(()),
            Some(e) => Err(e.clone()),
        }
    }

    /// Open a project folder exactly like "Open another project…": refused while an import runs or a dialog is
    /// open, and a project with unsaved edits is saved first (the switch is refused if that fails).
    pub fn open_project_path(&mut self, ctx: &egui::Context, path: PathBuf) -> bool {
        if self.imp.busy() || self.dialog_active() {
            return false;
        }
        if self.project.is_some() && self.unsaved() {
            if let Err(e) = self.save_project() {
                self.ai.notice = Some(format!("Not switching projects: {e}"));
                return false;
            }
        }
        self.path_input = path.display().to_string();
        self.load(ctx);
        true
    }

    /// File > New project: pending edits are saved first (refused if that fails), then only a destination
    /// folder is asked for. Nothing is closed until that folder was confirmed and the empty project exists.
    pub fn new_project_pick(&mut self, ctx: &egui::Context) -> bool {
        if self.imp.busy() || self.dialog_active() {
            return false;
        }
        if self.project.is_some() && self.unsaved() {
            if let Err(e) = self.save_project() {
                self.ai.notice = Some(format!("Not starting a new project: {e}"));
                return false;
            }
        }
        let started = self.request_pick(ctx, PickKind::NewProject);
        // The answer is only applied on the Board: leave the clip now (edits were saved above).
        self.back_requested |= started && self.in_workspace();
        started
    }

    /// The chosen parent folder waits for a project name (the native picker may not offer "new folder").
    pub(super) fn ask_project_name(&mut self, parent: PathBuf) {
        self.new_project_draft = Some((parent, "New project".into()));
    }

    /// The name window: Create makes `parent/name` an empty project and replaces the open one. Any failure
    /// leaves the open project, its drafts and the disk untouched.
    pub(super) fn new_project_window(&mut self, ctx: &egui::Context) {
        let Some((parent, mut name)) = self.new_project_draft.take() else { return };
        let (mut create, mut cancel) = (false, false);
        egui::Window::new("New project").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(RichText::new(format!("An empty project folder is created in {}", parent.display())).color(DIM));
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
            create = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                let c = super::theme::primary_button(ui, "Create project");
                self.new_project_create_rect = Some(c.rect);
                create |= c.clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if let Some(e) = &self.new_project_error { ui.colored_label(super::theme::FOCUS, e); }
        });
        if cancel {
            self.new_project_error = None;
            return;
        }
        if create {
            if self.project.is_some() && self.unsaved() {
                if let Err(e) = self.save_project() {
                    self.new_project_error = Some(format!("Not starting a new project: {e}"));
                    self.new_project_draft = Some((parent, name));
                    return;
                }
            }
            if self.imp.busy() {
                self.new_project_error = Some(format!("Not starting a new project: {}", self.imp.busy_what()));
                self.new_project_draft = Some((parent, name));
                return;
            }
            match crate::project::create_empty_project(&parent, &name) {
                Ok(dir) => {
                    self.new_project_error = None;
                    self.open_project_path(ctx, dir);
                    return;
                }
                Err(e) => self.new_project_error = Some(format!("Not starting a new project: {e}")),
            }
        }
        self.new_project_draft = Some((parent, name));
    }

    fn quit_request(&mut self, ctx: &egui::Context) {
        // Flush pending edits before the existing running-job close flow.
        if self.project.is_some() && self.unsaved() {
            if let Err(e) = self.save_project() { self.ai.notice = Some(format!("Not closing: {e}")); return; }
        }
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Modifiers;
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) && self.project.is_some() {
            self.report_save();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.request_pick(ctx, PickKind::Project);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Q)) {
            self.quit_request(ctx);
        }
    }

    fn report_save(&mut self) {
        if let Err(e) = self.save_project() {
            self.ai.notice = Some(format!("Not saved: {e}"));
        }
    }

    /// The global bar: File, Open, Save, Quit, the Saved/Unsaved indicator, and the project output folder.
    pub(super) fn chrome(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.refresh_draft_dirty();
        let has = self.project.is_some();
        let busy = self.imp.busy() || self.dialog_active();
        let pages = self.settings_open || self.models_open || self.clients_open;
        let (mut open, mut save, mut quit) = (false, false, false);
        let mut new = false;
        let mut rects: [Option<egui::Rect>; 2] = [None; 2];
        ui.horizontal(|ui| {
            let file = ui.menu_button("File", |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    let item = ui.button("New project…");
                    rects[1] = Some(item.rect);
                    if item.on_disabled_hover_text("An import or file dialog is already open").clicked() {
                        new = true;
                        ui.close();
                    }
                });
                if ui.add(egui::Button::new("Open project…").shortcut_text("Ctrl+O")).clicked() {
                    open = true;
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Save project").shortcut_text("Ctrl+S")).clicked() {
                    save = true;
                    ui.close();
                }
                ui.separator();
                if ui.add(egui::Button::new("Quit").shortcut_text("Ctrl+Q")).clicked() {
                    quit = true;
                    ui.close();
                }
            });
            rects[0] = Some(file.response.rect);
            open |= ui.add_enabled(!pages, egui::Button::new("Open")).clicked();
            save |= ui.add_enabled(has, egui::Button::new("Save")).on_hover_text("Ctrl+S").clicked();
            quit |= ui.button("Quit").clicked();
            if has {
                let unsaved = self.unsaved();
                let (t, c) = if unsaved {
                    ("Unsaved", super::theme::FOCUS)
                } else {
                    ("Saved", theme::ACCENT)
                };
                let r = ui.label(RichText::new(t).color(c).strong());
                if let Some(e) = &self.save_error {
                    r.on_hover_text(e);
                }
            }
            ui.separator();
            ui.label(RichText::new("CorridorKey").strong());
            ui.add_space(20.0);
            self.clients_button(ui);
            self.settings_button(ui);
        });
        if open {
            self.request_pick(&ctx, PickKind::Project);
        }
        self.new_menu_rects = rects;
        if new {
            self.new_project_pick(&ctx);
        }
        if save {
            self.report_save();
        }
        if quit {
            self.quit_request(&ctx);
        }
        if has {
            self.output_folder_bar(ui);
        }
    }

    fn output_folder_bar(&mut self, ui: &mut Ui) {
        let mut apply = false;
        ui.horizontal(|ui| {
            ui.menu_button("Project output folder…", |ui| {
                ui.set_width(520.0);
                ui.label(RichText::new("Project output folder").strong());
                ui.add_space(8.0);
                let mut root = self.loc.root.display().to_string();
                let r = ui.add(egui::TextEdit::singleline(&mut root).desired_width(500.0));
                self.loc.root = root.into();
                apply = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                ui.label(RichText::new("New renders use this root. Existing outputs stay in place.").color(DIM).size(12.0));
                apply |= super::theme::primary_button(ui, "Save folder").clicked();
                if apply { ui.close(); }
            });
            let shown = if self.loc.root.as_os_str().is_empty() { "Each clip's default output folder".to_string() } else { self.loc.root.display().to_string() };
            ui.add(egui::Label::new(RichText::new(&shown).color(DIM).size(12.0)).truncate()).on_hover_text(format!("{shown}/{}", self.loc.folder_template));
        });
        if apply {
            self.loc_note = Some(match self.save_output_location() {
                Ok(()) => "Project output folder saved".into(),
                Err(e) => format!("Not saved: {e}"),
            });
        }
        if let Some(n) = self.loc_note.as_ref().filter(|n| n.starts_with("Not saved")) { ui.colored_label(super::theme::FOCUS, n); }
    }

    /// Welcome page: Open, New (an empty project), a path entry and the Recent list.
    pub(super) fn welcome(&mut self, ui: &mut Ui, busy: bool, why: &str, want: &mut Option<PickKind>) {
        let ctx = ui.ctx().clone();
        let mut open: Option<PathBuf> = None;
        ui.add_space(48.0);
        let width = ui.available_width().min(980.0);
        let pad = ((ui.available_width() - width) * 0.5).max(0.0);
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.allocate_ui_with_layout(Vec2::new(width, 0.0), egui::Layout::top_down(egui::Align::Min), |ui| {
                ui.set_width(width);
                ui.label(RichText::new("YOUR WORKSPACE").size(12.0).color(DIM));
                ui.add_space(12.0);
                ui.label(RichText::new("Continue a project.").size(28.0).strong());
                ui.add_space(8.0);
                ui.label(RichText::new("Open your clips, refine the matte and deliver your enabled outputs.").color(DIM));
                ui.add_space(24.0);
                ui.horizontal(|ui| {
                    let b = egui::Button::new(RichText::new("Open project…").color(Color32::from_rgb(21, 36, 26)).strong())
                        .fill(theme::ACCENT).min_size(Vec2::new(176.0, 42.0));
                    if ui.add_enabled(!busy, b).on_disabled_hover_text(why).clicked() { *want = Some(PickKind::Project); }
                    if ui.add_enabled(!busy, egui::Button::new("New project…")).on_disabled_hover_text(why).clicked() { *want = Some(PickKind::NewProject); }
                });
                ui.add_space(22.0);
                ui.columns(2, |cols| {
                    theme::section(&mut cols[0], "Recent projects", |ui| {
                        if self.recent.list().is_empty() {
                            ui.label(RichText::new("Projects you open will appear here.").color(DIM));
                            ui.add_space(28.0);
                        }
                        for path in self.recent.list().to_vec() {
                            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
                            let exists = path.is_dir();
                            ui.horizontal(|ui| {
                                if ui.add_enabled(!busy && exists, egui::Button::new(RichText::new(&name).strong()).min_size(Vec2::new(160.0, 34.0)))
                                    .on_hover_text(path.display().to_string()).on_disabled_hover_text(if exists { why } else { "Project folder is missing" }).clicked() { open = Some(path.clone()); }
                                ui.label(RichText::new(if exists { "Open →" } else { "Missing" }).size(11.0).color(DIM));
                            });
                            ui.add(egui::Label::new(RichText::new(path.display().to_string()).size(11.0).color(DIM)).truncate());
                            ui.separator();
                        }
                    });
                    theme::section(&mut cols[1], "From clips to delivery", |ui| {
                        ui.label("Import  >  Pre-matte / Hint  >  Key  >  Deliver");
                        ui.add_space(12.0);
                        ui.label(RichText::new("Workflow presets repeat the complete pipeline for your selected clips.").color(DIM));
                        ui.add_space(20.0);
                        ui.horizontal(|ui| { self.clients_button(ui); self.settings_button(ui); });
                    });
                });
                ui.add_space(12.0);
                egui::CollapsingHeader::new("Open a project by path").show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let r = ui.add(egui::TextEdit::singleline(&mut self.path_input).desired_width((width - 100.0).max(180.0)));
                        let go = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                        if !busy && ((go && !self.path_input.trim().is_empty()) || ui.add_enabled(!self.path_input.trim().is_empty(), egui::Button::new("Open")).clicked()) {
                            open = Some(PathBuf::from(self.path_input.trim()));
                        }
                    });
                });
                if let Some(e) = &self.load_error { ui.colored_label(WARN, e); }
            });
        });
        if let Some(path) = open { self.open_project_path(&ctx, path); }
    }
}
