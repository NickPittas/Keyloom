"""Fast online update gates (packaging/online.py). Tiny temp fixtures, fake opener, no network, no real runtime.

Core lifecycle APIs (runtime_available, verify_bundle(skip_runtime=)) are faked here: this proves online.py's use of
them, not core behaviour, performance, or any packaged/Windows gate."""
import hashlib
import importlib.util
import json
import sys
import threading
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
_spec = importlib.util.spec_from_file_location("ck_online_t", ROOT / "packaging" / "online.py")
online = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(online)
L = online.L


def sha(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def meta(b: bytes) -> dict:
    return {"size": len(b), "sha256": sha(b)}


LC = b"# lifecycle"
APP = b"app-bytes"
RT = b"runtime-bytes"


def fixture(tmp_path):
    files = {"app/packaging/lifecycle.py": meta(LC), "app/a.py": meta(APP), "runtime/lib/big.so": meta(RT)}
    mani = {"schema": 1, "platform": online.PLATFORM, "version_id": "v1", "files": files,
            "runtime_id": online.runtime_id(files)}
    pdir = tmp_path / "payload"
    for rel, b in (("app/packaging/lifecycle.py", LC), ("app/a.py", APP)):
        (pdir / rel).parent.mkdir(parents=True, exist_ok=True)
        (pdir / rel).write_bytes(b)
    prefix = tmp_path / "prefix"
    prefix.mkdir()
    return mani, json.dumps(mani).encode(), pdir, prefix


def test_runtime_id_is_canonical_full_sha():
    files = {"runtime/b": {"size": 1, "sha256": "x"}, "app/z": {"size": 2}, "runtime/a": {"size": 3}}
    sub = {k: v for k, v in files.items() if k.startswith("runtime/")}
    want = hashlib.sha256(json.dumps(sub, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    assert online.runtime_id(files) == want and len(want) == 64
    files2 = dict(files, **{"app/z": {"size": 99}})
    assert online.runtime_id(files2) == want  # app files do not move the identity


def test_manifest_with_wrong_runtime_id_rejected(tmp_path):
    mani = fixture(tmp_path)[0]
    online.validate_manifest(mani)
    bad = dict(mani, runtime_id="0" * 64)
    with pytest.raises(online.Fail, match="runtime_id"):
        online.validate_manifest(bad)


def test_attach_stages_zero_runtime_bytes(tmp_path, monkeypatch):
    mani, raw, pdir, prefix = fixture(tmp_path)
    touched = []
    real_copy = online.copy_verified
    monkeypatch.setattr(online, "copy_verified", lambda s, d, m: touched.append(d.name) or real_copy(s, d, m))
    seen = {}
    monkeypatch.setattr(L, "verify_bundle", lambda d, skip_runtime=False: seen.update(skip=skip_runtime) or [])
    stage = prefix / ".staging.t"
    stage.mkdir()
    tree = online.build_tree(None, prefix, None, stage, prefix / ".staging.dl", pdir, mani, raw, [],
                             {"app/packaging/lifecycle.py", "app/a.py"}, attach=True)  # fx=None: any download would crash
    assert sorted(touched) == ["a.py", "lifecycle.py"]
    assert not (tree / "runtime").exists() and seen == {"skip": True}


def test_no_attach_requires_runtime_files(tmp_path, monkeypatch):
    mani, raw, pdir, prefix = fixture(tmp_path)
    stage = prefix / ".staging.t"
    stage.mkdir()
    with pytest.raises(online.Fail, match="could not be provided"):
        online.build_tree(None, prefix, None, stage, prefix / ".staging.dl", pdir, mani, raw, [],
                          {"app/packaging/lifecycle.py", "app/a.py"}, attach=False)


def install_receipt(prefix, mani):
    d = prefix / "versions" / "v1"
    d.mkdir(parents=True)
    (d / L.MANIFEST).write_text(json.dumps(mani))
    return {"current": "v1"}, d


def test_up_to_date_deep_is_one_nonruntime_verify(tmp_path, monkeypatch):
    mani, _, _, prefix = fixture(tmp_path)
    rec, _ = install_receipt(prefix, mani)
    calls = []
    monkeypatch.setattr(L, "pointer_ok", lambda p, v: True)
    monkeypatch.setattr(L, "verify_bundle", lambda d, skip_runtime=False: calls.append(skip_runtime) or [])
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: good(m), raising=False)
    assert online.up_to_date(prefix, rec, mani, deep=False) and calls == []  # pre-check hashes nothing
    assert online.up_to_date(prefix, rec, mani) and calls == [True]
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: {"ok": False, "reason": "no seal"}, raising=False)
    assert not online.up_to_date(prefix, rec, mani)


def good(m):
    return {"ok": True, "rid": online.runtime_id(m["files"])}


@pytest.mark.parametrize("resp", [{"ok": False, "reason": "x"}, True, False, None, {}, "ok",
                                  {"ok": 1, "rid": "r"}, {"ok": True, "rid": "0" * 64}, {"ok": True}])
def test_only_exact_ok_dict_with_matching_rid_is_trusted(tmp_path, monkeypatch, resp):
    mani = fixture(tmp_path)[0]
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: resp, raising=False)
    assert online.runtime_ok(tmp_path, mani) is False
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: good(m), raising=False)
    assert online.runtime_ok(tmp_path, mani) is True


def test_unchanged_untrusted_runtime_needs_repair_not_fallback(tmp_path, monkeypatch):
    mani, _, _, prefix = fixture(tmp_path)
    install_receipt(prefix, mani)
    monkeypatch.setattr(L, "read_receipt", lambda p: {"current": "v1"})
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: {"ok": False, "reason": "no seal"}, raising=False)
    with pytest.raises(online.Fail, match="Repair"):
        online.trusted_runtime(prefix, {"current": "v1"}, mani, None)
    changed = json.loads(json.dumps(mani))
    changed["files"]["runtime/lib/big.so"] = meta(b"new-runtime")
    assert online.trusted_runtime(prefix, {"current": "v1"}, changed, None) is False  # runtime changed: download path
    monkeypatch.setattr(L, "runtime_available", lambda p, m, s=None: good(m), raising=False)
    assert online.trusted_runtime(prefix, {"current": "v1"}, mani, None) is True


def test_progress_reports_each_second_and_marks_waiting(monkeypatch):
    lines = []
    monkeypatch.setattr(online, "say", lines.append)
    with online.Progress("w.whl", 4 << 20, phase="downloading") as p:
        p.tick(1 << 20)
        threading.Event().wait(2.3)
    assert len(lines) >= 2
    assert "downloading w.whl" in lines[0] and "MiB/s" in lines[0] and "elapsed" in lines[0]
    assert any("WAITING" in x for x in lines)  # no new bytes: marked as liveness, not progress


class _Resp:
    status = 200
    headers = {"Content-Length": "10"}

    def __init__(self, exc):
        self.exc = exc

    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def read(self, n):
        raise self.exc


class _Fx:
    calls = 0

    def open(self, url, offset):
        _Fx.calls += 1
        return _Resp(TimeoutError())


def test_stalled_download_fails_precisely_after_bounded_retries(tmp_path, monkeypatch):
    monkeypatch.setattr(online, "RETRIES", 2)
    monkeypatch.setattr(online.time, "sleep", lambda s: None)
    monkeypatch.setattr(online, "say", lambda m: None)
    w = {"url": "https://example.com/x.whl", "sha256": "a" * 64, "size": 10}
    with pytest.raises(online.Fail, match="stalled: no data for"):
        online.download(_Fx(), w, tmp_path)
    assert _Fx.calls == 2 and not (tmp_path / ("a" * 64 + ".whl")).exists()


def test_lifecycle_deadline_is_failure_and_attach_args(tmp_path, monkeypatch):
    sleeper = tmp_path / "lc.py"
    sleeper.write_text("import time\ntime.sleep(30)\n")
    tree = tmp_path / "t"
    (tree / "app/packaging").mkdir(parents=True)
    (tree / "app/packaging/lifecycle.py").write_text("import time\ntime.sleep(30)\n")
    monkeypatch.setattr(online, "FAST_DEADLINE", 1)
    monkeypatch.setattr(online, "PHASE_LIMIT", 2)
    monkeypatch.setattr(online, "say", lambda m: None)
    assert online.run_lifecycle(tmp_path, tree, "repair", "skip", None, attach=True) == 124
    monkeypatch.setattr(online, "FAST_DEADLINE", 30)
    seen = []
    monkeypatch.setattr(online.subprocess, "Popen", lambda cmd, env: seen.append(cmd) or type("P", (), {"wait": lambda s, timeout=None: 0})())
    assert online.run_lifecycle(tmp_path, tree, "install", "skip", tmp_path / "old", attach=True) == 0
    assert "--attach-runtime" not in seen[0] and "--runtime-src" not in seen[0]  # unpublished core flags: never passed
    assert seen[0][seen[0].index("--adopt-from") + 1] == str(tmp_path / "old")
