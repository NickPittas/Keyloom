//! Shared helpers: temp dirs, a tmp "repo" whose `backend/worker/__main__.py` runs the fake worker with a
//! baked config, job builder, polling. Every worker runs as /usr/bin/python3 inside the isolation wrapper.
#![allow(dead_code)]
use ck::contracts::RangeIdentity;
use ck::jobs::{JobMode, KeyJob, Require};
use ck::worker::{Target, TargetKind};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

pub const FAKE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_worker.py");
pub const PY: &str = "/usr/bin/python3";

pub fn tmp(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "ckjobs-{}-{tag}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// `repo` dir with `backend.worker` that execs the fake worker under `cfg` (log path is added).
pub fn make_repo(parent: &Path, name: &str, mut cfg: Value) -> PathBuf {
    let repo = parent.join(name);
    let pkg = repo.join("backend/worker");
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(repo.join("backend/__init__.py"), "").unwrap();
    std::fs::write(pkg.join("__init__.py"), "").unwrap();
    cfg.as_object_mut()
        .unwrap()
        .entry("sha")
        .or_insert(json!("aa")); // jobs below require "aa"
    cfg["log"] = json!(parent.join(format!("{}.log", name.replace(['/', ' '], "_"))));
    std::fs::write(
        pkg.join("__main__.py"),
        format!(
            "import os, runpy\nos.environ['FAKE_CFG'] = {:?}\nrunpy.run_path({FAKE:?}, run_name='__main__')\n",
            cfg.to_string()
        ),
    )
    .unwrap();
    repo
}

pub fn log_lines(parent: &Path, name: &str) -> Vec<Value> {
    std::fs::read_to_string(parent.join(format!("{}.log", name.replace(['/', ' '], "_"))))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

pub fn local(id: &str, gpu: u32, repo: &Path) -> Target {
    Target {
        id: id.into(),
        kind: TargetKind::Local {
            repo: repo.into(),
            python: Some(PY.into()),
        },
        gpu,
        enabled: true,
    }
}

/// `n` frames `1101..`, one `matte` output under `<base>/<id>/`, params.fake = `fake`.
pub fn job(base: &Path, id: &str, n: usize, fake: &str) -> KeyJob {
    let dir = base.join(id);
    let clip = base.join(format!("{id}_clip"));
    if base.starts_with(std::env::temp_dir()) {
        std::fs::create_dir_all(&clip).unwrap(); // pure-validation tests use fictional /mnt paths
    }
    KeyJob {
        job_id: id.into(),
        clip_id: format!("clip-{id}"),
        clip_root: clip,
        range: RangeIdentity {
            in_point: 0,
            out_point: n - 1,
            in_stem: "1101".into(),
            out_stem: format!("{}", 1100 + n),
        },
        params: json!({"fake": fake, "delay": 0.0, "hint_mode": "none"}),
        output_config: json!({"fg_enabled": false, "fg_format": "exr", "matte_enabled": true, "matte_format": "png",
            "comp_enabled": false, "comp_format": "png", "processed_enabled": false, "processed_format": "exr",
            "exr_compression": "dwab"}),
        hint_keyframes_abs: None,
        outputs: [(
            "matte".to_string(),
            (0..n)
                .map(|i| dir.join(format!("clip_{}.png", 1101 + i)))
                .collect(),
        )]
        .into(),
        manifest_dir: dir,
        mode: JobMode::New,
        publish: "final".into(),
        path_map: vec![],
        source_map: vec![],
        enforce_fixed_window: false,
        require: Require {
            protocol: 1,
            models: [("corridorkey_v2".to_string(), Some("aa".to_string()))].into(),
        },
    }
}

pub fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}
