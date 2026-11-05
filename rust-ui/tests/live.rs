//! LIVE suite: the real App (headless egui, synthetic clicks), the REAL worker, GPU and models (read-only) on a temporary copy of a real clip.
//! Every test is `#[ignore]` so `cargo test` never runs them. Run them ONLY through `bash rust-ui/tools/live-test.sh <test>` (one test per command, 60 s)
//! (see `rust-ui/tests/LIVE.md`): it sets KEYLOOM_LIVE=1 and the runtime/models env and runs one test at a time inside gpu-isolated-test.sh.
mod live_support;
use ck::hints::{Final, Producer};
use ck::ui::Tab;
use live_support::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};


fn dir_of(live: &Live, clip: &str, sub: &str) -> PathBuf {
    live.clip(clip).join(sub)
}
const GUIDE: &str = "VideoMamaMaskHint";
const HINT: &str = "AlphaHint";

fn hash_file(p: &Path) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::fs::read(p).unwrap_or_default().hash(&mut h);
    h.finish()
}
fn first_file(dir: &Path) -> PathBuf {
    let mut v: Vec<_> = std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.is_file() && !p.file_name().unwrap().to_string_lossy().starts_with('.')).collect();
    v.sort();
    v.remove(0)
}
fn expect_files(what: &str, dir: &Path, n: usize) {
    let c = count_files(dir);
    ev(format!("{what}: {c} files in {}", dir.file_name().unwrap().to_string_lossy()));
    assert_eq!(c, n, "{what}: {} holds {c} files, {n} expected", dir.display());
}
fn open_hint(live: &Live) -> Drv {
    Drv::open_clip(live, "SHOT_03", Tab::Hint)
}

// ====================================================================== worker

/// A6: one idle worker per GPU exists as soon as the app opens (no project, then project/clip open change nothing) and holds no model lock.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_worker_idle_at_open() {
    let live = Live::new("w-idle", frames_or(4), &["SHOT_03"]);
    assert!(pids().is_empty(), "a worker already runs before the test: {:?}", workers());
    let mut d = Drv::new(&live, false, None, None);
    assert!(d.pump(120, "idle worker Ready", |d| d.idle_ready()), "no idle worker became ready: {}", d.targets());
    let gpus = std::fs::read_dir("/dev").unwrap().flatten().filter(|e| e.file_name().to_string_lossy().strip_prefix("nvidia").is_some_and(|r| r.parse::<u32>().is_ok())).count();
    ev(format!("GPU device nodes {gpus}; workers {:?}; lock dir {:?}", workers(), locks()));
    assert_eq!(pids().len(), gpus.max(1), "one idle worker per GPU: {:?}", workers());
    assert!(locks().is_empty(), "the idle worker holds a model lock: {:?}", locks());
    let p0 = pids();
    let ctx = d.ctx.clone();
    d.app.open_project_for_test(&ctx, &live.proj);
    d.frames(30);
    assert_eq!(pids(), p0, "opening a project changed the worker set");
    ev(format!("project opened: workers {:?}, targets {}, locks {:?}", pids(), d.targets(), locks()));
    assert!(locks().is_empty());
}

/// The first real action (Chroma pre-matte, Hint page) completes on the idle worker, with the right frame count.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_worker_first_action_completes() {
    let live = Live::new("w-first", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    let p0 = pids();
    assert_eq!(p0.len(), 1, "one idle worker expected at open: {:?}", workers());
    cfg(&mut d, Producer::Chroma, Final::Direct);
    let r = run_job(&mut d, &live, "first action: Chroma pre-matte", "Generate pre-matte", 200);
    r.evidence();
    r.assert_complete();
    assert_eq!(r.pids_after, p0, "the first action ran on a different worker");
    expect_files("pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
}

/// Two clips queued from the Board run in order on the SAME worker process.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_worker_two_clips_same_pid() {
    let live = Live::new("w-two", 3, &["SHOT_03", "SHOT_04"]);
    write_preset(&live, "key-only", Producer::Chroma, Final::Direct, "none");
    let mut d = Drv::new(&live, true, None, None);
    d.warm();
    assert!(d.pump(120, "idle worker", |d| d.idle_ready()), "{}", d.targets());
    let p0 = pids();
    board_key_selected(&mut d, &["SHOT_03", "SHOT_04"], "key-only");
    if d.app.ai().batch_ready() {
        ev(format!("review: {:?}", d.app.ai().batch_summary()));
        d.app.ai_mut().confirm_batch();
    }
    d.pump(20, "two Key jobs queued", |d| d.app.ai().scheduler().snapshot().len() == 2);
    let queued: Vec<String> = d.app.ai().scheduler().snapshot().iter().map(|j| j.job.job_id.clone()).collect();
    assert_eq!(queued.len(), 2, "two Key jobs expected, got {}; notice {:?}", queued.len(), d.app.ai().notice);
    let mut done_at: Vec<Option<Instant>> = vec![None; 2];
    let t0 = Instant::now();
    let ok = d.pump(560, "both Key jobs end", |d| {
        for (i, id) in queued.iter().enumerate() {
            if done_at[i].is_none() && d.jobs().iter().any(|j| &j.0 == id && j.3) {
                done_at[i] = Some(Instant::now());
            }
        }
        done_at.iter().all(Option::is_some)
    });
    let states: Vec<_> = d.jobs().into_iter().filter(|j| queued.contains(&j.0)).map(|j| j.2).collect();
    ev(format!("Key jobs {states:?} in {:.1}s; worker pids {p0:?} -> {:?}", t0.elapsed().as_secs_f32(), pids()));
    assert!(ok && states.iter().all(|s| s.contains("Done Complete")), "Key jobs did not both complete: {states:?}");
    assert!(done_at[0] <= done_at[1], "the clips did not finish in queue order");
    assert_eq!(pids(), p0, "the clips did not run on one worker process");
}

/// Cancelling a Key part-way leaves the worker idle and alive (same pid, no lock left).
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_worker_cancel_key_leaves_worker_idle() {
    let live = Live::new("w-cancel", frames_or(6), &["SHOT_03"]);
    let mut d = Drv::open_clip(&live, "SHOT_03", Tab::Key);
    let p0 = pids();
    key_click(&mut d, live.frames);
    assert!(d.pump(240, "Key running", |d| d.jobs().iter().any(|j| j.2.starts_with("Running"))), "the Key never started running: {:?}", d.jobs());
    d.pump(4, "a moment of running", |_| false);
    ev(format!("cancelling at {:?}", d.jobs().iter().map(|j| j.2.clone()).collect::<Vec<_>>()));
    cancel_all(&mut d);
    let states: Vec<_> = d.jobs().into_iter().map(|j| j.2).collect();
    ev(format!("after cancel: jobs {states:?}; workers {:?}; targets {}; locks {:?}", workers(), d.targets(), locks()));
    assert!(states.iter().any(|s| s.contains("Cancelled")), "no job ended Cancelled: {states:?}");
    assert!(d.pump(90, "worker idle again", |d| d.idle_ready()), "the worker is not idle after the cancel: {}", d.targets());
    assert_eq!(pids(), p0, "the worker was replaced by the cancel");
    assert!(p0.iter().all(|p| alive(*p)));
    assert!(locks().is_empty(), "a model lock stayed behind: {:?}", locks());
}

/// Killing the worker (kill -9): the app reports it, and the next job starts a new one that completes.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_worker_killed_is_reported_and_restarted() {
    let live = Live::new("w-kill", frames_or(4), &["SHOT_03"]);
    let mut d = open_hint(&live);
    let p0 = pids();
    assert_eq!(p0.len(), 1, "{:?}", workers());
    kill9(p0[0]);
    let reported = d.pump(60, "death reported", |d| !d.ready());
    ev(format!("after kill -9 {}: reported={reported}; targets {}; notice {:?}", p0[0], d.targets(), d.app.ai().notice));
    assert!(reported, "the app never noticed the dead worker: {}", d.targets());
    cfg(&mut d, Producer::Chroma, Final::Direct);
    let r = run_job(&mut d, &live, "job after kill", "Generate pre-matte", 240);
    r.evidence();
    r.assert_complete();
    ev(format!("new worker pids {:?} (old {p0:?})", pids()));
    assert!(!pids().is_empty() && !pids().contains(&p0[0]), "no new worker started: {:?}", workers());
    expect_files("pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
}

// ====================================================================== producers

fn check_model_files(r: &JobRun) {
    assert!(!r.model_files.is_empty(), "[{}] the app sent the worker no model_files", r.name);
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_sam_preview_and_track() {
    let live = Live::new("p-sam", frames_or(6), &["SHOT_03"]);
    live.sam_prompt("SHOT_03");
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Sam, Final::Direct);
    let a = run_job(&mut d, &live, "SAM preview", "Preview frame", 240);
    a.evidence();
    a.assert_complete();
    check_model_files(&a);
    let b = run_job(&mut d, &live, "SAM track", "Track prompts", 400);
    b.evidence();
    b.assert_complete();
    expect_files("SAM track pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
    ev(format!("mask stats: {}", live.stats(&dir_of(&live, "SHOT_03", GUIDE))));
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_gvm() {
    let live = Live::new("p-gvm", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Gvm, Final::Direct);
    let r = run_job(&mut d, &live, "GVM", "Generate pre-matte", 560);
    r.evidence();
    r.assert_complete();
    check_model_files(&r);
    expect_files("GVM pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
    ev(format!("mask stats: {}", live.stats(&dir_of(&live, "SHOT_03", GUIDE))));
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_birefnet() {
    let live = Live::new("p-bire", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Birefnet, Final::Direct);
    let variant = std::env::var("KEYLOOM_LIVE_BIREFNET").unwrap_or_else(|_| d.app.hint_cfg_for_test().unwrap().birefnet_variant);
    let mut c = d.app.hint_cfg_for_test().unwrap();
    c.birefnet_variant = variant.clone();
    d.app.hint_apply_cfg_for_test(c);
    d.frames(4);
    ev(format!("BiRefNet variant: {variant}"));
    let r = run_job(&mut d, &live, "BiRefNet", "Generate pre-matte", 400);
    r.evidence();
    r.assert_complete();
    check_model_files(&r);
    expect_files("BiRefNet pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
    ev(format!("mask stats: {}", live.stats(&dir_of(&live, "SHOT_03", GUIDE))));
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_chroma() {
    let live = Live::new("p-chroma", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Direct);
    let r = run_job(&mut d, &live, "Chroma", "Generate pre-matte", 240);
    r.evidence();
    r.assert_complete();
    expect_files("Chroma pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
    ev(format!("mask stats: {}", live.stats(&dir_of(&live, "SHOT_03", GUIDE))));
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_matanyone2() {
    let live = Live::new("p-mat", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Matanyone2);
    // MatAnyone2 needs the guide on disk first ("generate the guide first"): the pre-matte is a separate click on the Hint page.
    run_job(&mut d, &live, "Chroma pre-matte for MatAnyone2", "Generate pre-matte", 120).assert_complete();
    d.frames(6);
    let r = run_job(&mut d, &live, "MatAnyone2 (on a Chroma pre-matte)", "Generate hint", 560);
    r.evidence();
    r.assert_complete();
    check_model_files(&r);
    expect_files("MatAnyone2 hint", &dir_of(&live, "SHOT_03", HINT), live.frames);
    ev(format!("hint stats: {}", live.stats(&dir_of(&live, "SHOT_03", HINT))));
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_producers_videomama() {
    let live = Live::new("p-vmama", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Videomama);
    // VideoMaMa needs the guide on disk first ("generate the guide first"): the pre-matte is a separate click on the Hint page.
    run_job(&mut d, &live, "Chroma pre-matte for VideoMaMa", "Generate pre-matte", 120).assert_complete();
    d.frames(6);
    let r = run_job(&mut d, &live, "VideoMaMa (on a Chroma pre-matte)", "Generate hint", 560);
    r.evidence();
    r.assert_complete();
    check_model_files(&r);
    expect_files("VideoMaMa hint", &dir_of(&live, "SHOT_03", HINT), live.frames);
    ev(format!("hint stats: {}", live.stats(&dir_of(&live, "SHOT_03", HINT))));
}

// ====================================================================== hint

/// Hint page, Chroma + Direct, "Generate hint" with no pre-matte makes both; the Board then queues ZERO hint steps (reuse note).
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_hint_direct_without_prematte_then_board_queues_no_hint_steps() {
    let live = Live::new("h-direct", frames_or(6), &["SHOT_03"]);
    write_preset(&live, "chroma-direct", Producer::Chroma, Final::Direct, "full");
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Direct);
    assert_eq!(count_files(&dir_of(&live, "SHOT_03", GUIDE)), 0, "the fresh clip already has a pre-matte");
    let r = run_job(&mut d, &live, "Generate hint (no pre-matte yet)", "Generate hint", 300);
    r.evidence();
    r.assert_complete();
    expect_files("pre-matte", &dir_of(&live, "SHOT_03", GUIDE), live.frames);
    expect_files("hint", &dir_of(&live, "SHOT_03", HINT), live.frames);
    let tasks_before = d.app.ai().scheduler().task_snapshot().len();
    d.page_tab("Board");
    d.frames(8);
    board_key_selected(&mut d, &["SHOT_03"], "chroma-direct");
    let review = d.app.ai().batch_summary().join(" | ");
    ev(format!("Board review: {review}; notice {:?}", d.app.ai().notice));
    // The reuse note label is layout (the new Board review does not draw it); the behaviour proof is the zero hint steps asserted below.
    ev(format!("reuse note drawn: {}", d.has_containing("reusing the existing hint")));
    if d.app.ai().batch_ready() {
        d.app.ai_mut().confirm_batch();
    }
    d.frames(6);
    let tasks_after = d.app.ai().scheduler().task_snapshot().len();
    ev(format!("hint steps queued by the Board: {}; Key jobs {}", tasks_after - tasks_before, d.app.ai().scheduler().snapshot().len()));
    cancel_all(&mut d); // the Key itself is not under test
    assert_eq!(tasks_after, tasks_before, "the Board queued hint steps although a matching hint exists");
}

/// After a changed Chroma setting, "Generate hint" reruns the producer (new pre-matte pixels and a new Chroma task).
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_hint_changed_setting_reruns_producer() {
    let live = Live::new("h-rerun", frames_or(6), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Direct);
    let a = run_job(&mut d, &live, "Generate hint (green)", "Generate hint", 300);
    a.assert_complete();
    let f0 = first_file(&dir_of(&live, "SHOT_03", GUIDE));
    let h0 = hash_file(&f0);
    let ctx = d.ctx.clone();
    d.app.act_for_test(&ctx, ck::ui::shot_page::Act::Props(ck::ui::shot_page::PropAct::Hint(ck::ui::shot_page::HintAct::ChromaScreen("blue".into()))));
    d.frames(6);
    let b = run_job(&mut d, &live, "Generate hint (blue)", "Generate hint", 300);
    b.evidence();
    b.assert_complete();
    let chroma_ran = b.states.iter().any(|s| s.starts_with("chroma"));
    let h1 = hash_file(&first_file(&dir_of(&live, "SHOT_03", GUIDE)));
    ev(format!("Chroma task reran: {chroma_ran}; pre-matte frame 0 digest {h0:x} -> {h1:x}"));
    assert!(chroma_ran, "the producer did not rerun after the setting changed: {:?}", b.states);
    assert_ne!(h0, h1, "the pre-matte pixels did not change");
}

/// Chroma "Pick colour" stays the selected tool with the SAM overlay off, across frames and frame stepping.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_hint_chroma_pick_colour_with_sam_overlay_off() {
    use ck::ui::annotate::Tool;
    let live = Live::new("h-pick", frames_or(4), &["SHOT_03"]);
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Sam, Final::Direct);
    let was = d.app.viewer().map(|v| v.sam_overlay);
    d.app.viewer_mut_for_test(|v| v.sam_overlay = false);
    cfg(&mut d, Producer::Chroma, Final::Direct);
    ev(format!("SAM overlay {was:?} -> {:?}", d.app.viewer().map(|v| v.sam_overlay)));
    // The Properties "Tool" dropdown's act (the old icon strip is gone).
    let ctx = d.ctx.clone();
    d.app.act_for_test(&ctx, ck::ui::shot_page::Act::Props(ck::ui::shot_page::PropAct::Hint(ck::ui::shot_page::HintAct::Tool("pick".into()))));
    d.frames(10);
    let tool = |d: &Drv| d.app.viewer().map(|v| v.ann.tool);
    assert_eq!(tool(&d), Some(Tool::Pick), "Pick colour was not selected by its button");
    for _ in 0..3 {
        d.press(eframe::egui::Key::ArrowRight);
    }
    ev(format!("after 3 frame steps: playhead {:?}, tool {:?}", d.app.viewer().map(|v| v.playhead), tool(&d)));
    assert_eq!(tool(&d), Some(Tool::Pick), "the tool changed while stepping frames");
    d.app.pick_colour_for_test([89, 56, 36]).unwrap();
    d.frames(6);
    let col = d.app.hint_cfg_for_test().unwrap().chroma.screen_color;
    ev(format!("picked colour {col:?}; tool {:?}", tool(&d)));
    assert_eq!(col, Some([89, 56, 36]));
    assert_eq!(tool(&d), Some(Tool::Pick), "the tool changed after picking");
}

// ====================================================================== board

/// Board "Key selected": a matching hint is reused (zero hint steps); changing one pre-matte frame makes it ask reuse / re-render.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_board_key_selected_reuses_matching_hint_and_asks_on_change() {
    let live = Live::new("b-reuse", frames_or(6), &["SHOT_03"]);
    write_preset(&live, "chroma-direct", Producer::Chroma, Final::Direct, "full");
    let mut d = open_hint(&live);
    cfg(&mut d, Producer::Chroma, Final::Direct);
    run_job(&mut d, &live, "Generate hint", "Generate hint", 300).assert_complete();
    d.page_tab("Board");
    d.frames(8);
    let steps = d.app.ai().scheduler().task_snapshot().len();
    board_key_selected(&mut d, &["SHOT_03"], "chroma-direct");
    let reuse = d.app.ai().batch_summary().join(" | ");
    ev(format!("matching hint -> review: {reuse}; hint steps queued {}", d.app.ai().scheduler().task_snapshot().len() - steps));
    // Same facts the review's reuse note comes from: a compatible hint is reused without a question.
    let row = d.app.ai_mut().batch_dialog().and_then(|b| b.rows.into_iter().next()).expect("a review row");
    assert!(row.reuse && row.offer.is_none() && !row.mismatch, "the matching hint is not reused silently: reuse={} offer={:?} mismatch={}", row.reuse, row.offer, row.mismatch);
    assert_eq!(d.app.ai().scheduler().task_snapshot().len(), steps, "hint steps were queued");
    d.app.ai_mut().cancel_batch();
    cancel_all(&mut d);
    d.frames(6);
    // One pre-matte frame changes on disk: it must ask.
    let f = first_file(&dir_of(&live, "SHOT_03", GUIDE));
    let (w, h) = (3840u32, 2160u32);
    let mut e = png::Encoder::new(std::fs::File::create(&f).unwrap(), w, h);
    e.set_color(png::ColorType::Grayscale);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&vec![255u8; (w * h) as usize]).unwrap();
    board_key_selected(&mut d, &["SHOT_03"], "chroma-direct");
    let ask = d.screen().into_iter().filter(|s| s.contains("differs") || s.contains("Reuse anyway") || s.contains("Re-render")).collect::<Vec<_>>();
    ev(format!("one pre-matte frame changed -> screen: {ask:?}; review {:?}", d.app.ai().batch_summary()));
    let asked = d.app.ai().batch_summary().join(" ").contains("differs") || ask.iter().any(|s| s.contains("Reuse anyway") || s.contains("differs"))
        || d.app.ai_mut().batch_dialog().is_some_and(|b| b.rows.iter().any(|r| r.offer.is_some() || r.mismatch));
    assert!(asked, "no reuse/re-render question after a pre-matte frame changed: {ask:?}");
    assert_eq!(d.app.ai().scheduler().task_snapshot().len(), steps, "a hint step was queued without asking");
    d.app.ai_mut().cancel_batch();
    cancel_all(&mut d);
}

/// After Key the OPEN clip shows FG / Matte / Comp / Processed without reopening, and nothing says "Update required".
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_board_after_key_open_clip_shows_outputs_without_update_required() {
    use ck::project::View;
    let live = Live::new("b-key", 6, &["SHOT_03"]);
    let mut d = Drv::open_clip(&live, "SHOT_03", Tab::Key);
    let counts = |d: &Drv| -> Vec<usize> { let ix = &d.app.viewer().unwrap().index; [View::Fg, View::Matte, View::Comp, View::Processed].map(|v| ix.view_frames(v)).to_vec() };
    ev(format!("before Key: outputs FG/Matte/Comp/Processed {:?}; Key pending {}", counts(&d), d.app.stage_pending_for_test("key")));
    let r = run_with(&mut d, &live, "Key 6 frames", 540, |d| key_click(d, 6));
    r.evidence();
    r.assert_complete();
    d.frames(30);
    let after = counts(&d);
    let upd = d.has_containing("Update required");
    ev(format!("after Key (clip not reopened): FG/Matte/Comp/Processed {after:?}; Key pending {}; 'Update required' drawn: {upd}; header {:?}", d.app.stage_pending_for_test("key"), d.find_containing("Complete")));
    assert_eq!(after, vec![6, 6, 6, 6], "the open clip did not pick up the Key outputs");
    assert!(!upd, "'Update required' is still drawn after Key");
    assert!(!d.app.stage_pending_for_test("key"), "the Key stage is still pending");
}

// ====================================================================== export

/// Key 6 frames, export Processed as MOV ProRes 4444: alpha pix_fmt, 6 frames, frame-0 alpha within 1/255 of the EXR.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_export_prores4444_has_alpha_and_six_frames() {
    use ck::presets::{Delivery, VideoDelivery};
    let live = Live::new("x-prores", 6, &["SHOT_03"]);
    let mut d = Drv::open_clip(&live, "SHOT_03", Tab::Key);
    let k = run_with(&mut d, &live, "Key 6 frames", 540, |d| key_click(d, 6));
    k.evidence();
    k.assert_complete();
    d.app.switch_tab(Tab::Deliver);
    d.frames(8);
    d.app.set_export_for_test(&[("processed", Delivery::Video(VideoDelivery { container: "mov_prores4444".into(), codec: "prores4444".into(), quality: 9 }))], None, false, None);
    assert!(d.pump(30, "deliver controls", |d| d.has("Export existing Key result")), "Deliver page has no Export existing Key result button: {:?}", d.screen());
    let e = run_with(&mut d, &live, "Export ProRes 4444", 300, |d| d.click("Export existing Key result"));
    e.evidence();
    e.assert_complete();
    let movs = find_ext(&live.proj, "mov");
    ev(format!("exported: {movs:?}"));
    assert_eq!(movs.len(), 1, "expected one exported .mov under the project: {movs:?}");
    let f = &movs[0];
    let p = Command::new("/usr/bin/ffprobe").args(["-v", "error", "-count_frames", "-select_streams", "v:0", "-show_entries", "stream=codec_name,pix_fmt,width,height,nb_read_frames", "-of", "default=nw=1"]).arg(f).output().unwrap();
    let probe = String::from_utf8_lossy(&p.stdout).replace('\n', " ");
    ev(format!("ffprobe: {probe}; size {} bytes", std::fs::metadata(f).unwrap().len()));
    let pix = probe.split("pix_fmt=").nth(1).and_then(|s| s.split_whitespace().next()).unwrap_or("").to_string();
    assert!(pix.starts_with("yuva") || pix.contains("rgba") || pix.contains("argb"), "no alpha pix_fmt: {pix}");
    assert!(probe.contains("codec_name=prores"), "{probe}");
    assert!(probe.contains("nb_read_frames=6"), "frame count is not 6: {probe}");
    // Frame-0 alpha: MOV (alphaextract) against the EXR the Key wrote.
    let exr = find_ext(&live.clip("SHOT_03").join("Output"), "exr").into_iter().find(|p| p.to_string_lossy().contains("Processed")).expect("a Processed EXR");
    const SNIP: &str = "import sys,subprocess,numpy as np,cv2\nmov,exr,w,h=sys.argv[1],sys.argv[2],int(sys.argv[3]),int(sys.argv[4])\nraw=subprocess.run(['/usr/bin/ffmpeg','-v','error','-i',mov,'-vf','alphaextract','-frames:v','1','-pix_fmt','gray16le','-f','rawvideo','-'],capture_output=True).stdout\na=np.frombuffer(raw,dtype='<u2')[:w*h].reshape(h,w).astype(np.float64)/65535.0\nim=cv2.imread(exr,cv2.IMREAD_UNCHANGED)\nb=im[...,3].astype(np.float64)\nd=np.abs(a-b)\nprint('MAXDIFF %.6f MEAN_MOV %.5f MEAN_EXR %.5f NAN %d'%(d.max(),a.mean(),b.mean(),int(np.isnan(a).sum()+np.isnan(b).sum())))";
    let out = live.python(SNIP, &[f.to_str().unwrap(), exr.to_str().unwrap(), "3840", "2160"]);
    ev(format!("frame-0 alpha MOV vs {}: {}", exr.file_name().unwrap().to_string_lossy(), out.trim()));
    let md: f64 = out.split("MAXDIFF ").nth(1).and_then(|s| s.split_whitespace().next()).and_then(|s| s.parse().ok()).unwrap_or_else(|| panic!("alpha comparison failed: {out}"));
    assert!(md <= 1.0 / 255.0, "frame-0 alpha differs by {md} (> 1/255)");
}

// ====================================================================== endpoint

mod ep {
    use super::*;
    use ck::app_api::{self, AppApi, Paths, Status};
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Arc;

    pub fn paths(live: &Live) -> Paths {
        // Live::new's descriptor points at a logging wrapper script in the temp folder; the endpoint needs the real runtime's python.
        let inst = live.base.join("install-ep");
        std::fs::create_dir_all(&inst).unwrap();
        std::fs::write(inst.join("keyloom-install.json"), json!({"schema":1,"app_name":"Keyloom","app_version":"2.5.0","roots":{"python":live.py,"helper":live.helper,"models":null,"cache":null,"scratch":live.base.join("scratch"),"user_data":null}}).to_string()).unwrap();
        std::env::set_var("CK_INSTALL", &inst);
        ck::runtime::embed_paths(&ck::runtime::launch(Some(&live.helper))).expect("the installed runtime resolves")
    }
    pub fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
    }
    /// A state dir the way the old service left it: loopback, Join allowed, a random free port (never 8443).
    pub fn state(live: &Live) -> (u16, PathBuf) {
        let port = free_port();
        assert_ne!(port, 8443);
        let sd = live.base.join("ep/server");
        std::fs::create_dir_all(&sd).unwrap();
        std::fs::write(sd.join("service.json"), json!({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": true}).to_string()).unwrap();
        (port, sd)
    }
    pub fn wait(api: &AppApi, ok: impl Fn(&Status) -> bool) -> Status {
        let end = Instant::now() + Duration::from_secs(30);
        while Instant::now() < end {
            let s = api.status();
            if ok(&s) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("endpoint status never settled: {:?}", api.status());
    }
    pub fn start(live: &Live, sd: &Path) -> (AppApi, Arc<AtomicU64>) {
        let woke = Arc::new(AtomicU64::new(0));
        let w = woke.clone();
        let api = AppApi::start(paths(live), sd.to_path_buf(), Arc::new(move || {
            w.fetch_add(1, Ordering::Relaxed);
        }));
        (api, woke)
    }
    pub fn reaches(port: u16) -> bool {
        std::net::TcpStream::connect_timeout(&format!("127.0.0.1:{port}").parse().unwrap(), Duration::from_secs(1)).is_ok()
    }
    /// The listening socket's inode is in THIS process's fd table.
    pub fn listener_is_ours(port: u16) -> bool {
        let want = format!(":{port:04X} ");
        let inodes: Vec<String> = ["/proc/net/tcp", "/proc/net/tcp6"]
            .iter()
            .filter_map(|f| std::fs::read_to_string(f).ok())
            .flat_map(|t| t.lines().skip(1).map(str::to_owned).collect::<Vec<_>>())
            .filter(|l| l.contains(&want) && l.split_whitespace().nth(3) == Some("0A"))
            .filter_map(|l| l.split_whitespace().nth(9).map(str::to_owned))
            .collect();
        std::fs::read_dir("/proc/self/fd").unwrap().flatten().filter_map(|e| std::fs::read_link(e.path()).ok()).any(|l| inodes.iter().any(|i| l.to_string_lossy() == format!("socket:[{i}]")))
    }
    pub fn children() -> Vec<String> {
        let mut out = vec![];
        for t in std::fs::read_dir("/proc/self/task").unwrap().flatten() {
            if let Ok(c) = std::fs::read_to_string(t.path().join("children")) {
                out.extend(c.split_whitespace().map(str::to_owned));
            }
        }
        out
    }
    pub const SENDER: &str = "import sys, json\nsys.path.insert(0, sys.argv[1])\nfrom backend.coordinator import pki\nfrom backend.coordinator.client import Client, join_to_file\nport, state, creds = sys.argv[2], sys.argv[3], sys.argv[4]\njoin_to_file(f'https://127.0.0.1:{port}', creds, 'sender', pem=open(state + '/server-cert.pem').read())\nc = Client(pki.read_credentials(creds))\nfrom tests.coordinator.specs import SPEC as spec\na = c.submit('a' * 32, spec)\nb = c.submit('a' * 32, spec)\nprint(json.dumps({'job': a['job_id'], 'again': b['job_id'], 'state': c.job(a['job_id'])['state']}))";
    pub fn running(api: &AppApi) -> Value {
        match wait(api, |s| !matches!(s, Status::Starting)) {
            Status::Running(i) => i,
            s => panic!("the endpoint did not start: {s:?}"),
        }
    }
    pub fn push(live: &Live, port: u16, sd: &Path) -> Value {
        let o = Command::new(&live.py).args(["-I", "-c", SENDER]).arg(&live.helper).arg(port.to_string()).arg(sd).arg(live.base.join("creds.json")).output().expect("the sender runs");
        assert!(o.status.success(), "sender failed: {}", String::from_utf8_lossy(&o.stderr));
        serde_json::from_slice(&o.stdout).unwrap()
    }
    pub fn inbox_pending() -> Vec<String> {
        app_api::call("inbox", json!({"method": "pending"})).expect("inbox").as_array().unwrap().iter().map(|j| j["job_id"].as_str().unwrap().to_string()).collect()
    }
}

/// The listener belongs to the app process itself: the socket is in its fd table, no netlink/service child exists.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_endpoint_listener_in_app_pid_no_child() {
    let live = Live::new("e-pid", 0, &[]);
    let (port, sd) = ep::state(&live);
    let (api, woke) = ep::start(&live, &sd);
    let info = ep::running(&api);
    ev(format!("endpoint running: port {} pid {} (this process {}); woke {}x", info["port"], info["pid"], std::process::id(), woke.load(std::sync::atomic::Ordering::Relaxed)));
    assert_eq!(info["port"], port);
    assert_eq!(info["pid"], std::process::id(), "the listener is not in the app PID");
    assert!(ep::listener_is_ours(port), "the listening socket is not in this process's fd table");
    assert!(ep::reaches(port));
    let kids = ep::children();
    ev(format!("child processes of the app: {kids:?}; netlink/service processes: {:?}", workers()));
    assert!(kids.is_empty(), "the app has child processes (netlink/service?): {kids:?}");
    assert!(api.stop(Duration::from_secs(20)));
}

/// An authenticated push (pinned certificate, joined sender) lands in the inbox; repeating it adds nothing; the driver is woken.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_endpoint_authenticated_push_lands_in_inbox() {
    let live = Live::new("e-push", 0, &[]);
    let (port, sd) = ep::state(&live);
    let (api, woke) = ep::start(&live, &sd);
    ep::running(&api);
    let before = woke.load(std::sync::atomic::Ordering::Relaxed);
    let got = ep::push(&live, port, &sd);
    let job = got["job"].as_str().unwrap().to_string();
    ev(format!("push: {got}"));
    assert_eq!(got["again"], got["job"], "a repeated submission is not the same job");
    assert_eq!(got["state"], "queued");
    let end = Instant::now() + Duration::from_secs(5);
    while woke.load(std::sync::atomic::Ordering::Relaxed) == before && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pending = ep::inbox_pending();
    ev(format!("inbox pending {pending:?}; driver woken {}x; events {:?}", woke.load(std::sync::atomic::Ordering::Relaxed) - before, api.take_events()));
    assert_eq!(pending, [job], "the pushed job is not (only) in the inbox");
    assert!(woke.load(std::sync::atomic::Ordering::Relaxed) > before, "the driver was not woken");
    assert!(api.stop(Duration::from_secs(20)));
}

/// Quit (stop) closes the socket and the same state reopens with the job still queued.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_endpoint_quit_closes_the_socket() {
    let live = Live::new("e-quit", 0, &[]);
    let (port, sd) = ep::state(&live);
    let (api, _) = ep::start(&live, &sd);
    ep::running(&api);
    assert!(ep::reaches(port));
    assert!(api.stop(Duration::from_secs(20)), "stop did not finish in time");
    ev(format!("after stop: reachable {}, ours {}, status {:?}", ep::reaches(port), ep::listener_is_ours(port), api.status()));
    assert!(!ep::reaches(port), "the port still answers after Quit");
    assert!(!ep::listener_is_ours(port));
    assert_eq!(api.status(), ck::app_api::Status::Stopped);
}

/// The old service holding the state: the app reports it, opens no listener and stays passive.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_endpoint_passive_while_old_service_holds_state() {
    let live = Live::new("e-passive", 0, &[]);
    let (port, sd) = ep::state(&live);
    let mut holder = Command::new(&live.py)
        .args(["-I", "-c", "import sys; sys.path.insert(0, sys.argv[1]); from backend.coordinator import pki; l = pki.OwnerLock(sys.argv[2] + '/coordinator.lock', 'old service'); print('held', flush=True); sys.stdin.read()"])
        .arg(&live.helper)
        .arg(&sd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    std::io::BufRead::read_line(&mut std::io::BufReader::new(holder.stdout.take().unwrap()), &mut line).unwrap();
    assert_eq!(line.trim(), "held");
    let (api, _) = ep::start(&live, &sd);
    let st = ep::wait(&api, |s| !matches!(s, ck::app_api::Status::Starting));
    ev(format!("status with the old service holding the state: {st:?}; port {port} reachable {}", ep::reaches(port)));
    drop(holder.stdin.take());
    holder.wait().unwrap();
    assert!(matches!(&st, ck::app_api::Status::Passive { code, .. } if code == "service_conflict"), "not passive: {st:?}");
    assert!(!ep::reaches(port), "a passive app opened a listener");
    assert!(api.stop(Duration::from_secs(20)));
}

/// A damaged runtime (stdlib without `encodings`) gives a clear failure status, not an abort of the process.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_endpoint_damaged_runtime_is_a_notice_not_an_abort() {
    let live = Live::new("e-damaged", 0, &[]);
    let good = ep::paths(&live);
    // Copy the runtime (read-only source) and remove `encodings` from the copy.
    let home = live.base.join("rt");
    let cp = Command::new("/usr/bin/cp").args(["-rL"]).arg(&good.home).arg(&home).status().unwrap();
    assert!(cp.success());
    for e in std::fs::read_dir(home.join("lib")).unwrap().flatten() {
        let enc = e.path().join("encodings");
        if enc.is_dir() {
            std::fs::remove_dir_all(&enc).unwrap();
        }
    }
    let lib = good.library.strip_prefix(&good.home).map(|r| home.join(r)).unwrap_or_else(|_| good.library.clone());
    let bad = ck::app_api::Paths { library: lib, home: home.clone(), roots: good.roots.clone() };
    let (_, sd) = ep::state(&live);
    let api = ck::app_api::AppApi::start(bad.clone(), sd, std::sync::Arc::new(|| {}));
    let st = ep::wait(&api, |s| !matches!(s, ck::app_api::Status::Starting));
    ev(format!("check_runtime: {:?}; endpoint status: {st:?}", ck::app_api::check_runtime(&bad)));
    assert!(matches!(&st, ck::app_api::Status::Failed { code, .. } if code == "runtime_unavailable"), "expected Failed runtime_unavailable: {st:?}");
    ev("the process is still alive after the damaged-runtime start (no abort)");
    api.stop(Duration::from_secs(5));
}

// ====================================================================== import

mod imp {
    use super::*;
    /// A movie of the first `n` frames of the real clip's own source (stream copy), outside the project.
    pub fn movie(live: &Live, n: usize) -> PathBuf {
        let src = env("KEYLOOM_LIVE_CLIP").join("Source");
        let mov = std::fs::read_dir(&src).unwrap().flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|x| x == "mov" || x == "mp4")).expect("the real clip has a Source movie");
        let out = live.base.join("orig/SHOT_03.mov");
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let st = Command::new("/usr/bin/ffmpeg").args(["-v", "error", "-y", "-i"]).arg(&mov).args(["-frames:v", &n.to_string(), "-c", "copy"]).arg(&out).status().unwrap();
        assert!(st.success());
        out
    }
    /// Import `mov` into a new project (auto-confirmed) and return the driver in the workspace plus the project folder.
    pub fn import(live: &Live, mov: &Path) -> (Drv, PathBuf) {
        let ctx = eframe::egui::Context::default();
        let data = live.base.join("data");
        let app = ck::ui::App::with_ctx(&ctx, ck::ui::AppOptions { data_dir: Some(data.clone()), repo: Some(live.helper.clone()), import_videos: vec![mov.to_path_buf()], auto_confirm: true, settings_path: Some(live.settings.clone()), ..Default::default() });
        let mut d = Drv { ctx, app, texts: vec![], t0: Instant::now() };
        assert!(d.pump(180, "import reaches the workspace", |d| d.app.in_workspace()), "import never reached the workspace: {:?}", d.screen());
        let proj = std::fs::read_dir(data.join("Projects")).unwrap().flatten().map(|e| e.path()).next().expect("a project was created");
        (d, proj)
    }
    pub fn clip_dir(proj: &Path) -> PathBuf {
        std::fs::read_dir(proj.join("clips")).unwrap().flatten().map(|e| e.path()).next().expect("a clip")
    }
}

fn clip_error(proj: &Path) -> Option<String> {
    let p = ck::project::load_project(proj, &ck::project::LoadOptions::default()).unwrap();
    p.clips.first().expect("a clip").error.clone()
}

#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_import_movie_in_place_makes_no_source_copy_and_extracts() {
    let live = Live::new("i-place", 0, &[]);
    let mov = imp::movie(&live, frames_or(6));
    let (mut d, proj) = imp::import(&live, &mov);
    let clip = imp::clip_dir(&proj);
    let cj: Value = serde_json::from_str(&std::fs::read_to_string(clip.join("clip.json")).unwrap()).unwrap();
    ev(format!("clip.json source: {}; Source/ exists: {}", cj["source"], clip.join("Source").exists()));
    assert_eq!(cj["source"]["copied"], json!(false), "the import copied the movie: {}", cj["source"]);
    assert!(count_files(&clip.join("Source")) == 0, "a copy of the movie sits in Source/");
    assert_eq!(cj["source"]["original_path"], json!(mov.to_str().unwrap()));
    // Extraction: the import extracts on its own, or the Media tab does it; wait for the frames.
    let frames_dir = clip.join("Frames");
    d.pump(240, "frames extracted", |_| count_files(&frames_dir) >= 1 && count_files(&frames_dir) == frames_or(6));
    ev(format!("Frames/ after import: {} files; original untouched: {}", count_files(&frames_dir), mov.exists()));
    assert_eq!(count_files(&frames_dir), frames_or(6), "the extraction did not write every frame");
    assert!(mov.exists());
}

/// The Hint-tab "Extract frames (validated)" re-extracts from the movie where it is when the Frames folder is gone.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_import_hint_tab_extract_works() {
    let live = Live::new("i-extract", 0, &[]);
    let mov = imp::movie(&live, frames_or(6));
    let (d, proj) = imp::import(&live, &mov);
    let clip = imp::clip_dir(&proj);
    let name = clip.file_name().unwrap().to_string_lossy().to_string();
    drop(d);
    // Back to the bare movie: no extracted frames; reopen on the Hint tab.
    let _ = std::fs::remove_dir_all(clip.join("Frames"));
    let live2 = live.for_project(proj.clone());
    let mut d = Drv::new(&live2, true, Some(&name), Some(Tab::Hint));
    d.warm();
    assert!(d.pump(120, "Hint tab", |d| d.app.in_workspace() && d.idle_ready()), "{}", d.targets());
    d.frames(10);
    let c = d.app.clip_for_test();
    ev(format!("clip source {:?}; state {:?}; Frames files {}", c.source, c.state, count_files(&clip.join("Frames"))));
    // "Extract frames" is an item of the Hint action bar's ⋯ menu: send its act.
    let r = run_with(&mut d, &live2, "Hint-tab Extract", 300, |d| {
        let ctx = d.ctx.clone();
        d.app.act_for_test(&ctx, ck::ui::shot_page::Act::Props(ck::ui::shot_page::PropAct::Hint(ck::ui::shot_page::HintAct::ExtractSource)));
    });
    r.evidence();
    r.assert_complete();
    let n = count_files(&clip.join("Frames"));
    ev(format!("Frames/ after Hint-tab Extract: {n} files (movie has {})", frames_or(6)));
    assert_eq!(n, frames_or(6));
}

/// A missing original shows "Source movie not found at <path>: reconnect or relink it"; renaming it back recovers.
#[test]
#[ignore = "live: needs the real worker/GPU/models; run tools/live-test.sh"]
fn live_import_missing_original_is_named_and_recovers() {
    let live = Live::new("i-missing", 0, &[]);
    let mov = imp::movie(&live, 6);
    let (d, proj) = imp::import(&live, &mov);
    let clip = imp::clip_dir(&proj);
    let name = clip.file_name().unwrap().to_string_lossy().to_string();
    drop(d);
    let _ = std::fs::remove_dir_all(clip.join("Frames")); // frames take precedence over the movie
    let gone = mov.with_extension("moved");
    std::fs::rename(&mov, &gone).unwrap();
    let live2 = live.for_project(proj.clone());
    let want = format!("Source movie not found at {}: reconnect or relink it", mov.display());
    let mut d = Drv::new(&live2, true, None, None);
    d.pump(60, "Board", |d| d.has("SHOT_03") || d.has_containing("not found"));
    d.frames(10);
    let err = clip_error(&proj);
    let on_board = d.has_containing("Source movie not found");
    ev(format!("clip.error = {err:?}; drawn on the Board: {on_board}"));
    let mut shown = on_board;
    if !shown {
        // Not on the Board card: open the clip and look at the Media tab.
        let mut d2 = Drv::new(&live2, true, Some(&name), Some(Tab::Media));
        d2.frames(30);
        shown = d2.has_containing("not found at");
        ev(format!("Media tab: {:?}", d2.find_containing("not found")));
    }
    assert!(err.as_deref() == Some(want.as_str()) || err.as_deref().is_some_and(|e| e.contains("Source movie not found at") && e.contains("reconnect or relink it")), "wrong or no clip error: {err:?}, wanted {want}");
    assert!(shown, "the message is not drawn anywhere (Board card / Media tab)");
    drop(d);
    std::fs::rename(&gone, &mov).unwrap();
    let err2 = clip_error(&proj);
    ev(format!("after renaming it back: clip.error = {err2:?}"));
    assert!(err2.is_none(), "the error stayed after the original came back: {err2:?}");
}
