//! Board › Queue panel (bible §7.3, mockup pages 10/11), reading `board::Fixture`.
use super::{
        controls::{
        buttons,
        card::shot_name,
        chips::{self, Job, Stage},
        line, paint_line, Tab, TabKind, TabStrip,
        table::{Column, Table},
    },
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use super::board::Fixture;
use eframe::egui::{self, accesskit::Role, pos2, vec2, Align, Color32, Key, Layout, Rect, Sense, Ui, UiBuilder, WidgetInfo};

const HEADER_H: f32 = 32.0;
const TABS_H: f32 = 24.0;
const MACHINE_ROW_H: f32 = 60.0;

fn content(ui: &mut Ui, r: Rect, fx: &mut Fixture, overlay: bool, live_ui: &mut impl FnMut(&mut Ui, &str)) {
    if fx.live && fx.qtab == "waiting" {
        ui.painter().rect_filled(r, 0, if overlay { tokens(ui).bg_overlay } else { tokens(ui).bg_panel });
        let mut body = ui.new_child(UiBuilder::new().max_rect(r));
        body.set_clip_rect(r.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().id_salt("board-live-waiting").show(&mut body, |ui| live_ui(ui, "waiting"));
    } else if fx.live {
        ui.painter().rect_filled(r, 0, if overlay { tokens(ui).bg_overlay } else { tokens(ui).bg_panel });
        let mut body = ui.new_child(UiBuilder::new().max_rect(r));
        body.set_clip_rect(r.intersect(ui.clip_rect()));
        egui::ScrollArea::vertical().id_salt("board-live-details").show(&mut body, |ui| live_ui(ui, "details"));
    } else {
        if fx.qtab == "waiting" {
            waiting(ui, r, fx, overlay)
        } else {
            machines(ui, r, fx, overlay)
        }
    }
}

fn machines(ui: &mut Ui, r: Rect, fx: &Fixture, overlay: bool) {
    let t = tokens(ui);
    ui.painter().rect_filled(r, 0, if overlay { t.bg_overlay } else { t.bg_panel });
    let mut body = ui.new_child(UiBuilder::new().max_rect(r));
    body.set_clip_rect(r.intersect(ui.clip_rect()));
    let row_w = (r.width() - 16.0).max(0.0);
    for (i, m) in fx.machines.iter().enumerate() {
        let row = Rect::from_min_size(pos2(r.min.x + 8.0, r.min.y + i as f32 * MACHINE_ROW_H), vec2(row_w, MACHINE_ROW_H));
        machine_row(&mut body, row, m, i, i + 1 == fx.machines.len());
    }
}

fn machine_row(ui: &mut Ui, r: Rect, m: &super::board::FxMachine, index: usize, last: bool) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    let local = m.local != 0;
    let online = m.state != "offline";
    let accessibility = format!(
        "{}; {}; job {}; GPU {}; VRAM {}; CPU {}; RAM {}; {}",
        m.name,
        if online { "Online" } else { "Offline" },
        if m.job.is_empty() { "Idle" } else { &m.job },
        meter_label(m.gpu), meter_label(m.vram), meter_label(m.cpu), meter_label(m.ram),
        m.telemetry_status,
    );
    let row_response = ui.interact(r, ui.scope_id().with(("queue-machine", if m.id.is_empty() { index.to_string() } else { m.id.clone() })), Sense::hover());
    row_response.widget_info(|| WidgetInfo::labeled(Role::Group, true, accessibility.clone()));
    let inner = Rect::from_min_max(pos2(r.min.x + 8.0, r.min.y), pos2(r.max.x - 8.0, r.max.y));
    let first = Rect::from_min_size(pos2(inner.min.x, r.min.y + 4.0), vec2(inner.width(), 20.0));
    icons::paint(&p, Rect::from_min_size(pos2(first.min.x, first.min.y + 2.0), vec2(16.0, 16.0)), if local { Icon::Machine } else { Icon::MachineNetwork }, IconSize::S16, t.text_secondary);

    let busy = m.state == "busy";
    let tail = if busy { 16.0 + 6.0 + 8.0 + 6.0 + 16.0 + 6.0 + 24.0 } else { 16.0 + 6.0 + 8.0 + 6.0 + 32.0 };
    let name_w = line(&p, &m.name, ty::value(), 16.0, f32::INFINITY).size().x.min((inner.width() - tail).max(0.0));
    let name_r = Rect::from_min_size(pos2(first.min.x + 22.0, first.min.y), vec2(name_w, first.height()));
    paint_line(&p, name_r, line(&p, &m.name, ty::value(), 16.0, name_w), 16.0, Align::Min, t.text_primary, 0.0);
    let _ = ui.interact(name_r, ui.scope_id().with(("queue-machine-name", index)), Sense::hover()).on_hover_text(&m.name);

    let dot_x = name_r.max.x + 6.0;
    let dot_r = Rect::from_min_size(pos2(dot_x, first.min.y), vec2(8.0, first.height()));
    p.circle_filled(pos2(dot_x + 3.0, first.center().y), 3.0, if online { t.success } else { t.error });
    let _ = ui.interact(dot_r, ui.scope_id().with(("queue-machine-status", index)), Sense::hover()).on_hover_text(connection_label(online));

    let mut x = dot_x + 12.0; // dot is 6 px wide; mockup gap 6 (mrow flex gap)
    if busy {
        icons::paint(&p, Rect::from_min_size(pos2(x, first.min.y + 2.0), vec2(16.0, 16.0)), Icon::Activity, IconSize::S16, t.text_secondary);
        x += 22.0;
        let job_r = Rect::from_min_max(pos2(x, first.min.y), pos2(first.max.x, first.max.y));
        let job_text = if m.telemetry_status.is_empty() { m.job.clone() } else { format!("{} · {}", m.job, m.telemetry_status) };
        let full = line(&p, &job_text, ty::small(), 14.0, f32::INFINITY);
        let job = if full.size().x > job_r.width() { without_verb(&job_text) } else { &job_text };
        paint_line(&p, job_r, line(&p, job, ty::small(), 14.0, job_r.width()), 14.0, Align::Min, t.text_secondary, 0.0);
        let _ = ui.interact(job_r, ui.scope_id().with(("queue-machine-job", index)), Sense::hover()).on_hover_text(&m.job);
    } else {
        let status = if online { "idle" } else { connection_label(false) };
        let c = if online { t.text_tertiary } else { t.text_secondary };
        let status_r = Rect::from_min_max(pos2(x, first.min.y), first.max);
        let status = if !online || m.telemetry_status.is_empty() { status.to_owned() } else { format!("{status} · {}", m.telemetry_status) };
        paint_line(&p, status_r, line(&p, &status, ty::small(), 14.0, status_r.width()), 14.0, Align::Min, c, 0.0);
    }

    let col_w = ((inner.width() - 12.0) / 2.0).max(0.0);
    for (j, (label, value, tip)) in [
        ("GPU", finite(m.gpu), None), ("VRAM", finite(m.vram), Some(m.vramt.as_str())),
        ("CPU", finite(m.cpu), None), ("RAM", finite(m.ram), Some(m.ramt.as_str())),
    ].into_iter().enumerate() {
        let cell = Rect::from_min_size(
            pos2(inner.min.x + (j % 2) as f32 * (col_w + 12.0), r.min.y + 24.0 + (j / 2) as f32 * 16.0),
            vec2(col_w, 16.0),
        );
        meter(ui, cell, label, if online { value } else { None }, if online { tip } else { None }, index, j);
    }
    if !last {
        p.rect_filled(Rect::from_min_size(pos2(r.min.x, r.max.y - 1.0), vec2(r.width(), 1.0)), 0, t.border_divider);
    }
}

fn meter(ui: &mut Ui, r: Rect, label: &str, value: Option<f32>, tip: Option<&str>, machine: usize, slot: usize) {
    let t = tokens(ui);
    let p = ui.painter().clone();
    paint_line(&p, Rect::from_min_size(r.min, vec2(36.0, r.height())), line(&p, label, ty::small(), 14.0, 36.0), 14.0, Align::Min, t.text_secondary, 0.0);
    let value_text = value.map_or_else(|| "—".to_owned(), |v| format!("{:.0}%", v.clamp(0.0, 100.0)));
    paint_line(&p, Rect::from_min_size(pos2(r.min.x + 36.0, r.min.y), vec2(36.0, r.height())), line(&p, &value_text, ty::mono(), 14.0, 36.0), 14.0, Align::Max, if value.is_some() { t.text_primary } else { t.text_tertiary }, 0.0);
    if let Some(v) = value.filter(|_| r.width() >= 100.0) {
        let bar = Rect::from_min_size(pos2(r.min.x + 76.0, r.center().y - 2.0), vec2(24.0, 4.0));
        p.rect_filled(bar, 0, t.border_default);
        let track = bar.shrink(1.0);
        p.rect_filled(track, 0, t.bg_sunken);
        let pct = v.clamp(0.0, 100.0);
        p.rect_filled(Rect::from_min_size(track.min, vec2(track.width() * pct / 100.0, track.height())), 0, t.busy_fill);
    }
    if let Some(tip) = tip.filter(|s| !s.is_empty()) {
        let _ = ui.interact(r, ui.scope_id().with(("queue-meter", machine, slot)), Sense::hover()).on_hover_text(tip);
    }
}

fn waiting(ui: &mut Ui, r: Rect, fx: &Fixture, overlay: bool) {
    let t = tokens(ui);
    ui.painter().rect_filled(r, 0, if overlay { t.bg_overlay } else { t.bg_panel });
    let waiting: Vec<_> = fx.waiting.iter().filter_map(|&i| fx.cards.get(i)).collect();
    let columns = [Column::new("Shot", 96.0).flexible(), Column::new("Stage", 72.0), Column::new("Job", 84.0)];
    let mut table_ui = ui.new_child(UiBuilder::new().max_rect(r));
    table_ui.set_clip_rect(r.intersect(ui.clip_rect()));
    let out = Table::new("board-queue-waiting", &columns)
        .rows(waiting.len(), |row, i| { if let Some(shot) = waiting.get(i) { row.name(&shot.n); } })
        .size(r.size())
        .label("Waiting queue")
        .show(&mut table_ui);
    let p = ui.painter().clone();
    for (i, row) in out.row_rects {
        let Some(shot) = waiting.get(i) else { continue };
        let x0 = out.rect.min.x - out.offset.x;
        let shot_cell = Rect::from_min_size(pos2(x0, row.min.y), vec2(out.widths[0], row.height()));
        let stage_cell = Rect::from_min_size(pos2(shot_cell.max.x, row.min.y), vec2(out.widths[1], row.height()));
        let job_cell = Rect::from_min_size(pos2(stage_cell.max.x, row.min.y), vec2(out.widths[2], row.height()));
        let text_r = Rect::from_min_max(pos2(shot_cell.min.x + 8.0, row.min.y), pos2(shot_cell.max.x - 8.0, row.max.y));
        let name = shot_name(&shot.n, text_r.width(), |s| line(&p, s, ty::mono(), 14.0, f32::INFINITY).size().x);
        paint_line(&p, text_r, line(&p, &name, ty::mono(), 14.0, text_r.width()), 14.0, Align::Min, t.text_primary, 0.0);
        let tip = format!("{} · {}", shot.n, if shot.pc.is_empty() { "no machine" } else { &shot.pc });
        let _ = ui.interact(row, ui.scope_id().with(("queue-waiting-row", i)), Sense::hover()).on_hover_text(tip);

        let stage = match shot.st.as_str() { "mask" => Stage::Mask, "ready" => Stage::Ready, "done" => Stage::Completed, _ => Stage::Raw };
        let chip_r = Rect::from_min_max(pos2(stage_cell.min.x + 8.0, row.min.y), pos2(stage_cell.max.x, row.max.y)); // "Ready" + icon is wider than 56
        let mut chip_ui = ui.new_child(UiBuilder::new().max_rect(chip_r).layout(Layout::left_to_right(Align::Center)));
        chip_ui.set_clip_rect(chip_r.intersect(ui.clip_rect()));
        chips::stage_chip(&mut chip_ui, stage);

        let badge_r = Rect::from_min_size(pos2(job_cell.min.x + 8.0, row.min.y + 3.0), vec2(72.0, 18.0));
        p.rect_filled(badge_r, 3, t.busy_queued);
        let mut badge_ui = ui.new_child(UiBuilder::new().max_rect(badge_r).layout(Layout::left_to_right(Align::Center)));
        badge_ui.set_clip_rect(badge_r.intersect(ui.clip_rect()));
        chips::job_badge(&mut badge_ui, Job::Queued);
    }
}

fn without_verb(job: &str) -> &str { job.split_once(' ').map_or(job, |(_, rest)| rest) }

/// §7.6: bg.overlay drawer, 1 px border.overlay left edge, dialog shadow, close icon, over the main-area scrim.
pub fn drawer(ui: &mut Ui, main: Rect, fx: &mut Fixture, live_ui: &mut impl FnMut(&mut Ui, &str)) -> bool {
    let ctx = ui.ctx().clone();
    let mut clear_finished = false;
    let id = ui.make_persistent_id("board-queue-drawer");
    let opener = ctx.data(|d| d.get_temp::<egui::Id>(id.with("opener")));
    let opened = ctx.data(|d| d.get_temp::<bool>(id).unwrap_or(false));
    let modal = egui::Modal::new(id).area(egui::Modal::default_area(id).fixed_pos(main.min).constrain(false).fade_in(false)).backdrop_color(Color32::TRANSPARENT).frame(egui::Frame::NONE).show(&ctx, |ui| {
        let t = tokens(ui);
        let p = ui.painter().clone();
        p.rect_filled(main, 0, t.scrim);
        let r = Rect::from_min_max(pos2(main.max.x - 300.0, main.min.y), main.max);
        ui.set_min_size(main.size());
        p.add(ui.visuals().popup_shadow.as_shape(r, 0));
        p.rect_filled(r, 0, t.bg_overlay);
        p.rect_filled(Rect::from_min_size(r.min, vec2(1.0, r.height())), 0, t.border_overlay);
        p.rect_filled(Rect::from_min_size(pos2(r.min.x, r.min.y + 31.0), vec2(r.width(), 1.0)), 0, t.border_overlay);
        icons::paint(&p, Rect::from_min_size(pos2(r.min.x + 9.0, r.min.y + 8.0), vec2(16.0, 16.0)), Icon::Queue, IconSize::S16, t.text_secondary);
        let g = line(&p, "Queue", ty::panel_header(), 16.0, f32::INFINITY);
        paint_line(&p, Rect::from_min_size(pos2(r.min.x + 31.0, r.min.y), vec2(200.0, 32.0)), g, 16.0, Align::Min, t.text_primary, 0.0);
        let close = ui.add(buttons::icon_button(Icon::Close, buttons::IconButtonSize::Normal22, "Close\nEsc").at(pos2(r.max.x - 8.0 - 22.0, r.min.y + 5.0)));
        if !opened { ui.ctx().memory_mut(|m| m.request_focus(close.id)); }
        if close.clicked() { fx.drawer = 0; }
        if fx.live && fx.finished > 0 && ui.add(buttons::icon_button(Icon::ClearFinished, buttons::IconButtonSize::Normal22, "Clear finished").at(pos2(r.max.x - 8.0 - 48.0, r.min.y + 5.0))).clicked() { clear_finished = true; }
        let strip = Rect::from_min_size(pos2(r.min.x + 1.0, r.min.y + 32.0), vec2(r.width() - 1.0, 24.0));
        p.rect_filled(strip, 0, t.bg_app);
        p.rect_filled(Rect::from_min_size(pos2(strip.min.x, strip.max.y - 1.0), vec2(strip.width(), 1.0)), 0, t.border_overlay);
        let mut active = usize::from(fx.qtab == "waiting");
        let tabs = [Tab::new("Machines").icon(Icon::Machines), Tab::new("Waiting").badge(waiting_count(fx))];
        let mut tab_rects = Vec::new();
        TabStrip::new(TabKind::Panel, &mut active, &tabs).name("Queue drawer").at(strip.min).rects_out(&mut tab_rects).show(ui);
        fx.qtab = if active == 1 { "waiting" } else { "machines" }.into();
        if active == 1 {
            if let Some(tab) = tab_rects.get(1).copied() {
                p.rect_filled(tab, 0, t.bg_overlay);
                p.rect_filled(Rect::from_min_size(tab.min, vec2(tab.width(), 2.0)), 0, t.accent_bright);
                p.rect_filled(Rect::from_min_size(pos2(tab.max.x - 1.0, tab.min.y), vec2(1.0, tab.height())), 0, t.border_divider);
                let label = line(&p, "Waiting", ty::button(), 16.0, f32::INFINITY);
                let x = tab.min.x + 12.0;
                paint_line(&p, Rect::from_min_size(pos2(x, tab.min.y), vec2(label.size().x + 1.0, tab.height())), label.clone(), 16.0, Align::Min, t.text_primary, 0.0);
                let count = waiting_count(fx);
                let badge_w = (line(&p, &count.to_string(), ty::chip(), 14.0, f32::INFINITY).size().x + 10.0).round();
                let badge = Rect::from_min_size(pos2((x + label.size().x + 6.0).round(), (tab.center().y - 8.0).round()), vec2(badge_w, 16.0));
                p.rect_filled(badge, 8, t.bg_pressed);
                paint_line(&p, badge, line(&p, &count.to_string(), ty::chip(), 14.0, f32::INFINITY), 14.0, Align::Center, t.text_primary, 0.0);
            }
        }
        content(ui, Rect::from_min_max(pos2(r.min.x + 1.0, r.min.y + HEADER_H + TABS_H), r.max), fx, true, live_ui);
    });
    let scrim_clicked = ctx.input(|i| i.pointer.button_clicked(egui::PointerButton::Primary) && i.pointer.interact_pos().is_some_and(|p| main.contains(p) && p.x < main.max.x - 300.0));
    if modal.is_top_modal && (ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) || scrim_clicked) { fx.drawer = 0; }
    if fx.drawer == 0 {
        if let Some(opener) = opener { ctx.data_mut(|d| d.insert_temp(id.with("return"), Some(opener))); }
        ctx.data_mut(|d| d.remove::<bool>(id));
    } else { ctx.data_mut(|d| d.insert_temp(id, true)); }
    clear_finished
}

/// Docked Queue (header 32 + tab strip 24) or the Compact rail.
pub fn panel(ui: &mut Ui, r: Rect, docked: bool, fx: &mut Fixture, live_ui: &mut impl FnMut(&mut Ui, &str)) -> bool {
    let t = tokens(ui);
    let p = ui.painter().clone();
    p.rect_filled(r, 0, t.bg_panel);
    if !docked {
        let c = |x: f32, y: f32, w: f32, h: f32| Rect::from_min_size(pos2(r.min.x + x, r.min.y + y), vec2(w, h));
        let queue_button = ui.add(buttons::icon_button(Icon::Queue, buttons::IconButtonSize::Normal22, &format!("Queue · {} rendering · {} waiting", fx.running, waiting_count(fx))).at(c(9.0, 8.0, 22.0, 22.0).min));
        let back = ui.ctx().data_mut(|d| d.remove_temp::<Option<egui::Id>>(ui.make_persistent_id("board-queue-drawer").with("return")).flatten());
        if back == Some(queue_button.id) { ui.ctx().memory_mut(|m| m.request_focus(queue_button.id)); }
        if queue_button.clicked() { ui.ctx().data_mut(|d| d.insert_temp(ui.make_persistent_id("board-queue-drawer").with("opener"), queue_button.id)); fx.drawer = 1; }
        for (i, (n, run)) in [(fx.running, fx.running > 0), (waiting_count(fx), false)].into_iter().enumerate() {
            let g = line(&p, &n.to_string(), ty::chip(), 14.0, f32::INFINITY);
            let w = (g.size().x + 10.0).max(16.0).round();
            let b = c((40.0 - w) / 2.0, 34.0 + i as f32 * 20.0, w, 16.0);
            p.rect_filled(b, 8, if run { t.busy_fill } else { t.bg_pressed });
            paint_line(&p, b, g, 14.0, Align::Center, if run { t.badge_rendering_text } else { t.text_primary }, 0.0);
        }
        return false;
    }
    p.rect_filled(Rect::from_min_size(pos2(r.min.x, r.min.y + HEADER_H - 1.0), vec2(r.width(), 1.0)), 0, t.border_divider);
    icons::paint(&p, Rect::from_min_size(pos2(r.min.x + 8.0, r.min.y + 8.0), vec2(16.0, 16.0)), Icon::Queue, IconSize::S16, t.text_secondary);
    let g = line(&p, "Queue", ty::panel_header(), 16.0, f32::INFINITY);
    paint_line(&p, Rect::from_min_size(pos2(r.min.x + 30.0, r.min.y), vec2(r.width() - 38.0, 32.0)), g, 16.0, Align::Min, t.text_primary, 0.0);
    let strip = Rect::from_min_size(pos2(r.min.x, r.min.y + HEADER_H), vec2(r.width(), TABS_H));
    p.rect_filled(strip, 0, t.bg_app);
    p.rect_filled(Rect::from_min_size(pos2(strip.min.x, strip.max.y - 1.0), vec2(strip.width(), 1.0)), 0, t.border_divider);
    let mut active = usize::from(fx.qtab == "waiting");
    let tabs = [Tab::new("Machines").icon(Icon::Machines), Tab::new("Waiting").badge(waiting_count(fx))];
    TabStrip::new(TabKind::Panel, &mut active, &tabs).name("Queue").at(strip.min).show(ui);
    fx.qtab = if active == 1 { "waiting" } else { "machines" }.into();
    let clear = ui.add(buttons::icon_button(Icon::ClearFinished, buttons::IconButtonSize::Normal22, if fx.finished > 0 { "Clear finished" } else { "Clear finished\nNothing finished to clear" })
        .disabled_if(fx.finished == 0, "Nothing finished to clear").at(pos2(strip.max.x - 1.0 - 22.0, strip.min.y + 1.0)));
    content(ui, Rect::from_min_max(pos2(r.min.x, r.min.y + HEADER_H + TABS_H), r.max), fx, false, live_ui);
    clear.clicked()
}

fn waiting_count(fx: &Fixture) -> u32 { if fx.live { fx.waiting_total } else { fx.waiting.len() as u32 } }
fn finite(v: f32) -> Option<f32> { v.is_finite().then_some(v) }
fn meter_label(v: f32) -> String { finite(v).map_or_else(|| "—".into(), |v| format!("{:.0}%", v.clamp(0.0, 100.0))) }
fn connection_label(online: bool) -> &'static str { if online { "Online" } else { "Offline" } }
#[cfg(test)]
mod tests {
    use super::{connection_label, meter_label, without_verb};

    #[test]
    fn queue_job_drops_verb_before_truncating_shot() {
        assert_eq!(without_verb("Preparing V1-0086"), "V1-0086");
        assert_eq!(without_verb("V1-0086"), "V1-0086");
    }

    #[test]
    fn missing_and_stale_telemetry_are_explicit() {
        assert_eq!(meter_label(f32::NAN), "—");
        assert_eq!(meter_label(0.0), "0%");
        assert_eq!(connection_label(false), "Offline");
    }
}
