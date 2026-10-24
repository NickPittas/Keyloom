"""Shared install-descriptor contract tests — the mirror of rust-ui/src/runtime.rs's tests.

Both languages parse the SAME fixture (rust-ui/tests/fixtures/install/keyloom-install.json)
and assert the same resolution and error wording: a mismatch here is a contract break, not a
test nit. Broken descriptors raise instead of falling back anywhere.
"""
import os
import json
import shutil
from pathlib import Path

import pytest

from backend import install_paths as ip
from backend import v2_paths as vp

FIXTURE = (
    Path(__file__).resolve().parents[1]
    / "rust-ui"
    / "tests"
    / "fixtures"
    / "install"
    / ip.DESCRIPTOR_NAME
)


@pytest.fixture
def no_install_env(monkeypatch):
    monkeypatch.delenv(ip.CK_INSTALL_ENV, raising=False)
    monkeypatch.delenv(ip.MODELS_DIR_ENV, raising=False)


def _layout(root: Path, name: str = "install") -> Path:
    install = root / name
    install.mkdir(parents=True, exist_ok=True)
    shutil.copy(FIXTURE, install / ip.DESCRIPTOR_NAME)
    return install


@pytest.mark.parametrize("configured", [True, False])
def test_installed_inference_scratch_uses_writable_descriptor_or_user_area(tmp_path, monkeypatch, configured):
    from backend.service.inference import _scratch_root

    descriptor = tmp_path / ip.DESCRIPTOR_NAME
    scratch = tmp_path / "chosen-scratch"
    descriptor.write_text(json.dumps({"schema": 1, "roots": {
        "python": "python", "helper": "app", "models": "/read-only-models",
        "scratch": str(scratch) if configured else None,
    }}))
    monkeypatch.setenv("CK_INSTALL", str(descriptor))
    monkeypatch.delenv("CORRIDORKEY_V2_SCRATCH_DIR", raising=False)
    monkeypatch.setattr(ip, "platform_scratch", lambda: tmp_path / "user-scratch")
    expected = scratch if configured else tmp_path / "user-scratch"
    assert _scratch_root() == str(expected)
    assert expected.is_dir()
    override = tmp_path / "override"
    monkeypatch.setenv("CORRIDORKEY_V2_SCRATCH_DIR", str(override))
    assert _scratch_root() == str(override)


def test_fixture_resolves_and_relocates(tmp_path, no_install_env):
    install = _layout(tmp_path)
    d = ip.discover(search_from=install / "app" / "backend")
    assert d is not None
    assert d.app_root == install
    assert d.helper == install / "app"
    assert d.python == install / "runtime" / "python" / "bin" / "python3"
    assert d.models == install / "models"
    assert d.cache == tmp_path / "var-cache"  # "../var-cache" escapes app root lexically
    assert d.scratch is None
    assert d.user_data is None
    # Relocate the WHOLE tree: every root (including the ".." one) tracks the new location.
    moved = tmp_path.parent / f"{tmp_path.name}-moved"
    if moved.exists():
        shutil.rmtree(moved)
    shutil.move(str(tmp_path), str(moved))
    d2 = ip.discover(search_from=moved / "install" / "app" / "backend")
    assert d2 is not None
    assert d2.helper == moved / "install" / "app"
    assert d2.cache == moved / "var-cache"


def test_ck_install_env_accepts_file_or_folder(tmp_path, monkeypatch, no_install_env):
    install = _layout(tmp_path)
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(install / ip.DESCRIPTOR_NAME))
    assert ip.discover().descriptor == install / ip.DESCRIPTOR_NAME
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(install))
    assert ip.discover().descriptor == install / ip.DESCRIPTOR_NAME


def test_ck_install_env_pointing_nowhere_raises(tmp_path, monkeypatch, no_install_env):
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(tmp_path / "nowhere"))
    with pytest.raises(ip.InstallDescriptorError, match="keyloom-install.json"):
        ip.discover()


@pytest.mark.parametrize(
    "body,match",
    [
        ('{"schema":2,"roots":{"python":"p","helper":"h"}}', "schema version 2"),
        ('{"roots":{"python":"p","helper":"h"}}', "missing or non-integer schema"),
        ('{"schema":1,"future":1,"roots":{"python":"p","helper":"h"}}', 'unknown field "future"'),
        ('{"schema":1,"roots":{"python":"p","helper":"h","wat":"x"}}', 'unknown roots field "wat"'),
        ('{"schema":1,"roots":{"helper":"h"}}', "roots.python and roots.helper are required"),
        (
            '{"schema":1,"roots":{"python":"p","helper":"h","cache":""}}',
            "non-empty path or null",
        ),
        ("{nope", "invalid JSON"),
    ],
)
def test_descriptor_errors_name_the_field_and_the_fix(tmp_path, monkeypatch, no_install_env, body, match):
    install = _layout(tmp_path)
    (install / ip.DESCRIPTOR_NAME).write_text(body)
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(install))
    with pytest.raises(ip.InstallDescriptorError, match=match):
        ip.discover()


def test_v2_models_dir_precedence(tmp_path, monkeypatch, no_install_env):
    install = _layout(tmp_path)
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(install))
    # Descriptor models root beats the platform default...
    assert vp.get_v2_models_dir() == install / "models"
    # ...but an explicit env override beats the descriptor.
    monkeypatch.setenv(ip.MODELS_DIR_ENV, str(tmp_path / "explicit"))
    assert vp.get_v2_models_dir() == tmp_path / "explicit"
    # A broken descriptor is an error, never a silent platform-default fallback.
    (install / ip.DESCRIPTOR_NAME).write_text('{"schema":2,"roots":{}}')
    monkeypatch.delenv(ip.MODELS_DIR_ENV, raising=False)
    with pytest.raises(ip.InstallDescriptorError, match="schema version 2"):
        vp.get_v2_models_dir()


def test_setup_helper_env_additive_never_overriding(tmp_path, monkeypatch, no_install_env):
    install = _layout(tmp_path)
    monkeypatch.setenv(ip.CK_INSTALL_ENV, str(install))
    env = ip.setup_helper_env(base={})
    assert env[ip.CK_INSTALL_ENV] == str(install / ip.DESCRIPTOR_NAME)
    assert env[ip.MODELS_DIR_ENV] == str(install / "models")
    # Explicit values in the base env win (user overrides are preserved).
    env = ip.setup_helper_env(
        base={ip.MODELS_DIR_ENV: "/explicit", ip.CK_INSTALL_ENV: "/explicit-install"}
    )
    assert env[ip.MODELS_DIR_ENV] == "/explicit"
    assert env[ip.CK_INSTALL_ENV] == "/explicit-install"


def test_platform_user_data_windows_needs_no_home():
    roaming = r"C:\Users\u\AppData\Roaming"
    local = r"C:\Users\u\AppData\Local"
    # Pure helpers mirror rust-ui settings/runtime split: no HOME involved on Windows.
    assert ip.windows_user_data(None, None) is None
    assert ip.windows_user_data("", "") is None
    assert ip.windows_user_data(roaming, local) == Path(roaming) / "Keyloom"
    assert ip.windows_user_data(None, local) == Path(local) / "Keyloom"


def test_platform_cache_prefers_local_appdata():
    roaming = r"C:\Users\u\AppData\Roaming"
    local = r"C:\Users\u\AppData\Local"
    assert ip.windows_cache(local, roaming) == Path(local) / "Keyloom/cache"
    assert ip.windows_cache(None, roaming) == Path(roaming) / "Keyloom/cache"
    assert ip.windows_cache(None, None) is None


def test_platform_cache_and_scratch(monkeypatch, tmp_path):
    monkeypatch.delenv("XDG_CACHE_HOME", raising=False)
    monkeypatch.delenv("XDG_DATA_HOME", raising=False)
    monkeypatch.setenv("HOME", str(tmp_path))
    assert ip.platform_user_data() == tmp_path / ".local/share/Keyloom"
    assert ip.platform_cache() == tmp_path / ".cache/Keyloom"
    assert ip.platform_scratch() == tmp_path / ".local/share/Keyloom/scratch"


def test_read_only_app_root_resolves(tmp_path, no_install_env):
    install = _layout(tmp_path)
    os.chmod(install, 0o555)
    try:
        d = ip.load(install / ip.DESCRIPTOR_NAME)
        assert d.helper == install / "app"
    finally:
        os.chmod(install, 0o755)  # let pytest remove tmp_path


def test_dev_checkout_without_descriptor_is_none(no_install_env):
    assert ip.discover() is None
