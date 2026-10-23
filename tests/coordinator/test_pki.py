import json
import os
import stat
import subprocess
import sys

import pytest

from backend.coordinator import pki
from backend.coordinator.__main__ import main


def test_private_files_mode_never_overwritten_and_perm_check(tmp_path):
    p = str(tmp_path / "c.json")
    pki.write_credentials(p, "https://127.0.0.1:1", "PEM", "ff", "inst_1", "TOKEN_0123456789abcdef")
    if os.name != "nt":
        assert stat.S_IMODE(os.stat(p).st_mode) == 0o600
    with pytest.raises(FileExistsError):
        pki.write_credentials(p, "u", "PEM", "ff", "inst_2", "OTHER_0123456789abcdef")
    assert pki.read_credentials(p)["token"] == "TOKEN_0123456789abcdef"
    if os.name != "nt":
        os.chmod(p, 0o644)
        with pytest.raises(pki.SecurityError):
            pki.read_credentials(p)
        os.chmod(p, 0o600)
    open(p, "w").write("{broken")
    with pytest.raises(pki.SecurityError):
        pki.read_credentials(p)
    assert open(p).read() == "{broken"  # critical files are never reset


def test_failed_protection_removes_just_created_file(tmp_path, monkeypatch):
    p = str(tmp_path / "x")

    def boom(path):
        raise pki.SecurityError("acl")
    monkeypatch.setattr(pki, "protect_private", boom)
    with pytest.raises(pki.SecurityError):
        pki.write_private_new(p, "secret")
    assert not os.path.exists(p)


def test_windows_acl_verification(monkeypatch):
    def fake(owner_only):
        def run(cmd, **kw):
            out = {"whoami": "PC\\me\n"}.get(cmd[0], "")
            if cmd[0] == "icacls" and len(cmd) == 2:
                out = "f PC\\me:(F)\n" + ("" if owner_only else "  BUILTIN\\Users:(R)\n")
            return subprocess.CompletedProcess(cmd, 0, out, "")
        return run
    monkeypatch.setattr(pki, "_IS_WINDOWS", True)
    monkeypatch.setattr(pki, "_windows_run", fake(True))
    pki.protect_private("f")
    monkeypatch.setattr(pki, "_windows_run", fake(False))
    with pytest.raises(pki.SecurityError):
        pki.protect_private("f")


def test_cert_san_and_hostname(tmp_path):
    pem, _ = pki.generate_identity(["127.0.0.1", "nas.lan"])
    assert pki.cert_covers_host(pem, "127.0.0.1") and pki.cert_covers_host(pem, "NAS.lan")
    assert not pki.cert_covers_host(pem, "evil.lan")
    with pytest.raises(pki.SecurityError):
        pki.write_invitation(str(tmp_path / "i"), "https://evil.lan:1", pem, "C", 1)


def test_cli_pair_flow_and_safe_failures(tmp_path, store, tls_server, capsys):
    _, url, pem = tls_server
    inv = str(tmp_path / "inv.json")
    pki.write_invitation(inv, url, pem, store.create_pairing(), 4_000_000_000)
    creds = str(tmp_path / "creds.json")
    assert main(["pair", "--invitation", inv, "--credentials", creds, "--name", "n"]) == 0
    token = pki.read_credentials(creds)["token"]
    out = capsys.readouterr()
    assert token not in out.out + out.err
    assert main(["pair", "--invitation", inv, "--credentials", creds, "--name", "n"]) == 1  # code spent / creds exist
    assert pki.read_credentials(creds)["token"] == token
    expired = str(tmp_path / "old.json")
    pki.write_invitation(expired, url, pem, "X", 1)
    assert main(["pair", "--invitation", expired, "--credentials", str(tmp_path / "n.json"), "--name", "n"]) == 1


def test_one_owner_per_state_dir(tmp_path):
    a = pki.OwnerLock(str(tmp_path / "l"))
    with pytest.raises(pki.SecurityError):
        pki.OwnerLock(str(tmp_path / "l"))
    del a


def test_serve_cli_end_to_end_loopback(tmp_path):
    """Real `serve` process on an ephemeral loopback port: invitation file -> pair -> authenticated call."""
    from backend.coordinator.client import Client
    repo = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    inv = tmp_path / "inv.json"
    proc = subprocess.Popen(
        [sys.executable, "-m", "backend.coordinator", "serve", "--state-dir", str(tmp_path / "st"), "--port", "0",
         "--invite-out", str(inv), "--invite-admin"],
        cwd=repo, env={"PYTHONPATH": repo, "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ["PATH"]},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        line = ""
        while "listening" not in line:
            line = proc.stdout.readline()
            assert line, proc.stderr.read()
        creds = str(tmp_path / "c.json")
        assert main(["pair", "--invitation", str(inv), "--credentials", creds, "--name", "cli"]) == 0
        c = pki.read_credentials(creds)
        assert Client(c).installations()["installations"][0]["name"] == "cli"
        again = subprocess.run([sys.executable, "-m", "backend.coordinator", "serve", "--state-dir", str(tmp_path / "st"), "--port", "0"],
                               cwd=repo, env={"PYTHONPATH": repo, "PYTHONDONTWRITEBYTECODE": "1", "PATH": os.environ["PATH"]},
                               capture_output=True, text=True, timeout=30)
        assert again.returncode == 1 and "already owns" in again.stderr  # one owner per state dir
        assert c["token"] not in line and c["token"] not in again.stdout + again.stderr
    finally:
        proc.terminate()
        proc.wait(10)
