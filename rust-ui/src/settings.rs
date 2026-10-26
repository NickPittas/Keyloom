//! Persistent native preferences: one versioned JSON file outside project data, validated before
//! save, written atomically. Unreadable or newer-version files are never overwritten in place:
//! they are renamed aside on the first successful Apply. UI-free (see `ui::settings`).
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

pub const VERSION: u32 = 1;
/// Mirrors backend `VALID_VRAM_PROFILES`; an engine policy, not an allocation cap.
pub const VRAM_PROFILES: [&str; 2] = ["auto", "low"];
/// Mirrors backend `VALID_HINT_MODES`.
pub const HINT_MODES: [&str; 3] = ["none", "full", "keyframes"];
/// Mirrors backend `VALID_TEMPORAL_WINDOWS`.
pub const TEMPORAL_WINDOWS: [&str; 6] = ["auto", "80/40", "48/24", "24/12", "8/4", "1/1"];

/// "Temporal awareness" wording for a wire value (the wire value itself never changes).
pub fn temporal_label(w: &str) -> String {
    match w {
        "auto" => "Auto (engine picks from frame size and VRAM)".into(),
        "1/1" => "Off: single frame (1/1)".into(),
        other => match other.split_once('/') {
            Some((c, s)) => format!("{c} frames (stride {s})"),
            None => other.to_string(),
        },
    }
}

/// The frame window of a wire value (`None` = auto).
pub fn temporal_chunk(w: &str) -> Option<usize> {
    w.split_once('/')?.0.parse().ok()
}

/// One sentence on what the selection means for a job over `frames` frames.
pub fn temporal_explain(w: &str, frames: Option<usize>) -> String {
    let base = "Temporal awareness: how many neighbouring frames the key sees at once. More frames steadier edges and more memory; context never crosses In/Out.";
    match (temporal_chunk(w), frames) {
        (Some(c), Some(n)) if c > n => {
            format!("{base} Your range has {n} frames, so the effective window is capped at {n}.")
        }
        (Some(1), _) => {
            format!("{base} Off: every frame is keyed on its own (flicker is not smoothed).")
        }
        (Some(c), _) => format!("{base} Window {c} frames, advancing by half a window."),
        (None, Some(n)) => format!("{base} Auto chooses for the {n}-frame range."),
        (None, None) => base.to_string(),
    }
}
/// (label, size, id) as in the Qt preferences dialog.
pub const TRACKER_MODELS: [(&str, &str, &str); 3] = [
    ("Fast", "184 MB", "facebook/sam2.1-hiera-small"),
    (
        "Base+ (default)",
        "324 MB",
        "facebook/sam2.1-hiera-base-plus",
    ),
    ("Highest quality", "898 MB", "facebook/sam2.1-hiera-large"),
];
pub const CACHE_MIB: std::ops::RangeInclusive<u32> = 64..=65536;
pub const WORKERS: std::ops::RangeInclusive<u32> = 0..=32;
pub const UI_SCALE: std::ops::RangeInclusive<f32> = 0.75..=2.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Playback {
    /// Decoded-frame RAM preview budget; 0 = Auto (`frames::default_cache_mb`).
    pub cache_mib: u32,
    /// 0 = Auto (`frames::default_workers`).
    pub decode_workers: u32,
    pub loop_default: bool,
    /// Keys this build does not know: carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Interface {
    pub ui_scale: f32,
    /// Closing the window minimizes it instead of quitting, so the network client stays available.
    pub background: bool,
    /// Start minimized when the user signs in (per-user autostart; needs `background`).
    pub launch_at_login: bool,
    /// Keys this build does not know: carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Backend `InferenceParams` defaults. They seed a clip's Key workflow only until that clip (or the Qt
/// session) has its own values.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ai {
    pub vram_profile: String,
    pub hint_mode: String,
    pub hint_quality: f32,
    pub carry_hint: bool,
    pub carry_hint_quality: f32,
    pub despill_strength: f32,
    pub cutout_linear: bool,
    pub temporal_window: String,
    /// Keys this build does not know: carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Mask/hint tool defaults; awaiting native mask-tool integration.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Masks {
    pub tracker_model: String,
    /// Keys this build does not know: carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Connection to a coordinator (no secret here: `credentials` is only the PATH of the private credentials file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Network {
    /// Connect to the joined network when the app starts.
    pub enabled: bool,
    /// Take work from the network when this computer's GPU is idle (opt-in).
    pub receive: bool,
    /// Empty = `<settings folder>/network/credentials.json`.
    pub credentials: String,
    /// Every network this PC has joined, each connected independently. An old single connection (the fields above)
    /// becomes the first entry when the file is loaded.
    pub connections: Vec<Connection>,
    /// Credentials path of the connection Auto Batch jobs go to (empty = first online one).
    pub default_submit: String,
    /// This PC's name on every joined network (empty = the computer's host name).
    pub name: String,
    /// The user removed this PC's own host service: it is not installed again at startup (Start clears it).
    pub host_removed: bool,
    /// Allow other PCs to join this PC's service.
    pub allow_join: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Network {
    fn default() -> Self {
        Self {
            enabled: false,
            receive: false,
            credentials: String::new(),
            connections: vec![],
            default_submit: String::new(),
            name: String::new(),
            host_removed: false,
            allow_join: true,
            extra: Map::new(),
        }
    }
}

/// One joined network (no secret here: `credentials` is only the PATH of its private credentials file).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Connection {
    pub credentials: String,
    pub enabled: bool,
    /// On the local service, accept jobs from other PCs; on a client, allow Automatic to send it shots.
    pub receive: bool,
    /// Last known https addresses of the host (tried in order on connect; refreshed by Rescan).
    pub endpoints: Vec<String>,
    /// User-picked address tried first (empty = automatic).
    pub preferred: String,
    /// Certificate fingerprint of the network (empty = learn it from the connection).
    pub network_id: String,
    /// Exact host installation this connection targets (empty = legacy: every member).
    pub host_installation_id: String,
    pub host_name: String,
    /// This PC's own service (joined automatically, receives work by default).
    pub local_host: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Network {
    pub fn connections(&self) -> &[Connection] {
        &self.connections
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub playback: Playback,
    pub interface: Interface,
    pub ai: Ai,
    pub masks: Masks,
    /// AI worker targets. Empty = one implicit local target (GPU 0).
    pub ai_targets: Vec<crate::worker::Target>,
    /// This installation's Path Mapping: shared location name -> local root (absent in old files = none).
    pub shared_roots: Vec<crate::shared_paths::SharedRoot>,
    /// Network client (coordinator connection, receive opt-in).
    pub network: Network,
    /// Keys this build does not know (same version): carried through saves untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Playback {
    fn default() -> Self {
        Self {
            cache_mib: 0,
            decode_workers: 0,
            loop_default: true,
            extra: Map::new(),
        }
    }
}
impl Default for Interface {
    fn default() -> Self {
        Self {
            ui_scale: 1.0,
            background: false,
            launch_at_login: false,
            extra: Map::new(),
        }
    }
}
impl Default for Ai {
    fn default() -> Self {
        Self {
            vram_profile: "auto".into(),
            hint_mode: "none".into(),
            hint_quality: 0.95,
            carry_hint: true,
            carry_hint_quality: 0.95,
            despill_strength: 0.5,
            cutout_linear: false,
            temporal_window: "auto".into(),
            extra: Map::new(),
        }
    }
}
impl Default for Masks {
    fn default() -> Self {
        Self {
            tracker_model: TRACKER_MODELS[1].2.into(),
            extra: Map::new(),
        }
    }
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            version: VERSION,
            playback: Default::default(),
            interface: Default::default(),
            ai: Default::default(),
            masks: Default::default(),
            ai_targets: vec![],
            shared_roots: vec![],
            network: Network::default(),
            extra: Map::new(),
        }
    }
}

impl Settings {
    /// Everything that can be saved: the core settings plus this PC's Path Mapping.
    pub fn validate(&self) -> Result<(), String> {
        self.validate_core()?;
        self.mapping_problem().map_or(Ok(()), Err)
    }

    /// Why the Path Mapping rows cannot be used on this PC (host path rules), if they cannot.
    pub fn mapping_problem(&self) -> Option<String> {
        crate::shared_paths::validate_roots(&self.shared_roots).err()
    }

    /// Validation applied when a file is loaded: everything except the host-specific mapping check, so a
    /// settings file copied from another OS keeps its other settings and its mapping rows (to be fixed in the
    /// Path mapping page; `validate` still refuses to save them as they are).
    fn validate_core(&self) -> Result<(), String> {
        let one_of = |name: &str, v: &str, ok: &[&str]| {
            ok.contains(&v)
                .then_some(())
                .ok_or_else(|| format!("{name} must be one of {ok:?}, got {v:?}"))
        };
        let unit = |name: &str, v: f32| {
            (0.0..=1.0)
                .contains(&v)
                .then_some(())
                .ok_or_else(|| format!("{name} must be in [0, 1], got {v}"))
        };
        if self.interface.launch_at_login && !self.interface.background {
            return Err("Start at sign-in needs \"Keep running in the background\"".into());
        }
        if self.playback.cache_mib != 0 && !CACHE_MIB.contains(&self.playback.cache_mib) {
            return Err(format!(
                "RAM budget must be Auto (0) or {}–{} MiB",
                CACHE_MIB.start(),
                CACHE_MIB.end()
            ));
        }
        if !WORKERS.contains(&self.playback.decode_workers) {
            return Err(format!(
                "decode workers must be {}–{} (0 = Auto)",
                WORKERS.start(),
                WORKERS.end()
            ));
        }
        if !UI_SCALE.contains(&self.interface.ui_scale) {
            return Err(format!(
                "UI scale must be {}–{}",
                UI_SCALE.start(),
                UI_SCALE.end()
            ));
        }
        let a = &self.ai;
        one_of("vram_profile", &a.vram_profile, &VRAM_PROFILES)?;
        one_of("hint_mode", &a.hint_mode, &HINT_MODES)?;
        one_of("temporal_window", &a.temporal_window, &TEMPORAL_WINDOWS)?;
        unit("hint_quality", a.hint_quality)?;
        unit("carry_hint_quality", a.carry_hint_quality)?;
        unit("despill_strength", a.despill_strength)?;
        for (i, t) in self.ai_targets.iter().enumerate() {
            t.validate()?;
            if self.ai_targets[..i].iter().any(|o| o.id == t.id) {
                return Err(format!("duplicate AI target id {}", t.id));
            }
        }
        if !TRACKER_MODELS
            .iter()
            .any(|m| m.2 == self.masks.tracker_model)
        {
            return Err(format!(
                "unknown tracker model {:?}",
                self.masks.tracker_model
            ));
        }
        Ok(())
    }

    /// Factory defaults with every unknown key (top-level and per-section) carried over.
    pub fn reset_keeping_unknown(&mut self) {
        let old = std::mem::take(self);
        self.extra = old.extra;
        self.playback.extra = old.playback.extra;
        self.interface.extra = old.interface.extra;
        self.ai.extra = old.ai.extra;
        self.masks.extra = old.masks.extra;
        self.ai_targets = old.ai_targets; // machines are not a preference default
        self.shared_roots = old.shared_roots; // neither are this PC's mounts
        self.network = old.network; // nor its coordinator pairing
    }

    /// Workers to hand to `frames::Loader`: Auto resolves here.
    pub fn workers(&self) -> usize {
        match self.playback.decode_workers {
            0 => crate::frames::default_workers(),
            n => n as usize,
        }
    }

    /// Cache budget for a new viewer; an explicit `--cache-mb` wins over the saved default.
    pub fn cache_mb(&self, cli: Option<usize>) -> usize {
        cli.unwrap_or(match self.playback.cache_mib {
            0 => crate::frames::default_cache_mb(),
            n => n as usize,
        })
    }
}

/// Platform settings location. Windows needs no `HOME`: Roaming AppData, then Local AppData; with
/// neither the app runs with session-only settings. Unix: `$XDG_CONFIG_HOME` or `~/.config`.
pub fn default_path() -> Option<PathBuf> {
    if cfg!(windows) {
        windows_path(
            std::env::var_os("APPDATA"),
            std::env::var_os("LOCALAPPDATA"),
        )
    } else {
        unix_path(
            std::env::var_os("XDG_CONFIG_HOME"),
            std::env::var_os("HOME"),
        )
    }
}

/// `<base>/keyloom`.
pub fn config_dir(base: PathBuf) -> PathBuf {
    base.join("keyloom")
}

/// `%APPDATA%\keyloom\settings.json`, else `%LOCALAPPDATA%\…`, else `None`.
pub fn windows_path(
    appdata: Option<std::ffi::OsString>,
    local: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let base = appdata
        .filter(|v| !v.is_empty())
        .or_else(|| local.filter(|v| !v.is_empty()))?;
    Some(config_dir(PathBuf::from(base)).join("settings.json"))
}

/// `$XDG_CONFIG_HOME/keyloom/settings.json`, else `~/.config/keyloom/…`.
pub fn unix_path(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    let base = xdg
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| home.map(|h| PathBuf::from(h).join(".config")))?;
    Some(config_dir(base).join("settings.json"))
}

/// Saved settings plus the file they came from. Cloning `saved()` gives an editable draft.
pub struct Store {
    path: Option<PathBuf>,
    saved: Settings,
    /// Why the existing file was not used; shown by the UI until the next successful save.
    pub problem: Option<String>,
    /// The file on disk is unusable and must be moved aside before it is replaced.
    preserve: bool,
}

impl Store {
    /// Never fails: a missing file gives defaults silently, a bad one gives defaults + `problem`.
    pub fn load(path: Option<PathBuf>) -> Self {
        let mut s = Self {
            path,
            saved: Settings::default(),
            problem: None,
            preserve: false,
        };
        let Some(p) = s.path.clone() else {
            s.problem =
                Some("No settings location available; changes last for this session only.".into());
            return s;
        };
        match std::fs::read(&p) {
            Ok(bytes) => match Self::parse(&bytes, p.parent()) {
                Ok(v) => s.saved = v,
                Err(e) => {
                    s.problem = Some(format!(
                        "Settings file {} was not used ({e}). Defaults are active; the file is kept and renamed aside if you Apply.",
                        p.display()
                    ));
                    s.preserve = true;
                }
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                s.problem = Some(format!(
                    "Cannot read {}: {e}. Defaults are active.",
                    p.display()
                ));
                s.preserve = true;
            }
        }
        s
    }

    fn parse(bytes: &[u8], dir: Option<&Path>) -> Result<Settings, String> {
        let mut raw: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("invalid JSON: {e}"))?;
        match raw.get("version").and_then(Value::as_u64) {
            Some(v) if v == VERSION as u64 => {}
            Some(v) => {
                return Err(format!(
                    "unsupported settings version {v}, this build reads {VERSION}"
                ))
            }
            None => return Err("missing settings version".into()),
        }
        // SSH machines were removed; drop them so an old file still loads with the rest of its settings.
        if let Some(a) = raw.get_mut("ai_targets").and_then(Value::as_array_mut) {
            a.retain(|t| t.get("kind").and_then(Value::as_str) == Some("local"));
        }
        if let Some(n) = raw.get_mut("network").and_then(Value::as_object_mut) {
            // The old `use_invitation` is `allow_join` turned around.
            if let Some(old) = n.remove("use_invitation").and_then(|v| v.as_bool()) {
                n.entry("allow_join").or_insert(Value::Bool(!old));
            }
            // An old file with one connection in the fields above and no list: that connection becomes the list.
            // An empty list marked migrated means the user removed everything and stays empty.
            let empty = n.get("connections").and_then(Value::as_array).is_none_or(Vec::is_empty);
            if empty && n.get("connections_migrated").and_then(Value::as_bool) != Some(true) {
                let field = |k| n.get(k).and_then(Value::as_bool).unwrap_or(false);
                let path = match n.get("credentials").and_then(Value::as_str).unwrap_or("").trim() {
                    "" => dir.map(|d| d.join("network").join("credentials.json")),
                    c => Some(PathBuf::from(c)),
                };
                let conns: Vec<Value> = path.filter(|p| p.exists()).map(|p| serde_json::json!({"credentials": p.display().to_string(), "enabled": field("enabled"), "receive": field("receive")})).into_iter().collect();
                if !conns.is_empty() {
                    n.insert("connections".into(), Value::Array(conns));
                    n.insert("connections_migrated".into(), Value::Bool(true));
                }
            }
        }
        let s: Settings = serde_json::from_value(raw).map_err(|e| e.to_string())?;
        s.validate_core()?;
        Ok(s)
    }

    pub fn saved(&self) -> &Settings {
        &self.saved
    }

    /// The file on disk was not usable, so defaults are active and Apply must still be offered.
    pub fn needs_repair(&self) -> bool {
        self.preserve
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Validate, move an unusable old file aside, then atomically replace. On error nothing changes.
    pub fn save(&mut self, new: Settings) -> Result<(), String> {
        new.validate()?;
        let Some(path) = self.path.clone() else {
            self.saved = new;
            return Ok(());
        };
        let dir = path.parent().ok_or("settings path has no folder")?;
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        let json = serde_json::to_vec_pretty(&new).map_err(|e| e.to_string())?;
        let tmp = path.with_extension(format!("json.tmp{}", std::process::id()));
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()
        };
        write().map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            format!("cannot write {}: {e}", tmp.display())
        })?;
        let mut aside = None;
        if self.preserve && path.exists() {
            let a = (0..)
                .map(|n| path.with_extension(format!("json.unusable{n}")))
                .find(|p| !p.exists())
                .expect("unbounded range");
            std::fs::rename(&path, &a).map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("cannot preserve old settings as {}: {e}", a.display())
            })?;
            aside = Some(a);
        }
        std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            if let Some(a) = &aside {
                let _ = std::fs::rename(a, &path); // put the old file back
            }
            format!("cannot replace {}: {e}", path.display())
        })?;
        self.saved = new;
        self.preserve = false;
        self.problem = None;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("ck-settings-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn windows_settings_path_needs_no_home() {
        let ad = std::ffi::OsString::from(r"C:\Users\u\AppData\Roaming");
        let la = std::ffi::OsString::from(r"C:\Users\u\AppData\Local");
        assert_eq!(
            windows_path(Some(ad.clone()), Some(la.clone())),
            Some(
                PathBuf::from(r"C:\Users\u\AppData\Roaming")
                    .join("keyloom")
                    .join("settings.json")
            )
        );
        // No APPDATA and no HOME: LOCALAPPDATA still gives a settings location on Windows.
        assert_eq!(
            windows_path(None, Some(la.clone())),
            Some(
                PathBuf::from(r"C:\Users\u\AppData\Local")
                    .join("keyloom")
                    .join("settings.json")
            )
        );
        assert_eq!(windows_path(None, None), None);
        assert_eq!(windows_path(Some(std::ffi::OsString::new()), None), None);
    }

    #[test]
    fn settings_dir_is_keyloom_even_with_legacy_dir() {
        let d = dir("legacy-config");
        std::fs::create_dir_all(d.join("corridorkey-native")).unwrap();
        assert_eq!(config_dir(d.clone()), d.join("keyloom"));
        let fresh = dir("fresh-config");
        assert_eq!(config_dir(fresh.clone()), fresh.join("keyloom"));
    }

    #[test]
    fn unix_settings_path_prefers_xdg() {
        let x = std::ffi::OsString::from("/xdg");
        let h = std::ffi::OsString::from("/home/u");
        assert_eq!(
            unix_path(Some(x.clone()), Some(h.clone())),
            Some(PathBuf::from("/xdg/keyloom/settings.json"))
        );
        assert_eq!(
            unix_path(None, Some(h.clone())),
            Some(PathBuf::from(
                "/home/u/.config/keyloom/settings.json"
            ))
        );
        assert_eq!(unix_path(None, None), None);
    }

    #[test]
    fn defaults_match_backend_and_validate() {
        let mut s = Settings::default();
        s.validate().unwrap();
        assert_eq!(
            (
                s.ai.vram_profile.as_str(),
                s.ai.hint_quality,
                s.ai.despill_strength
            ),
            ("auto", 0.95, 0.5)
        );
        assert!(s.playback.loop_default && s.playback.decode_workers == 0);
        assert_eq!(s.cache_mb(Some(7)), 7);
        assert_eq!(s.cache_mb(None), crate::frames::default_cache_mb());
        s.playback.cache_mib = 300;
        assert_eq!(s.cache_mb(None), 300);
        assert_eq!(s.workers(), crate::frames::default_workers());
    }

    #[test]
    fn apply_roundtrips_and_keeps_unknown_keys() {
        let p = dir("rt").join("s.json");
        std::fs::write(
            &p,
            r#"{"version":1,"future_key":{"a":1},"playback":{"cache_mib":2048}}"#,
        )
        .unwrap();
        let mut st = Store::load(Some(p.clone()));
        assert!(st.problem.is_none());
        assert_eq!(st.saved().playback.cache_mib, 2048);
        let mut n = st.saved().clone();
        n.playback.decode_workers = 3;
        n.ai.vram_profile = "low".into();
        st.save(n).unwrap();
        let again = Store::load(Some(p.clone()));
        assert_eq!(again.saved().playback.decode_workers, 3);
        assert_eq!(again.saved().workers(), 3);
        assert_eq!(again.saved().ai.vram_profile, "low");
        assert!(std::fs::read_to_string(&p).unwrap().contains("future_key"));
        assert_eq!(
            std::fs::read_dir(p.parent().unwrap()).unwrap().count(),
            1,
            "no temp files left"
        );
    }

    #[test]
    fn invalid_values_are_rejected_without_touching_the_file() {
        let p = dir("inv").join("s.json");
        let mut st = Store::load(Some(p.clone()));
        st.save(Settings::default()).unwrap();
        let before = std::fs::read(&p).unwrap();
        for bad in [
            |s: &mut Settings| s.playback.cache_mib = 1,
            |s: &mut Settings| s.interface.ui_scale = f32::NAN,
            |s: &mut Settings| s.ai.vram_profile = "8gb".into(),
            |s: &mut Settings| s.ai.hint_quality = 1.5,
            |s: &mut Settings| s.masks.tracker_model = "x".into(),
        ] {
            let mut n = Settings::default();
            bad(&mut n);
            assert!(st.save(n).is_err());
        }
        assert_eq!(std::fs::read(&p).unwrap(), before);
        assert_eq!(st.saved(), &Settings::default());
    }

    #[test]
    fn corrupt_and_newer_files_are_preserved_on_apply() {
        for (name, body) in [
            ("corrupt", "{not json".to_string()),
            (
                "newer",
                r#"{"version":2,"playback":{"cache_mib":99}}"#.to_string(),
            ),
            (
                "range",
                r#"{"version":1,"playback":{"cache_mib":1}}"#.to_string(),
            ),
        ] {
            let p = dir(name).join("s.json");
            std::fs::write(&p, &body).unwrap();
            let mut st = Store::load(Some(p.clone()));
            assert!(st.problem.is_some(), "{name}");
            assert_eq!(st.saved(), &Settings::default());
            assert_eq!(
                std::fs::read_to_string(&p).unwrap(),
                body,
                "load must not modify {name}"
            );
            st.save(Settings::default()).unwrap();
            assert!(st.problem.is_none());
            let kept = p.with_extension("json.unusable0");
            assert_eq!(std::fs::read_to_string(kept).unwrap(), body, "{name}");
            assert_eq!(Store::load(Some(p)).saved(), &Settings::default());
        }
    }

    #[test]
    fn failed_write_reports_and_keeps_saved_state() {
        let d = dir("ro");
        let blocker = d.join("file");
        std::fs::write(&blocker, "x").unwrap();
        let mut st = Store::load(Some(blocker.join("s.json"))); // parent is a file
        let mut n = Settings::default();
        n.playback.cache_mib = 512;
        assert!(st.save(n).is_err());
        assert_eq!(st.saved().playback.cache_mib, 0);
    }

    #[test]
    fn nested_unknown_keys_survive_apply_and_reset() {
        let p = dir("nested").join("s.json");
        std::fs::write(
            &p,
            r#"{"version":1,"playback":{"x_p":1},"interface":{"x_i":"a"},"ai":{"x_a":[1]},"masks":{"x_m":{"k":true}}}"#,
        )
        .unwrap();
        let mut st = Store::load(Some(p.clone()));
        assert!(st.problem.is_none());
        let mut n = st.saved().clone();
        n.playback.decode_workers = 2;
        st.save(n).unwrap();
        let v: Value = serde_json::from_slice(&std::fs::read(&p).unwrap()).unwrap();
        assert_eq!(v["playback"]["x_p"], 1);
        assert_eq!(v["interface"]["x_i"], "a");
        assert_eq!(v["ai"]["x_a"][0], 1);
        assert_eq!(v["masks"]["x_m"]["k"], true);
        assert_eq!(v["playback"]["decode_workers"], 2);

        let mut r = Store::load(Some(p)).saved().clone();
        r.reset_keeping_unknown();
        assert_eq!(r.playback.decode_workers, 0);
        assert_eq!(r.playback.extra["x_p"], 1);
        assert_eq!(r.interface.extra["x_i"], "a");
        assert_eq!(r.ai.extra["x_a"][0], 1);
        assert_eq!(r.masks.extra["x_m"]["k"], true);
    }

    #[test]
    fn needs_repair_tracks_unusable_file() {
        let p = dir("repair").join("s.json");
        std::fs::write(&p, "{nope").unwrap();
        let mut st = Store::load(Some(p));
        assert!(st.needs_repair());
        st.save(Settings::default()).unwrap();
        assert!(!st.needs_repair());
        assert!(!Store::load(None).needs_repair());
    }
}
