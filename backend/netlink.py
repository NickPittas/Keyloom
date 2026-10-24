"""Persistent JSON-lines bridge between the local app and a coordinator (`python -m backend.netlink`).

A transport adapter only: it never renders, never resolves media paths, never claims GPU ownership and has no
queue or auth of its own. The local scheduler decides when to send claim/start/reject; everything is forwarded
through `backend.coordinator.client.Client`, whose server-side schema is the single validator.

Protocol v1 — one JSON object per line on stdin (UTF-8, <= 1 MiB incl. newline), answers on stdout, one per request:
  request : {"id": "<1-64 of A-Za-z0-9_.->", "op": "<op>", "args": {...}}      (args may be omitted when empty)
  success : {"id": ..., "ok": true,  "result": {...}}
  failure : {"id": ..., "ok": false, "error": {"code": "...", "message": "..."}}   (id is null if unreadable)
  startup : {"event": "ready", "protocol": 1}      EOF on stdin or op "close" ends the process (exit 0).
stdout carries nothing else; stderr gets at most a few bounded diagnostics. Requests are served in order.
Ops (args are exact; unknown keys are refused; bodies are validated by the coordinator):
  pair          invitation_path, credentials_path, name, [existing_credentials_path, existing_credentials_paths[]]   -> installation_id   (credentials written 0600, never echoed;
                the destination is reserved before the one-use code is redeemed)
  connect       credentials_path                           -> installation_id   (session id kept beside it, 0600; takes an exclusive
                lock on <credentials>.lock until close/EOF/exit, so a second bridge on the same credentials gets `busy`)
  register      build, gpu{uuid,name,vram_total_mb,ram_total_mb}, [name]   (name = this PC's display name; updates the same installation)
  submit        submission_id, spec, target
  claim         claim_id, capabilities, resources
  start         attempt_id, effective_plan
  reject        attempt_id, code, message
  events        attempt_id, events[]
  stats         seq, measured_age_ms, [cpu_percent, ram_used_bytes, ram_total_bytes, gpus]   (README stats schema)
  cancel | job  job_id          job_events  job_id, [after, limit]          installations      close
  service_status|service_start|service_stop|service_remove  state_dir        service_install  state_dir, address, port, allow_address_join
  join_address  url, name, credentials_path, [existing_credentials_path, existing_credentials_paths[]]
  service_local_join  state_dir, credentials_path, name, [existing_credentials_path, existing_credentials_paths[]]
  network_invite  path (needs a connected link; writes a reusable 7-day non-admin invitation file, reply never carries the code)
  pair/join_address/service_local_join reply {installation_id, credentials_path, reused[, url, fingerprint_sha256, network_id]}: with
  an existing candidate pinning the SAME certificate nothing is redeemed and that identity is reused (credentials_path = the saved one);
  unreadable unrelated candidates are skipped. Status adds certificate_addresses[].
  Extra error codes: service_unavailable, permission, port_in_use, service_conflict, not_installed, not_configured, cert_mismatch,
  start_failed, start_timeout. Service ops use no coordinator connection and work with nothing paired.
Errors: codes from the coordinator are passed through; bridge codes are bad_request, too_large, unknown_op,
not_connected, busy (a running link already holds these credentials), security (incl. rejected server certificate), io (local file), transport (network/DNS/timeout; nothing is
retried, cancelled or reassigned), paired_not_saved (code spent, credentials lost: revoke + new invitation) and
pair_uncertain (transport failure while pairing: the code MAY be spent).
Token and invitation code are redacted from every message. The bridge holds no work: a started attempt stays
owned by the coordinator whatever happens to this process."""
import argparse
import hmac
import http.client
import json
import os
import re
import secrets
import ssl
import sys
import time

from . import coordinator as _c
from . import network_service as _svc
from .coordinator import pki
from .coordinator.client import BadReply, Client, CoordinatorError, join_to_file, redeem_to_file
from .coordinator.schema import MAX_BODY, ApiError, parse_json

_ID = re.compile(r"[A-Za-z0-9_.-]{1,64}\Z")
_REF = re.compile(r"[\w-]{1,80}\Z")  # attempt/job ids go into URLs: never anything else
_SESSION_ID = re.compile(r"[A-Za-z0-9_-]{16,64}\Z")
_MAX_DIAG = 20


class BridgeError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code, self.message = code, message


def _need(args, required, optional=()):
    if not isinstance(args, dict) or set(args) - set(required) - set(optional) or set(required) - set(args):
        raise BridgeError("bad_request", f"args must have exactly {sorted(required)} (+ optional {sorted(optional)})")
    return args


def _ref(v, what):
    if not isinstance(v, str) or not _REF.match(v):
        raise BridgeError("bad_request", f"{what} is not a valid id")
    return v


def _path(v, what):
    if not isinstance(v, str) or not v or len(v) > 4096 or "\0" in v:
        raise BridgeError("bad_request", f"{what} must be a path string")
    return v


_EXISTING = ("existing_credentials_path", "existing_credentials_paths")


def _same_network(a, fingerprint):
    """(path, credentials) of the first saved identity pinning this certificate, else None. Candidates are read only:
    an unreadable/missing unrelated one is skipped so it cannot block joining a different network."""
    paths = [] if "existing_credentials_path" not in a else [a["existing_credentials_path"]]
    extra = a.get("existing_credentials_paths", [])
    if not isinstance(extra, list) or len(extra) > 64:
        raise BridgeError("bad_request", "existing_credentials_paths must be a list of at most 64 paths")
    for p in dict.fromkeys(_path(p, "existing credentials path") for p in paths + extra):  # validate (type) before dedupe
        try:
            have = pki.read_credentials(p)
        except (OSError, ValueError, pki.SecurityError):
            continue
        if hmac.compare_digest(have["fingerprint_sha256"].lower(), fingerprint.lower()):
            return p, have
    return None


class _Net:
    """Classifies by operation boundary: anything raised while talking to the coordinator is transport (or security
    for a rejected certificate); local file errors elsewhere stay io/security. No retries, no insecure fallback."""

    def __init__(self, client):
        self._client = client

    def __getattr__(self, name):
        fn = getattr(self._client, name)

        def call(*a, **kw):
            try:
                return fn(*a, **kw)
            except ssl.SSLCertVerificationError as e:  # a SSLError and a ValueError: must be caught first
                raise pki.SecurityError(f"server certificate rejected ({e.verify_message})") from None
            except (OSError, http.client.HTTPException, BadReply) as e:  # gaierror, EHOSTUNREACH, timeout, TLS, garbled reply
                raise BridgeError("transport", type(e).__name__) from None
        return call


class Bridge:
    def __init__(self, timeout=15, client_cls=Client):
        self.timeout, self.client_cls = timeout, client_cls
        self.client = self.session_id = self.creds = None
        self.lock = None  # pki.OwnerLock on <credentials>.lock, held while connected
        self.secrets = set()
        self.diags = 0

    # ---- plumbing ---------------------------------------------------------------------------------

    def _secret(self, x):
        if not isinstance(x, str) or not x:
            raise pki.SecurityError("credential value is not a non-empty string")
        self.secrets.add(x)

    def _redact(self, s):
        s = str(s)
        for x in self.secrets:
            s = s.replace(x, "[redacted]")
        return s

    def _diag(self, msg):
        if self.diags < _MAX_DIAG:
            self.diags += 1
            print(self._redact(msg)[:200], file=sys.stderr, flush=True)

    def _session(self, creds_path):
        path = creds_path + ".session"
        if os.path.exists(path):
            sid = json.loads(pki.read_private(path)).get("session_id")
            if not isinstance(sid, str) or not _SESSION_ID.match(sid):
                raise pki.SecurityError("session file is corrupt; it was left untouched")
            return sid
        sid = secrets.token_hex(16)
        pki.write_private_new(path, json.dumps({"session_id": sid}))
        return sid

    def _live(self):
        if not self.client:
            raise BridgeError("not_connected", "send connect (or pair, then connect) first")
        return _Net(self.client)

    # ---- operations -------------------------------------------------------------------------------

    def op_pair(self, a):
        _need(a, ("invitation_path", "credentials_path", "name"), _EXISTING)
        inv = pki.read_invitation(_path(a["invitation_path"], "invitation_path"))
        self._secret(inv["code"])
        dest = _path(a["credentials_path"], "credentials_path")
        have = _same_network(a, inv["fingerprint_sha256"])  # same network (pinned certificate) as a saved identity: reuse it, redeem nothing
        if have:
            return {"installation_id": have[1]["installation_id"], "credentials_path": have[0], "reused": True}
        # Same routine as the CLI: reserves the private destination BEFORE the one-use code is redeemed.
        return {"installation_id": redeem_to_file(inv, dest, a["name"]), "credentials_path": dest, "reused": False}

    # ---- network service / address join / invitations (no coordinator connection needed except network_invite) ----

    def _state_dir(self, a, required=("state_dir",), optional=()):
        _need(a, required, optional)
        return _path(a["state_dir"], "state_dir")

    def op_service_status(self, a):
        return _svc.status(self._state_dir(a))

    def op_service_install(self, a):
        sd = self._state_dir(a, ("state_dir", "address", "port", "allow_address_join"))
        return _svc.install(sd, a["address"], a["port"], a["allow_address_join"])

    def op_service_start(self, a):
        return _svc.start(self._state_dir(a))

    def op_service_stop(self, a):
        return _svc.stop(self._state_dir(a))

    def op_service_remove(self, a):
        return _svc.remove(self._state_dir(a))

    def _joined(self, a, url, **kw):
        dest = _path(a["credentials_path"], "credentials_path")
        pem = kw.pop("pem", None)
        if pem is None:  # fetch ONCE (trust on first use, as join_to_file does) so the matching saved identity can be chosen first
            host, port = pki.endpoint(url)
            try:
                pem = ssl.get_server_certificate((host, port), timeout=self.timeout)  # handshake only: nothing is sent
            except (OSError, ValueError) as e:
                raise CoordinatorError(0, "transport", f"could not reach {host}:{port} ({type(e).__name__})") from None
            if not pki.cert_covers_host(pem, host):
                raise pki.SecurityError(f"the certificate presented by {host} does not cover that address; not trusting it")
        have = _same_network(a, pki.fingerprint(pem))
        iid, fp, reused = join_to_file(url, dest, a["name"], existing=have and have[0], timeout=self.timeout, pem=pem, **kw)
        return {"installation_id": iid, "credentials_path": have[0] if reused else dest, "reused": reused,
                "fingerprint_sha256": fp, "network_id": pki.network_id(fp)}

    def op_join_address(self, a):
        _need(a, ("url", "name", "credentials_path"), _EXISTING)
        url = _path(a["url"], "url")
        host, port = pki.endpoint(url)
        return {**self._joined(a, url), "url": f"https://{host if ':' not in host else '[' + host + ']'}:{port}"}

    def op_service_local_join(self, a):
        sd = _svc.state_path(self._state_dir(a, ("state_dir", "credentials_path", "name"), _EXISTING))
        cfg = _svc.read_config(sd) if sd.is_dir() else None
        if not cfg or not _svc.lock_busy(sd):
            raise BridgeError("service_unavailable", "the network service is not running on this PC")
        token = pki.read_private(str(sd / "local-join.token")).strip()
        self._secret(token)
        pem = (sd / "server-cert.pem").read_text(encoding="utf-8")
        host = "127.0.0.1" if pki.cert_covers_host(pem, "127.0.0.1") else cfg["address"]
        url = f"https://{host}:{cfg['port']}"
        return {**self._joined(a, url, path="/v1/local-join", extra={"token": token}, pem=pem, loopback=True), "url": url}

    def op_network_invite(self, a):
        """Reusable 7-day NON-ADMIN invitation for the network this bridge is connected to. The code goes only into the new
        private file (never overwritten); it is not part of the reply."""
        _need(a, ("path",))
        path = _path(a["path"], "path")
        if os.path.lexists(path):
            raise BridgeError("io", "that invitation filename already exists; choose a new filename")
        out = self._live().invite()
        code, exp, url = out.get("code"), out.get("expires_at"), out.get("url") or self.creds["url"]
        if not isinstance(code, str) or not 8 <= len(code) <= 64 or isinstance(exp, bool) or not isinstance(exp, (int, float)) \
                or not isinstance(url, str):
            raise BridgeError("transport", "the invitation reply was malformed")
        self._secret(code)
        pki.write_invitation(path, url, self.creds["certificate_pem"], code, exp)
        fp = self.creds["fingerprint_sha256"]
        return {"path": path, "expires_at": exp, "url": url, "fingerprint_sha256": fp, "network_id": pki.network_id(fp)}

    def release(self):
        if self.lock:
            self.lock.close()
            self.lock = None

    def op_connect(self, a):
        _need(a, ("credentials_path",))
        path = _path(a["credentials_path"], "credentials_path")
        creds = pki.read_credentials(path)
        self._secret(creds["token"])
        # One bridge per credentials: the persisted session id would otherwise let two processes act as one installation.
        # A reconnect to the SAME credentials keeps the lock we already hold; a different one is locked first and the
        # old lock/connection are replaced only after everything succeeded, so a failed connect disturbs nothing.
        keep = self.lock is not None and self.lock.path == path + ".lock"
        try:
            new = self.lock if keep else pki.OwnerLock(path + ".lock")
        except pki.OwnerBusy:
            raise pki.OwnerBusy("these credentials are already in use by a running link") from None
        try:
            client = self.client_cls(creds, self.timeout)
            session_id = self._session(path)
        except BaseException:
            if not keep:
                new.close()
            raise
        if not keep:
            self.release()
        self.lock, self.client, self.session_id, self.creds = new, client, session_id, creds
        return {"installation_id": creds["installation_id"], "session_id": session_id,
                "url": creds["url"], "network_id": pki.network_id(creds["fingerprint_sha256"])}

    def op_register(self, a):
        _need(a, ("build", "gpu"), ("name",))
        c = self._live()
        name = {"name": a["name"]} if "name" in a else {}  # the coordinator validates it (<= 64 chars)
        return c.register({"protocol": _c.PROTOCOL, "session_id": self.session_id, "build": a["build"], "gpus": [a["gpu"]], **name})

    def op_submit(self, a):
        _need(a, ("submission_id", "spec", "target"))
        return self._live().submit(a["submission_id"], a["spec"], a["target"])

    def op_claim(self, a):
        _need(a, ("claim_id", "capabilities", "resources"))
        return self._live().claim({"session_id": self.session_id, **a})

    def op_start(self, a):
        _need(a, ("attempt_id", "effective_plan"))
        return self._live().start(_ref(a["attempt_id"], "attempt_id"), {"session_id": self.session_id, "effective_plan": a["effective_plan"]})

    def op_reject(self, a):
        _need(a, ("attempt_id", "code", "message"))
        return self._live().reject(_ref(a["attempt_id"], "attempt_id"),
                                   {"session_id": self.session_id, "code": a["code"], "message": a["message"]})

    def op_events(self, a):
        _need(a, ("attempt_id", "events"))
        return self._live().report(_ref(a["attempt_id"], "attempt_id"), {"session_id": self.session_id, "events": a["events"]})

    def op_stats(self, a):
        if not isinstance(a, dict) or "session_id" in a:
            raise BridgeError("bad_request", "stats args must be an object without session_id")
        return self._live().stats({"session_id": self.session_id, **a})

    def op_cancel(self, a):
        return self._live().cancel(_ref(_need(a, ("job_id",))["job_id"], "job_id"))

    def op_job(self, a):
        return self._live().job(_ref(_need(a, ("job_id",))["job_id"], "job_id"))

    def op_job_events(self, a):
        _need(a, ("job_id",), ("after", "limit"))
        after, limit = a.get("after", 0), a.get("limit", 100)
        if any(isinstance(v, bool) or not isinstance(v, int) for v in (after, limit)):
            raise BridgeError("bad_request", "after/limit must be integers")
        return self._live().job_events(_ref(a["job_id"], "job_id"), after, limit)

    def op_estimate(self, a):
        """Receiver eligibility for a PINNED temporal window: the engine's own estimator (`v2_temporal`), the first
        frame's real size and the memory the receiver reports as free now. Estimates nothing itself; Auto never asks."""
        _need(a, ("first_frame", "frames", "temporal_window", "vram_profile", "free_vram_mb"))
        ff, n, tw, vp, free = (a[k] for k in ("first_frame", "frames", "temporal_window", "vram_profile", "free_vram_mb"))
        if (not isinstance(ff, str) or not ff or any(isinstance(v, bool) or not isinstance(v, int) or v < 1 for v in (n, free))
                or not isinstance(tw, str) or not isinstance(vp, str)):
            raise BridgeError("bad_request", "estimate needs first_frame, frames, temporal_window, vram_profile, free_vram_mb")
        from backend.service.v2_inputs import _image_size
        from backend.v2_temporal import estimate_temporal_window
        size = _image_size(ff)
        if size is None:
            raise BridgeError("io", "the first frame could not be read to measure its size")
        try:
            est = estimate_temporal_window(size[0], size[1], n, tw, vp, free / 1024)
        except ValueError as e:
            raise BridgeError("bad_request", str(e))
        return {"over_budget": est.over_budget, "required_gib": est.required_gib, "peak_gib": est.peak_gib,
                "budget_gib": est.budget_gib, "chunk": est.chunk, "stride": est.stride, "low_vram": est.low_vram,
                "caveat": est.caveat, "width": size[0], "height": size[1]}

    def op_installations(self, a):
        _need(a, ())
        return self._live().installations()

    # ---- request loop -----------------------------------------------------------------------------

    def handle(self, line):
        """bytes line -> (response dict, stop?)"""
        rid = None
        try:
            try:
                msg = parse_json(line)
            except ApiError as e:
                raise BridgeError("too_large" if e.status == 413 else "bad_request", e.message)
            if isinstance(msg.get("id"), str) and _ID.match(msg["id"]):
                rid = msg["id"]
            if rid is None or set(msg) - {"id", "op", "args"} or not isinstance(msg.get("op"), str):
                raise BridgeError("bad_request", "request needs id (1-64 of A-Za-z0-9_.-), op and optional args only")
            if msg["op"] == "close":
                return {"id": rid, "ok": True, "result": {}}, True
            fn = getattr(self, "op_" + msg["op"], None) if re.fullmatch(r"[a-z_]{1,24}", msg["op"]) and msg["op"] != "close" else None
            if fn is None:
                raise BridgeError("unknown_op", "unsupported op")
            return {"id": rid, "ok": True, "result": fn(msg.get("args", {}))}, False
        except BridgeError as e:
            err = (e.code, e.message)
        except CoordinatorError as e:
            err = (e.code, str(e))
        except _svc.ServiceError as e:
            err = (e.code, e.message)
        except pki.OwnerBusy as e:
            err = ("busy", str(e))
        except pki.PairedNotSaved as e:
            err = ("paired_not_saved", str(e))
        except pki.PairingUncertain as e:
            err = ("pair_uncertain", str(e))
        except pki.SecurityError as e:
            err = ("security", str(e))
        except ValueError as e:  # corrupt local credential/session file or unreadable server reply
            err = ("io", type(e).__name__)
        except OSError as e:
            err = ("io", type(e).__name__)
        except Exception as e:  # never a stack trace to the app
            err = ("internal", type(e).__name__)
        self._diag(f"{rid}: {err[0]}")
        return {"id": rid, "ok": False, "error": {"code": err[0], "message": self._redact(err[1])[:500]}}, False

    def run(self, rin, wout):
        def send(obj):
            try:  # raw UTF-8: never larger than the coordinator body
                data = json.dumps(obj, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
            except UnicodeEncodeError:  # invalid Unicode from the peer must not crash the bridge or reach the app
                data = json.dumps({"id": obj.get("id"), "ok": False, "error": {
                    "code": "internal", "message": "the reply contained invalid Unicode and was dropped"}}).encode()
            wout.write(data + b"\n")
            wout.flush()
        send({"event": "ready", "protocol": _c.PROTOCOL})
        try:
            return self._loop(rin, send)
        finally:
            self.release()  # EOF, close op or a crash in the loop: the credentials lock never outlives the bridge

    def _loop(self, rin, send):
        while True:
            line = rin.readline(MAX_BODY + 2)
            if not line:
                return 0  # EOF: nothing to cancel or hand over; the coordinator keeps ownership
            if not line.endswith(b"\n") and len(line) > MAX_BODY:
                while line and not line.endswith(b"\n"):  # drain the rest of the oversize line in bounded chunks
                    line = rin.readline(65536)
                send({"id": None, "ok": False, "error": {"code": "too_large", "message": "request line exceeds 1 MiB"}})
                continue
            if len(line) > MAX_BODY + 1:
                send({"id": None, "ok": False, "error": {"code": "too_large", "message": "request line exceeds 1 MiB"}})
                continue
            resp, stop = self.handle(line.rstrip(b"\r\n"))
            send(resp)
            if stop:
                return 0


def main(argv=None):
    p = argparse.ArgumentParser(prog="python -m backend.netlink", description=__doc__,
                                formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--timeout", type=float, default=15, help="per-request network timeout in seconds (default 15)")
    a = p.parse_args(argv)
    if not 1 <= a.timeout <= 120:
        p.error("--timeout must be between 1 and 120")
    return Bridge(a.timeout).run(sys.stdin.buffer, sys.stdout.buffer)


if __name__ == "__main__":
    sys.exit(main())
