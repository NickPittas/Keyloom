//! The pinned action bar (bible §7.2, §5.1 Busy row): 72 high at every width and state. Rows: blocker / status line 24,
//! gap 4, button row 26. Slots left → right: ⋯ 26 · 8 · secondary (flex) · 8 · primary (112–160), so rest, blocked and busy
//! put every control at the same x.
use super::buttons::{icon_button, secondary_button, Btn, IconButtonSize};
use super::{chips::{self, Progress, State}, line, paint_line};
use crate::ui::icons::{self, Icon, IconSize};
use crate::ui::tokens::tokens;
use crate::ui::type_scale as ty;
use eframe::egui::{self, Align, CornerRadius, Rect, Response, Sense, Stroke, Ui};

/// Why the primary is blocked: one sentence, plus an optional fix button (short label, e.g. "Assign machine").
pub struct Blocker<'a> {
    pub text: &'a str,
    pub fix_button: Option<&'a str>,
}

/// The stage's job is running: the primary shows `progress_text` ("Keying 34%") and `cancel` takes the secondary slot.
pub struct BusyState<'a> {
    pub progress_text: &'a str,
    pub cancel: &'a str,
    /// Fraction 0–1 for the 6 px bar under the status line (busy with a status line only); `None` = no bar.
    pub progress: Option<f32>,
}

pub struct ActionBar<'a> {
    /// Contents of the ⋯ menu; `None` = no other actions (the button stays, disabled, tooltip "No other actions").
    pub more_menu: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    /// The stage's own secondary action; `None` leaves the slot empty (Key bar).
    pub secondary: Option<Btn<'a>>,
    pub primary: Btn<'a>,
    pub blocker: Option<Blocker<'a>>,
    /// One tertiary sentence on the top row when there is no blocker ("Last keyed 14:02 on endor-omarchy", busy progress).
    pub status_text: Option<&'a str>,
    pub busy: Option<BusyState<'a>>,
    pub reduced_motion: bool,
}

/// Fixed slot rects for a bar occupying `rect` (width any, height 72).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BarSlots {
    pub top_row: Rect,
    pub more: Rect,
    pub secondary: Rect,
    pub primary: Rect,
}

pub struct ActionBarOut {
    pub more: Response,
    pub primary: Response,
    /// The stage's secondary action, or Cancel while busy.
    pub secondary: Option<Response>,
    pub fix: Option<Response>,
    pub slots: BarSlots,
}

pub const BAR_HEIGHT: f32 = 72.0;
/// Primary slot (§7.2): min 112, max 160, right-aligned; it widens to fit its label, never past 160.
const PRIMARY_MIN_W: f32 = 112.0;
const PRIMARY_MAX_W: f32 = 160.0;

impl BarSlots {
    pub fn new(rect: Rect) -> Self { Self::with_primary(rect, PRIMARY_MIN_W) }

    pub fn with_primary(rect: Rect, primary_w: f32) -> Self {
        let primary_w = primary_w.clamp(PRIMARY_MIN_W, PRIMARY_MAX_W);
        let (x0, x1, y) = (rect.min.x + 8.0, rect.max.x - 8.0, rect.min.y + 8.0);
        let row_y = y + 24.0 + 4.0;
        let at = |xa: f32, xb: f32| Rect::from_min_max(egui::pos2(xa, row_y), egui::pos2(xb, row_y + 26.0));
        BarSlots {
            top_row: Rect::from_min_max(egui::pos2(x0, y), egui::pos2(x1, y + 24.0)),
            more: at(x0, x0 + 26.0),
            secondary: at(x0 + 26.0 + 8.0, x1 - primary_w - 8.0),
            primary: at(x1 - primary_w, x1),
        }
    }
}

/// Draw the bar in the full width of `ui`'s remaining space (72 high).
pub fn action_bar(ui: &mut Ui, bar: ActionBar<'_>) -> ActionBarOut {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), BAR_HEIGHT), Sense::hover());
    action_bar_in(ui, rect, bar)
}

/// As [`action_bar`] in an explicit `rect` (specimen pages).
pub fn action_bar_in(ui: &mut Ui, rect: Rect, bar: ActionBar<'_>) -> ActionBarOut {
    let t = tokens(ui);
    let pw = bar.primary.natural_width(ui); // from the rest label only, so rest / blocked / busy share one slot (busy text truncates)
    let slots = BarSlots::with_primary(rect, pw);
    let p = ui.painter().clone();
    p.hline(rect.x_range(), rect.min.y, Stroke::new(1.0, t.border_divider));

    // Top row: blocker line (error tint, icon 14, sentence, fix button right-aligned) or one tertiary sentence.
    let mut fix = None;
    if let Some(b) = &bar.blocker {
        let row = slots.top_row;
        p.rect_filled(row, CornerRadius::same(3), t.error_tint);
        let icon = Rect::from_min_size(egui::pos2(row.min.x + 8.0, (row.center().y - 7.0).round()), egui::vec2(14.0, 14.0));
        icons::paint(&p, icon, Icon::Error, IconSize::S14, t.error);
        let mut text_right = row.max.x - 8.0;
        if let Some(label) = b.fix_button {
            let w = line(&p, label, ty::button(), 16.0, f32::INFINITY).size().x + 22.0;
            let w = w.max(64.0).round();
            let r = Rect::from_min_size(egui::pos2(row.max.x - w, row.min.y + 1.0), egui::vec2(w, 22.0));
            fix = Some(ui.add(secondary_button(label).in_rect(r)));
            text_right = r.min.x - 8.0;
        }
        let tr = Rect::from_min_max(egui::pos2(icon.max.x + 6.0, row.min.y), egui::pos2(text_right, row.max.y));
        paint_line(&p, tr, line(&p, b.text, ty::small(), 14.0, tr.width()), 14.0, Align::Min, t.text_primary, 0.0);
        ui.interact(tr, ui.scope_id().with("blocker-text"), Sense::hover()).on_hover_text(b.text);
    } else if let Some(s) = bar.status_text {
        let row = slots.top_row;
        let tr = Rect::from_min_max(egui::pos2(row.min.x + 8.0, row.min.y), egui::pos2(row.max.x - 8.0, row.max.y));
        match bar.busy.as_ref().and_then(|b| b.progress) {
            // Busy with progress (mockup 58): 14-high line at +2, then the 6 px bar across the full row at +16.
            Some(v) => {
                let text = Rect::from_min_max(egui::pos2(tr.min.x, row.min.y + 2.0), egui::pos2(tr.max.x, row.min.y + 16.0));
                paint_line(&p, text, line(&p, s, ty::small(), 14.0, text.width()), 14.0, Align::Min, t.text_tertiary, 0.0);
                let track = Rect::from_min_max(egui::pos2(row.min.x, row.min.y + 16.0), egui::pos2(row.max.x, row.min.y + 22.0));
                chips::progress(ui, track, Progress { value: Some(v), state: State::Running, thin: false, label: "Progress" });
            }
            None => paint_line(&p, tr, line(&p, s, ty::small(), 14.0, tr.width()), 14.0, Align::Min, t.text_tertiary, 0.0),
        }
    }

    // Button row.
    let more = match bar.more_menu {
        Some(menu) => {
            let r = ui.add(icon_button(Icon::More, IconButtonSize::Normal22, "More actions").height(26.0).in_rect(slots.more));
            egui::Popup::menu(&r).show(menu);
            r
        }
        None => ui.add(icon_button(Icon::More, IconButtonSize::Normal22, "No other actions").height(26.0).in_rect(slots.more).disabled("No other actions")),
    };
    let secondary = if let Some(b) = &bar.busy {
        Some(ui.add(secondary_button(b.cancel).in_rect(slots.secondary).reduced_motion(bar.reduced_motion)))
    } else {
        bar.secondary.map(|s| ui.add(s.in_rect(slots.secondary)))
    };
    let mut primary = bar.primary.in_rect(slots.primary).reduced_motion(bar.reduced_motion);
    if let Some(b) = &bar.busy {
        primary = primary.busy(b.progress_text);
    } else if let Some(b) = &bar.blocker {
        primary = primary.disabled(b.text);
    }
    let primary = ui.add(primary);
    ActionBarOut { more, primary, secondary, fix, slots }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::buttons::primary_button;

    fn run(f: impl Fn(&mut Ui) -> ActionBarOut) -> ActionBarOut {
        let ctx = egui::Context::default();
        let mut out = None;
        ctx.run_ui(egui::RawInput::default(), |ui| out = Some(f(ui))).textures_delta.clear();
        out.unwrap()
    }

    fn rest<'a>() -> ActionBar<'a> {
        ActionBar { more_menu: None, secondary: None, primary: primary_button("Key shot"), blocker: None, status_text: Some("Last keyed 14:02"), busy: None, reduced_motion: true }
    }

    /// Rest, blocked and busy: the same slot rects and the same primary / ⋯ rects, at 280 and 320 wide.
    #[test]
    fn slots_do_not_move_between_rest_blocked_and_busy() {
        for w in [264.0 + 16.0, 320.0] {
            let draw = |mode: u8| {
                run(|ui| {
                    ui.set_max_width(w);
                    let mut b = rest();
                    match mode {
                        1 => b.blocker = Some(Blocker { text: "Can't key: endor-omarchy is offline.", fix_button: Some("Assign machine") }),
                        2 => b.busy = Some(BusyState { progress_text: "Keying 34%", cancel: "Cancel", progress: Some(0.34) }),
                        _ => {}
                    }
                    action_bar(ui, b)
                })
            };
            let (a, b, c) = (draw(0), draw(1), draw(2));
            assert_eq!(a.slots, b.slots);
            assert_eq!(a.slots, c.slots);
            assert_eq!(a.primary.rect, b.primary.rect);
            assert_eq!(a.primary.rect, c.primary.rect);
            assert_eq!(a.more.rect, c.more.rect);
            assert_eq!(c.secondary.as_ref().unwrap().rect, a.slots.secondary);
            assert!(a.secondary.is_none());
            assert_eq!(a.slots.primary.width(), 112.0);
            assert_eq!(a.slots.secondary.width(), w - 16.0 - 26.0 - 8.0 - 8.0 - 112.0);
        }
    }

    /// Busy with the progress bar (mockup 58) keeps the same slots as busy without it.
    #[test]
    fn busy_progress_bar_keeps_slots() {
        let draw = |progress| run(|ui| {
            let mut b = rest();
            b.busy = Some(BusyState { progress_text: "Keying 34%", cancel: "Cancel", progress });
            action_bar(ui, b)
        });
        let (plain, bar) = (draw(None), draw(Some(0.34)));
        assert_eq!(plain.slots, bar.slots);
        assert_eq!(plain.primary.rect, bar.primary.rect);
        assert_eq!(plain.more.rect, bar.more.rect);
        assert_eq!(plain.secondary.unwrap().rect, bar.secondary.unwrap().rect);
    }

    /// §7.2: the primary slot is 112 at least, 160 at most, right-aligned.
    #[test]
    fn primary_slot_is_112_to_160_right_aligned() {
        let w = |label: &'static str| run(|ui| action_bar(ui, ActionBar { primary: primary_button(label), ..rest() })).slots.primary;
        let (short, long) = (w("Key"), w("Export every deliverable of this long shot"));
        assert_eq!((short.width(), long.width()), (112.0, 160.0));
        assert_eq!(short.max.x, long.max.x);
    }
}
