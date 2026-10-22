//! Workflow and export presets (`presets.json` next to the preferences). A preset is a field merge: applying
//! one changes only the fields it carries and never source-bound state (In/Out, input colour, prompts, marks,
//! output locations, file names). Unknown keys in the file survive a save; an unreadable file is kept aside.
use crate::workflow::{OutputCfg, Params};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Preset {
    pub name: String,
    /// `workflow` or `export`.
    pub kind: String,
    pub data: Value,
}

pub fn file_beside(settings_path: Option<&Path>) -> Option<PathBuf> {
    settings_path
        .and_then(Path::parent)
        .map(|d| d.join("presets.json"))
}

pub fn load(path: &Path) -> (Vec<Preset>, Option<String>) {
    let b = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (vec![], None),
        Err(e) => return (vec![], Some(format!("{}: {e}", path.display()))),
    };
    let v: Option<Value> = serde_json::from_slice(&b).ok();
    match v
        .as_ref()
        .and_then(|v| v.get("presets"))
        .and_then(|p| serde_json::from_value::<Vec<Preset>>(p.clone()).ok())
    {
        Some(p) => (p, None),
        None => {
            let kept = path.with_extension("json.unreadable");
            let _ = std::fs::rename(path, &kept);
            (
                vec![],
                Some(format!(
                    "presets file unreadable; kept as {}",
                    kept.display()
                )),
            )
        }
    }
}

pub fn save(path: &Path, presets: &[Preset]) -> Result<(), String> {
    let mut obj: Map<String, Value> = std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    obj.insert("version".into(), json!(1));
    obj.insert(
        "presets".into(),
        serde_json::to_value(presets).map_err(|e| e.to_string())?,
    );
    let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec_pretty(&Value::Object(obj)).unwrap(),
    )
    .and_then(|()| std::fs::rename(&tmp, path))
    .map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })
}

/// What a workflow preset captures: engine parameters and the four outputs; NOT the input colour.
pub fn workflow_data(p: &Params, o: &OutputCfg) -> Value {
    let mut params = serde_json::to_value(p).unwrap();
    params.as_object_mut().unwrap().remove("input_is_linear");
    json!({"params": params, "outputs": o})
}

/// Field-merge a workflow preset into a draft. Validated before anything is changed.
pub fn apply_workflow(data: &Value, p: &mut Params, o: &mut OutputCfg) -> Result<(), String> {
    let merge = |base: Value, over: Option<&Value>| -> Value {
        let mut b = base;
        if let (Some(bo), Some(Value::Object(oo))) = (b.as_object_mut(), over) {
            for (k, v) in oo {
                bo.insert(k.clone(), v.clone());
            }
        }
        b
    };
    let mut over = data.get("params").cloned();
    if let Some(Value::Object(m)) = &mut over {
        m.remove("input_is_linear"); // never source-bound
    }
    let np: Params =
        serde_json::from_value(merge(serde_json::to_value(&*p).unwrap(), over.as_ref()))
            .map_err(|e| e.to_string())?;
    let no: OutputCfg = serde_json::from_value(merge(
        serde_json::to_value(&*o).unwrap(),
        data.get("outputs"),
    ))
    .map_err(|e| e.to_string())?;
    np.validate()?;
    no.validate()?;
    (*p, *o) = (np, no);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workflow_preset_merges_fields_and_never_touches_colour() {
        let mut p = Params {
            input_is_linear: true,
            despill_strength: 0.9,
            ..Params::default()
        };
        let mut o = OutputCfg::default();
        let saved = workflow_data(
            &Params {
                input_is_linear: false,
                hint_mode: "full".into(),
                ..Params::default()
            },
            &OutputCfg {
                fg_enabled: false,
                ..OutputCfg::default()
            },
        );
        assert!(saved["params"].get("input_is_linear").is_none());
        apply_workflow(&saved, &mut p, &mut o).unwrap();
        assert!(p.input_is_linear, "source-bound colour is kept");
        assert_eq!(p.hint_mode, "full");
        assert!(!o.fg_enabled);
        // A partial preset changes only what it names.
        let mut p2 = p.clone();
        apply_workflow(
            &json!({"params": {"despill_strength": 0.2}}),
            &mut p2,
            &mut o,
        )
        .unwrap();
        assert_eq!((p2.hint_mode.as_str(), p2.despill_strength), ("full", 0.2));
        // Invalid content changes nothing.
        let mut p3 = p.clone();
        assert!(
            apply_workflow(&json!({"params": {"hint_mode": "bogus"}}), &mut p3, &mut o).is_err()
        );
        assert_eq!(p3, p);
    }

    #[test]
    fn file_keeps_unknown_keys_and_sets_aside_garbage() {
        let d = std::env::temp_dir().join(format!("ck-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("presets.json");
        std::fs::write(&f, r#"{"version":1,"presets":[],"future":{"a":1}}"#).unwrap();
        let ps = vec![Preset {
            name: "x".into(),
            kind: "export".into(),
            data: json!({"container": "mp4"}),
        }];
        save(&f, &ps).unwrap();
        let raw: Value = serde_json::from_slice(&std::fs::read(&f).unwrap()).unwrap();
        assert_eq!(raw["future"]["a"], 1);
        assert_eq!(load(&f).0, ps);
        std::fs::write(&f, "garbage").unwrap();
        let (got, err) = load(&f);
        assert!(got.is_empty() && err.is_some() && d.join("presets.json.unreadable").is_file());
        let _ = std::fs::remove_dir_all(d);
    }
}
