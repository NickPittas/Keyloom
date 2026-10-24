"""Fast shared-runtime reuse (lifecycle.runtime_id / runtime_available): tiny fixtures, no runtime hashing."""
import hashlib
import json
import os
import shutil
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "packaging"))
import lifecycle  # noqa: E402

PY = f"runtime/{lifecycle.RUNTIME_PY}"


def manifest(tag="a"):
    return {"schema": 1, "version_id": "1-x", "files": {
        PY: {"size": 1, "sha256": "0" * 64}, "runtime/lib/m.py": {"size": 1, "sha256": tag * 64},
        "app/x.py": {"size": 1, "sha256": "1" * 64}}}


def legacy(prefix: Path, m: dict, *, receipt=True, interp=True) -> Path:
    v = prefix / "versions" / "1-x"
    (v / "runtime/python/bin").mkdir(parents=True)
    if interp:
        (v / PY).write_text("x")
    (v / lifecycle.MANIFEST).write_text(json.dumps(m))
    if receipt:
        lifecycle.atomic_write_json(prefix / lifecycle.RECEIPT, {
            "schema": 1, "app": "Keyloom", "prefix": str(prefix), "current": "1-x", "versions": ["1-x"],
            "activated_at": "2026-01-01T00:00:00+0000"})
    return v


@pytest.fixture
def no_runtime_io(monkeypatch):
    def boom(*a, **k):
        raise AssertionError("runtime content touched")
    for mod, n in ((lifecycle, "sha256_file"), (shutil, "copy2"), (shutil, "copytree"), (os, "link"), (os, "walk")):
        monkeypatch.setattr(mod, n, boom)


def test_runtime_id_matches_agreed_formula():
    m = manifest()
    sub = {k: v for k, v in m["files"].items() if k.startswith("runtime/")}
    want = hashlib.sha256(json.dumps(sub, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    assert lifecycle.runtime_id(m["files"]) == want and len(want) == 64
    assert lifecycle.runtime_id(manifest("b")["files"]) != want


def test_legacy_receipt_reuse_without_hash_or_walk(tmp_path, no_runtime_io):
    m = manifest()
    legacy(tmp_path, m)
    r = lifecycle.runtime_available(tmp_path, m)
    assert r["ok"] and r["source"] == "legacy" and r["rid"] == lifecycle.runtime_id(m["files"])
    assert Path(r["path"]) == (tmp_path / "versions/1-x/runtime").resolve()
    m2 = dict(m, runtime_id=r["rid"])  # producer-stamped id is accepted when it matches
    assert lifecycle.runtime_available(tmp_path, m2)["ok"]


@pytest.mark.parametrize("case", ["other_runtime", "bad_stamp", "no_interp", "no_receipt", "foreign_app", "wrong_prefix"])
def test_rejects(tmp_path, case):
    m = manifest()
    v = legacy(tmp_path, m, receipt=case != "no_receipt", interp=case != "no_interp")
    want = manifest("b") if case == "other_runtime" else dict(m, runtime_id="f" * 64) if case == "bad_stamp" else m
    if case in ("foreign_app", "wrong_prefix"):
        rc = lifecycle.read_receipt(tmp_path)
        rc.update(app="Other") if case == "foreign_app" else rc.update(prefix="/elsewhere")
        lifecycle.atomic_write_json(tmp_path / lifecycle.RECEIPT, rc)
    r = lifecycle.runtime_available(tmp_path, want)
    assert not r["ok"] and r["reason"] and r["source"] is None and v.is_dir()


def test_runtime_outside_prefix_rejected(tmp_path):
    m = manifest()
    out = tmp_path / "outside/runtime/python/bin"
    out.mkdir(parents=True)
    (out / "python3").write_text("x")
    prefix = tmp_path / "p"
    v = legacy(prefix, m)
    shutil.rmtree(v / "runtime")
    os.symlink(tmp_path / "outside/runtime", v / "runtime")  # owner link escaping the prefix
    assert not lifecycle.runtime_available(prefix, m)["ok"]
