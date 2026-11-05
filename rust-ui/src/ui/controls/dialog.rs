//! Modal dialogs and empty states (bible §7.6, §8.5–8.6, §10.2).
use super::{buttons, line, paint_line};
use crate::ui::{icons::{self, Icon, IconSize}, tokens::tokens, type_scale as ty};
use eframe::egui::{self, accesskit::Role, Align, Color32, Context, Id, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo, vec2, pos2};

#[derive(Clone, Copy)]
pub enum DialogSize { S, M, L }
impl DialogSize {
    pub fn width(self) -> f32 { match self { Self::S => 400., Self::M => 560., Self::L => 720. } }
}
#[derive(Default)]
pub struct DialogOut {
    pub closed: bool,
    pub primary_clicked: bool,
    pub cancel_clicked: bool,
    pub ghost_clicked: bool,
}
#[derive(Default)]
pub struct Footer<'a> {
    ghost: Option<&'a str>, cancel: Option<&'a str>, primary: Option<&'a str>, danger: bool,
    min_widths: Option<(f32, f32)>, button_top: Option<f32>,
}
impl<'a> Footer<'a> {
    pub fn ghost_left(mut self, label: &'a str) -> Self { self.ghost = Some(label); self }
    pub fn cancel(mut self, label: &'a str) -> Self { self.cancel = Some(label); self }
    pub fn primary(mut self, label: &'a str) -> Self { self.primary = Some(label); self.danger = false; self }
    pub fn destructive(mut self, label: &'a str) -> Self { self.primary = Some(label); self.danger = true; self }
    /// Minimum Cancel and primary widths (defaults 64 and 72); the batch dialog's mockup uses 72 and 112.
    pub fn min_widths(mut self, cancel: f32, primary: f32) -> Self { self.min_widths = Some((cancel, primary)); self }
    /// Button top below the footer's top edge (default 13). The batch mockup puts them one row lower.
    pub fn button_top(mut self, y: f32) -> Self { self.button_top = Some(y); self }
}
pub struct Dialog<'a> {
    id: Id, title: &'a str, size: DialogSize,
    body: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    footer: Footer<'a>, initial: Option<Id>, fallback: Option<Id>,
}
#[derive(Clone)]
struct OpenState { opener: Option<Id>, frame: u64, pending_focus: bool }
impl<'a> Dialog<'a> {
    pub fn new(id: impl std::hash::Hash + std::fmt::Debug, title: &'a str, size: DialogSize) -> Self {
        Self { id: Id::unique(id), title, size, body: None, footer: Footer::default(), initial: None, fallback: None }
    }
    pub fn body<R>(mut self, body: impl FnOnce(&mut Ui) -> R + 'a) -> Self { self.body = Some(Box::new(move |ui| { body(ui); })); self }
    pub fn footer(mut self, footer: impl FnOnce(Footer<'a>) -> Footer<'a>) -> Self { self.footer = footer(Footer::default()); self }
    pub fn initial_focus(mut self, id: Id) -> Self { self.initial = Some(id); self }
    /// The opening panel's first control: return here after Primary can remove the opener.
    pub fn fallback_focus(mut self, id: Id) -> Self { self.fallback = Some(id); self }
    pub fn show(self, ctx: &Context) -> DialogOut {
        let key = self.id.with("open");
        let frame = ctx.cumulative_frame_nr();
        let previous = ctx.data(|d| d.get_temp::<OpenState>(key));
        let fresh = previous.as_ref().is_none_or(|s| s.frame + 1 < frame);
        let mut state = OpenState { opener: if fresh { ctx.memory(|m| m.focused()) } else { previous.as_ref().and_then(|s| s.opener) }, frame, pending_focus: fresh || previous.as_ref().is_some_and(|s| s.pending_focus) };
        ctx.data_mut(|d| d.insert_temp(key, state.clone()));
        let t = tokens(ctx);
        let title = self.title;
        let fallback = self.fallback;
        if self.footer.danger { ctx.input_mut(|i| { i.consume_key(egui::Modifiers::NONE,egui::Key::Enter); }); }
        let mut visible = false;
        // Modal is egui's Area with a blocking scrim and modal-layer focus traversal.
        let modal = egui::Modal::new(self.id).area(egui::Modal::default_area(self.id).fade_in(false)).backdrop_color(t.scrim).frame(egui::Frame::NONE).show(ctx, |ui| {
            let width = self.size.width().min((ctx.content_rect().width() - 32.).max(0.));
            visible = ui.is_visible();
            self.paint(ui, width, (ctx.content_rect().height() - 64.).max(0.), state.pending_focus && visible, None)
        });
        // Areas have an invisible sizing frame; initialize focus on the first visible frame.
        if visible { state.pending_focus = false; ctx.data_mut(|d| d.insert_temp(key,state.clone())); }
        modal.response.widget_info(|| WidgetInfo::labeled(Role::Dialog, true, title));
        let mut out = modal.inner;
        if modal.is_top_modal && !modal.any_popup_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            out.closed = true;
        }
        if out.closed {
            ctx.data_mut(|d| d.remove::<OpenState>(key));
            let opener = state.opener.filter(|id| ctx.read_response(*id).is_some());
            let restore = if out.primary_clicked { fallback.or(opener) } else { opener.or(fallback) };
            if let Some(id) = restore {
                ctx.memory_mut(|m| m.request_focus(id));
            } else {
                ctx.memory_mut(|m| m.move_focus(egui::FocusDirection::Next));
            }
        }
        out
    }
    /// Specimen frames use the real paint path without registering a live modal.
    pub fn show_in(self, ui: &mut Ui, rect: Rect, cancel_focus: bool) -> DialogOut {
        let mut child = ui.new_child(UiBuilder::new().id_salt(self.id).max_rect(rect));
        self.paint(&mut child, rect.width(), rect.height(), false, Some(cancel_focus))
    }
    fn paint(mut self, ui: &mut Ui, width: f32, max_height: f32, fresh: bool, forced: Option<bool>) -> DialogOut {
        let t = tokens(ui);
        let mut out = DialogOut::default();
        let shadow = egui::epaint::Shadow { offset: [0,8], blur: 24, spread: 0, color: Color32::from_black_alpha(if ui.visuals().dark_mode {128} else {51}) };
        // The 1 px border sits inside the dialog's size (§8.5: 560 × 172 with a 170 px content box), so the content is 2 px narrower.
        let width = width - 2.;
        let frame = egui::Frame::new().inner_margin(1).fill(t.bg_overlay).corner_radius(6).shadow(shadow).show(ui, |ui| {
            ui.set_width(width);
            ui.spacing_mut().item_spacing = vec2(0.,0.);
            let (title, title_response) = ui.allocate_exact_size(vec2(width,39.),Sense::hover());
            let title_text = Rect::from_min_size(title.min,vec2(width,40.)); // the sheet's body text starts 1 px above the 40 px title row
            let _ = title_response.on_hover_text(self.title);
            paint_line(ui.painter(),Rect::from_min_max(title_text.min+vec2(16.,0.),title_text.max-vec2(38.,0.)),line(ui.painter(),self.title,ty::title(),20.,width-56.),20.,Align::Min,t.text_primary,0.);
            out.closed = ui.add(buttons::icon_button(Icon::Close,buttons::IconButtonSize::Normal22,"Close dialog").at(pos2(title.right()-30.,title.top()+9.))).clicked();
            let before = ui.memory(|m| m.focused());
            if fresh && !self.footer.danger && self.initial.is_none() {
                ui.memory_mut(|m| { m.surrender_focus(before.unwrap_or(Id::NULL)); m.move_focus(egui::FocusDirection::Next); });
            }
            let scroll = egui::ScrollArea::vertical().id_salt(self.id.with("body")).max_height((max_height-92.).max(0.)).auto_shrink([false,true]).show(ui, |ui| {
                egui::Frame::new().inner_margin(egui::Margin { left:16,right:16,top:0,bottom:16 }).show(ui, |ui| {
                    ui.set_width((width-32.).max(0.));
                    ui.spacing_mut().item_spacing = vec2(8.,8.);
                    ui.style_mut().override_font_id = Some(ty::label());
                    ui.visuals_mut().override_text_color = Some(t.text_primary);
                    if let Some(body) = self.body.take() { body(ui); }
                });
            });
            if scroll.state.clamped_offset().y > 0. { ui.painter().hline(title.x_range(),title.bottom()+0.5,Stroke::new(1.,t.border_divider)); }
            let field = ui.memory(|m| m.focused()).filter(|id| Some(*id) != before);
            let (footer, _) = ui.allocate_exact_size(vec2(width,52.),Sense::hover());
            ui.painter().hline(footer.x_range(),footer.top()+0.5,Stroke::new(1.,t.border_overlay));
            let (cancel_min, primary_min) = self.footer.min_widths.unwrap_or((64.,72.));
            let button_y = footer.top()+self.footer.button_top.unwrap_or(13.);
            if let Some(label) = self.footer.ghost {
                out.ghost_clicked = ui.add(buttons::ghost_button(label).action_height().at(pos2(footer.left()+16.,button_y))).clicked();
            }
            let primary = self.footer.primary.map(|label| if self.footer.danger { buttons::destructive_confirm_button(label) } else { buttons::primary_button(label) }.action_height());
            let pw = primary.as_ref().map_or(0.,|b| b.natural_width(ui).max(if self.footer.danger {64.} else {primary_min}));
            let x = footer.right()-16.-pw;
            let mut primary_id = None;
            let mut cancel_id = None;
            if let Some(label) = self.footer.cancel {
                let button = buttons::secondary_button(label).action_height();
                let w = button.natural_width(ui).max(cancel_min);
                let cx = x-w-if primary.is_some() {8.} else {0.};
                let button = button.in_rect(Rect::from_min_size(pos2(cx,button_y),vec2(w,26.)));
                let r = ui.add(if forced == Some(true) { button.forced(buttons::Vis::Rest,true) } else { button });
                cancel_id = Some(r.id); out.cancel_clicked = r.clicked();
            }
            if let Some(button) = primary {
                let r = ui.add(button.in_rect(Rect::from_min_size(pos2(x,button_y),vec2(pw,26.))));
                primary_id = Some(r.id); out.primary_clicked = r.clicked();
            }
            if fresh {
                let initial = if self.footer.danger { cancel_id } else { self.initial.or(field).or(primary_id).or(cancel_id) };
                if let Some(id) = initial { ui.memory_mut(|m| m.request_focus(id)); }
            }
        });
        ui.painter().rect_stroke(frame.response.rect,6,Stroke::new(1.,t.border_overlay),egui::StrokeKind::Inside);
        out.closed |= out.cancel_clicked || out.primary_clicked;
        out
    }
}
/// Wrapping paragraph with the dialog's explicit 12/18 metrics.
pub fn paragraph(ui: &mut Ui, text: &str) {
    let mut job = egui::text::LayoutJob::default();
    job.append(text,0.,egui::TextFormat { font_id:ty::label(),color:tokens(ui).text_primary,line_height:Some(18.),..Default::default() });
    ui.add(egui::Label::new(job).wrap());
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Key, Modifiers, RawInput};
    fn input(key: Option<(Key,bool)>) -> RawInput {
        let mut i = RawInput { screen_rect:Some(Rect::from_min_size(pos2(0.,0.),vec2(900.,640.))),..Default::default() };
        if let Some((key,shift)) = key {
            i.events.push(Event::Key {key,physical_key:None,pressed:true,repeat:false,modifiers:Modifiers {shift,..Modifiers::NONE}});
        }
        i
    }
    fn frame(ctx: &Context, key: Option<(Key,bool)>, open: bool, danger: bool) -> (egui::FullOutput,Id,DialogOut) {
        let mut opener = Id::NULL;
        let mut result = DialogOut::default();
        let mut output = ctx.run_ui(input(key), |ui| {
            opener = ui.button("Open").id;
            let fallback = ui.button("Panel first").id;
            ctx.data_mut(|d| d.insert_temp(Id::unique("test-fallback"),fallback));
            if !open { ui.memory_mut(|m| m.request_focus(opener)); }
            if open {
                result=Dialog::new("test-dialog","Remove shot",DialogSize::M)
                    .fallback_focus(fallback)
                    .body(|ui| { assert!((ui.available_width()-526.).abs()<0.1); let _ = ui.button("First field"); let _ = ui.button("Second field"); })
                    .footer(|f| if danger { f.cancel("Cancel").destructive("Remove shot") } else {f.cancel("Cancel").primary("Save")})
                    .show(ctx);
            }
        });
        output.textures_delta.clear();
        (output,opener,result)
    }
    fn setup() -> Context { let ctx=Context::default();ctx.enable_accesskit();crate::ui::fonts::install(&ctx);ctx }
    fn named(out: &egui::FullOutput, name: &str) -> egui::accesskit::NodeId {
        out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().find(|(_,n)| n.label()==Some(name)).unwrap_or_else(|| panic!("missing {name}: {:?}",out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().map(|(_,n)| (n.role(),n.label())).collect::<Vec<_>>())).0
    }
    #[test]
    fn dialog_focus_trap_cycles_and_restores() {
        let ctx=setup();
        let (_,opener,_)=frame(&ctx,None,false,false);
        frame(&ctx,None,true,false);
        let (out,_,_)=frame(&ctx,None,true,false);
        let first=named(&out,"First field");
        assert!(out.platform_output.accesskit_update.as_ref().unwrap().nodes.iter().any(|(_,n)| n.role()==Role::Dialog && n.label()==Some("Remove shot")));
        assert_eq!(ctx.memory(|m|m.focused()).unwrap().accesskit_id(),first);
        let mut seen=std::collections::HashSet::new();
        for _ in 0..12 {
            frame(&ctx,Some((Key::Tab,false)),true,false);
            frame(&ctx,None,true,false);
            let focus=ctx.memory(|m|m.focused()).expect("cycle has focus");
            assert_ne!(focus,opener);
            seen.insert(focus);
        }
        assert_eq!(seen.len(),5,"close, two fields, Cancel, Save");
        for _ in 0..12 {
            frame(&ctx,Some((Key::Tab,true)),true,false);
            frame(&ctx,None,true,false);
            assert_ne!(ctx.memory(|m|m.focused()),Some(opener));
        }
        let (_,_,out)=frame(&ctx,Some((Key::Escape,false)),true,false);
        assert!(out.closed);
        assert_eq!(ctx.memory(|m|m.focused()),Some(opener));
    }
    #[test]
    fn dialog_escape_closes_and_returns_focus() {
        let ctx=setup();
        let (_,opener,_)=frame(&ctx,None,false,false);
        frame(&ctx,None,true,false);
        let (_,_,out)=frame(&ctx,Some((Key::Escape,false)),true,false);
        assert!(out.closed && !out.primary_clicked);
        assert_eq!(ctx.memory(|m|m.focused()),Some(opener));
    }
    #[test]
    fn destructive_dialog_initial_focus_is_cancel() {
        let ctx=setup();
        frame(&ctx,None,false,true);
        frame(&ctx,None,true,true);
        let (out,_,_)=frame(&ctx,None,true,true);
        assert_eq!(ctx.memory(|m|m.focused()).unwrap().accesskit_id(),named(&out,"Cancel"));
        let (_,_,out)=frame(&ctx,Some((Key::Enter,false)),true,true);
        assert!(!out.primary_clicked);
        frame(&ctx,Some((Key::Tab,false)),true,true);
        frame(&ctx,None,true,true);
        let (_,_,out)=frame(&ctx,Some((Key::Space,false)),true,true);
        assert!(out.primary_clicked && out.closed);
        assert_eq!(ctx.memory(|m|m.focused()),ctx.data(|d|d.get_temp::<Id>(Id::unique("test-fallback"))));
    }
}
pub struct EmptyButton<'a> { pub label: &'a str, pub primary: bool }
pub fn empty_state(ui: &mut Ui, icon: Icon, title: &str, text: &str, button: Option<EmptyButton<'_>>) -> bool {
    let t = tokens(ui);
    let area = ui.available_rect_before_wrap();
    let response = ui.allocate_rect(area,Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Group,true,title));
    ui.ctx().accesskit_node_builder(response.id,|n| n.set_description(text));
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = area.width().min(360.);
    job.halign = Align::Center;
    job.append(text,0.,egui::TextFormat { font_id:ty::label(),color:t.text_secondary,line_height:Some(16.),..Default::default() });
    let galley = ui.painter().layout_job(job);
    let height = 32.+12.+18.+4.+galley.size().y+if button.is_some() {34.} else {0.};
    let top = (area.center().y-height/2.).round();
    // §8.6 uses 1.5 px strokes; snap their axes to device pixels like the shared icon painter.
    let ppp = ui.ctx().pixels_per_point();
    let mut shapes = icons::shapes_at(ppp.max(1.5),Rect::from_center_size(pos2(area.center().x,top+16.),vec2(32.,32.)),icon,IconSize::S32,t.text_tertiary,Color32::TRANSPARENT,false);
    if ppp < 1.5 {
        for shape in &mut shapes {
            if let egui::Shape::Rect(r) = shape {
                r.rect = Rect::from_center_size((r.rect.center().to_vec2()*ppp).round().to_pos2()/ppp,(r.rect.size()*ppp).round()/ppp);
            }
        }
    }
    ui.painter().extend(shapes);
    let title_rect = Rect::from_min_size(pos2(area.left(),top+44.),vec2(area.width(),18.));
    paint_line(ui.painter(),title_rect,line(ui.painter(),title,egui::FontId::new(13.,egui::FontFamily::Name("Medium".into())),18.,area.width()),18.,Align::Center,t.text_primary,0.);
    ui.painter().galley(pos2(area.center().x.round(),top+66.),galley.clone(),t.text_secondary);
    button.is_some_and(|b| {
        let btn = if b.primary { buttons::primary_button(b.label) } else { buttons::secondary_button(b.label) };
        // Whole-pixel width and origin so the fill edges land on device pixels like the mockup.
        let w = btn.natural_width(ui).max(if b.primary {72.} else {64.}).round();
        ui.add(btn.at(pos2((area.center().x-w/2.).round(),(top+66.+galley.size().y+12.).round()))).clicked()
    })
}

