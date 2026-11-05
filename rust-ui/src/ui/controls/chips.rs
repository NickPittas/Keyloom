//! Progress and noninteractive badges, bible §2.6, §5.12–13, §11–12.
use super::{line, paint_line, spinner};
use crate::ui::{icons::{self, Icon, IconSize}, tokens::{tokens, Tokens}, type_scale as ty};
use eframe::egui::{self, accesskit::Role, Align, Color32, Rect, Response, Sense, Stroke, StrokeKind, Ui, WidgetInfo};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State { Running, Paused, Failed, Done }
#[derive(Clone, Copy, Debug)]
pub struct Progress<'a> { pub value: Option<f32>, pub state: State, pub thin: bool, pub label: &'a str }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage { Raw, Mask, Ready, Completed }
#[derive(Clone, Copy, Debug, PartialEq)]
/// Rendering uses a fraction from 0 to 1, like Progress::value.
pub enum Job { Queued, Rendering(f32), Preparing, Failed }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status { Warning, Error }
pub enum ProgressSlot { Width(f32), Rect(Rect) }
impl From<f32> for ProgressSlot { fn from(w: f32) -> Self { Self::Width(w) } }
impl From<Rect> for ProgressSlot { fn from(r: Rect) -> Self { Self::Rect(r) } }
fn value(v: f32) -> f32 { if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 } }
/// Values are fractions; accessibility values are percentages.
pub fn progress(ui: &mut Ui, slot: impl Into<ProgressSlot>, p: Progress<'_>) -> Response {
    progress_forced(ui, slot, p, false, None)
}
/// Reduce motion freezes unknown progress centrally; phase overrides are for specimens.
pub fn progress_forced(ui: &mut Ui, slot: impl Into<ProgressSlot>, p: Progress<'_>, reduced_motion: bool, phase: Option<f32>) -> Response {
    let h = if p.thin { 3.0 } else { 6.0 };
    let response = match slot.into() {
        ProgressSlot::Width(w) => ui.allocate_exact_size(egui::vec2(w.max(0.0), h), Sense::hover()).1,
        ProgressSlot::Rect(r) => ui.interact(Rect::from_min_size(r.min, egui::vec2(r.width(), h)), ui.scope_id().with(("progress", r.min.x.to_bits(), r.min.y.to_bits())), Sense::hover()),
    };
    let v = p.value.map(value);
    let label = if v.is_none() && reduced_motion { format!("{} · Working…", p.label) } else if let Some(v) = v { format!("{} {:.0}%", p.label, v * 100.0) } else { p.label.to_owned() };
    response.widget_info(|| WidgetInfo::labeled(Role::ProgressIndicator, ui.is_enabled(), label.as_str()));
    ui.ctx().accesskit_node_builder(response.id, |n| {
        n.set_min_numeric_value(0.0); n.set_max_numeric_value(100.0);
        if let Some(v) = v { n.set_numeric_value(f64::from(v) * 100.0); }
    });
    let rect = response.rect;
    if !ui.is_rect_visible(rect) || ui.input(|i| i.viewport().minimized == Some(true)) { return response }
    let t = tokens(ui);
    let fill = match p.state { State::Running => t.busy_fill, State::Paused => t.busy_queued, State::Failed => t.error, State::Done => t.success };
    let radius = if p.thin { 0 } else { 2 };
    ui.painter().rect_filled(rect, radius, t.bg_sunken);
    let track = if p.thin { rect } else { rect.shrink(1.0) };
    let (left, width) = if let Some(v) = v { (0.0, v) } else {
        let pos = phase.map(value).unwrap_or_else(|| if reduced_motion || p.state != State::Running { 0.5 } else { (ui.input(|i| i.time) / 1.2).rem_euclid(1.0) as f32 });
        (pos * 0.7, 0.3)
    };
    if p.value.is_none() && p.state == State::Running && !reduced_motion && phase.is_none() {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(33));
    }
    ui.painter().rect_filled(Rect::from_min_size(track.min + egui::vec2(track.width() * left, 0.0), egui::vec2(track.width() * width, track.height())), 0, fill);
    if !p.thin { ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, t.border_default), StrokeKind::Inside); }
    response
}
#[derive(Clone, Copy)]
pub struct BadgeLook { pub fill: Color32, pub text: Color32, pub icon: Option<Icon>, pub spinning: bool }
pub fn job_look(t: &Tokens, job: Job) -> BadgeLook {
    let (fill, text, icon, spinning) = match job {
        Job::Queued => (t.busy_queued, t.badge_queued_text, Some(Icon::Clock), false),
        Job::Rendering(_) | Job::Preparing => (t.busy_fill, t.badge_rendering_text, None, true),
        Job::Failed => (t.stage_failed_chip_fill, t.stage_failed_chip_text, Some(Icon::Error), false),
    };
    BadgeLook { fill, text, icon, spinning }
}
fn badge(ui: &mut Ui, text: &str, look: BadgeLook, min: f32, pad: f32, height: f32, radius: u8, reduced_motion: bool) -> Response {
    let g = line(ui.painter(), text, ty::chip(), 14.0, f32::INFINITY);
    let lead = if look.icon.is_some() || look.spinning { 16.0 } else { 0.0 };
    let w = (g.size().x + pad * 2.0 + lead).max(min).round();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(w, height), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, ui.is_enabled(), text));
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(rect, radius, look.fill);
        let lead_rect = Rect::from_min_size(egui::pos2(rect.left() + pad, rect.center().y - 6.0), egui::vec2(12.0, 12.0));
        if let Some(icon) = look.icon { icons::paint(ui.painter(), lead_rect, icon, IconSize::S12, look.text); }
        if look.spinning { spinner(ui, lead_rect, look.text, reduced_motion || ui.input(|i| i.viewport().minimized == Some(true))); }
        let text_rect = Rect::from_min_max(egui::pos2(rect.left() + pad + lead, rect.top()), rect.max - egui::vec2(pad, 0.0));
        paint_line(ui.painter(), text_rect, g, 14.0, if lead > 0.0 { Align::Min } else { Align::Center }, look.text, 0.0);
    }
    response
}
pub fn stage_chip(ui: &mut Ui, stage: Stage) -> Response {
    let t = tokens(ui);
    let (label, fill, text, icon) = match stage {
        Stage::Raw => ("Raw", t.stage_raw_chip_fill, t.stage_raw_chip_text, Icon::CircleEmpty),
        Stage::Mask => ("Mask", t.stage_mask_chip_fill, t.stage_mask_chip_text, Icon::Brush),
        Stage::Ready => ("Ready", t.stage_ready_chip_fill, t.stage_ready_chip_text, Icon::Key),
        Stage::Completed => ("Completed", t.stage_completed_chip_fill, t.stage_completed_chip_text, Icon::CheckCircle),
    };
    badge(ui, label, BadgeLook { fill, text, icon: Some(icon), spinning: false }, 56.0, 6.0, 18.0, 3, true)
}
pub fn job_badge(ui: &mut Ui, job: Job) -> Response { job_badge_motion(ui, job, false) }
pub fn job_badge_motion(ui: &mut Ui, job: Job, reduced_motion: bool) -> Response {
    let label = match job { Job::Queued => "Queued".to_owned(), Job::Rendering(v) => format!("{:.0}%", value(v) * 100.0), Job::Preparing => "Preparing".to_owned(), Job::Failed => "Failed".to_owned() };
    badge(ui, &label, job_look(tokens(ui), job), 0.0, 6.0, 18.0, 3, reduced_motion)
}
pub fn status_badge(ui: &mut Ui, status: Status, text: &str) -> Response {
    let color = match status { Status::Warning => tokens(ui).warning, Status::Error => tokens(ui).error };
    let g = line(ui.painter(), text, ty::small(), 14.0, f32::INFINITY);
    let (r, response) = ui.allocate_exact_size(egui::vec2(g.size().x + 10.0, 18.0), Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Status, ui.is_enabled(), text));
    if ui.is_rect_visible(r) {
        ui.painter().circle_filled(egui::pos2(r.left() + 3.0, r.center().y), 3.0, color);
        paint_line(ui.painter(), Rect::from_min_max(r.min + egui::vec2(10.0, 0.0), r.max), g, 14.0, Align::Min, color, 0.0);
    }
    response
}
pub fn count_badge(ui: &mut Ui, n: usize) -> Response { count_badge_running(ui, n, false) }
pub fn count_badge_running(ui: &mut Ui, n: usize, running: bool) -> Response {
    let t = tokens(ui);
    badge(ui, &n.to_string(), BadgeLook { fill: if running { t.busy_fill } else { t.bg_pressed }, text: if running { t.badge_rendering_text } else { t.text_primary }, icon: None, spinning: false }, 16.0, 5.0, 16.0, 8, true)
}
pub fn neutral_badge(ui: &mut Ui, text: &str) -> Response {
    let t = tokens(ui);
    badge(ui, text, BadgeLook { fill: t.bg_raised, text: t.text_secondary, icon: None, spinning: false }, 0.0, 6.0, 18.0, 3, true)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn progress_never_requests_repaint_when_hidden() {
        let ctx = egui::Context::default();
        for frame in 0..4 {
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() }, |ui| {
                progress(ui, Rect::from_min_size(egui::pos2(10000.0, 10000.0), egui::vec2(200.0, 6.0)), Progress { value: None, state: State::Running, thin: false, label: "Keying" });
            });
            out.textures_delta.clear();
            if frame > 1 { assert!(out.viewport_output.values().all(|v| v.repaint_delay > std::time::Duration::from_millis(33))); }
        }
    }
    #[test]
    fn job_badges_have_no_outline_and_inverted_fill() {
        for dark in [true, false] {
            let ctx = egui::Context::default();
            crate::ui::fonts::install(&ctx);
            ctx.set_visuals(if dark { egui::Visuals::dark() } else { egui::Visuals::light() });
            let t = if dark { &crate::ui::tokens::DARK } else { &crate::ui::tokens::LIGHT };
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                for j in [Job::Queued, Job::Rendering(0.34), Job::Preparing, Job::Failed] { job_badge_motion(ui, j, true); }
            });
            out.textures_delta.clear();
            let rects: Vec<_> = out.shapes.iter().filter_map(|s| if let egui::Shape::Rect(r) = &s.shape { (r.rect.height() == 18.0).then_some(r) } else { None }).collect();
            assert_eq!(rects.len(), 4);
            assert!(rects.iter().all(|r| r.stroke == Stroke::NONE));
            assert_eq!(rects[0].fill, t.busy_queued); assert_eq!(rects[1].fill, t.busy_fill); assert_eq!(rects[2].fill, t.busy_fill);
            assert_eq!(job_look(t, Job::Preparing).text, t.badge_rendering_text);
        }
    }
    #[test]
    fn progress_accesskit_numeric_value() {
        let ctx = egui::Context::default(); ctx.enable_accesskit();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            progress(ui, 200.0, Progress { value: Some(0.34), state: State::Running, thin: false, label: "Keying" });
        });
        out.textures_delta.clear();
        let update = out.platform_output.accesskit_update.unwrap();
        let n = &update.nodes.iter().find(|(_, n)| n.role() == Role::ProgressIndicator).unwrap().1;
        assert!((n.numeric_value().unwrap() - 34.0).abs() < 0.001);
        assert_eq!(n.min_numeric_value(), Some(0.0)); assert_eq!(n.max_numeric_value(), Some(100.0));
    }
}
