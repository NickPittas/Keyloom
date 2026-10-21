"""Tests for ui.preview.frame_cache.FrameCache."""

from __future__ import annotations

from PySide6.QtGui import QImage
from PySide6.QtWidgets import QApplication

from ui.preview.frame_cache import LEGACY_CACHE_BYTES, PREVIEW_CACHE_BYTES, FrameCache

_APP = QApplication.instance() or QApplication([])


def _img(w: int = 8, h: int = 8) -> QImage:
    img = QImage(w, h, QImage.Format.Format_RGB888)
    img.fill(0)
    return img


def test_constants():
    assert PREVIEW_CACHE_BYTES == 512 * 1024 * 1024
    assert LEGACY_CACHE_BYTES == 64 * 1024 * 1024


def test_put_get_and_accounting_with_replacement():
    img = _img()
    n = img.sizeInBytes()
    c = FrameCache(n * 10)
    assert c.put("a", img, generation=c.generation)
    assert c.bytes_used == n and len(c) == 1 and c.contains("a")
    big = _img(16, 16)
    assert c.put("a", big, generation=c.generation)
    assert c.bytes_used == big.sizeInBytes() and len(c) == 1
    assert c.get("a") is big
    assert c.get("missing") is None


def test_lru_eviction():
    n = _img().sizeInBytes()
    c = FrameCache(n * 3)
    for k in "abc":
        c.put(k, _img(), generation=0)
    c.get("a")
    c.put("d", _img(), generation=0)
    assert c.keys() == ["c", "a", "d"]
    assert c.bytes_used == n * 3


def test_oversized_not_retained():
    c = FrameCache(10)
    assert not c.put("a", _img(), generation=0)
    assert len(c) == 0 and c.bytes_used == 0


def test_oversized_replacement_drops_old():
    img = _img()
    c = FrameCache(img.sizeInBytes())
    c.put("a", img, generation=0)
    assert not c.put("a", _img(32, 32), generation=0)
    assert not c.contains("a") and c.bytes_used == 0


def test_stale_generation_and_clear():
    c = FrameCache(10**6)
    c.put("a", _img(), generation=0)
    g = c.generation
    c.clear()
    assert c.generation == g + 1 and len(c) == 0 and c.bytes_used == 0
    assert not c.put("b", _img(), generation=g)
    assert c.put("b", _img(), generation=c.generation)


def _fill(c, idxs):
    for i in idxs:
        c.put(i, _img(), generation=0, frame_index=i)


def test_playhead_forward_evicts_farthest_ahead():
    n = _img().sizeInBytes()
    c = FrameCache(n * 4)
    c.set_playhead(5, 1, (0, 9))
    _fill(c, [4, 5, 6, 7])
    c.put(8, _img(), generation=0, frame_index=8)
    # 4 is farthest forward (wraps: (4-5) % 10 = 9)
    assert not c.contains(4) and c.contains(5)


def test_playhead_backward():
    n = _img().sizeInBytes()
    c = FrameCache(n * 4)
    c.set_playhead(5, -1, (0, 9))
    _fill(c, [3, 4, 5, 6])
    c.put(2, _img(), generation=0, frame_index=2)
    # 6 is farthest in backward direction
    assert not c.contains(6) and c.contains(2)


def test_playhead_no_loop_and_unindexed_fallback():
    n = _img().sizeInBytes()
    c = FrameCache(n * 3)
    c.set_playhead(5, 1)
    _fill(c, [3, 6, 9])
    c.put(7, _img(), generation=0, frame_index=7)
    assert not c.contains(3)  # behind playhead goes first
    c.set_playhead(None)
    c.put(8, _img(), generation=0, frame_index=8)
    assert not c.contains(6)  # plain LRU again


def test_unindexed_entries_use_lru_under_playhead():
    n = _img().sizeInBytes()
    c = FrameCache(n * 2)
    c.set_playhead(0, 1, (0, 9))
    c.put("x", _img(), generation=0)
    c.put("y", _img(), generation=0)
    c.put("z", _img(), generation=0)
    assert c.keys() == ["y", "z"]
