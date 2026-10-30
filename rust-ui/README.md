# keyloom-ui — native application developer guide

For current downloads and end-user instructions, use [installation](../docs/install.md), the [user guide](../docs/user-guide.md) and [network rendering](../docs/network-rendering.md). The source repository is public; only the latest online installers are distributed. Build tooling and dated acceptance records do not establish redistribution clearance or clean-machine certification.

A native Rust window (eframe/egui, wgpu backend) that imports
footage or opens an existing CorridorKey project and shows its source and existing outputs in the selected layout:
Batch Board → clip workspace (Media · Hint · Key · Review · Deliver) with a persistent 7-node graph, a Properties panel,
and one shared viewer (mode chips top-right, A/B wipe top-centre, icon transport centred, scrubbable timeline,
Set In (I) / Set Out (O) / Clear In/Out on every page).

**Import**: *Import clips…* (movies, multi-select), *Import folder…* (movies, plus image
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
so partial frames never look playable. Companion alpha/mask movies are copied at import; extracting them into hint frames is done from the Hint page (*Import alpha / mask movie*, through the worker's validated normalization).

Settings writes a separate user preferences file. **Key** runs in a real Python worker process (`backend.worker` over stdio) — inference runs outside the UI process: result files appear only when a worker publishes them and its manifest says so. The Key tab queues the open clip confined to its In/Out; the Board queues selected clips as a batch (each clip its stored In/Out, else the whole clip) after validation, showing **Review batch** only when decisions are needed (OK / Skip / new version; never a silent overwrite). The Machines/Waiting queue tracks assigned and unassigned work;  one job runs at a time per worker target. A submission persists its options and result folder (`workflow`, `output_values` and `key_result` in `clip.json`). Saving a Deliver output location writes `project.json`. In/Out, input colour, hint settings, annotations and the workspace (open clip, page, view, frame, graph layout) are saved in the project (see *Saved state*); the queue history is saved and anything running at shutdown comes back as *held*, never restarted. An unknown output or hint lock found on disk requires explicit confirmation in the UI before release.

## Run

```sh
cd rust-ui
cargo run --release -- /absolute/path/to/project   # or pick a folder in the UI
cargo run --release                                                                     # landing: Import clips… / Import folder… / Open project…
bash tools/virtual-display-test.sh /usr/bin/cargo run --release -- --data-dir /tmp/ck --import /absolute/path/a.mp4 --auto-confirm --screenshot target/import.png # isolated QA
```

Developer launches require explicit approval; keep QA/tests inside the isolation wrappers below and use temporary fixtures, never user projects. Build installers only through the approved canonical command in [packaging](../packaging/README.md), on the target OS.

Useful options (all optional; see `--help`): `--global-output-dir D` (stands in for the former Qt global output
preference, which is not read), `--cache-mb N` (frame cache budget in MB; default = half of the available RAM, 1–8 GB), `--open-clip NAME`,
`--tab media|hint|key|review|deliver`, `--view input|hint|fg|matte|comp|processed`, `--ab VIEW`, `--frame N`, `--play`,
`--window WxH`, QA options `--keys i,Shift+Right,click:605x66,rclick:605x66` (synthetic egui events, one per 400 ms, state printed to
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
bash tools/virtual-display-test.sh /usr/bin/timeout 30 target/release/keyloom-ui --screenshot target/test.png --screenshot-after 2
```

The virtual-display wrapper runs a private Xvfb at 1440x900; `CK_DISPLAY_SIZE=2400x1500` (still private, still
its own Xvfb) lets an evidence run use a larger window, e.g. `--window 2240x1440`.

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
- Source precedence retained from the former Qt scanner: non-empty `Frames/` → `Input/` → `Source/` video (or clip.json original file) →
  external sequence (missing folder = **Error** state with its message) → root `Input.*` video. Clips with no source are
  not listed, as in the former Qt app; the Board notes each skipped clip. The clip's display name (not the folder) keys the output
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
- Set In / Set Out / Clear In/Out and the Input-colour radio are saved to the project as you edit (see *Saved state*).
- `contracts.rs` holds the range identity (stems, inclusive indices) and the output-location token resolver; the
  Deliver tab edits those templates and saves them to `project.json` — every new Key job of the project resolves
  its output paths through them (default: the clip's own output folder, `{name}_{version}/{output}/{name}_####`).
- Workers answer a `hello` with GPU, model SHA-256 and capabilities before any job; the first local worker's digest
  is the trusted reference and a remote job runs only on a matching digest (unverified models need an explicit
  opt-in). Outputs are published atomically with no-clobber; Cancel, *Resume* and *write as vNNN* come from the real
  disk state.

## Settings

The top-bar **Settings** button opens a page for the decoded-frame RAM budget (0 = Auto, half the
available RAM, 1–8 GB), decode workers (0 = Auto), default looping and UI scale, plus the **AI worker targets** and the AI defaults that seed a new clip's Key options;
mask defaults are saved but not used yet.
A target is a local checkout (repo folder, optional Python, GPU index); an empty list =
one implicit local worker on GPU 0 found via `$CK_REPO` or near the program/working folder. All local targets are assumed
to see the same shared storage (no transfer copies); other PCs are used through **Clients** (top bar, network connections), and a remote job needs the local worker's verified model digest first.
Preferences live in `$XDG_CONFIG_HOME/keyloom/settings.json` (never in project data). UI scale applies on Apply; RAM, workers
and looping apply to the next clip you open, and `--cache-mb` overrides the saved RAM budget. Back/Esc/Cancel discard unsaved edits.

## Hint, Media, Deliver and recovery

- **Hint page**: routes Import alpha / GVM / BiRefNet (all variants) / Chroma key / MatAnyone2 / VideoMaMa, Bypass (keeps settings and assets), SAM tracker model, SAM points / one box / paint on the Input frame (source-pixel coordinates), preview, track mask, sparse keyframe marks, a separate chroma holdout (`holdout_strokes.json`, not SAM paint), chroma colour pick. Generated hints are staged outside the clip and published by the worker, replacing only the frames of In/Out; the previous hint stays until the new set validates. Route availability comes from the worker's real model readiness.
- **Media**: frame extraction, relink of a missing image sequence (stored In/Out and result stems must match) or movie (size, rate and frame count checked first; a partial match needs your confirmation), companion alpha/mask movie import, and companion alpha/mask **image-sequence** import (PNG/EXR, stems/size/In-Out coverage checked, existing hint frames never replaced; the clip's hint lock is held during the copy).
- **Key**: one-off and batch review, hint steps chained ahead of Key (explicit prerequisites), *Temporal awareness* labels over the exact wire presets, **Replace** of an app-owned result (same files and range; confirmation required; needs a worker advertising `key_modes: replace`), New version and Resume (Resume is unavailable when a pipeline regenerates hints).
- **Deliver**: one row per enabled output, independently set to an EXR/PNG image sequence or a video. MP4 offers H.264/H.265, MOV offers ProRes 422/4444, and WebM offers VP9; each video keeps its own codec and CRF/qscale. EXR compression is shared. Videos never overwrite existing files and contain no audio. “Export existing Key result” encodes videos without running Key again.
- **Batch presets**: configure Hint, Key and Deliver, then save the workflow preset from Key or Deliver. On the main batch screen choose **Preset**, select clips and click **Key selected…**. Valid shots queue directly; the review appears when decisions are needed. Each clip uses its own range, input colour, client, prompts and marks. Hints disabled means Key only. SAM and Chroma pre-mattes must already cover that clip's required frames; missing ones block the clip with a precompute instruction. GVM and BiRefNet generate pre-mattes automatically. Direct uses that pre-matte as the hint; MatAnyone2/VideoMaMa refine it first. Key runs next, followed by every video delivery, in order. Image sequences are the Key outputs. A default output location uses each clip's own output folder; applying it resets a project-wide custom location. Version 2 single-video presets remain readable; legacy parameter/output presets are labelled partial.
- **Batch queue**: the **Machines** tab groups assigned shots and their Pre-matte, Hint, Keying and Export stages by PC. Meters show CPU/RAM/GPU/VRAM; earlier runs are collapsed into logs. **Waiting (N)** lists Automatic shots still awaiting a PC, with Pause/Resume/Cancel controls. Progress comes from the worker; multiple exports share an Export row. Details retains local recovery controls. See the [current queue contract](../docs/network-rendering.md#6-watch-the-batch-queue).
- **Saved state**: `clip.json` (`in_out_range` plus boundary stems, `ck_native` hint route/annotations), the session file (input colour), `project.json` (`ck_native.workspace`), `presets.json` beside the preferences, `.ck_queue.json` in the project. Unknown keys are preserved; unreadable files are set aside, not overwritten.
- **Whole-shot scheduling** can target this computer or a connected PC, with hint tasks, Key and exports ordered on that receiver. Automatic takes the next eligible waiting shot as a PC becomes free. A running receiver retains output ownership across reconnects; loss of contact is not proof that writing has stopped.

## Limits and acceptance evidence

Current Linux and Windows online installers are described in the [installation guide](../docs/install.md); the old release/acceptance records describe their dated snapshots, not universal certification. See the [local acceptance record](../docs/design/rust-ui/local-workflow-acceptance.md) for its exact tested platforms, workflows and limitations.

JPEG/TIFF/DPX sequences are refused at import. Native annotations are not shared with the former Qt app's `annotations.json`; movie relink references the movie in place rather than copying it. Exports do not retain audio. Former Qt global output preferences (QSettings) are not read. Matte display intentionally differs from that old app (no 0.85 gamma lift). Board thumbnails and whole-shot network scheduling are implemented. Playback speed on 3200×1800 frames is measured by `ck-bench`, not assumed. Arbitrary production-media or Windows/NAS compatibility is not established by synthetic checks. The [standalone release plan](../docs/design/rust-ui/standalone-release-plan.md) is a historical engineering plan, not a current installation guide.

Evidence: [feedback validation](../docs/design/rust-ui/feedback-validation.md), [native validation](../docs/design/rust-ui/native-validation.md), [remote validation](../docs/design/rust-ui/remote-validation.md) (synthetic 256x256 clips only).

At 700×660 the viewer is smaller than 280 px; use a taller window or Focus mode. The 728×900 and 1440×900 layouts were visually checked.
Real OS shortcuts remain unverified; the automated keyboard checks inject egui events.
