"""EZ-CorridorKey2 must never contact or run the upstream updater."""
from __future__ import annotations

import os
import subprocess
import urllib.request

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest
from PySide6.QtWidgets import QApplication, QMessageBox

# Hold the app at module level so it is never garbage collected mid-run.
_APP = QApplication.instance() or QApplication([])

import ui.main_window as main_window
from ui.main_window_mixins.settings_mixin import SettingsMixin


class _Btn:
    def __init__(self):
        self.visible = True

    def setVisible(self, v):
        self.visible = v


class _Fake:
    def __init__(self):
        self._update_btn = _Btn()


@pytest.fixture(autouse=True)
def _no_network(monkeypatch):
    def boom(*a, **k):
        raise AssertionError("network/subprocess call attempted")
    monkeypatch.setattr(urllib.request, "urlopen", boom)
    monkeypatch.setattr(urllib.request, "urlretrieve", boom)
    monkeypatch.setattr(subprocess, "Popen", boom)
    monkeypatch.setattr(subprocess, "run", boom)


def test_update_checker_class_removed():
    assert not hasattr(main_window, "_UpdateChecker")


def test_check_for_updates_is_noop():
    fake = _Fake()
    assert SettingsMixin._check_for_updates(fake) is None
    assert not hasattr(fake, "_update_thread")


def test_run_update_only_informs(monkeypatch):
    shown = []
    monkeypatch.setattr(QMessageBox, "information",
                        lambda *a, **k: shown.append(a))
    SettingsMixin._run_update(_Fake())
    assert len(shown) == 1


def test_update_button_never_shown():
    fake = _Fake()
    SettingsMixin._on_update_available(fake, "99.0.0")
    assert fake._update_btn.visible is False
    fake._update_btn.visible = True
    SettingsMixin._apply_update_button_setting(fake)
    assert fake._update_btn.visible is False
