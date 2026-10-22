# Native S1 review (independent round)

Scope: `rust-ui/` read-only viewer against `implementation-brief.md`, `native-input-map.md`, `range-output-contracts.md` and the
Python behaviour it ports. Optional features and parity gaps the brief defers (inference, import, movie decode, SAM, presets) are not findings.

## Checked and fine

- `cargo test --release`: all pass. `ck-bench inspect` on both real projects matches Python `scan_project_clips`: same 3 clips,
  all COMPLETE, frames 649/122/161, output root `<clip>/Output` (not the clip-root `Matte/` DWAB set), and session colour override false.
- The 16 baseline JSON SHA-256 hashes in `/tmp/ck-rust-readonly-baseline.json` are unchanged after this review. `src/` has no write
  path except the dev-only `--screenshot FILE`. eframe persistence is off, so it writes no app-state file either.
- Stale results: `FrameStore` keys by path+interp, and the viewer only draws the playhead's key or the explicitly held one, which
  gets a "showing previous frame" notice. I found no race that would show the wrong frame. The cache budget is enforced.
- Colour: Matte/Hint are grey and opaque, PNG passes through, colour EXR goes linear→sRGB, and the Input view follows the session flag.
  This matches the Python writers for v2 FG/Comp/Processed (`straight-linear`).

## Material bugs (priority order)

### P1-1 Playback freezes when nothing is decoding
`Viewer::tick` (viewer.rs) advances the clock only when egui repaints. Nothing calls `request_repaint` while `clock.is_some()`. The only
repaint source is the decode worker's wake callback.
Repro: open the 1080p clip, step → a few frames so they are cached, press Home, press Space, and keep the mouse still. Once the
wanted frames are all cached, `request()` sends an empty queue, no decode finishes, no repaint happens, and the playhead stops.
Moving the mouse resumes it.
The `--screenshot` path hides this because it calls `request_repaint_after(100ms)` every frame until capture. `ck-bench` runs its own
loop and never calls `tick`.
Fix: in `tick()`, when `self.clock.is_some()`, call `ctx.request_repaint()` (or `request_repaint_after(1/fps)`).

### P1-2 Output root and colour override use the folder name; Python uses the display name
`ClipEntry.find_assets` sets `self.name = display_name` when it differs from the folder (clip_state.py, `get_display_name`).
`output_dir` (step 2 `project.output_dir/<name>` and step 3 `global/<project>/<name>`) and the session `clip_input_is_linear` map
(session_mixin.py:155/161) both key on that name. `project.rs` uses the folder basename for both.
Repro (temp project, `project.json` `output_dir=/tmp/ckrev/out`, clip folder `a_disp` with `clip.json {"display_name":"Hero Shot"}`):
Python gives `/tmp/ckrev/out/Hero Shot` and Rust gives `/tmp/ckrev/out/a_disp`.
Effect on any renamed clip: the wrong or empty output set is shown, COMPLETE/Board state is wrong, and the input colour is wrong.
Fix: keep the folder as identity, but use the display name for the output join and the session-map lookup.

### P1-3 The keyframe sidecar is not strict
`parse_keyframes` checks only the top-level key set. Python `_parse` (hint_keyframes.py) also rejects:
- version ≠ 1
- records that are not exactly `{index, stem}`
- non-int values
- out-of-range indices
- duplicates
- unsorted records
- **stale stems** (`stems[idx] != stem`)

Repro: sidecar `{"version":1,"keyframes":[{"index":1,"stem":"WRONG"}]}`. Python reports "Stale keyframe 1: sidecar stem 'WRONG' !=
source stem 'f_001'" and keeps no keyframes. Rust shows mark `[1]` with no note.
Fix: port `_parse` against `input_files` stems. No test covers anything beyond the extra-key case.

## Medium

### P2-1 No ERROR state, and clip-list differences
A missing external sequence (`source.type=sequence`, `original_path` gone) gives Python ERROR "Source folder missing: <path>".
Rust gives Raw, "No source frames found", and no note. The brief's T2 lists ERROR.
Separately, a clip folder with no Frames/Input/Source/external source makes Python raise `ClipScanError`, so it is **not listed**.
Rust lists it as Raw, so the Board clip count differs from the Qt app.
Repro: `d_ext` and `c_empty` in the temp project above.
Further deviations that don't matter yet:
- `Frames/` counts as present in Python when the directory is non-empty with any files, but in Rust only when it holds images.
- Rust has an extra `!copied && original_path` video fallback that Python does not have.
- The legacy `Input.*` glob is missing.

### P2-2 Processed EXR alpha convention is read but ignored
The legacy writer `_write_outputs` writes Processed EXR as **premultiplied linear** (frame_ops.py:368–371). The v2 manifest says
`straight-linear`. `decode.rs` always treats RGBA as straight, and `processed_exr_convention` is only printed in Properties.
On legacy or manifest-less outputs, edges get alpha applied twice (too dark).
Fix: unpremultiply unless the manifest says `straight-linear`. The real local projects are straight-linear and are unaffected.

### P2-3 The resolver puts `####` after token expansion
`resolve_path` runs `ft.find('#')` / `matches('#').count()` on the *expanded* file name. `sanitize` does not replace `#`.
Example: name `A#B` with file template `{name}_####` expands to `A#B_####`. The code takes start=1, n=5 and produces
`A00001##.exr`, a wrong path with no error.
Fix: find the hash run in the template before expansion, or reject/sanitize `#` in values.

### P2-4 v1 global output path
`--global-output-dir` for v1 gives `g/<name>`. Python (clip_state.py:373–380) gives `g/<basename(root)>/<name>`. This is CLI-only today.

## Low

- **A malformed EXR can kill a decode worker.** `sample(s, i)` indexes `0..w*h` for every channel. A subsampled channel
  (x/y sampling ≠ 1) panics with an out-of-bounds index. That worker thread dies, its key stays in `inflight`, and the frame shows
  "Decoding…" forever. Enough such files and decoding stops entirely.
  Fix: return `Err` when a channel's sample count ≠ w·h.
- **Decode failures are cached for the session.** `FrameStore.errors` is never cleared. A frame the Qt app is writing at the same
  time keeps showing "Decode failed" until the clip is reopened.
- **Enter on a graph node skips `switch_tab`** (app.rs `graph`). It sets `self.tab` directly, so Enter on the Hint node opens the
  Hint tab while the viewer stays on Input. Clicking the tab does switch the view.

## Tests that give false confidence

- No test drives `Viewer::tick`, so P1-1 is invisible. Screenshot evidence repaints every 100 ms and masks it.
- `seek_storm_only_last_target_is_current` asserts `target == last`, but `request()` sets that unconditionally. Only the final
  Ready check means anything.
- `real_project_frames_decode` returns `Ok` when `CK_REAL_PROJECT` is unset, so `cargo test -- --ignored` "passes" without
  decoding anything. It checks only size and finiteness. The Python channel-mean match was a manual check, not a test.
- The loader no-write test covers load + `FrameIndex::build` on empty placeholder files only. There are no tests for display-name
  output roots, stale keyframes, a missing external source, or premultiplied Processed.
- `ck-bench` measures decode only. The GUI also uploads each frame as an RGBA8 texture on the UI thread: about 23 MB at 3200×1800,
  doubled with A/B. Read the reported fps (16.7/17.9/22.4 vs 24) as an upper bound for the GUI, not a measurement of it.
