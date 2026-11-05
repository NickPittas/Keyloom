//! Board card, bible §8.1. The Board owns composite navigation and the shared context menu.
use super::{buttons::{icon_button, IconButtonSize}, checkbox::Check, chips::{self, Job, Stage}, dropdown::{Dropdown, Item}, keyboard_focus, line, paint_line};
use crate::ui::{icons::Icon, tokens::{tokens, Tokens}, type_scale as ty};
use eframe::egui::{self, accesskit::Role, Align, Color32, FontId, Key, Rect, Response, Sense, Stroke, StrokeKind, TextureId, Ui, UiBuilder, WidgetInfo};

pub const SIZE: egui::Vec2 = egui::vec2(224.0, 260.0);
pub struct Machine<'a> {
    pub items: &'a [Item<'a>],
    pub selected: usize,
    pub tooltip: &'a str,
}
pub struct CardData<'a> {
    pub name: &'a str,
    pub thumb: Option<TextureId>,
    pub thumb_aspect: f32,
    pub thumb_label: &'a str,
    pub thumb_tooltip: &'a str,
    pub specimen: bool,
    pub stage: Stage,
    pub job: Option<Job>,
    pub meta: &'a str,
    pub status_line: &'a str,
    pub blocked: bool,
    pub machine: Machine<'a>,
    pub selected: bool,
    /// Only the Board's current card is a Tab stop.
    pub focusable: bool,
    pub hovered: bool,
    pub forced_focus: bool,
    pub reduced_motion: bool,
    /// Specimen plate variant; ignored when a thumbnail exists.
    pub plate_variant: usize,
}
impl Default for CardData<'_> {
    fn default() -> Self {
        Self { name: "", thumb: None, thumb_aspect: 16.0/9.0, thumb_label: "", thumb_tooltip: "", specimen: false, stage: Stage::Raw, job: None, meta: "— · — · —", status_line: "", blocked: false,
            machine: Machine { items: &[], selected: 0, tooltip: "" }, selected: false, focusable: true, hovered: false, forced_focus: false, reduced_motion: false, plate_variant: 0 }
    }
}
pub struct CardOut {
    pub response: Response,
    pub open: bool,
    pub select_toggled: bool,
    pub machine_changed: Option<usize>,
    /// More, right click and Shift+F10 all request the caller's same shot menu.
    pub context_menu: bool,
}

fn clip_token(name: &str) -> Option<usize> {
    name.as_bytes().windows(8).position(|b| b[0].is_ascii_uppercase() && b[1..4].iter().all(u8::is_ascii_digit) && b[4] == b'C' && b[5..8].iter().all(u8::is_ascii_digit))
}
/// End truncation, preserving the complete camera clip token with at most one ellipsis.
pub fn shot_name(name: &str, width: f32, measure: impl Fn(&str) -> f32) -> String {
    if measure(name) <= width { return name.to_owned() }
    let mut source = name;
    if let Some(i) = clip_token(name) {
        if measure(&format!("{}…", &name[..i+8])) > width {
            source = &name[i..];
            let leading = format!("…{source}");
            if measure(&leading) <= width { return leading }
        }
    }
    let mut end = source.len();
    while end > 0 {
        end = source[..end].char_indices().next_back().unwrap().0;
        let s = format!("{}…", &source[..end]);
        if measure(&s) <= width { return s }
    }
    "…".to_owned()
}
fn status_color(t: &Tokens, job: Option<Job>, blocked: bool) -> Color32 {
    if blocked && job.is_none() { t.error } else { t.text_secondary }
}
fn text(ui: &Ui, r: Rect, s: &str, font: FontId, lh: f32, color: Color32) {
    paint_line(ui.painter(), r, line(ui.painter(), s, font, lh, r.width()), lh, Align::Min, color, 0.0);
}
pub fn shot_card(ui: &mut Ui, data: &CardData<'_>) -> CardOut {
    let (r, response) = ui.allocate_exact_size(SIZE, if data.focusable { Sense::click() } else { Sense::CLICK });
    let t = tokens(ui);
    let slot = |x, y, w, h| Rect::from_min_size(r.min + egui::vec2(x,y), egui::vec2(w,h));
    let machine_name = data.machine.items.get(data.machine.selected).map_or("Unassigned", |i| i.label);
    let stage_name = format!("{:?}", data.stage);
    let job_name = match data.job { None => String::new(), Some(Job::Rendering(v)) => format!("Rendering {:.0}%", v*100.0), Some(j) => format!("{j:?}") };
    let thumb_name = if data.thumb.is_none() { data.thumb_label } else { "" };
    response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), format!("{}, {}, {}, {}, {}", data.name, stage_name, job_name, machine_name, thumb_name)));
    ui.ctx().accesskit_node_builder(response.id, |n| n.set_selected(data.selected));
    let p = ui.painter();
    p.rect_filled(r, 4, t.bg_raised);
    p.rect_stroke(r.expand(1.0), 5, Stroke::new(1.0, if data.hovered || response.contains_pointer() { t.border_hover } else { t.border_default }), StrokeKind::Inside);
    if data.selected { p.rect_stroke(r.expand(3.0), 7, Stroke::new(2.0, t.accent_bright), StrokeKind::Inside); }
    let thumb = slot(0.0,0.0,224.0,126.0);
    p.rect_filled(thumb, egui::CornerRadius { nw:4, ne:4, sw:0, se:0 }, t.viewer_surround);
    if let Some(texture) = data.thumb {
        let aspect = if data.thumb_aspect.is_finite() && data.thumb_aspect > 0.0 { data.thumb_aspect } else { 16.0/9.0 };
        let size = if aspect > thumb.aspect_ratio() { egui::vec2(thumb.width(), thumb.width()/aspect) } else { egui::vec2(thumb.height()*aspect, thumb.height()) };
        p.image(texture, Rect::from_center_size(thumb.center(), size), Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0,1.0)), Color32::WHITE);
    } else if data.specimen {
        // The flat specimen plate is intentionally identical in both themes, like footage.
        let plate = slot(22.0,6.0,178.0,114.0);
        p.rect_filled(plate, 0, if data.plate_variant%2 == 0 { Color32::from_rgb(47,111,58) } else { Color32::from_rgb(44,104,52) });
        for (x,h) in [(60.0,52.0),(84.0,56.0)] {
            p.rect_filled(Rect::from_min_size(plate.min + egui::vec2(x + data.plate_variant as f32*6.0,114.0-14.0-h), egui::vec2(18.0,h)), egui::CornerRadius { nw:9, ne:9, sw:3, se:3 }, Color32::from_rgb(30,36,32));
        }
    } else {
        let placeholder = Rect::from_min_size(slot(8.0, 6.0, 208.0, 114.0).min, egui::vec2(208.0, 114.0));
        p.rect_filled(placeholder, 2, t.bg_sunken);
        let label = if data.thumb_label.is_empty() { "No preview" } else { data.thumb_label };
        let g = line(&p, label, ty::small(), 14.0, placeholder.width() - 16.0);
        paint_line(&p, placeholder, g, 14.0, Align::Center, t.text_tertiary, 0.0);
        let tip = if data.thumb_tooltip.is_empty() { label } else { data.thumb_tooltip };
        child_placeholder_tooltip(ui, response.id, placeholder, tip);
    }
    let mut child = ui.new_child(UiBuilder::new().id_salt(response.id).max_rect(r).layout(egui::Layout::top_down(Align::Min)));
    child.set_clip_rect(r.intersect(ui.clip_rect()));
    child.painter().rect_filled(slot(6.0,6.0,20.0,20.0), 3, Color32::from_black_alpha(140));
    let mut selected = data.selected;
    // §8.1's 16 px box reuses the standard 14 px checkbox paint path.
    let center = r.min + egui::vec2(16.0,16.0);
    let scale = 16.0/14.0;
    let transform = egui::emath::TSTransform::new(center.to_vec2()*(1.0-scale),scale);
    // Sits on footage in both themes (sheet: unchecked box = dark translucent fill), so an unchecked box uses the Dark paint.
    let check = child.with_visual_transform(transform, |ui| ui.scope(|ui| {
        if !data.selected { ui.visuals_mut().dark_mode = true; }
        ui.add(Check::new(&mut selected, None).at(r.min + egui::vec2(9.0,5.0)))
    }).inner).inner.on_hover_text("Select shot");
    check.widget_info(|| WidgetInfo::selected(Role::CheckBox, ui.is_enabled(), selected, "Select shot"));
    if let Some(job) = data.job {
        let label = match job { Job::Queued => "Queued".to_owned(), Job::Preparing => "Preparing".to_owned(), Job::Failed => "Failed".to_owned(), Job::Rendering(v) => format!("{:.0}%", v.clamp(0.0,1.0)*100.0) };
        let w = (line(child.painter(), &label, ty::chip(),14.0,f32::INFINITY).size().x + 28.0).round();
        let badge_rect = slot(218.0-w,6.0,w,18.0);
        child.painter().rect_filled(badge_rect.expand(2.0), 3, Color32::from_black_alpha(140));
        let mut badge_ui = child.new_child(UiBuilder::new().max_rect(badge_rect));
        chips::job_badge_motion(&mut badge_ui,job,data.reduced_motion);
        let jr = slot(0.0,123.0,224.0,3.0);
        match job {
            Job::Queued => { for x in (0..224).step_by(7) { child.painter().rect_filled(slot(x as f32,123.0,4.0_f32.min(224.0-x as f32),3.0),0,t.busy_queued); } }
            Job::Rendering(v) => {
                child.painter().rect_filled(jr,0,Color32::from_black_alpha(102));
                let mut progress_ui = child.new_child(UiBuilder::new().max_rect(jr));
                let fraction = if v.is_finite() { v.clamp(0.0,1.0) } else { 0.0 };
                progress_ui.set_clip_rect(jr.with_max_x(jr.left()+jr.width()*fraction).intersect(child.clip_rect()));
                chips::progress(&mut progress_ui,jr,chips::Progress { value:Some(v), state:chips::State::Running, thin:true,label:"Keying" });
            }
            Job::Preparing => { child.painter().rect_filled(jr,0,Color32::from_black_alpha(102)); child.painter().rect_filled(slot(0.0,123.0,24.0,3.0),0,t.busy_fill); }
            Job::Failed => {}
        }
    }
    let bar = match data.stage { Stage::Raw => t.stage_raw_bar, Stage::Mask => t.stage_mask_bar, Stage::Ready => t.stage_ready_bar, Stage::Completed => t.stage_completed_bar };
    child.painter().rect_filled(slot(0.0,126.0,224.0,3.0),0,bar);
    let name = shot_name(data.name,208.0,|s| line(child.painter(),s,ty::button(),16.0,f32::INFINITY).size().x);
    text(&child,slot(8.0,137.0,208.0,18.0),&name,ty::button(),16.0,t.text_primary);
    let name_response = child.interact(slot(8.0,137.0,208.0,18.0),response.id.with("name"),Sense::CLICK).on_hover_text(data.name);
    let mut meta = data.meta;
    while line(child.painter(),meta,ty::mono(),14.0,f32::INFINITY).size().x > 208.0 {
        if let Some((_,rest)) = meta.split_once(" · ") { meta = rest; } else { break }
    }
    text(&child,slot(8.0,159.0,208.0,14.0),meta,ty::mono(),14.0,t.text_tertiary);
    child.interact(slot(8.0,159.0,208.0,14.0),response.id.with("meta"),Sense::hover()).on_hover_text(data.meta);
    let mut stage_ui = child.new_child(UiBuilder::new().max_rect(slot(8.0,181.0,208.0,18.0)));
    chips::stage_chip(&mut stage_ui,data.stage);
    let chip_width = (line(child.painter(),&stage_name,ty::chip(),14.0,f32::INFINITY).size().x+28.0).max(56.0);
    let status_x = 8.0+chip_width+6.0;
    let status = slot(status_x,181.0,216.0-status_x,18.0);
    text(&child,status,data.status_line,ty::small(),14.0,status_color(t,data.job,data.blocked));
    child.interact(status,response.id.with("status"),Sense::hover()).on_hover_text(data.status_line);
    let mut machine = data.machine.selected;
    let dropdown = Dropdown::new("machine",&mut machine,data.machine.items,208.0).name("Machine").placeholder("Assign…").at(r.min+egui::vec2(8.0,201.0)).show(&mut child).on_hover_text(data.machine.tooltip);
    let open = child.add(icon_button(Icon::Open,IconButtonSize::Normal22,"Open shot (Enter)").at(r.min+egui::vec2(8.0,229.0)));
    let more = child.add(icon_button(Icon::More,IconButtonSize::Normal22,"Shot menu (Shift+F10)").at(r.min+egui::vec2(194.0,229.0)));
    let focused = response.has_focus();
    if data.forced_focus || focused && keyboard_focus(ui.ctx()) { child.painter().rect_stroke(r,4,Stroke::new(2.0,t.focus_ring),StrokeKind::Inside); }
    response.clone().on_hover_text(data.name);
    let enter = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE,Key::Enter));
    let space = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE,Key::Space));
    let menu_key = focused && ui.input_mut(|i| i.consume_key(egui::Modifiers::SHIFT,Key::F10));
    CardOut { open:open.clicked() || enter || name_response.double_clicked() || response.double_clicked() && response.interact_pointer_pos().is_some_and(|p| thumb.contains(p)),
        select_toggled:check.changed() || space, machine_changed:dropdown.changed().then_some(machine), context_menu:more.clicked() || ui.rect_contains_pointer(r) && ui.input(|i| i.pointer.button_clicked(egui::PointerButton::Secondary)) || menu_key, response }
}

fn child_placeholder_tooltip(ui: &Ui, id: egui::Id, rect: Rect, tip: &str) {
    ui.interact(rect, id.with("thumbnail-placeholder"), Sense::hover()).on_hover_text(tip);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shot_name_keeps_clip_token_one_ellipsis() {
        let measure = |s: &str| s.chars().count() as f32;
        assert_eq!(shot_name("long_prefix_A002C008_tail",15.0,measure),"…A002C008_tail");
        assert_eq!(shot_name("long_prefix_A003C001_261012_R4LF_interview",20.0,measure),"A003C001_261012_R4L…");
        assert_eq!(shot_name("plain_long_name",8.0,measure),"plain_l…");
        assert_eq!(shot_name("V1_A002C008_tail",14.0,measure),"V1_A002C008_t…");
    }
    #[test]
    fn failed_badge_makes_status_line_secondary() {
        for t in [&crate::ui::tokens::DARK,&crate::ui::tokens::LIGHT] {
            assert_eq!(status_color(t,Some(Job::Failed),true),t.text_secondary);
            assert_eq!(status_color(t,None,true),t.error);
            assert_eq!(status_color(t,Some(Job::Queued),true),t.text_secondary);
        }
    }
    #[test]
    fn card_size_is_fixed_for_long_names_and_machines() {
        let ctx = egui::Context::default(); crate::ui::fonts::install(&ctx);
        let items = [Item::new("render-node-07-with-an-extremely-long-machine-name")];
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let data = CardData { name:"long_prefix_A003C001_261012_R4LF_interview",machine:Machine { items:&items,selected:0,tooltip:"" }, ..Default::default() };
            assert_eq!(shot_card(ui,&data).response.rect.size(),SIZE);
        }); out.textures_delta.clear();
    }
}
