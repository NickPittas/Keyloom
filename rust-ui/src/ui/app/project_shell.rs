//! Global chrome (bible §5.9 menu bar, §7.1 toolbar, §8.8 status bar on Start), Save with its state, the Start screen
//! (§7.4) and the Recent list. Everything here goes through the existing open/pick/close flows.
use super::*;
use crate::ui::controls::{
    self, buttons, dialog,
    menus::{self, Menu, MenuBar, MenuBarRight, MenuItem},
    scroll, statusbar, StageStatus, TabKind, TabStrip,
};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{pos2, vec2, Align, FontFamily, FontId, Rect};
use serde_json::json;

/// Dev page fixtures (screenshots only): the optional top-level `"shell"` object of the fixture file
/// (`--board-`, `--models-`, `--settings-`, `--machines-` or `--shot-fixture`), read once per run:
/// `"shell": { "project": "Night_exterior_greenscreen", "saved": true, "models_missing": 0 }`.
/// It draws the menu bar's project name and Saved state, and the Models page tab's cached attention state.
/// A real open project and the Board fixture's own project win over it.
/// Also the project-open status bar (mockups: "0 waiting", notices 0, CPU 7 %, RAM 34.7 / 123.5 GB, GPU0 21 %,
/// VRAM 3.6 / 24.0 GB). Absent fields keep those defaults; the GPU bar fills from `gpu_pct`.
#[derive(serde::Deserialize, Clone)]
#[serde(default)]
pub struct ShellFixture {
    pub project: String,
    pub saved: bool,
    pub models_missing: usize,
    pub waiting: u32,
    pub notices: u32,
    pub cpu: f32,
    pub ram_used: f32,
    pub ram_total: f32,
    pub gpu_pct: f32,
    pub gpu_label: String,
    pub vram_used: f32,
    pub vram_total: f32,
}

impl Default for ShellFixture {
    fn default() -> Self {
        Self {
            project: String::new(),
            saved: false,
            models_missing: 0,
            waiting: 0,
            notices: 0,
            cpu: 7.0,
            ram_used: 34.7,
            ram_total: 123.5,
            gpu_pct: 21.0,
            gpu_label: "RTX 4090".to_owned(),
            vram_used: 3.6,
            vram_total: 24.0,
        }
    }
}

static SHELL_FIXTURE: std::sync::OnceLock<Option<ShellFixture>> = std::sync::OnceLock::new();

fn shell_fixture(opts: &AppOptions) -> Option<&'static ShellFixture> {
    SHELL_FIXTURE
        .get_or_init(|| {
            let path = [&opts.board_fixture, &opts.models_fixture, &opts.settings_fixture, &opts.machines_fixture, &opts.shot_fixture]
                .into_iter()
                .find_map(|p| p.as_ref())?;
            let text = std::fs::read(path).map_err(|e| eprintln!("{}: {e}", path.display())).ok()?;
            let file: serde_json::Value = serde_json::from_slice(&text).map_err(|e| eprintln!("{}: {e}", path.display())).ok()?;
            let shell = file.get("shell")?.clone();
            serde_json::from_value(shell).map_err(|e| eprintln!("{}: shell: {e}", path.display())).ok()
        })
        .as_ref()
}

/// One Recent entry. `opened` = last-opened time (unix seconds; `None` in files written before it was stored).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub path: PathBuf,
    pub opened: Option<u64>,
    /// Folder missing, checked once when the list is loaded or changed (never per frame).
    pub missing: bool,
}

/// Recently opened projects: user state in `recent_projects.json` beside the preferences (never in a
/// project). Only projects that actually loaded are recorded; unknown keys in the file are preserved.
/// `recent` stays a list of path strings (older builds read it); the times are a separate `opened` map.
#[derive(Default)]
pub struct Recent {
    file: Option<PathBuf>,
    list: Vec<Entry>,
    /// Dev `--recent-fixture`: notice count shown in the status bar.
    fixture_notices: Option<u32>,
}

/// Lists over 8 rows scroll (§7.4), so more than 8 are kept.
const KEEP: usize = 20;

fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl Recent {
    pub fn load(settings_path: Option<&std::path::Path>) -> Self {
        let file = settings_path
            .and_then(std::path::Path::parent)
            .map(|d| d.join("recent_projects.json"));
        let v = file.as_ref().and_then(|f| std::fs::read(f).ok()).and_then(|b| serde_json::from_slice::<Value>(&b).ok());
        let opened = v.as_ref().and_then(|v| v.get("opened")).and_then(Value::as_object);
        let list = v
            .as_ref()
            .and_then(|v| v.get("recent"))
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(|s| {
                        let path = PathBuf::from(s);
                        let missing = !path.is_dir();
                        Entry { opened: opened.and_then(|o| o.get(s)).and_then(Value::as_u64), path, missing }
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self { file, list, fixture_notices: None }
    }

    /// Dev `--recent-fixture FILE.json`: `[{name, path, opened (seconds ago), missing}]`, or
    /// `{"notices": n, "recent": [...]}`. Never written back.
    pub fn fixture(path: &std::path::Path) -> Self {
        let v: Value = std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        let rows = v.as_array().or_else(|| v.get("recent").and_then(Value::as_array)).cloned().unwrap_or_default();
        let now = now_secs();
        let list = rows
            .iter()
            .filter_map(|r| {
                Some(Entry {
                    path: PathBuf::from(r.get("path")?.as_str()?),
                    opened: r.get("opened").and_then(Value::as_u64).map(|s| now.saturating_sub(s)),
                    missing: r.get("missing").and_then(Value::as_bool).unwrap_or(false),
                })
            })
            .collect();
        let notices = v.get("notices").and_then(Value::as_u64).map(|n| n as u32).or(Some(0));
        Self { file: None, list, fixture_notices: notices }
    }

    fn write(&self, next: &[Entry]) -> Result<(), String> {
        let Some(f) = &self.file else { return Ok(()) };
        let paths: Vec<String> = next.iter().map(|e| e.path.display().to_string()).collect();
        let opened: serde_json::Map<String, Value> =
            next.iter().filter_map(|e| Some((e.path.display().to_string(), json!(e.opened?)))).collect();
        crate::project::update_json(f, |m| {
            m.insert("recent".into(), json!(paths));
            m.insert("opened".into(), Value::Object(opened));
        })
    }

    /// Move `root` to the front with the time it was opened.
    fn record(&mut self, root: &std::path::Path) -> Result<(), String> {
        let mut next: Vec<Entry> = self.list.iter().filter(|e| e.path.as_path() != root).cloned().collect();
        next.insert(0, Entry { path: root.to_path_buf(), opened: Some(now_secs()), missing: false });
        next.truncate(KEEP);
        self.write(&next)?;
        self.list = next;
        Ok(())
    }

    /// Remove from list (Start: missing-folder dialog, row menu).
    pub fn remove(&mut self, path: &std::path::Path) -> Result<(), String> {
        let next: Vec<Entry> = self.list.iter().filter(|e| e.path.as_path() != path).cloned().collect();
        self.write(&next)?;
        self.list = next;
        Ok(())
    }

    pub fn list(&self) -> &[Entry] {
        &self.list
    }
}

/// "12 min ago", "Yesterday", "2 weeks ago": one relative family for the Recent time column.
pub fn ago(secs: u64) -> String {
    let (m, h, d) = (secs / 60, secs / 3600, secs / 86400);
    let n = |v: u64, one: &str, many: &str| if v == 1 { format!("1 {one} ago") } else { format!("{v} {many} ago") };
    match d {
        _ if secs < 60 => "Just now".into(),
        _ if h < 1 => format!("{m} min ago"),
        0 => n(h, "hour", "hours"),
        1 => "Yesterday".into(),
        2..=13 => n(d, "day", "days"),
        14..=59 => n(d / 7, "week", "weeks"),
        60..=364 => n(d / 30, "month", "months"),
        _ => n(d / 365, "year", "years"),
    }
}

/// The parent folder, middle-truncated keeping its last two segments; if even that does not fit,
/// "…/<second-last>/" + the end-truncated last segment (the parent is never dropped). `fits` measures.
pub fn mid_path(full: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(full) {
        return full.into();
    }
    let sep = if full.contains('\\') && !full.contains('/') { '\\' } else { '/' };
    let s = sep.to_string();
    let seg: Vec<&str> = full.split(sep).collect();
    let n = seg.len();
    let end = |head: String| {
        let mut t: Vec<char> = seg[n - 1].chars().collect();
        while t.len() > 1 && !fits(&format!("{head}{}…", t.iter().collect::<String>())) {
            t.pop();
        }
        format!("{head}{}…", t.iter().collect::<String>())
    };
    if n < 3 {
        return end(String::new());
    }
    let tail = seg[n - 2..].join(&s);
    let mut best = format!("…{s}{tail}");
    for i in 1..n - 2 {
        let c = format!("{}{s}…{s}{tail}", seg[..=i].join(&s));
        if fits(&c) { best = c } else { break }
    }
    if fits(&best) {
        return best;
    }
    let b = end(format!("…{s}{}{s}", seg[n - 2]));
    if fits(&b) { b } else { end(format!("…{s}")) }
}

/// Page tabs (§7.1): index into `PAGES`.
pub(super) const PAGES: [(&str, Icon, &str); 5] = [
    ("Board", Icon::Board, "Ctrl+Shift+1"),
    ("Shot", Icon::Shot, "Ctrl+Shift+2"),
    ("Machines", Icon::Machines, "Ctrl+Shift+3"),
    ("Models", Icon::Models, "Ctrl+Shift+4"),
    ("Settings", Icon::Settings, "Ctrl+Shift+5"),
];

/// Shell state that lives between frames (dialogs, Start focus, row tooltip).
#[derive(Default)]
pub struct Shell {
    /// File › Open project by path…: the text being typed.
    path_dialog: Option<String>,
    /// File › Project output folder…: the folder being typed.
    out_dialog: Option<String>,
    /// Recent entry whose folder is missing: its dialog is open (row drawn selected).
    missing: Option<PathBuf>,
    /// Locate… was chosen for this entry: the next Open-project pick re-points it.
    pub(super) locate_from: Option<PathBuf>,
    /// The user pressed a key or the pointer on Start: initial focus is no longer re-applied.
    touched: bool,
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
    pub(super) fn refresh_draft_dirty(&mut self) {
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
        let previous_path = std::mem::replace(&mut self.path_input, path.display().to_string());
        if self.load(ctx) { true } else {
            self.path_input = previous_path;
            false
        }
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
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.new_project_error = None;
            return;
        }
        let (mut create, mut cancel) = (false, false);
        egui::Window::new("New project").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO).show(ctx, |ui| {
            ui.label(RichText::new(format!("An empty project folder is created in {}", parent.display())).color(theme::text_dim(ui)));
            let r = ui.add(egui::TextEdit::singleline(&mut name).desired_width(320.0));
            create = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            ui.horizontal(|ui| {
                let c = super::theme::primary_button(ui, "Create project");
                self.new_project_create_rect = Some(c.rect);
                create |= c.clicked();
                cancel = ui.button("Cancel").clicked();
            });
            if let Some(e) = &self.new_project_error { ui.colored_label(theme::warn(ui), e); }
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

    pub(super) fn quit_request(&mut self, ctx: &egui::Context) {
        // Quit waits for the deletion helper; the user retries after the dialog closes (quit_pending is already cleared by the caller).
        if self.hint_delete_open() {
            return;
        }
        // Flush pending edits before the existing running-job close flow.
        if self.project.is_some() && self.unsaved() {
            if let Err(e) = self.save_project() { self.ai.notice = Some(format!("Not closing: {e}")); return; }
        }
        self.quitting = true;
        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    }

    pub(super) fn shortcuts(&mut self, ctx: &egui::Context) {
        use egui::Modifiers;
        let cs = Modifiers::COMMAND | Modifiers::SHIFT;
        for (i, k) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].into_iter().enumerate() {
            if ctx.input_mut(|inp| inp.consume_key(cs, k)) {
                self.goto_page(i);
            }
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::Comma)) {
            self.goto_page(4);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::S)) && self.project.is_some() {
            self.report_save();
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::N)) {
            self.new_project_pick(ctx);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::O)) {
            self.request_pick(ctx, PickKind::Project);
        }
        if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::I)) {
            self.request_pick(ctx, PickKind::Folder);
        } else if ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, Key::I)) {
            self.request_pick(ctx, PickKind::Clips);
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

    /// The page shown now (index into `PAGES`); `usize::MAX` = Start (no page tab is active).
    pub(super) fn page_now(&self) -> usize {
        if self.models_open {
            3
        } else if self.settings_open {
            4
        } else if self.clients_open {
            2
        } else if self.board_selected {
            0
        } else if (self.viewer.is_some() && self.open.is_some()) || self.shot_fx.is_some() {
            1
        } else if self.project.is_some() || self.board_fx.is_some() {
            0
        } else {
            usize::MAX
        }
    }

    /// Why page `p` cannot be shown now (tooltip of its disabled tab / menu item).
    fn page_block(&self, p: usize) -> Option<&'static str> {
        match p {
            0 | 1 if self.project.is_none() && self.board_fx.is_none() && self.shot_fx.is_none() => Some("Open a project first"),
            1 if self.viewer.is_none() && self.shot_fx.is_none() && self.board_fx.as_ref().is_none_or(|b| b.fx.cards.is_empty()) => Some("Open a shot from the Board"),
            _ => None,
        }
    }

    /// Page tabs, Window menu and Ctrl+Shift+1…5. Leaving Settings discards its draft (as its Back button does).
    pub(super) fn goto_page(&mut self, p: usize) {
        if p >= PAGES.len() || self.page_block(p).is_some() {
            return;
        }
        if p == 0 { self.board_selected = true; }
        if p == 1 { self.board_selected = false; }
        if p != 4 && self.settings_open {
            self.close_settings();
        }
        self.models_open = p == 3;
        if p == 2 && !self.clients_open {
            if let Some(v) = &mut self.viewer {
                v.pause();
            }
            self.clients_open = true;
            self.machines_entered();
            self.retry_endpoint(); // one more try when the page opens (no-op while the endpoint runs)
        }
        if p != 2 {
            self.clients_open = false;
        }
        if p == 4 && !self.settings_open {
            self.open_settings();
        }
    }

    /// Menu bar (24) and toolbar (36): §5.9, §7.1. Drawn in the 60 px top panel.
    pub(super) fn chrome(&mut self, ui: &mut Ui) {
        // The delete dialog is modal: keep menu/keyboard navigation from running File/Window actions behind it.
        if self.hint_delete_open() {
            return;
        }
        let ctx = ui.ctx().clone();
        let t = tokens(ui);
        self.refresh_draft_dirty();
        let has = self.project.is_some() || self.board_fx.is_some();
        let shot = self.viewer.is_some();
        let busy = self.imp.busy() || self.dialog_active();
        let busy_why = "An import or file dialog is already open";
        let full = ui.max_rect();
        let width = ctx.content_rect().width();
        let compact = width < 1200.0;

        // ---- menus
        let names: Vec<String> = self
            .recent
            .list()
            .iter()
            .map(|e| e.path.file_name().map_or_else(|| e.path.display().to_string(), |n| n.to_string_lossy().into_owned()))
            .collect();
        let mut recent_items: Vec<MenuItem> = names.iter().map(|n| MenuItem::new(n)).collect();
        if recent_items.is_empty() {
            recent_items.push(MenuItem::new("No recent projects").disabled(""));
        }
        let off = |on: bool, why: &'static str, it: MenuItem<'static>| if on { it } else { it.disabled(why) };
        let need_project = "Open a project first";
        let need_shot = "Open a shot first";
        let mut file = vec![
            MenuItem::new("New project…").shortcut("Ctrl+N"),
            MenuItem::new("Open project…").shortcut("Ctrl+O"),
            MenuItem::new("Open recent").submenu(&recent_items),
            MenuItem::new("Open project by path…"),
            MenuItem::sep(),
            off(has, need_project, MenuItem::new("Save project").shortcut("Ctrl+S")),
            off(has, need_project, MenuItem::new("Project output folder…")),
            off(has, need_project, MenuItem::new("Import shots…").shortcut("Ctrl+I")),
            off(has, need_project, MenuItem::new("Import folder…").shortcut("Ctrl+Shift+I")),
            MenuItem::sep(),
            MenuItem::new("Quit").shortcut("Ctrl+Q"),
        ];
        if busy {
            for i in [0, 1, 3, 7, 8] {
                file[i] = file[i].disabled(busy_why);
            }
        }
        let on_board = self.page_now() == 0;
        let has_shots = self.project.as_ref().is_some_and(|p| !p.clips.is_empty());
        let edit = [
            off(on_board && has_shots, "Open a project with shots first", MenuItem::new("Select all shots").shortcut("Ctrl+A")),
            off(on_board && !self.selected.is_empty(), "Select shots first", MenuItem::new("Assign selected shots").shortcut("Ctrl+Shift+A")),
        ];
        let view = [
            off(shot, need_shot, MenuItem::new("Fit")),
            off(shot, need_shot, MenuItem::new("Zoom in").shortcut("+")),
            off(shot, need_shot, MenuItem::new("Zoom out").shortcut("−")),
            MenuItem::sep(),
            off(shot, need_shot, MenuItem::new("Loop").shortcut("L")),
            off(shot, need_shot, MenuItem::new("Focus mode").shortcut("F")),
        ];
        let stages = [
            off(shot, need_shot, MenuItem::new("Media").shortcut("1")),
            off(shot, need_shot, MenuItem::new("Hint").shortcut("2")),
            off(shot, need_shot, MenuItem::new("Key").shortcut("3")),
            off(shot, need_shot, MenuItem::new("Review").shortcut("4")),
            off(shot, need_shot, MenuItem::new("Deliver").shortcut("5")),
        ];
        let blocks: Vec<Option<&'static str>> = (0..PAGES.len()).map(|p| self.page_block(p)).collect();
        let mut window: Vec<MenuItem> = PAGES
            .iter()
            .zip(&blocks)
            .map(|((n, _, k), b)| {
                let it = MenuItem::new(n).shortcut(k);
                match b { Some(why) => it.disabled(why), None => it }
            })
            .collect();
        window.extend([
            MenuItem::sep(),
            off(shot, need_shot, MenuItem::new("Properties").shortcut("P")),
            off(shot, need_shot, MenuItem::new("Bottom panel")),
            off(has, need_project, MenuItem::new("Queue")),
            MenuItem::new("Activity"),
            MenuItem::sep(),
            off(shot, need_shot, MenuItem::new("Reset panel sizes")),
        ]);
        let help: [MenuItem<'static>; 0] = [];
        let menus = [
            Menu { label: "File", items: &file },
            Menu { label: "Edit", items: &edit },
            Menu { label: "View", items: &view },
            Menu { label: "Shot", items: &stages },
            Menu { label: "Window", items: &window },
            Menu { label: "Help", items: &help },
        ];
        let shell = shell_fixture(&self.opts);
        let project = self.project.as_ref().map(|p| p.display_name.clone()).or_else(|| self.board_fx.as_ref().map(|b| b.fx.project.clone())).or_else(|| shell.map(|s| s.project.clone())).unwrap_or_default();
        let unsaved = match (&self.board_fx, shell) { (Some(b), _) => b.fx.saved == 0, (None, Some(s)) if !has => !s.saved, _ => has && self.unsaved() };
        let out = MenuBar::new(&menus, MenuBarRight { project: &project, unsaved }).dot_only(compact).at(full.min).width(full.width()).show(ui);
        self.menu_open = out.open;
        let clicked = out.clicked;

        // ---- toolbar: page tabs
        let bar = Rect::from_min_max(pos2(full.min.x, full.min.y + 24.0), pos2(full.max.x, full.min.y + 60.0));
        ui.painter().rect_filled(bar, 0.0, t.bg_app);
        ui.painter().rect_filled(Rect::from_min_max(pos2(bar.min.x, bar.max.y - 1.0), bar.max), 0.0, t.border_divider);
        let (models_attention, models_missing) = match shell {
            Some(s) => (s.models_missing > 0, Some(s.models_missing)),
            None => (self.models.startup_error_or_missing(), self.models.missing_count()),
        };
        let tips: Vec<String> = PAGES
            .iter()
            .enumerate()
            .zip(&blocks)
            .map(|((i, (n, _, k)), b)| {
                if let Some(why) = b { return format!("{n}\n{why}") }
                if i == 3 && models_attention {
                    let reason = models_missing.filter(|n| *n > 0).map_or_else(
                        || "Models need attention".to_owned(),
                        |n| format!("{n} required model{} missing", if n == 1 { "" } else { "s" }),
                    );
                    format!("Models\n{reason}\n{k}")
                } else {
                    format!("{n}\n{k}")
                }
            })
            .collect();
        let tabs: Vec<controls::Tab> = PAGES
            .iter()
            .zip(&blocks)
            .zip(&tips)
            .enumerate()
            .map(|(i, (((n, ic, _), b), tip))| {
                let mut tab = controls::Tab::new(n).icon(*ic);
                if i == 3 && models_attention {
                    if let Some(n) = models_missing.filter(|n| *n > 0) { tab = tab.badge(n.min(u32::MAX as usize) as u32); }
                    tab = tab.status(StageStatus::Blocked);
                }
                if b.is_some() { tab.disabled(tip) } else { tab.tip(tip) }
            })
            .collect();
        let before = self.page_now();
        let mut active = before;
        let mut rects = Vec::new();
        TabStrip::new(TabKind::Page, &mut active, &tabs)
            .name("Pages")
            .compact(compact)
            .whole_px(true)
            .at(pos2(bar.min.x + 8.0, bar.min.y + 7.0))
            .rects_out(&mut rects)
            .show(ui);
        self.clients_rect = rects.get(2).copied();
        self.settings_rect = rects.get(4).copied();
        if active != before {
            self.goto_page(active);
        }
        // ---- menu actions
        let Some(path) = clicked else { return };
        match (path[0], path.get(1).copied().unwrap_or(usize::MAX)) {
            (0, 0) => {
                self.new_project_pick(&ctx);
            }
            (0, 1) => {
                self.request_pick(&ctx, PickKind::Project);
            }
            (0, 2) => {
                if let Some(&i) = path.get(2) {
                    self.open_recent(&ctx, i);
                }
            }
            (0, 3) => self.shell.path_dialog = Some(self.path_input.clone()),
            (0, 5) => self.report_save(),
            (0, 6) => self.shell.out_dialog = Some(self.loc.root.display().to_string()),
            (0, 7) => { self.request_pick(&ctx, PickKind::Clips); }
            (0, 8) => { self.request_pick(&ctx, PickKind::Folder); }
            (0, 10) => self.quit_request(&ctx),
            (1, 0) => {
                self.selected = (0..self.project.as_ref().map_or(0, |p| p.clips.len())).collect();
            }
            (1, 1) => self.key_selected(&ctx),
            (2, i) => {
                if i == 5 {
                    let focus = !self.shot_page.focus();
                    self.shot_page.set_focus(focus);
                    self.focus_mode = focus;
                } else if let Some(v) = &mut self.viewer {
                    match i {
                        0 => v.fit(),
                        1 => v.zoom_by(1.25, Vec2::ZERO),
                        2 => v.zoom_by(0.8, Vec2::ZERO),
                        4 => v.toggle_loop(),
                        _ => {}
                    }
                }
            }
            (3, i) => {
                if let Some(t) = [Tab::Media, Tab::Hint, Tab::Key, Tab::Review, Tab::Deliver].get(i) {
                    self.switch_tab(*t);
                }
            }
            (4, i) if i < PAGES.len() => self.goto_page(i),
            (4, 6) => {
                self.shot_page.toggle_drawer();
                self.props_sheet = self.shot_page.drawer_open();
            }
            (4, 7) => {
                self.shot_page.set_dock_expanded(!self.shot_page.dock_expanded());
            }
            (4, 8) => {
                self.board_page.fx.qtab = "waiting".into();
                self.board_page.fx.drawer = 1;
                self.goto_page(0);
            }
            (4, 9) => self.activity_toggle(),
            (4, 11) => {
                ctx.memory_mut(|m| m.reset_areas());
                self.graph_collapsed = false;
                self.graph_short_open = true;
            }
            _ => {}
        }
    }

    /// Start status bar (§7.4, §8.8): activity and notices only.
    pub(super) fn start_status(&mut self, ui: &mut Ui) -> statusbar::StatusOut {
        let w = ui.ctx().content_rect().width();
        let width = if w >= 1600.0 { statusbar::Width::Wide } else if w >= 1200.0 { statusbar::Width::Medium } else { statusbar::Width::Compact };
        if let Some(s) = shell_fixture(&self.opts) {
            // Dev fixture: the project-open bar from the mockups, through the same control the Board draws.
            let meters = statusbar::Meters { cpu: s.cpu, ram_used: s.ram_used, ram_total: s.ram_total, gpu_pct: s.gpu_pct, gpu_label: &s.gpu_label, vram_used: s.vram_used, vram_total: s.vram_total };
            return statusbar::status_bar(ui, &statusbar::StatusData { notices: s.notices, queue_waiting: Some(s.waiting), meters: Some(meters), ..Default::default() }, width);
        }
        let notices = self.recent.fixture_notices.unwrap_or_else(|| self.notice_count());
        statusbar::status_bar(ui, &statusbar::StatusData { notices, failures: self.failure_count(), start: true, ..Default::default() }, width)
    }

    /// Open Recent row `i`: a missing folder opens the Locate / Remove dialog instead (§7.4).
    pub(super) fn open_recent(&mut self, ctx: &egui::Context, i: usize) {
        let Some(e) = self.recent.list().get(i).cloned() else { return };
        if e.missing || !e.path.is_dir() {
            self.shell.missing = Some(e.path);
        } else {
            self.open_project_path(ctx, e.path);
        }
    }

    /// The missing-folder entry, if its dialog is open (tests).
    pub fn missing_dialog(&self) -> Option<&std::path::Path> {
        self.shell.missing.as_deref()
    }

    /// Recent entries (tests).
    pub fn recent_entries(&self) -> &[Entry] {
        self.recent.list()
    }

    fn remove_recent(&mut self, path: &std::path::Path) {
        if let Err(e) = self.recent.remove(path) {
            self.ai.notice = Some(format!("Recent projects not saved: {e}"));
        }
    }

    /// A Project pick finished: when it came from Locate…, the old entry is replaced by the folder that opened.
    pub(super) fn located(&mut self, from: PathBuf) {
        if self.project.is_some() && self.load_error.is_none() {
            self.remove_recent(&from);
        }
    }

    /// Dialogs owned by the shell: missing folder (Locate… / Remove from list), Open project by path…, Project output folder….
    pub(super) fn shell_dialogs(&mut self, ctx: &egui::Context) {
        if let Some(path) = self.shell.missing.clone() {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            let full = path.display().to_string();
            let out = dialog::Dialog::new("missing-folder", "Project folder not found", dialog::DialogSize::M)
                .body(|ui| {
                    let t = tokens(ui);
                    let mono = FontId::new(12.0, FontFamily::Monospace);
                    let fmt = |f: FontId| egui::TextFormat { font_id: f, color: t.text_primary, line_height: Some(18.0), ..Default::default() };
                    let mut job = egui::text::LayoutJob::default();
                    job.append("The folder of ", 0.0, fmt(ty::label()));
                    job.append(&name, 0.0, fmt(mono.clone()));
                    job.append(" is not where it was saved. If it is on a drive or network share that is not connected, connect it and try again, or locate the folder.", 0.0, fmt(ty::label()));
                    ui.add(egui::Label::new(job).wrap());
                    ui.add_space(1.0);
                    let mut p = egui::text::LayoutJob::default();
                    p.append(&full, 0.0, fmt(mono));
                    ui.add(egui::Label::new(p).truncate());
                })
                .footer(|f| f.ghost_left("Remove from list").cancel("Cancel").primary("Locate…"))
                .show(ctx);
            if out.ghost_clicked {
                self.remove_recent(&path);
                self.shell.missing = None;
            } else if out.primary_clicked {
                self.shell.missing = None;
                if self.request_pick(ctx, PickKind::Project) {
                    self.shell.locate_from = Some(path);
                }
            } else if out.closed {
                self.shell.missing = None;
            }
        }
        if let Some(mut text) = self.shell.path_dialog.take() {
            let mut go = false;
            let out = dialog::Dialog::new("open-by-path", "Open project by path", dialog::DialogSize::S)
                .body(|ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY).hint_text("Project folder"));
                    go = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
                })
                .footer(|f| f.cancel("Cancel").primary("Open"))
                .show(ctx);
            let path = text.trim().to_owned();
            if (go || out.primary_clicked) && !path.is_empty() {
                self.open_project_path(ctx, PathBuf::from(path));
            } else if !out.closed {
                self.shell.path_dialog = Some(text);
            }
        }
        if let Some(mut text) = self.shell.out_dialog.take() {
            let out = dialog::Dialog::new("output-folder", "Project output folder", dialog::DialogSize::M)
                .body(|ui| {
                    ui.add(egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY));
                    dialog::paragraph(ui, "New renders use this root. Existing outputs stay in place.");
                })
                .footer(|f| f.cancel("Cancel").primary("Save folder"))
                .show(ctx);
            if out.primary_clicked {
                self.loc.root = text.trim().into();
                self.loc_note = Some(match self.save_output_location() {
                    Ok(()) => "Project output folder saved".into(),
                    Err(e) => {
                        self.ai.notice = Some(format!("Project output folder not saved: {e}"));
                        format!("Not saved: {e}")
                    }
                });
            } else if !out.closed {
                self.shell.out_dialog = Some(text);
            }
        }
    }

    /// Start screen (§7.4, §7.6): title, New / Open, Recent. Painted at absolute positions in the main area.
    pub(super) fn welcome(&mut self, ui: &mut Ui, busy: bool, why: &str, want: &mut Option<PickKind>) {
        let ctx = ui.ctx().clone();
        let t = tokens(ui);
        let screen = ctx.content_rect();
        let main = Rect::from_min_max(pos2(screen.min.x, screen.min.y + 60.0), pos2(screen.max.x, screen.max.y - 22.0));
        ui.painter().rect_filled(main, 0.0, t.bg_panel);
        let compact_or_short = screen.width() < 1200.0 || screen.height() < 760.0;
        let top = if compact_or_short { 16.0 } else { (main.height() * 0.25).round() };
        let x0 = main.min.x + ((main.width() - 640.0) / 2.0).floor();
        let y0 = main.min.y + top;
        let p = ui.painter().clone();
        let title = controls::line(&p, "Open or create a project", ty::display(), 28.0, 640.0);
        controls::paint_line(&p, Rect::from_min_size(pos2(x0, y0), vec2(640.0, 28.0)), title, 28.0, Align::Min, t.text_primary, 0.0);
        let by = y0 + 44.0;
        let new = ui.add(buttons::secondary_button("New project…").height(32.0).width(180.0).disabled_if(busy, why).at(pos2(x0, by)));
        let open = ui.add(buttons::primary_button("Open project…").height(32.0).width(180.0).disabled_if(busy, why).at(pos2(x0 + 188.0, by)));
        if !busy {
            menus::tooltip(&new, "New project…", Some("Ctrl+N"), "");
            menus::tooltip(&open, "Open project…", Some("Ctrl+O"), "");
        }
        if new.clicked() {
            *want = Some(PickKind::NewProject);
        }
        if open.clicked() {
            *want = Some(PickKind::Project);
        }
        // §7.4 initial focus: Open project… with its ring, as on any keyboard focus.
        // Kept until the user first presses a key or the pointer: the window system can drop focus while the window opens.
        if !self.shell.touched && ctx.input(|i| i.pointer.any_pressed() || i.events.iter().any(|e| matches!(e, egui::Event::Key { pressed: true, .. }))) {
            self.shell.touched = true;
        }
        if !self.shell.touched && !busy && ctx.memory(|m| m.focused().is_none()) {
            open.request_focus();
            ctx.data_mut(|d| d.insert_temp(egui::Id::NULL.with("keyloom-kb-focus"), true));
            ctx.request_repaint();
        }
        let mut bottom = by + 32.0;
        if let Some(e) = &self.load_error {
            let g = controls::line(&p, e, ty::small(), 14.0, 640.0);
            controls::paint_line(&p, Rect::from_min_size(pos2(x0, by + 36.0), vec2(640.0, 14.0)), g, 14.0, Align::Min, t.error, 0.0);
            bottom = by + 50.0;
        }
        if !self.recent.list().is_empty() {
            let ly = by + 32.0 + 24.0;
            let label = controls::line(&p, "Recent", ty::chip(), 16.0, 640.0);
            controls::paint_line(&p, Rect::from_min_size(pos2(x0, ly), vec2(640.0, 16.0)), label, 16.0, Align::Min, t.text_secondary, 0.0);
            let n = self.recent.list().len();
            let vp = 256f32.min(main.height() - 152.0).max(32.0);
            let list = Rect::from_min_size(pos2(x0, ly + 20.0), vec2(640.0, (n as f32 * 32.0).min(vp)));
            bottom = list.max.y;
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list).layout(egui::Layout::top_down(Align::Min)));
            let mut act: Option<(usize, usize)> = None;
            scroll::scroll_v(&mut child, "start-recent", |ui| {
                ui.spacing_mut().item_spacing = Vec2::ZERO;
                for i in 0..n {
                    if let Some(a) = self.recent_row(ui, i, busy) {
                        act = Some((i, a));
                    }
                }
            });
            match act {
                Some((i, 0)) => self.open_recent(&ctx, i),
                Some((i, 1)) => {
                    if let Some(e) = self.recent.list().get(i) {
                        super::reveal(&e.path.clone(), false, &mut self.ai);
                    }
                }
                Some((i, 2)) => {
                    if let Some(e) = self.recent.list().get(i).cloned() {
                        self.remove_recent(&e.path);
                    }
                }
                _ => {}
            }
        }
        // The cursor goes below the block (import notices drawn after it never overlap the Start block).
        ui.allocate_rect(Rect::from_min_max(main.min, pos2(main.max.x, bottom + 16.0)), Sense::hover());
    }

    /// One Recent row (§7.4): `[8][name 256][16][parent folder][16][time 72][8]`. Returns the chosen action
    /// (0 open, 1 reveal folder, 2 remove from list).
    fn recent_row(&mut self, ui: &mut Ui, i: usize, busy: bool) -> Option<usize> {
        let t = tokens(ui);
        let e = self.recent.list()[i].clone();
        let name = e.path.file_name().map_or_else(|| e.path.display().to_string(), |n| n.to_string_lossy().into_owned());
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::accesskit::Role::Row, !busy, &name));
        let p = ui.painter().clone();
        let selected = self.shell.missing.as_deref() == Some(e.path.as_path());
        let hot = resp.hovered() && !selected;
        if selected {
            p.rect_filled(r, 0.0, t.accent_tint);
            p.rect_filled(Rect::from_min_size(r.min, vec2(2.0, 32.0)), 0.0, t.accent_bright);
        } else if hot {
            p.rect_filled(r, 0.0, t.bg_hover);
        }
        let dim = if hot { t.text_secondary } else { t.text_tertiary };
        let row = |x: f32, w: f32| Rect::from_min_size(pos2(x, r.min.y), vec2(w, 32.0));
        let g = controls::line(&p, &name, ty::label(), 16.0, 256.0);
        controls::paint_line(&p, row(r.min.x + 8.0, 256.0), g, 16.0, Align::Min, t.text_primary, 0.0);
        let mut x = r.min.x + 8.0 + 256.0 + 16.0;
        let path_end = r.max.x - 8.0 - 72.0 - 16.0;
        if e.missing {
            icons::paint(&p, Rect::from_min_size(pos2(x, r.min.y + 9.0), vec2(14.0, 14.0)), Icon::Warning, IconSize::S14, t.warning);
            x += 18.0;
            let g = controls::line(&p, "Folder not found", ty::small(), 14.0, f32::INFINITY);
            let w = g.size().x;
            controls::paint_line(&p, row(x, w + 1.0), g, 14.0, Align::Min, t.warning, 0.0);
            x += w + 8.0;
        }
        let parent = e.path.parent().map(|q| q.display().to_string()).unwrap_or_default();
        let avail = (path_end - x).max(0.0);
        let fits = |s: &str| controls::line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x <= avail;
        let shown = mid_path(&parent, fits);
        let g = controls::line(&p, &shown, ty::mono(), 14.0, f32::INFINITY);
        controls::paint_line(&p, row(x, avail), g, 14.0, Align::Min, dim, 0.0);
        let when = e.opened.map(|o| ago(now_secs().saturating_sub(o))).unwrap_or_default();
        let g = controls::line(&p, &when, ty::small(), 14.0, 72.0);
        controls::paint_line(&p, row(r.max.x - 8.0 - 72.0, 72.0), g, 14.0, Align::Max, dim, 0.0);
        if resp.has_focus() && controls::keyboard_focus(ui.ctx()) {
            p.rect_stroke(r, 0.0, egui::Stroke::new(2.0, t.focus_ring), egui::StrokeKind::Inside);
        }
        // Tooltip (§5.11): name, then the keys in tertiary; 4 px below the row, at the pointer, after 500 ms still.
        if resp.hovered() {
            let still = ui.input(|inp| inp.pointer.time_since_last_movement());
            if still >= 0.5 {
                if let Some(ptr) = ui.input(|inp| inp.pointer.hover_pos()) {
                    row_tip(ui.ctx(), pos2(ptr.x.round(), r.max.y + 4.0), &name, e.missing);
                }
            } else {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(0.5 - still));
            }
        }
        if busy {
            return None;
        }
        let items = [MenuItem::new("Open"), MenuItem::new("Reveal folder").disabled_if_missing(e.missing), MenuItem::new("Remove from list")];
        if let Some(c) = menus::context_menu(&resp, &name, &items) {
            return c.first().copied();
        }
        if resp.clicked() || (resp.has_focus() && ui.input(|inp| inp.key_pressed(Key::Enter) && !inp.modifiers.any())) {
            return Some(0);
        }
        None
    }
}

trait MissingItem {
    fn disabled_if_missing(self, missing: bool) -> Self;
}
impl MissingItem for MenuItem<'_> {
    fn disabled_if_missing(self, missing: bool) -> Self {
        if missing { self.disabled("Folder not found") } else { self }
    }
}

/// The Recent row tooltip card (§5.11 sheet `.tip`): `bg.overlay`, 1 px `border.overlay`, radius 4, padding 6 × 8.
fn row_tip(ctx: &egui::Context, at: egui::Pos2, name: &str, missing: bool) {
    let t = tokens(ctx);
    egui::Area::new(egui::Id::unique("recent-row-tip")).order(egui::Order::Tooltip).fixed_pos(at).interactable(false).show(ctx, |ui| {
        let p = ui.painter();
        let fmt = |f: FontId| ty::format(f, t.text_tertiary, 14.0);
        let mut job = egui::text::LayoutJob::default();
        if missing {
            job.append("Folder not found", 0.0, fmt(ty::small()));
        } else {
            for (s, mono) in [("Enter", true), (" to open · ", false), ("Shift+F10", true), (" for more", false)] {
                job.append(s, 0.0, fmt(if mono { ty::mono() } else { ty::small() }));
            }
        }
        let l2 = p.layout_job(job);
        let l1 = controls::line(p, name, ty::small(), 14.0, f32::INFINITY);
        let w = (l1.size().x.max(l2.size().x) + 18.0).ceil();
        let rect = Rect::from_min_size(at, vec2(w, 42.0));
        p.add(ui.visuals().popup_shadow.as_shape(rect, egui::CornerRadius::same(4)));
        p.rect_filled(rect, 4.0, t.bg_overlay);
        p.rect_stroke(rect, 4.0, egui::Stroke::new(1.0, t.border_overlay), egui::StrokeKind::Inside);
        controls::paint_line(p, Rect::from_min_size(rect.min + vec2(9.0, 7.0), vec2(w, 14.0)), l1, 14.0, Align::Min, t.text_primary, 0.0);
        p.galley(rect.min + vec2(9.0, 21.0), l2, t.text_tertiary);
        ui.allocate_rect(rect, Sense::hover());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_match_the_start_mockups() {
        let d = 86400;
        for (s, want) in [(720, "12 min ago"), (d + 3600, "Yesterday"), (2 * d + 9, "2 days ago"), (4 * d, "4 days ago"), (14 * d, "2 weeks ago"),
            (21 * d, "3 weeks ago"), (28 * d, "4 weeks ago"), (35 * d, "5 weeks ago"), (42 * d, "6 weeks ago"), (61 * d, "2 months ago"),
            (70 * d, "2 months ago"), (92 * d, "3 months ago"), (3 * 3600, "3 hours ago"), (5, "Just now")] {
            assert_eq!(ago(s), want, "{s}");
        }
    }

    #[test]
    fn parent_path_middle_truncates_and_keeps_the_parent() {
        let fits = |n: usize| move |s: &str| s.chars().count() <= n;
        let p = "/mnt/raid/projects/2026/acme_docs/interview_series/season_02";
        assert_eq!(mid_path(p, fits(100)), p);
        assert_eq!(mid_path(p, fits(40)), "/mnt/raid/…/interview_series/season_02");
        assert_eq!(mid_path(p, fits(36)), "/mnt/…/interview_series/season_02");
        assert_eq!(mid_path(p, fits(21)), "…/interview_series/s…");
        assert_eq!(mid_path(p, fits(20)), "…/season_02…");
        assert!(mid_path(p, fits(10)).starts_with("…/"));
    }

    type Kinds = std::sync::Arc<std::sync::Mutex<Vec<PickKind>>>;

    /// A Start app with one missing Recent entry (`gone`); the picker records its kinds and answers `answer`.
    fn start_app(tag: &str, answer: Option<PathBuf>) -> (egui::Context, App, PathBuf, Kinds) {
        let d = tmp(tag);
        let gone = d.join("gone");
        let fx = d.join("fx.json");
        std::fs::write(&fx, json!({"recent": [{"name": "gone", "path": gone.display().to_string(), "opened": 60, "missing": true}]}).to_string()).unwrap();
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        crate::ui::fonts::install(&ctx);
        let kinds: Kinds = Default::default();
        let k = kinds.clone();
        let opts = AppOptions { settings_path: Some(d.join("settings.json")), recent_fixture: Some(fx), ..Default::default() };
        let app = App::with_picker(&ctx, opts, std::sync::Arc::new(move |kind, _| {
            k.lock().unwrap().push(kind);
            answer.clone().into_iter().collect()
        }));
        (ctx, app, d, kinds)
    }

    fn frame(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> egui::FullOutput {
        let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1920.0, 1080.0))), events, ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
        out
    }

    fn node(out: &egui::FullOutput, label: &str) -> Option<egui::accesskit::Node> {
        out.platform_output.accesskit_update.as_ref()?.nodes.iter().find(|(_, n)| n.label() == Some(label)).map(|(_, n)| n.clone())
    }

    fn click(ctx: &egui::Context, app: &mut App, label: &str) {
        let out = frame(ctx, app, vec![]);
        let nodes = &out.platform_output.accesskit_update.as_ref().unwrap().nodes;
        let n = nodes.iter().find(|(_, n)| n.role() == egui::accesskit::Role::MenuItem && n.label() == Some(label))
            .or_else(|| nodes.iter().find(|(_, n)| n.label() == Some(label)))
            .unwrap_or_else(|| panic!("no {label}"));
        let b = n.1.bounds().unwrap();
        let at = egui::pos2(((b.x0 + b.x1) / 2.0) as f32, ((b.y0 + b.y1) / 2.0) as f32);
        let btn = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(ctx, app, vec![egui::Event::PointerMoved(at)]);
        frame(ctx, app, vec![btn(true)]);
        frame(ctx, app, vec![btn(false)]);
    }

    fn key(k: Key, m: egui::Modifiers) -> Vec<egui::Event> {
        vec![egui::Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m }]
    }

    /// §7.4: the Start screen opens with keyboard focus on "Open project…".
    #[test]
    fn start_initial_focus_is_open_project() {
        let (ctx, mut app, d, _) = start_app("focus", None);
        let mut out = frame(&ctx, &mut app, vec![]);
        for _ in 0..12 {
            out = frame(&ctx, &mut app, vec![]);
        }
        let open = node(&out, "Open project…").expect("Open project… drawn");
        let focused = ctx.memory(|m| m.focused()).expect("something focused");
        let id = out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_, n)| n.label() == open.label()).unwrap().0;
        assert_eq!(focused.accesskit_id(), id);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A missing folder opens the dialog; Remove from list removes it.
    #[test]
    fn missing_row_dialog_remove_from_list() {
        let (ctx, mut app, d, _) = start_app("remove", None);
        frame(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, "gone");
        assert!(app.missing_dialog().is_some_and(|p| p.ends_with("gone")), "dialog opened by the row");
        click(&ctx, &mut app, "Remove from list");
        assert!(app.missing_dialog().is_none());
        assert!(app.recent_entries().is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Locate… asks for a project folder; the folder that opens replaces the missing entry.
    #[test]
    fn missing_row_dialog_locate_repoints_the_entry() {
        let d = tmp("locate-proj");
        let proj = d.join("moved");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
        let (ctx, mut app, d2, kinds) = start_app("locate", Some(proj.clone()));
        frame(&ctx, &mut app, vec![]);
        app.open_recent(&ctx, 0);
        click(&ctx, &mut app, "Locate…");
        for _ in 0..200 {
            if app.project.is_some() { break }
            std::thread::sleep(std::time::Duration::from_millis(10));
            frame(&ctx, &mut app, vec![]);
        }
        assert_eq!(kinds.lock().unwrap().as_slice(), &[PickKind::Project]);
        assert_eq!(app.recent_entries().iter().map(|e| e.path.clone()).collect::<Vec<_>>(), vec![app.project.as_ref().unwrap().root.clone()]);
        let _ = std::fs::remove_dir_all(&d);
        let _ = std::fs::remove_dir_all(&d2);
    }

    /// File shortcuts still fire: Ctrl+O / Ctrl+N ask for a folder, Ctrl+S saves, Ctrl+Q quits.
    #[test]
    fn file_shortcuts_fire() {
        let (ctx, mut app, d, kinds) = start_app("keys", None);
        frame(&ctx, &mut app, vec![]);
        for (k, want) in [(Key::O, PickKind::Project), (Key::N, PickKind::NewProject)] {
            frame(&ctx, &mut app, key(k, egui::Modifiers::COMMAND));
            for _ in 0..200 {
                if kinds.lock().unwrap().last() == Some(&want) && !app.dialog_active() { break }
                std::thread::sleep(std::time::Duration::from_millis(10));
                frame(&ctx, &mut app, vec![]);
            }
            assert_eq!(kinds.lock().unwrap().last(), Some(&want));
        }
        let proj = d.join("p");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
        app.open_project_path(&ctx, proj.clone());
        assert!(app.project.is_some());
        app.loc.root = d.join("renders");
        assert!(app.unsaved());
        frame(&ctx, &mut app, key(Key::S, egui::Modifiers::COMMAND));
        assert!(!app.unsaved(), "Ctrl+S saved the output folder");
        frame(&ctx, &mut app, key(Key::Q, egui::Modifiers::COMMAND));
        assert!(app.quitting);
        let _ = std::fs::remove_dir_all(&d);
    }

    fn project_with_clip(parent: &Path, name: &str) -> PathBuf {
        let root = parent.join(name);
        let frames = root.join("clips/shot/Frames");
        std::fs::create_dir_all(&frames).unwrap();
        std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
        std::fs::write(root.join("clips/shot/clip.json"), "{}").unwrap();
        std::fs::write(frames.join("frame0001.png"), include_bytes!("../../../tests/fixtures/shot-plate.png")).unwrap();
        root
    }

    #[test]
    fn g2_project_picker_and_board_navigation_keep_the_open_shot_until_replacement() {
        let (ctx, mut app, d, _) = start_app("g2-navigation", None);
        let current = project_with_clip(&d, "current");
        let next = d.join("next");
        std::fs::create_dir_all(&next).unwrap();
        std::fs::write(next.join("project.json"), r#"{"version":2}"#).unwrap();
        assert!(app.open_project_path(&ctx, current.clone()));
        assert_eq!(app.project.as_ref().unwrap().clips.len(), 1);
        app.open_clip(&ctx, 0);
        assert!(app.viewer.is_some());
        app.goto_page(0);
        assert_eq!(app.page_now(), 0);
        assert!(app.viewer.is_some());
        app.goto_page(1);
        assert_eq!(app.page_now(), 1);
        assert_eq!(app.open, Some(0));
        app.held_pick = Some((PickKind::Project, vec![next.clone()]));
        app.import_tick(&ctx);
        assert_eq!(app.project_root(), Some(next.as_path()));
        assert_eq!(app.page_now(), 0);
        assert!(app.viewer.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_failed_project_open_keeps_the_current_shot() {
        let (ctx, mut app, d, _) = start_app("g2-open-failure", None);
        let current = project_with_clip(&d, "current");
        assert!(app.open_project_path(&ctx, current.clone()));
        app.open_clip(&ctx, 0);
        assert!(!app.open_project_path(&ctx, d.join("missing")));
        assert_eq!(app.project_root(), Some(current.as_path()));
        assert_eq!(app.open, Some(0));
        assert!(app.viewer.is_some());
        assert!(app.load_error.is_some());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_global_status_opens_project_activity_and_notices_with_failure_count() {
        let (ctx, mut app, d, _) = start_app("g2-global-activity", None);
        app.recent.fixture_notices = None;
        app.opts.startup_notice = Some("Startup warning".into());
        app.activity_inject_for_test(vec![
            crate::ui::activity::Step::new("g2-job", "Keying shot", crate::ui::activity::Level::Error),
        ]);
        assert_eq!(app.notice_count(), 1);
        assert_eq!(app.failure_count(), 1);
        let out = frame(&ctx, &mut app, vec![]);
        assert!(node(&out, "No jobs running, 1 unacknowledged failures").is_some());
        assert!(node(&out, "1 notices").is_some());
        click(&ctx, &mut app, "No jobs running, 1 unacknowledged failures");
        assert!(app.activity_is_open());
        assert_eq!(app.strip.activity.tab, 0);
        let activity = frame(&ctx, &mut app, vec![]);
        let labels = activity.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().filter_map(|(_, n)| n.label()).collect::<Vec<_>>();
        assert!(labels.iter().any(|label| label.contains("Keying shot")));
        assert!(app.active_notice_text().iter().any(|notice| notice == "Startup warning"));
        click(&ctx, &mut app, "1 notices");
        assert!(app.activity_is_open());
        assert_eq!(app.strip.activity.tab, 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_queue_and_hardware_status_route_to_their_pages() {
        let (ctx, mut app, d, _) = start_app("g2-status-routes", None);
        let project = d.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
        assert!(app.open_project_path(&ctx, project));
        use crate::ui::controls::statusbar::{Segment, StatusOut};
        app.route_status(StatusOut { clicked: Some(Segment::Queue), segments: vec![] });
        assert_eq!(app.page_now(), 0);
        assert_eq!(app.board_page.fx.qtab, "waiting");
        app.route_status(StatusOut { clicked: Some(Segment::Gpu), segments: vec![] });
        assert_eq!(app.page_now(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_new_project_escape_cancels_and_overlays_own_settings_escape() {
        let (ctx, mut app, d, _) = start_app("g2-escape", None);
        app.ask_project_name(d.clone());
        frame(&ctx, &mut app, key(Key::Escape, egui::Modifiers::NONE));
        assert!(app.new_project_draft.is_none());
        app.open_settings();
        frame(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, "File");
        assert!(app.menu_open);
        frame(&ctx, &mut app, key(Key::Escape, egui::Modifiers::NONE));
        assert!(app.settings_open, "Escape over a menu must not close Settings");
        assert!(!app.menu_open, "Escape closes the menu first");
        app.activity_open_on(0, Rect::from_min_size(pos2(8.0, 8.0), vec2(480.0, 420.0)));
        frame(&ctx, &mut app, key(Key::Escape, egui::Modifiers::NONE));
        assert!(app.settings_open, "Escape over Activity must not close Settings");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_menu_actions_use_live_shot_state_and_remove_unapproved_help_placeholder() {
        let (ctx, mut app, d, _) = start_app("g2-menus", None);
        let project = project_with_clip(&d, "project");
        assert!(app.open_project_path(&ctx, project));
        app.open_clip(&ctx, 0);
        frame(&ctx, &mut app, vec![]);
        click(&ctx, &mut app, "Window");
        let window = frame(&ctx, &mut app, vec![]);
        let labels: Vec<_> = window.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().filter_map(|(_, n)| n.label()).collect();
        assert!(window.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().any(|(_, n)| n.role() == egui::accesskit::Role::MenuItem && n.label() == Some("Properties")));
        for approved in ["Properties", "Bottom panel", "Queue", "Activity", "Reset panel sizes"] {
            assert!(labels.contains(&approved), "missing approved Window item: {approved}");
        }
        assert!(!labels.contains(&"Keyboard shortcuts"));
        assert!(!app.shot_page.drawer_open());
        click(&ctx, &mut app, "Properties");
        assert!(app.shot_page.drawer_open());
        assert!(app.props_sheet);
        click(&ctx, &mut app, "View");
        click(&ctx, &mut app, "Focus mode");
        assert!(app.shot_page.focus());
        assert!(app.focus_mode);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn g2_import_shortcut_and_file_menu_request_the_same_pick_kinds() {
        let (ctx, mut app, d, kinds) = start_app("g2-import-shortcuts", None);
        let project = d.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
        assert!(app.open_project_path(&ctx, project));
        for (mods, want) in [
            (egui::Modifiers::COMMAND, PickKind::Clips),
            (egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, PickKind::Folder),
        ] {
            frame(&ctx, &mut app, key(Key::I, mods));
            for _ in 0..200 {
                if kinds.lock().unwrap().last() == Some(&want) { break }
                std::thread::sleep(std::time::Duration::from_millis(10));
                frame(&ctx, &mut app, vec![]);
            }
            assert_eq!(kinds.lock().unwrap().last(), Some(&want));
        }
        for (label, want) in [("Import shots…", PickKind::Clips), ("Import folder…", PickKind::Folder)] {
            click(&ctx, &mut app, "File");
            let out = frame(&ctx, &mut app, vec![]);
            let labels = out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().filter_map(|(_, n)| n.label()).collect::<Vec<_>>();
            assert!(labels.contains(&label));
            let before = kinds.lock().unwrap().len();
            click(&ctx, &mut app, label);
            for _ in 0..200 {
                if kinds.lock().unwrap().len() > before && !app.dialog_active() { break }
                std::thread::sleep(std::time::Duration::from_millis(10));
                frame(&ctx, &mut app, vec![]);
            }
            assert_eq!(kinds.lock().unwrap().len(), before + 1);
            assert_eq!(kinds.lock().unwrap().last(), Some(&want));
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-recent-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Times are stored beside the old `recent` list, unknown keys stay, and an old file (no times) still loads.
    #[test]
    fn recent_file_round_trip_keeps_times_unknown_keys_and_old_format() {
        let d = tmp("rt");
        let (a, b) = (d.join("a"), d.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        let f = d.join("recent_projects.json");
        std::fs::write(&f, json!({"recent": [a.display().to_string()], "custom": {"x": 1}}).to_string()).unwrap();
        let settings = d.join("settings.json");
        let mut r = Recent::load(Some(&settings));
        assert_eq!(r.list().len(), 1);
        assert_eq!(r.list()[0].opened, None, "old format: no time");
        r.record(&b).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(v["custom"], json!({"x": 1}));
        assert_eq!(v["recent"], json!([b.display().to_string(), a.display().to_string()]));
        let back = Recent::load(Some(&settings));
        assert!(back.list()[0].opened.is_some_and(|t| t + 5 >= now_secs()));
        assert_eq!(back.list()[1].opened, None);
        let mut back = back;
        back.remove(&a).unwrap();
        assert_eq!(Recent::load(Some(&settings)).list().iter().map(|e| e.path.clone()).collect::<Vec<_>>(), vec![b.clone()]);
        for i in 0..25 {
            let p = d.join(format!("p{i}"));
            back.record(&p).unwrap();
        }
        assert_eq!(Recent::load(Some(&settings)).list().len(), KEEP);
        let _ = std::fs::remove_dir_all(&d);
    }
}
