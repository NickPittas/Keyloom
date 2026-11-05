//! Scroll areas and keyboard-resizable splitters (bible §5.14–5.15, §10.2, §12).
use super::keyboard_focus;
use crate::ui::{theme, tokens::tokens};
use eframe::egui::{self, accesskit::Role, pos2, vec2, Id, Key, Rect, Response, Sense, Shape, Stroke, StrokeKind, Ui, WidgetInfo};
pub use egui::containers::scroll_area::ScrollAreaOutput;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State { Rest, Hover, Drag }
/// Axis of the size being changed: Horizontal paints a vertical divider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Axis { Horizontal, Vertical }

pub fn scroll_v<R>(ui: &mut Ui, id: impl egui::AsIdSalt, add: impl FnOnce(&mut Ui) -> R) -> ScrollAreaOutput<R> {
    scroll_area(ui, egui::ScrollArea::vertical().id_salt(id), false, None, add)
}
/// One scroll area for two-axis table overflow.
pub fn scroll_both<R>(ui: &mut Ui, id: impl egui::AsIdSalt, add: impl FnOnce(&mut Ui) -> R) -> ScrollAreaOutput<R> {
    scroll_area(ui, egui::ScrollArea::both().id_salt(id), true, None, add)
}
/// Specimen overrides use the native ScrollArea paint path.
pub fn scroll_v_forced<R>(ui: &mut Ui, id: impl egui::AsIdSalt, state: State, offset: f32, add: impl FnOnce(&mut Ui) -> R) -> ScrollAreaOutput<R> {
    scroll_area(ui, egui::ScrollArea::vertical().id_salt(id).vertical_scroll_offset(offset), false, Some(state), add)
}
fn scroll_area<R>(ui: &mut Ui, area: egui::ScrollArea, horizontal: bool, forced: Option<State>, add: impl FnOnce(&mut Ui) -> R) -> ScrollAreaOutput<R> {
    assert!(!ui.stack().iter().any(|s| s.kind() == Some(egui::UiKind::ScrollArea)), "A panel must have only one scroll area");
    let t = tokens(ui);
    let original = ui.style().clone();
    let overlay = ui.visuals().panel_fill == t.bg_overlay || ui.stack().bg_color() == t.bg_overlay;
    let surface = if overlay { t.bg_overlay } else { t.bg_panel };
    let divider = if overlay { t.border_overlay } else { t.border_divider };
    theme::scroll_thumb_scope(ui, |ui| {
        let style = ui.style_mut();
        style.spacing.scroll = egui::style::ScrollStyle { bar_width: 6.0, bar_inner_margin: 1.0, bar_outer_margin: 1.0, handle_min_length: 24.0, ..egui::style::ScrollStyle::solid() };
        style.spacing.scroll.fade.strength = 0.0;
        style.visuals.extreme_bg_color = surface;
        for w in [&mut style.visuals.widgets.inactive, &mut style.visuals.widgets.hovered, &mut style.visuals.widgets.active] {
            w.corner_radius = 3.into();
            if let Some(s) = forced { w.bg_fill = match s { State::Rest => t.scroll_thumb, State::Hover => t.scroll_thumb_hover, State::Drag => t.scroll_thumb_drag }; }
        }
        let available = ui.available_rect_before_wrap();
        let background = ui.painter().add(Shape::Noop);
        let out = area.auto_shrink([false, false]).scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::VisibleWhenNeeded).show(ui, |ui| {
            // Scroll colours belong to bars, not controls inside the viewport.
            ui.set_style(original);
            add(ui)
        });
        let r = out.inner_rect;
        let v = out.content_size.y > r.height();
        let h = horizontal && out.content_size.x > r.width();
        // Native bars animate their reserved space; overflow must have a full bar on this frame.
        ui.ctx().animate_bool_with_time(out.id.with("v"), v, 0.0);
        ui.ctx().animate_bool_with_time(out.id.with("h"), h, 0.0);
        if (available.right() - r.right() - if v { 8.0 } else { 0.0 }).abs() > 0.5
            || (available.bottom() - r.bottom() - if h { 8.0 } else { 0.0 }).abs() > 0.5 {
            ui.ctx().request_discard("Scrollbar space changed");
        }
        let mut tracks = Vec::new();
        if v {
            tracks.push(Shape::rect_filled(Rect::from_min_size(r.right_top(), vec2(8.0, r.height())), 0, surface));
            tracks.push(Shape::rect_filled(Rect::from_min_size(r.right_top(), vec2(1.0, r.height())), 0, divider));
        }
        if h {
            tracks.push(Shape::rect_filled(Rect::from_min_size(r.left_bottom(), vec2(r.width(), 8.0)), 0, surface));
            tracks.push(Shape::rect_filled(Rect::from_min_size(r.left_bottom(), vec2(r.width(), 1.0)), 0, divider));
        }
        if v && h { tracks.push(Shape::rect_filled(Rect::from_min_size(r.right_bottom(), vec2(8.0, 8.0)), 0, surface)); }
        ui.painter().set(background, Shape::Vec(tracks));
        out
    }).inner
}

/// Toolbar/View-menu toggle; zero means collapsed, and the last expanded size is restored.
pub fn toggle_splitter(ui: &Ui, id: Id, size: &mut f32, min: f32, max: f32, default: f32) {
    let saved = id.with("expanded-size");
    if *size == 0.0 {
        *size = ui.data(|d| d.get_temp::<f32>(saved)).unwrap_or(default).clamp(min, max);
    } else {
        ui.data_mut(|d| d.insert_temp(saved, size.clamp(min, max)));
        *size = 0.0;
    }
}
pub fn splitter(ui: &mut Ui, id: Id, axis: Axis, size: &mut f32, min: f32, max: f32, default: f32) -> Response {
    splitter_forced(ui, id, axis, size, min, max, default, None)
}
pub fn splitter_forced(ui: &mut Ui, id: Id, axis: Axis, size: &mut f32, min: f32, max: f32, default: f32, forced: Option<State>) -> Response {
    assert!(min.is_finite() && max.is_finite() && min >= 0.0 && min <= max && default.is_finite());
    let old = *size;
    if !size.is_finite() { *size = default.clamp(min, max); }
    if *size != 0.0 { *size = size.clamp(min, max); }
    let extent = match axis { Axis::Horizontal => vec2(6.0, ui.available_height()), Axis::Vertical => vec2(ui.available_width(), 6.0) };
    let (rect, _) = ui.allocate_exact_size(extent, Sense::hover());
    let mut response = ui.interact(rect, id, Sense::click_and_drag());
    if response.clicked() || response.drag_started() { response.request_focus(); }
    if response.has_focus() {
        ui.memory_mut(|m| m.set_focus_lock_filter(id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() }));
        let (minus, plus) = match axis { Axis::Horizontal => (Key::ArrowLeft, Key::ArrowRight), Axis::Vertical => (Key::ArrowUp, Key::ArrowDown) };
        let (delta, toggle) = ui.input_mut(|i| {
            let step = if i.modifiers.shift { 32.0 } else { 8.0 };
            let modifiers = i.modifiers;
            let delta = (i.consume_key(modifiers, plus) as i32 - i.consume_key(modifiers, minus) as i32) as f32 * step;
            (delta, i.consume_key(egui::Modifiers::NONE, Key::Enter))
        });
        if toggle { toggle_splitter(ui, id, size, min, max, default); }
        if delta != 0.0 { *size = (*size + delta).clamp(min, max); }
    }
    if response.dragged() {
        let delta = ui.input(|i| i.pointer.delta());
        *size = (*size + match axis { Axis::Horizontal => delta.x, Axis::Vertical => delta.y }).clamp(min, max);
    }
    if response.double_clicked() { *size = default.clamp(min, max); }
    if *size != old { response.mark_changed(); }
    response.widget_info(|| WidgetInfo::labeled(Role::Splitter, ui.is_enabled(), "Resize panel"));
    ui.ctx().accesskit_node_builder(id, |n| {
        n.set_numeric_value(*size as f64); n.set_min_numeric_value(0.0); n.set_max_numeric_value(max as f64); n.set_numeric_value_step(8.0);
        n.set_description("Arrows resize by 8 px; Shift by 32 px. Enter collapses or expands. Double-click resets.");
        n.set_orientation(match axis { Axis::Horizontal => egui::accesskit::Orientation::Vertical, Axis::Vertical => egui::accesskit::Orientation::Horizontal });
    });
    let state = forced.unwrap_or(if response.dragged() { State::Drag } else if response.hovered() { State::Hover } else { State::Rest });
    let t = tokens(ui);
    let (width, color) = if state == State::Rest || !ui.is_enabled() { (1.0, t.border_divider) } else { (2.0, t.accent_bright) };
    let line = match axis {
        Axis::Horizontal => Rect::from_min_size(pos2(rect.center().x.floor(), rect.top()), vec2(width, rect.height())),
        Axis::Vertical => Rect::from_min_size(pos2(rect.left(), rect.center().y.floor()), vec2(rect.width(), width)),
    };
    ui.painter().rect_filled(line, 0, color);
    if ui.is_enabled() && response.has_focus() && keyboard_focus(ui.ctx()) {
        ui.painter().rect_stroke(rect, 0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
    }
    response.on_hover_and_drag_cursor(match axis { Axis::Horizontal => egui::CursorIcon::ResizeHorizontal, Axis::Vertical => egui::CursorIcon::ResizeVertical })
        .on_hover_text("Resize panel\nArrows: 8 px · Shift: 32 px · Enter: collapse or expand · Double-click: reset")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn frame(ctx: &egui::Context, size: &mut f32, axis: Axis, mut events: Vec<egui::Event>, time: f64) -> egui::CursorIcon {
        let modifiers = events.iter().find_map(|e| if let egui::Event::Key { modifiers, .. } = e { Some(*modifiers) } else { None }).unwrap_or_default();
        events.insert(0, egui::Event::ModifiersChanged(modifiers));
        let out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(300.0, 200.0))), events, time: Some(time), ..Default::default() }, |root| {
            egui::CentralPanel::default().show(root, |ui| { splitter(ui, Id::unique("split"), axis, size, 100.0, 200.0, 150.0); });
        });
        let cursor = out.platform_output.cursor_icon;
        out.drop_without_applying_deltas();
        cursor
    }
    fn key(key: Key, shift: bool) -> egui::Event { egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers { shift, ..Default::default() } } }
    #[test]
    fn splitter_keyboard_resize_and_clamp() {
        for (axis, minus, plus) in [(Axis::Horizontal, Key::ArrowLeft, Key::ArrowRight), (Axis::Vertical, Key::ArrowUp, Key::ArrowDown)] {
            let ctx = egui::Context::default(); let mut size = 150.0;
            let _ = frame(&ctx, &mut size, axis, vec![], 0.0);
            ctx.memory_mut(|m| m.request_focus(Id::unique("split")));
            let _ = frame(&ctx, &mut size, axis, vec![key(plus, false)], 0.1); assert_eq!(size, 158.0);
            let _ = frame(&ctx, &mut size, axis, vec![key(plus, true)], 0.2); assert_eq!(size, 190.0);
            let _ = frame(&ctx, &mut size, axis, vec![key(plus, true)], 0.3); assert_eq!(size, 200.0);
            size = 110.0;
            let _ = frame(&ctx, &mut size, axis, vec![key(minus, true)], 0.4); assert_eq!(size, 100.0);
            let _ = frame(&ctx, &mut size, axis, vec![key(Key::Enter, false)], 0.5); assert_eq!(size, 0.0);
            let _ = frame(&ctx, &mut size, axis, vec![key(Key::Enter, false)], 0.6); assert_eq!(size, 100.0);
        }
    }
    #[test]
    fn splitter_double_click_resets() {
        let ctx = egui::Context::default(); let mut size = 180.0;
        let p = pos2(11.0, 50.0);
        let _ = frame(&ctx, &mut size, Axis::Horizontal, vec![egui::Event::PointerMoved(p)], 0.0);
        let _ = frame(&ctx, &mut size, Axis::Horizontal, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE }], 0.01);
        let far = pos2(290.0, 50.0);
        let cursor = frame(&ctx, &mut size, Axis::Horizontal, vec![egui::Event::PointerMoved(far)], 0.05);
        assert_eq!(size, 200.0);
        assert_eq!(cursor, egui::CursorIcon::ResizeHorizontal);
        let _ = frame(&ctx, &mut size, Axis::Horizontal, vec![egui::Event::PointerButton { pos: far, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE }, egui::Event::PointerMoved(p)], 0.1);
        for (time, pressed) in [(0.2, true), (0.25, false), (0.3, true), (0.35, false)] {
            let _ = frame(&ctx, &mut size, Axis::Horizontal, vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }], time);
        }
        assert_eq!(size, 150.0);
    }
    #[test]
    fn scroll_bar_visible_when_overflowing() {
        let ctx = egui::Context::default();
        for both in [false, true] { for overflow in [false, true] { for _ in 0..3 {
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(136.0, 136.0))), ..Default::default() }, |root| { egui::CentralPanel::default().show(root, |ui| {
                let add = |ui: &mut Ui| { ui.allocate_space(if overflow { vec2(if both { 300.0 } else { 10.0 }, 300.0) } else { vec2(10.0, 10.0) }); };
                let out = if both { scroll_both(ui, (both, overflow), add) } else { scroll_v(ui, (both, overflow), add) };
                assert_eq!(out.content_size.y > out.inner_rect.height(), overflow);
            }); });
            out.textures_delta.clear();
            fn thumbs(s: &Shape) -> usize { match s { Shape::Rect(r) => usize::from(r.fill == crate::ui::tokens::DARK.scroll_thumb && (r.rect.width() == 6.0 || r.rect.height() == 6.0)), Shape::Vec(v) => v.iter().map(thumbs).sum(), _ => 0 } }
            let n: usize = out.shapes.iter().map(|s| thumbs(&s.shape)).sum();
            if overflow { assert_eq!(n, if both { 2 } else { 1 }); } else { assert_eq!(n, 0); }
        } } }
    }
}
