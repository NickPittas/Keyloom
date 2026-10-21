# Implementation Plan

## Goal
Expose CorridorKey 2.0's temporal windows and unified reference/keyframe conditioning in EZ-CorridorKey2, with faithful engine semantics, persistent per-clip keyframes, and actionable EXR input-colour feedback.

## Open questions

Resolve these with the orchestrator before the affected assignments; do not let workers invent policy.

1. **EXR sampling coverage:** the threshold is fixed, but the frames to sample were not specified. Proposal: first, middle and last source frames (deduplicated), raw RGB values, maximum over finite colour samples, decoding one image at a time in a worker. This is a bounded heuristic, not proof of transfer function. Confirm this sampling policy before task 9. Do not substitute a whole-clip scan or an automatic colour-space change.
2. **AlphaHint replacement other than Reset Hint:** Reset Hint will clear keyframe marks (decision below). Should replacing/regenerating/importing AlphaHint without Reset preserve the marked *positions*, now pointing at the new mattes? Proposal: yes, while source index/stem identity is unchanged; explicit Reset Hint and destructive tray Clear Alpha/Clear All clear marks. Confirm this lifecycle policy before task 19. Do not silently delete marks on ordinary generator completion.

**Important interpretation, not another feature:** a reference at k is injected only at slot k. Forward window traversal does not revisit already-flushed earlier windows. However, a temporal window containing k can condition predictions for earlier frames *within that same window*. “Frames before k get no hint” means no direct external-hint tensor at those slots; it cannot promise zero influence from k inside a joint temporal model. Do not implement reverse inference, a second pass, or a hard temporal split.

## Scope and status

- Target: `/home/npittas/EZ-CorridorKey2`, branch `corridorkey-v2`, existing Python 3.11 `.venv` and PySide6 app. Reference only: `/home/npittas/CorridorKey2.0/README.md` and source checkout. Change only the vendored engine, never the reference checkout.
- Planning only: only this file was written. Source and the two previous plans were read; branch, working-tree/EOL state, tests and GPU performance were **not** executed/verified by the planner.
- Follow `PLAN-corridorkey-v2.md` and `PLAN-playback-and-hint-reset.md`: one small worker assignment per numbered task, exact files, tests, commands and ownership. No new dependency or alternative inference implementation.
- No brightness/contrast controls, `model_scale`, cutout-linear changes, backwards propagation, extra hint generator, per-keyframe trust slider, model changes, playback/cache refactor, or output-format/colour-conversion changes.
- In/Out and Resume continue to infer the **whole clip** and restrict publication only. Keyframes use absolute zero-based source indices, including marks outside the publication range. Resume saves publication work, not GPU inference work.

## Confirmed code and upstream contracts

Line numbers in the request are approximate; use symbols, not blind line replacements.

| File / symbol | Inspected behaviour / consequence |
| --- | --- |
| Reference `README.md`, Choosing An Inference Mode / Hint Quality / Hann Windows | Upstream offers none, first-frame, full hints; carry is auxiliary-matte conditioning on overlapping windows, not general previous-frame tracking. Empty window quality is -1 everywhere; a partially hinted window has 0 on unhinted slots. Full-hint carry currently overrides external hints on carried overlap frames. |
| `CorridorKeyV2/corridorkey/runtime.py:InferenceConfig` | No indexed reference or sparse-index setting. Default chunk/stride 80/40; adapter enables automatic selection. |
| `hann_stage1_stream` | Allocates hint/quality/presence tensors per window; dense hints fill all slots, then carry overrides overlap, then initial hint overrides slot 0 only when `t0 == 0`. Presence is internal bookkeeping; stage1 receives hint and quality, **not** a new `hint_present` model argument. |
| `video_io.py:SequentialHintFrameDirWindowReader` / `make_hint_reader` | Dense directory is lexically compacted and requires at least source-frame count. Missing numbered files cannot represent sparse timing today. Existing `.window()` returns a tensor. |
| `video_io.py:read_hint_image` / `load_initial_hint_frame` | uint8/uint16 normalization, RGBA alpha-channel selection, clamp to [0,1], resize/pad to model shape. Reuse these semantics for staged reference/keyframe PNGs; mattes never get an RGB transfer transform. |
| `runtime.py:estimate_peak_vram_gb(megapixels, frames, low_vram)` | Exact signature; table/interpolation already exists. Normal table starts at 8 frames, low table at 1. Do not create another table. |
| `select_vram_profile(shape_hw, num_frames, budget_gb)` | Returns `(chunk, stride, low_vram, estimate)`; uses actual padded model shape, `min(chunk, num_frames)`, and `estimate * 1.1 + 0.3` safety allowance. Auto can prefer a larger low-VRAM window. |
| `apply_auto_vram` / adapter `_probe_low_vram` | Budget is `(torch.cuda.mem_get_info()[0] + torch.cuda.memory_reserved()) / 2**30`, not total VRAM. `CorridorKeyService.get_vram_info()['free']` is total minus own reserved and is **not** this budget. |
| `backend/corridorkey_v2.py:run_clip` | Always passes `initial_hint_path=None`; takes only dense hint path today. Builds a fresh config per clip and resets low-VRAM tuning before reuse. `V2RunResult.warnings` already feeds service warnings. |
| `backend/service/v2_inputs.py` | Owns source-order staging, stem/legacy ordinal hint matching, dimension checks and staging cleanup. Dense `_match_hints` deliberately rejects incomplete AlphaHint; sparse must use selected-frame matching, not weaken dense validation. |
| `backend/service/core.py:InferenceParams` | `use_alpha_hint`, quality/carry, auto/low VRAM, `to_dict`/`from_dict`; constructors are also used by CLI and smoke script. Preserve old constructor/session behaviour. |
| `backend/clip_state.py:can_run_inference` | Boolean hint-on requires READY/COMPLETE. A partial AlphaHint may leave the clip RAW/MASKED, so this gate must distinguish Keyframes from Full AlphaHint. Do not falsify disk-derived clip state to enable sparse runs. |
| `ui/workers/job_helpers.py` | Deep-copies the clip, but currently stores the passed params object. Put per-clip marks in the frozen clip snapshot, not in global panel/session params. |
| `ui/main_window_mixins/inference_mixin.py` | Dense count dialog, Run/Resume/Run All/pipeline classification still use the boolean. `_on_params_changed` currently does not refresh Run state. |
| `batch_pipeline_mixin.py` / `worker_mixin.py` | Several `dataclasses.replace(..., use_alpha_hint=True/False)` force generator/direct batch modes. All must be audited when hint mode becomes canonical. |
| `ui/widgets/parameter_panel.py` | Inference group owns boolean checkbox, quality/carry, auto/low VRAM, colour selection and get/set/reset APIs. Existing carry tooltip incorrectly implies next-frame propagation; correct it within this feature. |
| `ui/widgets/frame_scrubber.py` / `timeline_widgets.py:MarkerOverlay` | Existing in/out overlay, immediate user gestures and FPS playback. Add non-interactive keyframe ticks without stealing slider or marker drag events. |
| `clip_mixin.py:_sync_selected_clip_view` / `session_mixin.py` | Central selected-view reload and per-clip input-colour memory already exist. Selection, worker completion and import all reload through this path. Preserve colour overrides. |
| `backend/hint_assets.py` / `hint_reset_mixin.py` | Shared exact-artifact deletion, busy/root guards, rescan and reset confirmation already exist. Reset preserves keyed outputs, optional SAM prompts, holdouts and settings. Extend, do not replace. |
| `ClipEntry.should_default_input_linear` | Standalone EXR defaults Linear (video-derived EXRs use transfer metadata). Do not change this default based solely on the heuristic. Supplied 4K EXRs peaking 0.93–0.99 explain why a notice is needed, not a universal inference rule. |
| `runtime.py:run_inference` | Prints a warning for display-coded EXR directory input regardless of peak values. Promote via adapter pre-check, not stdout capture or another vendor patch. |
| `tests/test_v2_runtime_hooks.py` | Existing CPU Torch fake reader/foundations/stage1 machinery makes real Hann-loop hint-tensor tests feasible, without CUDA or weights. |
| `scripts/v2_smoke.py` | Current cases: `nohint_auto,nohint_low,png_outputs,range,cancel,hint_auto`. Actual flags: `--clip-src`, `--hint-src`, `--frames`, `--work`, `--cases`. It deletes its work subfolders; use a disposable work root only. |

## Implementation contracts

### A. Unified hints and compatibility

- UI choices: **None / Full AlphaHint / Keyframes (N)**. No separate reference-frame list or independent reference mode. A single marked frame is the reference frame; multiple marks are sparse keyframes.
- Proposed parameter API: `hint_mode: str | None = None` (canonical normalized values `none/full/keyframes`) and `temporal_window: str = 'auto'` (other allowed values `80/40,48/24,24/12,8/4,1/1`). Keep `use_alpha_hint` as a legacy constructor/serialized compatibility field, normalized from the canonical mode after initialization. When no mode was supplied, the old boolean maps to none/full. Explicit mode wins over a conflicting legacy boolean. Validate unknown modes/presets.
- Every repository mode-changing `dataclasses.replace` must set `hint_mode` explicitly, not just the compatibility boolean. Do not keep two independently authoritative fields. Old `InferenceParams(use_alpha_hint=True)`, CLI and old session dictionaries must still mean Full AlphaHint. None stays the default.
- Per-clip marks do **not** belong to global `InferenceParams` or global QSettings. Add immutable `ClipEntry.hint_keyframes: tuple[int, ...] = ()`, loaded from its sidecar and deep-copied with jobs. `InferenceParams.to_dict/from_dict` round-trip mode/window; clip sidecar round-trips marks.
- Selected-frame source is always that source frame's current AlphaHint, whatever generator/import produced it. Never use VideoMamaMaskHint, SAM strokes, a displayed/tone-mapped image, inference output alpha, or a separately picked image.
- None: both engine hint paths None, sparse indices None; engine's no-hint quality signal is applied per **window**, independent of carry enabled.
- Full: existing validated dense path, initial path None, sparse indices None; preserve all current full-hint/carry semantics.
- Exactly one mark k: stage only its matte; pass `initial_hint_path`, `cfg.initial_hint_index=k`, no dense/sparse path. This exercises the locked engine patch, including k>0.
- Two or more marks: stage only selected mattes as `<absolute_index:06d>.png`; use the sparse reader with explicit absolute indices. Do not write placeholder mattes or compact the selected list to 0..N-1. No unmarked AlphaHint content is read/sent.
- At sparse explicit slots, the marked matte wins; carry fills **gaps**. Keep the carry-cache generation/eviction algorithm unchanged and retain dense-mode carry precedence. Unhinted slots have quality 0 if any explicit/carried hint is present in that window; only a completely hint-free window has quality -1 throughout. Quality 0 does not mean the frame is externally present; a real all-black matte is still present.

### B. Minimal vendor API patch

- `InferenceConfig.initial_hint_index: int = 0`; optional `hint_frame_indices: tuple[int, ...] | None = None` distinguishes sparse directory input from dense input. None retains upstream behaviour. An empty sparse set is an error, not implicit hint-free mode.
- Thread both fields through `run_inference` to `hann_stage1_stream` as defaulted keyword-only args. Validate indexed reference against probed full source frame count before allocating/writing inference products. No model signature or weights change.
- Indexed initial injection: `if initial_hint is not None and t0 <= k < t1`, use slot `k - t0`, quality `initial_hint_quality`, presence True, in **every** containing overlapping window. No local In/Out rebasing. Log `Initial Hint Index (0-based): k` in runtime settings; keep the default first-frame behaviour.
- Keep existing dense-reader `.window()` compatibility. Add `.window_with_presence(t0, t1)` to dense readers (their tensor plus all-True mask) and a `SequentialSparseHintFrameDirWindowReader` implementing it. `make_hint_reader(..., frame_indices=None)` selects sparse only explicitly. Runtime consumes values plus per-slot bool presence; no basename heuristics changing old dense directories.
- Sparse constructor validates unique integer indices in range and exact index-preserving PNG set, no duplicate index spellings, missing files, extra files or out-of-range keys. Cache only selected frames needed by overlapping windows; missing slots are zero tensor / False, not decode errors or “black external hint”. Reject sparse-video input at vendor boundary; app stages selected video matte frames to PNG.
- Record each patch, semantics/default compatibility, settings-log addition and test files in `CorridorKeyV2/VENDORED.md`. Preserve earlier progress/cancel/cleanup and EXR-writer patches.

### C. Clip persistence / reset

- Proposed sidecar: `<clip.root_path>/.corridorkey_hint_keyframes.json`, alongside `annotations.json`, not under AlphaHint/Output or external source folders. Schema: `{"version":1,"keyframes":[{"index":12,"stem":"frame_0012"}]}` (sorted, unique records). Source index is authoritative only while the stored stem still matches the ordered source list at that index. Video uses the same `%05d` source stems as `v2_inputs`.
- Use an atomic tmp + `os.replace`; no save on load. Empty set removes the sidecar. Invalid schema/stale index-stem identity is reported, not silently rebound to another frame or written back. Loading a bad sidecar must not crash scanning or silently permit inference with an unintended subset; propagate a keyframe-specific diagnostic for UI/preflight.
- UI mark/unmark uses the current **presented** `DualViewerPanel.current_stem_index`, never a future playback clock target. Stop playback for an explicit mutation. Require the current AlphaHint frame to be resolvable to mark; allow unmarking even if its matte was removed. Block mutation with existing pending/running/cancel-in-progress/chain guard and recheck selected root before writing.
- **Reset decision:** Reset Hint clears all marks and their sidecar as well as existing hint assets, even when annotations are preserved. Inform the user in the reset text. Sidecar-only clips must still enable Reset Hint. Marks must not reappear from stale in-memory state after rescan/session load. Retain completed outputs and truthful COMPLETE state as before.
- Replacement/import/tray policy is subject to Q2. Do not confuse SAM annotation marks with external-matte keyframe marks.

### D. Temporal window and VRAM

- Preserve `vram_profile='auto'/'low'`. With Temporal=Auto: auto profile keeps `cfg.auto_vram=True`; low profile keeps existing forced-low largest-fit selection. No change to automatic defaults.
- With a manual temporal preset: set exact `hann_chunk/hann_stride`, `cfg.auto_vram=False`; `cfg.low_vram = (vram_profile == 'low')`. Thus Auto VRAM + manual temporal uses normal execution, Low VRAM + manual temporal uses the same window with low-memory execution. Explain this interaction; do not silently change the user's manual preset or profile.
- Put one small app-side helper in `backend/v2_temporal.py` which lazily uses the engine estimator/profile functions. Use native resolution padded to engine shape quantum (currently 128), model_scale fixed 1, and `min(chunk, clip_frame_count)` just like runtime. Take care of width/height ordering.
- Every manual option includes estimated peak text; Auto includes its recommended profile/peak when budget is known. Label GiB and “estimated”, not guaranteed fit. Low toggle and clip context changes recompute estimates. Unknown shape/budget displays unavailable state, never stale previous-clip numbers.
- Compare the engine safety-adjusted requirement (`peak * _VRAM_MARGIN_SCALE + _VRAM_MARGIN_GB`) with fresh free+own-reserved budget. UI: red warning label and explanatory tooltip; run: warning logged **before inference**, and included in `V2RunResult.warnings` if completed. Never disable Run, clamp the preset or reject an over-budget choice.
- For normal 1/1 the upstream estimator conservatively floors to its 8-frame calibration. Label this caveat instead of inventing memory measurements, extending tables, or automatically turning Low VRAM on.
- UI metadata/VRAM probes run in a worker, without loading the keyer. Fresh adapter budget after model residency is authoritative for the run. Preserve low→auto/manual→auto model-tuning reset and fresh config per clip.

### E. EXR colour notice

- Background numeric sampling of the selected EXR input, per Q1; use `backend.frame_io.imread_unicode(... IMREAD_UNCHANGED)`, no gamma/clamp/preview QImage. Ignore alpha when finding RGB peak. Bounded memory; return scalars/shape/count, not float image arrays.
- Sampled peak <=1.0 while set Linear: non-modal notice, e.g. “Sampled EXR RGB peaks at 0.99; this may be display-coded. Currently Linear.” One-click **Use display-coded (sRGB)**. Genuine linear plates can peak below 1; do not call this proven detection or switch automatically.
- Sampled peak >1.0 while set display-coded: non-modal HDR/clipping notice + **Use Linear**. No notices for PNG/video/non-EXR input. Empty/failed/non-finite sampling does not classify the clip or silently switch interpretation.
- Button routes through the existing colour-change path so panel, both INPUT previews, thumbnails and `_clip_input_is_linear` session memory all agree. A programmatic `set_input_is_linear` suppresses signals, so the click handler must explicitly run the normal update path. Old/stale results cannot change a different clip or overwrite a user choice made while probing.
- Independently, adapter pre-check adds the runtime's existing display-coded-EXR clipping warning to `last_warnings`/`V2RunResult.warnings`, and logs it before runtime execution. Applies even if numeric sampling found <=1 or UI is absent. No stdout parsing and no engine edit for this warning.

## Tasks

Each task is ONE worker assignment. All paths are relative to `/home/npittas/EZ-CorridorKey2`. Commands run there. Every pytest command must exit **0**; passing counts followed by a shutdown segfault are failure. Preserve EOLs with `git ls-files --eol` (many files CRLF/mixed); new files LF. Do not run concurrent writers on the same file, including tests and the vendor ledger.

### 0. Read-only preflight — SERIAL before all workers
- Files: Git status/index, all paths listed below, existing two plans, test configuration. No application changes.
- Changes: verify target checkout/branch, overlapping uncommitted edits and existing playback/reset review state; record EOLs. Confirm no source-checkout edits. Establish test baseline and exit code.
- Verify: `git status --short; git branch --show-current; git ls-files --eol -- CorridorKeyV2/corridorkey/runtime.py CorridorKeyV2/corridorkey/video_io.py CorridorKeyV2/VENDORED.md backend ui tests scripts/v2_smoke.py`.
- Baseline: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests`.
- Acceptance: explicit baseline/count/exit/EOL evidence, no clean-tree assumption. Stop for overlapping work; do not overwrite it.

### 1. Vendor indexed-reference injection — SERIAL after 0; engine work first
- Files: `CorridorKeyV2/corridorkey/runtime.py`, `CorridorKeyV2/VENDORED.md`; new `tests/test_v2_reference_hint.py`.
- Changes: `initial_hint_index=0` config/defaulted Hann arg; pass through runtime, validate k, inject at `k-t0` in each containing window after carry; settings-log index line and ledger. Keep old default/no-hint calls working.
- Tests: use small CPU Torch fake reader/model patterned on `test_v2_runtime_hooks.py`; capture stage1 hint/quality tensors for k=0, middle, last, overlapping containing windows and windows wholly before/after k. Check hint quality, zeros at unhinted slots, -1 for truly empty windows; black explicit hint is present. Invalid negative/out-of-range/noninteger k is a clear preflight error. Test run_inference config forwarding/settings log, old call with omitted index, cancel/cleanup.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_reference_hint.py tests/test_v2_runtime_hooks.py`.
- Acceptance: nonzero reference never lands at local slot 0 unless k equals window start; earlier flushed windows are not rerun. No model or reference-checkout changes.

### 2. Vendor index-preserving sparse reader — SERIAL after 1 (ledger ownership)
- Files: `CorridorKeyV2/corridorkey/video_io.py`, `CorridorKeyV2/VENDORED.md`; new `tests/test_v2_sparse_hint_reader.py`.
- Changes: reader/presence API in B; keep dense `.window()` API. Explicit sparse index argument to factory; numeric-index PNG mapping, selected-frame-only bounded decode/cache, scalar normalization and model padding matching existing readers. Preserve writer code.
- Tests: selected indices 0/middle/last; windows with zero/one/multiple selected slots; overlap cache/release; no reads for unmarked frames; valid black/white uint8/uint16 PNGs; exact mask/shape/dtype/padding. Missing index, duplicate numeric stem, non-PNG, extra index, out-of-range index, failed decode, bad path and sparse video rejected. Dense frame/video reader presence remains all True.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_sparse_hint_reader.py tests/test_v2_exr_writer.py tests/test_v2_reference_hint.py`.
- Acceptance: a directory with files `000003.png` and `000017.png` conditions frames 3 and 17, never 0 and 1. No dense behaviour change.

### 3. Vendor sparse Hann conditioning — SERIAL after 2
- Files: `CorridorKeyV2/corridorkey/runtime.py`, `CorridorKeyV2/VENDORED.md`; new `tests/test_v2_sparse_hann.py`.
- Changes: config/defaulted Hann `hint_frame_indices`; pass to reader factory; consume tensor/presence, assign full_hint_quality only at explicit slots; merge carry into sparse gaps, retain dense carry order and initial-reference override. Do not change aux carry creation/eviction, weighting or stage2.
- Tests: actual CPU fake Hann loop, both carry on/off; empty sparse windows (-1 everywhere), partially present windows (quality zero in missing slots), explicit quality, all-black marked matte, carried-only windows, explicit-vs-carried overlapping slot, final flush and no sparse hints before first mark. Golden comparison of none/dense carry tensors before and after patch. Capture `hint_present` via a small hint-assembly seam only if necessary; do not add a model argument just for tests. Existing reference/progress/cancel/cleanup tests must pass.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_sparse_hann.py tests/test_v2_sparse_hint_reader.py tests/test_v2_reference_hint.py tests/test_v2_runtime_hooks.py`.
- Acceptance: only sparse marked/carry slots are present; dense/no-hint/carry algorithm unaffected. All subsequent assignments wait for engine tasks 1–3 to land.

### 4. Backwards-compatible inference parameter schema — PARALLEL-SAFE after 3
- Files: `backend/service/core.py`, `tests/test_v2_params.py`, `tests/test_session.py`.
- Changes: mode/window fields and validation/legacy normalization from A/D; keep VRAM profiles, carry/trust defaults and known-key filtering. `to_dict` emits JSON-safe supported fields; `from_dict` restores old bool-only sessions and new mode/window sessions.
- Tests: default none+Auto; legacy constructor True/False; old dictionaries missing all new fields; each new mode/preset round-trip; explicit mode beats stale boolean; invalid mode/window; auto/low preserved; unknown retired params ignored; dataclasses.replace explicit mode transitions; no unexpected model imports on params-only import.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_params.py tests/test_session.py tests/test_v2_cli.py`.
- Acceptance: old sessions/CLI still work; no keyframe list leaks through global params.

### 5. Per-clip keyframe sidecar helper — PARALLEL-SAFE after 3
- New files: `backend/hint_keyframes.py`, `tests/test_hint_keyframes.py`.
- Changes: schema/path constant and small read/validate/save/clear helpers per C, taking root and ordered source stems (no UI/service imports or circular ClipEntry dependency). Sorted unique immutable indices in memory; atomic file updates; empty deletion. Return/report stale/corrupt sidecar diagnostics without destructive rewrite.
- Tests: missing/empty/sorted round-trip; first/mid/last; Unicode root; atomic replace/write failure leaves prior file intact; bad versions/types/bools/negative/duplicate indices; missing/reordered/stem-mismatched source; unrelated annotations/holdouts/hints/outputs untouched; same display names in different roots isolated.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_keyframes.py`.
- Acceptance: marks survive process restart and source externalization, never reinterpret stale marks as another source frame. Parallel with 4, 8/9 when dependencies permit.

### 6. Clip mark state and mode-aware readiness — SERIAL after 4 + 5
- Files: `backend/clip_state.py`, `tests/test_clip_state.py`; new `tests/test_v2_hint_readiness.py`.
- Changes: immutable `hint_keyframes` plus a keyframe-load diagnostic on ClipEntry; load/validate sidecar after input resolution in find_assets, preserving diagnostics without modifying disk. Extend `can_run_inference` and `classify_pipeline_route` with mode-aware optional arguments while old boolean calls keep their meaning. Keyframes requires input, AlphaHint, valid nonempty marks and non-EXTRACTING/ERROR state, not READY/dense count. Keyframe route is direct inference (never synthesize more hints). Full retains current routes. No fake READY/COMPLETE state or completion resolver change.
- Tests: RAW/MASKED partial AlphaHint can run Keyframes but not Full; missing/zero/stale marks rejected; None ignores marks/hints; black matte unaffected; COMPLETE outputs remain COMPLETE after scans; sidecar survives re-created ClipEntry and deepcopy; video source stem identity.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_hint_readiness.py tests/test_clip_state.py tests/test_hint_keyframes.py`.
- Acceptance: readiness and pipeline classification distinguish dense vs sparse without changing disk-derived state. Serial owner of clip_state.py.

### 7. Selected AlphaHint lookup and sparse staging — PARALLEL-SAFE after 6
- Files: `backend/service/v2_inputs.py`, `tests/test_v2_inputs.py`; new `tests/test_v2_sparse_inputs.py`.
- Changes: extend prepared result with `initial_hint_path`, `initial_hint_index`, `hint_frame_indices` (defaults preserve existing construction); mode-aware preparation with legacy boolean input compatibility. Factor existing exact-stem / verified legacy-ordinal selection so sparse resolution validates **selected** frames, not total hint count. Single mark stages one reference PNG; multiple marks stage only `<absolute_index:06d>.png`. Source preparation/order/FPS stays unchanged. Use unicode-safe I/O and grayscale/alpha extraction; uint16 PNG for float matte normalization, no RGB gamma. For AlphaHint video, decode selected absolute ordinal frames in the worker, release capture, validate selected dimensions/count. No placeholder files.
- Tests: natural-vs-lexical source order; original and generator ordinal names; nonzero single reference; two separated indices; arbitrary missing unmarked hints accepted; missing/ambiguous/duplicate selected matte gives index+stem+actionable error; selected dimension/decode errors; extra unselected hint files ignored only in Keyframes; all existing dense rejection cases retained. Unicode, uint16/RGBA/float matte, input TIFF/EXR staging, selected hint video, cancellation and partial-staging cleanup. In/Out does not rebase/drop marks.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_sparse_inputs.py tests/test_v2_inputs.py tests/test_no_raw_cv2_file_io.py`.
- Acceptance: no selected frame can slide to a different ordinal, and no unselected AlphaHint pixel is decoded/staged. None ignores invalid hints entirely.

### 8. Shared temporal estimate/profile helper — PARALLEL-SAFE after 4
- New files: `backend/v2_temporal.py`, `tests/test_v2_temporal.py`.
- Changes: fixed preset validation/mapping and estimate/context helper per D. Lazily import engine estimator and selector; no copied VRAM numbers. Pure shape/count/profile/budget calculations separated from optional CUDA free+reserved query. Expose peak, safety-adjusted requirement, over-budget flag and Auto recommendation for UI/adapter. Profile low uses existing forced-low selection policy.
- Tests: spy exact estimator/selector signature and calls; width/height padding to 128; 1080p/4K/short clips; manual preset exactness; normal vs low; 1/1 conservative floor; margin boundary; unavailable CUDA/budget/shape; auto/low recommendations; changing clip/profile changes estimates. No model load or large pixel decode.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_temporal.py`.
- Acceptance: UI and run warnings use one computation with engine calibration. Parallel with 5–7, 9, 13, 15 after dependencies.

### 9. Bounded raw-EXR colour sampler — PARALLEL-SAFE after 3 + Q1
- New files: `backend/input_colour.py`, `tests/test_input_colour.py`.
- Changes: implement agreed bounded source-frame sampling; scalar result with finite RGB peak, sampled frames, dimensions/frame count where available, or unavailable/error result. Unicode-safe untransformed reads, ignore alpha, one decoded array at a time; non-EXR fast path. This helper makes no interpretation changes and imports no Qt/runtime/model.
- Tests: EXR peaks 0.93, 0.99, exactly 1, >1, negatives, alpha>1 with RGB<=1, finite/nonfinite/decode failure; short clips deduplicate samples; no scan beyond agreed samples; no PNG/video pixel I/O; no source mutation. Values remain raw, not preview-clipped or gamma corrected.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_input_colour.py tests/test_no_raw_cv2_file_io.py`.
- Acceptance: reports heuristic evidence, never asserts an EXR transfer function from range alone.

### 10. Adapter mapping for none/full/reference/sparse — SERIAL after 4 + 7
- Files: `backend/corridorkey_v2.py`, `tests/test_v2_adapter.py`; new `tests/test_v2_adapter_hints.py`.
- Changes: adapter args for staged initial path/index and sparse indices; validate mode-specific input combinations, map indexed reference and sparse config, keep required nullable engine kwargs explicit. None ignores all supplied hint paths; Full uses only dense path; Keyframes rejects missing/empty/contradictory paths rather than falling back. Fresh cfg resets index/sparse fields every call. Update legacy hint-on error copy to mode terminology.
- Tests: inspect runtime cfg/kwargs for all four effective paths, nonzero k, sorted sparse absolute indices; exactly one only reference; disabled/unused assets ignored; missing paths/index mismatch/out-of-range preflight; default old bool full works. Reused model reference→none→full→sparse has no previous hint/index leakage; load count, cancellation and offline env unchanged.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_adapter_hints.py tests/test_v2_adapter.py`.
- Acceptance: official runtime remains the only engine; no adapter-level frame loop. Serial owner of adapter/tests until task 11.

### 11. Adapter manual windows and pre-run warnings — SERIAL after 8 + 9 + 10
- Files: `backend/corridorkey_v2.py`, `tests/test_v2_adapter.py`; new `tests/test_v2_adapter_warnings.py`.
- Changes: generalize low-only probe to bounded run metadata needed by manual windows and colour-path detection; run outside GUI. Apply D exact config precedence and helper estimates with fresh post-load CUDA budget. Add manual over-budget warning and display-coded-EXR warning before runtime execution via logger, and preserve in result warnings. Do not require sampling to warn that display-coded EXR clips HDR. Avoid duplicate probes where metadata already available, no full-clip sample. Keep low/auto tuning reset and warnings reset per run.
- Tests: all preset/profile combinations; Auto old normal/forced-low mapping unchanged; manual never re-autoselects even over budget; warning contains preset, estimate and budget; warning logged before mocked failing runtime; completed result warnings present; missing CUDA metadata graceful where appropriate. Display-coded EXR warns, Linear EXR and PNG/video do not; warning independent of peak <=1 and absent UI; repeated runs don't retain old warnings. Test normal 1/1 caveat and manual→Auto/low→Auto reuse.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_adapter.py tests/test_v2_adapter_hints.py tests/test_v2_adapter_warnings.py tests/test_v2_temporal.py`.
- Acceptance: warnings are visible even if inference OOMs; over-budget is never a ValueError/disabled run. No vendor change for EXR warning.

### 12. Service handoff, frozen provenance and resume — SERIAL after 6 + 7 + 11
- Files: `backend/service/inference.py`, `backend/service/frame_ops.py`, `ui/workers/job_helpers.py`, `tests/test_v2_service_inference.py`, `tests/test_v2_outputs.py`; new `tests/test_v2_hint_resume.py`.
- Changes: pass canonical mode and **snapshot** clip marks to preparation and prepared hint fields to adapter; remove generic dense prerequisite for Keyframes. Freeze params/output objects when creating inference jobs, retaining frozen clip marks and source mapping. Add effective absolute keyframe indices to manifest and resume compatibility in Keyframes mode; reference is one-element keyframe provenance. Keep mode/window in params provenance, copied runtime settings include k/window. Use no keyframe provenance in None/Full. Preserve whole-clip input and publication-only range/skip semantics.
- Tests: end-to-end fake service source→staging→adapter mapping for none/full/one/multiple marks; selected missing hint fails clearly; UI/sidecar/params mutation after submit cannot change job. A changed mark/mode/window invalidates output skipping; unchanged snapshot permits it; nohint ignores unrelated sidecar changes. Old manifests are conservatively republished when compatibility cannot be proven. In/Out clips with k outside publication still send full source/absolute k; resumed publication never compacts source. Warning callback receives result warnings; errors/cancel never mark complete; output pixels/formats unchanged.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_service_inference.py tests/test_v2_hint_resume.py tests/test_v2_outputs.py tests/test_v2_chain_state.py`.
- Acceptance: queue snapshots and manifests describe exactly the hints/window actually inferred. Tests retain current output contracts; do not copy obsolete output-colour statements from the first port plan.

### 13. Hint-mode and mark/unmark panel controls — PARALLEL-SAFE after 4
- Files: `ui/widgets/parameter_panel.py`, `tests/test_parameter_panel.py`; new `tests/test_parameter_panel_hint_modes.py`.
- Changes: replace `_use_alpha_hint` checkbox with canonical mode combo and preserve trust/carry controls. Add mark/unmark current frame action + signal, current mark state/count context setter and separate busy gate. Keyframes row shows count; disabled with explanatory tooltip if zero; unavailable restored mode remains visibly invalid rather than silently choosing Full/None. Hint quality enabled only for external mode; carry independent. Correct carry tooltip to overlapping-Hann auxiliary predictions; explain None+carry and 1/1 limitations. get/set/reset mode/window defaults round-trip without emitting on programmatic updates.
- Tests: None/Full/Keyframes states, 0/1/N counts, quality/carry enablement, disabled-item tooltip, explicit unavailable restored Keyframes, mark/unmark label and clicked signal, busy/no-clip rules, programmatic suppression, old bool full params, reset defaults. Update tests accessing removed private checkbox, no compatibility dummy widgets.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_parameter_panel.py tests/test_parameter_panel_hint_modes.py tests/test_output_config.py`.
- Acceptance: reference is clearly one keyframe; no independent reference selection or image picker. This file is reserved from task 14 until 13 lands.

### 14. Temporal combo, estimate label and colour-notice surface — SERIAL after 8 + 9 + 13
- Files: `ui/widgets/parameter_panel.py`; new `tests/test_parameter_panel_v2_feedback.py`.
- Changes: add Temporal window combo presets, per-option estimated GiB text, Auto recommendation, warning label/tooltips and context setter with no I/O/model probe in widget. Add a compact hidden-by-default non-modal EXR notice + one-click interpretation signal/API, close/clear when context gone. get/set/reset include temporal field; keep existing VRAM combo. Text and data use canonical preset IDs, not parsed translated labels. All strings via tr.
- Tests: all presets/Auto and auto/low round-trip; estimate recomputation inputs, selected label unchanged after relabeling; red safety-budget warning but no disabled run/action; unavailable states; short clip/floor caveat. Notice threshold text and action target; no-notice path; widget construction/get/set never decode EXR or query CUDA. Programmatic updates emit no parameter storm.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_parameter_panel_v2_feedback.py tests/test_parameter_panel_hint_modes.py tests/test_parameter_panel.py`.
- Acceptance: inference group exposes exactly the requested knobs; existing cutout-linear remains disabled/unchanged.

### 15. Non-interactive keyframe ticks on the existing scrubber — PARALLEL-SAFE after 3
- Files: `ui/widgets/frame_scrubber.py`, `ui/widgets/timeline_widgets.py`; new `tests/test_hint_keyframe_ticks.py`.
- Changes: `FrameScrubber.set_keyframes(indices)` and overlay storage/painting, using existing coordinate conventions and distinct tick styling/legend. Render ticks without requiring in/out markers; update on range/resize, clear when changing clips. Ticks do not acquire a hitbox or intercept input. Preserve in/out drawing/hit testing, coverage, clock and genuine slider gestures.
- Tests: actual paint/render or painter spy for 0/mid/last ticks, resize/short/empty range and clear; distinguish from annotation coverage and in/out handles. QTest press→held-button moves→release scrubs across tick positions with intermediate frame signals; in/out marker dragging still works. Do not use setValue as a gesture substitute.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_keyframe_ticks.py tests/test_timeline_drag.py tests/test_frame_scrubber.py`.
- Acceptance: visible marks align with source-frame navigation without breaking previous playback/gesture fixes. Parallel with non-scrubber tasks.

### 16. Background inference-context probe — PARALLEL-SAFE after 8 + 9
- New files: `ui/workers/inference_context_worker.py`, `tests/test_inference_context_worker.py`.
- Changes: small QObject/QRunnable worker/controller following existing thumbnail/FPS patterns for source metadata, raw EXR sample and CUDA budget snapshot; immutable root/source-revision/request-token context, bounded/coalesced pending work and retained signal lifetime. Worker uses 8/9 helpers, returns scalars only. Explicit queued GUI completion; no widgets/QPixmap/model load in worker, no decode/metadata probe in GUI constructor. Invalidation on selection/project/close; bounded results cache keyed by source revision if needed, no full-clip image cache.
- Tests: decoder/thread spies show all raw EXR reads and dimension/video probes off QApplication.thread(); slow old request superseded by new clip; source same path overwritten invalidates; closed controller safely ignores completion; no keyer load; failed sample/no CUDA produces unavailable feedback, not GUI exception. Deterministic events/fake pool instead of long sleeps.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_inference_context_worker.py tests/test_input_colour.py tests/test_v2_temporal.py`.
- Acceptance: selected 4K EXR may take time to sample, but the GUI heartbeat/slider remains responsive and worker arrays are released.

### 17. Keyframe UI controller/mixin — PARALLEL-SAFE after 5 + 6 + 7 + 13 + 15
- New files: `ui/main_window_mixins/hint_keyframe_mixin.py`, `tests/test_hint_keyframe_controller.py`.
- Changes: narrow controller methods to load/refresh selected clip keyframe context, mark/unmark current presented source frame, persist through helper, and send count/state/ticks via public widget APIs. Use selected AlphaHint filename/ordinal lookup without pixel decoding on GUI; dimensions/decode errors remain worker/service preflight. Stop playback, reuse `_hint_mutation_allowed`, recheck root/frame/mutation state before save, update in-memory tuple only after successful atomic save. Unmark allowed with missing hint; mark missing selected matte explains exact frame. Keep clip error separate from global params.
- Tests: actual scrub gesture to k then clicked mark/unmark, current presented frame vs future clock target, one mark=reference count, many marks, duplicate toggle, unavailable current matte, Unicode/different same-name clip roots, failed save no false tick, busy/pending/cancelling/chain blocks, idle persistent GPU worker allowed. No SAM annotations changed; no GUI pixel decode.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_keyframe_controller.py tests/test_hint_keyframe_ticks.py tests/test_hint_keyframes.py`.
- Acceptance: controller works with fake MainWindow dependencies and never uses masks/output alpha. No edits to MainWindow/clip mixin in this assignment.

### 18. Selected-view lifecycle and keyframe wiring — SERIAL after 17
- Files: `ui/main_window.py`, `ui/main_window_mixins/__init__.py`, `ui/main_window_mixins/clip_mixin.py`, `ui/widgets/dual_viewer.py`; new `tests/test_main_window_hint_keyframes.py`.
- Changes: export/inherit/wire mixin and mark signal; public DualViewer keyframe forwarding/current source index use. Hook `_sync_selected_clip_view`, selection, placeholder/project switch and same-clip asset reload so counts/ticks come from that clip, not session/global panel state. Wire current presented-frame signal to mark button state without stopping playback just for refresh. Recreated ClipEntry loads sidecar; do not load new keyframe indices from global session params. Reload must not save/delete marks incidentally.
- Tests: switch clips, rescan, session/project reopen, worker result reload and import refresh retain only the correct sidecar marks; no cross-clip contamination; source reorder diagnostic; INPUT override and annotation models untouched; ticks clear on placeholder; no extra decode per frame. Existing selection/session tests remain valid.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_main_window_hint_keyframes.py tests/test_hint_keyframe_controller.py tests/test_dual_viewer_live_refresh.py tests/test_main_window_color_space.py`.
- Acceptance: per-clip persistence works across restarts without extending global session schema for marks. MainWindow/clip/dual shared ownership serial.

### 19. Reset/replacement/clear lifecycle for marks — SERIAL after 18 + Q2
- Files: `backend/hint_assets.py`, `ui/main_window_mixins/hint_reset_mixin.py`, `ui/widgets/io_tray_actions.py`, `tests/test_hint_assets.py`, `tests/test_hint_reset.py`, `tests/test_hint_tray_actions.py`.
- Changes: explicit keyframe-sidecar deletion option in shared helper, counted by has_hint_assets; Reset uses it and resets in-memory marks/diagnostic before reload. Explain cleared keyframes in confirmation. Implement confirmed Q2 for tray Clear Alpha/Clear All versus ordinary alpha-only generator replacement; do not accidentally clear marks through a shared default used by every replacement. Refresh selected keyframe tools/ticks through central lifecycle and busy guard. Clearing masks/annotations alone never destroys AlphaHint marks. Preserve previous exact file deletion boundaries and errors.
- Tests: Reset default/clear-annotations both remove sidecar+memory/ticks; marks cannot resurrect after rescan/save/reopen; sidecar-only Reset works; Cancel/busy/changed selection untouched; sidecar deletion failure surfaced/no false success; completed output manifests/state retained, prompts/holdout preserved by default. Cover agreed replacement/import/tray policy and mask-only clears, including legacy alpha videos.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_hint_assets.py tests/test_hint_reset.py tests/test_hint_tray_actions.py tests/test_main_window_hint_keyframes.py tests/test_hint_rerun.py`.
- Acceptance: Reset actually removes the reference/keyframe selection, not just its mattes, with no keyed-output deletion.

### 20. Run/Resume/Run All and ordinary pipeline mode routing — SERIAL after 12 + 18 + 19
- Files: `ui/main_window_mixins/inference_mixin.py`, `ui/main_window_mixins/clip_mixin.py`, `tests/test_v2_run_actions.py`, `tests/test_v2_hint_readiness.py`.
- Changes: all readiness, single/multi-selection, route and button-state calls use canonical mode + each clip's marks. Dense incomplete-alpha dialog applies only to Full; Keyframes validates selected set and shows clear errors, never offers automatic dense regeneration as a requirement. None explicitly replaces mode to none. `_on_params_changed` refreshes run/resume state as well as colour. Run All/selected pipeline obtains each clip's frozen marks, not current viewer's. Keyframes always direct inference; generator branches explicitly freeze Full AlphaHint. Retain full-source range/resume explanation and queue semantics.
- Tests: None RAW, Full dense/partial, Keyframes RAW/MASKED partial, empty/stale/missing selected matte; mode change updates Run immediately. Run+Resume exact snapshots; single ref k and many marks preserved with In/Out. Run All two clips with distinct marks; multi-select direct Keyframes doesn't run GVM/SAM; Full generator chain forces full; switch to no-hint dialog sets canonical mode. Test no full-hint count warning in Keyframes.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_run_actions.py tests/test_v2_hint_readiness.py tests/test_main_window_hint_keyframes.py tests/test_main_window_preview_modes.py`.
- Acceptance: no old boolean gate silently prevents sparse use or launches an unwanted hint generator. Serial after 18 (shared clip mixin).

### 21. Explicit batch and auto-chain forcing stays Full AlphaHint — SERIAL after 20
- Files: `ui/main_window_mixins/batch_pipeline_mixin.py`, `ui/main_window_mixins/worker_mixin.py`, `tests/test_v2_batch_options.py`, `tests/test_v2_chain_state.py`.
- Changes: replace every boolean-only mode mutation with explicit canonical none/full. Explicit batch “existing AlphaHint” remains Full, explicit no-hint remains None; no new batch-dialog feature. Every generator→inference path including legacy worker fallback forces Full irrespective of panel Keyframes. Preserve frozen temporal/profile/trust/output/range/input-colour fields and existing extraction-delay snapshot behaviour. Use new clip mark refresh on terminal/queue idle through central APIs; no extra GPU thread.
- Tests: panel Keyframes followed by each existing generator/chain (GVM, SAM→VideoMaMa, MatAnyone2 and relevant BiRefNet/chroma routes) still sends Full. Existing/nohint batch configs override panel mode correctly, delayed extraction and UI edits do not alter frozen temporal settings; terminal refresh/cancel cannot resurrect/reset marks unexpectedly. Audit all `use_alpha_hint=`/`dataclasses.replace` callers in production, including CLI constructors retained by task 4 compatibility.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_batch_options.py tests/test_v2_chain_state.py tests/test_hint_button_state.py tests/test_v2_gpu_worker.py tests/test_v2_cli.py`.
- Acceptance: Q6 generator behaviour is unchanged; Keyframes is opt-in only for direct keying. No expansion of batch options.

### 22. Async colour/temporal feedback integration — SERIAL after 14 + 16 + 18 + 20 + 21
- New file: `ui/main_window_mixins/inference_feedback_mixin.py`; new `tests/test_main_window_v2_feedback.py`.
- Files: `ui/main_window.py`, `ui/main_window_mixins/__init__.py`, `ui/main_window_mixins/clip_mixin.py`, `ui/main_window_mixins/inference_mixin.py`, `ui/main_window_mixins/session_mixin.py` (only necessary lifecycle/persistence hooks).
- Changes: wire feedback controller/notice action, request context on selected source revision change, recompute labels from cached context on low/profile/window/interpretation edits without decoding again. Reject old root/token/revision completions. Reevaluate current interpretation rather than probe-start interpretation. One-click colour switch invokes normal params-changed path once, updates current clip memory, both viewers, thumbnails and session save; do not change another clip/defaults. Free-budget updates can refresh warning from existing probe/monitor flow, but use real free+reserved accounting, not total/reserved arithmetic. Clear notices/estimates on project/clip deletion/close and safely release pending worker signals. Thin hooks only; no unrelated screen-colour detection refactor.
- Tests: fake delayed worker confirms sampling thread; 0.93–0.99 EXR+Linear shows non-modal display-coded action; >1 EXR+display-coded shows Linear action; PNG/video no notice; failed/unknown samples unavailable. Click updates preview/thumbnail/input-colour memory and survives session reopen, doesn't double-emit. User edits during probe and clip switch cannot be overridden. Estimates change with clip resolution/count/low flag, over-budget is red yet Run remains enabled; run warning tested through adapter/service, not GUI stdout capture. Real gestures, not state-setting-only tests.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_main_window_v2_feedback.py tests/test_inference_context_worker.py tests/test_main_window_color_space.py tests/test_preview_viewport_color_space.py tests/test_thumbnail_worker.py tests/test_session.py tests/test_parameter_panel_v2_feedback.py`.
- Acceptance: no new GUI-thread EXR/dimension/video decode; warning is actionable without automatic guesses or modal interruption. Serial MainWindow/clip/inference writer after earlier integrations.

### 23. Cross-feature regression and Qt shutdown audit — SERIAL after all implementation
- Files: all new test modules; existing tests explicitly touched above plus `tests/test_main_window_preview_modes.py`, `tests/test_main_window_color_space.py`, `tests/test_preview_viewport_color_space.py`, `tests/test_hint_button_state.py` only for deliberate fixture/API changes.
- Changes: update removed-checkbox fixtures/dummy panels, not production getattr bandaids. Keep module-level `_APP = QApplication.instance() or QApplication([])` in every QWidget-using test. Patch QMessageBox **module references**, never Qt class attributes. No QTimer.singleShot without a live app. Retain/drain worker signal lifetimes before deterministic widget destruction; don't mask QThread/QRunnable errors.
- Tests: combine engine reference/sparse tests, params/session migration, adapter staging/provenance, mark/ticks gestures, Reset, input colour and temporal feedback; old playback/scrub/annotation/mode/range/resume/hint-chain tests. Repeat touched Qt groups together to catch ordering/exit failures. No model downloads for unit tests.
- Verify: `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests`; `git diff --check`; `git ls-files --eol -- CorridorKeyV2 backend ui tests scripts/v2_smoke.py`; inspect diff for EOL-only noise, duplicate memory tables and unrecorded vendor edits.
- Acceptance: full suite exits 0, no new unexplained skips, shutdown segfaults, GUI-thread decode regressions or raw cv2 I/O outside facade. Every new requirement has a deterministic regression test; do not accept passing counts with nonzero exit.

### 24. Real 4090 short-clip smoke and on-screen acceptance — SERIAL after 23
- Files: `scripts/v2_smoke.py`; new `tests/test_v2_smoke_cases.py` (CPU mocked case registration/config/path assertions only).
- Changes: extend existing service-driven smoke cases with `reference_mid`, `keyframes_sparse`, `manual_window`; accept no new inference implementation. Reuse disposable clip builder. Reference case uses only one AlphaHint PNG at k>0. Sparse case physically removes all unmarked AlphaHint frames and marks separated absolute indices spanning multiple windows, e.g. 3 and 17 of 24 frames. Manual case exercises 8/4 on the short clip and verifies settings/provenance; optionally 1/1 on a tiny case for estimator/overlap edge, not a new performance benchmark. New hint cases require --hint-src and report SKIPPED explicitly if missing; final acceptance requires they actually ran. Capture warnings, index/window settings, source stems, output coverage/finite matte range, peak allocated VRAM, cleanup, cancel and model reuse. Temporary monkeypatch/spies for CPU test only, not replacement GPU runtime.
- Verify help and CPU registration: `.venv/bin/python scripts/v2_smoke.py --help`; `QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q -p no:cacheprovider tests/test_v2_smoke_cases.py`.
- Real command (substitute actual non-destructive source paths): `.venv/bin/python scripts/v2_smoke.py --clip-src /path/to/short-input-sequence --hint-src /path/to/aligned-AlphaHint --frames 24 --work /tmp/ezck2-v2-hint-modes-smoke --cases nohint_auto,nohint_low,hint_auto,reference_mid,keyframes_sparse,manual_window,range,cancel`.
- On-screen checklist: real current-frame scrub→mark/unmark; one reference at middle/last; multiple visible ticks; close/reopen persists; no-mark Keyframes disabled with tooltip; Full remains usable after generator; Reset removes marks but not outputs/prompts; colour notice on supplied 4K EXRs and one-click persisted switch; Low/profile/clip resolution relabel every temporal option; manual over-budget warning visible and Run enabled. Do not actually launch an obviously over-budget 4K window just to prove non-blocking UI—mock the budget in unit tests. Check heartbeat while EXR sampling, no playback/annotation regression.
- Acceptance: all required real cases PASS on RTX 4090 24 GiB with recorded settings/peak/exit code, real outputs exist for all expected source stems, one heavy model reused, scratch empty after completion/cancel. Visual continuity/subject choice is observed, not promised by file-count tests; record drift/quality limitations honestly. Run full suite again after final smoke-script/test changes. Orchestrator records measured evidence in its final report, not guessed numbers in this plan.

## Files to Modify

- `CorridorKeyV2/corridorkey/runtime.py` — defaulted reference index/sparse config, injection/presence mapping, index settings line.
- `CorridorKeyV2/corridorkey/video_io.py` — explicit sparse directory reader and presence API, dense compatibility.
- `CorridorKeyV2/VENDORED.md` — narrow local patch ledger and tests.
- `backend/service/core.py` — compatible hint-mode/temporal parameter schema.
- `backend/clip_state.py` — immutable clip marks/diagnostic, loading and mode-aware readiness/routes.
- `backend/service/v2_inputs.py` — selected AlphaHint resolution, one-reference/sparse PNG preparation.
- `backend/corridorkey_v2.py` — staged hint mapping, exact manual windows, shared estimates and pre-run warnings.
- `backend/service/inference.py`, `backend/service/frame_ops.py` — service handoff and mark/window-aware manifest/resume.
- `backend/hint_assets.py` — explicit sidecar reset/clear support.
- `ui/workers/job_helpers.py` — fully frozen params/output/clip snapshots.
- `ui/widgets/parameter_panel.py` — mode/mark controls, temporal estimates, non-modal colour notice.
- `ui/widgets/frame_scrubber.py`, `ui/widgets/timeline_widgets.py` — non-interactive keyframe ticks.
- `ui/widgets/dual_viewer.py` — small public mark-forwarding/context API.
- `ui/main_window.py`, `ui/main_window_mixins/__init__.py` — mixin/signal/lifetime wiring.
- `ui/main_window_mixins/clip_mixin.py`, `inference_mixin.py` — selected lifecycle, mode-aware actions, feedback hooks.
- `ui/main_window_mixins/session_mixin.py` — retain existing per-clip colour memory in one-click feedback/session path only if hooks are needed; marks stay in clip sidecars.
- `ui/main_window_mixins/hint_reset_mixin.py`, `ui/widgets/io_tray_actions.py` — confirmed reset/destructive-clear policy.
- `ui/main_window_mixins/batch_pipeline_mixin.py`, `worker_mixin.py` — explicit generator/full/nohint forcing and lifecycle refresh.
- `scripts/v2_smoke.py` — additional cases through the existing service.
- Existing test files named in each assignment — compatible fixtures and focused regression coverage, no unrelated test rewrite.

Read-only/reuse: reference checkout/README, prior plans, `backend/frame_io.py`, `ui/preview/frame_index.py`, thumbnail/FPS worker patterns, existing GPU queue/mutation guards, generator implementations, output encoding/publication and playback/cache machinery.

## New Files

- `backend/hint_keyframes.py` — per-clip validated atomic keyframe sidecar.
- `backend/v2_temporal.py` — shared thin engine-estimator/preset/budget helper, no second memory table.
- `backend/input_colour.py` — bounded raw-EXR sampling evidence.
- `ui/workers/inference_context_worker.py` — off-thread context acquisition with stale-result protection.
- `ui/main_window_mixins/hint_keyframe_mixin.py` — current-frame mark/persist/refresh controller.
- `ui/main_window_mixins/inference_feedback_mixin.py` — colour action and temporal-estimate integration.
- `tests/test_v2_reference_hint.py`, `test_v2_sparse_hint_reader.py`, `test_v2_sparse_hann.py` — CPU fake-loop vendor regressions.
- `tests/test_hint_keyframes.py`, `test_v2_hint_readiness.py`, `test_v2_sparse_inputs.py`, `test_v2_hint_resume.py` — persistence, sparse staging/routing and frozen provenance.
- `tests/test_v2_temporal.py`, `test_input_colour.py`, `test_v2_adapter_hints.py`, `test_v2_adapter_warnings.py` — estimator/colour/adapter contracts.
- `tests/test_parameter_panel_hint_modes.py`, `test_parameter_panel_v2_feedback.py`, `test_hint_keyframe_ticks.py` — widget states/estimates/gesture coverage.
- `tests/test_inference_context_worker.py`, `test_hint_keyframe_controller.py`, `test_main_window_hint_keyframes.py`, `test_main_window_v2_feedback.py` — thread, lifecycle, persistence and one-click integration tests.
- `tests/test_v2_smoke_cases.py` — CPU coverage for smoke-case registration and configuration.

Runtime data, **not checked-in assets**: `<each clip>/.corridorkey_hint_keyframes.json`. Staged selected PNGs live only in owned inference scratch and are removed on success/error/cancel.

## Dependencies and parallel ownership

- Engine first: `0 → 1 → 2 → 3`. All three share the patch ledger; no parallel vendor writers.
- After 3: 4, 5, 9 (Q1), 15 can run independently. `4 → 8,13`; `4+5 → 6 → 7`; `8+9 → 16`.
- Adapter/service: `4+7 → 10`; `8+9+10 → 11`; `6+7+11 → 12`.
- Widgets: `13+8+9 → 14` (same parameter panel ownership); 15 is disjoint from these.
- Marks: `5+6+7+13+15 → 17 → 18 → 19 (Q2)`.
- Main actions/chains: `12+18+19 → 20 → 21`.
- Feedback: `14+16+18+20+21 → 22`.
- Integration `all implementation → 23 → 24`; final full suite after 24.
- Never parallelize: tasks 1/3 (`runtime.py`), 1/2/3 (`VENDORED.md`); 10/11 (`corridorkey_v2.py`/adapter tests); 13/14 (parameter panel); 18/20/22 (MainWindow/clip/inference lifecycle); 18/19/22 (selected mark lifecycle). Tasks modifying shared existing tests wait for their previous owner.
- Session tests belong to 4 until it lands; later session/colour integration waits. MainWindow feedback integration is deliberately last to avoid racing small mark/run/chain workers.

## Risks

- Reference at k is window-local conditioning, not guaranteed causal tracking; k=last cannot control already-flushed earlier windows. With 1/1 there is no overlap, so carry cannot bridge unhinted frames. Tooltips/run expectations must be honest.
- Sparse keyframes must override carry **only at explicitly marked sparse slots**. Changing dense carry order would silently alter existing Full AlphaHint results; test dense equivalence.
- Natural source sorting, generator ordinal names and sparse numeric staging are different namespaces. Absolute source indices and saved stem identity must survive staging, In/Out, Resume and re-created clip objects without sliding.
- Canonical mode plus retained boolean compatibility is a migration trap for `dataclasses.replace`; audit all production callers and tests. Explicit generator forcing must survive a panel left on Keyframes.
- Sidecars are small, but stale/corrupt selections must not silently select a different subject or partial keyframe set. Reset/replacement/import behaviour must follow confirmed Q2, including sidecar-only resets and deletion errors.
- EXR peak <=1 is not evidence strong enough for automatic colour interpretation. Sampling can miss HDR elsewhere; warnings must say sampled/possible, and raw RGB must not be clipped first. Preserve manual/session decisions.
- EXR decode/dimension probing on GUI would repeat an existing responsiveness pitfall. New probing must be off-thread; existing unrelated screen-colour detection is not a licence for another synchronous read.
- VRAM estimates are empirical, use padded shape/short-clip count and a safety margin, and can be stale before a run. Normal 1/1 uses a conservative table floor. Warnings are advisory; actual OOM remains possible and must preserve existing cancellation/cleanup.
- Real 4090 smoke must use disposable work directories and aligned hint data. Existing sources/outputs are never reset/deleted as a test side effect. A skipped new smoke case is not acceptance.
- CRLF/mixed line endings and Qt teardown can produce deceptively large diffs or a passing summary followed by failure; verify both EOL preservation and process exit status.

## Orchestrator resolutions (binding)

1. **EXR sampling (Q1):** accepted as proposed. Sample the first, middle and last source frames (deduplicated) and read the raw RGB. Take the maximum over the finite colour samples, decoding one image at a time in the worker. Do no whole-clip scan and never change the colour space automatically; the notice only suggests a switch.
2. **Marks lifecycle (Q2):** accepted as proposed. Regenerating, replacing or importing an AlphaHint keeps the marked positions while the source index and stem identity are unchanged. Reset Hint, tray Clear Alpha and tray Clear All clear the marks. Marks are never deleted silently when a generator completes normally.
3. **Temporal influence:** the interpretation is accepted. "Frames before k" means those frames get no explicit hint slots; the model may still be influenced within the window that contains k. No reverse pass and no second pass.

## Review amendments (binding, from plan review)

1. **Task 2 — one meaning for matte channels.** The sparse keyframe reader loads every file through `read_hint_image`, the same semantics as the reference hint: uint8/uint16 are normalised, RGBA uses the alpha channel, values are clamped to [0,1]. It must not use the BGR→gray path of `preprocess_hint_frame`.
   - Task 2 adds a test where a staged RGBA PNG with different alpha and RGB conditions on alpha.
   - Task 7 still stages single-channel PNGs.
2. **Task 1 — log messages.** Replace the "loaded initial frame-1 hint" log in `load_initial_hint_frame` with an index-aware message, alongside the settings-log index line.
3. **Contract B — when k is validated.** Validate k right after `T_all` is known and before any writer or memmap is created. Leftover empty `output_dir`/`temp_dir` folders after a rejected k are acceptable.
4. **Tasks 17/18 — which frame is "current".** Take the frame index from the viewer that is actually presenting. In INPUT-only view that is the input viewer, not `DualViewerPanel.current_stem_index`, which returns the output viewer.
   - The alternative is to prove that the input and output indices stay in sync in every view mode, with a test.
5. **Task 5 — sidecar writes.** Write the temp file in the same directory as the sidecar (the clip root), then `os.replace` it.
