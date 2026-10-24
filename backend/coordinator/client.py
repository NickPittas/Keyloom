"""Reusable authenticated protocol-v1 client (the future bundled link process builds on this)."""
import hmac
import http.client
import json
import os
import socket
import ssl
import time

from . import PROTOCOL, pki
from .schema import MAX_BODY, canonical


class BadReply(ValueError):
    """The peer answered with something that is not a protocol-v1 reply (non-UTF-8, not JSON, wrong shape)."""


class CoordinatorError(Exception):
    def __init__(self, status, code, message):
        super().__init__(f"{status} {code}: {message}")
        self.status, self.code = status, code


class Client:
    def __init__(self, creds, timeout=15, *, loopback=False):
        """`creds` is a dict from pki.read_credentials. Trust is re-verified on construction."""
        self.host, self.port, self.ctx = pki.verified_trust(creds["url"], creds["certificate_pem"], creds["fingerprint_sha256"])
        self.bearer = f"{creds['installation_id']}.{creds['token']}"
        self.timeout = timeout
        self.loopback = loopback

    def _call(self, method, path, body=None, auth=True):
        conn = http.client.HTTPSConnection(self.host, self.port, context=self.ctx, timeout=self.timeout)
        if self.loopback:
            # Route local joining over loopback while TLS still verifies the certificate's advertised name.
            conn._create_connection = lambda addr, timeout, source_address=None: socket.create_connection(("127.0.0.1", addr[1]), timeout, source_address)
        try:
            data = canonical(body).encode() if body is not None else None
            headers = {"Connection": "close"}
            if data is not None:
                headers["Content-Type"] = "application/json"
            if auth:
                headers["Authorization"] = f"Bearer {self.bearer}"
            conn.request(method, path, data, headers)
            r = conn.getresponse()
            raw = r.read(MAX_BODY + 1)
        finally:
            conn.close()
        if len(raw) > MAX_BODY:
            raise CoordinatorError(r.status, "too_large", "response too large")
        try:
            out = json.loads(raw.decode("utf-8"))
        except ValueError:  # UnicodeDecodeError and JSONDecodeError
            raise BadReply("reply is not UTF-8 JSON") from None
        if not isinstance(out, dict):
            raise BadReply("reply is not a JSON object")
        if r.status != 200:
            e = out.get("error")
            if not isinstance(e, dict) or not isinstance(e.get("code"), str) or not isinstance(e.get("message", ""), str):
                raise BadReply("error reply has the wrong shape")
            raise CoordinatorError(r.status, e["code"][:64], e.get("message", "")[:500])
        return out

    @classmethod
    def pair(cls, invitation, name):
        """Redeem an invitation dict; returns the new credentials dict (caller stores it privately)."""
        c = cls({"url": invitation["url"], "certificate_pem": invitation["certificate_pem"],
                 "fingerprint_sha256": invitation["fingerprint_sha256"], "installation_id": "-", "token": "-"})
        out = c._call("POST", "/v1/pair", {"protocol": PROTOCOL, "code": invitation["code"], "name": name}, auth=False)
        if not pki.valid_installation_id(out.get("installation_id")) or not pki.valid_token(out.get("token")):
            raise BadReply("pairing reply carries unusable credentials")  # never reported as a success
        return {"version": 1, "url": invitation["url"], "certificate_pem": invitation["certificate_pem"],
                "fingerprint_sha256": invitation["fingerprint_sha256"],
                "installation_id": out["installation_id"], "token": out["token"]}

    def register(self, body): return self._call("POST", "/v1/session", body)
    def submit(self, submission_id, spec, target=None): return self._call("POST", "/v1/jobs", {"submission_id": submission_id, "spec": spec, "target": target})
    def claim(self, body): return self._call("POST", "/v1/claim", body)
    def reject(self, attempt_id, body): return self._call("POST", f"/v1/attempts/{attempt_id}/reject", body)
    def start(self, attempt_id, body): return self._call("POST", f"/v1/attempts/{attempt_id}/start", body)
    def report(self, attempt_id, body): return self._call("POST", f"/v1/attempts/{attempt_id}/events", body)
    def cancel(self, job_id): return self._call("POST", f"/v1/jobs/{job_id}/cancel", {})
    def job(self, job_id): return self._call("GET", f"/v1/jobs/{job_id}")
    def job_events(self, job_id, after=0, limit=100): return self._call("GET", f"/v1/jobs/{job_id}/events?after={after}&limit={limit}")
    def stats(self, body): return self._call("POST", "/v1/stats", body)
    def invite(self): return self._call("POST", "/v1/invitations", {})
    def installations(self): return self._call("GET", "/v1/installations")


def _store_new(credentials_path, issue):
    """Reserve a private destination, run `issue()` -> credentials dict, then store. Order matters (see redeem_to_file)."""
    pki.reserve_private(credentials_path)
    try:
        creds = issue()
    except ssl.SSLCertVerificationError as e:  # failed in the handshake: nothing was sent
        pki.release_reserved(credentials_path)
        raise pki.SecurityError(f"server certificate rejected ({e.verify_message}); nothing was sent") from None
    except (OSError, http.client.HTTPException, ValueError) as e:  # SSLError/timeouts/resets/garbled reply
        pki.release_reserved(credentials_path)
        raise pki.PairingUncertain(type(e).__name__) from None
    except BaseException:  # CoordinatorError (definite refusal), SecurityError (pre-send trust checks)
        pki.release_reserved(credentials_path)
        raise
    try:
        pki.fill_reserved(credentials_path, pki.credentials_json(creds["url"], creds["certificate_pem"], creds["fingerprint_sha256"],
                                                                 creds["installation_id"], creds["token"]))
    except BaseException as e:
        try:
            os.unlink(credentials_path)  # our own reservation: never leave a half-written secret
        except OSError:
            pass
        raise pki.PairedNotSaved(creds["installation_id"], type(e).__name__) from None
    return creds["installation_id"]


def redeem_to_file(inv, credentials_path, name):
    """The one pairing routine (CLI and bridge). Order matters: reserve a private destination first (exclusive, protected,
    verified), redeem the one-use code, then store. Returns installation_id. Raises SecurityError subclasses:
    PairedNotSaved (code spent, token lost) and PairingUncertain (transport failure; code MAY be spent). Never retries."""
    if inv["expires_at"] < time.time():
        raise pki.SecurityError("invitation has expired; ask the server owner for a new one")
    return _store_new(credentials_path, lambda: Client.pair(inv, name))


def join_to_file(url, credentials_path, name, path="/v1/join", extra=None, pem=None, timeout=15, existing=None, loopback=False):
    """Join by address without an invitation. With `pem=None` the certificate is fetched ONCE from the address the user
    typed (trust on first use) and pinned: it must cover that address, and the join call itself, like every later call, is a
    fully verified TLS connection to exactly that certificate. Verification is never disabled for any request that carries
    data. With `pem` (the local service's own certificate file) nothing is fetched. `existing` is a credentials file: if it
    pins the SAME certificate (same network) nothing is redeemed and its installation is reused. Returns
    (installation_id, fingerprint, reused)."""
    host, port = pki.endpoint(url)
    if pem is None:
        try:
            pem = ssl.get_server_certificate((host, port), timeout=timeout)  # handshake only: no request, no secret is sent
        except (OSError, ValueError) as e:
            raise CoordinatorError(0, "transport", f"could not reach {host}:{port} ({type(e).__name__})") from None
        if not pki.cert_covers_host(pem, host):
            raise pki.SecurityError(f"the certificate presented by {host} does not cover that address; not trusting it")
    fp = pki.fingerprint(pem)
    if existing is not None:
        have = pki.read_credentials(existing)
        if hmac.compare_digest(have["fingerprint_sha256"].lower(), fp):
            return have["installation_id"], fp, True
    base = {"url": f"https://{host if ':' not in host else '[' + host + ']'}:{port}", "certificate_pem": pem, "fingerprint_sha256": fp}

    def issue():
        c = Client({**base, "installation_id": "-", "token": "-"}, timeout, loopback=loopback)
        out = c._call("POST", path, {"protocol": PROTOCOL, "name": name, **(extra or {})}, auth=False)
        if not pki.valid_installation_id(out.get("installation_id")) or not pki.valid_token(out.get("token")):
            raise BadReply("join reply carries unusable credentials")
        return {**base, "installation_id": out["installation_id"], "token": out["token"]}
    return _store_new(credentials_path, issue), fp, False
