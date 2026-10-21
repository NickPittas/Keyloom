"""setup_models CorridorKey 2.0 flow (no network)."""
import importlib.util
import sys
from pathlib import Path
from types import SimpleNamespace

import pytest

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))
from backend import v2_paths  # noqa: E402

_spec = importlib.util.spec_from_file_location("setup_models_v2", ROOT / "scripts" / "setup_models.py")
sm = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(sm)


def _fake_downloader(calls, skip=()):
    def _w(root, repo, rev):
        calls.append("ck")
        for n in ("corridorkey_v2", "foundation_stats"):
            f = root / "weights" / f"{n}.safetensors"
            f.parent.mkdir(parents=True, exist_ok=True)
            f.write_bytes(b"x")

    def _c(root):
        calls.append("cradio")
        if "cradio" in skip:
            return
        d = root / "vfms/cradio/C-RADIOv4-SO400M"
        d.mkdir(parents=True, exist_ok=True)
        (d / "config.json").write_text("{}")
        (d / "hf_model.py").write_text("#")
        (d / "model.safetensors").write_bytes(b"x")

    def _m(root):
        calls.append("moge")
        f = root / "vfms/moge/model.pt"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"x")

    def _r(root):
        calls.append("rvm")
        f = root / "vfms/rvm/pretrain_rvm_small16_256_204031069.npz"
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"x")

    return SimpleNamespace(CORRIDORKEY_REPO="r", download_corridorkey=_w,
                           download_cradio=_c, download_moge=_m, download_rvm=_r)


@pytest.fixture
def env(tmp_path, monkeypatch):
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(tmp_path))
    # Package files are vendored in the repo; assets dir is the tmp dir.
    calls = []
    monkeypatch.setattr(sm, "_load_v2_downloader", lambda: _fake_downloader(calls))
    return tmp_path, calls


def test_not_installed_initially(env):
    assert sm.is_installed("corridorkey") is False


def test_download_then_idempotent(env):
    _, calls = env
    assert sm.download_corridorkey() is True
    assert calls == ["ck", "cradio", "moge", "rvm"]
    assert sm.is_installed("corridorkey") is True
    calls.clear()
    assert sm.download_corridorkey() is True
    assert calls == []


def test_incomplete_download_fails(env, monkeypatch):
    _, calls = env
    monkeypatch.setattr(sm, "_load_v2_downloader", lambda: _fake_downloader(calls, skip=("cradio",)))
    assert sm.download_corridorkey() is False
    assert not sm.is_installed("corridorkey")


def test_download_exception_returns_false(env, monkeypatch):
    def boom():
        raise RuntimeError("offline")
    monkeypatch.setattr(sm, "_load_v2_downloader", boom)
    assert sm.download_corridorkey() is False


def test_check_lists_problems(env, monkeypatch, capsys):
    tmp, _ = env
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--check"])
    sm.main()
    out = capsys.readouterr().out
    assert "CorridorKey 2.0" in out and "NOT INSTALLED" in out
    assert str(tmp) in out and "missing" in out


def test_check_installed(env, monkeypatch, capsys):
    sm.download_corridorkey()
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--check"])
    sm.main()
    out = capsys.readouterr().out
    assert "[OK] CorridorKey 2.0" in out


def test_help(monkeypatch, capsys):
    monkeypatch.setattr(sys, "argv", ["setup_models.py", "--help"])
    with pytest.raises(SystemExit) as e:
        sm.main()
    assert e.value.code == 0
    assert "CorridorKey 2.0 weights + foundation models (~5GB, required)" in " ".join(capsys.readouterr().out.split())


@pytest.mark.parametrize("flag", ["--corridorkey-blue", "--corridorkey-mlx"])
def test_removed_flags(monkeypatch, flag):
    monkeypatch.setattr(sys, "argv", ["setup_models.py", flag])
    with pytest.raises(SystemExit) as e:
        sm.main()
    assert e.value.code == 2


def test_removed_symbols():
    for n in ("MLX_CHECKPOINTS", "is_mlx_installed", "download_corridorkey_mlx", "download_corridorkey_blue"):
        assert not hasattr(sm, n)
    assert "corridorkey-blue" not in sm.MODELS


def test_real_loader_exposes_functions():
    mod = sm._load_v2_downloader()
    assert mod.CORRIDORKEY_REPO == "nikopueringer/CorridorKey2.0"
    assert callable(mod.download_rvm)
