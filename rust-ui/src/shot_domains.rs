//! Per-field shot overrides of the two domains that live outside the Key draft: the hint route (`HintCfg`) and the
//! video delivery (`PipelineExport`). A shot's marker lists exactly the fields the user changed by hand, each with
//! the value it had before its first edit (its Reset base) and the preset that value came from. Batch merges
//! "preset base + the shot's marked fields only". Groups that cannot sensibly be mixed are marked together (the
//! producer/refiner/model group of the route); everything else is independent (bypass, each chroma field, each mask
//! cleanup field, each output's delivery, fps, partial flag, folder). Stored at `ck_native.route_override` /
//! `ck_native.delivery_override` as `{"fields": {key: {"base": value, "preset": name|null}}}`; the earlier
//! whole-domain marker `{"base": whole value, "preset"}` is read as "every field of the domain".
use crate::hints::HintCfg;
use crate::persist::update_native;
use crate::presets::PipelineExport;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub base: Value,
    pub preset: Option<String>,
}
pub type Fields = BTreeMap<String, Field>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Domain {
    Route,
    Delivery,
}
impl Domain {
    pub fn key(self) -> &'static str {
        match self {
            Domain::Route => "route_override",
            Domain::Delivery => "delivery_override",
        }
    }
    pub fn read(self, native: &Value) -> Fields {
        read_fields(native, self)
    }
    pub fn keys(self) -> &'static [&'static str] {
        match self {
            Domain::Route => &ROUTE_KEYS,
            Domain::Delivery => &EXPORT_KEYS,
        }
    }
}

/// Producer, refiner and their models/parameters: one choice of route, marked together.
pub const ROUTE_GROUP: [&str; 5] = ["guide", "final", "sam_model", "birefnet_variant", "videomama_chunk"];
const ROUTE_KEYS: [&str; 16] = [
    "guide",
    "final",
    "sam_model",
    "birefnet_variant",
    "videomama_chunk",
    "bypass",
    "chroma.screen_type",
    "chroma.screen_color",
    "chroma.strength",
    "chroma.clip_black",
    "chroma.clip_white",
    "chroma.shrink_grow",
    "chroma.edge_blur",
    "mask_cleanup.grow",
    "mask_cleanup.fill_holes",
    "mask_cleanup.feather",
];
const EXPORT_KEYS: [&str; 7] = ["fg", "matte", "comp", "processed", "fps", "partial", "root"];

fn get_path(v: &Value, path: &str) -> Value {
    path.split('.').fold(v, |x, k| x.get(k).unwrap_or(&Value::Null)).clone()
}
fn set_path(v: &mut Value, path: &str, x: &Value) {
    let mut cur = v;
    let parts: Vec<&str> = path.split('.').collect();
    for (i, k) in parts.iter().enumerate() {
        if !cur.is_object() {
            *cur = json!({});
        }
        let m = cur.as_object_mut().unwrap();
        if i + 1 == parts.len() {
            m.insert((*k).into(), x.clone());
            return;
        }
        cur = m.entry(*k).or_insert_with(|| json!({}));
    }
}

// ---- route ----------------------------------------------------------------------------------------------------

pub fn route_get(c: &HintCfg, key: &str) -> Value {
    get_path(&serde_json::to_value(c).unwrap_or_default(), key)
}
fn route_with(c: &HintCfg, vals: &[(&str, Value)]) -> HintCfg {
    let mut v = serde_json::to_value(c).unwrap_or_default();
    for (k, x) in vals {
        set_path(&mut v, k, x);
    }
    HintCfg::from_value(&v)
}
/// The fields a route edit changed (a group member pulls the whole group in).
pub fn route_diff(before: &HintCfg, after: &HintCfg) -> Vec<String> {
    let mut keys: Vec<String> = ROUTE_KEYS.iter().filter(|k| route_get(before, k) != route_get(after, k)).map(|k| (*k).into()).collect();
    if keys.iter().any(|k| ROUTE_GROUP.contains(&k.as_str())) {
        keys.extend(ROUTE_GROUP.iter().map(|k| (*k).to_owned()));
        keys.sort();
        keys.dedup();
    }
    keys
}
/// `base` with the named fields taken from `own`.
pub fn route_merge(base: &HintCfg, own: &HintCfg, keys: &Fields) -> HintCfg {
    let vals: Vec<(&str, Value)> = keys.keys().filter(|k| ROUTE_KEYS.contains(&k.as_str())).map(|k| (k.as_str(), route_get(own, k))).collect();
    route_with(base, &vals)
}
/// `cfg` with the named fields set to the given values (Reset: the recorded bases; Use preset values: the preset's).
pub fn route_set(cfg: &HintCfg, vals: &[(String, Value)]) -> HintCfg {
    let v: Vec<(&str, Value)> = vals.iter().map(|(k, x)| (k.as_str(), x.clone())).collect();
    route_with(cfg, &v)
}

// ---- delivery -------------------------------------------------------------------------------------------------

pub fn export_get(e: &PipelineExport, key: &str) -> Value {
    match key {
        "fps" => json!(e.fps),
        "partial" => json!(e.allow_partial),
        "root" => json!(e.root),
        k => e.outputs.get(k).map_or(Value::Null, |d| serde_json::to_value(d).unwrap_or_default()),
    }
}
pub fn export_set(e: &mut PipelineExport, key: &str, x: &Value) {
    match key {
        "fps" => e.fps = x.as_f64(),
        "partial" => e.allow_partial = x.as_bool().unwrap_or(false),
        "root" => e.root = x.as_str().map(Into::into),
        k => match serde_json::from_value(x.clone()) {
            Ok(d) => {
                e.outputs.insert(k.into(), d);
            }
            Err(_) => {
                e.outputs.remove(k); // null: that output is not delivered
            }
        },
    }
}
pub fn export_diff(before: &PipelineExport, after: &PipelineExport) -> Vec<String> {
    EXPORT_KEYS.iter().filter(|k| export_get(before, k) != export_get(after, k)).map(|k| (*k).into()).collect()
}
pub fn export_merge(base: &PipelineExport, own: &PipelineExport, keys: &Fields) -> PipelineExport {
    let mut e = base.clone();
    for k in keys.keys().filter(|k| EXPORT_KEYS.contains(&k.as_str())) {
        export_set(&mut e, k, &export_get(own, k));
    }
    e
}
pub fn export_set_all(e: &PipelineExport, vals: &[(String, Value)]) -> PipelineExport {
    let mut e = e.clone();
    for (k, x) in vals {
        export_set(&mut e, k, x);
    }
    e
}

// ---- persistence -----------------------------------------------------------------------------------------------

/// A shot's markers (empty = nothing overridden). The earlier whole-domain marker reads as every field.
pub fn read_fields(native: &Value, d: Domain) -> Fields {
    let Some(o) = native.get(d.key()) else { return Fields::new() };
    let preset = |v: &Value| v.get("preset").and_then(Value::as_str).map(str::to_owned);
    if let Some(m) = o.get("fields").and_then(Value::as_object) {
        return m
            .iter()
            .filter(|(k, _)| d.keys().contains(&k.as_str()))
            .map(|(k, v)| (k.clone(), Field { base: v.get("base").cloned().unwrap_or(Value::Null), preset: preset(v) }))
            .collect();
    }
    let Some(base) = o.get("base") else { return Fields::new() };
    let p = preset(o);
    let all: Vec<(String, Value)> = match d {
        Domain::Route => {
            let c = HintCfg::from_value(base);
            ROUTE_KEYS.iter().map(|k| ((*k).to_owned(), route_get(&c, k))).collect()
        }
        Domain::Delivery => match serde_json::from_value::<PipelineExport>(base.clone()) {
            Ok(e) => EXPORT_KEYS.iter().map(|k| ((*k).to_owned(), export_get(&e, k))).collect(),
            Err(_) => vec![],
        },
    };
    all.into_iter().map(|(k, base)| (k, Field { base, preset: p.clone() })).collect()
}

fn fields_json(f: &Fields) -> Value {
    json!({"fields": f.iter().map(|(k, x)| (k.clone(), json!({"base": x.base, "preset": x.preset}))).collect::<serde_json::Map<_, _>>()})
}

/// Write the markers (or clear them) and, in the SAME atomic update, whatever `extra` writes (the delivery itself,
/// the merged hint route) and the source preset (`src`: `None` = leave it, `Some(None)` = remove, `Some(Some(p))` = set).
pub fn write_domain(meta: &Path, d: Domain, f: &Fields, src: Option<Option<&str>>, extra: impl FnOnce(&mut serde_json::Map<String, Value>)) -> Result<(), String> {
    update_native(meta, |n| {
        extra(n);
        if f.is_empty() {
            n.remove(d.key());
        } else {
            n.insert(d.key().into(), fields_json(f));
        }
        match src {
            Some(Some(p)) => {
                n.insert(format!("{}_src", d.key()), json!(p));
            }
            Some(None) => {
                n.remove(&format!("{}_src", d.key()));
            }
            None => {}
        }
    })
}

/// The one preset name all markers share (what Reset can honestly name), if any.
pub fn common_preset(f: &Fields) -> Option<String> {
    let mut it = f.values().map(|x| &x.preset);
    let first = it.next()?;
    it.all(|p| p == first).then(|| first.clone()).flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hints::{Final, Producer};
    use crate::presets::{Delivery, VideoDelivery};

    fn video(codec: &str) -> Delivery {
        Delivery::Video(VideoDelivery { container: "mp4".into(), codec: codec.into(), quality: 18 })
    }

    #[test]
    fn route_edits_mark_independent_fields_and_the_producer_group_together() {
        let base = HintCfg::default();
        let mut c = base.clone();
        c.mask_cleanup.grow += 2;
        assert_eq!(route_diff(&base, &c), ["mask_cleanup.grow"], "one independent field");
        let mut c = base.clone();
        c.chroma.strength += 0.1;
        c.bypass = true;
        assert_eq!(route_diff(&base, &c), ["bypass", "chroma.strength"]);
        let mut c = base.clone();
        c.guide = Producer::Birefnet;
        let d = route_diff(&base, &c);
        assert!(ROUTE_GROUP.iter().all(|k| d.contains(&(*k).to_string())) && d.len() == 5, "{d:?}");
        assert!(route_diff(&base, &base).is_empty());
    }

    #[test]
    fn merging_takes_only_the_marked_fields_from_the_shot() {
        let mut preset = HintCfg::default();
        (preset.guide, preset.finish) = (Producer::Birefnet, Final::Matanyone2);
        preset.mask_cleanup.grow = 5;
        let mut own = HintCfg::default();
        own.bypass = true;
        own.mask_cleanup.grow = 9;
        own.mask_cleanup.feather = 3.0; // edited-looking but NOT marked: the preset's value wins
        let keys: Fields = [("bypass".to_string(), Field { base: json!(false), preset: None })].into();
        let m = route_merge(&preset, &own, &keys);
        assert!(m.bypass);
        assert_eq!((m.guide, m.finish, m.mask_cleanup.grow, m.mask_cleanup.feather), (Producer::Birefnet, Final::Matanyone2, 5, preset.mask_cleanup.feather));
    }

    #[test]
    fn delivery_fields_are_per_output_fps_partial_and_folder() {
        let (mut a, mut b) = (PipelineExport::default(), PipelineExport::default());
        a.outputs.insert("matte".into(), video("h264"));
        b.outputs.insert("matte".into(), video("hevc"));
        assert_eq!(export_diff(&a, &b), ["matte"], "another output's, fps and folder stay unmarked");
        b.fps = Some(30.0);
        b.allow_partial = true;
        assert_eq!(export_diff(&a, &b), ["matte", "fps", "partial"]);
        let keys: Fields = [("matte".to_string(), Field { base: Value::Null, preset: None })].into();
        let mut preset = PipelineExport::default();
        preset.outputs.insert("fg".into(), video("h264"));
        let m = export_merge(&preset, &b, &keys);
        assert_eq!(m.outputs["matte"], video("hevc"));
        assert_eq!(m.outputs["fg"], video("h264"), "the preset's other output is inherited");
        assert_eq!((m.fps, m.allow_partial), (preset.fps, preset.allow_partial));
    }

    #[test]
    fn old_whole_domain_markers_read_as_every_field_and_new_ones_round_trip() {
        let mut e = PipelineExport::default();
        e.outputs.insert("matte".into(), video("h264"));
        let old = json!({"delivery_override": {"base": serde_json::to_value(&e).unwrap(), "preset": "p"}});
        let f = read_fields(&old, Domain::Delivery);
        assert_eq!(f.len(), 7);
        assert_eq!(f["matte"].base, serde_json::to_value(video("h264")).unwrap());
        assert_eq!(common_preset(&f).as_deref(), Some("p"));
        let new = json!({"route_override": fields_json(&[("bypass".to_string(), Field { base: json!(false), preset: Some("q".into()) })].into())});
        let f = read_fields(&new, Domain::Route);
        assert_eq!((f.len(), f["bypass"].preset.as_deref()), (1, Some("q")));
        assert!(read_fields(&json!({}), Domain::Route).is_empty());
        // Unknown keys in a marker are dropped, never trusted.
        let junk = json!({"route_override": {"fields": {"nope": {"base": 1, "preset": null}}}});
        assert!(read_fields(&junk, Domain::Route).is_empty());
    }
}
