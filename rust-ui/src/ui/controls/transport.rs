//! Fixed transport and sequence-frame timeline (bible §8.10).
use super::{
    buttons::{icon_button, toggle_button, IconButtonSize, Vis},
    keyboard_focus, line,
    menus::{self, MenuItem},
    numeric::{NumSpec, NumericField},
    paint_line,
};
use crate::ui::{
    icons::Icon,
    tokens::{tokens, DARK},
    type_scale as ty,
};
use eframe::egui::{
    self, accesskit::Role, pos2, vec2, Align, Key, Modifiers, Rect, Response, Sense, Stroke,
    StrokeKind, Ui, UiBuilder, WidgetInfo,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    Wide,
    Compact,
}
#[derive(Clone, Debug, Default)]
pub struct TransportState {
    pub frame: i64,
    pub first: i64,
    pub last: i64,
    pub playing: bool,
    pub reverse: bool,
    pub loop_on: bool,
    pub in_pt: Option<i64>,
    pub out_pt: Option<i64>,
    pub marks: Vec<i64>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    First,
    StepBack,
    PlayReverse,
    PlayPause,
    PlayForward,
    StepForward,
    Last,
    Seek(i64),
    Mark,
    Unmark,
    ClearMarks,
    SetIn,
    SetOut,
    ClearInOut,
    Loop,
}
pub struct TransportOut {
    pub action: Option<Action>,
}
/// Override one action's appearance through the real button paint path.
#[derive(Clone, Copy, Debug)]
pub struct Forced {
    pub action: Action,
    pub vis: Vis,
    pub focus: bool,
}

fn button(
    ui: &mut Ui,
    at: egui::Pos2,
    icon: Icon,
    name: &str,
    key: &str,
    action: Action,
    disabled: Option<&str>,
    forced: Option<Forced>,
) -> bool {
    let mut b = icon_button(icon, IconButtonSize::Normal22, name).at(at);
    if let Some(reason) = disabled {
        b = b.disabled(reason);
    }
    if let Some(f) = forced.filter(|f| f.action == action) {
        b = b.forced(f.vis, f.focus);
    }
    let r = ui.add(b);
    menus::tooltip(&r, name, Some(key), "");
    r.clicked()
}

fn side_actions(layout: Layout) -> &'static [Action] {
    if layout == Layout::Wide {
        &[
            Action::Mark,
            Action::Unmark,
            Action::ClearMarks,
            Action::SetIn,
            Action::SetOut,
            Action::ClearInOut,
            Action::Loop,
        ]
    } else {
        &[Action::Loop]
    }
}

/// Call only in the viewer/transport shortcut scope. Mutates state and returns the one requested action.
pub fn transport(ui: &mut Ui, state: &mut TransportState, layout: Layout) -> TransportOut {
    transport_forced(ui, state, layout, None)
}
pub fn transport_forced(
    ui: &mut Ui,
    s: &mut TransportState,
    layout: Layout,
    forced: Option<Forced>,
) -> TransportOut {
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::hover());
    let mut child = ui.new_child(
        UiBuilder::new()
            .max_rect(r)
            .layout(egui::Layout::left_to_right(Align::Center)),
    );
    child.set_clip_rect(r.intersect(ui.clip_rect()));
    let ui = &mut child;
    let t = tokens(ui);
    ui.painter().rect_filled(r, 0, t.bg_panel);
    ui.painter().rect_filled(
        Rect::from_min_size(r.min, vec2(r.width(), 1.0)),
        0,
        t.border_divider,
    );
    let y = r.top() + 3.0;
    let mut action = None;
    let marked = s.marks.contains(&s.frame);
    let specs = [
        (Action::Mark, Icon::Flag, "Mark frame", "M", None),
        (
            Action::Unmark,
            Icon::FlagOff,
            "Unmark",
            "Shift+M",
            (!marked).then_some("This frame is not marked"),
        ),
        (
            Action::ClearMarks,
            Icon::FlagClear,
            "Clear marks",
            "Ctrl+Shift+M",
            s.marks.is_empty().then_some("No marked frames"),
        ),
        (Action::SetIn, Icon::In, "Set In", "I", None),
        (Action::SetOut, Icon::Out, "Set Out", "O", None),
        (
            Action::ClearInOut,
            Icon::InOutClear,
            "Clear In/Out",
            "Alt+I",
            None,
        ),
    ];
    if side_actions(layout).contains(&Action::Mark) {
        for (i, &(a, icon, name, key, disabled)) in specs.iter().enumerate() {
            let x = if i < 3 {
                r.left() + 8.0 + i as f32 * 24.0
            } else {
                r.right() - 117.0 + (i - 3) as f32 * 24.0
            };
            if button(ui, pos2(x, y), icon, name, key, a, disabled, forced) {
                action = Some(a);
            }
        }
        ui.painter().rect_filled(
            Rect::from_min_size(pos2(r.right() - 39.0, y + 3.0), vec2(1.0, 16.0)),
            0,
            t.border_divider,
        );
    } else {
        let trigger = ui.add(
            icon_button(Icon::More, IconButtonSize::Normal22, "Transport actions")
                .at(pos2(r.right() - 58.0, y)),
        );
        let items: Vec<_> = specs
            .iter()
            .map(|&(_, icon, name, key, disabled)| {
                let item = MenuItem::new(name).icon(icon).shortcut(key);
                if let Some(reason) = disabled {
                    item.disabled(reason)
                } else {
                    item
                }
            })
            .collect();
        let nav_id = trigger.id.with("overflow-nav");
        let mut nav = ui
            .ctx()
            .data(|d| d.get_temp::<menus::Nav>(nav_id))
            .unwrap_or_default();
        let mut open = false;
        egui::Popup::menu(&trigger).show(|ui| {
            open = true;
            if !nav.open {
                nav.focus = true;
                nav.key(menus::NavKey::Down, &[&items], false);
            }
            trigger.request_focus();
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    trigger.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let mut picked = None;
            for (key, nk) in [
                (Key::ArrowDown, menus::NavKey::Down),
                (Key::ArrowUp, menus::NavKey::Up),
                (Key::Enter, menus::NavKey::Enter),
                (Key::Space, menus::NavKey::Enter),
                (Key::Escape, menus::NavKey::Esc),
            ] {
                if ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                    picked = nav
                        .key(nk, &[&items], false)
                        .and_then(|p| p.first().copied());
                }
            }
            let list = menus::MenuList::new(&items)
                .highlight(nav.path.first().copied())
                .show(ui);
            if let Some(i) = picked.or(list.clicked) {
                action = Some(specs[i].0);
                ui.close();
            } else if !nav.open {
                ui.close();
            }
        });
        if !open {
            nav = menus::Nav::default();
        }
        ui.ctx().data_mut(|d| d.insert_temp(nav_id, nav));
    }
    let total = format!("/ {}", s.last);
    let total_g = line(ui.painter(), &total, ty::small(), 14.0, f32::INFINITY);
    let cw = 142.0 + 10.0 + 64.0 + 6.0 + total_g.size().x;
    let cx = (r.center().x - cw / 2.0).round();
    for (i, (a, icon, name, key)) in [
        (Action::First, Icon::SkipStart, "First frame", "Home"),
        (Action::StepBack, Icon::StepBack, "Step back", "←"),
        (
            Action::PlayReverse,
            Icon::PlayReverse,
            "Play reverse",
            "Shift+←",
        ),
        (
            Action::PlayPause,
            if s.playing { Icon::Pause } else { Icon::Play },
            if s.playing { "Pause" } else { "Play" },
            "Space · Shift+→",
        ),
        (Action::StepForward, Icon::StepForward, "Step forward", "→"),
        (Action::Last, Icon::SkipEnd, "Last frame", "End"),
    ]
    .into_iter()
    .enumerate()
    {
        if button(
            ui,
            pos2(cx + i as f32 * 24.0, y),
            icon,
            name,
            key,
            a,
            None,
            forced,
        ) {
            action = Some(a);
        }
    }
    let fr = Rect::from_min_size(pos2(cx + 152.0, y), vec2(64.0, 22.0));
    let mut field_ui = ui.new_child(
        UiBuilder::new()
            .max_rect(fr)
            .layout(egui::Layout::top_down(Align::Min)),
    );
    let mut value = s.frame as f64;
    let spec = NumSpec::frames(s.first, s.first, s.last.max(s.first));
    let response = NumericField::new("sequence-frame", &mut value, &spec)
        .width(64.0)
        .name("Frame")
        .show(&mut field_ui);
    if response.changed() {
        action = Some(Action::Seek(value.round() as i64));
    }
    // Keep the shared numeric editing/drag contract; the transport readout is padded and centred.
    if !response.dragged() && !ui.memory(|m| m.has_focus(response.id.with("edit"))) {
        ui.painter().rect_filled(
            fr.shrink(2.0),
            1,
            if ui.is_enabled() {
                t.bg_sunken
            } else {
                t.bg_panel
            },
        );
        let g = line(
            ui.painter(),
            &format!("{:04}", s.frame),
            ty::frame_field(),
            16.0,
            60.0,
        );
        paint_line(
            ui.painter(),
            fr,
            g,
            16.0,
            Align::Center,
            if ui.is_enabled() {
                t.text_primary
            } else {
                t.text_disabled
            },
            0.0,
        );
    }
    paint_line(
        ui.painter(),
        Rect::from_min_size(pos2(fr.right() + 6.0, y), vec2(total_g.size().x, 22.0)),
        total_g,
        14.0,
        Align::Min,
        t.text_tertiary,
        0.0,
    );
    let mut on = s.loop_on;
    let mut b = toggle_button(Icon::Loop, None, &mut on, "Loop").at(pos2(r.right() - 30.0, y));
    if let Some(f) = forced.filter(|f| f.action == Action::Loop) {
        b = b.forced(f.vis, f.focus);
    }
    let lr = ui.add(b);
    menus::tooltip(&lr, "Loop", Some("L"), "");
    if lr.clicked() {
        action = Some(Action::Loop);
    }
    if forced.is_none()
        && ui.is_enabled()
        && !ui.ctx().text_edit_focused()
        && !egui::Popup::is_any_open(ui.ctx())
    {
        let shortcuts = [
            (Modifiers::NONE, Key::Space, Action::PlayPause),
            (Modifiers::SHIFT, Key::ArrowLeft, Action::PlayReverse),
            (Modifiers::SHIFT, Key::ArrowRight, Action::PlayForward),
            (Modifiers::NONE, Key::ArrowLeft, Action::StepBack),
            (Modifiers::NONE, Key::ArrowRight, Action::StepForward),
            (Modifiers::NONE, Key::Home, Action::First),
            (Modifiers::NONE, Key::End, Action::Last),
            (Modifiers::NONE, Key::I, Action::SetIn),
            (Modifiers::NONE, Key::O, Action::SetOut),
            (Modifiers::ALT, Key::I, Action::ClearInOut),
            (
                Modifiers::CTRL | Modifiers::SHIFT,
                Key::M,
                Action::ClearMarks,
            ),
            (Modifiers::SHIFT, Key::M, Action::Unmark),
            (Modifiers::NONE, Key::M, Action::Mark),
            (Modifiers::NONE, Key::L, Action::Loop),
        ];
        for (m, k, a) in shortcuts {
            if ui.input_mut(|i| i.consume_key(m, k)) {
                action = Some(a);
                break;
            }
        }
    }
    if let Some(a) = action {
        apply(s, a);
    }
    TransportOut { action }
}

fn apply(s: &mut TransportState, a: Action) {
    let last = s.last.max(s.first);
    match a {
        Action::First => s.frame = s.first,
        Action::Last => s.frame = last,
        Action::StepBack => s.frame = s.frame.saturating_sub(1).clamp(s.first, last),
        Action::StepForward => s.frame = s.frame.saturating_add(1).clamp(s.first, last),
        Action::Seek(f) => s.frame = f.clamp(s.first, last),
        Action::PlayPause => s.playing = !s.playing,
        Action::PlayReverse => {
            s.playing = true;
            s.reverse = true;
        }
        Action::PlayForward => {
            s.playing = true;
            s.reverse = false;
        }
        Action::Loop => s.loop_on = !s.loop_on,
        Action::Mark => {
            if !s.marks.contains(&s.frame) {
                s.marks.push(s.frame);
                s.marks.sort_unstable();
            }
        }
        Action::Unmark => s.marks.retain(|f| *f != s.frame),
        Action::ClearMarks => s.marks.clear(),
        Action::SetIn => {
            s.in_pt = Some(s.frame);
            if s.out_pt.is_some_and(|f| f < s.frame) {
                s.out_pt = Some(s.frame);
            }
        }
        Action::SetOut => {
            s.out_pt = Some(s.frame);
            if s.in_pt.is_some_and(|f| f > s.frame) {
                s.in_pt = Some(s.frame);
            }
        }
        Action::ClearInOut => {
            s.in_pt = None;
            s.out_pt = None;
        }
    }
}

pub struct TimelineData<'a> {
    pub first: i64,
    pub last: i64,
    pub frame: i64,
    pub in_pt: Option<i64>,
    pub out_pt: Option<i64>,
    pub marks: &'a [i64],
    /// Inclusive sequence-frame ranges.
    pub cached: &'a [(i64, i64)],
    pub fps: f64,
}
pub struct TimelineOut {
    pub scrub_to: Option<i64>,
}
fn frame_x(track: Rect, first: i64, last: i64, frame: i64) -> f32 {
    if last <= first {
        track.left()
    } else {
        track.left()
            + ((frame as f64 - first as f64) / (last as f64 - first as f64)).clamp(0.0, 1.0) as f32
                * track.width()
    }
}
fn scrub_frame(track: Rect, first: i64, last: i64, x: f32) -> i64 {
    if last <= first || track.width() <= 0.0 {
        first
    } else {
        (first as f64
            + ((x - track.left()) / track.width()).clamp(0.0, 1.0) as f64
                * (last as f64 - first as f64))
            .round() as i64
    }
}
fn clear_of_playhead(label: Rect, x: f32) -> bool {
    label.right() <= x - 5.0 || label.left() >= x + 5.0
}
pub fn timeline(ui: &mut Ui, d: &TimelineData<'_>) -> TimelineOut {
    timeline_forced(ui, d, false)
}
pub fn timeline_forced(ui: &mut Ui, d: &TimelineData<'_>, focus: bool) -> TimelineOut {
    let (r, response) =
        ui.allocate_exact_size(vec2(ui.available_width(), 28.0), Sense::click_and_drag());
    let t = tokens(ui);
    let p = ui.painter().with_clip_rect(r.intersect(ui.clip_rect()));
    p.rect_filled(r, 0, t.bg_panel);
    p.rect_filled(
        Rect::from_min_size(r.min, vec2(r.width(), 1.0)),
        0,
        t.border_divider,
    );
    let track = Rect::from_min_max(
        pos2(r.left() + 16.0, r.top() + 8.0),
        pos2((r.right() - 16.0).max(r.left() + 16.0), r.top() + 14.0),
    );
    p.rect_filled(track, 1, t.bg_sunken);
    p.rect_stroke(
        track,
        1,
        Stroke::new(1.0, t.border_default),
        StrokeKind::Inside,
    );
    let x = |f| frame_x(track, d.first, d.last, f).round();
    let ip = d.in_pt.unwrap_or(d.first);
    let op = d.out_pt.unwrap_or(d.last);
    p.rect_filled(
        Rect::from_min_max(
            pos2(x(ip), track.top()),
            pos2(x(op).max(x(ip)), track.bottom()),
        ),
        0,
        if t.bg_panel == DARK.bg_panel {
            t.stage_raw_chip_fill
        } else {
            egui::Color32::from_gray(0xD0)
        },
    );
    for f in [ip, op] {
        p.rect_filled(
            Rect::from_min_size(pos2(x(f) - 1.0, r.top() + 6.0), vec2(2.0, 10.0)),
            0,
            t.text_primary,
        );
    }
    for &(a, b) in d.cached {
        if b >= d.first && a <= d.last && b >= a {
            p.rect_filled(
                Rect::from_min_max(
                    pos2(x(a), r.top() + 15.0),
                    pos2((x(b) + 2.0).min(track.right()), r.top() + 17.0),
                ),
                0,
                t.success,
            );
        }
    }
    for &f in d.marks {
        if f >= d.first && f <= d.last {
            p.rect_filled(
                Rect::from_min_size(pos2(x(f) - 1.0, r.top() + 7.0), vec2(2.0, 8.0)),
                0,
                t.stage_mask_bar,
            );
        }
    }
    let ph = x(d.frame);
    let mut occupied: Vec<Rect> = Vec::new();
    let mut labels = vec![ip, op];
    // Never enumerate a huge sequence at a density that cannot fit a label.
    let stride = 10i64.saturating_mul(
        (((d.last as f64 - d.first as f64) / 10.0) / (track.width() as f64 / 24.0).max(1.0))
            .ceil()
            .max(1.0) as i64,
    );
    let mut f = d.first.div_euclid(stride).saturating_mul(stride);
    if f < d.first {
        f = f.saturating_add(stride);
    }
    while f <= d.last {
        labels.push(f);
        let Some(next) = f.checked_add(stride) else {
            break;
        };
        f = next;
    }
    for f in labels {
        if f < d.first || f > d.last {
            continue;
        }
        let g = line(&p, &f.to_string(), ty::mono(), 14.0, f32::INFINITY);
        let w = g.size().x;
        let left = (x(f) - w / 2.0).clamp(track.left(), (track.right() - w).max(track.left()));
        let lr = Rect::from_min_size(pos2(left, r.top() + 17.0), vec2(w, 11.0));
        if clear_of_playhead(lr, ph) && !occupied.iter().any(|o| o.expand(4.0).intersects(lr)) {
            paint_line(&p, lr, g, 14.0, Align::Min, t.text_tertiary, 0.0);
            occupied.push(lr);
        }
    }
    p.rect_filled(
        Rect::from_min_size(pos2(ph - 1.0, r.top() + 4.0), vec2(2.0, 18.0)),
        0,
        t.text_primary,
    );
    p.add(egui::Shape::convex_polygon(
        vec![
            pos2(ph - 4.5, r.top() + 2.0),
            pos2(ph + 4.5, r.top() + 2.0),
            pos2(ph, r.top() + 8.0),
        ],
        t.text_primary,
        Stroke::NONE,
    ));
    if focus || (response.has_focus() && keyboard_focus(ui.ctx())) {
        p.rect_stroke(r, 0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
    }
    response.widget_info(|| {
        WidgetInfo::slider(
            ui.is_enabled(),
            d.frame as f64,
            format!("Timeline, frame {}", d.frame),
        )
    });
    ui.ctx().accesskit_node_builder(response.id, |n| {
        n.set_min_numeric_value(d.first as f64);
        n.set_max_numeric_value(d.last as f64);
        n.set_numeric_value_step(1.0);
    });
    let mut scrub_to = if (response.clicked() || response.dragged()) && ui.is_enabled() {
        response
            .interact_pointer_pos()
            .map(|p| scrub_frame(track, d.first, d.last, p.x))
    } else {
        None
    };
    if response.has_focus() && ui.is_enabled() {
        for (key, f) in [
            (Key::ArrowLeft, d.frame.saturating_sub(1)),
            (Key::ArrowRight, d.frame.saturating_add(1)),
            (Key::Home, d.first),
            (Key::End, d.last),
        ] {
            if ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)) {
                scrub_to = Some(f.clamp(d.first, d.last.max(d.first)));
            }
        }
    }
    TimelineOut { scrub_to }
}
/// Separate 20 px meta strip: callers can keep the timeline itself exactly 28 px high.
pub fn timeline_meta(ui: &mut Ui, d: &TimelineData<'_>) -> Response {
    let (r, response) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::hover());
    let t = tokens(ui);
    ui.painter().rect_filled(r, 0, t.bg_panel);
    let text = format!(
        "Frame {} · In {} · Out {} · {} frames · {} fps · {} {}",
        d.frame,
        d.in_pt.unwrap_or(d.first),
        d.out_pt.unwrap_or(d.last),
        d.last.saturating_sub(d.first).saturating_add(1).max(0),
        d.fps,
        d.marks.len(),
        if d.marks.len() == 1 { "mark" } else { "marks" }
    );
    let g = line(ui.painter(), &text, ty::small(), 14.0, r.width() - 32.0);
    paint_line(
        ui.painter(),
        r.shrink2(vec2(16.0, 0.0)),
        g,
        14.0,
        Align::Min,
        t.text_tertiary,
        -1.0,
    );
    response.widget_info(|| WidgetInfo::labeled(Role::Label, ui.is_enabled(), text.as_str()));
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn frame_labels_keep_4px_clear_of_playhead() {
        let label = |x| Rect::from_min_size(pos2(x, 17.0), vec2(14.0, 11.0));
        assert!(!clear_of_playhead(label(32.0), 50.0));
        assert!(clear_of_playhead(label(31.0), 50.0));
        assert!(!clear_of_playhead(label(54.0), 50.0));
        assert!(clear_of_playhead(label(55.0), 50.0));
    }
    #[test]
    fn compact_moves_marks_and_inout_into_overflow_keeps_loop() {
        assert_eq!(side_actions(Layout::Compact), &[Action::Loop]);
        for a in [
            Action::Mark,
            Action::Unmark,
            Action::ClearMarks,
            Action::SetIn,
            Action::SetOut,
            Action::ClearInOut,
        ] {
            assert!(side_actions(Layout::Wide).contains(&a));
            assert!(!side_actions(Layout::Compact).contains(&a));
        }
    }
    #[test]
    fn scrub_maps_x_to_sequence_frame() {
        let r = Rect::from_min_size(pos2(16.0, 8.0), vec2(640.0, 6.0));
        assert_eq!(scrub_frame(r, 1001, 1065, 336.0), 1033);
        assert_eq!(scrub_frame(r, 1001, 1065, -10.0), 1001);
        assert_eq!(scrub_frame(r, 1001, 1065, 999.0), 1065);
        assert_eq!(scrub_frame(r, 1001, 1001, 336.0), 1001);
    }
    #[test]
    fn transport_actions_preserve_sequence_and_range() {
        let mut s = TransportState {
            first: 1001,
            last: 1065,
            frame: 1012,
            ..Default::default()
        };
        apply(&mut s, Action::Mark);
        apply(&mut s, Action::Mark);
        assert_eq!(s.marks, vec![1012]);
        apply(&mut s, Action::SetOut);
        apply(&mut s, Action::Last);
        apply(&mut s, Action::SetIn);
        assert_eq!(s.out_pt, Some(1065));
        apply(&mut s, Action::StepForward);
        assert_eq!(s.frame, 1065);
    }
}
