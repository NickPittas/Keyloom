# Preset contracts: code facts and design gaps

Read-only map for the selected Batch Board + Page Tabs + fixed per-clip graph design. This documents the existing Python engine/UI contracts; it does not prescribe a Rust schema. Field defaults below are dataclass/UI defaults unless marked global.

## Serializable inference and output settings

`InferenceParams` (`backend/service/core.py`) serializes via `to_dict()` / `from_dict()`. Unknown keys are ignored on load.

| Field | Default | Validation / meaning | Preset recommendation |
|---|---:|---|---|
| `input_is_linear` | `false` | Boolean; controls source interpretation. UI auto-detects from EXR/metadata and remembers overrides per clip in the session. | Preserve per clip; mixed sources need not share interpretation. |
| `hint_mode` | derived `none` | `none`, `full`, `keyframes`; synced to legacy `use_alpha_hint`. | Copy workflow choice, but only `full`/`keyframes` when target has a compatible hint. |
| `use_alpha_hint` | `false` | Legacy compatibility field; derived from `hint_mode` on construction. | Do not expose separately. |
| `hint_quality` | `0.95` | Number in `[0,1]`; used with external AlphaHint. | Copy. |
| `carry_hint` | `true` | Boolean. | Copy. |
| `carry_hint_quality` | `0.95` | Number in `[0,1]`. | Copy. |
| `vram_profile` | `auto` | `auto` or `low`. | Copy. |
| `despill_strength` | `0.5` | Number in `[0,1]`; UI steps by 0.1. | Copy. |
| `cutout_linear` | `false` | Carried into engine config; comment says no effect on EXR output. | Copy only if it remains an exposed/effective engine setting. |
| `temporal_window` | `auto` | `auto`, `80/40`, `48/24`, `24/12`, `8/4`, `1/1`. | Copy. |

`OutputConfig` also serializes with unknown-key filtering. It contains `fg_enabled=true`, `fg_format=exr`, `matte_enabled=true`, `matte_format=exr`, `comp_enabled=true`, `comp_format=png`, `processed_enabled=true`, `processed_format=exr`, and `exr_compression=dwab`. Each format accepts `exr` or `png`; compression accepts `dwab`, `piz`, `zip`, or `none`. `validate_selection()` rejects all outputs disabled. **Compression is one shared `OutputConfig.exr_compression` value, not per-output.** It is read from global QSettings (`output/exr_compression`, default `dwab`) when the panel snapshots output config; the current panel setter does not restore compression from the config. Compression only affects EXR writes; PNG ignores it. Video-frame extraction has its own internal ZIP16-to-DWAB storage conversion and does not use this preference.

Image outputs are separate `FG`, `Matte`, `Comp`, `Processed` sequences, independently enabled and each independently EXR/PNG. They use the source frame stem and are published under the clip's output root. EXR output is half-float through `write_exr`; PNG is 8-bit. A run manifest records enabled outputs, per-output formats, inference params, source stems, output frame range, and keyframe indices; compression is not recorded there. Video delivery is a separate later export: dialog choices include MOV ProRes 4444 (alpha), WebM VP9 (alpha), and MP4; file path is chosen at export time. It is not an `OutputConfig` field.

## Per-clip and global state

| Existing state | Owner / persistence | Workflow preset behavior |
|---|---|---|
| Source asset path/type, source stems and metadata | Per clip; project/clip metadata and imported media | Never overwrite. |
| `in_out_range` (`in_point`, `out_point`, inclusive, zero-based) | Per clip in `clip.json` (or legacy `project.json`); `None` means whole clip | Preserve. Range is not an inference limit: inference processes the whole clip, range controls frames written. Applying absolute indices to a different-length clip is unsafe. |
| Sparse hint keyframes | Per clip sidecar `.corridorkey_hint_keyframes.json`, indices plus source stems | Preserve. Loader validates schema/version, sorted unique indices, bounds, and that each index still names the same source stem. Never bulk-copy indices across clips. |
| Full hint / alpha / mask assets | Per clip files/assets | Preserve; do not copy or replace with preset. Validate target asset availability before choosing a hint-consuming route. |
| Paint annotations | Per clip `annotations.json`, keyed by frame index; strokes include coordinates, brush type and radius | Preserve. Frame-indexed pixel data is source-specific. |
| Chroma-key generator settings | Per clip `clip.json` as `chroma_params`: `screen_color`, `screen_samples`, `screen_type`, `strength`, `clip_black`, `clip_white`, `shrink_grow`, `edge_blur` | Treat as source-specific; preserve by default. Current persistence accepts a dict without schema validation. |
| Custom output directory | Per clip `clip.json` override | Preserve. |
| Project output directory | Project `project.json`; takes precedence over global default, after per-clip override | Preserve unless user explicitly applies an output-location preset. |
| Default output directory | Global QSettings `output/default_directory` | Do not silently apply per-project/clip. Resolved clip path precedence: clip override → project folder plus clip name → global folder plus project/clip → `Output/` under clip. |
| EXR compression | Global QSettings; copied into each job's `OutputConfig` snapshot | Reasonable Export preset field; preserve as a single global choice until schema supports per-output compression. |
| Batch hint generator/route and model variant | Batch dialog's per-clip transient `BatchClipConfig`; not `InferenceParams` | Workflow preset may capture route defaults, but generator availability is determined by each clip's hint/mask/annotation assets. |

## Existing preset-like mechanisms and batch behavior

- `.corridorkey_session.json` stores one workspace's current `params`, `output_config`, UI geometry, selected clip, and `clip_input_is_linear` map. It is a restorable session snapshot, not a named or shareable preset.
- `.corridorkey_manifest.json` describes one inference publication for resume/validation; it is not applied as settings.
- Batch Pipeline currently selects per-clip alpha routes in its dialog, then freezes the current parameter and output snapshots for the batch. Generator pipelines can chain into inference. Each inference job gets that clip's own `clip_frame_range(clip)`; the batch runner does not copy ranges, keyframes, source assets, or annotations between clips.
- There is no existing named workflow preset or Export preset store, apply-to-selected operation, preset schema/version, or partial-field merge behavior.

## Suggested preset copy boundary for the selected design

- **Workflow preset:** copy only reusable processing controls (`hint_mode`, hint/carry qualities, `carry_hint`, `vram_profile`, `despill_strength`, `temporal_window`, and any deliberately supported engine option), plus an optional default hint route if represented separately. Treat input interpretation as a per-clip override. On batch apply, retain each clip's source, range, hint assets, keyframe sidecar, annotations, chroma samples/params, and output location. If a preset selects Full/Keyframes, resolve missing/incompatible hints per clip and surface validation; do not fabricate/copy source-bound data.
- **Export preset:** copy output enables and each output's EXR/PNG format, shared EXR compression, and (if the product wants it) a video container/codec choice. Preserve source/range and per-clip output directory. Ask the user before applying any output path; current image sequence output roots are resolved per clip, while video export prompts for a file path.
- Applying presets to multiple clips should be a field merge over selected reusable settings, not replacement of the clip record. Existing serializers validate enum/range values, but do not validate `InOutRange` against a particular source length; target-specific validation remains necessary.

## Gaps to settle before implementation

1. Whether the UI's separate `Hint` step means generating/importing hint assets or only selecting how an existing hint is consumed. Current engine has multiple hint-producing pipelines beyond `InferenceParams.hint_mode`; a fixed `Media → optional Hint → Key` graph needs an explicit route/asset contract.
2. Preset storage scope and versioning (user/global, project, or portable file), and whether applying a preset replaces or selectively merges fields.
3. Whether `input_is_linear` and output directories are ever opt-in preset fields; defaults should avoid copying these across heterogeneous clips.
4. Export preset split: image-sequence output config versus later video render/container/codec and destination. Existing video export is path-prompted and has no reusable export-config object.
5. Whether EXR compression remains global or moves into per-output settings. Current engine is unequivocally one compression value per `OutputConfig`, shared by all EXR outputs; extraction's DWAB conversion is separate internal storage behavior.
6. Validation UX for hint route prerequisites, incompatible frame ranges/keyframes, all outputs disabled, missing output directories, and source changes that stale annotations or sidecars.
