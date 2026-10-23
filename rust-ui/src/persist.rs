//! Saving the Media state the Qt app also stores (In/Out, input colour) plus the native block `ck_native` in
//! `clip.json`. Every write goes through `project::update_json`: atomic, unknown keys preserved, an unreadable
//! file is left untouched and the error shown (never silently dropped).
use crate::project::{update_json, Clip};
use serde_json::{json, Map, Value};
use std::path::Path;

/// Atomic, unknown-key-preserving update of a clip's JSON (re-export of `project::update_json`).
pub fn update_json_pub(path: &Path, f: impl FnOnce(&mut Map<String, Value>)) -> Result<(), String> {
    update_json(path, f)
}

/// Set keys inside `ck_native` without touching its other keys.
pub fn update_native(meta: &Path, f: impl FnOnce(&mut Map<String, Value>)) -> Result<(), String> {
    update_json(meta, |m| {
        let e = m.entry("ck_native").or_insert_with(|| json!({}));
        if !e.is_object() {
            *e = json!({});
        }
        f(e.as_object_mut().unwrap());
    })
}

/// In/Out as `in_out_range` (Qt-compatible) plus the boundary stems (`ck_native.range_stems`) so a changed
/// source is detected on reload. `None` clears both.
pub fn save_range(
    clip: &Clip,
    range: Option<(usize, usize)>,
    stems: &[String],
) -> Result<(), String> {
    if let Some((a, b)) = range {
        if a > b || b >= stems.len() {
            return Err("range is outside the timeline".into());
        }
    }
    update_json(&clip.meta_path, |m| match range {
        Some((a, b)) => {
            m.insert(
                "in_out_range".into(),
                json!({"in_point": a, "out_point": b}),
            );
            let n = m.entry("ck_native").or_insert_with(|| json!({}));
            if !n.is_object() {
                *n = json!({});
            }
            n["range_stems"] = json!({"in": stems[a], "out": stems[b]});
        }
        None => {
            m.remove("in_out_range");
            if let Some(n) = m.get_mut("ck_native").and_then(Value::as_object_mut) {
                n.remove("range_stems");
            }
        }
    })
}

/// The render client this clip is assigned to (`ck_native.client`). `None` = Automatic.
pub fn clip_client(clip: &Clip) -> Option<String> {
    clip.native
        .get("client")
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Assign a clip to a configured client id, or `None` for Automatic. Only this key changes.
pub fn save_client(clip: &Clip, id: Option<&str>) -> Result<(), String> {
    update_native(&clip.meta_path, |n| match id {
        Some(id) => {
            n.insert("client".into(), json!(id));
        }
        None => {
            n.remove("client");
        }
    })
}

/// The Key hint-consumption mode the user picked for this clip (`ck_native.hint_mode`). Absent = nothing was
/// chosen explicitly, so defaults apply. Distinct from `workflow.params.hint_mode`, which a queued job froze.
pub fn clip_hint_mode(clip: &Clip) -> Option<String> {
    clip.native
        .get("hint_mode")
        .and_then(Value::as_str)
        .filter(|m| crate::settings::HINT_MODES.contains(m))
        .map(str::to_owned)
}

/// The user's own hint-mode pick: `clip_hint_mode` minus a value that only records what an applied preset
/// resolved to (`ck_native.hint_mode_src` = "preset"). Batch presets must not override this one.
pub fn explicit_hint_mode(clip: &Clip) -> Option<String> {
    clip_hint_mode(clip).filter(|_| clip.native.get("hint_mode_src").and_then(Value::as_str) != Some("preset"))
}

/// The preset the (not overridden) route/delivery was last applied from, for honest Reset labels.
pub fn write_src(meta: &Path, key: &str, preset: Option<&str>) -> Result<(), String> {
    update_native(meta, |n| match preset {
        Some(p) => {
            n.insert(format!("{key}_src"), json!(p));
        }
        None => {
            n.remove(&format!("{key}_src"));
        }
    })
}

/// What a hint-mode pick returns to on Reset: `(value, came from a preset)` as it was before the first pick.
pub fn hint_mode_base(clip: &Clip) -> Option<(String, bool)> {
    let b = clip.native.get("hint_mode_base")?;
    Some((b.get("value")?.as_str().filter(|m| crate::settings::HINT_MODES.contains(m))?.to_owned(), b.get("preset")?.as_bool()?))
}

/// The user's own pick. The first pick (not a re-pick) records `base`, the inherited mode it replaces.
pub fn save_hint_pick(clip: &Clip, mode: &str, base: (&str, bool)) -> Result<(), String> {
    if !crate::settings::HINT_MODES.contains(&mode) {
        return Err(format!("{mode} is not a hint mode"));
    }
    update_native(&clip.meta_path, |n| {
        let picked = n.contains_key("hint_mode") && n.get("hint_mode_src").and_then(Value::as_str) != Some("preset");
        n.insert("hint_mode".into(), json!(mode));
        n.remove("hint_mode_src");
        if !picked {
            n.insert("hint_mode_base".into(), json!({"value": base.0, "preset": base.1}));
        }
    })
}

/// A confirmed preset speaks for an existing pick's base: Reset now returns to that preset's mode.
pub fn set_hint_base(clip: &Clip, value: &str, preset: bool) -> Result<(), String> {
    update_native(&clip.meta_path, |n| {
        if n.contains_key("hint_mode_base") || n.contains_key("hint_mode") {
            n.insert("hint_mode_base".into(), json!({"value": value, "preset": preset}));
        }
    })
}

/// Persist the mode an applied preset resolved to (survives reload like a pick, but is not an override).
pub fn save_preset_hint_mode(clip: &Clip, mode: &str) -> Result<(), String> {
    save_hint_mode(clip, Some(mode))?;
    update_native(&clip.meta_path, |n| {
        n.insert("hint_mode_src".into(), json!("preset"));
        n.remove("hint_mode_base"); // no pick to return from
    })
}

/// Record (or forget, with `None`) this clip's explicit hint-consumption choice.
pub fn save_hint_mode(clip: &Clip, mode: Option<&str>) -> Result<(), String> {
    if let Some(m) = mode {
        if !crate::settings::HINT_MODES.contains(&m) {
            return Err(format!("{m} is not a hint mode"));
        }
    }
    update_native(&clip.meta_path, |n| match mode {
        Some(m) => {
            n.insert("hint_mode".into(), json!(m));
            n.remove("hint_mode_src");
        }
        None => {
            n.remove("hint_mode");
            n.remove("hint_mode_src");
            n.remove("hint_mode_base");
        }
    })
}

/// Input colour interpretation in the project session file (`clip_input_is_linear`, the key the Qt app uses).
pub fn save_colour(project_root: &Path, clip_name: &str, linear: bool) -> Result<(), String> {
    let candidates = [
        project_root.join(".corridorkey_session.json"),
        project_root.join("clips").join(".corridorkey_session.json"),
    ];
    let path = candidates
        .iter()
        .find(|p| p.is_file())
        .unwrap_or(&candidates[0]);
    update_json(path, |m| {
        let e = m.entry("clip_input_is_linear").or_insert_with(|| json!({}));
        if !e.is_object() {
            *e = json!({});
        }
        e[clip_name] = Value::Bool(linear);
    })
}

/// Sparse hint keyframes in the Qt-compatible sidecar (`.corridorkey_hint_keyframes.json`, version 1, exactly
/// `{index, stem}` records). Empty clears the file. Atomic; the indices are validated against `stems` first.
pub fn save_keyframes(clip_root: &Path, marks: &[usize], stems: &[String]) -> Result<(), String> {
    let path = clip_root.join(".corridorkey_hint_keyframes.json");
    let mut m: Vec<usize> = marks.to_vec();
    m.sort_unstable();
    m.dedup();
    if m.iter().any(|i| *i >= stems.len()) {
        return Err("a keyframe mark is outside the source frames".into());
    }
    if m.is_empty() {
        return match std::fs::remove_file(&path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("{}: {e}", path.display()))
            }
            _ => Ok(()),
        };
    }
    let v = json!({"version": 1, "keyframes": m.iter().map(|i| json!({"index": i, "stem": stems[*i]})).collect::<Vec<_>>()});
    let tmp = clip_root.join(format!(
        ".corridorkey_hint_keyframes.json.tmp{}",
        std::process::id()
    ));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).unwrap())
        .and_then(|()| std::fs::rename(&tmp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("{}: {e}", path.display())
        })
}

/// Point a clip whose external image sequence went missing at `folder`. Identity is verified, never guessed:
/// the stored In/Out boundary stems and the last result's source stems (when present) must all be found in
/// the new folder, and an empty or unsupported folder is refused. Only `source.original_path` changes.
pub fn relink_sequence(clip: &Clip, folder: &Path) -> Result<String, String> {
    let files = crate::project::list_images(folder);
    if files.is_empty() {
        return Err("that folder has no image sequence".into());
    }
    let have: std::collections::BTreeSet<&str> = files.iter().map(|f| f.0.as_str()).collect();
    let mut verified = false;
    if let Some(m) = clip.native.get("range_stems") {
        for k in ["in", "out"] {
            if let Some(s) = m.get(k).and_then(Value::as_str) {
                if !have.contains(s) {
                    return Err(format!("frame {s} (the stored In/Out) is not in that folder: it is a different source"));
                }
                verified = true;
            }
        }
    }
    if let Some(stems) = clip.manifest.as_ref().and_then(|m| m.source_stems.as_ref()) {
        if let Some(missing) = stems.iter().find(|s| !have.contains(s.as_str())) {
            return Err(format!("frame {missing} of the last result is not in that folder: it is a different source"));
        }
        verified = true;
    }
    let path = folder
        .to_str()
        .ok_or("the folder path is not valid UTF-8")?
        .to_owned();
    update_json(&clip.meta_path, |m| {
        let s = m.entry("source").or_insert_with(|| json!({}));
        if !s.is_object() {
            *s = json!({});
        }
        s["type"] = json!("sequence");
        s["original_path"] = json!(path);
    })?;
    Ok(if verified {
        format!(
            "Relinked to {} ({} frames); stored frame identity matched.",
            folder.display(),
            files.len()
        )
    } else {
        format!("Relinked to {} ({} frames). No stored frame identity existed to verify against: check the footage.", folder.display(), files.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::{load_project, LoadOptions};

    fn project(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ck-persist-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let clip = d.join("clips/c1");
        std::fs::create_dir_all(clip.join("Frames")).unwrap();
        for i in 0..5 {
            std::fs::write(clip.join(format!("Frames/f{i:03}.png")), b"x").unwrap();
        }
        std::fs::write(
            d.join("project.json"),
            r#"{"version":2,"clips":["c1"],"keep":1}"#,
        )
        .unwrap();
        std::fs::write(clip.join("clip.json"), r#"{"unknown":{"a":1}}"#).unwrap();
        d
    }

    #[test]
    fn range_and_colour_round_trip_keep_unknown_keys_and_detect_source_change() {
        let d = project("rt");
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let c = &p.clips[0];
        let stems: Vec<String> = c.input_files.iter().map(|f| f.0.clone()).collect();
        save_range(c, Some((1, 3)), &stems).unwrap();
        save_colour(&d, "c1", true).unwrap();
        update_native(&c.meta_path, |n| {
            n.insert("hint".into(), json!({"route": "gvm"}));
        })
        .unwrap();
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let c = &p.clips[0];
        assert_eq!(c.in_out, Some((1, 3)));
        assert!(c.input_is_linear);
        assert_eq!(c.native["hint"]["route"], "gvm");
        let raw: Value = serde_json::from_slice(&std::fs::read(&c.meta_path).unwrap()).unwrap();
        assert_eq!(raw["unknown"]["a"], 1);
        assert_eq!(raw["ck_native"]["range_stems"]["in"], "f001");
        // Insert a frame before: the stored boundary stems no longer match -> range dropped, with a note.
        std::fs::write(d.join("clips/c1/Frames/a000.png"), b"x").unwrap();
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        assert_eq!(p.clips[0].in_out, None);
        assert!(p.clips[0]
            .notes
            .iter()
            .any(|n| n.contains("Source changed")));
        // Clearing removes both.
        save_range(&p.clips[0], None, &[]).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&c.meta_path).unwrap()).unwrap();
        assert!(raw.get("in_out_range").is_none() && raw["ck_native"].get("range_stems").is_none());
        assert_eq!(raw["ck_native"]["hint"]["route"], "gvm");
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn keyframe_sidecar_round_trips_through_the_loader() {
        let d = project("kf");
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let c = &p.clips[0];
        let stems: Vec<String> = c.input_files.iter().map(|f| f.0.clone()).collect();
        save_keyframes(&c.root, &[3, 1, 1], &stems).unwrap();
        assert_eq!(
            load_project(&d, &LoadOptions::default()).unwrap().clips[0].keyframes,
            vec![1, 3]
        );
        assert!(save_keyframes(&c.root, &[9], &stems).is_err());
        save_keyframes(&c.root, &[], &stems).unwrap();
        assert!(load_project(&d, &LoadOptions::default()).unwrap().clips[0]
            .keyframes
            .is_empty());
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn relink_requires_matching_stored_identity_and_changes_only_the_source_path() {
        let d = project("relink");
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let c = &p.clips[0];
        let stems: Vec<String> = c.input_files.iter().map(|f| f.0.clone()).collect();
        save_range(c, Some((1, 3)), &stems).unwrap();
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        let c = &p.clips[0];
        let wrong = d.join("other");
        std::fs::create_dir_all(&wrong).unwrap();
        for i in 10..15 {
            std::fs::write(wrong.join(format!("g{i}.png")), b"x").unwrap();
        }
        assert!(relink_sequence(c, &wrong)
            .unwrap_err()
            .contains("different source"));
        assert!(relink_sequence(c, &d.join("nothing")).is_err());
        let right = d.join("moved");
        std::fs::create_dir_all(&right).unwrap();
        for s in &stems {
            std::fs::write(right.join(format!("{s}.png")), b"x").unwrap();
        }
        assert!(relink_sequence(c, &right)
            .unwrap()
            .contains("identity matched"));
        let raw: Value = serde_json::from_slice(&std::fs::read(&c.meta_path).unwrap()).unwrap();
        assert_eq!(raw["source"]["original_path"], right.to_str().unwrap());
        assert_eq!(raw["unknown"]["a"], 1);
        assert_eq!(raw["in_out_range"]["in_point"], 1);
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn unreadable_clip_json_is_never_replaced() {
        let d = project("bad");
        let p = load_project(&d, &LoadOptions::default()).unwrap();
        std::fs::write(&p.clips[0].meta_path, "[1,2").unwrap();
        assert!(update_native(&p.clips[0].meta_path, |_| {}).is_err());
        assert_eq!(std::fs::read(&p.clips[0].meta_path).unwrap(), b"[1,2");
        let _ = std::fs::remove_dir_all(d);
    }
}
