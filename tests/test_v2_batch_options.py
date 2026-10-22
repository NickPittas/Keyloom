"""Tests for the CorridorKey 2 'no AlphaHint' option in the batch dialog."""
from __future__ import annotations

import os

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

import pytest
from PySide6.QtWidgets import QApplication

from backend.batch_scanner import BatchClipInfo
from backend.job_queue import JobType
from ui.widgets.batch_pipeline_dialog import (
    HINT_EXISTING,
    HINT_NONE_V2,
    BatchPipelineDialog,
)

LABEL = "CorridorKey 2 (no AlphaHint)"


@pytest.fixture(scope="module")
def _app():
    return QApplication.instance() or QApplication([])


def _clip(name, hint_type="none"):
    return BatchClipInfo(source_path=f"/tmp/{name}.mp4", name=name, hint_type=hint_type)


def _dialog(clips):
    d = BatchPipelineDialog()
    d._clips = clips
    d._populate_table()
    return d


def test_no_hint_default_is_v2(_app):
    d = _dialog([_clip("a")])
    assert d._global_no_hint.itemText(0) == LABEL
    assert d._per_clip_combos[0].itemText(0) == LABEL
    cfg = d.get_batch_config()[0]
    assert cfg.hint_method == HINT_NONE_V2
    assert cfg.use_alpha_hint is False
    assert cfg.alpha_job_type == JobType.INFERENCE


def test_generator_uses_hint(_app):
    d = _dialog([_clip("a")])
    d._per_clip_combos[0].setCurrentText("GVM")
    cfg = d.get_batch_config()[0]
    assert cfg.alpha_job_type == JobType.GVM_ALPHA and cfg.use_alpha_hint is True
    d._per_clip_combos[0].setCurrentText("BiRefNet")
    cfg = d.get_batch_config()[0]
    assert cfg.alpha_job_type == JobType.BIREFNET_ALPHA and cfg.use_alpha_hint is True


def test_maskhint_uses_hint(_app):
    cfg = _dialog([_clip("m", "maskhint")]).get_batch_config()[0]
    assert cfg.use_alpha_hint is True


def test_existing_alphahint_choice(_app):
    d = _dialog([_clip("h", "alphahint")])
    cfg = d.get_batch_config()[0]
    assert cfg.use_alpha_hint is True and cfg.hint_method == HINT_EXISTING
    d._per_clip_combos[0].setCurrentText(LABEL)
    cfg = d.get_batch_config()[0]
    assert cfg.use_alpha_hint is False and cfg.hint_method == HINT_NONE_V2


def test_global_change_skips_alphahint_rows(_app):
    d = _dialog([_clip("a"), _clip("h", "alphahint"), _clip("b")])
    d._global_no_hint.setCurrentText("GVM")
    cfgs = d.get_batch_config()
    assert [c.use_alpha_hint for c in cfgs] == [True, True, True]
    assert cfgs[0].alpha_job_type == cfgs[2].alpha_job_type == JobType.GVM_ALPHA
    assert cfgs[1].hint_method == HINT_EXISTING
    d._global_no_hint.setCurrentIndex(0)
    cfgs = d.get_batch_config()
    assert [c.use_alpha_hint for c in cfgs] == [False, True, False]


def test_mixed_per_clip_round_trip(_app):
    d = _dialog([_clip("a"), _clip("b"), _clip("h", "alphahint"), _clip("m", "maskhint")])
    d._per_clip_cb.setChecked(True)
    d._per_clip_combos[1].setCurrentText("GVM")
    d._per_clip_combos[2].setCurrentText(LABEL)
    cfgs = d.get_batch_config()
    assert [c.use_alpha_hint for c in cfgs] == [False, True, False, True]


# ---------------------------------------------------------------------------
# Batch mixin wiring
# ---------------------------------------------------------------------------

from types import SimpleNamespace

from backend import InferenceParams, OutputConfig
from backend.clip_state import ClipAsset, ClipEntry, ClipState
from backend.job_queue import GPUJob, GPUJobQueue, JobStatus
from ui.main_window_mixins.batch_pipeline_mixin import BatchPipelineMixin
from ui.widgets.batch_pipeline_dialog import BatchClipConfig


class _Dlg:
    def __init__(self):
        self.errors, self.running = {}, []

    def set_clip_error(self, name, msg=""):
        self.errors[name] = msg

    def set_clip_running(self, name):
        self.running.append(name)

    def enter_processing_mode(self, _clips):
        pass


class _Host(BatchPipelineMixin):
    def __init__(self, clip, config):
        self._init_batch_pipeline()
        self._batch_dialog = _Dlg()
        self._batch_configs = {clip.name: config}
        self._clip_model = SimpleNamespace(clips=[clip])
        self._service = SimpleNamespace(job_queue=GPUJobQueue())
        self._pipeline_steps = {}
        self._pipeline_settings = {}
        self._param_panel = SimpleNamespace(
            get_params=lambda: InferenceParams(use_alpha_hint=True),
            get_output_config=OutputConfig,
        )
        self._start_worker_if_needed = lambda *a, **k: None


def _entry(state, alpha=False):
    c = ClipEntry(name="c", root_path="/tmp/c", state=state)
    c.input_asset = ClipAsset("/tmp/c/Input", "sequence")
    if alpha:
        c.alpha_asset = ClipAsset("/tmp/c/AlphaHint", "sequence")
    return c


def _cfg(job_type, use_hint, method):
    return BatchClipConfig(
        clip_info=_clip("c"), alpha_job_type=job_type,
        use_alpha_hint=use_hint, hint_method=method,
    )


def test_mixin_no_hint_raw_queues_inference_only(_app):
    h = _Host(_entry(ClipState.RAW), _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    clip = h._clip_model.clips[0]
    job, steps, settings = h._create_batch_job(
        clip, h._batch_configs["c"], InferenceParams(use_alpha_hint=True), OutputConfig())
    assert job.job_type == JobType.INFERENCE
    assert job.params["_inference_params"].use_alpha_hint is False
    assert steps == [] and settings is None
    assert job.params["_output_config"] is not None


def test_mixin_existing_hint_ready(_app):
    h = _Host(_entry(ClipState.READY, alpha=True),
              _cfg(JobType.INFERENCE, True, HINT_EXISTING))
    job, steps, _ = h._create_batch_job(
        h._clip_model.clips[0], h._batch_configs["c"],
        InferenceParams(use_alpha_hint=False), OutputConfig())
    assert job.job_type == JobType.INFERENCE
    assert job.params["_inference_params"].use_alpha_hint is True and steps == []


def test_mixin_existing_hint_missing_errors(_app):
    h = _Host(_entry(ClipState.RAW), _cfg(JobType.INFERENCE, True, HINT_EXISTING))
    job, steps, settings = h._create_batch_job(
        h._clip_model.clips[0], h._batch_configs["c"],
        InferenceParams(), OutputConfig())
    assert job is None and steps == [] and settings is None
    assert "No AlphaHint" in h._batch_dialog.errors["c"]


@pytest.mark.parametrize("jt", [JobType.GVM_ALPHA, JobType.BIREFNET_ALPHA])
def test_mixin_generator_chains_with_frozen_settings(_app, jt):
    h = _Host(_entry(ClipState.RAW), _cfg(jt, True, "generated"))
    h._submit_batch_jobs()
    assert h._pipeline_steps["c"] == [JobType.INFERENCE]
    s = h._pipeline_settings["c"]
    assert s["params"].use_alpha_hint is True
    assert set(s) == {"params", "output_config", "frame_range"}


def test_mixin_complete_rerun_no_hint(_app):
    h = _Host(_entry(ClipState.COMPLETE), _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    h._submit_batch_jobs()
    assert h._batch_dialog.errors == {}
    assert h._batch_dialog.running == ["c"]


def test_mixin_extract_finished_no_hint(_app):
    clip = _entry(ClipState.RAW)
    h = _Host(clip, _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    h._batch_pending_extractions["c"] = h._batch_configs["c"]
    h._on_batch_extract_finished("c")
    assert h._batch_dialog.errors == {} and clip.is_processing


def test_mixin_extract_finished_uses_settings_frozen_at_submit(_app):
    clip = _entry(ClipState.EXTRACTING)
    h = _Host(clip, _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    h._param_panel = SimpleNamespace(
        get_params=lambda: InferenceParams(despill_strength=0.2),
        get_output_config=OutputConfig,
    )
    h._submit_batch_jobs()
    assert "c" in h._batch_pending_extractions
    h._param_panel = SimpleNamespace(
        get_params=lambda: InferenceParams(despill_strength=0.9),
        get_output_config=OutputConfig,
    )
    clip.state = ClipState.RAW
    h._on_batch_extract_finished("c")
    job = h._service.job_queue.next_job()
    assert job.params["_inference_params"].despill_strength == 0.2


def test_clear_pipeline_cancels_queued_and_running_batch_jobs(_app):
    h = _Host(_entry(ClipState.RAW), _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    q = h._service.job_queue
    running = GPUJob(job_type=JobType.INFERENCE, clip_name="c")
    queued = GPUJob(job_type=JobType.GVM_ALPHA, clip_name="c")
    other = GPUJob(job_type=JobType.INFERENCE, clip_name="other")
    for j in (running, queued, other):
        assert q.submit(j)
    q.start_job(running)
    h._pipeline_steps["c"] = [JobType.INFERENCE]
    h._pipeline_settings["c"] = {}
    h._cancel_batch_jobs({"c"})
    assert running.is_cancelled
    assert queued.status == JobStatus.CANCELLED
    assert other.status == JobStatus.QUEUED


def test_mixin_keyframes_panel_overridden_by_explicit_choice(_app):
    kf = InferenceParams(hint_mode="keyframes")
    h = _Host(_entry(ClipState.READY, alpha=True),
              _cfg(JobType.INFERENCE, True, HINT_EXISTING))
    job, _, _ = h._create_batch_job(
        h._clip_model.clips[0], h._batch_configs["c"], kf, OutputConfig())
    assert job.params["_inference_params"].hint_mode == "full"
    h = _Host(_entry(ClipState.RAW), _cfg(JobType.INFERENCE, False, HINT_NONE_V2))
    job, _, _ = h._create_batch_job(
        h._clip_model.clips[0], h._batch_configs["c"], kf, OutputConfig())
    assert job.params["_inference_params"].hint_mode == "none"


@pytest.mark.parametrize("jt", [JobType.GVM_ALPHA, JobType.BIREFNET_ALPHA,
                                JobType.MATANYONE2_ALPHA, JobType.VIDEOMAMA_ALPHA])
def test_mixin_generator_chain_forces_full_over_keyframes(_app, jt):
    h = _Host(_entry(ClipState.RAW), _cfg(jt, True, "generated"))
    h._param_panel.get_params = lambda: InferenceParams(hint_mode="keyframes")
    h._submit_batch_jobs()
    assert h._pipeline_settings["c"]["params"].hint_mode == "full"
