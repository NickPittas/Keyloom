"""Retire the old background network service (keyloom-network.service / "Keyloom Network Service" task / Run value).

From the version that carries the in-app endpoint on, the app answers other PCs itself, so the service this install made
is stopped and its definition removed. Everything else stays exactly where it is: certificate, key, pin, join token, PC
identity, coordinator.sqlite3 and the registered PCs. No "host removed" tombstone is written, so an older build installed
on top finds its state and sets its service up again. Only a service this prefix owns is touched; running twice is safe.

Stdlib only. `python legacy_network_cleanup.py PREFIX` prints one `KEYLOOM-NET-RESULT {json}` line (lifecycle reads it).
"""
from __future__ import annotations

import contextlib
import hashlib
import json
import os
import re
import subprocess
import sys
import time
from pathlib import Path

IS_WIN = os.name == "nt"
UNIT = "keyloom-network.service"
TASK = "Keyloom Network Service"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
RUN_VALUE = "KeyloomNetworkService"
REGISTRY = "network-service.json"  # in the prefix: names the state dir of the service it owns
INSTALL = "service-install.json"  # in the state dir: who installed the service
RESULT = "KEYLOOM-NET-RESULT "
STOP_WAIT = 15  # seconds the service gets to leave on its own (stop.request) before the OS is asked
KILL_WAIT = 10  # seconds after the OS was asked


class CleanupError(Exception):
    pass


def owner_id(prefix) -> str:
    """Same id the old service wrote (backend/network_service.owner_id)."""
    return hashlib.sha256(os.path.normcase(str(Path(prefix).resolve())).encode("utf-8")).hexdigest()[:16]


def _json(p: Path):
    try:
        d = json.loads(p.read_text(encoding="utf-8"))
        return d if isinstance(d, dict) else None
    except (OSError, ValueError):
        return None


def _run(argv, timeout=30):
    return subprocess.run(argv, capture_output=True, timeout=timeout)


def _out(r) -> str:
    return (r.stdout + r.stderr).decode("utf-8", "replace").strip()[:2000]


def lock_busy(sd: Path) -> bool:
    """True while a service process holds the state directory's owner lock."""
    p = sd / "coordinator.lock"
    if not p.exists():
        return False
    with open(p, "a+") as f:
        try:
            if IS_WIN:
                import msvcrt
                msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
                f.seek(0)
                msvcrt.locking(f.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except OSError:
            return True
    return False


def unit_path() -> Path:
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "systemd/user" / UNIT


def _same(a, b) -> bool:
    try:
        return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(b))
    except OSError:
        return str(a) == str(b)


def _task_state_dir():
    """State dir recorded in the scheduled task's description, or None when there is no such task."""
    r = _run(["schtasks", "/Query", "/TN", TASK, "/XML"])
    if r.returncode:
        return None
    text = r.stdout.decode("utf-16", "replace") if r.stdout[:2] in (b"\xff\xfe", b"\xfe\xff") else r.stdout.decode("utf-8", "replace")
    m = re.search(r"<Description>(.*?)</Description>", text, re.S)
    try:
        import html
        return json.loads(html.unescape(m.group(1)))["state_dir"]
    except (AttributeError, ValueError, KeyError, TypeError):
        return ""


def _wait(sd: Path, seconds: float) -> bool:
    end = time.monotonic() + seconds
    while lock_busy(sd) and time.monotonic() < end:
        time.sleep(0.25)
    return not lock_busy(sd)


def _stop(sd: Path, mode: str) -> None:
    if not lock_busy(sd):
        return
    (sd / "stop.request").write_text("stop", encoding="utf-8")  # graceful on every platform; the service watches for it
    if not _wait(sd, STOP_WAIT):
        if mode == "systemd":
            _run(["systemctl", "--user", "stop", UNIT])
        elif mode == "task":
            _run(["schtasks", "/End", "/TN", TASK])
        _wait(sd, KILL_WAIT)
    (sd / "stop.request").unlink(missing_ok=True)
    if lock_busy(sd):
        raise CleanupError("the old network service did not stop; end it in Task Manager / `systemctl --user stop keyloom-network`, then run setup again")


def _delete_run_value() -> None:
    import winreg
    with contextlib.suppress(FileNotFoundError):
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
            winreg.DeleteValue(k, RUN_VALUE)


def _remove_definition(mode: str, sd: Path) -> bool:
    """Remove what starts the service; False = the definition is someone else's and was left alone."""
    if mode == "systemd":
        p = unit_path()
        if not p.is_file():
            return True
        m = re.search(r"^# X-Keyloom-State-Dir=(.*)$", p.read_text(encoding="utf-8", errors="replace"), re.M)
        if not (m and _same(m[1], sd)):
            return False
        r = _run(["systemctl", "--user", "disable", UNIT])
        if r.returncode:
            raise CleanupError(f"systemctl --user disable failed: {_out(r) or r.returncode}")
        p.unlink(missing_ok=True)
        r = _run(["systemctl", "--user", "daemon-reload"])
        if r.returncode:
            raise CleanupError(f"systemctl --user daemon-reload failed: {_out(r) or r.returncode}")
        return True
    if mode == "task":
        state = _task_state_dir()
        if state is not None:  # a task exists
            if not (state and _same(state, sd)):
                return False
            r = _run(["schtasks", "/Delete", "/TN", TASK, "/F"])
            if r.returncode:
                raise CleanupError(f"schtasks /Delete failed (exit {r.returncode}): {_out(r)}")
    _delete_run_value()  # task mode may have fallen back to the Run value; harmless when absent
    return True


def retire(prefix) -> dict:
    """Stop and unregister the service this prefix owns. Returns lifecycle's network result dict; raises CleanupError on failure."""
    prefix = Path(prefix)
    none = {"action": "none", "installed": False, "running": False, "mode": None}
    reg = _json(prefix / REGISTRY)
    if not reg or not isinstance(reg.get("state_dir"), str) or not os.path.isabs(reg["state_dir"]):
        return none | {"message": "no old network service to remove"}
    sd = Path(reg["state_dir"])
    inst = _json(sd / INSTALL) or {}
    if inst.get("owner") != owner_id(prefix):  # our registry, but the service belongs to another install (or is gone)
        if inst:
            return none | {"action": "foreign", "message": "the network service belongs to another installation and was left untouched"}
        (prefix / REGISTRY).unlink(missing_ok=True)  # half-removed earlier: nothing left to stop
        return none | {"message": "no old network service to remove"}
    mode = inst.get("mode") if inst.get("mode") in ("systemd", "task", "run") else ("task" if IS_WIN else "systemd")
    _stop(sd, mode)
    if not _remove_definition(mode, sd):
        return none | {"action": "foreign", "message": "the registered service definition belongs to another state folder and was left untouched"}
    (sd / INSTALL).unlink(missing_ok=True)
    (prefix / REGISTRY).unlink(missing_ok=True)  # last: a failed run above is repeated by the next one
    return none | {"action": "retired", "message": "the old background network service was stopped and removed; the app answers other PCs now"}


def main(argv=None) -> int:
    prefix = (argv if argv is not None else sys.argv[1:])[0]
    try:
        out = retire(prefix)
    except Exception as e:  # reported, never raised: lifecycle records it and goes on
        out = {"action": "failed", "installed": False, "running": False, "mode": None,
               "error": {"type": type(e).__name__, "code": "service_stop_failed", "message": str(e)}}
    print(RESULT + json.dumps(out))
    return 0


if __name__ == "__main__":
    sys.exit(main())
