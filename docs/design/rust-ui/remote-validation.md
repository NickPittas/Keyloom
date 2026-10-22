# Remote worker validation

## Linux worker (`npittas@100.126.185.76`, hostname `mustafar-omarchy`)

Validated 2026-10-22 over an authenticated ssh ControlMaster socket (no keys installed, no auth config changed).

**Machine:** RTX 5090 (GPU 0, 32 GB) + RTX 3050 (GPU 1). Local disks SSD1/SSD2. `/mnt/Projects` is a CIFS mount of
`//100.122.195.120/Projects` and is the same share the UI host sees as `//192.168.100.10/Projects` (proved with a
cross-host marker file). On that mount hardlinks work and symlinks fail ("Operation not supported"); the worker stages
symlinks in local scratch, so this is fine.

**Deployment** (`/home/npittas/ck-linux-smoke/`, new, nothing existing touched, no global installs):
- `repo/` — worker code copied from the dirty local tree (`backend`, `CorridorKeyV2`, `CorridorKeyModule`, `modules`,
  `gvm_core`, `ui`, `tests`, `pyproject.toml`, `requirements.txt`, `.git`; files > 30 MB skipped).
- `models/` — the CorridorKeyV2 tree (4.8 GB). SHA-256 matches the local copy: `corridorkey_v2.safetensors`
  `c43596de98cc8866d577c8a9f9ea239acdf3f7a130ff24e25db4375a50590436`, `foundation_stats.safetensors`
  `924783ceddd293fd470126332ce328166faea6c4c8d2f36402d5b95187166af3`.
- `venv/` — uv, Python 3.12.14, torch 2.9.1+cu128, torchvision 0.24.1 + `requirements.txt` (without PySide6 and
  triton-windows; `utils3d` from its pinned git commit). ComfyUI's environment was not reused (numpy 2 / transformers 5 /
  no OpenEXR are incompatible with the pins).
- `bin/python` — `sh` wrapper that sets `CORRIDORKEY_V2_MODELS_DIR`, `CORRIDORKEY_V2_SCRATCH_DIR`
  (`~/ck-linux-smoke/scratch`) and `PYTHONDONTWRITEBYTECODE`, then `exec`s `venv/bin/python "$@"`. Use it as
  `Target.python`; `Target.repo` is `~/ck-linux-smoke/repo`. The app then needs no env vars on the remote.

**Source provenance.** The `worker_rev` in hello (`5859239d6361…`) is only the HEAD the deployment was based on; the
deployed code was a dirty working tree (uncommitted `backend/worker`, `key_job.py`, etc.), so do not read it as
identifying the code. Identity: sha256 of the sorted `sha256sum` listing of the 422 deployed files (the directories
above minus `__pycache__`, files < 30 MB) is `5cde0ca65fe83d2940c4433a86ee5d200e5aa4025229c25f36464593d7cd04f5`,
identical on the UI host and the remote at deploy time.

**Tests and outcomes**
1. `python -m backend.worker --gpu 0` + shutdown: hello with RTX 5090, `capabilities.key=true`, model SHA = local, exit 0.
2. `tests/gpu_smoke_key_job.py` on the remote (outputs in `/tmp`) and again with clip and outputs on the NAS: fresh job
   (7 frames Fg+Matte EXR, complete manifest), resume no-op, dense hint, cancel mid-publish then resume, `exists`
   rejection, exit 0.
3. `rust-ui/tests/linux_remote_smoke.rs` (real Rust `WorkerConn` + `Target::Ssh`, real system `ssh` through
   `tools/ck-linux-smoke-ssh.sh`, which only adds `-S $CK_SSH_SOCKET`): hello (GPU 0, 5090); job requiring the model SHA →
   Complete 7/7; cancel → Cancelled (3 published) then `mode=resume` → Complete (4 published, 3 skipped); EOF mid-job via
   `request_stop()` → `Done(Cancelled)` then exit 0, no job lock left, no worker process left; fresh connection +
   `Cmd::Shutdown` → exit 0. The ssh argv is the production `Target::ssh_args` output
   (`cd -- <repo> && exec <python> -m backend.worker --gpu 0`).

**Limits.** Synthetic 256×256 clip only. `StrictHostKeyChecking=yes` is bypassed by the control socket, so host-key
handling is untested. No reconnect-after-socket-loss test. Linux only; the transfer storage mode is not covered.

**Re-running test 3** (writes real output remotely; opt-in, ignored by default):
```
CK_SSH_SOCKET=<socket> CK_SMOKE_HOST=<ip> CK_SMOKE_USER=<user> CK_SMOKE_DEPLOY=/home/npittas/ck-linux-smoke \
CK_SMOKE_CLIP=<shared clip root> CK_SMOKE_OUT=<new shared dir> CK_SMOKE_SHA=<sha> \
CARGO_HOME=$HOME/.cargo RUSTC=/usr/bin/rustc /usr/bin/cargo test --offline --test linux_remote_smoke -- --ignored --nocapture
```

## Windows worker (`nick@192.168.100.152`, hostname `Kamino-3`)

Validated 2026-10-22 over an authenticated ssh ControlMaster socket (`/tmp/ck-windows-smoke-ssh`; no keys installed, no
auth config changed). OpenSSH default shell is `cmd.exe`; PowerShell 5 is available for the encoded launcher.

**Machine:** RTX 4090 (GPU 0, 24 GB), Windows with a ComfyUI-Easy-Install environment on `C:`.

**Deployment** (`C:\Users\Nick\ck-wsmoke-1022a\`, new task dir, nothing existing touched, no global installs):
- `repo/` — worker code; the four worker-runtime files are byte-verified (sha256) against the local checkpoint commit
  `9c4712a`: `backend/worker/__init__.py` `db644341…`, `backend/service/key_job.py` `c711f79f…`,
  `backend/frame_io.py` `2a803cb9…`, `backend/worker/__main__.py` `90255e99…`.
- `py/` — copy of the embedded Python 3.12.10 (`python312._pth` disabled by rename); `py\Lib\site-packages\ck.pth`
  appends the CorridorKey2 `runtime-deps` and the ComfyUI `python_embeded` site-packages (torch 2.11 cu130, scipy;
  runtime-deps' cv2 4.11 shadows ComfyUI's newer cv2). The original ComfyUI tree is untouched.
- `ext/` — task-local `openexr` 3.5.2 cp312/win_amd64 wheel installed with `pip --target` from an offline wheel
  (scp'd from the UI host; the single wheel provides both the `OpenEXR` and `Imath` modules, dependency numpy already
  present, no legacy pins), wired in via `py\Lib\site-packages\exr.pth`.
- Models are read from `…\ComfyUI\models\CorridorKey2\upstream`; hello reports `corridorkey_v2.safetensors`
  `c43596de…` (same as the UI host and the Linux deployment).

**Driver:** `rust-ui/tools/win_smoke_worker.py`. The launcher is the production `remote_command()` output wrapped in
`powershell.exe -NoLogo -NoProfile -NonInteractive -EncodedCommand …`. Timeout bookkeeping (hardened after the recorded
runs below): every non-worker remote command is capped at 15 s; **every single wait on the worker stream is capped at
15 s** (`WAIT_CAP` — longer silence fails); each invocation carries **one absolute monotonic stage budget**
(`--stage-budget`, required for `job`/`full`/`eofmid` because GPU model load alone exceeds 15 s — the longer bound must
be explicit, never a hidden default; `full`'s whole chain runs inside that one budget); accept-wait and drain loops
**fail fast on terminal/rejected events**; every worker exit is asserted `0` (`assert_exit` — nonzero or
"STILL RUNNING" fails the stage); the budget and elapsed time are printed and recorded in the report JSON. Verified by
the local mock selfcheck `rust-ui/tools/test_win_smoke_worker.py` (10 checks; `timeout 60 python3 …`). The 2026-10-22
recorded runs below used the pre-hardening driver (job-total default 120 s, 20–30 s idles) — outcomes stand; only the
bookkeeping changed, so no remote rerun was needed.

**Tests and outcomes**
1. `--stage hello` (+ EOF idle): RTX 4090, `capabilities.key=true`, model SHA match, exit 0.
2. `--stage full --fmt exr` (real EXR output through the production `_write_exr_float32` path): w1 `complete` 7/7 with
   a complete manifest; w2 `cancelled` at 3 published; w3 rejected `exists`; EOF-idle exit 0; reconnect + EOF mid-job
   drained to the terminal event. Job artifacts verified on disk: Fg `shot_*.exr` channels B/G/R all **FLOAT
   (float32)**, Matte Y **FLOAT**, compression **PIZ_COMPRESSION**, EXR magic `76 2f 31 01`;
   `.corridorkey_manifest.json` records `exr_pixel_type: float32`, `exr_compression: piz`, `complete: true`. (An earlier
   half-float round-trip pre-check exercised `frame_io.write_exr`'s HALF default — test-only, not the job path.)
3. `--stage eofmid` (driver fix, rerun of only this stage): stdin EOF mid-job **drains with a bounded deadline to the
   terminal `done` event** and asserts `outcome=cancelled`, exit 0, `.corridorkey_job.lock` released, and no owned
   pids left (published 0 — EOF landed during warm-up before the first publish).
4. scipy blocked-stdin deadlock probe (`dl.py`: native import while another thread blocks on stdin): imports in 0.7 s
   with the production PolledStdin; previously hung indefinitely.

**Models location without an env prefix (open, proposal verified).** `backend/v2_paths.py` resolves
`CORRIDORKEY_V2_MODELS_DIR` (read at call time) and otherwise falls back to `%LOCALAPPDATA%\EZ-CorridorKey2\CorridorKeyV2`.
The recorded runs still launch the worker with a test-only `set CORRIDORKEY_V2_MODELS_DIR=…&& ` cmd prefix. Proposal
(mechanism verified 2026-10-22 on the remote interpreter): a task-local
`py\Lib\site-packages\ckmodels.pth` whose import line does
`os.environ.setdefault("CORRIDORKEY_V2_MODELS_DIR", <upstream models dir>)` — site.py executes it at interpreter start,
so `Target.python = C:\Users\Nick\ck-wsmoke-1022a\py\python.exe` works exactly, with a byte-identical production
launcher and no env prefix; `setdefault` never overrides an explicit env. A production-fidelity alternative for a later
run is populating/junctioning the `%LOCALAPPDATA%` default itself. The next GPU run should switch to the `.pth` route
and drop the prefix — not yet exercised end-to-end.

**Limits.** NAS UNC paths (`\\192.168.100.10\Projects`) **not tested** over this worker (auth unresolved; no guessing).
The models-config item above is verified only at the interpreter level, not yet in a full GPU run. Single Windows host,
synthetic 256×256 7-frame clip; PowerShell progress records add CLIXML noise to stderr (harmless); runtime-deps' cv2
4.11 shadows ComfyUI's newer cv2 (worker EXR path uses the OpenEXR bindings, not cv2). Not deployment-ready: the NAS
item and the `.pth`-route GPU run must land first.
