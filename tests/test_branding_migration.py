"""CorridorKey -> Keyloom rename: an existing install is migrated in place, never re-created (Linux paths, tiny fake payloads)."""
import json
import os
import sys
from pathlib import Path

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from tests.test_packaging import env, install, make_payload, run  # noqa: E402,F401  (fixtures + helpers)
from tests.test_packaging import lifecycle  # noqa: E402
from backend import network_service as ns  # noqa: E402
from backend import install_paths as ip  # noqa: E402

pytestmark = pytest.mark.skipif(os.name == "nt", reason="Linux paths")


def _legacy_prefix(env, prefix):
    """A prefix as the pre-rename installer left it: old receipt id, old shims, old desktop + autostart entries."""
    p1 = make_payload(env, "p1", "a")
    assert install(env, p1, prefix) == 3
    r = lifecycle.read_receipt(prefix)
    r["app"] = "CorridorKey"
    (prefix / "receipt.json").write_text(json.dumps(r))
    for new, old in zip(lifecycle.SHIM_NAMES, lifecycle.LEGACY_SHIM_NAMES):
        (prefix / "bin" / old).write_text((prefix / "bin" / new).read_text())
    owner = lifecycle.owner_id(prefix)
    apps = Path(os.environ["XDG_DATA_HOME"]) / "applications"
    desk = (apps / "keyloom.desktop")
    (apps / "corridorkey.desktop").write_text(desk.read_text().replace("X-Keyloom-Owner", "X-CorridorKey-Owner"))
    desk.unlink()
    auto = Path(os.environ["HOME"]) / ".config/autostart"
    auto.mkdir(parents=True)
    (auto / "corridorkey.desktop").write_text(
        f'[Desktop Entry]\nType=Application\nName=CorridorKey\nExec="{prefix}/bin/corridorkey" --background\nX-CorridorKey-Owner={owner}\n')
    return apps, auto


def test_upgrade_migrates_owned_entries_and_keeps_identity(env, monkeypatch):
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    prefix = env / "opt" / "CorridorKey"
    apps, auto = _legacy_prefix(env, prefix)
    foreign = Path(os.environ["HOME"]) / ".config/autostart/other.desktop"
    foreign.write_text("[Desktop Entry]\nName=other\n")
    install(env, make_payload(env, "p2", "b"), prefix)
    r = lifecycle.read_receipt(prefix)
    assert r["app"] == "CorridorKey"  # a rollback to the old lifecycle must still recognise the receipt
    assert (apps / "keyloom.desktop").is_file() and not (apps / "corridorkey.desktop").exists()
    assert f"X-Keyloom-Owner={lifecycle.owner_id(prefix)}" in (apps / "keyloom.desktop").read_text()
    assert (auto / "keyloom.desktop").is_file() and not (auto / "corridorkey.desktop").exists()
    text = (auto / "keyloom.desktop").read_text()
    assert f'Exec="{prefix}/bin/keyloom" --background' in text and "Name=Keyloom" in text
    assert foreign.read_text() == "[Desktop Entry]\nName=other\n"
    for old in lifecycle.LEGACY_SHIM_NAMES:  # old commands keep forwarding
        assert (prefix / "bin" / old).is_file()
    assert run("--prefix", str(prefix), "uninstall", "--skip-running", "--firewall", "skip") == 0
    assert not (auto / "keyloom.desktop").exists() and foreign.exists() and not prefix.exists()


def test_fresh_install_has_no_legacy_names(env):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 3
    assert not any((prefix / "bin" / n).exists() for n in lifecycle.LEGACY_SHIM_NAMES)
    assert lifecycle.read_receipt(prefix)["app"] == "Keyloom"
    assert not (Path(os.environ["XDG_DATA_HOME"]) / "applications/corridorkey.desktop").exists()


def test_foreign_autostart_with_new_name_is_left_alone(env):
    prefix = env / "opt" / "CorridorKey"
    _, auto = _legacy_prefix(env, prefix)
    (auto / "keyloom.desktop").write_text("[Desktop Entry]\nName=somebody else\n")
    lifecycle.migrate_autostart(prefix / "versions", prefix)
    assert (auto / "keyloom.desktop").read_text() == "[Desktop Entry]\nName=somebody else\n"
    assert (auto / "corridorkey.desktop").is_file()


def test_old_version_dir_still_launches_after_rollback_names(env):
    v = env / "oldver"
    (v / "bin").mkdir(parents=True)
    (v / "share").mkdir()
    (v / "bin/corridorkey-ui").write_text("x")
    (v / "corridorkey-install.json").write_text("{}")
    (v / "share/corridorkey.png").write_text("x")
    assert lifecycle.ui_exe(v).name == "corridorkey-ui"
    assert lifecycle.descriptor_file(v).name == "corridorkey-install.json"
    assert lifecycle.icon_file(v).name == "corridorkey.png"
    (v / "bin/keyloom-ui").write_text("x")
    (v / "keyloom-install.json").write_text("{}")
    assert lifecycle.ui_exe(v).name == "keyloom-ui" and lifecycle.descriptor_file(v).name == "keyloom-install.json"


def test_user_folders_are_read_where_they_are(tmp_path):
    (tmp_path / "corridorkey-native").mkdir()
    assert ns.settings_dir(tmp_path) == tmp_path / "corridorkey-native"
    (tmp_path / "keyloom").mkdir()
    assert ns.settings_dir(tmp_path) == tmp_path / "keyloom"
    share = tmp_path / "share"
    (share / "CorridorKey").mkdir(parents=True)
    assert ip.unix_user_data(str(share), None) == share / "CorridorKey"
    assert ip.unix_user_data(str(tmp_path / "none"), None) == tmp_path / "none" / "Keyloom"
    d = tmp_path / "app"
    d.mkdir()
    (d / ip.LEGACY_DESCRIPTOR_NAME).write_text("{}")
    assert ip.descriptor_in(d).name == ip.LEGACY_DESCRIPTOR_NAME


def test_owned_legacy_service_is_replaced_not_duplicated(tmp_path, monkeypatch):
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "cfg"))
    calls = []
    monkeypatch.setattr(ns, "_systemctl", lambda *a: calls.append(a))
    monkeypatch.setattr(ns, "service_python", lambda p: Path(p))
    monkeypatch.setattr(ns, "IS_WIN", False)
    sd = tmp_path / "state"
    sd.mkdir()
    (sd / "server-cert.pem").write_text("CERT")
    (sd / "service-install.json").write_text(json.dumps({"version": 1, "owner": "me", "mode": "systemd"}))
    units = tmp_path / "cfg/systemd/user"
    units.mkdir(parents=True)
    old = units / "corridorkey-network.service"
    old.write_text(f"# X-CorridorKey-Network-Owner=me\n# X-CorridorKey-State-Dir={sd}\n[Service]\n")
    assert ns._migrate_legacy(Path("/py"), Path("/app"), "me")
    new = units / "keyloom-network.service"
    assert new.is_file() and not old.exists()
    assert f"X-Keyloom-State-Dir={sd}" in new.read_text()
    assert (sd / "server-cert.pem").read_text() == "CERT"  # identity untouched
    assert ("enable", "keyloom-network.service") in calls and ("disable", "corridorkey-network.service") in calls
    assert ns._owned(sd)["mode"] == "systemd"
    # somebody else's legacy definition stays exactly as it is
    old.write_text(f"# X-CorridorKey-State-Dir={sd}\n")
    (sd / "service-install.json").write_text(json.dumps({"version": 1, "owner": "other", "mode": "systemd"}))
    assert ns._migrate_legacy(Path("/py"), Path("/app"), "me") is None and old.is_file()


# ---- reinstall / update preservation ------------------------------------------------------------------------------

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


def test_same_build_is_a_noop(env, monkeypatch, capsys):
    prefix = env / "opt" / "Keyloom"
    p = make_payload(env, "p1", "a")
    assert install(env, p, prefix) == 3
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


def test_update_keeps_settings_models_and_reuses_runtime(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 3
    lifecycle.record_models(prefix, {"profile": "full", "ready": True})
    d, snap = _seed_settings(env)
    _no_models(monkeypatch)
    p2 = make_payload(env, "p2", "b")
    install(env, p2, prefix)
    r = lifecycle.read_receipt(prefix)
    res = json.loads((prefix / "install-result.json").read_text())
    assert res["runtime_reuse"]["linked"] >= 1 and res["runtime_reuse"]["bytes"] > 0  # python3 stand-in is unchanged
    assert {q.relative_to(d): q.read_bytes() for q in d.rglob("*") if q.is_file()} == snap  # byte-identical, incl. unknown keys
    assert r["models"]["ready"] is True or r["models"].get("unverified")  # prior record kept, never reset to a download


def test_changed_catalog_marks_models_unverified_without_download(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 3
    lifecycle.record_models(prefix, {"profile": "full", "ready": True})
    _no_models(monkeypatch)
    p2 = make_payload(env, "p2", "b")
    m = json.loads((p2 / "install-manifest.json").read_text())
    m["catalog_sha256"] = "different"
    (p2 / "install-manifest.json").write_text(json.dumps(m))
    assert install(env, p2, prefix) == 3
    got = lifecycle.read_receipt(prefix)["models"]
    assert got["ready"] is True and got["unverified"] is True


def test_default_models_flag_is_skip_and_full_is_explicit():
    a = lifecycle.build_parser().parse_args(["--prefix", "p", "install", "--payload", "x"])
    assert a.models == "skip"
    assert lifecycle.build_parser().parse_args(["--prefix", "p", "install", "--models", "full"]).models == "full"


def test_repair_does_not_touch_settings_or_download(env, monkeypatch):
    prefix = env / "opt" / "Keyloom"
    p = make_payload(env, "p1", "a")
    assert install(env, p, prefix) == 3
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
    import importlib
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
    assert install(env, make_payload(env, "p1", "a"), prefix) == 3
    p2 = make_payload(env, "p2", "b")
    return p2


def test_link_runtime_is_all_or_nothing(env):
    prefix = env / "opt" / "Keyloom"
    assert install(env, make_payload(env, "p1", "a"), prefix) == 3
    cur = lifecycle.read_receipt(prefix)["current"]
    new = prefix / "versions" / "9.9-new"
    # a new version dir that carries the manifest but none of the runtime files
    p2 = make_payload(env, "p2", "b")
    vid2 = json.loads((p2 / "install-manifest.json").read_text())["version_id"]
    new = prefix / "versions" / vid2
    new.mkdir(parents=True)
    (new / "install-manifest.json").write_bytes((p2 / "install-manifest.json").read_bytes())
    r = lifecycle.link_runtime(prefix, vid2)
    assert r["ok"] and r["linked"] >= 1 and (new / "runtime/python/bin/python3").stat().st_ino == (prefix / "versions" / cur / "runtime/python/bin/python3").stat().st_ino
    # damaged source file -> nothing is linked, nothing is left behind
    for f in (new / "runtime").rglob("*"):
        if f.is_file() or f.is_symlink():
            f.unlink()
    (prefix / "versions" / cur / "runtime/python/bin/python3").write_text("corrupt")
    r = lifecycle.link_runtime(prefix, vid2)
    assert not r["ok"] and r["linked"] == 0 and not any(f.is_file() for f in (new / "runtime").rglob("*"))
    assert (prefix / "versions" / cur / "runtime/python/bin/python3").read_text() == "corrupt"  # the source is never touched


# ---- prefix hand-off (old CorridorKey prefix -> Keyloom prefix) --------------------------------------------------------

def test_adopt_moves_ownership_to_the_new_prefix_and_keeps_old_as_holder(env, monkeypatch):
    monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    old = env / "opt" / "CorridorKey"
    new = env / "opt" / "Keyloom"
    apps, auto = _legacy_prefix(env, old)
    lifecycle.record_models(old, {"profile": "full", "ready": True})
    _no_models(monkeypatch)
    rc = run("--prefix", str(new), "install", "--payload", str(make_payload(env, "pn", "a")), "--models", "skip", "--firewall", "skip",
             "--adopt-from", str(old))
    assert rc == 0  # models ready and unchanged catalog carried over
    r = lifecycle.read_receipt(new)
    assert r["adopted_from"] == str(old) and r["models"]["ready"] is True and r["app"] == "Keyloom"
    o = lifecycle.read_receipt(old)
    assert o["holder_of"] == str(new) and o["current"]  # old prefix intact, only marked
    assert (old / "versions" / o["current"]).is_dir()
    own = lifecycle.owner_id(new)
    assert f"X-Keyloom-Owner={own}" in (apps / "keyloom.desktop").read_text() and str(new) in (apps / "keyloom.desktop").read_text()
    assert not (apps / "corridorkey.desktop").exists()
    text = (auto / "keyloom.desktop").read_text()
    assert f"X-Keyloom-Owner={own}" in text and f'Exec="{new}/bin/keyloom"' in text and str(old) not in text
    assert not (auto / "corridorkey.desktop").exists()
    res = json.loads((new / "install-result.json").read_text())
    assert res["runtime_reuse"]["linked"] >= 1
    # the next run over the migrated prefix is a clean no-op (no adoption again, nothing changes)
    assert run("--prefix", str(new), "install", "--payload", str(make_payload(env, "pn2", "a")), "--models", "skip") == 0
    assert json.loads((new / "install-result.json").read_text())["already_current"] is True
    # rolling back still has the old version, and uninstalling the old holder leaves the new prefix alone
    assert run("--prefix", str(old), "uninstall", "--skip-running", "--firewall", "skip") == 0
    assert not old.exists() and (new / "current").exists() and (auto / "keyloom.desktop").exists()
    assert (apps / "keyloom.desktop").exists() and lifecycle.read_receipt(new).get("firewall") == lifecycle.read_receipt(new).get("firewall")


def test_adopt_refuses_a_foreign_prefix(env):
    foreign = env / "foreign"
    foreign.mkdir()
    (foreign / "receipt.json").write_text('{"app": "SomethingElse", "versions": []}')
    new = env / "opt" / "Keyloom"
    with pytest.raises(lifecycle.InstallError):
        lifecycle.adopt_source(type("A", (), {"adopt_from": str(foreign)})(), new)
    assert (foreign / "receipt.json").read_text() == '{"app": "SomethingElse", "versions": []}'


# ---- settings folder rename ------------------------------------------------------------------------------------------

def _settings_tree(base):
    old = base / "corridorkey-native"
    (old / "network/server").mkdir(parents=True)
    body = ('{"zz_unknown": {"k": [1, 2]}, "models": {"root": "/my/custom/models"}, "network": {"host_removed": true, "name": "X", '
            f'"cred": {json.dumps(str(old / "network/clients/a.json"))}}}}}')
    (old / "settings.json").write_text(body)
    (old / "instance.lock").write_text("")
    (old / "network/server/server-cert.pem").write_text("CERT")
    (old / "network/server/host-removed.marker").write_text("")
    return old, body


def test_settings_folder_rename_keeps_everything_and_leaves_a_link(tmp_path):
    old, body = _settings_tree(tmp_path)
    r = ns.migrate_settings_dir(tmp_path, ("me",))
    new = tmp_path / "keyloom"
    assert r["moved"] and new.is_dir() and old.is_symlink() and old.resolve() == new.resolve()
    assert (new / "network/server/server-cert.pem").read_text() == "CERT" and (new / "network/server/host-removed.marker").exists()
    want = body.replace(json.dumps(str(old))[1:-1], json.dumps(str(new))[1:-1])
    assert (new / "settings.json").read_text() == want  # only the path changed; unknown keys, order and spacing intact
    assert json.loads(want)["models"]["root"] == "/my/custom/models"  # custom roots are never rewritten
    assert not (tmp_path / ".keyloom-migrate.json").exists()
    assert ns.migrate_settings_dir(tmp_path, ("me",)) is None  # idempotent
    assert ns.settings_dir(tmp_path) == new


def test_settings_folder_stays_put_when_the_app_or_a_foreign_service_is_running(tmp_path, monkeypatch):
    old, body = _settings_tree(tmp_path)
    import fcntl
    held = open(old / "instance.lock", "a")
    fcntl.flock(held, fcntl.LOCK_EX)
    r = ns.migrate_settings_dir(tmp_path, ("me",))
    held.close()
    assert not r["moved"] and "running" in r["message"] and old.is_dir() and not old.is_symlink() and not (tmp_path / "keyloom").exists()
    monkeypatch.setattr(ns, "lock_busy", lambda sd: True)
    r = ns.migrate_settings_dir(tmp_path, ("me",))
    assert not r["moved"] and "does not own" in r["message"] and (old / "settings.json").read_text() == body


def test_interrupted_rename_is_finished_not_lost(tmp_path):
    old, body = _settings_tree(tmp_path)
    new = tmp_path / "keyloom"
    ns._write_plain(tmp_path / ".keyloom-migrate.json", {"version": 1, "from": str(old), "to": str(new)})
    os.rename(old, new)  # crash right after the rename: no link, paths not yet rewritten
    ns.migrate_settings_dir(tmp_path, ("me",))
    assert old.is_symlink() and not (tmp_path / ".keyloom-migrate.json").exists()
    assert str(old) not in (new / "settings.json").read_text() and (new / "network/server/server-cert.pem").exists()
    # crash BEFORE the rename: journal only, folder untouched, next run migrates normally
    tmp2 = tmp_path / "b"
    tmp2.mkdir()
    old2, _ = _settings_tree(tmp2)
    ns._write_plain(tmp2 / ".keyloom-migrate.json", {"version": 1, "from": str(old2), "to": str(tmp2 / "keyloom")})
    r = ns.migrate_settings_dir(tmp2, ("me",))
    assert r["moved"] and (tmp2 / "keyloom/settings.json").exists()


def test_online_installer_only_adopts_the_old_default_prefix(tmp_path, monkeypatch):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent / "packaging"))
    import online
    monkeypatch.setenv("HOME", str(tmp_path))
    new, old = online.default_prefix(), tmp_path / ".local/opt/CorridorKey"
    assert new == tmp_path / ".local/opt/Keyloom" and online.legacy_prefix(new) is None
    old.mkdir(parents=True)
    (old / "receipt.json").write_text("{}")
    assert online.legacy_prefix(new) == old
    assert online.legacy_prefix(tmp_path / "elsewhere") is None  # an explicit prefix is never replaced by the old one
    (new).mkdir(parents=True)
    (new / "receipt.json").write_text("{}")
    assert online.legacy_prefix(new) is None  # already migrated
