//! Drawing for `metrics::Cells`: the global footer row and the Clients card block. Fixed cell widths, one
//! monospace size, no animation, so values changing never move anything.
use super::theme;
use crate::metrics::Cells;
use eframe::egui::{self, Color32, RichText, Ui};

pub const FOOTER_H: f32 = 24.0;
const CPU_W: f32 = 78.0;
const RAM_W: f32 = 196.0;
const STATUS_W: f32 = 170.0;
const GAP: f32 = 14.0;

fn txt(s: &str, dim: bool) -> RichText {
    RichText::new(s).monospace().size(11.5).color(if dim { theme::TEXT_DIM } else { Color32::from_rgb(214, 226, 217) })
}

/// A fixed-width cell holding one truncated label (hover shows the full text and `tip`).
fn cell(ui: &mut Ui, w: f32, text: &str, dim: bool, tip: &str) {
    ui.allocate_ui_with_layout(egui::vec2(w, FOOTER_H - 4.0), egui::Layout::left_to_right(egui::Align::Center), |ui| {
        ui.set_width(w);
        let r = ui.add(egui::Label::new(txt(text, dim)).truncate());
        r.on_hover_text(if tip.is_empty() { text.to_string() } else { format!("{text}\n{tip}") });
    });
}

/// The longest GPU form whose width fits `w` (never clipped, so VRAM stays visible); the last form always shows
/// utilisation + VRAM in a handful of characters.
pub fn pick_gpu_tier<'a>(ui: &Ui, c: &'a Cells, w: f32) -> &'a str {
    let width = |t: &str| ui.painter().layout_no_wrap(t.to_string(), egui::FontId::monospace(11.5), Color32::WHITE).size().x;
    c.gpu_tiers.iter().find(|t| width(t) <= w).or(c.gpu_tiers.last()).map_or(c.gpu.as_str(), String::as_str)
}

/// The always-visible row: CPU %, RAM, every GPU's utilisation + VRAM (or a compact aggregate when narrow), then
/// why anything is missing/old.
pub fn footer(ui: &mut Ui, c: &Cells) {
    let gpu_w = (ui.available_width() - CPU_W - RAM_W - STATUS_W - GAP * 3.0 - 8.0).max(120.0);
    let gpu = pick_gpu_tier(ui, c, gpu_w - 4.0).to_string();
    ui.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = GAP;
        cell(ui, CPU_W, &format!("CPU {}", c.cpu), c.dim, "System-wide CPU use, all processes (not this app alone).");
        cell(ui, RAM_W, &format!("RAM {}", c.ram), c.dim, "System memory in use (total minus available).");
        let tip = c.detail.join("\n");
        cell(ui, gpu_w, &gpu, c.dim, &tip);
        cell(ui, STATUS_W, c.status.as_deref().unwrap_or(""), true, "");
    });
}


fn sample_of(t: &crate::metrics::ClientTelemetry) -> Option<&crate::metrics::Sample> {
    use crate::metrics::{ClientTelemetry as T, View};
    match t {
        T::Local(View::Live(s)) | T::Local(View::Stale { sample: s, .. }) | T::Reported { sample: s, .. } => Some(s),
        _ => None,
    }
}

fn ratio(used: Option<u64>, total: Option<u64>) -> Option<f32> {
    used.zip(total).filter(|(_, t)| *t > 0).map(|(u, t)| (u as f64 / t as f64).clamp(0.0, 1.0) as f32)
}

/// One bar: fill = `frac` (None draws an empty bar and "n/a", never zero), label left, value right.
fn meter(ui: &mut Ui, w: f32, label: &str, frac: Option<f32>, value: &str, dim: bool, tip: &str) {
    let (rect, r) = ui.allocate_exact_size(egui::vec2(w, 16.0), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(rect, 2.0, theme::FIELD);
    if let Some(f) = frac {
        let mut fill = rect;
        fill.set_right(rect.left() + rect.width() * f);
        p.rect_filled(fill, 2.0, if dim { theme::ACCENT.gamma_multiply(0.3) } else { theme::ACCENT.gamma_multiply(0.7) });
    }
    let col = if dim || frac.is_none() { theme::TEXT_DIM } else { theme::TEXT };
    let font = egui::FontId::monospace(10.5);
    p.text(rect.left_center() + egui::vec2(4.0, 0.0), egui::Align2::LEFT_CENTER, label, font.clone(), col);
    p.text(rect.right_center() - egui::vec2(4.0, 0.0), egui::Align2::RIGHT_CENTER, value, font, col);
    r.on_hover_text(tip);
}

/// One machine as compact bars in two columns: CPU, RAM, then GPU util / VRAM per GPU (active ones first, marked ▶).
/// Freshness comes from `client_cells`; stale/absent data is dimmed with its status line.
pub fn machine_meters(ui: &mut Ui, telemetry: &crate::metrics::ClientTelemetry, active: &[u32]) {
    let c = crate::metrics::client_cells(telemetry, active);
    let s = sample_of(telemetry);
    let w = ((ui.available_width() - ui.spacing().item_spacing.x) / 2.0).max(60.0);
    let na = "n/a";
    let mut m: Vec<(String, Option<f32>, String, String)> = vec![
        ("CPU".into(), s.and_then(|s| s.cpu_percent).map(|v| v / 100.0), c.cpu.clone(), "System-wide CPU".into()),
        ("RAM".into(), s.and_then(|s| ratio(s.ram_used, s.ram_total)), c.ram.replace(" GiB", "G"), "System memory in use".into()),
    ];
    match s.and_then(|s| s.gpus.as_ref()) {
        Some(g) if !g.is_empty() => {
            let is_act = |x: &crate::metrics::GpuStat| x.index.is_some_and(|i| active.contains(&i));
            for x in g.iter().filter(|x| is_act(x)).chain(g.iter().filter(|x| !is_act(x))) {
                let n = format!("{}{}", if is_act(x) { "▶" } else { "" }, x.index.map_or("?".into(), |i| i.to_string()));
                let tip = format!("{}\n{}", crate::metrics::gpu_line(x), x.uuid);
                m.push((format!("GPU{n}"), x.util.map(|v| v / 100.0), crate::metrics::fmt_pct(x.util), tip.clone()));
                m.push((format!("VRAM{n}"), ratio(x.vram_used, x.vram_total), crate::metrics::fmt_mem(x.vram_used, x.vram_total).replace(" GiB", "G"), tip));
            }
        }
        Some(_) => m.push(("GPU".into(), None, "none".into(), "No NVIDIA GPU was found".into())),
        None => {
            m.push(("GPU".into(), None, na.into(), c.detail.first().cloned().unwrap_or_default()));
            m.push(("VRAM".into(), None, na.into(), "GPU memory not reported".into()));
        }
    }
    egui::Grid::new(ui.next_auto_id()).num_columns(2).spacing(egui::vec2(4.0, 3.0)).show(ui, |ui| {
        for (i, (l, f, v, tip)) in m.iter().enumerate() {
            meter(ui, w, l, *f, if v == "—" { na } else { v }, c.dim, tip);
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
    if let Some(st) = &c.status {
        ui.label(RichText::new(st).color(theme::TEXT_DIM).size(10.5));
    }
}
