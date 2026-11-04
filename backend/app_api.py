"""The app's network endpoint, written to run INSIDE the app process (A3 embeds it; nothing here is a service).

It is the old coordinator's HTTPS API (TLS pin, request limits, throttles, Join, Remove/Retire) with a durable inbox behind it:
another PC pushes a shot (POST /v1/jobs, idempotent by submission_id), the commit notifies the app (`on_commit`), the app starts it
on its own worker with `Store.inbox_begin` and reports progress with `Store.inbox_report`. The sender asks /v1/jobs/<id> (state,
queue position, progress, result) and /v1/jobs/<id>/events, and cancels with /v1/jobs/<id>/cancel. No PC polls for work, and a
registered PC is never invalidated: being offline only clears its `connected` flag.

State lives where the old service kept it (same files, read in place): service.json, service-meta.json, server-cert/key.pem,
local-installation.json, coordinator.sqlite3 (upgraded in place to schema 3 after a `.v<N>.bak` copy). Old layouts reopen:
`open_inbox` migrates the database, `read_attempt_file` reads the old `<credentials>.attempt.json` side file.
"""
import errno
import json
import os
import re
import secrets
import socket
import threading
import uuid
from pathlib import Path

from . import discovery
from .coordinator import pki
from .coordinator.server import CoordinatorServer
from .coordinator.store import Store

PORTS = tuple(range(8443, 8453))
LOCAL_ID, META, CONFIG = "local-installation.json", "service-meta.json", "service.json"
_CFG = {"version", "address", "port", "allow_address_join"}
_IID = re.compile(r"[A-Za-z0-9_-]{1,80}\Z")


class EndpointError(Exception):
    """code: not_configured, service_conflict (the old service still owns this state), port_in_use, permission, cert_mismatch."""

    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message


# ---- state readers (moved from network_service so that file can go) -----------------------------

def _read_json(p):
    try:
        d = json.loads(Path(p).read_text(encoding="utf-8-sig"))
    except FileNotFoundError:
        return None
    except (OSError, ValueError) as e:
        raise EndpointError("not_configured", f"{Path(p).name} could not be read ({type(e).__name__}); it was left untouched") from None
    return d if isinstance(d, dict) else None


def _write_plain(p, data):
    tmp = Path(p).with_name(f".{Path(p).name}.{uuid.uuid4().hex[:8]}")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(data, f)
    os.replace(tmp, p)


def read_config(sd):
    d = _read_json(Path(sd) / CONFIG)
    if d is None:
        return None
    ok = (set(d) == _CFG and d["version"] == 1 and isinstance(d["address"], str) and isinstance(d["port"], int)
          and not isinstance(d["port"], bool) and 1024 <= d["port"] <= 65535 and isinstance(d["allow_address_join"], bool))
    if not ok:
        raise EndpointError("not_configured", "service.json is invalid; it was left untouched")
    return d


def read_name(sd):
    try:
        n = (_read_json(Path(sd) / META) or {}).get("name")
    except EndpointError:
        n = None
    return n if isinstance(n, str) and 1 <= len(n) <= 64 and n.isprintable() else (socket.gethostname()[:64] or "This PC")


def read_local_id(sd):
    try:
        v = (_read_json(Path(sd) / LOCAL_ID) or {}).get("installation_id")
    except EndpointError:
        return None
    return v if isinstance(v, str) and _IID.match(v) else None


def read_attempt_file(path):
    """Old outbox side file `<credentials>.attempt.json` -> {attempt_id, job_id, seq, stages} or None when absent/unusable.
    Only read: whatever it describes is held for the user, never replayed."""
    d = _read_json(path)
    ok = d and all(isinstance(d.get(k), str) and _IID.match(d[k]) for k in ("attempt_id", "job_id")) \
        and isinstance(d.get("seq"), int) and isinstance(d.get("stages"), list)
    return {k: d[k] for k in ("attempt_id", "job_id", "seq", "stages")} if ok else None


def open_inbox(state_dir, on_commit=None):
    """The durable inbox of this PC. An old (schema 1/2) database is backed up and upgraded; unstarted offers go back to the
    queue, started shots stay in `inbox_held()`."""
    return Store(str(Path(state_dir) / "coordinator.sqlite3"), on_commit=on_commit)


def _free_port(preferred=None):
    for p in dict.fromkeys(([preferred] if preferred else []) + list(PORTS)):
        s = socket.socket()
        try:
            if os.name != "nt":
                s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s.bind(("0.0.0.0", p))
            return p
        except OSError:
            continue
        finally:
            s.close()
    raise EndpointError("port_in_use", "ports 8443-8452 are all in use by other programs")


class AppEndpoint:
    """Listener + discovery + inbox for one state directory. start()/stop() are the only lifetime calls."""

    def __init__(self, state_dir, on_commit=None):
        self.sd, self.on_commit = Path(state_dir), on_commit
        self.store = self.server = self.resp = self.lock = self.thread = None
        self.live = {"allow": False, "name": "", "iid": None}
        self.port = self.fp = self.network_id = None
        self.discovery_error = None

    def info(self):
        iid = self.live["iid"]
        return {"fingerprint_sha256": self.fp, "network_id": self.network_id, "name": self.live["name"], "host_installation_id": iid,
                "receiver_online": bool(iid and self.store.session_online(iid)), "join": self.live["allow"],
                "endpoints": self._cache.get(self.port)}

    def _set_iid(self, iid):  # called by the API after a local join / verified local-identity
        tmp = self.sd / f".local.{uuid.uuid4().hex[:8]}"
        pki.write_private_new(str(tmp), json.dumps({"version": 1, "installation_id": iid}))
        os.replace(tmp, self.sd / LOCAL_ID)
        self.live["iid"] = iid

    def refresh(self):
        """Apply a changed name / join policy / identity file live (the app calls this when its settings change; nothing polls)."""
        cfg = read_config(self.sd)
        if cfg:
            self.live["allow"] = self.server.api.join_enabled = cfg["allow_address_join"]
        self.live["name"] = read_name(self.sd)
        iid = read_local_id(self.sd)
        self.live["iid"] = iid if iid and self.store.installation_active(iid) else None

    def start(self):
        sd = self.sd
        sd.mkdir(mode=0o700, parents=True, exist_ok=True)
        cfg = read_config(sd)
        if not cfg:  # a fresh app: the same file the old service wrote, so an older build can still read it
            cfg = {"version": 1, "address": "127.0.0.1", "port": _free_port(), "allow_address_join": False}
            tmp = sd / f".service.{uuid.uuid4().hex[:8]}.tmp"
            pki.write_private_new(str(tmp), json.dumps(cfg))
            os.replace(tmp, sd / CONFIG)
        try:
            self.lock = pki.OwnerLock(str(sd / "coordinator.lock"), "app endpoint")
        except pki.OwnerBusy:
            raise EndpointError("service_conflict", "another process (the old network service?) still owns this PC's network state; "
                                "stop it before the app takes over") from None
        try:
            self._start(cfg)
        except BaseException:
            self.stop()
            raise
        return self

    def _start(self, cfg):
        sd, port = self.sd, cfg["port"]
        cert, key = sd / "server-cert.pem", sd / "server-key.pem"
        if cert.exists() != key.exists():
            raise EndpointError("cert_mismatch", "the certificate and key files are inconsistent; they were not regenerated")
        if not cert.exists():  # first start of a NEW network only: an existing certificate is never regenerated (joined PCs pin it)
            c, k = pki.generate_identity(list(dict.fromkeys((cfg["address"], *discovery.san_hosts()))), days=3650)
            pki.write_private_new(str(key), k)
            pki.write_private_new(str(cert), c)
        pki.check_private(str(key))
        pem = cert.read_text(encoding="utf-8")
        self.fp = pki.fingerprint(pem)
        self.network_id = pki.network_id(self.fp)
        self.port = port
        token = secrets.token_urlsafe(32)
        tmp = sd / f".token.{uuid.uuid4().hex[:8]}"
        pki.write_private_new(str(tmp), token)
        os.replace(tmp, sd / "local-join.token")
        self.store = open_inbox(sd, self.on_commit)
        self._cache = discovery.EndpointCache(30)
        self.live.update(allow=cfg["allow_address_join"], name=read_name(sd), iid=None)
        iid = read_local_id(sd)
        self.live["iid"] = iid if iid and self.store.installation_active(iid) else None
        first = (self._cache.get(port) or [{"url": f"https://{cfg['address']}:{port}"}])[0]["url"]
        opts = dict(join_enabled=self.live["allow"], local_token=token, advertise_url=first, info=self.info, local_identity=self._set_iid)
        ctx = pki.server_context(str(cert), str(key))
        try:
            try:
                self.server = CoordinatorServer(("::", port), ctx, self.store, **opts)  # IPv4 + IPv6 on one socket
            except OSError as e:
                if e.errno in (errno.EADDRINUSE, 10048):
                    raise
                self.server = CoordinatorServer(("0.0.0.0", port), ctx, self.store, **opts)
        except OSError as e:
            code = "port_in_use" if e.errno in (errno.EADDRINUSE, 10048) else "permission"
            raise EndpointError(code, f"cannot listen on port {port} ({type(e).__name__}: {e.strerror or e})") from None
        try:
            self.resp = discovery.Responder(self.info, port)
            self.discovery_error = self.resp.start()
        except Exception as e:  # discovery is optional: the API keeps serving without it
            self.resp, self.discovery_error = None, f"discovery could not start ({type(e).__name__}: {e})"
        self.thread = threading.Thread(target=self.server.serve_forever, name="keyloom-app-api", daemon=True)
        self.thread.start()

    def stop(self):
        """Closes the listener and discovery first (nothing new is accepted), then the inbox and the owner lock. Idempotent."""
        # shutdown() waits for serve_forever, so it is only valid once the thread ran
        steps = [(self.resp, "stop"), (self.server if self.thread else None, "shutdown"), (self.server, "server_close")]
        for obj, name in steps:
            if obj:
                try:
                    getattr(obj, name)()
                except Exception:
                    pass
        if self.thread:
            self.thread.join(5)
        for obj, name in ((self.store, "close"), (self.lock, "close")):
            if obj:
                try:
                    getattr(obj, name)()
                except Exception:
                    pass
        try:
            (self.sd / "local-join.token").unlink(missing_ok=True)
        except OSError:
            pass
        self.resp = self.server = self.store = self.lock = self.thread = None

    # ---- what the app asks of its own inbox -----------------------------------------------------

    def registered(self):
        """Every PC registered here with its `connected` flag. Offline never removes one; only Retire (Store.revoke) does."""
        return self.store.local_installations(self.live["iid"])["installations"]
