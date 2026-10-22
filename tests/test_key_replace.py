"""Key mode='replace': journaled swap of an app-owned result; the old result survives everything."""
from __future__ import annotations

import json
import os
import threading
from pathlib import Path

import pytest

from backend.service import key_job
from backend.service.key_job import KeyJob, KeyJobRejected, run_key_job
from tests.test_key_job import (A, B, STEMS, FakeEngine, env, manifest, rejected, run, svc,  # noqa: F401
                                snapshot)


def contents(root):
    return {str(p): p.read_bytes() for p in Path(root).rglob("*") if p.is_file()}


def first_result(svc, env):
    _, done = run(svc, env.job(job_id="job-old00001"))
    assert done["outcome"] == "complete"
    return contents(env.out)


def replace_job(env, **over):
    over.setdefault("job_id", "job-new00001")
    return env.job(mode="replace", **over)


def test_replace_swaps_keeps_unknown_files_and_cleans_up(svc, env):
    old = first_result(svc, env)
    (env.out / "Fg" / "notes.txt").write_text("mine")
    (env.out / "keep.me").write_text("also mine")
    FakeEngine.hdr = 3.0  # the new fg differs from the old one
    events, done = run(svc, replace_job(env, params={"despill_strength": 0.7}))
    assert done["outcome"] == "complete" and (done["published"], done["manifest_complete"]) == (3, True)
    now = contents(env.out)
    fg = env.out / "Fg" / f"{STEMS[A]}.exr"
    assert now[str(fg)] != old[str(fg)]
    assert (env.out / "Fg" / "notes.txt").read_text() == "mine" and (env.out / "keep.me").read_text() == "also mine"
    m = manifest(env)
    assert m["job_id"] == "job-new00001" and m["replaces"] == "job-old00001" and m["complete"] is True
    assert m["params"]["despill_strength"] == 0.7
    assert not [p for p in now if "/.ck-" in p or key_job.REPLACE_JOURNAL in p or key_job.LOCK_NAME in p]
    assert [e["event"] for e in events].count("published") == 1


def test_replace_failure_and_cancel_keep_old_result(svc, env, monkeypatch):
    old = first_result(svc, env)
    snap = snapshot(env.out)

    def boom(self, check_cancel, **k):
        raise RuntimeError("engine blew up")
    FakeEngine.hdr = 3.0
    orig = FakeEngine.run_clip
    FakeEngine.run_clip = lambda self, *a, **k: (_ for _ in ()).throw(RuntimeError("engine blew up"))
    try:
        _, done = run(svc, replace_job(env))
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "failed" and contents(env.out) == old and snapshot(env.out) == snap

    cancel = threading.Event()
    FakeEngine.run_clip = lambda self, *a, **k: (cancel.set(), orig(self, *a, **k))[1]
    try:
        _, done = run(svc, replace_job(env, job_id="job-new00002"), cancel=cancel)
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "cancelled" and "previous result was kept" in done["message"]
    assert contents(env.out) == old and snapshot(env.out) == snap

    calls = []
    real = key_job._render

    def flaky(*a, **k):  # a frame fails to render after others were staged
        calls.append(1)
        if len(calls) == 3:
            raise OSError("disk full")
        return real(*a, **k)
    monkeypatch.setattr(key_job, "_render", flaky)
    _, done = run(svc, replace_job(env, job_id="job-new00003"))
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    assert contents(env.out) == old and snapshot(env.out) == snap


def test_cancel_after_staging_before_commit_keeps_old(svc, env, monkeypatch):
    old = first_result(svc, env)
    cancel = threading.Event()
    real = key_job._valid_image

    def late(path):
        cancel.set()  # cancel lands while validating, the last point before the swap
        return real(path)
    monkeypatch.setattr(key_job, "_valid_image", late)
    _, done = run(svc, replace_job(env), cancel=cancel)
    assert done["outcome"] == "cancelled" and contents(env.out) == old


def test_invalid_new_file_blocks_the_swap(svc, env, monkeypatch):
    old = first_result(svc, env)
    monkeypatch.setattr(key_job, "_valid_image", lambda p: False)
    _, done = run(svc, replace_job(env))
    assert done["outcome"] == "failed" and "validation" in done["message"] and contents(env.out) == old


@pytest.mark.parametrize("what", ["none", "foreign", "files", "range", "symlink", "format"])
def test_replace_rejections_touch_nothing(svc, env, what):
    kw = {}
    if what != "none":
        first_result(svc, env)
        if what == "foreign":
            m = manifest(env)
            m["clip_id"] = "other/clip"
            (env.out / ".corridorkey_manifest.json").write_text(json.dumps(m))
        elif what == "files":
            kw["cfg"] = {"comp_enabled": True}
        elif what == "format":
            kw["cfg"] = {"fg_format": "png"}
        elif what == "range":
            kw["range"] = {"in_point": A, "out_point": B - 1, "in_stem": STEMS[A], "out_stem": STEMS[B - 1]}
        elif what == "symlink":
            f = env.out / "Fg" / f"{STEMS[A]}.exr"
            f.unlink()
            f.symlink_to(env.tmp / "elsewhere")
    d = replace_job(env, **kw)
    if what == "range":
        d["outputs"] = {o: p[:-1] for o, p in d["outputs"].items()}
    snap = snapshot(env.out)
    assert rejected(svc, d).code in (("exists", "paths") if what == "symlink" else ("exists",))
    assert snapshot(env.out) == snap
    assert not (env.out / key_job.LOCK_NAME).exists()


def test_replace_does_not_steal_the_lock(svc, env):
    first_result(svc, env)
    (env.out / key_job.LOCK_NAME).write_text(json.dumps({"job_id": "other", "host": "x"}))
    assert rejected(svc, replace_job(env)).code == "busy"


# ---------------------------------------------------------------- crash recovery

def crash_at(monkeypatch, nth, real_rename=os.rename):
    """Die (no in-process undo) when the nth rename of the swap happens."""
    count = []

    def rename(src, dst, *a, **k):
        if ".ck-" in str(src) or ".ck-old-" in str(dst):
            count.append(1)
            if len(count) == nth:
                raise SystemExit("power loss")
        return real_rename(src, dst, *a, **k)
    monkeypatch.setattr(os, "rename", rename)
    monkeypatch.setattr(key_job, "_undo_swap", lambda ents: (_ for _ in ()).throw(SystemExit("dead")))
    # ^ the process is gone: no in-process undo, no journal removal


@pytest.mark.parametrize("nth", [1, 2, 5, 12, 13, 14])
def test_crash_mid_swap_rolls_back_then_next_job_proceeds(svc, env, monkeypatch, nth):
    old = first_result(svc, env)
    FakeEngine.hdr = 3.0
    with monkeypatch.context() as mp:
        crash_at(mp, nth)
        with pytest.raises(SystemExit):
            run(svc, replace_job(env))
    assert (env.out / key_job.REPLACE_JOURNAL).exists()
    (env.out / key_job.LOCK_NAME).unlink(missing_ok=True)  # the dead writer's lock is cleared by the user/UI
    assert key_job.recover_replace(str(env.out)) == "prepare"
    assert contents(env.out) == old  # exactly the old result, no leftovers, no journal
    _, done = run(svc, replace_job(env, job_id="job-new00009"))  # and a retry works
    assert done["outcome"] == "complete" and manifest(env)["job_id"] == "job-new00009"


def test_job_start_recovers_interrupted_swap(svc, env, monkeypatch):
    old = first_result(svc, env)
    FakeEngine.hdr = 3.0
    with monkeypatch.context() as mp:
        crash_at(mp, 3)
        with pytest.raises(SystemExit):
            run(svc, replace_job(env))
    (env.out / key_job.LOCK_NAME).unlink(missing_ok=True)
    FakeEngine.hdr = None
    _, done = run(svc, env.job(job_id="job-resume01", mode="resume"))  # any Key job settles it first
    assert not (env.out / key_job.REPLACE_JOURNAL).exists()
    assert manifest(env)["job_id"] == "job-old00001"
    assert done["outcome"] == "complete" and done["skipped"] == 3


def test_crash_after_commit_flag_rolls_forward(svc, env, monkeypatch):
    old = first_result(svc, env)
    FakeEngine.hdr = 3.0
    with monkeypatch.context() as mp:
        mp.setattr(key_job, "_finish_swap", lambda ents: (_ for _ in ()).throw(SystemExit("power loss")))
        with pytest.raises(SystemExit):
            run(svc, replace_job(env))
    j = json.loads((env.out / key_job.REPLACE_JOURNAL).read_text())
    assert j["phase"] == "committed" and any(".ck-old-" in p.name for p in env.out.rglob("*"))
    (env.out / key_job.LOCK_NAME).unlink(missing_ok=True)
    assert key_job.recover_replace(str(env.out)) == "committed"
    now = contents(env.out)
    assert manifest(env)["job_id"] == "job-new00001" and manifest(env)["complete"] is True
    assert not [p for p in now if "/.ck-" in p or key_job.REPLACE_JOURNAL in p]
    assert now[str(env.out / "Fg" / f"{STEMS[A]}.exr")] != old[str(env.out / "Fg" / f"{STEMS[A]}.exr")]


@pytest.mark.parametrize("bad", ["outside", "badname", "malformed", "symlink"])
def test_untrusted_journal_is_refused_untouched(svc, env, bad):
    old = first_result(svc, env)
    jp = env.out / key_job.REPLACE_JOURNAL
    ent = {"final": str(env.tmp / "victim.txt"), "new": str(env.out / ".ck-x"), "bak": str(env.out / ".ck-old-x"),
           "had_old": True}
    (env.tmp / "victim.txt").write_text("precious")
    if bad == "badname":
        ent = {**ent, "final": str(env.out / "Fg" / "notes"), "bak": str(env.out / "Fg" / "notes2")}
    if bad == "symlink":
        (env.tmp / "target.json").write_text("{}")
        jp.symlink_to(env.tmp / "target.json")
    else:
        jp.write_text("not json" if bad == "malformed" else json.dumps(
            {"schema": 1, "phase": "prepare", "items": [ent]}))
    with pytest.raises(KeyJobRejected) as e:
        key_job.recover_replace(str(env.out))
    assert e.value.code == "exists" and (env.tmp / "victim.txt").read_text() == "precious"
    assert {k: v for k, v in contents(env.out).items() if not k.endswith(key_job.REPLACE_JOURNAL)} == old


def test_hello_advertises_replace():
    from backend import worker
    assert "replace" in worker.probe_hello(None)["capabilities"]["key_modes"]


@pytest.mark.parametrize("mode", ["new", "resume", "replace"])
@pytest.mark.parametrize("nth", [13, 14])
def test_every_mode_reaches_recovery_after_crash_without_manifest(svc, env, monkeypatch, mode, nth):
    """14th rename = manifest backup taken, new manifest not yet in place: no manifest on disk."""
    old = first_result(svc, env)
    FakeEngine.hdr = 3.0
    with monkeypatch.context() as mp:
        crash_at(mp, nth)
        with pytest.raises(SystemExit):
            run(svc, replace_job(env))
    if nth == 14:
        assert not (env.out / key_job.MANIFEST_NAME).exists()
    (env.out / key_job.LOCK_NAME).unlink(missing_ok=True)
    FakeEngine.hdr = None
    if mode == "new":  # recovery first, then the normal policy: the restored old result blocks a "new"
        err = rejected(svc, env.job(job_id="job-after001"))
        assert err.code == "exists" and "already holds a result" in err.message
        assert contents(env.out) == old
    elif mode == "resume":
        _, done = run(svc, env.job(job_id="job-after002", mode="resume"))
        assert done["outcome"] == "complete" and done["skipped"] == 3 and contents(env.out) == old
    else:
        _, done = run(svc, replace_job(env, job_id="job-after003"))
        assert done["outcome"] == "complete" and manifest(env)["replaces"] == "job-old00001"
    assert not (env.out / key_job.REPLACE_JOURNAL).exists()
    assert not [p for p in contents(env.out) if "/.ck-" in p]


def test_journal_cannot_target_unlisted_files(svc, env):
    old = first_result(svc, env)
    victim = env.out / "Fg" / "notes.txt"
    victim.write_text("precious")
    jid = "job-evil0001"
    ent = {"final": str(victim), "new": str(env.out / "Fg" / f".ck-{jid}-x.txt"),
           "bak": str(env.out / "Fg" / f".ck-old-{jid}-x.txt"), "had_old": True}
    Path(ent["bak"]).write_text("planted")
    (env.out / key_job.REPLACE_JOURNAL).write_text(json.dumps(
        {"schema": 1, "job_id": jid, "phase": "prepare", "items": [ent]}))
    with pytest.raises(KeyJobRejected) as e:
        key_job.recover_replace(str(env.out))
    assert e.value.code == "exists" and victim.read_text() == "precious"
    # and a Key job in any mode is refused too, with the folder untouched
    assert rejected(svc, replace_job(env, job_id="job-after004")).code == "exists"
    assert victim.read_text() == "precious" and Path(ent["bak"]).read_text() == "planted"
