"""CLI (clip_manager) routes keyer inference through the CorridorKey 2.0 service."""
from __future__ import annotations

from pathlib import Path
from typing import ClassVar

import cv2
import numpy as np
import pytest

import clip_manager
from backend.errors import CorridorKeyError
from backend.service import InferenceParams


class FakeService:
    instances: ClassVar[list] = []
    calls: ClassVar[list] = []
    fail_on: ClassVar[set] = set()

    def __init__(self, *a, **k):
        FakeService.instances.append(self)

    def run_inference(self, clip, params, **kw):
        FakeService.calls.append((clip.name, params, kw))
        if clip.name in FakeService.fail_on:
            raise CorridorKeyError("boom")
        return []


@pytest.fixture(autouse=True)
def fake_service(monkeypatch):
    FakeService.instances, FakeService.calls, FakeService.fail_on = [], [], set()
    monkeypatch.setattr("backend.service.CorridorKeyService", FakeService)


def _make_clip(root, name):
    d = root / name / "Input"
    d.mkdir(parents=True)
    for i in range(2):
        assert cv2.imwrite(str(d / f"{i:05d}.png"), np.zeros((4, 4, 3), np.uint8))
    e = clip_manager.ClipEntry(name, str(root / name))
    e.find_assets()
    return e


@pytest.fixture
def clips(tmp_path):
    return [_make_clip(tmp_path, "a"), _make_clip(tmp_path, "b")]


def test_one_service_two_calls_default_params(clips):
    assert clip_manager.run_inference(clips, interactive=False) == 0
    assert len(FakeService.instances) == 1
    assert [c[0] for c in FakeService.calls] == ["a", "b"]
    assert all(c[1] == InferenceParams() for c in FakeService.calls)


def test_error_reported_and_next_clip_runs(clips):
    FakeService.fail_on = {"a"}
    assert clip_manager.run_inference(clips, interactive=False) == 1
    assert [c[0] for c in FakeService.calls] == ["a", "b"]


def test_interactive_answers_map_to_params(clips, monkeypatch):
    prompts = []
    answers = iter(["l", "0.3", "y", "low"])

    def fake_input(prompt=""):
        prompts.append(prompt)
        return next(answers)

    monkeypatch.setattr("builtins.input", fake_input)
    assert clip_manager.run_inference(clips) == 0
    p = FakeService.calls[0][1]
    assert p == InferenceParams(input_is_linear=True, despill_strength=0.3,
                                use_alpha_hint=True, vram_profile="low")
    assert len(prompts) == 4
    text = " ".join(prompts).lower()
    for banned in ("despeckle", "refiner", "garbage", "edge", "screen"):
        assert banned not in text


def test_interactive_defaults(clips, monkeypatch):
    monkeypatch.setattr("builtins.input", lambda prompt="": "")
    clip_manager.run_inference(clips)
    assert FakeService.calls[0][1] == InferenceParams()


def test_no_v1_engine_left():
    src = Path(clip_manager.__file__).read_text(encoding="utf-8")
    for token in ("process_frame", "CorridorKeyEngine", "inference_engine", "despeckle", "refiner"):
        assert token not in src
