//! Hidden `--specimen` page (dev/QA): every colour token of the current theme and every type role, drawn with the
//! embedded fonts, to compare against `bible/specimen@1920.png`. No project needed.
use super::controls::{self, action_bar::action_bar_in, ActionBar, Blocker, BusyState, IconButtonSize, Vis};
use super::icons::{self, Icon, IconSize};
use super::tokens::{tokens, Tokens};
use super::type_scale as ty;
use eframe::egui::{self, Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Ui};

/// `#RRGGBB`, or `#RRGGBB @ NN%` for the alpha tokens.
fn hex(c: Color32) -> String {
    let [r, g, b, a] = c.to_srgba_unmultiplied();
    if a == 255 { format!("#{r:02X}{g:02X}{b:02X}") } else { format!("#{r:02X}{g:02X}{b:02X} @ {}%", (a as f32 / 255.0 * 100.0).round()) }
}

fn groups(t: &Tokens) -> Vec<(&'static str, Vec<(&'static str, Color32)>)> {
    vec![
        ("Surfaces", vec![
            ("bg.app", t.bg_app), ("bg.panel", t.bg_panel), ("bg.raised", t.bg_raised), ("bg.hover", t.bg_hover),
            ("bg.pressed", t.bg_pressed), ("bg.menu-hover", t.bg_menu_hover), ("bg.sunken", t.bg_sunken),
            ("bg.overlay", t.bg_overlay), ("bg.selected", t.bg_selected), ("viewer.surround", t.viewer_surround),
            ("graph.canvas", t.graph_canvas), ("graph.grid", t.graph_grid), ("scrim", t.scrim),
        ]),
        ("Borders and text", vec![
            ("border.divider", t.border_divider), ("border.default", t.border_default), ("border.control", t.border_control),
            ("border.hover", t.border_hover), ("border.overlay", t.border_overlay), ("text.primary", t.text_primary),
            ("text.secondary", t.text_secondary), ("text.tertiary", t.text_tertiary), ("text.disabled", t.text_disabled),
            ("text.link", t.text_link), ("text.on-accent", t.text_on_accent), ("text.on-danger", t.text_on_danger),
        ]),
        ("Accent, focus and status", vec![
            ("accent", t.accent), ("accent.hover", t.accent_hover), ("accent.pressed", t.accent_pressed),
            ("accent.bright", t.accent_bright), ("accent.tint", t.accent_tint), ("busy.fill", t.busy_fill),
            ("busy.queued", t.busy_queued), ("focus.ring", t.focus_ring), ("text.selection", t.text_selection),
            ("success", t.success), ("success.tint", t.success_tint), ("warning", t.warning), ("warning.tint", t.warning_tint),
            ("error", t.error), ("error.tint", t.error_tint), ("info", t.info), ("info.tint", t.info_tint),
            ("danger.fill", t.danger_fill), ("danger.hover", t.danger_fill_hover), ("danger.pressed", t.danger_fill_pressed),
        ]),
        ("Stage, graph and scrollbar", vec![
            ("raw.chip.fill", t.stage_raw_chip_fill), ("raw.chip.text", t.stage_raw_chip_text), ("raw.bar", t.stage_raw_bar),
            ("mask.chip.fill", t.stage_mask_chip_fill), ("mask.chip.text", t.stage_mask_chip_text), ("mask.bar", t.stage_mask_bar),
            ("ready.chip.fill", t.stage_ready_chip_fill), ("ready.chip.text", t.stage_ready_chip_text), ("ready.bar", t.stage_ready_bar),
            ("done.chip.fill", t.stage_completed_chip_fill), ("done.chip.text", t.stage_completed_chip_text), ("done.bar", t.stage_completed_bar),
            ("failed.chip.fill", t.stage_failed_chip_fill), ("failed.chip.text", t.stage_failed_chip_text), ("failed.bar", t.stage_failed_bar),
            ("badge.queued.text", t.badge_queued_text), ("badge.render.text", t.badge_rendering_text),
            ("node.source", t.node_kind_source), ("node.mask", t.node_kind_mask), ("node.key", t.node_kind_key),
            ("node.output", t.node_kind_output), ("node.fill", t.node_fill), ("node.border", t.node_border),
            ("node.selected", t.node_selected), ("wire.image", t.wire_image), ("wire.matte", t.wire_matte),
            ("scroll.thumb", t.scroll_thumb), ("scroll.thumb-hover", t.scroll_thumb_hover), ("scroll.thumb-drag", t.scroll_thumb_drag),
        ]),
    ]
}

fn text(ui: &Ui, pos: egui::Pos2, s: &str, font: FontId, c: Color32) {
    ui.painter().text(pos, Align2::LEFT_TOP, s, font, c);
}

fn colour(ui: &mut Ui, t: &Tokens) {
    text_line(ui, "2 · Colour", ty::title(), t.text_primary);
    for (name, swatches) in groups(t) {
        ui.add_space(4.0);
        text_line(ui, name, ty::section_header(), t.text_secondary);
        ui.add_space(2.0);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, 6.0);
            for (n, c) in swatches {
                let (cell, _) = ui.allocate_exact_size(egui::vec2(120.0, 40.0 + 4.0 + 16.0 + 14.0), Sense::hover());
                let sw = Rect::from_min_size(cell.min, egui::vec2(56.0, 40.0));
                ui.painter().rect_filled(sw, 0.0, c);
                ui.painter().rect_stroke(sw, 0.0, Stroke::new(1.0, t.border_default), StrokeKind::Outside);
                text(ui, cell.min + egui::vec2(0.0, 44.0), n, ty::label(), t.text_primary);
                // Color32 is premultiplied, so text.selection's RGB is shown from `accent` (the bible's base colour).
                let shown = if n == "text.selection" { hex(t.accent) + &hex(c)[7..] } else { hex(c) };
                text(ui, cell.min + egui::vec2(0.0, 60.0), &shown, ty::mono(), t.text_tertiary);
            }
        });
    }
}

fn text_line(ui: &mut Ui, s: &str, font: FontId, c: Color32) {
    let g = ui.painter().layout_no_wrap(s.to_owned(), font, c);
    let (r, _) = ui.allocate_exact_size(g.size(), Sense::hover());
    ui.painter().galley(r.min, g, c);
}

fn typography(ui: &mut Ui, t: &Tokens) {
    text_line(ui, "3 · Typography", ty::title(), t.text_primary);
    // 1:1 with the reference "3 · Typography" block: columns at x = 49 / 265 / 715, sample tops at fixed offsets from y0.
    let y0 = 740.0;
    // (role, sample parts, colour, line height, label dy, sample dy, note)
    let (p, s2) = (t.text_primary, t.text_secondary);
    #[allow(clippy::type_complexity)]
    let rows: [(&str, Vec<(&str, FontId)>, Color32, f32, f32, f32, &str); 10] = [
        ("Display 22/28 SemiBold", vec![("Open or create a project", ty::display())], p, 28.0, 11.0, 0.0, "Start screen only"),
        ("Title 15/20 SemiBold", vec![("Remove shot", ty::title())], p, 20.0, 36.0, 32.0, "Page and dialog titles"),
        ("Shot name 13/18 SemiBold", vec![("V1-0062_A002C002_261012_R4LF", ty::shot_name())], p, 18.0, 58.0, 56.0, "Shot header"),
        ("Panel header 12/16 SemiBold", vec![("Properties", ty::panel_header())], p, 16.0, 78.0, 78.0, "Panel title bars"),
        ("Section 12/16 Medium", vec![("Key engine", ty::section_header())], p, 16.0, 98.0, 98.0, "Property-section headers"),
        ("Label 12/16 Regular", vec![("Temporal", ty::label())], s2, 16.0, 118.0, 118.0, "Property labels (secondary)"),
        ("Value / button 12/16", vec![("Auto · ", ty::value()), ("Key shot", ty::button())], p, 16.0, 138.0, 138.0, "Values Regular, buttons and tabs Medium"),
        ("Small / meta 11/14", vec![("3200×1800 · 24 fps · 65 fr", ty::small())], s2, 14.0, 158.0, 159.0, "Meta lines, chips, tooltips"),
        ("Mono 11/14", vec![("…/clips/V1-0062/Output · 0f002c3a…7268d90b", ty::mono())], s2, 14.0, 178.0, 179.0, "Paths, ids, numbers"),
        ("Mono frame 12/16", vec![("0012 / 64", ty::frame_field())], p, 14.0, 198.0, 199.0, "Frame field, timecode"),
    ];
    for (role, parts, c, lh, ldy, sdy, note) in rows {
        text(ui, egui::pos2(49.0, y0 + ldy), role, ty::small(), t.text_tertiary);
        let mut job = egui::text::LayoutJob::default();
        for (s, f) in parts { job.append(s, 0.0, ty::format(f, c, lh)); }
        let g = ui.painter().layout_job(job);
        ui.painter().galley(egui::pos2(265.0, y0 + sdy), g, c);
        text(ui, egui::pos2(715.0, y0 + ldy), note, ty::small(), t.text_tertiary);
    }
}

/// Set by `--specimen-icons`: show the "9 · Icons" sheet instead of the colour/type page (it lies far below them in the reference).
pub static ICONS_PAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Set by `--specimen-controls`: show the "5.1 · Buttons" sheet.
pub static CONTROLS_PAGE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
/// `--specimen-part NAME`: show only `specimen_parts::NAME` (foundation batch 2).
pub static PART: std::sync::OnceLock<String> = std::sync::OnceLock::new();

/// Chrome's layout snapping: a box edge lands on the nearest pixel.
fn snap(v: f32) -> f32 {
    (v + 0.5).floor()
}

fn snapped(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect::from_min_max(egui::pos2(snap(x), snap(y)), egui::pos2(snap(x + w), snap(y + h)))
}

fn centered(ui: &Ui, cx: f32, y: f32, s: &str, font: FontId, c: Color32, lh: f32) {
    let mut job = egui::text::LayoutJob::default();
    job.append(s, 0.0, ty::format(font, c, lh));
    let g = ui.painter().layout_job(job);
    ui.painter().galley(egui::pos2((cx - g.size().x / 2.0).round(), y), g, c);
}

/// 1:1 copy of the reference "9 · Icons" sheet (bible specimen.html y 10873..11495), shifted up so it fits a 1080 window.
fn icons_sheet(ui: &mut Ui, t: &Tokens) {
    let dy = 16.0 - 10873.0;
    let sheet = snapped(32.0, 10873.0 + dy, 1856.0, 622.0);
    ui.painter().rect_filled(sheet, 4.0, t.bg_panel);
    ui.painter().rect_stroke(sheet, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let title = ui.painter().layout_no_wrap("9 · Icons".into(), ty::title(), t.text_primary);
    let tw = title.size().x;
    ui.painter().galley(egui::pos2(49.0, 10890.0 + dy), title, t.text_primary);
    text(ui, egui::pos2(49.0 + tw + 12.0, 10890.0 + dy + 4.0), "1 px stroke, pixel-centred, square ends at scale 1.0 (1.5 px at ≥ 1.5) · 16 px grid (20 px for page tabs) · drawable with egui Painter · shown at 12 · 14 · 16 · 20 (12 and 14 are redrawn, not scaled, §9) · zoom-1to1 and channels retired (text \"1:1\" and \"RGB\")", ty::small(), t.text_tertiary);
    // Main grid: 12 columns of 144.5, gap 8, cells 64 high.
    let cells: Vec<Icon> = Icon::ALL.into_iter().filter(|&i| icons::drawing(i, 16).is_some()).collect();
    for (n, &icon) in cells.iter().enumerate() {
        let (col, row) = ((n % 12) as f32, (n / 12) as f32);
        let (cx, cy) = (49.0 + col * 152.5, 10922.0 + dy + row * 72.0);
        let cell = snapped(cx, cy, 144.5, 64.0);
        ui.painter().rect_filled(cell, 4.0, t.bg_raised);
        ui.painter().rect_stroke(cell, 4.0, Stroke::new(1.0, t.border_default), StrokeKind::Inside);
        let have = |g: u8| icons::drawing(icon, g).is_some();
        let sizes = [(IconSize::S12, have(12)), (IconSize::S14, have(14)), (IconSize::S16, true), (IconSize::S20, true)];
        let width: f32 = sizes.iter().map(|&(s, h)| if h { s.px() } else { 0.0 }).sum::<f32>() + 18.0;
        let mut x = cx + (144.5 - width) / 2.0;
        for (s, h) in sizes {
            if h {
                let r = Rect::from_min_size(egui::pos2(snap(x), snap(cy + 11.0 + (20.0 - s.px()) / 2.0)), egui::vec2(s.px(), s.px()));
                icons::paint_bg(ui.painter(), r, icon, s, t.text_primary, t.bg_raised);
                x += s.px();
            }
            x += 6.0;
        }
        centered(ui, cx + 144.5 / 2.0, cy + 37.0 + 1.0, icon.name(), ty::small(), t.text_tertiary, 16.0);
    }
    // Small rows (parts/icons-small.html): 19 columns of 92.105, gap 4, cells 60 high.
    use Icon as I;
    use IconSize::{S12, S14};
    let (info, warn, err, ok) = (t.info, t.warning, t.error, t.success);
    let s2 = t.text_secondary;
    #[allow(clippy::type_complexity)]
    let rows: [(usize, &str, Vec<(Icon, IconSize, Color32)>); 8] = [
        (1, "chevrons · check 12", vec![(I::ChevronDown, S12, s2), (I::ChevronRight, S12, s2), (I::Check, S12, s2)]),
        (1, "sort · close · spinner 12", vec![(I::SortUp, S12, s2), (I::SortDown, S12, s2), (I::Close, S12, s2), (I::Spinner, S12, s2)]),
        (1, "status 12", vec![(I::Info, S12, info), (I::Warning, S12, warn), (I::Error, S12, err), (I::Success, S12, ok)]),
        (1, "status 14", vec![(I::Info, S14, info), (I::Warning, S14, warn), (I::Error, S14, err), (I::Success, S14, ok)]),
        (2, "node headers 14", vec![(I::Film, S14, s2), (I::Gvm, S14, s2), (I::Hint, S14, s2), (I::Key, S14, s2), (I::Deliver, S14, s2), (I::Pen, S14, s2), (I::Sam, S14, s2)]),
        (1, "stage chips 12", vec![(I::CircleEmpty, S12, s2), (I::CheckCircle, S12, s2), (I::Clock, S12, s2)]),
        (2, "leading · path 14", vec![(I::Machine, S14, s2), (I::MachineNetwork, S14, s2), (I::Gpu, S14, s2), (I::Folder, S14, s2), (I::Copy, S14, s2), (I::Reveal, S14, s2)]),
        (1, "small buttons 14", vec![(I::More, S14, s2), (I::Close, S14, s2), (I::Cancel, S14, s2), (I::Retry, S14, s2), (I::Refresh, S14, s2)]),
    ];
    let colw = (1822.0 - 18.0 * 4.0) / 19.0;
    let (y0, mut col) = (11418.0 + dy, 0.0);
    for (span, label, items) in rows {
        let (x, w) = (49.0 + col * (colw + 4.0), colw * span as f32 + 4.0 * (span as f32 - 1.0));
        col += span as f32;
        let cell = snapped(x, y0, w, 60.0);
        ui.painter().rect_filled(cell, 0.0, t.bg_panel);
        ui.painter().rect_stroke(cell, 0.0, Stroke::new(1.0, Color32::from_rgb(0x2a, 0x2a, 0x2a)), StrokeKind::Inside);
        let rw: f32 = items.iter().map(|i| i.1.px()).sum::<f32>() + 6.0 * (items.len() as f32 - 1.0);
        let rh = items.iter().map(|i| i.1.px()).fold(0.0, f32::max);
        let mut ix = x + (w - rw) / 2.0;
        for (icon, s, c) in &items {
            let r = Rect::from_min_size(egui::pos2(snap(ix), snap(y0 + 7.0 + (rh - s.px()) / 2.0)), egui::vec2(s.px(), s.px()));
            icons::paint_bg(ui.painter(), r, *icon, *s, *c, t.bg_panel);
            ix += s.px() + 6.0;
        }
        let ly = y0 + 7.0 + rh + 4.0;
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = w - 2.0;
        job.append(label, 0.0, ty::format(ty::mono(), s2, 14.0));
        let g = ui.painter().layout_job(job);
        let lx = if g.rows.len() > 1 { x + 1.0 } else { x + (w - g.size().x) / 2.0 };
        ui.painter().galley(egui::pos2(lx.round(), ly.round()), g, s2);
    }
}

/// Wrapped text at a fixed top-left (line height `lh`, width `w`).
fn para(ui: &Ui, pos: egui::Pos2, s: &str, font: FontId, c: Color32, lh: f32, w: f32) {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = w;
    job.append(s, 0.0, ty::format(font, c, lh));
    let g = ui.painter().layout_job(job);
    ui.painter().galley(pos.round(), g, c);
}

/// 1:1 copy of the reference "5.1 · Buttons" sheet and its busy group (bible specimen.html y 1504..2060), shifted up so it
/// fits a 1080 window. Hover / pressed / focus are forced through the same painting path as the real states.
fn buttons_sheet(ui: &mut Ui, t: &Tokens) {
    use controls::{destructive_button, destructive_confirm_button, ghost_button, icon_button, primary_button, secondary_button, toggle_button};
    let dy = 16.0 - 1504.0;
    let pt = |x: f32, y: f32| egui::pos2(x, y + dy);
    let sheet = snapped(32.0, 1504.0 + dy, 1856.0, 556.0);
    ui.painter().rect_filled(sheet, 4.0, t.bg_panel);
    ui.painter().rect_stroke(sheet, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    let title = ui.painter().layout_no_wrap("5.1 · Buttons".into(), ty::title(), t.text_primary);
    let tw = title.size().x;
    ui.painter().galley(pt(49.0, 1521.0), title, t.text_primary);
    text(ui, pt(49.0 + tw + 12.0, 1525.0), "rest · hover · pressed · focus · disabled (with reason tooltip) · busy", ty::small(), t.text_tertiary);
    let cols = [207.0, 385.0, 563.0, 741.0, 919.0, 1097.0];
    for (i, name) in ["Rest", "Hover", "Pressed", "Focus (keyboard)", "Disabled", "On (toggles)"].into_iter().enumerate() {
        let w = if i == 5 { 240.0 } else { 170.0 };
        centered(ui, cols[i] + w / 2.0, 1557.0 + dy, name, ty::small(), t.text_tertiary, 16.0);
    }
    let states = [(Vis::Rest, false), (Vis::Hover, false), (Vis::Pressed, false), (Vis::Rest, true)];
    // (label, y) of each row; the Primary / Secondary / Ghost / Destructive rows share the state loop.
    for (label, y) in [("Primary (one per area)", 1586.0), ("Secondary", 1618.0), ("Ghost (toolbars)", 1650.0), ("Destructive, outside confirm", 1679.0), ("Destructive, in dialog", 1726.0), ("Icon 22 / small 18", 1760.0), ("Toggle (Loop, wipe, Focus)", 1804.0)] {
        para(ui, pt(49.0, y), label, ty::label(), t.text_secondary, 16.0, 150.0);
    }
    for (i, &(vis, focus)) in states.iter().enumerate() {
        let x = cols[i];
        ui.add(primary_button("Key shot").forced(vis, focus).at(pt(x, 1583.0)));
        ui.add(secondary_button("Open").forced(vis, focus).at(pt(x, 1615.0)));
        ui.add(ghost_button("Details").forced(vis, focus).at(pt(x, 1647.0)));
        ui.add(destructive_button("Delete frames…").forced(vis, focus).at(pt(x, 1684.0)));
        ui.add(destructive_confirm_button("Remove shot").action_height().forced(vis, focus).at(pt(x, 1721.0)));
        ui.add(icon_button(Icon::More, IconButtonSize::Normal22, "More actions").forced(vis, focus).at(pt(x, 1757.0)));
        if !focus { ui.add(icon_button(Icon::Copy, IconButtonSize::Small18, "Copy").forced(vis, false).at(pt(x + 30.0, 1759.0))); }
    }
    let x = cols[4];
    ui.add(primary_button("Key shot").disabled("Why this is disabled").at(pt(x, 1583.0)));
    ui.add(secondary_button("Apply").disabled("Why this is disabled").at(pt(x, 1615.0)));
    ui.add(ghost_button("Details").disabled("Why this is disabled").at(pt(x, 1647.0)));
    ui.add(destructive_button("Delete frames…").disabled("Why this is disabled").at(pt(x, 1684.0)));
    ui.add(destructive_confirm_button("Remove shot").action_height().disabled("Why this is disabled").at(pt(x, 1721.0)));
    ui.add(icon_button(Icon::Reveal, IconButtonSize::Normal22, "Reveal in folder").disabled("Why this is disabled").at(pt(x, 1757.0)));
    // Toggles: off rest / hover / pressed, on + focus, disabled, then the "On" column (on, on + hover).
    let (mut off, mut on, mut dis) = (false, true, false);
    for (i, (vis, icon, label)) in [(Vis::Rest, Icon::Loop, "Loop"), (Vis::Hover, Icon::Loop, "Loop"), (Vis::Pressed, Icon::Loop, "Loop")].into_iter().enumerate() {
        ui.add(toggle_button(icon, Some(label), &mut off, "Loop playback").forced(vis, false).at(pt(cols[i], 1801.0)));
    }
    ui.add(toggle_button(Icon::Focus, Some("Focus"), &mut on, "Focus mode").forced(Vis::Rest, true).at(pt(cols[3], 1801.0)));
    ui.add(toggle_button(Icon::Wipe, Some("A/B wipe"), &mut dis, "A/B wipe").disabled("Why this is disabled").at(pt(cols[4], 1801.0)));
    text(ui, pt(283.52, 1804.0 + 1.0), "off", ty::small(), t.text_tertiary);
    ui.add(toggle_button(Icon::Loop, Some("Loop"), &mut on, "Loop playback").forced(Vis::Rest, false).at(pt(1097.0, 1789.0)));
    // The sheet puts this one at the fractional x 1173.52 (width 90.7); the label then lands on a fractional x too.
    ui.add(toggle_button(Icon::Wipe, Some("A/B wipe"), &mut on, "A/B wipe").forced(Vis::Hover, false).in_rect(Rect::from_min_size(pt(1173.52, 1789.0), egui::vec2(90.7, 22.0))));
    text(ui, pt(1097.0, 1819.0 + 1.0), "on · on+hover", ty::small(), t.text_tertiary);

    // Busy group: Rest / Blocked / Busy action bars, 280 wide, 72 high, 1 px outline as in the sheet.
    let bars = [(49.0, "Rest (secondary empty)"), (353.0, "Blocked (fix button in the blocker line; secondary empty)"), (667.0, "Busy (Cancel takes the secondary slot)")];
    for (i, (x, cap)) in bars.into_iter().enumerate() {
        text(ui, pt(x, 1851.0 + 1.0), cap, ty::small(), t.text_tertiary);
        let r = Rect::from_min_size(pt(x, 1871.0), egui::vec2(280.0, 72.0));
        let bar = ActionBar {
            more_menu: Some(Box::new(|ui: &mut Ui| { ui.label("…"); })),
            secondary: None,
            primary: controls::primary_button("Key shot"),
            blocker: (i == 1).then_some(Blocker { text: "Can't key: endor-omarchy is offline.", fix_button: Some("Assign machine") }),
            status_text: match i { 0 => Some("Last keyed 14:02 on endor-omarchy"), 2 => Some("Keying 34% · frame 22 / 65"), _ => None },
            busy: (i == 2).then_some(BusyState { progress_text: "Keying 34%", cancel: "Cancel", progress: Some(0.34) }),
            reduced_motion: true,
        };
        action_bar_in(ui, r, bar);
        // The 1 px frame lies outside the 280 box (264 inside the 8 px padding).
        ui.painter().rect_stroke(r, 0.0, Stroke::new(1.0, t.border_divider), StrokeKind::Outside);
    }
    para(ui, pt(49.0, 1949.0), "All three: 280 wide (264 inside) · 72 high · … 26 · 8 · secondary 110, empty at rest/blocked (Cancel min 64) · 8 · primary 112. Same row, same x positions; Cancel never wraps.", ty::small(), t.text_tertiary, 14.0, 1822.0);
    for (i, ic) in [Icon::Import, Icon::Assign, Icon::ClearFinished, Icon::Edit, Icon::Retry, Icon::Cancel, Icon::Dismiss].into_iter().enumerate() {
        let b = icon_button(ic, IconButtonSize::Normal22, ic.name()).at(pt(287.0 + 24.0 * i as f32, 1975.0));
        ui.add(if i == 1 { b.forced(Vis::Hover, false) } else { b });
    }
    para(ui, pt(633.33, 2002.0), "Order inside a group: secondary … Cancel · Primary (rightmost). Disabled primary is never blue and never ringed.", ty::small(), t.text_tertiary, 14.0, 1200.0);
}


/// 1:1 copy of the reference "5.3–5.4 · Segmented control and tabs" sheet (bible specimen.html y 2076..2820, left column),
/// drawn at the same x and 1488 px higher, directly below the buttons sheet (hover / pressed / focus forced).
fn segtabs_sheet(ui: &mut Ui, t: &Tokens) {
    use controls::{icon_button, Seg, SegWidth, Segmented, StageStatus, Tab, TabKind, TabStrip};
    let dy = 588.0 - 2076.0;
    let pt = |x: f32, y: f32| egui::pos2(x, y + dy);
    let sheet = snapped(32.0, 2076.0 + dy, 920.0, 744.0);
    ui.painter().rect_filled(sheet, 4.0, t.bg_panel);
    ui.painter().rect_stroke(sheet, 4.0, Stroke::new(1.0, t.border_divider), StrokeKind::Inside);
    ui.painter().galley(pt(49.0, 2093.0), ui.painter().layout_no_wrap("5.3–5.4 · Segmented control and tabs".into(), ty::title(), t.text_primary), t.text_primary);
    for (y, s) in [
        (2125.0, "Segmented (2–5 short options): rest · hover segment · selected · disabled · focus"),
        (2213.0, "Page tabs (toolbar 36): rest · hover · active · disabled"),
        (2285.0, "Stage tabs (shot header 32): rest · hover · active · status dots"),
        (2353.0, "Panel tabs (browser-like, 24): active · rest · hover · disabled · tools at the right"),
    ] {
        para(ui, pt(49.0, y), s, ty::section_header(), t.text_secondary, 16.0, 886.0);
    }
    // Segmented controls.
    let overlay = [Seg::new(0usize, "Off"), Seg::new(1, "Pre-matte"), Seg::new(2, "Hint"), Seg::new(3, "SAM")];
    let (mut v1, mut v2) = (0usize, 0usize);
    text(ui, pt(49.0, 2152.0), "Overlay", ty::small(), t.text_tertiary);
    Segmented::new(&mut v1, &overlay).name("Overlay").at(pt(92.55, 2149.0)).forced(vec![Vis::Rest, Vis::Hover, Vis::Pressed, Vis::Rest], None).show(ui);
    text(ui, pt(299.56, 2152.0), "hover · pressed", ty::small(), t.text_tertiary);
    text(ui, pt(394.61, 2152.0), "Overlay", ty::small(), t.text_tertiary);
    Segmented::new(&mut v2, &overlay).name("Overlay focus").at(pt(438.16, 2149.0)).forced(vec![], Some(0)).show(ui);
    text(ui, pt(645.17, 2152.0), "focus", ty::small(), t.text_tertiary);
    let deliver = [Seg::new(0usize, "Sequence"), Seg::new(1, "Video file")];
    let fmt = [Seg::new(0usize, "MP4"), Seg::new(1, "MOV"), Seg::new(2, "WebM")];
    let (mut v3, mut v4, mut v5) = (0usize, 1usize, 1usize);
    Segmented::new(&mut v3, &deliver).name("Deliver as").at(pt(49.0, 2179.0)).width(SegWidth::Natural).show(ui);
    Segmented::new(&mut v4, &fmt).name("File type").at(pt(213.70, 2179.0)).show(ui);
    Segmented::new(&mut v5, &fmt).name("File type disabled").at(pt(377.0, 2179.0)).disabled("Why this is disabled").show(ui);
    text(ui, pt(540.30, 2182.0), "disabled", ty::small(), t.text_tertiary);

    // Page tabs on a 36 px strip (bg.app, 1 px divider underneath), ghost buttons at the right.
    ui.painter().rect_filled(snapped(49.0, 2237.0 + dy, 886.0, 36.0), 0.0, t.bg_app);
    ui.painter().rect_filled(snapped(49.0, 2272.0 + dy, 886.0, 1.0), 0.0, t.border_divider);
    let pages = [Tab::new("Board").icon(Icon::Board), Tab::new("Shot").icon(Icon::Shot).disabled("Open a shot from the Board"), Tab::new("Machines").icon(Icon::Machines),
                 Tab::new("Models").icon(Icon::Models), Tab::new("Settings").icon(Icon::Settings)];
    let mut page = 0usize;
    TabStrip::new(TabKind::Page, &mut page, &pages).name("Pages").at(pt(57.0, 2244.0))
        .forced(vec![Vis::Rest, Vis::Rest, Vis::Hover, Vis::Rest, Vis::Rest], None).show(ui);
    ui.add(controls::ghost_button("Models").with_icon(Icon::Models).at(pt(703.56, 2246.0)));
    {
        let g = controls::line(ui.painter(), "2 models missing", ty::button(), 16.0, f32::INFINITY);
        let r = Rect::from_min_size(pt(797.31, 2246.0), egui::vec2(129.69, 22.0));
        ui.painter().rect_filled(Rect::from_center_size(egui::pos2(806.0 + 3.0, r.center().y), egui::vec2(6.0, 6.0)), 3.0, t.error);
        controls::paint_line(ui.painter(), Rect::from_min_max(egui::pos2(818.31, r.min.y), r.max), g, 16.0, egui::Align::Min, t.error, 0.0);
    }

    // Stage tabs on a 32 px strip (bg.panel, divider underneath).
    ui.painter().rect_filled(snapped(49.0, 2309.0 + dy, 886.0, 32.0), 0.0, t.bg_panel);
    ui.painter().rect_filled(snapped(49.0, 2340.0 + dy, 886.0, 1.0), 0.0, t.border_divider);
    let stages = [Tab::new("Media"), Tab::new("Hint").status(StageStatus::Blocked), Tab::new("Key"), Tab::new("Review"), Tab::new("Deliver").status(StageStatus::OutOfDate)];
    let mut stage = 2usize;
    TabStrip::new(TabKind::Stage, &mut stage, &stages).name("Stages").at(pt(57.0, 2312.0))
        .forced(vec![Vis::Rest, Vis::Hover, Vis::Rest, Vis::Rest, Vis::Rest], None).show(ui);

    // Panel tabs on a 24 px strip (bg.app) joined to the panel body.
    ui.painter().rect_filled(snapped(49.0, 2377.0 + dy, 886.0, 24.0), 0.0, t.bg_app);
    let body = snapped(49.0, 2401.0 + dy, 886.0, 24.0);
    ui.painter().rect_filled(body, 0.0, t.bg_panel);
    for r in [(0.0, 0.0, 1.0, 24.0), (885.0, 0.0, 1.0, 24.0), (0.0, 23.0, 886.0, 1.0)] {
        ui.painter().rect_filled(snapped(49.0 + r.0, 2401.0 + dy + r.1, r.2, r.3), 0.0, t.border_divider);
    }
    text(ui, pt(50.0, 2400.0), "\u{a0} panel body joins the active tab", ty::small(), t.text_tertiary);
    ui.painter().rect_filled(snapped(49.0 + 89.28, 2400.0 + dy, 886.0 - 89.28, 1.0), 0.0, t.border_divider);
    let panels = [Tab::new("Actions").icon(Icon::Actions), Tab::new("Node graph").icon(Icon::Node), Tab::new("Activity"), Tab::new("Waiting").disabled("Nothing is waiting").badge(0)];
    let mut panel = 0usize;
    TabStrip::new(TabKind::Panel, &mut panel, &panels).name("Dock").at(pt(49.0, 2377.0))
        .forced(vec![Vis::Rest, Vis::Rest, Vis::Hover, Vis::Rest], None).show(ui);
    for (x, ic, tip) in [(799.0, Icon::Fit, "Fit"), (823.0, Icon::Minus, "Zoom out"), (885.0, Icon::Plus, "Zoom in"), (909.0, Icon::PanelBottom, "Dock position")] {
        ui.add(icon_button(ic, IconButtonSize::Normal22, tip).at(pt(x, 2378.0)));
    }
    centered(ui, 865.0, 2382.0 + dy, "85%", ty::mono(), t.text_secondary, 14.0);
}

/// One frame of the specimen page; writes the PNG and exits once `--screenshot` is due.
pub fn frame(ui: &mut Ui, shot: Option<&std::path::Path>, after: Option<f32>, started: std::time::Instant, requested: &mut bool) {
    let t = tokens(ui);
    if let Some(part) = PART.get() {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.bg_app)).show(ui, |ui| { super::specimen_parts::show(part, ui); });
    } else if CONTROLS_PAGE.load(std::sync::atomic::Ordering::Relaxed) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.bg_app)).show(ui, |ui| { buttons_sheet(ui, t); segtabs_sheet(ui, t); });
    } else if ICONS_PAGE.load(std::sync::atomic::Ordering::Relaxed) {
        egui::CentralPanel::default().frame(egui::Frame::new().fill(t.bg_app)).show(ui, |ui| icons_sheet(ui, t));
    } else {
    egui::CentralPanel::default().frame(egui::Frame::new().fill(t.bg_panel).inner_margin(16.0)).show(ui, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);
        colour(ui, t);
        ui.add_space(12.0);
        typography(ui, t);
    });
    }
    let (Some(path), false) = (shot, *requested) else { return };
    if started.elapsed().as_secs_f32() > after.unwrap_or(1.5) {
        *requested = true;
        let path = path.to_path_buf();
        ui.ctx().request_screenshot(move |img| {
            let [w, h] = img.size;
            let raw: Vec<u8> = img.pixels.iter().flat_map(|p| p.to_array()).collect();
            if let Ok(f) = std::fs::File::create(&path) {
                let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
                e.set_color(png::ColorType::Rgba);
                e.set_depth(png::BitDepth::Eight);
                if let Ok(mut wr) = e.write_header() {
                    let _ = wr.write_image_data(&raw);
                }
            }
            std::process::exit(0);
        });
    } else {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
    }
}
