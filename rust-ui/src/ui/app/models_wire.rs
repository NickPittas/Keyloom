//! App side of the new Models page: builds its snapshot from the old controller's cached `page_data()` and turns the
//! page's acts into controller requests. No IO here; the controller keeps its own startup/tick/close contracts.
use super::*;
use crate::ui::models::{ModelState as OldState, Request};
use crate::ui::models_page::{self as page, Act};

fn state(s: OldState) -> page::ModelState {
    match s {
        OldState::Present => page::ModelState::Present,
        OldState::Missing => page::ModelState::Missing,
        OldState::Downloading => page::ModelState::Downloading,
        OldState::Unknown => page::ModelState::Unknown,
        OldState::Error(e) => page::ModelState::Error(e),
    }
}

impl App {
    /// Rebuild the page's data from the controller's cached state (cheap; called every frame the page is open).
    pub(super) fn refresh_models_page(&mut self, ctx: &egui::Context) {
        self.models.start_background(ctx); // one-shot (guarded inside), as the old page's first draw did
        let d = self.models.page_data();
        let op = d.operation;
        let groups = d.groups.into_iter().map(|g| page::ModelGroup {
            name: g.name,
            expanded: false,
            models: g.models.into_iter().map(|m| {
                let running = op.as_ref().filter(|o| o.model_id.as_deref() == Some(m.id.as_str()));
                page::Model {
                    progress: running.and_then(|o| o.progress),
                    downloaded: running.map(|o| o.bytes_text.clone()).unwrap_or_default(),
                    state: state(m.state),
                    id: m.id,
                    name: m.name,
                    family: m.family,
                    kind: m.kind,
                    capability: m.capability,
                    required: m.required,
                    default_variant: m.default_variant,
                    size: m.size,
                    size_is_estimate: m.size_is_estimate,
                    location: m.location,
                    revision: m.revision,
                    gating: m.gating,
                    terms: m.terms,
                    probe_imports: m.probe_imports,
                    notes: m.notes,
                    requires: m.requires,
                    issues: m.issues,
                    local_runtime: m.local_runtime,
                    runtime_note: m.runtime_note,
                    can_download: Some(m.can_download),
                    download_reason: m.download_reason,
                    can_delete: Some(m.can_delete),
                    delete_reason: m.delete_reason,
                    can_reveal: Some(m.can_reveal),
                    reveal_reason: m.reveal_reason,
                    ..page::Model::default()
                }
            }).collect(),
        }).collect();
        let operation = op.map(|o| page::OperationInfo {
            kind: o.kind, model_id: o.model_id, progress: o.progress, bytes_text: o.bytes_text,
            unknown_total: o.unknown_total, expected_size: o.expected_size, cancellable: o.cancellable, error: o.error,
        });
        self.models_page.update(page::Fixture {
            groups,
            catalog_loaded: d.catalog_loaded,
            startup_error: d.startup_error,
            missing_count: d.missing_count,
            busy: d.busy,
            install_block: d.install_block,
            refresh_block: d.refresh_block,
            operation,
            last_result: d.last_result,
            cancel_requested: d.cancel_requested,
            cancel_note: d.cancel_note,
            cancel_failure: d.cancel_failure,
            local_runtime_note: d.local_runtime_note,
            worker_text: d.worker_text,
            ..page::Fixture::default()
        });
    }

    /// Run what the page asked; a refused request shows its reason as a notice.
    pub(super) fn handle_models_act(&mut self, ctx: &egui::Context, a: Act) {
        let r = match a {
            Act::Download(id) => Request::Download(id),
            Act::InstallRequired => Request::InstallRequired,
            Act::DownloadAll => Request::DownloadAll,
            Act::Cancel => Request::Cancel,
            Act::Delete(id) => Request::Delete(id),
            Act::Refresh => Request::Refresh,
            Act::Reveal(id) => {
                match self.models.artifact_path(&id) {
                    Some(p) => reveal(&p, p.is_file(), &mut self.ai),
                    None => self.ai.notice = Some("Location unavailable until the model status is checked.".into()),
                }
                return;
            }
        };
        if let Err(e) = self.models.request(r, ctx) {
            self.ai.notice = Some(e);
        }
    }
}
