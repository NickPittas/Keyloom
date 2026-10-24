"""Online installer/updater client for Keyloom (stdlib only, Linux + Windows).

The wrapper carries Keyloom itself (a slim, complete stage) and `wheel-sources.json`, which maps the few huge
third-party files left out of the slim stage (PyTorch / NVIDIA CUDA libraries ...) to public, hash-pinned wheel
URLs. This client, run by the wrapper's own bundled interpreter (`python -I -B online.py ...`), assembles the
complete payload under PREFIX/.staging.*: unchanged files are copied from the installed versions, everything else
from the slim payload, and only still-missing mapped files are extracted from wheels it downloads. Every file is
checked against the ORIGINAL install manifest (SHA-256 + size), then the staged lifecycle.py (`install --payload` /
`repair --payload`) owns the transaction, rollback, shims, shortcuts and receipt.

There is no account, token, repository or release lookup: only the pinned public wheel URLs are fetched, over HTTPS,
without credentials. Nothing here touches user data, settings, network identities, certificates, jobs or models,
installs no driver or compiler, and the app is never launched.

Exit codes: 0 installed or already up to date, 2 failed (previous install untouched), 4 Keyloom is running,
130 interrupted.
"""
from __future__ import annotations

import argparse
import concurrent.futures
import contextlib
import hashlib
import http.client
import importlib.util
import ipaddress
import json
import os
import posixpath
import re
import shutil
import signal
import socket
import ssl
import stat
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
import zipfile
import zlib
from pathlib import Path


def _lifecycle():
    """The lifecycle.py next to this file (both live in <payload>/app/packaging)."""
    spec = importlib.util.spec_from_file_location("ck_lifecycle", Path(__file__).resolve().with_name("lifecycle.py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


L = _lifecycle()
IS_WIN = L.IS_WIN
PLATFORM = "windows-x86_64" if IS_WIN else "linux-x86_64"
MAX_WHEEL = 8 << 30
MAX_JSON = 256 << 20
MAX_FILES = 500_000
RETRIES = 5
READ_TIMEOUT = 20  # connect and every socket read; a silent server fails after this, never hangs
WORKERS = 8  # default size of the ONE bounded worker pool shared by every parallel stage (probes, copies, wheels)
MAX_WORKERS = 16  # hard cap
MARGIN = 512 << 20
CACHE = ".staging.dl"  # a ".staging.*" name: lifecycle accepts it as ours in a not-yet-installed prefix
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
BAD_PATH_RE = re.compile(r'[\x00-\x1f<>:"|?*\\]' if IS_WIN else r"[\x00-\x1f\\]")
WIN_RESERVED_RE = re.compile(r"^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$", re.I)
NET_ERRORS = (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException, ssl.SSLError, EOFError)


class Budget:
    """Tracks which workers are in a network transfer (net) or doing local work (local) and owns KEYLOOM_WAIT_FILE accordingly.
    There is no time budget: the supervising installer stops a step only after it shows no progress (see installer_payload)."""

    def __init__(self):
        # Thread safe. The wait file exists only while at least one worker is in a network transfer (net) AND none is doing
        # local work (local). Overlapping hash/copy/extract keeps it away.
        self.lock, self.net_n, self.loc_n, self.pstart, self.wait_cm = threading.RLock(), 0, 0, None, None

    @property
    def network_only(self) -> bool:
        return self.pstart is not None

    def _sync(self):  # caller holds the lock
        want, now = self.net_n > 0 and self.loc_n == 0, time.monotonic()
        if want and self.pstart is None:
            self.pstart = now
            cm = L.user_wait("explicit public wheel download")
            cm.__enter__()
            self.wait_cm = cm
        elif not want and self.pstart is not None:
            self.pstart = None
            cm, self.wait_cm = self.wait_cm, None
            if cm is not None:
                cm.__exit__(None, None, None)

    @contextlib.contextmanager
    def _count(self, attr):
        with self.lock:
            setattr(self, attr, getattr(self, attr) + 1)
            self._sync()
        try:
            yield
        finally:
            with self.lock:
                setattr(self, attr, getattr(self, attr) - 1)
                self._sync()

    def net(self):
        return self._count("net_n")

    def local(self):
        return self._count("loc_n")

    def check(self):
        if POOL.stop.is_set():
            raise Fail("cancelled: another parallel task failed")

BUDGET = Budget()


class Pool:
    """The ONE bounded worker pool (default WORKERS, cap MAX_WORKERS), never nested: map() is only called from the main thread.
    At most `cap` tasks are in flight (no unbounded futures); results come back in INPUT order, so selection and file ownership
    never depend on completion order. Any error/interrupt sets `stop` (workers see it in Budget.check/Sub.tick), cancels what
    has not started and re-raises. One reporter line per second: phase, bytes, MiB/s, active/completed workers, elapsed, state."""

    def __init__(self):
        self.n = max(1, min(WORKERS, MAX_WORKERS))
        self.ex, self.stop, self.lock = None, threading.Event(), threading.Lock()
        self.phase, self.bytes, self.total, self.active, self.completed, self.tasks = "idle", 0, 0, 0, 0, 0

    def add(self, n: int):
        with self.lock:
            self.bytes += n

    def _report(self, end: threading.Event, cache: Path | None):
        t0, last, seen = time.monotonic(), 0, None
        while not end.wait(1.0):
            with self.lock:
                b, a, c, t, tot, ph = self.bytes, self.active, self.completed, self.tasks, self.total, self.phase
            if (b, c) == seen:  # nothing moved: stay silent so the supervisor's stall guard can see it
                last = b
                continue
            seen = (b, c)
            state = "NETWORK transfer only" if BUDGET.network_only else "local work"
            say(f"  {ph}: {b / 1048576:.1f}/{tot / 1048576:.1f} MiB, {(b - last) / 1048576:.1f} MiB/s, workers {a}/{self.n} active, "
                f"{c}/{t} done, elapsed {time.monotonic() - t0:.0f}s, {state}")
            last = b
            if cache is not None:
                try:
                    os.utime(cache)  # keep the resume cache young for lifecycle.recover()
                except OSError:
                    pass

    def map(self, phase: str, items, fn, limit: int | None = None, total: int = 0, cache: Path | None = None) -> list:
        items = list(items)
        if not items:
            return []
        cap = max(1, min(limit or self.n, self.n))
        if self.ex is None:
            self.ex = concurrent.futures.ThreadPoolExecutor(max_workers=self.n, thread_name_prefix="ck-online")
        with self.lock:
            self.phase, self.bytes, self.total, self.active, self.completed, self.tasks = phase, 0, total, 0, 0, len(items)
        self.stop.clear()

        def run(x):
            with self.lock:
                self.active += 1
            try:
                return fn(x)
            finally:
                with self.lock:
                    self.active -= 1
                    self.completed += 1
        end = threading.Event()
        th = threading.Thread(target=self._report, args=(end, cache), daemon=True)
        th.start()
        out, pending, it = [None] * len(items), {}, iter(enumerate(items))
        try:
            while True:
                while len(pending) < cap:
                    nxt = next(it, None)
                    if nxt is None:
                        break
                    pending[self.ex.submit(run, nxt[1])] = nxt[0]
                if not pending:
                    return out
                done, _ = concurrent.futures.wait(pending, return_when=concurrent.futures.FIRST_COMPLETED)
                for f in done:
                    out[pending.pop(f)] = f.result()
        except BaseException:
            self.stop.set()
            for f in pending:
                f.cancel()
            _, still = concurrent.futures.wait(pending, timeout=10)  # workers see the stop flag every chunk
            if still:
                say(f"  {len(still)} local worker(s) still stopping (they check the stop flag every chunk); "
                    "the partial downloads are kept for resume")
            raise
        finally:
            end.set()
            th.join(2)

    def close(self):
        self.stop.set()
        if self.ex is not None:
            self.ex.shutdown(wait=False, cancel_futures=True)
            self.ex = None


POOL = Pool()


class Sub:
    """Per-task byte counter feeding the pool's aggregate; the cancellation point for download/extract loops."""

    def __init__(self):
        self.done = 0

    def tick(self, d):
        if POOL.stop.is_set():
            raise Fail("cancelled: another parallel task failed")
        if d > self.done:
            POOL.add(d - self.done)
        self.done = d


def runtime_id(files: dict) -> str:
    """Same producer as bundle.runtime_id: sha256 of the canonical runtime/ manifest subset (full 64 hex)."""
    sub = {k: v for k, v in files.items() if k.startswith("runtime/")}
    return hashlib.sha256(json.dumps(sub, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


class Fail(RuntimeError):
    pass


class Running(Fail):
    pass


class Transient(Exception):
    pass


say = L.say


# ---- same per-user instance lock as the Rust app ----------------------------

def settings_dir() -> Path | None:
    """Folder holding instance.lock; mirrors rust-ui settings::default_path (no HOME/APPDATA = no lock)."""
    if IS_WIN:
        base = os.environ.get("APPDATA") or os.environ.get("LOCALAPPDATA")
    else:
        base = os.environ.get("XDG_CONFIG_HOME") or (str(Path(os.environ["HOME"]) / ".config") if os.environ.get("HOME") else None)
    if not base:
        return None
    old = Path(base) / "corridorkey-native"  # existing settings stay where they are (shared with the app and service)
    return old if old.is_dir() and not (Path(base) / "keyloom").exists() else Path(base) / "keyloom"


class InstanceLock:
    """Exclusive lock on instance.lock, held for the whole run including the lifecycle child, so the app cannot
    start mid-update. Unix: flock(LOCK_EX|LOCK_NB) like Rust File::try_lock. Windows: LockFileEx over the whole
    file with the same flags Rust uses (msvcrt.locking is a different, range-limited LockFile call)."""

    def __enter__(self):
        self.fd = None
        d = settings_dir()
        if d is None:
            return self
        d.mkdir(parents=True, exist_ok=True)
        fd = os.open(d / "instance.lock", os.O_RDWR | os.O_CREAT | getattr(os, "O_BINARY", 0), 0o666)  # never truncate
        try:
            self._lock(fd)
        except BaseException:
            os.close(fd)
            raise
        self.fd = fd
        return self

    @staticmethod
    def _lock(fd):
        busy = Running("Keyloom is running (possibly hidden in the background; there is no tray icon). "
                       "Choose File > Quit in Keyloom, or end its process, then run this again.")
        if not IS_WIN:
            import fcntl
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                raise busy
            return
        import ctypes
        import msvcrt
        from ctypes import wintypes

        class OVERLAPPED(ctypes.Structure):
            _fields_ = [("Internal", ctypes.c_void_p), ("InternalHigh", ctypes.c_void_p), ("Offset", wintypes.DWORD),
                        ("OffsetHigh", wintypes.DWORD), ("hEvent", wintypes.HANDLE)]
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.LockFileEx.argtypes = [wintypes.HANDLE, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD, wintypes.DWORD,
                                   ctypes.POINTER(OVERLAPPED)]
        k32.LockFileEx.restype = wintypes.BOOL
        if not k32.LockFileEx(msvcrt.get_osfhandle(fd), 0x3, 0, 0xFFFFFFFF, 0xFFFFFFFF, ctypes.byref(OVERLAPPED())):
            err = ctypes.get_last_error()
            if err in (32, 33):  # sharing / lock violation
                raise busy
            raise Fail(f"could not take the Keyloom instance lock (Windows error {err})")

    def __exit__(self, *exc):
        if self.fd is not None:
            os.close(self.fd)  # releases the lock


# ---- validation of the local inputs (trust boundary: the wrapper's files, not the network) ----------------------

def _int(v, lo=0, hi=1 << 50) -> bool:
    return isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi


def rel_ok(rel) -> bool:
    if not (isinstance(rel, str) and 0 < len(rel) < 1024 and L._rel_ok(rel) and not BAD_PATH_RE.search(rel)
            and all(p not in ("", ".") for p in rel.split("/")) and rel != L.MANIFEST):
        return False
    # Windows would silently store these under another name (or refuse them): never let a manifest path rely on that.
    return not IS_WIN or not any(p.endswith((" ", ".")) or WIN_RESERVED_RE.match(p) for p in rel.split("/"))


def load_small_json(path: Path, what: str) -> tuple[dict, bytes]:
    try:
        if path.stat().st_size > MAX_JSON:
            raise Fail(f"{what} is too large")
        raw = path.read_bytes()
        return json.loads(raw), raw
    except (OSError, ValueError) as e:
        raise Fail(f"{what} is unreadable ({type(e).__name__}); the installer is damaged, download it again")


def validate_manifest(m) -> dict:
    """Original install manifest: safe paths, regular files + validated relative symlinks, nothing below another entry."""
    if not (isinstance(m, dict) and m.get("schema") == 1 and m.get("platform") == PLATFORM
            and isinstance(m.get("files"), dict) and 0 < len(m["files"]) <= MAX_FILES):
        raise Fail(f"install manifest is not a {PLATFORM} Keyloom manifest")
    L.safe_version(m.get("version_id"))
    files = m["files"]
    if "runtime_id" in m and m["runtime_id"] != runtime_id(files):
        raise Fail("install manifest: runtime_id does not match its runtime files; the installer is damaged")
    for r, meta in files.items():
        if not rel_ok(r) or not isinstance(meta, dict):
            raise Fail(f"install manifest: unsafe path {str(r)[:80]!r}")
        if meta.get("symlink") is not None:
            t = meta["symlink"]
            if not isinstance(t, str) or not t or t.startswith("/") or "\\" in t or ":" in t or "\x00" in t or \
                    posixpath.normpath(posixpath.join(posixpath.dirname(r), t)).startswith(".."):
                raise Fail(f"install manifest: unsafe symlink target for {r[:80]!r}")
        elif not (_int(meta.get("size")) and SHA_RE.match(str(meta.get("sha256", "")))):
            raise Fail(f"install manifest: bad entry for {r[:80]!r}")
    key = str.casefold if IS_WIN else (lambda s: s)  # Windows: names differing only by case are the same file
    keys = {key(r) for r in files}
    if len(keys) != len(files):
        raise Fail("install manifest: two paths differ only by letter case")
    for r in files:  # nothing may be written through a file or symlink that is also a parent directory
        parts = r.split("/")
        if any(key("/".join(parts[:i])) in keys for i in range(1, len(parts))):
            raise Fail(f"install manifest: {r[:80]!r} sits below another entry")
    return m


def public_https_url(url) -> bool:
    if not (isinstance(url, str) and 8 < len(url) <= 2048 and all(32 < ord(c) < 127 for c in url)):
        return False
    u = urllib.parse.urlsplit(url)
    try:
        u.port
        ipaddress.ip_address(u.hostname or "")
        return False  # IP literals are never a public package server
    except ValueError:
        pass
    host = u.hostname or ""
    return (u.scheme == "https" and "." in host and host != "localhost" and not host.endswith((".local", ".internal"))
            and not u.username and not u.password and not u.fragment)


def validate_sources(s, mani: dict, bundled: set) -> list[dict]:
    """wheel-sources.json: schema 1, same version, https wheels with size+hash, each mapped file a regular manifest
    file mapped at most once, and every regular file missing from the slim payload covered by a mapping."""
    files = mani["files"]
    if not (isinstance(s, dict) and s.get("schema") == 1 and s.get("version_id") == mani["version_id"]
            and isinstance(s.get("wheels"), list) and len(s["wheels"]) <= 2000):
        raise Fail("wheel-sources.json does not describe this Keyloom version")
    mapped, shas = set(), set()
    for w in s["wheels"]:
        if not (isinstance(w, dict) and public_https_url(w.get("url")) and SHA_RE.match(str(w.get("sha256", "")))
                and _int(w.get("size"), 1, MAX_WHEEL) and isinstance(w.get("files"), dict) and w["files"]
                and len(w["files"]) <= MAX_FILES and w["sha256"] not in shas):
            raise Fail("wheel-sources.json: invalid or duplicate wheel entry")
        shas.add(w["sha256"])
        entries = list(w["files"].values())
        if len(set(entries)) != len(entries):
            raise Fail("wheel-sources.json: one wheel entry mapped to several files")
        for r, ent in w["files"].items():
            meta = files.get(r)
            if meta is None or meta.get("symlink") is not None or r in mapped:
                raise Fail(f"wheel-sources.json: {str(r)[:80]!r} is not a unique regular manifest file")
            if not (isinstance(ent, str) and 0 < len(ent) < 1024 and "\x00" not in ent and not ent.endswith("/")):
                raise Fail(f"wheel-sources.json: bad wheel entry name for {r[:80]!r}")
            mapped.add(r)
    uncovered = [r for r, m in files.items() if m.get("symlink") is None and r not in mapped and r not in bundled]
    if uncovered:
        raise Fail(f"{len(uncovered)} file(s) are neither bundled nor mapped to a wheel (e.g. {uncovered[0][:80]!r}); "
                   "the installer is damaged, download it again")
    return s["wheels"]


def load_inputs(pdir: Path, sources: Path):
    raw_manifest, raw = load_small_json(pdir / L.MANIFEST, "install-manifest.json")
    mani = validate_manifest(raw_manifest)
    bundled = set()
    for r, m in mani["files"].items():
        try:
            if m.get("symlink") is None and stat.S_ISREG(os.lstat(pdir / r).st_mode):
                bundled.add(r)
        except OSError:
            pass
    wheels = validate_sources(load_small_json(sources, "wheel-sources.json")[0], mani, bundled)
    return mani, raw, wheels, bundled


# ---- download of pinned public wheels --------------------------------------

def tls_context(*bases: Path) -> ssl.SSLContext:
    """certifi's bundle from the bundled interpreter's site-packages (read as a file; certifi/torch are not imported)."""
    for base in (Path(sys.prefix), Path(sys.base_prefix), *bases):
        for pat in ("lib/python3*/site-packages/certifi/cacert.pem", "Lib/site-packages/certifi/cacert.pem"):
            hit = next(base.glob(pat), None)
            if hit is not None and hit.is_file():
                return ssl.create_default_context(cafile=str(hit))
    return ssl.create_default_context()


class HttpsOnlyRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req, fp, code, msg, headers, newurl):
        if urllib.parse.urlsplit(newurl).scheme != "https":
            raise Fail("refusing a non-HTTPS download redirect")
        return super().redirect_request(req, fp, code, msg, headers, newurl)


class Fetcher:
    """Plain anonymous GET with optional Range; no credentials of any kind are ever attached."""

    def __init__(self, ctx: ssl.SSLContext):
        self.opener = urllib.request.build_opener(HttpsOnlyRedirect, urllib.request.HTTPSHandler(context=ctx))

    def open(self, url: str, offset: int):
        hdr = {"User-Agent": "keyloom-online-installer", "Accept-Encoding": "identity"}
        if offset:
            hdr["Range"] = f"bytes={offset}-"
        try:
            return self.opener.open(urllib.request.Request(url, headers=hdr), timeout=READ_TIMEOUT)
        except urllib.error.HTTPError as e:
            code = e.code
            e.close()
            if code in (408, 416, 429) or code >= 500:
                raise Transient(f"HTTP {code}")
            raise Fail(f"download refused (HTTP {code}) by {urllib.parse.urlsplit(url).hostname}")
        except NET_ERRORS as e:
            if isinstance(e, TimeoutError) or isinstance(getattr(e, "reason", None), TimeoutError):
                raise Transient(f"connect timed out after {READ_TIMEOUT}s")
            raise Transient(type(e).__name__)


_PHASE = ["local"]  # CHILD side only: "net" while connecting/transferring, "local" while hashing/renaming


def hash_file(path: Path, label: str) -> str:
    h = hashlib.sha256()
    with BUDGET.local(), open(path, "rb") as f:  # local work
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
            POOL.add(len(b))
            BUDGET.check()
    return h.hexdigest()


def wheel_name(w: dict) -> str:
    return posixpath.basename(urllib.parse.urlsplit(w["url"]).path) or w["sha256"][:12]


def fetch_once(fx: Fetcher, w: dict, cache: Path) -> Path:
    """Fetch one pinned wheel into cache/<sha>.whl (resumable <sha>.part), verified by size and SHA-256."""
    sha, size = w["sha256"], w["size"]
    final, part = cache / f"{sha}.whl", cache / f"{sha}.part"
    _PHASE[0] = "local"
    if final.is_file() and final.stat().st_size == size and hash_file(final, wheel_name(w)) == sha:
        return final
    final.unlink(missing_ok=True)
    prog = Sub()

    def attempt():
        have = part.stat().st_size if part.is_file() else 0
        if have > size:
            part.unlink()
            have = 0
        if have == size:
            return
        try:
            r = fx.open(w["url"], have)
        except Transient as e:
            if str(e) == "HTTP 416":
                part.unlink(missing_ok=True)  # our partial no longer matches the server
            raise
        with r:
            if r.status == 206 and have:
                m = re.fullmatch(r"bytes (\d+)-(\d+)/(\d+)", r.headers.get("Content-Range", ""))
                if not m or int(m[1]) != have or int(m[3]) != size:
                    raise Fail("server sent an invalid Content-Range")
            elif r.status != 200:
                raise Fail(f"unexpected download status {r.status}")
            else:
                have = 0  # fresh request, or the server ignored Range: restart this wheel
                if r.headers.get("Content-Length", str(size)) != str(size):
                    raise Fail(f"{wheel_name(w)} has an unexpected size on the server")
            with open(part, "ab" if have else "wb") as o:
                while True:
                    try:
                        b = r.read1(1 << 20)  # one recv: a poll timeout can never discard already-buffered bytes
                    except (TimeoutError, socket.timeout):
                        raise Transient(f"stalled: no data for {READ_TIMEOUT}s")  # a timed-out read is never resumed in place
                    except NET_ERRORS as e:
                        raise Transient(type(e).__name__)
                    if not b:
                        break
                    have += len(b)
                    if have > size:
                        raise Fail("download is larger than expected")
                    o.write(b)
                    prog.tick(have)
        if have != size:
            raise Transient("connection closed early")

    _PHASE[0] = "net"  # the parent reads this phase to tell a genuine transfer from local work
    attempt()
    _PHASE[0] = "local"
    if part.stat().st_size != size or hash_file(part, wheel_name(w)) != sha:
        part.unlink(missing_ok=True)
        raise Fail(f"{wheel_name(w)} failed verification; it was discarded, run again")
    os.replace(part, final)
    return final


# ---- staging ----------------------------------------------------------------

_FETCH_CODE = ("import sys,importlib.util as u;s=u.spec_from_file_location('ck_online_child',sys.argv[1]);"
               "m=u.module_from_spec(s);s.loader.exec_module(m);sys.exit(m.fetch_child())")


def fetch_child() -> int:
    """CHILD entry (own process, bootstrap interpreter): ONE attempt of fetch_once for the wheel on stdin. Only this process
    opens the connection and writes that wheel's .part/.whl. It publishes {b: bytes, p: phase} (and err/code at the end) to
    <cache>/<sha>.status twice a second. It has no wait file and no URL output. Exit 0 ok, 20 transient network
    error (the parent retries/resumes), 10 anything else."""
    job = json.loads(sys.stdin.readline())
    w, cache = job["w"], Path(job["cache"])
    status, end = cache / f"{w['sha256']}.status", threading.Event()
    BUDGET.check = lambda *a, **k: None  # the parent cancels this process by killing it

    def publish(extra=None):
        tmp = status.with_name(status.name + ".tmp")
        tmp.write_text(json.dumps(dict({"b": POOL.bytes, "p": _PHASE[0]}, **(extra or {}))))
        os.replace(tmp, status)

    def beat():
        while not end.wait(0.25):
            try:
                publish()
            except OSError:
                pass
    threading.Thread(target=beat, daemon=True).start()
    code, err = 0, None
    try:
        fetch_once(Fetcher(tls_context(Path(job["tls"]))), w, cache)
    except Transient as e:
        code, err = 20, str(e)
    except Fail as e:
        code, err = 10, str(e)
    except BaseException as e:
        code, err = 10, type(e).__name__
    end.set()
    try:
        publish({"p": "done", "err": err})
    except OSError:
        code = code or 10
    return code


def _read_status(path: Path) -> dict:
    try:
        d = json.loads(path.read_text())
        return d if isinstance(d, dict) else {}
    except (OSError, ValueError):
        return {}


def run_fetch_child(tls_base: Path, w: dict, cache: Path) -> tuple[int, str]:
    """PARENT supervision of one killable transfer process. Reacts to the shared stop flag within ~0.2 s (even during DNS/connect),
    enforces the no-progress deadline itself, feeds the aggregate byte counter, and tells the Budget what is genuinely happening:
    child phase "net" counts as network, anything else as local work. Returns (code, message); cancellation raises."""
    status = cache / f"{w['sha256']}.status"
    status.unlink(missing_ok=True)
    env = {k: v for k, v in L.clean_env().items() if k != "KEYLOOM_WAIT_FILE"}  # only the parent owns the wait file
    p = subprocess.Popen([sys.executable, "-I", "-B", "-c", _FETCH_CODE, str(Path(__file__).resolve())], stdin=subprocess.PIPE,
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, env=env)
    cm, cur, seen, quiet, early = None, None, 0, time.monotonic(), None
    try:
        try:
            p.stdin.write(json.dumps({"w": w, "cache": str(cache), "tls": str(tls_base)}).encode() + b"\n")
            p.stdin.close()
        except OSError:
            pass  # the child already died: its exit code is handled below
        while True:
            st = _read_status(status)
            ph = "net" if st.get("p", "net") == "net" else "local"
            b, now = st.get("b") if isinstance(st.get("b"), int) else 0, time.monotonic()
            if b > seen:
                POOL.add(b - seen)
                seen, quiet = b, now
            if ph != cur:
                if cm is not None:
                    cm.__exit__(None, None, None)
                cm = BUDGET.net() if ph == "net" else BUDGET.local()
                cm.__enter__()
                cur, quiet = ph, now
            rc = p.poll()
            if rc is not None:
                break
            if POOL.stop.is_set():
                raise Fail("cancelled: another parallel task failed")
            BUDGET.check()
            if ph == "net" and now - quiet > READ_TIMEOUT + 5:  # covers DNS/connect/read: nothing can evade this
                early = (20, f"stalled: no data for {READ_TIMEOUT + 5}s")
                break
            POOL.stop.wait(0.2)
        if early:
            return early
        if rc == 0:
            return 0, ""
        err = _read_status(status).get("err")
        return (rc, err or "transfer failed") if rc in (10, 20) else (10, "the transfer process ended unexpectedly")
    finally:
        if p.poll() is None:
            p.kill()  # no socket timeout or thread join is waited for
        try:
            p.wait(2)
        except subprocess.TimeoutExpired:
            pass
        if cm is not None:
            cm.__exit__(None, None, None)
        status.unlink(missing_ok=True)


def download(tls_base: Path, w: dict, cache: Path) -> Path:
    """Fetch one pinned wheel into cache/<sha>.whl through killable child processes (one per attempt, resuming <sha>.part);
    the parent owns retry/backoff. All integrity checks (HTTPS, size, SHA-256, atomic rename) happen in fetch_once."""
    final = cache / f"{w['sha256']}.whl"
    for attempt in range(RETRIES):
        code, msg = run_fetch_child(tls_base, w, cache)
        if code == 0:
            if not final.is_file() or final.stat().st_size != w["size"]:
                raise Fail(f"{wheel_name(w)}: the transfer finished but the verified file is missing; run again")
            return final
        if code != 20:
            raise Fail(msg)
        if attempt == RETRIES - 1:
            raise Fail(f"network error ({msg}); run again to resume")
        with BUDGET.net():  # a retry wait is a network wait
            stopped = POOL.stop.wait(min(2 ** attempt, 20))  # interruptible backoff
        if stopped:
            raise Fail("cancelled: another parallel task failed")
    raise Fail("download failed")


def set_mode(p: Path, executable: bool):
    if not IS_WIN:
        os.chmod(p, 0o755 if executable else 0o644)


def link_trusted(src: Path, dst: Path, meta: dict) -> bool:
    """An INSTALLED file whose manifest metadata equals the incoming one is trusted: it is never read or hashed. Immutable
    hardlink where the volume allows it, else a plain byte copy (cross-volume/unsupported: the copy cost is then unavoidable).
    Only an exact regular, non-symlink file of the expected size qualifies; the installed file itself is never modified."""
    try:
        st = os.lstat(src)
        if not stat.S_ISREG(st.st_mode) or st.st_size != meta["size"]:
            return False
        dst.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.link(src, dst)
        except OSError:
            if POOL.stop.is_set():
                return False
            with open(src, "rb") as i, open(dst, "xb") as o:
                for b in iter(lambda: i.read(1 << 20), b""):
                    o.write(b)
                    POOL.add(len(b))
                    BUDGET.check()
            set_mode(dst, bool(st.st_mode & 0o111))
        return True
    except OSError:
        dst.unlink(missing_ok=True)
        return False


def copy_verified(src: Path, dst: Path, meta: dict) -> bool:
    """Real byte copy (never a hardlink) of a local file; rejected unless the copy hashes as expected."""
    try:
        st = os.lstat(src)
        if not stat.S_ISREG(st.st_mode) or st.st_size != meta["size"]:
            return False
        h = hashlib.sha256()
        dst.parent.mkdir(parents=True, exist_ok=True)
        with open(src, "rb") as i, open(dst, "xb") as o:
            for b in iter(lambda: i.read(1 << 20), b""):
                h.update(b)
                o.write(b)
                POOL.add(len(b))
                BUDGET.check()
        if h.hexdigest() != meta["sha256"]:
            dst.unlink()
            return False
        set_mode(dst, bool(st.st_mode & 0o111))
        return True
    except OSError:
        dst.unlink(missing_ok=True)
        return False


def extract_wheel(path: Path, w: dict, regular: dict, tree: Path, staged, prog: Sub):
    """Only the still-needed mapped entries: regular, unique in the zip, exact size, streamed through the hash."""
    try:
        got = []  # this worker's own result record; the parent merges it after the worker finished
        with zipfile.ZipFile(path) as z:  # one archive handle, owned by exactly one worker
            infos, dup = {}, set()
            for i in z.infolist():
                if i.filename in infos:
                    dup.add(i.filename)
                infos[i.filename] = i
            for r, ent in w["files"].items():
                if r in staged:
                    continue
                meta, i = regular[r], infos.get(ent)
                mode = i.external_attr >> 16 if i is not None and i.create_system == 3 else 0
                if i is None or ent in dup or i.is_dir() or i.flag_bits & 0x1 or (mode and not stat.S_ISREG(mode)) \
                        or i.file_size != meta["size"]:
                    raise Fail(f"{wheel_name(w)}: entry for {r[:80]!r} is missing or not the expected regular file")
                dst = tree / r
                dst.parent.mkdir(parents=True, exist_ok=True)
                h, left = hashlib.sha256(), meta["size"]
                with z.open(i) as src, open(dst, "xb") as o:
                    while left:
                        b = src.read(min(1 << 20, left))
                        if not b:
                            raise Fail(f"{wheel_name(w)}: truncated entry for {r[:80]!r}")
                        left -= len(b)
                        h.update(b)
                        o.write(b)
                        prog.tick(prog.done + len(b))
                        BUDGET.check()
                    if src.read(1):
                        raise Fail(f"{wheel_name(w)}: entry for {r[:80]!r} is larger than declared")
                if h.hexdigest() != meta["sha256"]:
                    raise Fail(f"{wheel_name(w)}: entry for {r[:80]!r} does not match the install manifest")
                set_mode(dst, bool(mode & 0o111))
                got.append(r)
    except Fail:
        raise
    except (zipfile.BadZipFile, zlib.error, EOFError, NotImplementedError, RuntimeError) as e:
        raise Fail(f"{wheel_name(w)}: damaged wheel ({type(e).__name__})")
    return got


def free_check(prefix: Path, need: int, what: str):
    free = shutil.disk_usage(prefix).free
    if free < need + MARGIN:
        raise Fail(f"not enough space on {prefix} for {what}: need ~{(need + MARGIN) >> 20} MiB, free {free >> 20} MiB")


def cached_bytes(cache: Path, w: dict) -> int:
    return sum(x.stat().st_size for x in (cache / f"{w['sha256']}.whl", cache / f"{w['sha256']}.part") if x.is_file())


def parent_inside(root: Path, p: Path, memo: dict) -> bool:
    """No symlinked directory may redirect a copied path out of its tree."""
    roots = root if isinstance(root, tuple) else (root,)
    with _MEMO_LOCK:  # compound check-then-set on a dict shared by the copy workers; held only for the (cached) resolve
        if p.parent not in memo:
            memo[p.parent] = p.parent.resolve()
        res = memo[p.parent]
    return any(res.is_relative_to(r) for r in roots)


_MEMO_LOCK = threading.Lock()


def build_tree(tls_base: Path | None, prefix: Path, rec: dict | None, stage: Path, cache: Path, pdir: Path,
               mani: dict, raw_manifest: bytes, wheels: list, bundled: set, hint_prefix: Path | None = None,
               attach: bool = False, trust_installed: bool = True) -> Path:
    """Assemble the payload tree. attach=True: lifecycle attaches the trusted existing runtime/ as a whole directory, so no
    runtime/ file is staged here at all. Otherwise a file identical (manifest metadata) to one in an OWNED, previously activated
    install is linked/copied WITHOUT hashing (trust_installed; explicit repair passes False and re-hashes). Bundled files and
    wheel entries are hashed once as they are written; nothing is re-read afterwards (the caller then attests the payload)."""
    files = mani["files"]
    skip = (lambda r: attach and r.startswith("runtime/"))
    regular = {r: m for r, m in files.items() if m.get("symlink") is None and not skip(r)}
    tree = stage / "payload"
    tree.mkdir(parents=True)
    (tree / L.MANIFEST).write_bytes(raw_manifest)  # the original manifest, byte for byte

    hints = []  # installed versions whose identical files may be reused: (resolved dir, its manifest files)
    hp, hr = (hint_prefix, L.read_receipt(hint_prefix)) if hint_prefix else (prefix, rec)
    cands = []  # stage 1 (serial: needs the receipt's version ids): candidate dirs, current before previous
    try:
        owned = bool(hr) and L.load_json(hp / L.RECEIPT).get("app") in (L.APP_ID, L.LEGACY_APP_ID) and bool(hr.get("activated_at"))
    except (L.InstallError, AttributeError, OSError):
        owned = False
    if not owned:
        hr = None  # not an owned, previously activated install: nothing in it is trusted as a source
    for v in (hr or {}).get("current"), (hr or {}).get("previous"):
        try:
            d = L.vpath(hp, v) if v else None
            if d and d.is_dir() and d.resolve() not in [c.resolve() for c in cands]:
                cands.append(d)
        except L.InstallError:
            pass

    def read_hint(d):  # stage 2 (independent, read-only, shares no state): manifest + the runtime link lifecycle vouches for
        try:
            files_ = L.read_manifest(d)["files"]
            rt = (d / "runtime").resolve() if L.is_link(d / "runtime") and \
                L.runtime_trusted(hp, d, [hint_prefix] if hint_prefix else ()) else None
            return d.resolve(), files_, rt
        except (L.InstallError, OSError):
            return None
    found = [h for h in POOL.map("discovery: installed manifests", cands, read_hint) if h]  # input order = precedence
    hints = [(d, f) for d, f, _ in found]
    extra_root = {d: rt for d, _, rt in found if rt}  # built by the parent only; workers just read it
    plan = {r: next(((d, d / r) for d, hf in hints if hf.get(r) == m), None) for r, m in regular.items()}

    def needed_wheels(done):
        return [w for w in wheels if any(r in regular and r not in done and not (plan[r] or r in bundled) for r in w["files"])]
    total = sum(m["size"] for m in regular.values())
    dl = sum(max(0, w["size"] - cached_bytes(cache, w)) for w in needed_wheels(set()))
    # stage + downloads at the peak; lifecycle then wants 2x the bundle free (its own preflight), the cache not counted
    free_check(prefix, total + max(dl, 2 * int(mani.get("total_bytes") or 0)), "the new version and its downloads")

    staged, reused, memo = set(), 0, {}
    def reuse_one(r):  # one worker per destination path: no two workers write the same file; sources are only read
        hit = plan[r]
        if not parent_inside((hit[0],) + ((extra_root[hit[0]],) if hit[0] in extra_root else ()), hit[1], memo):
            return False
        # installed + metadata-equal = trusted: linked/copied without hashing. Explicit repair does not trust damaged installs.
        return (link_trusted if trust_installed else copy_verified)(hit[1], tree / r, regular[r])
    todo_r = [r for r in plan if plan[r]]
    for r, ok in zip(todo_r, POOL.map("reuse installed files", todo_r, reuse_one,
                                      total=sum(regular[r]["size"] for r in todo_r))):
        if ok:
            staged.add(r)
            reused += regular[r]["size"]
    if staged:
        say(f"Reusing {len(staged)} unchanged files ({reused >> 20} MiB) from the installed version.")
    proot, memo, own = pdir.resolve(), {}, 0
    def copy_one(r):
        return copy_verified(pdir / r, tree / r, regular[r]) if parent_inside(proot, pdir / r, memo) else False
    todo_b = [r for r in regular if r not in staged and r in bundled]
    for r, ok in zip(todo_b, POOL.map("copy installer files", todo_b, copy_one,
                                      total=sum(regular[r]["size"] for r in todo_b))):
        if ok:
            staged.add(r)
            own += 1
    say(f"Copied {own} files from the installer.")
    todo = [w for w in needed_wheels(staged)]
    if todo:
        gib = sum(max(0, w["size"] - cached_bytes(cache, w)) for w in todo) / (1 << 30)
        say(f"Downloading {len(todo)} wheel(s) from public package servers (about {gib:.1f} GiB; resumable).")
    snap = frozenset(staged)  # immutable input to every wheel worker; `staged` itself is touched by the parent only

    def need_of(w):
        return sum(regular[r]["size"] for r in w["files"] if r in regular and r not in snap)

    def fetch_one(w):  # one worker owns one wheel: its download, its verification and its own ZipFile handle
        arc = download(tls_base, w, cache)
        with BUDGET.local():
            return extract_wheel(arc, w, regular, tree, snap, Sub())
    if todo:
        # simultaneous cache + extracted bytes must fit the free space: fewer wheels in flight rather than a full disk
        worst = max(w["size"] + need_of(w) for w in todo)
        k = max(1, min(POOL.n, (shutil.disk_usage(prefix).free - MARGIN) // max(worst, 1)))
        for got in POOL.map("wheels", todo, fetch_one, limit=int(k), cache=cache,
                            total=sum(max(0, w["size"] - cached_bytes(cache, w)) + w["size"] + need_of(w) for w in todo)):
            if staged.intersection(got):  # destination uniqueness: validate_sources forbids it, enforced again at the merge
                raise Fail("two wheels provided the same file; the installer is damaged")
            staged.update(got)
    missing = [r for r in regular if r not in staged]
    if missing:
        raise Fail(f"{len(missing)} file(s) could not be provided (e.g. {missing[0][:80]!r}); the installer may be damaged")
    for r, m in files.items():  # symlinks last: targets already validated as relative and inside the version
        if m.get("symlink") is not None and not skip(r):
            (tree / r).parent.mkdir(parents=True, exist_ok=True)
            os.symlink(m["symlink"], tree / r)
    # No second pass over the tree: every incoming byte (bundled files, wheel entries) was hashed once as it was written, and
    # installed sources matching the manifest are trusted. Completeness was checked above (`missing`).
    if not (tree / "app/packaging/lifecycle.py").is_file():
        raise Fail("assembled payload is incomplete: lifecycle.py missing")
    return tree




# ---- orchestration ----------------------------------------------------------

def default_prefix() -> Path:
    return (Path(os.environ["LOCALAPPDATA"]) / "Programs" if IS_WIN else Path.home() / ".local/opt") / "Keyloom"


def legacy_prefix(prefix: Path) -> Path | None:
    """The pre-rename default install next to the new default prefix, when `prefix` is that new default and not installed yet."""
    old = prefix.with_name("CorridorKey")
    return old if prefix == default_prefix() and not (prefix / L.RECEIPT).is_file() and (old / L.RECEIPT).is_file() else None


def peek_receipt(prefix: Path) -> dict | None:
    """Pre-lock, READ-ONLY look at the receipt (no recover(), no write). Anything unreadable or foreign is None here; the
    authoritative ownership check and any recovery run in inspect_prefix, under the instance lock."""
    try:
        if prefix.is_symlink() or not (prefix / L.RECEIPT).is_file():
            return None
        raw = L.load_json(prefix / L.RECEIPT)
        return L.read_receipt(prefix) if isinstance(raw, dict) and raw.get("app") in (L.APP_ID, L.LEGACY_APP_ID) else None
    except (L.InstallError, OSError):
        return None


def inspect_prefix(prefix: Path):
    """(receipt or None). Refuses anything that is not empty or a Keyloom install."""
    if prefix.is_symlink() or (prefix.exists() and not prefix.is_dir()):
        raise Fail(f"{prefix} is not a plain folder")
    if (prefix / L.RECEIPT).exists():
        raw = L.load_json(prefix / L.RECEIPT)
        if not isinstance(raw, dict) or raw.get("app") not in (L.APP_ID, L.LEGACY_APP_ID):
            raise Fail(f"{prefix} has a receipt that is not a Keyloom install; refusing to touch it")
        with L.mutation_lock(prefix, 10):  # brief: released before the lifecycle child, which takes it itself
            L.recover(prefix)
        return L.read_receipt(prefix)
    if prefix.is_dir():
        foreign = [e.name for e in prefix.iterdir() if not (e.name.startswith(".staging.") or e.name in (L.TXN, "install-result.json", "install.log")
                   or (e.name in ("versions", "bin") and e.is_dir() and not any(e.iterdir())))]
        if foreign:
            raise Fail(f"{prefix} is not empty and not a Keyloom install ({', '.join(foreign[:4])}...); "
                       "choose an empty folder or a new subfolder")
    return None


def up_to_date(prefix: Path, rec: dict | None, mani: dict, deep: bool = True) -> bool:
    """Local check, no network: the receipt names this build and its recorded files equal the bundled manifest's.
    deep=False stops there (cheap pre-check, no hashing). deep=True, run once under the lock, also hashes every
    non-runtime file and asks lifecycle whether the shared runtime is the trusted one (no runtime bytes are read)."""
    cur = (rec or {}).get("current")
    if cur != mani["version_id"]:
        return False
    d = L.vpath(prefix, cur)
    try:
        if not (d.is_dir() and L.read_manifest(d).get("files") == mani["files"] and L.pointer_ok(prefix, cur)):
            return False
        return not deep or L.current_is(prefix, cur, mani["files"])  # lifecycle's fast check: runtime/ is never hashed
    except L.InstallError:
        return False


def runtime_ok(prefix: Path, mani: dict, src: Path | None = None) -> dict | None:
    """lifecycle.runtime_available (reads no runtime bytes) -> its dict ONLY when `ok is True` and `rid` is this manifest's
    runtime_id; anything else (failed dict, non-dict, wrong rid, error) is None. Never general truthiness."""
    try:
        r = L.runtime_available(prefix, mani, src)
    except L.InstallError:
        return None
    return r if isinstance(r, dict) and r.get("ok") is True and r.get("rid") == runtime_id(mani["files"]) else None


def trusted_runtime(prefix: Path, mani: dict, src: Path | None) -> bool:
    """True: attach the existing runtime by reference (payload omits runtime/). False: the runtime is new/different, so the
    payload carries it (downloaded if needed). The identical runtime that cannot be used safely never falls back to a
    copy/extraction on a normal update: it needs Repair."""
    if runtime_ok(prefix, mani, src):
        return True
    try:
        r = L.runtime_available(prefix, mani, src)
    except L.InstallError:
        r = None
    if isinstance(r, dict) and r.get("invalid") is True:
        raise Fail(f"install manifest runtime is invalid ({str(r.get('reason'))[:100]})")
    if isinstance(r, dict) and r.get("same_runtime") is True:
        raise Fail("the installed runtime is unchanged but cannot be reused safely "
                   f"({str(r.get('reason'))[:100]}); run Repair once; a normal update will not copy or download it again")
    return False


CHILD = None
STOPPED = False


def _forward_signal(n, _frame):
    global STOPPED
    STOPPED = True
    if CHILD is not None and CHILD.poll() is None:
        CHILD.send_signal(n)  # lifecycle rolls itself back; we keep the lock until it exits
    else:
        raise KeyboardInterrupt


def migrate_settings_folder(pdir: Path, prefix: Path, adopt: Path | None) -> None:
    """Rename corridorkey-native -> keyloom (see network_service.migrate_settings_dir) BEFORE this run takes the instance lock,
    because Windows cannot rename a folder that holds our own open lock file. Failure only means 'not renamed yet'."""
    py = pdir / "runtime/python" / ("python.exe" if IS_WIN else "bin/python3")
    code = ("import sys,json;sys.path.insert(0,sys.argv[1]);from pathlib import Path;from backend import network_service as s;"
            "b=s._settings_base();o=[s.owner_id(Path(sys.argv[2]))]+([s.owner_id(Path(sys.argv[3]))] if sys.argv[3] else []);"
            "print(json.dumps(s.migrate_settings_dir(b,o) if b else None))")
    try:
        with InstanceLock():
            pass  # refuse early (clear message) when the app is running; released before the child takes the app lock itself
        r = subprocess.run([str(py), "-I", "-B", "-c", code, str(pdir / "app"), str(prefix), str(adopt or "")], env=L.clean_env(),
                           capture_output=True, text=True, timeout=L.INNER_TIMEOUT)
        if r.returncode != 0 or not r.stdout.strip():
            raise ValueError(f"the migration check exited with code {r.returncode} or gave no result")
        out = json.loads(r.stdout.strip().splitlines()[-1])
    except Running:
        raise
    except Exception as e:  # timeout, start failure, nonzero exit, empty output, unparseable JSON: the state is UNKNOWN
        raise Fail(f"the settings folder migration state could not be determined ({type(e).__name__}); installation stopped before "
                   "anything was staged or downloaded. The check may already have completed or undone part of an earlier "
                   "migration, and its recovery journal is preserved; run the installer again") from None
    # Shapes the producer (network_service.migrate_settings_dir) returns: None (null), or a dict with a string `message` and
    # at most one of deferred / pending / blocked set to True. Anything else is malformed, never "clear".
    if out is None:
        return
    flags = [k for k in ("deferred", "pending", "blocked") if k in out] if isinstance(out, dict) else None
    if (flags is None or not isinstance(out.get("message"), str)
            or any(not isinstance(out[k], bool) for k in flags) or sum(1 for k in flags if out[k]) > 1):
        raise Fail("the settings folder migration check returned an unrecognised result; installation stopped before anything "
                   "was staged or downloaded (its recovery journal is preserved); run the installer again")
    say(out["message"])
    if out.get("blocked") is True:  # ambiguous interrupted migration: refuse before any staging or download
        raise Fail(out["message"] + " (installation stopped before anything was staged or downloaded)")
    # deferred / pending (or an informative dict with no flag set) continue: settings stay where they are and every resolver finds them


def run_lifecycle(prefix: Path, tree: Path, verb: str, firewall: str = "yes", adopt: Path | None = None,
                  attach: bool = False, nonce: str | None = None, script: Path | None = None) -> int:
    """The staged lifecycle.py runs under the separate bootstrap, outside the version being activated.
    attach=True passes `install --attach-runtime` (payload has no runtime/). nonce passes `install --payload-attested NONCE`
    (core consumes the marker written by L.write_attestation). script, for an in-place activation, is the already-running
    bootstrap lifecycle.py, so no staged file is needed. The handoff waits for the lifecycle to finish; the supervising
    installer stops it if it shows no output for too long, and the lifecycle rolls itself back."""
    global CHILD
    cmd = [sys.executable, "-I", "-B", str(script or tree / "app/packaging/lifecycle.py"), "--prefix", str(prefix), verb,
           "--payload", str(tree), "--firewall", firewall] + (["--models", "skip"] if verb == "install" else []) \
        + (["--adopt-from", str(adopt)] if adopt and verb == "install" else []) \
        + (["--attach-runtime"] if attach and verb == "install" else []) \
        + (["--payload-attested", nonce] if nonce and verb == "install" else [])
    CHILD =subprocess.Popen(cmd, env=L.clean_env())
    interrupted = False
    while True:
        try:
            rc = CHILD.wait()
            break
        except KeyboardInterrupt:
            interrupted = True  # the terminal sent the child the same SIGINT; wait for its rollback
    return 130 if (interrupted or STOPPED) and rc else rc


def install(a) -> int:
    try:
        return _install(a)
    finally:
        POOL.close()  # cancels anything not started; running workers end at their next chunk or socket timeout


def _install(a) -> int:
    pdir = Path(a.payload_dir).absolute()
    prefix = Path(a.prefix).absolute() if a.prefix else default_prefix()
    mani, raw_manifest, wheels, bundled = load_inputs(pdir, Path(a.sources))
    L.check_glibc(mani)
    L.check_system_libs()
    vid = mani["version_id"]
    # Stage 1: independent READ-ONLY candidate probes share the pool (no writes, before the lock); precedence is fixed below by
    # position, never by completion order: explicit --adopt-from wins, else the legacy default install, else none.
    explicit = Path(a.adopt_from).absolute() if a.adopt_from else None
    legacy, explicit_ok = POOL.map("discovery: install candidates", [
        lambda: legacy_prefix(prefix) if not a.prefix else None,
        lambda: bool(explicit and (explicit / L.RECEIPT).is_file())], lambda f: f())
    adopt = explicit if explicit else legacy
    root, boot = prefix.resolve(), pdir.resolve()
    if root.is_relative_to(boot) or (boot.is_relative_to(root)
            and not boot.relative_to(root).parts[0].startswith(".staging.")):
        raise Fail("the payload must be outside the install folder or inside its temporary installer staging folder")
    # The rename MUST run before this process takes the instance lock, on every platform: network_service.migrate_settings_dir
    # takes the app's own flock on instance.lock itself (POSIX; it reports "Keyloom is running" and changes nothing if we held
    # it) and Windows cannot rename a folder holding our open lock file. The rename is safe without our lock: on POSIX the child
    # holds the app lock across the rename; on Windows the rename fails while the app has files open.
    if not a.repair and not up_to_date(prefix, peek_receipt(prefix), mani, deep=False):
        migrate_settings_folder(pdir, prefix, adopt)
    with InstanceLock():
        rec = inspect_prefix(prefix)
        if not a.repair and up_to_date(prefix, rec, mani):
            say(f"Keyloom {vid} is already installed and verified; nothing to do.")
            return 0
        created = not prefix.exists()
        prefix.mkdir(parents=True, exist_ok=True)
        stage, cache = prefix / f".staging.{uuid.uuid4().hex[:8]}", prefix / CACHE
        ok = False
        try:
            stage.mkdir()
            cache.mkdir(exist_ok=True)
            for x in cache.iterdir():  # leftovers of another wheel set
                if x.stem not in {w["sha256"] for w in wheels}:
                    x.unlink(missing_ok=True)
            # Only an owned install may be a trusted source: explicit --adopt-from needs its receipt (explicit_ok); the legacy default
            # was found by its receipt. build_tree additionally requires the receipt's app id and a recorded activation.
            hint = adopt if (adopt and not (rec or {}).get("current") and (explicit_ok if explicit else True)) else None
            # core resolves the reusable-runtime source as --adopt-from (owned receipt required); mirror it exactly here
            rsrc = adopt if adopt and (explicit_ok if explicit else True) else None  # legacy was found by its receipt
            # Same-target decision BEFORE any attach/download: core's target_state is read-only and reads no runtime content.
            ts = L.target_state(prefix, mani, rsrc) if (rec and not a.repair) else {"state": "fresh", "reason": ""}
            if ts["state"] == "refused":
                raise Fail(ts["reason"])
            if ts["state"] == "current":
                say(f"Keyloom {vid} is already installed and verified; nothing to do.")
                ok = True
                return 0
            inplace = ts["state"] == "in-place"  # recorded, inactive, identical: core activates it where it stands, places nothing
            attach = False if (a.repair or inplace) else trusted_runtime(prefix, mani, rsrc)
            if attach and ts["state"] == "replace":
                av = runtime_ok(prefix, mani, rsrc)
                if av and Path(str(av.get("path"))).resolve().is_relative_to(L.vpath(prefix, vid).resolve()):
                    raise Fail(f"{vid} is damaged and owns the runtime it would be replaced from; run Repair instead")
            if inplace:
                tree = stage / "payload"  # manifest only: lifecycle reads it for identity, then touches no files
                tree.mkdir()
                (tree / L.MANIFEST).write_bytes(raw_manifest)
            else:
                tree = build_tree(pdir / "runtime/python", prefix, rec, stage, cache, pdir,
                                  mani, raw_manifest, wheels, bundled, hint, attach, not a.repair)
            if shutil.disk_usage(prefix).free < 2 * int(mani.get("total_bytes") or 0) + MARGIN:
                shutil.rmtree(cache, ignore_errors=True)  # lifecycle needs the room more than a resume cache does
            repairable = False
            if a.repair and (rec or {}).get("current") == vid:  # only an explicit repair does the deep (runtime) verification
                try:
                    repairable = L.read_manifest(L.vpath(prefix, vid)).get("files") == mani["files"]
                except L.InstallError:
                    pass
            verb = "repair" if repairable else "install"
            for d in {stage, pdir.parents[1]}:
                if d.parent == prefix and d.name.startswith(".staging."):
                    os.utime(d)  # The lifecycle cleanup must not remove a slow download's live payload or bootstrap.
            say(f"Handing the verified payload to the installer ({verb}) ...")
            nonce = L.write_attestation(tree, mani, "online") if (verb == "install" and not inplace) else None  # core's one writer, after ALL writes
            rc = run_lifecycle(prefix, tree, verb, a.firewall, adopt, attach, nonce,
                               Path(L.__file__).resolve() if inplace else None)
            ok = rc in (0, 3)  # 3 = models skipped on purpose; they are offered by the app
            return 0 if ok else (130 if rc == 130 else 2)
        finally:
            shutil.rmtree(stage, ignore_errors=True)
            if ok:
                shutil.rmtree(cache, ignore_errors=True)
            elif created:
                try:
                    prefix.rmdir()  # only succeeds when nothing (not even a resume cache) is left
                except OSError:
                    pass


def build_parser():
    p = argparse.ArgumentParser(prog="keyloom-online", description="Install or update Keyloom from its bundled payload; "
                                "only large public dependencies are downloaded (no account, token or GitHub access).")
    p.add_argument("--payload-dir", required=True, help="slim complete stage with the original install-manifest.json")
    p.add_argument("--sources", required=True, help="wheel-sources.json")
    p.add_argument("--prefix")
    p.add_argument("--repair", action="store_true")
    p.add_argument("--adopt-from", help="pre-rename prefix to take over (Linux stub / Windows installer pass it)")
    p.add_argument("--firewall", choices=["yes", "skip", "ask"], default="yes", help="firewall rules: yes (default) or skip; ask is accepted and means yes")
    return p


def main(argv=None) -> int:
    a = build_parser().parse_args(argv)
    for name in ("SIGTERM", "SIGHUP"):
        if hasattr(signal, name):
            signal.signal(getattr(signal, name), _forward_signal)
    try:
        return install(a)
    except Running as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 4
    except (Fail, L.InstallError) as e:
        print(f"ERROR: {e}", file=sys.stderr)
        return 2
    except KeyboardInterrupt:
        print("Interrupted; nothing was changed (run again to resume the download).", file=sys.stderr)
        return 130
    except SystemExit as e:
        return e.code if isinstance(e.code, int) else 2
    except OSError as e:  # never print urllib reprs
        print(f"ERROR: {e.strerror or type(e).__name__} ({e.filename})", file=sys.stderr)
        return 2
    except Exception as e:
        print(f"ERROR: unexpected {type(e).__name__}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
