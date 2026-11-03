"""Round 3 T1: per-range hint provenance (merge, overlap, legacy, malformed, record identities)."""
from __future__ import annotations

import json
import types

import cv2
import numpy as np

from backend.service import task_job

STEMS = [f"s_{1000 + i}" for i in range(8)]


def _stage(d, stems, value):
    d.mkdir(parents=True, exist_ok=True)
    for s in stems:
        assert cv2.imwrite(str(d / f"{s}.png"), np.full((4, 4), value, np.uint8))
    return d


def _publish(tmp, final, stems, value, job_id, record="default"):
    staged = _stage(tmp / f"stg-{job_id}", stems, value)
    prov = {"kind": "gvm", "job_id": job_id}
    if record:
        prov["record"] = {"kind": "gvm", "job_id": job_id, "params": {"v": value}} if record == "default" else record
    job = types.SimpleNamespace(job_id=job_id)
    task_job.publish_hint_dir(str(staged), str(final), stems, job, prov)


def _ranges(final):
    return task_job.read_hint_ranges(str(final))


def test_overlapping_partial_publishes_split_and_keep_unaffected(tmp_path):
    final = tmp_path / "AlphaHint"
    _publish(tmp_path, final, STEMS[0:6], 10, "a")
    _publish(tmp_path, final, STEMS[3:8], 20, "b")  # overlaps 3..5
    by_job = {r["job_id"]: r for r in _ranges(final)}
    assert by_job["a"]["stems"] == STEMS[0:3] and set(by_job["a"]["digests"]) == set(STEMS[0:3])
    assert by_job["b"]["stems"] == STEMS[3:8]
    _publish(tmp_path, final, STEMS[0:8], 30, "c")  # swallows both
    assert [r["job_id"] for r in _ranges(final)] == ["c"]
    assert set(task_job.certified_stems(str(final))) == set(STEMS)
    prov = json.loads((final / task_job.HINT_PROVENANCE).read_text())
    assert prov["job_id"] == "c" and "record" not in prov  # old top-level fields stay


def test_publish_record_uses_only_staged_stems(tmp_path):
    final = tmp_path / "AlphaHint"
    staged = _stage(tmp_path / "stg-partial", [STEMS[0], STEMS[2]], 10)
    task_job.publish_hint_dir(str(staged), str(final), STEMS[:3],
                              types.SimpleNamespace(job_id="partial"),
                              {"kind": "gvm", "record": {"kind": "gvm", "job_id": "partial"}})

    (record,) = _ranges(final)
    assert record["stems"] == [STEMS[0], STEMS[2]]
    assert set(record["stats"]) == set(record["digests"]) == set(record["stems"])


def test_changed_frame_is_no_longer_certified(tmp_path):
    final = tmp_path / "AlphaHint"
    _publish(tmp_path, final, STEMS[0:3], 10, "a")
    assert cv2.imwrite(str(final / f"{STEMS[1]}.png"), np.full((4, 4), 99, np.uint8))
    assert set(task_job.certified_stems(str(final))) == {STEMS[0], STEMS[2]}


def test_legacy_provenance_certifies_only_its_range(tmp_path):
    final = _stage(tmp_path / "AlphaHint", STEMS, 10)
    (final / task_job.HINT_PROVENANCE).write_text(json.dumps(
        {"kind": "gvm", "job_id": "old", "range": {"in_stem": STEMS[2], "out_stem": STEMS[4]}, "params": {}}))
    (r,) = _ranges(final)
    assert r["legacy"] and r["stems"] == STEMS[2:5] and r["digests"] == {}
    assert task_job.certified_stems(str(final)) == {}
    _publish(tmp_path, final, STEMS[3:5], 20, "new")  # legacy record shrinks, frames outside it stay on disk
    recs = _ranges(final)
    assert recs[0]["legacy"] and recs[0]["stems"] == [STEMS[2]]
    assert recs[1]["job_id"] == "new"
    assert len(list(final.glob("*.png"))) == len(STEMS)


def test_imported_result_without_record_is_kept_uncertified(tmp_path):
    final = _stage(tmp_path / "AlphaHint", STEMS[:4], 10)  # no provenance at all (imported)
    assert _ranges(final) == []
    _publish(tmp_path, final, STEMS[:2], 20, "j", record=None)
    assert _ranges(final) == [] and len(list(final.glob("*.png"))) == 4


def test_malformed_metadata_never_raises(tmp_path):
    final = _stage(tmp_path / "AlphaHint", STEMS[:3], 10)
    for bad in ("{", "[]", "null", '{"ranges": 5}', '{"ranges": [1, {"stems": 3}, {"stems": ["a"], "digests": []}]}',
                '{"range": {"in_stem": 1}}', '{"range": {"in_stem": "a"}}'):
        (final / task_job.HINT_PROVENANCE).write_text(bad)
        assert _ranges(final) == [] and task_job.certified_stems(str(final)) == {}
    _publish(tmp_path, final, STEMS[:2], 20, "ok")  # publishing over garbage still works
    assert [r["job_id"] for r in _ranges(final)] == ["ok"]


def test_record_has_identities_and_no_key_settings(tmp_path):
    src = _stage(tmp_path / "Frames", STEMS[:2], 5)
    m = tmp_path / "m.png"
    cv2.imwrite(str(m), np.zeros((4, 4), np.uint8))
    job = types.SimpleNamespace(
        kind="matanyone2", job_id="j", range={"in_stem": STEMS[0], "out_stem": STEMS[1]}, publish_dir=str(tmp_path / "AlphaHint"),
        params={"input_is_linear": True, "seed": 7, "mask_cleanup": {"grow": 2, "fill_holes": True, "feather": 1.0}})
    ctx = types.SimpleNamespace(clip=types.SimpleNamespace(input_asset=types.SimpleNamespace(path=str(src))),
                                stems=STEMS[:2], files=[f"{s}.png" for s in STEMS[:2]], masks={STEMS[0]: str(m)}, prompts={})
    r = task_job._hint_record(job, ctx)
    assert r["seed"] == 7 and r["colour"] is True and r["conditioning_range"] == r["range"]
    assert set(r["source"]) == set(STEMS[:2]) and set(r["upstream"]) == {STEMS[0]}
    assert r["cleanup"]["grow"] == 0  # stage_cleanup: this stage only feathers
    first = task_job._prompt_fp({"a": np.ones(2)})
    assert first != task_job._prompt_fp({"a": np.zeros(2)})


def test_publish_hashes_each_frame_once_and_recheck_skips_unchanged(tmp_path, monkeypatch):
    final = tmp_path / "AlphaHint"
    calls = []
    real = task_job._sha
    monkeypatch.setattr(task_job, "_sha", lambda p: calls.append(p) or real(p))
    _publish(tmp_path, final, STEMS[:3], 10, "a")
    assert len(calls) == 3 and all("stg-a" in c for c in calls)  # hashed in staging, not re-read on the share
    calls.clear()
    assert set(task_job.certified_stems(str(final))) == set(STEMS[:3]) and calls == []
    _publish(tmp_path, final, STEMS[2:4], 20, "b")  # only the new frames are hashed
    assert len(calls) == 2
    calls.clear()
    assert cv2.imwrite(str(final / f"{STEMS[0]}.png"), np.full((4, 4), 99, np.uint8))
    st = (final / f"{STEMS[0]}.png").stat()
    import os
    os.utime(final / f"{STEMS[0]}.png", ns=(st.st_atime_ns, st.st_mtime_ns + 5))
    assert STEMS[0] not in task_job.certified_stems(str(final)) and len(calls) == 1


def test_legacy_range_survives_a_deleted_end_stem(tmp_path):
    final = _stage(tmp_path / "AlphaHint", STEMS, 10)
    (final / task_job.HINT_PROVENANCE).write_text(json.dumps(
        {"kind": "gvm", "job_id": "old", "range": {"in_stem": STEMS[2], "out_stem": STEMS[4]}}))
    (final / f"{STEMS[2]}.png").unlink()
    (r,) = _ranges(final)
    assert r["legacy"] and r["stems"] == STEMS[3:5]


def test_record_marks_vanished_inputs_unknown(tmp_path):
    src = _stage(tmp_path / "Frames", STEMS[:2], 5)
    job = types.SimpleNamespace(
        kind="matanyone2", job_id="j", range={}, publish_dir=str(tmp_path / "AlphaHint"), params={})
    ctx = types.SimpleNamespace(clip=types.SimpleNamespace(input_asset=types.SimpleNamespace(path=str(src))),
                                stems=STEMS[:2], files=[f"{s}.png" for s in STEMS[:2]],
                                masks={STEMS[0]: str(tmp_path / "gone.png")}, prompts={})
    (src / f"{STEMS[1]}.png").unlink()
    r = task_job._hint_record(job, ctx)
    assert r["source"][STEMS[1]] == "unknown" and r["upstream"] == {STEMS[0]: "unknown"}
