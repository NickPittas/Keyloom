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
import ssl
import stat
import subprocess
import sys
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
MARGIN = 512 << 20
CACHE = ".staging.dl"  # a ".staging.*" name: lifecycle accepts it as ours in a not-yet-installed prefix
SHA_RE = re.compile(r"^[0-9a-f]{64}$")
BAD_PATH_RE = re.compile(r'[\x00-\x1f<>:"|?*\\]' if IS_WIN else r"[\x00-\x1f\\]")
WIN_RESERVED_RE = re.compile(r"^(con|prn|aux|nul|com[1-9]|lpt[1-9])(\..*)?$", re.I)
NET_ERRORS = (urllib.error.URLError, TimeoutError, ConnectionError, http.client.HTTPException, ssl.SSLError, EOFError)


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
    return Path(base) / "corridorkey-native" if base else None


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
            return self.opener.open(urllib.request.Request(url, headers=hdr), timeout=60)
        except urllib.error.HTTPError as e:
            code = e.code
            e.close()
            if code in (408, 416, 429) or code >= 500:
                raise Transient(f"HTTP {code}")
            raise Fail(f"download refused (HTTP {code}) by {urllib.parse.urlsplit(url).hostname}")
        except NET_ERRORS as e:
            raise Transient(type(e).__name__)


def retrying(fn):
    for attempt in range(RETRIES):
        try:
            return fn()
        except Transient as e:
            if attempt == RETRIES - 1:
                raise Fail(f"network error ({e}); run again to resume")
            time.sleep(min(2 ** attempt, 20))


class Progress:
    def __init__(self, label, total, cache: Path | None = None):
        self.label, self.total, self.cache, self.last = label, total, cache, 0.0

    def tick(self, done, force=False):
        if force or time.time() - self.last >= 3:
            self.last = time.time()
            say(f"  {self.label}: {done >> 20}/{self.total >> 20} MiB")
            if self.cache is not None:
                os.utime(self.cache)  # keep the resume cache young for lifecycle.recover()


def wheel_name(w: dict) -> str:
    return posixpath.basename(urllib.parse.urlsplit(w["url"]).path) or w["sha256"][:12]


def download(fx: Fetcher, w: dict, cache: Path) -> Path:
    """Fetch one pinned wheel into cache/<sha>.whl (resumable <sha>.part), verified by size and SHA-256."""
    sha, size = w["sha256"], w["size"]
    final, part = cache / f"{sha}.whl", cache / f"{sha}.part"
    if final.is_file() and final.stat().st_size == size and L.sha256_file(final) == sha:
        return final
    final.unlink(missing_ok=True)
    prog = Progress(wheel_name(w), size, cache)

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
                        b = r.read(1 << 20)
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

    retrying(attempt)
    prog.tick(size, force=True)
    if part.stat().st_size != size or L.sha256_file(part) != sha:
        part.unlink(missing_ok=True)
        raise Fail(f"{wheel_name(w)} failed verification; it was discarded, run again")
    os.replace(part, final)
    return final


# ---- staging ----------------------------------------------------------------

def set_mode(p: Path, executable: bool):
    if not IS_WIN:
        os.chmod(p, 0o755 if executable else 0o644)


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
        if h.hexdigest() != meta["sha256"]:
            dst.unlink()
            return False
        set_mode(dst, bool(st.st_mode & 0o111))
        return True
    except OSError:
        dst.unlink(missing_ok=True)
        return False


def extract_wheel(path: Path, w: dict, regular: dict, tree: Path, staged: set, prog: Progress):
    """Only the still-needed mapped entries: regular, unique in the zip, exact size, streamed through the hash."""
    try:
        with zipfile.ZipFile(path) as z:
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
                    if src.read(1):
                        raise Fail(f"{wheel_name(w)}: entry for {r[:80]!r} is larger than declared")
                if h.hexdigest() != meta["sha256"]:
                    raise Fail(f"{wheel_name(w)}: entry for {r[:80]!r} does not match the install manifest")
                set_mode(dst, bool(mode & 0o111))
                staged.add(r)
    except Fail:
        raise
    except (zipfile.BadZipFile, zlib.error, EOFError, NotImplementedError, RuntimeError) as e:
        raise Fail(f"{wheel_name(w)}: damaged wheel ({type(e).__name__})")
    prog.tick(prog.total, force=True)


def free_check(prefix: Path, need: int, what: str):
    free = shutil.disk_usage(prefix).free
    if free < need + MARGIN:
        raise Fail(f"not enough space on {prefix} for {what}: need ~{(need + MARGIN) >> 20} MiB, free {free >> 20} MiB")


def cached_bytes(cache: Path, w: dict) -> int:
    return sum(x.stat().st_size for x in (cache / f"{w['sha256']}.whl", cache / f"{w['sha256']}.part") if x.is_file())


def parent_inside(root: Path, p: Path, memo: dict) -> bool:
    """No symlinked directory may redirect a copied path out of its tree."""
    if p.parent not in memo:
        memo[p.parent] = p.parent.resolve().is_relative_to(root)
    return memo[p.parent]


def build_tree(fx: Fetcher | None, prefix: Path, rec: dict | None, stage: Path, cache: Path, pdir: Path,
               mani: dict, raw_manifest: bytes, wheels: list, bundled: set) -> Path:
    files = mani["files"]
    regular = {r: m for r, m in files.items() if m.get("symlink") is None}
    tree = stage / "payload"
    tree.mkdir(parents=True)
    (tree / L.MANIFEST).write_bytes(raw_manifest)  # the original manifest, byte for byte

    hints = []  # installed versions whose identical files may be reused: (resolved dir, its manifest files)
    for v in (rec or {}).get("current"), (rec or {}).get("previous"):
        try:
            d = L.vpath(prefix, v) if v else None
            if d and d.is_dir() and d.resolve() not in [h[0] for h in hints]:
                hints.append((d.resolve(), L.read_manifest(d)["files"]))
        except L.InstallError:
            pass
    plan = {r: next(((d, d / r) for d, hf in hints if hf.get(r) == m), None) for r, m in regular.items()}

    def needed_wheels(done):
        return [w for w in wheels if any(r not in done and not (plan[r] or r in bundled) for r in w["files"])]
    total = sum(m["size"] for m in regular.values())
    dl = sum(max(0, w["size"] - cached_bytes(cache, w)) for w in needed_wheels(set()))
    # stage + downloads at the peak; lifecycle then wants 2x the bundle free (its own preflight), the cache not counted
    free_check(prefix, total + max(dl, 2 * int(mani.get("total_bytes") or 0)), "the new version and its downloads")

    staged, reused, memo = set(), 0, {}
    for r, hit in plan.items():
        if hit and parent_inside(hit[0], hit[1], memo) and copy_verified(hit[1], tree / r, regular[r]):
            staged.add(r)
            reused += regular[r]["size"]
    if staged:
        say(f"Reusing {len(staged)} unchanged files ({reused >> 20} MiB) from the installed version.")
    proot, memo, own = pdir.resolve(), {}, 0
    for r in regular:
        if r not in staged and r in bundled and parent_inside(proot, pdir / r, memo) \
                and copy_verified(pdir / r, tree / r, regular[r]):
            staged.add(r)
            own += 1
    say(f"Copied {own} files from the installer.")
    todo = [w for w in needed_wheels(staged)]
    if todo:
        gib = sum(max(0, w["size"] - cached_bytes(cache, w)) for w in todo) / (1 << 30)
        say(f"Downloading {len(todo)} wheel(s) from public package servers (about {gib:.1f} GiB; resumable).")
    for w in todo:
        arc = download(fx, w, cache)
        extract_wheel(arc, w, regular, tree, staged, Progress(wheel_name(w), w["size"], cache))
    missing = [r for r in regular if r not in staged]
    if missing:
        raise Fail(f"{len(missing)} file(s) could not be provided (e.g. {missing[0][:80]!r}); the installer may be damaged")
    for r, m in files.items():  # symlinks last: targets already validated as relative and inside the version
        if m.get("symlink") is not None:
            (tree / r).parent.mkdir(parents=True, exist_ok=True)
            os.symlink(m["symlink"], tree / r)
    problems = L.verify_bundle(tree)
    if problems or not (tree / "app/packaging/lifecycle.py").is_file():
        raise Fail("assembled payload failed verification: " + (L.fmt_problems(problems) or "lifecycle.py missing"))
    return tree


# ---- orchestration ----------------------------------------------------------

def default_prefix() -> Path:
    return Path(os.environ["LOCALAPPDATA"]) / "Programs" / "CorridorKey" if IS_WIN else Path.home() / ".local/opt/CorridorKey"


def inspect_prefix(prefix: Path):
    """(receipt or None). Refuses anything that is not empty or a CorridorKey install."""
    if prefix.is_symlink() or (prefix.exists() and not prefix.is_dir()):
        raise Fail(f"{prefix} is not a plain folder")
    if (prefix / L.RECEIPT).exists():
        raw = L.load_json(prefix / L.RECEIPT)
        if not isinstance(raw, dict) or raw.get("app") != L.APP_ID:
            raise Fail(f"{prefix} has a receipt that is not a CorridorKey install; refusing to touch it")
        L.recover(prefix)
        return L.read_receipt(prefix)
    if prefix.is_dir():
        foreign = [e.name for e in prefix.iterdir() if not (e.name.startswith(".staging.") or e.name in (L.TXN, "install-result.json")
                   or (e.name in ("versions", "bin") and e.is_dir() and not any(e.iterdir())))]
        if foreign:
            raise Fail(f"{prefix} is not empty and not a CorridorKey install ({', '.join(foreign[:4])}...); "
                       "choose an empty folder or a new subfolder")
    return None


def up_to_date(prefix: Path, rec: dict | None, mani: dict) -> bool:
    """Exact local check, no network: the receipt names this build, its recorded files equal the bundled
    manifest's, and every installed file verifies."""
    cur = (rec or {}).get("current")
    if cur != mani["version_id"]:
        return False
    d = L.vpath(prefix, cur)
    try:
        return (d.is_dir() and L.read_manifest(d).get("files") == mani["files"] and L.pointer_ok(prefix, cur)
                and not L.verify_bundle(d))
    except L.InstallError:
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


def run_lifecycle(prefix: Path, tree: Path, verb: str) -> int:
    """The staged lifecycle.py runs under the separate bootstrap, outside the version being activated."""
    global CHILD
    cmd = [sys.executable, "-I", "-B", str(tree / "app/packaging/lifecycle.py"), "--prefix", str(prefix), verb,
           "--payload", str(tree)] + (["--models", "skip"] if verb == "install" else [])
    CHILD = subprocess.Popen(cmd, env=L.clean_env())
    interrupted = False
    while True:
        try:
            rc = CHILD.wait()
            break
        except KeyboardInterrupt:
            interrupted = True  # the terminal sent the child the same SIGINT; wait for its rollback
    return 130 if (interrupted or STOPPED) and rc else rc


def install(a) -> int:
    pdir = Path(a.payload_dir).absolute()
    prefix = Path(a.prefix).absolute() if a.prefix else default_prefix()
    mani, raw_manifest, wheels, bundled = load_inputs(pdir, Path(a.sources))
    L.check_glibc(mani)
    L.check_system_libs()
    vid = mani["version_id"]
    root, boot = prefix.resolve(), pdir.resolve()
    if root.is_relative_to(boot) or (boot.is_relative_to(root)
            and not boot.relative_to(root).parts[0].startswith(".staging.")):
        raise Fail("the payload must be outside the install folder or inside its temporary installer staging folder")
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
            tree = build_tree(Fetcher(tls_context(pdir / "runtime/python")), prefix, rec, stage, cache, pdir,
                              mani, raw_manifest, wheels, bundled)
            if shutil.disk_usage(prefix).free < 2 * int(mani.get("total_bytes") or 0) + MARGIN:
                shutil.rmtree(cache, ignore_errors=True)  # lifecycle needs the room more than a resume cache does
            repairable = False
            if (rec or {}).get("current") == vid:
                try:
                    repairable = L.read_manifest(L.vpath(prefix, vid)).get("files") == mani["files"]
                except L.InstallError:
                    pass
            verb = "repair" if repairable else "install"
            for d in {stage, pdir.parents[1]}:
                if d.parent == prefix and d.name.startswith(".staging."):
                    os.utime(d)  # The lifecycle cleanup must not remove a slow download's live payload or bootstrap.
            say(f"Handing the verified payload to the installer ({verb}) ...")
            rc = run_lifecycle(prefix, tree, verb)
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
