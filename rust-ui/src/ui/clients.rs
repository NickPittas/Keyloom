//! Clients page: the actual worker machines this session can use, their live connection state, hardware and
//! model identity as the workers reported them, what each is running now, and the scheduling policy behind
//! `Automatic`. Nothing here is invented: a machine that has not answered shows "not started".
use super::viewer::{DIM, WARN};
use crate::scheduler::{Placement, TargetStatus, TargetView, Trust};
use crate::ui::ai::AiUi;
use eframe::egui::{self, Color32, RichText, Ui};

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

/// Availability and setup only. Dispatch, progress and load live in the Batch queue.
pub fn ui(
    ui: &mut Ui,
    ai: &mut AiUi,
    default: &Placement,
    _local: &crate::metrics::View,
    _active: &[u32],
    _remote: &std::collections::BTreeMap<String, crate::metrics::ClientTelemetry>,
    paired: &[crate::remote::Install],
) -> Option<Placement> {
    let mut want = None;
    ui.horizontal(|ui| {
        ui.heading("Render clients");
        if ui.button("Check workers").clicked() { ai.probe_workers(); }
    });
    ui.add_space(8.0);
    let targets = ai.scheduler().targets();
    egui::Grid::new("client_availability").num_columns(5).striped(true).spacing(egui::vec2(24.0, 10.0)).show(ui, |ui| {
        for heading in ["Client", "Connection", "GPU", "Status", ""] { ui.label(RichText::new(heading).strong()); }
        ui.end_row();
        for t in &targets {
            let (state, colour) = state_text(t);
            ui.label(if t.local { "This computer" } else { &t.id });
            ui.label(if t.local { "Local" } else { "SSH" });
            let gpu = t.hello.as_ref().and_then(|h| h.gpu.as_ref()).map(|g| format!("{} · {:.0} GB", g.name, g.vram_gb)).unwrap_or_else(|| "Not reported".into());
            ui.label(gpu);
            ui.colored_label(colour, state.to_string());
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
        for pc in paired {
            ui.label(&pc.name);
            ui.label("Network");
            ui.label(pc.gpu.as_deref().unwrap_or("Not reported"));
            ui.colored_label(if pc.connected { super::theme::ACCENT } else { DIM }, if pc.connected { "Connected" } else { "Offline" });
            ui.label("");
            ui.end_row();
        }
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
    want
}
