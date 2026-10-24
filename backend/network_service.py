"""Per-user network service: the coordinator hosted by an installed app, started at user sign-in (no admin, no console).

Two halves in one file. The top half is stdlib-only OS management (also imported by packaging/lifecycle.py from a file path,
so nothing here may import the app at module level). The bottom half is the service process (`main`).

Process:  <python> -I -B -c BOOT <app root> <state dir>      (-I: no PYTHON* variables, no user site, no cwd on sys.path;
                                                              the app root is put on sys.path explicitly by BOOT)
  Linux    systemd --user unit  corridorkey-network.service     Windows  Task Scheduler logon task "CorridorKey Network Service"
  (pythonw, LeastPrivilege, current user only); if the task cannot be created without elevation, an owned HKCU Run value
  "CorridorKeyNetworkService" + a detached pythonw start is used instead. Ownership of the definition is proven by the
  state directory it names and by `service-install.json` inside that directory; anything else is never touched.

State dir (private, created 0700): service.json (config), service-install.json, service-state.json (pid/state/error, no secrets),
coordinator.lock (held while running), coordinator.sqlite3, server-cert.pem/server-key.pem, local-join.token (rewritten at every
start, loopback-only join), stop.request (graceful-stop sentinel). Errors are ServiceError(code, message) with codes:
bad_request, permission, port_in_use, service_unavailable, service_conflict, not_installed, not_configured, cert_mismatch,
start_failed, start_timeout, busy, internal."""
import errno
import hashlib
import html
import json
import os
import re
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path

IS_WIN = os.name == "nt"
UNIT = "corridorkey-network.service"
TASK = "CorridorKey Network Service"
RUN_VALUE = "CorridorKeyNetworkService"
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
REGISTRY = "network-service.json"  # in the install prefix: tells lifecycle which state dir its service uses
BOOT = "import sys;sys.path.insert(0,sys.argv[1]);from backend.network_service import main;sys.exit(main(sys.argv[2:]))"
LOCALHOSTS = ("localhost", "127.0.0.1", "::1")
_CFG = {"version", "address", "port", "allow_address_join"}
_HOST = re.compile(r"[A-Za-z0-9]([A-Za-z0-9.-]{0,251}[A-Za-z0-9])?\Z")


class ServiceError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message


# ---- small helpers -----------------------------------------------------------------------------

def _run(cmd, timeout=30):
    try:
        return subprocess.run(cmd, capture_output=True, timeout=timeout, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    except FileNotFoundError:
        raise ServiceError("service_unavailable", f"{cmd[0]} is not available on this system") from None
    except subprocess.TimeoutExpired:
        raise ServiceError("service_unavailable", f"{cmd[0]} did not answer in {timeout}s") from None


def _out(r):
    return (r.stderr or r.stdout or b"").decode("utf-8", "replace").strip()[:300]


def owner_id(root_or_prefix):
    return hashlib.sha256(os.path.normcase(str(Path(root_or_prefix).resolve())).encode("utf-8")).hexdigest()[:16]


def install_context():
    """(app root, owner id) of the running code. Packaged trees are <prefix>/versions/<v>/app: owner = lifecycle.owner_id(prefix)."""
    root = Path(__file__).resolve().parents[1]
    if root.name == "app" and (root.parent / "corridorkey-install.json").is_file():
        return root, owner_id(root.parent.parent.parent), root.parent.parent.parent
    return root, owner_id(root), None


def service_python(py):
    py = Path(py).absolute()
    if IS_WIN:
        w = py.with_name("pythonw.exe")
        if not w.is_file():
            raise ServiceError("service_unavailable", f"pythonw.exe was not found next to {py.name}; cannot run without a console")
        return w
    return py


def validate(address, port, allow):
    if (not isinstance(address, str) or not _HOST.match(address) and ":" not in address or len(address) > 253
            or address.lower() in LOCALHOSTS or address in ("0.0.0.0", "::")):
        raise ServiceError("bad_request", "address must be the host name or IP address other PCs will use (not localhost or a wildcard)")
    import ipaddress
    try:
        ip = ipaddress.ip_address(address)
        if ip.version != 4:
            raise ServiceError("bad_request", "hosting uses IPv4; enter this PC’s LAN or Tailscale IPv4 address")
        if ip.is_unspecified or ip.is_loopback:
            raise ValueError
    except ValueError:
        if ":" in address or re.fullmatch(r"[0-9.]+", address):
            raise ServiceError("bad_request", "address is not a usable IP address") from None
    if isinstance(port, bool) or not isinstance(port, int) or not 1024 <= port <= 65535:
        raise ServiceError("bad_request", "port must be an integer from 1024 to 65535")
    if not isinstance(allow, bool):
        raise ServiceError("bad_request", "allow_address_join must be true or false")


def state_path(state_dir):
    p = str(state_dir)
    if not p or len(p) > 4096 or "\0" in p or "\n" in p or "\r" in p or not os.path.isabs(p):
        raise ServiceError("bad_request", "state_dir must be an absolute path")
    return Path(p)


def ensure_dir(sd, create):
    if create:
        try:
            sd.mkdir(mode=0o700, parents=True, exist_ok=True)
        except OSError as e:
            raise ServiceError("permission", f"cannot create the service state directory ({type(e).__name__})") from None
    if not sd.is_dir():
        return False
    if not IS_WIN:
        st = sd.stat()
        if st.st_uid != os.getuid() or st.st_mode & 0o022:
            raise ServiceError("permission", f"{sd} must be owned by you and not writable by others (chmod 700)")
    return True


def _read_json(p):
    try:
        d = json.loads(Path(p).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError):
        raise ServiceError("internal", f"{Path(p).name} is unreadable; it was left untouched") from None
    return d if isinstance(d, dict) else None


def _write_plain(p, data):  # non-secret status files: owner-only file, atomic replace
    tmp = Path(p).with_name(f".{Path(p).name}.{uuid.uuid4().hex[:8]}")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(data, f)
    os.replace(tmp, p)


def read_config(sd):
    d = _read_json(sd / "service.json")
    if d is None:
        return None
    if set(d) != _CFG or d["version"] != 1:
        raise ServiceError("not_configured", "service.json is invalid; it was left untouched")
    validate(d["address"], d["port"], d["allow_address_join"])
    return d


def lock_busy(sd):
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


# ---- OS definitions ----------------------------------------------------------------------------

def _command(py, root, sd):
    return [str(py), "-I", "-B", "-c", BOOT, str(root), str(sd)]


def _unit_path():
    return Path(os.environ.get("XDG_CONFIG_HOME") or Path.home() / ".config") / "systemd/user" / UNIT


def _unit_text(cmd, sd, owner):
    def q(a):
        return '"' + a.replace("\\", "\\\\").replace('"', '\\"').replace("%", "%%").replace("$", "$$") + '"'
    return (f"# X-CorridorKey-Network-Owner={owner}\n# X-CorridorKey-State-Dir={sd}\n"
            "[Unit]\nDescription=Keyloom network service (powered by CorridorKey)\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n"
            f"[Service]\nType=simple\nExecStart={' '.join(q(a) for a in cmd)}\nWorkingDirectory={str(sd).replace(chr(37), chr(37) * 2)}\n"
            "Restart=on-failure\nRestartSec=10\nRestartPreventExitStatus=2\n\n[Install]\nWantedBy=default.target\n")


def _systemctl(*a):
    r = _run(["systemctl", "--user", *a])
    if r.returncode and a[0] in ("daemon-reload", "enable", "start", "stop", "disable"):
        raise ServiceError("service_unavailable", f"systemctl --user {a[0]} failed: {_out(r) or r.returncode}")
    return r


def _task_xml(cmd, sd, owner, who):
    from xml.sax.saxutils import escape as e
    meta = e(json.dumps({"owner": owner, "state_dir": str(sd)}))
    args = subprocess.list2cmdline(cmd[1:])
    return ('<?xml version="1.0" encoding="UTF-16"?>\n<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">'
            f"<RegistrationInfo><Description>{meta}</Description></RegistrationInfo>"
            f"<Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{e(who)}</UserId></LogonTrigger></Triggers>"
            f'<Principals><Principal id="Author"><UserId>{e(who)}</UserId><LogonType>InteractiveToken</LogonType>'
            "<RunLevel>LeastPrivilege</RunLevel></Principal></Principals>"
            "<Settings><MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy><DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries><StartWhenAvailable>true</StartWhenAvailable><Hidden>true</Hidden>"
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit><RestartOnFailure><Interval>PT1M</Interval><Count>3</Count></RestartOnFailure></Settings>"
            f'<Actions Context="Author"><Exec><Command>{e(cmd[0])}</Command><Arguments>{e(args)}</Arguments>'
            f"<WorkingDirectory>{e(str(sd))}</WorkingDirectory></Exec></Actions></Task>")


def _win_user():
    r = _run(["whoami"])
    if r.returncode or not r.stdout.strip():
        raise ServiceError("service_unavailable", "cannot determine the current Windows user")
    return r.stdout.decode("utf-8", "replace").strip()


def _definition():
    """-> {"mode", "state_dir"} of the per-user definition if one exists (owned or not), else None."""
    if not IS_WIN:
        p = _unit_path()
        if not p.is_file():
            return None
        m = re.search(r"^# X-CorridorKey-State-Dir=(.*)$", p.read_text(encoding="utf-8", errors="replace"), re.M)
        return {"mode": "systemd", "state_dir": m.group(1) if m else None}
    r = _run(["schtasks", "/Query", "/TN", TASK, "/XML"])
    if r.returncode == 0:
        text = r.stdout.decode("utf-16", "replace") if r.stdout[:2] in (b"\xff\xfe", b"\xfe\xff") else r.stdout.decode("utf-8", "replace")
        m = re.search(r"<Description>(.*?)</Description>", text, re.S)
        try:
            return {"mode": "task", "state_dir": json.loads(html.unescape(m.group(1)))["state_dir"]}
        except (AttributeError, ValueError, KeyError, TypeError):
            return {"mode": "task", "state_dir": None}
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_QUERY_VALUE) as k:
            val = str(winreg.QueryValueEx(k, RUN_VALUE)[0])
    except OSError:
        return None
    m = re.search(r'(?:"([^"]*)"|(\S+))\s*$', val)
    return {"mode": "run", "state_dir": (m.group(1) or m.group(2)) if m else None}


def _owned(sd):
    """The definition names `sd` AND `sd` carries our install record: only then may we change or remove it."""
    d = _definition()
    inst = _read_json(sd / "service-install.json") if sd.is_dir() else None
    if d and d["state_dir"] and inst and os.path.normcase(d["state_dir"]) == os.path.normcase(str(sd)):
        return d | {"install": inst}
    return None


def _write_definition(mode, cmd, sd, owner):
    """Create/replace the definition; returns the mode actually used (Windows may fall back from task to run)."""
    if mode == "systemd":
        p = _unit_path()
        p.parent.mkdir(parents=True, exist_ok=True)
        tmp = p.with_name(f".{p.name}.{uuid.uuid4().hex[:8]}")
        tmp.write_text(_unit_text(cmd, sd, owner), encoding="utf-8")
        os.replace(tmp, p)
        _systemctl("daemon-reload")
        _systemctl("enable", UNIT)
        return mode
    if mode == "task":
        xml = sd / f".task.{uuid.uuid4().hex[:8]}.xml"
        xml.write_text(_task_xml(cmd, sd, owner, _win_user()), encoding="utf-16")
        try:
            r = _run(["schtasks", "/Create", "/TN", TASK, "/XML", str(xml), "/F"])
        finally:
            xml.unlink(missing_ok=True)
        if r.returncode == 0:
            _delete_run_value()
            return "task"
        mode = "run"  # standard users may be refused a logon task: equivalent per-user autostart, still no elevation
    import winreg
    with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, RUN_VALUE, 0, winreg.REG_SZ, subprocess.list2cmdline(cmd))
    return "run"


def _delete_run_value():
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
            winreg.DeleteValue(k, RUN_VALUE)
    except OSError:
        pass


def _remove_definition(mode):
    if mode == "systemd":
        _systemctl("disable", UNIT)
        _unit_path().unlink(missing_ok=True)
        _systemctl("daemon-reload")
    else:
        if mode == "task":
            _run(["schtasks", "/Delete", "/TN", TASK, "/F"])
        _delete_run_value()


def _spawn(mode, cmd, sd):
    if mode == "systemd":
        _run(["systemctl", "--user", "reset-failed", UNIT])
        _systemctl("start", UNIT)
    elif mode == "task":
        r = _run(["schtasks", "/Run", "/TN", TASK])
        if r.returncode:
            raise ServiceError("service_unavailable", f"the scheduled task could not be started: {_out(r)}")
    else:
        env = {k: v for k, v in os.environ.items() if not k.startswith("PYTHON") and k != "VIRTUAL_ENV"}
        subprocess.Popen(cmd, cwd=str(sd), env=env, close_fds=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                         stderr=subprocess.DEVNULL, creationflags=0x00000008 | 0x00000200 | 0x08000000)


def _stop(mode, sd, wait=15):
    if not lock_busy(sd):
        return
    (sd / "stop.request").write_text("stop", encoding="utf-8")  # graceful on every platform; the service polls for it
    end = time.monotonic() + wait
    while lock_busy(sd) and time.monotonic() < end:
        time.sleep(0.25)
    if lock_busy(sd):
        _systemctl("stop", UNIT) if mode == "systemd" else _run(["schtasks", "/End", "/TN", TASK]) if mode == "task" else None
        end = time.monotonic() + 10
        while lock_busy(sd) and time.monotonic() < end:
            time.sleep(0.25)
    (sd / "stop.request").unlink(missing_ok=True)
    if lock_busy(sd):
        raise ServiceError("busy", "the service did not stop; close it from Task Manager / systemctl and retry")


def _wait_ready(sd, timeout=12):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        st = _read_json(sd / "service-state.json") or {}
        if st.get("state") == "failed":
            raise ServiceError(st.get("code") or "start_failed", st.get("message") or "the service failed to start")
        if st.get("state") == "running" and lock_busy(sd):
            return
        time.sleep(0.25)
    raise ServiceError("start_timeout", "the service did not report ready in time; check Status and try Start again")


def _begin_start(mode, cmd, sd):
    (sd / "service-state.json").unlink(missing_ok=True)
    (sd / "stop.request").unlink(missing_ok=True)
    _spawn(mode, cmd, sd)
    _wait_ready(sd)


def _port_free(port):
    s = socket.socket()
    try:
        if not IS_WIN:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("0.0.0.0", port))
    except OSError as e:
        if e.errno in (errno.EADDRINUSE, 10048):
            raise ServiceError("port_in_use", f"port {port} is already in use by another program; choose another port") from None
        raise ServiceError("permission", f"port {port} cannot be opened ({type(e).__name__}); it may be reserved or blocked") from None
    finally:
        s.close()


def cert_addresses(pem):
    """Host names / IPs the network certificate covers (what clients may be told to connect to)."""
    from backend.coordinator import pki
    san = pki.x509.load_pem_x509_certificate(pem.encode()).extensions.get_extension_for_class(pki.x509.SubjectAlternativeName).value
    return [str(a) for a in san.get_values_for_type(pki.x509.IPAddress)] + list(san.get_values_for_type(pki.x509.DNSName))


def _cert_ok(sd, address):
    """Keep existing networks, including older certificates without loopback names."""
    from backend.coordinator import pki
    cert, key = sd / "server-cert.pem", sd / "server-key.pem"
    if cert.exists() != key.exists():
        raise ServiceError("cert_mismatch", "the certificate and key files are inconsistent; they were not regenerated")
    if cert.exists():
        pem = cert.read_text(encoding="utf-8")
        if not pki.cert_covers_host(pem, address):
            usable = [a for a in cert_addresses(pem) if a.lower() not in LOCALHOSTS]
            raise ServiceError("cert_mismatch", f"the existing network certificate does not cover {address}; it was not regenerated "
                               "because PCs already joined trust it. Host on one of its covered addresses"
                               + (f": {', '.join(usable)}." if usable else "."))


# ---- public operations ---------------------------------------------------------------------------

def status(state_dir):
    sd = state_path(state_dir)
    exists = ensure_dir(sd, False)
    cfg = read_config(sd) if exists else None
    d = _owned(sd) if exists else None
    out = {"installed": bool(d), "running": exists and lock_busy(sd), "address": cfg and cfg["address"], "port": cfg and cfg["port"],
           "allow_address_join": cfg and cfg["allow_address_join"], "message": ""}
    if cfg:
        out["url"] = f"https://{cfg['address']}:{cfg['port']}"
    if d:
        out["mode"] = d["mode"]
    cert = sd / "server-cert.pem"
    if cert.is_file():
        from backend.coordinator import pki
        out["fingerprint_sha256"] = pki.fingerprint(cert.read_text(encoding="utf-8"))
        out["network_id"] = pki.network_id(out["fingerprint_sha256"])
        out["certificate_addresses"] = [a for a in cert_addresses(cert.read_text(encoding="utf-8")) if a.lower() not in LOCALHOSTS]
    st = (_read_json(sd / "service-state.json") or {}) if exists else {}
    if st.get("state") == "failed" and not out["running"]:
        out["message"], out["error_code"] = st.get("message", ""), st.get("code", "start_failed")
    elif out["running"]:
        out["message"] = "running"
    elif d:
        out["message"] = "installed, not running"
    else:
        out["message"] = "not installed"
    if not d and _definition():
        out["message"] = "a service for a different state folder is registered for this user"
    return out


def install(state_dir, address, port, allow):
    validate(address, port, allow)
    sd = state_path(state_dir)
    ensure_dir(sd, True)
    root, owner, prefix = install_context()
    py = service_python(sys.executable)
    existing = _definition()
    if existing and (not existing["state_dir"] or os.path.normcase(existing["state_dir"]) != os.path.normcase(str(sd))):
        raise ServiceError("service_conflict", "a different network service is already registered for this user; remove it first")
    _cert_ok(sd, address)
    cur = _owned(sd)
    mode = cur["mode"] if cur else ("systemd" if not IS_WIN else "task")
    if lock_busy(sd):
        _stop(mode, sd)
    _port_free(port)
    from backend.coordinator import pki
    cfg_path = sd / "service.json"
    tmp = sd / f".service.{uuid.uuid4().hex[:8]}.tmp"
    pki.write_private_new(str(tmp), json.dumps({"version": 1, "address": address, "port": port, "allow_address_join": allow}))
    os.replace(tmp, cfg_path)
    cmd = _command(py, root, sd)
    mode = _write_definition(mode, cmd, sd, owner)
    _write_plain(sd / "service-install.json", {"version": 1, "owner": owner, "mode": mode, "root": str(root), "python": str(py)})
    if prefix:
        _write_plain(prefix / REGISTRY, {"version": 1, "state_dir": str(sd)})
    _begin_start(mode, cmd, sd)
    return status(sd)


def _owned_or_raise(sd):
    ensure_dir(sd, False)
    d = _owned(sd)
    if not d:
        raise ServiceError("not_installed", "the network service is not installed for this state folder")
    return d


def start(state_dir):
    sd = state_path(state_dir)
    d = _owned_or_raise(sd)
    if not lock_busy(sd):
        i = d["install"]
        _begin_start(d["mode"], _command(i["python"], i["root"], sd), sd)
    return status(sd)


def stop(state_dir):
    sd = state_path(state_dir)
    d = _owned_or_raise(sd)
    _stop(d["mode"], sd)
    return status(sd)


def remove(state_dir):
    sd = state_path(state_dir)
    ensure_dir(sd, False)
    d = _owned(sd)
    if d:
        _stop(d["mode"], sd)
        _remove_definition(d["mode"])
        (sd / "service-install.json").unlink(missing_ok=True)
        reg = Path(d["install"].get("root", "")).parents[2] / REGISTRY if d["install"].get("root") else None
        if reg and reg.is_file() and (_read_json(reg) or {}).get("state_dir") == str(sd):
            reg.unlink()
    return status(sd)  # state, certificate and database stay: removing the service never deletes the network


def refresh(python, root, prefix):
    """Activation/rollback hook (lifecycle): repoint an OWNED service at the newly active version. Restarts it only if it ran."""
    reg = _read_json(Path(prefix) / REGISTRY)
    if not reg:
        return None
    sd = state_path(reg["state_dir"])
    d = _owned(sd)
    if not d or d["install"].get("owner") != owner_id(prefix):
        return None
    py, was = service_python(python), lock_busy(sd)
    if was:
        _stop(d["mode"], sd)
    cmd = _command(py, root, sd)
    mode = _write_definition(d["mode"], cmd, sd, d["install"]["owner"])
    _write_plain(sd / "service-install.json", d["install"] | {"mode": mode, "root": str(root), "python": str(py)})
    if was:
        _begin_start(mode, cmd, sd)
    return "network service now runs from the active version"


def remove_owned(prefix):
    """Uninstall hook: stop and unregister the service this prefix owns; state and database are left alone."""
    reg = _read_json(Path(prefix) / REGISTRY)
    if not reg:
        return None
    sd = state_path(reg["state_dir"])
    d = _owned(sd)
    if d and d["install"].get("owner") == owner_id(prefix):
        _stop(d["mode"], sd)
        _remove_definition(d["mode"])
        (sd / "service-install.json").unlink(missing_ok=True)
    (Path(prefix) / REGISTRY).unlink(missing_ok=True)
    return "network service removed"


# ---- the service process ---------------------------------------------------------------------------

def _publish(sd, state, code="", message="", **extra):
    _write_plain(sd / "service-state.json", {"state": state, "code": code, "message": message[:300], "pid": os.getpid(),
                                             "time": time.time(), **extra})


def _serve(sd):
    import secrets
    import signal
    import threading
    from backend.coordinator import pki
    from backend.coordinator.server import CoordinatorServer
    from backend.coordinator.store import Store
    cfg = read_config(sd)
    if not cfg:
        raise ServiceError("not_configured", "service.json is missing")
    ensure_dir(sd, False)
    # Status probes briefly take this lock; distinguish them from an already running service.
    until = time.monotonic() + 2
    while True:
        try:
            lock = pki.OwnerLock(str(sd / "coordinator.lock"), "network service")
            break
        except pki.OwnerBusy:
            if time.monotonic() >= until:
                raise
            time.sleep(0.05)
    store = server = None
    try:
        _cert_ok(sd, cfg["address"])
        cert, key = sd / "server-cert.pem", sd / "server-key.pem"
        if not cert.exists():
            cert_pem, key_pem = pki.generate_identity(list(dict.fromkeys((cfg["address"], *LOCALHOSTS))), days=3650)
            pki.write_private_new(str(key), key_pem)
            pki.write_private_new(str(cert), cert_pem)
        pki.check_private(str(key))
        pem = cert.read_text(encoding="utf-8")
        fp = pki.fingerprint(pem)
        token = secrets.token_urlsafe(32)
        tok = sd / "local-join.token"
        tmp = sd / f".token.{uuid.uuid4().hex[:8]}"
        pki.write_private_new(str(tmp), token)
        os.replace(tmp, tok)
        store = Store(str(sd / "coordinator.sqlite3"))
        try:
            server = CoordinatorServer(("0.0.0.0", cfg["port"]), pki.server_context(str(cert), str(key)), store,
                                       join_enabled=cfg["allow_address_join"], local_token=token,
                                       advertise_url=f"https://{cfg['address']}:{cfg['port']}")
        except OSError as e:
            if e.errno in (errno.EADDRINUSE, 10048):
                raise ServiceError("port_in_use", f"port {cfg['port']} is already in use") from None
            raise ServiceError("permission", f"cannot listen on port {cfg['port']} ({type(e).__name__})") from None
        stopper = threading.Thread(target=server.shutdown, daemon=True)
        once = threading.Lock()

        def stop(*_):
            if once.acquire(blocking=False):
                stopper.start()

        for name in ("SIGTERM", "SIGINT", "SIGBREAK"):
            if hasattr(signal, name):
                signal.signal(getattr(signal, name), stop)

        def watch():
            while not (sd / "stop.request").exists():
                time.sleep(1)
            stop()
        threading.Thread(target=watch, daemon=True).start()
        _publish(sd, "running", address=cfg["address"], port=cfg["port"], fingerprint_sha256=fp, network_id=pki.network_id(fp))
        server.serve_forever()
        _publish(sd, "stopped")
    finally:
        if server:
            server.server_close()
        if store:
            store.close()
        for n in ("local-join.token", "stop.request"):
            (sd / n).unlink(missing_ok=True)
        lock.close()


def main(argv=None):
    sd = state_path(argv[0])
    try:
        _serve(sd)
        return 0
    except Exception as e:
        from backend.coordinator import pki
        if isinstance(e, pki.OwnerBusy):
            return 0  # another instance owns this state; never overwrite its status
        if isinstance(e, ServiceError):
            code, msg, rc = e.code, e.message, 2  # exit 2: a configuration problem, systemd must not restart-loop
        elif isinstance(e, pki.SecurityError):
            code, msg, rc = "cert_mismatch", str(e), 2
        else:
            code, msg, rc = "start_failed", type(e).__name__, 1
        try:
            _publish(sd, "failed", code, msg)
        except OSError:
            pass
        return rc
