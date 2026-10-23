"""Latest-only system metrics sampler (run as ``python -I backend/system_metrics.py``, not ``-m``: importing the
``backend`` package pulls in cv2/torch, which this process must never load) for the desktop UI (and, later, the client stats report).

One long-lived process: prints one JSON line per sample on stdout until stdout closes or it is killed.
Counters are read from the OS (/proc on Linux, kernel32 on Windows) and from NVML for every physical GPU.
No torch import, no CUDA context, no model load, no history, no third-party package besides optional
nvidia-ml-py. A counter that cannot be read is null (never 0) with a short reason under ``errors``; one failing
counter never stops the others. Field names and units follow backend/coordinator/README.md (bytes, GPU uuid);
``v``/``type``/``errors`` are a local envelope the UI strips before any network use. ``index`` is the NVML
(PCI bus order) index, identical to the worker's ``--gpu`` (it sets CUDA_DEVICE_ORDER=PCI_BUS_ID), but the
device identity is the uuid.
"""
from __future__ import annotations

import argparse
import json
import sys
import time

VERSION = 1
MAX_GPUS = 64
MAX_TEXT = 120
NVML_RETRY_TICKS = 20


def _short(text: object) -> str:
    return " ".join(str(text).split())[:MAX_TEXT]


# ---- CPU (system-wide, not this process) ---------------------------------------------------------------


def parse_proc_stat(text: str) -> tuple[int, int] | None:
    """(idle, total) jiffies from the aggregate ``cpu`` line of /proc/stat."""
    for line in text.splitlines():
        parts = line.split()
        if parts and parts[0] == "cpu" and len(parts) >= 5:
            try:
                vals = [int(v) for v in parts[1:9]]
            except ValueError:
                return None
            vals += [0] * (8 - len(vals))
            idle = vals[3] + vals[4]  # idle + iowait
            return idle, sum(vals)
    return None


def cpu_percent(prev: tuple[int, int] | None, cur: tuple[int, int] | None) -> float | None:
    """Busy share between two (idle, total) readings; None while warming or when the counters did not advance."""
    if prev is None or cur is None:
        return None
    d_total = cur[1] - prev[1]
    d_idle = cur[0] - prev[0]
    if d_total <= 0 or d_idle < 0 or d_idle > d_total:
        return None
    return round(100.0 * (d_total - d_idle) / d_total, 1)


def parse_meminfo(text: str) -> tuple[int, int] | None:
    """(used_bytes, total_bytes) = MemTotal - MemAvailable from /proc/meminfo."""
    kib: dict[str, int] = {}
    for line in text.splitlines():
        name, _, rest = line.partition(":")
        if name in ("MemTotal", "MemAvailable"):
            try:
                kib[name] = int(rest.split()[0])
            except (ValueError, IndexError):
                return None
    if "MemTotal" not in kib or "MemAvailable" not in kib or kib["MemAvailable"] > kib["MemTotal"]:
        return None
    return (kib["MemTotal"] - kib["MemAvailable"]) * 1024, kib["MemTotal"] * 1024


class _Os:
    """Reads the platform counters; every method raises on failure (the caller records the reason)."""

    def __init__(self) -> None:
        self.windows = sys.platform == "win32"
        self.linux = sys.platform.startswith("linux")
        if self.windows:
            import ctypes
            from ctypes import wintypes

            class FILETIME(ctypes.Structure):
                _fields_ = [("lo", wintypes.DWORD), ("hi", wintypes.DWORD)]

            class MEMORYSTATUSEX(ctypes.Structure):
                _fields_ = [
                    ("dwLength", wintypes.DWORD),
                    ("dwMemoryLoad", wintypes.DWORD),
                    ("ullTotalPhys", ctypes.c_ulonglong),
                    ("ullAvailPhys", ctypes.c_ulonglong),
                    ("ullTotalPageFile", ctypes.c_ulonglong),
                    ("ullAvailPageFile", ctypes.c_ulonglong),
                    ("ullTotalVirtual", ctypes.c_ulonglong),
                    ("ullAvailVirtual", ctypes.c_ulonglong),
                    ("sullAvailExtendedVirtual", ctypes.c_ulonglong),
                ]

            self._ct, self._FT, self._MS = ctypes, FILETIME, MEMORYSTATUSEX
            self._k32 = ctypes.WinDLL("kernel32", use_last_error=True)

    def cpu_counters(self) -> tuple[int, int]:
        if self.linux:
            with open("/proc/stat", encoding="ascii", errors="replace") as f:
                got = parse_proc_stat(f.read(4096))
            if got is None:
                raise RuntimeError("unreadable /proc/stat")
            return got
        if self.windows:
            idle, kernel, user = self._FT(), self._FT(), self._FT()
            if not self._k32.GetSystemTimes(self._ct.byref(idle), self._ct.byref(kernel), self._ct.byref(user)):
                raise OSError("GetSystemTimes failed")
            ft = lambda t: (t.hi << 32) | t.lo  # noqa: E731
            # kernel time includes idle time
            return ft(idle), ft(kernel) + ft(user)
        raise RuntimeError(f"CPU usage is not supported on {sys.platform}")

    def memory(self) -> tuple[int, int]:
        if self.linux:
            with open("/proc/meminfo", encoding="ascii", errors="replace") as f:
                got = parse_meminfo(f.read(8192))
            if got is None:
                raise RuntimeError("unreadable /proc/meminfo")
            return got
        if self.windows:
            st = self._MS()
            st.dwLength = self._ct.sizeof(self._MS)
            if not self._k32.GlobalMemoryStatusEx(self._ct.byref(st)):
                raise OSError("GlobalMemoryStatusEx failed")
            return int(st.ullTotalPhys - st.ullAvailPhys), int(st.ullTotalPhys)
        raise RuntimeError(f"RAM usage is not supported on {sys.platform}")


# ---- GPUs (NVML, all physical devices) -----------------------------------------------------------------


def _text(v: object) -> str:
    return v.decode("utf-8", "replace") if isinstance(v, bytes) else str(v)


class _Gpus:
    def __init__(self, nvml=None) -> None:
        self._nvml = nvml
        self._up = False
        self._wait = 0

    def _init(self) -> None:
        if self._nvml is None:
            import pynvml  # nvidia-ml-py; imports the NVML client library, never CUDA/torch

            self._nvml = pynvml
        self._nvml.nvmlInit()
        self._up = True

    def read(self) -> tuple[list[dict] | None, str | None]:
        """(devices, error). devices None = NVML unusable; a device whose identity cannot be read is skipped."""
        if not self._up:
            if self._wait > 0:
                self._wait -= 1
                return None, self._why
            try:
                self._init()
            except Exception as e:  # missing package, driver or library
                self._wait = NVML_RETRY_TICKS
                self._why = _short(f"NVML unavailable: {type(e).__name__}: {e}")
                return None, self._why
        n = self._nvml
        try:
            count = min(int(n.nvmlDeviceGetCount()), MAX_GPUS)
        except Exception as e:
            self._up = False
            return None, _short(f"NVML device count failed: {e}")
        out: list[dict] = []
        problems: list[str] = []
        for i in range(count):
            try:
                h = n.nvmlDeviceGetHandleByIndex(i)
                uuid = _text(n.nvmlDeviceGetUUID(h))
            except Exception as e:
                problems.append(f"GPU {i}: {type(e).__name__}")
                continue
            g: dict = {"uuid": uuid, "index": i, "name": None, "utilization_percent": None,
                       "vram_used_bytes": None, "vram_total_bytes": None}
            for key, get in (
                ("name", lambda: _short(_text(n.nvmlDeviceGetName(h)))),
                ("utilization_percent", lambda: float(n.nvmlDeviceGetUtilizationRates(h).gpu)),
            ):
                try:
                    g[key] = get()
                except Exception as e:
                    problems.append(f"GPU {i} {key}: {type(e).__name__}")
            try:
                m = n.nvmlDeviceGetMemoryInfo(h)
                g["vram_used_bytes"], g["vram_total_bytes"] = int(m.used), int(m.total)
            except Exception as e:
                problems.append(f"GPU {i} memory: {type(e).__name__}")
            out.append(g)
        return out, (_short("; ".join(problems)) if problems else None)

    def close(self) -> None:
        if self._up:
            try:
                self._nvml.nvmlShutdown()
            except Exception:
                pass
            self._up = False


class Sampler:
    """Holds the previous CPU reading; `sample()` is one tick. Inject `os_` / `gpus` in tests."""

    def __init__(self, os_: _Os | None = None, gpus: _Gpus | None = None) -> None:
        self.os = os_ or _Os()
        self.gpus = gpus or _Gpus()
        self.seq = 0
        self._cpu_prev: tuple[int, int] | None = None

    def sample(self) -> dict:
        self.seq += 1
        errors: dict[str, str] = {}
        cpu = ram_used = ram_total = None
        try:
            cur = self.os.cpu_counters()
            cpu = cpu_percent(self._cpu_prev, cur)
            if self._cpu_prev is None:
                errors["cpu"] = "warming up"
            elif cpu is None:
                errors["cpu"] = "counters did not advance"
            self._cpu_prev = cur
        except Exception as e:
            self._cpu_prev = None
            errors["cpu"] = _short(e)
        try:
            ram_used, ram_total = self.os.memory()
        except Exception as e:
            errors["ram"] = _short(e)
        try:
            gpus, why = self.gpus.read()
        except Exception as e:  # defensive: the GPU path must never end the process
            gpus, why = None, _short(f"GPU read failed: {e}")
        if why:
            errors["gpu"] = why
        return {
            "v": VERSION, "type": "system_metrics", "seq": self.seq,
            "cpu_percent": cpu, "ram_used_bytes": ram_used, "ram_total_bytes": ram_total,
            "gpus": gpus, "errors": errors,
        }

    def close(self) -> None:
        self.gpus.close()


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--interval", type=float, default=1.5, help="seconds between samples (0.5-10)")
    ap.add_argument("--count", type=int, default=0, help="stop after N samples (0 = run until stdout closes)")
    args = ap.parse_args(argv)
    interval = min(10.0, max(0.5, args.interval))
    s = Sampler()
    try:
        n = 0
        while True:
            started = time.monotonic()
            try:
                sys.stdout.write(json.dumps(s.sample(), separators=(",", ":")) + "\n")
                sys.stdout.flush()
            except (BrokenPipeError, OSError):
                return 0  # the UI went away
            n += 1
            if args.count and n >= args.count:
                return 0
            time.sleep(max(0.0, interval - (time.monotonic() - started)))
    except KeyboardInterrupt:
        return 0
    finally:
        s.close()


if __name__ == "__main__":
    raise SystemExit(main())
