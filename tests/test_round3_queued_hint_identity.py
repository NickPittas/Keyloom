"""Round 3 T6: queue-time hint reuse (compatibility check) and the frozen hint identity Key revalidates."""
from __future__ import annotations

import os
import types

import cv2
import numpy as np
import pytest

from backend import hint_compat as hc
from backend.service import key_job, task_job
from backend.service.key_job import KeyJob, KeyJobRejected
from tests.test_key_job import A, B, STEMS, env, manifest, rejected, run, svc  # noqa: F401

S = [f"s_{1000 + i}" for i in range(6)]
WANT = {"kind": "matanyone2", "params": {"mask_stem": S[0], "mask_cleanup": {"grow": 0, "fill_holes": False, "feather": 0}},
        "source": {s: [10, i] for i, s in enumerate(S)}, "upstream": {s: f"sha{i}" for i, s in enumerate(S)}}


def _put(final, stems, value, job_id, record):
    staged = final.parent / f"stg-{job_id}"
    staged.mkdir(parents=True)
    for s in stems:
        assert cv2.imwrite(str(staged / f"{s}.png"), np.full((4, 4), value, np.uint8))
    task_job.publish_hint_dir(str(staged), str(final), stems, types.SimpleNamespace(job_id=job_id),
                              {"kind": record["kind"], "job_id": job_id, "record": {**record, "job_id": job_id}})


def _made(**over):
    return {**WANT, "params": {**WANT["params"], "feather": 0.0, "extra_default": 1}, **over}


def test_compatible_hint_is_reusable_even_for_a_subset(tmp_path):
    final = tmp_path / "AlphaHint"
    _put(final, S, 10, "a", _made())
    assert hc.check(str(final), S, WANT)[0] == hc.COMPATIBLE
    assert hc.check(str(final), S[2:4], WANT)[0] == hc.COMPATIBLE  # a broader render serves a subset


@pytest.mark.parametrize("field,value,text", [
    ("kind", "videomama", "made by"),
    ("params", {"mask_stem": S[1]}, "settings"),
    ("source", {S[2]: [10, 99]}, "source frame"),
    ("upstream", {S[3]: "other"}, "pre-matte"),
    ("prompt_fp", "new", "prompts"),
])
def test_known_difference_is_a_mismatch_with_the_reason(tmp_path, field, value, text):
    final = tmp_path / "AlphaHint"
    _put(final, S, 10, "a", _made())
    want = {**WANT, field: {**WANT.get(field, {}), **value} if isinstance(value, dict) else value}
    status, why = hc.check(str(final), S, want)
    assert status == hc.MISMATCH and text in why


def test_matanyone2_hint_is_compared_on_the_in_frame_only(tmp_path):
    final = tmp_path / "AlphaHint"
    _put(final, S, 10, "a", _made(upstream={S[0]: "sha0"}))  # the worker records the seed mask only
    seed_only = {**WANT, "upstream": {S[0]: "sha0"}}
    assert hc.check(str(final), S, seed_only)[0] == hc.COMPATIBLE
    assert hc.check(str(final), S, {**WANT, "upstream": {S[0]: "other"}})[0] == hc.MISMATCH


def test_duplicate_stems_resolve_to_the_last_sorted_name_and_dotfiles_are_ignored(tmp_path):
    for n in ("f1.png", "f1.exr", "f1.dpx", ".x.png", "sh~v2.0001.png"):
        (tmp_path / n).write_bytes(b"x")
    files = hc._files(str(tmp_path))
    assert files == {"f1": "f1.png", "sh~v2.0001": "sh~v2.0001.png"}


def test_missing_edited_and_legacy_hints(tmp_path):
    final = tmp_path / "AlphaHint"
    _put(final, S[:4], 10, "a", _made())
    assert hc.check(str(final), S, WANT)[0] == hc.MISSING
    cv2.imwrite(str(final / f"{S[1]}.png"), np.full((4, 4), 77, np.uint8))  # changed after it was made
    assert hc.check(str(final), S[:4], WANT)[0] == hc.MISMATCH
    legacy = tmp_path / "old" / "AlphaHint"
    legacy.mkdir(parents=True)
    for s in S:
        cv2.imwrite(str(legacy / f"{s}.png"), np.zeros((4, 4), np.uint8))
    assert hc.check(str(legacy), S, WANT) == (hc.UNRECORDED, "Settings not recorded")
    (legacy / ".corridorkey_hint.json").write_text("{not json")  # malformed record: same, never a crash
    assert hc.check(str(legacy), S, WANT)[0] == hc.UNRECORDED


def test_partial_rerender_keeps_the_other_frames_compatible(tmp_path):
    final = tmp_path / "AlphaHint"
    _put(final, S, 10, "a", _made())
    _put(final, S[2:4], 20, "b", _made(upstream={s: "newer" for s in S}))
    assert hc.check(str(final), S[:2] + S[4:], WANT)[0] == hc.COMPATIBLE
    assert hc.check(str(final), S[2:4], WANT)[0] == hc.MISMATCH


# ------------------------------------------------------------ Key protocol: frozen identity

def _hint(e):
    d = e.root / "AlphaHint"
    d.mkdir()
    for s in STEMS:
        cv2.imwrite(str(d / f"{s}.png"), np.full((4, 4), 200, np.uint8))
    return d


def _frozen(d, stems=STEMS[A:B + 1]):
    return hc.identity(str(d), stems)


def test_unchanged_hint_runs_and_the_identity_is_recorded(svc, env):  # noqa: F811
    d = _hint(env)
    job = env.job(params={"hint_mode": "full"}, hint_identity=_frozen(d))
    _, done = run(svc, job)
    assert done["outcome"] == "complete" and manifest(env)["hint_identity"] == job["hint_identity"]


def test_hint_changed_before_dispatch_is_refused(svc, env):  # noqa: F811
    d = _hint(env)
    frozen = _frozen(d)
    cv2.imwrite(str(d / f"{STEMS[A + 1]}.png"), np.full((4, 4), 1, np.uint8))  # re-rendered after queueing
    e = rejected(svc, env.job(params={"hint_mode": "full"}, hint_identity=frozen))
    assert e.code == "hint" and "changed since it was queued" in str(e)
    assert not env.out.exists() or not any(env.out.rglob("*.exr"))


@pytest.mark.parametrize("bad", [{}, {"x": "1:2"}, {"a": 1}])
def test_identity_must_name_exactly_the_frames_key_reads(env, bad):  # noqa: F811
    with pytest.raises(KeyJobRejected):
        KeyJob.from_dict(env.job(params={"hint_mode": "full"}, hint_identity=bad))
    with pytest.raises(KeyJobRejected):  # no hint used -> no identity
        KeyJob.from_dict(env.job(hint_identity={s: "1:1" for s in STEMS[A:B + 1]}))


def test_resume_is_refused_when_the_hint_changed(svc, env):  # noqa: F811
    d = _hint(env)
    _, done = run(svc, env.job(params={"hint_mode": "full"}, hint_identity=_frozen(d)))
    assert done["outcome"] == "complete"
    m = manifest(env)
    same = KeyJob.from_dict(env.job(params={"hint_mode": "full"}, hint_identity=_frozen(d), mode="resume"))
    assert key_job.resume_problem(m, same, STEMS[A:B + 1]) is None
    cv2.imwrite(str(d / f"{STEMS[B]}.png"), np.full((4, 4), 3, np.uint8))
    changed = KeyJob.from_dict(env.job(params={"hint_mode": "full"}, hint_identity=_frozen(d), mode="resume"))
    assert "hint changed" in key_job.resume_problem(m, changed, STEMS[A:B + 1])
    old = {k: v for k, v in m.items() if k != "hint_identity"}
    why = key_job.resume_problem(old, same, STEMS[A:B + 1])  # no recorded hint: old and new frames could mix
    assert why and "did not record which hint" in why
    plain = KeyJob.from_dict(env.job(params={"hint_mode": "full"}, mode="resume"))  # nothing frozen: as before
    assert key_job.resume_problem(old, plain, STEMS[A:B + 1]) is None
    assert os.path.isdir(d)


def test_task_job_lists_hint_files_like_hint_compat(tmp_path):
    for n in ("f1.png", "f1.exr", "f1.dpx", ".x.png", "sh~v2.0001.png", "f2.jpg"):
        (tmp_path / n).write_bytes(b"x")
    assert task_job._images(str(tmp_path)) == hc._files(str(tmp_path)) == {
        "f1": "f1.png", "sh~v2.0001": "sh~v2.0001.png", "f2": "f2.jpg"}  # sorted: f1.png beats f1.exr


def test_a_hint_made_in_a_chain_records_what_key_read_and_resume_rechecks_it(svc, env):  # noqa: F811
    d = _hint(env)
    _, done = run(svc, env.job(params={"hint_mode": "full"}))  # nothing frozen at queue time (hint came from the chain)
    assert done["outcome"] == "complete"
    assert manifest(env)["hint_identity"] == _frozen(d)
    cv2.imwrite(str(d / f"{STEMS[B]}.png"), np.full((4, 4), 3, np.uint8))  # re-rendered afterwards
    e = rejected(svc, env.job(params={"hint_mode": "full"}, mode="resume"))
    assert "hint changed" in str(e)


# --- one render, two routes: the producer writing AlphaHint directly vs promoting its pre-matte ------------------
CLEAN = {"grow": 0, "fill_holes": False, "feather": 0}
SRC = {s: [10, i] for i, s in enumerate(S)}


def _chroma_pair(tmp_path, hint_record, guide_value=10):
    guide, hint = tmp_path / "VideoMamaMaskHint", tmp_path / "AlphaHint"
    producer = {"kind": "chroma", "params": {"key_color": "green", "mask_cleanup": CLEAN}, "source": SRC}
    _put(guide, S, guide_value, "g", producer)
    _put(hint, S, 10, "h", hint_record(producer, guide))
    return guide, hint


def _promote(guide):
    digests = task_job.certified_stems(str(guide))[S[0]]["digests"]
    return {"kind": "prematte_hint", "params": {"mask_cleanup": CLEAN}, "source": SRC, "upstream": dict(digests)}


def _plan_promotion(guide):
    return {"kind": "prematte_hint", "params": {"mask_cleanup": CLEAN}, "source": SRC,
            "upstream": dict(task_job.certified_stems(str(guide))[S[0]]["digests"]), "guide_folder": str(guide)}


def test_a_direct_hint_is_reused_by_the_prematte_plan(tmp_path):
    guide, hint = _chroma_pair(tmp_path, lambda producer, guide: producer)
    assert hc.check(str(hint), S, {**_plan_promotion(guide), "guide_folder": None})[0] == hc.MISMATCH  # old rule
    assert hc.check(str(hint), S, _plan_promotion(guide))[0] == hc.COMPATIBLE


def test_a_prematte_hint_is_reused_by_the_direct_route(tmp_path):
    guide, hint = _chroma_pair(tmp_path, lambda producer, guide: _promote(guide))
    direct = {"kind": "chroma", "params": {"key_color": "green", "mask_cleanup": CLEAN}, "source": SRC,
              "guide_folder": str(guide)}
    assert hc.check(str(hint), S, direct)[0] == hc.COMPATIBLE
    assert hc.check(str(hint), S, {**direct, "params": {"key_color": "blue", "mask_cleanup": CLEAN}})[0] == hc.MISMATCH
    assert hc.check(str(hint), S, {**direct, "kind": "birefnet"})[0] == hc.MISMATCH


def test_a_changed_prematte_frame_or_producer_still_asks(tmp_path):
    guide, hint = _chroma_pair(tmp_path, lambda producer, guide: producer)
    plan = _plan_promotion(guide)
    assert hc.check(str(hint), S, {**plan, "params": {"mask_cleanup": {**CLEAN, "feather": 4}}})[0] == hc.MISMATCH
    assert hc.check(str(hint), S, {**plan, "source": {**SRC, S[1]: [10, 99]}})[0] == hc.MISMATCH
    assert cv2.imwrite(str(guide / f"{S[2]}.png"), np.full((4, 4), 200, np.uint8))  # pre-matte edited since
    status, _ = hc.check(str(hint), S, plan)
    assert status == hc.MISMATCH


def test_a_hint_from_another_producer_than_the_prematte_still_asks(tmp_path):
    guide, hint = _chroma_pair(tmp_path, lambda producer, guide: {**producer, "kind": "birefnet"})
    assert hc.check(str(hint), S, _plan_promotion(guide))[0] == hc.MISMATCH
