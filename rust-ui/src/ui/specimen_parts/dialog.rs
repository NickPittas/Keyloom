//! Reference section §8.5–8.6, original x, dy = -3056 px.
use eframe::egui::{self, Ui, Rect, Sense, Stroke, StrokeKind, UiBuilder, Align, pos2, vec2};
use crate::ui::{controls::{dialog::{Dialog, DialogSize, empty_state, EmptyButton},line,paint_line},icons::{self,Icon,IconSize},tokens::tokens,type_scale as ty};

fn row(ui: &mut Ui, text: &str, font: egui::FontId, h: f32, color: egui::Color32) {
    let (r,_) = ui.allocate_exact_size(vec2(ui.available_width(),h),Sense::hover());
    let tr = Rect::from_min_max(r.min-vec2(0.375,0.),r.max);
    paint_line(ui.painter(),tr,line(ui.painter(),text,font,h,tr.width()),h,Align::Min,color,0.);
}
fn body(ui: &mut Ui, checked: bool) {
    let t = tokens(ui);
    ui.spacing_mut().item_spacing.y = 0.;
    row(ui,"The shot leaves this project. Source files stay on disk.",ty::label(),18.,t.text_primary);
    ui.add_space(8.);
    row(ui,"V1-0046_A002C008_261012_R4LF",egui::FontId::monospace(12.),18.,t.text_primary);
    ui.add_space(8.);
    let (r,resp) = ui.allocate_exact_size(vec2(ui.available_width(),18.),Sense::click());
    let box_rect = Rect::from_min_size(r.min+vec2(0.,2.),vec2(14.,14.));
    ui.painter().rect_filled(box_rect,2,if checked {t.accent} else {t.bg_sunken});
    if checked { ui.painter().add(egui::Shape::line(vec![box_rect.min+vec2(3.25,7.375),box_rect.min+vec2(5.65,9.625),box_rect.min+vec2(10.75,4.375)],Stroke::new(2.,t.text_on_accent))); }
    else { ui.painter().rect_stroke(box_rect,2,Stroke::new(1.,t.border_control),StrokeKind::Inside); }
    resp.widget_info(|| egui::WidgetInfo::selected(egui::accesskit::Role::CheckBox,true,checked,"Also delete generated frames, masks and renders (1.2 GB)"));
    let tr = Rect::from_min_max(r.min+vec2(20.-0.375,0.),r.max);
    paint_line(ui.painter(),tr,line(ui.painter(),"Also delete generated frames, masks and renders (1.2 GB)",ty::label(),18.,tr.width()),18.,Align::Min,t.text_primary,0.);
    if checked {
        ui.add_space(8.);
        let (r,_) = ui.allocate_exact_size(vec2(ui.available_width(),28.),Sense::hover());
        icons::paint(ui.painter(),Rect::from_min_size(r.min,vec2(14.,14.)),Icon::Warning,IconSize::S14,t.error);
        for (i,text) in ["1.2 GB of generated frames, masks and renders are deleted from", "disk. This can't be undone."].into_iter().enumerate() {
            let tr = Rect::from_min_size(r.min+vec2(20.-0.375,i as f32*14.),vec2(r.width()-20.,14.));
            paint_line(ui.painter(),tr,line(ui.painter(),text,ty::small(),14.,tr.width()),14.,Align::Min,t.error,0.);
        }
    }
}
pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    let sheet=Rect::from_min_size(pos2(1359.296875,32.),vec2(528.6875,884.));
    ui.painter().rect_filled(sheet,4,t.bg_panel);
    ui.painter().rect_stroke(sheet,4,Stroke::new(1.,t.border_divider),StrokeKind::Inside);
    let head=Rect::from_min_size(pos2(1376.296875,49.),vec2(494.6875,20.));
    paint_line(ui.painter(),head,line(ui.painter(),"8.5–8.6 · Dialog and empty state",ty::title(),20.,head.width()),20.,Align::Min,t.text_primary,0.);
    for (i,y,h) in [(0,81.,280.),(1,405.,300.)] {
        let rect=Rect::from_min_size(pos2(1376.296875,y),vec2(494.6875,h));
        ui.painter().rect_filled(rect,0,t.bg_panel);
        ui.painter().rect_stroke(rect,0,Stroke::new(1.,t.border_divider),StrokeKind::Inside);
        ui.painter().rect_filled(rect.shrink(1.),0,t.scrim);
        let mut child=ui.new_child(UiBuilder::new().max_rect(rect));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        Dialog::new(("specimen-dialog",i),"Remove shot",DialogSize::S)
            .body(move |ui| body(ui,i==1))
            .footer(|f| f.cancel("Cancel").destructive(if i==0 {"Remove shot"} else {"Remove shot and delete files"}))
            .show_in(&mut child,Rect::from_min_size(pos2(1423.640625,y+25.),vec2(400.,250.)),i==0);
    }
    let mut job=egui::text::LayoutJob::default();
    job.wrap.max_width=494.6875;
    job.append("unchecked (initial focus Cancel) · below: checked, the consequence line appears and the confirm label changes",0.,ty::format(ty::small(),t.text_tertiary,16.));
    ui.painter().galley(pos2(1376.296875,365.),ui.painter().layout_job(job),t.text_tertiary);
    let rect=Rect::from_min_size(pos2(1376.296875,717.),vec2(494.6875,182.));
    ui.painter().rect_filled(rect,0,t.bg_panel);
    ui.painter().rect_stroke(rect,0,Stroke::new(1.,t.border_divider),StrokeKind::Inside);
    let mut child=ui.new_child(UiBuilder::new().max_rect(rect.shrink(25.)));
    empty_state(&mut child,Icon::Import,"No shots yet","Import video files or a folder of image sequences to make shots.",Some(EmptyButton {label:"Import shots…",primary:true}));
}
