//! Full-pipeline batch planning (pre-matte -> Key -> video export), pure and UI-free. Every clip gets the preset's
//! settings but its own media, range, prompts, masks and hints: automatic producers/refiners are planned to re-run,
//! and `hint_offer` tells the review when a hint render already exists so the user can reuse it (a hint that is
//! known to differ is never reused silently); manual SAM guides and imported alphas must already exist on that clip.
use crate::hints::{
    check_hint, covers, identity, staging_root, stamp_value, Annotations, Compat, Ctx, ExportCfg, Final, HintCfg,
    HintWant, Producer, CONTAINERS, GUIDE_DIR, HINT_DIR,
};
use crate::jobs::KeyJob;
use crate::presets::PipelineExport;
use crate::project::Clip;
use crate::tasks::TaskJob;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

pub struct HintPlan {
    /// Chained in order; Key follows the last one.
    pub steps: Vec<TaskJob>,
    /// Shown in the review when the configured producer is NOT run ("Key only: ...").
    pub note: Option<String>,
}

/// An existing hint render the queue could reuse instead of the planned hint steps.
#[derive(Debug, Clone)]
pub struct HintOffer {
    pub compat: Compat,
    /// `stem -> size:mtime_ns` of exactly the hint frames Key will read, frozen when the review was built.
    pub identity: BTreeMap<String, String>,
    /// Why the offer cannot be proven compatible even though a hint exists (e.g. the pre-matte is gone).
    pub note: Option<String>,
}

/// Compares the hint frames (and, when the plan regenerates it, the pre-matte) already on disk with the render
/// `steps` would make. `None` = nothing to reuse (no steps into `AlphaHint`, or a requested frame is missing).
/// Reads frames: call off the UI thread.
pub fn hint_offer(clip: &Clip, steps: &[TaskJob]) -> Option<HintOffer> {
    let (hint_dir, guide_dir) = (clip.root.join(HINT_DIR), clip.root.join(GUIDE_DIR));
    let sha = |p: &Path| crate::sha256::file(p, &std::sync::atomic::AtomicBool::new(false), |_, _| {}).ok().flatten();
    let guide = crate::hints::hint_files(&guide_dir);
    let (mut compat, mut read, mut note) = (Compat::Compatible, vec![], None);
    for t in steps {
        let (dir, r) = (t.publish_dir.as_deref()?, t.range.as_ref()?);
        let files = clip.input_files.get(r.in_point..=r.out_point)?;
        let stems: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
        let into_hint = dir == hint_dir;
        let mut want = HintWant {
            kind: t.kind.wire().into(),
            params: t.params.clone(),
            source: files.iter().map(|f| (f.0.clone(), stamp_value(&f.1))).collect(),
            upstream: Default::default(),
            // ponytail: the planner never makes a SAM hint itself, so there is no prompt fingerprint to compare
            prompt_fp: None,
            guide: (dir == hint_dir).then(|| guide_dir.clone()),
        };
        if into_hint {
            // The pre-matte this hint is refined from. MatAnyone2 is conditioned only on the In frame (the worker
            // records just that one), so only that frame is compared; the other kinds read every frame.
            let seeded = if t.kind == crate::tasks::TaskKind::Matanyone2 { 1 } else { stems.len() };
            want.upstream = if seeded == 1 {
                // The seed must be the very In frame: no guide file there compares as "" and never matches the
                // record, so a hint seeded from an earlier In frame is a Mismatch, not Compatible.
                stems.iter().take(1).map(|s| (s.clone(), guide.get(s).and_then(|p| sha(p)).unwrap_or_default())).collect()
            } else {
                stems.iter().filter_map(|s| Some((s.clone(), sha(guide.get(s)?)?))).collect()
            };
            read.extend(stems.iter().cloned());
        } else if dir != guide_dir {
            return None;
        }
        let c = check_hint(dir, &stems, &want);
        if c == Compat::Missing && dir == guide_dir {
            // pre-matte gone but the hint kept: still ask, saying why it cannot be verified (never a silent re-render)
            note = Some("the pre-matte it was made from is missing, so it cannot be verified".to_string());
            compat = compat.worst(Compat::Unrecorded);
            continue;
        }
        compat = compat.worst(c);
        if compat == Compat::Missing {
            return None;
        }
    }
    Some(HintOffer { compat, identity: identity(&hint_dir, &read).filter(|_| !read.is_empty())?, note })
}

fn frames(stems: &[String], r: (usize, usize)) -> String {
    if r.0 == r.1 {
        format!("frame {}", stems[r.0])
    } else {
        format!("frames {}-{}", stems[r.0], stems[r.1])
    }
}

/// The hint-generation steps a preset implies for one clip (`Err` = the reason this clip cannot be queued).
pub fn hint_steps(
    clip: &Clip,
    project_root: &Path,
    cfg: &HintCfg,
    hint_mode: &str,
    range: (usize, usize),
    linear: bool,
    mut id: impl FnMut() -> String,
) -> Result<HintPlan, String> {
    let none = |note: String| Ok(HintPlan { steps: vec![], note: Some(note) });
    if cfg.bypass {
        return none(format!("Key only: hint step bypassed (configured {} not run)", cfg.guide.label()));
    }
    if hint_mode == "none" {
        return none(format!(
            "Key only: hints disabled (mode none), configured {} not run",
            cfg.guide.label()
        ));
    }
    cfg.validate()?;
    let stems: Vec<String> = clip.input_files.iter().map(|f| f.0.clone()).collect();
    if stems.is_empty() || range.1 >= stems.len() || range.0 > range.1 {
        return Err("no source frames for this range".into());
    }
    // `keyframes` mode: hints on this clip's own marks inside the range only.
    let ranges: Vec<(usize, usize)> = if hint_mode == "keyframes" {
        let k: Vec<_> = clip
            .keyframes
            .iter()
            .copied()
            .filter(|k| (range.0..=range.1).contains(k))
            .map(|k| (k, k))
            .collect();
        if k.is_empty() {
            return Err("no keyframe marks inside In/Out on this clip: mark its own frames first".into());
        }
        k
    } else {
        vec![range]
    };
    let hint_dir = clip.root.join(HINT_DIR);
    let guide_dir = clip.root.join(GUIDE_DIR);
    if cfg.finish == Final::Import {
        for r in &ranges {
            if !covers(&hint_dir, &stems, *r) {
                return Err(format!(
                    "imported alpha: this clip's own AlphaHint does not cover {}; import its companion alpha (nothing is copied from another clip)",
                    frames(&stems, *r)
                ));
            }
        }
        return none("Imported alpha: using this clip's own AlphaHint".into());
    }
    let manual = |what: &str, missing: String| {
        format!(
            "{what} pre-matte missing: this clip has no VideoMamaMaskHint covering {missing}; precompute this clip's {what} pre-matte first (open the clip, generate the guide in AI/Hints), then queue again (prompts and key settings are per clip and never copied or generated across a batch)"
        )
    };
    let staging = staging_root(project_root, &clip.root, &clip.folder);
    let ann = Annotations::default();
    let mut steps = vec![];
    // SAM and Chroma pre-mattes are prepared by hand per clip; GVM and BiRefNet are generated here.
    let manual_guide = matches!(cfg.guide, Producer::Sam | Producer::Chroma);
    for r in ranges {
        let cx = |range: (usize, usize), id: &mut dyn FnMut() -> String| Ctx {
            clip,
            range,
            job_id: id(),
            staging: staging.clone(),
        };
        // MatAnyone2 seeds from the In frame only; VideoMaMa and the direct hint need the whole range.
        let need = if cfg.finish == Final::Matanyone2 { (r.0, r.0) } else { r };
        if manual_guide {
            if !covers(&guide_dir, &stems, need) {
                return Err(manual(cfg.guide.label(), frames(&stems, need)));
            }
        } else {
            steps.push(cx(need, &mut id).producer(cfg, &ann, linear, GUIDE_DIR)?);
        }
        steps.push(if cfg.finish == Final::Direct {
            cx(r, &mut id).prematte_hint(cfg)?
        } else {
            cx(r, &mut id).refine(cfg)?
        });
    }
    let note = manual_guide.then(|| {
        format!("{}: using this clip's own precomputed pre-matte", cfg.guide.label())
    });
    Ok(HintPlan { steps, note })
}

fn safe(s: &str) -> String {
    let t: String = s
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) || c.is_control() { '_' } else { c })
        .collect();
    let t = t.trim_matches(|c| c == '.' || c == ' ');
    if t.is_empty() { "clip".into() } else { t.to_string() }
}

/// The folder a clip's videos go to: the chosen root, else `<project>/Exports/<clip folder>` (file-name safe).
pub fn export_dir(clip: &Clip, project_root: &Path, root: Option<&Path>) -> PathBuf {
    root.map_or_else(|| project_root.join("Exports").join(safe(&clip.folder)), Path::to_path_buf)
}

/// The never-overwriting destination of one product's video for one clip:
/// `<root or project/Exports/<clip folder>>/<name>_<product>_<in>-<out>.<ext>`, `_v2`... when the file exists on
/// disk or another export of this batch already took the name.
pub fn export_dests(
    clip: &Clip,
    project_root: &Path,
    e: &PipelineExport,
    range: (usize, usize),
    reserved: &mut BTreeSet<String>,
) -> Result<Vec<(String, PathBuf)>, String> {
    e.validate_videos()?;
    let (a, b) = (
        &clip.input_files.get(range.0).ok_or("range outside the clip")?.0,
        &clip.input_files.get(range.1).ok_or("range outside the clip")?.0,
    );
    let dir = export_dir(clip, project_root, e.root.as_deref());
    let mut out = vec![];
    for (product, v) in e.videos() {
        let ext = CONTAINERS.iter().find(|c| c.0 == v.container).map(|c| c.2).ok_or("unknown container")?;
        let name = safe(&format!("{}_{}_{}-{}", clip.name, product, a, b));
        let free = |p: &PathBuf| !p.exists() && !reserved.contains(&p.to_string_lossy().to_lowercase());
        let first = dir.join(format!("{name}.{ext}"));
        let dest = if free(&first) {
            first
        } else {
            (2..1000)
                .map(|n| dir.join(format!("{name}_v{n}.{ext}")))
                .find(free)
                .ok_or("no free export file name")?
        };
        reserved.insert(dest.to_string_lossy().to_lowercase());
        out.push((product.to_string(), dest));
    }
    Ok(out)
}

/// The export jobs for a result folder (a chosen Key plan's frozen manifest folder, which may not exist yet, or
/// an existing result), one per Video delivery; `products` are that result's outputs. The caller chains them one after another after Key.
pub fn export_jobs(
    clip: &Clip,
    clip_id: &str,
    range: (usize, usize),
    e: &PipelineExport,
    manifest_dir: &Path,
    products: &[String],
    dests: &[(String, PathBuf)],
    mut new_id: impl FnMut() -> String,
) -> Result<Vec<TaskJob>, String> {
    e.validate_videos()?;
    if !(clip.fps.is_finite() && clip.fps > 0.0) && e.fps.is_none() {
        return Err("this clip has no known fps".into());
    }
    let mut jobs = vec![];
    for (product, v) in e.videos() {
        if !products.iter().any(|p| p == product) {
            return Err(format!("export product {product} is not one of this Key result's outputs"));
        }
        let dest = &dests.iter().find(|d| d.0 == product).ok_or_else(|| format!("no destination for {product}"))?.1;
        if dest.exists() {
            return Err(format!("{} appeared since the review; not overwriting it", dest.display()));
        }
        let cfg = ExportCfg {
            product: product.into(),
            manifest_dir: manifest_dir.display().to_string(),
            dest: dest.display().to_string(),
            container: v.container.clone(),
            codec: Some(v.codec.clone()),
            quality: Some(v.quality),
            fps: e.fps.unwrap_or(clip.fps),
            allow_partial: e.allow_partial,
        };
        let mut job = Ctx { clip, range, job_id: new_id(), staging: PathBuf::new() }.export(&cfg)?;
        job.clip_id = clip_id.into();
        job.validate()?;
        jobs.push(job);
    }
    Ok(jobs)
}

fn nested(a: &str, b: &str) -> bool {
    let (a, b) = (Path::new(a), Path::new(b));
    a.starts_with(b) || b.starts_with(a)
}

/// Cross-clip conflicts between frozen Key plans, as `(index of the later plan, message)`: a shared manifest
/// folder (or one inside another) or any output file written twice. Each later plan is reported once.
pub fn collisions<'a>(plans: impl IntoIterator<Item = (&'a str, &'a KeyJob)>) -> Vec<(usize, String)> {
    let (mut dirs, mut files): (Vec<(String, &str)>, BTreeMap<String, &str>) = (vec![], BTreeMap::new());
    let mut out = vec![];
    for (i, (who, j)) in plans.into_iter().enumerate() {
        let d = j.manifest_dir.to_string_lossy().to_lowercase();
        let mut hit = dirs
            .iter()
            .find(|(x, _)| nested(x, &d))
            .map(|(_, o)| format!("{who} and {o} would write the same result folder {}", j.manifest_dir.display()));
        dirs.push((d, who));
        for p in j.outputs.values().flatten() {
            if let Some(o) = files.insert(p.to_string_lossy().to_lowercase(), who) {
                if o != who && hit.is_none() {
                    hit = Some(format!("{who} and {o} would both write {}", p.display()));
                }
            }
        }
        out.extend(hit.map(|m| (i, m)));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::{Delivery, VideoDelivery};

    #[test]
    fn export_jobs_carry_the_key_clip_id_not_the_folder_name() {
        let root = std::env::temp_dir().join(format!("ck-export-jobs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = root.join("clips/c1");
        std::fs::create_dir_all(d.join("Frames")).unwrap();
        for i in 1..=4 {
            std::fs::write(d.join(format!("Frames/f{i}.png")), b"x").unwrap();
        }
        std::fs::write(d.join("clip.json"), "{}").unwrap();
        std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
        let clip = crate::project::load_project(&root, &crate::project::LoadOptions::default()).unwrap().clips.remove(0);
        assert!(clip.clip_id.is_none());
        let mut e = PipelineExport::default();
        e.outputs.clear();
        e.outputs.insert("fg".into(), Delivery::Video(VideoDelivery::default_for("mp4")));
        e.fps = Some(24.0);
        let dests = vec![("fg".to_string(), root.join("out.mp4"))];
        let jobs = export_jobs(&clip, "key-clip-id", (0, 3), &e, &d, &["fg".to_string()], &dests, || "job".into());
        let _ = std::fs::remove_dir_all(&root);
        let jobs = jobs.unwrap();
        assert_eq!(jobs.len(), 1);
        assert_eq!(jobs[0].clip_id, "key-clip-id");
    }

    /// A clip with 4 source frames, a pre-matte and a hint made by the very steps `hint_steps` plans, with the
    /// provenance record the worker writes for them.
    fn rendered(tag: &str) -> (PathBuf, Clip, Vec<TaskJob>) {
        use crate::hints::{Final, HintCfg, Producer};
        let root = std::env::temp_dir().join(format!("ck-offer-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let d = root.join("clips/c1");
        for sub in ["Frames", "VideoMamaMaskHint", "AlphaHint"] {
            std::fs::create_dir_all(d.join(sub)).unwrap();
        }
        for i in 1..=4 {
            std::fs::write(d.join(format!("Frames/f{i}.png")), format!("src{i}")).unwrap();
            std::fs::write(d.join(format!("VideoMamaMaskHint/f{i}.png")), format!("guide{i}")).unwrap();
            std::fs::write(d.join(format!("AlphaHint/f{i}.png")), format!("hint{i}")).unwrap();
        }
        std::fs::write(d.join("clip.json"), "{}").unwrap();
        std::fs::write(root.join("project.json"), r#"{"version":2}"#).unwrap();
        let clip = crate::project::load_project(&root, &crate::project::LoadOptions::default()).unwrap().clips.remove(0);
        let mut cfg = HintCfg::default();
        (cfg.guide, cfg.finish) = (Producer::Chroma, Final::Direct);
        let mut n = 0;
        let steps = hint_steps(&clip, &root, &cfg, "full", (0, 3), false, || {
            n += 1;
            format!("job{n}")
        })
        .unwrap()
        .steps;
        assert_eq!(steps.len(), 1, "a manual pre-matte plans only the hint step");
        let st = &steps[0];
        let hex = |f: &str| crate::sha256::hex(&std::fs::read(d.join(f)).unwrap());
        let stems: Vec<String> = (1..=4).map(|i| format!("f{i}")).collect();
        let rec = serde_json::json!({
            "kind": st.kind.wire(), "job_id": "old", "params": st.params, "stems": stems,
            "digests": stems.iter().map(|s| (s.clone(), hex(&format!("AlphaHint/{s}.png")))).collect::<BTreeMap<_, _>>(),
            "stats": stems.iter().map(|s| (s.clone(), crate::hints::stamp_value(&d.join(format!("AlphaHint/{s}.png"))))).collect::<BTreeMap<_, _>>(),
            "source": stems.iter().map(|s| (s.clone(), crate::hints::stamp_value(&d.join(format!("Frames/{s}.png"))))).collect::<BTreeMap<_, _>>(),
            "upstream": stems.iter().map(|s| (s.clone(), hex(&format!("VideoMamaMaskHint/{s}.png")))).collect::<BTreeMap<_, _>>(),
        });
        std::fs::write(d.join("AlphaHint/.corridorkey_hint.json"), json!({"kind": "prematte_hint", "ranges": [rec]}).to_string()).unwrap();
        (root, clip, steps)
    }
    use serde_json::json;

    #[test]
    fn a_hint_made_from_these_frames_with_these_settings_is_offered_as_compatible() {
        let (root, clip, steps) = rendered("ok");
        let o = hint_offer(&clip, &steps).unwrap();
        assert_eq!(o.compat, Compat::Compatible);
        assert_eq!(o.identity.len(), 4);
        // a wider recorded render serves the narrower range the queue asks for
        let sub: Vec<TaskJob> = vec![TaskJob { range: crate::contracts::RangeIdentity::from_inclusive(&["f1".into(), "f2".into(), "f3".into(), "f4".into()], 1, 2).ok(), ..steps[0].clone() }];
        assert_eq!(hint_offer(&clip, &sub).unwrap().compat, Compat::Compatible);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn known_differences_are_mismatches_old_hints_are_unrecorded_missing_frames_offer_nothing() {
        let (root, clip, steps) = rendered("diff");
        let d = root.join("clips/c1");
        let compat = |clip: &Clip| hint_offer(clip, &steps).map(|o| o.compat);
        let mut other = steps.clone();
        other[0].params["mask_cleanup"]["feather"] = json!(3.0);
        assert!(matches!(hint_offer(&clip, &other).unwrap().compat, Compat::Mismatch(w) if w.contains("settings")));
        std::fs::write(d.join("Frames/f2.png"), "replaced source!").unwrap();
        assert!(matches!(compat(&clip), Some(Compat::Mismatch(w)) if w.contains("source frame f2")));
        std::fs::write(d.join("Frames/f2.png"), "src2").unwrap(); // size equal again; mtime moved, so it still differs
        std::fs::write(d.join("VideoMamaMaskHint/f3.png"), "a new guide").unwrap();
        assert!(matches!(compat(&clip), Some(Compat::Mismatch(w)) if w.contains("pre-matte") || w.contains("source frame")));
        std::fs::write(d.join("AlphaHint/f1.png"), "edited by hand").unwrap();
        assert!(matches!(compat(&clip), Some(Compat::Mismatch(_))));
        std::fs::remove_file(d.join("AlphaHint/.corridorkey_hint.json")).unwrap();
        assert_eq!(compat(&clip), Some(Compat::Unrecorded)); // legacy / imported: the user decides
        std::fs::remove_file(d.join("AlphaHint/f4.png")).unwrap();
        assert!(compat(&clip).is_none());
        let _ = std::fs::remove_dir_all(root);
    }
    #[test]
    fn matanyone2_compares_only_the_in_frame_pre_matte() {
        let (root, clip, steps) = rendered("mat");
        let d = root.join("clips/c1");
        let mut mat = steps.clone();
        mat[0].kind = crate::tasks::TaskKind::Matanyone2;
        let path = d.join("AlphaHint/.corridorkey_hint.json");
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["ranges"][0]["kind"] = json!("matanyone2");
        v["ranges"][0]["params"] = mat[0].params.clone();
        let up = v["ranges"][0]["upstream"].as_object_mut().unwrap();
        up.retain(|k, _| k == "f1"); // what the worker records: the In frame only
        std::fs::write(&path, v.to_string()).unwrap();
        assert_eq!(hint_offer(&clip, &mat).unwrap().compat, Compat::Compatible, "a dense pre-matte is not an input");
        std::fs::write(d.join("VideoMamaMaskHint/f3.png"), "later frame edited").unwrap();
        assert_eq!(hint_offer(&clip, &mat).unwrap().compat, Compat::Compatible);
        std::fs::write(d.join("VideoMamaMaskHint/f1.png"), "the seed changed").unwrap();
        assert!(matches!(hint_offer(&clip, &mat).unwrap().compat, Compat::Mismatch(w) if w.contains("pre-matte f1")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn matanyone2_seeded_from_an_earlier_in_frame_is_not_compatible() {
        let (root, clip, steps) = rendered("seed");
        let d = root.join("clips/c1");
        let mut mat = steps.clone();
        mat[0].kind = crate::tasks::TaskKind::Matanyone2;
        let path = d.join("AlphaHint/.corridorkey_hint.json");
        let mut v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        v["ranges"][0]["kind"] = json!("matanyone2");
        v["ranges"][0]["params"] = mat[0].params.clone();
        v["ranges"][0]["upstream"].as_object_mut().unwrap().retain(|k, _| k == "f1"); // seeded from f1
        std::fs::write(&path, v.to_string()).unwrap();
        // the queue now starts at f2 and the sparse guide has no file there
        std::fs::remove_file(d.join("VideoMamaMaskHint/f2.png")).unwrap();
        mat[0].range = crate::contracts::RangeIdentity::from_inclusive(&["f1".into(), "f2".into(), "f3".into(), "f4".into()], 1, 3).ok();
        assert!(matches!(hint_offer(&clip, &mat).unwrap().compat, Compat::Mismatch(w) if w.contains("pre-matte f2")));
        let _ = std::fs::remove_dir_all(root);
    }

    /// The same record, checked by Rust here and by Python's `hint_compat.check` (the repo venv), must give the
    /// same verdict for each change to the render being asked for.
    #[test]
    fn rust_and_python_give_the_same_verdict_for_the_same_hint_record() {
        let root_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let Some(py) = [".venv/bin/python", ".venv/Scripts/python.exe"].iter().map(|p| root_dir.join(p)).find(|p| p.is_file()) else {
            eprintln!("SKIPPED parity test: no repo python at <repo>/.venv/bin/python or .venv/Scripts/python.exe");
            return;
        };
        let (root, _clip, steps) = rendered("parity");
        let d = root.join("clips/c1");
        let rec_path = d.join("AlphaHint/.corridorkey_hint.json");
        let rec_text = std::fs::read_to_string(&rec_path).unwrap();
        let stems: Vec<String> = (1..=4).map(|i| format!("f{i}")).collect();
        let hex = |f: &str| crate::sha256::hex(&std::fs::read(d.join(f)).unwrap());
        let want_json = |kind: &str, params: serde_json::Value, src: &str| {
            json!({
                "kind": kind, "params": params,
                "source": stems.iter().map(|s| (s.clone(), if s == "f1" { json!(src) } else { crate::hints::stamp_value(&d.join(format!("Frames/{s}.png"))) })).collect::<BTreeMap<_, _>>(),
                "upstream": stems.iter().map(|s| (s.clone(), hex(&format!("VideoMamaMaskHint/{s}.png")))).collect::<BTreeMap<_, _>>(),
            })
        };
        let kind = steps[0].kind.wire();
        let mut other = steps[0].params.clone();
        other.as_object_mut().unwrap().insert("zz_other".into(), json!(1));
        let src1 = crate::hints::stamp_value(&d.join("Frames/f1.png"));
        // each case: name, the wanted render, how the stored record is edited first, the verdict both must give
        type Edit = fn(&mut serde_json::Value);
        let mat_seed: Edit = |r| {
            r["ranges"][0]["kind"] = json!("matanyone2");
            r["ranges"][0]["upstream"].as_object_mut().unwrap().retain(|k, _| k == "f1"); // the In frame only
        };
        let mat_want = |hash: Option<&str>| {
            let mut w = want_json("matanyone2", steps[0].params.clone(), "");
            w["upstream"].as_object_mut().unwrap().retain(|k, _| k == "f1");
            if let Some(h) = hash {
                w["upstream"]["f1"] = json!(h);
            }
            w
        };
        let cases: Vec<(&str, serde_json::Value, Option<Edit>, &str)> = vec![
            ("same", want_json(kind, steps[0].params.clone(), src1.as_str().unwrap_or("")), None, "compatible"),
            ("kind", want_json("matanyone2", steps[0].params.clone(), ""), None, "mismatch"),
            ("settings", want_json(kind, other, ""), None, "mismatch"),
            ("source", want_json(kind, steps[0].params.clone(), "changed"), None, "mismatch"),
            ("settings not recorded", want_json(kind, steps[0].params.clone(), src1.as_str().unwrap_or("")),
                Some(|r| { r["ranges"][0].as_object_mut().unwrap().remove("params"); }), "mismatch"),
            ("missing pre-matte", want_json(kind, steps[0].params.clone(), src1.as_str().unwrap_or("")),
                Some(|r| { r["ranges"][0]["upstream"].as_object_mut().unwrap().remove("f2"); }), "mismatch"),
            ("matanyone2 seed unchanged", mat_want(None), Some(mat_seed), "compatible"),
            ("matanyone2 seed changed", mat_want(Some("0000")), Some(mat_seed), "mismatch"),
        ];
        for (name, mut w, edit, expect) in cases {
            if name == "same" || edit.is_some() {
                w["source"]["f1"] = src1.clone(); // the real stamp (an array, not a string)
            }
            let mut rec: serde_json::Value = serde_json::from_str(&rec_text).unwrap();
            if let Some(e) = edit {
                e(&mut rec);
            }
            std::fs::write(&rec_path, rec.to_string()).unwrap();
            let want = HintWant {
                kind: w["kind"].as_str().unwrap().into(),
                params: w["params"].clone(),
                source: w["source"].as_object().unwrap().clone().into_iter().collect(),
                upstream: w["upstream"].as_object().unwrap().iter().map(|(k, v)| (k.clone(), v.as_str().unwrap().into())).collect(),
                prompt_fp: None,
                guide: None,
            };
            let rust = match check_hint(&d.join("AlphaHint"), &stems, &want) {
                Compat::Compatible => "compatible",
                Compat::Unrecorded => "unrecorded",
                Compat::Mismatch(_) => "mismatch",
                Compat::Missing => "missing",
            };
            let out = std::process::Command::new(&py)
                .args(["-I", "-c", "import json,sys\nsys.path.insert(0,sys.argv[1])\nfrom backend import hint_compat as h\nprint(h.check(sys.argv[2],sys.argv[3].split(','),json.loads(sys.argv[4]))[0])"])
                .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/.."))
                .arg(d.join("AlphaHint"))
                .arg(stems.join(","))
                .arg(w.to_string())
                .output()
                .expect("run the repo python");
            let python = String::from_utf8_lossy(&out.stdout).trim().to_string();
            assert_eq!(python, rust, "case {name}: python said {python:?}, rust {rust:?}; stderr {}", String::from_utf8_lossy(&out.stderr));
            assert_eq!(rust, expect, "case {name}");
        }
        let _ = std::fs::remove_dir_all(root);
    }

    /// Writes the worker's record for every frame of `sub` of clip c1 (`upstream` = the pre-matte it was made from).
    fn record(d: &Path, sub: &str, kind: &str, params: &serde_json::Value, upstream: bool) {
        let stems: Vec<String> = (1..=4).map(|i| format!("f{i}")).collect();
        let by = |f: &dyn Fn(String) -> serde_json::Value| stems.iter().map(|s| (s.clone(), f(s.clone()))).collect::<BTreeMap<_, _>>();
        let file = |s: &str| d.join(format!("{sub}/{s}.png"));
        let mut rec = json!({
            "kind": kind, "job_id": "old", "params": params, "stems": stems,
            "digests": by(&|s| json!(crate::sha256::hex(&std::fs::read(file(&s)).unwrap()))),
            "stats": by(&|s| crate::hints::stamp_value(&file(&s))),
            "source": by(&|s| crate::hints::stamp_value(&d.join(format!("Frames/{s}.png")))),
        });
        if upstream {
            rec["upstream"] = json!(by(&|s| json!(crate::sha256::hex(&std::fs::read(d.join(format!("VideoMamaMaskHint/{s}.png"))).unwrap()))));
        }
        std::fs::write(d.join(sub).join(".corridorkey_hint.json"), json!({"kind": kind, "ranges": [rec]}).to_string()).unwrap();
    }

    /// The board plan (pre-matte promotion) over a clip whose hint was written by the producer directly (the Hint
    /// page's "Generate hint", Final = Direct), or the other way round.
    fn two_routes(tag: &str, direct_made: bool) -> (PathBuf, Clip, Vec<TaskJob>) {
        let (root, clip, steps) = rendered(tag);
        let d = root.join("clips/c1");
        let producer = json!({"key": "green", "mask_cleanup": steps[0].params["mask_cleanup"]});
        record(&d, "VideoMamaMaskHint", "chroma", &producer, false);
        if direct_made {
            record(&d, "AlphaHint", "chroma", &producer, false);
        }
        (root, clip, steps)
    }

    #[test]
    fn a_hint_made_directly_by_the_producer_is_reused_by_the_board_plan_and_changes_still_ask() {
        let (root, clip, steps) = two_routes("direct", true);
        let d = root.join("clips/c1");
        assert_eq!(hint_offer(&clip, &steps).unwrap().compat, Compat::Compatible);
        let mut feather = steps.clone();
        feather[0].params["mask_cleanup"]["feather"] = json!(3.0);
        assert!(matches!(hint_offer(&clip, &feather).unwrap().compat, Compat::Mismatch(_)), "other settings ask");
        std::fs::write(d.join("VideoMamaMaskHint/f3.png"), "a new pre-matte").unwrap();
        assert!(matches!(hint_offer(&clip, &steps).unwrap().compat, Compat::Mismatch(_)), "a changed pre-matte asks");
        std::fs::write(d.join("VideoMamaMaskHint/f3.png"), "guide3").unwrap(); // same bytes: certified again by hash
        assert_eq!(hint_offer(&clip, &steps).unwrap().compat, Compat::Compatible);
        std::fs::write(d.join("Frames/f2.png"), "replaced source!").unwrap();
        assert!(matches!(hint_offer(&clip, &steps).unwrap().compat, Compat::Mismatch(_)), "a changed source asks");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_plain_direct_generate_hint_plans_the_same_steps_as_the_board() {
        use crate::hints::{Final, Gen, HintCfg, Producer};
        let (root, clip, _) = rendered("plain");
        let d = root.join("clips/c1");
        std::fs::remove_dir_all(d.join("VideoMamaMaskHint")).unwrap(); // no pre-matte first
        let mut cfg = HintCfg::default();
        (cfg.guide, cfg.finish) = (Producer::Birefnet, Final::Direct);
        let stg = staging_root(&root, &clip.root, &clip.folder);
        let mut n = 0;
        let mut id = || { n += 1; format!("job{n}") };
        let page = Gen { clip: &clip, cfg: &cfg, ann: None, linear: false, staging: &stg }.hint((0, 3), false, &mut id).unwrap();
        let board = hint_steps(&clip, &root, &cfg, "full", (0, 3), false, || "b".to_string()).unwrap().steps;
        let shape = |v: &[TaskJob]| v.iter().map(|t| (t.kind.wire(), t.publish_dir.clone().map(|p| p.file_name().unwrap().to_owned()), t.params.clone())).collect::<Vec<_>>();
        assert_eq!(page.len(), 2, "pre-matte, then publish");
        assert_eq!(shape(&page), shape(&board), "both routes write the same records");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_hint_made_by_the_batch_route_is_recognised_for_the_direct_producer() {
        let (root, clip, steps) = two_routes("batch", false);
        let mut direct = steps.clone();
        direct[0].kind = crate::tasks::TaskKind::Chroma;
        direct[0].params = json!({"key": "green", "mask_cleanup": steps[0].params["mask_cleanup"]});
        assert_eq!(hint_offer(&clip, &direct).unwrap().compat, Compat::Compatible);
        direct[0].params["key"] = json!("blue");
        assert!(matches!(hint_offer(&clip, &direct).unwrap().compat, Compat::Mismatch(_)), "another producer setting asks");
        direct[0].kind = crate::tasks::TaskKind::Birefnet;
        assert!(matches!(hint_offer(&clip, &direct).unwrap().compat, Compat::Mismatch(_)), "another producer asks");
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_missing_pre_matte_is_offered_with_its_reason_not_silently_dropped() {
        let (root, clip, steps) = rendered("gone");
        let d = root.join("clips/c1");
        let mut guide = steps[0].clone();
        guide.publish_dir = Some(d.join(GUIDE_DIR));
        std::fs::remove_file(d.join("VideoMamaMaskHint/f2.png")).unwrap();
        let o = hint_offer(&clip, &[guide, steps[0].clone()]).unwrap();
        assert_eq!(o.compat, Compat::Unrecorded);
        assert!(o.note.unwrap().contains("pre-matte"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stems_with_a_tilde_and_dot_files_do_not_confuse_the_record_lookups() {
        let (root, _clip, _steps) = rendered("tilde");
        let d = root.join("clips/c1/AlphaHint");
        std::fs::write(d.join("sh~v2.0001.png"), "x").unwrap();
        std::fs::write(d.join(".hidden.png"), "x").unwrap();
        std::fs::write(d.join("f1.exr"), "x").unwrap();
        std::fs::write(d.join("f1.dpx"), "x").unwrap();
        let files = crate::hints::hint_files(&d);
        assert!(files.contains_key("sh~v2.0001") && !files.contains_key(".hidden"));
        assert_eq!(files["f1"].extension().unwrap(), "png", "dpx is ignored and the last sorted name wins, as in Python");
        let _ = std::fs::remove_dir_all(root);
    }
}
