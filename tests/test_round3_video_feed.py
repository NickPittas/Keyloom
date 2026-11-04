"""Round 3 T14: EXR -> video exports let FFmpeg read the EXR sequence; same pixels as the old Python decode + raw pipe."""
import shutil
import subprocess
import threading
import time

import numpy as np
import pytest

from backend.service import task_job
from tests import test_task_job as tj
from tests.test_task_job import A, B, STEMS, TaskRejected, export_job, fake_ffmpeg, run

env, svc = tj.env, tj.svc  # fixtures

pytestmark = pytest.mark.skipif(not shutil.which("ffmpeg"), reason="needs ffmpeg")
H, W = 48, 64


def _frame(i, kind):
    """Gradient test frame in cv2 BGR(A) order with distinct R,G,B; 'hdr' goes below 0 and above 1."""
    y, x = np.mgrid[0:H, 0:W].astype(np.float32)
    r, g, b = x / W, y / H, np.full((H, W), 0.1 + 0.1 * i, np.float32)
    if kind == "hdr":
        r, g = r * 2.5 - 0.2, g * 1.8 - 0.1
    img = np.stack([b, g, r] + ([(x + y) / (W + H)] if kind == "alpha" else []), -1).astype(np.float32)
    return img


def result(env, kind, product="fg", compression="zip"):
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = env.tmp / f"res-{kind}-{product}"
    (out / "P").mkdir(parents=True)
    rels = []
    for i in range(A, B + 1):
        rel = f"P/{STEMS[i]}.exr"
        rels.append(rel)
        _write_exr_float32(str(out / rel), _frame(i, kind), compression)
    import json
    (out / ".corridorkey_manifest.json").write_text(json.dumps({
        "version": 3, "engine": "corridorkey-v2", "clip_id": "proj/clip1",
        "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]},
        "source_stems": STEMS[A:B + 1], "files": {product: rels}, "complete": True}))
    return out


def _write_exr_half_rgba(path, img):
    import Imath
    import OpenEXR
    header = OpenEXR.Header(W, H)
    header["compression"] = Imath.Compression(Imath.Compression.DWAB_COMPRESSION)
    half = Imath.PixelType(Imath.PixelType.HALF)
    header["channels"] = {c: Imath.Channel(half) for c in "RGBA"}
    f = OpenEXR.OutputFile(str(path), header)
    try:
        f.writePixels({c: np.asarray(img[..., i], np.float16).tobytes()
                       for c, i in (("R", 2), ("G", 1), ("B", 0), ("A", 3))})
    finally:
        f.close()


def decode(path):
    r = subprocess.run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "rawvideo", "-pix_fmt", "rgba", "-"],
                       capture_output=True, timeout=30)
    assert r.returncode == 0, r.stderr.decode()
    return np.frombuffer(r.stdout, np.uint8).reshape(-1, H, W, 4).astype(int)


def spy_cmds(monkeypatch):
    cmds = []
    real = subprocess.Popen

    def spy(cmd, *a, **k):
        cmds.append((list(cmd), k.get("stdin")))
        return real(cmd, *a, **k)
    monkeypatch.setattr(task_job.subprocess, "Popen", spy)
    return cmds


def export(env, svc, out, name, product, container, old, monkeypatch):
    """Run an export; ``old`` forces the previous Python-decode + pipe path."""
    with monkeypatch.context() as m:
        if old:
            real = task_job._exr_layout
            m.setattr(task_job, "_exr_layout", lambda p: (*real(p)[:3], False))
        cmds = spy_cmds(m)
        dest = env.tmp / "deliver" / name
        _, done = run(svc, export_job(env, out, product=product, container=container, dest=str(dest)))
    assert done["outcome"] == "complete", done
    return dest, [c for c in cmds if "-an" in c[0]][0]


@pytest.mark.parametrize("kind,product,container,ext,tol,comp,direct", [
    ("linear", "fg", "mp4", ".mp4", 8, "zip", True),                    # H.264 without alpha
    ("hdr", "fg", "mp4", ".mp4", 8, "zip", True),                       # negatives and >1 clip like before
    ("linear", "fg", "mp4", ".mp4", 8, "dwab", True),                   # Key's default compression, RGB
    ("alpha", "processed", "mov_prores4444", ".mov", 3, "zip", True),   # ProRes 4444 keeps real alpha
    ("alpha", "processed", "mp4", ".mp4", 8, "zip", True),              # RGBA EXR into a codec without alpha
    ("alpha", "processed", "mov_prores4444", ".mov", 3, "dwab", False),  # FLOAT alpha uses Python decode
])
def test_ffmpeg_reads_exr_same_pixels_as_python_pipe(env, svc, monkeypatch, kind, product, container, ext, tol,
                                                     comp, direct):
    out = result(env, kind, product, comp)
    new, (cmd, stdin) = export(env, svc, out, f"new{ext}", product, container, False, monkeypatch)
    old, (ocmd, ostdin) = export(env, svc, out, f"old{ext}", product, container, True, monkeypatch)
    if direct:
        assert "rawvideo" not in cmd and stdin == subprocess.DEVNULL and "lutrgb" in " ".join(cmd)  # reads files
    else:
        assert "rawvideo" in cmd and stdin == subprocess.PIPE  # explicit safe fallback
    assert "rawvideo" in ocmd and ostdin == subprocess.PIPE
    a, b = decode(new), decode(old)
    assert a.shape == b.shape == (B - A + 1, H, W, 4)
    d = np.abs(a - b)  # H.264 quantisation differs a little between two equal-quality encodes
    assert d.max() <= tol and d.mean() < 1.0, (d.max(), d.mean())
    if container == "mov_prores4444":  # alpha is real and not flat
        assert a[..., 3].min() < 40 and a[..., 3].max() > 215
    assert not list((env.tmp / "deliver").glob("*.part*"))


def test_half_alpha_dwab_uses_ffmpeg_and_matches_python(env, svc, monkeypatch):
    pytest.importorskip("OpenEXR")
    out = result(env, "alpha", "processed", "dwab")
    files = sorted((out / "P").iterdir())
    for i, f in enumerate(files, A):
        _write_exr_half_rgba(f, _frame(i, "alpha"))
    direct, (cmd, stdin) = export(env, svc, out, "half-direct.mov", "processed", "mov_prores4444", False, monkeypatch)
    python, _ = export(env, svc, out, "half-python.mov", "processed", "mov_prores4444", True, monkeypatch)
    assert "rawvideo" not in cmd and stdin == subprocess.DEVNULL
    a, b = decode(direct), decode(python)
    assert np.abs(a[..., 3].astype(int) - b[..., 3].astype(int)).max() <= 3


def test_float_alpha_dwab_uses_python_decode(env, svc, monkeypatch):
    out = result(env, "alpha", "processed", "dwab")
    _, (cmd, stdin) = export(env, svc, out, "float-python.mov", "processed", "mov_prores4444", False, monkeypatch)
    assert "rawvideo" in cmd and stdin == subprocess.PIPE


def test_frame_order_follows_the_manifest(env, svc):
    out = result(env, "linear")
    dest = env.tmp / "deliver" / "o.mp4"
    run(svc, export_job(env, out, dest=str(dest)))
    blue = decode(dest)[:, H // 2, W // 2, 2]  # B channel steps up 0.1 per frame (sRGB-encoded, so monotonic)
    assert list(blue) == sorted(blue) and blue[0] < blue[-1]


def test_matte_y_exr_still_uses_the_python_pipe(env, svc, monkeypatch):
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = result(env, "linear", "matte")
    for f in (out / "P").iterdir():
        _write_exr_float32(str(f), np.full((H, W), 0.5, np.float32), "dwab")  # FFmpeg rejects Y DWAB
    cmds = spy_cmds(monkeypatch)
    dest = env.tmp / "deliver" / "m.mp4"
    _, done = run(svc, export_job(env, out, product="matte", dest=str(dest)))
    assert done["outcome"] == "complete" and "rawvideo" in [c for c in cmds if "-an" in c[0]][0][0]
    assert abs(decode(dest)[:, H // 2, W // 2, 0] - 125).max() <= 6  # data as-is, no sRGB curve


def test_mismatched_frame_size_is_rejected_and_nothing_is_left(env, svc):
    from backend.service.key_job import _write_exr_float32
    out = result(env, "linear")
    _write_exr_float32(str(sorted((out / "P").iterdir())[1]), np.zeros((H + 2, W, 3), np.float32), "zip")
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "s.mp4")))
    assert done["outcome"] == "failed" and "first frame" in done["message"]
    assert not list((env.tmp / "deliver").glob("s.mp4*"))


def test_truncated_frame_fails_without_publishing(env, svc):
    out = result(env, "linear")
    f = sorted((out / "P").iterdir())[1]
    f.write_bytes(f.read_bytes()[:40])
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "t.mp4")))
    assert done["outcome"] == "failed", done
    assert not list((env.tmp / "deliver").glob("t.mp4*"))


def test_cancel_kills_ffmpeg_and_removes_part(env, svc, monkeypatch):
    out = result(env, "linear")
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    ev = threading.Event()
    threading.Timer(0.7, ev.set).start()
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "c.mp4")), ev)
    assert done["outcome"] == "cancelled", done
    assert procs and all(p.poll() is not None for p in procs)
    assert not [t for t in threading.enumerate() if t.name.startswith("export-") and t.is_alive()]
    assert not list((env.tmp / "deliver").glob("c.mp4*"))
    assert not list((env.tmp / "scratch").glob("export-*"))


def test_existing_destination_is_never_replaced(env, svc):
    out = result(env, "linear")
    dest = env.tmp / "deliver" / "e.mp4"
    dest.parent.mkdir()
    dest.write_bytes(b"x")
    with pytest.raises(TaskRejected):
        run(svc, export_job(env, out, dest=str(dest)))
    assert dest.read_bytes() == b"x"


class _Tee:
    """Wraps FFmpeg's stdin and records every byte written to it."""
    def __init__(self, real, sink):
        self.real, self.sink = real, sink

    def write(self, data):
        self.sink.append(bytes(data))
        return self.real.write(data)

    def __getattr__(self, name):
        return getattr(self.real, name)


def _tee_stdin(monkeypatch):
    sink, real = [], subprocess.Popen

    def popen(cmd, *a, **k):
        proc = real(cmd, *a, **k)
        if k.get("stdin") == subprocess.PIPE:
            proc.stdin = _Tee(proc.stdin, sink)
        return proc
    monkeypatch.setattr(task_job.subprocess, "Popen", popen)
    return sink


def _slow_reader(monkeypatch):
    """Earlier frames decode slower, so out-of-order delivery would show; returns the live-decode high-water mark."""
    real, lock, state = task_job._read_exr, threading.Lock(), {"now": 0, "max": 0}

    def read(path):
        with lock:
            state["now"] += 1
            state["max"] = max(state["max"], state["now"])
        try:
            time.sleep(0.2 - 0.06 * (int(path[-5]) % 4))
            return real(path)
        finally:
            with lock:
                state["now"] -= 1
    monkeypatch.setattr(task_job, "_read_exr", read)
    return state


@pytest.mark.parametrize("product,y_only", [("processed", False), ("matte", True)])
def test_threaded_fallback_pipes_exactly_the_serial_bytes_in_order(env, svc, monkeypatch, product, y_only):
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = result(env, "alpha", product, "dwab")  # FLOAT alpha keeps this on the Python pipe path
    files = sorted((out / "P").iterdir())
    if y_only:
        for i, f in enumerate(files):
            _write_exr_float32(str(f), np.full((H, W), 0.1 + 0.2 * i, np.float32), "dwab")
    expected = b"".join(task_job._raw_frame(task_job._read_exr(str(f)), product != "matte") for f in files)
    sink, state = _tee_stdin(monkeypatch), _slow_reader(monkeypatch)
    _, done = run(svc, export_job(env, out, product=product, container="mov_prores4444" if not y_only else "mp4",
                                  dest=str(env.tmp / "deliver" / ("t.mp4" if y_only else "t.mov"))))
    assert done["outcome"] == "complete", done
    assert b"".join(sink) == expected and len(sink) == len(files)
    assert 1 < state["max"] <= 4  # decoded in parallel, never more than 4 frames at once


def test_threaded_fallback_error_is_first_and_cleans_up(env, svc, monkeypatch):
    out = result(env, "alpha", "processed", "dwab")  # noqa
    bad = sorted((out / "P").iterdir())[1]
    bad.write_bytes(bad.read_bytes()[:40])
    _, done = run(svc, export_job(env, out, product="processed", container="mov_prores4444", dest=str(env.tmp / "deliver" / "x.mov")))
    assert done["outcome"] == "failed" and "frame 2/" in done["message"], done
    assert not [t for t in threading.enumerate() if t.name.startswith(("export-", "frame-")) and t.is_alive()]
    assert not list((env.tmp / "deliver").glob("x.mov*")) and not list((env.tmp / "scratch").glob("export-*"))


def test_threaded_fallback_cancel_stops_decoding(env, svc, monkeypatch):
    out = result(env, "alpha", "processed", "dwab")  # noqa
    real = task_job._read_exr
    monkeypatch.setattr(task_job, "_read_exr", lambda p: (time.sleep(0.3), real(p))[1])
    ev = threading.Event()
    threading.Timer(0.5, ev.set).start()
    _, done = run(svc, export_job(env, out, product="processed", container="mov_prores4444", dest=str(env.tmp / "deliver" / "c.mov")), ev)
    assert done["outcome"] == "cancelled", done
    assert not [t for t in threading.enumerate() if t.name.startswith("export-") and t.is_alive()]
    assert not list((env.tmp / "deliver").glob("c.mov*")) and not list((env.tmp / "scratch").glob("export-*"))
