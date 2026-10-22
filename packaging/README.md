# Standalone installers (L7)

Private, local-only build pipeline. No publishing or redistribution clearance is implied
(see `docs/design/rust-ui/standalone-release-plan.md` §8). No listeners, services or firewall
rules are created; the only network use is the explicit model step.

## What a bundle contains
`bin/corridorkey-ui` (Rust UI) · `app/` (Python helper tree: backend, engines, `scripts/setup_models.py`,
`rust-ui/tools/ck_import.py`, `packaging/`) · `app/tools/ffmpeg/bin/{ffmpeg,ffprobe}` (where
`backend/ffmpeg_tools` already looks) · `runtime/python` (uv-managed relocatable CPython 3.11 + the
hash-pinned lock `packaging/locks/<os>-x86_64.txt`, torch 2.9.1+cu128) · `corridorkey-install.json`
(descriptor; models/cache/scratch/user_data = null → platform defaults, shared by all versions) ·
`install-manifest.json` (sha256 of every file). **No model weights.**

## Build (build host = target OS)
Linux, all unprivileged and resumable (long steps run in the background; each rerun resumes from caches):
```
sh packaging/linux/build-ui-old-glibc.sh           # zig-based glibc-2.31-target UI build (floor 2.30), ~1 min warm
sh packaging/linux/fetch-ffmpeg.sh                 # pinned static FFmpeg 7.0.2 (sha256-checked); prints the --ffmpeg-dir
python packaging/bundle.py linux --out DIR --ffmpeg-dir <bin> \
  --ui-binary rust-ui/target-oldglibc/x86_64-unknown-linux-gnu/release/corridorkey-ui \
  [--reuse-runtime <stage>/runtime/python]          # omit to build the runtime with uv from the lock
python packaging/bundle.py windows --out DIR --ffmpeg-dir C:\ffmpeg\bin --build-ui   # on Windows only
```
The bundle's **glibc floor is computed from every ELF** and refused above `--max-glibc` (default 2.31, recorded in the
manifest; the installer refuses older systems). Current floor: 2.30 (the zig-built UI; the runtime is <= 2.28).
sam-2's CUDA extension (`sam2/_C.so`) is rebuilt by `stage_runtime` with `--no-cache` (never a cached wheel) against the
installed torch 2.9.1+cu128 using a private, sha256-pinned CUDA 12.8.1 toolkit (nvcc/cudart/cccl from NVIDIA's redist,
in `~/.cache/ck-toolchain/cuda-12.8.1`) and a private conda-forge GCC 13 + glibc-2.17 sysroot (micromamba, same dir) as
nvcc's host compiler. The host CUDA/GCC are never used. The runtime records `.ck-runtime-provenance.json` (lock hash,
build recipe, `_C.so` hash); `--reuse-runtime` rejects a runtime whose recipe or extension differs. Wheels install with
the `manylinux_2_28` tag so e.g. cryptography stays low.
Prerequisites: `uv`, `git`, `curl`, system `rustc`/`cargo`, network (wheels, zig, FFmpeg, CUDA 12.8.1 parts, micromamba/GCC 13) for wheels/zig/FFmpeg;
Windows also needs Inno Setup 6 (`ISCC.exe`). Linux FFmpeg is 7.0.2 (backend accepts any non-development release >= 7
on Linux; Windows uses the separate 8.1.2 pin).
The build ends with a real launch validation of the staged tree (`selfcheck.py` + the UI's own `--self-check`).
Regenerate a lock after editing `runtime.in`:
`uv pip compile packaging/runtime.in --python-version 3.11 --python-platform x86_64-unknown-linux-gnu|x86_64-pc-windows-msvc --index-url https://pypi.org/simple --extra-index-url https://download.pytorch.org/whl/cu128 --index-strategy unsafe-best-match --generate-hashes --emit-index-url -o packaging/locks/<os>-x86_64.txt`

## Linux: `CorridorKey-<id>-linux-x86_64.run`
Needs only sh/tar/gzip/awk/sha256sum. `./X.run [--prefix DIR] [--models full|skip] [--model-arg ARG]... [--repair]`
(default prefix `~/.local/opt/CorridorKey`). Each `--model-arg` takes ONE argument, forwarded verbatim (spaces/globs
preserved) to `scripts/setup_models.py`, e.g. `--model-arg --hf-cache --model-arg "/my cache"`. Those are the manager's
path overrides (`--data-dir/--v2-models-dir/--hf-cache/--cache-dir`); there is no separate offline-bundle importer in
the installer — an existing model tree is used by pointing these at it (the manager verifies by hash).
The prefix must be new/empty or an existing CorridorKey install; any other non-empty folder is refused.
Flow: verify payload sha → extract into `PREFIX/.staging.*` → journal → bundled python validates manifest, glibc
floor and launches (imports + UI `--self-check`) → atomic `current` symlink switch → `~/.local/share/applications/
corridorkey.desktop` (Exec scrubs PYTHONPATH/PYTHONHOME), `PREFIX/bin/{corridorkey,corridorkey-setup}`, `receipt.json`
→ full models (`setup_models.py --download --profile full`, then `--verify`). Any failure or Ctrl-C restores the previous
version/pointer/shortcuts (or removes everything on a first install); a crash leaves a journal that the next run
(`corridorkey-setup recover`, also automatic) finishes.
Exit 0 = everything Ready; **3 = app installed but models incomplete/skipped**; 2 = refused/failed; 130 = interrupted.
Later: `PREFIX/bin/corridorkey-setup {status [--verify] | models | repair | rollback | recover | uninstall}`;
`X.run --repair` restores damaged files from the payload (models re-verified only with `--models full`).
Two versions are kept (active + previous). Concurrent installers on one prefix are not supported.

## Windows: `CorridorKey-<id>-windows-x86_64-setup.exe` (Inno Setup, per-user by default)
Installs to `%LOCALAPPDATA%\Programs\CorridorKey\versions\<id>`, then runs the same `lifecycle.py`
(validation, Start Menu shortcut, receipt, optional full models task with a visible console). Drive-root folders and an
already-installed identical build are refused; a failed launch validation sets a nonzero setup exit code
(`GetCustomSetupExitCode`) and the previous version's shim runs `recover` to delete the failed one. Add/Remove Programs
and the uninstaller come from Inno. Uninstall deletes only the exact version dir Inno created plus receipt-validated
versions carrying our own manifest; the running version is removed by a detached `cmd`. The Start Menu shortcut targets
the exe directly, so it does not scrub PYTHONPATH (the Rust side must). Repair of damaged files on Windows =
reinstall after uninstall. `/TASKS="!models"` makes a silent app-only install. Native build and installation checks are recorded in the acceptance document below.

## Preserved on uninstall/update/repair
Projects, outputs, models, HF cache and settings live outside the install prefix and are never
removed. Uninstall trusts only `receipt.json` and refuses a prefix without one.

## Tests
`bash rust-ui/tools/isolated-test.sh /usr/bin/timeout 14 .venv/bin/python -m pytest -p no:cacheprovider -q tests/test_packaging.py`
(tiny fake payloads; no network, runtime or models).

## Acceptance evidence
See [local workflow acceptance](../docs/design/rust-ui/local-workflow-acceptance.md) for the exact installer builds, installed-file checks, model verification and real GPU workflow results. Unit tests use tiny fixtures; they are not the evidence for native installation or GPU acceptance.
