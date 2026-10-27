import os
import stat
import subprocess

import pytest

from backend.coordinator import pki


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


def test_cert_identity_survives_address_change(tmp_path):
    pem, _ = pki.generate_identity(["127.0.0.1", "nas.lan"])
    assert pki.cert_covers_host(pem, "127.0.0.1") and pki.cert_covers_host(pem, "NAS.lan")
    assert not pki.cert_covers_host(pem, "evil.lan")
    _, _, ctx = pki.verified_trust("https://100.126.185.76:8443", pem, pki.fingerprint(pem))
    assert not ctx.check_hostname
    with pytest.raises(pki.SecurityError):
        pki.verified_trust("https://100.126.185.76:8443", pem, "0" * 64)




def test_one_owner_per_state_dir(tmp_path):
    a = pki.OwnerLock(str(tmp_path / "l"))
    with pytest.raises(pki.SecurityError):
        pki.OwnerLock(str(tmp_path / "l"))
    del a


