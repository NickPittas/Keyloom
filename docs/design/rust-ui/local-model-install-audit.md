# Local Model Install Audit — standalone full-local workflow (pre-remote)

Date: 2026-10-22 · Auditor: research sub-agent · Revised same day after parent review (4 corrections: OpenEXR/Build-Tools, license wording, Rust Key capability as of `9c4712a`, VideoMaMa rename framing) · Evidence: source inspection at HEAD `c09a822` (corridorkey-v2), plus live HF/GitHub API verification of every external model reference. **No downloads, installs, GPU runs, or production edits were performed.**

Scope: everything needed to install, provision, start, and configure **all models** (CorridorKey 2.0 bundle incl. C-RADIO/MoGe-2/RVM, SAM2.1, GVM, VideoMaMa, MatAnyone2, BiRefNet) on **Windows and Linux**, from installers through startup diagnostics; then the delta between what the **current Qt UI** can do and what the **Rust UI / worker protocol** lacks.

---

## 1. Executive summary

1. **The model provisioning backbone already exists and is good**: `scripts/setup_models.py` is a single asset registry (catalogue, `is_installed` checks, per-model download fns, disk-space preflight, proxy guard). Both installers and the Qt Setup Wizard drive it. Recommendation is to **reuse it, not replace it** (§10).
2. **"MatAnyone" is not in this repo — only MatAnyone2.** `modules/` contains `MatAnyone2Module` (checkpoint `matanyone2.pth`, GitHub `pq-yang/MatAnyone2` release v1.0.0, 141 MB). There is no MatAnyone-v1 module anywhere in the tree (verified by grep `MatAnyone(?!2)` → 0 hits). Any requirement doc that says "MatAnyone" means MatAnyone2 here.
3. **Integrity validation is nearly absent**: only RVM has a content hash (MD5-b64, enforced). CorridorKey v2 assets are existence/non-empty checked; GVM / VideoMaMa / SVD / SAM2 / MatAnyone2 / BiRefNet are single-sentinel-file existence checks with **no content hash and no revision pin** (moving-tag risk). SAM2 additionally gets a size+ZIP-magic sanity check at load.
4. **Revision pinning exists only for the two v2 foundation models** (C-RADIO `c0457f5d…`, MoGe `ca5f0e0…` — both verified live and resolving today via HF API). CorridorKey repo, GVM, VideoMaMa, SVD, SAM2, BiRefNet, MatAnyone2-release all float.
5. **Hidden transitive weights are real and must be managed explicitly**: VideoMaMa cannot run without the **Stability SVD img2vid-xt base (~9.5 GB, includes CLIPVision image encoder + CLIPImageProcessor + temporal-decoder VAE)**; the CorridorKey bundle hides **C-RADIO (~SO400M), MoGe-2 ViT-B, RVM small**; BiRefNet lazy-downloads any of **16 variants** on first use and needs `timm` remote code (`trust_remote_code=True`); SAM2 needs the `sam-2` **Python package** (git-pinned) *and* a checkpoint.
6. **License discrepancy found**: `modules/MatAnyone2Module/LICENSE.txt` is **S-Lab License 1.0 (non-commercial)**, while `README.md` line ~688 claims MatAnyone2 is **Apache 2.0**. The vendored code license governs; docs must be corrected or clarified before any redistribution story.
7. **Legacy per-model installers are broken v1 leftovers**: `Install_GVM_Windows.bat` / `Install_VideoMaMa_Windows.bat` target old `venv\` (v2 creates `.venv`), and the VideoMaMa one **never downloads the SVD base** and skips the `unet/ → VideoMaMa/` rename that `setup_models.py` applies — see §3.4: the rename is an accommodation of a repo-layout inconsistency whose necessity still needs a loader-level check, not a verified requirement.
8. **HF cache-path mismatch**: `setup_models.py` hardcodes `~/.cache/huggingface/hub` (L78) while the wizard's detection honours `HF_HOME`/`HF_HUB_CACHE` (`setup_wizard._hf_hub_cache_dir`). With `HF_HOME` set, the wizard can report SAM2 missing while the download lands in the default cache (or vice-versa).
9. **Rust UI has zero model *provisioning* surface — but it does run AI.** As of checkpoint `9c4712a` the Rust workspace runs **local Key/batch** jobs through the worker (`rust-ui/src/scheduler.rs` KeyJob + GPU/capability preflight; `ui/ai.rs`); the rust-ui README sentence "does not run, queue or simulate AI" quoted during the original audit is **stale** relative to that capability. The worker protocol reports only `corridorkey_v2` (path/bytes/sha256) and `capabilities.generate_hint: false`. Everything the Qt app does for model *install/status/diagnostics* (§8) is still missing from Rust (§9).
10. **Windows/Linux both gated on NVIDIA CUDA** for the whole v2 stack: installers hard-fail without working `nvidia-smi`; engine raises without `torch.cuda.is_available()`. macOS is explicitly unsupported in v2 (installer exits on Darwin), though legacy macOS code paths remain. On Windows, **OpenEXR does not necessarily require VS Build Tools**: the real remote validation (2026-10-22) installed the cp312 win_amd64 **OpenEXR 3.5.2 wheel (bundling Imath)** without compiling, and the full GPU EXR stage including **float32 PIZ** ran successfully on RTX 4090. The installer's Build-Tools prompt is a fallback for source builds, not a prerequisite.

---

## 2. Master model inventory

Sizes are the app's own estimates (shown to users); no hashes are enforced unless stated.

| # | Model (app key) | Weights source (exact ID) | Revision pin | Files / sentinel check | Install size | Cache / target path | License (evidence) | Content hash? |
|---|---|---|---|---|---|---|---|---|
| 1 | CorridorKey 2.0 (`corridorkey`) | HF `nikopueringer/CorridorKey2.0` | **none** | `corridorkey_v2.safetensors`, `foundation_stats.safetensors` → validated non-empty by `backend/v2_paths.validate_v2_assets` | ~5 GB bundle | `<v2 models dir>/weights/` (§5) | "Corridor Key Licence" = CC BY-NC-SA 4.0 + additional terms (`CorridorKeyV2/LICENSE`; HF card confirms, not gated) | No (existence/size only; worker computes sha256 for *identity*, not against known-good) |
| 2 | C-RADIO v4 SO400M (hidden dep of #1) | HF `nvidia/C-RADIOv4-SO400M` | `c0457f5dc26ca145f954cd4fc5bb6114e5705ad8` ✅ live | snapshot; needs `config.json`, `hf_model.py`, ≥1 non-empty `*.safetensors` (`.pth.tar` ignored) | (in 5 GB) | `<v2>/vfms/cradio/C-RADIOv4-SO400M/` | NVIDIA Open Model License (`THIRD_PARTY_NOTICES.md`; HF: `license: other`, custom_code, gated:false) | No |
| 3 | MoGe-2 ViT-B (hidden dep of #1) | HF `Ruicheng/moge-2-vitb-normal` | `ca5f0e07ff01d3e5a364c1d954ed12ee1814b368` ✅ live | `model.pt` single file | (in 5 GB) | `<v2>/vfms/moge/model.pt` | MIT (notices; HF card `license:mit`, gated:false; base dinov2-base) | No |
| 4 | RVM small (hidden dep of #1) | GCS `storage.googleapis.com/representations4d/checkpoints/pretrain_rvm_small16_256_204031069.npz` | URL-pinned filename | npz file | (in 5 GB) | `<v2>/vfms/rvm/…npz` | Weights CC-BY 4.0, code Apache 2.0 (notices; DeepMind representations4d) | **Yes — MD5-b64 `ZJb6kj8+tasiJbVeplXu7Q==`, enforced pre/post** |
| 5 | SAM2.1 tracker (`sam2`) | HF `facebook/sam2.1-hiera-{small,base-plus,large}` — default **base-plus** | **none** | `sam2.1_hiera_{…}.pt` in **shared HF hub cache**; load-time check ≥10 MB + ZIP magic (`sam2_tracker/wrapper.py:107-155`) | 184/324/898 MB | `~/.cache/huggingface/hub` (⚠ hardcoded in setup_models L78; wizard honours `HF_HOME`/`HF_HUB_CACHE`) | Apache 2.0 (README §licenses) | No (magic bytes only) |
| 6 | SAM2 Python package | git `github.com/facebookresearch/sam2.git@2b90b9f5ceec907a1c18123530e92e794ad901a4` (pyproject `[tracker]` extra) | git SHA ✅ | import `sam2.build_sam` (`setup_models.tracker_dependency_installed`) | pip pkg | venv site-packages | Apache 2.0 | n/a (git pin) |
| 7 | GVM alpha generator (`gvm`) | HF `geyongtao/gvm` (full snapshot; not gated) | **none** | `unet/diffusion_pytorch_model.safetensors` sentinel; runtime loads `vae/` (AutoencoderKLTemporalDecoder), `scheduler/` (FlowMatchEuler), `unet/` — all inside the snapshot | ~6 GB | `<data>/gvm_core/weights/` | CC BY-NC-SA 4.0 (`gvm_core/LICENSE.md`; upstream aim-uofa/GVM) | No |
| 8 | VideoMaMa (`videomama`) | HF `SammyLim/VideoMaMa` + **base** HF `stabilityai/stable-video-diffusion-img2vid-xt` | **none / none** | `VideoMaMa/diffusion_pytorch_model.safetensors` — checked after `setup_models.py:289-296` applies an `unet/ → VideoMaMa/` rename to reconcile the HF repo layout with the loader's expected subdir (rename consistency flagged for loader-level verification, §3.4) — **and** `stable-video-diffusion-img2vid-xt/model_index.json` | ~37 GB (app) = ~27.5 ckpt + ~9.5 SVD | `<data>/VideoMaMaInferenceModule/checkpoints/{VideoMaMa/, stable-video-diffusion-img2vid-xt/}` | Code CC BY-NC 4.0; weights Stability AI Community License (`VideoMaMaInferenceModule/LICENSE.md`, README ~L687). ⚠ SVD repo carries Stability custom license — **gating not confirmed either way** by page extraction; do an anonymous preflight HEAD in managed provisioning | No |
| 9 | MatAnyone2 (`matanyone2`) | GitHub release `pq-yang/MatAnyone2` **tag v1.0.0** asset `matanyone2.pth` (✅ live via GH API, published 2026-03-06) | tag | single `.pth`, exact size 141,429,115 B | 141 MB | `<data>/modules/MatAnyone2Module/checkpoints/matanyone2.pth` (+legacy `<root>/pretrained_models/`) | ⚠ **S-Lab License 1.0 (non-commercial)** in vendored `modules/MatAnyone2Module/LICENSE.txt` — README's "Apache 2.0" claim is wrong/conflicting | No (URL release asset; size constant exists but unchecked) |
| 10 | BiRefNet (`birefnet`) | HF `ZhengPeng7/<variant>` — 16 variants (Matting default; see §6.4) | **none** | any `*.safetensors` in variant dir; loaded via `AutoModelForImageSegmentation.from_pretrained(..., trust_remote_code=True)` (wrapper L217) — **needs `timm==1.0.24`** (requirements comment) | ~940 MB (default) | `<data>/modules/BiRefNetModule/checkpoints/<Variant>/` | MIT (README; HF ZhengPeng7) | No |
| 11 | Apple Vision (macOS only, no weights) | OS framework | — | — | — | — | — | — |

Device needs: **all of #1–#10 are used CUDA-only in practice** — installers hard-gate on `nvidia-smi` (`1-install.sh` L33-40, `1-install.bat` pip path), `backend/corridorkey_v2.py` raises `V2EngineError` without CUDA, `core.detect_device()` (`backend/service/core.py:220`) resolves `cuda`/`cpu`. GVM wrapper retains legacy MPS branches from upstream (dead on v2; macOS install exits). Single-model-in-VRAM policy: `backend/service/model_manager.py` `_ActiveModel` enum (NONE/INFERENCE/GVM/SAM2/VIDEOMAMA/MATANYONE2/BIREFNET, L20-29) — one heavy model loaded at a time, switch = offload + double gc + `torch.cuda.empty_cache`.

---

## 3. Per-model download code & lazy paths (callers an implementer must know)

### 3.1 CorridorKey 2.0 bundle (#1-4)
- Driver: `scripts/setup_models.py:231 download_corridorkey()` → loads `CorridorKeyV2/scripts/download_weights.py` (vendored) → `download_corridorkey` (2 files, `hf_hub_download`, **no revision**), `download_cradio` (snapshot @ pinned rev, ignores `*.pth.tar`), `download_moge` (single `model.pt` @ pinned rev), `download_rvm` (urllib + **MD5-b64 gate**, `.part` tmp + atomic rename).
- Post-check: `backend.v2_paths.validate_v2_assets()` (L88) — also asserts the *code* package (`corridorkey/runtime.py`, `third_party/MoGe/`) exists beside the package root.
- Runtime loader: `backend/corridorkey_v2.py:139 CorridorKeyV2Engine.load()` — re-validates, requires CUDA, loads under `_offline_env()` (L38; **inference never hits the network**).
- Error UX: diagnostics `id="v2-assets-missing"` message tells users to run `python scripts/setup_models.py --corridorkey` (`ui/widgets/diagnostic_checks.py` ~L74-80).

### 3.2 SAM2 (#5-6)
- Package: installer extra `-e ".[tracker]"` (opt-in prompt, default Y; env `CORRIDORKEY_INSTALL_SAM2`).
- Checkpoint: `setup_models.download_sam2_model()` → `hf_hub_download(repo, filename, cache_dir=~/.cache/huggingface/hub)`; `--sam2` CLI takes `small|base-plus|large|all` (default base-plus). Installers pre-download base-plus (env `CORRIDORKEY_PREDOWNLOAD_SAM2`).
- Lazy path (Qt): `sam2_tracker/wrapper.py:74` default `facebook/sam2.1-hiera-base-plus`; `_get_predictor` uses the **sam2 package's** `HF_MODEL_ID_TO_FILENAMES` map (L239) + `hf_hub_download` (L253) — i.e. the model-id list partly lives in the pinned sam-2 package, then `_validate_checkpoint` (L107). Model selection from Preferences: `KEY_TRACKER_MODEL="tracking/sam2_model"` (QSettings), options small/base-plus/large (`ui/widgets/preferences_dialog.py:50,75-79`), applied via `core.set_sam2_model()`.
- Note: selection of small/large from the UI implies a **lazy download at click time** (checkpoint for that variant is fetched on first use into the HF cache).

### 3.3 GVM (#7)
- `setup_models.download_repo("gvm")` → `snapshot_download("geyongtao/gvm", local_dir=<data>/gvm_core/weights)`.
- Runtime: `gvm_core/wrapper.py:36 _resolve_gvm_weights_dir()` — prefers `<data>/gvm_core/weights`, falls back to bundled `gvm_core/weights` (dev repo copy; currently present locally, gitignored), **else falls back to live HF repo id** `"geyongtao/gvm"` (L100) — meaning a missing install silently turns into an on-demand snapshot at first GVM use (managed provisioning must be aware this network path exists).
- Extra deps: installed via main `requirements.txt` (diffusers/transformers/accelerate/peft); `gvm_core/requirements.txt` exists but is only used by the **legacy** bat.

### 3.4 VideoMaMa (#8) — the most fragile install
- `setup_models.download_model("videomama")` (`download_repo`, L285): first `snapshot_download("stabilityai/stable-video-diffusion-img2vid-xt", local_dir=…/stable-video-diffusion-img2vid-xt)` (base_model cfg L92-97), then `snapshot_download("SammyLim/VideoMaMa", …)`, then **renames `unet/ → VideoMaMa/`** because the HF repo ships `unet/` while the module resolves `checkpoints/VideoMaMa` (`_resolve_checkpoint("VideoMaMa")`, `inference.py`). ⚠ This rename is an inferred accommodation of a repo-layout inconsistency: **it has not been verified against the actual loader/pipeline expectations** (e.g. whether the vendored pipeline could read `unet/` directly, or whether other SammyLim snapshot shapes need different handling). Managed provisioning must resolve the discrepancy with a loader-level check (cold-install smoke of `load_videomama_model`) rather than treating either layout as declared-correct.
- Runtime: `VideoMaMaInferenceModule/inference.py:62 load_videomama_model()` — resolves both via `_candidate_checkpoint_dirs()` (data-dir first, bundled fallback), raises `FileNotFoundError` with explicit download instructions if either is missing. **No lazy download** — this is the one model that hard-fails instead of self-provisioning.
- Hidden weights inside SVD base: `CLIPVisionModelWithProjection` image encoder + `CLIPImageProcessor` + `AutoencoderKLTemporalDecoder` VAE + unet (`VideoMaMaInferenceModule/pipeline.py:11,82-103`).

### 3.5 MatAnyone2 (#9)
- Installer: `setup_models.download_matanyone2()` (L384) — `urllib.urlretrieve` from GitHub release to `<data>/modules/MatAnyone2Module/checkpoints/`, `.tmp` + rename, progress hook.
- Lazy path (Qt): `modules/MatAnyone2Module/wrapper.py:79 _ensure_checkpoint()` — candidate dirs (data-dir → bundled → legacy `pretrained_models/`), and if absent **auto-downloads via `torch.hub.download_url_to_file`** (L105-106). No hash on either path.

### 3.6 BiRefNet (#10)
- Installer: `setup_models.download_birefnet()` (L443) — default variant `BiRefNet-matting` snapshot only.
- Lazy path (Qt): `modules/BiRefNetModule/wrapper.py:91 BIREFNET_MODELS` (16 display names → `ZhengPeng7/<repo>`), `_resolve_model_dir` (L70) → `snapshot_download` on first use of *any* variant (L196), `trust_remote_code=True` load (L217), per-variant resolutions (L112-124: HR 2048², lite-2K 2560×1440, dynamic=input-res, default 1024²).
- **Managed-provisioning implication**: full "all models installed" coverage for BiRefNet means either pre-fetching all 16 variants (~15 GB) or accepting lazy per-variant network hits at click time.

---

## 4. Where things install (path contract)

`backend/project.py:49 get_data_dir()`:
- Dev (this repo checkout): **repo root** — GVM weights in `gvm_core/weights/` (gitignored), MatAnyone2 in `modules/MatAnyone2Module/checkpoints/` (gitignored), etc.
- Frozen portable: dir of exe if `portable.txt` present.
- Frozen: `QSettings("app/install_path")`, else platform default.

CorridorKey v2 bundle override chain (`backend/v2_paths.py:44 get_v2_models_dir()`):
1. env `CORRIDORKEY_V2_MODELS_DIR`
2. Win `%LOCALAPPDATA%/EZ-CorridorKey2/CorridorKeyV2` · mac `~/Library/Application Support/EZ-CorridorKey2/CorridorKeyV2` · Linux `${XDG_DATA_HOME:-~/.local/share}/EZ-CorridorKey2/CorridorKeyV2`

Sub-layout: `weights/corridorkey_v2.safetensors`, `weights/foundation_stats.safetensors`, `vfms/cradio/C-RADIOv4-SO400M/`, `vfms/moge/model.pt`, `vfms/rvm/pretrain_rvm_small16_256_204031069.npz`. (This machine: `~/.local/share/EZ-CorridorKey2/CorridorKeyV2/` fully populated.)

SAM2: **shared HF hub cache**, not the data dir — `~/.cache/huggingface/hub` by default; ⚠ mismatch between `setup_models.HF_CACHE_DIR` (hardcoded, L78) and wizard detection `_hf_hub_cache_dir()` (honours `HF_HUB_CACHE`/`HF_HOME`). README documents cache reclamation (`models--facebook--sam2.1-hiera-base-plus`, `models--ZhengPeng7--BiRefNet*` folders).

FFmpeg (not a model, but part of local install): Windows pinned verified build `ffmpeg-8.1.2-full_build.zip` from `github.com/GyanD/codexffmpeg/releases/download/8.1.2/…` (`backend/ffmpeg_tools/discovery.py:38-42`); installer + in-app "Repair FFmpeg" share the pin (issue #184). Linux: system package guidance only (apt/dnf/pacman).

---

## 5. Installer scripts matrix

| Script | Status | Model coverage | Notes |
|---|---|---|---|
| `1-install.sh` (Linux) + `1-install.bat` (Win) | **Current** | corridorkey (always, failure = warn+retry hint), SAM2 pkg+base-plus ckpt (opt-in default Y), GVM (~6 GB, default N), VideoMaMa (~37 GB, default N) | uv→pip fallback, managed Py 3.11 (3.11-3.13 supported), cu128 torch, `verify_torch_runtime.py` gate (writes `logs/diagnostics/install-torch-runtime.json` + GitHub-issue helper), triton check (Win), nvidia-smi hard gate, FFmpeg (Win auto-download pinned; Linux package-manager guidance), disk-space preflights, proxy guard, desktop shortcut. Env switches: `CORRIDORKEY_INSTALL_SAM2`, `CORRIDORKEY_PREDOWNLOAD_SAM2`, `CORRIDORKEY_INSTALL_GVM`, `CORRIDORKEY_INSTALL_VIDEOMAMA`, `CORRIDORKEY_PYTHON_VERSION`, `CORRIDORKEY_CREATE_SHORTCUT`, `CORRIDORKEY_INSTALL_FFMPEG`. **MatAnyone2 and BiRefNet are NOT offered by either installer** — only wizard/lazy. |
| `2-start.sh/.bat` | Current | — | Recreates .venv via installer if missing; launches `main.py`. |
| `3-update.sh/.bat` | Current | — | git pull / ZIP fallback; git-link step for ZIP installs (reset to `origin/main` — note: on the v2 fork this is legacy-upstream logic). |
| `ui/widgets/setup_wizard.py` (in-app) | Current | corridorkey, sam2(base-plus), gvm, matanyone2, birefnet, videomama — all six | Runs `setup_models.py` fns in a QThread with per-model progress; auto-opens when required assets missing or version changed (`needs_setup`, L284); re-openable as **Edit → Download Manager**; data-dir Browse/Default; also desktop-shortcut + one-time crash-report opt-in. |
| `Install_GVM_Windows.bat` | **Legacy/broken on v2** | GVM only | Old `venv\` (never created by v2 installer → script exits "run Install_CorridorKey_Windows.bat first"); `huggingface-cli download` into `gvm_core\weights`; claims "80GB+" (wrong vs ~6 GB); installs `gvm_core/requirements.txt` which can resolve a *different* torch than the main pin. |
| `Install_VideoMaMa_Windows.bat` | **Legacy/broken on v2** | VideoMaMa ckpt only | Same `venv\` problem; downloads `SammyLim/VideoMaMa` but **not the SVD base** and **no `unet/→VideoMaMa/` rename** → runtime `FileNotFoundError` even after "successful" install. |
| `Install_CorridorKey_Windows.bat` | Legacy v1 full installer | — | Superseded by `1-install.bat`; still references v1 flow/`venv`. |
| `RunGVMOnly.sh`, `RunInferenceOnly.sh`, `CorridorKey_DRAG_CLIPS_HERE_local.*` | Legacy v1 entry points | — | Untouched by v2 audit; candidates for deletion/rewrite. |

**Linux vs Windows deltas**: Win offers a VS Build Tools prompt (optional fallback for source builds such as OpenEXR when no wheel matches — **not a prerequisite**: cp312 win_amd64 OpenEXR 3.5.2 wheel incl. Imath verified working, float32 PIZ, remote RTX 4090 validation 2026-10-22), Git/winget, pinned FFmpeg auto-download, triton-windows check; Linux requires system python 3.11-3.13 or uv-managed, package-manager FFmpeg. Both gate on NVIDIA + cu128 wheels. Neither offers matanyone2/birefnet pre-download at install time.

---

## 6. Startup, settings, diagnostics (Qt today)

- **Startup flow**: `main.py` (~L163) → `needs_setup()` → auto `SetupWizard`; `MainWindow` init → `detect_device()` → `run_startup_diagnostics(device)` (`ui/widgets/diagnostic_checks.py:451`) → StartupIssues dialog.
- **Diagnostic registry** (`_DIAGNOSTICS`, error-pattern → fix steps): `gpu-required` (CPU-only torch), `pytorch-cpu-wheel` (NVML present but +cpu wheel), `v2-assets-missing` (run setup_models), `gvm-weights-missing` (pattern `gvm_core.*weights.*not found` — **stale**: points to `python -m gvm_core.download` which doesn't exist in this tree; GVM now auto-resolves or fails differently), ffmpeg repair guidance, and more (714-line file).
- **Settings (QSettings)**: model-relevant key `tracking/sam2_model` (default `facebook/sam2.1-hiera-base-plus`; options small/base-plus/large); `app/install_path` (data dir); `privacy/crash_reports_*`. All other model behaviour has **no persistent settings** — VRAM profile, hint mode/quality etc. are per-job inference params (`InferenceParams`, parameter panel).
- **Model manager**: `backend/service/model_manager.py` — lazy `_get_*` per model (`_get_v2_engine`, `_get_gvm`, `_get_sam2_tracker`, `_get_videomama_pipeline`, `_get_matanyone2`, `_get_birefnet(usage)`) with `_ensure_model` single-residency switch and `on_status` progress callbacks; `unload_engines()`.
- **Network hygiene**: `backend/net_proxy.sanitized_proxy_env()` wraps all downloads (socks4→socks5 fix-up); v2 engine loads strictly offline (`_offline_env`).

---

## 7. Worker protocol today (what Rust already has)

`python -m backend.worker` (`backend/worker/__init__.py`, JSON-lines over stdio, stays alive across jobs):
- `hello` (L97-108): `worker_rev`, `engine:"corridorkey-v2"`, `host`, `platform`, `gpu {index,name,vram_gb}` (probe; missing GPU → stderr note, `capabilities.key:false`), `models` = **only** `corridorkey_v2 {path, bytes, sha256}` (sha256 cached in `<scratch>/cache/model_sha256.json`, L36-75), `capabilities {key, generate_hint:false, publish_modes:["final"]}`.
- Asset problems are written to **stderr** at startup (L82-84), not structured into `hello`.
- Commands: `key`, `cancel`, `shutdown` (L211-222). Rust side (`rust-ui/src/worker.rs`): `ModelId {path, bytes, sha256?}`, identity gate `verify()` rejects digest mismatch/missing-digest (L176-200), `Capabilities {key, generate_hint, publish_modes}`.
- **Hint generation is not in the worker at all** (`generate_hint:false` hard-coded) — GVM/BiRefNet/VideoMaMa/MatAnyone2/SAM2 remain Qt-side only.

## 8. Qt model actions vs Rust UI gaps

Qt can (today): ① first-run + on-demand model downloads with progress for all 6 catalogue entries; ② data-dir choice/persistence; ③ SAM2 variant preference (settings + lazy ckpt fetch); ④ lazy self-provisioning for MatAnyone2 / BiRefNet(any variant) / SAM2(any variant) / GVM(HF fallback) at click time; ⑤ startup diagnostics + known-error fix guidance; ⑥ single-model VRAM lifecycle with status callbacks; ⑦ FFmpeg repair with pinned build.

Rust UI (`rust-ui/`) has, as of checkpoint `9c4712a`: import + viewing, **local Key/batch execution through the worker** (`scheduler.rs` KeyJob pipeline with GPU/capability preflight — the README's "does not run, queue or simulate AI" line predates this and is stale), its own `settings.json` (`settings.rs`: playback/interface/**ai** params/**masks.tracker_model**/ai_targets), worker spawn + job protocol + corridorkey_v2 identity gate. It **lacks** — for model *provisioning* specifically, in priority order for "all models local":

| Gap | Detail | Natural reuse |
|---|---|---|
| G1 Model install/download surface | No wizard/download-manager equivalent; no catalogue, sizes, required/optional flags | Drive `scripts/setup_models.py` (spawn with `--check`/`--<model>`); it already handles resume/disk/proxy. Add a `--json` status mode instead of parsing human output. |
| G2 Install-state detection | No `detect_installed_models` equivalent (incl. SVD-base co-check for VideoMaMa, sam2 pkg+cache dual check) | Port `ui/widgets/setup_wizard.py:120 detect_installed_models` logic or expose via worker `hello`. |
| G3 Asset/revision/hash integrity | No verification beyond corridorkey_v2 sha256 identity; no pins for optional models | Extend setup_models registry with `revision` + `sha256` per model; report digests in `hello.models` (reuse `model_sha256`). |
| G4 Hint-generator models in worker | `generate_hint:false`; GVM/VideoMaMa/MatAnyone2/BiRefNet/SAM2 can't be driven from Rust at all | Backend work (out of audit scope) — but provisioning (G1-G3) should land first so the data is there. |
| G5 Structured missing-asset errors | Worker prints asset problems to stderr only | Fold `validate_v2_assets()` + per-model checks into `hello` (e.g. `assets: {corridorkey:[…], …}`) so Rust can render remediation. |
| G6 SAM2 variant setting wiring | `masks.tracker_model` exists in rust settings but nothing consumes/validates it | Align with Qt `tracking/sam2_model` semantics + checkpoint-presence check. |
| G7 FFmpeg management | `ck_import.py` needs FFmpeg but has no repair/pin surface | Reuse `scripts/check_ffmpeg.py` + `backend/ffmpeg_tools` pin. |
| G8 HF cache env handling | Rust must not repeat the setup_models-vs-wizard `HF_HOME` mismatch | Single source: honour `HF_HOME`/`HF_HUB_CACHE` everywhere. |

Protocol sketch (for the planner): extend `hello` with `models: {<key>: {installed, path, bytes, sha256?, revision?, problems[]}}` for corridorkey/cradio/moge/rvm/sam2(+pkg)/gvm/videomama(+svd)/matanyone2/birefnet(variants), add cmd `install {"model": key}` streaming `progress` events — all implementable by calling into existing setup_models functions; no new download logic needed.

---

## 9. Risk register (ranked)

1. **VideoMaMa cold install fragility** — two unpinned repos plus the `unet/ → VideoMaMa/` rename step whose necessity vs the real loader is unverified (§3.4); legacy bat installs a broken half. Mitigate: pin revisions, validate both sentinels, and resolve the layout question with a cold-install loader smoke before encoding the rename as canonical.
2. **No integrity hashes on ~all weights** — silent corruption/truncation passes `is_installed` (single-file existence). RVM proves the pattern; extend MD5/SHA registry. (SAM2's magic-byte check only catches stubs.)
3. **Unpinned moving tags** (CorridorKey repo, GVM, VideoMaMa, SVD, SAM2, BiRefNet, MatAnyone2-release) — upstream repo changes break installs nondeterministically. C-RADIO/MoGe pins show the fix.
4. **MatAnyone2 license conflict** (S-Lab 1.0 vendored vs README "Apache 2.0") — must be reconciled before any redistribution story. Licensing is **per-component** (see §2 table: Corridor Key Licence = CC BY-NC-SA 4.0 + terms; NVIDIA Open Model License; MIT ×2; CC-BY 4.0; Apache 2.0; CC BY-NC-SA 4.0 (GVM); CC BY-NC 4.0 + Stability AI Community License (VideoMaMa); S-Lab 1.0 (vendored MatAnyone2)). **Redistribution and commercial-use permissions across the combined stack are unresolved; this audit makes no legal determination** — routing through the respective licences/owners is a separate decision for the project owner.
5. **SVD gating uncertainty** — Stability repo may require license acceptance; do anonymous preflight and surface a clear "accept license on HF / provide token" path rather than an opaque 401.
6. **Lazy network calls at click time** (GVM HF fallback, BiRefNet 16 variants, SAM2 variants, MatAnyone2) — offline/air-gapped machines get stalls; "fully provisioned" must mean these lazy paths are pre-satisfied or disabled. `--all` covers them except: SAM2 small/large (only base-plus default) and 15/16 BiRefNet variants.
7. **`gvm-weights-missing` diagnostic is stale** (`python -m gvm_core.download` doesn't exist).
8. **`setup_models.PROJECT_ROOT` bakes at import** — wizard deliberately decoupled detection for this reason; any Rust/worker status command must re-instantiate per data-dir or pass an env.
9. **Disk math**: advertised sizes vs `check_disk_space` (10% headroom) — VideoMaMa needs ~40 GB free (37 + headroom) plus extraction room; SVD base inside the 37 figure is *additional* to the VideoMaMa repo size (~27.5 GB) — installer text should say "~37 GB total" (it does) but the two downloads are sequential and resumable.
10. **Windows specifics**: OpenEXR **does not necessarily require VS Build Tools** — verified counter-example from the real remote validation (2026-10-22): the cp312 win_amd64 OpenEXR 3.5.2 wheel (bundles OpenEXR + Imath extensions, dep numpy only) installed task-locally without compiling and the full GPU EXR stage incl. float32 PIZ ran on RTX 4090; keep Build Tools only as an optional source-build fallback in docs/installers. Also: `sam2` package is a git direct-ref (`allow-direct-references`) so offline installs need a wheelhouse.

---

## 10. Recommendations for the implementer (reuse-first)

1. **Single asset registry**: keep `scripts/setup_models.py` as the one catalogue. Add per-entry `revision` + `sha256` (or MD5-b64 like RVM) + a `--json` status flag. Files: `scripts/setup_models.py` (`MODELS` L80, `MATANYONE2_CHECKPOINT` L109, `BIREFNET_DEFAULT_CHECKPOINT` L124, `SAM2_MODELS` L132; `is_installed` L152).
2. **Fix the two sentinel gaps**: VideoMaMa already checks both files (setup_models + wizard) — mirror it in any Rust detection; add hash for `matanyone2.pth` (size constant exists: 141,429,115 — upgrade to sha256 from the release).
3. **Worker `hello` enrichment** (file: `backend/worker/__init__.py` L85-108): loop over a shared status function (new `backend/model_status.py` or import setup_models helpers) instead of the inline corridorkey-only dict; keep stderr warnings but also emit structured `assets` problems. Caller: `rust-ui/src/worker.rs` `Hello`/`Capabilities` + `verify()`.
4. **Rust model-manager slice** (G1/G2/G5): spawn `python scripts/setup_models.py --check --json` / `--<model>` with `--data-dir`-aware env (`CORRIDORKEY_V2_MODELS_DIR` already exists); parse progress from the existing print stream or add `--progress-json`. Do **not** port download logic to Rust.
5. **Housekeeping (small, safe)**: delete or fix `Install_GVM_Windows.bat` / `Install_VideoMaMa_Windows.bat` / `Install_CorridorKey_Windows.bat` (v1, broken); fix the stale GVM diagnostic entry; unify HF cache resolution (`setup_models.HF_CACHE_DIR` → respect `HF_HOME`/`HF_HUB_CACHE`); correct README MatAnyone2 license line or replace vendored LICENSE header after owner decision.
6. **Windows/Linux full-local acceptance checklist** (for the next slice): fresh `.venv` install → `--check` shows all present after `--all` + `--sam2 all`; unplug network → wizard/GVM/BiRefNet/MatAnyone2/SAM2 clicks must not attempt HF (except documented SVD/GVM repo fallbacks which should be disabled or pre-satisfied); worker `hello` reports every model; VideoMaMa cold-install loader smoke resolves the `unet/` vs `VideoMaMa/` layout question (§3.4) — both repos present under whichever layout the loader verifiably accepts.

### Verification commands used/available (all ≤15 s, no downloads)
- `python scripts/setup_models.py --check` (status only; safe offline after imports)
- `.venv/bin/python -c "from backend.v2_paths import validate_v2_assets, get_v2_asset_paths; print(validate_v2_assets())"` — asset problems list
- `rg -n 'repo_id|REPO|revision' scripts/setup_models.py CorridorKeyV2/scripts/download_weights.py` — registry source of truth
- Live external checks performed 2026-10-22 (HF API): C-RADIO rev `c0457f5d…` ✅ (`license:other` nvidia-open-model, custom_code, gated:false), MoGe rev `ca5f0e0…` ✅ (mit, `model.pt`), MatAnyone2 release v1.0.0 asset `matanyone2.pth` ✅ (published 2026-03-06); repos exist & not gated: `nikopueringer/CorridorKey2.0`, `geyongtao/gvm`, `SammyLim/VideoMaMa`, `facebook/sam2.1-hiera-base-plus`, `ZhengPeng7/BiRefNet-matting`; `stabilityai/stable-video-diffusion-img2vid-xt` exists under Stability custom license, gating unconfirmed (preflight before relying on anonymous download).
