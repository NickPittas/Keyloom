//! Reference items: colour dy=-1100, progress dy=-2900, cards dy=-3500; dx=0.
use crate::ui::{controls::{chips::*, line, paint_line}, tokens::tokens, type_scale as ty};
use eframe::egui::{self, Align, Rect, Ui, UiBuilder};
fn at(ui: &mut Ui, x: f32, y: f32, f: impl FnOnce(&mut Ui)) {
    let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(egui::pos2(x,y), egui::vec2(400.0,24.0))));
    f(&mut child);
}
pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    ui.painter().rect_filled(ui.max_rect(), 0, t.bg_panel);
    at(ui, 49.0000, 57.0000, |ui| { stage_chip(ui, Stage::Raw); });
    at(ui, 113.0000, 57.0000, |ui| { stage_chip(ui, Stage::Mask); });
    at(ui, 177.0000, 57.0000, |ui| { stage_chip(ui, Stage::Ready); });
    at(ui, 245.1875, 57.0000, |ui| { stage_chip(ui, Stage::Completed); });
    at(ui, 338.7656, 57.0000, |ui| { job_badge_motion(ui, Job::Queued, true); });
    at(ui, 416.6406, 57.0000, |ui| { job_badge_motion(ui, Job::Rendering(0.34), true); });
    at(ui, 530.0938, 57.0000, |ui| { job_badge_motion(ui, Job::Preparing, true); });
    at(ui, 616.0156, 57.0000, |ui| { job_badge_motion(ui, Job::Failed, true); });
    at(ui, 715.2031, 57.0000, |ui| { status_badge(ui, Status::Warning, "Out of date"); });
    at(ui, 791.8438, 57.0000, |ui| { status_badge(ui, Status::Error, "Model missing"); });
    at(ui, 885.2188, 57.0000, |ui| { neutral_badge(ui, "HDR clipped display"); });
    at(ui, 1009.6875, 57.0000, |ui| { neutral_badge(ui, "EXR · half · 3200×1800"); });
    at(ui, 1144.6562, 58.0000, |ui| { count_badge_running(ui, 3, false); });
    at(ui, 1168.9531, 58.0000, |ui| { count_badge_running(ui, 1, true); });
    at(ui, 949.5938, 248.0000, |ui| { progress_forced(ui, 206.2344, Progress { value: Some(0.34), state: State::Running, thin: false, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 949.5938, 280.0000, |ui| { progress_forced(ui, 240.0000, Progress { value: Some(1.0), state: State::Done, thin: false, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 949.5938, 306.0000, |ui| { progress_forced(ui, 240.0000, Progress { value: Some(0.62), state: State::Failed, thin: false, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 949.5938, 332.0000, |ui| { progress_forced(ui, 240.0000, Progress { value: Some(0.2), state: State::Paused, thin: false, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 949.5938, 358.0000, |ui| { progress_forced(ui, 240.0000, Progress { value: None, state: State::Running, thin: false, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 949.5938, 379.0000, |ui| { progress_forced(ui, 240.0000, Progress { value: Some(0.55), state: State::Running, thin: true, label: "Keying" }, false, Some(0.5714286)); });
    at(ui, 58.0000, 719.0000, |ui| { stage_chip(ui, Stage::Raw); });
    at(ui, 294.0000, 719.0000, |ui| { stage_chip(ui, Stage::Mask); });
    at(ui, 530.0000, 719.0000, |ui| { stage_chip(ui, Stage::Ready); });
    at(ui, 904.1250, 544.0000, |ui| { job_badge_motion(ui, Job::Queued, true); });
    at(ui, 766.0000, 719.0000, |ui| { stage_chip(ui, Stage::Ready); });
    at(ui, 1162.0312, 544.0000, |ui| { job_badge_motion(ui, Job::Rendering(0.34), true); });
    at(ui, 1002.0000, 719.0000, |ui| { stage_chip(ui, Stage::Mask); });
    at(ui, 1368.0781, 544.0000, |ui| { job_badge_motion(ui, Job::Preparing, true); });
    at(ui, 1238.0000, 719.0000, |ui| { stage_chip(ui, Stage::Ready); });
    at(ui, 1622.8125, 544.0000, |ui| { job_badge_motion(ui, Job::Failed, true); });
    at(ui, 1474.0000, 719.0000, |ui| { stage_chip(ui, Stage::Completed); });
    at(ui, 206.8125, 842.0000, |ui| { job_badge_motion(ui, Job::Failed, true); });
    at(ui, 58.0000, 1017.0000, |ui| { stage_chip(ui, Stage::Ready); });
    { let r = Rect::from_min_size(egui::pos2(831.5938, 243.0000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Standard 6", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(1163.7031, 244.0000), egui::vec2(19.8125, 14.0000)); let g=line(ui.painter(), "34%", ty::mono(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.text_primary, 0.0); }
    { let r=Rect::from_min_size(egui::pos2(1191.6406,237.0),egui::vec2(134.6562,14.0)); let g=line(ui.painter(),"frame 22 / 65 · endor-",ty::small(),14.0,f32::INFINITY); paint_line(ui.painter(),r,g,14.0,Align::Min,t.text_tertiary,0.0); }
    { let r=Rect::from_min_size(egui::pos2(1191.6406,251.0),egui::vec2(134.6562,14.0)); let g=line(ui.painter(),"omarchy",ty::small(),14.0,f32::INFINITY); paint_line(ui.painter(),r,g,14.0,Align::Min,t.text_tertiary,0.0); }
    { let r = Rect::from_min_size(egui::pos2(831.5938, 275.0000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Done", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(1197.5938, 276.0000), egui::vec2(26.4062, 14.0000)); let g=line(ui.painter(), "100%", ty::mono(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.text_primary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(831.5938, 301.0000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Failed", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(1197.5938, 302.0000), egui::vec2(90.6875, 14.0000)); let g=line(ui.painter(), "failed at frame 40", ty::small(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.error, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(831.5938, 327.0000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Paused / queued", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(1197.5938, 328.0000), egui::vec2(112.1406, 14.0000)); let g=line(ui.painter(), "waiting for a machine", ty::small(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.text_tertiary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(831.5938, 353.0000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Indeterminate", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(1197.5938, 354.0000), egui::vec2(128.1875, 14.0000)); let g=line(ui.painter(), "30 % segment, 1.2 s loop", ty::small(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.text_tertiary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(831.5938, 383.5000), egui::vec2(110.0000, 16.0000)); let g=line(ui.painter(), "Thin 3", ty::label(), 16.0, f32::INFINITY); paint_line(ui.painter(), r, g, 16.0, Align::Min, t.text_secondary, 0.0); }
    { let r = Rect::from_min_size(egui::pos2(949.5938, 390.0000), egui::vec2(135.7812, 14.0000)); let g=line(ui.painter(), "cards, nodes, Activity rows", ty::small(), 14.0, f32::INFINITY); paint_line(ui.painter(), r, g, 14.0, Align::Min, t.text_tertiary, 0.0); }
}
