"""CorridorKey 2.0 preferences + app identity (isolated from EZ-CorridorKey v1)."""
from __future__ import annotations

import pytest
from PySide6.QtCore import QCoreApplication, QSettings
from PySide6.QtWidgets import QApplication

from ui.widgets import preferences_dialog as pd


# Module-level reference: a QApplication that gets garbage-collected while
# widgets are alive crashes the interpreter at exit.
_APP = QApplication.instance() or QApplication([])


@pytest.fixture
def qapp_settings(tmp_path):
    app = _APP
    old = (QCoreApplication.organizationName(), QCoreApplication.applicationName())
    QSettings.setPath(QSettings.IniFormat, QSettings.UserScope, str(tmp_path))
    QSettings.setDefaultFormat(QSettings.IniFormat)
    QCoreApplication.setOrganizationName("TestOrg")
    QCoreApplication.setApplicationName("TestApp")
    yield app
    # Destroy dialogs before interpreter teardown (late native destruction segfaults).
    import shiboken6
    for w in QApplication.topLevelWidgets():
        if isinstance(w, pd.PreferencesDialog):
            shiboken6.delete(w)
    QCoreApplication.setOrganizationName(old[0])
    QCoreApplication.setApplicationName(old[1])


def test_removed_keys_gone():
    for name in ("KEY_MODEL_RESOLUTION", "KEY_INFERENCE_BACKEND", "KEY_PARALLEL_CLIPS",
                 "DEFAULT_MODEL_RESOLUTION", "DEFAULT_PARALLEL_CLIPS"):
        assert not hasattr(pd, name)


def test_dialog_has_no_removed_widgets(qapp_settings):
    dlg = pd.PreferencesDialog()
    assert not hasattr(dlg, "_model_resolution_combo")
    assert not hasattr(dlg, "_backend_combo")


def test_save_reopen_roundtrip(qapp_settings):
    dlg = pd.PreferencesDialog()
    idx = dlg._exr_compression_combo.findData("zip")
    assert idx >= 0
    dlg._exr_compression_combo.setCurrentIndex(idx)
    dlg._save_and_accept()
    assert pd.get_setting_str(pd.KEY_EXR_COMPRESSION, "") == "zip"
    assert pd.PreferencesDialog()._exr_compression_combo.currentData() == "zip"
    # obsolete keys are never written
    s = QSettings()
    assert not s.contains("inference/model_resolution")
    assert not s.contains("inference/backend")


def test_app_identity_and_no_migration(tmp_path):
    """Run create_app in a clean process with an isolated config home."""
    import os, subprocess, sys, textwrap
    from pathlib import Path
    repo = Path(__file__).resolve().parent.parent
    env = dict(os.environ, QT_QPA_PLATFORM="offscreen",
               XDG_CONFIG_HOME=str(tmp_path), HOME=str(tmp_path))
    code = textwrap.dedent("""
        from PySide6.QtCore import QSettings, QCoreApplication
        old = QSettings("Corridor Digital", "CorridorKey")
        old.setValue("ui/language", "fr"); old.sync()
        import ui.app as m
        assert not hasattr(m, "_migrate_legacy_settings")
        app = m.create_app(["x"])
        s = QSettings()
        print(QCoreApplication.applicationName(), QCoreApplication.organizationName(),
              s.value("ui/language"), s.contains("_migrated_from_legacy"))
    """)
    r = subprocess.run([sys.executable, "-c", code], cwd=repo, env=env,
                       capture_output=True, text=True, timeout=60)
    assert r.returncode == 0, r.stderr
    assert r.stdout.strip().splitlines()[-1] == "EZ-CorridorKey2 EZSCAPE en False"
