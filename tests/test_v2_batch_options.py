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
