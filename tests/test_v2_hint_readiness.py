"""Mode-aware readiness / routing and keyframe-mark loading on ClipEntry."""
import copy
import json
import os

import pytest

from backend.clip_state import (
    ClipEntry, ClipState, PipelineRoute, can_run_inference, classify_pipeline_route,
)
from backend.hint_keyframes import KEYFRAMES_FILENAME, save_hint_keyframes

N = 6
STEMS = [f"shot_{i:04d}" for i in range(N)]


def _make(root, alpha=N, marks=None, manifest=False):
    (root / "Frames").mkdir()
    for s in STEMS:
        (root / "Frames" / f"{s}.png").write_bytes(b"x")
    if alpha:
        (root / "AlphaHint").mkdir()
        for s in STEMS[:alpha]:
            (root / "AlphaHint" / f"{s}.png").write_bytes(b"x")
    if marks is not None:
        save_hint_keyframes(str(root), marks, STEMS)
    clip = ClipEntry(name="c", root_path=str(root))
    clip.find_assets()
    return clip


def test_marks_loaded_in_natural_order(tmp_path):
    clip = _make(tmp_path, marks=[4, 1])
    assert clip.hint_keyframes == (1, 4) and clip.hint_keyframes_error is None


@pytest.mark.parametrize("alpha,state", [(0, ClipState.RAW), (3, ClipState.RAW)])
def test_partial_or_no_alpha(tmp_path, alpha, state):
    clip = _make(tmp_path, alpha=alpha, marks=[1])
    assert clip.state == state
    assert can_run_inference(clip, hint_mode="keyframes") is (alpha > 0)
    assert can_run_inference(clip, True) is False  # Full rejected on partial


def test_masked_partial_alpha_keyframes_allowed(tmp_path):
    clip = _make(tmp_path, alpha=2, marks=[0])
    (tmp_path / "VideoMamaMaskHint").mkdir()
    (tmp_path / "VideoMamaMaskHint" / "a.png").write_bytes(b"x")
    clip.find_assets()
    assert clip.state == ClipState.MASKED
    assert can_run_inference(clip, hint_mode="keyframes")
    assert not can_run_inference(clip, True, hint_mode="full")
    assert classify_pipeline_route(clip, hint_mode="keyframes") == PipelineRoute.INFERENCE_ONLY


def test_absent_empty_marks_rejected(tmp_path):
    clip = _make(tmp_path)
    assert clip.hint_keyframes == ()
    assert not can_run_inference(clip, hint_mode="keyframes")
    assert classify_pipeline_route(clip, hint_mode="keyframes") == PipelineRoute.SKIP


def test_stale_marks_rejected_not_rewritten(tmp_path):
    clip = _make(tmp_path, marks=[2])
    side = tmp_path / KEYFRAMES_FILENAME
    before = side.read_bytes()
    os.rename(tmp_path / "Frames" / "shot_0002.png", tmp_path / "Frames" / "renamed.png")
    clip.find_assets()
    assert clip.hint_keyframes == () and clip.hint_keyframes_error
    assert not can_run_inference(clip, hint_mode="keyframes")
    assert classify_pipeline_route(clip, hint_mode="keyframes") == PipelineRoute.SKIP
    assert side.read_bytes() == before


def test_corrupt_sidecar_diagnostic(tmp_path):
    clip = _make(tmp_path, marks=[1])
    (tmp_path / KEYFRAMES_FILENAME).write_text("{nope")
    clip.find_assets()
    assert clip.hint_keyframes == () and clip.hint_keyframes_error


def test_missing_sidecar_resets_marks(tmp_path):
    clip = _make(tmp_path, marks=[1])
    os.remove(tmp_path / KEYFRAMES_FILENAME)
    clip.find_assets()
    assert clip.hint_keyframes == () and clip.hint_keyframes_error is None


def test_none_mode_ignores_marks_and_error(tmp_path):
    clip = _make(tmp_path, alpha=0, marks=[1])
    clip.hint_keyframes_error = "boom"
    assert can_run_inference(clip, True, hint_mode="none")
    assert classify_pipeline_route(clip, hint_mode="none") == PipelineRoute.INFERENCE_ONLY


def test_legacy_bool_unchanged_and_full_mode_maps(tmp_path):
    clip = _make(tmp_path)  # READY
    assert can_run_inference(clip, True) and can_run_inference(clip, hint_mode="full")
    assert classify_pipeline_route(clip) == classify_pipeline_route(clip, hint_mode="full")
    assert classify_pipeline_route(clip, False) == PipelineRoute.INFERENCE_ONLY


def test_unknown_mode_raises(tmp_path):
    clip = _make(tmp_path)
    with pytest.raises(ValueError):
        can_run_inference(clip, hint_mode="bogus")
    with pytest.raises(ValueError):
        classify_pipeline_route(clip, hint_mode="bogus")


def test_error_state_rejected(tmp_path):
    clip = _make(tmp_path, marks=[1])
    clip.state = ClipState.ERROR
    assert not can_run_inference(clip, hint_mode="keyframes")
    assert classify_pipeline_route(clip, hint_mode="keyframes") == PipelineRoute.SKIP


def test_complete_persists_and_reloads(tmp_path):
    clip = _make(tmp_path, marks=[0, 3])
    out = tmp_path / "Output" / "FG"
    out.mkdir(parents=True)
    for s in STEMS:
        (out / f"{s}.exr").write_bytes(b"x")
    (tmp_path / "Output" / ".corridorkey_manifest.json").write_text(json.dumps({
        "engine": "corridorkey-v2", "complete": True, "frame_range": None,
        "enabled_outputs": ["fg"], "formats": {"fg": "exr"}}))
    clip.find_assets()
    state = clip.state
    assert state == ClipState.COMPLETE
    again = ClipEntry(name="c", root_path=str(tmp_path))
    again.find_assets()
    assert again.state == state and again.hint_keyframes == (0, 3)
    assert copy.deepcopy(again).hint_keyframes == (0, 3)
    assert can_run_inference(again, hint_mode="keyframes")


def test_sidecar_without_source_reports_diagnostic(tmp_path):
    (tmp_path / KEYFRAMES_FILENAME).write_text(json.dumps({"version": 1, "keyframes": []}))
    clip = ClipEntry(name="c", root_path=str(tmp_path))
    clip.input_asset = None
    clip._load_hint_keyframes()
    assert clip.hint_keyframes == () and clip.hint_keyframes_error


def test_video_stems_are_zero_padded(tmp_path):
    from backend.clip_state import ClipAsset
    clip = ClipEntry(name="c", root_path=str(tmp_path))
    asset = ClipAsset.__new__(ClipAsset)
    asset.path, asset.asset_type, asset.frame_count = "v.mp4", "video", 3
    clip.input_asset = asset
    assert clip._source_stems() == ["00000", "00001", "00002"]
    save_hint_keyframes(str(tmp_path), [2], ["00000", "00001", "00002"])
    clip._load_hint_keyframes()
    assert clip.hint_keyframes == (2,)
