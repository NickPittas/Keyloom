# ruff: noqa: F811  (fixtures imported from tests.test_key_job)
"""An idle worker holds no model file: after every job the leases are released (Linux: no open lock file).

The engine below takes a REAL kernel lock (backend.model_lock.DestinationLock, the same lock every model wrapper's
lease uses) while loaded and releases it in unload(), exactly like the real wrappers."""
from __future__ import annotations

import os
import sys

import pytest

from backend.model_lock import DestinationBusyError, DestinationLock
from tests.test_key_job import Env, FakeEngine, svc  # noqa: F401,F811 (svc is a fixture)
from tests.test_worker_protocol import Session


def _open_lock_files() -> list[str]:
    out = []
    for fd in os.listdir("/proc/self/fd"):
        try:
            target = os.readlink(f"/proc/self/fd/{fd}")
        except OSError:
            continue
        if "keyloom-model-locks" in target:
            out.append(target)
    return out


@pytest.fixture
def env(tmp_path):
    return Env(tmp_path)


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="counts open descriptors via /proc")
def test_idle_worker_holds_no_model_lock_after_a_job(svc, env, tmp_path, monkeypatch):
    model_dir = tmp_path / "model"
    model_dir.mkdir()
    seen_during = []

    class LeasedEngine(FakeEngine):
        lease = None

        def load(self, on_status=None):
            self.lease = DestinationLock(model_dir, shared=True).acquire()
            super().load(on_status)

        def unload(self):
            if self.lease is not None:
                self.lease.close()
                self.lease = None
            super().unload()

        def run_clip(self, *a, **k):
            seen_during.append(len(_open_lock_files()))
            return super().run_clip(*a, **k)

    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine", LeasedEngine)
    assert _open_lock_files() == []
    s = Session(svc)
    s.key(env.job())
    s.sink.wait(lambda e: e["event"] == "done")
    # The proof is not vacuous: the lock WAS held while the job ran.
    assert seen_during and seen_during[0] >= 1
    # Idle now (worker process still alive, serving): nothing held, so an installer's exclusive lock succeeds.
    assert _open_lock_files() == []
    with DestinationLock(model_dir, shared=False):
        pass
    s.eof()


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="counts open descriptors via /proc")
def test_a_failed_job_also_leaves_the_worker_without_locks(svc, env, tmp_path, monkeypatch):
    model_dir = tmp_path / "model"
    model_dir.mkdir()

    class BrokenEngine(FakeEngine):
        lease = None

        def load(self, on_status=None):
            self.lease = DestinationLock(model_dir, shared=True).acquire()
            super().load(on_status)

        def unload(self):
            if self.lease is not None:
                self.lease.close()
                self.lease = None
            super().unload()

        def run_clip(self, *a, **k):
            raise RuntimeError("engine died")

    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine", BrokenEngine)
    s = Session(svc)
    s.key(env.job())
    s.sink.wait(lambda e: e["event"] in ("done", "rejected"))
    assert _open_lock_files() == []
    s.eof()


def test_a_held_lease_really_blocks_an_installer(tmp_path):
    """Guard for the two tests above: a held shared lease is visible to the exclusive (install) lock."""
    held = DestinationLock(tmp_path, shared=True).acquire()
    with pytest.raises(DestinationBusyError):
        DestinationLock(tmp_path, shared=False).acquire()
    held.close()


def _stale_session(svc, now_ready):
    from tests.test_worker_protocol import HELLO
    stale = {**HELLO, "capabilities": {**HELLO["capabilities"], "key": False}}
    probes = []

    def reprobe():
        probes.append(1)
        return {"models": HELLO["models"], "capabilities": {**HELLO["capabilities"], "key": now_ready}}

    s = Session(svc, hello=stale)
    s.worker._reprobe = reprobe
    return s, probes


def test_refresh_command_re_reads_the_models_and_says_hello_again(svc):
    s, probes = _stale_session(svc, True)
    s.sink.wait(lambda e: e["event"] == "hello")
    s.send({"v": 1, "cmd": "refresh"})
    fresh = s.sink.wait(lambda e: e["event"] == "hello" and e["capabilities"]["key"] is True)
    assert fresh["models"]["corridorkey_v2"] and probes == [1]
    s.eof()


def test_a_job_for_a_model_installed_after_start_runs_without_a_restart(svc, env):
    s, probes = _stale_session(svc, True)
    s.key(env.job())
    done = s.sink.wait(lambda e: e["event"] == "done")
    assert done["outcome"] == "complete" and probes == [1]
    s.eof()


def test_a_job_for_a_model_still_missing_is_rejected_after_one_re_read(svc, env):
    s, probes = _stale_session(svc, False)
    s.key(env.job())
    assert s.sink.wait(lambda e: e["event"] == "rejected")["code"] == "invalid"
    assert probes == [1]
    s.eof()
