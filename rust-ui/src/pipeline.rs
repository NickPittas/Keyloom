//! Full-pipeline batch planning (pre-matte -> Key -> video export), pure and UI-free. Every clip gets the preset's
//! settings but its own media, range, prompts, masks and hints: automatic producers/refiners are re-run (a stale
//! hint is never reused), manual SAM guides and imported alphas must already exist on that clip.
use crate::hints::{
    covers, staging_root, Annotations, Ctx, ExportCfg, Final, HintCfg, Producer, CONTAINERS, GUIDE_DIR, HINT_DIR,
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
}
