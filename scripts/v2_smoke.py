"""Real-GPU smoke test for the CorridorKey 2.0 shared service (no second inference impl)."""
import argparse
import json
import logging
import os
import re
import shutil
import sys
import time
from pathlib import Path

# CorridorKey 2.0 4K windows OOM from allocator fragmentation without this;
# must be set before torch initialises CUDA. Linux only (unsupported on Windows).
# torch 2.9 warns the old name is deprecated but ignores PYTORCH_ALLOC_CONF.
if sys.platform.startswith("linux"):
    os.environ.setdefault("PYTORCH_CUDA_ALLOC_CONF", "expandable_segments:True")

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import cv2
import numpy as np
import torch

from backend.clip_state import ClipEntry, ClipState
from backend.errors import JobCancelledError
from backend.hint_keyframes import save_hint_keyframes
from backend.job_queue import GPUJob, JobType
from backend.service import CorridorKeyService, InferenceParams, OutputConfig

DEFAULT_WORK = os.path.expanduser("~/.local/share/EZ-CorridorKey2/smoke/work")
CASES = ["nohint_auto", "nohint_low", "png_outputs", "range", "cancel", "hint_auto",
         "reference_mid", "keyframes_sparse", "manual_window"]
HINT_CASES = ("hint_auto", "reference_mid", "keyframes_sparse")  # need --hint-src, else SKIPPED
SPARSE_MARKS = (3, 17)  # absolute source indices (span several 8/4 windows)
MANUAL_WINDOW = "8/4"
PREVIEWS = {"nohint_auto": "/tmp/ezck2-smoke-nohint.jpg", "hint_auto": "/tmp/ezck2-smoke-hint.jpg"}
EXTS = {".exr", ".png"}


def hint_marks(case, n):
    """Absolute marked indices for a keyframe case (None = not a keyframe case)."""
    if case == "reference_mid":
        return (n // 2,)
    if case == "keyframes_sparse":
        return SPARSE_MARKS
    return None


def validate_cases(cases, n, hint_src, available=None):
    """Return error strings for unknown cases / unusable --frames. Pure, no I/O."""
    errs = [f"unknown case {c!r} (known: {', '.join(CASES)})" for c in cases if c not in CASES]
    if n < 1:
        errs.append(f"--frames must be >= 1, got {n}")
        return errs
    if available is not None and available < n:
        errs.append(f"--frames {n} but --clip-src has only {available} frames")
    runs_hint = bool(hint_src)
    if "reference_mid" in cases and runs_hint and n // 2 <= 0:
        errs.append("reference_mid needs --frames >= 2 (reference index n//2 must be > 0)")
    if "keyframes_sparse" in cases and runs_hint and n <= max(SPARSE_MARKS):
        errs.append(f"keyframes_sparse needs --frames >= {max(SPARSE_MARKS) + 1} "
                    f"(marks {SPARSE_MARKS})")
    return errs


def windows(n, chunk, stride):
    """Hann windows (start, end-exclusive) tiling n frames with the given stride."""
    out, s = [], 0
    while True:
        out.append((s, min(s + chunk, n)))
        if s + chunk >= n:
            return out
        s += stride


def check_marks_in_windows(marks, n, chunk, stride):
    errs = []
    wins = windows(n, chunk, stride)
    for k in marks:
        if not any(a <= k < b for a, b in wins):
            errs.append(f"mark {k} not covered by any {chunk}/{stride} window")
    if len(marks) > 1:
        shared = [w for w in wins if all(w[0] <= k < w[1] for k in marks)]
        if shared:
            errs.append(f"marks {list(marks)} all fit in one {chunk}/{stride} window {shared[0]}")
    return errs


def check_manifest(m, case, n, stems, marks, window):
    """Validate manifest provenance (real fields: params, hint_keyframes, source_stems, ...)."""
    errs = []
    params = m.get("params") or {}
    want_mode = "keyframes" if marks else ("full" if case == "hint_auto" else "none")
    if params.get("hint_mode") != want_mode:
        errs.append(f"manifest hint_mode {params.get('hint_mode')!r} != {want_mode!r}")
    if params.get("temporal_window") != (window or "auto"):
        errs.append(f"manifest temporal_window {params.get('temporal_window')!r} != {window or 'auto'!r}")
    if m.get("source_stems") != list(stems):
        errs.append("manifest source_stems != expected clip stems")
    if m.get("complete") is not True or m.get("frame_range") is not None:
        errs.append(f"manifest complete={m.get('complete')} frame_range={m.get('frame_range')}")
    if marks:
        if m.get("hint_keyframes") != list(marks):
            errs.append(f"manifest hint_keyframes {m.get('hint_keyframes')} != {list(marks)}")
    elif "hint_keyframes" in m:
        errs.append(f"unexpected manifest hint_keyframes {m.get('hint_keyframes')}")
    return errs


def build_clip(name, work, src, hint_src, n, with_hint, hw=None, keep=None, marks=None):
    """Disposable clip under ``work``. ``keep`` = absolute indices whose hint matte is
    physically written (None = all); ``marks`` = keyframe marks saved to the sidecar."""
    root = Path(work) / name
    shutil.rmtree(root, ignore_errors=True)
    (root / "Frames").mkdir(parents=True)
    files = sorted(p for p in Path(src).iterdir() if p.suffix.lower() in EXTS)[:n]
    for f in files:  # symlinks: source is read-only, never written
        (root / "Frames" / f.name).symlink_to(f.resolve())
    if with_hint:
        (root / "AlphaHint").mkdir()
        stems = ClipEntry(name=name, root_path=str(root))
        stems.find_assets()
        stems = stems._source_stems()
        selected = range(len(stems)) if keep is None else sorted(keep)
        if any(i < 0 or i >= len(stems) for i in selected):
            raise ValueError(f"hint indices {list(selected)} outside 0..{len(stems) - 1}")
        for i in selected:
            h = Path(hint_src) / (stems[i] + ".png")
            if not h.exists():
                raise FileNotFoundError(h)
            m = read_img(h)  # resize to clip resolution (source hints may differ)
            if m.ndim == 3:
                m = m[..., 0]
            if hw and m.shape[:2] != hw:
                m = cv2.resize(m, (hw[1], hw[0]), interpolation=cv2.INTER_LINEAR)
            cv2.imwrite(str(root / "AlphaHint" / h.name), m)
        if marks:
            save_hint_keyframes(str(root), list(marks), stems)
    clip = ClipEntry(name=name, root_path=str(root))
    clip.find_assets()
    return clip


def read_img(path):
    img = cv2.imread(str(path), cv2.IMREAD_UNCHANGED)
    if img is None:
        raise RuntimeError(f"unreadable output {path}")
    return img


def matte_range_errors(files):
    """Every matte frame must be finite and within [0,1] (tolerance .02)."""
    errs = []
    for f in files:
        img = read_img(f)
        if np.issubdtype(img.dtype, np.integer):
            img = img.astype(np.float32) / np.iinfo(img.dtype).max
        if not np.isfinite(img).all():
            errs.append(f"Matte {f.name}: non-finite values")
        elif img.min() < -0.02 or img.max() > 1.02:
            errs.append(f"Matte {f.name}: values outside [0,1] ({img.min():.3f}..{img.max():.3f})")
    return errs


def check_outputs(clip, n, expect_hw, stems=None):
    out = Path(clip.output_dir)
    errs = []
    for sub in ("FG", "Matte", "Comp", "Processed"):
        files = sorted((out / sub).glob("*")) if (out / sub).is_dir() else []
        if len(files) != n:
            errs.append(f"{sub}: {len(files)} files != {n}")
            continue
        if stems is not None and {f.stem for f in files} != set(stems):
            errs.append(f"{sub}: output stems != expected source stems")
        if sub == "Matte" and stems is not None:
            errs += matte_range_errors(files)
        img = read_img(files[0])
        info = f"{sub} {files[0].name} shape={img.shape} dtype={img.dtype} min={img.min():.3f} max={img.max():.3f}"
        print("   ", info)
        if img.shape[:2] != expect_hw:
            errs.append(f"{sub}: size {img.shape[:2]} != {expect_hw}")
        if img.dtype == np.float32 and not (-0.02 <= img.min() and img.max() <= 1.02) and sub == "Matte":
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
    case_names = [c.strip() for c in a.cases.split(",") if c.strip()]
    available = sum(1 for p in Path(a.clip_src).iterdir() if p.suffix.lower() in EXTS) \
        if os.path.isdir(a.clip_src) else 0
    errs = validate_cases(case_names, a.frames, a.hint_src, available)
    if errs:
        ap.error("; ".join(errs))  # before any destructive build
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
    engines = set()  # (engine id, model id) seen after each case; >1 = heavy model reloaded

    def run(name, fn):
        if name in HINT_CASES and not a.hint_src:
            results.append((name, "SKIPPED", "-", "-", "no --hint-src"))
            return
        phases = []
        torch.cuda.reset_peak_memory_stats()
        t0 = time.time()
        left0 = list(Path(scratch).iterdir())
        try:
            note = fn(phases) or ""
            if name != "cancel":  # cancel checks its own scratch
                left = [p.name for p in Path(scratch).iterdir()]
                if left and not left0:
                    note = "; ".join(filter(None, [note, f"scratch not empty: {left[:5]}"]))
            eid = (id(svc._v2_engine), id(getattr(svc._v2_engine, "_model", None)))
            if svc._v2_engine is not None:
                engines.add(eid)
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

    def go(clip, params, phases, tag, window=None):
        """Service run capturing warnings and the actual hann chunk/stride from its log."""
        warns, seen = [], []

        class H(logging.Handler):
            def emit(self, rec):
                mt = re.search(r"hann_chunk=(\d+) hann_stride=(\d+)", rec.getMessage())
                if mt:
                    seen.append((int(mt[1]), int(mt[2])))

        lg = logging.getLogger("backend.service.inference")
        old, h = lg.level, H()
        lg.setLevel(min(old or logging.INFO, logging.INFO))
        lg.addHandler(h)
        try:
            svc.run_inference(clip, params, on_progress=progress_to(phases), output_config=cfg,
                              on_warning=warns.append)
        finally:
            lg.removeHandler(h)
            lg.setLevel(old)
        for w in warns:
            print(f"   [{tag}] warning: {w}")
        return seen[-1] if seen else None

    def keyframe_case(name, phases):
        marks = hint_marks(name, n)
        clip = build_clip(name, work, a.clip_src, a.hint_src, n, True, expect_hw,
                          keep=marks, marks=marks)
        stems = clip._source_stems()
        errs = []
        kept = sorted(p.stem for p in (Path(clip.root_path) / "AlphaHint").iterdir())
        if kept != sorted(stems[k] for k in marks):
            errs.append(f"AlphaHint files {kept} != marked stems")
        if tuple(clip.hint_keyframes) != marks:
            errs.append(f"clip.hint_keyframes {clip.hint_keyframes} != {marks}")
        chunk, stride = (int(x) for x in MANUAL_WINDOW.split("/"))
        errs += check_marks_in_windows(marks, n, chunk, stride)
        print(f"   marks (absolute indices)={list(marks)} stems={[stems[k] for k in marks]}")
        win = go(clip, InferenceParams(hint_mode="keyframes", temporal_window=MANUAL_WINDOW),
                 phases, name)
        print(f"   hann chunk/stride={win}")
        if win != (chunk, stride):
            errs.append(f"effective window {win} != {(chunk, stride)}")
        errs += check_outputs(clip, n, expect_hw, stems)
        errs += check_manifest(manifest(clip), name, n, stems, marks, MANUAL_WINDOW)
        return "; ".join(errs)

    def reference(phases):
        return keyframe_case("reference_mid", phases)

    def sparse(phases):
        return keyframe_case("keyframes_sparse", phases)

    def manual(phases):
        clip = build_clip("manual_window", work, a.clip_src, a.hint_src, n, False)
        stems = clip._source_stems()
        win = go(clip, InferenceParams(temporal_window=MANUAL_WINDOW), phases, "manual_window")
        chunk, stride = (int(x) for x in MANUAL_WINDOW.split("/"))
        print(f"   manual window chunk/stride={win}")
        errs = []
        if win != (chunk, stride):
            errs.append(f"effective window {win} != {(chunk, stride)}")
        errs += check_outputs(clip, n, expect_hw, stems)
        errs += check_manifest(manifest(clip), "manual_window", n, stems, None, MANUAL_WINDOW)
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

    table = {"nohint_auto": nohint, "nohint_low": low, "png_outputs": png, "range": rng, "cancel": cancel, "hint_auto": hint,
             "reference_mid": reference, "keyframes_sparse": sparse, "manual_window": manual}
    for name in case_names:
        run(name, table[name])

    svc.unload_engines()
    print(f"after unload: memory_allocated={torch.cuda.memory_allocated()}")
    print("\ncase | status | time | peak | phases/reason")
    for r in results:
        print(" | ".join(r))
    print(f"heavy engine instances across cases: {len(engines)} {'(reused)' if len(engines) <= 1 else '(RELOADED)'}")
    sys.exit(1 if any(r[1] == "FAIL" for r in results) or len(engines) > 1 else 0)


if __name__ == "__main__":
    main()
