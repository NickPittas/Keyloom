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

## Limits

Production-resolution throughput, arbitrary HDR/rotation/codec combinations, physical OS shortcuts and live compositor drag-and-drop are not established by these checks. Remote task scheduling, audio retention in video exports, JPEG/TIFF/DPX sequence support and sharing native annotations with the Qt annotation file are outside this local implementation. Windows evidence must be recorded from native runs; Linux and mocked platform tests are not a substitute.
