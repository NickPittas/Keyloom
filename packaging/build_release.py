#!/usr/bin/env python3
"""Canonical installer build: ONE command builds the offline AND online installers for THIS OS from ONE staged payload.

    python packaging/build_release.py [--config FILE] [--out-root DIR] [--preflight-only]

Phases (each bounded, logged, run as a child process): preflight -> ui -> stage -> offline -> online -> verify.
The stage is built once (one manifest, one launch self-check); both installers are made from that same stage, so they
carry the same version_id. Output: <out-root>/<run-id>/{build.log, build-result.json, stage/, installers}.
Exit 0 only if every artifact exists, is complete and matches its checksum. The low-level packaging/bundle.py
stays the internal toolbox; agents must not compose its steps by hand.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import os
import platform
import re
import shutil
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import bundle  # noqa: E402
import run_monitored  # noqa: E402

REPO, IS_WIN, PLATFORM = bundle.REPO, bundle.IS_WIN, bundle.PLATFORM
DEFAULT_CONFIG = Path.home() / ".config" / "keyloom-build.json"  # outside the repo: never staged into a payload
SLICE = 1992294400  # DiskSliceSize in both .iss templates (1900 MiB)
TIMEOUTS = {"helper-load": 120, "ui": 1800, "runtime": 7200, "stage": 7200, "offline": 7200, "online": 7200}  # seconds per phase
KEYS = {"out_root", "build_cache", "ffmpeg_dir", "runtime_dir", "wheel_cache", "cargo_target_dir", "iscc", "python_version",
        "split", "deadline_seconds", "timeouts"}
FP_ROOTS = [*bundle.APP_DIRS, *bundle.APP_FILES, "ui/theme", "rust-ui/src", "rust-ui/tools", "rust-ui/Cargo.toml", "rust-ui/Cargo.lock"]
FP_SKIP = {"__pycache__", ".git", "target", "target-oldglibc", "dist", ".build", "checkpoints", "weights", "tests"}


class BuildError(Exception):
    pass


# ---- inputs ----------------------------------------------------------------------------------------------------

def source_fingerprint() -> tuple[str, int]:
    """Hash of the source files a payload is made from (path + content). Detects edits during a build."""
    h, n = hashlib.sha256(), 0
    found: list[Path] = []
    for root in FP_ROOTS:
        if FP_SKIP & set(Path(root).parts):  # an excluded root (e.g. tests) is never touched
            continue
        base = REPO / root
        if base.is_file():
            found.append(base)
        elif base.is_dir():
            # Prune excluded directories BEFORE descending: their contents are never listed, statted or opened.
            for dirpath, dirnames, filenames in os.walk(base):
                dirnames[:] = [d for d in dirnames if d not in FP_SKIP]
                found += [Path(dirpath, f) for f in filenames]
    for p in sorted(found):
        rel = p.relative_to(REPO)
        if not p.is_file() or p.suffix == ".pyc" or (p.suffix in bundle.WEIGHT_EXT and p.stat().st_size > 1_000_000):
            continue
        h.update(f"{rel.as_posix()}\0{bundle.sha256_file(p)}\n".encode())
        n += 1
    return h.hexdigest(), n


def tool_version(cmd: list[str]) -> str | None:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=15)
        return (r.stdout or r.stderr).strip().splitlines()[0] if r.returncode == 0 else None
    except (OSError, subprocess.SubprocessError, IndexError):
        return None


def git_state() -> dict:
    def git(*a):
        try:
            r = subprocess.run(["git", "-C", str(REPO), *a], capture_output=True, text=True, timeout=15)
            return r.stdout.strip() if r.returncode == 0 else None
        except (OSError, subprocess.SubprocessError):
            return None
    status = git("status", "--porcelain")
    return {"commit": git("rev-parse", "HEAD"), "dirty": None if status is None else bool(status)}


def load_config(path: Path | None, explicit: bool) -> dict:
    home = Path.home()
    cfg = {"out_root": REPO / "packaging/dist", "python_version": "3.11.16", "split": True, "deadline_seconds": 14400,
           "timeouts": dict(TIMEOUTS), "runtime_dir": None, "iscc": None, "cargo_target_dir": None,
           "build_cache": REPO / ".build"}
    if not IS_WIN:
        cfg["ffmpeg_dir"] = home / ".cache/ck-ffmpeg-pinned/bin"
        cfg["wheel_cache"] = home / ".cache/ck-toolchain/public-wheels-linux"
    if path and path.is_file() or explicit:
        try:
            raw = json.loads(path.read_text())
        except (OSError, ValueError) as e:
            raise BuildError(f"cannot read config {path}: {e}")
        if not isinstance(raw, dict) or set(raw) - KEYS:
            raise BuildError(f"config {path}: unknown keys {sorted(set(raw) - KEYS) if isinstance(raw, dict) else 'not an object'}; "
                             f"allowed: {sorted(KEYS)}")
        for k, v in raw.items():
            cfg[k] = {**cfg["timeouts"], **v} if k == "timeouts" else v
    # Persistent, git-ignored dependency cache inside the ONE checkout: downloaded/built once, reused by every later build
    # (a `git pull` changes only code). Each key can still point elsewhere.
    cfg["build_cache"] = Path(cfg["build_cache"]).expanduser().resolve()
    bc = cfg["build_cache"]
    cfg["runtime_dir"] = cfg["runtime_dir"] or bc / "runtime/python"
    cfg["wheel_cache"] = cfg.get("wheel_cache") or bc / "wheels"
    if IS_WIN:
        cfg["ffmpeg_dir"] = cfg.get("ffmpeg_dir") or bc / "ffmpeg/bin"
        cfg["cargo_target_dir"] = cfg["cargo_target_dir"] or bc / "cargo-target"
    for k in ("out_root", "ffmpeg_dir", "runtime_dir", "wheel_cache", "cargo_target_dir", "iscc"):
        if cfg.get(k):
            cfg[k] = Path(cfg[k]).expanduser()
    return cfg


# ---- running children ------------------------------------------------------------------------------------------

class Build:
    def __init__(self, run_dir: Path, deadline: float):
        self.dir, self.deadline = run_dir, time.monotonic() + deadline
        self.log = open(run_dir / "build.log", "a", buffering=1, encoding="utf-8")
        self.phases: list[dict] = []
        self.t0 = time.monotonic()

    def note(self, msg: str, console: bool = False) -> None:
        line = f"[{datetime.now(timezone.utc):%H:%M:%S}] {msg}"
        self.log.write(line + "\n")
        if console:
            print(line, flush=True)

    def run(self, phase: str, cmd: list[str], timeout: float, cwd: Path | None = None, env: dict | None = None) -> None:
        """One bounded child, supervised in the foreground by run_monitored: its stdout+stderr stream live to the console
        AND build.log, with a once-a-second heartbeat (supervisor liveness, not child progress). The whole process tree is
        killed on timeout/interrupt. The argv is not echoed (the display label is the program name).
        Effective deadline = min(this phase's configured budget, what is left of the overall build deadline); both are
        finite and printed at phase start. Build phases are the user-approved exception to the 10 s ordinary-command gate."""
        timeout = min(timeout, self.deadline - time.monotonic())
        rec = {"phase": phase, "command": cmd, "started": time.time()}
        self.phases.append(rec)
        self.note(f"phase {phase}: start (timeout {int(timeout)}s)", console=True)
        if timeout <= 0:
            rec["exit"] = "deadline"
            raise BuildError(f"phase {phase}: overall deadline reached before start")
        t = time.monotonic()
        info: dict = {}
        rc = run_monitored.run(cmd, timeout, os.path.basename(cmd[0]), phase, cwd=cwd, env={**os.environ, **(env or {})},
                               log=self.log, info=info)
        if info.get("timed_out"):
            rec["exit"] = "timeout"
            raise BuildError(f"phase {phase}: timed out after {int(timeout)}s (child tree killed; effective deadline is "
                             f"min(phase budget, remaining overall deadline))")
        rec.update(exit=rc, seconds=round(time.monotonic() - t, 1))
        if rc != 0:
            raise BuildError(f"phase {phase}: exit code {rc}")
        self.note(f"phase {phase}: ok ({rec['seconds']}s)", console=True)


# ---- artifact verification -------------------------------------------------------------------------------------

def read_sums(sidecar: Path) -> dict:
    sums = {}
    for line in sidecar.read_text().splitlines():
        m = re.fullmatch(r"([0-9a-f]{64})  (.+)", line)
        if not m:
            raise BuildError(f"{sidecar.name}: malformed line {line!r}")
        sums[m[2]] = m[1]
    return sums


def check_group(out: Path, main: Path, extras: list[Path], sidecar_name: str) -> list[Path]:
    """`main` + its companion files must each be non-empty and listed (with the right sha256) in the sidecar."""
    side = out / sidecar_name
    if not main.is_file() or not side.is_file():
        raise BuildError(f"missing artifact or checksum file: {main.name} / {sidecar_name}")
    sums, files = read_sums(side), [main, *extras]
    if set(sums) != {f.name for f in files}:
        raise BuildError(f"{sidecar_name} lists {sorted(sums)} but the files on disk are {sorted(f.name for f in files)}")
    for f in files:
        if f.stat().st_size == 0 or bundle.sha256_file(f) != sums[f.name]:
            raise BuildError(f"{f.name}: empty or checksum differs from {sidecar_name}")
    return [*files, side]


def numbered(out: Path, pattern: str, rx: str) -> list[Path]:
    found = sorted(out.glob(pattern))
    nums = [int(re.search(rx, f.name)[1]) for f in found]
    if nums != list(range(1, len(nums) + 1)):
        raise BuildError(f"{pattern}: numbered files are not contiguous from 1: {[f.name for f in found]}")
    return found


def verify_artifacts(out: Path, vid: str, split: bool) -> list[Path]:
    got: list[Path] = []
    if IS_WIN:
        for name, exe in (("offline", f"Keyloom-{vid}-windows-x86_64-setup"), ("online", f"Keyloom-online-{vid}-windows-x86_64")):
            bins = numbered(out, f"{exe}-*.bin", r"-(\d+)\.bin$")
            if any(b.stat().st_size > SLICE for b in bins):  # Inno packs whole files, so slices are <= DiskSliceSize
                raise BuildError(f"{name}: a slice exceeds {SLICE} bytes")
            got += check_group(out, out / f"{exe}.exe", bins, f"{exe}.exe.sha256")
        # The offline setup is useless without its runtime companion (the multi-GB runtime is not inside the Inno set): a missing,
        # incomplete or oversized companion is a producer failure, never a silently smaller release.
        meta_name, sums_name = bundle.runtime_companion_names(vid)
        meta = out / meta_name
        if not meta.is_file():
            raise BuildError(f"offline runtime companion metadata is missing: {meta_name}")
        parts = numbered(out, f"Keyloom-{vid}-windows-x86_64-runtime-[0-9][0-9][0-9].tgz", r"-(\d+)\.tgz$")
        listed = json.loads(meta.read_text())["shards"]
        if [p.name for p in parts] != [e["name"] for e in listed] or \
                any(p.stat().st_size != e["size"] or p.stat().st_size > bundle.PART_BYTES for p, e in zip(parts, listed)):
            raise BuildError("offline runtime companion parts are missing, misnamed, oversized or differ from the metadata sizes")
        got += check_group(out, meta, parts, sums_name)
    else:
        for run in (out / f"Keyloom-{vid}-linux-x86_64.run", out / f"Keyloom-online-{vid}-linux-x86_64.run"):
            got += check_group(out, run, [], run.name + ".sha256")
        if split:
            run = out / f"Keyloom-{vid}-linux-x86_64.run"
            parts = numbered(out, run.name + ".part[0-9][0-9][0-9]", r"\.part(\d+)$")
            if not parts or sum(p.stat().st_size for p in parts) != run.stat().st_size or \
                    any(p.stat().st_size > bundle.PART_BYTES for p in parts):
                raise BuildError("offline .run parts are missing, oversized or do not add up to the .run size")
            got += check_group(out, parts[0], parts[1:], run.name + ".parts.sha256")
    return got


# ---- orchestration ---------------------------------------------------------------------------------------------

def ui_binary_path(cfg: dict) -> Path:
    if IS_WIN:
        return (cfg["cargo_target_dir"] or REPO / "rust-ui/target") / "release/keyloom-ui.exe"
    return REPO / "rust-ui/target-oldglibc/x86_64-unknown-linux-gnu/release/keyloom-ui"


def preflight(cfg: dict, b: Build, res: dict) -> None:
    if not (REPO / "packaging/locks" / f"{PLATFORM}.txt").is_file():
        raise BuildError(f"no packaging lock for {PLATFORM}")
    for key in ("ffmpeg_dir", "wheel_cache"):
        if not cfg.get(key):
            raise BuildError(f"config key '{key}' is required on this OS (see packaging/build-config.example.json)")
    for tool in ("ffmpeg", "ffprobe"):
        if not (cfg["ffmpeg_dir"] / (tool + (".exe" if IS_WIN else ""))).is_file():
            raise BuildError(f"{tool} not found in ffmpeg_dir {cfg['ffmpeg_dir']} (Linux: sh packaging/linux/fetch-ffmpeg.sh)")
    if IS_WIN:
        res["tools"]["iscc"] = bundle.find_iscc(str(cfg["iscc"]) if cfg["iscc"] else None)  # SystemExit -> handled by caller
    if cfg["runtime_dir"].exists():  # the persistent runtime cache: validated, never rebuilt or deleted here
        res["inputs"]["runtime_lock_sha256"] = bundle.validate_reused_runtime(cfg["runtime_dir"])
    elif not shutil.which("uv"):
        raise BuildError(f"runtime cache {cfg['runtime_dir']} does not exist yet and `uv` was not found: cannot create it")
    cfg["wheel_cache"].mkdir(parents=True, exist_ok=True)
    b.note("preflight ok")


def check_binary(path: Path) -> None:
    magic = b"MZ" if IS_WIN else b"\x7fELF"
    if not path.is_file() or path.open("rb").read(len(magic)) != magic:
        raise BuildError(f"UI binary {path} is missing or not a {PLATFORM} executable")


# Loads the staged installer helper exactly as lifecycle does at install time (lifecycle._firewall), in a fresh
# isolated interpreter from an unrelated CWD. Only imports; firewall.run/remove are never called.
HELPER_LOAD = """
import importlib.util, os, sys
if not os.path.isfile(sys.argv[1] + "/app/packaging/firewall.py"):  # lifecycle._firewall() would raise FileNotFoundError
    raise SystemExit("staged firewall helper is missing or lacks run()/remove()/main()")
spec =importlib.util.spec_from_file_location("ck_lifecycle", sys.argv[1] + "/app/packaging/lifecycle.py")
lc = importlib.util.module_from_spec(spec); sys.modules["ck_lifecycle"] = lc; spec.loader.exec_module(lc)
fw = lc._firewall()
if fw is None or not all(callable(getattr(fw, n, None)) for n in ("run", "remove", "main")):
    raise SystemExit("staged firewall helper is missing or lacks run()/remove()/main()")
print("installer helper load ok")
"""


def helper_load_gate(b: Build, stage: Path, py: Path | str, timeout: float) -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as cwd:
        b.run("helper-load", [str(py), "-I", "-B", "-c", HELPER_LOAD, str(stage)], timeout, Path(cwd))


def run_build(cfg: dict, b: Build, res: dict, preflight_only: bool) -> None:
    t, py = cfg["timeouts"], sys.executable
    out = b.dir
    fp, nfiles = source_fingerprint()
    res["source"] = {**git_state(), "fingerprint_sha256": fp, "fingerprint_files": nfiles}
    res["inputs"] = {"cargo_lock_sha256": bundle.sha256_file(REPO / "rust-ui/Cargo.lock"),
                     "packaging_lock_sha256": bundle.sha256_file(REPO / "packaging/locks" / f"{PLATFORM}.txt"),
                     "pyproject_sha256": bundle.sha256_file(REPO / "pyproject.toml"), "python_version": cfg["python_version"]}
    res["tools"] = {"builder_python": sys.version.split()[0], "cargo": tool_version(["cargo", "--version"]),
                    "rustc": tool_version(["rustc", "--version"]), "uv": tool_version(["uv", "--version"])}
    res["platform"] = {"target": PLATFORM, "host": platform.platform()}
    preflight(cfg, b, res)
    if preflight_only:
        return
    # ui: Linux uses the pinned zig/old-glibc script offline; Windows builds natively against the configured target dir.
    ui = ui_binary_path(cfg)
    if IS_WIN:
        env = {"CARGO_TARGET_DIR": str(cfg["cargo_target_dir"])} if cfg["cargo_target_dir"] else {}
        b.run("ui", ["cargo", "build", "--release", "--locked", "--bin", "keyloom-ui"], t["ui"], REPO / "rust-ui", env)
    else:
        b.run("ui", ["sh", str(REPO / "packaging/linux/build-ui-old-glibc.sh")], t["ui"], REPO,
              {"PATH": "/usr/bin:/bin", "RUSTC": "/usr/bin/rustc", "CK_CARGO_FLAGS": "--locked --offline"})
    check_binary(ui)
    if not cfg["runtime_dir"].exists():  # first build only: create the persistent runtime ONCE (heavy wheels via the shared wheel cache)
        b.run("runtime", [py, str(Path(__file__).resolve()), "--_step", "runtime", "--runtime-dir", str(cfg["runtime_dir"]),
                          "--python-version", cfg["python_version"], "--wheel-cache", str(cfg["wheel_cache"])],
              t["runtime"], REPO, {"UV_CACHE_DIR": str(cfg["build_cache"] / "uv-cache")})
        res["inputs"]["runtime_lock_sha256"] = bundle.validate_reused_runtime(cfg["runtime_dir"])
    cmd = [py, str(REPO / "packaging/bundle.py"), "bundle", "--out", str(out), "--ffmpeg-dir", str(cfg["ffmpeg_dir"]),
           "--ui-binary", str(ui), "--python-version", cfg["python_version"], "--reuse-runtime", str(cfg["runtime_dir"])]
    b.run("stage", cmd, t["stage"], REPO)  # the single manifest + launch self-check (Python + UI)
    stage = out / "stage"
    man = json.loads((stage / "install-manifest.json").read_text())
    vid = man["version_id"]
    if man.get("platform") != PLATFORM or (out / "VERSION_ID").read_text() != vid:
        raise BuildError(f"stage manifest does not match this build (platform {man.get('platform')}, id {vid})")
    res["payload"] = {"version_id": vid, "app_version": man["app_version"], "platform": man["platform"],
                      "lock_sha256": man.get("lock_sha256"), "min_glibc": man.get("min_glibc")}
    helper_load_gate(b, stage, stage / ("runtime/python/python.exe" if IS_WIN else "runtime/python/bin/python3"), t["helper-load"])  # before ANY installer is packed
    step = [py, str(Path(__file__).resolve()), "--_step", "", "--stage", str(stage), "--out", str(out),
            "--wheel-cache", str(cfg["wheel_cache"])]
    if cfg["iscc"]:
        step += ["--iscc", str(cfg["iscc"])]
    if cfg["split"]:
        step += ["--split"]
    for phase in ("offline", "online"):
        step[step.index("--_step") + 1] = phase
        b.run(phase, list(step), t[phase], REPO)
    b.phases.append({"phase": "verify"})
    arts = verify_artifacts(out, vid, cfg["split"])
    res["artifacts"] = [{"name": a.name, "bytes": a.stat().st_size, "sha256": bundle.sha256_file(a)} for a in arts]
    if source_fingerprint()[0] != fp:
        raise BuildError("source files changed during the build; artifacts are not from one snapshot - rebuild")
    b.note(f"verify ok: {len(arts)} files", console=True)


def worker(a) -> int:
    """Child process for the offline/online phases: existing bundle.py functions on the already-built stage."""
    if a._step == "runtime":
        bundle.provision_runtime(a.runtime_dir.resolve(), a.python_version, a.wheel_cache.resolve())
        return 0
    stage, out, cache, iscc = a.stage.resolve(), a.out.resolve(), a.wheel_cache.resolve(), a.iscc
    if a._step == "offline":
        if IS_WIN:
            bundle.make_windows_exe(stage, out, iscc)
        else:
            run = bundle.make_linux_run(stage, out)
            if a.split:
                bundle.split_linux_run(run)
    else:
        (bundle.make_online_exe(stage, out, cache, True, iscc) if IS_WIN else bundle.make_online_run(stage, out, cache, True))
    return 0


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--config", type=Path, help=f"JSON config (default {DEFAULT_CONFIG} if present)")
    ap.add_argument("--out-root", type=Path, help="parent of the per-run directory (default packaging/dist)")
    ap.add_argument("--preflight-only", action="store_true", help="validate config/tools/inputs, build nothing")
    ap.add_argument("--_step", choices=["runtime", "offline", "online"], help=argparse.SUPPRESS)
    ap.add_argument("--runtime-dir", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--python-version", help=argparse.SUPPRESS)
    ap.add_argument("--stage", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--out", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--wheel-cache", type=Path, help=argparse.SUPPRESS)
    ap.add_argument("--iscc", help=argparse.SUPPRESS)
    ap.add_argument("--split", action="store_true", help=argparse.SUPPRESS)
    a = ap.parse_args(argv)
    if a._step:
        return worker(a)
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    cfg_err = None
    try:
        cfg = load_config(a.config or DEFAULT_CONFIG, explicit=a.config is not None)
    except BuildError as e:
        cfg, cfg_err = None, e
    out_root = (a.out_root or (cfg or {}).get("out_root") or REPO / "packaging/dist").expanduser().resolve()
    run_dir = out_root / datetime.now().strftime("%Y%m%d-%H%M%S")
    run_dir.mkdir(parents=True)
    res = {"schema": 1, "status": "failed", "run_dir": str(run_dir), "started": datetime.now(timezone.utc).isoformat(),
           "source": {}, "inputs": {}, "tools": {}, "platform": {}, "payload": {}, "artifacts": [], "phases": [],
           "notes": ["Payload identity (version_id) is deterministic for the same source/locks/runtime; installer "
                     "bytes (gzip/Inno timestamps) are not promised identical across builds.",
                     "Source is fingerprinted before/after the build; a change fails the build (no isolated copy)."]}
    b = Build(run_dir, 14400)
    rc, why = 1, None
    budgets = {**TIMEOUTS, **(cfg or {}).get("timeouts", {})}
    b.note(f"timing policy: user-approved long build phases, each with an explicit deadline {budgets} s and overall "
           f"{(cfg or {}).get('deadline_seconds', 14400)} s; live output + 1 s heartbeat to console and build.log; no unit tests "
           f"or test discovery (checks: staged self-check, firewall-helper import, online.py --help, artifact sha256).", console=True)
    try:
        if cfg_err:
            raise cfg_err
        b.deadline = time.monotonic() + cfg["deadline_seconds"]
        run_build(cfg, b, res, a.preflight_only)
        res["status"], rc = ("preflight-ok" if a.preflight_only else "ok"), 0
    except BuildError as e:
        why = str(e)
    except SystemExit as e:  # bundle.py helpers report problems this way
        why = str(e.code) if isinstance(e.code, str) else f"exit {e.code}"
    except KeyboardInterrupt:
        why, rc = "interrupted", 130
    except Exception as e:  # noqa: BLE001 - a build must always leave a log + result
        why = f"{type(e).__name__}: {e}"
    res.update(phases=b.phases, finished=datetime.now(timezone.utc).isoformat(), seconds=round(time.monotonic() - b.t0, 1),
               error=why, failed_phase=(b.phases[-1]["phase"] if why and b.phases else "preflight" if why else None))
    if why:
        res["artifacts"] = []  # never advertise outputs of a failed build as a release
        b.note(f"FAILED: {why}")
    (run_dir / "build-result.json").write_text(json.dumps(res, indent=1, default=str))
    b.log.close()
    print(f"{'OK' if not why else 'FAILED: ' + why}\n  log:    {run_dir / 'build.log'}\n  result: {run_dir / 'build-result.json'}")
    if not why and not a.preflight_only:
        print(f"  id:     {res['payload']['version_id']}")
        for x in res["artifacts"]:
            print(f"  {x['sha256'][:12]}  {x['bytes']:>12}  {x['name']}")
    return rc


if __name__ == "__main__":
    sys.exit(main())
