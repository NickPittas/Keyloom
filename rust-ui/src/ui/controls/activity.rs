//! Activity popover and its items (bible §8.7, §11 failure acknowledgement, §10.2 popover focus, §12). 480 wide, tabs
//! All · Jobs · Notices, one scrolling list of 44 px items. `activity_popover` is the live (modal-layer) entry;
//! `show_in` paints the same popover into a rect for the specimen and the tests, with forced hover / scroll.
use super::{
    buttons::{self, IconButtonSize},
    chips::{self, Progress, ProgressSlot},
    dialog::empty_state,
    keyboard_focus, line, paint_line, scroll,
    tabs::{TabKind, TabStrip, Tab},
};
use crate::ui::{icons::{self, Icon, IconSize}, tokens::tokens, type_scale as ty};
use eframe::egui::{self, accesskit::Role, pos2, vec2, Align, Color32, Context, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, WidgetInfo};
use std::collections::HashSet;

pub const WIDTH: f32 = 480.0;
pub const MAX_HEIGHT: f32 = 420.0;
const ITEM_H: f32 = 44.0;
const HEADER_H: f32 = 28.0;
const TABS_H: f32 = 24.0;
/// Time slot maximum (§8.7).
const TIME_MAX: f32 = 72.0;
/// Tools (max 2 × 18 + 2) plus their 8 px inset: line 2 never runs under them.
const TOOLS_CLEAR: f32 = 46.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum JobState {
    Queued,
    /// `None` = no percentage yet (preparing).
    Running(Option<f32>),
    Done,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NoticeLevel { Info, Warning }
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ItemKind { Job(JobState), Notice(NoticeLevel) }
/// Hover tools, in the order they are drawn (left to right).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action { Cancel, Retry, Reveal, Dismiss }
impl Action {
    fn icon(self) -> Icon { match self { Self::Cancel => Icon::Cancel, Self::Retry => Icon::Retry, Self::Reveal => Icon::Reveal, Self::Dismiss => Icon::Dismiss } }
    fn tip(self) -> &'static str { match self { Self::Cancel => "Cancel", Self::Retry => "Retry", Self::Reveal => "Reveal", Self::Dismiss => "Dismiss" } }
}

pub struct ActivityItem<'a> {
    /// Stable id (acknowledgement bookkeeping, action results).
    pub id: u64,
    pub kind: ItemKind,
    /// Line 1; the caller has already shortened the shot name (§8.1). End-truncated here, tooltip = full.
    pub title: &'a str,
    /// Line 2: "endor-omarchy · frame 22 / 65".
    pub detail: &'a str,
    /// "now", "2 min ago", "Yesterday", "14:02"; empty = no time.
    pub time: &'a str,
    pub actions: &'a [Action],
}

#[derive(Default, Clone)]
pub struct ActivityState {
    /// 0 All · 1 Jobs · 2 Notices.
    pub tab: usize,
    pub reduced_motion: bool,
    acked: HashSet<u64>,
}
impl ActivityState {
    /// Status bar "Notices" segment opens the popover on Notices.
    pub fn open_on(tab: usize) -> Self { Self { tab: tab.min(2), ..Self::default() } }
    pub fn is_acknowledged(&self, id: u64) -> bool { self.acked.contains(&id) }
    /// §11 (b) Dismiss and (c) a new job for that shot starts.
    pub fn acknowledge(&mut self, id: u64) { self.acked.insert(id); }
    /// The red count of the status bar's activity segment.
    pub fn unacknowledged_failures(&self, items: &[ActivityItem]) -> usize {
        items.iter().filter(|i| matches!(i.kind, ItemKind::Job(JobState::Failed)) && !self.acked.contains(&i.id)).count()
    }
}

#[derive(Default)]
pub struct ActivityOut {
    pub closed: bool,
    /// A hover tool was pressed on the item with this id.
    pub action: Option<(u64, Action)>,
    /// Failures acknowledged this frame by showing them (§11 a).
    pub acknowledged: Vec<u64>,
}

/// Specimen / test only: pointer and scroll state drawn through the real paint path.
#[derive(Default, Clone, Copy)]
pub struct Forced {
    /// Index (in the filtered list) of the hovered item.
    pub hover: Option<usize>,
    pub scroll: Option<f32>,
    /// Still spinner arc.
    pub reduced_motion: bool,
}

/// §11 (a): the tab is All or Jobs, the item is a failed job and its row is at least half inside the view.
pub fn acknowledges(tab: usize, kind: ItemKind, row: Rect, view: Rect) -> bool {
    tab != 2 && matches!(kind, ItemKind::Job(JobState::Failed)) && row.intersect(view).height() >= row.height() / 2.0
}

fn matches_tab(tab: usize, kind: ItemKind) -> bool {
    match tab { 1 => matches!(kind, ItemKind::Job(_)), 2 => matches!(kind, ItemKind::Notice(_)), _ => true }
}

/// Where each part of one item sits (item rect = 44 high, any width).
pub struct RowLayout { pub icon: Rect, pub title: Rect, pub time: Rect, pub line2: Rect, pub progress: Rect, pub tools: Option<Rect> }
pub fn row_layout(row: Rect, time_w: f32, n_tools: usize, show_tools: bool) -> RowLayout {
    let col_x = row.left() + 36.0;
    let col_w = (row.width() - 36.0 - 12.0).max(0.0);
    let time_w = time_w.min(TIME_MAX);
    let title_w = if time_w > 0.0 { col_w - 8.0 - time_w } else { col_w };
    let tools = (show_tools && n_tools > 0).then(|| {
        let w = n_tools as f32 * 18.0 + (n_tools - 1) as f32 * 2.0;
        Rect::from_min_size(pos2(row.right() - 8.0 - w, row.top() + ITEM_H - 4.0 - 18.0), vec2(w, 18.0))
    });
    RowLayout {
        icon: Rect::from_min_size(pos2(row.left() + 12.0, row.top() + 7.0), vec2(16.0, 16.0)),
        title: Rect::from_min_size(pos2(col_x, row.top() + 7.0), vec2(title_w.max(0.0), 16.0)),
        time: Rect::from_min_size(pos2(col_x + col_w - time_w, row.top() + 7.0), vec2(time_w, 16.0)),
        line2: Rect::from_min_size(pos2(col_x, row.top() + 23.0), vec2((col_w - if show_tools { TOOLS_CLEAR } else { 0.0 }).max(0.0), 14.0)),
        progress: Rect::from_min_size(pos2(col_x, row.top() + ITEM_H - 3.0 - 3.0), vec2(col_w, 3.0)),
        tools,
    }
}

/// Live popover: a modal layer (focus trap, Esc, click outside) anchored above `anchor` (the status bar) at its left.
/// Call only while open; `out.closed` says to stop calling. Focus returns to the control that opened it (§10.2).
pub fn activity_popover(ctx: &Context, anchor: Rect, state: &mut ActivityState, items: &[ActivityItem]) -> ActivityOut {
    let id = Id::NULL.with("keyloom-activity-popover");
    let (key, frame) = (id.with("open"), ctx.cumulative_frame_nr());
    let prev = ctx.data(|d| d.get_temp::<(u64, Option<Id>)>(key));
    let fresh = prev.is_none_or(|(f, _)| f + 1 < frame);
    let opener = if fresh { ctx.memory(|m| m.focused()) } else { prev.and_then(|p| p.1) };
    ctx.data_mut(|d| d.insert_temp(key, (frame, opener)));
    let screen = ctx.content_rect();
    let h = (anchor.top() - screen.top() - 16.0).clamp(120.0, MAX_HEIGHT);
    let x = anchor.left().min(screen.right() - WIDTH).max(screen.left());
    let rect = Rect::from_min_size(pos2(x, anchor.top() - h), vec2(WIDTH, h));
    let area = egui::Area::new(id).kind(egui::UiKind::Modal).sense(Sense::hover()).fixed_pos(rect.min).order(egui::Order::Foreground).interactable(true).fade_in(false);
    let modal = egui::Modal::new(id).area(area).backdrop_color(Color32::TRANSPARENT).frame(egui::Frame::NONE).show(ctx, |ui| {
        paint(ui, rect, state, items, None, fresh)
    });
    // Status-bar trigger clicks are routed by the caller; the modal must not also treat them as outside dismissals.
    let trigger_clicked = ctx.input(|i| i.pointer.any_released() && i.pointer.interact_pos().is_some_and(|p| anchor.contains(p)));
    let should_close = !fresh && !trigger_clicked && modal.should_close();
    let mut out = modal.inner;
    if should_close { out.closed = true }
    if out.closed {
        ctx.data_mut(|d| d.remove::<(u64, Option<Id>)>(key));
        if let Some(o) = opener { ctx.memory_mut(|m| m.request_focus(o)) }
    }
    out
}

/// The same popover painted into `rect` (480 × any height) without a modal layer.
pub fn show_in(ui: &mut Ui, rect: Rect, state: &mut ActivityState, items: &[ActivityItem], forced: Forced) -> ActivityOut {
    paint(ui, rect, state, items, Some(forced), false)
}

fn paint(ui: &mut Ui, rect: Rect, state: &mut ActivityState, items: &[ActivityItem], forced: Option<Forced>, fresh: bool) -> ActivityOut {
    let t = tokens(ui);
    let mut out = ActivityOut::default();
    let shadow = egui::epaint::Shadow { offset: [0, 8], blur: 24, spread: 0, color: Color32::from_black_alpha(if ui.visuals().dark_mode { 128 } else { 51 }) };
    ui.painter().add(egui::Frame::new().fill(t.bg_overlay).corner_radius(4).shadow(shadow).paint(rect));
    let mut ui = ui.new_child(UiBuilder::new().id_salt(("activity", rect.min.x.to_bits(), rect.min.y.to_bits())).max_rect(rect));
    ui.set_clip_rect(rect.intersect(ui.clip_rect()));
    ui.visuals_mut().panel_fill = t.bg_overlay;
    ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
    let reduced = forced.map_or(state.reduced_motion, |f| f.reduced_motion);

    // Header 28: title 12 SemiBold at 12 pad, close icon button 22 at right 4.
    let head = Rect::from_min_size(rect.min + vec2(1.0, 1.0), vec2(rect.width() - 2.0, HEADER_H));
    paint_line(ui.painter(), Rect::from_min_max(head.min + vec2(12.0, 0.0), head.max - vec2(30.0, 0.0)), line(ui.painter(), "Activity", ty::panel_header(), 16.0, 400.0), 16.0, Align::Min, t.text_primary, 0.0);
    let close = ui.add(buttons::icon_button(Icon::Close, IconButtonSize::Normal22, "Close Activity").at(pos2(rect.right() - 27.0, rect.top() + 4.0)));
    if close.clicked() { out.closed = true }
    if fresh { ui.memory_mut(|m| m.request_focus(close.id)) }

    // Panel tabs 24 on a bg.app strip with the 1 px divider underneath.
    let strip = Rect::from_min_size(pos2(head.left(), head.bottom()), vec2(head.width(), TABS_H));
    ui.painter().rect_filled(strip, 0.0, t.bg_app);
    ui.painter().rect_filled(Rect::from_min_max(pos2(strip.left(), strip.bottom() - 1.0), strip.max), 0.0, t.border_divider);
    let notices = items.iter().filter(|i| matches!(i.kind, ItemKind::Notice(_))).count() as u32;
    let tabs = [Tab::new("All"), Tab::new("Jobs"), if notices > 0 { Tab::new("Notices").badge(notices) } else { Tab::new("Notices") }];
    TabStrip::new(TabKind::Panel, &mut state.tab, &tabs).name("Activity").at(strip.min).show(&mut ui);
    let tab = state.tab.min(2);

    // Body: the list, or an empty state.
    let body = Rect::from_min_max(pos2(head.left(), strip.bottom()), pos2(head.right(), rect.bottom() - 1.0));
    let mut bodyui = ui.new_child(UiBuilder::new().id_salt("body").max_rect(body).layout(egui::Layout::top_down(Align::Min)));
    bodyui.set_clip_rect(body.intersect(ui.clip_rect()));
    let shown: Vec<&ActivityItem> = items.iter().filter(|i| matches_tab(tab, i.kind)).collect();
    if shown.is_empty() {
        let (title, text) = match tab { 1 => ("No jobs", "Jobs you start appear here."), 2 => ("No notices", "App notices appear here."), _ => ("No activity", "Jobs and notices appear here.") };
        empty_state(&mut bodyui, Icon::Activity, title, text, None);
    } else {
        let acked = &mut state.acked;
        let list = |ui: &mut Ui| {
            ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
            for (n, item) in shown.iter().enumerate() {
                if let Some(a) = row(ui, item, n, forced, reduced, tab, acked, &mut out.acknowledged) { out.action = Some((item.id, a)) }
            }
        };
        match forced.and_then(|f| f.scroll) {
            Some(offset) => { scroll::scroll_v_forced(&mut bodyui, "activity-list", scroll::State::Rest, offset, list); }
            None => { scroll::scroll_v(&mut bodyui, "activity-list", list); }
        }
    }
    if let Some((id, Action::Dismiss)) = out.action { state.acknowledge(id) }

    ui.painter().rect_stroke(rect, 4.0, Stroke::new(1.0, t.border_overlay), StrokeKind::Inside);
    out
}

/// One item. Returns the pressed tool.
#[allow(clippy::too_many_arguments)]
fn row(ui: &mut Ui, item: &ActivityItem, n: usize, forced: Option<Forced>, reduced: bool, tab: usize, acked: &mut HashSet<u64>, newly: &mut Vec<u64>) -> Option<Action> {
    let t = tokens(ui);
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), ITEM_H), Sense::focusable_noninteractive());
    if acknowledges(tab, item.kind, rect, ui.clip_rect()) && acked.insert(item.id) { newly.push(item.id) }
    let tf_key = resp.id.with("tool-focus");
    let tool_focus = ui.data(|d| d.get_temp::<bool>(tf_key)).unwrap_or(false);
    let kb = resp.has_focus() && keyboard_focus(ui.ctx());
    let hovered = match forced { Some(f) => f.hover == Some(n), None => ui.rect_contains_pointer(rect.intersect(ui.clip_rect())) };
    let show_tools = !item.actions.is_empty() && (hovered || kb || tool_focus);
    let painter = ui.painter().clone();
    let time_g = (!item.time.is_empty()).then(|| line(&painter, item.time, ty::small(), 16.0, TIME_MAX));
    let lay = row_layout(rect, time_g.as_ref().map_or(0.0, |g| g.size().x), item.actions.len(), show_tools);

    if hovered { painter.rect_filled(rect, 0.0, t.bg_hover); }
    if n > 0 { painter.rect_filled(Rect::from_min_size(rect.min, vec2(rect.width(), 1.0)), 0.0, t.border_overlay); }

    let (icon, color) = match item.kind {
        ItemKind::Job(JobState::Running(_)) => (None, t.busy_fill),
        ItemKind::Job(JobState::Queued) => (Some(Icon::Clock), t.busy_queued),
        ItemKind::Job(JobState::Done) => (Some(Icon::CheckCircle), t.success),
        ItemKind::Job(JobState::Failed) => (Some(Icon::Error), t.error),
        ItemKind::Notice(NoticeLevel::Info) => (Some(Icon::Info), t.info),
        ItemKind::Notice(NoticeLevel::Warning) => (Some(Icon::Warning), t.warning),
    };
    match icon {
        Some(ic) => icons::paint(&painter, lay.icon, ic, IconSize::S16, color),
        None => buttons::spinner(ui, lay.icon, color, reduced),
    }
    let tg = line(&painter, item.title, ty::label(), 16.0, lay.title.width());
    let elided = tg.elided;
    paint_line(&painter, lay.title, tg, 16.0, Align::Min, t.text_primary, 0.0);
    if let Some(g) = time_g { paint_line(&painter, lay.time, g, 16.0, Align::Max, if hovered { t.text_secondary } else { t.text_tertiary }, 0.0) }
    paint_line(&painter, lay.line2, line(&painter, item.detail, ty::small(), 14.0, lay.line2.width()), 14.0, Align::Min, t.text_secondary, 0.0);
    if let ItemKind::Job(JobState::Running(p)) = item.kind {
        chips::progress(ui, ProgressSlot::Rect(lay.progress), Progress { value: p, state: chips::State::Running, thin: true, label: item.title });
    }
    if elided {
        let title_hit = ui.interact(lay.title, resp.id.with("title"), Sense::hover());
        let _ = title_hit.on_hover_text(item.title);
    }
    let (role, notice) = match item.kind { ItemKind::Notice(_) => (Role::Status, true), _ => (Role::ListItem, false) };
    let name = format!("{}{}{}", item.title, if item.detail.is_empty() { String::new() } else { format!(", {}", item.detail) }, if item.time.is_empty() { String::new() } else { format!(", {}", item.time) });
    resp.widget_info(|| WidgetInfo::labeled(role, true, name.as_str()));
    let _ = notice;

    let mut pressed = None;
    let mut any_focus = false;
    if let Some(tools) = lay.tools {
        painter.rect_filled(Rect::from_min_max(tools.min - vec2(2.0, 0.0), tools.max), 0.0, t.bg_hover);
        for (i, a) in item.actions.iter().enumerate() {
            let r = tool_button(ui, *a, pos2(tools.left() + i as f32 * 20.0, tools.top()));
            any_focus |= r.has_focus();
            if r.clicked() { pressed = Some(*a) }
        }
    }
    ui.data_mut(|d| d.insert_temp(tf_key, any_focus));
    if kb { painter.rect_stroke(rect, 0.0, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside); }
    pressed
}

/// 18 px ghost icon button for a hover tool. `dismiss` has no 14 px drawing (bible §9 lists none), so it is painted
/// here at 16 inside the 18 box from the same `look` table; the others are the shared small icon button.
fn tool_button(ui: &mut Ui, a: Action, pos: egui::Pos2) -> egui::Response {
    if a != Action::Dismiss { return ui.add(buttons::icon_button(a.icon(), IconButtonSize::Small18, a.tip()).at(pos)) }
    let t = tokens(ui);
    let rect = Rect::from_min_size(pos, vec2(18.0, 18.0));
    let resp = ui.interact(rect, ui.scope_id().with(("dismiss-tool", pos.x as i32, pos.y as i32)), Sense::click());
    let vis = if resp.is_pointer_button_down_on() { buttons::Vis::Pressed } else if resp.hovered() { buttons::Vis::Hover } else { buttons::Vis::Rest };
    let l = buttons::look(t, buttons::Kind::Icon, vis, false, true);
    let (p, r) = (ui.painter(), egui::CornerRadius::same(3));
    if let Some(f) = l.fill { p.rect_filled(rect, r, f); }
    if resp.has_focus() && keyboard_focus(ui.ctx()) {
        p.rect_stroke(rect, r, Stroke::new(2.0, t.focus_ring), StrokeKind::Inside);
    } else if let Some(b) = l.border {
        p.rect_stroke(rect, r, Stroke::new(1.0, b), StrokeKind::Inside);
    }
    icons::paint(p, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), Icon::Dismiss, IconSize::S16, l.icon);
    resp.widget_info(|| WidgetInfo::labeled(Role::Button, true, a.tip()));
    resp.on_hover_text(a.tip())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(tab_failed_first: bool) -> Vec<ActivityItem<'static>> {
        let mut v = vec![ActivityItem { id: 1, kind: ItemKind::Job(JobState::Failed), title: "Keying failed", detail: "render-node-07", time: "14:02", actions: &[Action::Retry, Action::Dismiss] }];
        for i in 2..9 { v.push(ActivityItem { id: i, kind: ItemKind::Job(JobState::Done), title: "Exported", detail: "This machine", time: "13:12", actions: &[Action::Reveal] }); }
        v.push(ActivityItem { id: 20, kind: ItemKind::Notice(NoticeLevel::Info), title: "No system tray", detail: "Notice", time: "13:40", actions: &[Action::Dismiss] });
        if !tab_failed_first { v.rotate_left(1) }
        v
    }

    /// Runs three frames of the popover at a forced scroll offset; returns the state afterwards.
    fn run(tab: usize, offset: f32, list: &[ActivityItem]) -> ActivityState {
        let ctx = egui::Context::default();
        let mut state = ActivityState { tab, ..Default::default() };
        for k in 0..3 {
            let mut o = ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(600.0, 500.0))), time: Some(k as f64 * 0.1), ..Default::default() }, |root| {
                egui::CentralPanel::default().show(root, |ui| {
                    show_in(ui, Rect::from_min_size(pos2(10.0, 10.0), vec2(WIDTH, 230.0)), &mut state, list, Forced { scroll: Some(offset), ..Default::default() });
                });
            });
            o.textures_delta.clear();
        }
        state
    }

    #[test]
    fn failure_acknowledged_only_when_half_visible_on_all_or_jobs() {
        let list = items(true);
        // Item 1 is the first 44 px row of a 176 px list body; scrolled by 23 only 21 px (< half) remain.
        for tab in [0, 1] {
            assert!(!run(tab, 23.0, &list).is_acknowledged(1), "tab {tab}: under half visible");
            assert!(run(tab, 22.0, &list).is_acknowledged(1), "tab {tab}: exactly half visible");
            assert!(run(tab, 0.0, &list).is_acknowledged(1), "tab {tab}: fully visible");
        }
        assert_eq!(run(0, 0.0, &list).unacknowledged_failures(&list), 0);
        assert_eq!(ActivityState::default().unacknowledged_failures(&list), 1);
        let mut s = ActivityState::default();
        s.acknowledge(1);
        assert_eq!(s.unacknowledged_failures(&list), 0);
    }

    #[test]
    fn notices_tab_does_not_acknowledge_jobs() {
        let list = items(true);
        let s = run(2, 0.0, &list);
        assert!(!s.is_acknowledged(1));
        let row = Rect::from_min_size(pos2(0.0, 0.0), vec2(478.0, 44.0));
        assert!(!acknowledges(2, ItemKind::Job(JobState::Failed), row, row));
        assert!(acknowledges(0, ItemKind::Job(JobState::Failed), row, row));
        assert!(!acknowledges(0, ItemKind::Job(JobState::Done), row, row));
    }

    #[test]
    fn long_title_and_time_never_collide() {
        let ctx = egui::Context::default();
        let mut o = ctx.run_ui(egui::RawInput::default(), |root| {
            egui::CentralPanel::default().show(root, |ui| {
                let p = ui.painter().clone();
                for w in [478.0, 470.0] {
                    for time in ["Yesterday", "14:02", "2 min ago", "Yesterday afternoon at 14:02:33", ""] {
                        let row = Rect::from_min_size(pos2(0.0, 0.0), vec2(w, ITEM_H));
                        let tg = line(&p, time, ty::small(), 16.0, TIME_MAX);
                        let lay = row_layout(row, if time.is_empty() { 0.0 } else { tg.size().x }, 2, true);
                        assert!(lay.time.width() <= TIME_MAX);
                        let title = line(&p, "Keying V1-0046_A002C008_261012_R4LF_interview_wide", ty::label(), 16.0, lay.title.width());
                        assert!(title.size().x <= lay.title.width() + 0.01);
                        if !time.is_empty() { assert!(lay.title.right() + 8.0 <= lay.time.left() + 0.01, "{time}: title {} vs time {}", lay.title.right(), lay.time.left()); }
                        let tools = lay.tools.unwrap();
                        // Line 1 ink ends 1 px above the tools; line 2 stops 46 px before the right edge.
                        assert!(tools.top() >= lay.title.bottom() - 1.0 && tools.top() >= lay.time.bottom() - 1.0);
                        assert!(lay.line2.right() <= tools.left() - 8.0 + 0.01 || tools.width() < 38.0);
                        assert!(tools.right() <= row.right() - 8.0 + 0.01 && lay.title.left() >= lay.icon.right() + 8.0);
                    }
                }
            });
        });
        o.textures_delta.clear();
    }
}
