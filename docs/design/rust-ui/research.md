# Research: Rust backend/UI options for EZ-CorridorKey2 (real-time playback)

**Date:** 2026-10-22 (rev 2 — corrections after parent verification) · **Author:** research sub-agent · **Status:** research only — no implementation
**Scope:** Verify the user-named libraries "EffectCraft" and "Filmcraft"; survey minimal plausible native Rust UI options for a Python/PySide6 → Rust migration of EZ-CorridorKey2's playback UI. The CUDA CorridorKeyV2 inference backend (Python, whole-clip run_inference) is explicitly **out of scope** — playback of rendered EXR/PNG frames is a separate problem from inference.

**Evidence levels used throughout:** *[documented]* = stated in the project's own docs; *[source-verified]* = read directly in repo source at a pinned revision; *[benchmark-verified]* = measured in a benchmark we ran (none this session — no perf claims below are benchmark-verified).

**CorridorKey2 output facts (source-verified in this repo, `backend/frame_io.py`):** mattes are written as EXR half-float; grayscale mattes are a **single `Y` channel**; RGB(A) outputs are separate; default compression is **DWAB**, user-selectable PIZ/ZIP/none; internal staging EXRs are always DWAB. Any Rust EXR decoder must therefore handle single-channel (arbitrary-name) files and DWAB/PIZ/ZIP to cover all outputs. The existing app also already accepts **movie files as input** (FFmpeg/PyAV extraction path) — movie *decode* is a current capability, not a future one; only movie playback in a Rust viewer would be new.

---

## 1. Identity of the named libraries (verified)

Both exist and are siblings in the same "ArtCraft" clean-room suite by the `storytold` org:

| | EffectCraft | FilmCraft |
|---|---|---|
| Repo | https://github.com/storytold/effectcraft | https://github.com/storytold/filmcraft |
| What it is | Motion-graphics / VFX **compositor** (After Effects target) | Nonlinear **video editor** (Premiere Pro 2026 target) |
| Language/UI | Pure Rust, egui + wgpu | Pure Rust, egui + wgpu |
| License | **MIT OR Apache-2.0** (dual, workspace `Cargo.toml` + GitHub) | **MIT OR Apache-2.0** (dual) |
| Created | 2026-10-01 | 2026-09-30 |
| Maturity | ~3 weeks old, v0.14.x, alpha | ~3 weeks old, v0.10.x, alpha |
| Platforms | macOS / Windows / Linux (+ WASM) | macOS / Windows / Linux (+ WASM) |
| Revision read (source-verified) | `f37f20ff` (2026-10-21) | `45901fcc` (2026-10-21) |

Sources: GitHub repo pages (show both MIT and Apache-2.0 license files); workspace `Cargo.toml` `license = "MIT OR Apache-2.0"` in both repos; [EffectCraft site](https://getartcraft.com/apps/effectcraft); [FilmCraft architecture.md](https://github.com/storytold/filmcraft/blob/main/docs/architecture.md); [EffectCraft CLAUDE.md](https://github.com/storytold/effectcraft/blob/main/CLAUDE.md).

**Consumability caveat (revised):** neither `filmcraft-*` nor `effectcraft-*` is published on crates.io (queried 2026-10-22). They are Cargo-workspace-internal crates, so consumption means a **git dependency pinned to a specific rev** — which is a supported, reproducible Cargo workflow, and indeed **EffectCraft itself consumes FilmCraft exactly this way**: `filmcraft-media`/`filmcraft-codecs`/etc. pinned to `rev = "2c5ba7a72619935f0481f99d59409e0d1844b234"` with an ADR documenting the pin (EffectCraft `Cargo.toml`, source-verified). That is strong precedent that pinned-rev reuse of these crates is practical. What is *not* available is semver stability: updates mean moving the pin manually and absorbing whatever changed. The real distinction to track is **git dependency (workable, pin + periodic bump)** vs **coupling to their app-level architecture** (engine/project/egui crates, which we would not adopt anyway — we'd consume only `media`/`codecs`-level crates).

## 2. FilmCraft — capabilities relevant to EZ-CorridorKey2 playback

From `docs/architecture.md` (documented) plus direct source reads at `45901fcc` (source-verified where noted):

**Documented (architecture.md):**
- Headless engine-first design: every feature reachable without a window; egui UI is just one client (`Session::execute(id, params)` command registry). Mirrors the swappable-UI-over-headless-core architecture EZ-CorridorKey2 wants.
- Decode: pure-Rust clean-room demuxers/decoders (MP4/MOV, MKV, MXF, MPEG-TS, H.264, HEVC, VP9, AV1, ProRes, DNx, …) — no FFmpeg. Hardware decode via `platform` crate: VideoToolbox, Media Foundation/DXVA, VA-API, with software fallback and mid-stream recovery.
- Seek: MP4 seeks to preceding sync sample and decodes forward, caching the GOP; sequential playback reuses the decoder.
- Cache: `media` crate byte-budgeted shared LRU frame cache (`MediaSource` trait; sources `Send + Sync`).
- Playback scheduling: `ui-egui/frames.rs` worker pool (on-screen frame first, then prefetch, then thumbnails); UI thread never decodes; preroll (~0.5 s); audio-clock-driven playhead (cpal) with wall-clock fallback; `cargo xtask bench-playback` headless benchmark exists.
- Color/alpha: `color` crate; premultiplied linear-light f32 compositing; 8/16-bit YUV and RGBA8/linear-RGBA-f32 `VideoFrame` types.
- Time: integer `Tick` (254,016,000,000/s) exact rational time.
- Image sequences and stills listed as source kinds in the `media` crate.

**Source-verified (crash course through `crates/media/src/`):**
- **EXR decode exists** (this corrects rev 1: `crates/media/src/still/exr.rs` is a hand-rolled scan-line EXR decoder, wired into both still open and image-sequence paths). Scope: single-part scan-line files with **no, RLE, ZIPS, ZIP, or PXR24** compression; half/float/uint channels; R,G,B,A → frame channels, luminance-only (Y) shown grey; display-window handling. **It explicitly refuses PIZ, B44, DWA-compressed, tiled, deep, and multi-part files with an error.**
- ⚠️ **Consequence for us:** CorridorKey2's default **DWAB** (and optional **PIZ**) matte/staging EXRs are *outside FilmCraft's EXR decoder's supported set*. Reusing `filmcraft-media` as-is would not read our default outputs; it would work only if we write output EXRs as ZIP/ZIPS/none (a possible but nontrivial change to our output settings and existing-file compatibility), or extend their decoder.

**Unknown / not verified for FilmCraft:**
- No public playback benchmark numbers (Linux/NVIDIA or otherwise) — `bench-playback` exists but no published results.
- Zero-copy GPU presentation: the Windows path is documented as doing one staging-texture readback; **Linux VA-API presentation path not read — no claim either way.** Scope any zero-copy statement to the path actually read.
- Linux VA-API implementation details not read in full.

## 3. EffectCraft — capabilities relevant to EZ-CorridorKey2 playback

From `docs/architecture.md` / `docs/footage.md` (documented) plus source reads at `f37f20ff`:

**Documented:**
- Image sequences are a first-class footage type: numbered stills import as one footage item that "plays like a movie", with run/missing-frame reporting ([footage.md](https://github.com/storytold/effectcraft/blob/main/docs/footage.md)).
- Media layer reuses FilmCraft's pure-Rust codecs plus stills/sequence support and a frame cache (one ecosystem for our purposes).
- Playback model: viewer **RAM preview** keyed by (comp content, frame, scale, view, options) + persistent disk cache (LZ4, atomic rename, checksummed, LRU eviction) — AE-style, vs FilmCraft's real-time NLE model. Which model fits EZ-CorridorKey2 is a design decision.
- GPU: wgpu compute compositor, CPU reference oracle, deferred readbacks; 8/16/32 bpc, linear-light blending.

**Source-verified (rev `f37f20ff`):**
- **EXR decode is confirmed** (corrects rev 1): `crates/media` depends on `image` with the `exr` feature (RGB/RGBA route, via the `exr` crate backend) **and directly on `exr = "1.74"`**; `crates/media/src/exr_channels.rs` reads *arbitrary-channel* EXR (AOVs, Cryptomatte, 3D-channel data) with `exr::image::read().all_channels().all_layers()…`. Export-side EXR also present (`crates/export` `image` exr feature; engine `frame_export.rs`).
- Since the underlying `exr` crate (exrs 1.74.2) implements **decode for DWAA/DWAB, PIZ, ZIP, RLE, B44, PXR24, uncompressed** (docs.rs enum `Compression`: "Decoding support is implemented" for DWAA/DWAB; encode not implemented for DWA), EffectCraft's media layer *can in principle* read single-channel DWAB/PIZ EXRs like CorridorKey's. Caveat: whether the sequence-playback path surfaces arbitrary channels (vs the RGBA `image` path) end-to-end was not traced — needs a bounded runtime test before relying on it.

**Unknown for EffectCraft:** end-to-end arbitrary-channel EXR sequence playback (above); real-time sequence playback throughput at 1080p/4K (no public numbers, nothing benchmark-verified).

## 4. The actually-usable published Rust playback crates (adjacent finding)

Two **published** crates wrap FFmpeg for exactly EZ-CorridorKey2's movie playback shape (frame-accurate seek, A/V sync, RGBA sink) — relevant since movie *input/decode* is a current app capability, not hypothetical:

| Crate | Version | What it does (per docs.rs, documented) | Repo |
|---|---|---|---|
| [`ff-decode`](https://docs.rs/ff-decode) | (docs.rs live) | Frame-by-frame decode, keyframe + exact seek without re-open, thumbnails | via ff-preview repo |
| [`ff-preview`](https://docs.rs/ff-preview) | 0.18.4 (2026-09-29) | Real-time playback loop: audio-master sync, `FrameSink`/`RgbaSink` traits, RGBA via libswscale, proxy generation | https://github.com/itsakeyfut/avio |

Caveats: FFmpeg-linked (C dependency — CorridorKey2 already ships FFmpeg, so tolerable); maturity/license/bus factor not audited; no EXR support (FFmpeg EXR decode is slow, and pointless given the crates below).

**For EXR/PNG sequence decode, the verified story (revised):**

- [`image`](https://crates.io/crates/image) (0.25.x, MIT OR Apache-2.0): PNG solid; its OpenEXR decoder is **RGB/RGBA-only** (docs). **Insufficient alone for CorridorKey2**: grayscale mattes are a single `Y`-channel file, so `image`'s EXR decoder cannot read them.
- [`exr`](https://crates.io/crates/exr) (exrs, 1.74.2, **BSD-3-Clause**, no unsafe, MSRV 1.83, ~82M downloads): generic arbitrary-channel/multi-layer EXR; **decode supported for DWAA, DWAB, PIZ, ZIP1/16, RLE, B44/B44A, PXR24, uncompressed** (DWA decode implemented, DWA *encode* returns an error; HTJ2K not supported). This is the crate that covers all CorridorKey2 EXR outputs, including single-channel DWAB mattes. The practical stack is: `exr` directly for matte/single-channel files (and any float channel layout), `image` (or `exr` too) for RGB(A) and PNG.
- Note EffectCraft's media layer (§3) already combines both crates this same way — a reusable reference implementation of exactly this split.

## 5. Minimal plausible native Rust UI options for EZ-CorridorKey2

EZ-CorridorKey2's UI need is small: a viewer canvas, timeline/scrubber, a few panels, file pickers. Ranked by fit (top = starting point):

| Option | Video texture story | Evidence | Risk |
|---|---|---|---|
| **egui + eframe (wgpu paint)** | Per-frame texture upload via `Context::load_texture`/`TextureHandle` (`ColorImage`) or a custom `egui-wgpu` paint callback with a dedicated texture + WGSL tonemap shader; update via `TextureHandle::set`/`TextureDelta` (API verified on docs.rs egui 0.36/0.37 — no `Color32Texture` type exists; rev-1 claim corrected) | Both Craft apps use egui+wgpu for video monitors (documented in their arch docs; egui 0.36 in FilmCraft's Cargo.toml, source-verified) | Immediate-mode; fine for a tool UI |
| **iced** | wgpu-native `image::viewer` widget exists | General knowledge — **not verified in this pass** | Smaller video precedent |
| **Slint** | `Image` with software/wgpu renderers | General knowledge — not verified | Licensing needs a read before use |
| **Tauri (webview)** | Decode in Rust core, blit via canvas | General knowledge — not verified | Heavier; float EXR in webview compositing is awkward |
| **raw winit + wgpu** | Full control, custom quad+blit shader | Standard stable crates | Most code to write |

(The Craft apps' shared `craft-ui` docking crate is itself a pinned git dep on `storytold/craft-libs` — available the same way as §1 describes, not a blocker but not versioned on crates.io either.)

## 6. Capability matrix

| Capability | FilmCraft (`media` crates, rev 45901fcc) | EffectCraft (`media`, rev f37f20ff) | ff-preview/+ff-decode | exr/image crates + egui/wgpu |
|---|---|---|---|---|
| Real-time playback loop | ✅ documented (worker pool, audio clock) | ✅ RAM-preview model (documented) | ✅ documented | DIY (decode thread → texture) |
| Exact seek / GOP handling | ✅ documented | ✅ (via FilmCraft codecs) | ✅ documented | n/a (per-file stills) |
| Frame cache | ✅ byte-budget LRU (documented) | ✅ RAM + disk cache (documented) | ❓ proxy cache only | DIY LRU |
| PNG sequence | ✅ (documented source kind) | ✅ first-class footage (documented) | ❌ | ✅ |
| Single-channel (Y) EXR decode | ✅ supported by their decoder *but* only for its compression set | ✅ via `exr` crate aux-channel reader (source-verified); end-to-end playback untested | ❌ | ✅ `exr` crate, arbitrary channels |
| DWAB / PIZ EXR | ❌ **refused by their EXR decoder** (source-verified) | ⚠️ should work via `exr` 1.74 (decode documented); untested end-to-end | ❌ | ✅ `exr` decode documented for both |
| Movie decode (MP4/ProRes/…) | ✅ documented, pure Rust, no FFmpeg | ✅ (via FilmCraft codecs) | ✅ FFmpeg-backed | ✅ FFmpeg-backed |
| Color / alpha | ✅ premult linear f32, color crate (documented) | ✅ 8/16/32 bpc linear (documented) | RGBA8 via swscale | f32 via `exr`; tonemap DIY |
| GPU presentation | ⚠️ one CPU readback **on the Windows path only** (documented); Linux path unread | deferred readbacks (documented); details unread | CPU RGBA delivery | Standard texture upload |
| License | MIT OR Apache-2.0 | MIT OR Apache-2.0 | ❓ repo not audited | MIT OR Apache-2.0 (image); BSD-3-Clause (exr) |
| Consumable as library | ⚠️ not on crates.io; **pinned git rev dep works** (EffectCraft does exactly this) | ⚠️ same | ✅ crates.io | ✅ crates.io |
| Maturity | ~3 weeks, alpha, fast-moving | ~3 weeks, alpha | small project | 10+ years / 5+ years |

## 7. Design-selection guidance (no implementation, no premature decision)

1. **Nothing is decided yet — decide by benchmark, not by paper.** The two viable Rust playback strategies are:
   - **(a) Minimal sequence backend:** `exr` (mattes, any channels, DWAB/PIZ/ZIP) + `image` (PNG, RGB(A) EXR) → decode thread pool → byte-budgeted LRU (pattern from FilmCraft) → egui/eframe or winit+wgpu texture upload with a WGSL tonemap shader. Fully crates.io-published dependencies.
   - **(b) Pinned Craft reuse:** git dep on `effectcraft-media`/`filmcraft-media` at a pinned rev (the EffectCraft→FilmCraft pinned-rev precedent shows it's workable), getting their cache/decode/color machinery for free — accepting manual pin bumps and (for FilmCraft) an EXR compression mismatch with our DWAB/PIZ outputs.
   **Recommended next step:** a small benchmark harness comparing both on a real CorridorKey2 4K EXR/PNG sequence (warm playback fps, warm/cold seek latency, memory), plus the end-to-end arbitrary-channel EXR test for EffectCraft (§3). Choose on measurements.
2. **What to steal regardless of choice:** headless engine + command registry; integer tick time; byte-budgeted shared LRU frame cache; UI thread never decodes; compression-aware EXR acceptance matrix.
3. **If Craft crates are adopted later**, prefer the `media`/`codecs` layer only (not their engine/project/egui crates), pin the rev, and record the pin in an ADR like EffectCraft does. Re-evaluate the pin quarterly; the suite is moving fast.
4. **Zero-copy GPU presentation:** only the Windows FilmCraft path is *documented* to do a CPU readback; no universal claim is justified. For a file-based sequence viewer, CPU upload of decoded files is the normal model; budget for it and measure.
5. **Movie input** already works in the current app via FFmpeg extraction; a Rust rewrite needs movie *decode* parity (FFmpeg-backed `ff-decode`, or FilmCraft's pure-Rust codecs) — this is a current-capability port, not a future feature.

## 8. Explicitly unverified / open questions

- EffectCraft: end-to-end playback of single-channel DWAB/PIZ EXR **sequences** (crate-level decode is source-verified; the path from sequence reader → aux-channel reader → viewer was not traced, and nothing was built/run this session).
- FilmCraft: whether their EXR decoder scope is a deliberate product choice that will persist (affects option (b) if outputs were re-compressed to ZIP).
- No performance claim in this document is benchmark-verified; all throughput/latency statements are pending the benchmark in §7.1.
- ff-preview repository license, bus factor, Windows/Linux CI not audited.
- iced/Slint/Tauri rows in §5 are general knowledge, not source-verified.
- Provenance of the `storytold` suite (mirrors exist on third-party Gitea/TFGIT instances; GitHub `storytold` org appears primary) not independently verified.
- egui API row verified against docs.rs (0.36/0.37) only; not compiled.

## Source list (primary)

1. https://github.com/storytold/filmcraft — repo, MIT OR Apache-2.0, read at rev `45901fcc` (2026-10-21)
2. https://github.com/storytold/filmcraft/blob/main/docs/architecture.md — layering, codecs, seek, cache, playback scheduling, HW decode
3. `filmcraft/crates/media/src/still/exr.rs` @ `45901fcc` — EXR decoder scope: no/RLE/ZIPS/ZIP/PXR24; refuses PIZ/B44/DWA (source-verified)
4. https://github.com/storytold/effectcraft — repo, MIT OR Apache-2.0, read at rev `f37f20ff` (2026-10-21)
5. `effectcraft/Cargo.toml` @ `f37f20ff` — `exr = "1.74"`, `image` exr feature, FilmCraft pinned-rev deps `rev = "2c5ba7a7…"` (source-verified)
6. `effectcraft/crates/media/src/exr_channels.rs` @ `f37f20ff` — arbitrary-channel EXR read (source-verified)
7. https://github.com/storytold/effectcraft/blob/main/docs/footage.md — image-sequence import (documented)
8. https://docs.rs/exr/latest/exr/compression/enum.Compression.html — DWAA/DWAB decode implemented, PIZ/ZIP/RLE/B44/PXR24; HTJ2K unsupported (documented)
9. https://crates.io/api/v1/crates/exr — 1.74.2, BSD-3-Clause, MSRV 1.83 (verified 2026-10-22)
10. https://docs.rs/image/latest/image/codecs/openexr/ — image EXR decoder RGB/RGBA-only (documented)
11. https://docs.rs/egui — texture APIs `Context::load_texture`/`TextureHandle`/`ColorImage`; no `Color32Texture` (verified 2026-10-22)
12. https://docs.rs/ff-preview / https://docs.rs/ff-decode — published playback crates (documented)
13. This repo: `backend/frame_io.py` (single-channel `Y` half-float mattes; DWAB default, PIZ/ZIP/none options), `backend/ffmpeg_tools/extraction.py` (DWAB staging), movie input path (source-verified)
14. crates.io API queries 2026-10-22: `filmcraft-media`, `effectcraft-engine` → not published

## Rev-2 correction log (for the parent's audit trail)

1. License corrected Apache-2.0 → **MIT OR Apache-2.0** (both repos, dual).
2. Issue-count references removed as quality evidence; replaced with the git-dep-vs-coupling distinction and the EffectCraft→FilmCraft pinned-rev precedent.
3. Pinned-rev proof-of-concept retained as a live option (§7.1b) instead of a blanket "do not couple".
4. Unverified "23.9 fps warm 4K" claim removed; no perf claims remain un-benchmarked.
5. Movie playback no longer framed as purely future — the app supports movie input today.
6. Single-channel matte EXR requirement added; `image` crate alone declared insufficient for mattes; `exr` crate arbitrary-channel + DWAB/PIZ/ZIP decode verified (docs.rs + crates.io).
7. Craft EXR decode resolved by bounded source reads: FilmCraft = hand-rolled decoder refusing DWAB/PIZ; EffectCraft = image-exr + direct `exr` crate arbitrary channels.
8. `egui::Color32Texture` removed (does not exist); correct APIs cited.
9. CPU-readback/zero-copy claims scoped to the documented Windows FilmCraft path only.
10. §7 reframed as benchmark-first comparison (minimal backend vs pinned Craft reuse) with no premature recommendation.
