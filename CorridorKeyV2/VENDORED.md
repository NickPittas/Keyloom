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
(none yet)
