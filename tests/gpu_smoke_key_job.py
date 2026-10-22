"""Manual GPU smoke (NOT collected by pytest): real worker + real model on a synthetic fixture.

Run only through rust-ui/tools/gpu-isolated-test.sh:
    cd rust-ui && bash tools/gpu-isolated-test.sh ../.venv/bin/python -B ../tests/gpu_smoke_key_job.py
Everything is written below /tmp (tmpfs inside the wrapper)."""
import os
os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
import json
import subprocess
import sys
import threading
import time
from pathlib import Path

import cv2
import numpy as np

REPO = Path(__file__).resolve().parent.parent
TMP = Path("/tmp/ck-smoke")
N, A, B, SIZE = 12, 3, 9, 256
STEMS = [f"shot_{1001 + i}" for i in range(N)]


def fixture():
    root = TMP / "proj" / "clips" / "clip1"
    frames, hint = root / "Frames", root / "AlphaHint"
    frames.mkdir(parents=True)
    hint.mkdir()
    yy, xx = np.mgrid[0:SIZE, 0:SIZE]
    for i, s in enumerate(STEMS):
        if not A <= i <= B:  # must never be read: undecodable
            (frames / f"{s}.png").write_bytes(b"POISON")
            (hint / f"{s}.png").write_bytes(b"POISON")
            continue
        cx = 90 + 8 * i
        disc = ((xx - cx) ** 2 + (yy - 128) ** 2) < 50 ** 2
        img = np.zeros((SIZE, SIZE, 3), np.uint8)
        img[...] = (40, 200, 40)          # green screen (BGR)
        img[disc] = (60, 90, 200)         # subject
        cv2.imwrite(str(frames / f"{s}.png"), img)
        cv2.imwrite(str(hint / f"{s}.png"), (disc * 255).astype(np.uint8))
    return root


def job(root, jid, out, mode="new", hint="none", comp="piz"):
    outs = {o: [str(out / o.capitalize() / f"{s}.exr") for s in STEMS[A:B + 1]] for o in ("fg", "matte")}
    cfg = dict(fg_enabled=True, fg_format="exr", matte_enabled=True, matte_format="exr",
               comp_enabled=False, comp_format="png", processed_enabled=False,
               processed_format="exr", exr_compression=comp)
    params = dict(input_is_linear=False, use_alpha_hint=hint != "none", hint_quality=0.95,
                  carry_hint=True, carry_hint_quality=0.95, vram_profile="auto",
                  despill_strength=0.5, cutout_linear=False, hint_mode=hint, temporal_window="auto")
    return {"job_id": jid, "clip_id": "smoke/clip1", "clip_root": str(root),
            "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]},
            "params": params, "output_config": cfg, "hint_keyframes_abs": None, "outputs": outs,
            "manifest_dir": str(out), "mode": mode, "publish": "final", "path_map": [],
            "require": {"protocol": 1, "models": {"corridorkey_v2": None}}}


class Worker:
    def __init__(self):
        self.t0 = time.time()
        self.p = subprocess.Popen([sys.executable, "-B", "-m", "backend.worker", "--gpu", "0"],
                                  cwd=REPO, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
        self.err = []
        threading.Thread(target=lambda: self.err.extend(self.p.stderr), daemon=True).start()
        self.events = []

    def read(self):
        line = self.p.stdout.readline()
        if not line:
            raise SystemExit("worker closed stdout; stderr tail:\n" + "".join(self.err[-30:]))
        e = json.loads(line)
        e["_t"] = round(time.time() - self.t0, 2)
        self.events.append(e)
        return e

    def send(self, obj):
        self.p.stdin.write(json.dumps({"v": 1, **obj}) + "\n")
        self.p.stdin.flush()

    def until(self, pred, on_event=None):
        while True:
            e = self.read()
            if on_event:
                on_event(e)
            if pred(e):
                return e

    def run_job(self, j, cancel_at_publish=None):
        t = time.time()
        self.send({"cmd": "key", "job": j})

        def hook(e):
            if (cancel_at_publish and e["event"] == "progress" and e.get("phase") == "publish"
                    and e["done"] >= cancel_at_publish):
                self.send({"cmd": "cancel", "job_id": j["job_id"]})
        e = self.until(lambda e: e["event"] in ("done", "rejected"), hook)
        e["_wall"] = round(time.time() - t, 2)
        return e


def exr_ok(path):
    img = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    return img is not None and np.isfinite(img).all() and img.shape[:2] == (SIZE, SIZE)


def check_outputs(out, expect_n):
    res = {}
    for o in ("Fg", "Matte"):
        files = sorted((out / o).glob("*.exr")) if (out / o).is_dir() else []
        res[o] = len(files)
        assert all(exr_ok(f) for f in files), f"unreadable exr in {o}"
        assert {f.stem for f in files} <= set(STEMS[A:B + 1]), "output outside range!"
    assert res == {"Fg": expect_n, "Matte": expect_n}, res
    return res


def main():
    assert str(TMP).startswith("/tmp")
    root = fixture()
    report = {}
    w = Worker()
    hello = w.read()
    assert hello["event"] == "hello", hello
    report["hello_s"] = hello["_t"]
    report["hello"] = {k: hello[k] for k in ("host", "platform", "gpu", "capabilities", "worker_rev")}
    report["hello"]["model"] = hello["models"]
    assert hello["capabilities"]["key"], "key not available: " + "".join(w.err[-10:])

    # 1. fresh job, no hint
    out1 = TMP / "out" / "v001"
    d1 = w.run_job(job(root, "smoke-1", out1))
    report["job1"] = {k: d1.get(k) for k in ("outcome", "message", "error_kind", "published", "skipped",
                                              "expected", "manifest_complete", "_wall")}
    assert d1["event"] == "done" and d1["outcome"] == "complete", (d1, "".join(w.err[-30:]))
    report["job1_outputs"] = check_outputs(out1, B - A + 1)
    m = json.loads((out1 / ".corridorkey_manifest.json").read_text())
    assert m["complete"] and m["frame_range"] == [A, B] and m["source_stems"] == STEMS[A:B + 1]
    report["job1_profile"] = m["effective_profile"]
    report["job1_events"] = [(e["event"], e.get("phase"), e["_t"]) for e in w.events
                             if e["event"] in ("accepted", "profile", "status") ][:8]
    assert not (out1 / ".corridorkey_job.lock").exists()

    # 2. resume of a complete result: no-op, quick
    d2 = w.run_job(job(root, "smoke-2", out1, mode="resume"))
    report["job2_resume_complete"] = {k: d2.get(k) for k in ("outcome", "published", "skipped", "_wall")}
    assert d2["outcome"] == "complete" and d2["published"] == 0

    # 3. dense hint job (image-sequence AlphaHint, range only)
    out3 = TMP / "out" / "v003"
    d3 = w.run_job(job(root, "smoke-3", out3, hint="full", comp="none"))
    report["job3_hint"] = {k: d3.get(k) for k in ("outcome", "message", "published", "_wall")}
    assert d3["outcome"] == "complete", (d3, "".join(w.err[-30:]))
    check_outputs(out3, B - A + 1)

    # 4. cancel mid-publish, then resume to completion
    out4 = TMP / "out" / "v004"
    d4 = w.run_job(job(root, "smoke-4", out4), cancel_at_publish=2)
    report["job4_cancel"] = {k: d4.get(k) for k in ("outcome", "published", "manifest_complete", "_wall")}
    assert d4["outcome"] == "cancelled", d4
    partial = sum(1 for _ in out4.rglob("*.exr"))
    report["job4_partial_files"] = partial
    assert 0 < partial < 2 * (B - A + 1)
    d5 = w.run_job(job(root, "smoke-5", out4, mode="resume"))
    report["job5_resume"] = {k: d5.get(k) for k in ("outcome", "published", "skipped", "manifest_complete", "_wall")}
    assert d5["outcome"] == "complete", (d5, "".join(w.err[-30:]))
    check_outputs(out4, B - A + 1)

    # 5. no-clobber: same folder, mode=new must be refused untouched
    before = sorted(str(p) for p in out1.rglob("*"))
    d6 = w.run_job(job(root, "smoke-6", out1))
    assert d6["event"] == "rejected" and d6["code"] == "exists", d6
    assert before == sorted(str(p) for p in out1.rglob("*"))

    w.send({"cmd": "shutdown"})
    w.p.wait(30)
    report["exit_code"] = w.p.returncode
    report["stderr_tail"] = "".join(w.err[-8:])[-1500:]
    # leftover poison untouched
    assert (root / "Frames" / f"{STEMS[0]}.png").read_bytes() == b"POISON"
    print(json.dumps(report, indent=2, default=str))


if __name__ == "__main__":
    main()
