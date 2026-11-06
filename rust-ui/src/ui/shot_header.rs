use super::{part, Fixture};
use crate::ui::{controls::{buttons::{icon_button, IconButtonSize}, chips::{self, Job, Stage}, tabs::{Tab, TabKind, TabStrip}}, icons::{self, Icon, IconSize}, tokens::tokens, type_scale as ty};
use eframe::egui::{pos2, vec2, CornerRadius, Rect, Sense, Ui};

pub(super) struct HeaderOut {
    pub stage: Option<&'static str>,
    pub toggle_drawer: bool,
}

pub(super) fn draw(ui: &mut Ui, rect: Rect, fx: &Fixture, drawer_open: bool) -> HeaderOut {
    let t = tokens(ui);
    let p = ui.painter().with_clip_rect(rect);
    p.rect_filled(rect, 0.0, t.bg_panel);
    p.rect_filled(Rect::from_min_size(pos2(rect.left(), rect.bottom() - 1.0), vec2(rect.width(), 1.0)), 0.0, t.border_divider);

    let compact = ui.ctx().content_rect().width() < 1200.0;
    let stage_names = ["Media", "Hint", "Key", "Review", "Deliver"];
    let active = stage_names.iter().position(|s| *s == fx.stage).unwrap_or(2);
    let tabs = stage_names.map(Tab::new);
    let mut active = active;
    let start = active;
    let props_rect = Rect::from_min_size(pos2(rect.right() - 30.0, rect.top() + 3.0), vec2(22.0, 26.0));
    // Mockup §7.2: each tab is 12 + 16 circle + 6 + label + 12; tabs sit 2 px apart and end 8 px before Properties.
    let widths: f32 = tabs.iter().enumerate().map(|(i, tab)| {
        let text = ui.painter().layout_no_wrap(tab.label.to_owned(), ty::button(), t.text_primary);
        if compact && i != active { 28.0 } else { (if compact { 34.0 } else { 46.0 }) + text.size().x + 1.0 / 64.0 }
    }).sum::<f32>() + 8.0;
    let tabs_x = props_rect.left() - 8.0 - widths;
    let mut tab_ui = part(ui, Rect::from_min_size(pos2(tabs_x, rect.top() + 2.0), vec2(widths, 28.0)), "stage-tabs");
    TabStrip::new(TabKind::Stage, &mut active, &tabs)
        .compact(compact).name("Shot stages").at(pos2(tabs_x, rect.top() + 2.0)).show(&mut tab_ui);
    let stage = (active != start).then(|| stage_names[active]);

    let max_name = if compact { 160.0 } else if ui.ctx().content_rect().width() < 1600.0 { 240.0 } else { 360.0 };
    let name = shown_name(ui, &fx.shot, max_name);
    let truncated = name != fx.shot;
    let galley = ui.painter().layout_no_wrap(name, ty::shot_name(), t.text_primary);
    let name_w = galley.size().x;
    let name_rect = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top()), vec2(name_w, rect.height()));
    ui.painter().with_clip_rect(name_rect).galley(
        pos2(name_rect.left(), name_rect.center().y - galley.size().y / 2.0), galley, t.text_primary,
    );
    // Chip takes its natural width: text + 6 px pads + 16 px icon lead, minimum 56 (chips::badge).
    let (chip_stage, label) = match fx.clip_stage.as_str() {
        "raw" => (Stage::Raw, "Raw"),
        "mask" => (Stage::Mask, "Mask"),
        "ready" => (Stage::Ready, "Ready"),
        _ => (Stage::Completed, "Completed"),
    };
    // Compact (mockup 58 @900): icon-only 22 px chip with a coloured left bar; the label is its tooltip.
    let chip_w = if compact { 22.0 } else { (ui.painter().layout_no_wrap(label.to_owned(), ty::chip(), t.text_primary).size().x + 28.0).max(56.0).round() };
    // Compact: a truncated name fills its full 160 px cap (mockup flex box), not the last glyph-trimmed width.
    let name_slot_w = if compact && truncated { max_name } else { name_w };
    let chip = Rect::from_min_size(pos2(rect.left() + 8.0 + name_slot_w + 8.0, rect.top() + 7.0), vec2(chip_w, 18.0));
    if compact {
        compact_stage_chip(ui, chip, chip_stage, label);
    } else {
        let mut chip_ui = part(ui, chip, "stage-chip");
        chips::stage_chip(&mut chip_ui, chip_stage);
    }
    if let Some(percent) = fx.busy_percent {
        let mut job_ui = part(ui, Rect::from_min_size(pos2(chip.right() + 8.0, rect.top() + 5.0), vec2(72.0, 22.0)), "job-badge");
        chips::job_badge_motion(&mut job_ui, Job::Rendering(percent as f32 / 100.0), true);
    } else if let Some(label) = &fx.job_label {
        let job_rect = Rect::from_min_size(pos2(chip.right() + 8.0, rect.top() + 5.0), vec2(72.0, 22.0));
        let g = ui.painter().layout_no_wrap(label.clone(), ty::chip(), t.text_tertiary);
        ui.painter().with_clip_rect(job_rect).galley(pos2(job_rect.left(), job_rect.center().y - g.size().y / 2.0), g, t.text_tertiary);
    }

    let button = ui.add(icon_button(Icon::PanelRight, IconButtonSize::Normal22, if drawer_open { "Close Properties" } else { "Properties" }).at(props_rect.min));
    HeaderOut { stage, toggle_drawer: button.clicked() }
}

/// Icon-only stage chip for the Compact header: fill, 3 px left bar and 12 px state icon, no text.
fn compact_stage_chip(ui: &mut Ui, rect: Rect, stage: Stage, label: &str) {
    let t = tokens(ui);
    let (fill, text, bar, icon) = match stage {
        Stage::Ready => (t.stage_ready_chip_fill, t.stage_ready_chip_text, t.stage_ready_bar, Icon::Key),
        _ => (t.stage_completed_chip_fill, t.stage_completed_chip_text, t.stage_completed_bar, Icon::CheckCircle),
    };
    ui.interact(rect, ui.make_persistent_id("stage-chip-compact"), Sense::hover()).on_hover_text(label);
    let p = ui.painter();
    p.rect_filled(rect, 3, fill);
    p.rect_filled(Rect::from_min_size(rect.min, vec2(3.0, rect.height())), CornerRadius { nw: 3, sw: 3, ne: 0, se: 0 }, bar);
    icons::paint(p, Rect::from_center_size(rect.center(), vec2(12.0, 12.0)), icon, IconSize::S12, text);
}

fn shown_name(ui: &Ui, name: &str, max_width: f32) -> String {
    let p = ui.painter();
    let font = ty::shot_name();
    let fits = |s: &str| p.layout_no_wrap(s.to_owned(), font.clone(), tokens(ui).text_primary).size().x <= max_width;
    if fits(name) { return name.to_owned(); }
    let mut chars: Vec<char> = name.chars().collect();
    while !chars.is_empty() {
        let candidate = format!("{}…", chars.iter().collect::<String>());
        if fits(&candidate) { return candidate; }
        chars.pop();
    }
    "…".to_owned()
}
