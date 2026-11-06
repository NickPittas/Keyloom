//! App side of the new Shot page (batch B1d): builds the page's snapshot from the live viewer/clip/job state, draws the
//! real picture and timeline in the page's regions, and turns the page's acts into viewer/App calls.
//! Not routed yet: `workspace` stays the production path until the cutover.
use super::*;
use super::hint::{self, DeliverCmd, DeliverOutput, DeliverView, HintCmd, HintEdit, HintStage, HintView};
use super::media::{MediaCmd, MediaSource, MediaView};
use crate::hints::{Final, Producer};
use crate::project::View as PV;
use crate::tasks::{TaskJob, TaskKind};
use crate::ui::activity::{self, Cancel, Level};
use crate::ui::ai::{IntentView, KeyCmd, KeyEdit, KeyFix, KeyView};
use crate::ui::annotate::Tool;
use crate::ui::controls::transport::Action;
use crate::ui::controls::viewer_toolbar as tb;
use crate::ui::shot_page::{self, Act, ActState, BarAct, BarProps, CardAction, Choice, ChromaProps, CleanupProps, CompanionProps, DeliverAct, DeliverOutputProps, DeliverPresetProps, DeliverProps, DeliverVideoProps, HintAct, HintProps, HintTool, KeyAct, KeyOverrides, KeyProps, LiveProps, MachineChoice, MarkedProps, MediaAct, MediaProps, MovieProps, PathProp, PropAct, ReadyChoice, RelinkProps, ReviewAct, ReviewProps, RouteProps, StageProps};
use crate::ui::viewer::{DisplayOpts, MarkReq};
use crate::workflow::Intent;

/// View names the Review rows and the toolbar share, with the viewer view each one selects.
const VIEW_NAMES: [(&str, PV); 7] = [("Input", PV::Input), ("Hint", PV::Hint), ("Mask", PV::Guide), ("FG", PV::Fg), ("Matte", PV::Matte), ("Comp", PV::Comp), ("Processed", PV::Processed)];

/// What a bar button / menu item / blocker fix does when pressed. Rebuilt from live state on press, never cached.
#[derive(Clone, Debug)]
enum Go {
    Key(KeyCmd),
    Media(MediaCmd),
    Hint(HintCmd),
    Deliver(DeliverCmd),
    Tab(Tab),
    Page(usize),
    Activity,
    Cancel(String),
    /// Nothing to run: show this text.
    Notice(String),
}

/// One stage's action bar: what the page draws (`props`) and what each slot does (same order as `props.menu`).
#[derive(Default)]
struct StageBar {
    props: BarProps,
    primary: Option<Go>,
    secondary: Option<Go>,
    cancel: Option<Go>,
    menu: Vec<Go>,
    fix: Option<Go>,
}

fn disp_id() -> egui::Id {
    egui::Id::unique("shot_wire_display")
}

fn key_cmd_of(a: KeyAct) -> KeyCmd {
    // f32 slider values are rounded so a stored 0.85 does not become 0.8500000238.
    let r = |f: f32| (f as f64 * 10_000.0).round() / 10_000.0;
    match a {
        KeyAct::Vram(v) => KeyCmd::Edit(KeyEdit::VramProfile(v)),
        KeyAct::Temporal(v) => KeyCmd::Edit(KeyEdit::TemporalWindow(v)),
        KeyAct::HintMode(v) => KeyCmd::Edit(KeyEdit::HintMode(v)),
        KeyAct::HintQuality(f) => KeyCmd::Edit(KeyEdit::HintQuality(r(f))),
        KeyAct::CarryHint(b) => KeyCmd::Edit(KeyEdit::CarryHint(b)),
        KeyAct::CarryHintQuality(f) => KeyCmd::Edit(KeyEdit::CarryHintQuality(r(f))),
        KeyAct::Despill(f) => KeyCmd::Edit(KeyEdit::DespillStrength(r(f))),
        KeyAct::CutoutLinear(b) => KeyCmd::Edit(KeyEdit::CutoutLinear(b)),
        KeyAct::Machine(id) => KeyCmd::AssignMachine(id),
        KeyAct::ResetOverrides => KeyCmd::ResetOverrides,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hint_movie_choice_reaches_the_picker_command() {
        assert!(matches!(hint_cmd_of(HintAct::ChooseMovie), Ok(HintCmd::ChooseMovie)));
    }

    #[test]
    fn key_menu_keeps_the_disabled_intent_reason() {
        let intent = IntentView {
            intent: Intent::Replace,
            label: "Replace".into(),
            enabled: false,
            reason: Some("The result is in use.".into()),
        };
        let (label, enabled, reason, cmd) = key_menu_item(&intent, None);
        assert_eq!(label, "Replace");
        assert!(!enabled);
        assert_eq!(reason.as_deref(), Some("The result is in use."));
        assert!(matches!(cmd, KeyCmd::RequestReplace));
    }

    #[test]
    fn task_cards_follow_task_kind_and_publish_target() {
        assert_eq!(task_card_kind(TaskKind::PrematteHint, None), Some("hint"));
        assert_eq!(task_card_kind(TaskKind::SamPreview, None), None);
        assert_eq!(task_card_kind(TaskKind::Gvm, None), Some("prematte"));
        assert_eq!(task_card_kind(TaskKind::Gvm, Some(std::path::Path::new("/clip/AlphaHint"))), Some("hint"));
        assert_eq!(card_kind("Pre-matte hint", None), Some("hint"));
    }
}

fn media_cmd_of(a: MediaAct) -> MediaCmd {
    match a {
        MediaAct::InputLinear(b) => MediaCmd::SetInputLinear(b),
        MediaAct::Relink => MediaCmd::Relink,
        MediaAct::CancelRelink => MediaCmd::CancelRelink,
        MediaAct::ConfirmMatch(b) => MediaCmd::ConfirmMovieMatch(b),
        MediaAct::ExtractRelinked => MediaCmd::ExtractRelinked,
        MediaAct::ChooseCompanion => MediaCmd::ChooseCompanion,
        MediaAct::ImportAs { mask } => MediaCmd::ImportAs { mask },
        MediaAct::StartImport => MediaCmd::StartSequenceImport,
        MediaAct::CancelImport => MediaCmd::CancelSequenceImport,
        MediaAct::StopImport => MediaCmd::StopSequenceImport,
    }
}

fn fix_label(f: KeyFix) -> &'static str {
    match f {
        KeyFix::OpenModels => "Open Models",
        KeyFix::OpenMachines => "Open Machines",
        KeyFix::ResultActions => "Key again…",
        KeyFix::RecoveryDetails => "Show details",
        KeyFix::Relink => "Relink…",
    }
}

/// "Key again…" with an existing result: the page has no choice popup, so it takes the safe new write
/// (new version, else resume); Replace stays an explicit menu item with its confirmation.
fn key_again(kv: &KeyView) -> Go {
    let pick = |w: Intent| kv.intents.iter().find(|i| i.enabled && i.intent == w);
    match pick(Intent::NewVersion).or_else(|| pick(Intent::Resume)) {
        Some(i) => Go::Key(KeyCmd::Submit(i.intent)),
        None if pick(Intent::Replace).is_some() => Go::Key(KeyCmd::RequestReplace),
        None => Go::Notice(kv.intents.iter().find_map(|i| i.reason.clone()).unwrap_or_else(|| "Choose how to key again from the ⋯ menu".into())),
    }
}

fn key_fix(f: KeyFix, kv: &KeyView) -> Go {
    match f {
        KeyFix::OpenModels => Go::Page(3),
        KeyFix::OpenMachines => Go::Page(2),
        KeyFix::RecoveryDetails => Go::Activity,
        KeyFix::Relink => Go::Media(MediaCmd::Relink),
        KeyFix::ResultActions => key_again(kv),
    }
}

fn key_menu_item(i: &IntentView, replace_pending: Option<usize>) -> (String, bool, Option<String>, KeyCmd) {
    let (label, cmd) = match (i.intent, replace_pending) {
        (Intent::Replace, Some(n)) => (format!("Confirm replace ({n} files)"), KeyCmd::ConfirmReplace),
        (Intent::Replace, None) => (i.label.clone(), KeyCmd::RequestReplace),
        (w, _) => (i.label.clone(), KeyCmd::Submit(w)),
    };
    (label, i.enabled, i.reason.clone(), cmd)
}

fn key_bar(kv: &KeyView) -> StageBar {
    let mut b = StageBar::default();
    let p = &mut b.props;
    p.primary_label = kv.primary_label.clone();
    p.primary_enabled = kv.primary_enabled;
    p.primary_reason = kv.blocker.as_ref().map(|k| k.message.clone());
    p.blocker = kv.blocker.as_ref().map(|k| (k.message.clone(), k.fix.map(|f| fix_label(f).to_string())));
    p.status = kv.status_line.clone();
    p.busy_percent = kv.busy_percent;
    b.primary = Some(match kv.primary_intent {
        Some(i) => Go::Key(KeyCmd::Submit(i)),
        None => key_again(kv),
    });
    b.fix = kv.blocker.as_ref().and_then(|k| k.fix).map(|f| key_fix(f, kv));
    b.cancel = kv.cancel_job.as_ref().map(|_| Go::Key(KeyCmd::CancelJob));
    if kv.result_exists {
        for i in kv.intents.iter().filter(|i| i.intent != Intent::New) {
            let (label, enabled, reason, cmd) = key_menu_item(i, kv.replace_pending);
            p.menu.push((label, enabled, reason));
            b.menu.push(Go::Key(cmd));
        }
        if kv.replace_pending.is_some() {
            p.menu.push(("Cancel replace".into(), true, None));
            b.menu.push(Go::Key(KeyCmd::CancelReplace));
        }
    }
    b
}

fn media_bar(mv: &MediaView) -> StageBar {
    let mut b = StageBar::default();
    let p = &mut b.props;
    p.primary_label = "Extract frames".into();
    if let Some(c) = &mv.companion {
        p.primary_label = if c.mask { "Import mask frames" } else { "Import hint frames" }.into();
        if c.running {
            p.busy_percent = Some(0);
            p.status = Some("Importing frames…".into());
            b.cancel = Some(Go::Media(MediaCmd::StopSequenceImport));
        } else {
            p.primary_enabled = c.plan.is_ok();
            p.primary_reason = c.plan.as_ref().err().cloned();
            b.primary = Some(Go::Media(MediaCmd::StartSequenceImport));
            p.menu.push(("Cancel import".into(), true, None));
            b.menu.push(Go::Media(MediaCmd::CancelSequenceImport));
        }
    } else if let Some(d) = &mv.extract {
        p.primary_label = d.label.clone();
        p.primary_enabled = d.enabled;
        p.primary_reason = (!d.enabled).then(|| d.reason.clone());
        b.primary = Some(Go::Media(MediaCmd::ExtractRelinked));
    } else {
        p.primary_reason = Some("Nothing to extract: relink a missing movie first".into());
    }
    b
}

fn hint_bar(hv: &HintView, head: Option<&activity::Step>) -> StageBar {
    let mut b = StageBar::default();
    let p = &mut b.props;
    let marks = hv.marked.as_ref();
    let pre_on = marks.is_some_and(|m| m.pre);
    let hint_on = pre_on && marks.is_some_and(|m| m.hint || hv.finish != Final::Matanyone2);
    let (g, pre) = (&hv.hint.generate, &hv.prematte.generate);
    p.primary_label = g.label.clone();
    p.primary_enabled = g.enabled;
    p.primary_reason = (!g.enabled).then(|| g.reason.clone());
    b.primary = Some(Go::Hint(HintCmd::GenerateHint { marked: hint_on }));
    p.secondary = Some((pre.label.clone(), pre.enabled));
    b.secondary = Some(Go::Hint(HintCmd::GeneratePreMatte { marked: pre_on }));
    if !g.enabled && !g.reason.is_empty() {
        p.blocker = Some((g.reason.clone(), None));
    }
    let mut item = |label: &str, on: bool, cmd: HintCmd| {
        p.menu.push((label.to_string(), on, None));
        b.menu.push(Go::Hint(cmd));
    };
    item("Import movie", hv.import.as_ref().is_some_and(|a| a.enabled), HintCmd::ImportMovie);
    if let Some(e) = &hv.extract {
        item("Extract frames", e.enabled, HintCmd::ExtractSource);
    }
    if let Some(pv) = &hv.prematte.preview {
        item("Preview frame", pv.enabled, HintCmd::PreviewSam);
    }
    item("Delete pre-matte frames…", true, HintCmd::RequestDelete { prematte: true });
    item("Delete hint frames…", true, HintCmd::RequestDelete { prematte: false });
    if let Some(h) = head {
        p.busy_percent = Some((h.total > 0).then(|| (h.done * 100 / h.total).min(100) as u8).unwrap_or_default());
        p.status = Some(h.stage.clone());
        if let Cancel::Job(id) = &h.cancel {
            b.cancel = Some(Go::Cancel(id.clone()));
        }
    }
    b
}

fn producer_id(p: Producer) -> &'static str {
    match p {
        Producer::Sam => "sam",
        Producer::Gvm => "gvm",
        Producer::Birefnet => "birefnet",
        Producer::Chroma => "chroma",
    }
}

fn final_id(f: Final) -> &'static str {
    match f {
        Final::Direct => "direct",
        Final::Matanyone2 => "matanyone2",
        Final::Videomama => "videomama",
        Final::Import => "import",
    }
}

fn tool_id(t: Tool) -> &'static str {
    match t {
        Tool::None => "none",
        Tool::FgPoint => "fg_point",
        Tool::BgPoint => "bg_point",
        Tool::Box => "box",
        Tool::PaintFg => "paint_fg",
        Tool::PaintBg => "paint_bg",
        Tool::Erase => "erase",
        Tool::Pick => "pick",
        Tool::HoldFg => "hold_fg",
        Tool::HoldBg => "hold_bg",
        Tool::EraseHold => "erase_hold",
    }
}

/// The value of a page id back to its app value; ids come from this file's own lists, so a miss is a stale page.
fn by_id<T: Copy>(all: &[T], id: &str, of: fn(T) -> &'static str) -> Option<T> {
    all.iter().copied().find(|&x| of(x) == id)
}

fn unknown(what: &str, id: &str) -> String {
    format!("Unknown {what} {id}")
}

/// A Hint act as the Hint command it stands for.
fn hint_cmd_of(a: HintAct) -> Result<HintCmd, String> {
    Ok(match a {
        HintAct::ChooseMovie => HintCmd::ChooseMovie,
        HintAct::Bypass(b) => HintCmd::Edit(HintEdit::Bypass(b)),
        HintAct::Producer(s) => HintCmd::Edit(HintEdit::Producer(by_id(&Producer::ALL, &s, producer_id).ok_or_else(|| unknown("producer", &s))?)),
        HintAct::BirefnetVariant(v) => HintCmd::Edit(HintEdit::BirefnetVariant(v)),
        HintAct::SamModel(m) => HintCmd::Edit(HintEdit::SamModel(m)),
        HintAct::Finish(s) => HintCmd::Edit(HintEdit::Finish(by_id(&Final::ALL, &s, final_id).ok_or_else(|| unknown("finish", &s))?)),
        HintAct::VideomamaChunk(n) => HintCmd::Edit(HintEdit::VideomamaChunk(n)),
        HintAct::ChromaScreen(s) => HintCmd::Edit(HintEdit::ChromaScreen(s)),
        HintAct::ChromaStrength(v) => HintCmd::Edit(HintEdit::ChromaStrength(v)),
        HintAct::ChromaClipBlack(v) => HintCmd::Edit(HintEdit::ChromaClipBlack(v)),
        HintAct::ChromaClipWhite(v) => HintCmd::Edit(HintEdit::ChromaClipWhite(v)),
        HintAct::ChromaShrinkGrow(v) => HintCmd::Edit(HintEdit::ChromaShrinkGrow(v)),
        HintAct::ChromaEdgeBlur(v) => HintCmd::Edit(HintEdit::ChromaEdgeBlur(v)),
        HintAct::CleanupGrow(v) => HintCmd::Edit(HintEdit::CleanupGrow(v)),
        HintAct::CleanupFill(b) => HintCmd::Edit(HintEdit::CleanupFill(b)),
        HintAct::CleanupFeather(v) => HintCmd::Edit(HintEdit::CleanupFeather(v)),
        HintAct::MovieAsMask(b) => HintCmd::Edit(HintEdit::MovieAsMask(b)),
        HintAct::MovieAlphaChannel(b) => HintCmd::Edit(HintEdit::MovieAlphaChannel(b)),
        HintAct::MarkedMode { pre, hint } => HintCmd::MarkedMode { pre, hint },
        HintAct::ResetRoute => HintCmd::ResetRoute,
        HintAct::Tool(s) => HintCmd::Tool(by_id(&Tool::ALL, &s, tool_id).ok_or_else(|| unknown("tool", &s))?),
        HintAct::Brush(b) => HintCmd::Brush(b),
        HintAct::UndoPrompt => HintCmd::UndoPrompt,
        HintAct::ClearPrompts { all } => HintCmd::ClearPrompts { all },
        HintAct::PreviewSam => HintCmd::PreviewSam,
        HintAct::GeneratePreMatte { marked } => HintCmd::GeneratePreMatte { marked },
        HintAct::GenerateHint { marked } => HintCmd::GenerateHint { marked },
        HintAct::ExtractSource => HintCmd::ExtractSource,
        HintAct::ImportMovie => HintCmd::ImportMovie,
        HintAct::CancelMovie => HintCmd::CancelMovie,
        HintAct::RequestDelete { prematte } => HintCmd::RequestDelete { prematte },
        HintAct::ClearLegacyHoldout => HintCmd::ClearLegacyHoldout,
        HintAct::ResetChromaColour => HintCmd::ResetChromaColour,
    })
}

/// A Deliver act as the Deliver command it stands for. Output indexes follow `DeliverView.outputs` (Fg, Matte, Comp, Processed).
fn deliver_cmd_of(a: DeliverAct) -> Result<DeliverCmd, String> {
    let out = |i: usize| DeliverOutput::ALL.get(i).copied().ok_or_else(|| format!("Unknown output {i}"));
    Ok(match a {
        DeliverAct::SetEnabled { output, on } => DeliverCmd::SetEnabled { output: out(output)?, on },
        DeliverAct::SetSeqFormat { output, exr } => DeliverCmd::SetSeqFormat { output: out(output)?, exr },
        DeliverAct::SetKind { output, video } => DeliverCmd::SetKind { output: out(output)?, video },
        DeliverAct::SetContainer { output, container } => DeliverCmd::SetContainer { output: out(output)?, container },
        DeliverAct::SetCodec { output, codec } => DeliverCmd::SetCodec { output: out(output)?, codec },
        DeliverAct::SetQuality { output, quality } => DeliverCmd::SetQuality { output: out(output)?, quality },
        DeliverAct::SetCompression(s) => DeliverCmd::SetCompression(s),
        DeliverAct::SetFps(f) => DeliverCmd::SetFps(f),
        DeliverAct::UseSourceFps => DeliverCmd::UseSourceFps,
        DeliverAct::SetRange { done_frames } => DeliverCmd::SetRange { done_frames },
        DeliverAct::ChooseExportDir => DeliverCmd::ChooseExportDir,
        DeliverAct::ResetExportDir => DeliverCmd::ResetExportDir,
        DeliverAct::EditLocation { root, folder, file } => DeliverCmd::EditLocation { root, folder, file },
        DeliverAct::SaveLocation => DeliverCmd::SaveLocation,
        DeliverAct::ResetLocation => DeliverCmd::ResetLocation,
        DeliverAct::ResetDelivery => DeliverCmd::ResetDelivery,
        DeliverAct::SavePreset(n) => DeliverCmd::SavePreset(n),
        DeliverAct::ApplyPreset(n) => DeliverCmd::ApplyPreset(n),
        DeliverAct::RequestDeletePreset(n) => DeliverCmd::RequestDeletePreset(n),
        DeliverAct::ConfirmDeletePreset(n) => DeliverCmd::ConfirmDeletePreset(n),
        DeliverAct::CancelDeletePreset => DeliverCmd::CancelDeletePreset,
    })
}

/// The Deliver bar: Export, or Confirm / Export existing while a full-workflow confirmation waits; the other runs in the menu.
fn deliver_bar(dv: &DeliverView, head: Option<&activity::Step>) -> StageBar {
    let mut b = StageBar::default();
    let p = &mut b.props;
    p.primary_label = dv.export.label.clone();
    p.primary_enabled = dv.export.enabled;
    p.primary_reason = (!dv.export.enabled && !dv.export.reason.is_empty()).then(|| dv.export.reason.clone());
    p.blocker = dv.block.clone().map(|m| (m, None));
    b.primary = Some(Go::Deliver(DeliverCmd::Export));
    if let Some(c) = &dv.confirm {
        p.primary_label = "Run full workflow again".into();
        p.primary_enabled = true;
        p.primary_reason = None;
        b.primary = Some(Go::Deliver(DeliverCmd::ConfirmWorkflow));
        p.secondary = Some(("Export existing Key result instead".into(), c.can_export_existing));
        b.secondary = Some(Go::Deliver(DeliverCmd::ExportExisting));
    }
    let mut item = |label: &str, on: bool, cmd: DeliverCmd| {
        p.menu.push((label.into(), on, None));
        b.menu.push(Go::Deliver(cmd));
    };
    item("Export existing", dv.export_existing.enabled, DeliverCmd::ExportExisting);
    item("Run full workflow…", dv.workflow.enabled, DeliverCmd::RunWorkflow);
    item("Reset delivery", dv.reset_label.is_some(), DeliverCmd::ResetDelivery);
    if dv.confirm.is_some() {
        item("Cancel", true, DeliverCmd::CancelWorkflow);
    }
    if let Some(h) = head {
        p.busy_percent = Some((h.total > 0).then(|| (h.done * 100 / h.total).min(100) as u8).unwrap_or_default());
        p.status = Some(h.stage.clone());
        if let Cancel::Job(id) = &h.cancel {
            b.cancel = Some(Go::Cancel(id.clone()));
        }
    }
    b
}

fn act_state(a: &super::hint::HintAct) -> ActState {
    ActState { enabled: a.enabled, label: a.label.clone(), reason: a.reason.clone() }
}

fn ready_choices<T: Clone>(cs: &[super::hint::Choice<T>], id: impl Fn(&T) -> String) -> Vec<ReadyChoice> {
    cs.iter().map(|c| ReadyChoice { value: id(&c.value), label: c.label.clone(), ready: c.ready }).collect()
}

/// The Hint stage's values for the page (mirror of `HintView`; ids are the strings `hint_cmd_of` reads back).
fn hint_props_of(hv: &HintView) -> HintProps {
    let stage = |s: &HintStage| StageProps {
        frames_on_disk: s.frames_on_disk,
        stale: s.stale,
        preview: s.preview.as_ref().map(act_state),
        generate: act_state(&s.generate),
        delete: act_state(&s.delete),
    };
    HintProps {
        bypass: hv.bypass,
        worker_unknown: hv.worker_unknown,
        producer: producer_id(hv.producer).into(),
        producers: ready_choices(&hv.producers, |p| producer_id(*p).into()),
        producer_missing: hv.producer_missing.clone(),
        birefnet_variant: hv.birefnet_variant.clone(),
        birefnet_variants: ready_choices(&hv.birefnet_variants, |v| v.clone()),
        sam_model: hv.sam_model.clone(),
        sam_models: ready_choices(&hv.sam_models, |v| v.clone()),
        chroma: hv.chroma.as_ref().map(|c| ChromaProps {
            screen: c.cfg.screen_type.clone(),
            sampled: c.cfg.screen_color,
            strength: c.cfg.strength,
            clip_black: c.cfg.clip_black,
            clip_white: c.cfg.clip_white,
            shrink_grow: c.cfg.shrink_grow,
            edge_blur: c.cfg.edge_blur,
            holdout: c.holdout.clone(),
        }),
        finish: final_id(hv.finish).into(),
        finishes: ready_choices(&hv.finishes, |f| final_id(*f).into()),
        finish_missing: hv.finish_missing.clone(),
        videomama_chunk: hv.videomama_chunk,
        marked: hv.marked.as_ref().map(|m| MarkedProps { pre: m.pre, hint: m.hint, two_rows: m.two_rows, show_hint_row: m.show_hint_row, in_frame_note: m.in_frame_note, key_uses_marked: m.key_uses_marked }),
        marked_gate: hv.marked_gate.clone(),
        marks_err: hv.marks_err.clone(),
        cleanup: CleanupProps { grow: hv.cleanup.grow, fill_holes: hv.cleanup.fill_holes, feather: hv.cleanup.feather, note: hv.cleanup_note.into() },
        movie: hv.movie.as_ref().map(|m| MovieProps { path: m.path.clone(), as_mask: m.as_mask, alpha_channel: m.alpha_channel }),
        import: hv.import.as_ref().map(act_state),
        tool: tool_id(hv.tool).into(),
        tools: hv.tools.iter().map(|&(t, l, tip)| HintTool { value: tool_id(t).into(), label: l.into(), tip: tip.into() }).collect(),
        brush: hv.brush,
        prompt_frames: hv.prompt_frames,
        frame_annotated: hv.frame_annotated,
        can_undo: hv.can_undo,
        prematte: stage(&hv.prematte),
        hint: stage(&hv.hint),
        route: RouteProps { overrides: hv.route.overrides.clone(), reset_label: hv.route.reset_label.clone(), unsaved: hv.route.unsaved, save_failed: hv.route.save_failed },
        note: hv.note.clone(),
    }
}

fn choice_list(cs: &[super::hint::Choice<String>]) -> Vec<Choice> {
    cs.iter().map(|c| Choice { value: c.value.clone(), label: c.label.clone() }).collect()
}

fn deliver_video_props(v: &super::hint::DeliverVideo) -> DeliverVideoProps {
    DeliverVideoProps {
        container: v.container.clone(),
        codec: v.codec.clone(),
        quality: v.quality,
        custom: v.custom,
        quality_name: v.quality_name.into(),
        min: v.min,
        max: v.max,
        quality_note: v.quality_note.clone(),
        alpha_warning: v.alpha_warning.map(String::from),
        containers: choice_list(&v.containers),
        codecs: choice_list(&v.codecs),
    }
}

/// The Deliver stage's values for the page (mirror of `DeliverView`).
fn deliver_props_of(dv: &DeliverView) -> DeliverProps {
    DeliverProps {
        outputs: dv
            .outputs
            .iter()
            .map(|o| DeliverOutputProps {
                title: o.title.into(),
                enabled: o.enabled,
                exr: o.exr,
                video: o.video.as_ref().map(deliver_video_props),
                summary: o.summary.clone(),
                enabled_overridden: o.enabled_overridden,
                format_overridden: o.format_overridden,
                delivery_overridden: o.delivery_overridden,
            })
            .collect(),
        confirm_existing: dv.confirm.as_ref().map(|c| c.existing.clone()),
        compression: dv.compression.value.clone(),
        compression_choices: dv.compression.choices.iter().map(|s| s.to_string()).collect(),
        compression_note: dv.compression.shared_note.into(),
        compression_lossy: dv.compression.lossy_note.map(String::from),
        compression_overridden: dv.compression.overridden,
        fps: dv.fps.stored,
        fps_source: dv.fps.source,
        fps_overridden: dv.fps.overridden,
        has_video: dv.has_video,
        done_frames: dv.done_frames,
        done_frames_overridden: dv.done_frames_overridden,
        dest_dir: dv.destination.dir.clone(),
        dest_resolved: dv.destination.resolved.clone(),
        dest_overridden: dv.destination.overridden,
        loc_root: dv.location.root.clone(),
        loc_folder: dv.location.folder.clone(),
        loc_file: dv.location.file.clone(),
        loc_preview: dv.location.preview.clone(),
        loc_saved: dv.location.saved,
        loc_note: dv.location.note.clone(),
        out_of_date: dv.out_of_date,
        out_of_date_why: dv.out_of_date_why.clone(),
        unverified: dv.unverified.clone(),
        key_older: dv.key_older,
        presets: dv.presets.iter().map(|p| DeliverPresetProps { name: p.name.clone(), label: p.label.clone(), legacy: p.legacy }).collect(),
        preset_delete: dv.preset_delete.clone(),
        overridden: dv.overridden.clone(),
        reset_label: dv.reset_label.clone(),
        block: dv.block.clone(),
        note: dv.note.clone(),
    }
}

/// Card kind of an activity step: the Key job, a pre-matte run or a hint run/import (other steps are no cards).
fn card_kind(stage: &str, job: Option<&TaskJob>) -> Option<&'static str> {
    if let Some(job) = job {
        return task_card_kind(job.kind, job.publish_dir.as_deref());
    }
    match stage {
        "Key" => Some("key"),
        s if s.starts_with("Pre-matte") && !s.to_lowercase().contains("hint") => Some("prematte"),
        s if s.to_lowercase().contains("hint") => Some("hint"),
        _ => None,
    }
}

fn task_card_kind(kind: TaskKind, publish_dir: Option<&std::path::Path>) -> Option<&'static str> {
    match kind {
        TaskKind::Gvm | TaskKind::Birefnet | TaskKind::Chroma | TaskKind::SamTrack => {
            Some(if publish_dir.and_then(std::path::Path::file_name).is_some_and(|n| n == "AlphaHint") { "hint" } else { "prematte" })
        }
        TaskKind::Matanyone2 | TaskKind::Videomama | TaskKind::PrematteHint | TaskKind::ImportHint => Some("hint"),
        TaskKind::SamPreview | TaskKind::ExportVideo | TaskKind::PrepareSource => None,
    }
}

fn title_case(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
}

fn choices(values: &[&str], label: impl Fn(&str) -> String) -> Vec<Choice> {
    values.iter().map(|v| Choice { value: v.to_string(), label: label(v) }).collect()
}

fn key_props(kv: &KeyView, clip: &Clip) -> KeyProps {
    use crate::settings::{temporal_label, HINT_MODES, TEMPORAL_WINDOWS, VRAM_PROFILES};
    KeyProps {
        machine: kv.machine.clone(),
        machine_effective: kv.machine_effective.clone(),
        machines: kv.machines.iter().map(|m| MachineChoice { id: m.id.clone(), label: m.label.clone(), available: m.available, reason: m.reason.clone() }).collect(),
        inherited: kv.machine.is_none(),
        model: clip.manifest.as_ref().map_or_else(|| "Default".to_string(), |m| m.engine.clone()),
        vram: kv.vram_profile.value.clone(),
        vram_choices: choices(&VRAM_PROFILES, title_case),
        temporal: kv.temporal_window.value.clone(),
        temporal_choices: choices(&TEMPORAL_WINDOWS, temporal_label),
        hint_mode: kv.hint_mode.value.clone(),
        hint_mode_choices: choices(&HINT_MODES, title_case),
        hint_quality: kv.hint_quality.value,
        carry_hint: kv.carry_hint.value,
        carry_hint_quality: kv.carry_hint_quality.value,
        despill: kv.despill_strength.value,
        cutout_linear: kv.cutout_linear.value,
        overridden: KeyOverrides {
            vram: kv.vram_profile.overridden,
            temporal: kv.temporal_window.overridden,
            hint_mode: kv.hint_mode.overridden,
            hint_quality: kv.hint_quality.overridden,
            carry_hint: kv.carry_hint.overridden,
            carry_hint_quality: kv.carry_hint_quality.overridden,
            despill: kv.despill_strength.overridden,
            cutout_linear: kv.cutout_linear.overridden,
        },
        reset_label: kv.reset_label.clone(),
    }
}

impl App {
    /// Deliver output `output_index` (Fg, Matte, Comp, Processed) as a video file or an image sequence (test hook).
    #[doc(hidden)]
    pub fn deliver_set_video_for_test(&mut self, ctx: &egui::Context, output_index: usize, video: bool) -> Result<(), String> {
        let output = *DeliverOutput::ALL.get(output_index).ok_or("no such output")?;
        self.deliver_cmd(ctx, DeliverCmd::SetKind { output, video })
    }

    /// A Deliver preset step: op is "save", "apply", "request_delete" or "confirm_delete" (test hook).
    #[doc(hidden)]
    pub fn deliver_preset_for_test(&mut self, ctx: &egui::Context, op: &str, name: &str) -> Result<(), String> {
        let n = name.to_string();
        let c = match op {
            "save" => DeliverCmd::SavePreset(n),
            "apply" => DeliverCmd::ApplyPreset(n),
            "request_delete" => DeliverCmd::RequestDeletePreset(n),
            "confirm_delete" => DeliverCmd::ConfirmDeletePreset(n),
            _ => return Err(format!("unknown preset op {op}")),
        };
        self.deliver_cmd(ctx, c)
    }

    /// The Deliver outputs as (key, delivered as video) (test hook).
    #[doc(hidden)]
    pub fn deliver_outputs_for_test(&self) -> Vec<(String, bool)> {
        self.deliver_view().map(|v| v.outputs.iter().map(|o| (o.output.key().to_string(), o.video.is_some())).collect()).unwrap_or_default()
    }

    /// Reset the Hint route (test hook; the one Hint command the tests need).
    #[doc(hidden)]
    pub fn hint_reset_route_for_test(&mut self, ctx: &egui::Context) -> Result<(), String> {
        self.hint_cmd(ctx, HintCmd::ResetRoute)
    }

    /// One act of the Shot page, as if the page had sent it (test hook).
    #[doc(hidden)]
    pub fn act_for_test(&mut self, ctx: &egui::Context, a: Act) {
        match &a {
            Act::DockTab(t) => self.shot_page.set_dock_tab(*t),
            Act::DockExpanded(b) => self.shot_page.set_dock_expanded(*b),
            _ => {}
        }
        self.handle_shot_act(ctx, a);
    }

    /// Real state -> the page's snapshot (header, transport, Properties, action cards).
    pub(super) fn refresh_shot_page(&mut self) {
        let mut fx = shot_page::Fixture { live: true, ..Default::default() };
        self.deliver_tick();
        if let Some(clip) = self.clip().cloned() {
            fx.shot_id = clip.folder.clone();
            fx.shot = clip.display_name.clone();
            fx.stage = self.tab.label().to_string();
            fx.clip_stage = match clip.state { ClipState::Masked => "mask", ClipState::Ready => "ready", ClipState::Complete => "done", _ => "raw" }.into();
            if let Some(p) = &self.project {
                fx.project = p.display_name.clone();
            }
            // Running job of this clip: the active head step of the activity model (same source as the old strip).
            let (steps, _) = self.ai.activity_steps(&clip.root);
            if let Some(h) = activity::build(steps).head.filter(|h| h.level == Level::Active) {
                fx.job_label = Some(h.stage);
                fx.busy_percent = (h.total > 0).then(|| (h.done * 100 / h.total).min(100) as u8);
            }
            self.shot_fill(&clip, &mut fx);
        }
        if let Some(v) = &self.viewer {
            let at = |i: usize| v.index.frame_number(i) as i64;
            fx.first = v.first_frame();
            fx.last = v.last_frame();
            fx.frame = v.current_frame();
            fx.frame_count = v.frame_count();
            fx.in_point = v.range.map(|r| at(r.0));
            fx.out_point = v.range.map(|r| at(r.1));
            fx.marks = v.marks.iter().map(|&i| at(i)).collect();
            fx.cached = vec![]; // later batch: cache coverage
            fx.fps = v.fps;
            fx.playing = v.playing();
            fx.reverse = v.playing_reverse();
            fx.loop_on = v.is_looping();
            fx.format = v.format_text();
            fx.zoom_percent = v.zoom_percent();
        }
        self.shot_page.update(fx);
        if let Some(s) = self.shot_snapshot() {
            self.shot_page.sync_viewer_state(s);
        }
    }

    /// The live viewer as toolbar state (Review, restored workspaces and keys change the viewer directly).
    fn shot_snapshot(&self) -> Option<tb::ViewerState> {
        let v = self.viewer.as_ref()?;
        use crate::project::View as V;
        let o = self.shot_display(None);
        Some(tb::ViewerState {
            view: match v.view {
                V::Input => tb::View::Input,
                V::Hint => tb::View::Hint,
                V::Guide => tb::View::Mask,
                V::Fg => tb::View::Fg,
                V::Matte => tb::View::Matte,
                V::Comp => tb::View::Comp,
                V::Processed => tb::View::Processed,
            },
            overlay: match v.overlay {
                _ if v.sam_overlay => tb::Overlay::Sam,
                crate::ui::viewer::Overlay::Guide => tb::Overlay::PreMatte,
                crate::ui::viewer::Overlay::Hint => tb::Overlay::Hint,
                crate::ui::viewer::Overlay::Off => tb::Overlay::Off,
            },
            channel: o.channel,
            background: o.background,
            gain: o.gain as f64,
            gamma: o.gamma as f64,
            wipe: v.ab,
            ..Default::default()
        })
    }

    /// The Shot page's frame: chrome from the page, then the live picture and timeline in its regions.
    pub(super) fn shot_workspace(&mut self, ui: &mut egui::Ui) {
        if self.open.is_none() {
            return;
        }
        let ctx = ui.ctx().clone();
        // Old per-frame duties that are not drawing: keep the Key gate fresh (the old Properties panel did it by
        // drawing; the new page does not), and advance playback / decodes, repainting while playing.
        self.ai_watch(&ctx);
        if let Some(v) = &mut self.viewer {
            if v.tick() {
                ctx.request_repaint();
            }
        }
        self.refresh_shot_page();
        let acts = self.shot_page.ui(ui);
        if let Some(l) = self.shot_page.layout() {
            let mut viewer_rect = l.viewer;
            if self.tab == crate::ui::Tab::Hint && !hint::tools_for(self.hint.cfg.guide).is_empty() {
                let strip = egui::Rect::from_min_max(viewer_rect.min, egui::pos2(viewer_rect.left() + hint::STRIP_W, viewer_rect.bottom()));
                let rest = egui::Rect::from_min_max(egui::pos2(strip.right(), viewer_rect.top()), viewer_rect.max);
                viewer_rect = rest;
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(strip).layout(egui::Layout::top_down(egui::Align::Center)).id_salt("hint_tool_strip"));
                child.set_clip_rect(strip);
                self.hint_tool_strip(&mut child);
            }
            if let Some(v) = self.viewer.as_mut() {
                v.image_ui(ui, viewer_rect);
                v.timeline_ui(ui, l.timeline);
            }
        }
        if let Some(r) = self.shot_page.graph_rect() {
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).layout(egui::Layout::top_down(egui::Align::Min)).id_salt("shot_graph"));
            child.set_clip_rect(r);
            self.shot_graph(&mut child, r.height());
        }
        for a in acts {
            self.handle_shot_act(&ctx, a);
        }
        // The batch review (Key selected / Send again) is a dialog: the old Properties panel drew it, the Board draws it
        // only while shown, so with a shot open nothing consumed the planner's answer or showed the review.
        if let Some(clip) = self.clip().cloned() {
            self.hint_delete_dialog(&ctx, &clip); // "Delete pre-matte/hint frames…" (Properties, ⋯ menu, dock card)
        }
        if self.ai.batch_open() {
            crate::ui::board_batch::live_dialog(ui, &mut self.ai);
        }
    }

    /// Fallback anchor of the Activity popover (the status-bar item's rect is preferred, see `frame_inner`):
    /// the old strip's last rect, else the bottom-left corner of the picture column.
    pub(super) fn shot_activity_anchor(&self) -> Option<egui::Rect> {
        self.strip.rect.or_else(|| self.shot_page.layout().map(|l| egui::Rect::from_min_size(l.viewer_column.left_bottom(), egui::vec2(360.0, 24.0))))
    }

    /// The real node graph in the dock's graph body: the old drawing and event handling (`App::graph`), with the
    /// page's own Fit/zoom buttons instead of the on-canvas one.
    fn shot_graph(&mut self, ui: &mut Ui, height: f32) {
        let Some(folder) = self.clip().map(|c| c.folder.clone()) else { return };
        let fit_button = |app: &mut Self, on: bool| {
            app.graphs.entry(folder.clone()).or_insert_with(|| (graph::Graph::new(), graph::View::default())).1.draw_fit_button = on;
        };
        fit_button(self, false);
        self.graph(ui, height);
        fit_button(self, true); // the old workspace keeps its own button
    }

    fn shot_graph_view(&mut self, f: impl FnOnce(&mut graph::View)) {
        if let Some((_, v)) = self.clip().map(|c| c.folder.clone()).and_then(|k| self.graphs.get_mut(&k)) {
            f(v);
        }
    }

    /// Bible §10.1 shortcuts that belong to the app (the toolbar owns view/channel/background/wipe/focus/fit and
    /// consumes those keys itself). Exact modifiers, read from the key events; each press builds the same `Act` a click does.
    pub(super) fn shot_keys(&mut self, ctx: &egui::Context) {
        use egui::{Key, Modifiers as M};
        let hit = |m: M, k: Key| ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Key { key, pressed: true, modifiers, .. } if *key == k && modifiers.matches_exact(m))));
        let mut acts = vec![];
        for (i, k) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5].into_iter().enumerate() {
            if hit(M::NONE, k) {
                acts.push(Act::Stage(Tab::ALL[i].label()));
            }
        }
        // Playback, range and marks keys (Space, arrows, Home/End, I/O, Alt+I, M, Shift+M, Ctrl+Shift+M, L) are not
        // here: the transport control handles them while the shot page draws, so handling them here applied them twice.
        // P: Properties column (Wide/Medium) or drawer (Compact). Keeps the old props_sheet flag in step.
        if hit(M::NONE, Key::P) {
            self.shot_page.toggle_drawer();
            self.props_sheet = self.shot_page.drawer_open();
        }
        // Esc: one action per press, most local first. The Activity popover closes itself on Esc, so it is skipped here.
        if hit(M::NONE, Key::Escape) && !self.activity_is_open() {
            let drawer_at_compact = self.shot_page.layout().is_some_and(|l| l.drawer.is_some());
            let acted = if self.shot_page.dock_expanded() {
                self.shot_page.collapse_dock();
                true
            } else if drawer_at_compact {
                self.shot_page.toggle_drawer();
                self.props_sheet = self.shot_page.drawer_open();
                true
            } else if self.shot_page.focus() {
                self.shot_page.set_focus(false);
                self.focus_mode = false;
                true
            } else {
                false
            };
            // Consumed so a card menu or other Esc listener drawn later does not act on the same press.
            if acted { ctx.input_mut(|i| i.consume_key(M::NONE, Key::Escape)); }
        }
        // + / − (Shift+= is + on many layouts): same call the old keys made; no Act exists for relative zoom.
        let zoom = |k: Key| ctx.input(|i| i.key_pressed(k) && !i.modifiers.command && !i.modifiers.alt);
        if let Some(v) = &mut self.viewer {
            if zoom(Key::Equals) || zoom(Key::Plus) {
                v.zoom_by(1.25, Vec2::ZERO);
            }
            if zoom(Key::Minus) {
                v.zoom_by(0.8, Vec2::ZERO);
            }
        }
        for a in acts {
            self.handle_shot_act(ctx, a);
        }
    }

    pub(super) fn handle_shot_act(&mut self, ctx: &egui::Context, a: Act) {
        match a {
            Act::Stage(s) => {
                if let Some(t) = Tab::parse(s) {
                    self.switch_tab(t);
                }
            }
            Act::ToggleDrawer => {} // page-local
            Act::Transport(t) => self.shot_transport(t),
            Act::ViewChanged(s) => self.shot_view(&s),
            Act::Fit => self.viewer.iter_mut().for_each(|v| v.fit()),
            Act::OneToOne => self.viewer.iter_mut().for_each(|v| v.one_to_one()),
            Act::ZoomTo(p) => self.viewer.iter_mut().for_each(|v| v.set_zoom_percent(p)),
            Act::DockTab(_) | Act::DockExpanded(_) => {} // page-local
            Act::GraphFit => self.shot_graph_view(graph::View::request_fit),
            Act::GraphZoom(f) => self.shot_graph_view(|v| v.zoom_by(f)),
            Act::Card(id, a) => self.shot_card(ctx, &id, a),
            Act::Props(p) => self.shot_props_act(ctx, p),
        }
        ctx.request_repaint();
    }

    // ------------------------------------------------------------------------------------ snapshot

    /// The stage's bar from live state; the same function builds what the page draws and what a press does.
    fn shot_stage_bar(&self, clip: &Clip) -> StageBar {
        match self.tab {
            Tab::Key => match self.project.as_ref().map(|p| self.ai.key_view(clip, p, self.settings.saved())) {
                Some(kv) => key_bar(&kv),
                None => StageBar::default(),
            },
            Tab::Media => self.media_view().map(|m| media_bar(&m)).unwrap_or_default(),
            Tab::Hint => {
                let head = activity::build(self.ai.activity_steps(&clip.root).0).head.filter(|h| h.level == Level::Active);
                match self.hint_view() {
                    Some(hv) => hint_bar(&hv, head.as_ref()),
                    None => {
                        let mut b = StageBar::default();
                        (b.props.primary_label, b.props.primary_reason) = ("Generate hint".into(), Some("This shot's hint settings are still loading.".into()));
                        b
                    }
                }
            }
            Tab::Review => {
                let mut b = StageBar::default();
                b.props.primary_label = "Go to Key".into();
                b.props.primary_enabled = true;
                b.primary = Some(Go::Tab(Tab::Key));
                b.props.secondary = Some(("Go to Deliver".into(), true));
                b.secondary = Some(Go::Tab(Tab::Deliver));
                b
            }
            Tab::Deliver => {
                let head = activity::build(self.ai.activity_steps(&clip.root).0).head.filter(|h| h.level == Level::Active);
                match self.deliver_view() {
                    Some(dv) => deliver_bar(&dv, head.as_ref()),
                    None => {
                        let mut b = StageBar::default();
                        (b.props.primary_label, b.props.primary_reason) = ("Export".into(), Some("This shot's delivery settings are still loading.".into()));
                        b
                    }
                }
            }
        }
    }

    /// Blocker, Properties values (live) and cards of the open shot.
    fn shot_fill(&mut self, clip: &Clip, fx: &mut shot_page::Fixture) {
        if matches!(self.tab, Tab::Deliver) {
            self.deliver_prepare(); // loads the presets file once; deliver_view reads them
        }
        let mut lp = LiveProps { bar: self.shot_stage_bar(clip).props, ..Default::default() };
        match self.tab {
            Tab::Key => {
                if let Some(p) = &self.project {
                    let kv = self.ai.key_view(clip, p, self.settings.saved());
                    fx.blocker = kv.blocker.as_ref().map(|b| shot_page::Blocker { message: b.message.clone(), action: b.fix.map(|f| fix_label(f).to_string()) });
                    lp.key = Some(key_props(&kv, clip));
                }
            }
            Tab::Media => lp.media = self.media_view().map(|m| self.shot_media_props(&m)),
            Tab::Review => lp.review = self.shot_review_props(clip),
            Tab::Hint => lp.hint = self.hint_view().map(|h| hint_props_of(&h)),
            Tab::Deliver => lp.deliver = self.deliver_view().map(|d| deliver_props_of(&d)),
        }
        fx.live_props = Some(lp);
        fx.cards = self.shot_cards(clip);
    }

    fn shot_media_props(&self, m: &MediaView) -> MediaProps {
        use crate::ui::shot_page::RelinkState as S;
        use super::media::RelinkState as M;
        let frames = match &self.viewer {
            Some(v) => format!("{}–{} · {} frames", v.first_frame(), v.last_frame(), v.index.len()),
            None => format!("{} frames", m.source_frames),
        };
        MediaProps {
            source_kind: match m.source {
                MediaSource::Sequence { .. } => "Frame folder",
                MediaSource::Movie => "Movie",
                MediaSource::Missing => "Missing movie",
            }
            .into(),
            source_path: m.source_path.clone(),
            frame_size: m.resolution.clone(),
            frame_rate: m.fps,
            fps_assumed: m.fps_assumed,
            frames,
            input_linear: m.input_linear,
            frame_count: m.source_frames,
            timeline_frames: m.timeline_frames,
            stored_linear: m.stored_linear,
            colour_save_failed: m.colour_save_failed,
            result_range: m.result_range.clone(),
            colour_note: Some(m.colour_origin.to_string()),
            paths: m.paths.iter().map(|r| PathProp { label: r.label.to_string(), path: r.path.clone(), frames: r.frames }).collect(),
            missing: m.missing,
            relink: m.relink.as_ref().map(|r| RelinkProps {
                path: r.path.clone(),
                state: match r.state {
                    M::Checking => S::Checking,
                    M::Failed => S::Failed,
                    M::Partial => S::Partial,
                    M::Verified => S::Verified,
                },
                failure: r.failure.clone(),
                detail: r.probed.as_ref().map(|p| format!("{}x{} · {:.3} fps{}", p.width, p.height, p.fps, p.frames.map_or(String::new(), |n| format!(" · {n} frames")))),
                confirmed: r.confirmed,
                extract_enabled: m.extract.as_ref().is_some_and(|d| d.enabled),
                extract_reason: m.extract.as_ref().filter(|d| !d.enabled).map(|d| d.reason.clone()),
            }),
            can_choose_companion: m.can_choose_companion,
            companion: m.companion.as_ref().map(|c| CompanionProps {
                folder: c.folder.clone(),
                mask: c.mask,
                plan: c.plan.as_ref().map(|p| (p.dest.clone(), p.copy, p.kept)).map_err(Clone::clone),
                running: c.running,
            }),
            note: m.note.clone(),
            error: m.error.clone(),
        }
    }

    /// Result rows may be empty; the Review controls are still useful before the first Key run.
    fn shot_review_props(&self, clip: &Clip) -> Option<ReviewProps> {
        let v = self.viewer.as_ref()?;
        let expected: Vec<&str> = clip.manifest.as_ref().and_then(|m| m.source_stems.as_ref())
            .map(|stems| stems.iter().map(String::as_str).collect())
            .or_else(|| {
                let range = clip.manifest.as_ref()?.range.as_ref()?;
                let start = clip.input_files.iter().position(|f| f.0 == range.in_stem)?;
                let end = clip.input_files.iter().position(|f| f.0 == range.out_stem)?;
                (start <= end).then(|| clip.input_files[start..=end].iter().map(|f| f.0.as_str()).collect())
            })
            .unwrap_or_else(|| clip.input_files.iter().map(|f| f.0.as_str()).collect());
        let outs: Vec<(&str, usize)> = VIEW_NAMES.iter()
            .filter(|(_, w)| matches!(w, PV::Fg | PV::Matte | PV::Comp | PV::Processed))
            .map(|(n, w)| (*n, expected.iter().filter(|stem| v.index.view_has(*w, stem)).count()))
            .filter(|(_, n)| *n > 0).collect();
        let total = expected.len();
        let name = |w: PV| VIEW_NAMES.iter().find(|(_, x)| *x == w).map_or("Input", |(n, _)| *n).to_string();
        Some(ReviewProps {
            frames: outs.iter().map(|(n, c)| (n.to_string(), format!("{c} of {}", total.max(*c)))).collect(),
            last_run: clip.manifest.as_ref().map(|m| format!("{}{}", m.engine, if m.complete { "" } else { " · partial" })),
            views: VIEW_NAMES.iter().filter(|(_, w)| v.index.view_frames(*w) > 0).map(|(n, _)| n.to_string()).collect(),
            a: name(v.view),
            b: name(v.b_view),
            wipe: v.ab,
            horizontal: self.shot_display(None).wipe_horizontal,
            missing_frames: outs.iter().map(|(_, c)| total.saturating_sub(*c)).max().unwrap_or(0),
            out_of_range: v.out_of_range(),
        })
    }

    /// The toolbar's display options as last applied (the viewer keeps them private); `Some` stores new ones.
    fn shot_display(&self, set: Option<DisplayOpts>) -> DisplayOpts {
        let Some(ctx) = &self.last_ctx else { return set.unwrap_or_default() };
        match set {
            Some(o) => {
                ctx.data_mut(|d| d.insert_temp(disp_id(), o));
                o
            }
            None => ctx.data(|d| d.get_temp::<DisplayOpts>(disp_id())).unwrap_or_default(),
        }
    }

    /// Action cards, pipeline order: the newest run of each stage (card id = its job id), else what is on disk
    /// (card id = the stage key "prematte" / "hint" / "key").
    fn shot_cards(&mut self, clip: &Clip) -> Vec<shot_page::Card> {
        let steps = self.activity_snapshot().map(|s| s.model.list).unwrap_or_default();
        let mut out = vec![];
        for (kind, title, node) in [("prematte", "Pre-matte", Node::PreMatte), ("hint", "Hint", Node::Hint), ("key", "Key", Node::Key)] {
            let stale = self.stage_pending(node);
            if let Some(s) = steps.iter().find(|s| card_kind(&s.stage, self.ai.task_job(&s.id).as_deref()) == Some(kind)) {
                let mut meta = String::new();
                for p in [&s.pc, &s.phase, &s.message].into_iter().filter(|p| !p.is_empty()) {
                    if !meta.is_empty() {
                        meta.push_str(" · ");
                    }
                    meta.push_str(p);
                }
                out.push(shot_page::Card {
                    id: s.id.clone(),
                    kind: kind.into(),
                    title: title.into(),
                    meta,
                    state: s.phase.clone(),
                    percent: (s.level == Level::Active && s.total > 0).then(|| (s.done * 100 / s.total).min(100) as u8),
                    can_cancel: matches!(s.cancel, Cancel::Job(_)),
                    can_reveal: true,
                    can_delete: kind != "key",
                    menu: vec![],
                });
                continue;
            }
            let on_disk = match kind {
                "key" => clip.manifest.as_ref().map(|m| format!("Result on disk · {}{}", m.engine, if m.complete { "" } else { " (partial)" })),
                "hint" => (clip.alpha_frames > 0).then(|| format!("{} frames", clip.alpha_frames)),
                _ => (clip.mask_frames > 0).then(|| format!("{} frames", clip.mask_frames)),
            };
            if let Some(meta) = on_disk {
                out.push(shot_page::Card {
                    id: kind.into(),
                    kind: kind.into(),
                    title: title.into(),
                    meta: if stale { format!("{meta} · needs update") } else { meta },
                    state: if stale { "Needs update" } else { "Done" }.into(),
                    percent: None,
                    can_cancel: false,
                    can_reveal: true,
                    can_delete: kind != "key",
                    menu: vec![],
                });
            }
        }
        out
    }

    // --------------------------------------------------------------------------------------- acts

    /// Runs one command; a refusal or failure goes to the same notice the old panels used.
    fn shot_go(&mut self, ctx: &egui::Context, clip: &Clip, go: Go) {
        let err = match go {
            Go::Key(c) => match self.project.as_ref() {
                Some(p) => self.ai.key_cmd(clip, p, self.settings.saved(), ctx, c),
                None => Err("No project is open".into()),
            },
            Go::Media(c) => {
                if let Err(e) = self.media_cmd(ctx, c) {
                    self.media.note = Some(e);
                }
                Ok(())
            }
            // hint_cmd already put the reason in the Hint note.
            Go::Hint(c) => self.hint_cmd(ctx, c).or_else(|e| if self.hint_note_text().is_some() { Ok(()) } else { Err(e) }),
            Go::Deliver(c) => self.deliver_cmd(ctx, c),
            Go::Tab(t) => {
                self.switch_tab(t);
                Ok(())
            }
            Go::Page(p) => {
                self.goto_page(p);
                Ok(())
            }
            Go::Activity => {
                self.strip.open = true;
                Ok(())
            }
            Go::Cancel(id) => {
                self.ai.cancel(&id);
                Ok(())
            }
            Go::Notice(m) => Err(m),
        };
        if let Err(e) = err {
            self.ai.notice = Some(e);
        }
    }

    fn shot_props_act(&mut self, ctx: &egui::Context, p: PropAct) {
        let Some(clip) = self.clip().cloned() else { return };
        match p {
            PropAct::Key(k) => self.shot_go(ctx, &clip, Go::Key(key_cmd_of(k))),
            PropAct::Media(m) => self.shot_go(ctx, &clip, Go::Media(media_cmd_of(m))),
            PropAct::Hint(h) => match hint_cmd_of(h) {
                Ok(c) => self.shot_go(ctx, &clip, Go::Hint(c)),
                Err(e) => self.shot_go(ctx, &clip, Go::Notice(e)),
            },
            PropAct::Deliver(d) => match deliver_cmd_of(d) {
                Ok(c) => self.shot_go(ctx, &clip, Go::Deliver(c)),
                Err(e) => self.shot_go(ctx, &clip, Go::Notice(e)),
            },
            PropAct::Review(r) => self.shot_review(r),
            PropAct::CopyPath(path) => ctx.copy_text(path.display().to_string()),
            PropAct::RevealPath(path) => reveal(&path, path.is_file(), &mut self.ai),
            PropAct::Bar(b) => {
                let mut bar = self.shot_stage_bar(&clip);
                let go = match b {
                    BarAct::Primary => bar.primary.take(),
                    BarAct::Secondary => bar.secondary.take(),
                    BarAct::Cancel => bar.cancel.take(),
                    BarAct::Fix => bar.fix.take(),
                    BarAct::Menu(label) => bar.props.menu.iter().position(|(l, _, _)| *l == label).filter(|&i| bar.props.menu[i].1).and_then(|i| bar.menu.get(i).cloned()),
                };
                if let Some(go) = go {
                    self.shot_go(ctx, &clip, go);
                }
            }
        }
    }

    fn shot_review(&mut self, r: ReviewAct) {
        match r {
            ReviewAct::GoToDeliver => self.switch_tab(Tab::Deliver),
            ReviewAct::GoToKey => self.switch_tab(Tab::Key),
            ReviewAct::WipeHorizontal(h) => {
                let o = self.shot_display(None);
                let o = self.shot_display(Some(DisplayOpts { wipe_horizontal: h, ..o }));
                self.viewer.iter_mut().for_each(|v| v.set_display(o));
            }
            ReviewAct::Wipe(on) => self.viewer.iter_mut().for_each(|v| v.ab = on),
            ReviewAct::CompareA(n) => self.shot_compare(true, &n),
            ReviewAct::CompareB(n) => self.shot_compare(false, &n),
        }
    }

    fn shot_compare(&mut self, a: bool, name: &str) {
        let Some(w) = VIEW_NAMES.iter().find(|(x, _)| *x == name).map(|(_, w)| *w) else { return };
        if let Some(v) = self.viewer.as_mut().filter(|v| v.index.view_frames(w) > 0) {
            if a { v.view = w } else { v.b_view = w }
        }
    }

    /// Card buttons. The dock may send the card's list position instead of its id, so both are accepted.
    fn shot_card(&mut self, ctx: &egui::Context, id: &str, a: CardAction) {
        let cards = &self.shot_page.fx.cards;
        let Some(card) = cards.iter().find(|c| c.id == id).or_else(|| id.parse::<usize>().ok().and_then(|i| cards.get(i))).cloned() else { return };
        let Some(clip) = self.clip().cloned() else { return };
        let go = match a {
            CardAction::Cancel if card.can_cancel => Go::Cancel(card.id.clone()),
            CardAction::Cancel => Go::Notice("Nothing is running for this step".into()),
            CardAction::Reveal => {
                let dir = match card.kind.as_str() {
                    "key" => clip.output_dir.clone(),
                    "hint" => clip.view_dir(PV::Hint),
                    _ => clip.view_dir(PV::Guide),
                };
                reveal(&dir.unwrap_or_else(|| clip.root.clone()), false, &mut self.ai);
                return;
            }
            CardAction::Menu(m) if m == "Delete frames…" && card.kind != "key" => Go::Hint(HintCmd::RequestDelete { prematte: card.kind == "prematte" }),
            CardAction::Menu(m) => Go::Notice(format!("{m} is not available for this step")),
        };
        self.shot_go(ctx, &clip, go);
    }

    fn shot_transport(&mut self, t: Action) {
        let Some(v) = &mut self.viewer else { return };
        match t {
            Action::First => v.go_first(),
            Action::StepBack => v.step(-1),
            Action::PlayReverse => v.toggle_play(-1),
            Action::PlayPause => {
                if v.playing() { v.pause() } else { v.toggle_play(1) }
            }
            Action::PlayForward => v.toggle_play(1),
            Action::StepForward => v.step(1),
            Action::Last => v.go_last(),
            // A file frame number -> timeline index; a missing number lands on the nearest existing frame.
            Action::Seek(n) => {
                let best = (0..v.index.len()).min_by_key(|&i| (v.index.frame_number(i) as i64 - n).abs());
                if let Some(i) = best {
                    v.set_playhead(i);
                }
            }
            // Hint_sync takes `mark_req` and saves it, same as the old buttons.
            Action::Mark => v.mark_req = Some(MarkReq::Mark),
            Action::Unmark => v.mark_req = Some(MarkReq::Unmark),
            Action::ClearMarks => v.mark_req = Some(MarkReq::Clear),
            Action::SetIn => v.set_in(),
            Action::SetOut => v.set_out(),
            Action::ClearInOut => v.clear_range(),
            Action::Loop => v.toggle_loop(),
        }
    }

    /// Applies only what differs from the live viewer, so an unrelated edit (channel, gain) never resets the view or wipe.
    fn shot_view(&mut self, s: &tb::ViewerState) {
        self.focus_mode = s.focus;
        let Some(cur) = self.shot_snapshot() else { return };
        let horizontal = self.shot_display(None).wipe_horizontal; // the Review row owns the direction
        let Some(v) = &mut self.viewer else { return };
        use crate::project::View as V;
        if s.view != cur.view {
            let want = match s.view {
                tb::View::Input => V::Input,
                tb::View::Hint => V::Hint,
                tb::View::Mask => V::Guide, // the old "Guide" chip
                tb::View::Fg => V::Fg,
                tb::View::Matte => V::Matte,
                tb::View::Comp => V::Comp,
                tb::View::Processed => V::Processed,
            };
            // Same rule as the old chips: a view with nothing on disk is not selectable.
            if v.index.view_frames(want) > 0 {
                v.view = want;
            }
        }
        if s.wipe != cur.wipe {
            v.ab = s.wipe;
        }
        if s.overlay != cur.overlay {
            v.overlay = match s.overlay {
                tb::Overlay::PreMatte => crate::ui::viewer::Overlay::Guide,
                tb::Overlay::Hint => crate::ui::viewer::Overlay::Hint,
                _ => crate::ui::viewer::Overlay::Off,
            };
            v.sam_overlay = s.overlay == tb::Overlay::Sam && v.sam_available;
        }
        let o = DisplayOpts { channel: s.channel, gain: s.gain as f32, gamma: s.gamma as f32, background: s.background, wipe_horizontal: horizontal };
        v.set_display(o);
        self.shot_display(Some(o));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::Intent;
    use serde_json::json;
    use std::fs;
    use std::path::PathBuf;

    fn png(path: &std::path::Path) {
        let mut encoder = png::Encoder::new(fs::File::create(path).unwrap(), 4, 4);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.write_header().unwrap().write_image_data(&[0; 48]).unwrap();
    }

    fn app_with_clip(tag: &str, with_result: bool) -> (egui::Context, App, PathBuf) {
        let base = std::env::temp_dir().join(format!("ck-shot-wire-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let project = base.join("project");
        let clip = project.join("clips/shot");
        fs::create_dir_all(clip.join("Frames")).unwrap();
        for n in 1101..=1105 { png(&clip.join(format!("Frames/f_{n}.png"))); }
        fs::write(clip.join(".video_metadata.json"), r#"{"fps":24.0,"width":4,"height":4}"#).unwrap();
        let native = if with_result {
            let result = base.join("result");
            fs::create_dir_all(&result).unwrap();
            png(&result.join("shot_1102.png"));
            fs::write(result.join(crate::project::MANIFEST_NAME), json!({
                "version": 3, "engine": "corridorkey-v2", "clip_id": "clip-id", "enabled_outputs": ["matte"],
                "formats": {"matte": "png"}, "range": {"in_point": 1, "out_point": 2, "in_stem": "f_1102", "out_stem": "f_1103"},
                "source_stems": ["f_1102", "f_1103"], "files": {"matte": ["shot_1102.png", "shot_1103.png"]}, "complete": false
            }).to_string()).unwrap();
            json!({"clip_id": "clip-id", "key_result": {"manifest_dir": result}})
        } else { json!({}) };
        fs::write(clip.join("clip.json"), native.to_string()).unwrap();
        fs::write(project.join("project.json"), r#"{"version":2,"clips":["shot"]}"#).unwrap();

        let ctx = egui::Context::default();
        let mut app = App::with_ctx(&ctx, AppOptions { project: Some(project), open_clip: Some("shot".into()), tab: Some(Tab::Review), ..Default::default() });
        if app.clip().is_none() { app.open_clip(&ctx, 0); }
        (ctx, app, base)
    }

    #[test]
    fn review_counts_only_the_manifest_range() {
        let (_ctx, app, base) = app_with_clip("review-range", true);
        let clip = app.clip().unwrap().clone();
        let review = app.shot_review_props(&clip).expect("the manifest provides a review");
        assert_eq!(review.frames, [("Matte".into(), "1 of 2".into())]);
        assert_eq!(review.missing_frames, 1);
        drop(app);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn live_header_stage_comes_from_the_clip_state() {
        let (_ctx, mut app, base) = app_with_clip("header-stage", false);
        app.refresh_shot_page();
        assert_eq!(app.shot_page.fx.clip_stage, "raw");
        drop(app);
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn replace_only_key_again_requests_the_replace_confirmation() {
        let (ctx, mut app, base) = app_with_clip("replace-only", false);
        let project = app.project.as_ref().unwrap().clone();
        let clip = app.clip().unwrap().clone();
        let settings = app.settings_page().saved().clone();
        let mut view = app.ai.key_view(&clip, &project, &settings);
        view.intents = vec![crate::ui::ai::IntentView { intent: Intent::Replace, label: "Replace".into(), enabled: true, reason: None }];
        assert!(matches!(key_again(&view), Go::Key(KeyCmd::RequestReplace)));
        drop(app);
        drop(ctx);
        let _ = fs::remove_dir_all(base);
    }
}
