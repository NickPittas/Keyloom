//! The frozen Key job contract: the Rust wire type is accepted by the REAL coordinator validators (run through a
//! stdlib-only Python helper), the coordinator's own fixture deserializes here, long clips stay compact, and the
//! receiver's preflight scans its own copy of the clip (self-contained, matching stems and hints) before anything
//! starts. Two PCs with different mounts; temp dirs only; no network.
mod common;
use ck::contracts::{ClipOutputValues, OutputLocation, RangeIdentity};
use ck::jobs::{common_dir, resolve_outputs, JobMode, Require};
use ck::shared_paths::SharedRoot;
use ck::submission::{Envelope, JobSpec};
use ck::workflow::{OutputCfg, Params};
use serde_json::{json, Value};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const SID: &str = "0123456789abcdef0123";

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}
fn root(n: &str, l: &Path) -> SharedRoot {
    SharedRoot { name: n.into(), local: l.to_str().unwrap().into(), ..Default::default() }
}

/// The real `validate_spec` (and id/target checks) on a submit body.
fn python_says(body: &Value) -> Result<Value, String> {
    let mut c = Command::new(common::PY)
        .args(["-I", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/validate_spec.py"), repo_root().to_str().unwrap()])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    c.stdin.take().unwrap().write_all(body.to_string().as_bytes()).unwrap();
    let out = c.wait_with_output().unwrap();
    let line = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let v: Value = serde_json::from_str(&line).unwrap_or_else(|_| panic!("helper failed: {line} {}", String::from_utf8_lossy(&out.stderr)));
    if v["ok"] == true { Ok(v["info"].clone()) } else { Err(v["error"].as_str().unwrap().to_string()) }
}
fn envelope(spec: &JobSpec) -> Envelope {
    Envelope { submission_id: SID.into(), spec: spec.clone(), target: None }
}

/// A project on one PC: `<mount>/projects/Show/clips/shot_01` with 3 frames named f_1101.. and a full AlphaHint.
fn project(mount: &Path) -> PathBuf {
    let clip = mount.join("projects/Show/clips/shot_01");
    for d in ["Frames", "AlphaHint"] {
        fs::create_dir_all(clip.join(d)).unwrap();
        for i in 0..3 {
            let mut e = png::Encoder::new(fs::File::create(clip.join(d).join(format!("f_{}.png", 1101 + i))).unwrap(), 2, 2);
            e.set_color(png::ColorType::Grayscale);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[9; 4]).unwrap();
        }
    }
    fs::write(clip.join("clip.json"), "{}").unwrap();
    fs::write(mount.join("projects/Show/project.json"), r#"{"version":2}"#).unwrap();
    fs::create_dir_all(mount.join("out")).unwrap();
    clip
}
struct Pcs {
    base: PathBuf,
    a: PathBuf,
    b: PathBuf,
}
fn pcs(tag: &str) -> Pcs {
    let base = common::tmp(tag);
    let (a, b) = (base.join("pcA/nas"), base.join("pcB/mnt/share"));
    project(&a);
    project(&b);
    Pcs { base, a, b }
}
fn roots(m: &Path) -> Vec<SharedRoot> {
    vec![root("projects", &m.join("projects")), root("out", &m.join("out"))]
}
fn params(hint_mode: &str) -> Params {
    Params { hint_mode: hint_mode.into(), temporal_window: "1/1".into(), ..Params::default() }
}
fn out_cfg() -> OutputCfg {
    OutputCfg { fg_enabled: false, comp_enabled: false, processed_enabled: false, matte_format: "png".into(), ..OutputCfg::default() }
}
fn require() -> Require {
    Require { protocol: 1, models: [("corridorkey_v2".to_string(), Some("aa".to_string()))].into() }
}
/// Plan on PC A the way the app does (templates -> per-frame outputs -> common folder), then build the spec.
fn spec(p: &Pcs, hint_mode: &str, marks: Option<Vec<usize>>) -> JobSpec {
    let loc = OutputLocation { root: p.a.join("out"), folder_template: "{project}/{name}/{output}".into(), file_template: "{name}_{output}_####".into() };
    let values = ClipOutputValues { project: "Show".into(), name: "shot_01".into(), resolution: "HD".into(), date: "2026-10-23".into(), version: 1 };
    let o = out_cfg();
    let stems: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
    let outputs = resolve_outputs(&loc, &values, &o.kinds(), &stems).unwrap();
    let mdir = common_dir(&outputs).unwrap();
    JobSpec::from_parts(
        &roots(&p.a),
        &p.a.join("projects/Show/clips/shot_01"),
        &mdir,
        "clip-1",
        RangeIdentity { in_point: 0, out_point: 2, in_stem: "f_1101".into(), out_stem: "f_1103".into() },
        &params(hint_mode),
        &o,
        marks,
        &loc,
        &values,
        require(),
        Some("Quality"),
    )
    .unwrap()
}

// ------------------------------------------------------------------------------------------------ contract

#[test]
fn the_rust_wire_type_is_accepted_by_the_real_coordinator_validators() {
    let p = pcs("sub-contract");
    let s = spec(&p, "none", None);
    let info = python_says(&serde_json::to_value(envelope(&s)).unwrap()).expect("validate_spec accepts it");
    assert_eq!(info["roots"], json!(["out", "projects"]), "the server sees exactly the three references' roots");
    let wire = serde_json::to_string(&envelope(&s)).unwrap();
    assert!(!wire.contains(p.a.to_str().unwrap()), "no host path on the wire: {wire}");
    assert!(!wire.contains("sources") && !wire.contains("outputs"), "no per-frame lists");
    // keyframes mode, another preset-less spec, and a targeted submission also pass.
    let k = spec(&p, "keyframes", Some(vec![0, 2]));
    python_says(&serde_json::to_value(envelope(&k)).unwrap()).expect("keyframes spec");
    let mut e = envelope(&JobSpec { preset_name: None, ..s.clone() });
    e.target = Some("abcdefghij0123456789".into());
    let v = serde_json::to_value(&e).unwrap();
    assert!(v["spec"].get("preset_name").is_none(), "omitted, not null");
    python_says(&v).expect("no preset name, explicit target");
    // What Rust refuses before sending, Python refuses too (same rules, not a looser copy).
    for (what, edit) in [
        ("mode", json!({"mode": "resume"})),
        ("kind", json!({"kind": "hint"})),
        ("publish", json!({"publish": "draft"})),
        ("clip_root", json!({"clip_root": "/mnt/nas/shot"})),
        ("manifest_dir", json!({"manifest_dir": "shared:out/../x"})),
    ] {
        let mut v = serde_json::to_value(envelope(&s)).unwrap();
        for (k, x) in edit.as_object().unwrap() {
            v["spec"][k] = x.clone();
        }
        assert!(python_says(&v).is_err(), "{what}: the server must refuse it");
        if let Ok(bad) = serde_json::from_value::<Envelope>(v) {
            assert!(bad.check().is_err(), "{what}: this build must refuse it before sending");
        }
    }
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn the_coordinators_own_fixture_deserializes_and_round_trips_exactly() {
    let out = Command::new(common::PY)
        .args(["-I", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/dump_spec.py"), repo_root().join("tests/coordinator/specs.py").to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let theirs: Value = serde_json::from_slice(&out.stdout).unwrap();
    let ours: JobSpec = serde_json::from_value(theirs.clone()).expect("the Python fixture deserializes");
    assert_eq!(serde_json::to_value(&ours).unwrap(), theirs, "and serializes back to the same JSON");
    ours.check_shape().unwrap();
    // An unknown key is not silently dropped.
    let mut extra = theirs;
    extra["path_map"] = json!([]);
    assert!(serde_json::from_value::<JobSpec>(extra).is_err());
}

#[test]
fn a_long_clip_stays_compact() {
    let p = pcs("sub-long");
    let mut s = spec(&p, "none", None);
    s.range = RangeIdentity { in_point: 0, out_point: 199_999, in_stem: "f_0000001".into(), out_stem: "f_0200000".into() };
    let wire = serde_json::to_vec(&envelope(&s)).unwrap();
    assert!(wire.len() < 2048, "{} bytes for 200000 frames", wire.len());
    python_says(&serde_json::from_slice(&wire).unwrap()).expect("accepted by the server schema");
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn envelope_ids_follow_the_servers_rules() {
    let p = pcs("sub-env");
    let s = spec(&p, "none", None);
    for bad in ["short", "has space has space!", &"x".repeat(65)] {
        assert!(Envelope { submission_id: bad.into(), spec: s.clone(), target: None }.check().is_err(), "{bad}");
        let v = serde_json::to_value(Envelope { submission_id: bad.into(), spec: s.clone(), target: None }).unwrap();
        assert!(python_says(&v).is_err(), "server refuses {bad}");
    }
    assert!(envelope(&s).check().is_ok());
    let _ = fs::remove_dir_all(p.base);
}

// ------------------------------------------------------------------------------------------------ receiver

#[test]
fn the_receiver_expands_outputs_through_its_own_mount_from_templates() {
    let p = pcs("sub-resolve");
    let s: JobSpec = serde_json::from_str(&serde_json::to_string(&spec(&p, "full", None)).unwrap()).unwrap();
    let pre = s.preflight(&roots(&p.b), "attempt-1").unwrap();
    assert_eq!((pre.job.job_id.as_str(), pre.job.mode), ("attempt-1", JobMode::New));
    let all: Vec<&PathBuf> = std::iter::once(&pre.job.clip_root).chain(std::iter::once(&pre.job.manifest_dir)).chain(pre.job.outputs.values().flatten()).collect();
    assert!(all.iter().all(|x| x.starts_with(&p.b) && !x.starts_with(&p.a)), "{all:?}");
    assert_eq!(pre.job.outputs["matte"].len(), 3, "per-frame paths exist on the receiver only");
    assert_eq!(pre.job.outputs["matte"][0], p.b.join("out/Show/shot_01/Matte/shot_01_Matte_1101.png"));
    assert_eq!(pre.job.manifest_dir, p.b.join("out/Show/shot_01/Matte"));
    assert!(pre.probes.iter().all(|(_, a)| a.write.is_ok()), "write access was probed");
    assert!(!p.b.join("out/Show").exists(), "preflight created nothing");
    let _ = fs::remove_dir_all(p.base);
}

fn problems(p: &Pcs, s: &JobSpec) -> Vec<String> {
    s.preflight(&roots(&p.b), "j").unwrap_err()
}
fn has(v: &[String], needle: &str) -> bool {
    v.iter().any(|l| l.contains(needle))
}

#[test]
fn external_or_movie_sources_and_mismatched_frames_are_refused_before_start() {
    let p = pcs("sub-source");
    let s = spec(&p, "none", None);
    let clip = p.b.join("projects/Show/clips/shot_01");
    // No mapping for the share on this PC.
    assert!(has(&s.preflight(&[root("projects", &p.b.join("projects"))], "j").unwrap_err(), "no path mapping for shared location \"out\""));
    // clip.json names an external source, but local Frames win (ClipEntry.find_assets order): the worker never opens it.
    fs::write(clip.join("clip.json"), r#"{"source":{"type":"sequence","original_path":"/sender/only/frames"}}"#).unwrap();
    let pre = s.preflight(&roots(&p.b), "j").expect("local Frames are the source");
    assert_eq!(pre.first_frame, clip.join("Frames/f_1101.png"));
    assert!(pre.job.source_map.is_empty(), "no external path to remap");
    // No local Frames at all, only an external sequence that exists on this PC.
    fs::remove_dir_all(clip.join("Frames")).unwrap();
    let ext = p.base.join("elsewhere");
    fs::create_dir_all(&ext).unwrap();
    for i in 0..3 {
        fs::write(ext.join(format!("f_{}.png", 1101 + i)), b"x").unwrap();
    }
    fs::write(clip.join("clip.json"), json!({"source": {"type": "sequence", "original_path": ext}}).to_string()).unwrap();
    assert!(has(&problems(&p, &s), "reads an external folder"), "{:?}", problems(&p, &s));
    // A movie clip.
    fs::write(clip.join("clip.json"), "{}").unwrap();
    fs::create_dir_all(clip.join("Source")).unwrap();
    fs::write(clip.join("Source/a.mp4"), b"x").unwrap();
    assert!(has(&problems(&p, &s), "extract its frames first"), "{:?}", problems(&p, &s));
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn the_range_must_name_this_pcs_own_frames_by_stem() {
    let p = pcs("sub-stems");
    let clip = p.b.join("projects/Show/clips/shot_01");
    // A different source with the same count: the stems differ.
    for i in 0..3 {
        fs::rename(clip.join(format!("Frames/f_{}.png", 1101 + i)), clip.join(format!("Frames/g_{}.png", 1101 + i))).unwrap();
    }
    let e = problems(&p, &spec(&p, "none", None));
    assert!(has(&e, "range does not match this PC's frames"), "{e:?}");
    for i in 0..3 {
        fs::rename(clip.join(format!("Frames/g_{}.png", 1101 + i)), clip.join(format!("Frames/f_{}.png", 1101 + i))).unwrap();
    }
    // Swapped In/Out stems.
    let mut s = spec(&p, "none", None);
    std::mem::swap(&mut s.range.in_stem, &mut s.range.out_stem);
    assert!(has(&problems(&p, &s), "range does not match"));
    // A range beyond the receiver's frames (the stems it names are not there).
    let mut s = spec(&p, "none", None);
    s.range.out_point = 9;
    assert!(has(&problems(&p, &s), "frame 9 is (no such frame) here"), "{:?}", problems(&p, &s));
    spec(&p, "none", None).preflight(&roots(&p.b), "j").expect("restored: accepted");
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn hint_coverage_is_checked_by_stem_not_by_count() {
    let p = pcs("sub-hints");
    let hints = p.b.join("projects/Show/clips/shot_01/AlphaHint");
    // Three files, so the count matches, but frame f_1102 has no hint and an unrelated one stands in.
    fs::rename(hints.join("f_1102.png"), hints.join("f_9999.png")).unwrap();
    let e = problems(&p, &spec(&p, "full", None));
    assert!(has(&e, "AlphaHint has no frame for 1 of the 3 selected frames, e.g. f_1102"), "{e:?}");
    // Keyframes mode checks the marked frames only (and that this PC's clip carries the same marks).
    let clip = p.b.join("projects/Show/clips/shot_01");
    let all: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
    ck::persist::save_keyframes(&clip, &[0, 1], &all).unwrap();
    assert!(has(&problems(&p, &spec(&p, "keyframes", Some(vec![0, 1]))), "marked frame f_1102"));
    ck::persist::save_keyframes(&clip, &[0, 2], &all).unwrap();
    spec(&p, "keyframes", Some(vec![0, 2])).preflight(&roots(&p.b), "j").expect("marks on frames that have hints");
    // Hint mode none needs none.
    spec(&p, "none", None).preflight(&roots(&p.b), "j").unwrap();
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn outputs_must_be_new_writable_and_where_the_spec_says() {
    use std::os::unix::fs::PermissionsExt;
    let p = pcs("sub-outputs");
    let s = spec(&p, "none", None);
    let m = p.b.join("out/Show/shot_01/Matte");
    fs::create_dir_all(&m).unwrap();
    fs::write(m.join("shot_01_Matte_1102.png"), b"old").unwrap();
    assert!(has(&problems(&p, &s), "output matte frame 2 already exists"));
    assert_eq!(fs::read(m.join("shot_01_Matte_1102.png")).unwrap(), b"old", "existing files are never touched");
    fs::remove_file(m.join("shot_01_Matte_1102.png")).unwrap();
    for name in [".corridorkey_manifest.json", ".corridorkey_job.lock"] {
        fs::write(m.join(name), b"{}").unwrap();
        assert!(has(&problems(&p, &s), "new-only"), "{name}");
        fs::remove_file(m.join(name)).unwrap();
    }
    // The spec's result folder must be where the templates write.
    let mut t = s.clone();
    t.manifest_dir = "shared:out/Show/elsewhere".into();
    assert!(has(&problems(&p, &t), "does not match where the output templates write"));
    // A template that climbs out of the root is refused by the same expansion the app uses.
    let mut t = s.clone();
    t.output.folder_template = "../escape/{output}".into();
    assert!(has(&problems(&p, &t), "output templates"), "{:?}", problems(&p, &t));
    // Read-only location (skipped when the mode is ignored, e.g. running as root).
    fs::remove_dir_all(p.b.join("out")).unwrap();
    fs::create_dir_all(p.b.join("out")).unwrap();
    fs::set_permissions(p.b.join("out"), fs::Permissions::from_mode(0o555)).unwrap();
    let writable = fs::write(p.b.join("out/.t"), b"x").is_ok();
    let r = s.preflight(&roots(&p.b), "j");
    fs::set_permissions(p.b.join("out"), fs::Permissions::from_mode(0o755)).unwrap();
    if !writable {
        assert!(has(&r.unwrap_err(), "cannot write"));
    }
    s.preflight(&roots(&p.b), "j").expect("writable again: accepted");
    let _ = fs::remove_dir_all(p.base);
}

// ------------------------------------------------------------------------------------------------ confinement

use std::os::unix::fs::symlink;

/// A directory outside every mapped location, with something in it that must never be touched or written beside.
fn outside(p: &Pcs) -> PathBuf {
    let d = p.base.join("outside");
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("secret.txt"), b"s").unwrap();
    d
}
fn entries(d: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(d).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    v.sort();
    v
}

#[test]
fn a_symlink_that_leaves_the_mapped_location_is_refused_for_sources_and_hints() {
    let p = pcs("sub-link-src");
    let s = spec(&p, "full", None);
    let clip = p.b.join("projects/Show/clips/shot_01");
    let out = outside(&p);
    // Frames directory is a link to frames outside the location: the loader sees an internal sequence.
    for i in 0..3 {
        fs::write(out.join(format!("f_{}.png", 1101 + i)), b"x").unwrap();
    }
    fs::rename(clip.join("Frames"), clip.join("Frames_real")).unwrap();
    symlink(&out, clip.join("Frames")).unwrap();
    let e = problems(&p, &s);
    assert!(has(&e, "leaves shared location \"projects\"") && has(&e, "outside"), "{e:?}");
    // The same link pointing INSIDE the location is fine.
    fs::remove_file(clip.join("Frames")).unwrap();
    symlink(clip.join("Frames_real"), clip.join("Frames")).unwrap();
    s.preflight(&roots(&p.b), "j").expect("an in-location symlink is allowed");
    // One symlinked hint frame that leaves the location.
    let h = clip.join("AlphaHint/f_1102.png");
    fs::remove_file(&h).unwrap();
    fs::write(out.join("hint.png"), b"x").unwrap();
    symlink(out.join("hint.png"), &h).unwrap();
    assert!(has(&problems(&p, &s), "f_1102.png"), "{:?}", problems(&p, &s));
    assert_eq!(entries(&out).len(), 4 + 1, "nothing was written beside the outside files");
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn an_output_symlink_that_leaves_its_own_location_is_refused_and_nothing_is_written_there() {
    let p = pcs("sub-link-out");
    let s = spec(&p, "none", None);
    let out = outside(&p);
    // out/Show -> outside
    symlink(&out, p.b.join("out/Show")).unwrap();
    let e = problems(&p, &s);
    assert!(has(&e, "leaves shared location \"out\""), "{e:?}");
    assert_eq!(entries(&out), vec!["secret.txt"], "no probe, no directory, nothing created outside");
    fs::remove_file(p.b.join("out/Show")).unwrap();
    // Inside ANOTHER mapped location is still outside THIS reference's location.
    fs::create_dir_all(p.b.join("projects/elsewhere")).unwrap();
    symlink(p.b.join("projects/elsewhere"), p.b.join("out/Show")).unwrap();
    assert!(has(&problems(&p, &s), "leaves shared location \"out\""));
    assert!(entries(&p.b.join("projects/elsewhere")).is_empty());
    fs::remove_file(p.b.join("out/Show")).unwrap();
    s.preflight(&roots(&p.b), "j").expect("clean again: accepted");
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn a_mapped_root_that_is_itself_an_alias_is_the_trust_anchor() {
    let p = pcs("sub-alias");
    let alias = p.base.join("pcB/alias");
    symlink(&p.b, &alias).unwrap();
    let s = spec(&p, "none", None);
    let pre = s.preflight(&roots(&alias), "j").expect("the canonical root is the anchor: a root reached through an alias works");
    assert!(pre.job.clip_root.starts_with(&alias));
    let _ = fs::remove_dir_all(p.base);
}

#[test]
fn keyframe_marks_must_match_this_pcs_clip_and_templates_are_checked_before_sending() {
    let p = pcs("sub-marks");
    let clip = p.b.join("projects/Show/clips/shot_01");
    let stems: Vec<String> = (0..3).map(|i| format!("f_{}", 1101 + i)).collect();
    ck::persist::save_keyframes(&clip, &[0, 1], &stems).unwrap();
    let e = problems(&p, &spec(&p, "keyframes", Some(vec![0, 2])));
    assert!(has(&e, "keyframe marks differ"), "{e:?}");
    ck::persist::save_keyframes(&clip, &[0, 2], &stems).unwrap();
    spec(&p, "keyframes", Some(vec![0, 2])).preflight(&roots(&p.b), "j").expect("same marks: accepted");
    // An empty mark list is refused here (the local job refuses it) before it is ever sent.
    let mut s = spec(&p, "keyframes", Some(vec![0, 2]));
    s.hint_keyframes_abs = Some(vec![]);
    assert!(s.check_shape().is_err());
    // A template the server refuses is refused by this build first, by the app's own expander.
    for (field, bad) in [("{bogus}/{output}", "folder"), ("{project}/{output}", "file-no-hashes")] {
        let mut t = spec(&p, "none", None);
        if bad == "folder" {
            t.output.folder_template = field.into();
        } else {
            t.output.file_template = "{name}_{output}".into();
        }
        assert!(t.check_shape().is_err(), "{bad}");
        assert!(python_says(&serde_json::to_value(envelope(&t)).unwrap()).is_err(), "the server refuses it too: {bad}");
    }
    // Models: names and digests are bounded like the server's.
    let mut t = spec(&p, "none", None);
    t.require.models.insert("m".repeat(65), None);
    assert!(t.check_shape().is_err());
    let _ = fs::remove_dir_all(p.base);
}
