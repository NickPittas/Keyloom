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
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
IS_WIN = os.name == "nt"
PLATFORM = "windows-x86_64" if IS_WIN else "linux-x86_64"
APP_DIRS = ["backend", "CorridorKeyModule", "CorridorKeyV2", "gvm_core", "sam2_tracker",
            "VideoMaMaInferenceModule", "modules", "ui", "scripts", "packaging"]
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
    shutil.copy2(REPO / "ui/theme/corridorkey.png", stage / "share/corridorkey.png")
    shutil.copy2(REPO / "ui/theme/corridorkey.ico", stage / "share/corridorkey.ico")


def stage_ui(stage: Path, binary: Path | None, build: bool) -> None:
    exe = "corridorkey-ui.exe" if IS_WIN else "corridorkey-ui"
    binary = binary or REPO / "rust-ui/target/release" / exe
    if build:
        subprocess.run(["cargo", "build", "--release", "--locked", "--bin", "corridorkey-ui"],
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


def stage_runtime(stage: Path, py_version: str, uv: str = "uv") -> str:
    """Managed standalone CPython (relocatable) + hashed lock install. Returns lock sha256.

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
    text = lock.read_text()
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
    (stage / "corridorkey-install.json").write_text(json.dumps({
        "schema": 1, "app_name": "Keyloom", "app_version": version,
        # models/cache/scratch/user_data null => platform defaults, shared across versions and
        # untouched by uninstall; the app root itself may be read-only.
        "roots": {"python": py, "helper": "app", "models": None, "cache": None,
                  "scratch": None, "user_data": None}}, indent=2) + "\n")


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
        "files": files}, indent=1, sort_keys=True))
    return vid


def validate_reused_runtime(runtime_dir: Path) -> str:
    """Reject a reused runtime BEFORE anything is created or copied; returns its lock hash."""
    mark = runtime_dir / PROVENANCE
    if not mark.is_file():
        raise SystemExit(f"{runtime_dir} has no {PROVENANCE}: only runtimes built by this tool's current recipe can be reused")
    prov = json.loads(mark.read_text())
    lock_sha = prov.get("lock_sha256")
    if prov.get("recipe") != RUNTIME_RECIPE or lock_sha != sha256_file(REPO / "packaging/locks" / f"{PLATFORM}.txt"):
        raise SystemExit("reused runtime was built with a different recipe/lock; rebuild it (stage_runtime)")
    if prov.get("sam2_c_sha256"):
        c = sorted(runtime_dir.glob("lib/python*/site-packages/sam2/_C*.so"))
        if len(c) != 1 or sha256_file(c[0]) != prov["sam2_c_sha256"]:
            raise SystemExit("reused runtime's sam2 extension differs from its provenance record")
    return lock_sha


def build_stage(out: Path, *, ui_binary=None, build_ui=False, ffmpeg_dir: Path, py_version="3.11.16",
                runtime_dir: Path | None = None, uv="uv", smoke=True, max_glibc: str | None = "2.31") -> Path:
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
        lock_sha = stage_runtime(stage, py_version, uv)
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
        lifecycle.run_selfcheck(stage)
    (out / "VERSION_ID").write_text(vid)
    return stage


def make_linux_run(stage: Path, out: Path) -> Path:
    print("compressing payload (single-threaded gzip; several minutes for ~8 GB)", flush=True)
    vid = json.loads((stage / "install-manifest.json").read_text())["version_id"]
    tgz = out / "payload.tar.gz"
    with tarfile.open(tgz, "w:gz", compresslevel=3) as t:
        t.add(stage, arcname="payload")
    sha = sha256_file(tgz)
    stub = (REPO / "packaging/linux/installer-stub.sh").read_text().replace("@PAYLOAD_SHA256@", sha) \
        .replace("@VERSION_ID@", vid)
    run = out / f"Keyloom-{vid}-linux-x86_64.run"
    with open(run, "wb") as f:
        f.write(stub.encode())
        with open(tgz, "rb") as p:
            shutil.copyfileobj(p, f, 1 << 20)
    run.chmod(0o755)
    tgz.unlink()
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


def render_iss(stage: Path, out: Path) -> Path:
    m = json.loads((stage / "install-manifest.json").read_text())
    text = (REPO / "packaging/windows/corridorkey.iss.in").read_text()
    for k, v in {"@VERSION_ID@": m["version_id"], "@APP_VERSION@": m["app_version"],
                 "@STAGE@": str(stage), "@OUT@": str(out)}.items():
        text = text.replace(k, v)
    iss = out / "corridorkey.iss"
    iss.write_text(text, encoding="utf-8-sig")
    return iss


def make_windows_exe(stage: Path, out: Path, iscc: str | None) -> Path:
    if not IS_WIN:
        raise SystemExit("The Windows runtime/installer must be built on Windows (the bundle contains "
                         "Windows wheels and an Inno Setup compile). Nothing was built.")
    exe = find_iscc(iscc)
    iss = render_iss(stage, out)
    subprocess.run([exe, str(iss)], check=True)
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


def find_wheel(name: str, version: str, tags: set, hashes: set) -> tuple[str, str]:
    """Exact public wheel URL + sha256: name, version and every platform tag must match what is installed, and the
    index-advertised hash must be one of the lock's pinned hashes (a hash only the index states is never trusted)."""
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
            sha = dict(urllib.parse.parse_qsl(frag)).get("sha256")
            if n == name and v.lower() == version.lower() and t == tags and sha in hashes and url.startswith("https://"):
                return url, sha
    raise SystemExit(f"no public wheel for {name}=={version} {sorted(tags)} matches a hash in {PLATFORM}.txt on {INDEXES}")


def fetch_wheel(url: str, sha: str, cache: Path) -> Path:
    cache.mkdir(parents=True, exist_ok=True)
    dest = cache / f"{sha}.whl"
    if dest.is_file() and sha256_file(dest) == sha:
        return dest
    part = dest.with_suffix(".part")
    for i in range(3):
        h = hashlib.sha256()
        try:
            with urllib.request.urlopen(urllib.request.Request(url, headers={"User-Agent": "keyloom-build"}), timeout=60) as r, \
                    open(part, "wb") as f:
                for c in iter(lambda: r.read(1 << 20), b""):
                    h.update(c)
                    f.write(c)
            break
        except OSError as e:
            if i == 2:
                raise SystemExit(f"cannot download {url}: {e}")
            time.sleep(2 ** i)
    if h.hexdigest() != sha:
        part.unlink()
        raise SystemExit(f"{url}: sha256 differs from the lock's pinned hash")
    part.replace(dest)
    return dest


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
    wheels, mapped, problems = [], set(), []
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
        url, sha = find_wheel(name, version, tags, pin[1])
        whl = fetch_wheel(url, sha, cache)
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
            entries = {}
            for rel in big:
                info, want = zmap.get(rel), files.get(rel)
                if rel in mapped:
                    problems.append(f"{rel}: claimed by two wheels")
                elif info is None:
                    problems.append(f"{rel}: not in {whl.name}")
                elif not want or "sha256" not in want:
                    problems.append(f"{rel}: not a regular file in the install manifest")
                else:
                    h, size = hashlib.sha256(), 0
                    with z.open(info) as src:
                        for c in iter(lambda: src.read(1 << 20), b""):
                            h.update(c)
                            size += len(c)
                    if (h.hexdigest(), size) != (want["sha256"], want["size"]):
                        problems.append(f"{rel}: bytes differ from {whl.name}:{info.filename}")
                    else:
                        entries[rel] = info.filename
        mapped.update(entries)
        wheels.append({"url": url, "sha256": sha, "size": whl.stat().st_size, "files": entries})
    if problems:
        raise SystemExit("heavy files not reproducible from public wheels:\n  " + "\n  ".join(problems))
    return {"schema": 1, "version_id": manifest["version_id"], "wheels": wheels}, mapped


def make_slim(stage: Path, dest: Path, mapped: set) -> Path:
    """Copy (hardlink where possible) the stage keeping symlinks, omitting ONLY the mapped regular files."""
    def ignore(d, names):
        return {n for n in names if (Path(d) / n).relative_to(stage).as_posix() in mapped}
    shutil.copytree(stage, dest, symlinks=True, ignore=ignore, copy_function=_link_or_copy)
    files = json.loads((stage / "install-manifest.json").read_text())["files"]
    gone = [r for r in files if r not in mapped and not os.path.lexists(dest / r)]
    if gone or any(os.path.lexists(dest / r) for r in mapped):
        raise SystemExit(f"slim payload differs from the plan: missing {gone[:5]}")
    return dest


def make_wrapper(stage: Path, out: Path, cache: Path, smoke: bool) -> Path:
    """<out>/online-boot/keyloom-online/{payload/, wheel-sources.json}; the sources are never inside the payload."""
    wrap = out / "online-boot" / "keyloom-online"
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
    print("compressing online payload", flush=True)
    tgz = out / "online-payload.tar.gz"
    with tarfile.open(tgz, "w:gz", compresslevel=3) as t:
        t.add(wrap, arcname="keyloom-online")
    shutil.rmtree(wrap.parent)
    sha = sha256_file(tgz)
    stub = (REPO / "packaging/linux/online-stub.sh").read_text().replace("@PAYLOAD_SHA256@", sha).replace("@VERSION_ID@", vid)
    run = out / f"Keyloom-online-{vid}-linux-x86_64.run"
    with open(run, "wb") as f:
        f.write(stub.encode())
        with open(tgz, "rb") as p:
            shutil.copyfileobj(p, f, 1 << 20)
    run.chmod(0o755)
    tgz.unlink()
    (out / (run.name + ".sha256")).write_text(f"{sha256_file(run)}  {run.name}\n")
    return run


def make_online_exe(stage: Path, out: Path, cache: Path, smoke: bool, iscc: str | None) -> Path:
    if not IS_WIN:
        raise SystemExit("The Windows online installer must be built on Windows (it carries the stage's Windows "
                         "interpreter and needs Inno Setup). Nothing was built.")
    exe = find_iscc(iscc)
    m = json.loads((stage / "install-manifest.json").read_text())
    wrap = make_wrapper(stage, out, cache, smoke)
    text = (REPO / "packaging/windows/keyloom-online.iss.in").read_text()
    for k, v in {"@VERSION_ID@": m["version_id"], "@APP_VERSION@": m["app_version"], "@WRAP@": str(wrap), "@OUT@": str(out)}.items():
        text = text.replace(k, v)
    iss = out / "keyloom-online.iss"
    iss.write_text(text, encoding="utf-8-sig")
    subprocess.run([exe, str(iss)], check=True)
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
    if a.stage and a.target.startswith("online-"):
        stage = a.stage.resolve()
    else:
        if not a.ffmpeg_dir:
            raise SystemExit("--ffmpeg-dir is required to build a stage (or pass a finished --stage to online-*)")
        stage = build_stage(out, ui_binary=a.ui_binary, build_ui=a.build_ui,
                            ffmpeg_dir=a.ffmpeg_dir, py_version=a.python_version, smoke=not a.no_smoke, max_glibc=a.max_glibc,
                            runtime_dir=a.reuse_runtime)
    cache = (a.wheel_cache or out / "wheel-cache").resolve()
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
