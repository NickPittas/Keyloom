//! Per-installation Path Mapping: backward-compatible settings persistence and the Path mapping pane wired into
//! the real `App`. Browse and Test are button presses; their behaviour is driven through the controller in the
//! `settings` unit tests. Temp files only.
use ck::settings::{Settings, Store};
use ck::shared_paths::SharedRoot;
use ck::ui::picker::Backend;
use ck::ui::{App, AppOptions};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ck-pathmap-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn root(n: &str, l: &str) -> SharedRoot {
    SharedRoot { name: n.into(), local: l.into(), ..Default::default() }
}

const OLD: &str = r#"{"version":1,"future_key":{"a":1},"playback":{"cache_mib":512},"ai_targets":[{"id":"gpu-box","kind":"ssh","host":"h","user":null,"port":null,"python":"/p","repo":"/r","gpu":0,"storage":"shared","path_map":[["/mnt/NAS","/srv/nas"]],"enabled":true},{"id":"l0","kind":"local","repo":"/r","python":"/p","gpu":0}]}"#;

#[test]
fn old_ssh_targets_are_dropped_on_load_and_the_rest_survives() {
    let p = tmp("old").join("s.json");
    fs::write(&p, OLD).unwrap();
    let st = Store::load(Some(p));
    assert!(st.problem.is_none(), "{:?}", st.problem);
    let s = st.saved();
    assert_eq!(s.ai_targets.len(), 1);
    assert_eq!(s.ai_targets[0].id, "l0");
    assert!(s.shared_roots.is_empty());
    assert_eq!(s.playback.cache_mib, 512);
    assert_eq!(s.extra["future_key"]["a"], 1);
}

#[test]
fn invalid_mappings_are_refused_and_nothing_is_written() {
    let p = tmp("bad").join("s.json");
    let mut st = Store::load(Some(p.clone()));
    for bad in [
        vec![root("A", "/mnt/a"), root("a", "/mnt/b")],
        vec![root("A", "/mnt/a"), root("B", "/mnt/a/x")],
        vec![root("A", "relative/dir")],
        vec![root("A", "/mnt/../etc")],
        vec![root("", "/mnt/a")],
    ] {
        let s = Settings { shared_roots: bad, ..Default::default() };
        assert!(st.save(s).is_err());
    }
    assert!(!p.exists());
    // A hand-edited unusable row loads (see the foreign-mapping test) but is still never saveable.
    fs::write(&p, r#"{"version":1,"shared_roots":[{"name":"A","local":"rel"}]}"#).unwrap();
    let mut st = Store::load(Some(p));
    assert!(st.problem.is_none() && st.saved().mapping_problem().is_some());
    assert!(st.save(st.saved().clone()).is_err());
}

fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(1440.0, 900.0))),
        events,
        ..Default::default()
    };
    ctx.run_ui(raw, |ui| app.frame(ui)).textures_delta.clear();
}
fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    run(ctx, app, vec![Event::PointerMoved(at)]);
    run(ctx, app, vec![b(true)]);
    run(ctx, app, vec![b(false)]);
    run(ctx, app, vec![]);
}

#[test]
fn applied_mapping_row_is_saved_and_reloaded() {
    let base = tmp("apply");
    let share = base.join("share dir é");
    fs::create_dir_all(&share).unwrap();
    let project = base.join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let sp = base.join("cfg/settings.json");

    let backend: Backend = Arc::new(|_, _| vec![]);
    let ctx = egui::Context::default();
    let opts = AppOptions { project: Some(project), settings_path: Some(sp.clone()), ..Default::default() };
    let mut app = App::with_picker(&ctx, opts, backend);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let at = app.settings_rect().expect("Settings button").center();
    click(&ctx, &mut app, at);
    assert!(app.settings_open());
    app.settings_page().select_path_mapping();
    run(&ctx, &mut app, vec![]);
    app.settings_page().draft_mut().shared_roots.push(root("NAS", share.to_str().unwrap()));
    assert!(app.settings_page().dirty());
    assert!(app.settings_page().apply());
    assert_eq!(Store::load(Some(sp)).saved().shared_roots, vec![root("NAS", share.to_str().unwrap())]);
}

#[test]
fn unusable_mapping_keeps_other_settings_and_unknown_row_keys_and_refuses_save_until_fixed() {
    let p = tmp("foreign").join("s.json");
    let body = r#"{"version":1,"playback":{"cache_mib":2048},"shared_roots":[{"name":"NAS","local":"Z:\\Shared","note":{"k":1}},{"name":"Rel","local":"rel"}]}"#;
    fs::write(&p, body).unwrap();
    let mut st = Store::load(Some(p.clone()));
    assert!(st.problem.is_none(), "the file is used: {:?}", st.problem);
    assert_eq!(st.saved().playback.cache_mib, 2048, "unrelated settings survive");
    assert_eq!(st.saved().shared_roots.len(), 2, "rows are kept");
    // The pane shows the problem for those rows.
    let page = ck::ui::settings::Page::new(Store::load(Some(p.clone())));
    assert!(page.mapping_problem().is_some_and(|e| e.contains("not an absolute path")), "{:?}", page.mapping_problem());
    // Other genuinely invalid settings still make the file unusable.
    let bad = tmp("foreign2").join("s.json");
    fs::write(&bad, r#"{"version":1,"ai":{"hint_quality":3}}"#).unwrap();
    assert!(Store::load(Some(bad)).problem.is_some());
    // Saving the rows as they are is refused and the original file is untouched.
    let before = fs::read(&p).unwrap();
    let mut edited = st.saved().clone();
    edited.playback.cache_mib = 4096;
    assert!(st.save(edited.clone()).is_err());
    assert_eq!(fs::read(&p).unwrap(), before);
    // Fixed rows save; the unknown key inside a row round-trips.
    edited.shared_roots = vec![SharedRoot { local: "/mnt/nas".into(), ..edited.shared_roots[0].clone() }];
    st.save(edited).unwrap();
    let again = Store::load(Some(p));
    assert_eq!(again.saved().playback.cache_mib, 4096);
    assert_eq!(again.saved().shared_roots[0].local, "/mnt/nas");
    assert_eq!(again.saved().shared_roots[0].extra["note"]["k"], 1);
}

#[test]
fn added_row_keeps_name_and_folder_through_apply_and_removal() {
    for with_project in [true, false] {
        let base = tmp(&format!("typing{with_project}"));
        let project = base.join("proj");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
        let sp = base.join("cfg/settings.json");
        let ctx = egui::Context::default();
        let opts = AppOptions { project: with_project.then_some(project), settings_path: Some(sp.clone()), ..Default::default() };
        let mut app = App::with_picker(&ctx, opts, Arc::new(|_, _| vec![]));
        for _ in 0..3 {
            run(&ctx, &mut app, vec![]);
        }
        let at = app.settings_rect().expect("Settings button").center();
        click(&ctx, &mut app, at);
        app.settings_page().select_path_mapping();
        run(&ctx, &mut app, vec![]);
        for round in 0..3 {
            // Add a row and fill in its name and folder (what typing into the fields writes to the draft).
            let d = app.settings_page().draft_mut();
            d.shared_roots.push(SharedRoot::default());
            d.shared_roots[0].name = "NAS".into();
            d.shared_roots[0].local = "/mnt/nas".into();
            assert_eq!(app.settings_page().draft().shared_roots[0].name, "NAS", "w{with_project} round {round}: name");
            assert_eq!(app.settings_page().draft().shared_roots[0].local, "/mnt/nas", "w{with_project} round {round}: folder");
            assert!(app.settings_page().apply(), "w{with_project} round {round}: apply");
            assert_eq!(Store::load(Some(sp.clone())).saved().shared_roots, vec![root("NAS", "/mnt/nas")]);
            // Remove the applied row so the next round starts from row 0 again.
            app.settings_page().draft_mut().shared_roots.clear();
            assert!(app.settings_page().apply());
            run(&ctx, &mut app, vec![]);
        }
    }
}
