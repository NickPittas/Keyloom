# Local workflow acceptance — 2026-10-23

This records observed results, not a claim that every production codec, GPU or operating system has been tested.

## Implemented workflow

The native app imports movies and PNG/EXR sequences, normalizes movie sources, relinks missing footage, and imports companion alpha/mask movies or sequences. Hint routes include GVM, all catalog BiRefNet variants, chroma, SAM preview/tracking, MatAnyone2 and VideoMaMa. Prompt points, boxes and paint use source-pixel coordinates; chroma holdout is separate.

One-off and batch jobs share the worker queue. Saved hint routes are queued as prerequisites of Key. In/Out confines processing and publication. Key exposes temporal awareness (Auto, 80/40, 48/24, 24/12, 8/4 and Off 1/1), output settings, Resume, New Version and explicitly confirmed recoverable Replace. Deliver exports MP4, ProRes 4444 MOV and WebM alpha. Video exports currently contain no audio.

Range identity, input colour, workflow options, annotations, marks, presets and workspace state persist. Interrupted queue entries reopen held, rather than restarting. Model management distinguishes presence, verification and readiness; model leases prevent managed repair during active use. Companion sequence import owns the same hint lock as worker publication and fences Key/hint submissions.

## Linux evidence

Hardware: NVIDIA RTX 4090. All media below is synthetic test footage, not user footage. GUI checks used a private Xvfb display and software Vulkan for the UI; worker inference used the GPU. Keyboard and pointer actions were injected through the app's QA interface, not the live desktop.

- Actual Hint-page SAM preview and tracking completed; the preview overlay was visually checked.
- Actual Hint-page GVM Generate completed two frames at the source dimensions.
- Actual Key-page one-off processing completed two frames with FG, Matte, Comp and Processed outputs.
- Actual Key-page Replace confirmation was visible and completed two frames. The replacement manifest was complete and recorded the previous job ID.
- Actual Board batch processing completed two chroma-hint → Key chains. Both manifests retained temporal window `48/24`; all four output products were present.
- The same two-clip chain completed through the installed Linux app and its bundled Python runtime, with no network access.
- Actual Deliver export completed a two-frame, 24 fps H.264 file. Real-FFmpeg tests cover all six supported product/container combinations, colour values, alpha, no-clobber publication and cancellation cleanup.
- Media controls were rendered at 1440×900 after removing the unrelated Key footer. Input colour and companion-sequence import are visible.
- The staged helper reported all 30 catalog assets Ready, with no missing or partial assets.

The bundled Python 3.11.16 / torch 2.9.1+cu128 runtime completed real worker runs for BiRefNet, GVM, SAM tracking, MatAnyone2, VideoMaMa and Key. SAM2's CUDA extension was rebuilt against the pinned torch and CUDA 12.8 toolkit; its connected-components GPU probe passed. The combined Linux bundle requires glibc 2.30, below the build limit of 2.31.

The pinned Linux FFmpeg 7.0.2 passed 18 export tests and 33 hint-normalization tests. Sixteen source-normalization regressions passed unchanged. One test's synthetic VFR fixture advertised different rates under this FFmpeg version (`50/1`, `0/0`, rather than `25/1`, `25/1`); a separate probe confirmed normalization rejected it without publishing frames. The original equal-advertised-rate regression remains covered by the development FFmpeg run.

Independent reviews signed off source/hint normalization, provisioning and runtime leases, cancellation reporting, recoverable Replace, EXR export, native workflow integration, clip-bound relink and live-import fencing. Rust's final import-lock interoperability test calls the real Python inspection/clear implementation and verifies that both clear reasons refuse a live importer.

## Installer acceptance tracking

The Linux candidate installed into a fresh path containing spaces and Unicode, passed installed-file verification, and launched the full batch workflow. The final refreshed installer `CorridorKey-2.1.5-499c8c7d47-linux-x86_64.run` installed into `~/.local/opt/CorridorKey` with exit code 0. Its receipt records full-profile models Ready. A fresh installed-helper check reported all 30 assets Ready, and installed-file verification reported no problems. This build includes the reviewed download-supervisor EOF ordering fix reproduced during native Windows setup. The installed UI self-check resolves the bundled interpreter and helper tree and the platform user-data directory.

Linux launcher: `~/.local/opt/CorridorKey/bin/corridorkey` (also registered as CorridorKey in the application menu). Installer location on the build machine: `~/.cache/ck-dist-ready-1023/CorridorKey-2.1.5-499c8c7d47-linux-x86_64.run`.

Installer SHA-256: `2b903566bdbb719d98f92fe8df3fbd2bf8c330673676009fc23fab4f101db030`.

The final Windows installer `CorridorKey-2.1.5-4fcdbb4758-windows-x86_64-setup.exe` installed natively to `C:\Users\Nick\AppData\Local\Programs\CorridorKey`. Its install result reports exit code 0, app installed and models Ready. Installed-file verification reported zero problems. Runtime and UI self-checks from `C:\` passed, using bundled Python 3.11.16, torch 2.9.1+cu128 and FFmpeg 8.1.2. The Start Menu contains `CorridorKey.lnk`.

Windows SAM2 uses upstream's fallback without the optional CUDA extension (`sam2_c: false`); the GPU matmul check passed. Real native worker runs completed chroma, BiRefNet, GVM, SAM preview, SAM tracking, MatAnyone2 and VideoMaMa on synthetic frames. A two-clip native batch ran BiRefNet hint generation followed by Key and produced finite FG/matte outputs at 256×256. A fresh installed-helper JSON check reported 30/30 assets verified and Ready, without problems. A final serial Key smoke published 7/7 frames, preserved three published frames on cancellation, refused an existing destination, and exited cleanly on stdin EOF both idle and mid-job, with no owned child processes left. SAM preview also passed serially with no Python processes remaining. An earlier SAM preview attempt failed while another GPU smoke ran concurrently; it did not recur serially and its cause was not established. These are worker tests, not a claim of clicking every Windows UI control.

Windows launcher: `C:\Users\Nick\AppData\Local\Programs\CorridorKey\bin\corridorkey.cmd`, or CorridorKey in the Start Menu. Installer files are in `C:\Users\Nick\ck-local-build-1023\dist4`; keep the `.exe` and `setup-1.bin` together.

- Installer SHA-256: `2159faad475a5979c08a0489815f2c8e00135dd1cb788b6faf8b62a471e77fa6`.
- Data slice SHA-256: `a4d6dd4e788274c7127f46c1d0a5c4b0cbc45e4bd0d02640b36869586ca836b1`.

## Approved UI update — 2026-10-23

The approved layout is installed on Linux as `2.1.5-9a8df9d8f8` through the existing lifecycle, with the previous build retained for rollback. Installation exited 0 and reported models Ready. This update preserves the native timeline/playhead and vertical SAM tools, compacts Board cards, exposes per-clip client choices, and keeps Deliver actions visible. The client inherit entry names the session default when it is a specific client.

Hint consumption now distinguishes explicit clip choices from old session/job snapshots. Saved routes and hint frames derive the consumption mode after reload; explicitly choosing `none` remains authoritative. Saving a new mode refreshes the open clip before subsequent hint generation. A headless interaction regression clicks the actual mode picker, checks the refreshed clip, invokes the post-submit promotion callback, and checks queued Key parameters. It fails without the refresh fix. The fake-worker fixture did not successfully queue via the Generate button; that part of the interaction is not claimed by this regression.

Final isolated Rust suite: 308 passed, 4 ignored, no failures. Targeted backend tests: 111 passed. The compatible release build has glibc floor 2.30. The installed UI self-check resolves its bundled interpreter/helper; an isolated Xvfb render confirms the installed Hint layout. Evidence is under `rust-ui/target/local-acceptance/approved-*`. The private render uses a temporary HOME, so its missing-model banner does not represent the real installation's verified model status. Earlier GPU-chain results above were not rerun for these UI/state fixes. The Windows installation and existing downloadable installer files were not refreshed by this update.

## Reported workflow fixes — 2026-10-23

Linux build `2.1.5-d476889117` is installed, with `2.1.5-9a8df9d8f8` retained for rollback. The existing app and worker were left running. SAM repository IDs from Settings and saved clips normalize to the worker's short keys; GVM validates only its relevant settings. Hint/Guide replacement invalidates both decoded pixels and uploaded textures, and rejects older in-flight decodes. A known active Key lock displays the lock holder's progress instead of orphan-lock recovery instructions.

The SAM tool column is 36 px, with aligned buttons and brush size in Properties. Hint/SAM processing, Key and Deliver actions share the centered bottom action area; settings stay in Properties. The preset-name field has a stable identity, and an interaction test edits it and saves a second preset without replacing the first. A wheel-event test verifies graph zoom under the pointer and no parent scrolling.

Grow/shrink, fill holes and feather are real worker operations saved with hint settings. Guide generation and SAM preview apply grow/fill only; refiners apply feather only, preventing duplicate growth and feather-induced expansion of binary guides. Direct hints apply all three. Changes take effect on regeneration: with a refiner, regenerate the pre-matte for growth/hole changes, then the hint for feather. Imported or manually painted guides are not automatically cleaned.

Final validation: 316 Rust tests passed, 4 ignored; 132 targeted backend tests passed through the repository isolation wrapper. Private Xvfb renders checked the source and installed Hint UI and the Key action placement. The installed binary matches the built SHA-256, installed-file verification has no problems, and its isolated self-check resolves the bundled Python/helper. No real GPU inference was rerun during this pass.

The installer returned 3 because the running worker held model-reader leases on four Key assets; these were not missing. A separate managed verify rehashed 26 assets but could not obtain exclusive locks for those four. The installed helper's offline status check then reported all 30 assets verified and Ready, including the four whose current file identities still match their verified hashes. The receipt's model status was reconciled from that evidence; the original exit-3 install result is preserved. Evidence is under `rust-ui/target/local-acceptance/workflow-fixes-*`. Model warnings in the private-display screenshots reflect their temporary HOME, not the real model roots.

## Full workflow presets and above-graph panel — 2026-10-23

Linux build `2.1.5-e4d10b9a16` is installed; `2.1.5-d476889117` is retained for rollback. Status, progress, marks and overlay occupy the left half of one resizable panel immediately above the node graph. Hint, SAM, Key and Deliver actions occupy its right half. Buttons wrap within their columns at 1440×900. The SAM task ID is deduplicated, and a known Dispatching/Running hint job does not show orphan-lock instructions; foreign, lost and ended-job recovery remains available.

Workflow presets now capture the pre-matte and hint route/settings, Key parameters, enabled outputs and formats, project output root/templates, and optional video export product/container/fps/folder. Save and Apply use the same typed schema as the Batch preset dropdown. Source ranges, colour, client assignments, marks, prompts and masks are not copied. `None` output location means each clip's default; applying it resets the project-wide custom location. Export settings are editable before the first Key result. An unset fps follows each clip's source fps.

Batch selection recomputes frozen per-clip plans and chains producer → refiner → Key → optional export. SAM guides and imported alpha must already cover that clip's required frames; SAM rows explain manual preparation and missing guides block that row. Automatic masks rerun. Hint mode none/bypass runs Key only. Resume is unavailable when hints regenerate, preventing mixed old/new-hint frames in one result. New version and confirmed Replace remain. Output collisions are checked before queueing; export reads the chosen Key plan's exact manifest and never overwrites an existing file. Legacy presets are labelled parameters/outputs only. Missing full-preset fields are rejected rather than defaulted silently.

Final isolated Rust suite: 328 passed, 0 failed, 4 ignored. Coverage includes actual Save/Apply and Batch dropdown clicks, per-clip range/fps/path dependency chains, manual SAM prerequisites, strict parsing, output collisions, bypass and the Resume guard. Independent Opus review approved the corrections. Private Xvfb renders checked Hint, Key and Deliver placement and the installed Hint UI. The installed binary matches the compatible release SHA-256; glibc floor is 2.30, installed-file verification reports no problems, and the isolated installed self-check resolves its bundled Python/helper. Evidence: `rust-ui/target/local-acceptance/pipeline-*`.

The install model step returned 3 because the running worker held reader leases on four Key assets. No worker was stopped. The installed helper's offline status reported 30/30 assets Ready and verified against current file identities; the model receipt was reconciled from that evidence, preserving the original exit-3 install result. Private screenshots use temporary HOME and their missing-model banner does not represent the real installation. No new GPU inference or real-worker pipeline export was run this round; dependency ordering was checked with the fake worker. Python sources were unchanged from the previous 132-test validation. Windows and downloadable installers were not refreshed. One implementation test run reportedly had a settings timing failure; all subsequent settings runs and the independent final full suite passed, and its cause was not established.

## Empty preset name field — 2026-10-23

Linux build `2.1.5-2a48de47c5` changes the preset name editor shared by Key and export presets to an explicitly labelled, fixed-size text field with an absolute widget ID and click-to-focus. It no longer switches to the placeholder-text rendering path when empty. It leaves preset selection, full-pipeline capture and persistent data unchanged.

The reported delete-last-preset failure could not be reproduced before this change. New regressions nevertheless cover saving, deleting the last preset, typing letter by letter into the blank field, saving a replacement, select-all/Backspace and retyping. Empty fields accept clicks near both edges and in the centre. These headless tests use separate hover/press/release frames, scroll Properties into view at 1440×900, and also cover wider/narrower windows. The checks passed before and after the change, so they establish continued supported behaviour, not a confirmed diagnosis of the user's runtime failure.

Targeted isolated checks: 6 action-area tests, 3 pipeline UI tests and 8 settings tests passed. A private Xvfb render confirms the labelled blank input and adjacent Save control. The compatible release build retains glibc floor 2.30. No GPU/backend or Windows verification was needed or performed for this input-field change. Evidence: `rust-ui/target/local-acceptance/empty-preset-*`.

## Confirmed workflow and per-output delivery — 2026-10-23

Linux build `2.1.5-ca712a8ab3` is installed, with `2.1.5-2a48de47c5` retained for rollback. Installer exit 0 reports models Ready. The confirmed preset schema saves pre-matte/final-hint settings, hint mode/quality/carry quality, grow/shrink/fill/feather, temporal awareness, VRAM, despill, enabled products, sequence formats, shared EXR compression, output location and per-product delivery choices. Each enabled product independently delivers as a sequence or video with its own container, codec and compression quality. Frame rate policy, partial-result permission and video destination remain shared. Version 3 requires every enabled output's delivery; version 2 single-video presets migrate without adding video exports for the other products.

The main Board has a visible Preset selector before the batch button; review can change it and recompute the plans. Hints disabled/bypassed queues Key without hint generation. SAM and Chroma require the clip's own precomputed pre-matte (MatAnyone2 needs its In-frame guide; Direct and VideoMaMa need the range). Missing mattes block with a precompute instruction. GVM/BiRefNet generate guides automatically. Direct promotes the pre-matte into AlphaHint; refiners read it and generate AlphaHint first. The worker promotion task validates coverage and image size, leaves source/guide files alone, and uses staged, locked publication. Grow/fill apply at guide generation and feather at promotion/refinement. Guides retain the producer's soft alpha; refiners threshold when reading them. Previously saved binary guides retain their stored pixels.

Key follows the hint step, and every video output follows Key in a sequential dependency chain using its exact manifest folder. Existing paths are never overwritten. Preset hint mode is persisted on submission, so an older explicit none cannot reappear after reload. Resume remains unavailable when a pipeline regenerates/promotes hints. The separate centred Export existing Key result button encodes selected videos without re-keying; it refuses incomplete results unless explicitly allowed, uncovered ranges, foreign/missing clip IDs and missing output products before queueing.

Final isolated checks: 341 Rust tests passed, 4 ignored; 168 targeted backend tests passed using bundled FFmpeg 7.0.2. Real codec tests cover H.264/H.265, ProRes 422/4444, VP9 and gray matte to MOV. A soft-alpha ramp survives GVM → guide → direct promotion unchanged. Tests exercise Save/Apply of every scalar and mixed deliveries, actual Board preset selection, missing/prepared SAM and Chroma guides, automatic producer/refiner chains, Key-only plus multiple videos, hint-mode reload and the existing-result export button with no Key queued. Opus approved the corrections; its minor early-refusal suggestions were implemented and tested. Private 1440×900 renders show the main selector, actual preset selection and review, and Apply restoring FG EXR, Matte H.265/CRF 22, Comp PNG and Processed ProRes 4444/qscale 9. All processing buttons remain in the right region above the graph.

The compatible Rust binary has glibc floor 2.30 and matches the installed SHA-256. Installed-file verification reports no problems, the isolated self-check resolves its bundled Python/helper, and the receipt reports models Ready. Evidence is under `rust-ui/target/local-acceptance/confirmed-*`. Private renders use temporary HOME, so their missing-model banner does not describe the real install. The full inference-to-delivery chain was checked at the scheduler level with a fake worker; no new GPU inference or Windows run was performed. Video export remains audio-free, and very small HEVC frames may be rejected by libx265 (the real HEVC test uses 128×128).

## Grouped batch queue — 2026-10-23

Linux build `2.1.5-077171fe2f` is installed, with `2.1.5-ca712a8ab3` retained for rollback. Installation exited 0, installed-file verification reports no problems, and the receipt reports full-profile models Ready. The installed binary matches the compatible build's SHA-256 and passes the isolated self-check using its bundled Python/helper.

The queue now groups jobs under each clip's title, with ordered Pre-matte, Hint, Keying and Export rows for the stages actually used. Producer/refiner names are shown, manual SAM/Chroma guides say Precomputed, and multiple video exports share an aggregate row with per-product statuses. Image sequences remain Key outputs, without an invented export task. Separate runs of the same clip do not mix progress, and identical folder names in different projects remain separate groups. Active/unresolved clips are retained; completed history is limited by whole clip sections.

Percentages use worker counts and reach 100 only after successful completion. Waiting, Queued, Starting, Failed, Cancelled and Lost remain explicit. Inline Cancel and the existing Details/recovery controls remain available. A finished cancellation no longer forces Details open. Scheduling, preset and backend processing semantics are unchanged.

The isolated Rust suite passed 349 tests, with 4 ignored. After final width/cancellation adjustments, the six grouped-queue integration tests passed again. Independent Opus review found no blockers. Private 1440×900 Xvfb renders exercised an actual two-clip queued chain with a fake worker held at 50%: both clip sections, four ordered stages, export product statuses and Cancel buttons are visible without right-edge clipping. Rows use a single line when the panel is wide enough and stack the name above progress when resized narrower. Compatible release build glibc floor: 2.30. Evidence: `rust-ui/target/local-acceptance/grouped-queue-*`. This is UI/scheduler validation with synthetic fixtures; no new GPU inference, backend or Windows run was performed.

## Limits

Production-resolution throughput, arbitrary HDR/rotation/codec combinations, physical OS shortcuts and live compositor drag-and-drop are not established by these checks. Remote task scheduling, audio retention in video exports, JPEG/TIFF/DPX sequence support and sharing native annotations with the Qt annotation file are outside this local implementation. Windows evidence must be recorded from native runs; Linux and mocked platform tests are not a substitute.
