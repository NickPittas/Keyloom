# UI redesign and Rust rewrite exploration

Repository reconnaissance for proposing 5–10 muted, Nuke-like UI directions for normal users. This describes the current CorridorKey 2.0 fork; the lower historical portion of the root README describes upstream v1 and is not authoritative.

## Current product and screen

- Native desktop GUI is **PySide6/Qt Widgets**, built in `ui/main_window.py` and styled with `ui/theme/`. `MainWindow` composes mixins in `ui/main_window_mixins/`; actual app entry is `main.py`.
- First screen is `WelcomeScreen` (`ui/widgets/welcome_screen.py`) with drag/drop, open folder/files, and recent-project affordances. Workspace is a dual viewer (`ui/widgets/dual_viewer.py`), input/output viewers, scrubber/timeline and In/Out range, right parameter panel (`ui/widgets/parameter_panel.py`), bottom I/O tray (`ui/widgets/io_tray_panel.py`), collapsible queue (`ui/widgets/queue_panel.py`), top GPU/VRAM status, and run/progress/cancel status bar.
- Main window has menu and hotkeys; settings/preferences include language, file copying, EXR compression, paths, and other app behavior. Geometry, splitter sizes, selected clip, parameters, output configuration, and per-clip color-space overrides are saved in `.corridorkey_session.json` within the project (`ui/main_window_mixins/session_mixin.py`). QSettings stores app preferences. Clip and project metadata are `clip.json` and `project.json`; annotations and hints have separate sidecars/assets.
- Current color style is dark/near-black with high-saturation yellow/green accents and compact technical labels (`ui/theme/`). Redesign can soften contrast/accent and simplify labels without changing the underlying controls or state model.

## User workflow and semantics

1. **Open/import** a project folder, videos, image sequences, or images. Video import creates a project and extracts frames to `Frames/`; sequences are scanned as assets. Source copy/reference behavior is configurable. Companion `*_AlphaHint.*` and `*_MaskHint.*` files can be detected and copied (`backend/project.py`, `ui/main_window_mixins/import_mixin.py`, `export_mixin.py`). Extraction is asynchronous and reports progress; retrying failed extraction clears partial frames first.
2. **Review/manage clips** in I/O tray. Clip states are `EXTRACTING`, `RAW`, `MASKED`, `READY`, `COMPLETE`, `ERROR` (`backend/clip_state.py`). Selection changes viewer, parameter context, inferred/known background color, badges and available actions. Queue panel shows sequential GPU job status/progress/history and cancel (`backend/job_queue.py`, `ui/widgets/queue_panel.py`).
3. **Supply/generate alpha hint** (optional for CorridorKey 2.0): import an AlphaHint sequence/video; generate via Chroma Key, GVM, BiRefNet, VideoMaMa, MatAnyone2, or Apple Vision (platform dependent). VideoMaMa can use imported mask or foreground/background annotation strokes; SAM2 can track prompts into dense masks. Chroma Key includes screen eyedropper, key strength, clip black/white, shrink/grow, edge blur and live preview. Reset Hint deletes alpha/SAM masks but deliberately retains keyed outputs and holdout strokes (`hint_reset_mixin.py`). Annotation strokes, holdout strokes, sparse hint keyframes and masks are independent persisted data (`annotation_mixin.py`, `hint_keyframe_mixin.py`, `backend/hint_keyframes.py`).
4. **Configure inference** (`InferenceParams`, `backend/service/core.py`): background color Auto/Green/Blue; source interpretation sRGB/Linear; hint mode None/Full AlphaHint/Keyframes; hint quality 0–1; marked keyframe toggle; carry hint and carry quality; VRAM profile Auto/Low; temporal window Auto, 80/40, 48/24, 24/12, 8/4, 1/1; despill 0–1. Cutout linear appears disabled and has no effect for this engine. BG choice controls checkpoint/despill/spill behavior. Input color-space setting changes inference interpretation and viewer presentation; it does not rewrite existing outputs.
5. **Run/review**: infer current clip, resume compatible prior run, run eligible clips, or run batch pipeline (`inference_mixin.py`, `batch_pipeline_mixin.py`). Output modes are IN/FG/MATTE/COMP/PROCESSED in viewer. Brush and eyedropper tools act over the viewer. Phase and frame progress are surfaced; cancel is cooperative at GPU-step boundary. One GPU job runs at a time to avoid VRAM contention.
6. **Export** four independently enabled products (FG, Matte, Comp, Processed), each EXR or PNG (`OutputConfig`). EXR compression preference: DWAB/PIZ/ZIP/none. Outputs are published below clip output root into `FG/`, `Matte/`, `Comp/`, `Processed/`; generated run metadata is `.corridorkey_manifest.json`. Video export/stitching is a separate operation. In/Out marks are zero-based inclusive and limit published frames only.

### State/readiness rules that affect UI

- Hint mode None is valid for RAW clips and CorridorKey keys the clip itself. Full hint requires an AlphaHint and READY/COMPLETE. Keyframes needs an AlphaHint plus valid non-empty marked frames, and partial AlphaHint is permitted. UI must not collapse all clips into a simplistic “hint required” readiness state.
- Full pipeline routes differ: RAW/no annotations → GVM pipeline; annotations without dense tracked mask → SAM2 tracking then VideoMaMa then inference; MASKED → VideoMaMa then inference; READY/COMPLETE → inference. Hint mode None bypasses generators/masks and routes directly to inference. `classify_pipeline_route()` in `backend/clip_state.py` is canonical.
- CorridorKey 2.0 runs inference on the **whole clip** for temporal context. In/Out only limits which frames are published. Separate range runs do not aggregate into a complete clip. Resume only skips missing outputs if manifest engine, exact params, keyframe marks, enabled outputs/formats, and source frame stems are compatible; incompatible runs re-publish selected frames. A full-range manifest is needed for COMPLETE state (`backend/service/inference.py`, README fork-specific section).
- Carry hint is model prediction reuse across overlapping Hann windows, not ordinary frame-to-frame tracking. 1/1 has no overlap/carry. “Auto” temporal window estimates are approximate and adapt to free VRAM.
- UI values/defaults visible in panel are not every persisted parameter: `cutout_linear` is always passed false, EXR compression comes from QSettings, BiRefNet model choice is separately persisted. Keep those sources distinct in any settings migration.

## Data layout and persistence

- Project: `Projects/<timestamp>_<name>/project.json`; clips under `clips/<clip>/`. Typical clip data: `Source/`, extracted `Frames/`, optional `AlphaHint/` or AlphaHint video, `VideoMamaMaskHint/`, annotation/holdout data, sparse keyframe sidecar `.corridorkey_hint_keyframes.json`, outputs and `.corridorkey_manifest.json`; individual metadata in `clip.json`.
- Session state is a project-local JSON sidecar, preferences are QSettings, source/hint/output media are ordinary files. Existing users therefore have inspectable, filesystem-based project state; a new UI should retain compatibility or define explicit migration/ownership boundaries before changing layout.
- `backend/project.py`, `backend/clip_state.py`, `backend/hint_keyframes.py`, `backend/service/inference.py`, `backend/service/v2_inputs.py` contain important data/validation behavior. Do not treat widget state as authoritative for persisted assets or completion.

## Frontend/backend boundary and Rust implications

- UI is currently tightly integrated Python/Qt: `MainWindow` creates `CorridorKeyService`, `GPUJobWorker`/other QThreads, connects Qt signals, snapshots widget values into dataclasses, and updates widgets from progress/status/result signals (`ui/main_window.py`, `ui/workers/gpu_job_worker.py`, `ui/workers/extract_worker.py`).
- `backend/service/` owns GPU/model lifetime, inference pipelines, hint input preparation, output validation/publication, resume compatibility and manifests. `backend/job_queue.py` serializes/deduplicates jobs and defines cancellation/history. Backend is not presently a network service or stable language-neutral API; it is imported directly in-process by Python UI/CLI.
- A Rust front end does not automatically make inference Rust-native. Lowest-risk initial boundary is retain Python service/worker as a local child process and define an explicit versioned IPC contract for project scan, submit/cancel job, progress/status/warnings, completion/errors, parameter/output snapshots. Rust must avoid owning CUDA/model lifetime unless backend replacement is separately scoped. Alternative is Rust shell embedding Python, but still needs clear lifecycle/error protocol.
- Stable contract candidates already exist in dataclasses/enums: `InferenceParams`, `OutputConfig`, `ClipEntry`/`ClipState`, `InOutRange`, `GPUJob`/`JobType`/`JobStatus`; they need serializable protocol schemas and compatibility rules, not one-to-one widget ports. Keep session/preferences versioning and output manifests explicit.

## Visual assets usable for proposals

- `docs/screenshots/EZ-CorridorKey_Inference_Done.jpg` is the only full application screenshot found, but it is referenced inside the **historical upstream README** and should be treated as legacy visual reference, not guaranteed current fork appearance.
- `docs/screenshots/quality_comparison_v1.5.0.png` is a historical quality comparison, not a current app screen.
- `ui/theme/keyloom.png` and `ui/theme/icons/eyedropper.png` are brand/UI assets. Repository scan found no sample video, EXR frames, or current workspace screenshots. For mockups, use the legacy app screenshot only as structural reference and use a clearly labeled synthetic checkerboard subject/media frame, or capture current runtime UI separately.

## Key code map

| Area | Main paths |
|---|---|
| Window composition/theme | `ui/main_window.py`, `ui/theme/`, `ui/app.py` |
| Welcome/import/session | `ui/widgets/welcome_screen.py`, `ui/main_window_mixins/import_mixin.py`, `session_mixin.py`, `backend/project.py` |
| Viewer/timeline/annotations | `ui/widgets/dual_viewer.py`, `preview_viewport.py`, `split_view.py`, `frame_scrubber.py`, `timeline_widgets.py`, `annotation_overlay.py`, `ui/main_window_mixins/annotation_mixin.py`, `hint_keyframe_mixin.py` |
| Parameters/outputs | `ui/widgets/parameter_panel.py`, `backend/service/core.py` |
| Clip states/readiness | `backend/clip_state.py`, `backend/clip_scanner.py` |
| Queue/workers/progress | `backend/job_queue.py`, `ui/widgets/queue_panel.py`, `ui/widgets/status_bar.py`, `ui/workers/gpu_job_worker.py`, `extract_worker.py` |
| Inference, hints, publish/resume | `backend/service/inference.py`, `v2_inputs.py`, `ui/main_window_mixins/inference_mixin.py`, `model_run_mixin.py`, `batch_pipeline_mixin.py` |
| Import/export | `ui/main_window_mixins/alpha_import_mixin.py`, `export_mixin.py`, `backend/project_media.py`, `backend/frame_io.py` |

## Proposal design constraints

For the requested 5–10 options, vary the *information architecture* (guided single-shot workflow, compact compositor desk, review-first viewer, queue/batch-first, etc.) while preserving truthful state labels, discoverable hint mode, color interpretation, output format/product selection, and visible job/clip progress. Keep advanced engine concepts behind plain-language labels/tooltips; expose sparse keyframe readiness and partial-range warning where they change outcomes. Avoid implying real-time single-frame re-key, parallel GPU processing, automatic range accumulation, or cross-frame carry semantics that the engine does not provide.
