"""Verify + extract ONE independently compressed segment of a Keyloom .run installer (stdlib only; runs on the bundled bootstrap).

    installer_payload.py extract --run FILE --base STUB_BYTES --offset N --len N --sha256 H --dest DIR --label NAME
                                 [--require PREFIX] [--forbid PREFIX]

Only the bytes [base+offset, base+offset+len) of FILE are ever read, so an update that skips the runtime segment never touches it.
The segment is hashed while it streams through tar (single pass); a mismatch fails BEFORE the caller activates anything.
A ticker prints the actual bytes read every second. `extract` runs the work in a supervised child process group (heartbeat each
second; killed and reaped after STALL_SECONDS without any child output, exit 124). `run` supervises any command the same way.
There is no install-time budget: only a stalled step is stopped.
"""
from __future__ import annotations

import argparse
import codecs
import contextlib
import hashlib
import io
import json
import ntpath
import os
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import threading
import time
import traceback
from concurrent.futures import FIRST_EXCEPTION, ThreadPoolExecutor, wait


STALL_SECONDS = 300  # a step is stopped only after this long with no output at all: a hung child, never a slow disk or network


class Segment:
    """Read-only window over one segment that hashes and counts what tar consumes."""

    def __init__(self, f, start: int, length: int, clock=time.monotonic):
        self.f, self.left, self.total, self.done = f, length, length, 0
        self.h, self.clock, self.t0 = hashlib.sha256(), clock, clock()
        f.seek(start)

    def read(self, n: int = -1) -> bytes:
        n = self.left if n is None or n < 0 else min(n, self.left)
        b = self.f.read(n)
        if len(b) < n:
            raise EOFError("installer is truncated")
        self.left -= len(b)
        self.done += len(b)
        self.h.update(b)
        return b


def extract_segment(run, base: int, offset: int, length: int, sha256: str, dest, label: str,
                    require: str | None = None, forbid: str | None = None, out=sys.stdout, opener=open,
                    clock=time.monotonic) -> int:
    """Returns bytes read. Raises on tamper/truncation/unsafe member."""
    def check(member, path):
        n = member.name.rstrip("/") + "/"  # the root dir entry "payload" matches "payload/"
        if require and not n.startswith(require) or forbid and n.startswith(forbid):
            raise ValueError(f"unexpected member {member.name!r} in the {label} segment")
        return tarfile.data_filter(member, path)

    with opener(run, "rb") as f:
        seg = Segment(f, base + offset, length, clock)
        stop = threading.Event()

        def tick():
            seen = -1
            while not stop.wait(1):
                if seg.done == seen:  # nothing moved: stay silent so the supervisor's stall guard can see it
                    continue
                seen = seg.done
                el = clock() - seg.t0
                print(f"[{label}] {seg.done / 1048576:.0f}/{seg.total / 1048576:.0f} MiB read, "
                      f"{seg.done / 1048576 / max(el, 1e-9):.1f} MiB/s, elapsed {el:.0f}s", file=out, flush=True)
        t = threading.Thread(target=tick, daemon=True)
        t.start()
        try:
            with tarfile.open(fileobj=seg, mode="r|gz") as tar:
                tar.extractall(dest, filter=check)
            while seg.left:  # gzip trailer / padding: the hash covers the whole segment
                seg.read(1 << 20)
        finally:
            stop.set()
        if seg.h.hexdigest() != sha256:
            raise ValueError(f"{label} segment checksum mismatch (installer damaged or tampered)")
        print(f"[{label}] verified and extracted {seg.done >> 20} MiB in {clock() - seg.t0:.1f}s", file=out, flush=True)
        return seg.done


PART_RE = re.compile(r"[A-Za-z0-9][A-Za-z0-9._-]*")  # a plain file name next to the metadata: no separators, no ".."


def _is_link(p) -> bool:
    """Symlink or Windows reparse point (junction): never written through."""
    try:
        st = os.lstat(p)
    except OSError:
        return False
    return os.path.islink(p) or bool(getattr(st, "st_file_attributes", 0) & 0x400)


def load_runtime_meta(meta_path, meta_sha256: str, runtime_id: str) -> tuple[list, int]:
    """Read + verify the small runtime companion metadata against the hash and runtime_id EMBEDDED in the installer, and check
    that every listed part exists with exactly the recorded size. Reads no part bytes. Returns ([(path, size, sha256)], total)."""
    raw = open(meta_path, "rb").read()
    if hashlib.sha256(raw).hexdigest() != meta_sha256:
        raise ValueError("runtime companion metadata does not match the checksum built into this installer (modified or wrong file)")
    m = json.loads(raw)
    if m.get("schema") != 2 or m.get("runtime_id") != runtime_id or not m.get("shards"):
        raise ValueError("runtime companion metadata is for a different runtime or is malformed")
    base, shards, total, owned = os.path.dirname(os.path.abspath(meta_path)), [], 0, set()
    for e in m["shards"]:
        name, size, sha, files = e.get("name"), e.get("size"), e.get("sha256"), e.get("files")
        if not isinstance(name, str) or not PART_RE.fullmatch(name) or ".." in name or not isinstance(size, int) or size <= 0 \
                or not isinstance(sha, str) or not re.fullmatch(r"[0-9a-f]{64}", sha) or not isinstance(files, list) or not files:
            raise ValueError(f"runtime companion metadata has an invalid shard entry {name!r}")
        for rel in files:  # the files this shard (and only it) may create: plain relative paths under runtime/
            if not isinstance(rel, str) or not rel.startswith("runtime/") or "\\" in rel or ":" in rel or \
                    any(p in ("", ".", "..") for p in rel.split("/")):
                raise ValueError(f"runtime companion metadata lists an unsafe path {rel!r}")
            if rel in owned:
                raise ValueError(f"runtime companion metadata assigns {rel!r} to more than one shard")
            owned.add(rel)
        p = os.path.join(base, name)
        if not os.path.isfile(p):
            raise FileNotFoundError(f"runtime companion shard is missing: {name} (keep every file next to the setup program)")
        if os.path.getsize(p) != size:
            raise ValueError(f"runtime companion shard {name} has {os.path.getsize(p)} bytes, expected {size} (incomplete download?)")
        shards.append({"path": p, "name": name, "size": size, "sha256": sha, "files": files})
        total += size
    return shards, total


WORKERS = 8      # default concurrency of the runtime extraction (one bounded pool, one task per shard)
MAX_WORKERS = 16


def clamp_workers(n) -> int:
    return max(WORKERS, min(MAX_WORKERS, int(n)))


class ShardReader:
    """Read one shard file: counts bytes, hashes them, checks the exact size, and stops at the cancel flag."""

    def __init__(self, shard, stop, t0: float, clock=time.monotonic, opener=open):
        self.name, self.size, self.sha, self.stop, self.t0, self.clock = \
            shard["name"], shard["size"], shard["sha256"], stop, t0, clock
        self.f, self.h, self.done, self.left, self.started = opener(shard["path"], "rb"), hashlib.sha256(), 0, shard["size"], False

    def read(self, n: int = -1) -> bytes:
        self.started = True
        if self.stop.is_set():
            raise InterruptedError("cancelled because another worker failed")
        if self.left == 0:
            return b""
        b = self.f.read(self.left if n is None or n < 0 else min(n, self.left))
        if not b:
            raise EOFError(f"runtime companion shard {self.name} ended early")
        self.h.update(b)
        self.left -= len(b)
        self.done += len(b)
        return b

    def finish(self):
        self.f.close()
        if self.left or self.h.hexdigest() != self.sha:
            raise ValueError(f"runtime companion shard {self.name} checksum mismatch (damaged or modified)")


def extract_runtime(meta_path, meta_sha256: str, runtime_id: str, dest, label: str = "runtime",
                    out=sys.stdout, clock=time.monotonic, opener=open, workers: int = WORKERS) -> int:
    """Extract the independently compressed shards IN PARALLEL (one bounded pool, one task per shard, disjoint files) into
    <dest>/.runtime-extract/runtime and, only after every shard verified and consumed, rename the holder's runtime/ to
    <dest>/runtime in one step. Everything order-dependent stays serial and after the workers: directory pre-creation before
    them, the final rename after them. Any worker error cancels the rest, nothing becomes <dest>/runtime, and the
    caller's supervisor discards the private holder. Never writes through a link; creates no symlinks."""
    dest = os.path.abspath(dest)
    holder, final = os.path.join(dest, ".runtime-extract"), os.path.join(dest, "runtime")
    if _is_link(dest) or not os.path.isdir(dest):
        raise ValueError(f"{dest} is not a plain directory")
    if os.path.lexists(holder) or (os.path.lexists(final) and not _is_link(final)):
        raise FileExistsError(f"{final} (or its holder) already exists; refusing to overwrite it")
    if _is_link(final):
        raise FileExistsError(f"{final} is a link; a runtime is already attached")
    shards, total = load_runtime_meta(meta_path, meta_sha256, runtime_id)  # metadata + sizes only: no shard byte is read yet
    n = min(clamp_workers(workers), len(shards))
    stop, t0 = threading.Event(), clock()
    readers: list = [None] * len(shards)

    def tick():
        seen = -1
        while not stop.wait(1):
            live = [r for r in readers if r is not None]
            done = sum(r.done for r in live)
            if done == seen:  # nothing moved: stay silent so the supervisor's stall guard can see it
                continue
            seen = done
            el = clock() - t0
            per = " ".join(f"{r.name[-7:-4]}:{r.done >> 20}/{r.size >> 20}" for r in live if r.left)
            print(f"[{label}] {done / 1048576:.0f}/{total / 1048576:.0f} MiB read by {n} worker(s), {done / 1048576 / max(el, 1e-9):.1f} MiB/s, "
                  f"elapsed {el:.0f}s, shards done {sum(1 for r in live if not r.left)}/{len(shards)}; active MiB {per or '-'}",
                  file=out, flush=True)

    def work(i):
        sh = shards[i]
        rd = readers[i] = ShardReader(sh, stop, t0, clock, opener)
        want, seen = set(sh["files"]), set()

        def check(member, path):
            if member.name not in want or member.name in seen or not member.isfile():
                raise ValueError(f"unexpected, duplicate or unsafe member {member.name!r} in shard {sh['name']}")
            seen.add(member.name)
            return tarfile.data_filter(member, path)
        with tarfile.open(fileobj=rd, mode="r|gz") as tar:
            tar.extractall(holder, filter=check)
        while rd.read(1 << 20):  # gzip trailer/padding: the hash covers the whole shard
            pass
        rd.finish()
        if seen != want:
            raise ValueError(f"shard {sh['name']} lacks {len(want - seen)} of the files the metadata assigns to it")
        return rd.done

    threading.Thread(target=tick, daemon=True).start()
    os.mkdir(holder)
    pool, futs = ThreadPoolExecutor(max_workers=n), []
    try:
        for d in sorted({os.path.dirname(rel) for sh in shards for rel in sh["files"]}):  # serial: no concurrent mkdir races
            os.makedirs(os.path.join(holder, *d.split("/")), exist_ok=True)
        futs = [pool.submit(work, i) for i in range(len(shards))]
        done, pending = wait(futs, return_when=FIRST_EXCEPTION)
        if any(f.exception() is not None for f in done):
            stop.set()  # cancel: running workers raise at their next read, queued ones never start
            for f in pending:
                f.cancel()
    finally:
        stop.set()
        pool.shutdown(wait=True)  # no worker outlives this call
    errs = [f.exception() for f in futs if not f.cancelled() and f.exception() is not None]
    if errs:  # report the root cause, not the cancellations it triggered
        raise next((e for e in errs if not isinstance(e, InterruptedError)), errs[0])
    if not os.path.isdir(os.path.join(holder, "runtime")):
        raise ValueError("runtime shards hold no runtime/ directory")
    os.rename(os.path.join(holder, "runtime"), final)  # serial, last: O(1)
    os.rmdir(holder)
    print(f"[{label}] verified and extracted {total >> 20} MiB in {clock() - t0:.1f}s with {n} worker(s)", file=out, flush=True)
    return total


def discard_holder(dest) -> None:
    """After a failed/killed extraction: remove ONLY the private holder this command created (never a link, never `runtime`)."""
    holder = os.path.join(os.path.abspath(dest), ".runtime-extract")
    if os.path.lexists(holder) and not _is_link(holder):
        shutil.rmtree(holder, ignore_errors=True)


_UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{3F9A2C71-8E4B-4D6F-B1A5-7C0E92D4A6B3}_is1"


def _reg_app_path(hive_name: str):
    """Read-only: the full installer's recorded install folder in one registry hive, or None."""
    import winreg
    try:
        with winreg.OpenKey(getattr(winreg, hive_name), _UNINSTALL_KEY) as k:
            return str(winreg.QueryValueEx(k, "Inno Setup: App Path")[0])
    except OSError:
        return None


def _trim(p: str) -> str:
    return p if len(p.rstrip("\\/")) <= 2 else p.rstrip("\\/")  # keep a drive root's backslash like Inno's RemoveBackslashUnlessRoot


def discover_install(prefix_param, localappdata, probe_reg=_reg_app_path, isdir=os.path.isdir, isfile=os.path.isfile) -> tuple[str, str]:
    """-> (prefix, source): /PREFIX, else the Keyloom folder the full installer recorded (only if it holds a receipt), else the default."""
    if prefix_param:
        return _trim(prefix_param), "param"
    for hive in ("HKEY_CURRENT_USER", "HKEY_LOCAL_MACHINE"):
        p = probe_reg(hive)
        if p and isdir(p) and isfile(ntpath.join(p, "receipt.json")):
            return _trim(p), hive
    return ntpath.join(localappdata, "Programs", "Keyloom"), "default"


def write_attestation(lifecycle, payload, producer: str) -> str:
    """Call the lifecycle's ONE attestation writer (never a hand-rolled copy of its schema). It authenticates nothing itself."""
    import importlib.util
    spec = importlib.util.spec_from_file_location("ck_lifecycle_attest", lifecycle)
    lc = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = lc
    spec.loader.exec_module(lc)
    from pathlib import Path
    root = Path(payload).absolute()
    return lc.write_attestation(root, lc.read_manifest(root), producer)


LOG = None  # open install.log (append, line-buffered) when --log is given; set by main()
LOG_TITLE = ""
_PART = ""  # the unterminated tail of the install log: stamped when its line ends (log_flush)
_LOG_LOCK = threading.RLock()  # re-entrant: log_flush calls log_write, and a signal handler may log on the main thread
WAIT_REPEAT = 20  # seconds between repeats of an unchanged wait reason
HEARTBEAT_LOG_EVERY = 30  # every Nth heartbeat is also copied into the log; the console gets each one


def log_write(text: str) -> None:
    global LOG, _PART
    with _LOG_LOCK:
        if not LOG:
            return
        try:
            *lines, _PART = (_PART + text).split("\n")
            for line in lines:
                LOG.write(f"{time.strftime('%Y-%m-%d %H:%M:%S %z')} {line}\n")
        except (OSError, ValueError) as e:  # a log that cannot be written never blocks the install, but it says so once
            print(f"WARNING: writing install log {LOG.name} failed: {type(e).__name__}: {e}", file=sys.stderr)
            LOG = None


def log_flush() -> None:
    with _LOG_LOCK:
        if _PART:
            log_write("\n")


def log_exc(context: str, e: BaseException) -> None:
    """The contract's exception record: `ERROR: <context>: <Type>: <message>`, then the whole traceback."""
    log_write(f"ERROR: {context}: {type(e).__name__}: {e}\n{''.join(traceback.format_exception(e))}")


def open_log(path: str, title: str) -> None:
    global LOG, LOG_TITLE
    try:
        os.makedirs(os.path.dirname(os.path.abspath(path)), exist_ok=True)
        LOG, LOG_TITLE = open(path, "a", encoding="utf-8", buffering=1), title
    except OSError as e:  # a log that cannot be written never blocks the install
        print(f"WARNING: install log {path} not writable ({type(e).__name__}: {e})", file=sys.stderr)


def supervise(argv, label: str, out=sys.stdout, clock=time.monotonic, poll: float = 1.0,
              wait_file=None, grace: float = 3.0, stall: float = STALL_SECONDS, **popen) -> int:
    """Run argv as the leader of its OWN session/process group, relay its output live and print a heartbeat every `poll` s
    (liveness only, never progress). Any output (or a declared wait) is progress; after `stall` s without it, or on a signal
    or any error, SIGTERM the group (so a lifecycle can unwind its transaction), then SIGKILL it after `grace` s, and reap it.
    Returns the child's exit code or 124. While `wait_file` exists the child has declared an explicit wait for the user (or a
    visible transfer): that counts as progress and the heartbeat says so. Automatic work never creates it."""
    log_write(f"\n===== {time.strftime('%Y-%m-%d %H:%M:%S %z')} {LOG_TITLE} step={label} =====\n")

    def note(msg: str) -> None:
        print(msg, file=out, flush=True)
        log_flush()  # a partial child line must end before this supervisor line starts
        log_write(msg + "\n")
    env = dict(popen.get("env") or os.environ)
    if wait_file:  # the child (lifecycle) creates this file only while it waits for the user / runs a visible transfer
        env["KEYLOOM_WAIT_FILE"] = str(wait_file)
    popen["env"] = env
    win = os.name == "nt"
    popen.update(creationflags=subprocess.CREATE_NEW_PROCESS_GROUP) if win else popen.update(start_new_session=True)
    log_write(f"[{label}] run: {' '.join(map(str, argv))}\n")
    try:
        p = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, **popen)
    except OSError as e:
        note(f"FAILED: {label}: cannot start {argv[0]}: {type(e).__name__}: {e}")
        log_exc(f"{label}: cannot start {argv[0]}", e)
        return 127
    t0 = last = clock()

    def relay():
        nonlocal last
        fd, dec = p.stdout.fileno(), codecs.getincrementaldecoder("utf-8")("replace")  # keeps a split multibyte char whole
        broken = False

        def show(text):
            nonlocal broken
            try:
                out.write(text)
                out.flush()
            except Exception as e:  # a dead relay would block the child on a full pipe: keep draining, say it once
                if not broken:
                    broken = True
                    with contextlib.suppress(Exception):
                        print(f"WARNING: [{label}] console output failed: {type(e).__name__}: {e}", file=sys.stderr)
                    log_exc(f"{label}: console output", e)
            log_write(text)
        while chunk := os.read(fd, 65536):  # raw chunks, not lines: a prompt without a newline must show at once
            last = clock()
            show(dec.decode(chunk))
        show(dec.decode(b"", final=True))  # a multibyte char cut at EOF must not vanish
        log_flush()
    rt = threading.Thread(target=relay, daemon=True)
    rt.start()

    def reap(term_wait=grace):
        if win:  # no process groups to signal: stop exactly this child's tree
            r = subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True, text=True, errors="replace")
            log_write(f"[{label}] taskkill exit {r.returncode}\n")
            for stream, text in (("stdout", r.stdout), ("stderr", r.stderr)):
                for line in text.splitlines():
                    log_write(f"[{label}] taskkill {stream}| {line}\n")
            if r.returncode not in (0, 128):  # 128: the tree was already gone
                note(f"WARNING: [{label}] taskkill exit {r.returncode}")
            p.wait()
            return
        try:
            if term_wait:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(timeout=term_wait)
        except subprocess.TimeoutExpired:
            pass  # still running after the grace period: SIGKILL below
        except ProcessLookupError:
            pass  # the group is already gone after a normal exit: expected
        except PermissionError as e:
            log_write(f"[{label}] killpg SIGTERM: {type(e).__name__}: {e}\n")
        try:
            os.killpg(p.pid, signal.SIGKILL)  # whatever of the group is left
        except ProcessLookupError:
            pass  # the group is already gone after a normal exit: expected
        except PermissionError as e:
            log_write(f"[{label}] killpg SIGKILL: {type(e).__name__}: {e}\n")
        p.wait()

    def on_signal(signum, _frame):
        reap()
        raise SystemExit(128 + signum)
    old = {}
    for s in [getattr(signal, n) for n in ("SIGTERM", "SIGHUP", "SIGINT") if hasattr(signal, n)]:
        try:
            old[s] = signal.signal(s, on_signal)
        except ValueError:  # not the main thread
            pass
    shown_why, shown_at, beats = None, 0.0, 0
    try:
        while True:
            try:
                rc = p.wait(timeout=poll)
                reap()  # stray grandchildren of a finished child die with its group
                rt.join(5)  # the rest of its output
                note(f"[{label}] exit {rc} after {clock() - t0:.0f}s")
                return rc
            except subprocess.TimeoutExpired:
                pass
            now = clock()
            waiting = wait_file and os.path.exists(wait_file)
            if waiting:
                last = now
            if now - last >= stall:
                reap()
                note(f"FAILED: {label}: no progress for {stall:g}s; stopped and reaped")
                return 124
            if waiting:
                try:
                    why = open(wait_file).read().strip()[:200]
                except OSError:
                    why = ""
                why = why or "no reason given"
                if why != shown_why or now - shown_at >= WAIT_REPEAT:  # said once, then every WAIT_REPEAT s: it must not drown in heartbeats
                    note(f"[{label}] WAITING FOR USER/TRANSFER ({why})")
                    shown_why, shown_at = why, now
            else:
                if shown_why is not None:
                    note(f"[{label}] wait finished ({shown_why})")
                    shown_why = None
                beat = f"[{label}] supervisor heartbeat: running {now - t0:.0f}s, last output {now - last:.0f}s ago (liveness only)"
                print(beat, file=out, flush=True)
                beats += 1
                if beats % HEARTBEAT_LOG_EVERY == 0:
                    log_flush()  # never merge into a partial child line
                    log_write(beat + "\n")
    except BaseException:
        reap()
        raise
    finally:
        for s, h in old.items():
            signal.signal(s, h)


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["extract", "extract-worker", "run", "runtime-extract", "runtime-extract-worker", "discover", "attest"])
    ap.add_argument("--lifecycle")
    ap.add_argument("--producer", default="segments")
    ap.add_argument("--prefix-param", default="")
    ap.add_argument("--out")
    ap.add_argument("--workers", type=int, default=WORKERS)
    ap.add_argument("--meta")
    ap.add_argument("--meta-sha256")
    ap.add_argument("--runtime-id")
    ap.add_argument("--run")
    for k in ("base", "offset", "len"):
        ap.add_argument(f"--{k}", type=int)
    ap.add_argument("--sha256")
    ap.add_argument("--dest")
    ap.add_argument("--label", default="step")
    ap.add_argument("--require")
    ap.add_argument("--forbid")
    ap.add_argument("--wait-file")
    ap.add_argument("--log")
    ap.add_argument("--log-title", default="")
    ap.add_argument("rest", nargs="*")
    err = io.StringIO()
    try:
        with contextlib.redirect_stderr(err):  # argparse writes its usage errors to stderr: the log keeps them too
            a, extra = ap.parse_known_args(argv)
    except SystemExit as e:
        if e.code:  # a usage error, not --help
            sys.stderr.write(err.getvalue())
            args = sys.argv[1:] if argv is None else argv
            if "--log" in args[:-1]:
                open_log(args[args.index("--log") + 1], "")
            log_write(f"ERROR: usage: {err.getvalue().strip()}\n")
        raise
    if a.log:
        open_log(a.log, a.log_title)
    if a.cmd == "run":  # run -- CMD ...: stall-guarded, heartbeat-supervised foreign command (exit code passed through)
        cmd = a.rest + extra  # everything after `--` lands in the positional list (argparse version dependent)
        cmd = cmd[1:] if cmd[:1] == ["--"] else cmd
        if not cmd:
            log_write("ERROR: usage: run needs -- COMMAND\n")
            ap.error("run needs -- COMMAND")
        return supervise(cmd, a.label, wait_file=a.wait_file)
    if a.cmd == "attest":  # writes the lifecycle's attestation marker; the caller invokes it ONLY after every runtime byte was authenticated
        try:
            nonce = write_attestation(a.lifecycle, a.dest, a.producer)
            with open(a.out, "w", encoding="utf-8") as f:
                f.write(nonce + "\n")
        except Exception as e:  # noqa: BLE001 - any failure means "no attestation": the lifecycle then verifies the runtime itself
            print(f"FAILED: attest: {type(e).__name__}: {e}", file=sys.stderr)
            traceback.print_exc()
            log_exc("attest", e)
            return 1
        log_write(f"attest: nonce written to {a.out}\n")
        return 0
    if a.cmd == "discover":  # read-only; writes "prefix\nsource\n" for the Inno wrapper (no JSON parsing in Pascal)
        try:
            prefix, source = discover_install(a.prefix_param, os.environ["LOCALAPPDATA"])
            with open(a.out, "w", encoding="utf-8") as f:
                f.write(prefix + "\n" + source + "\n")
            log_write(f"discover: prefix={prefix} source={source}\n")
        except Exception as e:  # noqa: BLE001 - the Inno wrapper stops on any failure; the log keeps the cause
            print(f"FAILED: discover: {type(e).__name__}: {e}", file=sys.stderr)
            traceback.print_exc()
            log_exc("discover", e)
            return 1
        return 0
    if a.cmd == "runtime-extract":  # supervisor of the worker; discards the private holder if the worker failed or was killed
        w = [sys.executable, "-I", "-B", __file__, "runtime-extract-worker", "--meta", a.meta, "--meta-sha256", a.meta_sha256,
             "--runtime-id", a.runtime_id, "--dest", a.dest, "--label", a.label,
             "--workers", str(clamp_workers(a.workers))]
        rc = supervise(w, a.label)
        if rc != 0:
            discard_holder(a.dest)
        return rc
    if a.cmd == "runtime-extract-worker":
        try:
            extract_runtime(a.meta, a.meta_sha256, a.runtime_id, a.dest, a.label, workers=a.workers)
        except (OSError, EOFError, ValueError, tarfile.TarError) as e:
            print(f"FAILED: {a.label}: {type(e).__name__}: {e}", file=sys.stderr)
            traceback.print_exc()
            return 1
        return 0
    seg = (a.run, a.base, a.offset, a.len, a.sha256, a.dest, a.label)
    if a.cmd == "extract":  # supervisor: the blocked-I/O-proof owner of the worker
        w = [sys.executable, "-I", "-B", __file__, "extract-worker", "--run", a.run, "--base", str(a.base), "--offset", str(a.offset),
             "--len", str(a.len), "--sha256", a.sha256, "--dest", a.dest, "--label", a.label]
        for k in ("require", "forbid"):
            if getattr(a, k):
                w += [f"--{k}", getattr(a, k)]
        return supervise(w, a.label)
    try:
        extract_segment(*seg, require=a.require, forbid=a.forbid)
    except (OSError, EOFError, ValueError, tarfile.TarError) as e:
        print(f"FAILED: {a.label}: {type(e).__name__}: {e}", file=sys.stderr)
        traceback.print_exc()
        return 1
    return 0


if __name__ == "__main__":
    try:
        rc = main()
    except SystemExit as e:  # usage errors and signal exits bypass the next clause: the log keeps the code
        if e.code:
            log_write(f"exit {e.code}\n")
        log_flush()
        raise
    except Exception as e:  # noqa: BLE001 - the parent relays stderr; the log keeps the same record
        traceback.print_exc()
        log_exc("payload", e)
        rc = 1
    log_flush()
    sys.exit(rc)
