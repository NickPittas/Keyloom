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
use crate::hints::Final;
use crate::project::*;
use crate::settings::{default_path, Store};
use eframe::egui::{self, Color32, Key, Panel, RichText, Sense, Ui, Vec2};
use serde_json::Value;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Instant;

mod driver;
mod hint;
mod media;
mod project_shell;
mod ws;
/// 24 px less than before: the always-visible metrics footer takes that from the viewer.
const GRAPH_DEFAULT: f32 = 156.0;
const GRAPH_MIN: f32 = 120.0;
/// Narrow windows: the graph shrinks to this before the viewer does (the graph pans, the Hint image stays large).
const GRAPH_MIN_NARROW: f32 = 88.0;
/// Narrow windows open the graph short, so the viewer keeps its height (the graph pans; Fit keeps it clear of its button).
const GRAPH_DEFAULT_NARROW: f32 = 148.0;
/// Hint page bar: progress, marks and overlay on the left; the generate actions sit in the same panel on the right.
const HINT_TOOLS_H: f32 = 160.0;
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
    PreMatte,
    Hint,
    Key,
    Out(View),
}
fn to_kind(n: Node) -> graph::NodeKind {
    use graph::{NodeKind as K, OutKind as O};
    match n {
        Node::Media => K::Media,
        Node::PreMatte => K::PreMatte,
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
        K::PreMatte => Node::PreMatte,
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
            Node::PreMatte => "Pre-matte",
            Node::Hint => "Hint",
            Node::Key => "Key",
            Node::Out(v) => v.label(),
        }
    }
    fn tab(self) -> Tab {
        match self {
            Node::Media => Tab::Media,
            Node::PreMatte | Node::Hint => Tab::Hint,
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
    pub start_background: bool,
    pub instance_dir: Option<PathBuf>,
    /// Shown as the first notice (startup problems the user must see).
    pub startup_notice: Option<String>,
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
    /// Local CPU/RAM/GPU sampler: `Off` (default) starts nothing; the binary uses `Local`.
    pub metrics: crate::metrics::Source,
    /// Network client link: `Off` (default, what tests use) starts nothing; the binary uses `Local`.
    pub net: crate::netlink::Source,
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
    /// A secondary-click release still owed to egui: egui only counts a right click whose press it saw on an
    /// earlier frame, so the press and the release go out on consecutive frames.
    inject_release: Option<egui::Pos2>,
    /// Frames to wait before the owed secondary release (see `inject_release`).
    inject_release_wait: u8,
    inject_report: u8,
    /// Back clicked during this frame: applied after the workspace finishes rendering.
    back_requested: bool,
    back_rect: Option<egui::Rect>,
    header_rects: Vec<egui::Rect>,
    /// Tools panel above the graph, its status (left) and action (right) regions from the last frame (layout tests).
    tools_rects: Option<[egui::Rect; 3]>,
    imp: ImportUi,
    pending_reload: Option<PathBuf>,
    qa_import: Option<Sources>,
    picker: Picker,
    /// A finished dialog waits here until the Board is showing and no import is running.
    held_pick: Option<(PickKind, Vec<PathBuf>)>,
    /// The last created-project reload was refused (old project kept): its "open the new clip" must not run.
    reload_refused: bool,
    /// File button and its New project item, from the last frame (menu tests; the item only while the menu is open).
    new_menu_rects: [Option<egui::Rect>; 2],
    /// File > New project: the chosen parent folder and the project name being typed.
    new_project_draft: Option<(PathBuf, String)>,
    new_project_error: Option<String>,
    new_project_create_rect: Option<egui::Rect>,
    /// "Open clip settings" button rectangles from the last Board frame: per-card (layout tests).
    open_rects: Vec<(Option<usize>, egui::Rect)>,
    /// Settings and Models pages replace the Board/workspace while open, which stay untouched underneath.
    settings: SettingsPage,
    settings_open: bool,
    models: super::models::Page,
    models_open: bool,
    /// Clients page (real machines, live state). Replaces Board/workspace while open, like Settings.
    clients_open: bool,
    /// Remove asked to confirm: the connection's credentials path.
    remove_confirm: Option<String>,
    /// One long-lived sampler for the whole session (see `metrics`); the footer and the local client card read it.
    metrics: driver::Held<crate::metrics::Supervisor>,
    metrics_wake: Option<std::sync::Arc<dyn Fn() + Send + Sync>>,
    metrics_rect: Option<egui::Rect>,
    remote: driver::Held<crate::remote_set::RemoteSet>,
    setup: crate::network_setup::Setup,
    /// The runtime driver thread (set by `App::new`, the window path); `None` = this host ticks inline from `frame`.
    driver: Option<driver::Driver>,
    net_repo: Option<PathBuf>,
    net_python: Option<PathBuf>,
    clients_rect: Option<egui::Rect>,
    clients_back_rect: Option<egui::Rect>,
    settings_rect: Option<egui::Rect>,
    /// The Settings button was clicked this frame; handled once the screen has finished drawing.
    settings_requested: bool,
    settings_back_rect: Option<egui::Rect>,
    models_back_rect: Option<egui::Rect>,
    /// A clip an import asked to open while the page was showing.
    pending_open: Option<String>,
    ai: driver::Held<AiUi>,
    close: CloseState,
    /// The user chose Quit anyway, or all graceful work has stopped: a close request now goes through.
    close_force: bool,
    /// Explicit Quit was chosen: closing really closes even in background mode.
    pub(super) quitting: bool,
    close_rects: Vec<egui::Rect>,
    /// Output-location editor for the open project (Deliver tab).
    loc_note: Option<String>,
    /// (range, linear) whose save failed: not retried every frame, shown in the notice instead.
    persist_failed: Option<(Option<(usize, usize)>, bool)>,
    hint: hint::State,
    /// Route/delivery edits of shots that could not be written when they were left (shown as unsaved).
    hint_unsaved: std::collections::BTreeMap<String, hint::Unsaved>,
    recent: project_shell::Recent,
    /// Last explicit/implicit Save failure; the indicator stays Unsaved until a Save succeeds.
    save_error: Option<String>,
    /// Key-panel draft (params + output config) per clip: clips opened this load, what disk holds, and whether the open one differs.
    draft_clips: BTreeSet<String>,
    draft_saved: std::collections::BTreeMap<String, Value>,
    draft_dirty: bool,
    /// project.json `output_location` as last read or written (for the Unsaved indicator).
    loc_saved: Option<Value>,
    ws: ws::WsState,
    media: media::MediaState,
    last_ctx: Option<egui::Context>,
    /// Board thumbnails: decoded off the UI thread, kept per source frame.
    thumbs: Option<super::thumbs::Thumbs>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, opts: AppOptions) -> Self {
        let notice = opts.startup_notice.clone();
        let mut app = Self::with_ctx(&cc.egui_ctx, opts);
        app.ai.notice = notice;
        app.models.start_background(&cc.egui_ctx);
        // The window path runs scheduling, workers and the network client off the frame loop.
        app.release_shared();
        let parts = driver::Parts {
            ai: app.ai.shared(),
            remote: app.remote.shared(),
            metrics: app.metrics.shared(),
            repo: app.net_repo.clone(),
            python: app.net_python.clone(),
            ctx: cc.egui_ctx.clone(),
        };
        let first = driver::Snapshot { settings: app.settings.saved().clone(), path: app.settings.store_path().map(PathBuf::from) };
        match driver::Driver::start(parts, first) {
            Ok(d) => app.driver = Some(d),
            Err(e) => app.ai.notice = Some(format!("The background driver could not start ({e}); work advances only while the window is drawn.")),
        }
        app.release_shared();
        app
    }

    /// Window-free constructor (tests drive the same `frame` the window does).
    pub fn with_ctx(ctx: &egui::Context, opts: AppOptions) -> Self {
        Self::with_picker(ctx, opts, rfd_backend())
    }

    /// Same, with the file-dialog backend replaced (tests never open a real dialog).
    pub fn with_picker(ctx: &egui::Context, opts: AppOptions, dialogs: Backend) -> Self {
        super::theme::apply(ctx);
        let settings_path = opts.settings_path.clone().or_else(default_path);
        crate::queue::set_settings_path(settings_path.clone());
        let settings = SettingsPage::new(Store::load(settings_path));
        ctx.set_zoom_factor(settings.saved().interface.ui_scale);
        let settings_saved = settings.saved().clone();
        let models = super::models::Page::new(opts.repo.as_deref());
        // Runtime resolution (descriptor / CK_REPO / checkout discovery) happens once here; import
        // and AI launching re-resolve on demand, never from a build-time path.
        let launch = crate::runtime::launch(opts.repo.as_deref());
        let recent = project_shell::Recent::load(settings.store_path());
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
            inject_release: None,
            inject_release_wait: 1,
            inject_report: 0,
            back_requested: false,
            back_rect: None,
            header_rects: vec![],
            tools_rects: None,
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
            reload_refused: false,
            new_menu_rects: [None; 2],
            new_project_draft: None,
            new_project_error: None,
            new_project_create_rect: None,
            open_rects: vec![],
            settings,
            settings_open: false,
            models,
            models_open: false,
            clients_open: false,
            remove_confirm: None,
            metrics: driver::Held::new(opts.metrics.supervisor(opts.repo.as_deref().or(launch.repo.as_deref()), launch.python.clone())),
            metrics_wake: None,
            metrics_rect: None,
            remote: driver::Held::new(crate::remote_set::RemoteSet::new(opts.net.clone())),
            setup: crate::network_setup::Setup::new(opts.net.clone()),
            driver: None,
            net_repo: opts.repo.clone().or(launch.repo.clone()),
            net_python: launch.python.clone(),
            clients_rect: None,
            clients_back_rect: None,
            settings_rect: None,
            settings_requested: false,
            settings_back_rect: None,
            models_back_rect: None,
            pending_open: None,
            ai: driver::Held::new(AiUi::new(&settings_saved, opts.repo.as_deref())),
            close: CloseState::None,
            close_force: false,
            quitting: false,
            close_rects: vec![],
            loc_note: None,
            persist_failed: None,
            hint: Default::default(),
            hint_unsaved: Default::default(),
            recent,
            save_error: None,
            draft_clips: BTreeSet::new(),
            draft_saved: Default::default(),
            draft_dirty: false,
            loc_saved: None,
            ws: Default::default(),
            media: Default::default(),
            last_ctx: None,
            thumbs: None,
        };
        app.ai.set_presets_file(crate::presets::file_beside(app.settings.store_path()));
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
        let prev = self.project.as_ref().map(|p| p.root.clone());
        match load_project(&PathBuf::from(self.path_input.trim()), &lo) {
            Ok(p) => {
                // Same-root reloads (import progress) keep unsaved drafts, their baselines and the location.
                let switched = prev.as_ref() != Some(&p.root);
                self.summaries = p.clips.iter().map(card_summary).collect();
                self.project = Some(p);
                self.load_error = None;
                self.reload_refused = false;
                if switched {
                    self.save_error = None;
                    self.forget_project_state();
                    self.init_location();
                }
                self.record_recent();
            }
            Err(e) => {
                self.summaries.clear();
                self.project = None;
                self.load_error = Some(e);
                self.forget_project_state();
            }
        }
        // A reload of the same project keeps a waiting plan; switching projects forgets it (its "open
        // project" destination would otherwise still point at the old one).
        self.imp
            .project_changed(self.project.as_ref().map(|p| p.root.as_path()));
        ctx.request_repaint();
    }

    /// A different project is loading: nothing keyed by clip folder may carry over (same names, other project).
    fn forget_project_state(&mut self) {
        self.draft_clips.clear();
        self.draft_saved.clear();
        self.draft_dirty = false;
        self.ai.forget_project();
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
        self.mark_location_clean();
        if let Some(c) = self.clip().cloned() {
            self.adopt_draft(&c);
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
        self.shortcuts(ctx);
        self.new_project_window(ctx);
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
        if self.clients_open {
            if pressed(Key::Escape) {
                self.clients_open = false;
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
        let busy = self.imp.busy() || self.dialog_active();
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
            self.welcome(ui, busy, why, &mut want);
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
            });
        }
        if let Some(k) = want {
            self.request_pick(&ctx, k);
        }
        self.imp.ui(ui);
        if let Some(k) = self.imp.dest_pick_requested.take() {
            self.request_pick(&ctx, k);
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
        let picks: Vec<usize> = sel.iter().copied().collect();
        let chosen: Vec<&Clip> = picks.iter().filter_map(|i| p.clips.get(*i)).collect();
        let every = !p.clips.is_empty() && sel.len() == p.clips.len();
        self.ai.batch_ui(ui, p, self.settings.saved(), &picks);
        ui.horizontal_wrapped(|ui| {
            let mut all = every;
            if super::theme::checkbox(ui, &mut all, "All").changed() {
                sel = if all {
                    (0..p.clips.len()).collect()
                } else {
                    BTreeSet::new()
                };
            }
            self.ai.board_preset_ui(ui);
            ui.separator();
            self.ai.assign_selected_ui(ui, &chosen, every);
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
        // Thumbnails decode in the background; the grid draws placeholders until a frame arrives.
        if self.thumbs.is_none() {
            self.thumbs = Some(super::thumbs::Thumbs::new(&ctx));
        }
        let all_clips: Vec<&Clip> = p.clips.iter().collect();
        if let Some(t) = &mut self.thumbs {
            t.request(&all_clips);
            t.tick();
        }
        let local_view = self.metrics.view(std::time::Instant::now());
        let active = self.active_gpus();
        let mut queue_actions = vec![];
        if ui.available_width() > 760.0 {
            Panel::right("board_queue")
                .resizable(true)
                .default_size(540.0)
                .min_size(340.0)
                .max_size(900.0)
                .show_separator_line(true)
                .show(ui, |ui| {
                    queue_actions = Self::batch_queue(ui, &mut self.ai, &mut self.remote, &local_view, &active);
                });
        } else {
            Panel::bottom("board_queue_bottom").resizable(true).default_size(280.0).min_size(150.0).show(ui, |ui| { queue_actions = Self::batch_queue(ui, &mut self.ai, &mut self.remote, &local_view, &active); });
        }
        egui::ScrollArea::vertical()
            .auto_shrink([false; 2])
            .show(ui, |ui| {
                // Approved density: compact cards, five across when the queue panel leaves room.
                let card_w = 220.0;
                let gap = 10.0;
                // Never more columns than clips, so a single clip gets a readable card instead of a stamp.
                let fit = ((ui.available_width() + gap) / (card_w + gap)).floor().max(1.0) as usize;
                let cols = fit.min(p.clips.len().max(1));
                // A card never stretches past a readable width, even when the board holds one clip.
                let w = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32)
                    .clamp(170.0, card_w);
                let mut base = 0usize;
                for row in p.clips.chunks(cols) {
                    let first = base;
                    base += row.len();
                    ui.horizontal_top(|ui| {
                        for (k, c) in row.iter().enumerate() {
                            let i = first + k;
                            let st = self.imp.card_state(c);
                            // A cell is its own top-down layout: a group frame inside a horizontal row would
                            // otherwise lay the card's lines out side by side.
                            ui.allocate_ui_with_layout(
                                Vec2::new(w, 0.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    egui::Frame::group(ui.style()).inner_margin(8.0).show(
                                        ui,
                                        |ui| {
                                            ui.set_width(w - 18.0);
                                            board_card(
                                                ui,
                                                i,
                                                c,
                                                st,
                                                &mut sel,
                                                &mut open_idx,
                                                &mut resume_idx,
                                                self.thumbs.as_mut(),
                                                &mut self.ai,
                                                &mut self.open_rects,
                                            );
                                        },
                                    );
                                },
                            );
                        }
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
        for action in queue_actions { self.net_action(action); }
    }

    fn batch_queue(ui: &mut Ui, ai: &mut AiUi, remote: &mut crate::remote_set::RemoteSet, local: &crate::metrics::View, active: &[u32]) -> Vec<crate::remote::Action> {
        ui.horizontal(|ui| {
            ui.heading("Queue");
            if ui.small_button("Clear finished").on_hover_text("Removes finished, failed and cancelled jobs from the queue and its logs. Running jobs stay.").clicked() {
                ai.clear_finished();
                remote.clear_finished();
            }
        });
        let mut actions = vec![];
        egui::ScrollArea::vertical().id_salt("batch_work_queue").auto_shrink([false; 2]).show(ui, |ui| {
            ai.queue_ui(ui, local, active);
            actions = remote.queue_ui(ui);
        });
        actions
    }

    // ------------------------------------------------------------------ workspace
    fn workspace(&mut self, ui: &mut Ui) {
        let narrow = ui.available_width() < 900.0;
        let focus = self.focus_mode;
        let ctx = ui.ctx().clone();
        if self.open.is_none() {
            return;
        }
        if narrow && !self.props_sheet && !matches!(self.tab, Tab::Key | Tab::Deliver) {
            self.ai_watch(&ctx);
        }

        let mut hr: Vec<egui::Rect> = vec![];
        Panel::top("header").frame(egui::Frame::new().fill(theme::CARD).inner_margin(8)).show(ui, |ui| {
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
                        // Fixed-height bar: room for the global buttons without costing viewer height.
                        self.ai_bar_status(ui);
                    });
                });
            if !narrow || self.props_sheet {
                Panel::right("props")
                    .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(14).stroke(egui::Stroke::new(1.0, Color32::from_rgb(96, 120, 104))))
                    .default_size(380.0)
                    .min_size(300.0)
                    .max_size(520.0)
                    .show_separator_line(true)
                    .resizable(true)
                    .show(ui, |ui| self.properties(ui));
            }
            // Resizable graph panel; its max keeps the viewer at least MIN_VIEWER px tall (Hint image stays large).
            let actions = self.clip().is_some_and(|c| matches!(self.tab, Tab::Hint | Tab::Key | Tab::Deliver) || self.tab == Tab::Media && self.media_has_actions(c));
            let tools_h = match (actions, narrow) {
                (true, false) => HINT_TOOLS_H,
                // Narrow: the viewer keeps its height; the action group is first, the status scrolls below it.
                (true, true) => 96.0,
                _ => 78.0,
            };
            let graph_min = if narrow { GRAPH_MIN_NARROW } else { GRAPH_MIN };
            let max_h = (ui.available_height() - 34.0 - 26.0 - tools_h - MIN_VIEWER).max(graph_min);
            if self.graph_collapsed {
                Panel::bottom("graph_collapsed")
                    .frame(egui::Frame::new().fill(theme::CANVAS).inner_margin(8))
                    .exact_size(GRAPH_HEADER)
                    .show(ui, |ui| self.graph_header(ui));
            } else {
                // Separate ids: each mode keeps its own dragged height (a narrow window must not inherit a wide one's).
                Panel::bottom(if narrow { "graph_narrow" } else { "graph" })
                    .frame(egui::Frame::new().fill(theme::CANVAS).inner_margin(10).stroke(egui::Stroke::new(1.0, Color32::from_rgb(69, 91, 76))))
                    .resizable(true)
                    .default_size(if narrow { GRAPH_DEFAULT_NARROW } else { GRAPH_DEFAULT })
                    .min_size(graph_min)
                    .max_size(max_h)
                    .show(ui, |ui| {
                        self.graph_header(ui);
                        let h = ui.available_height();
                        self.graph(ui, h);
                    });
            }
            Panel::bottom(if narrow { "pagetools_narrow" } else { "pagetools" })
                .frame(egui::Frame::new().fill(theme::CARD).inner_margin(12).stroke(egui::Stroke::new(1.0, Color32::from_rgb(83, 103, 90))))
                .resizable(true)
                .default_size(tools_h)
                .min_size(64.0)
                .max_size((ui.available_height() - MIN_VIEWER).max(64.0))
                .show_separator_line(true)
                .show(ui, |ui| self.tools_row(ui, actions, narrow));
        }
        egui::CentralPanel::default().show(ui, |ui| {
            // The Hint page's tools sit beside the viewer, not above it: a narrow icon column on the left.
            ui.horizontal_top(|ui| {
                self.hint_tool_strip(ui);
                if let Some(v) = &mut self.viewer {
                    // Keep repainting while playing: with all frames cached no decode completes to wake the UI.
                    if v.tick() {
                        ctx.request_repaint();
                    }
                    // The viewer fills what the strip left; a plain call inside a horizontal row would get the
                    // row's remaining width but no height.
                    let size = ui.available_size();
                    ui.allocate_ui_with_layout(
                        size,
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| v.ui(ui),
                    );
                }
            });
        });
    }

    /// The one panel above the graph: page status (progress, marks, overlay) on the left, a vertical divider,
    /// and the centered primary action group on the right. Narrow windows stack the two inside the same panel.
    fn tools_row(&mut self, ui: &mut Ui, actions: bool, narrow: bool) {
        let full = ui.available_rect_before_wrap();
        let scroll = |id: &'static str| egui::ScrollArea::vertical().id_salt(id).auto_shrink([false; 2]);
        if !actions || narrow {
            self.tools_rects = Some([full, full, full]);
            scroll("page_scroll").show(ui, |ui| {
                if actions {
                    self.action_bar(ui, false);
                    ui.separator();
                }
                self.page_tools(ui);
            });
            return;
        }
        let mid = full.min.x + full.width() * 0.34;
        let left = egui::Rect::from_min_max(full.min, egui::pos2(mid - 6.0, full.max.y));
        let right = egui::Rect::from_min_max(egui::pos2(mid + 6.0, full.min.y), full.max);
        self.tools_rects = Some([full, left, right]);
        ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
            scroll("page_scroll").show(ui, |ui| self.page_tools(ui));
        });
        ui.painter().vline(mid, full.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
        ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| self.action_bar(ui, true));
    }

    /// The page's processing actions (Generate / Key / Render / Export) in one centered group of sensible width.
    fn action_bar(&mut self, ui: &mut Ui, scroll: bool) {
        let Some(clip) = self.clip().cloned() else {
            return;
        };
        let w = ui.available_width().min(if self.tab == Tab::Hint { 760.0 } else { 520.0 });
        let pad = ((ui.available_width() - w) / 2.0).max(0.0);
        ui.horizontal_top(|ui| {
            ui.add_space(pad);
            ui.allocate_ui_with_layout(
                egui::vec2(w, if scroll { ui.available_height() } else { 0.0 }),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    if scroll {
                        egui::ScrollArea::vertical()
                            .id_salt("action_scroll")
                            .auto_shrink([false; 2])
                            .show(ui, |ui| self.action_body(ui, &clip));
                    } else {
                        self.action_body(ui, &clip);
                    }
                },
            );
        });
    }

    fn action_body(&mut self, ui: &mut Ui, clip: &Clip) {
        let video = self.hint.deliveries_has_video();
        match self.tab {
            Tab::Media => self.media_actions(ui, clip),
            Tab::Hint => self.hint_actions(ui, clip),
            Tab::Deliver if video => self.deliver_action(ui, clip),
            Tab::Key => self.key_actions(ui, clip, "Key clip"),
            _ => self.key_actions(ui, clip, "Render image sequences"),
        }
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
        let mut cfg = self.hint_cfg_for(&clip);
        let pending: Vec<Node> = [Node::PreMatte, Node::Hint, Node::Key, Node::Out(View::Fg), Node::Out(View::Matte), Node::Out(View::Comp), Node::Out(View::Processed)].into_iter().filter(|n| self.stage_pending(*n)).collect();
        let badge = |n: Node| -> String {
            if pending.contains(&n) { return "Update required".to_string(); }
            match n {
                Node::Media => format!(
                    "{} · {} fr",
                    if v.linear { "Linear" } else { "sRGB" },
                    clip.input_files.len()
                ),
                Node::PreMatte => {
                    // Direct: the pre-matte is the hint itself, otherwise it is the refiner's guide.
                    let n = if cfg.finish == Final::Direct { clip.alpha_frames } else { clip.mask_frames };
                    if n > 0 { format!("Pre-matte · {n} frames") } else { "Pre-matte · none yet".into() }
                }
                Node::Hint => {
                    if clip.alpha_frames > 0 {
                        format!("Hint · {} frames", clip.alpha_frames)
                    } else {
                        "Hint · none yet".into()
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
            Node::PreMatte => (if cfg.finish == Final::Direct { clip.alpha_frames } else { clip.mask_frames }) == 0,
            Node::Hint => clip.alpha_frames == 0,
            Node::Out(view) => v.index.view_frames(view) == 0,
            _ => false,
        };
        let ctx = ui.ctx().clone();
        // What Key will actually read: the graph must not show a hint edge into Key that Key ignores.
        let hint_consumed = match &self.project {
            Some(p) => self
                .ai
                .draft(&clip, p, self.settings.saved())
                .params
                .hint_mode
                != "none",
            None => false,
        };
        let menu = self.node_menu(&clip, &cfg);
        let (g, gv) = self
            .graphs
            .entry(clip.folder.clone())
            .or_insert_with(|| (graph::Graph::new(), graph::View::default()));
        g.sync(&cfg, hint_consumed);
        let ev = graph::show(
            ui,
            g,
            gv,
            Some(to_kind(self.node)),
            |k| graph::NodeLook {
                badge: badge(from_kind(k)),
                dimmed: false,
                dashed: dashed(from_kind(k)),
                actions: menu
                    .iter()
                    .find(|(n, _, _)| *n == from_kind(k))
                    .map(|(_, _, a)| a.clone())
                    .unwrap_or_default(),
                note: menu
                    .iter()
                    .find(|(n, _, _)| *n == from_kind(k))
                    .and_then(|(_, note, _)| note.clone()),
            },
            height,
        );
        match ev {
            Some(graph::Event::Connect(c)) => {
                if g.rewire(&mut cfg, c) {
                    self.hint_set_cfg(&clip, cfg);
                }
            }
            Some(graph::Event::Select(k)) => self.node = from_kind(k),
            Some(graph::Event::Open(k)) => {
                // Same path as clicking the tab, so the viewer's default view follows too.
                let n = from_kind(k);
                self.node = n;
                self.switch_tab(n.tab());
                self.node = n;
            }
            Some(graph::Event::Menu(k, a)) => self.node_action(&ctx, &clip, from_kind(k), a),
            None => {}
        }
    }

    /// Context-menu entries per node, built from what this build can actually do. A format is named only when
    /// the worker writes it (hints/guides are 8-bit PNG; Key products are EXR/PNG by their output setting).
    #[allow(clippy::type_complexity)]
    fn node_menu(
        &self,
        clip: &Clip,
        cfg: &crate::hints::HintCfg,
    ) -> Vec<(Node, Option<String>, Vec<(graph::Action, String, bool)>)> {
        let local = self.ai.local_caps();
        let ready = |k: crate::tasks::TaskKind| {
            local
                .as_ref()
                .is_some_and(|c| c.tasks.get(k.wire()).copied().unwrap_or(false))
        };
        let busy = self.ai.task_busy(&clip.folder);
        let guide_ok = !cfg.bypass && !busy && ready(cfg.guide.task());
        let fin_ok = !cfg.bypass
            && !busy
            && cfg
                .finish
                .task()
                .map_or(cfg.finish == Final::Direct && guide_ok, ready);
        let mut out = vec![];
        for node in [Node::Media, Node::PreMatte, Node::Hint, Node::Key] {
            let (note, actions): (Option<String>, Vec<(graph::Action, String, bool)>) = match node {
                Node::Media => {
                    let missing = matches!(clip.source, Source::Missing) || clip.error.is_some();
                    (
                        None,
                        vec![
                            (graph::Action::Open, "Open Media".into(), true),
                            (
                                graph::Action::Relink,
                                "Relink missing footage…".into(),
                                missing,
                            ),
                        ],
                    )
                }
                Node::PreMatte => (
                    Some("Pre-matte frames are written to VideoMamaMaskHint as 8-bit PNG.".into()),
                    vec![
                        (graph::Action::Open, "Open Pre-matte".into(), true),
                        (
                            graph::Action::GenerateRange,
                            format!("Generate {} over In/Out", cfg.guide.label()),
                            guide_ok,
                        ),
                        (
                            graph::Action::GenerateMarks,
                            "Generate on marked frames".into(),
                            guide_ok,
                        ),
                    ],
                ),
                Node::Hint => (
                    Some(
                        "Hint frames are written to AlphaHint as 8-bit PNG (no EXR hint output)."
                            .into(),
                    ),
                    vec![
                        (graph::Action::Open, "Open Hint".into(), true),
                        (
                            graph::Action::GenerateRange,
                            format!("Generate {} hint over In/Out", cfg.finish.label()),
                            fin_ok,
                        ),
                        (
                            graph::Action::GenerateMarks,
                            "Generate on marked frames".into(),
                            fin_ok,
                        ),
                    ],
                ),
                _ => (
                    Some("Key reads the hint; results are EXR/PNG image sequences per Deliver.".into()),
                    vec![(graph::Action::Open, "Open Key".into(), true)],
                ),
            };
            out.push((node, note, actions));
        }
        out
    }

    /// Apply a node context action through the same paths the pages use (no shortcut around the gate).
    fn node_action(
        &mut self,
        ctx: &egui::Context,
        clip: &Clip,
        node: Node,
        action: graph::Action,
    ) {
        match action {
            graph::Action::Open => {
                self.node = node;
                self.switch_tab(node.tab());
                self.node = node;
            }
            graph::Action::Relink => {
                let _ = self.request_pick(ctx, PickKind::Relink);
            }
            graph::Action::GenerateRange | graph::Action::GenerateMarks => {
                // The same validated path the Hint page uses: settings are saved, In/Out is respected, and a
                // refusal is reported instead of silently starting nothing.
                let marks = action == graph::Action::GenerateMarks;
                self.node = node;
                self.tab = Tab::Hint;
                let pre_matte = node == Node::PreMatte;
                self.hint_run_from_graph(pre_matte, marks);
                let _ = clip;
            }
        }
    }

    fn page_tools(&mut self, ui: &mut Ui) {
        let (Some(clip), Some(v)) = (self.clip().cloned(), self.viewer.as_ref()) else {
            return;
        };
        let (_v_linear, v_len, v_range, v_range_text) =
            (v.linear, v.index.len(), v.range, v.range_text());
        let v_view_frames: Vec<(View, usize)> =
            [View::Fg, View::Matte, View::Comp, View::Processed]
                .into_iter()
                .map(|w| (w, v.index.view_frames(w)))
                .collect();
        match self.tab {
            Tab::Media => {
                self.media_details(ui, &clip);
                ui.add_space(4.0);
                ui.horizontal_wrapped(|ui| {
                    if (matches!(clip.source, Source::Missing) || clip.error.is_some())
                        && ui.button("Relink missing footage…").on_hover_text("Choose the folder the image sequence now lives in; its frames must match the stored identity").clicked()
                    {
                        let ctx = ui.ctx().clone();
                        self.request_pick(&ctx, PickKind::Relink);
                    }
                    ui.label(RichText::new("Input colour and companion imports are in Properties (right).").color(DIM).size(11.5));
                });
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
                ui.label(RichText::new("Set options in Properties (right). Key clip, Resume and Replace are in the action area on the right; drag the panel edge to resize.").color(DIM).size(12.0));
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
                ui.label(RichText::new("Choose image sequence or video settings in Properties (right); render or export in the action area on the right.").color(DIM).size(12.0));
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
        // Deliver is one page with two halves, so it is titled as the page, not as the product node.
        ui.heading(format!(
            "Properties: {}",
            if self.tab == Tab::Deliver {
                "Deliver"
            } else {
                node.title()
            }
        ));
        egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            if self.tab == Tab::Key {
                theme::section(ui, "Render client", |ui| self.ai.render_client_ui(ui, &clip));
                egui::CollapsingHeader::new("Worker details").show(ui, |ui| self.ai.worker_controls(ui));
                ui.add_space(8.0);
            }
            if self.tab == Tab::Deliver {
                self.preset_ui(ui, &clip);
                ui.separator();
                self.deliver_ui(ui, &clip);
                ui.separator();
                self.ai.render_client_ui(ui, &clip);
                ui.separator();
                // Everything about *where* and *how* is behind folds: it is settings, not the action.
                egui::CollapsingHeader::new("Output folder & templates")
                    .default_open(false)
                    .show(ui, |ui| self.location_preview(ui, &clip));
                egui::CollapsingHeader::new("How the files are written")
                    .default_open(false)
                    .show(ui, |ui| self.output_details(ui, &clip));
                egui::CollapsingHeader::new("Workers")
                    .default_open(false)
                    .show(ui, |ui| self.ai.worker_controls(ui));
                return;
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
                self.media_paths(ui, &clip);
            }
            Node::PreMatte | Node::Hint => {
                kv(ui, "Hint frames", &clip.alpha_frames.to_string());
                kv(ui, "Mask hint frames", &clip.mask_frames.to_string());
                kv(ui, "Keyframe marks", &if clip.keyframes.is_empty() { "none".into() } else { format!("{:?}", clip.keyframes) });
                ui.separator();
                self.hint_props(ui, &clip);
                ui.separator();
                let saved = self
                    .project
                    .as_ref()
                    .is_some_and(|p| self.ai.hint_controls(ui, &clip, p, self.settings.saved()));
                if saved {
                    self.refresh_clip(&ui.ctx().clone(), &clip.folder);
                }
                self.hint_cleanup_props(ui, &clip);
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
                self.preset_ui(ui, &clip);
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

    /// Deliver's second fold: the parts of a sequence run that are explanation, not action.
    fn output_details(&mut self, ui: &mut Ui, clip: &Clip) {
        ui.label(
            RichText::new("Key writes each product as its own EXR/PNG sequence into the output folder. Existing results are kept unless you explicitly confirm Replace.")
                .color(DIM)
                .size(12.0),
        );
        if let Some(m) = &clip.manifest {
            kv(ui, "Last run engine", &m.engine);
            kv(ui, "Complete", &m.complete.to_string());
            if m.raw.get("exr_lossy").and_then(Value::as_bool) == Some(true) {
                kv(
                    ui,
                    "EXR",
                    &format!(
                        "lossy {} ({})",
                        m.exr_compression.as_deref().unwrap_or("?"),
                        m.raw
                            .get("exr_pixel_type")
                            .and_then(Value::as_str)
                            .unwrap_or("?")
                    ),
                );
            }
        }
        if let Some(r) = clip.result_range_text() {
            kv(ui, "Result covers", &r);
        }
        kv(
            ui,
            "Output folder",
            &clip
                .output_dir
                .as_ref()
                .map_or("unknown".into(), |d| d.display().to_string()),
        );
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
            Some(Ok(p)) => {
                let text = display(&p);
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Resolves to").color(DIM).size(11.5));
                    ui.add(
                        egui::Label::new(RichText::new(&text).monospace().size(11.0))
                            .truncate(),
                    )
                    .on_hover_text(&text);
                    if ui.small_button("Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            }

            Some(Err(e)) => {
                ui.colored_label(Color32::from_rgb(225, 110, 110), format!("{e:?}"));
            }
            None => {
                ui.colored_label(WARN, "No output enabled");
            }
        }
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

    fn key_actions(&mut self, ui: &mut Ui, clip: &Clip, action: &str) {
        let range = self.ai_open_range();
        let lin = self.viewer.as_ref().is_some_and(|v| v.linear);
        if let Some(p) = &self.project {
            theme::section(ui, "Keying", |ui| {
                self.ai.key_panel(ui, clip, p, self.settings.saved(), range, lin, action);
            });
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
        // With the driver running, scheduling already happened off-frame: the UI only adopts its results.
        let due = if self.driver.is_some() { self.ai.take_refreshes() } else { self.ai.tick() };
        for r in due {
            self.refresh_clip(ctx, &r.folder);
        }
        if self.ai.active() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }

    /// Active work cancels the close request; graceful stop keeps the window updating until it exits.
    fn close_tick(&mut self, ctx: &egui::Context) {
        // Hiding also works on compositors without minimize support; relaunch restores this window.
        if ctx.input(|i| i.viewport().close_requested())
            && !self.close_force
            && !self.quitting
            && self.close == CloseState::None
            && self.settings.saved().interface.background
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            return;
        }
        if ctx.input(|i| i.viewport().close_requested()) && !self.close_force {
            let (jobs, alive, models) = (
                self.ai.running_jobs(),
                self.ai.workers_alive() || !self.remote.quiescent(),
                self.models.busy(),
            );
            if jobs || alive || models {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if self.close == CloseState::None {
                    if jobs || models {
                        self.close = CloseState::Prompt;
                    } else {
                        self.ai.begin_stop(); // only idle workers: no question to ask
                        self.remote.shutdown_requested();
                        self.close = CloseState::Stopping;
                    }
                }
            }
        }
        if self.close == CloseState::Stopping && self.ai.all_stopped() && self.remote.quiescent() && !self.models.busy() {
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
                            self.quitting = false;
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
            self.remote.shutdown_requested();
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

/// One Board card: thumbnail, name/state, the compact facts, and the clip's own render client.
#[allow(clippy::too_many_arguments)]
fn board_card(
    ui: &mut Ui,
    i: usize,
    c: &Clip,
    st: CardState,
    sel: &mut BTreeSet<usize>,
    open: &mut Option<usize>,
    resume: &mut Option<usize>,
    mut thumbs: Option<&mut super::thumbs::Thumbs>,
    ai: &mut AiUi,
    open_rects: &mut Vec<(Option<usize>, egui::Rect)>,
) {
    let idle = matches!(st, CardState::Idle);
    let mut on = sel.contains(&i);
    let thumb_h = ((ui.available_width()) * 9.0 / 16.0).clamp(56.0, 118.0);
    egui::Frame::new()
        .fill(ui.visuals().extreme_bg_color)
        .corner_radius(4.0)
        .show(ui, |ui| {
            let (rect, resp) = ui.allocate_exact_size(
                Vec2::new(ui.available_width(), thumb_h),
                Sense::click(),
            );
            let tex = match thumbs.as_mut() {
                Some(t) => t.texture(ui.ctx(), c),
                None => None,
            };
            if let Some(t) = tex {
                let img = t.size_vec2();
                let scale = (rect.width() / img.x).min(rect.height() / img.y);
                let size = img * scale;
                let r = egui::Rect::from_center_size(rect.center(), size);
                ui.painter().image(
                    t.id(),
                    r,
                    egui::Rect::from_min_max(egui::Pos2::ZERO, egui::Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
            } else {
                let why = thumbs
                    .as_deref()
                    .and_then(|t| t.failed(c))
                    .map_or_else(
                        || {
                            if idle {
                                "decoding thumbnail…".to_string()
                            } else {
                                "no frames yet".to_string()
                            }
                        },
                        |e| format!("first frame unreadable: {e}"),
                    );
                ui.painter().text(
                    rect.center(),
                    egui::Align2::CENTER_CENTER,
                    why,
                    egui::FontId::proportional(11.0),
                    DIM,
                );
            }
            if idle && resp.double_clicked() {
                *open = Some(i);
            }
        });
    ui.horizontal_wrapped(|ui| {
        let mut on_box = on;
        if super::theme::checkbox(ui, &mut on_box, "").changed() {
            if on_box {
                sel.insert(i);
            } else {
                sel.remove(&i);
            }
            on = on_box;
        }
        ui.add(egui::Label::new(RichText::new(&c.display_name).strong().size(13.0)).truncate())
            .on_hover_text(&c.display_name);
        ui.label(
            RichText::new(c.state.label())
                .color(Color32::from_rgb(150, 190, 150))
                .size(11.0),
        );
    });
    match &st {
        CardState::Busy(t, f) => {
            ui.add(egui::ProgressBar::new(*f).text(t.as_str()));
        }
        CardState::Waiting => {
            ui.label(RichText::new("Waiting for its turn to extract…").size(11.0).color(WARN));
        }
        CardState::Failed(m) => {
            ui.colored_label(
                Color32::from_rgb(235, 120, 120),
                RichText::new(format!("Extraction failed: {m}")).size(11.0),
            );
        }
        CardState::NeedsFrames => {
            ui.label(RichText::new("Frames not extracted yet. The source movie is safe in this clip's Source folder.").size(11.0).color(WARN));
        }
        CardState::Idle => {}
    }
    if matches!(st, CardState::Idle) {
        let facts = match c.sample_size {
            Some((w, h)) => format!("{}×{} · {:.0} fps · {} fr", w, h, c.fps, c.input_files.len()),
            None => format!("{:.0} fps · {} fr", c.fps, c.input_files.len()),
        };
        ui.label(RichText::new(facts).color(DIM).size(10.5));
        ui.label(RichText::new(board_status(c)).color(DIM).size(10.5));
    }
    // One Open affordance (the thumbnail also opens on double-click) and the clip's client on the same row.
    ui.horizontal_wrapped(|ui| {
        let r = ui
            .add_enabled(idle, egui::Button::new("Open").small())
            .on_disabled_hover_text("Frames are not ready yet")
            .on_hover_text("Open this clip (or double-click its thumbnail)");
        open_rects.push((Some(i), r.rect));
        if r.clicked() {
            *open = Some(i);
        }
        if matches!(st, CardState::NeedsFrames | CardState::Failed(_))
            && ui.small_button("Resume").clicked()
        {
            *resume = Some(i);
        }
        if idle {
            ai.client_combo(ui, c);
        }
    });
}

/// One short line per clip, from what the load already read (no directory listings per frame): the generated
/// masks, the clip state and whether a result was recorded.
fn board_status(c: &Clip) -> String {
    let mut parts: Vec<String> = vec![];
    if c.alpha_frames > 0 {
        parts.push(format!("Hint {}", c.alpha_frames));
    }
    if c.mask_frames > 0 {
        parts.push(format!("Guide {}", c.mask_frames));
    }
    if !c.keyframes.is_empty() {
        parts.push(format!("{} marks", c.keyframes.len()));
    }
    match &c.manifest {
        Some(m) if m.complete => parts.push("result recorded".into()),
        Some(_) => parts.push("partial result".into()),
        None => parts.push("no result yet".into()),
    }
    parts.join(" · ")
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.net_logic(ctx);
        // eframe skips ui() for hidden/occluded Windows windows, but keeps calling logic().
        if let Some(dir) = &self.opts.instance_dir {
            if std::fs::remove_file(dir.join("instance.show")).is_ok() {
                self.opts.start_background = false;
                crate::background::show(ctx);
            }
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        if self.opts.start_background {
            if ctx.cumulative_frame_nr() > 0 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
                self.opts.start_background = false;
            }
            ctx.request_repaint();
        }
    }

    fn on_exit(&mut self) {
        self.release_shared();
        if let Some(d) = &self.driver {
            d.stop();
        }
        self.metrics.stop(); // kill + reap the helper
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        use egui::{Event, Modifiers, PointerButton, Pos2};
        // The release lands two frames after the press: egui opens a context menu on the release, and a
        // release in that very same frame would count as the click that closes it again.
        if let Some(pos) = self.inject_release {
            if self.inject_release_wait > 0 {
                self.inject_release_wait -= 1;
                return;
            }
            self.inject_release = None;
            raw.events.push(Event::PointerButton {
                pos,
                button: PointerButton::Secondary,
                pressed: false,
                modifiers: Modifiers::NONE,
            });
            return;
        }
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
        // `text:abc` types into the focused field (QA: forms such as pairing).
        if let Some(t) = tok.strip_prefix("text:") {
            raw.events.push(Event::Text(t.to_owned()));
            return;
        }
        // `click:XxY` = primary, `rclick:XxY` = secondary (QA evidence for context menus).
        if let Some((button, xy)) = tok
            .strip_prefix("rclick:")
            .map(|xy| (PointerButton::Secondary, xy))
            .or_else(|| tok.strip_prefix("click:").map(|xy| (PointerButton::Primary, xy)))
        {
            let (x, y) = xy.split_once('x').unwrap_or(("0", "0"));
            let pos = Pos2::new(x.parse().unwrap_or(0.0), y.parse().unwrap_or(0.0));
            raw.events.push(Event::PointerMoved(pos));
            raw.events.push(Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers: Modifiers::NONE,
            });
            if button == PointerButton::Secondary {
                self.inject_release = Some(pos);
            } else {
                raw.events.push(Event::PointerButton {
                    pos,
                    button,
                    pressed: false,
                    modifiers: Modifiers::NONE,
                });
            }
            return;
        }
        let (mods, name) = match (tok.strip_prefix("Shift+"), tok.strip_prefix("Ctrl+")) {
            (Some(n), _) => (Modifiers::SHIFT, n),
            (_, Some(n)) => (Modifiers::COMMAND, n),
            _ => (Modifiers::NONE, tok.as_str()),
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
        // ponytail: IME kept off. egui toggles it off/on on every focus change, and some fcitx5 / Windows IME
        // setups then stop delivering typed text until restart. Plain key text still works (layouts, dead keys);
        // CJK composition does not. Revisit when egui-winit/winit fix the toggle.
        ui.ctx().output_mut(|o| o.ime = None);
    }
}

impl App {
    fn import_tick(&mut self, ctx: &egui::Context) {
        let out = self.imp.poll(ctx);
        if let Some(p) = out.reload {
            self.pending_reload = Some(p);
        }
        if let Some(r) = self.picker.poll() {
            if r.0 == PickKind::MappingRoot {
                self.settings.browse_done(r.1.into_iter().next());
            } else {
                self.held_pick = Some(r);
            }
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
                // A reload clears drafts and output location: save them first, or keep the old project open.
                let old = self.project.as_ref().map(|o| o.root.clone()).filter(|r| *r != p);
                let saved = match old {
                    Some(_) if self.unsaved() => self.save_project(),
                    _ => Ok(()),
                };
                if let (Err(e), Some(old)) = (saved, old) {
                    self.ai.notice = Some(format!(
                        "New project created at {}, but {} was kept open: {e}. Open it with File > Open project.",
                        p.display(),
                        old.display()
                    ));
                    self.reload_refused = true;
                } else {
                    self.path_input = p.display().to_string();
                    self.reload_keep_selection(ctx);
                }
            }
            if let Some(name) = out.open_clip {
                // A refused switch leaves the old project open: never open the new project's clip name in it.
                if !std::mem::take(&mut self.reload_refused) {
                    self.pending_open = Some(name);
                }
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
                if self.dialog_active() {
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
        self.picker.pending().is_some() || self.held_pick.is_some() || self.new_project_draft.is_some()
    }

    /// Open a native dialog off the UI thread. False when one is already open or an import is running.
    pub fn request_pick(&mut self, ctx: &egui::Context, kind: PickKind) -> bool {
        let dest = matches!(kind, PickKind::Location | PickKind::AddTo);
        // The New project name window is a dialog too: nothing else may start behind it.
        if self.new_project_draft.is_some() {
            return false;
        }
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

    /// Clients and Settings buttons: one global pair, shown in the Board and the workspace bar (not per clip).
    fn clients_button(&mut self, ui: &mut Ui) -> egui::Rect {
        let r = ui
            .add_enabled(
                !self.settings_open && !self.models_open && !self.clients_open,
                egui::Button::new("Clients"),
            )
            .on_hover_text("Render clients: machines, connection, models, what each is running");
        if r.clicked() {
            if let Some(v) = &mut self.viewer {
                v.pause();
            }
            self.clients_open = true;
        }
        self.clients_rect = Some(r.rect);
        r.rect
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
    /// The open clip as the panels see it (test hook).
    pub fn clip_for_test(&self) -> Clip {
        self.clip().cloned().expect("a clip is open")
    }

    /// Switch projects exactly as the "Open project" flow does (test hook: one AI session, two projects).
    pub fn open_project_for_test(&mut self, ctx: &egui::Context, path: &std::path::Path) {
        self.path_input = path.display().to_string();
        self.load(ctx);
    }

    pub fn viewer_mut_for_test(&mut self, f: impl FnOnce(&mut Viewer)) {
        if let Some(v) = &mut self.viewer {
            f(v);
        }
    }
    /// The name window's Create button and the folder it will create in (tests).
    pub fn new_project_create_rect(&self) -> Option<egui::Rect> {
        self.new_project_create_rect
    }
    pub fn new_project_parent(&self) -> Option<&std::path::Path> {
        self.new_project_draft.as_ref().map(|d| d.0.as_path())
    }
    pub fn new_menu_rects(&self) -> [Option<egui::Rect>; 2] {
        self.new_menu_rects
    }
    pub fn settings_open(&self) -> bool {
        self.settings_open
    }
    /// GPUs (NVML/PCI index, the worker's `--gpu`) that a local worker is running a job on right now.
    fn active_gpus(&self) -> Vec<u32> {
        self.ai
            .scheduler()
            .targets()
            .iter()
            .filter(|t| t.local && t.busy_with.is_some())
            .filter_map(|t| t.hello.as_ref()?.gpu.as_ref().map(|g| g.index))
            .collect()
    }

    /// Per frame: drive the network client (connect, stats, Batch submissions, receiving) without blocking.
    fn net_tick(&mut self, now: std::time::Instant, wake: &std::sync::Arc<dyn Fn() + Send + Sync>) {
        self.setup.ensure(self.settings.saved(), self.settings.store_path());
        self.setup.tick(self.net_repo.as_deref(), self.net_python.as_deref(), wake);
        if !self.settings_open {
            if let Some(j) = self.setup.take_joined() {
                self.net_joined(j);
            }
            let ups = self.setup.take_updates();
            if !ups.is_empty() {
                self.net_scan_updates(ups);
            }
        }
        if !self.remote.available() {
            let view = self.metrics.view(now);
            self.ai.dispatch_auto(&[], &view); // no network: Automatic means this computer
            return;
        }
        // The driver ticks without the UI's settings, so Leave results are finished here either way.
        for (creds, how) in self.remote.take_left() {
            let name = self.conn_name(&creds);
            let tail = match how {
                crate::remote::Leave::Retired => None,
                crate::remote::Leave::TooOld => Some(" (that PC runs an older Keyloom)"),
                crate::remote::Leave::Failed => Some(""),
            };
            self.finish_remove(&creds, tail.map(|t| format!("{name} still lists this PC; retire it from that PC's Clients page{t}.")));
        }
        if self.driver.is_none() {
            let view = self.metrics.view(now);
            let mut env = crate::remote::Env {
                settings: self.settings.saved(),
                settings_path: self.settings.store_path(),
                ai: &mut self.ai,
                metrics: &view,
                repo: self.net_repo.as_deref(),
                python: self.net_python.as_deref(),
                wake,
                cfg: Default::default(),
            };
            self.remote.tick(now, &mut env);
            for f in self.remote.take_finished() {
                if let Some(ctx) = self.last_ctx.clone() {
                    self.refresh_clip(&ctx, &f);
                }
            }
        }
    }

    /// Save the edited network settings and hand them to the driver.
    fn net_save(&mut self, what: &str) {
        if self.settings.apply() {
            if let Some(d) = &self.driver {
                d.publish(self.settings.saved(), self.settings.store_path());
            }
        } else {
            self.ai.notice = Some(format!("{what}, but the connection settings could not be saved."));
        }
    }

    /// Open the settings draft; edits touch only the connection list entries they name.
    fn net_draft(&mut self) -> &mut crate::settings::Network {
        self.settings.open();
        &mut self.settings.draft_mut().network
    }

    /// A join finished: keep every existing connection and session; only an explicit Save changes the global name.
    /// This PC's own service is received on by default; a chosen peer is only a render target (receive off).
    fn net_joined(&mut self, j: crate::network_setup::Joined) {
        let infos = self.remote.infos();
        let known = infos.iter().any(|c| c.credentials == j.creds || (j.network_id.is_some() && c.network_id == j.network_id));
        let n = self.net_draft();
        let found = n.connections.iter().position(|c| c.credentials == j.creds || (!c.network_id.is_empty() && Some(&c.network_id) == j.network_id.as_ref() && (j.host_installation_id.is_none() || c.host_installation_id.is_empty() || Some(&c.host_installation_id) == j.host_installation_id.as_ref())))
            .or_else(|| infos.iter().find(|i| i.network_id.is_some() && i.network_id == j.network_id).and_then(|i| n.connections.iter().position(|c| c.credentials == i.credentials)));
        let idx = match found {
            Some(i) => i,
            None if !known => {
                n.connections.push(crate::settings::Connection { credentials: j.creds.clone(), receive: j.local_host, ..Default::default() });
                n.connections.len() - 1
            }
            None => return,
        };
        let c = &mut n.connections[idx];
        c.enabled = true; // rejoining a saved (possibly unticked) connection connects it
        if let Some(id) = &j.network_id {
            if c.network_id.is_empty() {
                c.network_id.clone_from(id);
            }
        }
        if !j.endpoints.is_empty() {
            c.endpoints = j.endpoints.clone();
        }
        if j.local_host {
            c.local_host = true;
        } else if c.host_installation_id.is_empty() {
            if let Some(h) = &j.host_installation_id {
                c.host_installation_id.clone_from(h); // a new peer connection targets exactly this host
            }
        }
        if c.host_name.is_empty() {
            c.host_name = j.host_name;
        }
        self.net_save("Joined");
    }

    /// Fresh scan addresses for saved connections (matched by full fingerprint, never by name). A legacy connection
    /// keeps targeting every member; one that already names a host is corrected to the host the scan shows.
    fn net_scan_updates(&mut self, ups: Vec<crate::network_setup::ScanUpdate>) {
        let infos = self.remote.infos();
        let conns = self.settings.saved().network.connections();
        let nid = |c: &crate::settings::Connection| if c.network_id.is_empty() { infos.iter().find(|i| i.credentials == c.credentials).and_then(|i| i.network_id.clone()).unwrap_or_default() } else { c.network_id.clone() };
        let edits: Vec<_> = conns.iter().filter(|c| !c.local_host).filter_map(|c| {
            let id = nid(c);
            let u = ups.iter().find(|u| u.network_id.eq_ignore_ascii_case(&id) && (c.host_installation_id.is_empty() || u.host_iid.as_deref().is_none_or(|h| h.is_empty() || h == c.host_installation_id)))?;
            let mut n = c.clone();
            n.network_id = id;
            if !u.endpoints.is_empty() {
                n.endpoints = u.endpoints.clone();
            }
            if n.host_name.is_empty() {
                n.host_name.clone_from(&u.name);
            }
            (n != *c).then_some(n)
        }).collect();
        if edits.is_empty() {
            return;
        }
        let list = &mut self.net_draft().connections;
        for e in edits {
            if let Some(c) = list.iter_mut().find(|c| c.credentials == e.credentials) {
                *c = e;
            }
        }
        self.net_save("Addresses refreshed");
    }

    /// A Clients row checkbox: switches a saved connection on or off (files, attempts and jobs are never touched).
    fn net_use_pc(&mut self, credentials: &str, network_id: &str, host_iid: Option<String>, name: &str, endpoints: Vec<String>, on: bool) {
        if let Some(c) = self.net_draft().connections.iter_mut().find(|c| c.credentials == credentials) {
            c.enabled = on;
            if c.network_id.is_empty() {
                c.network_id = network_id.to_owned();
            }
            if !endpoints.is_empty() {
                c.endpoints = endpoints;
            }
            if c.host_name.is_empty() {
                c.host_name = name.to_owned();
            }
            if let (false, Some(h)) = (c.host_installation_id.is_empty(), host_iid) {
                c.host_installation_id = h; // metadata exists: point it at the exact host the scan shows
            }
        }
        self.net_save("Updated");
    }

    fn conn_name(&mut self, creds: &str) -> String {
        let n = self.net_draft().connections.iter().find(|c| c.credentials == creds).map(|c| c.host_name.clone()).unwrap_or_default();
        if n.is_empty() { "That PC".into() } else { n }
    }

    /// The Remove confirmation; Remove asks the host to retire this PC's login first when online.
    fn remove_ui(&mut self, ctx: &egui::Context) {
        let Some(creds) = self.remove_confirm.clone() else { return };
        let name = self.conn_name(&creds);
        let (mut yes, mut no) = (false, false);
        egui::Window::new("Remove")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!("Remove {name}? This PC's login on it is retired; finished jobs stay in History."));
                ui.horizontal(|ui| {
                    yes = ui.button("Remove").clicked();
                    no = ui.button("Cancel").clicked();
                });
            });
        if !(yes || no) {
            return;
        }
        self.remove_confirm = None;
        if !yes {
            return;
        }
        if let Some(why) = self.remote.remove_blocker(&creds) {
            self.ai.notice = Some(format!("Connection not removed: {why}."));
        } else if !self.remote.leave(&creds) {
            self.finish_remove(&creds, Some(format!("{name} still lists this PC; retire it from that PC's Clients page.")));
        }
    }

    /// Drop the settings entry and the session, save, then delete the login file and its side files (never history or the attempt file, never the own service's login).
    fn finish_remove(&mut self, creds: &str, notice: Option<String>) {
        let n = self.net_draft();
        let own = n.connections.iter().any(|c| c.credentials == creds && c.local_host);
        n.connections.retain(|c| c.credentials != creds);
        if n.default_submit == creds {
            n.default_submit.clear();
        }
        self.remote.remove(creds);
        self.net_save("Removed");
        if !own {
            for ext in ["", ".session", ".lock", ".network.json"] {
                let _ = std::fs::remove_file(format!("{creds}{ext}"));
            }
        }
        if notice.is_some() {
            self.ai.notice = notice;
        }
    }

    fn net_action(&mut self, a: crate::remote::Action) {
        use crate::remote::Action;
        match a {
            Action::Enable { creds, on } => {
                if let Some(c) = self.net_draft().connections.iter_mut().find(|c| c.credentials == creds) {
                    c.enabled = on;
                }
                self.net_save("Updated");
            }
            Action::Receive { creds, on } => {
                if let Some(c) = self.net_draft().connections.iter_mut().find(|c| c.credentials == creds) {
                    c.receive = on;
                }
                self.net_save("Updated");
            }
            Action::DefaultSubmit(creds) => {
                self.net_draft().default_submit = creds;
                self.net_save("Updated");
            }
            Action::Remove(creds) => self.remove_confirm = Some(creds),
            Action::CancelJob { creds, job_id } => self.remote.cancel_job(&creds, &job_id),
            Action::ForgetAttempt(creds) => self.remote.forget_attempt(&creds),
            Action::Prefer { creds, url } => {
                if let Some(c) = self.net_draft().connections.iter_mut().find(|c| c.credentials == creds) {
                    c.preferred = url;
                }
                self.net_save("Updated");
            }
        }
    }

    /// The footer's rectangle from the last frame (layout tests).
    pub fn metrics_rect(&self) -> Option<egui::Rect> {
        self.metrics_rect
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
    pub fn clients_rect(&self) -> Option<egui::Rect> {
        self.clients_rect
    }
    pub fn clients_open(&self) -> bool {
        self.clients_open
    }
    pub fn clients_back_rect(&self) -> Option<egui::Rect> {
        self.clients_back_rect
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
                self.open_project_path(ctx, paths[0].clone());
            }
            PickKind::Location => {
                self.imp.set_dest(ctx, Dest::NewIn(paths[0].clone()));
            }
            PickKind::AddTo => {
                self.imp.set_dest(ctx, Dest::Add(paths[0].clone()));
            }
            PickKind::NewProject => self.ask_project_name(paths[0].clone()),
            PickKind::HintMovie => self.hint_picked(paths[0].clone(), true),
            PickKind::ExportFolder => self.hint_picked(paths[0].clone(), false),
            PickKind::Relink => self.relink_picked(ctx, paths[0].clone()),
            PickKind::RelinkMovie => self.movie_picked(paths[0].clone()),
            PickKind::HintSequence => self.sequence_picked(paths[0].clone()),
            PickKind::MappingRoot => {} // answered inside Settings (see import_tick)
        }
    }

    /// "Open clip settings" button rectangles from the last Board frame: `Some(i)` = card `i`.
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
    /// Tools panel, status region and action region above the graph from the last frame (layout tests).
    pub fn tools_rects(&self) -> Option<[egui::Rect; 3]> {
        self.tools_rects
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
    /// Prompt tools of the Hint page's vertical strip, with their screen rects (layout tests).
    pub fn tool_strip_rects(&self) -> &[(super::annotate::Tool, egui::Rect)] {
        &self.hint.strip_rects
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
    /// Between frames (`logic`) the network client still ticks. It must take the shared objects in the driver's
    /// order (ai, remote, metrics) and hand them all back: a lone lazy `remote` lock kept until the next frame's
    /// `ai.hold()` deadlocks against the driver, which holds ai and waits for remote.
    fn net_logic(&mut self, ctx: &egui::Context) {
        if self.driver.is_none() {
            return;
        }
        self.ai.hold();
        self.remote.hold();
        self.metrics.hold();
        if self.remote.available() {
            let wake = self.metrics_wake.get_or_insert_with(|| {
                let c = ctx.clone();
                std::sync::Arc::new(move || c.request_repaint())
            }).clone();
            self.net_tick(std::time::Instant::now(), &wake);
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }
        self.release_shared();
    }

    /// Hand the shared runtime objects back to the driver.
    fn release_shared(&mut self) {
        self.ai.release();
        self.remote.release();
        self.metrics.release();
    }

    pub fn frame(&mut self, ui: &mut Ui) {
        // Take the shared objects in the driver's lock order for the whole frame, give them back at its end.
        self.ai.hold();
        self.remote.hold();
        self.metrics.hold();
        if let Some(d) = &self.driver {
            d.publish(self.settings.saved(), self.settings.store_path());
        }
        self.frame_inner(ui);
        self.release_shared();
    }

    fn frame_inner(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.import_tick(&ctx);
        self.ai
            .set_model_mutation_active(self.models.blocks_worker_launch());
        self.ai_tick(&ctx);
        self.last_ctx = Some(ctx.clone());
        self.persist_media();
        self.hint_pending_refresh();
        self.hint_sync();
        self.feedback_tick();
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
        let now = std::time::Instant::now();
        let wake = self
            .metrics_wake
            .get_or_insert_with(|| {
                let c = ctx.clone();
                std::sync::Arc::new(move || c.request_repaint())
            })
            .clone();
        if self.driver.is_none() {
            self.metrics.tick(now, &wake);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs(1)); // ages (stale) advance without any new sample
        if self.driver.is_none() {
            self.net_tick(now, &wake);
        }
        let active = self.active_gpus();
        let cells = crate::metrics::view_cells(&self.metrics.view(now), &active);
        // Outermost bottom panel: on every page (Board, clip, Settings, Clients, Models), fixed height.
        let metrics_panel = Panel::bottom("system_metrics")
            .exact_size(super::metrics_bar::FOOTER_H)
            .frame(egui::Frame::new().fill(theme::CANVAS).inner_margin(egui::Margin::symmetric(8, 0)))
            .show(ui, |ui| super::metrics_bar::footer(ui, &cells));
        self.metrics_rect = Some(metrics_panel.response.rect);
        let mut open_models = false;
        Panel::top("banner").show(ui, |ui| {
            self.chrome(ui);
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
                let ev = self.settings.ui(ui);
                if let Some((_, dir)) = self.settings.take_browse() {
                    let c = ctx.clone();
                    if !self.picker.start(PickKind::MappingRoot, dir, move || c.request_repaint()) {
                        self.settings.set_error("Another file dialog is already open; close it first".into());
                        self.settings.browse_done(None);
                    }
                }
                match ev {
                    super::settings::Event::Applied => {
                        let i = &self.settings.saved().interface;
                        if let Err(e) = crate::background::sync_login(i.launch_at_login) {
                            self.settings.set_error(format!("Settings saved, but the sign-in entry could not be updated: {e}"));
                        }
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
        } else if self.clients_open {
            Panel::top("clients_nav").show(ui, |ui| {
                let back = ui.button("‹ Back");
                self.clients_back_rect = Some(back.rect);
                if back.clicked() {
                    self.clients_open = false;
                }
            });
            if self.clients_open {
                let default = self.ai.placement.clone();
                let mut net_actions = vec![];
                let mut setup_cmds = vec![];
                let local_view = self.metrics.view(std::time::Instant::now());
                let active_now = self.active_gpus();
                egui::CentralPanel::default().show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false; 2])
                        .show(ui, |ui| {
                            if self.remote.available() {
                                let infos = self.remote.infos();
                                let cx = crate::network_setup::Ctx {
                                    settings: self.settings.saved(),
                                    settings_path: self.settings.store_path(),
                                    connected: self.remote.online(),
                                    connections: &infos,
                                };
                                setup_cmds = self.setup.ui(ui, &cx);
                                ui.add_space(14.0);
                            }
                            let (own_net, own_iid) = self.setup.svc().map(|v| (v.network_id.clone(), v.host_installation_id.clone())).unwrap_or_default();
                            let (placement, table_acts) = super::clients::ui(ui, &mut self.ai, &default, &local_view, &active_now, &self.remote, self.settings.saved(), self.settings.store_path(), own_net.as_deref(), own_iid.as_deref());
                            if let Some(p) = placement {
                                self.ai.placement = p;
                            }
                            net_actions.extend(table_acts);
                            if self.remote.available() {
                                self.setup.installs_ui(ui);
                            }
                        });
                });
                for a in net_actions {
                    self.net_action(a);
                }
                for c in setup_cmds {
                    match c {
                        crate::network_setup::Cmd::SaveName(n) => {
                            self.net_draft().name = n;
                            self.net_save("Name changed");
                        }
                        crate::network_setup::Cmd::AllowJoin(allow) => {
                            self.net_draft().allow_join = allow;
                            self.net_save("Updated");
                        }
                        crate::network_setup::Cmd::UsePc { credentials, network_id, host_iid, name, endpoints, on } => {
                            self.net_use_pc(&credentials, &network_id, host_iid, &name, endpoints, on);
                        }
                        crate::network_setup::Cmd::HostRemoved(gone) => {
                            let n = self.net_draft();
                            n.host_removed = gone;
                            if gone {
                                n.connections.iter_mut().filter(|c| c.local_host).for_each(|c| c.enabled = false);
                            } else {
                                n.connections.iter_mut().filter(|c| c.local_host).for_each(|c| c.enabled = true);
                            }
                            self.net_save("Updated");
                        }
                    }
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
        self.remove_ui(&ctx);
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

    fn project_with_clip(root: &std::path::Path) {
        std::fs::create_dir_all(root.join("clips/a/Frames")).unwrap();
        std::fs::write(root.join("project.json"), br#"{"version":2}"#).unwrap();
        std::fs::write(root.join("clips/a/.video_metadata.json"), br#"{"fps":24.0}"#).unwrap();
        let mut e = png::Encoder::new(std::fs::File::create(root.join("clips/a/Frames/f_0.png")).unwrap(), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[0u8; 48]).unwrap();
    }

    #[test]
    fn new_project_from_an_open_clip_returns_to_the_batch_and_keeps_every_clip_draft() {
        let (repo, _log) = stub_repo("newclip");
        let cur = repo.parent().unwrap().join("cur");
        project_with_clip(&cur);
        let parent = repo.parent().unwrap().to_path_buf();
        let backend: Backend = std::sync::Arc::new(move |_, _| vec![parent.clone()]);
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions { repo: Some(repo.clone()), project: Some(cur.clone()), ..AppOptions::default() },
            backend,
        );
        pump(&ctx, &mut app, |_| true);
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |a| a.in_workspace() && a.draft_clips.contains("a"));
        // An edited draft stays "unsaved" after leaving the clip (the guard must not forget it).
        app.draft_saved.insert("a".into(), serde_json::json!({"stale": true}));
        app.back_requested = true;
        pump(&ctx, &mut app, |a| !a.in_workspace());
        assert!(app.unsaved(), "closed clip's unsaved draft still counts");
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |a| a.in_workspace());
        assert!(app.new_project_pick(&ctx));
        pump(&ctx, &mut app, |a| !a.in_workspace() && a.new_project_parent().is_some());
        assert!(!app.unsaved(), "saved before leaving");
        assert_eq!(app.project_root(), Some(cur.as_path()));
        drop(app);
        let _ = std::fs::remove_dir_all(repo.parent().unwrap());
    }

    #[test]
    fn a_new_project_reload_saves_edits_first_or_keeps_the_old_project_with_a_notice() {
        use std::os::unix::fs::PermissionsExt;
        let (repo, _log) = stub_repo("reload");
        let base = repo.parent().unwrap().to_path_buf();
        let (cur, fresh) = (base.join("cur"), base.join("fresh"));
        project_with_clip(&cur);
        project_with_clip(&fresh);
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions { repo: Some(repo.clone()), project: Some(cur.clone()), ..AppOptions::default() },
            std::sync::Arc::new(|k, _| panic!("no dialog ({k:?})")),
        );
        pump(&ctx, &mut app, |_| true);
        app.loc.root = "/edited".into();
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o500)).unwrap();
        app.pending_reload = Some(fresh.clone());
        pump(&ctx, &mut app, |_| true);
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(app.project_root(), Some(cur.as_path()), "old project kept");
        assert!(app.unsaved());
        let n = app.ai.notice.clone().unwrap();
        assert!(n.contains(&fresh.display().to_string()) && n.contains("Open project"), "{n}");
        // With a working save the edit is flushed to disk, then the new project loads.
        app.pending_reload = Some(fresh.clone());
        pump(&ctx, &mut app, |a| a.project_root() == Some(fresh.as_path()));
        assert!(std::fs::read_to_string(cur.join("project.json")).unwrap().contains("/edited"));
        drop(app);
        let _ = std::fs::remove_dir_all(&base);
    }

    fn edit_draft(app: &mut App, v: f64) {
        let c = app.clip_for_test();
        let p = app.project.as_ref().unwrap();
        app.ai.draft(&c, p, app.settings.saved()).params.despill_strength = v;
    }
    fn draft_despill(app: &mut App) -> f64 {
        let c = app.project.as_ref().unwrap().clips[0].clone();
        let p = app.project.as_ref().unwrap();
        app.ai.draft(&c, p, app.settings.saved()).params.despill_strength
    }
    fn app_on(repo: &std::path::Path, root: &std::path::Path) -> (egui::Context, App) {
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions { repo: Some(repo.into()), project: Some(root.into()), ..AppOptions::default() },
            std::sync::Arc::new(|k, _| panic!("no dialog ({k:?})")),
        );
        pump(&ctx, &mut app, |_| true);
        (ctx, app)
    }

    #[test]
    fn switching_projects_with_a_same_named_clip_never_carries_the_old_draft() {
        let (repo, _log) = stub_repo("xproj");
        let base = repo.parent().unwrap().to_path_buf();
        let (a, b) = (base.join("A"), base.join("B"));
        project_with_clip(&a);
        project_with_clip(&b);
        let (ctx, mut app) = app_on(&repo, &a);
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |x| x.draft_clips.contains("a"));
        edit_draft(&mut app, 0.123);
        pump(&ctx, &mut app, |_| true);
        assert!(app.unsaved());
        app.open_project_for_test(&ctx, &b);
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |x| x.draft_clips.contains("a"));
        assert_ne!(draft_despill(&mut app), 0.123, "B shows its own default, not A's edit");
        assert!(!app.unsaved(), "B is clean against its own baseline");
        drop(app);
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn a_same_project_reload_keeps_unsaved_drafts_dirty_and_saveable() {
        let (repo, _log) = stub_repo("sameroot");
        let a = repo.parent().unwrap().join("A");
        project_with_clip(&a);
        let (ctx, mut app) = app_on(&repo, &a);
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |x| x.draft_clips.contains("a"));
        edit_draft(&mut app, 0.123);
        app.back_requested = true;
        pump(&ctx, &mut app, |x| !x.in_workspace());
        app.pending_reload = Some(a.clone());
        pump(&ctx, &mut app, |_| true);
        assert!(app.unsaved(), "the reload must not turn an unsaved draft into Saved");
        assert_eq!(draft_despill(&mut app), 0.123);
        app.save_project().unwrap();
        pump(&ctx, &mut app, |_| true);
        assert!(!app.unsaved());
        let meta = std::fs::read_to_string(a.join("clips/a/clip.json")).unwrap();
        assert!(meta.contains("0.123"), "{meta}");
        drop(app);
        let _ = std::fs::remove_dir_all(a.parent().unwrap());
    }

    #[test]
    fn a_refused_switch_blocks_opening_the_new_projects_clip_in_the_old_one() {
        use std::os::unix::fs::PermissionsExt;
        let (repo, _log) = stub_repo("refuse");
        let base = repo.parent().unwrap().to_path_buf();
        let (cur, fresh) = (base.join("cur"), base.join("fresh"));
        project_with_clip(&cur);
        project_with_clip(&fresh);
        let (ctx, mut app) = app_on(&repo, &cur);
        app.loc.root = "/edited".into();
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o500)).unwrap();
        app.pending_reload = Some(fresh.clone());
        pump(&ctx, &mut app, |_| true);
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(app.reload_refused && app.project_root() == Some(cur.as_path()));
        app.pending_reload = Some(fresh.clone());
        pump(&ctx, &mut app, |x| x.project_root() == Some(fresh.as_path()));
        assert!(!app.reload_refused, "an accepted switch clears the refusal");
        drop(app);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Click Create once the name window has settled (an anchored egui Window moves after its first frames).
    fn click_create(ctx: &egui::Context, app: &mut App) {
        for _ in 0..4 {
            pump(ctx, app, |_| true);
        }
        let at = app.new_project_create_rect().expect("name window drawn").center();
        click(ctx, app, at);
    }

    fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
        let b = |pressed| egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        for events in [vec![egui::Event::PointerMoved(at)], vec![b(true)], vec![b(false)], vec![]] {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                events,
                ..Default::default()
            };
            ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
        }
    }

    /// An app whose New project dialog answers `picked` (None = cancelled); any other dialog is a bug.
    fn new_project_app(tag: &str, project: Option<&str>, picked: Option<&str>) -> (egui::Context, App, PathBuf, PathBuf) {
        let (repo, log) = stub_repo(&format!("np-{tag}"));
        let base = repo.parent().unwrap().to_path_buf();
        let cur = base.join("cur");
        let parent = base.join("where");
        std::fs::create_dir_all(&parent).unwrap();
        if project.is_some() {
            project_with_clip(&cur);
        }
        let answer: Vec<PathBuf> = picked.map(|_| parent.clone()).into_iter().collect();
        let backend: Backend = std::sync::Arc::new(move |k, _| {
            assert_eq!(k, PickKind::NewProject, "only the New project folder dialog may open");
            answer.clone()
        });
        let ctx = egui::Context::default();
        let mut app = App::with_picker(
            &ctx,
            AppOptions { repo: Some(repo), project: project.map(|_| cur.clone()), ..AppOptions::default() },
            backend,
        );
        pump(&ctx, &mut app, |_| true);
        (ctx, app, cur, log)
    }

    #[test]
    fn file_menu_new_project_from_an_open_clip_creates_and_opens_an_empty_project() {
        let (ctx, mut app, cur, log) = new_project_app("menu", Some("clip"), Some("go"));
        app.open_clip(&ctx, 0);
        pump(&ctx, &mut app, |a| a.in_workspace());
        let at = app.new_menu_rects()[0].expect("File drawn").center();
        click(&ctx, &mut app, at);
        let at = app.new_menu_rects()[1].expect("New project… in the open File menu").center();
        click(&ctx, &mut app, at);
        pump(&ctx, &mut app, |a| !a.in_workspace() && a.new_project_parent().is_some());
        assert_eq!(app.project_root(), Some(cur.as_path()), "old project stays open until Create");
        click_create(&ctx, &mut app);
        let made = cur.parent().unwrap().join("where/New project");
        pump(&ctx, &mut app, |a| a.project_root() == Some(made.as_path()));
        assert_eq!(app.project.as_ref().unwrap().clips.len(), 0);
        assert!(app.loc.root.as_os_str().is_empty(), "the previous project's output folder is not inherited");
        assert!(!app.unsaved());
        let pj: serde_json::Value = serde_json::from_slice(&std::fs::read(made.join("project.json")).unwrap()).unwrap();
        assert_eq!((pj["version"].as_i64(), pj["clips"].as_array().map(Vec::len)), (Some(2), Some(0)));
        assert!(made.join("clips").is_dir() && std::fs::read_dir(made.join("clips")).unwrap().next().is_none());
        assert!(cur.join("clips/a/Frames/f_0.png").is_file(), "previous project untouched");
        assert!(calls(&log).iter().all(|c| c.contains("backend.worker")), "only the clip worker ran: {:?}", calls(&log));
        assert!(!app.in_workspace() && !app.imp.has_pending() && !app.dialog_active());
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
    }

    #[test]
    fn new_project_from_the_welcome_page_needs_no_open_project() {
        let (ctx, mut app, cur, log) = new_project_app("cold", None, Some("go"));
        assert!(app.project_root().is_none() && app.request_pick(&ctx, PickKind::NewProject));
        pump(&ctx, &mut app, |a| a.new_project_parent().is_some());
        click_create(&ctx, &mut app);
        let made = cur.parent().unwrap().join("where/New project");
        pump(&ctx, &mut app, |a| a.project_root() == Some(made.as_path()));
        assert!(app.project.as_ref().unwrap().clips.is_empty() && started(&log).is_empty());
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
    }

    #[test]
    fn cancelled_new_project_keeps_the_open_project_and_creates_nothing() {
        let (ctx, mut app, cur, _log) = new_project_app("cancel", Some("clip"), None);
        assert!(app.new_project_pick(&ctx));
        pump(&ctx, &mut app, |a| !a.picking() && !a.dialog_active());
        assert_eq!(app.project_root(), Some(cur.as_path()));
        assert!(app.new_project_parent().is_none());
        assert!(std::fs::read_dir(cur.parent().unwrap().join("where")).unwrap().next().is_none());
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
    }

    #[test]
    fn new_project_is_refused_when_unsaved_work_cannot_be_saved() {
        use std::os::unix::fs::PermissionsExt;
        let (ctx, mut app, cur, _log) = new_project_app("dirty", Some("clip"), Some("go"));
        app.loc.root = "/somewhere/else".into();
        assert!(app.unsaved());
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert!(!app.new_project_pick(&ctx));
        std::fs::set_permissions(&cur, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(!app.picking() && app.unsaved() && app.project_root() == Some(cur.as_path()));
        assert!(app.ai.notice.as_deref().unwrap().contains("Not starting a new project"));
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
    }

    #[test]
    fn the_name_window_blocks_every_other_dialog_and_import_start() {
        let (ctx, mut app, cur, log) = new_project_app("modal", Some("clip"), Some("go"));
        assert!(app.new_project_pick(&ctx));
        pump(&ctx, &mut app, |a| a.new_project_parent().is_some());
        // The backend panics on any kind but NewProject, so a false here also means no dialog was opened.
        for k in [PickKind::Clips, PickKind::Folder, PickKind::Project, PickKind::NewProject] {
            assert!(!app.request_pick(&ctx, k), "{k:?}");
        }
        assert!(!app.new_project_pick(&ctx) && !app.picking());
        dropped(&ctx, &mut app, &cur);
        pump(&ctx, &mut app, |_| true);
        assert!(!app.imp.has_pending() && !app.imp.busy(), "a drop must not start an import behind the window");
        assert!(app.new_project_parent().is_some() && app.project_root() == Some(cur.as_path()));
        assert!(started(&log).is_empty());
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
    }

    #[test]
    fn creating_over_a_populated_folder_is_refused_and_keeps_the_open_project() {
        let (ctx, mut app, cur, log) = new_project_app("exists", Some("clip"), Some("go"));
        let taken = cur.parent().unwrap().join("where/New project");
        std::fs::create_dir_all(&taken).unwrap();
        std::fs::write(taken.join("precious.txt"), b"keep").unwrap();
        assert!(app.new_project_pick(&ctx));
        pump(&ctx, &mut app, |a| a.new_project_parent().is_some());
        click_create(&ctx, &mut app);
        pump(&ctx, &mut app, |a| a.new_project_error.is_some());
        assert!(app.new_project_error.as_deref().unwrap().contains("already exists"));
        assert_eq!(app.project_root(), Some(cur.as_path()));
        assert!(app.new_project_parent().is_some(), "window stays so another name can be typed");
        assert_eq!(std::fs::read(taken.join("precious.txt")).unwrap(), b"keep");
        assert!(!taken.join("project.json").exists() && started(&log).is_empty());
        for bad in ["", "  ", ".", "..", "a/b", "a\\b"] {
            assert!(crate::project::create_empty_project(taken.parent().unwrap(), bad).is_err(), "{bad:?}");
        }
        drop(app);
        let _ = std::fs::remove_dir_all(cur.parent().unwrap());
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

#[cfg(test)]
mod lock_order_tests {
    use super::*;

    /// `logic` runs between frames while the driver ticks. It used to leave `remote` locked until the next frame's
    /// `ai.hold()`, the reverse of the driver's order: the UI and the driver then waited on each other forever.
    #[test]
    fn logic_never_keeps_a_shared_lock_between_frames() {
        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, AppOptions { net: crate::netlink::Source::Command(vec!["/bin/false".into()]), ..Default::default() });
        app.release_shared();
        let parts = driver::Parts {
            ai: app.ai.shared(),
            remote: app.remote.shared(),
            metrics: app.metrics.shared(),
            repo: None,
            python: None,
            ctx: ctx.clone(),
        };
        app.driver = Some(driver::Driver::start(parts, driver::Snapshot::default()).unwrap());
        let t0 = std::time::Instant::now();
        while t0.elapsed() < std::time::Duration::from_secs(2) {
            app.net_logic(&ctx);
            assert!(!app.ai.is_held() && !app.remote.is_held() && !app.metrics.is_held());
            app.ai.hold(); // what `frame` does next: must not wait on a driver that waits on us
            app.remote.hold();
            app.metrics.hold();
            app.release_shared();
        }
    }
}
