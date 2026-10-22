"""Key job: strict parsing, range confinement, no-clobber publication, resume, lease (fake engine)."""
from __future__ import annotations

import builtins
import copy
import dataclasses
import errno
import json
import os
import sys
import threading
import time
import types
from pathlib import Path

import cv2
import numpy as np
import pytest

from backend.hint_keyframes import save_hint_keyframes
from backend.service import CorridorKeyService, InferenceParams, OutputConfig, key_job
from backend.service.key_job import KeyJob, KeyJobRejected, run_key_job
from tests.test_v2_outputs import make_runtime

FIXTURES = Path(__file__).parent / "fixtures" / "worker_protocol"
N = 6
STEMS = [f"shot_{1001 + i}" for i in range(N)]
A, B = 1, 3  # range of the standard job: shot_1002..shot_1004


class FakeEngine:
    instances: list = []
    profile = (4, 2, False)
    settings = False  # write a runtime settings txt
    hdr = None  # float written to the runtime fg EXR as FLOAT32 (e.g. 70000.5)

    def __init__(self, *a, **k):
        self.loaded, self.calls, self.on_run = False, [], None
        FakeEngine.instances.append(self)

    is_loaded = property(lambda self: self.loaded)

    def load(self, on_status=None):
        self.loaded = True

    def unload(self):
        self.loaded = False

    def run_clip(self, input_path, output_dir, params, *, outputs, temp_dir, frame_dir_fps,
                 hint_video_path, ffmpeg_bin, on_progress, check_cancel, on_status,
                 initial_hint_path=None, initial_hint_index=0, hint_frame_indices=None):
        listing = sorted(os.listdir(input_path))
        self.calls.append(dict(
            listing=listing, hint=hint_video_path, initial_hint_path=initial_hint_path,
            initial_hint_index=initial_hint_index, hint_frame_indices=hint_frame_indices,
            targets=[os.path.realpath(os.path.join(input_path, f)) for f in listing]))
        os.makedirs(temp_dir, exist_ok=True)
        if on_progress:
            on_progress("stage1", 1, 2)
        if self.on_run:
            self.on_run(check_cancel)
        check_cancel()
        dirs = make_runtime(Path(output_dir), n=len(listing))
        if FakeEngine.hdr is not None:
            for i in range(len(listing)):
                assert cv2.imwrite(os.path.join(dirs["fg"], f"fg_{i:06d}.exr"),
                                   np.full((4, 4, 3), FakeEngine.hdr, np.float32),
                                   [cv2.IMWRITE_EXR_TYPE, cv2.IMWRITE_EXR_TYPE_FLOAT])
        chunk, stride, low = self.profile
        sp = None
        if FakeEngine.settings:
            sp = os.path.join(output_dir, "c_settings.txt")
            Path(sp).write_text("Hann Chunk: 4\n")
        return types.SimpleNamespace(clip_name="c", output_dir=output_dir, product_dirs=dirs,
                                     settings_path=sp, hann_chunk=chunk, hann_stride=stride,
                                     low_vram=low, warnings=())


@pytest.fixture
def svc(tmp_path, monkeypatch):
    FakeEngine.instances, FakeEngine.profile, FakeEngine.hdr = [], (4, 2, False), None
    FakeEngine.settings = False
    monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine", FakeEngine)
    return CorridorKeyService()


class Env:
    """A tiny clip (Frames/, 4x4 PNGs) and a job factory writing to <tmp>/out/v001."""

    def __init__(self, tmp_path):
        self.root = tmp_path / "proj" / "clips" / "clip1"
        self.frames = self.root / "Frames"
        self.frames.mkdir(parents=True)
        for s in STEMS:
            assert cv2.imwrite(str(self.frames / f"{s}.png"), np.full((4, 4, 3), 90, np.uint8))
        self.out = tmp_path / "out" / "v001"
        self.tmp = tmp_path

    def job(self, **over) -> dict:
        base = dict(fg_enabled=True, matte_enabled=True, comp_enabled=False,
                    processed_enabled=False, exr_compression=over.pop("compression", "piz"))
        cfg = OutputConfig(**{**base, **over.pop("cfg", {})})
        d = {
            "job_id": over.pop("job_id", "job-aaaaaaaa"), "clip_id": "proj/clip1",
            "clip_root": str(self.root),
            "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]},
            "params": InferenceParams(**over.pop("params", {})).to_dict(),
            "output_config": cfg.to_dict(), "hint_keyframes_abs": None,
            "outputs": {o: [str(self.out / o.capitalize() / f"{s}.{getattr(cfg, o + '_format')}")
                            for s in STEMS[A:B + 1]] for o in cfg.enabled_outputs},
            "manifest_dir": str(self.out), "mode": "new", "publish": "final", "path_map": [],
            "require": {"protocol": 1, "models": {"corridorkey_v2": None}},
        }
        d.update(over)
        return d


@pytest.fixture
def env(tmp_path):
    return Env(tmp_path)


def run(svc, d, cancel=None, **kw):
    events: list = []
    done = run_key_job(svc, KeyJob.from_dict(d), emit=events.append,
                       cancel=cancel or threading.Event(), **kw)
    return events, done


def manifest(env):
    return json.loads((env.out / ".corridorkey_manifest.json").read_text())


# ------------------------------------------------------------------ strict parsing

BAD = [
    ("unknown key", lambda d: d.update(extra=1), "protocol"),
    ("missing key", lambda d: d.pop("mode"), "protocol"),
    ("bad job id", lambda d: d.update(job_id="../x"), "protocol"),
    ("relative clip root", lambda d: d.update(clip_root="clip"), "paths"),
    ("range order", lambda d: d["range"].update(in_point=5), "range"),
    ("unknown param", lambda d: d["params"].update(bogus=1), "invalid"),
    ("param range", lambda d: d["params"].update(despill_strength=2.0), "invalid"),
    ("non canonical params", lambda d: d["params"].update(use_alpha_hint=True), "invalid"),
    ("marks without mode", lambda d: d.update(hint_keyframes_abs=[2]), "hint"),
    ("wrong count", lambda d: d["outputs"]["fg"].pop(), "paths"),
    ("extra output", lambda d: d["outputs"].update(comp=["/x/a.png"] * 3), "paths"),
    ("traversal", lambda d: d["outputs"]["fg"].__setitem__(0, d["manifest_dir"] + "/../e.exr"), "paths"),
    ("outside manifest dir", lambda d: d["outputs"]["fg"].__setitem__(0, "/elsewhere/a.exr"), "paths"),
    ("wrong extension", lambda d: d["outputs"]["fg"].__setitem__(0, d["manifest_dir"] + "/a.png"), "paths"),
    ("hidden name", lambda d: d["outputs"]["fg"].__setitem__(0, d["manifest_dir"] + "/.a.exr"), "paths"),
    ("duplicate", lambda d: d["outputs"]["fg"].__setitem__(1, d["outputs"]["fg"][0]), "paths"),
    ("staging publish", lambda d: d.update(publish="staging"), "invalid"),
    ("path_map", lambda d: d.update(path_map=[["/a", "/b"]]), "invalid"),
    ("mode", lambda d: d.update(mode="overwrite"), "invalid"),
    ("protocol 2", lambda d: d["require"].update(protocol=2), "invalid"),
]


@pytest.mark.parametrize("name,mutate,code", BAD, ids=[b[0] for b in BAD])
def test_from_dict_rejects(env, name, mutate, code):
    d = env.job()
    mutate(d)
    with pytest.raises(KeyJobRejected) as exc:
        KeyJob.from_dict(d)
    assert exc.value.code == code


def test_snapshot_is_immutable(env):
    d = env.job()
    job = KeyJob.from_dict(d)
    d["outputs"]["fg"][0] = "/changed.exr"
    d["params"]["despill_strength"] = 0.9
    assert job.outputs["fg"][0].endswith(".exr") and "changed" not in job.outputs["fg"][0]
    assert job.params["despill_strength"] == 0.5
    with pytest.raises(dataclasses.FrozenInstanceError):
        job.mode = "resume"
    with pytest.raises(TypeError):
        job.params["despill_strength"] = 1.0
    assert KeyJob.from_dict(job.to_dict()) == job


@pytest.mark.parametrize("name", ["key_new", "key_resume", "legacy_layout", "rejected_paths"])
def test_cross_language_fixtures(name):
    fx = json.loads((FIXTURES / f"{name}.json").read_text())
    job_dict = fx["message"]["job"]
    assert fx["message"]["v"] == 1 and fx["message"]["cmd"] == "key"
    if fx["expect"]["parse"] == "rejected":
        with pytest.raises(KeyJobRejected) as exc:
            KeyJob.from_dict(job_dict)
        assert exc.value.code == fx["expect"]["code"]
        return
    job = KeyJob.from_dict(job_dict)
    assert InferenceParams.from_dict(job_dict["params"]).to_dict() == job_dict["params"]
    assert OutputConfig.from_dict(job_dict["output_config"]).to_dict() == job_dict["output_config"]
    assert job.to_dict() == job_dict  # nothing added, dropped or renamed


def test_events_fixture_is_well_formed():
    fx = json.loads((FIXTURES / "events.json").read_text())
    assert {e["event"] for e in fx["events"]} == {
        "hello", "accepted", "rejected", "status", "progress", "warning", "profile",
        "published", "done"}
    assert all(e["v"] == 1 for e in fx["events"])


# ---------------------------------------------------------------------- happy path

def test_happy_path_confined_to_range_and_manifest_v3(svc, env, monkeypatch):
    out_of_range = [env.frames / f"{s}.png" for s in STEMS[:A] + STEMS[B + 1:]]
    for p in out_of_range:
        p.write_bytes(b"not an image")
    banned = {os.path.realpath(p) for p in out_of_range}
    real_open = builtins.open

    def spy(path, *a, **k):
        if isinstance(path, (str, os.PathLike)) and os.path.realpath(path) in banned:
            raise AssertionError(f"out-of-range read {path}")
        return real_open(path, *a, **k)
    monkeypatch.setattr(builtins, "open", spy)

    events, done = run(svc, env.job(), worker={"host": "h", "worker_rev": "r", "gpu": "g"})
    kinds = [e["event"] for e in events]
    assert kinds[0] == "accepted" and "profile" in kinds and "published" in kinds
    assert done["outcome"] == "complete" and done["manifest_complete"]
    assert (done["published"], done["skipped"], done["expected"]) == (3, 0, 3)
    call = FakeEngine.instances[0].calls[0]
    assert call["targets"] == [os.path.realpath(env.frames / f"{s}.png") for s in STEMS[A:B + 1]]
    published = [s for e in events if e["event"] == "published" for s in e["stems"]]
    assert published == STEMS[A:B + 1]
    m = manifest(env)
    assert m["version"] == 3 and m["complete"] is True and m["clip_id"] == "proj/clip1"
    assert m["range"]["in_stem"] == STEMS[A] and m["frame_range"] == [A, B]
    assert m["source_stems"] == STEMS[A:B + 1] and m["exr_compression"] == "piz"
    assert m["files"]["fg"] == [f"Fg/{s}.exr" for s in STEMS[A:B + 1]]
    assert m["effective_profile"] == {"hann_chunk": 4, "hann_stride": 2, "low_vram": False}
    assert m["worker"] == {"host": "h", "worker_rev": "r", "gpu": "g"}
    assert not (env.out / key_job.LOCK_NAME).exists()
    assert not [p for p in env.out.rglob("*") if ".tmp." in p.name]


def read_exr(path, channel="R"):
    import Imath
    import OpenEXR
    f = OpenEXR.InputFile(str(path))
    h = f.header()
    vals = np.frombuffer(f.channel(channel, Imath.PixelType(Imath.PixelType.FLOAT)), np.float32)
    return h, vals


@pytest.mark.parametrize("comp", ["dwab", "piz", "zip", "none"])
def test_exr_float32_preserved_and_compression_honored(svc, env, comp):
    pytest.importorskip("OpenEXR")
    import Imath
    FakeEngine.hdr = 70000.5  # > half max (65504) and not representable in half
    events, done = run(svc, env.job(compression=comp))
    assert done["outcome"] == "complete"
    m = manifest(env)
    assert m["exr_compression"] == comp and m["exr_pixel_type"] == "float32"
    want = {"dwab": Imath.Compression.DWAB_COMPRESSION, "piz": Imath.Compression.PIZ_COMPRESSION,
            "zip": Imath.Compression.ZIP_COMPRESSION, "none": Imath.Compression.NO_COMPRESSION}[comp]
    for name, expect in (("Fg", 70000.5), ("Matte", 0.25)):
        h, vals = read_exr(env.out / name / f"{STEMS[A]}.exr", "R" if name == "Fg" else "Y")
        assert h["compression"].v == want
        assert all(c.type.v == Imath.PixelType.FLOAT for c in h["channels"].values())
        if comp == "dwab":  # lossy codec: honored, flagged and warned, not claimed exact
            assert np.allclose(vals, min(expect, 65504), rtol=0.1)  # clipped, never exact
        else:
            assert (vals == np.float32(expect)).all()
    assert m["exr_lossy"] is (comp == "dwab")
    assert any(e["event"] == "warning" and "lossy" in e["message"] for e in events) == (comp == "dwab")


def test_png_output_is_srgb_encoded(svc, env):
    _, done = run(svc, env.job(cfg={"comp_enabled": True}))
    assert done["outcome"] == "complete"
    img = cv2.imread(str(env.out / "Comp" / f"{STEMS[A]}.png"), cv2.IMREAD_UNCHANGED)
    assert img.dtype == np.uint8 and abs(int(img[0, 0, 0]) - 188) <= 2  # 0.5 linear -> 0.735


def test_keyframes_rebased_to_range(svc, env):
    save_hint_keyframes(str(env.root), [0, 2, 3, 5], STEMS)
    hint = env.root / "AlphaHint"
    hint.mkdir()
    for s in STEMS:
        cv2.imwrite(str(hint / f"{s}.png"), np.full((4, 4), 200, np.uint8))
    d = env.job(params={"hint_mode": "keyframes"}, hint_keyframes_abs=[2, 3])
    _, done = run(svc, d)
    assert done["outcome"] == "complete"
    assert FakeEngine.instances[0].calls[0]["hint_frame_indices"] == (1, 2)
    assert manifest(env)["hint_keyframes"] == [2, 3]


# ----------------------------------------------------------- rejections write nothing

def snapshot(root):
    return sorted(str(p) for p in Path(root).rglob("*")) if Path(root).exists() else []


def rejected(svc, d):
    with pytest.raises(KeyJobRejected) as exc:
        run(svc, d)
    return exc.value


def test_range_identity_checked_against_disk(svc, env):
    d = env.job()
    d["range"]["in_stem"] = "other"
    assert rejected(svc, d).code == "range" and snapshot(env.out) == []
    d = env.job()
    d["range"].update(out_point=N + 4, out_stem="x")
    d["outputs"] = {"fg": [d["manifest_dir"] + f"/F/{i}.exr" for i in range(N + 4 - A + 1)],
                    "matte": [d["manifest_dir"] + f"/M/{i}.exr" for i in range(N + 4 - A + 1)]}
    assert rejected(svc, d).code == "range"


def test_hint_modes_unsatisfiable(svc, env):
    assert rejected(svc, env.job(params={"hint_mode": "full"})).code == "hint"
    save_hint_keyframes(str(env.root), [0], STEMS)
    (env.root / "AlphaHint").mkdir()
    cv2.imwrite(str(env.root / "AlphaHint" / "x.png"), np.zeros((4, 4), np.uint8))
    d = env.job(params={"hint_mode": "keyframes"}, hint_keyframes_abs=[2])
    assert rejected(svc, d).code == "hint" and snapshot(env.out) == []


def test_existing_target_is_never_overwritten(svc, env):
    target = env.out / "Fg" / f"{STEMS[B]}.exr"
    target.parent.mkdir(parents=True)
    target.write_bytes(b"precious")
    assert rejected(svc, env.job()).code == "exists"
    assert target.read_bytes() == b"precious"
    assert not (env.out / key_job.LOCK_NAME).exists()
    assert not (env.out / key_job.MANIFEST_NAME).exists()


def test_foreign_manifest_blocks(svc, env):
    env.out.mkdir(parents=True)
    (env.out / key_job.MANIFEST_NAME).write_text(json.dumps({"clip_id": "someone-else"}))
    assert rejected(svc, env.job()).code == "exists"
    assert not (env.out / "Fg").exists()


def test_symlinked_subfolder_cannot_escape(svc, env):
    outside = env.tmp / "outside"
    outside.mkdir()
    env.out.mkdir(parents=True)
    os.symlink(outside, env.out / "Fg")
    assert rejected(svc, env.job()).code == "paths"
    assert list(outside.iterdir()) == []


def test_symlinked_manifest_dir_cannot_escape(svc, env):
    outside = env.tmp / "outside"
    outside.mkdir()
    link = env.tmp / "out" / "link"
    link.parent.mkdir(parents=True)
    os.symlink(outside, link)
    d = env.job()
    d["manifest_dir"] = str(link / "v001")
    d["outputs"] = {o: [p.replace(str(env.out), str(link / "v001")) for p in ps]
                    for o, ps in d["outputs"].items()}
    # a symlinked manifest_dir that resolves elsewhere is a legitimate mount; but outputs must
    # still land inside its resolved location and nothing may touch the clip source
    run(svc, d)
    assert not any(env.tmp.joinpath("proj").rglob("*.exr"))


def test_output_overlapping_source_rejected(svc, env):
    d = env.job()
    d["manifest_dir"] = str(env.frames)
    d["outputs"] = {o: [str(env.frames / f"{s}.{'exr'}") for s in STEMS[A:B + 1]]
                    for o in d["outputs"]}
    d["outputs"]["matte"] = [str(env.frames / f"m_{s}.exr") for s in STEMS[A:B + 1]]
    err = rejected(svc, d)
    assert err.code == "paths" and "overlap" in err.message
    assert sorted(p.name for p in env.frames.iterdir()) == [f"{s}.png" for s in STEMS]


def test_output_symlink_to_source_file_rejected(svc, env):
    """A name inside manifest_dir that is a symlink onto a source frame must not be written."""
    d = env.job()
    env.out.mkdir(parents=True)
    (env.out / "Fg").mkdir()
    src = env.frames / f"{STEMS[A]}.png"
    os.symlink(src, env.out / "Fg" / f"{STEMS[A]}.png")
    d["output_config"]["fg_format"] = "png"
    d["outputs"]["fg"] = [str(env.out / "Fg" / f"{s}.png") for s in STEMS[A:B + 1]]
    before = src.read_bytes()
    assert rejected(svc, d).code in ("paths", "exists")
    assert src.read_bytes() == before


# ---------------------------------------------------------------------------- lease

def test_fresh_lock_blocks(svc, env):
    env.out.mkdir(parents=True)
    lock = env.out / key_job.LOCK_NAME
    lock.write_text(json.dumps({"job_id": "other", "host": "render2"}))
    err = rejected(svc, env.job())
    assert err.code == "busy" and "render2" in err.message
    assert lock.exists() and not (env.out / "Fg").exists()


def test_stale_heartbeat_is_not_stolen(svc, env):
    env.out.mkdir(parents=True)
    lock = env.out / key_job.LOCK_NAME
    lock.write_text(json.dumps({"job_id": "other", "host": "render2"}))
    old = time.time() - 3600
    os.utime(lock, (old, old))
    err = rejected(svc, env.job())
    assert err.code == "busy" and "delete" in err.message
    assert json.loads(lock.read_text())["job_id"] == "other"


def test_lock_released_after_failure(svc, env):
    FakeEngine.profile = (4, 2, False)
    orig = FakeEngine.run_clip

    def boom(self, *a, **k):
        raise RuntimeError("CUDA out of memory")
    FakeEngine.run_clip = boom
    try:
        _, done = run(svc, env.job())
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "failed" and done["error_kind"] == "oom"
    assert not (env.out / key_job.LOCK_NAME).exists()
    assert done["manifest"] is None


# ------------------------------------------------------------- cancel, no-clobber, resume

def cancel_after_first_frame(cancel):
    def emit(e):
        if e.get("event") == "progress" and e.get("phase") == "publish":
            cancel.set()
    return emit


def test_cancel_before_publish_writes_no_outputs(svc, env):
    cancel = threading.Event()
    FakeEngine.instances = []
    orig = FakeEngine.run_clip
    FakeEngine.run_clip = lambda self, *a, **k: (cancel.set(), orig(self, *a, **k))[1]
    try:
        _, done = run(svc, env.job(), cancel=cancel)
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "cancelled" and done["manifest_complete"] is False
    assert not list(env.out.rglob("*.exr")) and not (env.out / key_job.LOCK_NAME).exists()


def partial_then_resume_setup(svc, env):
    cancel, events = threading.Event(), []
    job = KeyJob.from_dict(env.job())

    def emit(e):
        events.append(e)
        if e.get("event") == "progress" and e.get("phase") == "publish":
            cancel.set()
    done = run_key_job(svc, job, emit=emit, cancel=cancel)
    return done


def test_cancel_mid_publish_keeps_files_and_manifest_incomplete(svc, env):
    done = partial_then_resume_setup(svc, env)
    assert done["outcome"] == "cancelled" and done["published"] == 1
    assert manifest(env)["complete"] is False
    assert len(list(env.out.rglob("*.exr"))) == 2  # fg + matte of frame 0


def test_resume_publishes_only_missing(svc, env):
    partial_then_resume_setup(svc, env)
    first = env.out / "Fg" / f"{STEMS[A]}.exr"
    before = first.read_bytes()
    mtime = first.stat().st_mtime_ns
    _, done = run(svc, env.job(job_id="job-bbbbbbbb", mode="resume"))
    assert done["outcome"] == "complete" and (done["published"], done["skipped"]) == (2, 1)
    assert first.read_bytes() == before and first.stat().st_mtime_ns == mtime
    m = manifest(env)
    assert m["complete"] is True and m["job_id"] == "job-bbbbbbbb"
    assert len(list(env.out.rglob("*.exr"))) == 6


@pytest.mark.parametrize("change", [
    {"params": {"despill_strength": 0.7}}, {"compression": "none"}, {"cfg": {"fg_format": "png"}}])
def test_resume_incompatible_is_rejected_and_untouched(svc, env, change):
    partial_then_resume_setup(svc, env)
    files = snapshot(env.out)
    err = rejected(svc, env.job(job_id="job-cccccccc", mode="resume", **change))
    assert err.code == "exists" and snapshot(env.out) == files


def test_resume_needs_a_manifest(svc, env):
    assert rejected(svc, env.job(mode="resume")).code == "exists"


def test_resume_with_different_profile_publishes_nothing(svc, env):
    partial_then_resume_setup(svc, env)
    files = snapshot(env.out)
    FakeEngine.profile = (2, 1, True)
    _, done = run(svc, env.job(job_id="job-dddddddd", mode="resume"))
    assert done["outcome"] == "failed" and done["error_kind"] == "input"
    assert "Write as v002" in done["message"] and "4/2" in done["message"]
    assert snapshot(env.out) == [f for f in files if not f.endswith(key_job.LOCK_NAME)]


def test_file_appearing_mid_run_is_not_overwritten(svc, env):
    victim = env.out / "Fg" / f"{STEMS[A + 1]}.exr"

    def on_run(check_cancel):
        victim.parent.mkdir(parents=True, exist_ok=True)
        victim.write_bytes(b"external")
    orig = FakeEngine.run_clip

    def patched(self, *a, **k):
        self.on_run = on_run
        return orig(self, *a, **k)
    FakeEngine.run_clip = patched
    try:
        _, done = run(svc, env.job())
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    assert victim.read_bytes() == b"external" and manifest(env)["complete"] is False


def test_no_hardlink_support_fails_before_engine_without_fallback(svc, env, monkeypatch):
    replaced = []
    real_replace = os.replace
    monkeypatch.setattr(os, "link", lambda *a, **k: (_ for _ in ()).throw(OSError(errno.EPERM, "no")))
    monkeypatch.setattr(os, "replace", lambda s, d, *a, **k: (replaced.append(d), real_replace(s, d))[1])
    err = rejected(svc, env.job())
    assert err.code == "paths" and "overwrite" in err.message
    assert not FakeEngine.instances or not FakeEngine.instances[0].calls  # no model work
    assert not env.out.exists() and replaced == []  # nothing left behind, no replace fallback


def test_link_failure_mid_job_still_fails_without_fallback(svc, env, monkeypatch):
    real_link, calls = os.link, []

    def flaky(src, dst, *a, **k):
        calls.append(dst)
        if ".ck-probe" not in dst and ".corridorkey_manifest" not in dst:
            raise OSError(errno.EXDEV, "cross-device")
        return real_link(src, dst, *a, **k)
    monkeypatch.setattr(os, "link", flaky)
    _, done = run(svc, env.job())
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    assert not list(env.out.rglob("*.exr"))


# ------------------------------------------------- new-mode manifest, temp files, protection

def test_new_mode_refuses_any_existing_manifest(svc, env):
    env.out.mkdir(parents=True)
    mf = env.out / key_job.MANIFEST_NAME
    mf.write_text(json.dumps({"clip_id": "proj/clip1", "files": {"fg": ["old/a.exr"]}}))
    before = mf.read_bytes()
    assert rejected(svc, env.job()).code == "exists"
    assert mf.read_bytes() == before and not (env.out / key_job.LOCK_NAME).exists()


def test_tmp_names_use_full_job_id_and_stay_short(env):
    job = KeyJob.from_dict(env.job(job_id="j" * 64))
    long_final = str(env.out / ("n" * 200 + ".exr"))
    t1, t2 = key_job._tmp_path(long_final, job), key_job._tmp_path(long_final + "x", job)
    assert "j" * 64 in os.path.basename(t1) and t1 != t2 and len(os.path.basename(t1)) < 255
    assert t1.endswith(".exr")


def test_preexisting_tmp_symlink_is_never_written_through(svc, env):
    victim = env.tmp / "victim.txt"
    victim.write_text("keep")
    d = env.job()
    job = KeyJob.from_dict(d)
    final = d["outputs"]["fg"][0]
    os.makedirs(os.path.dirname(final))
    os.symlink(victim, key_job._tmp_path(final, job))
    _, done = run(svc, d)
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    assert victim.read_text() == "keep"
    assert os.path.islink(key_job._tmp_path(final, job))  # not ours, not removed


def test_mask_hint_paths_are_protected(svc, env):
    mask = env.root / "VideoMamaMaskHint"
    mask.mkdir()
    cv2.imwrite(str(mask / "m.png"), np.zeros((4, 4), np.uint8))
    d = env.job()
    d["manifest_dir"] = str(mask / "out")
    d["outputs"] = {o: [p.replace(str(env.out), str(mask / "out")) for p in ps]
                    for o, ps in d["outputs"].items()}
    assert rejected(svc, d).code == "paths"
    assert [p.name for p in mask.iterdir()] == ["m.png"]


# ------------------------------------------------ resume of a complete result; lock schema

def test_resume_of_complete_result_is_a_noop(svc, env):
    run(svc, env.job())
    mf = env.out / key_job.MANIFEST_NAME
    before = mf.read_bytes()
    cancel = threading.Event()
    cancel.set()  # even a pending cancel must not demote a finished result
    _, done = run(svc, env.job(job_id="job-bbbbbbbb", mode="resume"), cancel=cancel)
    assert done["outcome"] == "complete" and (done["published"], done["skipped"]) == (0, 3)
    assert done["manifest_complete"] is True and mf.read_bytes() == before
    assert len(FakeEngine.instances[0].calls) == 1 and not (env.out / key_job.LOCK_NAME).exists()


def test_resume_with_missing_file_reruns_and_completes(svc, env):
    run(svc, env.job())
    (env.out / "Matte" / f"{STEMS[B]}.exr").unlink()
    _, done = run(svc, env.job(job_id="job-bbbbbbbb", mode="resume"))
    assert done["outcome"] == "complete" and (done["published"], done["skipped"]) == (1, 2)


def test_lock_schema_matches_fixture(svc, env):
    seen = {}
    orig = FakeEngine.run_clip

    def patched(self, *a, **k):
        seen.update(json.loads((env.out / key_job.LOCK_NAME).read_text()))
        return orig(self, *a, **k)
    FakeEngine.run_clip = patched
    try:
        run(svc, env.job(), worker={"host": "h", "worker_rev": "r", "gpu": None})
    finally:
        FakeEngine.run_clip = orig
    fx = json.loads((FIXTURES / "lock.json").read_text())
    assert set(seen) == set(fx["required_keys"]) and seen["schema"] == 1
    assert seen["pid"] == os.getpid() and seen["job_id"] == "job-aaaaaaaa"
    assert seen["clip_id"] == "proj/clip1" and seen["host"] == "h" and seen["worker_rev"] == "r"
    assert seen["boot_id"] is None or isinstance(seen["boot_id"], str)
    if sys.platform.startswith("linux"):
        assert isinstance(seen["proc_start_ticks"], int)


def test_manifest_keys_match_fixture(svc, env):
    run(svc, env.job())
    fx = json.loads((FIXTURES / "manifest_v3.json").read_text())
    assert set(manifest(env)) == set(fx["required_keys"])


# ----------------------------------- cross-platform behaviour (Windows is SIMULATED on Linux)

import ntpath  # noqa: E402

WIN_MDIR = "C:\\NAS\\Out\\v001"


def win_job(env, **over):
    d = env.job()
    d["clip_root"] = "\\\\nas\\share\\proj\\clips\\clip1"
    d["manifest_dir"] = over.pop("manifest_dir", WIN_MDIR)
    d["outputs"] = {o: [f"{d['manifest_dir']}\\{o.capitalize()}\\{s}.exr" for s in STEMS[A:B + 1]]
                    for o in d["outputs"]}
    d.update(over)
    return d


@pytest.fixture
def nt(monkeypatch):
    monkeypatch.setattr(key_job, "_P", ntpath)


def test_windows_paths_accepted_and_normalised(nt, env):
    d = win_job(env)
    d["outputs"]["fg"][0] = "c:/nas/out/v001/Fg/" + STEMS[A] + ".exr"  # forward slashes, other case
    job = KeyJob.from_dict(d)
    assert job.outputs["fg"][0] == "c:\\nas\\out\\v001\\Fg\\" + STEMS[A] + ".exr"
    assert job.clip_root.startswith("\\\\nas\\share")


WIN_BAD = [
    ("drive relative", lambda d: d["outputs"]["fg"].__setitem__(0, "C:Out\\a.exr")),
    ("traversal", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\..\\..\\a.exr")),
    ("other drive", lambda d: d["outputs"]["fg"].__setitem__(0, "D:\\NAS\\Out\\v001\\a.exr")),
    ("reserved name", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\CON.exr")),
    ("reserved name with ext", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\nul.txt.exr")),
    ("alternate stream", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\a.exr:evil.exr")),
    ("trailing dot folder", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\x.\\a.exr")),
    ("wildcard", lambda d: d["outputs"]["fg"].__setitem__(0, WIN_MDIR + "\\a*.exr")),
    ("case-insensitive duplicate", lambda d: d["outputs"]["fg"].__setitem__(
        1, d["outputs"]["fg"][0].upper().replace(".EXR", ".exr"))),
    ("upper-case extension is fine only as .EXR==.exr", lambda d: None),  # placeholder: accepted
    ("trailing slash", lambda d: d.update(manifest_dir=WIN_MDIR + "\\")),
    ("posix path", lambda d: d["outputs"]["fg"].__setitem__(0, "/mnt/nas/a.exr")),
    ("rooted without drive", lambda d: d["outputs"]["fg"].__setitem__(0, "\\foo\\a.exr")),
    ("rooted manifest dir", lambda d: d.update(manifest_dir="\\foo")),
    ("forward-slash rooted", lambda d: d.update(clip_root="/foo/clip")),
    ("unc without share", lambda d: d.update(manifest_dir="\\\\server")),
]


@pytest.mark.parametrize("name,mutate", WIN_BAD, ids=[b[0] for b in WIN_BAD])
def test_windows_path_rules(nt, env, name, mutate):
    d = win_job(env)
    mutate(d)
    if name.startswith("upper-case"):
        d["outputs"]["fg"][0] = WIN_MDIR + "\\Fg\\A.EXR"
        KeyJob.from_dict(d)
        return
    with pytest.raises(KeyJobRejected) as exc:
        KeyJob.from_dict(d)
    assert exc.value.code == "paths"


def test_manifest_file_map_uses_forward_slashes(nt, env):
    files = key_job._rel_files(KeyJob.from_dict(win_job(env)))
    assert files["fg"] == [f"Fg/{s}.exr" for s in STEMS[A:B + 1]]


def test_posix_backslash_is_just_a_filename_char(env):
    d = env.job()
    d["outputs"]["fg"][0] = str(env.out / "Fg" / "a\\b.exr")  # legal on POSIX, not a separator
    assert KeyJob.from_dict(d).outputs["fg"][0].endswith("a\\b.exr")
    d["outputs"]["fg"][0] = str(env.out / "Fg") + "\\..\\x.exr"
    with pytest.raises(KeyJobRejected):
        KeyJob.from_dict(d)  # but '..' is refused whichever slash surrounds it


def test_windows_long_path_rejected_before_writing(svc, env, monkeypatch):
    clip, files = key_job._open_clip(KeyJob.from_dict(env.job()))
    monkeypatch.setattr(key_job, "_P", ntpath)  # only the length rule is under test
    job = KeyJob.from_dict(win_job(env, manifest_dir="C:\\" + "d" * 235))
    monkeypatch.setattr(key_job, "_resolve", lambda p: p)
    monkeypatch.setattr(key_job.os.path, "normcase", ntpath.normcase)
    monkeypatch.setattr(key_job.os.path, "islink", lambda p: False)
    with pytest.raises(KeyJobRejected) as exc:
        key_job._check_paths(job, clip, files)
    assert exc.value.code == "paths" and "too long" in exc.value.message


@pytest.mark.parametrize("err", [OSError(errno.EINVAL, "Incorrect function"),
                                 OSError(errno.EACCES, "Access is denied")])
def test_smb_style_link_failure_is_rejected_before_inference(svc, env, monkeypatch, err):
    monkeypatch.setattr(os, "link", lambda *a, **k: (_ for _ in ()).throw(err))
    e = rejected(svc, env.job())
    assert e.code == "paths" and "hard links unavailable" in e.message and "overwrite" in e.message
    assert not FakeEngine.instances or not FakeEngine.instances[0].calls
    assert not env.out.exists()


def test_tmp_files_opened_binary_exclusive_nofollow(env, monkeypatch):
    seen = []
    real = os.open
    monkeypatch.setattr(os, "O_BINARY", 0x40000000, raising=False)  # pretend to be Windows
    monkeypatch.setattr(os, "open", lambda p, flags, *a: (seen.append(flags), real(p, flags & ~0x40000000, *a))[1])
    key_job._write_tmp(str(env.tmp / "t.bin"), b"a\nb")
    assert seen[0] & os.O_EXCL and seen[0] & 0x40000000 and seen[0] & getattr(os, "O_NOFOLLOW", 0) == getattr(os, "O_NOFOLLOW", 0)
    assert (env.tmp / "t.bin").read_bytes() == b"a\nb"


def test_manifest_replace_retries_when_a_reader_holds_it(svc, env, monkeypatch):
    real, fails = os.replace, [2]

    def flaky(src, dst):
        if fails[0] and dst.endswith(key_job.MANIFEST_NAME):
            fails[0] -= 1
            raise PermissionError(errno.EACCES, "sharing violation")
        return real(src, dst)
    monkeypatch.setattr(os, "replace", flaky)
    monkeypatch.setattr(key_job.time, "sleep", lambda s: None)
    _, done = run(svc, env.job())
    assert done["outcome"] == "complete" and fails[0] == 0


def test_scratch_cleanup_failure_does_not_fail_the_job(svc, env, monkeypatch):
    monkeypatch.setattr(key_job.shutil, "rmtree",
                        lambda p, ignore_errors=False, **k: (_ for _ in ()).throw(PermissionError("locked"))
                        if not ignore_errors else None)
    monkeypatch.setattr(key_job.time, "sleep", lambda s: None)
    _, done = run(svc, env.job())
    assert done["outcome"] == "complete"


def test_lock_records_platform(svc, env):
    seen = {}
    orig = FakeEngine.run_clip

    def patched(self, *a, **k):
        seen.update(json.loads((env.out / key_job.LOCK_NAME).read_text()))
        return orig(self, *a, **k)
    FakeEngine.run_clip = patched
    try:
        run(svc, env.job())
    finally:
        FakeEngine.run_clip = orig
    assert seen["platform"] == sys.platform


# ------------------------------------------------ round 2: cleanup ownership and early rejection

def test_output_folder_blocked_by_a_file_is_rejected_cleanly(svc, env):
    env.out.mkdir(parents=True)
    (env.out / "Fg").write_text("a regular file named like the folder")
    err = rejected(svc, env.job())
    assert err.code == "paths" and "Fg" in err.message
    assert (env.out / "Fg").read_text().startswith("a regular")
    assert not (env.out / key_job.LOCK_NAME).exists()
    assert not (env.out / "Matte").exists()


def test_rollback_removes_only_folders_this_job_created(svc, env):
    env.out.mkdir(parents=True)  # pre-existing: must survive
    (env.out / "Matte").write_text("blocker")  # Fg (sorted first) is created, then Matte fails
    assert rejected(svc, env.job()).code == "paths"
    assert env.out.is_dir() and not (env.out / "Fg").exists()
    assert (env.out / "Matte").read_text() == "blocker"


def test_mkdirs_tracks_only_successful_exclusive_mkdir(env, monkeypatch):
    real, created = os.mkdir, []
    target = env.tmp / "base" / "b" / "c"
    (env.tmp / "base").mkdir()

    def racing(p, *a, **k):
        if p.endswith("c"):
            real(p)  # someone else wins the race for the last level
        return real(p, *a, **k)
    monkeypatch.setattr(os, "mkdir", racing)
    key_job._mkdirs(str(target), created)
    assert created == [str(env.tmp / "base" / "b")]
    assert target.is_dir()


def test_lease_heartbeat_is_joined_before_the_lock_is_removed(env, monkeypatch):
    monkeypatch.setattr(key_job, "HEARTBEAT_S", 0.01)
    env.out.mkdir(parents=True)
    lease = key_job._Lease(KeyJob.from_dict(env.job()), {"host": "h"})
    lease.acquire()
    beat, state, real = lease._beat, [], os.remove

    def watch(p, *a, **k):
        if p.endswith(key_job.LOCK_NAME):
            state.append(beat.is_alive())
        return real(p, *a, **k)
    monkeypatch.setattr(os, "remove", watch)
    lease.release()
    assert state == [False] and not (env.out / key_job.LOCK_NAME).exists()


def test_lease_never_removes_a_lock_it_did_not_create(env):
    env.out.mkdir(parents=True)
    lock = env.out / key_job.LOCK_NAME
    lock.write_text(json.dumps({"job_id": "job-aaaaaaaa"}))  # same id, but not created by us
    lease = key_job._Lease(KeyJob.from_dict(env.job()), {})
    with pytest.raises(KeyJobRejected):
        lease.acquire()
    lease.release()
    assert lock.exists()


def test_write_tmp_never_claims_or_deletes_foreign_files(env):
    f = env.tmp / "foreign.tmp"
    f.write_bytes(b"theirs")
    owned = []
    with pytest.raises(key_job.PublishError):
        key_job._write_tmp(str(f), b"mine", owned)
    assert owned == [] and f.read_bytes() == b"theirs"


def test_foreign_probe_files_are_left_alone(svc, env):
    env.out.mkdir(parents=True)
    (env.out / "Fg").mkdir()
    a = env.out / "Fg" / ".ck-probe-job-aaaaaaaa-a"
    a.write_bytes(b"foreign a")
    assert rejected(svc, env.job()).code == "paths" and a.read_bytes() == b"foreign a"
    a.unlink()
    b = env.out / "Fg" / ".ck-probe-job-aaaaaaaa-b"
    b.write_bytes(b"foreign b")
    assert rejected(svc, env.job()).code == "paths" and b.read_bytes() == b"foreign b"
    assert not (env.out / "Fg" / ".ck-probe-job-aaaaaaaa-a").exists()  # our own was removed


def test_foreign_manifest_tmp_is_left_alone(svc, env):
    env.out.mkdir(parents=True)
    t = env.out / f"{key_job.MANIFEST_NAME}.tmp.job-aaaaaaaa"
    t.write_bytes(b"foreign")
    _, done = run(svc, env.job())
    assert done["outcome"] == "failed" and done["error_kind"] == "disk"
    assert t.read_bytes() == b"foreign"
    assert not list(env.out.rglob("*.exr"))


def test_temp_names_match_the_shared_contract(svc, env, monkeypatch):
    import re
    fx = json.loads((FIXTURES / "temp_names.json").read_text())
    pats = [re.compile(v) for v in fx["patterns"].values()]
    seen, real = [], key_job._write_tmp
    monkeypatch.setattr(key_job, "_write_tmp",
                        lambda path, data, owned=None: (seen.append(os.path.basename(path)),
                                                        real(path, data, owned))[1])
    FakeEngine.settings = True
    _, done = run(svc, env.job())
    assert done["outcome"] == "complete" and len(seen) >= 6 + 2 + 1 + 2
    assert all(any(p.match(n) for p in pats) for n in seen), seen
    for kind, rx in fx["patterns"].items():  # every kind really occurs
        assert any(re.match(rx, n) for n in seen), kind
    assert (env.out / fx["fixed_names"]["settings"]).is_file()
    assert not [p for p in env.out.rglob("*") if p.name.startswith(".ck-")]


def test_resume_compares_full_source_stems(svc, env):
    partial_then_resume_setup(svc, env)
    os.rename(env.frames / f"{STEMS[A + 1]}.png", env.frames / f"{STEMS[A + 1]}x.png")  # interior name
    files = snapshot(env.out)
    err = rejected(svc, env.job(job_id="job-cccccccc", mode="resume"))
    assert err.code == "exists" and "source frames" in err.message and snapshot(env.out) == files


def test_pinned_profile_mismatch_rejected_before_inference(svc, env):
    FakeEngine.profile = (8, 4, False)
    cancel = threading.Event()
    job = KeyJob.from_dict(env.job(params={"temporal_window": "8/4"}))

    def emit(e):
        if e.get("event") == "progress" and e.get("phase") == "publish":
            cancel.set()
    assert run_key_job(svc, job, emit=emit, cancel=cancel)["outcome"] == "cancelled"
    mf = env.out / key_job.MANIFEST_NAME
    data = json.loads(mf.read_text())
    assert data["effective_profile"] == {"hann_chunk": 8, "hann_stride": 4, "low_vram": False}
    # a recorded profile that contradicts the pinned window can never be resumed: no GPU needed to know
    data["effective_profile"]["hann_chunk"] = 80
    mf.write_text(json.dumps(data))
    calls = len(FakeEngine.instances[0].calls)
    err = rejected(svc, env.job(job_id="job-dddddddd", mode="resume", params={"temporal_window": "8/4"}))
    assert err.code == "exists" and "pinned" in err.message
    assert len(FakeEngine.instances[0].calls) == calls


# ------------------------------------------- round 3: video hints, lock fencing, post-accept safety

def _alpha_video(env):
    w = cv2.VideoWriter(str(env.root / "AlphaHint.avi"), cv2.VideoWriter_fourcc(*"MJPG"), 5, (16, 16))
    if not w.isOpened():
        pytest.skip("no MJPG writer in this OpenCV build")
    for i in range(N):
        w.write(np.full((16, 16, 3), 20 * i, np.uint8))
    w.release()


@pytest.mark.parametrize("params", [{"hint_mode": "full"}, {"hint_mode": "keyframes"}])
def test_video_alphahint_rejected_before_accept_and_any_write(svc, env, params):
    _alpha_video(env)
    save_hint_keyframes(str(env.root), [2], STEMS)
    d = env.job(params=params, hint_keyframes_abs=[2] if params["hint_mode"] == "keyframes" else None)
    err = rejected(svc, d)
    assert err.code == "hint" and "image sequence" in err.message
    assert snapshot(env.out) == [] and not FakeEngine.instances


def test_video_alphahint_ignored_when_hints_are_off(svc, env):
    _alpha_video(env)
    _, done = run(svc, env.job())
    assert done["outcome"] == "complete"


def _steal_lock(env, owner="job-other"):
    lock = env.out / key_job.LOCK_NAME
    lock.unlink()
    lock.write_text(json.dumps({"job_id": owner, "host": "elsewhere"}))
    return lock


def test_lost_lock_stops_the_job_before_any_output(svc, env):
    orig = FakeEngine.run_clip

    def patched(self, *a, **k):
        _steal_lock(env)  # e.g. the lock was removed because this worker was judged dead
        return orig(self, *a, **k)
    FakeEngine.run_clip = patched
    try:
        _, done = run(svc, env.job())
    finally:
        FakeEngine.run_clip = orig
    assert done["outcome"] == "failed" and "lock" in done["message"]
    assert not list(env.out.rglob("*.exr")) and not (env.out / key_job.MANIFEST_NAME).exists()
    assert json.loads((env.out / key_job.LOCK_NAME).read_text())["job_id"] == "job-other"  # not removed


def test_lock_lost_midway_stops_publishing_and_keeps_the_other_lock(svc, env):
    job, steal = KeyJob.from_dict(env.job()), []

    def emit(e):
        if e.get("event") == "progress" and e.get("phase") == "publish" and not steal:
            steal.append(_steal_lock(env))
    done = run_key_job(svc, job, emit=emit, cancel=threading.Event())
    assert done["outcome"] == "failed" and "lock" in done["message"]
    assert done["published"] == 1  # frame 0 only; frames 1-2 never written
    assert len(list(env.out.rglob("*.exr"))) == 2
    assert json.loads(steal[0].read_text())["job_id"] == "job-other"


def test_manifest_replace_is_fenced_by_the_lock(env):
    env.out.mkdir(parents=True)
    job = KeyJob.from_dict(env.job())
    lease = key_job._Lease(job, {"host": "h"})
    lease.acquire()
    mf = env.out / key_job.MANIFEST_NAME
    mf.write_text('{"keep": true}')
    _steal_lock(env)
    with pytest.raises(key_job.PublishError, match="lock"):
        key_job._put_manifest(str(mf), {"new": 1}, job, replace_ok=True, guard=lease.verify)
    assert json.loads(mf.read_text()) == {"keep": True}
    assert not [p for p in env.out.iterdir() if ".tmp." in p.name]
    lease.release()


def test_failing_emit_after_accept_still_ends_in_done_and_releases_the_lock(svc, env):
    def emit(e):
        if e["event"] == "accepted":
            raise RuntimeError("pipe closed")
    done = run_key_job(svc, KeyJob.from_dict(env.job()), emit=emit, cancel=threading.Event())
    assert done["outcome"] == "failed" and "pipe closed" in done["message"]
    assert not (env.out / key_job.LOCK_NAME).exists() and not list(env.out.rglob("*.exr"))


def test_settings_parse_failure_is_rejected_before_accept_and_leaks_nothing(svc, env, monkeypatch):
    job, events = KeyJob.from_dict(env.job()), []
    monkeypatch.setattr(key_job.InferenceParams, "from_dict",
                        classmethod(lambda cls, d: (_ for _ in ()).throw(RuntimeError("boom"))))
    with pytest.raises(KeyJobRejected) as exc:
        run_key_job(svc, job, emit=events.append, cancel=threading.Event())
    assert exc.value.code == "invalid" and not events
    assert not env.out.exists()
