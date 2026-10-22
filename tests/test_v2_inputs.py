"""Tests for backend.service.v2_inputs (ordinal input/hint preparation)."""
from __future__ import annotations

import os

import cv2
import numpy as np
import pytest

from backend.clip_state import ClipAsset, ClipEntry
from backend.service import v2_inputs
from backend.service.v2_inputs import (
    V2InputError,
    prepare_v2_inputs,
    validate_dense_hint_alignment,
)


def _rgb(seed=0, size=(6, 8)):
    rng = np.random.default_rng(seed)
    return rng.integers(0, 255, (*size, 3), dtype=np.uint8)


def _write(path, img):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    assert cv2.imwrite(str(path), img)


def _seq(tmp_path, names, sub="in", size=(6, 8), gray=False):
    d = tmp_path / sub
    for i, n in enumerate(names):
        img = _rgb(i, size)
        _write(d / n, img[..., 0] if gray else img)
    return str(d)


def _clip(tmp_path, in_dir, alpha_dir=None, alpha_type="sequence"):
    return ClipEntry(
        name="c", root_path=str(tmp_path),
        input_asset=ClipAsset(in_dir, "sequence"),
        alpha_asset=ClipAsset(alpha_dir, alpha_type) if alpha_dir else None)


def _prep(clip, tmp_path, hint=False, **kw):
    return prepare_v2_inputs(clip, use_alpha_hint=hint, staging_dir=tmp_path / "stage", **kw)


def test_direct_fast_path(tmp_path):
    d = _seq(tmp_path, [f"f_{i:04d}.png" for i in range(3)])
    p = _prep(_clip(tmp_path, d), tmp_path)
    assert p.input_path == d and not p.staged_input and p.hint_path is None
    assert p.source_stems == ["f_0000", "f_0001", "f_0002"] and p.frame_count == 3
    assert p.fps == 24.0
    assert not (tmp_path / "stage" / "input").exists()


def test_staging_when_natural_order_differs(tmp_path):
    d = _seq(tmp_path, ["frame_1.png", "frame_2.png", "frame_10.png"])
    p = _prep(_clip(tmp_path, d), tmp_path)
    assert p.staged_input and p.source_stems == ["frame_1", "frame_2", "frame_10"]
    staged = sorted(os.listdir(p.input_path))
    assert staged == ["000000.png", "000001.png", "000002.png"]
    last = cv2.imread(os.path.join(p.input_path, "000002.png"))
    assert np.array_equal(last, cv2.imread(os.path.join(d, "frame_10.png")))


def test_uppercase_extension(tmp_path):
    d = _seq(tmp_path, ["a.PNG", "b.PNG"])
    p = _prep(_clip(tmp_path, d), tmp_path)
    assert p.staged_input
    assert sorted(os.listdir(p.input_path)) == ["000000.png", "000001.png"]


def test_tiff16_to_png16(tmp_path):
    img = (np.arange(6 * 8 * 3, dtype=np.uint16).reshape(6, 8, 3) * 300)
    _write(tmp_path / "in" / "a.tif", img)
    p = _prep(_clip(tmp_path, str(tmp_path / "in")), tmp_path)
    out = cv2.imread(os.path.join(p.input_path, "000000.png"), cv2.IMREAD_UNCHANGED)
    assert out.dtype == np.uint16 and np.array_equal(out, img)


def test_float_tiff_to_exr(tmp_path):
    img = np.random.default_rng(1).uniform(-0.5, 4.0, (6, 8, 3)).astype(np.float32)
    img[0, 0] = (-0.25, 3.5, 7.0)
    _write(tmp_path / "in" / "a.tiff", img)
    p = _prep(_clip(tmp_path, str(tmp_path / "in")), tmp_path)
    out = cv2.imread(os.path.join(p.input_path, "000000.exr"), cv2.IMREAD_UNCHANGED)
    ref = cv2.imread(str(tmp_path / "in" / "a.tiff"), cv2.IMREAD_UNCHANGED)  # what the TIFF holds
    assert out.dtype == np.float32 and np.array_equal(out, ref)
    assert out.max() > 1 and out.min() < 0


def test_bmp(tmp_path):
    d = _seq(tmp_path, ["a.bmp", "b.bmp"])
    p = _prep(_clip(tmp_path, d), tmp_path)
    assert sorted(os.listdir(p.input_path)) == ["000000.png", "000001.png"]
    assert np.array_equal(cv2.imread(os.path.join(p.input_path, "000001.png")),
                          cv2.imread(os.path.join(d, "b.bmp")))


def test_mixed_exr_and_png_errors(tmp_path):
    _write(tmp_path / "in" / "a.exr", np.zeros((6, 8, 3), np.float32))
    _write(tmp_path / "in" / "b.bmp", _rgb())
    with pytest.raises(V2InputError, match="mixes EXR"):
        _prep(_clip(tmp_path, str(tmp_path / "in")), tmp_path)
    assert not (tmp_path / "stage" / "input").exists()


def test_duplicate_stems_error(tmp_path):
    _write(tmp_path / "in" / "a.png", _rgb())
    _write(tmp_path / "in" / "a.exr", np.zeros((6, 8, 3), np.float32))
    with pytest.raises(V2InputError, match="duplicate"):
        _prep(_clip(tmp_path, str(tmp_path / "in")), tmp_path)


def test_unicode_dir(tmp_path):
    d = _seq(tmp_path, ["a.bmp", "b.bmp"], sub="клип_日本")
    p = _prep(_clip(tmp_path, d), tmp_path)
    assert len(os.listdir(p.input_path)) == 2


def test_cleanup_on_failure(tmp_path, monkeypatch):
    d = _seq(tmp_path, ["a.bmp", "b.bmp"])
    calls = []

    def boom(src, base):
        calls.append(src)
        if len(calls) == 2:
            raise RuntimeError("boom")
        return base + ".png"

    monkeypatch.setattr(v2_inputs, "_convert", boom)
    with pytest.raises(RuntimeError):
        _prep(_clip(tmp_path, d), tmp_path)
    assert not (tmp_path / "stage" / "input").exists()


def test_cancel_stops_and_cleans(tmp_path):
    d = _seq(tmp_path, ["frame_1.png", "frame_2.png", "frame_10.png"])
    n = []

    def cancel():
        n.append(1)
        if len(n) == 2:
            raise KeyboardInterrupt

    with pytest.raises(KeyboardInterrupt):
        _prep(_clip(tmp_path, d), tmp_path, check_cancel=cancel)
    assert len(n) == 2 and not (tmp_path / "stage" / "input").exists()


def _video(path, n=5, fps=10.0, size=(16, 16)):
    for codec in ("mp4v", "MJPG"):
        p = str(path) if codec == "mp4v" else str(path.with_suffix(".avi"))
        w = cv2.VideoWriter(p, cv2.VideoWriter_fourcc(*codec), fps, size)
        if w.isOpened():
            for i in range(n):
                w.write(np.full((size[1], size[0], 3), i * 20, np.uint8))
            w.release()
            return p
    pytest.skip("no OpenCV video codec available")


def test_video_input_stems_and_fps(tmp_path):
    v = _video(tmp_path / "v.mp4")
    clip = ClipEntry(name="v", root_path=str(tmp_path), input_asset=ClipAsset(v, "video"))
    p = _prep(clip, tmp_path)
    assert p.input_path == v and not p.staged_input
    assert p.source_stems == [f"{i:05d}" for i in range(p.frame_count)] and p.frame_count == 5
    assert p.fps == pytest.approx(10.0, abs=0.5)


def test_video_fps_default(tmp_path, monkeypatch):
    v = _video(tmp_path / "v.mp4")
    clip = ClipEntry(name="v", root_path=str(tmp_path), input_asset=ClipAsset(v, "video"))
    monkeypatch.setattr(cv2.VideoCapture, "get", lambda self, prop: 0.0, raising=False)
    assert v2_inputs._video_fps(clip) == 24.0


NAMES = ["f_0000.png", "f_0001.png", "f_0002.png"]


def test_hints_off_ignores_alpha(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, ["junk.png"], sub="alpha", gray=True)
    assert _prep(_clip(tmp_path, d, a), tmp_path).hint_path is None


def test_hints_exact_stems_direct(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="alpha", gray=True)
    assert _prep(_clip(tmp_path, d, a), tmp_path, hint=True).hint_path == a


def test_hints_staged_when_natural_order(tmp_path):
    names = ["frame_1.png", "frame_2.png", "frame_10.png"]
    d = _seq(tmp_path, names)
    a = _seq(tmp_path, names, sub="alpha", gray=True)
    p = _prep(_clip(tmp_path, d, a), tmp_path, hint=True)
    assert p.hint_path != a
    assert sorted(os.listdir(p.hint_path)) == ["000000.png", "000001.png", "000002.png"]
    assert np.array_equal(cv2.imread(os.path.join(p.hint_path, "000002.png"), -1),
                          cv2.imread(os.path.join(a, "frame_10.png"), -1))


def test_hints_legacy_suffix_stem(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, [f"{n[:-4]}_alphaHint_{i:04d}.png" for i, n in enumerate(NAMES)],
             sub="alpha", gray=True)
    clip = _clip(tmp_path, d, a)
    assert validate_dense_hint_alignment(clip, [n[:-4] for n in NAMES]) == sorted(os.listdir(a))


def test_hints_empty_stem_ordinal(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, [f"_alphaHint_{i:05d}.png" for i in range(3)], sub="alpha", gray=True)
    got = validate_dense_hint_alignment(_clip(tmp_path, d, a), [n[:-4] for n in NAMES])
    assert got == [f"_alphaHint_{i:05d}.png" for i in range(3)]


def test_missing_hints_error_counts(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES[:1], sub="alpha", gray=True)
    with pytest.raises(V2InputError, match=r"covers 1 of 3 frames"):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True)


def test_extra_hint_error(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES + ["extra.png"], sub="alpha", gray=True)
    with pytest.raises(V2InputError, match="extra"):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True)


def test_hint_dimension_mismatch(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="alpha", size=(4, 4), gray=True)
    with pytest.raises(V2InputError, match="4x4"):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True)


def test_hint_later_frame_dimension_mismatch(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="alpha", gray=True)
    _write(tmp_path / "alpha" / NAMES[1], _rgb(9, (4, 4))[..., 0])
    with pytest.raises(V2InputError, match=NAMES[1]):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True)


def test_no_alpha_asset(tmp_path):
    d = _seq(tmp_path, NAMES)
    with pytest.raises(V2InputError, match="no AlphaHint"):
        _prep(_clip(tmp_path, d), tmp_path, hint=True)


def test_hint_video_count(tmp_path):
    d = _seq(tmp_path, NAMES)
    v = _video(tmp_path / "h.mp4", n=5)
    clip = _clip(tmp_path, d, v, alpha_type="video")
    with pytest.raises(V2InputError, match="5 frames"):
        _prep(clip, tmp_path, hint=True)


def test_hint_video_ok(tmp_path):
    d = _seq(tmp_path, NAMES)
    v = _video(tmp_path / "h.mp4", n=3)
    clip = _clip(tmp_path, d, v, alpha_type="video")
    assert _prep(clip, tmp_path, hint=True).hint_path == v


def test_sources_untouched(tmp_path):
    names = ["frame_1.png", "frame_10.bmp", "frame_2.png"]
    d = _seq(tmp_path, names)
    a = _seq(tmp_path, names, sub="alpha", gray=True)
    snap = {p: (p.stat().st_mtime_ns, p.read_bytes())
            for r in (d, a) for p in __import__("pathlib").Path(r).iterdir()}
    _prep(_clip(tmp_path, d, a), tmp_path, hint=True)
    for p, (m, b) in snap.items():
        assert p.stat().st_mtime_ns == m and p.read_bytes() == b


def _hashes(d):
    import hashlib
    return {n: hashlib.sha256(open(os.path.join(d, n), "rb").read()).hexdigest()
            for n in sorted(os.listdir(d))}


def test_gvm_sized_hint_staged_to_source_size(tmp_path):
    # 1920x1080 input vs GVM short-edge-1024 truncation 1820x1024 (scaled 1/10 here)
    names = ["a.png", "b.png"]
    d = _seq(tmp_path, names, size=(108, 192))
    a = _seq(tmp_path, names, sub="alpha", size=(102, 182), gray=True)
    before = _hashes(a)
    p = _prep(_clip(tmp_path, d, a), tmp_path, hint=True)
    assert p.hint_path == str(tmp_path / "stage" / "hint")
    assert sorted(os.listdir(p.hint_path)) == ["000000.png", "000001.png"]
    for f in os.listdir(p.hint_path):
        assert cv2.imread(os.path.join(p.hint_path, f), cv2.IMREAD_UNCHANGED).shape == (108, 192)
    assert _hashes(a) == before


def test_aspect_compatible_hint_mixed_sizes_stage_all(tmp_path):
    names = ["a.png", "b.png"]
    d = _seq(tmp_path, names, size=(9, 16))
    a = _seq(tmp_path, names, sub="alpha", size=(9, 16), gray=True)
    _write(tmp_path / "alpha" / "b.png", _rgb(3, (18, 32))[..., 0])
    p = _prep(_clip(tmp_path, d, a), tmp_path, hint=True)
    assert p.hint_path != str(tmp_path / "alpha")
    for f in ("000000.png", "000001.png"):
        assert cv2.imread(os.path.join(p.hint_path, f), cv2.IMREAD_UNCHANGED).shape == (9, 16)


def test_wrong_aspect_message(tmp_path):
    d = _seq(tmp_path, NAMES, size=(9, 16))
    a = _seq(tmp_path, NAMES, sub="alpha", size=(16, 16), gray=True)
    with pytest.raises(V2InputError, match="aspect ratio"):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True)


def test_resize_cancel_before_decode(tmp_path):
    d = _seq(tmp_path, ["a.png", "b.png"], size=(8, 16))
    a = _seq(tmp_path, ["a.png", "b.png"], sub="alpha", size=(4, 8), gray=True)
    n = []

    def cancel():
        n.append(1)
        if len(n) == 2:
            raise KeyboardInterrupt

    with pytest.raises(KeyboardInterrupt):
        _prep(_clip(tmp_path, d, a), tmp_path, hint=True, check_cancel=cancel)
