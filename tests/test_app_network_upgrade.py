"""A4: installers stop installing the background network service; the app hosts the endpoint.
Temp prefixes and fake systemctl / schtasks / ufw / powershell only: nothing here touches a real service, firewall or ~/.config."""
import fcntl
import json
import os
import stat
import sys
import threading
import time
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "packaging"))
import bundle  # noqa: E402
import firewall as fw  # noqa: E402
import legacy_network_cleanup as cl  # noqa: E402
import lifecycle  # noqa: E402
import online  # noqa: E402
from backend.coordinator.store import Store  # noqa: E402

UNIT_NAME = "keyloom-network.service"


class World:
    """An old-layout install: prefix, version folder, systemd user unit, state folder with a registered PC."""

    def __init__(self, tmp_path, monkeypatch):
        self.tmp = tmp_path
        self.prefix = tmp_path / "prefix"
        self.vdir = self.prefix / "versions" / "v2"
        self.xdg = tmp_path / "xdg"
        self.sd = tmp_path / "settings/keyloom/network/server"
        self.calls = tmp_path / "calls.log"
        bindir = tmp_path / "bin"
        bindir.mkdir()
        for name in ("systemctl", "schtasks", "ufw"):  # fakes: log argv, succeed
            f = bindir / name
            f.write_text(f'#!/bin/sh\necho "{name} $*" >> "{self.calls}"\nexit 0\n')
            f.chmod(f.stat().st_mode | stat.S_IEXEC)
        monkeypatch.setenv("PATH", f"{bindir}:{os.environ['PATH']}")
        monkeypatch.setenv("XDG_CONFIG_HOME", str(self.xdg))
        monkeypatch.setattr(cl, "IS_WIN", False)
        monkeypatch.setattr(cl, "STOP_WAIT", 0.5)
        monkeypatch.setattr(cl, "KILL_WAIT", 0.5)

        self.sd.mkdir(parents=True, mode=0o700)
        self.unit = self.xdg / "systemd/user" / UNIT_NAME
        self.unit.parent.mkdir(parents=True)
        self.unit.write_text(f"# X-Keyloom-Network-Owner=x\n# X-Keyloom-State-Dir={self.sd}\n[Service]\nExecStart=/x\n")
        self.prefix.mkdir()
        (self.prefix / cl.REGISTRY).write_text(json.dumps({"version": 1, "state_dir": str(self.sd)}))
        (self.sd / cl.INSTALL).write_text(json.dumps({"version": 1, "owner": cl.owner_id(self.prefix), "mode": "systemd"}))
        # network state that must survive: certificate, key, pin, join token, identity, database with a registered PC
        for name, data in (("server.crt", "CERT"), ("server.key", "KEY"), ("pin.json", '{"fp":"ab"}'), ("join-token", "TOKEN"),
                           ("local-installation.json", '{"id":"inst_1"}'), ("service-config.json", '{"port":8443}')):
            (self.sd / name).write_text(data)
        self.store = Store(self.sd / "coordinator.sqlite3")
        iid, token = self.store.join("Kamino3")
        self.bearer = f"{iid}.{token}"
        self.store.close()
        (self.sd / "coordinator.lock").write_text("")
        self.before = self.state()

    def state(self):
        return {p.name: p.read_bytes() for p in sorted(self.sd.iterdir()) if p.name not in (cl.INSTALL, "coordinator.lock")}

    def log(self):
        return self.calls.read_text().splitlines() if self.calls.exists() else []

    def still_registered(self):
        s = Store(self.sd / "coordinator.sqlite3")
        try:
            return s.authenticate(self.bearer)["name"] == "Kamino3"
        finally:
            s.close()

    def install_tree(self, endpoint):
        """The new version folder with the REAL packaging helpers and an interpreter reached through the descriptor."""
        (self.vdir / "app/packaging").mkdir(parents=True)
        for f in ("firewall.py", "lifecycle.py", "legacy_network_cleanup.py"):
            (self.vdir / "app/packaging" / f).write_text((REPO / "packaging" / f).read_text())
        py = self.vdir / "runtime/python/bin/python3"
        py.parent.mkdir(parents=True)
        py.write_text(f'#!/bin/sh\nexec {sys.executable} "$@"\n')
        py.chmod(0o755)
        (self.vdir / "keyloom-install.json").write_text(json.dumps({"roots": {"python": "runtime/python/bin/python3"}}))
        if endpoint:
            (self.vdir / "app-endpoint.json").write_text(json.dumps({"schema": 1, "app_endpoint": 1}))
        lifecycle.atomic_write_json(self.prefix / lifecycle.RECEIPT, {"schema": 1, "current": "v2", "versions": ["v2"]})


@pytest.fixture
def world(tmp_path, monkeypatch):
    return World(tmp_path, monkeypatch)


# ---- 1/2: the service goes, the network state stays, the PCs stay registered ----------------------------------------------

def test_upgrade_removes_the_service_and_keeps_every_state_file(world):
    out = cl.retire(world.prefix)
    assert out["action"] == "retired"
    assert not world.unit.exists() and not (world.prefix / cl.REGISTRY).exists() and not (world.sd / cl.INSTALL).exists()
    log = world.log()
    assert "systemctl --user disable keyloom-network.service" in log and "systemctl --user daemon-reload" in log
    assert world.state() == world.before  # certificate, key, pin, token, identity, config and the database: byte for byte
    assert not (world.sd / "host-removed.marker").exists()  # no tombstone: an older build sets its service up again
    assert world.still_registered()


def test_cleanup_is_repeatable(world):
    assert cl.retire(world.prefix)["action"] == "retired"
    n = len(world.log())
    again = cl.retire(world.prefix)
    assert again["action"] == "none" and len(world.log()) == n and world.state() == world.before


def test_a_failed_cleanup_is_finished_by_the_next_run(world, monkeypatch):
    real = cl._run
    monkeypatch.setattr(cl, "_run", lambda argv, timeout=30: type("R", (), {"returncode": 1, "stdout": b"", "stderr": b"boom"})()
                        if "disable" in argv else real(argv, timeout))
    with pytest.raises(cl.CleanupError):
        cl.retire(world.prefix)
    assert world.unit.exists() and (world.prefix / cl.REGISTRY).exists()  # nothing half-forgotten
    monkeypatch.setattr(cl, "_run", real)
    assert cl.retire(world.prefix)["action"] == "retired" and not world.unit.exists()


def test_a_running_service_is_asked_to_leave_first(world):
    lock = open(world.sd / "coordinator.lock", "a+")
    fcntl.flock(lock, fcntl.LOCK_EX)

    def service():  # the old service: leaves when it sees stop.request
        while not (world.sd / "stop.request").exists():
            time.sleep(0.02)
        fcntl.flock(lock, fcntl.LOCK_UN)
    t = threading.Thread(target=service)
    t.start()
    try:
        assert cl.retire(world.prefix)["action"] == "retired"
    finally:
        t.join()
        lock.close()
    assert not (world.sd / "stop.request").exists() and world.still_registered()


def test_a_service_that_will_not_stop_keeps_its_definition(world):
    lock = open(world.sd / "coordinator.lock", "a+")
    fcntl.flock(lock, fcntl.LOCK_EX)
    try:
        with pytest.raises(cl.CleanupError, match="did not stop"):
            cl.retire(world.prefix)
    finally:
        lock.close()
    assert world.unit.exists() and (world.prefix / cl.REGISTRY).exists() and "systemctl --user stop keyloom-network.service" in world.log()


def test_another_installs_service_is_left_alone(world):
    (world.sd / cl.INSTALL).write_text(json.dumps({"owner": "someoneelse", "mode": "systemd"}))
    assert cl.retire(world.prefix)["action"] == "foreign" and world.unit.exists() and world.log() == []
    (world.sd / cl.INSTALL).write_text(json.dumps({"owner": cl.owner_id(world.prefix), "mode": "systemd"}))
    world.unit.write_text("# X-Keyloom-State-Dir=/somewhere/else\n")  # a unit of the same name that belongs to another state folder
    assert cl.retire(world.prefix)["action"] == "foreign" and world.unit.exists()


def test_windows_task_is_deleted_and_state_kept(world, monkeypatch):
    (world.sd / cl.INSTALL).write_text(json.dumps({"owner": cl.owner_id(world.prefix), "mode": "task"}))
    bindir = world.tmp / "bin"
    desc = json.dumps({"owner": "x", "state_dir": str(world.sd)}).replace('"', "&quot;")
    q = bindir / "schtasks"
    q.write_text(f'#!/bin/sh\necho "schtasks $*" >> "{world.calls}"\n'
                 f'case "$1" in /Query) echo "<Task><Description>{desc}</Description></Task>";; esac\nexit 0\n')
    monkeypatch.setattr(cl, "IS_WIN", True)
    monkeypatch.setattr(cl, "lock_busy", lambda sd: False)
    deleted = []
    monkeypatch.setattr(cl, "_delete_run_value", lambda: deleted.append(1))
    assert cl.retire(world.prefix)["action"] == "retired"
    assert f"schtasks /Delete /TN {cl.TASK} /F" in world.log() and deleted
    assert world.state() == world.before and world.still_registered()


def test_lifecycle_runs_the_cleanup_in_the_installed_interpreter_and_records_it(world):
    world.install_tree(endpoint=True)
    net = lifecycle.network_step(world.vdir, world.prefix, lifecycle.read_receipt(world.prefix))
    assert net["action"] == "retired" and net["ok"] is True
    saved = json.loads((world.prefix / lifecycle.RECEIPT).read_text())
    assert saved["network"]["services"] is False and saved["network"]["setup"]["action"] == "retired"
    assert not world.unit.exists() and world.state() == world.before and world.still_registered()


# ---- 5: the gate -------------------------------------------------------------------------------------------------------

def test_without_the_in_app_endpoint_the_service_stays(world, monkeypatch):
    world.install_tree(endpoint=False)
    assert not lifecycle.app_endpoint_ready(world.vdir)
    seen = []
    monkeypatch.setattr(lifecycle, "refresh_network_service", lambda *a: seen.append("refresh") or {"action": "refreshed"})
    monkeypatch.setattr(lifecycle, "retire_legacy_service", lambda *a: seen.append("retire") or {"action": "retired"})
    assert lifecycle.network_step(world.vdir, world.prefix, {})["action"] == "refreshed"
    assert seen == ["refresh"] and world.unit.exists() and world.log() == []
    (world.vdir / "app-endpoint.json").write_text(json.dumps({"schema": 1, "app_endpoint": 1}))
    assert lifecycle.network_step(world.vdir, world.prefix, {})["action"] == "retired" and seen == ["refresh", "retire"]


@pytest.mark.parametrize("content", ["", "not json", '{"schema":1,"app_endpoint":0}', '{"schema":2,"app_endpoint":1}', "[]"])
def test_a_damaged_or_unfinished_marker_does_not_count(world, content):
    world.install_tree(endpoint=False)
    (world.vdir / "app-endpoint.json").write_text(content)
    assert not lifecycle.app_endpoint_ready(world.vdir)


def test_activation_uses_the_gate_and_cannot_fail_on_it():
    src = Path(lifecycle.__file__).read_text()
    body = src[src.index("def activate"):src.index("# ---- models")]
    assert "network_step(" in body and "refresh_network_service(" not in body


def test_the_marker_is_written_only_for_a_binary_that_contains_the_endpoint(tmp_path):
    stage = tmp_path / "stage"
    (stage / "bin").mkdir(parents=True)
    exe = stage / "bin" / ("keyloom-ui.exe" if bundle.IS_WIN else "keyloom-ui")
    exe.write_bytes(b"\x7fELF" + b"x" * 100)
    assert not bundle.write_endpoint_marker(stage) and not (stage / bundle.ENDPOINT_FILE).exists()
    exe.write_bytes(b"\x7fELF" + b"x" * (9 << 20) + b"KEYLOOM_APP_ENDPOINT_V1" + b"y" * 50)
    assert not bundle.write_endpoint_marker(stage) and not (stage / bundle.ENDPOINT_FILE).exists()
    exe.write_bytes(b"\x7fELF" + b"x" * (9 << 20) + bundle.ENDPOINT_MARKER + b"y" * 50)  # found past a read-chunk boundary
    assert bundle.write_endpoint_marker(stage) and lifecycle.app_endpoint_ready(stage)


def test_inno_templates_escape_apostrophes_inside_pascal_strings():
    for name in ("keyloom.iss.in", "keyloom-online.iss.in"):
        source = (REPO / "packaging/windows" / name).read_text()
        assert "PC's network endpoint" not in source, name
        assert "PC''s network endpoint" in source, name


# ---- 3/4: firewall -----------------------------------------------------------------------------------------------------

APP = r"C:\Users\u\Programs\Keyloom\versions\2.5.0-abc\bin\keyloom-ui.exe"
OLD = r"C:\Users\u\Programs\Keyloom\versions\2.4.0-abc\runtime\python\pythonw.exe"


class WinHost:
    def __init__(self, monkeypatch, allowed=False):
        self.elevated, self.allowed = [], allowed
        monkeypatch.setattr(fw, "win_allowed", lambda p: self.allowed)
        monkeypatch.setattr(fw, "win_ask", lambda q: True)
        monkeypatch.setattr(fw, "win_elevated", self.elevate)
        monkeypatch.setattr(sys.stdin.__class__, "isatty", lambda self: True, raising=False)

    def elevate(self, script):
        self.elevated.append(script)
        self.allowed = True
        return True


def old_rules():
    return fw.win_rule_specs(OLD)


def test_windows_rules_move_from_the_service_python_to_the_app(monkeypatch):
    host = WinHost(monkeypatch)
    rec = fw.run_windows("ask", {"owned": old_rules()}, APP)
    assert rec["status"] == "ok" and len(host.elevated) == 1  # ONE administrator step
    script = host.elevated[0]
    assert script.count("Remove-NetFirewallRule") == 2 and OLD in script  # the old service's rules go ...
    assert script.count("New-NetFirewallRule") == 2 and APP in script and "8443-8452" in script and "44554" in script
    assert "-RemoteAddress LocalSubnet,100.64.0.0/10,fd7a:115c:a1e0::/48,192.168.195.0/24" in script  # ... the networks stay
    assert all(r["program"] == APP for r in rec["owned"]) and len(rec["owned"]) == 2 and rec["program"] == APP
    assert [r["name"] for r in rec["owned"]] == ["Keyloom app endpoint (2.5.0-abc)", "Keyloom app discovery (2.5.0-abc)"]
    again = fw.run_windows("ask", rec, APP)  # same app path later: allowed -> no prompt, nothing duplicated
    assert again["owned"] == rec["owned"] and len(host.elevated) == 1


def test_windows_without_the_app_marker_keeps_the_service_rules(monkeypatch):
    monkeypatch.setattr(fw, "win_program", lambda: OLD)
    host = WinHost(monkeypatch)
    rec = fw.run_windows("ask", None)  # no program passed: the old behaviour
    assert [r["program"] for r in rec["owned"]] == [r["program"] for r in old_rules()] and "Remove-NetFirewallRule" not in host.elevated[0]


def test_windows_skip_gives_the_exact_swap_command(monkeypatch):
    host = WinHost(monkeypatch)
    rec = fw.run_windows("skip", {"owned": old_rules()}, APP)
    assert rec["status"] == "pending" and host.elevated == [] and APP in rec["commands"][0] and OLD in rec["commands"][0]
    assert len(rec["owned"]) == 2 and rec["owned"][0]["program"] == OLD  # nothing was claimed or dropped


def test_windows_uninstall_removes_the_apps_rules(monkeypatch):
    host = WinHost(monkeypatch)
    rec = fw.run_windows("ask", {"owned": old_rules()}, APP)
    host.elevated.clear()
    assert "removed" in fw.remove_windows(rec, "ask")
    assert host.elevated[0].count("Remove-NetFirewallRule") == 2 and APP in host.elevated[0] and OLD not in host.elevated[0]


def test_lifecycle_hands_the_app_exe_to_the_firewall_step_only_with_the_marker(world, monkeypatch):
    world.install_tree(endpoint=True)
    seen = []
    monkeypatch.setattr(lifecycle, "_firewall_run", lambda vdir, mode, prior, *program: seen.append(program[0] if program else None) or
                        {"status": "ok", "detail": "d", "commands": [], "owned": []})
    monkeypatch.setattr(lifecycle, "IS_WIN", False)
    lifecycle.firewall_step(world.prefix, "ask")
    monkeypatch.setattr(lifecycle, "IS_WIN", True)
    receipt = lifecycle.read_receipt(world.prefix)
    receipt["network"] = {"setup": {"action": "retired"}}
    lifecycle.atomic_write_json(world.prefix / lifecycle.RECEIPT, receipt)
    lifecycle.firewall_step(world.prefix, "ask")
    receipt["network"]["setup"]["action"] = "failed"
    lifecycle.atomic_write_json(world.prefix / lifecycle.RECEIPT, receipt)
    lifecycle.firewall_step(world.prefix, "ask")
    assert seen[0] is None and seen[1] == str(lifecycle.ui_exe(world.vdir)) and seen[2] is None


def test_firewall_helper_main_accepts_the_optional_app_exe(monkeypatch, tmp_path):
    got = []
    monkeypatch.setattr(fw, "run", lambda mode, prior, program=None: got.append((mode, program)) or {"owned": []})
    assert fw.main(["ask", "{}", str(tmp_path / "o.json"), APP]) == 0 and fw.main(["ask", "{}", str(tmp_path / "o.json")]) == 0
    assert got == [("ask", APP), ("ask", None)] and fw.main(["ask", "{}", "o", "a", "b"]) == 64


def test_linux_ufw_rules_are_by_port_and_network_and_uninstall_removes_them(monkeypatch):
    """UFW names no program, so nothing is swapped: the networks and ports stay; new rules carry the app comment."""
    add = fw.allow_args("10.0.0.0/24", "tcp")
    assert "8443:8452" in add and add[-1] == "Keyloom installer: app endpoint"
    calls = []

    def sudo(ufw, args, capture=False):
        calls.append(args)
        return 0, ("8443:8452/tcp  ALLOW  10.0.0.0/24  # Keyloom installer: app endpoint\n"
                   "44554/udp  ALLOW  10.0.0.0/24  # Keyloom installer: discovery\n") if args == ["status"] else ""
    monkeypatch.setattr(fw, "ufw_path", lambda: "/usr/sbin/ufw")
    monkeypatch.setattr(fw, "interactive", lambda: True)
    monkeypatch.setattr(fw, "ask", lambda q: True)
    monkeypatch.setattr(fw, "sudo_ufw", sudo)
    monkeypatch.setattr(sys, "platform", "linux")
    assert fw.remove({"owned": ["10.0.0.0/24"]}, "ask") == "Keyloom firewall rules removed from UFW."
    assert len([c for c in calls if c[:2] == ["--force", "delete"]]) == 2


def test_uninstall_retires_the_service_through_the_same_cleanup(world):
    world.install_tree(endpoint=True)
    lifecycle.remove_network_service(world.prefix, "v2")
    assert not world.unit.exists() and world.state() == world.before
    lifecycle.remove_network_service(world.prefix, "v2")  # nothing registered any more: a no-op, not an error


# ---- the embedded runtime library ships in both installers ---------------------------------------------------------------

def fake_stage(tmp_path, lib=True):
    stage = tmp_path / "stage"
    if bundle.IS_WIN:
        (stage / "runtime/python").mkdir(parents=True)
        if lib:
            (stage / "runtime/python/python311.dll").write_bytes(b"MZ")
    else:
        (stage / "runtime/python/lib").mkdir(parents=True)
        if lib:
            (stage / "runtime/python/lib/libpython3.11.so.1.0").write_bytes(b"\x7fELF")
    return stage


def test_bundle_requires_the_runtime_library(tmp_path):
    assert bundle.embed_lib("3.11.16") == ("runtime/python/python311.dll" if bundle.IS_WIN else "runtime/python/lib/libpython3.11.so.1.0")
    assert bundle.check_embed_runtime(fake_stage(tmp_path), "3.11.16") == bundle.embed_lib("3.11.16")
    with pytest.raises(SystemExit, match="cannot load Python in-process"):
        bundle.check_embed_runtime(fake_stage(tmp_path / "no", lib=False), "3.11.16")
    src = Path(bundle.__file__).read_text()
    assert "check_embed_runtime(stage, py_version)" in src[src.index("def build_stage"):]


def manifest(*rels, endpoint=True):
    files = {r: {"size": 1, "sha256": "a" * 64} for r in rels}
    if endpoint:
        files["app-endpoint.json"] = {"size": 1, "sha256": "a" * 64}
    return {"schema": 1, "platform": online.PLATFORM, "version_id": "2.5.0-abc", "files": files}


def test_both_installers_carry_the_library_through_the_manifest(tmp_path):
    """Offline installers pack the whole stage; the online installer rebuilds its tree from the manifest and refuses one without it."""
    lib = bundle.embed_lib("3.11.16")
    online.validate_manifest(manifest("bin/keyloom-ui", lib))
    with pytest.raises(online.Fail, match="embedded Python runtime library"):
        online.validate_manifest(manifest("bin/keyloom-ui"))
    online.validate_manifest(manifest("bin/keyloom-ui", endpoint=False))  # an old-style build has no such duty
    stage = fake_stage(tmp_path)
    (stage / "bin").mkdir()
    (stage / "bin/keyloom-ui").write_bytes(b"x")
    bundle.write_manifest(stage, "2.5.0", "lock", "cat")
    files = json.loads((stage / "install-manifest.json").read_text())["files"]
    assert lib in files and bundle.runtime_id(files) == online.runtime_id(files)


def test_the_ui_binary_may_not_link_a_foreign_libpython():
    import struct
    if bundle.IS_WIN:
        pytest.skip("ELF check")
    needed, run = bundle.elf_dynamic(Path(sys.executable).resolve())
    assert isinstance(needed, list) and isinstance(run, list)  # parses a real ELF
    assert bundle.elf_dynamic(Path(__file__)) == ([], [])  # anything else is ignored
    assert struct  # noqa: F401  (parser relies on struct layouts)


def test_old_glibc_build_script_checks_the_libpython_link():
    sh = (REPO / "packaging/linux/build-ui-old-glibc.sh").read_text()
    assert "bundle.check_ui_libpython" in sh and "$ORIGIN/../runtime/python/lib" in sh
