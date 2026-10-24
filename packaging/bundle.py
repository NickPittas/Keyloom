"""Build a Keyloom (powered by CorridorKey) standalone bundle and its OS installer.

    python packaging/bundle.py bundle   --out dist/            # stage dir (runtime + app + UI + FFmpeg)
    python packaging/bundle.py linux    --out dist/            # stage + self-extracting .run installer
    python packaging/bundle.py windows  --out dist/            # stage + Inno Setup .exe (build on Windows)
    python packaging/bundle.py online-linux|online-windows --out dist/ [--stage S] [--wheel-cache DIR]
        (online installer: bundles Keyloom itself, leaves out only torch/triton/nvidia-* files that the public PyPI /
         PyTorch wheels reproduce byte for byte; S = a finished stage, else one is built first)

Build host = target OS: the runtime is installed with `uv` from the pinned per-OS lock
(packaging/locks/*.txt), and git is needed for the two git-pinned dependencies. No model
weights are bundled; the installer fetches them through scripts/setup_models.py only.
Stdlib only (plus the external tools above); each function is importable for tests.
"""
from __future__ import annotations

import argparse
import csv
import email.parser
import hashlib
import html
import json
import os
import posixpath
import re
import shutil
import subprocess
import sys
import tarfile
import threading
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
IS_WIN = os.name == "nt"
BUILD_SELFCHECK_TIMEOUT = 300  # seconds, finite; staged-tree self-check + UI check at build time only
PLATFORM = "windows-x86_64" if IS_WIN else "linux-x86_64"
APP_DIRS = ["backend", "CorridorKeyModule", "CorridorKeyV2", "gvm_core", "sam2_tracker",
            "VideoMaMaInferenceModule", "modules", "scripts", "packaging"]
APP_FILES = ["pyproject.toml", "README.md"]
WEIGHT_EXT = {".pt", ".pth", ".ckpt", ".safetensors", ".onnx", ".npz", ".bin", ".gguf"}
SKIP_DIRS = {"__pycache__", ".git", "tests", "checkpoints", "weights", "locks", "dist"}
SKIP_PACKAGING = {"locks"}  # lock files are recorded by hash, not shipped twice


def sha256_file(p: Path) -> str:
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for c in iter(lambda: f.read(1 << 20), b""):
            h.update(c)
    return h.hexdigest()


def _ignore(directory: str, names: list[str]) -> set[str]:
    out = set()
    for n in names:
        p = Path(directory) / n
        if n in SKIP_DIRS and p.is_dir() or n.endswith(".pyc"):
            out.add(n)
        elif p.is_file() and p.suffix in WEIGHT_EXT and p.stat().st_size > 1_000_000:
            out.add(n)  # never ship model weights; the shared manager owns them
    return out


def app_version() -> str:
    return tomllib.loads((REPO / "pyproject.toml").read_text())["project"]["version"]


def stage_app(stage: Path) -> None:
    app = stage / "app"
    for d in APP_DIRS:
        shutil.copytree(REPO / d, app / d, ignore=_ignore, symlinks=True)
    for f in APP_FILES:
        shutil.copy2(REPO / f, app / f)
    helper = app / "rust-ui" / "tools"
    helper.mkdir(parents=True)
    shutil.copy2(REPO / "rust-ui/tools/ck_import.py", helper / "ck_import.py")
    (stage / "share").mkdir(exist_ok=True)
    shutil.copy2(REPO / "ui/theme/keyloom.png", stage / "share/keyloom.png")
    shutil.copy2(REPO / "ui/theme/keyloom.ico", stage / "share/keyloom.ico")


def stage_ui(stage: Path, binary: Path | None, build: bool) -> None:
    exe = "keyloom-ui.exe" if IS_WIN else "keyloom-ui"
    binary = binary or REPO / "rust-ui/target/release" / exe
    if build:
        subprocess.run(["cargo", "build", "--release", "--locked", "--bin", "keyloom-ui"],
                       cwd=REPO / "rust-ui", check=True)
    if not binary.is_file():
        raise SystemExit(f"UI binary not found: {binary} (pass --ui-binary or --build-ui)")
    (stage / "bin").mkdir(exist_ok=True)
    shutil.copy2(binary, stage / "bin" / exe)


def stage_ffmpeg(stage: Path, ffmpeg_dir: Path) -> None:
    # backend/ffmpeg_tools/discovery.py looks in <helper>/tools/ffmpeg/bin first.
    dest = stage / "app/tools/ffmpeg/bin"
    dest.mkdir(parents=True)
    for tool in ("ffmpeg", "ffprobe"):
        src = ffmpeg_dir / (tool + (".exe" if IS_WIN else ""))
        if not src.is_file():
            raise SystemExit(f"{src} missing: --ffmpeg-dir must hold ffmpeg and ffprobe (static build)")
        shutil.copy2(src, dest / src.name)
    for notice in [*ffmpeg_dir.glob("LICENSE*"), *ffmpeg_dir.glob("GPLv*.txt"), *ffmpeg_dir.glob("readme*.txt"),
                   *ffmpeg_dir.parent.glob("LICENSE*"), *ffmpeg_dir.parent.glob("README*"),
                   *ffmpeg_dir.parent.glob("GPLv*.txt"), *ffmpeg_dir.parent.glob("readme*.txt")]:
        shutil.copy2(notice, dest.parent / notice.name)
    notices = [p for p in dest.parent.iterdir() if p.is_file()]
    if not any("GNU GENERAL PUBLIC LICENSE" in p.read_text(errors="replace").upper() for p in notices):
        raise SystemExit(f"no GPL licence text found next to {ffmpeg_dir}; refusing to bundle FFmpeg without it")


VC_DLLS = ("vcruntime140.dll", "vcruntime140_1.dll", "msvcp140.dll", "msvcp140_1.dll", "msvcp140_2.dll",
           "msvcp140_codecvt_ids.dll", "vcomp140.dll", "concrt140.dll")


def stage_vc_runtime(stage: Path) -> None:
    """Windows: no host VC++ Redistributable needed. The Rust UI imports VCRUNTIME140 and torch/c10 import MSVCP140;
    the Microsoft redistributable DLLs ship inside the PySide6 wheel and are placed next to python.exe and the UI."""
    src = stage / "runtime/python/Lib/site-packages/PySide6"
    py = stage / "runtime/python"
    for n in VC_DLLS:
        f = src / n
        if not f.is_file():
            raise SystemExit(f"{f} missing: cannot bundle the Visual C++ runtime")
        # Reused runtimes are hardlinked: replacing a DLL must not mutate the source runtime.
        (py / n).unlink(missing_ok=True)
        shutil.copy2(f, py / n)
        if n.startswith("vcruntime"):
            shutil.copy2(f, stage / "bin" / n)


PROVENANCE = ".ck-runtime-provenance.json"  # next to the interpreter: lock + build recipe + extension ABI hash
ARCHS = "7.5;8.0;8.6;8.9;9.0+PTX"
# Bump when ANY runtime build step changes; a reused runtime must carry the same recipe.
RUNTIME_RECIPE = ("r3: wheels --python-platform manylinux_2_28 from the hashed lock; sam-2 git sdist built "
                  "--no-cache --no-build-isolation against the installed torch with CUDA 12.8.1 redist nvcc "
                  f"with conda-forge GCC 13 + glibc 2.17 sysroot as nvcc host compiler (SAM2_BUILD_CUDA=1 SAM2_BUILD_ALLOW_ERRORS=0, TORCH_CUDA_ARCH_LIST={ARCHS})")
CUDA_REDIST = "https://developer.download.nvidia.com/compute/cuda/redist/"
CUDA_PARTS = [  # official CUDA 12.8.1 redistributables (matches torch 2.9.1+cu128), sha256 from redistrib_12.8.1.json
    ("cuda_nvcc/linux-x86_64/cuda_nvcc-linux-x86_64-12.8.93-archive.tar.xz", "9961b3484b6b71314063709a4f9529654f96782ad39e72bf1e00f070db8210d3"),
    ("cuda_cudart/linux-x86_64/cuda_cudart-linux-x86_64-12.8.90-archive.tar.xz", "8d566b5fe745c46842dc16945cf36686227536decd2302c372be86da37faca68"),
    ("cuda_cccl/linux-x86_64/cuda_cccl-linux-x86_64-12.8.90-archive.tar.xz", "0740e9e01e4f15e17c5ab8d68bba4f8ec0eb6b84edccba4ac45112d2d2174e4b"),
]


def toolchain_dir() -> Path:
    return Path(os.environ.get("CK_TOOLCHAIN") or Path.home() / ".cache/ck-toolchain")


def cuda128_home(tc: Path | None = None) -> Path:
    """Private, user-local CUDA 12.8.1 toolkit (nvcc + cudart + cccl) for building sam-2's extension.
    The host toolkit (any other major) is never used: torch refuses a mismatched nvcc and the .so would
    link the wrong libcudart. Downloads are sha256-pinned and idempotent."""
    import urllib.request
    import tarfile as _tar
    tc = tc or toolchain_dir()
    home = tc / "cuda-12.8.1"
    if (home / "bin/nvcc").is_file() and (home / ".ck-complete").read_text() == "".join(h for _, h in CUDA_PARTS) \
            if (home / ".ck-complete").is_file() else False:
        return home
    shutil.rmtree(home, ignore_errors=True)
    tc.mkdir(parents=True, exist_ok=True)
    tmp = tc / "cuda-12.8.1.tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    tmp.mkdir()
    for rel, sha in CUDA_PARTS:
        f = tc / Path(rel).name
        if not f.is_file() or sha256_file(f) != sha:
            urllib.request.urlretrieve(CUDA_REDIST + rel, f)
            if sha256_file(f) != sha:
                f.unlink()
                raise SystemExit(f"CUDA component checksum mismatch: {rel}")
        with _tar.open(f) as t:
            members = [m for m in t.getmembers()]
            for m in members:
                m.name = m.name.split("/", 1)[1] if "/" in m.name else ""
            t.extractall(tmp, members=[m for m in members if m.name], filter="data")
    if not (tmp / "lib64").exists():
        (tmp / "lib64").symlink_to("lib")
    (tmp / ".ck-complete").write_text("".join(h for _, h in CUDA_PARTS))
    tmp.rename(home)
    return home


MAMBA_URL = "https://github.com/mamba-org/micromamba-releases/releases/download/2.9.0-0/micromamba-linux-64"
MAMBA_SHA = "366cd9cd8be14df1ab8ed50352a82111082a36686b2d389fdb79a92c3fafb3e3"  # the versioned binary, not "latest"
GCC_SPECS = ["gxx_linux-64=13.4.0", "gcc_linux-64=13.4.0", "sysroot_linux-64=2.17"]
GCC_MARK = ".ck-toolchain-complete.json"


def gcc13_dir(tc: Path | None = None) -> Path:
    """Private conda-forge GCC 13.4.0 + glibc-2.17 sysroot (no root, no host changes). nvcc 12.8 cannot parse the
    host's GCC 16 headers. Reused only if the completion marker matches the pinned specs and the compiler reports 13.4.0."""
    import urllib.request
    tc = tc or toolchain_dir()
    mm = tc / "mamba"
    env = mm / "gcc13"
    cxx = env / "bin/x86_64-conda-linux-gnu-g++"
    mark = env / GCC_MARK
    if cxx.is_file() and mark.is_file():
        try:
            if json.loads(mark.read_text()).get("specs") == GCC_SPECS and \
                    "13.4.0" in subprocess.run([str(cxx), "--version"], capture_output=True, text=True, timeout=10).stdout:
                return env
        except (OSError, ValueError, subprocess.SubprocessError):
            pass
    shutil.rmtree(env, ignore_errors=True)  # incomplete/unknown tree: ours alone, rebuilt from the pinned specs
    mm.mkdir(parents=True, exist_ok=True)
    exe = mm / "micromamba-2.9.0"
    if not exe.is_file() or sha256_file(exe) != MAMBA_SHA:
        tmp = exe.with_suffix(".part")
        urllib.request.urlretrieve(MAMBA_URL, tmp)
        if sha256_file(tmp) != MAMBA_SHA:
            tmp.unlink()
            raise SystemExit("micromamba checksum differs from the pinned one; review before updating MAMBA_SHA")
        tmp.chmod(0o755)
        tmp.rename(exe)
    subprocess.run([str(exe), "create", "-y", "-q", "-p", str(env), "--override-channels", "-c", "conda-forge", *GCC_SPECS],
                   check=True, env=dict(os.environ, MAMBA_ROOT_PREFIX=str(mm / "root")))
    ver = subprocess.run([str(cxx), "--version"], capture_output=True, text=True, timeout=10).stdout.splitlines()[0]
    if "13.4.0" not in ver:
        raise SystemExit(f"unexpected compiler after install: {ver}")
    mark.write_text(json.dumps({"specs": GCC_SPECS, "gxx": ver, "micromamba": "2.9.0"}))
    return env


def sam2_build_env(site: Path, home: Path) -> dict:
    incs = [str(d) for d in sorted((site / "nvidia").glob("*/include"))]  # cublas/cusparse/... headers torch includes
    env = dict(os.environ)
    for k in ("CUDA_PATH", "CUDA_ROOT", "CUDNN_HOME"):
        env.pop(k, None)
    gcc = gcc13_dir()
    cxx, cc = gcc / "bin/x86_64-conda-linux-gnu-g++", gcc / "bin/x86_64-conda-linux-gnu-gcc"
    env.update(CC=str(cc), CXX=str(cxx), CUDA_HOME=str(home), PATH=f"{home / 'bin'}{os.pathsep}{env.get('PATH', '')}",
               TORCH_CUDA_ARCH_LIST=ARCHS, SAM2_BUILD_CUDA="1", SAM2_BUILD_ALLOW_ERRORS="0",
               CPATH=os.pathsep.join(incs), LIBRARY_PATH=str(home / "lib"),
               NVCC_APPEND_FLAGS=f"-ccbin {cxx}")  # nvcc 12.8 supports <= GCC 14; the host has 16
    return env
PY_INDEXES = ["--index-url", "https://pypi.org/simple", "--extra-index-url", "https://download.pytorch.org/whl/cu128",
              "--index-strategy", "unsafe-best-match"]  # torch==x+cu128 exists only on the PyTorch index


def _link_or_copy(src, dst):
    try:
        os.link(src, dst)
    except OSError:
        shutil.copy2(src, dst)


def stage_runtime(stage: Path, py_version: str, uv: str = "uv", wheel_cache: Path | None = None) -> str:
    """Managed standalone CPython (relocatable) + hashed lock install. Returns lock sha256.

    With `wheel_cache`, the heavy pinned wheels (torch, triton, nvidia-*) are downloaded ONCE into that shared cache under
    their original names and installed by uv from those files (same bytes the online installer plan later reuses).

    py_version must be exact (e.g. 3.11.16): uv>=0.8 also creates a minor-version symlink next to
    the real directory, which must never end up in the bundle.
    """
    if not re.fullmatch(r"\d+\.\d+\.\d+", py_version):
        raise SystemExit(f"--python-version must be exact major.minor.patch, got {py_version!r}")
    lock = REPO / "packaging/locks" / f"{PLATFORM}.txt"
    tmp = stage / "runtime/_py"
    subprocess.run([uv, "python", "install", py_version, "--install-dir", str(tmp)], check=True)
    real = [d for d in tmp.iterdir() if d.is_dir() and not d.is_symlink() and d.name.startswith(f"cpython-{py_version}-")]
    if len(real) != 1:
        raise SystemExit(f"expected exactly one real cpython-{py_version} dir in {tmp}, found {[d.name for d in real]}")
    target = stage / "runtime/python"
    real[0].rename(target)
    shutil.rmtree(tmp)  # drops the minor-version links
    py = target / ("python.exe" if IS_WIN else "bin/python3")
    # Wheels are installed for an explicit old-glibc platform tag so cryptography & co. do not pick their
    # newest-glibc wheel; the two git-pinned sdists (sam-2, utils3d) are built in a second step.
    text = lock.read_text().replace("\\\n", " ")  # one logical requirement per line
    if wheel_cache:
        local = prefetch_heavy_wheels(py_version.split(".")[1], wheel_cache)
        text = "\n".join(
            f"{m[1]} @ {local[_norm(m[1])][0].resolve().as_uri()} --hash=sha256:{local[_norm(m[1])][1]}"
            if (m := re.match(r"([A-Za-z0-9][\w.-]*)==", l)) and _norm(m[1]) in local else l for l in text.splitlines()) + "\n"
    git = [l for l in text.splitlines() if " @ git+" in l]
    wheels = Path(tmp.parent / "wheels.lock.txt")
    wheels.write_text("\n".join(l for l in text.splitlines() if l not in git) + "\n")
    site = target / ("Lib/site-packages" if IS_WIN else f"lib/python{py_version.rsplit('.', 1)[0]}/site-packages")
    plat = [] if IS_WIN else ["--python-platform", "x86_64-manylinux_2_28", "--python-version", py_version.rsplit(".", 1)[0]]
    subprocess.run([uv, "pip", "install", "--python", str(py), "--target", str(site), *plat, "--no-deps", "--no-config",
                    *PY_INDEXES, "-r", str(wheels)], check=True)
    wheels.unlink()
    if not IS_WIN:
        # --no-cache: a cached wheel of this sdist (built earlier against another torch/CUDA) must never be reused.
        env = sam2_build_env(site, cuda128_home())
        flags = ["--no-cache", "--no-build-isolation"]
    else:
        # No CUDA extension on Windows: upstream SAM2 falls back to Python post-processing (selfcheck reports sam2_c=false).
        env, flags = dict(os.environ, SAM2_BUILD_CUDA="0", SAM2_BUILD_ALLOW_ERRORS="0"), ["--no-cache", "--no-build-isolation"]
    subprocess.run([uv, "pip", "install", "--python", str(py), "--target", str(site), "--no-deps", "--no-config",
                    *flags, *PY_INDEXES, *[l.strip() for l in git]], check=True, env=env)
    c_so = sorted(site.glob("sam2/_C*.so"))
    if not IS_WIN and len(c_so) != 1:
        raise SystemExit("sam-2 CUDA extension (sam2/_C*.so) was not built; refusing a runtime without it")
    prov = {"lock_sha256": sha256_file(lock), "recipe": RUNTIME_RECIPE,
            "toolchain": None if IS_WIN else {"cuda": "12.8.1", "cuda_parts": [r for r, _ in CUDA_PARTS], "gcc": GCC_SPECS,
                                              "micromamba": "2.9.0"},
            "sam2_c_sha256": sha256_file(c_so[0]) if c_so else None}
    (target / PROVENANCE).write_text(json.dumps(prov, indent=1))
    return prov["lock_sha256"]


GLIBC_RE = re.compile(rb"GLIBC_(\d+)\.(\d+)(?:\.(\d+))?\x00")


def min_glibc(stage: Path) -> str | None:
    """Highest GLIBC_x.y symbol version referenced by any ELF in the bundle (the install floor)."""
    best = (0, 0, 0)
    for root, _, names in os.walk(stage):
        for n in names:
            p = Path(root) / n
            if p.is_symlink() or not p.is_file():
                continue
            with open(p, "rb") as f:
                if f.read(4) != b"\x7fELF":
                    continue
                f.seek(0)
                tail = b""
                while chunk := f.read(8 << 20):
                    for m in GLIBC_RE.finditer(tail + chunk):
                        best = max(best, tuple(int(x or 0) for x in m.groups()))
                    tail = chunk[-32:]
    return ".".join(map(str, best[:2] if best[2] == 0 else best)) if best != (0, 0, 0) else None


def descriptor(stage: Path, version: str) -> None:
    py = "runtime/python/python.exe" if IS_WIN else "runtime/python/bin/python3"
    (stage / "keyloom-install.json").write_text(json.dumps({
        "schema": 1, "app_name": "Keyloom", "app_version": version,
        # models/cache/scratch/user_data null => platform defaults, shared across versions and
        # untouched by uninstall; the app root itself may be read-only.
        "roots": {"python": py, "helper": "app", "models": None, "cache": None,
                  "scratch": None, "user_data": None}}, indent=2) + "\n")


def runtime_id(files: dict) -> str:
    """Full 64-hex identity of the runtime: sha256 of the canonical JSON of the manifest `files` entries whose key starts
    with "runtime/" (keys kept verbatim, not stripped). Producer and every consumer must use exactly this function."""
    sub = {k: v for k, v in files.items() if k.startswith("runtime/")}
    return hashlib.sha256(json.dumps(sub, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def write_manifest(stage: Path, version: str, lock_sha: str, catalog_sha: str, glibc: str | None = None) -> str:
    files, total = {}, 0
    for root, dirs, names in os.walk(stage):
        dirs[:] = sorted(d for d in dirs if d != "__pycache__")  # bytecode is regenerated at run time, never part of identity
        for n in sorted(names + [d for d in dirs if (Path(root) / d).is_symlink()]):
            p = Path(root) / n
            rel = p.relative_to(stage).as_posix()
            if rel == "install-manifest.json":
                continue
            if p.is_symlink():
                files[rel] = {"symlink": os.readlink(p)}
            elif p.is_file():
                st = p.stat().st_size
                total += st
                files[rel] = {"size": st, "sha256": sha256_file(p)}
    tree = hashlib.sha256(json.dumps(files, sort_keys=True).encode()).hexdigest()[:10]
    vid = f"{version}-{tree}"
    (stage / "install-manifest.json").write_text(json.dumps({
        "schema": 1, "app_version": version, "version_id": vid, "platform": PLATFORM,
        "lock_sha256": lock_sha, "catalog_sha256": catalog_sha, "min_glibc": glibc, "total_bytes": total,
        "runtime_id": runtime_id(files), "files": files}, indent=1, sort_keys=True))
    return vid


def validate_reused_runtime(runtime_dir: Path) -> str:
    """Reject a reused runtime BEFORE anything is created or copied; returns its lock hash."""
    mark = runtime_dir / PROVENANCE
    if not mark.is_file():
        raise SystemExit(f"{runtime_dir} has no {PROVENANCE}: only runtimes built by this tool's current recipe can be reused")
    prov = json.loads(mark.read_text())
    lock_sha = prov.get("lock_sha256")
    if prov.get("recipe") != RUNTIME_RECIPE or lock_sha != sha256_file(REPO / "packaging/locks" / f"{PLATFORM}.txt"):
        raise SystemExit(f"runtime {runtime_dir} was built with a different recipe/lock (a pin changed). It is never deleted automatically: "
                         "move it away or set runtime_dir to a new path, then rerun the build to create the new runtime once")
    if prov.get("sam2_c_sha256"):
        c = sorted(runtime_dir.glob("lib/python*/site-packages/sam2/_C*.so"))
        if len(c) != 1 or sha256_file(c[0]) != prov["sam2_c_sha256"]:
            raise SystemExit("reused runtime's sam2 extension differs from its provenance record")
    return lock_sha


def provision_runtime(runtime_dir: Path, py_version: str, wheel_cache: Path, uv: str = "uv") -> str:
    """The persistent runtime cache: an existing `runtime_dir` is only validated (never touched, hashed beyond its provenance,
    or rebuilt); a missing one is built ONCE in a private sibling work dir and moved into place only after stage_runtime
    wrote its provenance, so a failed build leaves no half-valid cache. Returns the lock sha256."""
    if runtime_dir.exists():
        return validate_reused_runtime(runtime_dir)
    work = runtime_dir.parent / f"{runtime_dir.name}.work"  # ours alone; a leftover is an unfinished earlier attempt
    shutil.rmtree(work, ignore_errors=True)
    work.mkdir(parents=True)
    stage_runtime(work, py_version, uv, wheel_cache)
    (work / "runtime/python").rename(runtime_dir)
    shutil.rmtree(work, ignore_errors=True)
    return validate_reused_runtime(runtime_dir)


def build_stage(out: Path, *, ui_binary=None, build_ui=False, ffmpeg_dir: Path, py_version="3.11.16",
                runtime_dir: Path | None = None, uv="uv", smoke=True, max_glibc: str | None = "2.31",
                wheel_cache: Path | None = None) -> Path:
    """Returns the finished stage dir. `runtime_dir` (tests/offline) substitutes the uv install."""
    reused_lock = validate_reused_runtime(runtime_dir) if runtime_dir else None
    stage = out / "stage"
    if stage.exists():
        raise SystemExit(f"{stage} exists; refusing to overwrite (remove it or pick another --out)")
    stage.mkdir(parents=True)
    version = app_version()
    t0 = time.time()
    log = lambda m: print(f"[{time.time() - t0:6.0f}s] {m}", flush=True)
    log("staging app tree")
    stage_app(stage)
    stage_ui(stage, ui_binary, build_ui)
    stage_ffmpeg(stage, ffmpeg_dir)
    if runtime_dir:
        lock_sha = reused_lock
        # Hardlink copy: the multi-GB runtime is reused without duplicating bytes (same filesystem).
        shutil.copytree(runtime_dir, stage / "runtime/python", symlinks=True, copy_function=_link_or_copy)
    else:
        lock_sha = stage_runtime(stage, py_version, uv, wheel_cache)
    if IS_WIN:
        stage_vc_runtime(stage)
    descriptor(stage, version)
    catalog_sha = sha256_file(REPO / "backend/model_catalog.json")
    log("scanning ELF glibc floor")
    glibc = None if IS_WIN else min_glibc(stage)
    log(f"glibc floor {glibc}; writing manifest (hashing all files)")
    if glibc and max_glibc and tuple(map(int, glibc.split("."))) > tuple(map(int, max_glibc.split("."))):
        raise SystemExit(f"bundle needs glibc {glibc} (> allowed {max_glibc}); build the UI binary in an older-glibc "
                         "container (packaging/linux/build-ui-old-glibc.sh) or pass --max-glibc to accept a newer floor")
    vid = write_manifest(stage, version, lock_sha, catalog_sha, glibc)
    log("launch validation")
    if smoke:  # real launch validation of the staged tree, from an unrelated CWD
        sys.path.insert(0, str(REPO / "packaging"))
        import lifecycle
        # Build-only budget: the first import of a fresh staged tree is slow (cold Windows runtime). Installer
        # phases keep lifecycle.PHASE_TIMEOUT (10 s); this finite value applies to the staged build gate only.
        lifecycle.run_selfcheck(stage, timeout=BUILD_SELFCHECK_TIMEOUT)
    (out / "VERSION_ID").write_text(vid)
    return stage


BOOT_DROP_DIRS = {"site-packages", "test", "tests", "tkinter", "idlelib", "turtledemo", "ensurepip", "__pycache__", "include",
                  "share", "pkgconfig", "Scripts", "Tools", "libs", "tcl", "Doc"}  # the last five: Windows layout


def _boot_keep(rel: str) -> bool:
    """Stdlib + interpreter closure of the staged python: no site-packages (torch/CUDA/modules), tests, Tk, headers or pip."""
    parts = rel.split("/")
    if set(parts) & BOOT_DROP_DIRS or (parts[0] == "bin" and len(parts) > 1 and not parts[1].startswith("python")):
        return False
    if re.match(r"(_tkinter|tcl\d|tk\d)", parts[-1], re.I):
        return False
    return not (parts[0] == "lib" and len(parts) > 1 and re.match(r"(tcl|tk|itcl|Tix)", parts[1])) \
        and not rel.startswith(PROVENANCE)


def add_bootstrap(t: tarfile.TarFile, stage: Path) -> None:
    py = stage / "runtime/python"

    def flt(info):
        rel = info.name[len("boot/python"):].lstrip("/")
        return info if not rel or _boot_keep(rel) else None
    t.add(py, arcname="boot/python", filter=flt)
    t.add(REPO / "packaging/installer_payload.py", arcname="boot/installer_payload.py")


def make_boot_dir(stage: Path, dest: Path) -> Path:
    """Windows: the same minimal Python closure as the Linux boot segment, as a plain folder the Inno scripts extract to
    {tmp}\\kb (outside any runtime that will move): python/ is NOT nested, python.exe sits in the folder root."""
    shutil.rmtree(dest, ignore_errors=True)
    src = stage / "runtime/python"

    def ignore(d, names):
        rel = Path(d).relative_to(src).as_posix()
        return {n for n in names if not _boot_keep(n if rel == "." else f"{rel}/{n}")}
    shutil.copytree(src, dest, symlinks=True, ignore=ignore)
    shutil.copy2(REPO / "packaging/installer_payload.py", dest / "installer_payload.py")
    return dest


def _assemble_run(stub: str, segs: list, run: Path) -> None:
    """<stub><seg...>: fills the 12-wide @STUB_BYTES@ token (it cannot change the length it records), appends the segment files."""
    stub = stub.replace("@STUB_BYTES@", str(len(stub.encode())).ljust(12))
    with open(run, "wb") as f:
        f.write(stub.encode())
        for tgz, _, _ in segs:
            with open(tgz, "rb") as p:
                shutil.copyfileobj(p, f, 1 << 20)
            tgz.unlink()
    run.chmod(0o755)
    (run.parent / (run.name + ".sha256")).write_text(f"{sha256_file(run)}  {run.name}\n")


def compress_segments(out: Path, jobs) -> dict:
    """Independent segments are compressed concurrently by one bounded pool (one task per segment, never nested). The result is
    keyed by name, so the archive order is the caller's, never completion order."""
    def one(job):
        name, add = job
        tgz = out / f"{name}.tar.gz"
        with tarfile.open(tgz, "w:gz", compresslevel=3) as t:
            add(t)
        return name, (tgz, tgz.stat().st_size, sha256_file(tgz))
    with ThreadPoolExecutor(max_workers=min(SHARD_WORKERS, len(jobs))) as pool:
        return dict(pool.map(one, jobs))


def make_linux_run(stage: Path, out: Path) -> Path:
    """Layout: <stub><boot.tgz><app.tgz><runtime.tgz>, independent gzip streams. boot = a minimal launchable Python (boot/python,
    boot/installer_payload.py) that runs the install from OUTSIDE the payload the lifecycle later renames; app = payload/ without
    runtime/; runtime = payload/runtime/. An update that keeps the runtime reads only boot+app. The stub embeds each segment's
    byte offset (relative to the first byte after the stub), length, sha256, the runtime_id and its own length."""
    print("compressing payload (bootstrap, app, runtime segments; single-threaded gzip)", flush=True)
    m = json.loads((stage / "install-manifest.json").read_text())
    vid = m["version_id"]
    segs = {}
    segs = compress_segments(out, (("boot", lambda t: add_bootstrap(t, stage)),
                                   ("app", lambda t: t.add(stage, arcname="payload", filter=lambda i: None if i.name.startswith("payload/runtime") else i)),
                                   ("runtime", lambda t: t.add(stage / "runtime", arcname="payload/runtime"))))
    boot, app, rt = segs["boot"], segs["app"], segs["runtime"]
    stub = (REPO / "packaging/linux/installer-stub.sh").read_text()
    for k, v in {"@VERSION_ID@": vid, "@BOOT_OFFSET@": 0, "@BOOT_LEN@": boot[1], "@BOOT_SHA256@": boot[2],
                 "@APP_OFFSET@": boot[1], "@APP_LEN@": app[1], "@APP_SHA256@": app[2],
                 "@RUNTIME_OFFSET@": boot[1] + app[1], "@RUNTIME_LEN@": rt[1], "@RUNTIME_SHA256@": rt[2],
                 "@RUNTIME_ID@": m["runtime_id"]}.items():
        stub = stub.replace(k, str(v))
    # The token is 12 chars; the value is padded to 12, so filling it in cannot change the stub length it records.
    stub = stub.replace("@STUB_BYTES@", str(len(stub.encode())).ljust(12))
    run = out / f"Keyloom-{vid}-linux-x86_64.run"
    with open(run, "wb") as f:
        f.write(stub.encode())
        for tgz, _, _ in (boot, app, rt):
            with open(tgz, "rb") as p:
                shutil.copyfileobj(p, f, 1 << 20)
            tgz.unlink()
    run.chmod(0o755)
    (out / (run.name + ".sha256")).write_text(f"{sha256_file(run)}  {run.name}\n")
    return run


PART_BYTES = 1900 << 20  # GitHub release assets are limited to 2 GiB each


def split_linux_run(run: Path, part_bytes: int = PART_BYTES) -> list[Path]:
    """Optional distribution form: `<run>.part001..` (each <= part_bytes) + `<run>.parts.sha256` (every part). Reassemble with: cat <run>.part[0-9][0-9][0-9] > <run> && sha256sum -c <run>.sha256 && chmod +x <run>"""
    parts = []
    with open(run, "rb") as f:
        while True:
            first = f.read(min(1 << 20, part_bytes))
            if not first:
                break
            part = run.with_name(f"{run.name}.part{len(parts) + 1:03d}")
            with open(part, "wb") as target:
                target.write(first)
                left = part_bytes - len(first)
                while left:
                    chunk = f.read(min(1 << 20, left))
                    if not chunk:
                        break
                    target.write(chunk)
                    left -= len(chunk)
            parts.append(part)
    (run.with_name(run.name + ".parts.sha256")).write_text("".join(f"{sha256_file(p)}  {p.name}\n" for p in parts))
    return parts


def find_iscc(explicit: str | None) -> str:
    for c in [explicit, shutil.which("ISCC"), shutil.which("iscc"),
              r"C:\Program Files (x86)\Inno Setup 6\ISCC.exe", r"C:\Program Files\Inno Setup 6\ISCC.exe"]:
        if c and Path(c).is_file():
            return c
    raise SystemExit("Inno Setup 6 (ISCC.exe) not found. Install it on the Windows build host "
                     "(https://jrsoftware.org/isinfo.php) or pass --iscc PATH. No Windows installer was built.")


SHARD_WORKERS = 8   # bounded compression pool (8..16); one task per shard, never per file
SHARD_CAP = PART_BYTES - (64 << 20)   # uncompressed bytes per shard: leaves headroom so the .tgz stays <= PART_BYTES


def runtime_companion_names(vid: str) -> tuple[str, str]:
    stem = f"Keyloom-{vid}-windows-x86_64-runtime"
    return stem + ".json", stem + ".sha256"


def shard_name(vid: str, i: int) -> str:
    return f"Keyloom-{vid}-windows-x86_64-runtime-{i:03d}.tgz"


def plan_runtime_shards(files: dict, workers: int = SHARD_WORKERS, cap: int = SHARD_CAP) -> list:
    """Deterministic partition of the manifest's runtime/ files into DISJOINT shards (sorted paths, greedy fill). The target size
    is total/workers (at most `cap`) so the shards can be extracted in parallel; an independently compressed shard is the only
    thing that can be decompressed alone - pieces of one gzip stream cannot."""
    rt = sorted((k, v) for k, v in files.items() if k.startswith("runtime/"))
    if not rt:
        raise SystemExit("the manifest lists no runtime/ files")
    if any("symlink" in v for _, v in rt):
        raise SystemExit("the runtime contains symlinks, which the Windows runtime companion does not carry")
    total = sum(v["size"] for _, v in rt)
    target = min(cap, max(64 << 20, -(-total // max(workers, 1))))
    shards, cur, size = [], [], 0
    for k, v in rt:
        if v["size"] > cap:
            raise SystemExit(f"{k} is {v['size']} bytes: larger than one {cap}-byte shard can hold")
        if cur and size + v["size"] > target:
            shards.append(cur)
            cur, size = [], 0
        cur.append(k)
        size += v["size"]
    if cur:
        shards.append(cur)
    return shards


def make_runtime_companion(stage: Path, out: Path, part_bytes: int = PART_BYTES) -> Path:
    """Windows offline: the multi-GB runtime is NOT part of the Inno compressed set. The SAME stage/runtime (same runtime_id as
    the manifest) is written as independently compressed tar.gz SHARDS over disjoint manifest-listed files
    (`Keyloom-<id>-windows-x86_64-runtime-NNN.tgz`, each <= part_bytes = GitHub's per-asset limit), compressed in parallel by a
    bounded pool; `<stem>.json` (schema 2: runtime_id, ordered shards with size, sha256 and the exact files each owns) and
    `<stem>.sha256` (meta + shards). Because every shard is a complete gzip stream they can also be extracted in parallel.
    The meta file's own sha256 and the runtime_id are built into the setup program, so a modified companion cannot change what
    is accepted. Returns the metadata path."""
    m = json.loads((stage / "install-manifest.json").read_text())
    vid = m["version_id"]
    meta_name, sums_name = runtime_companion_names(vid)
    if not (stage / "runtime").is_dir() or not m.get("runtime_id"):
        raise SystemExit("stage has no runtime/ or the manifest has no runtime_id: cannot build the runtime companion")
    if m["runtime_id"] != runtime_id(m["files"]):
        raise SystemExit("manifest runtime_id does not match its runtime/ entries")
    plan = plan_runtime_shards(m["files"], SHARD_WORKERS, min(SHARD_CAP, part_bytes))
    sizes = [sum(m["files"][r]["size"] for r in rels) for rels in plan]
    workers = min(max(SHARD_WORKERS, 8), 16, len(plan))
    prog = [0] * len(plan)   # uncompressed bytes added per shard (each slot is written by its own worker only)
    stop, t0 = threading.Event(), time.monotonic()

    def tick():
        while not stop.wait(1):
            el, done = time.monotonic() - t0, sum(prog)
            print(f"[runtime-companion] {done >> 20}/{sum(sizes) >> 20} MiB compressed-in by {workers} worker(s), "
                  f"{done / 1048576 / max(el, 1e-9):.1f} MiB/s, elapsed {el:.0f}s; per shard MiB "
                  + " ".join(f"{i + 1:03d}:{prog[i] >> 20}/{sizes[i] >> 20}" for i in range(len(plan))), flush=True)

    def pack(i):
        path = out / shard_name(vid, i + 1)
        with tarfile.open(path, "w:gz", compresslevel=3) as t:
            for rel in plan[i]:   # sorted manifest order: deterministic member order
                t.add(stage / rel, arcname=rel, recursive=False)
                prog[i] += m["files"][rel]["size"]
        if path.stat().st_size > part_bytes:
            raise SystemExit(f"{path.name} is {path.stat().st_size} bytes, over the {part_bytes}-byte asset limit")
        return {"name": path.name, "size": path.stat().st_size, "sha256": sha256_file(path), "bytes": sizes[i], "files": plan[i]}
    print(f"compressing runtime companion: {len(plan)} independent shard(s), {workers} worker(s)", flush=True)
    threading.Thread(target=tick, daemon=True).start()
    try:
        with ThreadPoolExecutor(max_workers=workers) as pool:
            shards = list(pool.map(pack, range(len(plan))))   # results come back in shard order whatever finishes first
    finally:
        stop.set()
    owned = [r for s in shards for r in s["files"]]
    if len(owned) != len(set(owned)) or set(owned) != {k for k in m["files"] if k.startswith("runtime/")}:
        raise SystemExit("runtime shards do not cover the manifest's runtime files exactly once")
    meta = out / meta_name
    meta.write_text(json.dumps({"schema": 2, "version_id": vid, "runtime_id": m["runtime_id"], "shards": shards}, indent=1))
    (out / sums_name).write_text(f"{sha256_file(meta)}  {meta.name}\n" + "".join(f"{s['sha256']}  {s['name']}\n" for s in shards))
    return meta


def render_iss(stage: Path, out: Path, meta: Path) -> Path:
    m = json.loads((stage / "install-manifest.json").read_text())
    text = (REPO / "packaging/windows/keyloom.iss.in").read_text()
    boot = make_boot_dir(stage, out / "bw")
    for k, v in {"@VERSION_ID@": m["version_id"], "@APP_VERSION@": m["app_version"],
                 "@STAGE@": str(stage), "@OUT@": str(out), "@BOOT@": str(boot),
                 "@RUNTIME_ID@": m["runtime_id"], "@RUNTIME_META@": meta.name,
                 "@RUNTIME_META_SHA256@": sha256_file(meta)}.items():
        text = text.replace(k, v)
    iss = out / "keyloom.iss"
    iss.write_text(text, encoding="utf-8-sig")
    return iss


def make_windows_exe(stage: Path, out: Path, iscc: str | None) -> Path:
    if not IS_WIN:
        raise SystemExit("The Windows runtime/installer must be built on Windows (the bundle contains "
                         "Windows wheels and an Inno Setup compile). Nothing was built.")
    exe = find_iscc(iscc)
    meta = make_runtime_companion(stage, out)  # before ISCC: the setup program embeds the metadata's sha256 and the runtime_id
    iss = render_iss(stage, out, meta)
    subprocess.run([exe, "/Q", str(iss)], check=True)
    shutil.rmtree(out / "bw", ignore_errors=True)
    built = sorted(out.glob("Keyloom-*-setup.exe"))
    if not built:
        raise SystemExit("ISCC finished but produced no setup exe")
    stem = built[-1].name[:-len(".exe")]  # setup exe + every Inno disk slice (<stem>-1.bin ...), hashed together
    files = [built[-1], *sorted(out.glob(stem + "-*.bin"))]
    (out / (built[-1].name + ".sha256")).write_text("".join(f"{sha256_file(f)}  {f.name}\n" for f in files))
    return built[-1]


# ---- online installers: Keyloom itself is bundled; only the large public wheels are fetched at install time ----

HEAVY_MIN = 1 << 20  # stage files at least this big that belong to a heavy wheel are omitted from the slim payload
INDEXES = ["https://pypi.org/simple/", "https://download.pytorch.org/whl/cu128/"]  # same order as PY_INDEXES
HREF_RE = re.compile(r"""<a\s[^>]*?href\s*=\s*(?:"([^"]*)"|'([^']*)')""", re.I)


def _norm(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def _heavy(name: str) -> bool:
    # torchvision is small enough to stay bundled. sam-2's git-built CUDA extension is never a wheel: it stays in the payload.
    return name in ("torch", "triton", "triton-windows") or name.startswith("nvidia-")


def lock_pins() -> dict:
    """{normalized name: (version, {sha256, ...})} from the pinned per-OS lock."""
    pins = {}
    text = (REPO / "packaging/locks" / f"{PLATFORM}.txt").read_text().replace("\\\n", " ")
    for line in text.splitlines():
        m = re.match(r"([A-Za-z0-9][\w.-]*)==([^\s;]+)(.*)$", line)
        if m:
            pins[_norm(m[1])] = (m[2], set(re.findall(r"--hash=sha256:([0-9a-f]{64})", m[3])))
    return pins


def _get(url: str, tries: int = 3) -> bytes | None:
    """Page bytes, None for 404 (project not on this index)."""
    for i in range(tries):
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "keyloom-build"}), timeout=60) as r:
                return r.read()
        except urllib.error.HTTPError as e:
            if e.code == 404:
                return None
            err = e
        except OSError as e:
            err = e
        if i == tries - 1:
            raise SystemExit(f"cannot fetch {url}: {err}")
        time.sleep(2 ** i)


def _wheel_id(filename: str):
    """(normalized name, version, {py-abi-platform tags}) from a wheel file name (PEP 427)."""
    parts = filename[:-len(".whl")].split("-")
    if len(parts) not in (5, 6):
        raise ValueError(filename)
    tags = {f"{p}-{a}-{pl}" for p in parts[-3].split(".") for a in parts[-2].split(".") for pl in parts[-1].split(".")}
    return _norm(parts[0]), parts[1], tags


def _index_wheels(name: str):
    """(url, sha256 the index states, tags, version) of every wheel file of `name` on the indexes (hash NOT yet trusted)."""
    for index in INDEXES:
        page = _get(index + name + "/")
        if page is None:
            continue
        for m in HREF_RE.finditer(page.decode("utf-8", "replace")):
            url, frag = urllib.parse.urldefrag(urllib.parse.urljoin(index + name + "/", html.unescape(m[1] if m[1] is not None else m[2])))
            fn = urllib.parse.unquote(urllib.parse.urlsplit(url).path.rsplit("/", 1)[-1])
            if not fn.endswith(".whl"):
                continue
            try:
                n, v, t = _wheel_id(fn)
            except ValueError:
                continue
            if n == name and url.startswith("https://"):
                yield url, dict(urllib.parse.parse_qsl(frag)).get("sha256"), t, v


def find_wheel(name: str, version: str, tags: set, hashes: set) -> tuple[str, str]:
    """Exact public wheel URL + sha256: name, version and every platform tag must match what is installed, and the
    index-advertised hash must be one of the lock's pinned hashes (a hash only the index states is never trusted)."""
    for url, sha, t, v in _index_wheels(name):
        if v.lower() == version.lower() and t == tags and sha in hashes:
            return url, sha
    raise SystemExit(f"no public wheel for {name}=={version} {sorted(tags)} matches a hash in {PLATFORM}.txt on {INDEXES}")


def _tag_ok(tag: str, minor: str) -> bool:
    py, abi, plat = tag.split("-")
    if py not in (f"cp3{minor}", f"py3{minor}", "py3", "cp3") or abi not in (f"cp3{minor}", "abi3", "none"):
        return False
    if IS_WIN:
        return plat in ("win_amd64", "any")
    m = re.fullmatch(r"manylinux_2_(\d+)_x86_64", plat)  # the runtime is installed for x86_64-manylinux_2_28
    return plat in ("any", "manylinux2014_x86_64", "manylinux2010_x86_64", "manylinux1_x86_64") or bool(m and int(m[1]) <= 28)


def prefetch_heavy_wheels(minor: str, cache: Path) -> dict:
    """Download (once, into the shared `cache`) the ORIGINAL pinned public wheel of every heavy distribution (torch, triton,
    nvidia-*) that fits this target; the runtime install and the online installer plan then use these same cached bytes.
    Returns {normalized name: (path, sha256)}. A wheel is chosen only if the lock pins its hash."""
    jobs = [(n, v, hs) for n, (v, hs) in lock_pins().items() if _heavy(n)]

    def one(job):
        name, version, hashes = job
        cands = sorted({(url, sha) for url, sha, t, v in _index_wheels(name)
                        if v.lower() == version.lower() and sha in hashes and any(_tag_ok(x, minor) for x in t)})
        if not cands:
            raise SystemExit(f"no public wheel for {name}=={version} fits {PLATFORM} with a hash in {PLATFORM}.txt on {INDEXES}")
        url, sha = max(cands, key=lambda c: c[0].rsplit("/", 1)[-1])
        return name, (fetch_wheel(url, sha, cache), sha)
    with ThreadPoolExecutor(max_workers=min(4, max(len(jobs), 1))) as pool:
        return dict(pool.map(one, jobs))


_FETCH_GUARD = threading.Lock()
_FETCH_LOCKS: dict = {}


def fetch_wheel(url: str, sha: str, cache: Path) -> Path:
    """One writer per cache file: concurrent callers for the same sha256 queue on that sha's lock (the second finds the verified file)."""
    with _FETCH_GUARD:
        lock = _FETCH_LOCKS.setdefault(sha, threading.Lock())
    with lock:
        return _fetch_wheel(url, sha, cache)


def _fetch_wheel(url: str, sha: str, cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    fn = urllib.parse.unquote(urllib.parse.urlsplit(url).path.rsplit("/", 1)[-1])  # original wheel basename: uv/pip need a valid one
    if Path(fn).name != fn or not fn.endswith(".whl"):
        raise SystemExit(f"{url}: not a plain wheel file name")
    dest = cache / fn
    mark = cache / (fn + ".ok")  # written only after the sha256 was verified; unchanged size+mtime means no re-hash of a multi-GB wheel

    def trusted() -> bool:
        try:
            st, m = dest.stat(), json.loads(mark.read_text())
            return m == {"sha256": sha, "size": st.st_size, "mtime_ns": st.st_mtime_ns}
        except (OSError, ValueError):
            return False

    def stamp() -> Path:
        st = dest.stat()
        mark.write_text(json.dumps({"sha256": sha, "size": st.st_size, "mtime_ns": st.st_mtime_ns}))
        return dest
    if trusted():
        return dest
    if dest.is_file() and sha256_file(dest) == sha:
        return stamp()
    print(f"downloading {fn}", flush=True)
    part = dest.with_suffix(".part")
    for i in range(3):
        h, got, t0, last = hashlib.sha256(), 0, time.monotonic(), 0.0
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "keyloom-build"}), timeout=60) as r, \
                    open(part, "wb") as f:
                total = int(r.headers.get("Content-Length") or 0)
                for c in iter(lambda: r.read(1 << 20), b""):
                    h.update(c)
                    f.write(c)
                    got += len(c)
                    if (now := time.monotonic()) - last >= 1:  # real bytes, once a second (the 60 s socket timeout is the stall limit)
                        last = now
                        print(f"  {fn}: {got >> 20}/{total >> 20} MiB, {got / max(now - t0, 1e-6) / 1e6:.1f} MB/s, {now - t0:.0f}s", flush=True)
            break
        except OSError as e:
            if i == 2:
                raise SystemExit(f"cannot download {url}: {e}")
            time.sleep(2 ** i)
    if h.hexdigest() != sha:
        part.unlink()
        raise SystemExit(f"{url}: sha256 differs from the lock's pinned hash")
    part.replace(dest)
    print(f"downloaded {fn} ({dest.stat().st_size >> 20} MiB, sha256 verified)", flush=True)
    return stamp()


def _site_packages(stage: Path) -> Path:
    sites = [*stage.glob("runtime/python/Lib/site-packages"), *stage.glob("runtime/python/lib/python3.*/site-packages")]
    if len(sites) != 1:
        raise SystemExit(f"expected exactly one site-packages in {stage}/runtime/python, found {len(sites)}")
    return sites[0]


def wheel_sources(stage: Path, cache: Path) -> tuple[dict, set]:
    """Plan the online install: map every >=1 MiB regular stage file of the heavy wheels (torch, triton, nvidia-*)
    to its entry in the exact public wheel. Returns (wheel-sources.json content, mapped stage-relative paths).
    Fails listing any such file that is not mapped byte-for-byte (size + sha256 from the stage's install manifest)."""
    manifest = json.loads((stage / "install-manifest.json").read_text())
    files = manifest["files"]
    site = _site_packages(stage)
    site_rel = site.relative_to(stage).as_posix()
    pins = lock_pins()
    jobs = []   # stage 1 (serial, local reads only): which distributions need a public wheel, in sorted dist-info order
    for di in sorted(site.glob("*.dist-info")):
        meta = email.parser.Parser().parsestr((di / "METADATA").read_text(encoding="utf-8", errors="replace"), headersonly=True)
        name, version = _norm(meta["Name"] or ""), meta["Version"] or ""
        if not _heavy(name):
            continue
        big = []  # stage-relative regular files of this distribution, as RECORD lists them
        with open(di / "RECORD", newline="", encoding="utf-8") as f:
            for row in csv.reader(f):
                rel = posixpath.normpath(posixpath.join(site_rel, row[0])) if row else ".."
                if rel.startswith(".."):
                    continue
                p = stage / rel
                if p.is_relative_to(di):
                    continue  # Installer-generated metadata (notably RECORD) stays bundled verbatim.
                if p.is_file() and not p.is_symlink() and p.stat().st_size >= HEAVY_MIN:
                    big.append(rel)
        if not big:
            continue  # e.g. nvidia-ml-py: stays in the payload
        tags = {l.split(":", 1)[1].strip() for l in (di / "WHEEL").read_text().splitlines() if l.startswith("Tag:")}
        pin = pins.get(name)
        if not pin or pin[0].lower() != version.lower():
            raise SystemExit(f"{name} {version} is not pinned with that version in {PLATFORM}.txt")
        jobs.append((name, version, tags, pin[1], big))

    def run(job):
        """Stage 2, one independent task per distribution on ONE bounded pool: index lookup (pinned-hash check inside find_wheel),
        download to its own sha-named cache file (one writer per file, see fetch_wheel) and byte-for-byte hash of the mapped files.
        Returns (wheel record, problems); shared state is only read."""
        name, version, tags, hashes, big = job
        url, sha = find_wheel(name, version, tags, hashes)
        whl = fetch_wheel(url, sha, cache)
        problems, entries = [], {}
        with zipfile.ZipFile(whl) as z:
            zmap = {}
            for info in z.infolist():
                n = info.filename
                if n.split("/", 1)[0].endswith(".data"):
                    sub = n.split("/", 2)
                    if len(sub) < 3 or sub[1] not in ("purelib", "platlib"):
                        continue  # scripts/headers/data land outside site-packages
                    n_site = sub[2]
                else:
                    n_site = n
                if not info.is_dir() and ".." not in n_site.split("/"):
                    zmap[posixpath.join(site_rel, n_site)] = info
            for rel in big:
                info, want = zmap.get(rel), files.get(rel)
                if info is None:
                    problems.append(f"{rel}: not in {whl.name}")
                elif not want or "sha256" not in want:
                    problems.append(f"{rel}: not a regular file in the install manifest")
                else:
                    h, size = hashlib.sha256(), 0
                    with z.open(info) as src:
                        for c in iter(lambda: src.read(1 << 20), b""):  # 1 MiB at a time: bounded memory per worker
                            h.update(c)
                            size += len(c)
                    if (h.hexdigest(), size) != (want["sha256"], want["size"]):
                        problems.append(f"{rel}: bytes differ from {whl.name}:{info.filename}")
                    else:
                        entries[rel] = info.filename
        return {"url": url, "sha256": sha, "size": whl.stat().st_size, "files": entries}, problems
    with ThreadPoolExecutor(max_workers=min(SHARD_WORKERS, max(len(jobs), 1))) as pool:
        results = list(pool.map(run, jobs))   # job (sorted dist-info) order, whatever finished first
    wheels, mapped, problems = [], set(), []
    for rec, probs in results:   # stage 3 (serial): file ownership is decided in this fixed order, never by completion order
        problems += probs
        for rel in rec["files"]:
            if rel in mapped:
                problems.append(f"{rel}: claimed by two wheels")
        mapped.update(rec["files"])
        wheels.append(rec)
    if problems:
        raise SystemExit("heavy files not reproducible from public wheels:\n  " + "\n  ".join(problems))
    return {"schema": 1, "version_id": manifest["version_id"], "wheels": wheels}, mapped


def make_slim(stage: Path, dest: Path, mapped: set) -> Path:
    """Copy (hardlink where possible) the stage keeping symlinks, omitting ONLY the mapped regular files.
    Ordered by dependency, not serialised: (1) directories are created serially, parents first; (2) the independent file/symlink
    writes, each to its own distinct destination, run in chunks on ONE bounded pool (SHARD_WORKERS threads, one task per chunk of
    >=256 entries, so in-flight work is a few hundred small tuples, never one task per file); (3) directory metadata is restored
    serially afterwards, deepest first (a read-only directory must not block the file writes)."""
    dirs, ops = [], []
    for root, dnames, fnames in os.walk(stage):
        dnames.sort()
        r = Path(root)
        dest_dir = dest / r.relative_to(stage)
        dest_dir.mkdir(parents=True, exist_ok=True)
        dirs.append((r, dest_dir))
        for n in dnames:
            if (r / n).is_symlink():
                ops.append((r / n, dest_dir / n))
        for n in sorted(fnames):
            if (r / n).relative_to(stage).as_posix() not in mapped:
                ops.append((r / n, dest_dir / n))
    cancel = threading.Event()

    def one(src: Path, dst: Path) -> None:
        if cancel.is_set():
            return
        if src.is_symlink():
            os.symlink(os.readlink(src), dst)
        else:
            _link_or_copy(src, dst)

    def chunk(part):
        try:
            for src, dst in part:
                one(src, dst)
        except BaseException:
            cancel.set()  # a failing worker stops the others at their next entry
            raise
    step = max(256, -(-len(ops) // (SHARD_WORKERS * 8)))
    with ThreadPoolExecutor(max_workers=SHARD_WORKERS) as pool:
        list(pool.map(chunk, [ops[i:i + step] for i in range(0, len(ops), step)]))
    for r, d in reversed(dirs):
        shutil.copystat(r, d, follow_symlinks=False)
    files = json.loads((stage / "install-manifest.json").read_text())["files"]
    gone = [r for r in files if r not in mapped and not os.path.lexists(dest / r)]
    if gone or any(os.path.lexists(dest / r) for r in mapped):
        raise SystemExit(f"slim payload differs from the plan: missing {gone[:5]}")
    return dest


def make_wrapper(stage: Path, out: Path, cache: Path, smoke: bool) -> Path:
    """<out>/online-boot/keyloom-online/{payload/, wheel-sources.json}; the sources are never inside the payload."""
    wrap = out / "boot" / "k"  # Keep deep Qt paths within Inno's source-path limit.
    shutil.rmtree(wrap.parent, ignore_errors=True)
    wrap.mkdir(parents=True)
    sources, mapped = wheel_sources(stage, cache)
    make_slim(stage, wrap / "payload", mapped)
    (wrap / "wheel-sources.json").write_text(json.dumps(sources, indent=1))
    if smoke:
        py = wrap / "payload/runtime/python" / ("python.exe" if IS_WIN else "bin/python3")
        subprocess.run([str(py), "-I", "-B", str(wrap / "payload/app/packaging/online.py"), "--help"], check=True,
                       stdout=subprocess.DEVNULL)
    return wrap


def make_online_run(stage: Path, out: Path, cache: Path, smoke: bool) -> Path:
    vid = json.loads((stage / "install-manifest.json").read_text())["version_id"]
    wrap = make_wrapper(stage, out, cache, smoke)
    print("compressing online payload (bootstrap + online segments)", flush=True)
    segs = []
    for name, add in (("boot", lambda t: add_bootstrap(t, stage)), ("online", lambda t: t.add(wrap, arcname="keyloom-online"))):
        tgz = out / f"{name}.tar.gz"
        with tarfile.open(tgz, "w:gz", compresslevel=3) as t:
            add(t)
        segs.append((tgz, tgz.stat().st_size, sha256_file(tgz)))
    shutil.rmtree(wrap.parent)
    (_, bl, bs), (_, ol, os_) = segs
    stub = (REPO / "packaging/linux/online-stub.sh").read_text()
    for k, v in {"@VERSION_ID@": vid, "@BOOT_OFFSET@": 0, "@BOOT_LEN@": bl, "@BOOT_SHA256@": bs,
                 "@ONLINE_OFFSET@": bl, "@ONLINE_LEN@": ol, "@ONLINE_SHA256@": os_}.items():
        stub = stub.replace(k, str(v))
    run = out / f"Keyloom-online-{vid}-linux-x86_64.run"
    _assemble_run(stub, segs, run)
    return run


def make_online_exe(stage: Path, out: Path, cache: Path, smoke: bool, iscc: str | None) -> Path:
    if not IS_WIN:
        raise SystemExit("The Windows online installer must be built on Windows (it carries the stage's Windows "
                         "interpreter and needs Inno Setup). Nothing was built.")
    exe = find_iscc(iscc)
    m = json.loads((stage / "install-manifest.json").read_text())
    wrap = make_wrapper(stage, out, cache, smoke)
    text = (REPO / "packaging/windows/keyloom-online.iss.in").read_text()
    boot = make_boot_dir(stage, out / "bw")
    for k, v in {"@VERSION_ID@": m["version_id"], "@APP_VERSION@": m["app_version"], "@WRAP@": str(wrap), "@OUT@": str(out),
                 "@BOOT@": str(boot)}.items():
        text = text.replace(k, v)
    iss = out / "keyloom-online.iss"
    iss.write_text(text, encoding="utf-8-sig")
    subprocess.run([exe, "/Q", str(iss)], check=True)
    shutil.rmtree(boot, ignore_errors=True)
    stem = f"Keyloom-online-{m['version_id']}-windows-x86_64"  # no "-setup": make_windows_exe globs Keyloom-*-setup.exe
    built = out / (stem + ".exe")
    if not built.is_file():
        raise SystemExit("ISCC finished but produced no online setup exe")
    shutil.rmtree(wrap.parent)
    files = [built, *sorted(out.glob(stem + "-*.bin"))]
    (out / (built.name + ".sha256")).write_text("".join(f"{sha256_file(f)}  {f.name}\n" for f in files))
    return built


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("target", choices=["bundle", "linux", "windows", "online-linux", "online-windows"])
    ap.add_argument("--out", required=True, type=Path)
    ap.add_argument("--ffmpeg-dir", type=Path, help="dir with static ffmpeg + ffprobe (needed whenever a stage is built)")
    ap.add_argument("--stage", type=Path, help="online-*: use this finished stage instead of building one")
    ap.add_argument("--wheel-cache", type=Path, help="online-*: where the heavy public wheels are downloaded/verified (default OUT/wheel-cache)")
    ap.add_argument("--ui-binary", type=Path)
    ap.add_argument("--build-ui", action="store_true", help="cargo build --release first")
    ap.add_argument("--python-version", default="3.11.16", help="exact version, e.g. 3.11.16")
    ap.add_argument("--max-glibc", default="2.31", help="fail if the bundle's glibc floor is higher (Linux)")
    ap.add_argument("--iscc")
    ap.add_argument("--split", action="store_true", help="Linux: also write the .run as <=1900 MiB parts for GitHub release upload")
    ap.add_argument("--no-smoke", action="store_true")
    ap.add_argument("--reuse-runtime", type=Path, help="existing <stage>/runtime/python built by this tool (hardlinked, lock-checked)")
    a = ap.parse_args(argv)
    if a.target in ("linux", "online-linux") and IS_WIN or a.target in ("windows", "online-windows") and not IS_WIN:
        raise SystemExit(f"build the {a.target} installer on a {a.target.split('-')[-1]} host")
    a.out.mkdir(parents=True, exist_ok=True)
    out = a.out.resolve()
    cache = (a.wheel_cache or out / "wheel-cache").resolve()
    if a.stage and a.target.startswith("online-"):
        stage = a.stage.resolve()
    else:
        if not a.ffmpeg_dir:
            raise SystemExit("--ffmpeg-dir is required to build a stage (or pass a finished --stage to online-*)")
        stage = build_stage(out, ui_binary=a.ui_binary, build_ui=a.build_ui,
                            ffmpeg_dir=a.ffmpeg_dir, py_version=a.python_version, smoke=not a.no_smoke, max_glibc=a.max_glibc,
                            runtime_dir=a.reuse_runtime, wheel_cache=cache)
    if a.target == "online-linux":
        print(make_online_run(stage, out, cache, not a.no_smoke))
    elif a.target == "online-windows":
        print(make_online_exe(stage, out, cache, not a.no_smoke, a.iscc))
    elif a.target == "linux":
        run = make_linux_run(stage, out)
        print(run)
        if a.split:
            print(*split_linux_run(run), sep="\n")
    elif a.target == "windows":
        print(make_windows_exe(stage, out, a.iscc))
    else:
        print(stage)
    return 0


if __name__ == "__main__":
    sys.exit(main())
