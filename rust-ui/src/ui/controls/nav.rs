//! Shared keyboard model of the composite controls (bible §10.2, §12): a segmented control or a tab strip is ONE Tab
//! stop; ←/→ move (and select) inside it, Home/End jump, disabled children are skipped.
use eframe::egui::{EventFilter, Id, Key, Modifiers, Sense, Ui};

/// Id of child `i` of the composite `id`.
pub fn child_id(id: Id, i: usize) -> Id {
    id.with(("child", i))
}

/// The child that carries the Tab stop: the selected one, or the first enabled one when the selection is disabled.
pub fn stop(enabled: &[bool], index: usize) -> Option<usize> {
    if enabled.get(index) == Some(&true) { Some(index) } else { enabled.iter().position(|&e| e) }
}

/// Sense of child `i` (§12): only the stop is focusable; the others use the constant `Sense::CLICK` (the function
/// `Sense::click()` includes FOCUSABLE and would make every child a Tab stop); a disabled child only hovers.
pub fn child_sense(enabled: &[bool], index: usize, i: usize) -> Sense {
    if !enabled[i] { Sense::hover() } else if stop(enabled, index) == Some(i) { Sense::click() } else { Sense::CLICK }
}

/// Call BEFORE interacting with the children. While the stop has keyboard focus it locks the arrow keys to the
/// composite (`set_focus_lock_filter`) and handles ←/→ (previous / next enabled child), Home/End (first / last enabled
/// child). Returns true when the keyboard changed `*index`; focus has then moved to the new child, which gets the
/// focusable sense in the same frame. Space/Enter on the stop need no handling: arrows select at once.
pub fn composite_nav(ui: &Ui, id: Id, enabled: &[bool], index: &mut usize) -> bool {
    let Some(cur) = stop(enabled, *index) else { return false };
    let cid = child_id(id, cur);
    if !ui.memory(|m| m.has_focus(cid)) { return false }
    ui.memory_mut(|m| m.set_focus_lock_filter(cid, EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() }));
    let (left, right, home, end) = ui.input_mut(|i| {
        (i.consume_key(Modifiers::NONE, Key::ArrowLeft), i.consume_key(Modifiers::NONE, Key::ArrowRight),
         i.consume_key(Modifiers::NONE, Key::Home), i.consume_key(Modifiers::NONE, Key::End))
    });
    let target = if left {
        (0..cur).rev().find(|&i| enabled[i])
    } else if right {
        (cur + 1..enabled.len()).find(|&i| enabled[i])
    } else if home {
        enabled.iter().position(|&e| e)
    } else if end {
        enabled.iter().rposition(|&e| e)
    } else {
        None
    };
    match target {
        Some(t) if t != cur => {
            *index = t;
            ui.memory_mut(|m| m.request_focus(child_id(id, t)));
            true
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::{page_tabs, secondary_button, segmented, Seg, SegWidth, Tab};
    use super::*;
    use eframe::egui::{self, accesskit::{Role, Toggled}, Context, Event, RawInput};

    fn key(k: Key) -> Event {
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }
    }

    fn ctx() -> Context {
        let ctx = Context::default();
        crate::ui::fonts::install(&ctx);
        ctx
    }

    /// One frame with `events`; clears the texture deltas the test never uploads.
    fn frame(ctx: &Context, events: Vec<Event>, mut ui_fn: impl FnMut(&mut Ui)) -> egui::FullOutput {
        let mut out = ctx.run_ui(RawInput { events, ..Default::default() }, |ui| ui_fn(ui));
        out.textures_delta.clear();
        out
    }

    fn opts() -> [Seg<'static, usize>; 4] {
        [Seg::new(0, "Off"), Seg::new(1, "Pre-matte").disabled("Needs a hint"), Seg::new(2, "Hint"), Seg::new(3, "SAM")]
    }

    fn tabs() -> [Tab<'static>; 4] {
        [Tab::new("Board"), Tab::new("Shot").disabled("Open a shot from the Board"), Tab::new("Machines"), Tab::new("Models")]
    }

    /// (a) A button, a segmented control (4 options), a tab strip (4 tabs) and a button: Tab visits exactly four stops.
    #[test]
    fn seg_tab_is_one_tab_stop_each() {
        let ctx = ctx();
        let (mut v, mut page) = (0usize, 0usize);
        let mut ui_fn = |ui: &mut Ui| {
            ui.add(secondary_button("A"));
            segmented(ui, &mut v, &opts(), SegWidth::Natural);
            page_tabs(ui, &mut page, &tabs());
            ui.add(secondary_button("B"));
        };
        for _ in 0..3 { frame(&ctx, vec![], &mut ui_fn); }
        let mut seen = Vec::new();
        for _ in 0..10 {
            frame(&ctx, vec![key(Key::Tab)], &mut ui_fn);
            frame(&ctx, vec![], &mut ui_fn);
            let f = ctx.memory(|m| m.focused()).expect("Tab always lands on a stop");
            if !seen.contains(&f) { seen.push(f) }
        }
        assert_eq!(seen.len(), 4, "A, the segmented control, the tab strip and B are the only stops: {seen:?}");
    }

    /// (b) ←/→ select the previous / next enabled option (skipping the disabled one), Home / End jump; tabs too.
    #[test]
    fn seg_tab_arrows_skip_disabled() {
        let ctx = ctx();
        // Segmented control alone first, then tabs alone (a fresh context each, so Tab lands on the only stop).
        for round in 0..2 {
            let ctx = if round == 0 { ctx.clone() } else { self::ctx() };
            let sel = std::cell::Cell::new(0usize);
            let mut ui_fn = |ui: &mut Ui| {
                let mut s = sel.get();
                if round == 0 { segmented(ui, &mut s, &opts(), SegWidth::Natural); } else { page_tabs(ui, &mut s, &tabs()); }
                sel.set(s);
            };
            for _ in 0..3 { frame(&ctx, vec![], &mut ui_fn); }
            frame(&ctx, vec![key(Key::Tab)], &mut ui_fn);
            for _ in 0..2 { frame(&ctx, vec![], &mut ui_fn); }
            let (mut seq, mut got) = (Vec::new(), Vec::new());
            for k in [Key::ArrowRight, Key::ArrowRight, Key::ArrowRight, Key::ArrowLeft, Key::ArrowLeft, Key::End, Key::Home] {
                frame(&ctx, vec![key(k)], &mut ui_fn);
                frame(&ctx, vec![], &mut ui_fn);
                seq.push(k);
                got.push(sel.get());
            }
            // Round 0 options: 0, (1 disabled), 2, 3.  Round 1 tabs: 0, (1 disabled), 2, 3.
            assert_eq!(got, vec![2, 3, 3, 2, 0, 3, 0], "round {round}: {seq:?}");
        }
    }

    /// (c) §12: segmented = RadioGroup of RadioButtons (toggled), tabs = TabList of Tabs (selected).
    #[test]
    fn seg_tab_accesskit_roles() {
        let ctx = Context::default();
        crate::ui::fonts::install(&ctx);
        ctx.enable_accesskit();
        let (mut v, mut page) = (2usize, 2usize);
        let mut last = None;
        for _ in 0..3 {
            let out = frame(&ctx, vec![], |ui| {
                segmented(ui, &mut v, &opts(), SegWidth::Natural);
                page_tabs(ui, &mut page, &tabs());
            });
            last = out.platform_output.accesskit_update;
        }
        let update = last.expect("accesskit tree");
        let nodes = |role: Role| update.nodes.iter().filter(|(_, n)| n.role() == role).map(|(_, n)| n.clone()).collect::<Vec<_>>();
        assert_eq!(nodes(Role::RadioGroup).len(), 1);
        assert_eq!(nodes(Role::TabList).len(), 1);
        // Node order in the update is by id, so look nodes up by their accessible name.
        let by = |role: Role, name: &str| nodes(role).into_iter().find(|n| n.label() == Some(name)).unwrap_or_else(|| panic!("{role:?} {name}"));
        assert_eq!(nodes(Role::RadioButton).len(), 4);
        for (name, on) in [("Off", false), ("Pre-matte", false), ("Hint", true), ("SAM", false)] {
            assert_eq!(by(Role::RadioButton, name).toggled(), Some(if on { Toggled::True } else { Toggled::False }), "{name}");
        }
        assert_eq!(by(Role::RadioButton, "Pre-matte").description(), Some("Needs a hint"));
        assert!(by(Role::RadioButton, "Pre-matte").is_disabled());
        assert_eq!(nodes(Role::Tab).len(), 4);
        for (name, on) in [("Board", false), ("Shot", false), ("Machines", true), ("Models", false)] {
            assert_eq!(by(Role::Tab, name).is_selected(), Some(on), "{name}");
        }
        assert!(by(Role::Tab, "Shot").is_disabled());
    }

    #[test]
    fn stop_falls_back_to_first_enabled() {
        assert_eq!(stop(&[true, true], 1), Some(1));
        assert_eq!(stop(&[false, true, true], 0), Some(1));
        assert_eq!(stop(&[false, false], 0), None);
    }
}
