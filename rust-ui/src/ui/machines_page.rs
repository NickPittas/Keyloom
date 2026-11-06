//! Machines page: renders snapshots and returns actions for the app to dispatch.
use super::{
    controls::{
        buttons,
        checkbox::Check,
        dialog::{self, Dialog, DialogSize},
        scroll,
        table::{Column, RowUi, Table},
        textfield::TextField,
        line, paint_line,
    },
    icons::{self, Icon, IconSize},
    tokens::{tokens, Tokens},
    type_scale as ty,
};
use eframe::egui::{self, accesskit::Role, Align, Color32, FontId, Layout, Rect, Response, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2};
use serde::Deserialize;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Confirm {
    RetireUnused,
    RetireOne,
    RemoveService,
    RemoveConnection,
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(default)]
pub struct Fixture {
    pub local_open: bool,
    pub local_details: bool,
    pub other_open: bool,
    pub other_details: bool,
    pub joined_open: bool,
    pub selected_row: Option<String>,
    pub scanning: bool,
    pub unsettled: bool,
    pub dialog: Option<Confirm>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ServiceMode {
    AppHosted,
    Legacy { running: bool },
    NotInstalled,
    Checking,
    #[default]
    Unknown,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meter {
    pub fraction: Option<f32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meters {
    pub gpu: Meter,
    pub vram: Meter,
    pub cpu: Meter,
    pub ram: Meter,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GpuInfo {
    pub name: String,
    pub vram_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldItem {
    pub attempt_id: String,
    pub text: String,
    pub actionable: bool,
    pub foreign: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalTarget {
    pub id: String,
    pub label: String,
    pub state_text: String,
    pub ready_tasks: Vec<String>,
    pub can_recheck: bool,
    pub default: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LocalLive {
    pub name: String,
    pub allow_joins: bool,
    pub service_state_text: String,
    pub service_error: Option<String>,
    pub discovery_error: Option<String>,
    pub service_mode: ServiceMode,
    pub can_start: bool,
    pub can_stop: bool,
    pub can_remove: bool,
    pub can_refresh: bool,
    pub addresses: Vec<(String, String)>,
    pub receive_jobs: bool,
    pub endpoint_inactive: Option<String>,
    pub receive_summary: Vec<String>,
    pub held_attempts: Vec<HeldItem>,
    pub local_targets: Vec<LocalTarget>,
    pub gpus: Vec<GpuInfo>,
    pub meters: Meters,
    pub installation_id: String,
    pub certificate_id: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanLive {
    pub scanning: bool,
    pub elapsed_secs: Option<u64>,
    pub summary: String,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct OtherRow {
    pub key: String,
    pub name: String,
    pub kind_local: bool,
    pub status_text: String,
    pub status_detail: String,
    pub address: String,
    pub address_choices: Vec<String>,
    pub preferred: Option<String>,
    pub connect: bool,
    pub accept_work: bool,
    pub default: bool,
    pub joined: bool,
    pub join_allowed: bool,
    pub joining: bool,
    pub gpus: Vec<GpuInfo>,
    pub meters: Meters,
    pub leaving: bool,
    pub ready_tasks: Vec<String>,
    pub installation_id: String,
    pub last_seen_text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct JoinedRow {
    pub id: String,
    pub name: String,
    pub joined_text: String,
    pub last_seen_text: String,
    pub online: bool,
    pub in_use: bool,
    pub is_self: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum JoinedState {
    #[default]
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    pub local: LocalLive,
    pub scan: ScanLive,
    pub others: Vec<OtherRow>,
    pub default_automatic: bool,
    pub joined: Vec<JoinedRow>,
    pub joined_state: JoinedState,
    pub notes: Vec<(String, bool)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    SaveName(String),
    AllowJoin(bool),
    OpenPage,
    Rescan,
    CancelScan,
    Join(String),
    RefreshService,
    StartService,
    StopService,
    RemoveServiceConfirmed,
    OpenJoined,
    RefreshJoined,
    Retire { ids: Vec<String>, unused_only: bool },
    Connect { key: String, on: bool },
    AcceptWork { key: String, on: bool },
    LocalAcceptWork(bool),
    Prefer { key: String, url: Option<String> },
    RetryEndpoint,
    ReleaseHeld(String),
    DefaultMachine(Option<String>),
    Recheck(String),
    RemoveConnectionConfirmed(String),
    DismissNotice,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Confirmation {
    kind: Confirm,
    ids: Vec<String>,
    names: Vec<String>,
    connection_key: Option<String>,
}

pub struct Page {
    pub fx: Fixture,
    pub live_mode: bool,
    live: Live,
    notice: Option<String>,
    name: String,
    name_dirty: bool,
    selected_key: Option<String>,
    confirm: Option<Confirmation>,
    open_page_sent: bool,
    joined_was_open: bool,
}

const MACHINE_COLUMNS: [Column; 9] = [
    Column::new("Machine", 176.0), Column::new("Kind", 80.0), Column::new("Address", 144.0),
    Column::new("GPU", 200.0), Column::new("Status", 184.0), Column::new("Connect", 80.0),
    Column::new("Accept work", 96.0), Column::new("Default for new shots", 144.0),
    Column::new("Actions", 152.0).flexible(),
];
const MACHINE_WIDTHS: [f32; 9] = [176.0, 80.0, 144.0, 200.0, 184.0, 80.0, 96.0, 144.0, 152.0];
const JOINED_COLUMNS: [Column; 5] = [
    Column::new("Machine", 240.0), Column::new("Joined", 144.0), Column::new("Last seen", 160.0),
    Column::new("Status", 184.0), Column::new("Retire", 80.0).flexible(),
];
const JOINED_WIDTHS: [f32; 5] = [240.0, 144.0, 160.0, 184.0, 80.0];

impl Page {
    pub fn new() -> Self {
        Self {
            fx: Fixture { local_open: true, other_open: true, joined_open: true, ..Fixture::default() },
            live_mode: true,
            live: Live::default(),
            notice: None,
            name: String::new(),
            name_dirty: false,
            selected_key: None,
            confirm: None,
            open_page_sent: false,
            joined_was_open: false,
        }
    }

    pub fn update(&mut self, live: Live) {
        if self.name == live.local.name {
            self.name_dirty = false;
        }
        if !self.name_dirty {
            self.name.clone_from(&live.local.name);
        }
        self.fx.scanning = live.scan.scanning;
        self.fx.unsettled = !live.local.held_attempts.is_empty();
        self.live = live;
        self.live_mode = true;
    }

    pub fn set_notice(&mut self, notice: Option<String>) { self.notice = notice; }
    pub fn dismiss_notice(&mut self) { self.notice = None; }
    pub fn notice_text(&self) -> Option<&str> { self.notice.as_deref() }

    /// The page was just opened again (the app keeps one page for the session): re-arm the open requests so the
    /// service status, scan seed and joined list reload. Each request is still sent once per entry.
    pub fn reset_entry(&mut self) {
        self.open_page_sent = false;
        self.joined_was_open = false;
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let fx: Fixture = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut live = sample_live();
        live.scan.scanning = fx.scanning;
        if !fx.unsettled {
            live.local.held_attempts.clear();
        }
        let selected_key = fx.selected_row.as_deref().and_then(|name| live.others.iter().find(|row| row.name == name).map(|row| row.key.clone()));
        let confirm = fixture_confirmation(fx.dialog, &live);
        Ok(Self {
            name: live.local.name.clone(),
            live,
            live_mode: false,
            notice: None,
            selected_key,
            confirm,
            open_page_sent: true,
            joined_was_open: false,
            fx,
            name_dirty: false,
        })
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Act> {
        let mut acts = Vec::new();
        if self.live_mode && !self.open_page_sent {
            acts.push(Act::OpenPage);
            self.open_page_sent = true;
        }
        let t = *tokens(ui);
        ui.painter().rect_filled(ui.max_rect(), 0.0, t.bg_panel);
        let bounds = ui.available_rect_before_wrap();
        let overflow = self.scrolls_at(bounds.width(), bounds.height());
        let gutter = if overflow { 8.0 } else { 0.0 };
        let scroll_rect = Rect::from_min_max(bounds.min - egui::vec2(0.0, gutter), bounds.max + egui::vec2(gutter, gutter));
        let mut page = ui.new_child(UiBuilder::new().max_rect(scroll_rect).layout(Layout::top_down(Align::Min)));
        page.set_clip_rect(scroll_rect);
        let _ = scroll::scroll_v(&mut page, "machines-page", |ui| {
            if overflow {
                let _ = egui::Frame::NONE.inner_margin(egui::Margin { left: 0, right: 0, top: 8, bottom: 0 }).show(ui, |ui| self.content(ui, t, &mut acts));
            } else {
                self.content(ui, t, &mut acts);
            }
        });
        if let Some(confirm) = self.confirm.clone() {
            let (title, message, action) = dialog_copy(&confirm);
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::NULL.with("keyloom-kb-focus"), true));
            let names = confirm.names.join(" · ");
            let out = Dialog::new("machines-confirm", &title, DialogSize::S)
                .body(|body| {
                    dialog::paragraph(body, &message);
                    if !names.is_empty() {
                        let color = tokens(body).text_secondary;
                        let label = egui::Label::new(egui::RichText::new(&names).font(ty::mono()).color(color));
                        let _ = body.add(label);
                    }
                    body.add_space(-4.0);
                })
                .footer(|footer| footer.cancel("Cancel").destructive(action))
                .show(ui.ctx());
            ui.ctx().data_mut(|d| d.insert_temp(egui::Id::NULL.with("keyloom-kb-focus"), false));
            if out.primary_clicked {
                match confirm.kind {
                    Confirm::RetireUnused => acts.push(Act::Retire { ids: confirm.ids.clone(), unused_only: true }),
                    Confirm::RetireOne => acts.push(Act::Retire { ids: confirm.ids.clone(), unused_only: false }),
                    Confirm::RemoveService => acts.push(Act::RemoveServiceConfirmed),
                    Confirm::RemoveConnection => {
                        if let Some(key) = confirm.connection_key.clone() {
                            acts.push(Act::RemoveConnectionConfirmed(key));
                        }
                    }
                }
            }
            if out.closed {
                self.confirm = None;
                self.fx.dialog = None;
            }
        }
        if self.live_mode && !self.joined_was_open && self.fx.joined_open {
            acts.push(Act::OpenJoined);
        }
        self.joined_was_open = self.fx.joined_open;
        acts
    }

    fn content(&mut self, ui: &mut Ui, t: Tokens, acts: &mut Vec<Act>) {
        let w = ui.available_width();
        let local_table_w = (w - 32.0).max(0.0);
        let local_table_h = table_height(1, local_table_w < 1256.0);
        let other_table_h = table_height(self.live.others.len(), local_table_w < 1256.0);
        let joined_body_h = self.joined_body_height(local_table_w < 808.0);
        let notes_h = (self.live.notes.len() + usize::from(self.notice.is_some())) as f32 * 24.0;
        let local_h = if self.fx.local_open {
            24.0 + 2.0 + 72.0
                + self.live.local.service_error.iter().count() as f32 * 24.0
                + self.live.local.discovery_error.iter().count() as f32 * 24.0
                + self.live.local.receive_summary.len() as f32 * 24.0
                + self.live.local.held_attempts.len() as f32 * 24.0
                + local_table_h + 2.0 + 24.0 + self.local_details_height() + 3.0
        } else { 25.0 };
        let other_details_h = self.other_details_height();
        let other_h = if self.fx.other_open {
            24.0 + 26.0 + (self.live.scan.warnings.len() + self.scan_empty_row() as usize) as f32 * 24.0 + other_table_h + 5.0 + other_details_h
        } else { 25.0 };
        let joined_h = if self.fx.joined_open { 24.0 + 2.0 + joined_body_h + 5.0 } else { 25.0 };
        let content_h = 20.0 + 8.0 + notes_h + local_h + other_h + joined_h + 8.0;
        let (content_rect, _) = ui.allocate_exact_size(Vec2::new(w, content_h), Sense::hover());
        let mut page = ui.new_child(UiBuilder::new().max_rect(content_rect).layout(Layout::top_down(Align::Min)));
        page.set_clip_rect(content_rect.intersect(ui.clip_rect()));
        let origin = content_rect.min - egui::vec2(8.0, 0.0);
        draw_text(&page, Rect::from_min_size(origin + egui::vec2(8.0, 4.0), egui::vec2(w - 16.0, 20.0)), "Machines", ty::title(), 20.0, t.text_primary, Align::Min);
        let mut y = origin.y + 28.0;
        for (note, is_error) in &self.live.notes {
            draw_text(&page, Rect::from_min_size(egui::pos2(origin.x + 16.0, y), egui::vec2(w - 32.0, 24.0)), note, ty::small(), 14.0, if *is_error { t.error } else { t.text_secondary }, Align::Min);
            y += 24.0;
        }
        if let Some(notice) = self.notice.as_deref() {
            draw_text(&page, Rect::from_min_size(egui::pos2(origin.x + 16.0, y), egui::vec2(w - 120.0, 24.0)), notice, ty::small(), 14.0, t.error, Align::Min);
            if ui.add(buttons::ghost_button("Dismiss").at(egui::pos2(origin.x + w - 92.0, y))).clicked() { acts.push(Act::DismissNotice); }
            y += 24.0;
        }
        self.local_section(&mut page, t, origin.x, y, w, local_table_h, acts);
        y += local_h;
        self.other_section(&mut page, t, origin.x, y, w, other_table_h, acts);
        y += other_h;
        self.joined_section(&mut page, t, origin.x, y, w, local_table_w < 808.0, joined_body_h, acts);
    }

    fn scrolls_at(&self, width: f32, height: f32) -> bool {
        self.content_height(width) > height + 8.0
    }

    fn content_height(&self, w: f32) -> f32 {
        let local_table_w = (w - 32.0).max(0.0);
        let local_table_h = table_height(1, local_table_w < 1256.0);
        let other_table_h = table_height(self.live.others.len(), local_table_w < 1256.0);
        let notes_h = (self.live.notes.len() + usize::from(self.notice.is_some())) as f32 * 24.0;
        let local_h = if self.fx.local_open {
            24.0 + 2.0 + 72.0
                + self.live.local.service_error.iter().count() as f32 * 24.0
                + self.live.local.discovery_error.iter().count() as f32 * 24.0
                + self.live.local.receive_summary.len() as f32 * 24.0
                + self.live.local.held_attempts.len() as f32 * 24.0
                + local_table_h + 2.0 + 24.0 + self.local_details_height() + 3.0
        } else { 25.0 };
        let other_h = if self.fx.other_open {
            24.0 + 26.0 + (self.live.scan.warnings.len() + self.scan_empty_row() as usize) as f32 * 24.0 + other_table_h + 5.0 + self.other_details_height()
        } else { 25.0 };
        let joined_h = if self.fx.joined_open { 24.0 + 2.0 + self.joined_body_height(local_table_w < 808.0) + 5.0 } else { 25.0 };
        20.0 + 8.0 + notes_h + local_h + other_h + joined_h + 8.0
    }

    fn local_details_height(&self) -> f32 {
        if !self.fx.local_details { return 0.0; }
        if !self.live_mode { return 72.0; }
        let local = &self.live.local;
        let rows = local.local_targets.len().max(1)
            + local.addresses.len()
            + (!local.installation_id.is_empty()) as usize
            + (!local.certificate_id.is_empty()) as usize
            + meter_rows(&local.meters).len();
        rows as f32 * 24.0
    }

    fn other_details_height(&self) -> f32 {
        if !self.fx.other_details { return 0.0; }
        let Some(row) = self.selected_other() else { return 0.0; };
        if !self.live_mode { return 144.0; }
        let rows = row.gpus.len().max(1) + 1 + (!row.last_seen_text.is_empty()) as usize
            + (!row.status_detail.is_empty()) as usize + 1
            + (!row.installation_id.is_empty()) as usize + meter_rows(&row.meters).len();
        24.0 + rows as f32 * 24.0
    }

    fn joined_body_height(&self, horizontal: bool) -> f32 {
        let loaded = matches!(&self.live.joined_state, JoinedState::Loaded);
        let indicator = !loaded;
        let table = loaded || !self.live.joined.is_empty();
        (indicator as usize as f32 * 24.0)
            + if table { table_height(self.live.joined.len(), horizontal) } else { 0.0 }
    }

    fn scan_empty_row(&self) -> bool {
        !self.live.scan.scanning && self.live.others.is_empty()
    }

    fn selected_other(&self) -> Option<&OtherRow> {
        let key = self.selected_key.as_deref()?;
        self.live.others.iter().find(|row| row.key == key)
    }

    fn local_section(&mut self, ui: &mut Ui, t: Tokens, x: f32, y: f32, w: f32, table_h: f32, acts: &mut Vec<Act>) {
        let rect = Rect::from_min_size(egui::pos2(x + 16.0, y), egui::vec2(w - 16.0, if self.fx.local_open { 24.0 } else { 25.0 }));
        if disclosure(ui, rect, "machines-local", "This machine", self.fx.local_open, true, 0.0).clicked() { self.fx.local_open = !self.fx.local_open; }
        if !self.fx.local_open { return; }

        let local = self.live.local.clone();
        let row_y = y + 26.0;
        draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, row_y), egui::vec2(112.0, 24.0)), "Name", ty::label(), 16.0, t.text_secondary, Align::Min);
        let field_rect = Rect::from_min_size(egui::pos2(x + 136.0, row_y + 1.0), egui::vec2(240.0, 22.0));
        let mut field = ui.new_child(UiBuilder::new().max_rect(field_rect).layout(Layout::top_down(Align::Min)));
        field.set_clip_rect(field_rect.intersect(ui.clip_rect()));
        let _ = TextField::new("machines-local-name", &mut self.name, 240.0).show(&mut field);
        self.name_dirty = self.name != local.name;
        let trimmed = self.name.trim();
        let valid_name = !trimmed.is_empty() && trimmed.chars().count() <= 64 && !trimmed.chars().any(char::is_control);
        let save = buttons::secondary_button("Save").width(64.0).disabled_if(self.live_mode && (!valid_name || trimmed == local.name), "Enter a changed name of at most 64 characters");
        if ui.add(save.at(egui::pos2(x + 380.0, row_y + 1.0))).clicked() {
            acts.push(Act::SaveName(trimmed.to_owned()));
        }
        if ui.add(buttons::ghost_button("Cancel").at(egui::pos2(x + 448.0, row_y + 1.0))).clicked() {
            self.name.clone_from(&local.name);
            self.name_dirty = false;
        }

        let mut allow = local.allow_joins;
        if ui.add(Check::new(&mut allow, Some("Let other machines join this machine")).at(egui::pos2(x + 16.0, row_y + 25.0))).changed() {
            acts.push(Act::AllowJoin(allow));
        }
        let status_y = row_y + 48.0;
        if !local.service_state_text.is_empty() || !local.addresses.is_empty() || local.endpoint_inactive.is_some() {
            if local.service_state_text == "Receiving on" && local.addresses.first().is_some() {
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, status_y), egui::vec2(180.0, 24.0)), "Receiving on ", ty::small(), 14.0, t.text_secondary, Align::Min);
                let prefix_w = line(&ui.painter().clone(), "Receiving on ", ty::small(), 14.0, f32::INFINITY).size().x;
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 17.0 + prefix_w, status_y), egui::vec2(240.0, 24.0)), &local.addresses[0].0, ty::mono(), 14.0, t.text_secondary, Align::Min);
            } else {
                let state: &str = if local.service_state_text.is_empty() && local.endpoint_inactive.is_some() { "Receiving inactive" } else { local.service_state_text.as_str() };
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, status_y), egui::vec2(w - 32.0, 24.0)), state, ty::small(), 14.0, t.text_secondary, Align::Min);
            }
            if let Some(reason) = local.endpoint_inactive.as_deref() {
                let retry = ui.add(buttons::secondary_button("Retry").width(72.0).at(egui::pos2(x + w - 96.0, status_y + 1.0)));
                if retry.clicked() { acts.push(Act::RetryEndpoint); }
                let _ = retry.on_hover_text(reason);
            }
        }
        let mut status_extra_y = status_y + 24.0;
        if let Some(error) = local.service_error.as_deref() {
            draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, status_extra_y), egui::vec2(w - 32.0, 24.0)), error, ty::small(), 14.0, t.error, Align::Min);
            status_extra_y += 24.0;
        }
        if let Some(error) = local.discovery_error.as_deref() {
            draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, status_extra_y), egui::vec2(w - 32.0, 24.0)), error, ty::small(), 14.0, t.error, Align::Min);
            status_extra_y += 24.0;
        }
        for summary in &local.receive_summary {
            draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, status_extra_y), egui::vec2(w - 32.0, 24.0)), summary, ty::small(), 14.0, t.text_secondary, Align::Min);
            status_extra_y += 24.0;
        }

        let mut table_y = row_y + 72.0 + (local.service_error.iter().count() + local.discovery_error.iter().count()) as f32 * 24.0 + local.receive_summary.len() as f32 * 24.0;
        for held in &local.held_attempts {
            if !self.live_mode {
                let banner_y = table_y;
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, banner_y), egui::vec2(220.0, 24.0)), &held.text, ty::small(), 14.0, t.text_secondary, Align::Min);
                let forget = buttons::ghost_button("Forget attempt");
                let forget_w = forget.natural_width(ui);
                let _ = ui.add(forget.at(egui::pos2(x + 231.0, banner_y + 1.0)));
                let _ = ui.add(buttons::ghost_button("Clear").at(egui::pos2(x + 231.0 + forget_w + 4.0, banner_y + 1.0)));
            } else {
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, table_y), egui::vec2(w - 160.0, 24.0)), &held.text, ty::small(), 14.0, t.text_secondary, Align::Min);
                if held.actionable {
                    let button = ui.add(buttons::ghost_button("Release attempt").at(egui::pos2(x + w - 148.0, table_y + 1.0)));
                    if button.clicked() { acts.push(Act::ReleaseHeld(held.attempt_id.clone())); }
                    if held.foreign { let _ = button.on_hover_text("Received from another machine"); }
                }
            }
            table_y += 24.0;
        }

        let table_rect = Rect::from_min_size(egui::pos2(x + 16.0, table_y), egui::vec2(w - 16.0, table_h)).shrink(1.0);
        let mut table_ui = ui.new_child(UiBuilder::new().max_rect(table_rect).layout(Layout::top_down(Align::Min)));
        table_ui.set_clip_rect(table_rect.intersect(ui.clip_rect()));
        let local_gpu = gpu_summary(&local.gpus);
        let local_address = local.addresses.first().map(|(address, _)| address_host(address)).unwrap_or("—");
        let local_accept = local.receive_jobs && local.endpoint_inactive.is_none();
        let local_default = !self.live.default_automatic && local.local_targets.iter().any(|target| target.default);
        let mut local_tip = None;
        let _ = Table::new("machines-local-table", &MACHINE_COLUMNS)
            .widths(&MACHINE_WIDTHS)
            .size(table_rect.size())
            .label("This machine")
            .rows(1, |row: &mut RowUi<'_>, _| {
                row.name(&local.name);
                row.text(0, &local.name); row.text(1, "Local"); row.mono(2, local_address);
                if local.gpus.is_empty() { row.muted(3, &local_gpu); } else { row.text(3, &local_gpu); }
                row.dash(4); row.dash(5);
                if row.checkbox(6, local_accept, local.endpoint_inactive.is_none()) { acts.push(Act::LocalAcceptWork(!local.receive_jobs)); }
                let default_id = local.local_targets.iter().find(|target| target.default).or_else(|| local.local_targets.first()).map(|target| target.id.clone());
                if row.checkbox(7, local_default, true) {
                    acts.push(Act::DefaultMachine(if local_default { None } else { default_id }));
                }
                if let Some(reason) = local.endpoint_inactive.as_deref() {
                    local_tip = Some((row.cell_rect(6), reason.to_owned()));
                }
            })
            .show(&mut table_ui);
        if let Some((cell, reason)) = local_tip {
            let rect = Rect::from_min_size(egui::pos2(cell.min.x + 8.0, cell.min.y + 5.0), Vec2::splat(14.0));
            let _ = table_ui.interact(rect, egui::Id::unique("machines-local-accept-tip"), Sense::hover()).on_hover_text(reason);
        }
        table_frame(ui, table_rect, t);

        let details_y = table_y + table_h;
        let details_rect = Rect::from_min_size(egui::pos2(x + 16.0, details_y), egui::vec2(w - 16.0, 24.0));
        if disclosure(ui, details_rect, "machines-local-details", "Details", self.fx.local_details, false, 4.0).clicked() {
            self.fx.local_details = !self.fx.local_details;
        }
        if self.fx.local_details {
            local_details(ui, t, x, details_y + 24.0, &local, self.live_mode, acts);
        }
    }

    fn other_section(&mut self, ui: &mut Ui, t: Tokens, x: f32, y: f32, w: f32, table_h: f32, acts: &mut Vec<Act>) {
        let rect = Rect::from_min_size(egui::pos2(x + 16.0, y), egui::vec2(w - 16.0, if self.fx.other_open { 24.0 } else { 25.0 }));
        if disclosure(ui, rect, "machines-other", "Other machines", self.fx.other_open, true, 0.0).clicked() { self.fx.other_open = !self.fx.other_open; }
        if !self.fx.other_open { return; }
        let local = &self.live.local;

        let actions_y = y + 26.0;
        if self.live.scan.scanning {
            let p = ui.painter().clone();
            let scan = line(&p, "Scanning…", ty::small(), 14.0, f32::INFINITY);
            paint_line(&p, Rect::from_min_size(egui::pos2(x + 16.0, actions_y), egui::vec2(scan.size().x, 24.0)), scan.clone(), 14.0, Align::Min, t.text_primary, 0.0);
            let close_x = if let Some(secs) = self.live.scan.elapsed_secs {
                let num_x = x + 20.0 + scan.size().x;
                let num = line(&p, &secs.to_string(), ty::mono(), 14.0, f32::INFINITY);
                paint_line(&p, Rect::from_min_size(egui::pos2(num_x, actions_y), egui::vec2(num.size().x, 24.0)), num.clone(), 14.0, Align::Min, t.text_primary, 0.0);
                let suffix = line(&p, " s", ty::small(), 14.0, f32::INFINITY);
                paint_line(&p, Rect::from_min_size(egui::pos2(num_x + num.size().x, actions_y), egui::vec2(suffix.size().x, 24.0)), suffix.clone(), 14.0, Align::Min, t.text_primary, 0.0);
                num_x + num.size().x + suffix.size().x + 4.0
            } else {
                x + 20.0 + scan.size().x
            };
            if ui.add(buttons::icon_button(Icon::Close, buttons::IconButtonSize::Normal22, "Cancel scan\nStop looking for machines").at(egui::pos2(close_x, actions_y + 1.0))).clicked() {
                acts.push(Act::CancelScan);
            }
        } else if ui.add(buttons::ghost_button("Rescan").at(egui::pos2(x + 16.0, actions_y + 1.0))).clicked() {
            acts.push(Act::Rescan);
        }
        draw_text(ui, Rect::from_min_size(egui::pos2(x + 204.0, actions_y), egui::vec2(240.0, 24.0)), &self.live.scan.summary, ty::small(), 14.0, t.text_secondary, Align::Min);

        if self.live_mode {
            let mut right = x + w;
            if local.can_remove {
                let remove = buttons::destructive_button("Remove service…");
                let remove_w = remove.natural_width(ui);
                let remove_response = ui.add(remove.at(egui::pos2(right - remove_w, actions_y + 1.0)));
                right -= remove_w + 4.0;
                if remove_response.clicked() { self.confirm = Some(Confirmation { kind: Confirm::RemoveService, ids: Vec::new(), names: Vec::new(), connection_key: None }); self.fx.dialog = Some(Confirm::RemoveService); }
            }
            if local.can_stop {
                let stop = buttons::secondary_button("Stop").width(64.0);
                let response = ui.add(stop.at(egui::pos2(right - 64.0, actions_y + 1.0)));
                right -= 68.0;
                if response.clicked() { acts.push(Act::StopService); }
            }
            if local.can_start {
                let start = buttons::secondary_button("Start").width(64.0);
                let response = ui.add(start.at(egui::pos2(right - 64.0, actions_y + 1.0)));
                right -= 68.0;
                if response.clicked() { acts.push(Act::StartService); }
            }
            if local.can_refresh {
                let refresh = ui.add(buttons::icon_button(Icon::Refresh, buttons::IconButtonSize::Normal22, "Refresh service state").at(egui::pos2(right - 22.0, actions_y + 1.0)));
                if refresh.clicked() { acts.push(Act::RefreshService); }
            }
        } else {
            let remove = buttons::destructive_button("Remove service…");
            let remove_w = remove.natural_width(ui);
            let refresh = ui.add(buttons::icon_button(Icon::Refresh, buttons::IconButtonSize::Normal22, "Refresh\nCheck service state").at(egui::pos2(x + w - remove_w - 4.0 - 22.0, actions_y + 1.0)));
            if refresh.clicked() { acts.push(Act::RefreshService); }
            if ui.add(remove.at(egui::pos2(x + w - remove_w, actions_y + 1.0))).clicked() {
                self.confirm = Some(Confirmation { kind: Confirm::RemoveService, ids: Vec::new(), names: Vec::new(), connection_key: None });
                self.fx.dialog = Some(Confirm::RemoveService);
            }
        }

        let empty_row = self.scan_empty_row();
        if empty_row {
            draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, y + 50.0), egui::vec2(w - 32.0, 24.0)), "No other machines found. Rescan to search again.", ty::small(), 14.0, t.text_secondary, Align::Min);
        }
        let warning_y = y + 50.0 + empty_row as usize as f32 * 24.0;
        for (i, warning) in self.live.scan.warnings.iter().enumerate() {
            draw_text(ui, Rect::from_min_size(egui::pos2(x + 16.0, warning_y + i as f32 * 24.0), egui::vec2(w - 32.0, 24.0)), warning, ty::small(), 14.0, t.error, Align::Min);
        }
        let table_y = warning_y + self.live.scan.warnings.len() as f32 * 24.0;
        let table_rect = Rect::from_min_size(egui::pos2(x + 16.0, table_y), egui::vec2(w - 16.0, table_h)).shrink(1.0);
        let mut selected: BTreeSet<usize> = self.selected_key.as_deref()
            .and_then(|key| self.live.others.iter().position(|row| row.key == key)).into_iter().collect();
        let others = self.live.others.clone();
        let joined = self.live.joined.clone();
        let default_key = others.iter().find(|row| row.default).map(|row| row.key.as_str());
        let mut requested_selection = None;
        let mut requested_confirm = None;
        let mut checkbox_tips: Vec<(Rect, String, usize, usize)> = Vec::new();
        let mut status_tips: Vec<(Rect, String, usize)> = Vec::new();
        let mut table_ui = ui.new_child(UiBuilder::new().max_rect(table_rect).layout(Layout::top_down(Align::Min)));
        table_ui.set_clip_rect(table_rect.intersect(ui.clip_rect()));
        let default_automatic = self.live.default_automatic;
        let out = Table::new("machines-other-table", &MACHINE_COLUMNS)
            .widths(&MACHINE_WIDTHS)
            .size(table_rect.size())
            .label("Other machines")
            .rows(others.len(), |row: &mut RowUi<'_>, i| {
                let m = &others[i];
                row.name(&m.name);
                row.text(0, &m.name); row.text(1, if m.kind_local { "Local" } else { "Network" });
                if m.address.is_empty() { row.dash(2); } else { row.mono(2, &m.address); }
                let gpu = gpu_summary(&m.gpus);
                if m.gpus.is_empty() { row.muted(3, &gpu); } else { row.text(3, &gpu); }
                row.text(4, &m.status_text);
                if !m.status_detail.is_empty() { status_tips.push((row.cell_rect(4), m.status_detail.clone(), i)); }
                let reason = if !m.joined { Some(format!("Join {} first", m.name)) } else if m.leaving { Some(format!("{} is leaving", m.name)) } else { None };
                let enabled = m.joined && !m.leaving;
                if row.checkbox(5, m.connect, enabled) { acts.push(Act::Connect { key: m.key.clone(), on: !m.connect }); }
                if row.checkbox(6, m.accept_work, enabled) { acts.push(Act::AcceptWork { key: m.key.clone(), on: !m.accept_work }); }
                let is_default = !default_automatic && default_key == Some(m.key.as_str());
                if row.checkbox(7, is_default, enabled) {
                    acts.push(Act::DefaultMachine(if is_default { None } else { Some(m.key.clone()) }));
                }
                if let Some(reason) = reason.as_ref() {
                    for col in [5, 6, 7] {
                        let cell = row.cell_rect(col);
                        checkbox_tips.push((Rect::from_min_size(egui::pos2(cell.min.x + 8.0, cell.min.y + 5.0), Vec2::splat(14.0)), reason.clone(), i, col));
                    }
                }
                if !m.joined || m.joining {
                    let join_reason = if m.joining { "Join is already in progress".to_owned() } else if !m.join_allowed { format!("{} does not allow joins", m.name) } else { String::new() };
                    let label = if m.joining { "Joining…" } else { "Join" };
                    let join = buttons::secondary_button(label).width(64.0).disabled_if(m.joining || !m.join_allowed, &join_reason);
                    if row.button(8, join).clicked() && !m.joining && m.join_allowed {
                        acts.push(Act::Join(m.key.clone()));
                    }
                } else if row.show_actions() {
                    let cell = row.cell_rect(8);
                    let more_rect = Rect::from_min_size(egui::pos2((cell.min.x + 8.5).floor(), cell.min.y + 2.0), Vec2::splat(18.0));
                    let more = row.button(8, buttons::icon_button(Icon::More, buttons::IconButtonSize::Small18, "Machine actions").in_rect(more_rect));
                    egui::Popup::menu(&more).show(|menu| {
                        // Registrations are keyed by installation id; other-machine rows carry it in `installation_id`.
                        match joined.iter().find(|j| !m.installation_id.is_empty() && j.id == m.installation_id) {
                            Some(j) => {
                                let can_retire = !j.is_self && !j.in_use;
                                let reason = if j.is_self { "This is this machine's own login; it cannot be retired" } else if j.in_use { "This machine is in use; it cannot be retired" } else { "" };
                                let retire = menu.add_enabled(can_retire, egui::Button::new("Retire…"));
                                let clicked = retire.clicked();
                                let _ = retire.on_hover_text(reason);
                                if clicked {
                                    requested_confirm = Some(Confirmation { kind: Confirm::RetireOne, ids: vec![j.id.clone()], names: vec![j.name.clone()], connection_key: None });
                                }
                            }
                            None => {
                                let _ = menu.add_enabled(false, egui::Button::new("Retire…")).on_hover_text("This machine is not joined to this network, so it cannot be retired here");
                            }
                        }
                        if m.joined || m.connect || m.accept_work {
                            if menu.button("Remove connection…").clicked() {
                                requested_confirm = Some(Confirmation { kind: Confirm::RemoveConnection, ids: Vec::new(), names: vec![m.name.clone()], connection_key: Some(m.key.clone()) });
                            }
                        }
                    });
                    if more.clicked() { requested_selection = Some(i); }
                }
            })
            .selected(&mut selected)
            .show(&mut table_ui);
        for (rect, reason, index, col) in checkbox_tips {
            let _ = table_ui.interact(rect, egui::Id::unique(("machines-checkbox-tip", index, col)), Sense::hover()).on_hover_text(reason);
        }
        for (rect, detail, index) in status_tips {
            let tip_rect = Rect::from_min_max(rect.min + Vec2::new(8.0, 0.0), rect.max - Vec2::new(8.0, 0.0));
            let _ = table_ui.interact(tip_rect, egui::Id::unique(("machines-status-tip", index)), Sense::hover()).on_hover_text(detail);
        }
        table_frame(ui, table_rect, t);
        if let Some(index) = requested_selection {
            selected.insert(index);
            self.fx.other_details = true;
        }
        if out.selection_changed || out.clicked.is_some() || requested_selection.is_some() {
            self.selected_key = selected.iter().next().and_then(|&index| others.get(index).map(|row| row.key.clone()));
            self.fx.selected_row = selected.iter().next().and_then(|&index| others.get(index).map(|row| {
                if self.live_mode { row.key.clone() } else { row.name.clone() }
            }));
            if out.clicked.is_some() { self.fx.other_details = true; }
        }
        if let Some(confirm) = requested_confirm {
            self.fx.dialog = Some(confirm.kind);
            self.confirm = Some(confirm);
        }

        if self.fx.other_details {
            if let Some(row) = self.selected_other().cloned() {
                let details_y = table_y + table_h;
                let details_rect = Rect::from_min_size(egui::pos2(x + 16.0, details_y), egui::vec2(w - 16.0, 24.0));
                if disclosure(ui, details_rect, "machines-other-details", &format!("Details: {}", row.name), true, false, 4.0).clicked() {
                    self.fx.other_details = false;
                }
                other_details(ui, t, x, details_y + 24.0, &row, self.live_mode, acts);
            }
        }
    }

    fn joined_section(&mut self, ui: &mut Ui, t: Tokens, x: f32, y: f32, w: f32, horizontal: bool, body_h: f32, acts: &mut Vec<Act>) {
        let rect = Rect::from_min_size(egui::pos2(x + 16.0, y), egui::vec2(w - 16.0, 24.0));
        let response = disclosure(ui, rect, "machines-joined", "Joined machines", self.fx.joined_open, true, 0.0);
        if response.clicked() { self.fx.joined_open = !self.fx.joined_open; }
        let retire = buttons::destructive_button("Retire all unused…");
        let retire_w = retire.natural_width(ui);
        let right = rect.max.x;
        let refresh = ui.add(buttons::ghost_button("Refresh").at(egui::pos2(right - retire_w - 8.0 - buttons::ghost_button("Refresh").natural_width(ui), y + 1.0)));
        if refresh.clicked() { acts.push(Act::RefreshJoined); }
        let eligible: Vec<&JoinedRow> = self.live.joined.iter().filter(|row| !row.is_self && !row.in_use && !row.online).collect();
        let retire = retire.disabled_if(eligible.is_empty(), "There are no unused machines to retire");
        if ui.add(retire.at(egui::pos2(right - retire_w, y + 1.0))).clicked() {
            let rows = eligible;
            self.confirm = Some(Confirmation {
                kind: Confirm::RetireUnused,
                ids: rows.iter().map(|row| row.id.clone()).collect(),
                names: rows.iter().map(|row| row.name.clone()).collect(),
                connection_key: None,
            });
            self.fx.dialog = Some(Confirm::RetireUnused);
        }
        if !self.fx.joined_open {
            ui.painter().hline(rect.x_range(), y + 24.5, Stroke::new(1.0, t.border_divider));
            return;
        }
        let joined = self.live.joined.clone();
        let state = self.live.joined_state.clone();
        let table_rect = Rect::from_min_size(egui::pos2(x + 16.0, y + 26.0), egui::vec2(w - 16.0, body_h));
        let mut row_y = table_rect.min.y;
        match state {
            JoinedState::Loading => {
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 24.0, row_y), egui::vec2(w - 32.0, 24.0)), "Loading joined machines…", ty::small(), 14.0, t.text_secondary, Align::Min);
                row_y += 24.0;
            }
            JoinedState::Failed(error) => {
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 24.0, row_y), egui::vec2(w - 32.0, 24.0)), &error, ty::small(), 14.0, t.error, Align::Min);
                row_y += 24.0;
            }
            JoinedState::Loaded => {}
        }
        let table_needed = matches!(&self.live.joined_state, JoinedState::Loaded) || !joined.is_empty();
        if table_needed {
            let inner = Rect::from_min_size(egui::pos2(table_rect.min.x, row_y), egui::vec2(table_rect.width(), table_height(joined.len(), horizontal))).shrink(1.0);
            let mut table_ui = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            table_ui.set_clip_rect(inner.intersect(ui.clip_rect()));
            let mut requested = None;
            let _ = Table::new("machines-joined-table", &JOINED_COLUMNS)
                .widths(&JOINED_WIDTHS)
                .size(inner.size())
                .label("Joined machines")
                .rows(joined.len(), |row: &mut RowUi<'_>, i| {
                    let item = &joined[i];
                    row.name(&item.name);
                    row.text(0, &item.name); row.text(1, &item.joined_text); row.text(2, &item.last_seen_text);
                    row.text(3, if item.is_self { "This machine" } else if item.online { "Connected" } else { "Not used" });
                    let (disabled, reason) = if item.is_self {
                        (true, "This is this machine's own login; it cannot be retired")
                    } else if item.in_use {
                        (true, "This machine is in use; it cannot be retired")
                    } else { (false, "") };
                    let button = buttons::destructive_button("Retire…").min_width(64.0).disabled_if(disabled, reason);
                    if row.button(4, button).clicked() {
                        requested = Some(Confirmation { kind: Confirm::RetireOne, ids: vec![item.id.clone()], names: vec![item.name.clone()], connection_key: None });
                    }
                })
                .show(&mut table_ui);
            table_frame(ui, inner, t);
            if let Some(confirm) = requested {
                self.confirm = Some(confirm.clone());
                self.fx.dialog = Some(confirm.kind);
            }
        }
    }
}

impl Default for Page {
    fn default() -> Self { Self::new() }
}

fn local_details(ui: &mut Ui, t: Tokens, x: f32, y: f32, local: &LocalLive, live_mode: bool, acts: &mut Vec<Act>) {
    let label_x = x + 32.0;
    if !live_mode {
        let target = local.local_targets.first();
        let tasks = target.map(|row| row.ready_tasks.join(" · ")).unwrap_or_default();
        let state = target.map(|row| row.state_text.as_str()).unwrap_or("");
        draw_text(ui, Rect::from_min_size(egui::pos2(label_x, y), egui::vec2(68.0, 24.0)), "Ready tasks", ty::small(), 14.0, t.text_secondary, Align::Min);
        draw_text(ui, Rect::from_min_size(egui::pos2(x + 100.0, y), egui::vec2(30.0, 24.0)), &tasks, ty::value(), 16.0, t.text_primary, Align::Min);
        draw_text(ui, Rect::from_min_size(egui::pos2(x + 130.0, y), egui::vec2(280.0, 24.0)), state, ty::value(), 16.0, t.error, Align::Min);
        if let Some(target) = target {
            let button = buttons::secondary_button("Recheck").width(72.0).disabled_if(!target.can_recheck, "This target cannot be rechecked");
            if ui.add(button.at(egui::pos2(x + 417.0, y + 1.0))).clicked() { acts.push(Act::Recheck(target.id.clone())); }
        }
        detail_id(ui, t, label_x, y + 24.0, "Installation", &local.installation_id);
        detail_id(ui, t, label_x, y + 48.0, "Certificate", &local.certificate_id);
        return;
    }

    let mut row_y = y;
    if local.local_targets.is_empty() {
        detail_pair(ui, t, label_x, row_y, "Ready tasks", "None", false);
        row_y += 24.0;
    } else {
        for target in &local.local_targets {
            detail_pair(ui, t, label_x, row_y, &target.label, &target.ready_tasks.join(" · "), !target.state_text.is_empty());
            if !target.state_text.is_empty() {
                let label_w = line(&ui.painter().clone(), &target.ready_tasks.join(" · "), ty::value(), 16.0, f32::INFINITY).size().x;
                draw_text(ui, Rect::from_min_size(egui::pos2(x + 130.0 + label_w, row_y), egui::vec2(280.0, 24.0)), &target.state_text, ty::value(), 16.0, t.error, Align::Min);
            }
            if target.can_recheck {
                if ui.add(buttons::secondary_button("Recheck").width(72.0).at(egui::pos2(x + 417.0, row_y + 1.0))).clicked() {
                    acts.push(Act::Recheck(target.id.clone()));
                }
            }
            row_y += 24.0;
        }
    }
    for (address, kind) in &local.addresses {
        detail_pair(ui, t, label_x, row_y, &format!("Address {kind}"), address, false);
        row_y += 24.0;
    }
    if !local.installation_id.is_empty() {
        detail_id(ui, t, label_x, row_y, "Installation", &local.installation_id);
        row_y += 24.0;
    }
    if !local.certificate_id.is_empty() {
        detail_id(ui, t, label_x, row_y, "Certificate", &local.certificate_id);
        row_y += 24.0;
    }
    for (label, meter) in meter_rows(&local.meters) {
        detail_pair(ui, t, label_x, row_y, label, &meter_text(meter), false);
        row_y += 24.0;
    }
}

fn other_details(ui: &mut Ui, t: Tokens, x: f32, y: f32, row: &OtherRow, live_mode: bool, acts: &mut Vec<Act>) {
    let label_x = x + 32.0;
    if !live_mode {
        let values: Vec<(String, String)> = if row.gpus.is_empty() {
            vec![("GPU 0".into(), "Not reported".into()), ("GPU 1".into(), "Not reported".into())]
        } else {
            row.gpus.iter().take(2).enumerate().map(|(i, gpu)| (format!("GPU {i}"), format!("{} · {}", gpu.name, gpu.vram_text))).collect()
        };
        for (i, (label, value)) in values.iter().enumerate() {
            detail_pair(ui, t, label_x, y + i as f32 * 24.0, label, value, false);
        }
        detail_pair(ui, t, label_x, y + 48.0, "Ready tasks", &row.ready_tasks.join(" · "), false);
        detail_pair(ui, t, label_x, y + 72.0, "Last seen", &row.last_seen_text, false);
        detail_id(ui, t, label_x, y + 96.0, "Installation", &row.installation_id);
        return;
    }

    let mut row_y = y;
    if row.gpus.is_empty() {
        detail_pair(ui, t, label_x, row_y, "GPU", "Not reported", false);
        row_y += 24.0;
    } else {
        for (i, gpu) in row.gpus.iter().enumerate() {
            detail_pair(ui, t, label_x, row_y, &format!("GPU {i}"), &format!("{} · {}", gpu.name, gpu.vram_text), false);
            row_y += 24.0;
        }
    }
    detail_pair(ui, t, label_x, row_y, "Ready tasks", &row.ready_tasks.join(" · "), false);
    row_y += 24.0;
    if !row.last_seen_text.is_empty() {
        detail_pair(ui, t, label_x, row_y, "Last seen", &row.last_seen_text, false);
        row_y += 24.0;
    }
    if !row.status_detail.is_empty() {
        detail_pair(ui, t, label_x, row_y, "Reach / RTT", &row.status_detail, false);
        row_y += 24.0;
    }
    draw_text(ui, Rect::from_min_size(egui::pos2(label_x, row_y), egui::vec2(96.0, 24.0)), "Preferred address", ty::small(), 14.0, t.text_secondary, Align::Min);
    let selected = row.preferred.as_deref().unwrap_or("Automatic");
    let response = ui.add(buttons::secondary_button(selected).width(260.0).at(egui::pos2(x + 136.0, row_y + 1.0)));
    let key = row.key.clone();
    let choices = row.address_choices.clone();
    egui::Popup::menu(&response).show(|menu| {
        if menu.button("Automatic").clicked() { acts.push(Act::Prefer { key: key.clone(), url: None }); }
        for address in &choices {
            if menu.button(address).clicked() { acts.push(Act::Prefer { key: key.clone(), url: Some(address.clone()) }); }
        }
    });
    row_y += 24.0;
    if !row.installation_id.is_empty() {
        detail_id(ui, t, label_x, row_y, "Installation", &row.installation_id);
        row_y += 24.0;
    }
    for (label, meter) in meter_rows(&row.meters) {
        detail_pair(ui, t, label_x, row_y, label, &meter_text(meter), false);
        row_y += 24.0;
    }
}

fn detail_pair(ui: &mut Ui, t: Tokens, x: f32, y: f32, label: &str, value: &str, error: bool) {
    draw_text(ui, Rect::from_min_size(egui::pos2(x, y), egui::vec2(120.0, 24.0)), label, ty::small(), 14.0, t.text_secondary, Align::Min);
    let p = ui.painter().clone();
    let label_w = line(&p, label, ty::small(), 14.0, f32::INFINITY).size().x;
    draw_text(ui, Rect::from_min_size(egui::pos2(x + label_w + 8.0, y), egui::vec2(520.0, 24.0)), value, ty::value(), 16.0, if error { t.error } else { t.text_primary }, Align::Min);
}

fn detail_id(ui: &mut Ui, t: Tokens, x: f32, y: f32, label: &str, value: &str) {
    draw_text(ui, Rect::from_min_size(egui::pos2(x, y), egui::vec2(68.0, 24.0)), label, ty::small(), 14.0, t.text_secondary, Align::Min);
    let p = ui.painter().clone();
    let display = short_id(value);
    let galley = line(&p, &display, ty::mono(), 14.0, f32::INFINITY);
    let value_x = x + line(&p, label, ty::small(), 14.0, f32::INFINITY).size().x + 8.0;
    let value_w = galley.size().x;
    paint_line(&p, Rect::from_min_size(egui::pos2(value_x, y), egui::vec2(value_w, 24.0)), galley, 14.0, Align::Min, t.text_tertiary, 0.0);
    if ui.add(buttons::icon_button(Icon::Copy, buttons::IconButtonSize::Small18, "Copy").at(egui::pos2(value_x + value_w + 8.0, y + 3.0))).clicked() {
        ui.ctx().copy_text(value.to_owned());
    }
}

fn fixture_confirmation(kind: Option<Confirm>, live: &Live) -> Option<Confirmation> {
    let kind = kind?;
    Some(match kind {
        Confirm::RetireUnused => {
            let rows: Vec<&JoinedRow> = live.joined.iter().filter(|row| !row.is_self && !row.in_use && !row.online).collect();
            Confirmation {
                kind,
                ids: rows.iter().map(|row| row.id.clone()).collect(),
                names: rows.iter().map(|row| row.name.clone()).collect(),
                connection_key: None,
            }
        }
        Confirm::RetireOne => {
            let row = live.joined.iter().find(|row| !row.is_self && !row.in_use && !row.online);
            Confirmation {
                kind,
                ids: row.map(|row| vec![row.id.clone()]).unwrap_or_default(),
                names: row.map(|row| vec![row.name.clone()]).unwrap_or_else(|| vec!["machine".into()]),
                connection_key: None,
            }
        }
        Confirm::RemoveService => Confirmation { kind, ids: Vec::new(), names: Vec::new(), connection_key: None },
        Confirm::RemoveConnection => Confirmation { kind, ids: Vec::new(), names: Vec::new(), connection_key: None },
    })
}

fn dialog_copy(confirm: &Confirmation) -> (String, String, &'static str) {
    match confirm.kind {
        Confirm::RetireUnused => (format!("Retire {} {}?", confirm.names.len(), if confirm.names.len() == 1 { "machine" } else { "machines" }), "Those machines must join again.".into(), "Retire"),
        Confirm::RetireOne => (format!("Retire {}?", confirm.names.first().map(String::as_str).unwrap_or("machine")), "It must join again to use this machine.".into(), "Retire"),
        Confirm::RemoveService => ("Remove the machine service?".into(), "This machine stops receiving work from other machines until the service is started again.".into(), "Remove"),
        Confirm::RemoveConnection => (format!("Remove the connection to {}?", confirm.names.first().map(String::as_str).unwrap_or("this machine")), "This saved connection will be removed from this machine.".into(), "Remove"),
    }
}

fn meter_rows(meters: &Meters) -> Vec<(&'static str, &Meter)> {
    [("GPU", &meters.gpu), ("VRAM", &meters.vram), ("CPU", &meters.cpu), ("RAM", &meters.ram)]
        .into_iter().filter(|(_, meter)| meter.fraction.is_some_and(f32::is_finite) || !meter.text.is_empty()).collect()
}

fn meter_text(meter: &Meter) -> String {
    if !meter.text.is_empty() { return meter.text.clone(); }
    meter.fraction.filter(|value| value.is_finite()).map(|value| format!("{:.0}%", value.clamp(0.0, 1.0) * 100.0)).unwrap_or_default()
}

fn gpu_summary(gpus: &[GpuInfo]) -> String {
    let Some(gpu) = gpus.first() else { return "Not reported".into(); };
    let summary = if gpu.vram_text.is_empty() { gpu.name.clone() } else { format!("{} · {}", gpu.name, gpu.vram_text) };
    if gpus.len() > 1 { format!("{summary} +{}", gpus.len() - 1) } else { summary }
}

fn address_host(address: &str) -> &str {
    if let Some(rest) = address.strip_prefix('[') {
        if let Some((host, port)) = rest.split_once("]:") {
            if port.parse::<u16>().is_ok() { return host; }
        }
        return address;
    }
    let Some((host, port)) = address.rsplit_once(':') else { return address; };
    if host.contains(':') || port.parse::<u16>().is_err() { return address; }
    host
}

fn short_id(value: &str) -> String {
    if value.chars().count() <= 18 { return value.to_owned(); }
    let head: String = value.chars().take(8).collect();
    let tail: String = value.chars().rev().take(6).collect::<String>().chars().rev().collect();
    format!("{head}…{tail}")
}

fn sample_live() -> Live {
    let mut live = Live::default();
    live.local = LocalLive {
        name: "endor-omarchy".into(),
        allow_joins: true,
        service_state_text: "Receiving on".into(),
        service_error: None,
        discovery_error: None,
        service_mode: ServiceMode::Legacy { running: true },
        can_start: false,
        can_stop: true,
        can_remove: true,
        can_refresh: true,
        addresses: vec![("192.168.1.20:7410".into(), "LAN".into())],
        receive_jobs: true,
        endpoint_inactive: None,
        receive_summary: Vec::new(),
        held_attempts: vec![HeldItem {
            attempt_id: "attempt:A002C008".into(),
            text: "A002C008 · Received shot has not settled".into(),
            actionable: true,
            foreign: false,
        }],
        local_targets: vec![LocalTarget {
            id: "target:local-key".into(),
            label: "Ready tasks".into(),
            state_text: "Hint, Pre-matte not ready: SAM 2.1 model missing".into(),
            ready_tasks: vec!["Key".into()],
            can_recheck: true,
            default: false,
        }],
        gpus: vec![GpuInfo { name: "RTX 4090".into(), vram_text: "24 GB".into() }],
        meters: Meters::default(),
        installation_id: "0f002c3a-1111-2222-3333-68d90b".into(),
        certificate_id: "f21a93b6-1111-2222-3333-e25b71".into(),
    };
    live.scan = ScanLive { scanning: false, elapsed_secs: Some(12), summary: "6 machines found".into(), warnings: Vec::new() };
    let row = |key: &str, name: &str, address: &str, status: &str, joined: bool, connect: bool, accept_work: bool, default: bool, join_allowed: bool, gpus: Vec<GpuInfo>, installation_id: &str, last_seen: &str| OtherRow {
        key: key.into(),
        name: name.into(),
        kind_local: false,
        status_text: status.into(),
        status_detail: "Reachable on 2 addresses · RTT 8 ms".into(),
        address: address.into(),
        address_choices: vec![format!("https://{address}:7410")],
        preferred: if default { Some(format!("https://{address}:7410")) } else { None },
        connect,
        accept_work,
        default,
        joined,
        join_allowed,
        joining: false,
        gpus,
        meters: Meters::default(),
        leaving: false,
        ready_tasks: vec!["Key".into(), "Hint".into(), "Pre-matte".into()],
        installation_id: installation_id.into(),
        last_seen_text: last_seen.into(),
    };
    live.others = vec![
        row("f04a9a:host-iid-04", "render-node-04", "192.168.1.34", "Available", false, false, false, false, true, Vec::new(), "", ""),
        row("f05a9a:host-iid-05", "render-node-05", "192.168.1.35", "Join not allowed", false, false, false, false, false, vec![GpuInfo { name: "RTX 3090".into(), vram_text: "24 GB".into() }], "", ""),
        row("connection:kamino2", "kamino-2", "192.168.1.33", "Enabled, not connected", true, false, false, false, true, vec![GpuInfo { name: "RTX A6000".into(), vram_text: "48 GB".into() }], "6a34f001-1111-2222-3333-aa5510", "2 days ago"),
        row("install:kamino3", "kamino3", "192.168.1.31", "Connected", true, true, true, true, true, vec![
            GpuInfo { name: "RTX 4090".into(), vram_text: "24 GB".into() },
            GpuInfo { name: "RTX 3090".into(), vram_text: "24 GB".into() },
        ], "7c41e0a2-1111-2222-3333-9d4a87", "Just now"),
        row("install:old-workstation", "old-workstation", "192.168.1.12", "Not used", true, false, false, false, true, vec![GpuInfo { name: "RTX 3090".into(), vram_text: "24 GB".into() }], "aa120011-1111-2222-3333-bb7812", "2 weeks ago"),
        row("install:render-node-07", "render-node-07", "192.168.1.37", "Connected", true, true, true, false, true, vec![GpuInfo { name: "RTX A6000".into(), vram_text: "48 GB".into() }], "77a60001-1111-2222-3333-e07102", "Just now"),
    ];
    live.joined = vec![
        JoinedRow { id: "0f002c3a-1111-2222-3333-68d90b".into(), name: "endor-omarchy".into(), joined_text: "Oct 12".into(), last_seen_text: "Now".into(), online: true, in_use: false, is_self: true },
        JoinedRow { id: "install:kamino3".into(), name: "kamino3".into(), joined_text: "Oct 28".into(), last_seen_text: "Just now".into(), online: true, in_use: true, is_self: false },
        JoinedRow { id: "install:render-node-07".into(), name: "render-node-07".into(), joined_text: "Oct 30".into(), last_seen_text: "Just now".into(), online: true, in_use: true, is_self: false },
        JoinedRow { id: "install:old-workstation".into(), name: "old-workstation".into(), joined_text: "Sep 12".into(), last_seen_text: "2 weeks ago".into(), online: false, in_use: false, is_self: false },
        JoinedRow { id: "install:studio-laptop".into(), name: "studio-laptop".into(), joined_text: "Aug 04".into(), last_seen_text: "3 weeks ago".into(), online: false, in_use: false, is_self: false },
        JoinedRow { id: "install:render-node-02".into(), name: "render-node-02".into(), joined_text: "Jul 19".into(), last_seen_text: "1 month ago".into(), online: false, in_use: false, is_self: false },
    ];
    live.joined_state = JoinedState::Loaded;
    live.default_automatic = false;
    live
}

fn table_height(rows: usize, horizontal: bool) -> f32 { 2.0 + 24.0 + rows as f32 * 24.0 + if horizontal { 8.0 } else { 0.0 } }

/// The mockup frames every machines table with a 1 px border_default line; inner is the table content rect.
fn table_frame(ui: &Ui, inner: Rect, t: Tokens) {
    ui.painter().rect_stroke(inner.expand(1.0), 0.0, Stroke::new(1.0, t.border_default), StrokeKind::Inside);
}

fn draw_text(ui: &Ui, rect: Rect, text: &str, font: FontId, line_height: f32, color: Color32, align: Align) {
    let p = ui.painter().clone();
    let g = line(&p, text, font, line_height, rect.width().max(1.0));
    paint_line(&p, rect, g, line_height, align, color, 0.0);
}

fn disclosure(ui: &mut Ui, rect: Rect, id: &str, title: &str, expanded: bool, band: bool, indent: f32) -> Response {
    let t = *tokens(ui);
    let response = ui.interact(rect, ui.scope_id().with(("machines-disclosure", id)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Button, true, title));
    ui.ctx().accesskit_node_builder(response.id, |node| node.set_expanded(expanded));
    let p = ui.painter().clone();
    let fill = if response.is_pointer_button_down_on() { t.bg_pressed } else if response.hovered() { t.bg_hover } else if band { t.bg_raised } else { t.bg_panel };
    p.rect_filled(rect, 0.0, fill);
    if band { p.hline(rect.x_range(), rect.bottom() - 0.5, Stroke::new(1.0, t.border_divider)); }
    if response.has_focus() { p.rect_stroke(rect, 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
    let remote_details = id == "machines-other-details";
    let size = if remote_details { IconSize::S16 } else { IconSize::S12 };
    let icon_x = rect.min.x + if remote_details { 0.0 } else { indent };
    icons::paint(&p, Rect::from_min_size(egui::pos2(icon_x, rect.center().y - size.px() / 2.0), Vec2::splat(size.px())), if expanded { Icon::ChevronDown } else { Icon::ChevronRight }, size, t.text_secondary);
    let title_x = rect.min.x + if indent < 8.0 { 16.0 } else { indent + 16.0 };
    draw_text(ui, Rect::from_min_max(egui::pos2(title_x, rect.min.y), rect.max - egui::vec2(8.0, 0.0)), title, if remote_details { ty::label() } else { ty::section_header() }, 16.0, if band { t.text_primary } else { t.text_secondary }, Align::Min);
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines_fixtures_select_the_approved_page_states() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let cases = [
            ("machines-20.json", false, false, false, None, None),
            ("machines-21.json", true, true, true, Some("kamino3"), None),
            ("machines-22.json", true, true, true, Some("kamino3"), Some(Confirm::RetireUnused)),
            ("machines-23.json", true, true, true, Some("kamino3"), Some(Confirm::RetireOne)),
            ("machines-24.json", false, false, false, None, Some(Confirm::RemoveService)),
        ];
        for (file, local, other, joined, selected, dialog) in cases {
            let page = Page::load(&base.join(file)).unwrap_or_else(|e| panic!("{e}"));
            assert!(page.fx.local_open && page.fx.other_open);
            assert_eq!(page.fx.local_details, local);
            assert_eq!(page.fx.other_details, other);
            assert_eq!(page.fx.joined_open, joined);
            assert_eq!(page.fx.selected_row.as_deref(), selected);
            assert_eq!(page.fx.dialog, dialog);
        }
    }

    #[test]
    fn update_keeps_selected_key_and_open_confirmation() {
        let mut page = Page::new();
        page.selected_key = Some("fingerprint:host-iid".into());
        page.confirm = Some(Confirmation {
            kind: Confirm::RetireOne,
            ids: vec!["install-id".into()],
            names: vec!["same-name".into()],
            connection_key: None,
        });
        page.update(Live::default());
        assert_eq!(page.selected_key.as_deref(), Some("fingerprint:host-iid"));
        let confirm = page.confirm.as_ref().unwrap();
        assert_eq!(confirm.kind, Confirm::RetireOne);
        assert_eq!(confirm.ids.first().map(String::as_str), Some("install-id"));
        assert_eq!(confirm.names.first().map(String::as_str), Some("same-name"));
    }

    #[test]
    fn machines_page_scrollbar_is_only_reserved_for_overflow() {
        let base = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let expanded = Page::load(&base.join("machines-21.json")).unwrap();
        assert!(expanded.scrolls_at(1424.0, 802.0));
        assert!(expanded.scrolls_at(884.0, 542.0));
        assert!(!expanded.scrolls_at(1904.0, 982.0));
        let collapsed = Page::load(&base.join("machines-20.json")).unwrap();
        assert!(!collapsed.scrolls_at(884.0, 542.0));
    }
}
