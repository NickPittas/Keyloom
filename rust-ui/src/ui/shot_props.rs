use super::{Fixture, part};
use crate::ui::controls::{
    action_bar::{ActionBar, Blocker, BusyState},
    buttons::{IconButtonSize, ghost_button, icon_button, primary_button, secondary_button},
    checkbox::Check,
    dialog::{Dialog, DialogSize, EmptyButton, empty_state, paragraph},
    dropdown::{Dropdown, Item},
    line,
    numeric::{self, NumSpec, NumericField},
    paint_line,
    property::{self, PropBody, PropRows},
    segmented::{Seg, SegWidth, Segmented},
    textfield::text_field,
};
use crate::ui::{icons::Icon, tokens::tokens, type_scale as ty};
use eframe::egui::{self, Align, Id, Rect, Sense, Ui};
use std::path::PathBuf;

// shortcut: preview values reset per frame, bind them to shot parameters when this fixture panel becomes production UI.
const KEY_SECTIONS: [&str; 5] = ["Render", "Key engine", "Hint", "Despill", "Cutout"];
const TEMPORAL: &[&str] = &[
    "Auto",
    "80 frames (stride 40)",
    "48 frames (stride 24)",
    "24 frames (stride 12)",
    "8 frames (stride 4)",
    "Off: single frame (1/1)",
];
const VIEWS: &[&str] = &["Input", "Hint", "Mask", "FG", "Matte", "Comp", "Processed"];
const SOURCE: &str = "/projects/Night_exterior_greenscreen/clips/V1-0062/Frames";
const MASK: &str = "/projects/Night_exterior_greenscreen/clips/V1-0062/VideoMamaMaskHint";
const HINT: &str = "/projects/Night_exterior_greenscreen/clips/V1-0062/AlphaHint";
const OUTPUT: &str = "/projects/Night_exterior_greenscreen/clips/V1-0062/Output";

/// One pickable value: `value` goes back in the act, `label` is what the row shows.
#[derive(Clone, Debug, Default)]
pub struct Choice {
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Default)]
pub struct MachineChoice {
    /// `None` = inherit the project default.
    pub id: Option<String>,
    pub label: String,
    pub available: bool,
    pub reason: Option<String>,
}

/// Which Key rows this shot overrides (the page has no "changed" mark yet; the flags ride along for it).
#[derive(Clone, Debug, Default)]
pub struct KeyOverrides {
    pub vram: bool,
    pub temporal: bool,
    pub hint_mode: bool,
    pub hint_quality: bool,
    pub carry_hint: bool,
    pub carry_hint_quality: bool,
    pub despill: bool,
    pub cutout_linear: bool,
}

#[derive(Clone, Debug, Default)]
pub struct KeyProps {
    /// The shot's own machine id; `None` = inherits.
    pub machine: Option<String>,
    pub machine_effective: String,
    pub machines: Vec<MachineChoice>,
    pub inherited: bool,
    pub model: String,
    pub vram: String,
    pub vram_choices: Vec<Choice>,
    pub temporal: String,
    pub temporal_choices: Vec<Choice>,
    pub hint_mode: String,
    pub hint_mode_choices: Vec<Choice>,
    pub hint_quality: f64,
    pub carry_hint: bool,
    pub carry_hint_quality: f64,
    pub despill: f64,
    pub cutout_linear: bool,
    pub overridden: KeyOverrides,
    /// Text of the reset button; `None` = nothing overridden, no button.
    pub reset_label: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RelinkState {
    #[default]
    Checking,
    Failed,
    Partial,
    Verified,
}

#[derive(Clone, Debug, Default)]
pub struct PathProp {
    pub label: String,
    pub path: PathBuf,
    pub frames: Option<usize>,
}

#[derive(Clone, Debug, Default)]
pub struct RelinkProps {
    pub path: PathBuf,
    pub state: RelinkState,
    pub failure: Option<String>,
    /// One line about the probed movie (size, rate, frames).
    pub detail: Option<String>,
    pub confirmed: bool,
    pub extract_enabled: bool,
    pub extract_reason: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CompanionProps {
    pub folder: PathBuf,
    /// false = AlphaHint, true = VideoMamaMaskHint.
    pub mask: bool,
    /// Ok((destination, frames written, frames kept)) or the reason it cannot import.
    pub plan: Result<(PathBuf, usize, usize), String>,
    pub running: bool,
}

#[derive(Clone, Debug, Default)]
pub struct MediaProps {
    /// "Frame folder" / "Movie" / "Missing movie".
    pub source_kind: String,
    pub source_path: Option<PathBuf>,
    pub frame_size: Option<String>,
    pub frame_rate: f64,
    pub fps_assumed: bool,
    /// "0–64 · 65 frames".
    pub frames: String,
    /// Source frame count (N in "Frames N (timeline M)").
    pub frame_count: usize,
    /// Timeline frame count (M); `None` when no viewer is open on this clip.
    pub timeline_frames: Option<usize>,
    pub input_linear: bool,
    /// The saved colour reading (shown as provenance in the Input colour tooltip).
    pub stored_linear: bool,
    /// The last input-colour save failed; it is retried on the next change.
    pub colour_save_failed: bool,
    /// Where the colour reading came from (shown as "Set by").
    pub colour_note: Option<String>,
    pub paths: Vec<PathProp>,
    /// Keyed result range, e.g. "0–64"; shown in the Paths section.
    pub result_range: Option<String>,
    pub missing: bool,
    pub relink: Option<RelinkProps>,
    pub can_choose_companion: bool,
    pub companion: Option<CompanionProps>,
    pub note: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default)]
pub struct ReviewProps {
    /// (output name, "N of M") per output.
    pub frames: Vec<(String, String)>,
    /// "endor-omarchy · Today 14:02"; `None` = Unknown.
    pub last_run: Option<String>,
    pub views: Vec<String>,
    pub a: String,
    pub b: String,
    pub wipe: bool,
    pub horizontal: bool,
    pub missing_frames: usize,
    /// (below 0, above 1); `None` = not measured (8-bit source).
    pub out_of_range: Option<(usize, usize)>,
}

#[derive(Clone, Debug, Default)]
pub struct BarProps {
    pub primary_label: String,
    pub primary_enabled: bool,
    pub primary_reason: Option<String>,
    pub secondary: Option<(String, bool)>,
    pub menu: Vec<(String, bool, Option<String>)>,
    /// (message, fix button label).
    pub blocker: Option<(String, Option<String>)>,
    pub status: Option<String>,
    pub busy_percent: Option<u8>,
}

/// A pickable value that may be missing its model (`ready` false = the app's check found it missing).
#[derive(Clone, Debug, Default)]
pub struct ReadyChoice {
    pub value: String,
    pub label: String,
    pub ready: bool,
}

/// One action's state: label, enabled, and the reason it is off or waits.
#[derive(Clone, Debug, Default)]
pub struct ActState {
    pub enabled: bool,
    pub label: String,
    pub reason: String,
}

#[derive(Clone, Debug)]
pub struct ChromaProps {
    /// "green" / "blue".
    pub screen: String,
    /// Sampled screen colour; `None` = automatic.
    pub sampled: Option<[u8; 3]>,
    pub strength: f64,
    pub clip_black: f64,
    pub clip_white: f64,
    pub shrink_grow: i64,
    pub edge_blur: i64,
    /// Legacy holdout strokes (foreground, background), or the load error.
    pub holdout: Result<(usize, usize), String>,
}

#[derive(Clone, Debug, Default)]
pub struct MarkedProps {
    pub pre: bool,
    pub hint: bool,
    /// Two rows (pre-matte, same marks as hints) instead of one.
    pub two_rows: bool,
    pub show_hint_row: bool,
    /// "Full-range hints use In-frame pre-matte."
    pub in_frame_note: bool,
    pub key_uses_marked: bool,
}

#[derive(Clone, Debug, Default)]
pub struct CleanupProps {
    pub grow: i64,
    pub fill_holes: bool,
    pub feather: f64,
    /// Which stage needs regenerating after a change (tooltip).
    pub note: String,
}

#[derive(Clone, Debug, Default)]
pub struct MovieProps {
    pub path: PathBuf,
    pub as_mask: bool,
    pub alpha_channel: bool,
}

#[derive(Clone, Debug, Default)]
pub struct HintTool {
    pub value: String,
    pub label: String,
    pub tip: String,
}

/// Pre-matte or Final hint: frames on disk, staleness and its actions.
#[derive(Clone, Debug, Default)]
pub struct StageProps {
    pub frames_on_disk: usize,
    pub stale: bool,
    /// SAM single-frame preview (Pre-matte with SAM only).
    pub preview: Option<ActState>,
    pub generate: ActState,
    pub delete: ActState,
}

#[derive(Clone, Debug, Default)]
pub struct RouteProps {
    /// The shot's own route fields (names).
    pub overrides: Vec<String>,
    pub reset_label: Option<String>,
    pub unsaved: bool,
    pub save_failed: bool,
}

/// Mirror of the app's Hint view; values only, no app types.
#[derive(Clone, Debug, Default)]
pub struct HintProps {
    pub bypass: bool,
    /// The worker has not been asked yet: readiness is unknown, not missing.
    pub worker_unknown: bool,
    pub producer: String,
    pub producers: Vec<ReadyChoice>,
    /// Model the producer needs when it is missing.
    pub producer_missing: Option<String>,
    pub birefnet_variant: Option<String>,
    pub birefnet_variants: Vec<ReadyChoice>,
    pub sam_model: Option<String>,
    pub sam_models: Vec<ReadyChoice>,
    pub chroma: Option<ChromaProps>,
    pub finish: String,
    pub finishes: Vec<ReadyChoice>,
    pub finish_missing: Option<String>,
    /// 1–64, VideoMaMa only.
    pub videomama_chunk: Option<u32>,
    /// `None` for an imported alpha.
    pub marked: Option<MarkedProps>,
    pub marked_gate: Option<String>,
    pub marks_err: Option<String>,
    pub cleanup: CleanupProps,
    pub movie: Option<MovieProps>,
    /// Companion movie import; `None` until a movie is picked.
    pub import: Option<ActState>,
    /// Current prompt tool value ("none" = Navigate).
    pub tool: String,
    pub tools: Vec<HintTool>,
    /// 2–120; `Some` when the producer has paint/point tools.
    pub brush: Option<f32>,
    pub prompt_frames: usize,
    pub frame_annotated: bool,
    pub can_undo: bool,
    pub prematte: StageProps,
    pub hint: StageProps,
    pub route: RouteProps,
    pub note: Option<String>,
}

/// One video output's exact stored values (mirror of the app's Deliver view). `custom` = a container/codec pair the
/// export does not know: Container/Codec/Quality then show "Custom".
#[derive(Clone, Debug)]
pub struct DeliverVideoProps {
    pub container: String,
    pub codec: String,
    pub quality: u32,
    pub custom: bool,
    /// "CRF" / "qscale" (empty when custom).
    pub quality_name: String,
    pub min: u32,
    pub max: u32,
    pub quality_note: String,
    pub alpha_warning: Option<String>,
    pub containers: Vec<Choice>,
    /// Codecs of the current container.
    pub codecs: Vec<Choice>,
}

#[derive(Clone, Debug)]
pub struct DeliverOutputProps {
    pub title: String,
    pub enabled: bool,
    /// Sequence format: EXR (true) or PNG.
    pub exr: bool,
    /// `Some` = a video file, `None` = an image sequence.
    pub video: Option<DeliverVideoProps>,
    /// "EXR sequence", "MOV · ProRes 4444 · qscale 9", "Not written".
    pub summary: String,
    // The page has no "changed" mark yet; the flags ride along for it.
    pub enabled_overridden: bool,
    pub format_overridden: bool,
    pub delivery_overridden: bool,
}

#[derive(Clone, Debug)]
pub struct DeliverPresetProps {
    pub name: String,
    pub label: String,
    pub legacy: bool,
}

/// Mirror of the app's Deliver view; values only, no app types.
#[derive(Clone, Debug)]
pub struct DeliverProps {
    pub outputs: Vec<DeliverOutputProps>,
    pub confirm_existing: Option<Vec<String>>,
    /// Shared EXR compression: current value, the values on offer, and its notes.
    pub compression: String,
    pub compression_choices: Vec<String>,
    pub compression_note: String,
    pub compression_lossy: Option<String>,
    pub compression_overridden: bool,
    /// Shared frame rate; `None` = each shot's own (`fps_source` is this shot's).
    pub fps: Option<f64>,
    pub fps_source: f64,
    pub fps_overridden: bool,
    pub has_video: bool,
    /// In/Out frames (false) or the Done frames of a partial result (true).
    pub done_frames: bool,
    pub done_frames_overridden: bool,
    /// The shot's own video folder; `None` = the project default.
    pub dest_dir: Option<PathBuf>,
    pub dest_resolved: String,
    pub dest_overridden: bool,
    /// Project-wide image templates.
    pub loc_root: String,
    pub loc_folder: String,
    pub loc_file: String,
    pub loc_preview: Result<String, String>,
    pub loc_saved: bool,
    pub loc_note: Option<String>,
    /// Outputs badge; the reasons are listed under it.
    pub out_of_date: bool,
    pub out_of_date_why: Vec<String>,
    pub unverified: Vec<String>,
    pub key_older: bool,
    pub presets: Vec<DeliverPresetProps>,
    /// A preset waiting for delete confirmation.
    pub preset_delete: Option<String>,
    pub overridden: Vec<String>,
    pub reset_label: Option<String>,
    pub block: Option<String>,
    pub note: Option<String>,
}

/// Live values for the Properties column; `review: None` is the "No result yet" state.
#[derive(Clone, Debug, Default)]
pub struct LiveProps {
    pub key: Option<KeyProps>,
    pub media: Option<MediaProps>,
    pub hint: Option<HintProps>,
    pub review: Option<ReviewProps>,
    pub deliver: Option<DeliverProps>,
    pub bar: BarProps,
}

/// Values are the app's own strings (producer/finish ids, "green"/"blue", tool ids).
#[derive(Clone, Debug, PartialEq)]
pub enum HintAct {
    ChooseMovie,
    Bypass(bool),
    Producer(String),
    BirefnetVariant(String),
    SamModel(String),
    Finish(String),
    VideomamaChunk(u32),
    ChromaScreen(String),
    ChromaStrength(f64),
    ChromaClipBlack(f64),
    ChromaClipWhite(f64),
    ChromaShrinkGrow(i64),
    ChromaEdgeBlur(i64),
    CleanupGrow(i64),
    CleanupFill(bool),
    CleanupFeather(f64),
    MovieAsMask(bool),
    MovieAlphaChannel(bool),
    MarkedMode { pre: bool, hint: bool },
    ResetRoute,
    Tool(String),
    Brush(f32),
    UndoPrompt,
    ClearPrompts { all: bool },
    PreviewSam,
    GeneratePreMatte { marked: bool },
    GenerateHint { marked: bool },
    ExtractSource,
    ImportMovie,
    CancelMovie,
    RequestDelete { prematte: bool },
    ClearLegacyHoldout,
    ResetChromaColour,
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeyAct {
    Vram(String),
    Temporal(String),
    HintMode(String),
    HintQuality(f32),
    CarryHint(bool),
    CarryHintQuality(f32),
    Despill(f32),
    CutoutLinear(bool),
    Machine(Option<String>),
    ResetOverrides,
}

#[derive(Clone, Debug, PartialEq)]
pub enum MediaAct {
    InputLinear(bool),
    Relink,
    CancelRelink,
    ConfirmMatch(bool),
    ExtractRelinked,
    ChooseCompanion,
    ImportAs { mask: bool },
    StartImport,
    CancelImport,
    StopImport,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ReviewAct {
    CompareA(String),
    CompareB(String),
    Wipe(bool),
    WipeHorizontal(bool),
    GoToDeliver,
    GoToKey,
}

/// Deliver edits. `output` indexes `DeliverProps.outputs`. Export / Export existing / Run full workflow are action-bar
/// items (BarAct); the App routes them.
#[derive(Clone, Debug, PartialEq)]
pub enum DeliverAct {
    SetEnabled { output: usize, on: bool },
    SetSeqFormat { output: usize, exr: bool },
    SetKind { output: usize, video: bool },
    SetContainer { output: usize, container: String },
    SetCodec { output: usize, codec: String },
    SetQuality { output: usize, quality: f32 },
    SetCompression(String),
    SetFps(f64),
    /// Back to each shot's own frame rate.
    UseSourceFps,
    SetRange { done_frames: bool },
    ChooseExportDir,
    ResetExportDir,
    EditLocation { root: String, folder: String, file: String },
    SaveLocation,
    ResetLocation,
    ResetDelivery,
    SavePreset(String),
    ApplyPreset(String),
    RequestDeletePreset(String),
    ConfirmDeletePreset(String),
    CancelDeletePreset,
}

#[derive(Clone, Debug, PartialEq)]
pub enum BarAct {
    Primary,
    Secondary,
    /// Cancel shown in the secondary slot while the stage is busy.
    Cancel,
    Menu(String),
    Fix,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PropAct {
    Key(KeyAct),
    Media(MediaAct),
    Hint(HintAct),
    Review(ReviewAct),
    Deliver(DeliverAct),
    CopyPath(PathBuf),
    RevealPath(PathBuf),
    Bar(BarAct),
}

pub(super) fn draw(ui: &mut Ui, rect: Rect, fx: &Fixture) -> Vec<PropAct> {
    if let (true, Some(lp)) = (fx.live, fx.live_props.as_ref()) {
        return draw_live(ui, rect, fx, lp);
    }
    let mut child = part(ui, rect, "properties");
    let busy_label = fx.busy_percent.map(|p| format!("Keying {p}%"));
    let status = if let Some(p) = fx.busy_percent {
        Some(format!("Keying {p}% · frame 22 / 65 · endor-omarchy"))
    } else if fx.stage == "Key" && fx.blocker.is_none() {
        Some("Last keyed 14:02 on endor-omarchy".to_owned())
    } else {
        None
    };
    let primary = match fx.stage.as_str() {
        "Key" if fx.blocker.is_some() || fx.empty => primary_button("Key shot"),
        "Key" => primary_button("Key again…"),
        "Hint" => primary_button("Generate hint"),
        "Deliver" => primary_button("Export"),
        _ => ghost_button(""),
    };
    let secondary = match fx.stage.as_str() {
        "Hint" => Some(secondary_button("Generate pre-matte")),
        "Review" if !fx.empty => Some(secondary_button("Go to Deliver")),
        _ => None,
    };
    let blocker = fx.blocker.as_ref().map(|b| Blocker {
        text: &b.message,
        fix_button: b.action.as_deref(),
    });
    let busy = busy_label.as_deref().map(|label| BusyState {
        progress_text: label,
        cancel: "Cancel",
        progress: fx.busy_percent.map(|p| p as f32 / 100.0),
    });
    let menu_items: &'static [&'static str] = match fx.stage.as_str() {
        "Hint" => &["Import movie"],
        "Review" if !fx.empty => &["Key again…", "Reveal result"],
        "Deliver" => &["Run full workflow"],
        _ => &[],
    };
    let bar = ActionBar {
        more_menu: (!menu_items.is_empty()).then(|| menu(menu_items)),
        secondary,
        primary,
        blocker,
        status_text: if fx.blocker.is_some() {
            None
        } else {
            status.as_deref()
        },
        busy,
        reduced_motion: true,
    };

    let _ = property::properties_panel(
        &mut child,
        "Properties",
        &fx.stage,
        rect.width(),
        |body| {
            if fx.stage == "Review" && fx.empty {
                empty_state(
                    body.ui(),
                    Icon::Review,
                    "No result yet",
                    "Key this shot to review it.",
                    Some(EmptyButton {
                        label: "Go to Key",
                        primary: false,
                    }),
                );
            } else {
                match fx.stage.as_str() {
                    "Key" => key(body),
                    "Media" => media(body),
                    "Hint" => hint(body),
                    "Review" => review(body, &fx.view),
                    "Deliver" => deliver(body),
                    _ => {}
                }
            }
        },
        Some(bar),
    );
    Vec::new()
}

fn menu(items: &'static [&'static str]) -> Box<dyn FnOnce(&mut Ui)> {
    Box::new(move |ui| {
        for item in items {
            let _ = ui.button(*item);
        }
    })
}

fn key(body: &mut PropBody) {
    section(body, "Key", KEY_SECTIONS[0], None, |r| {
        dropdown_row(
            r,
            "key-machine",
            "Machine",
            "Machine\nRender this shot on the selected machine",
            2,
            &["This machine", "Automatic", "endor-omarchy", "kamino3"],
        );
    });
    section(body, "Key", KEY_SECTIONS[1], None, |r| {
        dropdown_row(
            r,
            "key-model",
            "Model",
            "Model\nKey engine",
            0,
            &["CorridorKey v2"],
        );
        dropdown_row(
            r,
            "key-vram",
            "VRAM profile",
            "VRAM profile\nNeither caps memory.",
            0,
            &["Auto", "Low VRAM"],
        );
        dropdown_row(
            r,
            "key-temporal",
            "Temporal",
            "Temporal\nNeighbouring frames seen by the key",
            0,
            TEMPORAL,
        );
    });
    section(body, "Key", KEY_SECTIONS[2], None, |r| {
        dropdown_row(
            r,
            "key-hint-mode",
            "Hint mode",
            "Hint mode\nHow hints guide the key",
            2,
            &["None", "Full", "Marks"],
        );
        number_row(
            r,
            "Hint quality",
            "Hint quality",
            "key-hint-quality",
            0.95,
            NumSpec::new(0.95, 0.0, 1.0, 0.01, 3),
            true,
            None,
            true,
        );
        check_row(
            r,
            "Carry hint",
            "Carry hint\nUse the hint between temporal windows",
            true,
        );
        number_row(
            r,
            "Carry quality",
            "Carry hint quality · between temporal windows",
            "key-carry-quality",
            0.95,
            NumSpec::new(0.95, 0.0, 1.0, 0.01, 3),
            true,
            None,
            true,
        );
    });
    section(body, "Key", KEY_SECTIONS[3], None, |r| {
        number_row(
            r,
            "Despill strength",
            "Despill strength",
            "key-despill",
            0.5,
            NumSpec::unit_float(0.5),
            true,
            None,
            true,
        );
    });
    section(body, "Key", KEY_SECTIONS[4], None, |r| {
        check_row(
            r,
            "Linear cutout",
            "Linear cutout\nUse a linear cutout for supported output",
            false,
        );
    });
}

fn media(body: &mut PropBody) {
    section(body, "Media", "Source", None, |r| {
        path_row(r, "Source", "Source folder", SOURCE, true);
        mono_row(r, "Frame size", "3200×1800");
        mono_row(r, "Frame rate", "24 fps");
        mono_row(r, "Frames", "0–64 · 65 frames");
    });
    section(body, "Media", "Interpretation", None, |r| {
        dropdown_row(
            r,
            "input-colour",
            "Input colour",
            "Input colour\nInterpret source frames",
            0,
            &["Linear", "sRGB"],
        );
    });
    section(body, "Media", "Paths", None, |r| {
        path_row(r, "Mask", "Mask folder", MASK, true);
        path_row(r, "Hint", "Hint folder", HINT, true);
    });
    section(body, "Media", "Companion alpha / mask frames", None, |r| {
        dropdown_row(
            r,
            "sequence-import-as",
            "Imports as",
            "Imports as\nChoose the destination folder",
            0,
            &["Alpha hint (AlphaHint)", "Mask (VideoMamaMaskHint)"],
        );
        r.row("Frames", "Import alpha or mask image sequence…\nPNG/EXR frames; existing frames are kept", |ui| {
            let _ = ui.add(secondary_button("Import sequence…").tooltip("Import alpha or mask image sequence…\nPNG/EXR frames; existing frames are kept"));
        });
    });
}

fn hint(body: &mut PropBody) {
    section(body, "Hint", "Hint", None, |r| {
        check_row(r, "Bypass hint", "", false);
    });
    section(body, "Hint", "Pre-matte", None, |r| {
        dropdown_row(
            r,
            "prematte-producer",
            "Producer",
            "Producer",
            0,
            &["GVM", "BiRefNet", "Chroma", "SAM 2.1"],
        );
    });
    section(body, "Hint", "Final hint", None, |r| {
        dropdown_row(
            r,
            "hint-method",
            "Method",
            "Method\nGenerate or import a hint",
            0,
            &["MatAnyone2", "VideoMaMa", "Direct", "Import"],
        );
    });
    section(body, "Hint", "Marked frames", None, |r| {
        check_row(
            r,
            "Pre-matte on marked frames",
            "Pre-matte on marked frames\nGenerate pre-matte on marked frames only",
            true,
        );
        check_row(
            r,
            "Use the same marked frames as hints",
            "Use the same marked frames as hints\nGenerate hints on the same marked frames",
            true,
        );
    });
    section(body, "Hint", "Mask cleanup", None, |r| {
        number_row(
            r,
            "Grow / shrink",
            "Grow / shrink",
            "hint-grow",
            0.0,
            NumSpec::pixels(0.0, -100.0, 100.0, false),
            false,
            Some("px"),
            true,
        );
        check_row(r, "Fill holes", "", false);
        number_row(
            r,
            "Feather",
            "Feather",
            "hint-feather",
            0.0,
            NumSpec::pixels(0.0, 0.0, 100.0, false),
            false,
            Some("px"),
            true,
        );
    });
    section(body, "Hint", "Key consumption", None, |r| {
        dropdown_row(
            r,
            "consume-hint-mode",
            "Hint mode",
            "Hint mode\nHow Key consumes the generated hint",
            2,
            &["None", "Full", "Marks"],
        );
        number_row(
            r,
            "Hint quality",
            "Hint quality",
            "hint-quality",
            0.95,
            NumSpec::new(0.95, 0.0, 1.0, 0.01, 3),
            true,
            None,
            true,
        );
        check_row(
            r,
            "Carry hint",
            "Carry hint\nUse hints between temporal windows",
            true,
        );
        number_row(
            r,
            "Carry quality",
            "Carry hint quality · between temporal windows",
            "hint-carry-quality",
            0.95,
            NumSpec::new(0.95, 0.0, 1.0, 0.01, 3),
            true,
            None,
            true,
        );
    });
    section(body, "Hint", "Import source", None, |r| {
        r.row(
            "Movie",
            "Companion movie\nChoose the alpha / mask movie",
            |ui| {
                let _ = ui.add(secondary_button("Choose movie…").tooltip(
                    "Choose alpha / mask movie…\nCompanion movie source for AlphaHint or Pre-matte",
                ));
            },
        );
        dropdown_row(
            r,
            "movie-import-as",
            "Imports as",
            "Imports as\nChoose Hint or Pre-matte",
            0,
            &["AlphaHint", "Pre-matte"],
        );
        control_check_row(
            r,
            "Matte channel",
            "In the alpha channel (else gray)\nRead the movie matte from alpha",
            "Alpha channel",
            true,
        );
        r.row(
            "Import",
            "Import movie\nImport the selected companion movie",
            |ui| {
                let _ = ui.add(
                    secondary_button("Import movie")
                        .tooltip("Import movie\nImport the selected companion movie"),
                );
            },
        );
    });
}

fn review(body: &mut PropBody, view: &str) {
    section(body, "Review", "Result", None, |r| {
        r.readonly("Engine", "CorridorKey v2");
        r.readonly("VRAM profile", "Auto");
        r.readonly("Machine", "endor-omarchy");
        r.readonly("Finished", "Today 14:02");
        r.readonly("Frames", "65 of 65");
    });
    section(body, "Review", "Compare", None, |r| {
        dropdown_row(
            r,
            "review-a",
            "A",
            "A\nFirst comparison view",
            VIEWS.iter().position(|v| *v == view).unwrap_or(5),
            VIEWS,
        );
        dropdown_row(r, "review-b", "B", "B\nSecond comparison view", 0, VIEWS);
        r.row("Wipe", "Wipe direction", |ui| {
            let mut value = 0;
            let options = [Seg::new(0, "Vertical"), Seg::new(1, "Horizontal")];
            let _ = Segmented::new(&mut value, &options).name("Wipe").show(ui);
        });
        number_row(
            r,
            "Position",
            "Position",
            "review-position",
            50.0,
            NumSpec::new(50.0, 0.0, 100.0, 1.0, 0),
            true,
            Some("%"),
            true,
        );
    });
    section(body, "Review", "Checks", None, |r| {
        r.readonly("Missing frames", "None");
        r.readonly("Out of range", "None");
    });
}

fn deliver(body: &mut PropBody) {
    section(
        body,
        "Deliver",
        "Outputs",
        section_menu(&["Save preset…", "Apply preset ›", "Delete preset ›"]),
        |r| {
            output_row(r, "FG", false, false, "EXR · half · ZIP");
            output_row(r, "Matte", true, false, "EXR · half · DWAB");
            output_row(r, "Comp", false, false, "PNG · 16-bit");
            output_row(r, "Processed", true, true, "MOV · ProRes 4444");
        },
    );
    section(
        body,
        "Deliver",
        "Format · Processed",
        section_menu(&["Use this format for all outputs"]),
        |r| {
            let file_types = ["EXR", "PNG", "MP4", "MOV", "WebM"];
            r.row_or_stacked("File type", "File type", 174.0, |ui| {
                let mut value = 3;
                let options: Vec<_> = file_types
                    .iter()
                    .enumerate()
                    .map(|(i, l)| Seg::new(i, *l))
                    .collect();
                let width = ui.available_width();
                let _ = Segmented::new(&mut value, &options)
                    .width(SegWidth::FitVariable)
                    .track_width(width)
                    .name("File type")
                    .show(ui);
            });
            dropdown_row(
                r,
                "deliver-codec",
                "Codec",
                "Codec\nChoose the video encoder",
                0,
                &["ProRes", "H.264", "H.265", "VP9", "MJPEG"],
            );
            dropdown_row(
                r,
                "deliver-quality",
                "Quality",
                "Quality\nProRes profile; independent of qscale",
                1,
                &[
                    "Best · 4444 XQ",
                    "High · 4444",
                    "Medium · HQ",
                    "Small · LT",
                    "Custom",
                ],
            );
            number_row(
                r,
                "Encoder value",
                "ProRes qscale 1–31; empty = encoder default",
                "deliver-qscale",
                9.0,
                NumSpec::integer(9, 1, 31),
                false,
                Some("qscale 1–31"),
                false,
            );
            dropdown_row(
                r,
                "deliver-frame-rate",
                "Frame rate",
                "Frame rate\nChoose the output frame rate",
                0,
                &[
                    "Source", "23.976", "24", "25", "29.97", "30", "50", "59.94", "60",
                ],
            );
            r.row48_tip("Range", "Frames 0–64 · one gap-free keyed run", |ui| {
                let mut value = 0;
                let options = [
                    Seg::new(0, "In/Out"),
                    Seg::new(1, "All frames"),
                    Seg::new(2, "Done frames"),
                ];
                let _ = Segmented::new(&mut value, &options)
                    .width(SegWidth::Equal)
                    .track_width(ui.available_width())
                    .name("Range")
                    .show(ui);
            });
        },
    );
    section(body, "Deliver", "Destination", None, |r| {
        path_row(r, "Output", "Output folder", OUTPUT, true);
        check_row(r, "Keep partial results", "", false);
    });
}

fn section_menu(items: &'static [&'static str]) -> Option<Box<dyn FnOnce(&mut Ui)>> {
    Some(menu(items))
}

fn section(
    body: &mut PropBody,
    stage: &str,
    title: &str,
    menu: Option<Box<dyn FnOnce(&mut Ui)>>,
    rows: impl FnOnce(&mut PropRows),
) {
    section_with(body, stage, title, None, menu, rows);
}

fn section_with<'a>(
    body: &mut PropBody,
    stage: &str,
    title: &str,
    badge: Option<property::Badge<'a>>,
    menu: Option<Box<dyn FnOnce(&mut Ui) + 'a>>,
    rows: impl FnOnce(&mut PropRows),
) {
    let id = body
        .ui()
        .scope_id()
        .with(("shot-props-section", stage, title));
    let ctx = body.ui().ctx().clone();
    let mut collapsed = ctx.data(|d| d.get_temp(id).unwrap_or(false));
    body.section(title, badge, menu, &mut collapsed, rows);
    ctx.data_mut(|d| d.insert_temp(id, collapsed));
}

fn dropdown_row(
    rows: &mut PropRows,
    id: &str,
    label: &str,
    tip: &str,
    selected: usize,
    choices: &[&str],
) -> Option<usize> {
    rows.row(label, tip, |ui| {
        let items: Vec<_> = choices.iter().map(|label| Item::new(*label)).collect();
        let mut value = selected.min(items.len().saturating_sub(1));
        let r = Dropdown::new(id, &mut value, &items, ui.available_width())
            .name(label)
            .tip(tip)
            .show(ui);
        r.changed().then_some(value)
    })
}

/// Returns the value only when an edit committed (drag release, Enter, slider release).
fn number_row(
    rows: &mut PropRows,
    label: &str,
    tip: &str,
    id: &str,
    sample: f64,
    spec: NumSpec,
    slider: bool,
    unit: Option<&str>,
    tick: bool,
) -> Option<f64> {
    rows.row(label, tip, |ui| {
        let mut value = sample;
        let field = NumericField::new(id, &mut value, &spec)
            .name(label)
            .slider(slider)
            .default_tick(tick);
        let field = if let Some(unit) = unit {
            field.unit(unit)
        } else {
            field
        };
        let _ = field.show(ui).on_hover_text(tip);
        // Same id NumericField derives in `show`.
        let nid = ui.scope_id().with(Id::NULL.with(id));
        numeric::committed(ui.ctx(), nid).then_some(value)
    })
}

fn check_row(rows: &mut PropRows, label: &str, tip: &str, checked: bool) -> Option<bool> {
    rows.row_full(false, false, |ui, cols| {
        let mut value = checked;
        let r = ui
            .add(Check::new(&mut value, Some(label)).at(cols.label.min))
            .on_hover_text(tip);
        r.changed().then_some(value)
    })
}

fn control_check_row(
    rows: &mut PropRows,
    label: &'static str,
    tip: &'static str,
    check_label: &'static str,
    checked: bool,
) {
    rows.row(label, tip, |ui| {
        let mut value = checked;
        // Mockup nests the check in the control cell, inset 8 px from its start.
        let at = ui.max_rect().min + egui::vec2(8.0, 0.0);
        let _ = ui
            .add(Check::new(&mut value, Some(check_label)).at(at))
            .on_hover_text(tip);
    });
}

fn mono_row(rows: &mut PropRows, label: &str, value: &str) {
    rows.row(label, "", |ui| {
        let rect = ui.available_rect_before_wrap();
        let p = ui.painter().clone();
        paint_line(
            &p,
            rect,
            line(&p, value, ty::mono(), 14.0, rect.width()),
            14.0,
            Align::Min,
            tokens(ui).text_secondary,
            0.0,
        );
    });
}

#[derive(Clone, Copy, PartialEq)]
enum PathClick {
    Copy,
    Reveal,
}

fn path_row(
    rows: &mut PropRows,
    short_label: &str,
    full_label: &str,
    path: &str,
    copy_local: bool,
) -> Option<PathClick> {
    rows.row_full(false, false, |ui, cols| {
        let row = cols.row;
        let label = Rect::from_min_size(
            row.min + egui::vec2(8.0, 0.0),
            egui::vec2(44.0, row.height()),
        );
        let y = row.center().y - 9.0;
        let reveal_rect = Rect::from_min_size(
            egui::pos2(row.max.x - 8.0 - 18.0, y),
            egui::Vec2::splat(18.0),
        );
        let copy_rect = Rect::from_min_size(
            egui::pos2(reveal_rect.min.x - 4.0 - 18.0, y),
            egui::Vec2::splat(18.0),
        );
        let text_rect = Rect::from_min_max(
            egui::pos2(label.max.x + 4.0, row.min.y),
            egui::pos2(copy_rect.min.x - 4.0, row.max.y),
        );
        let painter = ui.painter().clone();
        let t = tokens(ui);
        paint_line(
            &painter,
            label,
            line(&painter, short_label, ty::label(), 16.0, label.width()),
            16.0,
            Align::Min,
            t.text_secondary,
            0.0,
        );
        ui.interact(
            label,
            ui.scope_id().with(("shot-path-label", short_label)),
            Sense::hover(),
        )
        .on_hover_text(full_label);
        let shown = property::middle_truncate(path, |s| {
            line(&painter, s, ty::mono(), 14.0, f32::INFINITY).size().x <= text_rect.width()
        });
        paint_line(
            &painter,
            text_rect,
            line(&painter, &shown, ty::mono(), 14.0, f32::INFINITY),
            14.0,
            Align::Min,
            t.text_secondary,
            0.0,
        );
        ui.interact(
            text_rect,
            ui.scope_id().with(("shot-path", short_label)),
            Sense::hover(),
        )
        .on_hover_text(path);
        let copy = ui
            .add(icon_button(Icon::Copy, IconButtonSize::Small18, "Copy path").in_rect(copy_rect));
        if copy.clicked() && copy_local {
            ui.ctx().copy_text(path.to_owned());
        }
        let reveal = ui.add(
            icon_button(Icon::Reveal, IconButtonSize::Small18, "Reveal folder")
                .in_rect(reveal_rect),
        );
        if copy.clicked() {
            Some(PathClick::Copy)
        } else if reveal.clicked() {
            Some(PathClick::Reveal)
        } else {
            None
        }
    })
}

fn output_row(
    rows: &mut PropRows,
    name: &'static str,
    checked: bool,
    selected: bool,
    summary: &'static str,
) {
    rows.row_full(selected, false, |ui, cols| {
        let mut checked = checked;
        let check = ui.add(Check::new(&mut checked, None).at(cols.label.min));
        let t = tokens(ui);
        let p = ui.painter().clone();
        let name_rect = Rect::from_min_max(egui::pos2(check.rect.max.x + 6.0, check.rect.top()), cols.label.max);
        paint_line(&p, name_rect, line(&p, name, ty::label(), 16.0, name_rect.width()), 16.0, Align::Min, t.text_primary, 0.0);
        let value_rect = Rect::from_min_max(
            egui::pos2(cols.control.min.x.max(check.rect.max.x + 8.0), cols.control.min.y),
            cols.control.max,
        );
        ui.interact(value_rect, ui.scope_id().with(("deliver-out-summary", name)), Sense::hover())
            .on_hover_text(summary);
        paint_line(
            &p,
            value_rect,
            line(&p, summary, ty::mono(), 14.0, value_rect.width()),
            14.0,
            Align::Min,
            if selected {
                t.text_primary
            } else {
                t.text_secondary
            },
            0.0,
        );
    });
}

fn draw_live(ui: &mut Ui, rect: Rect, fx: &Fixture, lp: &LiveProps) -> Vec<PropAct> {
    let mut child = part(ui, rect, "properties");
    let b = &lp.bar;
    let busy_text = b.busy_percent.map(|p| format!("{p}%"));
    let reason = b.primary_reason.as_deref().unwrap_or("Not available right now");
    let primary = primary_button(&b.primary_label).disabled_if(!b.primary_enabled, reason);
    let secondary = b
        .secondary
        .as_ref()
        .map(|(l, on)| secondary_button(l).disabled_if(!*on, "Not available right now"));
    let blocker = b.blocker.as_ref().map(|(text, fix)| Blocker {
        text,
        fix_button: fix.as_deref(),
    });
    let busy = busy_text.as_deref().map(|label| BusyState {
        progress_text: label,
        cancel: "Cancel",
        progress: b.busy_percent.map(|p| p as f32 / 100.0),
    });
    let mut picked: Option<String> = None;
    let more_menu: Option<Box<dyn FnOnce(&mut Ui) + '_>> = if b.menu.is_empty() {
        None
    } else {
        Some(Box::new(|ui: &mut Ui| {
            for (label, on, reason) in &b.menu {
                let response = ui.add_enabled(*on, egui::Button::new(label));
                let response = if !on {
                    match reason {
                        Some(r) => response.on_disabled_hover_text(r),
                        None => response,
                    }
                } else {
                    response
                };
                if response.clicked() {
                    picked = Some(label.clone());
                }
            }
        }))
    };
    let bar = ActionBar {
        more_menu,
        secondary,
        primary,
        status_text: if b.blocker.is_some() { None } else { b.status.as_deref() },
        blocker,
        busy,
        reduced_motion: true,
    };
    let mut acts = Vec::new();
    let out = property::properties_panel(
        &mut child,
        "Properties",
        &fx.stage,
        rect.width(),
        |body| match fx.stage.as_str() {
            "Key" => {
                if let Some(k) = &lp.key {
                    key_live(body, k, &mut acts);
                }
            }
            "Media" => {
                if let Some(m) = &lp.media {
                    media_live(body, m, &mut acts);
                }
            }
            "Hint" => match &lp.hint {
                Some(h) => hint_live(body, h, &mut acts),
                None => hint(body),
            },
            "Review" => match &lp.review {
                Some(r) => review_live(body, r, &mut acts),
                None => {
                    if empty_state(
                        body.ui(),
                        Icon::Review,
                        "No result yet",
                        "Key this shot to review it.",
                        Some(EmptyButton { label: "Go to Key", primary: false }),
                    ) {
                        acts.push(PropAct::Review(ReviewAct::GoToKey));
                    }
                }
            },
            "Deliver" => match &lp.deliver {
                Some(d) => deliver_live(body, d, &mut acts),
                None => deliver(body),
            },
            _ => {}
        },
        Some(bar),
    );
    if let Some(out) = out {
        let busy = b.busy_percent.is_some();
        if out.primary.clicked() && b.primary_enabled && !busy {
            acts.push(PropAct::Bar(BarAct::Primary));
        }
        if let Some(s) = &out.secondary {
            if s.clicked() {
                if busy {
                    acts.push(PropAct::Bar(BarAct::Cancel));
                } else if b.secondary.as_ref().is_some_and(|(_, on)| *on) {
                    acts.push(if fx.stage == "Review" {
                        PropAct::Review(ReviewAct::GoToDeliver)
                    } else {
                        PropAct::Bar(BarAct::Secondary)
                    });
                }
            }
        }
        if out.fix.as_ref().is_some_and(|f| f.clicked()) {
            acts.push(PropAct::Bar(BarAct::Fix));
        }
    }
    if let Some(m) = picked {
        acts.push(PropAct::Bar(BarAct::Menu(m)));
    }
    acts
}

/// Dropdown over `choices` (value + label); an empty list shows the value read-only.
fn choice_row(
    r: &mut PropRows,
    id: &str,
    label: &str,
    tip: &str,
    current: &str,
    choices: &[Choice],
) -> Option<String> {
    if choices.is_empty() {
        r.readonly(label, current);
        return None;
    }
    let labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
    let sel = choices.iter().position(|c| c.value == current).unwrap_or(0);
    let i = dropdown_row(r, id, label, tip, sel, &labels)?;
    choices.get(i).map(|c| c.value.clone())
}

fn path_acts(click: Option<PathClick>, path: &std::path::Path, acts: &mut Vec<PropAct>) {
    match click {
        Some(PathClick::Copy) => acts.push(PropAct::CopyPath(path.to_owned())),
        Some(PathClick::Reveal) => acts.push(PropAct::RevealPath(path.to_owned())),
        None => {}
    }
}

fn button_row(r: &mut PropRows, label: &str, tip: &str, button: &str, enabled: bool, reason: &str) -> bool {
    r.row(label, tip, |ui| {
        ui.add(secondary_button(button).disabled_if(!enabled, reason).tooltip(tip))
            .clicked()
    })
}

fn key_live(body: &mut PropBody, k: &KeyProps, acts: &mut Vec<PropAct>) {
    let mut push = |a: KeyAct| acts.push(PropAct::Key(a));
    section(body, "Key", KEY_SECTIONS[0], None, |r| {
        if k.machines.is_empty() {
            r.readonly("Machine", &k.machine_effective);
            return;
        }
        let tip = format!("Machine\nRendering on {}", k.machine_effective);
        r.row(&marked_label("Machine", !k.inherited), &tip, |ui| {
            let items: Vec<_> = k
                .machines
                .iter()
                .map(|m| {
                    let it = Item::new(&m.label);
                    if m.available {
                        it
                    } else {
                        it.disabled(m.reason.as_deref().unwrap_or("Unavailable"))
                    }
                })
                .collect();
            let mut sel = k.machines.iter().position(|m| m.id == k.machine).unwrap_or(0);
            let resp = Dropdown::new("key-machine", &mut sel, &items, ui.available_width())
                .name("Machine")
                .tip(&tip)
                .show(ui);
            if resp.changed() {
                if let Some(m) = k.machines.get(sel) {
                    push(KeyAct::Machine(m.id.clone()));
                }
            }
        });
    });
    section(body, "Key", KEY_SECTIONS[1], None, |r| {
        dropdown_row(r, "key-model", "Model", "Model\nKey engine", 0, &[k.model.as_str()]);
        let vram = marked_label("VRAM profile", k.overridden.vram);
        if let Some(v) = choice_row(r, "key-vram", &vram, "VRAM profile\nNeither caps memory.", &k.vram, &k.vram_choices) {
            push(KeyAct::Vram(v));
        }
        let temporal = marked_label("Temporal", k.overridden.temporal);
        if let Some(v) = choice_row(r, "key-temporal", &temporal, "Temporal\nNeighbouring frames seen by the key", &k.temporal, &k.temporal_choices) {
            push(KeyAct::Temporal(v));
        }
    });
    section(body, "Key", KEY_SECTIONS[2], None, |r| {
        let mode = marked_label("Hint mode", k.overridden.hint_mode);
        if let Some(v) = choice_row(r, "key-hint-mode", &mode, "Hint mode\nHow hints guide the key", &k.hint_mode, &k.hint_mode_choices) {
            push(KeyAct::HintMode(v));
        }
        let quality = marked_label("Hint quality", k.overridden.hint_quality);
        if let Some(v) = number_row(r, &quality, "Hint quality", "key-hint-quality", k.hint_quality, NumSpec::new(0.95, 0.0, 1.0, 0.01, 3), true, None, true) {
            push(KeyAct::HintQuality(v as f32));
        }
        let carry = marked_label("Carry hint", k.overridden.carry_hint);
        if let Some(v) = check_row(r, &carry, "Carry hint\nUse the hint between temporal windows", k.carry_hint) {
            push(KeyAct::CarryHint(v));
        }
        let carry_quality = marked_label("Carry quality", k.overridden.carry_hint_quality);
        if let Some(v) = number_row(r, &carry_quality, "Carry hint quality · between temporal windows", "key-carry-quality", k.carry_hint_quality, NumSpec::new(0.95, 0.0, 1.0, 0.01, 3), true, None, true) {
            push(KeyAct::CarryHintQuality(v as f32));
        }
    });
    section(body, "Key", KEY_SECTIONS[3], None, |r| {
        let despill = marked_label("Despill strength", k.overridden.despill);
        if let Some(v) = number_row(r, &despill, "Despill strength", "key-despill", k.despill, NumSpec::unit_float(0.5), true, None, true) {
            push(KeyAct::Despill(v as f32));
        }
    });
    section(body, "Key", KEY_SECTIONS[4], None, |r| {
        let cutout = marked_label("Linear cutout", k.overridden.cutout_linear);
        if let Some(v) = check_row(r, &cutout, "Linear cutout\nUse a linear cutout for supported output", k.cutout_linear) {
            push(KeyAct::CutoutLinear(v));
        }
        let overrides = key_override_names(k);
        if !overrides.is_empty() {
            r.readonly("Shot overrides", &overrides.join(", "));
        }
        if let Some(label) = &k.reset_label {
            if button_row(r, "Overrides", "Return this shot's changed values", label, true, "") {
                push(KeyAct::ResetOverrides);
            }
        }
    });
}

/// Dropdown over `ReadyChoice`s; a model the app found missing is marked in its label but stays pickable.
fn ready_row(r: &mut PropRows, id: &str, label: &str, tip: &str, current: &str, choices: &[ReadyChoice]) -> Option<String> {
    if choices.is_empty() {
        r.readonly(label, current);
        return None;
    }
    let labels: Vec<String> = choices
        .iter()
        .map(|c| if c.ready { c.label.clone() } else { format!("{} · not installed", c.label) })
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let sel = choices.iter().position(|c| c.value == current).unwrap_or(0);
    let i = dropdown_row(r, id, label, tip, sel, &refs)?;
    choices.get(i).map(|c| c.value.clone())
}

fn frames_text(s: &StageProps) -> String {
    let n = format!("{} on disk", s.frames_on_disk);
    if s.stale { format!("{n} · out of date") } else { n }
}

fn marked_label(label: &str, overridden: bool) -> String {
    if overridden { format!("{label} *") } else { label.to_owned() }
}

fn key_override_names(k: &KeyProps) -> Vec<&'static str> {
    let o = &k.overridden;
    [
        (k.machine.is_some(), "Machine"),
        (o.vram, "VRAM profile"),
        (o.temporal, "Temporal"),
        (o.hint_mode, "Hint mode"),
        (o.hint_quality, "Hint quality"),
        (o.carry_hint, "Carry hint"),
        (o.carry_hint_quality, "Carry quality"),
        (o.despill, "Despill strength"),
        (o.cutout_linear, "Linear cutout"),
    ]
    .into_iter()
    .filter_map(|(on, name)| on.then_some(name))
    .collect()
}

fn hint_live(body: &mut PropBody, h: &HintProps, acts: &mut Vec<PropAct>) {
    let mut push = |a: HintAct| acts.push(PropAct::Hint(a));
    let mut paths: Vec<PropAct> = Vec::new();
    let err = property::Status::Error;
    let warn = property::Status::Warning;
    section(body, "Hint", "Hint", None, |r| {
        r.readonly("Work", "Pre-matte and hint run on the local worker. Key runs on the client assigned to this clip.");
        if let Some(v) = check_row(r, "Bypass hint", "Bypass hint\nKey without a hint; your hint settings and frames are kept", h.bypass) {
            push(HintAct::Bypass(v));
        }
        if !h.route.overrides.is_empty() {
            r.readonly("Shot overrides", &h.route.overrides.join(", "));
        }
        if let Some(label) = &h.route.reset_label {
            let tip = format!("Return this shot's own hint settings ({})", h.route.overrides.join(", "));
            if button_row(r, "Overrides", &tip, label, true, "") {
                push(HintAct::ResetRoute);
            }
        }
        if h.route.save_failed {
            r.message(err, "Hint settings could not be saved. They are kept and retried on the next change.");
        } else if h.route.unsaved {
            r.message(warn, "Hint settings not saved yet.");
        }
        if let Some(n) = &h.note {
            r.message(warn, n);
        }
    });
    // Bypass keeps the data but greys everything below it; the rest of this body's ui is disabled from here.
    if h.bypass {
        body.ui().disable();
    }
    section(body, "Hint", "Pre-matte", None, |r| {
        if let Some(v) = ready_row(r, "prematte-producer", "Producer", "Producer\nWhat makes the pre-matte", &h.producer, &h.producers) {
            push(HintAct::Producer(v));
        }
        if let Some(m) = &h.producer_missing {
            r.message(warn, &format!("{m} is not installed. Install it in Models."));
        }
        if let Some(cur) = &h.birefnet_variant {
            if let Some(v) = ready_row(r, "birefnet-variant", "Variant", "Variant\nBiRefNet model variant", cur, &h.birefnet_variants) {
                push(HintAct::BirefnetVariant(v));
            }
        }
        if let Some(cur) = &h.sam_model {
            if let Some(v) = ready_row(r, "sam-model", "Tracker model", "Tracker model\nSAM model used to track the subject", cur, &h.sam_models) {
                push(HintAct::SamModel(v));
            }
        }
        r.readonly("Frames", &frames_text(&h.prematte));
        if let Some(p) = &h.prematte.preview {
            if button_row(r, "Preview", "Preview the pre-matte on the current frame", &p.label, p.enabled, &p.reason) {
                push(HintAct::PreviewSam);
            }
        }
        if button_row(r, "Delete", "Delete generated pre-matte frames", &h.prematte.delete.label, true, "") {
            push(HintAct::RequestDelete { prematte: true });
        }
    });
    if let Some(c) = &h.chroma {
        section(body, "Hint", "Chroma", None, |r| {
            let sel = usize::from(c.screen == "blue");
            if let Some(i) = dropdown_row(r, "chroma-screen", "Screen", "Screen\nColour of the screen to key out", sel, &["Green", "Blue"]) {
                push(HintAct::ChromaScreen(if i == 1 { "blue" } else { "green" }.to_owned()));
            }
            match c.sampled {
                Some([red, g, b]) => {
                    r.readonly("Colour", &format!("Sampled · {red}, {g}, {b}"));
                    if button_row(r, "Colour", "Go back to the automatic screen colour", "Use automatic", true, "") {
                        push(HintAct::ResetChromaColour);
                    }
                }
                None => r.readonly("Colour", "Automatic"),
            }
            if let Some(v) = number_row(r, "Strength", "Strength", "chroma-strength", c.strength, NumSpec::new(1.0, 0.1, 10.0, 0.1, 2), true, None, true) {
                push(HintAct::ChromaStrength(v));
            }
            if let Some(v) = number_row(r, "Clip black", "Clip black", "chroma-clip-black", c.clip_black, NumSpec::new(0.0, 0.0, 1.0, 0.01, 2), true, None, true) {
                push(HintAct::ChromaClipBlack(v));
            }
            if let Some(v) = number_row(r, "Clip white", "Clip white", "chroma-clip-white", c.clip_white, NumSpec::new(1.0, 0.0, 1.0, 0.01, 2), true, None, true) {
                push(HintAct::ChromaClipWhite(v));
            }
            if let Some(v) = number_row(r, "Shrink / grow", "Shrink / grow", "chroma-shrink-grow", c.shrink_grow as f64, NumSpec::pixels(0.0, -100.0, 100.0, false), false, Some("px"), true) {
                push(HintAct::ChromaShrinkGrow(v.round() as i64));
            }
            if let Some(v) = number_row(r, "Edge blur", "Edge blur", "chroma-edge-blur", c.edge_blur as f64, NumSpec::pixels(0.0, 0.0, 100.0, false), false, Some("px"), true) {
                push(HintAct::ChromaEdgeBlur(v.round() as i64));
            }
            match &c.holdout {
                Ok((0, 0)) => {}
                Ok((fg, bg)) => {
                    let tip = "Holdout\nStrokes drawn in the old editor; the key still reads them";
                    r.readonly("Holdout", &format!("{fg} foreground · {bg} background"));
                    if button_row(r, "Holdout", tip, "Clear holdout", true, "") {
                        push(HintAct::ClearLegacyHoldout);
                    }
                }
                Err(e) => {
                    r.message(err, e);
                }
            }
        });
    }
    section(body, "Hint", "Final hint", None, |r| {
        if let Some(v) = ready_row(r, "hint-method", "Method", "Method\nGenerate or import a hint", &h.finish, &h.finishes) {
            push(HintAct::Finish(v));
        }
        if let Some(m) = &h.finish_missing {
            r.message(warn, &format!("{m} is not installed. Install it in Models."));
        }
        if let Some(n) = h.videomama_chunk {
            let tip = "Chunk size\nFrames VideoMaMa processes at a time (1–64); it also needs a pre-matte";
            if let Some(v) = number_row(r, "Chunk size", tip, "videomama-chunk", n as f64, NumSpec::integer(16, 1, 64), false, None, true) {
                push(HintAct::VideomamaChunk(v.round().clamp(1.0, 64.0) as u32));
            }
        }
        r.readonly("Frames", &frames_text(&h.hint));
        if button_row(r, "Delete", "Delete generated hint frames", &h.hint.delete.label, true, "") {
            push(HintAct::RequestDelete { prematte: false });
        }
    });
    if let Some(m) = &h.marked {
        section(body, "Hint", "Marked frames", None, |r| {
            if m.two_rows {
                let tip = "Pre-matte on marked frames\nGenerate pre-matte on marked frames only";
                if let Some(v) = check_row(r, "Pre-matte on marked frames", tip, m.pre) {
                    push(HintAct::MarkedMode { pre: v, hint: m.hint });
                }
                if m.show_hint_row {
                    let tip = "Use the same marked frames as hints\nGenerate hints on the same marked frames";
                    if let Some(v) = check_row(r, "Use the same marked frames as hints", tip, m.hint) {
                        push(HintAct::MarkedMode { pre: m.pre, hint: v });
                    }
                }
            } else if let Some(v) = check_row(r, "Pre-matte and hint on marked frames", "Marked frames\nGenerate the pre-matte and the hint on marked frames only", m.pre) {
                push(HintAct::MarkedMode { pre: v, hint: v });
            }
            if m.in_frame_note {
                r.message(warn, "Full-range hints use In-frame pre-matte.");
            }
            if m.key_uses_marked {
                r.readonly("Key uses", "Marked frames");
            }
            if let Some(g) = &h.marked_gate {
                r.message(warn, g);
            }
            if let Some(e) = &h.marks_err {
                r.message(err, &format!("Marked-frame choice not saved: {e}"));
            }
        });
    }
    section(body, "Hint", "Mask cleanup", None, |r| {
        let note = &h.cleanup.note;
        let tip = |t: &str| if note.is_empty() { t.to_owned() } else { format!("{t}\n{note}") };
        if let Some(v) = number_row(r, "Grow / shrink", &tip("Grow / shrink"), "hint-grow", h.cleanup.grow as f64, NumSpec::pixels(0.0, -100.0, 100.0, false), false, Some("px"), true) {
            push(HintAct::CleanupGrow(v.round() as i64));
        }
        if let Some(v) = check_row(r, "Fill holes", &tip("Fill holes"), h.cleanup.fill_holes) {
            push(HintAct::CleanupFill(v));
        }
        if let Some(v) = number_row(r, "Feather", &tip("Feather"), "hint-feather", h.cleanup.feather, NumSpec::pixels(0.0, 0.0, 100.0, false), false, Some("px"), true) {
            push(HintAct::CleanupFeather(v));
        }
    });
    if h.brush.is_some() || !h.tools.is_empty() {
        section(body, "Hint", "Prompts", None, |r| {
            if !h.tools.is_empty() {
                let mut labels = vec!["Navigate"];
                labels.extend(h.tools.iter().map(|t| t.label.as_str()));
                let sel = h.tools.iter().position(|t| t.value == h.tool).map_or(0, |i| i + 1);
                if let Some(i) = dropdown_row(r, "hint-tool", "Tool", "Tool\nWhat a click on the frame does", sel, &labels) {
                    push(HintAct::Tool(if i == 0 { "none".to_owned() } else { h.tools[i - 1].value.clone() }));
                }
            }
            if let Some(b) = h.brush {
                if let Some(v) = number_row(r, "Brush size", "Brush size", "hint-brush", f64::from(b), NumSpec::pixels(20.0, 2.0, 120.0, false), false, Some("px"), true) {
                    push(HintAct::Brush(v.clamp(2.0, 120.0) as f32));
                }
            }
            r.readonly("Prompts", &format!("{} frame{} annotated", h.prompt_frames, if h.prompt_frames == 1 { "" } else { "s" }));
            if button_row(r, "Undo", "Undo the last prompt change", "Undo", h.can_undo, "Nothing to undo") {
                push(HintAct::UndoPrompt);
            }
            if button_row(r, "Clear frame", "Remove the prompts on the current frame", "Clear frame", h.frame_annotated, "This frame has no prompts") {
                push(HintAct::ClearPrompts { all: false });
            }
            if button_row(r, "Clear all", "Remove the prompts on every frame", "Clear all", h.prompt_frames > 0, "No prompts yet") {
                push(HintAct::ClearPrompts { all: true });
            }
        });
    }
    section(body, "Hint", "Import source", None, |r| {
        if button_row(r, "Movie", "Choose a companion alpha or mask movie", "Choose movie…", true, "") {
            push(HintAct::ChooseMovie);
        }
        if let Some(m) = &h.movie {
            let click = path_row(r, "Movie", "Companion movie", &m.path.to_string_lossy(), false);
            path_acts(click, &m.path, &mut paths);
            let sel = usize::from(m.as_mask);
            if let Some(i) = dropdown_row(r, "movie-import-as", "Imports as", "Imports as\nChoose Hint or Pre-matte", sel, &["AlphaHint", "Pre-matte"]) {
                push(HintAct::MovieAsMask(i == 1));
            }
            let tip = "In the alpha channel (else gray)\nRead the movie matte from alpha";
            r.row("Matte channel", tip, |ui| {
                let mut v = m.alpha_channel;
                let at = ui.max_rect().min + egui::vec2(8.0, 0.0);
                if ui.add(Check::new(&mut v, Some("Alpha channel")).at(at)).on_hover_text(tip).changed() {
                    push(HintAct::MovieAlphaChannel(v));
                }
            });
            if let Some(i) = &h.import {
                if button_row(r, "Import", "Import the selected companion movie", &i.label, i.enabled, &i.reason) {
                    push(HintAct::ImportMovie);
                }
            }
            if button_row(r, "Cancel", "Drop the picked movie", "Cancel import", true, "") {
                push(HintAct::CancelMovie);
            }
        }
    });
    acts.extend(paths);
}

const FPS_PRESETS: [f64; 8] = [23.976, 24.0, 25.0, 29.97, 30.0, 50.0, 59.94, 60.0];
const EXISTING_EXPORT_SUPPORT: &str = "Existing Key export writes selected video outputs only; audio is not included.";
const VIDEO_EXPORT_NOTE: &str = "Audio is not included. Files: <clip>_<output>_<in>-<out>.<mp4|mov|webm>; an existing file is never replaced.";

fn exr_label(s: &str) -> String {
    if s == "none" { "None".to_owned() } else { s.to_uppercase() }
}

/// Output row: checkbox toggles enabled; a click anywhere else on the row only selects it.
fn output_row_live(rows: &mut PropRows, name: &str, enabled: bool, selected: bool, summary: &str) -> (Option<bool>, bool) {
    rows.row_full(selected, false, |ui, cols| {
        // Added before the checkbox so the checkbox (later widget) wins the overlap.
        let row = ui
            .interact(cols.row, ui.scope_id().with(("deliver-out-row", name)), Sense::click())
            .on_hover_text("Select to edit its format below");
        let mut on = enabled;
        let check = ui.add(Check::new(&mut on, None).at(cols.label.min));
        let t = tokens(ui);
        let p = ui.painter().clone();
        let name_rect = Rect::from_min_max(egui::pos2(check.rect.max.x + 6.0, check.rect.top()), cols.label.max);
        paint_line(&p, name_rect, line(&p, name, ty::label(), 16.0, name_rect.width()), 16.0, Align::Min, t.text_primary, 0.0);
        let value_rect = Rect::from_min_max(
            egui::pos2(cols.control.min.x.max(check.rect.max.x + 8.0), cols.control.min.y),
            cols.control.max,
        );
        ui.interact(value_rect, ui.scope_id().with(("deliver-out-summary", name)), Sense::hover())
            .on_hover_text(summary);
        paint_line(
            &p,
            value_rect,
            line(&p, summary, ty::mono(), 14.0, value_rect.width()),
            14.0,
            Align::Min,
            if selected { t.text_primary } else { t.text_secondary },
            0.0,
        );
        (check.changed().then_some(on), row.clicked() && !check.clicked())
    })
}

/// Dropdown over `choices`; a current value the list does not hold shows as an extra "Custom" item that sends nothing.
fn custom_choice_row(r: &mut PropRows, id: &str, label: &str, tip: &str, current: &str, choices: &[Choice]) -> Option<String> {
    let mut labels: Vec<&str> = choices.iter().map(|c| c.label.as_str()).collect();
    let pos = choices.iter().position(|c| c.value == current);
    if pos.is_none() {
        labels.push("Custom");
    }
    let i = dropdown_row(r, id, label, tip, pos.unwrap_or(choices.len()), &labels)?;
    choices.get(i).map(|c| c.value.clone())
}

fn seg_row(ui: &mut Ui, name: &str, labels: &[&str], current: usize) -> Option<usize> {
    let options: Vec<_> = labels.iter().enumerate().map(|(i, l)| Seg::new(i, *l)).collect();
    let mut v = current;
    let w = ui.available_width();
    Segmented::new(&mut v, &options)
        .width(SegWidth::Equal)
        .track_width(w)
        .name(name)
        .show(ui)
        .changed()
        .then_some(v)
}

fn deliver_live(body: &mut PropBody, d: &DeliverProps, acts: &mut Vec<PropAct>) {
    let mut push = |a: DeliverAct| acts.push(PropAct::Deliver(a));
    let mut paths: Vec<PropAct> = Vec::new();
    let ctx = body.ui().ctx().clone();
    let sel_id = body.ui().scope_id().with("deliver-selected");
    let save_id = body.ui().scope_id().with("deliver-save-preset");
    // The selected output is presentation owned by the page; it lives in egui memory across updates.
    let sel = ctx.data(|x| x.get_temp::<usize>(sel_id).unwrap_or(0)).min(d.outputs.len().saturating_sub(1));
    let mut new_sel = sel;
    let mut menu_act: Option<DeliverAct> = None;
    let mut open_save = false;
    let warn = property::Status::Warning;
    let err = property::Status::Error;

    let badge = d.out_of_date.then_some(property::Badge { status: warn, text: "Out of date" });
    let menu: Box<dyn FnOnce(&mut Ui) + '_> = Box::new(|ui: &mut Ui| {
        if ui.button("Save preset…").clicked() {
            open_save = true;
        }
        ui.add_enabled_ui(!d.presets.is_empty(), |ui| {
            ui.menu_button("Apply preset", |ui| {
                for p in &d.presets {
                    if ui.button(&p.label).clicked() {
                        menu_act = Some(DeliverAct::ApplyPreset(p.name.clone()));
                        ui.close();
                    }
                }
            });
            ui.menu_button("Delete preset", |ui| {
                for p in &d.presets {
                    if ui.button(&p.label).clicked() {
                        menu_act = Some(DeliverAct::RequestDeletePreset(p.name.clone()));
                        ui.close();
                    }
                }
            });
        });
        if let Some(l) = &d.reset_label {
            if ui.button(l).clicked() {
                menu_act = Some(DeliverAct::ResetDelivery);
            }
        }
    });
    section_with(body, "Deliver", "Outputs", badge, Some(menu), |r| {
        for (i, o) in d.outputs.iter().enumerate() {
            let title = marked_label(&o.title, o.enabled_overridden || o.format_overridden || o.delivery_overridden);
            let (on, clicked) = output_row_live(r, &title, o.enabled, i == sel, &o.summary);
            if let Some(v) = on {
                push(DeliverAct::SetEnabled { output: i, on: v });
            }
            if clicked {
                new_sel = i;
            }
        }
        if let Some(existing) = &d.confirm_existing {
            if !existing.is_empty() { r.readonly("Existing results", &existing.join(", ")); }
            r.message(warn, &format!("The full workflow may rerun Pre-matte, Hint and Key. {EXISTING_EXPORT_SUPPORT}"));
        }
        if !d.overridden.is_empty() {
            r.readonly("Shot overrides", &d.overridden.join(", "));
        }
        for why in &d.out_of_date_why {
            r.message(warn, why);
        }
        if d.key_older {
            r.message(warn, "The Key result is older than the current Key settings.");
        }
        if !d.unverified.is_empty() {
            r.readonly("Not checked", &d.unverified.join(", "));
        }
        if let Some(b) = &d.block {
            r.message(warn, b);
        }
        if let Some(n) = &d.note {
            r.message(warn, n);
        }
    });

    if let Some(o) = d.outputs.get(sel) {
        let title = format!("Format · {}", o.title);
        section_with(body, "Deliver", &title, None, None, |r| {
            if !o.enabled {
                r.readonly("Output", "Not written · tick it in Outputs");
                return;
            }
            let kind_label = marked_label("File type", o.delivery_overridden);
            r.row_or_stacked(&kind_label, "File type\nImage sequence or one video file", 174.0, |ui| {
                if let Some(v) = seg_row(ui, "File type", &["Image sequence", "Video file"], usize::from(o.video.is_some())) {
                    push(DeliverAct::SetKind { output: sel, video: v == 1 });
                }
            });
            match &o.video {
                None => {
                    let format_label = marked_label("Format", o.format_overridden);
                    r.row(&format_label, "Format\nImage format of every frame", |ui| {
                        if let Some(v) = seg_row(ui, "Format", &["EXR", "PNG"], usize::from(!o.exr)) {
                            push(DeliverAct::SetSeqFormat { output: sel, exr: v == 0 });
                        }
                    });
                    if o.exr {
                        let labels: Vec<String> = d.compression_choices.iter().map(|c| exr_label(c)).collect();
                        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                        let cur = d.compression_choices.iter().position(|c| *c == d.compression).unwrap_or(0);
                        let tip = format!("Compression\n{}", d.compression_note);
                        let compression = marked_label("Compression", d.compression_overridden);
                        if let Some(i) = dropdown_row(r, "deliver-compression", &compression, &tip, cur, &refs) {
                            if let Some(c) = d.compression_choices.get(i) {
                                push(DeliverAct::SetCompression(c.clone()));
                            }
                        }
                        if let Some(n) = &d.compression_lossy {
                            r.message(warn, n);
                        }
                    }
                }
                Some(v) => {
                    let id = |s: &str| format!("deliver-{s}-{sel}");
                    if let Some(c) = custom_choice_row(r, &id("container"), "Container", "Container\nFile wrapper of the video", &v.container, &v.containers) {
                        push(DeliverAct::SetContainer { output: sel, container: c });
                    }
                    if let Some(c) = custom_choice_row(r, &id("codec"), "Codec", "Codec\nVideo encoder", &v.codec, &v.codecs) {
                        push(DeliverAct::SetCodec { output: sel, codec: c });
                    }
                    if v.custom {
                        r.readonly("Quality", &format!("Custom · {}", v.quality));
                    } else {
                        let spec = NumSpec::integer(i64::from(v.quality), i64::from(v.min), i64::from(v.max));
                        let tip = format!("Quality\n{}", v.quality_note);
                        if let Some(q) = number_row(r, "Quality", &tip, &id("quality"), f64::from(v.quality), spec, false, Some(v.quality_name.as_str()), false) {
                            push(DeliverAct::SetQuality { output: sel, quality: q.round() as f32 });
                        }
                    }
                    if let Some(w) = &v.alpha_warning {
                        r.message(warn, w);
                    }
                }
            }
            if d.has_video {
                let mut labels = vec![format!("Source ({} fps)", fmt_fps(d.fps_source))];
                labels.extend(FPS_PRESETS.iter().map(|f| fmt_fps(*f)));
                let cur = match d.fps {
                    None => 0,
                    Some(s) => match FPS_PRESETS.iter().position(|f| (f - s).abs() < 0.0005) {
                        Some(i) => i + 1,
                        None => {
                            labels.push(fmt_fps(s)); // an exact stored rate off the list is kept, never rounded
                            labels.len() - 1
                        }
                    },
                };
                let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
                let fps_label = marked_label("Frame rate", d.fps_overridden);
                if let Some(i) = dropdown_row(r, "deliver-fps", &fps_label, "Frame rate\nShared by every video output", cur, &refs) {
                    if i == 0 {
                        push(DeliverAct::UseSourceFps);
                    } else if let Some(f) = FPS_PRESETS.get(i - 1) {
                        push(DeliverAct::SetFps(*f));
                    }
                }
                let range_label = marked_label("Range", d.done_frames_overridden);
                r.row48_tip(&range_label, "Range\nIn/Out frames, or only the frames the Key run finished", |ui| {
                    if let Some(v) = seg_row(ui, "Range", &["In/Out", "Done frames"], usize::from(d.done_frames)) {
                        push(DeliverAct::SetRange { done_frames: v == 1 });
                    }
                });
                r.readonly("Video export", VIDEO_EXPORT_NOTE);
            }
        });
    }

    if d.has_video {
        let dest = PathBuf::from(&d.dest_resolved);
        section_with(body, "Deliver", "Destination", None, None, |r| {
            let output_label = marked_label("Output", d.dest_overridden);
            let click = path_row(r, &output_label, "Output folder", &d.dest_resolved, false);
            path_acts(click, &dest, &mut paths);
            if button_row(r, "Folder", "Choose where video files are written", "Choose folder…", true, "") {
                push(DeliverAct::ChooseExportDir);
            }
            if d.dest_dir.is_some() && button_row(r, "Reset", "Go back to the project's video folder", "Use project folder", true, "") {
                push(DeliverAct::ResetExportDir);
            }
        });
    }

    section_with(body, "Deliver", "Image location · whole project", None, None, |r| {
        r.readonly("Scope", "Every shot in this project");
        let mut loc = [d.loc_root.clone(), d.loc_folder.clone(), d.loc_file.clone()];
        let mut changed = false;
        for (i, (label, tip)) in [("Root", "Root\nBase folder of image outputs"), ("Folder", "Folder\nFolder template"), ("File", "File\nFile name template")].into_iter().enumerate() {
            let field = &mut loc[i];
            changed |= r.row(label, tip, |ui| {
                let w = ui.available_width();
                text_field(ui, ("deliver-loc", i), field, w).changed()
            });
        }
        if changed {
            let [root, folder, file] = loc;
            push(DeliverAct::EditLocation { root, folder, file });
        }
        match &d.loc_preview {
            Ok(p) => mono_row(r, "Preview", p),
            Err(e) => {
                r.message(err, e);
            }
        }
        if let Some(n) = &d.loc_note {
            r.message(warn, n);
        }
        if button_row(r, "Save", "Save these templates for the whole project", "Save for project", true, "") {
            push(DeliverAct::SaveLocation);
        }
        if button_row(r, "Reset", "Forget the saved templates", "Reset", d.loc_saved, "Nothing saved yet") {
            push(DeliverAct::ResetLocation);
        }
    });

    if new_sel != sel {
        ctx.data_mut(|x| x.insert_temp(sel_id, new_sel));
    }
    if let Some(a) = menu_act {
        push(a);
    }
    if open_save {
        ctx.data_mut(|x| x.insert_temp::<Option<String>>(save_id, Some(String::new())));
    }
    if let Some(mut name) = ctx.data(|x| x.get_temp::<Option<String>>(save_id)).flatten() {
        let replaces = d.presets.iter().any(|p| p.name == name.trim());
        let out = Dialog::new("deliver-save-preset", "Save preset", DialogSize::S)
            .body(|ui| {
                let w = ui.available_width();
                text_field(ui, "deliver-preset-name", &mut name, w);
                if replaces {
                    paragraph(ui, "A preset with this name exists and will be replaced.");
                }
            })
            .footer(|f| f.cancel("Cancel").primary("Save"))
            .show(&ctx);
        if out.primary_clicked && !name.trim().is_empty() {
            push(DeliverAct::SavePreset(name.trim().to_owned()));
        }
        ctx.data_mut(|x| {
            if out.closed {
                x.remove::<Option<String>>(save_id);
            } else {
                x.insert_temp(save_id, Some(name));
            }
        });
    }
    if let Some(name) = &d.preset_delete {
        let text = format!("Delete the preset \"{name}\"? This cannot be undone.");
        let out = Dialog::new("deliver-delete-preset", "Delete preset", DialogSize::S)
            .body(|ui| paragraph(ui, &text))
            .footer(|f| f.cancel("Cancel").destructive("Delete preset"))
            .show(&ctx);
        if out.primary_clicked {
            push(DeliverAct::ConfirmDeletePreset(name.clone()));
        } else if out.closed {
            push(DeliverAct::CancelDeletePreset);
        }
    }
    acts.extend(paths);
}

fn fmt_fps(f: f64) -> String {
    let s = format!("{f:.3}");
    s.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn media_live(body: &mut PropBody, m: &MediaProps, acts: &mut Vec<PropAct>) {
    let mut push = |a: MediaAct| acts.push(PropAct::Media(a));
    let mut paths: Vec<PropAct> = Vec::new();
    section(body, "Media", "Source", None, |r| {
        match &m.source_path {
            Some(p) => {
                let click = path_row(r, "Source", &m.source_kind, &p.to_string_lossy(), false);
                path_acts(click, p, &mut paths);
            }
            None => mono_row(r, "Source", &m.source_kind),
        }
        if let Some(size) = &m.frame_size {
            mono_row(r, "Frame size", size);
        }
        let fps = format!("{} fps{}", fmt_fps(m.frame_rate), if m.fps_assumed { " (assumed)" } else { "" });
        mono_row(r, "Frame rate", &fps);
        let frames = match m.timeline_frames {
            Some(t) if t != m.frame_count => format!("{} frames (timeline {t})", m.frame_count),
            _ => m.frames.clone(),
        };
        mono_row(r, "Frames", &frames);
        if let Some(e) = &m.error {
            r.message(property::Status::Error, e);
        }
        if let Some(n) = &m.note {
            r.message(property::Status::Warning, n);
        }
    });
    section(body, "Media", "Interpretation", None, |r| {
        let sel = if m.input_linear { 0 } else { 1 };
        let saved = if m.stored_linear { "Saved as linear" } else { "Saved as sRGB" };
        let tip = format!("Input colour\nInterpret source frames · {saved}");
        if let Some(i) = dropdown_row(r, "input-colour", "Input colour", &tip, sel, &["Linear", "sRGB"]) {
            push(MediaAct::InputLinear(i == 0));
        }
        if m.colour_save_failed {
            r.message(property::Status::Warning, "Couldn't save the input colour. It will be retried on the next change.");
        }
        if let Some(n) = &m.colour_note {
            r.readonly("Set by", n);
        }
    });
    if !m.paths.is_empty() {
        section(body, "Media", "Paths", None, |r| {
            for p in &m.paths {
                let full = match p.frames {
                    Some(n) => format!("{} folder · {n} frames", p.label),
                    None => format!("{} folder", p.label),
                };
                let click = path_row(r, &p.label, &full, &p.path.to_string_lossy(), false);
                path_acts(click, &p.path, &mut paths);
            }
            if let Some(range) = &m.result_range {
                r.readonly("Result range", range);
            }
        });
    }
    if m.missing || m.relink.is_some() {
        section(body, "Media", "Source movie", None, |r| match &m.relink {
            None => {
                r.message(property::Status::Error, "Source movie not found");
                if button_row(r, "Movie", "Choose a replacement movie", "Relink movie…", true, "") {
                    push(MediaAct::Relink);
                }
            }
            Some(l) => {
                let click = path_row(r, "Movie", "Replacement movie", &l.path.to_string_lossy(), false);
                path_acts(click, &l.path, &mut paths);
                let state = match l.state {
                    RelinkState::Checking => "Checking…",
                    RelinkState::Failed => "Failed",
                    RelinkState::Partial => "Needs confirmation",
                    RelinkState::Verified => "Verified",
                };
                r.readonly("Check", state);
                if let Some(d) = &l.detail {
                    mono_row(r, "Movie", d);
                }
                if let Some(f) = &l.failure {
                    r.message(property::Status::Error, f);
                }
                if l.state == RelinkState::Partial {
                    if let Some(v) = check_row(r, "Same footage", "Same footage\nConfirm this movie matches the source", l.confirmed) {
                        push(MediaAct::ConfirmMatch(v));
                    }
                }
                let why = l.extract_reason.as_deref().unwrap_or("Not available right now");
                if button_row(r, "Extract", "Extract frames from the replacement movie", "Extract frames", l.extract_enabled, why) {
                    push(MediaAct::ExtractRelinked);
                }
                if button_row(r, "Relink", "Stop relinking", "Cancel relink", true, "") {
                    push(MediaAct::CancelRelink);
                }
            }
        });
    }
    section(body, "Media", "Companion alpha / mask frames", None, |r| match &m.companion {
        None => {
            let why = "Not available for this shot";
            if button_row(r, "Frames", "Import alpha or mask image sequence…\nPNG/EXR frames; existing frames are kept", "Import sequence…", m.can_choose_companion, why) {
                push(MediaAct::ChooseCompanion);
            }
        }
        Some(c) => {
            let click = path_row(r, "Folder", "Companion folder", &c.folder.to_string_lossy(), false);
            path_acts(click, &c.folder, &mut paths);
            if !c.running {
                let sel = if c.mask { 1 } else { 0 };
                if let Some(i) = dropdown_row(r, "sequence-import-as", "Imports as", "Imports as\nChoose the destination folder", sel, &["Alpha hint (AlphaHint)", "Mask (VideoMamaMaskHint)"]) {
                    push(MediaAct::ImportAs { mask: i == 1 });
                }
            }
            let ok = match &c.plan {
                Ok((dest, copy, kept)) => {
                    r.readonly("Plan", &format!("Write {copy} · keep {kept}"));
                    let click = path_row(r, "To", "Destination folder", &dest.to_string_lossy(), false);
                    path_acts(click, dest, &mut paths);
                    true
                }
                Err(e) => {
                    r.message(property::Status::Error, e);
                    false
                }
            };
            if c.running {
                if button_row(r, "Import", "Stop the running import", "Stop import", true, "") {
                    push(MediaAct::StopImport);
                }
            } else {
                if button_row(r, "Import", "Copy the frames into the destination", "Start import", ok, "Nothing to import") {
                    push(MediaAct::StartImport);
                }
                if button_row(r, "Cancel", "Drop this proposed import", "Cancel import", true, "") {
                    push(MediaAct::CancelImport);
                }
            }
        }
    });
    acts.extend(paths);
}

fn review_live(body: &mut PropBody, v: &ReviewProps, acts: &mut Vec<PropAct>) {
    let mut push = |a: ReviewAct| acts.push(PropAct::Review(a));
    let has_result = !v.frames.is_empty() || v.last_run.is_some();
    section(body, "Review", "Result", None, |r| {
        if has_result {
            for (name, frames) in &v.frames {
                r.readonly(name, frames);
            }
            r.readonly("Last run", v.last_run.as_deref().unwrap_or("Unknown"));
        } else {
            r.readonly("Result", "No result yet · Key this shot to review it.");
            if button_row(r, "Key", "Open Key for this shot", "Go to Key", true, "") {
                push(ReviewAct::GoToKey);
            }
        }
    });
    section(body, "Review", "Compare", None, |r| {
        let views: Vec<&str> = v.views.iter().map(String::as_str).collect();
        let pos = |s: &str| views.iter().position(|x| *x == s).unwrap_or(0);
        if !views.is_empty() {
            if let Some(i) = dropdown_row(r, "review-a", "A", "A\nFirst comparison view", pos(&v.a), &views) {
                push(ReviewAct::CompareA(views[i].to_owned()));
            }
            if let Some(i) = dropdown_row(r, "review-b", "B", "B\nSecond comparison view", pos(&v.b), &views) {
                push(ReviewAct::CompareB(views[i].to_owned()));
            }
        }
        if let Some(on) = check_row(r, "Wipe", "Wipe\nSplit the picture between A and B", v.wipe) {
            push(ReviewAct::Wipe(on));
        }
        r.row("Direction", "Wipe direction", |ui| {
            let mut value = usize::from(v.horizontal);
            let options = [Seg::new(0, "Vertical"), Seg::new(1, "Horizontal")];
            if Segmented::new(&mut value, &options).name("Wipe direction").show(ui).changed() {
                push(ReviewAct::WipeHorizontal(value == 1));
            }
        });
    });
    if has_result {
        section(body, "Review", "Checks", None, |r| {
            let missing = if v.missing_frames == 0 { "None".to_owned() } else { v.missing_frames.to_string() };
            r.readonly("Missing frames", &missing);
            let range = match v.out_of_range {
                None => "Not measured".to_owned(),
                Some((0, 0)) => "None".to_owned(),
                Some((lo, hi)) => format!("{lo} below 0 · {hi} above 1"),
            };
            r.readonly("Out of range", &range);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, Rect, Vec2};

    fn live_frame(ctx: &egui::Context, fx: &Fixture, lp: &LiveProps, events: Vec<Event>) -> (Vec<egui::epaint::ClippedShape>, Vec<PropAct>) {
        let raw = RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(700.0, 900.0))), events, ..Default::default() };
        let mut acts = vec![];
        let mut output = ctx.run_ui(raw, |ui| acts = draw_live(ui, ui.max_rect(), fx, lp));
        output.textures_delta.clear();
        (output.shapes, acts)
    }

    fn find_text(shapes: &[egui::epaint::ClippedShape], want: &str) -> Option<Pos2> {
        fn find(shape: &egui::Shape, want: &str) -> Option<Pos2> {
            match shape {
                egui::Shape::Text(t) if t.galley.text() == want => Some(t.pos + t.galley.size() * 0.5),
                egui::Shape::Vec(v) => v.iter().find_map(|s| find(s, want)),
                _ => None,
            }
        }
        shapes.iter().find_map(|s| find(&s.shape, want))
    }

    fn click(ctx: &egui::Context, fx: &Fixture, lp: &LiveProps, p: Pos2) -> Vec<PropAct> {
        live_frame(ctx, fx, lp, vec![Event::PointerMoved(p)]);
        live_frame(ctx, fx, lp, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Default::default() }]);
        live_frame(ctx, fx, lp, vec![Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: false, modifiers: Default::default() }]).1
    }

    #[test]
    fn shot_props_key_section_order_matches_the_shot_spec() {
        assert_eq!(
            KEY_SECTIONS,
            ["Render", "Key engine", "Hint", "Despill", "Cutout"]
        );
    }

    #[test]
    fn companion_movie_picker_is_a_clickable_hint_property_without_a_selection() {
        let ctx = egui::Context::default();
        let fx = Fixture { stage: "Hint".into(), live: true, ..Default::default() };
        let lp = LiveProps { hint: Some(HintProps::default()), ..Default::default() };
        let (shapes, _) = live_frame(&ctx, &fx, &lp, vec![]);
        let p = find_text(&shapes, "Choose movie…").expect("movie choice in Hint properties");
        assert!(click(&ctx, &fx, &lp, p).contains(&PropAct::Hint(HintAct::ChooseMovie)));
    }

    #[test]
    fn review_compare_controls_remain_visible_without_a_result() {
        let ctx = egui::Context::default();
        let fx = Fixture { stage: "Review".into(), live: true, ..Default::default() };
        let lp = LiveProps { review: Some(ReviewProps::default()), ..Default::default() };
        let (shapes, _) = live_frame(&ctx, &fx, &lp, vec![]);
        assert!(find_text(&shapes, "No result yet · Key this shot to review it.").is_some());
        assert!(find_text(&shapes, "Compare").is_some());
        let p = find_text(&shapes, "Go to Key").expect("empty-state action");
        assert!(click(&ctx, &fx, &lp, p).contains(&PropAct::Review(ReviewAct::GoToKey)));
    }

    #[test]
    fn hint_properties_explain_where_each_stage_runs() {
        let ctx = egui::Context::default();
        let fx = Fixture { stage: "Hint".into(), live: true, ..Default::default() };
        let lp = LiveProps { hint: Some(HintProps::default()), ..Default::default() };
        let (shapes, _) = live_frame(&ctx, &fx, &lp, vec![]);
        assert!(find_text(&shapes, "Work").is_some());
        assert!(find_text(&shapes, "Pre-matte and hint run on the local worker. Key runs on the client assigned to this clip.").is_some());
    }

    #[test]
    fn override_fields_are_marked_and_listed() {
        let mut key = KeyProps::default();
        key.overridden.hint_mode = true;
        key.overridden.despill = true;
        assert!(marked_label("Hint mode", key.overridden.hint_mode).ends_with(" *"));
        assert_eq!(key_override_names(&key), ["Hint mode", "Despill strength"]);
        let ctx = egui::Context::default();
        let fx = Fixture { stage: "Key".into(), live: true, ..Default::default() };
        let lp = LiveProps { key: Some(key), ..Default::default() };
        let (shapes, _) = live_frame(&ctx, &fx, &lp, vec![]);
        assert!(find_text(&shapes, "Hint mode *").is_some());
        assert!(find_text(&shapes, "Despill strength *").is_some());
        assert!(find_text(&shapes, "Shot overrides").is_some());
    }

    #[test]
    fn deliver_confirmation_describes_only_supported_export_types() {
        assert_eq!(EXISTING_EXPORT_SUPPORT, "Existing Key export writes selected video outputs only; audio is not included.");
        assert_eq!(VIDEO_EXPORT_NOTE, "Audio is not included. Files: <clip>_<output>_<in>-<out>.<mp4|mov|webm>; an existing file is never replaced.");
    }
}
