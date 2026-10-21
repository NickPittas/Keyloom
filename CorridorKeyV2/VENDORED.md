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
