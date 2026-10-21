# Implementation Plan

## Goal
Make all existing preview panes scrub live and play at clip FPS using background decoding and bounded viewport-sized RAM caches, and add safe hint reset/replacement while preserving prompts and inference outputs.

## Scope

Target: `/home/npittas/EZ-CorridorKey2`, branch `corridorkey-v2`, Python 3.11 `.venv`, existing PySide6/OpenCV/NumPy. This is a plan only: no application code was modified and no tests or performance measurements were run by the planner. No new dependencies, disk proxies, engine/output-format changes, SAM3 backend, or unrelated refactors.

### Direct code inspection: confirmed locations and corrections

| Code read | Finding |
| --- | --- |
| `ui/widgets/timeline_widgets.py:39-50` | **Additional root cause:** `_FatSlider.mousePressEvent` sets the click value, accepts and returns without QSlider's base press handling. There is no mouse-move override. Native slider drag is never armed. Removing debounce alone will NOT repair click-and-drag scrubbing. Implement and test the actual gesture. |
| `ui/widgets/frame_scrubber.py:96/:131-139/:260-277/:339-374` | Slider changes and marker scrubs restart 50 ms debounce; playback is fixed at 333 ms and advances one frame per timeout. No clip FPS or elapsed-time compensation. |
| `ui/preview/async_decoder.py:78-82/:93-126` | Mutates global QThreadPool maximum (Windows 2, Linux 4). Every request is submitted; stale results are discarded only after decoding. This is not queued-work coalescing. |
| `ui/widgets/preview_viewport.py:460-493/:502-565/:567-593/:640-653` | Navigation requests async primary frames, but split-left INPUT directly calls decode functions on GUI. Split completion routing by **mode** also misroutes a primary INPUT fallback to the left side. Route by explicit destination. |
| `preview_viewport.py:347/:421-436`; `backend/clip_state.py:172-181` | Worker temp-preview decode is also synchronous. Clip info asks `get_dimensions()`, which reads a full first sequence image. Avoid retaining either hidden GUI decode path. |
| `ui/preview/display_transform.py:33-35/:49-80/:121-160/:162-248/:251-302` | One global 10-image cache (comment says 20), no prefetch/downsample. EXRs use `imread_unicode(..., IMREAD_UNCHANGED)`, float processing then detached QImage copy. Actual gamma helper is `:229`, QImage copy `:236-248`; supplied `:263/:276` references are stale/video region. |
| `ui/app.py:33-39/:55-56/:108-109` | **Correction:** non-Windows returns at :55-56. `cv2.setNumThreads(1)` is **Windows-only**, not a Linux cap. Current docstring explains decoder fan-out/desktop starvation; it does not establish a Torch deadlock. Git history must be checked as task 0. |
| `ui/widgets/split_view.py:543-603/:624-653` | Paint/brush/mouse coordinate transforms use decoded QImage dimensions. Downsampling without original source dimensions would silently corrupt saved SAM/holdout strokes. Zoom is fit-relative; preserve it and pixel pan. |
| `ui/widgets/dual_viewer.py:85/:115-116/:174-229/:241-274/:395-427` | Shared scrubber drives both panes. Wipe listens directly to decoder results. Rendered annotation models are shared, but each PreviewViewport still owns a separate private `_annotation_model`; reset/reload can resurrect cleared prompts if these copies are saved in the wrong order. |
| `ui/main_window_mixins/clip_mixin.py:184-194` | Apple Vision/GVM/BiRefNet/Chroma disabled outside RAW/MASKED. VideoMaMa/MatAnyone2 use actual mask predicate. |
| `model_run_mixin.py:21-56/:58-111`; `chroma_key_mixin.py:42-76` | Handlers repeat state guards. Partial-alpha dialog offers Resume/Regenerate for existing PNG/JPEG hints; complete READY replacement needs a distinct confirmation, not a misleading resume of another generator. |
| `backend/service/pipelines_auto.py:106/:219/:376/:507` | Auto generators create AlphaHint and transition READY. Retain pipeline architecture. |
| `backend/service/pipelines_guided.py:153-177/:354/:474/:549/:582` | SAM writes masks + provenance, removes AlphaHint, sets MASKED; VideoMaMa and MatAnyone2 write AlphaHint and transition READY. |
| `backend/service/frame_ops.py:559-573/:575-582` | Writes `.corridorkey_mask_manifest.json`; alpha removal only removes its directory. Shared reset helper must also cover existing video hint variants. |
| `ui/widgets/io_tray_actions.py:235-273/:300-344/:346-377` | Existing clear-mask/clear-all/clear-alpha already handle directory and recognized video-file variants; mask clear omits manifest. Clear All deletes outputs, so MUST NOT be the Reset Hint implementation. |
| `ui/main_window_mixins/annotation_mixin.py:139-165/:236-256` | SAM full-track submission already asks to replace AlphaHint. Existing clear-annotations removes masks/manifest. Reuse shared deletion, not a second inconsistent implementation. |
| `ui/widgets/annotation_overlay.py:92-98/:115-145` | Clearing an empty annotation model and saving removes annotations.json. Holdouts have their own filename/model. |
| `backend/clip_state.py:55-65/:512-676` | READY→RAW exists; **MASKED→RAW does not**. find_assets discovers folder/video hints. Valid complete full-range v2 outputs take COMPLETE priority even without hints. |
| `ui/workers/gpu_job_worker.py:94-103/:134-147`; `backend/job_queue.py:316-364` | Use current_job_id/is_busy plus pending/running queue state. Idle worker stays running. Cancel requested does not mean terminal. |
| `ui/main_window_mixins/inference_mixin.py:87-120/:223-238/:364-387`; `worker_mixin.py:242-255/:342-359` | SAM preview result drives full-track confirmation; Run/Resume refresh does not refresh all hint tools. Add one shared hint refresh at selection, submission, completion/error/cancel and queue idle. Finished signal may precede clearing worker current_job_id. |
| `backend/ffmpeg_tools/metadata.py:12-38`; `backend/service/v2_inputs.py:124-138`; `backend/frame_io.py:246-271` | Reuse sidecar FPS → video probe → 24 FPS convention, and unicode-safe open_video. Do not import inference internals into widgets. |
| `ui/preview/frame_index.py:91-98/:140-172/:211-245` | Existing video navigation/path support must remain. |
| `ui/widgets/preferences_dialog.py:65/:75-79` | Actual tracker options are SAM2.1 Small/Base+/Large. No existing SAM3 backend/UI was found; preserve current options, do not expand scope. |

Supplied warm decode timings (source EXR 134 ms, FG EXR 326 ms, Comp PNG 60 ms, AlphaHint PNG 25 ms at 3200×1800) are user measurements, not measurements performed for this document.

## Summary approach / implementation contracts

### Playback and cache

- Keep AsyncDecoder/mixins/widgets. Workers produce detached QImages; GUI-only widget changes/painting; no QPixmaps in workers.
- Add `FrameCache(max_bytes)` and `PREVIEW_CACHE_BYTES = 512 * 1024 * 1024` **per PreviewViewport**. Primary and split-input lanes share that budget. Count `QImage.sizeInBytes()` including row padding; LRU eviction, correct replacement accounting, oversized entry not retained. No cached float arrays. Legacy direct decode callers can use a separate small byte-bounded default cache.
- Cache identity includes path, source revision (`mtime_ns`, size), video index, actual transform mode, INPUT interpretation, target size and clip/cache generation. `clear()` increments generation so in-flight old work can neither reinsert nor paint. Each pane clears only its own cache on switch. Reset/replacement invalidates affected generations, including same-path regenerated/temp images.
- Add keyword-only `target_size: tuple[int, int] | None` to both decode functions, retaining old QImage-returning/full-resolution defaults. Aspect-fit to physical canvas pixels (DPR-aware); no upscaling. Downsample decoded numeric data before expensive gamma/8-bit conversion. For straight Processed RGBA, composite in linear light before reduction/gamma to avoid alpha-edge bleeding. OpenCV still decompresses full EXRs: reduced gamma cost is not reduced EXR decompression cost.
- Preserve original width/height as QImage text `source_width`/`source_height`, with `image_source_size(image)` fallback to image dimensions for existing/live images. Source dimensions drive strokes/brush/overlay coordinates; raster dimensions drive sampling. When zoom exceeds the fit cache's resolution, request full resolution asynchronously, retaining the old fit image until ready. Full-res images obey the same budget. Fit reset/resize must not jump pan/zoom or alter annotations.
- Use a **preview-owned shared** QThreadPool, not a modified global Qt pool. Linux outer workers use available affinity CPUs, capped at 32 on this machine; Windows retains the small cap. One replaceable foreground-pending request per lane; bounded near-playhead prefetch; deduplicate cached/in-flight keys. Remove superseded unstarted work, foreground before background. GUI-thread QObject slots own scheduling/completions; explicit queued worker signals and retained in-flight signal lifetimes. No worker-thread lambdas touching widgets.
- Initial prefetch: `PREFETCH_BEHIND = 8`, `PREFETCH_AHEAD = 32`, bounded `MAX_PREFETCH_REQUESTS` proportional to pool capacity. Closest/forward first for playback, reverse priority during reverse scrub, clamp at clip/in/out edges, wrap only inside loop range. Slide continuously, not after drag quiets. Bound retained history by bytes, not clip length; no eager full-clip decode.
- **Separate clock target from actual presentation.** FrameScrubber uses QElapsedTimer and PreciseTimer: target = anchor + floor(elapsed × FPS), not timeout count. New `playback_requested(index, cycle)` and `playback_state_changed(bool)`; keep `frame_changed(index)` for immediate user navigation. Viewports expose `prepare_frame`, `has_cached_frame`, `present_cached_frame`, `frame_ready`, `frame_presented`. Dual playback selects newest frame at/before clock target ready in **both panes and active split lanes**, presents the pair together, otherwise retains last pair. Drop missed frames; never wait for decode/replay backlog. Update playhead/annotations from the shown frame. Standalone viewport uses the same readiness rule.
- This separation is essential: applying strict latest-request-only painting at 24 FPS can reject every 326 ms decode forever. Latest-wins remains strict for paused/user scrubbing; prefetch results only populate current-generation cache. During playback choose ready cached frames without waiting for the current clock target to finish.
- Resolve finite-positive FPS from metadata, otherwise a background video probe, otherwise 24 for untimed image sequences; preserve fractional FPS. Apply async probe only to still-selected clip. No per-frame probe.

### Reset and retry

- Visible translated **Reset Hint** in Alpha Hint tools. Confirmation has unchecked **Also clear annotations**. Delete AlphaHint directory/recognized video variants, VideoMamaMaskHint directory/recognized video variants, and mask manifest. Preserve input, Output/custom Output and completion manifests, annotations by default, holdout_strokes.json, parameters, metadata and in/out.
- Checked clearing removes SAM prompts in the authoritative/shared model AND both viewports' private models **before any reload/save**; remove annotations.json, refresh overlays/counts/coverage. Do not route through chroma-mode-dependent clear, which can clear holdouts instead.
- Pre-inference RAW/MASKED/READY resets to RAW through successful deletion + rescan. Idempotent; deletion failures surfaced, no false success. Add MASKED→RAW only if the explicit reset path uses transitions.
- **Completed-output exception:** preserved valid complete outputs necessarily resolve COMPLETE, not RAW. Keep that truthful disk-derived state while resetting the hint workflow and allowing generators again. Say outputs are retained. Do not delete/invalidate output manifests, add fake-RAW persistence, or assign RAW only to have the watcher reverse it. Tests cover COMPLETE after rescan/reopen as well as RAW for reset clips without completed outputs.
- Apple Vision/GVM/BiRefNet/Chroma can retry in READY and retained-output COMPLETE, with platform checks. Existing complete alpha requires Replace/Cancel, never silent mixing/resume. Preserve existing RAW/MASKED partial-alpha Resume/Regenerate/Cancel where applicable. Replacement clears AlphaHint only; keep masks and prompts. Normalize generator job snapshot state with existing `reset_complete_state` where needed.
- SAM preview after auto/chroma hints does not delete anything. Confirmed full track replaces the hint/mask through existing job queue/backend. Reject stale preview/continuation after reset/clip change. MatAnyone2/VideoMaMa still require actual mask assets; READY or prompts alone never enables them; imported masks remain accepted.
- One guard shared by buttons AND direct handlers: block destructive hint mutations while any GPU job is running/pending, cancel is still completing, or a pipeline chain is active. Use is_busy/current_job_id, queue pending/running snapshots, processing state; not isRunning alone. Recheck after modal dialogs and before deletion/submission; validate selected clip root identity. Idle persistent worker is allowed.
- `use_alpha_hint` stays unchanged. Hint off still runs RAW; hint on without AlphaHint stays disabled/warns as before.

## Tasks

Each numbered item is ONE worker assignment. Commands run from `/home/npittas/EZ-CorridorKey2`. Every pytest invocation must exit **0**; a green summary followed by segfault is failure. Preserve existing EOLs, including mixed files; new files LF. Shared-file writers are serial.

### 0. Read-only preflight — serial before all workers
- Files: `ui/app.py`, `.gitattributes`, Git history/index; optionally add evidence to this plan. No application changes.
- Changes: confirm branch/status; record EOLs for all planned files; run `git log -S 'setNumThreads' -- ui/app.py`, `git blame -L 30,112 -- ui/app.py`, `git show <introducing-commit> -- ui/app.py`. Record actual upstream rationale; current code proves Windows-only scope but not historical intent. Stop for overlapping uncommitted edits.
- Verify: `git status --short; git branch --show-current; git ls-files --eol -- ui/app.py ui/preview ui/widgets ui/main_window.py ui/main_window_mixins backend/clip_state.py tests`.
- Baseline: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests` (reported baseline 927 passed/25 skipped; independently confirm counts **and exit status**).
- Acceptance: history/EOL/baseline evidence recorded without normalizing files. Planner had file-reading tools, not a shell runner: these commands are required, not claimed already executed.

### 1. Byte-bounded display cache — parallel-safe after 0
- New files: `ui/preview/frame_cache.py`, `tests/test_frame_cache.py`.
- Changes: `FrameCache(max_bytes)` with get/put/clear/bytes_used/generation; constants for 512 MiB pane and small legacy budget. Thread-safe LRU keyed by the contracts above. Clear prevents stale in-flight insertion.
- Tests: row-padding bytes, replacement/LRU, oversized entry, clear/generation, key separation, concurrent get/put, detached QImage lifetime.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_frame_cache.py`.
- Acceptance: retained image bytes never exceed constructor budget, no float buffers cached. Disjoint from 2 and 4–8/12–13.

### 2. Viewport-size transforms with source geometry — parallel-safe after 0
- Files: `ui/preview/display_transform.py`, `tests/test_display_transform.py`.
- Changes: optional target_size on image/video decode; aspect-fit numeric downsample; image_source_size/text metadata; preserve existing full-res color behavior and unicode-safe I/O. Composite Processed alpha correctly; always release VideoCapture in finally. Cache migration belongs to 3.
- Tests: synthetic 4K-shaped EXR/PNG/video reduced to fit; gamma spy sees reduced arrays; no upscale; original-size metadata/copies; full-res requests; INPUT linear truth, matte gamma, negative/HDR/alpha edge colors; failed-video release.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_display_transform.py tests/test_frame_io.py tests/test_unicode_paths.py`.
- Acceptance: no expensive full-res gamma at fit; no format/mode regression. Parallel with 1; 3 waits for both.

### 3. Bounded foreground/prefetch scheduler — serial after 1 + 2
- Files: `ui/preview/async_decoder.py`, `ui/preview/display_transform.py`; new `tests/test_async_decoder.py`.
- Changes: preview-owned shared pool; immutable `DecodeRequest` with generation, lane (`primary`/`split_input`), requested presentation vs actual transform mode, source/index/video/target size. Latest pending foreground per lane, bounded prioritized prefetch, key deduplication, queued-work replacement, invalidate/cache lookup APIs. Preserve request_decode/frame_decoded compatibility until callers migrate; add lane-aware ready signals. Inject per-pane FrameCache into decode functions; replace old global frame-count LRU with small bounded default; clear_cache remains for legacy callers. Explicit queued QObject-slot completion and signal lifetime cleanup.
- Tests: hundreds of requests behind blocked fake workers leave bounded running/latest pending/prefetch; no duplicate work; newest scrub only; independent lanes; clip/mode/size generation rejection; completion on GUI thread; prefetch ahead/behind/window clamp/wrap, obsolete queue removal and foreground priority; decoder destroyed safely.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_async_decoder.py tests/test_frame_cache.py tests/test_display_transform.py tests/test_thumbnail_worker.py`.
- Acceptance: no global-pool mutation or whole obsolete clip ahead of a seek; default thumbnail calls continue working. Use fake pools/events, not long sleeps.

### 4. Deliberate CPU fan-out policy — parallel-safe after 0; integrate with 3
- Files: `ui/app.py`; new `tests/test_preview_threading.py`.
- Changes: preserve verified Windows safeguard. For Linux outer preview pool using affinity CPUs (max 32 here), set OpenCV internal threads to 1 once at startup to avoid nested teams. This is a justified process-global policy, NOT a per-worker setting; never flip it during decode jobs. Do not change Torch/TF32/CUDA settings. Benchmark before accepting policy; removing a Windows-only setting cannot be presented as the Linux fix.
- Tests: platform behavior via fake module references, affinity/pool sizing, no global Qt pool modification. Keep documented deadlock/starvation protections discovered in task 0.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_preview_threading.py tests/test_service_concurrency.py tests/test_v2_runtime_hooks.py`.
- Acceptance: independent frames can occupy available Linux cores without nested-thread explosion; actual inference/performance check in 20.

### 5. Preserve annotation coordinates at cache resolution — after 2; parallel with 3/4/6–8
- Files: `ui/widgets/split_view.py`, `tests/test_split_view.py`.
- Changes: use source dimensions for paint_annotations/brush/mouse image coordinates, actual raster for eyedropper samples. Add canvas-size notification for resize/split-divider sizing. Preserve image setter APIs, fit-relative zoom, pan/tandem sync and wipe behavior.
- Tests: full/fit images yield identical source-coordinate strokes/clicks/radii at zoom/pan; split-left clipping; unchanged image rect; eyedropper mapping; no feedback loop.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_split_view.py tests/test_annotation_prompts.py`.
- Acceptance: strokes painted at 720p fit stay correctly located on 4K SAM/holdout frames.

### 6. Repair the actual slider click-and-drag gesture — parallel-safe after 0
- Files: `ui/widgets/timeline_widgets.py`; new `tests/test_timeline_drag.py`.
- Changes: `_FatSlider` must establish real left-button drag state, update value from held-button mouse moves and finalize on release; preserve click-to-seek enlarged hit area. Small explicit `_scrubbing` flag plus shared `_value_from_pos` helper is sufficient, or correctly arm native QSlider handling; do not accept press and leave base move unarmed. Respect min/max, disabled state, release outside widget, and right-to-left/inverted appearance via QStyle.sliderValueFromPosition where appropriate. Preserve MarkerOverlay interception near in/out handles.
- Tests: real QTest press→multiple held-button QMouseEvent moves→release, across groove and handle; intermediate valueChanged before release; disabled/no-button moves don't seek; end clamping; marker hover/drag doesn't steal ordinary slider dragging. Merely calling setValue is NOT a gesture test.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_timeline_drag.py`.
- Acceptance: dragging works continuously even with the old scrubber debounce in isolation. Separate file ownership permits parallel work with 7.

### 7. Immediate scrub and FPS transport — parallel-safe after 0; integrate with 6
- Files: `ui/widgets/frame_scrubber.py`; new `tests/test_frame_scrubber.py`.
- Changes: remove debounce timer/docs; slider/marker/wheel/key steps emit immediately, external set_frame stays silent. Add set_fps, playback_requested(index, cycle), playback_state_changed(bool), public stop_playback. QElapsedTimer + PreciseTimer computes skipped targets at fractional FPS; last-presented playhead separate. Re-anchor on user seek/range/FPS edits. Loop inclusive valid in/out, stop at final frame without drift; same-count coverage refresh must not stop playback.
- Tests: actual task-6 drag produces frame_changed before release, repeated <50 ms changes do not stall; marker drag; silent set_frame; synthetic clock 24/23.976/60 FPS; delayed timeout skips frames, no drift; loop cycle, one-frame bounds, reversed/clamped markers, non-loop end, pause/seek/resume, unchanged range refresh.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_frame_scrubber.py tests/test_timeline_drag.py`.
- Acceptance: no hidden quiet-period stall and no decode wait in transport. Use fake elapsed clock, not flaky timing sleeps.

### 8. FPS/readiness helpers — parallel-safe after 0
- New files: `ui/preview/playback.py`, `tests/test_playback.py`.
- Changes: resolve_clip_fps from sidecar → background-safe unicode video probe → 24 default; validate finite positive values. Pure choose_ready_frame(target, lo, hi, last_presented, ready_indices) picks newest eligible ready index; caller handles loop cycle reset. No QWidget/inference imports.
- Tests: fractional metadata; missing/malformed/null/NaN/inf/zero FPS; extracted sidecar precedence, video release, sequence default; newest-ready gaps/drop/empty/end selection.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_playback.py`.
- Acceptance: reusable timing semantics without per-frame probing or arbitrary 3 FPS fallback.

### 9. Cached background navigation in PreviewViewport — serial after 3 + 5
- Files: `ui/widgets/preview_viewport.py`, `tests/test_preview_viewport_color_space.py`; new `tests/test_preview_navigation.py`.
- Changes: one bounded cache per viewport; explicit invalidate on clip/placeholder/hint-content reset/destruction. Existing FrameIndex/fallback builds requests, including video. Add prepare_frame/has_cached_frame/present_cached_frame and frame_ready/frame_presented signals. Ordinary scrub paints latest requested frame and prefetches. Canvas/DPR fit sizing and zoom full-res promotion; no per-pan decode flood. Get clip dimensions from known metadata/decoded source size, not synchronous full-image get_dimensions. Preserve requested mode when displaying INPUT fallback.
- Tests: delayed old clip/index/mode/linear results don't paint; own-pane cache isolation; same-path overwrite refresh; target/DPR/zoom full-res; all modes + video + missing-mode fallback; annotation stem follows shown image; clip-info path does not read pixels on GUI.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_preview_navigation.py tests/test_preview_viewport_color_space.py tests/test_frame_index.py tests/test_view_mode_bar.py`.
- Acceptance: primary navigation is cache-hit presentation or bounded worker request; no GUI pixel I/O.

### 10. Async split-left/live preview and standalone playback — serial after 9 + 7 + 8
- Files: `ui/widgets/preview_viewport.py`; new `tests/test_preview_playback.py`.
- Changes: _load_split_images schedules split_input lane sharing cache, never direct decode. Route by lane even when primary INPUT fallback. Async load_preview_from_file retains same-clip/latest/annotation guards and reused-temp revision invalidation. Internal scrubber clock prepares/selects ready frames rather than rejecting all slow results; stop/seek/mode changes invalidate presentation context, not valid current-generation caches indiscriminately. Update annotation stem to displayed frame.
- Tests: thread spies show split-left sequence/video and temp previews off QApplication.thread(); fallback correct sides; split presentation not mixed; cold decode advances despite faster clock; pause/seek strict latest-wins; annotation mode suppresses intrusive worker previews.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_preview_playback.py tests/test_preview_navigation.py tests/test_main_window_preview_modes.py`.
- Acceptance: standalone and split viewers use the same bounded off-thread path.

### 11. Dual-pane ready-pair playback, FPS and wipe — serial after 10
- Files: `ui/widgets/dual_viewer.py`, `tests/test_dual_viewer_live_refresh.py`; new `tests/test_dual_viewer_playback.py`.
- Changes: background FPS resolve once/clip with root identity check; configure shared scrubber. Each clock target prepares both panes/slides prefetch; present newest common ready index together, or retain pair, never await. Separate clock target/display index and emit consistent actual frame_changed. Wipe updates from accepted frame_presented, not arbitrary decoder completion. Larger wipe canvas requests appropriate resolution through existing pane caches (no third cache). Live coverage refresh preserves clock, navigation, in/out and INPUT truth.
- Tests: unequal decode latency cannot show mismatched stems; cold makes progress and warm follows FPS; stalled pane drops frames without blocking; loop/end/markers/live progress; wipe only current accepted images; video input and all output modes; clip switch cancels presentation.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_dual_viewer_playback.py tests/test_dual_viewer_live_refresh.py tests/test_preview_playback.py tests/test_main_window_color_space.py`.
- Acceptance: clip duration follows FPS, both panes agree, missed frames dropped instead of slow-motion backlog.

### 12. Shared hint-artifact deletion — parallel-safe after 0
- New files: `backend/hint_assets.py`, `tests/test_hint_assets.py`; file: `backend/clip_state.py` (transition table only if used).
- Changes: clear_hint_assets(clip, *, clear_alpha=True, clear_masks=True), has_hint_assets; exact existing folder/video extension logic. Remove provenance when masks cleared; don't follow source symlinks/delete outside clip root. Raise deletion errors (no ignore_errors). No Qt/queue/annotations/output handling. Permit MASKED→RAW if used; no completion resolver rewrite.
- Tests: alpha/mask folders/videos/partial/empty/idempotent; similarly named unrelated files untouched; deletion error; source, outputs/custom output/manifest, metadata, annotations/holdout/in-out unchanged; mask manifest gone; READY/MASKED→RAW after rescan; COMPLETE outputs retain COMPLETE after repeated rescan/restart.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_assets.py tests/test_clip_state.py tests/test_v2_chain_state.py`.
- Acceptance: exact deletion boundary; no false RAW state/persistence hack.

### 13. Reset Hint parameter-panel control — parallel-safe after 0
- Files: `ui/widgets/parameter_panel.py`, `tests/test_parameter_panel.py`.
- Changes: _reset_hint_btn self.tr("Reset Hint"), reset_hint_requested Signal, set_reset_hint_enabled. Add explicit Track-enabled API so annotation info refresh cannot override busy disable. Preserve platform availability/use_alpha_hint controls. Checkbox is per-reset dialog, not persistent preference.
- Tests: visible in hint group, signal once, disabled without clip, enable APIs, busy annotation refresh doesn't enable Track, Apple Vision platform gate retained.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_parameter_panel.py`.
- Acceptance: reset discoverable beside hint tools, not context-menu-only.

### 14. Guarded reset confirmation/orchestration — after 12 + 13; parallel with playback chain
- New files: `ui/main_window_mixins/hint_reset_mixin.py`, `tests/test_hint_reset.py`.
- Files: `ui/main_window_mixins/__init__.py`, `ui/main_window.py` (exports/inheritance/signal wiring only).
- Changes: HintResetMixin with _hint_mutation_allowed, _refresh_hint_button_state, _on_reset_hint, _refresh_after_hint_change, _confirm_replace_alpha. Translated QMessageBox + unchecked QCheckBox, Reset/Cancel; retained outputs/holdouts explained. Busy/root check before and after modal; stop playback; delete through 12, optionally clear authoritative + private annotation models BEFORE reload/save. Refresh state/model/thumbnails/coverage/counters/assets, keep current frame/in-out/INPUT override/zoom. Invalidate decode/presentation and pending SAM preview context.
- Tests: cancel untouched; default prompts survive disk + memory + later clip switch; checked prompts absent and cannot resurrect, holdout retained; exact artifacts; RAW without complete output, retained COMPLETE otherwise; partial files even missing alpha_asset; running/queued/cancel-in-progress refusal; idle persistent worker allowed; changed selection refused; deletion error no false success; hint-on/off Run unchanged.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_reset.py tests/test_hint_assets.py tests/test_v2_run_actions.py`.
- Acceptance: no GPU cancellation/forced teardown/output deletion hidden in reset. Integrate viewport APIs against 9–11 before end-to-end verification.

### 15. Shared hint control lifecycle refresh — after 14
- Files: `ui/main_window_mixins/clip_mixin.py`, `ui/main_window_mixins/inference_mixin.py`, `ui/main_window_mixins/worker_mixin.py`; new `tests/test_hint_button_state.py`.
- Changes: replace selection-only duplicate gating with shared refresh, called via _refresh_button_state, submission, completion/error/cancel and queue idle. Validate SAM preview job/context in _on_sam2_preview_result before display/confirm/submission. Queue idle must re-enable after current_job_id is cleared, even if clip_finished arrived earlier. Keep Run/Resume/batch rules.
- Tests: idle input RAW/MASKED/READY/COMPLETE retry tools; extracting/error/missing input disabled; queued-before-worker disabled; terminal idle re-enables; stale preview cannot track; coverage cannot re-enable busy tools; guided generators only with actual tracked/imported mask.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_button_state.py tests/test_hint_reset.py tests/test_v2_run_actions.py tests/test_main_window_preview_modes.py`.
- Acceptance: buttons and handlers agree, including immediately after generator finishes rather than only after reselect.

### 16. Confirmed auto/chroma READY retries — after 15
- Files: `ui/main_window_mixins/model_run_mixin.py`, `ui/main_window_mixins/chroma_key_mixin.py`; new `tests/test_hint_rerun.py`.
- Changes: shared guard/replace for Apple Vision/GVM/BiRefNet/Chroma. READY/COMPLETE Replace/Cancel, RAW/MASKED partial recovery retained. Replacement uses alpha-only helper, rescans before frozen snapshot and normalizes snapshot state with reset_complete_state as needed; keeps masks/prompts/outputs. No double prompts. Keep guided mask guards; guided alpha overwrite also explicitly confirms without deleting masks. Preserve chroma persistence/live preview.
- Tests: each JobType from READY after Yes, cancel unchanged; chroma↔GVM; partial resume/regenerate/cancel; platform; direct busy handler refusal; masks/prompts/output unchanged; READY no-mask cannot run guided; snapshot no stale alpha pointer; declined guided overwrite untouched.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_rerun.py tests/test_hint_button_state.py tests/test_v2_run_actions.py`.
- Acceptance: retry methods without deleting outputs or mixing another generator's existing hints.

### 17. SAM replacement/annotation-clear consistency — after 16; serial with 15
- Files: `ui/main_window_mixins/annotation_mixin.py`; tests: `tests/test_hint_rerun.py`, `tests/test_hint_reset.py`, `tests/test_main_window_preview_modes.py`.
- Changes: guard Track but allow previews after READY; alpha-only replacement only on confirmed full track, never on preview rejection. Existing backend still owns new masks/manifest. Handle preview→track terminal race: if accepted preview job is not yet terminal, defer continuation to terminal/queue-idle signal, revalidate context; don't start a second GPU job or silently drop accepted continuation. Reuse 12 for annotation-clear mask cleanup, clear both private prompt copies; keep actual-mask predicate/import route.
- Tests: chroma→SAM, SAM→chroma, reset→retained prompts→SAM; rejected preview leaves files; terminal handoff serial/not lost; stale/other-job preview ignored; annotation clear removes manifest/guided enable; track/import enables guided. Mock SAM/service, no model downloads.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_rerun.py tests/test_hint_reset.py tests/test_main_window_preview_modes.py tests/test_service_videomama_contract.py tests/test_sam2_wrapper.py`.
- Acceptance: all existing SAM2.1 variants share safe retry semantics; no old alpha destroyed just to preview.

### 18. Reuse helper in existing tray clear actions — after 12 + 14; serial with main_window.py writer
- Files: `ui/widgets/io_tray_actions.py`, `ui/main_window.py` (guard wiring only); new `tests/test_hint_tray_actions.py`.
- Changes: clear-mask/alpha and hint portion of clear-all delegate to shared helper. Keep current confirmation/selection/extraction recovery; only Clear All retains output deletion. Small set_hint_mutation_guard(callback) hook supplied by MainWindow; recheck after confirmation so context menu cannot bypass busy guard. Show deletion errors; no general menu/delete redesign.
- Tests: mask manifest cleanup; alpha keeps masks/prompts; Clear All old output behavior; Reset never invokes Clear All; active/pending queue refusal; coverage/selection/video extraction recovery.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_tray_actions.py tests/test_hint_assets.py tests/test_hint_reset.py`.
- Acceptance: one deletion implementation and no legacy clear bypass of mutation safety.

### 19. Touched Qt harness + full regression — after implementation
- Files: `tests/test_main_window_preview_modes.py`, `tests/test_dual_viewer_live_refresh.py`, `tests/test_split_view.py`, `tests/test_parameter_panel.py`, new Qt test modules (fixture/mock safety and API fixture changes only).
- Changes: module-level `_APP = QApplication.instance() or QApplication([])` wherever QWidget used. Replace existing static QMessageBox class monkeypatch in preview-mode tests with fake **module reference**. Update __new__/dummy viewport fixtures explicitly for cache/decoder state, not production getattr bandaids. Drain worker completions before deterministic `shiboken6.delete` dialogs/widgets; no QTimer.singleShot without app. No active QThread/QRunnable signal destruction.
- Tests: every new group, color-space persistence, annotations, split/wipe, all modes, in/out+loop, raw video/unicode, live progress, use_alpha_hint Run/Resume, imported masks, output overrides/completion recovery. Repeat preview/reset groups together to catch ordering/exit bugs.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests`; `git diff --check`; compare `git ls-files --eol` with 0.
- Acceptance: full suite exits 0, no unexplained skips, shutdown segfaults/QThread-destroyed warnings, or EOL-only diffs. Baseline counts do not excuse an exit crash.

### 20. Real-clip benchmark + GUI verification — after 19
- New file: `scripts/preview_smoke.py` (diagnostic only); optionally append measured evidence to this plan.
- Changes: accept --clip-root/--mode/--viewport WIDTHxHEIGHT/--seconds; use existing DualViewerPanel, record presentation timestamps/indices, cache bytes, requested/displayed FPS, drops and heartbeat lateness. Separate cold/warm. Real source/FG EXR, not only tiny PNGs. Read-only by default, no hint reset/proxy/inference/output writes. Stop/close/drain workers cleanly.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python scripts/preview_smoke.py --clip-root /path/to/the/122-frame-4K-clip --mode FG --viewport 1280x720 --seconds 10` (substitute actual clip). Repeat on-screen without offscreen, COMP/Alpha/Mask/Processed and video input. Run `.venv/bin/python scripts/v2_smoke.py --help`, then its existing short real-GPU smoke invocation (don't invent flags), to check threading-policy regression.
- Acceptance: targets/checklist below, measured on actual machine. Missed targets require bottleneck report/fix within these components, not proxies/new dependencies/changed output compression.

## Files to Modify

- `ui/app.py` — documented startup thread policy.
- `ui/preview/display_transform.py` — sized transforms/source geometry/cache injection.
- `ui/preview/async_decoder.py` — bounded lane-aware scheduler/private preview pool.
- `ui/widgets/timeline_widgets.py` — **actual click-and-drag repair**.
- `ui/widgets/frame_scrubber.py` — immediate events and FPS/elapsed transport.
- `ui/widgets/split_view.py` — original source-coordinate painting and canvas sizing.
- `ui/widgets/preview_viewport.py` — cached async primary/split/temp preview and standalone playback.
- `ui/widgets/dual_viewer.py` — ready-pair presentation, FPS and wipe.
- `ui/widgets/parameter_panel.py` — Reset Hint/explicit enable APIs.
- `ui/widgets/io_tray_actions.py` — deletion helper/busy guard reuse.
- `ui/main_window.py`, `ui/main_window_mixins/__init__.py` — mixin/signal/guard wiring.
- `ui/main_window_mixins/clip_mixin.py`, `inference_mixin.py`, `worker_mixin.py` — shared hint lifecycle refresh and stale preview guard.
- `ui/main_window_mixins/model_run_mixin.py`, `chroma_key_mixin.py`, `annotation_mixin.py` — confirmed retries/consistent clear.
- `backend/clip_state.py` — MASKED→RAW only if used; retain disk resolver.
- Existing test files explicitly named in assignments — regression additions and safe Qt fixtures.

Read-only/reuse (no service rewrite): `backend/service/frame_ops.py`, `pipelines_auto.py`, `pipelines_guided.py`, `backend/job_queue.py`, `ui/workers/gpu_job_worker.py`, `job_helpers.py`, `ui/preview/frame_index.py`, `backend/frame_io.py`, `backend/ffmpeg_tools/metadata.py`.

## New Files

- `ui/preview/frame_cache.py` — byte-bounded display-image LRU.
- `ui/preview/playback.py` — small FPS/readiness helpers.
- `backend/hint_assets.py` — exact reusable hint-artifact deletion.
- `ui/main_window_mixins/hint_reset_mixin.py` — reset/replacement guard and orchestration.
- `scripts/preview_smoke.py` — real-clip presentation measurement.
- `tests/test_frame_cache.py`, `test_async_decoder.py`, `test_preview_threading.py`, `test_timeline_drag.py`, `test_frame_scrubber.py`, `test_playback.py`, `test_preview_navigation.py`, `test_preview_playback.py`, `test_dual_viewer_playback.py` — cache/gesture/decode/playback regression coverage.
- `tests/test_hint_assets.py`, `test_hint_reset.py`, `test_hint_button_state.py`, `test_hint_rerun.py`, `test_hint_tray_actions.py` — deletion/job-safety/state/retry coverage.

## Dependencies / parallel ownership

- 0 first; then 1, 2, 4, 6, 7, 8, 12, 13 can run in parallel. 5 follows 2 and overlaps 3.
- Playback: `(1 + 2) → 3`; `(3 + 5) → 9`; `(9 + 6 + 7 + 8) → 10 → 11`; 4 integrates before performance acceptance.
- Reset: `(12 + 13) → 14 → 15 → 16 → 17`; 18 after 14, and may overlap 15–17 with disjoint files/tests. 14's viewer invalidation must integrate against 9–11 before end-to-end tests.
- 19 after all implementation; 20 after green tests.
- Never parallelize writers to display_transform.py (2/3), preview_viewport.py (9/10), main_window.py (14/18), or hint_rerun/reset tests (16/17). These are dependencies, not permission for last-writer-wins.

## Manual GUI verification checklist

Use real 122-frame 4K source/output, then a raw video project. Use disposable copies for destructive checks.

- [ ] Actual mouse press/continuous drag updates both panes **while held**, from groove and handle; exact release frame. Repeat with wheel/keys/Home/End and in/out marker drags.
- [ ] Play/pause/seek/resume, 23.976/24/30/60 timing as fixtures allow; loop on/off and short inclusive range; correct duration/end, no out-of-range display.
- [ ] Cold EXR cache stays interactive, advances when ready, drops missing frames without mismatched panes. Warm cache gives realtime presentation.
- [ ] INPUT/FG/Matte/Comp/Processed/Alpha Hint/Mask including holes/INPUT fallback; no wrong-side split input or old-mode image.
- [ ] Split divider, A/B wipe, tandem zoom/pan, resizing and HiDPI; zoom sharpens asynchronously without geometry jump.
- [ ] Paint/undo/save/reopen and eyedropper before/after zoom; source-pixel alignment and brush radii unchanged.
- [ ] Clip switch/project clear during in-flight decode: no old-image resurrection, old cache evicted. Live progress/temp previews respect painting/browsing.
- [ ] Chroma→Reset(default)→SAM→MatAnyone2/VideoMaMa, SAM→Reset→Chroma/GVM; default prompts survive and are editable.
- [ ] Checked Also clear annotations removes SAM strokes/counters/coverage across reopen and clip-switch; holdouts remain.
- [ ] READY rerun each existing generator; decline then accept Replace. Partial RAW hints retain intended recovery. Apple Vision is mock-tested on Linux, not claimed runtime-supported there.
- [ ] Guided generators disabled for alpha-only/annotation-only READY, enabled for imported/tracked masks, disabled again after reset.
- [ ] Running/queued/Cancel-in-progress reset and direct/menu overwrite refused; idle persistent worker allows it.
- [ ] COMPLETE reset preserves output/custom output bytes/manifest and disk-derived COMPLETE after reopen, but permits fresh hint workflow. READY/MASKED without complete outputs reset/reopen RAW.
- [ ] use_alpha_hint remains selected; missing-hint Run differs correctly on versus off.

## Performance acceptance

Report actual machine/clip FPS/canvas size/cold vs warm. Test 1280×720 **physical image per pane**, then desktop layout. 122 RGB888 frames at 1280×720 ≈322 MiB/pane, at 1600×900 ≈503 MiB; larger viewports may not retain entire clip under 512 MiB and must evict, not exceed budget.

- Warm seek-to-visible p95 ≤50 ms; intermediate drag updates and exact settled target.
- Warm 24 FPS playback ≥23 synchronized displayed frames/sec, ≤5% drops, real loop duration within 5%. For higher FPS report achieved FPS/drops; transport duration must remain correct.
- Cold decode: no decoder-induced GUI stall >100 ms; 20 ms GUI heartbeat p95 lateness ≤50 ms on otherwise idle machine. Cold EXR FPS codec/storage-dependent, not promised 24 before prefetch warms. Exact settled scrub normally ≤500 ms with supplied warm-filesystem clip; report limiting stage if missed.
- Cache ≤512 MiB per viewport including split lane; old-clip cache cleared; running/pending decode bounded after prolonged scrubbing. Report RSS separately: in-flight decompressed float EXRs/Qt/engine memory is not the image cache and can temporarily exceed combined 1 GiB.
- Thread spies confirm no GUI image/video/temp-preview decode. No proxy files or output re-encoding.
- CPU utilization scales across available cores without nested-team explosion. Short real-GPU smoke still works; investigate repeatable inference throughput loss >10% from startup thread policy.

## Risks / guardrails

- Planner read the code itself but had no shell execution tool; task 0 must supply requested Git-history/EOL/baseline evidence. Do not falsely claim those commands were already run.
- **Two independent scrub defects:** _FatSlider does not arm drag; debounce waits for quiet. Fix/test both with real held-button mouse events, not setValue-only tests.
- Latest-only playback can starve forever when decode slower than FPS. Ready-cache presentation differs deliberately from strict latest-wins scrub.
- ZIP EXR decompression remains full-res. Downsample transforms + CPU concurrency + prefetch, and measure; don't change user outputs or add proxies.
- OpenCV thread setting is process-global, not worker-local. Set once, preserve verified upstream rationale, test UI and inference.
- Downsampled raster dimensions must not become saved prompt coordinates. Live images without source metadata still need safe dimension fallback.
- Bound images AND queued/running jobs: a small cache alone cannot prevent many full-float transient arrays or obsolete decode backlog.
- Invalidate generation as well as clearing cache; late results/same-path overwrites must not revive deleted hints. Two panes must not clear each other.
- Reset is not Clear All/Clear Outputs. Retained complete outputs mean disk-derived COMPLETE; no fake RAW persistence or manifest removal.
- Modal dialogs permit nested event processing: recheck queue/clip root/context. Accepted SAM preview continuation must wait for terminal state without disappearing.
- Shared rendered prompts are not both private models. Clear/reload order must prevent annotations.json resurrection; don't clear chroma holdouts accidentally.
- Qt test rules: module-level QApplication; patch module references, never Qt class attributes; no singleShot without app; shiboken6.delete dialogs; drain active work before destruction; exit-time segfault is failure.
- Keep _tr/self.tr, Apple Vision platform gating, all current SAM2.1 choices, video/unicode sources, source interpretation, annotation painting, split/wipe, pan/zoom, all modes, in/out, loop and live refresh. No new dependencies or settings UI.

## Review amendments (orchestrator, binding — override the tasks above where they conflict)

1. **In-flight memory cap (task 3):** add `MAX_INFLIGHT_DECODES` (default `min(pool_threads, 12)`) so at most that many full-res float frames exist at once (~92 MB each at 4K RGBA float). Prefetch never exceeds it. `scripts/preview_smoke.py` reports peak RSS; acceptance: preview RSS growth ≤ 2.5 GB during 10 s cold 4K playback with both panes.
2. **Video input (task 3):** add a sequential-reader fast path — one `cv2.VideoCapture` per lane that reads forward when the next requested index is current+1 (no seek), and re-seeks otherwise. Close it on clip switch/destroy.
3. **Thread policy (task 4 merged into 3):** measure single-frame warm decode latency before/after the Linux `cv2.setNumThreads(1)` change; keep it only if single-decode latency is not worse by >20% OR playback throughput clearly improves. Record numbers in the task report.
4. **Loop-aware eviction (task 1/11):** when the cache is full, evict the entry farthest from the playhead in the playback direction (within the active loop range) instead of pure LRU; when not playing, LRU. If the clip doesn't fit, re-decoding on wrap is accepted.
5. **Baseline:** workers must re-measure the baseline themselves; 927/25 is only a reference.
6. **Task merges:** 13 → 14 (Reset button + dialog + mixin as one task); 5 → 9 (source-coordinate handling lands with the viewport downscale); 4 → 3 (pool policy with scheduler); 18 stays separate.
7. Worker prompts must include verbatim: "Merely calling setValue is NOT a gesture test."
