//! Loader tests on temp projects generated at runtime (synthetic, empty-ish files). Includes a no-write check.
use ck::frames::*;
use ck::project::*;
use std::fs;
use std::path::{Path, PathBuf};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ck-loader-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}
fn touch(p: &Path) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, b"x").unwrap();
}
fn put(p: &Path, s: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, s).unwrap();
}
/// (relative path, len, mtime, bytes) for every file → proves nothing was written.
fn snapshot(root: &Path) -> Vec<(PathBuf, u64, std::time::SystemTime, Vec<u8>)> {
    fn walk(d: &Path, out: &mut Vec<PathBuf>) {
        for e in fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out)
            } else {
                out.push(p)
            }
        }
    }
    let mut files = vec![];
    walk(root, &mut files);
    files.sort();
    files
        .into_iter()
        .map(|p| {
            (
                p.clone(),
                fs::metadata(&p).unwrap().len(),
                fs::metadata(&p).unwrap().modified().unwrap(),
                fs::read(&p).unwrap(),
            )
        })
        .collect()
}

fn v2_project() -> PathBuf {
    let root = tmpdir("v2");
    put(
        &root.join("project.json"),
        r#"{"version":2,"display_name":"Demo","clips":["a","b"],"removed_clips":["gone"],"output_dir":"/nonexistent/out","future_key":{"x":1}}"#,
    );
    put(
        &root.join(".corridorkey_session.json"),
        r#"{"version":1,"params":{"hint_quality":0.7},"output_config":{"exr_compression":"dwab"},"clip_input_is_linear":{"a":true}}"#,
    );
    // Clip a: sequence with holes, natural-sort names, partial alpha, in/out.
    for n in [1, 2, 3, 10, 11] {
        touch(&root.join(format!("clips/a/Frames/f_{n}.exr")));
    }
    touch(&root.join("clips/a/Frames/notes.txt"));
    put(
        &root.join("clips/a/clip.json"),
        r#"{"source":{"original_path":"/x/a.mov","filename":"a.mov","copied":true},"in_out_range":{"in_point":1,"out_point":3}}"#,
    );
    touch(&root.join("clips/a/AlphaHint/f_2.png"));
    // Output dir comes from project.json output_dir/<clip> → nonexistent: outputs empty, not an error.
    // Clip b: custom output dir with a complete v2 manifest.
    for n in 0..3 {
        touch(&root.join(format!("clips/b/Frames/frame_{n:06}.png")));
        touch(&root.join(format!("custom_out/Matte/frame_{n:06}.exr")));
    }
    put(
        &root.join("clips/b/clip.json"),
        &format!(
            r#"{{"output_dir":"{}"}}"#,
            root.join("custom_out").display()
        ),
    );
    put(
        &root.join("custom_out/.corridorkey_manifest.json"),
        r#"{"version":2,"engine":"corridorkey-v2","enabled_outputs":["matte"],"formats":{"matte":"exr"},"source_stems":["frame_000000","frame_000001","frame_000002"],"frame_range":null,"complete":true}"#,
    );
    touch(&root.join("clips/_hidden/Frames/x.png"));
    touch(&root.join("clips/gone/Frames/x.png"));
    root
}

#[test]
fn v2_discovery_state_output_root_color_and_no_write() {
    let root = v2_project();
    let before = snapshot(&root);
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    assert_eq!(p.display_name, "Demo");
    assert!(p.is_v2);
    assert_eq!(
        p.clips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    ); // _hidden / removed skipped
    let (a, b) = (&p.clips[0], &p.clips[1]);
    // natural sort + non-image sidecar ignored
    assert_eq!(
        a.input_files
            .iter()
            .map(|f| f.0.as_str())
            .collect::<Vec<_>>(),
        ["f_1", "f_2", "f_3", "f_10", "f_11"]
    );
    // inclusive range 1..=3 → 3 frames; alpha covers 1 of 3 → partial → Raw, not Ready
    assert_eq!(a.range_len(), 3);
    assert_eq!(a.state, ClipState::Raw);
    assert!(readiness(a).contains("Partial hint: 1 of 3"));
    // session colour override wins even for EXR; clip b has none: PNG → not linear
    assert!(a.input_is_linear && a.color_origin == ColorOrigin::SessionOverride);
    assert!(!b.input_is_linear);
    // output roots
    assert_eq!(
        a.output_dir.as_deref(),
        Some(Path::new("/nonexistent/out/a"))
    );
    assert_eq!(
        b.output_dir.as_deref(),
        Some(root.join("custom_out").as_path())
    );
    assert_eq!(b.state, ClipState::Complete);
    assert_eq!(b.completed_stems().len(), 3);
    // sparse hint view keeps its hole on the shared timeline
    let idx = FrameIndex::build(a);
    assert_eq!(idx.view_frames(View::Hint), 1);
    assert!(matches!(
        idx.availability(View::Hint, 0, None),
        Availability::NoFile
    ));
    assert!(matches!(
        idx.availability(View::Hint, 1, None),
        Availability::Present(_)
    ));
    assert_eq!(clip_range_to_timeline(a, &idx), Some((1, 3)));
    assert_eq!(p.session_params.as_ref().unwrap()["hint_quality"], 0.7);
    // Exercise the frame index too, then prove byte/mtime preservation.
    let _ = FrameIndex::build(b);
    assert_eq!(
        snapshot(&root),
        before,
        "loader must not modify or create files"
    );
}

#[test]
fn global_output_dir_used_only_when_no_override() {
    let root = tmpdir("glob");
    put(&root.join("project.json"), r#"{"version":2,"clips":["c"]}"#);
    touch(&root.join("clips/c/Frames/f_1.png"));
    let p = load_project(
        &root,
        &LoadOptions {
            global_output_dir: Some("/g".into()),
        },
    )
    .unwrap();
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        p.clips[0].output_dir.as_deref(),
        Some(Path::new(&format!("/g/{name}/c")))
    );
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    assert_eq!(
        p.clips[0].output_dir.as_deref(),
        Some(root.join("clips/c/Output").as_path())
    );
    assert!(p.clips[0].output_dir_note.contains("not read"));
}

#[test]
fn v1_project_root_is_the_clip() {
    let root = tmpdir("v1");
    put(
        &root.join("project.json"),
        r#"{"source":{"type":"sequence","original_path":"/none"},"display_name":"One"}"#,
    );
    touch(&root.join("Input/f_1.png"));
    touch(&root.join("Input/f_2.png"));
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    assert!(!p.is_v2);
    assert_eq!(p.clips.len(), 1);
    assert_eq!(p.clips[0].input_files.len(), 2);
    assert_eq!(p.clips[0].display_name, "One");
}

#[test]
fn external_sequence_is_referenced_not_copied() {
    let root = tmpdir("ext");
    let ext = tmpdir("ext-src");
    for n in 1..=3 {
        touch(&ext.join(format!("s{n}.exr")));
    }
    put(&root.join("project.json"), r#"{"version":2,"clips":["c"]}"#);
    put(
        &root.join("clips/c/clip.json"),
        &format!(
            r#"{{"source":{{"type":"sequence","original_path":"{}"}}}}"#,
            ext.display()
        ),
    );
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    assert_eq!(
        c.source,
        Source::Sequence {
            dir: ext.clone(),
            external: true
        }
    );
    assert_eq!(c.input_files.len(), 3);
    assert!(
        c.input_is_linear,
        "standalone EXR sequence defaults to linear"
    );
    assert_eq!(c.color_origin, ColorOrigin::DefaultLinearExr);
    assert!(!root.join("clips/c/Frames").exists());
}

#[test]
fn video_source_without_frames_is_extracting_not_viewable() {
    let root = tmpdir("vid");
    put(&root.join("project.json"), r#"{"version":2,"clips":["v"]}"#);
    touch(&root.join("clips/v/Source/shot.mov"));
    put(
        &root.join("clips/v/clip.json"),
        r#"{"source":{"filename":"shot.mov","copied":true}}"#,
    );
    let c = &load_project(&root, &LoadOptions::default()).unwrap().clips[0];
    assert!(matches!(c.source, Source::Video { .. }));
    assert_eq!(c.state, ClipState::Extracting);
    assert!(readiness(c).contains("not integrated"));
    assert!(c.fps_assumed);
}

#[test]
fn corrupt_and_partial_metadata_never_panics() {
    let root = tmpdir("bad");
    put(&root.join("project.json"), r#"{"version":2,"clips":["c"]}"#);
    touch(&root.join("clips/c/Frames/f_1.png"));
    put(&root.join("clips/c/clip.json"), "{not json");
    put(
        &root.join("clips/c/.corridorkey_hint_keyframes.json"),
        r#"{"version":1,"keyframes":[],"extra":1}"#,
    );
    put(
        &root.join("clips/c/Output/.corridorkey_manifest.json"),
        "garbage",
    );
    let c = &load_project(&root, &LoadOptions::default()).unwrap().clips[0];
    assert!(
        c.notes
            .iter()
            .any(|n| n.contains("clip metadata unreadable")),
        "{:?}",
        c.notes
    );
    assert!(c.notes.iter().any(|n| n.contains("keyframe sidecar")));
    assert!(c.notes.iter().any(|n| n.contains("manifest unreadable")));
    assert_eq!(c.input_files.len(), 1);
}

#[test]
fn partial_manifest_range_is_not_complete() {
    let root = tmpdir("part");
    put(&root.join("project.json"), r#"{"version":2,"clips":["c"]}"#);
    for n in 0..2 {
        touch(&root.join(format!("clips/c/Frames/f_{n}.png")));
        touch(&root.join(format!("clips/c/Output/Matte/f_{n}.exr")));
    }
    put(
        &root.join("clips/c/Output/.corridorkey_manifest.json"),
        r#"{"engine":"corridorkey-v2","enabled_outputs":["matte"],"formats":{"matte":"exr"},"frame_range":[0,0],"complete":true}"#,
    );
    assert_ne!(
        load_project(&root, &LoadOptions::default()).unwrap().clips[0].state,
        ClipState::Complete
    );
}

fn stems(n: usize) -> Vec<String> {
    (0..n).map(|i| format!("f_{i:03}")).collect()
}

#[test]
fn keyframe_sidecar_is_strict_like_python() {
    let st = stems(5);
    let ok = |j: &str| parse_keyframes(&serde_json::from_str(j).unwrap(), &st);
    assert_eq!(
        ok(r#"{"version":1,"keyframes":[{"index":1,"stem":"f_001"},{"index":3,"stem":"f_003"}]}"#),
        Ok(vec![1, 3])
    );
    for (bad, why) in [
        (r#"{"version":2,"keyframes":[]}"#, "version"),
        (r#"{"version":1,"keyframes":[],"x":1}"#, "schema"),
        (r#"{"version":1.0,"keyframes":[]}"#, "version"),
        (
            r#"{"version":1,"keyframes":[{"index":1}]}"#,
            "Invalid keyframe record",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":1,"stem":"f_001","x":0}]}"#,
            "Invalid keyframe record",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":1.0,"stem":"f_001"}]}"#,
            "types",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":true,"stem":"f_001"}]}"#,
            "types",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":1,"stem":5}]}"#,
            "types",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":5,"stem":"f_005"}]}"#,
            "out of range",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":-1,"stem":"f_000"}]}"#,
            "out of range",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":1,"stem":"f_001"},{"index":2,"stem":"f_001"}]}"#,
            "Duplicate keyframe stem",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":1,"stem":"WRONG"}]}"#,
            "Stale keyframe 1",
        ),
        (
            r#"{"version":1,"keyframes":[{"index":3,"stem":"f_003"},{"index":1,"stem":"f_001"}]}"#,
            "not sorted",
        ),
    ] {
        let e = ok(bad).unwrap_err();
        assert!(e.contains(why), "{bad} → {e} (wanted {why})");
    }
}

#[test]
fn stale_keyframe_sidecar_is_reported_and_ignored_on_load() {
    let root = tmpdir("kfstale");
    put(&root.join("project.json"), r#"{"version":2,"clips":["c"]}"#);
    touch(&root.join("clips/c/Frames/f_001.png"));
    touch(&root.join("clips/c/Frames/f_002.png"));
    put(
        &root.join("clips/c/.corridorkey_hint_keyframes.json"),
        r#"{"version":1,"keyframes":[{"index":1,"stem":"WRONG"}]}"#,
    );
    let c = &load_project(&root, &LoadOptions::default()).unwrap().clips[0];
    assert!(c.keyframes.is_empty());
    assert!(
        c.notes.iter().any(|n| n.contains("Stale keyframe 1")),
        "{:?}",
        c.notes
    );
}

#[test]
fn display_name_is_the_output_and_colour_identity_but_folder_stays_stable() {
    let root = tmpdir("disp");
    put(
        &root.join("project.json"),
        r#"{"version":2,"clips":["a_disp"],"output_dir":"/tmp/ckrev/out"}"#,
    );
    put(
        &root.join(".corridorkey_session.json"),
        r#"{"clip_input_is_linear":{"Hero Shot":true,"a_disp":false}}"#,
    );
    touch(&root.join("clips/a_disp/Frames/f_1.png"));
    put(
        &root.join("clips/a_disp/clip.json"),
        r#"{"display_name":"Hero Shot"}"#,
    );
    let c = &load_project(&root, &LoadOptions::default()).unwrap().clips[0];
    assert_eq!(
        (c.folder.as_str(), c.name.as_str()),
        ("a_disp", "Hero Shot")
    );
    assert_eq!(
        c.output_dir.as_deref(),
        Some(Path::new("/tmp/ckrev/out/Hero Shot"))
    );
    assert!(c.input_is_linear && c.color_origin == ColorOrigin::SessionOverride);
    let g = load_project(
        &root,
        &LoadOptions {
            global_output_dir: Some("/g".into()),
        },
    )
    .unwrap();
    // project.json output_dir still wins over the global dir; remove it to see the global join.
    assert_eq!(
        g.clips[0].output_dir.as_deref(),
        Some(Path::new("/tmp/ckrev/out/Hero Shot"))
    );
    put(
        &root.join("project.json"),
        r#"{"version":2,"clips":["a_disp"]}"#,
    );
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    let g = load_project(
        &root,
        &LoadOptions {
            global_output_dir: Some("/g".into()),
        },
    )
    .unwrap();
    assert_eq!(
        g.clips[0].output_dir.as_deref(),
        Some(Path::new(&format!("/g/{name}/Hero Shot")))
    );
}

#[test]
fn v1_global_output_is_global_project_name() {
    let root = tmpdir("v1g");
    put(&root.join("project.json"), r#"{"display_name":"One"}"#);
    touch(&root.join("Input/f_1.png"));
    let name = root.file_name().unwrap().to_string_lossy().into_owned();
    let c = &load_project(
        &root,
        &LoadOptions {
            global_output_dir: Some("/g".into()),
        },
    )
    .unwrap()
    .clips[0];
    assert_eq!(
        c.output_dir.as_deref(),
        Some(Path::new(&format!("/g/{name}/One")))
    );
}

#[test]
fn missing_external_source_is_error_state_and_sourceless_clips_are_not_listed() {
    let root = tmpdir("err");
    put(
        &root.join("project.json"),
        r#"{"version":2,"clips":["d_ext","c_empty","e_emptyinput","ok"]}"#,
    );
    put(
        &root.join("clips/d_ext/clip.json"),
        r#"{"source":{"type":"sequence","original_path":"/definitely/not/here"},"display_name":"Renamed"}"#,
    );
    fs::create_dir_all(root.join("clips/c_empty")).unwrap();
    fs::create_dir_all(root.join("clips/e_emptyinput/Input")).unwrap();
    touch(&root.join("clips/ok/Frames/f_1.png"));
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    let names: Vec<_> = p.clips.iter().map(|c| c.folder.as_str()).collect();
    assert_eq!(names, ["d_ext", "ok"]);
    let e = &p.clips[0];
    assert_eq!(e.state, ClipState::Error);
    assert!(readiness(e).contains("Source folder missing: /definitely/not/here"));
    assert_eq!(
        e.name, "d_ext",
        "Python keeps the folder name for the early ERROR return"
    );
    assert_eq!(
        p.notes
            .iter()
            .filter(|n| n.contains("Clip not listed"))
            .count(),
        2,
        "{:?}",
        p.notes
    );
}

#[test]
fn root_input_video_and_empty_source_dir_with_original_file() {
    let root = tmpdir("vid2");
    put(
        &root.join("project.json"),
        r#"{"version":2,"clips":["a","b"]}"#,
    );
    touch(&root.join("clips/a/Input.mov"));
    fs::create_dir_all(root.join("clips/b/Source")).unwrap();
    let orig = root.join("orig.mp4");
    touch(&orig);
    put(
        &root.join("clips/b/clip.json"),
        &format!(r#"{{"source":{{"original_path":"{}"}}}}"#, orig.display()),
    );
    let p = load_project(&root, &LoadOptions::default()).unwrap();
    assert!(matches!(&p.clips[0].source, Source::Video { path } if path.ends_with("Input.mov")));
    assert!(matches!(&p.clips[1].source, Source::Video { path } if *path == orig));
}
