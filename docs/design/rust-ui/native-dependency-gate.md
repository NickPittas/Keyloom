# Native dependency gate — first runnable Rust sequence viewer

Date: 2026-10-22 · Status: gate recommendation (no production code written) · Supersedes nothing; companion to `research.md` (rev 2)
Scope: pinned dependency choices for the first build spike, exact API entry points, fixture/benchmark commands, and the go/no-go gate. FilmCraft DWAB/PIZ incompatibility is already established in `research.md` §2 and is not re-derived here.

All claims below are **source-verified** (repo reads / local `cargo tree` runs / docs.rs + crates.io API, 2026-10-22) unless marked otherwise. No performance claims — nothing has been measured yet.

---

## 1. Toolchain

Local: `rustc 1.98.1`, `cargo 1.98.1` (verified on this machine). Every recommendation below has MSRV ≤ 1.95, so 1.98.1 satisfies all of them with headroom.

## 2. Recommendation: direct `exr` + `image` + `eframe` (NOT EffectCraft-media)

**Use the crates.io-published minimal stack. Do not take a git dependency on `effectcraft-media` for this spike.** Verified reasons:

### 2.1 EffectCraft-media footprint (source-verified at rev `f37f20ff`)

`effectcraft/crates/media/Cargo.toml` declares **20 direct dependencies**, including workspace-internal crates that are themselves git-pinned only:

- `effectcraft-{time, project, raster, render, psd, svg, pdf, model}` (project-internal, not on crates.io)
- `filmcraft-{codecs, media, isobmff, frame, color, time}` — git deps on `storytold/filmcraft` rev `2c5ba7a7…` (which pulls the full pure-Rust video-decoder stack, incl. `symphonia 0.5.5` for audio, AV1/HEVC/ProRes decoders, etc.)
- `exr 1.74`, `image 0.25` (with gif/webp/tiff/bmp/exr/tga/hdr features), `gif`, `rayon`, `serde_json`

So "reuse just the EXR part" is not separable: the arbitrary-channel reader `exr_channels.rs::read_exr_channels(bytes) -> Option<AuxChannels>` **returns an `effectcraft_raster::AuxChannels`** and uses `effectcraft_raster::channels3d` — it is **not callable independently** of the `effectcraft-raster` crate, which transitively requires the workspace (project/render/psd/svg/pdf/model + all of FilmCraft). The full transitive tree is hundreds of crates across two moving git repos pinned 3 weeks into their alpha. There is no way to depend on `effectcraft-media` without all of it.

**Conclusion:** EffectCraft's value to us is as a *reference implementation to read* (its `exr_channels.rs` is exactly the decode pattern we need — see §3.1), not as a dependency. Revisit git-rev reuse only if we later want their frame cache or codecs wholesale.

### 2.2 Direct stack footprint (measured locally with `cargo tree`, 2026-10-22)

| Crate | Version | License | MSRV | Dep tree (unique crates, `cargo tree`) |
|---|---|---|---|---|
| `exr` | **1.74.2** (pinned) | BSD-3-Clause | 1.83 | **32 crates** with `default-features = false`; default features = `["rayon"]` only (multithreaded block decompression) — keep defaults, adds just rayon-core |
| `image` | **0.25.10**, `default-features = false, features = ["png"]` | MIT OR Apache-2.0 | 1.88 | small (png + zune cores; ~30 crates incl. shared deps) |
| `eframe`/`egui` | **0.37.0** (pinned) | MIT OR Apache-2.0 | **1.95** | large (wgpu/winit tree, several hundred crates) — inherent to any GPU UI; first build takes minutes, subsequent builds incremental |
| `rayon` | 1.x | MIT OR Apache-2.0 | — | comes via `exr` default |
| `anyhow` | 1.x | MIT OR Apache-2.0 | — | spike error handling |

Note: `eframe 0.37.0` rust-version 1.95 — fine for our 1.98.1, but if we ever want to match EffectCraft's reference code exactly (they use egui 0.36 at `f37f20ff`), egui 0.36 also compiles on 1.98.1. Pin **0.37.0**; the texture APIs we need (`Context::load_texture`, `TextureHandle::set`, `ColorImage`) are present in both (verified on docs.rs, 2026-10-22).

Suggested `Cargo.toml` for the spike:

```toml
[dependencies]
exr = "1.74.2"                       # arbitrary-channel EXR, DWAB/PIZ/ZIP decode
image = { version = "0.25.10", default-features = false, features = ["png"] }
eframe = "0.37.0"
egui = "0.37.0"
rayon = "1"                          # worker pool (also exr's parallel decode)
anyhow = "1"
```

Licenses: BSD-3-Clause (exr) + MIT/Apache-2.0 (rest) — all permissive, no copyleft, compatible with the current private-repo distribution. (The app itself is presumably not redistributing the viewer as a library; no license conflict identified. *Assumption, not legal review.*)

## 3. Exact decode API entry points

### 3.1 Single-Y / arbitrary-channel EXR (mattes) — direct `exr` crate

Pattern lifted verbatim from EffectCraft's `exr_channels.rs` (source-verified at `f37f20ff`, lines ~53–80), which itself uses the public `exr` API — we can call the same API without their crates:

```rust
use exr::prelude::*;

let img = read()
    .no_deep_data()
    .largest_resolution_level()
    .all_channels().all_layers().all_attributes()
    .from_buffered(std::io::BufReader::new(file))?;
// img.layers[i].channel_data lists each channel (e.g. "Y") as f32 Vec
// display window: img.attributes.display_window (usize w/h)
```

Decompression support in `exr 1.74.2` (docs.rs, verified): **DWAA, DWAB, PIZ, ZIP/ZIP16, ZIPS, RLE, B44/B44A, PXR24, uncompressed — all decode**; DWA *encode* not implemented (we only need decode); HTJ2K unsupported (not produced by our backend — `backend/frame_io.py` offers DWAB/PIZ/ZIP/none only). This covers 100% of CorridorKey2's EXR outputs including the default DWAB.

### 3.2 RGB(A) EXR

Two options: same `all_channels()` path as above (uniform code path — recommended for the spike; matte and RGB go through one reader), or `exr::prelude::rgba()` for RGBA-only. Use the uniform path.

### 3.3 PNG

`image::ImageReader::open(path)?.decode()?` → `DynamicImage` → `to_rgba8()`. Solid, no caveats.

### 3.4 Worker / cache basics (spike-scope minimum)

- One `std::thread` (or `rayon` scoped) decode thread with a bounded channel; UI thread never decodes (pattern both Craft apps follow — documented in their arch docs).
- Simplest cache: `HashMap<u32, Arc<DecodedFrame>>` capped by decoded-bytes budget with LRU eviction (`hashlink::LinkedHashMap` if desired, or hand-roll ~30 lines; do not add a cache crate for the spike).
- Upload to GPU: `TextureHandle::set(...)` / `TextureDelta` from decoded f32 → `egui::ColorImage` (tonemap in Rust for the spike: Y f32 → Rgba8 with a fixed exposure; WGSL tonemap is a later optimization).

## 4. Platform assumptions

- Linux/Wayland (this machine, Omarchy/Hyprland): eframe 0.37 uses winit with Wayland backend — standard, no x11 required. *(Assumed from winit's platform support; not yet run here — the spike itself is the test.)*
- No CUDA, no FFmpeg linkage in the viewer. System deps for eframe: Vulkan/OpenGL drivers (already present) plus XDG wayland libs at link time (`libxkbcommon`, `wayland-client` etc. — pulled via system packages that exist on this desktop).
- macOS/Windows portability later: all chosen crates are cross-platform; nothing Linux-specific in the spike.

## 5. Fixtures (verified on disk, this repo)

`Projects/261021_171345_V1-0069_A002C012_261012_R4LF/clips/V1-0069_A002C012_261012_R4LF/`:

- `Frames/frame_%06d.exr` — **3200×1800 RGB, DWAB, half** (source-verified header read: channels B,G,R; DWAB_COMPRESSION), 24+ frames
- `Matte/frame_%06d.exr` — **3200×1800 single `Y` channel, DWAB, HALF** (source-verified), **122 frames**

Compression-variant fixtures (PIZ/ZIP) for a later acceptance matrix: re-render a few frames via `scripts/v2_smoke.py` with the user-selectable compression option (backend supports PIZ/ZIP/none — `backend/frame_io.py`), or copy its write path with compression overridden. Not needed for the gate: DWAB is the default and the hardest case; PIZ/ZIP acceptance is a follow-up matrix test.

Additional media confirmed on disk by parent: 1080p project (161 frames) and a 3200×1800 project (122 frames), both 24 fps, with EXR source, Y-matte, and PNG comp outputs — enough for a real warm-decode benchmark once the Sonnet decoder builds.

## 6. Build-spike gate (go/no-go)

**Spike scope:** a binary that opens a folder, plays the `Matte/` + `Frames/` sequences of the project above, scrubs with a slider, at 3200×1800 (viewer downscaled to window).

Gate criteria — all must pass on this machine (RTX 4090 / current desktop):

1. **Build**: `cargo build --release` clean with only the §2.2 deps; no git dependencies, no patch sections.
2. **Decode correctness**: first matte frame decodes to single-Y f32; mean value within 1e-3 of the Python-side read of the same frame (`.venv/bin/python` OpenEXR reference); RGB frame decodes to R,G,B f32 planes. Print channel names + checksums to stdout.
3. **Playback (measured, not claimed)**: run the sequence once through the decode thread and report warm sequential decode fps for both sequences (`--bench-decode` mode: no UI, just decode loop). *Gate: no fixed number* — record it; the number informs the next design step. Anything ≥ ~24 fps warm decode at 3200×1800 means CPU decode is comfortably ahead of the UI; below that, cache/prefetch design matters more.
4. **UI run**: eframe window displays and scrubs both sequences without UI-thread decode (verify: scrub while decode thread busy stays responsive). Screenshot attached to the spike PR.
5. **Memory**: RSS after a full warm pass ≤ ~4 GiB with a 2 GiB decoded-frame cache budget (print RSS).

**Minimal benchmark commands:**

```bash
cd rust-spike/  # new, outside backend/
cargo build --release
./target/release/ezck2-viewer --bench-decode \
  Projects/261021_171345_V1-0069_A002C012_261012_R4LF/clips/V1-0069_A002C012_261012_R4LF/Matte
./target/release/ezck2-viewer --bench-decode .../Frames
./target/release/ezck2-viewer .../Matte   # interactive gate 4
/usr/bin/time -v ./target/release/ezck2-viewer --bench-decode .../Matte 2>&1 | grep -E 'Maximum resident|Elapsed'
```

**Out of gate scope:** movie decode, WGSL tonemap, disk cache, EXR encode, PNG out, multi-clip UI. Gate passes → Sonnet builds the real crate on this stack.

## 7. Verification log (what was actually checked, 2026-10-22)

- effectcraft cloned at `f37f20ff`; `crates/media/Cargo.toml` read directly (20 direct deps incl. raster/render/psd/svg/pdf/project + filmcraft git rev `2c5ba7a7…` + symphonia) — **source-verified**
- `exr_channels.rs::read_exr_channels` returns `effectcraft_raster::AuxChannels` (not independently callable) — **source-verified**
- `cargo tree` on a fresh crate with `exr 1.74.2` (no-default-features): 32 unique crates; default features = `["rayon"]` — **measured locally**
- crates.io API (with UA): eframe 0.37.0 / egui 0.37.0 (MSRV 1.95), image 0.25.10 (MSRV 1.88), exr 1.74.2 (MSRV 1.83, BSD-3-Clause) — **verified 2026-10-22**
- Fixture headers read with repo venv OpenEXR: Frames = RGB half DWAB 3200×1800; Matte = single `Y` half DWAB — **source-verified**
- exr decompression coverage (DWAB/PIZ/ZIP decode) — docs.rs enum docs, **documented** (runtime confirmation is gate item 2)
- eframe on Wayland — **assumed** (standard winit support); gate item 4 confirms
