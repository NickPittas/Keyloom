"""CorridorKey 2.0 run actions on InferenceMixin (offscreen, no CUDA)."""
import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import types

import pytest

pytest.importorskip("PySide6", reason="PySide6 not installed")
from PySide6.QtWidgets import QMessageBox

from backend import ClipAsset, ClipEntry, ClipState, JobType
from backend.job_queue import GPUJobQueue
from backend.service import InferenceParams, OutputConfig
from backend.clip_state import InOutRange
from ui.main_window_mixins import inference_mixin
from ui.main_window_mixins.inference_mixin import InferenceMixin
from ui.main_window_mixins.session_mixin import SessionMixin


def _asset(n=3):
    a = ClipAsset.__new__(ClipAsset)
    a.path = "/tmp/x"
    a.asset_type = "sequence"
    a.frame_count = n
    a._dimensions = None
    return a


def _clip(name="c", state=ClipState.RAW, alpha=False, mask=False, n=3):
    c = ClipEntry(name=name, root_path=f"/tmp/{name}")
    c.state = state
    c.input_asset = _asset(n)
    c.alpha_asset = _asset(n) if alpha else None
    c.mask_asset = _asset(n) if mask else None
    return c


class _Panel:
    def __init__(self, hint=False):
        self.hint = hint

    def get_params(self):
        return InferenceParams(use_alpha_hint=self.hint)

    def get_output_config(self):
        return OutputConfig()


class _Status:
    def __init__(self):
        self.calls = []

    def update_button_state(self, **kw):
        self.calls.append(kw)


def _self(clips, hint=False, current=None, selected=1):
    started = []
    s = types.SimpleNamespace()
    s._param_panel = _Panel(hint)
    s._service = types.SimpleNamespace(job_queue=GPUJobQueue())
    s._current_clip = current
    s._clip_model = types.SimpleNamespace(clips=clips, update_clip_state=lambda *a: None)
    s._io_tray = types.SimpleNamespace(
        selected_count=lambda: selected, get_selected_clips=lambda: clips,
    )
    s._status_bar = _Status()
    s._pipeline_steps = {}
    s._pipeline_settings = {}
    s._auto_save_annotations = lambda: None
    s._start_worker_if_needed = lambda *a, **k: started.append((a, k))
    s.started = started
    return s


@pytest.fixture
def boxes(monkeypatch):
    log = {"warning": [], "info": []}
    # Patch the module reference, not the Shiboken class: restoring static
    # methods on QMessageBox itself segfaults the interpreter at exit.
    box = types.SimpleNamespace(
        warning=lambda *a, **k: log["warning"].append(a[2]),
        information=lambda *a, **k: log["info"].append(a[2]),
    )
    monkeypatch.setattr(inference_mixin, "QMessageBox", box)
    return log


def _jobs(s):
    return s._service.job_queue.queue_snapshot


# ── single clip ──

def test_run_raw_no_alpha_hint_off(boxes):
    clip = _clip()
    s = _self([clip], current=clip)
    InferenceMixin._on_run_inference(s)
    (job,) = _jobs(s)
    assert job.job_type == JobType.INFERENCE
    assert job.params["_inference_params"].use_alpha_hint is False
    assert s.started and not boxes["warning"]


def test_run_hint_on_without_alpha_warns(boxes):
    clip = _clip()
    s = _self([clip], hint=True, current=clip)
    InferenceMixin._on_run_inference(s)
    assert _jobs(s) == []
    assert "no AlphaHint" in boxes["warning"][0]


@pytest.mark.parametrize("state", [ClipState.EXTRACTING, ClipState.ERROR])
def test_run_extracting_error_warns(boxes, state):
    clip = _clip(state=state)
    s = _self([clip], current=clip)
    InferenceMixin._on_run_inference(s)
    assert _jobs(s) == []
    assert "cannot be keyed" in boxes["warning"][0]


def test_run_complete_resets_by_assets(boxes):
    for kw, expected in (({"alpha": True}, ClipState.READY), ({"mask": True}, ClipState.MASKED), ({}, ClipState.RAW)):
        clip = _clip(state=ClipState.COMPLETE, **kw)
        s = _self([clip], current=clip)
        InferenceMixin._on_run_inference(s)
        assert clip.state == expected
        assert len(_jobs(s)) == 1


def test_run_passes_frame_range(boxes):
    clip = _clip()
    clip.in_out_range = InOutRange(1, 2)
    s = _self([clip], current=clip)
    InferenceMixin._on_run_inference(s)
    assert _jobs(s)[0].params["_frame_range"] == (1, 2)


class _FakeBox:
    choice = "nohint"

    def __init__(self, *a, **k):
        self.btns = {}

    def setIcon(self, *a): pass
    def setWindowTitle(self, *a): pass
    def setText(self, *a): pass
    Warning = QMessageBox.Warning
    AcceptRole = QMessageBox.AcceptRole
    ActionRole = QMessageBox.ActionRole
    Cancel = QMessageBox.Cancel

    def addButton(self, x, role=None):
        if isinstance(x, str):
            self.btns[x] = x
            return x
        return "cancel"

    def exec(self): pass

    def clickedButton(self):
        return {"nohint": "Key without AlphaHint", "rerun": "Re-run GVM"}.get(
            _FakeBox.choice, "cancel")


def test_incomplete_alpha_key_without_hint(monkeypatch, boxes):
    monkeypatch.setattr(inference_mixin, "QMessageBox", _FakeBox)
    clip = _clip(state=ClipState.READY, alpha=True)
    clip.alpha_asset.frame_count = 2
    s = _self([clip], hint=True, current=clip)
    _FakeBox.choice = "nohint"
    InferenceMixin._on_run_inference(s)
    (job,) = _jobs(s)
    assert job.job_type == JobType.INFERENCE
    assert job.params["_inference_params"].use_alpha_hint is False


def test_incomplete_alpha_cancel_and_not_offered_when_hint_off(monkeypatch, boxes):
    monkeypatch.setattr(inference_mixin, "QMessageBox", _FakeBox)
    clip = _clip(state=ClipState.READY, alpha=True)
    clip.alpha_asset.frame_count = 2
    s = _self([clip], hint=True, current=clip)
    _FakeBox.choice = "cancel"
    InferenceMixin._on_run_inference(s)
    assert _jobs(s) == []
    # hint off: dialog never shown, job queued straight away
    s = _self([clip], hint=False, current=clip)
    InferenceMixin._on_run_inference(s)
    assert len(_jobs(s)) == 1


def test_incomplete_alpha_rerun_gvm_from_ready(monkeypatch, boxes):
    monkeypatch.setattr(inference_mixin, "QMessageBox", _FakeBox)
    clip = _clip(state=ClipState.READY, alpha=True)
    clip.alpha_asset.frame_count = 2
    s = _self([clip], hint=True, current=clip)
    _FakeBox.choice = "rerun"
    InferenceMixin._on_run_inference(s)
    (job,) = _jobs(s)
    assert job.job_type == JobType.GVM_ALPHA
    assert clip.state == ClipState.RAW


def test_resume(boxes):
    clip = _clip(state=ClipState.COMPLETE, alpha=True)
    s = _self([clip], current=clip)
    InferenceMixin._on_resume_inference(s)
    (job,) = _jobs(s)
    assert clip.state == ClipState.READY
    assert job.params["_inference_params"].use_alpha_hint is False
    assert "_frame_range" not in job.params


# ── run all ──

def test_run_all_hint_off(boxes):
    clips = [
        _clip("raw"), _clip("masked", ClipState.MASKED), _clip("ready", ClipState.READY, alpha=True),
        _clip("done", ClipState.COMPLETE, alpha=True), _clip("ext", ClipState.EXTRACTING),
        _clip("err", ClipState.ERROR),
    ]
    s = _self(clips)
    InferenceMixin._on_run_all_ready(s)
    assert [j.clip_name for j in _jobs(s)] == ["raw", "masked", "ready"]
    assert s.started[0][0][0] == _jobs(s)[0].id


def test_run_all_hint_on_only_ready_with_alpha(boxes):
    clips = [_clip("raw"), _clip("ready", ClipState.READY, alpha=True),
             _clip("done", ClipState.COMPLETE, alpha=True)]
    s = _self(clips, hint=True)
    InferenceMixin._on_run_all_ready(s)
    assert [j.clip_name for j in _jobs(s)] == ["ready"]


def test_run_all_no_screen_colour_dependence(boxes):
    clips = [_clip("a"), _clip("b")]
    clips[0]._screen_color_cache = "blue"
    clips[1]._screen_color_cache = "green"
    s = _self(clips)
    InferenceMixin._on_run_all_ready(s)
    assert [j.clip_name for j in _jobs(s)] == ["a", "b"]


# ── pipeline ──

def test_pipeline_hint_off_two_inference_jobs(boxes):
    clips = [_clip("raw"), _clip("ready", ClipState.READY, alpha=True)]
    s = _self(clips, selected=2)
    InferenceMixin._on_run_pipeline(s)
    jobs = _jobs(s)
    assert [j.job_type for j in jobs] == [JobType.INFERENCE, JobType.INFERENCE]
    assert s._pipeline_settings == {} and s._pipeline_steps == {}


def test_pipeline_hint_on_raw_chains_gvm(boxes):
    clips = [_clip("raw"), _clip("ready", ClipState.READY, alpha=True)]
    s = _self(clips, hint=True, selected=2)
    InferenceMixin._on_run_pipeline(s)
    by = {j.clip_name: j for j in _jobs(s)}
    assert by["raw"].job_type == JobType.GVM_ALPHA
    assert by["ready"].job_type == JobType.INFERENCE
    assert s._pipeline_steps == {"raw": [JobType.INFERENCE]}
    assert s._pipeline_settings["raw"]["params"].use_alpha_hint is True
    assert "ready" not in s._pipeline_settings


def test_pipeline_skips_extracting(boxes):
    s = _self([_clip("e", ClipState.EXTRACTING)], selected=2)
    InferenceMixin._on_run_pipeline(s)
    assert _jobs(s) == [] and boxes["info"]


# ── buttons ──

def test_refresh_button_state():
    clip = _clip()
    s = _self([clip], hint=False, current=clip)
    InferenceMixin._refresh_button_state(s)
    assert s._status_bar.calls[-1]["can_run"] is True
    s._param_panel.hint = True
    InferenceMixin._refresh_button_state(s)
    assert s._status_bar.calls[-1]["can_run"] is False


# ── removed v1 behaviour / session ──

def test_live_reprocess_removed():
    assert not hasattr(InferenceMixin, "_do_reprocess")
    assert not hasattr(InferenceMixin, "_on_live_preview_toggled")
    assert not hasattr(InferenceMixin, "_on_reprocess_result")


def test_session_live_preview_ignored(monkeypatch):
    from ui.main_window_mixins import session_mixin
    # QTimer.singleShot without a QCoreApplication segfaults at interpreter exit.
    monkeypatch.setattr(session_mixin, "QTimer",
                        types.SimpleNamespace(singleShot=lambda *a, **k: None))
    applied = {}
    s = types.SimpleNamespace(
        _param_panel=types.SimpleNamespace(
            get_params=lambda: InferenceParams(),
            get_output_config=lambda: OutputConfig(),
            set_params=lambda p: applied.setdefault("p", p),
            set_output_config=lambda c: None,
        ),
        _remember_current_clip_input_color_space=lambda: None,
        geometry=lambda: types.SimpleNamespace(x=lambda: 0, y=lambda: 0, width=lambda: 1, height=lambda: 1),
        _splitter=types.SimpleNamespace(sizes=lambda: [1, 1]),
        _vsplitter=types.SimpleNamespace(sizes=lambda: [1, 1]),
        _clips_dir=None, _current_clip=None, _clip_input_is_linear={},
        _ensure_window_mode=lambda: None,
    )
    data = SessionMixin._build_session_data(s)
    assert "live_preview" not in data
    SessionMixin._apply_session_data(s, {"live_preview": True, "params": InferenceParams().to_dict()})
    assert "p" in applied
