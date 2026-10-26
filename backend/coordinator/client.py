"""Reusable authenticated protocol-v1 client (the future bundled link process builds on this).

Addressing: the pinned certificate is the identity, an address is only a route. A client holds an ordered endpoint list (last
working first, then the credentials' own URL, then known extras). Fail-over happens ONLY while connecting / in the TLS
handshake / on a pin mismatch, i.e. before a single request byte is sent; once the request is sent nothing is retried (a
mutation is never replayed). The leaf SHA-256 is compared right after every handshake, before the headers go out."""
import hmac
import http.client
import ipaddress
import json
import os
import socket
import ssl
import threading
import time

from . import PROTOCOL, pki
from .schema import MAX_BODY, canonical

PER_ENDPOINT = 3  # seconds for connect + handshake when there is more than one address to try


class BadReply(ValueError):
    """The peer answered with something that is not a protocol-v1 reply (non-UTF-8, not JSON, wrong shape)."""


class CoordinatorError(Exception):
    def __init__(self, status, code, message):
        super().__init__(f"{status} {code}: {message}")
        self.status, self.code = status, code


def _is_ip(host):
    try:
        ipaddress.ip_address(host)
        return True
    except ValueError:
        return False


def fetch_cert(host, port, timeout, loopback=False):
    """Handshake only (nothing is sent): (pem, sha256) of the certificate presented at host:port. The TLS handshake itself proves
    the peer holds the matching private key; trust is decided by the caller comparing the fingerprint with a pin."""
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname, ctx.verify_mode, ctx.minimum_version = False, ssl.CERT_NONE, ssl.TLSVersion.TLSv1_2
    raw = socket.create_connection(("127.0.0.1" if loopback else host, port), timeout)
    try:
        with ctx.wrap_socket(raw, server_hostname=None if _is_ip(host) else host) as s:
            der = s.getpeercert(binary_form=True)
    except BaseException:
        raw.close()
        raise
    if not der:
        raise ssl.SSLError("no certificate presented")
    pem = ssl.DER_cert_to_PEM_cert(der)
    return pem, pki.fingerprint(pem)


def fetch_pinned_cert(urls, pin, timeout):
    """First address in `urls` presenting the pinned certificate (any certificate when `pin` is None: trust on first use, as a
    typed address always was). -> (pem, url). A different certificate at an address is skipped, never trusted."""
    end, wrong, last = time.monotonic() + timeout, False, None
    for u in urls:
        left = end - time.monotonic()
        if left <= 0:
            break
        host, port = pki.endpoint(u)
        try:
            pem, fp = fetch_cert(host, port, min(left, PER_ENDPOINT if len(urls) > 1 else left))
        except (OSError, ValueError) as e:
            last = e
            continue
        if pin is None or hmac.compare_digest(fp, pin.lower()):
            return pem, u
        wrong = True
    if wrong:
        raise pki.SecurityError("an address answered with a different certificate than the one selected; not trusting it")
    raise CoordinatorError(0, "transport", f"could not reach any address ({type(last).__name__ if last else 'timeout'})")


def exchange(urls, ctx, fp, method, path, data, headers, timeout, loopback=False):
    """One request with remaining-budget socket timeouts. -> (url, status, body).
    Buffered header/body reads can exceed the budget if a peer continuously trickles bytes.
    Only connect / TLS / pin failures move on to the next address (nothing was sent yet)."""
    end = time.monotonic() + timeout
    changed, last = False, None
    for u in urls:
        left = end - time.monotonic()
        if left <= 0:
            break
        host, port = pki.endpoint(u)
        try:
            raw = socket.create_connection(("127.0.0.1" if loopback else host, port), min(left, PER_ENDPOINT) if len(urls) > 1 else left)
            try:
                sock = ctx.wrap_socket(raw, server_hostname=None if _is_ip(host) else host)
            except BaseException:
                raw.close()
                raise
            try:
                pki.check_pin(sock, fp)
            except BaseException:
                sock.close()
                raise
        except pki.SecurityError as e:
            changed, last = True, e
            continue
        except ssl.SSLCertVerificationError as e:
            changed, last = True, e
            continue
        except (OSError, ValueError) as e:
            last = e
            continue
        conn = http.client.HTTPConnection(host, port)
        conn.sock = sock  # request bytes start here: from now on nothing is retried
        try:
            sock.settimeout(max(0.05, end - time.monotonic()))
            conn.request(method, path, data, headers)
            r = conn.getresponse()
            body = bytearray()
            while len(body) <= MAX_BODY and not r.isclosed():
                sock.settimeout(max(0.05, end - time.monotonic()))
                if time.monotonic() >= end:
                    raise socket.timeout("call deadline")
                chunk = r.read(min(65536, MAX_BODY + 1 - len(body)))
                if not chunk:
                    break
                body += chunk
            return u, r.status, bytes(body)
        finally:
            conn.close()
    if changed:
        raise pki.SecurityError("no address presented the pinned certificate (changed certificate or a different PC); nothing was sent")
    if last is not None:
        raise last
    raise socket.timeout("no address answered in time")


class Client:
    def __init__(self, creds, timeout=15, *, loopback=False, endpoints=None):
        """`creds` is a dict from pki.read_credentials. Trust is re-verified on construction. `endpoints` are extra https
        addresses of the SAME pinned certificate (never a different identity)."""
        self.host, self.port, self.ctx = pki.verified_trust(creds["url"], creds["certificate_pem"], creds["fingerprint_sha256"])
        self.fp = creds["fingerprint_sha256"].lower()
        self.bearer = f"{creds['installation_id']}.{creds['token']}"
        self.timeout = timeout
        self.loopback = loopback
        self.url = pki.url_for(self.host, self.port)
        self.last = None  # last working address: tried first next time
        self._lock = threading.Lock()
        self._known = [self.url]
        self.set_endpoints(endpoints or [])

    def set_endpoints(self, urls):
        """Atomically replace the extra addresses; token, identity and the credentials' own URL are untouched."""
        extra = [u for u in pki.clean_endpoints(urls, explicit=[self.url]) if u != self.url]
        with self._lock:
            self._known = [self.url, *extra]
            if self.last not in self._known:
                self.last = None

    def prefer(self, url):
        """The user's pick is tried first; a failure still falls back to the other addresses."""
        c = pki.clean_endpoints([url], explicit=[self.url])
        with self._lock:
            if c and c[0] in self._known:
                # ponytail: once the preferred address fails, `last` moves to the fallback until the next endpoints op re-sends the pick.
                self.last = c[0]

    @property
    def endpoints(self):
        with self._lock:
            return list(self._known)

    def _order(self):
        with self._lock:
            return [self.last, *[u for u in self._known if u != self.last]] if self.last else list(self._known)

    def _call(self, method, path, body=None, auth=True):
        data = canonical(body).encode() if body is not None else None
        headers = {"Connection": "close"}
        if data is not None:
            headers["Content-Type"] = "application/json"
        if auth:
            headers["Authorization"] = f"Bearer {self.bearer}"
        urls = [self.url] if self.loopback else self._order()
        url, status, raw = exchange(urls, self.ctx, self.fp, method, path, data, headers, self.timeout, self.loopback)
        if not self.loopback:
            self.last = url
        if len(raw) > MAX_BODY:
            raise CoordinatorError(status, "too_large", "response too large")
        try:
            out = json.loads(raw.decode("utf-8"))
        except ValueError:  # UnicodeDecodeError and JSONDecodeError
            raise BadReply("reply is not UTF-8 JSON") from None
        if not isinstance(out, dict):
            raise BadReply("reply is not a JSON object")
        if status != 200:
            e = out.get("error")
            if not isinstance(e, dict) or not isinstance(e.get("code"), str) or not isinstance(e.get("message", ""), str):
                raise BadReply("error reply has the wrong shape")
            raise CoordinatorError(status, e["code"][:64], e.get("message", "")[:500])
        return out

    def hello(self):
        """Unauthenticated identity of the host this certificate belongs to; its fingerprint must be the pinned one."""
        out = self._call("GET", "/v1/hello", auth=False)
        if not isinstance(out.get("fingerprint_sha256"), str) or not hmac.compare_digest(out["fingerprint_sha256"].lower(), self.fp):
            raise BadReply("hello does not carry the pinned fingerprint")
        return out

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
    def installations(self): return self._call("GET", "/v1/installations")
    def leave(self): return self._call("POST", "/v1/leave", {})


def _store_new(credentials_path, issue):
    """Reserve a private destination, run `issue()` -> credentials dict, then store. Order matters: the destination is reserved first."""
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


def join_to_file(url, credentials_path, name, path="/v1/join", extra=None, pem=None, timeout=15, existing=None, loopback=False,
                 fingerprint_sha256=None, endpoints=None):
    """Join by address without an invitation. With `pem=None` the certificate is fetched ONCE from the address (or, with
    `fingerprint_sha256`, from the first of `url` + `endpoints` presenting exactly that certificate) and pinned; a different
    certificate at an address is never trusted. The join call itself, like every later call, is a fully verified TLS
    connection to exactly that certificate (pin compared again before any byte is sent). With `pem` (the local service's own
    certificate file) nothing is fetched. `existing` is a credentials file: if it pins the SAME certificate nothing is redeemed
    and its installation is reused. The stored credentials name `url`, not whichever address failed over. Returns
    (installation_id, fingerprint, reused)."""
    host, port = pki.endpoint(url)
    first = pki.url_for(host, port)
    urls = [first, *[u for u in pki.clean_endpoints(endpoints or [], explicit=[url]) if u != first]]
    chosen = first
    pin = fingerprint_sha256.lower() if fingerprint_sha256 else None
    if pem is None:
        try:
            pem, chosen = fetch_pinned_cert(urls, pin, timeout)  # handshake only: no request, no secret is sent
        except (OSError, ValueError) as e:
            raise CoordinatorError(0, "transport", f"could not reach {host}:{port} ({type(e).__name__})") from None
    fp = pki.fingerprint(pem)
    if pin and not hmac.compare_digest(fp, pin):
        raise pki.SecurityError("the certificate does not match the one selected; not trusting it")
    if existing is not None:
        have = pki.read_credentials(existing)
        if hmac.compare_digest(have["fingerprint_sha256"].lower(), fp):
            return have["installation_id"], fp, True
    base = {"url": first, "certificate_pem": pem, "fingerprint_sha256": fp}

    def issue():
        c = Client({**base, "installation_id": "-", "token": "-"}, timeout, loopback=loopback, endpoints=urls)
        c.last = chosen
        out = c._call("POST", path, {"protocol": PROTOCOL, "name": name, **(extra or {})}, auth=False)
        if not pki.valid_installation_id(out.get("installation_id")) or not pki.valid_token(out.get("token")):
            raise BadReply("join reply carries unusable credentials")
        return {**base, "installation_id": out["installation_id"], "token": out["token"]}
    return _store_new(credentials_path, issue), fp, False
