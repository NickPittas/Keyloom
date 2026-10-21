# Vendored CorridorKey 2.0 engine

- Source: https://github.com/nikopueringer/CorridorKey2.0
- Commit: 485f5e9 (copied 2026-10-21 from a local checkout)
- Copied unchanged: `corridorkey/`, `third_party/MoGe/`, `scripts/download_weights.py`, `LICENSE`, `THIRD_PARTY_NOTICES.md`
- Added: `__init__.py` (empty namespace boundary), this file
- License: Corridor Key Licence v2.0 (CC BY-NC-SA 4.0 plus additional terms) — see `LICENSE`. Private/personal use fork.
- No weights or model assets are stored here; they live in the app data dir (`~/.local/share/EZ-CorridorKey2/CorridorKeyV2`).

## Dependency deviation
- Upstream `requirements.txt` lists `opencv-python`; this app uses a single `opencv-python-headless` install.

## Local patches to vendored code
1. `corridorkey/video_io.py` — `ExrSequenceWriter` failure-safe queue/shutdown (EZ-CorridorKey2).
   - Worker keeps draining the queue after the first error (stored in `self.err`) until the sentinel, so producers never block on a full queue.
   - New `_put()` enqueues with 0.1 s timeout loops and raises `self.err` (or RuntimeError if the thread died) instead of hanging.
   - Writes frames with pool worker threads (`workers`, default min(8, cpu_count//4)), one queued item per frame, queue bounded to `workers` frames; same file names/content. Reason: single-threaded float ZIP EXR writing (~2 s/frame at 4K) stalled stage 2.
   - `release()` is idempotent; first call joins and re-raises the stored error, later calls are no-ops.
   - Unchanged: naming `{frame_name}_{idx:06d}.exr`, half clamp ±65504, ZIP compression, dtype/channel validation.
   - Tests: `tests/test_v2_exr_writer.py`.
2. `corridorkey/runtime.py` — optional progress/cancel hooks + independent cleanup (EZ-CorridorKey2).
   - New keyword args `on_progress(phase, current, total)` and `check_cancel()` (default None) on `run_inference`, `precompute_rvm_to_memmap`, `precompute_foundations_to_memmap`, `hann_stage1_stream`.
   - Phases: `rvm`, `vfm` (per prepass chunk), `stage1` (per Hann window), `stage2` (per written batch, both normal and low-VRAM paths via `write_prediction_batches`).
   - `check_cancel()` runs at the top of every prepass chunk, Hann window and stage-2 batch; the caller's exception propagates untouched.
   - New `_run_cleanups(steps, body_ok)`: each writer release, reader close and memmap removal runs independently; cleanup errors never mask an in-flight exception, and are re-raised only when the body succeeded.
   - No algorithm/math/colour changes. Tests: `tests/test_v2_runtime_hooks.py`.
3. `corridorkey/runtime.py` (+ log line in `video_io.load_initial_hint_frame`) — indexed reference hint (EZ-CorridorKey2).
   - New `InferenceConfig.initial_hint_index: int = 0` and keyword-only `initial_hint_index` on `hann_stage1_stream`. The initial hint is injected into slot `k - t0` of every Hann window with `t0 <= k < t1` (same value/quality/presence as upstream's frame-0 injection). Order kept: dense fill → carry → initial override → `-1` empty-window sentinel.
   - `run_inference` validates `k` (int, not bool, `0 <= k < T_all`) right after the source frame count is known, before writers/memmaps are created; raises `ValueError`. Settings log gains `Initial Hint Index (0-based): k`; the hint load log is now index-neutral ("loaded reference hint").
   - Why: EZ lets the user pick any frame as the reference matte. Default `k=0` is identical to upstream behaviour.
   - Tests: `tests/test_v2_reference_hint.py`.
4. `corridorkey/video_io.py` — index-preserving sparse hint reader (EZ-CorridorKey2).
   - New `SequentialSparseHintFrameDirWindowReader(dir, meta, frame_indices)` and `make_hint_reader(path, meta, frame_indices=None)` (sparse only when indices are given; sparse video rejected). Dir must hold exactly `<absolute_index>.png` for the selected unique in-range indices (no extras, duplicates of one numeric spelling, non-PNG or missing files). Files load via `read_hint_image` (uint8/uint16 normalised, RGBA → alpha), same model resize/padding as `load_initial_hint_frame`.
   - New `.window_with_presence(t0, t1)` → (tensor, per-frame bool list) on all hint readers: dense readers all True; sparse zero tensor/False for unselected slots, loaded matte/True for selected ones (even all-black). Dense `.window()` unchanged; sparse cache holds only selected decoded frames and drops those before the window start.
   - Why: sparse keyframe hints with real absolute timing. Tests: `tests/test_v2_sparse_hint_reader.py`.
5. `corridorkey/runtime.py` — sparse keyframe hints in Hann stage 1 (EZ-CorridorKey2).
   - New `InferenceConfig.hint_frame_indices` (None = dense) and keyword-only `hint_frame_indices` on `hann_stage1_stream`; forwarded by `run_inference` and logged in the settings file. Sparse mode uses `window_with_presence`: only explicit keyframe slots get hint/`hint_quality`/presence; others are zero hint, quality 0. Carry only fills gaps (never overwrites explicit slots); indexed initial hint still overrides; windows with no hint at all keep quality -1.
   - `run_inference` preflights sparse requests (needs a hint PNG directory, non-empty indices) before writers/memmaps; invalid requests raise `ValueError`. Dense/no-hint behaviour unchanged.
   - Tests: `tests/test_v2_sparse_hann.py`.
