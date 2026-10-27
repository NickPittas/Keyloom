//! Regression: "Back to batch" used to clear the open clip mid-frame and panic in graph().
//! Drives the same `App::frame` the window runs, with real egui pointer events at the button's own rectangle.
use ck::ui::{App, AppOptions, Tab};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use std::fs;

fn project(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ck-nav-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..3u8 {
        let f = fs::File::create(root.join(format!("clips/c/Frames/f_{i}.png"))).unwrap();
        let mut e = png::Encoder::new(f, 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&[i; 48])
            .unwrap();
    }
    root
}

fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(1440.0, 900.0),
        )),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
}

fn click(at: egui::Pos2) -> Vec<Event> {
    let b = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    vec![Event::PointerMoved(at), b(true), b(false)]
}

#[test]
fn back_to_batch_from_every_tab_and_focus_mode_then_reopen() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(project("back")),
            ..Default::default()
        },
    );
    for round in 0..2 {
        for tab in Tab::ALL {
            for focus in [false, true] {
                app.open_clip(&ctx, 0);
                app.switch_tab(tab);
                run(&ctx, &mut app, vec![]);
                run(&ctx, &mut app, vec![]);
                assert!(app.in_workspace(), "round {round} {tab:?}");
                if focus {
                    let f = Event::Key {
                        key: egui::Key::F,
                        physical_key: Some(egui::Key::F),
                        pressed: true,
                        repeat: false,
                        modifiers: Modifiers::NONE,
                    };
                    run(&ctx, &mut app, vec![f]);
                    run(&ctx, &mut app, vec![]);
                    assert!(app.focus_mode());
                    // Focus hides panels but the header (and Back) stays.
                }
                let at = app.back_rect().expect("back button drawn").center();
                run(&ctx, &mut app, click(at)); // must not panic mid-frame
                run(&ctx, &mut app, vec![]);
                assert!(!app.in_workspace(), "Back from {tab:?} focus={focus}");
            }
        }
    }
    // Reopen after all that: normal workspace again.
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    assert!(app.in_workspace());
    app.open_clip(&ctx, 99); // stale index after a reload must be ignored, not panic
    run(&ctx, &mut app, vec![]);
}

fn two_clip_project(tag: &str) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ck-nav-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    write_project(&root);
    root
}
fn write_project(root: &std::path::Path) {
    {
        use std::fs::*;
        create_dir_all(root.join("clips/b/Frames")).unwrap();
        write(root.join("project.json"), r#"{"version":2}"#).unwrap();
        write(root.join("clips/b/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
        for i in 0..3u8 {
            let mut e = png::Encoder::new(
                File::create(root.join(format!("clips/b/Frames/f_{i}.png"))).unwrap(),
                4,
                4,
            );
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header()
                .unwrap()
                .write_image_data(&[i; 48])
                .unwrap();
        }
    }
}

#[test]
fn board_selection_follows_the_clip_folder_across_a_reload_that_inserts_an_earlier_clip() {
    let root = two_clip_project("sel");
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(root.clone()),
            ..Default::default()
        },
    );
    app.select_folders(&["b"]);
    assert_eq!(app.selected_folders(), ["b"]);
    // An import adds clip `a`, which sorts before `b`: every index shifts.
    fs::create_dir_all(root.join("clips/a/Frames")).unwrap();
    fs::copy(
        root.join("clips/b/Frames/f_0.png"),
        root.join("clips/a/Frames/f_0.png"),
    )
    .unwrap();
    app.reload_keep_selection(&ctx);
    assert_eq!(
        app.selected_folders(),
        ["b"],
        "the selection must stay on b, not move to index 0 (= a)"
    );
}

#[test]
fn board_frames_do_not_rescan_clip_directories() {
    let root = two_clip_project("cache");
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(root.clone()),
            ..Default::default()
        },
    );
    run(&ctx, &mut app, vec![]);
    let first = app.card_summary_text(0).unwrap().to_owned();
    assert!(first.contains("3 frames"), "{first}");
    // Remove the frames behind the app's back: a per-frame rescan would change the summary.
    fs::remove_dir_all(root.join("clips/b/Frames")).unwrap();
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert_eq!(app.card_summary_text(0).unwrap(), first);
}

fn run_at(ctx: &egui::Context, app: &mut App, w: f32, h: f32) {
    for _ in 0..3 {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(w, h))),
            ..Default::default()
        };
        let mut o = ctx.run_ui(raw, |ui| app.frame(ui));
        o.textures_delta.clear();
    }
}

#[test]
fn narrow_728_header_controls_do_not_overlap_and_graph_clears_fit_button_and_hint_stays_tall() {
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            project: Some(project("narrow")),
            ..Default::default()
        },
    );
    app.open_clip(&ctx, 0);
    app.switch_tab(Tab::Hint);
    // Wide -> narrow -> wide -> narrow: the remembered graph panel height must not leak between modes.
    for (w, h) in [(1440.0, 900.0), (728.0, 900.0), (1440.0, 900.0), (728.0, 900.0)] {
        run_at(&ctx, &mut app, w, h);
        let hr = app.header_rects().to_vec();
        assert!(
            hr.len() >= 7,
            "back + 5 tabs + focus (+ properties when narrow): {hr:?}"
        );
        for (i, a) in hr.iter().enumerate() {
            assert!(
                a.max.x <= w + 0.5,
                "{w}: control {i} runs off the window: {a:?}"
            );
            for b in &hr[i + 1..] {
                assert!(
                    !a.shrink(0.5).intersects(b.shrink(0.5)),
                    "{w}: header controls overlap {a:?} {b:?}"
                );
            }
        }
        let (canvas, nodes) = app.graph_rects().expect("graph drawn");
        let fit = ck::ui::graph::fit_button_rect(canvas);
        assert_eq!(nodes.len(), 8);
        for n in &nodes {
            assert!(
                !n.intersects(fit),
                "{w}: node {n:?} under the Fit button {fit:?}"
            );
        }
        assert!(
            app.viewer_height_hint().unwrap() >= 280.0,
            "{w}: Hint viewer too short: {:?}",
            app.viewer_height_hint()
        );
    }
}

// ---------------------------------------------------------------- Board buttons, mocked dialogs, shortcuts

use ck::ui::picker::{Backend, PickKind};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

fn no_dialogs() -> Backend {
    Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"))
}

fn key(k: egui::Key) -> Vec<Event> {
    [true, false]
        .map(|pressed| Event::Key {
            key: k,
            physical_key: Some(k),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        })
        .into()
}

#[test]
fn open_clip_settings_works_from_the_card() {
    let tag = format!("btn-{}", std::process::id());
    let root = std::env::temp_dir().join(format!("ck-nav-{tag}"));
    let _ = fs::remove_dir_all(&root);
    write_project(&root);
    let ctx = egui::Context::default();
    let opts = AppOptions {
        project: Some(root.clone()),
        ..Default::default()
    };
    let mut app = App::with_picker(&ctx, opts, no_dialogs());
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    // Card button (it used to sit under an overlay that swallowed the click).
    let r = app
        .open_settings_rects()
        .iter()
        .find(|(i, _)| *i == Some(0))
        .expect("card button drawn")
        .1;
    run(&ctx, &mut app, click(r.center()));
    run(&ctx, &mut app, vec![]);
    assert!(app.in_workspace(), "card button opens the clip");
    let at = app.back_rect().unwrap().center();
    run(&ctx, &mut app, click(at));
    run(&ctx, &mut app, vec![]);
    assert!(!app.in_workspace());
}

#[test]
fn dialogs_run_off_the_ui_thread_one_at_a_time_and_results_wait_for_the_board() {
    let one = two_clip_project("dlg-a");
    let other = two_clip_project("dlg-b");
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (g, pick) = (gate.clone(), other.clone());
    let backend: Backend = Arc::new(move |kind, _| {
        assert_eq!(kind, PickKind::Project);
        let mut open = g.0.lock().unwrap();
        while !*open {
            open = g.1.wait(open).unwrap();
        }
        vec![pick.clone()]
    });
    let ctx = egui::Context::default();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(one.clone()),
            ..Default::default()
        },
        backend,
    );
    run(&ctx, &mut app, vec![]);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    assert!(app.request_pick(&ctx, PickKind::Project));
    assert!(
        !app.request_pick(&ctx, PickKind::Folder),
        "one dialog at a time"
    );
    // The dialog is blocked, yet frames keep rendering quickly.
    for _ in 0..5 {
        let t = Instant::now();
        run(&ctx, &mut app, vec![]);
        assert!(
            t.elapsed() < Duration::from_millis(500),
            "UI frame blocked by the dialog"
        );
    }
    *gate.0.lock().unwrap() = true;
    gate.1.notify_all();
    let d = Instant::now() + Duration::from_secs(5);
    while app.picking() {
        run(&ctx, &mut app, vec![]);
        assert!(Instant::now() < d);
        std::thread::sleep(Duration::from_millis(2));
    }
    run(&ctx, &mut app, vec![]);
    // Still inside the clip: the picked project is held, not swapped under the viewer.
    assert!(app.in_workspace());
    assert_eq!(app.project_root(), Some(one.as_path()));
    let at = app.back_rect().unwrap().center();
    run(&ctx, &mut app, click(at));
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert!(!app.in_workspace());
    assert_eq!(
        app.project_root(),
        Some(other.as_path()),
        "applied once the Board is back"
    );
}

#[test]
fn viewer_shortcuts_ab_interpretation_loop_and_zoom() {
    let ctx = egui::Context::default();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(project("shortcuts")),
            ..Default::default()
        },
        no_dialogs(),
    );
    run(&ctx, &mut app, vec![]);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    let (ab, lin, looping) = {
        let v = app.viewer().unwrap();
        (v.ab, v.linear, v.looping)
    };
    // A and C are not shortcuts (the A/B and colour controls stay in the UI).
    for k in [egui::Key::A, egui::Key::C, egui::Key::L, egui::Key::Equals] {
        run(&ctx, &mut app, key(k));
        run(&ctx, &mut app, vec![]);
    }
    let v = app.viewer().unwrap();
    assert_eq!((v.ab, v.linear, v.looping), (ab, lin, !looping));
    assert!(v.zoom > 1.0);
    run(&ctx, &mut app, key(egui::Key::Num0));
    run(&ctx, &mut app, vec![]);
    assert_eq!(app.viewer().unwrap().zoom, 1.0, "0 fits");
    assert!(app.in_workspace(), "0 is zoom-fit, not a tab");
}
