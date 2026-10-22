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
