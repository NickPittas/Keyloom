import os
import shutil
import sys
import time
from types import SimpleNamespace

import cv2
import numpy as np
import pytest

from backend import frame_io  # noqa: F401  (enables OpenCV EXR before cv2 writes one)
from backend.errors import CorridorKeyError
from backend.ffmpeg_tools import find_ffmpeg
from backend.service import prepared_frames as pf
from backend.service.frame_ops import FrameOpsMixin

pytestmark = pytest.mark.skipif(find_ffmpeg() is None, reason="needs FFmpeg")


class Svc(FrameOpsMixin):
    pass


def gradient(h=48, w=64):
    y, x = np.mgrid[0:h, 0:w].astype(np.float32)
    return np.dstack([x / (w - 1), y / (h - 1), 0.25 + 0.5 * (x + y) / (w + h - 2)])


@pytest.fixture
def env(tmp_path, monkeypatch):
    src = tmp_path / "Frames"
    src.mkdir()
    for i in range(4):
        cv2.imwrite(str(src / f"f{i}.png"), np.full((8, 10, 3), 40 * i + 10, np.uint8))
    batches = []
    real = pf._ffmpeg_batch
    monkeypatch.setattr(pf, "_ffmpeg_batch", lambda srcs, *a: (batches.append(list(srcs)), real(srcs, *a))[1])
    asset = SimpleNamespace(path=str(src))
    names = sorted(os.listdir(src))
    root = tmp_path / "proj" / ".ck_staging" / "clip" / "sam-frames"

    def run(n=names, linear=False, cancel=None, on_status=None, a=asset, r=root):
        return pf.prepare(a, n, "clip", str(r), input_is_linear=linear, check_cancel=cancel, on_status=on_status)
    return SimpleNamespace(src=src, batches=batches, run=run, names=names, tmp=tmp_path, root=root, asset=asset)


def converted(env):
    return sum(len(b) for b in env.batches)


def test_second_track_converts_nothing_and_uses_cached_files_in_place(env, monkeypatch):
    paths, n, shape = env.run()
    assert (n, shape) == (4, (8, 10)) and converted(env) == 4 and len(env.batches) == 1  # ONE FFmpeg batch
    assert all(os.path.dirname(p).startswith(str(env.root)) for p in paths)
    monkeypatch.setattr(os, "link", lambda *a: pytest.fail("no links"))
    monkeypatch.setattr(shutil, "copyfile", lambda *a, **k: pytest.fail("no copies"))
    monkeypatch.setattr(shutil, "copy2", lambda *a, **k: pytest.fail("no copies"))
    again, *_ = env.run()  # also a "restart": nothing is held in memory
    assert again == paths and converted(env) == 4


def test_changed_source_colour_miss_only_what_differs(env):
    env.run()
    cv2.imwrite(str(env.src / "f1.png"), np.full((8, 10, 3), 250, np.uint8))
    env.run()
    assert converted(env) == 5
    env.run(linear=True)  # colour interpretation = different cache entry
    assert converted(env) == 9


def test_corrupt_cached_jpeg_is_repaired_subset_and_extension(env):
    env.run(env.names[:2])
    env.run(env.names[1:3])  # subset + extension: only f2 is new
    assert converted(env) == 3
    next(env.root.rglob("f0.jpg")).write_bytes(b"bad")
    env.run(env.names[:2])
    assert converted(env) == 4


def test_cancel_leaves_no_completion_and_no_part_files(env):
    n = []

    def cancel():
        n.append(1)
        if len(n) > 10:  # scan 4 + hash 4, then after FFmpeg, mid-verification
            raise RuntimeError("cancelled")
    with pytest.raises(RuntimeError):
        env.run(cancel=cancel)
    assert not [p for p in env.root.rglob("*") if p.suffix == ".jpg" or ".part-" in p.name]
    env.run()
    assert converted(env) == 8  # the cancelled run cached nothing


def test_cancel_kills_the_ffmpeg_process(tmp_path):
    marker = tmp_path / "pid"
    code = f"import os,time;open({str(marker)!r},'w').write(str(os.getpid()));time.sleep(60)"
    t0 = time.time()

    def check():
        if marker.exists() and marker.read_text():
            raise RuntimeError("cancelled")
    with pytest.raises(RuntimeError):
        pf._run([sys.executable, "-c", code], str(tmp_path / "log"), check)
    pid = int(marker.read_text())
    time.sleep(0.5)
    assert time.time() - t0 < 20
    with pytest.raises(ProcessLookupError):
        os.kill(pid, 0)


def test_source_replaced_during_conversion_is_never_cached(env, monkeypatch):
    real = pf._ffmpeg_batch

    def swap(srcs, *a):
        out = real(srcs, *a)
        cv2.imwrite(srcs[1], np.full((8, 10, 3), 7, np.uint8))  # replaced while FFmpeg ran
        os.utime(srcs[1], ns=(1, 1))
        return out
    monkeypatch.setattr(pf, "_ffmpeg_batch", swap)
    with pytest.raises(CorridorKeyError, match="changed while"):
        env.run()
    assert not list(env.root.rglob("*.jpg"))
    monkeypatch.setattr(pf, "_ffmpeg_batch", real)
    env.run()
    assert len(list(env.root.rglob("f?.jpg"))) == 4


def test_unwritable_cache_is_actionable(env, monkeypatch):
    monkeypatch.setattr(os, "makedirs", lambda *a, **k: (_ for _ in ()).throw(PermissionError("no")))
    with pytest.raises(CorridorKeyError, match="prepared-frame cache"):
        env.run()


def test_reuse_reads_no_jpeg_and_no_source_when_stats_match(env, monkeypatch):
    env.run()
    monkeypatch.setattr(pf, "_digest", lambda p: pytest.fail("a matching stat must not re-read " + p))
    env.run()
    assert converted(env) == 4


def test_converting_never_hashes_a_source_frame(env, monkeypatch):
    real = pf._digest
    seen = []
    monkeypatch.setattr(pf, "_digest", lambda p: (seen.append(p), real(p))[1])
    env.run()
    assert seen and all(p.endswith(".jpg") for p in seen)  # only our own outputs, never Frames/*


def test_touched_but_identical_jpeg_is_rehashed_once_not_reconverted(env):
    paths, *_ = env.run()
    os.utime(paths[0], ns=(5_000_000_000, 5_000_000_000))
    env.run()
    assert converted(env) == 4
    import json
    m = json.loads(next(env.root.rglob("manifest.json")).read_text())
    assert m["entries"]["f0"]["jpeg_stat"][1] == 5_000_000_000


def test_cache_key_ignores_where_the_project_is_mounted(env):
    clip = env.tmp / "proj"
    clip.mkdir(exist_ok=True)
    os.rename(env.src, clip / "Frames")
    asset = SimpleNamespace(path=str(clip / "Frames"))
    root = clip / "stg" / "sam-frames"
    pf.prepare(asset, env.names, "clip", str(root), input_is_linear=False, rel_base=str(clip))
    moved = env.tmp / "elsewhere"  # "the other PC" sees the same share under another path / drive
    os.rename(clip, moved)
    pf.prepare(SimpleNamespace(path=str(moved / "Frames")), env.names, "clip",
               str(moved / "stg" / "sam-frames"), input_is_linear=False, rel_base=str(moved))
    assert converted(env) == 4


def test_other_pcs_recent_temporary_files_survive_old_ones_go(env):
    env.run()
    folder = next(env.root.rglob("manifest.json")).parent
    fresh, old = folder / ".part-live-000001.jpg", folder / ".part-dead-000001.jpg"
    fresh.write_bytes(b"in flight")
    old.write_bytes(b"left by a kill")
    long_ago = time.time() - pf.PART_STALE_S - 10
    os.utime(old, (long_ago, long_ago))
    env.run()
    assert fresh.exists() and not old.exists()


def test_jpeg_sources_use_the_apps_reader_and_keep_exif_orientation(tmp_path):
    from PIL import Image
    d = tmp_path / "jf"
    d.mkdir()
    exif = Image.Exif()
    exif[0x0112] = 6  # rotate 90 CW when displayed
    for i in range(2):
        Image.new("RGB", (10, 8), (200, 30, 30)).save(d / f"j{i}.jpg", exif=exif)
    paths, n, shape = pf.prepare(SimpleNamespace(path=str(d)), sorted(os.listdir(d)), "c",
                                 str(tmp_path / "r"), input_is_linear=False)
    _, ref = next(Svc()._iter_named_sequence_frames(SimpleNamespace(path=str(d)), ["j0.jpg"], "c", gamma_correct_exr=False))
    assert shape == ref.shape[:2] == (10, 8)  # same orientation as the app's other readers
    assert Image.open(paths[0]).size == (8, 10)


# ---- colour: FFmpeg's JPEG must equal the app's own in-Python conversion --------------------------

def make_frames(tmp_path, kind):
    d = tmp_path / kind
    d.mkdir()
    img = gradient()
    if kind == "png":
        cv2.imwrite(str(d / "a.png"), cv2.cvtColor((img * 255).astype(np.uint8), cv2.COLOR_RGB2BGR))
    elif kind == "png16":
        cv2.imwrite(str(d / "a.png"), cv2.cvtColor((img * 65535).astype(np.uint16), cv2.COLOR_RGB2BGR))
    else:
        lin = img.copy()
        if kind == "linear_exr":
            lin = lin ** 2.2 * 1.3  # linear light incl. values above 1
            lin[:4, :4] = -0.2  # and negatives
        cv2.imwrite(str(d / "a.exr"), cv2.cvtColor(lin.astype(np.float32), cv2.COLOR_RGB2BGR))
    return SimpleNamespace(path=str(d)), sorted(os.listdir(d))


def diffs(tmp_path, kind, linear, vf=None, monkeypatch=None):
    asset, names = make_frames(tmp_path, kind + ("_v" if vf else ""))
    if vf:
        monkeypatch.setattr(pf, "build_sam_jpeg_vf", lambda _linear: vf)
    paths, *_ = pf.prepare(asset, names, "c", str(tmp_path / ("root_" + kind + ("_v" if vf else ""))),
                           input_is_linear=linear)
    ours = cv2.cvtColor(cv2.imread(paths[0]), cv2.COLOR_BGR2RGB).astype(np.int16)
    _, ref = next(Svc()._iter_named_sequence_frames(asset, names, "c", gamma_correct_exr=linear))
    d = np.abs(ours - ref.astype(np.int16))
    return float(d.mean()), float(np.percentile(d, 99))  # JPEG ringing at hard edges is not a colour error


@pytest.mark.parametrize("kind,linear", [("png", False), ("png16", False), ("display_exr", False), ("linear_exr", True)])
def test_ffmpeg_jpeg_matches_the_apps_own_conversion(tmp_path, kind, linear):
    mean, worst = diffs(tmp_path, kind, linear)
    assert mean < 1.6 and worst <= 6, (mean, worst)


@pytest.mark.parametrize("kind,linear,bad", [
    ("linear_exr", True, "format=rgb24,format=yuvj420p"),                         # sRGB curve forgotten
    ("display_exr", False, "format=rgb24,colorchannelmixer=rr=0:rb=1:bb=0:br=1,format=yuvj420p"),  # R/B swapped
    ("png", False, "format=rgb24,format=yuvj420p,lutyuv=y='16+val*219/255'"),   # limited-range luma
    ("display_exr", False, "format=rgb24,eq=gamma=1.4,format=yuvj420p"),          # stray gamma
])
def test_colour_channel_or_range_mistakes_are_caught(tmp_path, monkeypatch, kind, linear, bad):
    mean, worst = diffs(tmp_path, kind, linear, vf=bad, monkeypatch=monkeypatch)
    assert mean >= 1.6 or worst > 6, (mean, worst)


def test_sam2_reads_the_listed_jpegs_in_place(env):
    pytest.importorskip("sam2")
    torch = pytest.importorskip("torch")
    import sam2.sam2_video_predictor as spv
    from sam2_tracker.wrapper import _listed_frame_loader
    paths, n, _ = env.run()
    original = spv.load_video_frames
    with _listed_frame_loader():
        images, h, w = spv.load_video_frames(paths, 16, True, compute_device=torch.device("cpu"))
    assert spv.load_video_frames is original
    assert tuple(images.shape) == (4, 3, 16, 16) and (h, w) == (8, 10)
