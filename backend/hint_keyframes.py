"""Per-clip sparse hint keyframe sidecar (indices validated against source stems)."""
from __future__ import annotations

import json
import os
import tempfile
from collections.abc import Sequence

KEYFRAMES_FILENAME = ".corridorkey_hint_keyframes.json"
_VERSION = 1


class HintKeyframesError(ValueError):
    """Corrupt, stale or invalid hint keyframe state."""


def _sidecar(root_path: str) -> str:
    return os.path.join(root_path, KEYFRAMES_FILENAME)


def _is_int(value: object) -> bool:
    return isinstance(value, int) and not isinstance(value, bool)


def _validate_indices(indices: Sequence[int], stems: Sequence[str]) -> tuple[int, ...]:
    seen: set[int] = set()
    for idx in indices:
        if not _is_int(idx):
            raise HintKeyframesError(f"Keyframe index must be an int: {idx!r}")
        if idx < 0 or idx >= len(stems):
            raise HintKeyframesError(f"Keyframe index {idx} out of range 0..{len(stems) - 1}")
        if idx in seen:
            raise HintKeyframesError(f"Duplicate keyframe index {idx}")
        seen.add(idx)
    return tuple(sorted(seen))


def _parse(data: object, stems: Sequence[str]) -> tuple[int, ...]:
    if not isinstance(data, dict) or set(data) != {"version", "keyframes"}:
        raise HintKeyframesError("Invalid keyframe sidecar schema")
    if not _is_int(data["version"]) or data["version"] != _VERSION:
        raise HintKeyframesError(f"Unsupported keyframe sidecar version: {data['version']!r}")
    records = data["keyframes"]
    if not isinstance(records, list):
        raise HintKeyframesError("'keyframes' must be a list")
    indices: list[int] = []
    seen_stems: set[str] = set()
    for rec in records:
        if not isinstance(rec, dict) or set(rec) != {"index", "stem"}:
            raise HintKeyframesError(f"Invalid keyframe record: {rec!r}")
        idx, stem = rec["index"], rec["stem"]
        if not _is_int(idx) or not isinstance(stem, str):
            raise HintKeyframesError(f"Invalid keyframe record types: {rec!r}")
        if stem in seen_stems:
            raise HintKeyframesError(f"Duplicate keyframe stem {stem!r}")
        seen_stems.add(stem)
        _validate_indices([idx], stems)
        if stems[idx] != stem:
            raise HintKeyframesError(
                f"Stale keyframe {idx}: sidecar stem {stem!r} != source stem {stems[idx]!r}")
        indices.append(idx)
    result = _validate_indices(indices, stems)
    if indices != list(result):
        raise HintKeyframesError("Keyframes are not sorted by index")
    return result


def load_hint_keyframes(root_path: str, source_stems: Sequence[str]) -> tuple[int, ...]:
    """Return validated keyframe indices; () if no sidecar. Never rewrites."""
    path = _sidecar(root_path)
    if not os.path.lexists(path):
        return ()
    with open(path, "r", encoding="utf-8") as fh:
        raw = fh.read()
    try:
        data = json.loads(raw)
    except ValueError as exc:
        raise HintKeyframesError(f"Corrupt keyframe sidecar: {exc}") from exc
    return _parse(data, source_stems)


def clear_hint_keyframes(root_path: str) -> None:
    """Delete only the sidecar (a symlink is unlinked, never followed)."""
    path = _sidecar(root_path)
    if os.path.lexists(path):
        os.unlink(path)


def save_hint_keyframes(
    root_path: str, indices: Sequence[int], source_stems: Sequence[str],
) -> None:
    """Validate then atomically write the sidecar; empty indices clear it."""
    ordered = _validate_indices(indices, source_stems)
    if not ordered:
        clear_hint_keyframes(root_path)
        return
    payload = {
        "version": _VERSION,
        "keyframes": [{"index": i, "stem": source_stems[i]} for i in ordered],
    }
    fd, tmp = tempfile.mkstemp(prefix=KEYFRAMES_FILENAME + ".", suffix=".tmp", dir=root_path)
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(payload, fh, ensure_ascii=False, indent=2)
            fh.flush()
            os.fsync(fh.fileno())
        os.replace(tmp, _sidecar(root_path))
    except BaseException:
        try:
            os.unlink(tmp)
        except FileNotFoundError:
            pass
        raise
