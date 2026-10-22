# Rust UI: import → Board → workspace (implementation brief)

Status: contract for the implementer (2026-10-22). The user rejected the read-only viewer because it could only open a
project folder, so there was nothing to review. **Goal of this slice:** a user with no project picks videos or an
image-sequence folder, waits while a progress bar runs, and then plays real frames in the Board → Media/Hint/Key/Review/Deliver tabs
with the graph. The user has authorized writes for **import only**. Everything else stays read-only.

**2026-10-22 user correction (supersedes compatibility wording below):** backward compatibility with the Qt app is
no longer an acceptance requirement. Reuse Python only where it is the cheapest route, not to keep the Qt app reading
projects unchanged. Never destroy or overwrite existing user data. The graph is now specified by `node-canvas-brief.md`.

Parent implementation corrections (take precedence over the detailed suggestions below):

- Collision exhaustion must be checked **before** calling a mutating creation function, including collisions among the items in the same batch. Checking a returned folder against a prior snapshot is too late to prevent overwrite.
- Incomplete native imports should have an explicit importer-owned marker or equivalent reliable provenance. Do not reclassify every legacy clip with frames but no metadata sidecar as an incomplete native import.
- Preserve unknown project metadata and use atomic metadata replacement. Missing extraction prerequisites should be reported before copying where possible.

Scope boundary: no AI, queue, Generate hint, Key or presets. **Why:** the inference worker and job queue live in the Qt process.
Wiring them in is milestone work (`rust-path.md`), and this slice's purpose is a reviewable UI with real footage.
The Key/Generate buttons stay disabled with their existing reason text.

## 1. Decision: reuse the Python import and extraction through one helper subprocess

Rust must not re-implement project creation or FFmpeg extraction. Python already owns the on-disk format, and the
Qt app has to keep reading these projects unchanged. Rust spawns **one Python helper per import** and reads JSON lines from it.

| Need | Reuse (do not rewrite) |
|---|---|
| New project (videos + sequences, timestamped folder, dedup `_2…_99`) | `backend/project_media.py::create_project_from_media(copy_video=True, copy_sequences=False)`: the Qt defaults (`DEFAULT_COPY_SOURCE=True`, `DEFAULT_COPY_SEQUENCES=False`) |
| Add to existing project | `backend/project.py::add_clips_to_project`, `backend/project_media.py::add_sequences_to_project`. Both read and rewrite the whole `project.json` dict, so unknown keys survive |
| Folder → clips | Port the rules in `ui/main_window_mixins/import_mixin.py::_add_folder_to_project` (L247–295) into the helper: top-level videos (`filter_companion_hints`), the folder itself if `folder_has_image_sequence`, and image subfolders that do not start with `.`/`_` and are not hint dirs |
| Video → playable frames | `backend/ffmpeg_tools/extraction.py::extract_frames(video, <clip>/Frames, on_progress, on_recompress_progress, cancel_event, total_frames)` produces EXR half DWAB plus `.dwab_done`. It already resumes |
| Metadata sidecar | `write_video_metadata`. **Move** the metadata dict built inline in `ui/workers/extract_worker.py::_process_job` (L190–210) into `backend/ffmpeg_tools/metadata.py::build_video_metadata(video_path, info, extracted)`, then call it from both the Qt worker and the helper. Do not copy the dict |

**Not done in this slice:** extracting companion AlphaHint/MaskHint videos. They are still copied next to the clip
by `_create_clip_folder`, so the Qt app can extract them later. Say so in the README.

### Helper: `rust-ui/tools/ck_import.py` (new, about 150 lines)

```
python rust-ui/tools/ck_import.py [--data-dir D] new --name NAME [--video F]... [--folder DIR]... [--sequence DIR]...
python rust-ui/tools/ck_import.py add --project P [--video F]... [--folder DIR]... [--sequence DIR]...
python rust-ui/tools/ck_import.py extract --clip-root C          # resume one clip's extraction
```

- Put the repo root on `sys.path`. `--data-dir D` → `set_app_dir(D)`, so new projects go to `D/Projects`. The default is the Qt dev
  default `<repo>/Projects`. QA **always** passes a temp `--data-dir`.
- stdout carries only JSON lines. Logs go to stderr, which Rust shows in an expandable error detail:
  `{"event":"created","project":P,"clips":[{"name":..,"root":..,"kind":"video"|"sequence","video":..}]}`,
  `{"event":"progress","clip":..,"phase":"extract"|"compress","done":n,"total":m}` (throttled to 100 ms, as the Qt worker does),
  `{"event":"clip_done","clip":..,"frames":n}`, `{"event":"clip_error","clip":..,"message":..}`,
  `{"event":"cancelled"}`, `{"event":"finished"}`. A fatal error prints `{"event":"error","message":..}` and exits with code 1.
- Cancel: when a stdin line `cancel` arrives **or stdin closes** (for example because the UI died), the helper sets the `threading.Event` it passes to
  `extract_frames`, emits `cancelled`, and exits with code 0. Rust never SIGKILLs during extraction unless the helper ignores
  cancel for 10 s. A cancelled clip stays on disk and can be resumed.
- Extraction runs one video at a time, after **all** clip folders and `project.json` are written. That way a cancel or crash
  never leaves a clip that is not listed in `project.json`.
- **Guards. Each one is a fatal error that happens before any write:**
  - `add` on a v1 project (`not is_v2_project`) → refuse. `add_clips_to_project` would create `clips/` and change the project's layout.
  - `project.json` exists but `read_project_json` returns `None` (corrupt) → refuse. The add functions would otherwise rewrite it as `{}`.
  - A dedup overflow (`_create_clip_folder` and `create_clip_from_sequence` fall back to the *existing* folder after `_99`)
    → snapshot `clips/` before the call. If a returned name was already in the snapshot, abort and report it.
  - A sequence whose images are not all `.png`/`.exr` → refuse, with the message "JPEG/TIFF/DPX/BMP sequences can't be shown
    in this native build yet. Convert to PNG/EXR or import in the Qt app." Rust only decodes PNG/EXR, and the slice must
    not create clips it cannot play. Rust runs the same check during preflight, so the user sees it before confirming.
  - Videos are copied with `copy2` into `Source/` and never overwritten (the existing `isfile` check). Sequences are referenced in
    place, so the helper writes nothing in the source folder. Extraction writes only `<clip>/Frames/` and the sidecar.

## 2. Rust changes

| File | Change |
|---|---|
| `rust-ui/src/import.rs` (new) | `ImportJob::spawn(args) -> ImportJob` runs `std::process::Command` with piped stdin/stdout/stderr and a reader thread that sends `ImportEvent` on an `mpsc` channel. It also provides `cancel()` (write `cancel\n`), `try_recv()`, and a `Drop` that cancels. Python is `$CK_PYTHON`, else `<repo>/.venv/bin/python` (`.venv\Scripts\python.exe` on Windows). The repo is `--repo`, else `env!("CARGO_MANIFEST_DIR")/..`. Missing python or helper → inline error that names the path. Unit-test the event parser |
| `rust-ui/src/ui/app.rs` | **Landing** (no project open) replaces the bare path field. It has three large actions: **Import videos…** (`rfd` `pick_files`, filter = `_VIDEO_EXTS`), **Import folder…** (`pick_folder`) and **Open project…**, plus a dropped-files handler that sorts dropped paths into videos and folders. On the Board, enable the existing **Import clips…** / **Import folder…** buttons (L369–372). With an open project they add to it |
| | **Inline confirm bar, not a modal.** Example: "Create project **[harbour promo ✎]** in `<root>/Projects` · 3 videos (copied into the project, frames extracted to EXR) · 1 sequence (referenced in place) · Not importable: `x.jpg` sequence — reason. [Import] [Cancel]". For an existing project it reads "Add to 'Harbour Promo'". The name defaults to Python's derivation (first file stem, `_`→space) |
| | While a job runs, each new clip card shows "Extracting 120/412" or "Compressing 300/412" with a progress bar and a per-import **Cancel**. Opening that clip is disabled with the tooltip "Frames still extracting". The UI thread only polls `try_recv`, calls `request_repaint_after(100ms)`, and never blocks. Only one import job runs at a time. The import buttons are disabled with the reason "Import running" |
| | On `created` and on each `clip_done`, re-run `load_project` (it reads only), keeping Board selection, open clip and tab. On `finished`: if one clip was imported, open it on **Media**. Otherwise stay on the Board with the cards playable. Errors show on the card (`clip_error`) or in a Board banner (`error`) with the stderr tail behind "Details" |
| `rust-ui/src/project.rs` | A new state, `ClipState::ExtractionIncomplete`: `Frames/` is non-empty, there is no `.video_metadata.json`, and a video source exists. This covers a cancelled or crashed import. Its readiness reads "Extraction incomplete — Resume", and the card button runs `ck_import.py extract --clip-root`. Without this state, a partial `Frames/` is listed as a short, playable Raw clip. A video with an empty `Frames/` keeps the existing `Extracting` state and gets the same Resume button |
| `rust-ui/README.md`, `Cargo.toml` description | Change "read-only" to "read-only except import". List what import writes |

No new crates. `rfd`, `serde_json` and `std::process` are enough.

## 3. Acceptance (all with synthetic inputs in temp dirs; never touch `Projects/` or any real project)

Fixture: `ffmpeg -f lavfi -i testsrc2=size=320x180:rate=24 -t 2 a.mp4` (48 frames), a copy as `a2.mp4` with the same stem in another dir,
`b.mov`, and `seq/` with 10 synthetic PNGs plus `seqjpg/` with JPGs, all in `$TMP`. Run the app with `--data-dir $TMP/data`.

1. **Landing → new project → play:** pick `a.mp4` and `b.mov`. The confirm bar appears. Import. Progress advances for each card
   and the window stays responsive: drag/resize and Board scroll keep working during extraction. On finish, both cards open.
   Media shows the testsrc image at 24 fps with 48 timeline frames. The tabs and graph match the read-only viewer.
   Screenshot evidence: `--screenshot` taken during extraction and after it finishes.
2. **Folder import:** `seq/` becomes one sequence clip that is playable and referenced in place. `seqjpg/` is listed under
   "Not importable" with its reason, and **no clip folder is created** for it.
3. **Existing project:** open the project from step 1 and add `a2.mp4`. Its clip folder is `a_2`, and `project.json` still has an
   unknown key that the test inserted beforehand. A Qt-side check, `pytest` calling `get_clip_dirs` and `read_project_json`, lists all clips.
4. **Cancel and resume:** start a 10 s fixture and cancel mid-extract. The helper exits cleanly, the card shows "Extraction
   incomplete — Resume", Resume completes it, and playback works.
5. **Safety:** compare `sha256` and mtime for every source file before and after import. Nothing in `seq/` changes. Adding to a v1
   project, or to one with a corrupt `project.json`, gives an error before any write (`find -newer` shows nothing). A missing
   `.venv` python or a missing ffmpeg shows an inline error without a crash.
6. **Python compatibility:** `tests/test_ffmpeg_tools.py` and the existing project tests pass after the `build_video_metadata` move.
   Add one test proving the Qt worker and the helper produce identical sidecars for the same probe dict.
7. `cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test`. A new `rust-ui/tests/import.rs`
   runs the helper end-to-end against the fixture. It is `#[ignore]` when ffmpeg or `.venv` is absent, and it prints why.

## 4. Explicitly deferred

These are deferred: quick-open of media without creating a project, because Python owns clips and Frames, so a session-only mode would need a second format; companion hint extraction; JPG/TIFF/DPX
decode; a copy-sequences toggle; recent projects; thumbnails; parallel extraction; AI, queue and presets.
