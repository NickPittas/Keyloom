"""CorridorKey 2.0 GPU-worker result handling, auto-chain and clip selection."""
from __future__ import annotations

import os
from types import SimpleNamespace

import pytest

from backend import ClipState, InferenceParams, JobType, OutputConfig
from backend.job_queue import GPUJobQueue
from backend.service import CorridorKeyService
from ui.main_window_mixins import clip_mixin
from ui.main_window_mixins.clip_mixin import ClipMixin
from ui.main_window_mixins.worker_mixin import WorkerMixin


class _Noop:
    def __getattr__(self, name):
        return lambda *a, **k: None


class _Model:
    def __init__(self, clips):
        self.clips = clips

    def update_clip_state(self, name, state):
        pass


class _Win(WorkerMixin):
    def __init__(self, clip, param_params=None):
        self._clip_model = _Model([clip])
        self._service = SimpleNamespace(job_queue=GPUJobQueue())
        self._status_bar = _Noop()
        self._queue_panel = _Noop()
        self._io_tray = _Noop()
        self._dual_viewer = _Noop()
        self._param_panel = SimpleNamespace(
            get_params=lambda: param_params or InferenceParams(),
            get_output_config=lambda: OutputConfig(),
            set_videomama_enabled=lambda *a: None,
            set_matanyone2_enabled=lambda *a: None,
            set_import_alpha_enabled=lambda *a: None,
        )
        self._live_asset_refresh_timer = _Noop()
        self._pending_live_asset_refresh_clip = None
        self._current_clip = None
        self._force_stop_armed = False
        self._cancel_requested_job_id = None
        self._active_job_id = None
        self._pipeline_steps = {}
        self._pipeline_settings = {}
        self._refresh_input_thumbnail = lambda c: None
        self._refresh_export_thumbnail = lambda c: None


@pytest.fixture(autouse=True)
def _silence_audio(monkeypatch):
    import ui.sounds.audio_manager as am
    monkeypatch.setattr(am.UIAudio, "mask_done", staticmethod(lambda: None), raising=False)
    monkeypatch.setattr(am.UIAudio, "inference_done", staticmethod(lambda: None), raising=False)
    monkeypatch.setattr(am.UIAudio, "error", staticmethod(lambda: None), raising=False)


@pytest.fixture
def clip(sample_clip, monkeypatch):
    # Default output dir = <clip>/Output regardless of the user's QSettings.
    from PySide6.QtCore import QSettings
    monkeypatch.setattr(QSettings, "value", lambda self, *a, **k: "")
    sample_clip.state = ClipState.RAW
    return sample_clip


def _write_outputs(clip, frame_range=None):
    out = clip.output_dir
    stems = [f"frame_{i:05d}" for i in range(3)]
    for sub in ("FG", "Matte", "Comp", "Processed"):
        os.makedirs(os.path.join(out, sub), exist_ok=True)
    cfg = OutputConfig()
    exts = {"FG": cfg.fg_format, "Matte": cfg.matte_format,
            "Comp": cfg.comp_format, "Processed": cfg.processed_format}
    for sub, ext in exts.items():
        for stem in stems:
            open(os.path.join(out, sub, f"{stem}.{ext}"), "wb").close()
    CorridorKeyService._write_manifest(
        SimpleNamespace(), out, cfg, InferenceParams(),
        source_stems=stems, frame_range=frame_range, complete=frame_range is None,
    )


def _queued(win):
    return list(win._service.job_queue._queue)


def test_chain_uses_frozen_settings(clip):
    win = _Win(clip)
    params = InferenceParams(despill_strength=0.9, use_alpha_hint=False)
    cfg = OutputConfig(fg_format="png")
    win._pipeline_steps[clip.name] = [JobType.INFERENCE]
    win._pipeline_settings[clip.name] = {"params": params, "output_config": cfg, "frame_range": (1, 2)}
    win._on_worker_clip_finished("j1", clip.name, JobType.GVM_ALPHA.value)
    jobs = _queued(win)
    assert len(jobs) == 1 and jobs[0].job_type == JobType.INFERENCE
    assert jobs[0].params["_inference_params"].despill_strength == 0.9
    assert jobs[0].params["_inference_params"].use_alpha_hint is False
    assert jobs[0].params["_output_config"].fg_format == "png"
    assert jobs[0].params["_frame_range"] == (1, 2)
    # chain exhausted
    assert clip.name not in win._pipeline_steps
    assert clip.name not in win._pipeline_settings


def test_legacy_chain_forces_alpha_hint(clip):
    win = _Win(clip, InferenceParams(use_alpha_hint=False))
    win._pipeline_steps[clip.name] = [JobType.INFERENCE]
    win._on_worker_clip_finished("j1", clip.name, JobType.GVM_ALPHA.value)
    jobs = _queued(win)
    assert len(jobs) == 1
    assert jobs[0].params["_inference_params"].use_alpha_hint is True
    assert not win._pipeline_steps and not win._pipeline_settings


def test_generator_finish_states(clip):
    win = _Win(clip)
    for jt in (JobType.CHROMA_KEY_ALPHA, JobType.GVM_ALPHA):
        clip.state = ClipState.RAW
        win._on_worker_clip_finished("j", clip.name, jt.value)
        assert clip.state == ClipState.READY


def test_inference_full_manifest_complete(clip):
    win = _Win(clip)
    _write_outputs(clip)
    win._on_worker_clip_finished("j", clip.name, JobType.INFERENCE.value)
    assert clip.state == ClipState.COMPLETE


def test_inference_partial_range_not_complete(clip):
    win = _Win(clip)
    _write_outputs(clip, frame_range=[1, 1])
    win._on_worker_clip_finished("j", clip.name, JobType.INFERENCE.value)
    assert clip.state != ClipState.COMPLETE
    assert clip.state in (ClipState.RAW, ClipState.MASKED, ClipState.READY)


def test_error_clears_chain_and_queues_nothing(clip, monkeypatch):
    from PySide6.QtWidgets import QMessageBox
    monkeypatch.setattr(QMessageBox, "critical", staticmethod(lambda *a, **k: None))
    win = _Win(clip)
    win._service.get_vram_info = dict
    win._pipeline_steps[clip.name] = [JobType.INFERENCE]
    win._pipeline_settings[clip.name] = {"params": InferenceParams(), "output_config": OutputConfig(),
                                         "frame_range": None}
    win._clip_model.update_clip_state = lambda *a: None
    win._on_worker_error("j", clip.name, "boom")
    assert not win._pipeline_steps and not win._pipeline_settings
    assert _queued(win) == []


def test_cancel_warning_clears_chain(clip):
    win = _Win(clip)
    win._pipeline_steps[clip.name] = [JobType.INFERENCE]
    win._pipeline_settings[clip.name] = {}
    win._on_worker_warning("j", f"Cancelled: {clip.name}")
    assert not win._pipeline_steps and not win._pipeline_settings


def test_blue_clip_no_download_offer(clip, monkeypatch):
    from PySide6.QtWidgets import QMessageBox
    assert not hasattr(ClipMixin, "_offer_blue_checkpoint_download")

    def _fail(*a, **k):
        raise AssertionError("QMessageBox shown")

    monkeypatch.setattr(QMessageBox, "question", staticmethod(_fail))
    clip._screen_color_cache = "blue"
    seen = []
    win = SimpleNamespace(
        _param_panel=SimpleNamespace(_bg_color=SimpleNamespace(currentIndex=lambda: 0)),
        _on_screen_color_changed=seen.append,
    )
    ClipMixin._detect_and_apply_screen_color(win, clip)
    assert seen == ["blue"]
    assert clip_mixin is not None
