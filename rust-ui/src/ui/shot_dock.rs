use super::{part, Fixture};
use crate::ui::{
    controls::{
        buttons::{icon_button, secondary_button, IconButtonSize},
        menus::{MenuItem, MenuList},
        node::{graph_canvas, GraphView, NodeKind, NodeSpec, NodeState, Wire},
        tabs::{Tab, TabKind, TabStrip},
        line, paint_line, LABEL_DX,
    },
    icons::{self, Icon, IconSize},
    tokens::tokens,
    type_scale as ty,
};
use eframe::egui::{self, pos2, vec2, Align2, Area, Id, Order, Rect, Sense, Stroke, StrokeKind, Ui};

const CARD: egui::Vec2 = vec2(280.0, 96.0);
const CARD_GAP: f32 = 8.0;
const COMPACT_GRAPH_BODY: f32 = 263.0;
const ZOOM_STEP: f32 = 1.25;

/// What the dock reports to the Shot page this frame. The page keeps the tab and expanded state.
#[derive(Default)]
pub(super) struct DockOut {
    /// A tab was clicked.
    pub tab: Option<DockTab>,
    /// The expand/collapse button was clicked; the value is the requested state.
    pub expand: Option<bool>,
    /// A card button or menu item was clicked: (card id, action).
    pub card_action: Option<(String, CardAction)>,
    /// Live mode, Node graph tab: the rect where the App draws the real graph.
    pub graph_body: Option<Rect>,
    pub graph_fit: bool,
    /// One click's zoom factor: ZOOM_STEP in, 1 / ZOOM_STEP out.
    pub graph_zoom: Option<f32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum DockTab {
    Actions,
    Graph,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum CardAction {
    Cancel,
    Reveal,
    Menu(String),
}

/// `body_rect` is None when the dock has no body (compact, collapsed); a bare Rect cannot express that.
pub(super) fn draw(ui: &mut Ui, tabs_rect: Rect, body_rect: Option<Rect>, fx: &Fixture) -> DockOut {
    let mut out = DockOut::default();
    let compact = ui.ctx().content_rect().width() < 1200.0;
    if fx.dock_expanded {
        if compact {
            let width = if fx.drawer { tabs_rect.width() } else { ui.ctx().content_rect().width() };
            let tabs = Rect::from_min_size(pos2(tabs_rect.left(), ui.ctx().content_rect().top() + 120.0), vec2(width, tabs_rect.height()));
            let body = Rect::from_min_size(pos2(tabs.left(), tabs.bottom()), vec2(width, COMPACT_GRAPH_BODY));
            overlay(ui, tabs, body, fx, &mut out);
            return out;
        }
        if let Some(body) = body_rect {
            let expanded_h = ((ui.ctx().content_rect().height() - 114.0) / 2.0).max(body.height());
            if expanded_h > body.height() + 0.5 {
                let lift = expanded_h - body.height();
                let tabs = tabs_rect.translate(vec2(0.0, -lift));
                let body = Rect::from_min_size(pos2(body.left(), body.top() - lift), vec2(body.width(), expanded_h));
                overlay(ui, tabs, body, fx, &mut out);
                return out;
            }
        }
    }
    panel(ui, tabs_rect, body_rect, fx, compact && !fx.dock_expanded, &mut out);
    out
}

fn overlay(ui: &mut Ui, tabs: Rect, body: Rect, fx: &Fixture, out: &mut DockOut) {
    let bounds = Rect::from_min_max(tabs.min, body.max);
    Area::new(Id::unique("shot-dock-expanded"))
        .order(Order::Foreground)
        .fixed_pos(bounds.min)
        .interactable(true)
        .show(ui.ctx(), |area| {
            area.allocate_rect(bounds, Sense::hover());
            panel(area, tabs, Some(body), fx, false, out);
        });
}

fn panel(ui: &mut Ui, tabs_rect: Rect, body_rect: Option<Rect>, fx: &Fixture, collapsed_compact: bool, out: &mut DockOut) {
    let t = tokens(ui);
    let p = ui.painter().with_clip_rect(tabs_rect);
    p.rect_filled(tabs_rect, 0.0, t.bg_app);
    p.rect_filled(Rect::from_min_size(pos2(tabs_rect.left(), tabs_rect.bottom() - 1.0), vec2(tabs_rect.width(), 1.0)), 0.0, t.border_divider);
    if let Some(body) = body_rect { ui.painter().rect_filled(body, 0.0, t.bg_panel); }

    let tabs = [
        Tab::new("Actions").tip("Results and job actions"),
        Tab::new("Node graph").tip("Drag to pan · wheel to zoom"),
    ];
    let mut active = if collapsed_compact { tabs.len() } else { usize::from(fx.dock_tab == "Node graph") };
    let before = active;
    let mut strip = part(ui, tabs_rect, "dock-tabs");
    TabStrip::new(TabKind::Panel, &mut active, &tabs).name("Shot dock").show(&mut strip);
    if active != before { out.tab = Some(if active == 0 { DockTab::Actions } else { DockTab::Graph }); }
    panel_labels(ui, tabs_rect, active);
    if active == 1 { graph_tools(&mut strip, tabs_rect, fx.dock_expanded, out); }

    if let Some(body) = body_rect {
        if active == 1 {
            if fx.live {
                out.graph_body = Some(body);
            } else {
                let nodes = graph_nodes(fx);
                let wires = graph_wires();
                let mut graph = part(ui, body, "dock-graph");
                graph_canvas(&mut graph, "shot-dock-graph", &nodes, &wires, &mut GraphView::default());
            }
        } else {
            let cards = if fx.live { live_cards(fx) } else { fixture_cards(fx) };
            draw_actions(ui, body, &cards, out);
        }
    }
}

fn panel_labels(ui: &Ui, tabs: Rect, active: usize) {
    let t = tokens(ui);
    let p = ui.painter().with_clip_rect(tabs);
    let mut x = tabs.left();
    for (i, label) in ["Actions", "Node graph"].into_iter().enumerate() {
        let galley = line(&p, label, ty::button(), 16.0, f32::INFINITY);
        let width = galley.size().x;
        let left = x + 12.0;
        let back = if i == active { t.bg_panel } else { t.bg_app };
        p.rect_filled(Rect::from_min_max(pos2(left - 1.0, tabs.top() + 3.0), pos2(left + width + 1.0, tabs.bottom() - 3.0)), 0.0, back);
        paint_line(&p, Rect::from_min_max(pos2(left + LABEL_DX, tabs.top() - 1.0), pos2(left + width + 1.0, tabs.bottom() - 1.0)), galley, 16.0, egui::Align::Min, if i == active { t.text_primary } else { t.text_secondary }, 0.0);
        x += width + 25.0;
    }
}

fn graph_tools(ui: &mut Ui, tabs: Rect, expanded: bool, out: &mut DockOut) {
    let y = tabs.top() + (tabs.height() - 18.0) / 2.0;
    let right = tabs.right() - 8.0;
    let expand = Rect::from_min_size(pos2(right - 18.0, y), vec2(18.0, 18.0));
    let plus = Rect::from_min_size(pos2(expand.left() - 22.0, y), vec2(18.0, 18.0));
    let percent = Rect::from_min_size(pos2(plus.left() - 36.0, y), vec2(32.0, 18.0));
    let minus = Rect::from_min_size(pos2(percent.left() - 22.0, y), vec2(18.0, 18.0));
    let fit = Rect::from_min_size(pos2(minus.left() - 22.0, y), vec2(18.0, 18.0));
    if ui.add(icon_button(Icon::Fit, IconButtonSize::Normal22, "Fit graph").in_rect(fit)).clicked() { out.graph_fit = true; }
    if ui.add(icon_button(Icon::Minus, IconButtonSize::Normal22, "Zoom out").in_rect(minus)).clicked() { out.graph_zoom = Some(1.0 / ZOOM_STEP); }
    ui.painter().with_clip_rect(tabs).text(percent.center(), Align2::CENTER_CENTER, "100%", ty::mono(), tokens(ui).text_secondary);
    if ui.add(icon_button(Icon::Plus, IconButtonSize::Normal22, "Zoom in").in_rect(plus)).clicked() { out.graph_zoom = Some(ZOOM_STEP); }
    let (icon, tip) = if expanded { (Icon::Collapse, "Collapse bottom panel") } else { (Icon::Expand, "Expand bottom panel") };
    if ui.add(icon_button(icon, IconButtonSize::Normal22, tip).in_rect(expand)).clicked() { out.expand = Some(!expanded); }
}

#[derive(Clone, Copy)]
struct CardSpec {
    icon: Icon,
    title: &'static str,
    meta: &'static str,
    actions: bool,
}

/// One card as the dock draws it; fixture and live cards both become this.
struct CardView {
    id: String,
    icon: Icon,
    title: String,
    meta: String,
    actions: bool,
    busy: bool,
    /// Shown as the disabled reason when `busy`.
    busy_reason: &'static str,
    /// Adds a "Cancel job" menu item (disabled unless `busy`).
    cancel: bool,
}

fn action_cards(fx: &Fixture) -> [CardSpec; 3] {
    [
        CardSpec { icon: Icon::Hint, title: "Pre-matte", meta: "65 frames", actions: true },
        CardSpec { icon: Icon::Hint, title: "Hint", meta: "65 frames", actions: true },
        CardSpec {
            icon: Icon::Key,
            title: "Key result",
            meta: if fx.empty { "No result yet" } else if fx.stage == "Key" && fx.busy_percent.is_some() { "Retained · 65 of 65 frames · endor-omarchy" } else { "65 of 65 frames · endor-omarchy" },
            actions: !fx.empty,
        },
    ]
}

fn fixture_cards(fx: &Fixture) -> Vec<CardView> {
    action_cards(fx).into_iter().enumerate().map(|(i, c)| CardView {
        id: i.to_string(),
        icon: c.icon,
        title: c.title.into(),
        meta: c.meta.into(),
        actions: c.actions,
        busy: fx.busy_percent.is_some(),
        busy_reason: "Keying in progress",
        cancel: false,
    }).collect()
}

/// Ponytail: the Key icon is picked by `kind == "key"`; other kinds get Hint. Revisit when Card gains a real kind enum.
fn live_cards(fx: &Fixture) -> Vec<CardView> {
    fx.cards.iter().enumerate().map(|(i, c)| CardView {
        id: i.to_string(),
        icon: if c.kind == "key" { Icon::Key } else { Icon::Hint },
        title: c.title.clone(),
        meta: match c.percent {
            Some(p) => format!("{} · {p}%", c.meta),
            None => c.meta.clone(),
        },
        actions: true,
        busy: c.percent.is_some(),
        busy_reason: "Job in progress",
        cancel: true,
    }).collect()
}

fn draw_actions(ui: &mut Ui, body: Rect, cards: &[CardView], out: &mut DockOut) {
    let t = tokens(ui);
    let clip = ui.painter().with_clip_rect(body);
    for (i, card) in cards.iter().enumerate() {
        let rect = Rect::from_min_size(body.min + vec2(8.0 + i as f32 * (CARD.x + CARD_GAP), 8.0), CARD);
        clip.rect_stroke(rect, 4.0, Stroke::new(1.0, t.border_default), StrokeKind::Inside);
        icons::paint(ui.painter(), Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 12.0), vec2(16.0, 16.0)), card.icon, IconSize::S16, t.text_primary);
        let title = Rect::from_min_size(pos2(rect.left() + 32.0, rect.top() + 8.0), vec2(CARD.x - 40.0, 24.0));
        paint_line(ui.painter(), title, line(ui.painter(), &card.title, ty::button(), 16.0, title.width()), 16.0, egui::Align::Min, t.text_primary, -0.125);
        let meta = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 32.0), vec2(CARD.x - 16.0, 20.0));
        paint_line(&clip, meta, line(&clip, &card.meta, ty::small(), 14.0, meta.width()), 14.0, egui::Align::Min, t.text_secondary, 0.0);
        if !card.actions { continue; }

        let reveal = Rect::from_min_size(pos2(rect.left() + 8.0, rect.top() + 64.0), vec2(64.0, 22.0));
        let more = Rect::from_min_size(pos2(rect.right() - 8.0 - 22.0, rect.top() + 64.0), vec2(22.0, 22.0));
        if ui.add(secondary_button("Reveal").width(64.0).height(22.0).tooltip("Reveal folder\nOpen the result folder").in_rect(reveal)).clicked() {
            out.card_action = Some((card.id.clone(), CardAction::Reveal));
        }

        let busy = card.busy;
        let reason = card.busy_reason;
        let mut more_button = icon_button(Icon::More, IconButtonSize::Normal22, "More actions").in_rect(more);
        if busy { more_button = more_button.disabled(reason); }
        let response = ui.add(more_button);
        let menu_id = Id::unique(("shot-dock-card-menu", i));
        let was_open = ui.ctx().data(|d| d.get_temp::<bool>(menu_id)).unwrap_or(false);
        let just_opened = response.clicked() && !busy && !was_open;
        let mut open = if response.clicked() && !busy { !was_open } else { was_open };
        if open {
            let mut items = vec![MenuItem::new("Reveal folder")];
            if card.cancel { items.push(if busy { MenuItem::new("Cancel job") } else { MenuItem::new("Cancel job").disabled("") }); }
            items.push(MenuItem::sep());
            let delete = MenuItem::new("Delete frames…").destructive();
            items.push(if busy { delete.disabled(reason) } else { delete });
            let list = MenuList::new(&items).name("Result actions");
            let size = list.measure(ui.painter());
            let screen = ui.ctx().content_rect();
            let pos = pos2(
                (more.right() - size.x).clamp(screen.left() + 4.0, (screen.right() - size.x - 4.0).max(screen.left() + 4.0)),
                (more.bottom() + 2.0).clamp(screen.top() + 4.0, (screen.bottom() - size.y - 4.0).max(screen.top() + 4.0)),
            );
            let popup = Area::new(menu_id.with("popup")).order(Order::Foreground).fixed_pos(pos).show(ui.ctx(), |menu_ui| {
                MenuList::new(&items).name("Result actions").at(pos).show(menu_ui)
            });
            let menu_rect = popup.inner.rect;
            let outside = ui.input(|input| input.pointer.any_pressed() && input.pointer.interact_pos().is_some_and(|p| !menu_rect.contains(p) && !more.contains(p)));
            let escape = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
            if let Some(k) = popup.inner.clicked {
                let action = match items[k].label {
                    "Reveal folder" => CardAction::Reveal,
                    "Cancel job" => CardAction::Cancel,
                    other => CardAction::Menu(other.to_string()),
                };
                out.card_action = Some((card.id.clone(), action));
            }
            if popup.inner.clicked.is_some() || (outside && !just_opened) || escape { open = false; }
        }
        ui.ctx().data_mut(|d| d.insert_temp(menu_id, open));
    }
}

fn graph_nodes(fx: &Fixture) -> Vec<NodeSpec<'static>> {
    let key_state = if fx.stage == "Key" { fx.busy_percent.map_or(NodeState::Idle, |p| NodeState::Running(p as f32 / 100.0)) } else { NodeState::Idle };
    vec![
        NodeSpec { id: 0, kind: NodeKind::Source, title: "Media", icon: Icon::Film, ports_in: vec![], ports_out: vec![("image", false, true)], pos: pos2(12.0, 12.0), state: NodeState::Idle, selected: false },
        NodeSpec { id: 1, kind: NodeKind::Mask, title: "Pre-matte (GVM)", icon: Icon::Gvm, ports_in: vec![("image", false, true)], ports_out: vec![("pre-matte", false, true)], pos: pos2(204.0, 12.0), state: NodeState::Idle, selected: false },
        NodeSpec { id: 2, kind: NodeKind::Mask, title: "Hint", icon: Icon::Hint, ports_in: vec![("image", false, true), ("pre-matte", true, true), ("mask", true, true)], ports_out: vec![("hint", false, true)], pos: pos2(396.0, 12.0), state: NodeState::Idle, selected: false },
        NodeSpec { id: 3, kind: NodeKind::Key, title: "Key", icon: Icon::Key, ports_in: vec![("image", false, true), ("hint", false, true), ("mask", true, true)], ports_out: vec![("result", false, true)], pos: pos2(588.0, 12.0), state: key_state, selected: true },
        NodeSpec { id: 4, kind: NodeKind::Output, title: "Deliver", icon: Icon::Deliver, ports_in: vec![("result", false, true)], ports_out: vec![("Matte", false, true), ("Processed", false, true)], pos: pos2(780.0, 12.0), state: NodeState::Idle, selected: false },
        NodeSpec { id: 5, kind: NodeKind::Mask, title: "Roto", icon: Icon::Pen, ports_in: vec![("image", false, true)], ports_out: vec![("matte", false, true)], pos: pos2(12.0, 146.0), state: NodeState::Idle, selected: false },
        NodeSpec { id: 6, kind: NodeKind::Mask, title: "SAM", icon: Icon::Sam, ports_in: vec![("image", false, true)], ports_out: vec![("mask", false, true)], pos: pos2(396.0, 146.0), state: NodeState::Idle, selected: false },
    ]
}

fn graph_wires() -> [Wire; 10] {
    [
        Wire { from: (0, 0), to: (1, 0), matte: false, optional: false },
        Wire { from: (0, 0), to: (2, 0), matte: false, optional: false },
        Wire { from: (0, 0), to: (3, 0), matte: false, optional: false },
        Wire { from: (1, 0), to: (2, 1), matte: true, optional: false },
        Wire { from: (2, 0), to: (3, 1), matte: true, optional: false },
        Wire { from: (3, 0), to: (4, 0), matte: false, optional: false },
        Wire { from: (0, 0), to: (5, 0), matte: false, optional: false },
        Wire { from: (0, 0), to: (6, 0), matte: false, optional: false },
        Wire { from: (5, 0), to: (2, 2), matte: true, optional: false },
        Wire { from: (6, 0), to: (3, 2), matte: true, optional: false },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_dock_deliver_ports_match_enabled_outputs() {
        let deliver = graph_nodes(&Fixture::default()).into_iter().find(|node| node.title == "Deliver").unwrap();
        assert_eq!(deliver.ports_out.len(), 2);
        assert!(deliver.ports_out.iter().all(|(_, _, enabled)| *enabled));
        assert_eq!(deliver.ports_out.iter().map(|(name, _, _)| *name).collect::<Vec<_>>(), ["Matte", "Processed"]);
    }
}
