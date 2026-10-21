"""Hint tool enablement (HintResetMixin._refresh_hint_tools) — offscreen, no GPU."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import types

import pytest

from backend import ClipAsset, ClipEntry, ClipState, JobType
from backend.job_queue import GPUJobQueue
from backend.service import InferenceParams
from ui.main_window_mixins.hint_reset_mixin import HintResetMixin
from ui.main_window_mixins.inference_mixin import InferenceMixin
from ui.workers.job_helpers import create_job_snapshot

GENERATORS = ("applevision", "gvm", "birefnet", "chroma_key")
GUIDED = ("videomama", "matanyone2")


def _asset(n=3):
    a = ClipAsset.__new__(ClipAsset)
    a.path = "/tmp/x"
    a.asset_type = "sequence"
    a.frame_count = n
    a._dimensions = None
    return a


def _clip(state=ClipState.RAW, mask=False, with_input=True):
    c = ClipEntry(name="c", root_path="/tmp/does-not-exist-c")
    c.state = state
    c.input_asset = _asset() if with_input else None
    c.mask_asset = _asset() if mask else None
    return c


class _Panel:
    def __init__(self):
        self.v = {}

    def __getattr__(self, name):
        if name.startswith("set_") and name.endswith("_enabled"):
            key = name[4:-8]
            return lambda val: self.v.__setitem__(key, bool(val))
        raise AttributeError(name)


class _Worker:
    def __init__(self, busy=False):
        self.busy = busy
        self.current_job_id = "j" if busy else None

    def is_busy(self):
        return self.busy


class _Host(HintResetMixin):
    def __init__(self, clip, busy=False):
        self._current_clip = clip
        self._param_panel = _Panel()
        self._gpu_worker = _Worker(busy)
        self._service = types.SimpleNamespace(job_queue=GPUJobQueue())
        self._pipeline_steps = {}
        self._clip_model = types.SimpleNamespace(clips=[clip] if clip else [])

    def _clip_has_videomama_ready_mask(self, clip):
        return clip.mask_asset is not None


def _vals(host, names):
    return [host._param_panel.v[n] for n in names]


@pytest.mark.parametrize(
    "state", [ClipState.RAW, ClipState.MASKED, ClipState.READY, ClipState.COMPLETE],
)
def test_idle_generators_enabled(state):
    h = _Host(_clip(state))
    h._refresh_hint_tools()
    assert all(_vals(h, GENERATORS))


@pytest.mark.parametrize("state", [ClipState.EXTRACTING, ClipState.ERROR])
def test_extracting_error_disabled(state):
    h = _Host(_clip(state))
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS + GUIDED + ("import_alpha",)))


def test_missing_input_and_no_clip_disabled():
    h = _Host(_clip(ClipState.RAW, with_input=False))
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS))
    h = _Host(None)
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS + GUIDED))


def test_busy_worker_disables_everything():
    clip = _clip(ClipState.RAW, mask=True)
    h = _Host(clip, busy=True)
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS + GUIDED + ("import_alpha",)))


def test_queued_before_worker_disables_then_idle_reenables():
    clip = _clip(ClipState.RAW, mask=True)
    h = _Host(clip)
    job = create_job_snapshot(clip, job_type=JobType.GVM_ALPHA)
    assert h._service.job_queue.submit(job)
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS + GUIDED))
    h._service.job_queue.cancel_job(job)
    h._refresh_hint_tools()
    assert all(_vals(h, GENERATORS + GUIDED))


def test_terminal_idle_reenables_after_job_id_cleared():
    clip = _clip(ClipState.READY, mask=True)
    h = _Host(clip, busy=True)
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS))
    h._gpu_worker.busy = False
    h._gpu_worker.current_job_id = None
    h._refresh_hint_tools()
    assert all(_vals(h, GENERATORS + GUIDED))


def test_processing_clip_disables():
    clip = _clip(ClipState.RAW)
    clip.set_processing(True)
    h = _Host(clip)
    h._refresh_hint_tools()
    assert not any(_vals(h, GENERATORS))


@pytest.mark.parametrize("state", [ClipState.RAW, ClipState.MASKED, ClipState.READY])
def test_guided_needs_actual_mask(state):
    h = _Host(_clip(state, mask=False))
    h._refresh_hint_tools()
    assert not any(_vals(h, GUIDED))
    assert all(_vals(h, GENERATORS))
    h = _Host(_clip(state, mask=True))
    h._refresh_hint_tools()
    assert all(_vals(h, GUIDED))


def test_complete_disables_guided():
    h = _Host(_clip(ClipState.COMPLETE, mask=True))
    h._refresh_hint_tools()
    assert not any(_vals(h, GUIDED))


def test_inference_mixin_refresh_calls_hint_tools():
    calls = []
    s = types.SimpleNamespace(
        _current_clip=_clip(),
        _param_panel=types.SimpleNamespace(get_params=lambda: InferenceParams(use_alpha_hint=False)),
        _io_tray=types.SimpleNamespace(selected_count=lambda: 1),
        _status_bar=types.SimpleNamespace(update_button_state=lambda **k: None),
        _refresh_hint_tools=lambda: calls.append(1),
    )
    s._refresh_hint_tools_if_available = lambda: InferenceMixin._refresh_hint_tools_if_available(s)
    InferenceMixin._refresh_button_state(s)
    assert calls == [1]
