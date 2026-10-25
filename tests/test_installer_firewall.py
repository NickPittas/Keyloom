"""Linux installer UFW step: decisions only. Nothing here touches a real firewall, sudo or the network."""
import json
import subprocess
import sys
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / "packaging"))
import firewall as fw  # noqa: E402
import lifecycle  # noqa: E402
import online  # noqa: E402


def link(name, *addrs):
    return {"ifname": name, "addr_info": [{"local": a.split("/")[0], "prefixlen": int(a.split("/")[1])} for a in addrs]}


STATUS_USER_RULES = """Status: active

To                         Action      From
--                         ------      ----
8443:8452/tcp              ALLOW       192.168.100.0/24           # Keyloom service (LAN)
44554/udp                  ALLOW       192.168.100.0/24           # Keyloom discovery (LAN)
22/tcp                     ALLOW       Anywhere
"""


def test_networks_derived_and_filtered():
    links = [
        link("lo", "127.0.0.1/8"), link("docker0", "172.17.0.1/16"), link("br-abc", "172.18.0.1/16"),
        link("veth1", "10.9.0.1/24"), link("virbr0", "192.168.122.1/24"), link("vboxnet0", "192.168.56.1/24"),
        link("vEthernet (WSL)", "172.30.0.1/20"),
        link("eth0", "192.168.100.7/24", "fe80::1/64", "93.184.216.34/24", "8.8.8.8/24", "fd12:3456:789a::7/64"),
        link("zt123", "10.147.17.4/24"), link("tailscale0", "100.101.102.103/32", "fd7a:115c:a1e0::1/128", "10.0.0.1/24"),
        link("wide", "10.1.1.1/4"),  # a /4 is not a trusted LAN
    ]
    assert fw.trusted_networks(links) == ["10.147.17.0/24", "100.64.0.0/10", "192.168.100.0/24",
                                          "fd12:3456:789a::/64", "fd7a:115c:a1e0::/48"]
    assert fw.trusted_networks("junk") == [] and fw.trusted_networks([{"ifname": "eth0", "addr_info": [{"local": "x"}]}]) == []


def test_cidr_and_args_validated():
    assert not fw.valid_cidr("192.168.1.5/24") and not fw.valid_cidr("1.2.3.0/24; rm") and fw.valid_cidr("10.0.0.0/8")
    with pytest.raises(ValueError):
        fw.allow_args("10.0.0.0/8; reboot", "tcp")
    assert fw.allow_args("10.0.0.0/24", "udp")[:9] == ["allow", "from", "10.0.0.0/24", "to", "any", "port", "44554", "proto", "udp"]
    assert fw.delete_args("10.0.0.0/24", "tcp")[:2] == ["--force", "delete"] and "comment" not in fw.delete_args("10.0.0.0/24", "tcp")


def test_user_added_rules_count_as_coverage():
    rules = fw.parse_status(STATUS_USER_RULES)
    assert fw.missing(rules, ["192.168.100.0/24"]) == []
    assert fw.missing(rules, ["192.168.100.0/25"]) == []  # a narrower net is inside it
    assert fw.missing(rules, ["10.0.0.0/24"]) == [("10.0.0.0/24", "tcp"), ("10.0.0.0/24", "udp")]
    assert fw.missing(fw.parse_status("8443/tcp  ALLOW  192.168.100.0/24"), ["192.168.100.0/24"]) == \
        [("192.168.100.0/24", "tcp"), ("192.168.100.0/24", "udp")]  # one port is not the range
    assert fw.missing(fw.parse_status("Anywhere  DENY  192.168.100.0/24"), ["192.168.100.0/24"])  # deny is not coverage
    anywhere = "8443:8452/tcp  ALLOW  Anywhere\n44554/udp  ALLOW  Anywhere\n"
    assert fw.missing(fw.parse_status(anywhere), ["192.168.100.0/24"]) == []


class Env:
    """Fake host: records every elevated ufw call."""

    def __init__(self, monkeypatch, status=STATUS_USER_RULES, links=None, answer=True, tty=True, rc=0, ufw="/usr/sbin/ufw",
                 enabled=True):
        self.calls, self.asked = [], []
        self.status, self.rc = status, rc
        monkeypatch.setattr(fw, "ufw_path", lambda: ufw)
        monkeypatch.setattr(fw, "ufw_enabled", lambda: enabled)
        monkeypatch.setattr(fw, "firewalld_active", lambda: False)
        monkeypatch.setattr(fw, "interface_links", lambda: links if links is not None else [link("eth0", "10.0.0.5/24")])
        monkeypatch.setattr(fw, "interactive", lambda: tty)
        monkeypatch.setattr(fw, "ask", lambda q: self.asked.append(q) or answer)
        monkeypatch.setattr(fw, "sudo_ufw", self.sudo)

    def sudo(self, ufw, args, capture=False):
        self.calls.append(args)
        if args == ["status"]:
            return self.rc, self.status
        return self.rc, ""

    @property
    def adds(self):
        return [c for c in self.calls if c[0] == "allow"]


def test_absent_or_disabled_ufw_needs_no_sudo(monkeypatch):
    env = Env(monkeypatch, ufw=None)
    assert fw.run("ask", None)["status"] == "not_needed"
    env = Env(monkeypatch, enabled=False)
    assert fw.run("ask", None)["status"] == "not_needed" and env.calls == [] and env.asked == []


def test_firewalld_is_reported_unsupported(monkeypatch):
    Env(monkeypatch, ufw=None)
    monkeypatch.setattr(fw, "firewalld_active", lambda: True)
    rec = fw.run("ask", None)
    assert rec["status"] == "unsupported" and "firewalld" in rec["detail"]


def test_skip_and_no_terminal_stay_pending_with_commands(monkeypatch):
    for kw, mode in (({}, "skip"), ({"tty": False}, "ask")):
        env = Env(monkeypatch, **kw)
        rec = fw.run(mode, None)
        assert rec["status"] == "pending" and env.calls == []
        assert rec["commands"][0].startswith("sudo ufw allow from 10.0.0.0/24 to any port 8443:8452 proto tcp comment ")
        assert len(rec["commands"]) == 2
    assert env.asked == []  # no terminal: never prompted (cannot hang)


def test_no_trusted_network_is_not_claimed_ready(monkeypatch):
    Env(monkeypatch, links=[link("lo", "127.0.0.1/8")])
    assert fw.run("ask", None)["status"] == "pending"


def test_accept_adds_only_missing_rules_and_records_ownership(monkeypatch):
    env = Env(monkeypatch, links=[link("eth0", "192.168.100.7/24"), link("zt1", "10.147.0.2/24")])
    rec = fw.run("ask", None)
    assert rec["status"] == "ok" and rec["owned"] == ["10.147.0.0/24"]
    assert [c[2] for c in env.adds] == ["10.147.0.0/24"] * 2 and {c[-3] for c in env.adds} == {"tcp", "udp"}
    assert all(c[-1].startswith("Keyloom installer") for c in env.adds)  # ownership comment
    assert env.calls[0] == ["status"] and env.asked == []  # the firewall choice is made up front: no question


def test_fully_covered_by_user_rules_adds_nothing_and_owns_nothing(monkeypatch):
    env = Env(monkeypatch, links=[link("eth0", "192.168.100.7/24")])
    rec = fw.run("ask", None)
    assert rec["status"] == "ok" and rec["owned"] == [] and env.adds == []


def test_settled_networks_do_not_ask_or_sudo_again_but_new_ones_do(monkeypatch):
    env = Env(monkeypatch)
    first = fw.run("ask", None)
    env.calls.clear(), env.asked.clear()
    again = fw.run("ask", first)
    assert again["status"] == "ok" and again["owned"] == first["owned"] and env.calls == [] and env.asked == []
    env2 = Env(monkeypatch, links=[link("eth0", "10.0.0.5/24"), link("wlan0", "172.20.1.5/24")])
    third = fw.run("ask", first)
    assert env2.asked == []  # the install path no longer prompts
    assert third["owned"] == ["10.0.0.0/24", "172.20.1.0/24"]
    env3 = Env(monkeypatch, status=STATUS_USER_RULES + "8443:8452/tcp  ALLOW  10.0.0.0/24\n44554/udp  ALLOW  10.0.0.0/24\n")
    assert fw.run("ask", {"status": "pending", "owned": ["10.0.0.0/24"], "networks": []})["owned"] == ["10.0.0.0/24"]
    assert env3.adds == []  # reinstall after a covered state adds no duplicate


@pytest.mark.parametrize("mode", ["skip", "ask"])
def test_reinstall_over_settled_rules_never_claims_new_rules_or_uses_sudo(monkeypatch, mode):
    env = Env(monkeypatch)
    first = fw.run("ask", None)
    assert "added" in first["detail"]
    env.calls.clear(), env.asked.clear()
    again = fw.run(mode, first)
    assert env.calls == [] and env.asked == []  # --firewall skip (and a settled ask) never reach sudo
    assert again["owned"] == first["owned"] and again["status"] == "ok"
    assert "added" not in again["detail"].replace("nothing was added", "") and "nothing was added or changed" in again["detail"]


@pytest.mark.parametrize("rc", [1, None])
def test_sudo_failure_or_timeout_is_pending_without_writes(monkeypatch, rc):
    env = Env(monkeypatch, rc=rc)
    rec = fw.run("ask", None)
    assert rec["status"] == "pending" and env.adds == [] and rec["commands"] and rec["owned"] == []


def test_partial_failure_keeps_truth(monkeypatch):
    env = Env(monkeypatch)

    def sudo(ufw, args, capture=False):
        env.calls.append(args)
        if args == ["status"]:
            return 0, env.status
        return (0, "") if args[-3] == "tcp" else (1, "")

    monkeypatch.setattr(fw, "sudo_ufw", sudo)
    rec = fw.run("ask", None)
    assert rec["status"] == "pending" and len(rec["commands"]) == 1 and "proto udp" in rec["commands"][0]


class FakePopen:
    seen = {}

    def __init__(self, cmd, **kw):
        FakePopen.seen = {"cmd": cmd, **kw}
        self.returncode = 0

    def communicate(self, timeout):
        FakePopen.seen["timeout"] = timeout
        return "out", None


def test_sudo_runs_only_ufw_argv_on_the_terminal(monkeypatch):
    monkeypatch.setattr(fw.subprocess, "Popen", FakePopen)
    monkeypatch.setattr(fw.os, "geteuid", lambda: 1000)
    assert fw.sudo_ufw("/usr/sbin/ufw", ["status"], capture=True) == (0, "out")
    s = FakePopen.seen
    assert s["cmd"][0] == "sudo" and s["cmd"][-3:] == ["--", "/usr/sbin/ufw", "status"] and s["timeout"] > 0
    assert "stdin" not in s and "input" not in s and "shell" not in s and "env" not in s  # password stays on the tty


def test_remove_touches_only_owned_rules(monkeypatch):
    rec = {"owned": ["10.0.0.0/24"]}
    status = (STATUS_USER_RULES + "8443:8452/tcp  ALLOW  10.0.0.0/24  # Keyloom installer: service\n"
              "44554/udp  ALLOW  10.0.0.0/24  # someone else's\n")
    env = Env(monkeypatch, status=status)
    assert fw.remove(rec, "ask") == "Keyloom firewall rules removed from UFW."
    deletes = [c for c in env.calls if c[:2] == ["--force", "delete"]]
    assert len(deletes) == 1 and deletes[0][4] == "10.0.0.0/24" and "tcp" in deletes[0]  # not the foreign udp, never 192.168.100.0/24
    assert fw.remove({"owned": []}, "ask") == "" and fw.remove(None, "ask") == ""


@pytest.mark.parametrize("kw,mode", [({"answer": False}, "ask"), ({"tty": False}, "ask"), ({}, "skip"), ({"rc": 1}, "ask")])
def test_remove_declined_or_failed_reports_owned_rules_left(monkeypatch, kw, mode):
    env = Env(monkeypatch, **kw)
    msg = fw.remove({"owned": ["10.0.0.0/24"]}, mode)
    assert "left in UFW" in msg and "sudo ufw --force delete allow from 10.0.0.0/24" in msg
    assert not [c for c in env.calls if c[:2] == ["--force", "delete"]]


def installed_tree(tmp_path, rec_json=None):
    """prefix/versions/v1 with the REAL packaging files and an interpreter reached only through the descriptor path."""
    vdir = tmp_path / "versions" / "v1"
    (vdir / "app/packaging").mkdir(parents=True)
    for f in ("firewall.py", "lifecycle.py"):
        (vdir / "app/packaging" / f).write_text((REPO / "packaging" / f).read_text())
    py = vdir / "runtime/python/bin/python3"
    py.parent.mkdir(parents=True)
    py.write_text(f'#!/bin/sh\nexec {sys.executable} "$@"\n')
    py.chmod(0o755)
    (vdir / "keyloom-install.json").write_text(json.dumps({"roots": {"python": "runtime/python/bin/python3"}}))
    lifecycle.atomic_write_json(tmp_path / lifecycle.RECEIPT, {"schema": 1, "current": "v1", "versions": ["v1"],
                                                               "network": {"services": True}})
    return vdir


def test_real_package_load_path_runs_in_the_installed_interpreter(monkeypatch, tmp_path, capsys):
    """Regression for the 2.4.0-99b6a39c52 failure: the staged payload (and its Python) is renamed before the step, so the
    step must run in a fresh `python -I -B` of the INSTALLED tree. --firewall skip: no prompt, no sudo, no rule change."""
    vdir = installed_tree(tmp_path)
    monkeypatch.setattr(lifecycle, "IS_WIN", False)
    status = lifecycle.firewall_step(tmp_path, "skip")
    saved = json.loads((tmp_path / lifecycle.RECEIPT).read_text())
    out = capsys.readouterr().out
    assert "could not run" not in out and "ModuleNotFoundError" not in out
    assert status == saved["firewall"]["status"] and status in ("pending", "not_needed", "unsupported")
    assert saved["firewall"]["owned"] == [] and saved["network"]["firewall_rules"] is False
    assert not list(tmp_path.glob(".firewall.*"))  # private result dir removed


def test_helper_failure_names_the_error_and_is_pending(monkeypatch, tmp_path, capsys):
    vdir = installed_tree(tmp_path)
    (vdir / "app/packaging/firewall.py").write_text("import not_a_real_module_xyz\n")
    monkeypatch.setattr(lifecycle, "IS_WIN", False)
    assert lifecycle.firewall_step(tmp_path, "skip") == "pending"
    assert "NETWORK FIREWALL SETUP PENDING: the helper could not run (InstallError: firewall helper exited" in capsys.readouterr().out


def test_lifecycle_records_pending_truth_in_receipt(monkeypatch, tmp_path, capsys):
    Env(monkeypatch)
    monkeypatch.setattr(lifecycle, "_firewall_run", lambda v, m, p: fw.run(m, p))
    installed_tree(tmp_path)
    monkeypatch.setattr(lifecycle, "IS_WIN", False)
    assert lifecycle.firewall_step(tmp_path, "skip") == "pending"
    saved = json.loads((tmp_path / lifecycle.RECEIPT).read_text())
    assert saved["firewall"]["status"] == "pending" and saved["firewall"]["commands"] and saved["network"]["firewall_rules"] is False
    out = capsys.readouterr().out
    assert "NETWORK FIREWALL SETUP PENDING" in out and "sudo ufw allow from 10.0.0.0/24" in out


def test_lifecycle_step_never_raises(monkeypatch, tmp_path, capsys):
    installed_tree(tmp_path)
    monkeypatch.setattr(lifecycle, "IS_WIN", False)
    monkeypatch.setattr(lifecycle, "_firewall_run", lambda *a: 1 / 0)
    assert lifecycle.firewall_step(tmp_path, "ask") == "pending"
    assert "ZeroDivisionError" in capsys.readouterr().out


def test_load_gate_survives_nothing_but_the_helper_api(tmp_path):
    """Build gate contract: a fresh `python -I -B` loads helper via lifecycle._firewall() with no firewall.run call."""
    vdir = installed_tree(tmp_path)
    code = ("import importlib.util,sys;s=importlib.util.spec_from_file_location('l',sys.argv[1]);m=importlib.util.module_from_spec(s);"
            "s.loader.exec_module(m);fw=m._firewall();assert callable(fw.run) and callable(fw.remove) and callable(fw.main)")
    r = subprocess.run([str(vdir / "runtime/python/bin/python3"), "-I", "-B", "-c", code, str(vdir / "app/packaging/lifecycle.py")],
                       capture_output=True, text=True, timeout=30)
    assert r.returncode == 0, r.stderr


class WinEnv:
    """Fake Windows host for firewall.run_windows: no PowerShell, no UAC; records every elevated script."""

    def __init__(self, monkeypatch, allowed=False, tty=True, answer=True, elevated_ok=True):
        self.elevated, self.allowed = [], allowed
        prog = r"C:\Users\u\Programs\Keyloom\versions\2.4.0-abc\runtime\python\pythonw.exe"
        monkeypatch.setattr(fw, "win_program", lambda: prog)
        monkeypatch.setattr(fw, "win_allowed", lambda p: self.allowed)
        monkeypatch.setattr(fw, "win_ask", lambda q: answer)

        def elev(script):
            self.elevated.append(script)
            if elevated_ok:
                self.allowed = True
            return elevated_ok
        monkeypatch.setattr(fw, "win_elevated", elev)
        monkeypatch.setattr(sys.stdin.__class__, "isatty", lambda self: tty, raising=False)
        self.prog = prog


def test_windows_already_allowed_changes_nothing(monkeypatch):
    env = WinEnv(monkeypatch, allowed=True)
    rec = fw.run_windows("ask", None)
    assert rec["status"] == "ok" and rec["owned"] == [] and env.elevated == [] and "nothing was added" in rec["detail"]


def test_windows_skip_adds_nothing_and_gives_the_exact_command(monkeypatch):
    for kw in ({"tty": True}, {"tty": False}):
        env = WinEnv(monkeypatch, **kw)
        rec = fw.run_windows("skip", None)  # the UAC step needs no terminal: only skip stops it
        assert rec["status"] == "pending" and env.elevated == [] and env.prog in rec["commands"][0] and rec["owned"] == []


def test_windows_adds_only_the_scoped_rules_once_and_records_them(monkeypatch):
    env = WinEnv(monkeypatch)
    rec = fw.run_windows("ask", None)
    assert rec["status"] == "ok" and len(rec["owned"]) == 2 and len(env.elevated) == 1
    script = env.elevated[0]
    assert script.count("New-NetFirewallRule") == 2 and env.prog in script and "8443-8452" in script and "44554" in script
    assert "-RemoteAddress LocalSubnet,100.64.0.0/10,fd7a:115c:a1e0::/48,192.168.195.0/24" in script
    assert "Any" not in script and "Disable" not in script and "Remove" not in script
    assert {r["protocol"] for r in rec["owned"]} == {"TCP", "UDP"} and all(r["program"] == env.prog for r in rec["owned"])
    again = fw.run_windows("ask", rec)  # same path later: allowed -> no prompt, nothing duplicated
    assert again["owned"] == rec["owned"] and len(env.elevated) == 1


def test_windows_failed_elevation_stays_pending_without_ownership(monkeypatch):
    env = WinEnv(monkeypatch, elevated_ok=False)
    rec = fw.run_windows("ask", None)
    assert rec["status"] == "pending" and rec["owned"] == [] and len(env.elevated) == 1


def test_windows_remove_touches_only_recorded_rules(monkeypatch):
    env = WinEnv(monkeypatch)
    rec = fw.run_windows("ask", None)
    env.elevated.clear()
    msg = fw.remove_windows(rec, "ask")
    assert "removed" in msg and len(env.elevated) == 1 and env.elevated[0].count("Remove-NetFirewallRule") == 2
    assert "Keyloom network" in env.elevated[0] and env.prog in env.elevated[0]
    assert fw.remove_windows({"owned": []}, "ask") == "" and len(env.elevated) == 1
    assert "left in Windows Firewall" in fw.remove_windows(rec, "skip")


def test_online_forwards_firewall_choice_to_the_same_lifecycle(monkeypatch, tmp_path):
    seen = []
    monkeypatch.setattr(online.subprocess, "Popen", lambda cmd, **kw: seen.append(cmd) or type("P", (), {"wait": lambda s: 0})())
    for verb in ("install", "repair"):
        online.run_lifecycle(tmp_path, tmp_path / "tree", verb, "skip")
    assert [c[c.index("--firewall") + 1] for c in seen] == ["skip", "skip"]
    assert seen[0][-2:] == ["--models", "skip"] and "--models" not in seen[1]
    assert online.build_parser().parse_args(["--payload-dir", "p", "--sources", "s"]).firewall == "yes"


def test_offline_and_online_stubs_pass_the_choice_and_stay_nonblocking():
    off = (REPO / "packaging/linux/installer-stub.sh").read_text()
    on = (REPO / "packaging/linux/online-stub.sh").read_text()
    assert off.count('--firewall "$FIREWALL"') == 3 and 'FIREWALL=yes' in off
    assert on.count('--firewall "$FIREWALL"') == 1 and 'FIREWALL=yes' in on
    assert '--models "$MODELS" --firewall' in off  # models handling untouched
    parser = lifecycle.build_parser()
    for verb in ("install", "repair", "uninstall"):
        assert parser.parse_args(["--prefix", "p", verb, "--firewall", "skip"]).firewall == "skip"


def test_build_and_selfcheck_never_reach_the_firewall():
    for f in ("packaging/bundle.py", "packaging/selfcheck.py"):
        p = REPO / f
        assert not p.exists() or "firewall" not in p.read_text()
    src = Path(lifecycle.__file__).read_text()
    body = src[src.index("def run_selfcheck"):src.index("def owner_id")]
    assert "firewall" not in body
    assert "activate(" in src and "firewall_step" not in src[src.index("def activate"):src.index("# ---- models")]


def test_windows_rule_names_carry_the_version_id():
    n = fw.win_rule_specs(r"C:\Users\u\Programs\Keyloom\versions\2.4.0-abc\runtime\python\pythonw.exe")
    assert [r["name"] for r in n] == ["Keyloom network service (2.4.0-abc)", "Keyloom network discovery (2.4.0-abc)"]
