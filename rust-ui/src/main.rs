#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use ck::project::View;
use ck::ui::{App, AppOptions, Tab};
use eframe::egui;

fn usage() -> ! {
    eprintln!("usage: corridorkey-ui [--version | --self-check] [PROJECT_DIR] [--global-output-dir D] [--cache-mb N] [--window WxH]\n       [--open-clip NAME] [--tab media|hint|key|review|deliver] [--view input|hint|fg|matte|comp|processed]\n       [--ab VIEW] [--frame N] [--play] [--data-dir D (new projects go in D/Projects)] [--import FILE]... [--import-folder DIR]... [--auto-confirm]  (the last three: QA) [--keys i,o,Shift+Right,click:605x66,rclick:605x66 (synthetic egui events, QA)] [--screenshot FILE.png [--screenshot-after SECS]] [--metrics-fixture FILE.jsonl]  (dev/evidence options)");
    std::process::exit(2)
}

fn view(s: &str) -> View {
    View::ALL
        .into_iter()
        .find(|v| v.label().eq_ignore_ascii_case(s))
        .unwrap_or_else(|| usage())
}

/// `--version` / `--self-check`: no window, no worker. The self-check resolves the install descriptor the same
/// way the app does and reports it as one JSON line; exit 0 only when a usable runtime and Python were found.
fn headless(a: &[String]) -> Option<i32> {
    let version = env!("CARGO_PKG_VERSION");
    if a.iter().any(|x| x == "--version" || x == "-V") {
        println!("corridorkey-ui {version}");
        return Some(0);
    }
    if !a.iter().any(|x| x == "--self-check") {
        return None;
    }
    let l = ck::runtime::launch(None);
    let python = l
        .python
        .clone()
        .or_else(|| l.repo.as_ref().map(|r| ck::import::python(r)));
    let helper_ok = l
        .repo
        .as_ref()
        .is_some_and(|r| r.join("backend/worker/__main__.py").is_file());
    let py_ok = python.as_ref().is_some_and(|p| p.is_file());
    let ok = l.problem.is_none() && helper_ok && py_ok;
    println!(
        "{}",
        serde_json::json!({
            "app": "corridorkey-ui", "version": version, "ok": ok,
            "installed": l.install.is_some(),
            "repo": l.repo, "python": python, "worker_module": helper_ok, "python_found": py_ok,
            "problem": l.problem,
            "data_dir": ck::runtime::default_data_dir(&l),
        })
    );
    Some(if ok { 0 } else { 1 })
}

fn main() -> eframe::Result {
    let a: Vec<String> = std::env::args().skip(1).collect();
    if let Some(code) = headless(&a) {
        std::process::exit(code);
    }
    ck::ui::platform::prefer_xwayland();
    let mut o = AppOptions { metrics: ck::metrics::Source::Local, net: ck::netlink::Source::Local, ..Default::default() };
    let mut background = false;
    let mut size = (1440.0f32, 900.0f32);
    let mut i = 0;
    while i < a.len() {
        let next = |i: &mut usize| -> String {
            *i += 1;
            a.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match a[i].as_str() {
            "--global-output-dir" => o.global_output_dir = Some(next(&mut i).into()),
            "--cache-mb" => o.cache_mb = next(&mut i).parse().ok(),
            "--open-clip" => o.open_clip = Some(next(&mut i)),
            "--tab" => o.tab = Some(Tab::parse(&next(&mut i)).unwrap_or_else(|| usage())),
            "--view" => o.view = Some(view(&next(&mut i))),
            "--ab" => o.ab = Some(view(&next(&mut i))),
            "--play" => o.play = true,
            "--background" => background = true,
            "--data-dir" => o.data_dir = Some(next(&mut i).into()),
            "--repo" => o.repo = Some(next(&mut i).into()),
            "--import" => o.import_videos.push(next(&mut i).into()),
            "--import-folder" => o.import_folders.push(next(&mut i).into()),
            "--auto-confirm" => o.auto_confirm = true,
            "--keys" => o.keys = next(&mut i).split(',').map(str::to_owned).collect(),
            "--frame" => o.frame = next(&mut i).parse().ok(),
            "--metrics-fixture" => o.metrics = ck::metrics::Source::Fixture(next(&mut i).into()),
            "--screenshot" => o.screenshot = Some(next(&mut i).into()),
            "--screenshot-after" => o.screenshot_after = next(&mut i).parse().ok(),
            "--window" => {
                let s = next(&mut i);
                let (w, h) = s.split_once('x').unwrap_or_else(|| usage());
                size = (w.parse().unwrap_or(1440.0), h.parse().unwrap_or(900.0));
            }
            "-h" | "--help" => usage(),
            s if !s.starts_with("--") && o.project.is_none() => o.project = Some(s.into()),
            _ => usage(),
        }
        i += 1;
    }
    // One window/receiver per user: a second launch shows the first window and exits. Dev/QA runs are exempt.
    let dev = o.screenshot.is_some() || !o.keys.is_empty() || !matches!(o.metrics, ck::metrics::Source::Local);
    let settings_dir = ck::settings::default_path().and_then(|p| p.parent().map(std::path::Path::to_path_buf));
    let mut _lock = None;
    if !dev {
        // Fail closed: without the lock a second copy could run a duplicate network receiver.
        let guard = settings_dir.as_deref().ok_or_else(|| "no settings folder for the single-instance lock".to_string()).and_then(ck::background::acquire);
        match guard {
            Ok(ck::background::Instance::Other) => return Ok(()),
            Ok(ck::background::Instance::Primary(f)) => _lock = Some(f),
            Err(e) => {
                eprintln!("CorridorKey did not start: cannot take the single-instance lock ({e})");
                #[cfg(windows)]
                rfd::MessageDialog::new()
                    .set_title("CorridorKey could not start")
                    .set_description(format!("Cannot take the single-instance lock: {e}"))
                    .set_level(rfd::MessageLevel::Error)
                    .show();
                std::process::exit(1);
            }
        }
    }
    let saved = ck::settings::Store::load(ck::settings::default_path());
    if saved.saved().interface.launch_at_login {
        if let Err(e) = ck::background::sync_login(true) {
            eprintln!("sign-in entry: {e}");
            o.startup_notice = Some(format!("Start at sign-in is NOT active: {e}"));
        }
    }
    let start_background = background && saved.saved().interface.background;
    let instance_dir = _lock.as_ref().and(settings_dir);
    o.start_background = start_background && !cfg!(windows);
    o.instance_dir = instance_dir.clone().filter(|_| !cfg!(windows));
    let native = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CorridorKey")
            .with_inner_size([size.0, size.1])
            .with_min_inner_size([700.0, 660.0]),
        ..Default::default()
    };
    let result = eframe::run_native(
        "corridorkey-ui",
        native,
        Box::new(move |cc| {
            #[cfg(windows)]
            if let Some(dir) = instance_dir.clone() {
                if let Err(e) = ck::background::watch_windows(cc, dir, start_background) {
                    o.startup_notice = Some(format!("Background window control is unavailable: {e}"));
                }
            }
            Ok(Box::new(App::new(cc, o.clone())))
        }),
    );
    drop(_lock); // held for the whole run
    result
}
