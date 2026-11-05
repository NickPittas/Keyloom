//! Text-field cells from the reference; all y coordinates shifted by -2076.
use crate::ui::{controls::textfield::{TextField, State}, tokens::tokens, type_scale as ty};
use eframe::egui::{self, Ui, Rect, Sense};

pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    ui.painter().rect_filled(Rect::from_min_size(egui::pos2(968.0,0.0), egui::vec2(920.0,744.0)), 4, t.bg_panel);
    for (i,x,y,w,value,label,state) in [
        (0,985.0,2287.0,120.0,"","placeholder",State::Rest),
        (1,1117.0,2287.0,120.0,"endor-omarchy","hover",State::Hover),
        (2,1249.0,2287.0,120.0,"endor-omarchy","editing (focus)",State::Editing),
        (3,1381.0,2279.0,120.0,"endor omarchy","error, shown on Save only",State::Error),
        (4,1578.78125,2287.0,120.0,"endor-omarchy","disabled",State::Disabled),
        (5,985.0,2351.0,160.0,"endor-omarchy","",State::Editing),
        (6,985.0,2375.0,160.0,"render-node-07","",State::Rest),
    ] {
        let y = y-2076.0;
        let rect = Rect::from_min_size(egui::pos2(x,y),egui::vec2(w,22.0));
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min, egui::vec2(240.0,100.0))).layout(egui::Layout::top_down(egui::Align::Min)));
        child.spacing_mut().item_spacing.y = 2.0;
        let mut value = value.to_owned();
        let mut field = TextField::new(("specimen-textfield",i), &mut value,w).placeholder("Preset name").forced(state);
        if state == State::Error { field = field.error(Some("Use letters, numbers and dashes")); }
        if state == State::Disabled { field = field.disabled("This machine name is managed remotely"); }
        field.show(&mut child);
        if !label.is_empty() {
            let label_y = if state == State::Error { y+62.0 } else { y+28.0 };
            let g = crate::ui::controls::line(ui.painter(),label,ty::small(),14.0,f32::INFINITY);
            ui.painter().galley(egui::pos2(x,label_y),g,t.text_tertiary);
        }
    }
    let caption = "focus ring 2 px inside · 22 fields at 24 pitch, upper focused";
    let galley = crate::ui::controls::line(ui.painter(),caption,ty::small(),14.0,f32::INFINITY);
    ui.painter().galley(egui::pos2(985.0,327.0),galley,t.text_tertiary);
    let _ = ui.allocate_exact_size(egui::vec2(1.0,1.0),Sense::hover());
}
