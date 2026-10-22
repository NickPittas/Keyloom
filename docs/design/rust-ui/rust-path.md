# Rust rewrite: implementation path for the selected interface

The user selected **Batch Board + Page Tabs + persistent Node Graph** on 2026-10-22. Batch Board is the home screen for importing one or many clips. Opening clip settings navigates into Page Tabs; a graph remains beneath every clip page. Media, Hint, Key and separate export nodes expose their properties. Workflow and export presets can be saved and reused across a batch.

This is a proposed migration path, not an approved backend architecture or a Rust implementation. The interface decision is made; playback backend and toolkit selection remain evidence-gated. Keep the current application working while proving the new pieces. `selected-design.md` and `preset-contracts.md` provide the combined design and data details.

## First milestone: one real shot in the chosen layout

Build a native Rust window with the selected layout, open an existing project without changing it, and play its source and rendered outputs in sync. Support source, alpha hint, foreground, matte, composite and processed views. Preserve frame identity across missing output frames, aspect ratio, frame rate, source colour interpretation and straight/premultiplied alpha behavior.

That slice should answer the practical questions before a broad rewrite: does the layout work at the user's display size, are matte edges shown correctly, and is seeking/playback responsive with actual media?

## Playback selection: measure before choosing

Keep both user-suggested projects in consideration:

- [FilmCraft](https://github.com/storytold/filmcraft): investigate reusing its media, seeking and scheduling components at a pinned revision.
- [EffectCraft](https://github.com/storytold/effectcraft): investigate its footage/image-sequence and preview-cache components at a pinned revision.
- Compare with a small sequence viewer using published Rust decoders and a bounded frame cache. Do not build a compositor or NLE that this app does not need.

The research report records source findings and uncertainties. A dependency does not need to be on crates.io to be reusable. What matters here is whether the useful crates build independently, cover the actual files and can be maintained without taking unrelated application machinery.

Test the same media and machine for every candidate:

| Check | Acceptance evidence |
| --- | --- |
| File compatibility | Real PNG, single-channel matte EXR, RGB foreground EXR, RGBA processed EXR and movie sources; every compression setting emitted by the current writer |
| Timing | Correct source frame rate; deterministic stepping; source/result synchronization by frame identity; holes remain visible rather than shifting other frames |
| Playback | Cold and warm cache frame delivery, dropped frames, sustained source-rate playback or clearly stated proxy/cache requirement |
| Seeking | Measured median and tail latency during jumps and scrubbing; stale decode requests cannot replace a newer requested frame |
| Memory | Bounded CPU cache; measured peak GPU memory; behavior while the CUDA worker is active |
| Colour and alpha | Linear/sRGB reference images, grayscale matte display, transparent edges, negative/HDR values and no accidental double gamma |
| Platforms | Local Linux/NVIDIA first; Windows build and representative playback before claiming Windows parity |

Record resolution, frame rate, codec/compression, clip length, hardware, revision and cache state with each result. No backend has been benchmarked in this design task.

## Proposed ownership boundary

A staged approach would move the desktop interface, playback, project navigation and job supervision to Rust first. A local Python worker would temporarily retain CorridorKeyV2 and the hint models. This is a Rust application with a Python inference dependency; it is not a claim of an entirely Rust implementation.

If a Python-free final application is required, add a separate feasibility milestone for the full model/runtime stack. Compare outputs and temporal behavior against the current engine, including hint modes, cancellation, memory consumption and model availability, before replacing it.

Start with one explicit worker protocol instead of embedding UI logic into the model runtime. A possible transport is versioned JSON lines over stdio; diagnostics go to stderr and media stays in files. Messages need request/job identity, accepted/rejected responses, phase progress, warnings, terminal outcomes, cancellation and worker-exit recovery. Do not introduce a network service unless a later requirement needs one.

Only one layer should schedule GPU jobs. During the first integration milestone, the Python worker retains the existing queue and Rust submits commands and displays its authoritative state. At the later queue-parity milestone, ownership can move to Rust and the worker accepts exactly one active job with no independent pending queue. That transfer is explicit; do not run two schedulers. The design brief describes the proposed final ownership, not the first integration step.

Job requests carry the frozen range identity and the fully resolved output paths. The worker revalidates them and never resolves templates itself. Terminal worker events must report the output root, published frame identities/count, and manifest completeness, including after cancellation or failure. On a crash during writing, rescan the manifest and files: an incomplete manifest stays incomplete even if all files appear present. Offer a compatible full-range Resume or a clearly labelled re-key after changed settings; never mark a shot Complete solely from a file count. Already published files remain available for review.

## Compatibility checklist for implementation

- Reuse `InferenceParams` and `OutputConfig` validation rules in `backend/service/core.py`, including hint modes and temporal-window values.
- Preserve projects, clip metadata, `.corridorkey_session.json`, hint keyframe sidecars, annotations and output manifests. Read older data first; use explicit versioned migration and backups if a write format changes.
- Carry forward input colour overrides, custom output folders and source reference/copy settings.
- **Changed (user-authorized 2026-10-22):** In/Out is the processed range for Hint, Key and Export. Today Python infers the whole clip and limits only publication (`InferenceMixin` docstring, `_v2_publish`). Hint generators already read only `_selected_sequence_files`. The target confines Key inference and temporal context to `in..out`, which is an engine change in R0 with golden tests that no out-of-range frame is read. Keep compatible Resume rules (now also keyed on range identity and resolved paths), sequential jobs and cooperative cancellation. See `range-output-contracts.md`.
- Output location: today `ClipEntry.output_dir` resolves clip override → project folder/`<clip>` → global/`<project>`/`<clip>` → `<clip>/Output`, with fixed `FG|Matte|Comp|Processed/<stem>` subfolders (`ensure_output_dirs`). The target adds the root + tokenized folder/file templates, sanitized token values, traversal rejection and blocking collision checks across outputs, clips, queued jobs and existing manifests. Legacy projects keep the old layout.
- The selected design proposes a stricter compression compatibility check. Today's manifest omits compression: version it to record `exr_compression` before promising that Resume cannot mix compression settings. Treat legacy unknown compression as requiring a labelled full rewrite under the new rule; test this separately from the existing parameter equality checks.
- Preserve unreadable or unsupported-version preset data before replacing a store. Dependent Key jobs must not start after a failed/cancelled hint job, and dispatch must validate assets against the frozen request.
- Freeze the serialized parameter keys, defaults, values and output settings. Resume uses structured dictionary equality in the Python backend, not raw JSON byte equality; include a cross-language golden check that Rust requests deserialize to the same parameter/output data and marked frame identities. Whitespace/key order may differ; settings may not silently change. Resume still runs AI inference over the (now range-confined) frames and skips only compatible file publication.
- Keep existing mask painting, imported alpha and chroma-key hint workflows in the parity inventory. Simplifying their presentation does not authorize removing them.
- Build on the behavior already covered by `tests/test_v2_outputs.py`, `test_v2_hint_resume.py`, `test_frame_index.py`, `test_preview_playback.py`, `test_session.py` and related tests; add protocol and Rust boundary tests where new behavior lives.

## Sequence after selection

### Later AI performance requirement (user feedback, 2026-10-22)

When the AI worker is integrated, evaluate SageAttention and other compatible attention/inference accelerators against the existing backend on this machine. This is deferred until after the native import/viewer fixes. Measure end-to-end latency, steady-state throughput and peak VRAM with identical models, inputs, ranges and settings; compare output mattes and temporal stability, particularly hair, motion blur and fine edges. Prefer accelerators that preserve quality. Any measurable quality tradeoff must be disclosed and agreed before becoming the default. Keep the baseline backend available when an accelerator is unsupported or fails; do not assume SageAttention supports every model or GPU path.

1. Refine the chosen layout with real empty, importing, running, stopping, failed, partial-output and completed states.
2. Run the playback comparison and choose the smallest proven backend/toolkit combination.
3. Implement the read-only Rust viewer slice against existing projects.
4. Add the supervised inference worker and one end-to-end import → key → review → export path.
5. Complete hint/mask, queue, settings and recovery parity; verify representative projects before retiring Qt.
6. Evaluate the remaining Python dependencies separately if a completely Rust-native application is the target.

No toolkit or playback library is selected yet. See `research.md` for the investigation and `design-brief.md` for the layout rationale.
