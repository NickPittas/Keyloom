"""A15: movies and companion hints are referenced in place; a legacy Source/ copy still wins."""
import importlib.util
import json
import os
import shutil
import sys

import pytest

from backend.clip_state import ClipEntry, ClipState
from backend.project import add_clips_to_project, read_clip_json, resolve_source_movie, write_clip_json, write_project_json


def _load_ck_import():
    path = os.path.join(os.path.dirname(__file__), "..", "rust-ui", "tools", "ck_import.py")
    spec = importlib.util.spec_from_file_location("ck_import_a15", path)
    mod = importlib.util.module_from_spec(spec)
    saved = sys.stdout
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.stdout = saved  # ck_import redirects stdout at import
    return mod


@pytest.fixture
def project(tmp_path):
    proj = tmp_path / "proj"
    (proj / "clips").mkdir(parents=True)
    write_project_json(str(proj), {"version": 2, "clips": []})
    return str(proj)


def _movie(folder, name="Shot01.mov"):
    os.makedirs(folder, exist_ok=True)
    p = os.path.join(folder, name)
    with open(p, "wb") as f:
        f.write(b"\x00" * 64)
    return p


def test_movie_import_makes_no_source_copy(tmp_path, project):
    mov = _movie(tmp_path / "media")
    (clip,) = add_clips_to_project(project, [mov])
    assert not os.path.exists(os.path.join(clip, "Source"))
    assert read_clip_json(clip)["source"]["original_path"] == os.path.abspath(mov)
    assert resolve_source_movie(clip) == os.path.abspath(mov)


def test_extraction_reads_the_original(tmp_path, project, monkeypatch):
    ck = _load_ck_import()
    mov = _movie(tmp_path / "media")
    (clip,) = add_clips_to_project(project, [mov])
    seen = {}
    monkeypatch.setattr(ck, "probe_video", lambda p: seen.setdefault("probe", p) and {"frame_count": 1})

    def fake_extract(src, tmp, **kw):
        seen["extract"] = src
        os.makedirs(tmp)
        open(os.path.join(tmp, ".dwab_done"), "w").close()
        return 1

    monkeypatch.setattr(ck, "extract_frames", fake_extract)
    monkeypatch.setattr(ck, "build_video_metadata", lambda *a: {})
    monkeypatch.setattr(ck, "write_video_metadata", lambda *a: None)
    assert ck.extract_clip(clip) == 1
    assert seen["extract"] == os.path.abspath(mov)
    assert not os.path.exists(os.path.join(clip, "Source"))


def test_companion_movies_are_not_copied(tmp_path, project):
    mov = _movie(tmp_path / "media")
    alpha = _movie(tmp_path / "media", "Shot01_AlphaHint.mov")
    mask = _movie(tmp_path / "media", "Shot01_MaskHint.mp4")
    (clip,) = add_clips_to_project(project, [mov])
    assert sorted(os.listdir(clip)) == ["clip.json"]
    assert read_clip_json(clip)["source"]["hint_movies"] == {
        "AlphaHint": os.path.abspath(alpha), "VideoMamaMaskHint": os.path.abspath(mask)}
    # the scan resolves them from their original location
    write_clip_json(clip, read_clip_json(clip))
    entry = ClipEntry(name="c", root_path=clip)
    entry.find_assets()
    assert entry.input_asset.path == os.path.abspath(mov)
    assert entry.alpha_asset.path == os.path.abspath(alpha)
    assert entry.mask_asset.path == os.path.abspath(mask)


def test_companion_sequence_is_referenced(tmp_path):
    from backend.project_media import create_clip_from_sequence
    seq = tmp_path / "media" / "Shot02"
    seq.mkdir(parents=True)
    (seq / "0001.png").write_bytes(b"x")
    hint = tmp_path / "media" / "Shot02_AlphaHint"
    hint.mkdir()
    (hint / "0001.png").write_bytes(b"x")
    clips = tmp_path / "clips"
    clips.mkdir()
    name = create_clip_from_sequence(str(clips), str(seq))
    assert os.listdir(clips / name) == ["clip.json"]
    assert read_clip_json(str(clips / name))["source"]["hint_sequences"] == {"AlphaHint": str(hint)}
    entry = ClipEntry(name=name, root_path=str(clips / name))
    entry.find_assets()
    assert entry.alpha_asset.path == str(hint)


def test_old_project_with_source_copy_still_resolves(tmp_path, project):
    mov = _movie(tmp_path / "media")
    (clip,) = add_clips_to_project(project, [mov])
    copy = _movie(os.path.join(clip, "Source"))
    os.remove(mov)  # even with the original gone, the legacy copy is used
    assert resolve_source_movie(clip) == copy
    entry = ClipEntry(name="c", root_path=clip)
    entry.find_assets()
    assert entry.input_asset.path == copy
    shutil.rmtree(os.path.join(clip, "Source"))


def test_missing_original_gives_clear_message(tmp_path, project):
    mov = _movie(tmp_path / "media")
    (clip,) = add_clips_to_project(project, [mov])
    os.remove(mov)
    with pytest.raises(FileNotFoundError, match=r"Source movie not found at .*Shot01\.mov: reconnect or relink it"):
        resolve_source_movie(clip)
    assert not os.path.exists(os.path.join(clip, "Source"))  # never re-copied
    entry = ClipEntry(name="c", root_path=clip)
    entry.find_assets()
    assert entry.state == ClipState.ERROR
    assert "Source movie not found at" in entry.error_message and "Shot01.mov" in entry.error_message


def test_non_ascii_path(tmp_path, project):
    mov = _movie(tmp_path / "médïa Ω 映像", "Šhot 01.mov")
    (clip,) = add_clips_to_project(project, [mov])
    assert resolve_source_movie(clip) == os.path.abspath(mov)
    assert not os.path.exists(os.path.join(clip, "Source"))


def test_relinked_movie_is_found(tmp_path, project):
    mov = _movie(tmp_path / "media")
    (clip,) = add_clips_to_project(project, [mov])
    os.remove(mov)
    new = _movie(tmp_path / "elsewhere")
    data = read_clip_json(clip)  # what relink.rs point_at_movie writes
    data["source"].update(original_path=new, filename=os.path.basename(new))
    write_clip_json(clip, data)
    assert resolve_source_movie(clip) == new
