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
nothing re-installs the service until Start), stop.request (graceful-stop sentinel). Operations and failures are logged to
service.log beside the state folder (`<settings>/network/service.log`, see `_log`).
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
import traceback
import uuid
from pathlib import Path

IS_WIN = os.name == "nt"
UNIT = "keyloom-network.service"
TASK = "Keyloom Network Service"
RUN_VALUE = "KeyloomNetworkService"
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


# ---- logging -----------------------------------------------------------------------------------

ECHO = False  # True mirrors every service.log line to stderr
_T0 = time.monotonic()
_CTRL = re.compile(r"[\x00-\x1f\x7f]")


def _write_line(sd, text):
    """One line in <settings>/network/service.log (the file remote.rs writes). Best effort: a log failure never stops the service."""
    if not sd:
        return
    line = f"{int(time.time())} [svc {time.monotonic() - _T0:7.1f}s pid={os.getpid()}] {_CTRL.sub('', text)}"
    try:
        p = Path(sd).parent / "service.log"
        if p.is_file() and p.stat().st_size > 1 << 20:
            try:
                os.replace(p, p.with_name("service.log.old"))
            except OSError:
                pass
        with open(p, "a", encoding="utf-8") as f:
            f.write(line + "\n")
    except OSError:
        pass
    if ECHO:
        print(line, file=sys.stderr)


def _log(sd, msg):
    _write_line(sd, msg[:4000])


def _log_exc(sd, ctx, e):
    _log(sd, f"{ctx}: {type(e).__name__}: {e}")
    for ln in "".join(traceback.format_exception(e)).splitlines():  # traceback lines are kept whole
        _write_line(sd, ln)


def redact(argv):
    """argv as one log line: the value after --token/--password/--secret and any K=V whose K names a secret are hidden."""
    out, hide = [], False
    for a in map(str, argv):
        k, eq, _ = a.partition("=")
        if hide:
            out.append("***")
        elif eq and re.search(r"TOKEN|SECRET|PASSWORD|KEY", k, re.I):
            out.append(k + "=***")
        else:
            out.append(a)
        hide = a in ("--token", "--password", "--secret")
    return " ".join(out)


@contextlib.contextmanager
def _op(sd, name, quiet=False):
    """An operation's start and result; a failure is logged as `code: message` (ServiceError) or with its traceback. quiet: failures only."""
    if not quiet:
        _log(sd, f"{name}: start")
    try:
        yield
    except ServiceError as e:
        _log(sd, f"{name}: failed: {e.code}: {e.message}")
        raise
    except Exception as e:
        _log_exc(sd, name, e)
        raise
    if not quiet:
        _log(sd, f"{name}: done")


# ---- small helpers -----------------------------------------------------------------------------

def _run(cmd, timeout=30, sd=None, quiet=False):
    """quiet: a read-only query. When it succeeds (rc 0) nothing is logged; otherwise the whole run is logged as usual."""
    run = f"run: {redact(cmd)}"
    if not quiet:
        _log(sd, run)
    t0 = time.monotonic()
    try:
        r = subprocess.run(cmd, capture_output=True, timeout=timeout, creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    except FileNotFoundError as e:
        if quiet:
            _log(sd, run)
        _log(sd, f"{cmd[0]}: FileNotFoundError: {e}")
        raise ServiceError("service_unavailable", f"{cmd[0]} is not available on this system") from None
    except subprocess.TimeoutExpired:
        if quiet:
            _log(sd, run)
        _log(sd, f"{cmd[0]}: timed out after {timeout}s")
        raise ServiceError("service_unavailable", f"{cmd[0]} did not answer in {timeout}s") from None
    if quiet and r.returncode == 0:
        return r
    if quiet:
        _log(sd, run)
    _log(sd, f"exit {r.returncode} after {time.monotonic() - t0:.1f}s")
    for name, data in (("stdout", r.stdout), ("stderr", r.stderr)):
        for ln in (data or b"").decode("oem" if IS_WIN else "utf-8", "replace").splitlines():
            _log(sd, f"{name}| {ln}")
    return r


def _warn_if_failed(sd, r, what):
    if r.returncode:  # the caller carries on: the lock check that follows decides
        _log(sd, f"WARNING: {what} failed (exit {r.returncode}): {_out(r)}")


def _out(r):
    return (r.stderr or r.stdout or b"").decode("utf-8", "replace").strip()


def owner_id(root_or_prefix):
    return hashlib.sha256(os.path.normcase(str(Path(root_or_prefix).resolve())).encode("utf-8")).hexdigest()[:16]


def install_context():
    """(app root, owner id) of the running code. Packaged trees are <prefix>/versions/<v>/app: owner = lifecycle.owner_id(prefix)."""
    root = Path(__file__).resolve().parents[1]
    if root.name == "app" and (root.parent / "keyloom-install.json").is_file():
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
            raise ServiceError("permission", f"cannot create the service state directory {sd} ({type(e).__name__}: {e})") from None
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
    raise ServiceError("internal", f"{Path(p).name} could not be read ({type(last).__name__}: {last}); it was left untouched") from None


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
            "[Unit]\nDescription=Keyloom network service\nStartLimitIntervalSec=300\nStartLimitBurst=5\n\n"
            f"[Service]\nType=simple\nExecStart={' '.join(q(a) for a in cmd)}\nWorkingDirectory={str(sd).replace(chr(37), chr(37) * 2)}\n"
            "Restart=on-failure\nRestartSec=10\nRestartPreventExitStatus=2\n\n[Install]\nWantedBy=default.target\n")


def _systemctl(*a, sd=None):
    r = _run(["systemctl", "--user", *a], sd=sd)
    if r.returncode and a[0] in ("daemon-reload", "enable", "start", "stop", "disable"):
        raise ServiceError("service_unavailable", f"systemctl --user {a[0]} failed: {_out(r)[:2000] or r.returncode}")
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


def _win_user(sd):
    r = _run(["whoami"], sd=sd)
    if r.returncode or not r.stdout.strip():
        raise ServiceError("service_unavailable", "cannot determine the current Windows user")
    return r.stdout.decode("utf-8", "replace").strip()


def _definition(sd=None):
    """-> {"mode", "state_dir"} of the per-user definition if one exists (owned or not), else None."""
    if not IS_WIN:
        p = _unit_path()
        if not p.is_file():
            return None
        m = re.search(r"^# X-Keyloom-State-Dir=(.*)$", p.read_text(encoding="utf-8", errors="replace"), re.M)
        return {"mode": "systemd", "state_dir": m.group(1) if m else None}
    r = _run(["schtasks", "/Query", "/TN", TASK, "/XML"], sd=sd, quiet=True)
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
    """Same folder (by real path)."""
    try:
        return os.path.normcase(os.path.realpath(a)) == os.path.normcase(os.path.realpath(b))
    except OSError:
        return os.path.normcase(str(a)) == os.path.normcase(str(b))


def _owned(sd):
    """The definition names `sd` AND `sd` carries our install record: only then may we change or remove it."""
    d = _definition(sd)
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
        _systemctl("daemon-reload", sd=sd)
        _systemctl("enable", UNIT, sd=sd)
        return mode
    if mode == "task":
        xml = sd / f".task.{uuid.uuid4().hex[:8]}.xml"
        xml.write_text(_task_xml(cmd, sd, owner, _win_user(sd)), encoding="utf-16")
        try:
            r = _run(["schtasks", "/Create", "/TN", TASK, "/XML", str(xml), "/F"], sd=sd)
        finally:
            xml.unlink(missing_ok=True)
        if r.returncode == 0:
            _delete_run_value(sd)
            return "task"
        _log(sd, f"schtasks /Create refused the logon task (exit {r.returncode}): {_out(r)}; falling back to the HKCU Run value {RUN_VALUE}")
        mode = "run"  # standard users may be refused a logon task: equivalent per-user autostart, still no elevation
    import winreg
    with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
        winreg.SetValueEx(k, RUN_VALUE, 0, winreg.REG_SZ, subprocess.list2cmdline(cmd))
        _log(sd, "Run value written")
    return "run"


def _delete_run_value(sd):
    import winreg
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
            winreg.DeleteValue(k, RUN_VALUE)
    except FileNotFoundError:
        pass
    except OSError as e:
        _log(sd, f"could not remove the HKCU Run value {RUN_VALUE}: {type(e).__name__}: {e}")


def _remove_definition(mode, sd):
    if mode == "systemd":
        _systemctl("disable", UNIT, sd=sd)
        _unit_path().unlink(missing_ok=True)
        _systemctl("daemon-reload", sd=sd)
    else:
        if mode == "task":
            r = _run(["schtasks", "/Delete", "/TN", TASK, "/F"], sd=sd)
            if r.returncode:
                raise ServiceError("service_unavailable", f"schtasks /Delete failed (exit {r.returncode}): {_out(r)[:2000]}")
        _delete_run_value(sd)


def _spawn(mode, cmd, sd):
    if mode == "systemd":
        _warn_if_failed(sd, _run(["systemctl", "--user", "reset-failed", UNIT], sd=sd), "systemctl reset-failed")
        _systemctl("start", UNIT, sd=sd)
    elif mode == "task":
        r = _run(["schtasks", "/Run", "/TN", TASK], sd=sd)
        if r.returncode:
            raise ServiceError("service_unavailable", f"the scheduled task could not be started: {_out(r)[:2000]}")
    else:
        env = {k: v for k, v in os.environ.items() if not k.startswith("PYTHON") and k != "VIRTUAL_ENV"}
        try:
            p = subprocess.Popen(cmd, cwd=str(sd), env=env, close_fds=True, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                 stderr=subprocess.DEVNULL, creationflags=0x00000008 | 0x00000200 | 0x08000000)
        except OSError as e:
            raise ServiceError("start_failed", f"{type(e).__name__}: {e}") from None
        _log(sd, f"started pid={p.pid}: {redact(cmd)}")


def _kill_holder(sd):
    """Last resort for a service that ignores stop.request and has no definition we can stop: end the pid its state file names,
    but only if that process's command line mentions this state folder (guards against pid reuse)."""
    pid = (_read_json(sd / "service-state.json") or {}).get("pid")
    if not isinstance(pid, int) or pid <= 0:
        return
    if IS_WIN:
        r = _run(["powershell", "-NoProfile", "-Command", f"(Get-CimInstance Win32_Process -Filter 'ProcessId={pid}').CommandLine"], sd=sd)
        _warn_if_failed(sd, r, "PowerShell CommandLine query")
        cmdline = r.stdout.decode("utf-8", "replace")
    else:
        try:
            cmdline = Path(f"/proc/{pid}/cmdline").read_bytes().decode("utf-8", "replace").replace("\0", " ")
        except OSError:
            return
    if str(sd) not in cmdline:
        return
    if IS_WIN:
        _warn_if_failed(sd, _run(["taskkill", "/PID", str(pid), "/F"], sd=sd), "taskkill")
    else:
        with contextlib.suppress(OSError):
            os.kill(pid, signal.SIGTERM)
    end = time.monotonic() + 10
    while lock_busy(sd) and time.monotonic() < end:
        time.sleep(0.25)


def _stop(mode, sd, wait=15):
    if not lock_busy(sd):
        return
    (sd / "stop.request").write_text("stop", encoding="utf-8")  # graceful on every platform; the service polls for it
    end = time.monotonic() + wait
    while lock_busy(sd) and time.monotonic() < end:
        time.sleep(0.25)
    if lock_busy(sd):
        if mode == "systemd":
            _systemctl("stop", UNIT, sd=sd)
        elif mode == "task":
            _warn_if_failed(sd, _run(["schtasks", "/End", "/TN", TASK], sd=sd), "schtasks /End")
        end = time.monotonic() + 10
        while lock_busy(sd) and time.monotonic() < end:
            time.sleep(0.25)
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
        raise ServiceError("permission", f"port {port} cannot be opened ({type(e).__name__}: {e}); it may be reserved or blocked") from None
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
    with _op(sd, "status", quiet=True):
        return _status(sd)


def _status(sd):
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
    if not d and _definition(sd):
        out["message"] = "a service for a different state folder is registered for this user"
    diag = (st.get("message") if st.get("state") == "failed" else "") or (st.get("diagnostic") if live else "") or ""
    out["diagnostic"] = str(diag)[:300]  # last error only; the service never puts secrets in it
    return out


def install(state_dir, address=None, port=None, allow=None, name=None):
    """New callers pass only state_dir (+ allow_address_join, name): the host listens on every interface, the port is the existing
    one or the first free of 8443..8452, and `address` is a cosmetic value chosen here. Old callers may still pass address/port."""
    root, owner, prefix = install_context()
    sd = state_path(state_dir)
    with _op(sd, "install"):
        return _install(sd, service_python(sys.executable), root, owner, prefix, address, port, allow, name)


def _install(sd, py, root, owner, prefix, address=None, port=None, allow=None, name=None):
    if allow is not None and not isinstance(allow, bool):
        raise ServiceError("bad_request", "allow_address_join must be true or false")
    ensure_dir(sd, True)
    existing = _definition(sd)
    if existing and (not existing["state_dir"] or os.path.normcase(existing["state_dir"]) != os.path.normcase(str(sd))):
        raise ServiceError("service_conflict", "a different network service is already registered for this user; remove it first")
    _cert_ok(sd)
    cur = _owned(sd)
    mode = cur["mode"] if cur else ("systemd" if not IS_WIN else "task")
    cfg = read_config(sd)
    if name is not None:
        _write_meta(sd, name)
    if lock_busy(sd):
        _log(sd, "a service is running: stopping it before the change")
        _stop(mode, sd)
    allow = allow if allow is not None else (cfg["allow_address_join"] if cfg else True)
    address = address if address is not None else (cfg["address"] if cfg else _choose_address(sd))
    if port is not None:
        validate(address, port, allow)
        _port_free(port)
    else:
        port = _pick_port(cfg["port"] if cfg else None)
    validate(address, port, allow)
    _log(sd, f"address={address} port={port} allow_address_join={allow} mode={mode}")
    (sd / REMOVED).unlink(missing_ok=True)
    _write_cfg(sd, address, port, allow)
    cmd = _command(py, root, sd)
    mode = _write_definition(mode, cmd, sd, owner)
    _log(sd, f"definition written: mode={mode}")
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
    with _op(sd, "start"):
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
    with _op(sd, "stop"):
        d = _owned_or_raise(sd)
        _stop(d["mode"], sd)
        return status(sd)


def remove(state_dir):
    sd = state_path(state_dir)
    with _op(sd, "remove"):
        ensure_dir(sd, False)
        d = _owned(sd)
        if d:
            _stop(d["mode"], sd)
            _remove_definition(d["mode"], sd)
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
    with _op(sd, "configure"):
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


def _refresh_owned(sd, d, python, root, owner=None):
    py, was = service_python(python), lock_busy(sd)
    if was:
        _stop(d["mode"], sd)
    owner = owner or d["install"]["owner"]
    cmd = _command(py, root, sd)
    mode = _write_definition(d["mode"], cmd, sd, owner)
    _write_plain(sd / "service-install.json", d["install"] | {"owner": owner, "mode": mode, "root": str(root), "python": str(py)})
    if was:
        _begin_start(mode, cmd, sd)


def _refreshed_message(sd):
    if lock_busy(sd):
        return "network service now runs from the active version"
    return ("network service registration updated to the active version; it is not running "
            "(it starts at the next sign-in, or press Start in Clients)")


def refresh(python, root, prefix):
    """Activation/rollback hook (lifecycle): repoint an OWNED service at the newly active version. Restarts it only if it ran."""
    reg = _read_json(Path(prefix) / REGISTRY)
    if not reg:
        return None
    sd = state_path(reg["state_dir"])
    with _op(sd, "refresh"):
        d = _owned(sd)
        if not d or d["install"].get("owner") != owner_id(prefix):
            return None
        _refresh_owned(sd, d, python, root)
        return _refreshed_message(sd)


def settings_dir(base):
    """`<base>/keyloom`: the settings and state folder of this user. Mirrors rust-ui settings.rs config_dir."""
    return Path(base) / "keyloom"


def _settings_base():
    return (os.environ.get("APPDATA") or os.environ.get("LOCALAPPDATA")) if IS_WIN else \
        (os.environ.get("XDG_CONFIG_HOME") or (str(Path(os.environ["HOME"]) / ".config") if os.environ.get("HOME") else None))


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


def ensure_default(python, root, prefix):
    sd = _user_settings()[1]
    with _op(sd, "ensure_default"):
        out = _ensure_default(python, root, prefix)
        _log(sd, f"ensure_default branch: {out['action']}: {out['message']}")
        return out


def _ensure_default(python, root, prefix):
    """Installer hook (run under the installed version's full Python, after activation): host this PC's network by default.
    Absent -> install + start; an existing service this prefix owns -> repointed at this version (restarted only if it ran);
    removed by the user (settings network.host_removed or our tombstone) or someone else's service -> left alone.
    Returns the JSON-serializable status plus `action`."""
    owner = owner_id(prefix)
    net, sd = _user_settings()
    if sd is None:
        return {"installed": False, "running": False, "action": "none", "message": "no settings location is available for this user; open Clients and press Start"}
    if net is None:
        return {"installed": False, "running": False, "action": "none", "message": "settings could not be read; open Clients and press Start"}
    d = _owned(sd) if sd.is_dir() else None
    if d:
        if d["install"].get("owner") != owner:
            return status(sd) | {"action": "none", "message": "the network service belongs to another installation and was left untouched"}
        _refresh_owned(sd, d, python, root, owner=owner)
        _write_plain(Path(prefix) / REGISTRY, {"version": 1, "state_dir": str(sd)})
        return status(sd) | {"action": "refreshed", "message": _refreshed_message(sd)}
    if net.get("host_removed") is True or (sd / REMOVED).exists():
        return (status(sd) if sd.is_dir() else {"installed": False, "running": False}) | {
            "action": "none", "message": "hosting was removed by you; open Clients and press Start to turn it on"}
    if _definition(sd):
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
    with _op(sd, "remove_owned"):
        d = _owned(sd)
        if d and d["install"].get("owner") == owner_id(prefix):
            _stop(d["mode"], sd)
            _remove_definition(d["mode"], sd)
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
    _log(sd, f"service starting: pid={os.getpid()} python={sys.executable} root={sys.path[0]} state={sd} port={cfg['port']}")
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
                _log(sd, f"listening on [::]:{port}")
            except OSError as e:
                if e.errno in (errno.EADDRINUSE, 10048):
                    raise
                server = CoordinatorServer(("0.0.0.0", port), ctx, store, **opts)  # no usable IPv6: IPv4 wildcard
                _log(sd, f"listening on 0.0.0.0:{port} (no IPv6)")
        except OSError as e:
            _log_exc(sd, f"cannot listen on port {port}", e)
            if e.errno in (errno.EADDRINUSE, 10048):
                raise ServiceError("port_in_use", f"port {port} is already in use ({e.strerror or e})") from None
            raise ServiceError("permission", f"cannot listen on port {port} ({type(e).__name__}: {e.strerror or e})") from None
        derr = None
        try:
            resp = discovery.Responder(info, port)
            derr = resp.start()
        except Exception as e:  # discovery is optional: the coordinator must keep serving without it
            _log_exc(sd, "discovery", e)
            derr = f"discovery could not start ({type(e).__name__}: {e})"
        stopper = threading.Thread(target=server.shutdown, daemon=True)
        once = threading.Lock()

        def stop(reason):
            if once.acquire(blocking=False):
                _log(sd, f"stop: {reason}")
                stopper.start()

        for name in ("SIGTERM", "SIGINT", "SIGBREAK"):
            if hasattr(signal, name):
                signal.signal(getattr(signal, name), lambda *_, n=name: stop(n))

        def snapshot():
            i = info()
            return dict(address=cfg["address"], port=port, fingerprint_sha256=fp, network_id=nid, endpoints=i["endpoints"],
                        discovery=derr is None, diagnostic=(derr or "")[:300], host_installation_id=i["host_installation_id"],
                        receiver_online=i["receiver_online"], name=i["name"])

        def watch():
            last, at, last_err = None, 0.0, None
            while not stopping.is_set():
                if (sd / "stop.request").exists():
                    stop("stop.request")
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
                except Exception as e:
                    text = f"{type(e).__name__}: {e}"  # once per distinct error: the loop runs every second
                    if text != last_err:
                        _log_exc(sd, "watch", e)
                        last_err = text
                stopping.wait(1)
        _publish(sd, "running", **snapshot())
        watcher = threading.Thread(target=watch, name="keyloom-service-watch", daemon=True)
        watcher.start()
        server.serve_forever()
        _log(sd, "service stopped")
        stopping.set()  # the watcher must not publish "running" after "stopped"
        watcher.join(3)
        _publish(sd, "stopped")
    finally:
        stopping.set()
        # each step has its own try: one failure must not skip the lock release below
        try:
            if resp:
                resp.stop()
        except Exception as e:
            _log_exc(sd, "cleanup resp", e)
        try:
            if watcher:
                watcher.join(3)
        except Exception as e:
            _log_exc(sd, "cleanup watcher", e)
        try:
            if server:
                server.server_close()
        except Exception as e:
            _log_exc(sd, "cleanup server", e)
        try:
            if store:
                store.close()
        except Exception as e:
            _log_exc(sd, "cleanup store", e)
        for n in ("local-join.token", "stop.request"):
            try:
                (sd / n).unlink(missing_ok=True)
            except Exception as e:
                _log_exc(sd, f"cleanup unlink {n}", e)
        try:
            lock.close()
        except Exception as e:
            _log_exc(sd, "cleanup lock", e)


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
            code, msg, rc = "start_failed", f"{type(e).__name__}: {e}", 1
        _log_exc(sd, "service failed", e)
        try:
            _publish(sd, "failed", code, msg)
        except OSError as pe:
            _log_exc(sd, "could not publish the failure", pe)
        return rc
