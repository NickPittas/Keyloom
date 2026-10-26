//! Source relink and companion-sequence import, pure and testable: identity checks come before any write, and
//! nothing that already exists is overwritten.
use crate::persist::update_json_pub;
use crate::project::{list_images, Clip};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// What the clip remembers about its movie (`.video_metadata.json`, written at extraction).
#[derive(Debug, Clone, PartialEq)]
pub struct Identity {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: Option<u64>,
}

/// What a probe of the candidate movie returned.
#[derive(Debug, Clone, PartialEq)]
pub struct Probed {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub frames: Option<u64>,
}

pub fn stored_identity(clip_root: &Path) -> Option<Identity> {
    let v: Value =
        serde_json::from_slice(&std::fs::read(clip_root.join(".video_metadata.json")).ok()?)
            .ok()?;
    Some(Identity {
        width: v.get("width")?.as_u64()? as u32,
        height: v.get("height")?.as_u64()? as u32,
        fps: v.get("fps")?.as_f64()?,
        frames: v
            .get("source_probe")
            .and_then(|p| p.get("frame_count"))
            .and_then(Value::as_u64)
            .filter(|n| *n > 0)
            .or_else(|| {
                v.get("frame_count")
                    .and_then(Value::as_u64)
                    .filter(|n| *n > 0)
            }),
    })
}

/// `Ok(true)` = same geometry, rate and frame count. `Err` = a different movie (never extracted).
pub fn compare(stored: &Identity, p: &Probed) -> Result<bool, String> {
    if (stored.width, stored.height) != (p.width, p.height) {
        return Err(format!(
            "this movie is {}x{} but the clip was {}x{}: it is a different source",
            p.width, p.height, stored.width, stored.height
        ));
    }
    if (stored.fps - p.fps).abs() > 0.001 {
        return Err(format!(
            "this movie runs at {:.3} fps but the clip was {:.3} fps",
            p.fps, stored.fps
        ));
    }
    if let (Some(a), Some(b)) = (stored.frames, p.frames) {
        if a != b {
            return Err(format!(
                "this movie has {b} frames but the clip had {a}: it is a different source"
            ));
        }
    }
    Ok(stored.frames.is_some() && p.frames.is_some())
}

/// Point the clip at `movie` (referenced in place, not copied). Only `source.*` changes; other keys survive.
pub fn point_at_movie(clip: &Clip, movie: &Path) -> Result<(), String> {
    let path = std::path::absolute(movie).map_err(|e| e.to_string())?;
    let (p, name) = (
        path.to_str()
            .ok_or("the movie path is not valid UTF-8")?
            .to_owned(),
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    update_json_pub(&clip.meta_path, |m| {
        let s = m.entry("source").or_insert_with(|| json!({}));
        if !s.is_object() {
            *s = json!({});
        }
        s["original_path"] = json!(p);
        s["filename"] = json!(name);
        s["copied"] = json!(false);
    })
}

// ---- exclusive hint ownership while an import writes ---------------------------------------------

/// `<clip>/.corridorkey_hint.lock` created exclusively (`O_EXCL`) with the worker's lock schema, held for the
/// whole import: a worker hint job on the clip is refused (`busy`) and a second importer cannot start. Released
/// only if the file is still the one we created. A crash leaves it for the normal inspect/clear recovery.
pub struct ImportLock {
    path: PathBuf,
    job_id: String,
    ident: Option<(u64, u64)>,
}

fn file_ident(m: &std::fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = m;
        None
    }
}

/// Linux process facts the worker uses to prove a lock's owner alive or dead (same as `key_job._boot_id`,
/// `_proc_start_ticks`, `task_job._pid_ns`); `None` elsewhere, where the worker can prove nothing either.
fn proc_facts() -> (Option<String>, Option<u64>, Option<String>) {
    let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| s.rsplit_once(')').and_then(|(_, r)| r.split_whitespace().nth(19).and_then(|t| t.parse().ok())));
    let ns = std::fs::read_link("/proc/self/ns/pid").ok().map(|p| p.to_string_lossy().into_owned());
    (boot, ticks, ns)
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .or_else(|| std::env::var("HOSTNAME").ok())
        .unwrap_or_default()
}

impl ImportLock {
    pub fn acquire(clip_root: &Path) -> Result<Self, String> {
        use std::io::Write;
        let path = clip_root.join(".corridorkey_hint.lock");
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let job_id = format!(
            "import-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        );
        let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                "this clip's hints are locked by a running or interrupted hint job; finish or recover it first".to_string()
            } else {
                format!("{}: {e}", path.display())
            }
        })?;
        let (boot_id, proc_start_ticks, pid_ns) = proc_facts();
        let body = serde_json::json!({"schema":1,"job_id":job_id,"host":hostname(),
            "platform": if cfg!(windows) {"win32"} else {"linux"}, "pid": std::process::id(),
            "boot_id": boot_id, "proc_start_ticks": proc_start_ticks, "pid_ns": pid_ns,
            "started": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())});
        let ident = f.metadata().ok().and_then(|m| file_ident(&m));
        f.write_all(body.to_string().as_bytes()).map_err(|e| {
            let _ = std::fs::remove_file(&path);
            format!("{}: {e}", path.display())
        })?;
        Ok(Self {
            path,
            job_id,
            ident,
        })
    }
}

impl Drop for ImportLock {
    fn drop(&mut self) {
        let mine = std::fs::symlink_metadata(&self.path)
            .ok()
            .is_some_and(|m| m.is_file() && file_ident(&m) == self.ident)
            && std::fs::read(&self.path)
                .ok()
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some_and(|v| v.get("job_id").and_then(Value::as_str) == Some(&self.job_id));
        if mine {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

// ---- companion image sequences ------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub struct SeqPlan {
    pub dest: PathBuf,
    /// (source file, destination file name) for frames that will be written.
    pub copy: Vec<(PathBuf, String)>,
    /// Frames the destination already has: kept, never replaced.
    pub kept: Vec<String>,
}

fn dims(p: &Path) -> Result<(u32, u32), String> {
    match crate::project::ext_of(p).as_str() {
        "png" => {
            let d = png::Decoder::new(std::io::BufReader::new(
                std::fs::File::open(p).map_err(|e| format!("{}: {e}", p.display()))?,
            ));
            let r = d.read_info().map_err(|e| format!("{}: {e}", p.display()))?;
            let i = r.info();
            Ok((i.width, i.height))
        }
        "exr" => {
            let m = exr::meta::MetaData::read_from_file(p, false)
                .map_err(|e| format!("{}: {e}", p.display()))?;
            let s = m.headers.first().ok_or("EXR without a header")?.layer_size;
            Ok((s.width() as u32, s.height() as u32))
        }
        other => Err(format!(
            "{}: .{other} is not supported (PNG or EXR)",
            p.display()
        )),
    }
}

/// Validate a folder of alpha/mask frames against the clip's source and plan the copy. Every incoming frame
/// must be PNG/EXR, match a source frame stem, have the source's size, and cover the whole In/Out range
/// (so a stray folder can't half-define a hint). Frames the destination already has are kept untouched.
pub fn plan_sequence(
    clip: &Clip,
    folder: &Path,
    mask: bool,
    range: (usize, usize),
) -> Result<SeqPlan, String> {
    let incoming = list_images(folder);
    if incoming.is_empty() {
        return Err("that folder has no images".into());
    }
    let src: Vec<&str> = clip.input_files.iter().map(|f| f.0.as_str()).collect();
    if src.is_empty() {
        return Err("the clip has no source frames yet (extract the movie first)".into());
    }
    let have: std::collections::BTreeMap<&str, &PathBuf> =
        incoming.iter().map(|(s, p)| (s.as_str(), p)).collect();
    if have.len() != incoming.len() {
        return Err("two files share one frame number (for example a .png and a .exr)".into());
    }
    if let Some(bad) = have.keys().find(|s| !src.contains(*s)) {
        return Err(format!(
            "frame {bad} matches no source frame: the sequence does not belong to this clip"
        ));
    }
    if let Some(miss) = src[range.0..=range.1]
        .iter()
        .find(|s| !have.contains_key(**s))
    {
        return Err(format!(
            "frame {miss} of In/Out is missing from that folder"
        ));
    }
    let want = clip.sample_size;
    for (stem, path) in &have {
        let d = dims(path)?;
        if let Some(w) = want.filter(|w| *w != d) {
            return Err(format!(
                "frame {stem} is {}x{} but the source is {}x{}",
                d.0, d.1, w.0, w.1
            ));
        }
    }
    let dest = clip.root.join(if mask {
        "VideoMamaMaskHint"
    } else {
        "AlphaHint"
    });
    if std::fs::symlink_metadata(&dest).is_ok_and(|m| !m.is_dir()) {
        return Err(format!(
            "{} is not a plain folder; left untouched",
            dest.display()
        ));
    }
    let existing: std::collections::BTreeSet<String> =
        list_images(&dest).into_iter().map(|f| f.0).collect();
    let (mut copy, mut kept) = (vec![], vec![]);
    for (stem, path) in have {
        if existing.contains(stem) {
            kept.push(stem.to_owned());
        } else {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            copy.push((path.clone(), name));
        }
    }
    if copy.is_empty() {
        return Err("every frame already exists in the clip's hint folder; nothing would be written (existing frames are never replaced)".into());
    }
    Ok(SeqPlan { dest, copy, kept })
}

/// Write the planned frames: each through a hidden temp file and a no-clobber hard link, so a crash leaves no
/// partial frame and an existing file is never replaced. Returns the number written.
pub fn run_sequence(
    plan: &SeqPlan,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<usize, String> {
    std::fs::create_dir_all(&plan.dest).map_err(|e| format!("{}: {e}", plan.dest.display()))?;
    let mut done = 0;
    for (from, name) in &plan.copy {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(format!(
                "cancelled after {done} frame(s); frames already written stay"
            ));
        }
        let tmp = plan.dest.join(format!(".{name}.tmp"));
        let fin = plan.dest.join(name);
        std::fs::copy(from, &tmp).map_err(|e| format!("{}: {e}", from.display()))?;
        let r = std::fs::hard_link(&tmp, &fin);
        let _ = std::fs::remove_file(&tmp);
        r.map_err(|e| {
            format!(
                "{} was not written ({e}); existing files are never replaced",
                fin.display()
            )
        })?;
        done += 1;
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{load_project, LoadOptions};

    fn png(p: &Path, w: u32, h: u32) {
        let f = std::fs::File::create(p).unwrap();
        let mut e = png::Encoder::new(f, w, h);
        e.set_color(png::ColorType::Grayscale);
        e.set_depth(png::BitDepth::Eight);
        e.write_header()
            .unwrap()
            .write_image_data(&vec![5; (w * h) as usize])
            .unwrap();
    }

    fn project(tag: &str) -> (PathBuf, Clip) {
        let d = std::env::temp_dir().join(format!("ck-relink-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let c = d.join("clips/c1");
        std::fs::create_dir_all(c.join("Frames")).unwrap();
        for i in 0..4 {
            png(&c.join(format!("Frames/f_{}.png", 1101 + i)), 4, 4);
        }
        std::fs::write(d.join("project.json"), r#"{"version":2}"#).unwrap();
        std::fs::write(
            c.join("clip.json"),
            r#"{"source":{"filename":"a.mov","original_path":"/gone/a.mov"},"keep":1}"#,
        )
        .unwrap();
        std::fs::write(
            c.join(".video_metadata.json"),
            r#"{"fps":24,"width":4,"height":4,"frame_count":4}"#,
        )
        .unwrap();
        let clip = load_project(&d, &LoadOptions::default())
            .unwrap()
            .clips
            .remove(0);
        (d, clip)
    }

    #[test]
    fn movie_identity_is_compared_before_anything_is_written() {
        let (d, clip) = project("id");
        let id = stored_identity(&clip.root).unwrap();
        assert_eq!((id.width, id.fps, id.frames), (4, 24.0, Some(4)));
        let same = Probed {
            width: 4,
            height: 4,
            fps: 24.0,
            frames: Some(4),
        };
        assert_eq!(compare(&id, &same), Ok(true));
        assert!(compare(
            &id,
            &Probed {
                width: 8,
                ..same.clone()
            }
        )
        .unwrap_err()
        .contains("different source"));
        assert!(compare(
            &id,
            &Probed {
                fps: 25.0,
                ..same.clone()
            }
        )
        .unwrap_err()
        .contains("fps"));
        assert!(compare(
            &id,
            &Probed {
                frames: Some(5),
                ..same.clone()
            }
        )
        .unwrap_err()
        .contains("5 frames"));
        assert_eq!(
            compare(
                &id,
                &Probed {
                    frames: None,
                    ..same
                }
            ),
            Ok(false),
            "frame count unverifiable"
        );
        point_at_movie(&clip, Path::new("/new/place/b.mov")).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&clip.meta_path).unwrap()).unwrap();
        assert_eq!(
            (
                raw["source"]["original_path"].as_str(),
                raw["keep"].as_i64()
            ),
            (Some("/new/place/b.mov"), Some(1))
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn import_lock_is_exclusive_blocks_the_worker_schema_and_is_released_only_if_ours() {
        let (d, clip) = project("lock");
        let l = ImportLock::acquire(&clip.root).unwrap();
        let raw: Value = serde_json::from_slice(
            &std::fs::read(clip.root.join(".corridorkey_hint.lock")).unwrap(),
        )
        .unwrap();
        assert!(
            raw["job_id"].as_str().unwrap().starts_with("import-")
                && raw["pid"].as_u64() == Some(std::process::id() as u64)
        );
        assert!(
            ImportLock::acquire(&clip.root).is_err(),
            "a second importer (or a hint job's lock) excludes it"
        );
        drop(l);
        assert!(!clip.root.join(".corridorkey_hint.lock").exists());
        // Someone else replaced the lock: ours is not removed.
        let l = ImportLock::acquire(&clip.root).unwrap();
        std::fs::remove_file(clip.root.join(".corridorkey_hint.lock")).unwrap();
        std::fs::write(
            clip.root.join(".corridorkey_hint.lock"),
            r#"{"job_id":"other"}"#,
        )
        .unwrap();
        drop(l);
        assert!(clip.root.join(".corridorkey_hint.lock").exists());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn sequence_import_validates_stems_size_and_range_and_never_replaces() {
        let (d, clip) = project("seq");
        let inc = d.join("matte");
        std::fs::create_dir_all(&inc).unwrap();
        for i in 0..4 {
            png(&inc.join(format!("f_{}.png", 1101 + i)), 4, 4);
        }
        // Wrong size / foreign stem / missing range frame are all refused before any write.
        let bad = d.join("bad");
        std::fs::create_dir_all(&bad).unwrap();
        png(&bad.join("f_1101.png"), 8, 8);
        assert!(plan_sequence(&clip, &bad, false, (0, 0))
            .unwrap_err()
            .contains("8x8"));
        let foreign = d.join("foreign");
        std::fs::create_dir_all(&foreign).unwrap();
        png(&foreign.join("zzz_1.png"), 4, 4);
        assert!(plan_sequence(&clip, &foreign, false, (0, 0))
            .unwrap_err()
            .contains("matches no source frame"));
        let partial = d.join("partial");
        std::fs::create_dir_all(&partial).unwrap();
        png(&partial.join("f_1101.png"), 4, 4);
        assert!(plan_sequence(&clip, &partial, false, (0, 3))
            .unwrap_err()
            .contains("missing"));
        assert!(!clip.root.join("AlphaHint").exists());
        // Good set: writes all four; a second import has nothing new and replaces nothing.
        let plan = plan_sequence(&clip, &inc, false, (0, 3)).unwrap();
        assert_eq!(plan.copy.len(), 4);
        let cancel = std::sync::atomic::AtomicBool::new(false);
        assert_eq!(run_sequence(&plan, &cancel).unwrap(), 4);
        assert!(clip.root.join("AlphaHint/f_1102.png").is_file());
        assert!(std::fs::read_dir(clip.root.join("AlphaHint"))
            .unwrap()
            .all(|e| !e.unwrap().file_name().to_string_lossy().starts_with('.')));
        let before = std::fs::read(clip.root.join("AlphaHint/f_1101.png")).unwrap();
        png(&inc.join("f_1101.png"), 4, 4);
        assert!(plan_sequence(&clip, &inc, false, (0, 3))
            .unwrap_err()
            .contains("never replaced"));
        assert_eq!(
            std::fs::read(clip.root.join("AlphaHint/f_1101.png")).unwrap(),
            before
        );
        // Mask import goes to its own folder; a hint frame that appears later is kept, not overwritten.
        let m = plan_sequence(&clip, &inc, true, (0, 3)).unwrap();
        assert!(m.dest.ends_with("VideoMamaMaskHint"));
        std::fs::create_dir_all(&m.dest).unwrap();
        std::fs::write(m.dest.join("f_1101.png"), b"mine").unwrap();
        assert!(matches!(run_sequence(&m, &cancel), Err(e) if e.contains("never replaced")));
        assert_eq!(std::fs::read(m.dest.join("f_1101.png")).unwrap(), b"mine");
        let _ = std::fs::remove_dir_all(d);
    }
}
