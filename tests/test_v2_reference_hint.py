"""Indexed reference-hint injection in the vendored v2 runtime (CPU fakes)."""
from __future__ import annotations

import pytest
import torch
from torch import nn

import CorridorKeyV2.corridorkey.runtime as rt

H = W = 64
T = 10
Q = 0.8


class _Reader:
    def __init__(self, source, meta):
        pass

    def window(self, t0, t1):
        x = torch.zeros(t1 - t0, 3, H, W)
        return x, x, x

    def close(self):
        pass


class _Stage1(nn.Module):
    d_16 = 2
    cradio_dim = 0
    moge_dim = 0

    def __init__(self):
        super().__init__()
        self.p = nn.Parameter(torch.zeros(1))
        self.calls = []

    def forward(self, rgb, hint, moge_feat, cradio_feat=None, rvm_feat=None,
                t_stride=1.0, hint_quality=None):
        self.calls.append((hint.detach().clone(), hint_quality.detach().clone()))
        Tc = rgb.shape[1]
        return {"baton": torch.ones(1, Tc, self.d_16, H // 16, W // 16),
                "aux": torch.zeros(1, Tc, 1, H // 16, W // 16)}


class _Foundations(nn.Module):
    def forward(self, rgb_cradio, rgb_moge):
        n = rgb_cradio.shape[0]
        return {"moge": torch.zeros(n, 2, 1, 1), "cradio": torch.zeros(n, 2, 1, 1)}


@pytest.fixture(autouse=True)
def _no_cuda(monkeypatch):
    monkeypatch.setattr(torch.cuda, "synchronize", lambda *a, **k: None)
    monkeypatch.setattr(torch.cuda, "empty_cache", lambda: None)


def _stream(tmp_path, hint, **kw):
    model = nn.Module()
    model.stage1 = _Stage1()
    model.foundations = _Foundations()
    meta = {"num_frames": T, "shape_hw": (H, W)}
    rt.hann_stage1_stream(
        model, "x", meta, reader_cls=_Reader, reader_kind="video", chunk=4,
        stride=2, baton_path=tmp_path / "b.f16", initial_hint=hint,
        initial_hint_quality=Q, full_hint_video_path=None,
        full_hint_quality=0.9, rvm_path=None, foundation_paths=None,
        carry_hint_from_aux=False, carry_hint_quality=0.9,
        on_baton_frames=lambda f: None, **kw)
    return model.stage1.calls


def _check(calls, k):
    starts = rt.window_starts(T, 4, 2)
    assert len(calls) == len(starts)
    for t0, (hint, q) in zip(starts, calls):
        n = q.shape[1]
        if t0 <= k < t0 + n:
            j = k - t0
            expect = torch.zeros(n)
            expect[j] = Q
            assert torch.allclose(q[0].float(), expect.bfloat16().float())
            assert float(hint[0, j].float().mean()) == pytest.approx(1.0)
            others = [i for i in range(n) if i != j]
            assert float(hint[0, others].abs().sum()) == 0
        else:
            assert bool((q == -1).all())


@pytest.mark.parametrize("k", [0, 5, 9])
def test_index_injection(tmp_path, k):
    _check(_stream(tmp_path, torch.ones(1, H, W), initial_hint_index=k), k)


def test_default_is_frame_zero(tmp_path):
    calls = _stream(tmp_path, torch.ones(1, H, W))
    _check(calls, 0)


def test_black_reference_still_present(tmp_path):
    calls = _stream(tmp_path, torch.zeros(1, H, W), initial_hint_index=5)
    starts = rt.window_starts(T, 4, 2)
    for t0, (_, q) in zip(starts, calls):
        if t0 <= 5 < t0 + q.shape[1]:
            assert float(q[0, 5 - t0]) == pytest.approx(Q, abs=0.01)
            assert not bool((q == -1).all())
        else:
            assert bool((q == -1).all())


def test_overlapping_windows_all_get_reference(tmp_path):
    calls = _stream(tmp_path, torch.ones(1, H, W), initial_hint_index=5)
    starts = rt.window_starts(T, 4, 2)
    containing = [t0 for t0 in starts if t0 <= 5 < min(t0 + 4, T)]
    assert len(containing) >= 2
    n_with = sum(1 for _, q in calls if not bool((q == -1).all()))
    assert n_with == len(containing)


# ------------------------------------------------------------ run_inference

@pytest.mark.parametrize("k", [-1, T, 1.5, True, "2"])
def test_invalid_index_rejected_before_writers(monkeypatch, tmp_path, k):
    created = []
    meta = {"num_frames": T, "shape_hw": (H, W), "output_hw": (H, W),
            "native_fps": 24.0}
    monkeypatch.setattr(rt, "probe_video_meta", lambda *a, **kw: dict(meta))
    monkeypatch.setattr(rt, "ThreadedVideoWriter",
                        lambda *a, **kw: created.append(1))
    cfg = rt.InferenceConfig(temp_dir=tmp_path / "tmp", initial_hint_index=k)
    with pytest.raises(ValueError, match="initial_hint_index"):
        rt.run_inference(nn.Module(), str(tmp_path / "c.mp4"),
                         str(tmp_path / "out"), cfg, rt.WriterConfig(),
                         initial_hint_path=str(tmp_path / "h.png"),
                         hint_video_path=None)
    assert not created
