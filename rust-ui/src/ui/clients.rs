//! Clients page: the actual worker machines this session can use, their live connection state, hardware and
//! model identity as the workers reported them, what each is running now, and the scheduling policy behind
//! `Automatic`. Nothing here is invented: a machine that has not answered shows "not started".
use super::viewer::{DIM, WARN};
use crate::remote::Action;
use crate::scheduler::{Placement, TargetStatus, TargetView, Trust};
use crate::settings::Settings;
use crate::ui::ai::AiUi;
use eframe::egui::{self, Color32, RichText, Ui};
use std::path::Path;

const BAD: Color32 = Color32::from_rgb(225, 110, 110);

fn state_text(t: &TargetView) -> (&'static str, Color32) {
    if !t.enabled {
        return ("disabled", DIM);
    }
    match &t.status {
        TargetStatus::Fresh => ("not started", DIM),
        TargetStatus::Starting => match t.starting_for {
            Some(e) => (
                if e.as_secs() < 1 {
                    "starting"
                } else {
                    "starting (slow)"
                },
                WARN,
            ),
            None => ("starting", WARN),
        },
        TargetStatus::Ready if t.busy_with.is_some() => ("busy", Color32::from_rgb(150, 190, 150)),
        TargetStatus::Ready => ("ready", Color32::from_rgb(150, 190, 150)),
        TargetStatus::Refused(_) => ("refused", BAD),
        TargetStatus::Down(_) => ("down", BAD),
        TargetStatus::Stopping => ("stopping", WARN),
        TargetStatus::Stopped => ("stopped", WARN),
    }
}

fn trust_text(t: &TargetView) -> &'static str {
    match t.trust {
        Trust::Unknown => "verifying",
        Trust::Reference => "reference",
        Trust::Matches => "matches local",
        Trust::Mismatch => "DIFFERS from local",
        Trust::Unverified => "unverifiable",
    }
}

/// Availability and setup only. Dispatch, progress and load live in the Batch queue. One table holds every client:
/// this computer's local render client, then each network PC's row with its own connect and assignment controls.
pub fn ui(
    ui: &mut Ui,
    ai: &mut AiUi,
    default: &Placement,
    _local: &crate::metrics::View,
    _active: &[u32],
    _remote: &std::collections::BTreeMap<String, crate::metrics::ClientTelemetry>,
    net: &crate::remote_set::RemoteSet,
    s: &Settings,
    sp: Option<&Path>,
    own_net: Option<&str>,
    own_iid: Option<&str>,
) -> (Option<Placement>, Vec<Action>) {
    let mut want = None;
    let mut acts = vec![];
    ui.horizontal(|ui| {
        ui.heading("Clients");
        if ui.button("Check workers").clicked() { ai.probe_workers(); }
    });
    ui.add_space(8.0);
    let targets = ai.scheduler().targets();
    egui::ScrollArea::horizontal().show(ui, |ui| {
        egui::Grid::new("client_availability").num_columns(11).striped(true).spacing(egui::vec2(24.0, 10.0)).show(ui, |ui| {
            // Header widths set each column's width (cells wrap inside it); drag a header's right edge to resize.
            let wid = ui.scope_id().with("client_col_widths");
            let mut widths: Vec<f32> = ui.data(|d| d.get_temp(wid)).unwrap_or_else(|| vec![140.0, 70.0, 190.0, 280.0, 220.0, 130.0, 60.0, 60.0, 60.0, 70.0, 90.0]);
            for (heading, w) in ["Client", "Kind", "Address", "Status", "GPU", "ID", "Connect", "Accept work when GPU is idle", "Default for Batch job", "", ""].iter().zip(widths.iter_mut()) {
                col_header(ui, heading, w);
            }
            ui.data_mut(|d| d.insert_temp(wid, widths));
            ui.end_row();
            for (i, t) in targets.iter().filter(|t| t.local).enumerate() {
                let (state, colour) = state_text(t);
                ui.label("This computer");
                ui.label("Local");
                ui.label("—");
                ui.vertical(|ui| {
                    ui.colored_label(colour, state.to_string());
                    if i == 0 {
                        acts.extend(net.local_status(ui, s, sp));
                    }
                });
                let gpu = t.hello.as_ref().and_then(|h| h.gpu.as_ref()).map(|g| format!("{} · {:.0} GB", g.name, g.vram_gb)).unwrap_or_else(|| "Not reported".into());
                ui.label(gpu);
                if i == 0 {
                    acts.extend(net.local_controls(ui, s, sp));
                } else {
                    for _ in 0..5 { ui.label(""); }
                }
                egui::CollapsingHeader::new("Details").id_salt(("availability", &t.id)).show(ui, |ui| {
                    ui.label(format!("Model identity: {}", trust_text(t)));
                    if let Some(h) = &t.hello {
                        let mut ready: Vec<_> = h.capabilities.tasks.iter().filter(|(_, ready)| **ready).map(|(kind, _)| kind.as_str()).collect();
                        ready.sort_unstable();
                        ui.add(egui::Label::new(ready.join(", ")).wrap());
                    }
                    if let TargetStatus::Refused(reason) | TargetStatus::Down(reason) = &t.status { ui.colored_label(BAD, reason); }
                    if t.busy_with.is_none() && ui.small_button("Recheck").clicked() { ai.scheduler_mut().reset_target(&t.id); }
                });
                ui.end_row();
            }
            acts.extend(net.rows(ui, s, sp, own_net, own_iid));
        });
    });
    ui.add_space(12.0);
    egui::CollapsingHeader::new("Default assignment").show(ui, |ui| {
        let mut cur = default.clone();
        egui::ComboBox::from_id_salt("clients_default").selected_text(match &cur { Placement::Auto => "Automatic".to_string(), Placement::Target(id) => id.clone() }).show_ui(ui, |ui| {
            ui.selectable_value(&mut cur, Placement::Auto, "Automatic");
            for t in &targets { ui.selectable_value(&mut cur, Placement::Target(t.id.clone()), &t.id); }
        });
        if &cur != default { want = Some(cur); }
    });
    (want, acts)
}

/// A grid header cell at least `w` wide, with a drag handle on its right edge that changes `w`.
fn col_header(ui: &mut egui::Ui, text: &str, w: &mut f32) {
    ui.horizontal(|ui| {
        let start = ui.cursor().min.x;
        ui.label(RichText::new(text).strong());
        let used = ui.cursor().min.x - start;
        ui.add_space((*w - used - 6.0).max(0.0));
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(6.0, 18.0), egui::Sense::drag());
        let resp = resp.on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
        if resp.dragged() {
            *w = (*w + resp.drag_delta().x).max(used + 6.0);
        }
        ui.painter().vline(rect.center().x, rect.y_range(), ui.visuals().widgets.noninteractive.bg_stroke);
    });
}
