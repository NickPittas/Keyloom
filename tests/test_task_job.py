"""Generalized worker tasks (hint/SAM/export/normalize) with fake engines and real tiny media."""
from __future__ import annotations

import io
import json
import os
import subprocess
import threading
import types
from pathlib import Path

import cv2
import numpy as np
import pytest

from backend.service import CorridorKeyService
from backend.service import task_job
from backend.service.task_job import TaskJob, TaskRejected, run_task_job, task_capabilities
from backend.worker import Worker

N = 6
STEMS = [f"shot_{1001 + i}" for i in range(N)]
A, B = 1, 3
W, H = 8, 6


def png(path, value=90, shape=(H, W, 3)):
    assert cv2.imwrite(str(path), np.full(shape, value, np.uint8))


class Env:
    def __init__(self, tmp):
        self.tmp = tmp
        self.root = tmp / "proj" / "clip1"
        self.frames = self.root / "Frames"
        self.frames.mkdir(parents=True)
        for s in STEMS:
            png(self.frames / f"{s}.png")
        self.stg = tmp / "staging" / "j1"

    def job(self, kind, params=None, *, job_id="job-1", rng=True, staging=True, publish=None, dest=None, **over):
        dst = dest or {"sam_track": "VideoMamaMaskHint"}.get(kind, "AlphaHint")
        hint = kind in task_job.HINT_DST
        d = {"job_id": job_id, "kind": kind, "clip_id": "proj/clip1", "clip_root": str(self.root),
             "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]} if rng else None,
             "params": params or {}, "staging_dir": str(self.stg) if (hint or kind == "sam_preview") and staging else None,
             "publish_dir": str(self.root / dst) if hint else None,
             "require": {"protocol": 1, "models": {}}}
        d.update(over)
        return d

    def old_hint(self, name="AlphaHint", value=10):
        d = self.root / name
        d.mkdir()
        for s in STEMS:
            png(d / f"{s}.png", value, (H, W))
        return d


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(tmp_path / "scratch"))
    (tmp_path / "scratch").mkdir()
    return Env(tmp_path)


@pytest.fixture
def svc():
    return CorridorKeyService()


def run(svc, d, cancel=None):
    events: list = []
    done = run_task_job(svc, TaskJob.from_dict(d), emit=events.append, cancel=cancel or threading.Event())
    return events, done


def alpha_files(d):
    return {f.name: f.read_bytes() for f in Path(d).iterdir() if f.suffix == ".png"}


def fake_hint(value, stems=None, drop=None):
    """run_* replacement writing AlphaHint for the VIEW's frames only."""
    seen = []

    def fn(clip, *a, **k):
        names = [os.path.splitext(f)[0] for f in clip.input_asset.get_frame_files()]
        seen.append(names)
        out = Path(clip.root_path) / "AlphaHint"
        out.mkdir()
        for s in (stems or names):
            if s != drop:
                png(out / f"{s}.png", value, (H, W))
        if k.get("on_progress"):
            k["on_progress"](clip.name, len(names), len(names))
    fn.seen = seen
    return fn


# ------------------------------------------------------------------ parsing

@pytest.mark.parametrize("edit,code", [
    (lambda d: d.update(extra=1), "protocol"),
    (lambda d: d.update(kind="nope"), "unsupported"),
    (lambda d: d.update(range=None), "range"),
    (lambda d: d["params"].update(x=1), "invalid"),
    (lambda d: d.update(publish_dir=None), "paths"),
    (lambda d: d.update(staging_dir="rel"), "paths"),
    (lambda d: d.update(require={"protocol": 2, "models": {}}), "invalid"),
])
def test_rejects_malformed(env, edit, code):
    d = env.job("gvm")
    edit(d)
    with pytest.raises(TaskRejected) as e:
        TaskJob.from_dict(d)
    assert e.value.code == code


def test_whole_movie_actions_have_no_range(env):
    d = env.job("prepare_source", {"source_path": str(env.tmp / "a.mov")}, rng=True)
    with pytest.raises(TaskRejected):
        TaskJob.from_dict(d)
    assert TaskJob.from_dict(env.job("prepare_source", {"source_path": str(env.tmp / "a.mov")}, rng=False)).range is None


def test_capabilities_truthful():
    st = {"artifacts": [{"artifact": "gvm", "ready": True, "depends": ["dep"]}, {"artifact": "dep", "ready": False},
                        {"artifact": "sam2-package", "ready": True}, {"artifact": "sam2-small", "ready": True},
                        {"artifact": "birefnet-runtime", "ready": True}, {"artifact": "birefnet-BiRefNet", "ready": True}]}
    c = task_capabilities(st, gpu=True, ffmpeg=False, repos={"General": "BiRefNet", "Portrait": "BiRefNet-portrait"})
    t = c["tasks"]
    assert not t["gvm"] and t["birefnet"] and c["birefnet_variants"] == ["General"]
    assert t["sam_track"] and c["sam_models"] == ["small"] and not t["videomama"] and not t["export_video"]
    nogpu = task_capabilities(st, gpu=False, ffmpeg=True, repos={})["tasks"]
    assert nogpu["chroma"] and not nogpu["sam_preview"] and nogpu["export_video"]
    assert not task_capabilities(None, gpu=True, ffmpeg=True)["tasks"]["gvm"]


# -------------------------------------------------------------- hint publication

def test_gvm_is_range_confined_and_merges(env, svc, monkeypatch):
    old = env.old_hint()
    fake = fake_hint(200)
    monkeypatch.setattr(svc, "run_gvm", fake)
    events, done = run(svc, env.job("gvm"))
    assert done["outcome"] == "complete" and done["kind"] == "gvm", done
    assert fake.seen == [STEMS[A:B + 1]]  # engine saw exactly In..Out
    r = done["result"]
    assert (r["published"], r["expected"], r["complete"]) == (3, 3, True)
    files = alpha_files(old)
    assert set(files) == {f"{s}.png" for s in STEMS}
    got = {s: cv2.imread(str(old / f"{s}.png"), cv2.IMREAD_UNCHANGED).max() for s in STEMS}
    assert [got[s] for s in STEMS] == [10, 200, 200, 200, 10, 10]  # outside range kept
    prov = json.loads((old / ".corridorkey_hint.json").read_text())
    assert prov["kind"] == "gvm" and prov["range"]["in_stem"] == STEMS[A]
    assert not list(env.root.glob("AlphaHint.*")) and not (env.root / ".corridorkey_hint.lock").exists()
    assert not (env.stg / "work-job-1").exists()
    assert [e["event"] for e in events][0] == "accepted" and any(e["event"] == "progress" for e in events)


@pytest.mark.parametrize("how", ["raise", "cancel", "short", "size"])
def test_failure_or_cancel_keeps_old_hint(env, svc, monkeypatch, how):
    old = env.old_hint()
    before = alpha_files(old)
    cancel = threading.Event()

    def engine(clip, *a, **k):
        if how == "raise":
            raise RuntimeError("CUDA out of memory")
        out = Path(clip.root_path) / "AlphaHint"
        out.mkdir()
        for f in clip.input_asset.get_frame_files():
            s = os.path.splitext(f)[0]
            if how == "short" and s == STEMS[B]:
                continue
            png(out / f"{s}.png", 200, (H + 1, W) if how == "size" and s == STEMS[B] else (H, W))
        if how == "cancel":
            cancel.set()
    monkeypatch.setattr(svc, "run_gvm", engine)
    events, done = run(svc, env.job("gvm"), cancel)
    assert done["outcome"] == ("cancelled" if how == "cancel" else "failed"), done
    if how == "raise":
        assert done["error_kind"] == "oom"
    assert alpha_files(old) == before
    assert not list(env.root.glob("AlphaHint.*")) and not (env.root / ".corridorkey_hint.lock").exists()


def test_publish_over_missing_dir_and_unowned_staging_preserved(env, svc, monkeypatch):
    env.stg.mkdir(parents=True)
    (env.stg / "inputs").mkdir()
    keep = env.stg / "inputs" / "user.txt"
    keep.write_text("mine")
    monkeypatch.setattr(svc, "run_birefnet", fake_hint(150))
    monkeypatch.setattr(task_job, "birefnet_repos", lambda: {"General": "BiRefNet"})
    _, done = run(svc, env.job("birefnet", {"variant": "General"}))
    assert done["outcome"] == "complete", done
    assert len(alpha_files(env.root / "AlphaHint")) == 3  # no old dir: just the range
    assert keep.read_text() == "mine"


def test_publish_dir_must_be_the_live_hint_folder(env):
    d = env.job("gvm", publish_dir=str(env.tmp / "elsewhere"))
    with pytest.raises(TaskRejected) as e:
        run_task_job(None, TaskJob.from_dict(d), emit=lambda m: None, cancel=threading.Event())
    assert e.value.code == "paths" and not (env.root / ".corridorkey_hint.lock").exists()


def test_hint_lock_is_not_stolen(env, svc):
    (env.root / ".corridorkey_hint.lock").write_text(json.dumps({"job_id": "other"}))
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("gvm"))
    assert e.value.code == "busy" and (env.root / ".corridorkey_hint.lock").exists()


def test_interrupted_publish_is_recovered(env, svc, monkeypatch):
    old = env.old_hint()
    os.rename(old, env.root / "AlphaHint.prev-crashed")  # crash between the two renames
    monkeypatch.setattr(svc, "run_gvm", fake_hint(200))
    _, done = run(svc, env.job("gvm"))
    assert done["outcome"] == "complete"
    assert len(alpha_files(env.root / "AlphaHint")) == N  # old frames restored then merged


def test_chroma_real_cpu(env, svc):
    for s in STEMS:
        img = np.zeros((60, 80, 3), np.uint8)
        img[:, :] = (20, 200, 20)  # green screen
        img[20:40, 20:40] = (200, 60, 60)
        assert cv2.imwrite(str(env.frames / f"{s}.png"), cv2.cvtColor(img, cv2.COLOR_RGB2BGR))
    p = {"screen_color": [20, 200, 20], "screen_type": "green", "strength": 1.0, "clip_black": 0.0,
         "clip_white": 1.0, "shrink_grow": 0, "edge_blur": 0}
    _, done = run(svc, env.job("chroma", p))
    assert done["outcome"] == "complete", done
    m = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_UNCHANGED)
    assert m.shape == (60, 80) and m[30, 30] == 255 and m[0, 0] == 0


# --------------------------------------------------------------- guided kinds

def masks(env, stems):
    d = env.root / "VideoMamaMaskHint"
    d.mkdir(exist_ok=True)
    for s in stems:
        png(d / f"{s}.png", 255, (H, W))


def test_videomama_needs_every_mask_and_passes_chunk(env, svc, monkeypatch):
    masks(env, STEMS[A:B])  # last range frame missing
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("videomama", {"chunk_size": 4}))
    assert e.value.code == "invalid" and STEMS[B] in e.value.message
    masks(env, STEMS)
    seen = {}

    def eng(clip, **k):
        seen["chunk"], seen["masks"] = k["chunk_size"], sorted(clip.mask_asset.get_frame_files())
        fake_hint(180)(clip)
    monkeypatch.setattr(svc, "run_videomama", eng)
    _, done = run(svc, env.job("videomama", {"chunk_size": 4}, job_id="job-2"))
    assert done["outcome"] == "complete", done
    assert seen == {"chunk": 4, "masks": [f"{s}.png" for s in STEMS[A:B + 1]]}


def test_matanyone2_seeds_from_in_frame(env, svc, monkeypatch):
    with pytest.raises(TaskRejected):
        run(svc, env.job("matanyone2", {"mask_stem": None}))  # no mask yet
    masks(env, [STEMS[A]])
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("matanyone2", {"mask_stem": STEMS[B]}, job_id="job-2"))
    assert e.value.code == "invalid"
    monkeypatch.setattr(svc, "run_matanyone2", lambda clip, **k: fake_hint(170)(clip))
    _, done = run(svc, env.job("matanyone2", {"mask_stem": STEMS[A]}, job_id="job-3"))
    assert done["outcome"] == "complete"


class FakeTracker:
    model_id = "fake"

    def __init__(self):
        self.calls = []

    def track_video(self, frames, prompts, *, on_progress=None, on_status=None, check_cancel=None):
        self.calls.append((len(frames), prompts))
        return [np.full(f.shape[:2], 255 if i % 2 else 0, np.uint8) for i, f in enumerate(frames)]


def sam_svc(svc, monkeypatch):
    tr = FakeTracker()
    monkeypatch.setattr(svc, "_get_sam2_tracker", lambda **k: tr)
    monkeypatch.setattr(svc, "set_sam2_model", lambda m: tr.__dict__.setdefault("model", m))
    return tr


def test_sam_preview_writes_only_owned_preview(env, svc, monkeypatch):
    tr = sam_svc(svc, monkeypatch)
    (env.stg / "inputs").mkdir(parents=True)
    png(env.stg / "inputs" / "m.png", 255, (H, W))
    (env.stg / "unowned.txt").write_text("x")
    p = {"frame_stem": STEMS[B], "tracker_model": "large",
         "prompts": {"points": [{"x": 2, "y": 3, "label": 1}, {"x": 5, "y": 1, "label": 0}],
                     "boxes": [[1, 1, 6, 5]], "mask_png": "m.png"}}
    events, done = run(svc, env.job("sam_preview", p, publish=None))
    assert done["outcome"] == "complete", done
    pv = next(e for e in events if e["event"] == "preview")
    assert pv["frame_stem"] == STEMS[B] and (pv["width"], pv["height"]) == (W, H)
    assert os.path.isfile(pv["mask_png"]) and os.path.dirname(pv["mask_png"]) == str(env.stg)
    n, prompts = tr.calls[0]
    assert n == 1 and prompts[0].frame_index == 0 and prompts[0].positive_points == [(2, 3)]
    assert prompts[0].negative_points == [(5, 1)] and prompts[0].box == (1, 1, 6, 5) and prompts[0].mask.any()
    assert tr.model == "facebook/sam2.1-hiera-large"
    assert not (env.root / "AlphaHint").exists() and not (env.root / "VideoMamaMaskHint").exists()
    assert (env.stg / "unowned.txt").exists() and not (env.stg / "work-job-1").exists()


@pytest.mark.parametrize("bad,code", [
    ({"points": [{"x": 99, "y": 1, "label": 1}]}, "invalid"),
    ({"points": [{"x": 1, "y": 1, "label": 0}]}, "invalid"),
    ({"mask_png": "../escape.png"}, "invalid"),
    ({"mask_png": "absent.png"}, "paths"),
])
def test_sam_prompt_validation(env, svc, monkeypatch, bad, code):
    sam_svc(svc, monkeypatch)
    (env.stg / "inputs").mkdir(parents=True)
    p = {"frame_stem": STEMS[A], "tracker_model": "small", "prompts": bad}
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("sam_preview", p))
    assert e.value.code == code


def test_sam_track_publishes_masks_keeps_alpha(env, svc, monkeypatch):
    tr = sam_svc(svc, monkeypatch)
    alpha = env.old_hint("AlphaHint")
    before = alpha_files(alpha)
    p = {"tracker_model": "base_plus", "frames": {STEMS[A]: {"points": [{"x": 2, "y": 2, "label": 1}]}}}
    with pytest.raises(TaskRejected) as e:  # prompt frame outside the range
        run(svc, env.job("sam_track", {**p, "frames": {STEMS[0]: p["frames"][STEMS[A]]}}))
    assert e.value.code == "range"
    _, done = run(svc, env.job("sam_track", p, job_id="job-2"))
    assert done["outcome"] == "complete", done
    n, prompts = tr.calls[0]
    assert n == 3 and prompts[0].frame_index == 0  # local index inside the range
    assert len(alpha_files(env.root / "VideoMamaMaskHint")) == 3
    assert alpha_files(alpha) == before
    man = json.loads((env.root / ".corridorkey_mask_manifest.json").read_text())
    assert man["source"] == "sam2" and man["frame_stems"] == STEMS[A:B + 1]
    from backend.clip_state import mask_sequence_is_videomama_ready
    assert mask_sequence_is_videomama_ready(str(env.root))


# ------------------------------------------- producer -> guide -> consumer (central workflow)

@pytest.mark.parametrize("destination", ["AlphaHint", "VideoMamaMaskHint"])
def test_generated_guide_keeps_gray_values_like_a_direct_hint(env, svc, monkeypatch, destination):
    ramp = np.tile(np.array([0, 1, 10, 11, 64, 128, 254, 255], dtype=np.uint8), (H, 1))

    def engine(clip, **kwargs):
        folder = Path(clip.root_path) / "AlphaHint"
        folder.mkdir()
        for name in clip.input_asset.get_frame_files():
            assert cv2.imwrite(str(folder / name), ramp)

    monkeypatch.setattr(svc, "run_gvm", engine)
    _, done = run(svc, env.job("gvm", {}, dest=destination))
    assert done["outcome"] == "complete", done
    expected = ramp  # the guide is published as produced; consumers threshold at 10 when they load it
    for stem in STEMS[A:B + 1]:
        actual = cv2.imread(str(env.root / destination / f"{stem}.png"), cv2.IMREAD_GRAYSCALE)
        np.testing.assert_array_equal(actual, expected)

CHROMA = {"screen_color": [20, 200, 20], "screen_type": "green", "strength": 1.0, "clip_black": 0.0,
          "clip_white": 1.0, "shrink_grow": 0, "edge_blur": 0}


def producer_job(env, svc, monkeypatch, kind, dest, value=200, job_id="prod"):
    """Run one basic producer (engines faked at the service boundary) publishing to ``dest``."""
    params = {"gvm": {}, "birefnet": {"variant": "v"}, "chroma": CHROMA,
              "sam_track": {"tracker_model": "small", "frames": {STEMS[A]: {"points": [{"x": 2, "y": 2, "label": 1}]}}}}[kind]
    if kind == "gvm":
        monkeypatch.setattr(svc, "run_gvm", fake_hint(value))
    elif kind == "birefnet":
        monkeypatch.setattr(task_job, "birefnet_repos", lambda: {"v": "r"})
        monkeypatch.setattr(svc, "run_birefnet", lambda clip, **k: fake_hint(value)(clip, **k))
    elif kind == "sam_track":
        sam_svc(svc, monkeypatch)
    return run(svc, env.job(kind, params, dest=dest, job_id=job_id))[1]


@pytest.mark.parametrize("kind", ["gvm", "birefnet", "chroma", "sam_track"])
def test_producer_final_hint_goes_to_alphahint_only(env, svc, monkeypatch, kind):
    done = producer_job(env, svc, monkeypatch, kind, "AlphaHint")
    assert done["outcome"] == "complete" and done["result"]["hint_dir"] == str(env.root / "AlphaHint"), done
    assert len(alpha_files(env.root / "AlphaHint")) == 3
    assert not (env.root / "VideoMamaMaskHint").exists() and not (env.root / ".corridorkey_mask_manifest.json").exists()


@pytest.mark.parametrize("consumer", ["matanyone2", "videomama"])
@pytest.mark.parametrize("kind", ["gvm", "birefnet", "chroma", "sam_track"])
def test_producer_guide_feeds_consumer_final_alpha(env, svc, monkeypatch, kind, consumer):
    from backend.clip_state import mask_sequence_is_videomama_ready
    alpha = env.old_hint("AlphaHint")
    before = alpha_files(alpha)
    done = producer_job(env, svc, monkeypatch, kind, "VideoMamaMaskHint")
    assert done["outcome"] == "complete", done
    guide = env.root / "VideoMamaMaskHint"
    assert sorted(alpha_files(guide)) == [f"{s}.png" for s in STEMS[A:B + 1]]
    assert alpha_files(alpha) == before  # a guide never touches the final hint
    man = json.loads((env.root / ".corridorkey_mask_manifest.json").read_text())
    assert man["source"] == ("sam2" if kind == "sam_track" else kind) and man["frame_stems"] == STEMS[A:B + 1]
    assert mask_sequence_is_videomama_ready(str(env.root))
    guide_before, seen = alpha_files(guide), {}

    def eng(clip, **k):  # what the real engines read: first mask file (MatAnyone2) / stem-matched masks (VideoMaMa)
        seen["masks"] = sorted(clip.mask_asset.get_frame_files())
        fake_hint(77)(clip)
    monkeypatch.setattr(svc, "run_" + consumer, eng)
    params = {"matanyone2": {"mask_stem": STEMS[A]}, "videomama": {"chunk_size": 4}}[consumer]
    _, done = run(svc, env.job(consumer, params, job_id="cons"))
    assert done["outcome"] == "complete", done
    assert seen["masks"] == [f"{s}.png" for s in (STEMS[A:A + 1] if consumer == "matanyone2" else STEMS[A:B + 1])]
    assert cv2.imread(str(alpha / f"{STEMS[A]}.png"), cv2.IMREAD_UNCHANGED).max() == 77
    assert cv2.imread(str(alpha / f"{STEMS[0]}.png"), cv2.IMREAD_UNCHANGED).max() == 10  # outside range kept
    assert alpha_files(guide) == guide_before  # consumers leave the guide alone


@pytest.mark.parametrize("consumer", ["matanyone2", "videomama"])
def test_consumers_without_guide_fail_and_only_make_alphahint(env, svc, monkeypatch, consumer):
    called = []
    monkeypatch.setattr(svc, "run_" + consumer, lambda clip, **k: called.append(1))
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job(consumer, {}))
    assert e.value.code == "invalid" and not called and not (env.root / ".corridorkey_hint.lock").exists()
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job(consumer, {}, dest="VideoMamaMaskHint", job_id="j2"))
    assert e.value.code == "paths"


@pytest.mark.parametrize("kind", ["gvm", "chroma", "sam_track"])
def test_producer_destination_is_one_of_the_two_known_folders(env, svc, kind):
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job(kind, {} if kind == "gvm" else CHROMA if kind == "chroma" else
                         {"tracker_model": "small", "frames": {STEMS[A]: {"points": [{"x": 1, "y": 1, "label": 1}]}}},
                         publish_dir=str(env.root / "Elsewhere")))
    assert e.value.code == "paths" and not (env.root / ".corridorkey_hint.lock").exists()


def test_failed_guide_producer_keeps_old_guide_and_manifest(env, svc, monkeypatch):
    masks(env, STEMS)
    before = alpha_files(env.root / "VideoMamaMaskHint")
    monkeypatch.setattr(svc, "run_gvm", fake_hint(200, drop=STEMS[B]))
    _, done = run(svc, env.job("gvm", {}, dest="VideoMamaMaskHint"))
    assert done["outcome"] == "failed"
    assert alpha_files(env.root / "VideoMamaMaskHint") == before
    assert not (env.root / ".corridorkey_mask_manifest.json").exists()


def test_keyframe_only_guide_seeds_matanyone2(env, svc, monkeypatch):
    """A one-frame range (a keyframe) is a valid, merged guide for MatAnyone2 seeded at that frame."""
    monkeypatch.setattr(svc, "run_gvm", fake_hint(200))
    d = env.job("gvm", {}, dest="VideoMamaMaskHint", rng=False)
    d["range"] = {"in_point": A, "out_point": A, "in_stem": STEMS[A], "out_stem": STEMS[A]}
    assert run(svc, d)[1]["outcome"] == "complete"
    assert sorted(alpha_files(env.root / "VideoMamaMaskHint")) == [f"{STEMS[A]}.png"]
    monkeypatch.setattr(svc, "run_matanyone2", lambda clip, **k: fake_hint(170)(clip))
    assert run(svc, env.job("matanyone2", {"mask_stem": STEMS[A]}, job_id="c"))[1]["outcome"] == "complete"
    with pytest.raises(TaskRejected):  # VideoMaMa needs every range frame
        run(svc, env.job("videomama", {}, job_id="c2"))


# ------------------------------------------------------------------- export

def make_result(env, ext="png", complete=True, skip=None, size=(H, W)):
    out = env.tmp / "out" / "v001"
    files = {}
    for prod in ("fg", "processed"):
        (out / prod.capitalize()).mkdir(parents=True)
        files[prod] = []
        for i in range(A, B + 1):
            rel = f"{prod.capitalize()}/{STEMS[i]}.{ext}"
            files[prod].append(rel)
            if rel != skip:
                png(out / rel, 120 + i, (*size, 4) if prod == "processed" else (*size, 3))
    m = {"version": 3, "engine": "corridorkey-v2", "clip_id": "proj/clip1", "range": {"in_point": A, "out_point": B,
         "in_stem": STEMS[A], "out_stem": STEMS[B]}, "source_stems": STEMS[A:B + 1], "files": files, "complete": complete}
    (out / ".corridorkey_manifest.json").write_text(json.dumps(m))
    return out


def export_job(env, out, **over):
    p = {"product": "fg", "manifest_dir": str(out), "dest": str(env.tmp / "deliver" / "a.mp4"), "container": "mp4",
         "fps": 24, "allow_partial": False}
    p.update(over)
    return env.job("export_video", p, staging=False)


@pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")
def test_export_real_mp4_and_mov(env, svc):
    out = make_result(env)
    events, done = run(svc, export_job(env, out))
    assert done["outcome"] == "complete", done
    dest = env.tmp / "deliver" / "a.mp4"
    assert dest.is_file() and not list((env.tmp / "deliver").glob("*.part*"))
    assert done["result"]["frames"] == 3 and done["result"]["audio"] == "none"
    assert any("Audio" in e.get("message", "") for e in events if e["event"] == "warning")
    cap = cv2.VideoCapture(str(dest))
    assert int(cap.get(cv2.CAP_PROP_FRAME_COUNT)) == 3
    with pytest.raises(TaskRejected) as e:  # existing deliverable survives
        run(svc, export_job(env, out), None)
    assert e.value.code == "exists" and dest.is_file()
    _, done = run(svc, export_job(env, out, product="processed", container="mov_prores4444",
                                  dest=str(env.tmp / "deliver" / "b.mov")))
    assert done["outcome"] == "complete" and done["result"]["alpha"] is True, done


def test_export_rejects_gaps_and_alpha_misuse(env, svc):
    out = make_result(env, skip=f"Fg/{STEMS[2]}.png")
    with pytest.raises(TaskRejected) as e:
        run(svc, export_job(env, out))
    assert e.value.code == "invalid" and "gap" in e.value.message
    with pytest.raises(TaskRejected):
        run(svc, export_job(env, out, container="webm_alpha", dest=str(env.tmp / "x.webm")))
    out2 = env.tmp / "o2"
    out2.mkdir()
    bad = export_job(env, out)
    bad["params"]["audio"] = "source"
    with pytest.raises(TaskRejected) as e:
        TaskJob.from_dict(bad)
    assert e.value.code == "unsupported"


def test_export_incomplete_needs_allow_partial(env, svc):
    out = make_result(env, complete=False)
    with pytest.raises(TaskRejected):
        run(svc, export_job(env, out))
    job = TaskJob.from_dict(export_job(env, out, allow_partial=True))
    assert task_job._preflight(job).partial is True


# ------------------------------------------------------------ real normalizers

@pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")
def test_prepare_source_then_import_mask(env, svc):
    root = env.tmp / "proj" / "clip2"
    root.mkdir()
    mov, hint = env.tmp / "src.mp4", env.tmp / "hint.mp4"

    def ffm(*a):
        subprocess.run(["ffmpeg", "-v", "error", "-y", *a], check=True, timeout=10)
    ffm("-f", "lavfi", "-i", "testsrc=size=64x48:rate=25:duration=0.4", "-pix_fmt", "yuv420p", str(mov))
    ffm("-f", "lavfi", "-i", "color=c=white:size=64x48:rate=25:duration=0.4", "-pix_fmt", "gray", "-c:v", "libx264", str(hint))
    base = dict(clip_id="proj/clip2", clip_root=str(root))
    events, done = run(svc, env.job("prepare_source", {"source_path": str(mov)}, rng=False, **base))
    assert done["outcome"] == "complete", done
    assert done["result"]["frames"] == 10 and (root / "Frames").is_dir()
    p = {"hint_path": str(hint), "source_path": str(mov), "hint_kind": "mask", "channel": "gray"}
    _, done = run(svc, env.job("import_hint", p, rng=False, job_id="job-2", **base))
    assert done["outcome"] == "complete", done
    assert len(list((root / "VideoMamaMaskHint").glob("*.png"))) == 10
    assert json.loads((root / ".corridorkey_mask_manifest.json").read_text())["source"] == "imported"
    _, again = run(svc, env.job("import_hint", p, rng=False, job_id="job-3", **base))
    assert again["outcome"] == "complete"  # idempotent, not destructive


# --------------------------------------------------------------- worker dispatch

def worker_with(tasks, svc, **caps):
    out = io.StringIO()
    hello = {"host": "h", "worker_rev": "r", "gpu": {"name": "g"}, "models": {},
             "capabilities": {"key": False, "tasks": tasks, **caps}}
    return Worker(svc, out, hello), out


def lines(out):
    return [json.loads(x) for x in out.getvalue().splitlines()]


def test_worker_dispatches_task_and_rejects_unavailable(env, svc, monkeypatch):
    monkeypatch.setattr(svc, "run_gvm", fake_hint(222))
    w, out = worker_with({"gvm": True, "videomama": False}, svc)
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("gvm")})
    w._thread.join(10)
    ev = lines(out)
    assert [e["event"] for e in ev if e["event"] in ("accepted", "done")] == ["accepted", "done"]
    assert ev[-1]["outcome"] == "complete" and ev[-1]["kind"] == "gvm"
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("videomama", {}, job_id="job-2")})
    rej = lines(out)[-1]
    assert rej["event"] == "rejected" and rej["code"] == "model_missing"
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("gvm")})  # same job_id again
    assert lines(out)[-1]["code"] == "busy"


def test_worker_serves_one_job_at_a_time(env, svc, monkeypatch):
    gate = threading.Event()

    def slow(clip, **k):
        gate.wait(5)
        fake_hint(1)(clip)
    monkeypatch.setattr(svc, "run_gvm", slow)
    w, out = worker_with({"gvm": True}, svc)
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("gvm")})
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("gvm", job_id="job-2")})
    assert lines(out)[-1]["code"] == "busy"
    gate.set()
    w._thread.join(10)


# ------------------------------------------------- review fixes (B1 B2 B3 M1 M2)

def test_main_detects_device_for_default_service_only(monkeypatch):
    from backend import worker
    made = []

    class Svc:
        def __init__(self):
            made.append(self)
            self.detected = []

        def detect_device(self):
            self.detected.append(os.environ.get("CUDA_VISIBLE_DEVICES"))

    monkeypatch.setattr("backend.service.CorridorKeyService", Svc)
    hello = {"host": "h", "worker_rev": "r", "gpu": None, "models": {}, "capabilities": {"key": False}}
    worker.main(["--gpu", "1"], stdin=io.BytesIO(b""), out=io.StringIO(), hello=hello)
    assert made[0].detected == ["1"]  # after CUDA_VISIBLE_DEVICES was set
    injected = Svc()
    injected.detected.clear()
    worker.main([], stdin=io.BytesIO(b""), out=io.StringIO(), service=injected, hello=hello)
    assert injected.detected == []


def test_gvm_dotted_stems_survive():
    pytest.importorskip("av")
    pytest.importorskip("torchvision")
    from gvm_core.gvm.utils.inference_utils import ImageSequenceWriter
    import torch
    d = Path(os.environ.get("TMPDIR", "/tmp")) / f"gvmw-{os.getpid()}"
    w = ImageSequenceWriter(str(d), "png")
    w.write(torch.zeros(1, 3, 4, 4), ["shot.0001.png"])
    try:
        assert sorted(os.listdir(d)) == ["shot.0001.png"]
    finally:
        import shutil
        shutil.rmtree(d, ignore_errors=True)
    assert ".split('.')[0]" not in Path("gvm_core/wrapper.py").read_text(encoding="utf-8") if Path("gvm_core/wrapper.py").exists() else True


def test_export_matte_exr_has_no_srgb_curve(env, svc):
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = env.tmp / "out" / "v001"
    (out / "Matte").mkdir(parents=True)
    rel = []
    for i in range(A, B + 1):
        r = f"Matte/{STEMS[i]}.exr"
        rel.append(r)
        _write_exr_float32(str(out / r), np.full((H, W), 0.5, np.float32), "zip")
    m = {"version": 3, "engine": "corridorkey-v2", "clip_id": "proj/clip1", "range": {"in_point": A, "out_point": B},
         "source_stems": STEMS[A:B + 1], "files": {"matte": rel}, "complete": True}
    (out / ".corridorkey_manifest.json").write_text(json.dumps(m))
    _, done = run(svc, export_job(env, out, product="matte", dest=str(env.tmp / "deliver" / "m.mp4")))
    assert done["outcome"] == "complete", done
    cap = cv2.VideoCapture(str(env.tmp / "deliver" / "m.mp4"))
    ok, frame = cap.read()
    assert ok and 100 <= int(frame[:, :, 0].mean()) <= 150  # linear 0.5 ~ 125; with the sRGB curve ~ 188


def test_prev_restored_at_preflight_before_engine(env, svc, monkeypatch):
    old = env.old_hint()
    os.rename(old, env.root / "AlphaHint.prev-crashed")
    seen = {}

    def eng(clip, **k):
        seen["restored"] = (env.root / "AlphaHint").is_dir()
        raise RuntimeError("engine failed")
    monkeypatch.setattr(svc, "run_gvm", eng)
    _, done = run(svc, env.job("gvm"))
    assert seen["restored"] and done["outcome"] == "failed"
    assert len(alpha_files(env.root / "AlphaHint")) == N  # prior result back even though the job failed


def write_lock(env, **over):
    import socket
    meta = {"schema": 1, "job_id": "dead-job", "host": socket.gethostname(), "platform": os.sys.platform,
            "pid": 2 ** 22 + 12345, "boot_id": None, "proc_start_ticks": None, "pid_ns": task_job._pid_ns(),
            "started": 1.0}
    meta.update(over)
    (env.root / ".corridorkey_hint.lock").write_text(json.dumps(meta))
    return meta


def lock_cmd(env, svc, **msg):
    w, out = worker_with({"gvm": True}, svc)
    w.hello["host"] = __import__("socket").gethostname()
    w._handle_lock_cmd({"v": 1, "request_id": "r1", "clip_root": str(env.root), **msg})
    return lines(out)[-1]


def test_busy_rejection_carries_lock_identity(env, svc):
    meta = write_lock(env)
    w, out = worker_with({"gvm": True}, svc)
    w._handle_task({"v": 1, "cmd": "task", "job": env.job("gvm")})
    w._thread.join(10)
    rej = lines(out)[-1]
    assert rej["code"] == "busy" and rej["lock"]["job_id"] == "dead-job"
    assert rej["lock"]["pid"] == meta["pid"] and rej["lock"]["local_dead"] is True and rej["lock"]["safe_to_clear"]


def test_clear_stale_lock_with_proof_then_job_runs(env, svc, monkeypatch):
    meta = write_lock(env)
    old = env.old_hint()
    os.rename(old, env.root / "AlphaHint.prev-crashed")
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="observed_exit",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_cleared" and ev["recovered"] == [str(env.root / "AlphaHint")]
    assert not (env.root / ".corridorkey_hint.lock").exists() and len(alpha_files(env.root / "AlphaHint")) == N
    monkeypatch.setattr(svc, "run_gvm", fake_hint(7))
    _, done = run(svc, env.job("gvm"))
    assert done["outcome"] == "complete"


@pytest.mark.parametrize("case", ["mismatch", "alive", "other_host_observed", "symlink", "garbage"])
def test_clear_lock_refusals_leave_lock(env, svc, case):
    import socket
    meta = write_lock(env)
    expect = {"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]}
    reason = "observed_exit"
    lock = env.root / ".corridorkey_hint.lock"
    if case == "mismatch":
        expect["job_id"] = "someone-else"
    elif case == "alive":
        write_lock(env, pid=os.getpid(), proc_start_ticks=None)
        expect["pid"] = os.getpid()
    elif case == "other_host_observed":
        write_lock(env, host="nas-box")
        expect["host"] = "nas-box"
    elif case == "symlink":
        lock.unlink()
        (env.tmp / "elsewhere").write_text("{}")
        lock.symlink_to(env.tmp / "elsewhere")
    elif case == "garbage":
        lock.write_text("not json")
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason=reason, expect=expect)
    assert ev["event"] == "hint_lock_refused", ev
    assert os.path.lexists(lock)
    assert ev["code"] == {"mismatch": "mismatch", "alive": "alive", "other_host_observed": "unproven",
                          "symlink": "unsafe", "garbage": "unsafe"}[case]


def test_orphan_on_other_host_needs_explicit_confirmation(env, svc):
    meta = write_lock(env, host="nas-box")
    expect = {"job_id": "dead-job", "host": "nas-box", "pid": meta["pid"]}
    ev = lock_cmd(env, svc, cmd="inspect_hint_lock")
    assert ev["event"] == "hint_lock" and ev["lock"]["same_host"] is False and not ev["lock"]["safe_to_clear"]
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="user_confirmed_orphan", expect=expect)
    assert ev["event"] == "hint_lock_cleared"
    assert not (env.root / ".corridorkey_hint.lock").exists()


# ------------------------------------------- second review: lock race, recovery safety

def _race_rename(monkeypatch, env, newer_after=False):
    """Replace the lock with a live writer's between inspect and rename."""
    real = os.rename
    new_meta = {"schema": 1, "job_id": "live-job", "host": __import__("socket").gethostname(),
                "platform": os.sys.platform, "pid": os.getpid(), "boot_id": None, "proc_start_ticks": None,
                "pid_ns": task_job._pid_ns(), "started": 2.0}

    def rename(src, dst, *a, **k):
        if str(src).endswith(".corridorkey_hint.lock") and ".cleared-" in str(dst):
            monkeypatch.setattr(os, "rename", real)  # once
            tmp = str(src) + ".racetmp"
            Path(tmp).write_text(json.dumps(new_meta))
            os.replace(tmp, src)  # the replacement lands just before our rename
            real(src, dst, *a, **k)
            if newer_after:  # and yet another owner takes the vacated path
                Path(src).write_text(json.dumps({**new_meta, "job_id": "third"}))
            return
        return real(src, dst, *a, **k)
    monkeypatch.setattr(os, "rename", rename)
    return new_meta


def test_clear_lock_race_restores_replacement(env, svc, monkeypatch):
    meta = write_lock(env)
    live = _race_rename(monkeypatch, env)
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="observed_exit",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_refused" and ev["code"] == "mismatch", ev
    kept = json.loads((env.root / ".corridorkey_hint.lock").read_text())
    assert kept["job_id"] == "live-job" and kept["started"] == live["started"]  # restored byte-for-byte owner
    assert not list(env.root.glob(".corridorkey_hint.lock.cleared-*"))


def test_clear_lock_race_never_overwrites_a_newer_owner(env, svc, monkeypatch):
    meta = write_lock(env)
    _race_rename(monkeypatch, env, newer_after=True)
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="observed_exit",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_refused" and ev["code"] == "mismatch"
    assert json.loads((env.root / ".corridorkey_hint.lock").read_text())["job_id"] == "third"
    kept = list(env.root.glob(".corridorkey_hint.lock.cleared-*"))
    assert len(kept) == 1 and json.loads(kept[0].read_text())["job_id"] == "live-job"  # displaced lock kept


def test_recovery_oserror_becomes_warning(env, svc, monkeypatch):
    meta = write_lock(env)

    def boom(final):
        raise PermissionError("denied")
    monkeypatch.setattr(task_job, "_recover_prev", boom)
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="observed_exit",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_cleared" and "denied" in ev["warning"]
    assert not (env.root / ".corridorkey_hint.lock").exists()


def test_other_pid_namespace_is_no_proof(env, svc):
    meta = write_lock(env, pid_ns="pid:[4026539999]")
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="observed_exit",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_refused" and ev["code"] == "unproven"
    assert ev["lock"]["same_pid_namespace"] is False
    ev = lock_cmd(env, svc, cmd="clear_hint_lock", reason="user_confirmed_orphan",
                  expect={"job_id": "dead-job", "host": meta["host"], "pid": meta["pid"]})
    assert ev["event"] == "hint_lock_cleared"


def test_recover_prev_only_touches_what_it_owns(env):
    final = str(env.root / "AlphaHint")
    for n in ("a", "b"):  # two candidates and no final: ambiguous, never guessed
        (env.root / f"AlphaHint.prev-{n}").mkdir()
    task_job._recover_prev(final)
    assert not os.path.lexists(final) and len(list(env.root.glob("AlphaHint.prev-*"))) == 2
    (env.root / "AlphaHint.prev-b").rename(env.root / "elsewhere")
    task_job._recover_prev(final)
    assert os.path.isdir(final)  # exactly one: restored
    (env.root / "AlphaHint.prev-zzz").mkdir()
    (env.root / "AlphaHint.prev-done").mkdir()
    (env.root / "AlphaHint" / ".corridorkey_hint.json").write_text(json.dumps({"job_id": "done"}))
    (env.root / "AlphaHint.new-x").mkdir()
    task_job._recover_prev(final)
    assert not (env.root / "AlphaHint.prev-done").exists()  # its publish completed
    assert (env.root / "AlphaHint.prev-zzz").exists()  # unknown owner: kept
    assert not (env.root / "AlphaHint.new-x").exists()


def test_destination_lock_closes_after_fcntl_unavailable():
    """Late finalisation (interpreter shutdown) must not import fcntl."""
    code = (
        "import sys, tempfile, os\n"
        "from backend.model_lock import DestinationLock\n"
        "d = tempfile.mkdtemp()\n"
        "lock = DestinationLock(os.path.join(d, 'x')).acquire()\n"
        "sys.modules['fcntl'] = None\n"  # makes `import fcntl` raise ImportError
        "lock.close()\n"
        "print('closed')\n")
    r = subprocess.run([os.sys.executable, "-c", code], capture_output=True, text=True, timeout=10,
                       cwd=str(Path(__file__).resolve().parent.parent),
                       env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parent.parent)})
    assert r.stdout.strip() == "closed" and "ModuleNotFoundError" not in r.stderr and r.returncode == 0, r.stderr


# ------------------------------------------------------ third review: lock ownership fencing

def _replace_lock(env, job_id="usurper"):
    lock = env.root / ".corridorkey_hint.lock"
    tmp = env.root / ".lock.tmp"
    tmp.write_text(json.dumps({"schema": 1, "job_id": job_id, "host": "h", "pid": 1}))
    os.replace(tmp, lock)  # a new file (new inode) takes the path


def test_release_never_removes_a_replaced_lock(env):
    job = types.SimpleNamespace(job_id="mine", clip_root=str(env.root))
    lk = task_job._HintLock(job)
    lk.acquire()
    assert lk.owned()
    _replace_lock(env)
    assert not lk.owned()
    with pytest.raises(task_job.PublishError):
        lk.verify()
    lk.release()
    assert json.loads((env.root / ".corridorkey_hint.lock").read_text())["job_id"] == "usurper"


def test_publication_after_lost_lock_keeps_old_hints(env, svc, monkeypatch):
    old = env.old_hint()
    before = alpha_files(old)

    def eng(clip, **k):
        fake_hint(200)(clip)
        _replace_lock(env)  # someone cleared this writer as dead and took the lock mid-job
    monkeypatch.setattr(svc, "run_gvm", eng)
    _, done = run(svc, env.job("gvm"))
    assert done["outcome"] == "failed" and "lock" in done["message"].lower(), done
    assert alpha_files(old) == before and not list(env.root.glob("AlphaHint.*"))
    assert json.loads((env.root / ".corridorkey_hint.lock").read_text())["job_id"] == "usurper"


def test_lock_lost_between_moves_restores_old_folder(env, monkeypatch):
    old = env.old_hint()
    before = alpha_files(old)
    job = types.SimpleNamespace(job_id="mine", clip_root=str(env.root))
    staged = env.tmp / "staged"
    staged.mkdir()
    for s in STEMS[A:B + 1]:
        png(staged / f"{s}.png", 200, (H, W))
    calls = []

    def guard():
        calls.append(1)
        if len(calls) == 2:  # the check after final -> .prev
            raise task_job.PublishError("lock lost")
    with pytest.raises(task_job.PublishError):
        task_job.publish_hint_dir(str(staged), str(old), STEMS[A:B + 1], job, {}, guard=guard)
    assert alpha_files(old) == before and not list(env.root.glob("AlphaHint.*"))


def test_win_liveness_is_unknown(monkeypatch):
    monkeypatch.setattr(task_job.sys, "platform", "win32")
    assert task_job._pid_alive(os.getpid()) is None


# ------------------------------------------- real Key-style EXR export (acceptance regression)

def key_style_result(env, tmp_name="keyout", fmt_comp="png", compression="dwab", size=(H, W)):
    """Float32 EXR FG/Matte/Processed + PNG Comp exactly as run_key_job writes them."""
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = env.tmp / tmp_name
    files = {}
    for prod, d in (("fg", "FG"), ("matte", "Matte"), ("comp", "Comp"), ("processed", "Processed")):
        (out / d).mkdir(parents=True)
        files[prod] = []
        for i in range(A, B + 1):
            ext = "png" if prod == "comp" else "exr"
            rel = f"{d}/{STEMS[i]}.{ext}"
            files[prod].append(rel)
            if prod == "fg":
                _write_exr_float32(str(out / rel), np.full((*size, 3), 0.2, np.float32), compression)
            elif prod == "matte":
                _write_exr_float32(str(out / rel), np.full(size, 0.5, np.float32), compression)
            elif prod == "processed":
                px = np.full((*size, 4), 0.2, np.float32)
                px[:, :, 3] = 0.5
                _write_exr_float32(str(out / rel), px, compression)
            else:
                png(out / rel, 100)
    m = {"version": 3, "engine": "corridorkey-v2", "clip_id": "proj/clip1",
         "range": {"in_point": A, "out_point": B, "in_stem": STEMS[A], "out_stem": STEMS[B]},
         "source_stems": STEMS[A:B + 1], "files": files, "complete": True,
         "formats": {"fg": "exr", "matte": "exr", "comp": fmt_comp, "processed": "exr"}}
    (out / ".corridorkey_manifest.json").write_text(json.dumps(m))
    return out


def decode(path, pix="rgba", dec=()):
    r = subprocess.run(["ffmpeg", "-v", "error", *dec, "-i", str(path), "-f", "rawvideo", "-pix_fmt", pix, "-"],
                       capture_output=True, timeout=10)
    assert r.returncode == 0, r.stderr.decode()
    n = {"rgba": 4, "gray": 1}[pix]
    return np.frombuffer(r.stdout, np.uint8).reshape(-1, H, W, n)


@pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")
@pytest.mark.parametrize("product,container,expect_rgb,expect_a", [
    ("fg", "mp4", 123, None),              # linear 0.2 -> sRGB ~0.484
    ("matte", "mp4", 125, None),           # data: 0.5 as-is (limited range ~125), no curve
    ("comp", "mp4", 100, None),            # PNG already display-encoded
    ("processed", "mp4", 123, None),       # alpha dropped, colour still correct
    ("processed", "mov_prores4444", 123, 127),
    ("processed", "webm_alpha", 123, 127),
])
def test_export_real_key_exr_all_products_and_containers(env, svc, product, container, expect_rgb, expect_a):
    out = key_style_result(env)
    dest = env.tmp / "deliver" / f"{product}{CONTAINER_EXT[container]}"
    events, done = run(svc, export_job(env, out, product=product, container=container, dest=str(dest)))
    assert done["outcome"] == "complete", done
    assert done["result"]["frames"] == 3
    assert done["result"]["alpha"] is (container != "mp4" and product == "processed")
    dec = ("-c:v", "libvpx-vp9") if container == "webm_alpha" else ()
    px = decode(dest, "rgba", dec)
    assert px.shape[0] == 3
    centre = px[:, H // 2, W // 2, :].astype(int)
    assert all(abs(c[:3] - expect_rgb).max() <= 6 for c in centre), centre
    if expect_a is not None:
        assert all(abs(c[3] - expect_a) <= 4 for c in centre), centre
    else:
        assert all(c[3] == 255 for c in centre)
    assert not list((env.tmp / "deliver").glob("*.part*"))
    with pytest.raises(TaskRejected) as e:  # an existing deliverable is never replaced
        run(svc, {**export_job(env, out, product=product, container=container, dest=str(dest)),
                  "job_id": "job-again"})
    assert e.value.code == "exists"


CONTAINER_EXT = {"mp4": ".mp4", "mov_prores4444": ".mov", "webm_alpha": ".webm"}


@pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")
def test_export_failure_reports_ffmpeg_error_and_keeps_nothing(env, svc, monkeypatch):
    out = key_style_result(env)
    real = subprocess.Popen

    def bad(cmd, *a, **k):  # an invalid filter makes FFmpeg print a real error
        cmd = [x if not str(x).startswith("scale=in_range") else "nosuchfilter" for x in cmd]
        return real(cmd, *a, **k)
    monkeypatch.setattr(task_job.subprocess, "Popen", bad)
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "x.mp4")))
    assert done["outcome"] == "failed" and "nosuchfilter" in done["message"].lower()
    assert not (env.tmp / "deliver" / "x.mp4").exists() and not list((env.tmp / "deliver").glob("*.part*"))


@pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")
def test_export_cancel_leaves_no_part_file(env, svc):
    out = key_style_result(env)
    cancel = threading.Event()
    cancel.set()
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "c.mp4")), cancel)
    assert done["outcome"] == "cancelled"
    assert not list((env.tmp / "deliver").glob("c.mp4*"))


# ---------------------------------------------- supervised export: hung consumer, bad frames, cleanup

def fake_ffmpeg(tmp, monkeypatch, body="exec sleep 300"):
    """An 'ffmpeg' that creates its output file, never reads stdin and never exits by itself."""
    script = tmp / "fake-ffmpeg.sh"
    script.write_text('#!/bin/sh\nfor a; do last=$a; done\n: > "$last"\n' + body + "\n")
    script.chmod(0o755)
    monkeypatch.setattr("backend.ffmpeg_tools.discovery.require_ffmpeg_install",
                        lambda require_probe=True: types.SimpleNamespace(ffmpeg_path=str(script)))
    procs = []
    real = subprocess.Popen

    def spy(cmd, *a, **k):
        proc = real(cmd, *a, **k)
        procs.append(proc)
        return proc
    monkeypatch.setattr(task_job.subprocess, "Popen", spy)
    return procs


def export_threads():
    return [t for t in threading.enumerate() if t.name.startswith("export-") and t.is_alive()]


def assert_cleaned(env, procs, name):
    assert procs and all(p.poll() is not None for p in procs)  # child reaped
    assert not export_threads()                                # helper threads joined
    assert not list((env.tmp / "deliver").glob(f"{name}*"))    # no .part, no deliverable


def cancel_after(delay):
    ev = threading.Event()
    threading.Timer(delay, ev.set).start()
    return ev


def test_export_cancel_while_ffmpeg_consumer_is_hung_mid_stream(env, svc, monkeypatch):
    pytest.importorskip("OpenEXR")
    out = key_style_result(env, size=(512, 512), compression="zip")  # frames far larger than the pipe buffer
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    t0 = __import__("time").monotonic()
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "h.mp4")), cancel_after(0.7))
    assert done["outcome"] == "cancelled", done
    assert __import__("time").monotonic() - t0 < 10  # blocked pipe write did not hold the job hostage
    assert_cleaned(env, procs, "h.mp4")


def test_export_cancel_when_ffmpeg_hangs_after_all_frames(env, svc, monkeypatch):
    out = key_style_result(env)  # tiny frames: every write succeeds, then FFmpeg never finishes
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "h2.mp4")), cancel_after(0.7))
    assert done["outcome"] == "cancelled", done
    assert_cleaned(env, procs, "h2.mp4")


def test_export_bad_second_frame_names_it_and_reaps_child(env, svc, monkeypatch):
    out = key_style_result(env)
    (out / "FG" / f"{STEMS[A + 1]}.exr").write_bytes(b"not an exr")
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "b.mp4")))
    assert done["outcome"] == "failed" and "frame 2/3" in done["message"] and STEMS[A + 1] in done["message"], done
    assert_cleaned(env, procs, "b.mp4")


def test_export_wrong_size_frame_is_named(env, svc, monkeypatch):
    pytest.importorskip("OpenEXR")
    from backend.service.key_job import _write_exr_float32
    out = key_style_result(env)
    _write_exr_float32(str(out / "FG" / f"{STEMS[B]}.exr"), np.full((H + 2, W, 3), 0.2, np.float32), "zip")
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "s.mp4")))
    assert done["outcome"] == "failed" and "frame 3/3" in done["message"].lower(), done
    assert_cleaned(env, procs, "s.mp4")


def test_export_progress_callback_failure_is_reported_and_cleaned(env, svc, monkeypatch):
    out = key_style_result(env)
    procs = fake_ffmpeg(env.tmp, monkeypatch)
    events = []

    def emit(m):
        if m.get("event") == "progress" and m.get("phase") == "export_video":
            raise RuntimeError("UI pipe closed")
        events.append(m)
    done = run_task_job(svc, TaskJob.from_dict(export_job(env, out, dest=str(env.tmp / "deliver" / "c2.mp4"))),
                        emit=emit, cancel=threading.Event())
    assert done["outcome"] == "failed" and "UI pipe closed" in done["message"], done
    assert_cleaned(env, procs, "c2.mp4")


def test_export_ffmpeg_dying_early_reports_its_error(env, svc, monkeypatch):
    out = key_style_result(env)
    procs = fake_ffmpeg(env.tmp, monkeypatch, body='echo "boom: bad filter" >&2; exit 3')
    _, done = run(svc, export_job(env, out, dest=str(env.tmp / "deliver" / "d.mp4")))
    assert done["outcome"] == "failed" and "boom: bad filter" in done["message"], done
    assert_cleaned(env, procs, "d.mp4")


# ------------------------------------------------------------------ mask cleanup

def _blob():
    m = np.zeros((H, W), np.uint8)
    m[2:4, 2:4] = 255
    return m


def test_cleanup_grow_shrink_hole_feather_known_pixels():
    c = task_job.cleanup_mask
    m = _blob()
    assert c(m, {}) is m and c(m, {"grow": 0, "fill_holes": False, "feather": 0.0}) is m
    g = c(m, {"grow": 1})
    assert g.shape == m.shape and g[1:5, 1:5].sum() > m.sum() and g[0, 0] == 0 and g[2, 2] == 255
    assert c(g, {"grow": -1}).tolist() == m.tolist()  # erode undoes dilate on a convex blob
    assert not c(m, {"grow": -1}).any()
    ring = np.zeros((H, W), np.uint8)
    ring[0:5, 1:6] = 200
    ring[2, 3] = 0  # enclosed hole
    ring[5, 0] = 0  # exterior stays exterior
    f = c(ring, {"fill_holes": True})
    assert f[2, 3] == 255 and f[5, 0] == 0 and f[0, 0] == 0
    soft = c(m, {"feather": 1.0})
    assert 0 < soft[1, 2] < 255 and soft.dtype == np.uint8 and soft.shape == m.shape
    assert c(m.astype(np.uint16) * 257, {"grow": 1}).dtype == np.uint16


def _run_gvm_cleanup(env, svc, monkeypatch, cleanup, dest="AlphaHint"):
    def engine(clip, **k):
        out = Path(clip.root_path) / "AlphaHint"
        out.mkdir()
        for n in clip.input_asset.get_frame_files():
            assert cv2.imwrite(str(out / n), _blob())
    monkeypatch.setattr(svc, "run_gvm", engine)
    p = {} if cleanup is None else {"mask_cleanup": cleanup}
    return run(svc, env.job("gvm", p, dest=dest))[1]


@pytest.mark.parametrize("dest", ["AlphaHint", "VideoMamaMaskHint"])
def test_cleanup_changes_the_published_mask(env, svc, monkeypatch, dest):
    done = _run_gvm_cleanup(env, svc, monkeypatch, {"grow": 1, "fill_holes": True, "feather": 0}, dest)
    assert done["outcome"] == "complete", done
    got = cv2.imread(str(env.root / dest / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    assert got.shape == (H, W) and (got > 0).sum() > (_blob() > 0).sum()
    assert json.loads((env.root / dest / ".corridorkey_hint.json").read_text())["params"]["mask_cleanup"]["grow"] == 1 \
        if (env.root / dest / ".corridorkey_hint.json").exists() else True


def test_neutral_or_absent_cleanup_is_identity(env, svc, monkeypatch):
    for c in (None, {}):
        _run_gvm_cleanup(env, svc, monkeypatch, c)
        got = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
        np.testing.assert_array_equal(got, _blob())
        import shutil
        shutil.rmtree(env.root / "AlphaHint")


@pytest.mark.parametrize("bad", [
    {"grow": 101}, {"grow": 1.5}, {"grow": True}, {"feather": -1}, {"feather": float("nan")}, {"feather": 101},
    {"fill_holes": 1}, {"x": 1}, [],
])
def test_cleanup_rejects_bad_params(env, svc, bad):
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("gvm", {"mask_cleanup": bad}))
    assert e.value.code == "invalid"


def test_cleanup_not_accepted_by_export(env):
    with pytest.raises(TaskRejected):
        task_job.TaskJob.from_dict(env.job("prepare_source", {"source_path": "/x.mp4", "mask_cleanup": {}}, rng=False))


def test_cleanup_applies_to_sam_track_and_preview(env, svc, monkeypatch):
    sam_svc(svc, monkeypatch)
    pr = {"points": [{"x": 2, "y": 2, "label": 1}]}
    plain = run(svc, env.job("sam_track", {"tracker_model": "small", "frames": {STEMS[A]: pr}}))[1]
    assert plain["outcome"] == "complete"
    base = cv2.imread(str(env.root / "VideoMamaMaskHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    import shutil
    shutil.rmtree(env.root / "VideoMamaMaskHint")
    done = run(svc, env.job("sam_track", {"tracker_model": "small", "frames": {STEMS[A]: pr},
                                          "mask_cleanup": {"grow": -1}}, job_id="job-2"))[1]
    assert done["outcome"] == "complete"
    cleaned = cv2.imread(str(env.root / "VideoMamaMaskHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    assert cleaned.shape == base.shape and (cleaned > 0).sum() <= (base > 0).sum()
    if base.any() and not base.all():
        assert (cleaned > 0).sum() < (base > 0).sum()
    (env.stg / "inputs").mkdir(parents=True, exist_ok=True)
    ev, d = run(svc, env.job("sam_preview", {"frame_stem": STEMS[A], "tracker_model": "small", "prompts": pr,
                                             "mask_cleanup": {"grow": -1}}, job_id="job-3"))
    assert d["outcome"] == "complete" and d["result"]["fill"] <= float((base > 0).mean())


def test_cancel_during_cleanup_keeps_old_hint(env, svc, monkeypatch):
    old = env.old_hint("AlphaHint")
    before = alpha_files(old)
    cancel = threading.Event()
    real = task_job.cleanup_mask
    monkeypatch.setattr(task_job, "cleanup_mask", lambda *a: (cancel.set(), real(*a))[1])
    monkeypatch.setattr(svc, "run_gvm", fake_hint(255))
    _, done = run(svc, env.job("gvm", {"mask_cleanup": {"grow": 1}}), cancel)
    assert done["outcome"] == "cancelled" and alpha_files(old) == before


# ------------------------------------------- stage policy: each cleanup operation applies once along the chain

def test_stage_cleanup_policy():
    c = {"grow": -2, "fill_holes": True, "feather": 5.0}
    sc = task_job.stage_cleanup
    assert sc("gvm", "/x/AlphaHint", c) == c
    assert sc("gvm", "/x/VideoMamaMaskHint", c) == {**c, "feather": 0.0}
    assert sc("sam_track", "/x/VideoMamaMaskHint", c) == {**c, "feather": 0.0}
    assert sc("sam_preview", None, c) == {**c, "feather": 0.0}
    for k in ("matanyone2", "videomama"):
        assert sc(k, "/x/AlphaHint", c) == {"grow": 0, "fill_holes": False, "feather": 5.0}
    assert sc("gvm", "/x/AlphaHint", None) == {}
    neutral = {"grow": 0, "fill_holes": False, "feather": 0.0}
    assert sc("matanyone2", "/x/AlphaHint", neutral) == neutral


def test_guide_producer_never_feathers_and_does_not_grow_the_guide(env, svc, monkeypatch):
    done = _run_gvm_cleanup(env, svc, monkeypatch, {"grow": 0, "fill_holes": False, "feather": 5.0}, "VideoMamaMaskHint")
    assert done["outcome"] == "complete", done
    got = cv2.imread(str(env.root / "VideoMamaMaskHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    np.testing.assert_array_equal(got, _blob())  # malformed-stage feather ignored: no softness, no growth


def test_direct_hint_keeps_feather_softness(env, svc, monkeypatch):
    done = _run_gvm_cleanup(env, svc, monkeypatch, {"grow": 0, "fill_holes": False, "feather": 1.0}, "AlphaHint")
    assert done["outcome"] == "complete", done
    got = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    assert ((got > 0) & (got < 255)).any()


@pytest.mark.parametrize("consumer", ["matanyone2", "videomama"])
def test_refiner_only_feathers_never_shrinks_again(env, svc, monkeypatch, consumer):
    done = _run_gvm_cleanup(env, svc, monkeypatch, {"grow": 0, "fill_holes": False, "feather": 0.0}, "VideoMamaMaskHint")
    assert done["outcome"] == "complete", done

    def eng(clip, **k):
        out = Path(clip.root_path) / "AlphaHint"
        out.mkdir(exist_ok=True)
        for n in clip.input_asset.get_frame_files():
            assert cv2.imwrite(str(out / n), _blob())
    monkeypatch.setattr(svc, "run_" + consumer, eng)
    params = {"matanyone2": {"mask_stem": STEMS[A]}, "videomama": {"chunk_size": 4}}[consumer]
    run(svc, env.job(consumer, {**params, "mask_cleanup": {"grow": -1, "fill_holes": True, "feather": 0.0}}, job_id="r1"))
    got = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    np.testing.assert_array_equal(got, _blob())  # grow/fill belong to the guide stage; the refiner left them alone
    run(svc, env.job(consumer, {**params, "mask_cleanup": {"grow": -1, "fill_holes": True, "feather": 1.0}}, job_id="r2"))
    soft = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    assert ((soft > 0) & (soft < 255)).any() and soft[2:4, 2:4].min() > 0


# ----------------------------------------------------- export codec / quality

def _probe(path):
    r = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v:0", "-show_entries",
                        "stream=codec_name,pix_fmt,profile", "-of", "json", str(path)], capture_output=True, timeout=20)
    return json.loads(r.stdout)["streams"][0]


NEEDS_FFMPEG = pytest.mark.skipif(not __import__("shutil").which("ffmpeg"), reason="needs ffmpeg")


@NEEDS_FFMPEG
@pytest.mark.parametrize("product,container,params,codec,pix,quality", [
    ("fg", "mp4", {}, "h264", "yuv420p", 18),                                   # legacy defaults
    ("fg", "mp4", {"codec": "hevc", "quality": 28}, "hevc", "yuv420p", 28),
    ("fg", "mp4", {"codec": "hevc"}, "hevc", "yuv420p", 23),
    ("fg", "mp4", {"codec": "h264", "quality": 0}, "h264", "yuv420p", 0),
    ("fg", "mov_prores4444", {}, "prores", "yuva444p10le", 9),                   # opaque alpha plane for RGB
    ("fg", "mov_prores4444", {"codec": "prores422", "quality": 4}, "prores", "yuv422p10le", 4),
    ("processed", "mov_prores4444", {"codec": "prores4444", "quality": 31}, "prores", "yuva444p10le", 31),
    ("processed", "mov_prores4444", {"codec": "prores422"}, "prores", "yuv422p10le", 9),
    ("fg", "webm_alpha", {"quality": 63}, "vp9", "yuv420p", 63),                  # ffprobe shows alpha as a tag
])
def test_export_codec_matrix_real_ffmpeg(env, svc, product, container, params, codec, pix, quality):
    out = make_result(env, size=(128, 128))  # libx265 refuses tiny frames
    dest = env.tmp / "deliver" / f"v{CONTAINER_EXT[container]}"
    _, done = run(svc, export_job(env, out, product=product, container=container, dest=str(dest), **params))
    assert done["outcome"] == "complete", done
    r = done["result"]
    assert r["quality"] == quality and r["frames"] == 3
    assert r["codec"] == params.get("codec") or r["codec"] == {"mp4": "h264", "mov_prores4444": "prores4444",
                                                               "webm_alpha": "vp9"}[container]
    st = _probe(dest)
    assert st["codec_name"] == codec and st["pix_fmt"].startswith(pix[:8])  # ProRes decodes as 12-bit
    if r["codec"] == "prores422":
        assert st["profile"] == "Standard"
    # transparency is claimed only for real alpha content in an alpha-capable codec
    assert r["alpha"] is (product == "processed" and r["codec"] in ("prores4444", "vp9"))


@NEEDS_FFMPEG
@pytest.mark.parametrize("codec,pix", [("prores422", "yuv422p10le"), ("prores4444", "yuva444p10le")])
def test_export_grayscale_matte_to_mov(env, svc, codec, pix):
    out = key_style_result(env)
    dest = env.tmp / "deliver" / "m.mov"
    _, done = run(svc, export_job(env, out, product="matte", container="mov_prores4444", codec=codec,
                                  dest=str(dest)))
    assert done["outcome"] == "complete", done
    assert _probe(dest)["pix_fmt"].startswith(pix[:8]) and done["result"]["alpha"] is False


@pytest.mark.parametrize("over", [
    {"codec": "vp9"}, {"codec": "prores4444"}, {"codec": "x264"}, {"codec": ""}, {"codec": 1},
    {"quality": 52}, {"quality": -1}, {"quality": 18.0}, {"quality": True}, {"quality": "18"},
    {"codec": "hevc", "quality": 52},
    {"container": "mov_prores4444", "dest": "/o/a.mov", "quality": 0},
    {"container": "mov_prores4444", "dest": "/o/a.mov", "quality": 32},
    {"container": "webm_alpha", "dest": "/o/a.webm", "quality": 64},
    {"container": "webm_alpha", "dest": "/o/a.webm", "codec": "hevc"},
    {"extra_flag": "-vf x"},
])
def test_export_codec_quality_validation(env, over):
    with pytest.raises(TaskRejected) as e:
        TaskJob.from_dict(export_job(env, env.tmp, **{"dest": "/o/a.mp4", **over}))
    assert e.value.code == "invalid"


def test_export_params_freeze_codec_and_quality(env):
    j = TaskJob.from_dict(export_job(env, env.tmp, dest="/o/a.mp4", codec="hevc", quality=20))
    assert (j.params["codec"], j.params["quality"]) == ("hevc", 20)
    j = TaskJob.from_dict(export_job(env, env.tmp, dest="/o/a.mp4"))
    assert (j.params["codec"], j.params["quality"]) == ("h264", 18)
    j = TaskJob.from_dict(export_job(env, env.tmp, container="mov_prores4444", dest="/o/a.mov", product="fg"))
    assert (j.params["codec"], j.params["quality"]) == ("prores4444", 9)  # alpha containers take every product


# ------------------------------------------------------------ prematte_hint

def guide(env, value=255, stems=STEMS, shape=(H, W)):
    d = env.root / "VideoMamaMaskHint"
    d.mkdir(exist_ok=True)
    for i, s in enumerate(stems):
        m = np.zeros(shape, np.uint8)
        m[1:4, 1 + (i % 2):6] = value
        assert cv2.imwrite(str(d / f"{s}.png"), m)
    return d


def tree(d):
    return {str(f.relative_to(d)): f.read_bytes() for f in Path(d).rglob("*") if f.is_file()}


def test_prematte_capability_needs_neither_gpu_nor_models():
    caps = task_capabilities(None, gpu=False, ffmpeg=False)
    assert caps["tasks"]["prematte_hint"] is True and caps["tasks"]["videomama"] is False


def test_prematte_promotes_the_guide_range_only_and_keeps_everything_else(env, svc):
    g = guide(env, 200)
    env.old_hint(value=10)
    src, gbefore = tree(env.frames), tree(g)
    events, done = run(svc, env.job("prematte_hint", {}))
    assert done["outcome"] == "complete", done
    assert done["result"]["published"] == B - A + 1 and done["result"]["hint_dir"] == str(env.root / "AlphaHint")
    for i, s in enumerate(STEMS):
        got = cv2.imread(str(env.root / "AlphaHint" / f"{s}.png"), cv2.IMREAD_UNCHANGED)
        if A <= i <= B:
            np.testing.assert_array_equal(got, cv2.imread(str(g / f"{s}.png"), cv2.IMREAD_UNCHANGED))
        else:
            assert (got == 10).all()  # outside In/Out the old hint survives
    assert tree(env.frames) == src and tree(g) == gbefore  # source and guide untouched
    prov = json.loads((env.root / "AlphaHint" / ".corridorkey_hint.json").read_text())
    assert prov["kind"] == "prematte_hint" and prov["range"]["in_stem"] == STEMS[A]
    assert not (env.root / ".corridorkey_hint.lock").exists()
    assert not (env.root / "VideoMamaMaskHint" / ".corridorkey_hint.json").exists()
    assert not (env.root / ".corridorkey_mask_manifest.json").exists()  # a final hint, not a guide


def test_prematte_applies_only_feather(env, svc):
    g = guide(env)
    gbefore = tree(g)
    _, done = run(svc, env.job("prematte_hint", {"mask_cleanup": {"grow": 5, "fill_holes": True, "feather": 1.0}}))
    assert done["outcome"] == "complete", done
    got = cv2.imread(str(env.root / "AlphaHint" / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE)
    want = cv2.GaussianBlur(cv2.imread(str(g / f"{STEMS[A]}.png"), cv2.IMREAD_GRAYSCALE), (0, 0), 1.0)
    np.testing.assert_array_equal(got, want)  # no second grow/fill on an already-cleaned guide
    assert tree(g) == gbefore


def test_prematte_needs_a_dense_guide_and_publishes_nothing(env, svc):
    guide(env, stems=STEMS[A:B])  # last range frame missing
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("prematte_hint", {}))
    assert e.value.code == "invalid" and STEMS[B] in e.value.message
    assert not (env.root / "AlphaHint").exists() and not (env.root / ".corridorkey_hint.lock").exists()


def test_prematte_wrong_size_guide_keeps_old_hint(env, svc):
    guide(env, shape=(H + 2, W))
    old = env.old_hint()
    before = tree(old)
    _, done = run(svc, env.job("prematte_hint", {}))
    assert done["outcome"] == "failed" and "nothing was published" in done["message"]
    assert tree(old) == before and not (env.root / ".corridorkey_hint.lock").exists()


def test_prematte_cancel_keeps_old_hint(env, svc):
    guide(env)
    old = env.old_hint()
    before = tree(old)
    cancel = threading.Event()
    cancel.set()
    _, done = run(svc, env.job("prematte_hint", {}), cancel)
    assert done["outcome"] == "cancelled" and tree(old) == before
    assert not list(env.root.glob("AlphaHint.*")) and not (env.root / ".corridorkey_hint.lock").exists()


def test_prematte_rejects_unknown_params_and_other_publish_dir(env, svc):
    guide(env)
    with pytest.raises(TaskRejected):
        TaskJob.from_dict(env.job("prematte_hint", {"mask_stem": "x"}))
    with pytest.raises(TaskRejected) as e:
        run(svc, env.job("prematte_hint", {}, dest="VideoMamaMaskHint"))
    assert e.value.code == "paths"


def test_soft_guide_is_promoted_with_intermediate_alpha(env, svc, monkeypatch):
    """Batch Direct = GVM -> guide -> prematte_hint must end with the same soft alpha as Generate Direct."""
    ramp = np.tile(np.array([0, 1, 10, 11, 64, 128, 254, 255], dtype=np.uint8), (H, 1))

    def engine(clip, **kwargs):
        folder = Path(clip.root_path) / "AlphaHint"
        folder.mkdir()
        for name in clip.input_asset.get_frame_files():
            assert cv2.imwrite(str(folder / name), ramp)

    monkeypatch.setattr(svc, "run_gvm", engine)
    assert run(svc, env.job("gvm", {}, dest="VideoMamaMaskHint"))[1]["outcome"] == "complete"
    _, done = run(svc, env.job("prematte_hint", {}, job_id="job-2"))
    assert done["outcome"] == "complete", done
    for stem in STEMS[A:B + 1]:
        got = cv2.imread(str(env.root / "AlphaHint" / f"{stem}.png"), cv2.IMREAD_GRAYSCALE)
        np.testing.assert_array_equal(got, ramp)
    # the guide stays the one the refiners/loaders read, and readiness is manifest-based, not value-based
    assert json.loads((env.root / ".corridorkey_mask_manifest.json").read_text())["source"] == "gvm"
