//! Offline status and explicit local model-management actions.
use crate::models::{self, Action, Event as ProcessEvent, Job, Profile, Runtime};
use eframe::egui::{self, Color32, RichText, Ui};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

const BAD: Color32 = Color32::from_rgb(235, 120, 120);

#[derive(Clone, Debug, Default, PartialEq)]
struct Artifact {
    id: String,
    label: String,
    kind: String,
    capability: String,
    revision: Option<String>,
    revision_status: Option<String>,
    gated: Option<Value>,
    provenance: BTreeMap<String, Value>,
    required_profile: bool,
    default_variant: bool,
    dependencies: Vec<String>,
    probe_imports: Vec<String>,
    bytes_required: Option<u64>,
    size_bytes_estimate: Option<u64>,
    install_hint: Option<String>,
    notes: Vec<String>,
    path: Option<String>,
    state: Option<String>,
    present: Option<bool>,
    verified: Option<bool>,
    ready: Option<bool>,
    verify_note: Option<String>,
    problems: Vec<String>,
    last_error: Option<String>,
}

impl Artifact {
    fn from_catalog(v: &Value) -> Option<Self> {
        Some(Self {
            id: models::string(v, "artifact")?,
            label: models::string(v, "label").unwrap_or_default(),
            kind: models::string(v, "kind").unwrap_or_default(),
            capability: models::string(v, "capability").unwrap_or_default(),
            revision: models::string(v, "revision"),
            revision_status: models::string(v, "revision_status"),
            gated: v.get("gated").cloned(),
            provenance: v
                .get("provenance")
                .and_then(Value::as_object)
                .map(|p| p.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
                .unwrap_or_default(),
            required_profile: models::flag(v, "required_profile").unwrap_or(false),
            default_variant: models::flag(v, "default_variant").unwrap_or(false),
            dependencies: models::strings(v, "depends"),
            probe_imports: models::strings(v, "probe_imports"),
            bytes_required: models::number(v, "bytes_required"),
            size_bytes_estimate: models::number(v, "size_bytes_estimate"),
            install_hint: models::string(v, "install_hint"),
            notes: models::strings(v, "notes"),
            ..Self::default()
        })
    }

    fn apply_status(&mut self, v: &Value) {
        self.path = models::string(v, "path");
        self.state = models::string(v, "state");
        self.present = models::flag(v, "present");
        self.verified = models::flag(v, "verified");
        self.ready = models::flag(v, "ready");
        self.verify_note = models::string(v, "verify_note");
        self.problems = models::strings(v, "problems");
    }

    fn clear_status(&mut self) {
        self.path = None;
        self.state = None;
        self.present = None;
        self.verified = None;
        self.ready = None;
        self.verify_note = None;
        self.problems.clear();
    }
}

struct Running {
    action: Action,
    operation_id: Option<String>,
    job: Job,
    terminal: Option<Value>,
    malformed: Option<String>,
    current_artifact: Option<String>,
    bytes_total: Option<u64>,
    progress: Option<Progress>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Progress {
    unit: &'static str,
    done: u64,
    total: Option<u64>,
}

pub struct Page {
    runtime: Result<Runtime, String>,
    artifacts: BTreeMap<String, Artifact>,
    active: Option<Running>,
    cancel_job: Option<Job>,
    cancel_terminal: Option<Value>,
    cancel_requested: bool,
    cancel_note: Option<String>,
    cancel_failure: bool,
    cancel_protocol_error: Option<String>,
    cancel_target_id: Option<String>,
    cancel_request_id: Option<String>,
    cancel_acknowledged: bool,
    last_outcome: Option<(String, bool)>,
    required_ready: Option<bool>,
    assets_in_use: bool,
    startup_error: Option<String>,
    catalog_version: Option<u64>,
    initialized: bool,
    stop_after_active: bool,
    pending_check: bool,
    stop_workers_available: bool,
    unresolved_worker_fences: bool,
    check_expected: BTreeSet<String>,
    check_seen: BTreeSet<String>,
}

impl Page {
    pub fn new(repo: Option<&Path>) -> Self {
        Self::from_runtime(Runtime::resolve(repo))
    }

    fn from_runtime(runtime: Result<Runtime, String>) -> Self {
        let startup_error = runtime.as_ref().err().cloned();
        Self {
            runtime,
            artifacts: BTreeMap::new(),
            active: None,
            cancel_job: None,
            cancel_terminal: None,
            cancel_requested: false,
            cancel_note: None,
            cancel_failure: false,
            cancel_protocol_error: None,
            cancel_target_id: None,
            cancel_request_id: None,
            cancel_acknowledged: false,
            last_outcome: None,
            required_ready: None,
            assets_in_use: false,
            startup_error,
            catalog_version: None,
            initialized: false,
            stop_after_active: false,
            pending_check: false,
            stop_workers_available: false,
            unresolved_worker_fences: false,
            check_expected: BTreeSet::new(),
            check_seen: BTreeSet::new(),
        }
    }

    /// Start the read-only, offline catalog and presence checks. Called by the native window on
    /// launch; `with_ctx` tests deliberately use the same page without starting a real helper.
    pub fn start_background(&mut self, ctx: &egui::Context) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        if self.runtime.is_err() {
            return;
        }
        self.start(Action::Catalog, ctx);
    }

    pub fn busy(&self) -> bool {
        self.active.is_some() || self.cancel_job.is_some()
    }
    pub fn can_cancel(&self) -> bool {
        self.active.as_ref().is_some_and(|r| {
            r.operation_id.is_some()
                && matches!(
                    r.action,
                    Action::Download(_) | Action::DownloadProfile(_) | Action::Repair(_)
                )
        }) && self.cancel_job.is_none()
    }
    pub fn stop_for_close(&mut self, ctx: &egui::Context) {
        self.stop_after_active = true;
        if self.can_cancel() {
            self.cancel(ctx);
        }
    }

    pub fn close_status(&self) -> String {
        if self.cancel_requested {
            "Waiting for the model helper to report its terminal result…".into()
        } else if self.can_cancel() {
            "Request operation cancellation, then wait for the helper to exit.".into()
        } else {
            "Waiting for the current offline status or verification operation to finish.".into()
        }
    }

    pub fn needs_attention(&self) -> bool {
        self.startup_error.is_some() || self.required_ready != Some(true)
    }

    fn required_artifacts(&self) -> Option<Vec<&Artifact>> {
        let mut pending: Vec<_> = self
            .artifacts
            .values()
            .filter(|a| a.required_profile)
            .map(|a| a.id.clone())
            .collect();
        if pending.is_empty() {
            return None;
        }
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id.clone()) {
                continue;
            }
            let artifact = self.artifacts.get(&id)?;
            pending.extend(artifact.dependencies.iter().cloned());
        }
        Some(seen.iter().map(|id| &self.artifacts[id]).collect())
    }

    fn required_is_ready(&self) -> Option<bool> {
        let required = self.required_artifacts()?;
        if required.iter().any(|a| {
            a.state.is_none()
                || a.present.is_none()
                || a.ready.is_none()
                || (!matches!(a.kind.as_str(), "local_code" | "local_package")
                    && a.verified.is_none())
        }) {
            return None;
        }
        Some(required.iter().all(|a| {
            a.present == Some(true)
                && a.ready == Some(true)
                && (matches!(a.kind.as_str(), "local_code" | "local_package")
                    || a.verified == Some(true))
        }))
    }

    pub fn set_asset_safety(
        &mut self,
        blocked: bool,
        can_stop_workers: bool,
        unresolved_fences: bool,
    ) {
        self.assets_in_use = blocked;
        self.stop_workers_available = can_stop_workers;
        self.unresolved_worker_fences = unresolved_fences;
    }

    pub fn blocks_worker_launch(&self) -> bool {
        self.active.as_ref().is_some_and(|r| {
            matches!(
                r.action,
                Action::Download(_) | Action::DownloadProfile(_) | Action::Repair(_)
            )
        })
    }

    pub fn startup_label(&self) -> String {
        if let Some(error) = &self.startup_error {
            format!("OFFLINE · Model status unavailable: {error}")
        } else if self
            .active
            .as_ref()
            .is_some_and(|r| matches!(r.action, Action::Catalog | Action::Check))
            || self.required_ready.is_none()
        {
            "OFFLINE · Checking installed model files…".into()
        } else if self.required_ready == Some(false) {
            let required = self.required_artifacts().unwrap_or_default();
            if !required.is_empty()
                && required
                    .iter()
                    .all(|a| a.state.as_deref() == Some("installed"))
            {
                "OFFLINE · Required files are present but unverified. Run Verify before they are Ready.".into()
            } else {
                "OFFLINE · Required model files are missing or incomplete.".into()
            }
        } else {
            "OFFLINE · READY · Required model assets are verified; required runtime dependencies are present.".into()
        }
    }

    fn start(&mut self, action: Action, ctx: &egui::Context) {
        if self.active.is_some() || self.cancel_job.is_some() {
            return;
        }
        if is_asset_mutation(&action) && self.assets_in_use {
            let message = "Stop all workers and resolve any outstanding job fences before changing model files.".to_string();
            self.last_outcome = Some((message, false));
            return;
        }
        if matches!(action, Action::Check) {
            self.required_ready = None;
            self.check_expected = self
                .required_artifacts()
                .unwrap_or_default()
                .into_iter()
                .map(|a| a.id.clone())
                .collect();
            self.check_seen.clear();
            for artifact in self.artifacts.values_mut() {
                artifact.clear_status();
            }
        }
        if !matches!(action, Action::Catalog | Action::Check) {
            self.last_outcome = None;
            self.cancel_note = None;
            self.cancel_failure = false;
            self.cancel_protocol_error = None;
            self.cancel_target_id = None;
            self.cancel_request_id = None;
            match &action {
                Action::Download(id) | Action::Repair(id) | Action::Verify(id) => {
                    if let Some(item) = self.artifacts.get_mut(id) {
                        item.last_error = None;
                    }
                }
                Action::DownloadProfile(_) | Action::VerifyAll => {
                    for item in self.artifacts.values_mut() {
                        item.last_error = None;
                    }
                }
                _ => {}
            }
        }
        let operation_id = action.requires_operation_id().then(crate::workflow::new_id);
        let result = self
            .runtime
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|r| r.spawn_with_operation_id(&action, operation_id.as_deref()));
        match result {
            Ok(job) => {
                self.startup_error = None;
                self.active = Some(Running {
                    action,
                    operation_id,
                    job,
                    terminal: None,
                    malformed: None,
                    current_artifact: None,
                    bytes_total: None,
                    progress: None,
                });
                ctx.request_repaint();
            }
            Err(e) => {
                self.startup_error = Some(e.clone());
                if !matches!(action, Action::Catalog | Action::Check) {
                    self.last_outcome = Some((e, false));
                }
            }
        }
    }

    fn start_cancel(&mut self, ctx: &egui::Context) {
        if self.cancel_job.is_some() || !self.can_cancel() {
            return;
        }
        self.cancel_requested = true;
        self.cancel_failure = false;
        self.cancel_protocol_error = None;
        let Some(target_id) = self.active.as_ref().and_then(|r| r.operation_id.clone()) else {
            self.cancel_requested = false;
            self.cancel_failure = true;
            self.cancel_note =
                Some("The active helper has no operation ID; cancellation was not sent.".into());
            return;
        };
        self.cancel_target_id = Some(target_id.clone());
        self.cancel_request_id = None;
        self.cancel_acknowledged = false;
        self.cancel_note = Some("Requesting cancellation…".into());
        match self
            .runtime
            .as_ref()
            .map_err(Clone::clone)
            .and_then(|r| r.spawn_with_operation_id(&Action::Cancel, Some(&target_id)))
        {
            Ok(job) => {
                self.cancel_terminal = None;
                self.cancel_job = Some(job);
                ctx.request_repaint();
            }
            Err(e) => {
                self.cancel_failure = true;
                self.cancel_target_id = None;
                self.cancel_note = Some(format!("Could not send cancel request: {e}"));
            }
        }
    }

    pub fn tick(&mut self, ctx: &egui::Context) {
        let active_events: Vec<_> = self
            .active
            .as_ref()
            .map(|r| std::iter::from_fn(|| r.job.try_recv()).collect())
            .unwrap_or_default();
        let mut exited = None;
        for event in active_events {
            match event {
                ProcessEvent::Json(v) => {
                    let operation_id = self.active.as_ref().and_then(|r| r.operation_id.as_deref());
                    if operation_id
                        .is_some_and(|id| models::string(&v, "operation_id").as_deref() != Some(id))
                    {
                        if let Some(r) = &mut self.active {
                            r.malformed.get_or_insert_with(|| {
                                "helper event had a missing or mismatched operation ID".into()
                            });
                        }
                    } else {
                        self.apply_json(v);
                    }
                }
                ProcessEvent::Malformed(e) => {
                    if let Some(r) = &mut self.active {
                        r.malformed.get_or_insert(e);
                    }
                }
                ProcessEvent::Exited { code, stderr } => exited = Some((code, stderr)),
            }
        }
        if let Some((code, stderr)) = exited {
            self.finish_active(code, &stderr, ctx);
        }

        let cancel_events: Vec<_> = self
            .cancel_job
            .as_ref()
            .map(|j| std::iter::from_fn(|| j.try_recv()).collect())
            .unwrap_or_default();
        let mut cancel_exit = None;
        for event in cancel_events {
            match event {
                ProcessEvent::Json(v) => match models::string(&v, "event").as_deref() {
                    Some("op-start") => {
                        let target_matches = self.cancel_target_id.as_ref().is_some_and(|id| {
                            models::string(&v, "operation_id").as_deref() == Some(id)
                        });
                        let request_id =
                            models::string(&v, "request_id").filter(|id| !id.is_empty());
                        if models::string(&v, "op").as_deref() == Some("cancel")
                            && target_matches
                            && request_id.is_some()
                        {
                            let request_id = request_id.unwrap();
                            if self
                                .cancel_request_id
                                .as_deref()
                                .is_some_and(|old| old != request_id)
                            {
                                self.set_cancel_protocol_error(
                                    "cancel op-start event changed its request ID".into(),
                                );
                            } else {
                                self.cancel_request_id = Some(request_id);
                            }
                        } else {
                            self.set_cancel_protocol_error(
                                "cancel op-start event had a missing or mismatched operation/request ID".into(),
                            );
                        }
                    }
                    Some("cancel-requested") => {
                        let target_matches = self.cancel_target_id.as_ref().is_some_and(|id| {
                            models::string(&v, "operation_id").as_deref() == Some(id)
                        });
                        let request_matches = self.cancel_request_id.as_ref().is_some_and(|id| {
                            models::string(&v, "request_id").as_deref() == Some(id)
                        });
                        if models::string(&v, "op").as_deref() == Some("cancel")
                            && target_matches
                            && request_matches
                        {
                            self.cancel_acknowledged = true;
                        } else {
                            self.set_cancel_protocol_error(
                                "cancel-requested event had a missing or mismatched operation/request ID".into(),
                            );
                        }
                    }
                    Some("op-done") => {
                        let target_matches = self.cancel_target_id.as_ref().is_some_and(|id| {
                            models::string(&v, "operation_id").as_deref() == Some(id)
                        });
                        let request_matches = self.cancel_request_id.as_ref().is_some_and(|id| {
                            models::string(&v, "request_id").as_deref() == Some(id)
                        });
                        let ok = models::flag(&v, "ok");
                        if models::string(&v, "op").as_deref() == Some("cancel")
                            && target_matches
                            && ok.is_some()
                        {
                            if !request_matches {
                                self.set_cancel_protocol_error(
                                    "cancel terminal event had a missing or mismatched request ID"
                                        .into(),
                                );
                            }
                            self.cancel_terminal = Some(v);
                        } else {
                            self.set_cancel_protocol_error(
                                "cancel terminal event had a missing or mismatched operation/request ID".into(),
                            );
                        }
                    }
                    _ => self
                        .set_cancel_protocol_error(format!("unexpected cancel helper event: {v}")),
                },
                ProcessEvent::Malformed(e) => self.set_cancel_protocol_error(e),
                ProcessEvent::Exited { code, stderr } => cancel_exit = Some((code, stderr)),
            }
        }
        if let Some((code, stderr)) = cancel_exit {
            let accepted =
                self.cancel_terminal.as_ref().is_some_and(|v| {
                    models::string(v, "event").as_deref() == Some("op-done")
                        && models::string(v, "op").as_deref() == Some("cancel")
                        && self.cancel_target_id.as_ref().is_some_and(|id| {
                            models::string(v, "operation_id").as_deref() == Some(id)
                        })
                        && self.cancel_request_id.as_ref().is_some_and(|id| {
                            models::string(v, "request_id").as_deref() == Some(id)
                        })
                        && models::flag(v, "ok") == Some(true)
                        && models::flag(v, "requested") == Some(true)
                }) && self.cancel_protocol_error.is_none()
                    && self.cancel_acknowledged;
            let detail = self
                .cancel_terminal
                .as_ref()
                .and_then(|v| models::string(v, "error").or_else(|| models::string(v, "message")));
            let protocol_error = self.cancel_protocol_error.clone();
            self.cancel_note = Some(if code == Some(0) && accepted {
                if self.active.is_some() {
                    "Cancel signal sent; waiting for the active helper's terminal result…".into()
                } else {
                    "Cancel request was accepted; the active helper has exited. See its result."
                        .into()
                }
            } else {
                self.cancel_failure = true;
                format!(
                    "Cancel request failed (exit {code:?}){}{}{}",
                    detail.map_or_else(String::new, |s| format!(": {s}")),
                    protocol_error.map_or_else(String::new, |e| format!("; protocol: {e}")),
                    if stderr.is_empty() {
                        String::new()
                    } else {
                        format!("; stderr: {stderr}")
                    }
                )
            });
            self.cancel_job = None;
            self.cancel_target_id = None;
            self.cancel_request_id = None;
            self.cancel_acknowledged = false;
        }

        if self.pending_check
            && !self.stop_after_active
            && self.active.is_none()
            && self.cancel_job.is_none()
        {
            self.pending_check = false;
            self.start(Action::Check, ctx);
        }

        if self.busy() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    fn set_cancel_protocol_error(&mut self, error: String) {
        self.cancel_protocol_error
            .get_or_insert_with(|| error.clone());
        self.cancel_failure = true;
        self.cancel_note = Some(error);
    }

    fn apply_json(&mut self, v: Value) {
        match models::string(&v, "event").as_deref() {
            Some("catalog") => {
                self.catalog_version = models::number(&v, "catalog_version");
                if let Some(items) = v.get("artifacts").and_then(Value::as_array) {
                    self.artifacts = items
                        .iter()
                        .filter_map(Artifact::from_catalog)
                        .map(|a| (a.id.clone(), a))
                        .collect();
                }
            }
            Some("status") | Some("verify") => {
                if let Some(id) = models::string(&v, "artifact") {
                    if let Some(item) = self.artifacts.get_mut(&id) {
                        item.apply_status(&v);
                        if self
                            .active
                            .as_ref()
                            .is_some_and(|r| r.action == Action::Check)
                            && self.check_expected.contains(&id)
                        {
                            self.check_seen.insert(id);
                        }
                    }
                }
            }
            Some("artifact-start") => {
                if let Some(r) = &mut self.active {
                    r.current_artifact =
                        models::string(&v, "artifact").or_else(|| r.current_artifact.clone());
                    r.bytes_total = models::number(&v, "bytes_total");
                }
            }
            Some("progress") => {
                if let Some(r) = &mut self.active {
                    r.current_artifact =
                        models::string(&v, "artifact").or_else(|| r.current_artifact.clone());
                    r.progress = parse_progress(&v);
                }
            }
            Some("artifact-error") => {
                if let Some(id) = models::string(&v, "artifact") {
                    if let Some(item) = self.artifacts.get_mut(&id) {
                        item.last_error = Some(format!(
                            "{}: {}",
                            models::string(&v, "error").unwrap_or_else(|| "error".into()),
                            models::string(&v, "message").unwrap_or_default()
                        ));
                    }
                }
            }
            Some("op-done") => {
                if let Some(r) = &mut self.active {
                    r.terminal = Some(v);
                }
            }
            Some("cancelled") => {
                // A sticky cancel failure outranks this progress note; never overwrite it.
                if !self.cancel_failure
                    && matches!(
                        models::string(&v, "op").as_deref(),
                        Some("download" | "repair")
                    )
                {
                    self.cancel_note = Some(
                        "The active helper reports cancellation; waiting for its terminal result…"
                            .into(),
                    );
                }
            }
            _ => {}
        }
    }

    fn finish_active(&mut self, code: Option<i32>, stderr: &str, ctx: &egui::Context) {
        let Some(r) = self.active.take() else {
            return;
        };
        let op = r.action.name();
        let terminal = r.terminal;
        let term_ok = terminal.as_ref().and_then(|v| models::flag(v, "ok"));
        let term_op = terminal.as_ref().and_then(|v| models::string(v, "op"));
        let cancelled = terminal.as_ref().and_then(|v| models::flag(v, "cancelled")) == Some(true);
        let message = terminal.as_ref().and_then(|v| models::string(v, "message"));
        let error = terminal.as_ref().and_then(|v| models::string(v, "error"));
        let operation_id_matches = terminal.as_ref().is_some_and(|v| {
            !r.action.requires_operation_id()
                || r.operation_id
                    .as_ref()
                    .is_some_and(|id| models::string(v, "operation_id").as_deref() == Some(id))
        });
        let check_protocol_valid = op == "check"
            && r.malformed.is_none()
            && terminal_protocol_valid(
                &r.action,
                terminal.as_ref(),
                code,
                r.operation_id.as_deref(),
            );
        let missing_check_statuses: Vec<_> = self
            .check_expected
            .difference(&self.check_seen)
            .cloned()
            .collect();
        let check_coverage_complete =
            !self.check_expected.is_empty() && missing_check_statuses.is_empty();
        if op == "check" {
            self.required_ready = if check_protocol_valid && check_coverage_complete {
                self.required_is_ready()
            } else {
                None
            };
        }
        let looks_successful = r.malformed.is_none()
            && operation_succeeded(
                &r.action,
                terminal.as_ref(),
                code,
                r.operation_id.as_deref(),
            );
        let mut outcome = if terminal.is_none() {
            format!("{op} ended without an op-done terminal event (helper exit {code:?})")
        } else if term_op.as_deref() != Some(op) {
            format!("{op} returned a mismatched terminal event (helper exit {code:?})")
        } else if r.action.requires_operation_id() && !operation_id_matches {
            format!("{op} terminal event had a missing or mismatched operation ID")
        } else if r.malformed.is_some() {
            format!(
                "{op} output was malformed; terminal result is not trusted (helper exit {code:?})"
            )
        // The backend exits 1 on a legitimately cancelled operation; earlier branches
        // already rejected malformed or mismatched terminals, so exit 1 is expected here.
        } else if code != Some(0) && !(cancelled && code == Some(1)) {
            format!("{op} terminal event and process exit disagree (helper exit {code:?})")
        } else if op == "check" && check_protocol_valid && !check_coverage_complete {
            let missing = if self.check_expected.is_empty() {
                "no required artifacts were reported by the catalog".into()
            } else {
                missing_check_statuses.join(", ")
            };
            format!("check returned incomplete status coverage; missing: {missing}")
        } else if op == "check" && term_ok == Some(false) {
            "check completed; one or more models are missing or unverified".into()
        } else if cancelled {
            format!("{op} cancelled (helper exit {code:?})")
        } else if term_ok == Some(false) {
            let detail = error
                .or(message)
                .unwrap_or_else(|| "one or more assets did not pass".into());
            format!("{op} finished with an incomplete result: {detail} (helper exit {code:?})")
        } else if term_ok != Some(true) {
            format!("{op} terminal event did not report ok:true (helper exit {code:?})")
        } else {
            format!("{op} completed (helper exit 0; op-done ok)")
        };
        if let Some(e) = r.malformed {
            outcome.push_str(&format!("; {e}"));
        }
        if !stderr.is_empty() {
            outcome.push_str(&format!("; stderr: {stderr}"));
        }
        if self.cancel_requested && !cancelled {
            outcome = format!("Operation ended before cancellation was confirmed: {outcome}");
        }
        if r.action.name() != "catalog" && r.action.name() != "check" {
            self.last_outcome = Some((outcome.clone(), looks_successful && !cancelled));
            self.required_ready = None;
        } else if r.action.name() == "catalog" && !looks_successful {
            self.startup_error = Some(outcome.clone());
            self.last_outcome = Some((outcome.clone(), false));
            self.required_ready = Some(false);
        } else if op == "check" && !check_protocol_valid {
            self.startup_error = Some(outcome.clone());
            self.required_ready = None;
        }
        if r.action.name() == "catalog" && looks_successful {
            self.required_ready = None;
        }
        if !self.stop_after_active
            && matches!(
                r.action,
                Action::Catalog
                    | Action::Download(_)
                    | Action::DownloadProfile(_)
                    | Action::Repair(_)
                    | Action::VerifyAll
                    | Action::Verify(_)
            )
        {
            self.required_ready = None;
        }
        if self.cancel_requested && !self.cancel_failure {
            if cancelled {
                self.cancel_note = Some("The helper reported cancellation and exited.".into());
            } else {
                self.cancel_note = Some("The helper exited before confirming cancellation.".into());
            }
        }
        if op == "check" && check_protocol_valid {
            if !check_coverage_complete {
                self.startup_error = Some(outcome.clone());
                self.required_ready = None;
            } else if self.required_ready.is_none() {
                self.startup_error = Some(
                    "The helper returned a model check without all required artifact statuses."
                        .into(),
                );
            } else {
                self.startup_error = None;
            }
        }
        let follow_up_check = matches!(
            r.action,
            Action::Download(_)
                | Action::DownloadProfile(_)
                | Action::Repair(_)
                | Action::VerifyAll
                | Action::Verify(_)
        ) || matches!(r.action, Action::Catalog) && looks_successful;
        if !self.stop_after_active && follow_up_check {
            // Refresh presence and cached verification using the same roots; this check never hashes.
            if self.cancel_job.is_some() {
                self.pending_check = true;
            } else {
                self.start(Action::Check, ctx);
            }
        }
        self.cancel_requested = false;
    }

    fn cancel(&mut self, ctx: &egui::Context) {
        self.start_cancel(ctx);
    }

    pub fn startup_ui(&mut self, ui: &mut Ui) -> bool {
        if !self.initialized {
            return false;
        }
        let mut open = false;
        ui.horizontal_wrapped(|ui| {
            let needs_attention = self.needs_attention();
            let error_or_missing =
                self.startup_error.is_some() || self.required_ready == Some(false);
            ui.label(
                RichText::new(self.startup_label()).color(if error_or_missing {
                    BAD
                } else {
                    Color32::from_rgb(150, 190, 150)
                }),
            );
            if needs_attention && ui.button("Models…").clicked() {
                open = true;
            }
        });
        open
    }

    pub fn ui(&mut self, ui: &mut Ui) -> bool {
        if !self.initialized {
            self.start_background(ui.ctx());
        }
        self.tick(ui.ctx());
        let mut stop_workers = false;
        egui::Panel::bottom("models_actions").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                let idle = !self.busy();
                let can_change = idle && !self.assets_in_use;
                if self.assets_in_use {
                    ui.colored_label(BAD, "Install and Repair are disabled until workers have exited and job fences are resolved.");
                    if self.stop_workers_available && ui.button("Stop workers").clicked() {
                        stop_workers = true;
                    }
                    if self.unresolved_worker_fences {
                        ui.colored_label(BAD, "Unresolved Key job fences still block model changes. Return to Key to resolve them.");
                    }
                } else if !idle {
                    ui.colored_label(BAD, "One model operation at a time; wait for it to finish before starting another.");
                }
                if ui.add_enabled(can_change, egui::Button::new("Install required models")).on_disabled_hover_text("Stop AI workers before changing model files.").clicked() {
                    self.start(Action::DownloadProfile(Profile::Required), ui.ctx());
                }
                if ui.add_enabled(can_change, egui::Button::new("Install full profile")).on_disabled_hover_text("Stop AI workers before changing model files.").clicked() {
                    self.start(Action::DownloadProfile(Profile::Full), ui.ctx());
                }
                if ui.add_enabled(idle, egui::Button::new("Verify all")).clicked() {
                    self.start(Action::VerifyAll, ui.ctx());
                }
                if ui.add_enabled(idle, egui::Button::new("Refresh status")).clicked() {
                    self.required_ready = None;
                    self.start(Action::Check, ui.ctx());
                }
                if ui.add_enabled(self.can_cancel(), egui::Button::new("Cancel operation")).clicked() {
                    self.cancel(ui.ctx());
                }
            });
            if let Some(r) = &self.active {
                ui.label(RichText::new(format!("{} running…", r.action.name())).strong());
                let activity = r.current_artifact.as_deref().map(|id| format!(" · {id}")).unwrap_or_default();
                let size = r.bytes_total.map(|n| format!(" · expected size {}", format_bytes(n))).unwrap_or_default();
                let (fraction, progress_text, indeterminate) = match r.progress {
                    Some(p) => match p.total.filter(|n| *n > 0) {
                        Some(total) => ((p.done as f32 / total as f32).clamp(0.0, 1.0),
                            format!("{} / {} {}", p.done, total, p.unit), false),
                        None => (0.0, format!("{} {}", p.done, p.unit), true),
                    },
                    None => (0.0, "progress unit unavailable".into(), true),
                };
                ui.add(egui::ProgressBar::new(fraction).animate(indeterminate).text(format!("Working{activity}{size} · {progress_text}")));
                if let Some(id) = &r.current_artifact {
                    ui.label(format!("Current asset: {id}"));
                }
            }
            if let Some(note) = &self.cancel_note { ui.label(note); }
            if let Some((outcome, ok)) = &self.last_outcome {
                ui.colored_label(if *ok { Color32::from_rgb(150, 190, 150) } else { BAD }, outcome);
            }
            if let Some(error) = &self.startup_error { ui.colored_label(BAD, error); }
            if let Some(e) = self.active.as_ref().and_then(|r| r.malformed.as_ref()) { ui.colored_label(BAD, e); }
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                ui.heading("Models & runtime");
                let status_color = if self.startup_error.is_some() || self.required_ready == Some(false) { BAD } else { Color32::from_rgb(150, 190, 150) };
                ui.label(RichText::new(self.startup_label()).color(status_color));
                ui.label("Catalog and startup presence checks run offline. Startup does not hash large files or download models; downloads and verification run only when requested.");
                if self.artifacts.is_empty() {
                    ui.label("Waiting for the local model catalog…");
                } else {
                    ui.label(format!("Catalog version {} · {} assets", self.catalog_version.map_or("unknown".into(), |v| v.to_string()), self.artifacts.len()));
                    ui.label("Catalog revision, gating, and provenance are shown as recorded; no license approval is inferred.");
                    if self.artifacts.values().any(|a| a.id == "sam2-package" && a.state.is_some() && a.state.as_deref() != Some("installed")) {
                        ui.colored_label(BAD, "Runtime repair required: SAM2 checkpoint downloads do not install its pinned Python package. Repair the runtime to supply this dependency.");
                    }
                    ui.label("Capability labels describe asset use and do not confirm that a workflow route is integrated.");
                    let actions_enabled = !self.busy();
                    let items: Vec<_> = self.artifacts.values().cloned().collect();
                    for item in items {
                        ui.separator();
                        ui.horizontal_wrapped(|ui| {
                            ui.label(RichText::new(&item.label).strong());
                            if item.required_profile { ui.label("Required profile"); }
                            if item.default_variant { ui.label("Default variant"); }
                            ui.label(format!("{} · {}", item.capability, item.kind));
                        });
                        let state = match item.state.as_deref() {
                            Some("installed") => "Complete",
                            Some("partial") => "Partial",
                            Some("missing") => "Missing",
                            _ => "Status not checked",
                        };
                        ui.label(format!(
                            "{state} · present: {} · verified: {} · ready: {}",
                            bool_status(item.present),
                            bool_status(item.verified),
                            bool_status(item.ready)
                        ));
                        ui.label(format!("Catalog revision: {} · {}", item.revision_status.as_deref().unwrap_or("status not reported"), item.revision.as_deref().unwrap_or("revision not reported")));
                        ui.label(format!("Catalog gating: {}", catalog_value(item.gated.as_ref())));
                        if item.provenance.is_empty() {
                            ui.label("Catalog provenance and access terms: not reported");
                        } else {
                            ui.label("Catalog provenance and access terms:");
                            for (key, value) in &item.provenance {
                                ui.label(format!("{key}: {}", catalog_value(Some(value))));
                            }
                        }
                        for note in &item.notes { ui.label(note); }
                        let size = item.bytes_required.or(item.size_bytes_estimate);
                        ui.label(size.map_or_else(|| "Expected size unavailable".into(), |n| format!("Expected: {}", format_bytes(n))));
                        if !item.dependencies.is_empty() {
                            let deps = item.dependencies.iter().map(|id| self.artifacts.get(id).map_or(id.as_str(), |a| a.label.as_str())).collect::<Vec<_>>().join(", ");
                            ui.label(format!("Requires: {deps}"));
                        }
                        if !item.probe_imports.is_empty() {
                            ui.label(format!("Runtime import requirements: {}", item.probe_imports.join(", ")));
                        }
                        if let Some(path) = &item.path { ui.label(format!("Location: {path}")); }
                        if let Some(note) = &item.verify_note { ui.label(note); }
                        if !item.problems.is_empty() { ui.colored_label(BAD, item.problems.join("; ")); }
                        if let Some(error) = &item.last_error { ui.colored_label(BAD, error); }
                        if matches!(item.kind.as_str(), "local_package" | "local_code") {
                            if item.state.is_some() && item.state.as_deref() != Some("installed") {
                                let message = if item.kind == "local_package" {
                                    "Runtime repair required; the model helper cannot install this package."
                                } else {
                                    "Runtime repair required: restore vendored engine code with the app runtime."
                                };
                                ui.colored_label(BAD, message);
                                if let Some(hint) = &item.install_hint { ui.label(hint); }
                                for note in &item.notes { ui.label(note); }
                            }
                        } else {
                            ui.horizontal_wrapped(|ui| {
                                if ui.add_enabled(actions_enabled && !self.assets_in_use, egui::Button::new("Install")).on_disabled_hover_text("Stop AI workers before changing model files.").clicked() {
                                    self.start(Action::Download(item.id.clone()), ui.ctx());
                                }
                                if ui.add_enabled(actions_enabled, egui::Button::new("Verify")).clicked() {
                                    self.start(Action::Verify(item.id.clone()), ui.ctx());
                                }
                                if ui.add_enabled(actions_enabled && !self.assets_in_use, egui::Button::new("Repair")).on_disabled_hover_text("Stop AI workers before replacing model files.").clicked() {
                                    self.start(Action::Repair(item.id.clone()), ui.ctx());
                                }
                            });
                        }
                    }
                }
            });
        });
        stop_workers
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut unit = 0;
    while n >= 1000.0 && unit < UNITS.len() - 1 {
        n /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{n:.1} {}", UNITS[unit])
    }
}

fn operation_succeeded(
    action: &Action,
    terminal: Option<&Value>,
    code: Option<i32>,
    operation_id: Option<&str>,
) -> bool {
    terminal_protocol_valid(action, terminal, code, operation_id)
        && terminal.is_some_and(|v| models::flag(v, "ok") == Some(true))
}

fn terminal_protocol_valid(
    action: &Action,
    terminal: Option<&Value>,
    code: Option<i32>,
    operation_id: Option<&str>,
) -> bool {
    code == Some(0)
        && terminal.is_some_and(|v| {
            models::string(v, "event").as_deref() == Some("op-done")
                && models::string(v, "op").as_deref() == Some(action.name())
                && models::flag(v, "ok").is_some()
                && (!action.requires_operation_id()
                    || operation_id
                        .is_some_and(|id| models::string(v, "operation_id").as_deref() == Some(id)))
        })
}

fn is_asset_mutation(action: &Action) -> bool {
    matches!(
        action,
        Action::Download(_) | Action::DownloadProfile(_) | Action::Repair(_)
    )
}

fn parse_progress(v: &Value) -> Option<Progress> {
    match models::string(v, "unit").as_deref()? {
        "bytes" => Some(Progress {
            unit: "bytes",
            done: models::number(v, "bytes")?,
            total: models::number(v, "bytes_total"),
        }),
        "files" => Some(Progress {
            unit: "files",
            done: models::number(v, "files_done")?,
            total: models::number(v, "files_total"),
        }),
        _ => None,
    }
}

fn catalog_value(value: Option<&Value>) -> String {
    value.map_or_else(
        || "not reported".into(),
        |v| match v {
            Value::String(s) => s.clone(),
            _ => v.to_string(),
        },
    )
}

fn bool_status(value: Option<bool>) -> &'static str {
    match value {
        Some(true) => "yes",
        Some(false) => "no",
        None => "not reported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Pos2, RawInput, Rect, Vec2};

    fn render(page: &mut Page) {
        let ctx = egui::Context::default();
        super::super::theme::apply(&ctx);
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(360.0, 560.0))),
            ..Default::default()
        };
        for _ in 0..2 {
            ctx.run_ui(raw.clone(), |ui| {
                page.ui(ui);
            })
            .textures_delta
            .clear();
        }
    }

    #[test]
    fn fake_catalog_status_and_missing_runtime_package_render() {
        let mut page = Page::from_runtime(Err("fake helper unavailable".into()));
        page.startup_error = None;
        page.catalog_version = Some(7);
        let catalog = serde_json::json!({"event":"catalog","catalog_version":7,"artifacts":[
            {"artifact":"key","label":"Key weights","kind":"hf_files","capability":"key","required_profile":true,"bytes_required":200,"depends":[]},
            {"artifact":"sam2-package","label":"SAM2 Python package","kind":"local_package","capability":"hint-track","install_hint":"Repair the Python runtime","required_profile":false,"depends":[]}
        ]});
        page.apply_json(catalog);
        assert_eq!(page.artifacts["key"].revision.as_deref(), None);
        assert_eq!(page.artifacts["sam2-package"].gated, None);
        page.artifacts.get_mut("key").unwrap().apply_status(&serde_json::json!({"artifact":"key","state":"installed","present":true,"ready":false,"verified":false,"verify_state":"unknown (not verified yet)","path":"/model/key"}));
        page.artifacts.get_mut("sam2-package").unwrap().apply_status(&serde_json::json!({"artifact":"sam2-package","state":"missing","present":false,"ready":false,"verified":false}));
        page.required_ready = Some(false);
        render(&mut page);
        assert_eq!(page.artifacts["key"].state.as_deref(), Some("installed"));
        assert_eq!(page.artifacts["key"].present, Some(true));
        assert_eq!(page.artifacts["key"].verified, Some(false));
        assert_eq!(page.artifacts["key"].ready, Some(false));
        assert_eq!(
            page.artifacts["sam2-package"].state.as_deref(),
            Some("missing")
        );
        assert!(page.startup_label().contains("present but unverified"));
        assert!(page.needs_attention());
        assert!(format_bytes(1_500_000_000).contains("GB"));
    }

    #[test]
    fn terminal_result_requires_real_exit_and_op_done() {
        let action = Action::Download("key".into());
        let terminal = serde_json::json!({"event":"op-done","op":"download","operation_id":"app-id","ok":true});
        assert!(operation_succeeded(
            &action,
            Some(&terminal),
            Some(0),
            Some("app-id")
        ));
        assert!(!operation_succeeded(
            &action,
            Some(&terminal),
            Some(0),
            Some("other-id")
        ));
        assert!(!operation_succeeded(&action, None, Some(0), Some("app-id")));
        assert!(!operation_succeeded(
            &action,
            Some(&terminal),
            Some(7),
            Some("app-id")
        ));
        let incomplete = serde_json::json!({"event":"op-done","op":"download"});
        assert!(!operation_succeeded(
            &action,
            Some(&incomplete),
            Some(0),
            Some("app-id")
        ));
    }

    #[test]
    fn helper_events_with_another_operation_id_cannot_complete_the_active_job() {
        let mut page = Page::from_runtime(Err("unused".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        page.active = Some(Running {
            action: Action::Download("weights".into()),
            operation_id: Some("active-id".into()),
            job: Job::from_receiver(rx),
            terminal: None,
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-done","op":"download","operation_id":"other-id","ok":true
        })))
        .unwrap();
        tx.send(ProcessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })
        .unwrap();

        page.tick(&egui::Context::default());

        assert!(page.active.is_none());
        assert_eq!(page.last_outcome.as_ref().map(|v| v.1), Some(false));
        assert!(page
            .last_outcome
            .as_ref()
            .unwrap()
            .0
            .contains("operation ID"));
    }

    #[test]
    fn missing_check_is_domain_status_and_keeps_prior_operation_outcome() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.apply_json(serde_json::json!({"event":"catalog","artifacts":[
            {"artifact":"weights","kind":"hf_files","required_profile":true,"depends":[],"probe_imports":["torch","timm.layers"],"revision":"rev-1","revision_status":"pinned","gated":"manual","provenance":{"source":"upstream","weights_license":"recorded"}}
        ]}));
        page.artifacts.get_mut("weights").unwrap().apply_status(&serde_json::json!({
            "artifact":"weights","state":"installed","present":true,"verified":false,"ready":false
        }));
        page.required_ready = Some(false);
        page.check_expected.insert("weights".into());
        page.check_seen.insert("weights".into());
        page.last_outcome = Some(("previous repair result".into(), false));
        page.active = Some(Running {
            action: Action::Check,
            operation_id: None,
            job: Job::disconnected(),
            terminal: Some(serde_json::json!({"event":"op-done","op":"check","ok":false})),
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });
        let ctx = egui::Context::default();
        page.finish_active(Some(0), "", &ctx);
        assert!(page.startup_error.is_none());
        assert_eq!(page.required_ready, Some(false));
        assert_eq!(
            page.last_outcome.as_ref().map(|x| x.0.as_str()),
            Some("previous repair result")
        );
        assert_eq!(
            page.artifacts["weights"].state.as_deref(),
            Some("installed")
        );
        assert_eq!(page.artifacts["weights"].verified, Some(false));
        assert_eq!(page.artifacts["weights"].revision.as_deref(), Some("rev-1"));
        assert_eq!(
            page.artifacts["weights"].probe_imports,
            ["torch", "timm.layers"]
        );
        assert_eq!(
            page.artifacts["weights"].revision_status.as_deref(),
            Some("pinned")
        );
        assert_eq!(
            page.artifacts["weights"].gated,
            Some(serde_json::json!("manual"))
        );
        assert_eq!(
            page.artifacts["weights"].provenance["weights_license"],
            "recorded"
        );
    }

    #[test]
    fn check_with_missing_dependency_status_invalidates_old_ready_result() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.apply_json(serde_json::json!({"event":"catalog","artifacts":[
            {"artifact":"engine","kind":"local_code","required_profile":false,"depends":[]},
            {"artifact":"weights","kind":"hf_files","required_profile":true,"depends":["engine"]}
        ]}));
        for id in ["engine", "weights"] {
            page.artifacts
                .get_mut(id)
                .unwrap()
                .apply_status(&serde_json::json!({
                    "artifact":id,"state":"installed","present":true,"verified":true,"ready":true
                }));
        }
        page.required_ready = Some(true);
        page.check_expected = page
            .required_artifacts()
            .unwrap()
            .into_iter()
            .map(|a| a.id.clone())
            .collect();
        page.check_seen.insert("weights".into());
        page.active = Some(Running {
            action: Action::Check,
            operation_id: None,
            job: Job::disconnected(),
            terminal: Some(serde_json::json!({"event":"op-done","op":"check","ok":true})),
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });

        page.finish_active(Some(0), "", &egui::Context::default());

        assert_eq!(page.required_ready, None);
        assert!(page.startup_error.as_deref().unwrap().contains("engine"));
    }

    #[test]
    fn empty_check_coverage_cannot_report_ready() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.apply_json(serde_json::json!({"event":"catalog","artifacts":[
            {"artifact":"weights","kind":"hf_files","required_profile":true,"depends":[]}
        ]}));
        page.artifacts
            .get_mut("weights")
            .unwrap()
            .apply_status(&serde_json::json!({
                "artifact":"weights","state":"installed","present":true,"verified":true,"ready":true
            }));
        page.required_ready = Some(true);
        page.check_expected.insert("weights".into());
        page.active = Some(Running {
            action: Action::Check,
            operation_id: None,
            job: Job::disconnected(),
            terminal: Some(serde_json::json!({"event":"op-done","op":"check","ok":true})),
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });

        page.finish_active(Some(0), "", &egui::Context::default());

        assert_eq!(page.required_ready, None);
        assert!(page.startup_error.as_deref().unwrap().contains("weights"));
    }

    #[test]
    fn status_check_start_failure_does_not_replace_operation_result() {
        let mut page = Page::from_runtime(Err("helper unavailable".into()));
        page.apply_json(serde_json::json!({"event":"catalog","artifacts":[
            {"artifact":"weights","kind":"hf_files","required_profile":true,"depends":[]}
        ]}));
        page.artifacts
            .get_mut("weights")
            .unwrap()
            .apply_status(&serde_json::json!({
                "artifact":"weights","state":"installed","present":true,"verified":true,"ready":true
            }));
        page.required_ready = Some(true);
        page.last_outcome = Some(("repair failed: disk full".into(), false));
        page.start(Action::Check, &egui::Context::default());
        assert!(page
            .startup_error
            .as_deref()
            .unwrap()
            .contains("helper unavailable"));
        assert_eq!(
            page.last_outcome.as_ref().map(|x| x.0.as_str()),
            Some("repair failed: disk full")
        );
        assert_eq!(page.required_ready, None);
        assert_eq!(page.artifacts["weights"].state, None);
    }

    #[test]
    fn progress_never_treats_file_counts_as_bytes() {
        assert_eq!(
            parse_progress(&serde_json::json!({"unit":"files","files_done":2,"files_total":5})),
            Some(Progress {
                unit: "files",
                done: 2,
                total: Some(5)
            })
        );
        assert_eq!(
            parse_progress(&serde_json::json!({"unit":"bytes","bytes":20,"bytes_total":100})),
            Some(Progress {
                unit: "bytes",
                done: 20,
                total: Some(100)
            })
        );
        assert_eq!(
            parse_progress(&serde_json::json!({"unit":"unknown","files_done":2,"files_total":5})),
            None
        );
        assert_eq!(
            parse_progress(&serde_json::json!({"files_done":2,"files_total":5})),
            None
        );
    }

    #[test]
    fn cancel_failure_survives_active_exit_and_late_success_is_not_waiting() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.stop_after_active = true;
        page.cancel_requested = true;
        page.cancel_failure = true;
        page.cancel_note = Some("Could not send cancel request: cache is read-only".into());
        page.active = Some(Running {
            action: Action::Repair("weights".into()),
            operation_id: Some("repair-id".into()),
            job: Job::disconnected(),
            terminal: Some(
                serde_json::json!({"event":"op-done","op":"repair","operation_id":"repair-id","ok":false}),
            ),
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });
        page.finish_active(Some(1), "", &egui::Context::default());
        assert_eq!(
            page.cancel_note.as_deref(),
            Some("Could not send cancel request: cache is read-only")
        );

        page.apply_json(serde_json::json!({"event":"cancelled","op":"cancel"}));
        assert_eq!(
            page.cancel_note.as_deref(),
            Some("Could not send cancel request: cache is read-only")
        );
        assert!(!page.cancel_note.as_deref().unwrap().contains("waiting"));
    }

    #[test]
    fn late_cancelled_event_does_not_replace_failure_note_after_both_helpers_exit() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.stop_after_active = true;
        let (tx, rx) = std::sync::mpsc::channel();
        page.cancel_job = Some(Job::from_receiver(rx));
        page.cancel_requested = true;
        page.cancel_target_id = Some("download-id".into());
        page.active = Some(Running {
            action: Action::Download("key".into()),
            operation_id: Some("download-id".into()),
            job: Job::disconnected(),
            terminal: None,
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });
        tx.send(ProcessEvent::Malformed("invalid JSONL".into()))
            .unwrap();
        tx.send(ProcessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })
        .unwrap();
        page.tick(&egui::Context::default());
        assert!(page.cancel_failure);

        // The active helper's cancelled event arrives after the cancel helper already
        // failed its protocol; its exit follows.
        page.apply_json(serde_json::json!({
            "event": "cancelled", "op": "download", "operation_id": "download-id"
        }));
        page.apply_json(serde_json::json!({
            "event": "op-done", "op": "download", "operation_id": "download-id",
            "ok": false, "cancelled": true
        }));
        page.finish_active(Some(1), "", &egui::Context::default());

        let note = page.cancel_note.as_deref().unwrap().to_string();
        assert!(page.cancel_failure);
        assert!(note.contains("invalid JSONL"), "{note}");
        assert!(!note.contains("waiting"), "{note}");
    }

    #[test]
    fn cancelled_terminal_accepts_expected_exit_one_but_rejects_other_exits() {
        let cancelled_page = || {
            let mut page = Page::from_runtime(Err("unused".into()));
            page.stop_after_active = true;
            page.cancel_requested = true;
            page.active = Some(Running {
                action: Action::Download("key".into()),
                operation_id: Some("download-id".into()),
                job: Job::disconnected(),
                terminal: Some(serde_json::json!({
                    "event": "op-done", "op": "download", "operation_id": "download-id",
                    "ok": false, "cancelled": true
                })),
                malformed: None,
                current_artifact: None,
                bytes_total: None,
                progress: None,
            });
            page
        };

        let mut page = cancelled_page();
        page.finish_active(Some(1), "", &egui::Context::default());
        let (outcome, ok) = page.last_outcome.clone().unwrap();
        assert!(!ok);
        assert!(outcome.contains("download cancelled"), "{outcome}");
        assert!(outcome.contains("helper exit Some(1)"), "{outcome}");
        assert!(!outcome.contains("disagree"), "{outcome}");
        assert_eq!(
            page.cancel_note.as_deref(),
            Some("The helper reported cancellation and exited.")
        );

        let mut page = cancelled_page();
        page.finish_active(Some(2), "", &egui::Context::default());
        let (outcome, _ok) = page.last_outcome.clone().unwrap();
        assert!(outcome.contains("disagree"), "{outcome}");
        assert!(outcome.contains("helper exit Some(2)"), "{outcome}");

        let mut page = cancelled_page();
        page.finish_active(Some(0), "", &egui::Context::default());
        let (outcome, _ok) = page.last_outcome.clone().unwrap();
        assert!(outcome.contains("download cancelled"), "{outcome}");
        assert!(!outcome.contains("disagree"), "{outcome}");
    }

    #[test]
    fn cancel_malformed_output_stays_failed_after_success_terminal() {
        let mut page = Page::from_runtime(Err("unused".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        page.cancel_job = Some(Job::from_receiver(rx));
        page.cancel_requested = true;
        page.cancel_target_id = Some("download-id".into());
        tx.send(ProcessEvent::Malformed("invalid JSONL".into()))
            .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-start","op":"cancel","operation_id":"download-id","request_id":"cancel-id"
        })))
        .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"cancel-requested","op":"cancel","operation_id":"download-id","request_id":"cancel-id"
        })))
        .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-done","op":"cancel","operation_id":"download-id","request_id":"cancel-id","ok":true,"requested":true
        })))
        .unwrap();
        tx.send(ProcessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })
        .unwrap();

        page.tick(&egui::Context::default());

        assert!(page.cancel_failure);
        assert!(page
            .cancel_note
            .as_deref()
            .unwrap()
            .contains("invalid JSONL"));
        assert!(!page
            .cancel_note
            .as_deref()
            .unwrap()
            .contains("Cancel signal sent"));
    }

    #[test]
    fn cancel_success_requires_matching_operation_and_request_ids() {
        let mut page = Page::from_runtime(Err("unused".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        page.cancel_job = Some(Job::from_receiver(rx));
        page.cancel_requested = true;
        page.cancel_target_id = Some("download-id".into());
        page.active = Some(Running {
            action: Action::DownloadProfile(Profile::Required),
            operation_id: Some("download-id".into()),
            job: Job::disconnected(),
            terminal: None,
            malformed: None,
            current_artifact: None,
            bytes_total: None,
            progress: None,
        });
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-start","op":"cancel","operation_id":"download-id","request_id":"cancel-id"
        })))
        .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"cancel-requested","op":"cancel","operation_id":"download-id","request_id":"cancel-id"
        })))
        .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-done","op":"cancel","operation_id":"download-id","request_id":"cancel-id","ok":true,"requested":true
        })))
        .unwrap();
        tx.send(ProcessEvent::Exited {
            code: Some(0),
            stderr: String::new(),
        })
        .unwrap();

        page.tick(&egui::Context::default());

        assert!(!page.cancel_failure);
        assert!(page
            .cancel_note
            .as_deref()
            .unwrap()
            .contains("waiting for the active helper"));
    }

    #[test]
    fn inactive_operation_id_is_a_cancel_failure() {
        let mut page = Page::from_runtime(Err("unused".into()));
        let (tx, rx) = std::sync::mpsc::channel();
        page.cancel_job = Some(Job::from_receiver(rx));
        page.cancel_requested = true;
        page.cancel_target_id = Some("finished-id".into());
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-start","op":"cancel","operation_id":"finished-id","request_id":"cancel-id"
        })))
        .unwrap();
        tx.send(ProcessEvent::Json(serde_json::json!({
            "event":"op-done","op":"cancel","operation_id":"finished-id","request_id":"cancel-id","ok":false,"error":"not-active","message":"target already finished"
        })))
        .unwrap();
        tx.send(ProcessEvent::Exited {
            code: Some(1),
            stderr: String::new(),
        })
        .unwrap();

        page.tick(&egui::Context::default());

        assert!(page.cancel_failure);
        assert!(page.cancel_note.as_deref().unwrap().contains("not-active"));
        assert!(!page.cancel_note.as_deref().unwrap().contains("accepted"));
    }

    #[test]
    fn presence_does_not_make_weight_assets_ready() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.apply_json(serde_json::json!({"event":"catalog","artifacts":[
            {"artifact":"engine","kind":"local_code","required_profile":true,"depends":[]},
            {"artifact":"key","kind":"hf_files","required_profile":true,"depends":["engine"]}
        ]}));
        page.artifacts.get_mut("engine").unwrap().apply_status(&serde_json::json!({"artifact":"engine","state":"installed","ready":true,"verified":true}));
        page.artifacts.get_mut("engine").unwrap().present = Some(true);
        page.artifacts.get_mut("key").unwrap().apply_status(&serde_json::json!({"artifact":"key","state":"installed","present":true,"ready":false,"verified":false}));
        assert_eq!(page.required_is_ready(), Some(false));
        page.artifacts.get_mut("key").unwrap().verified = Some(true);
        page.artifacts.get_mut("key").unwrap().ready = Some(true);
        assert_eq!(page.required_is_ready(), Some(true));
    }

    #[test]
    fn asset_mutation_is_blocked_until_worker_and_fence_safety_is_confirmed() {
        let mut page = Page::from_runtime(Err("unused".into()));
        page.set_asset_safety(true, true, false);
        assert!(page.assets_in_use);
        assert!(page.stop_workers_available);
        assert!(!page.unresolved_worker_fences);
        page.set_asset_safety(true, false, true);
        assert!(page.unresolved_worker_fences);
        page.set_asset_safety(false, false, false);
        assert!(!page.assets_in_use);
    }
}
