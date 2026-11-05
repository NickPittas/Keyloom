//! Live Settings renderer and the isolated `--settings-fixture` screenshot page.
use super::{
    controls::{
        buttons::{self, Btn, IconButtonSize},
        checkbox::Check,
        dropdown::{Dropdown, Item},
        line,
        numeric::{NumSpec, NumericField},
        paint_line,
        property::middle_truncate,
        scroll,
        textfield::TextField,
    },
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use crate::settings::{self as cfg, Settings};
use crate::worker::{Target, TargetKind};
use eframe::egui::{self, Align, Layout, Rect, Sense, Ui, UiBuilder, Vec2, accesskit::Role};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::PathBuf;

const PANES: [&str; 7] = [
    "Playback",
    "Appearance",
    "AI defaults",
    "Path mapping",
    "GPUs",
    "Masks and hints",
    "Paths and info",
];

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Fixture {
    pub pane: String,
    pub dirty: bool,
    pub vram_profile: String,
    pub hint_mode: String,
    pub temporal: String,
    pub hint_quality: f64,
    pub carry_hint: bool,
    pub carry_hint_quality: f64,
    pub despill: f64,
    pub linear_cutout: bool,
    pub cache_auto: bool,
    pub cache_mib: f64,
    pub decode_auto: bool,
    pub decode_threads: f64,
    pub loop_default: bool,
    pub theme: String,
    pub ui_scale: f64,
    pub background: bool,
    pub start_at_login: bool,
    pub reduce_motion: bool,
    pub shared_name: String,
    pub shared_folder: String,
    pub tracker_model: String,
    #[serde(default = "default_gpus")]
    pub gpus: Vec<GpuFixture>,
}

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct GpuFixture {
    pub name: String,
    pub memory: String,
    pub enabled: bool,
    pub keying: bool,
    pub hints: bool,
}

impl Default for GpuFixture {
    fn default() -> Self {
        Self {
            name: String::new(),
            memory: String::new(),
            enabled: false,
            keying: false,
            hints: false,
        }
    }
}

fn default_gpus() -> Vec<GpuFixture> {
    vec![
        GpuFixture {
            name: "RTX 4090".into(),
            memory: "24 GB".into(),
            enabled: true,
            keying: true,
            hints: true,
        },
        GpuFixture {
            name: "RTX 3090".into(),
            memory: "24 GB".into(),
            enabled: false,
            keying: false,
            hints: false,
        },
    ]
}

impl Default for Fixture {
    fn default() -> Self {
        Self {
            pane: "AI defaults".into(),
            dirty: false,
            vram_profile: "Auto".into(),
            hint_mode: "None".into(),
            temporal: "Auto".into(),
            hint_quality: 0.950,
            carry_hint: true,
            carry_hint_quality: 0.950,
            despill: 0.500,
            linear_cutout: false,
            cache_auto: true,
            cache_mib: 0.0,
            decode_auto: true,
            decode_threads: 0.0,
            loop_default: true,
            theme: "Dark".into(),
            ui_scale: 1.00,
            background: false,
            start_at_login: false,
            reduce_motion: false,
            shared_name: "NAS".into(),
            shared_folder: "/mnt/nas".into(),
            tracker_model: "Base+ (324 MB)".into(),
            gpus: default_gpus(),
        }
    }
}

pub struct Page {
    pub fx: Fixture,
    search: String,
    live_pane: String,
    live_search: String,
    closed_sections: HashSet<String>,
    open_gpu_details: HashSet<u32>,
    live_overflow: bool,
    numeric_epoch: u64,
}

fn empty_fixture() -> Fixture {
    Fixture {
        pane: String::new(),
        dirty: false,
        vram_profile: String::new(),
        hint_mode: String::new(),
        temporal: String::new(),
        hint_quality: 0.0,
        carry_hint: false,
        carry_hint_quality: 0.0,
        despill: 0.0,
        linear_cutout: false,
        cache_auto: false,
        cache_mib: 0.0,
        decode_auto: false,
        decode_threads: 0.0,
        loop_default: false,
        theme: String::new(),
        ui_scale: 0.0,
        background: false,
        start_at_login: false,
        reduce_motion: false,
        shared_name: String::new(),
        shared_folder: String::new(),
        tracker_model: String::new(),
        gpus: Vec::new(),
    }
}

pub struct View<'a> {
    pub saved: &'a Settings,
    /// Apply is offered for edits or when an unusable file must be replaced (`Settings::can_apply`).
    pub can_apply: bool,
    /// Why the defaults are active and the file will be moved aside on Apply; shown in the footer.
    pub repair: Option<String>,
    pub error: Option<&'a str>,
    pub mapping_problem: Option<String>,
    pub access: Vec<Option<String>>,
    pub samples: Vec<Option<String>>,
    pub probing: Option<usize>,
    pub browsing: Option<usize>,
    pub paths: Vec<PathInfo>,
    pub gpus: Vec<GpuInfo>,
}

#[derive(Clone, Debug, Default)]
pub struct PathInfo {
    pub label: String,
    pub path: Option<PathBuf>,
    pub exists: bool,
}

#[derive(Clone, Debug, Default)]
pub struct GpuInfo {
    pub index: u32,
    pub name: String,
    pub vram_gb: Option<f32>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    Apply,
    Cancel,
    Reset,
    Browse(usize),
    Test(usize),
    Reveal(PathBuf),
    OpenModels,
}

impl Page {
    pub fn new() -> Self {
        Self {
            fx: empty_fixture(),
            search: String::new(),
            live_pane: PANES[0].into(),
            live_search: String::new(),
            closed_sections: HashSet::new(),
            open_gpu_details: HashSet::new(),
            live_overflow: false,
            numeric_epoch: 0,
        }
    }

    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut fx: Fixture =
            serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        if !PANES.contains(&fx.pane.as_str()) {
            fx.pane = "AI defaults".into();
        }
        let mut page = Self::new();
        page.fx = fx;
        Ok(page)
    }

    pub fn ui_live(&mut self, ui: &mut Ui, draft: &mut Settings, view: &View<'_>) -> Vec<Act> {
        draw_live_page(self, ui, draft, view)
    }

    pub fn ui(&mut self, ui: &mut Ui) {
        let available = ui.available_rect_before_wrap();
        let full = Rect::from_min_max(
            egui::pos2(available.left(), 60.0),
            available.max,
        );
        let t = tokens(ui);
        let p = ui.painter().clone();
        p.rect_filled(full, 0, t.bg_panel);

        let title = Rect::from_min_size(full.min, Vec2::new(full.width(), 44.0));
        p.rect_filled(
            Rect::from_min_size(
                title.left_bottom() - egui::vec2(0.0, 1.0),
                Vec2::new(title.width(), 1.0),
            ),
            0,
            t.border_divider,
        );
        paint_text(
            &p,
            title.shrink2(Vec2::new(8.0, 0.0)),
            "Settings",
            ty::title(),
            20.0,
            t.text_primary,
            Align::Min,
        );

        let workspace = Rect::from_min_max(egui::pos2(full.left(), title.bottom()), full.max);
        let side_width = if full.width() < 1200.0 { 160.0 } else { 200.0 };
        let side = Rect::from_min_size(workspace.min, Vec2::new(side_width, workspace.height()));
        p.rect_filled(
            Rect::from_min_size(
                egui::pos2(side.right() - 1.0, side.top()),
                Vec2::new(1.0, side.height()),
            ),
            0,
            t.border_divider,
        );

        let search_rect = Rect::from_min_size(
            side.min + Vec2::new(8.0, 8.0),
            Vec2::new(side_width - 16.0, 22.0),
        );
        let mut search_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(search_rect)
                .layout(Layout::left_to_right(Align::Center))
                .id_salt("settings-search"),
        );
        search_ui.add(
            TextField::new(
                "settings-search-field",
                &mut self.search,
                search_rect.width(),
            )
            .placeholder("Search settings"),
        );

        let nav_top = search_rect.bottom() + 8.0;
        for (i, pane) in PANES.iter().enumerate() {
            let row = Rect::from_min_size(
                egui::pos2(side.left(), nav_top + i as f32 * 24.0),
                Vec2::new(side_width, 24.0),
            );
            let selected = self.fx.pane == *pane;
            let response = ui.interact(
                row,
                ui.make_persistent_id(("settings-pane", pane)),
                Sense::click(),
            );
            if selected {
                p.rect_filled(row, 0, t.accent_tint);
                p.rect_filled(
                    Rect::from_min_size(row.min, Vec2::new(2.0, row.height())),
                    0,
                    t.accent_bright,
                );
            } else if response.hovered() {
                p.rect_filled(row, 0, t.bg_hover);
            }
            response.widget_info(|| egui::WidgetInfo::labeled(Role::Tab, true, pane));
            paint_text(
                &p,
                row.shrink2(Vec2::new(8.0, 0.0)),
                pane,
                ty::label(),
                24.0,
                t.text_primary,
                Align::Min,
            );
            if response.clicked() {
                self.fx.pane = (*pane).to_owned();
            }
        }

        let content = Rect::from_min_max(egui::pos2(side.right(), workspace.top()), workspace.max);
        let viewport = content.shrink2(Vec2::splat(8.0));
        let mut scroll_ui = ui.new_child(
            UiBuilder::new()
                .max_rect(viewport)
                .layout(Layout::top_down(Align::Min))
                .id_salt("settings-content-root"),
        );
        scroll_ui.spacing_mut().item_spacing = Vec2::ZERO;
        scroll::scroll_v(&mut scroll_ui, "settings-content", |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let width = ui.available_width().min(720.0).max(480.0);
            let (r, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
            paint_text(
                ui.painter(),
                r,
                &self.fx.pane,
                ty::title(),
                20.0,
                t.text_primary,
                Align::Min,
            );
            draw_pane(ui, width, &mut self.fx, full.width() < 1200.0);
        });
    }
}

fn paint_text(
    p: &egui::Painter,
    rect: Rect,
    text: &str,
    font: egui::FontId,
    line_height: f32,
    color: egui::Color32,
    align: Align,
) {
    let g = line(p, text, font, line_height, f32::INFINITY);
    paint_line(p, rect, g, line_height, align, color, 0.0);
}

fn section(ui: &mut Ui, width: f32, title: &str, border: bool, rows: impl FnOnce(&mut Ui)) {
    let t = tokens(ui);
    section_heading(ui, width, title, true);
    ui.allocate_exact_size(Vec2::new(width, 2.0), Sense::hover());
    rows(ui);
    ui.allocate_exact_size(Vec2::new(width, 4.0), Sense::hover());
    if border {
        let (r, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
        ui.painter().rect_filled(r, 0, t.border_divider);
    }
}

fn section_heading(ui: &mut Ui, width: f32, title: &str, open: bool) -> Rect {
    let (head, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    ui.painter().rect_filled(head, 0, tokens(ui).bg_raised);
    icons::paint(
        ui.painter(),
        Rect::from_min_size(head.min + Vec2::new(8.0, 6.0), Vec2::splat(12.0)),
        if open { Icon::ChevronDown } else { Icon::ChevronRight },
        IconSize::S12,
        tokens(ui).text_secondary,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_max(head.min + Vec2::new(24.0, 0.0), head.max),
        title,
        ty::section_header(),
        16.0,
        tokens(ui).text_primary,
        Align::Min,
    );
    head
}

fn property_row<R>(
    ui: &mut Ui,
    width: f32,
    label: Option<&str>,
    tooltip: &str,
    f: impl FnOnce(&mut Ui) -> R,
) -> R {
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    if let Some(label) = label {
        let label_rect = Rect::from_min_size(row.min, Vec2::new(200.0, row.height()));
        paint_text(
            ui.painter(),
            label_rect,
            label,
            ty::label(),
            16.0,
            tokens(ui).text_secondary,
            Align::Min,
        );
        if !tooltip.is_empty() {
            ui.interact(
                label_rect,
                ui.make_persistent_id(("settings-label", label)),
                Sense::hover(),
            )
            .on_hover_text(tooltip);
        }
    }
    let control_rect = Rect::from_min_max(
        row.min + Vec2::new(208.0, 0.0),
        row.max - Vec2::new(8.0, 0.0),
    );
    let mut control = ui.new_child(
        UiBuilder::new()
            .max_rect(control_rect)
            .layout(Layout::left_to_right(Align::Center))
            .id_salt((
                "settings-control",
                label.unwrap_or("checkbox"),
                row.min.y.to_bits(),
            )),
    );
    control.set_clip_rect(control_rect.intersect(ui.clip_rect()));
    control.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
    f(&mut control)
}

fn dropdown_row(
    ui: &mut Ui,
    width: f32,
    label: &str,
    value: &mut String,
    options: &[&str],
    tip: &str,
) -> bool {
    property_row(ui, width, Some(label), tip, |ui| {
        let items: Vec<Item<'_>> = options.iter().map(|v| Item::new(*v)).collect();
        let mut selected = options
            .iter()
            .position(|v| *v == value.as_str())
            .unwrap_or(0);
        let mut dd = Dropdown::new(
            ("settings-dropdown", label),
            &mut selected,
            &items,
            ui.available_width(),
        )
        .name(label);
        if !tip.is_empty() {
            dd = dd.tip(tip);
        }
        let response = dd.show(ui);
        if response.changed() {
            if let Some(selected_value) = options.get(selected) {
                *value = (*selected_value).to_owned();
            }
        }
        response.changed()
    })
}

fn check_row(
    ui: &mut Ui,
    width: f32,
    value: &mut bool,
    label: &str,
    disabled: Option<&str>,
    tip: &str,
) -> bool {
    property_row(ui, width, None, "", |ui| {
        let mut check = Check::new(value, Some(label));
        if let Some(reason) = disabled {
            check = check.disabled(reason);
        }
        let response = ui.add(check);
        let changed = response.changed();
        if !tip.is_empty() {
            response.on_hover_text(tip);
        }
        changed
    })
}

fn draw_pane(ui: &mut Ui, width: f32, fx: &mut Fixture, compact: bool) {
    match fx.pane.as_str() {
        "Playback" => {
            section(ui, width, "Decoding", false, |ui| {
                let spec = NumSpec::integer(0, 64, 65536);
                if property_row(
                    ui,
                    width,
                    Some("Decoded-frame RAM"),
                    "Decoded-frame RAM in MiB. Auto uses half the free RAM, 1–8 GiB.",
                    |ui| {
                        NumericField::new("settings-cache-mib", &mut fx.cache_mib, &spec)
                            .auto(&mut fx.cache_auto)
                            .unit("MiB")
                            .name("Decoded-frame RAM")
                            .show(ui)
                    },
                )
                .changed()
                {
                    fx.dirty = true;
                }
                let spec = NumSpec::integer(0, 1, 32);
                if property_row(
                    ui,
                    width,
                    Some("Decode threads"),
                    "Decode threads\nAuto picks a count for this machine.",
                    |ui| {
                        NumericField::new("settings-decode-threads", &mut fx.decode_threads, &spec)
                            .auto(&mut fx.decode_auto)
                            .name("Decode threads")
                            .show(ui)
                    },
                )
                .changed()
                {
                    fx.dirty = true;
                }
                if check_row(
                    ui,
                    width,
                    &mut fx.loop_default,
                    "Loop within In/Out by default",
                    None,
                    "",
                ) {
                    fx.dirty = true;
                }
            });
            draw_applybar(ui, width, fx);
        }
        "Appearance" => {
            section(ui, width, "Window", false, |ui| {
                if dropdown_row(
                    ui,
                    width,
                    "Theme",
                    &mut fx.theme,
                    &["System", "Dark", "Light"],
                    "",
                ) {
                    fx.dirty = true;
                }
                let spec = NumSpec::new(1.0, 0.75, 2.0, 0.05, 2);
                if property_row(ui, width, Some("UI scale"), "", |ui| {
                    NumericField::new("settings-ui-scale", &mut fx.ui_scale, &spec)
                        .slider(true)
                        .default_tick(true)
                        .name("UI scale")
                        .show(ui)
                })
                .changed()
                {
                    fx.dirty = true;
                }
                if check_row(
                    ui,
                    width,
                    &mut fx.background,
                    "Keep running in the background",
                    None,
                    "Keep running in the background\nKeep Keyloom running when the window is closed.",
                ) {
                    fx.dirty = true;
                }
                if check_row(
                    ui,
                    width,
                    &mut fx.start_at_login,
                    "Start at sign-in",
                    Some("Start at sign-in\nEnable Keep running in the background first"),
                    "",
                ) {
                    fx.dirty = true;
                }
                if check_row(ui, width, &mut fx.reduce_motion, "Reduce motion", None, "") {
                    fx.dirty = true;
                }
            });
            draw_applybar(ui, width, fx);
        }
        "AI defaults" => {
            section(ui, width, "Key engine", true, |ui| {
                let tip = "VRAM profile\nAuto picks; Low VRAM conserves. Neither caps memory.";
                if dropdown_row(
                    ui,
                    width,
                    "VRAM profile",
                    &mut fx.vram_profile,
                    &["Auto", "Low VRAM"],
                    tip,
                ) {
                    fx.dirty = true;
                }
                if dropdown_row(
                    ui,
                    width,
                    "Hint mode",
                    &mut fx.hint_mode,
                    &["None", "Full", "Marks"],
                    "",
                ) {
                    fx.dirty = true;
                }
                let tip = "Temporal awareness: neighbouring frames seen by the key; context stays within In/Out.";
                if dropdown_row(
                    ui,
                    width,
                    "Temporal",
                    &mut fx.temporal,
                    &[
                        "Auto",
                        "80 frames (stride 40)",
                        "48 frames (stride 24)",
                        "24 frames (stride 12)",
                        "8 frames (stride 4)",
                        "Off: single frame (1/1)",
                    ],
                    tip,
                ) {
                    fx.dirty = true;
                }
            });
            section(ui, width, "Hint", true, |ui| {
                let spec = NumSpec::unit_float(0.950);
                if property_row(ui, width, Some("Hint quality"), "", |ui| {
                    NumericField::new("settings-hint-quality", &mut fx.hint_quality, &spec)
                        .slider(true)
                        .default_tick(true)
                        .name("Hint quality")
                        .show(ui)
                })
                .changed()
                {
                    fx.dirty = true;
                }
                if check_row(
                    ui,
                    width,
                    &mut fx.carry_hint,
                    "Carry hint",
                    None,
                    "Carry hint\nCarry the hint between temporal windows.",
                ) {
                    fx.dirty = true;
                }
                let spec = NumSpec::unit_float(0.950);
                if property_row(ui, width, Some("Carry hint quality"), "", |ui| {
                    NumericField::new(
                        "settings-carry-hint-quality",
                        &mut fx.carry_hint_quality,
                        &spec,
                    )
                    .slider(true)
                    .default_tick(true)
                    .name("Carry hint quality")
                    .show(ui)
                })
                .changed()
                {
                    fx.dirty = true;
                }
            });
            section(ui, width, "Despill", true, |ui| {
                let spec = NumSpec::unit_float(0.500);
                if property_row(
                    ui,
                    width,
                    Some("Despill"),
                    "Despill\nStrength; default 0.500",
                    |ui| {
                        NumericField::new("settings-despill", &mut fx.despill, &spec)
                            .slider(true)
                            .default_tick(true)
                            .name("Despill")
                            .show(ui)
                    },
                )
                .changed()
                {
                    fx.dirty = true;
                }
            });
            section(ui, width, "Cutout", false, |ui| {
                if check_row(ui, width, &mut fx.linear_cutout, "Linear cutout", None, "") {
                    fx.dirty = true;
                }
            });
            draw_applybar(ui, width, fx);
        }
        "Path mapping" => {
            section(ui, width, "Shared locations", true, |ui| {
                if property_row(ui, width, Some("Shared location name"), "", |ui| {
                    ui.add(TextField::new(
                        "settings-shared-name",
                        &mut fx.shared_name,
                        ui.available_width(),
                    ))
                })
                .changed()
                {
                    fx.dirty = true;
                }
                if property_row(ui, width, Some("Folder on this machine"), "", |ui| {
                    let browse = buttons::secondary_button("Browse…");
                    let browse_width = browse.natural_width(ui);
                    let field_width = (ui.available_width() - browse_width - 4.0).max(120.0);
                    let changed = ui
                        .add(TextField::new(
                            "settings-shared-folder",
                            &mut fx.shared_folder,
                            field_width,
                        ))
                        .changed();
                    ui.add(browse.min_width(72.0).at(ui.cursor().min - egui::vec2(4.0, 0.0)));
                    changed
                }) {
                    fx.dirty = true;
                }
                property_row(ui, width, Some("Access"), "", |ui| {
                    ui.add(buttons::secondary_button("Test read and write"));
                    ui.add(buttons::secondary_button("Remove"));
                });
            });
            let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
            ui.add(
                buttons::secondary_button("Add shared location")
                    .at(egui::pos2(row.left(), row.center().y - 11.0)),
            );
            draw_applybar(ui, width, fx);
        }
        "GPUs" => {
            section(ui, width, "Hardware", false, |ui| {
                for (index, gpu) in fx.gpus.iter_mut().enumerate() {
                    if draw_gpu_row(ui, width, index, gpu, compact) {
                        fx.dirty = true;
                    }
                    draw_gpu_details(ui, width);
                }
            });
            draw_applybar(ui, width, fx);
        }
        "Masks and hints" => {
            section(ui, width, "SAM 2.1 tracker", false, |ui| {
                if dropdown_row(
                    ui,
                    width,
                    "SAM tracker model",
                    &mut fx.tracker_model,
                    &[
                        "Fast (184 MB)",
                        "Base+ (324 MB)",
                        "Highest quality (898 MB)",
                    ],
                    "",
                ) {
                    fx.dirty = true;
                }
            });
            draw_applybar(ui, width, fx);
        }
        "Paths and info" => {
            section(ui, width, "Locations", false, |ui| {
                path_row(
                    ui,
                    width,
                    "Preferences file",
                    "/home/artist/.config/keyloom/settings.json",
                );
                path_row(ui, width, "Runtime folder", "/opt/keyloom/app");
                path_row(
                    ui,
                    width,
                    "Python",
                    "/opt/keyloom/runtime/python/bin/python3",
                );
            });
        }
        _ => {}
    }
}

fn draw_gpu_row(
    ui: &mut Ui,
    width: f32,
    index: usize,
    gpu: &mut GpuFixture,
    compact: bool,
) -> bool {
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let y = row.top() + 1.0;
    let label = format!("GPU {index} · {} · {}", gpu.name, gpu.memory);
    let mut changed = ui
        .add(Check::new(&mut gpu.enabled, Some(&label)).at(egui::pos2(row.left() + 8.0, y)))
        .changed();
    let role_x = row.left() + if compact { 296.0 } else { 304.0 };
    let first = "Use for keying";
    let role_w = 20.0
        + line(ui.painter(), first, ty::label(), 16.0, f32::INFINITY)
            .size()
            .x;
    let mut keying = Check::new(&mut gpu.keying, Some(first)).at(egui::pos2(role_x, y));
    let role_gap = if compact { 8.0 } else { 16.0 };
    let mut hints = Check::new(&mut gpu.hints, Some("Use for hints"))
        .at(egui::pos2(role_x + role_w + role_gap, y));
    let key_tip = format!("Use for keying\nEnable GPU {index} first");
    let hints_tip = format!("Use for hints\nEnable GPU {index} first");
    if !gpu.enabled {
        keying = keying.disabled(&key_tip);
        hints = hints.disabled(&hints_tip);
    }
    changed |= ui.add(keying).changed();
    changed |= ui.add(hints).changed();
    changed
}

fn draw_gpu_details(ui: &mut Ui, width: f32) {
    ui.allocate_exact_size(Vec2::new(width, 4.0), Sense::hover());
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    icons::paint(
        ui.painter(),
        Rect::from_min_size(row.min + Vec2::new(8.0, 6.0), Vec2::splat(12.0)),
        Icon::ChevronRight,
        IconSize::S12,
        tokens(ui).text_secondary,
    );
    paint_text(
        ui.painter(),
        Rect::from_min_max(row.min + Vec2::new(24.0, 0.0), row.max),
        "Details",
        ty::label(),
        16.0,
        tokens(ui).text_secondary,
        Align::Min,
    );
    ui.allocate_exact_size(Vec2::new(width, 4.0), Sense::hover());
}

fn path_row(ui: &mut Ui, width: f32, label: &str, path: &str) {
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let t = tokens(ui);
    let p = ui.painter().clone();
    paint_text(
        &p,
        Rect::from_min_size(row.min, Vec2::new(200.0, 24.0)),
        label,
        ty::label(),
        16.0,
        t.text_secondary,
        Align::Min,
    );
    let copy_x = row.right() - 8.0 - 18.0;
    let reveal_x = copy_x - 22.0;
    let value_rect = Rect::from_min_max(
        row.min + Vec2::new(208.0, 0.0),
        egui::pos2(reveal_x - 4.0, row.bottom()),
    );
    ui.interact(
        value_rect,
        ui.make_persistent_id(("settings-path", label)),
        Sense::hover(),
    )
    .on_hover_text(path);
    let shown = middle_truncate(path, |s| {
        line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x <= value_rect.width()
    });
    paint_line(
        &p,
        value_rect,
        line(&p, &shown, ty::mono(), 14.0, f32::INFINITY),
        14.0,
        Align::Min,
        t.text_secondary,
        0.0,
    );
    let y = row.center().y - 9.0;
    ui.add(
        buttons::icon_button(
            Icon::Copy,
            IconButtonSize::Small18,
            "Copy path\nCopy the full path",
        )
        .at(egui::pos2(reveal_x, y)),
    );
    ui.add(
        buttons::icon_button(
            Icon::Reveal,
            IconButtonSize::Small18,
            "Reveal folder\nOpen the containing folder",
        )
        .at(egui::pos2(copy_x, y)),
    );
}

fn draw_applybar(ui: &mut Ui, width: f32, fx: &mut Fixture) {
    let (bar, _) = ui.allocate_exact_size(Vec2::new(width, 43.0), Sense::hover());
    ui.painter().rect_filled(
        Rect::from_min_size(bar.min, Vec2::new(width, 1.0)),
        0,
        tokens(ui).border_divider,
    );
    let y = bar.top() + 9.0;
    let reset = buttons::ghost_button("Reset defaults").action_height();
    let cancel = buttons::secondary_button("Cancel").action_height();
    let cancel_w = cancel.natural_width(ui);
    let dirty = fx.dirty && fx.pane == "AI defaults";
    let mut apply: Btn<'_> = buttons::primary_button("Apply settings").action_height();
    if !dirty {
        apply = apply.disabled("Apply settings\nNo changes");
    }
    let apply_w = apply.natural_width(ui);
    let apply_x = bar.right() - apply_w;
    let cancel_x = apply_x - 12.0 - cancel_w;
    let reset_response = ui.add(reset.at(egui::pos2(bar.left(), y)));
    if !dirty {
        let note = "No changes";
        let note_w = line(ui.painter(), note, ty::small(), 14.0, f32::INFINITY)
            .size()
            .x;
        paint_text(
            ui.painter(),
            Rect::from_min_size(
                egui::pos2(cancel_x - 8.0 - note_w, y),
                Vec2::new(note_w, 26.0),
            ),
            note,
            ty::small(),
            14.0,
            tokens(ui).text_tertiary,
            Align::Min,
        );
    }
    ui.add(cancel.at(egui::pos2(cancel_x, y)));
    ui.add(apply.at(egui::pos2(apply_x, y)));
    if reset_response.clicked() {
        let pane = fx.pane.clone();
        *fx = Fixture {
            pane,
            ..Fixture::default()
        };
    }
}

const PANE_SEARCH: [&str; 7] = [
    "playback decoded frame ram cache mib memory workers threads loop in out",
    "appearance interface theme ui scale zoom background running sign in login",
    "ai defaults key engine vram profile hint marks temporal quality carry despill cutout linear",
    "path mapping shared location storage nas mount drive unc root folder browse test access",
    "gpus gpu hardware configuration enabled automatic runtime repo python",
    "masks hints sam2 tracker model",
    "paths info preferences runtime repo python folder models",
];

fn draw_live_page(page: &mut Page, ui: &mut Ui, draft: &mut Settings, view: &View<'_>) -> Vec<Act> {
    const FOOTER_H: f32 = 43.0;
    let available = ui.available_rect_before_wrap();
    let full = Rect::from_min_max(egui::pos2(available.left(), 60.0), available.max);
    let t = tokens(ui);
    let p = ui.painter().clone();
    p.rect_filled(full, 0, t.bg_panel);
    let title = Rect::from_min_size(full.min, Vec2::new(full.width(), 44.0));
    p.rect_filled(
        Rect::from_min_size(title.left_bottom() - egui::vec2(0.0, 1.0), Vec2::new(title.width(), 1.0)),
        0,
        t.border_divider,
    );
    paint_text(&p, title.shrink2(Vec2::new(8.0, 0.0)), "Settings", ty::title(), 20.0, t.text_primary, Align::Min);

    let workspace = Rect::from_min_max(egui::pos2(full.left(), title.bottom()), full.max);
    let side_width = if full.width() < 1200.0 { 160.0 } else { 200.0 };
    let side = Rect::from_min_size(workspace.min, Vec2::new(side_width, workspace.height()));
    p.rect_filled(Rect::from_min_size(egui::pos2(side.right() - 1.0, side.top()), Vec2::new(1.0, side.height())), 0, t.border_divider);
    let search_rect = Rect::from_min_size(side.min + Vec2::new(8.0, 8.0), Vec2::new(side_width - 16.0, 22.0));
    let mut search_ui = ui.new_child(
        UiBuilder::new().max_rect(search_rect).layout(Layout::left_to_right(Align::Center)).id_salt("settings-live-search"),
    );
    let search_changed = search_ui
        .add(TextField::new("settings-live-search-field", &mut page.live_search, search_rect.width()).placeholder("Search settings"))
        .changed();
    let term = page.live_search.trim().to_lowercase();
    let matching: Vec<usize> = PANES
        .iter()
        .enumerate()
        .filter_map(|(i, pane)| {
            (term.is_empty() || pane.to_lowercase().contains(term.as_str()) || PANE_SEARCH[i].contains(term.as_str())).then_some(i)
        })
        .collect();
    if search_changed {
        if let Some(&first) = matching.first() {
            page.live_pane = PANES[first].to_owned();
        }
    } else if !matching.iter().any(|i| PANES[*i] == page.live_pane.as_str()) {
        if let Some(&first) = matching.first() {
            page.live_pane = PANES[first].to_owned();
        }
    }

    let nav_top = search_rect.bottom() + 8.0;
    for (position, &i) in matching.iter().enumerate() {
        let pane = PANES[i];
        let row = Rect::from_min_size(egui::pos2(side.left(), nav_top + position as f32 * 24.0), Vec2::new(side_width, 24.0));
        let selected = page.live_pane.as_str() == pane;
        let response = ui.interact(row, ui.make_persistent_id(("settings-live-pane", pane)), Sense::click());
        if selected {
            p.rect_filled(row, 0, t.accent_tint);
            p.rect_filled(Rect::from_min_size(row.min, Vec2::new(2.0, row.height())), 0, t.accent_bright);
        } else if response.hovered() {
            p.rect_filled(row, 0, t.bg_hover);
        }
        response.widget_info(|| egui::WidgetInfo::labeled(Role::Tab, true, pane));
        paint_text(&p, row.shrink2(Vec2::new(8.0, 0.0)), pane, ty::label(), 24.0, t.text_primary, Align::Min);
        if response.clicked() {
            page.live_pane = pane.to_owned();
        }
        response.on_hover_text(format!("Open the {pane} settings pane"));
    }

    let content = Rect::from_min_max(egui::pos2(side.right(), workspace.top()), workspace.max);
    let viewport = content.shrink2(Vec2::splat(8.0));
    let scroll_rect = if page.live_overflow {
        Rect::from_min_max(viewport.min, egui::pos2(viewport.right(), (viewport.bottom() - FOOTER_H).max(viewport.top())))
    } else {
        viewport
    };
    let mut scroll_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(scroll_rect)
            .layout(Layout::top_down(Align::Min))
            .id_salt("settings-live-content-root"),
    );
    scroll_ui.spacing_mut().item_spacing = Vec2::ZERO;
    let mut actions = Vec::new();
    let output = scroll::scroll_both(&mut scroll_ui, ("settings-live-content", page.live_pane.as_str()), |ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        let width = ui.available_width().min(720.0).max(480.0);
        let (r, _) = ui.allocate_exact_size(Vec2::new(width, 32.0), Sense::hover());
        paint_text(ui.painter(), r, &page.live_pane, ty::title(), 20.0, tokens(ui).text_primary, Align::Min);
        if let Some(error) = view.error {
            let color = super::theme::error(ui);
            ui.colored_label(color, error);
            ui.add_space(4.0);
        }
        if matching.is_empty() {
            ui.label("No matching settings");
        } else {
            draw_live_fields(
                ui,
                width,
                &page.live_pane,
                draft,
                view,
                &mut page.closed_sections,
                &mut page.open_gpu_details,
                page.numeric_epoch,
                &mut actions,
            );
        }
        if !page.live_overflow {
            if let Some(action) = draw_live_applybar(ui, width, draft, view, &mut page.numeric_epoch) {
                actions.push(action);
            }
        }
    });
    let now_overflow = if page.live_overflow {
        output.content_size.y + FOOTER_H > viewport.height()
    } else {
        output.content_size.y > viewport.height()
    };
    if now_overflow != page.live_overflow {
        page.live_overflow = now_overflow;
        ui.ctx().request_repaint();
    }
    if page.live_overflow {
        let footer_rect = Rect::from_min_size(egui::pos2(viewport.left(), viewport.bottom() - FOOTER_H), Vec2::new(viewport.width(), FOOTER_H));
        let mut footer = ui.new_child(
            UiBuilder::new().max_rect(footer_rect).layout(Layout::top_down(Align::Min)).id_salt("settings-live-sticky-footer"),
        );
        if let Some(action) = draw_live_applybar(&mut footer, (footer_rect.width()).min(720.0).max(480.0), draft, view, &mut page.numeric_epoch) {
            actions.push(action);
        }
    }
    if actions.iter().any(|a| matches!(a, Act::Reset | Act::Cancel)) {
        super::theme::apply_preference(ui.ctx(), &view.saved.interface.theme);
    }
    actions
}

fn draw_live_fields(
    ui: &mut Ui,
    width: f32,
    pane: &str,
    draft: &mut Settings,
    view: &View<'_>,
    closed_sections: &mut HashSet<String>,
    open_gpu_details: &mut HashSet<u32>,
    numeric_epoch: u64,
    actions: &mut Vec<Act>,
) {
    match pane {
        "Playback" => live_section(ui, width, "Decoding", false, pane, closed_sections, |ui| {
            let spec = NumSpec::integer(0, 64, 65536);
            let mut value = draft.playback.cache_mib as f64;
            let mut auto = draft.playback.cache_mib == 0;
            let changed = property_row(ui, width, Some("Decoded-frame RAM"), "Decoded-frame RAM in MiB. Auto uses half the free RAM, 1–8 GiB. A CLI cache value takes precedence.", |ui| {
                NumericField::new(("settings-live-cache-mib", numeric_epoch), &mut value, &spec)
                    .auto(&mut auto).unit("MiB").name("Decoded-frame RAM").show(ui)
            }).changed();
            if changed { draft.playback.cache_mib = if auto { 0 } else { value.round().clamp(64.0, 65536.0) as u32 }; }

            let spec = NumSpec::integer(0, 1, 32);
            let mut value = draft.playback.decode_workers as f64;
            let mut auto = draft.playback.decode_workers == 0;
            let changed = property_row(ui, width, Some("Decode threads"), "Auto picks a count for this machine. Used for the next clip open.", |ui| {
                NumericField::new(("settings-live-decode-threads", numeric_epoch), &mut value, &spec)
                    .auto(&mut auto).name("Decode threads").show(ui)
            }).changed();
            if changed { draft.playback.decode_workers = if auto { 0 } else { value.round().clamp(1.0, 32.0) as u32 }; }
            check_row(ui, width, &mut draft.playback.loop_default, "Loop within In/Out by default", None, "Seeds a new viewer; the current range and playhead stay unchanged.");
        }),
        "Appearance" => live_section(ui, width, "Window", false, pane, closed_sections, |ui| {
            const THEMES: [(&str, &str); 3] = [("System", "System"), ("Dark", "Dark"), ("Light", "Light")];
            if mapped_dropdown_row(ui, width, "Theme", &mut draft.interface.theme, &THEMES, "") {
                super::theme::apply_preference(ui.ctx(), &draft.interface.theme);
            }
            let spec = NumSpec::new(1.0, 0.75, 2.0, 0.05, 2);
            let mut value = draft.interface.ui_scale as f64;
            if property_row(ui, width, Some("UI scale"), "Applied after saving.", |ui| {
                NumericField::new(("settings-live-ui-scale", numeric_epoch), &mut value, &spec)
                    .slider(true).default_tick(true).name("UI scale").show(ui)
            }).changed() { draft.interface.ui_scale = value as f32; }
            check_row(ui, width, &mut draft.interface.background, "Keep running in the background", None, "Closing the window hides it; File → Quit exits. Reopening shows the existing window.");
            if !draft.interface.background { draft.interface.launch_at_login = false; }
            check_row(
                ui,
                width,
                &mut draft.interface.launch_at_login,
                "Start at sign-in",
                (!draft.interface.background).then_some("Start at sign-in\nEnable Keep running in the background first"),
                "Starts Keyloom minimized when you sign in.",
            );
        }),
        "AI defaults" => {
            live_section(ui, width, "Key engine", true, pane, closed_sections, |ui| {
                const VRAM: [(&str, &str); 2] = [("Auto", "auto"), ("Low VRAM", "low")];
                mapped_dropdown_row(ui, width, "VRAM profile", &mut draft.ai.vram_profile, &VRAM, "Auto picks a profile; Low VRAM conserves. Neither caps memory.");
                const HINTS: [(&str, &str); 3] = [("None", "none"), ("Full", "full"), ("Marks", "keyframes")];
                mapped_dropdown_row(ui, width, "Hint mode", &mut draft.ai.hint_mode, &HINTS, "Seeds clips that do not already have saved Hint settings.");
                const TEMPORAL: [(&str, &str); 6] = [
                    ("Auto", "auto"),
                    ("80 frames (stride 40)", "80/40"),
                    ("48 frames (stride 24)", "48/24"),
                    ("24 frames (stride 12)", "24/12"),
                    ("8 frames (stride 4)", "8/4"),
                    ("Off: single frame (1/1)", "1/1"),
                ];
                let temporal_tip = cfg::temporal_explain(&draft.ai.temporal_window, None);
                mapped_dropdown_row(ui, width, "Temporal", &mut draft.ai.temporal_window, &TEMPORAL, &temporal_tip);
            });
            live_section(ui, width, "Hint", true, pane, closed_sections, |ui| {
                let spec = NumSpec::unit_float(0.950);
                let mut value = draft.ai.hint_quality as f64;
                if property_row(ui, width, Some("Hint quality"), "Seeds clips without saved Hint settings.", |ui| {
                    NumericField::new(("settings-live-hint-quality", numeric_epoch), &mut value, &spec)
                        .slider(true).default_tick(true).name("Hint quality").show(ui)
                }).changed() { draft.ai.hint_quality = value as f32; }
                check_row(ui, width, &mut draft.ai.carry_hint, "Carry hint", None, "Carry the hint between temporal windows.");
                let mut value = draft.ai.carry_hint_quality as f64;
                if property_row(ui, width, Some("Carry hint quality"), "Stored even when Carry hint is off.", |ui| {
                    NumericField::new(("settings-live-carry-hint-quality", numeric_epoch), &mut value, &spec)
                        .slider(true).default_tick(true).name("Carry hint quality").show(ui)
                }).changed() { draft.ai.carry_hint_quality = value as f32; }
            });
            live_section(ui, width, "Despill", true, pane, closed_sections, |ui| {
                let spec = NumSpec::unit_float(0.500);
                let mut value = draft.ai.despill_strength as f64;
                if property_row(ui, width, Some("Despill"), "Strength; default 0.500.", |ui| {
                    NumericField::new(("settings-live-despill", numeric_epoch), &mut value, &spec)
                        .slider(true).default_tick(true).name("Despill").show(ui)
                }).changed() { draft.ai.despill_strength = value as f32; }
            });
            live_section(ui, width, "Cutout", false, pane, closed_sections, |ui| {
                check_row(ui, width, &mut draft.ai.cutout_linear, "Linear cutout", None, "Seeds clips without saved Key settings.");
            });
        }
        "Path mapping" => draw_live_mappings(ui, width, draft, view, closed_sections, actions),
        "GPUs" => draw_live_gpus(ui, width, draft, view, closed_sections, open_gpu_details, numeric_epoch),
        "Masks and hints" => {
            live_section(ui, width, "SAM 2.1 tracker", false, pane, closed_sections, |ui| {
                let labels: Vec<String> = cfg::TRACKER_MODELS.iter().map(|(label, size, _)| format!("{label} ({size})")).collect();
                let options: Vec<(&str, &str)> = cfg::TRACKER_MODELS.iter().zip(&labels).map(|((_, _, id), label)| (label.as_str(), *id)).collect();
                mapped_dropdown_row(ui, width, "SAM tracker model", &mut draft.masks.tracker_model, &options, "Seeds the SAM tracker model for clips without saved Hint settings.");
            });
        }
        "Paths and info" => {
            live_section(ui, width, "Locations", false, pane, closed_sections, |ui| {
                for path in &view.paths {
                    path_row_live(ui, width, path, actions);
                    if path.label == "Python" && path.path.is_none() {
                        live_note(ui, width, "Developer default: CK_PYTHON, then <repo>/.venv.");
                    }
                }
                let (row, _) = ui.allocate_exact_size(Vec2::new(width, 28.0), Sense::hover());
                if ui.add(buttons::secondary_button("Open Models & runtime…").at(egui::pos2(row.left(), row.center().y - 11.0))).clicked() {
                    actions.push(Act::OpenModels);
                }
            });
        }
        _ => {}
    }
}

fn live_section(
    ui: &mut Ui,
    width: f32,
    title: &str,
    border: bool,
    pane: &str,
    closed: &mut HashSet<String>,
    rows: impl FnOnce(&mut Ui),
) {
    let id = format!("{pane}/{title}");
    let mut open = !closed.contains(&id);
    section_live(ui, width, title, border, &id, &mut open, rows);
    if open { closed.remove(&id); } else { closed.insert(id); }
}

fn section_live(ui: &mut Ui, width: f32, title: &str, border: bool, id: &str, open: &mut bool, rows: impl FnOnce(&mut Ui)) {
    let t = tokens(ui);
    let head = section_heading(ui, width, title, *open);
    let response = ui.interact(head, ui.make_persistent_id(("settings-live-section", id)), Sense::click());
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Button, true, title));
    let clicked = response.clicked();
    response.on_hover_text(if *open { "Collapse section" } else { "Expand section" });
    if clicked { *open = !*open; }
    if *open {
        ui.allocate_exact_size(Vec2::new(width, 2.0), Sense::hover());
        rows(ui);
        ui.allocate_exact_size(Vec2::new(width, 4.0), Sense::hover());
        if border {
            let (r, _) = ui.allocate_exact_size(Vec2::new(width, 1.0), Sense::hover());
            ui.painter().rect_filled(r, 0, t.border_divider);
        }
    }
}

fn mapped_dropdown_row(ui: &mut Ui, width: f32, label: &str, value: &mut String, options: &[(&str, &str)], tip: &str) -> bool {
    property_row(ui, width, Some(label), tip, |ui| {
        let known = options.iter().position(|(_, wire)| *wire == value.as_str());
        let mut labels: Vec<String> = options.iter().map(|(shown, _)| (*shown).to_owned()).collect();
        if known.is_none() { labels.push(format!("Unknown · {}", value)); }
        let items: Vec<Item<'_>> = labels.iter().map(|shown| Item::new(shown.as_str())).collect();
        let mut selected = known.unwrap_or(labels.len() - 1);
        let mut dropdown = Dropdown::new(("settings-live-dropdown", label), &mut selected, &items, ui.available_width()).name(label);
        if !tip.is_empty() { dropdown = dropdown.tip(tip); }
        let response = dropdown.show(ui);
        if response.changed() {
            if let Some((_, wire)) = options.get(selected) { *value = (*wire).to_owned(); }
        }
        response.changed()
    })
}

fn draw_live_mappings(ui: &mut Ui, width: f32, draft: &mut Settings, view: &View<'_>, closed: &mut HashSet<String>, actions: &mut Vec<Act>) {
    live_section(ui, width, "Shared locations", true, "Path mapping", closed, |ui| {
        ui.label("Each PC maps the shared location name to its own folder, drive, or UNC share.");
        if let (Some(problem), Some(error)) = (&view.mapping_problem, view.error) {
            let submitted = error.strip_prefix("Not saved: ").or_else(|| error.strip_prefix("Not usable on this PC: "));
            if submitted == Some(problem.as_str()) {
                let color = super::theme::error(ui);
                ui.colored_label(color, problem);
            }
        }
        let mut remove = None;
        for (i, root) in draft.shared_roots.iter_mut().enumerate() {
            let busy = view.browsing == Some(i) || view.probing == Some(i);
            let lock_rows = view.browsing.is_some() || view.probing.is_some();
            property_row(ui, width, Some("Shared location name"), "This name is written into portable job references.", |ui| {
                let mut field = TextField::new(("settings-live-shared-name", i), &mut root.name, ui.available_width()).placeholder("e.g. NAS");
                if busy { field = field.disabled("This location is busy"); }
                ui.add(field)
            });
            property_row(ui, width, Some("Folder on this machine"), "The root folder used by this installation.", |ui| {
                let browse = buttons::secondary_button("Browse…");
                let browse_width = browse.natural_width(ui);
                let field_width = (ui.available_width() - browse_width - 4.0).max(120.0);
                let mut field = TextField::new(("settings-live-shared-folder", i), &mut root.local, field_width)
                    .placeholder(if cfg!(windows) { r"Z:\Shared or \\nas\share" } else { "/mnt/nas" });
                if busy { field = field.disabled("This location is busy"); }
                let changed = ui.add(field).changed();
                let mut button = browse.min_width(72.0);
                if lock_rows { button = button.disabled("A location operation is in progress"); }
                let response = ui.add(button.at(ui.cursor().min - egui::vec2(4.0, 0.0)));
                if response.clicked() { actions.push(Act::Browse(i)); }
                changed
            });
            property_row(ui, width, Some("Access"), "Checks the selected folder only when requested.", |ui| {
                let mut test = buttons::secondary_button(if view.probing == Some(i) { "Testing…" } else { "Test read and write" });
                if view.probing.is_some() || view.browsing.is_some() || busy { test = test.disabled("Wait for the current location operation to finish"); }
                let response = ui.add(test);
                if response.clicked() { actions.push(Act::Test(i)); }
                let mut remove_button = buttons::secondary_button("Remove");
                if lock_rows { remove_button = remove_button.disabled("A location operation is in progress"); }
                if ui.add(remove_button).clicked() { remove = Some(i); }
            });
            if let Some(sample) = view.samples.get(i).and_then(Option::as_deref) { live_note(ui, width, &format!("Sample path: {sample}")); }
            if let Some(result) = view.access.get(i).and_then(Option::as_deref) {
                live_note(ui, width, result);
            }
            if view.probing == Some(i) { live_note(ui, width, "Checking read and write access…"); }
        }
        if let Some(i) = remove { draft.shared_roots.remove(i); }
        let (add_rect, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
        let mut add = buttons::secondary_button("Add shared location");
        if view.browsing.is_some() || view.probing.is_some() { add = add.disabled("A location operation is in progress"); }
        if ui.add(add.at(egui::pos2(add_rect.left(), add_rect.center().y - 11.0))).clicked() { draft.shared_roots.push(Default::default()); }
    });
}

fn draw_live_gpus(
    ui: &mut Ui,
    width: f32,
    draft: &mut Settings,
    view: &View<'_>,
    closed: &mut HashSet<String>,
    open_details: &mut HashSet<u32>,
    numeric_epoch: u64,
) {
    live_section(ui, width, "Hardware", false, "GPUs", closed, |ui| {
        if draft.ai_targets.is_empty() {
            live_note(ui, width, "Automatic GPU selection is active. No explicit GPU configurations are stored.");
        }
        if view.gpus.is_empty() {
            live_note(ui, width, "GPU hardware: Not reported");
        }
        let mut gpu_indices: Vec<u32> = view.gpus.iter().map(|gpu| gpu.index).collect();
        for target in &draft.ai_targets {
            if !gpu_indices.contains(&target.gpu) { gpu_indices.push(target.gpu); }
        }
        let mut remove = None;
        for index in gpu_indices {
            let reported = view.gpus.iter().find(|gpu| gpu.index == index);
            let header = match reported {
                Some(gpu) => {
                    let name = if gpu.name.is_empty() { "Not reported" } else { gpu.name.as_str() };
                    let vram = gpu.vram_gb.filter(|v| v.is_finite() && *v >= 0.0).map_or("VRAM not reported".to_owned(), |v| format!("{v:.1} GB VRAM"));
                    format!("GPU {index} · {name} · {vram}")
                }
                None => format!("GPU {index} · Not reported"),
            };
            live_note(ui, width, &header);
            let mut open = open_details.contains(&index);
            if live_disclosure(ui, width, &format!("Details · GPU {index}"), &format!("gpu-{index}"), &mut open) {
                open_details.insert(index);
            } else {
                open_details.remove(&index);
            }
            if open {
                let mut found = false;
                for (slot, target) in draft.ai_targets.iter_mut().enumerate() {
                    if target.gpu != index { continue; }
                    found = true;
                    ui.allocate_exact_size(Vec2::new(width, 4.0), Sense::hover());
                    property_row(ui, width, Some("Enabled"), "This explicit configuration is kept even when disabled.", |ui| {
                        ui.add(Check::new(&mut target.enabled, Some("Use this configuration")))
                    });
                    property_row(ui, width, Some("Configuration name"), "Configuration names must be unique.", |ui| {
                        ui.add(TextField::new(("settings-live-target-id", slot), &mut target.id, ui.available_width()))
                    });
                    let mut gpu_value = target.gpu as f64;
                    let spec = NumSpec::integer(0, 0, 15);
                    if property_row(ui, width, Some("GPU index"), "Uses the actual hardware index. Existing out-of-range values stay unchanged until edited.", |ui| {
                        NumericField::new(("settings-live-target-gpu", slot, numeric_epoch), &mut gpu_value, &spec).name("GPU index").show(ui)
                    }).changed() { target.gpu = gpu_value.round().clamp(0.0, u32::MAX as f64) as u32; }
                    match &mut target.kind {
                        TargetKind::Local { repo, python } => {
                            let mut repo_text = repo.to_string_lossy().into_owned();
                            if property_row(ui, width, Some("Runtime folder"), "Local runtime repository override.", |ui| {
                                ui.add(TextField::new(("settings-live-target-repo", slot), &mut repo_text, ui.available_width()))
                            }).changed() { *repo = PathBuf::from(repo_text); }
                            let mut python_text = python.as_ref().map_or_else(String::new, |path| path.to_string_lossy().into_owned());
                            if property_row(ui, width, Some("Python"), "Blank uses the interpreter selected for this runtime. Whitespace is kept until Apply.", |ui| {
                                ui.add(TextField::new(("settings-live-target-python", slot), &mut python_text, ui.available_width()).placeholder("Use runtime default"))
                            }).changed() { *python = Some(PathBuf::from(python_text)); }
                        }
                    }
                    if ui.add(buttons::ghost_button("Remove configuration")).clicked() { remove = Some(slot); }
                }
                if !found && draft.ai_targets.is_empty() {
                    live_note(ui, width, "Automatic selection includes this GPU when it is available.");
                } else if !found {
                    live_note(ui, width, "No explicit configuration for this GPU.");
                }
            }
        }
        if let Some(slot) = remove { draft.ai_targets.remove(slot); }
        if !draft.ai_targets.is_empty() {
            if ui.add(buttons::ghost_button("Use automatic GPU selection")).clicked() { draft.ai_targets.clear(); }
        }
        if ui.add(buttons::secondary_button("Add GPU configuration")).clicked() {
            let mut suffix = draft.ai_targets.len() + 1;
            let id = loop {
                let candidate = format!("target-{suffix}");
                if draft.ai_targets.iter().all(|target| target.id != candidate) { break candidate; }
                suffix += 1;
            };
            draft.ai_targets.push(Target {
                id,
                kind: TargetKind::Local { repo: PathBuf::new(), python: None },
                gpu: view.gpus.first().map_or(0, |gpu| gpu.index),
                enabled: true,
            });
            open_details.insert(view.gpus.first().map_or(0, |gpu| gpu.index));
        }
        live_note(ui, width, "GPU changes wait for running or lost work to settle.");
        if !draft.ai_targets.is_empty() { live_note(ui, width, "Removing the last explicit configuration restores automatic GPU selection."); }
    });
}

fn live_disclosure(ui: &mut Ui, width: f32, label: &str, id: &str, open: &mut bool) -> bool {
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let response = ui.interact(row, ui.make_persistent_id(("settings-live-gpu-details", id)), Sense::click());
    icons::paint(ui.painter(), Rect::from_min_size(row.min + Vec2::new(8.0, 6.0), Vec2::splat(12.0)), if *open { Icon::ChevronDown } else { Icon::ChevronRight }, IconSize::S12, tokens(ui).text_secondary);
    paint_text(ui.painter(), Rect::from_min_max(row.min + Vec2::new(24.0, 0.0), row.max), label, ty::label(), 16.0, tokens(ui).text_secondary, Align::Min);
    response.widget_info(|| egui::WidgetInfo::labeled(Role::Button, true, label));
    let clicked = response.clicked();
    response.on_hover_text(if *open { "Hide configurations" } else { "Show configurations" });
    if clicked { *open = !*open; }
    *open
}

fn path_row_live(ui: &mut Ui, width: f32, info: &PathInfo, actions: &mut Vec<Act>) {
    let (row, _) = ui.allocate_exact_size(Vec2::new(width, 24.0), Sense::hover());
    let t = tokens(ui);
    let p = ui.painter().clone();
    let label = if info.label == "Runtime repo" { "Runtime folder" } else { info.label.as_str() };
    paint_text(&p, Rect::from_min_size(row.min, Vec2::new(200.0, 24.0)), label, ty::label(), 16.0, t.text_secondary, Align::Min);
    let copy_x = row.right() - 8.0 - 18.0;
    let reveal_x = copy_x - 22.0;
    let value_rect = Rect::from_min_max(row.min + Vec2::new(208.0, 0.0), egui::pos2(reveal_x - 4.0, row.bottom()));
    let full = info.path.as_ref().map_or_else(|| "not set".to_owned(), |path| path.display().to_string());
    ui.interact(value_rect, ui.make_persistent_id(("settings-live-path", &info.label)), Sense::hover()).on_hover_text(&full);
    let shown_text = if info.path.is_some() && !info.exists { format!("{full} · missing") } else { full.clone() };
    let shown = middle_truncate(&shown_text, |s| line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x <= value_rect.width());
    paint_line(&p, value_rect, line(&p, &shown, ty::mono(), 14.0, f32::INFINITY), 14.0, Align::Min, t.text_secondary, 0.0);
    let y = row.center().y - 9.0;
    let mut copy = buttons::icon_button(Icon::Copy, IconButtonSize::Small18, "Copy path\nCopy the full path");
    if info.path.is_none() { copy = copy.disabled("No path is set"); }
    if ui.add(copy.at(egui::pos2(reveal_x, y))).clicked() {
        if let Some(path) = &info.path { ui.ctx().copy_text(path.to_string_lossy().into_owned()); }
    }
    let mut reveal = buttons::icon_button(Icon::Reveal, IconButtonSize::Small18, "Reveal folder\nOpen the containing folder");
    if info.path.is_none() { reveal = reveal.disabled("No path is set"); }
    else if !info.exists { reveal = reveal.disabled("Path does not exist"); }
    if ui.add(reveal.at(egui::pos2(copy_x, y))).clicked() {
        if let Some(path) = &info.path { actions.push(Act::Reveal(path.clone())); }
    }
}

fn live_note(ui: &mut Ui, _width: f32, text: &str) {
    let color = tokens(ui).text_secondary;
    ui.add(egui::Label::new(egui::RichText::new(text).size(12.0).color(color)).wrap()).on_hover_text(text);
}

fn draw_live_applybar(ui: &mut Ui, width: f32, draft: &Settings, view: &View<'_>, numeric_epoch: &mut u64) -> Option<Act> {
    let (bar, _) = ui.allocate_exact_size(Vec2::new(width, 43.0), Sense::hover());
    ui.painter().rect_filled(Rect::from_min_size(bar.min, Vec2::new(width, 1.0)), 0, tokens(ui).border_divider);
    let y = bar.top() + 9.0;
    let reset = buttons::ghost_button("Reset defaults").action_height();
    let cancel = buttons::secondary_button("Cancel").action_height();
    let cancel_w = cancel.natural_width(ui);
    let dirty = draft != view.saved;
    let mut apply: Btn<'_> = buttons::primary_button("Apply settings").action_height();
    if !view.can_apply { apply = apply.disabled("Apply settings\nNo changes"); }
    let apply_w = apply.natural_width(ui);
    let apply_x = bar.right() - apply_w;
    let cancel_x = apply_x - 12.0 - cancel_w;
    let reset_response = ui.add(reset.at(egui::pos2(bar.left(), y)));
    if let Some(repair) = &view.repair {
        // Clipped to the space between Reset and Cancel; the full text is on hover.
        let rect = Rect::from_min_size(egui::pos2(reset_response.rect.right() + 12.0, y), Vec2::new((cancel_x - 8.0 - reset_response.rect.right() - 12.0).max(0.0), 26.0));
        paint_text(&ui.painter().with_clip_rect(rect), rect, repair, ty::small(), 14.0, tokens(ui).warning, Align::Min);
        ui.interact(rect, ui.scope_id().with("settings-repair-note"), Sense::hover()).on_hover_text(repair.as_str());
    } else if !dirty {
        let note = "No changes";
        let note_w = line(ui.painter(), note, ty::small(), 14.0, f32::INFINITY).size().x;
        paint_text(ui.painter(), Rect::from_min_size(egui::pos2(cancel_x - 8.0 - note_w, y), Vec2::new(note_w, 26.0)), note, ty::small(), 14.0, tokens(ui).text_tertiary, Align::Min);
    }
    let cancel_response = ui.add(cancel.at(egui::pos2(cancel_x, y)));
    let apply_response = ui.add(apply.at(egui::pos2(apply_x, y)));
    if reset_response.clicked() {
        *numeric_epoch = (*numeric_epoch).wrapping_add(1);
        Some(Act::Reset)
    } else if cancel_response.clicked() {
        *numeric_epoch = (*numeric_epoch).wrapping_add(1);
        Some(Act::Cancel)
    } else if apply_response.clicked() {
        *numeric_epoch = (*numeric_epoch).wrapping_add(1);
        Some(Act::Apply)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn general_fixture_loads_the_changed_despill_value() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/settings-40.json");
        let page = Page::load(&path).unwrap();
        assert_eq!(page.fx.pane, "AI defaults");
        assert_eq!(page.fx.despill, 0.650);
        assert!(page.fx.dirty);
    }
}
