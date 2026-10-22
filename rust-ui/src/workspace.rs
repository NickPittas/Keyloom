//! Per-project workspace state (`project.json["ck_native"]["workspace"]`): open clip, page, node, view, frame,
//! A/B, Board selection and the node-graph layout of each clip. Restoring is always checked against the clips
//! that exist now: unknown clips, stems and malformed graph numbers are dropped, never applied blindly.
use crate::project::update_json;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphState {
    pub pan: [f32; 2],
    pub zoom: f32,
    /// Node positions in graph units, in node order.
    pub nodes: Vec<[f32; 2]>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Workspace {
    pub open: Option<String>,
    pub tab: String,
    pub node: String,
    pub view: String,
    pub ab: Option<String>,
    /// Playhead as a frame stem (positions shift when frames are added; stems do not).
    pub frame: Option<String>,
    pub selected: Vec<String>,
    pub graph_collapsed: bool,
    pub graphs: BTreeMap<String, GraphState>,
}

impl Workspace {
    pub fn load(project_root: &Path) -> Option<Self> {
        let b = std::fs::read(project_root.join("project.json")).ok()?;
        let v: Value = serde_json::from_slice(&b).ok()?;
        let mut ws = v.get("ck_native")?.get("workspace")?.clone();
        // One damaged graph must not discard the rest: graphs are parsed one by one.
        let graphs = ws.as_object_mut()?.remove("graphs");
        let mut w: Workspace = serde_json::from_value(ws).ok()?;
        for (k, g) in graphs
            .as_ref()
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if let Ok(g) = serde_json::from_value::<GraphState>(g.clone()) {
                w.graphs.insert(k.clone(), g);
            }
        }
        Some(w)
    }

    /// Atomic, other `project.json` keys (and other `ck_native` keys) preserved.
    pub fn save(&self, project_root: &Path) -> Result<(), String> {
        let v = serde_json::to_value(self).map_err(|e| e.to_string())?;
        update_json(&project_root.join("project.json"), |m| {
            let n = m.entry("ck_native").or_insert_with(|| json!({}));
            if !n.is_object() {
                *n = json!({});
            }
            n["workspace"] = v;
        })
    }

    /// Keep only what is valid for the clips that exist now.
    pub fn sanitized(mut self, clips: &[String]) -> Self {
        let known = |c: &String| clips.contains(c);
        if self.open.as_ref().is_some_and(|o| !known(o)) {
            self.open = None;
        }
        self.selected.retain(known);
        self.graphs.retain(|k, g| {
            known(k)
                && g.zoom.is_finite()
                && (0.05..=20.0).contains(&g.zoom)
                && g.pan.iter().all(|x| x.is_finite() && x.abs() < 1.0e5)
                && g.nodes
                    .iter()
                    .all(|p| p.iter().all(|x| x.is_finite() && x.abs() < 1.0e5))
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_keeps_unknown_keys_and_drops_what_no_longer_exists() {
        let d = std::env::temp_dir().join(format!("ck-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("project.json"),
            r#"{"version":2,"keep":1,"ck_native":{"other":true}}"#,
        )
        .unwrap();
        let w = Workspace {
            open: Some("gone".into()),
            tab: "Hint".into(),
            selected: vec!["a".into(), "gone".into()],
            graphs: [
                (
                    "a".to_string(),
                    GraphState {
                        pan: [1.0, 2.0],
                        zoom: 1.5,
                        nodes: vec![[0.0, 0.0]],
                    },
                ),
                (
                    "b".to_string(),
                    GraphState {
                        pan: [f32::NAN, 0.0],
                        zoom: 1.0,
                        nodes: vec![],
                    },
                ),
                (
                    "gone".to_string(),
                    GraphState {
                        pan: [0.0, 0.0],
                        zoom: 1.0,
                        nodes: vec![],
                    },
                ),
            ]
            .into(),
            ..Default::default()
        };
        w.save(&d).unwrap();
        let raw: Value =
            serde_json::from_slice(&std::fs::read(d.join("project.json")).unwrap()).unwrap();
        assert_eq!(
            (raw["keep"].as_i64(), raw["ck_native"]["other"].as_bool()),
            (Some(1), Some(true))
        );
        let got = Workspace::load(&d)
            .unwrap()
            .sanitized(&["a".into(), "b".into()]);
        assert_eq!(got.open, None);
        assert_eq!(got.selected, vec!["a".to_string()]);
        assert_eq!(
            got.graphs.keys().collect::<Vec<_>>(),
            vec!["a"],
            "stale and non-finite graphs are dropped"
        );
        let _ = std::fs::remove_dir_all(d);
    }
}
