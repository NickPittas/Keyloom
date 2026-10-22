# Native Rust input map: current on-disk projects

Scope: read-only first slice only. Open an existing project and display its source and existing outputs. Do not normalize/rewrite JSON, create metadata, extract video, or create preview/cache files. Preserve bytes in all project files.

## Project and clip discovery

- V2 project: `<project>/project.json` has `version` (currently 2), `created`, `display_name`, and `clips` (clip folder basenames). `removed_clips` is an optional list of excluded basenames. `clips/` is the actual container; enumerate folders there and retain folder basename as stable clip identity. Do not assume JSON list order equals UI order: backend scanning sorts folders.
- V1 project: no `clips/` directory; the project root itself is the single clip. Its `project.json` also serves legacy clip metadata.
- `project.json` may additionally contain `output_dir` (project output root). Read unknown fields without dropping them; first slice never writes.
- V2 `<project>/clips/<folder>/clip.json`: `source` may contain `original_path`, `filename`, `copied`, and for sequences `type: "sequence"`; optional fields include `display_name`, `in_out_range: {in_point, out_point}` (zero-based inclusive), and `output_dir` (clip output override). Absence of `type` with `source.filename` is legacy video metadata.
- Some clip properties are sidecars/assets, not JSON: `<clip>/.video_metadata.json` (extraction/probe metadata), `.corridorkey_session.json` (workspace session), `.corridorkey_manifest.json` (outputs), `AlphaHint/`, `VideoMamaMaskHint/`, annotations and keyframe sidecars. First slice should read only what it displays and tolerate missing/corrupt optional metadata.

## Source resolution and frame identity

- Image sequence: prefer `<clip>/Frames/`, then legacy `<clip>/Input/`. If neither exists and `clip.json` has `source.type == "sequence"`, `source.original_path` is an external sequence directory. It must exist; do not copy it. Stems come from image filenames; the unified timeline is a natural sort of the union of stems across input and output modes. Missing mode/stem is a hole (`No file on disk`), never positional renumbering.
- Video: copied imports are at `<clip>/Source/<source.filename>`; metadata `source.original_path` still points to the original import location, and `copied: true` does not make that external path the viewer source. Use the in-project Source file first. Referenced imports have `copied: false` and use `source.original_path`. Legacy fallback is `<clip>/Input.*`. Video timeline identities are generated from probed frame count (or decoder count fallback), not output stems; synchronize by the backend's frame naming/metadata contract before claiming video/output sync.
- Still image/EXR suffixes are case-insensitive and accepted extensions are PNG/JPG/JPEG/EXR/TIF/TIFF/BMP/DPX. Frame index stores full file paths by `(view mode, stem)`.
- Hints are clip-root-relative: `AlphaHint/` and `VideoMamaMaskHint/`. Outputs use the resolved output root's `FG/`, `Matte/`, `Comp/`, `Processed/` subdirectories.

## Output-root precedence and lookup

1. `clip.json.output_dir` (absolute/custom as saved) wins.
2. `project.json.output_dir/<clip-name>`.
3. Global Qt preference `output/default_directory/<project-folder>/<clip-name>` for v2 (or `<clip-folder>/<clip-name>` in v1). This is machine-local and not serialized with the project; if unavailable to Rust, report that resolution is unknown instead of silently treating an external output set as empty.
4. `<clip>/Output`.

`clip.name` is the runtime clip name (normally the folder basename). Current tests and app pass the resolved output directory to the frame index. EXR outputs include FG/Matte/Processed; Comp may be PNG or EXR. Each mode is independently sparse; use shared stems, not equal array indices. Manifest gives enabled products, formats, and source stems when available; missing/legacy manifest does not prove completion.

## Session and color

`<clips-dir>/.corridorkey_session.json` is version 1 and stores global `params`, `output_config`, optional `workspace_path`, `selected_clip`, and `clip_input_is_linear` map keyed by clip name, plus window geometry/splitter sizes. Input color overrides currently live here, not in clip JSON. `InferenceParams.input_is_linear` defaults false; standalone EXR sequences default linear, while extracted video frames default linear only when `.video_metadata.json` says source transfer is `linear`. Honor explicit session override first. Read-only opening must not migrate the override into clip JSON.

## EXR matte contract

The EXR writer default is `dwab` (lossy DWAB); supported settings are `dwab`, `piz`, `zip`, and `none`. Matte output is an EXR image and may be single-channel Y. Treat Y as grayscale matte for display; do not assume RGBA or reinterpret it as color RGB. RGB foreground and RGBA processed output are expected by the playback acceptance criteria, but repository tests mostly create empty placeholder files and are not decoder fixtures.

## Repository fixture inventory and golden recommendation

No actual image-sequence EXR/PNG project media or complete project fixture is checked in. The discovered PNGs are UI/theme graphics and a comparison screenshot; tests synthesize empty files for stem/index behavior, so they are not usable decode goldens. `tests/test_project.py`, `tests/test_frame_index.py`, `tests/test_session.py`, `tests/test_input_colour.py`, and `tests/test_v2_outputs.py` document behavior but don't supply actual representative media.

Recommended golden: choose one existing small real user project locally (do not check private media into the repository), copy only its project/clip/session/manifest JSON and a tiny representative frame subset into a consented, sanitized fixture location, and record provenance/expected SHA-256. Include a source sequence or short copied video plus same-stem FG RGB EXR, Matte single-channel-Y EXR written with default DWAB, Comp PNG, Processed RGBA EXR, and one deliberately missing output stem. Keep `project.json`, `clip.json`, and session fields byte-identical in the read-only smoke test before/after opening; fixture creation itself should be external to the application. If no shareable project is available, request one explicitly rather than fabricate media.

## Native smoke environment

At exploration time both `DISPLAY=:0` and `WAYLAND_DISPLAY=wayland-1` are set. This establishes a GUI-capable session environment only; no native Rust window was built or launched.

## Parent verification: local media available

Two gitignored projects under `Projects/` contain real media suitable for local, read-only validation. They include 24 fps clips at 1920×1080 (161 frames) and 3200×1800 (122 and 649 frames). OpenEXR headers confirm DWAB RGB source frames, ZIP single-channel Y mattes, ZIP RGB foregrounds and ZIP RGBA processed outputs; PNG hints/composites are also present. These files are local validation inputs, not distributable fixtures. No user upload is needed. The parent recorded SHA-256 hashes of 16 project JSON files before native implementation checks in `/tmp/ck-rust-readonly-baseline.json`.

Deterministic generated fixtures are appropriate for automated tests of channel layout, compression, sparse frame identity and malformed metadata, provided they are labelled synthetic. Real-project playback measurements use the local projects above and report their actual resolution/compression.

Additional header verification: the first 3200×1800 clip has two distinct output sets. Its direct `Matte/` directory contains 122 single-Y DWAB frames, while `Output/Matte/` contains 122 single-Y ZIP frames. Both are usable decoder inputs; the viewer must follow metadata output-root precedence instead of mixing them.
