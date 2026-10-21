# Implementation Plan

## Goal
Replace EZ-CorridorKey's v1 keyer with a vendored, CUDA-only CorridorKey 2.0 whole-clip engine, preserving optional AlphaHint generators and EZ output/viewer conventions.

## Scope and status
- Target `/home/npittas/EZ-CorridorKey2`, branch `corridorkey-v2`, upstream base `35b6b75` (EZ v2.1.5). Engine source `/home/npittas/CorridorKey2.0`, commit `485f5e9`.
- Source-inspected planning only: no implementation, installation, downloads or GPU tests performed. Only this plan file was written, incrementally.
- Do not modify `/home/npittas/EZ-CorridorKey`, its environment/projects/settings, or require the engine source checkout at runtime.
- Fixed decisions: replace v1, no selectable v1/MLX/MPS/CPU keyer; official `load_model` once per residency and whole-clip `run_inference`; intermediate EXR; publish EZ FG/Matte/Comp/Processed; optional hints; one heavy model at a time; model assets in app data. These are not open questions.
- Preserve GVM, VideoMaMa, SAM2, MatAnyone2, BiRefNet and chroma hints. CPU frame I/O/offload and optional hint implementations are not forbidden CPU **keying**. Apple Vision is not a required NVIDIA workflow.

## Verified contracts and implementation boundaries

### Vendor, imports and paths
Proposed tree:

```text
CorridorKeyV2/
  __init__.py                    # new empty namespace boundary
  corridorkey/                  # complete upstream engine package
  third_party/MoGe/             # complete required code and licenses
  scripts/download_weights.py   # upstream helper, unchanged
  LICENSE                      # full Corridor Key Licence v2.0
  THIRD_PARTY_NOTICES.md
  VENDORED.md                   # source/revision and explicit patch list
```

Import `CorridorKeyV2.corridorkey.runtime`; do not install another distribution named corridorkey (EZ's distribution already uses that name) or add arbitrary source roots to sys.path. Runtime adds `package_root/third_party/MoGe` for `moge` imports itself. Package/wheel must include MoGe resources and licenses, not model assets.

**RuntimePaths explicitly supports external asset paths.** Its dataclass fields are `package_root`, `weights`, `foundation_stats`, `rvm_ckpt`, `moge_checkpoint`, `cradio_dir`. Construct directly. `from_package()` only overrides the main weights, not the other fields; do not use it for this layout. `package_root` points to vendored code, all five asset fields to app data. **No vendor path patch needed.**

Required assets relative to the chosen v2 model-data root:

| Path | Source |
|---|---|
| `weights/corridorkey_v2.safetensors` | HF `nikopueringer/CorridorKey2.0` |
| `weights/foundation_stats.safetensors` | same |
| `vfms/rvm/pretrain_rvm_small16_256_204031069.npz` | representations4d GCS; upstream MD5 `ZJb6kj8+tasiJbVeplXu7Q==` |
| `vfms/moge/model.pt` | `Ruicheng/moge-2-vitb-normal` revision `ca5f0e07ff01d3e5a364c1d954ed12ee1814b368` |
| `vfms/cradio/C-RADIOv4-SO400M/` | NVIDIA complete snapshot revision `c0457f5dc26ca145f954cd4fc5bb6114e5705ad8`; ignore duplicate `*.pth.tar` |

C-RADIO needs code (`hf_model.py` etc.), config and safetensors, not just weights. Runtime validate() only checks existence: app readiness must reject obviously incomplete snapshots. Main CorridorKey HF revision is not pinned by upstream downloader; record resolved revision or retain that uncertainty, never invent a SHA.

**Scout correction:** `backend.project.get_data_dir()` returns checkout root in development, not always ~/.local/share/EZ-CorridorKey. Introduce a v2-specific model-data helper shared by setup/runtime/wizard; do not relocate existing projects/hint assets. Exact new identity/root needs Q1.

`prepare_runtime_environment()` globally setdefaults HF_HUB_OFFLINE/TRANSFORMERS_OFFLINE. Snapshot/restore both around v2 calls, preserving user-provided values; otherwise later optional-model downloads can fail. No vendor environment patch is necessary.

### API/config bridge
Actual API:

```python
load_model(paths, device="cuda")
run_inference(model, input_path, output_dir, cfg, writer_cfg,
              *, initial_hint_path, hint_video_path)  # returns None
```

Hint kwargs are required despite being nullable; pass explicit None. `hint_video_path` also accepts a dense hint directory. `load_model` happens once until model switch/unload, not per clip. Preflight CUDA/assets before allocating model.

Proposed v2 `InferenceParams` contract (default hint mode confirmed in Q6):

| App field | Mapping/default |
|---|---|
| `input_is_linear=False` | cfg.frame_dir_linear, directories only; runtime video decode is display-coded |
| `use_alpha_hint=False` | off: both hint paths None, hint_quality=-1; on: validated dense AlphaHint to hint_video_path, initial_hint_path=None |
| `hint_quality=0.95` | external trust [0,1], only active when hints enabled |
| `carry_hint=True` | internal temporal carry, independent of external hint switch |
| `carry_hint_quality=0.95` | cfg carry quality [0,1] |
| `vram_profile="auto"` | auto or low, see below |
| `despill_strength=0.5` | cfg despill [0,1] |
| `cutout_linear=False` | carried to cfg; no EXR effect, see Q4 |

Retain OutputConfig enabled flags/formats and exr_compression. Filter retired params when reading old sessions (`from_dict`); do not dynamically restore them. Supported fields must survive preferences and frozen job snapshots.

Internal cfg: output_format="exr", exr_dtype="float", model_scale=1.0, num_frames=-1, start_mode="begin", stage2_batch=1; unique job temp dir. Directory FPS comes from clip/extraction metadata when available, else upstream 24.0. WriterConfig receives EZ's selected FFmpeg path; validate required executable/video capability before processing. No advanced tuning panel beyond user-requested settings.

Auto means cfg.auto_vram=True. **Low-VRAM=True alone is insufficient**: default Hann=80 can exceed 24GB. Forced-low mode must select largest budget-fit low-VRAM candidate using upstream peak estimates/free+reserved accounting and padded shape/count, set chunk/stride, auto_vram=False, low_vram=True. Do not let auto switch forced-low back to normal. Adapter may probe metadata; no extra vendor patch required. Rebuild config every clip because auto mutates it. Snapshot/reset low-VRAM module tuning and test low→auto reuse versus fresh model.

### Exact output naming and color mapping
`raw_clip_name()` uses directory basename, except Input/Frames/rgb use parent basename. Staged input changes that name; derive from actual runtime input, never editable EZ display name. Runtime returns None; output paths are deterministic:

| Choice | Runtime directory / file | Data | EZ target |
|---|---|---|---|
| alpha | `<name>_alpha_exr/alpha_%06d.exr` | scalar [0,1] | `Matte/<original_stem>.<fmt>` |
| fg | `<name>_fg_exr/fg_%06d.exr` | straight scene-linear BGR, **not despilled** | `FG/<original_stem>.<fmt>` |
| checker | `<name>_checker_comp_despillNNN_exr/checker_comp_despillNNN_%06d.exr` | already composited/despilled scene-linear BGR | `Comp/<original_stem>.<fmt>` |
| cutout | `<name>_cutout_exr/cutout_%06d.exr` | despilled **straight** scene-linear BGRA, NOT premultiplied | `Processed/<original_stem>.<fmt>` |

Zero-based six digits; NNN is `int(despill_strength*100):03d`. Runtime also writes `<name>_settings.txt`; preserve settings/provenance under an engine-v2 name in EZ output root, not runtime image subfolders there. Map enabled outputs fg→fg, matte→alpha, comp→checker, processed→cutout; request only enabled products, reject empty selection. Cutout contains alpha, so do not force separate alpha unnecessarily.

- Runtime EXR colors are unclamped decoded scene-linear, including HDR/negative values. Use float intermediates (upstream half clips ±65504). Existing EZ final EXR writer writes half: preserve its established format, test ordinary HDR/negative values, document finite half limits and reject/nonfinite output rather than promising full-float final range.
- FG/Comp EXR retain BGR float, no transfer conversion. Matte scalar unchanged. Processed EXR multiplies straight cutout RGB by A **once** to retain EZ's premultiplied-linear contract.
- PNG FG/Comp use linear→sRGB once then clamp/quantize. Processed PNG is straight sRGB BGRA; encode RGB, never alpha, no premultiply. Avoid divide-by-zero at alpha=0.
- **cutout_linear has no effect in runtime EXR path**; only video path uses it. Do not claim a functioning EXR toggle (Q4).
- Runtime EXR writer fixes ZIP compression: decode/rewrite using EZ `_write_image(... exr_compression=...)` for DWAB/PIZ/ZIP/none. Do not use legacy `_write_outputs()` on linear runtime EXRs: it expects sRGB and would double-decode fg/comp.
- Preserve original sequence stems and current video `%05d` fallback; duplicate stems are preflight errors. Raw FG changes v1 tooltip semantics: label honestly rather than invent a new despill/postprocess.

### Input/hint ordering, ranges and resume
Upstream frame_paths only globs lowercase EXR/PNG/JPG/JPEG, lexical sort. EZ also supports TIFF/BMP/uppercase and its own ordered list. Build explicit index→source filename/stem map from ClipAsset.get_frame_files(). Direct directory fast path only when runtime list/order exactly matches. Otherwise stage numbered frames: symlink/copy supported formats, lossless normalized PNG/float EXR conversion for unsupported formats as appropriate. Preserve HDR/linear and integer ranges; stream preparation, not whole-clip RAM lists. Video can pass directly; keep GUI extraction policy unless explicitly changed.

Dense hints pair **ordinally**, not by EZ stem. Runtime resizes dense hints; app must validate dimensions/count/alignment instead of silently changing them. Recognize original stems and documented generator ordinal/suffix patterns (`_alphaHint_00000`, legacy names) with unambiguous provenance; do not reject all existing GVM outputs for lacking exact stems. Missing/ambiguous/extra hints produce actionable errors. Hint-off ignores hints/annotations/masks even when present. Hint-on with partial/range-only hints must offer regeneration or disabling hint, not silent hint-free fallback or ordinal sliding.

Runtime offers prefix num_frames/start_mode, not arbitrary range or skip_stems. Recommended Q5: infer full clip (retain temporal context), restrict EZ **publication** to in/out, skip valid completed outputs on resume. Communicate no GPU-work savings; never remove skipped frames from temporal input. Validate engine/version/params/output format/source mapping before resume; v1 outputs are not valid v2 resume data. Manifest must reflect actual required stems/extensions, partial range and completion, not count arbitrary files. Per-stem atomic output writes preserve useful partial publication.

### Progress/cancel/cleanup/residency
- Runtime uses print, **not tqdm**: RVM/VFM headings and Hann `window i/n`. No public progress/cancel callback. Internal on_baton_frames is the model's streaming render hook, not a UI seam. Avoid stdout parsing/redirection and monkeypatching internals.
- Recommended Q2 vendor patch: optional keyword-only `on_progress(phase,current,total)` and `check_cancel()` with defaults None in runtime and prepass/Hann/stage2 loops. Adapter-supplied cancel function raises existing JobCancelledError; vendor never imports EZ. Check before/after bounded CPU/GPU work, before final success. Cannot safely interrupt model load, active CUDA kernels or a decoder already blocked; report “Stopping after current GPU step”.
- Show honest phase counters/status/busy loading, not Hann windows as completed output frames or invented FPS/ETA. Existing worker progress callback calls _save_preview on every N progress ticks: gate that to publication after files exist, not prepass/stage counters.
- **Scout correction:** run_inference finally removes four memmap files, not cfg.temp_dir. Adapter owns unique TemporaryDirectory with separate memmaps/intermediates/input staging; close readers/writers then remove everything on success/error/cancel. Never shared /tmp/corridorkey_v2, source/AlphaHint/output root as scratch.
- Finally's writer.release can raise and prevent subsequent cleanup. ExrSequenceWriter bounded queue can deadlock if writer thread dies with full queue while producer/release calls put. Narrow writer error/shutdown fix and independent finally cleanup are required for trustworthy cancellation/errors; test imwrite failure/disk full.
- Replace _engine_pool with one _v2_engine and serial GPU jobs. V2's frozen RVM/MoGe/C-RADIO are part of that same engine. _ensure_model remains authoritative: unload/offload/dereference old heavyweight before new one; load once and reuse across consecutive clips.
- Lock whole inference operation and optional GPU hint compute ownership, not merely model loading. Current hint methods release lock after _get_* then compute; direct API race can unload in-use models. Remove obsolete pool/gate protocol to avoid inference waiting on itself during switch. No extra pool, GPU QThread.terminate or unsafe thread kill.

### Dependencies and licenses
- EZ torch==2.9.1 / torchvision==0.24.1 / timm==1.0.24 are a compatible starting point. NVIDIA installation uses cu128; validate actual torch CUDA/device, not just nvidia-smi. Python 3.11 target (SciPy>=1.16 requires >=3.11), align advertised metadata accordingly.
- Merge into pyproject and root requirements: numpy>=1.26,<2; transformers>=4.57,<5; safetensors>=0.6; huggingface-hub>=0.36; einops>=0.8; scipy>=1.16; Pillow>=11; tqdm>=4.67; `utils3d @ git+https://github.com/EasternJournalist/utils3d.git@83bfa5bda008803f6521ce9547f94023b8afeddd`.
- Keep **one** cv2 provider: opencv-python-headless>=4.11,<5 with existing OpenEXR>=3.4. VideoMaMa's own requirements currently says opencv-python while GVM uses headless; align module requirements so optional install does not replace cv2 files.
- Preserve optional generator dependencies (diffusers/peft/accelerate/av/kornia etc.). GVM uses diffusers internal UNet/LoRA/PeftAdapterMixin; VideoMaMa imports CLIPVisionModelWithProjection and StableVideoDiffusion internals; BiRefNet uses AutoModelForImageSegmentation; SAM2 extra git pin `2b90b9f5ceec907a1c18123530e92e794ad901a4`; MatAnyone2 torch/OpenCV wrapper. Static inspection cannot prove diffusers/peft exact version compatibility: resolve in fresh target venv, smoke-import/load available generators, record solved versions. No speculative new pins.
- Engine LICENSE is **Corridor Key Licence v2.0**, CC BY-NC-SA 4.0 **plus additional terms**: repackaging/redistribution restrictions, freely accessible derivative/attribution/same-license duties, paid API/commercial integration restrictions, large-organization seat requirements. Preserve complete text and THIRD_PARTY_NOTICES, MoGe MIT/DINOv2 Apache text and foundation publisher licenses. Do not relabel plain CC. Distribution suitability requires Q7; no legal or remote-code audit claimed.

## Tasks
Each numbered item is one worker assignment, with exact file ownership and acceptance. PARALLEL-SAFE means disjoint files after dependencies land; SERIAL means integration/shared-file work. Resolve applicable Q before conditional behavior. Do not concurrently edit the same tests/service/UI file.

1. **Vendor fixed source/provenance — PARALLEL-SAFE**
   - Files: new CorridorKeyV2 tree above.
   - Changes: copy corridorkey/, third_party/MoGe, scripts/download_weights.py, LICENSE and THIRD_PARTY_NOTICES.md from 485f5e9; add namespace __init__.py and VENDORED.md (source URL/commit/patch ledger). No weights, caches, git internals or source-checkout symlinks.
   - Acceptance: initial tree diff equals upstream except declared additions; imports work with source checkout absent; legal files included, no assets in git.

2. **Add model-data path contract — PARALLEL-SAFE, Q1**
   - Files: new `backend/v2_paths.py`, `tests/test_v2_paths.py`.
   - Symbols: get_v2_package_root(), get_v2_models_dir(), get_v2_asset_paths(), validate_v2_assets().
   - Changes: stdlib-only path/readiness helpers for checkout/installed code and agreed writable app-data assets; validate all files and C-RADIO contents. Leave backend/project.py:get_data_dir/project locations untouched.
   - Acceptance: HOME/XDG/frozen-root tests; no CUDA/Qt/model load needed; incomplete snapshot rejected; no writes to old app/source checkout.

3. **Merge dependencies/build inclusion — PARALLEL-SAFE after 1**
   - Files: `pyproject.toml`, `requirements.txt`, `gvm_core/requirements.txt`, `VideoMaMaInferenceModule/requirements.txt`.
   - Symbols: dependencies, requires-python, Hatch wheel packages/resources; delete optional-dependencies.mlx and uv.sources.corridorkey-mlx.
   - Changes: bounds above, Python>=3.11, preserve torch/vision/timm/hint extras, single headless cv2; include CorridorKeyV2/MoGe/scripts/notices but no assets.
   - Acceptance: fresh 3.11 editable resolution, pip/uv check, wheel contents verified; optional imports recorded, not assumed working.

4. **Replace keyer downloads without removing hint downloads — PARALLEL-SAFE after 1–2**
   - Files: `scripts/setup_models.py`, new `tests/test_v2_setup_models.py`.
   - Symbols: MODELS['corridorkey'], download_corridorkey(), is_installed(), download_model(), check_all(), main(); remove MLX_CHECKPOINTS/_MLX_RELEASE_BASE, is_mlx_installed(), _download_mlx_one(), download_corridorkey_mlx(), download_corridorkey_blue(), blue/MLX flags/status.
   - Changes: --corridorkey means complete v2 asset set, using unchanged vendor downloader functions with explicit app root and pins/checksum. --all includes it once. Preserve optional model downloaders/proxy guard; report ~5GB estimate/disk headroom/errors and resumable partial state. No automatic 37GB VideoMaMa for core setup.
   - Acceptance: mocked targets/revisions/checksum/incomplete cases, --check/--help/--all, nonzero failures, repeated setup idempotent; no v1 URL/local source paths.

5. **Add runtime structured hooks/cleanup — SERIAL after 1, Q2**
   - Files: `CorridorKeyV2/corridorkey/runtime.py`, patch ledger VENDORED.md, new `tests/test_v2_runtime_hooks.py`.
   - Symbols: run_inference(), precompute_rvm_to_memmap(), precompute_foundations_to_memmap(), hann_stage1_stream(), nested write_prediction_batches().
   - Changes: optional on_progress/check_cancel kwargs; propagate phases RVM/VFM/stage1/stage2; checks before/after bounded work and success. Finally closes every writer/reader/memmap/prefetch resource even when release/callback raises; retain original error. No algorithm/math changes or EZ imports.
   - Acceptance: callbacks/default-no-callback tests with fake models; cancellation in every phase unwinds; cleanup failures do not mask primary error or skip other resources.

6. **Fix EXR writer failed-thread shutdown — PARALLEL-SAFE after 1**
   - Files: `CorridorKeyV2/corridorkey/video_io.py`, new `tests/test_v2_exr_writer.py`; ledger edit serialized with 5.
   - Symbols: ExrSequenceWriter._worker(), write_many(), release(), constructor state only if necessary.
   - Changes: error-aware bounded enqueue/shutdown, idempotent release; background failure cannot block producer on put/join. Preserve naming/channels/float/ZIP. No ThreadedVideoWriter edits unless separately justified.
   - Acceptance: imwrite first/later failure with saturated queue, cancel/repeated release finish within bounded test timeout; success pixels unchanged, errors propagate.

7. **Define v2 params/service state — PARALLEL-SAFE**
   - File: `backend/service/core.py`; param/init/device portions of `tests/test_service.py`.
   - Symbols: InferenceParams, OutputConfig validation, CorridorKeyService.__init__(), detect_device(); delete _D import, VALID_MODEL_RESOLUTIONS/model_resolution, set_model_resolution(), set_inference_backend(), set_pool_size(), _begin_inference(), _end_inference() and old pool/gates in coordination with 9.
   - Changes: new parameter table, filtering legacy serialized keys, _v2_engine=None, CUDA-only keying preflight. Keep non-GPU app scanning/setup possible without eager model import; CUDA absence must show diagnostic, not CPU keying or immediate GUI crash.
   - Acceptance: roundtrip/defaults/invalid params/profile/output selection, legacy keys ignored; no CPU/MPS keying fallback. Temporary gate callers removed in same integration as 9.

8. **Implement lazy whole-clip adapter — PARALLEL-SAFE after 1–3,5–7**
   - Files: new `backend/corridorkey_v2.py`, `tests/test_v2_adapter.py`.
   - Symbols: CorridorKeyV2Engine.__init__(), load(), run_clip(), unload(), build_inference_config(); scoped offline-env helper.
   - Changes: explicit RuntimePaths, lazy vendor imports, CUDA/assets checks, official load_model once, fresh cfg/auto-or-forced-low policy; call official run_inference with supplied normalized source/hints/temp/products/WriterConfig/hooks; return deterministic output metadata, not fake frame dicts. Reset profile tuning between clips; restore HF env, clean load failures/model refs.
   - Acceptance: two run_clip calls one load; unload→new residency reload; exact code/data paths; None/-1 hint-off and dense-hint-on; alternating low/auto/config-mutation tests; no process_frame facade.

9. **Replace service frame dispatch/model lifecycle — SERIAL after 7–8; integrate 10–11**
   - Files: `backend/service/model_manager.py`, `backend/service/inference.py`; delete `inference_parallel.py`; related `tests/test_service.py`, `test_service_concurrency.py`.
   - Symbols: _get_engine_pool→_get_v2_engine; _ensure_model(), unload_engines(), is_engine_loaded(); InferenceMixin inheritance/run_inference/_finalize_inference; remove _run_inference_sequential, _resolve_screen_color and old reprocess_single_frame implementation (Q3).
   - Changes: whole-clip GPU lock; input preparation 10, one adapter call, publication 11, FrameResult summaries, proper error/cancel/finally; unload old heavy model before new; no pool/screen checkpoint switch/optimization-mode or v1 dynamo reset machinery. Remove self-wait gate protocol, keep _ensure_model authoritative. Preserve public run_inference args for callers with Q5 semantics.
   - Acceptance: no AlphaHint required when off; model reuse and hint↔v2 residency tests, no unload during active call; error/cancel cannot mark complete; no misleading stage-as-frame results.

10. **Normalize ordered sources/dense hints — PARALLEL-SAFE after 2,7**
    - Files: new `backend/service/v2_inputs.py`, `tests/test_v2_inputs.py`.
    - Symbols: prepare_v2_inputs(), validate_dense_hint_alignment(), owned staging context/metadata result.
    - Changes: direct fast path only exact runtime/EZ list match; numbered lossless staging for other formats/order; preserve integer/HDR/linear/color interpretation, map original stems, metadata FPS/shape/count. Validate dense hint count/dimensions/alignment and known ordinal naming; ignore hints entirely when off, reject ambiguous/range-only hints. Stream staging; cleanup partial failures.
    - Acceptance: natural/lexical mismatch, duplicate stems, TIFF/BMP/.EXR/16-bit PNG, HDR negatives/>1, Unicode/renamed clip, generated ordinal hints, dense hint video, wrong/extra/missing hint cases; no source mutation.

11. **Publish v2 linear products into EZ outputs — PARALLEL-SAFE after 7**
    - Files: `backend/service/frame_ops.py`, new `tests/test_v2_outputs.py`; existing service output tests after other test_service writers.
    - Symbols: add _write_v2_outputs()/_publish_v2_sequence(); update _write_manifest(); retain _write_image()/hint loaders; delete legacy _write_outputs only after no callers.
    - Changes: table/conversions above, exact original names, enable flags/formats/compression and per-file atomic writes; check all expected intermediate counts/dimensions/files. Manifest engine/version/source stems/params/formats/color/alpha convention/required range/completion; copy runtime settings. Apply Q5 publication range/resume with compatible manifest, not legacy outputs. Preview callback after files exist.
    - Acceptance: synthetic EXR tests for BGR, no double gamma, premul once, straight PNG, alpha 0/1, HDR/negative/finite half limits; enabled subsets/formats/compression/custom dirs; missing runtime output/disk failure/cancel/partial resume. No runtime-named product dirs published.

12. **Ensure optional GPU hints hold ownership through compute — SERIAL after 9**
    - Files: `backend/service/pipelines_auto.py`, `pipelines_guided.py`; concurrency and `tests/test_service_videomama_contract.py`.
    - Symbols: run_gvm(), run_birefnet(), run_sam2_track(), preview_sam2_prompt(), run_videomama(), run_matanyone2() GPU sections.
    - Changes: model creation/use lifetime under GPU lock, not merely getter block; avoid nested acquisition of nonreentrant lock. Preserve names/algorithms/progress/cancel and run_chroma_key/core import/CPU path. No unrelated model refactor.
    - Acceptance: hint-first/keyer-first direct two-thread tests cannot unload in-use model; existing generator cancellation/regression tests pass.

13. **Make routing/completion hint-aware — PARALLEL-SAFE after 7**
    - Files: `backend/clip_state.py`, `tests/test_clip_state.py`.
    - Symbols: _TRANSITIONS, classify_pipeline_route(), ClipEntry._resolve_state(), completed_stems()/_read_manifest(); shared can_run_inference(clip,use_alpha_hint) predicate if needed.
    - Changes: hint-off direct RAW/MASKED route precedes annotation/mask generator routing; hint-on preserves explicit existing pipeline routes. Allow successful RAW/MASKED→COMPLETE; complete disk detection no longer requires alpha. Check required v2 stems/extensions/manifest, not arbitrary count; EXTRACTING/ERROR remain gated. RAW still means absent external alpha, not incapable of v2 inference.
    - Acceptance: hint-off ignores annotations/mask/hints; hint-on routes retained; no-hint complete survives restart; partial range not full complete; old v1 outputs not v2 resume data.

14. **Replace inference panel controls — PARALLEL-SAFE after 7, Q3/Q4/Q6**
    - Files: `ui/widgets/parameter_panel.py`, `tests/test_parameter_panel.py`, `test_output_config.py`.
    - Symbols: ParameterPanel constructor/get_params/set_params/reset map/get_output_config/set_output_config; remove _on_despeckle_toggled/_on_garbage_matte_toggled/_on_refiner_changed/_on_parallel_changed and parallel_frames_changed; live-preview control per Q3.
    - Changes: hint enable+quality, carry+quality, VRAM Auto/Low, existing input Linear/sRGB/despill/outputs, truthful cutout-linear status Q4. Remove v1 inference despeckle/refiner/garbage/source passthrough/edge/pool controls. Retain _bg_color only as chroma-hint/cosmetic selector (get_chroma_params uses it), chroma shrink/grow/blur remain; no checkpoint tooltip. Clarify FG raw and Processed EXR premul.
    - Acceptance: offscreen construction/reset/roundtrip, retired widgets/signals absent, optional hint/chroma controls intact, old-session defaults safe.

15. **Update preferences and startup wiring — SERIAL after 14, Q1**
    - Files: `ui/widgets/preferences_dialog.py`, `ui/main_window_mixins/settings_mixin.py`, `ui/main_window.py`, `ui/app.py`; affected setup-consent/main-window color tests.
    - Symbols: remove KEY_MODEL_RESOLUTION/DEFAULT_MODEL_RESOLUTION, KEY_INFERENCE_BACKEND, KEY_PARALLEL_CLIPS/DEFAULT_PARALLEL_CLIPS; remove _apply_model_resolution_setting/_apply_inference_backend_setting/_apply_parallel_clips_setting and callers; constructor/_save_and_accept/_show_preferences/MainWindow wiring/app identity.
    - Changes: versioned inference/v2 preference keys and validated supported fields; retain tracker/EXR compression/output/language prefs. Delete pool constructor argument/signals; identity Q1 isolates old app. Startup no MPS/CPU keying advertising; About links complete v2 license/additional terms.
    - Acceptance: relaunch settings roundtrip, no old-app writes, obsolete key values ignored; no missing imports/signals; no-model offscreen init displays setup/diagnostic without loading weights.

16. **Serialize worker/progress and cooperative shutdown — SERIAL after 9,11–12,15**
    - Files: `ui/workers/gpu_job_worker.py`, `ui/main_window_mixins/cancel_mixin.py`, main_window.py closeEvent block; new `tests/test_v2_gpu_worker.py`.
    - Symbols: GPUJobWorker constructor/run/stop/_process_job_inner/_run_inference/progress/_save_preview; _run_preview_reprocess per Q3; remove _PARALLEL_JOB_TYPES/max_workers/set_max_workers/_ensure_pool/_submit_to_pool/_drain_pool/pool/semaphore/inflight.
    - Changes: inline serial queue jobs in existing QThread, retain IDs/signals/optional dispatch/error/cancel. Phase counts/status do not trigger file previews; only publication does. Stop requests cancel without GUI blocking on futures/kernel completion. Close waits cooperatively/asynchronously or refuses until safe; never destroy running GPU QThread or unload model underneath it. Existing explicit force-restart remains process escape, not normal cancel; no GPU terminate().
    - Acceptance: fake queued jobs strictly serial; no success after cancellation in load/prepass/Hann/stage2/publish; stale IDs filtered; GUI cancel/close responsive and resources cleaned. Extraction-worker policy is not a v2 keyer mechanism.

17. **Update single/multiple main run actions — SERIAL after 13–16**
    - File: `ui/main_window_mixins/inference_mixin.py`; new `tests/test_v2_run_actions.py`, affected preview/color tests.
    - Symbols: _on_run_inference/_on_resume_inference/_on_run_all_ready/_on_run_pipeline/_refresh_button_state/_do_reprocess/_on_params_changed; screen-color sorting removed.
    - Changes: shared readiness/hint-aware route, no alpha-required warning when off, correct params/output/range snapshots; explicit generators enable their generated hint, direct v2 disables it. Q3 removes automatic single-frame v1 keying but keeps output viewing. Range/resume messaging Q5; no checkpoint color sort.
    - Acceptance: RAW direct, existing hint on/off, multi-select/run all/pipeline all dispatch correct mode; partial hints warn only when used; no stale v1 live preview.

18. **Update clip selection and auto-chain snapshots — SERIAL after 17**
    - Files: `ui/main_window_mixins/clip_mixin.py`, `worker_mixin.py`, `ui/workers/job_helpers.py`; new `tests/test_v2_chain_state.py`.
    - Symbols: clip _on_clip_selected/_on_selection_changed/_detect_and_apply_screen_color, remove _offer_blue_checkpoint_download; worker _on_worker_clip_finished/chain; create_job_snapshot().
    - Changes: common hint readiness, no blue-model offer, retain cosmetic/chroma color detection. Freeze initial pipeline params/output/hint choices so panel changes mid-run do not alter auto-chain. Rescan actual completion after inference; don't unconditionally force COMPLETE on partial-range clip_finished. Cancel clears chain.
    - Acceptance: changing UI after queuing preserves frozen choices; no-hint complete persists, partial range state accurate; no download prompt for blue clip, generators/viewer refresh still work.

19. **Add direct no-hint folder-batch option — PARALLEL-SAFE after 13; batch mixin integration after 18**
    - Files: `ui/widgets/batch_pipeline_dialog.py`, `ui/main_window_mixins/batch_pipeline_mixin.py`; new `tests/test_v2_batch_options.py`.
    - Symbols: _NO_HINT_OPTIONS, BatchClipConfig, _build_ui/_populate_table/_on_global_changed/get_batch_config; _create_batch_job/_submit_batch_jobs/_on_batch_extract_finished.
    - Changes: explicit “CorridorKey 2 (no AlphaHint)” option; distinguish direct INFERENCE from existing-hint INFERENCE with use_alpha_hint field, preserve GVM/BiRefNet/VideoMaMa/MatAnyone2 selections. Pass frozen settings/choice through import/extraction/chain, not automatic GVM for every absent hint.
    - Acceptance: mixed batch per-clip choices roundtrip and dispatch correctly, no-hint input keys without downloaded hint model; existing hinted method remains distinct.

20. **Replace setup wizard required bundle checks — PARALLEL-SAFE after 2,4**
    - Files: `ui/widgets/setup_wizard.py`, new `tests/test_v2_setup_wizard.py`; retain consent tests.
    - Symbols: _checkpoint_dir replacement, detect_installed_models/MODELS/_required_model_keys/needs_setup/has_required_models/_DownloadWorker._download_one; remove _patch_mlx_progress and call.
    - Changes: one required CorridorKey 2 bundle row using common paths/readiness; no green/blue/MLX/arbitrary .pth checks. Updated size/download/error labels, optional model rows and consent preserved.
    - Acceptance: v1 .pth alone not ready, every required v2 asset checked, incomplete snapshot fails, full fake bundle ready; wizard opens without CUDA/models and downloader target equals runtime target.

21. **Replace diagnostic/model-path/opt-mode surfaces — PARALLEL-SAFE after 2,7–8; main.py edits serialized**
    - Files: `main.py`, `ui/widgets/diagnostic_checks.py`; `tests/test_diagnostic_step_resolver.py`, affected Pascal diagnostic tests, new `test_v2_model_path_check.py`.
    - Symbols: run_model_path_check/main opt-mode/env branch/v1 checkpoint load probes; missing-checkpoint diagnostic/_resolve_missing_checkpoint/run_startup_diagnostics/OOM instructions.
    - Changes: report five external asset paths plus code root; optional --verify-model-loads does real CUDA v2 load/unload; preserve hint path checks. Remove _discover_checkpoint/blue/MLX probes/--opt-mode. Remediation --corridorkey and Auto/Low, not old .pth/refiner variables. Path-only verification never loads models.
    - Acceptance: useful missing/incomplete errors, v2 code/data split output, no opt-mode help/CPU keying fallback or old checkpoint readiness.

22. **Route CLI inference through shared v2 service — SERIAL after 9–13,21**
    - Files: `clip_manager.py`, new `tests/test_v2_cli.py`.
    - Symbols: delete get_corridor_key_engine; replace run_inference(clips) old frame loop/prompts; inspect interactive_wizard/scan/dispatch callers; preserve organize/map/import helpers and alpha actions.
    - Changes: adapt legacy ClipEntry to backend contract; one service per clip batch, whole-clip call; supported prompts only, no-hint accepted; no CPU fallback/despeckle/refiner/process_frame/checkpoint glob. Hint generator/keyer never simultaneously resident. Keep main.py --cli and RunInferenceOnly.sh usable; invalid legacy-only args error clearly.
    - Acceptance: mocked CLI hinted/no-hint batch correct params/paths, two clips one model load; no v1 prompt/API, unsupported hardware fails helpfully.

23. **Remove retired v1 code and stale tests/tools — SERIAL after 9,14,18,21–22**
    - Delete files: `CorridorKeyModule/{backend,inference_engine,mlx_inference_engine}.py`, `CorridorKeyModule/core/{model_transformer,mlx_model}.py`; `tests/test_inference_engine_init.py`, `test_inference_engine_compile_fallback.py`, `test_refiner_compile.py`, old `test_backend_output_contract.py` replaced by v2 tests.
    - Preserve core/chroma_key.py (pipelines_auto/chroma_key_mixin depend on it), core/color_utils.py while cosmetic/chroma/tests use it; no wholesale CorridorKeyModule deletion. Remove unused v1 guard functions only after caller grep, not unrelated cleanup.
    - Retire v1-only scripts `bench_parallel_frames.py`, `compare_upstream.py`, `compare_upstream_d2.py`, `color_analysis.py` after caller check; update scripts/import_smoke_test.py, tests/test_no_raw_cv2_file_io.py vendor exclusion, engine-dependent test_color_utils_guard assertions, remaining test_service old mocks, test_update_zip_exclusions artifact assumptions where needed.
    - Acceptance: collection/full suite works without deleted imports; production legacy grep clean; retain meaningful color/chroma tests, focused adapter Unicode test instead of blanket app I/O exemption.

24. **Update source install/update scripts — PARALLEL-SAFE after 3–4**
    - Files: `1-install.sh`, `3-update.sh`, `Install_CorridorKey_Windows.bat`; matching core install/check steps in `1-install.bat`, `3-update.bat` after inspecting.
    - Changes: remove CPU/MPS keying fallbacks/MLX extras+downloads, use cu128 consistently uv/pip, actionable unsupported-platform/CUDA errors, Python3.11; --corridorkey now fetches all v2. Replace Windows direct v1 curl with same setup helper. Preserve optional tracker/hint choices; don't delete old app assets. Start wrappers only changed if necessary.
    - Acceptance: bash -n, no v1 URLs/MLX actions/CPU install promise; fresh target setup uses app data/single cv2/CUDA; Windows static review labeled unexecuted unless run.

25. **Document fork and guard upstream overwrite — SERIAL after 15,21,24; Q7/Q8**
    - Files: `README.md`, `CHANGELOG.md` new entry, `docker/.env.example`, translated README archived-upstream banner; settings_mixin update actions only after other writers. Root license/notice metadata only with Q7 decision.
    - Changes: NVIDIA source setup/assets/temp disk/new settings/hints/output contracts/progress/cancel/range-resume/license/provenance. Remove active opt-mode/MLX instructions, keep history. Link translations to canonical fork instructions instead of speculative translation. Disable upstream update notifications/actions pending approved fork policy; do not invent UPDATE_ORIGIN/trust keys/release repo or silently replace fork with upstream v1.
    - Acceptance: commands match CLI, full license accurately linked/About consistent, no upstream binary overwrite path, no untested CPU/macOS support claims.

26. **Run regression and real 4090 acceptance — SERIAL final**
    - Files: new `scripts/v2_smoke.py`, `tests/test_v2_no_legacy_paths.py`; scripts/gui_smoke_test.py and ci_smoke.py only where old assumptions fail. Record results in implementation report.
    - Changes: small explicit input/output/hint/profile service smoke, no second inference implementation; execute checklist below, report versions/timings/peak VRAM/residency/cancel and skipped optional tests honestly.
    - Acceptance: automated tests pass; actual short clips both hint-free/hinted, correct EXR/PNG mapping/color and persistence; consecutive model reuse and hint switches; responsive cancel/cleanup. No old-app/source-tree modifications. Unavailable optional model weights marked untested, not passing.

## Files to Modify
Exact paths/symbol ownership are above. Consolidated inventory:
- Backend: service/{core,model_manager,inference,frame_ops,pipelines_auto,pipelines_guided}.py; clip_state.py. Delete service/inference_parallel.py.
- UI widgets: parameter_panel.py, preferences_dialog.py, setup_wizard.py, diagnostic_checks.py, batch_pipeline_dialog.py; ui/app.py/main_window.py; workers/gpu_job_worker.py/job_helpers.py; main_window_mixins/{settings,inference,clip,worker,batch_pipeline,cancel}_mixin.py.
- Entry/setup/build: main.py, clip_manager.py, scripts/setup_models.py, root and GVM/VideoMaMa dependencies, NVIDIA install/update scripts.
- Tests/docs/tools: named per task. Unchanged display_transform/frame_io/viewer tests continue protecting existing EZ premul/display convention. backend/project.py stays unchanged unless Q1 explicitly changes this (prefer v2-only helper).
- **Only vendor code edits:** runtime.py optional hooks/resource cleanup; video_io.py ExrSequenceWriter failure-safe queue/shutdown. Record in VENDORED.md. No foundation/model/color math patches; path/offline-env/color publication adaptation stays outside vendor.

## New Files (if any)
- CorridorKeyV2/ vendor tree, notices/provenance and unchanged downloader helper.
- backend/v2_paths.py; backend/corridorkey_v2.py; backend/service/v2_inputs.py.
- scripts/v2_smoke.py.
- Focused tests/test_v2_{paths,setup_models,runtime_hooks,exr_writer,adapter,inputs,outputs,gpu_worker,run_actions,chain_state,batch_options,setup_wizard,model_path_check,cli,no_legacy_paths}.py. Normal pytest uses fake heavy models/tiny real image fixtures, never requires ~5GB assets or CUDA.

## Dependencies
- Wave A: 1,2,7 in parallel (2 waits Q1; new hint default Q6).
- Wave B: 3 after1; 4 after1–2; 5/6 after1, distinct files but shared ledger serialized; 10/11/13/14 after7; 20 after2/4; 24 after3/4.
- Adapter/service: 8 after1–3/5–7; 9 after8 and integrate10/11 before validation; 12 after9. GPU lifecycle testing serial, no competing heavyweight jobs.
- GUI same-file chain: 14→15→16→17→18→19 integration; 17 also13; folder dialog part19 can parallel after13. 25 waits all its same-file writers.
- Entrypoints: 21→22→23 after callers migrated. Shared tests/test_service.py/concurrency tests serialize owning-task edits; new independent tests parallel-safe.
- Final integrated 23–25→26. Do not mark passing until relevant branches merged; no parallel writers of same file, including tests/ledger.

## Validation
Commands are **implementation-worker instructions, not executed results**. Run in target only, no old app venv reuse.

### Fresh environment/dependencies
```bash
cd /home/npittas/EZ-CorridorKey2
uv python install 3.11
uv venv --python 3.11 .venv
uv pip install --python .venv/bin/python --torch-backend=cu128 -e '.[dev,tracker]'
uv pip check --python .venv/bin/python
.venv/bin/python -c 'import torch,cv2,numpy,transformers,scipy; print(torch.__version__,torch.version.cuda,numpy.__version__,cv2.__version__,transformers.__version__,scipy.__version__); assert torch.cuda.is_available(); print(torch.cuda.get_device_name(0))'
QT_QPA_PLATFORM=offscreen .venv/bin/python -m pytest -q
.venv/bin/python scripts/import_smoke_test.py
bash -n 1-install.sh 3-update.sh
```
Pip alternative: install exact torch/vision from https://download.pytorch.org/whl/cu128 first, then editable dev/tracker while retaining pins. Check only one cv2 distribution, resolved dependency versions and a temporary wheel install containing MoGe/legal resources but not weights. Smoke imports: GVM wrapper/custom UNet/LoRA, VideoMaMa CLIP/pipeline, sam2.build_sam, MatAnyone2 wrapper, BiRefNet AutoModelForImageSegmentation. Imports are not a substitute for actual available-model execution.

### Assets and paths
```bash
.venv/bin/python scripts/setup_models.py --help
.venv/bin/python scripts/setup_models.py --check
.venv/bin/python scripts/setup_models.py --corridorkey
.venv/bin/python scripts/setup_models.py --check
.venv/bin/python main.py --verify-model-paths
```
Repeat setup for idempotency; all assets in agreed app-data root, code in vendor tree, no hardcoded /home/npittas/CorridorKey2.0. Optional --verify-model-loads is explicitly heavyweight/CUDA; do not run it concurrently with GUI inference.

### Legacy removal gates
```bash
rg -n 'CorridorKeyEngine|create_engine|resolve_backend|_discover_checkpoint|has_blue_checkpoint|_MLXEngineAdapter|MLXCorridorKeyEngine|_get_engine_pool|_engine_pool|set_pool_size|set_model_resolution|set_inference_backend|KEY_MODEL_RESOLUTION|KEY_INFERENCE_BACKEND|KEY_PARALLEL_CLIPS|CORRIDORKEY_OPT_MODE|CORRIDORKEY_BACKEND' backend ui main.py clip_manager.py scripts pyproject.toml requirements.txt
rg -n 'CorridorKey_v1\.0|CorridorKeyBlue|\.mlx\.safetensors|corridorkey-mlx|--corridorkey-blue|--corridorkey-mlx' backend ui scripts main.py clip_manager.py pyproject.toml requirements.txt 1-install.sh 3-update.sh Install_CorridorKey_Windows.bat
rg -n 'process_frame\(|_run_inference_parallel|_run_inference_sequential' backend ui clip_manager.py scripts
```
Expect zero active v1 production routes/downloaders/settings. Narrow allowlist for explicit negative tests/migration documentation/history, not executable fallback. Intentional core.chroma_key/cosmetic color imports are allowed. Don't globally ban CPU/offload/image ops or hint model internals.

### Real 4090 GUI/service checklist
1. Short known green-screen PNG/video, **no AlphaHint**: RAW run, hint paths None/quality=-1; four EZ outputs/exact stems, COMPLETE after rescan/relaunch.
2. Same clip dense AlphaHint explicitly on/high-quality then off (existing hints ignored), no model reload. Run GVM→v2 and a guided generator→v2 if weights available; other optional generators actually tested or explicitly unexecuted.
3. Scene-linear EXR plate, edges/negative/>1 RGB: compare raw runtime products to publication (no double gamma, premul once, straight PNG, scalar alpha), ordinary HDR through final half; viewer/export behavior unchanged.
4. Two clips consecutive load_model count1; v2→SAM2/GVM→v2 switches drop old references and retain one heavyweight set. Record allocated/reserved VRAM, not a false requirement for driver memory to become zero.
5. Auto at 1080p and short4K on24GB; forced-low; low→auto reuse. Record selected Hann/chunk/stride/runtime/peak VRAM; no resolution2048/1024/pool. Invalid input fails preflight.
6. Cancel during load (deferred), RVM/VFM/Hann/stage2/publication; no success/COMPLETE, no leaked temp/writer/memmap, next job works. Inject imwrite/ENOSPC failures, close during job cooperative, UI responsive.
7. Range/resume Q5: whole temporal input, selected/missing publication only; nonzero original stems; v1/mismatched manifest rejected; partial job not falsely complete.
8. Renamed display name/custom output/Unicode path, TIFF/BMP/.EXR/16-bit files and ordinal hints preserve alignment; all output subsets/formats/compressions.
9. Single/multiple/folder batch no-hint run enabled; hint-on missing/partial warning; frozen settings survive panel changes and prefs relaunch. No blue checkpoint offer/backend/despeckle/refiner/pool widgets. Chroma hint generation/tracking/annotations/output viewing still work.

## Risks
- Models/vfms currently empty; no runtime/dependency compatibility claimed from planning. Core~5GB download, optional generators much larger. Full-clip float EXRs/memmaps can exceed weight size by orders of magnitude: preflight scratch disk, clean after publication.
- CUDA cancellation is cooperative, can wait for a large Hann window/active operation. Measure latency; don't promise instant stop. Writer/cleanup deadlock fixes are required integration safety, not optional polish.
- Partial-range hint generators cannot supply dense full-clip hints without regeneration or explicit hint-off; no silent alignment fix.
- Straight-v2 cutout vs EZ premul is easy to mishandle; wrong gamma/premultiplication looks plausible until edges/HDR are examined. Final EZ half format has finite range, unlike float intermediates.
- Hann80 native-res can exceed4090 VRAM; real Auto/Low testing required, not old downscale knob/CPU fallback.
- Dev data roots/Qt identities can collide with old app; isolate before preferences/setup. Upstream updater can overwrite fork; no approved new release trust origin known.
- Additional license terms materially affect redistribution/commercial integration. This is technical planning, not legal advice. Windows/frozen builds/translations not locally verified; avoid unrequested cross-platform release expansion.

## Open questions
Only unresolved implementation details follow; fixed decisions are not revisited.

**Q1 — isolated data/settings identity:** Approve Qt app name EZ-CorridorKey2 (org EZSCAPE) and `$XDG_DATA_HOME/EZ-CorridorKey2/CorridorKeyV2` (fallback `~/.local/share/EZ-CorridorKey2/CorridorKeyV2`) for v2 model assets? Recommended to avoid old-app settings/assets. Projects stay put. Specify another app-data/install-path policy if desired.

**Q2 — vendor hook/safety patches:** Approve optional progress/check_cancel callbacks in runtime loops plus EXR error-safe writer/cleanup fixes, the only planned engine-code edits? Recommended. Bit-identical vendor requires accepting deferred cancellation until blocking call returns and less precise progress. RuntimePaths needs no patch.

**Q3 — live reprocess:** Disable automatic v1 single-frame live keying, retain output viewer/playback (recommended minimal scope), or require a new explicit slow temporal-v2 preview? If required, specify context/range/latency; add assignment rather than emulate per-frame process_frame.

**Q4 — cutout-linear:** Official EXR renderer ignores it. Recommended: carry field into params/cfg but show disabled/not-applicable explanation; EXR stays linear, PNG display-coded. If it must actively change results, specify intended PNG/Processed semantics before redefining EZ contracts.

**Q5 — range/resume:** Approve whole-clip inference with range/resume affecting publication only (recommended temporal-correct minimum; no GPU compute savings)? Otherwise approve additional synchronized selected-span input/hint staging and changed boundary behavior.

**Q6 — initial external-hint default:** Recommend hint-off for fresh prefs/direct no-hint batch; explicit generator pipelines set hint-on for generated outputs, carry_hint stays independently True. Confirm if new default should instead use/generate external AlphaHint.

**Q7 — licensing/distribution:** Private local adaptation or intended public/commercial redistribution? Keep full notices either way; clarify permissions/applicable additional terms before publication. Do not treat plain CC BY-NC-SA label as complete license.

**Q8 — updates:** Disable upstream notifications/update actions until approved fork origin/policy (recommended), or specify intended fork update source? Never invent signed trust origin or allow upstream v1 binary overwrite.

## Resolved decisions (user, 2026-10-21)
- Q1 APPROVED: Qt app name EZ-CorridorKey2 (org EZSCAPE); v2 assets in `$XDG_DATA_HOME/EZ-CorridorKey2/CorridorKeyV2` (fallback `~/.local/share/EZ-CorridorKey2/CorridorKeyV2`).
- Q2 APPROVED: only vendor edits = optional `on_progress`/`check_cancel` hooks in runtime.py + ExrSequenceWriter failure-safe queue/shutdown in video_io.py; list both in VENDORED.md.
- Q3: DROP automatic single-frame live re-key. Keep output viewer/playback. No temporal preview feature.
- Q4 APPROVED: carry cutout_linear; show disabled/not-applicable for EXR.
- Q5 APPROVED: whole-clip inference; in/out range and resume affect publication only.
- Q6 APPROVED: external hint OFF by default; generator pipelines set hint ON; carry_hint stays True.
- Q7: PRIVATE/personal use. Keep full license + notices in vendored dir; no extra distribution work.
- Q8 APPROVED: disable upstream update notifications/actions.

## Review amendments (reviewer 2026-10-21, verdict approve-with-changes) — these OVERRIDE earlier text
1. Task 5: hook sites must also cover the low-VRAM stage-2 path inside `run_inference` (CorridorKey2.0 runtime.py:907-927, the `on_baton_frames=None` branch), not only prepass/Hann/normal stage-2 loops.
2. Hint-off: pass `initial_hint_path=None, hint_video_path=None` and leave `cfg.hint_quality` at its default. Do NOT set `hint_quality=-1` (runtime decides -1 internally when no hints, runtime.py:502/818).
3. Hint matching: drop the invented `_alphaHint_00000` example. Accept only verified naming: original source stems, and the generator convention `<stem>_alphahint.*` (gvm_core/wrapper.py:310-316). Anything else = actionable error.
4. FFmpeg/WriterConfig validation only when video outputs are requested. Internal runs use output_format="exr" and never touch ffmpeg; do not fail preflight on ffmpeg absence for EXR runs.
5. Split Task 23: 23a = delete v1/MLX engine files + dead v1 tests; 23b (SERIAL after 23a) = migrate surviving tests and retire/migrate scripts. Task 3: record the opencv-python → opencv-python-headless deviation in VENDORED.md and verify engine code uses only imread/imwrite/VideoCapture (no GUI cv2 calls).
