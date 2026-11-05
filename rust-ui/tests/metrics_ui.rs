//! Always-visible CPU/RAM/GPU/VRAM: the status footer is on every main page, shows real numbers
//! or honest dashes, and remote cells never invent numbers. Private fixtures,
//! a fake helper process, headless egui; no counters, GPU, display or network.
mod common;
use ck::metrics::{client_cells_at, ClientTelemetry, Sample, Source};
use ck::ui::{App, AppOptions};
use eframe::egui::{self, epaint::Shape, Event, Modifiers, RawInput, Rect, Vec2};
use serde_json::json;
use std::fs;
use std::path::PathBuf;

const SCREEN: Vec2 = Vec2::new(1440.0, 900.0);

fn painted(ctx: &egui::Context, app: &mut App, events: Vec<Event>) -> String {
    let raw = RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, SCREEN)), events, ..Default::default() };
    let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
    out.textures_delta.clear();
    let mut s = String::new();
    for c in &out.shapes {
        if let Shape::Text(t) = &c.shape {
            s.push_str(&t.galley.text());
            s.push('\n');
        }
    }
    s
}
/// True when `label` is painted and `value` is painted after it (paint order, one text shape per line).
fn has_pair(texts: &str, label: &str, value: &str) -> bool {
    let mut lines = texts.lines();
    lines.any(|l| l == label) && lines.any(|l| l == value)
}
/// Ctrl+Shift+N page shortcut (3 = Machines, 5 = Settings); returns the painted text after the switch.
fn page(ctx: &egui::Context, app: &mut App, k: egui::Key) -> String {
    let ev = [true, false].map(|pressed| Event::Key { key: k, physical_key: Some(k), pressed, repeat: false, modifiers: Modifiers::COMMAND | Modifiers::SHIFT });
    painted(ctx, app, ev.to_vec());
    painted(ctx, app, vec![])
}

struct Env {
    base: PathBuf,
    project: PathBuf,
    settings: PathBuf,
}
fn env(tag: &str) -> Env {
    let base = common::tmp(tag);
    let project = base.join("proj");
    let c = project.join("clips/a");
    fs::create_dir_all(c.join("Frames")).unwrap();
    for i in 0..3u32 {
        let mut e = png::Encoder::new(fs::File::create(c.join(format!("Frames/f_{}.png", 1101 + i))).unwrap(), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
    }
    fs::write(c.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    fs::write(c.join("clip.json"), "{}").unwrap();
    fs::write(project.join("project.json"), r#"{"version":2}"#).unwrap();
    let repo = common::make_repo(&base, "repo", json!({}));
    let settings = base.join("settings.json");
    fs::write(&settings, json!({"version":1,"ai_targets":[
        {"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0}]}).to_string()).unwrap();
    Env { base, project, settings }
}
fn fixture(e: &Env) -> PathBuf {
    let f = e.base.join("metrics.jsonl");
    fs::write(&f, json!({"v":1,"type":"system_metrics","seq":4,"cpu_percent":37.0,"ram_used_bytes":12u64<<30,"ram_total_bytes":32u64<<30,
        "gpus":[{"uuid":"GPU-aaa","index":0,"name":"Fixture RTX","utilization_percent":63.0,"vram_used_bytes":10u64<<30,"vram_total_bytes":24u64<<30},
                {"uuid":"GPU-bbb","index":1,"name":"Fixture RTX","utilization_percent":4.0,"vram_used_bytes":1u64<<30,"vram_total_bytes":24u64<<30}],"errors":{}}).to_string() + "\n").unwrap();
    f
}
fn app(e: &Env, metrics: Source, open: Option<&str>) -> (egui::Context, App) {
    let ctx = egui::Context::default();
    let mut a = App::with_ctx(&ctx, AppOptions { project: Some(e.project.clone()), settings_path: Some(e.settings.clone()), open_clip: open.map(Into::into), metrics, ..Default::default() });
    for _ in 0..12 {
        painted(&ctx, &mut a, vec![]);
    }
    (ctx, a)
}

#[test]
fn the_footer_is_on_board_clip_settings_and_machines() {
    let e = env("mui-pages");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), None);
    let text = painted(&ctx, &mut a, vec![]);
    assert!(has_pair(&text, "CPU", "37%") && has_pair(&text, "RAM", "12.0 / 32.0 GB"), "{text}");
    assert!(has_pair(&text, "GPU0", "63%") && has_pair(&text, "VRAM", "10.0 / 24.0 GB"), "this machine's main GPU: {text}");
    // Settings is Ctrl+Shift+5; Machines (the clients page) is Ctrl+Shift+3.
    let t = page(&ctx, &mut a, egui::Key::Num5);
    assert!(a.settings_open() && has_pair(&t, "CPU", "37%"), "visible on Settings");
    let t = page(&ctx, &mut a, egui::Key::Num3);
    assert!(a.clients_open() && has_pair(&t, "CPU", "37%"), "visible on Machines");
    let _ = fs::remove_dir_all(&e.base);
    // Clip workspace
    let e = env("mui-clip");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), Some("a"));
    assert!(a.in_workspace());
    assert!(has_pair(&painted(&ctx, &mut a, vec![]), "RAM", "12.0 / 32.0 GB"));
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn the_footer_shows_real_numbers_or_honest_dashes() {
    let e = env("mui-stable");
    let (ctx, mut live) = app(&e, Source::Fixture(fixture(&e)), None);
    assert!(has_pair(&painted(&ctx, &mut live, vec![]), "CPU", "37%"));
    let (c2, mut off2) = app(&e, Source::Off, None);
    let t = painted(&c2, &mut off2, vec![]);
    assert!(has_pair(&t, "CPU", "—") && has_pair(&t, "RAM", "—"), "honest, no zeros: {t}");
    assert!(!has_pair(&t, "CPU", "0%") && !t.contains("0.0 / 0.0"));
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn a_real_helper_process_feeds_the_footer_and_is_reaped_with_the_app() {
    let e = env("mui-proc");
    let cmd = Source::Command(vec![common::PY.into(), "-I".into(), concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_metrics.py").into(), "run".into()]);
    let (ctx, mut a) = app(&e, cmd, None);
    let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let text = loop {
        let t = painted(&ctx, &mut a, vec![]);
        if has_pair(&t, "CPU", "42%") {
            break t;
        }
        assert!(std::time::Instant::now() < end, "no live sample: {t}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(has_pair(&text, "GPU0", "55%") && has_pair(&text, "VRAM", "2.0 / 8.0 GB"), "{text}");
    drop(a); // drop = kill + reap
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn client_cards_show_local_numbers_and_never_invent_remote_ones() {
    let e = env("mui-cards");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), None);
    let t = page(&ctx, &mut a, egui::Key::Num3);
    assert!(a.clients_open());
    // Machines page: this computer's worker starts when the app opens; depending on frame timing it has either not
    // reported yet (starting) or already answered (its GPU painted).
    let local_ok = (t.contains("starting") && t.contains("Not reported")) || (t.contains("Fake GPU") && t.contains("24 GB"));
    assert!(local_ok, "{t}");
    // Remote telemetry is painted by the Batch queue for a remote client with a run: these are the cells it paints.
    // The network API fills it through this model: same cells, server-computed age.
    let (s, age) = Sample::from_api(&json!({"seq": 2, "cpu_percent": 91.0, "ram_used_bytes": 3u64<<30, "ram_total_bytes": 8u64<<30, "measured_age_s": 2.0,
        "gpus": [{"uuid": "GPU-r", "index": 0, "name": "Remote GPU", "utilization_percent": 12.0, "vram_used_bytes": 1u64<<30, "vram_total_bytes": 4u64<<30}]})).unwrap().unwrap();
    let t0 = std::time::Instant::now();
    let c = client_cells_at(&ClientTelemetry::reported_at(s.clone(), age, Some(0.3), t0), &[], t0);
    assert_eq!((c.cpu.as_str(), c.ram.as_str(), c.status.as_deref()), ("91%", "3.0 / 8.0 GiB", None));
    assert!(c.gpu.contains("Remote GPU"), "{}", c.gpu);
    let c = client_cells_at(&ClientTelemetry::reported(s, Some(120.0), None), &[], t0);
    assert_eq!(c.status.as_deref(), Some("stale 120 s"));
    let c = client_cells_at(&ClientTelemetry::NotConnected, &[], t0);
    assert_eq!((c.cpu.as_str(), c.status.as_deref()), ("—", Some("not connected")));
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn a_remote_card_goes_stale_when_polling_stops_by_injected_clock() {
    use ck::metrics::client_cells_at;
    let (s, age) = Sample::from_api(&json!({"seq": 2, "cpu_percent": 5.0, "measured_age_s": 1.0})).unwrap().unwrap();
    let t0 = std::time::Instant::now();
    let t = ClientTelemetry::reported_at(s, age, Some(0.2), t0);
    assert_eq!(client_cells_at(&t, &[], t0 + std::time::Duration::from_secs(5)).status, None);
    assert_eq!(client_cells_at(&t, &[], t0 + std::time::Duration::from_secs(30)).status.as_deref(), Some("stale 31 s"));
}
