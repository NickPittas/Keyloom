"""Install lifecycle for a packaged Keyloom bundle (stdlib only, Linux + Windows).

Runs with the bundle's own interpreter (`python -I`), both from the freshly
extracted payload (install) and from the installed tree (repair/rollback/
uninstall/models/status). Layout under PREFIX:

    versions/<ver>/   one self-contained bundle (bin/, app/, runtime/, manifest)
    current -> versions/<ver>   (Linux symlink; Windows uses receipt + shortcuts)
    bin/keyloom[-setup|-coordinator]  stable shims
    receipt.json                owned files + state (the only thing uninstall trusts)

User data (projects, settings, models) lives OUTSIDE PREFIX and is never touched.
Activation configures the per-user network host unless it was explicitly removed.
On Linux the install/repair step uses sudo (unless --firewall skip) to let trusted networks reach an active UFW; see firewall.py.
Projects and network identities are preserved.
"""
from __future__ import annotations

import argparse
import contextlib
import copy
import hashlib
import json
import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import traceback
import uuid
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

IS_WIN = os.name == "nt"
PROBE_WORKERS = 8  # bound for independent read-only preflight probes (8..16 allowed); one short-lived pool per stage
MANIFEST = "install-manifest.json"
RECEIPT = "receipt.json"
APP_ID = "Keyloom"
DESCRIPTOR = "keyloom-install.json"
KEEP_VERSIONS = 2  # active + previous (rollback target)
STALL_SECONDS = 300  # a local step is stopped only after this long with no output at all: a hung child, never a slow disk or network
INNER_TIMEOUT = STALL_SECONDS - 60  # inner hang guards must fire (and report) before the supervisor's outer no-output stall guard


VERSION_RE = re.compile(r"^[0-9A-Za-z][0-9A-Za-z._+-]{0,79}$")
CLEAN_ENV_DROP = ("PYTHONPATH", "PYTHONHOME", "VIRTUAL_ENV", "PYTHONSTARTUP", "PYTHONUSERBASE")


class InstallError(RuntimeError):
    pass


def safe_version(v) -> str:
    """Version ids become directory names: one plain path component, nothing else."""
    if not isinstance(v, str) or not VERSION_RE.match(v) or ".." in v:
        raise InstallError(f"invalid version id {v!r}; the receipt or payload is damaged/tampered")
    return v


def vpath(prefix: Path, version: str) -> Path:
    p = prefix / "versions" / safe_version(version)
    root = (prefix / "versions").resolve()
    if p.is_symlink() or (p.exists() and p.resolve().parent != root):
        raise InstallError(f"{p} is not a plain version directory under {root}")
    return p


def clean_env(**extra) -> dict:
    env = {k: v for k, v in os.environ.items() if k not in CLEAN_ENV_DROP}
    env["PYTHONNOUSERSITE"] = "1"
    env["PYTHONUTF8"] = "1"
    env["PYTHONDONTWRITEBYTECODE"] = "1"  # keep verified trees byte-identical while validating
    env.update(extra)
    return env


def prefix_of(a) -> Path:
    """--prefix, absolute and normalised: the shims pass PREFIX/bin/.."""
    return Path(os.path.abspath(a.prefix))


def is_link(p: Path) -> bool:
    """Symlink, or a Windows junction (mount-point reparse point). Windows junction handling is UNPROVEN on real Windows."""
    if p.is_symlink():
        return True
    try:
        return IS_WIN and getattr(os.lstat(p), "st_reparse_tag", 0) == 0xA0000003
    except OSError:
        return False


def unlink_link(p: Path) -> None:
    """Remove a link/junction itself, never its target."""
    if p.is_symlink():
        os.unlink(p)
    elif is_link(p):
        os.rmdir(p)  # a junction is removed as an (empty-looking) directory entry, not recursively


def try_rmtree(p: Path) -> bool:
    """Remove a tree without following links; False if something (e.g. a running exe) remains."""
    if is_link(p):
        unlink_link(p)
        return True
    if is_link(p / "runtime"):
        unlink_link(p / "runtime")  # a shared runtime is never deleted through a version that merely references it
    shutil.rmtree(p, ignore_errors=True)
    return not p.exists()


def say(msg: str) -> None:
    print(msg, flush=True)


def log_exc(context: str, e: BaseException) -> None:
    """`ERROR: <context>: <Type>: <message>` and the full traceback. No debug gate: install.log always has it."""
    say(f"ERROR: {context}: {type(e).__name__}: {e}")
    say("".join(traceback.format_exception(e)).rstrip())


def redact(argv) -> list[str]:
    """Hide the value of a --token/--password/--secret option and of any K=V argument whose key looks secret."""
    out, hide = [], False
    for a in map(str, argv):
        if hide:
            out.append("***")
            hide = False
        elif a in ("--token", "--password", "--secret"):
            out.append(a)
            hide = True
        else:
            out.append(re.sub(r"^([^=]*(?:TOKEN|SECRET|PASSWORD|KEY)[^=]*)=.*", r"\1=***", a, flags=re.I))
    return out


def run_logged(argv, label: str, **kw) -> subprocess.CompletedProcess:
    """subprocess.run with install.log lines: `[label] run:`, `[label] exit`, then every stdout and stderr line untruncated."""
    say(f"[{label}] run: {' '.join(redact(argv))}")
    t0 = time.monotonic()
    try:
        r = subprocess.run(argv, capture_output=True, text=True, errors="replace", **kw)
    except subprocess.TimeoutExpired as e:
        for name, text in (("stdout", e.stdout), ("stderr", e.stderr)):
            text = text.decode(errors="replace") if isinstance(text, bytes) else text
            for line in (text or "").splitlines():
                say(f"[{label}] {name}| {line}")
        raise
    except (OSError, subprocess.SubprocessError) as e:
        log_exc(f"[{label}] run", e)
        raise
    say(f"[{label}] exit {r.returncode} after {time.monotonic() - t0:.1f}s")
    for name, text in (("stdout", r.stdout), ("stderr", r.stderr)):
        for line in (text or "").splitlines():
            say(f"[{label}] {name}| {line}")
    return r


@contextlib.contextmanager
def mutation_lock(prefix: Path, wait: float = 10):
    """One lifecycle mutation per prefix at a time (install/update/repair/rollback/uninstall/recover/models/attach): an OS file lock
    (flock / msvcrt) in the user's temp dir keyed by the prefix, so it vanishes with a crashed process and never lives inside
    (or blocks removal of) the prefix. NOT re-entrant and no bypass: it is distinct from any caller's own instance lock.
    Lock order for callers is always instance lock first, then this one; a caller that wants the lock for its own step (e.g.
    recovery before handing off) holds it only for that step and releases it BEFORE launching a lifecycle child, which takes
    it normally. Waits at most `wait` seconds, then fails with a clear message (never proceeds unlocked)."""
    key = owner_id(prefix)
    uid = os.getuid() if hasattr(os, "getuid") else 0
    path = Path(tempfile.gettempdir()) / f"keyloom-lifecycle-{uid}-{key}.lock"
    try:
        fh = open(path, "a+b")
    except OSError as e:
        raise InstallError(f"cannot open the install lock {path}: {e}") from e
    deadline = time.monotonic() + wait
    try:
        while True:
            try:
                if IS_WIN:
                    import msvcrt
                    fh.seek(0)
                    msvcrt.locking(fh.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(fh.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() > deadline:
                    raise InstallError(f"another Keyloom install/update/repair for {prefix} is running; wait for it to finish") from None
                time.sleep(0.2)
        try:
            yield
        finally:
            try:
                if IS_WIN:
                    fh.seek(0)
                    msvcrt.locking(fh.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    fcntl.flock(fh.fileno(), fcntl.LOCK_UN)
            except OSError:
                pass
    finally:
        fh.close()


@contextlib.contextmanager
def user_wait(reason: str):
    """Declare a wait (counts as progress for the supervisor's stall guard) ONLY while the user must act (consent/UAC/sudo) or an explicit model/network
    transfer runs: KEYLOOM_WAIT_FILE exists exactly as long as the block does, and is removed on success, failure and Ctrl-C.
    Never wrap service setup, selfchecks, hashing or extraction. No password is read or stored here."""
    f = os.environ.get("KEYLOOM_WAIT_FILE")
    made = False
    try:
        if f:
            try:
                fd = os.open(f, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
                with os.fdopen(fd, "w", encoding="utf-8") as h:
                    h.write(reason)
                made = True
            except OSError:
                pass  # no pause rather than a failed install
        yield
    finally:
        if made:
            try:
                os.unlink(f)
            except OSError:
                pass


def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def atomic_write_json(path: Path, data: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(f".{path.name}.{uuid.uuid4().hex[:8]}.tmp")
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2, sort_keys=True)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, path)


def load_json(path: Path) -> dict | None:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as e:
        raise InstallError(f"{path} is unreadable ({e}); it is kept untouched, repair or reinstall") from e


# ---- bundle verification ---------------------------------------------------

def read_manifest(version_dir: Path) -> dict:
    m = load_json(version_dir / MANIFEST)
    if not m or m.get("schema") != 1 or "files" not in m:
        raise InstallError(f"{version_dir}: missing or unsupported {MANIFEST}")
    return m


def _rel_ok(rel: str) -> bool:
    parts = Path(rel).parts
    return bool(parts) and not Path(rel).is_absolute() and ".." not in parts and "\\" not in rel and ":" not in parts[0]


RUNTIME_PY = "python/python.exe" if IS_WIN else "python/bin/python3"  # interpreter, relative to runtime/
STORE = "runtimes"  # PREFIX/runtimes/<rid> (+ <rid>.seal.json): shared runtime kept by identity


def runtime_id(files: dict) -> str:
    """Identity of the runtime/ subset of a manifest. MUST stay byte-identical to bundle.runtime_id (full 64 hex)."""
    sub = {k: v for k, v in files.items() if k.startswith("runtime/")}
    return hashlib.sha256(json.dumps(sub, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def runtime_available(prefix: Path, manifest: dict) -> dict:
    """Can `manifest`'s runtime be attached without extracting it? Read-only: JSON + one interpreter stat, never a runtime file
    hash/walk/copy. Trust model: a sealed store entry, or the verified ACTIVE version of an owned receipt whose manifest has the
    same runtime identity (its activation already verified the files; explicit repair re-hashes, see cmd_repair).
    -> {ok, rid, source: "store"|"legacy"|None, path, reason, invalid}
    (invalid: the manifest itself is bad. A runtime that exists but cannot be used is never reported as reusable: ok=False, exit 1)"""
    out = {"ok": False, "rid": "", "source": None, "path": None, "reason": "", "invalid": False}
    try:
        files = manifest["files"]
        rid = runtime_id(files)
        if manifest.get("runtime_id") not in (None, rid):
            out.update(reason="manifest runtime_id does not match its runtime files", invalid=True)
            return out
        out["rid"] = rid
        if f"runtime/{RUNTIME_PY}" not in files:
            out.update(reason="manifest has no runtime interpreter", invalid=True)
            return out
        d, seal = prefix / STORE / rid, prefix / STORE / f"{rid}.seal.json"  # sealed store first
        s = load_json(seal) if seal.is_file() and not seal.is_symlink() else None
        if (s and s.get("runtime_id") == rid and s.get("prefix") == str(prefix) and d.is_dir() and not d.is_symlink()
                and (d / RUNTIME_PY).exists()):
            out.update(ok=True, source="store", path=str(d))
            return out
        receipt = load_json(prefix / RECEIPT)
        if not receipt or receipt.get("app") != APP_ID or receipt.get("prefix") not in (None, str(prefix)):
            out["reason"] = "no owned install receipt to reuse a runtime from"
            return out
        cur = receipt.get("current")
        if not cur or not receipt.get("activated_at"):
            out["reason"] = "no previously activated version"
            return out
        d = vpath(prefix, cur)
        if not d.is_dir() or runtime_id(read_manifest(d)["files"]) != rid:
            out["reason"] = "active version has a different runtime"
            return out
        rt = (d / "runtime").resolve()  # a plain dir, or a link into this prefix
        if not rt.is_dir() or not rt.is_relative_to(prefix.resolve()):
            out["reason"] = "active runtime is missing or outside this prefix; a fresh runtime is needed"
            return out
        if not (rt / RUNTIME_PY).exists():
            out["reason"] = "active runtime interpreter is missing; a fresh runtime is needed"
            return out
        out.update(ok=True, source="legacy", path=str(rt))
    except (InstallError, OSError, KeyError, TypeError, AttributeError) as e:
        out.update(ok=False, source=None, path=None, reason=f"{type(e).__name__}: {e}")
    return out


def attach_runtime(prefix: Path, version: str) -> dict:
    """Make versions/<version>/runtime ONE link (symlink; Windows junction, unproven) to an available runtime. No file is
    iterated, hashed, copied or moved. Never replaces a real runtime/ and never touches the running interpreter's tree.
    -> {ok, rid, source, reason}; ok=False leaves the version unchanged (caller extracts or asks for repair)."""
    try:
        return _attach(prefix, vpath(prefix, version))
    except InstallError as e:
        return {"ok": False, "rid": "", "source": None, "reason": f"{type(e).__name__}: {e}"}


def _attach(prefix: Path, vdir: Path, final: Path | None = None) -> dict:
    """attach_runtime on any directory holding a manifest (the staged payload before its rename); `final` is where it will live."""
    out = {"ok": False, "rid": "", "source": None, "reason": ""}
    final = final or vdir
    try:
        av = runtime_available(prefix, read_manifest(vdir))
        out["rid"] = av["rid"]
        if not av["ok"]:
            out["reason"] = av["reason"]
            return out
        target, link = Path(av["path"]).resolve(), vdir / "runtime"
        if target.is_relative_to(vdir.resolve()) or target.is_relative_to(final.resolve()):
            out["reason"] = "the runtime lives inside this version"
        elif is_link(link):
            if link.resolve() == target:
                out.update(ok=True, source=av["source"])
            else:
                out["reason"] = "version already references a different runtime"
        elif link.exists():
            out["reason"] = "version already has its own runtime"
        else:
            if IS_WIN:
                import _winapi
                _winapi.CreateJunction(str(target), str(link))
            else:
                os.symlink(os.path.relpath(target, final.resolve()), link, target_is_directory=True)
            out.update(ok=True, source=av["source"])
    except (InstallError, OSError, ImportError, AttributeError) as e:
        out["reason"] = f"{type(e).__name__}: {e}"
    return out


def runtime_refs(prefix: Path, only: set | None = None) -> set[Path]:
    """Resolved runtime dirs that this prefix's version dirs link to (all, or just the names in `only`). Reads one link per
    version dir; no runtime file is touched."""
    out = set()
    vr = prefix / "versions"
    for e in (vr.iterdir() if vr.is_dir() else ()):
        if (only is None or e.name in only) and not e.is_symlink() and is_link(e / "runtime"):
            out.add((e / "runtime").resolve())
    return out


def runtime_in_use(prefix: Path, d: Path, refs: set | None = None) -> bool:
    """True when `d` owns a real runtime/ that another version of this prefix links to."""
    rt = d / "runtime"
    if is_link(rt) or not rt.is_dir():
        return False
    me = rt.resolve()
    if refs is not None:
        return me in refs
    vr = prefix / "versions"
    for e in (vr.iterdir() if vr.is_dir() else ()):
        if e.resolve() != d.resolve() and not e.is_symlink() and is_link(e / "runtime") and (e / "runtime").resolve() == me:
            return True
    return False


def runtime_trusted(prefix: Path, vdir: Path) -> bool:
    """Is vdir/runtime a runtime this install may skip re-hashing? Reads a handful of JSON/links, never a runtime file.
    A real runtime/ counts when its interpreter exists (its activation verified it; explicit repair re-hashes). An attached link
    counts only when its target is inside this prefix and its owner's manifest carries the same runtime identity."""
    rt = vdir / "runtime"
    try:
        if not is_link(rt):
            return rt.is_dir() and (rt / RUNTIME_PY).exists()
        t = rt.resolve()
        if not t.is_dir() or not (t / RUNTIME_PY).exists() or not t.is_relative_to(prefix.resolve()):
            return False
        want = runtime_id(read_manifest(vdir)["files"])
        return t.name == want if t.parent.name == STORE else runtime_id(read_manifest(t.parent)["files"]) == want
    except (InstallError, OSError, KeyError, TypeError):
        return False


def verify_bundle(version_dir: Path, skip_runtime: bool = False, only: set | None = None) -> list[tuple[str, str]]:
    """Structured problems [(kind, relpath)] for every manifest file; kinds: missing/changed/link/unsafe.
    skip_runtime leaves out runtime/ entries; ONLY for callers that established runtime_trusted() first (repair/status never do).
    only limits the check to those relpaths (re-checking what a repair just restored, without re-hashing the rest)."""
    problems = []
    for rel, meta in read_manifest(version_dir).get("files", {}).items():
        if (skip_runtime and rel.startswith("runtime/")) or (only is not None and rel not in only):
            continue
        if not _rel_ok(rel):
            problems.append(("unsafe", rel))
            continue
        p = version_dir / rel
        try:
            if meta.get("symlink") is not None:
                if not p.is_symlink() or os.readlink(p) != meta["symlink"]:
                    problems.append(("link", rel))
            elif not p.is_file() or p.is_symlink():
                problems.append(("missing", rel))
            elif p.stat().st_size != meta["size"] or sha256_file(p) != meta["sha256"]:
                problems.append(("changed", rel))
        except OSError:
            problems.append(("missing", rel))
    return problems


def fmt_problems(problems) -> str:
    return "; ".join(f"{k}: {r}" for k, r in problems[:5]) + (f" (+{len(problems) - 5} more)" if len(problems) > 5 else "")


def descriptor_file(version_dir: Path) -> Path:
    return version_dir / DESCRIPTOR


def ui_exe(version_dir: Path) -> Path:
    return version_dir / "bin" / f"keyloom-ui{'.exe' if IS_WIN else ''}"


def icon_file(version_dir: Path) -> Path | None:
    p = version_dir / "share" / f"keyloom.{'ico' if IS_WIN else 'png'}"
    return p if p.is_file() else None


def descriptor_python(version_dir: Path) -> Path:
    d = load_json(descriptor_file(version_dir))
    if not d:
        raise InstallError(f"{version_dir}: missing {DESCRIPTOR}")
    return version_dir / d["roots"]["python"]


def host_glibc() -> tuple[int, ...] | None:
    try:
        return tuple(int(x) for x in os.confstr("CS_GNU_LIBC_VERSION").split()[1].split("."))
    except (AttributeError, ValueError, OSError, IndexError):
        return None


def check_glibc(manifest: dict) -> None:
    need, have = manifest.get("min_glibc"), host_glibc()
    if need and have and tuple(int(x) for x in need.split(".")) > have:
        raise InstallError(f"this bundle needs glibc >= {need} but the system has {'.'.join(map(str, have))}; "
                           "use an installer built for an older glibc")


# OS-provided libraries the bundle deliberately does not ship (graphics/windowing stack + NVIDIA driver).
# Each tuple = alternatives; one must load. Checked before anything is extracted into place.
LINUX_SYSTEM_LIBS = (("libGL.so.1", "libEGL.so.1"), ("libxkbcommon.so.0",), ("libX11.so.6", "libwayland-client.so.0"),
                     ("libfontconfig.so.1",), ("libfreetype.so.6",), ("libglib-2.0.so.0",), ("libdbus-1.so.3",),
                     ("libstdc++.so.6",))
APT_HINT = ("sudo apt install libgl1 libegl1 libxkbcommon0 libxkbcommon-x11-0 libx11-6 libxcursor1 libxi6 "
            "libfontconfig1 libfreetype6 libglib2.0-0 libdbus-1-3 libstdc++6")


def check_system_libs() -> None:
    import ctypes

    def loads(n):
        try:
            (ctypes.WinDLL if IS_WIN else ctypes.CDLL)(n)
            return True
        except OSError:
            return False
    with ThreadPoolExecutor(max_workers=PROBE_WORKERS) as ex:  # independent loader probes; results keep LINUX_SYSTEM_LIBS order
        cuda = ex.submit(loads, "nvcuda.dll" if IS_WIN else "libcuda.so.1")
        found = [] if IS_WIN else list(ex.map(lambda alts: any(loads(n) for n in alts), LINUX_SYSTEM_LIBS))
    missing = [" or ".join(alts) for alts, ok in zip(LINUX_SYSTEM_LIBS, found) if not ok]
    if missing:
        raise InstallError("missing OS libraries: " + ", ".join(missing) + f"\n  Debian/Ubuntu/Mint: {APT_HINT}")
    if not cuda.result():  # advisory: app and models install; GPU workflows need the driver
        say("WARNING: NVIDIA driver (CUDA) not found. Keyloom needs an NVIDIA GPU + driver to run; install the "
            "current NVIDIA driver (Linux Mint: Driver Manager) before use.")


def run_selfcheck(version_dir: Path, timeout: int = INNER_TIMEOUT, shared: Path | None = None) -> dict:
    """Launch validation from an unrelated CWD with a polluted-env-free child:
    the bundled interpreter imports every engine, and the real UI binary resolves the descriptor."""
    py, app = descriptor_python(version_dir), version_dir / "app"
    with tempfile.TemporaryDirectory() as cwd:
        env = clean_env(CK_INSTALL=str(descriptor_file(version_dir)))
        try:
            r = run_logged([str(py), "-I", "-B", str(app / "packaging" / "selfcheck.py"), str(app)], "selfcheck",
                           cwd=cwd, env=env, timeout=timeout)
        except (OSError, subprocess.SubprocessError) as e:
            raise InstallError(f"launch validation could not run the bundled interpreter: {e}")
        if r.returncode != 0:
            raise InstallError(f"launch validation failed ({r.returncode}): {(r.stderr or r.stdout).strip()[-800:]}")
        try:
            info = json.loads(r.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            raise InstallError(f"launch validation gave no result: {r.stdout[-300:]}")
        info["ui"] = _ui_check(version_dir, env, cwd, shared, timeout)
    return info


def _ui_check(version_dir: Path, env: dict, cwd: str, shared: Path | None, timeout: int) -> dict:
    """The real UI binary's --self-check. Its repo must resolve inside the version; its python inside the version, or inside
    `shared` (a runtime the caller validated with runtime_trusted: the one outside-the-version location allowed)."""
    exe = ui_exe(version_dir)
    try:
        u = run_logged([str(exe), "--self-check"], "ui-selfcheck", cwd=cwd, env=env, timeout=timeout)
    except (OSError, subprocess.SubprocessError) as e:  # a deadline is a failure, never success
        raise InstallError(f"UI self-check could not run {exe}: {e}")
    try:
        ui = json.loads(u.stdout.strip().splitlines()[-1])
    except (ValueError, IndexError):
        ui = {}
    root = version_dir.resolve()
    py_roots = [root] + ([shared.resolve()] if shared else [])
    inside = (bool(ui.get("repo")) and Path(ui["repo"]).resolve().is_relative_to(root) and bool(ui.get("python"))
              and any(Path(ui["python"]).resolve().is_relative_to(r) for r in py_roots))
    if u.returncode != 0 or not ui.get("ok") or not inside:
        raise InstallError(f"UI self-check failed ({u.returncode}): stderr: {u.stderr.strip()[-2000:]} stdout: {u.stdout.strip()[-2000:]}")
    return {k: ui.get(k) for k in ("version", "installed")}


def run_light_check(version_dir: Path, shared: Path | None = None, timeout: int = INNER_TIMEOUT) -> dict:
    """Ordinary app update on a validated shared runtime: the descriptor resolves to an existing interpreter and the UI binary
    self-checks, each with a hang guard (default INNER_TIMEOUT). No engine import, no GPU probe, no torch."""
    py = descriptor_python(version_dir)
    if not py.exists():
        raise InstallError(f"launch validation: interpreter {py} is missing")
    with tempfile.TemporaryDirectory() as cwd:
        return {"light": True, "ui": _ui_check(version_dir, clean_env(CK_INSTALL=str(descriptor_file(version_dir))), cwd, shared, timeout)}


def ffmpeg_record(vdir: Path) -> dict:
    """The bundled FFmpeg of this version, in the shape selfcheck.py reports (receipt runtime.ffmpeg)."""
    b = vdir / "app/tools/ffmpeg/bin"
    e = ".exe" if IS_WIN else ""
    f, p = b / f"ffmpeg{e}", b / f"ffprobe{e}"
    ok = f.is_file() and p.is_file()
    return {"ok": ok, "message": "bundled FFmpeg of the active version" if ok else f"bundled FFmpeg missing in {b}",
            "ffmpeg": str(f) if f.is_file() else None, "ffprobe": str(p) if p.is_file() else None}


# ---- shortcuts and shims ---------------------------------------------------

def owner_id(prefix: Path) -> str:
    return hashlib.sha256(str(prefix).encode("utf-8")).hexdigest()[:16]


def owner_ids(prefix: Path) -> list[str]:
    return [owner_id(prefix)]


def desktop_entry(exec_path: Path, icon: Path | None, prefix: Path | None = None) -> str:
    """Exec quoting per the desktop-entry spec: reserved chars backslash-escaped inside double quotes,
    '%' doubled (field codes), then every backslash doubled again for the key-file string layer."""
    q = str(exec_path)
    for ch in ('\\', '"', '`', '$'):
        q = q.replace(ch, "\\" + ch)
    q = q.replace("%", "%%").replace("\\", "\\\\")
    lines = ["[Desktop Entry]", "Type=Application", "Name=Keyloom", "GenericName=Chroma keyer", "Comment=Keyloom — AI chroma keyer", "StartupWMClass=keyloom-ui",
             f'Exec=env -u PYTHONPATH -u PYTHONHOME -u VIRTUAL_ENV PYTHONNOUSERSITE=1 "{q}" %F',
             "Terminal=false", "Categories=AudioVideo;Video;Graphics;"]
    if icon:
        lines.append(f"Icon={icon}")
    if prefix:
        lines.append(f"X-Keyloom-Owner={owner_id(prefix)}")
    return "\n".join(lines) + "\n"


def powershell_str(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def windows_shortcut_script(lnk: Path, target: Path, workdir: Path, icon: Path | None) -> str:
    parts = ["$s=(New-Object -ComObject WScript.Shell).CreateShortcut(%s)" % powershell_str(str(lnk)),
             "$s.TargetPath=%s" % powershell_str(str(target)),
             "$s.WorkingDirectory=%s" % powershell_str(str(workdir))]
    if icon:
        parts.append("$s.IconLocation=%s" % powershell_str(str(icon)))
    parts.append("$s.Save()")
    return "; ".join(parts)


def shortcut_paths() -> dict:
    if IS_WIN:
        start = Path(os.environ["APPDATA"]) / "Microsoft/Windows/Start Menu/Programs/Keyloom"
        return {"start_menu_dir": start, "lnk": start / "Keyloom.lnk"}
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    return {"desktop": data / "applications/keyloom.desktop"}


def owns_desktop(path: Path, prefix: Path) -> bool:
    """True only for a regular desktop file this prefix wrote."""
    if not path.is_file() or path.is_symlink():
        return False
    text = path.read_text(encoding="utf-8", errors="replace")
    return any(f"X-Keyloom-Owner={o}" in text for o in owner_ids(prefix))


def write_shortcuts(version_dir: Path, prefix: Path) -> None:
    sp = shortcut_paths()
    exe = ui_exe(version_dir)
    icon = icon_file(version_dir)
    if IS_WIN:
        sp["start_menu_dir"].mkdir(parents=True, exist_ok=True)
        # By value through a temp .ps1 so quotes/Unicode in paths need no shell escaping.
        script = windows_shortcut_script(sp["lnk"], exe, Path.home(), icon)
        with tempfile.NamedTemporaryFile("w", suffix=".ps1", delete=False, encoding="utf-8-sig") as f:
            f.write(script)
        try:
            r = run_logged(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", f.name], "shortcut", timeout=60)
        finally:
            os.unlink(f.name)
        if r.returncode:
            raise InstallError(f"Start Menu shortcut could not be written (powershell exit {r.returncode}): {r.stderr.strip() or r.stdout.strip()}")
        update_autostart(version_dir, prefix)
        return
    sp["desktop"].parent.mkdir(parents=True, exist_ok=True)
    tmp = sp["desktop"].with_suffix(".tmp")
    tmp.write_text(desktop_entry(exe, icon, prefix), encoding="utf-8")
    os.replace(tmp, sp["desktop"])
    update_autostart(version_dir, prefix)


def remove_shortcuts(prefix: Path) -> None:
    sp = shortcut_paths()
    d = sp.get("desktop")
    if d and owns_desktop(d, prefix):
        d.unlink()  # only the entry this prefix wrote
    if IS_WIN:
        _unlink_owned(sp["lnk"])
        _unlink_owned(sp["start_menu_dir"])
    remove_autostart(prefix)


# Per-user background autostart is written by the app, never by the installer, except that activation UPDATES an
# entry this prefix already owns. Contract: it launches the stable Linux shim PREFIX/bin/keyloom; Windows GUI exe.
# Linux: $XDG_CONFIG_HOME/autostart/keyloom.desktop containing X-Keyloom-Owner=<owner_id(prefix)>. Windows: HKCU\Software\
# Microsoft\Windows\CurrentVersion\Run value "Keyloom" under this prefix. Anything not matching this prefix is left alone.
AUTOSTART_RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
AUTOSTART_VALUES = ("Keyloom",)


def owns_windows_autostart(value: str, prefix: Path) -> bool:
    return any(value.lower().startswith('"' + str(prefix / d).lower() + "\\") for d in ("bin", "versions"))


def autostart_files() -> Path:
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "autostart" / "keyloom.desktop"


def update_autostart(version_dir: Path, prefix: Path) -> None:
    """Point an autostart entry this prefix owns at the active version; no entry is ever created."""
    if IS_WIN:
        import winreg
        try:
            k = winreg.OpenKey(winreg.HKEY_CURRENT_USER, AUTOSTART_RUN_KEY, 0, winreg.KEY_QUERY_VALUE | winreg.KEY_SET_VALUE)
        except FileNotFoundError:
            return
        with k:
            try:
                value = str(winreg.QueryValueEx(k, "Keyloom")[0])
            except FileNotFoundError:
                return
            if not owns_windows_autostart(value, prefix):
                return  # not ours: the name belongs to somebody else
            winreg.SetValueEx(k, "Keyloom", 0, winreg.REG_SZ, f'"{ui_exe(version_dir)}" --background')
        return
    new = autostart_files()
    if owns_desktop(new, prefix):
        text = new.read_text(encoding="utf-8", errors="replace")
        text = re.sub(r"^Comment=.*$", "Comment=Keeps the Keyloom network client available", text, flags=re.M)
        tmp = new.with_suffix(".tmp")
        tmp.write_text(text, encoding="utf-8")
        os.replace(tmp, new)


def remove_autostart(prefix: Path) -> None:
    if IS_WIN:
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, AUTOSTART_RUN_KEY, 0,
                                winreg.KEY_QUERY_VALUE | winreg.KEY_SET_VALUE) as k:
                for n in AUTOSTART_VALUES:
                    try:
                        if owns_windows_autostart(str(winreg.QueryValueEx(k, n)[0]), prefix):
                            winreg.DeleteValue(k, n)
                    except FileNotFoundError:
                        pass
        except OSError:
            pass
        return
    d = autostart_files()
    if owns_desktop(d, prefix):
        d.unlink()


def _network_service(version_dir: Path):
    """The version's own stdlib-only service manager, loaded by path (no sys.path or cwd involvement)."""
    import importlib.util
    f = version_dir / "app/backend/network_service.py"
    spec = importlib.util.spec_from_file_location("ck_network_service", f)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


TRANSIENT_NET = {"start_timeout", "busy", "port_in_use", "start_failed", "internal"}  # moment-dependent: one retry is safe (both steps are idempotent)
NET_BOOT = (
    "import sys,json,traceback;from pathlib import Path;"
    "sys.path.insert(0,sys.argv[1]);from backend import network_service as s;s.ECHO=True\n"
    "try:\n out=s.ensure_default(Path(sys.executable),Path(sys.argv[1]),Path(sys.argv[2]))\n"
    "except Exception as e:\n traceback.print_exc()\n"
    " out={'action':'failed','installed':False,'running':False,'error':{'type':type(e).__name__,'code':getattr(e,'code',None),'message':str(e)}}\n"
    "print('KEYLOOM-NET-RESULT '+json.dumps(out))"
)


def _net_attempt(version_dir: Path, prefix: Path) -> dict:
    """One network setup in the version's own interpreter. A child failure comes back as a failed result; only a spawn error raises."""
    failed = {"action": "failed", "installed": False, "running": False, "mode": None}
    try:
        r = run_logged([str(descriptor_python(version_dir)), "-I", "-B", "-c", NET_BOOT, str(version_dir / "app"), str(prefix)],
                       "network", env=clean_env(), timeout=INNER_TIMEOUT)  # hang guard; a timeout is reported, never success
    except subprocess.TimeoutExpired:
        return dict(failed, code="timeout", message=f"no result within {INNER_TIMEOUT} s")
    line = next((ln for ln in r.stdout.splitlines() if ln.startswith("KEYLOOM-NET-RESULT ")), None)
    if r.returncode or line is None:
        return dict(failed, code="crashed", message=f"network setup exited with code {r.returncode} without a result")
    out = json.loads(line.split(" ", 1)[1])
    if "error" in out:
        err = out["error"]
        return dict(failed, code=err.get("code") or "internal", message=f"{err['type']}: {err['message']}")
    return {"action": out.get("action"), "installed": bool(out.get("installed")), "running": bool(out.get("running")),
            "mode": out.get("mode"), "code": out.get("error_code"), "message": out.get("message", "")}


def refresh_network_service(version_dir: Path, prefix: Path, receipt: dict | None = None) -> dict:
    """Start automatic hosting, or refresh the owned service, with the version's own network_service. The outcome is saved as
    receipt network.setup (also on failure) and returned. Never fails activation: every error becomes a result, logged in full."""
    say("Configuring this PC's background network service ...")
    attempts = 1
    try:
        net = _net_attempt(version_dir, prefix)
        if net["action"] == "failed" and net["code"] in TRANSIENT_NET:
            say(f"network setup attempt 1 failed ({net['code']}: {net['message']}); retrying once in 5 s")
            time.sleep(5)
            attempts, net = 2, _net_attempt(version_dir, prefix)
    except Exception as e:
        log_exc("network setup", e)
        net = {"action": "failed", "installed": False, "running": False, "mode": None, "code": "internal",
               "message": f"{type(e).__name__}: {e}"}
    net = dict(net, ok=net["action"] != "failed", attempts=attempts, time=time.strftime("%Y-%m-%dT%H:%M:%S%z"))
    say(f"network: {net['action']}; installed={net['installed']} running={net['running']}; {net['message']}")
    try:
        saved = receipt if receipt is not None else read_receipt(prefix)
        saved["network"] = {"services": net["installed"], "listeners": net["running"],
                            "firewall_rules": (saved.get("network") or {}).get("firewall_rules", False), "setup": net}
        atomic_write_json(prefix / RECEIPT, saved)
    except Exception as e:
        log_exc("recording the network setup", e)
    return net


def remove_network_service(prefix: Path, version: str | None) -> None:
    """Uninstall: unregister only the service this prefix owns (state, certificate and database are left in place)."""
    if not (prefix / "network-service.json").is_file():
        return
    try:
        msg = _network_service(vpath(prefix, version)).remove_owned(prefix)
        if msg:
            say(msg)
    except Exception as e:
        log_exc("network service removal", e)
        raise InstallError(f"the network service could not be stopped and removed ({type(e).__name__}: {getattr(e, 'message', None) or e}); the app was kept installed so you can stop the service from Clients and retry") from e


def _firewall():
    """packaging/firewall.py next to this file, loaded in this interpreter (uninstall, and the build's load gate)."""
    import importlib.util
    f = Path(__file__).resolve().with_name("firewall.py")
    if not f.is_file():
        return None  # callers (and the build's helper-load gate) report a missing helper explicitly
    spec = importlib.util.spec_from_file_location("ck_firewall", f)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def _firewall_run(version_dir: Path, mode: str, prior: dict | None) -> dict | None:
    """Run firewall.run in the INSTALLED interpreter. cmd_install renames the staged payload (and its Python) before this
    step, so the starting interpreter can no longer import stdlib modules it had not loaded yet. stdio stays on the terminal
    for the prompts and sudo; the record comes back through a private file."""
    out = Path(tempfile.mkdtemp(prefix=".firewall.", dir=version_dir.parents[1])) / "record.json"
    try:
        cmd = [str(descriptor_python(version_dir)), "-I", "-B", str(version_dir / "app/packaging/firewall.py"),
               mode, json.dumps(prior or {}), str(out)]
        # The helper itself declares the wait (user_wait in firewall.py) around sudo/UAC; its 180 s sudo deadline bounds it.
        say(f"[firewall] run: {' '.join(redact(cmd))}")
        t0 = time.monotonic()
        rc = subprocess.run(cmd, env=clean_env(), timeout=1200).returncode
        say(f"[firewall] exit {rc} after {time.monotonic() - t0:.1f}s")
        if rc:
            raise InstallError(f"firewall helper exited with code {rc}")
        return json.loads(out.read_text(encoding="utf-8")) or None
    finally:
        shutil.rmtree(out.parent, ignore_errors=True)


def firewall_step(prefix: Path, mode: str) -> str | None:
    """Linux: allow trusted networks through an active UFW (runs unless the firewall choice is skip). Never fails the install; the outcome is
    recorded in the receipt as `firewall` and printed. Returns its status, None when not applicable."""
    try:
        receipt = read_receipt(prefix)
        rec = _firewall_run(vpath(prefix, receipt["current"]), mode, receipt.get("firewall"))
        if rec is None:
            return None
        receipt["firewall"] = rec
        if "network" in receipt:
            receipt["network"]["firewall_rules"] = bool(rec["owned"])
        atomic_write_json(prefix / RECEIPT, receipt)
    except Exception as e:
        log_exc("firewall setup", e)
        say(f"NETWORK FIREWALL SETUP PENDING: the helper could not run ({type(e).__name__}: {e}); "
            "other PCs may not reach this one. Run the installer again or see docs/install.md (Linux firewall).")
        return "pending"
    if rec["status"] in ("ok", "not_needed"):
        say(f"Network firewall: {rec['detail']}.")
    else:
        say(f"NETWORK FIREWALL SETUP PENDING: {rec['detail']}.")
        if rec["commands"]:
            say("  To allow it yourself (skip any rule your firewall already has):\n    " + "\n    ".join(rec["commands"]))
    return rec["status"]


def remove_firewall(prefix: Path, receipt: dict, mode: str) -> None:
    """Uninstall: remove only the UFW rules this installer added, if the user agrees; otherwise say how."""
    try:
        msg = _firewall().remove(receipt.get("firewall"), mode)  # None helper -> AttributeError -> the warning below
        if msg:
            say(msg)
    except Exception as e:
        log_exc("firewall removal", e)
        say(f"WARNING: Keyloom firewall rules were not checked ({type(e).__name__}: {e}); remove them with `sudo ufw status numbered` / `sudo ufw delete N`.")


# Order matters: SHIM_NAMES[1] is the setup shim (install's "Resume with" hint).
SHIM_NAMES = (("keyloom.cmd", "keyloom-setup.cmd", "keyloom-coordinator.cmd") if IS_WIN
              else ("keyloom", "keyloom-setup", "keyloom-coordinator"))


def write_shims(prefix: Path) -> None:
    """Stable launchers in PREFIX/bin that follow `current` and scrub Python environment variables."""
    bindir = prefix / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    if IS_WIN:
        cur = r'for /f "usebackq delims=" %%V in ("%~dp0..\current.txt") do set "CKV=%%V"'
        env = 'set "PYTHONPATH="\nset "PYTHONHOME="\nset "VIRTUAL_ENV="\nset "PYTHONNOUSERSITE=1"'
        files = {
            "keyloom.cmd": f'@echo off\n{cur}\n{env}\nset "U=%~dp0..\\versions\\%CKV%\\bin\\keyloom-ui.exe"\nstart "" "%U%" %*\n',
            "keyloom-setup.cmd": f'@echo off\n{cur}\n{env}\n"%~dp0..\\versions\\%CKV%\\runtime\\python\\python.exe" -I -B '
                                     '"%~dp0..\\versions\\%CKV%\\app\\packaging\\lifecycle.py" --prefix "%~dp0.." %*\n',
            # Server mode is opt-in: this shim only runs when invoked; it opens nothing by itself.
            # -P keeps the caller's working directory off sys.path; relative --state-dir stays relative to it.
            "keyloom-coordinator.cmd": f'@echo off\n{cur}\n{env}\nset "PYTHONPATH=%~dp0..\\versions\\%CKV%\\app"\n'
                                           '"%~dp0..\\versions\\%CKV%\\runtime\\python\\python.exe" -B -P -m backend.coordinator %*\n',
        }
    else:
        env = "unset PYTHONPATH PYTHONHOME VIRTUAL_ENV PYTHONSTARTUP PYTHONUSERBASE\nPYTHONNOUSERSITE=1; export PYTHONNOUSERSITE\n"
        files = {
            "keyloom": env + 'B="$(dirname "$0")/../current/bin"\nexec "$B/keyloom-ui" "$@"\n',
            "keyloom-setup": env + 'P="$(dirname "$0")/.."\nexec "$P/current/runtime/python/bin/python3" -I -B '
                                       '"$P/current/app/packaging/lifecycle.py" --prefix "$P" "$@"\n',
            # Server mode is opt-in: this shim only runs when invoked; it opens nothing by itself.
            # -P keeps the caller's working directory off sys.path; relative --state-dir stays relative to it.
            "keyloom-coordinator": env + 'P="$(cd "$(dirname "$0")/.." && pwd)"\n'
                                             'PYTHONPATH="$P/current/app"; export PYTHONPATH\n'
                                             'exec "$P/current/runtime/python/bin/python3" -B -P -m backend.coordinator "$@"\n',
        }
    for name, text in files.items():
        p = bindir / name
        tmp = p.with_name(p.name + ".tmp")
        tmp.write_text(text if IS_WIN else "#!/bin/sh\n" + text, encoding="ascii" if IS_WIN else "utf-8")
        if not IS_WIN:
            tmp.chmod(0o755)
        os.replace(tmp, p)


# ---- receipt, pointer, transaction ----------------------------------------

def read_receipt(prefix: Path) -> dict:
    r = load_json(prefix / RECEIPT) or {"schema": 1, "versions": []}
    for v in [r.get("current"), r.get("previous"), *r.get("versions", [])]:
        if v is not None:
            safe_version(v)  # a tampered receipt can never name a path outside versions/
    r.setdefault("versions", [])
    return r


def switch_current(prefix: Path, version: str | None) -> None:
    txt = prefix / "current.txt"
    if version is None:
        for p in (txt, prefix / "current"):
            if p.is_symlink() or p.exists():
                p.unlink()
        return
    safe_version(version)
    txt.write_text(version, encoding="utf-8")  # Windows shim + diagnostics
    if not IS_WIN:
        tmp = prefix / f".current.{uuid.uuid4().hex[:8]}"
        os.symlink(f"versions/{version}", tmp)
        os.replace(tmp, prefix / "current")  # atomic switch


def pointer_ok(prefix: Path, version: str) -> bool:
    try:
        if (prefix / "current.txt").read_text(encoding="utf-8") != version:
            return False
        return IS_WIN or os.readlink(prefix / "current") == f"versions/{version}"
    except OSError:
        return False


def gpu_info() -> dict:
    try:
        r = run_logged(["nvidia-smi", "--query-gpu=name,driver_version,memory.total", "--format=csv,noheader"],
                       "nvidia-smi", timeout=INNER_TIMEOUT)
        if r.returncode == 0 and r.stdout.strip():
            return {"nvidia": r.stdout.strip().splitlines()}
        return {"nvidia": None, "note": "nvidia-smi failed; Keyloom needs an NVIDIA GPU + driver"}
    except (OSError, subprocess.SubprocessError):
        return {"nvidia": None, "note": "nvidia-smi not found; Keyloom needs an NVIDIA GPU + driver"}


TXN = ".txn.json"


def recover(prefix: Path, resume: str | None = None) -> None:
    """Finish/undo an interrupted install: unreceipted version dirs, same-version backups, stale
    staging, and a pointer that disagrees with the receipt. Safe to run at the start of any command."""
    if not prefix.is_dir():
        return
    receipt = read_receipt(prefix)
    keep = {v for v in (receipt.get("current"), receipt.get("previous")) if v}
    txn = load_json(prefix / TXN)
    protect = None
    if txn and resume is not None and txn.get("prepared") and txn.get("version") == resume:
        # `prepare-target` readied this destination and the files were placed natively; the `install --version` that follows
        # consumes this journal. Undoing it here would delete those files (and restore the backup over them).
        protect, txn = txn.get("backup"), None
    if txn:
        v = safe_version(txn["version"])
        d = vpath(prefix, v)
        backup = txn.get("backup")
        bd = prefix / "versions" / backup if backup else None
        if bd is not None and (not re.fullmatch(re.escape(v) + r"\.old-[0-9a-f]{6}", backup)):
            raise InstallError(f"damaged transaction journal in {prefix}")
        # Installed metadata decides, not runtime content: app files are re-hashed (small), the runtime must be a trusted one.
        good = d.is_dir() and v in keep and runtime_trusted(prefix, d) and not verify_bundle(d, skip_runtime=True)
        if txn.get("inplace"):
            good = True  # an in-place activation moved nothing: the target is an installed version and is never removed here
        if not good:
            if d.exists() and v not in keep or d.exists() and bd is not None:
                if runtime_in_use(prefix, d):
                    pass  # a kept version links to this runtime: it is retained, never deleted
                elif not try_rmtree(d):
                    return  # still locked (Windows); journal stays for the next run
            # a referenced backup is not renamed back either: the links that name it must keep working
            if bd is not None and bd.is_dir() and not d.exists() and not runtime_in_use(prefix, bd):
                os.rename(bd, d)
        if bd is not None and bd.exists() and not runtime_in_use(prefix, bd):
            try_rmtree(bd)
        (prefix / TXN).unlink()
    vroot = prefix / "versions"
    if vroot.is_dir():
        for e in vroot.iterdir():
            if (re.search(r"\.old-[0-9a-f]{6}$", e.name) and e.is_dir() and not e.is_symlink() and e.name != protect
                    and not runtime_in_use(prefix, e)):  # a still-referenced runtime owner survives until the last link is gone
                try_rmtree(e)
    cutoff = time.time() - 6 * 3600
    for e in prefix.glob(".staging.*"):
        if e.is_dir() and not e.is_symlink() and e.stat().st_mtime < cutoff:
            try_rmtree(e)
    cur = receipt.get("current")
    if cur and vpath(prefix, cur).is_dir() and not pointer_ok(prefix, cur):
        switch_current(prefix, cur)


def prune_versions(prefix: Path, receipt: dict) -> None:
    keep = [v for v in (receipt.get("current"), receipt.get("previous")) if v]
    refs = runtime_refs(prefix, set(keep))  # kept versions may link to an older owner
    for v in list(receipt["versions"]):
        if v not in keep:
            d = vpath(prefix, v)
            if d.is_dir() and runtime_in_use(prefix, d, refs):
                continue  # runtime owner still referenced: stays listed until the last reference is gone
            if d.is_dir() and not try_rmtree(d):
                continue  # locked: stays listed so uninstall/recover retries
            receipt["versions"].remove(v)


def activate(prefix: Path, version: str, *, shortcuts: bool = True, selfcheck: bool = True, reused: bool = False) -> dict:
    """Validate, then switch as one unit. Any failure (even Ctrl-C) restores the previous pointer,
    shortcuts and receipt, so `current` never dangles.
    reused=True (the version's runtime/ is an attached link): the runtime must pass runtime_trusted() (else refuse), only the
    non-runtime files are hashed, only the bounded light check runs, and the prior runtime/GPU metadata is carried over.
    Returns this run's network setup result (refresh_network_service); the receipt is only written, never returned."""
    vdir = vpath(prefix, version)
    receipt = read_receipt(prefix)
    linked = is_link(vdir / "runtime")
    if (reused or linked) and not runtime_trusted(prefix, vdir):
        raise InstallError("the attached runtime is not a validated owned runtime; run the full installer or --repair")
    shared = (vdir / "runtime").resolve() if linked else None
    problems = verify_bundle(vdir, skip_runtime=reused)
    if problems:
        raise InstallError("bundle verification failed: " + fmt_problems(problems))
    if not selfcheck:
        info = dict(receipt.get("runtime") or {}, ffmpeg=ffmpeg_record(vdir))
    else:
        # Ordinary activation never imports engines or probes the GPU, for a trusted OR a freshly extracted runtime: only the
        # descriptor interpreter and the real UI binary are checked (bounded). The full engine self-check stays a build-time
        # gate (run_selfcheck, called by bundle.py), not an install step.
        say("Checking the runtime (first start can take a few minutes) ...")  # the one step that may stay silent
        info = dict(receipt.get("runtime") or {}, **run_light_check(vdir, shared), ffmpeg=ffmpeg_record(vdir))
    previous_receipt = copy.deepcopy(receipt)
    old = receipt.get("current")
    try:
        write_shims(prefix)
        if shortcuts:
            write_shortcuts(vdir, prefix)
        switch_current(prefix, version)
        manifest = read_manifest(vdir)
        receipt.update({
            "schema": 1, "app": receipt.get("app") or APP_ID, "prefix": str(prefix), "current": version,
            "previous": old if old and old != version else receipt.get("previous"),
            "app_version": manifest.get("app_version"), "platform": manifest.get("platform"),
            "min_glibc": manifest.get("min_glibc"),
            "lock_sha256": manifest.get("lock_sha256"), "catalog_sha256": manifest.get("catalog_sha256"),
            "activated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"),
            "gpu": receipt.get("gpu") or {},  # never probed here
            "runtime": info,
        })
        for k in ("adopted_from", "holder_of", "runtime_holder"):  # receipts from the adoption era: never written or read again
            receipt.pop(k, None)
        if version not in receipt["versions"]:
            receipt["versions"].append(version)
        prune_versions(prefix, receipt)
        atomic_write_json(prefix / RECEIPT, receipt)
        net = refresh_network_service(vdir, prefix, receipt)  # never fails activation; its outcome is recorded in receipt network.setup
    except BaseException as e:
        log_exc("activation failed", e)
        try:
            if old and vpath(prefix, old).is_dir():
                switch_current(prefix, old)
                atomic_write_json(prefix / RECEIPT, previous_receipt)
                if shortcuts:
                    write_shortcuts(vpath(prefix, old), prefix)
            else:  # first install: leave nothing of ours behind
                switch_current(prefix, None)
                remove_shortcuts(prefix)
                for name in SHIM_NAMES:
                    _unlink_owned(prefix / "bin" / name)
                _unlink_owned(prefix / "bin")
                (prefix / RECEIPT).unlink(missing_ok=True)
        except Exception as e2:
            log_exc("activation rollback did not complete; run 'keyloom-setup recover'", e2)
        raise
    return net


# ---- models ----------------------------------------------------------------

def run_models(prefix: Path, version: str, extra: list[str], timeout: int | None = None) -> dict:
    """Full model setup through the shared manager only (no other downloader).

    Installed models are trusted: nothing already ready is re-hashed or re-downloaded. An offline per-artifact check (stat/cache
    based, with a hang guard) selects only the missing/new artifacts, and only those are downloaded; the manager
    authenticates exactly those incoming files once. Ready = every artifact ready or its download reported ok.
    """
    vdir = vpath(prefix, version)
    py, app = descriptor_python(vdir), vdir / "app"
    env = clean_env(CK_INSTALL=str(descriptor_file(vdir)))
    base = [str(py), "-I", "-B", str(app / "scripts" / "setup_models.py")]

    def stage(op_args: list[str], op: str, limit: float | None = None, statuses: list | None = None) -> bool:
        """Run one manager op, forwarding its events; every quiet second prints elapsed/stall, every progress second bytes+speed.
        `limit` (seconds) is a stall guard: no data for that long kills it (reported as failure, never success)."""
        import queue
        import threading
        op_id = f"install-{uuid.uuid4().hex[:12]}"
        cmd = base + op_args + (["--operation-id", op_id] if op == "download" else []) + extra
        done = None
        say(f"[{op}] run: {' '.join(redact(cmd))}")
        p = subprocess.Popen(cmd, cwd=app, env=env, stdout=subprocess.PIPE, text=True, encoding="utf-8")
        lines: queue.Queue = queue.Queue()
        threading.Thread(target=lambda: ([lines.put(l) for l in p.stdout], lines.put(None)), daemon=True).start()
        t0 = last_data = last_say = time.monotonic()
        seen: dict = {}
        try:
            while True:
                try:
                    line = lines.get(timeout=1.0)
                except queue.Empty:
                    line = ""
                now = time.monotonic()
                if line is None:
                    break
                if limit is not None and now - last_data > limit:
                    p.kill()
                    p.wait()
                    say(f"  [{op}] FAILED: no data for {limit:.0f} s")
                    return False
                try:
                    ev = json.loads(line) if line else None
                except ValueError:
                    ev = None
                if ev is None:
                    if now - last_data >= 1.0 and now - last_say >= 1.0:  # visible stall/elapsed line, once per second
                        say(f"  [{op}] elapsed {now - t0:.0f}s, no data for {now - last_data:.0f}s")
                        last_say = now
                    continue
                last_data = now
                if ev.get("event") == "op-done":
                    done = ev
                elif ev.get("event") == "status" and statuses is not None:
                    statuses.append(ev)
                elif ev.get("event") in ("artifact-start", "artifact-done", "artifact-error"):
                    say(f"  [{op}] {ev.get('artifact') or ev.get('id') or ''} {ev['event']} {ev.get('message', '')}".rstrip())
                elif ev.get("event") == "progress" and now - last_say >= 1.0:
                    key, b = ev.get("artifact"), ev.get("bytes", ev.get("files_done", 0))
                    pb, pt = seen.get(key, (b, now))
                    speed = (b - pb) / max(now - pt, 1e-6) if b >= pb and now > pt else 0
                    seen[key] = (b, now)
                    total = ev.get("bytes_total") or ev.get("files_total") or 0
                    say(f"  [{op}] {key or ''} {b:,}/{total:,} {ev.get('unit', 'bytes')}, {speed / 1048576:.1f} MiB/s, elapsed {now - t0:.0f}s")
                    last_say = now
            p.wait(timeout=timeout)
        except BaseException:
            p.kill()
            p.wait()
            raise
        say(f"[{op}] exit {p.returncode}")
        # `check` reports readiness in op-done.ok; whether the check itself worked is the exit code plus a terminal event.
        return bool(done and p.returncode == 0 and (done.get("ok") or op == "check"))

    statuses: list = []
    checked = stage(["--check", "--json"], "check", INNER_TIMEOUT, statuses)  # offline, automatic, hang guard
    # Protocol (backend/model_provision.op_check): one {"event":"status","artifact":ID,"kind":K,"present":bool,"ready":bool,...} per
    # artifact. `present` = every required file exists at its catalog size (stat only); that is what an installed model is
    # trusted for here. Python-package/vendored-code artifacts are not downloadable and are not selected.
    if not checked or not statuses or any(not isinstance(s.get("artifact"), str) or not isinstance(s.get("present"), bool)
                                           for s in statuses):
        say("MODELS INCOMPLETE: the model status check gave no usable result; nothing was downloaded "
            "(run `keyloom-setup models` again).")
        return {"profile": "full", "ready": False, "error": "model-status-unusable", "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")}
    todo = sorted(s["artifact"] for s in statuses
                  if s["present"] is False and s.get("kind") not in ("local_package", "local_code"))
    if todo:
        ok = _download_models(stage, ["--download"] + [x for i in todo for x in ("--target", i)])
    else:
        say("All models are already installed; nothing to download.")
        ok = True
    return {"profile": "full", "ready": ok, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")}


def _download_models(stage, args: list[str]) -> bool:
    """The explicit model transfer: only this declares a wait to the supervisor (visible per-second events come from `stage`)."""
    with user_wait("explicit model download transfer"):
        return stage(args, "download")


def record_models(prefix: Path, result: dict) -> None:
    receipt = read_receipt(prefix)
    receipt["models"] = result
    atomic_write_json(prefix / RECEIPT, receipt)


def models_step(prefix: Path, version: str, extra: list[str]) -> bool:
    """Run + record; an interrupt is recorded as incomplete before it propagates."""
    try:
        models = run_models(prefix, version, extra)
    except BaseException:
        record_models(prefix, {"profile": "full", "ready": False, "interrupted": True})
        raise
    record_models(prefix, models)
    return models["ready"]


# ---- commands --------------------------------------------------------------

def preflight(prefix: Path, need_bytes: int) -> None:
    prefix.mkdir(parents=True, exist_ok=True)
    if not os.access(prefix, os.W_OK):
        raise InstallError(f"{prefix} is not writable; pick another --prefix")
    free = shutil.disk_usage(prefix).free
    if free < need_bytes * 2:  # staged copy + the old version kept for rollback
        raise InstallError(f"not enough space on {prefix}: need ~{need_bytes * 2 >> 20} MiB, free {free >> 20} MiB")


def discard_new(prefix: Path, version: str, backup: str | None) -> None:
    """Undo a payload that failed validation/was interrupted: remove it, put any same-version backup back."""
    dest = vpath(prefix, version)
    r = read_receipt(prefix)
    if backup is None and version in (r.get("current"), r.get("previous")):
        (prefix / TXN).unlink(missing_ok=True)
        return  # never delete a live version because a same-id re-activation failed
    if runtime_in_use(prefix, dest):
        return  # journal stays; recover() keeps the referenced runtime
    if not try_rmtree(dest):
        return  # locked (Windows): journal stays; recover() finishes this on the next run
    if backup and (prefix / "versions" / backup).is_dir():
        os.rename(prefix / "versions" / backup, dest)
    (prefix / TXN).unlink(missing_ok=True)


def current_is(prefix: Path, version: str, payload_files: dict | None = None, *, full: bool = False) -> bool:
    """True when `version` is active, its pointer is right and its files verify (and, given a payload manifest, the installed
    manifest equals it). Read-only.
    Fast (default, ordinary same-build check): trusts the owned receipt of a previously activated build, so runtime/ is not
    hashed, walked, stat'ed or linked; the non-runtime files are hashed and the runtime must pass runtime_trusted().
    full=True (explicit repair/deep check) hashes every file including the runtime, detecting bit-rot."""
    try:
        r = read_receipt(prefix)
        if r.get("current") != version:
            return False
        d = vpath(prefix, version)
        if payload_files is not None and read_manifest(d).get("files") != payload_files:
            return False
        if not (d.is_dir() and pointer_ok(prefix, version)):
            return False
        if full:
            return not verify_bundle(d)
        return bool(r.get("activated_at")) and runtime_trusted(prefix, d) and not verify_bundle(d, skip_runtime=True)
    except InstallError:
        return False


def target_state(prefix: Path, manifest: dict) -> dict:
    """What `install` would do with an incoming build of `manifest`. Read-only; no runtime content is read.
    {"state", "reason"}: current  - active and intact, nothing to do
                         in-place - recorded, inactive, identical, trusted: activate where it stands, place no files
                         fresh    - no folder yet
                         leftover - an UNRECORDED folder: removable (link-safe) before placing
                         replace  - recorded but damaged: replaced via the backup path
                         refused  - damaged/unrecorded and another version links to its runtime: stop, use --repair"""
    version = safe_version(manifest["version_id"])
    dest, rec = vpath(prefix, version), read_receipt(prefix)
    if current_is(prefix, version, manifest["files"]):
        return {"state": "current", "reason": ""}
    if not dest.exists():
        return {"state": "fresh", "reason": ""}
    used = runtime_in_use(prefix, dest)
    refused = {"state": "refused", "reason": f"{version} has a runtime that other installed versions use; run --repair on it "
                                             "instead of replacing or removing it"}
    if version not in rec["versions"]:
        return refused if used else {"state": "leftover", "reason": ""}
    try:
        same = (rec.get("current") != version and read_manifest(dest).get("files") == manifest["files"]
                and runtime_trusted(prefix, dest) and not verify_bundle(dest, skip_runtime=True))
    except InstallError:
        same = False
    if same:
        return {"state": "in-place", "reason": ""}
    return refused if used else {"state": "replace", "reason": ""}


def cmd_target_state(a) -> int:
    """Exit 0: nothing to place (current / in-place). 1: place the files (fresh / leftover / replace). 4: refused (see message).
    2 (InstallError): manifest unreadable. Prints `state=<name> <reason>`."""
    m = load_json(Path(a.manifest))
    if not m or not isinstance(m.get("files"), dict) or "version_id" not in m:
        raise InstallError(f"{a.manifest} is not a readable install manifest")
    ts = target_state(prefix_of(a), m)
    say(f"state={ts['state']} {ts['reason']}".rstrip())
    return {"current": 0, "in-place": 0, "fresh": 1, "leftover": 1, "replace": 1, "refused": 4}[ts["state"]]


def prepare_target(prefix: Path, manifest: dict) -> dict:
    """Make versions/<id> safe to receive natively copied files (the Windows installer writes them BEFORE `install --version`).
    Mutating: run under the mutation lock. Journals `prepared` first so the later `install --version` (which resumes this
    journal instead of undoing it) and any failure/crash can restore the old tree.
    -> {state, reason, backup}: current|in-place - place NOTHING (recorded in-place never enters file copy)
                                fresh   - destination absent/empty and journaled (a leftover was removed link-safe)
                                replace - a damaged recorded tree was MOVED to `backup` (journaled first); copy into a new folder
                                refused - its runtime is used by other installed versions or cannot be moved/removed: place nothing,
                                          run --repair on the installed version instead."""
    recover(prefix)  # settle any earlier interrupted prepare/install before deciding
    ts = target_state(prefix, manifest)
    out = {"state": ts["state"], "reason": ts["reason"], "backup": None}
    if ts["state"] in ("current", "in-place", "refused"):
        return out
    version = safe_version(manifest["version_id"])
    dest = vpath(prefix, version)
    (prefix / "versions").mkdir(parents=True, exist_ok=True)
    if ts["state"] == "leftover":  # unrecorded folder: not an installed version; removed link-safe before anything is placed
        if not try_rmtree(dest):
            return dict(out, state="refused", reason=f"{dest} is a leftover that cannot be removed now; close Keyloom and retry")
        out["state"] = "fresh"
    if ts["state"] == "replace":
        backup = f"{version}.old-{uuid.uuid4().hex[:6]}"
        atomic_write_json(prefix / TXN, {"version": version, "backup": backup, "prepared": True})  # journal BEFORE the move
        try:
            os.rename(dest, prefix / "versions" / backup)
        except OSError as e:
            (prefix / TXN).unlink(missing_ok=True)
            return dict(out, state="refused", reason=f"cannot replace this build; quit Keyloom and retry ({e})")
        out["backup"] = backup
    else:
        atomic_write_json(prefix / TXN, {"version": version, "backup": None, "prepared": True})
    return out


def cmd_prepare_target(a) -> int:
    """Exit 0: place nothing (state=current or state=in-place; then `install --version V` activates). 1: files may be placed
    (state=fresh or state=replace, destination prepared and journaled). 4: refused (message says why). 2: manifest unreadable."""
    m = load_json(Path(a.manifest))
    if not m or not isinstance(m.get("files"), dict) or "version_id" not in m:
        raise InstallError(f"{a.manifest} is not a readable install manifest")
    r = prepare_target(prefix_of(a), m)
    say(f"state={r['state']} {r['reason']}".rstrip())
    return {"current": 0, "in-place": 0, "fresh": 1, "replace": 1, "refused": 4}[r["state"]]


def cmd_attach_runtime(a) -> int:
    prefix = prefix_of(a)
    r = attach_runtime(prefix, safe_version(a.version))
    say(f"runtime attached ({r['source']}, {r['rid'][:12]})" if r["ok"] else f"runtime cannot be attached ({r['reason']})")
    return 0 if r["ok"] else 1


def cmd_runtime_available(a) -> int:
    """Exit 0: reusable. 1: a fresh runtime is needed. 2 (via InstallError in main): the manifest is missing or invalid."""
    m = load_json(Path(a.manifest))
    if not m or not isinstance(m.get("files"), dict):
        raise InstallError(f"{a.manifest} is not a readable install manifest")
    r = runtime_available(prefix_of(a), m)
    if r["invalid"]:
        raise InstallError(f"invalid manifest: {r['reason']}")
    say(f"runtime available ({r['source']}, {r['rid'][:12]})" if r["ok"] else f"runtime not available ({r['reason']})")
    return 0 if r["ok"] else 1


def cmd_check(a) -> int:
    prefix = prefix_of(a)
    if current_is(prefix, safe_version(a.version), full=a.full):
        say(f"Keyloom {a.version} is already installed and verified.")
        return 0
    return 1


ATTEST = ".keyloom-payload-verified.json"
ATTEST_PRODUCERS = ("segments", "online")  # installer_payload.py (offline authenticated segments/shards) / online.py (per-entry verified writes)


def write_attestation(payload: Path, manifest: dict, producer: str) -> str:
    """The ONE writer of the attestation (producers must not hand-roll the file). Call it only AFTER every payload byte was
    authenticated against its trusted hash and all writes under `payload` are complete; it records but verifies nothing itself.
    `payload` is the staged tree (or, for the Windows installer, versions/<id> where it placed the files). Returns the secure
    nonce the producer then passes as `install --payload-attested NONCE`. Same-user record; no claim of unforgeability."""
    if producer not in ATTEST_PRODUCERS:
        raise InstallError(f"unknown attestation producer {producer!r}")
    import secrets
    nonce = secrets.token_hex(16)
    atomic_write_json(payload / ATTEST, {"schema": 1, "producer": producer, "manifest_sha256": sha256_file(payload / MANIFEST),
                                         "runtime_id": runtime_id(manifest["files"]), "root": str(payload.resolve()), "nonce": nonce})
    if not IS_WIN:
        (payload / ATTEST).chmod(0o600)
    return nonce


def consume_attestation(payload: Path, manifest: dict | None, nonce: str | None) -> bool:
    """Same-user attestation that a producer authenticated every payload byte against its trusted hashes and then finished writing
    (NOT unforgeable proof: whoever can run the installer can also write it). Contract, schema 1, in PAYLOAD/ATTEST:
      {"schema":1,"producer":"segments"|"online","manifest_sha256":<sha256 of PAYLOAD/install-manifest.json>,
       "runtime_id":<runtime_id(manifest)>,"root":<resolved PAYLOAD path>,"nonce":<hex>}
    and the producer passes the same nonce as `install --payload-attested NONCE`. The file is removed whenever present (it must
    never be carried into versions/). Returns True only for a complete, matching attestation; False when no nonce was passed (the
    caller then verifies the incoming runtime itself, once). A nonce with a missing/invalid attestation is an error, never a skip."""
    f = payload / ATTEST
    present = f.is_symlink() or f.exists()
    try:
        if not nonce:
            return False
        if not re.fullmatch(r"[0-9a-f]{32,128}", nonce):
            raise InstallError("--payload-attested needs a hex nonce")
        if f.is_symlink() or not f.is_file():
            raise InstallError(f"--payload-attested was given but {ATTEST} is missing from the payload")
        if hasattr(os, "getuid") and f.stat().st_uid != os.getuid():
            raise InstallError(f"{ATTEST} is not owned by the installing user")
        d = load_json(f)
        manifest = manifest or read_manifest(payload)  # no-payload install: the version dir itself carries the manifest
        want = {"schema": 1, "manifest_sha256": sha256_file(payload / MANIFEST), "runtime_id": runtime_id(manifest["files"]),
                "root": str(payload.resolve())}
        if (not isinstance(d, dict) or d.get("producer") not in ATTEST_PRODUCERS or d.get("nonce") != nonce
                or any(d.get(k) != v for k, v in want.items())):
            raise InstallError(f"{ATTEST} does not match this payload (manifest, runtime identity, location or nonce)")
        return True
    finally:
        if present:
            try:
                f.unlink()
            except OSError:
                pass


SUMMARY_DONE = False  # set once print_summary ran, so a failure path after it never prints a second summary


def print_summary(prefix: Path, result: dict, rc: int) -> None:
    """The outcome block of install and repair. `say` puts it on the console and, through PAY, into install.log."""
    global SUMMARY_DONE
    SUMMARY_DONE = True
    net = result.get("network")
    app = f"installed {result.get('version')} at {prefix}" if result.get("app_installed") else f"NOT installed: {result.get('error', '')}"
    if net is None:
        netline = "not attempted"
    elif net.get("ok") is False:
        netline = f"FAILED ({net['code']}): {net['message']} [after {net['attempts']} attempts]"
    elif "running" in net:
        netline = f"{net['action']}, {'running' if net['running'] else 'not running'} ({net.get('mode') or 'no mode'})"
    else:
        netline = net.get("action", "not run")
    say("===== Keyloom setup summary =====")
    say(f"app:       {app}")
    say(f"firewall:  {result.get('firewall') or 'not applicable'}")
    say(f"network:   {netline}")
    say(f"models:    {result.get('models', 'not run')}")
    say(f"exit code: {rc}")
    say("=" * 33)


def foreign_entries(prefix: Path) -> list[str]:
    """Entries in prefix that a Keyloom install would not own (empty if prefix is absent)."""
    mine = lambda e: (e.name.startswith(".staging.") or e.name in (TXN, "install-result.json", "install.log")
                      or (e.name in ("versions", "bin") and e.is_dir() and not any(e.iterdir())))
    return [e.name for e in prefix.iterdir() if not mine(e)] if prefix.is_dir() else []


def cmd_install(a) -> int:
    prefix = prefix_of(a)
    payload = Path(a.payload).absolute() if a.payload else None
    backup = None
    # Stage 1: read-only probes that need none of each other's results (the payload manifest, the OS libraries). Results are
    # consumed in the fixed order below, so precedence and which error wins stay deterministic.
    # Everything that writes or depends on these results (preflight, recover, pointer/receipt work) stays serial after it.
    with ThreadPoolExecutor(max_workers=PROBE_WORKERS) as ex:
        f_man = ex.submit(read_manifest, payload) if payload else None
        f_libs = ex.submit(check_system_libs)
        manifest = f_man.result() if f_man else None
        if manifest:  # stage 2: needs the manifest
            version = safe_version(manifest["version_id"])
            check_glibc(manifest)
        else:  # Windows installer already placed files into versions/<version>
            version = safe_version(a.version or "")
        f_libs.result()
    if payload:
        preflight(prefix, manifest.get("total_bytes", 0))
    else:
        if not (prefix / "versions" / version).is_dir():
            raise InstallError("install needs --payload or an existing --version under PREFIX/versions")
    if payload and not (prefix / RECEIPT).exists():
        foreign = foreign_entries(prefix)
        if foreign:
            raise InstallError(f"{prefix} is not empty and not a Keyloom install ({', '.join(foreign[:4])}...); "
                               "choose an empty folder or a new subfolder")
    recover(prefix, resume=None if payload else version)
    dest = vpath(prefix, version)
    reuse = None
    if payload and current_is(prefix, version, manifest["files"]):
        # Same build, intact: change nothing (no activation, shortcuts, service, settings, runtime or models).
        say(f"Keyloom {version} is already installed and verified; nothing was changed. Use --repair to restore damaged files.")
        result = {"app_installed": True, "models": "unchanged", "version": version, "already_current": True, "exit_code": 0,
                  "network": {"action": "unchanged"}}
        atomic_write_json(prefix / "install-result.json", result)
        print_summary(prefix, result, 0)
        return 0
    inplace = False  # the target version is already installed with an identical manifest: activate it where it is
    if payload:
        ts = target_state(prefix, manifest)
        if ts["state"] == "refused":
            raise InstallError(ts["reason"])
        if ts["state"] == "leftover" and not try_rmtree(dest):  # unrecorded folder: removed link-safe, under the mutation lock
            raise InstallError(f"{dest} is a leftover that cannot be removed safely; close Keyloom and retry")
        # in-place: nothing is renamed or re-attached, so links from other versions to this (possibly runtime-owning) dir survive
        inplace = ts["state"] == "in-place"
    elif version in read_receipt(prefix)["versions"]:
        # `install --version V` (no payload, e.g. the Windows installer). Either `prepare-target` replaced a damaged recorded
        # version (journal `prepared`, old tree kept as `backup`): the placed files are NEW and a failure restores the backup.
        # Or the folder is an already recorded version: activated where it stands, never deleted on failure.
        t = load_json(prefix / TXN) or {}
        if t.get("prepared") and t.get("version") == version:
            if t.get("backup") and not re.fullmatch(re.escape(version) + r"\.old-[0-9a-f]{6}", str(t["backup"])):
                raise InstallError(f"damaged transaction journal in {prefix}")
            backup = t.get("backup")
        else:
            inplace = True
    # The producer's attestation is consumed (removed) here, before anything is moved, whatever happens next.
    attested = consume_attestation(payload or dest, manifest if payload else None, a.payload_attested)  # --version: dest holds the files
    if inplace:
        say(f"Keyloom {version} is already installed; activating it in place.")
        reuse = {"ok": True, "source": "in-place", "reason": ""}
        # Journal BEFORE activation writes shims/shortcuts/pointer/receipt/service: a crash leaves a record, and recover()
        # (which also repairs a pointer that disagrees with the receipt) never removes this installed target.
        atomic_write_json(prefix / TXN, {"version": version, "backup": None, "inplace": True})
    if payload and not inplace:
        has_rt = (payload / "runtime").exists() or is_link(payload / "runtime")
        if a.attach_runtime and has_rt:
            raise InstallError("--attach-runtime needs a payload without its own runtime/")
        if a.attach_runtime or (not has_rt and any(k.startswith("runtime/") for k in manifest["files"])):
            # One link to an already-installed, validated runtime; never an extraction/copy fallback. Nothing is moved yet, so a
            # refusal leaves the prefix exactly as it was.
            say("Attaching the installed runtime (one link, no files copied) ...")
            reuse = _attach(prefix, payload, dest)
            if not reuse["ok"]:
                raise InstallError(f"the installed runtime cannot be reused ({reuse['reason']}); "
                                   "run the full installer (or --repair) to extract a runtime")
    if payload and not inplace:
        (prefix / "versions").mkdir(parents=True, exist_ok=True)
        if dest.exists():  # same version present: keep it as a backup until the new one is live
            if runtime_in_use(prefix, dest):  # other versions link to this runtime: renaming it would break them
                raise InstallError(f"{version} is damaged but its runtime is still used by other installed versions; "
                                   "run --repair on it instead of replacing it")
            backup = f"{version}.old-{uuid.uuid4().hex[:6]}"
        atomic_write_json(prefix / TXN, {"version": version, "backup": backup})  # journal BEFORE touching versions/
        if backup:
            try:
                os.rename(dest, prefix / "versions" / backup)
            except OSError as e:
                (prefix / TXN).unlink(missing_ok=True)
                raise InstallError(f"cannot replace this build; quit Keyloom and retry ({e})") from None
        try:
            os.rename(payload, dest)  # same filesystem by construction (stub stages inside PREFIX)
        except BaseException:
            if backup:
                os.rename(prefix / "versions" / backup, dest)
            (prefix / TXN).unlink(missing_ok=True)
            raise
    if not payload and not inplace:  # files were placed by the Windows installer: journal them so a failure is recoverable
        atomic_write_json(prefix / TXN, {"version": version, "backup": backup})  # backup is set when prepare-target kept the old tree
    say(f"Validating Keyloom {version} ...")
    before = read_receipt(prefix)
    prior_models, prior_catalog = before.get("models"), before.get("catalog_sha256")
    try:
        net = activate(prefix, version, shortcuts=not a.no_shortcuts, reused=inplace or attested or is_link(dest / "runtime"))
    except BaseException:
        # previous version stays current (activate restored it); drop the new one, or leave the
        # journal so the next run finishes the cleanup (Windows: our own exe is locked while running).
        # An in-place target was installed before this run and is never deleted by its failed re-activation.
        if inplace:
            (prefix / TXN).unlink(missing_ok=True)  # activate() already restored pointer/receipt/shortcuts
        else:
            discard_new(prefix, version, backup)
        raise
    if inplace:
        (prefix / TXN).unlink(missing_ok=True)
        if payload and payload.resolve().is_relative_to(prefix.resolve()):
            try_rmtree(payload)  # the redundant staged copy (inside this prefix's staging area) is consumed, as a rename would have
    if backup and not runtime_in_use(prefix, prefix / "versions" / backup):  # a backup whose runtime another version links to stays
        try_rmtree(prefix / "versions" / backup)
    if not (backup and (prefix / "versions" / backup).exists()):
        (prefix / TXN).unlink(missing_ok=True)
    say(f"Application installed: {prefix} (version {version}).")
    rc, result = 0, {"app_installed": True, "models": "skipped", "runtime_reuse": reuse}
    try:
        result["firewall"] = firewall_step(prefix, a.firewall)
        if a.models == "full":
            say("Installing and verifying the full model set (large download; resumable) ...")
            ready = models_step(prefix, version, a.model_arg)
            result["models"] = "ready" if ready else "incomplete"
            if not ready:
                say("MODELS INCOMPLETE: the application is installed, but not every model is verified.\n"
                    f"  Resume with: {prefix / 'bin' / SHIM_NAMES[1]} models")
                rc = 3
        elif prior_models and prior_catalog == read_receipt(prefix).get("catalog_sha256"):
            say("Models unchanged (same model catalog as the previous version).")  # an update never resets verified models
        elif prior_models:
            # Catalog changed: installed artifacts stay ready (nothing is re-hashed or downloaded here); the next explicit
            # models run checks per artifact and fetches only what is missing or new.
            record_models(prefix, dict(prior_models, catalog_changed=True))
            say("Models kept as they are (not re-downloaded or re-verified); `keyloom-setup models` fetches any new ones.")
        else:
            record_models(prefix, {"profile": "full", "ready": False, "skipped": True})
            say("Models skipped: install them from the app (Models) or with `keyloom-setup models`.")
        # A skip that was not requested to install models is a success (exit 0); only a requested install that fails is 3.
    except BaseException as e:
        rc = 130 if isinstance(e, (KeyboardInterrupt, SystemExit)) else 2
        result["models"] = "interrupted" if rc == 130 else "failed"
        raise
    finally:
        result["network"] = net  # this run's own result: activate() returned it, so it is never a previous run's receipt
        atomic_write_json(prefix / "install-result.json", dict(result, version=version, exit_code=rc))
        print_summary(prefix, dict(result, version=version), rc)
    return rc


def cmd_repair(a) -> int:
    prefix = prefix_of(a)
    recover(prefix)
    receipt = read_receipt(prefix)
    cur = receipt.get("current")
    if not cur:
        raise InstallError("nothing installed here; run the installer")
    vdir = vpath(prefix, cur)
    problems = verify_bundle(vdir)
    if problems and a.payload:
        src = Path(a.payload).absolute()
        sm = read_manifest(src)
        if sm["version_id"] != cur:
            raise InstallError("payload version differs from the installed version; use the installer to update")
        want = read_manifest(vdir)["files"]
        for kind, rel in problems:
            if kind == "unsafe" or sm["files"].get(rel) != want.get(rel):
                continue  # only restore entries the payload manifest vouches for, identical to ours
            s, d = src / rel, vdir / rel
            d.parent.mkdir(parents=True, exist_ok=True)
            tmp = d.with_name(d.name + ".repair")
            tmp.unlink(missing_ok=True)
            if want[rel].get("symlink") is not None:
                os.symlink(want[rel]["symlink"], tmp)
            elif s.is_file() and not s.is_symlink() and sha256_file(s) == want[rel]["sha256"]:
                shutil.copy2(s, tmp)
            else:
                continue
            os.replace(tmp, d)
        problems = verify_bundle(vdir, only={r for _, r in problems})  # only what was restored; everything else was just hashed
    if problems:
        raise InstallError(f"{len(problems)} bundle file(s) still bad ({fmt_problems(problems)}); "
                           "run the installer again with --repair")
    # The full verify above is the one runtime hash of an explicit repair; activation must not repeat it (nor import engines).
    net = activate(prefix, cur, shortcuts=True, reused=True)  # rewrite shims/shortcuts/receipt
    say("Application files verified.")
    result = {"app_installed": True, "version": cur, "firewall": firewall_step(prefix, a.firewall), "models": "skipped"}
    rc = 0
    if a.models:
        ready = models_step(prefix, cur, a.model_arg)
        result["models"], rc = ("ready", 0) if ready else ("incomplete", 3)
    result["network"] = net
    print_summary(prefix, result, rc)
    return rc


def cmd_rollback(a) -> int:
    prefix = prefix_of(a)
    recover(prefix)
    prev = read_receipt(prefix).get("previous")
    if not prev or not vpath(prefix, prev).is_dir():
        raise InstallError("no previous version to roll back to")
    activate(prefix, prev, reused=True)  # already activated once: trusted installed runtime, no re-hash (app files still checked)
    say(f"Rolled back to {prev}.")
    return 0


def _unlink_owned(path: Path) -> bool:
    """Remove a file or link, or a directory only if empty (never recursive). False when it is still there."""
    try:
        if path.is_symlink() or path.is_file():
            path.unlink()
        elif path.is_dir():
            path.rmdir()
    except OSError as e:
        say(f"WARNING: cannot remove {path}: {type(e).__name__}: {e}")
        return False
    return not (path.is_symlink() or path.exists())


def owned_dir(d: Path) -> bool:
    """A versions/ entry is ours only if its own manifest names it (`<id>` or `<id>.old-xxxxxx`)."""
    base = re.sub(r"\.old-[0-9a-f]{6}$", "", d.name)
    try:
        m = json.loads((d / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return False
    return VERSION_RE.match(d.name) is not None and m.get("version_id") == base


def deferred_delete_command(dirs: list[Path], prefix: Path) -> list[str]:
    """Windows: a running interpreter cannot delete its own tree; a detached cmd removes it afterwards
    (and the then-empty versions/ and prefix dirs, without /s so a non-empty one survives)."""
    steps = [f'rmdir /s /q "{d}"' for d in dirs] + [f'rmdir "{prefix / "versions"}"', f'rmdir "{prefix / "bin"}"', f'rmdir "{prefix}"']
    return ["cmd", "/c", "ping -n 4 127.0.0.1 >nul & " + " & ".join(steps)]


def cmd_uninstall(a) -> int:
    prefix = prefix_of(a)
    receipt = load_json(prefix / RECEIPT)
    if not receipt or receipt.get("app") != APP_ID:
        raise InstallError(f"{prefix} has no Keyloom receipt; refusing to delete anything")
    if receipt.get("prefix") is not None and os.path.normcase(os.path.abspath(receipt["prefix"])) != os.path.normcase(str(prefix)):
        raise InstallError(f"receipt names prefix {receipt.get('prefix')}, not {prefix}; nothing was removed")
    receipt = read_receipt(prefix)  # validates every version id
    failed: list[str] = []
    # Owned set is recomputed, never read back from the receipt, so a tampered receipt cannot aim deletions.
    for v in receipt.get("versions", []):  # check EVERYTHING before deleting anything
        d = prefix / "versions" / v
        if d.exists() and not owned_dir(d):
            raise InstallError(f"{d} is listed in the receipt but is not a Keyloom bundle; refusing to uninstall")
    remove_network_service(prefix, receipt.get("current"))
    remove_firewall(prefix, receipt, a.firewall)
    remove_shortcuts(prefix)
    for name in SHIM_NAMES:
        if not _unlink_owned(prefix / "bin" / name):
            failed.append(str(prefix / "bin" / name))
    for name in ("current", "current.txt", "install-result.json", "install.log", TXN):
        if not _unlink_owned(prefix / name):
            failed.append(str(prefix / name))
    running = Path(sys.executable).resolve()  # resolved: a junction/symlinked runtime points into another version's tree
    vroot = prefix / "versions"
    names = set(receipt.get("versions", []))
    if vroot.is_dir():  # interrupted-install leftovers, but ONLY directories carrying our own manifest
        for e in vroot.iterdir():
            if e.is_dir() and not e.is_symlink() and owned_dir(e):
                names.add(e.name)
    deferred: list[Path] = []
    for v in sorted(names):
        d = prefix / "versions" / v
        if d.is_symlink() or not d.is_dir():
            continue
        if IS_WIN and running.is_relative_to(d.resolve()):
            deferred.append(d)  # the interpreter running us cannot delete itself
            continue
        if not try_rmtree(d):
            say(f"Could not remove {d} (in use?); remove it manually.")
            failed.append(str(d))
    for e in prefix.glob(".staging.*"):
        if e.is_dir() and not e.is_symlink():
            try_rmtree(e)
    if not _unlink_owned(prefix / RECEIPT):
        failed.append(str(prefix / RECEIPT))
    for d in (prefix / "bin", vroot, prefix):
        if not _unlink_owned(d) and not deferred:  # with deferred versions the Windows cleanup removes these after this window closes
            failed.append(str(d))
    if deferred:
        cmd = deferred_delete_command(deferred, prefix)
        try:
            p = subprocess.Popen(cmd, creationflags=0x00000008 | 0x08000000,
                                 close_fds=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            say(f"[uninstall] run: {' '.join(cmd)} (pid {p.pid})")
        except OSError as e:
            log_exc("[uninstall] deferred delete", e)
            failed.extend(str(d) for d in deferred)
    if failed:
        say("Keyloom uninstall INCOMPLETE; still present: " + "; ".join(failed))
        return 1
    if deferred:
        say(f"Keyloom removed; {len(deferred)} running version folder(s) are deleted after this window closes.")
    else:
        say("Keyloom removed. Projects, outputs, models and settings were left untouched.")
    return 0


def cmd_recover(a) -> int:
    recover(prefix_of(a))
    say("Recovery complete.")
    return 0


def cmd_status(a) -> int:
    prefix = prefix_of(a)
    receipt = read_receipt(prefix)
    cur = receipt.get("current")
    out = {"installed": bool(cur), "receipt": receipt}
    if cur and a.verify:
        out["problems"] = [list(p) for p in verify_bundle(vpath(prefix, cur))]
    print(json.dumps(out, indent=2, sort_keys=True))
    return 0


def cmd_models(a) -> int:
    prefix = prefix_of(a)
    recover(prefix)
    cur = read_receipt(prefix).get("current")
    if not cur:
        raise InstallError("nothing installed here")
    ready = models_step(prefix, cur, a.model_arg)
    say("Models READY." if ready else "Models INCOMPLETE.")
    return 0 if ready else 3


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="keyloom-setup")
    p.add_argument("--prefix", required=True)
    sub = p.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("install")
    i.add_argument("--payload")
    i.add_argument("--version")
    i.add_argument("--models", choices=["full", "skip"], default="skip",
                   help="skip (default): never download models; full: explicit request to download and verify all of them")
    i.add_argument("--no-shortcuts", action="store_true")
    i.add_argument("--attach-runtime", action="store_true",
                   help="the payload carries no runtime/: link the already-installed, validated one (fails instead of extracting)")
    i.add_argument("--payload-attested", metavar="NONCE",
                   help="the producer (offline segments / online installer) authenticated every payload byte; NONCE matches "
                        "PAYLOAD/.keyloom-payload-verified.json. Skips only the incoming-runtime re-hash (same-user attestation)")
    i.add_argument("--firewall", choices=["yes", "skip", "ask"], default="yes", help="firewall rules: yes (default) or skip; ask is accepted and means yes")
    i.add_argument("--model-arg", action="append", default=[], help="extra arg for setup_models.py; use --model-arg=--hf-cache")
    r = sub.add_parser("repair")
    r.add_argument("--payload")
    r.add_argument("--models", action="store_true")
    r.add_argument("--firewall", choices=["yes", "skip", "ask"], default="yes")
    r.add_argument("--model-arg", action="append", default=[])
    c = sub.add_parser("check", help="exit 0 if --version is the active, fully verified build (no changes)")
    c.add_argument("--version", required=True)
    c.add_argument("--full", action="store_true", help="also hash every runtime file (slow; what repair does)")
    al = sub.add_parser("attach-runtime", help="link --version's runtime/ to an available runtime (one link, no file copied/hashed); exit 1 if none")
    al.add_argument("--version", required=True)
    ts = sub.add_parser("target-state", help="what install would do with this manifest: exit 0 nothing to place, 1 place files, 4 refused")
    ts.add_argument("--manifest", required=True)
    pt = sub.add_parser("prepare-target", help="(mutating, locked) ready versions/<id> for natively placed files; exit 0 place nothing, 1 place, 4 refused")
    pt.add_argument("--manifest", required=True)
    ra = sub.add_parser("runtime-available", help="exit 0 if the manifest's runtime can be attached without extracting it")
    ra.add_argument("--manifest", required=True)
    sub.add_parser("rollback")
    sub.add_parser("recover")
    u = sub.add_parser("uninstall")
    u.add_argument("--skip-running", action="store_true", help="accepted for the Inno uninstaller; on Windows the running version is always deferred for deletion")
    u.add_argument("--firewall", choices=["ask", "skip"], default="ask")
    s = sub.add_parser("status")
    s.add_argument("--verify", action="store_true")
    m = sub.add_parser("models")
    m.add_argument("--model-arg", action="append", default=[])
    return p


def _die_on_signal(signum, _frame):
    raise SystemExit(128 + signum)  # unwinds through the BaseException cleanup paths


def main(argv=None) -> int:
    a = build_parser().parse_args(argv)
    for name in ("SIGTERM", "SIGHUP"):
        if hasattr(signal, name):
            signal.signal(getattr(signal, name), _die_on_signal)
    rc, failure = 2, ""
    try:
        # Everything that writes (recover, leftover removal, attach, activation, receipt/pointer, models) runs under the lock;
        # the read-only verbs (status, check, runtime-available) do not take it.
        lock = (mutation_lock(prefix_of(a)) if a.cmd in
                ("install", "repair", "rollback", "uninstall", "recover", "models", "attach-runtime", "prepare-target")
                else contextlib.nullcontext())
        with lock:
            return {"install": cmd_install, "repair": cmd_repair, "rollback": cmd_rollback,
                    "uninstall": cmd_uninstall, "status": cmd_status, "models": cmd_models,
                    "recover": cmd_recover, "check": cmd_check,
                    "attach-runtime": cmd_attach_runtime, "runtime-available": cmd_runtime_available,
                    "target-state": cmd_target_state, "prepare-target": cmd_prepare_target}[a.cmd](a)
    except InstallError as e:
        failure = f"{type(e).__name__}: {e}"
        say(f"ERROR: {e}")
        if e.__cause__:
            say("".join(traceback.format_exception(e)).rstrip())
    except KeyboardInterrupt:
        failure, rc = "KeyboardInterrupt: interrupted", 130
        say("Interrupted; state was rolled back or is recoverable (rerun to finish).")
    except Exception as e:  # cleanup already ran in the BaseException paths; never exit with a bare traceback
        failure = f"{type(e).__name__}: {e}"
        log_exc("unexpected", e)
    if a.cmd in ("install", "repair") and not SUMMARY_DONE:
        prefix, result = prefix_of(a), {"app_installed": False, "error": failure, "exit_code": rc, "version": getattr(a, "version", None)}
        # a failure before the normal result write still leaves its outcome on disk; write only into our own install or an empty/our-files folder
        try:
            if prefix.is_dir() and ((prefix / RECEIPT).exists() or not foreign_entries(prefix)):
                atomic_write_json(prefix / "install-result.json", result)
        except Exception as e:  # never let a failed write hide the original error
            log_exc("writing install-result.json", e)
        print_summary(prefix, result, rc)
    return rc


if __name__ == "__main__":
    sys.exit(main())
