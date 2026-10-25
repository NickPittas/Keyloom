"""Keyloom install lifecycle: reinstall and update keep settings and models, never re-create them (Linux paths, tiny fake payloads)."""
import json
import os
import shutil
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from tests.test_packaging import env, install, make_payload, run  # noqa: E402,F401  (fixtures + helpers)
from tests.test_packaging import lifecycle  # noqa: E402
from backend import network_service as ns  # noqa: E402
from backend import install_paths as ip  # noqa: E402

pytestmark = pytest.mark.skipif(os.name == "nt", reason="Linux paths")


def test_fresh_install_has_no_legacy_names(env):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 0  # models skipped on request is a success
    assert not (prefix / "bin/corridorkey").exists()
    assert lifecycle.read_receipt(prefix)["app"] == "Keyloom"
    assert not (Path(os.environ["XDG_DATA_HOME"]) / "applications/corridorkey.desktop").exists()


def test_only_keyloom_version_names_are_launched(env):
    v = env / "oldver"
    (v / "bin").mkdir(parents=True)
    (v / "share").mkdir()
    (v / "bin/corridorkey-ui").write_text("x")  # a CorridorKey-named leftover is ignored
    (v / "share/keyloom.png").write_text("x")
    assert lifecycle.ui_exe(v).name == "keyloom-ui"
    assert lifecycle.descriptor_file(v).name == "keyloom-install.json"
    assert lifecycle.icon_file(v).name == "keyloom.png"


def test_user_folders_are_keyloom_only(tmp_path):
    (tmp_path / "corridorkey-native").mkdir()
    assert ns.settings_dir(tmp_path) == tmp_path / "keyloom"
    share = tmp_path / "share"
    (share / "CorridorKey").mkdir(parents=True)
    assert ip.unix_user_data(str(share), None) == share / "Keyloom"
    assert ip.unix_user_data(str(tmp_path / "none"), None) == tmp_path / "none" / "Keyloom"


def test_same_build_is_a_noop(env, monkeypatch, capsys):
    prefix = env / "opt" / "Keyloom"
    p = make_payload(env, "p1", "a")
    assert install(env, p, prefix) == 0
    _no_models(monkeypatch)
    before = {q: q.read_bytes() for q in prefix.rglob("*") if q.is_file() and ".txn" not in q.name and "install-result" not in q.name}
    apps = Path(os.environ["XDG_DATA_HOME"]) / "applications" / "keyloom.desktop"
    mt = apps.stat().st_mtime_ns
    monkeypatch.setattr(lifecycle, "activate", lambda *a, **k: (_ for _ in ()).throw(AssertionError("must not activate")))
    p_again = make_payload(env, "p1b", "a")
    assert install(env, p_again, prefix) == 0
    assert "already installed and verified" in capsys.readouterr().out
    assert json.loads((prefix / "install-result.json").read_text())["already_current"] is True
    assert {q: q.read_bytes() for q in before} == before and apps.stat().st_mtime_ns == mt
    vid = lifecycle.read_receipt(prefix)["current"]
    assert run("--prefix", str(prefix), "check", "--version", vid) == 0
    assert run("--prefix", str(prefix), "check", "--version", "0.0-nothere") == 1


def _no_models(monkeypatch):
    def boom(*a, **k):
        raise AssertionError("a reinstall or update must never download models")
    monkeypatch.setattr(lifecycle, "run_models", boom)


def _seed_settings(tmp_path_home):
    d = Path(os.environ["HOME"]) / ".config/corridorkey-native"
    (d / "network/server").mkdir(parents=True)
    body = '{"zz_unknown":{"k":[1,2]},"models":{"root":"/my/custom/models"},"network":{"host_removed":true,"name":"X"}}'
    (d / "settings.json").write_text(body)
    (d / "network/server/host-removed.marker").write_text("")
    (d / "network/server/server-cert.pem").write_text("CERT")
    snap = {p.relative_to(d): p.read_bytes() for p in d.rglob("*") if p.is_file()}
    return d, snap


def test_update_keeps_settings_models_and_reuses_runtime(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 0
    lifecycle.record_models(prefix, {"profile": "full", "ready": True})
    d, snap = _seed_settings(env)
    _no_models(monkeypatch)
    p2 = make_payload(env, "p2", "b")
    shutil.rmtree(p2 / "runtime")  # no runtime of its own: the update attaches the installed one (one link)
    install(env, p2, prefix)
    r = lifecycle.read_receipt(prefix)
    res = json.loads((prefix / "install-result.json").read_text())
    assert res["runtime_reuse"]["ok"] is True
    assert {q.relative_to(d): q.read_bytes() for q in d.rglob("*") if q.is_file()} == snap  # byte-identical, incl. unknown keys
    assert r["models"]["ready"] is True or r["models"].get("unverified")  # prior record kept, never reset to a download
    assert lifecycle.is_link(prefix / "versions" / r["current"] / "runtime")  # attached by reference, not copied


def test_changed_catalog_keeps_models_and_marks_catalog_changed_without_download(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 0
    lifecycle.record_models(prefix, {"profile": "full", "ready": True})
    _no_models(monkeypatch)
    p2 = make_payload(env, "p2", "b")
    m = json.loads((p2 / "install-manifest.json").read_text())
    m["catalog_sha256"] = "different"
    (p2 / "install-manifest.json").write_text(json.dumps(m))
    assert install(env, p2, prefix) == 0
    got = lifecycle.read_receipt(prefix)["models"]
    assert got["ready"] is True and got["catalog_changed"] is True


def test_default_models_flag_is_skip_and_full_is_explicit():
    a = lifecycle.build_parser().parse_args(["--prefix", "p", "install", "--payload", "x"])
    assert a.models == "skip"
    assert lifecycle.build_parser().parse_args(["--prefix", "p", "install", "--models", "full"]).models == "full"


def test_repair_does_not_touch_settings_or_download(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    p = make_payload(env, "p1", "a")
    assert install(env, p, prefix) == 0
    d, snap = _seed_settings(env)
    _no_models(monkeypatch)
    vid = lifecycle.read_receipt(prefix)["current"]
    (prefix / "versions" / vid / "share/keyloom.png").write_text("tampered")
    assert run("--prefix", str(prefix), "repair", "--payload", str(make_payload(env, "p1r", "a")), "--firewall", "skip") == 0
    assert (prefix / "versions" / vid / "share/keyloom.png").read_bytes() == b"png"
    assert {q.relative_to(d): q.read_bytes() for q in d.rglob("*") if q.is_file()} == snap


def test_state_file_read_survives_a_concurrent_replace(tmp_path, monkeypatch):
    f = tmp_path / "service-install.json"
    f.write_text('{"version": 1}')
    real = Path.read_text
    n = {"i": 0}

    def flaky(self, *a, **k):
        if self.name == "service-install.json" and n["i"] < 3:
            n["i"] += 1
            raise PermissionError("sharing violation")
        return real(self, *a, **k)
    monkeypatch.setattr(Path, "read_text", flaky)
    assert ns._read_json(f) == {"version": 1}
    n["i"] = -100  # always failing: a clear error naming the cause, the file untouched
    with pytest.raises(ns.ServiceError, match="PermissionError"):
        ns._read_json(f)
    assert real(f) == '{"version": 1}'


def test_annotation_model_is_backend_and_qt_free(tmp_path):
    import subprocess
    code = ("import sys; from backend.annotations import AnnotationModel; "
            "assert not any(m.startswith('PySide6') for m in sys.modules), 'Qt imported'")
    r = subprocess.run([sys.executable, "-I", "-c", code], cwd=Path(__file__).resolve().parent.parent,
                       env={**os.environ, "PYTHONPATH": str(Path(__file__).resolve().parent.parent)}, capture_output=True, text=True)
    assert r.returncode == 0, r.stderr
    from backend.annotations import AnnotationModel
    m = AnnotationModel()
    assert not m.has_annotations()


# ---- runtime link (all or nothing) ---------------------------------------------------------------------------------

def _two_versions(env, prefix, change_py=False):
    assert install(env, make_payload(env, "p1", "a"), prefix) == 0
    p2 = make_payload(env, "p2", "b")
    return p2


def test_attached_runtime_is_one_link_and_never_replaces_a_runtime(env):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 0
    cur = lifecycle.read_receipt(prefix)["current"]
    # a new version dir that carries the manifest but none of the runtime files
    p2 = make_payload(env, "p2", "b")
    vid2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    new = prefix / "versions" / vid2
    new.mkdir(parents=True)
    (new / "install-manifest.json").write_bytes((p2 / "install-manifest.json").read_bytes())
    r = lifecycle._attach(prefix, new)
    assert r["ok"] and (new / "runtime").is_symlink()
    assert (new / "runtime/python/bin/python3").stat().st_ino == (prefix / "versions" / cur / "runtime/python/bin/python3").stat().st_ino
    # a version that already has its own runtime is refused, and nothing is linked over it
    own = prefix / "versions" / "9.9-own"
    (own / "runtime/python/bin").mkdir(parents=True)
    (own / "install-manifest.json").write_bytes((p2 / "install-manifest.json").read_bytes())
    r = lifecycle._attach(prefix, own)
    assert not r["ok"] and "own runtime" in r["reason"] and not (own / "runtime").is_symlink()


def test_online_installer_default_prefix_is_keyloom(tmp_path, monkeypatch):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "packaging"))
    import online
    monkeypatch.setenv("HOME", str(tmp_path))
    assert online.default_prefix() == tmp_path / ".local/opt/Keyloom"
