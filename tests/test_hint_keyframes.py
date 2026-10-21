import json
import os

import pytest

from backend import hint_keyframes as hk
from backend.hint_keyframes import (
    KEYFRAMES_FILENAME, HintKeyframesError, clear_hint_keyframes,
    load_hint_keyframes, save_hint_keyframes,
)

STEMS = [f"frame_{i:04d}" for i in range(20)]


def _write(root, data):
    (root / KEYFRAMES_FILENAME).write_text(
        data if isinstance(data, str) else json.dumps(data), encoding="utf-8")


def test_missing_returns_empty(tmp_path):
    assert load_hint_keyframes(str(tmp_path), STEMS) == ()


def test_roundtrip_sorted_first_mid_last(tmp_path):
    save_hint_keyframes(str(tmp_path), [19, 0, 10], STEMS)
    assert load_hint_keyframes(str(tmp_path), STEMS) == (0, 10, 19)
    data = json.loads((tmp_path / KEYFRAMES_FILENAME).read_text())
    assert data == {"version": 1, "keyframes": [
        {"index": 0, "stem": STEMS[0]}, {"index": 10, "stem": STEMS[10]},
        {"index": 19, "stem": STEMS[19]}]}


def test_unicode_stems(tmp_path):
    stems = ["café_é", "日本_1"]
    save_hint_keyframes(str(tmp_path), [1, 0], stems)
    assert load_hint_keyframes(str(tmp_path), stems) == (0, 1)


def test_empty_clears(tmp_path):
    save_hint_keyframes(str(tmp_path), [1], STEMS)
    save_hint_keyframes(str(tmp_path), [], STEMS)
    assert not (tmp_path / KEYFRAMES_FILENAME).exists()
    save_hint_keyframes(str(tmp_path), [], STEMS)


@pytest.mark.parametrize("data", [
    "not json", "[]", {"version": 1}, {"version": 2, "keyframes": []},
    {"version": True, "keyframes": []}, {"version": 1, "keyframes": {}},
    {"version": 1, "keyframes": [1]},
    {"version": 1, "keyframes": [{"index": "1", "stem": "frame_0001"}]},
    {"version": 1, "keyframes": [{"index": True, "stem": "frame_0001"}]},
    {"version": 1, "keyframes": [{"index": 1, "stem": 5}]},
    {"version": 1, "keyframes": [{"index": 1, "stem": "frame_0001", "x": 1}]},
    {"version": 1, "keyframes": [{"index": 1.0, "stem": "frame_0001"}]},
    {"version": 1, "keyframes": [{"index": -1, "stem": "x"}]},
    {"version": 1, "keyframes": [{"index": 20, "stem": "x"}]},
    {"version": 1, "keyframes": [{"index": 1, "stem": "frame_0001"}] * 2},
    {"version": 1, "keyframes": [{"index": 2, "stem": "frame_0002"},
                                 {"index": 1, "stem": "frame_0001"}]},
    {"version": 1, "keyframes": [{"index": 1, "stem": "other"}]},
])
def test_malformed_raises_and_is_not_rewritten(tmp_path, data):
    _write(tmp_path, data)
    before = (tmp_path / KEYFRAMES_FILENAME).read_bytes()
    with pytest.raises(HintKeyframesError):
        load_hint_keyframes(str(tmp_path), STEMS)
    assert (tmp_path / KEYFRAMES_FILENAME).read_bytes() == before


def test_stale_when_sources_change(tmp_path):
    save_hint_keyframes(str(tmp_path), [3], STEMS)
    with pytest.raises(HintKeyframesError):
        load_hint_keyframes(str(tmp_path), STEMS[:3])
    with pytest.raises(HintKeyframesError):
        load_hint_keyframes(str(tmp_path), ["a", "b", "c", "d"])


@pytest.mark.parametrize("bad", [[True], [-1], [20], [1, 1], ["1"], [1.0]])
def test_save_invalid_does_not_touch_prior(tmp_path, bad):
    save_hint_keyframes(str(tmp_path), [2], STEMS)
    before = (tmp_path / KEYFRAMES_FILENAME).read_bytes()
    with pytest.raises(HintKeyframesError):
        save_hint_keyframes(str(tmp_path), bad, STEMS)
    assert (tmp_path / KEYFRAMES_FILENAME).read_bytes() == before
    assert os.listdir(tmp_path) == [KEYFRAMES_FILENAME]


def test_write_failure_preserves_prior_and_cleans_tmp(tmp_path, monkeypatch):
    save_hint_keyframes(str(tmp_path), [2], STEMS)
    before = (tmp_path / KEYFRAMES_FILENAME).read_bytes()

    def boom(*a, **k):
        raise OSError("disk full")
    monkeypatch.setattr(hk.json, "dump", boom)
    with pytest.raises(OSError):
        save_hint_keyframes(str(tmp_path), [5], STEMS)
    assert (tmp_path / KEYFRAMES_FILENAME).read_bytes() == before
    assert os.listdir(tmp_path) == [KEYFRAMES_FILENAME]


def test_replace_failure_preserves_prior_and_cleans_tmp(tmp_path, monkeypatch):
    save_hint_keyframes(str(tmp_path), [2], STEMS)
    before = (tmp_path / KEYFRAMES_FILENAME).read_bytes()
    seen = {}

    def fail_replace(src, dst):
        seen["dir"] = os.path.dirname(src)
        raise OSError("nope")
    monkeypatch.setattr(hk.os, "replace", fail_replace)
    with pytest.raises(OSError):
        save_hint_keyframes(str(tmp_path), [5], STEMS)
    assert seen["dir"] == str(tmp_path)
    assert (tmp_path / KEYFRAMES_FILENAME).read_bytes() == before
    assert os.listdir(tmp_path) == [KEYFRAMES_FILENAME]


def test_unrelated_files_untouched(tmp_path):
    (tmp_path / "clip.json").write_text("{}")
    (tmp_path / "AlphaHint").mkdir()
    save_hint_keyframes(str(tmp_path), [1], STEMS)
    clear_hint_keyframes(str(tmp_path))
    assert (tmp_path / "clip.json").read_text() == "{}"
    assert (tmp_path / "AlphaHint").is_dir()
    assert not (tmp_path / KEYFRAMES_FILENAME).exists()


def test_roots_isolated(tmp_path):
    a, b = tmp_path / "a" / "shot", tmp_path / "b" / "shot"
    a.mkdir(parents=True)
    b.mkdir(parents=True)
    save_hint_keyframes(str(a), [4], STEMS)
    assert load_hint_keyframes(str(b), STEMS) == ()
    save_hint_keyframes(str(b), [7], STEMS)
    clear_hint_keyframes(str(a))
    assert load_hint_keyframes(str(b), STEMS) == (7,)


def test_symlink_sidecar_external_target_untouched(tmp_path):
    root = tmp_path / "clip"
    root.mkdir()
    ext = tmp_path / "external.json"
    ext.write_text("precious")
    os.symlink(ext, root / KEYFRAMES_FILENAME)
    save_hint_keyframes(str(root), [1], STEMS)
    assert ext.read_text() == "precious"
    assert not (root / KEYFRAMES_FILENAME).is_symlink()
    assert load_hint_keyframes(str(root), STEMS) == (1,)
    os.unlink(root / KEYFRAMES_FILENAME)
    os.symlink(ext, root / KEYFRAMES_FILENAME)
    clear_hint_keyframes(str(root))
    assert ext.read_text() == "precious"
    assert not os.path.lexists(root / KEYFRAMES_FILENAME)
