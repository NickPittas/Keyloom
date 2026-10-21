"""Tests for backend.input_colour (bounded raw-EXR peak sampling)."""
from unittest import mock

import numpy as np
import pytest

from backend import input_colour
from backend.input_colour import InputColourSample, sample_input_colour


def _paths(n, tmp_path=None):
    return [f"/x/f{i:03d}.exr" for i in range(n)]


def _patch(images):
    """images: dict path->array|None (or callable). Returns patcher + calls."""
    calls = []

    def fake(path, flags=None):
        calls.append(path)
        v = images[path] if isinstance(images, dict) else images
        if isinstance(v, Exception):
            raise v
        return None if v is None else v.copy()

    return mock.patch.object(input_colour, "imread_unicode", fake), calls


def _img(val, alpha=None):
    a = np.full((2, 3, 3), val, np.float32)
    if alpha is not None:
        a = np.concatenate([a, np.full((2, 3, 1), alpha, np.float32)], axis=2)
    return a


@pytest.mark.parametrize("v", [0.93, 0.99, 1.0, 4.5, -0.5])
def test_peaks(v):
    p = _paths(1)
    ctx, _ = _patch({p[0]: _img(v)})
    with ctx:
        s = sample_input_colour(p)
    assert s.peak == pytest.approx(v)
    assert s.dimensions == (3, 2)
    assert s.frame_count == 1 and s.sampled_indices == (0,) and s.error is None


def test_alpha_ignored():
    p = _paths(1)
    ctx, _ = _patch({p[0]: _img(0.5, alpha=9.0)})
    with ctx:
        assert sample_input_colour(p).peak == pytest.approx(0.5)


def test_nan_inf_ignored_and_all_nonfinite():
    p = _paths(1)
    a = _img(0.7)
    a[0, 0, 0] = np.nan
    a[0, 1, 1] = np.inf
    ctx, _ = _patch({p[0]: a})
    with ctx:
        assert sample_input_colour(p).peak == pytest.approx(0.7)
    ctx, _ = _patch({p[0]: _img(np.nan)})
    with ctx:
        s = sample_input_colour(p)
    assert s.peak is None and s.error


def test_grayscale_counts():
    p = _paths(1)
    ctx, _ = _patch({p[0]: np.full((2, 2), 2.0, np.float32)})
    with ctx:
        assert sample_input_colour(p).peak == 2.0


def test_failed_decode_is_unavailable():
    p = _paths(3)
    imgs = {p[0]: _img(5.0), p[1]: None, p[2]: _img(0.1)}
    ctx, _ = _patch(imgs)
    with ctx:
        s = sample_input_colour(p)
    assert s.peak is None and s.error and s.sampled_indices == (0, 1, 2)


def test_decode_exception_is_unavailable():
    p = _paths(1)
    ctx, _ = _patch({p[0]: RuntimeError("boom")})
    with ctx:
        s = sample_input_colour(p)
    assert s.peak is None and "boom" in s.error


def test_only_first_middle_last_decoded():
    p = _paths(11)
    ctx, calls = _patch({q: _img(0.5) for q in p})
    with ctx:
        s = sample_input_colour(p)
    assert s.sampled_indices == (0, 5, 10)
    assert calls == [p[0], p[5], p[10]]
    assert s.frame_count == 11


@pytest.mark.parametrize("n,exp", [(1, (0,)), (2, (0, 1)), (3, (0, 1, 2))])
def test_short_clip_dedup(n, exp):
    p = _paths(n)
    ctx, calls = _patch({q: _img(0.5) for q in p})
    with ctx:
        s = sample_input_colour(p)
    assert s.sampled_indices == exp and len(calls) == len(exp)


def test_nonexr_empty_and_mixed_no_io():
    ctx, calls = _patch({})
    with ctx:
        for lst in ([], ["a.png"], ["a.mp4"], ["a.exr", "b.png"]):
            s = sample_input_colour(lst)
            assert s.peak is None and s.sampled_indices == ()
            assert s.frame_count == len(lst)
    assert calls == []


def test_real_unicode_path_and_inputs_unchanged(tmp_path):
    cv2 = pytest.importorskip("cv2")
    from backend.frame_io import imwrite_unicode
    p = str(tmp_path / "каρ_é.exr")
    arr = np.full((4, 4, 3), 2.5, np.float32)
    try:
        ok = imwrite_unicode(p, arr)
    except Exception:
        ok = False
    if not ok:
        pytest.skip("EXR writing unavailable")
    before = open(p, "rb").read()
    paths = [p]
    s = sample_input_colour(paths)
    assert s.peak == pytest.approx(2.5)
    assert paths == [p] and open(p, "rb").read() == before


def test_result_is_frozen():
    s = InputColourSample(None, (), None, 0)
    with pytest.raises(Exception):
        s.peak = 1.0
