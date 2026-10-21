"""Confirmed alpha-generator re-runs (ModelRunMixin/ChromaKeyMixin) — offscreen, no GPU."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import types
from typing import ClassVar

import pytest
from PySide6.QtWidgets import QMessageBox

from backend import ClipEntry, ClipState, JobType
from backend.clip_state import MASK_TRACK_MANIFEST
from backend.job_queue import GPUJobQueue
from ui.main_window_mixins import hint_reset_mixin, model_run_mixin
from ui.main_window_mixins.chroma_key_mixin import ChromaKeyMixin
from ui.main_window_mixins.hint_reset_mixin import HintResetMixin
from ui.main_window_mixins.model_run_mixin import ModelRunMixin


def _touch(path, text="x"):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w") as f:
        f.write(text)


def _make_clip(tmp_path, alpha=3, mask=True, frames=3):
    root = str(tmp_path / "shot")
    for i in range(frames):
        _touch(os.path.join(root, "Frames", f"frame_{i:05d}.png"))
    for i in range(alpha):
        _touch(os.path.join(root, "AlphaHint", f"frame_{i:05d}.png"))
    if mask:
        for i in range(frames):
            _touch(os.path.join(root, "VideoMamaMaskHint", f"frame_{i:05d}.png"))
        _touch(os.path.join(root, MASK_TRACK_MANIFEST), "{}")
    _touch(os.path.join(root, "Output", "FG", "frame_00000.png"))
    _touch(os.path.join(root, "annotations.json"), "{}")
    clip = ClipEntry(name="shot", root_path=root)
    clip.find_assets()
    return clip


class _Worker:
    current_job_id = None

    def is_busy(self):
        return False


class _Host(HintResetMixin, ModelRunMixin, ChromaKeyMixin):
    def __init__(self, clip, busy=False):
        self._current_clip = clip
        self._gpu_worker = _Worker()
        if busy:
            self._gpu_worker.is_busy = lambda: True
        self._service = types.SimpleNamespace(job_queue=GPUJobQueue())
        self._pipeline_steps = {}
        self.states = []
        self._clip_model = types.SimpleNamespace(
            clips=[clip], update_clip_state=lambda n, s: self.states.append((n, s)),
        )
        self.started = []

    def _start_worker_if_needed(self, *a, **k):
        self.started.append((a, k))

    def _warn_mps_slow(self, _name):
        return True

    def _clip_has_videomama_ready_mask(self, clip):
        return clip.mask_asset is not None


class _Box:
    """Stands in for QMessageBox in both mixin modules (module refs patched)."""

    Yes, No = QMessageBox.Yes, QMessageBox.No
    AcceptRole, DestructiveRole = QMessageBox.AcceptRole, QMessageBox.DestructiveRole
    Cancel = QMessageBox.Cancel
    answer = QMessageBox.Yes
    partial = "Resume"  # Resume | Regenerate | Cancel
    questions: ClassVar[list] = []
    warnings: ClassVar[list] = []

    def __init__(self, *a, **k):
        self.btns = {}

    def __getattr__(self, name):
        if name.startswith("set"):
            return lambda *a, **k: None
        raise AttributeError(name)

    def addButton(self, x, role=None):
        if isinstance(x, str):
            self.btns[x] = x
            return x
        return "Cancel"

    def exec(self):
        pass

    def clickedButton(self):
        return _Box.partial

    @staticmethod
    def question(parent, title, text, buttons=None, default=None):
        _Box.questions.append(text)
        return _Box.answer

    @staticmethod
    def warning(parent, title, text):
        _Box.warnings.append(text)

    @staticmethod
    def information(*a, **k):
        pass


@pytest.fixture(autouse=True)
def box(monkeypatch):
    _Box.answer, _Box.partial = QMessageBox.Yes, "Resume"
    _Box.questions, _Box.warnings = [], []
    monkeypatch.setattr(hint_reset_mixin, "QMessageBox", _Box)
    monkeypatch.setattr(model_run_mixin, "QMessageBox", _Box)


def _run(h, job_type):
    if job_type == JobType.APPLEVISION_ALPHA:
        h._on_run_applevision()
    elif job_type == JobType.GVM_ALPHA:
        h._on_run_gvm()
    elif job_type == JobType.BIREFNET_ALPHA:
        h._on_run_birefnet("General")
    else:
        h._on_run_chroma_key({"screen_type": "green"})


ALL = (
    JobType.APPLEVISION_ALPHA, JobType.GVM_ALPHA,
    JobType.BIREFNET_ALPHA, JobType.CHROMA_KEY_ALPHA,
)


def _jobs(h):
    return h._service.job_queue.queue_snapshot


def _snapshot_kept(clip):
    root = clip.root_path
    return [
        os.path.join(root, "VideoMamaMaskHint", "frame_00000.png"),
        os.path.join(root, MASK_TRACK_MANIFEST),
        os.path.join(root, "annotations.json"),
        os.path.join(root, "Output", "FG", "frame_00000.png"),
    ]


@pytest.mark.parametrize("job_type", ALL)
def test_ready_replace_yes(tmp_path, job_type):
    clip = _make_clip(tmp_path, alpha=3)
    clip.state = ClipState.READY
    h = _Host(clip)
    _run(h, job_type)
    assert len(_Box.questions) == 1
    (job,) = _jobs(h)
    assert job.job_type == job_type
    assert not os.path.exists(os.path.join(clip.root_path, "AlphaHint"))
    assert all(os.path.exists(p) for p in _snapshot_kept(clip))
    assert clip.alpha_asset is None
    assert clip.state in (ClipState.RAW, ClipState.MASKED)
    assert h.started


@pytest.mark.parametrize("job_type", ALL)
def test_ready_replace_cancel_untouched(tmp_path, job_type):
    clip = _make_clip(tmp_path, alpha=3)
    clip.state = ClipState.READY
    _Box.answer = QMessageBox.No
    h = _Host(clip)
    _run(h, job_type)
    assert len(_Box.questions) == 1
    assert _jobs(h) == []
    assert os.path.exists(os.path.join(clip.root_path, "AlphaHint", "frame_00000.png"))
    assert clip.alpha_asset is not None
    assert clip.state == ClipState.READY


def test_gvm_complete_replace_keeps_outputs_and_normalizes_state(tmp_path):
    clip = _make_clip(tmp_path, alpha=3)
    clip.state = ClipState.COMPLETE
    h = _Host(clip)
    h._on_run_gvm()
    (job,) = _jobs(h)
    assert job.job_type == JobType.GVM_ALPHA
    assert clip.state != ClipState.COMPLETE  # never left COMPLETE for a generator job
    assert clip.alpha_asset is None  # no stale alpha pointer in the snapshot
    assert os.path.exists(os.path.join(clip.root_path, "Output", "FG", "frame_00000.png"))


def test_chroma_then_gvm_each_replace_once(tmp_path):
    clip = _make_clip(tmp_path, alpha=3, mask=False)
    clip.state = ClipState.READY
    h = _Host(clip)
    _run(h, JobType.CHROMA_KEY_ALPHA)
    assert len(_Box.questions) == 1
    # chroma job finished (simulate): alpha exists again, queue idle
    h._service.job_queue.cancel_all()
    _touch(os.path.join(clip.root_path, "AlphaHint", "frame_00000.png"))
    clip.find_assets()
    clip.set_processing(False)
    clip.state = ClipState.READY
    h._service.job_queue.clear_history()
    _run(h, JobType.GVM_ALPHA)
    assert len(_Box.questions) == 2


@pytest.mark.parametrize("choice", ["Resume", "Regenerate", "Cancel"])
def test_partial_alpha_flow(tmp_path, choice):
    clip = _make_clip(tmp_path, alpha=1)
    assert clip.state in (ClipState.RAW, ClipState.MASKED)
    _Box.partial = choice
    h = _Host(clip)
    h._on_run_gvm()
    assert _Box.questions == []  # partial uses Resume/Regenerate/Cancel, no replace prompt
    if choice == "Cancel":
        assert _jobs(h) == []
        assert os.path.exists(os.path.join(clip.root_path, "AlphaHint", "frame_00000.png"))
        return
    assert len(_jobs(h)) == 1
    exists = os.path.exists(os.path.join(clip.root_path, "AlphaHint"))
    assert exists is (choice == "Resume")
    assert all(os.path.exists(p) for p in _snapshot_kept(clip))


def test_chroma_partial_single_prompt(tmp_path):
    clip = _make_clip(tmp_path, alpha=1)
    h = _Host(clip)
    h._on_run_chroma_key({"screen_type": "green"})
    assert _Box.questions == []
    assert len(_jobs(h)) == 1


def test_regenerate_failure_aborts(tmp_path, monkeypatch):
    clip = _make_clip(tmp_path, alpha=1)
    _Box.partial = "Regenerate"

    def boom(*a, **k):
        raise OSError("locked")

    monkeypatch.setattr(model_run_mixin, "clear_hint_assets", boom)
    h = _Host(clip)
    h._on_run_gvm()
    assert _jobs(h) == []
    assert _Box.warnings and "locked" in _Box.warnings[0]


@pytest.mark.parametrize("job_type", ALL)
def test_busy_handler_refuses(tmp_path, job_type):
    clip = _make_clip(tmp_path, alpha=3)
    clip.state = ClipState.READY
    h = _Host(clip, busy=True)
    _run(h, job_type)
    assert _jobs(h) == []
    assert _Box.questions == []
    assert os.path.exists(os.path.join(clip.root_path, "AlphaHint", "frame_00000.png"))


@pytest.mark.parametrize("state", [ClipState.EXTRACTING, ClipState.ERROR])
def test_bad_state_refused(tmp_path, state):
    clip = _make_clip(tmp_path, alpha=0, mask=False)
    clip.state = state
    h = _Host(clip)
    h._on_run_gvm()
    assert _jobs(h) == []


def test_declined_guided_not_applicable_ready_without_mask(tmp_path):
    """READY alone never enables guided generators (button predicate)."""
    clip = _make_clip(tmp_path, alpha=3, mask=False)
    clip.state = ClipState.READY
    h = _Host(clip)
    assert not h._clip_has_videomama_ready_mask(clip)
