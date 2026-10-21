import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

import main as app_main

ROOT = Path(__file__).resolve().parent.parent


def _make_bundle(root: Path):
    for rel in (
        "weights/corridorkey_v2.safetensors",
        "weights/foundation_stats.safetensors",
        "vfms/rvm/pretrain_rvm_small16_256_204031069.npz",
        "vfms/moge/model.pt",
    ):
        f = root / rel
        f.parent.mkdir(parents=True, exist_ok=True)
        f.write_bytes(b"x")
    c = root / "vfms/cradio/C-RADIOv4-SO400M"
    c.mkdir(parents=True, exist_ok=True)
    (c / "config.json").write_text("{}")
    (c / "hf_model.py").write_text("#")
    (c / "model.safetensors").write_bytes(b"x")


def _run(tmp_path, capsys):
    rc = app_main.run_model_path_check(str(tmp_path / "data"))
    out = capsys.readouterr().out
    return rc, json.loads(out[out.index("{"):])


@pytest.fixture
def models(tmp_path, monkeypatch):
    d = tmp_path / "models"
    monkeypatch.setenv("CORRIDORKEY_V2_MODELS_DIR", str(d))
    return d


def test_missing_assets_reported(models, tmp_path, capsys):
    rc, res = _run(tmp_path, capsys)
    assert rc == 1
    assert res["corridorkey_v2"] == "MISSING"
    assert any("setup_models.py --corridorkey" in e for e in res["errors"])


def test_full_bundle_ok_and_no_model_load(models, tmp_path, capsys, monkeypatch):
    _make_bundle(models)

    def boom(*a, **k):
        raise AssertionError("engine.load called")

    monkeypatch.setattr("backend.corridorkey_v2.CorridorKeyV2Engine.load", boom)
    rc, res = _run(tmp_path, capsys)
    assert res["corridorkey_v2"] == "OK"
    assert not [e for e in res.get("errors", []) if "CorridorKey 2.0" in e]
    assert res["corridorkey_v2_models_dir"] == str(models)
    assert res["corridorkey_v2_code_root"].endswith("CorridorKeyV2")


def test_opt_mode_rejected():
    r = subprocess.run(
        [sys.executable, "main.py", "--opt-mode", "speed", "--help"],
        cwd=ROOT, capture_output=True, text=True,
        env={**os.environ, "QT_QPA_PLATFORM": "offscreen"},
    )
    assert "--opt-mode" not in r.stdout
