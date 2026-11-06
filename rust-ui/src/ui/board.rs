//! Board page (bible §7.3 grid, §8.1 card, §8.6 empty state, §5.10 shot menu): toolbar, card grid, room for the Queue.
//!
//! `--board-fixture FILE.json` (hidden, screenshots only, never written) fills it with synthetic data. The file is the
//! page data of `bible/pages/tools/gen_board.py` as JSON, so phase 2 (Queue panel, batch dialog) reads the same fields:
//!
//! ```text
//! { project, saved (0/1), activity ("" = idle), notices, running, qtab ("machines"|"waiting"),
//!   cards:    [{ n name, st raw|mask|ready|done, meta, s status line, pc machine ("" none, "Automatic",
//!                "This machine" or a machine name), dot, job ""|q|r|p|f, prog 0..100, err 0/1,
//!                q/r (batch dialog: queued / running) }],
//!   machines: [{ name, local, state busy|idle|offline, job, gpu, vram, cpu, ram (%), vramt, ramt (text) }],
//!   waiting:  [card index],  sel: [card index],  hov: card index|null,
//!   menu: card index|null (shot menu open),  dialog: 0/1 (batch dialog open),  drawer: 0/1 }
//! ```
use super::controls::{
    buttons::{self, Kind, Vis},
    card::{self, CardData, Machine},
    checkbox::Check,
    chips::{Job, Stage},
    dialog::{self, EmptyButton},
    dropdown::{Dropdown, Item},
    line,
    menus::{MenuItem, MenuList},
    paint_line, scroll,
    statusbar::{ActivityIcon, Meters, StatusData},
};
use super::{
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, pos2, vec2, Align, Id, Key, Order, Rect, Sense, Ui, UiBuilder};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct FxShot {
    pub n: String,
    pub st: String,
    pub meta: String,
    pub s: String,
    pub pc: String,
    pub dot: String,
    pub job: String,
    pub prog: f32,
    pub err: u8,
    pub q: u8,
    pub r: u8,
    #[serde(skip)]
    pub thumb: Option<egui::TextureId>,
    #[serde(skip)]
    pub thumb_aspect: f32,
    #[serde(skip)]
    pub thumb_label: String,
    #[serde(skip)]
    pub thumb_tooltip: String,
    #[serde(skip)]
    pub source_available: Option<bool>,
    #[serde(skip)]
    pub exports_available: bool,
    #[serde(skip)]
    pub resume: bool,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct FxMachine {
    #[serde(default)]
    pub id: String,
    pub name: String,
    pub local: u8,
    pub state: String,
    pub job: String,
    pub gpu: f32,
    pub vram: f32,
    pub cpu: f32,
    pub ram: f32,
    pub vramt: String,
    pub ramt: String,
    #[serde(skip)]
    pub telemetry_status: String,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Fixture {
    pub project: String,
    pub saved: u8,
    pub activity: String,
    pub notices: u32,
    pub running: u32,
    #[serde(skip)]
    pub waiting_total: u32,
    #[serde(skip)]
    pub finished: u32,
    #[serde(skip)]
    pub details: bool,
    #[serde(skip)]
    pub details_explicit: bool,
    #[serde(skip)]
    pub notice_text: Vec<String>,
    pub qtab: String,
    pub cards: Vec<FxShot>,
    pub machines: Vec<FxMachine>,
    pub waiting: Vec<usize>,
    pub sel: Vec<usize>,
    pub hov: Option<usize>,
    pub menu: Option<usize>,
    pub dialog: u8,
    pub drawer: u8,
    #[serde(skip)]
    pub live: bool,
    #[serde(skip)]
    pub meters: Option<FxMeters>,
}

#[derive(Clone, Debug, Default)]
pub struct FxMeters {
    pub cpu: f32,
    pub ram_used: f32,
    pub ram_total: f32,
    pub gpu_pct: f32,
    pub gpu_label: String,
    pub vram_used: f32,
    pub vram_total: f32,
}

/// What the user asked for on the page; the caller carries it out (the fixture page ignores all of it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    ImportShots,
    ImportFolder,
    Open(usize),
    Key(usize),
    KeySelected,
    RevealSource(usize),
    RevealExports(usize),
    Remove(usize),
    Assign(usize, usize),
    Select(usize, bool),
    SelectAll(bool),
    Resume(usize),
    Move(usize, usize),
    ClearFinished,
}

/// §7.3: W = grid − 2×16 − 8; n = ⌊(W+12)/236⌋; gap = clamp((W − 224n)/(n−1), 12, 32); the rest splits left/right.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geometry {
    pub cols: usize,
    pub gap: f32,
    pub x0: f32,
}
pub fn geometry(grid_w: f32) -> Geometry {
    let w = grid_w - 40.0;
    let cols = (((w + 12.0) / 236.0).floor() as usize).max(1);
    let gap = if cols > 1 { ((w - cols as f32 * 224.0) / (cols as f32 - 1.0)).floor().clamp(12.0, 32.0) } else { 0.0 };
    let used = cols as f32 * 224.0 + (cols as f32 - 1.0) * gap;
    Geometry { cols, gap, x0: 16.0 + ((w - used) / 2.0).floor() }
}
/// Queue width: 300 Wide, 260 Medium, `None` = the 40 px rail (Compact).
pub fn queue_width(window_w: f32) -> Option<f32> {
    if window_w >= 1600.0 { Some(300.0) } else if window_w >= 1200.0 { Some(260.0) } else { None }
}

pub struct Page {
    pub fx: Fixture,
    queue_w: Option<f32>,
    sel: BTreeSet<usize>,
    menu: Option<usize>,
    hl: usize,
    assign: usize,
    rects: Vec<Rect>,
    queue_rect: Option<Rect>,
    drag_candidate: Option<usize>,
    drag: Option<usize>,
}

const MENU_W: f32 = 250.0;

impl Page {
    pub fn new() -> Self {
        Self { fx: Fixture::default(), queue_w: None, sel: BTreeSet::new(), menu: None, hl: 0, assign: usize::MAX, rects: vec![], queue_rect: None, drag_candidate: None, drag: None }
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fx: Fixture = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Self { sel: fx.sel.iter().copied().collect(), queue_w: None, menu: fx.menu, hl: 0, assign: usize::MAX, rects: vec![], queue_rect: None, drag_candidate: None, drag: None, fx })
    }

    pub fn update(&mut self, mut fx: Fixture, selected: &BTreeSet<usize>) {
        if self.fx.live {
            fx.qtab.clone_from(&self.fx.qtab);
            fx.drawer = self.fx.drawer;
            if self.fx.details_explicit {
                fx.details = self.fx.details;
            }
            fx.details_explicit = self.fx.details_explicit;
        }
        self.sel.clone_from(selected);
        fx.sel = selected.iter().copied().collect();
        self.fx = fx;
        if self.menu.is_some_and(|i| i >= self.fx.cards.len()) { self.menu = None; }
    }

    pub fn selected(&self) -> &BTreeSet<usize> { &self.sel }
    pub fn card_rects(&self) -> &[Rect] { &self.rects }
    pub fn queue_rect(&self) -> Option<Rect> { self.queue_rect }

    pub fn update_global_status(&mut self, activity: String, notices: u32, running: u32, waiting: u32, saved: u8, meters: Option<FxMeters>) {
        self.fx.activity = activity;
        self.fx.notices = notices;
        self.fx.running = running;
        self.fx.waiting_total = waiting;
        self.fx.saved = saved;
        self.fx.meters = meters;
    }

    pub fn status(&self, ui: &mut Ui, failures: u32) -> super::controls::statusbar::StatusOut {
        let w = ui.ctx().content_rect().width();
        let width = if w >= 1600.0 { super::controls::statusbar::Width::Wide } else if w >= 1200.0 { super::controls::statusbar::Width::Medium } else { super::controls::statusbar::Width::Compact };
        let activity = if self.fx.activity.is_empty() { None } else { Some((ActivityIcon::Spinner, self.fx.activity.as_str())) };
        let fixture_meters = Meters { cpu: 7.0, ram_used: 34.7, ram_total: 123.5, gpu_pct: 21.0, gpu_label: "RTX 4090", vram_used: 3.6, vram_total: 24.0 };
        let meters = self.fx.meters.as_ref().map(|m| Meters {
            cpu: m.cpu, ram_used: m.ram_used, ram_total: m.ram_total, gpu_pct: m.gpu_pct,
            gpu_label: &m.gpu_label, vram_used: m.vram_used, vram_total: m.vram_total,
        }).or_else(|| (!self.fx.live).then_some(fixture_meters));
        let data = StatusData {
            activity,
            notices: self.fx.notices,
            failures,
            queue_waiting: Some(if self.fx.live { self.fx.waiting_total } else { self.fx.waiting.len() as u32 }),
            meters,
            start: !self.fx.live || self.fx.project.is_empty(),
            ..Default::default()
        };
        super::controls::statusbar::status_bar(ui, &data, width)
    }

    /// Draws the whole Board into `ui`'s rect (the area between the page tabs and the status bar).
    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Act> {
        self.ui_with_queue(ui, |_, _| {})
    }

    pub fn ui_with_queue(&mut self, ui: &mut Ui, mut render_live_tab: impl FnMut(&mut Ui, &str)) -> Vec<Act> {
        let t = tokens(ui);
        let mut acts = vec![];
        let full = ui.max_rect();
        let window_w = ui.ctx().content_rect().width();
        let qw = queue_width(window_w);
        let max_w = 900.0_f32.min(full.width() - 360.0).max(340.0);
        let side = qw.map(|_| self.queue_w.unwrap_or(540.0).clamp(340.0, max_w)).unwrap_or(40.0);
        let gcol = Rect::from_min_max(full.min, pos2(full.max.x - side - 1.0, full.max.y));
        let queue = Rect::from_min_max(pos2(gcol.max.x + 1.0, full.min.y), full.max);
        let p = ui.painter().clone();
        p.rect_filled(full, 0, t.bg_panel);
        p.rect_filled(Rect::from_min_size(pos2(gcol.max.x, full.min.y), vec2(1.0, full.height())), 0, t.border_divider);

        if qw.is_some() {
            let hit = Rect::from_center_size(pos2(gcol.max.x + 0.5, full.center().y), vec2(6.0, full.height()));
            let response = egui::Area::new(egui::Id::new("board-queue-splitter"))
                .order(egui::Order::Foreground).fixed_pos(hit.min)
                .show(ui.ctx(), |ui| {
                    ui.allocate_exact_size(hit.size(), Sense::click_and_drag()).1
                }).inner;
            if response.hovered() || response.dragged() { ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal); }
            if response.double_clicked() { self.queue_w = None; }
            else if response.dragged() {
                if let Some(pointer) = response.interact_pointer_pos() { self.queue_w = Some((full.max.x - pointer.x).clamp(340.0, max_w)); }
            }
        }

        let n = self.fx.cards.len();
        self.sel.retain(|&i| i < n);
        let Page { fx, sel, menu, hl, assign, rects, queue_rect, drag_candidate, drag, .. } = self;
        *queue_rect = Some(queue);

        // ---- toolbar (32 high, controls at y + 4)
        let tb = Rect::from_min_size(gcol.min, vec2(gcol.width(), 32.0));
        p.rect_filled(Rect::from_min_size(pos2(tb.min.x, tb.max.y - 1.0), vec2(tb.width(), 1.0)), 0, t.border_divider);
        let y = tb.min.y + 4.0;
        let at = |x: f32| pos2(tb.min.x + x, y);
        if ui.add(buttons::icon_button(Icon::Import, buttons::IconButtonSize::Normal22, "Import shots…\nCtrl+I").at(at(8.0))).clicked() { acts.push(Act::ImportShots) }
        if ui.add(buttons::icon_button(Icon::Folder, buttons::IconButtonSize::Normal22, "Import folder…\nCtrl+Shift+I").at(at(34.0))).clicked() { acts.push(Act::ImportFolder) }
        let sep = |x: f32| p.rect_filled(Rect::from_min_size(pos2(tb.min.x + x, tb.min.y + 7.0), vec2(1.0, 16.0)), 0, t.border_divider);
        sep(64.0);
        let s = sel.len();
        let mut state = if n > 0 && s == n { Some(true) } else if s > 0 { None } else { Some(false) };
        let all = if n == 0 { Check::mixed(&mut state, Some("All")).disabled("No shots to select") } else { Check::mixed(&mut state, Some("All")).at(at(73.0)) };
        let all = if n == 0 { all.at(at(73.0)) } else { all };
        let r = ui.add(all);
        if r.changed() {
            let on = state == Some(true);
            if on { sel.extend(0..n) } else { sel.clear() }
            acts.push(Act::SelectAll(on));
        }
        let cnt = if n == 0 { "No shots".to_owned() } else { format!("{s} of {n} selected") };
        let g = line(ui.painter(), &cnt, ty::small(), 14.0, f32::INFINITY);
        let cw = g.size().x.ceil();
        let cx = r.rect.max.x.ceil() - tb.min.x + 8.0;
        paint_line(&p, Rect::from_min_size(at(cx), vec2(cw, 22.0)), g, 14.0, Align::Min, if n == 0 { t.text_tertiary } else { t.text_secondary }, 0.0);
        let mut x = cx + cw + 8.0;
        sep(x);
        x += 9.0;
        let compact = qw.is_none();
        let assign_items: Vec<Item> = std::iter::once(Item::new("Automatic").icon(Icon::Refresh))
            .chain(fx.machines.iter().map(|m| {
                let it = Item::new(&m.name).icon(if m.local != 0 { Icon::Machine } else { Icon::MachineNetwork });
                it.dot(if m.state == "offline" { t.error } else { t.success })
            }))
            .collect();
        if compact && !fx.live {
            let wbtn = 89.0;
            let rect = Rect::from_min_size(at(x), vec2(wbtn, 22.0));
            let resp = ui.interact(rect, Id::unique("board-selection"), if s > 0 { Sense::click() } else { Sense::hover() });
            let vis = if s > 0 && resp.is_pointer_button_down_on() { Vis::Pressed } else if s > 0 && resp.hovered() { Vis::Hover } else { Vis::Rest };
            let l = buttons::look(t, Kind::Ghost, vis, false, s > 0);
            if let Some(f) = l.fill { p.rect_filled(rect, 3, f); }
            if let Some(b) = l.border { p.rect_stroke(rect, 3, egui::Stroke::new(1.0, b), egui::StrokeKind::Inside); }
            let g = line(&p, "Selection", ty::button(), 16.0, f32::INFINITY);
            let gw = g.size().x;
            let x0 = (rect.center().x - (gw + 6.0 + 12.0) / 2.0).round();
            paint_line(&p, Rect::from_min_size(pos2(x0, rect.min.y), vec2(gw, 22.0)), g, 16.0, Align::Min, l.text, 0.0);
            icons::paint(&p, Rect::from_min_size(pos2(x0 + gw + 6.0, rect.center().y - 6.0), vec2(12.0, 12.0)), Icon::ChevronDown, IconSize::S12, l.icon);
        } else if compact {
            let mut pick = *assign;
            let dd = Dropdown::new("board-selection", &mut pick, &assign_items, 89.0)
                .name("Selection").display("Selection").compact()
                .disabled_if(s == 0, "Select shots first").at(at(x));
            if dd.show(ui).changed() { for &k in sel.iter() { acts.push(Act::Assign(k, pick)) } }
            *assign = usize::MAX;
        } else {
            let label = if s == 0 { "Assign selected to".to_owned() } else if s == n { format!("Assign all {n} shots to") } else { format!("Assign {s} selected to") };
            let mut pick = *assign;
            let dd = Dropdown::new("board-assign", &mut pick, &assign_items, 160.0).name("Assign selected to").placeholder(&label).disabled_if(s == 0, "Select shots first").tip("Applies on pick").at(at(x));
            if dd.show(ui).changed() {
                for &k in sel.iter() { acts.push(Act::Assign(k, pick)) }
            }
            *assign = usize::MAX;
            if fx.live {
                let mut icon_pick = *assign;
                let dd = Dropdown::new("board-assign-icon", &mut icon_pick, &assign_items, 42.0)
                    .name("Assign selected shots").display("").compact().list_width(220.0)
                    .disabled_if(s == 0, "Select shots first").at(at(x + 164.0));
                let response = dd.show(ui);
                icons::paint(ui.painter(), Rect::from_min_size(response.rect.min + vec2(3.0, 3.0), vec2(16.0, 16.0)), Icon::Assign, IconSize::S16, if s == 0 { t.text_disabled } else { t.text_secondary });
                if response.changed() { for &k in sel.iter() { acts.push(Act::Assign(k, icon_pick)) } }
                *assign = usize::MAX;
            } else {
                let ib = buttons::icon_button(Icon::Assign, buttons::IconButtonSize::Normal22, "Assign selected shots\nCtrl+Shift+A").disabled_if(s == 0, "Select shots first").at(at(x + 164.0));
                ui.add(ib);
            }
        }
        let label = format!("Key selected ({s})…");
        let probe = buttons::primary_button(&label);
        let kw = probe.natural_width(ui).max(72.0).round();
        let key = buttons::primary_button(&label).disabled_if(s == 0, "Select shots to key").tooltip("Key selected\nCtrl+Enter").at(pos2(tb.max.x - 8.0 - kw, y));
        if ui.add(key).clicked() { acts.push(Act::KeySelected) }

        // ---- grid
        let notice_h = if fx.notice_text.is_empty() { 0.0 } else { 24.0 };
        if !fx.notice_text.is_empty() {
            let mut seen = std::collections::HashSet::new();
            let note = fx.notice_text.iter().filter(|s| seen.insert(s.as_str())).cloned().collect::<Vec<_>>().join(" · ");
            let rect = Rect::from_min_size(pos2(gcol.min.x + 16.0, gcol.min.y + 34.0), vec2((gcol.width() - 32.0).max(0.0), 20.0));
            ui.put(rect, egui::Label::new(egui::RichText::new(&note).color(t.warning)).truncate()).on_hover_text(note);
        }
        let grid = Rect::from_min_max(pos2(gcol.min.x, gcol.min.y + 32.0 + notice_h), gcol.max);
        rects.clear();
        let mut gui = ui.new_child(UiBuilder::new().max_rect(grid).layout(egui::Layout::top_down(Align::Min)));
        gui.set_clip_rect(grid.intersect(ui.clip_rect()));
        if n == 0 {
            // §8.6 block: 360 wide, left = round((grid_w − 360) / 2); empty_state centres within the rect it gets.
            let left = ((grid.width() - 360.0) / 2.0).round();
            let mut block = gui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(grid.min + vec2(left, 0.0), vec2(360.0, grid.height()))).layout(egui::Layout::top_down(Align::Min)));
            let clicked = dialog::empty_state(&mut block, Icon::Import, "No shots yet", "Import video files or a folder of image sequences to make shots.", Some(EmptyButton { label: "Import shots…", primary: true }));
            if clicked { acts.push(Act::ImportShots) }
        } else {
            let geo = geometry(grid.width());
            let rows = n.div_ceil(geo.cols);
            let content_h = 32.0 + rows as f32 * 260.0 + (rows as f32 - 1.0) * 12.0;
            let items: Vec<Item> = assign_items.clone();
            scroll::scroll_v(&mut gui, "board-grid", |ui| {
                let origin = ui.cursor().min;
                ui.allocate_space(vec2(ui.available_width(), content_h));
                for (k, c) in fx.cards.iter().enumerate() {
                    let (row, col) = (k / geo.cols, k % geo.cols);
                    let rect = Rect::from_min_size(origin + vec2(geo.x0 + col as f32 * (224.0 + geo.gap), 16.0 + row as f32 * 272.0), card::SIZE);
                    rects.push(rect);
                    if !ui.is_rect_visible(rect) { continue }
                    let selected_machine = if c.pc.is_empty() { items.len() } else { items.iter().position(|i| i.label == c.pc).unwrap_or(items.len()) };
                    let offline = c.pc != "Automatic" && fx.machines.iter().any(|m| m.name == c.pc && m.state == "offline");
                    let tip = if c.pc.is_empty() { "Assign a machine".to_owned() } else if c.pc == "Automatic" { "Automatic\nRuns on the next free machine".to_owned() } else if offline { format!("{}\nOffline", c.pc) } else { c.pc.clone() };
                    let data = CardData {
                        name: &c.n,
                        thumb: c.thumb,
                        thumb_aspect: c.thumb_aspect,
                        thumb_label: &c.thumb_label,
                        thumb_tooltip: &c.thumb_tooltip,
                        specimen: !fx.live,
                        stage: if matches!(c.job.as_str(), "q" | "r" | "p") { Stage::Ready } else { match c.st.as_str() { "mask" => Stage::Mask, "ready" => Stage::Ready, "done" => Stage::Completed, _ => Stage::Raw } },
                        job: match c.job.as_str() { "q" => Some(Job::Queued), "r" => Some(Job::Rendering(c.prog / 100.0)), "p" => Some(Job::Preparing), "f" => Some(Job::Failed), _ => None },
                        meta: &c.meta,
                        status_line: &c.s,
                        blocked: c.err != 0,
                        machine: Machine { items: &items, selected: selected_machine, tooltip: &tip },
                        selected: sel.contains(&k),
                        hovered: fx.hov == Some(k),
                        plate_variant: k % 8,
                        ..Default::default()
                    };
                    let mut cui = ui.new_child(UiBuilder::new().id_salt(("board-card", k)).max_rect(rect));
                    let out = card::shot_card(&mut cui, &data);
                    if out.select_toggled {
                        let on = if sel.remove(&k) { false } else { sel.insert(k); true };
                        acts.push(Act::Select(k, on));
                    }
                    if out.open { acts.push(Act::Open(k)) }
                    if let Some(m) = out.machine_changed { acts.push(Act::Assign(k, m)) }
                    if out.context_menu { *menu = Some(k); *hl = 0; }
                }
            });

            fx.sel = sel.iter().copied().collect();
            let (pressed, down, released, pos, origin) = ui.ctx().input(|i| (
                i.pointer.button_pressed(egui::PointerButton::Primary),
                i.pointer.button_down(egui::PointerButton::Primary),
                i.pointer.button_released(egui::PointerButton::Primary),
                i.pointer.latest_pos(),
                i.pointer.press_origin(),
            ));
            if pressed && menu.is_none() {
                *drag_candidate = pos.and_then(|p| rects.iter().position(|r| Rect::from_min_size(r.min, vec2(224.0, 126.0)).contains(p)));
            }
            if down && drag.is_none() {
                if let (Some(i), Some(pos), Some(origin)) = (*drag_candidate, pos, origin) {
                    if pos.distance(origin) >= 5.0 { *drag = Some(i); }
                }
            }
            if let (Some(src), Some(pos)) = (*drag, pos) {
                let (slot, near) = drop_slot(rects, pos);
                if let Some(r) = rects.get(near) {
                    let x = if slot == near { r.min.x - 5.0 } else { r.max.x + 5.0 };
                    ui.painter().line_segment([pos2(x, r.min.y), pos2(x, r.max.y)], egui::Stroke::new(3.0, ui.visuals().widgets.noninteractive.fg_stroke.color));
                }
                if let Some(c) = fx.cards.get(src) {
                    let lp = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, ui.make_persistent_id("board_drag_ghost")));
                    let ink = ui.style().visuals.text_color();
                    let g = lp.layout_no_wrap(c.n.clone(), egui::FontId::proportional(12.0), ink);
                    let r = Rect::from_min_size(pos + vec2(14.0, 14.0), g.size()).expand(5.0);
                    lp.rect_filled(r, 4.0, t.bg_raised);
                    lp.galley(r.min + vec2(5.0, 5.0), g, ink);
                }
            }
            if released {
                if let (Some(src), Some(pos)) = (drag.take(), pos) {
                    let (slot, _) = drop_slot(rects, pos);
                    let to = if slot > src { slot - 1 } else { slot };
                    if src != to { acts.push(Act::Move(src, to)); }
                }
                *drag_candidate = None;
            }
        }

        // ---- Queue room (phase 2 fills it)
        if super::board_queue::panel(ui, queue, qw.is_some(), fx, &mut render_live_tab) { acts.push(Act::ClearFinished); }

        // ---- Compact Queue modal; its focus scope prevents opening a shot menu behind it.
        let drawer = qw.is_none() && fx.drawer != 0;
        if drawer && super::board_queue::drawer(ui, full, fx, &mut render_live_tab) { acts.push(Act::ClearFinished); }
        if fx.dialog != 0 { super::board_batch::dialog(ui, fx) }

        // ---- shot menu
        if let (false, Some(k)) = (drawer, *menu) {
            if let (Some(c), Some(&card_rect)) = (fx.cards.get(k), rects.get(k)) {
                if let Some(a) = shot_menu(ui, t, c, k, card_rect, hl, menu) { acts.push(a) }
            } else {
                *menu = None;
            }
        }
        fx.sel = sel.iter().copied().collect();
        acts
    }
}

fn busy_job(c: &FxShot) -> bool {
    matches!(c.job.as_str(), "q" | "r" | "p")
}

/// §5.10 / §8.1 shot menu, anchored under the card's ⋯ button, flipped above when it would not fit.
fn shot_menu(ui: &mut Ui, t: &'static super::tokens::Tokens, c: &FxShot, k: usize, card_rect: Rect, hl: &mut usize, menu: &mut Option<usize>) -> Option<Act> {
    let mut items = vec![MenuItem::new("Open").icon(Icon::Open).shortcut("Enter")];
    let mut actions = vec![Some(Act::Open(k))];
    items.push(if busy_job(c) { MenuItem::new("Key this shot…").icon(Icon::Key).disabled("already in queue") } else { MenuItem::new("Key this shot…").icon(Icon::Key) });
    actions.push((!busy_job(c)).then_some(Act::Key(k)));
    if c.resume {
        items.push(MenuItem::new("Resume extraction").icon(Icon::Refresh));
        actions.push(Some(Act::Resume(k)));
    }
    items.push(if c.source_available == Some(false) { MenuItem::new("Reveal source").icon(Icon::Reveal).disabled("source not found") } else { MenuItem::new("Reveal source").icon(Icon::Reveal) });
    actions.push((c.source_available != Some(false)).then_some(Act::RevealSource(k)));
    items.push(if c.exports_available { MenuItem::new("Reveal exports") } else { MenuItem::new("Reveal exports").disabled("nothing exported yet") });
    actions.push(c.exports_available.then_some(Act::RevealExports(k)));
    items.push(MenuItem::sep());
    actions.push(None);
    items.push(MenuItem::new("Remove shot…").icon(Icon::Trash).shortcut("Delete").destructive());
    actions.push(Some(Act::Remove(k)));
    let ctx = ui.ctx().clone();
    let screen = ctx.content_rect();
    let prefix = "Shot ";
    let measure = |s: &str| line(ui.painter(), s, ty::small(), 14.0, f32::INFINITY).size().x;
    let head = format!("{prefix}{}", card::shot_name(&c.n, MENU_W - 2.0 - 24.0 - measure(prefix), measure));
    let size = MenuList::new(&items).header(&head).width(MENU_W).measure(ui.painter());
    let more = Rect::from_min_size(card_rect.min + vec2(194.0, 229.0), vec2(22.0, 22.0));
    let mut pos = pos2(more.min.x.round(), more.max.y.round() + 2.0);
    if pos.y + size.y > screen.max.y - 8.0 { pos.y = more.min.y.round() - 2.0 - size.y }
    if pos.x + MENU_W > screen.max.x - 8.0 { pos.x = screen.max.x - 8.0 - MENU_W }
    let selectable: Vec<usize> = items.iter().enumerate().filter(|(_, i)| !i.separator && i.disabled.is_none()).map(|(i, _)| i).collect();
    if !selectable.contains(hl) { *hl = selectable[0] }
    let (down, up, enter, esc, delete) = ctx.input_mut(|i| (i.consume_key(egui::Modifiers::NONE, Key::ArrowDown), i.consume_key(egui::Modifiers::NONE, Key::ArrowUp), i.consume_key(egui::Modifiers::NONE, Key::Enter), i.consume_key(egui::Modifiers::NONE, Key::Escape), i.consume_key(egui::Modifiers::NONE, Key::Delete)));
    let at = selectable.iter().position(|i| i == hl).unwrap_or(0);
    if down { *hl = selectable[(at + 1) % selectable.len()] }
    if up { *hl = selectable[(at + selectable.len() - 1) % selectable.len()] }
    let mut picked = None;
    let area = egui::Area::new(Id::unique("board-shot-menu")).order(Order::Foreground).fixed_pos(pos).show(&ctx, |ui| {
        let out = MenuList::new(&items).header(&head).width(MENU_W).highlight(Some(*hl)).name("Shot menu").show(ui);
        if let Some(h) = out.hovered { *hl = h }
        picked = out.clicked;
        out.rect
    });
    if enter { picked = Some(*hl) }
    let remove = actions.iter().position(|a| matches!(a, Some(Act::Remove(_)))).unwrap_or(0);
    if delete { picked = Some(remove) }
    let outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.press_origin().is_some_and(|p| !area.inner.contains(p)));
    let _ = t;
    let act = picked.and_then(|i| actions.get(i).cloned().flatten());
    if esc || outside || picked.is_some() { *menu = None }
    act
}

fn drop_slot(cards: &[Rect], pos: egui::Pos2) -> (usize, usize) {
    let Some(near) = (0..cards.len()).min_by(|a, b| cards[*a].distance_to_pos(pos).total_cmp(&cards[*b].distance_to_pos(pos))) else { return (0, 0) };
    (near + usize::from(pos.x > cards[near].center().x), near)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    #[test]
    fn grid_columns_are_6_4_3_at_the_three_widths() {
        let g = |w| geometry(w);
        assert_eq!((g(1619.0).cols, g(1619.0).gap, g(1619.0).x0), (6, 32.0, 53.0));
        assert_eq!((g(1179.0).cols, g(1179.0).gap, g(1179.0).x0), (4, 32.0, 89.0));
        assert_eq!((g(859.0).cols, g(859.0).gap, g(859.0).x0), (3, 32.0, 57.0));
        assert_eq!((queue_width(1920.0), queue_width(1440.0), queue_width(900.0)), (Some(300.0), Some(260.0), None));
    }

    #[test]
    fn board_fixtures_load() {
        for (f, cards, sel, menu) in [("board-10.json", 14, 1, None), ("board-11.json", 14, 4, Some(3)), ("board-12.json", 30, 30, None), ("board-13.json", 0, 0, None)] {
            let p = Page::load(&fixture(f)).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!((p.fx.cards.len(), p.sel.len(), p.menu), (cards, sel, menu), "{f}");
            assert!(!p.fx.machines.is_empty(), "{f}: machines");
        }
        assert_eq!(Page::load(&fixture("board-12.json")).unwrap().fx.dialog, 1);
    }

    #[test]
    fn queue_tab_and_drawer_state_survive_board_refresh() {
        let mut p = Page::new();
        p.update(Fixture { live: true, qtab: "waiting".into(), drawer: 1, details: true, details_explicit: true, ..Default::default() }, &BTreeSet::new());
        p.update(Fixture { live: true, ..Default::default() }, &BTreeSet::new());
        assert_eq!((p.fx.qtab.as_str(), p.fx.drawer, p.fx.details), ("waiting", 1, true));
    }

    #[test]
    fn live_queue_details_follow_work_until_the_user_chooses_machines() {
        let mut p = Page::new();
        p.update(Fixture { live: true, ..Default::default() }, &BTreeSet::new());
        p.update(Fixture { live: true, details: true, ..Default::default() }, &BTreeSet::new());
        assert!(p.fx.details);
        p.fx.details = false;
        p.fx.details_explicit = true;
        p.update(Fixture { live: true, details: true, ..Default::default() }, &BTreeSet::new());
        assert!(!p.fx.details);
    }
}
