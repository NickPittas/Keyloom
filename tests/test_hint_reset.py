"""Reset Hint orchestration (HintResetMixin) — offscreen, no GPU."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import json
import types
from typing import ClassVar

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QApplication

from backend import ClipEntry, ClipState
from backend.clip_state import MASK_TRACK_MANIFEST
from backend.job_queue import GPUJobQueue
from ui.main_window_mixins import annotation_mixin, hint_reset_mixin
from ui.main_window_mixins.annotation_mixin import AnnotationMixin
from ui.main_window_mixins.hint_reset_mixin import HintResetMixin
from ui.widgets.annotation_overlay import AnnotationModel
from ui.widgets.parameter_panel import ParameterPanel

_APP = QApplication.instance() or QApplication([])


def _touch(path, text="x"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def _make_clip(tmp_path, alpha=True, mask=True, complete=False, frames=3):
    root = str(tmp_path / "shot")
    for i in range(frames):
        _touch(os.path.join(root, "Frames", f"frame_{i:05d}.png"))
        if alpha:
            _touch(os.path.join(root, "AlphaHint", f"frame_{i:05d}.png"))
        if mask:
            _touch(os.path.join(root, "VideoMamaMaskHint", f"frame_{i:05d}.png"))
        if complete:
            _touch(os.path.join(root, "Output", "FG", f"frame_{i:05d}.png"))
    if mask:
        _touch(os.path.join(root, MASK_TRACK_MANIFEST), "{}")
    if complete:
        _touch(
            os.path.join(root, "Output", ".corridorkey_manifest.json"),
            json.dumps({
                "engine": "corridorkey-v2", "complete": True, "frame_range": None,
                "enabled_outputs": ["fg"], "formats": {"fg": "png"},
            }),
        )
    _touch(os.path.join(root, "holdout_strokes.json"), "{}")
    clip = ClipEntry(name="shot", root_path=root)
    clip.find_assets()
    return clip


class _Panel:
    def __init__(self):
        self.reset = None
        self.track = None
        self.calls = []

    def set_reset_hint_enabled(self, v):
        self.reset = v

    def set_track_enabled(self, v):
        self.track = v

    def __getattr__(self, name):
        if name.startswith("set_"):
            return lambda v: self.calls.append((name, v))
        raise AttributeError(name)


class _Worker:
    def __init__(self, busy=False):
        self.busy = busy
        self.current_job_id = "j" if busy else None

    def is_busy(self):
        return self.busy


class _Viewer:
    def __init__(self):
        self.annotation_model = AnnotationModel()
        self._split_view = None

    def invalidate_cache(self):
        pass


class _Host(HintResetMixin):
    def __init__(self, clip, busy=False):
        self._current_clip = clip
        self._param_panel = _Panel()
        self._gpu_worker = _Worker(busy)
        self._service = types.SimpleNamespace(job_queue=GPUJobQueue())
        self._pipeline_steps = {}
        self._cancel_requested_job_id = None
        self.states = []
        self._clip_model = types.SimpleNamespace(
            clips=[clip],
            update_clip_state=lambda n, s: self.states.append((n, s)),
        )
        shared = AnnotationModel()
        self.shared = shared
        iv, ov = _Viewer(), _Viewer()
        iv.annotation_model = shared
        self.iv, self.ov = iv, ov  # output has its own private model
        self.stopped = 0
        self.refreshed = 0
        self._dual_viewer = types.SimpleNamespace(
            input_viewer=iv, output_viewer=ov,
            stop_playback=self._stop,
            refresh_generated_assets=self._refresh,
        )
        self.annotation_updates = 0

    def _stop(self):
        self.stopped += 1

    def _refresh(self):
        self.refreshed += 1

    def _update_annotation_info(self):
        self.annotation_updates += 1

    def _clip_has_videomama_ready_mask(self, clip):
        return clip.mask_asset is not None

    def _refresh_button_state(self):
        pass

    def _refresh_export_thumbnail(self, clip):
        pass

    # parent for the fake QMessageBox
    def __hash__(self):
        return id(self)


class _FakeBox:
    Question = 1
    DestructiveRole = 2
    RejectRole = 3
    choice = "reset"   # "reset" | "cancel"
    tick = False
    on_exec = None
    warnings: ClassVar[list] = []
    instances: ClassVar[list] = []

    def __init__(self, parent=None):
        self.buttons = {}
        self.cb = None
        self.clicked = None
        _FakeBox.instances.append(self)

    def __getattr__(self, name):
        if name.startswith("set"):
            return lambda *a: None
        raise AttributeError(name)

    def setCheckBox(self, cb):
        self.cb = cb

    def addButton(self, text, role):
        b = object()
        self.buttons[text] = b
        return b

    def exec(self):
        if _FakeBox.on_exec:
            _FakeBox.on_exec()
        self.cb.setChecked(_FakeBox.tick)
        self.clicked = self.buttons["Reset" if _FakeBox.choice == "reset" else "Cancel"]

    def clickedButton(self):
        return self.clicked

    @staticmethod
    def warning(parent, title, text):
        _FakeBox.warnings.append(text)


@pytest.fixture(autouse=True)
def fake_box(monkeypatch):
    _FakeBox.choice, _FakeBox.tick, _FakeBox.on_exec = "reset", False, None
    _FakeBox.warnings, _FakeBox.instances = [], []
    monkeypatch.setattr(hint_reset_mixin, "QMessageBox", _FakeBox)
    monkeypatch.setattr(annotation_mixin, "QMessageBox", _FakeBox)


def _paint(model, n=2):
    for i in range(n):
        model.start_stroke(i, 1.0, 1.0, "fg", 5)
        model.finish_stroke()


def test_cancel_leaves_everything(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    _FakeBox.choice = "cancel"
    h._on_reset_hint()
    assert os.path.isdir(os.path.join(clip.root_path, "AlphaHint"))
    assert h.stopped == 0 and not h.states
    assert _FakeBox.instances[0].cb.isChecked() is False


def test_reset_removes_hints_keeps_rest_and_annotations(tmp_path):
    clip = _make_clip(tmp_path)
    root = clip.root_path
    assert clip.state == ClipState.READY
    _touch(os.path.join(root, "annotations.json"), "{}")
    h = _Host(clip)
    _paint(h.shared)
    _paint(h.ov.annotation_model)
    h._on_reset_hint()
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert not os.path.exists(os.path.join(root, MASK_TRACK_MANIFEST))
    assert os.path.exists(os.path.join(root, "annotations.json"))
    assert os.path.exists(os.path.join(root, "holdout_strokes.json"))
    assert h.shared.has_annotations()  # default: annotations kept
    assert clip.state == ClipState.RAW and clip.alpha_asset is None
    assert h.states == [("shot", ClipState.RAW)]
    assert h.stopped == 1 and h.refreshed == 1 and h.annotation_updates == 1


def test_masked_resets_to_raw(tmp_path):
    clip = _make_clip(tmp_path, alpha=False)
    assert clip.state == ClipState.MASKED
    h = _Host(clip)
    h._on_reset_hint()
    assert clip.state == ClipState.RAW


def test_checked_clears_all_annotation_models_and_file(tmp_path):
    clip = _make_clip(tmp_path)
    root = clip.root_path
    h = _Host(clip)
    _paint(h.shared)
    _paint(h.ov.annotation_model)
    h.shared.save(root)
    assert os.path.exists(os.path.join(root, "annotations.json"))
    _FakeBox.tick = True
    h._on_reset_hint()
    assert not os.path.exists(os.path.join(root, "annotations.json"))
    assert not h.shared.has_annotations()
    assert not h.ov.annotation_model.has_annotations()
    # a later save from either private model cannot resurrect prompts
    h.ov.annotation_model.save(root)
    h.shared.save(root)
    assert not os.path.exists(os.path.join(root, "annotations.json"))
    assert os.path.exists(os.path.join(root, "holdout_strokes.json"))


def test_complete_clip_keeps_outputs_and_stays_complete(tmp_path):
    clip = _make_clip(tmp_path, complete=True)
    root = clip.root_path
    h = _Host(clip)
    h._on_reset_hint()
    assert not os.path.exists(os.path.join(root, "AlphaHint"))
    assert os.path.exists(os.path.join(root, "Output", ".corridorkey_manifest.json"))
    assert os.path.exists(os.path.join(root, "Output", "FG", "frame_00000.png"))
    assert clip.state == ClipState.COMPLETE
    assert h.states == [("shot", ClipState.COMPLETE)]


def test_refused_when_worker_busy(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip, busy=True)
    h._on_reset_hint()
    assert not _FakeBox.instances
    assert os.path.isdir(os.path.join(clip.root_path, "AlphaHint"))
    h._refresh_hint_button_state()
    assert h._param_panel.reset is False and h._param_panel.track is False


def test_refused_when_job_queued(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    h._service.job_queue = types.SimpleNamespace(pending_count=1, running_count=0)
    assert not h._hint_mutation_allowed()
    h._on_reset_hint()
    assert not _FakeBox.instances


def test_refused_when_pipeline_or_processing(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    h._pipeline_steps = {"shot": [1]}
    assert not h._hint_mutation_allowed()
    h._pipeline_steps = {}
    clip.set_processing(True)
    assert not h._hint_mutation_allowed()


def test_refused_if_clip_changed_during_dialog(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    _FakeBox.on_exec = lambda: setattr(h, "_current_clip", ClipEntry(name="o", root_path="/tmp/o"))
    h._on_reset_hint()
    assert os.path.isdir(os.path.join(clip.root_path, "AlphaHint"))
    assert _FakeBox.warnings and h.stopped == 0


def test_refused_if_became_busy_during_dialog(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    _FakeBox.on_exec = lambda: setattr(h._gpu_worker, "busy", True)
    h._on_reset_hint()
    assert os.path.isdir(os.path.join(clip.root_path, "AlphaHint"))
    assert _FakeBox.warnings


def test_deletion_error_warns_and_does_not_claim_success(tmp_path, monkeypatch):
    clip = _make_clip(tmp_path)

    def boom(c, **k):
        raise OSError("denied")

    monkeypatch.setattr(hint_reset_mixin, "clear_hint_assets", boom)
    h = _Host(clip)
    h._on_reset_hint()
    assert len(_FakeBox.warnings) == 1 and "denied" in _FakeBox.warnings[0]
    assert clip.state == ClipState.READY  # truthful: still has alpha on disk
    assert h.refreshed == 1  # still rescanned


def test_idle_worker_allowed_and_button_state(tmp_path):
    clip = _make_clip(tmp_path)
    h = _Host(clip)
    h._refresh_hint_button_state()
    assert h._param_panel.reset is True and h._param_panel.track is True
    h._on_reset_hint()
    h._refresh_hint_button_state()
    assert h._param_panel.reset is False  # nothing left to reset
    h._current_clip = None
    h._refresh_hint_button_state()
    assert h._param_panel.reset is False


def test_real_panel_reset_signal_and_track_gate():
    panel = ParameterPanel()
    got = []
    panel.reset_hint_requested.connect(lambda: got.append(1))
    assert not panel._reset_hint_btn.isEnabled()
    panel.set_reset_hint_enabled(True)
    panel._reset_hint_btn.click()
    assert got == [1]
    panel.set_track_enabled(False)
    panel.set_annotation_info(3, 10)
    assert not panel._track_masks_btn.isEnabled()
    panel.set_track_enabled(True)
    assert panel._track_masks_btn.isEnabled()


class _ClearHost(_Host, AnnotationMixin):
    pass


def test_clear_annotations_removes_masks_keeps_alpha_and_holdouts(tmp_path):
    clip = _make_clip(tmp_path)
    root = clip.root_path
    h = _ClearHost(clip)
    _paint(h.shared)
    h.shared.save(root)
    h._on_clear_annotations()
    assert not os.path.exists(os.path.join(root, "VideoMamaMaskHint"))
    assert not os.path.exists(os.path.join(root, MASK_TRACK_MANIFEST))
    assert not os.path.exists(os.path.join(root, "annotations.json"))
    assert os.path.exists(os.path.join(root, "AlphaHint", "frame_00000.png"))
    assert os.path.exists(os.path.join(root, "holdout_strokes.json"))
    assert clip.mask_asset is None and clip.alpha_asset is not None
    assert ("set_videomama_enabled", False) in h._param_panel.calls
    assert ("set_matanyone2_enabled", False) in h._param_panel.calls
    assert h.annotation_updates == 1


def test_clear_annotations_refused_when_busy(tmp_path):
    clip = _make_clip(tmp_path)
    h = _ClearHost(clip, busy=True)
    _paint(h.shared)
    h._on_clear_annotations()
    assert os.path.exists(os.path.join(clip.root_path, "VideoMamaMaskHint"))
    assert h.shared.has_annotations()


def _mark(clip, idx=(1,)):
    from backend.hint_keyframes import save_hint_keyframes
    stems = [f"frame_{i:05d}" for i in range(3)]
    save_hint_keyframes(clip.root_path, list(idx), stems)
    clip.find_assets()
    assert clip.hint_keyframes == tuple(idx)


def test_reset_clears_marks_sidecar_and_memory_no_resurrection(tmp_path):
    from backend.hint_keyframes import KEYFRAMES_FILENAME
    clip = _make_clip(tmp_path)
    _mark(clip)
    h = _Host(clip)
    h._on_reset_hint()
    assert not os.path.exists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert clip.hint_keyframes == () and clip.hint_keyframes_error is None
    clip.find_assets()
    assert clip.hint_keyframes == ()
    assert os.path.exists(os.path.join(clip.root_path, "holdout_strokes.json"))


def test_sidecar_only_reset_enabled_and_works(tmp_path):
    from backend.hint_keyframes import KEYFRAMES_FILENAME
    clip = _make_clip(tmp_path, alpha=False, mask=False)
    _mark(clip)
    h = _Host(clip)
    h._refresh_hint_button_state()
    assert h._param_panel.reset is True
    h._on_reset_hint()
    assert not os.path.exists(os.path.join(clip.root_path, KEYFRAMES_FILENAME))
    assert clip.hint_keyframes == ()


def test_cancel_keeps_marks(tmp_path):
    clip = _make_clip(tmp_path)
    _mark(clip)
    _FakeBox.choice = "cancel"
    _Host(clip)._on_reset_hint()
    assert clip.hint_keyframes == (1,)


def test_replacement_keeps_marks(tmp_path):
    from backend.hint_assets import clear_hint_assets
    clip = _make_clip(tmp_path)
    _mark(clip)
    clear_hint_assets(clip, clear_alpha=True, clear_masks=False)
    clip.find_assets()
    assert clip.hint_keyframes == (1,)
