# Standalone installers (L7)

Private, local-only build pipeline. No publishing or redistribution clearance is implied
(see `docs/design/rust-ui/standalone-release-plan.md` §8). No listeners, services or firewall
rules are created; network use is the explicit model step and, for the online installers only, the large public wheels.

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

## Linux: `Keyloom-<id>-linux-x86_64.run`
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
Release upload (GitHub 2 GiB/asset limit): add `--split` to `bundle.py linux` to also write `X.run.partNNN` (<=1900 MiB each) +
`X.run.parts.sha256`. User: `sha256sum -c X.run.parts.sha256 && cat X.run.part[0-9][0-9][0-9] > X.run && sha256sum -c X.run.sha256 && chmod +x X.run`. The unsplit `.run` is unchanged.
**OS prerequisites (not bundled; checked before install, with an apt hint for Debian/Ubuntu/Mint):** GL/EGL, X11 or Wayland client,
xkbcommon, fontconfig, freetype, glib, dbus, libstdc++, and the proprietary NVIDIA driver (`libcuda.so.1`; a missing driver only warns,
since models can still be installed). Python, Rust, FFmpeg, the CUDA toolkit and GCC are NOT needed on the target.
Later: `PREFIX/bin/corridorkey-setup {status [--verify] | models | repair | rollback | recover | uninstall}`;
server mode: `PREFIX/bin/corridorkey-coordinator {serve|pair|revoke|abandon} ...` (runs the bundled `backend.coordinator`; `serve` only listens when started, loopback by default);
`X.run --repair` restores damaged files from the payload (models re-verified only with `--models full`).
Two versions are kept (active + previous). Concurrent installers on one prefix are not supported.

## Windows: `Keyloom-<id>-windows-x86_64-setup.exe` (Inno Setup, per-user by default)
Installs to `%LOCALAPPDATA%\Programs\CorridorKey\versions\<id>`, then runs the same `lifecycle.py`
(validation, Start Menu shortcut, receipt, optional full models task with a visible console). Drive-root folders and an
already-installed identical build are refused; a failed launch validation sets a nonzero setup exit code
(`GetCustomSetupExitCode`) and the previous version's shim runs `recover` to delete the failed one. Windows needs no VC++ Redistributable: the Microsoft VC runtime DLLs are bundled beside python.exe and the UI. The NVIDIA driver
is the only prerequisite (missing driver warns). Per-user only (no elevation prompt). Inno slices are 1900 MiB: upload the exe plus every `-N.bin` and
the `.sha256` (one line per file); all must be in one folder to install.
Add/Remove Programs
and the uninstaller come from Inno. Uninstall deletes only the exact version dir Inno created plus receipt-validated
versions carrying our own manifest; the running version is removed by a detached `cmd`. The Start Menu shortcut targets
the exe directly, so it does not scrub PYTHONPATH (the Rust side must). Repair of damaged files on Windows =
reinstall after uninstall. `/TASKS="!models"` makes a silent app-only install. Native build and installation checks are recorded in the acceptance document below.

## Online installers (smaller .run / .exe, large public wheels downloaded)
Build host = target OS; the offline targets above are unchanged and stay the complete, self-contained option. The source repo stays private
and **installation never contacts GitHub**: no sign-in, client id, token or private release manifest. You hand the installer to a user once;
it carries Keyloom itself (app, Rust UI, FFmpeg, the standalone Python/stdlib, certifi, lifecycle/`online.py`, the custom git-built SAM2 CUDA
extension and every small dependency). Only wheels that PyPI / `download.pytorch.org` serve publicly are downloaded at install time:
`torch`, `triton` / `triton-windows` and `nvidia-*` (a first install is several GB; an update reuses files and downloads nothing the
installed versions already hold). Models are never installed by the wrapper: download them from the app. Nothing is installed that needs a
compiler or an NVIDIA display driver on the PC.
```
python packaging/bundle.py online-linux   --out DIR [--stage DIR/stage] [--wheel-cache DIR]   # Keyloom-online-<id>-linux-x86_64.run
python packaging/bundle.py online-windows --out DIR [--stage DIR/stage] [--wheel-cache DIR]   # Keyloom-online-<id>-windows-x86_64.exe (+ -N.bin slices)
```
Without `--stage` a full stage is built first (add `--ffmpeg-dir`, `--ui-binary`, ... as for the offline targets). The online and offline
installers of one build carry the **same `version_id`, install manifest and installed content**.

Build step `wheel_sources(stage, cache)`: for every installed distribution named `torch`, `triton`, `triton-windows` or `nvidia-*` whose
RECORD lists regular stage files >= 1 MiB, it resolves the exact public wheel (PyPI simple index first, then the PyTorch cu128 index; name,
version and every platform tag must equal the installed `METADATA`/`WHEEL`, and the index-advertised hash must be one of the pinned
`packaging/locks/<os>-x86_64.txt` hashes), downloads it into the cache with sha256 verification, maps each such file to its zip entry (`*.data/purelib|platlib`
adjusted) and checks the entry's size + sha256 against the stage's `install-manifest.json`. A big file that cannot be mapped
byte-for-byte fails the build with the list. `make_slim` then copies (hardlinks) the stage keeping symlinks and omitting only those mapped regular files.
Result wrapper (embedded in the installer; the sources file is outside `payload/`):
```
keyloom-online/payload/...                 slim complete stage minus mapped heavy files, original install-manifest.json
keyloom-online/wheel-sources.json          {"schema":1,"version_id":ID,"wheels":[{"url":https wheel URL,"sha256":64hex,"size":N,"files":{"<stage path>":"<zip entry>"}}]}
```
The installer runs `payload/runtime/python/{bin/python3|python.exe} -I -B payload/app/packaging/online.py --payload-dir PATH --sources PATH [--prefix DIR] [--repair]`.
`online.py` verifies, reuses byte-identical files of the current/previous installed versions, downloads only the wheels still needed
and hands the assembled complete payload to `lifecycle.py` (same transaction, rollback, shims, receipt). The exact build already installed is a no-op
without network; settings, models, credentials, certificates and jobs are untouched. The install manifest records no mode bits, so
executables extracted from wheels get their mode from the zip entry (done by `online.py`); copied payload files keep theirs. Exit 0 = installed/up to date, 2 failed (old install
intact), 4 Keyloom running (quit it with File > Quit), 130 cancelled.

Linux: `X.run [--prefix DIR] [--repair]` (default `~/.local/opt/CorridorKey`; needs only sh/tar/gzip/awk/sha256sum). The payload is extracted
into `PREFIX/.staging.*` (same filesystem, not a small `/tmp`).
Windows: per-user Inno exe (1900 MiB slices like the full installer: keep the exe and every `-N.bin` together; `.sha256` lists all); unpacks to `{tmp}`
with a visible console. `/PREFIX=DIR` and `/REPAIR=1` are accepted; otherwise an existing full-installer location is read from its Inno
registry key (`Inno Setup: App Path`), else `%LOCALAPPDATA%\Programs\CorridorKey`. It has its own AppId, `Uninstallable=no` and no Add/Remove Programs
entry, so it never touches the full installer's entry; remove an online-only install with `<prefix>\bin\corridorkey-setup.cmd uninstall`.

## Background autostart contract (written by the app, removed by uninstall)
Linux launches the stable shim `PREFIX/bin/corridorkey`. Windows launches the GUI exe directly; activation and rollback rewrite an existing owned entry. Linux:
`$XDG_CONFIG_HOME/autostart/corridorkey.desktop` containing `X-CorridorKey-Owner=<sha256(str(PREFIX))[:16]>` (`lifecycle.owner_id`).
Windows: `HKCU\\...\\Run` value `CorridorKey` whose command points into this prefix. `lifecycle.remove_autostart` deletes only a matching entry.

## Preserved on uninstall/update/repair
Projects, outputs, models, HF cache and settings live outside the install prefix and are never
removed. Uninstall trusts only `receipt.json` and refuses a prefix without one.

## Tests
`bash rust-ui/tools/isolated-test.sh /usr/bin/timeout 14 .venv/bin/python -m pytest -p no:cacheprovider -q tests/test_packaging.py`
(tiny fake payloads; no network, runtime or models).

## Acceptance evidence
See [local workflow acceptance](../docs/design/rust-ui/local-workflow-acceptance.md) for the exact installer builds, installed-file checks, model verification and real GPU workflow results. Unit tests use tiny fixtures; they are not the evidence for native installation or GPU acceptance.
