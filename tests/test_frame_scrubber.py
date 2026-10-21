"""FrameScrubber: immediate navigation signals and clock-driven playback."""
from __future__ import annotations

import math

from PySide6.QtWidgets import QApplication

from ui.widgets.frame_scrubber import FrameScrubber

_APP = QApplication.instance() or QApplication([])


class Clock:
    def __init__(self):
        self.ms = 0

    def __call__(self):
        return self.ms


def make(total=100, fps=24.0, loop=True):
    s = FrameScrubber()
    s.set_range(total)
    s.set_fps(fps)
    clock = Clock()
    s._clock = clock
    s._loop_enabled = lambda: loop
    s.playback_log = []
    s.frames = []
    s.states = []
    s.playback_requested.connect(lambda i, c: s.playback_log.append((i, c)))
    s.frame_changed.connect(s.frames.append)
    s.playback_state_changed.connect(s.states.append)
    return s, clock


def play(s):
    s._start_playback()
    s.frames.clear()
    s.playback_log.clear()


def test_slider_changes_emit_immediately():
    s, _ = make()
    for v in (1, 2, 3, 5, 4):
        s._slider.setValue(v)
    assert s.frames == [1, 2, 3, 5, 4]


def test_marker_scrub_immediate():
    s, _ = make()
    s._on_marker_scrub(7)
    s._on_marker_scrub(9)
    assert s.frames == [7, 9]
    assert s.current_frame() == 9


def test_set_frame_silent():
    s, _ = make()
    s.set_frame(10)
    assert s.frames == []
    assert s.current_frame() == 10


def test_targets_follow_floor_elapsed_times_fps():
    for fps in (24.0, 23.976, 60.0):
        s, c = make(total=100000, fps=fps)
        play(s)
        for ms in (0, 10, 41, 42, 500, 1234, 5000):
            c.ms = ms
            s._playback_tick()
            assert s.playback_log[-1][0] == math.floor(ms * fps / 1000)
        s.stop_playback()


def test_delayed_tick_skips_frames():
    s, c = make(fps=24.0)
    play(s)
    c.ms = 1000
    s._playback_tick()
    assert s.playback_log[-1] == (24, 0)


def test_no_drift_over_1000_frames():
    s, c = make(total=5000, fps=23.976)
    play(s)
    c.ms = int(1000 * 1000 / 23.976)
    s._playback_tick()
    assert s.playback_log[-1][0] == math.floor(c.ms * 23.976 / 1000)
    assert abs(s.playback_log[-1][0] - 1000) <= 1


def test_loop_wrap_increments_cycle():
    s, c = make(total=10, fps=10.0)
    s.set_in_out(2, 5)
    s.set_frame(2)
    play(s)
    c.ms = 300
    s._playback_tick()
    assert s.playback_log[-1] == (5, 0)
    c.ms = 400
    s._playback_tick()
    assert s.playback_log[-1] == (2, 1)
    c.ms = 1000
    s._playback_tick()
    assert s.playback_log[-1] == (4, 2)
    assert s.is_playing()


def test_one_frame_range_loops():
    s, c = make(total=10, fps=10.0)
    s.set_in_out(3, 3)
    s.set_frame(3)
    play(s)
    c.ms = 250
    s._playback_tick()
    assert s.playback_log[-1] == (3, 2)
    assert s.is_playing()


def test_loop_off_stops_at_end():
    s, c = make(total=10, fps=10.0, loop=False)
    play(s)
    c.ms = 5000
    s._playback_tick()
    assert s.playback_log[-1] == (9, 0)
    assert not s.is_playing()
    assert s.states == [True, False]


def test_seek_reanchors_and_resume():
    s, c = make(total=1000, fps=10.0)
    play(s)
    c.ms = 1000
    s._playback_tick()
    assert s.playback_log[-1][0] == 10
    s._slider.setValue(500)  # user seek during playback
    c.ms = 1500
    s._playback_tick()
    assert s.playback_log[-1][0] == 505
    s.stop_playback()
    c.ms = 9000
    s.set_frame(100)
    s._start_playback()
    c.ms = 9200
    s._playback_tick()
    assert s.playback_log[-1][0] == 102


def test_set_fps_invalid_defaults_24():
    s, _ = make()
    for bad in (0, -5, float("nan"), float("inf"), "x", None):
        s.set_fps(30.0)
        s.set_fps(bad)
        assert s._fps == 24.0


def test_unchanged_range_refresh_keeps_playing():
    s, _ = make(total=50)
    play(s)
    s.set_range(50)
    s.set_in_out(None, None)
    assert s.is_playing()
    s.set_range(60)
    assert not s.is_playing()


def test_legacy_frame_changed_flag():
    s, c = make(fps=10.0)
    play(s)
    c.ms = 300
    s._playback_tick()
    assert s.frames == [3]
    s._legacy_playback_emit = False
    c.ms = 500
    s._playback_tick()
    assert s.frames == [3]
    assert s.playback_log[-1][0] == 5


def test_show_presented_frame_does_not_reanchor():
    s, c = make(total=100000, fps=24.0)
    play(s)
    c.ms = 1000
    s._playback_tick()
    assert s.playback_log[-1][0] == 24
    s.playback_log.clear()
    s.frames.clear()
    s.show_presented_frame(5)
    assert s._slider.value() == 5
    assert s.frames == [] and s.playback_log == []
    c.ms = 2000
    s._playback_tick()
    assert s.playback_log[-1][0] == 48
    s.stop_playback()
