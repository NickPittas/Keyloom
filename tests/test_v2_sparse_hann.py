"""Sparse keyframe hints in the vendored v2 Hann stage 1 (CPU fakes)."""
from __future__ import annotations

import pytest
import torch
from torch import nn

import CorridorKeyV2.corridorkey.runtime as rt
from tests.test_v2_reference_hint import H, W, T, _Reader, _Stage1, _Foundations

HQ = 0.9
CQ = 0.7


class _SparseReader:
    def __init__(self, idx, kw=None):
        self.idx = set(idx)

    def window_with_presence(self, t0, t1):
        win = torch.zeros(t1 - t0, 1, H, W)
        pres = []
        for j, t in enumerate(range(t0, t1)):
            has = t in self.idx
            if has:
                win[j] = 1.0
            pres.append(has)
        return win, pres

    def window(self, t0, t1):
        return torch.ones(t1 - t0, 1, H, W)

    def close(self):
        pass


@pytest.fixture(autouse=True)
def _no_cuda(monkeypatch):
    monkeypatch.setattr(torch.cuda, "synchronize", lambda *a, **k: None)
    monkeypatch.setattr(torch.cuda, "empty_cache", lambda: None)


def _stream(tmp_path, monkeypatch, indices, carry=False, hint=None, k=0,
            path=None):
    path = path or str(tmp_path)
    seen = []

    def fake(p, meta, **kw):
        seen.append(kw)
        return _SparseReader(indices) if "frame_indices" in kw else _SparseReader(range(T))

    monkeypatch.setattr(rt, "make_hint_reader", fake)
    model = nn.Module()
    model.stage1 = _Stage1()
    model.foundations = _Foundations()
    extra = {} if indices is None else {"hint_frame_indices": indices}
    rt.hann_stage1_stream(
        model, "x", {"num_frames": T, "shape_hw": (H, W)}, reader_cls=_Reader,
        reader_kind="video", chunk=4, stride=2, baton_path=tmp_path / "b.f16",
        initial_hint=hint, initial_hint_quality=0.8, initial_hint_index=k,
        full_hint_video_path=path, full_hint_quality=HQ, rvm_path=None,
        foundation_paths=None, carry_hint_from_aux=carry,
        carry_hint_quality=CQ, on_baton_frames=lambda f: None, **extra)
    return model.stage1.calls, seen


def test_sparse_slots_only(tmp_path, monkeypatch):
    calls, seen = _stream(tmp_path, monkeypatch, (3, 7))
    assert seen[0]["frame_indices"] == (3, 7)
    for t0, (h, q) in zip(rt.window_starts(T, 4, 2), calls):
        for j in range(q.shape[1]):
            if t0 + j in (3, 7):
                assert float(q[0, j]) == pytest.approx(HQ, abs=0.01)
                assert float(h[0, j].float().mean()) == 1.0
            elif bool((q == -1).all()):
                continue
            else:
                assert float(q[0, j]) == 0 and float(h[0, j].abs().sum()) == 0


def test_no_keyframe_window_is_minus_one(tmp_path, monkeypatch):
    calls, _ = _stream(tmp_path, monkeypatch, (9,))
    for t0, (h, q) in zip(rt.window_starts(T, 4, 2), calls):
        if t0 + q.shape[1] <= 9:
            assert bool((q == -1).all())
        else:
            assert float(q[0, 9 - t0]) == pytest.approx(HQ, abs=0.01)


def test_carry_fills_gaps_not_keyframes(tmp_path, monkeypatch):
    calls, _ = _stream(tmp_path, monkeypatch, (1,), carry=True)
    # window 0 has no carry; later overlapping windows carry earlier frames.
    starts = rt.window_starts(T, 4, 2)
    saw_carry = False
    for t0, (h, q) in zip(starts, calls):
        for j in range(q.shape[1]):
            if t0 + j == 1:
                assert float(q[0, j]) == pytest.approx(HQ, abs=0.01)
            elif float(q[0, j]) > 0:
                assert float(q[0, j]) == pytest.approx(CQ, abs=0.01)
                saw_carry = True
    assert saw_carry


def test_initial_overrides_carry(tmp_path, monkeypatch):
    calls, _ = _stream(tmp_path, monkeypatch, (1,), carry=True,
                       hint=torch.ones(1, H, W), k=5)
    for t0, (h, q) in zip(rt.window_starts(T, 4, 2), calls):
        if t0 <= 5 < t0 + q.shape[1]:
            assert float(q[0, 5 - t0]) == pytest.approx(0.8, abs=0.01)


def test_dense_call_has_no_frame_indices(tmp_path, monkeypatch):
    calls, seen = _stream(tmp_path, monkeypatch, None)
    assert seen == [{}]
    assert all(float(q.float().min()) == pytest.approx(HQ, abs=0.01)
               for _, q in calls)


def test_no_hint_unchanged(tmp_path, monkeypatch):
    monkeypatch.setattr(rt, "make_hint_reader", lambda p, m, **kw: None)
    model = nn.Module()
    model.stage1 = _Stage1()
    model.foundations = _Foundations()
    rt.hann_stage1_stream(
        model, "x", {"num_frames": T, "shape_hw": (H, W)}, reader_cls=_Reader,
        reader_kind="video", chunk=4, stride=2, baton_path=tmp_path / "b.f16",
        initial_hint=None, initial_hint_quality=0.8,
        full_hint_video_path=None, full_hint_quality=HQ, rvm_path=None,
        foundation_paths=None, carry_hint_from_aux=False,
        carry_hint_quality=CQ, on_baton_frames=lambda f: None)
    assert all(bool((q == -1).all()) for _, q in model.stage1.calls)


def test_run_inference_forwarding_settings_and_validation(monkeypatch, tmp_path):
    meta = {"num_frames": T, "shape_hw": (H, W), "output_hw": (H, W),
            "native_fps": 24.0}
    monkeypatch.setattr(rt, "probe_video_meta", lambda *a, **kw: dict(meta))
    created = []

    class _W:
        def __init__(self, *a, **kw):
            created.append(1)

        def release(self):
            pass

    monkeypatch.setattr(rt, "ThreadedVideoWriter", _W)
    hd = tmp_path / "h"
    hd.mkdir()

    def run(cfg, hint):
        rt.run_inference(nn.Module(), str(tmp_path / "c.mp4"),
                         str(tmp_path / "out"), cfg, rt.WriterConfig(),
                         initial_hint_path=None, hint_video_path=hint)

    for idx, hint in [((1,), None), ((), str(hd)), ((1,), str(tmp_path / "v.mp4"))]:
        cfg = rt.InferenceConfig(temp_dir=tmp_path / "tmp", hint_frame_indices=idx)
        with pytest.raises(ValueError):
            run(cfg, hint)
    assert not created

    class Stop(Exception):
        pass

    got = {}

    def fake_stream(*a, **kw):
        got.update(kw)
        raise Stop

    monkeypatch.setattr(rt, "hann_stage1_stream", fake_stream)
    monkeypatch.setattr(rt, "_phase1_setup", lambda *a, **k: None)
    m = nn.Module()
    m.foundations = nn.Module()
    m.foundations.rvm = None
    m.stage2 = nn.Linear(1, 1)
    cfg = rt.InferenceConfig(temp_dir=tmp_path / "tmp", hint_frame_indices=(2, 4),
                             auto_vram=False, low_vram=False)
    monkeypatch.setattr(torch.nn.Module, "cuda", lambda self: self, raising=False)
    with pytest.raises(Stop):
        rt.run_inference(m, str(tmp_path / "c.mp4"), str(tmp_path / "out"), cfg,
                         rt.WriterConfig(), initial_hint_path=None,
                         hint_video_path=str(hd))
    assert got["hint_frame_indices"] == (2, 4)
    log = (tmp_path / "out" / "c_settings.txt").read_text()
    assert "Hint Frame Indices (0-based): [2, 4]" in log
