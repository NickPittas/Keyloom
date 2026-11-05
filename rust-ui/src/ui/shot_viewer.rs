use super::Fixture;
use crate::ui::{
    controls::viewer_toolbar::{self, Layout, ToolbarOut, ViewerState},
    tokens::tokens,
};
use eframe::egui::{pos2, vec2, Color32, Rect, TextureHandle, Ui};

pub(super) fn toolbar(ui: &mut Ui, state: &mut ViewerState, width: f32, format: &str) -> ToolbarOut {
    let layout = if width >= 1600.0 { Layout::Wide } else if width >= 1200.0 { Layout::Medium } else { Layout::Compact };
    viewer_toolbar::viewer_toolbar(ui, state, layout, state.view.has_alpha(), format, false)
}

pub(super) fn picture(ui: &mut Ui, viewer: Rect, picture: Rect, plate: &TextureHandle, state: &ViewerState, fx: &Fixture) {
    let t = tokens(ui);
    let p = ui.painter().with_clip_rect(viewer);
    p.rect_filled(viewer, 0.0, t.viewer_surround);
    if state.view == viewer_toolbar::View::Input {
        p.rect_filled(picture, 0.0, Color32::from_rgb(0x2F, 0x6F, 0x3A));
    } else {
        viewer_toolbar::paint_background(ui, picture, state.background);
    }
    let portrait = Rect::from_min_size(
        pos2(picture.left() + picture.width() * 0.37, picture.top() + picture.height() * 0.14),
        vec2(picture.width() * 0.26, picture.height() * 0.86),
    );
    p.image(plate.id(), portrait, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    let sample = if fx.channel == "A" { [0.0, 0.0, 0.0, 0.72] }
        else if fx.viewer_state == "input" || fx.viewer_state == "input-g" { [0.184, 0.435, 0.227, 1.0] }
        else { fx.pixel_rgba };
    viewer_toolbar::pixel_readout(ui, viewer, picture, Some(viewer.center()), sample, fx.pixel_xy, ui.ctx().content_rect().width() < 1200.0);
}

