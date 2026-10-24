"""Install lifecycle for a packaged Keyloom (powered by CorridorKey) bundle (stdlib only, Linux + Windows).

Runs with the bundle's own interpreter (`python -I`), both from the freshly
extracted payload (install) and from the installed tree (repair/rollback/
uninstall/models/status). Layout under PREFIX:

    versions/<ver>/   one self-contained bundle (bin/, app/, runtime/, manifest)
    current -> versions/<ver>   (Linux symlink; Windows uses receipt + shortcuts)
    bin/corridorkey[-setup|-coordinator]  stable shims
    receipt.json                owned files + state (the only thing uninstall trusts)

User data (projects, settings, models) lives OUTSIDE PREFIX and is never touched.
No listeners, services, firewall rules or network access are created here; the
only network use is the explicit model step (scripts/setup_models.py).
"""
from __future__ import annotations

import argparse
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
import uuid
from pathlib import Path

IS_WIN = os.name == "nt"
MANIFEST = "install-manifest.json"
RECEIPT = "receipt.json"
APP_ID = "CorridorKey"
KEEP_VERSIONS = 2  # active + previous (rollback target)


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


def try_rmtree(p: Path) -> bool:
    """Remove a tree without following links; False if something (e.g. a running exe) remains."""
    if p.is_symlink():
        p.unlink()
        return True
    shutil.rmtree(p, ignore_errors=True)
    return not p.exists()


def say(msg: str) -> None:
    print(msg, flush=True)


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


def verify_bundle(version_dir: Path) -> list[tuple[str, str]]:
    """Structured problems [(kind, relpath)] for every manifest file; kinds: missing/changed/link/unsafe."""
    problems = []
    for rel, meta in read_manifest(version_dir).get("files", {}).items():
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


def descriptor_python(version_dir: Path) -> Path:
    d = load_json(version_dir / "corridorkey-install.json")
    if not d:
        raise InstallError(f"{version_dir}: missing corridorkey-install.json")
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
    missing = [] if IS_WIN else [" or ".join(alts) for alts in LINUX_SYSTEM_LIBS if not any(loads(n) for n in alts)]
    if missing:
        raise InstallError("missing OS libraries: " + ", ".join(missing) + f"\n  Debian/Ubuntu/Mint: {APT_HINT}")
    if not loads("nvcuda.dll" if IS_WIN else "libcuda.so.1"):  # advisory: app and models install; GPU workflows need the driver
        say("WARNING: NVIDIA driver (CUDA) not found. Keyloom needs an NVIDIA GPU + driver to run; install the "
            "current NVIDIA driver (Linux Mint: Driver Manager) before use.")


def run_selfcheck(version_dir: Path, timeout: int = 300) -> dict:
    """Launch validation from an unrelated CWD with a polluted-env-free child:
    the bundled interpreter imports every engine, and the real UI binary resolves the descriptor."""
    py, app = descriptor_python(version_dir), version_dir / "app"
    with tempfile.TemporaryDirectory() as cwd:
        env = clean_env(CK_INSTALL=str(version_dir / "corridorkey-install.json"))
        try:
            r = subprocess.run([str(py), "-I", "-B", str(app / "packaging" / "selfcheck.py"), str(app)],
                               capture_output=True, text=True, cwd=cwd, env=env, timeout=timeout)
        except (OSError, subprocess.SubprocessError) as e:
            raise InstallError(f"launch validation could not run the bundled interpreter: {e}")
        if r.returncode != 0:
            raise InstallError(f"launch validation failed ({r.returncode}): {(r.stderr or r.stdout).strip()[-800:]}")
        try:
            info = json.loads(r.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            raise InstallError(f"launch validation gave no result: {r.stdout[-300:]}")
        exe = version_dir / "bin" / ("corridorkey-ui.exe" if IS_WIN else "corridorkey-ui")
        try:
            u = subprocess.run([str(exe), "--self-check"], capture_output=True, text=True, cwd=cwd, env=env, timeout=60)
        except (OSError, subprocess.SubprocessError) as e:
            raise InstallError(f"UI self-check could not run {exe}: {e}")
        try:
            ui = json.loads(u.stdout.strip().splitlines()[-1])
        except (ValueError, IndexError):
            ui = {}
        root = version_dir.resolve()
        inside = all(ui.get(k) and Path(ui[k]).resolve().is_relative_to(root) for k in ("python", "repo"))
        if u.returncode != 0 or not ui.get("ok") or not inside:
            raise InstallError(f"UI self-check failed ({u.returncode}): {(u.stdout or u.stderr).strip()[-500:]}")
    info["ui"] = {k: ui.get(k) for k in ("version", "installed")}
    return info


# ---- shortcuts and shims ---------------------------------------------------

def owner_id(prefix: Path) -> str:
    return hashlib.sha256(str(prefix).encode("utf-8")).hexdigest()[:16]


def desktop_entry(exec_path: Path, icon: Path | None, prefix: Path | None = None) -> str:
    """Exec quoting per the desktop-entry spec: reserved chars backslash-escaped inside double quotes,
    '%' doubled (field codes), then every backslash doubled again for the key-file string layer."""
    q = str(exec_path)
    for ch in ('\\', '"', '`', '$'):
        q = q.replace(ch, "\\" + ch)
    q = q.replace("%", "%%").replace("\\", "\\\\")
    lines = ["[Desktop Entry]", "Type=Application", "Name=Keyloom", "GenericName=powered by CorridorKey", "Comment=Keyloom — powered by CorridorKey: AI chroma keyer",
             f'Exec=env -u PYTHONPATH -u PYTHONHOME -u VIRTUAL_ENV PYTHONNOUSERSITE=1 "{q}" %F',
             "Terminal=false", "Categories=AudioVideo;Video;Graphics;"]
    if icon:
        lines.append(f"Icon={icon}")
    if prefix:
        lines.append(f"X-CorridorKey-Owner={owner_id(prefix)}")
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
        old = start.with_name("CorridorKey")  # pre-rename shortcut, removed on upgrade/uninstall
        return {"start_menu_dir": start, "lnk": start / "Keyloom.lnk", "old_dir": old, "old_lnk": old / "CorridorKey.lnk"}
    data = Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local/share")
    return {"desktop": data / "applications/corridorkey.desktop"}


def write_shortcuts(version_dir: Path, prefix: Path) -> None:
    sp = shortcut_paths()
    exe = version_dir / "bin" / ("corridorkey-ui.exe" if IS_WIN else "corridorkey-ui")
    icon = version_dir / "share" / ("corridorkey.ico" if IS_WIN else "corridorkey.png")
    icon = icon if icon.is_file() else None
    if IS_WIN:
        sp["start_menu_dir"].mkdir(parents=True, exist_ok=True)
        # By value through a temp .ps1 so quotes/Unicode in paths need no shell escaping.
        script = windows_shortcut_script(sp["lnk"], exe, Path.home(), icon)
        with tempfile.NamedTemporaryFile("w", suffix=".ps1", delete=False, encoding="utf-8-sig") as f:
            f.write(script)
        try:
            subprocess.run(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", f.name],
                           check=True, capture_output=True, timeout=60)
        finally:
            os.unlink(f.name)
        _unlink_owned(sp["old_lnk"])
        _unlink_owned(sp["old_dir"])
        update_windows_autostart(version_dir, prefix)
        return
    sp["desktop"].parent.mkdir(parents=True, exist_ok=True)
    tmp = sp["desktop"].with_suffix(".tmp")
    tmp.write_text(desktop_entry(exe, icon, prefix), encoding="utf-8")
    os.replace(tmp, sp["desktop"])


def remove_shortcuts(prefix: Path) -> None:
    sp = shortcut_paths()
    d = sp.get("desktop")
    if d and d.is_file() and not d.is_symlink() and \
            f"X-CorridorKey-Owner={owner_id(prefix)}" in d.read_text(encoding="utf-8", errors="replace"):
        d.unlink()  # only the entry this prefix wrote
    if IS_WIN:
        _unlink_owned(sp["lnk"])
        _unlink_owned(sp["start_menu_dir"])
        _unlink_owned(sp["old_lnk"])
        _unlink_owned(sp["old_dir"])
    remove_autostart(prefix)


# Per-user background autostart is written by the app, never by the installer. Contract: it launches the stable
# Linux shim PREFIX/bin/corridorkey; Windows GUI exe, rewritten on activation/rollback (no console at login).
# Linux: $XDG_CONFIG_HOME/autostart/corridorkey.desktop containing X-CorridorKey-Owner=<owner_id(prefix)>.
# Windows: HKCU\Software\Microsoft\Windows\CurrentVersion\Run value "CorridorKey" under this prefix.
# Uninstall removes only an entry that matches this prefix; anything else is left alone.
AUTOSTART_RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def owns_windows_autostart(value: str, prefix: Path) -> bool:
    return any(value.lower().startswith('"' + str(prefix / d).lower() + "\\") for d in ("bin", "versions"))


def update_windows_autostart(version_dir: Path, prefix: Path) -> None:
    import winreg
    try:
        k = winreg.OpenKey(winreg.HKEY_CURRENT_USER, AUTOSTART_RUN_KEY, 0,
                          winreg.KEY_QUERY_VALUE | winreg.KEY_SET_VALUE)
    except FileNotFoundError:
        return
    with k:
        try:
            value = str(winreg.QueryValueEx(k, "CorridorKey")[0])
        except FileNotFoundError:
            return
        if owns_windows_autostart(value, prefix):
            winreg.SetValueEx(k, "CorridorKey", 0, winreg.REG_SZ,
                             f'"{version_dir / "bin/corridorkey-ui.exe"}" --background')


def remove_autostart(prefix: Path) -> None:
    if IS_WIN:
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, AUTOSTART_RUN_KEY, 0,
                                winreg.KEY_QUERY_VALUE | winreg.KEY_SET_VALUE) as k:
                val = str(winreg.QueryValueEx(k, "CorridorKey")[0])
                if owns_windows_autostart(val, prefix):
                    winreg.DeleteValue(k, "CorridorKey")
        except OSError:
            pass
        return
    d = Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "autostart/corridorkey.desktop"
    if d.is_file() and not d.is_symlink() and \
            f"X-CorridorKey-Owner={owner_id(prefix)}" in d.read_text(encoding="utf-8", errors="replace"):
        d.unlink()


def _network_service(version_dir: Path):
    """The version's own stdlib-only service manager, loaded by path (no sys.path or cwd involvement)."""
    import importlib.util
    f = version_dir / "app/backend/network_service.py"
    spec = importlib.util.spec_from_file_location("ck_network_service", f)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def refresh_network_service(version_dir: Path, prefix: Path) -> None:
    """Activation/rollback: an optional per-user network service this prefix owns follows the active version.
    Absent registry = nothing to do; any failure is reported but never fails the activation."""
    if not (prefix / "network-service.json").is_file():
        return
    try:
        msg = _network_service(version_dir).refresh(descriptor_python(version_dir), version_dir / "app", prefix)
        if msg:
            say(msg)
    except Exception as e:
        say(f"WARNING: the network service was not updated ({getattr(e, 'message', type(e).__name__)}); open the app and use Network > Service.")


def remove_network_service(prefix: Path, version: str | None) -> None:
    """Uninstall: unregister only the service this prefix owns (state, certificate and database are left in place)."""
    if not (prefix / "network-service.json").is_file():
        return
    try:
        msg = _network_service(vpath(prefix, version)).remove_owned(prefix)
        if msg:
            say(msg)
    except Exception as e:
        raise InstallError(f"the network service could not be stopped and removed ({getattr(e, 'message', type(e).__name__)}); the app was kept installed so you can stop the service from Clients and retry") from e


# Order matters: SHIM_NAMES[1] is the setup shim (install's "Resume with" hint).
SHIM_NAMES = (("corridorkey.cmd", "corridorkey-setup.cmd", "corridorkey-coordinator.cmd") if IS_WIN
              else ("corridorkey", "corridorkey-setup", "corridorkey-coordinator"))


def write_shims(prefix: Path) -> None:
    """Stable launchers in PREFIX/bin that follow `current` and scrub Python environment variables."""
    bindir = prefix / "bin"
    bindir.mkdir(parents=True, exist_ok=True)
    if IS_WIN:
        cur = r'for /f "usebackq delims=" %%V in ("%~dp0..\current.txt") do set "CKV=%%V"'
        env = 'set "PYTHONPATH="\nset "PYTHONHOME="\nset "VIRTUAL_ENV="\nset "PYTHONNOUSERSITE=1"'
        files = {
            "corridorkey.cmd": f'@echo off\n{cur}\n{env}\nstart "" "%~dp0..\\versions\\%CKV%\\bin\\corridorkey-ui.exe" %*\n',
            "corridorkey-setup.cmd": f'@echo off\n{cur}\n{env}\n"%~dp0..\\versions\\%CKV%\\runtime\\python\\python.exe" -I -B '
                                     '"%~dp0..\\versions\\%CKV%\\app\\packaging\\lifecycle.py" --prefix "%~dp0.." %*\n',
            # Server mode is opt-in: this shim only runs when invoked; it opens nothing by itself.
            # -P keeps the caller's working directory off sys.path; relative --state-dir stays relative to it.
            "corridorkey-coordinator.cmd": f'@echo off\n{cur}\n{env}\nset "PYTHONPATH=%~dp0..\\versions\\%CKV%\\app"\n'
                                           '"%~dp0..\\versions\\%CKV%\\runtime\\python\\python.exe" -B -P -m backend.coordinator %*\n',
        }
    else:
        env = "unset PYTHONPATH PYTHONHOME VIRTUAL_ENV PYTHONSTARTUP PYTHONUSERBASE\nPYTHONNOUSERSITE=1; export PYTHONNOUSERSITE\n"
        files = {
            "corridorkey": env + 'exec "$(dirname "$0")/../current/bin/corridorkey-ui" "$@"\n',
            "corridorkey-setup": env + 'P="$(dirname "$0")/.."\nexec "$P/current/runtime/python/bin/python3" -I -B '
                                       '"$P/current/app/packaging/lifecycle.py" --prefix "$P" "$@"\n',
            # Server mode is opt-in: this shim only runs when invoked; it opens nothing by itself.
            # -P keeps the caller's working directory off sys.path; relative --state-dir stays relative to it.
            "corridorkey-coordinator": env + 'P="$(cd "$(dirname "$0")/.." && pwd)"\n'
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
        r = subprocess.run(["nvidia-smi", "--query-gpu=name,driver_version,memory.total",
                            "--format=csv,noheader"], capture_output=True, text=True, timeout=15)
        if r.returncode == 0 and r.stdout.strip():
            return {"nvidia": r.stdout.strip().splitlines()}
        return {"nvidia": None, "note": "nvidia-smi failed; CorridorKey 2.0 needs an NVIDIA GPU + driver"}
    except (OSError, subprocess.SubprocessError):
        return {"nvidia": None, "note": "nvidia-smi not found; CorridorKey 2.0 needs an NVIDIA GPU + driver"}


TXN = ".txn.json"


def recover(prefix: Path) -> None:
    """Finish/undo an interrupted install: unreceipted version dirs, same-version backups, stale
    staging, and a pointer that disagrees with the receipt. Safe to run at the start of any command."""
    if not prefix.is_dir():
        return
    receipt = read_receipt(prefix)
    keep = {v for v in (receipt.get("current"), receipt.get("previous")) if v}
    txn = load_json(prefix / TXN)
    if txn:
        v = safe_version(txn["version"])
        d = vpath(prefix, v)
        backup = txn.get("backup")
        bd = prefix / "versions" / backup if backup else None
        if bd is not None and (not re.fullmatch(re.escape(v) + r"\.old-[0-9a-f]{6}", backup)):
            raise InstallError(f"damaged transaction journal in {prefix}")
        good = d.is_dir() and v in keep and not verify_bundle(d)
        if not good:
            if d.exists() and v not in keep or d.exists() and bd is not None:
                if not try_rmtree(d):
                    return  # still locked (Windows); journal stays for the next run
            if bd is not None and bd.is_dir() and not d.exists():
                os.rename(bd, d)
        if bd is not None and bd.exists():
            try_rmtree(bd)
        (prefix / TXN).unlink()
    vroot = prefix / "versions"
    if vroot.is_dir():
        for e in vroot.iterdir():
            if re.search(r"\.old-[0-9a-f]{6}$", e.name) and e.is_dir() and not e.is_symlink():
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
    for v in list(receipt["versions"]):
        if v not in keep:
            d = vpath(prefix, v)
            if d.is_dir() and not try_rmtree(d):
                continue  # locked: stays listed so uninstall/recover retries
            receipt["versions"].remove(v)


def activate(prefix: Path, version: str, *, shortcuts: bool = True, selfcheck: bool = True) -> dict:
    """Validate, then switch as one unit. Any failure (even Ctrl-C) restores the previous pointer,
    shortcuts and receipt, so `current` never dangles."""
    vdir = vpath(prefix, version)
    problems = verify_bundle(vdir)
    if problems:
        raise InstallError("bundle verification failed: " + fmt_problems(problems))
    info = run_selfcheck(vdir) if selfcheck else {}
    receipt = read_receipt(prefix)
    previous_receipt = copy.deepcopy(receipt)
    old = receipt.get("current")
    try:
        write_shims(prefix)
        if shortcuts:
            write_shortcuts(vdir, prefix)
        switch_current(prefix, version)
        manifest = read_manifest(vdir)
        receipt.update({
            "schema": 1, "app": APP_ID, "prefix": str(prefix), "current": version,
            "previous": old if old and old != version else receipt.get("previous"),
            "app_version": manifest.get("app_version"), "platform": manifest.get("platform"),
            "min_glibc": manifest.get("min_glibc"),
            "lock_sha256": manifest.get("lock_sha256"), "catalog_sha256": manifest.get("catalog_sha256"),
            "activated_at": time.strftime("%Y-%m-%dT%H:%M:%S%z"), "gpu": gpu_info(), "runtime": info,
            "network": {"listeners": False, "services": False, "firewall_rules": False},
        })
        if version not in receipt["versions"]:
            receipt["versions"].append(version)
        prune_versions(prefix, receipt)
        atomic_write_json(prefix / RECEIPT, receipt)
        refresh_network_service(vdir, prefix)  # last step: nothing after it can fail and leave the service on a rolled-back tree
    except BaseException:
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
        except Exception:
            pass
        raise
    return receipt


# ---- models ----------------------------------------------------------------

def run_models(prefix: Path, version: str, extra: list[str], timeout: int | None = None) -> dict:
    """Full model setup through the shared manager only (no other downloader).

    Ready is recorded only when download AND an offline verify both report ok.
    """
    vdir = vpath(prefix, version)
    py, app = descriptor_python(vdir), vdir / "app"
    env = clean_env(CK_INSTALL=str(vdir / "corridorkey-install.json"))
    base = [str(py), "-I", "-B", str(app / "scripts" / "setup_models.py")]

    def stage(op_args: list[str], op: str) -> bool:
        op_id = f"install-{uuid.uuid4().hex[:12]}"
        cmd = base + op_args + (["--operation-id", op_id] if op == "download" else []) + extra
        done = None
        p = subprocess.Popen(cmd, cwd=app, env=env, stdout=subprocess.PIPE, text=True, encoding="utf-8")
        try:
            for line in p.stdout:
                try:
                    ev = json.loads(line)
                except ValueError:
                    continue
                if ev.get("event") == "op-done":
                    done = ev
                elif ev.get("event") in ("artifact-start", "artifact-done", "artifact-error"):
                    say(f"  [{op}] {ev.get('artifact') or ev.get('id') or ''} {ev['event']} {ev.get('message', '')}".rstrip())
            p.wait(timeout=timeout)
        except BaseException:
            p.kill()
            p.wait()
            raise
        return bool(done and done.get("ok") and p.returncode == 0)

    ok = stage(["--download", "--profile", "full"], "download") and stage(["--verify"], "verify")
    return {"profile": "full", "ready": ok, "checked_at": time.strftime("%Y-%m-%dT%H:%M:%S%z")}


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
    if not try_rmtree(dest):
        return  # locked (Windows): journal stays; recover() finishes this on the next run
    if backup and (prefix / "versions" / backup).is_dir():
        os.rename(prefix / "versions" / backup, dest)
    (prefix / TXN).unlink(missing_ok=True)


def cmd_install(a) -> int:
    prefix = Path(a.prefix).absolute()
    payload = Path(a.payload).absolute() if a.payload else None
    backup = None
    if payload:
        manifest = read_manifest(payload)
        version = safe_version(manifest["version_id"])
        check_glibc(manifest)
        check_system_libs()
        preflight(prefix, manifest.get("total_bytes", 0))
    else:  # Windows installer already placed files into versions/<version>
        version = safe_version(a.version or "")
        check_system_libs()
        if not (prefix / "versions" / version).is_dir():
            raise InstallError("install needs --payload or an existing --version under PREFIX/versions")
    if payload and not (prefix / RECEIPT).exists():
        mine = lambda e: (e.name.startswith(".staging.") or e.name in (TXN, "install-result.json")
                          or (e.name in ("versions", "bin") and e.is_dir() and not any(e.iterdir())))
        foreign = [e.name for e in prefix.iterdir() if not mine(e)] if prefix.is_dir() else []
        if foreign:
            raise InstallError(f"{prefix} is not empty and not a CorridorKey install ({', '.join(foreign[:4])}...); "
                               "choose an empty folder or a new subfolder")
    recover(prefix)
    dest = vpath(prefix, version)
    if payload:
        (prefix / "versions").mkdir(parents=True, exist_ok=True)
        if dest.exists():  # same version present: keep it as a backup until the new one is live
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
    if not payload:  # files were placed by the Windows installer: journal them so a failure is recoverable
        atomic_write_json(prefix / TXN, {"version": version, "backup": None})
    say(f"Validating Keyloom {version} ...")
    before = read_receipt(prefix)
    prior_models, prior_catalog = before.get("models"), before.get("catalog_sha256")
    try:
        activate(prefix, version, shortcuts=not a.no_shortcuts)
    except BaseException:
        # previous version stays current (activate restored it); drop the new one, or leave the
        # journal so the next run finishes the cleanup (Windows: our own exe is locked while running).
        discard_new(prefix, version, backup)
        raise
    if backup:
        try_rmtree(prefix / "versions" / backup)
    if not (backup and (prefix / "versions" / backup).exists()):
        (prefix / TXN).unlink(missing_ok=True)
    say(f"Application installed: {prefix} (version {version}).")
    rc, result = 0, {"app_installed": True, "models": "skipped"}
    try:
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
            rc = 0 if prior_models.get("ready") else 3
        else:
            record_models(prefix, {"profile": "full", "ready": False, "skipped": True})
            say("Models skipped: setup is INCOMPLETE until `corridorkey-setup models` succeeds.")
            rc = 3
    except BaseException:
        rc, result["models"] = 130, "interrupted"
        raise
    finally:
        atomic_write_json(prefix / "install-result.json", dict(result, version=version, exit_code=rc))
    return rc


def cmd_repair(a) -> int:
    prefix = Path(a.prefix).absolute()
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
        problems = verify_bundle(vdir)
    if problems:
        raise InstallError(f"{len(problems)} bundle file(s) still bad ({fmt_problems(problems)}); "
                           "run the installer again with --repair")
    activate(prefix, cur, shortcuts=True)  # re-validate, rewrite shims/shortcuts/receipt
    say("Application files verified.")
    if a.models:
        return 0 if models_step(prefix, cur, a.model_arg) else 3
    return 0


def cmd_rollback(a) -> int:
    prefix = Path(a.prefix).absolute()
    recover(prefix)
    prev = read_receipt(prefix).get("previous")
    if not prev or not vpath(prefix, prev).is_dir():
        raise InstallError("no previous version to roll back to")
    activate(prefix, prev)
    say(f"Rolled back to {prev}.")
    return 0


def _unlink_owned(path: Path) -> None:
    if path.is_symlink() or path.is_file():
        path.unlink()
    elif path.is_dir():
        try:
            path.rmdir()  # only if empty; never recursive on an arbitrary path
        except OSError:
            pass


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
    prefix = Path(a.prefix).absolute()
    receipt = load_json(prefix / RECEIPT)
    if not receipt or receipt.get("app") != APP_ID:
        raise InstallError(f"{prefix} has no CorridorKey receipt; refusing to delete anything")
    receipt = read_receipt(prefix)  # validates every version id
    # Owned set is recomputed, never read back from the receipt, so a tampered receipt cannot aim deletions.
    for v in receipt.get("versions", []):  # check EVERYTHING before deleting anything
        d = prefix / "versions" / v
        if d.exists() and not owned_dir(d):
            raise InstallError(f"{d} is listed in the receipt but is not a CorridorKey bundle; refusing to uninstall")
    remove_network_service(prefix, receipt.get("current"))
    remove_shortcuts(prefix)
    for name in SHIM_NAMES:
        _unlink_owned(prefix / "bin" / name)
    for name in ("current", "current.txt", "install-result.json", TXN):
        _unlink_owned(prefix / name)
    running = Path(sys.executable).absolute()
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
        if running.is_relative_to(d) and (a.skip_running or IS_WIN):
            deferred.append(d)  # the interpreter running us cannot delete itself
            continue
        if not try_rmtree(d):
            say(f"Could not remove {d} (in use?); remove it manually.")
    for e in prefix.glob(".staging.*"):
        if e.is_dir() and not e.is_symlink():
            try_rmtree(e)
    _unlink_owned(prefix / RECEIPT)
    for d in (prefix / "bin", vroot, prefix):
        _unlink_owned(d)
    if deferred and IS_WIN:
        subprocess.Popen(deferred_delete_command(deferred, prefix), creationflags=0x00000008 | 0x08000000,
                         close_fds=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    say("Keyloom removed. Projects, outputs, models and settings were left untouched.")
    return 0


def cmd_recover(a) -> int:
    recover(Path(a.prefix).absolute())
    say("Recovery complete.")
    return 0


def cmd_status(a) -> int:
    prefix = Path(a.prefix).absolute()
    receipt = read_receipt(prefix)
    cur = receipt.get("current")
    out = {"installed": bool(cur), "receipt": receipt}
    if cur and a.verify:
        out["problems"] = [list(p) for p in verify_bundle(vpath(prefix, cur))]
    print(json.dumps(out, indent=2, sort_keys=True))
    return 0


def cmd_models(a) -> int:
    prefix = Path(a.prefix).absolute()
    recover(prefix)
    cur = read_receipt(prefix).get("current")
    if not cur:
        raise InstallError("nothing installed here")
    ready = models_step(prefix, cur, a.model_arg)
    say("Models READY." if ready else "Models INCOMPLETE.")
    return 0 if ready else 3


def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(prog="corridorkey-setup")
    p.add_argument("--prefix", required=True)
    sub = p.add_subparsers(dest="cmd", required=True)
    i = sub.add_parser("install")
    i.add_argument("--payload")
    i.add_argument("--version")
    i.add_argument("--models", choices=["full", "skip"], default="full")
    i.add_argument("--no-shortcuts", action="store_true")
    i.add_argument("--model-arg", action="append", default=[], help="extra arg for setup_models.py; use --model-arg=--hf-cache")
    r = sub.add_parser("repair")
    r.add_argument("--payload")
    r.add_argument("--models", action="store_true")
    r.add_argument("--model-arg", action="append", default=[])
    sub.add_parser("rollback")
    sub.add_parser("recover")
    u = sub.add_parser("uninstall")
    u.add_argument("--skip-running", action="store_true")
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
    try:
        return {"install": cmd_install, "repair": cmd_repair, "rollback": cmd_rollback,
                "uninstall": cmd_uninstall, "status": cmd_status, "models": cmd_models,
                "recover": cmd_recover}[a.cmd](a)
    except InstallError as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("Interrupted; state was rolled back or is recoverable (rerun to finish).", file=sys.stderr)
        return 130
    except Exception as e:  # cleanup already ran in the BaseException paths; never exit with a bare traceback
        if os.environ.get("CK_DEBUG"):
            raise
        print(f"ERROR: unexpected {type(e).__name__}: {e} (set CK_DEBUG=1 for a traceback)", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
