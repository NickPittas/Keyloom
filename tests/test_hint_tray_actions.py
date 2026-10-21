"""Tests for IOTrayActionsMixin hint clearing (delegation, guard, failures)."""
from __future__ import annotations

import os

from PySide6.QtWidgets import QApplication, QMessageBox

from backend import ClipEntry
from ui.widgets import io_tray_actions
from ui.widgets.io_tray_actions import IOTrayActionsMixin

_APP = QApplication.instance() or QApplication([])


class _Model:
    def __init__(self):
        self.updated = []
        self.layout = 0
        self.layoutChanged = self

    def update_clip_state(self, name, state):
        self.updated.append(name)

    def emit(self):
        self.layout += 1


class _Sig:
    def __init__(self):
        self.calls = []

    def emit(self, *a):
        self.calls.append(a)


class _Tray(IOTrayActionsMixin):
    def __init__(self):
        self._model = _Model()
        self.clip_clicked = _Sig()
        self.extract_requested = _Sig()

    def tr(self, s):
        return s


class _Box:
    def __init__(self, answer=QMessageBox.Yes):
        self.answer = answer
        self.questions = self.warnings = self.infos = 0
        self.last = None
        self.Yes, self.No = QMessageBox.Yes, QMessageBox.No

    def question(self, *a, **k):
        self.questions += 1
        return self.answer

    def warning(self, *a, **k):
        self.warnings += 1
        self.last = a
        return None

    def information(self, *a, **k):
        self.infos += 1


def _touch(p, t="x"):
    os.makedirs(os.path.dirname(p), exist_ok=True)
    open(p, "w").write(t)


def _clip(tmp_path):
    root = str(tmp_path / "shot")
    for i in range(3):
        _touch(os.path.join(root, "Frames", f"frame_{i:05d}.png"))
        _touch(os.path.join(root, "AlphaHint", f"frame_{i:05d}.png"))
        _touch(os.path.join(root, "VideoMamaMaskHint", f"frame_{i:05d}.png"))
    _touch(os.path.join(root, "VideoMamaMaskHint", ".corridorkey_mask_manifest.json"), "{}")
    _touch(os.path.join(root, "Output", "FG", "frame_00000.png"))
    _touch(os.path.join(root, "annotations.json"), "{}")
    c = ClipEntry(name="shot", root_path=root)
    c.find_assets()
    return c, root


def _patch(monkeypatch, box):
    monkeypatch.setattr(io_tray_actions, "QMessageBox", box)


def test_clear_alpha_keeps_masks_and_annotations(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    _patch(monkeypatch, _Box())
    t = _Tray()
    t._clear_alpha_batch([clip])
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert os.path.isdir(os.path.join(root, "VideoMamaMaskHint"))
    assert os.path.isfile(os.path.join(root, "annotations.json"))
    assert clip.alpha_asset is None
    assert t._model.updated == ["shot"] and t.clip_clicked.calls


def test_clear_mask_keeps_alpha_and_removes_manifest(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    _patch(monkeypatch, _Box())
    t = _Tray()
    t._clear_mask_batch([clip])
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert os.path.isdir(os.path.join(root, "AlphaHint"))
    assert os.path.isfile(os.path.join(root, "annotations.json"))
    assert clip.mask_asset is None
    assert t._model.updated == ["shot"]


def test_clear_all_removes_hints_and_outputs(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    _patch(monkeypatch, _Box())
    t = _Tray()
    t._clear_all_batch([clip])
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert not os.path.exists(os.path.join(root, "Output", "FG", "frame_00000.png"))
    assert os.path.isfile(os.path.join(root, "annotations.json"))
    assert os.path.isdir(os.path.join(root, "Frames"))


def test_guard_blocks_when_busy(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    box = _Box()
    _patch(monkeypatch, box)
    t = _Tray()
    t.set_hint_mutation_guard(lambda: False)
    for fn in (t._clear_alpha_batch, t._clear_mask_batch, t._clear_all_batch):
        fn([clip])
    assert box.infos == 3 and box.questions == 0
    assert os.path.isdir(os.path.join(root, "AlphaHint"))
    assert os.path.isdir(os.path.join(root, "VideoMamaMaskHint"))
    assert t._model.updated == []


def test_guard_allows_when_idle(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    _patch(monkeypatch, _Box())
    t = _Tray()
    t.set_hint_mutation_guard(lambda: True)
    t._clear_alpha_batch([clip])
    assert not os.path.exists(os.path.join(root, "AlphaHint"))


def test_declined_confirmation_deletes_nothing(tmp_path, monkeypatch):
    clip, root = _clip(tmp_path)
    _patch(monkeypatch, _Box(QMessageBox.No))
    t = _Tray()
    t._clear_alpha_batch([clip])
    assert os.path.isdir(os.path.join(root, "AlphaHint"))
    assert t._model.updated == []


def test_oserror_reported_once_and_state_refreshed(tmp_path, monkeypatch):
    c1, _ = _clip(tmp_path / "a")
    c2, _ = _clip(tmp_path / "b")
    c1.name, c2.name = "one", "two"
    box = _Box()
    _patch(monkeypatch, box)

    def boom(clip, **k):
        raise OSError("denied")

    monkeypatch.setattr(io_tray_actions, "clear_hint_assets", boom)
    t = _Tray()
    t._clear_alpha_batch([c1, c2])
    assert box.warnings == 1
    assert "one" in box.last[2] and "two" in box.last[2] and "denied" in box.last[2]
    assert t._model.updated == ["one", "two"]


def test_guard_rechecked_after_confirmation(tmp_path, monkeypatch):
    """A GPU job that starts while the confirm dialog is open blocks the clear."""
    clip, root = _clip(tmp_path)
    box = _Box()
    _patch(monkeypatch, box)
    t = _Tray()
    for fn in (t._clear_alpha_batch, t._clear_mask_batch, t._clear_all_batch):
        answers = iter([True, False])  # idle before the dialog, busy after
        t.set_hint_mutation_guard(lambda: next(answers))
        fn([clip])
    assert box.questions == 3 and box.infos == 3
    assert os.path.isdir(os.path.join(root, "AlphaHint"))
    assert os.path.isdir(os.path.join(root, "VideoMamaMaskHint"))
