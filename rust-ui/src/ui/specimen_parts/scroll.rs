//! Reference sub-block at its original x, shifted by dy = -3040 px.
use crate::ui::{controls::{line, paint_line, scroll::{self, Axis, State}}, tokens::tokens, type_scale as ty};
use eframe::egui::{self, pos2, vec2, Align, Id, Rect, Stroke, StrokeKind, Ui, UiBuilder};
pub fn show(ui: &mut Ui) {
    let t = tokens(ui);
    ui.painter().rect_filled(Rect::from_min_size(pos2(815.59375, 260.0), vec2(528.703125, 360.0)), 0, t.bg_panel);
    let heading = Rect::from_min_size(pos2(831.59375, 276.0), vec2(495.0, 16.0));
    paint_line(ui.painter(), heading, line(ui.painter(), "Scrollbar: rest · hover · drag     Splitter: rest · hover / drag", ty::section_header(), 16.0, 495.0), 16.0, Align::Min, t.text_secondary, 0.0);
    for (i, state) in [State::Rest, State::Hover, State::Drag].into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(831.59375 + i as f32 * 132.0, 300.0), vec2(120.0, 120.0));
        ui.painter().rect_filled(rect, 0, t.bg_panel);
        ui.painter().rect_stroke(rect, 0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink(1.0)).layout(egui::Layout::top_down(Align::Min)));
        child.set_clip_rect(rect.shrink(1.0).intersect(ui.clip_rect()));
        let content_height = 118.0 * 118.0 / 44.0;
        let top = if state == State::Drag { 50.0 } else { 16.0 };
        scroll::scroll_v_forced(&mut child, ("scroll-specimen", i), state, top * content_height / 118.0, |ui| {
            ui.allocate_space(vec2(10.0, content_height));
        });
    }
    for (i, state) in [State::Rest, State::Hover, State::Drag].into_iter().enumerate() {
        let rect = Rect::from_min_size(pos2(831.59375 + i as f32 * 132.0, 432.0), vec2(120.0, 120.0));
        ui.painter().rect_filled(rect, 0, t.bg_panel);
        ui.painter().rect_stroke(rect, 0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
        let hit = Rect::from_min_size(pos2(rect.left() + 57.0, rect.top() + 1.0), vec2(6.0, 118.0));
        let mut child = ui.new_child(UiBuilder::new().max_rect(hit).layout(egui::Layout::top_down(Align::Min)));
        let mut size = 150.0;
        scroll::splitter_forced(&mut child, Id::unique(("split-specimen", i)), Axis::Horizontal, &mut size, 100.0, 200.0, 150.0, Some(state));
    }
    let mut caption = egui::text::LayoutJob::default();
    caption.wrap.max_width = 494.703125;
    caption.append("Thumb scroll.thumb · hover scroll.thumb-hover · drag scroll.thumb-drag (≥ 3:1). Track 8, thumb 6, always visible on overflow. Splitter: 1 px line, 6 px hit area, 2 px accent on hover.", 0.0, ty::format(ty::small(), t.text_tertiary, 14.0));
    ui.painter().galley(pos2(831.59375, 558.0), ui.painter().layout_job(caption), t.text_tertiary);
}
