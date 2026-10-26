"""HTTPS API (protocol v1) on stdlib http.server: bounded bodies, exact Content-Length framing, one request per
connection, finite timeouts, bounded concurrent handlers, throttled bad auth. No long-polling: every call returns."""
import hmac
import ipaddress
import json
import re
import socket
import socketserver
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

from . import PROTOCOL, pki
from .schema import ApiError, MAX_BODY, bad, obj, parse_json
from .store import Store

IO_TIMEOUT = 15
BODY_DEADLINE = 30
MAX_CONNECTIONS = 32
CONN_DEADLINE = 30  # absolute seconds per connection: TLS handshake + headers + body + reply


class Limiter:
    """Failures per key inside a sliding window; in-memory (a restart forgives, which is acceptable on a LAN)."""

    def __init__(self, limit, window, clock=time.monotonic):
        self.limit, self.window, self.clock, self.hits, self.lock = limit, window, clock, {}, threading.Lock()

    def _recent(self, key):
        now = self.clock()
        return [t for t in self.hits.get(key, ()) if now - t < self.window]

    def blocked(self, key):
        with self.lock:
            return len(self._recent(key)) >= self.limit

    def fail(self, key):
        with self.lock:
            self.hits[key] = self._recent(key) + [self.clock()]
            if len(self.hits) > 4096:  # bound memory under a spray of source addresses
                self.hits = {k: v for k, v in self.hits.items() if self._recent(k)}


def trusted_peer(ip, loopback_only=False):
    """Unauthenticated joins are limited to this machine, private/link-local LAN ranges and Tailscale (100.64/10, fd7a::/48 is ULA)."""
    if pki.trusted_address(ip, allow_loopback=True) and not loopback_only:
        return True
    try:
        a = ipaddress.ip_address(str(ip).split("%")[0])
    except ValueError:
        return False
    return (getattr(a, "ipv4_mapped", None) or a).is_loopback


_POST = {
    r"/v1/join": "join", r"/v1/local-join": "local_join", r"/v1/local-identity": "local_identity", r"/v1/session": "session", r"/v1/stats": "stats", r"/v1/claim": "claim", r"/v1/jobs": "submit",
    r"/v1/attempts/([\w-]{1,80})/(reject|start|events)": "attempt", r"/v1/jobs/([\w-]{1,80})/cancel": "cancel",
}
_GET = {r"/v1/hello": "hello", r"/v1/installations": "installations", r"/v1/jobs/([\w-]{1,80})": "job", r"/v1/jobs/([\w-]{1,80})/events": "job_events"}


class Api:
    """Transport-independent routing so the HTTP layer stays dumb."""

    def __init__(self, store, throttle=None, join_enabled=False, local_token=None, advertise_url=None, info=None, local_identity=None):
        self.store = store
        self.info = info  # () -> {fingerprint_sha256, network_id, name, host_installation_id, receiver_online, endpoints[{url,kind}]}
        self.local_identity = local_identity  # (installation_id) -> None: records which installation is THIS PC (private file)
        self.hello_try = Limiter(60, 60)
        self.hello_global = Limiter(600, 60)
        self.auth_fail = throttle or Limiter(10, 60)
        self.join_enabled, self.local_token, self.advertise_url = join_enabled, local_token, advertise_url
        self.join_try = Limiter(10, 300)  # every attempt counts, successful or not
        self.join_global = Limiter(30, 300)

    def handle(self, method, target, bearer, body, peer):
        u = urlsplit(target)
        table = _POST if method == "POST" else _GET
        for pat, name in table.items():
            m = re.fullmatch(pat, u.path)
            if m:
                break
        else:
            raise ApiError(404, "not_found", "no such endpoint")
        if name == "hello":
            return self._hello(peer)
        if name == "local_identity":
            return self._local_identity(parse_json(body), peer)
        if name in ("join", "local_join"):
            return self._join(name, parse_json(body), peer)
        if self.auth_fail.blocked(peer):
            raise ApiError(429, "throttled", "too many failed authentication attempts")
        try:
            inst = self.store.authenticate(bearer or "")
        except ApiError:
            self.auth_fail.fail(peer)
            raise
        s = self.store
        if method == "GET":
            if name == "installations":
                return s.installations(inst)
            if name == "job":
                return s.job_view(inst, m[1])
            try:
                q = parse_qs(u.query, keep_blank_values=True, strict_parsing=bool(u.query))
                if any(len(v) != 1 for v in q.values()):
                    bad("each query parameter may appear once")
                after, limit = int(q.get("after", ["0"])[0]), int(q.get("limit", ["100"])[0])
            except ValueError:
                bad("malformed query string (after/limit must be integers)")
            if set(q) - {"after", "limit"}:
                bad("unknown query parameter")
            return s.job_events(inst, m[1], after, limit)
        b = parse_json(body)
        if name == "session":
            return s.register(inst, b)
        if name == "stats":
            return s.report_stats(inst, b)
        if name == "claim":
            return s.claim(inst, b)
        if name == "submit":
            return s.submit(inst, b)
        if name == "cancel":
            obj(b, "cancel", ())
            return s.cancel(inst, m[1])
        return {"reject": s.reject, "start": s.start, "events": s.report}[m[2]](inst, m[1], b)

    def _hello(self, peer):
        """Unauthenticated, read-only, secret-free identity of this host (peer certificate fingerprint, PC name, whether the
        PC's own app session is online, whether address join is on, its addresses). Rate-capped per peer and globally."""
        if self.hello_try.blocked(peer) or self.hello_global.blocked("*"):
            raise ApiError(429, "throttled", "too many hello requests")
        self.hello_try.fail(peer)
        self.hello_global.fail("*")
        i = (self.info() if self.info else None) or {}
        iid = i.get("host_installation_id")
        return {"protocol": PROTOCOL, "fingerprint_sha256": i.get("fingerprint_sha256"), "network_id": i.get("network_id"),
                "name": i.get("name"), "host_installation_id": iid, "receiver_online": bool(i.get("receiver_online")),
                "join": bool(self.join_enabled), "endpoints": list(i.get("endpoints") or [])[:16]}

    def _local_identity(self, b, peer):
        """Loopback + the service's private token: tell the host which EXISTING installation is this PC (legacy credentials
        reused instead of joining again). Mints nothing and returns no secret."""
        if not self.local_token or not self.local_identity:
            raise ApiError(404, "not_found", "no such endpoint")
        if not trusted_peer(peer, loopback_only=True):
            raise ApiError(403, "join_refused", "this is limited to this PC")
        if self.join_try.blocked(peer):
            raise ApiError(429, "throttled", "too many attempts")
        self.join_try.fail(peer)
        obj(b, "local identity", ("protocol", "token", "installation_id"))
        if b["protocol"] != PROTOCOL or isinstance(b["protocol"], bool):
            bad("unsupported protocol", "unsupported_protocol")
        if not (isinstance(b["token"], str) and hmac.compare_digest(b["token"].encode(), self.local_token.encode())):
            raise ApiError(403, "join_refused", "local join token rejected")
        iid = b["installation_id"]
        if not pki.valid_installation_id(iid) or not self.store.installation_active(iid):
            raise ApiError(404, "unknown_installation", "no such installation")
        self.local_identity(iid)
        return {"host_installation_id": iid}

    def _join(self, name, b, peer):
        """/v1/join (explicitly enabled) or /v1/local-join (loopback + the service's private token). Both mint a unique
        non-admin installation; neither gives access to jobs without the token they return."""
        local = name == "local_join"
        if not (self.local_token if local else self.join_enabled):
            raise ApiError(404, "not_found", "no such endpoint")  # disabled unless configured
        if not trusted_peer(peer, loopback_only=local):
            raise ApiError(403, "join_refused", "joining is limited to local, private-network and Tailscale peers")
        if self.join_try.blocked(peer) or (not local and self.join_global.blocked("*")):
            raise ApiError(429, "throttled", "too many join attempts")
        self.join_try.fail(peer)
        if not local:
            self.join_global.fail("*")
        obj(b, "join", ("protocol", "name", "token") if local else ("protocol", "name"))
        if b["protocol"] != PROTOCOL or isinstance(b["protocol"], bool):
            bad("unsupported protocol", "unsupported_protocol")
        if local and not (isinstance(b["token"], str) and hmac.compare_digest(b["token"].encode(), self.local_token.encode())):
            raise ApiError(403, "join_refused", "local join token rejected")
        iid, token = self.store.join(b["name"])
        if local and self.local_identity:
            try:
                self.local_identity(iid)
            except OSError:
                pass  # the join stands; status will simply not know this PC's installation yet
        return {"installation_id": iid, "token": token}


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "KeyloomCoordinator"
    sys_version = ""

    def setup(self):
        self.request.settimeout(IO_TIMEOUT)
        self.request.do_handshake()  # in the handler thread, so a stalled TLS client cannot block accept()
        super().setup()

    def log_message(self, *a):  # request lines can carry ids; the server logs nothing per request
        pass

    def _reply(self, status, payload):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(data)
        self.close_connection = True

    def _body(self):
        if self.headers.get("Transfer-Encoding") is not None:
            raise ApiError(400, "bad_framing", "Transfer-Encoding is not supported")
        lengths = self.headers.get_all("Content-Length") or []
        if len(lengths) > 1 or (lengths and not re.fullmatch(r"[0-9]{1,8}", lengths[0].strip())):
            raise ApiError(400, "bad_framing", "invalid Content-Length")
        n = int(lengths[0]) if lengths else None
        if self.command == "GET":
            if n:
                raise ApiError(400, "bad_framing", "GET must not carry a body")
            return b""
        if n is None:
            raise ApiError(411, "length_required", "Content-Length is required")
        if n > MAX_BODY:
            raise ApiError(413, "too_large", "body too large")
        if (self.headers.get("Content-Type") or "").split(";")[0].strip().lower() != "application/json":
            raise ApiError(415, "unsupported_media_type", "Content-Type must be application/json")
        buf, deadline = b"", time.monotonic() + BODY_DEADLINE
        while len(buf) < n:
            if time.monotonic() > deadline:
                raise ApiError(408, "timeout", "body too slow")
            chunk = self.rfile.read(min(65536, n - len(buf)))
            if not chunk:
                raise ApiError(400, "bad_framing", "body shorter than Content-Length")
            buf += chunk
        return buf

    def _run(self):
        try:
            auth = self.headers.get("Authorization") or ""
            bearer = auth[7:] if auth.startswith("Bearer ") else None
            status, payload = 200, self.server.api.handle(self.command, self.path, bearer, self._body(), self.client_address[0])
        except ApiError as e:
            status, payload = e.status, {"error": {"code": e.code, "message": e.message}}
        except Exception as e:  # never echo internals to the peer or the console
            status, payload = 500, {"error": {"code": "internal", "message": type(e).__name__}}
        self._reply(status, payload)

    do_GET = do_POST = _run

    def do_PUT(self):
        self._reply(405, {"error": {"code": "method_not_allowed", "message": "GET and POST only"}})

    do_DELETE = do_PATCH = do_PUT


class _Deadline:
    """Hard per-connection cut-off. On expiry the raw socket is shut down (not closed), so a handler blocked in
    handshake/recv/send fails promptly and its own cleanup frees the slot. `done` + lock stop a late timer from
    touching a socket the handler already finished with."""

    def __init__(self, sock, seconds):
        self.sock, self.done, self.lock = sock, False, threading.Lock()
        self.timer = threading.Timer(seconds, self._fire)
        self.timer.daemon = True
        self.timer.start()

    def _fire(self):
        with self.lock:
            if not self.done:
                try:
                    socket.socket.shutdown(self.sock, socket.SHUT_RDWR)  # bypass SSLSocket.shutdown bookkeeping
                except OSError:
                    pass

    def cancel(self):
        with self.lock:
            self.done = True
        self.timer.cancel()


class CoordinatorServer(ThreadingHTTPServer):
    daemon_threads = True
    request_queue_size = 64

    def __init__(self, addr, ssl_context, store: Store, conn_deadline=CONN_DEADLINE, max_connections=MAX_CONNECTIONS, **api_opts):
        self.api = Api(store, **api_opts)
        self.conn_deadline = conn_deadline
        self.slots = threading.BoundedSemaphore(max_connections)
        self._guards = {}
        if addr[0] == "::":  # dual-stack wildcard: IPv4 and IPv6 on one socket; the caller falls back to IPv4 on OSError
            self.address_family = socket.AF_INET6
        super().__init__(addr, _Handler)
        self.socket = ssl_context.wrap_socket(self.socket, server_side=True, do_handshake_on_connect=False)

    def server_bind(self):
        if self.address_family == socket.AF_INET6:
            try:
                self.socket.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
            except (OSError, AttributeError):
                raise OSError("dual-stack sockets are not available") from None
        socketserver.TCPServer.server_bind(self)  # not HTTPServer's: its getfqdn() can block on DNS
        self.server_name, self.server_port = "keyloom", self.server_address[1]

    def process_request(self, request, client_address):
        if not self.slots.acquire(blocking=False):
            self.shutdown_request(request)  # over capacity: drop instead of queueing unbounded threads
            return
        self._guards[id(request)] = _Deadline(request, self.conn_deadline)
        try:
            super().process_request(request, client_address)
        except BaseException:  # the handler thread could not start
            self._cancel(request)
            self.slots.release()
            raise

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            self.slots.release()

    def _cancel(self, request):
        g = self._guards.pop(id(request), None)
        if g:
            g.cancel()

    def shutdown_request(self, request):
        self._cancel(request)  # strictly before the socket is closed, so the timer can never hit a recycled descriptor
        super().shutdown_request(request)

    def handle_error(self, request, client_address):  # failed handshakes/resets are routine; no stack traces
        pass
