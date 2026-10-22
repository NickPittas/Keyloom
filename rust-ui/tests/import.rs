//! End-to-end import tests: the real helper (repo .venv Python + FFmpeg) on generated files in temp dirs.
//! Skipped with a printed reason when ffmpeg or the .venv is unavailable.
use ck::import::{python, Event, Job, Request, Sources};
use ck::project::*;
use ck::ui::{App, AppOptions};
use eframe::egui::{self, RawInput, Rect, Vec2};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}
fn can_run() -> bool {
    let ff = Command::new("ffmpeg").arg("-version").output().is_ok();
    let py = python(&repo()).is_file();
    if !(ff && py) {
        eprintln!(
            "SKIP: needs ffmpeg on PATH and the repo .venv python (ffmpeg={ff}, python={py})"
        );
    }
    ff && py
}
fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ck-import-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&d);
    fs::create_dir_all(&d).unwrap();
    d
}
fn movie(path: &Path, secs: u32, size: &str) {
    let st = Command::new("ffmpeg")
        .args(["-v", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=24"))
        .args(["-t", &secs.to_string(), "-pix_fmt", "yuv420p"])
        .arg(path)
        .status()
        .unwrap();
    assert!(st.success());
}
fn png_seq(dir: &Path, n: u8, ext: &str) {
    fs::create_dir_all(dir).unwrap();
    for i in 0..n {
        let p = dir.join(format!("fr_{i:04}.{ext}"));
        if ext == "png" {
            let mut e = png::Encoder::new(fs::File::create(p).unwrap(), 8, 8);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header()
                .unwrap()
                .write_image_data(&[i * 9; 192])
                .unwrap();
        } else {
            fs::write(p, b"not really a jpeg").unwrap();
        }
    }
}
/// path → (len, content bytes): proves nothing was written/changed.
fn snap(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut m = BTreeMap::new();
    fn walk(d: &Path, m: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for e in fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, m)
            } else {
                m.insert(p.clone(), fs::read(&p).unwrap());
            }
        }
    }
    walk(root, &mut m);
    m
}

fn drive(mut job: Job, cancel_at_first_progress: bool) -> Vec<Event> {
    let mut ev = vec![];
    let end = Instant::now() + Duration::from_secs(180);
    loop {
        assert!(Instant::now() < end, "import timed out: {ev:?}");
        match job.try_recv() {
            Some(e) => {
                if cancel_at_first_progress
                    && matches!(e, Event::Progress { .. })
                    && !job.cancel_sent
                {
                    job.cancel();
                }
                let last = matches!(e, Event::Exited { .. });
                ev.push(e);
                if last {
                    return ev;
                }
            }
            None => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}
fn run(req: &Request, src: &Sources, dry: bool) -> Vec<Event> {
    drive(Job::spawn(req, src, dry, None).unwrap(), false)
}
fn created(ev: &[Event]) -> PathBuf {
    ev.iter()
        .find_map(|e| match e {
            Event::Created { project, .. } => Some(project.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no Created in {ev:?}"))
}
fn new_req(d: &Path) -> Request {
    Request::New {
        name: "My Test".into(),
        data_dir: d.join("data"),
    }
}
fn err_of(ev: &[Event]) -> String {
    ev.iter()
        .find_map(|e| match e {
            Event::Error(m) => Some(m.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("expected Error in {ev:?}"))
}

#[test]
fn new_project_from_special_named_movies_and_png_sequence_then_add_with_duplicate() {
    if !can_run() {
        return;
    }
    let d = tmp("new");
    let src = d.join("src");
    fs::create_dir_all(&src).unwrap();
    movie(&src.join("a b'.mp4"), 2, "320x180");
    movie(&src.join("ünï côde.mov"), 1, "160x90");
    png_seq(&src.join("seq"), 10, "png");
    png_seq(&src.join("seqjpg"), 3, "jpg");
    let before = snap(&src);
    let s = Sources {
        videos: vec![src.join("a b'.mp4"), src.join("ünï côde.mov")],
        folders: vec![src.join("seq"), src.join("seqjpg")],
    };
    let plan = run(&new_req(&d), &s, true);
    let Some(Event::Plan(items)) = plan.iter().find(|e| matches!(e, Event::Plan(_))) else {
        panic!("{plan:?}")
    };
    assert_eq!(items.iter().filter(|i| i.ok).count(), 3, "{items:?}");
    let bad = items.iter().find(|i| i.source.ends_with("seqjpg")).unwrap();
    assert!(
        !bad.ok && bad.reason.as_deref().unwrap().contains("PNG and EXR"),
        "{bad:?}"
    );
    assert!(!d.join("data").exists(), "dry run must write nothing");

    let ev = run(&new_req(&d), &s, false);
    assert!(ev.contains(&Event::Finished), "{ev:?}");
    let proj = created(&ev);
    assert!(proj.starts_with(d.join("data/Projects")));
    let p = load_project(&proj, &LoadOptions::default()).unwrap();
    assert_eq!(
        p.clips.len(),
        3,
        "{:?}",
        p.clips.iter().map(|c| &c.folder).collect::<Vec<_>>()
    );
    let a = p.clips.iter().find(|c| c.folder == "a_b").unwrap();
    assert_eq!(a.input_files.len(), 48);
    assert!(a.input_files[0].1.extension().unwrap() == "exr");
    assert_eq!(a.input_files[0].0, "frame_000000", "source-frame naming");
    assert!((a.fps - 24.0).abs() < 1e-6 && !a.fps_assumed);
    assert!(!a.input_is_linear, "h264 video is not linear");
    let u = p.clips.iter().find(|c| c.folder == "ünï_côde").unwrap();
    assert_eq!(u.input_files.len(), 24);
    let sq = p.clips.iter().find(|c| c.folder == "seq").unwrap();
    assert!(matches!(sq.source, Source::Sequence { external: true, .. }));
    assert_eq!(sq.input_files.len(), 10);
    assert!(
        !p.clips.iter().any(|c| c.folder.contains("jpg")),
        "no clip folder for the refused sequence"
    );
    assert_eq!(snap(&src), before, "source files and folders untouched");
    assert!(!a.root.join(".frames_importing").exists());

    // Add to the existing project: same stem collides -> a_b_2; unknown project.json key and old clip bytes survive.
    let pj = proj.join("project.json");
    let mut j: serde_json::Value = serde_json::from_slice(&fs::read(&pj).unwrap()).unwrap();
    j["future_key"] = serde_json::json!({"x": [1, 2]});
    fs::write(&pj, serde_json::to_vec_pretty(&j).unwrap()).unwrap();
    let old = snap(&a.root);
    let other = d.join("other");
    fs::create_dir_all(&other).unwrap();
    movie(&other.join("a b'.mp4"), 1, "160x90");
    let s2 = Sources {
        videos: vec![other.join("a b'.mp4"), src.join("a b'.mp4")],
        folders: vec![],
    };
    let ev = run(
        &Request::Add {
            project: proj.clone(),
        },
        &s2,
        false,
    );
    assert!(ev.contains(&Event::Finished), "{ev:?}");
    let j: serde_json::Value = serde_json::from_slice(&fs::read(&pj).unwrap()).unwrap();
    assert_eq!(j["future_key"]["x"][1], 2, "unknown keys preserved");
    assert_eq!(j["clips"].as_array().unwrap().len(), 4);
    let p = load_project(&proj, &LoadOptions::default()).unwrap();
    assert!(p
        .clips
        .iter()
        .any(|c| c.folder == "a_b_2" && c.input_files.len() == 24));
    assert_eq!(snap(&a.root), old, "existing clip folder not modified");
    // Python still reads it (the shared backend owns the format).
    let out = Command::new(python(&repo()))
        .current_dir(repo())
        .args(["-c", "import sys,json;from backend.project import get_clip_dirs,read_project_json;print(len(get_clip_dirs(sys.argv[1])), 'future_key' in read_project_json(sys.argv[1]))", proj.to_str().unwrap()])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "4 True");
    // Importing the very same source again is refused, not duplicated.
    let ev = run(
        &Request::Add {
            project: proj.clone(),
        },
        &Sources {
            videos: vec![src.join("a b'.mp4")],
            folders: vec![],
        },
        false,
    );
    assert!(err_of(&ev).contains("Nothing to import"));
}

#[test]
fn cancel_leaves_resumable_clip_and_resume_completes() {
    if !can_run() {
        return;
    }
    let d = tmp("cancel");
    movie(&d.join("long.mp4"), 10, "640x360");
    let s = Sources {
        videos: vec![d.join("long.mp4")],
        folders: vec![],
    };
    let ev = drive(Job::spawn(&new_req(&d), &s, false, None).unwrap(), true);
    assert!(ev.contains(&Event::Cancelled), "{ev:?}");
    assert!(!ev.contains(&Event::Finished));
    let proj = created(&ev);
    let p = load_project(&proj, &LoadOptions::default()).unwrap();
    let c = &p.clips[0];
    assert!(
        c.input_files.is_empty(),
        "partial frames must not look playable"
    );
    assert!(matches!(c.source, Source::Video { .. }) && ck::ui::import::needs_frames(c));
    assert!(
        c.root.join("Source").read_dir().unwrap().count() == 1,
        "source copy kept"
    );
    // Resume.
    let ev = run(
        &Request::Resume {
            clip_root: c.root.clone(),
        },
        &Sources::default(),
        false,
    );
    assert!(
        ev.contains(&Event::Finished)
            && ev
                .iter()
                .any(|e| matches!(e, Event::ClipDone { frames: 240, .. })),
        "{ev:?}"
    );
    let p = load_project(&proj, &LoadOptions::default()).unwrap();
    assert_eq!(p.clips[0].input_files.len(), 240);
    assert!(!ck::ui::import::needs_frames(&p.clips[0]));
}

#[test]
fn failures_write_nothing() {
    if !can_run() {
        return;
    }
    let d = tmp("fail");
    fs::write(d.join("broken.mp4"), b"this is not a movie").unwrap();
    let s = Sources {
        videos: vec![d.join("broken.mp4"), d.join("missing.mp4"), d.clone()],
        folders: vec![d.join("nope")],
    };
    let ev = run(&new_req(&d), &s, false);
    assert!(err_of(&ev).contains("Nothing to import"));
    assert!(
        !d.join("data").exists(),
        "nothing created for an all-bad selection"
    );
    let Some(Event::Plan(items)) = run(&new_req(&d), &s, true)
        .into_iter()
        .find(|e| matches!(e, Event::Plan(_)))
    else {
        panic!()
    };
    assert!(
        items.iter().all(|i| !i.ok && i.reason.is_some()),
        "{items:?}"
    );

    // Corrupt project.json and v1 projects are refused before any write.
    let good = d.join("good.mp4");
    movie(&good, 1, "160x90");
    let s = Sources {
        videos: vec![good.clone()],
        folders: vec![],
    };
    let corrupt = d.join("corrupt");
    fs::create_dir_all(corrupt.join("clips")).unwrap();
    fs::write(corrupt.join("project.json"), b"{ not json").unwrap();
    let v1 = d.join("v1");
    fs::create_dir_all(v1.join("Input")).unwrap();
    fs::write(v1.join("project.json"), b"{\"version\":1}").unwrap();
    for p in [&corrupt, &v1] {
        let before = snap(p);
        let ev = run(&Request::Add { project: p.clone() }, &s, false);
        let m = err_of(&ev);
        assert!(m.contains("unreadable") || m.contains("v1"), "{m}");
        assert_eq!(snap(p), before);
    }
    // Missing helper python: inline error naming the path, no panic.
    let e = Job::spawn(&new_req(&d), &s, true, Some(&d.join("no-such-repo")))
        .err()
        .unwrap();
    assert!(e.contains("no-such-repo"), "{e}");
}

fn frames(ctx: &egui::Context, app: &mut App, n: usize) {
    for _ in 0..n {
        let raw = RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                Vec2::new(1440.0, 900.0),
            )),
            ..Default::default()
        };
        let mut o = ctx.run_ui(raw, |ui| app.frame(ui));
        o.textures_delta.clear();
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn landing_import_opens_the_single_imported_clip_on_media() {
    if !can_run() {
        return;
    }
    let d = tmp("ui");
    movie(&d.join("one.mp4"), 1, "160x90");
    let ctx = egui::Context::default();
    let mut app = App::with_ctx(
        &ctx,
        AppOptions {
            data_dir: Some(d.join("data")),
            import_videos: vec![d.join("one.mp4")],
            auto_confirm: true,
            ..Default::default()
        },
    );
    let end = Instant::now() + Duration::from_secs(120);
    while !app.in_workspace() {
        assert!(Instant::now() < end, "never reached the workspace");
        frames(&ctx, &mut app, 5);
    }
    assert_eq!(app.open_frame_count(), Some(24));
    assert!(d.join("data/Projects").is_dir());
}

#[test]
fn cancel_sent_at_spawn_creates_and_copies_nothing() {
    if !can_run() {
        return;
    }
    let d = tmp("early-cancel");
    for n in ["a", "b", "c"] {
        movie(&d.join(format!("{n}.mp4")), 1, "160x90");
    }
    let s = Sources {
        videos: ["a", "b", "c"].map(|n| d.join(format!("{n}.mp4"))).to_vec(),
        folders: vec![],
    };
    let mut job = Job::spawn(&new_req(&d), &s, false, None).unwrap();
    job.cancel(); // before the helper has even finished probing
    let ev = drive(job, false);
    assert!(ev.contains(&Event::Cancelled), "{ev:?}");
    assert!(
        !ev.iter().any(|e| matches!(e, Event::Created { .. })),
        "{ev:?}"
    );
    assert!(
        !d.join("data").exists(),
        "cancelled before the first write: no project, no copies"
    );

    // Same for adding to an existing project: project bytes unchanged.
    let ev = run(
        &new_req(&d),
        &Sources {
            videos: vec![d.join("a.mp4")],
            folders: vec![],
        },
        false,
    );
    let proj = created(&ev);
    let before = snap(&proj);
    let mut job = Job::spawn(
        &Request::Add {
            project: proj.clone(),
        },
        &Sources {
            videos: vec![d.join("b.mp4"), d.join("c.mp4")],
            folders: vec![],
        },
        false,
        None,
    )
    .unwrap();
    job.cancel();
    let ev = drive(job, false);
    assert!(ev.contains(&Event::Cancelled), "{ev:?}");
    assert_eq!(snap(&proj), before);
}

#[test]
fn folder_with_trailing_separator_gets_the_planned_name_not_a_hidden_duplicate() {
    if !can_run() {
        return;
    }
    let d = tmp("slash");
    png_seq(&d.join("seq"), 4, "png");
    let s = Sources {
        videos: vec![],
        folders: vec![PathBuf::from(format!("{}/", d.join("seq").display()))],
    };
    let ev = run(&new_req(&d), &s, false);
    assert!(ev.contains(&Event::Finished), "{ev:?}");
    let p = load_project(&created(&ev), &LoadOptions::default()).unwrap();
    assert_eq!(p.clips.len(), 1);
    assert_eq!(p.clips[0].folder, "seq");
    let names: Vec<_> = fs::read_dir(created(&ev).join("clips"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "no hidden _2 folder: {names:?}");
}

#[test]
fn new_in_creates_the_project_directly_inside_the_chosen_folder() {
    if !can_run() {
        return;
    }
    let d = tmp("newin");
    png_seq(&d.join("seq"), 3, "png");
    let s = Sources {
        videos: vec![],
        folders: vec![d.join("seq")],
    };
    let parent = d.join("chosen");
    let req = Request::NewIn {
        name: "My Show".into(),
        parent: parent.clone(),
    };
    run(&req, &s, true);
    assert!(!parent.exists(), "a dry run writes nothing");
    let proj = created(&run(&req, &s, false));
    assert_eq!(
        proj.parent(),
        Some(parent.as_path()),
        "no hidden Projects level"
    );
    assert!(proj
        .file_name()
        .unwrap()
        .to_string_lossy()
        .ends_with("_My_Show"));
}

/// The helper by hand (always run inside the isolated-test wrapper): stdout JSON, exit code, nothing else.
fn helper(cwd: &Path, args: &[&str]) -> (bool, String) {
    let o = Command::new(python(&repo()))
        .current_dir(cwd)
        .arg("-I")
        .arg(repo().join("rust-ui/tools/ck_import.py"))
        .args(args)
        .output()
        .unwrap();
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).into_owned(),
    )
}

#[test]
fn malformed_destinations_fail_closed_without_a_default_location() {
    if !can_run() {
        return;
    }
    let d = tmp("baddest");
    png_seq(&d.join("seq"), 2, "png");
    let seq = format!("--folder={}", d.join("seq").display());
    let default_projects = repo().join("Projects");
    let existed = default_projects.exists();
    for args in [
        vec!["new", "--name", "X", "--parent-dir", "", seq.as_str()],
        vec!["new", "--name", "X", "--parent-dir", "  ", seq.as_str()],
        vec!["new", "--name", "X", "--data-dir", "", seq.as_str()],
        vec![
            "--dry-run",
            "new",
            "--name",
            "X",
            "--data-dir",
            "",
            seq.as_str(),
        ],
        vec!["new", "--name", "X", seq.as_str()],
        vec![
            "new",
            "--name",
            "X",
            "--data-dir",
            "a",
            "--parent-dir",
            "b",
            seq.as_str(),
        ],
        vec!["new", "--name", " ", "--parent-dir", "p", seq.as_str()],
        vec!["add", "--project", "", seq.as_str()],
    ] {
        let (ok, out) = helper(&d, &args);
        assert!(!ok, "{args:?} must fail: {out}");
        assert!(
            !out.contains("\"created\"") && !out.contains("\"finished\""),
            "{args:?}: {out}"
        );
    }
    let (_, out) = helper(
        &d,
        &["new", "--name", "X", "--parent-dir", "", seq.as_str()],
    );
    assert!(out.contains("empty"), "{out}");
    assert_eq!(
        default_projects.exists(),
        existed,
        "repo Projects/ untouched"
    );
    let left: Vec<_> = fs::read_dir(&d)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        left,
        vec![std::ffi::OsString::from("seq")],
        "nothing created next to the cwd"
    );
}

#[test]
fn probe_plans_without_any_destination_and_cannot_write() {
    if !can_run() {
        return;
    }
    let d = tmp("probe");
    png_seq(&d.join("seq"), 2, "png");
    let s = Sources {
        videos: vec![],
        folders: vec![d.join("seq")],
    };
    let before = snap(&d);
    for dry in [true, false] {
        let ev = run(&Request::Probe, &s, dry);
        let Some(Event::Plan(items)) = ev.iter().find(|e| matches!(e, Event::Plan(_))) else {
            panic!("{ev:?}")
        };
        assert!(items.len() == 1 && items[0].ok, "{items:?}");
        assert!(
            !ev.iter()
                .any(|e| matches!(e, Event::Created { .. } | Event::Error(_))),
            "{ev:?}"
        );
    }
    assert_eq!(snap(&d), before);
    // The bridge itself refuses an empty destination rather than letting the helper guess.
    for r in [
        Request::NewIn {
            name: "X".into(),
            parent: PathBuf::new(),
        },
        Request::New {
            name: "X".into(),
            data_dir: PathBuf::new(),
        },
        Request::Add {
            project: PathBuf::new(),
        },
    ] {
        assert!(Job::spawn(&r, &s, true, None).is_err(), "{r:?}");
    }
}

#[test]
fn add_refuses_projects_that_are_not_readable_supported_v2_projects() {
    if !can_run() {
        return;
    }
    let d = tmp("addbad");
    png_seq(&d.join("seq"), 2, "png");
    let s = Sources {
        videos: vec![],
        folders: vec![d.join("seq")],
    };
    let cases: [(&str, Option<&[u8]>, &str); 11] = [
        ("nojson", None, "project.json"),
        ("garbage", Some(b"{ not json"), "project.json"),
        ("list", Some(b"[2]"), "project.json"),
        ("nover", Some(b"{}"), "version"),
        ("v1", Some(br#"{"version":1}"#), "version"),
        ("v3", Some(br#"{"version":3}"#), "version"),
        ("strver", Some(br#"{"version":"2"}"#), "version"),
        (
            "clipsstr",
            Some(br#"{"version":2,"clips":"a"}"#),
            "clips list",
        ),
        (
            "clipsobj",
            Some(br#"{"version":2,"clips":[{}]}"#),
            "clips list",
        ),
        (
            "removedint",
            Some(br#"{"version":2,"removed_clips":["a",1]}"#),
            "clips list",
        ),
        (
            "removeddict",
            Some(br#"{"version":2,"removed_clips":{}}"#),
            "clips list",
        ),
    ];
    for (name, json, want) in cases {
        let p = d.join(name);
        fs::create_dir_all(p.join("clips")).unwrap();
        if let Some(j) = json {
            fs::write(p.join("project.json"), j).unwrap();
        }
        let before = snap(&p);
        for dry in [true, false] {
            let m = err_of(&run(&Request::Add { project: p.clone() }, &s, dry));
            assert!(m.contains(want), "{name}: {m}");
        }
        assert_eq!(snap(&p), before, "{name} untouched");
        assert!(
            p.join("clips").read_dir().unwrap().next().is_none(),
            "{name}"
        );
    }
    let ok = d.join("ok");
    fs::create_dir_all(ok.join("clips")).unwrap();
    fs::write(ok.join("project.json"), br#"{"version":2}"#).unwrap();
    let ev = run(&Request::Add { project: ok }, &s, true);
    assert!(ev.iter().any(|e| matches!(e, Event::Plan(_))), "{ev:?}");
}

#[test]
fn new_project_never_reuses_a_folder_once_every_numbered_name_is_taken() {
    if !can_run() {
        return;
    }
    let d = tmp("exhaust");
    png_seq(&d.join("seq"), 2, "png");
    let parent = d.join("parent");
    // Same second for the whole run: the helper pins the stamp, so a test can pin its own clock too.
    let stamp = "260101_000000_My_Test";
    for n in std::iter::once(stamp.to_owned()).chain((2..100).map(|i| format!("{stamp}_{i}"))) {
        fs::create_dir_all(parent.join(n).join("clips/old")).unwrap();
    }
    let before = snap(&parent);
    let script = r#"
import importlib.util, sys, datetime
spec = importlib.util.spec_from_file_location("ck_import", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
class D(datetime.datetime):
    @classmethod
    def now(cls, tz=None): return datetime.datetime(2026, 1, 1)
m.datetime = D
sys.argv = ["ck_import"] + sys.argv[2:]
m.main()
"#;
    let run_it = |extra: &[&str]| {
        let mut child = Command::new(python(&repo()))
            .current_dir(&d)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .args(["-I", "-c", script])
            .arg(repo().join("rust-ui/tools/ck_import.py"))
            .args(extra)
            .args(["new", "--name", "My Test", "--parent-dir"])
            .arg(&parent)
            .arg(format!("--folder={}", d.join("seq").display()))
            .spawn()
            .unwrap();
        let _open = child.stdin.take(); // closing stdin means "cancel" to the helper
        let o = child.wait_with_output().unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
        )
    };
    for extra in [&[][..], &["--dry-run"][..]] {
        let (ok, out) = run_it(extra);
        assert!(!ok && out.contains("No free project folder name"), "{out}");
    }
    assert_eq!(snap(&parent), before, "no existing project touched");
    // One free number left: that one is created, again without touching the others.
    fs::remove_dir_all(parent.join(format!("{stamp}_57"))).unwrap();
    let (ok, out) = run_it(&[]);
    assert!(ok && out.contains(&format!("{stamp}_57")), "{out}");
}

/// Runs `new` with a pinned clock (so every process wants the same folder name) and, when asked, with the
/// plan-time precheck disabled: that is the state a process is in when another one wins the race after it.
const NO_PRECHECK: &str = "m.project_folder_problem = lambda *a: None";

fn pinned_new(cwd: &Path, parent: &Path, seq: &Path, patch: &str) -> std::process::Child {
    let script = r#"
import importlib.util, sys, datetime
spec = importlib.util.spec_from_file_location("ck_import", sys.argv[1])
m = importlib.util.module_from_spec(spec); spec.loader.exec_module(m)
class D(datetime.datetime):
    @classmethod
    def now(cls, tz=None): return datetime.datetime(2026, 1, 1)
m.datetime = D
exec(sys.argv[2])
sys.argv = ["ck_import"] + sys.argv[3:]
m.main()
"#;
    Command::new(python(&repo()))
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .args(["-I", "-c", script])
        .arg(repo().join("rust-ui/tools/ck_import.py"))
        .arg(patch)
        .args(["new", "--name", "My Test", "--parent-dir"])
        .arg(parent)
        .arg(format!("--folder={}", seq.display()))
        .spawn()
        .unwrap()
}

#[test]
fn losing_the_folder_race_never_reuses_or_rewrites_the_winners_project() {
    if !can_run() {
        return;
    }
    let d = tmp("race");
    png_seq(&d.join("seq"), 2, "png");
    let parent = d.join("parent");
    let stamp = "260101_000000_My_Test";
    // The winner already exists (as if created after our precheck passed): metadata we must not touch.
    let win = parent.join(stamp);
    fs::create_dir_all(win.join("clips/old")).unwrap();
    fs::write(
        win.join("project.json"),
        br#"{"version":2,"clips":["old"],"keep":1}"#,
    )
    .unwrap();
    let before = snap(&parent);
    let mut c = pinned_new(&d, &parent, &d.join("seq"), NO_PRECHECK);
    let _open = c.stdin.take();
    let out = c.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = snap(&parent);
    for (k, v) in &before {
        assert_eq!(after.get(k), Some(v), "winner file changed: {k:?}");
    }
    let json: serde_json::Value =
        serde_json::from_slice(&fs::read(parent.join(format!("{stamp}_2/project.json"))).unwrap())
            .unwrap();
    assert_eq!(json["clips"], serde_json::json!(["seq"]));

    // Nothing free (precheck disabled): fails, still touching nothing that exists.
    for n in 3..100 {
        fs::create_dir_all(parent.join(format!("{stamp}_{n}/clips"))).unwrap();
    }
    let before = snap(&parent);
    let mut c = pinned_new(&d, &parent, &d.join("seq"), NO_PRECHECK);
    let _open = c.stdin.take();
    let out = c.wait_with_output().unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("No free project folder name"));
    assert_eq!(snap(&parent), before);
}

#[test]
fn concurrent_new_projects_each_get_their_own_folder_and_metadata() {
    if !can_run() {
        return;
    }
    let d = tmp("conc");
    png_seq(&d.join("seq"), 2, "png");
    let parent = d.join("parent");
    // Same name, same pinned second, no precheck head start: every process fights for the same names.
    let mut kids: Vec<_> = (0..8)
        .map(|_| {
            let mut c = pinned_new(&d, &parent, &d.join("seq"), NO_PRECHECK);
            let keep = c.stdin.take();
            (c, keep)
        })
        .collect();
    for (c, _) in kids.iter_mut() {
        assert!(c.wait().unwrap().success());
    }
    let mut dirs: Vec<_> = fs::read_dir(&parent)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    dirs.sort();
    assert_eq!(dirs.len(), 8, "{dirs:?}");
    for p in dirs {
        let j: serde_json::Value =
            serde_json::from_slice(&fs::read(p.join("project.json")).unwrap()).unwrap();
        assert_eq!(j["clips"], serde_json::json!(["seq"]), "{p:?}");
        assert!(p.join("clips/seq/clip.json").is_file(), "{p:?}");
        assert_eq!(fs::read_dir(p.join("clips")).unwrap().count(), 1, "{p:?}");
    }
}

#[test]
fn failure_after_claiming_a_new_project_names_the_partial_folder_and_keeps_it() {
    if !can_run() {
        return;
    }
    let d = tmp("partial");
    png_seq(&d.join("seq"), 2, "png");
    let parent = d.join("parent");
    let boom = "def boom(*a, **k): raise OSError('disk full')\nm.P.add_sequences_to_project = boom";
    let mut c = pinned_new(&d, &parent, &d.join("seq"), boom);
    let _open = c.stdin.take();
    let out = c.wait_with_output().unwrap();
    let so = String::from_utf8_lossy(&out.stdout);
    let kept = parent.join("260101_000000_My_Test");
    assert!(!out.status.success() && so.contains("disk full"), "{so}");
    assert!(
        so.contains(&format!("partial project kept at {}", kept.display())),
        "{so}"
    );
    assert!(kept.join("project.json").is_file(), "never deleted");
}
