"""Tests for backend.hint_assets.clear_hint_assets."""
from __future__ import annotations

import json
import os

import pytest

from backend import ClipEntry, ClipState
from backend.clip_state import MASK_TRACK_MANIFEST
from backend.hint_assets import clear_hint_assets, has_hint_assets


def _touch(path, text="x"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def _clip(tmp_path, frames=3):
    root = str(tmp_path / "shot")
    for i in range(frames):
        _touch(os.path.join(root, "Frames", f"frame_{i:05d}.png"))
    clip = ClipEntry(name="shot", root_path=root)
    clip.find_assets()
    return clip


def _preserved(root):
    _touch(os.path.join(root, "Output", "FG", "frame_00000.exr"))
    _touch(os.path.join(root, "Output", ".corridorkey_manifest.json"), "{}")
    _touch(os.path.join(root, "annotations.json"), "{}")
    _touch(os.path.join(root, "holdout_strokes.json"), "{}")
    _touch(os.path.join(root, "clip.json"), "{}")
    _touch(os.path.join(root, "Source", "in.mp4"))
    return [
        os.path.join(root, "Frames", "frame_00000.png"),
        os.path.join(root, "Output", "FG", "frame_00000.exr"),
        os.path.join(root, "Output", ".corridorkey_manifest.json"),
        os.path.join(root, "annotations.json"),
        os.path.join(root, "holdout_strokes.json"),
        os.path.join(root, "clip.json"),
        os.path.join(root, "Source", "in.mp4"),
    ]


def test_clears_dirs_videos_and_manifest_preserves_rest(tmp_path):
    clip = _clip(tmp_path)
    root = clip.root_path
    keep = _preserved(root)
    _touch(os.path.join(root, "AlphaHint", "a.png"))
    _touch(os.path.join(root, "AlphaHint.mp4"))
    _touch(os.path.join(root, "VideoMamaMaskHint", "m.png"))
    _touch(os.path.join(root, "VideoMamaMaskHint.mov"))
    _touch(os.path.join(root, MASK_TRACK_MANIFEST), json.dumps({"source": "sam2"}))
    removed = clear_hint_assets(clip)
    assert len(removed) == 5
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert not os.path.exists(os.path.join(root, "AlphaHint.mp4"))
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint.mov"))
    assert not os.path.exists(os.path.join(root, MASK_TRACK_MANIFEST))
    assert all(os.path.exists(p) for p in keep)


def test_selective_flags(tmp_path):
    clip = _clip(tmp_path)
    root = clip.root_path
    _touch(os.path.join(root, "AlphaHint", "a.png"))
    _touch(os.path.join(root, "VideoMamaMaskHint", "m.png"))
    _touch(os.path.join(root, MASK_TRACK_MANIFEST), "{}")
    clear_hint_assets(clip, clear_masks=False)
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert os.path.exists(os.path.join(root, "VideoMamaMaskHint", "m.png"))
    assert os.path.exists(os.path.join(root, MASK_TRACK_MANIFEST))
    clear_hint_assets(clip, clear_alpha=False)
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert not os.path.exists(os.path.join(root, MASK_TRACK_MANIFEST))


def test_partial_and_empty_dirs_and_idempotent(tmp_path):
    clip = _clip(tmp_path)
    root = clip.root_path
    os.makedirs(os.path.join(root, "AlphaHint"))
    _touch(os.path.join(root, "VideoMamaMaskHint", "m_00000.png"))
    assert len(clear_hint_assets(clip)) == 2
    assert clear_hint_assets(clip) == []


def test_similar_names_and_non_video_untouched(tmp_path):
    clip = _clip(tmp_path)
    root = clip.root_path
    others = [
        os.path.join(root, "AlphaHint_backup", "a.png"),
        os.path.join(root, "notes_AlphaHint.txt"),
        os.path.join(root, "AlphaHint.txt"),
        os.path.join(root, "VideoMamaMaskHint_old", "m.png"),
        os.path.join(root, "VideoMamaMaskHint.json"),
    ]
    for p in others:
        _touch(p)
    assert clear_hint_assets(clip) == []
    assert all(os.path.exists(p) for p in others)


def test_symlinked_dir_unlinks_link_only(tmp_path):
    clip = _clip(tmp_path)
    outside = tmp_path / "outside"
    _touch(str(outside / "keep.png"))
    os.symlink(str(outside), os.path.join(clip.root_path, "AlphaHint"))
    removed = clear_hint_assets(clip)
    assert len(removed) == 1
    assert not os.path.lexists(os.path.join(clip.root_path, "AlphaHint"))
    assert (outside / "keep.png").exists()


def test_symlinked_video_unlinks_link_only(tmp_path):
    clip = _clip(tmp_path)
    target = tmp_path / "ext.mp4"
    target.write_text("x")
    os.symlink(str(target), os.path.join(clip.root_path, "AlphaHint.mp4"))
    clear_hint_assets(clip)
    assert target.exists()
    assert not os.path.lexists(os.path.join(clip.root_path, "AlphaHint.mp4"))


def test_symlink_inside_hint_dir_not_followed(tmp_path):
    clip = _clip(tmp_path)
    outside = tmp_path / "outside"
    _touch(str(outside / "keep.png"))
    os.makedirs(os.path.join(clip.root_path, "AlphaHint"))
    os.symlink(str(outside), os.path.join(clip.root_path, "AlphaHint", "lnk"))
    clear_hint_assets(clip)
    assert (outside / "keep.png").exists()


def test_failure_raises_oserror(tmp_path, monkeypatch):
    clip = _clip(tmp_path)
    _touch(os.path.join(clip.root_path, "AlphaHint", "a.png"))

    def boom(path):
        raise PermissionError("denied")

    monkeypatch.setattr("backend.hint_assets.shutil.rmtree", boom)
    with pytest.raises(OSError):
        clear_hint_assets(clip)
    assert os.path.exists(os.path.join(clip.root_path, "AlphaHint", "a.png"))


def test_rescan_after_clear_goes_raw(tmp_path):
    clip = _clip(tmp_path)
    for i in range(3):
        _touch(os.path.join(clip.root_path, "AlphaHint", f"frame_{i:05d}.png"))
    clip.find_assets()
    assert clip.state == ClipState.READY
    clear_hint_assets(clip)
    clip.find_assets()
    assert clip.alpha_asset is None and clip.state == ClipState.RAW


def test_masked_to_raw_transition():
    clip = ClipEntry(name="s", root_path="/tmp/s")
    clip.state = ClipState.MASKED
    clip.transition_to(ClipState.RAW)
    assert clip.state == ClipState.RAW
    clip.state = ClipState.READY
    clip.transition_to(ClipState.RAW)
    assert clip.state == ClipState.RAW


def test_complete_outputs_resolve_complete_after_clear(tmp_path):
    clip = _clip(tmp_path)
    root = clip.root_path
    for i in range(3):
        _touch(os.path.join(root, "Output", "FG", f"frame_{i:05d}.png"))
    _touch(
        os.path.join(root, "Output", ".corridorkey_manifest.json"),
        json.dumps({
            "engine": "corridorkey-v2", "complete": True, "frame_range": None,
            "enabled_outputs": ["fg"], "formats": {"fg": "png"},
        }),
    )
    _touch(os.path.join(root, "AlphaHint", "frame_00000.png"))
    clear_hint_assets(clip)
    clip.find_assets()
    assert os.path.exists(os.path.join(root, "Output", ".corridorkey_manifest.json"))
    assert clip.state == ClipState.COMPLETE


def test_has_hint_assets(tmp_path):
    from backend.clip_state import ClipEntry
    from backend.hint_assets import clear_hint_assets, has_hint_assets, has_hint_assets
    root = tmp_path / "clip"
    (root / "Frames").mkdir(parents=True)
    clip = ClipEntry(name="clip", root_path=str(root))
    assert not has_hint_assets(clip)
    (root / "AlphaHint").mkdir()
    assert has_hint_assets(clip)
    clear_hint_assets(clip)
    assert not has_hint_assets(clip)


def test_keyframe_sidecar_opt_in_and_counted(tmp_path):
    from backend.hint_keyframes import KEYFRAMES_FILENAME
    clip = _clip(tmp_path)
    root = clip.root_path
    side = os.path.join(root, KEYFRAMES_FILENAME)
    _touch(side, "{}")
    clear_hint_assets(clip)
    assert os.path.exists(side)  # default never touches marks
    assert has_hint_assets(clip)  # sidecar counts
    clear_hint_assets(clip, clear_alpha=False, clear_masks=False, clear_keyframes=True)
    assert not os.path.exists(side)
    assert not has_hint_assets(clip)
