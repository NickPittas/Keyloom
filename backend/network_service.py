"""Per-user network service: the coordinator hosted by an installed app, started at user sign-in (no admin, no console).

Two halves in one file. The top half is stdlib-only OS management (also imported by packaging/lifecycle.py from a file path,
so nothing here may import the app at module level). The bottom half is the service process (`main`).

Process:  <python> -I -B -c BOOT <app root> <state dir>      (-I: no PYTHON* variables, no user site, no cwd on sys.path;
                                                              the app root is put on sys.path explicitly by BOOT)
  Linux    systemd --user unit  keyloom-network.service     Windows  Task Scheduler logon task "Keyloom Network Service"
  (pythonw, LeastPrivilege, current user only); if the task cannot be created without elevation, an owned HKCU Run value
  "KeyloomNetworkService" + a detached pythonw start is used instead. Ownership of the definition is proven by the
  state directory it names and by `service-install.json` inside that directory; anything else is never touched.

State dir (private, created 0700): service.json (config; strict v1 {version,address,port,allow_address_join}: `address` is only
cosmetic now, the certificate pin is the identity, and the format is kept so an older build can still read it), service-meta.json
(PC name), service-install.json, service-state.json (pid/state/endpoints/discovery/receiver, no secrets), coordinator.lock (held
while running), coordinator.sqlite3, server-cert.pem/server-key.pem (never regenerated), local-join.token (rewritten at every
start, loopback-only join), local-installation.json (which installation is THIS PC), host-removed.marker (user chose Remove:
nothing re-installs the service until Start), stop.request (graceful-stop sentinel).
The service listens dual-stack on one of ports 8443..8452 (existing port kept) on all interfaces, and answers UDP discovery on the
fixed port 44554. Errors are ServiceError(code, message) with codes:
bad_request, permission, port_in_use, service_unavailable, service_conflict, not_installed, not_configured, cert_mismatch,
start_failed, start_timeout, busy, internal."""
import contextlib
import errno
import hashlib
import html
import json
import os
import re
import signal
import socket
import subprocess
import sys
import time
import uuid
from pathlib import Path

IS_WIN = os.name == "nt"
UNIT = "keyloom-network.service"
TASK = "Keyloom Network Service"
RUN_VALUE = "KeyloomNetworkService"
LEGACY_NAMES = ("corridorkey-network.service", "CorridorKey Network Service", "CorridorKeyNetworkService")  # before the rename
RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"
REGISTRY = "network-service.json"  # in the install prefix: tells lifecycle which state dir its service uses
BOOT = "import sys;sys.path.insert(0,sys.argv[1]);from backend.network_service import main;sys.exit(main(sys.argv[2:]))"
LOCALHOSTS = ("localhost", "127.0.0.1", "::1")
_CFG = {"version", "address", "port", "allow_address_join"}
META = "service-meta.json"
REMOVED = "host-removed.marker"
LOCAL_ID = "local-installation.json"
PORTS = tuple(range(8443, 8453))
_IID = re.compile(r"[A-Za-z0-9_-]{1,80}\Z")
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
    if root.name == "app" and ((root.parent / "keyloom-install.json").is_file() or (root.parent / "corridorkey-install.json").is_file()):
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
            or address in ("0.0.0.0", "::")):
        raise ServiceError("bad_request", "address must be a host name or IP address (not a wildcard)")
    import ipaddress
    try:
        ip = ipaddress.ip_address(address)
        if ip.version != 4:
            raise ServiceError("bad_request", "hosting uses IPv4; enter this PC’s LAN or Tailscale IPv4 address")
        if ip.is_unspecified:
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
    """Absent -> None. A reader can collide with the atomic replace done by the service or the installer (Windows reports a
    sharing violation, or a half-visible file): retry briefly before calling it unreadable, and name the real cause."""
    last = None
    for attempt in range(8):
        try:
            d = json.loads(Path(p).read_text(encoding="utf-8-sig"))
            return d if isinstance(d, dict) else None
        except FileNotFoundError:
            return None
        except (OSError, ValueError) as e:
            last = e
            time.sleep(0.05 * (attempt + 1))
    raise ServiceError("internal", f"{Path(p).name} could not be read ({type(last).__name__}); it was left untouched") from None


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
    return (f"# X-Keyloom-Network-Owner={owner}\n# X-Keyloom-State-Dir={sd}\n"
            "[Unit]\nDescription=Keyloom network service (powered by CorridorKey)\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n"
            f"[Service]\nType=simple\nExecStart={' '.join(q(a) for a in cmd)}\nWorkingDirectory={str(sd).replace(chr(37), chr(37) * 2)}\n"
            "Restart=on-failure\nRestartSec=10\nRestartPreventExitStatus=2\n\n[Install]\nWantedBy=default.target\n")


@contextlib.contextmanager
def _legacy_names():
    """Run the OS-definition helpers against the pre-rename unit/task/value names."""
    global UNIT, TASK, RUN_VALUE
    saved = UNIT, TASK, RUN_VALUE
    UNIT, TASK, RUN_VALUE = LEGACY_NAMES
    try:
        yield
    finally:
        UNIT, TASK, RUN_VALUE = saved


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
        m = re.search(r"^# X-(?:Keyloom|CorridorKey)-State-Dir=(.*)$", p.read_text(encoding="utf-8", errors="replace"), re.M)
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


def _same_dir(a, b):
    """Same folder, also through the compatibility link the settings-folder rename leaves under the old name."""
    try:
        return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(b))
    except OSError:
        return os.path.normcase(str(a)) == os.path.normcase(str(b))


def _owned(sd):
    """The definition names `sd` AND `sd` carries our install record: only then may we change or remove it."""
    d = _definition()
    inst = _read_json(sd / "service-install.json") if sd.is_dir() else None
    if d and d["state_dir"] and inst and _same_dir(d["state_dir"], sd):
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


def _retire_legacy(sd):
    """Stop and unregister a pre-rename (CorridorKey) definition that serves THIS state folder. Only the definition goes;
    the state folder, certificate, database and settings are never touched. True if one was found."""
    with _legacy_names():
        d = _definition()
        if not d or not d["state_dir"] or not _same_dir(d["state_dir"], sd):
            return False
        try:
            _stop(d["mode"], sd, fallback=False)
        except ServiceError:
            pass  # the caller falls back to _kill_holder
        try:
            _remove_definition(d["mode"])
        except ServiceError:
            pass
    return True


def _kill_holder(sd):
    """Last resort for a service that ignores stop.request and has no definition we can stop: end the pid its state file names,
    but only if that process's command line mentions this state folder (guards against pid reuse)."""
    pid = (_read_json(sd / "service-state.json") or {}).get("pid")
    if not isinstance(pid, int) or pid <= 0:
        return
    if IS_WIN:
        r = _run(["powershell", "-NoProfile", "-Command", f"(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"])
        cmdline = r.stdout.decode("utf-8", "replace")
    else:
        try:
            cmdline = Path(f"/proc/{pid}/cmdline").read_bytes().decode("utf-8", "replace").replace("\0", " ")
        except OSError:
            return
    if str(sd) not in cmdline:
        return
    if IS_WIN:
        _run(["taskkill", "/PID", str(pid), "/F"])
    else:
        with contextlib.suppress(OSError):
            os.kill(pid, signal.SIGTERM)
    end = time.monotonic() + 10
    while lock_busy(sd) and time.monotonic() < end:
        time.sleep(0.25)


def _stop(mode, sd, wait=15, fallback=True):
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
    if fallback and lock_busy(sd):  # a pre-rename service may hold the lock under the old names
        _retire_legacy(sd)
        if lock_busy(sd):
            _kill_holder(sd)
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


def _port_ok(port):
    s = socket.socket()
    try:
        if not IS_WIN:
            s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        s.bind(("0.0.0.0", port))
        return True
    except OSError:
        return False
    finally:
        s.close()


def _port_free(port):  # an explicitly requested port (old arguments) must be free
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


def _pick_port(preferred=None):
    """The existing port when it is free, else the first free one of 8443..8452."""
    for p in dict.fromkeys(([preferred] if preferred else []) + list(PORTS)):
        if _port_ok(p):
            return p
    raise ServiceError("port_in_use", "ports 8443-8452 are all in use by other programs; close one and press Start again")


def cert_addresses(pem):
    """Host names / IPs the network certificate covers (what clients may be told to connect to)."""
    from backend.coordinator import pki
    san = pki.x509.load_pem_x509_certificate(pem.encode()).extensions.get_extension_for_class(pki.x509.SubjectAlternativeName).value
    return [str(a) for a in san.get_values_for_type(pki.x509.IPAddress)] + list(san.get_values_for_type(pki.x509.DNSName))


def _cert_ok(sd):
    """The certificate and key must exist together. Addresses are not checked: the pin is the identity, the SAN is cosmetic,
    and an existing certificate is never regenerated (joined PCs pin it)."""
    if (sd / "server-cert.pem").exists() != (sd / "server-key.pem").exists():
        raise ServiceError("cert_mismatch", "the certificate and key files are inconsistent; they were not regenerated")


def _default_name():
    n = "".join(c for c in socket.gethostname() if c.isprintable()).strip()[:64]
    return n or "This PC"


def read_name(sd):
    try:
        d = _read_json(sd / META)
    except ServiceError:
        d = None
    n = d.get("name") if d else None
    return n if isinstance(n, str) and 1 <= len(n) <= 64 and n.isprintable() else _default_name()


def _write_meta(sd, name):
    if not isinstance(name, str) or len(name.strip()) > 64 or not name.strip().isprintable() and name.strip():
        raise ServiceError("bad_request", "name must be 1-64 printable characters")
    if name.strip():
        _write_plain(sd / META, {"version": 1, "name": name.strip()})
    else:
        (sd / META).unlink(missing_ok=True)  # empty = this PC's host name


def read_local_id(sd):
    try:
        d = _read_json(sd / LOCAL_ID)
    except ServiceError:
        return None
    v = d.get("installation_id") if d else None
    return v if isinstance(v, str) and _IID.match(v) else None


def _write_cfg(sd, address, port, allow):
    from backend.coordinator import pki
    tmp = sd / f".service.{uuid.uuid4().hex[:8]}.tmp"
    pki.write_private_new(str(tmp), json.dumps({"version": 1, "address": address, "port": port, "allow_address_join": allow}))
    os.replace(tmp, sd / "service.json")


def _v4(a):
    import ipaddress
    try:
        ip = ipaddress.ip_address(a)
        return ip.version == 4 and not ip.is_loopback and not ip.is_unspecified
    except ValueError:
        return False


def _choose_address(sd):
    """Cosmetic v1 `address` for a new config: a certificate-covered local IPv4 (so an older build reading this file works), else
    127.0.0.1. Never asked from the user."""
    live = []
    try:
        from backend import discovery
        live = [a for _, a, _, _ in discovery.interfaces() if _v4(a)]
    except Exception:
        pass
    cert = sd / "server-cert.pem"
    if cert.is_file():
        try:
            cov = [a for a in cert_addresses(cert.read_text(encoding="utf-8")) if _v4(a)]
        except Exception:
            cov = []
        return next((a for a in cov if a in live), cov[0] if cov else "127.0.0.1")
    return live[0] if live else "127.0.0.1"


# ---- public operations ---------------------------------------------------------------------------

def _live_endpoints(port):
    try:
        from backend import discovery
        return discovery.local_endpoints(port)
    except Exception:
        return []


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
    live = out["running"] and st.get("state") == "running"
    out["name"] = read_name(sd) if exists else _default_name()
    out["host_installation_id"] = read_local_id(sd) if exists else None
    out["removed"] = exists and (sd / REMOVED).exists()
    eps = st.get("endpoints") if live else None
    out["endpoints"] = [e for e in eps if isinstance(e, dict) and isinstance(e.get("url"), str) and isinstance(e.get("kind"), str)][:16] \
        if isinstance(eps, list) else (_live_endpoints(cfg["port"]) if cfg else [])
    out["discovery"] = bool(live and st.get("discovery"))
    out["receiver_online"] = bool(live and st.get("receiver_online"))
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
    diag = (st.get("message") if st.get("state") == "failed" else "") or (st.get("diagnostic") if live else "") or ""
    out["diagnostic"] = str(diag)[:300]  # last error only; the service never puts secrets in it
    return out


def install(state_dir, address=None, port=None, allow=None, name=None):
    """New callers pass only state_dir (+ allow_address_join, name): the host listens on every interface, the port is the existing
    one or the first free of 8443..8452, and `address` is a cosmetic value chosen here. Old callers may still pass address/port."""
    root, owner, prefix = install_context()
    return _install(state_path(state_dir), service_python(sys.executable), root, owner, prefix, address, port, allow, name)


def _install(sd, py, root, owner, prefix, address=None, port=None, allow=None, name=None):
    if allow is not None and not isinstance(allow, bool):
        raise ServiceError("bad_request", "allow_address_join must be true or false")
    ensure_dir(sd, True)
    _retire_legacy(sd)  # take over a pre-rename service of this state folder
    existing = _definition()
    if existing and (not existing["state_dir"] or os.path.normcase(existing["state_dir"]) != os.path.normcase(str(sd))):
        raise ServiceError("service_conflict", "a different network service is already registered for this user; remove it first")
    _cert_ok(sd)
    cur = _owned(sd)
    mode = cur["mode"] if cur else ("systemd" if not IS_WIN else "task")
    cfg = read_config(sd)
    if name is not None:
        _write_meta(sd, name)
    if lock_busy(sd):
        _stop(mode, sd)
    allow = allow if allow is not None else (cfg["allow_address_join"] if cfg else True)
    address = address if address is not None else (cfg["address"] if cfg else _choose_address(sd))
    if port is not None:
        validate(address, port, allow)
        _port_free(port)
    else:
        port = _pick_port(cfg["port"] if cfg else None)
    validate(address, port, allow)
    (sd / REMOVED).unlink(missing_ok=True)
    _write_cfg(sd, address, port, allow)
    cmd = _command(py, root, sd)
    mode = _write_definition(mode, cmd, sd, owner)
    _write_plain(sd / "service-install.json", {"version": 1, "owner": owner, "mode": mode, "root": str(root), "python": str(py)})
    if prefix:
        _write_plain(Path(prefix) / REGISTRY, {"version": 1, "state_dir": str(sd)})
    _begin_start(mode, cmd, sd)
    return status(sd)


def _owned_or_raise(sd):
    ensure_dir(sd, False)
    d = _owned(sd)
    if not d:
        raise ServiceError("not_installed", "the network service is not installed for this state folder")
    return d


def start(state_dir):
    """Start; installs first when nothing is installed (Remove then Start brings the same network back)."""
    sd = state_path(state_dir)
    ensure_dir(sd, False)
    d = _owned(sd) if sd.is_dir() else None
    if not d or not (sd / "service.json").exists():
        return install(state_dir)
    (sd / REMOVED).unlink(missing_ok=True)
    if not lock_busy(sd):
        cfg = read_config(sd)
        if not _port_ok(cfg["port"]):  # another program took our port: move to the next free one (PCs follow by discovery)
            _write_cfg(sd, cfg["address"], _pick_port(), cfg["allow_address_join"])
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
    if sd.is_dir():
        _write_plain(sd / REMOVED, {"version": 1, "time": time.time()})  # tombstone: installers/startup must not bring it back
    return status(sd)  # state, certificate and database stay: removing the service never deletes the network


def configure(state_dir, allow, name=None):
    """Change policy / PC name of an existing configuration. The running service notices within ~1 s; nothing restarts."""
    sd = state_path(state_dir)
    ensure_dir(sd, False)
    cfg = read_config(sd) if sd.is_dir() else None
    if not cfg:
        raise ServiceError("not_configured", "the network service has no configuration yet; press Start")
    if not isinstance(allow, bool):
        raise ServiceError("bad_request", "allow_address_join must be true or false")
    if name is not None:
        _write_meta(sd, name)
    if allow != cfg["allow_address_join"]:
        _write_cfg(sd, cfg["address"], cfg["port"], allow)
    return status(sd)


def _refresh_owned(sd, d, python, root, owner=None, restart=False):
    py, was = service_python(python), lock_busy(sd)
    if was:
        _stop(d["mode"], sd)
    owner = owner or d["install"]["owner"]
    cmd = _command(py, root, sd)
    mode = _write_definition(d["mode"], cmd, sd, owner)
    _write_plain(sd / "service-install.json", d["install"] | {"owner": owner, "mode": mode, "root": str(root), "python": str(py)})
    if was or restart:
        _begin_start(mode, cmd, sd)


def refresh(python, root, prefix):
    """Activation/rollback hook (lifecycle): repoint an OWNED service at the newly active version. Restarts it only if it ran."""
    reg = _read_json(Path(prefix) / REGISTRY)
    if not reg:
        return None
    sd = state_path(reg["state_dir"])
    d = _owned(sd)
    if not d or d["install"].get("owner") != owner_id(prefix):
        return None
    _refresh_owned(sd, d, python, root)
    return "network service now runs from the active version"


def settings_dir(base):
    """`<base>/keyloom`, unless only the pre-rename `<base>/corridorkey-native` exists: existing settings, certificates and
    jobs stay where they are and the same folder (and instance lock) is shared by old and new builds. Mirrors rust-ui settings.rs."""
    old = Path(base) / "corridorkey-native"
    return old if old.is_dir() and not (Path(base) / "keyloom").exists() else Path(base) / "keyloom"


def _migrate_legacy(python, root, owner):
    """Replace a pre-rename definition this installation owns with one under the Keyloom names (stop old, write new, remove
    old). The state directory, certificate, database and service identity are untouched. Foreign definitions are left alone."""
    with _legacy_names():
        d = _definition()
    if not d or not d["state_dir"]:
        return None
    sd = Path(d["state_dir"])
    inst = _read_json(sd / "service-install.json") if sd.is_dir() else None
    if not inst:  # same user's per-user definition: the owner id of an older install location no longer blocks the takeover
        return None
    was = lock_busy(sd)
    if was:
        with _legacy_names():
            _stop(d["mode"], sd)
    cmd = _command(service_python(python), root, sd)
    mode = _write_definition(d["mode"], cmd, sd, owner)
    with _legacy_names():
        try:
            _remove_definition(d["mode"])
        except ServiceError:
            pass
    _write_plain(sd / "service-install.json", inst | {"owner": owner, "mode": mode, "root": str(root), "python": str(service_python(python))})
    if was:
        _begin_start(mode, cmd, sd)
    return "network service moved to the Keyloom name"


def _settings_base():
    return (os.environ.get("APPDATA") or os.environ.get("LOCALAPPDATA")) if IS_WIN else \
        (os.environ.get("XDG_CONFIG_HOME") or (str(Path(os.environ["HOME"]) / ".config") if os.environ.get("HOME") else None))


def _exclusive_lock(path):
    """Linux: take the same advisory lock the app holds while it runs; None if it is busy. Windows: the rename itself fails
    while the app has files open, so no separate lock is needed."""
    if IS_WIN:
        return True
    import fcntl
    f = open(path, "a")
    try:
        fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError:
        f.close()
        return None
    return f


def _link_dir(link, target):
    if IS_WIN:
        r = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(target)], capture_output=True, timeout=30)
        return r.returncode == 0
    os.symlink(target, link)
    return True


def migrate_settings_dir(base, owners=()):
    """The settings/state folder `corridorkey-native` is NOT renamed any more: no new rename is started on any platform.
    Why: a rename leaves a gap (rename -> compatibility link) in which an older build, which only knows the old name, can start
    and create a second, empty folder; holding the app's flock does not prevent that, and Windows has no lock shared with the
    app at all. Every resolver (rust-ui settings.rs config_dir, this module's settings_dir, online) uses `corridorkey-native`
    whenever it is a directory and `keyloom` does not exist, so settings, certificates, jobs and the instance lock simply stay
    where they are. Only the cosmetic branding of that folder name is deferred.
    What still runs: an INTERRUPTED rename left by an earlier version is completed or undone (see _finish_migration), under the
    app's own flock, without ever replacing or deleting a folder.
    Returns None (nothing to say), {"deferred": True, "message"} for the deferral notice, {"pending": True, "message"} when an
    unfinished migration must wait (app running / Windows), or {"blocked": True, "message"} when two folders or an unreadable
    journal make the state ambiguous (the caller must refuse to activate or change services until the user resolves it)."""
    base = Path(base)
    old, new, journal = base / "corridorkey-native", base / "keyloom", base / ".keyloom-migrate.json"
    if os.path.lexists(journal):  # an interrupted run: complete it or undo it BEFORE anything else looks at the folders
        blocked = _finish_migration(old, new, journal)
        if blocked:
            return blocked
    if old.is_dir() and not old.is_symlink() and not os.path.lexists(new):
        return {"moved": False, "deferred": True, "was_running": False,
                "message": "settings folder name unchanged: Keyloom keeps using the existing corridorkey-native folder "
                           "(renaming it to keyloom is deferred; nothing was moved)"}
    return None
def _rename_noreplace(src, dst):
    """Rename a directory only if `dst` does not exist (Linux renameat2 + RENAME_NOREPLACE through stdlib ctypes). True: done.
    False: `dst` exists (EEXIST/ENOTEMPTY), nothing changed. None: not available here (other OS, old kernel/filesystem): the
    caller must NOT fall back to a plain rename, which would silently replace an empty directory."""
    if not sys.platform.startswith("linux"):
        return None
    import ctypes
    try:
        fn = ctypes.CDLL(None, use_errno=True).renameat2
    except (OSError, AttributeError):
        return None
    fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
    if fn(-100, os.fsencode(src), -100, os.fsencode(dst), 1) == 0:  # AT_FDCWD, RENAME_NOREPLACE
        return True
    e = ctypes.get_errno()
    return False if e in (errno.EEXIST, errno.ENOTEMPTY) else None


def _undo_rename(old, new):
    """Put the renamed folder back under its original name, never replacing anything: True only on a completed no-replace rename."""
    if os.path.lexists(old) or not new.is_dir() or new.is_symlink():
        return False
    return _rename_noreplace(new, old) is True


def _finish_migration(old, new, journal):
    """An interrupted rename (a timeout can stop the run between the rename and the link): reach a consistent state before
    anything else, deterministically, deleting nothing.
      original folder only            -> the rename never happened: drop the journal, migrate normally
      keyloom only (rename done)      -> create the link; if that fails undo the rename; then rewrite paths, drop the journal
      keyloom + link under old name   -> rewrite paths (idempotent), drop the journal
      anything else (two real folders, foreign journal, unreadable journal, link impossible AND undo impossible)
                                      -> journal and both folders untouched, returns {"blocked": True, "message"}; the caller
                                         refuses to start or change services until the user resolves it.
    Returns None when consistent."""
    kind, msg = _migration_state(old, new, journal)
    if kind == "blocked":
        return _blocked(msg)
    if kind == "drop":  # journal only: the rename never happened, no folder is touched
        journal.unlink(missing_ok=True)
        return None
    if IS_WIN:  # no lock shared with the app's startup: completing (link, path rewrite) waits for a non-Windows run or the user
        return {"moved": False, "pending": True, "was_running": False,
                "message": "an earlier settings-folder rename is unfinished; it keeps working through the resolver and is "
                           "completed when it can be done safely"}
    lock = _exclusive_lock(new / "instance.lock")  # the app's own flock, BEFORE any link / undo / path rewrite
    if lock is None:
        return {"moved": False, "pending": True, "was_running": False,
                "message": "an earlier settings-folder rename is unfinished; it is completed when Keyloom is not running"}
    try:
        kind, msg = _migration_state(old, new, journal)  # decided again under the lock
        if kind == "blocked":
            return _blocked(msg)
        if kind == "drop":
            journal.unlink(missing_ok=True)
            return None
        if kind == "link":
            try:
                linked = _link_dir(old, new)  # os.symlink never replaces an existing name
            except OSError:
                linked = False
            if not linked:
                if _undo_rename(old, new):  # no-replace rename back: the original, usable folder again
                    _fix_paths(old, new, old)  # an earlier version rewrote paths before linking: point them back
                    journal.unlink(missing_ok=True)
                    return None
                return _blocked("no link could be created under the old name and the rename could not be undone without "
                                "risking another folder; nothing was deleted or replaced")
        if _fix_paths(new, old, new):
            journal.unlink(missing_ok=True)  # cleared only after the link and every path rewrite succeeded
    finally:
        if lock is not True:
            lock.close()
    return None


def _blocked(msg):
    return {"moved": False, "blocked": True, "was_running": False,
            "message": "settings folder migration is unfinished and ambiguous: " + msg + ". Nothing was deleted or replaced. "
                       "Keyloom setup will not activate or change services until it is resolved: keep the folder with your "
                       "settings as 'corridorkey-native' (move the other aside) and run setup again"}


def _migration_state(old, new, journal):
    """Read-only classification of an interrupted rename -> (kind, message), kind one of:
    drop (journal only: rename never happened) | link (keyloom exists, no old name) | fix (old name is the link; paths pending) |
    blocked (two real folders, foreign/unreadable journal, old name not a link to keyloom, keyloom missing)."""
    try:
        j = _read_json(journal)
    except ServiceError:
        return "blocked", "its journal cannot be read"
    if not j or Path(j.get("from", "")) != old or Path(j.get("to", "")) != new:
        return "blocked", "its journal does not name these folders"
    old_real = old.is_dir() and not old.is_symlink()
    new_real = new.is_dir() and not new.is_symlink()
    if old_real and not os.path.lexists(new):
        return "drop", ""
    if old_real and new_real:
        return "blocked", "both a corridorkey-native and a keyloom folder exist"
    if not new_real:
        return "blocked", "the keyloom entry is missing or is not a plain folder"
    if not os.path.lexists(old):
        return "link", ""
    if old.is_symlink() and _same_dir(old, new):
        return "fix", ""
    return "blocked", "the old name exists but is not a link to the keyloom folder"


def settings_migration_state(base=None):
    """Read-only (no lock, no write): {"blocked": bool, "message": str} for callers that must refuse to activate or change services
    while an interrupted settings-folder rename is ambiguous. An unfinished but unambiguous one is not blocked (resolvers cope)."""
    base = base or _settings_base()
    if not base:
        return {"blocked": False, "message": ""}
    base = Path(base)
    journal = base / ".keyloom-migrate.json"
    if not os.path.lexists(journal):
        return {"blocked": False, "message": ""}
    kind, msg = _migration_state(base / "corridorkey-native", base / "keyloom", journal)
    return {"blocked": kind == "blocked", "message": _blocked(msg)["message"] if kind == "blocked" else ""}


def _fix_paths(new, old, new_path):
    """Rewrite the old folder path inside settings.json textually (JSON-escaped form), keeping every other byte."""
    f = Path(new_path) / "settings.json"
    try:
        text = f.read_text(encoding="utf-8")
    except FileNotFoundError:
        return True  # nothing to rewrite
    except OSError:
        return False  # retried on the next run (the journal stays)
    a, b = json.dumps(str(old))[1:-1], json.dumps(str(new_path))[1:-1]
    if a in text:
        tmp = f.with_name(f".settings.{uuid.uuid4().hex[:8]}")
        try:
            tmp.write_text(text.replace(a, b), encoding="utf-8")
            os.replace(tmp, f)
        except OSError:
            tmp.unlink(missing_ok=True)
            return False
    return True  # idempotent: the new path text never contains the old one


def _user_settings():
    """(settings["network"] dict | None if unreadable, state dir | None) derived exactly like rust-ui (settings.rs / network_setup.rs)."""
    base = _settings_base()
    if not base:
        return None, None
    sp = settings_dir(Path(base)) / "settings.json"
    net = {}
    try:
        d = json.loads(sp.read_text(encoding="utf-8"))
        net = d.get("network") if isinstance(d, dict) and isinstance(d.get("network"), dict) else {}
    except FileNotFoundError:
        pass
    except (OSError, ValueError):
        net = None
    return net, sp.parent / "network" / "server"


def ensure_default(python, root, prefix, adopt=None):
    mig = {}
    out = _ensure_default(python, root, prefix, adopt, mig)
    if mig.get("message"):
        out["message"] = (mig["message"] + "; " + out.get("message", "")).strip("; ")
    return out


def _ensure_default(python, root, prefix, adopt, mig_out):
    """Installer hook (run under the installed version's full Python, after activation): host this PC's network by default.
    Absent -> install + start; an existing service this prefix owns -> repointed at this version (restarted only if it ran);
    removed by the user (settings network.host_removed or our tombstone) or someone else's service -> left alone.
    Returns the JSON-serializable status plus `action`."""
    owner = owner_id(prefix)
    owners = (owner, owner_id(adopt)) if adopt else (owner,)
    mig = migrate_settings_dir(_settings_base(), owners) if _settings_base() else None
    mig_out.update(mig or {})
    if mig and mig.get("blocked"):  # unfinished folder migration: no service or settings change until it is resolved
        return {"installed": False, "running": False, "action": "none", "message": mig["message"]}
    net, sd = _user_settings()
    if sd is None:
        return {"installed": False, "running": False, "action": "none", "message": "no settings location is available for this user; open Clients and press Start"}
    if net is None:
        return {"installed": False, "running": False, "action": "none", "message": "settings could not be read; open Clients and press Start"}
    _migrate_legacy(python, root, owner)
    d = _owned(sd) if sd.is_dir() else None
    if d:
        if d["install"].get("owner") not in owners:
            return status(sd) | {"action": "none", "message": "the network service belongs to another installation and was left untouched"}
        marker = sd / ".restart-after-migrate"
        _refresh_owned(sd, d, python, root, owner=owner, restart=marker.exists() and not lock_busy(sd))
        marker.unlink(missing_ok=True)
        _write_plain(Path(prefix) / REGISTRY, {"version": 1, "state_dir": str(sd)})
        return status(sd) | {"action": "refreshed", "message": "network service now runs from the active version"}
    if net.get("host_removed") is True or (sd / REMOVED).exists():
        return (status(sd) if sd.is_dir() else {"installed": False, "running": False}) | {
            "action": "none", "message": "hosting was removed by you; open Clients and press Start to turn it on"}
    if _definition():
        return (status(sd) if sd.is_dir() else {"installed": False, "running": False}) | {
            "action": "none", "message": "another network service is registered for this user and was left untouched"}
    allow = (not net["use_invitation"]) if isinstance(net.get("use_invitation"), bool) else True
    name = net["name"].strip()[:64] if isinstance(net.get("name"), str) and net["name"].strip() else None
    out = _install(sd, service_python(python), Path(root), owner, Path(prefix), None, None, allow, name)
    return out | {"action": "installed", "message": "this PC now hosts the network"}


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
    with _legacy_names():
        old = _definition()
        if old and old["state_dir"] and (_read_json(Path(old["state_dir"]) / "service-install.json") or {}).get("owner") == owner_id(prefix):
            _stop(old["mode"], Path(old["state_dir"]))
            _remove_definition(old["mode"])
            (Path(old["state_dir"]) / "service-install.json").unlink(missing_ok=True)
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
    from backend import discovery
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
    store = server = resp = None
    stopping = threading.Event()
    watcher = None
    try:
        _cert_ok(sd)
        cert, key = sd / "server-cert.pem", sd / "server-key.pem"
        if not cert.exists():  # first start of a NEW network only: SAN = loopback + this PC's current addresses (cosmetic)
            cert_pem, key_pem = pki.generate_identity(list(dict.fromkeys((cfg["address"], *discovery.san_hosts()))), days=3650)
            pki.write_private_new(str(key), key_pem)
            pki.write_private_new(str(cert), cert_pem)
        pki.check_private(str(key))
        pem = cert.read_text(encoding="utf-8")
        fp = pki.fingerprint(pem)
        nid = pki.network_id(fp)
        port = cfg["port"]
        token = secrets.token_urlsafe(32)
        tok = sd / "local-join.token"
        tmp = sd / f".token.{uuid.uuid4().hex[:8]}"
        pki.write_private_new(str(tmp), token)
        os.replace(tmp, tok)
        store = Store(str(sd / "coordinator.sqlite3"))
        cache = discovery.EndpointCache(30)
        live = {"allow": cfg["allow_address_join"], "name": read_name(sd), "iid": None}

        def adopt_iid():
            iid = read_local_id(sd)
            live["iid"] = iid if iid and store.installation_active(iid) else None

        def set_iid(iid):  # called by the API after a local join / verified local-identity
            tmp2 = sd / f".local.{uuid.uuid4().hex[:8]}"
            pki.write_private_new(str(tmp2), json.dumps({"version": 1, "installation_id": iid}))
            os.replace(tmp2, sd / LOCAL_ID)
            live["iid"] = iid

        def info():
            iid = live["iid"]
            return {"fingerprint_sha256": fp, "network_id": nid, "name": live["name"], "host_installation_id": iid,
                    "receiver_online": bool(iid and store.session_online(iid)), "join": live["allow"], "endpoints": cache.get(port)}

        adopt_iid()
        first = (cache.get(port) or [{"url": f"https://{cfg['address']}:{port}"}])[0]["url"]
        opts = dict(join_enabled=live["allow"], local_token=token, advertise_url=first, info=info, local_identity=set_iid)
        ctx = pki.server_context(str(cert), str(key))
        try:
            try:
                server = CoordinatorServer(("::", port), ctx, store, **opts)  # IPv4 + IPv6 on one socket
            except OSError as e:
                if e.errno in (errno.EADDRINUSE, 10048):
                    raise
                server = CoordinatorServer(("0.0.0.0", port), ctx, store, **opts)  # no usable IPv6: IPv4 wildcard
        except OSError as e:
            if e.errno in (errno.EADDRINUSE, 10048):
                raise ServiceError("port_in_use", f"port {port} is already in use") from None
            raise ServiceError("permission", f"cannot listen on port {port} ({type(e).__name__})") from None
        derr = None
        try:
            resp = discovery.Responder(info, port)
            derr = resp.start()
        except Exception as e:  # discovery is optional: the coordinator must keep serving without it
            derr = f"discovery could not start ({type(e).__name__})"
        stopper = threading.Thread(target=server.shutdown, daemon=True)
        once = threading.Lock()

        def stop(*_):
            if once.acquire(blocking=False):
                stopper.start()

        for name in ("SIGTERM", "SIGINT", "SIGBREAK"):
            if hasattr(signal, name):
                signal.signal(getattr(signal, name), stop)

        def snapshot():
            i = info()
            return dict(address=cfg["address"], port=port, fingerprint_sha256=fp, network_id=nid, endpoints=i["endpoints"],
                        discovery=derr is None, diagnostic=(derr or "")[:300], host_installation_id=i["host_installation_id"],
                        receiver_online=i["receiver_online"], name=i["name"])

        def watch():
            last, at = None, 0.0
            while not stopping.is_set():
                if (sd / "stop.request").exists():
                    stop()
                    return
                try:  # policy / name / identity changes apply live, without restarting anything
                    c = read_config(sd)
                    if c:
                        live["allow"] = server.api.join_enabled = c["allow_address_join"]
                    live["name"] = read_name(sd)
                    adopt_iid()
                    server.api.advertise_url = (cache.get(port) or [{"url": server.api.advertise_url}])[0]["url"]
                    snap = snapshot()
                    if snap != last or time.monotonic() - at > 30:
                        _publish(sd, "running", **snap)
                        last, at = snap, time.monotonic()
                except Exception:
                    pass
                stopping.wait(1)
        _publish(sd, "running", **snapshot())
        watcher = threading.Thread(target=watch, name="keyloom-service-watch", daemon=True)
        watcher.start()
        server.serve_forever()
        stopping.set()  # the watcher must not publish "running" after "stopped"
        watcher.join(3)
        _publish(sd, "stopped")
    finally:
        stopping.set()
        if resp:
            resp.stop()
        if watcher:
            watcher.join(3)
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
