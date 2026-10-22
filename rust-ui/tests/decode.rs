//! Decoder tests against SYNTHETIC fixtures written by `tools/gen_fixtures.py` (Python OpenEXR), covering
//! DWAB/PIZ/ZIP/none for Y, RGB(half) and RGBA(half). Plus ignored checks on real local projects.
use ck::decode::*;
use std::path::{Path, PathBuf};

fn fx(n: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(n)
}
const W: usize = 64;
const H: usize = 32;
fn base(i: usize) -> f32 {
    i as f32 / (W * H) as f32
}

#[test]
fn y_all_compressions_decode_as_scalar() {
    for (c, tol, name) in [
        ("none", 0.0, "Uncompressed"),
        ("zip", 0.0, "ZIP"),
        ("piz", 0.0, "PIZ"),
        ("dwab", 0.02, "DWAB"),
    ] {
        let r = read_exr_raw(&fx(&format!("y_{c}.exr"))).unwrap();
        assert!(
            r.compression.starts_with(name) || r.compression.contains(name),
            "{}",
            r.compression
        );
        assert_eq!((r.width, r.height, r.channels.len()), (W, H, 1));
        assert_eq!(r.channels[0].name, "Y");
        assert_eq!(r.channels[0].sample_type, "float");
        let worst = r.channels[0]
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| (v - base(i)).abs())
            .fold(0.0, f32::max);
        assert!(worst <= tol, "{c}: worst error {worst}");
        let d = decode_display(&fx(&format!("y_{c}.exr")), Interp::Matte).unwrap();
        assert!(!d.has_alpha);
        let p = &d.rgba[(H / 2 * W + 10) * 4..][..4];
        assert!(
            p[0] == p[1] && p[1] == p[2] && p[3] == 255,
            "matte must be grey, opaque"
        );
    }
}

#[test]
fn rgb_and_rgba_half_all_compressions() {
    for c in ["none", "zip", "piz", "dwab"] {
        let tol = if c == "dwab" { 0.03 } else { 0.001 };
        let r = read_exr_raw(&fx(&format!("rgba_{c}.exr"))).unwrap();
        let names: Vec<_> = r.channels.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["A", "B", "G", "R"], "{c}");
        assert!(r.channels.iter().all(|c| c.sample_type == "half"));
        let rch = &r.channels[3].data;
        assert!(
            rch.iter()
                .enumerate()
                .all(|(i, v)| (v - base(i)).abs() <= tol),
            "{c} R"
        );
        assert!(
            r.channels[0].data.iter().all(|v| (v - 0.5).abs() <= tol),
            "{c} A"
        );
        // Display: straight alpha preserved, linear→sRGB applied to colour.
        let d = decode_display(&fx(&format!("rgba_{c}.exr")), Interp::LinearToSrgb).unwrap();
        assert!(d.has_alpha);
        let px = &d.rgba[(5 * W + 7) * 4..][..4];
        assert!((px[3] as i32 - 128).abs() <= 4, "alpha {px:?}");
        let b = linear_to_srgb8(0.25);
        assert!((px[2] as i32 - b as i32).abs() <= 6, "blue {px:?} vs {b}");
        let d = decode_display(&fx(&format!("rgb_{c}.exr")), Interp::Passthrough).unwrap();
        assert!(!d.has_alpha);
    }
}

#[test]
fn header_summary_lists_channels() {
    let s = exr_header_summary(&fx("y_piz.exr")).unwrap();
    assert!(
        s.contains("64×32") && s.contains("PIZ") && s.contains("[Y]"),
        "{s}"
    );
}

/// Real local media; never committed. Run:
///   CK_REAL_PROJECT=../Projects/<project> [CK_REAL_REFERENCE=/tmp/ck-rust-decoder-reference.json] \
///     cargo test --release -- --ignored
/// Fails loudly when the environment is not set up, so a bare `--ignored` run cannot "pass" without decoding.
#[test]
#[ignore]
fn real_project_frames_decode() {
    let root = std::env::var("CK_REAL_PROJECT")
        .expect("set CK_REAL_PROJECT=<project folder> to run this ignored test");
    let p = ck::project::load_project(Path::new(&root), &Default::default()).unwrap();
    assert!(!p.clips.is_empty(), "project lists no clips");
    let mut decoded = 0;
    for c in &p.clips {
        let idx = ck::frames::FrameIndex::build(c);
        for v in ck::project::View::ALL {
            for i in [0, idx.len() / 2] {
                if let ck::frames::Availability::Present(path) = idx.availability(v, i, None) {
                    let interp = ck::frames::view_interp(
                        v,
                        &path,
                        c.input_is_linear,
                        ck::frames::processed_is_straight(c),
                    );
                    let d = decode_display(&path, interp).unwrap();
                    assert!(
                        d.width > 0 && d.rgba.len() == d.width * d.height * 4,
                        "{}",
                        path.display()
                    );
                    if path.extension().is_some_and(|e| e == "exr") {
                        let r = read_exr_raw(&path).unwrap();
                        assert!(
                            r.channels
                                .iter()
                                .all(|c| c.data.iter().all(|x| x.is_finite())),
                            "{}",
                            path.display()
                        );
                    }
                    decoded += 1;
                }
            }
        }
    }
    assert!(decoded > 0, "no frame was decoded");
}

/// Numeric match against an independent Python OpenEXR reference (`[{path, channels:{name:{mean,min,max}}}]`).
#[test]
#[ignore]
fn real_frames_match_python_reference() {
    let r = std::env::var("CK_REAL_REFERENCE")
        .expect("set CK_REAL_REFERENCE=<reference json> to run this ignored test");
    let refs: serde_json::Value = serde_json::from_slice(&std::fs::read(r).unwrap()).unwrap();
    let list = refs.as_array().expect("reference must be a list");
    assert!(!list.is_empty());
    for e in list {
        let path = Path::new(e["path"].as_str().unwrap());
        let raw = read_exr_raw(path).unwrap();
        for (name, st) in e["channels"].as_object().unwrap() {
            let c = raw
                .channels
                .iter()
                .find(|c| &c.name == name)
                .unwrap_or_else(|| panic!("{} lacks {name}", path.display()));
            let mean = c.data.iter().map(|v| *v as f64).sum::<f64>() / c.data.len() as f64;
            let (mn, mx) = c
                .data
                .iter()
                .fold((f32::INFINITY, f32::NEG_INFINITY), |(a, b), v| {
                    (a.min(*v), b.max(*v))
                });
            assert!(
                (mean - st["mean"].as_f64().unwrap()).abs() < 1e-6,
                "{} {name}: mean {mean}",
                path.display()
            );
            assert!(
                (mn as f64 - st["min"].as_f64().unwrap()).abs() < 1e-6,
                "{} {name} min",
                path.display()
            );
            assert!(
                (mx as f64 - st["max"].as_f64().unwrap()).abs() < 1e-6,
                "{} {name} max",
                path.display()
            );
        }
    }
}
