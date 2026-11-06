//! Screenshot fixture page for the Shot workspace. Fixture data is read-only.
#[path = "shot_header.rs"]
mod header;
#[path = "shot_viewer.rs"]
mod viewer;
#[path = "shot_transport.rs"]
mod transport;
#[path = "shot_dock.rs"]
mod dock;
#[path = "shot_props.rs"]
mod props;
pub use props::{
    ActState, BarAct, BarProps, Choice, ChromaProps, CleanupProps, CompanionProps, DeliverAct,
    DeliverOutputProps, DeliverPresetProps, DeliverProps, DeliverVideoProps, HintAct, HintProps,
    HintTool, KeyAct, KeyOverrides, KeyProps, LiveProps, MachineChoice, MarkedProps, MediaAct,
    MediaProps, MovieProps, PathProp, PropAct, ReadyChoice, RelinkProps, RelinkState, ReviewAct,
    ReviewProps, RouteProps, StageProps,
};

use eframe::egui::{self, Align, ColorImage, Rect, TextureHandle, TextureOptions, Ui, UiBuilder, Vec2};
use serde::Deserialize;
use std::{io::Cursor, path::Path};

use super::{controls::viewer_toolbar::{Background, Channel, Overlay, View, ViewerState}, tokens::tokens};

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
pub struct Fixture {
    pub project: String,
    pub shot: String,
    pub stage: String,
    /// Processing stage for the status chip; `stage` is the selected workspace tab.
    pub clip_stage: String,
    pub view: String,
    pub viewer_state: String,
    pub overlay: String,
    pub channel: String,
    pub background: String,
    /// "Fit" or a numeric percentage string such as "38%".
    pub zoom: String,
    pub focus: bool,
    pub drawer: bool,
    pub dock_tab: String,
    pub dock_expanded: bool,
    pub empty: bool,
    pub wipe: bool,
    pub busy_percent: Option<u8>,
    pub blocker: Option<Blocker>,
    pub first: i64,
    pub last: i64,
    pub frame: i64,
    pub fps: f64,
    pub in_point: Option<i64>,
    pub out_point: Option<i64>,
    pub marks: Vec<i64>,
    pub cached: Vec<(i64, i64)>,
    pub pixel_xy: (i32, i32),
    pub pixel_rgba: [f32; 4],
    pub properties: serde_json::Value,
    pub cards: Vec<Card>,
    /// Live page: the App draws the real picture and timeline; fixtures leave this false.
    pub live: bool,
    /// Live Properties values (stage rows + action bar); fixtures leave this `None`.
    #[serde(skip)]
    pub live_props: Option<props::LiveProps>,
    pub playing: bool,
    pub reverse: bool,
    #[serde(default = "yes")]
    pub loop_on: bool,
    /// Explicit frame count; None = last - first + 1.
    pub frame_count: Option<i64>,
    /// Header text for the running job; None = no job.
    pub job_label: Option<String>,
    /// Identifies the shot; `Page::update` resets presentation state when it changes.
    pub shot_id: String,
    #[serde(default = "default_format")]
    pub format: String,
    /// Live zoom readout; None = fixture behaviour (3200-px picture assumption).
    pub zoom_percent: Option<f64>,
}

fn yes() -> bool { true }
fn default_format() -> String { "EXR · half · 3200×1800".into() }

impl Default for Fixture {
    fn default() -> Self {
        Self {
            project: "Night_exterior_greenscreen".into(),
            shot: "V1-0062_A003C001_261012_R4LF".into(),
            stage: "Key".into(),
            clip_stage: "done".into(),
            view: "Comp".into(),
            viewer_state: "comp".into(),
            overlay: "Off".into(),
            channel: "RGB".into(),
            background: "Grey".into(),
            zoom: "Fit".into(),
            focus: false,
            drawer: false,
            dock_tab: "Actions".into(),
            dock_expanded: false,
            empty: false,
            wipe: false,
            busy_percent: None,
            blocker: None,
            first: 0,
            last: 64,
            frame: 12,
            fps: 24.0,
            in_point: Some(0),
            out_point: Some(64),
            marks: vec![14, 30],
            cached: vec![(0, 22)],
            pixel_xy: (1204, 388),
            pixel_rgba: [0.612, 0.574, 0.482, 0.740],
            properties: serde_json::Value::Null,
            cards: Vec::new(),
            live: false,
            live_props: None,
            playing: false,
            reverse: false,
            loop_on: true,
            frame_count: None,
            job_label: None,
            shot_id: String::new(),
            format: default_format(),
            zoom_percent: None,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Blocker {
    pub message: String,
    pub action: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Card {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub meta: String,
    pub state: String,
    pub percent: Option<u8>,
    pub can_cancel: bool,
    pub can_reveal: bool,
    pub can_delete: bool,
    /// Extra menu item labels.
    pub menu: Vec<String>,
}

/// Dock tabs as the page sees them (mirrors `dock::DockTab`, which is private to this module).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockTab {
    Actions,
    Graph,
}

/// Card button or menu item as the page sees it (mirrors `dock::CardAction`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CardAction {
    Cancel,
    Reveal,
    Menu(String),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Regions {
    pub header: Option<Rect>,
    pub viewer_column: Rect,
    pub viewer_toolbar: Rect,
    pub viewer: Rect,
    pub picture: Rect,
    pub transport: Rect,
    pub timeline: Rect,
    pub dock_tabs: Option<Rect>,
    pub dock_body: Option<Rect>,
    pub splitter: Option<Rect>,
    pub properties: Option<Rect>,
    pub properties_header: Option<Rect>,
    pub properties_body: Option<Rect>,
    pub properties_action: Option<Rect>,
    pub drawer: Option<Rect>,
}

/// Fixed Shot page geometry for the Wide, Medium and Compact rules in bible §7.2/§7.5.
pub fn regions(viewport: Rect, focus: bool, drawer_open: bool) -> Regions {
    let wide = viewport.width() >= 1600.0;
    let compact = viewport.width() < 1200.0;
    let show_properties = !focus && drawer_open;
    let header = (!focus).then(|| Rect::from_min_size(viewport.min, Vec2::new(viewport.width(), 32.0)));
    let work_top = viewport.top() + if focus { 0.0 } else { 32.0 };
    let work = Rect::from_min_max(egui::pos2(viewport.left(), work_top), viewport.max);
    let props_width = if wide { 320.0 } else { 280.0 };
    let viewer_width = (viewport.width() - if show_properties { props_width + 1.0 } else { 0.0 }).max(0.0);
    let viewer_column = Rect::from_min_size(work.min, Vec2::new(viewer_width, work.height()));
    let splitter = show_properties.then(|| Rect::from_min_size(
        egui::pos2(viewer_column.right(), work.top()), Vec2::new(1.0, work.height()),
    ));
    let properties = show_properties.then(|| Rect::from_min_size(
        egui::pos2(viewport.right() - props_width, work.top()), Vec2::new(props_width, work.height()),
    ));
    let dock_body_h = if focus || compact { 0.0 } else if wide { 176.0 } else { 116.0 };
    let dock_h = if focus { 0.0 } else { 24.0 + dock_body_h };
    let viewer_h = (work.height() - 28.0 - 28.0 - 28.0 - dock_h).max(0.0);
    let toolbar = Rect::from_min_size(work.min, Vec2::new(viewer_width, 28.0));
    let viewer = Rect::from_min_size(egui::pos2(work.left(), toolbar.bottom()), Vec2::new(viewer_width, viewer_h));
    let transport = Rect::from_min_size(egui::pos2(work.left(), viewer.bottom()), Vec2::new(viewer_width, 28.0));
    let timeline = Rect::from_min_size(egui::pos2(work.left(), transport.bottom()), Vec2::new(viewer_width, 28.0));
    let dock_tabs = (!focus).then(|| Rect::from_min_size(egui::pos2(work.left(), timeline.bottom()), Vec2::new(viewer_width, 24.0)));
    let dock_body = (!focus && dock_body_h > 0.0).then(|| Rect::from_min_size(
        egui::pos2(work.left(), timeline.bottom() + 24.0), Vec2::new(viewer_width, dock_body_h),
    ));
    let picture = fitted_picture(viewer);
    let properties_header = properties.map(|r| Rect::from_min_size(r.min, Vec2::new(r.width(), 28.0)));
    let properties_action = properties.map(|r| Rect::from_min_size(
        egui::pos2(r.left(), r.bottom() - 72.0), Vec2::new(r.width(), 72.0),
    ));
    let properties_body = properties.map(|r| Rect::from_min_max(
        egui::pos2(r.left(), r.top() + 28.0), egui::pos2(r.right(), r.bottom() - 72.0),
    ));
    let drawer = (compact && drawer_open).then_some(properties).flatten();
    Regions {
        header, viewer_column, viewer_toolbar: toolbar, viewer, picture, transport, timeline,
        dock_tabs, dock_body, splitter, properties, properties_header, properties_body,
        properties_action, drawer,
    }
}

pub fn fitted_picture(viewer: Rect) -> Rect {
    let width = viewer.width().min(viewer.height() * 16.0 / 9.0).round();
    let height = (width * 9.0 / 16.0).round();
    Rect::from_center_size(viewer.center(), Vec2::new(width, height))
}

pub(super) fn part(ui: &mut Ui, rect: Rect, id: &'static str) -> Ui {
    let mut child = ui.new_child(UiBuilder::new()
        .max_rect(rect)
        .layout(egui::Layout::top_down(Align::Min))
        .id_salt(("shot", id)));
    child.set_clip_rect(rect);
    child
}

/// What the page asks the App to do. Props acts are added by later batches.
#[derive(Clone, Debug)]
pub enum Act {
    Stage(&'static str),
    ToggleDrawer,
    Transport(super::controls::transport::Action),
    ViewChanged(ViewerState),
    Fit,
    OneToOne,
    ZoomTo(f64),
    DockTab(DockTab),
    DockExpanded(bool),
    Card(String, CardAction),
    GraphFit,
    GraphZoom(f32),
    Props(props::PropAct),
}

pub struct Page {
    pub fx: Fixture,
    layout: Option<Regions>,
    viewer_state: ViewerState,
    zoom_fit: bool,
    drawer_open: bool,
    dock_tab: DockTab,
    dock_expanded: bool,
    graph_body: Option<Rect>,
    plate: Option<TextureHandle>,
}

fn dock_tab(s: &str) -> DockTab {
    if s == "Node graph" { DockTab::Graph } else { DockTab::Actions }
}

fn to_card_action(a: dock::CardAction) -> CardAction {
    match a {
        dock::CardAction::Cancel => CardAction::Cancel,
        dock::CardAction::Reveal => CardAction::Reveal,
        dock::CardAction::Menu(s) => CardAction::Menu(s),
    }
}

impl Page {
    pub fn load(path: &Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fx: Fixture = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut viewer_state = ViewerState::default();
        viewer_state.view = view(&fx.view);
        viewer_state.overlay = overlay(&fx.overlay);
        viewer_state.channel = channel(&fx.channel);
        viewer_state.background = background(&fx.background);
        viewer_state.focus = fx.focus;
        let zoom_fit = fx.zoom.eq_ignore_ascii_case("fit");
        if !zoom_fit { viewer_state.zoom = fx.zoom.trim_end_matches('%').trim().parse().unwrap_or(38.0); }
        Ok(Self {
            drawer_open: fx.drawer, dock_tab: dock_tab(&fx.dock_tab), dock_expanded: fx.dock_expanded, graph_body: None,
            fx, viewer_state, zoom_fit, plate: None, layout: None,
        })
    }

    pub fn new() -> Self {
        Self {
            fx: Fixture { live: true, ..Fixture::default() },
            layout: None, viewer_state: ViewerState::default(), zoom_fit: true, drawer_open: true,
            dock_tab: DockTab::Actions, dock_expanded: false, graph_body: None, plate: None,
        }
    }

    /// Replaces the data; presentation state survives unless the shot changed.
    pub fn update(&mut self, mut fx: Fixture) {
        fx.live = true;
        if fx.shot_id != self.fx.shot_id {
            self.viewer_state = ViewerState::default();
            self.zoom_fit = true;
            self.drawer_open = true;
            self.dock_expanded = false;
        }
        self.fx = fx;
    }

    /// Regions computed by the last `ui` call.
    pub fn layout(&self) -> Option<Regions> { self.layout }

    /// Properties column or drawer visibility, as the header toggle sets it.
    pub fn drawer_open(&self) -> bool { self.drawer_open }
    pub fn toggle_drawer(&mut self) { self.drawer_open = !self.drawer_open; }
    /// Toolbar snapshot from the live viewer; Focus and the zoom readout stay page-owned.
    pub fn sync_viewer_state(&mut self, s: ViewerState) {
        self.viewer_state = ViewerState { focus: self.viewer_state.focus, zoom: self.viewer_state.zoom, ..s };
    }
    pub fn focus(&self) -> bool { self.viewer_state.focus }
    pub fn set_focus(&mut self, on: bool) { self.viewer_state.focus = on; }
    pub fn dock_expanded(&self) -> bool { self.dock_expanded }
    pub fn collapse_dock(&mut self) { self.dock_expanded = false; }
    pub fn set_dock_tab(&mut self, t: DockTab) { self.dock_tab = t; }
    pub fn set_dock_expanded(&mut self, b: bool) { self.dock_expanded = b; }

    /// Where the App draws the live node graph; None unless the Graph tab is open in live mode.
    pub fn graph_rect(&self) -> Option<Rect> { self.graph_body }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Act> {
        let mut acts = Vec::new();
        let viewport = ui.ctx().content_rect();
        let full = Rect::from_min_max(
            egui::pos2(viewport.left(), viewport.top() + 60.0),
            egui::pos2(viewport.right(), viewport.bottom() - 22.0),
        )
        // Never lay out below/above what the caller's panel really gives (live chrome heights differ from the 60/22 assumed here).
        .intersect(ui.clip_rect());
        let mut layout = regions(full,self.viewer_state.focus, self.drawer_open);
        let t = tokens(ui);
        ui.painter().with_clip_rect(full).rect_filled(full, 0.0, t.bg_panel);
        if let Some(header_rect) = layout.header {
            let out = header::draw(ui, header_rect, &self.fx, self.drawer_open);
            if out.toggle_drawer { self.toggle_drawer(); acts.push(Act::ToggleDrawer); }
            if let Some(stage) = out.stage { acts.push(Act::Stage(stage)); }
        }
        if let Some(splitter) = layout.splitter { ui.painter().rect_filled(splitter, 0.0, t.border_divider); }
        // The page's own dock state is the source of truth; the dock reads it from the fixture fields.
        self.fx.dock_tab = (if self.dock_tab == DockTab::Graph { "Node graph" } else { "Actions" }).into();
        self.fx.dock_expanded = self.dock_expanded;
        let mut dock_out = dock::DockOut::default();
        if let Some(dock_tabs) = layout.dock_tabs { dock_out = dock::draw(ui, dock_tabs, layout.dock_body, &self.fx); }
        self.graph_body = if self.fx.live { dock_out.graph_body } else { None };
        if let Some(tab) = dock_out.tab {
            self.dock_tab = if tab == dock::DockTab::Graph { DockTab::Graph } else { DockTab::Actions };
            acts.push(Act::DockTab(self.dock_tab));
        }
        if let Some(expanded) = dock_out.expand {
            self.dock_expanded = expanded;
            acts.push(Act::DockExpanded(expanded));
        }
        if let Some((id, action)) = dock_out.card_action { acts.push(Act::Card(id, to_card_action(action))); }
        if dock_out.graph_fit { acts.push(Act::GraphFit); }
        if let Some(z) = dock_out.graph_zoom { acts.push(Act::GraphZoom(z)); }
        if let Some(properties) = layout.properties { acts.extend(props::draw(ui, properties, &self.fx).into_iter().map(Act::Props)); }

        if let (true, Some(z)) = (self.fx.live, self.fx.zoom_percent) {
            self.viewer_state.zoom = z;
        } else if self.zoom_fit && !self.fx.live {
            self.viewer_state.zoom = (layout.picture.width() / 3200.0 * 100.0) as f64;
        }
        let mut toolbar_ui = part(ui, layout.viewer_toolbar, "viewer-toolbar");
        let out = viewer::toolbar(&mut toolbar_ui, &mut self.viewer_state, full.width(), &self.fx.format);
        if out.changed { acts.push(Act::ViewChanged(self.viewer_state.clone())); }
        if out.fit { acts.push(Act::Fit); }
        if out.one_to_one { acts.push(Act::OneToOne); }
        if let Some(z) = out.zoom_to { acts.push(Act::ZoomTo(z)); }
        if out.fit { self.zoom_fit = true; }
        else if out.zoom_to.is_some() || out.one_to_one { self.zoom_fit = false; }

        if !self.zoom_fit {
            let size = Vec2::new(3200.0, 1800.0) * (self.viewer_state.zoom as f32 / 100.0);
            layout.picture = Rect::from_center_size(layout.viewer.center(), size);
        } else {
            layout.picture = fitted_picture(layout.viewer);
        }
        if !self.fx.live {
            if self.plate.is_none() { self.plate = Some(load_plate(ui.ctx())); }
            viewer::picture(ui, layout.viewer, layout.picture, self.plate.as_ref().unwrap(), &self.viewer_state, &self.fx);
        }
        acts.extend(transport::draw(ui, layout.transport, layout.timeline, &self.fx).into_iter().map(Act::Transport));
        self.layout = Some(layout);
        acts
    }
}

fn view(s: &str) -> View {
    match s { "Hint" => View::Hint, "Mask" => View::Mask, "FG" => View::Fg, "Matte" => View::Matte, "Comp" => View::Comp, "Processed" => View::Processed, _ => View::Input }
}
fn overlay(s: &str) -> Overlay {
    match s { "Pre-matte" => Overlay::PreMatte, "Hint" => Overlay::Hint, "SAM" => Overlay::Sam, _ => Overlay::Off }
}
fn channel(s: &str) -> Channel {
    match s { "R" => Channel::R, "G" => Channel::G, "B" => Channel::B, "A" => Channel::A, "Luma" => Channel::Luma, _ => Channel::Rgb }
}
fn background(s: &str) -> Background {
    match s { "Black" => Background::Black, "White" => Background::White, "Checkerboard" => Background::Checker, _ => Background::Grey }
}

fn load_plate(ctx: &egui::Context) -> TextureHandle {
    let bytes = include_bytes!("../../tests/fixtures/shot-plate.png");
    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().expect("shot-plate.png is a valid fixture");
    let mut data = vec![0; reader.output_buffer_size().expect("shot plate fits in memory")];
    let info = reader.next_frame(&mut data).expect("shot-plate.png decodes");
    let data = &data[..info.buffer_size()];
    let rgba: Vec<u8> = match reader.output_color_type().0 {
        png::ColorType::Rgba => data.to_vec(),
        png::ColorType::Rgb => data.chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        png::ColorType::GrayscaleAlpha => data.chunks_exact(2).flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        png::ColorType::Grayscale => data.iter().flat_map(|&v| [v, v, v, 255]).collect(),
        png::ColorType::Indexed => unreachable!("EXPAND converts indexed PNGs"),
    };
    let image = ColorImage::from_rgba_unmultiplied([info.width as usize, info.height as usize], &rgba);
    ctx.load_texture("shot-placeholder-plate", image, TextureOptions::LINEAR)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    #[test]
    fn update_keeps_drawer_for_same_shot_and_resets_for_new() {
        let mut page = Page::new();
        let fx = Fixture { shot_id: "a".into(), ..Fixture::default() };
        page.update(fx.clone());
        page.drawer_open = true;
        page.viewer_state.zoom = 77.0;
        page.update(fx.clone());
        assert!(page.drawer_open && page.viewer_state.zoom == 77.0);
        page.update(Fixture { shot_id: "b".into(), ..fx });
        assert!(page.viewer_state.zoom == ViewerState::default().zoom);
    }

    #[test]
    fn shot_page_regions_match_all_three_sizes_and_focus() {
        let wide = regions(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 998.0)), false, true);
        assert_eq!(wide.viewer.size(), vec2(1599.0, 682.0));
        assert_eq!(wide.picture.size(), vec2(1212.0, 682.0));
        assert_eq!(wide.properties_header.unwrap().height(), 28.0);
        assert_eq!(wide.properties_body.unwrap().height(), 866.0);
        assert_eq!(wide.properties_action.unwrap().height(), 72.0);

        let medium = regions(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 818.0)), false, true);
        assert_eq!(medium.viewer.size(), vec2(1159.0, 562.0));
        assert_eq!(medium.picture.size(), vec2(999.0, 562.0));
        assert_eq!(medium.dock_body.unwrap().height(), 116.0);
        let medium_hidden = regions(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 818.0)), false, false);
        assert!(medium_hidden.properties.is_none());

        let compact = regions(Rect::from_min_size(pos2(0.0, 0.0), vec2(900.0, 558.0)), false, true);
        assert_eq!(compact.viewer.size(), vec2(619.0, 418.0));
        assert_eq!(compact.picture.size(), vec2(619.0, 348.0));
        assert_eq!(compact.properties_body.unwrap().height(), 426.0);
        assert!(compact.dock_body.is_none());

        let focus = regions(Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 998.0)), true, false);
        assert_eq!(focus.viewer.size(), vec2(1920.0, 914.0));
        assert_eq!(focus.picture.size(), vec2(1625.0, 914.0));
        assert!(focus.header.is_none() && focus.properties.is_none() && focus.dock_tabs.is_none());
    }

    #[test]
    fn properties_toggle_changes_wide_and_medium_layout() {
        let viewport = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 818.0));
        let ctx = egui::Context::default();
        let mut page = Page::new();
        assert!(regions(viewport, false, page.drawer_open()).properties.is_some());
        let click = pos2(1421.0, 76.0);
        let frame = |page: &mut Page, events: Vec<egui::Event>| {
            let raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0))), events, ..Default::default() };
            let mut acts = vec![];
            let mut output = ctx.run_ui(raw, |ui| acts = page.ui(ui));
            output.textures_delta.clear();
            acts
        };
        frame(&mut page, vec![egui::Event::PointerMoved(click)]);
        frame(&mut page, vec![egui::Event::PointerButton { pos: click, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        let acts = frame(&mut page, vec![egui::Event::PointerButton { pos: click, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        assert!(acts.iter().any(|a| matches!(a, Act::ToggleDrawer)));
        assert!(regions(viewport, false, page.drawer_open()).properties.is_none());
        frame(&mut page, vec![egui::Event::PointerMoved(click)]);
        frame(&mut page, vec![egui::Event::PointerButton { pos: click, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        let acts = frame(&mut page, vec![egui::Event::PointerButton { pos: click, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
        assert!(acts.iter().any(|a| matches!(a, Act::ToggleDrawer)));
        assert!(regions(viewport, false, page.drawer_open()).properties.is_some());
    }
}
