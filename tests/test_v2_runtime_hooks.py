"""on_progress / check_cancel hooks and cleanup in the vendored v2 runtime.

No CUDA, no weights: heavy pieces are replaced with tiny CPU fakes.
"""
from __future__ import annotations

from typing import ClassVar

import numpy as np
import pytest
import torch
from torch import nn

import CorridorKeyV2.corridorkey.runtime as rt

H = W = 64
T = 6


class _Cancel(Exception):
    pass


class _Recorder:
    def __init__(self):
        self.events = []

    def __call__(self, phase, cur, total):
        self.events.append((phase, cur, total))

    def phases(self, name):
        return [e for e in self.events if e[0] == name]


class _CancelAfter:
    """check_cancel that raises on the n-th call (1-based)."""

    def __init__(self, n):
        self.n = n
        self.calls = 0

    def __call__(self):
        self.calls += 1
        if self.calls >= self.n:
            raise _Cancel()


def _assert_progress(events, phase, total):
    ev = [e for e in events if e[0] == phase]
    assert ev[0] == (phase, 0, total)
    curs = [e[1] for e in ev]
    assert curs == sorted(curs) and curs[-1] == total
    assert all(e[1] <= e[2] == total for e in ev)


@pytest.fixture(autouse=True)
def _no_cuda(monkeypatch):
    monkeypatch.setattr(torch.cuda, "synchronize", lambda *a, **k: None)
    monkeypatch.setattr(torch.cuda, "empty_cache", lambda: None)


def _fake_chunks(monkeypatch, tc=2):
    def gen(source, meta, chunk_size, need_model):
        for t0 in range(0, T, tc):
            n = min(tc, T - t0)
            yield t0, None, torch.rand(n, 3, H, W)

    monkeypatch.setattr(rt, "iter_video_frame_chunks", gen)


class _FakeRvm(nn.Module):
    embed_dim = 3

    def __init__(self):
        super().__init__()
        self.p = nn.Parameter(torch.zeros(1))

    def forward(self, rgb, state=None):
        return torch.zeros(1, rgb.shape[1], self.embed_dim, 1, 1), state


class _FakeFoundations(nn.Module):
    def __init__(self, with_rvm=False):
        super().__init__()
        self.p = nn.Parameter(torch.zeros(1))
        self.rvm = _FakeRvm() if with_rvm else None
        self.register_buffer("rvm_mean", torch.zeros(3))
        self.register_buffer("rvm_std", torch.ones(3))

    def forward(self, rgb_cradio, rgb_moge):
        n = rgb_cradio.shape[0]
        return {"moge": torch.zeros(n, 2, 1, 1), "cradio": torch.zeros(n, 2, 1, 1)}


class _FakeStage1(nn.Module):
    d_16 = 2
    cradio_dim = 2
    moge_dim = 2

    def __init__(self):
        super().__init__()
        self.p = nn.Parameter(torch.zeros(1))

    def forward(self, rgb, hint, moge_feat, cradio_feat=None, rvm_feat=None,
                t_stride=1.0, hint_quality=None):
        Tc = rgb.shape[1]
        return {"baton": torch.ones(1, Tc, self.d_16, H // 16, W // 16),
                "aux": torch.zeros(1, Tc, 1, H // 16, W // 16)}


def _model(with_rvm=False):
    m = nn.Module()
    m.foundations = _FakeFoundations(with_rvm)
    m.stage1 = _FakeStage1()
    return m


META = {"num_frames": T, "shape_hw": (H, W)}


def test_rvm_prepass_progress_and_cancel(monkeypatch, tmp_path):
    _fake_chunks(monkeypatch)
    rec = _Recorder()
    rt.precompute_rvm_to_memmap(_model(True), "x", META, tmp_path / "r.f16",
                                chunk=2, reader_kind="video", on_progress=rec)
    _assert_progress(rec.events, "rvm", T)
    assert {e[0] for e in rec.events} == {"rvm"}

    rec2 = _Recorder()
    cancel = _CancelAfter(2)
    with pytest.raises(_Cancel):
        rt.precompute_rvm_to_memmap(
            _model(True), "x", META, tmp_path / "r2.f16", chunk=2,
            reader_kind="video", on_progress=rec2, check_cancel=cancel)
    assert cancel.calls == 2
    assert rec2.phases("rvm")[-1][1] < T


def test_vfm_prepass_progress_and_cancel(monkeypatch, tmp_path):
    _fake_chunks(monkeypatch)
    rec = _Recorder()
    paths = rt.precompute_foundations_to_memmap(
        _model(), "x", META, tmp_path / "f", chunk=2, reader_kind="video",
        on_progress=rec)
    assert "moge" in paths
    _assert_progress(rec.events, "vfm", T)

    cancel = _CancelAfter(1)
    rec2 = _Recorder()
    with pytest.raises(_Cancel):
        rt.precompute_foundations_to_memmap(
            _model(), "x", META, tmp_path / "g", chunk=2, reader_kind="video",
            on_progress=rec2, check_cancel=cancel)
    assert rec2.phases("vfm") == [("vfm", 0, T)]


class _FakeReader:
    instances: ClassVar[list] = []

    def __init__(self, source, meta):
        self.closed = False
        _FakeReader.instances.append(self)

    def window(self, t0, t1):
        n = t1 - t0
        x = torch.zeros(n, 3, H, W)
        return x, x, x

    def close(self):
        self.closed = True


def _hann(model, tmp_path, **kw):
    _FakeReader.instances.clear()
    frames = []
    rt.hann_stage1_stream(
        model, "x", META, reader_cls=_FakeReader, reader_kind="video",
        chunk=4, stride=2, baton_path=tmp_path / "b.f16", initial_hint=None,
        initial_hint_quality=0.9, full_hint_video_path=None,
        full_hint_quality=0.9, rvm_path=None, foundation_paths=None,
        carry_hint_from_aux=False, carry_hint_quality=0.9,
        on_baton_frames=frames.extend, **kw)
    return frames


def test_hann_progress(tmp_path):
    rec = _Recorder()
    cancel = _CancelAfter(10**6)
    frames = _hann(_model(), tmp_path, on_progress=rec, check_cancel=cancel)
    n = len(rt.window_starts(T, 4, 2))
    _assert_progress(rec.events, "stage1", n)
    assert len(frames) == T
    assert cancel.calls == n
    assert _FakeReader.instances[0].closed


def test_hann_cancel_stops_and_closes_reader(tmp_path):
    rec = _Recorder()
    with pytest.raises(_Cancel):
        _hann(_model(), tmp_path, on_progress=rec, check_cancel=_CancelAfter(2))
    assert rec.phases("stage1") == [("stage1", 0, 2), ("stage1", 1, 2)]
    assert _FakeReader.instances[0].closed


# ---------------------------------------------------------------- run_inference

class _FakeWriter:
    instances: ClassVar[list] = []
    fail_release: ClassVar[tuple] = ()

    def __init__(self, path, *a, **k):
        self.path = path
        self.written = 0
        self.released = False
        _FakeWriter.instances.append(self)

    def write_many(self, frames):
        self.written += len(frames)

    def release(self):
        self.released = True
        if any(f in self.path for f in _FakeWriter.fail_release):
            raise RuntimeError("release boom")


class _Stage2(nn.Module):
    def __init__(self):
        super().__init__()
        self.p = nn.Parameter(torch.zeros(1))

    def forward(self, rgb, baton):
        return torch.zeros(1, rgb.shape[1], 4, H, W)


@pytest.fixture
def env(monkeypatch, tmp_path):
    _FakeWriter.instances.clear()
    _FakeWriter.fail_release = ()
    _FakeReader.instances.clear()
    meta = {"num_frames": T, "shape_hw": (H, W), "output_hw": (H, W),
            "native_fps": 24.0}
    model = _model()
    model.stage2 = _Stage2()
    state = {"calls": []}

    monkeypatch.setattr(rt, "probe_video_meta", lambda *a, **k: dict(meta))
    monkeypatch.setattr(rt, "ThreadedVideoWriter", _FakeWriter)
    monkeypatch.setattr(rt, "SequentialVideoWindowReader", _FakeReader)
    monkeypatch.setattr(rt, "make_checker", lambda h, w: np.zeros((3, h, w), np.float32))
    monkeypatch.setattr(rt, "_phase1_setup", lambda *a, **k: None)
    monkeypatch.setattr(rt, "_phase2_setup", lambda *a, **k: None)
    monkeypatch.setattr(rt, "configure_low_vram_inference", lambda *a, **k: None)
    monkeypatch.setattr(
        rt, "_render_predictions_torch",
        lambda meta, outputs, checker, logits: {
            n: np.zeros((logits.shape[0], H, W, 3), np.uint16) for n in outputs})

    def fake_foundations(model, src, meta, prefix, *, chunk, reader_kind,
                         on_progress=None, check_cancel=None):
        state["calls"].append("vfm")
        if check_cancel:
            check_cancel()
        if on_progress:
            on_progress("vfm", 0, T)
            on_progress("vfm", T, T)
        return {}

    def fake_hann(model, src, meta, *, baton_path, on_baton_frames,
                  on_progress=None, check_cancel=None, **kw):
        state["calls"].append("hann")
        if check_cancel:
            check_cancel()
        if on_progress:
            on_progress("stage1", 0, 1)
        if on_baton_frames is None:
            np.memmap(baton_path, dtype=np.float16, mode="w+",
                      shape=(T, 2, H // 16, W // 16)).flush()
        else:
            on_baton_frames([
                (t, np.zeros((2, H // 16, W // 16), np.float16),
                 torch.zeros(3, H, W)) for t in range(T)])
        if on_progress:
            on_progress("stage1", 1, 1)

    monkeypatch.setattr(rt, "precompute_foundations_to_memmap", fake_foundations)
    monkeypatch.setattr(rt, "hann_stage1_stream", fake_hann)
    model.foundations.rvm = None
    state["model"] = model
    state["tmp"] = tmp_path
    return state


def _run(env, low_vram=False, **kw):
    tmp = env["tmp"]
    cfg = rt.InferenceConfig(low_vram=low_vram, temp_dir=tmp / "tmp",
                             stage2_batch=2, outputs={"alpha", "fg"})
    return rt.run_inference(
        env["model"], str(tmp / "clip.mp4"), str(tmp / "out"), cfg,
        rt.WriterConfig(),
        initial_hint_path=None, hint_video_path=None, **kw)


def test_defaults_none_still_work(env):
    _run(env)
    assert [w.written for w in _FakeWriter.instances] == [T, T]
    assert all(w.released for w in _FakeWriter.instances)


@pytest.mark.parametrize("low_vram", [False, True])
def test_stage2_progress(env, low_vram):
    rec = _Recorder()
    _run(env, low_vram=low_vram, on_progress=rec,
         check_cancel=_CancelAfter(10**6))
    _assert_progress(rec.events, "stage2", T)
    assert ("vfm" in {e[0] for e in rec.events}) == low_vram
    assert (env["tmp"] / "tmp").exists()
    assert not list((env["tmp"] / "tmp").iterdir())


@pytest.mark.parametrize("low_vram", [False, True])
def test_cancel_in_stage2_propagates_and_cleans(env, low_vram):
    # Let the prepass/stage-1 fakes pass, cancel on the 2nd stage-2 batch.
    n_before = 2 if low_vram else 1  # vfm+hann vs hann
    cancel = _CancelAfter(n_before + 2)
    rec = _Recorder()
    with pytest.raises(_Cancel):
        _run(env, low_vram=low_vram, on_progress=rec, check_cancel=cancel)
    s2 = rec.phases("stage2")
    assert s2[0] == ("stage2", 0, T) and s2[-1][1] < T
    assert all(w.released for w in _FakeWriter.instances)
    assert not list((env["tmp"] / "tmp").iterdir())


def test_cancel_in_prepass_propagates(env):
    with pytest.raises(_Cancel):
        _run(env, low_vram=True, check_cancel=_CancelAfter(1))
    assert env["calls"] == ["vfm"]
    assert all(w.released for w in _FakeWriter.instances)


def test_release_failure_does_not_block_other_cleanup(env):
    _FakeWriter.fail_release = ("_alpha",)
    leftover = env["tmp"] / "tmp" / "clip_baton.f16"
    with pytest.raises(RuntimeError, match="release boom"):
        _run(env, low_vram=True)  # body ok -> cleanup error surfaces
    assert all(w.released for w in _FakeWriter.instances)
    assert not leftover.exists()


def test_release_failure_does_not_mask_primary(env):
    _FakeWriter.fail_release = ("_alpha", "_fg")
    with pytest.raises(_Cancel):
        _run(env, low_vram=True, check_cancel=_CancelAfter(1))
    assert all(w.released for w in _FakeWriter.instances)
    assert not list((env["tmp"] / "tmp").iterdir())
