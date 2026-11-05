//! Queue hardware rows, bible §7.3, §7.6, §8.4, §12.
use super::{
    chips::{self, Progress, State},
    keyboard_focus, line,
    menus::tooltip,
    paint_line,
};
use crate::ui::{
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{
    self, accesskit::Role, Align, Color32, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui,
    UiBuilder, WidgetInfo,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MachineKind {
    Local,
    Network,
}
/// Percentages are 0..=100; memory capacities are (used, total) GB.
#[derive(Clone, Copy, Debug)]
pub struct Meters {
    pub gpu: f32,
    pub vram_pct: f32,
    pub vram_gb: (f32, f32),
    pub cpu: f32,
    pub ram_pct: f32,
    pub ram_gb: (f32, f32),
}
#[derive(Clone, Debug)]
pub struct MachineRowData {
    pub name: String,
    pub kind: MachineKind,
    pub online: bool,
    pub offline_since: Option<String>,
    pub job: Option<(String, f32)>,
    pub meters: Option<Meters>,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct RowState {
    pub hovered: bool,
    pub selected: bool,
}
fn percent(v: f32) -> f32 {
    if v.is_finite() {
        v.clamp(0.0, 100.0)
    } else {
        0.0
    }
}
fn memory_tip(gb: (f32, f32)) -> String {
    format!("{:.1} / {:.1} GB", gb.0, gb.1)
}
fn text(ui: &Ui, rect: Rect, s: &str, font: FontId, color: Color32, align: Align) {
    let g = line(ui.painter(), s, font, 14.0, rect.width().max(0.0));
    paint_line(ui.painter(), rect, g, 14.0, align, color, 0.0);
}
/// `panel_width` excludes its border; the caller supplies 8 px horizontal panel padding.
/// The returned row occupies panel_width - 16 by exactly 60 points.
pub fn machine_row(ui: &mut Ui, data: &MachineRowData, panel_width: f32) -> Response {
    machine_row_forced(ui, data, panel_width, RowState::default())
}
/// Forced states use the real paint path.
pub fn machine_row_forced(
    ui: &mut Ui,
    data: &MachineRowData,
    panel_width: f32,
    state: RowState,
) -> Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2((panel_width - 16.0).max(0.0), 60.0),
        Sense::click(),
    );
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    child.set_clip_rect(rect.intersect(ui.clip_rect()));
    let ui = &mut child;
    let t = *tokens(ui);
    let hovered = state.hovered || response.hovered();
    let secondary = if state.selected {
        t.text_primary
    } else {
        t.text_secondary
    };
    let tertiary = if state.selected {
        t.text_primary
    } else if hovered {
        t.text_secondary
    } else {
        t.text_tertiary
    };
    ui.painter().rect_filled(
        rect,
        0,
        if state.selected {
            t.accent_tint
        } else if hovered {
            t.bg_hover
        } else {
            t.bg_panel
        },
    );
    ui.painter().rect_filled(
        Rect::from_min_size(
            rect.left_bottom() - egui::vec2(0.0, 1.0),
            egui::vec2(rect.width(), 1.0),
        ),
        0,
        t.border_divider,
    );
    if state.selected {
        ui.painter().rect_filled(
            Rect::from_min_size(rect.min, egui::vec2(2.0, 60.0)),
            0,
            t.accent_bright,
        );
    }
    let top = rect.min + egui::vec2(8.0, 4.0);
    let right = rect.right() - 8.0;
    let icon = if data.kind == MachineKind::Local {
        Icon::Machine
    } else {
        Icon::MachineNetwork
    };
    icons::paint(
        ui.painter(),
        Rect::from_min_size(top + egui::vec2(0.0, 2.0), egui::vec2(16.0, 16.0)),
        icon,
        IconSize::S16,
        t.text_primary,
    );
    let job = data.job.as_ref().filter(|_| data.online);
    let status = if !data.online {
        "offline"
    } else if job.is_some() {
        "rendering"
    } else {
        "idle"
    };
    let name_x = top.x + 22.0;
    let status_w = if job.is_some() {
        20.0
    } else {
        line(ui.painter(), status, ty::small(), 14.0, f32::INFINITY)
            .size()
            .x
            + 16.0
    };
    let name = line(
        ui.painter(),
        &data.name,
        ty::value(),
        16.0,
        (right - name_x - status_w).max(0.0),
    );
    let name_w = name.size().x;
    paint_line(
        ui.painter(),
        Rect::from_min_size(egui::pos2(name_x, top.y), egui::vec2(name_w, 20.0)),
        name,
        16.0,
        Align::Min,
        t.text_primary,
        0.0,
    );
    let sx = name_x + name_w + 6.0;
    if let Some((shot, pct)) = job {
        icons::paint(
            ui.painter(),
            Rect::from_min_size(egui::pos2(sx, top.y + 2.0), egui::vec2(16.0, 16.0)),
            Icon::Activity,
            IconSize::S16,
            secondary,
        );
        let label = format!("Keying {shot} · {:.0}%", percent(*pct));
        text(
            ui,
            Rect::from_min_max(
                egui::pos2(sx + 20.0, top.y),
                egui::pos2(right, top.y + 20.0),
            ),
            &label,
            ty::small(),
            secondary,
            Align::Min,
        );
    } else if !data.online {
        let r = Rect::from_min_max(egui::pos2(sx, top.y + 1.0), egui::pos2(right, top.y + 19.0));
        let mut badge_ui = ui.new_child(UiBuilder::new().max_rect(r));
        chips::status_badge(&mut badge_ui, chips::Status::Error, "offline");
    } else {
        ui.painter()
            .circle_filled(egui::pos2(sx + 3.0, top.y + 10.0), 3.0, t.success);
        text(
            ui,
            Rect::from_min_max(
                egui::pos2(sx + 10.0, top.y),
                egui::pos2(right, top.y + 20.0),
            ),
            "idle",
            ty::small(),
            tertiary,
            Align::Min,
        );
    }
    let col = ((rect.width() - 28.0) / 2.0).max(0.0);
    let meters = data.meters.filter(|_| data.online);
    let values = meters.map(|m| [m.gpu, m.vram_pct, m.cpu, m.ram_pct]);
    for (i, label) in ["GPU", "VRAM", "CPU", "RAM"].into_iter().enumerate() {
        let r = Rect::from_min_size(
            top + egui::vec2((i % 2) as f32 * (col + 12.0), 20.0 + (i / 2) as f32 * 16.0),
            egui::vec2(col, 16.0),
        );
        let val = values
            .map(|v| format!("{:.0}%", percent(v[i])))
            .unwrap_or_else(|| "—".into());
        text(
            ui,
            Rect::from_min_size(r.min, egui::vec2(36.0, 16.0)),
            label,
            ty::small(),
            secondary,
            Align::Min,
        );
        text(
            ui,
            Rect::from_min_size(r.min + egui::vec2(36.0, 0.0), egui::vec2(36.0, 16.0)),
            &val,
            ty::mono(),
            if values.is_some() {
                t.text_primary
            } else {
                tertiary
            },
            Align::Max,
        );
        let tip = meters
            .map(|m| match i {
                1 => memory_tip(m.vram_gb),
                3 => memory_tip(m.ram_gb),
                _ => format!("{label} {val}"),
            })
            .unwrap_or_else(|| "Not reported".into());
        let meter_response = ui.interact(r, response.id.with(i), Sense::hover());
        meter_response.widget_info(|| {
            WidgetInfo::labeled(
                Role::Label,
                ui.is_enabled(),
                format!("{label} {val}, {tip}"),
            )
        });
        tooltip(&meter_response, label, None, &tip);
        if let Some(v) = values.filter(|_| panel_width >= 240.0) {
            let gap = if col < 100.0 { 2.0 } else { 4.0 };
            let bar =
                Rect::from_min_size(r.min + egui::vec2(72.0 + gap, 6.0), egui::vec2(24.0, 4.0));
            let mut bar_ui = ui.new_child(UiBuilder::new().max_rect(bar));
            bar_ui.set_clip_rect(bar.shrink(1.0).intersect(ui.clip_rect()));
            chips::progress(
                &mut bar_ui,
                bar.shrink(1.0),
                Progress {
                    value: Some(percent(v[i]) / 100.0),
                    state: State::Running,
                    thin: true,
                    label,
                },
            );
            ui.painter().rect_stroke(
                bar,
                0,
                Stroke::new(1.0, t.border_default),
                StrokeKind::Inside,
            );
        }
    }
    if response.has_focus() && keyboard_focus(ui.ctx()) {
        ui.painter()
            .rect_stroke(rect, 0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
    }
    let reason = if data.online {
        job.map(|(shot, pct)| format!("Keying {shot} · {:.0}%", percent(*pct)))
            .unwrap_or_else(|| "Online, idle".into())
    } else {
        data.offline_since
            .as_ref()
            .map(|s| format!("Offline since {s}"))
            .unwrap_or_else(|| "Machine is offline".into())
    };
    response.widget_info(|| {
        WidgetInfo::labeled(
            Role::Button,
            ui.is_enabled(),
            format!("{}, {:?}, {status}. {reason}", data.name, data.kind),
        )
    });
    ui.ctx()
        .accesskit_node_builder(response.id, |n| n.set_selected(state.selected));
    tooltip(&response, &data.name, None, &reason)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn data() -> MachineRowData {
        MachineRowData {
            name: "This machine".into(),
            kind: MachineKind::Local,
            online: true,
            offline_since: None,
            job: Some(("V1-0062".into(), 34.0)),
            meters: Some(Meters {
                gpu: 21.0,
                vram_pct: 15.0,
                vram_gb: (3.6, 24.0),
                cpu: 7.0,
                ram_pct: 28.0,
                ram_gb: (34.7, 123.5),
            }),
        }
    }
    fn render(data: &MachineRowData, width: f32) -> egui::FullOutput {
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        ctx.enable_accesskit();
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let r = machine_row(ui, data, width);
            assert_eq!(r.rect.height(), 60.0);
        });
        out.textures_delta.clear();
        out
    }
    #[test]
    fn row_is_60_high_with_2x2_meters() {
        for width in [300.0, 260.0, 240.0, 220.0] {
            let out = render(&data(), width);
            let bars: Vec<_> = out
                .shapes
                .iter()
                .filter_map(|s| match &s.shape {
                    egui::Shape::Rect(r) if r.rect.size() == egui::vec2(24.0, 4.0) => Some(r.rect),
                    _ => None,
                })
                .collect();
            if width < 240.0 {
                assert!(bars.is_empty());
                continue;
            }
            assert_eq!(bars.len(), 4);
            assert_eq!(bars[0].top(), bars[1].top());
            assert_eq!(bars[2].top() - bars[0].top(), 16.0);
            assert_eq!(bars[0].left(), bars[2].left());
            assert!(bars.iter().all(|r| r.right() <= 8.0 + width - 24.0));
        }
    }
    #[test]
    fn meters_show_percent_and_gb_in_tooltip() {
        let out = render(&data(), 300.0);
        let update = out.platform_output.accesskit_update.unwrap();
        let names: Vec<_> = update.nodes.iter().filter_map(|(_, n)| n.value()).collect();
        assert!(
            names.iter().any(|s| s.contains("VRAM 15%, 3.6 / 24.0 GB")),
            "{names:?}"
        );
        assert!(names.iter().any(|s| s.contains("RAM 28%, 34.7 / 123.5 GB")));
        let ctx = egui::Context::default();
        crate::ui::fonts::install(&ctx);
        let mut shown = false;
        for time in [0.0, 1.0, 2.0] {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events: if time == 0.0 {
                        vec![egui::Event::PointerMoved(egui::pos2(190.0, 36.0))]
                    } else {
                        vec![]
                    },
                    ..Default::default()
                },
                |ui| {
                    machine_row(ui, &data(), 300.0);
                },
            );
            out.textures_delta.clear();
            shown |= out.shapes.iter().any(|s|matches!(&s.shape,egui::Shape::Text(t) if t.galley.job.text.contains("3.6 / 24.0 GB")));
        }
        assert!(shown, "VRAM hover must show its absolute capacity");
    }
    #[test]
    fn offline_row_shows_reason_and_no_meters() {
        let mut d = data();
        d.online = false;
        d.offline_since = Some("14:32".into());
        let out = render(&d, 300.0);
        assert!(!out.shapes.iter().any(
            |s| matches!(&s.shape,egui::Shape::Rect(r) if r.rect.size()==egui::vec2(24.0,4.0))
        ));
        let u = out.platform_output.accesskit_update.unwrap();
        assert!(u
            .nodes
            .iter()
            .any(|(_, n)| n.label().is_some_and(|s| s.contains("Offline since 14:32"))));
        assert!(!u
            .nodes
            .iter()
            .any(|(_, n)| n.role() == Role::ProgressIndicator));
    }
}
