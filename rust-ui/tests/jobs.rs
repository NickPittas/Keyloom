mod common;
use ck::contracts::{ClipOutputValues, OutputKind, OutputLocation};
use ck::jobs::*;
use common::{job, tmp};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

fn j(n: usize) -> KeyJob {
    job(Path::new("/mnt/NAS/out"), "a", n, "ok")
}
fn bad(f: impl FnOnce(&mut KeyJob), field: &str) {
    let mut x = j(2);
    f(&mut x);
    assert_eq!(x.validate().unwrap_err().field, field);
}

#[test]
fn valid_job_round_trips_as_json_value() {
    let x = j(2);
    x.validate().unwrap();
    let v = serde_json::to_value(&x).unwrap();
    assert_eq!(v["outputs"]["matte"][1], "/mnt/NAS/out/a/clip_1102.png");
    assert_eq!(v["hint_keyframes_abs"], Value::Null);
    assert_eq!(serde_json::from_value::<KeyJob>(v.clone()).unwrap(), x);
    // Unknown keys are refused, not ignored.
    let mut v = v;
    v["extra"] = json!(1);
    assert!(serde_json::from_value::<KeyJob>(v).is_err());
}

#[test]
fn validation_rejects_bad_jobs() {
    bad(|x| x.job_id = "".into(), "job_id");
    bad(|x| x.range.in_point = 5, "range");
    bad(|x| x.params = json!([]), "params");
    bad(
        |x| x.hint_keyframes_abs = Some(vec![7]),
        "hint_keyframes_abs",
    );
    bad(|x| x.publish = "staging".into(), "publish");
    bad(|x| x.require.protocol = 2, "require");
    bad(|x| x.outputs.clear(), "outputs");
    bad(
        |x| {
            x.outputs.get_mut("matte").unwrap().pop();
        },
        "outputs",
    ); // length != frames
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "rel/x.png".into(),
        "outputs",
    );
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "/mnt/NAS/out/a/../b/x.png".into(),
        "outputs",
    );
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "/mnt/NAS/out/a/x.jpg".into(),
        "outputs",
    );
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "/mnt/NAS/other/x.png".into(),
        "outputs",
    );
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[1] = "/mnt/NAS/out/a/CLIP_1101.png".into(),
        "outputs",
    ); // dup, case-insens
    bad(
        |x| {
            let v = x.outputs.remove("matte").unwrap();
            x.outputs.insert("bogus".into(), v);
        },
        "outputs",
    );
    bad(|x| x.manifest_dir = "/mnt/NAS".into(), "manifest_dir"); // swallows the source clip
}

#[test]
fn windows_style_paths_validate_without_unix_assumptions() {
    let mut x = j(1);
    x.clip_root = r"D:\clips\a".into();
    x.manifest_dir = r"D:\out\a".into();
    x.outputs
        .insert("matte".into(), vec![r"D:\out\a\clip_1101.png".into()]);
    x.validate().unwrap();
    x.clip_root = r"\\nas\share\clips\a".into(); // UNC
    x.manifest_dir = r"\\nas\share\out\a".into();
    x.outputs.insert(
        "matte".into(),
        vec![r"\\nas\share\out\a\clip_1101.png".into()],
    );
    x.validate().unwrap();
}

#[test]
fn conflicts_are_nested_or_equal_manifest_folders() {
    let (a, mut b) = (j(2), j(2));
    b.job_id = "b".into();
    assert!(a.conflicts_with(&b)); // same folder
    let at = |d: &str| {
        let mut x = j(1);
        x.manifest_dir = d.into();
        x.outputs
            .insert("matte".into(), vec![PathBuf::from(d).join("f_1101.png")]);
        x
    };
    assert!(at("/mnt/NAS/out/a/FG").conflicts_with(&a)); // nested
    assert!(a.conflicts_with(&at("/MNT/nas/OUT/A"))); // case-insensitive, conservative
    assert!(!at("/mnt/NAS/out/ab").conflicts_with(&a)); // component boundary
    assert!(!at("/mnt/NAS/out/b").conflicts_with(&a));
}

#[test]
fn path_map_longest_prefix_whole_components_and_unmappable() {
    let x = j(2);
    let map = |p: &[(&str, &str)]| {
        p.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect::<Vec<_>>()
    };
    let m = x
        .mapped(&map(&[
            ("/mnt/NAS", "/srv/nas"),
            ("/mnt/NAS/out", "/data/render/"),
        ]))
        .unwrap();
    assert_eq!(m.manifest_dir, Path::new("/data/render/a"));
    assert_eq!(
        m.outputs["matte"][1],
        Path::new("/data/render/a/clip_1102.png")
    );
    assert!(m.path_map.is_empty()); // the wire job carries no map once paths are mapped
    assert_eq!(x.job_id, m.job_id); // original untouched
                                    // /mnt/NAS2 is not covered by /mnt/NAS.
    assert!(x
        .mapped(&map(&[("/mnt/NAS2", "/x")]))
        .unwrap_err()
        .contains("no path_map entry"));
    // clip_root /mnt/NAS/out/a_clip is covered, so a map for only a sub-folder leaves it unmapped.
    assert!(x.mapped(&map(&[("/mnt/NAS/out/a", "/x")])).is_err());
    assert!(x.mapped(&[]).is_err());
    // Mapping that splits outputs from the manifest folder is caught by re-validation.
    let e = x.mapped(&map(&[
        ("/mnt/NAS/out/a_clip", "/c"),
        ("/mnt/NAS/out/a", "/y"),
        ("/mnt/NAS/out", "/z"),
    ]));
    assert!(e.is_ok());
}

#[test]
fn resolves_templated_and_legacy_outputs_with_source_frame_numbers() {
    let loc = OutputLocation {
        root: "/mnt/NAS/Masks".into(),
        folder_template: "{name}_{version}/{output}".into(),
        file_template: "{name}_####".into(),
    };
    let v = ClipOutputValues {
        project: "P".into(),
        name: "clip".into(),
        resolution: "2K".into(),
        date: "d".into(),
        version: 1,
    };
    let stems: Vec<String> = ["clip_1101", "clip_1102"].map(String::from).into();
    let kinds = [(OutputKind::Matte, "exr"), (OutputKind::Fg, "exr")];
    let o = resolve_outputs(&loc, &v, &kinds, &stems).unwrap();
    assert_eq!(
        o["matte"][1],
        Path::new("/mnt/NAS/Masks/clip_v001/Matte/clip_1102.exr")
    );
    assert_eq!(
        o["fg"][0],
        Path::new("/mnt/NAS/Masks/clip_v001/FG/clip_1101.exr")
    );
    assert_eq!(
        common_dir(&o).unwrap(),
        Path::new("/mnt/NAS/Masks/clip_v001")
    );
    // Missing {output} with two outputs is the contract's collision error.
    let bad_loc = OutputLocation {
        folder_template: "{name}".into(),
        ..loc.clone()
    };
    assert!(matches!(
        resolve_outputs(&bad_loc, &v, &kinds, &stems),
        Err(ResolveError::Location(_))
    ));
    assert_eq!(
        resolve_outputs(&loc, &v, &kinds, &["nodigits".to_string()]).unwrap_err(),
        ResolveError::NoFrameNumber("nodigits".into())
    );
    let l = legacy_outputs(
        Path::new("/p/clip/Output"),
        &[(OutputKind::Comp, "png")],
        &stems,
    );
    assert_eq!(l["comp"][0], Path::new("/p/clip/Output/Comp/clip_1101.png"));
    assert_eq!(stem_frame_number("a_0042"), Some(42));
    assert_eq!(stem_frame_number("a"), None);
}

#[cfg(unix)]
#[test]
fn disk_check_rejects_symlink_escape_and_source_overlap() {
    let base = tmp("disk");
    let mut x = job(&base, "a", 2, "ok");
    x.check_disk().unwrap(); // nothing exists yet
                             // An output sub-folder that is a symlink pointing outside the manifest folder.
    let outside = base.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::create_dir_all(&x.manifest_dir).unwrap();
    std::os::unix::fs::symlink(&outside, x.manifest_dir.join("FG")).unwrap();
    x.outputs.insert(
        "matte".into(),
        vec![
            x.manifest_dir.join("FG/clip_1101.png"),
            x.manifest_dir.join("clip_1102.png"),
        ],
    );
    let e = x.check_disk().unwrap_err();
    assert_eq!(e.field, "outputs");
    // Manifest folder that is a symlink to a folder containing the source clip.
    let mut y = job(&base, "b", 1, "ok");
    std::os::unix::fs::symlink(&base, base.join("link")).unwrap();
    y.manifest_dir = base.join("link");
    y.outputs
        .insert("matte".into(), vec![base.join("link/f_1101.png")]);
    assert_eq!(y.check_disk().unwrap_err().field, "manifest_dir");
    std::fs::remove_dir_all(&base).unwrap();
}

fn cfg(matte_fmt: &str, fg: bool) -> Value {
    json!({"fg_enabled": fg, "fg_format": "exr", "matte_enabled": true, "matte_format": matte_fmt,
           "comp_enabled": false, "comp_format": "png", "processed_enabled": false, "processed_format": "exr",
           "exr_compression": "dwab"})
}

#[test]
fn validation_mirrors_the_python_worker() {
    bad(|x| x.job_id = "a b".into(), "job_id");
    bad(|x| x.job_id = "x".repeat(65), "job_id");
    bad(|x| x.job_id = "é".into(), "job_id");
    bad(|x| x.clip_id = "c".repeat(513), "clip_id");
    // normalised absolute paths only
    for p in [
        "/mnt/NAS//out/a",
        "/mnt/NAS/out/./a",
        "/mnt/NAS/out/a/",
        "/mnt/NAS/out\\a",
    ] {
        bad(|x| x.manifest_dir = p.into(), "manifest_dir");
    }
    bad(|x| x.clip_root = "/mnt/NAS/x/../y".into(), "clip_root");
    // enabled output names and extensions come from output_config
    bad(|x| x.output_config = cfg("exr", false), "outputs"); // matte path is .png, config says .exr
    bad(|x| x.output_config = cfg("png", true), "outputs"); // fg enabled but no fg paths
    bad(
        |x| x.output_config["matte_enabled"] = json!(false),
        "output_config",
    ); // nothing enabled
    bad(
        |x| x.output_config["matte_format"] = json!("tiff"),
        "output_config",
    );
    bad(
        |x| x.output_config["exr_compression"] = json!("lzw"),
        "output_config",
    );
    bad(
        |x| {
            x.output_config
                .as_object_mut()
                .unwrap()
                .remove("exr_compression");
        },
        "output_config",
    );
    bad(|x| x.output_config["extra"] = json!(1), "output_config");
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "/mnt/NAS/out/a/clip_1101.PNG".into(),
        "outputs",
    ); // exact case
    bad(
        |x| x.outputs.get_mut("matte").unwrap()[0] = "/mnt/NAS/out/a/.hidden.png".into(),
        "outputs",
    );
    // hint rules
    bad(|x| x.params["hint_mode"] = json!("sometimes"), "params");
    bad(
        |x| x.hint_keyframes_abs = Some(vec![1]),
        "hint_keyframes_abs",
    ); // marks without keyframes mode
    bad(
        |x| x.params["hint_mode"] = json!("keyframes"),
        "hint_keyframes_abs",
    ); // keyframes mode without marks
    bad(
        |x| {
            x.params["hint_mode"] = json!("keyframes");
            x.hint_keyframes_abs = Some(vec![]);
        },
        "hint_keyframes_abs",
    );
    bad(
        |x| {
            x.params["hint_mode"] = json!("keyframes");
            x.hint_keyframes_abs = Some(vec![1, 0]);
        },
        "hint_keyframes_abs",
    );
    bad(
        |x| {
            x.params["hint_mode"] = json!("keyframes");
            x.hint_keyframes_abs = Some(vec![0, 0]);
        },
        "hint_keyframes_abs",
    );
    bad(
        |x| {
            x.params["hint_mode"] = json!("keyframes");
            x.hint_keyframes_abs = Some(vec![5]);
        },
        "hint_keyframes_abs",
    );
    let mut ok = j(2);
    ok.params["hint_mode"] = json!("keyframes");
    ok.hint_keyframes_abs = Some(vec![0, 1]);
    ok.validate().unwrap();
    // a submitted job never carries a map
    bad(
        |x| x.path_map = vec![("/a".into(), "/b".into())],
        "path_map",
    );
    // several outputs: exactly the enabled set
    let mut two = j(1);
    two.output_config = cfg("png", true);
    two.outputs
        .insert("fg".into(), vec!["/mnt/NAS/out/a/fg_1101.exr".into()]);
    two.validate().unwrap();
}

#[test]
fn remapped_wire_job_equals_original_except_paths_and_empty_map() {
    let x = j(3);
    let m = x
        .mapped(&[("/mnt/NAS".to_string(), r"Z:\Shared".to_string())])
        .unwrap();
    assert!(m.path_map.is_empty());
    assert_eq!(m.manifest_dir, Path::new(r"Z:\Shared\out\a"));
    assert_eq!(
        m.outputs["matte"][2],
        Path::new(r"Z:\Shared\out\a\clip_1103.png")
    );
    // Everything but the paths is untouched.
    let (mut a, mut b) = (
        serde_json::to_value(&x).unwrap(),
        serde_json::to_value(&m).unwrap(),
    );
    for k in ["clip_root", "manifest_dir", "outputs"] {
        a[k] = Value::Null;
        b[k] = Value::Null;
    }
    assert_eq!(a, b);
    // UNC target root.
    let u = x
        .mapped(&[("/mnt/NAS".to_string(), r"\\nas\share".to_string())])
        .unwrap();
    assert_eq!(u.manifest_dir, Path::new(r"\\nas\share\out\a"));
}

#[test]
fn sha1_matches_known_vectors_and_the_backends_tmp_tags() {
    assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
    assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    // hashlib.sha1(path.encode()).hexdigest()[:12], computed with the backend's formula (`_tmp_path`).
    for (p, tag) in [
        ("/mnt/NAS/out/a/clip_1101.png", "974304f0bdd7"),
        ("/mnt/NAS/out/a/clip_1102.png", "c4d27c044afd"),
        (r"Z:\Shared\out\a\clip_1101.png", "2b1c23f0da88"),
    ] {
        assert_eq!(&sha1_hex(p.as_bytes())[..12], tag, "{p}");
    }
}

#[cfg(unix)]
#[test]
fn recovery_removes_only_this_jobs_exact_backend_lock_and_temps() {
    use std::fs;
    let base = tmp("recover");
    let x = job(&base, "job-AAAAAAAA-1", 2, "ok");
    fs::create_dir_all(&x.manifest_dir).unwrap();
    let (d, id) = (&x.manifest_dir, &x.job_id);
    let w = |n: &str, c: &str| fs::write(d.join(n), c).unwrap();
    let tag =
        |i: usize| sha1_hex(x.outputs["matte"][i].to_str().unwrap().as_bytes())[..12].to_string();
    w(".corridorkey_job.lock", &json!({"job_id": id}).to_string());
    // This job's temps, real backend names.
    let own = [
        format!(".ck-{id}-{}.png", tag(0)),
        format!(".ck-{id}-{}.png", tag(1)),
        format!(".ck-probe-{id}-a"),
        format!(".ck-probe-{id}-b"),
        format!(".corridorkey_manifest.json.tmp.{id}"),
        format!("corridorkey_v2_settings.txt.tmp.{id}"),
    ];
    own.iter().for_each(|n| w(n, "t"));
    // Look-alikes that must survive: another job whose id starts with ours, shorter ids, other tags, outputs.
    let other = format!("{id}-2"); // "job-AAAAAAAA-1-2": id of a different job beginning with our full id
    let keep = [
        format!(".ck-{other}-{}.png", tag(0)),
        format!(".ck-probe-{other}-a"),
        format!(".corridorkey_manifest.json.tmp.{other}"),
        format!(".ck-job-AAAAAAAA-{}.png", tag(0)),
        format!(".ck-{id}-000000000000.png"), // our id, a tag that is not one of our outputs
        format!(".ck-{id}-{}.exr", tag(0)),   // wrong extension
        "clip_1101.png".into(),
        ".corridorkey_manifest.json".into(),
        format!(".clip_1101.tmp.{id}.png"), // old naming: not the backend's, not touched
    ];
    keep.iter().for_each(|n| w(n, "keep"));
    let r = x.recover_writer_gone();
    assert!(r.lock_removed && r.errors.is_empty(), "{r:?}");
    assert_eq!(r.temps_removed.len(), own.len(), "{r:?}");
    for n in &own {
        assert!(!d.join(n).exists(), "{n} should be gone");
    }
    for n in &keep {
        assert!(d.join(n).exists(), "{n} must stay");
    }
    // Someone else's lease (a different job id, even one beginning with ours) is never removed.
    w(
        ".corridorkey_job.lock",
        &json!({"job_id": other}).to_string(),
    );
    let r = x.recover_writer_gone();
    assert!(!r.lock_removed && r.lock_left.is_some() && d.join(".corridorkey_job.lock").exists());
    // Unreadable lock and a symlinked lock: left alone.
    w(".corridorkey_job.lock", "garbage");
    assert!(!x.recover_writer_gone().lock_removed);
    fs::remove_file(d.join(".corridorkey_job.lock")).unwrap();
    std::os::unix::fs::symlink(base.join("elsewhere"), d.join(".corridorkey_job.lock")).unwrap();
    let r = x.recover_writer_gone();
    assert!(!r.lock_removed && r.lock_left.is_some());
    assert_eq!(x.manifest_path(), d.join(".corridorkey_manifest.json"));
    fs::remove_dir_all(&base).unwrap();
}

#[cfg(unix)]
#[test]
fn recovery_of_a_remapped_job_hashes_the_workers_paths_but_cleans_local_files() {
    let base = tmp("recmap");
    let x = job(&base, "rj-1", 1, "ok");
    let wire = x
        .mapped(&[(base.to_str().unwrap().to_string(), "/srv/nas".to_string())])
        .unwrap();
    std::fs::create_dir_all(&x.manifest_dir).unwrap();
    let theirs = format!(
        ".ck-rj-1-{}.png",
        &sha1_hex(wire.outputs["matte"][0].to_str().unwrap().as_bytes())[..12]
    );
    let ours = format!(
        ".ck-rj-1-{}.png",
        &sha1_hex(x.outputs["matte"][0].to_str().unwrap().as_bytes())[..12]
    );
    std::fs::write(x.manifest_dir.join(&theirs), "t").unwrap();
    std::fs::write(
        x.manifest_dir.join(&ours),
        "not what the worker would have written",
    )
    .unwrap();
    let r = x.recover_as_sent(&wire);
    assert_eq!(r.temps_removed, vec![x.manifest_dir.join(&theirs)]);
    assert!(x.manifest_dir.join(&ours).exists());
    std::fs::remove_dir_all(&base).unwrap();
}

#[cfg(unix)]
#[test]
fn recovery_accepts_the_backend_lock_schema() {
    // tests/fixtures/worker_protocol/lock.json: schema 1 lock with host/platform/pid/boot_id fields.
    let fx: Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../tests/fixtures/worker_protocol/lock.json"
        ))
        .unwrap(),
    )
    .unwrap();
    for k in fx["required_keys"].as_array().unwrap() {
        assert!(fx["example"].get(k.as_str().unwrap()).is_some(), "{k}");
    }
    let base = tmp("lockschema");
    let mut x = job(&base, "job-key-new", 1, "ok");
    x.clip_id = fx["example"]["clip_id"].as_str().unwrap().into();
    std::fs::create_dir_all(&x.manifest_dir).unwrap();
    std::fs::write(
        x.manifest_dir.join(".corridorkey_job.lock"),
        fx["example"].to_string(),
    )
    .unwrap();
    assert!(x.recover_writer_gone().lock_removed);
    std::fs::remove_dir_all(&base).unwrap();
}

#[test]
fn tag_uses_the_windows_normalised_path_like_ntpath() {
    assert_eq!(
        worker_path_form("Z:/Shared/out/a/clip_1101.png"),
        r"Z:\Shared\out\a\clip_1101.png"
    );
    assert_eq!(
        worker_path_form(r"Z:\Shared\out\a\clip_1101.png"),
        r"Z:\Shared\out\a\clip_1101.png"
    );
    assert_eq!(
        worker_path_form("//nas/share/out/clip_1101.png"),
        r"\\nas\share\out\clip_1101.png"
    );
    assert_eq!(
        worker_path_form(r"\\nas\share\out\clip_1101.png"),
        r"\\nas\share\out\clip_1101.png"
    );
    assert_eq!(
        worker_path_form("/mnt/NAS/out/a/c.png"),
        "/mnt/NAS/out/a/c.png"
    ); // POSIX untouched
       // hashlib.sha1(r"Z:\Shared\out\a\clip_1101.png".encode()).hexdigest()[:12] == "2b1c23f0da88" for every spelling
    let base = tmp("ntnorm");
    let x = job(&base, "w-1", 1, "ok");
    for spelling in [
        "Z:/Shared/out/a/clip_1101.png",
        r"Z:\Shared\out\a\clip_1101.png",
    ] {
        let mut wire = x.clone();
        wire.outputs.insert("matte".into(), vec![spelling.into()]);
        let names: Vec<_> = x
            .owned_temps(&wire)
            .iter()
            .filter_map(|p| p.file_name()?.to_str().map(String::from))
            .collect();
        assert!(
            names.contains(&".ck-w-1-2b1c23f0da88.png".to_string()),
            "{spelling}: {names:?}"
        );
    }
    std::fs::remove_dir_all(base).unwrap();
}

/// Every name the Rust side would delete must satisfy the backend's authoritative `temp_names.json` patterns
/// and carry exactly this job's id (so job `abc` can never match job `abc-2`). Matching runs in Python's `re`
/// (inside the isolation wrapper), against the shared fixture itself.
#[test]
fn owned_temps_match_the_shared_backend_fixture_and_never_match_a_prefix_job() {
    let base = tmp("fixture");
    let mut x = job(&base, "abc", 3, "ok");
    x.output_config["fg_enabled"] = json!(true);
    x.outputs.insert(
        "fg".into(),
        (0..3)
            .map(|i| x.manifest_dir.join(format!("FG/clip_{}.exr", 1101 + i)))
            .collect(),
    );
    let mut names: Vec<String> = x
        .owned_temps(&x)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 3 + 3 + 2 + 2, "3 matte + 3 fg tags, manifest/settings tmp, probe a/b (same names per folder, deduped): {names:?}");
    let script = r#"
import json, re, sys
fx = json.load(open(sys.argv[1]))
names, job_id = json.load(sys.stdin)
pats = {k: re.compile(v) for k, v in fx["patterns"].items()}
for n in names:
    hits = [(k, m) for k, p in pats.items() if (m := p.match(n))]
    assert hits, "no fixture pattern matches " + n
    assert hits[0][1].group("job_id") == job_id, n
    # a different job whose id merely starts with ours has a distinct, non-matching name
    assert n.replace(job_id, job_id + "-2", 1) != n
assert all(isinstance(v, str) for v in fx["fixed_names"].values())
print("ok", len(names))
"#;
    let fixture = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/fixtures/worker_protocol/temp_names.json"
    );
    let mut child = std::process::Command::new("/usr/bin/python3")
        .args(["-I", "-c", script, fixture])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    use std::io::Write;
    child
        .stdin
        .take()
        .unwrap()
        .write_all(json!([names, "abc"]).to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // And the cleanup itself is exact: sibling job "abc-2" files (same prefix) survive.
    std::fs::create_dir_all(&x.manifest_dir).unwrap();
    let tag = &sha1_hex(x.outputs["matte"][0].to_str().unwrap().as_bytes())[..12];
    std::fs::write(x.manifest_dir.join(format!(".ck-abc-{tag}.png")), "own").unwrap();
    std::fs::write(
        x.manifest_dir.join(format!(".ck-abc-2-{tag}.png")),
        "sibling",
    )
    .unwrap();
    std::fs::write(x.manifest_dir.join(".ck-probe-abc-2-a"), "sibling").unwrap();
    let r = x.recover_writer_gone();
    assert_eq!(r.temps_removed.len(), 1, "{r:?}");
    assert!(
        x.manifest_dir.join(format!(".ck-abc-2-{tag}.png")).exists()
            && x.manifest_dir.join(".ck-probe-abc-2-a").exists()
    );
    std::fs::remove_dir_all(base).unwrap();
}

#[cfg(unix)]
#[test]
fn orphan_lock_recovery_needs_the_exact_id_and_never_uses_age() {
    use std::fs;
    let base = tmp("orphan");
    let d = base.join("out");
    fs::create_dir_all(&d).unwrap();
    assert_eq!(read_lock(&d).unwrap(), None);
    let lock = d.join(".corridorkey_job.lock");
    fs::write(&lock, json!({"schema": 1, "job_id": "abc", "host": "render2", "platform": "linux", "pid": 4242, "started": 1.5}).to_string()).unwrap();
    // A heartbeat that went stale long ago changes nothing: it is only reported.
    fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 30))
        .unwrap();
    let l = read_lock(&d).unwrap().unwrap();
    assert_eq!(
        (
            l.job_id.as_deref(),
            l.host.as_deref(),
            l.platform.as_deref(),
            l.pid
        ),
        (Some("abc"), Some("render2"), Some("linux"), Some(4242))
    );
    assert!(l.heartbeat_age.unwrap() > std::time::Duration::from_secs(86_400 * 29));
    // Another job's id (including one that merely starts with ours): refused, nothing touched.
    let wrong = recover_orphan(&d, "ab", true);
    assert!(!wrong.lock_removed && wrong.lock_left.is_some() && lock.exists());
    let other = d.join(".ck-abc-2-0123456789ab.png");
    fs::write(&other, "x").unwrap();
    // Temps: exact structure only.
    let own = [
        ".ck-abc-0123456789ab.png",
        ".ck-abc-0123456789ab.exr",
        ".ck-probe-abc-a",
        ".ck-probe-abc-b",
        ".corridorkey_manifest.json.tmp.abc",
        "corridorkey_v2_settings.txt.tmp.abc",
    ];
    own.iter().for_each(|n| fs::write(d.join(n), "t").unwrap());
    for n in [
        ".ck-abc-0123456789ab-0123456789ab.png",
        ".ck-abc-ZZZZZZZZZZZZ.png",
        ".ck-abc-0123456789a.png",
        ".ck-probe-abc-2-a",
        ".ck-abc-0123456789ab.tif",
        "clip_1101.png",
    ] {
        fs::write(d.join(n), "keep").unwrap();
    }
    assert_eq!(leftover_temps(&d, "abc").len(), own.len());
    assert_eq!(leftover_temps(&d, "abc-2").len(), 2); // its output tmp and its probe, and only those
                                                      // Without remove_temps: the lock goes, leftovers are only reported.
    let r = recover_orphan(&d, "abc", false);
    assert!(
        r.lock_removed && r.temps_removed.is_empty() && r.temps_left.len() == own.len(),
        "{r:?}"
    );
    assert!(own.iter().all(|n| d.join(n).exists()));
    // With remove_temps: exactly those, nothing else.
    let r = recover_orphan(&d, "abc", true);
    assert_eq!(r.temps_removed.len(), own.len());
    assert!(
        other.exists() && d.join(".ck-probe-abc-2-a").exists() && d.join("clip_1101.png").exists()
    );
    // Symlinked or garbled locks: never followed / owner unknown.
    std::os::unix::fs::symlink(base.join("nowhere"), &lock).unwrap();
    assert!(read_lock(&d).is_err() && !recover_orphan(&d, "abc", true).lock_removed);
    fs::remove_file(&lock).unwrap();
    fs::write(&lock, "garbage").unwrap();
    assert_eq!(read_lock(&d).unwrap().unwrap().job_id, None);
    assert!(!recover_orphan(&d, "abc", true).lock_removed && lock.exists());
    fs::remove_dir_all(&base).unwrap();
}
