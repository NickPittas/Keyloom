# Standalone installers (L7)

Private, local-only build pipeline. No publishing or redistribution clearance is implied
(see `docs/design/rust-ui/standalone-release-plan.md` §8). Activation configures the per-user network host automatically
unless the user removed it. Firewall permissions remain OS-managed. Online installers also fetch the large public wheels.

## What a bundle contains
`bin/keyloom-ui` (Rust UI) · `app/` (Python helper tree: backend, engines, `scripts/setup_models.py`,
`rust-ui/tools/ck_import.py`, `packaging/`) · `app/tools/ffmpeg/bin/{ffmpeg,ffprobe}` (where
`backend/ffmpeg_tools` already looks) · `runtime/python` (uv-managed relocatable CPython 3.11 + the
hash-pinned lock `packaging/locks/<os>-x86_64.txt`, torch 2.9.1+cu128) · `keyloom-install.json`
(descriptor; models/cache/scratch/user_data = null → platform defaults, shared by all versions) ·
`install-manifest.json` (sha256 of every file). **No model weights.**

## Build: one canonical command (build host = target OS)
**Every installer build — by any person or agent — is exactly this command, on the target OS** (Linux builds Linux, Windows builds Windows; no cross-compiling):
```
python packaging/build_release.py            # Linux: .venv/bin/python;  Windows: the bundled/known Python
```
It builds the UI, ONE stage (one manifest, one launch self-check of Python + UI), a **helper-load gate** (the staged runtime Python, `-I -B` from an unrelated CWD, imports `lifecycle` and loads `firewall.py` through `lifecycle._firewall()` and requires callable `run`/`remove`/`main`; import-only, no firewall calls, Linux only; a missing module fails the build before any installer is packed), then the **offline and online installers from that same
stage** (same `version_id`), verifies every output, and prints `OK`/`FAILED: <reason>` plus the log and result paths. Exit 0 only when all
artifacts exist, are complete and match their sha256. Do not compose `bundle.py` steps by hand, do not rerun its checks separately, and do not
inspect the intermediate files: read the final line and `build-result.json`. A full run takes tens of minutes (compression); run it once and wait for its exit.
Output (`packaging/dist/<timestamp>/`, git-ignored; `--out-root DIR` or config `out_root` to move it): `build.log` (all phase output, written even
when preflight fails), `build-result.json` (status, failed phase, source commit + dirty flag + source-file fingerprint, Cargo.lock/packaging-lock/pyproject
hashes, tool versions, platform, payload `version_id`, and name/size/sha256 of every artifact; artifacts are empty on failure), `stage/` (kept on
failure and success for inspection or reuse; delete when done), the installers and their `.sha256` files.
`--preflight-only` validates config/tools/runtime without building. Phases are bounded (config `timeouts`, `deadline_seconds`); on timeout or Ctrl-C/SIGTERM the
child process tree is killed.

One-time machine config (optional on Linux; required keys on Windows): `~/.config/keyloom-build.json` (or `--config FILE`), see
`packaging/build-config.example.json`. Keys: `build_cache`, `runtime_dir` (a validated `<stage>/runtime/python`; default `<build_cache>/runtime/python`, built once if missing), `ffmpeg_dir`
(Linux default `~/.cache/ck-ffmpeg-pinned/bin`, create with `sh packaging/linux/fetch-ffmpeg.sh`), `wheel_cache` (Linux default
`~/.cache/ck-toolchain/public-wheels-linux`), `iscc`, `cargo_target_dir` (Windows warmed target), `out_root`, `python_version`, `split`
(Linux `.run` also written as <=1900 MiB parts), `timeouts`, `deadline_seconds`. Unknown keys fail the build.
Linux UI is built by `packaging/linux/build-ui-old-glibc.sh` with `PATH=/usr/bin:/bin RUSTC=/usr/bin/rustc CK_CARGO_FLAGS='--locked --offline'`;
Windows runs `cargo build --release --locked`. The binary must be an ELF (Linux) / PE (Windows) file.
Known limits: the payload identity (`version_id`) is deterministic for the same source, locks and runtime, but installer bytes are not promised identical
(gzip/Inno timestamps). The build reads the live checkout, so it fingerprints the source before and after and fails if it changed (no isolated copy).
`version_id` differs between Linux and Windows. Nothing is published, installed or launched, and no models or drivers are bundled.
The build-time staged self-check (`bundle.BUILD_SELFCHECK_TIMEOUT`, 300 s) is a build-only limit; installers have no time budget and use only the 300 s no-progress stall guard (`STALL_SECONDS`).

**Windows workflow (Kamino3 only).** Commit and push on the dev machine, then `git pull` in the single persistent Kamino3 checkout; never ship source by archive,
snapshot, scp/rsync/SSH pipe or a second checkout, and never copy a runtime, model or artifact over 100 MB. Pinned build dependencies (Python, wheels, FFmpeg,
Rust/Inno) are downloaded once into one persistent git-ignored build folder outside Git and updated only when a pin changes. No Windows build or test on Kamino1,
and the build never installs, launches or alters an installed app.

**Persistent build cache (`build_cache`, default `<checkout>/.build`, git-ignored).** The canonical build owns it; later builds after a `git pull` reuse it and do not
reinstall, re-download or rebuild unchanged dependencies:
- `runtime/python`: the Python runtime + locked packages. If it does not exist, the `runtime` phase builds it ONCE (`bundle.provision_runtime`: private `runtime.work` dir, moved
  into place only after `.ck-runtime-provenance.json` is written, so a failed build never leaves a half-valid cache). If it exists, only its provenance (lock hash + recipe) is
  checked and every stage hardlinks it. A changed lock/recipe fails with an explicit message: move the old runtime away (it is never auto-deleted) or point `runtime_dir` at a new path.
- `wheels`: the ONE shared cache of the ORIGINAL pinned public wheels (torch, triton, nvidia-*), stored under their real wheel file names with a `.ok` marker written after the sha256
  (lock-pinned hash) was verified. The runtime install (`uv`, via `name @ file://<cached wheel> --hash=...`) and the online installer plan (`wheel_sources`) use the same cached bytes, so each
  wheel is downloaded once. Git-pinned `sam-2`/`utils3d` stay pinned git sources.
- `uv-cache`, `cargo-target` (Windows) and `ffmpeg/bin` (Windows; place the pinned FFmpeg here, or set `ffmpeg_dir`). Config keys `runtime_dir`, `wheel_cache`, `ffmpeg_dir`, `cargo_target_dir`
  override individual locations; `timeouts.runtime` (7200 s) bounds the one-time runtime phase.

### Internal: `packaging/bundle.py`
The low-level toolbox that `build_release.py` calls (`build_stage`, `make_linux_run`, `split_linux_run`, `make_windows_exe`, `make_online_*`, `validate_reused_runtime`).
Its CLI (`python packaging/bundle.py linux|windows|online-linux|online-windows|bundle --out DIR --ffmpeg-dir ...`) is for developing the build itself, not for routine builds.
Regarding build prerequisites (`uv`, system `rustc`/`cargo`, network for wheels/zig/FFmpeg/CUDA parts, Inno Setup 6 on Windows): see below.

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
(default prefix `~/.local/opt/Keyloom`). Each `--model-arg` takes ONE argument, forwarded verbatim (spaces/globs
preserved) to `scripts/setup_models.py`, e.g. `--model-arg --hf-cache --model-arg "/my cache"`. Those are the manager's
path overrides (`--data-dir/--v2-models-dir/--hf-cache/--cache-dir`); there is no separate offline-bundle importer in
the installer — an existing model tree is used by pointing these at it (the manager verifies by hash).
The prefix must be new/empty or an existing Keyloom install; any other non-empty folder is refused.
Flow: verify payload sha → extract into `PREFIX/.staging.*` → journal → bundled python validates manifest, glibc
floor and launches (imports + UI `--self-check`) → atomic `current` symlink switch → `~/.local/share/applications/
keyloom.desktop` (Exec scrubs PYTHONPATH/PYTHONHOME), `PREFIX/bin/{keyloom,keyloom-setup}`, `receipt.json`
→ full models (`setup_models.py --download --profile full`, then `--verify`). Any failure or Ctrl-C restores the previous
version/pointer/shortcuts (or removes everything on a first install); a crash leaves a journal that the next run
(`keyloom-setup recover`, also automatic) finishes.
Exit 0 = everything Ready; **3 = app installed but models incomplete/skipped**; 2 = refused/failed; 130 = interrupted.
Release upload (GitHub 2 GiB/asset limit): add `--split` to `bundle.py linux` to also write `X.run.partNNN` (<=1900 MiB each) +
`X.run.parts.sha256`. User: `sha256sum -c X.run.parts.sha256 && cat X.run.part[0-9][0-9][0-9] > X.run && sha256sum -c X.run.sha256 && chmod +x X.run`. The unsplit `.run` is unchanged.
**OS prerequisites (not bundled; checked before install, with an apt hint for Debian/Ubuntu/Mint):** GL/EGL, X11 or Wayland client,
xkbcommon, fontconfig, freetype, glib, dbus, libstdc++, and the proprietary NVIDIA driver (`libcuda.so.1`; a missing driver only warns,
since models can still be installed). Python, Rust, FFmpeg, the CUDA toolkit and GCC are NOT needed on the target.
Later: `PREFIX/bin/keyloom-setup {status [--verify] | models | repair | rollback | recover | uninstall}`;
server mode: `PREFIX/bin/keyloom-coordinator {serve|pair|revoke|abandon} ...` (runs the bundled `backend.coordinator`; `serve` only listens when started, loopback by default);
`X.run --repair` restores damaged files from the payload (models re-verified only with `--models full`).
Two versions are kept (active + previous). Concurrent installers on one prefix are not supported.

## Windows: `Keyloom-<id>-windows-x86_64-setup.exe` (Inno Setup, per-user by default)
Installs to `%LOCALAPPDATA%\Programs\Keyloom\versions\<id>`, then runs the same `lifecycle.py`
(validation, Start Menu shortcut, receipt, optional full models task with a visible console). Drive-root folders and an
already-installed identical build are refused; a failed launch validation sets a nonzero setup exit code
(`GetCustomSetupExitCode`) and the previous version's shim runs `recover` to delete the failed one. Windows needs no VC++ Redistributable: the Microsoft VC runtime DLLs are bundled beside python.exe and the UI. The NVIDIA driver
is the only prerequisite (missing driver warns). Per-user only (no elevation prompt). Inno slices are 1900 MiB: upload the exe plus every `-N.bin` and
the `.sha256` (one line per file); all must be in one folder to install.
**Runtime companion (offline Windows only).** The multi-GB `runtime/` is NOT inside the Inno set. It ships beside the setup exe as
`Keyloom-<id>-windows-x86_64-runtime.json` (schema 2: `runtime_id`, ordered shards with size, sha256 and the exact runtime files each owns; no path may
belong to two shards), `Keyloom-<id>-windows-x86_64-runtime-001.tgz`, `-002.tgz`, ... (INDEPENDENTLY compressed tar.gz shards over disjoint files of the SAME
stage `runtime/`, each <= 1900 MiB; pieces of one gzip stream could not be decompressed independently) and `...-runtime.sha256` (one line per file). Put the exe,
every `-N.bin` and all companion files in one folder. The setup embeds the metadata file's sha256 and the `runtime_id`, so a modified companion is refused. Shards are
compressed by a bounded pool of 8 workers (never more than 16; one task per shard) and extracted by the same bounded pool (`installer_payload.py runtime-extract
--workers N`, clamped to 8..16), with a combined and per-shard MiB/speed/elapsed line each second; directory pre-creation, the final rename to `runtime`, activation,
receipt/pointer updates and rollback stay serial after the workers. At install time,
after the new manifest and `lifecycle.py` are on disk and before activation, ONE decision runs (`runtime-available`): exit 0 attaches the installed runtime
as a whole directory (no companion file is opened, hashed or read); exit 1 (fresh install / really different runtime) runs the supervised extraction
(`installer_payload.py runtime-extract`: verifies each shard's size and sha256 while streaming into a private holder inside the new version folder, accepting only
the files the metadata assigns to that shard,
renames it to `runtime` only after everything verified, no time limit (only the 300 s no-progress stall guard), any worker error cancels the rest, process tree killed
and holder discarded on failure); any other result stops visibly with no fallback; a missing companion stops visibly.
Installation discovery (online Windows wrapper) is one read-only `installer_payload.py discover` run that checks, in order: /PREFIX; the folder recorded under the
Keyloom AppId, only if it holds `receipt.json`; else `Programs\Keyloom`. The checks run one after another, not in parallel. The offline setup's folder choice stays in
Inno Pascal because it runs before any bundled Python exists.
A fresh/changed-runtime Windows install takes as long as the extraction takes; it is stopped only if it shows no output for 300 s. The remaining
native Inno copy is the app files and the small bootstrap and is not timed. The Linux `.run` keeps its runtime in-file (separate gzip segment). Online installers never use this companion.
Add/Remove Programs
and the uninstaller come from Inno. Uninstall removes only the individual files Inno copied (no recursive directory entry, so an attached runtime link is never
traversed) plus, through the lifecycle's safe-unlink logic, the receipt-validated versions carrying our own manifest; the running version is removed by a detached `cmd`. The Start Menu shortcut targets
the exe directly, so it does not scrub PYTHONPATH (the Rust side must). Repair of damaged files on Windows =
reinstall after uninstall. `/TASKS="!models"` makes a silent app-only install; the checked-by-default `firewall` task adds the Windows Firewall rules (one UAC prompt), so unattended installs that must not prompt use `/MERGETASKS="!firewall"` (or `/TASKS="!models,!firewall"`). Linux `.run` installers configure UFW by default; `--no-firewall` skips it. Every install appends to `<prefix>/install.log` (a header per step: installer kind, version, time; all supervised output, wait lines and failure reasons); the app writes service errors to `<settings>/network/service.log`. The supervisor prints a declared wait reason once and then every 20 s (plus `wait finished`); the one-second heartbeat only runs outside waits. Native build and installation checks are recorded in the acceptance document below.

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

Linux: `X.run [--prefix DIR] [--repair]` (default `~/.local/opt/Keyloom`; needs only sh/tar/gzip/awk/sha256sum). The payload is extracted
into `PREFIX/.staging.*` (same filesystem, not a small `/tmp`).
Windows: per-user Inno exe (1900 MiB slices like the full installer: keep the exe and every `-N.bin` together; `.sha256` lists all); unpacks to `{tmp}`
with a visible console. `/PREFIX=DIR` and `/REPAIR=1` are accepted; otherwise the folder in the Keyloom AppId's Inno
registry key (`Inno Setup: App Path`), where the recorded folder is looked up under HKCU first, then HKLM, is used if it holds `receipt.json`, else `%LOCALAPPDATA%\Programs\Keyloom`. It has its own AppId, `Uninstallable=no` and no Add/Remove Programs
entry, so it never touches the full installer's entry; remove an online-only install with `<prefix>\bin\keyloom-setup.cmd uninstall`.

## Background autostart contract (written by the app, removed by uninstall)
The network host is configured during activation using the installed version's full Python runtime with the `STALL_SECONDS` (300 s)
hang guard. Existing identities, service state and explicit removal choices are preserved. Setup failures leave the app
installed and report that **Clients → Start / Refresh** is needed. This host coordinates discovery and queues; the rendering
client still needs Keyloom running, visibly or in its existing background mode.

Linux launches the stable shim `PREFIX/bin/keyloom`. Windows launches the GUI exe directly; activation and rollback rewrite an existing owned entry. Linux:
`$XDG_CONFIG_HOME/autostart/keyloom.desktop` containing `X-Keyloom-Owner=<sha256(str(PREFIX))[:16]>` (`lifecycle.owner_id`).
Windows: `HKCU\\...\\Run` value `Keyloom` whose command points into this prefix. `lifecycle.remove_autostart` deletes only a matching entry.

## Preserved on uninstall/update/repair
Projects, outputs, models, HF cache and settings live outside the install prefix and are never
removed. Uninstall trusts only `receipt.json` and refuses a prefix without one.

## Tests
`bash rust-ui/tools/isolated-test.sh /usr/bin/timeout 14 .venv/bin/python -m pytest -p no:cacheprovider -q tests/test_packaging.py`
(tiny fake payloads; no network, runtime or models).

## Acceptance evidence
See [local workflow acceptance](../docs/design/rust-ui/local-workflow-acceptance.md) for the exact installer builds, installed-file checks, model verification and real GPU workflow results. Unit tests use tiny fixtures; they are not the evidence for native installation or GPU acceptance.

## Old CorridorKey installs

Keyloom never reads or changes a CorridorKey install; remove old CorridorKey folders by hand.

## Monitored commands (`run_monitored.py`)
`python packaging/run_monitored.py --deadline SECS [--label NAME] [--phase NAME] -- cmd arg...` supervises one foreground command: required
deadline, flushed `| line` streaming of stdout+stderr, a one-second `heartbeat` (the supervisor is alive and how long the child has been quiet; it is not
child progress), final `exit code=N elapsed=Ts`, exit 124 on deadline after killing the child's process tree (also swept after a normal exit);
SIGTERM/SIGHUP/Ctrl-C clean up the same way (exit 143/129/130). The argv and environment are never printed (`--label` is the display name); the child's
own output is passed through unredacted.
Default mode (Linux): stdin is `/dev/null`, the child gets its own session, and cleanup SIGSTOP/SIGKILLs every descendant found via `/proc` PPid (even
after `setsid()`) plus the child's process group. A descendant already reparented to init before the scan cannot be found.
`--interactive` (Linux only; refused on Windows): the child inherits stdin and stays in the caller's session/process group, so it keeps the controlling
terminal and can prompt (e.g. `sudo`, UFW) on `/dev/tty` itself. The tool never reads, forwards, logs or stores input, and does not use `killpg` in this mode
(only the child's own descendants are signalled). It has only been tested with pipes, never with a real terminal. Do not feed passwords through
pipes or `sudo -S`. Windows cleanup (`taskkill /T /F`) is untested best effort and prints a warning. Tests: `tests/test_run_monitored.py`.

### Canonical build reporting and timing policy
`build_release.py` runs every phase child through `run_monitored.run`: child stdout+stderr stream live to the console AND `build.log` as `| line`
(unterminated output is shown at the next heartbeat), with a one-second `heartbeat phase=... quiet Ns` line (supervisor liveness, not child progress)
and a final `exit code=N elapsed=Ts`; a phase timeout kills the whole child tree and fails the build (a child that itself exits 124 is a plain exit-code
failure). Command argv is no longer echoed to the console or log (display label = program name); `build-result.json` is unchanged and still lists each
phase command.

**Build phase deadlines (user-approved exception to the 10 s ordinary-command gate).** `Build.run` gives each phase child the effective deadline
min(that phase's configured budget in `timeouts`, what is left of the finite `deadline_seconds`); there is no other cap, nothing is lengthened silently, and
the start of `build.log` prints the budgets in force (defaults in `build_release.py` `TIMEOUTS`; `deadline_seconds` default 14400 s). Output is live with a
one-second heartbeat in the console and `build.log`. The only checks are the staged app self-check, the firewall-helper import gate, `online.py --help`
and artifact sha256 verification; the build runs no tests and does no test discovery (the source fingerprint prunes test directories before traversing).
This policy applies to build phases only; installers have no install-time budget, only the 300 s no-progress stall guard (live output and a one-second heartbeat stay).
Windows `taskkill` cleanup is unproven.
