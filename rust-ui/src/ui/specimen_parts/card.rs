//! Reference §8.1 at x unchanged, dy = -3980.
use crate::ui::{controls::{card::*, chips::{Job,Stage}, dropdown::Item, line, paint_line}, icons::Icon, tokens::tokens, type_scale as ty};
use eframe::egui::{self, Align, Rect, Ui, UiBuilder};
pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    ui.painter().rect_filled(ui.max_rect(),0,t.bg_app);
    ui.painter().rect(Rect::from_min_size(egui::pos2(32.0,8.0),egui::vec2(1856.0,650.0)),4,t.bg_panel,egui::Stroke::new(1.0,t.border_divider),egui::StrokeKind::Inside);
    let title = line(ui.painter(),"8.1 · Board shot cards",ty::title(),20.0,f32::INFINITY);
    let title_width = title.size().x;
    paint_line(ui.painter(),Rect::from_min_size(egui::pos2(49.0,25.0),egui::vec2(title_width,20.0)),title,20.0,Align::Min,t.text_primary,0.0);
    let caption = "fixed 224 × 260 · stage bar y 126 always stage colour · job line y 123 + job badge top-right of thumbnail · meta row y 159 always · stage chip + status line y 181 · names end-truncate";
    let rr=Rect::from_min_size(egui::pos2(49.0+title_width+12.0,30.0),egui::vec2(1500.0,14.0));
    paint_line(ui.painter(),rr,line(ui.painter(),caption,ty::small(),14.0,1500.0),14.0,Align::Min,t.text_tertiary,0.0);
    let names = ["V1-0062_A002C001_261012_R4LF","V1-0046_A002C008_261012_R4LF","A003C001_261012_R4LF_intervi…","V1-0080_A003C004_261013_R4LF","V1-0083_A003C006_261013_R4LF","V1-0086_A003C007_261013_R4LF","V1-0046_A002C008_261012_R4LF","V1-0090_A004C002_261014_R4LF"];
    let stages = [Stage::Raw,Stage::Mask,Stage::Ready,Stage::Ready,Stage::Mask,Stage::Ready,Stage::Completed,Stage::Ready];
    let jobs = [None,None,None,Some(Job::Queued),Some(Job::Rendering(0.34)),Some(Job::Preparing),Some(Job::Failed),Some(Job::Failed)];
    let meta = ["3200×1800 · 24 fps · 65 fr","3200×1800 · 24 fps · 108 fr","4096×2160 · 25 fps · 240 fr","3200×1800 · 24 fps · 96 fr","3200×1800 · 24 fps · 65 fr","3200×1800 · 24 fps · 80 fr","3200×1800 · 24 fps · 108 fr","3840×2160 · 24 fps · 72 fr"];
    let status = ["No result yet","Pre-matte done","Hint done","Waiting for a free machine","Keying 34% · frame 22 / 65","Preparing · loading model","Re-key failed: out of VRAM","Can't key: machine offline"];
    let machines = ["This machine","Automatic","render-node-07-with-a-long-name","Automatic","endor-omarchy","endor-omarchy","This machine","endor-omarchy"];
    let captions = ["Raw · selected","Mask","Ready · one ellipsis · hover","Ready + job Queued (stage bar kept)","Mask + job Rendering","Ready + job Preparing (static 24 px, no %)","Completed + re-key Failed","Ready + job Failed"];
    for k in 0..8 {
        let x = 49.0 + (k%7) as f32*236.0;
        let y = if k<7 { 57.0 } else { 355.0 };
        let items = [Item { label:machines[k],icon:Some(if k==0 || k==6 { Icon::Machine } else if k==1 || k==3 { Icon::Refresh } else { Icon::MachineNetwork }),dot:if k==1 || k==3 { None } else { Some(if k==7 { t.error } else { t.success }) }, ..Default::default() }];
        let mut child = ui.new_child(UiBuilder::new().id_salt(k).max_rect(Rect::from_min_size(egui::pos2(x,y),SIZE)));
        let _ = shot_card(&mut child,&CardData { name:names[k],stage:stages[k],job:jobs[k],meta:meta[k],status_line:status[k],machine:Machine { items:&items,selected:0,tooltip:if k==7 { "endor-omarchy — offline since 14:02" } else { machines[k] } },selected:k==0,hovered:k==2,reduced_motion:true,plate_variant:k,..Default::default() });
        let r=Rect::from_min_size(egui::pos2(x,y+270.0),egui::vec2(224.0,14.0));
        paint_line(ui.painter(),r,line(ui.painter(),captions[k],ty::small(),14.0,224.0),14.0,Align::Min,t.text_tertiary,0.0);
    }
}
