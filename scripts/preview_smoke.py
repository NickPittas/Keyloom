"""Preview playback/scrub benchmark (Task 20). Read-only: no inference, no writes.

Example:
    QT_QPA_PLATFORM=offscreen .venv/bin/python scripts/preview_smoke.py \
        --clip-root <clip dir> --mode FG --viewport 1280x720 --seconds 10

Phases: cold scrub (20 random frames), cold playback, warm playback,
warm scrub (20 random frames). A 20 ms GUI heartbeat runs throughout.
"""
from __future__ import annotations

import os

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"

import argparse
import itertools
import json
import random
import resource
import statistics
import sys
import time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, REPO)

import cv2  # noqa: F401  (after the env var)
from PySide6.QtCore import QTimer
from PySide6.QtWidgets import QApplication

from backend.clip_state import ClipEntry
from ui.preview import async_decoder
from ui.preview.frame_index import ViewMode
from ui.widgets.dual_viewer import DualViewerPanel

HEARTBEAT_MS = 20
MiB = 1024 * 1024


def _proc_status(key: str) -> int:
    with open("/proc/self/status") as fh:
        for line in fh:
            if line.startswith(key + ":"):
                return int(line.split()[1]) * 1024
    return 0


def _pct(vals, p):
    if not vals:
        return 0.0
    s = sorted(vals)
    return s[min(len(s) - 1, round(p / 100 * (len(s) - 1)))]


def _stats(vals):
    return {"n": len(vals), "p50": round(_pct(vals, 50), 1),
            "p95": round(_pct(vals, 95), 1), "max": round(max(vals, default=0.0), 1)}


class Bench:
    def __init__(self, app, panel, args):
        self.app, self.panel, self.args = app, panel, args
        self.sc = panel._scrubber
        self.out, self.inp = panel._output_viewer, panel._input_viewer
        self.beats: list[float] = []
        self.rss_peak = 0
        self._last_beat = time.perf_counter()
        self.hb = QTimer()
        self.hb.setInterval(HEARTBEAT_MS)
        self.hb.timeout.connect(self._beat)
        self.hb.start()
        self.rss = QTimer()
        self.rss.setInterval(200)
        self.rss.timeout.connect(self._sample_rss)
        self.rss.start()
        self.presented: dict[str, int] = {}
        self.shown: list[tuple[float, int]] = []
        self.inp.frame_presented.connect(lambda i: self.presented.__setitem__("in", i))
        self.out.frame_presented.connect(lambda i: self.presented.__setitem__("out", i))
        panel.frame_changed.connect(lambda i: self.shown.append((time.perf_counter(), i)))
        self.cache_peak = {"in": 0, "out": 0}
        self.pool_peak = 0

    def _beat(self):
        now = time.perf_counter()
        self.beats.append((now - self._last_beat) * 1000 - HEARTBEAT_MS)
        self._last_beat = now

    def _sample_rss(self):
        self.rss_peak = max(self.rss_peak, _proc_status("VmRSS"))
        self.cache_peak["in"] = max(self.cache_peak["in"], self.inp._frame_cache.bytes_used)
        self.cache_peak["out"] = max(self.cache_peak["out"], self.out._frame_cache.bytes_used)
        self.pool_peak = max(self.pool_peak, async_decoder.preview_pool().activeThreadCount())

    def pump(self, seconds=None, until=None, timeout=30.0):
        end = time.perf_counter() + (seconds if seconds is not None else timeout)
        while time.perf_counter() < end:
            self.app.processEvents()
            if until is not None and until():
                return True
            time.sleep(0.001)
        return until() if until else True

    def seek(self, idx, timeout=30.0):
        """Scrubber path; returns ms until both panes presented ``idx`` (None=timeout)."""
        cur = self.sc.current_frame()
        if cur == idx:
            idx = (idx + 1) % self.sc._total
        self.presented.clear()
        t0 = time.perf_counter()
        self.sc._slider.setValue(idx)
        ok = self.pump(until=lambda: self.presented.get("in") == idx
                       and self.presented.get("out") == idx, timeout=timeout)
        return (time.perf_counter() - t0) * 1000 if ok else None

    def scrub(self, rng, n=20):
        total = self.sc._total
        beat0 = len(self.beats)
        times, timeouts = [], 0
        for i in rng.sample(range(total), min(n, total)):
            ms = self.seek(i)
            if ms is None:
                timeouts += 1
            else:
                times.append(ms)
            self.pump(0.15)  # human-ish pause between jumps
        res = _stats(times)
        res["timeouts"] = timeouts
        res["heartbeat_lateness_ms"] = _stats(self.beats[beat0:])
        return res

    def play(self, seconds):
        fps = self.sc._fps
        span = self.sc._play_range()
        span_n = span[1] - span[0] + 1
        self.shown.clear()
        beat0 = len(self.beats)
        self.pump(0.2)
        t0 = time.perf_counter()
        self.sc.toggle_playback()
        self.pump(seconds)
        t1 = time.perf_counter()
        self.sc.toggle_playback()
        self.pump(0.2)
        el = t1 - t0
        evs = [e for e in self.shown if t0 <= e[0] <= t1]
        displayed = len(evs)
        expected = el * fps
        wraps = [evs[k][0] for k in range(1, len(evs)) if evs[k][1] < evs[k - 1][1]]
        loops = [b - a for a, b in itertools.pairwise([t0, *wraps])]  # playback starts at range start
        stalls = [(evs[k][0] - evs[k - 1][0]) * 1000 for k in range(1, len(evs))]
        return {
            "seconds": round(el, 2), "clip_fps": fps,
            "displayed": displayed,
            "displayed_fps": round(displayed / el, 2),
            "expected_frames": round(expected, 1),
            "drop_pct": round(max(0.0, 1 - displayed / expected) * 100.0, 1) if expected else 0.0,
            "loop_expected_s": round(span_n / fps, 3),
            "loop_wraps": len(wraps),
            "loop_measured_s": round(statistics.mean(loops), 3) if loops else None,
            "max_present_gap_ms": round(max(stalls, default=0.0), 1),
            "heartbeat_lateness_ms": _stats(self.beats[beat0:]),
        }


def _parse_viewport(text):
    w, h = text.lower().split("x")
    return int(w), int(h)


def _resolve_mode(text):
    for m in ViewMode:
        if text.lower() in (m.name.lower(), m.value.lower()):
            return m
    raise SystemExit(f"unknown mode {text!r}; choose from {[m.value for m in ViewMode]}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--clip-root", required=True)
    ap.add_argument("--mode", default="Comp")
    ap.add_argument("--viewport", default="1280x720", help="per-pane canvas WxH")
    ap.add_argument("--seconds", type=float, default=10.0)
    ap.add_argument("--split", action="store_true", help="enable split lane on output pane")
    ap.add_argument("--seed", type=int, default=1234)
    args = ap.parse_args()
    mode = _resolve_mode(args.mode)
    vw, vh = _parse_viewport(args.viewport)
    root = os.path.abspath(args.clip_root)

    app = QApplication.instance() or QApplication([])
    rss_start = _proc_status("VmRSS")
    clip = ClipEntry(name=os.path.basename(root), root_path=root)
    clip.find_assets()

    panel = DualViewerPanel()
    panel.resize(2 * vw + 40, vh + 160)
    panel.show()
    app.processEvents()
    panel.set_clip(clip)
    panel.output_viewer.set_view_mode(mode.value)
    if args.split:
        panel.output_viewer.set_split_mode(True)
    for _ in range(5):  # converge each pane's canvas on the requested size
        panel._viewer_splitter.setSizes([1000, 1000])
        app.processEvents()
        cw, ch = panel.output_viewer._canvas_size()
        if abs(cw - vw) <= 2 and abs(ch - vh) <= 2:
            break
        panel.resize(max(400, panel.width() + 2 * (vw - cw)), max(300, panel.height() + (vh - ch)))
        app.processEvents()
        time.sleep(0.2)
        app.processEvents()
    app.processEvents()

    b = Bench(app, panel, args)
    b.pump(until=lambda: panel._scrubber._fps != 24.0, timeout=1.0)
    total = b.sc._total
    if total < 2:
        raise SystemExit(f"clip has {total} frames; nothing to benchmark")
    actual_mode = panel.current_output_mode
    if actual_mode != mode:
        print(f"WARNING: output mode is {actual_mode.value}, requested {mode.value} "
              "(frames missing for that mode?)", file=sys.stderr)
    b.pump(0.5)
    rss_loaded = _proc_status("VmRSS")
    rng = random.Random(args.seed)

    report = {
        "clip_root": root, "frames": total, "mode": actual_mode.value, "split": args.split,
        "canvas_px": {"input": list(panel.input_viewer._canvas_size()),
                      "output": list(panel.output_viewer._canvas_size())},
        "pool_threads": async_decoder.preview_pool().maxThreadCount(),
    }
    report["cold_scrub"] = b.scrub(rng)
    for v in (panel.input_viewer, panel.output_viewer):
        v.invalidate_cache()
    b.seek(0)
    b.pump(until=lambda: async_decoder.preview_pool().activeThreadCount() == 0, timeout=10)
    report["cold_play"] = b.play(args.seconds)
    # let prefetch finish, then replay from the start with a warm cache
    b.pump(2.0)
    b.seek(0)
    b.pump(2.0)
    report["warm_play"] = b.play(args.seconds)
    report["warm_scrub"] = b.scrub(rng)

    async_decoder.preview_pool().waitForDone(10000)
    app.processEvents()
    b._sample_rss()
    report["cache_mib"] = {
        "input_now": round(panel.input_viewer._frame_cache.bytes_used / MiB, 1),
        "output_now": round(panel.output_viewer._frame_cache.bytes_used / MiB, 1),
        "input_peak": round(b.cache_peak["in"] / MiB, 1),
        "output_peak": round(b.cache_peak["out"] / MiB, 1),
        "budget_each": round(panel.output_viewer._frame_cache.max_bytes / MiB, 1),
    }
    report["pool_active_peak"] = b.pool_peak
    report["heartbeat_lateness_ms_all"] = _stats(b.beats)
    report["rss_mib"] = {
        "start": round(rss_start / MiB), "after_load": round(rss_loaded / MiB),
        "peak_sampled": round(b.rss_peak / MiB),
        "vm_hwm": round(_proc_status("VmHWM") / MiB),
        "ru_maxrss": round(resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / 1024),
        "growth_vs_loaded": round((max(b.rss_peak, _proc_status("VmHWM")) - rss_loaded) / MiB),
    }
    print(json.dumps(report, indent=2))

    b.hb.stop()
    b.rss.stop()
    panel.input_viewer.shutdown()
    panel.output_viewer.shutdown()
    async_decoder.preview_pool().waitForDone(10000)
    app.processEvents()
    panel.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
