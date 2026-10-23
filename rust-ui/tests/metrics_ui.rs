//! Always-visible CPU/RAM/GPU/VRAM: the footer is on every main page with a fixed rectangle, shows real numbers
//! or honest dashes, and the Clients cards show the same values or say why there are none. Private fixtures,
//! a fake helper process, headless egui; no counters, GPU, display or network.
mod common;
use ck::metrics::{ClientTelemetry, Sample, Source};
use ck::ui::{App, AppOptions};
use eframe::egui::{self, epaint::Shape, Event, Modifiers, PointerButton, RawInput, Rect, Vec2};
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
fn click(ctx: &egui::Context, app: &mut App, at: egui::Pos2) -> String {
    let b = |pressed| Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
    painted(ctx, app, vec![Event::PointerMoved(at)]);
    painted(ctx, app, vec![b(true)]);
    painted(ctx, app, vec![b(false)]);
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
        {"id":"l0","kind":"local","repo":repo,"python":common::PY,"gpu":0},
        {"id":"far","kind":"ssh","host":"h","user":null,"port":null,"python":"/p","repo":"/r","gpu":0,"storage":"shared","path_map":[],"enabled":true}]}).to_string()).unwrap();
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
fn the_footer_is_on_board_clip_settings_and_clients_with_one_fixed_rectangle() {
    let e = env("mui-pages");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), None);
    let footer = a.metrics_rect().expect("footer drawn on the Board");
    assert_eq!(footer.height(), ck::ui::metrics_bar::FOOTER_H);
    assert_eq!(footer.max.y, SCREEN.y, "outermost bottom panel");
    let text = painted(&ctx, &mut a, vec![]);
    assert!(text.contains("CPU 37%") && text.contains("RAM 12.0 / 32.0 GiB"), "{text}");
    assert!(text.contains("GPU0 Fixture RTX 63% · VRAM 10.0 / 24.0 GiB") && text.contains("GPU1 Fixture RTX 4% · VRAM 1.0 / 24.0 GiB"), "every GPU, idle included: {text}");
    // Settings
    let at = a.settings_rect().unwrap().center();
    let t = click(&ctx, &mut a, at);
    assert!(a.settings_open() && t.contains("CPU 37%"), "visible on Settings");
    assert_eq!(a.metrics_rect(), Some(footer));
    let back = a.settings_back_rect().unwrap().center();
    click(&ctx, &mut a, back);
    // Clients
    let at = a.clients_rect().expect("Clients button").center();
    let t = click(&ctx, &mut a, at);
    assert!(a.clients_open() && t.contains("CPU 37%"), "visible on Clients");
    assert_eq!(a.metrics_rect(), Some(footer));
    let _ = fs::remove_dir_all(&e.base);
    // Clip workspace
    let e = env("mui-clip");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), Some("a"));
    assert!(a.in_workspace());
    assert_eq!(a.metrics_rect(), Some(footer));
    assert!(painted(&ctx, &mut a, vec![]).contains("RAM 12.0 / 32.0 GiB"));
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn the_footer_never_moves_when_the_values_or_the_status_change() {
    let e = env("mui-stable");
    let (_, off) = app(&e, Source::Off, None);
    let (ctx, mut live) = app(&e, Source::Fixture(fixture(&e)), None);
    assert_eq!(off.metrics_rect(), live.metrics_rect());
    let (c2, mut off2) = app(&e, Source::Off, None);
    let t = painted(&c2, &mut off2, vec![]);
    assert!(t.contains("CPU —") && t.contains("RAM —") && t.contains("unavailable: not started"), "honest, no zeros: {t}");
    assert!(!t.contains("CPU 0%") && !t.contains("0.0 / 0.0"));
    let _ = painted(&ctx, &mut live, vec![]);
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
        if t.contains("CPU 42%") {
            break t;
        }
        assert!(std::time::Instant::now() < end, "no live sample: {t}");
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    assert!(text.contains("GPU0 Fake GPU 55% · VRAM 2.0 / 8.0 GiB"), "{text}");
    drop(a); // drop = kill + reap
    let _ = fs::remove_dir_all(&e.base);
}

#[test]
fn client_cards_show_local_numbers_and_never_invent_remote_ones() {
    let e = env("mui-cards");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), None);
    let at = a.clients_rect().unwrap().center();
    let t = click(&ctx, &mut a, at);
    assert!(a.clients_open());
    let cards = t.matches("CPU 37% · RAM 12.0 / 32.0 GiB").count();
    assert_eq!(cards, 1, "only this computer has telemetry: {t}");
    assert!(t.contains("no telemetry: SSH workers do not report it"), "{t}");
    // The network API will fill remote telemetry through this hook: same cells, server-computed age.
    let (s, age) = Sample::from_api(&json!({"seq": 2, "cpu_percent": 91.0, "ram_used_bytes": 3u64<<30, "ram_total_bytes": 8u64<<30, "measured_age_s": 2.0,
        "gpus": [{"uuid": "GPU-r", "index": 0, "name": "Remote GPU", "utilization_percent": 12.0, "vram_used_bytes": 1u64<<30, "vram_total_bytes": 4u64<<30}]})).unwrap().unwrap();
    let t0 = std::time::Instant::now();
    a.set_client_telemetry("far", Some(ClientTelemetry::reported_at(s.clone(), age, Some(0.3), t0)));
    let t = painted(&ctx, &mut a, vec![]);
    assert!(t.contains("CPU 91% · RAM 3.0 / 8.0 GiB") && t.contains("Remote GPU"), "{t}");
    a.set_client_telemetry("far", Some(ClientTelemetry::reported(s, Some(120.0), None)));
    let t = painted(&ctx, &mut a, vec![]);
    assert!(t.contains("stale 120 s"), "{t}");
    a.set_client_telemetry("far", Some(ClientTelemetry::NotConnected));
    let t = painted(&ctx, &mut a, vec![]);
    assert!(t.contains("not connected"), "{t}");
    let _ = fs::remove_dir_all(&e.base);
}

/// Paint at `width` and return the footer's text shapes: (text, elided, right edge).
fn footer_texts(ctx: &egui::Context, app: &mut App, width: f32) -> Vec<(String, bool, f32)> {
    let size = Vec2::new(width, 700.0);
    let mut out = ctx.run_ui(RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, size)), ..Default::default() }, |ui| app.frame(ui));
    out.textures_delta.clear();
    let foot = app.metrics_rect().expect("footer");
    assert_eq!(foot.max.y, size.y, "footer is the bottom row at width {width}");
    out.shapes
        .iter()
        .filter_map(|c| match &c.shape {
            Shape::Text(t) if t.pos.y >= foot.min.y - 1.0 && t.pos.y <= foot.max.y => Some((t.galley.text().to_string(), t.galley.elided, t.pos.x + t.galley.size().x)),
            _ => None,
        })
        .collect()
}

#[test]
fn every_gpus_utilisation_and_vram_stay_visible_unclipped_from_wide_to_narrow() {
    let e = env("mui-narrow");
    let (ctx, mut a) = app(&e, Source::Fixture(fixture(&e)), None);
    for (width, must) in [
        (1600.0, vec!["GPU0 Fixture RTX 63% · VRAM 10.0 / 24.0 GiB", "GPU1 Fixture RTX 4% · VRAM 1.0 / 24.0 GiB"]),
        (900.0, vec!["G0 63% 10.0/24.0G", "G1 4% 1.0/24.0G"]),
        (640.0, vec!["GPU 63% 11/48G"]),
    ] {
        for _ in 0..3 {
            footer_texts(&ctx, &mut a, width);
        }
        let texts = footer_texts(&ctx, &mut a, width);
        let joined: String = texts.iter().map(|t| t.0.as_str()).collect::<Vec<_>>().join(" | ");
        for m in &must {
            assert!(joined.contains(m), "width {width}: {m:?} missing from {joined:?}");
        }
        assert!(texts.iter().all(|t| !t.1), "width {width}: a footer label was elided (VRAM clipped): {texts:?}");
        assert!(texts.iter().all(|t| t.2 <= width), "width {width}: painted outside the window: {texts:?}");
        assert!(joined.contains("CPU 37%") && joined.contains("RAM 12.0 / 32.0 GiB"), "{joined}");
    }
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
