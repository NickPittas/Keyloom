import pytest

from backend import v2_temporal as vt
from CorridorKeyV2.corridorkey import runtime as rt


def test_unknown_shape_manual_has_no_estimate():
    est = vt.estimate_temporal_window(None, None, 10, "24/12")
    assert est.peak_gib is None


def test_invalid_inputs():
    with pytest.raises(ValueError):
        vt.estimate_temporal_window(1920, 1080, 10, "7/3")
    with pytest.raises(ValueError):
        vt.estimate_temporal_window(1920, 1080, 10, "auto", "high")


def test_spy_calls_padding_and_order(monkeypatch):
    calls = []
    real = rt.estimate_peak_vram_gb

    def spy(mp, frames, low):
        calls.append((mp, frames, low))
        return real(mp, frames, low)

    monkeypatch.setattr(rt, "estimate_peak_vram_gb", spy)
    est = vt.estimate_temporal_window(1920, 1080, 100, "24/12", "auto", 100.0)
    # 1920x1080 -> pad to 1920x1152 (height 1080 -> 1152)
    assert calls == [(pytest.approx(1152 * 1920 / 1e6), 24, False)]
    assert (est.chunk, est.stride, est.low_vram) == (24, 12, False)
    assert est.required_gib == pytest.approx(
        est.peak_gib * rt._VRAM_MARGIN_SCALE + rt._VRAM_MARGIN_GB)
    # width/height not swapped
    calls.clear()
    vt.estimate_temporal_window(1080, 1920, 100, "8/4")
    assert calls[0][0] == pytest.approx(1152 * 1920 / 1e6)


def test_short_clip_and_resolutions():
    short = vt.estimate_temporal_window(3840, 2160, 5, "80/40", "low")
    full = vt.estimate_temporal_window(3840, 2160, 500, "80/40", "low")
    assert short.peak_gib < full.peak_gib
    hd = vt.estimate_temporal_window(1920, 1080, 500, "24/12", "low")
    assert hd.peak_gib < vt.estimate_temporal_window(3840, 2160, 500, "24/12", "low").peak_gib


def test_manual_normal_vs_low_and_exact_preset():
    n = vt.estimate_temporal_window(1920, 1080, 500, "8/4", "auto")
    lo = vt.estimate_temporal_window(1920, 1080, 500, "8/4", "low")
    assert (n.chunk, n.stride, n.low_vram) == (8, 4, False)
    assert (lo.chunk, lo.stride, lo.low_vram) == (8, 4, True)
    assert lo.peak_gib < n.peak_gib


def test_one_frame_normal_floor_caveat():
    n = vt.estimate_temporal_window(1920, 1080, 500, "1/1", "auto")
    eight = vt.estimate_temporal_window(1920, 1080, 500, "8/4", "auto")
    assert (n.chunk, n.stride) == (1, 1)
    assert n.peak_gib == pytest.approx(eight.peak_gib)
    assert n.caveat
    assert vt.estimate_temporal_window(1920, 1080, 500, "1/1", "low").caveat is None
    assert eight.caveat is None


def test_margin_boundary():
    est = vt.estimate_temporal_window(1920, 1080, 500, "24/12", "auto", 1000.0)
    req = est.required_gib
    assert not vt.estimate_temporal_window(1920, 1080, 500, "24/12", "auto", req).over_budget
    assert vt.estimate_temporal_window(1920, 1080, 500, "24/12", "auto", req - 0.01).over_budget


def test_unavailable_states():
    e = vt.estimate_temporal_window(1920, 1080, 500, "24/12", "auto", None)
    assert e.peak_gib is not None and not e.over_budget and e.budget_gib is None
    e = vt.estimate_temporal_window(None, 1080, 500, "24/12", "auto", 10.0)
    assert (e.peak_gib, e.required_gib) == (None, None) and not e.over_budget
    assert e.chunk == 24
    e = vt.estimate_temporal_window(1920, 1080, 0, "24/12")
    assert e.peak_gib is None
    for prof in ("auto", "low"):
        e = vt.estimate_temporal_window(1920, 1080, 500, "auto", prof, None)
        assert (e.chunk, e.stride, e.peak_gib, e.over_budget) == (None, None, None, False)
    e = vt.estimate_temporal_window(None, None, 500, "auto", "auto", 10.0)
    assert e.chunk is None


def test_query_budget_no_cuda(monkeypatch):
    import torch

    monkeypatch.setattr(torch.cuda, "is_available", lambda: False)
    assert vt.query_vram_budget_gib() is None
    monkeypatch.setattr(torch.cuda, "is_available", lambda: True)

    def boom():
        raise RuntimeError("cuda")

    monkeypatch.setattr(torch.cuda, "mem_get_info", boom)
    assert vt.query_vram_budget_gib() is None
    monkeypatch.setattr(torch.cuda, "mem_get_info", lambda: (2 * 2**30, 8 * 2**30))
    monkeypatch.setattr(torch.cuda, "memory_reserved", lambda: 2**30)
    assert vt.query_vram_budget_gib() == pytest.approx(3.0)


def test_auto_uses_engine_selector(monkeypatch):
    seen = []
    real = rt.select_vram_profile

    def spy(shape_hw, n, budget):
        seen.append((shape_hw, n, budget))
        return real(shape_hw, n, budget)

    monkeypatch.setattr(rt, "select_vram_profile", spy)
    e = vt.estimate_temporal_window(1920, 1080, 200, "auto", "auto", 24.0)
    assert seen == [((1152, 1920), 200, 24.0)]
    expect = real((1152, 1920), 200, 24.0)
    assert (e.chunk, e.stride, e.low_vram) == expect[:3]
    assert not e.over_budget
    # changing profile/clip changes the estimate
    small = vt.estimate_temporal_window(1920, 1080, 200, "auto", "auto", 8.0)
    assert small.chunk != e.chunk or small.low_vram != e.low_vram


def test_auto_low_policy():
    from backend.corridorkey_v2 import _select_low_vram_window

    e = vt.estimate_temporal_window(1920, 1080, 200, "auto", "low", 12.0)
    assert (e.chunk, e.stride) == _select_low_vram_window((1152, 1920), 200, 12.0)
    assert e.low_vram is True and not e.over_budget
    tiny = vt.estimate_temporal_window(3840, 2160, 200, "auto", "low", 0.5)
    assert (tiny.chunk, tiny.stride, tiny.low_vram) == (1, 1, True)
    assert tiny.over_budget


def test_auto_picks_what_fits_on_a_24gb_card_measured():
    # Measured on Endor (RTX 4090, 20.3 GiB available, 3200x1800 padded to 3200x1920, 2026-10-27):
    #   8 normal: ok, peak 10.6 GiB allocated / 12.5 GiB reserved;
    #   24 normal and 24 low: out of memory (over 17.7 GiB allocated / 20.8 GiB reserved).
    chunk, _, low, est = rt.select_vram_profile((1920, 3200), 98, 20.3)
    assert (chunk, low) == (8, False), "the largest window that really fits, on the GPU"
    assert est * rt._VRAM_MARGIN_SCALE + rt._VRAM_MARGIN_GB >= 12.5, "the requirement covers the measured reserve"
    # A 12 GiB card (about 11 GiB free) cannot hold 8 normal (12.5 GiB measured): 8 in low-VRAM mode.
    assert rt.select_vram_profile((1920, 3200), 98, 11.0)[:3] == (8, 4, True)
