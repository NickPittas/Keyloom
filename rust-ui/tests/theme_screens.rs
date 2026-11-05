//! Every primary screen (Board with its Queue, and the clip view's Media/Hint/Key/Deliver tabs with the node
//! graph) is drawn headless in Dark and Light; no painted text may fall under 4.5:1 against the fill behind it.
use ck::project::ClipState;
use ck::ui::{theme, App, AppOptions, Tab};
use eframe::egui::{self, Color32, RawInput, Rect, Vec2};
use std::{fs, sync::Arc};

fn png(p: &std::path::Path, v: u8) {
    let f = fs::File::create(p).unwrap();
    let mut e = png::Encoder::new(f, 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&[v; 48]).unwrap();
}

/// Four clips, one per Board stage: c Raw, m Masked, r Ready, d Complete.
fn project(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ck-theme-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for c in ["c", "m", "r", "d"] {
        fs::create_dir_all(root.join(format!("clips/{c}/Frames"))).unwrap();
        fs::write(root.join(format!("clips/{c}/.video_metadata.json")), r#"{"fps":24.0}"#).unwrap();
        for i in 0..3u8 {
            png(&root.join(format!("clips/{c}/Frames/f_{i}.png")), i * 80);
        }
    }
    fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    fs::create_dir_all(root.join("clips/m/VideoMamaMaskHint")).unwrap();
    png(&root.join("clips/m/VideoMamaMaskHint/m_0.png"), 9);
    fs::create_dir_all(root.join("clips/r/AlphaHint")).unwrap();
    for i in 0..3u8 {
        png(&root.join(format!("clips/r/AlphaHint/a_{i}.png")), i);
    }
    fs::create_dir_all(root.join("clips/d/Output/Matte")).unwrap();
    fs::write(root.join("clips/d/Output/.corridorkey_manifest.json"), r#"{"version":2,"engine":"corridorkey-v2","enabled_outputs":["matte"],"formats":{"matte":"png"},"complete":true}"#).unwrap();
    for i in 0..3u8 {
        png(&root.join(format!("clips/d/Output/Matte/f_{i}.png")), i);
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
        let board = frame(&ctx, &mut app, size);
        check(pref, "Board+Queue", &board, base);
        stage_chips_are_painted(pref, &ctx, &board);
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

/// Every text painted in the frame as (rect on screen, clip rect, text), found by walking the painted shapes.
fn painted_texts(shapes: &[egui::epaint::ClippedShape]) -> Vec<(Rect, Rect, String)> {
    fn walk(s: &egui::epaint::Shape, clip: Rect, out: &mut Vec<(Rect, Rect, String)>) {
        match s {
            egui::epaint::Shape::Vec(v) => v.iter().for_each(|x| walk(x, clip, out)),
            egui::epaint::Shape::Text(t) => out.push((t.galley.rect.translate(t.pos.to_vec2()), clip, t.galley.text().to_string())),
            _ => {}
        }
    }
    let mut out = vec![];
    for cs in shapes {
        walk(&cs.shape, cs.clip_rect, &mut out);
    }
    out
}

/// Centre of the first painted text equal to `label` (what the user reads, not a stored rectangle).
fn label_at(shapes: &[egui::epaint::ClippedShape], label: &str) -> Option<egui::Pos2> {
    painted_texts(shapes).into_iter().find(|(_, _, s)| s == label).map(|(r, _, _)| r.center())
}

/// Tabs and chips show the hand; a disabled chip does not. Found by their painted labels.
#[test]
fn tabs_and_chips_show_the_hand_and_disabled_chips_do_not() {
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
        let mut shapes = vec![];
        for _ in 0..4 { shapes = frame(&ctx, &mut app, size); }
        let at = |label: &str| label_at(&shapes, label).unwrap_or_else(|| panic!("{pref}: {label:?} not painted"));
        assert_eq!(cursor_at(&ctx, &mut app, at("Key")), egui::CursorIcon::PointingHand, "{pref} tab");
        // The default pre-matte producer is GVM, so the SAM chip is disabled.
        assert_eq!(cursor_at(&ctx, &mut app, at("SAM")), egui::CursorIcon::Default, "{pref} disabled SAM chip");
    }
}

/// Short labels, cards and buttons painted inside a clipped area (the bottom action panel) must lie fully inside their
/// clip: nothing is cut off at 900x640, 1440x900 or 2400x1400, in Dark and Light, on every tab that has actions.
/// Long status prose scrolls in its own column, so only labels of up to 40 characters are checked.
#[test]
fn action_cards_and_buttons_are_not_cut_off_at_three_window_sizes() {
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
                // Clipped areas are the panels and columns; a label inside one must show in full.
                let labels: Vec<_> = painted_texts(&shapes)
                    .into_iter()
                    .filter(|(r, c, s)| r.is_positive() && (c.width() < size.x - 1.0 || c.height() < size.y - 1.0) && s.len() <= 40 && !s.contains("marked frames"))
                    .collect();
                assert!(labels.len() > 2, "{pref} {tab:?} {size:?}: no action label painted");
                let cut: Vec<String> = labels
                    .iter()
                    // Text fields clip long values by design; a folder template ("{name}_{version}/{output}") is such a value.
                    .filter(|(_, _, s)| !(s.contains('{') && s.contains('}')))
                    .filter(|(r, c, _)| r.left() < c.left() - 1.0 || r.top() < c.top() - 1.0 || r.right() > c.right() + 1.0 || r.bottom() > c.bottom() + 1.0)
                    .map(|(r, c, s)| format!("{s:?} {r:?} clip {c:?}"))
                    .collect();
                assert!(cut.is_empty(), "{pref} {tab:?} {size:?}: cut off:\n{}", cut.join("\n"));
            }
        }
    }
}

/// The viewer's chip labels (A/B wipe, overlay chips, SAM, the view chips) never paint over each other at any width.
#[test]
fn viewer_chip_labels_never_overlap_at_three_window_sizes() {
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
                let shapes = frame(&ctx, &mut app, size);
                let labels: Vec<_> = painted_texts(&shapes).into_iter().filter(|(r, _, s)| r.is_positive() && s.len() <= 20).collect();
                assert!(labels.len() >= 5, "{pref} {tab:?} {size:?}: chips not drawn: {labels:?}");
                for (i, (ra, _, a)) in labels.iter().enumerate() {
                    for (rb, _, b) in &labels[i + 1..] {
                        // The dock repaints its tab labels on the strip's own labels: same text, same spot is not a collision.
                        let repaint = a == b && (ra.left() - rb.left()).abs() <= 2.0 && (ra.top() - rb.top()).abs() <= 2.0;
                        assert!(repaint || !ra.shrink(1.0).intersects(rb.shrink(1.0)), "{pref} {tab:?} {size:?}: {a:?} {ra:?} overlaps {b:?} {rb:?}");
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
        let root = std::env::temp_dir().join(format!("ck-theme-gone{pref}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
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
        app.open_clip(&ctx, 0);
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

fn stage_colours(ctx: &egui::Context) -> Vec<(ClipState, theme::StageColors, Color32, Color32)> {
    let mut out = vec![];
    let mut o = ctx.run_ui(RawInput::default(), |ui| {
        for st in [ClipState::Raw, ClipState::Masked, ClipState::Ready, ClipState::Complete, ClipState::Extracting, ClipState::Error] {
            out.push((st, theme::stage(ui, st), theme::card_fill(ui), ui.visuals().panel_fill));
        }
    });
    o.textures_delta.clear();
    out
}

/// The Board draws every stage's chip fill and a card edge in its own colour, and names it.
fn stage_chips_are_painted(pref: &str, ctx: &egui::Context, shapes: &[egui::epaint::ClippedShape]) {
    use egui::Shape;
    fn rects(s: &Shape, out: &mut Vec<egui::epaint::RectShape>) {
        match s {
            Shape::Vec(v) => v.iter().for_each(|x| rects(x, out)),
            Shape::Rect(r) => out.push(r.clone()),
            _ => {}
        }
    }
    let mut all = vec![];
    shapes.iter().for_each(|c| rects(&c.shape, &mut all));
    for (st, c, _, _) in stage_colours(ctx).into_iter().take(4) {
        assert!(all.iter().any(|r| r.fill == c.chip), "{pref}: no {st:?} chip painted");
        // The Board card's edge is a 3 px filled bar in the stage colour (controls/card.rs), or a stroke.
        assert!(all.iter().any(|r| r.fill == c.edge) || all.iter().any(|r| r.stroke.color == c.edge && r.stroke.width > 0.0), "{pref}: no {st:?} card edge painted");
    }
}

#[test]
fn stage_colours_are_distinct_and_readable_in_dark_and_light() {
    for pref in ["Dark", "Light"] {
        let ctx = egui::Context::default();
        theme::apply_preference(&ctx, pref);
        let cols = stage_colours(&ctx);
        for (st, c, card, panel) in &cols {
            assert!(theme::contrast(c.text, c.chip) >= 4.5, "{pref} {st:?}: chip text {:.2}:1", theme::contrast(c.text, c.chip));
            assert!(theme::contrast(c.edge, *card) >= 3.0, "{pref} {st:?}: edge on the card {:.2}:1", theme::contrast(c.edge, *card));
            assert!(theme::contrast(c.edge, *panel) >= 3.0, "{pref} {st:?}: edge on the panel {:.2}:1", theme::contrast(c.edge, *panel));
        }
        for (i, a) in cols.iter().enumerate() {
            for b in &cols[i + 1..] {
                assert!(a.1.edge != b.1.edge && a.1.chip != b.1.chip, "{pref}: {:?} and {:?} share a colour", a.0, b.0);
            }
        }
    }
}
