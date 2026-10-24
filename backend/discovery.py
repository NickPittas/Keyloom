"""Finding Keyloom hosts on this PC's networks, and answering for this PC when it hosts.

Host side (`Responder`): a UDP listener on the FIXED port 44554 (independent of the TCP API port, which may be any of
8443..8452) answers a tiny probe with a unicast datagram <= 1200 bytes: fingerprint, name, host installation id, join flag, API
port and addresses. Replies are rate-limited per source and globally and never carry secrets.

Client side (`scan`): UDP broadcast on every up interface (Tailscale/ZeroTier included; IPv6 multicast where viable) for 3 s,
`tailscale status --json` (online peers, both families), `ip -j neigh` / `arp -a` (non-failed neighbours), the URLs the caller
already knows, and this PC's loopback. "self" means the certificate was verified over this PC's own loopback, never that an address collides with a local one. Datagrams are HINTS, never trust: every candidate is proven by a TLS handshake whose leaf
SHA-256 matches the advertised fingerprint and then a hello over a connection pinned to exactly that certificate. Candidates are
capped at 512, never come from subnet or public sweeps, and the scan returns by its hard deadline (default 20 s) no matter what
is still running; what could not be covered is reported in `sources` / `partial`, never silently as full coverage."""
import ipaddress
import json
import os
import re
import secrets
import select
import shutil
import socket
import struct
import subprocess
import threading
import time
from concurrent.futures import ThreadPoolExecutor, wait

from .coordinator import pki
from .coordinator.client import PER_ENDPOINT, exchange, fetch_cert
from .coordinator.server import Limiter

UDP_PORT = 44554
PORTS = tuple(range(8443, 8453))
MAX_CANDIDATES = 512
MAX_WORKERS = 32
MAX_DATAGRAM = 1200
_NOWIN = getattr(subprocess, "CREATE_NO_WINDOW", 0)
_KINDS = ("lan", "tailscale", "zerotier", "loopback", "name")


# ---- interfaces ------------------------------------------------------------------------------------

def interfaces():
    """[(name, address, netmask, broadcast)] of every UP interface address (psutil is installed with the app; if it is somehow
    missing the list is empty and scans/advertising degrade, they do not fail)."""
    try:
        import psutil
        stats, addrs = psutil.net_if_stats(), psutil.net_if_addrs()
    except Exception:
        return []
    out = []
    for name, lst in addrs.items():
        st = stats.get(name)
        if st is not None and not st.isup:
            continue
        for a in lst:
            if a.family in (socket.AF_INET, socket.AF_INET6) and a.address:
                out.append((name, a.address.split("%")[0], a.netmask, a.broadcast))
    return out


def local_addresses():
    return {a for _, a, _, _ in interfaces()} | {"127.0.0.1", "::1"}


# Container / VM bridges are private to this PC (every Docker host has the same 172.17.0.1): never advertised. Tailscale/ZeroTier stay.
_VIRTUAL = re.compile(r"(docker|br-|veth|virbr|vboxnet|vethernet)", re.I)


def local_endpoints(port):
    """This PC's reachable addresses as [{url, kind}] (<= 16): LAN first, then ZeroTier, then Tailscale; v4 before v6.
    Loopback, link-local and virtual-adapter (docker*, br-*, veth*, virbr*, vboxnet*, vEthernet*) addresses are not advertised."""
    rank = {"lan": 0, "zerotier": 1, "tailscale": 2}
    found = []
    for name, addr, _, _ in interfaces():
        if _VIRTUAL.match(name):
            continue
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if ip.is_loopback or ip.is_link_local or not pki.trusted_address(addr):
            continue
        kind = pki.address_kind(addr, name)
        found.append((rank.get(kind, 0), ip.version, pki.url_for(addr, port), kind))
    out, seen = [], set()
    for _, _, url, kind in sorted(found):
        if url not in seen:
            seen.add(url)
            out.append({"url": url, "kind": kind})
    return out[:16]


def san_hosts():
    """Cosmetic certificate names: loopback plus the addresses this PC has now."""
    hosts = ["127.0.0.1", "::1", "localhost"]
    for _, addr, _, _ in interfaces():
        try:
            ip = ipaddress.ip_address(addr)
        except ValueError:
            continue
        if not ip.is_link_local and not ip.is_multicast and addr not in hosts:
            hosts.append(addr)
    return hosts[:64]


class EndpointCache:
    """Interface enumeration, live, at most every `ttl` seconds."""

    def __init__(self, ttl=30):
        self.ttl, self.at, self.value, self.lock = ttl, 0.0, [], threading.Lock()

    def get(self, port):
        with self.lock:
            if time.monotonic() - self.at >= self.ttl or not self.at:
                try:
                    self.value = local_endpoints(port)
                except Exception:
                    pass
                self.at = time.monotonic()
            return list(self.value)


# ---- host side ---------------------------------------------------------------------------------------

def _clean_name(v):
    return "".join(c for c in v if c.isprintable())[:64] if isinstance(v, str) else ""


class Responder:
    """UDP discovery listener owned by the service process. start() never raises: it returns an error string (or None) and the
    service publishes `discovery: false`. stop() ends and joins the listener thread and closes its sockets."""

    def __init__(self, info, port):
        self.info, self.port = info, port  # info() -> hello-shaped dict
        self.socks, self.thread, self.stopping = [], None, threading.Event()
        self.src_limit, self.all_limit = Limiter(8, 10), Limiter(100, 10)

    def start(self):
        err = None
        try:
            s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
            if os.name != "nt":
                s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s.bind(("", UDP_PORT))
            self.socks.append(s)
        except OSError as e:
            err = f"UDP port {UDP_PORT} could not be opened ({type(e).__name__}); other PCs may not find this one automatically"
        try:
            s6 = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
            s6.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
            if os.name != "nt":
                s6.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            s6.bind(("::", UDP_PORT))
            for name in {n for n, a, _, _ in interfaces() if ":" in a}:  # best effort: all-nodes multicast per interface
                try:
                    s6.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_JOIN_GROUP,
                                  socket.inet_pton(socket.AF_INET6, "ff02::1") + struct.pack("@I", socket.if_nametoindex(name)))
                except (OSError, ValueError):
                    pass
            self.socks.append(s6)
        except (OSError, AttributeError):
            pass
        if not self.socks:
            return err
        self.thread = threading.Thread(target=self._loop, name="keyloom-discovery", daemon=True)
        self.thread.start()
        return None

    def stop(self):
        self.stopping.set()
        if self.thread:
            self.thread.join(3)
        for s in self.socks:
            try:
                s.close()
            except OSError:
                pass
        self.socks = []

    def _reply(self, nonce):
        i = self.info() or {}
        eps = list(i.get("endpoints") or [])[:16]
        while True:
            d = {"v": 1, "t": "reply", "n": nonce, "fingerprint_sha256": i.get("fingerprint_sha256"), "network_id": i.get("network_id"),
                 "name": i.get("name"), "host_installation_id": i.get("host_installation_id"), "join": bool(i.get("join")),
                 "port": self.port, "endpoints": eps}
            raw = json.dumps(d, separators=(",", ":")).encode()
            if len(raw) <= MAX_DATAGRAM:
                return raw
            if eps:
                eps.pop()
            else:
                i = {**i, "name": None}

    def _loop(self):
        while not self.stopping.is_set():
            try:
                ready, _, _ = select.select(self.socks, [], [], 0.5)
            except (OSError, ValueError):
                return
            for s in ready:
                try:
                    data, src = s.recvfrom(512)
                    ip = src[0].split("%")[0]
                    if len(data) > 256 or self.src_limit.blocked(ip) or self.all_limit.blocked("*"):
                        continue
                    self.src_limit.fail(ip)
                    self.all_limit.fail("*")
                    m = json.loads(data.decode("utf-8"))
                    n = m.get("n") if isinstance(m, dict) and m.get("v") == 1 and m.get("t") == "probe" else None
                    if isinstance(n, str) and re.fullmatch(r"[0-9a-f]{8,32}", n):
                        s.sendto(self._reply(n), src)
                except (OSError, ValueError, UnicodeDecodeError, TypeError, AttributeError):
                    continue  # a bad datagram or a send failure never stops the listener


# ---- client side -------------------------------------------------------------------------------------

def _err(e):
    if isinstance(e, pki.SecurityError):
        return "different certificate"
    if isinstance(e, ConnectionRefusedError):
        return "refused"
    if isinstance(e, (socket.timeout, TimeoutError)):
        return "timeout"
    if isinstance(e, OSError) and e.__class__.__name__ == "SSLCertVerificationError":
        return "certificate rejected"
    return type(e).__name__[:40]


def udp_probe(cancel, seconds=3.0):
    """-> ([(source_ip, reply dict)], interfaces probed). Broadcast on every up v4 interface, multicast on up v6 interfaces."""
    nonce = secrets.token_hex(8)
    msg = json.dumps({"v": 1, "t": "probe", "n": nonce}).encode()
    socks, targets, used = [], [], set()
    try:
        s4 = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s4.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        s4.bind(("", 0))
        socks.append(s4)
    except OSError:
        s4 = None
    try:
        s6 = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
        s6.bind(("::", 0))
        socks.append(s6)
    except OSError:
        s6 = None
    if s4:
        targets.append((s4, ("255.255.255.255", UDP_PORT), None))
    for name, addr, mask, bcast in interfaces():
        if ":" in addr:
            if s6 and not addr.lower().startswith("fe80") and name not in used:
                try:
                    targets.append((s6, ("ff02::1", UDP_PORT, 0, socket.if_nametoindex(name)), name))
                except OSError:
                    pass
            continue
        if not s4:
            continue
        try:
            b = bcast or str(ipaddress.ip_network(f"{addr}/{mask}", strict=False).broadcast_address)
            if ipaddress.ip_address(addr).is_loopback or (mask and ipaddress.ip_network(f"{addr}/{mask}", strict=False).prefixlen >= 31):
                continue
            targets.append((s4, (b, UDP_PORT), name))
        except (ValueError, TypeError):
            continue

    def send():
        for sock, dest, name in targets:
            try:
                sock.sendto(msg, dest)
                if name:
                    used.add(name)
            except OSError:
                pass
    replies, end = [], time.monotonic() + seconds
    try:
        send()
        resent = False
        while socks and not cancel.is_set():
            left = end - time.monotonic()
            if left <= 0:
                break
            if not resent and left < seconds - 1.0:
                resent = True
                send()
            ready, _, _ = select.select(socks, [], [], min(left, 0.25))
            for s in ready:
                try:
                    data, src = s.recvfrom(2048)
                    m = json.loads(data.decode("utf-8")) if len(data) <= MAX_DATAGRAM else None
                except (OSError, ValueError):
                    continue
                if isinstance(m, dict) and m.get("t") == "reply" and m.get("n") == nonce:
                    replies.append((src[0].split("%")[0], m))
    finally:
        for s in socks:
            s.close()
    return replies, len(used)


def _tailscale_exe():
    exe = shutil.which("tailscale")
    if exe:
        return exe
    cands = [os.path.join(os.environ.get(v, ""), "Tailscale", "tailscale.exe") for v in ("ProgramFiles", "ProgramFiles(x86)")] if os.name == "nt" \
        else ["/usr/bin/tailscale", "/usr/sbin/tailscale", "/usr/local/bin/tailscale", "/Applications/Tailscale.app/Contents/MacOS/Tailscale"]
    return next((c for c in cands if os.path.isfile(c)), None)


def tailscale_peers():
    """-> (online peer addresses, 'ok'|'absent'|'error')."""
    exe = _tailscale_exe()
    if not exe:
        return [], "absent"
    try:
        r = subprocess.run([exe, "status", "--json"], capture_output=True, timeout=3, creationflags=_NOWIN)
        d = json.loads(r.stdout.decode("utf-8", "replace"))
        if r.returncode or not isinstance(d, dict) or d.get("BackendState") not in (None, "Running"):
            return [], "error"
        out = []
        for p in (d.get("Peer") or {}).values():
            if isinstance(p, dict) and p.get("Online"):
                out += [a for a in p.get("TailscaleIPs") or [] if isinstance(a, str)]
        return out, "ok"
    except (OSError, ValueError, subprocess.SubprocessError, AttributeError):
        return [], "error"


def neighbours():
    """-> (non-failed neighbour addresses, 'ok'|'error'); the OS neighbour table only, nothing is swept."""
    try:
        ip = shutil.which("ip") if os.name != "nt" else None
        if ip:
            r = subprocess.run([ip, "-j", "neigh"], capture_output=True, timeout=3, creationflags=_NOWIN)
            if r.returncode == 0:
                return [n["dst"] for n in json.loads(r.stdout.decode("utf-8", "replace"))
                        if isinstance(n, dict) and isinstance(n.get("dst"), str) and n.get("lladdr")
                        and not {"FAILED", "INCOMPLETE"} & set(n.get("state") or [])], "ok"
        arp = shutil.which("arp")
        if not arp:
            return [], "error"
        r = subprocess.run([arp, "-a"], capture_output=True, timeout=3, creationflags=_NOWIN)
        if r.returncode:
            return [], "error"
        text = r.stdout.decode("utf-8", "replace")
        out = re.findall(r"\((\d{1,3}(?:\.\d{1,3}){3})\) at (?!<incomplete>)", text)
        out += re.findall(r"^\s*(\d{1,3}(?:\.\d{1,3}){3})\s+(?!ff-ff-ff-ff-ff-ff)[0-9a-f]{2}(?:-[0-9a-f]{2}){5}\s+(?:dynamic|static)", text, re.M | re.I)
        return out, "ok"
    except (OSError, ValueError, subprocess.SubprocessError, TypeError, KeyError):
        return [], "error"


def _valid_hello(h, fp):
    if not isinstance(h, dict) or not isinstance(h.get("fingerprint_sha256"), str) or h["fingerprint_sha256"].lower() != fp:
        return None
    iid = h.get("host_installation_id")
    return {"name": _clean_name(h.get("name")), "host_installation_id": iid if iid is not None and pki.valid_installation_id(iid) else None,
            "join": h.get("join") is True, "receiver_online": h.get("receiver_online") is True,
            "endpoints": [e for e in (h.get("endpoints") or []) if isinstance(e, dict) and isinstance(e.get("url"), str)][:16]
            if isinstance(h.get("endpoints"), list) else []}


_scan_lock = threading.Lock()
_current = None


def scan(timeout_s=20, known=()):
    """See the module docstring. Returns the `discover` reply; never raises for network problems."""
    global _current
    with _scan_lock:
        if _current is not None:
            _current.set()  # an older scan still running is cancelled: its workers stop at their next step
        _current = cancel = threading.Event()
    t0 = time.monotonic()
    total = max(3.0, min(float(timeout_s), 20.0))
    deadline = t0 + total
    left = lambda: deadline - time.monotonic()  # noqa: E731
    known = [pki.url_for(*pki.endpoint(u)) for u in known]
    explicit_hosts = {pki.endpoint(u)[0] for u in known}
    pool = ThreadPoolExecutor(MAX_WORKERS, thread_name_prefix="keyloom-scan")
    lock = threading.Lock()
    certs, pcs, probed = {}, {}, {}  # fp -> pem ; fp -> record ; url -> result
    try:
        # 1. hints (all three in parallel; each <= 3 s)
        f_udp, f_ts, f_nb = pool.submit(udp_probe, cancel), pool.submit(tailscale_peers), pool.submit(neighbours)
        wait([f_udp, f_ts, f_nb], timeout=max(0.1, min(4.5, left())))
        replies, nifaces = f_udp.result() if f_udp.done() and not f_udp.exception() else ([], 0)
        ts, ts_state = f_ts.result() if f_ts.done() and not f_ts.exception() else ([], "error")
        nb, nb_state = f_nb.result() if f_nb.done() and not f_nb.exception() else ([], "error")

        def kind_of(url):
            return pki.address_kind(pki.endpoint(url)[0])

        def probe(url, expect, tmo, refused_ok=False):
            """-> (fp, pem, hello, rtt_ms). Handshake fingerprint check, then a hello on a connection pinned to that cert."""
            if cancel.is_set():
                raise TimeoutError("cancelled")
            host, port = pki.endpoint(url)
            fp = pem = None
            if expect:
                with lock:
                    pem = certs.get(expect)
            if pem is None:
                pem, fp = fetch_cert(host, port, min(tmo, max(0.2, left())))
                if expect and fp != expect.lower():
                    raise pki.SecurityError("different certificate")
            fp = fp or expect.lower()
            with lock:
                certs[fp] = pem
            if cancel.is_set():
                raise TimeoutError("cancelled")
            t = time.monotonic()
            _, status, raw = exchange([url], pki.client_context(pem, fp), fp, "GET", "/v1/hello", None, {"Connection": "close"},
                                      min(PER_ENDPOINT, max(0.2, left())))
            rtt = round((time.monotonic() - t) * 1000)
            h = _valid_hello(json.loads(raw.decode("utf-8")) if status == 200 and len(raw) < 65536 else None, fp)
            if not h:
                raise ValueError("not a Keyloom host")
            return fp, pem, h, rtt

        def record(url, fp, h, rtt, kind=None):
            with lock:
                pc = pcs.setdefault(fp, {"fp": fp, "eps": {}, "advert": {}})
                pc.update(name=h["name"], iid=h["host_installation_id"], join=h["join"], online=h["receiver_online"])
                pc["eps"][url] = {"url": url, "kind": kind or kind_of(url), "reachable": True, "rtt_ms": rtt, "error": None}
                for e in h["endpoints"]:
                    for c in pki.clean_endpoints([e["url"]]):
                        pc["advert"].setdefault(c, e.get("kind") if e.get("kind") in _KINDS else kind_of(c))

        def probe_known(url, expect):
            try:
                fp, _, h, rtt = probe(url, expect, 2.0)
                record(url, fp, h, rtt)
            except Exception as e:  # noqa: BLE001 - an unreachable hint is simply not listed
                with lock:
                    probed[url] = _err(e)

        def probe_ip(host, ports):
            for port in ports:
                if cancel.is_set() or left() < 0.3:
                    return
                url = pki.url_for(host, port)
                try:
                    fp, _, h, rtt = probe(url, None, 1.0)
                    record(url, fp, h, rtt)
                    return
                except ConnectionRefusedError:
                    continue  # the host is up, this port is not ours: next port
                except (pki.SecurityError, ValueError):
                    continue
                except OSError as e:
                    if isinstance(e, (socket.timeout, TimeoutError)) or getattr(e, "errno", None) in (113, 101, 10060, 10065, 10051):
                        return  # silent / unreachable host: do not spend the remaining ports on it
                    continue

        # 2. verify advertised (UDP) endpoints, then fallback hello probes for the remaining candidate addresses
        futs, claimed_hosts = [], set()
        for src, m in replies[:64]:
            fp = m.get("fingerprint_sha256")
            if not (isinstance(fp, str) and re.fullmatch(r"[0-9a-f]{64}", fp.lower())):
                continue
            port = m.get("port") if isinstance(m.get("port"), int) and 1024 <= m["port"] <= 65535 else None
            urls = pki.clean_endpoints([e.get("url") for e in m.get("endpoints", []) if isinstance(e, dict)] if isinstance(m.get("endpoints"), list) else [])
            if port and pki.trusted_address(src):
                urls.insert(0, pki.url_for(src, port))
            for u in list(dict.fromkeys(urls))[:16]:
                claimed_hosts.add(pki.endpoint(u)[0])
                futs.append(pool.submit(probe_known, u, fp.lower()))
        for u in known:
            claimed_hosts.add(pki.endpoint(u)[0])
            futs.append(pool.submit(probe_known, u, None))
        cands = {}
        for h in ["127.0.0.1", *ts, *nb]:
            if h in claimed_hosts or h in cands:
                continue
            if h == "127.0.0.1" or pki.trusted_address(h):
                cands[h] = PORTS
        capped = len(cands) > MAX_CANDIDATES
        for h in list(cands)[:MAX_CANDIDATES]:
            futs.append(pool.submit(probe_ip, h, cands[h]))
        done, pending = wait(futs, timeout=max(0.1, min(left(), total * 0.75 - (time.monotonic() - t0))))
        partial = bool(pending) or capped

        # 3. verify every address each found PC advertises
        def _verify(url, fp):
            try:
                _, _, h, rtt = probe(url, fp, 2.0)
                record(url, fp, h, rtt, pcs[fp]["advert"].get(url))
            except Exception as e:  # noqa: BLE001
                with lock:
                    pcs[fp]["eps"][url] = {"url": url, "kind": pcs[fp]["advert"].get(url) or kind_of(url), "reachable": False,
                                           "rtt_ms": None, "error": _err(e)}
        with lock:
            todo = [(u, fp) for fp, pc in pcs.items() for u in pc["advert"] if u not in pc["eps"]][:256]
        vf = [pool.submit(lambda u=u, fp=fp: _verify(u, fp)) for u, fp in todo]

        _, pend2 = wait(vf, timeout=max(0.1, left()))
        partial = partial or bool(pend2)
    finally:
        cancel.set()  # running tasks stop at their next step; the pool is NOT waited for
        pool.shutdown(wait=False, cancel_futures=True)
    out = []
    with lock:
        for fp, pc in pcs.items():
            for u in pc["advert"]:
                pc["eps"].setdefault(u, {"url": u, "kind": pc["advert"][u], "reachable": False, "rtt_ms": None,
                                         "error": "not verified before the scan deadline"})
            eps = sorted(pc["eps"].values(), key=lambda e: (not e["reachable"], e["rtt_ms"] if e["rtt_ms"] is not None else 1 << 30))
            out.append({"network_id": pki.network_id(fp), "fingerprint_sha256": fp, "name": pc.get("name", ""),
                        "host_installation_id": pc.get("iid"), "join": pc.get("join", False), "receiver_online": pc.get("online", False),
                        "self": any(e["reachable"] and pki.endpoint(e["url"])[0] in ("127.0.0.1", "::1") for e in eps),
                        "endpoints": eps})
    out.sort(key=lambda p: (not p["self"], p["name"].lower(), p["fingerprint_sha256"]))
    return {"elapsed_ms": round((time.monotonic() - t0) * 1000),
            "sources": {"broadcast_ifaces": nifaces, "tailscale": ts_state, "neighbors": nb_state},
            "partial": partial, "pcs": out}
