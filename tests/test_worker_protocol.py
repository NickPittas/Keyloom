"""Worker protocol v1: strict framing, one job at a time, cancel/EOF, stdout purity (fake engine)."""
from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from pathlib import Path

import pytest

from backend.worker import Worker
from tests.test_key_job import STEMS, A, B, Env, FakeEngine, svc  # noqa: F401 (svc is a fixture)

REPO = Path(__file__).resolve().parent.parent
HELLO = {"worker_rev": "abc", "engine": "corridorkey-v2", "host": "testhost", "gpu": None,
         "models": {"corridorkey_v2": {"path": "/m", "bytes": 1, "sha256": None}},
         "capabilities": {"key": True, "generate_hint": False, "publish_modes": ["final"]}}


class Sink:
    def __init__(self):
        self.lines, self.lock = [], threading.Lock()

    def write(self, s):
        with self.lock:
            self.lines.append(s)

    def flush(self):
        pass

    def events(self):
        with self.lock:
            return [json.loads(x) for x in self.lines]

    def wait(self, pred, timeout=10):
        end = time.time() + timeout
        while time.time() < end:
            hit = [e for e in self.events() if pred(e)]
            if hit:
                return hit[0]
            time.sleep(0.01)
        raise AssertionError(f"timeout; saw {self.events()}")


class Session:
    def __init__(self, service, hello=HELLO):
        self.sink, self.r, self.w = Sink(), *os.pipe()
        self.worker = Worker(service, self.sink, hello)
        self.rc = None
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        with os.fdopen(self.r, "rb") as stdin:
            self.rc = self.worker.serve(stdin)

    def send(self, obj):
        raw = obj if isinstance(obj, bytes) else (json.dumps(obj) + "\n").encode()
        os.write(self.w, raw)

    def key(self, job):
        self.send({"v": 1, "cmd": "key", "job": job})

    def eof(self):
        os.close(self.w)
        self.thread.join(10)
        assert not self.thread.is_alive()


@pytest.fixture
def env(tmp_path):
    return Env(tmp_path)


def test_hello_first_and_truthful(svc):
    s = Session(svc)
    hello = s.sink.wait(lambda e: e["event"] == "hello")
    assert hello["v"] == 1 and hello["protocol"] == 1 and hello["gpu"] is None
    assert hello["models"]["corridorkey_v2"]["sha256"] is None
    assert hello["capabilities"] == {"key": True, "generate_hint": False, "publish_modes": ["final"]}
    s.eof()
    assert s.rc == 0 and s.sink.events()[0]["event"] == "hello"


@pytest.mark.parametrize("raw,code", [
    (b"not json\n", "protocol"), (b"[1]\n", "protocol"), (b'{"cmd":"key"}\n', "protocol"),
    (b'{"v":2,"cmd":"shutdown"}\n', "protocol"), (b'{"v":true,"cmd":"shutdown"}\n', "protocol"),
    (b'{"v":1,"cmd":"explode"}\n', "protocol"), (b'{"v":1,"cmd":"key","job":5}\n', "protocol"),
])
def test_malformed_input_is_rejected_and_worker_survives(svc, raw, code):
    s = Session(svc)
    s.send(raw)
    rej = s.sink.wait(lambda e: e["event"] == "rejected")
    assert rej["code"] == code and rej["job_id"] is None and rej["v"] == 1
    s.send({"v": 1, "cmd": "cancel", "job_id": "x"})
    s.eof()
    assert s.rc == 0


def test_rejected_carries_job_identity_and_writes_nothing(svc, env):
    d = env.job()
    d["outputs"]["fg"][0] = "/elsewhere/x.exr"
    s = Session(svc)
    s.key(d)
    rej = s.sink.wait(lambda e: e["event"] == "rejected")
    assert (rej["job_id"], rej["code"]) == ("job-aaaaaaaa", "paths")
    s.eof()
    assert not env.out.exists() and not any(e["event"] == "accepted" for e in s.sink.events())


def test_job_runs_then_same_id_is_busy_but_next_id_runs(svc, env):
    s = Session(svc)
    s.key(env.job())
    done = s.sink.wait(lambda e: e["event"] == "done")
    kinds = [e["event"] for e in s.sink.events()]
    assert kinds.index("accepted") < kinds.index("done") and done["outcome"] == "complete"
    assert all(e.get("job_id") in (None, "job-aaaaaaaa") for e in s.sink.events())
    s.key(env.job())  # already ran
    assert s.sink.wait(lambda e: e["event"] == "rejected")["code"] == "busy"
    s.key(env.job(job_id="job-bbbbbbbb", mode="resume"))
    s.sink.wait(lambda e: e["event"] == "done" and e["job_id"] == "job-bbbbbbbb")
    assert len(FakeEngine.instances) == 1  # the model stayed loaded across jobs
    s.eof()


def blocking(release):
    orig = FakeEngine.run_clip

    def patched(self, *a, **k):
        self.on_run = lambda check_cancel: [check_cancel() or time.sleep(0.01) for _ in range(1000)
                                            if not release.is_set()]
        return orig(self, *a, **k)
    return orig, patched


@pytest.fixture
def slow_engine():
    release = threading.Event()
    orig, patched = blocking(release)
    FakeEngine.run_clip = patched
    yield release
    release.set()
    FakeEngine.run_clip = orig


def test_second_job_while_busy_and_cancel_command(svc, env, slow_engine):
    s = Session(svc)
    s.key(env.job())
    s.sink.wait(lambda e: e["event"] == "accepted")
    s.key(env.job(job_id="job-bbbbbbbb"))
    busy = s.sink.wait(lambda e: e["event"] == "rejected")
    assert (busy["job_id"], busy["code"]) == ("job-bbbbbbbb", "busy")
    s.send({"v": 1, "cmd": "cancel", "job_id": "someone-else"})  # ignored
    time.sleep(0.1)
    assert not any(e["event"] == "done" for e in s.sink.events())
    s.send({"v": 1, "cmd": "cancel", "job_id": "job-aaaaaaaa"})
    done = s.sink.wait(lambda e: e["event"] == "done")
    assert done["outcome"] == "cancelled" and done["job_id"] == "job-aaaaaaaa"
    s.eof()


def test_eof_cancels_running_job_and_still_sends_done(svc, env, slow_engine):
    s = Session(svc)
    s.key(env.job())
    s.sink.wait(lambda e: e["event"] == "accepted")
    s.eof()
    done = [e for e in s.sink.events() if e["event"] == "done"]
    assert len(done) == 1 and done[0]["outcome"] == "cancelled" and s.rc == 0
    assert not (env.out / ".corridorkey_job.lock").exists()


def test_shutdown_cancels_and_exits(svc, env, slow_engine):
    s = Session(svc)
    s.key(env.job())
    s.sink.wait(lambda e: e["event"] == "accepted")
    s.send({"v": 1, "cmd": "shutdown"})
    s.thread.join(10)
    assert s.rc == 0 and s.sink.wait(lambda e: e["event"] == "done")["outcome"] == "cancelled"
    os.close(s.w)


def test_model_hash_requirement_not_assumed(svc, env):
    d = env.job()
    d["require"]["models"]["corridorkey_v2"] = "deadbeef"
    s = Session(svc)
    s.key(d)
    rej = s.sink.wait(lambda e: e["event"] == "rejected")
    assert rej["code"] == "invalid" and "hash" in rej["message"]
    s.eof()


def test_key_unavailable_is_rejected_not_faked(svc, env):
    hello = {**HELLO, "capabilities": {**HELLO["capabilities"], "key": False}}
    s = Session(svc, hello)
    s.key(env.job())
    assert "unavailable" in s.sink.wait(lambda e: e["event"] == "rejected")["message"]
    s.eof()


RUNNER = """
import sys
sys.path.insert(0, {repo!r})
import tests.test_key_job as t
from backend.service import CorridorKeyService
import backend.corridorkey_v2 as v2
import backend.worker as w

class Noisy(t.FakeEngine):
    def run_clip(self, *a, **k):
        import os, subprocess
        print("STRAY PRINT FROM A LIBRARY")            # must not reach the protocol stream
        os.write(1, b"NATIVE FD1 WRITE\\n")             # C code writing straight to fd 1
        subprocess.run([sys.executable, "-c", "print('CHILD STDOUT')"], check=True)  # like ffmpeg
        sys.stderr.write("log line\\n" * 20000)         # bigger than a pipe buffer
        self.on_run = lambda check_cancel: [check_cancel() or __import__("time").sleep(0.01)
                                            for _ in range(2000)]
        return super().run_clip(*a, **k)

v2.CorridorKeyV2Engine = Noisy
sys.exit(w.main([], service=CorridorKeyService(), hello={hello!r}))
"""


def test_subprocess_stdout_is_protocol_only_stderr_drained_eof_cancels(env, tmp_path):
    e = {"PATH": "/usr/bin:/bin", "HOME": str(tmp_path), "CORRIDORKEY_V2_SCRATCH_DIR": str(tmp_path / "sc"),
         "PYTHONDONTWRITEBYTECODE": "1"}
    p = subprocess.Popen([sys.executable, "-B", "-c", RUNNER.format(repo=str(REPO), hello=HELLO)],
                         stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                         cwd=REPO, env=e)
    err = []
    threading.Thread(target=lambda: err.append(p.stderr.read()), daemon=True).start()  # drain
    out = p.stdout
    assert json.loads(out.readline())["event"] == "hello"
    p.stdin.write((json.dumps({"v": 1, "cmd": "key", "job": env.job()}) + "\n").encode())
    p.stdin.flush()
    seen = []
    while not seen or seen[-1]["event"] != "progress":  # engine is now running (and noisy)
        seen.append(json.loads(out.readline()))
    assert seen[0]["event"] == "accepted"
    p.stdin.close()  # EOF == cancel
    rest = [json.loads(line) for line in out]  # every remaining stdout line must be JSON
    assert p.wait(30) == 0
    assert rest[-1]["event"] == "done" and rest[-1]["outcome"] == "cancelled"
    assert all(x["v"] == 1 for x in rest)
    time.sleep(0.2)
    blob = b"".join(err)
    assert b"STRAY PRINT" in blob and b"NATIVE FD1 WRITE" in blob and b"CHILD STDOUT" in blob
    assert len(blob) > 100_000


# ------------------------------------------------------------- hello: hash, GPU, env order

import hashlib  # noqa: E402
import io  # noqa: E402
import types  # noqa: E402

import backend.worker as worker_mod  # noqa: E402


def test_model_sha256_real_and_cached_outside_repo(tmp_path, monkeypatch):
    f, cache = tmp_path / "w.safetensors", tmp_path / "scratch" / "cache"
    f.write_bytes(b"abc" * 1000)
    want = hashlib.sha256(b"abc" * 1000).hexdigest()
    assert worker_mod.model_sha256(f, cache) == want
    assert (cache / "model_sha256.json").is_file() and not str(cache).startswith(str(REPO))
    monkeypatch.setattr(worker_mod.hashlib, "sha256", lambda *a: (_ for _ in ()).throw(AssertionError("rehash")))
    assert worker_mod.model_sha256(f, cache) == want  # served from cache, file untouched


def test_model_sha256_cache_invalidated_by_any_identity_change(tmp_path):
    f, cache = tmp_path / "w.bin", tmp_path / "cache"
    f.write_bytes(b"one")
    first = worker_mod.model_sha256(f, cache)
    st = f.stat()
    f.write_bytes(b"two")  # same size
    os.utime(f, ns=(st.st_atime_ns, st.st_mtime_ns + 5))
    second = worker_mod.model_sha256(f, cache)
    assert second == hashlib.sha256(b"two").hexdigest() != first
    other = tmp_path / "other.bin"  # swapped file: same size and mtime, different inode
    other.write_bytes(b"333")
    os.utime(other, ns=(st.st_atime_ns, os.stat(f).st_mtime_ns))
    os.replace(other, f)
    assert worker_mod.model_sha256(f, cache) == hashlib.sha256(b"333").hexdigest()


def test_model_sha256_unreadable_or_changing_is_unknown(tmp_path, monkeypatch):
    assert worker_mod.model_sha256(tmp_path / "missing", tmp_path / "c") is None
    f = tmp_path / "w.bin"
    f.write_bytes(b"data")
    real = hashlib.sha256

    def racing(*a):
        f.write_bytes(b"changed while hashing!")  # file mutates mid-hash
        return real(*a)
    monkeypatch.setattr(worker_mod.hashlib, "sha256", racing)
    assert worker_mod.model_sha256(f, tmp_path / "c") is None
    assert not (tmp_path / "c" / "model_sha256.json").exists()


def fake_torch(monkeypatch, available):
    cuda = types.SimpleNamespace(
        is_available=lambda: available, current_device=lambda: 0,
        get_device_properties=lambda i: types.SimpleNamespace(name="Fake GPU", total_memory=8 * 2**30))
    monkeypatch.setitem(sys.modules, "torch", types.SimpleNamespace(cuda=cuda))


def fake_assets(monkeypatch, tmp_path):
    w = tmp_path / "weights.safetensors"
    w.write_bytes(b"fake checkpoint")
    monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(tmp_path / "scratch"))
    monkeypatch.setattr("backend.v2_paths.get_v2_asset_paths", lambda: types.SimpleNamespace(weights=w))
    monkeypatch.setattr("backend.v2_paths.validate_v2_assets", lambda paths=None: [])
    return w


def test_hello_reports_real_hash_and_gpu(tmp_path, monkeypatch):
    w = fake_assets(monkeypatch, tmp_path)
    fake_torch(monkeypatch, True)
    h = worker_mod.probe_hello(1)
    m = h["models"]["corridorkey_v2"]
    assert m["sha256"] == hashlib.sha256(b"fake checkpoint").hexdigest() and m["bytes"] == w.stat().st_size
    assert h["gpu"] == {"index": 1, "name": "Fake GPU", "vram_gb": 8.0}
    assert h["capabilities"]["key"] is True


def test_no_gpu_means_key_unavailable(tmp_path, monkeypatch):
    fake_assets(monkeypatch, tmp_path)
    fake_torch(monkeypatch, False)
    h = worker_mod.probe_hello(None)
    assert h["gpu"] is None and h["capabilities"]["key"] is False
    assert h["models"]["corridorkey_v2"]["sha256"]  # hash is still real


def test_matching_required_hash_is_accepted(svc, env):
    sha = "ab" * 32
    hello = {**HELLO, "models": {"corridorkey_v2": {"path": "/m", "bytes": 1, "sha256": sha}}}
    d = env.job()
    d["require"]["models"]["corridorkey_v2"] = sha
    s = Session(svc, hello)
    s.key(d)
    assert s.sink.wait(lambda e: e["event"] == "done")["outcome"] == "complete"
    s.eof()


def test_pci_bus_order_is_set_before_probe_and_visibility(monkeypatch):
    seen = {}
    monkeypatch.setenv("CUDA_DEVICE_ORDER", "FASTEST_FIRST")
    monkeypatch.setenv("CUDA_VISIBLE_DEVICES", "")
    monkeypatch.setattr(worker_mod, "probe_hello",
                        lambda gpu: seen.update(order=os.environ["CUDA_DEVICE_ORDER"],
                                                vis=os.environ["CUDA_VISIBLE_DEVICES"]) or HELLO)
    out = Sink()
    rc = worker_mod.main(["--gpu", "2"], stdin=io.BytesIO(b""), out=out, service=object())
    assert rc == 0 and seen == {"order": "PCI_BUS_ID", "vis": "2"}


def test_stdout_newline_is_not_translated_and_hello_has_platform(monkeypatch):
    calls = []

    class Out(Sink):
        def reconfigure(self, **kw):
            calls.append(kw)
    out = Out()
    assert worker_mod.main([], stdin=io.BytesIO(b""), out=out, service=object(),
                           hello={**HELLO, "platform": sys.platform}) == 0
    assert calls == [{"newline": "\n"}]
    assert json.loads(out.lines[0])["platform"] == sys.platform
    assert "\r" not in out.lines[0]


def test_probe_hello_reports_platform(tmp_path, monkeypatch):
    fake_assets(monkeypatch, tmp_path)
    fake_torch(monkeypatch, True)
    assert worker_mod.probe_hello(0)["platform"] == sys.platform


def test_worker_survives_a_rejected_prepare_and_can_retry_the_same_job(svc, env):
    env.out.mkdir(parents=True)
    blocker = env.out / "Fg"
    blocker.write_text("file where a folder is needed")
    s = Session(svc)
    s.key(env.job())
    rej = s.sink.wait(lambda e: e["event"] == "rejected")
    assert (rej["job_id"], rej["code"]) == ("job-aaaaaaaa", "paths")
    assert not (env.out / ".corridorkey_job.lock").exists()
    blocker.unlink()
    s.key(env.job())  # same persistent worker, same job id: nothing was accepted, so allowed
    assert s.sink.wait(lambda e: e["event"] == "done")["outcome"] == "complete"
    s.eof()
