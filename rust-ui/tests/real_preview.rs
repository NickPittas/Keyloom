//! Opt-in real-media RAM check. Run only through tools/isolated-test.sh.
use ck::{
    project::{load_project, LoadOptions, View},
    ui::viewer::Viewer,
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[test]
#[ignore = "requires CK_PREVIEW_PROJECT pointing to a project with at least 129 Input frames"]
fn real_129_frame_preview_reuses_decoded_ram_across_two_loops() {
    let root = PathBuf::from(std::env::var_os("CK_PREVIEW_PROJECT").expect("CK_PREVIEW_PROJECT"));
    let project = load_project(&root, &LoadOptions::default()).unwrap();
    let clip = project
        .clips
        .iter()
        .find(|c| c.input_files.len() >= 129)
        .expect("129-frame clip");
    let mut viewer = Viewer::new(0, clip, 4096, &eframe::egui::Context::default());
    viewer.view = View::Input;
    viewer.range = Some((0, 128));
    viewer.looping = true;
    let start = Instant::now();
    viewer.toggle_play(1);
    while viewer.clock.is_none() {
        viewer.tick();
        assert!(
            start.elapsed() < Duration::from_secs(120),
            "RAM preview did not finish buffering"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(
        viewer.preview_note.is_none(),
        "expected full RAM preview, not streaming"
    );
    assert_eq!(viewer.store.cache.len(), 129);
    let decoded = viewer.store.decode_ms.len();
    assert_eq!(decoded, 129, "cold fill decodes each frame exactly once");
    let fill = start.elapsed();
    let playback = Instant::now();
    let mut wraps = 0;
    let mut previous = viewer.playhead;
    while wraps < 2 {
        viewer.tick();
        if viewer.playhead < previous {
            wraps += 1;
        }
        previous = viewer.playhead;
        assert!(
            playback.elapsed() < Duration::from_secs(60),
            "did not loop twice"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        viewer.store.decode_ms.len(),
        decoded,
        "warm loops must not decode again"
    );
    println!(
        "129-frame RAM preview: fill={fill:?}, decodes={decoded}, wraps={wraps}, headless_drops={}",
        viewer.drops
    );
}
