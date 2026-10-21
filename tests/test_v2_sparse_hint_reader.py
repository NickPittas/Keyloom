"""Index-preserving sparse hint reader (real tiny PNG fixtures)."""
from __future__ import annotations

import cv2
import numpy as np
import pytest
import torch

import CorridorKeyV2.corridorkey.video_io as vio
from CorridorKeyV2.corridorkey.video_io import (
    SequentialHintFrameDirWindowReader, SequentialSparseHintFrameDirWindowReader,
    make_hint_reader)

N = 10
NATIVE = (6, 8)
OUT = (8, 8)  # padded 1 row top/bottom
PAD = (0, 0, 1, 1)


def meta(model_hw=NATIVE, **kw):
    m = {"num_frames": N, "native_hw": NATIVE, "model_native_hw": model_hw,
         "shape_hw": OUT, "pad": PAD}
    m.update(kw)
    return m


def put(d, idx, arr, name=None):
    p = d / (name or f"{idx:06d}.png")
    assert cv2.imwrite(str(p), arr)
    return p


def gray8(v):
    return np.full(NATIVE, v, np.uint8)


def make(tmp_path, indices=(0, 4, 9), **kw):
    for i in indices:
        put(tmp_path, i, gray8(10 + i))
    return make_hint_reader(str(tmp_path), meta(), frame_indices=list(indices))


def test_selected_0_mid_last_values_shapes(tmp_path):
    r = make(tmp_path)
    assert isinstance(r, SequentialSparseHintFrameDirWindowReader)
    x, p = r.window_with_presence(0, N)
    assert x.shape == (N, 1, *OUT) and x.dtype == torch.bfloat16
    assert p == [i in (0, 4, 9) for i in range(N)]
    for i in (0, 4, 9):
        exp = torch.tensor((10 + i) / 255.0).bfloat16()
        assert torch.allclose(x[i, 0, 1:-1].float(), exp.float())
    assert float(x[1].abs().sum()) == 0.0
    assert torch.equal(r.window(0, N), x)


def test_windows_empty_one_multiple(tmp_path):
    r = make(tmp_path)
    x, p = r.window_with_presence(1, 4)
    assert p == [False] * 3 and float(x.abs().sum()) == 0.0 and r.cache == {}
    x, p = r.window_with_presence(3, 5)
    assert p == [False, True] and x.shape[0] == 2
    x, p = r.window_with_presence(4, 10)
    assert p == [True, False, False, False, False, True]
    x, p = r.window_with_presence(5, 5)
    assert x.shape == (0, 1, *OUT) and p == []


def test_overlap_cache_release_and_no_unselected_reads(tmp_path, monkeypatch):
    r = make(tmp_path)
    reads = []
    real = vio.read_hint_image
    monkeypatch.setattr(vio, "read_hint_image",
                        lambda p: (reads.append(str(p)), real(p))[1])
    r.window_with_presence(0, 6)
    assert sorted(r.cache) == [0, 4]
    r.window_with_presence(4, 10)  # overlap: 4 cached, 0 released
    assert sorted(r.cache) == [4, 9]
    assert [p[-10:] for p in reads] == ["000000.png", "000004.png", "000009.png"]
    r.window_with_presence(5, 8)
    assert sorted(r.cache) == [9]
    r.window_with_presence(10, 10)
    assert r.cache == {}
    r.close()


@pytest.mark.parametrize("dtype,mx", [(np.uint8, 255), (np.uint16, 65535)])
def test_grayscale_normalization_and_black_present(tmp_path, dtype, mx):
    put(tmp_path, 2, np.full(NATIVE, mx, dtype))
    put(tmp_path, 3, np.zeros(NATIVE, dtype))
    put(tmp_path, 5, np.full(NATIVE, mx // 2, dtype))
    r = make_hint_reader(str(tmp_path), meta(), frame_indices=[2, 3, 5])
    x, p = r.window_with_presence(2, 6)
    assert p == [True, True, False, True]
    assert float(x[0, 0, 1:-1].float().min()) == 1.0
    assert float(x[1].abs().sum()) == 0.0 and p[1]  # black but present
    assert abs(float(x[3, 0, 3, 3]) - (mx // 2) / mx) < 5e-3


def test_rgba_alpha_wins_over_rgb(tmp_path):
    img = np.zeros((*NATIVE, 4), np.uint8)
    img[..., :3] = 255
    img[..., 3] = 51  # 0.2
    put(tmp_path, 1, img)
    r = make_hint_reader(str(tmp_path), meta(), frame_indices=[1])
    x, p = r.window_with_presence(0, 3)
    assert p == [False, True, False]
    assert abs(float(x[1, 0, 3, 3]) - 0.2) < 5e-3


def test_model_resize_and_shape_mismatch(tmp_path):
    put(tmp_path, 0, gray8(255))
    r = make_hint_reader(str(tmp_path), meta(model_hw=(4, 8),
                                             shape_hw=(6, 8), pad=(0, 0, 1, 1)),
                         frame_indices=[0])
    x, _ = r.window_with_presence(0, 1)
    assert x.shape == (1, 1, 6, 8)
    cv2.imwrite(str(tmp_path / "000000.png"), np.zeros((3, 3), np.uint8))
    r2 = make_hint_reader(str(tmp_path), meta(), frame_indices=[0])
    with pytest.raises(ValueError, match="does not match"):
        r2.window_with_presence(0, 1)


def test_pad_mismatch(tmp_path):
    put(tmp_path, 0, gray8(1))
    r = make_hint_reader(str(tmp_path), meta(pad=(0, 0, 0, 2)), frame_indices=[0])
    with pytest.raises(ValueError, match="pad"):
        r.window_with_presence(0, 1)


@pytest.mark.parametrize("bad", [[], [True], [1.0], ["1"], [1, 1], [-1], [N]])
def test_invalid_indices(tmp_path, bad):
    put(tmp_path, 1, gray8(1))
    with pytest.raises(ValueError):
        make_hint_reader(str(tmp_path), meta(), frame_indices=bad)


def test_invalid_files(tmp_path):
    put(tmp_path, 1, gray8(1))
    put(tmp_path, 2, gray8(1))

    def build(idx):
        return make_hint_reader(str(tmp_path), meta(), frame_indices=idx)

    with pytest.raises(ValueError, match="missing"):
        build([1, 2, 3])
    with pytest.raises(ValueError, match="not a selected"):
        build([1])
    (tmp_path / "000002.png").rename(tmp_path / "2.png")
    with pytest.raises(ValueError, match="duplicate"):
        put(tmp_path, 2, gray8(1))
        build([1, 2])
    (tmp_path / "2.png").unlink()
    (tmp_path / "000002.png").unlink()
    put(tmp_path, 12, gray8(1))
    with pytest.raises(ValueError, match="out of range"):
        build([1])
    (tmp_path / "000012.png").unlink()
    cv2.imwrite(str(tmp_path / "000002.jpg"), gray8(1))
    with pytest.raises(ValueError, match="numeric"):
        build([1])
    (tmp_path / "000002.jpg").unlink()
    (tmp_path / "abc.png").write_bytes(b"x")
    with pytest.raises(ValueError, match="numeric"):
        build([1])


def test_bad_path_and_video_rejected(tmp_path):
    with pytest.raises(ValueError, match="PNG directory"):
        make_hint_reader(str(tmp_path / "nope.mp4"), meta(), frame_indices=[1])
    f = tmp_path / "x.mp4"
    f.write_bytes(b"x")
    with pytest.raises(ValueError, match="PNG directory"):
        make_hint_reader(str(f), meta(), frame_indices=[1])


def test_failed_decode(tmp_path):
    (tmp_path / "000003.png").write_bytes(b"not a png")
    r = make_hint_reader(str(tmp_path), meta(), frame_indices=[3])
    assert r.window_with_presence(0, 3)[1] == [False] * 3
    with pytest.raises(FileNotFoundError):
        r.window_with_presence(3, 4)


def test_dense_directory_presence_all_true(tmp_path):
    for i in range(N):
        put(tmp_path, i, gray8(i))
    m = {"shape_hw": OUT, "model_native_hw": NATIVE, "num_frames": N}
    r = make_hint_reader(str(tmp_path), m)
    assert isinstance(r, SequentialHintFrameDirWindowReader)
    x, p = r.window_with_presence(2, 5)
    assert p == [True] * 3 and torch.equal(x, r.window(2, 5))


def test_dense_video_presence_all_true(tmp_path):
    path = str(tmp_path / "h.avi")
    w = cv2.VideoWriter(path, cv2.VideoWriter_fourcc(*"MJPG"), 10, (8, 6))
    for i in range(4):
        w.write(np.full((6, 8, 3), 40 * i, np.uint8))
    w.release()
    m = {"shape_hw": OUT, "model_native_hw": NATIVE, "num_frames": 4,
         "start_frame": 0}
    r = make_hint_reader(path, m)
    x, p = r.window_with_presence(0, 3)
    assert p == [True] * 3 and x.shape == (3, 1, *OUT)
    r.close()
