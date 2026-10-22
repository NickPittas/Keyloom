//! Settings page wired into the real `App::frame`: reachable from Batch and a clip workspace, Cancel/Back discard,
//! Apply persists and rescales at once, RAM/workers/loop reach the next opened viewer. Temp settings files only;
//! clicks land on rectangles the app itself reports.
use ck::settings::{Settings, Store};
use ck::ui::picker::{Backend, PickKind};
use ck::ui::{App, AppOptions};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ck-settings-app-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn project(tag: &str, name: &str) -> PathBuf {
    let root = tmp(tag).join(name);
    fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
    fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..5u8 {
        let mut e = png::Encoder::new(
            fs::File::create(root.join(format!("clips/c/Frames/f_{i}.png"))).unwrap(),
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
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
}
fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton {
        pos: at,
        button: PointerButton::Primary,
        pressed,
        modifiers: Modifiers::NONE,
    };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
    run(ctx, app, vec![]);
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
fn no_dialogs() -> Backend {
    Arc::new(|k, _| panic!("a real dialog ({k:?}) must never open in tests"))
}
fn start(tag: &str, cache_mb: Option<usize>, saved: Option<&str>) -> (egui::Context, App, PathBuf) {
    let sp = tmp(&format!("{tag}-cfg")).join("settings.json");
    if let Some(body) = saved {
        fs::write(&sp, body).unwrap();
    }
    let ctx = egui::Context::default();
    let opts = AppOptions {
        project: Some(project(tag, "p")),
        settings_path: Some(sp.clone()),
        cache_mb,
        ..Default::default()
    };
    let mut app = App::with_picker(&ctx, opts, no_dialogs());
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    (ctx, app, sp)
}
fn open_page(ctx: &egui::Context, app: &mut App) {
    let at = app.settings_rect().expect("Settings button drawn").center();
    click(ctx, app, at);
    assert!(app.settings_open());
}
fn click_apply(ctx: &egui::Context, app: &mut App) {
    let at = app
        .settings_page()
        .apply_rect()
        .expect("Apply drawn")
        .center();
    click(ctx, app, at);
}
fn back(ctx: &egui::Context, app: &mut App) {
    let at = app.settings_back_rect().expect("Back drawn").center();
    click(ctx, app, at);
    assert!(!app.settings_open());
}

#[test]
fn button_opens_from_batch_and_cancel_discards_the_draft() {
    let (ctx, mut app, sp) = start("batch", None, None);
    assert!(!app.in_workspace());
    open_page(&ctx, &mut app);
    app.settings_page().draft_mut().playback.decode_workers = 5;
    back(&ctx, &mut app);
    // Reopening starts from saved values; nothing was written.
    open_page(&ctx, &mut app);
    assert_eq!(app.settings_page().draft().playback.decode_workers, 0);
    app.settings_page().draft_mut().playback.decode_workers = 5;
    let at = app.settings_page().cancel_rect().unwrap().center();
    click(&ctx, &mut app, at);
    assert!(!app.settings_open());
    assert_eq!(app.settings_page().saved(), &Settings::default());
    assert!(!sp.exists());
}

#[test]
fn workspace_state_survives_the_page_and_shortcuts_are_off_while_it_is_open() {
    let (ctx, mut app, _) = start("ws", None, None);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    let snap = |a: &App| {
        let v = a.viewer().unwrap();
        (v.clip, v.playhead, v.range, v.ab, v.linear, v.looping)
    };
    app.viewer_mut_for_test(|v| {
        v.set_playhead(3);
        v.set_in();
        v.set_playhead(2);
        v.set_out();
    });
    let before = snap(&app);
    app.viewer_mut_for_test(|v| v.toggle_play(1));
    open_page(&ctx, &mut app);
    let v = app.viewer().unwrap();
    assert!(
        v.clock.is_none() && v.buffering().is_none(),
        "opening the page pauses playback"
    );
    assert_eq!(snap(&app), before);
    // L, Space, Home, B would all change the workspace; none may reach it.
    for k in [
        egui::Key::L,
        egui::Key::Space,
        egui::Key::Home,
        egui::Key::B,
    ] {
        run(&ctx, &mut app, key(k));
    }
    assert!(app.in_workspace() && app.settings_open());
    assert_eq!(snap(&app), before);
    assert!(app.viewer().unwrap().clock.is_none() && app.viewer().unwrap().buffering().is_none());
    back(&ctx, &mut app);
    assert!(app.in_workspace());
    assert_eq!(
        snap(&app),
        before,
        "same clip, range and playhead after leaving the page"
    );
    // Esc is Cancel on the page.
    open_page(&ctx, &mut app);
    run(&ctx, &mut app, key(egui::Key::Escape));
    assert!(!app.settings_open() && app.in_workspace());
}

#[test]
fn apply_persists_rescales_now_and_reaches_the_next_viewer_only() {
    let (ctx, mut app, sp) = start("apply", None, None);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    let open_cfg = app.viewer().unwrap().loader_config();
    assert_eq!(
        open_cfg.1,
        ck::frames::default_cache_mb() << 20,
        "unset RAM budget = the dynamic default"
    );
    open_page(&ctx, &mut app);
    {
        let d = app.settings_page().draft_mut();
        d.interface.ui_scale = 1.5;
        d.playback.decode_workers = 2;
        d.playback.cache_mib = 300;
        d.playback.loop_default = false;
    }
    click_apply(&ctx, &mut app);
    assert!(app.settings_open(), "Apply keeps the page open");
    assert_eq!(ctx.zoom_factor(), 1.5);
    assert_eq!(
        app.viewer().unwrap().loader_config(),
        open_cfg,
        "open clip's loader is not rebuilt"
    );
    assert!(app.viewer().unwrap().looping);
    let saved = Store::load(Some(sp.clone()));
    assert_eq!(
        (saved.saved().playback.cache_mib, saved.saved().workers()),
        (300, 2)
    );
    back(&ctx, &mut app);
    // Next open clip consumes the saved values.
    app.open_clip(&ctx, 0);
    let v = app.viewer().unwrap();
    assert_eq!(v.loader_config(), (2, 300 << 20));
    assert!(!v.looping);
    // A fresh launch loads the saved scale before the first frame.
    let ctx2 = egui::Context::default();
    let mut fresh = App::with_picker(
        &ctx2,
        AppOptions {
            settings_path: Some(sp),
            ..Default::default()
        },
        no_dialogs(),
    );
    run(&ctx2, &mut fresh, vec![]);
    assert_eq!(ctx2.zoom_factor(), 1.5);
}

#[test]
fn explicit_cli_cache_wins_over_saved_ram_budget() {
    let (ctx, mut app, _) = start(
        "cli",
        Some(7),
        Some(r#"{"version":1,"playback":{"cache_mib":300,"decode_workers":3}}"#),
    );
    app.open_clip(&ctx, 0);
    assert_eq!(app.viewer().unwrap().loader_config(), (3, 7 << 20));
}

#[test]
fn picked_projects_and_dropped_files_wait_while_the_page_is_open() {
    let other = project("held-b", "other");
    let backend: Backend = {
        let o = other.clone();
        Arc::new(move |k, _| {
            assert_eq!(k, PickKind::Project);
            vec![o.clone()]
        })
    };
    let sp = tmp("held-cfg").join("settings.json");
    let ctx = egui::Context::default();
    let first = project("held-a", "first");
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            project: Some(first.clone()),
            settings_path: Some(sp),
            ..Default::default()
        },
        backend,
    );
    run(&ctx, &mut app, vec![]);
    open_page(&ctx, &mut app);
    assert!(app.request_pick(&ctx, PickKind::Project));
    let d = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.picking() {
        run(&ctx, &mut app, vec![]);
        assert!(std::time::Instant::now() < d);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    // A dropped folder is ignored (not queued) while the page is showing.
    let dropped = Path::new(&other).join("clips");
    #[derive(Debug)]
    struct Dropped(PathBuf);
    impl egui::DroppedFile for Dropped {
        fn path(&self) -> &Path {
            &self.0
        }
        fn bytes(&self) -> Result<Vec<u8>, String> {
            Err("unused".into())
        }
    }
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(1440.0, 900.0),
        )),
        dropped_files: vec![Arc::new(Dropped(dropped.clone()))],
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    assert!(!app.import_active());
    assert_eq!(
        app.project_root(),
        Some(first.as_path()),
        "pick is held under the page"
    );
    back(&ctx, &mut app);
    run(&ctx, &mut app, vec![]);
    assert_eq!(
        app.project_root(),
        Some(other.as_path()),
        "applied once the Board shows again"
    );
    assert!(!app.import_active());
}

#[derive(Debug)]
struct Dropped(PathBuf);
impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Err("unused".into())
    }
}
fn drop_folder(ctx: &egui::Context, app: &mut App, p: PathBuf) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(1440.0, 900.0),
        )),
        dropped_files: vec![Arc::new(Dropped(p))],
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
}

#[test]
fn drop_in_workspace_is_refused_visibly_and_keeps_the_open_clip() {
    let (ctx, mut app, _) = start("drop-ws", None, None);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    drop_folder(&ctx, &mut app, tmp("drop-ws-src"));
    run(&ctx, &mut app, vec![]);
    assert!(app.in_workspace(), "the open clip is not discarded");
    assert!(!app.import_active());
    assert!(app.notice().unwrap().contains("go back to the batch"));
}

#[test]
fn drop_on_settings_is_refused_and_keeps_the_open_clip() {
    let (ctx, mut app, _) = start("drop-set", None, None);
    app.open_clip(&ctx, 0);
    run(&ctx, &mut app, vec![]);
    app.viewer_mut_for_test(|v| {
        v.set_playhead(3);
        v.set_in();
        v.set_playhead(2);
        v.set_out();
    });
    let snap = |a: &App| {
        let v = a.viewer().unwrap();
        (v.clip, v.playhead, v.range)
    };
    let before = snap(&app);
    open_page(&ctx, &mut app);
    drop_folder(&ctx, &mut app, tmp("drop-set-src"));
    assert!(!app.import_active());
    back(&ctx, &mut app);
    run(&ctx, &mut app, vec![]);
    assert!(app.in_workspace() && !app.import_active());
    assert!(app.notice().unwrap().contains("close Settings"));
    assert_eq!(snap(&app), before, "same clip, playhead and range");
}

#[test]
fn drop_while_a_dialog_is_open_is_refused_visibly() {
    let release = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let r = release.clone();
    let backend: Backend = Arc::new(move |_, _| {
        while !r.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        vec![]
    });
    let ctx = egui::Context::default();
    let mut app = App::with_picker(
        &ctx,
        AppOptions {
            settings_path: Some(tmp("drop-pick-cfg").join("settings.json")),
            ..Default::default()
        },
        backend,
    );
    run(&ctx, &mut app, vec![]);
    assert!(app.request_pick(&ctx, PickKind::Clips));
    drop_folder(&ctx, &mut app, tmp("drop-pick-src"));
    assert!(!app.import_active());
    assert!(app.notice().unwrap().contains("file dialog is open"));
    release.store(true, std::sync::atomic::Ordering::SeqCst);
    let d = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while app.picking() {
        run(&ctx, &mut app, vec![]);
        assert!(std::time::Instant::now() < d);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
}
