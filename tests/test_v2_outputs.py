"""Tests for CorridorKey 2.0 output publication (synthetic EXRs, no CUDA)."""
import json
import os
import warnings

import cv2
import numpy as np
import pytest

from backend.errors import CorridorKeyError
from backend.frame_io import _linear_to_srgb
from backend.service import InferenceParams, OutputConfig
from backend.service.frame_ops import FrameOpsMixin

STEMS = ["shot_1001", "shot_1002", "shot_1003"]
H = W = 4


class Svc(FrameOpsMixin):
    pass


def _exr(path, arr):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    assert cv2.imwrite(path, np.ascontiguousarray(arr, dtype=np.float32))


def make_runtime(root, n=3, fg=0.5, alpha=0.25, checker=0.5, cutout=(0.5, 0.5, 0.5, 0.5)):
    d = {
        "alpha": str(root / "c_alpha_exr"),
        "fg": str(root / "c_fg_exr"),
        "checker": str(root / "c_checker_comp_despill050_exr"),
        "cutout": str(root / "c_cutout_exr"),
    }
    for i in range(n):
        _exr(f"{d['alpha']}/alpha_{i:06d}.exr", np.full((H, W), alpha))
        _exr(f"{d['fg']}/fg_{i:06d}.exr", np.full((H, W, 3), fg))
        _exr(f"{d['checker']}/checker_comp_despill050_{i:06d}.exr", np.full((H, W, 3), checker))
        _exr(f"{d['cutout']}/cutout_{i:06d}.exr", np.broadcast_to(np.array(cutout), (H, W, 4)))
    return d


def make_dirs(root):
    dirs = {k: str(root / "out" / k.capitalize()) for k in ("fg", "matte", "comp", "processed")}
    dirs["root"] = str(root / "out")
    for v in dirs.values():
        os.makedirs(v, exist_ok=True)
    return dirs


def cfg(**kw):
    base = {k: "exr" for k in ("fg_format", "matte_format", "comp_format", "processed_format")}
    base.update(kw)
    return OutputConfig(**base)


def read(path):
    img = cv2.imread(path, cv2.IMREAD_UNCHANGED)
    assert img is not None, path
    return img


def publish(tmp_path, config, runtime=None, **kw):
    runtime = runtime or make_runtime(tmp_path)
    dirs = make_dirs(tmp_path)
    out = Svc()._publish_v2_outputs(runtime, dirs, STEMS, config, "clip", **kw)
    return runtime, dirs, out


def test_names_folders_and_enabled_only(tmp_path):
    config = cfg(matte_enabled=False, processed_enabled=False)
    _, dirs, out = publish(tmp_path, config)
    assert out == STEMS
    assert sorted(os.listdir(dirs["fg"])) == [f"{s}.exr" for s in STEMS]
    assert sorted(os.listdir(dirs["comp"])) == [f"{s}.exr" for s in STEMS]
    assert os.listdir(dirs["matte"]) == []
    assert os.listdir(dirs["processed"]) == []
    for _root, ds, _fs in os.walk(dirs["root"]):
        assert not any("_exr" in d for d in ds)


def test_all_products_default_formats(tmp_path):
    config = OutputConfig()
    _, dirs, _ = publish(tmp_path, config)
    assert os.path.isfile(os.path.join(dirs["comp"], "shot_1001.png"))
    assert os.path.isfile(os.path.join(dirs["processed"], "shot_1003.exr"))
    assert os.path.isfile(os.path.join(dirs["matte"], "shot_1002.exr"))


@pytest.mark.parametrize("value", [0.5, 2.5, -0.1])
def test_fg_comp_exr_preserve_values(tmp_path, value):
    runtime = make_runtime(tmp_path, fg=value, checker=value)
    _, dirs, _ = publish(tmp_path, cfg(), runtime)
    for key in ("fg", "comp"):
        img = read(os.path.join(dirs[key], "shot_1001.exr"))
        assert img.shape == (H, W, 3)
        np.testing.assert_allclose(img, value, rtol=1e-3, atol=1e-3)


def test_png_no_double_gamma(tmp_path):
    runtime = make_runtime(tmp_path, fg=0.214, checker=0.214)
    _, dirs, _ = publish(tmp_path, cfg(fg_format="png", comp_format="png"), runtime)
    for key in ("fg", "comp"):
        img = read(os.path.join(dirs[key], "shot_1001.png"))
        assert img.dtype == np.uint8
        assert abs(int(img[0, 0, 0]) - 127) <= 2


def test_png_negative_and_hdr_clamped(tmp_path):
    runtime = make_runtime(tmp_path, fg=-0.1, checker=2.5)
    _, dirs, _ = publish(tmp_path, cfg(fg_format="png", comp_format="png"), runtime)
    assert read(os.path.join(dirs["fg"], "shot_1001.png")).max() == 0
    assert read(os.path.join(dirs["comp"], "shot_1001.png")).min() == 255


def test_processed_exr_premultiplied_once(tmp_path):
    runtime = make_runtime(tmp_path, cutout=(0.8, 0.4, 0.2, 0.5))  # BGRA
    _, dirs, _ = publish(tmp_path, cfg(), runtime)
    img = read(os.path.join(dirs["processed"], "shot_1001.exr"))
    assert img.shape == (H, W, 4)
    np.testing.assert_allclose(img[0, 0], [0.4, 0.2, 0.1, 0.5], atol=1e-3)


def test_processed_png_straight_srgb_alpha_0_and_1(tmp_path):
    root = make_runtime(tmp_path, cutout=(0.214, 0.214, 0.214, 1.0))
    # alpha 0 pixel with non-zero colour in frame 0
    p = f"{root['cutout']}/cutout_000000.exr"
    arr = read(p)
    arr[0, 0] = [0.214, 0.214, 0.214, 0.0]
    arr[0, 1] = [0.214, 0.214, 0.214, 0.5]
    _exr(p, arr)
    with warnings.catch_warnings():
        warnings.simplefilter("error")
        _, dirs, _ = publish(tmp_path, cfg(processed_format="png"), root)
    img = read(os.path.join(dirs["processed"], "shot_1001.png"))
    assert img.shape == (H, W, 4) and img.dtype == np.uint8
    expect = int(_linear_to_srgb(np.float32(0.214)) * 255)
    assert abs(int(img[1, 1, 0]) - expect) <= 1 and img[1, 1, 3] == 255
    assert img[0, 0, 3] == 0
    assert abs(int(img[0, 1, 0]) - expect) <= 1  # straight, not premultiplied
    assert abs(int(img[0, 1, 3]) - 127) <= 1


def test_matte_values(tmp_path):
    runtime = make_runtime(tmp_path, alpha=0.25)
    _, dirs, _ = publish(tmp_path, cfg(matte_format="exr"), runtime)
    m = read(os.path.join(dirs["matte"], "shot_1001.exr"))
    assert m.ndim == 2
    np.testing.assert_allclose(m, 0.25, atol=1e-3)
    d2 = make_dirs(tmp_path / "b")
    Svc()._publish_v2_outputs(runtime, d2, STEMS, cfg(matte_format="png"), "clip")
    m = read(os.path.join(d2["matte"], "shot_1001.png"))
    assert m.ndim == 2 and abs(int(m[0, 0]) - 63) <= 1


@pytest.mark.parametrize("comp", ["dwab", "piz", "zip", "none"])
def test_exr_compression(tmp_path, comp):
    runtime = make_runtime(tmp_path, fg=0.5)
    _, dirs, _ = publish(tmp_path, cfg(exr_compression=comp), runtime)
    np.testing.assert_allclose(read(os.path.join(dirs["fg"], "shot_1002.exr")), 0.5, atol=1e-2)


def test_indices_and_skip(tmp_path):
    _, dirs, out = publish(tmp_path, cfg(), publish_indices=[1, 2], skip_stems={"shot_1003"})
    assert out == ["shot_1002"]
    assert os.listdir(dirs["fg"]) == ["shot_1002.exr"]


def test_on_frame_called(tmp_path):
    seen = []
    publish(tmp_path, cfg(), on_frame=lambda i, s: seen.append((i, s)))
    assert seen == list(enumerate(STEMS))


def test_cancel_leaves_no_tmp(tmp_path):
    calls = []

    def cancel():
        calls.append(1)
        if len(calls) == 2:
            raise ValueError("cancelled")

    runtime = make_runtime(tmp_path)
    dirs = make_dirs(tmp_path)
    with pytest.raises(ValueError):
        Svc()._publish_v2_outputs(runtime, dirs, STEMS, cfg(), "clip", check_cancel=cancel)
    assert os.listdir(dirs["fg"]) == ["shot_1001.exr"]
    for _r, _d, files in os.walk(dirs["root"]):
        assert not [f for f in files if "tmp" in f]


def test_missing_runtime_file_raises(tmp_path):
    runtime = make_runtime(tmp_path)
    os.remove(f"{runtime['fg']}/fg_000001.exr")
    dirs = make_dirs(tmp_path)
    with pytest.raises(CorridorKeyError, match="fg_000001"):
        Svc()._publish_v2_outputs(runtime, dirs, STEMS, cfg(), "clip")
    assert not [f for f in os.listdir(dirs["fg"]) if "tmp" in f]


def test_wrong_channels_raises(tmp_path):
    runtime = make_runtime(tmp_path)
    _exr(f"{runtime['cutout']}/cutout_000000.exr", np.zeros((H, W, 3)))
    with pytest.raises(CorridorKeyError, match="cutout_000000"):
        Svc()._publish_v2_outputs(runtime, make_dirs(tmp_path), STEMS, cfg(), "clip")


@pytest.mark.parametrize("bad", [np.nan, np.inf])
def test_nonfinite_raises(tmp_path, bad):
    runtime = make_runtime(tmp_path)
    arr = np.full((H, W, 3), 0.5, np.float32)
    arr[1, 1, 1] = bad
    _exr(f"{runtime['fg']}/fg_000000.exr", arr)
    dirs = make_dirs(tmp_path)
    with pytest.raises(CorridorKeyError, match="fg_000000"):
        Svc()._publish_v2_outputs(runtime, dirs, STEMS, cfg(), "clip")
    assert os.listdir(dirs["fg"]) == []


def test_required_products():
    assert Svc._v2_required_products(OutputConfig()) == ["fg", "alpha", "checker", "cutout"]
    c = OutputConfig(fg_enabled=False, comp_enabled=False)
    assert Svc._v2_required_products(c) == ["alpha", "cutout"]
    c = OutputConfig(fg_enabled=False, matte_enabled=False, comp_enabled=False,
                     processed_enabled=False)
    with pytest.raises(ValueError):
        Svc._v2_required_products(c)


def _manifest(root):
    with open(root / ".corridorkey_manifest.json") as f:
        return json.load(f)


def test_manifest_fields(tmp_path):
    params = InferenceParams()
    Svc()._write_manifest(str(tmp_path), OutputConfig(), params)
    m = _manifest(tmp_path)
    assert m["version"] == 2 and m["engine"] == "corridorkey-v2"
    assert m["source_stems"] is None and m["frame_range"] is None and m["complete"] is False
    assert m["alpha_convention"] == {"processed_exr": "premultiplied-linear",
                                     "processed_png": "straight-srgb"}
    Svc()._write_manifest(str(tmp_path), OutputConfig(), params, source_stems=STEMS,
                          frame_range=(0, 2), complete=True)
    m = _manifest(tmp_path)
    assert m["source_stems"] == STEMS and m["frame_range"] == [0, 2] and m["complete"] is True
    assert m["formats"]["processed"] == "exr" and m["params"] == params.to_dict()
    assert not (tmp_path / ".corridorkey_manifest.json.tmp").exists()


def test_manifest_write_failure_raises(tmp_path):
    with pytest.raises(OSError):
        Svc()._write_manifest(str(tmp_path / "missing"), OutputConfig(), InferenceParams())


def test_copy_v2_settings(tmp_path):
    src = tmp_path / "settings.txt"
    src.write_text("x")
    out = tmp_path / "o"
    out.mkdir()
    Svc()._copy_v2_settings(str(src), str(out))
    assert (out / "corridorkey_v2_settings.txt").read_text() == "x"
    out2 = tmp_path / "o2"
    out2.mkdir()
    Svc()._copy_v2_settings(str(tmp_path / "nope.txt"), str(out2))
    assert os.listdir(out2) == []


def test_half_overflow_clamped_with_single_warning(tmp_path):
    runtime = make_runtime(tmp_path, fg=70000.0)
    warns = []
    _, dirs, _ = publish(tmp_path, cfg(), runtime=runtime, on_warning=warns.append)
    img = read(os.path.join(dirs["fg"], f"{STEMS[0]}.exr"))
    assert np.isfinite(img).all() and img.max() <= 65504.0
    assert len(warns) == 1 and "half-float" in warns[0]
