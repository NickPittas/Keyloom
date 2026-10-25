"""attach_runtime / retention / holder uninstall: one link, never file iteration; fixtures only (Windows junction unproven)."""
import json
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_fast_runtime import PY, legacy, manifest, no_runtime_io  # noqa: E402,F401
import lifecycle  # noqa: E402


def new_version(prefix: Path, m: dict, vid="2-y", real_runtime=False) -> Path:
    v = prefix / "versions" / vid
    v.mkdir(parents=True)
    (v / lifecycle.MANIFEST).write_text(json.dumps(dict(m, version_id=vid)))
    if real_runtime:
        (v / PY).parent.mkdir(parents=True)
        (v / PY).write_text("own")
    return v


def test_attach_is_one_link(tmp_path, no_runtime_io):
    m = manifest()
    owner = legacy(tmp_path, m)
    v = new_version(tmp_path, m)
    r = lifecycle.attach_runtime(tmp_path, "2-y")
    assert r["ok"] and r["source"] == "legacy" and os.path.islink(v / "runtime")
    assert (v / "runtime").resolve() == (owner / "runtime").resolve()
    assert lifecycle.attach_runtime(tmp_path, "2-y")["ok"]  # idempotent
    assert lifecycle.runtime_in_use(tmp_path, owner)


def test_attach_refuses_without_changes(tmp_path):
    legacy(tmp_path, manifest())
    v = new_version(tmp_path, manifest("b"))  # different runtime
    r = lifecycle.attach_runtime(tmp_path, "2-y")
    assert not r["ok"] and r["reason"] and not os.path.lexists(v / "runtime")
    v3 = new_version(tmp_path, manifest(), "3-z", real_runtime=True)  # own runtime is never replaced
    assert not lifecycle.attach_runtime(tmp_path, "3-z")["ok"] and not os.path.islink(v3 / "runtime")


def test_prune_retains_referenced_owner_then_releases(tmp_path):
    m = manifest()
    owner = legacy(tmp_path, m)
    new_version(tmp_path, m)
    assert lifecycle.attach_runtime(tmp_path, "2-y")["ok"]
    rc = {"current": "2-y", "previous": None, "versions": ["1-x", "2-y"]}
    lifecycle.prune_versions(tmp_path, rc)
    assert owner.is_dir() and rc["versions"] == ["1-x", "2-y"]
    lifecycle.unlink_link(tmp_path / "versions/2-y/runtime")  # no longer referenced
    lifecycle.prune_versions(tmp_path, rc)
    assert not owner.exists() and rc["versions"] == ["2-y"]


def test_rmtree_of_referencing_version_keeps_owner_runtime(tmp_path):
    m = manifest()
    owner = legacy(tmp_path, m)
    v = new_version(tmp_path, m)
    assert lifecycle.attach_runtime(tmp_path, "2-y")["ok"]
    assert lifecycle.try_rmtree(v) and not v.exists()
    assert (owner / PY).is_file()


def test_cli_exit_codes(tmp_path, capsys):
    m = manifest()
    legacy(tmp_path, m)
    new_version(tmp_path, m)
    mp = tmp_path / "m.json"
    mp.write_text(json.dumps(m))
    p = str(tmp_path)
    assert lifecycle.main(["--prefix", p, "runtime-available", "--manifest", str(mp)]) == 0
    mp.write_text(json.dumps(manifest("b")))
    assert lifecycle.main(["--prefix", p, "runtime-available", "--manifest", str(mp)]) == 1
    assert lifecycle.main(["--prefix", p, "runtime-available", "--manifest", str(tmp_path / "none.json")]) == 2  # unreadable manifest
    with pytest.raises(SystemExit):  # the old link-runtime alias is gone
        lifecycle.main(["--prefix", p, "link-runtime", "--version", "2-y"])
    assert lifecycle.main(["--prefix", p, "attach-runtime", "--version", "2-y"]) == 0


def _env(tmp_path, monkeypatch):
    for k, d in (("HOME", "home"), ("XDG_DATA_HOME", "xd"), ("XDG_CONFIG_HOME", "xc")):
        monkeypatch.setenv(k, str(tmp_path / d))


def test_corridorkey_install_is_never_a_runtime_source(tmp_path, monkeypatch):
    """CorridorKey adoption is gone: a CorridorKey prefix is not consulted, so a Keyloom prefix cannot attach its runtime."""
    _env(tmp_path, monkeypatch)
    m = manifest()
    old, new = tmp_path / "old", tmp_path / "new"
    legacy(old, m)
    lifecycle.atomic_write_json(old / lifecycle.RECEIPT, dict(lifecycle.read_receipt(old), app="CorridorKey"))
    new_version(new, m, "2-y")
    assert not lifecycle.attach_runtime(new, "2-y")["ok"] and not os.path.islink(new / "versions/2-y/runtime")


def test_unvalidated_outside_link_is_not_trusted(tmp_path):
    m = manifest()
    other, new = tmp_path / "other", tmp_path / "new"
    legacy(other, m)  # an owned install, but with NO adopted_from/holder_of tie to `new`
    new_version(new, m, "2-y")
    os.symlink(other / "versions/1-x/runtime", new / "versions/2-y/runtime")
    lifecycle.atomic_write_json(new / lifecycle.RECEIPT, {"schema": 1, "app": "Keyloom", "prefix": str(new), "current": "2-y",
                                                          "versions": ["2-y"], "activated_at": "x"})
    assert not lifecycle.runtime_available(new, m)["ok"]


def _interrupted(prefix: Path, m: dict) -> Path:
    """Reinstall of 1-x was interrupted: 1-x.old-abc123 (real runtime) is the txn backup, a half-installed 1-x exists,
    and the kept version 3-w links to the backup's runtime."""
    owner = legacy(prefix, m)
    bd = prefix / "versions" / "1-x.old-abc123"
    owner.rename(bd)
    (prefix / "versions/1-x").mkdir()
    w = new_version(prefix, m, "3-w")
    os.symlink(bd / "runtime", w / "runtime")
    lifecycle.atomic_write_json(prefix / lifecycle.RECEIPT, {"schema": 1, "app": "Keyloom", "prefix": str(prefix), "current": "3-w",
                                                             "versions": ["3-w"], "activated_at": "x"})
    lifecycle.atomic_write_json(prefix / ".txn.json", {"version": "1-x", "backup": "1-x.old-abc123"})
    return bd


def test_recover_keeps_referenced_backup_then_sweeps_when_unreferenced(tmp_path, no_runtime_io):
    bd = _interrupted(tmp_path, manifest())
    lifecycle.recover(tmp_path)
    assert (bd / PY).is_file() and (tmp_path / "versions/3-w/runtime").resolve() == (bd / "runtime").resolve()
    assert not (tmp_path / ".txn.json").exists() and not (tmp_path / "versions/1-x").exists()
    lifecycle.unlink_link(tmp_path / "versions/3-w/runtime")
    lifecycle.recover(tmp_path)
    assert not bd.exists()


def test_discard_new_keeps_referenced_runtime(tmp_path):
    m = manifest()
    owner = legacy(tmp_path, m)
    new_version(tmp_path, m)
    assert lifecycle.attach_runtime(tmp_path, "2-y")["ok"]
    lifecycle.atomic_write_json(tmp_path / lifecycle.RECEIPT, {"schema": 1, "app": "Keyloom", "prefix": str(tmp_path),
                                                               "current": "2-y", "versions": ["2-y"]})
    lifecycle.discard_new(tmp_path, "1-x", None)
    assert (owner / PY).is_file()
