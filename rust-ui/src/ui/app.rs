//! Import clips, key them, review results, and deliver exports in the native app.
use super::ai::AiUi;
use super::graph;
use super::import::{CardState, Dest, ImportUi};
use super::picker::{rfd_backend, Backend, PickKind, Picker};
use super::settings::Page as SettingsPage;
use super::theme;
use super::viewer::{Viewer, DIM, WARN};
use crate::contracts::*;
use crate::decode::exr_header_summary;
use crate::frames::{Availability, FrameIndex};
use crate::import::Sources;
use crate::project::*;
use crate::settings::{default_path, Store};
use eframe::egui::{self, Align, Color32, Key, Layout, Panel, RichText, Sense, Ui, Vec2};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

pub const BANNER: &str = "Import > Hint > Key > Review > Deliver | In/Out, colour and hint settings are saved in the project";
mod hint;
mod media;
mod ws;
const GRAPH_DEFAULT: f32 = 220.0;
const GRAPH_MIN: f32 = 120.0;
/// Hint page bar: actions, tools and brush rows stay visible without scrolling.
const HINT_TOOLS_H: f32 = 132.0;
const GRAPH_HEADER: f32 = 32.0;
const MIN_VIEWER: f32 = 300.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Media,
    Hint,
    Key,
    Review,
    Deliver,
}
impl Tab {
    pub const ALL: [Tab; 5] = [Tab::Media, Tab::Hint, Tab::Key, Tab::Review, Tab::Deliver];
    pub fn label(self) -> &'static str {
        ["Media", "Hint", "Key", "Review", "Deliver"][self as usize]
    }
    pub fn parse(s: &str) -> Option<Tab> {
        Tab::ALL
            .into_iter()
            .find(|t| t.label().eq_ignore_ascii_case(s))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Node {
    Media,
    Hint,
    Key,
    Out(View),
}
fn to_kind(n: Node) -> graph::NodeKind {
    use graph::{NodeKind as K, OutKind as O};
    match n {
        Node::Media => K::Media,
        Node::Hint => K::Hint,
        Node::Key => K::Key,
        Node::Out(View::Fg) => K::Out(O::Fg),
        Node::Out(View::Matte) => K::Out(O::Matte),
        Node::Out(View::Comp) => K::Out(O::Comp),
        Node::Out(_) => K::Out(O::Processed),
    }
}
fn from_kind(k: graph::NodeKind) -> Node {
    use graph::{NodeKind as K, OutKind as O};
    match k {
        K::Media => Node::Media,
        K::Hint => Node::Hint,
        K::Key => Node::Key,
        K::Out(O::Fg) => Node::Out(View::Fg),
        K::Out(O::Matte) => Node::Out(View::Matte),
        K::Out(O::Comp) => Node::Out(View::Comp),
        K::Out(O::Processed) => Node::Out(View::Processed),
    }
}
impl Node {
    fn title(self) -> &'static str {
        match self {
            Node::Media => "Media",
            Node::Hint => "Hint",
            Node::Key => "Key",
            Node::Out(v) => v.label(),
        }
    }
    fn tab(self) -> Tab {
        match self {
            Node::Media => Tab::Media,
            Node::Hint => Tab::Hint,
            Node::Key => Tab::Key,
            Node::Out(_) => Tab::Deliver,
        }
    }
}

/// Closing the window while AI workers or the model helper are active.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseState {
    None,
    /// Work is running: the user chooses (window close was cancelled).
    Prompt,
    /// Graceful stop requested; the window stays open until workers and any model helper have exited.
    Stopping,
}

#[derive(Default, Clone)]
pub struct AppOptions {
    pub project: Option<PathBuf>,
    pub global_output_dir: Option<PathBuf>,
    pub cache_mb: Option<usize>,
    pub open_clip: Option<String>,
    pub tab: Option<Tab>,
    pub view: Option<View>,
    pub frame: Option<usize>,
    pub ab: Option<View>,
    pub play: bool,
    /// Dev/QA: comma-separated synthetic egui events, e.g. `i,Shift+Right,click:605x66`, one per 400 ms.
    pub keys: Vec<String>,
    pub screenshot: Option<PathBuf>,
    pub screenshot_after: Option<f32>,
    /// Where new projects go (`<dir>/Projects`); default is the repo root, like the Qt app.
    pub data_dir: Option<PathBuf>,
    pub repo: Option<PathBuf>,
    /// QA: start an import of these at launch (still shows the confirm bar unless `auto_confirm`).
    pub import_videos: Vec<PathBuf>,
    pub import_folders: Vec<PathBuf>,
    pub auto_confirm: bool,
    /// Preferences file; `None` = the user's default location. Tests point this at a temp file.
    pub settings_path: Option<PathBuf>,
}

pub struct App {
    opts: AppOptions,
    project: Option<Project>,
    path_input: String,
    load_error: Option<String>,
    selected: BTreeSet<usize>,
    /// Per-clip Board summary text, rebuilt on load only.
    summaries: Vec<String>,
    open: Option<usize>,
    viewer: Option<Viewer>,
    tab: Tab,
    node: Node,
    focus_mode: bool,
    props_sheet: bool,
    /// Per-clip (by folder) graph layout and view; kept across Back/open, not persisted.
    graphs: std::collections::BTreeMap<String, (graph::Graph, graph::View)>,
    graph_collapsed: bool,
    loc: OutputLocation,
    started: Instant,
    shot_requested: bool,
    last_clip_view: Option<usize>,
    inject_i: usize,
    inject_t: Instant,
    inject_reported: usize,
    inject_report: u8,
    /// Back clicked during this frame: applied after the workspace finishes rendering.
    back_requested: bool,
    back_rect: Option<egui::Rect>,
    header_rects: Vec<egui::Rect>,
    imp: ImportUi,
    pending_reload: Option<PathBuf>,
    qa_import: Option<Sources>,
    picker: Picker,
    /// A finished dialog waits here until the Board is showing and no import is running.
    held_pick: Option<(PickKind, Vec<PathBuf>)>,
    /// "Open clip settings" button rectangles from the last Board frame: top bar, then per-card (layout tests).
    open_rects: Vec<(Option<usize>, egui::Rect)>,
    /// Settings and Models pages replace the Board/workspace while open, which stay untouched underneath.
    settings: SettingsPage,
    settings_open: bool,
    models: super::models::Page,
    models_open: bool,
    settings_rect: Option<egui::Rect>,
    /// The Settings button was clicked this frame; handled once the screen has finished drawing.
    settings_requested: bool,
    settings_back_rect: Option<egui::Rect>,
    models_back_rect: Option<egui::Rect>,
    /// A clip an import asked to open while the page was showing.
    pending_open: Option<String>,
    ai: AiUi,
    close: CloseState,
    /// The user chose Quit anyway, or all graceful work has stopped: a close request now goes through.
    close_force: bool,
    close_rects: Vec<egui::Rect>,
    /// Output-location editor for the open project (Deliver tab).
    loc_note: Option<String>,
    /// (range, linear) whose save failed: not retried every frame, shown in the notice instead.
    persist_failed: Option<(Option<(usize, usize)>, bool)>,
    hint: hint::State,
    ws: ws::WsState,
    media: media::MediaState,
    last_ctx: Option<egui::Context>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: AppOptions) -> Self {
        let mut app = Self::with_ctx(&cc.egui_ctx, opts);
        app.models.start_background(&cc.egui_ctx);
        app
    }

    /// Window-free constructor (tests drive the same `frame` the window does).
    pub fn with_ctx(ctx: &egui::Context, opts: AppOptions) -> Self {
        Self::with_picker(ctx, opts, rfd_backend())
    }

    /// Same, with the file-dialog backend replaced (tests never open a real dialog).
    pub fn with_picker(ctx: &egui::Context, opts: AppOptions, dialogs: Backend) -> Self {
        super::theme::apply(ctx);
        let settings = SettingsPage::new(Store::load(
            opts.settings_path.clone().or_else(default_path),
        ));
        ctx.set_zoom_factor(settings.saved().interface.ui_scale);
        let settings_saved = settings.saved().clone();
        let models = super::models::Page::new(opts.repo.as_deref());
        // Runtime resolution (descriptor / CK_REPO / checkout discovery) happens once here; import
        // and AI launching re-resolve on demand, never from a build-time path.
        let launch = crate::runtime::launch(opts.repo.as_deref());
        let mut app = App {
            path_input: opts
                .project
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
            opts: opts.clone(),
            project: None,
            load_error: None,
            selected: BTreeSet::new(),
            summaries: vec![],
            open: None,
            viewer: None,
            tab: Tab::Media,
            node: Node::Media,
            focus_mode: false,
            props_sheet: false,
            graphs: Default::default(),
            graph_collapsed: false,
            // Never a machine-specific example path: `open_clip` fills in the clip's own default.
            loc: OutputLocation {
                root: PathBuf::new(),
                folder_template: "{name}_{version}/{output}".into(),
                file_template: "{name}_####".into(),
            },
            started: Instant::now(),
            shot_requested: false,
            last_clip_view: None,
            inject_i: 0,
            inject_t: Instant::now(),
            inject_reported: 0,
            inject_report: 0,
            back_requested: false,
            back_rect: None,
            header_rects: vec![],
            imp: ImportUi::new(
                opts.data_dir
                    .clone()
                    .or_else(|| crate::runtime::default_data_dir(&launch)),
                opts.repo.clone(),
            ),
            pending_reload: None,
            qa_import: (!opts.import_videos.is_empty() || !opts.import_folders.is_empty()).then(
                || Sources {
                    videos: opts.import_videos.clone(),
                    folders: opts.import_folders.clone(),
                },
            ),
            picker: Picker::new(dialogs),
            held_pick: None,
            open_rects: vec![],
            settings,
            settings_open: false,
            models,
            models_open: false,
            settings_rect: None,
            settings_requested: false,
            settings_back_rect: None,
            models_back_rect: None,
            pending_open: None,
            ai: AiUi::new(&settings_saved, opts.repo.as_deref()),
            close: CloseState::None,
            close_force: false,
            close_rects: vec![],
            loc_note: None,
            persist_failed: None,
            hint: Default::default(),
            ws: Default::default(),
            media: Default::default(),
            last_ctx: None,
        };
        if opts.project.is_some() {
            app.load(ctx);
            if let (Some(name), Some(p)) = (&opts.open_clip, &app.project) {
                if let Some(i) = p
                    .clips
                    .iter()
                    .position(|c| &c.folder == name || &c.name == name || &c.display_name == name)
                {
                    app.open_clip(ctx, i);
                }
            }
            if app.viewer.is_some() {
                if let Some(t) = opts.tab {
                    app.switch_tab(t);
                }
                let v = app.viewer.as_mut().unwrap();
                if let Some(vw) = opts.view {
                    v.view = vw;
                }
                if let Some(b) = opts.ab {
                    v.ab = true;
                    v.b_view = b;
                }
                if let Some(f) = opts.frame {
                    v.set_playhead(f);
                }
                if opts.play {
                    v.toggle_play(1);
                }
            }
        }
        app
    }

    fn load(&mut self, ctx: &egui::Context) {
        self.viewer = None;
        self.open = None;
        self.selected.clear();
        let lo = LoadOptions {
            global_output_dir: self.opts.global_output_dir.clone(),
        };
        match load_project(&PathBuf::from(self.path_input.trim()), &lo) {
            Ok(p) => {
                self.summaries = p.clips.iter().map(card_summary).collect();
                self.project = Some(p);
                self.load_error = None;
            }
            Err(e) => {
                self.summaries.clear();
                self.project = None;
                self.load_error = Some(e);
            }
        }
        // A reload of the same project keeps a waiting plan; switching projects forgets it (its "open
        // project" destination would otherwise still point at the old one).
        self.imp
            .project_changed(self.project.as_ref().map(|p| p.root.as_path()));
        ctx.request_repaint();
    }

    fn folder_of(&self, i: usize) -> Option<String> {
        self.project
            .as_ref()
            .and_then(|p| p.clips.get(i))
            .map(|c| c.folder.clone())
    }

    /// Reload from `path_input`; the Board selection follows clip folders, not list positions.
    pub fn reload_keep_selection(&mut self, ctx: &egui::Context) {
        let keep: BTreeSet<String> = self
            .selected
            .iter()
            .filter_map(|i| self.folder_of(*i))
            .collect();
        self.load(ctx);
        self.selected = self
            .project
            .as_ref()
            .map(|p| {
                p.clips
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| keep.contains(&c.folder))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
    }

    /// Select clips by folder name (tests / QA).
    pub fn select_folders(&mut self, folders: &[&str]) {
        self.selected = self
            .project
            .as_ref()
            .map(|p| {
                p.clips
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| folders.contains(&c.folder.as_str()))
                    .map(|(i, _)| i)
                    .collect()
            })
            .unwrap_or_default();
    }
    pub fn selected_folders(&self) -> Vec<String> {
        self.selected
            .iter()
            .filter_map(|i| self.folder_of(*i))
            .collect()
    }
    /// Cached Board summary of clip `i` (tests).
    pub fn card_summary_text(&self, i: usize) -> Option<&str> {
        self.summaries.get(i).map(String::as_str)
    }

    pub fn open_clip(&mut self, ctx: &egui::Context, i: usize) {
        let Some(clip) = self.project.as_ref().and_then(|p| p.clips.get(i)) else {
            return;
        };
        // Preferences apply from here on; an explicit --cache-mb beats the saved RAM budget.
        let st = self.settings.saved();
        let mut v =
            Viewer::new_with_workers(i, clip, st.cache_mb(self.opts.cache_mb), st.workers(), ctx);
        v.looping = st.playback.loop_default;
        self.viewer = Some(v);
        self.open = Some(i);
        let saved = self
            .project
            .as_ref()
            .and_then(|p| crate::workflow::read_location(p).ok().flatten());
        if let Some(l) = saved.or_else(|| self.clip().and_then(crate::workflow::default_location)) {
            self.loc = l;
        }
        self.tab = Tab::Media;
        self.node = Node::Media;
        self.focus_mode = false;
    }

    pub fn switch_tab(&mut self, t: Tab) {
        self.tab = t;
        self.node = match t {
            Tab::Media => Node::Media,
            Tab::Hint => Node::Hint,
            Tab::Key => Node::Key,
            Tab::Review => self.node,
            Tab::Deliver => {
                if matches!(self.node, Node::Out(_)) {
                    self.node
                } else {
                    Node::Out(View::Fg)
                }
            }
        };
        // Tabs choose a sensible default view for the shared viewer; the user can change it afterwards.
        if let Some(v) = &mut self.viewer {
            let want = match t {
                Tab::Media => Some(View::Input),
                Tab::Hint => Some(View::Hint),
                _ => None,
            };
            if let Some(w) = want.filter(|w| v.index.view_frames(*w) > 0) {
                v.view = w;
            }
        }
    }

    fn clip(&self) -> Option<&Clip> {
        self.open
            .and_then(|i| self.project.as_ref().and_then(|p| p.clips.get(i)))
    }

    // ------------------------------------------------------------------ keyboard
    fn keys(&mut self, ctx: &egui::Context) {
        // A focused graph node is a button, not text entry: shortcuts (F, B, 1-5, Esc…) must keep working there.
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.command);
        if self.models_open {
            if pressed(Key::Escape) {
                self.close_models();
            }
            return;
        }
        if self.settings_open {
            // The page owns the keyboard: no workspace shortcuts, Esc = Cancel.
            if pressed(Key::Escape) {
                self.close_settings();
            }
            return;
        }
        if self.viewer.is_none() {
            return;
        }
        if pressed(Key::B) {
            self.viewer = None;
            self.open = None;
            return;
        }
        for (k, t) in [
            (Key::Num1, Tab::Media),
            (Key::Num2, Tab::Hint),
            (Key::Num3, Tab::Key),
            (Key::Num4, Tab::Review),
            (Key::Num5, Tab::Deliver),
        ] {
            if pressed(k) {
                self.switch_tab(t);
            }
        }
        if pressed(Key::P) {
            self.props_sheet = !self.props_sheet;
        }
        if pressed(Key::G) {
            self.graph_collapsed = !self.graph_collapsed;
        }
        if pressed(Key::F) {
            self.focus_mode = !self.focus_mode;
        }
        if pressed(Key::Escape) {
            self.focus_mode = false;
            self.props_sheet = false;
        }
        // Shift read from the key event itself, so it also holds for synthetic (QA) events.
        let shift = ctx.input(|i| {
            i.events.iter().any(|e| {
                matches!(e, egui::Event::Key { pressed: true, modifiers, .. } if modifiers.shift)
            }) || i.modifiers.shift
        });
        let v = self.viewer.as_mut().unwrap();
        if pressed(Key::R) {
            v.retry_failed();
        }
        if pressed(Key::Space) {
            v.toggle_play(1);
        }
        {
            let d = if shift { 10 } else { 1 };
            if pressed(Key::ArrowLeft) {
                v.step(-d);
            }
            if pressed(Key::ArrowRight) {
                v.step(d);
            }
        }
        if pressed(Key::Home) {
            v.set_playhead(0);
        }
        if pressed(Key::End) {
            v.set_playhead(v.index.len().saturating_sub(1));
        }
        if pressed(Key::L) {
            v.toggle_loop();
        }
        if pressed(Key::Num0) {
            v.fit();
        }
        if pressed(Key::Equals) || pressed(Key::Plus) {
            v.zoom_by(1.25, Vec2::ZERO);
        }
        if pressed(Key::Minus) {
            v.zoom_by(0.8, Vec2::ZERO);
        }
        if pressed(Key::I) {
            v.set_in();
        }
        if pressed(Key::O) {
            v.set_out();
        }
    }

    // ------------------------------------------------------------------ board
    fn board(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        // Buttons stay off while an import runs or a dialog is already open (one at a time).
        let busy = self.imp.busy() || self.picker.pending().is_some();
        let why = if self.picker.pending().is_some() {
            "A file dialog is open"
        } else if self.imp.busy_what().contains("running") {
            "Import running"
        } else {
            "Checking the selected files"
        };
        let mut want: Option<PickKind> = None;
        self.open_rects.clear();
        let has_project = self.project.is_some();
        if !has_project {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("Bring in footage to start");
                ui.label(
                    RichText::new("Movies are copied into a new project and converted to EXR frames. Image sequences are used where they are. Your original files are never modified.")
                        .color(DIM),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 40.0).min(520.0);
                    ui.add_space(((ui.available_width() - w) / 2.0).max(0.0));
                    let big = |t: &str| egui::Button::new(RichText::new(t).size(16.0).strong()).min_size(Vec2::new(w / 3.0 - 6.0, 44.0));
                    if ui.add_enabled(!busy, big("Import clips…")).on_disabled_hover_text(why).clicked() {
                        want = Some(PickKind::Clips);
                    }
                    if ui.add_enabled(!busy, big("Import folder…")).on_disabled_hover_text(why).clicked() {
                        want = Some(PickKind::Folder);
                    }
                    if ui.add_enabled(!busy, big("Open project…")).on_disabled_hover_text(why).clicked() {
                        want = Some(PickKind::Project);
                    }
                });
                ui.label(RichText::new("You can also drop movie files or folders onto this window.").color(DIM).size(12.0));
                self.settings_button(ui);
            });
            ui.add_space(8.0);
        } else {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("Import clips…"))
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    want = Some(PickKind::Clips);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Import folder…"))
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    want = Some(PickKind::Folder);
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Open another project…"))
                    .on_disabled_hover_text(why)
                    .clicked()
                {
                    want = Some(PickKind::Project);
                }
                self.settings_button(ui);
            });
        }
        if let Some(k) = want {
            self.request_pick(&ctx, k);
        }
        self.imp.ui(ui);
        if let Some(k) = self.imp.dest_pick_requested.take() {
            self.request_pick(&ctx, k);
        }
        if let Some(e) = &self.load_error {
            ui.colored_label(Color32::from_rgb(235, 120, 120), e);
        }
        let Some(p) = &self.project else { return };
        let mut open_idx = None;
        let mut resume_idx = None;
        let mut sel = self.selected.clone();
        ui.horizontal(|ui| {
            ui.heading(&p.display_name);
            ui.label(
                RichText::new(format!(
                    "{} · {} clips · {}",
                    if p.is_v2 { "v2 project" } else { "v1 project" },
                    p.clips.len(),
                    p.root.display()
                ))
                .color(DIM),
            );
        });
        for n in &p.notes {
            ui.colored_label(WARN, n);
        }
        ui.horizontal(|ui| {
            let mut all = !p.clips.is_empty() && sel.len() == p.clips.len();
            if super::theme::checkbox(ui, &mut all, "All").changed() {
                sel = if all {
                    (0..p.clips.len()).collect()
                } else {
                    BTreeSet::new()
                };
            }
            ui.label(format!("{} selected", sel.len()));
            let one = sel.len() == 1;
            let ready = one
                && sel
                    .iter()
                    .next()
                    .and_then(|i| p.clips.get(*i))
                    .is_some_and(|c| matches!(self.imp.card_state(c), CardState::Idle));
            let r = ui
                .add_enabled(ready, egui::Button::new("Open clip settings"))
                .on_disabled_hover_text(if one {
                    "This clip has no frames yet"
                } else {
                    "Select exactly one clip"
                });
            self.open_rects.push((None, r.rect));
            if r.clicked() {
                open_idx = sel.iter().next().copied();
            }
            let picks: Vec<usize> = sel.iter().copied().collect();
            self.ai.batch_ui(ui, p, self.settings.saved(), &picks);
        });
        if let Some(n) = self.ai.notice.clone() {
            ui.colored_label(WARN, n);
        }
        if let Some(n) = self.ai.pending_note() {
            ui.colored_label(WARN, n);
        }
        self.ai.batch_review(ui);
        self.ai.unresolved_ui(ui);
        ui.separator();
        let total_w = ui.available_width();
        let queue_w = if total_w > 900.0 { 230.0 } else { 0.0 };
        ui.horizontal_top(|ui| {
            ui.allocate_ui_with_layout(
                Vec2::new(total_w - queue_w - 36.0, ui.available_height()),
                Layout::top_down(Align::Min),
                |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            for (i, c) in p.clips.iter().enumerate() {
                                // No overlay interaction on the frame: it would sit above the card's own buttons.
                                egui::Frame::group(ui.style())
                                    .inner_margin(8.0)
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        card(
                                            ui,
                                            i,
                                            c,
                                            &mut sel,
                                            &mut open_idx,
                                            self.imp.card_state(c),
                                            &mut resume_idx,
                                            self.summaries.get(i).map_or("", String::as_str),
                                            &mut self.open_rects,
                                        );
                                    });
                            }
                        });
                },
            );
            if queue_w > 0.0 {
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(queue_w);
                    ui.label(RichText::new("Queue").strong());
                    self.ai.queue_ui(ui);
                    self.ai.history_ui(ui);
                });
            }
        });
        self.selected = sel;
        if let Some(i) = resume_idx {
            if let Some(c) = p.clips.get(i) {
                let root = p.root.clone();
                self.imp.resume(&ctx, &root, c);
            }
        }
        if let Some(i) = open_idx {
            self.open_clip(&ctx, i);
        }
    }

    // ------------------------------------------------------------------ workspace
    fn workspace(&mut self, ui: &mut Ui) {
        let narrow = ui.available_width() < 900.0;
        let focus = self.focus_mode;
        let ctx = ui.ctx().clone();
        if self.open.is_none() {
            return;
        }
        if narrow && !self.props_sheet {
            self.ai_watch(&ctx);
        }

        let mut hr: Vec<egui::Rect> = vec![];
        Panel::top("header").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let back = ui.button("‹ Back to batch").on_hover_text("B");
                hr.push(back.rect);
                self.back_rect = Some(back.rect);
                if back.clicked() {
                    self.back_requested = true;
                }
                if let Some(c) = self.clip() {
                    ui.label(RichText::new(&c.display_name).strong());
                    ui.label(RichText::new(format!("· {}", c.state.label())).color(DIM));
                }
                ui.add_space(12.0);
                for t in Tab::ALL {
                    let r = ui.selectable_label(
                        self.tab == t,
                        format!("{} {}", t as usize + 1, t.label()),
                    );
                    hr.push(r.rect);
                    if r.clicked() {
                        self.switch_tab(t);
                    }
                }
                // One wrapped flow (no right-to-left sub-layout, which cannot wrap): controls drop to a second row
                // instead of overlapping the tabs on narrow windows.
                if narrow {
                    let r = ui.selectable_label(self.props_sheet, "Properties");
                    hr.push(r.rect);
                    if r.clicked() {
                        self.props_sheet = !self.props_sheet;
                    }
                }
                let r = ui.selectable_label(focus, "Focus (F)");
                hr.push(r.rect);
                if r.clicked() {
                    self.focus_mode = !self.focus_mode;
                }
            });
        });
        self.header_rects = hr;
        if let Some(t) = self.imp.notice_text().map(str::to_owned) {
            Panel::top("notice").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.colored_label(WARN, t);
                    if ui.button("Dismiss").clicked() {
                        self.imp.dismiss_notice();
                    }
                });
            });
        }
        if !focus {
            // Narrow: the primary action lives here (Properties is a closed sheet), disabled with its reason.
            Panel::bottom("queue")
                .exact_size(if narrow { 36.0 } else { 26.0 })
                .show(ui, |ui| {
                    ui.horizontal_centered(|ui| {
                        // Fixed-height bar: room for the global button without costing viewer height.
                        self.settings_button(ui);
                        if narrow && ui.button("Key clip").clicked() {
                            self.ai_quick_submit();
                        }
                        self.ai_bar_status(ui);
                    });
                });
            // Resizable graph panel; its max keeps the viewer at least MIN_VIEWER px tall (Hint image stays large).
            let tools_h = match (self.tab, narrow) {
                (Tab::Hint, false) => HINT_TOOLS_H,
                // Narrow: the viewer keeps its height; the action row is first, the rest scrolls.
                (Tab::Hint, true) => 100.0,
                _ => 78.0,
            };
            let max_h = (ui.available_height() - 34.0 - 26.0 - tools_h - MIN_VIEWER).max(GRAPH_MIN);
            if self.graph_collapsed {
                Panel::bottom("graph_collapsed")
                    .exact_size(GRAPH_HEADER)
                    .show(ui, |ui| self.graph_header(ui));
            } else {
                Panel::bottom("graph")
                    .resizable(true)
                    .default_size(GRAPH_DEFAULT)
                    .min_size(GRAPH_MIN)
                    .max_size(max_h)
                    .show(ui, |ui| {
                        self.graph_header(ui);
                        let h = ui.available_height();
                        self.graph(ui, h);
                    });
            }
            Panel::bottom("pagetools")
                .exact_size(tools_h)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| self.page_tools(ui));
                });
            if !narrow || self.props_sheet {
                Panel::right("props")
                    .default_size(310.0)
                    .min_size(240.0)
                    .resizable(true)
                    .show(ui, |ui| self.properties(ui));
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(v) = &mut self.viewer {
                // Keep repainting while playing: with all frames cached no decode completes to wake the UI.
                if v.tick() {
                    ctx.request_repaint();
                }
                v.ui(ui);
            }
        });
    }

    fn graph_header(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.set_height(GRAPH_HEADER - 8.0);
            ui.label(RichText::new("Node graph").strong());
            if ui
                .button(if self.graph_collapsed { "Show" } else { "Hide" })
                .on_hover_text("G")
                .clicked()
            {
                self.graph_collapsed = !self.graph_collapsed;
            }
            ui.label(
                RichText::new("drag to pan · wheel to zoom · double-click background to fit")
                    .color(DIM)
                    .size(12.0),
            );
        });
    }

    fn graph(&mut self, ui: &mut Ui, height: f32) {
        let (Some(clip), Some(v)) = (self.clip().cloned(), self.viewer.as_ref()) else {
            return;
        };
        let badge = |n: Node| -> String {
            match n {
                Node::Media => format!(
                    "{} · {} fr",
                    if v.linear { "Linear" } else { "sRGB" },
                    clip.input_files.len()
                ),
                Node::Hint => {
                    if clip.alpha_frames > 0 {
                        format!("{} hint frames", clip.alpha_frames)
                    } else {
                        "No hint yet".into()
                    }
                }
                Node::Key => clip
                    .state
                    .label()
                    .split(' ')
                    .next()
                    .unwrap_or("")
                    .to_string(),
                Node::Out(view) => {
                    let n = v.index.view_frames(view);
                    if n == 0 {
                        "none on disk".into()
                    } else {
                        format!("{n}/{} on disk", clip.input_files.len().max(n))
                    }
                }
            }
        };
        let dashed = |n: Node| match n {
            Node::Hint => clip.alpha_frames == 0,
            Node::Out(view) => v.index.view_frames(view) == 0,
            _ => false,
        };
        let (g, gv) = self
            .graphs
            .entry(clip.folder.clone())
            .or_insert_with(|| (graph::Graph::corridorkey(), graph::View::default()));
        let ev = graph::show(
            ui,
            g,
            gv,
            Some(to_kind(self.node)),
            |k| graph::NodeLook {
                badge: badge(from_kind(k)),
                dimmed: false,
                dashed: dashed(from_kind(k)),
            },
            height,
        );
        match ev {
            Some(graph::Event::Select(k)) => self.node = from_kind(k),
            Some(graph::Event::Open(k)) => {
                // Same path as clicking the tab, so the viewer's default view follows too.
                let n = from_kind(k);
                self.node = n;
                self.switch_tab(n.tab());
                self.node = n;
            }
            None => {}
        }
    }

    fn page_tools(&mut self, ui: &mut Ui) {
        let (Some(clip), Some(v)) = (self.clip().cloned(), self.viewer.as_ref()) else {
            return;
        };
        let (v_linear, v_len, v_range, v_range_text) =
            (v.linear, v.index.len(), v.range, v.range_text());
        let v_view_frames: Vec<(View, usize)> =
            [View::Fg, View::Matte, View::Comp, View::Processed]
                .into_iter()
                .map(|w| (w, v.index.view_frames(w)))
                .collect();
        match self.tab {
            Tab::Media => {
                ui.label(format!(
                    "Source: {}",
                    match &clip.source {
                        Source::Sequence { dir, external } => format!(
                            "{} frames in {}{}",
                            clip.input_files.len(),
                            dir.display(),
                            if *external {
                                " (external, not copied)"
                            } else {
                                ""
                            }
                        ),
                        Source::Video { path } => format!(
                            "video {} — native movie decode not integrated yet (staged)",
                            path.display()
                        ),
                        Source::Missing => "no source found".into(),
                    }
                ));
                if matches!(clip.source, Source::Missing) || clip.error.is_some() {
                    if ui.button("Relink missing footage…").on_hover_text("Choose the folder the image sequence now lives in; its frames must match the stored identity").clicked() {
                        let ctx = ui.ctx().clone();
                        self.request_pick(&ctx, PickKind::Relink);
                    }
                }
                ui.label(format!("Input colour: {} ({}). Change it in Properties > Media (saved in the project).", if v_linear { "Linear" } else { "sRGB" }, color_origin_text(&clip)));
            }
            Tab::Hint => {
                self.hint_tools(ui, &clip);
            }
            Tab::Key => {
                let n = v_range.map_or(v_len, |r| r.1 - r.0 + 1);
                ui.label(format!(
                    "Key will process {} of {} frames. {}",
                    n, v_len, v_range_text
                ));
                ui.label(RichText::new("Set options in Properties, then press Key clip (summary, blockers and target are in the Properties footer).").color(DIM).size(12.0));
            }
            Tab::Review => {
                ui.label("Playback of files already on disk. Missing frames show “No file on disk”; frames outside In/Out show “Outside In/Out”.");
                let line: Vec<String> = v_view_frames
                    .iter()
                    .map(|(w, n)| format!("{} {}/{}", w.label(), n, v_len))
                    .collect();
                ui.label(line.join("  ·  "));
            }
            Tab::Deliver => {
                ui.label(format!(
                    "Output folder: {}",
                    clip.output_dir
                        .as_ref()
                        .map_or("unknown".into(), |p| p.display().to_string())
                ));
                ui.label(RichText::new("Video export, presets and the output location are in Properties (right), export first.").color(DIM).size(12.0));
                ui.label(RichText::new(format!("Resolved from: {} · Legacy layout <output>/FG|Matte|Comp|Processed/<stem>.<ext>", clip.output_dir_note)).color(DIM).size(12.0));
            }
        }
    }

    fn properties(&mut self, ui: &mut Ui) {
        let range_frames_now = self.ai_open_range().ok().map(|(a, b)| b - a + 1);
        let (clip, session_o, node) = {
            let (Some(p), Some(c)) = (self.project.as_ref(), self.clip()) else {
                return;
            };
            (c.clone(), p.session_output_config.clone(), self.node)
        };
        ui.heading(format!("Properties: {}", node.title()));
        // Pinned footer first so it keeps its space.
        // The Key footer (summary, target, Key button) belongs to the Key page only; Media, Hint, Review and Deliver
        // need the panel's height for their own controls.
        if self.tab == Tab::Key {
            Panel::bottom("props_footer")
                .show_separator_line(true)
                .show(ui, |ui| {
                    self.key_footer(ui, &clip);
                });
        }
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            // Deliver: the video export is the primary action, first and always visible, whichever node is selected.
            if self.tab == Tab::Deliver {
                self.export_ui(ui, &clip);
                ui.separator();
            }
            match node {
            Node::Media => {
                let v = self.viewer.as_mut().unwrap();
                kv(ui, "Source", &match &clip.source {
                    Source::Sequence { dir, .. } => dir.display().to_string(),
                    Source::Video { path } => path.display().to_string(),
                    Source::Missing => "missing".into(),
                });
                kv(ui, "Frames", &format!("{} (timeline {})", clip.input_files.len(), v.index.len()));
                kv(ui, "Frame rate", &format!("{:.3} fps{}", clip.fps, if clip.fps_assumed { " (assumed, no metadata)" } else { "" }));
                if let Some((w, h)) = clip.sample_size {
                    kv(ui, "Size", &format!("{w}×{h}"));
                    kv(ui, "Resolution label", &resolution_label(w, h));
                }
                ui.separator();
                ui.label(RichText::new("Input colour").strong());
                let (mut lin, stored) = (v.linear, v.linear_stored);
                ui.radio_value(&mut lin, false, "sRGB (display-encoded)");
                ui.radio_value(&mut lin, true, "Linear");
                if lin != v.linear {
                    v.linear = lin;
                }
                ui.label(RichText::new(format!("Stored: {} ({})", if stored { "Linear" } else { "sRGB" }, color_origin_text(&clip))).color(DIM).size(12.0));
                if v.linear != stored {
                    ui.colored_label(WARN, "Saving the new colour interpretation…");
                }
                self.media_props(ui, &clip);
            }
            Node::Hint => {
                kv(ui, "Hint frames", &clip.alpha_frames.to_string());
                kv(ui, "Mask hint frames", &clip.mask_frames.to_string());
                kv(ui, "Keyframe marks", &if clip.keyframes.is_empty() { "none".into() } else { format!("{:?}", clip.keyframes) });
                ui.separator();
                self.hint_props(ui, &clip);
                ui.separator();
                if let Some(p) = &self.project {
                    self.ai.hint_controls(ui, &clip, p, self.settings.saved());
                }
            }
            Node::Key => {
                if let Some(p) = &self.project {
                    let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
                    let range_frames = range_frames_now;
                    let st = self.settings.saved();
                    self.ai.key_controls(ui, &clip, p, st, lin, range_frames);
                    ui.separator();
                    ui.label(RichText::new("Outputs").strong());
                    self.ai.output_controls(ui, &clip, p, st, None);
                }
                ui.separator();
                self.preset_ui(ui, &clip, false);
                if let Some(m) = &clip.manifest {
                    ui.separator();
                    ui.label(RichText::new("Last run (manifest)").strong());
                    kv(ui, "Engine", &m.engine);
                    kv(ui, "Complete", &m.complete.to_string());
                    kv(ui, "Partial range", &m.partial_range.to_string());
                    if m.raw.get("exr_lossy").and_then(Value::as_bool) == Some(true) {
                        kv(ui, "EXR", &format!("lossy {} ({})", m.exr_compression.as_deref().unwrap_or("?"), m.raw.get("exr_pixel_type").and_then(Value::as_str).unwrap_or("?")));
                    }
                    if let Some(r) = clip.result_range_text() {
                        kv(ui, "Result covers", &r);
                    }
                    if let Some(d) = &clip.manifest_dir {
                        kv(ui, "Result folder", &d.display().to_string());
                    }
                }
            }
            Node::Out(view) => {
                let v = self.viewer.as_ref().unwrap();
                let key = match view { View::Fg => "fg", View::Matte => "matte", View::Comp => "comp", _ => "processed" };
                let sess_en = session_o.as_ref().and_then(|o| o.get(format!("{key}_enabled"))).and_then(Value::as_bool);
                let sess_fmt = session_o.as_ref().and_then(|o| o.get(format!("{key}_format"))).and_then(Value::as_str);
                let kind = match view { View::Fg => OutputKind::Fg, View::Matte => OutputKind::Matte, View::Comp => OutputKind::Comp, _ => OutputKind::Processed };
                kv(ui, "On disk", &format!("{} of {} frames", v.index.view_frames(view), v.index.len()));
                kv(ui, "Folder", &clip.view_dir(view).map_or("unknown".into(), |p| p.display().to_string()));
                if let Some(m) = &clip.manifest {
                    kv(ui, "Last run enabled", &m.enabled.contains(&key.to_string()).to_string());
                    kv(ui, "Last run format", m.formats.get(key).map_or("?", |s| s.as_str()));
                    if view == View::Processed {
                        kv(ui, "Alpha convention", m.processed_exr_convention.as_deref().unwrap_or("unknown"));
                    }
                }
                kv(ui, "Session: enabled", &sess_en.map_or("?".into(), |b| b.to_string()));
                kv(ui, "Session: format", sess_fmt.unwrap_or("?"));
                if let Some(Availability::Present(p)) = (0..v.index.len()).map(|i| v.index.availability(view, i, None)).find(|a| matches!(a, Availability::Present(_))) {
                    if ext_of(&p) == "exr" {
                        kv(ui, "First file header", &exr_header_summary(&p).unwrap_or_else(|e| e));
                    }
                }
                let comp = session_o.as_ref().and_then(|o| o.get("exr_compression")).and_then(Value::as_str);
                kv(ui, "EXR compression", &format!("{} — shared by all EXR exports (session)", comp.unwrap_or("?")));
                ui.label(RichText::new("The compression in each file's header (above) is authoritative for that file.").color(DIM).size(12.0));
                if let Some(p) = &self.project {
                    ui.separator();
                    ui.label(RichText::new("Next run").strong());
                    self.ai.output_controls(ui, &clip, p, self.settings.saved(), Some(kind));
                }
                if self.tab == Tab::Deliver {
                    ui.separator();
                    self.location_preview(ui, &clip);
                }
            }
            }
        });
    }

    /// Output location editor: templates are saved to `project.json` and used by every Key job of this project.
    fn location_preview(&mut self, ui: &mut Ui, clip: &Clip) {
        ui.label(RichText::new("Output location").strong());
        let saved = self
            .project
            .as_ref()
            .is_some_and(|p| p.output_location.as_ref().is_some_and(|v| !v.is_null()));
        ui.label(
            RichText::new(if saved { "Saved in project.json: new jobs use it." } else { "Default (not saved): <this clip's output folder>/{name}_{version}/{output}/{name}_####. Earlier results stay where they are." })
                .color(DIM)
                .size(12.0),
        );
        let mut root = self.loc.root.display().to_string();
        ui.label("Root (a folder on the shared storage; absolute)");
        ui.text_edit_singleline(&mut root);
        self.loc.root = root.into();
        ui.label("Folder template");
        ui.text_edit_singleline(&mut self.loc.folder_template);
        ui.label("File template");
        ui.text_edit_singleline(&mut self.loc.file_template);
        let (w, h) = clip.sample_size.unwrap_or((0, 0));
        let sv = |k: &str| {
            clip.output_values
                .as_ref()
                .and_then(|v| v.get(k))
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let vals = ClipOutputValues {
            project: self
                .project
                .as_ref()
                .map(|p| p.display_name.clone())
                .unwrap_or_default(),
            name: clip.name.clone(),
            resolution: sv("resolution").unwrap_or_else(|| resolution_label(w, h)),
            date: sv("date").unwrap_or_else(crate::workflow::today),
            version: clip
                .output_values
                .as_ref()
                .and_then(|v| v.get("version"))
                .and_then(Value::as_u64)
                .map_or(1, |v| v as u32),
        };
        let kinds = self
            .ai
            .draft(clip, self.project.as_ref().unwrap(), self.settings.saved())
            .out
            .kinds();
        let frame = self
            .viewer
            .as_ref()
            .map_or(0, |v| v.index.frame_number(v.playhead));
        match kinds
            .first()
            .map(|k| resolve_path(&self.loc, &vals, k.0, kinds.len(), frame, k.1))
        {
            Some(Ok(p)) => ui.label(RichText::new(display(&p)).monospace().size(12.0)),
            Some(Err(e)) => ui.colored_label(Color32::from_rgb(225, 110, 110), format!("{e:?}")),
            None => ui.colored_label(WARN, "No output enabled"),
        };
        ui.label(RichText::new("{resolution} is a label only; nothing is resized. All machines write to this same storage.").color(DIM).size(12.0));
        let root_dir = self.project.as_ref().map(|p| p.root.clone());
        ui.horizontal_wrapped(|ui| {
            if ui.button("Save output location").clicked() {
                if let Some(r) = &root_dir {
                    self.loc_note = Some(match crate::workflow::save_location(r, &self.loc) {
                        Ok(v) => {
                            if let Some(p) = &mut self.project {
                                p.output_location = Some(v);
                            }
                            "Saved to project.json".to_string()
                        }
                        Err(e) => format!("Not saved: {e}"),
                    });
                }
            }
            if saved && ui.button("Reset to default").clicked() {
                if let Some(r) = &root_dir {
                    self.loc_note = Some(match crate::workflow::clear_location(r) {
                        Ok(()) => {
                            if let Some(p) = &mut self.project {
                                p.output_location = None;
                            }
                            if let Some(l) = crate::workflow::default_location(clip) {
                                self.loc = l;
                            }
                            "Default location restored".to_string()
                        }
                        Err(e) => format!("Not saved: {e}"),
                    });
                }
            }
        });
        if let Some(n) = &self.loc_note {
            ui.label(RichText::new(n).color(DIM).size(12.0));
        }
    }

    // ------------------------------------------------------------------ AI
    /// Inclusive source-frame range the open viewer would key (whole clip when no In/Out is set).
    fn ai_open_range(&self) -> Result<(usize, usize), String> {
        let (Some(c), Some(v)) = (self.clip(), self.viewer.as_ref()) else {
            return Err("No clip open".into());
        };
        if v.range.is_none() {
            return match c.input_files.len() {
                0 => Err("No source frames".into()),
                n => Ok((0, n - 1)),
            };
        }
        let (a, b) = v.bounds();
        let at = |t: usize| {
            v.index
                .stems
                .get(t)
                .and_then(|s| c.input_files.iter().position(|f| &f.0 == s))
        };
        match (at(a), at(b)) {
            (Some(a), Some(b)) => Ok((a, b)),
            _ => Err("An In/Out edge is not a source frame: re-set In/Out".into()),
        }
    }

    fn key_footer(&mut self, ui: &mut Ui, clip: &Clip) {
        let range = self.ai_open_range();
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        if let Some(p) = &self.project {
            self.ai
                .key_panel(ui, clip, p, self.settings.saved(), range, lin);
        }
    }

    /// Keep the gate for the open clip fresh while the Properties panel (which draws it) is closed.
    fn ai_watch(&mut self, ctx: &egui::Context) {
        let (range, lin) = (
            self.ai_open_range(),
            self.viewer.as_ref().is_some_and(|v| v.linear),
        );
        let (Some(p), Some(i)) = (self.project.as_ref(), self.open) else {
            return;
        };
        self.ai
            .watch_gate(ctx, &p.clips[i], p, self.settings.saved(), range, lin);
    }

    /// Narrow-layout "Key clip": queues the job the gate froze for the current range.
    fn ai_quick_submit(&mut self) {
        let Some(c) = self.clip().cloned() else {
            return;
        };
        self.ai.notice = self
            .ai
            .submit_current(&c, crate::workflow::Intent::New)
            .err()
            .map(|e| format!("Not queued: {e}"));
    }

    fn ai_bar_status(&mut self, ui: &mut Ui) {
        let folder = self.clip().map(|c| c.folder.clone());
        match (&self.ai.notice, folder) {
            (Some(n), _) => {
                ui.label(RichText::new(n).color(WARN).size(12.0));
            }
            (None, Some(f)) => self.ai.job_status(ui, &f),
            _ => {}
        }
    }

    /// In/Out and input colour are saved as soon as they change (same keys the Qt app reads). A failed save
    /// leaves the edit visible as unsaved and is reported once per distinct value, never silently dropped.
    fn persist_media(&mut self) {
        let (Some(i), Some(p), Some(v)) = (self.open, self.project.as_mut(), self.viewer.as_mut())
        else {
            return;
        };
        if !v.range_dirty() && v.linear == v.linear_stored {
            return;
        }
        let sig = (v.range, v.linear);
        if self.persist_failed == Some(sig) {
            return;
        }
        let clip = p.clips[i].clone();
        let mut err = None;
        if v.range_dirty() {
            // Timeline indices -> indices into the clip's own source frames.
            let at = |t: usize| {
                v.index
                    .stems
                    .get(t)
                    .and_then(|s| clip.input_files.iter().position(|f| &f.0 == s))
            };
            let src = match v.range {
                None => Ok(None),
                Some((a, b)) => match (at(a), at(b)) {
                    (Some(a), Some(b)) => Ok(Some((a, b))),
                    _ => Err("an In/Out edge is not a source frame".to_string()),
                },
            };
            let stems: Vec<String> = clip.input_files.iter().map(|f| f.0.clone()).collect();
            match src.and_then(|r| crate::persist::save_range(&clip, r, &stems).map(|()| r)) {
                Ok(r) => {
                    v.range_stored = v.range;
                    p.clips[i].in_out = r;
                }
                Err(e) => err = Some(format!("In/Out not saved: {e}")),
            }
        }
        if v.linear != v.linear_stored {
            match crate::persist::save_colour(&p.root, &clip.name, v.linear) {
                Ok(()) => {
                    v.linear_stored = v.linear;
                    p.clips[i].input_is_linear = v.linear;
                    p.clips[i].color_origin = crate::project::ColorOrigin::SessionOverride;
                }
                Err(e) => err = Some(format!("Colour not saved: {e}")),
            }
        }
        self.persist_failed = err.as_ref().map(|_| sig);
        if err.is_some() {
            self.ai.notice = err;
        }
    }

    /// Poll the scheduler; re-read the LOCAL manifest of any clip a job published to or finished.
    fn ai_tick(&mut self, ctx: &egui::Context) {
        for r in self.ai.tick() {
            self.refresh_clip(ctx, &r.folder);
        }
        if self.ai.active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Active work cancels the close request; graceful stop keeps the window updating until it exits.
    fn close_tick(&mut self, ctx: &egui::Context) {
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_force {
            let (jobs, alive, models) = (
                self.ai.running_jobs(),
                self.ai.workers_alive(),
                self.models.busy(),
            );
            if jobs || alive || models {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.close == CloseState::None {
                    if jobs || models {
                        self.close = CloseState::Prompt;
                    } else {
                        self.ai.begin_stop(); // only idle workers: no question to ask
                        self.close = CloseState::Stopping;
                    }
                }
            }
        }
        if self.close == CloseState::Stopping && self.ai.all_stopped() && !self.models.busy() {
            self.close_force = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn close_ui(&mut self, ctx: &egui::Context) {
        self.close_rects.clear();
        if self.close == CloseState::None {
            return;
        }
        let mut next = self.close;
        let (mut stop, mut quit) = (false, false);
        let mut rects = vec![];
        egui::Window::new("Work is running")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| match self.close {
                CloseState::Prompt => {
                    if self.ai.running_jobs() {
                        ui.label("AI jobs are still running. Stopping them gracefully keeps frames already written so a run can be resumed.");
                    }
                    if self.models.busy() {
                        ui.label(self.models.close_status());
                    }
                    ui.horizontal_wrapped(|ui| {
                        let a = ui.button("Stop work, then quit");
                        let b = ui.button("Keep working");
                        let c = ui.button("Quit anyway");
                        rects.extend([a.rect, b.rect, c.rect]);
                        stop = a.clicked();
                        if b.clicked() {
                            next = CloseState::None;
                        }
                        quit = c.clicked();
                    });
                    ui.label(RichText::new("Quit anyway may leave workers or the model helper running; the UI will no longer show their final result.").color(DIM).size(12.0));
                }
                _ => {
                    let lines = self.ai.stopping_lines();
                    ui.label(format!("Stopping work… ({} target(s))", lines.len()));
                    for l in lines {
                        ui.label(RichText::new(l).color(DIM).size(12.0));
                    }
                    if self.models.busy() { ui.label(self.models.close_status()); }
                    let c = ui.button("Quit anyway");
                    rects.push(c.rect);
                    quit = c.clicked();
                    ui.label(RichText::new("The window closes when workers and the helper have exited. Quit anyway may leave the helper running and hide its final result.").color(DIM).size(12.0));
                }
            });
        self.close_rects = rects;
        if stop {
            if self.models.busy() {
                self.models.stop_for_close(ctx);
            }
            self.ai.begin_stop();
            next = CloseState::Stopping;
        }
        self.close = next;
        if quit {
            self.close_force = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    pub fn close_state(&self) -> CloseState {
        self.close
    }
    /// Close-window button rectangles from the last frame: Prompt = [stop, keep, quit]; Stopping = [quit].
    pub fn close_rects(&self) -> &[egui::Rect] {
        &self.close_rects
    }

    /// Re-read one clip from disk and adopt it in the Board and (keeping playhead, range, zoom, A/B) the viewer.
    pub fn refresh_clip(&mut self, ctx: &egui::Context, folder: &str) {
        let Some(p) = &mut self.project else { return };
        if p.reload_clip(folder).is_err() {
            return;
        }
        let Some(i) = p.clips.iter().position(|c| c.folder == folder) else {
            return;
        };
        if let Some(s) = self.summaries.get_mut(i) {
            *s = card_summary(&p.clips[i]);
        }
        if let (Some(o), Some(v)) = (self.open, &mut self.viewer) {
            if o == i {
                v.refresh(&p.clips[i]);
            }
        }
        ctx.request_repaint();
    }

    /// The AI panel (tests / QA).
    pub fn ai(&self) -> &AiUi {
        &self.ai
    }
    pub fn ai_mut(&mut self) -> &mut AiUi {
        &mut self.ai
    }
    /// Test entry for the Board's batch button.
    pub fn key_selected(&mut self, ctx: &egui::Context) {
        let picks: Vec<usize> = self.selected.iter().copied().collect();
        if let Some(p) = &self.project {
            self.ai.begin_batch(ctx, p, self.settings.saved(), &picks);
        }
    }
}

fn color_origin_text(c: &Clip) -> &'static str {
    match c.color_origin {
        ColorOrigin::SessionOverride => "from session override",
        ColorOrigin::DefaultLinearExr => "default for EXR sequence",
        ColorOrigin::VideoMetadataLinear => "video metadata says linear",
        ColorOrigin::DefaultNotLinear => "default",
    }
}

fn kv(ui: &mut Ui, k: &str, v: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(format!("{k}:")).color(DIM));
        ui.label(v);
    });
}

fn card_summary(c: &Clip) -> String {
    let idx = FrameIndex::build(c);
    let range = match c.in_out {
        Some((a, b)) if b >= a && b < c.input_files.len() => format!(
            "Range {}–{} · {} of {} frames",
            idx.frame_number(idx.index_of(&c.input_files[a].0).unwrap_or(a)),
            idx.frame_number(idx.index_of(&c.input_files[b].0).unwrap_or(b)),
            b - a + 1,
            c.input_files.len()
        ),
        _ => format!("Range: whole clip · {} frames", c.input_files.len()),
    };
    let outs: Vec<String> = [View::Fg, View::Matte, View::Comp, View::Processed]
        .into_iter()
        .filter(|v| idx.view_frames(*v) > 0)
        .map(|v| format!("{} {}", v.label(), idx.view_frames(v)))
        .collect();
    format!(
        "{} · {:.3} fps{} · {} · outputs on disk: {}",
        range,
        c.fps,
        if c.fps_assumed { " (assumed)" } else { "" },
        if c.input_is_linear { "Linear" } else { "sRGB" },
        if outs.is_empty() {
            "none".into()
        } else {
            outs.join(", ")
        }
    )
}

#[allow(clippy::too_many_arguments)]
fn card(
    ui: &mut Ui,
    i: usize,
    c: &Clip,
    sel: &mut BTreeSet<usize>,
    open: &mut Option<usize>,
    st: CardState,
    resume: &mut Option<usize>,
    summary: &str,
    open_rects: &mut Vec<(Option<usize>, egui::Rect)>,
) {
    ui.horizontal(|ui| {
        let mut on = sel.contains(&i);
        if super::theme::checkbox(ui, &mut on, "").changed() {
            if on {
                sel.insert(i);
            } else {
                sel.remove(&i);
            }
        }
        let idle = matches!(st, CardState::Idle);
        let name = ui.add(
            egui::Label::new(RichText::new(&c.display_name).strong().size(15.0))
                .sense(Sense::click()),
        );
        if idle && name.on_hover_text("Double-click to open").double_clicked() {
            *open = Some(i);
        }
        ui.label(RichText::new(c.state.label()).color(Color32::from_rgb(150, 190, 150)));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let r = ui
                .add_enabled(idle, egui::Button::new("Open clip settings"))
                .on_disabled_hover_text("Frames are not ready yet");
            open_rects.push((Some(i), r.rect));
            if r.clicked() {
                *open = Some(i);
            }
            if matches!(st, CardState::NeedsFrames | CardState::Failed(_))
                && ui.button("Resume extraction").clicked()
            {
                *resume = Some(i);
            }
        });
    });
    match &st {
        CardState::Busy(t, f) => {
            ui.add(egui::ProgressBar::new(*f).text(t.as_str()));
        }
        CardState::Waiting => {
            ui.label(RichText::new("Waiting for its turn to extract…").color(WARN));
        }
        CardState::Failed(m) => {
            ui.colored_label(
                Color32::from_rgb(235, 120, 120),
                format!("Extraction failed: {m}"),
            );
        }
        CardState::NeedsFrames => {
            ui.label(RichText::new("Frames not extracted yet (import stopped or failed). The source movie is safe in this clip's Source folder.").color(WARN));
        }
        CardState::Idle => {}
    }
    if !matches!(st, CardState::Idle) {
        // No frames yet: readiness/range/fps lines would be misleading placeholders.
        return;
    }
    ui.label(RichText::new(readiness(c)).color(DIM));
    ui.label(summary);
    for n in &c.notes {
        ui.colored_label(WARN, n);
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        use egui::{Event, Modifiers, PointerButton, Pos2};
        if self.inject_i >= self.opts.keys.len()
            || self.started.elapsed().as_secs_f32() < 2.5
            || self.inject_t.elapsed().as_millis() < 400
        {
            return;
        }
        let tok = self.opts.keys[self.inject_i].clone();
        self.inject_i += 1;
        self.inject_t = Instant::now();
        self.inject_report = 3;
        if let Some(xy) = tok.strip_prefix("click:") {
            let (x, y) = xy.split_once('x').unwrap_or(("0", "0"));
            let pos = Pos2::new(x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
            raw.events.push(Event::PointerMoved(pos));
            for pressed in [true, false] {
                raw.events.push(Event::PointerButton {
                    pos,
                    button: PointerButton::Primary,
                    pressed,
                    modifiers: Modifiers::NONE,
                });
            }
            return;
        }
        let (mods, name) = match tok.strip_prefix("Shift+") {
            Some(n) => (Modifiers::SHIFT, n),
            None => (Modifiers::NONE, tok.as_str()),
        };
        let Some(key) = Key::from_name(name) else {
            return;
        };
        for pressed in [true, false] {
            raw.events.push(Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: mods,
            });
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame(ui);
    }
}

impl App {
    fn import_tick(&mut self, ctx: &egui::Context) {
        let out = self.imp.poll(ctx);
        if let Some(p) = out.reload {
            self.pending_reload = Some(p);
        }
        if let Some(r) = self.picker.poll() {
            self.held_pick = Some(r);
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        // Settings and Models have no import target: refuse, and show the notice after leaving the page.
        let dropped = if (self.settings_open || self.models_open) && !dropped.is_empty() {
            let page = if self.models_open {
                "Models"
            } else {
                "Settings"
            };
            self.imp.notify(&format!(
                "Dropped files were not imported: leave {page} and drop them again."
            ));
            vec![]
        } else {
            dropped
        };
        // Clip-level dialogs (relink, hint movie/sequence, export folder) answer inside the open clip.
        if self.in_workspace()
            && !self.settings_open
            && matches!(
                self.held_pick.as_ref().map(|h| h.0),
                Some(
                    PickKind::HintMovie
                        | PickKind::ExportFolder
                        | PickKind::Relink
                        | PickKind::RelinkMovie
                        | PickKind::HintSequence
                )
            )
        {
            self.apply_pick(ctx);
        }
        if self.in_workspace() {
            // Never leave or replace the open clip for a drop.
            if !dropped.is_empty() {
                self.imp.notify(
                    "Dropped files were not imported: go back to the batch to import them.",
                );
            }
        } else {
            if !self.settings_open {
                self.apply_pick(ctx);
            }
            if let Some(p) = self.pending_reload.take() {
                self.path_input = p.display().to_string();
                self.reload_keep_selection(ctx);
            }
            if let Some(name) = out.open_clip {
                self.pending_open = Some(name);
            }
            if !self.settings_open {
                if let Some(name) = self.pending_open.take() {
                    let i = self
                        .project
                        .as_ref()
                        .and_then(|p| p.clips.iter().position(|c| c.folder == name));
                    if let Some(i) = i {
                        self.open_clip(ctx, i);
                    }
                }
            }
            if let Some(src) = self.qa_import.take() {
                let root = self.project.as_ref().map(|p| p.root.clone());
                self.imp.begin_qa(ctx, root.as_deref(), src);
            }
            if !dropped.is_empty() {
                if self.picker.pending().is_some() {
                    self.imp
                        .notify("Dropped files were not imported: a file dialog is open. Close it and drop them again.");
                } else if self.imp.busy() {
                    let why = format!(
                        "Dropped files were not imported: {}. Drop them again when it finishes.",
                        self.imp.busy_what()
                    );
                    self.imp.notify(&why);
                } else {
                    let (folders, videos): (Vec<_>, Vec<_>) =
                        dropped.into_iter().partition(|p| p.is_dir());
                    let root = self.project.as_ref().map(|p| p.root.clone());
                    self.imp
                        .begin(ctx, root.as_deref(), Sources { videos, folders });
                }
            }
            self.imp.hold = self.dialog_active();
            if self.opts.auto_confirm {
                self.imp.confirm_now(ctx);
            }
        }
        self.imp.hold = self.dialog_active();
    }

    /// A file dialog is open, or its answer is held and not applied yet.
    fn dialog_active(&self) -> bool {
        self.picker.pending().is_some() || self.held_pick.is_some()
    }

    /// Open a native dialog off the UI thread. False when one is already open or an import is running.
    pub fn request_pick(&mut self, ctx: &egui::Context, kind: PickKind) -> bool {
        let dest = matches!(kind, PickKind::Location | PickKind::AddTo);
        // A destination is only chosen for a waiting plan; other dialogs never open while an import is busy.
        if (dest && !self.imp.has_pending()) || (!dest && self.imp.busy()) {
            return false;
        }
        let dir = (kind == PickKind::Location)
            .then(|| self.imp.data_dir.clone())
            .flatten();
        let c = ctx.clone();
        let started = self.picker.start(kind, dir, move || c.request_repaint());
        self.imp.hold |= started;
        started
    }

    pub fn viewer(&self) -> Option<&Viewer> {
        self.viewer.as_ref()
    }
    pub fn project_root(&self) -> Option<&std::path::Path> {
        self.project.as_ref().map(|p| p.root.as_path())
    }

    /// The one global Settings button, shown in the Board and the workspace header (not per clip).
    fn settings_button(&mut self, ui: &mut Ui) -> egui::Rect {
        let r = ui
            .add_enabled(
                !self.settings_open && !self.models_open,
                egui::Button::new("Settings"),
            )
            .on_hover_text("Preferences: playback, interface, defaults");
        self.settings_rect = Some(r.rect);
        self.settings_requested |= r.clicked();
        r.rect
    }
    fn open_settings(&mut self) {
        if let Some(v) = &mut self.viewer {
            v.pause();
        }
        self.settings.open();
        self.settings_open = true;
    }
    /// Leave the page, discarding the draft; clip, range and playhead were never touched.
    fn close_settings(&mut self) {
        self.settings.cancel();
        self.settings_open = false;
    }
    fn close_models(&mut self) {
        self.models_open = false;
    }
    /// An import is probing, waiting for confirmation or running (tests).
    pub fn import_active(&self) -> bool {
        self.imp.busy() || self.imp.has_pending()
    }
    /// Mutate the open viewer (tests).
    pub fn viewer_mut_for_test(&mut self, f: impl FnOnce(&mut Viewer)) {
        if let Some(v) = &mut self.viewer {
            f(v);
        }
    }
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }
    pub fn settings_page(&mut self) -> &mut SettingsPage {
        &mut self.settings
    }
    /// Global "Settings" button (top bar) and the page's own Back button, from the last frame.
    pub fn settings_rect(&self) -> Option<egui::Rect> {
        self.settings_rect
    }
    pub fn settings_back_rect(&self) -> Option<egui::Rect> {
        self.settings_back_rect
    }
    pub fn models_back_rect(&self) -> Option<egui::Rect> {
        self.models_back_rect
    }

    /// True while a file dialog is open (tests).
    pub fn picking(&self) -> bool {
        self.picker.pending().is_some()
    }

    /// Use a finished dialog's result on the Board; a result that arrives while an import runs is refused, not queued.
    fn apply_pick(&mut self, ctx: &egui::Context) {
        let Some((kind, paths)) = self.held_pick.take() else {
            return;
        };
        if paths.is_empty() {
            return;
        }
        let dest = matches!(kind, PickKind::Location | PickKind::AddTo);
        if self.imp.busy() && !dest {
            let why = format!(
                "The file you picked was ignored because {}. Pick again when it finishes.",
                self.imp.busy_what()
            );
            self.imp.notify(&why);
            return;
        }
        let root = self.project.as_ref().map(|p| p.root.clone());
        match kind {
            PickKind::Clips => self.imp.begin(
                ctx,
                root.as_deref(),
                Sources {
                    videos: paths,
                    folders: vec![],
                },
            ),
            PickKind::Folder => self.imp.begin(
                ctx,
                root.as_deref(),
                Sources {
                    videos: vec![],
                    folders: paths,
                },
            ),
            PickKind::Project => {
                self.path_input = paths[0].display().to_string();
                self.load(ctx);
            }
            PickKind::Location => {
                self.imp.set_dest(ctx, Dest::NewIn(paths[0].clone()));
            }
            PickKind::AddTo => {
                self.imp.set_dest(ctx, Dest::Add(paths[0].clone()));
            }
            PickKind::HintMovie => self.hint_picked(paths[0].clone(), true),
            PickKind::ExportFolder => self.hint_picked(paths[0].clone(), false),
            PickKind::Relink => self.relink_picked(ctx, paths[0].clone()),
            PickKind::RelinkMovie => self.movie_picked(paths[0].clone()),
            PickKind::HintSequence => self.sequence_picked(paths[0].clone()),
        }
    }

    /// "Open clip settings" button rectangles from the last Board frame: `None` = the top bar, `Some(i)` = card `i`.
    pub fn open_settings_rects(&self) -> &[(Option<usize>, egui::Rect)] {
        &self.open_rects
    }

    /// True while a clip workspace (not the Board) is shown.
    /// Visible import/drop notice text (tests).
    pub fn notice(&self) -> Option<&str> {
        self.imp.notice_text()
    }
    pub fn in_workspace(&self) -> bool {
        self.viewer.is_some() && self.open.is_some()
    }
    /// Source-frame count of the open clip (tests).
    pub fn open_frame_count(&self) -> Option<usize> {
        self.clip().map(|c| c.input_files.len())
    }
    /// Rectangles of the workspace header controls from the last frame (layout tests).
    pub fn header_rects(&self) -> &[egui::Rect] {
        &self.header_rects
    }
    /// Graph canvas and node rectangles of the open clip from the last frame (layout tests).
    pub fn graph_rects(&self) -> Option<(egui::Rect, Vec<egui::Rect>)> {
        let f = &self.clip()?.folder;
        self.graphs
            .get(f)
            .map(|(_, v)| (v.canvas, v.node_rects.clone()))
    }
    pub fn viewer_height_hint(&self) -> Option<f32> {
        self.viewer.as_ref().map(|v| v.draw_h)
    }
    pub fn focus_mode(&self) -> bool {
        self.focus_mode
    }
    /// Screen rectangle of the "Back to batch" button from the last workspace frame.
    pub fn back_rect(&self) -> Option<egui::Rect> {
        self.back_rect
    }
    pub fn frame(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.import_tick(&ctx);
        self.ai
            .set_model_mutation_active(self.models.blocks_worker_launch());
        self.ai_tick(&ctx);
        self.last_ctx = Some(ctx.clone());
        self.persist_media();
        self.hint_sync();
        self.media_poll(&ctx);
        self.workspace_sync();
        self.ai
            .queue_open(self.project.as_ref().map(|p| p.root.as_path()));
        self.ai.queue_sync();
        if let Some(f) = self.ai.tk.requeue.take() {
            if let Some(i) = self
                .project
                .as_ref()
                .and_then(|p| p.clips.iter().position(|c| c.folder == f))
            {
                self.selected = [i].into();
                self.key_selected(&ctx);
            }
        }
        self.models.set_asset_safety(
            !self.ai.model_asset_mutation_safe(),
            self.ai.workers_need_stop_for_model_change(),
            self.ai.unresolved_model_fences(),
        );
        self.models.tick(&ctx);
        self.ai
            .set_model_mutation_active(self.models.blocks_worker_launch());
        self.close_tick(&ctx);
        self.keys(&ctx);
        let mut open_models = false;
        Panel::top("banner").show(ui, |ui| {
            ui.label(
                RichText::new(BANNER)
                    .color(Color32::from_rgb(225, 190, 110))
                    .size(12.5),
            );
            if !self.models_open && self.models.startup_ui(ui) {
                open_models = true;
            }
        });
        self.models_open |= open_models;
        if std::mem::take(&mut self.settings_requested) {
            self.open_settings();
        }
        if self.models_open {
            Panel::top("models_nav").show(ui, |ui| {
                let back = ui.button("‹ Back");
                self.models_back_rect = Some(back.rect);
                if back.clicked() {
                    self.models_open = false;
                }
            });
            if self.models_open {
                if self.models.ui(ui) {
                    self.ai.begin_stop();
                }
            }
        } else if self.settings_open {
            Panel::top("settings_nav").show(ui, |ui| {
                let back = ui.button("‹ Back");
                self.settings_back_rect = Some(back.rect);
                if back.clicked() {
                    self.close_settings();
                }
            });
            if self.settings_open {
                match self.settings.ui(ui) {
                    super::settings::Event::Applied => {
                        ctx.set_zoom_factor(self.settings.saved().interface.ui_scale);
                        self.ai
                            .set_targets(self.settings.saved(), self.opts.repo.as_deref());
                    }
                    super::settings::Event::OpenModels => {
                        self.models_open = true;
                    }
                    super::settings::Event::Closed => self.close_settings(),
                    super::settings::Event::None => {}
                }
            }
        } else if self.viewer.is_some() && self.open.is_some() {
            self.workspace(ui);
            if std::mem::take(&mut self.back_requested) {
                self.viewer = None;
                self.open = None;
                ctx.request_repaint();
            }
        } else {
            egui::CentralPanel::default().show(ui, |ui| self.board(ui));
        }
        self.close_ui(&ctx);
        // Dev-only evidence capture: wait for the first frame, then write a PNG and quit.
        if let (Some(path), false) = (self.opts.screenshot.clone(), self.shot_requested) {
            let ready = self.viewer.as_ref().is_none_or(|v| {
                matches!(
                    v.index.availability(v.view, v.playhead, v.range),
                    Availability::Present(_)
                ) && !v.store.cache.is_empty()
                    || v.index.is_empty()
            });
            let after = self.opts.screenshot_after.unwrap_or(1.5);
            if ready && self.started.elapsed().as_secs_f32() > after {
                self.shot_requested = true;
                ctx.request_screenshot(move |img| {
                    let [w, h] = img.size;
                    let mut raw = Vec::with_capacity(w * h * 4);
                    img.pixels
                        .iter()
                        .for_each(|p| raw.extend_from_slice(&p.to_array()));
                    if let Ok(f) = std::fs::File::create(&path) {
                        let mut e =
                            png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
                        e.set_color(png::ColorType::Rgba);
                        e.set_depth(png::BitDepth::Eight);
                        if let Ok(mut wr) = e.write_header() {
                            let _ = wr.write_image_data(&raw);
                        }
                    }
                    std::process::exit(0);
                });
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
        }
        let _ = self.last_clip_view;
        if self.inject_report > 0 {
            self.inject_report -= 1;
            ctx.request_repaint();
        }
        if self.inject_report == 0 && self.inject_i > 0 && self.inject_reported < self.inject_i {
            self.inject_reported = self.inject_i;
            // One line per injected event, after the app has processed it (QA evidence on stderr).
            let (ph, range, view) = self
                .viewer
                .as_ref()
                .map_or((0, None, None), |v| (v.playhead, v.range, Some(v.view)));
            eprintln!(
                "after {:?}: screen={} tab={:?} node={:?} playhead={ph} range={range:?} view={view:?} focus_mode={} props_sheet={} graph_collapsed={}",
                self.opts.keys[self.inject_i - 1],
                if self.viewer.is_some() { "workspace" } else { "board" },
                self.tab,
                self.node,
                self.focus_mode,
                self.props_sheet,
                self.graph_collapsed
            );
            ctx.request_repaint_after(std::time::Duration::from_millis(450));
        }
    }
}

#[cfg(test)]
mod settings_handoff_tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn settings_opens_models_without_discarding_its_draft() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, AppOptions::default());
        app.open_settings();
        let original = app.settings.draft().playback.loop_default;
        app.settings.draft_mut().playback.loop_default = !original;
        let frame = |app: &mut App, events| {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                events,
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
        };
        frame(&mut app, vec![]);
        let pos = app.settings.models_rect().unwrap().center();
        frame(
            &mut app,
            vec![
                egui::Event::PointerMoved(pos),
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: Default::default(),
                },
            ],
        );
        frame(
            &mut app,
            vec![egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: Default::default(),
            }],
        );
        assert!(app.models_open && app.settings_open);
        assert!(app.settings.dirty());
    }

    #[test]
    fn completed_import_open_request_waits_until_settings_close() {
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions::default(),
            std::sync::Arc::new(|_, _| panic!("unexpected picker")),
        );
        app.open_settings();
        app.pending_open = Some("imported_clip".into());
        app.import_tick(&ctx);
        assert_eq!(app.pending_open.as_deref(), Some("imported_clip"));
        app.close_settings();
        app.import_tick(&ctx);
        assert!(app.pending_open.is_none());
    }

    #[test]
    fn drop_while_files_are_being_checked_notifies_and_does_not_replace_the_plan() {
        let dir = std::env::temp_dir().join(format!("ck-drop-busy-{}", std::process::id()));
        // Fake repo: a stub python that blocks until stdin closes, so the job stays running without real Python.
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::create_dir_all(repo.join("rust-ui/tools")).unwrap();
        std::fs::write(repo.join("rust-ui/tools/ck_import.py"), "").unwrap();
        link_fake_python(&repo);
        std::fs::write(dir.join("block"), "").unwrap();
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo),
                ..AppOptions::default()
            },
            std::sync::Arc::new(|_, _| panic!("unexpected picker")),
        );
        app.imp.begin(
            &ctx,
            None,
            Sources {
                videos: vec![],
                folders: vec![dir.clone()],
            },
        );
        assert!(app.imp.busy(), "fake importer job must start");
        #[derive(Debug)]
        struct D(PathBuf);
        impl egui::DroppedFile for D {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                Err("unused".into())
            }
        }
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            dropped_files: vec![std::sync::Arc::new(D(dir.clone()))],
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
        assert!(app
            .imp
            .notice_text()
            .is_some_and(|t| t.contains("still being checked")));
        drop(app);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Fake repo whose "python" logs every invocation to `log`, answers dry runs with one importable
    /// clip (or an error when the args mention `badproj`) and otherwise idles until stdin closes.
    /// `.venv/bin/python` -> the committed fixture script (a symlink is no write-then-exec, so no ETXTBSY).
    fn link_fake_python(repo: &std::path::Path) {
        let fixture = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/fake_import_python.sh"
        );
        std::os::unix::fs::symlink(fixture, repo.join(".venv/bin/python")).unwrap();
    }

    fn stub_repo(tag: &str) -> (PathBuf, PathBuf) {
        let dir = std::env::temp_dir().join(format!("ck-dest-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let repo = dir.join("repo");
        std::fs::create_dir_all(repo.join(".venv/bin")).unwrap();
        std::fs::create_dir_all(repo.join("rust-ui/tools")).unwrap();
        std::fs::write(repo.join("rust-ui/tools/ck_import.py"), "").unwrap();
        let log = dir.join("calls.log");
        link_fake_python(&repo);
        (repo, log)
    }

    fn calls(log: &std::path::Path) -> Vec<String> {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn pump(ctx: &egui::Context, app: &mut App, until: impl Fn(&App) -> bool) {
        let end = Instant::now() + Duration::from_secs(10);
        for _ in 0..400 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1000.0, 700.0),
                )),
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
            if until(app) {
                return;
            }
            assert!(Instant::now() < end, "timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("condition never reached");
    }

    fn dropped(ctx: &egui::Context, app: &mut App, p: &std::path::Path) {
        #[derive(Debug)]
        struct D(PathBuf);
        impl egui::DroppedFile for D {
            fn path(&self) -> &std::path::Path {
                &self.0
            }
            fn bytes(&self) -> Result<Vec<u8>, String> {
                Err("unused".into())
            }
        }
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 700.0),
            )),
            dropped_files: vec![std::sync::Arc::new(D(p.into()))],
            ..Default::default()
        };
        ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
    }

    fn picker(answer: PathBuf) -> Backend {
        std::sync::Arc::new(move |k, _| {
            assert!(
                matches!(k, PickKind::Location | PickKind::AddTo),
                "only destination dialogs expected"
            );
            vec![answer.clone()]
        })
    }

    fn started(log: &std::path::Path) -> Vec<String> {
        calls(log)
            .into_iter()
            .filter(|c| !c.contains("--dry-run"))
            .collect()
    }

    #[test]
    fn landing_drop_needs_a_chosen_folder_and_creates_the_project_inside_it_literally() {
        let (repo, log) = stub_repo("landing");
        let chosen = repo.parent().unwrap().join("MyFolder");
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                data_dir: Some(repo.parent().unwrap().join("default-data")),
                ..AppOptions::default()
            },
            picker(chosen.clone()),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        assert_eq!(app.imp.pending_dest(), None, "no silent default folder");
        app.imp.confirm_now(&ctx);
        pump(&ctx, &mut app, |_| true);
        assert!(
            app.imp.has_pending() && started(&log).is_empty(),
            "no write without a destination"
        );
        app.imp.dest_pick_requested = Some(PickKind::Location);
        pump(&ctx, &mut app, |a| a.imp.pending_dest().is_some());
        assert_eq!(app.imp.pending_dest(), Some(&Dest::NewIn(chosen.clone())));
        assert!(started(&log).is_empty(), "choosing is not importing");
        app.imp.confirm_now(&ctx);
        pump(&ctx, &mut app, |_| !started(&log).is_empty());
        let c = started(&log);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            c[0].contains("new --name")
                && c[0].contains(&format!("--parent-dir {}", chosen.display())),
            "{}",
            c[0]
        );
        assert!(
            !c[0].contains("Projects") && !c[0].contains("default-data"),
            "{}",
            c[0]
        );
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn cancelling_the_confirm_bar_forgets_sources_and_writes_nothing() {
        let (repo, log) = stub_repo("cancel");
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                ..AppOptions::default()
            },
            picker(repo.clone()),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        app.imp.dest_pick_requested = Some(PickKind::Location);
        pump(&ctx, &mut app, |a| a.imp.pending_dest().is_some());
        app.imp.discard_pending();
        assert!(!app.imp.has_pending() && app.imp.pending_dest().is_none());
        app.imp.confirm_now(&ctx);
        assert!(started(&log).is_empty());
        // A late destination answer has nothing to attach to.
        assert!(!app.request_pick(&ctx, PickKind::Location));
        assert!(!app.imp.set_dest(&ctx, Dest::NewIn(repo.clone())));
        assert!(app
            .imp
            .notice_text()
            .unwrap()
            .contains("no import is waiting"));
        assert!(started(&log).is_empty());
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn existing_project_drop_offers_current_and_can_switch_without_losing_sources() {
        let (repo, log) = stub_repo("existing");
        let base = repo.parent().unwrap().to_path_buf();
        let cur = base.join("cur");
        let other = base.join("other");
        let bad = base.join("badproj");
        std::fs::create_dir_all(cur.join("clips")).unwrap();
        std::fs::write(cur.join("project.json"), br#"{"version":2}"#).unwrap();
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                project: Some(cur.clone()),
                ..AppOptions::default()
            },
            picker(other.clone()),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        assert_eq!(
            app.imp.pending_dest(),
            Some(&Dest::Add(cur.clone())),
            "current project shown and pre-selected"
        );
        // Picking a project that cannot be added to keeps the plan and the previous choice.
        let mut app2 = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                project: Some(cur.clone()),
                ..AppOptions::default()
            },
            picker(bad.clone()),
        );
        dropped(&ctx, &mut app2, &repo);
        pump(&ctx, &mut app2, |a| a.imp.has_pending());
        app2.imp.dest_pick_requested = Some(PickKind::AddTo);
        pump(&ctx, &mut app2, |a| a.imp.notice_text().is_some());
        pump(&ctx, &mut app2, |a| a.imp.has_pending());
        assert_eq!(app2.imp.pending_dest(), Some(&Dest::Add(cur.clone())));
        // A valid other project replaces the choice; confirming adds there and nowhere else.
        app.imp.dest_pick_requested = Some(PickKind::AddTo);
        pump(&ctx, &mut app, |a| {
            a.imp.pending_dest() == Some(&Dest::Add(other.clone()))
        });
        assert!(started(&log).is_empty());
        app.imp.confirm_now(&ctx);
        pump(&ctx, &mut app, |_| !started(&log).is_empty());
        let c = started(&log);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            c[0].contains(&format!("add --project {}", other.display())),
            "{}",
            c[0]
        );
        drop((app, app2));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_fresh_second_drop_replaces_the_plan_and_resets_the_destination() {
        let (repo, log) = stub_repo("second");
        let chosen = repo.parent().unwrap().join("Chosen");
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                ..AppOptions::default()
            },
            picker(chosen.clone()),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        app.imp.dest_pick_requested = Some(PickKind::Location);
        pump(&ctx, &mut app, |a| a.imp.pending_dest().is_some());
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        assert_eq!(
            app.imp.pending_dest(),
            None,
            "destination must be chosen again for new sources"
        );
        app.imp.confirm_now(&ctx);
        assert!(started(&log).is_empty());
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn qa_import_keeps_the_documented_data_dir_projects_default() {
        let (repo, log) = stub_repo("qa");
        let data = repo.parent().unwrap().join("data");
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                data_dir: Some(data.clone()),
                import_folders: vec![repo.clone()],
                auto_confirm: true,
                ..AppOptions::default()
            },
            std::sync::Arc::new(|_, _| panic!("unexpected picker")),
        );
        pump(&ctx, &mut app, |_| !started(&log).is_empty());
        let c = started(&log);
        assert!(
            c[0].contains(&format!("--parent-dir {}", data.join("Projects").display())),
            "{}",
            c[0]
        );
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn landing_plan_is_a_destination_less_probe_never_a_default_folder() {
        let (repo, log) = stub_repo("probe");
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                ..AppOptions::default()
            },
            std::sync::Arc::new(|_, _| panic!("unexpected picker")),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        let c = calls(&log);
        assert_eq!(c.len(), 1, "{c:?}");
        assert!(
            c[0].contains("--dry-run probe") && !c[0].contains("new") && !c[0].contains("-dir"),
            "{}",
            c[0]
        );
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn opening_another_project_forgets_a_waiting_plan_so_it_cannot_write_the_old_one() {
        let (repo, log) = stub_repo("switch");
        let base = repo.parent().unwrap().to_path_buf();
        let (a, b) = (base.join("A"), base.join("B"));
        for d in [&a, &b] {
            std::fs::create_dir_all(d.join("clips")).unwrap();
            std::fs::write(d.join("project.json"), br#"{"version":2}"#).unwrap();
        }
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                project: Some(a.clone()),
                ..AppOptions::default()
            },
            std::sync::Arc::new(|_, _| panic!("unexpected picker")),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |x| x.imp.has_pending());
        assert_eq!(app.imp.pending_dest(), Some(&Dest::Add(a.clone())));
        // Reloading the same project keeps the plan ...
        app.reload_keep_selection(&ctx);
        assert!(app.imp.has_pending());
        // ... opening B drops it: nothing can be confirmed into A any more.
        app.path_input = b.display().to_string();
        app.load(&ctx);
        assert!(!app.imp.has_pending() && app.imp.pending_dest().is_none());
        app.imp.confirm_now(&ctx);
        assert!(started(&log).is_empty(), "{:?}", started(&log));
        // A failed load (project closes) forgets it as well.
        app.path_input = a.display().to_string();
        app.load(&ctx);
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |x| x.imp.has_pending());
        app.path_input = base.join("missing").display().to_string();
        app.load(&ctx);
        assert!(!app.imp.has_pending());
        drop(app);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn confirm_and_cancel_are_held_while_a_destination_dialog_is_open() {
        let (repo, log) = stub_repo("hold");
        let chosen = repo.parent().unwrap().join("Chosen");
        let gate = std::sync::Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let g = gate.clone();
        let answer = chosen.clone();
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions {
                repo: Some(repo.clone()),
                data_dir: Some(repo.parent().unwrap().join("d")),
                ..AppOptions::default()
            },
            std::sync::Arc::new(move |_, _| {
                let mut open = g.0.lock().unwrap();
                while !*open {
                    open = g.1.wait(open).unwrap();
                }
                vec![answer.clone()]
            }),
        );
        dropped(&ctx, &mut app, &repo);
        pump(&ctx, &mut app, |a| a.imp.has_pending());
        app.imp.dest_pick_requested = Some(PickKind::Location);
        pump(&ctx, &mut app, |a| a.picking());
        app.imp.confirm_now(&ctx);
        app.imp.discard_pending();
        assert!(
            app.imp.has_pending(),
            "Cancel is off while the dialog is open"
        );
        assert!(started(&log).is_empty());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        pump(&ctx, &mut app, |a| a.imp.pending_dest().is_some());
        // The answer waits behind Settings: confirming now would still use the old (absent/other) choice.
        app.settings_open = true;
        app.held_pick = Some((PickKind::Location, vec![chosen.clone()]));
        app.import_tick(&ctx);
        app.imp.confirm_now(&ctx);
        assert!(app.imp.has_pending() && started(&log).is_empty());
        app.held_pick = None;
        app.settings_open = false;
        app.import_tick(&ctx);
        app.imp.confirm_now(&ctx);
        pump(&ctx, &mut app, |_| !started(&log).is_empty());
        assert_eq!(started(&log).len(), 1);
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn folder_stem_matches_backend_sanitising() {
        assert_eq!(
            super::super::import::folder_stem("  My shot #1 / A  "),
            "My_shot_1_A"
        );
        assert_eq!(super::super::import::folder_stem("__"), "");
    }
}
