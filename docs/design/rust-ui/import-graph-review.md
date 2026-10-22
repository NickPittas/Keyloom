# Import → Board → workspace + node canvas: independent review

Scope: `rust-ui/src/import.rs`, `src/ui/{import,app,graph,theme,viewer}.rs`, `src/main.rs`, `tools/ck_import.py`,
`tests/{import,navigation}.rs`, the `backend/ffmpeg_tools/metadata.py` change (`build_video_metadata`) and the
`ui/workers/extract_worker.py` change. I checked them against `import-implementation-brief.md` and `node-canvas-brief.md`
(the newest notes win), with `native-review-round2.md` as context. I read the code and did not rely on the
implementor's claims. All repros used temp dirs only (`/tmp/ckrev-*`). No real project was opened. Nothing in the
repo was edited except this file.

## Verdict

**Accepted at code level, no blockers.** I found no reachable path that overwrites or deletes user data. Collision
exhaustion is checked before the first write, and project and clip JSON writes are atomic. Partial frames are never
playable, and they reopen as *Resume*. The Back crash is fixed at the root, and its test fails on the original bug.

Four should-fix items remain (S1–S4). Each one has a concrete repro. None of them loses data. S1 and S2 are the
ones a user will notice. The README contradictions (D1) need a doc fix.

## Verified (with evidence)

| Area | Result | Evidence |
|---|---|---|
| Atomic `project.json` / `clip.json` | OK | `backend/project.py:386` / `:408` write temp + `os.replace`. Each function is defined once in `backend/` (grep). `project_media.py` imports the same functions. No later definition overrides them. The sidecar `write_video_metadata` is now temp + replace too. |
| Unknown project keys kept | OK | The `add_*` functions read the whole dict, change `clips` / `removed_clips` and write it back. `tests/import.rs` checks that `future_key` survives and that Python `get_clip_dirs` reads 4 clips. |
| Collision preflight (new + existing + within one batch) | OK | `make_plan`: `taken = os.listdir(clips/)` (hidden and removed folders included), and `free_name` adds each name to `taken`, so items in one batch cannot collide. Exhausted names are skipped before `create_*` runs. The backend's own `_2.._99` loop produces the same names for the same order (videos first, then sequences, as in `create_project_from_media`). A post-create check compares `clip.json` `original_path` before any extraction runs. |
| Corrupt / v1 project | OK | Refused before any write. The test snapshots bytes before and after. |
| Source safety | OK | Movies are copied (`copy2`, and an existing target is never overwritten). Sequences are referenced in place. Companion sequences are symlinked or copied *into* the clip. The test snapshots source bytes before and after. |
| No writes before Import | OK | The dry run spawns `--dry-run`, which returns after `plan`. The test asserts `data/` does not exist. The parent saw the same on screen. |
| Incomplete staging | OK | Extraction goes to `.frames_importing`. Only a complete run (`.dwab_done`) is renamed to `Frames/`, and only after the sidecar is written. On reopen, a video clip with an empty `Frames/` is `NeedsFrames` → *Resume extraction*. No in-memory state is needed. `extract_clip` refuses to touch a non-empty `Frames/`. The cancel → resume test reaches 240/240 frames. |
| Cancel / EOF | OK during extraction | Both the stdin `cancel` line and EOF set the event, and extraction stops. `Drop` sends cancel and closes stdin, and the child is never killed. See S1 for the phase before extraction. |
| UI never blocks on the job | OK | The thread drains stderr. `poll` uses `try_recv` plus `request_repaint_after(100ms)`. `child.wait()` runs only after stdout EOF. See S3 for the per-frame filesystem work. |
| Back crash | Fixed at root | Back is deferred to after the workspace renders (`back_requested`), and `graph`, `page_tools` and `properties` now guard with let-else. **Mutation check** (in a `/tmp` copy): with the original shape (Back clears immediately, `graph()` unwraps), `tests/navigation.rs` **fails** with a panic at `app.rs:691`. With the deferred Back it passes. The test clicks the real `back_rect()` with egui pointer events from every tab and in Focus mode. |
| Graph | OK | 7 typed nodes and explicit edges. `validate()` is unit-tested. Header drag moves a node and a body drag does not. Background drag pans, Ctrl/pinch zooms (clamped), and plain wheel pans only while the pointer is over the canvas. Double-click opens, Enter opens the focused node, and Fit is available. State is kept per clip (`graphs` keyed by folder). Sockets are not drag sources. Events go through `switch_tab`, the same path a tab click uses. |
| Theme | OK | `theme::apply` runs at construction. Every checkbox goes through `theme::checkbox` (CHECKED fill plus white mark). Buttons and fields have a 1 px border. A unit test asserts the contrast values. |
| Build / tests | Pass | `cargo test --locked`: lib, decode 3 (+2 ignored), import 4 (≈4 s), loader 13, navigation 1, viewer 2, all ok. This matches the parent's run. |

## Should fix (reachable, not data loss)

**S1. Cancel (or closing the app) before extraction still creates the project and copies every movie.**
`watch_stdin()` starts after the plan, but `cancel` is first checked before the first extraction. The backend's
`create_project_from_media` / `add_clips_to_project` loop copies every movie without checking it.
Repro: send a `cancel` line, or close stdin with `</dev/null`, at spawn for `new … a.mp4 b.mp4 c.mp4`. Result:
`created` → `cancelled`, and all 3 movies are copied into a new project (`/tmp/ckrev-h/d1`, `d2`). With 10 large
movies, Cancel still copies all of them. The UI text "Stopping after the current step (a file copy already under way
finishes first)" promises less copying than actually happens.
Smallest fix: in the helper, check `cancel` before calling the create/add function. If it is set, emit `cancelled`
and exit with nothing written. To stop between copies, the helper can create clips one at a time. It already owns
the plan, so it can call `_create_clip_folder` / `create_clip_from_sequence` per item and write `project.json` once.
Otherwise, make the UI text honest.

**S2. Board selection follows the index, not the clip, across an import reload.**
`import_tick` saves `self.selected` (a `BTreeSet<usize>`), calls `load()` and restores the indices. Clips are sorted
by folder name, so an imported clip that sorts earlier moves the selection.
Repro by code path: open a project with clip `b`, tick `b`, then import `a.mp4`. After `Created`, index 0 is `a`, so
the new (still extracting) clip is checked and `b` is not. *Open clip settings* then refers to the wrong clip.
Today the only selection action is opening a clip, so the harm is low, but this is the exact case the user named.
Fix: keep the selection as folder names (or map indices through folder names around `load`).

**S3. Board cards scan the disk every frame, at 10 Hz while an import runs.**
`card()` calls `FrameIndex::build(c)`, which runs `list_images` / `read_dir` on every view dir of every clip, every
frame. `ImportUi::poll` repaints every 100 ms for the whole import. For a project with many clips, or one on a NAS,
this is UI-thread I/O 10×/s. I found this by reading the code and did not measure it.
Fix: cache the index per clip and rebuild it only on reload.

**S4. A folder path with a trailing separator makes a hidden orphan clip.**
`collect` passes `d` through unchanged. `create_clip_from_sequence` uses `basename("…/seq/") == ""`, so the
clip name is `""`, which dedups to `_2`. The plan said `seq`. The post-create check stops the import, but by then
`clips/_2` exists and `project.json` lists `"_2"`. The loader hides `_` folders, so the user never sees it.
Repro: `ck_import.py new --name Z --data-dir … --folder=/tmp/ckrev-h/src/seq/`. Result: error, `clips/_2`,
`"clips":["_2"]`.
This is reachable through the QA `--import-folder dir/` option (shell completion adds the `/`). The picker and
drag-and-drop paths don't add a trailing slash.
Fix: normalise every input path once in `collect` (`os.path.abspath`, which strips the trailing separator).

## Doc corrections

**D1. `rust-ui/README.md` contradicts itself.** The header still says "native viewer" and that it "opens an
**existing** CorridorKey project". The "Not implemented" list at lines 98–100 says "saving anything", "FFmpeg" and
"video-only clips show 'native movie decode not integrated yet (staged)'". Import now writes projects, runs FFmpeg
(through the helper) and extracts the frames those clips play. Suggested wording:
- Header: "native project app: import footage into a new or existing project, then review it", and keep the
  "provisional toolkit" note.
- Not implemented: replace "saving anything" with "saving edits (In/Out, input colour, graph layout stay in
  memory)". Replace "FFmpeg" with "native movie decode (movies are converted to EXR by the import helper, which uses
  FFmpeg)". Drop the "staged" sentence, or limit it to Qt clips whose movie is referenced but not extracted (the card
  offers Resume only when the movie is in `Source/`).
- Keep "Companion AlphaHint/MaskHint videos are copied but not extracted".

## Test-coverage notes (not blockers)

- **The UI-level import test bypasses the button flow.** `landing_import_opens_the_single_imported_clip_on_media`
  uses `AppOptions.import_videos` + `auto_confirm`, which reaches `confirm_now()` → `start()`. It never clicks
  *Import N clips*, never edits the name and never uses *Cancel*. The button calls the same `start()`, so the risk is
  small. Still, the user's complaint was about the UI flow. One more test with `auto_confirm: false` would cover it:
  drive frames until `has_pending()`, then click the button's rect (expose it the way `back_rect` is) and assert the
  workspace opens. The parent says it is testing the real confirm button now. The rfd pickers cannot be tested
  headlessly. That is expected.
- No test covers S1 (cancel before create), S2 (selection across reload) or a drop. Dropped files are handled only
  on the Board, so a drop while a clip is open is silently ignored.

## Minor (record only)

- While an import runs, *Cancel import* exists only on the Board. From a workspace the user has to go Back first.
- The real run re-plans and emits `plan`, which the UI ignores. If the project changed between preview and
  Import, the names can differ from what the preview showed. The post-create check keeps this safe.
- `graphs` is keyed by clip folder, not by project plus folder, so two projects with the same folder names share a
  layout for the session.
- On a case-insensitive filesystem, `taken` (case-sensitive) and `os.path.exists` can disagree. The post-create
  check stops before extraction. Windows and macOS are not claimed.
- The brief's Bypass Hint dashes the Hint edge on *bypass*. The app dashes it when the clip has *no hint frames*.
  Bypass is not editable yet, so this is not a defect now.

## Not checked

Live OS input and real dialogs (left to the parent's screenshots and the click test). Windows. A timing measurement
for S3. Python `pytest` (the parent reports 117 passed). I reviewed only that `build_video_metadata` is a verbatim
move of the old inline dict and that the Qt worker now calls it.
