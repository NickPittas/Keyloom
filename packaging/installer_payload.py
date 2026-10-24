"""Verify + extract ONE independently compressed segment of a Keyloom .run installer (stdlib only; runs on the bundled bootstrap).

    installer_payload.py extract --run FILE --base STUB_BYTES --offset N --len N --sha256 H --dest DIR --label NAME
                                 [--deadline SECONDS] [--require PREFIX] [--forbid PREFIX]

Only the bytes [base+offset, base+offset+len) of FILE are ever read, so an update that skips the runtime segment never touches it.
The segment is hashed while it streams through tar (single pass); a mismatch fails BEFORE the caller activates anything.
A ticker prints the actual bytes read every second. `extract` runs the work in a supervised child process group (heartbeat each\nsecond; killed and reaped on the deadline, exit 124); --deadline is mandatory. `run` supervises any command the same way and\n`limit` computes a phase's seconds from the shared total budget.
"""
from __future__ import annotations

import argparse
import hashlib
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
from concurrent.futures import FIRST_EXCEPTION, ThreadPoolExecutor, wait


class Segment:
    """Read-only window over one segment that hashes and counts what tar consumes."""

    def __init__(self, f, start: int, length: int, deadline: float, clock=time.monotonic):
        self.f, self.left, self.total, self.done = f, length, length, 0
        self.h, self.clock, self.t0, self.deadline = hashlib.sha256(), clock, clock(), deadline
        f.seek(start)

    def read(self, n: int = -1) -> bytes:
        if self.deadline and self.clock() - self.t0 > self.deadline:
            raise TimeoutError(f"phase exceeded its {self.deadline:g}s limit after {self.done >> 20} MiB")
        n = self.left if n is None or n < 0 else min(n, self.left)
        b = self.f.read(n)
        if len(b) < n:
            raise EOFError("installer is truncated")
        self.left -= len(b)
        self.done += len(b)
        self.h.update(b)
        return b


def extract_segment(run, base: int, offset: int, length: int, sha256: str, dest, label: str, deadline: float = 0,
                    require: str | None = None, forbid: str | None = None, out=sys.stdout, opener=open,
                    clock=time.monotonic) -> int:
    """Returns bytes read. Raises on tamper/truncation/unsafe member/deadline."""
    def check(member, path):
        n = member.name.rstrip("/") + "/"  # the root dir entry "payload" matches "payload/"
        if require and not n.startswith(require) or forbid and n.startswith(forbid):
            raise ValueError(f"unexpected member {member.name!r} in the {label} segment")
        return tarfile.data_filter(member, path)

    with opener(run, "rb") as f:
        seg = Segment(f, base + offset, length, deadline, clock)
        stop = threading.Event()

        def tick():
            while not stop.wait(1):
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
    """Read one shard file: counts bytes, hashes them, checks the exact size, and stops at the shared deadline / cancel flag."""

    def __init__(self, shard, stop, deadline: float, t0: float, clock=time.monotonic, opener=open):
        self.name, self.size, self.sha, self.stop, self.deadline, self.t0, self.clock = \
            shard["name"], shard["size"], shard["sha256"], stop, deadline, t0, clock
        self.f, self.h, self.done, self.left, self.started = opener(shard["path"], "rb"), hashlib.sha256(), 0, shard["size"], False

    def read(self, n: int = -1) -> bytes:
        self.started = True
        if self.stop.is_set():
            raise InterruptedError("cancelled because another worker failed or the limit was reached")
        if self.deadline and self.clock() - self.t0 > self.deadline:
            raise TimeoutError(f"phase exceeded its {self.deadline:g}s limit")
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


def extract_runtime(meta_path, meta_sha256: str, runtime_id: str, dest, label: str = "runtime", deadline: float = 0,
                    out=sys.stdout, clock=time.monotonic, opener=open, workers: int = WORKERS) -> int:
    """Extract the independently compressed shards IN PARALLEL (one bounded pool, one task per shard, disjoint files) into
    <dest>/.runtime-extract/runtime and, only after every shard verified and consumed, rename the holder's runtime/ to
    <dest>/runtime in one step. Everything order-dependent stays serial and after the workers: directory pre-creation before
    them, the final rename after them. Any worker error / the deadline cancels the rest, nothing becomes <dest>/runtime, and the
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
        while not stop.wait(1):
            el = clock() - t0
            live = [r for r in readers if r is not None]
            done = sum(r.done for r in live)
            per = " ".join(f"{r.name[-7:-4]}:{r.done >> 20}/{r.size >> 20}" for r in live if r.left)
            print(f"[{label}] {done / 1048576:.0f}/{total / 1048576:.0f} MiB read by {n} worker(s), {done / 1048576 / max(el, 1e-9):.1f} MiB/s, "
                  f"elapsed {el:.0f}s, shards done {sum(1 for r in live if not r.left)}/{len(shards)}; active MiB {per or '-'}",
                  file=out, flush=True)

    def work(i):
        sh = shards[i]
        rd = readers[i] = ShardReader(sh, stop, deadline, t0, clock, opener)
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


_UNINSTALL_KEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{7C1E6C52-3B0F-4D5A-9E57-2D6C7A0B7E21}_is1"


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


def discover_install(prefix_param: str, localappdata: str, probe_reg=_reg_app_path, isfile=os.path.isfile, isdir=os.path.isdir,
                     workers: int = WORKERS) -> tuple[str, str]:
    """Which prefix does this online setup install into, and which pre-rename install does it adopt? ONE read-only operation.
    Stage 1 runs the INDEPENDENT probes (HKCU key, HKLM key, new-folder receipt, old-folder receipt) in the bounded pool; stage 2
    is the one probe that needs a stage-1 result (does the recorded folder exist). Precedence is fixed in code, never by completion
    order: /PREFIX > HKCU > HKLM > default new folder, with adoption of the old default folder exactly as the Pascal logic had it.
    Nothing is written, repaired or migrated here. Returns (prefix, adopt) - adopt is '' when there is none."""
    new = ntpath.join(localappdata, "Programs", "Keyloom")
    old = ntpath.join(localappdata, "Programs", "CorridorKey")
    probes = {"hkcu": lambda: probe_reg("HKEY_CURRENT_USER"), "hklm": lambda: probe_reg("HKEY_LOCAL_MACHINE"),
              "new": lambda: isfile(ntpath.join(new, "receipt.json")), "old": lambda: isfile(ntpath.join(old, "receipt.json"))}
    with ThreadPoolExecutor(max_workers=min(clamp_workers(workers), len(probes))) as pool:
        futs = {k: pool.submit(f) for k, f in probes.items()}
        r = {k: f.result() for k, f in futs.items()}  # collected by name: completion order cannot matter
    adopt, prefix = "", prefix_param
    if not prefix:
        recorded = r["hkcu"] if r["hkcu"] is not None else r["hklm"]
        if recorded and isdir(recorded):  # stage 2: depends on the registry result
            prefix = recorded
        if prefix and ntpath.normcase(_trim(prefix)) == ntpath.normcase(old) and not r["new"]:
            adopt, prefix = old, new
        if not prefix:
            prefix = new
            if not r["new"] and r["old"]:
                adopt = old
    return _trim(prefix), adopt


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


def phase_limit(start: float, total: float, phase: float, now: float | None = None) -> int:
    """Seconds this phase may take: the phase cap, but never more than what is left of the shared total budget
    (`start` = wall-clock epoch of the install's first step). Raises TimeoutError when the budget is spent."""
    left = total - ((time.time() if now is None else now) - start)
    if int(min(phase, left)) < 1:
        raise TimeoutError(f"the {total:g}s local install budget is spent")
    return int(min(phase, left))


def supervise(argv, label: str, deadline: float, out=sys.stdout, clock=time.monotonic, poll: float = 1.0,
              wait_file=None, grace: float = 3.0, used_file=None, **popen) -> int:
    """Run argv as the leader of its OWN session/process group; print a heartbeat every `poll` s (liveness only, never
    progress); on the deadline, a signal or any error SIGTERM the group (so a lifecycle can unwind its transaction), then
    SIGKILL it after `grace` s, and reap it. Returns the child's exit code or 124.
    While `wait_file` exists the child has declared an explicit wait for the user (or a visible transfer): the deadline
    clock is paused and the heartbeat says so. Automatic work never creates it."""
    if not deadline or deadline <= 0:
        raise ValueError("a deadline is mandatory")
    if wait_file:  # the child (lifecycle) creates this file only while it waits for the user / runs a visible transfer
        popen["env"] = dict(popen.get("env") or os.environ, KEYLOOM_WAIT_FILE=str(wait_file))
    win = os.name == "nt"
    popen.update(creationflags=subprocess.CREATE_NEW_PROCESS_GROUP) if win else popen.update(start_new_session=True)
    p = subprocess.Popen(argv, **popen)
    t0 = last = clock()
    active = 0.0

    def reap(term_wait=grace):
        if win:  # no process groups to signal: stop exactly this child's tree
            subprocess.run(["taskkill", "/PID", str(p.pid), "/T", "/F"], capture_output=True)
            p.wait()
            return
        try:
            if term_wait:
                os.killpg(p.pid, signal.SIGTERM)
                p.wait(timeout=term_wait)
        except (ProcessLookupError, PermissionError, subprocess.TimeoutExpired):
            pass
        try:
            os.killpg(p.pid, signal.SIGKILL)  # whatever of the group is left
        except (ProcessLookupError, PermissionError):
            pass
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
    try:
        while True:
            try:
                rc = p.wait(timeout=poll)
                reap()  # stray grandchildren of a finished child die with its group
                return rc
            except subprocess.TimeoutExpired:
                pass
            now = clock()
            waiting = wait_file and os.path.exists(wait_file)
            if not waiting:
                active += now - last
            last = now
            if active >= deadline:
                reap()
                print(f"FAILED: {label}: exceeded its {deadline:g}s limit; stopped and reaped", file=out, flush=True)
                return 124
            if waiting:
                try:
                    why = open(wait_file).read().strip()[:200]
                except OSError:
                    why = ""
                print(f"[{label}] WAITING FOR USER/TRANSFER ({why or 'no reason given'}); time limit paused at "
                      f"{active:.0f}s of {deadline:g}s", file=out, flush=True)
            else:
                print(f"[{label}] supervisor heartbeat: running {active:.0f}s of {deadline:g}s (liveness only)", file=out, flush=True)
    except BaseException:
        reap()
        raise
    finally:
        for s, h in old.items():
            signal.signal(s, h)
        if used_file:  # seconds of automatic work (user-wait excluded) for the caller's shared budget
            try:
                with open(used_file, "w") as f:
                    f.write(str(int(active + 0.999)))
            except OSError:
                pass


def main(argv=None) -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("cmd", choices=["extract", "extract-worker", "run", "limit", "runtime-extract", "runtime-extract-worker", "discover", "attest"])
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
    ap.add_argument("--deadline", type=float, default=0)
    ap.add_argument("--require")
    ap.add_argument("--forbid")
    ap.add_argument("--wait-file")
    ap.add_argument("--used-file")
    ap.add_argument("--start", type=float)
    ap.add_argument("--total", type=float, default=30)
    ap.add_argument("--phase", type=float, default=10)
    ap.add_argument("rest", nargs="*")
    a, extra = ap.parse_known_args(argv)
    if a.cmd == "limit":
        try:
            print(phase_limit(a.start, a.total, a.phase))
        except TimeoutError as e:
            print(f"FAILED: {e}", file=sys.stderr)
            return 124
        return 0
    if a.cmd == "run":  # run -- CMD ...: bounded, heartbeat-supervised foreign command (exit code passed through)
        cmd = a.rest + extra  # everything after `--` lands in the positional list (argparse version dependent)
        cmd = cmd[1:] if cmd[:1] == ["--"] else cmd
        if a.deadline <= 0 or not cmd:
            ap.error("run needs --deadline > 0 and -- COMMAND")
        return supervise(cmd, a.label, a.deadline, wait_file=a.wait_file, used_file=a.used_file)
    if a.deadline <= 0:
        ap.error("--deadline > 0 is mandatory (no unlimited phase)")
    if a.cmd == "attest":  # writes the lifecycle's attestation marker; the caller invokes it ONLY after every runtime byte was authenticated
        try:
            nonce = write_attestation(a.lifecycle, a.dest, a.producer)
            with open(a.out, "w", encoding="utf-8") as f:
                f.write(nonce + "\n")
        except Exception as e:  # noqa: BLE001 - any failure means "no attestation": the lifecycle then verifies the runtime itself
            print(f"FAILED: attest: {type(e).__name__}: {e}", file=sys.stderr)
            return 1
        return 0
    if a.cmd == "discover":  # read-only; writes "prefix\nadopt\n" for the Inno wrapper (no JSON parsing in Pascal)
        try:
            prefix, adopt = discover_install(a.prefix_param, os.environ["LOCALAPPDATA"], workers=a.workers)
            with open(a.out, "w", encoding="utf-8") as f:
                f.write(prefix + "\n" + adopt + "\n")
            if a.used_file:
                open(a.used_file, "w").write("1")
        except (OSError, KeyError) as e:
            print(f"FAILED: discover: {type(e).__name__}: {e}", file=sys.stderr)
            return 1
        return 0
    if a.cmd == "runtime-extract":  # supervisor of the worker; discards the private holder if the worker failed or was killed
        w = [sys.executable, "-I", "-B", __file__, "runtime-extract-worker", "--meta", a.meta, "--meta-sha256", a.meta_sha256,
             "--runtime-id", a.runtime_id, "--dest", a.dest, "--label", a.label, "--deadline", str(a.deadline),
             "--workers", str(clamp_workers(a.workers))]
        rc = supervise(w, a.label, a.deadline, used_file=a.used_file)
        if rc != 0:
            discard_holder(a.dest)
        return rc
    if a.cmd == "runtime-extract-worker":
        try:
            extract_runtime(a.meta, a.meta_sha256, a.runtime_id, a.dest, a.label, a.deadline, workers=a.workers)
        except TimeoutError as e:
            print(f"FAILED: {a.label}: {e}", file=sys.stderr)
            return 124
        except (OSError, EOFError, ValueError, tarfile.TarError) as e:
            print(f"FAILED: {a.label}: {type(e).__name__}: {e}", file=sys.stderr)
            return 1
        return 0
    seg = (a.run, a.base, a.offset, a.len, a.sha256, a.dest, a.label)
    if a.cmd == "extract":  # supervisor: the blocked-I/O-proof owner of the worker
        w = [sys.executable, "-I", "-B", __file__, "extract-worker", "--run", a.run, "--base", str(a.base), "--offset", str(a.offset),
             "--len", str(a.len), "--sha256", a.sha256, "--dest", a.dest, "--label", a.label, "--deadline", str(a.deadline)]
        for k in ("require", "forbid"):
            if getattr(a, k):
                w += [f"--{k}", getattr(a, k)]
        return supervise(w, a.label, a.deadline)
    try:
        extract_segment(*seg, deadline=a.deadline, require=a.require, forbid=a.forbid)
    except TimeoutError as e:
        print(f"FAILED: {a.label}: {e}", file=sys.stderr)
        return 124
    except (OSError, EOFError, ValueError, tarfile.TarError) as e:
        print(f"FAILED: {a.label}: {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
