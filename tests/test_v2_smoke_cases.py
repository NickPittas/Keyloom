"""CPU tests for scripts/v2_smoke.py case registration, validation and clip building."""
import importlib.util
import json
import sys
from pathlib import Path

import cv2
import numpy as np
import pytest

_SPEC = importlib.util.spec_from_file_location(
    "v2_smoke", Path(__file__).resolve().parent.parent / "scripts" / "v2_smoke.py")
smoke = importlib.util.module_from_spec(_SPEC)
sys.modules["v2_smoke"] = smoke
_SPEC.loader.exec_module(smoke)


def _make_src(tmp_path, n, name="src"):
    d = tmp_path / name
    d.mkdir()
    for i in range(n):
        cv2.imwrite(str(d / f"f{i:04d}.png"), np.full((4, 6, 3), i, np.uint8))
    return d


def _make_hints(tmp_path, n):
    d = tmp_path / "hints"
    d.mkdir()
    for i in range(n):
        cv2.imwrite(str(d / f"f{i:04d}.png"), np.full((4, 6), 200, np.uint8))
    return d


def test_cases_registered():
    for c in ("reference_mid", "keyframes_sparse", "manual_window"):
        assert c in smoke.CASES
    assert set(smoke.HINT_CASES) == {"hint_auto", "reference_mid", "keyframes_sparse"}
    assert "manual_window" not in smoke.HINT_CASES  # always runs
    assert smoke.hint_marks("reference_mid", 24) == (12,)
    assert smoke.hint_marks("keyframes_sparse", 24) == (3, 17)
    assert smoke.hint_marks("manual_window", 24) is None


def test_validate_cases():
    ok = smoke.validate_cases(smoke.CASES, 24, "h", available=24)
    assert ok == []
    assert any("unknown case" in e for e in smoke.validate_cases(["bogus"], 24, None))
    assert smoke.validate_cases(["nohint_auto"], 0, None)
    assert smoke.validate_cases(["keyframes_sparse"], 18, "h") == []
    assert smoke.validate_cases(["keyframes_sparse"], 17, "h")
    assert smoke.validate_cases(["reference_mid"], 1, "h")
    assert smoke.validate_cases(["manual_window"], 5, None, available=3)
    # hint cases are skipped (not validated) without --hint-src
    assert smoke.validate_cases(["keyframes_sparse"], 5, None) == []


def test_windows_cover_marks():
    assert smoke.check_marks_in_windows((12,), 24, 8, 4) == []
    assert smoke.check_marks_in_windows((3, 17), 24, 8, 4) == []
    assert smoke.check_marks_in_windows((3, 5), 24, 8, 4)  # all in one window


def test_build_clip_reference_physically_keeps_only_selected(tmp_path):
    src, hints = _make_src(tmp_path, 6), _make_hints(tmp_path, 6)
    clip = smoke.build_clip("ref", tmp_path / "work", src, hints, 6, True, (4, 6),
                            keep=(3,), marks=(3,))
    kept = sorted(p.name for p in (Path(clip.root_path) / "AlphaHint").iterdir())
    assert kept == ["f0003.png"]
    assert clip.hint_keyframes == (3,)
    assert clip.hint_keyframes_error is None
    assert all(p.is_symlink() for p in (Path(clip.root_path) / "Frames").iterdir())


def test_build_clip_sparse_deletes_unmarked_and_rebuild_is_clean(tmp_path):
    src, hints = _make_src(tmp_path, 20), _make_hints(tmp_path, 20)
    work = tmp_path / "work"
    stale = work / "sp" / "AlphaHint"
    stale.mkdir(parents=True)
    (stale / "f0000.png").write_bytes(b"stale")
    clip = smoke.build_clip("sp", work, src, hints, 20, True, (4, 6),
                            keep=(3, 17), marks=(3, 17))
    assert sorted(p.stem for p in (Path(clip.root_path) / "AlphaHint").iterdir()) == ["f0003", "f0017"]
    assert clip.hint_keyframes == (3, 17)
    side = json.loads((Path(clip.root_path) / ".corridorkey_hint_keyframes.json").read_text())
    assert [(k["index"], k["stem"]) for k in side["keyframes"]] == [(3, "f0003"), (17, "f0017")]


def test_build_clip_missing_hint_fails_clearly(tmp_path):
    src = _make_src(tmp_path, 4)
    (tmp_path / "nohints").mkdir()
    with pytest.raises(FileNotFoundError):
        smoke.build_clip("x", tmp_path / "work", src, tmp_path / "nohints", 4, True, (4, 6))


def test_check_manifest_exact_provenance():
    stems = [f"s{i}" for i in range(24)]
    good = {"params": {"hint_mode": "keyframes", "temporal_window": "8/4"},
            "source_stems": stems, "complete": True, "frame_range": None,
            "hint_keyframes": [3, 17]}
    assert smoke.check_manifest(good, "keyframes_sparse", 24, stems, (3, 17), "8/4") == []
    bad = dict(good, hint_keyframes=[3, 18])
    assert smoke.check_manifest(bad, "keyframes_sparse", 24, stems, (3, 17), "8/4")
    bad = dict(good, params={"hint_mode": "keyframes", "temporal_window": "auto"})
    assert smoke.check_manifest(bad, "keyframes_sparse", 24, stems, (3, 17), "8/4")
    manual = {"params": {"hint_mode": "none", "temporal_window": "8/4"},
              "source_stems": stems, "complete": True, "frame_range": None}
    assert smoke.check_manifest(manual, "manual_window", 24, stems, None, "8/4") == []
    assert smoke.check_manifest(dict(manual, hint_keyframes=[1]), "manual_window", 24, stems, None, "8/4")
    assert smoke.check_manifest(dict(manual, source_stems=stems[:5]), "manual_window", 24, stems, None, "8/4")


def test_matte_range_errors(tmp_path):
    ok, bad = tmp_path / "ok.png", tmp_path / "bad.exr"
    cv2.imwrite(str(ok), np.full((2, 2), 255, np.uint8))
    assert smoke.matte_range_errors([ok]) == []
    import os
    os.environ["OPENCV_IO_ENABLE_OPENEXR"] = "1"
    cv2.imwrite(str(bad), np.full((2, 2), 1.5, np.float32))
    assert smoke.matte_range_errors([bad]) or not bad.exists()
