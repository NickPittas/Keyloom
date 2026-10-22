//! Headless evidence tool sharing the viewer's core: `inspect`, `raw`, `bench`.
use ck::decode::{read_exr_raw, Interp};
use ck::frames::*;
use ck::project::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn arg(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn pct(v: &mut [f64], p: f64) -> f64 {
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    v[((v.len() - 1) as f64 * p).round() as usize]
}

fn rss_kb(field: &str) -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with(field))
                .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

fn load(args: &[String]) -> Project {
    let root = args.get(1).expect("project path required");
    let opts = LoadOptions {
        global_output_dir: arg(args, "--global-output-dir").map(PathBuf::from),
    };
    load_project(Path::new(root), &opts).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    })
}

fn inspect(args: &[String]) {
    let p = load(args);
    let clips: Vec<Value> = p
        .clips
        .iter()
        .map(|c| {
            let idx = FrameIndex::build(c);
            let views: serde_json::Map<String, Value> = View::ALL.iter().map(|v| (v.label().to_string(), json!(idx.view_frames(*v)))).collect();
            json!({
                "name": c.name, "folder": c.folder, "error": c.error, "state": c.state.label(), "source": format!("{:?}", c.source),
                "input_frames": c.input_files.len(), "timeline": idx.len(), "in_out": c.in_out,
                "fps": c.fps, "fps_assumed": c.fps_assumed, "input_is_linear": c.input_is_linear, "color_origin": format!("{:?}", c.color_origin),
                "output_dir": c.output_dir, "output_dir_rule": c.output_dir_note, "view_frames": views, "notes": c.notes,
            })
        })
        .collect();
    println!(
        "{}",
        json!({"project": p.display_name, "v2": p.is_v2, "notes": p.notes, "clips": clips})
    );
}

fn raw(args: &[String]) {
    let path = Path::new(args.get(1).expect("exr path required"));
    let t = Instant::now();
    let r = read_exr_raw(path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let ms = t.elapsed().as_secs_f64() * 1000.0;
    let (w, h) = (r.width, r.height);
    let chans: Vec<Value> = r
        .channels
        .iter()
        .map(|c| {
            let n = c.data.len() as f64;
            json!({"name": c.name, "type": c.sample_type, "mean": c.data.iter().map(|v| *v as f64).sum::<f64>() / n,
                "min": c.data.iter().cloned().fold(f32::INFINITY, f32::min), "max": c.data.iter().cloned().fold(f32::NEG_INFINITY, f32::max),
                "finite": c.data.iter().all(|v| v.is_finite()),
                "tl": c.data[0], "tr": c.data[w - 1], "bl": c.data[(h - 1) * w], "br": c.data[h * w - 1], "center": c.data[(h / 2) * w + w / 2]})
        })
        .collect();
    println!(
        "{}",
        json!({"file": path, "width": w, "height": h, "compression": r.compression, "decode_ms": ms, "channels": chans})
    );
}

fn bench(args: &[String]) {
    let p = load(args);
    let cname = arg(args, "--clip");
    let clip = p
        .clips
        .iter()
        .find(|c| {
            cname
                .as_deref()
                .is_none_or(|n| c.name == n || c.folder == n)
        })
        .expect("clip not found");
    let view = match arg(args, "--view")
        .unwrap_or_else(|| "input".into())
        .as_str()
    {
        "hint" => View::Hint,
        "fg" => View::Fg,
        "matte" => View::Matte,
        "comp" => View::Comp,
        "processed" => View::Processed,
        _ => View::Input,
    };
    let cache_mb: usize = arg(args, "--cache-mb")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1024);
    let nframes: usize = arg(args, "--frames")
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    let idx = FrameIndex::build(clip);
    // `--dir D` benchmarks an arbitrary frame folder as `--view` (e.g. a legacy clip-root Matte/ DWAB set).
    let keys: Vec<(usize, Key)> = if let Some(d) = arg(args, "--dir") {
        list_images(Path::new(&d))
            .into_iter()
            .enumerate()
            .map(|(i, (_, p))| {
                (
                    i,
                    Key {
                        interp: view_interp(
                            view,
                            &p,
                            clip.input_is_linear,
                            processed_is_straight(clip),
                        ),
                        path: p,
                    },
                )
            })
            .take(nframes.max(1))
            .collect()
    } else {
        (0..idx.len())
            .filter_map(|i| match idx.availability(view, i, None) {
                Availability::Present(p) => Some((
                    i,
                    Key {
                        interp: view_interp(
                            view,
                            &p,
                            clip.input_is_linear,
                            processed_is_straight(clip),
                        ),
                        path: p,
                    },
                )),
                _ => None,
            })
            .take(nframes.max(1))
            .collect()
    };
    assert!(!keys.is_empty(), "view has no frames");
    let workers = arg(args, "--workers")
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(default_workers);
    let mut store = FrameStore::new(cache_mb << 20, workers, real_decode(), None);
    let wait = |s: &mut FrameStore, k: &Key| -> Option<Arc2> {
        let dl = Instant::now() + Duration::from_secs(60);
        loop {
            s.poll();
            match s.lookup(k) {
                Lookup::Ready(f) => return Some(f),
                Lookup::Failed(e) => {
                    eprintln!("{e}");
                    return None;
                }
                Lookup::Loading => {
                    if Instant::now() > dl {
                        return None;
                    }
                    s.wait_one(Duration::from_millis(2));
                }
            }
        }
    };
    store.request(keys[0].1.clone(), vec![]);
    let first = wait(&mut store, &keys[0].1).expect("first frame failed");
    let info = first.info.clone();
    let (fw, fh) = (first.width, first.height);
    // Sequential cold pass with a 3-frame prefetch window = what the player does.
    let mut cold = vec![];
    for (n, (_, k)) in keys.iter().enumerate() {
        let t = Instant::now();
        store.request(
            k.clone(),
            keys.iter()
                .skip(n + 1)
                .take(3)
                .map(|x| x.1.clone())
                .collect(),
        );
        wait(&mut store, k);
        cold.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    // Warm pass (cache permitting).
    let mut warm = vec![];
    for (_, k) in &keys {
        let t = Instant::now();
        store.request(k.clone(), vec![]);
        wait(&mut store, k);
        warm.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let cache_peak = store.cache.peak_bytes;
    drop(store);
    // Real-time playback at clip fps from a cold cache: hold frame when decode lags, count drops.
    let mut pstore = FrameStore::new(cache_mb << 20, workers, real_decode(), None);
    let (mut shown, mut drops, mut shown_n) = (0usize, 0usize, 0usize);
    let clock = Clock::start(clip.fps, 0, 1);
    let t0 = Instant::now();
    while shown < keys.len() - 1 || shown_n == 0 {
        let want = (clock.frame_at(Instant::now()).max(0) as usize).min(keys.len() - 1);
        pstore.request(
            keys[want].1.clone(),
            keys.iter()
                .skip(want + 1)
                .take(4)
                .map(|x| x.1.clone())
                .collect(),
        );
        pstore.poll();
        // Display the newest frame that is ready, not later than the clock target.
        for i in (shown..=want).rev() {
            if matches!(pstore.lookup(&keys[i].1), Lookup::Ready(_)) {
                if i > shown + 1 || (shown_n > 0 && i > shown + 1) {
                    drops += i - shown - 1;
                }
                if i != shown || shown_n == 0 {
                    shown_n += 1;
                }
                shown = i;
                break;
            }
        }
        if want >= keys.len() - 1 && shown == keys.len() - 1 {
            break;
        }
        if t0.elapsed() > Duration::from_secs(120) {
            break;
        }
        pstore.wait_one(Duration::from_millis(1));
    }
    let play_s = t0.elapsed().as_secs_f64();
    drop(pstore);
    // Random seeks, cold cache each time → decode latency.
    let mut seeks = vec![];
    let mut sstore = FrameStore::new(cache_mb << 20, workers, real_decode(), None);
    let mut seed = 99u64;
    for _ in 0..20 {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let k = &keys[(seed >> 33) as usize % keys.len()].1;
        let t = Instant::now();
        sstore.request(k.clone(), vec![]);
        wait(&mut sstore, k);
        seeks.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let cold_total: f64 = cold.iter().sum();
    println!(
        "{}",
        json!({
            "project": p.display_name, "clip": clip.name, "view": view.label(), "decoded": info, "width": fw, "height": fh,
            "frames_measured": keys.len(), "clip_fps": clip.fps, "fps_assumed": clip.fps_assumed, "workers": workers, "cache_mb": cache_mb,
            "cold_ms": {"median": pct(&mut cold.clone(), 0.5), "p95": pct(&mut cold.clone(), 0.95), "max": pct(&mut cold, 1.0)},
            "warm_ms": {"median": pct(&mut warm.clone(), 0.5), "p95": pct(&mut warm.clone(), 0.95), "max": pct(&mut warm, 1.0)},
            "cold_throughput_fps": keys.len() as f64 / (cold_total / 1000.0),
            "realtime_playback": {"seconds": play_s, "frames_shown": shown_n, "frames_dropped": drops, "achieved_fps": shown_n as f64 / play_s, "reaches_clip_fps": drops == 0},
            "seek_ms": {"median": pct(&mut seeks.clone(), 0.5), "p95": pct(&mut seeks, 0.95)},
            "cache_peak_bytes": cache_peak, "rss_peak_kb": rss_kb("VmHWM"),
            "git_rev": std::process::Command::new("git").args(["rev-parse", "--short", "HEAD"]).output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()),
            "interp": format!("{:?}", keys[0].1.interp),
        })
    );
    let _ = Interp::Matte;
}
type Arc2 = std::sync::Arc<ck::decode::DisplayFrame>;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    match a.first().map(String::as_str) {
        Some("inspect") => inspect(&a),
        Some("raw") => raw(&a),
        Some("bench") => bench(&a),
        _ => {
            eprintln!("usage: ck-bench inspect <project> | raw <file.exr> | bench <project> [--clip N] [--view input|hint|fg|matte|comp|processed] [--dir FRAMES_DIR] [--frames N] [--cache-mb N] [--workers N] [--global-output-dir D]");
            std::process::exit(1)
        }
    }
}
