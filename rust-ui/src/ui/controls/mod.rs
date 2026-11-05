//! Keyloom controls (design bible §5, §7.2, §11, §12). Built page by page into the app later; for now only the
//! `--specimen-controls` page uses them. Every control paints itself with the tokens (no egui `Button`), sets AccessKit
//! info, and is a fixed-size box (`allocate_exact_size` / explicit rect).
pub mod action_bar;
pub mod buttons;
pub mod nav;
pub mod segmented;
pub mod tabs;
// Foundation batch 2 (one agent per file).
pub mod checkbox;
pub mod chips;
pub mod dialog;
pub mod dropdown;
pub mod menus;
pub mod numeric;
pub mod property;
pub mod scroll;
pub mod table;
pub mod textfield;
// Foundation batch 3 (one agent per file).
pub mod card;
pub mod activity;
pub mod statusbar;
pub mod node;
pub mod transport;
pub mod viewer_toolbar;
pub mod machine_row;

pub use nav::composite_nav;
pub use segmented::{segmented, Seg, SegWidth, Segmented};
pub use tabs::{page_tabs, panel_tabs, stage_tabs, StageStatus, Tab, TabKind, TabStrip};
pub use action_bar::{action_bar, ActionBar, ActionBarOut, BarSlots, Blocker, BusyState};
pub use buttons::{
    destructive_button, destructive_confirm_button, ghost_button, icon_button, look, primary_button, secondary_button, spinner,
    toggle_button, Btn, IconButtonSize, Kind, Look, Vis,
};

use eframe::egui::{self, text::LayoutJob, Align, Color32, FontId, Id, Key, Painter, Rect};
use std::sync::Arc;

/// One-line galley (`max_rows 1`, end "…"), bible line metrics (`type_scale::format`); colour `PLACEHOLDER` takes the
/// colour given at paint time. `max_width` `INFINITY` measures the natural width.
pub(crate) fn line(painter: &Painter, text: &str, font: FontId, line_height: f32, max_width: f32) -> Arc<egui::Galley> {
    let mut job = LayoutJob::default();
    job.wrap.max_rows = 1;
    job.wrap.max_width = max_width;
    job.wrap.overflow_character = Some('…');
    job.wrap.break_anywhere = true;
    job.append(text, 0.0, super::type_scale::format(font, Color32::PLACEHOLDER, line_height));
    painter.layout_job(job)
}

/// Applied by the segmented control and the tabs to their label x: epaint's ¼ px glyph bins round up where Chrome's (the
/// sheet) round to nearest, a mean +0.12 px bias that showed on those labels.
pub(crate) const LABEL_DX: f32 = -0.125;

/// Paint `galley` with its line box (height `line_height`) vertically centred in `rect` ([`line`] galleys are 2 px
/// shorter than their 12 px line box, see `type_scale::format`), at pixel-rounded x per `align`.
pub(crate) fn paint_line(painter: &Painter, rect: Rect, galley: Arc<egui::Galley>, line_height: f32, align: Align, color: Color32, dy: f32) {
    let x = match align {
        Align::Min => rect.min.x,
        Align::Center => rect.center().x - galley.size().x / 2.0,
        Align::Max => rect.max.x - galley.size().x,
    };
    let y = rect.center().y - line_height / 2.0 + dy;
    // The fractional part of x goes into the layout as leading space: epaint bins glyphs at ¼ px offsets there, while
    // a fractional galley position would only resample the glyph bitmaps (blur). The galley itself sits on a whole pixel.
    let (gx, fx) = (x.floor(), x - x.floor());
    // A truncated label keeps its whole-pixel position (the extra width would push its "…" out).
    let galley = if fx > 0.01 && !galley.elided {
        let mut job = (*galley.job).clone();
        if let Some(sec) = job.sections.first_mut() { sec.leading_space += fx }
        job.wrap.max_width = f32::INFINITY;
        painter.layout_job(job)
    } else {
        galley
    };
    painter.galley(egui::pos2(gx, y.round()), galley, color);
}

/// True while the focus on screen came from the keyboard: set by Tab / arrows / Enter / Space, cleared by a pointer
/// press (§12 Focus ring: a mouse click never draws the ring).
pub fn keyboard_focus(ctx: &egui::Context) -> bool {
    let (press, key) = ctx.input(|i| {
        let key = i.events.iter().any(|e| {
            matches!(e, egui::Event::Key { pressed: true, key: Key::Tab | Key::ArrowLeft | Key::ArrowRight | Key::ArrowUp | Key::ArrowDown | Key::Enter | Key::Space, .. })
        });
        (i.pointer.any_pressed(), key)
    });
    ctx.data_mut(|d| {
        let v = d.get_temp_mut_or_insert_with(Id::NULL.with("keyloom-kb-focus"), || false);
        if press { *v = false } else if key { *v = true }
        *v
    })
}
