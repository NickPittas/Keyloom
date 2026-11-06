use super::{part, Fixture};
use crate::ui::{controls::{self, transport::{Action, Layout, TimelineData, TransportState}}, icons::{self, Icon, IconSize}, tokens::tokens, type_scale as ty};
use eframe::egui::{pos2, vec2, Rect, Ui};

pub(super) fn draw(ui: &mut Ui, transport_rect: Rect, timeline_rect: Rect, fx: &Fixture) -> Vec<Action> {
    let mut state = TransportState {
        frame: fx.frame,
        first: fx.first,
        last: fx.last,
        playing: fx.playing,
        reverse: fx.reverse,
        loop_on: fx.loop_on,
        in_pt: fx.in_point,
        out_pt: fx.out_point,
        marks: fx.marks.clone(),
        ..Default::default()
    };
    let window_width = ui.ctx().content_rect().width();
    let layout = if window_width < 1200.0 { Layout::Compact } else { Layout::Wide };
    let mut actions = Vec::new();
    if fx.live {
        meta(ui, transport_rect, fx, layout);
    }
    let mut controls_ui = part(ui, transport_rect, "transport");
    if let Some(a) = controls::transport::transport(&mut controls_ui, &mut state, layout).action {
        actions.push(a);
    }
    if fx.live {
        return actions;
    }
    let cached = fx.cached.as_slice();
    let data = TimelineData {
        first: fx.first,
        last: fx.last,
        frame: fx.frame,
        in_pt: fx.in_point,
        out_pt: fx.out_point,
        marks: &fx.marks,
        cached,
        fps: fx.fps,
    };
    meta(ui, transport_rect, fx, layout);
    let mut timeline_ui = part(ui, timeline_rect, "timeline");
    if let Some(f) = controls::transport::timeline(&mut timeline_ui, &data).scrub_to {
        actions.push(Action::Seek(f));
    }
    actions
}

// The Shot meta line overlays the 28 px transport; the shared helper paints a separate full-width band.
fn meta(ui: &Ui, rect: Rect, fx: &Fixture, layout: Layout) {
    let compact = layout == Layout::Compact;
    let frame_count = fx.frame_count.unwrap_or(fx.last - fx.first + 1);
    let text = if compact && fx.drawer {
        format!("{}–{} · {}f · {}fps · {}", fx.first, fx.last, frame_count, fps(fx.fps), fx.marks.len())
    } else if compact {
        format!("In {} · Out {} · {} fr · {} fps · {} marks", fx.in_point.unwrap_or(fx.first), fx.out_point.unwrap_or(fx.last), frame_count, fps(fx.fps), fx.marks.len())
    } else {
        let frames = if ui.ctx().content_rect().width() < 1600.0 { "fr" } else { "frames" };
        format!("Frame {} · In {} · Out {} · {} {} · {} fps · {} marks", fx.frame, fx.in_point.unwrap_or(fx.first), fx.out_point.unwrap_or(fx.last), frame_count, frames, fps(fx.fps), fx.marks.len())
    };
    let t = tokens(ui);
    let g = ui.painter().layout_no_wrap(text, ty::mono(), t.text_tertiary);
    // Mockup §8.10: text starts 88 px in (8 px at Compact) and is clipped before the centred group (half width less 96/98/134).
    let (x, room) = if compact {
        (rect.left() + 8.0, rect.width() / 2.0 - 134.0)
    } else {
        (rect.left() + 88.0, rect.width() / 2.0 - if ui.ctx().content_rect().width() < 1600.0 { 98.0 } else { 96.0 })
    };
    let y = rect.top() + 7.0;
    let total = ui.painter().layout_no_wrap(format!("/ {}", fx.last), ty::small(), t.text_primary);
    let cx = (rect.center().x - (142.0 + 10.0 + 64.0 + 6.0 + total.size().x) / 2.0).round();
    let room = room.min(cx - 8.0 - x);
    if room <= 0.0 {
        return;
    }
    let clip = Rect::from_min_size(pos2(x, y), vec2(room, 14.0));
    ui.painter().with_clip_rect(clip).rect_filled(clip, 0.0, t.bg_panel);
    ui.painter().with_clip_rect(clip).galley(pos2(x, y), g.clone(), t.text_tertiary);
    if compact && fx.drawer {
        icons::paint(ui.painter(), Rect::from_min_size(pos2(x + g.size().x + 4.0, y - 1.0), vec2(16.0, 16.0)), Icon::Flag, IconSize::S16, t.text_tertiary);
    }
}

fn fps(fps: f64) -> String {
    if fps.fract() == 0.0 { format!("{fps:.0}") } else { fps.to_string() }
}
