//! Per-installation Path Mapping: backward-compatible settings persistence and the Settings pane wired into
//! the real `App` with a mocked folder picker (no native dialog). Temp files only.
use ck::settings::{Settings, Store};
use ck::shared_paths::SharedRoot;
use ck::ui::picker::{Backend, PickKind};
use ck::ui::{App, AppOptions};
use eframe::egui::{self, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ck-pathmap-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}

fn root(n: &str, l: &str) -> SharedRoot {
    SharedRoot { name: n.into(), local: l.into(), ..Default::default() }
}

const OLD: &str = r#"{"version":1,"future_key":{"a":1},"ai_targets":[{"id":"gpu-box","kind":"ssh","host":"h","user":null,"port":null,"python":"/p","repo":"/r","gpu":0,"storage":"shared","path_map":[["/mnt/NAS","/srv/nas"]],"enabled":true}]}"#;

#[test]
fn old_settings_load_unchanged_and_survive_a_save() {
    let p = tmp("old").join("s.json");
    fs::write(&p, OLD).unwrap();
    let mut st = Store::load(Some(p.clone()));
    assert!(st.problem.is_none(), "{:?}", st.problem);
    let s = st.saved().clone();
    assert!(s.shared_roots.is_empty());
    assert_eq!(s.ai_targets[0].path_map, vec![("/mnt/NAS".into(), "/srv/nas".into())]);
    let mut n = s.clone();
    n.shared_roots.push(root("NAS", "/mnt/nas"));
    st.save(n).unwrap();
    let again = Store::load(Some(p));
    assert_eq!(again.saved().ai_targets, s.ai_targets, "SSH target map is not converted");
    assert_eq!(again.saved().extra["future_key"]["a"], 1);
    assert_eq!(again.saved().shared_roots, vec![root("NAS", "/mnt/nas")]);
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

static W: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1440);
fn run(ctx: &egui::Context, app: &mut App, events: Vec<Event>) {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(W.load(std::sync::atomic::Ordering::SeqCst) as f32, 900.0))),
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
fn pane_browse_test_and_apply_use_the_mocked_picker() {
    let base = tmp("ui");
    let share = base.join("share dir é");
    fs::create_dir_all(&share).unwrap();
    fs::write(share.join("clip.mov"), b"user data").unwrap();
    let project = base.join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let sp = base.join("cfg/settings.json");

    let asked: Arc<Mutex<Vec<PickKind>>> = Default::default();
    let (a, answer) = (asked.clone(), share.clone());
    let dialogs: Backend = Arc::new(move |k, _| {
        a.lock().unwrap().push(k);
        vec![answer.clone()]
    });
    let ctx = egui::Context::default();
    let opts = AppOptions { project: Some(project), settings_path: Some(sp.clone()), ..Default::default() };
    let mut app = App::with_picker(&ctx, opts, dialogs);
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let at = app.settings_rect().expect("Settings button").center();
    click(&ctx, &mut app, at);
    assert!(app.settings_open());
    app.settings_page().select_path_mapping();
    run(&ctx, &mut app, vec![]);
    app.settings_page().draft_mut().shared_roots.push(root("NAS", ""));
    run(&ctx, &mut app, vec![]);

    let at = app.settings_page().map_rect("browse", 0).expect("Browse drawn").center();
    click(&ctx, &mut app, at);
    for _ in 0..50 {
        if app.settings_page().draft().shared_roots[0].local.len() > 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        run(&ctx, &mut app, vec![]);
    }
    assert_eq!(*asked.lock().unwrap(), vec![PickKind::MappingRoot]);
    assert_eq!(app.settings_page().draft().shared_roots[0].local, share.to_str().unwrap());

    let at = app.settings_page().map_rect("test", 0).expect("Test drawn").center();
    click(&ctx, &mut app, at);
    for _ in 0..100 {
        if app.settings_page().access_of(0).is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
        run(&ctx, &mut app, vec![]);
    }
    let acc = app.settings_page().access_of(0).expect("test result");
    assert!(acc.read.is_ok() && acc.write.is_ok(), "{acc:?}");
    let left: Vec<_> = fs::read_dir(&share).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(left, vec![std::ffi::OsString::from("clip.mov")], "probe file removed, user data untouched");

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

fn type_into(ctx: &egui::Context, app: &mut App, what: &str, row: usize, text: &str) {
    let at = app.settings_page().map_rect(what, row).unwrap_or_else(|| panic!("{what} {row} drawn")).center();
    click(ctx, app, at);
    // What winit sends when a field takes focus: IME enabled and an empty preedit; then the key text.
    let pre = Event::Ime(egui::ImeEvent::Preedit { text: String::new(), active_range_chars: None });
    run(ctx, app, vec![pre.clone(), pre]);
    run(ctx, app, vec![Event::Text(text.into())]);
    run(ctx, app, vec![]);
}

#[test]
fn name_and_folder_accept_typing_across_add_and_apply() {
  for (w, with_project) in [(1440, true), (1440, false), (800, false), (500, false), (360, false)] {
    W.store(w, std::sync::atomic::Ordering::SeqCst);
    let base = tmp(&format!("typing{w}{with_project}"));
    let project = base.join("proj");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let ctx = egui::Context::default();
    let opts = AppOptions { project: with_project.then_some(project), settings_path: Some(base.join("cfg/settings.json")), ..Default::default() };
    let mut app = App::with_picker(&ctx, opts, Arc::new(|_, _| vec![]));
    for _ in 0..3 {
        run(&ctx, &mut app, vec![]);
    }
    let at = app.settings_rect().expect("Settings button").center();
    click(&ctx, &mut app, at);
    app.settings_page().select_path_mapping();
    run(&ctx, &mut app, vec![]);
    for round in 0..3 {
        let at = app.settings_page().map_rect("add", 0).expect("Add drawn").center();
        click(&ctx, &mut app, at);
        run(&ctx, &mut app, vec![]);
        type_into(&ctx, &mut app, "name", 0, "NAS");
        assert_eq!(app.settings_page().draft().shared_roots[0].name, "NAS", "w{w} p{with_project} round {round}: name");
        type_into(&ctx, &mut app, "local", 0, "/mnt/nas");
        assert_eq!(app.settings_page().draft().shared_roots[0].local, "/mnt/nas", "w{w} p{with_project} round {round}: folder");
        let at = app.settings_page().apply_rect().expect("Apply drawn").center();
        click(&ctx, &mut app, at);
        // Remove the applied row so the next round starts from row 0 again.
        app.settings_page().draft_mut().shared_roots.clear();
        assert!(app.settings_page().apply());
        run(&ctx, &mut app, vec![]);
    }
}
}
