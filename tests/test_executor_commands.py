"""A6: the render worker only executes. Ready is transport-only; every model job carries its exact files;
hint-lock recovery is an app-side helper (same writer checks), not a worker command."""
from __future__ import annotations

import json
import os
import socket
import subprocess
import sys
from pathlib import Path

import pytest

from backend.hint_lock_cli import run as lock_run
from backend.worker import MODEL_KINDS
from tests.test_key_job import Env, FakeEngine, svc  # noqa: F401 (svc is a fixture)
from tests.test_model_status import roots  # noqa: F401 (fixture)
from tests.test_worker_protocol import READY, Session

REPO = Path(__file__).resolve().parent.parent


@pytest.fixture
def env(tmp_path):
    return Env(tmp_path)


def test_ready_carries_no_models_and_no_capabilities(svc):
    s = Session(svc)
    ready = s.sink.wait(lambda e: e["event"] == "ready")
    assert set(ready) == {"v", "event", "protocol", *READY}
    s.eof()


def test_key_job_without_model_files_is_refused_and_writes_nothing(svc, env):
    s = Session(svc)
    s.send({"v": 1, "cmd": "key", "job": {k: v for k, v in env.job().items() if k != "model_files"}})
    rej = s.sink.wait(lambda e: e["event"] == "rejected")
    assert rej["code"] == "invalid" and "model_files" in rej["message"]
    s.eof()
    assert not env.out.exists()


def test_key_job_holds_exactly_the_supplied_files(svc, env):
    s = Session(svc)
    s.key(env.job())
    assert s.sink.wait(lambda e: e["event"] == "done")["outcome"] == "complete"
    assert svc._model_files == env.model_files  # what the app sent is what the executor opened
    s.eof()


def test_model_job_kinds_all_need_files_and_the_rest_do_not():
    assert set(MODEL_KINDS) == {"gvm", "birefnet", "sam_preview", "sam_track", "matanyone2", "videomama"}


@pytest.mark.parametrize("old", ["inspect_hint_lock", "clear_hint_lock", "refresh"])
def test_the_old_management_commands_are_not_worker_commands(svc, old):
    s = Session(svc)
    s.send({"v": 1, "cmd": old, "request_id": "r1", "clip_root": "/x"})
    assert s.sink.wait(lambda e: e["event"] == "rejected")["code"] == "protocol"
    s.eof()


def _lock(root, **kw):
    meta = {"schema": 1, "job_id": "dead-job", "host": socket.gethostname(), "platform": sys.platform,
            "pid": 2**22 + 12345, "boot_id": None, "proc_start_ticks": None, "pid_ns": None, **kw}
    (root / ".corridorkey_hint.lock").write_text(json.dumps(meta))
    return meta


def test_app_side_lock_helper_keeps_the_exact_identity_checks(tmp_path):
    root = tmp_path / "clip"
    root.mkdir()
    meta = _lock(root)
    got = lock_run({"cmd": "inspect", "request_id": "r1", "clip_root": str(root)})
    assert got["event"] == "hint_lock" and got["lock"]["job_id"] == "dead-job"
    wrong = lock_run({"cmd": "clear", "request_id": "r2", "clip_root": str(root), "reason": "observed_exit",
                      "expect": {"job_id": "someone-else", "host": meta["host"], "pid": meta["pid"]}})
    assert wrong["event"] == "hint_lock_refused" and wrong["code"] == "mismatch"
    assert (root / ".corridorkey_hint.lock").exists()


def test_app_side_lock_helper_runs_as_a_one_shot_process(tmp_path):
    root = tmp_path / "clip"
    root.mkdir()
    req = json.dumps({"cmd": "inspect", "request_id": "r1", "clip_root": str(root)})
    out = subprocess.run([sys.executable, "-m", "backend.hint_lock_cli", req], cwd=REPO, capture_output=True,
                         text=True, timeout=60, env={**os.environ, "PYTHONDONTWRITEBYTECODE": "1"})
    assert out.returncode == 0 and json.loads(out.stdout) == {
        "event": "hint_lock", "request_id": "r1", "clip_root": str(root), "lock": None}


def test_app_side_lock_helper_rejects_a_bad_request():
    assert lock_run({"cmd": "inspect", "request_id": "bad id!", "clip_root": "/x"})["code"] == "protocol"
    assert lock_run({"cmd": "inspect", "request_id": "r1", "clip_root": "relative"})["event"] == "rejected"


def test_snapshot_event_is_what_the_app_keeps(roots):
    from backend.app_models import AppModels
    doc = AppModels(roots=roots).document()
    assert set(doc) == {"models", "ffmpeg", "sam2", "birefnet"}
    assert set(doc["models"]) == {"key", "gvm", "matanyone2", "videomama", "sam2", "birefnet"}
    assert doc["sam2"] == {"small": "sam2-small", "base_plus": "sam2-base-plus", "large": "sam2-large"}
    assert doc["birefnet"]["Matting"] == "birefnet-BiRefNet-matting"
    assert doc["models"]["birefnet"]["birefnet-BiRefNet-matting"]["state"] == "missing"  # empty temp roots, nothing hashed
