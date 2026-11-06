//! Screenshot fixture for the Models page (`--models-fixture FILE.json`).
use super::{
    controls::{self, buttons, chips, dialog::{self, Dialog, DialogSize}, scroll},
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, Align, Id, Layout, Pos2, Rect, Sense, Ui, UiBuilder, Vec2};
use serde::{Deserialize, Deserializer};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum ModelState {
    Present,
    Missing,
    Downloading,
    #[default]
    Unknown,
    Error(String),
}

impl<'de> Deserialize<'de> for ModelState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = serde_json::Value::deserialize(deserializer)?;
        let error = value.get("error").or_else(|| value.get("Error")).and_then(serde_json::Value::as_str);
        if let Some(error) = error { return Ok(Self::Error(error.to_owned())); }
        let raw = value.as_str().unwrap_or_default();
        Ok(match raw.to_ascii_lowercase().as_str() {
            "present" => Self::Present,
            "missing" => Self::Missing,
            "downloading" => Self::Downloading,
            "unknown" | "checking" | "" => Self::Unknown,
            "error" => Self::Error(String::new()),
            _ => Self::Error(raw.to_owned()),
        })
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub depth: usize,
    pub state: ModelState,
    pub expandable: bool,
    pub expanded: bool,
    pub path: String,
    pub size: String,
    pub progress: Option<f32>,
    pub downloaded: String,
    pub family: String,
    pub kind: String,
    pub capability: String,
    pub required: bool,
    pub default_variant: bool,
    pub size_is_estimate: bool,
    pub location: Option<String>,
    pub revision: String,
    pub gating: String,
    pub terms: Vec<(String, String)>,
    pub probe_imports: Vec<String>,
    pub notes: Vec<String>,
    pub requires: Vec<(String, String)>,
    pub issues: Vec<String>,
    pub local_runtime: bool,
    pub runtime_note: Option<String>,
    pub can_download: Option<bool>,
    pub download_reason: Option<String>,
    pub can_delete: Option<bool>,
    pub delete_reason: Option<String>,
    pub can_reveal: Option<bool>,
    pub reveal_reason: Option<String>,
    pub children: Vec<Model>,
}

impl Model {
    fn path(&self) -> Option<&str> {
        self.location.as_deref().or_else(|| (!self.path.is_empty()).then_some(self.path.as_str()))
    }

    fn can_download(&self) -> bool {
        self.can_download.unwrap_or(matches!(&self.state, ModelState::Missing | ModelState::Present | ModelState::Error(_)))
            && !self.local_runtime
    }

    fn can_delete(&self) -> bool {
        self.can_delete.unwrap_or(matches!(&self.state, ModelState::Present | ModelState::Error(_)))
            && !self.local_runtime
    }
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct ModelGroup {
    pub name: String,
    pub expanded: bool,
    pub models: Vec<Model>,
}

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct Fixture {
    pub selected: String,
    pub groups: Vec<ModelGroup>,
    #[serde(default = "loaded")]
    pub catalog_loaded: bool,
    pub startup_error: Option<String>,
    pub required_present: Option<bool>,
    pub missing_count: Option<usize>,
    pub busy: bool,
    pub install_block: Option<String>,
    pub required_block: Option<String>,
    pub refresh_block: Option<String>,
    pub operation: Option<OperationInfo>,
    pub last_result: Option<(String, bool)>,
    pub cancel_requested: bool,
    pub cancel_note: Option<String>,
    pub cancel_failure: bool,
    pub local_runtime_note: Option<String>,
    pub worker_text: Option<String>,
}

fn loaded() -> bool { true }

#[derive(Deserialize, Clone, Debug, Default)]
#[serde(default)]
pub struct OperationInfo {
    pub kind: String,
    pub model_id: Option<String>,
    pub progress: Option<f32>,
    pub bytes_text: String,
    pub unknown_total: bool,
    pub expected_size: Option<String>,
    pub cancellable: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Act {
    Download(String),
    InstallRequired,
    DownloadAll,
    Cancel,
    Delete(String),
    Refresh,
    Reveal(String),
}

impl Fixture {
    fn model(&self, id: &str) -> Option<&Model> {
        fn find<'a>(models: &'a [Model], id: &str) -> Option<&'a Model> {
            models.iter().find(|m| m.id == id || m.name == id).or_else(|| models.iter().find_map(|m| find(&m.children, id)))
        }
        self.groups.iter().find_map(|g| find(&g.models, id))
    }

    fn has_missing(&self) -> bool {
        fn has_missing(models: &[Model]) -> bool {
            models.iter().any(|m| m.state == ModelState::Missing || has_missing(&m.children))
        }
        self.groups.iter().any(|g| has_missing(&g.models))
    }

    fn required_status(&self) -> Option<bool> {
        if self.required_present.is_some() { return self.required_present; }
        let mut required = Vec::new();
        for group in &self.groups { collect_required(&group.models, &mut required); }
        if required.is_empty() { None }
        else if required.iter().any(|state| **state == ModelState::Missing) { Some(false) }
        else if required.iter().all(|state| **state == ModelState::Present) { Some(true) }
        else { None }
    }

    fn required_missing(&self) -> bool {
        self.required_status() == Some(false)
    }

    fn missing_names(&self) -> Vec<String> {
        fn collect(models: &[Model], out: &mut Vec<String>) {
            for model in models {
                if model.state == ModelState::Missing { out.push(model.name.clone()); }
                collect(&model.children, out);
            }
        }
        let mut out = Vec::new();
        for group in &self.groups { collect(&group.models, &mut out); }
        out
    }
}

fn collect_required<'a>(models: &'a [Model], out: &mut Vec<&'a ModelState>) {
    for model in models {
        if model.required { out.push(&model.state); }
        collect_required(&model.children, out);
    }
}

pub struct Page {
    pub fx: Fixture,
    selected: String,
    tree_width: f32,
    model_collapsed: bool,
    confirm_delete: Option<String>,
    confirm_download_all: bool,
    license_id: Option<String>,
}

impl Page {
    pub fn load(path: &std::path::Path) -> Result<Self, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut fx: Fixture = serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", path.display()))?;
        normalize(&mut fx);
        let selected = selected_id(&fx, &fx.selected).unwrap_or_else(|| first_id(&fx).unwrap_or_default());
        Ok(Self { fx, selected, tree_width: 0.0, model_collapsed: false, confirm_delete: None, confirm_download_all: false, license_id: None })
    }

    pub fn new() -> Self {
        Self { fx: Fixture { catalog_loaded: true, ..Fixture::default() }, selected: String::new(), tree_width: 0.0, model_collapsed: false, confirm_delete: None, confirm_download_all: false, license_id: None }
    }

    pub fn update(&mut self, mut fx: Fixture) {
        normalize(&mut fx);
        let mut groups = BTreeMap::new();
        let mut expanded = BTreeMap::new();
        for group in &self.fx.groups {
            groups.insert(group.name.clone(), group.expanded);
            collect_expanded(&group.models, &mut expanded);
        }
        for group in &mut fx.groups {
            if let Some(value) = groups.get(&group.name) { group.expanded = *value; }
            apply_expanded(&mut group.models, &expanded);
        }
        let selected = selected_id(&fx, &self.selected)
            .or_else(|| selected_id(&fx, &fx.selected))
            .or_else(|| first_id(&fx))
            .unwrap_or_default();
        if selected != self.selected {
            self.confirm_delete = None;
            self.license_id = None;
        }
        if self.confirm_delete.as_deref().is_some_and(|id| fx.busy || fx.startup_error.is_some() || !fx.catalog_loaded || fx.model(id).map_or(true, |m| !m.can_delete() || m.delete_reason.is_some())) {
            self.confirm_delete = None;
        }
        if fx.busy || fx.startup_error.is_some() || fx.install_block.is_some() || !fx.catalog_loaded || !fx.has_missing() { self.confirm_download_all = false; }
        self.fx = fx;
        self.selected = selected;
    }

    pub fn ui(&mut self, ui: &mut Ui) -> Vec<Act> {
        let mut acts = Vec::new();
        let root = ui.max_rect();
        let full = Rect::from_min_max(
            egui::pos2(root.min.x, root.min.y + 60.0),
            egui::pos2(root.max.x, root.max.y - 22.0),
        );
        let t = tokens(ui);
        ui.painter().rect_filled(full, 0, t.bg_panel);
        self.title_bar(ui, full, &mut acts);

        let window_w = ui.ctx().content_rect().width();
        let default_tree = if window_w >= 1600.0 { 320.0 } else if window_w >= 1200.0 { 280.0 } else { 240.0 };
        if self.tree_width == 0.0 { self.tree_width = default_tree; }
        let workspace = Rect::from_min_max(
            egui::pos2(full.min.x, full.min.y + 44.0),
            full.max,
        );

        let tree = Rect::from_min_size(workspace.min, Vec2::new(self.tree_width, workspace.height()));
        let mut tree_ui = ui.new_child(
            UiBuilder::new().max_rect(tree).layout(Layout::top_down(Align::Min)).id_salt("models-tree"),
        );
        let catalog_loaded = self.fx.catalog_loaded;
        let startup_error = self.fx.startup_error.clone();
        let (groups, selected) = (&mut self.fx.groups, &mut self.selected);
        scroll::scroll_v(&mut tree_ui, "models-tree-scroll", |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            ui.add_space(6.0);
            if !catalog_loaded || startup_error.is_some() {
                let message = startup_error.as_deref().unwrap_or("Loading model catalog…");
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width(), 36.0)), message, ty::small(), 14.0, tokens(ui).text_secondary, Align::Min);
                ui.add_space(36.0);
            } else if groups.is_empty() {
                ui.add_space(10.0);
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(ui.available_width() - 16.0, 30.0)), "No models are available", ty::small(), 14.0, tokens(ui).text_secondary, Align::Min);
            } else {
                for group in groups.iter_mut() {
                    tree_group(ui, group);
                    if group.expanded { tree_models(ui, &mut group.models, selected); }
                }
            }
        });

        let split_rect = Rect::from_min_size(
            egui::pos2(workspace.min.x + self.tree_width - 3.0, workspace.min.y),
            Vec2::new(6.0, workspace.height()),
        );
        let mut split_ui = ui.new_child(
            UiBuilder::new().max_rect(split_rect).layout(Layout::top_down(Align::Min)).id_salt("models-splitter"),
        );
        scroll::splitter(
            &mut split_ui,
            Id::unique("models-tree-splitter"),
            scroll::Axis::Horizontal,
            &mut self.tree_width,
            240.0,
            480.0,
            default_tree,
        );

        let detail = Rect::from_min_max(
            egui::pos2(workspace.min.x + self.tree_width + 1.0, workspace.min.y),
            workspace.max,
        );
        self.detail(ui, detail, &mut acts);
        self.dialogs(ui, &mut acts);
        acts
    }

    fn title_bar(&mut self, ui: &mut Ui, full: Rect, acts: &mut Vec<Act>) {
        let t = tokens(ui);
        let p = ui.painter();
        let title = Rect::from_min_size(full.min, Vec2::new(full.width(), 44.0));
        p.rect_filled(Rect::from_min_size(egui::pos2(title.min.x, title.max.y - 1.0), Vec2::new(title.width(), 1.0)), 0, t.border_divider);
        text(ui, Rect::from_min_size(title.min + Vec2::splat(8.0), Vec2::new(title.width() - 16.0, 28.0)), "Models & runtime", ty::title(), 20.0, t.text_primary, Align::Min);

        let block = self.fx.install_block.as_deref()
            .or(self.fx.startup_error.as_deref())
            .or_else(|| self.fx.busy.then_some("Wait for the current model operation to finish."));
        let all_enabled = self.fx.catalog_loaded && self.fx.startup_error.is_none() && self.fx.has_missing() && block.is_none();
        let all = if all_enabled {
            buttons::primary_button("Install all models…")
        } else {
            buttons::secondary_button("Install all models…")
                .disabled(block.unwrap_or(if !self.fx.catalog_loaded { "Model catalog is unavailable" } else if self.fx.groups.is_empty() { "No models are available" } else { "All models are present" }))
        };
        let all_w = all.natural_width(ui);
        let all_x = title.max.x - 8.0 - all_w;
        if ui.add(all.at(Pos2::new(all_x, title.min.y + 11.0))).clicked() {
            self.confirm_download_all = true;
        }

        let required_block = self.fx.required_block.as_deref()
            .or(self.fx.startup_error.as_deref())
            .or_else(|| self.fx.busy.then_some("Wait for the current model operation to finish."));
        let required_enabled = self.fx.catalog_loaded && self.fx.startup_error.is_none() && self.fx.required_missing() && required_block.is_none();
        let required = if required_enabled {
            buttons::secondary_button("Install required")
        } else {
            buttons::secondary_button("Install required").disabled(
                required_block.unwrap_or(match self.fx.required_status() {
                    Some(true) => "All required models are present",
                    Some(false) => "No required models can be installed right now",
                    None if self.fx.catalog_loaded => "Required model status is unknown",
                    None => "Model catalog is unavailable",
                }),
            )
        };
        let required_w = required.natural_width(ui);
        let required_x = all_x - 8.0 - required_w;
        if ui.add(required.at(Pos2::new(required_x, title.min.y + 11.0))).clicked() {
            acts.push(Act::InstallRequired);
        }

        let refresh_block = self.fx.refresh_block.as_deref()
            .or_else(|| self.fx.busy.then_some("Wait for the current model operation to finish."));
        let refresh = if refresh_block.is_none() {
            buttons::secondary_button("Refresh status")
        } else {
            buttons::secondary_button("Refresh status").disabled(refresh_block.unwrap())
        };
        let refresh_w = refresh.natural_width(ui);
        let refresh_x = required_x - 8.0 - refresh_w;
        if ui.add(refresh.at(Pos2::new(refresh_x, title.min.y + 11.0))).clicked() {
            acts.push(Act::Refresh);
        }

    }

    fn detail(&mut self, ui: &mut Ui, rect: Rect, acts: &mut Vec<Act>) {
        let model = self.fx.model(&self.selected).cloned();
        let inner_left = rect.min.x + 8.0;
        let inner_right = rect.max.x - 8.0;
        let header = Rect::from_min_size(egui::pos2(inner_left, rect.min.y + 8.0), Vec2::new((inner_right - inner_left).max(0.0), 32.0));
        if let Some(model) = model.as_ref() { self.detail_header(ui, header, model, acts); }

        let body = Rect::from_min_max(
            egui::pos2(inner_left, header.max.y + 8.0),
            egui::pos2(inner_right, rect.max.y - 8.0),
        );
        let mut detail_ui = ui.new_child(
            UiBuilder::new().max_rect(body).layout(Layout::top_down(Align::Min)).id_salt("models-detail"),
        );
        let collapsed = &mut self.model_collapsed;
        let license_id = &mut self.license_id;
        let operation = self.fx.operation.clone().or_else(|| model.as_ref().and_then(legacy_operation));
        let result = self.fx.last_result.clone();
        let local_runtime_note = self.fx.local_runtime_note.clone();
        let worker_text = self.fx.worker_text.clone();
        let cancel_requested = self.fx.cancel_requested;
        let cancel_note = self.fx.cancel_note.clone();
        let cancel_failure = self.fx.cancel_failure;
        scroll::scroll_v(&mut detail_ui, "models-detail-scroll", |ui| {
            ui.spacing_mut().item_spacing = Vec2::ZERO;
            let form_w = ui.available_width().min(720.0);
            if let Some(model) = model.as_ref() {
                let section_h = model_section_height(model, *collapsed);
                let section = ui.allocate_exact_size(Vec2::new(form_w, section_h), Sense::hover()).0;
                model_section(ui, section, model, collapsed, license_id, acts);
            } else {
                let message = self.fx.startup_error.as_deref().or_else(|| (!self.fx.catalog_loaded).then_some("Loading model catalog…"))
                    .unwrap_or(if self.fx.groups.is_empty() { "No models are available" } else { "Select a model" });
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(form_w, 28.0)), message, ty::value(), 16.0, tokens(ui).text_secondary, Align::Min);
            }
            if let Some(worker) = worker_text.as_deref() {
                let activity = local_activity(worker);
                ui.add_space(8.0);
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(form_w, 20.0)), &format!("This computer · {activity}"), ty::small(), 14.0, tokens(ui).text_secondary, Align::Min);
            }
            if let Some(operation) = operation.as_ref() {
                ui.add_space(8.0);
                let download_rect = ui.allocate_exact_size(Vec2::new(form_w, 52.0), Sense::hover()).0;
                download(ui, download_rect, operation, cancel_requested, cancel_note.as_deref(), cancel_failure, acts);
            }
            if let Some((message, succeeded)) = result.as_ref() {
                ui.add_space(8.0);
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(form_w, 24.0)), message, ty::small(), 14.0, if *succeeded { tokens(ui).success } else { tokens(ui).error }, Align::Min);
            }
            if let Some(note) = local_runtime_note.as_deref() {
                ui.add_space(8.0);
                text(ui, Rect::from_min_size(ui.cursor().min, Vec2::new(form_w, 36.0)), note, ty::small(), 14.0, tokens(ui).text_secondary, Align::Min);
            }
        });
    }

    fn detail_header(&mut self, ui: &mut Ui, rect: Rect, model: &Model, acts: &mut Vec<Act>) {
        let delete_block = self.fx.startup_error.as_deref()
            .or_else(|| (!self.fx.catalog_loaded).then_some("Model catalog is unavailable."))
            .or_else(|| self.fx.busy.then_some("Wait for the current model operation to finish."))
            .or_else(|| model.delete_reason.as_deref())
            .or_else(|| (!model.can_delete()).then_some("This model cannot be deleted here."));
        let delete = if delete_block.is_none() { buttons::destructive_button("Delete…") } else { buttons::secondary_button("Delete…").disabled(delete_block.unwrap()) };
        let delete_w = delete.natural_width(ui);
        let delete_x = rect.max.x - delete_w;
        if ui.add(delete.at(Pos2::new(delete_x, rect.min.y + 5.0))).clicked() {
            self.confirm_delete = Some(model.id.clone());
        }

        // Only a missing model offers an install; present, downloading and other states show no download button (bible §5.12).
        let mut name_right = delete_x - 8.0;
        if model.state == ModelState::Missing {
            let download_block = self.fx.startup_error.as_deref()
                .or_else(|| (!self.fx.catalog_loaded).then_some("Model catalog is unavailable."))
                .or_else(|| self.fx.busy.then_some("Wait for the current model operation to finish."))
                .or_else(|| model.download_reason.as_deref())
                .or_else(|| (!model.can_download()).then_some("This model cannot be installed here."));
            let download = if download_block.is_none() { buttons::primary_button("Install model") } else { buttons::secondary_button("Install model").disabled(download_block.unwrap()) };
            let download_w = download.natural_width(ui);
            let download_x = delete_x - 8.0 - download_w;
            if ui.add(download.at(Pos2::new(download_x, rect.min.y + 5.0))).clicked() {
                acts.push(Act::Download(model.id.clone()));
            }
            name_right = download_x - 8.0;
        }
        let t = tokens(ui);
        let name_rect = Rect::from_min_max(rect.min, egui::pos2(name_right, rect.max.y));
        text(ui, name_rect, &model.name, ty::title(), 20.0, t.text_primary, Align::Min);
    }

    fn dialogs(&mut self, ui: &mut Ui, acts: &mut Vec<Act>) {
        if let Some(id) = self.confirm_delete.clone() {
            let Some(model) = self.fx.model(&id).cloned() else { self.confirm_delete = None; return };
            let outcome = Dialog::new(("models-delete", id.clone()), "Delete model", DialogSize::M)
                .body(|ui| dialog::paragraph(ui, &format!("Delete {} from this machine? Only this model's files are removed; other models and project files stay.", model.name)))
                .footer(|f| f.cancel("Keep").destructive("Delete files"))
                .show(ui.ctx());
            if outcome.closed {
                if outcome.primary_clicked && self.fx.catalog_loaded && !self.fx.busy && self.fx.startup_error.is_none() && model.can_delete() && model.delete_reason.is_none() { acts.push(Act::Delete(id)); }
                self.confirm_delete = None;
            }
        }
        if self.confirm_download_all {
            let missing = self.fx.missing_names().join("\n");
            let outcome = Dialog::new("models-download-all", "Install all models", DialogSize::M)
                .body(|ui| {
                    dialog::paragraph(ui, "Install the missing models on this machine?");
                    if !missing.is_empty() { dialog::paragraph(ui, &missing); }
                })
                .footer(|f| f.cancel("Cancel").primary("Install all models"))
                .show(ui.ctx());
            if outcome.closed {
                if outcome.primary_clicked && !self.fx.busy && self.fx.install_block.is_none() && self.fx.startup_error.is_none() && self.fx.catalog_loaded && self.fx.has_missing() {
                    acts.push(Act::DownloadAll);
                }
                self.confirm_download_all = false;
            }
        }
        if let Some(id) = self.license_id.clone() {
            if let Some(model) = self.fx.model(&id) {
                let terms = model.terms.iter().map(|(k, v)| format!("{k}: {v}")).collect::<Vec<_>>().join("\n");
                let notes = model.notes.join("\n");
                let technical = [
                    (!model.family.is_empty()).then(|| format!("Family: {}", model.family)),
                    (!model.gating.is_empty()).then(|| format!("Access: {}", model.gating)),
                    (!model.probe_imports.is_empty()).then(|| format!("Imports: {}", model.probe_imports.join(", "))),
                    model.runtime_note.as_deref().map(|s| format!("Runtime: {s}")),
                    model.default_variant.then(|| "Default variant".to_owned()),
                ].into_iter().flatten().collect::<Vec<_>>().join("\n");
                let outcome = Dialog::new(("models-license", id), "Licence and terms", DialogSize::M)
                    .body(|ui| {
                        if !terms.is_empty() { dialog::paragraph(ui, &terms); }
                        if !technical.is_empty() { dialog::paragraph(ui, &technical); }
                        if !notes.is_empty() { dialog::paragraph(ui, &notes); }
                        if terms.is_empty() && technical.is_empty() && notes.is_empty() { dialog::paragraph(ui, "No licence details are recorded for this model."); }
                    })
                    .footer(|f| f.primary("Done"))
                    .show(ui.ctx());
                if outcome.closed { self.license_id = None; }
            } else {
                self.license_id = None;
            }
        }
    }
}

fn normalize(fx: &mut Fixture) {
    fn set_ids(models: &mut [Model]) {
        for model in models {
            if model.id.is_empty() { model.id = model.name.clone(); }
            set_ids(&mut model.children);
        }
    }
    for group in &mut fx.groups { set_ids(&mut group.models); }
}

fn selected_id(fx: &Fixture, value: &str) -> Option<String> {
    fn find(models: &[Model], value: &str) -> Option<String> {
        models.iter().find(|m| m.id == value || m.name == value)
            .map(|m| m.id.clone())
            .or_else(|| models.iter().find_map(|m| find(&m.children, value)))
    }
    fx.groups.iter().find_map(|g| find(&g.models, value))
}

fn first_id(fx: &Fixture) -> Option<String> {
    fn first(models: &[Model]) -> Option<String> {
        models.first().map(|m| m.id.clone()).or_else(|| models.iter().find_map(|m| first(&m.children)))
    }
    fx.groups.iter().find_map(|g| first(&g.models))
}

fn collect_expanded(models: &[Model], out: &mut BTreeMap<String, bool>) {
    for model in models {
        out.insert(model.id.clone(), model.expanded);
        collect_expanded(&model.children, out);
    }
}

fn apply_expanded(models: &mut [Model], state: &BTreeMap<String, bool>) {
    for model in models {
        if let Some(expanded) = state.get(&model.id) { model.expanded = *expanded; }
        apply_expanded(&mut model.children, state);
    }
}

fn legacy_operation(model: &Model) -> Option<OperationInfo> {
    (model.state == ModelState::Downloading).then(|| OperationInfo {
        kind: "download".into(), model_id: Some(model.id.clone()), progress: model.progress,
        bytes_text: model.downloaded.clone(), unknown_total: model.progress.is_none(),
        cancellable: true, ..OperationInfo::default()
    })
}

fn local_activity(worker: &str) -> String {
    match worker {
        "Worker not running" => "Not running".into(),
        "Worker idle" => "Idle".into(),
        "Worker has queued work" => "Has queued work".into(),
        _ => worker.strip_prefix("Worker working on ").map_or_else(|| worker.to_owned(), |step| format!("Working on {step}")),
    }
}

fn model_issues(model: &Model) -> Option<String> {
    let mut issues = model.issues.clone();
    if let ModelState::Error(reason) = &model.state {
        if !reason.is_empty() && !issues.contains(reason) { issues.push(reason.clone()); }
    }
    (!issues.is_empty()).then(|| issues.join(" · "))
}

fn model_section_height(model: &Model, collapsed: bool) -> f32 {
    if collapsed { return 25.0; }
    let rows = 2 + [
        model.path().is_some(), !model.size.is_empty(), !model.kind.is_empty(), !model.capability.is_empty(),
        !model.revision.is_empty(), !model.gating.is_empty(), !model.requires.is_empty(), !model.probe_imports.is_empty(),
        model_issues(model).is_some(), model.runtime_note.is_some(),
    ].iter().filter(|visible| **visible).count();
    31.0 + rows as f32 * 24.0
}

fn text(ui: &Ui, rect: Rect, value: &str, font: egui::FontId, line_height: f32, color: egui::Color32, align: Align) {
    let galley = controls::line(ui.painter(), value, font, line_height, rect.width());
    controls::paint_line(ui.painter(), rect, galley, line_height, align, color, 0.0);
}

fn tree_group(ui: &mut Ui, group: &mut ModelGroup) {
    let t = tokens(ui);
    let rect = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover()).0;
    let response = ui.interact(rect, ui.make_persistent_id(("models-group", &group.name)), Sense::click());
    if response.clicked() { group.expanded = !group.expanded; }
    if response.hovered() { ui.painter().rect_filled(rect, 0, t.bg_hover); }
    icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.center().y - 6.0), Vec2::splat(12.0)), if group.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, IconSize::S12, t.text_secondary);
    text(ui, Rect::from_min_max(egui::pos2(rect.min.x + 24.0, rect.min.y), rect.max - Vec2::new(8.0, 0.0)), &group.name, ty::label(), 16.0, t.text_secondary, Align::Min);
    response.on_hover_text(&group.name);
}

fn tree_models(ui: &mut Ui, models: &mut [Model], selected: &mut String) {
    for model in models.iter_mut() {
        tree_model(ui, model, selected);
        if model.expandable && model.expanded {
            tree_models(ui, &mut model.children, selected);
        }
    }
}

fn tree_model(ui: &mut Ui, model: &mut Model, selected: &mut String) {
    let t = tokens(ui);
    let rect = ui.allocate_exact_size(Vec2::new(ui.available_width(), 24.0), Sense::hover()).0;
    let response = ui.interact(rect, ui.make_persistent_id(("models-item", &model.id)), Sense::click());
    if response.clicked() {
        *selected = model.id.clone();
        if model.expandable { model.expanded = !model.expanded; }
    }
    let is_selected = *selected == model.id;
    if is_selected {
        ui.painter().rect_filled(rect, 0, t.accent_tint);
        ui.painter().rect_filled(Rect::from_min_size(rect.min, Vec2::new(2.0, rect.height())), 0, t.accent_bright);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 0, t.bg_hover);
    }
    let x = rect.min.x + if model.depth > 1 || model.expandable { 40.0 } else { 24.0 };
    if model.expandable {
        icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(rect.min.x + 24.0 * model.depth as f32, rect.center().y - 6.0), Vec2::splat(12.0)), if model.expanded { Icon::ChevronDown } else { Icon::ChevronRight }, IconSize::S12, t.text_secondary);
    }
    let state_w = tree_state_width(ui, model);
    let state_x = rect.max.x - 8.0 - state_w;
    text(ui, Rect::from_min_max(egui::pos2(x, rect.min.y), egui::pos2(state_x - 8.0, rect.max.y)), &model.name, ty::label(), 16.0, t.text_primary, Align::Min);
    paint_tree_state(ui, rect, model, is_selected, state_x);
    response.on_hover_text(match &model.state { ModelState::Error(e) if !e.is_empty() => format!("{}\n{}", model.name, e), _ => model.name.clone() });
}

fn tree_state_width(ui: &Ui, model: &Model) -> f32 {
    let (value, icon) = match &model.state {
        ModelState::Present => ("Present".to_owned(), false),
        ModelState::Missing => ("Missing".to_owned(), true),
        ModelState::Downloading => (model.progress.map_or_else(|| "Downloading".to_owned(), |p| format!("{:.0}%", p * 100.0)), false),
        ModelState::Unknown => ("Unknown".to_owned(), false),
        ModelState::Error(_) => ("Error".to_owned(), true),
    };
    controls::line(ui.painter(), &value, if matches!(&model.state, ModelState::Downloading) { ty::mono() } else { ty::small() }, 14.0, f32::INFINITY).size().x
        + if icon { 16.0 } else { 0.0 }
}

fn paint_tree_state(ui: &Ui, row: Rect, model: &Model, selected: bool, x: f32) {
    let t = tokens(ui);
    let (value, icon, color) = match &model.state {
        ModelState::Present => ("Present".to_owned(), None, if selected { t.text_primary } else { t.success }),
        ModelState::Missing => ("Missing".to_owned(), Some(Icon::Error), t.error),
        ModelState::Downloading => (model.progress.map_or_else(|| "Downloading".to_owned(), |p| format!("{:.0}%", p * 100.0)), None, if selected { t.text_primary } else { t.text_secondary }),
        ModelState::Unknown => ("Unknown".to_owned(), None, t.text_secondary),
        ModelState::Error(_) => ("Error".to_owned(), Some(Icon::Warning), t.error),
    };
    let has_icon = icon.is_some();
    if let Some(icon) = icon {
        icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(x, row.center().y - 6.0), Vec2::splat(12.0)), icon, IconSize::S12, color);
    }
    let text_x = x + if has_icon { 16.0 } else { 0.0 };
    text(ui, Rect::from_min_max(egui::pos2(text_x, row.min.y + 3.0), egui::pos2(row.max.x - 8.0, row.max.y)), &value, if matches!(&model.state, ModelState::Downloading) { ty::mono() } else { ty::small() }, 14.0, color, Align::Min);
}

fn model_section(ui: &mut Ui, rect: Rect, model: &Model, collapsed: &mut bool, license_id: &mut Option<String>, acts: &mut Vec<Act>) {
    let t = tokens(ui);
    let header = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 24.0));
    let response = ui.interact(header, ui.make_persistent_id("models-model-section"), Sense::click());
    if response.clicked() { *collapsed = !*collapsed; }
    if response.hovered() { ui.painter().rect_filled(header, 0, t.bg_hover); } else { ui.painter().rect_filled(header, 0, t.bg_raised); }
    icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(header.min.x + 8.0, header.center().y - 6.0), Vec2::splat(12.0)), if *collapsed { Icon::ChevronRight } else { Icon::ChevronDown }, IconSize::S12, t.text_secondary);
    text(ui, Rect::from_min_max(egui::pos2(header.min.x + 24.0, header.min.y), header.max), "Model", ty::section_header(), 16.0, t.text_primary, Align::Min);
    if !*collapsed {
        let y = header.max.y + 2.0;
        let mut i = 0;
        model_row(ui, rect, y + i as f32 * 24.0, "Status", |ui, r| paint_status(ui, r, model)); i += 1;
        if let Some(path) = model.path() {
            model_row(ui, rect, y + i as f32 * 24.0, "Path", |ui, r| paint_path(ui, r, path, model, acts)); i += 1;
        }
        if !model.size.is_empty() {
            model_row(ui, rect, y + i as f32 * 24.0, "Size", |ui, r| {
                text(ui, r, &model.size, ty::mono(), 14.0, tokens(ui).text_primary, Align::Min);
                if model.size_is_estimate { ui.interact(r, ui.make_persistent_id(("model-size", &model.id)), Sense::hover()).on_hover_text("Estimated size. The exact size is known after download."); }
            }); i += 1;
        }
        if !model.kind.is_empty() {
            model_row(ui, rect, y + i as f32 * 24.0, "Type", |ui, r| text(ui, r, &model.kind, ty::value(), 16.0, tokens(ui).text_primary, Align::Min)); i += 1;
        }
        if !model.capability.is_empty() {
            model_row(ui, rect, y + i as f32 * 24.0, "Capability", |ui, r| text(ui, r, &model.capability, ty::value(), 16.0, tokens(ui).text_primary, Align::Min)); i += 1;
        }
        if !model.revision.is_empty() {
            model_row(ui, rect, y + i as f32 * 24.0, "Revision", |ui, r| text(ui, r, &model.revision, ty::mono(), 14.0, tokens(ui).text_primary, Align::Min)); i += 1;
        }
        if !model.gating.is_empty() {
            model_row(ui, rect, y + i as f32 * 24.0, "Access", |ui, r| text(ui, r, &model.gating, ty::value(), 16.0, tokens(ui).text_primary, Align::Min)); i += 1;
        }
        if !model.requires.is_empty() {
            let requires = model.requires.iter().map(|(_, label)| label.as_str()).collect::<Vec<_>>().join(", ");
            model_row(ui, rect, y + i as f32 * 24.0, "Requires", |ui, r| text(ui, r, &requires, ty::value(), 16.0, tokens(ui).text_primary, Align::Min)); i += 1;
        }
        if !model.probe_imports.is_empty() {
            let imports = model.probe_imports.join(", ");
            model_row(ui, rect, y + i as f32 * 24.0, "Imports", |ui, r| text(ui, r, &imports, ty::mono(), 14.0, tokens(ui).text_secondary, Align::Min)); i += 1;
        }
        if let Some(issues) = model_issues(model) {
            model_row(ui, rect, y + i as f32 * 24.0, "Issues", |ui, r| {
                text(ui, r, &issues, ty::small(), 14.0, tokens(ui).error, Align::Min);
                ui.interact(r, ui.make_persistent_id(("model-issues", &model.id)), Sense::hover()).on_hover_text(&issues);
            }); i += 1;
        }
        if let Some(note) = model.runtime_note.as_deref() {
            model_row(ui, rect, y + i as f32 * 24.0, "Runtime", |ui, r| text(ui, r, note, ty::small(), 14.0, tokens(ui).text_secondary, Align::Min)); i += 1;
        }
        model_row(ui, rect, y + i as f32 * 24.0, "Licence", |ui, r| {
            if ui.add(buttons::ghost_button("View licence…").at(Pos2::new(r.min.x, r.min.y + 1.0))).clicked() {
                *license_id = Some(model.id.clone());
            }
        });
    }
    ui.painter().rect_filled(Rect::from_min_size(egui::pos2(rect.min.x, rect.max.y - 1.0), Vec2::new(rect.width(), 1.0)), 0, t.border_divider);
}

fn model_row(ui: &mut Ui, section: Rect, y: f32, label: &str, paint: impl FnOnce(&mut Ui, Rect)) {
    let t = tokens(ui);
    let row = Rect::from_min_size(egui::pos2(section.min.x, y), Vec2::new(section.width(), 24.0));
    let label_rect = Rect::from_min_max(egui::pos2(row.min.x + 8.0, row.min.y), egui::pos2(row.min.x + 120.0, row.max.y));
    let control = Rect::from_min_max(egui::pos2(row.min.x + 128.0, row.min.y), egui::pos2(row.max.x - 8.0, row.max.y));
    text(ui, label_rect, label, ty::label(), 16.0, t.text_secondary, Align::Min);
    let mut child = ui.new_child(UiBuilder::new().max_rect(control).layout(Layout::left_to_right(Align::Center)).id_salt(("models-row", label, y.to_bits())));
    child.spacing_mut().item_spacing = Vec2::new(4.0, 0.0);
    paint(&mut child, control);
}

fn paint_status(ui: &mut Ui, rect: Rect, model: &Model) {
    let t = tokens(ui);
    let (icon, label, color) = match &model.state {
        ModelState::Present => (Some(Icon::Success), "Present", t.success),
        ModelState::Missing => (Some(Icon::Error), "Missing", t.error),
        ModelState::Downloading => (None, "Downloading", t.accent_bright),
        ModelState::Unknown => (Some(Icon::Info), "Unknown", t.text_secondary),
        ModelState::Error(_) => (Some(Icon::Warning), "Unavailable", t.error),
    };
    let mut x = rect.min.x;
    if let Some(icon) = icon {
        icons::paint(ui.painter(), Rect::from_min_size(egui::pos2(x, rect.center().y - 7.0), Vec2::splat(14.0)), icon, IconSize::S14, color);
        x += 18.0;
    }
    if let (ModelState::Downloading, Some(p)) = (&model.state, model.progress) {
        let pct = format!("{:.0}%", p * 100.0);
        let w = controls::line(ui.painter(), &pct, ty::mono(), 16.0, f32::INFINITY).size().x;
        text(ui, Rect::from_min_size(egui::pos2(x, rect.min.y), Vec2::new(w, rect.height())), &pct, ty::mono(), 16.0, t.text_primary, Align::Min);
        x += w + 8.0;
    }
    text(ui, Rect::from_min_max(egui::pos2(x, rect.min.y), rect.max), label, ty::value(), 16.0, t.text_primary, Align::Min);
    if let ModelState::Error(error) = &model.state {
        if !error.is_empty() { ui.interact(rect, ui.make_persistent_id(("model-state-error", &model.id)), Sense::hover()).on_hover_text(error); }
    }
}

fn paint_path(ui: &mut Ui, rect: Rect, path: &str, model: &Model, acts: &mut Vec<Act>) {
    let copy_x = rect.max.x - 18.0 - 4.0 - 18.0;
    let path_rect = Rect::from_min_max(rect.min, egui::pos2(copy_x - 4.0, rect.max.y));
    let shown = controls::property::middle_truncate(path, |s| controls::line(ui.painter(), s, ty::mono(), 14.0, f32::INFINITY).size().x <= path_rect.width());
    text(ui, path_rect, &shown, ty::mono(), 14.0, tokens(ui).text_secondary, Align::Min);
    ui.interact(path_rect, ui.make_persistent_id(("models-path", path)), Sense::hover()).on_hover_text(path);
    let copy = Rect::from_min_size(egui::pos2(copy_x, rect.center().y - 9.0), Vec2::splat(18.0));
    let reveal = Rect::from_min_size(egui::pos2(rect.max.x - 18.0, rect.center().y - 9.0), Vec2::splat(18.0));
    if ui.add(buttons::icon_button(Icon::Copy, buttons::IconButtonSize::Small18, "Copy path\nCopy the full path").in_rect(copy)).clicked() {
        ui.ctx().copy_text(path.to_owned());
    }
    let reveal_button = buttons::icon_button(Icon::Reveal, buttons::IconButtonSize::Small18, "Reveal folder\nOpen the containing folder");
    let reveal_button = if model.can_reveal == Some(false) {
        reveal_button.disabled(model.reveal_reason.as_deref().unwrap_or("This location cannot be revealed."))
    } else { reveal_button };
    if ui.add(reveal_button.in_rect(reveal)).clicked() { acts.push(Act::Reveal(model.id.clone())); }
}

/// Paints one run of the operation line at `x` and advances `x` past it.
fn run_text(ui: &Ui, x: &mut f32, line: Rect, value: &str, font: egui::FontId, color: egui::Color32) {
    let w = controls::line(ui.painter(), value, font.clone(), 14.0, f32::INFINITY).size().x;
    text(ui, Rect::from_min_max(egui::pos2(*x, line.min.y), line.max), value, font, 14.0, color, Align::Min);
    *x += w;
}

fn download(ui: &mut Ui, rect: Rect, operation: &OperationInfo, cancel_requested: bool, cancel_note: Option<&str>, cancel_failure: bool, acts: &mut Vec<Act>) {
    let t = tokens(ui);
    let line = Rect::from_min_size(rect.min, Vec2::new(rect.width(), 24.0));
    let label = if cancel_requested { "Stopping model operation…" } else {
        match operation.kind.as_str() {
            "download" => "Downloading",
            "check" => "Refreshing model status",
            "delete" => "Deleting model",
            "install" => "Installing model",
            _ if operation.kind.is_empty() => "Model operation",
            _ => operation.kind.as_str(),
        }
    };
    // Verb in the UI font, then the byte numbers in the mono font (bible §5.12: "Downloading 1.1 of 3.2 GB").
    let mut x = line.min.x + 8.0;
    run_text(ui, &mut x, line, label, ty::small(), t.text_secondary);
    if !operation.bytes_text.is_empty() {
        x += 4.0;
        run_text(ui, &mut x, line, &operation.bytes_text, ty::mono(), t.text_secondary);
    }
    if let Some(note) = cancel_note { run_text(ui, &mut x, line, &format!(" · {note}"), ty::small(), t.text_secondary); }
    let cancel = if operation.cancellable && !cancel_requested {
        buttons::secondary_button("Cancel download")
    } else {
        buttons::secondary_button("Cancel download").disabled(if cancel_requested { "Cancellation requested" } else { "This operation cannot be cancelled" })
    };
    let w = cancel.natural_width(ui);
    if ui.add(cancel.at(Pos2::new(line.max.x - 8.0 - w, line.min.y + 1.0))).clicked() { acts.push(Act::Cancel); }
    let bar_rect = Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.min.y + 24.0), Vec2::new((rect.width() - 16.0).max(0.0), 6.0));
    let mut bar_ui = ui.new_child(UiBuilder::new().max_rect(bar_rect).layout(Layout::top_down(Align::Min)).id_salt("models-download-progress"));
    let progress_label = if operation.unknown_total { "Downloading · total unknown" } else { "Model download" };
    chips::progress(&mut bar_ui, bar_rect.width(), chips::Progress { value: operation.progress, state: if operation.error.is_some() || cancel_failure { chips::State::Failed } else { chips::State::Running }, thin: false, label: progress_label });
    if let Some(error) = operation.error.as_deref() {
        text(ui, Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.min.y + 32.0), Vec2::new((rect.width() - 16.0).max(0.0), 18.0)), error, ty::small(), 14.0, t.error, Align::Min);
        ui.interact(rect, ui.make_persistent_id("models-operation-error"), Sense::hover()).on_hover_text(error);
    } else if cancel_failure {
        text(ui, Rect::from_min_size(egui::pos2(rect.min.x + 8.0, rect.min.y + 32.0), Vec2::new((rect.width() - 16.0).max(0.0), 18.0)), "Cancellation failed", ty::small(), 14.0, t.error, Align::Min);
    }
}

#[cfg(test)]
mod tests {
    use super::Page;
    use std::path::Path;

    #[test]
    fn both_model_fixtures_load_their_sample_selection() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
        let ready = Page::load(&dir.join("models-30.json")).unwrap();
        assert_eq!(ready.selected, "CorridorKey v2");
        fn count(models: &[super::Model]) -> usize { models.iter().map(|m| 1 + count(&m.children)).sum() }
        assert_eq!(ready.fx.groups.iter().map(|g| count(&g.models)).sum::<usize>(), 9);
        let missing = Page::load(&dir.join("models-31.json")).unwrap();
        assert_eq!(missing.selected, "GVM");
        assert!(missing.fx.has_missing());
    }
}
