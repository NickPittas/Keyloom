//! Marked-frame behaviour: what the Pre-matte / Hint buttons queue for each checkbox state, what is saved, and the
//! viewer's mark controls and overlay chips (including narrow and wide windows).
mod common;
use ck::hints::{in_frame_gate, staging_root, Final, Gen, HintCfg, Producer};
use ck::persist;
use ck::project::{load_project, Clip, LoadOptions};
use ck::tasks::TaskKind;
use ck::ui::viewer::{MarkReq, Overlay, Viewer};
use ck::ui::{App, AppOptions, Tab};
use ck::workflow::new_id;
use common::*;
use eframe::egui::{self, Pos2, RawInput, Rect, Vec2};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn png(p: &Path) {
    let mut e = png::Encoder::new(std::fs::File::create(p).unwrap(), 4, 4);
    e.set_color(png::ColorType::Rgb);
    e.set_depth(png::BitDepth::Eight);
    e.write_header().unwrap().write_image_data(&[7; 48]).unwrap();
}

/// A 5-frame project; `guides` = indices that already have a pre-matte.
fn project(tag: &str, clip_json: &str, guides: &[usize]) -> (PathBuf, PathBuf) {
    let base = tmp(tag);
    let proj = base.join("proj");
    let root = proj.join("clips/c1");
    std::fs::create_dir_all(root.join("Frames")).unwrap();
    for i in 0..5 {
        png(&root.join(format!("Frames/f_{}.png", 1101 + i)));
    }
    std::fs::write(root.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
    std::fs::write(root.join("clip.json"), clip_json).unwrap();
    std::fs::write(proj.join("project.json"), r#"{"version":2}"#).unwrap();
    if !guides.is_empty() {
        std::fs::create_dir_all(root.join("VideoMamaMaskHint")).unwrap();
    }
    for g in guides {
        png(&root.join(format!("VideoMamaMaskHint/f_{}.png", 1101 + g)));
    }
    (base, proj)
}

fn load(proj: &Path) -> Clip {
    load_project(proj, &LoadOptions::default()).unwrap().clips[0].clone()
}

fn cfg(finish: Final, pre: bool, hint: bool) -> HintCfg {
    let mut c = HintCfg::default();
    // Chroma needs no prompts, so guide steps can really be built.
    c.guide = Producer::Chroma;
    c.finish = finish;
    c.marks_pre = pre;
    c.marks_hint = hint;
    c
}

fn run(clip: &Clip, cfg: &HintCfg, range: (usize, usize), marks: &[usize]) -> Result<Vec<(TaskKind, Option<(usize, usize)>, String)>, String> {
    let stg = staging_root(&clip.root, &clip.root, &clip.folder);
    let steps = Gen { clip, cfg, ann: None, linear: false, staging: &stg }.hint_run(range, marks, &mut new_id)?;
    Ok(steps
        .into_iter()
        .map(|j| {
            let dir = j.publish_dir.as_ref().and_then(|p| p.file_name()).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            (j.kind, j.range.map(|r| (r.in_point, r.out_point)), dir)
        })
        .collect())
}

fn kinds(v: &[(TaskKind, Option<(usize, usize)>, String)]) -> Vec<TaskKind> {
    v.iter().map(|s| s.0).collect()
}

#[test]
fn matanyone2_marks_dense_and_sparse() {
    let (_b, proj) = project("mf-mat", "{}", &[]);
    let c = load(&proj);
    // Pre-matte and hint on the marks: a pre-matte and a refine for each mark.
    let s = run(&c, &cfg(Final::Matanyone2, true, true), (0, 4), &[1, 3]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::Chroma, TaskKind::Matanyone2, TaskKind::Chroma, TaskKind::Matanyone2]);
    assert_eq!(s[1].1, Some((1, 1)));
    assert_eq!(s[3].1, Some((3, 3)));
    // Marked pre-matte only: ONE refine over In/Out from the In-frame pre-matte (made first, In is marked).
    let s = run(&c, &cfg(Final::Matanyone2, true, false), (0, 4), &[0, 3]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::Chroma, TaskKind::Matanyone2]);
    assert_eq!((s[0].1, s[1].1), (Some((0, 0)), Some((0, 4))));
    // Neither box: the usual dense run, no marks involved.
    let err = run(&c, &cfg(Final::Matanyone2, false, true), (0, 4), &[1]).unwrap_err();
    assert!(err.contains("generate the guide first"), "{err}");
}

#[test]
fn in_frame_must_be_marked_or_already_have_a_pre_matte() {
    let (_b, proj) = project("mf-in", "{}", &[]);
    let c = load(&proj);
    let sparse = cfg(Final::Matanyone2, true, false);
    assert_eq!(in_frame_gate(&c, &sparse, (0, 4), &[2]), Err("Mark In frame first".into()));
    assert_eq!(run(&c, &sparse, (0, 4), &[2]).unwrap_err(), "Mark In frame first", "nothing is marked or queued behind the user's back");
    assert!(in_frame_gate(&c, &sparse, (0, 4), &[0, 2]).is_ok());
    // A pre-matte already on disk for the In frame is enough.
    let (_b2, proj2) = project("mf-in2", "{}", &[0]);
    let c2 = load(&proj2);
    assert!(in_frame_gate(&c2, &sparse, (0, 4), &[2]).is_ok());
    let s = run(&c2, &sparse, (0, 4), &[2]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::Matanyone2], "existing In pre-matte reused");
    // The gate belongs to this choice only.
    assert!(in_frame_gate(&c, &cfg(Final::Matanyone2, true, true), (0, 4), &[2]).is_ok());
    assert!(in_frame_gate(&c, &cfg(Final::Videomama, true, false), (0, 4), &[2]).is_ok());
}

#[test]
fn videomama_rejects_a_dense_run_over_sparse_pre_mattes_and_works_on_marks() {
    let (_b, proj) = project("mf-vm", "{}", &[1, 3]);
    let c = load(&proj);
    let err = run(&c, &cfg(Final::Videomama, false, true), (0, 4), &[1, 3]).unwrap_err();
    assert!(err.contains("generate the guide first") && err.contains("frames 0-4"), "{err}");
    // The one combined checkbox: existing pre-mattes are reused, a missing one is made, each mark is refined.
    let s = run(&c, &cfg(Final::Videomama, true, true), (0, 4), &[1, 2, 3]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::Videomama, TaskKind::Chroma, TaskKind::Videomama, TaskKind::Videomama]);
    assert_eq!(s[1].1, Some((2, 2)));
}

/// Writes the worker's record for the pre-matte folder, as made by `kind` with `params` from the clip's current frames.
fn certify(c: &Clip, kind: &str, params: &Value) {
    let d = c.root.join("VideoMamaMaskHint");
    let stems: Vec<String> = c.input_files.iter().map(|f| f.0.clone()).filter(|s| d.join(format!("{s}.png")).exists()).collect();
    let by = |f: &dyn Fn(&str) -> Value| stems.iter().map(|s| (s.clone(), f(s))).collect::<serde_json::Map<_, _>>();
    let rec = json!({
        "kind": kind, "job_id": "old", "params": params, "stems": stems,
        "digests": by(&|s| json!(ck::sha256::hex(&std::fs::read(d.join(format!("{s}.png"))).unwrap()))),
        "stats": by(&|s| ck::hints::stamp_value(&d.join(format!("{s}.png")))),
        "source": by(&|s| ck::hints::stamp_value(&c.input_files.iter().find(|f| f.0 == s).unwrap().1)),
    });
    std::fs::write(d.join(".corridorkey_hint.json"), json!({"kind": kind, "ranges": [rec]}).to_string()).unwrap();
}

fn chroma_params(h: &HintCfg) -> Value {
    h.params(h.guide.task())
}

#[test]
fn direct_publishes_the_pre_matte_then_promotes_it_instead_of_running_twice() {
    let (_b, proj) = project("mf-direct", "{}", &[1]);
    let c = load(&proj);
    let h = cfg(Final::Direct, true, true);
    // A bare pre-matte has no record: it is not certified, so it is rebuilt (producer, then publish).
    let s = run(&c, &h, (0, 4), &[1]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::Chroma, TaskKind::PrematteHint], "unrecorded pre-matte runs the producer");
    // Certified frame 1 is reused; frame 3 has no pre-matte, so it is produced once, then promoted.
    certify(&c, "chroma", &chroma_params(&h));
    let s = run(&c, &h, (0, 4), &[1, 3]).unwrap();
    assert_eq!(kinds(&s), [TaskKind::PrematteHint, TaskKind::Chroma, TaskKind::PrematteHint]);
    assert_eq!(s[1].2, "VideoMamaMaskHint");
    assert_eq!((s[0].2.as_str(), s[2].2.as_str()), ("AlphaHint", "AlphaHint"));
    assert_eq!((s[0].1, s[1].1, s[2].1), (Some((1, 1)), Some((3, 3)), Some((3, 3))));
    // Unchecked: the pre-matte over In/Out (it is not all there), then published, like the Board plan.
    let s = run(&c, &cfg(Final::Direct, false, true), (0, 4), &[1, 3]).unwrap();
    assert_eq!((kinds(&s), s[0].2.as_str(), s[1].2.as_str()), (vec![TaskKind::Chroma, TaskKind::PrematteHint], "VideoMamaMaskHint", "AlphaHint"));
}

#[test]
fn direct_hint_reuses_only_a_pre_matte_certified_for_this_producer_settings_and_source() {
    let steps = |c: &Clip, h: &HintCfg| {
        let stg = staging_root(&c.root, &c.root, &c.folder);
        Gen { clip: c, cfg: h, ann: None, linear: false, staging: &stg }.hint((0, 4), false, &mut new_id).unwrap().len()
    };
    let h = cfg(Final::Direct, false, true);
    let all: Vec<usize> = (0..5).collect();
    let (_b, proj) = project("mf-cert", "{}", &all);
    let c = load(&proj);
    assert_eq!(steps(&c, &h), 2, "bare pre-matte");
    certify(&c, "chroma", &chroma_params(&h));
    assert_eq!(steps(&c, &h), 1, "matching record: publish only");
    let mut other = h.clone();
    other.chroma.strength += 0.25;
    assert_ne!(chroma_params(&other), chroma_params(&h), "the setting reaches the params");
    certify(&c, "chroma", &chroma_params(&other));
    assert_eq!(steps(&c, &h), 2, "different settings");
    certify(&c, "gvm", &chroma_params(&h));
    assert_eq!(steps(&c, &h), 2, "different producer");
    let (_b, proj) = project("mf-cert-none", "{}", &[]);
    assert_eq!(steps(&load(&proj), &h), 2, "no pre-matte");
    // A source frame that is gone must not panic: the producer runs.
    certify(&c, "chroma", &chroma_params(&h));
    let mut gone = c.clone();
    gone.input_files.remove(2);
    let stg = staging_root(&gone.root, &gone.root, &gone.folder);
    let g = Gen { clip: &gone, cfg: &h, ann: None, linear: false, staging: &stg };
    let _ = g.hint((0, 3), false, &mut new_id);
}

#[test]
fn no_marks_means_a_clear_error_and_nothing_queued() {
    let (_b, proj) = project("mf-none", "{}", &[]);
    let c = load(&proj);
    for f in [Final::Matanyone2, Final::Videomama, Final::Direct] {
        let e = run(&c, &cfg(f, true, true), (0, 4), &[]).unwrap_err();
        assert!(e.contains("mark frames first"), "{f:?}: {e}");
    }
    let stg = staging_root(&c.root, &c.root, &c.folder);
    let g = Gen { clip: &c, cfg: &cfg(Final::Direct, true, true), ann: None, linear: false, staging: &stg };
    assert!(g.guide_marks(&[], &mut new_id).unwrap_err().contains("mark frames first"));
    assert_eq!(g.guide_marks(&[1, 2], &mut new_id).unwrap().len(), 2);
}

#[test]
fn old_clips_load_unchanged_and_the_consumption_mode_only_changes_on_a_user_action() {
    // Saved hint settings from before this feature, with an explicit "none".
    let json = r#"{"ck_native":{"hint":{"guide":"gvm","final":"direct"},"hint_mode":"none"}}"#;
    let (_b, proj) = project("mf-old", json, &[]);
    let c = load(&proj);
    let h = HintCfg::load(&c);
    assert!(!h.marks_pre && h.marks_hint, "defaults for a config that never had the keys");
    assert!(!h.pre_on_marks() && !h.hint_on_marks());
    assert_eq!(persist::clip_hint_mode(&c).as_deref(), Some("none"));
    // The user checks the box: settings and keyframes are saved together, over the saved "none".
    let mut on = h.clone();
    on.marks_pre = true;
    persist::save_marked_hints(&c, &on, "keyframes", ("none", false)).unwrap();
    let c = load(&proj);
    assert_eq!(persist::clip_hint_mode(&c).as_deref(), Some("keyframes"));
    assert!(HintCfg::load(&c).hint_on_marks());
    assert_eq!(c.native["hint"]["guide"], "gvm", "other saved settings kept");
    // Unchecking saves the dense choice.
    persist::save_marked_hints(&c, &h, "full", ("keyframes", false)).unwrap();
    let c = load(&proj);
    assert_eq!(persist::clip_hint_mode(&c).as_deref(), Some("full"));
    assert!(!HintCfg::load(&c).hint_on_marks());
    // A mode that does not exist is refused and nothing is written.
    assert!(persist::save_marked_hints(&c, &on, "sparse", ("full", false)).is_err());
    assert_eq!(persist::clip_hint_mode(&load(&proj)).as_deref(), Some("full"));
}

fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1440.0, 900.0))), ..Default::default() };
        let mut out = ctx.run_ui(raw, |ui| app.frame(ui));
        out.textures_delta.clear();
    }
}

fn open(tag: &str, clip_json: &str) -> (PathBuf, egui::Context, App) {
    let (base, proj) = project(tag, clip_json, &[]);
    let repo = make_repo(&base, "w0", json!({}));
    let settings = base.join("settings.json");
    std::fs::write(&settings, json!({"version":1,"ai_targets":[{"id":"l0","kind":"local","repo":repo,"python":PY,"gpu":0}]}).to_string()).unwrap();
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(&ctx, AppOptions { project: Some(proj.clone()), settings_path: Some(settings), open_clip: Some("c1".into()), tab: Some(Tab::Hint), ..Default::default() });
    frames(&ctx, &mut app, 12);
    (proj, ctx, app)
}

#[test]
fn the_sam_layer_exists_only_while_sam_is_the_producer() {
    for (guide, sam) in [("sam", true), ("gvm", false)] {
        let (_p, ctx, mut app) = open(&format!("mf-prod-{guide}"), &format!(r#"{{"ck_native":{{"hint":{{"guide":"{guide}"}}}}}}"#));
        frames(&ctx, &mut app, 4);
        let mut seen = None;
        app.viewer_mut_for_test(|v| seen = Some((v.sam_available, v.sam_overlay, v.overlay)));
        assert_eq!(seen, Some((sam, true, Overlay::Off)), "{guide}");
    }
}

#[test]
fn mark_controls_save_marks_and_clicking_a_mark_seeks() {
    let (proj, ctx, mut app) = open("mf-marks", "{}");
    for i in [1usize, 3] {
        app.viewer_mut_for_test(|v| {
            v.set_playhead(i);
            v.mark_req = Some(MarkReq::Mark);
        });
        frames(&ctx, &mut app, 4);
    }
    let mut seen = None;
    app.viewer_mut_for_test(|v| seen = Some(v.marks.clone()));
    assert_eq!(seen, Some(vec![1, 3]));
    assert_eq!(load(&proj).keyframes, [1, 3], "saved with the stored frame identities");
    app.viewer_mut_for_test(|v| {
        v.set_playhead(1);
        v.mark_req = Some(MarkReq::Unmark);
    });
    frames(&ctx, &mut app, 4);
    assert_eq!(load(&proj).keyframes, [3]);
    app.viewer_mut_for_test(|v| v.mark_req = Some(MarkReq::Clear));
    frames(&ctx, &mut app, 4);
    assert!(load(&proj).keyframes.is_empty());
    let raw: Value = serde_json::from_slice(&std::fs::read(proj.join("clips/c1/clip.json")).unwrap()).unwrap();
    assert!(raw["ck_native"].get("hint_mode").is_none(), "marking frames never touches the consumption mode");
}

/// The top bar (A/B wipe + overlay chips + SAM) and the marks row fit narrow and wide windows: every control is
/// inside the viewer and none overlaps another, with and without the B-view picker.
#[test]
fn moved_toolbar_and_marks_row_fit_narrow_and_wide_windows() {
    let base = tmp("mf-layout");
    let root = base.join("p");
    std::fs::create_dir_all(root.join("clips/c/Frames")).unwrap();
    std::fs::write(root.join("project.json"), r#"{"version":2,"clips":["c"]}"#).unwrap();
    std::fs::write(root.join("clips/c/.video_metadata.json"), r#"{"fps":24.0}"#).unwrap();
    for i in 0..8 {
        png(&root.join(format!("clips/c/Frames/f_{i:03}.png")));
    }
    let ctx = egui::Context::default();
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let vctx = egui::Context::default();
    let mut v = Viewer::new(0, &p.clips[0], 16, &ctx);
    v.marks = vec![1, 4, 6];
    v.range = Some((0, 5));
    v.sam_available = true;
    for w in [420.0f32, 560.0, 700.0, 900.0, 1100.0, 1400.0, 1900.0] {
        for ab in [false, true] {
            v.ab = ab;
            let mut out = vctx.run_ui(
                RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(w, 800.0))), ..Default::default() },
                |ui| v.ui(ui),
            );
            out.textures_delta.clear();
            let area = Rect::from_min_size(Pos2::ZERO, Vec2::new(w, 800.0));
            let r = v.ui_rects.clone();
            for name in ["ab", "overlay_off", "overlay_guide", "overlay_hint", "sam", "mark", "unmark", "clear_marks"] {
                assert!(r.iter().any(|x| x.0 == name), "{name} missing at {w}");
            }
            assert_eq!(r.iter().any(|x| x.0 == "marks_text"), w >= 1028.0, "the count shares the status line when the transport is wide, else the status text");
            let chips = r.iter().find(|x| x.0 == "chips").unwrap().1;
            assert!(chips.width() <= 400.0, "mode chips {} wide: raise CHIPS_W", chips.width());
            for (n, a) in &r {
                assert!(area.contains_rect(*a), "{n} {a:?} outside the viewer at width {w} (ab={ab})");
            }
            for (i, (n1, a)) in r.iter().enumerate() {
                for (n2, b) in &r[i + 1..] {
                    assert!(!a.shrink(0.5).intersects(b.shrink(0.5)), "{n1} {a:?} overlaps {n2} {b:?} at width {w} (ab={ab})");
                }
            }
        }
    }
}
