"""prepare_v2_inputs(frame_range=...): only in-range frames/hints are read, staged and exposed."""
from __future__ import annotations

import builtins
import os
import shutil

import cv2
import pytest

from backend.service import CorridorKeyService, OutputConfig
from backend.service import v2_inputs
from backend.service.v2_inputs import V2InputError, prepare_v2_inputs, range_dimensions
from tests.test_v2_inputs import _clip, _seq, _write, _rgb

N = 10
NAMES = [f"f_{i:04d}.png" for i in range(N)]


def _poison(directory, names):
    """Replace frames with undecodable bytes: any read of them fails the test."""
    for n in names:
        with open(os.path.join(directory, n), "wb") as f:
            f.write(b"not an image")


@pytest.fixture
def spy(monkeypatch):
    """Fail on any open()/imread of a poisoned path."""
    banned: set[str] = set()
    real_open, real_imread = builtins.open, cv2.imread

    def guard(path):
        if isinstance(path, (str, os.PathLike)) and os.path.realpath(path) in banned:
            raise AssertionError(f"out-of-range read: {path}")

    def fake_open(path, *a, **k):
        guard(path)
        return real_open(path, *a, **k)

    def fake_imread(path, *a, **k):
        guard(path)
        return real_imread(path, *a, **k)

    monkeypatch.setattr(builtins, "open", fake_open)
    monkeypatch.setattr(cv2, "imread", fake_imread)
    return banned


def _prep(clip, tmp_path, rng, **kw):
    return prepare_v2_inputs(clip, use_alpha_hint=False, staging_dir=tmp_path / "stage",
                             frame_range=rng, **kw)


def test_input_staged_only_range_and_never_reads_outside(tmp_path, spy):
    d = _seq(tmp_path, NAMES)
    _poison(d, NAMES[:3] + NAMES[7:])  # includes frame 0, which dimension probing would read
    spy.update(os.path.realpath(os.path.join(d, n)) for n in NAMES[:3] + NAMES[7:])
    p = _prep(_clip(tmp_path, d), tmp_path, (3, 6))
    assert p.staged_input and p.frame_count == 4 and p.range_offset == 3
    assert p.source_stems == ["f_0003", "f_0004", "f_0005", "f_0006"]
    staged = sorted(os.listdir(p.input_path))
    assert staged == [f"{i:06d}.png" for i in range(4)]
    targets = [os.path.realpath(os.path.join(p.input_path, s)) for s in staged]
    assert targets == [os.path.realpath(os.path.join(d, n)) for n in NAMES[3:7]]


def test_range_dimensions_probe_uses_first_range_frame(tmp_path, spy):
    d = _seq(tmp_path, NAMES, size=(6, 8))
    _write(os.path.join(d, NAMES[5]), _rgb(1, (10, 12)))
    _poison(d, NAMES[:5])
    spy.update(os.path.realpath(os.path.join(d, n)) for n in NAMES[:5])
    assert range_dimensions(_clip(tmp_path, d), (5, 6)) == (12, 10)


@pytest.mark.parametrize("rng", [(-1, 3), (3, 2), (0, N), (N, N)])
def test_bad_range_rejected_without_staging(tmp_path, rng):
    d = _seq(tmp_path, NAMES)
    with pytest.raises(V2InputError):
        _prep(_clip(tmp_path, d), tmp_path, rng)
    assert not (tmp_path / "stage" / "input").exists()


def test_video_input_needs_sequence(tmp_path):
    clip = _clip(tmp_path, _seq(tmp_path, NAMES))
    clip.input_asset.asset_type = "video"
    with pytest.raises(V2InputError, match="image sequence"):
        _prep(clip, tmp_path, (1, 2))


def test_dense_hint_only_range_staged(tmp_path, spy):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="hint", gray=True)
    out = NAMES[:4] + NAMES[8:]
    _poison(a, out)
    spy.update(os.path.realpath(os.path.join(a, n)) for n in out)
    p = prepare_v2_inputs(_clip(tmp_path, d, a), use_alpha_hint=True, hint_mode="full",
                          staging_dir=tmp_path / "stage", frame_range=(4, 7))
    assert sorted(os.listdir(p.hint_path)) == [f"{i:06d}.png" for i in range(4)]
    assert os.path.realpath(os.path.join(p.hint_path, "000002.png")) == \
        os.path.realpath(os.path.join(a, NAMES[6]))


def test_dense_hint_missing_range_stem_is_named(tmp_path):
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="hint", gray=True)
    os.remove(os.path.join(a, NAMES[5]))
    with pytest.raises(V2InputError, match="f_0005"):
        prepare_v2_inputs(_clip(tmp_path, d, a), use_alpha_hint=True, hint_mode="full",
                          staging_dir=tmp_path / "stage", frame_range=(4, 7))


def test_dense_hint_resized_only_for_range(tmp_path):
    d = _seq(tmp_path, NAMES, size=(8, 16))
    a = _seq(tmp_path, NAMES, sub="hint", size=(4, 8), gray=True)  # same aspect, half size
    p = prepare_v2_inputs(_clip(tmp_path, d, a), use_alpha_hint=True, hint_mode="full",
                          staging_dir=tmp_path / "stage", frame_range=(2, 4))
    img = cv2.imread(os.path.join(p.hint_path, "000001.png"), cv2.IMREAD_UNCHANGED)
    assert img.shape[:2] == (8, 16) and len(os.listdir(p.hint_path)) == 3


def _kf_clip(tmp_path, marks, poison=()):
    from backend.hint_keyframes import save_hint_keyframes
    d = _seq(tmp_path, NAMES)
    a = _seq(tmp_path, NAMES, sub="hint", gray=True)
    clip = _clip(tmp_path, d, a)
    save_hint_keyframes(str(tmp_path), marks, [os.path.splitext(n)[0] for n in NAMES])
    clip.hint_keyframes = tuple(marks)
    _poison(a, [NAMES[k] for k in poison])
    return clip


def _kf(clip, tmp_path, rng):
    return prepare_v2_inputs(clip, use_alpha_hint=True, hint_mode="keyframes",
                             staging_dir=tmp_path / "stage", frame_range=rng)


def test_keyframes_filtered_and_rebased(tmp_path, spy):
    clip = _kf_clip(tmp_path, [1, 5, 7, 9], poison=[1, 9])
    a = clip.alpha_asset.path
    spy.update(os.path.realpath(os.path.join(a, NAMES[k])) for k in (1, 9))
    p = _kf(clip, tmp_path, (4, 8))
    assert p.hint_frame_indices == (1, 3) and p.range_offset == 4 and p.initial_hint_path is None
    assert sorted(os.listdir(p.hint_path)) == ["000001.png", "000003.png"]


def test_single_keyframe_rebased(tmp_path):
    p = _kf(_kf_clip(tmp_path, [2, 6]), tmp_path, (5, 8))
    assert p.hint_frame_indices is None and p.initial_hint_index == 1
    assert p.initial_hint_path.endswith("000001.png")


def test_keyframes_none_inside_range(tmp_path):
    with pytest.raises(V2InputError, match="none inside In/Out"):
        _kf(_kf_clip(tmp_path, [1, 9]), tmp_path, (3, 7))


def test_no_range_is_unchanged(tmp_path):
    d = _seq(tmp_path, NAMES)
    p = prepare_v2_inputs(_clip(tmp_path, d), use_alpha_hint=False, staging_dir=tmp_path / "s")
    assert p == v2_inputs.V2PreparedInput(d, [n[:-4] for n in NAMES], N, 24.0, False, None)
    assert p.range_offset == 0 and not (tmp_path / "s" / "input").exists()


def test_check_disk_uses_range_count_and_dims(tmp_path, monkeypatch):
    svc, cfg = CorridorKeyService(), OutputConfig(matte_enabled=True, fg_enabled=False,
                                                  comp_enabled=False, processed_enabled=False)
    need = 100 * 100 * 2 * 1 * 4
    usage = shutil.disk_usage
    monkeypatch.setattr(shutil, "disk_usage", lambda p: usage(p)._replace(free=need - 1))
    clip = _clip(tmp_path, _seq(tmp_path, NAMES))
    with pytest.raises(Exception, match="Not enough free disk"):
        svc._v2_check_disk(clip, cfg, str(tmp_path), count=2, dims=(100, 100))
    monkeypatch.setattr(shutil, "disk_usage", lambda p: usage(p)._replace(free=need))
    svc._v2_check_disk(clip, cfg, str(tmp_path), count=2, dims=(100, 100))


def _video(path, n=10, size=16):
    w = cv2.VideoWriter(str(path), cv2.VideoWriter_fourcc(*"MJPG"), 5, (size, size))
    if not w.isOpened():
        pytest.skip("no MJPG writer in this OpenCV build")
    for i in range(n):
        w.write(__import__("numpy").full((size, size, 3), 20 * i, "uint8"))
    w.release()
    return str(path)


class NoDecode:
    """VideoCapture proxy: header queries are fine, any decode/seek is a contract violation."""

    def __init__(self, cap):
        self._cap = cap

    def _bad(self, *a):
        raise AssertionError("video hint decoded/seeked in range mode")
    read = grab = retrieve = set = _bad

    def __getattr__(self, name):
        return getattr(self._cap, name)


@pytest.fixture
def no_decode(monkeypatch):
    from backend import frame_io
    wrap = lambda p: NoDecode(cv2.VideoCapture(p))  # noqa: E731
    monkeypatch.setattr(v2_inputs, "open_video", wrap)
    monkeypatch.setattr(frame_io, "open_video", wrap)


def _video_clip(tmp_path):
    d = _seq(tmp_path, NAMES, size=(16, 16))
    return _clip(tmp_path, d, _video(tmp_path / "hint.avi"), alpha_type="video"), d


@pytest.mark.parametrize("mode", ["full", "keyframes"])
def test_video_alphahint_rejected_in_range_mode_without_reading_or_writing(tmp_path, no_decode, mode):
    clip, d = _video_clip(tmp_path)
    clip.hint_keyframes = (1, 5, 7)
    with pytest.raises(V2InputError, match="image sequence"):
        prepare_v2_inputs(clip, use_alpha_hint=True, hint_mode=mode,
                          staging_dir=tmp_path / "stage", frame_range=(4, 8))
    assert not (tmp_path / "stage").exists()  # not even the input was staged


@pytest.mark.parametrize("fn", ["dense", "keyframes"])
def test_video_alphahint_defense_in_depth_in_branch_helpers(tmp_path, no_decode, fn):
    clip, _ = _video_clip(tmp_path)
    clip.hint_keyframes = (1, 5)
    stage = tmp_path / "stage"
    with pytest.raises(V2InputError, match="image sequence"):
        if fn == "dense":
            v2_inputs._prepare_hint_range(clip, ["a"], 4, (16, 16), stage, None)
        else:
            v2_inputs._prepare_keyframe_hint(clip, ["a", "b", "c", "d", "e"], stage, None,
                                             (4, 8), (16, 16))
    assert not stage.exists()


def test_video_alphahint_unused_when_hints_off_and_non_range_path_unchanged(tmp_path):
    clip, d = _video_clip(tmp_path)
    p = prepare_v2_inputs(clip, use_alpha_hint=False, staging_dir=tmp_path / "s1", frame_range=(2, 5))
    assert p.hint_path is None and p.frame_count == 4
    q = prepare_v2_inputs(clip, use_alpha_hint=True, hint_mode="full", staging_dir=tmp_path / "s2")
    assert q.hint_path == clip.alpha_asset.path  # Qt/whole-clip behaviour: the video is passed through
