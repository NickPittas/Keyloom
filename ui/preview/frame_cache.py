"""Byte-bounded, thread-safe QImage cache for preview playback."""

from __future__ import annotations

import threading
from collections import OrderedDict
from collections.abc import Hashable

from PySide6.QtGui import QImage

PREVIEW_CACHE_BYTES = 512 * 1024 * 1024
LEGACY_CACHE_BYTES = 64 * 1024 * 1024


class FrameCache:
    """LRU cache of QImages bounded by ``QImage.sizeInBytes()``.

    While a playhead is set (playing), eviction removes the entry farthest
    ahead of the playhead in the playback direction (modulo the loop range),
    falling back to LRU for entries without a frame index.
    """

    def __init__(self, max_bytes: int) -> None:
        self._max_bytes = int(max_bytes)
        self._lock = threading.Lock()
        # key -> (image, nbytes, frame_index)
        self._items: OrderedDict[Hashable, tuple[QImage, int, int | None]] = OrderedDict()
        self._bytes = 0
        self._generation = 0
        self._playhead: int | None = None
        self._direction = 1
        self._loop: tuple[int, int] | None = None

    @property
    def max_bytes(self) -> int:
        return self._max_bytes

    @property
    def bytes_used(self) -> int:
        with self._lock:
            return self._bytes

    @property
    def generation(self) -> int:
        with self._lock:
            return self._generation

    def __len__(self) -> int:
        with self._lock:
            return len(self._items)

    def keys(self) -> list[Hashable]:
        with self._lock:
            return list(self._items.keys())

    def contains(self, key: Hashable) -> bool:
        with self._lock:
            return key in self._items

    def get(self, key: Hashable) -> QImage | None:
        with self._lock:
            item = self._items.get(key)
            if item is None:
                return None
            self._items.move_to_end(key)
            return item[0]

    def set_playhead(
        self,
        index: int | None,
        direction: int = 1,
        loop_range: tuple[int, int] | None = None,
    ) -> None:
        with self._lock:
            self._playhead = index
            self._direction = -1 if direction < 0 else 1
            self._loop = loop_range

    def clear(self) -> None:
        with self._lock:
            self._items.clear()
            self._bytes = 0
            self._generation += 1

    def put(
        self,
        key: Hashable,
        image: QImage,
        *,
        generation: int,
        frame_index: int | None = None,
    ) -> bool:
        nbytes = int(image.sizeInBytes())
        with self._lock:
            if generation != self._generation:
                return False
            old = self._items.pop(key, None)
            if old is not None:
                self._bytes -= old[1]
            if nbytes > self._max_bytes:
                return False
            self._items[key] = (image, nbytes, frame_index)
            self._bytes += nbytes
            while self._bytes > self._max_bytes and len(self._items) > 1:
                victim = self._pick_victim(key)
                _img, vbytes, _idx = self._items.pop(victim)
                self._bytes -= vbytes
            return True

    def _pick_victim(self, protect: Hashable) -> Hashable:
        """Choose the eviction victim (lock held; O(n) only on overflow)."""
        head = self._playhead
        if head is not None:
            best_key = None
            best_dist = -1
            for k, (_img, _n, idx) in self._items.items():
                if k == protect or idx is None:
                    continue
                d = self._distance(idx, head)
                if d > best_dist:
                    best_key, best_dist = k, d
            if best_key is not None:
                return best_key
        for k in self._items:  # LRU order, oldest first
            if k != protect:
                return k
        return protect

    def _distance(self, idx: int, head: int) -> int:
        d = (idx - head) * self._direction
        if self._loop is not None:
            lo, hi = self._loop
            n = hi - lo + 1
            if n > 0:
                if idx < lo or idx > hi:
                    return n * 2  # outside loop: farthest
                return d % n
        # no loop: frames behind the playhead are the farthest
        return d if d >= 0 else (1 << 40) - d
