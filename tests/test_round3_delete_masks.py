"""Round 3 A8: exact app-side deletion of generated pre-mattes/hints and the reuse of what stays."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import types

import cv2
import numpy as np
import pytest

from backend import hint_assets as ha
from backend import hint_compat as hc
from backend.clip_state import MASK_TRACK_MANIFEST
from backend.mask_delete_cli import run as cli_run
from backend.service import task_job

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
S = [f"s_{1000 + i}" for i in range(5)]
WANT = {"kind": "matanyone2", "params": {"mask_stem": S[0]}, "source": {s: [10, i] for i, s in enumerate(S)}, "upstream": {}}


def _put(final, stems, job_id, value=10):
    staged = final.parent / f"stg-{job_id}"
    staged.mkdir(parents=True)
    for s in stems:
        assert cv2.imwrite(str(staged / f"{s}.png"), np.full((4, 4), value, np.uint8))
    task_job.publish_hint_dir(str(staged), str(final), stems, types.SimpleNamespace(job_id=job_id),
                              {"kind": "matanyone2", "job_id": job_id, "record": {**WANT, "job_id": job_id}})


@pytest.fixture
def clip(tmp_path):
    root = tmp_path / "clip"
    root.mkdir()
    _put(root / "AlphaHint", S, "h1")
    _put(root / "VideoMamaMaskHint", S, "p1")
    (root / MASK_TRACK_MANIFEST).write_text(json.dumps({"source": "sam2", "frame_stems": S}))
    (root / "AlphaHint.mp4").write_bytes(b"movie")
    (root / "clip.json").write_text("{}")
    (root / "sam-frames").mkdir()
    (root / "sam-frames" / "f.png").write_bytes(b"prepared")
    return root


def _names(d):
    return sorted(p.name for p in d.iterdir() if not p.name.startswith("."))


def test_exact_frames_are_deleted_and_only_their_records(clip):
    out = ha.delete_masks(str(clip), "hint", "frames", [S[1], S[3]])
    assert out["errors"] == [] and sorted(os.path.basename(p) for p in out["deleted"]) == [f"{S[1]}.png", f"{S[3]}.png"]
    assert _names(clip / "AlphaHint") == [f"{S[i]}.png" for i in (0, 2, 4)]
    recs = json.loads((clip / "AlphaHint" / ha.PROVENANCE).read_text())["ranges"]
    assert sorted(recs[0]["stems"]) == [S[0], S[2], S[4]] and set(recs[0]["digests"]) == set(recs[0]["stems"])
    assert len(_names(clip / "VideoMamaMaskHint")) == 5  # the other asset is untouched
    assert (clip / "AlphaHint.mp4").exists() and (clip / "sam-frames" / "f.png").exists()


def test_all_frames_clear_folder_records_and_manifest_but_keep_the_rest(clip):
    out = ha.delete_masks(str(clip), "prematte", "all")
    assert out["errors"] == [] and not (clip / "VideoMamaMaskHint").exists() and not (clip / MASK_TRACK_MANIFEST).exists()
    assert (clip / "AlphaHint").is_dir() and (clip / "clip.json").exists() and (clip / "sam-frames" / "f.png").exists()


def test_partial_prematte_deletion_keeps_the_manifest_in_step(clip):
    ha.delete_masks(str(clip), "prematte", "frames", [S[0]])
    assert json.loads((clip / MASK_TRACK_MANIFEST).read_text())["frame_stems"] == S[1:]


def test_regenerate_reuses_unchanged_frames(clip):
    keep = {s: (clip / "AlphaHint" / f"{s}.png").stat() for s in S[1:]}
    ha.delete_masks(str(clip), "hint", "frames", [S[0]])
    assert hc.check(str(clip / "AlphaHint"), S[1:], WANT)[0] == hc.COMPATIBLE  # survivors still certified
    assert hc.check(str(clip / "AlphaHint"), S, WANT)[0] != hc.COMPATIBLE  # the deleted frame must be made again
    _put(clip / "AlphaHint", [S[0]], "h2")
    assert hc.check(str(clip / "AlphaHint"), S, WANT)[0] == hc.COMPATIBLE
    for s, st in keep.items():  # reused: never rewritten (same inode contents + mtime)
        assert (clip / "AlphaHint" / f"{s}.png").stat().st_mtime_ns == st.st_mtime_ns


def test_companion_movie_is_kept_unless_ticked(clip):
    ha.delete_masks(str(clip), "hint", "all")
    assert (clip / "AlphaHint.mp4").exists()
    plan = ha.plan_mask_delete(str(clip), "hint", "all", movies=True)
    assert [os.path.basename(m) for m in plan["movies"]] == ["AlphaHint.mp4"]
    out = ha.delete_masks(str(clip), "hint", "all", movies=True)
    assert not (clip / "AlphaHint.mp4").exists() and any(p.endswith("AlphaHint.mp4") for p in out["deleted"])


def test_symlink_leaving_the_clip_is_never_followed(clip, tmp_path):
    outside = tmp_path / "outside.png"
    outside.write_bytes(b"precious")
    victim = clip / "AlphaHint" / f"{S[2]}.png"
    victim.unlink()
    victim.symlink_to(outside)
    out = ha.delete_masks(str(clip), "hint", "all")
    assert outside.read_bytes() == b"precious"
    assert [e["path"] for e in out["errors"]] == [str(victim)]
    assert os.path.lexists(victim) and not (clip / "AlphaHint" / f"{S[0]}.png").exists()  # the rest is gone
    # a linked asset folder is refused outright
    (clip / "VideoMamaMaskHint").rename(tmp_path / "moved")
    (clip / "VideoMamaMaskHint").symlink_to(tmp_path / "moved")
    with pytest.raises(ha.DeleteRefused):
        ha.delete_masks(str(clip), "prematte", "all")
    assert len(list((tmp_path / "moved").glob("*.png"))) == 5


def test_refused_while_a_writer_holds_the_lock_or_a_publish_was_interrupted(clip):
    (clip / ha.PUBLISH_LOCK).write_text(json.dumps({"job_id": "j", "host": "h", "pid": 1}))
    with pytest.raises(ha.DeleteRefused, match="lock"):
        ha.delete_masks(str(clip), "hint", "all")
    assert len(_names(clip / "AlphaHint")) == 5
    (clip / ha.PUBLISH_LOCK).unlink()
    (clip / "AlphaHint.prev-j").mkdir()
    out = cli_run({"cmd": "delete", "request_id": "r1", "clip_root": str(clip), "asset": "hint", "scope": "all"})
    assert out["event"] == "rejected" and "interrupted" in out["message"]
    assert len(_names(clip / "AlphaHint")) == 5 and not (clip / ha.PUBLISH_LOCK).exists()


def test_partial_failure_still_reports_every_file_so_the_app_rescans(clip, monkeypatch):
    real = ha._remove

    def flaky(path, real_root, removed):
        if path.endswith(f"{S[1]}.png"):
            raise OSError("in use")
        real(path, real_root, removed)

    monkeypatch.setattr(ha, "_remove", flaky)
    out = cli_run({"cmd": "delete", "request_id": "r2", "clip_root": str(clip), "asset": "hint", "scope": "all"})
    assert out["event"] == "masks_deleted"
    assert [e["message"] for e in out["errors"]] == ["in use"] and len(out["deleted"]) == 4
    assert _names(clip / "AlphaHint") == [f"{S[1]}.png"]  # folder kept, remaining record trimmed
    assert json.loads((clip / "AlphaHint" / ha.PROVENANCE).read_text())["ranges"][0]["stems"] == [S[1]]


def test_helper_process_answers_one_json_line(clip):
    req = json.dumps({"cmd": "plan", "request_id": "r3", "clip_root": str(clip), "asset": "hint", "scope": "frames", "stems": [S[0], "nope"]})
    p = subprocess.run([sys.executable, "-m", "backend.mask_delete_cli", req], cwd=REPO, capture_output=True, text=True, timeout=30)
    out = json.loads(p.stdout.strip().splitlines()[-1])
    assert out["event"] == "mask_plan" and out["files"] == [f"{S[0]}.png"] and not out["whole"]
    assert len(_names(clip / "AlphaHint")) == 5  # a plan deletes nothing
