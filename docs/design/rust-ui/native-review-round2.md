# Native S1 review: round 2 (independent regression review)

Scope: `rust-ui/`, the first slice of the read-only native viewer. I checked it against `implementation-brief.md`, `native-input-map.md`, round 1 (`native-review.md`) and `native-validation.md`.
I read the actual code and did not rely on the implementor's claims. Deferred full-parity items are not findings: AI, import, save, presets, movie decode and painting.

## Verdict

**Accepted at code level for S1 (the read-only viewer).** Every round-1 P1 and P2 has a real root-cause fix in shared code, and each has a regression test. I reproduced the main ones against Python. I found no new material bug.

Acceptance is still **conditional on the parent's live QA**. Two things remain unproven on a real desktop: native OS keyboard input, and cached playback with no helper repaint (see "Untested").

## Round-1 findings: status

| Item | Status | Evidence |
|---|---|---|
| P1-1: cached playback never redraws | Fixed | `Viewer::tick` returns `clock.is_some()`. Its only call site (`app.rs:540`) calls `ctx.request_repaint()` when it returns true. There is one tick/ui call site, so Focus mode and every tab use the same path. `tests/viewer.rs` caches all frames first (so no decode can wake the UI), then asserts: tick returns true throughout, the playhead advances at about 24 fps, there are 0 drops, playback stops at the end or at In in reverse, and a paused viewer requests no repaint. |
| P1-2: display_name output/session lookup | Fixed | `project.rs:428`: after source resolution, the name becomes the display name, except on the ERROR return (this matches Python). The output join and `color_map` use `name`; the folder stays the identity. **Differential repro**: a temp project with `a_disp` / `display_name "Hero Shot"`, project `output_dir`, and session map `{"Hero Shot": true}`. Rust and Python `scan_project_clips` both give `/tmp/ckrev2/out/Hero Shot`, and Rust's input colour is linear=True from the session override. |
| P1-3: keyframe validation too weak | Fixed | `parse_keyframes` is a line-by-line port of `hint_keyframes._parse`. It checks the version, exact record keys, int/str types, duplicate stems, duplicate indices, range, stale stems and sort order, and validates against the input-file stems. The repro sidecar `{"index":1,"stem":"WRONG"}` now gives the note `Hint keyframes ignored: Stale keyframe 1 …` and no marks. Tests: `keyframe_sidecar_is_strict_like_python`, `stale_keyframe_sidecar_is_reported_and_ignored_on_load`. |
| P2-1: ERROR state, missing source, skipped clips | Fixed | A missing external sequence now gives state Error with "Source folder missing: …", the same as Python. A clip with no source (`c_empty`) is not listed, and the Board note says "Clip not listed (same as the Qt app)". The non-empty `Frames/` rule and the `Input.*` glob are ported. Tests: `missing_external_source_is_error_state_…`, `root_input_video_and_empty_source_dir_…`. |
| P2-2: Processed legacy premultiplied alpha | Fixed | `view_interp` uses `LinearToSrgbPremul` unless the manifest says `straight-linear`. Test: `premultiplied_processed_is_unpremultiplied_before_encode`. Both real projects declare straight-linear, so they are unaffected. |
| P2-3: `####` token bug | Fixed | The hash run is found in the template before expansion (`contracts.rs:229`), and validation already guarantees one contiguous run. Test: `hash_in_value_does_not_confuse_frame_number` (`A#B` gives `A#B/A#B_0007.exr`). |
| P2-4: v1 global output path | Fixed | `g/<project folder>/<name>`. Test: `v1_global_output_is_global_project_name`. |
| Low: malformed EXR kills a worker | Fixed | The decoder rejects short or subsampled channels with `Err` (test `subsampled_or_short_channel_is_an_error_not_an_index_panic`). The worker also wraps `decode` in `catch_unwind` and always clears `inflight`. The test injects a panicking decoder and shows the worker survives and keeps serving frames. |
| Low: decode failures cached forever | Fixed (as designed) | Retry is explicit with `R` only, up to 3 times per frame, never automatic. The UI shows "press R to retry (n left)" or "retries exhausted". The test covers the bound and the no-auto-retry rule. |
| Low: graph Enter skips `switch_tab` | Fixed | Enter or double-click on a node now calls `switch_tab(n.tab())` (`app.rs:631`), the same path as clicking the tab. |
| Docs: default-run, bench caveat | Fixed | `default-run = "corridorkey-ui"`. The README says bench fps exclude GPU texture upload and are upper bounds. |

## Test isolation (`tests/viewer.rs`)

Both tests used `ck-viewer-<pid>`, and each one ran `remove_dir_all` on it. The tests run in parallel in one process with one PID, so one test could delete the other's frames in the middle of a decode. That race was real.

**It is already fixed in the tree under review.** `project(tag, n)` now uses `ck-viewer-{tag}-{pid}`, with the tags `play` and `reverse`, so each test owns its directory.

Stress evidence on the final binary (`target/debug/deps/viewer-fe6a07a374c703e8`):
- 40 back-to-back runs: 0 failures.
- 20 runs with all 32 cores saturated by `yes` load: 0 failures.

The timing assertions are loose enough to stay stable under load: playhead ≥4 in 300 ms, and more than 10 repaint-wanted ticks. The tests still leave their temp dirs behind, which is harmless.

## `--keys` dev helper (QA only, reviewed proportionally)

The helper is acceptable as a dev tool. It is clearly labelled QA in `--help` and the README, and has no production effect when it is unused (the `raw_input_hook` returns early). Three caveats:

1. **It is not evidence for helper-free playback.** While it runs it forces repaints: 3 immediate repaints after each event, then `request_repaint_after(450ms)`. Any playback seen during a `--keys` run is helped by those repaints. The same applies to `--screenshot`, which repaints every 100 ms. Helper-free cached playback is proven only by the headless `tick` test plus code reading.
2. **Plausible but not reproduced: the first event can stall.** Injection fires only inside `raw_input_hook`, after 2.5 s. Nothing schedules a repaint at 2.5 s, so on an idle Board (nothing decoding, not playing, no `--screenshot`) the first key may wait for real mouse or focus input. The fix is one line: `request_repaint_after` while `inject_i < keys.len()`. I did not launch a GUI to confirm this (see "Untested").
3. Unknown key names and malformed `click:` coordinates are skipped or default to 0 silently. They are still reported as `after "<tok>"`, so a typo looks like a no-op key, not an error. This is cosmetic for a dev tool.

## Remaining low and cosmetic objections (non-blocking)

- **The A/B group may be slightly off centre.** `viewer.rs:389`: the comment says the combo is 100 px wide, but the code reserves 85. The real ComboBox width is 80 plus padding plus the arrow. If it is wider than 85, the group overflows to the right of its centred rect by a few pixels. This needs a visual check in live QA and is not a correctness issue.
- **Transport centring follows the brief.** The six icons are centred on their own rect, and Set In/Out/Clear sits to their right. It moves to a second centred row when the window is under 864 px. The `bottom_h` reserve uses the same 864 threshold, so the image does not overlap the extra row.

## Commands and evidence (this review)

Toolchain: system `/usr/bin/cargo` 1.99.1 with `PATH=/usr/bin:$PATH`, because rustup has no default toolchain after the restart.

- `cargo test --locked`: lib 23, decode 3 (+2 ignored), loader 13, viewer 2. That is **41 passed, 2 ignored**.
- `cargo fmt --check`: OK. `cargo clippy --locked --all-targets -- -D warnings`: clean.
- `CK_REAL_PROJECT=<abs 1080 project> CK_REAL_REFERENCE=/tmp/ck-rust-decoder-reference.json cargo test --release --test decode -- --ignored`: 2 passed.
  - An earlier failure came from my own `$(ls …)` substitution. On this machine `ls` is aliased to eza, which injects colour codes into the path. It was not a code fault.
- `ck-bench inspect` on both real projects gives the same results as round 1:
  - 3 clips, all Complete
  - frames 649 / 122 / 161
  - output root `<clip>/Output`
  - colour from the session override
- Differential parity, temp project `/tmp/ckrev2/p`: Rust `inspect` and Python `backend.clip_scanner.scan_project_clips` agree on:
  - names (`Hero Shot`)
  - states (RAW / ERROR / RAW)
  - the error text
  - output dirs
  - skipping the sourceless clip
- No-write check: `find Projects -newer /tmp/ckr2.stamp` after release inspect and the real-frame tests found **0 files**.

## Untested (left to the parent's live QA)

- Native OS keyboard input in a real compositor: the parent reports that global focus was taken over by the user's browser. Synthetic `--keys` events are not OS input.
- Cached playback with no helper repaint in a live window. It is covered headlessly through `Viewer::tick` and by code reading of the single repaint call site, but not observed on screen.
- The 1440×900 and 728×900 layouts: Hint viewer ≥280 px, Properties side sheet, A/B and transport centring, chip/info overlap at 728.
- The cost of GUI texture upload per frame. Bench fps remain upper bounds, and 3200×1800 was already below 24 fps in the core bench.
- Windows (not claimed by the brief).
