//! Drives `Viewer::tick` headlessly (no window) on a synthetic PNG project.
use ck::project::*;
use ck::ui::viewer::Viewer;
use std::fs;
use std::time::{Duration, Instant};

/// `tag` makes the directory unique per test: tests run in parallel and must not share or delete each other's files.
fn project(tag: &str, n: usize) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("ck-viewer-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    fs::write(root.join("project.json"), r#"{"version":2,"clips":["c"]}"#).unwrap();
    // 24 fps declared so the clock rate is explicit, not assumed.
    fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..n {
        let f = fs::File::create(root.join(format!("clips/c/Frames/f_{i:03}.png"))).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), 4, 4);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&[i as u8; 48])
            .unwrap();
    }
    root
}

fn warm(v: &mut Viewer, n: usize) {
    for i in 0..n {
        v.set_playhead(i);
        let d = Instant::now() + Duration::from_secs(5);
        while !v.store.cache.contains(&ck::frames::Key {
            path: v.index_path(i),
            interp: ck::decode::Interp::Passthrough,
        }) {
            v.tick();
            assert!(Instant::now() < d, "frame {i} never cached");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn replacing_a_guide_at_the_same_path_uploads_new_pixels_without_changing_views() {
    let root = project("replace-guide", 1);
    let dir = root.join("clips/c/VideoMamaMaskHint");
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("f_000.png");
    let write = |value: u8| {
        let mut encoder = png::Encoder::new(fs::File::create(&path).unwrap(), 4, 4);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[value; 16]).unwrap();
    };
    write(0);
    let ctx = egui::Context::default();
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut viewer = Viewer::new(0, &p.clips[0], 16, &ctx);
    viewer.view = View::Guide;
    let upload = |viewer: &mut Viewer| {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            viewer.tick();
            let mut out = ctx.run_ui(egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))),
                ..Default::default()
            }, |ui| viewer.ui(ui));
            let pixel = out.textures_delta.set.values().flatten().find_map(|delta| {
                let egui::ImageData::Color(image) = &delta.image;
                (image.size == [4, 4]).then(|| image.pixels[0].r())
            });
            out.textures_delta.clear();
            if let Some(pixel) = pixel {
                return pixel;
            }
            assert!(Instant::now() < deadline, "the changed guide was not uploaded");
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    assert_eq!(upload(&mut viewer), 0);
    write(255);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    viewer.refresh(&p.clips[0]);
    assert_eq!(upload(&mut viewer), 255);
    assert_eq!(viewer.view, View::Guide);
    drop(viewer);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn playback_keeps_requesting_repaints_and_advances_with_every_frame_cached() {
    let n = 12;
    let root = project("play", n);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let ctx = eframe::egui::Context::default();
    let mut v = Viewer::new(0, &p.clips[0], 256, &ctx);
    v.view = View::Input;
    v.looping = false;
    warm(&mut v, n);
    assert_eq!(
        v.store.cache.len(),
        n,
        "every frame is cached: no decode can wake the UI"
    );
    v.set_playhead(0);
    assert!(!v.tick(), "paused viewer needs no repaint");
    v.toggle_play(1);
    let t0 = Instant::now();
    let mut seen_true = 0;
    while t0.elapsed() < Duration::from_millis(300) {
        if v.tick() {
            seen_true += 1;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    // 24 fps for ~0.3 s ⇒ about 7 frames; allow scheduler slack but require clear progress.
    assert!(v.playhead >= 4, "playhead only reached {}", v.playhead);
    assert!(
        seen_true > 10,
        "tick must return true (repaint wanted) throughout playback"
    );
    assert_eq!(v.drops, 0, "all frames cached ⇒ nothing may be dropped");
    // Runs to the end and stops by itself.
    let t1 = Instant::now();
    while v.tick() {
        assert!(
            t1.elapsed() < Duration::from_secs(3),
            "playback never stopped"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(v.playhead, n - 1);
    assert!(v.clock.is_none());
    drop(v); // joins the decode workers before the files go away
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn reverse_playback_and_range_bounds() {
    let n = 10;
    let root = project("reverse", n);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &eframe::egui::Context::default());
    v.view = View::Input;
    v.looping = false;
    warm(&mut v, n);
    v.set_playhead(5);
    v.set_in();
    v.set_playhead(7);
    v.set_out();
    assert_eq!(v.range, Some((5, 7)));
    assert!(v.range_dirty());
    v.set_playhead(7);
    v.toggle_play(-1);
    let t = Instant::now();
    while v.tick() {
        assert!(t.elapsed() < Duration::from_secs(3));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(v.playhead, 5, "reverse play stops at In");
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

// ---------------------------------------------------------------- RAM preview, loop, zoom/pan

use ck::frames::{real_decode, FrameStore};
use eframe::egui::{self, Event, Modifiers, PointerButton, Pos2, RawInput, Rect, Vec2};

/// `rgba` frames carry alpha (checker); `hint` adds an AlphaHint view; `exr` copies a real EXR fixture instead of PNG.
fn project_with(tag: &str, n: usize, rgba: bool, hint: bool, exr: bool) -> std::path::PathBuf {
    let root = project(tag, 0);
    let fixture =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rgb_none.exr");
    if hint {
        fs::create_dir_all(root.join("clips/c/AlphaHint")).unwrap();
    }
    for i in 0..n {
        for dir in ["Frames", "AlphaHint"] {
            if dir == "AlphaHint" && !hint {
                continue;
            }
            if exr && dir == "Frames" {
                fs::copy(&fixture, root.join(format!("clips/c/Frames/f_{i:03}.exr"))).unwrap();
                continue;
            }
            let f = fs::File::create(root.join(format!("clips/c/{dir}/f_{i:03}.png"))).unwrap();
            let mut e = png::Encoder::new(std::io::BufWriter::new(f), 4, 4);
            e.set_color(if rgba {
                png::ColorType::Rgba
            } else {
                png::ColorType::Rgb
            });
            e.set_depth(png::BitDepth::Eight);
            e.write_header()
                .unwrap()
                .write_image_data(&vec![i as u8 + 1; if rgba { 64 } else { 48 }])
                .unwrap();
        }
    }
    root
}

fn pump(v: &mut Viewer, mut until: impl FnMut(&mut Viewer) -> bool) {
    let d = Instant::now() + Duration::from_secs(5);
    while !until(v) {
        v.tick();
        assert!(Instant::now() < d, "condition never reached");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn play_buffers_the_whole_range_first_and_reports_progress() {
    let n = 12;
    let root = project_with("buffer", n, false, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    v.toggle_play(1);
    assert!(v.clock.is_none(), "cold cache: must not start playing yet");
    let (done, total) = v.buffering().expect("buffering");
    assert!(done < total && total == n);
    let mut seen = vec![done];
    pump(&mut v, |v| {
        if let Some((d, _)) = v.buffering() {
            seen.push(d);
        }
        v.clock.is_some()
    });
    assert_eq!(
        v.store.cache.len(),
        n,
        "every frame of the range is in RAM before the clock starts"
    );
    assert!(v.buffering().is_none() && v.preview_note.is_none());
    assert!(
        seen.windows(2).all(|w| w[0] <= w[1]),
        "progress never goes back: {seen:?}"
    );
    v.toggle_play(1);
    assert!(v.clock.is_none(), "second press pauses");
    // Pressing play again and cancelling mid-buffer leaves nothing running.
    let mut v2 = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v2.view = View::Input;
    v2.toggle_play(-1);
    v2.toggle_play(-1);
    assert!(v2.buffering().is_none() && v2.clock.is_none());
    drop((v, v2));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn range_over_the_ram_budget_is_refused_explicitly_and_streams() {
    let n = 12;
    let root = project_with("over", n, false, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    // 4x4 RGBA = 64 B per frame; room for 5 of the 12.
    v.store = FrameStore::new(5 * 64, 1, real_decode(), None);
    v.toggle_play(1);
    pump(&mut v, |v| v.clock.is_some());
    let note = v.preview_note.clone().expect("explicit over-budget note");
    assert!(
        note.contains("RAM preview needs")
            && note.contains("12 frames")
            && note.contains("--cache-mb"),
        "{note}"
    );
    assert!(
        v.store.cache.bytes() <= 5 * 64,
        "cache never exceeds its budget"
    );
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_b_views_and_input_interpretation_are_part_of_the_buffer() {
    let n = 4;
    let root = project_with("ab", n, false, true, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    v.ab = true;
    v.b_view = View::Hint;
    v.toggle_play(1);
    assert_eq!(
        v.buffering().unwrap().1,
        2 * n,
        "A and B frames are both needed"
    );
    pump(&mut v, |v| v.clock.is_some());
    assert_eq!(v.store.cache.len(), 2 * n);
    // Switching B while the preview runs buffers the new set instead of playing holes.
    v.b_view = View::Input;
    v.tick();
    assert!(v.clock.is_none() || v.buffering().is_none());
    drop(v);
    let _ = fs::remove_dir_all(&root);

    let root = project_with("interp", n, false, false, true);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    v.toggle_play(1);
    pump(&mut v, |v| v.clock.is_some());
    assert_eq!(v.store.cache.len(), n);
    v.linear = !v.linear; // EXR reading changes ⇒ different cache keys
    v.tick();
    assert!(v.clock.is_none(), "preview stopped to re-buffer");
    assert_eq!(v.buffering().map(|b| b.1), Some(n));
    pump(&mut v, |v| v.clock.is_some());
    assert_eq!(
        v.store.cache.len(),
        2 * n,
        "both interpretations are in RAM"
    );
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn looping_wraps_in_both_directions_inside_in_out() {
    let n = 10;
    let root = project_with("loop", n, false, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    warm(&mut v, n);
    v.set_playhead(3);
    v.set_in();
    v.set_playhead(6);
    v.set_out();
    for dir in [1isize, -1] {
        v.set_playhead(if dir > 0 { 5 } else { 4 });
        v.toggle_play(dir);
        let (mut wraps, mut last) = (0, v.playhead);
        let t = Instant::now();
        while t.elapsed() < Duration::from_millis(900) && wraps < 2 {
            assert!(v.tick(), "a looping preview never stops by itself");
            assert!((3..=6).contains(&v.playhead), "left In/Out: {}", v.playhead);
            if (dir > 0 && v.playhead < last) || (dir < 0 && v.playhead > last) {
                wraps += 1;
            }
            last = v.playhead;
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(wraps >= 1, "dir {dir}: never wrapped");
        v.toggle_play(dir);
    }
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn zoom_about_cursor_clamps_and_fit_restores() {
    let root = project_with("zoom", 1, false, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 16, &egui::Context::default());
    v.zoom_by(2.0, Vec2::new(10.0, 0.0));
    assert_eq!((v.zoom, v.pan), (2.0, Vec2::new(-10.0, 0.0))); // the point under the cursor stays put
    v.zoom_by(1000.0, Vec2::ZERO);
    assert_eq!(v.zoom, 32.0);
    v.zoom_by(0.0001, Vec2::ZERO);
    assert_eq!((v.zoom, v.pan), (1.0, Vec2::ZERO));
    v.zoom_by(4.0, Vec2::new(5.0, 5.0));
    v.fit();
    assert_eq!((v.zoom, v.pan), (1.0, Vec2::ZERO));
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

fn ui_frame(ctx: &egui::Context, v: &mut Viewer, events: Vec<Event>) -> usize {
    let raw = RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 700.0))),
        events,
        ..Default::default()
    };
    let mut out = ctx.run_ui(raw, |ui| v.ui(ui));
    out.textures_delta.clear();
    out.shapes.len()
}

fn button(at: Pos2, b: PointerButton, pressed: bool) -> Event {
    Event::PointerButton {
        pos: at,
        button: b,
        pressed,
        modifiers: Modifiers::NONE,
    }
}

#[test]
fn wheel_zooms_middle_pans_and_primary_keeps_moving_the_wipe() {
    let n = 2;
    let root = project_with("input", n, false, true, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let ctx = egui::Context::default();
    let mut v = Viewer::new(0, &p.clips[0], 64, &ctx);
    v.view = View::Input;
    warm(&mut v, n);
    v.set_playhead(0);
    let at = Pos2::new(500.0, 250.0);
    for _ in 0..3 {
        ui_frame(&ctx, &mut v, vec![Event::PointerMoved(at)]);
    }
    assert_eq!(v.zoom, 1.0);
    ui_frame(
        &ctx,
        &mut v,
        vec![Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: Vec2::new(0.0, 120.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Modifiers::NONE,
        }],
    );
    for _ in 0..8 {
        ui_frame(&ctx, &mut v, vec![]);
    }
    assert!(v.zoom > 1.2, "wheel zoom: {}", v.zoom);
    // Not in A/B: primary drag pans.
    let drag = |ctx: &egui::Context, v: &mut Viewer, b: PointerButton| {
        ui_frame(ctx, v, vec![Event::PointerMoved(at), button(at, b, true)]);
        for k in 1..=4 {
            ui_frame(
                ctx,
                v,
                vec![Event::PointerMoved(at + Vec2::new(30.0 * k as f32, 0.0))],
            );
        }
        ui_frame(ctx, v, vec![button(at + Vec2::new(120.0, 0.0), b, false)]);
        ui_frame(ctx, v, vec![]);
    };
    let before = v.pan;
    drag(&ctx, &mut v, PointerButton::Primary);
    assert!(
        v.pan.x > before.x + 50.0,
        "primary drag pans without A/B: {:?}",
        v.pan
    );
    // A/B on: primary moves the wipe only; middle pans.
    v.ab = true;
    v.b_view = View::Hint;
    v.fit();
    v.zoom_by(2.0, Vec2::ZERO);
    pump(&mut v, |v| v.store.cache.len() >= 2 * n); // B (hint) frames decoded
    for _ in 0..3 {
        ui_frame(&ctx, &mut v, vec![]);
    }
    let (pan0, wipe0) = (v.pan, v.wipe);
    drag(&ctx, &mut v, PointerButton::Primary);
    assert_eq!(v.pan, pan0, "primary drag must not pan during A/B");
    assert!(v.wipe > wipe0 + 0.05, "wipe moved: {} -> {}", wipe0, v.wipe);
    let wipe1 = v.wipe;
    drag(&ctx, &mut v, PointerButton::Middle);
    assert!(
        v.pan.x > pan0.x + 50.0,
        "middle drag pans during A/B: {:?}",
        v.pan
    );
    assert_eq!(v.wipe, wipe1, "middle drag must not move the wipe");
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn deep_zoom_paints_only_the_checker_cells_inside_the_viewport() {
    let root = project_with("checker", 1, true, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let ctx = egui::Context::default();
    let mut v = Viewer::new(0, &p.clips[0], 64, &ctx);
    v.view = View::Input;
    warm(&mut v, 1);
    v.set_playhead(0);
    let fit_shapes = ui_frame(&ctx, &mut v, vec![]);
    v.zoom_by(32.0, Vec2::ZERO);
    let t = Instant::now();
    let zoomed = ui_frame(&ctx, &mut v, vec![]);
    assert!(
        zoomed < fit_shapes.max(200) * 3 && zoomed < 6000,
        "{zoomed} shapes at 32x (fit: {fit_shapes})"
    );
    assert!(t.elapsed() < Duration::from_secs(2));
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn range_change_during_play_clamps_the_playhead_in_every_mode() {
    let n = 12;
    let root = project("clamp", n);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    for (looping, dir) in [(true, 1), (true, -1), (false, 1), (false, -1)] {
        let mut v = Viewer::new(0, &p.clips[0], 256, &eframe::egui::Context::default());
        v.view = View::Input;
        v.looping = looping;
        warm(&mut v, n);
        v.set_playhead(2);
        v.toggle_play(dir);
        v.range = Some((8, 10));
        v.tick();
        assert!(
            (8..=10).contains(&v.playhead),
            "loop={looping} dir={dir}: playhead {}",
            v.playhead
        );
        assert!(v.clock.is_some() || v.buffering().is_some());
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn drops_reset_when_a_new_run_starts_not_on_pause() {
    let root = project("drops", 4);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &eframe::egui::Context::default());
    v.view = View::Input;
    warm(&mut v, 4);
    v.drops = 5;
    v.toggle_play(1);
    assert_eq!(v.drops, 0);
    v.drops = 3;
    v.pause();
    assert_eq!(v.drops, 3);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn loop_off_after_laps_continues_from_the_current_frame() {
    let n = 10;
    let root = project("loopoff", n);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &eframe::egui::Context::default());
    v.view = View::Input;
    v.looping = true;
    v.range = Some((0, 9));
    let t0 = Instant::now();
    v.clock = Some(ck::frames::Clock::start_at(24.0, 0, 1, t0));
    v.playhead = 3; // three laps in: unrebased, the clamped clock would sit on Out
    let later = t0 + Duration::from_secs(5);
    v.toggle_loop_at(later);
    assert!(!v.looping);
    let c = v.clock.as_ref().unwrap();
    assert_eq!(c.frame_at(later), 3);
    assert_eq!(c.frame_at(later + Duration::from_millis(50)), 4);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn oversized_frames_with_unknown_size_fail_visibly_with_bounded_decodes() {
    let n = 4;
    let root = std::env::temp_dir().join(format!("ck-viewer-big-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    fs::write(root.join("project.json"), r#"{"version":2,"clips":["c"]}"#).unwrap();
    fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..n {
        let f = fs::File::create(root.join(format!("clips/c/Frames/f_{i:03}.png"))).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), 700, 700); // 1.96 MB RGBA > 1 MiB cache
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&vec![i as u8; 700 * 700 * 3])
            .unwrap();
    }
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 1, &eframe::egui::Context::default());
    v.view = View::Input;
    v.toggle_play(1);
    let d = Instant::now() + Duration::from_secs(10);
    while v.buffering().is_some() {
        v.tick();
        assert!(Instant::now() < d, "buffering never gave up");
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(v
        .preview_note
        .as_deref()
        .is_some_and(|m| m.contains("could not be loaded")));
    for _ in 0..20 {
        v.tick();
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(
        v.store.decode_ms.len() <= n,
        "decoded {} times",
        v.store.decode_ms.len()
    );
    assert!(v.store.cache.is_empty());
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

/// Starts mid-range with Loop off (only the remaining direction cached), then switches Loop on mid-play.
fn loop_on_midplay(
    tag: &str,
    dir: isize,
    start: usize,
    budget: Option<usize>,
) -> (Viewer, std::path::PathBuf) {
    let n = 10;
    let root = project_with(tag, n, false, false, false);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    if let Some(b) = budget {
        v.store = FrameStore::new(b, 1, real_decode(), None);
    }
    v.looping = false;
    v.set_playhead(start);
    v.toggle_play(dir);
    pump(&mut v, |v| v.clock.is_some());
    let remaining = if dir > 0 { n - start } else { start + 1 };
    assert_eq!(
        v.store.cache.len(),
        remaining,
        "only the remaining direction is warm"
    );
    v.toggle_loop();
    (v, root)
}

#[test]
fn loop_on_midplay_rebuffers_the_whole_range_then_wraps_warm() {
    for (dir, start) in [(1, 4), (-1, 5)] {
        let (mut v, root) = loop_on_midplay(&format!("loopon{dir}"), dir, start, None);
        v.tick();
        assert!(
            v.clock.is_none(),
            "cold pre-wrap frames: must pause and rebuffer"
        );
        let (_, total) = v.buffering().expect("rebuffering");
        let n = 10;
        assert_eq!(total, n);
        pump(&mut v, |v| v.buffering().is_none() && v.clock.is_some());
        assert_eq!(v.store.cache.len(), n);
        let decodes = v.store.decode_ms.len();
        // Observe two real wraps (playhead jumps against the play direction), bounded by a timeout.
        let (mut wraps, mut last) = (0, v.playhead);
        let d = Instant::now() + Duration::from_secs(10);
        while wraps < 2 {
            v.tick();
            if (v.playhead as isize - last as isize) * dir < 0 {
                wraps += 1;
            }
            last = v.playhead;
            assert!(Instant::now() < d, "only {wraps} wraps seen");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            v.store.decode_ms.len(),
            decodes,
            "warm wraps decode nothing"
        );
        drop(v);
        let _ = fs::remove_dir_all(&root);
    }
}

#[test]
fn loop_on_midplay_over_budget_streams_with_an_explicit_note() {
    // 7 frames of room: the 6 remaining fit, the whole 10-frame loop does not.
    let (mut v, root) = loop_on_midplay("loopon-budget", 1, 4, Some(7 * 64));
    v.tick();
    assert!(
        v.buffering().is_none() && v.clock.is_some(),
        "streams at once"
    );
    let note = v.preview_note.clone().unwrap();
    assert!(
        note.contains("RAM preview needs") && note.contains("10 frames"),
        "{note}"
    );
    assert!(v.store.cache.bytes() <= 7 * 64);
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

/// Loop off, `bad` frames undecodable: the run must reach the end, stop, and never re-decode the failures.
fn run_with_failed(tag: &str, dir: isize, bad: &[usize]) {
    let n = 6;
    let root = project_with(tag, n, false, false, false);
    for i in bad {
        fs::write(
            root.join(format!("clips/c/Frames/f_{i:03}.png")),
            b"not a png",
        )
        .unwrap();
    }
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let mut v = Viewer::new(0, &p.clips[0], 256, &egui::Context::default());
    v.view = View::Input;
    v.looping = false;
    let end = if dir > 0 { n - 1 } else { 0 };
    v.set_playhead(n - 1 - end);
    v.toggle_play(dir);
    pump(&mut v, |v| v.clock.is_some());
    pump(&mut v, |v| v.clock.is_none()); // never stops if a failed end frame is not "ready"
    assert_eq!(v.playhead, end);
    let decodes = v.store.decode_ms.len();
    assert!(decodes <= n, "decoded {decodes} times");
    for _ in 0..20 {
        v.tick();
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(v.clock.is_none() && v.store.decode_ms.len() == decodes);
    drop(v);
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_failed_final_frame_still_stops_loop_off_playback() {
    run_with_failed("failed-out", 1, &[5]);
    run_with_failed("failed-in", -1, &[0]);
    run_with_failed("failed-all", 1, &[0, 1, 2, 3, 4, 5]);
    run_with_failed("failed-all-rev", -1, &[0, 1, 2, 3, 4, 5]);
}
