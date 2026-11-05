//! This-machine chrome (§8.8). Navigation and failure acknowledgement belong to the caller.
use super::{buttons::spinner, chips, keyboard_focus, line, menus::tooltip, paint_line, Vis};
use crate::ui::{
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{
    self, accesskit::Role, Align, Color32, FontId, Painter, Rect, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, WidgetInfo,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Width {
    Wide,
    Medium,
    Compact,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Segment {
    Activity,
    Notices,
    Queue,
    Cpu,
    Ram,
    Gpu,
    Vram,
}
#[derive(Clone, Copy, Debug)]
pub enum ActivityIcon {
    Spinner,
    Icon(Icon),
}
/// Percentages are 0–100; memory quantities are GB. Supply this machine's data only.
#[derive(Clone, Debug)]
pub struct Meters<'a> {
    pub cpu: f32,
    pub ram_used: f32,
    pub ram_total: f32,
    pub gpu_pct: f32,
    pub gpu_label: &'a str,
    pub vram_used: f32,
    pub vram_total: f32,
}
#[derive(Default)]
pub struct StatusData<'a> {
    pub activity: Option<(ActivityIcon, &'a str)>,
    pub notices: u32,
    pub failures: u32,
    /// Both absent means Start. This component never samples hardware or queue state.
    pub queue_waiting: Option<u32>,
    pub meters: Option<Meters<'a>>,
    /// Start-screen look (§7.4, Start mockups): idle activity gets the 16 px `activity` icon and secondary text, the
    /// notices icon is tertiary at zero, and segment widths are whole pixels (one-column divider).
    pub start: bool,
}
pub struct StatusOut {
    pub clicked: Option<Segment>,
    /// Anchors for the caller's popover.
    pub segments: Vec<(Segment, Rect)>,
}
#[derive(Clone, Copy, Default)]
pub struct ForcedState {
    pub segment: Option<Segment>,
    pub visual: Option<Vis>,
    pub focus: bool,
    pub reduced_motion: bool,
}
pub fn status_bar(ui: &mut Ui, data: &StatusData<'_>, width: Width) -> StatusOut {
    status_bar_forced(ui, data, width, ForcedState::default())
}
struct Cell {
    segment: Segment,
    label: String,
    value: String,
    meter: Option<f32>,
    icon: Option<ActivityIcon>,
    tip: String,
}
impl Cell {
    fn text(
        segment: Segment,
        label: impl Into<String>,
        value: impl Into<String>,
        tip: impl Into<String>,
    ) -> Self {
        Self {
            segment,
            label: label.into(),
            value: value.into(),
            meter: None,
            icon: None,
            tip: tip.into(),
        }
    }
    fn width(&self, ui: &Ui) -> f32 {
        16.0 + measure(ui.painter(), &self.label, ty::small())
            + measure(ui.painter(), &self.value, number_font(self.segment))
            + if !self.label.is_empty() && !self.value.is_empty() {
                6.0
            } else {
                0.0
            }
            + if self.meter.is_some() { 30.0 } else { 0.0 }
            + match self.icon {
                Some(ActivityIcon::Spinner) => 18.0,
                Some(ActivityIcon::Icon(Icon::Activity)) => 22.0,
                Some(_) => 20.0,
                None => 0.0,
            }
    }
}
fn cells(data: &StatusData<'_>, width: Width) -> Vec<Cell> {
    let (icon, text) = data
        .activity
        .map(|(i, t)| (Some(i), t))
        .unwrap_or((None, "No jobs running"));
    let mut activity = Cell::text(
        Segment::Activity,
        text,
        "",
        format!(
            "Activity · All · {text} · {} unacknowledged failures",
            data.failures
        ),
    );
    activity.icon = if icon.is_none() { Some(ActivityIcon::Icon(Icon::Activity)) } else { icon };
    let mut notices = Cell::text(
        Segment::Notices,
        "",
        data.notices.to_string(),
        "Activity · Notices",
    );
    notices.icon = Some(ActivityIcon::Icon(Icon::Info));
    let mut out = vec![activity, notices];
    if let Some(n) = data.queue_waiting {
        out.push(Cell::text(
            Segment::Queue,
            "waiting",
            n.to_string(),
            "Board · Queue · Waiting",
        ));
    }
    if let Some(m) = &data.meters {
        let gpu_tip = format!("GPU 0 · {} · {} GB", m.gpu_label, m.vram_total);
        if width == Width::Compact {
            out.push(Cell::text(Segment::Gpu,"GPU",format!("{:.0}%",m.gpu_pct),format!("This machine · CPU {:.0}% · RAM {:.1} / {:.1} GB · {gpu_tip} · GPU {:.0}% · VRAM {:.1} / {:.1} GB",m.cpu,m.ram_used,m.ram_total,m.gpu_pct,m.vram_used,m.vram_total)));
        } else {
            for (seg, label, value, pct, tip) in [
                (
                    Segment::Cpu,
                    "CPU",
                    format!("{:.0}%", m.cpu),
                    m.cpu,
                    "This machine · CPU".to_owned(),
                ),
                (
                    Segment::Ram,
                    "RAM",
                    format!("{:.1} / {:.1} GB", m.ram_used, m.ram_total),
                    percent(m.ram_used, m.ram_total),
                    "This machine · RAM".to_owned(),
                ),
                (
                    Segment::Gpu,
                    "GPU0",
                    format!("{:.0}%", m.gpu_pct),
                    m.gpu_pct,
                    gpu_tip,
                ),
                (
                    Segment::Vram,
                    "VRAM",
                    format!("{:.1} / {:.1} GB", m.vram_used, m.vram_total),
                    percent(m.vram_used, m.vram_total),
                    "This machine · VRAM".to_owned(),
                ),
            ] {
                let mut c = Cell::text(seg, label, value, tip);
                c.meter = Some(pct);
                out.push(c);
            }
        }
    }
    out
}
fn percent(used: f32, total: f32) -> f32 {
    if total > 0.0 {
        used / total * 100.0
    } else {
        0.0
    }
}
fn measure(p: &Painter, s: &str, font: FontId) -> f32 {
    line(p, s, font, 14.0, f32::INFINITY).size().x
}
fn number_font(segment: Segment) -> FontId {
    // Reuse the calibrated unhinted Plex face: a lone hinted 11 px counter is too heavy in Light.
    if segment == Segment::Notices {
        FontId::new(11.0, egui::FontFamily::Name("MonoFrame".into()))
    } else {
        ty::mono()
    }
}
fn text(p: &Painter, r: Rect, x: &mut f32, s: &str, font: FontId, color: Color32, max: f32) {
    let g = line(p, s, font, 14.0, max.max(0.0));
    let w = g.size().x;
    paint_line(
        p,
        Rect::from_min_size(egui::pos2(*x, r.top()), egui::vec2(w, 22.0)),
        g,
        14.0,
        Align::Min,
        color,
        0.0,
    );
    *x += w;
}
/// Forced states follow the production layout, interaction and paint path.
pub fn status_bar_forced(
    ui: &mut Ui,
    data: &StatusData<'_>,
    width: Width,
    forced: ForcedState,
) -> StatusOut {
    let t = tokens(ui);
    let (bar, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), Sense::hover());
    let p = ui.painter().with_clip_rect(bar.intersect(ui.clip_rect()));
    p.rect_filled(bar, 0, t.bg_app);
    p.rect_filled(
        Rect::from_min_size(bar.min, egui::vec2(bar.width(), 1.0)),
        0,
        t.border_divider,
    );
    let cells = cells(data, width);
    let widths: Vec<f32> = cells
        .iter()
        .enumerate()
        .map(|(i, c)| c.width(ui) + if i + 1 < cells.len() && i != 1 && !data.start { 1.0 } else { 0.0 })
        .map(|w| if data.start { w.ceil() } else { w })
        .collect();
    let failure_w = if data.failures > 0 {
        16.0 + measure(&p, &data.failures.to_string(), ty::small())
    } else {
        0.0
    };
    let right: f32 = widths.iter().skip(2).sum();
    let activity_w = (widths[0] + failure_w).min((bar.width() - widths[1] - right).max(0.0));
    let mut x = bar.left();
    let mut out = StatusOut {
        clicked: None,
        segments: Vec::new(),
    };
    for (i, c) in cells.iter().enumerate() {
        if i == 2 {
            x = x.max(bar.right() - right);
        }
        let w = if i == 0 { activity_w } else { widths[i] };
        let r = Rect::from_min_size(egui::pos2(x, bar.top()), egui::vec2(w, 22.0)).intersect(bar);
        x += w;
        if !r.is_positive() {
            continue;
        }
        let response = ui.interact(
            r,
            ui.scope_id().with(("statusbar", c.segment)),
            Sense::click(),
        );
        if response.clicked() {
            response.request_focus();
        }
        let name = if c.segment == Segment::Activity {
            format!("{}, {} unacknowledged failures", c.label, data.failures)
        } else if c.segment == Segment::Queue {
            format!("{} waiting", c.value)
        } else if c.segment == Segment::Notices {
            format!("{} notices", c.value)
        } else {
            format!("{} {}", c.label, c.value)
        };
        response.widget_info(|| WidgetInfo::labeled(Role::Button, ui.is_enabled(), &name));
        tooltip(&response, &name, None, &c.tip);
        // The shared hover tooltip needs a one-shot wakeup for keyboard focus (§10.2).
        let tip_id = response.id.with("keyboard tooltip");
        if response.has_focus()
            && keyboard_focus(ui.ctx())
            && ui.input(|i| i.viewport().minimized != Some(true))
        {
            let now = ui.input(|i| i.time);
            let start = ui
                .ctx()
                .data_mut(|d| *d.get_temp_mut_or_insert_with(tip_id, || now));
            if now - start < 0.5 {
                ui.ctx()
                    .request_repaint_after(std::time::Duration::from_secs_f64(0.5 - (now - start)));
            } else if !response.hovered() {
                let mut tip = egui::Tooltip::for_widget(&response);
                tip.popup = tip.popup.frame(egui::Frame::NONE);
                tip.show(|ui| {
                    super::menus::paint_tooltip(
                        ui,
                        ui.next_widget_position(),
                        320.0,
                        &name,
                        None,
                        &c.tip,
                        None,
                    )
                });
            }
        } else {
            ui.ctx().data_mut(|d| d.remove::<f64>(tip_id));
        }
        if ui.is_enabled()
            && (response.clicked()
                || (response.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))))
        {
            out.clicked = Some(c.segment);
        }
        let force = forced.segment == Some(c.segment);
        let vis = if force {
            forced.visual.unwrap_or(Vis::Rest)
        } else if response.is_pointer_button_down_on() {
            Vis::Pressed
        } else if response.hovered() {
            Vis::Hover
        } else {
            Vis::Rest
        };
        let bg = match vis {
            Vis::Rest => t.bg_app,
            Vis::Hover => t.bg_hover,
            Vis::Pressed => t.bg_pressed,
        };
        p.rect_filled(r.shrink2(egui::vec2(0.0, 1.0)), 0, bg);
        if (force && forced.focus) || (response.has_focus() && keyboard_focus(ui.ctx())) {
            p.rect_stroke(r, 0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
        }
        let anchor = r;
        // Start look: content centred in the 21 px below the top border (divider at status top + 6).
        let r = if data.start { r.translate(egui::vec2(0.0, 1.0)) } else { r };
        if i + 1 < cells.len() && i != 1 {
            p.rect_filled(
                Rect::from_min_size(
                    egui::pos2(r.right() - 1.0, r.center().y - 6.0),
                    egui::vec2(1.0, 12.0),
                ),
                0,
                t.border_divider,
            );
        }
        let cp = p.with_clip_rect(r.shrink2(egui::vec2(8.0, 0.0)));
        let mut tx = r.left() + 8.0;
        if let Some(icon) = c.icon {
            let size = match icon {
                ActivityIcon::Spinner => 12.0,
                ActivityIcon::Icon(Icon::Activity) => 16.0,
                _ => 14.0,
            };
            let ir = Rect::from_center_size(
                egui::pos2(tx + size / 2.0, r.center().y),
                egui::vec2(size, size),
            );
            match icon {
                ActivityIcon::Spinner => spinner(
                    ui,
                    ir,
                    t.busy_fill,
                    forced.reduced_motion || ui.input(|i| i.viewport().minimized == Some(true)),
                ),
                ActivityIcon::Icon(Icon::Activity) => {
                    icons::paint_bg(&cp, ir, Icon::Activity, IconSize::S16, t.text_secondary, bg)
                }
                ActivityIcon::Icon(icon) => {
                    let col = if data.start && c.segment == Segment::Notices && data.notices == 0 { t.text_tertiary } else { t.info };
                    icons::paint_bg(&cp, ir, icon, IconSize::S14, col, bg)
                }
            };
            tx += size + 6.0;
        }
        if c.segment == Segment::Queue {
            text(
                &cp,
                r,
                &mut tx,
                &c.value,
                ty::mono(),
                t.text_primary,
                f32::INFINITY,
            );
            tx += 6.0;
            text(
                &cp,
                r,
                &mut tx,
                "waiting",
                ty::small(),
                t.text_secondary,
                f32::INFINITY,
            );
        } else {
            let max = r.right() - 8.0 - tx - if i == 0 { failure_w } else { 0.0 };
            text(
                &cp,
                r,
                &mut tx,
                &c.label,
                ty::small(),
                if i == 0 && data.start {
                    t.text_secondary
                } else if i == 0 {
                    t.text_primary
                } else {
                    t.text_secondary
                },
                max,
            );
            if !c.label.is_empty() && (!c.value.is_empty() || c.meter.is_some()) {
                tx += 6.0;
            }
            if let Some(pct) = c.meter {
                let mr =
                    Rect::from_min_size(egui::pos2(tx, r.center().y - 2.0), egui::vec2(24.0, 4.0));
                cp.rect_filled(mr, 0, t.bg_sunken);
                cp.rect_stroke(
                    mr,
                    0,
                    Stroke::new(1.0, t.border_default),
                    StrokeKind::Inside,
                );
                cp.rect_filled(
                    Rect::from_min_size(
                        mr.min + egui::vec2(1.0, 1.0),
                        egui::vec2(22.0 * pct.clamp(0.0, 100.0) / 100.0, 2.0),
                    ),
                    0,
                    t.text_secondary,
                );
                tx += 30.0;
            }
            if !c.value.is_empty() {
                text(
                    &cp,
                    r,
                    &mut tx,
                    &c.value,
                    number_font(c.segment),
                    t.text_primary,
                    f32::INFINITY,
                );
            }
        }
        if i == 0 && data.failures > 0 {
            let mut child = ui.new_child(UiBuilder::new().max_rect(Rect::from_min_size(
                egui::pos2(r.right() - 8.0 - failure_w + 6.0, r.top() + 4.0),
                egui::vec2(failure_w, 14.0),
            )));
            chips::status_badge(&mut child, chips::Status::Error, &data.failures.to_string());
        }
        out.segments.push((c.segment, anchor));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> StatusData<'static> {
        StatusData {
            queue_waiting: Some(3),
            meters: Some(Meters {
                cpu: 7.0,
                ram_used: 34.7,
                ram_total: 123.5,
                gpu_pct: 21.0,
                gpu_label: "RTX 4090",
                vram_used: 3.6,
                vram_total: 24.0,
            }),
            ..Default::default()
        }
    }
    #[test]
    fn compact_reduces_meters_to_gpu() {
        let c = cells(&data(), Width::Compact);
        assert_eq!(
            c.iter().map(|c| c.segment).collect::<Vec<_>>(),
            vec![
                Segment::Activity,
                Segment::Notices,
                Segment::Queue,
                Segment::Gpu
            ]
        );
        assert_eq!(c[3].label, "GPU");
        assert_eq!(c[3].value, "21%");
        assert!(c[3].meter.is_none());
        for s in [
            "CPU 7%",
            "RAM 34.7 / 123.5 GB",
            "RTX 4090",
            "VRAM 3.6 / 24.0 GB",
        ] {
            assert!(c[3].tip.contains(s));
        }
    }
    #[test]
    fn start_screen_shows_only_activity_and_notices() {
        for w in [Width::Wide, Width::Medium, Width::Compact] {
            assert_eq!(
                cells(&StatusData::default(), w)
                    .iter()
                    .map(|c| c.segment)
                    .collect::<Vec<_>>(),
                vec![Segment::Activity, Segment::Notices]
            );
        }
    }
    fn frame(ctx: &egui::Context, events: Vec<egui::Event>) -> StatusOut {
        let mut out = None;
        let mut full = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1920.0, 1080.0),
                )),
                time: Some(ctx.input(|i| i.time) + 1.0 / 30.0),
                events,
                ..Default::default()
            },
            |ui| {
                out = Some(status_bar(ui, &data(), Width::Wide));
            },
        );
        full.textures_delta.clear();
        out.unwrap()
    }
    /// Start look: whole-pixel segments, so the one divider sits on a single pixel column.
    #[test]
    fn start_segments_are_whole_pixels() {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let mut segs = vec![];
        let mut full = ctx.run_ui(egui::RawInput::default(), |ui| {
            segs = status_bar(ui, &StatusData { start: true, ..Default::default() }, Width::Wide).segments;
        });
        full.textures_delta.clear();
        assert_eq!(segs.iter().map(|s| s.0).collect::<Vec<_>>(), vec![Segment::Activity, Segment::Notices]);
        assert!(segs.iter().all(|s| s.1.width().fract() == 0.0 && s.1.min.x.fract() == 0.0), "{segs:?}");
    }

    #[test]
    fn each_segment_reports_its_click_target() {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let first = frame(&ctx, vec![]);
        assert_eq!(first.segments.len(), 7);
        for (seg, r) in first.segments {
            let pos = r.center();
            frame(
                &ctx,
                vec![
                    egui::Event::PointerMoved(pos),
                    egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: true,
                        modifiers: Default::default(),
                    },
                ],
            );
            assert_eq!(
                frame(
                    &ctx,
                    vec![egui::Event::PointerButton {
                        pos,
                        button: egui::PointerButton::Primary,
                        pressed: false,
                        modifiers: Default::default()
                    }]
                )
                .clicked,
                Some(seg)
            );
            assert_eq!(
                frame(
                    &ctx,
                    vec![egui::Event::Key {
                        key: egui::Key::Enter,
                        physical_key: None,
                        pressed: true,
                        repeat: false,
                        modifiers: Default::default()
                    }]
                )
                .clicked,
                Some(seg)
            );
            frame(
                &ctx,
                vec![egui::Event::Key {
                    key: egui::Key::Enter,
                    physical_key: None,
                    pressed: false,
                    repeat: false,
                    modifiers: Default::default(),
                }],
            );
        }
        frame(&ctx, vec![egui::Event::PointerGone]);
        for _ in 0..18 {
            frame(&ctx, vec![]);
        }
    }
}
