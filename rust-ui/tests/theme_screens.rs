//! Every primary screen (Board with its Queue, and the clip view's Media/Hint/Key/Deliver tabs with the node
//! graph) is drawn headless in Dark and Light; no painted text may fall under 4.5:1 against the fill behind it.
use ck::ui::{theme, App, AppOptions, Tab};
use eframe::egui::{self, Color32, RawInput, Rect, Vec2};
use std::{fs, sync::Arc};

fn project(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ck-theme-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..3u8 {
        let f = fs::File::create(root.join(format!("clips/c/Frames/f_{i}.png"))).unwrap();
        let mut e = png::Encoder::new(f, 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[i * 80; 48]).unwrap();
    }
    root
}

fn frame(ctx: &egui::Context, app: &mut App, size: Vec2) -> Vec<egui::epaint::ClippedShape> {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    out.shapes
}

fn check(pref: &str, screen: &str, shapes: &[egui::epaint::ClippedShape], base: Color32) {
    let (n, bad) = theme::text_contrast_report(shapes, base, 4.5);
    assert!(n > 5, "{pref} {screen}: only {n} texts painted, the screen did not draw");
    assert!(bad.is_empty(), "{pref} {screen}: {} text(s) under 4.5:1:\n{}", bad.len(), bad.join("\n"));
}

#[test]
fn board_queue_and_every_clip_tab_have_readable_text_in_dark_and_light() {
    for pref in ["Dark", "Light"] {
        let ctx = egui::Context::default();
        let dialogs: ck::ui::picker::Backend = Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"));
        let sp = std::env::temp_dir().join(format!("ck-theme-cfg-{pref}-{}", std::process::id()));
        fs::create_dir_all(&sp).unwrap();
        let mut app = App::with_picker(
            &ctx,
            AppOptions { project: Some(project(pref)), settings_path: Some(sp.join("settings.json")), ..Default::default() },
            dialogs,
        );
        theme::apply_preference(&ctx, pref);
        let size = Vec2::new(1440.0, 900.0);
        for _ in 0..3 { frame(&ctx, &mut app, size); }
        let base = ctx.global_style().visuals.panel_fill;
        assert_eq!(base == theme::PANEL, pref == "Dark", "{pref} palette not installed");
        check(pref, "Board+Queue", &frame(&ctx, &mut app, size), base);
        app.open_clip(&ctx, 0);
        for tab in Tab::ALL {
            app.switch_tab(tab);
            for _ in 0..3 { frame(&ctx, &mut app, size); }
            assert!(app.in_workspace(), "{pref} {tab:?}");
            check(pref, &format!("{tab:?}+graph"), &frame(&ctx, &mut app, size), base);
        }
    }
}

fn cursor_at(ctx: &egui::Context, app: &mut App, at: egui::Pos2) -> egui::CursorIcon {
    let size = Vec2::new(1440.0, 900.0);
    let mut last = egui::CursorIcon::Default;
    for _ in 0..2 {
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)), events: vec![egui::Event::PointerMoved(at)], ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
        last = out.platform_output.cursor_icon;
    }
    last
}

/// Tabs, chips and the A/B toggle show the hand; a disabled chip and empty space do not.
#[test]
fn tabs_and_chips_show_the_hand_and_disabled_or_empty_do_not() {
    for pref in ["Dark", "Light"] {
        let ctx = egui::Context::default();
        let dialogs: ck::ui::picker::Backend = Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"));
        let sp = std::env::temp_dir().join(format!("ck-theme-hover-{pref}-{}", std::process::id()));
        fs::create_dir_all(&sp).unwrap();
        let mut app = App::with_picker(&ctx, AppOptions { project: Some(project(&format!("h{pref}"))), settings_path: Some(sp.join("settings.json")), ..Default::default() }, dialogs);
        theme::apply_preference(&ctx, pref);
        app.open_clip(&ctx, 0);
        app.switch_tab(Tab::Hint);
        let size = Vec2::new(1440.0, 900.0);
        for _ in 0..4 { frame(&ctx, &mut app, size); }
        let rects = app.header_rects().to_vec();
        assert_eq!(cursor_at(&ctx, &mut app, rects[0].center()), egui::CursorIcon::PointingHand, "{pref} back");
        assert_eq!(cursor_at(&ctx, &mut app, rects[1].center()), egui::CursorIcon::PointingHand, "{pref} tab");
        let rects = app.viewer().unwrap().ui_rects.clone();
        let find = |n: &str| rects.iter().find(|(k, _)| *k == n).unwrap_or_else(|| panic!("{n} chip not drawn")).1.center();
        assert_eq!(cursor_at(&ctx, &mut app, find("ab")), egui::CursorIcon::PointingHand, "{pref} A/B chip");
        // The default pre-matte producer is GVM, so the SAM chip is disabled; the rightmost view chip (Processed) has no output yet, so it is disabled too.
        assert_eq!(cursor_at(&ctx, &mut app, find("sam")), egui::CursorIcon::Default, "{pref} disabled SAM chip");
        let chips = find("chips");
        let last = egui::Pos2::new(rects.iter().find(|(k, _)| *k == "chips").unwrap().1.right() - 20.0, chips.y);
        assert_eq!(cursor_at(&ctx, &mut app, last), egui::CursorIcon::Default, "{pref} disabled Processed chip");
        assert_eq!(cursor_at(&ctx, &mut app, egui::Pos2::new(700.0, 20.0)), egui::CursorIcon::Default, "{pref} empty header");
    }
}

/// Every text painted in the bottom action panel (cards, buttons) must lie fully inside its clip: nothing is cut
/// off at 900x640, 1440x900 or 2400x1400, in Dark and Light, on every tab that has actions.
#[test]
fn action_cards_and_buttons_are_not_cut_off_at_three_window_sizes() {
    use egui::Shape;
    fn texts(s: &Shape, clip: Rect, out: &mut Vec<(Rect, Rect, String)>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| texts(x, clip, out)),
            Shape::Text(t) => out.push((t.galley.rect.translate(t.pos.to_vec2()), clip, t.galley.job.text.clone())),
            _ => {}
        }
    }
    for pref in ["Dark", "Light"] {
        for size in [Vec2::new(900.0, 640.0), Vec2::new(1440.0, 900.0), Vec2::new(2400.0, 1400.0)] {
            let ctx = egui::Context::default();
            let dialogs: ck::ui::picker::Backend = Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"));
            let sp = std::env::temp_dir().join(format!("ck-theme-lay-{pref}-{}-{}", size.x, std::process::id()));
            fs::create_dir_all(&sp).unwrap();
            let tag = format!("l{pref}{}", size.x);
            let mut app = App::with_picker(&ctx, AppOptions { project: Some(project(&tag)), settings_path: Some(sp.join("settings.json")), ..Default::default() }, dialogs);
            theme::apply_preference(&ctx, pref);
            app.open_clip(&ctx, 0);
            for tab in [Tab::Hint, Tab::Key, Tab::Deliver] {
                app.switch_tab(tab);
                for _ in 0..4 { frame(&ctx, &mut app, size); }
                let shapes = frame(&ctx, &mut app, size);
                let panel = app.tools_rects().expect("tools panel drawn")[0];
                let mut t = vec![];
                for cs in &shapes { texts(&cs.shape, cs.clip_rect, &mut t); }
                let inside: Vec<_> = t.iter().filter(|(r, c, s)| r.is_positive() && panel.contains(r.center()) && c.center().y > panel.top() && s.len() <= 40 && !s.contains("marked frames")).collect();
                assert!(inside.len() > 2, "{pref} {tab:?} {size:?}: no text in the action panel");
                let cut: Vec<_> = inside.iter().filter(|(r, c, _)| r.top() < c.top() - 1.0 || r.bottom() > c.bottom() + 1.0 || r.bottom() > panel.bottom() + 1.0).map(|(r, _, s)| format!("{s:?} {r:?} panel {panel:?}")).collect();
                // (The marked-frames row under the cards is reached with the always-visible scrollbar. Long status prose scrolls in its own column; cards and buttons must show.)
                // The viewer's own bottom rows (marks line) must stay above the action panel.
                let marks = app.viewer().unwrap().ui_rects.iter().find(|(k, _)| *k == "marks_text").map(|(_, r)| r.bottom());
                if let Some(b) = marks { assert!(b <= panel.top() + 1.0, "{pref} {tab:?} {size:?}: viewer marks row {b} runs under the action panel {panel:?}"); }
                assert!(cut.is_empty(), "{pref} {tab:?} {size:?}: cut off:\n{}", cut.join("\n"));
            }
        }
    }
}

/// The viewer's chip rows (A/B wipe, overlay chips, SAM, the view chips) never overlap at any width.
#[test]
fn viewer_chip_rows_never_overlap_at_three_window_sizes() {
    for pref in ["Dark", "Light"] {
        for size in [Vec2::new(900.0, 640.0), Vec2::new(1440.0, 900.0), Vec2::new(2400.0, 1400.0)] {
            let ctx = egui::Context::default();
            let dialogs: ck::ui::picker::Backend = Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"));
            let sp = std::env::temp_dir().join(format!("ck-theme-chip-{pref}-{}-{}", size.x, std::process::id()));
            fs::create_dir_all(&sp).unwrap();
            let mut app = App::with_picker(&ctx, AppOptions { project: Some(project(&format!("c{pref}{}", size.x))), settings_path: Some(sp.join("settings.json")), ..Default::default() }, dialogs);
            theme::apply_preference(&ctx, pref);
            app.open_clip(&ctx, 0);
            for tab in [Tab::Hint, Tab::Key] {
                app.switch_tab(tab);
                for _ in 0..5 { frame(&ctx, &mut app, size); }
                let rects: Vec<_> = app.viewer().unwrap().ui_rects.iter().filter(|(k, _)| matches!(*k, "chips" | "ab" | "overlay_off" | "overlay_guide" | "overlay_hint" | "sam")).cloned().collect();
                assert!(rects.len() >= 5, "{pref} {tab:?} {size:?}: chips not drawn: {rects:?}");
                for (i, (a, ra)) in rects.iter().enumerate() {
                    for (b, rb) in &rects[i + 1..] {
                        assert!(!ra.intersects(*rb), "{pref} {tab:?} {size:?}: {a} {ra:?} overlaps {b} {rb:?}");
                    }
                }
            }
        }
    }
}

/// A shot whose original movie is gone shows the "not found at <path>: reconnect or relink it" text on its Board card and in Media, readable in both themes.
#[test]
fn missing_original_movie_text_shows_on_board_card_and_in_media() {
    use egui::Shape;
    fn texts(s: &Shape, out: &mut Vec<String>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| texts(x, out)),
            Shape::Text(t) => out.push(t.galley.job.text.clone()),
            _ => {}
        }
    }
    let want = "Source movie not found at /gone/movie.mp4: reconnect or relink it";
    for pref in ["Dark", "Light"] {
        let root = project(&format!("gone{pref}"));
        fs::create_dir_all(root.join("clips/m/Source")).unwrap();
        fs::write(root.join("clips/m/clip.json"), r#"{"source":{"type":"video","filename":"movie.mp4","original_path":"/gone/movie.mp4"}}"#).unwrap();
        let ctx = egui::Context::default();
        let dialogs: ck::ui::picker::Backend = Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"));
        let sp = std::env::temp_dir().join(format!("ck-theme-gone-{pref}-{}", std::process::id()));
        fs::create_dir_all(&sp).unwrap();
        let mut app = App::with_picker(&ctx, AppOptions { project: Some(root), settings_path: Some(sp.join("settings.json")), ..Default::default() }, dialogs);
        theme::apply_preference(&ctx, pref);
        let size = Vec2::new(1440.0, 900.0);
        for _ in 0..3 { frame(&ctx, &mut app, size); }
        let base = ctx.global_style().visuals.panel_fill;
        let shapes = frame(&ctx, &mut app, size);
        let mut t = vec![];
        for cs in &shapes { texts(&cs.shape, &mut t); }
        assert!(t.iter().any(|s| s.contains("Source movie not found at /gone/movie.mp4")), "{pref} Board card lacks the text: {t:?}");
        check(pref, "Board missing movie", &shapes, base);
        app.open_clip(&ctx, 1); // clips are listed by folder name: c, m
        assert!(app.in_workspace());
        app.switch_tab(Tab::Media);
        for _ in 0..3 { frame(&ctx, &mut app, size); }
        let shapes = frame(&ctx, &mut app, size);
        let mut t = vec![];
        for cs in &shapes { texts(&cs.shape, &mut t); }
        assert!(t.iter().any(|s| s == want), "{pref} Media lacks the full text: {t:?}");
        check(pref, "Media missing movie", &shapes, base);
    }
}
