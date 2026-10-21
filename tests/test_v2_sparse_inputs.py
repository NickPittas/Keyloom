"""Keyframe (sparse) hint staging in backend.service.v2_inputs."""
from __future__ import annotations

import os

import cv2
import numpy as np
import pytest

from backend.clip_state import ClipAsset, ClipEntry
from backend.service.v2_inputs import V2InputError, prepare_v2_inputs

H, W = 6, 8


def _w(path, img):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    from backend.frame_io import imwrite_unicode
    assert imwrite_unicode(str(path), img)


def _clip(tmp_path, n=5, in_names=None, hints=None, marks=(), err=None, hint_dir="hint_src"):
    names = in_names or [f"f_{i:04d}.png" for i in range(n)]
    for nm in names:
        _w(tmp_path / "in" / nm, np.zeros((H, W, 3), np.uint8))
    for nm, img in (hints or {}).items():
        _w(tmp_path / hint_dir / nm, img)
    c = ClipEntry(name="c", root_path=str(tmp_path),
                  input_asset=ClipAsset(str(tmp_path / "in"), "sequence"),
                  alpha_asset=ClipAsset(str(tmp_path / hint_dir), "sequence") if hints else None)
    c.hint_keyframes = tuple(marks)
    c.hint_keyframes_error = err
    return c


def _prep(clip, tmp_path, **kw):
    return prepare_v2_inputs(clip, use_alpha_hint=False, hint_mode="keyframes",
                             staging_dir=tmp_path / "stage", **kw)


def _g(v, dt=np.uint8):
    return np.full((H, W), v, dt)


def _read(p):
    return cv2.imread(p, cv2.IMREAD_UNCHANGED)


def test_single_nonzero_reference(tmp_path):
    c = _clip(tmp_path, hints={f"f_{i:04d}.png": _g(i * 10) for i in range(5)}, marks=(3,))
    p = _prep(c, tmp_path)
    assert p.initial_hint_index == 3 and p.hint_path is None and p.hint_frame_indices is None
    assert p.initial_hint_path.endswith("hint/000003.png")
    assert sorted(os.listdir(tmp_path / "stage" / "hint")) == ["000003.png"]
    assert int(_read(p.initial_hint_path)[0, 0]) == 30
    assert p.frame_count == 5 and len(p.source_stems) == 5


def test_two_marks_absolute_missing_unmarked_and_extras(tmp_path):
    hints = {"f_0001.png": _g(10), "f_0004.png": _g(40), "f_0002.png": _g(1)}
    c = _clip(tmp_path, hints=hints, marks=(4, 1))
    # corrupt unmarked file: must not be decoded
    (tmp_path / "hint_src" / "f_0002.png").write_bytes(b"garbage")
    p = _prep(c, tmp_path)
    assert p.hint_frame_indices == (1, 4) and p.initial_hint_path is None
    assert p.hint_path == str(tmp_path / "stage" / "hint")
    assert sorted(os.listdir(p.hint_path)) == ["000001.png", "000004.png"]


def test_natural_vs_lexical_source_order(tmp_path):
    names = ["a_1.png", "a_2.png", "a_10.png"]
    c = _clip(tmp_path, in_names=names,
              hints={"a_10.png": _g(200), "a_1.png": _g(1)}, marks=(2,))
    p = _prep(c, tmp_path)
    assert p.initial_hint_index == 2 and p.staged_input
    assert int(_read(p.initial_hint_path)[0, 0]) == 200


def test_legacy_ordinal_names(tmp_path):
    c = _clip(tmp_path, hints={f"zz_alphaHint_{i:05d}.png": _g(i + 1) for i in range(5)},
              marks=(1, 3))
    p = _prep(c, tmp_path)
    assert int(_read(os.path.join(p.hint_path, "000003.png"))[0, 0]) == 4


def test_exact_stem_alphahint_suffix(tmp_path):
    c = _clip(tmp_path, hints={"f_0002_alphaHint_0002.png": _g(7)}, marks=(2,))
    assert int(_read(_prep(c, tmp_path).initial_hint_path)[0, 0]) == 7


def test_missing_selected_reports_index_and_stem(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1)}, marks=(1, 3))
    with pytest.raises(V2InputError, match=r"keyframe 3.*f_0003"):
        _prep(c, tmp_path)
    assert not (tmp_path / "stage" / "hint").exists()


def test_ambiguous_selected(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1), "f_0001.jpg": _g(2)}, marks=(1,))
    with pytest.raises(V2InputError, match=r"keyframe 1.*f_0001"):
        _prep(c, tmp_path)


def test_ambiguous_unselected_ignored(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1), "f_0002.png": _g(1),
                               "f_0002.jpg": _g(2)}, marks=(1,))
    assert _prep(c, tmp_path).initial_hint_index == 1


def test_dense_still_rejects_incomplete(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1)}, marks=(1,))
    with pytest.raises(V2InputError, match="covers 1 of 5"):
        prepare_v2_inputs(c, use_alpha_hint=False, hint_mode="full",
                          staging_dir=tmp_path / "stage")


def test_invalid_marks(tmp_path):
    h = {"f_0001.png": _g(1)}
    for kw in ({"marks": ()}, {"marks": (1,), "err": "bad sidecar"}, {"marks": (9,)},
               {"marks": (-1,)}):
        with pytest.raises(V2InputError):
            _prep(_clip(tmp_path, hints=h, **kw), tmp_path)


def test_dimension_mismatch(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": np.zeros((3, 3), np.uint8)}, marks=(1,))
    with pytest.raises(V2InputError, match="3x3"):
        _prep(c, tmp_path)
    assert not (tmp_path / "stage" / "hint").exists()


def test_decode_failure(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1)}, marks=(1,))
    (tmp_path / "hint_src" / "f_0001.png").write_bytes(b"garbage")
    with pytest.raises(V2InputError, match="Could not read"):
        _prep(c, tmp_path)


def test_uint16_rgba_rgb_float(tmp_path):
    u16 = np.full((H, W), 12345, np.uint16)
    rgba = np.zeros((H, W, 4), np.uint8)
    rgba[..., 3] = 77
    rgba[..., 0] = 5
    rgb = np.zeros((H, W, 3), np.uint8)
    rgb[..., 0] = 9  # channel 0 (engine semantics)
    rgb[..., 2] = 200
    c = _clip(tmp_path, hints={"f_0000.png": u16, "f_0001.png": rgba, "f_0002.png": rgb},
              marks=(0, 1, 2))
    p = _prep(c, tmp_path)
    a, b, d = (_read(os.path.join(p.hint_path, f"{i:06d}.png")) for i in range(3))
    assert a.dtype == np.uint16 and int(a[0, 0]) == 12345
    assert b.ndim == 2 and int(b[0, 0]) == 77
    assert d.ndim == 2 and int(d[0, 0]) == 9


def test_float_exr_to_uint16(tmp_path, monkeypatch):
    os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
    img = np.full((H, W), 0.5, np.float32)
    c = _clip(tmp_path, hints={"f_0001.exr": img}, marks=(1,))
    try:
        p = _prep(c, tmp_path)
    except V2InputError:
        pytest.skip("OpenEXR unavailable in this cv2 build")
    out = _read(p.initial_hint_path)
    assert out.dtype == np.uint16 and int(out[0, 0]) == round(0.5 * 65535)


def test_unicode_paths(tmp_path):
    root = tmp_path / "клип 日本"
    root.mkdir()
    c = _clip(root, hints={"f_0001.png": _g(3)}, marks=(1,))
    p = prepare_v2_inputs(c, use_alpha_hint=False, hint_mode="keyframes",
                          staging_dir=root / "stage")
    assert int(_read(p.initial_hint_path)[0, 0]) == 3


def test_cancel_cleans_partial(tmp_path):
    c = _clip(tmp_path, hints={f"f_{i:04d}.png": _g(i) for i in range(5)}, marks=(0, 1, 2))
    calls = []

    def cancel():
        calls.append(1)
        if len(calls) == 2:
            raise KeyboardInterrupt

    with pytest.raises(KeyboardInterrupt):
        _prep(c, tmp_path, check_cancel=cancel)
    assert not (tmp_path / "stage" / "hint").exists()


def test_hint_video_selected_frames(tmp_path):
    c = _clip(tmp_path, marks=(1, 3), hints={"x.png": _g(0)})
    vid = str(tmp_path / "h.avi")
    wr = cv2.VideoWriter(vid, cv2.VideoWriter_fourcc(*"MJPG"), 24, (W, H))
    if not wr.isOpened():
        pytest.skip("no video writer")
    for i in range(5):
        wr.write(np.full((H, W, 3), i * 50, np.uint8))
    wr.release()
    c.alpha_asset = ClipAsset(vid, "video")
    p = _prep(c, tmp_path)
    assert sorted(os.listdir(p.hint_path)) == ["000001.png", "000003.png"]
    assert abs(int(_read(os.path.join(p.hint_path, "000003.png"))[0, 0]) - 150) <= 8


def test_inout_not_rebased_and_none_untouched(tmp_path):
    c = _clip(tmp_path, hints={"f_0004.png": _g(4)}, marks=(4,))
    assert _prep(c, tmp_path).initial_hint_index == 4
    p = prepare_v2_inputs(c, use_alpha_hint=False, hint_mode="none",
                          staging_dir=tmp_path / "s2")
    assert (p.hint_path, p.initial_hint_path, p.hint_frame_indices) == (None, None, None)


def test_explicit_mode_wins_over_bool(tmp_path):
    c = _clip(tmp_path, hints={"f_0001.png": _g(1)}, marks=(1,))
    p = prepare_v2_inputs(c, use_alpha_hint=True, hint_mode="none",
                          staging_dir=tmp_path / "s")
    assert p.hint_path is None
    with pytest.raises(V2InputError, match="Unknown hint mode"):
        prepare_v2_inputs(c, use_alpha_hint=True, hint_mode="bogus",
                          staging_dir=tmp_path / "s")
