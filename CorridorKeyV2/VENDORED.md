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
   - `release()` is idempotent; first call joins and re-raises the stored error, later calls are no-ops.
   - Unchanged: naming `{frame_name}_{idx:06d}.exr`, half clamp ±65504, ZIP compression, dtype/channel validation.
   - Tests: `tests/test_v2_exr_writer.py`.
2. `corridorkey/runtime.py` — optional progress/cancel hooks + independent cleanup (EZ-CorridorKey2).
   - New keyword args `on_progress(phase, current, total)` and `check_cancel()` (default None) on `run_inference`, `precompute_rvm_to_memmap`, `precompute_foundations_to_memmap`, `hann_stage1_stream`.
   - Phases: `rvm`, `vfm` (per prepass chunk), `stage1` (per Hann window), `stage2` (per written batch, both normal and low-VRAM paths via `write_prediction_batches`).
   - `check_cancel()` runs at the top of every prepass chunk, Hann window and stage-2 batch; the caller's exception propagates untouched.
   - New `_run_cleanups(steps, body_ok)`: each writer release, reader close and memmap removal runs independently; cleanup errors never mask an in-flight exception, and are re-raised only when the body succeeded.
   - No algorithm/math/colour changes. Tests: `tests/test_v2_runtime_hooks.py`.
