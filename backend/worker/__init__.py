"""Persistent Key worker: strict JSON-lines protocol v1 over stdin/stdout (one job at a time).

stdout carries protocol lines only (``__main__`` points ``sys.stdout`` at stderr first); logs go
to stderr, which the launching app must drain. stdin EOF means cancel and exit. This package
imports only the stdlib until a job arrives, so ``python -m backend.worker`` can capture stdout
before any heavy import prints.
"""
from __future__ import annotations

import hashlib
import json
import os
import socket
import sys
import threading
import time

PROTOCOL = 1
MAX_LINE = 8 * 1024 * 1024  # a longer line is a protocol violation, not something to buffer


def _worker_rev() -> str:
    """Commit of the checkout, read from .git without spawning git; 'unknown' otherwise."""
    git = os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__)))), ".git")
    try:
        with open(os.path.join(git, "HEAD"), encoding="utf-8") as f:
            head = f.read().strip()
        if not head.startswith("ref: "):
            return head or "unknown"
        with open(os.path.join(git, *head[5:].split("/")), encoding="utf-8") as f:
            return f.read().strip() or "unknown"
    except OSError:
        return "unknown"


def model_sha256(path, cache_dir) -> str | None:
    """SHA-256 of a checkpoint, cached under ``cache_dir`` (never the repo).

    A cache entry is trusted only if path, device, inode, size and mtime_ns all match, and a
    file that changes while it is being hashed yields None (unknown), never a guess."""
    def ident():
        st = os.stat(path)
        return {"path": os.path.realpath(path), "dev": st.st_dev, "ino": st.st_ino,
                "size": st.st_size, "mtime_ns": st.st_mtime_ns}
    cache_file = os.path.join(cache_dir, "model_sha256.json")
    try:
        before = ident()
        try:
            with open(cache_file, encoding="utf-8") as f:
                cache = json.load(f)
        except (OSError, ValueError):
            cache = {}
        ent = cache.get(before["path"]) if isinstance(cache, dict) else None
        if (isinstance(ent, dict) and all(ent.get(k) == v for k, v in before.items())
                and isinstance(ent.get("sha256"), str) and len(ent["sha256"]) == 64):
            return ent["sha256"]
        h = hashlib.sha256()
        with open(path, "rb") as f:
            for chunk in iter(lambda: f.read(4 << 20), b""):
                h.update(chunk)
        if ident() != before:
            return None
        digest = h.hexdigest()
    except OSError as exc:
        sys.stderr.write(f"cannot hash {path}: {exc}\n")
        return None
    try:
        os.makedirs(cache_dir, exist_ok=True)
        tmp = f"{cache_file}.tmp.{os.getpid()}"
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump({**(cache if isinstance(cache, dict) else {}), before["path"]: {**before, "sha256": digest}}, f)
        os.replace(tmp, cache_file)  # our own cache file, not user data
    except OSError as exc:
        sys.stderr.write(f"model hash cache not written: {exc}\n")
    return digest


def probe_hello(gpu_arg: int | None) -> dict:
    """Facts about THIS machine; every unknown stays null/false rather than guessed."""
    from backend.v2_paths import get_v2_asset_paths, validate_v2_assets
    paths = get_v2_asset_paths()
    problems = validate_v2_assets(paths)
    for p in problems:
        sys.stderr.write(f"model asset problem: {p}\n")
    models = {}
    if paths.weights.is_file():
        from backend.service.inference import _scratch_root
        models["corridorkey_v2"] = {
            "path": str(paths.weights), "bytes": paths.weights.stat().st_size,
            "sha256": model_sha256(paths.weights, os.path.join(_scratch_root(), "cache"))}
    gpu = None
    try:
        import torch
        if torch.cuda.is_available():
            idx = torch.cuda.current_device()  # CUDA_VISIBLE_DEVICES already remapped --gpu to 0
            prop = torch.cuda.get_device_properties(idx)
            gpu = {"index": gpu_arg if gpu_arg is not None else idx, "name": prop.name,
                   "vram_gb": round(prop.total_memory / 2**30, 1)}
    except Exception as exc:  # noqa: BLE001 - no usable GPU is a valid answer
        sys.stderr.write(f"GPU probe failed: {exc}\n")
    if gpu is None:
        sys.stderr.write("no usable CUDA GPU: Key is unavailable on this worker\n")
    return {"worker_rev": _worker_rev(), "engine": "corridorkey-v2", "host": socket.gethostname(),
            "platform": sys.platform, "gpu": gpu, "models": models,
            "capabilities": {"key": not problems and gpu is not None, "generate_hint": False,
                             "publish_modes": ["final"]}}


class Worker:
    def __init__(self, service, out, hello: dict):
        self.service, self.out, self.hello = service, out, hello
        self._out_lock = threading.Lock()
        self._state = threading.Lock()  # guards _thread/_cancel/_current/_seen
        self._thread: threading.Thread | None = None
        self._cancel = threading.Event()
        self._current: str | None = None
        self._seen: set[str] = set()
        self._closed = False

    # -- output
    def emit(self, msg: dict) -> None:
        line = json.dumps({"v": PROTOCOL, **msg}, separators=(",", ":")) + "\n"
        with self._out_lock:
            if self._closed:
                return
            try:
                self.out.write(line)
                self.out.flush()
            except (OSError, ValueError):  # app went away: behave like EOF
                self._closed = True
                self._cancel.set()

    def _reject(self, job_id, code: str, message: str) -> None:
        self.emit({"event": "rejected", "job_id": job_id, "code": code, "message": message})

    # -- commands
    def _handle_key(self, msg: dict) -> None:
        from backend.service.key_job import KeyJob, KeyJobRejected, run_key_job
        raw = msg.get("job")
        jid = raw.get("job_id") if isinstance(raw, dict) and isinstance(raw.get("job_id"), str) else None
        try:
            job = KeyJob.from_dict(raw)
        except KeyJobRejected as exc:
            return self._reject(jid, exc.code, exc.message)
        if not self.hello["capabilities"]["key"]:
            return self._reject(jid, "invalid", "Key is unavailable on this worker: CorridorKey 2.0 "
                                                "model files are missing or incomplete")
        known = self.hello["models"].get("corridorkey_v2") or {}
        for name, want in job.require["models"].items():
            if want is not None and (name != "corridorkey_v2" or known.get("sha256") != want):
                return self._reject(jid, "invalid", f"Cannot verify model '{name}' hash on this "
                                                    "worker; refusing to assume it is identical")
        with self._state:
            if self._thread is not None or job.job_id in self._seen:
                return self._reject(jid, "busy", "Worker is running a job or already ran this job_id")
            self._cancel = cancel = threading.Event()
            self._current = job.job_id

            def emit(m):
                if m.get("event") == "accepted":
                    with self._state:
                        self._seen.add(job.job_id)
                self.emit(m)

            def run():
                try:
                    done = run_key_job(self.service, job, emit=emit, cancel=cancel, worker={
                        "host": self.hello["host"], "worker_rev": self.hello["worker_rev"],
                        "gpu": (self.hello["gpu"] or {}).get("name")})
                    self.emit(done)
                except KeyJobRejected as exc:
                    self._reject(job.job_id, exc.code, exc.message)
                except Exception as exc:  # noqa: BLE001 - never die silently mid-protocol
                    sys.stderr.write(f"internal worker error: {exc!r}\n")
                    self._reject(job.job_id, "invalid", f"internal error before start: {exc}")
                finally:
                    with self._state:
                        self._thread = self._current = None
            self._thread = threading.Thread(target=run, name=f"key-{job.job_id}", daemon=True)
            self._thread.start()

    def _stop_job(self) -> None:
        with self._state:
            t = self._thread
            self._cancel.set()
        if t is not None:
            t.join()

    def serve(self, stdin) -> int:
        """Read commands until EOF/shutdown. ``stdin`` is a binary line stream."""
        self.emit({"event": "hello", "protocol": PROTOCOL, **self.hello})
        while True:
            try:
                line = stdin.readline(MAX_LINE + 1)
            except OSError as exc:  # unreadable stdin is as final as EOF: cancel and leave
                sys.stderr.write(f"stdin failed, treating as EOF: {exc}\n")
                break
            if not line:  # EOF: cancel and leave
                break
            if len(line) > MAX_LINE:
                self._reject(None, "protocol", "line too long")
                break
            try:
                msg = json.loads(line)
            except ValueError:
                self._reject(None, "protocol", "not JSON")
                continue
            if not isinstance(msg, dict) or type(msg.get("v")) is not int or msg["v"] != PROTOCOL:
                self._reject(None, "protocol", "expected an object with \"v\":1")
                continue
            cmd = msg.get("cmd")
            if cmd == "key":
                self._handle_key(msg)
            elif cmd == "cancel":
                with self._state:
                    if msg.get("job_id") == self._current:
                        self._cancel.set()
            elif cmd == "shutdown":
                break
            else:
                self._reject(msg.get("job_id") if isinstance(msg.get("job_id"), str) else None,
                             "protocol", f"unknown cmd {cmd!r}")
        self._stop_job()
        return 0


_ERROR_BROKEN_PIPE, _ERROR_NO_DATA = 109, 232  # writer closed / pipe being closed: EOF
_FILE_TYPE_PIPE = 3


def _win_pipe_peek(k32, get_last_error, handle):
    """``peek() -> bytes available`` for a Windows pipe handle; raises EOFError / OSError.

    Everything is declared (argtypes/restype) so a 64-bit HANDLE is never truncated to int."""
    import ctypes
    from ctypes import wintypes
    fn = k32.PeekNamedPipe
    fn.argtypes = [wintypes.HANDLE, wintypes.LPVOID, wintypes.DWORD, wintypes.LPDWORD,
                   wintypes.LPDWORD, wintypes.LPDWORD]
    fn.restype = wintypes.BOOL

    def peek() -> int:
        avail = wintypes.DWORD(0)
        if fn(handle, None, 0, None, ctypes.byref(avail), None):
            return avail.value
        err = get_last_error()
        if err in (_ERROR_BROKEN_PIPE, _ERROR_NO_DATA):
            raise EOFError(err)
        raise OSError(None, f"PeekNamedPipe failed (winerror {err})", None, err)
    return peek


class PolledStdin:
    """Binary stdin whose ``readline`` never leaves a synchronous ReadFile pending on a pipe.

    Windows: a blocked ReadFile on the stdin pipe handle stalls other threads' native calls
    (seen: a job thread hung forever inside a native-extension import such as scipy while
    ``serve`` waited for a command). So for a PIPE we poll PeekNamedPipe and read only bytes
    that are already there. A closed writer (ERROR_BROKEN_PIPE / ERROR_NO_DATA) is EOF; any
    other peek error raises OSError (``serve`` then stops like EOF) - there is deliberately no
    fallback to a blocking read. A FILE or CONSOLE stdin cannot hang this way and is read with
    plain blocking reads (intended: files never block, an interactive console is a person).
    ``peek``/``read``/``sleep`` are injectable for tests."""

    def __init__(self, fd=0, peek=None, read=os.read, sleep=time.sleep, poll_s=0.02):
        self._fd, self._read, self._sleep, self._poll = fd, read, sleep, poll_s
        self._buf = b""
        self._peek = peek if peek is not None else self._detect_peek(fd)

    @staticmethod
    def _detect_peek(fd):
        if sys.platform != "win32":
            return None  # pipe detection exists only for the Windows deadlock; elsewhere: block normally
        import ctypes
        import msvcrt
        from ctypes import wintypes
        k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        k32.GetFileType.argtypes, k32.GetFileType.restype = [wintypes.HANDLE], wintypes.DWORD
        handle = wintypes.HANDLE(msvcrt.get_osfhandle(fd))
        if k32.GetFileType(handle) != _FILE_TYPE_PIPE:
            return None  # file / console: plain blocking reads
        return _win_pipe_peek(k32, ctypes.get_last_error, handle)

    def _take(self, n: int) -> bytes:
        line, self._buf = self._buf[:n], self._buf[n:]
        return line

    def readline(self, limit=-1) -> bytes:
        """Like BufferedReader.readline: at most ``limit`` bytes (default MAX_LINE + 1),
        newline included; b"" only at EOF (or limit == 0)."""
        limit = MAX_LINE + 1 if limit is None or limit < 0 else limit
        while True:
            i = self._buf.find(b"\n", 0, limit)  # a newline beyond the limit does not count
            if i >= 0:
                return self._take(i + 1)
            if len(self._buf) >= limit:
                return self._take(limit)
            want = 65536
            if self._peek is not None:
                try:
                    avail = self._peek()
                except EOFError:
                    return self._take(len(self._buf))
                if not avail:
                    self._sleep(self._poll)
                    continue
                want = min(avail, want)
            chunk = self._read(self._fd, want)
            if not chunk:
                return self._take(len(self._buf))
            self._buf += chunk


def _protocol_stream():
    """A private handle on the real stdout; fd 1 itself is pointed at stderr.

    Python prints (sys.stdout) AND native code (ffmpeg children, C libraries writing to fd 1)
    then land on stderr, and only this handle can write protocol lines."""
    sys.stdout.flush()
    proto_fd = os.dup(1)
    os.dup2(2, 1)
    sys.stdout = sys.stderr
    return os.fdopen(proto_fd, "w", encoding="utf-8", newline="\n", buffering=1)


def main(argv=None, stdin=None, out=None, service=None, hello=None) -> int:
    import argparse
    ap = argparse.ArgumentParser(prog="python -m backend.worker")
    ap.add_argument("--gpu", type=int, default=None)
    args = ap.parse_args(argv)
    if out is None:
        out = _protocol_stream()
    os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
    os.environ["CUDA_DEVICE_ORDER"] = "PCI_BUS_ID"  # same index meaning as nvidia-smi, before visibility
    if args.gpu is not None:
        os.environ["CUDA_VISIBLE_DEVICES"] = str(args.gpu)
    if service is None:
        from backend.service import CorridorKeyService
        service = CorridorKeyService()
    if hasattr(out, "reconfigure"):  # Windows text mode would turn "\n" into "\r\n"
        out.reconfigure(newline="\n")
    worker = Worker(service, out, hello if hello is not None else probe_hello(args.gpu))
    return worker.serve(stdin or (PolledStdin(0) if sys.platform == "win32" else sys.stdin.buffer))
