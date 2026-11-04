"""Round 3 T9: every pre-matte/hint producer writes through the bounded encoder, same pixels as before."""
import os
import sys
import threading
import types
from types import SimpleNamespace

import cv2
import numpy as np
import pytest

from backend import frame_encoding as fe
from backend.frame_io import imread_unicode


def _read(path):
    return imread_unicode(str(path), cv2.IMREAD_UNCHANGED | cv2.IMREAD_ANYDEPTH)


def _fail_images(monkeypatch, match):
    """Make the encoder's PNG write fail for paths containing ``match``."""
    real = fe._write_image_job
    monkeypatch.setattr(fe, "_write_image_job", lambda p, i, params: False if match in p else real(p, i, params))


# ---- annotations -----------------------------------------------------------------------------------------------

def _store():
    from backend.annotations import AnnotationModel
    return AnnotationModel()


def test_annotation_export_pixels_and_failure(tmp_path, monkeypatch):
    store = _store()
    d = store.export_masks(str(tmp_path), ["fé_0001", "fé_0002"], 8, 6)
    for stem in ("fé_0001", "fé_0002"):
        m = _read(os.path.join(d, f"{stem}.png"))
        assert m.shape == (6, 8) and m.dtype == np.uint8 and not m.any()
    _fail_images(monkeypatch, "fé_0002")
    with pytest.raises(fe.EncodingError):
        store.export_masks(str(tmp_path / "x"), ["fé_0001", "fé_0002"], 8, 6)


# ---- GVM writer ------------------------------------------------------------------------------------------------

def test_gvm_writer_matches_pil_and_reports_failure(tmp_path, monkeypatch):
    torch = pytest.importorskip("torch")
    iu = pytest.importorskip("gvm_core.gvm.utils.inference_utils")
    from torchvision.transforms.functional import to_pil_image
    alpha = torch.rand(3, 1, 7, 9)
    alpha[0, 0, 0, 0] = 1.0
    w = iu.ImageSequenceWriter(str(tmp_path / "out"), extension="png")
    w.write(alpha, filenames=["a.jpg", "b.jpg", "c.jpg"])
    w.close()
    for i, stem in enumerate("abc"):
        ref = np.array(to_pil_image(alpha[i]))
        assert np.array_equal(_read(tmp_path / "out" / f"{stem}.png"), ref)
    assert w.counter == 3
    _fail_images(monkeypatch, "b.png")
    w2 = iu.ImageSequenceWriter(str(tmp_path / "bad"), extension="png")
    with pytest.raises(fe.EncodingError):  # raised by a later write or by close(), never lost
        w2.write(alpha, filenames=["a", "b", "c"])
        w2.close()


def test_gvm_writer_abort_stops_workers(tmp_path):
    torch = pytest.importorskip("torch")
    iu = pytest.importorskip("gvm_core.gvm.utils.inference_utils")
    w = iu.ImageSequenceWriter(str(tmp_path), extension="png")
    w.write(torch.rand(1, 1, 4, 4), filenames=["a"])
    w.abort()
    assert w._encoder is None
    w.close()  # idempotent after abort


# ---- BiRefNet / MatAnyone2 -------------------------------------------------------------------------------------

def _birefnet(cancel_at=None):
    torch = pytest.importorskip("torch")
    mod = pytest.importorskip("modules.BiRefNetModule.wrapper")
    proc = mod.BiRefNetProcessor(device="cpu")
    proc._half = False
    proc._resolution = (32, 32)
    proc._model = lambda x: [torch.zeros(x.shape[0], 1, 32, 32) + torch.linspace(-3, 3, 32)]
    return proc, mod


def test_birefnet_sink_pixels_stems_and_cancel(tmp_path):
    proc, _ = _birefnet()
    frames = [np.random.randint(0, 255, (24, 40, 3), np.uint8) for _ in range(5)]
    names = ["é_0001", "é_0002", "é_0003"]  # fewer names than frames -> numbered fallback
    out = str(tmp_path / "out")
    n = proc.process_frames(iter(frames), out, names, num_frames=5)
    assert n == 5
    assert sorted(os.listdir(out)) == sorted([f"{x}.png" for x in names] + ["frame_000003.png", "frame_000004.png"])
    ref = _read(os.path.join(out, "é_0001.png"))
    assert ref.shape == (24, 40) and ref.dtype == np.uint8 and ref.min() < ref.max()
    # cancel before publish: nothing in the final dir, temp removed
    out2 = str(tmp_path / "out2")
    calls = {"n": 0}

    def cancel():
        calls["n"] += 1
        return calls["n"] > 3
    with pytest.raises(Exception):
        proc.process_frames(iter(frames), out2, names, cancel_check=cancel, num_frames=5)
    assert not os.path.exists(out2) and not os.path.exists(out2 + "._birefnet_tmp")


def test_birefnet_write_failure_publishes_nothing(tmp_path, monkeypatch):
    proc, _ = _birefnet()
    _fail_images(monkeypatch, "é_0002")
    frames = [np.zeros((24, 40, 3), np.uint8) for _ in range(3)]
    out = str(tmp_path / "out")
    with pytest.raises(RuntimeError):
        proc.process_frames(iter(frames), out, ["é_0001", "é_0002", "é_0003"], num_frames=3)
    assert not os.path.exists(out) and not os.path.exists(out + "._birefnet_tmp")


class _FakeCore:
    curr_ti = last_mem_ti = 0

    def __init__(self):
        self.n = 0

    def step(self, image, mask=None, objects=None, first_frame_pred=False):
        self.n += 1
        return image.mean(0)

    def output_prob_to_mask(self, prob):
        return prob.clamp(0, 1)

    def clear_memory(self):
        pass


def _matanyone():
    pytest.importorskip("torch")
    mod = pytest.importorskip("modules.MatAnyone2Module.wrapper")
    proc = mod.MatAnyone2Processor(device="cpu", n_warmup=1, r_erode=0, r_dilate=0)
    proc._processor = _FakeCore()
    return proc


def test_matanyone_sink_pixels_stems_cancel_and_failure(tmp_path, monkeypatch):
    frames = [np.full((10, 12, 3), 40 * i + 5, np.uint8) for i in range(4)]
    mask = np.zeros((10, 12), np.uint8)
    out = str(tmp_path / "out")
    n = _matanyone().process_frames(frames, mask, out, ["é_1", "é_2", "é_3"], clip_name="c")
    assert n == 4 and sorted(os.listdir(out)) == ["frame_000003.png", "é_1.png", "é_2.png", "é_3.png"]
    for i, name in enumerate(["é_1", "é_2", "é_3"]):
        want = np.round(np.clip((40 * i + 5) / 255.0, 0, 1) * 255).astype(np.uint8)
        got = _read(os.path.join(out, name + ".png"))
        assert got.shape == (10, 12) and (got == want).all()
    out2 = str(tmp_path / "out2")
    seen = {"n": 0}

    def cancel():
        seen["n"] += 1
        return seen["n"] > 3
    with pytest.raises(Exception):
        _matanyone().process_frames(frames, mask, out2, ["a", "b", "c", "d"], cancel_check=cancel, clip_name="c")
    assert not os.path.exists(out2) and not os.path.exists(out2 + "._matanyone2_tmp")
    _fail_images(monkeypatch, "é_2")
    out3 = str(tmp_path / "out3")
    with pytest.raises(RuntimeError):
        _matanyone().process_frames(frames, mask, out3, ["é_1", "é_2", "é_3", "é_4"], clip_name="c")
    assert not os.path.exists(out3) and not os.path.exists(out3 + "._matanyone2_tmp")


# ---- VideoMaMa -------------------------------------------------------------------------------------------------

def _videomama(tmp_path, chunks):
    from backend.service.pipelines_guided import GuidedPipelinesMixin
    root = tmp_path / "clip"
    inp = root / "Input"
    msk = root / "mask"
    inp.mkdir(parents=True)
    msk.mkdir()
    names = [f"fé_{i:04d}.png" for i in range(3)]
    for n in names:
        cv2.imwrite(str(inp / n), np.zeros((4, 6, 3), np.uint8))
    asset = lambda p: SimpleNamespace(path=str(p), asset_type="sequence", get_frame_files=lambda: names)  # noqa: E731
    clip = SimpleNamespace(name="c", root_path=str(root), input_asset=asset(inp), mask_asset=asset(msk),
                           transition_to=lambda s: None)
    frames = [(n, np.zeros((4, 6, 3), np.uint8)) for n in names]

    class Svc(GuidedPipelinesMixin):
        _device = "cuda"
        _gpu_lock = threading.Lock()
        _get_videomama_pipeline = lambda self: object()  # noqa: E731
        _selected_sequence_files = lambda self, c: names  # noqa: E731
        _load_named_sequence_frames = lambda self, *a, **k: frames  # noqa: E731

    mod = types.ModuleType("VideoMaMaInferenceModule.inference")
    mod.run_inference = lambda *a, **k: iter(chunks)
    pkg = types.ModuleType("VideoMaMaInferenceModule")
    pkg.inference = mod
    return Svc(), clip, root, names, {"VideoMaMaInferenceModule": pkg, "VideoMaMaInferenceModule.inference": mod}


def test_videomama_sink_pixels_failure_and_cancel(tmp_path, monkeypatch):
    from backend.errors import JobCancelledError, WriteFailureError
    rgb = np.zeros((4, 6, 3), np.float32)
    rgb[..., 0] = 1.0
    rgb[..., 1] = 0.5
    chunks = [[rgb, rgb], [rgb]]
    svc, clip, root, names, mods = _videomama(tmp_path, chunks)
    [monkeypatch.setitem(sys.modules, k, v) for k, v in mods.items()]
    svc.run_videomama(clip, chunk_size=2)
    for n in names:
        got = _read(root / "AlphaHint" / n)
        want = cv2.cvtColor((np.clip(rgb, 0, 1) * 255).astype(np.uint8), cv2.COLOR_RGB2BGR)
        assert np.array_equal(got, want)
    # write failure -> WriteFailureError, nothing claimed
    svc2, clip2, root2, names2, mods2 = _videomama(tmp_path / "f", chunks)
    [monkeypatch.setitem(sys.modules, k, v) for k, v in mods2.items()]
    _fail_images(monkeypatch, names2[1])
    with pytest.raises(WriteFailureError):
        svc2.run_videomama(clip2, chunk_size=2)
    # cancel: stops, no later chunk written
    monkeypatch.undo()
    svc3, clip3, root3, names3, mods3 = _videomama(tmp_path / "c", chunks)
    [monkeypatch.setitem(sys.modules, k, v) for k, v in mods3.items()]
    job = SimpleNamespace(is_cancelled=False)

    def gen(*a, **k):
        yield chunks[0]
        job.is_cancelled = True
        yield chunks[1]
    mods3["VideoMaMaInferenceModule.inference"].run_inference = gen
    with pytest.raises(JobCancelledError):
        svc3.run_videomama(clip3, job=job, chunk_size=2)
    assert not (root3 / "AlphaHint" / names3[2]).exists()


# ---- task_job: chroma, SAM, promotion, cleanup -----------------------------------------------------------------

def _job(kind="matanyone2", publish="VideoMamaMaskHint", cleanup=None, params=None):
    p = {"mask_cleanup": cleanup, **(params or {})}
    return SimpleNamespace(job_id="j", kind=kind, publish_dir=publish, params=p)


def test_cleanup_staged_matches_cleanup_mask(tmp_path):
    from backend.service.task_job import _cleanup_staged, cleanup_mask, stage_cleanup
    rng = np.random.default_rng(1)
    stems = ["é1", "é2", "e3"]
    originals = {}
    for s in stems:
        m = (rng.random((40, 50)) > 0.6).astype(np.uint8) * 255
        m[10:30, 10:30] = 255
        m[18:22, 18:22] = 0  # a hole
        originals[s] = m
        cv2.imwrite(str(tmp_path / f"{s}.png"), m) if s == "e3" else None
    from backend.frame_io import imwrite_unicode
    for s in stems:
        assert imwrite_unicode(str(tmp_path / f"{s}.png"), originals[s])
    ctx = SimpleNamespace(stems=stems)
    cleanup = {"grow": 2, "fill_holes": True, "feather": 1.5}
    job = _job(kind="refiner", publish="AlphaHint", cleanup=cleanup)
    _cleanup_staged(job, ctx, str(tmp_path), threading.Event())
    c = stage_cleanup("refiner", "AlphaHint", cleanup)
    for s in stems:
        assert np.array_equal(_read(tmp_path / f"{s}.png"), cleanup_mask(originals[s], c))


def test_cleanup_neutral_leaves_bytes_untouched(tmp_path):
    from backend.service.task_job import _cleanup_staged
    p = tmp_path / "a.png"
    cv2.imwrite(str(p), np.full((4, 4), 7, np.uint8))
    before = p.read_bytes()
    neutral = {"grow": 0, "fill_holes": False, "feather": 0.0}
    _cleanup_staged(_job(cleanup=neutral), SimpleNamespace(stems=["a"]), str(tmp_path), threading.Event())
    assert p.read_bytes() == before


def test_cleanup_cancel_and_write_failure(tmp_path, monkeypatch):
    from backend.errors import CorridorKeyError, JobCancelledError
    from backend.service.task_job import _cleanup_staged
    for s in ("a", "b"):
        cv2.imwrite(str(tmp_path / f"{s}.png"), np.full((6, 6), 255, np.uint8))
    cleanup = {"grow": 1, "fill_holes": False, "feather": 0.0}
    mk = lambda: _job(kind="refiner", publish="AlphaHint", cleanup=cleanup)  # noqa: E731
    ev = threading.Event()
    ev.set()
    with pytest.raises(JobCancelledError):
        _cleanup_staged(mk(), SimpleNamespace(stems=["a", "b"]), str(tmp_path), ev)
    _fail_images(monkeypatch, "b.png")
    with pytest.raises(CorridorKeyError, match="cleaned masks"):
        _cleanup_staged(mk(), SimpleNamespace(stems=["a", "b"]), str(tmp_path), threading.Event())


def _view(tmp_path, n, h=6, w=8, color=(0, 255, 0)):
    src = tmp_path / "src"
    src.mkdir()
    files = []
    for i in range(n):
        f = f"é_{i:04d}.png"
        img = np.zeros((h, w, 3), np.uint8)
        img[..., :] = color[::-1]  # BGR on disk
        cv2.imwrite(str(src / f), img)
        files.append(f)
    root = tmp_path / "root"
    root.mkdir()
    view = SimpleNamespace(name="v", root_path=str(root),
                           input_asset=SimpleNamespace(path=str(src), get_frame_files=lambda: files))
    return view, files


_CHROMA = {"screen_color": None, "screen_type": "green", "strength": 1.0, "clip_black": 0.0, "clip_white": 1.0,
           "shrink_grow": 0, "edge_blur": 0.0}


def test_chroma_sink_pixels_stems_and_failure(tmp_path, monkeypatch):
    from CorridorKeyModule.core.chroma_key import chroma_key_matte
    from backend.errors import CorridorKeyError
    from backend.service.task_job import _chroma
    view, files = _view(tmp_path, 3)
    job = SimpleNamespace(clip_root=str(tmp_path), params=dict(_CHROMA))
    seen = []
    _chroma(job, SimpleNamespace(), view, threading.Event(), lambda *a: seen.append(a))
    out = os.path.join(view.root_path, "AlphaHint")
    assert sorted(os.listdir(out)) == [os.path.splitext(f)[0] + ".png" for f in files]
    rgb = np.zeros((6, 8, 3), np.uint8)
    rgb[..., 1] = 255
    want = chroma_key_matte(rgb, holdout_mask=None, screen_color=None, screen_type="green", strength=1.0,
                            clip_black=0.0, clip_white=1.0, shrink_grow=0, edge_blur=0.0)
    for f in files:
        assert np.array_equal(_read(os.path.join(out, os.path.splitext(f)[0] + ".png")), want)
    assert [s[1] for s in seen] == [1, 2, 3]
    view2, _ = _view(tmp_path / "b", 3) if (tmp_path / "b").mkdir() is None else (None, None)
    _fail_images(monkeypatch, "é_0001")
    with pytest.raises(CorridorKeyError, match="hint frames"):
        _chroma(job, SimpleNamespace(), view2, threading.Event(), lambda *a: None)


def test_chroma_cancel_raises_before_publish(tmp_path):
    from backend.errors import JobCancelledError
    from backend.service.task_job import _chroma
    view, _ = _view(tmp_path, 2)
    ev = threading.Event()
    ev.set()
    with pytest.raises(JobCancelledError):
        _chroma(SimpleNamespace(clip_root=str(tmp_path), params=dict(_CHROMA)), SimpleNamespace(), view, ev,
                lambda *a: None)


def test_promote_guide_pixels_stems_and_checks(tmp_path, monkeypatch):
    from backend.errors import CorridorKeyError
    from backend.frame_io import imwrite_unicode
    from backend.service.task_job import _promote_guide
    view, files = _view(tmp_path, 2)
    stems = [os.path.splitext(f)[0] for f in files]
    gdir = tmp_path / "guide"
    gdir.mkdir()
    masks = {}
    for i, s in enumerate(stems):
        m = np.full((6, 8), 1000 * (i + 1), np.uint16)  # 16-bit guide stays 16-bit
        imwrite_unicode(str(gdir / f"{s}.png"), m)
        masks[s] = str(gdir / f"{s}.png")
    ctx = SimpleNamespace(stems=stems, files=files, masks=masks)
    job = SimpleNamespace(job_id="j")
    staged = str(tmp_path / "staged")
    _promote_guide(job, ctx, view, staged, threading.Event())
    for i, s in enumerate(stems):
        got = _read(os.path.join(staged, f"{s}.png"))
        assert got.dtype == np.uint16 and (got == 1000 * (i + 1)).all()
    # size mismatch is still rejected, and a failed write is reported
    imwrite_unicode(masks[stems[1]], np.zeros((3, 3), np.uint8))
    with pytest.raises(CorridorKeyError, match="nothing was published"):
        _promote_guide(job, ctx, view, str(tmp_path / "s2"), threading.Event())
    imwrite_unicode(masks[stems[1]], np.zeros((6, 8), np.uint8))
    _fail_images(monkeypatch, stems[1])
    with pytest.raises(CorridorKeyError, match="staged hint frames"):
        _promote_guide(job, ctx, view, str(tmp_path / "s3"), threading.Event())


def test_decode_loop_keeps_order_cancel_and_read_error(tmp_path):
    from backend.errors import FrameReadError, JobCancelledError
    from backend.service.frame_ops import FrameOpsMixin
    files = []
    for i in range(9):
        f = f"f_{i}.png"
        cv2.imwrite(str(tmp_path / f), np.full((4, 4, 3), i * 10, np.uint8))
        files.append(f)
    asset = SimpleNamespace(path=str(tmp_path))
    svc = FrameOpsMixin()
    got = list(svc._iter_named_sequence_frames(asset, files, "c"))
    assert [n for n, _ in got] == files and [int(a[0, 0, 0]) for _, a in got] == [i * 10 for i in range(9)]
    job = SimpleNamespace(is_cancelled=False)
    it = svc._iter_named_sequence_frames(asset, files, "c", job=job)
    next(it)
    job.is_cancelled = True
    with pytest.raises(JobCancelledError):
        next(it)
    os.remove(tmp_path / "f_4.png")
    with pytest.raises(FrameReadError):
        list(svc._iter_named_sequence_frames(asset, files, "c"))
