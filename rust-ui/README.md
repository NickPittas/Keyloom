# corridorkey-ui — native viewer + importer (first slice)

A native Rust window (eframe/egui, wgpu backend — **provisional toolkit**, final choice is milestone R1) that imports
footage or opens an existing CorridorKey project and shows its source and existing outputs in the selected layout:
Batch Board → clip workspace (Media · Hint · Key · Review · Deliver) with a persistent 7-node graph, a Properties panel,
and one shared viewer (mode chips top-right, A/B wipe top-centre, icon transport centred, scrubbable timeline,
Set In (I) / Set Out (O) / Clear In/Out on every page).

**Import** (the only thing that writes project data): *Import clips…* (movies, multi-select), *Import folder…* (movies, plus image
sequences in the folder or its sub-folders) or drop files/folders on the window. File dialogs run on a worker thread (one at a time; the window never freezes) and a result picked while a clip is open or an import runs is held / refused, never applied under you. Drops need X11: under Wayland the app starts through XWayland when `$DISPLAY` has a live socket (`CK_WAYLAND=1` keeps native Wayland, where winit delivers no drops). A plan is shown first (name, destination,
which clips are skipped and why); nothing is written until you press *Import*. The plan includes a **Destination** section that previews exactly where it writes:
with no project open (drop or button) you must choose a folder (*New project in folder…*) or an existing project (*Add to another
project…*) first; *Import* stays disabled until you do. The chosen folder is the parent: the new project is `<chosen folder>/<yymmdd_hhmmss>_<name>`
(no extra `Projects` level; the stamp is the import time). With a project open it is pre-selected as *Add to the open project*,
shown on the bar, and can be switched the same way. Changing the destination re-plans (clip names depend on it) and never writes;
Cancel or a fresh drop forgets sources and destination. The `--data-dir … --import … --auto-confirm` QA flags keep creating in `<data-dir>/Projects/…`. Written by import only: `project.json` (atomic replace, unknown keys kept),
`clips/<clip>/clip.json`, a copy of each movie in `Source/`, extracted EXR frames in `Frames/` and `.video_metadata.json`.
Sequences are referenced in place (PNG/EXR only) and the source folder is never touched. The work is done by
`tools/ck_import.py`, which reuses `backend/project*.py` and `backend/ffmpeg_tools` (needs the repo `.venv` and FFmpeg; `CK_PYTHON` overrides
the interpreter). Cancel stops after the current step; an unfinished movie keeps its source copy and its card offers
*Resume extraction*. Frames are extracted into a hidden `.frames_importing` folder and renamed to `Frames/` only when complete,
so partial frames never look playable. Companion AlphaHint/MaskHint videos are copied but **not** extracted here.

Settings writes a separate user preferences file. Project viewing is read-only; it **does not run, queue or simulate AI**. In/Out and input-colour edits stay in memory (not saved).

## Run

```sh
cd rust-ui
cargo run --release -- ../Projects/261022_122316_V1-0047_A001C055_261012_R4LF_1080   # or pick a folder in the UI
cargo run --release                                                                     # landing: Import clips… / Import folder… / Open project…
cargo run --release -- --data-dir /tmp/ck --import a.mp4 --import b.mov --auto-confirm      # QA: import at launch
```

Useful options (all optional; see `--help`): `--global-output-dir D` (stands in for the Qt-only global output
preference, which is not read), `--cache-mb N` (frame cache budget in MB; default = half of the available RAM, 1–8 GB), `--open-clip NAME`,
`--tab media|hint|key|review|deliver`, `--view input|hint|fg|matte|comp|processed`, `--ab VIEW`, `--frame N`, `--play`,
`--window WxH`, QA options `--keys i,Shift+Right,click:605x66` (synthetic egui events, one per 400 ms, state printed to
stderr) and the evidence option `--screenshot FILE.png [--screenshot-after SECS]` (writes a PNG, then exits).

Keys: `B` back · `1`–`5` tabs · `G` show/hide graph · `F` focus viewer · `Esc` leave focus/sheet · `Space` play ·
`←/→` step (Shift = 10) · `Home/End` · `I` / `O` set In / Out · `L` loop in/out (forward and reverse) · wheel / `+` `-` zoom, middle-drag pan (primary-drag pans too unless A/B is on, where it moves the wipe), `0` fit · `P` Properties sheet (narrow windows) · `R` retry a failed frame
(max 3 per frame, never automatic). Shortcuts are ignored while typing; they keep working while a graph node has focus.

## Headless evidence tool (same core code as the GUI)

```sh
cargo run --release --bin ck-bench -- inspect <project>                       # clips, states, frame counts, output roots
cargo run --release --bin ck-bench -- raw <file.exr>                          # untouched channel stats (numeric check)
cargo run --release --bin ck-bench -- bench <project> [--clip N] [--view matte] [--frames 40] [--cache-mb 1024] [--dir FRAMES_DIR]
```

`bench` measures the **core decode + playback scheduler only** and prints one JSON line. Texture upload to the GPU
(~23 MB per 3200×1800 RGBA8 frame, doubled with A/B) happens on the UI thread in the GUI and is *not* included, so its fps are
**upper bounds** for the GUI, not GUI measurements. The line contains: decoded format/size, cold and warm per-frame ms (median/p95/max), a real-time playback
run at the clip fps from a cold cache (frames shown / dropped / achieved fps), cold-cache seek latency, peak cache
bytes and peak RSS.

## Test

All tests must use the isolation wrappers in [AGENTS.md](AGENTS.md). Never test on the live desktop.

```sh
bash tools/isolated-test.sh /usr/bin/env CARGO=/usr/bin/cargo /usr/bin/cargo-fmt fmt --check
bash tools/isolated-test.sh /usr/bin/env CARGO=/usr/bin/cargo /usr/bin/cargo-clippy clippy --offline --all-targets -- -D warnings
bash tools/isolated-test.sh /usr/bin/cargo test --offline
bash tools/isolated-test.sh /usr/bin/env CK_PREVIEW_PROJECT=../Projects/<p> /usr/bin/cargo test --release --offline --test real_preview -- --ignored --nocapture
bash tools/virtual-display-test.sh /usr/bin/timeout 30 target/release/corridorkey-ui --screenshot target/test.png --screenshot-after 2
```

The opt-in RAM test needs a real project with at least 129 Input frames. The separate `decode` tests use
`CK_REAL_PROJECT` and `CK_REAL_REFERENCE` (an independent Python OpenEXR statistics dump), passed through
`/usr/bin/env` inside the same wrapper. Real media is never committed.

`tests/fixtures/*.exr` are **synthetic** 64×32 gradients (not footage) written by `tools/gen_fixtures.py` with the
repo's Python OpenEXR: Y/float, RGB/half and RGBA/half × DWAB, PIZ, ZIP, none. Rust cannot *encode* DWAB, hence Python.
Loader tests build temporary projects at run time and prove the loader changes no byte or mtime.

## What it does

- Reads v1/v2 projects: `project.json`, `clips/*/clip.json`, `removed_clips`, `.corridorkey_session.json` (project
  root or clips dir; input-colour map, params, output config), `.video_metadata.json` (fps/size), manifests,
  keyframe sidecars (strict port of the Python parser incl. stale stems; reported, never repaired), `AlphaHint/`, `VideoMamaMaskHint/`.
- Source precedence as in the Qt scanner: non-empty `Frames/` → `Input/` → `Source/` video (or clip.json original file) →
  external sequence (missing folder = **Error** state with its message) → root `Input.*` video. Clips with no source are
  not listed, as in Qt; the Board notes each skipped clip. The clip's display name (not the folder) keys the output
  join and the session colour map; the folder stays the identity. Natural-sorted stems; the timeline is
  the union of stems over all views, so **holes stay holes** ("No file on disk"); outside the In/Out range output
  views say "Outside In/Out". Output root precedence: clip `output_dir` → project `output_dir/<clip>` →
  `--global-output-dir/<project folder>/<name>` → `<clip>/Output`.
- Decodes PNG (8/16-bit, gray/RGB/RGBA) and EXR (half/float; `Y`, RGB, RGBA; any compression the `exr` crate reads,
  verified here for DWAB, PIZ, ZIP, none) on worker threads; a byte-budgeted LRU cache; stale requests are dropped
  and a result for another frame never replaces the current one.
- Colour/alpha: Matte/Hint are clamped grey (no gamma lift). Colour EXRs are linear→sRGB except the Input view, which
  follows the session's input-colour flag. PNG is shown as stored. RGBA is shown over a checker; Processed EXR is un-premultiplied first unless the manifest says `straight-linear`
  (legacy writer = premultiplied).
  HDR/negative values are clamped and flagged "HDR clipped display".
- RAM preview: Play buffers the whole In/Out loop (A and B views, current colour reading) with a progress bar; with Loop off it buffers the remaining frames in the play direction. A range that cannot fit the cache budget says so (needed vs budget) and streams instead; changing view, A/B, colour reading, In/Out or Loop re-buffers. Failed and oversized frames report an error without endless buffering or preventing playback from stopping at the end.
- Playback: timer at the clip's fps (24 with a visible "fps assumed" note when metadata is missing), forward/reverse,
  holds the last frame while a decode lags and counts dropped frames.
- Set In / Set Out / Clear In/Out and the Input-colour radio work **in memory only**, shown as "edited in memory — NOT
  saved to the project".
- `contracts.rs` holds the range identity (stems, inclusive indices) and the output-location token resolver; the
  Deliver tab shows a *preview-only* resolver (the user's NAS example resolves exactly).

## Settings

The **Settings** button (Board, and the bottom bar of a clip workspace) opens a page for the decoded-frame RAM budget (0 = Auto, half the
available RAM, 1–8 GB), decode workers (0 = Auto), default looping and UI scale; AI and mask defaults are saved but not used yet.
Preferences live in `$XDG_CONFIG_HOME/corridorkey-native/settings.json` (never in project data). UI scale applies on Apply; RAM, workers
and looping apply to the next clip you open, and `--cache-mb` overrides the saved RAM budget. Back/Esc/Cancel discard unsaved edits.

## Not implemented (deliberately)

No AI/Key/Generate hint (button disabled with the reason), no queue, presets, JPEG/TIFF/DPX sequences (refused at import, not playable), Bypass Hint editing,
saving graph layouts, native movie decode (movies are first extracted through FFmpeg), board
thumbnails, or hint painting/SAM/keyframe marking. Output-location templates are not applied to anything.
The Qt global output preference (QSettings) is not read; when a clip would fall to it the Board says so.
Windows is not built or tested. Matte display intentionally differs from the Qt app (no 0.85 gamma lift).
Playback speed for 3200×1800 frames is measured by `ck-bench`, not assumed; see its output for the actual gap.

Current feedback validation and remaining checks: [feedback validation](../docs/design/rust-ui/feedback-validation.md).
Earlier milestone evidence: [native validation](../docs/design/rust-ui/native-validation.md).
At 700×660 the viewer is smaller than 280 px; use a taller window or Focus mode. The 728×900 and 1440×900 layouts were visually checked.
Real OS shortcuts remain unverified; the automated keyboard checks inject egui events.
