"""Real-GPU smoke test for the CorridorKey 2.0 shared service (no second inference impl)."""
import argparse
import json
import os
import shutil
import sys
import time
from pathlib import Path

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import cv2
import numpy as np
import torch

from backend.clip_state import ClipEntry, ClipState
from backend.errors import JobCancelledError
from backend.job_queue import GPUJob, JobType
from backend.service import CorridorKeyService, InferenceParams, OutputConfig

DEFAULT_WORK = os.path.expanduser("~/.local/share/EZ-CorridorKey2/smoke/work")
CASES = ["nohint_auto", "nohint_low", "png_outputs", "range", "cancel", "hint_auto"]
PREVIEWS = {"nohint_auto": "/tmp/ezck2-smoke-nohint.jpg", "hint_auto": "/tmp/ezck2-smoke-hint.jpg"}
EXTS = {".exr", ".png"}


def build_clip(name, work, src, hint_src, n, with_hint, hw=None):
    root = Path(work) / name
    shutil.rmtree(root, ignore_errors=True)
    (root / "Frames").mkdir(parents=True)
    files = sorted(p for p in Path(src).iterdir() if p.suffix.lower() in EXTS)[:n]
    for f in files:  # symlinks: source is read-only, never written
        (root / "Frames" / f.name).symlink_to(f.resolve())
    if with_hint:
        (root / "AlphaHint").mkdir()
        for f in files:
            h = Path(hint_src) / (f.stem + ".png")
            if not h.exists():
                raise FileNotFoundError(h)
            m = read_img(h)  # resize to clip resolution (source hints may differ)
            if m.ndim == 3:
                m = m[..., 0]
            if hw and m.shape[:2] != hw:
                m = cv2.resize(m, (hw[1], hw[0]), interpolation=cv2.INTER_LINEAR)
            cv2.imwrite(str(root / "AlphaHint" / h.name), m)
    clip = ClipEntry(name=name, root_path=str(root))
    clip.find_assets()
    return clip


def read_img(path):
    img = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if img is None:
        raise RuntimeError(f"unreadable output {path}")
    return img


def check_outputs(clip, n, expect_hw):
    out = Path(clip.output_dir)
    errs = []
    for sub in ("FG", "Matte", "Comp", "Processed"):
        files = sorted((out / sub).glob("*")) if (out / sub).is_dir() else []
        if len(files) != n:
            errs.append(f"{sub}: {len(files)} files != {n}")
            continue
        img = read_img(files[0])
        info = f"{sub} {files[0].name} shape={img.shape} dtype={img.dtype} min={img.min():.3f} max={img.max():.3f}"
        print("   ", info)
        if img.shape[:2] != expect_hw:
            errs.append(f"{sub}: size {img.shape[:2]} != {expect_hw}")
        if img.dtype == np.float32 and not (-0.02 <= img.min() and img.max() <= 1.02) and sub in ("FG", "Matte", "Comp"):
            errs.append(f"{sub}: values outside [0,1]")
    return errs


def manifest(clip):
    return json.loads((Path(clip.output_dir) / ".corridorkey_manifest.json").read_text())


def preview(clip, dest):
    f = min((Path(clip.output_dir) / "Comp").glob("*"))
    img = read_img(f)
    if img.dtype != np.uint8:
        img = (np.clip(img.astype(np.float32), 0, 1) ** (1 / 2.2) * 255 + 0.5).astype(np.uint8)
    h = int(img.shape[0] * 960 / img.shape[1])
    cv2.imwrite(dest, cv2.resize(img[..., :3], (960, h), interpolation=cv2.INTER_AREA))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--clip-src", required=True)
    ap.add_argument("--hint-src")
    ap.add_argument("--frames", type=int, default=24)
    ap.add_argument("--work", default=DEFAULT_WORK)
    ap.add_argument("--cases", default=",".join(CASES))
    a = ap.parse_args()
    work = os.path.abspath(a.work)
    scratch = os.path.join(work, "scratch")
    shutil.rmtree(scratch, ignore_errors=True)
    os.makedirs(scratch)
    os.environ["CORRIDORKEY_V2_SCRATCH_DIR"] = scratch
    n = a.frames
    first = next(p for p in sorted(Path(a.clip_src).iterdir()) if p.suffix.lower() in EXTS)
    expect_hw = read_img(first).shape[:2]
    svc = CorridorKeyService()
    results = []

    def run(name, fn):
        if name == "hint_auto" and not a.hint_src:
            results.append((name, "SKIPPED", "-", "-", "no --hint-src"))
            return
        phases = []
        torch.cuda.reset_peak_memory_stats()
        t0 = time.time()
        try:
            note = fn(phases) or ""
            status, reason = ("FAIL", note) if note else ("PASS", "")
        except Exception as e:  # noqa: BLE001
            import traceback
            traceback.print_exc()
            status, reason = "FAIL", f"{type(e).__name__}: {e}"
        dt = time.time() - t0
        peak = torch.cuda.max_memory_allocated() / 2**30
        results.append((name, status, f"{dt:.1f}s", f"{peak:.2f}GiB", ",".join(dict.fromkeys(phases)) + (f" | {reason}" if reason else "")))
        print(f"== {name}: {status} {dt:.1f}s peak={peak:.2f}GiB phases={phases and list(dict.fromkeys(phases))} {reason}")

    def progress_to(phases, extra=None):
        def cb(clip_name, cur, total, phase=None, **kw):
            phases.append(phase or "?")
            if extra:
                extra(phase, cur, total)
        return cb

    P = InferenceParams()
    cfg = OutputConfig(fg_format="exr")

    def nohint(phases):
        clip = build_clip("nohint_auto", work, a.clip_src, a.hint_src, n, False)
        svc.run_inference(clip, P, on_progress=progress_to(phases), output_config=cfg)
        errs = check_outputs(clip, n, expect_hw)
        clip.find_assets()
        if clip.state != ClipState.COMPLETE:
            errs.append(f"state {clip.state}")
        preview(clip, PREVIEWS["nohint_auto"])
        return "; ".join(errs)

    def rng(phases):
        clip = build_clip("range", work, a.clip_src, a.hint_src, n, False)
        svc.run_inference(clip, P, on_progress=progress_to(phases), output_config=cfg, frame_range=(5, 9))
        errs = []
        m = manifest(clip)
        nfg = len(list((Path(clip.output_dir) / "FG").glob("*")))
        if nfg != 5:
            errs.append(f"{nfg} FG frames != 5")
        if m.get("frame_range") != [5, 9]:
            errs.append(f"frame_range {m.get('frame_range')}")
        clip.find_assets()
        if clip.state == ClipState.COMPLETE:
            errs.append("COMPLETE after partial")
        svc.run_inference(clip, P, on_progress=progress_to(phases), output_config=cfg,
                          skip_stems=clip.completed_stems())
        errs += check_outputs(clip, n, expect_hw)
        m = manifest(clip)
        if m.get("complete") is not True or m.get("frame_range") is not None:
            errs.append(f"manifest complete={m.get('complete')} frame_range={m.get('frame_range')}")
        clip.find_assets()
        if clip.state != ClipState.COMPLETE:
            errs.append(f"state {clip.state} after full run")
        return "; ".join(errs)

    def cancel(phases):
        clip = build_clip("cancel", work, a.clip_src, a.hint_src, n, False)
        job = GPUJob(JobType.INFERENCE, clip.name)
        t = {}

        def on_cancel(phase, cur, total):
            if phase == "stage1" and "req" not in t:
                t["req"] = time.time()
                job.request_cancel()
        try:
            svc.run_inference(clip, P, job=job, on_progress=progress_to(phases, on_cancel), output_config=cfg)
            return "no JobCancelledError raised"
        except JobCancelledError as e:
            lat = time.time() - t.get("req", time.time())
            print(f"   cancel latency {lat:.2f}s exception={e!r}")
        if "req" not in t:
            return "stage1 never reported"
        errs = []
        left = list(Path(scratch).iterdir())
        if left:
            errs.append(f"scratch not empty: {[p.name for p in left][:5]}")
        mp = Path(clip.output_dir) / ".corridorkey_manifest.json"
        if mp.exists() and json.loads(mp.read_text()).get("complete") is True:
            errs.append("manifest complete after cancel")
        return "; ".join(errs)

    def hint(phases):
        clip = build_clip("hint_auto", work, a.clip_src, a.hint_src, n, True, expect_hw)
        svc.run_inference(clip, InferenceParams(use_alpha_hint=True), on_progress=progress_to(phases), output_config=cfg)
        errs = check_outputs(clip, n, expect_hw)
        preview(clip, PREVIEWS["hint_auto"])
        return "; ".join(errs)

    def low(phases):
        clip = build_clip("nohint_low", work, a.clip_src, a.hint_src, n, False)
        svc.run_inference(clip, InferenceParams(vram_profile="low"),
                          on_progress=progress_to(phases), output_config=cfg)
        errs = check_outputs(clip, n, expect_hw)
        if "stage2" not in phases:
            errs.append("no stage2 progress in low-VRAM mode")
        return "; ".join(errs)

    def png(phases):
        clip = build_clip("png_outputs", work, a.clip_src, a.hint_src, n, False)
        pcfg = OutputConfig(fg_format="png", matte_format="png", comp_format="png",
                            processed_format="png")
        svc.run_inference(clip, P, on_progress=progress_to(phases), output_config=pcfg)
        errs = check_outputs(clip, n, expect_hw)
        for sub in ("FG", "Matte", "Comp", "Processed"):
            exts = {f.suffix for f in (Path(clip.output_dir) / sub).glob("*")}
            if exts != {".png"}:
                errs.append(f"{sub}: extensions {exts}")
        proc = read_img(min((Path(clip.output_dir) / "Processed").glob("*")))
        if proc.ndim != 3 or proc.shape[2] != 4:
            errs.append(f"Processed PNG not RGBA: {proc.shape}")
        return "; ".join(errs)

    table = {"nohint_auto": nohint, "nohint_low": low, "png_outputs": png, "range": rng, "cancel": cancel, "hint_auto": hint}
    for name in a.cases.split(","):
        run(name.strip(), table[name.strip()])

    svc.unload_engines()
    print(f"after unload: memory_allocated={torch.cuda.memory_allocated()}")
    print("\ncase | status | time | peak | phases/reason")
    for r in results:
        print(" | ".join(r))
    sys.exit(1 if any(r[1] == "FAIL" for r in results) else 0)


if __name__ == "__main__":
    main()
