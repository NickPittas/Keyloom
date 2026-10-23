"""HTTPS API (protocol v1) on stdlib http.server: bounded bodies, exact Content-Length framing, one request per
connection, finite timeouts, bounded concurrent handlers, throttled bad auth. No long-polling: every call returns."""
import json
import re
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.parse import parse_qs, urlsplit

from . import PROTOCOL
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


_POST = {
    r"/v1/pair": "pair", r"/v1/session": "session", r"/v1/stats": "stats", r"/v1/claim": "claim", r"/v1/jobs": "submit",
    r"/v1/attempts/([\w-]{1,80})/(reject|start|events)": "attempt", r"/v1/jobs/([\w-]{1,80})/cancel": "cancel",
}
_GET = {r"/v1/installations": "installations", r"/v1/jobs/([\w-]{1,80})": "job", r"/v1/jobs/([\w-]{1,80})/events": "job_events"}


class Api:
    """Transport-independent routing so the HTTP layer stays dumb."""

    def __init__(self, store, throttle=None):
        self.store = store
        self.auth_fail = throttle or Limiter(10, 60)
        self.pair_fail = Limiter(10, 60)
        self.pair_global = Limiter(30, 300)

    def handle(self, method, target, bearer, body, peer):
        u = urlsplit(target)
        table = _POST if method == "POST" else _GET
        for pat, name in table.items():
            m = re.fullmatch(pat, u.path)
            if m:
                break
        else:
            raise ApiError(404, "not_found", "no such endpoint")
        if name == "pair":
            return self._pair(parse_json(body), peer)
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

    def _pair(self, b, peer):
        if self.pair_fail.blocked(peer) or self.pair_global.blocked("*"):
            raise ApiError(429, "throttled", "too many pairing attempts")
        obj(b, "pair", ("protocol", "code", "name"))
        if b["protocol"] != PROTOCOL or isinstance(b["protocol"], bool):
            bad("unsupported protocol", "unsupported_protocol")
        try:
            iid, token = self.store.pair(b["code"], b["name"])
        except ApiError as e:
            if e.status == 403:
                self.pair_fail.fail(peer)
                self.pair_global.fail("*")
            raise
        return {"installation_id": iid, "token": token}


class _Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "CorridorKeyCoordinator"
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

    def __init__(self, addr, ssl_context, store: Store, conn_deadline=CONN_DEADLINE, max_connections=MAX_CONNECTIONS):
        self.api = Api(store)
        self.conn_deadline = conn_deadline
        self.slots = threading.BoundedSemaphore(max_connections)
        self._guards = {}
        super().__init__(addr, _Handler)
        self.socket = ssl_context.wrap_socket(self.socket, server_side=True, do_handshake_on_connect=False)

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
