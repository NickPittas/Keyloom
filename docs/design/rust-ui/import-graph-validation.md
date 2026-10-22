# Integrated import and graph validation — 2026-10-22

Parent verification of the integrated native build, before the final review-polish pass:

- `cargo test --locked --offline`: 62 tests passed (39 library, 3 decode, 4 import, 13 loader, 1 navigation, 2 viewer); 2 real-media tests ignored in this run.
- Python `tests/test_ffmpeg_tools.py` and `tests/test_project.py`: 117 passed.
- Existing Projects baseline: 5,417 files, zero added, removed or changed; JSON hash verification passed.
- Rendered the native landing screen at 1440×900. Import clips, Import folder and Open project have visible fills and borders.
- Seeded an import plan with a synthetic 48-frame movie, then clicked the visible **Import 1 clip** button using an injected egui pointer event. No `--auto-confirm` was used. The import completed and opened Media with the actual extracted frames.
- Inspected the 1440×900 workspace: centered transport, timeline, readable controls, visible branching node cards and connections.
- Inspected Hint at 728×900 with Input displayed: all seven nodes are visible and the image area is about 291 px tall. Found header text collision (Deliver/Properties) and Fit overlapping the FG card. These have been sent for correction; this is not final narrow-layout acceptance.

Evidence images on this machine:

- `/tmp/ck-parent-import-landing.png`
- `/tmp/ck-parent-import-confirm.png`
- `/tmp/ck-parent-import-media.png`
- `/tmp/ck-parent-import-narrow.png`

Independent code review: [import-graph-review.md](import-graph-review.md). No blockers. The follow-up fixed cancellation-before-copy, selection identity on reload, per-frame disk reads and trailing-slash normalization.

## Final follow-up

- Parent reran all 6 import and 4 navigation tests after the fixes; all passed. These include cancellation at spawn, a folder ending in a separator, selection after reordered discovery, cached Board summaries, Back navigation and layout geometry.
- Implementer reports the full Rust suite, release build, fmt and clippy clean, plus 117 Python tests passing. The parent did not repeat the full suite after this follow-up; it reran the affected integration tests above.
- Parent captured the actual final release window at 728×900 in `/tmp/ck-parent-final-narrow.png`. Properties and Focus wrap without colliding with Deliver; Fit no longer overlaps an output node; all seven nodes remain visible. The available image area is smaller at this width with the wrapped header; Focus remains available to expand it.
- Rechecked the existing Projects baseline after final QA: 5,417 files, zero added, removed or changed, with JSON hashes preserved.

Limits: the pointer event above is injected into egui, not an OS mouse click. Native file dialogs and drag/drop were not exercised. Screenshot mode requests extra repaints, so these screenshots do not prove helper-free playback timing. No AI execution or editable graph topology is implemented in this slice. Settings and graph layout remain in memory.
