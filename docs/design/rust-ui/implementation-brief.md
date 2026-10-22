# Implementation brief: first runnable native slice (S1)

Status: approved for implementation 2026-10-22. Executor: `rust-start-implement-sonnet-01`, owns `rust-ui/` (repo root) only. No Python, HTML or `docs/` edits.
Authorities, in order: `range-output-contracts.md` > `selected-design.md` (§2 layout) > `native-input-map.md` (on-disk input rules) > this file. `selected-workflow.html` is the visual reference only: do not port it, embed it, or build a webview.

S1 is a **read-only native viewer** of existing projects in the accepted layout. It is not R2–R7. It never writes to a project, never runs or simulates AI, and never claims parity it lacks.

## 1. Scope

**In**
- Open an existing project folder (CLI arg or folder picker) read-only: v2 `project.json` + `clips/*/clip.json`, v1 fallback, per `native-input-map.md`.
- Board home → **Open clip settings** → workspace with tabs **Media · Hint · Key · Review · Deliver**, **← Back to batch**, 7-node graph strip on every tab, Properties panel on the right. Node click selects; double-click/Enter opens the node's tab.
- Shared viewer on every tab: mode chips **Input · Hint · FG · Matte · Comp · Processed** top-right, **A/B wipe** top-centre, icon transport ⏮ ◀| ◀ ▶ |▶ ⏭ centred below, compact timeline (playhead, source frame numbers, In/Out band). One clip playhead shared by all tabs.
- Real sequence playback at clip fps, seek, step, scrub; frame identity by source stem; holes show **No file on disk**, frames outside In/Out show **Outside In/Out**.
- Decode PNG (8-bit) and EXR half: single-channel `Y`, RGB, RGBA; compressions DWAB, PIZ, ZIP(S), none.
- Properties show **current values read from disk** (session params/output config, manifest, sidecars), labelled with their source. Processing controls not integrated yet show their reason. Set In/Out remains functional in memory, with a clear unsaved status; no project metadata is written.
- Hint page: large viewer (≥280 px drawing height at 1440×900) + Focus mode (`F` / Esc). Shows existing AlphaHint and keyframe marks; no painting.
- Deliver page: legacy resolved output folder (today's precedence) + read-only preview of the target token resolver (§4), labelled "Preview — not used by the current engine".
- Headless benchmark binary (§5) to feed the R1 gate.

**Out (show honestly, do not fake)**
- Primary action: disabled **Key clip** with reason "AI inference is not integrated in this build. Key in the Qt app." No Generate hint, no queue simulation.
- Persistent top banner: "Native preview · read-only · AI inference not integrated".
- Video sources not yet extracted to `Frames/`: card/viewer state "Video source: native movie decode not integrated yet (staged)". No FFmpeg dependency in S1.
- Import clips…/Import folder…, presets, Bypass Hint: present in the layout, disabled with reason. Labels exactly as in the design.
- Global output preference (QSettings) is not read; accept `--global-output-dir <path>` and otherwise show "Global output preference not read" when the precedence would reach that step.

## 2. Provisional stack (decided for S1 only; R1 still decides final)

| Need | Choice | Why / upgrade trigger |
|---|---|---|
| UI | `eframe`/`egui` with wgpu backend | Same stack as FilmCraft/EffectCraft, so Craft reuse stays cheap if GLM's pin study passes. Swap only if the R1 toolkit spike fails. |
| EXR | `exr` (exrs) | Decodes DWAB/PIZ/ZIP/none and arbitrary channels. Rust DWA encode is unsupported; deterministic DWAB fixtures can be generated with the installed Python OpenEXR writer (see §6). |
| PNG | `png` | Avoids the full `image` crate. |
| JSON | `serde`, `serde_json` | Unknown keys ignored everywhere (Python tolerates them). |
| Threads | `std::thread` + `std::sync::mpsc` | No async runtime. |

Nothing else without a one-line justification in the PR. Commit `Cargo.lock`. Keep all UI code in `src/ui/`; core modules must not import egui, so a toolkit swap only touches `src/ui/`.

## 3. Files and tasks (do in order; each ends green: `cargo test`, `cargo clippy -- -D warnings`)

```
rust-ui/
  Cargo.toml  Cargo.lock  README.md        # build/run/test/bench commands, honest limits list
  src/lib.rs
  src/natural_sort.rs   T1
  src/project.rs        T2  read-only loader → Project{clips: Vec<Clip>}
  src/clip.rs           T2  assets, state, readiness text, output dir, manifest
  src/contracts.rs      T3  range/output structs + token resolver (pure)
  src/decode.rs         T4  PNG/EXR → DisplayFrame (RGBA8 + flags)
  src/frames.rs         T5  stem index per view, bounded LRU cache, decode worker
  src/playback.rs       T5  clock, play/pause/step/seek, latest-request-wins
  src/bin/ck-bench.rs   T6  headless decode/seek benchmark
  src/main.rs           T7  eframe entry, CLI args
  src/ui/{app,board,workspace,graph,properties,viewer,timeline}.rs  T7
  tests/fixtures/…      tiny documented synthetic decoder fixtures; no private media
  tests/loader.rs  tests/decode.rs  tests/playback.rs  tests/contracts.rs
```

**T1 natural sort.** Port `backend/natural_sort.py` (`(\d+)` split, int compare). Test: `f2 < f10`, mixed case as Python.

**T2 loader + clip model.** Follow `native-input-map.md` exactly: clip discovery (skip `.`/`_` dirs, honour `removed_clips`), source precedence `Frames/` → `Input/` → `Source/` video → `clip.json` external sequence → `Input.*`, display name, `in_out_range` (zero-based inclusive, `None` = whole), `output_dir` precedence (clip `output_dir` → project `output_dir`/`<clip>` → `--global-output-dir`/`<project>`/`<clip>` → `<clip>/Output`), AlphaHint/VideoMamaMaskHint, `.corridorkey_hint_keyframes.json` (strict: keys exactly `{version, keyframes}`, else show the error; never repair), `.corridorkey_manifest.json`, `.video_metadata.json`, session `.corridorkey_session.json` (`params`, `output_config`, `clip_input_is_linear`). States EXTRACTING/RAW/MASKED/READY/COMPLETE/ERROR mirror `ClipEntry._resolve_state` (COMPLETE only with `engine == "corridorkey-v2"`, `complete: true`, `frame_range: null`, and completed stems ≥ input count; partial alpha vs range count). Input colour: session map, else default Linear for an EXR sequence without video metadata or with `color_transfer == "linear"`. Corrupt JSON → clip/field error text, never a panic, never a write.
Test: generated temp projects for v2, v1, external sequence, clip/project output overrides, partial alpha, manifest complete / partial / with `frame_range`, bad sidecar, corrupt `clip.json`; and a **no-write check** (hash every file's mtime+len before/after load and a full playback pass).

**T3 contracts.** Serde structs only; no behaviour wired to Python:
- `RangeIdentity { in_point, out_point, in_stem, out_stem }` (+ source fingerprint placeholder `Option<String>`); `validate(stems) → Ok | SourceChanged`.
- `OutputLocation { output_root, folder_template, file_template }`, `ClipOutputValues { resolution, date, version }`, `OutputKind { Fg, Matte, Comp, Processed }`.
- `resolve(location, values, project, clip, kind, stem_number, ext) → Result<PathBuf, LocationError>` with the full §4 validation of `range-output-contracts.md` (closed token set, one `####` run, no `/` in file template, no absolute/`..`/`.`/empty segments, value sanitising, Windows reserved names, 255-byte segments, stays under root) and `collisions(&[Resolved]) → Vec<Collision>` (two outputs same path; two clips same path).
- `default_resolution_label(width)`.
Tests: user example resolves to `/mnt/NAS/Project/Masks/clip_name_2K_18-10-2027_v001/clip_name_1101.exr`; padding grows (`####` with 12345); FG+Matte without `{output}` collide; Matte-only passes; traversal/absolute/unknown token rejected; label thresholds 1919/1920/3199/3200/4999/5000.
These are the Rust half of the R0 golden check; Python stays authoritative and unchanged.

**T4 decode → `DisplayFrame { w, h, rgba8, has_alpha, clipped_hdr: bool }`.**
- EXR: read all channels by name. `Y` → grey. RGB/RGBA linear → sRGB encode for display unless the source view is interpreted sRGB (input colour). Values <0 or >1 clamp and set `clipped_hdr` (viewer shows "HDR clipped for display"). Never double-encode: PNG and sRGB-interpreted EXR pass through.
- Alpha: FG/Processed shown straight over a checker; Matte/Hint as grey; Processed EXR is straight-linear per manifest `alpha_convention`.
- Size mismatch vs source → letterbox at source aspect, with a notice; do not resample silently.
Tests: generated PNG and EXR (Y, RGB, RGBA × PIZ/ZIP/none) with known pixel values → exact RGBA8 after transform; negative/HDR flag; linear 0.18 → sRGB 118 ±1.

**T5 frames + playback.** `FrameIndex` maps clip stem → path per view (Input, Hint=AlphaHint, FG/Matte/Comp/Processed using manifest formats, else probe `.exr` then `.png`). `Cache` byte-budgeted LRU (default 1 GiB, CLI `--cache-mb`). Decode on a worker pool (N = cores−1, max 4); requests carry a generation id; stale results are dropped, never displayed over a newer request. Prefetch ahead of the playhead while playing. Clock at fps from `.video_metadata.json`, else 24 with a visible "fps assumed 24" note. If decode lags, hold the frame and count drops (no skipping of frame identity in step/seek).
Tests: missing stem → `NoFile`; stem outside range → `OutsideRange`; seek storm (1000 random seeks) displays only the last request; cache never exceeds budget; step is deterministic.

**T6 `ck-bench`.** `ck-bench <project> [--clip NAME] [--view matte|fg|…] [--cache-mb N]` prints one JSON line per run: resolution, channels, compression (from header), frames, cold and warm decode ms (median/p95/max), sustained fps vs clip fps, seek latency median/p95, peak cache bytes, CPU, git rev. This is evidence for R1, not a pass/fail gate for S1.

**T7 UI.** Tokens from `design-brief.md` §2 (hex values, 13 px base / 12 px min, tabular numerals, 2 px amber focus ring, ≥28 px targets). Layout per `selected-design.md` §2.1–2.4: Board cards (name, state + icon text, readiness text, range "Writes 1101–1300 of 412", outputs present) with checkbox multi-select (Space toggles); workspace header with Back + tabs; graph strip (~130 px, collapses to ~40 px one-row on Hint/Review) with status badges from real state ("200/200 on disk", "Off", "Needs hint", "Linear ▲ check"); Properties scroll with pinned disabled primary action; queue strip shows "No jobs (queue not integrated)". Keyboard: `B`, `1`–`5`, `G`, ←/→ (graph vs frame step), Enter, Space play/pause, `F`, Esc, Home/End. Minimum window 660 px high; at 728 px wide Properties becomes a toggled side sheet. Unavailable view chips disabled with a reason ("No hint yet", "Output off").

## 4. Run

```
cd rust-ui
cargo run --release -- ../Projects/261022_122316_V1-0047_A001C055_261012_R4LF_1080
cargo run --release --bin ck-bench -- ../Projects/261021_171345_V1-0069_A002C012_261012_R4LF
```

## 5. Acceptance (S1 done = all true, with evidence pasted in the PR)

1. `cargo build --release`, `cargo test`, `cargo clippy -- -D warnings`, `cargo fmt --check` pass on Linux. Windows build not claimed.
2. Both real projects under `Projects/` open; Board lists every clip with the same state and frame count the Qt app shows (parent verifies against Python `scan_project_clips`; Sonnet records its own output).
3. No-write proof on both real projects: `find <project> -newer <stamp>` is empty after open + 60 s playback + tab tour.
4. Matte (single-channel Y ZIP from the local projects plus a synthetic Y DWAB fixture), DWAB RGB source, RGB FG and RGBA Processed EXR and Comp PNG display; screenshots of each mode and A/B wipe at 1440×900 and 728×900 attached.
5. `ck-bench` JSON for each real project, cold and warm. Record whether sustained playback reaches clip fps; if not, state the gap — do not tune past it in S1.
6. Banner, disabled Key action and disabled controls all show their reasons; nothing in the UI suggests AI ran.
7. Hint viewer ≥280 px tall at 1440×900; graph and primary action visible at 1440×900 and 728×900; no horizontal page scroll at 728.

## 6. Fixtures and real data

- Unit/integration tests use tiny deterministic synthetic fixtures. PNG/PIZ/ZIP/none can be generated in Rust at runtime; small DWAB fixtures generated with Python OpenEXR may be committed with their generation script and expected pixel values.
- **DWAB cannot be generated by the Rust exr encoder.** Sonnet may use the installed `.venv` Python OpenEXR writer to generate test fixtures under `rust-ui/`; this does not change production Python code. Also run opt-in local-media checks: real source frames are DWAB RGB, while local Matte/FG/Processed outputs inspected by the parent use ZIP. Assert dimensions, channel interpretation and finite values against known references.
- Real projects are read-only and git-ignored: never copy their frames into the repo, never write beside them.

## 7. Hand-back

PR description lists: what works, each disabled feature with reason, bench JSON, screenshots, test counts, and open issues for R1 (toolkit verdict input, cache size, fps gap, any EXR variant that failed). Next after S1 is not chosen here; R0 contracts and the R1 gate decide it.
