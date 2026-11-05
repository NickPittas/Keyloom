//! The Board's Compact Queue drawer (900 px window): opening it from the rail focuses its Close button, Tab never
//! leaves it, and Escape closes it and returns focus to the rail Queue button. Headless egui, synthetic key and
//! pointer events, the board fixture only; run through tools/isolated-test.sh.
use ck::ui::board::Page;
use ck::ui::theme;
use eframe::egui::{self, Event, Key, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use std::path::Path;

const SIZE: Vec2 = Vec2::new(900.0, 640.0);

fn fixture() -> Page {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/board-11.json");
    let mut p = Page::load(&path).unwrap_or_else(|e| panic!("{e}"));
    p.fx.drawer = 0;
    p.fx.menu = None;
    p
}

fn frame(ctx: &egui::Context, page: &mut Page, events: Vec<Event>) {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, SIZE)), events, ..Default::default() };
    let out = ctx.run_ui(raw, |ui| {
        page.ui(ui);
    });
    drop(out);
}

fn key(k: Key, pressed: bool) -> Event {
    Event::Key { key: k, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE }
}

fn press_at(ctx: &egui::Context, page: &mut Page, p: Pos2) {
    frame(ctx, page, vec![Event::PointerMoved(p)]);
    frame(ctx, page, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
    frame(ctx, page, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]);
}

fn focused_rect(ctx: &egui::Context) -> (egui::Id, Rect) {
    let id = ctx.memory(|m| m.focused()).expect("something has focus");
    let r = ctx.read_response(id).expect("focused widget has a response").rect;
    (id, r)
}

#[test]
fn drawer_focus_stays_inside_and_escape_returns_focus_to_the_rail() {
    let ctx = egui::Context::default();
    theme::apply_preference(&ctx, "Dark");
    let mut page = fixture();
    for _ in 0..3 { frame(&ctx, &mut page, vec![]); }

    // Open the drawer from the rail Queue button (its centre is the rail's top-left plus 20 px, 19 px).
    let rail = page.queue_rect().expect("rail drawn at 900 px");
    press_at(&ctx, &mut page, rail.min + Vec2::new(20.0, 19.0));
    for _ in 0..3 { frame(&ctx, &mut page, vec![]); }
    assert_eq!(page.fx.drawer, 1, "clicking the rail Queue button opens the drawer");

    // Close is 22 px at the drawer's top right: x = 900 - 30, y = 5.
    let (close_id, close) = focused_rect(&ctx);
    assert!((close.min.x - 870.0).abs() <= 1.0 && (close.min.y - 5.0).abs() <= 1.0, "initial focus is on Close, got {close:?}");

    // Tab through the drawer: focus must never land outside it (drawer starts at x = 600).
    for step in 0..8 {
        frame(&ctx, &mut page, vec![key(Key::Tab, true), key(Key::Tab, false)]);
        // Past the last drawer control egui may hold no focus at all; that is not an escape to the Board.
        let Some(id) = ctx.memory(|m| m.focused()) else { continue };
        let r = ctx.read_response(id).expect("focused widget has a response").rect;
        assert!(r.min.x >= 600.0 - 0.5, "Tab step {step} moved focus to {r:?} outside the drawer (id {id:?}, close {close_id:?})");
    }

    // Escape closes the drawer and focus returns to the rail Queue button.
    frame(&ctx, &mut page, vec![key(Key::Escape, true), key(Key::Escape, false)]);
    for _ in 0..3 { frame(&ctx, &mut page, vec![]); }
    assert_eq!(page.fx.drawer, 0, "Escape closes the drawer");
    let (_, r) = focused_rect(&ctx);
    assert!((r.min - rail.min - Vec2::new(9.0, 8.0)).length() <= 1.5, "focus back on the rail Queue button, got {r:?} (rail {rail:?})");
}
