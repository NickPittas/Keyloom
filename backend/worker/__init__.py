"""Persistent executor: strict JSON-lines protocol v1 over stdin/stdout (one job at a time).

It only executes what the app sends: after start it says ``ready`` (protocol + this device), then runs the
jobs it is given with the exact ``model_files`` they carry. It claims, probes, discovers and decides nothing.

stdout carries protocol lines only (``__main__`` points ``sys.stdout`` at stderr first); logs go
to stderr, which the launching app must drain. stdin EOF means cancel and exit. This package
imports only the stdlib until a job arrives, so ``python -m backend.worker`` can capture stdout
before any heavy import prints.
"""
from __future__ import annotations

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


def probe_ready(gpu_arg: int | None) -> dict:
    """Facts about THIS process and device for the ready acknowledgment. No model or capability discovery."""
    gpu = None
    try:
        import torch
        if torch.cuda.is_available():
            idx = torch.cuda.current_device()  # CUDA_VISIBLE_DEVICES already remapped --gpu to 0
            prop = torch.cuda.get_device_properties(idx)
            gpu = {"index": gpu_arg if gpu_arg is not None else idx, "name": prop.name,
                   "vram_gb": round(prop.total_memory / 2**30, 1)}
            try:  # identity of the device actually opened (not the NVML index); local use never depends on it
                from backend.worker.gpu_identity import device_identity
                gpu.update(device_identity(prop))
            except Exception as exc:  # noqa: BLE001
                gpu.update(uuid=None, uuid_status="unverified", uuid_reason=f"identity probe failed: {type(exc).__name__}")
    except Exception as exc:  # noqa: BLE001 - no usable GPU is a valid answer
        sys.stderr.write(f"GPU probe failed: {exc}\n")
    if gpu is None:
        sys.stderr.write("no usable CUDA GPU on this worker\n")
    return {"worker_rev": _worker_rev(), "engine": "corridorkey-v2", "host": socket.gethostname(),
            "platform": sys.platform, "gpu": gpu}


# Task kinds that load a model: the app must send their exact files (the others need none).
MODEL_KINDS = ("gvm", "birefnet", "sam_preview", "sam_track", "matanyone2", "videomama")


class Worker:
    def __init__(self, service, out, ready: dict):
        self.service, self.out, self.ready = service, out, ready
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

    def _reject(self, job_id, code: str, message: str, **extra) -> None:
        self.emit({"event": "rejected", "job_id": job_id, "code": code, "message": message, **extra})

    # -- commands
    def _handle_key(self, msg: dict) -> None:
        from backend.service.key_job import KeyJob, KeyJobRejected, run_key_job
        raw = msg.get("job")
        jid = raw.get("job_id") if isinstance(raw, dict) and isinstance(raw.get("job_id"), str) else None
        try:
            job = KeyJob.from_dict(raw)
        except KeyJobRejected as exc:
            return self._reject(jid, exc.code, exc.message)
        if job.model_files is None:
            return self._reject(jid, "invalid", "no model_files: the app sends the exact model files with every Key job")
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
                        "host": self.ready["host"], "worker_rev": self.ready["worker_rev"],
                        "gpu": (self.ready["gpu"] or {}).get("name")})
                    self._release_gpu()
                    self.emit(done)
                except KeyJobRejected as exc:
                    self._release_gpu()
                    self._reject(job.job_id, exc.code, exc.message)
                except Exception as exc:  # noqa: BLE001 - never die silently mid-protocol
                    sys.stderr.write(f"internal worker error: {exc!r}\n")
                    self._release_gpu()
                    self._reject(job.job_id, "invalid", f"internal error before start: {exc}")
                finally:
                    with self._state:
                        self._thread = self._current = None
            self._thread = threading.Thread(target=run, name=f"key-{job.job_id}", daemon=True)
            self._thread.start()

    def _release_gpu(self) -> None:
        """Every job starts on an empty GPU: drop all models and PyTorch's cached memory before the result is
        reported (so the next job, sent on that result, never finds this worker still busy unloading)."""
        try:
            self.service.unload_engines()
        except Exception as exc:  # noqa: BLE001 - a failed unload must not hide the job's own result
            sys.stderr.write(f"releasing GPU memory failed: {exc!r}\n")

    def _handle_task(self, msg: dict) -> None:
        from backend.service.key_job import KeyJobRejected
        from backend.service.task_job import TaskJob, run_task_job
        raw = msg.get("job")
        jid = raw.get("job_id") if isinstance(raw, dict) and isinstance(raw.get("job_id"), str) else None
        try:
            job = TaskJob.from_dict(raw)
        except KeyJobRejected as exc:
            return self._reject(jid, exc.code, exc.message, **getattr(exc, "extra", {}))
        if job.kind in MODEL_KINDS and job.model_files is None:
            return self._reject(jid, "invalid", f"no model_files: the app sends the exact model files with every '{job.kind}' job")
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
                    done = run_task_job(self.service, job, emit=emit, cancel=cancel)
                    self._release_gpu()
                    self.emit(done)
                except KeyJobRejected as exc:
                    self._release_gpu()
                    self._reject(job.job_id, exc.code, exc.message, **getattr(exc, "extra", {}))
                except Exception as exc:  # noqa: BLE001 - never die silently mid-protocol
                    sys.stderr.write(f"internal worker error: {exc!r}\n")
                    self._release_gpu()
                    self._reject(job.job_id, "invalid", f"internal error before start: {exc}")
                finally:
                    with self._state:
                        self._thread = self._current = None
            self._thread = threading.Thread(target=run, name=f"task-{job.job_id}", daemon=True)
            self._thread.start()

    def _stop_job(self) -> None:
        with self._state:
            t = self._thread
            self._cancel.set()
        if t is not None:
            t.join()

    def serve(self, stdin) -> int:
        """Read commands until EOF/shutdown. ``stdin`` is a binary line stream."""
        self.emit({"event": "ready", "protocol": PROTOCOL, **self.ready})
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
            elif cmd == "task":
                self._handle_task(msg)
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


def main(argv=None, stdin=None, out=None, service=None, ready=None) -> int:
    import argparse
    ap = argparse.ArgumentParser(prog="python -m backend.worker")
    ap.add_argument("--gpu", type=int, default=None)
    args = ap.parse_args(argv)
    if out is None:
        out = _protocol_stream()
    os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
    os.environ["CUDA_DEVICE_ORDER"] = "PCI_BUS_ID"  # same index meaning as nvidia-smi, before visibility
    if sys.platform.startswith("linux"):  # PyTorch's own advice for reserved-but-unused memory; Linux-only option
        os.environ.setdefault("PYTORCH_ALLOC_CONF", "expandable_segments:True")
    if args.gpu is not None:
        os.environ["CUDA_VISIBLE_DEVICES"] = str(args.gpu)
    if service is None:
        from backend.service import CorridorKeyService
        service = CorridorKeyService()
        service.detect_device()  # after CUDA_VISIBLE_DEVICES; the default device is 'cpu' until detected
    if hasattr(out, "reconfigure"):  # Windows text mode would turn "\n" into "\r\n"
        out.reconfigure(newline="\n")
    worker = Worker(service, out, ready if ready is not None else probe_ready(args.gpu))
    return worker.serve(stdin or (PolledStdin(0) if sys.platform == "win32" else sys.stdin.buffer))
