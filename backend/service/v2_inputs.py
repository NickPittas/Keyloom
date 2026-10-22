"""Prepare CorridorKey 2.0 inputs: the engine pairs input frames and dense hints by ORDINAL position.

EZ orders frames naturally and supports TIFF/BMP/uppercase extensions, while the engine
(`corridorkey.video_io.frame_paths`) only globs lowercase exr/png/jpg/jpeg in lexical order.
When the two orderings agree the source directory is passed straight through; otherwise
numbered symlinks (or lossless PNG/EXR conversions) are staged under ``staging_dir``.
Staged dirs are named ``input``/``hint`` (not special to ``raw_clip_name``; callers use the
returned paths anyway). Source files are never modified.

Verified AlphaHint frame naming (the only patterns accepted):
  * ``<stem>.<ext>``            gvm_core/wrapper.py:310-318 (GVM writes ``<input_stem>.<ext>``)
  * ``<stem>_alphaHint_NNNN``   clip_manager.py:238 (legacy GVM rename, ordinal NNNN)
  * ``_alphaHint_NNNNN``        tests/conftest.py:66,130 (empty stem, ordinal)
"""
from __future__ import annotations

import logging
import numbers
import os
import re
import shutil
from dataclasses import dataclass
from pathlib import Path

import cv2
import numpy as np

from ..errors import CorridorKeyError
from ..frame_io import imread_unicode, imwrite_unicode, open_video

logger = logging.getLogger(__name__)

_RUNTIME_EXTS = {".exr", ".png", ".jpg", ".jpeg"}
_HINT_RE = re.compile(r"^(?P<prefix>.*)_alphahint_(?P<n>\d+)$", re.IGNORECASE)
_NO_HINT_MSG = "This clip has no AlphaHint. Generate one or turn off 'Use AlphaHint'."
_DEFAULT_FPS = 24.0
_RANGE_VIDEO_HINT_MSG = (
    "Range keying needs the AlphaHint as an image sequence, not a video: decoding a video "
    "hint would read frames outside In/Out. Extract the AlphaHint to frames or regenerate it "
    "for this range.")
_UNSET = object()  # "probe the input asset" (whole-clip behaviour); range mode always passes dims


def _dims(clip, want):
    return clip.input_asset.get_dimensions() if want is _UNSET else want


class V2InputError(CorridorKeyError):
    """Input/AlphaHint cannot be handed to the CorridorKey 2.0 engine."""


@dataclass
class V2PreparedInput:
    input_path: str            # video file or frame dir handed to run_inference
    source_stems: list[str]    # index -> original stem used for EZ output names
    frame_count: int
    fps: float
    staged_input: bool
    hint_path: str | None      # None when hints off
    initial_hint_path: str | None = None       # single-mark reference matte file
    initial_hint_index: int = 0                # absolute source index of that mark
    hint_frame_indices: tuple[int, ...] | None = None  # sparse marks (absolute, sorted)
    range_offset: int = 0      # frame_range start: absolute index = relative index + range_offset


# --------------------------------------------------------------------- helpers

def _runtime_listing(directory: str) -> list[str]:
    """Basenames in the order the engine's frame_paths() would list them."""
    from CorridorKeyV2.corridorkey.video_io import frame_paths
    return [os.path.basename(p) for p in frame_paths(directory)]


def _stem(name: str) -> str:
    return os.path.splitext(name)[0]


def _link_or_copy(src: str, dst: str) -> None:
    try:
        os.symlink(os.path.abspath(src), dst)
    except OSError:
        shutil.copy2(src, dst)


def _convert(src: str, dst_base: str) -> str:
    """Losslessly convert an engine-unreadable frame; returns the written path."""
    img = imread_unicode(src, cv2.IMREAD_UNCHANGED)
    if img is None:
        raise V2InputError(f"Could not read frame '{src}'.")
    if img.dtype in (np.uint8, np.uint16):
        out, data = dst_base + ".png", img
        params = []
    elif np.issubdtype(img.dtype, np.floating):
        out, data = dst_base + ".exr", img.astype(np.float32)
        params = [cv2.IMWRITE_EXR_TYPE, cv2.IMWRITE_EXR_TYPE_FLOAT]
    else:
        raise V2InputError(f"Unsupported pixel format {img.dtype} in '{src}'.")
    if not imwrite_unicode(out, data, params):
        raise V2InputError(f"Could not write staged frame '{out}'.")
    return out


def _stage_one(src: str, dst_dir: str, index: int) -> str:
    base = os.path.join(dst_dir, f"{index:06d}")
    ext = os.path.splitext(src)[1].lower()
    if ext in _RUNTIME_EXTS:
        dst = base + ext
        _link_or_copy(src, dst)
        return dst
    return _convert(src, base)


def _stage_files(paths: list[str], dst_dir: str, check_cancel) -> None:
    """Stage ``paths`` as ``{i:06d}.ext`` in ``dst_dir`` (ordinal order)."""
    os.makedirs(dst_dir, exist_ok=True)
    kinds: set[bool] = set()
    for i, src in enumerate(paths):
        if check_cancel is not None:
            check_cancel()
        kinds.add(_stage_one(src, dst_dir, i).lower().endswith(".exr"))
        if len(kinds) > 1:
            raise V2InputError(
                "This clip mixes EXR and non-EXR frames; colour interpretation would "
                "differ per frame. Convert the clip to a single format first.")


def _check_duplicate_stems(names: list[str], what: str) -> None:
    seen: dict[str, str] = {}
    for n in names:
        key = _stem(n).lower()
        if key in seen:
            raise V2InputError(
                f"{what} has duplicate frame names '{seen[key]}' and '{n}' "
                "(same name, different format). Remove one of them.")
        seen[key] = n


def _video_fps(clip) -> float:
    from ..ffmpeg_tools.metadata import read_video_metadata
    meta = read_video_metadata(clip.root_path) or {}
    try:
        fps = float(meta.get("fps") or 0)
    except (TypeError, ValueError):
        fps = 0.0
    if fps > 0:
        return fps
    cap = open_video(clip.input_asset.path)
    try:
        fps = float(cap.get(cv2.CAP_PROP_FPS) or 0) if cap.isOpened() else 0.0
    finally:
        cap.release()
    return fps if fps > 0 else _DEFAULT_FPS


def _video_frame_count(path: str) -> int:
    cap = open_video(path)
    try:
        return int(cap.get(cv2.CAP_PROP_FRAME_COUNT)) if cap.isOpened() else 0
    finally:
        cap.release()


# ------------------------------------------------------------------ public API

def validate_dense_hint_alignment(clip, source_stems: list[str]) -> list[str]:
    """Return hint filenames ordered to match ``source_stems`` or raise V2InputError."""
    return _validate_dense(clip, source_stems)[0]


def _validate_dense(clip, source_stems: list[str]) -> tuple[list[str], bool]:
    """(ordered hint names, True when any hint needs resizing to the input size)."""
    asset = clip.alpha_asset
    if asset is None:
        raise V2InputError(_NO_HINT_MSG)
    files = asset.get_frame_files()
    _check_duplicate_stems(files, "AlphaHint")
    ordered = _match_hints(files, source_stems)
    want = clip.input_asset.get_dimensions()
    resize = False
    for f in ordered:
        resize |= _check_hint_dims(want, os.path.join(asset.path, f))
    return ordered, resize


def _hint_by_stem(files: list[str]) -> dict[str, list[str]]:
    by_stem: dict[str, list[str]] = {}
    for f in files:
        m = _HINT_RE.match(_stem(f))
        by_stem.setdefault((m.group("prefix") if m else _stem(f)).lower(), []).append(f)
        if m:  # also reachable by its exact (suffixed) stem
            by_stem.setdefault(_stem(f).lower(), []).append(f)
    return by_stem


def _match_hints(files: list[str], source_stems: list[str]) -> list[str]:
    total = len(source_stems)
    by_stem = _hint_by_stem(files)
    ordered = [_pick(by_stem.get(s.lower(), [])) for s in source_stems]
    if all(o is None for o in ordered):
        ordered = _match_by_ordinal(files, total)
    found = [o for o in ordered if o is not None]
    if len(found) != total:
        raise V2InputError(
            f"AlphaHint covers {len(found)} of {total} frames — regenerate the AlphaHint "
            "for the whole clip or turn off 'Use AlphaHint'.")
    if len(set(found)) != len(found) or len(files) != total:
        raise V2InputError(
            f"AlphaHint has {len(files)} files for {total} frames with ambiguous or extra "
            "names — regenerate the AlphaHint or turn off 'Use AlphaHint'.")
    return found


def _pick(cands: list[str]) -> str | None:
    uniq = list(dict.fromkeys(cands))
    if len(uniq) > 1:
        raise V2InputError(f"Ambiguous AlphaHint files for one frame: {', '.join(uniq)}.")
    return uniq[0] if uniq else None


def _match_by_ordinal(files: list[str], total: int, offset: int = 0) -> list[str | None]:
    """Legacy ``<prefix>_alphaHint_<n>`` names whose prefix is not a frame stem: n == index."""
    parsed = {}
    for f in files:
        m = _HINT_RE.match(_stem(f))
        if not m:
            return [None] * total
        parsed[int(m.group("n"))] = f
    if len(parsed) != len(files):
        return [None] * total
    return [parsed.get(offset + i) for i in range(total)]


def _image_size(path: str) -> tuple[int, int] | None:
    """(width, height) from the file header when possible (PIL), else a full read."""
    try:
        from PIL import Image
        with Image.open(path) as im:
            return int(im.size[0]), int(im.size[1])
    except Exception:  # EXR / unsupported by PIL -> decode with cv2
        img = imread_unicode(path, cv2.IMREAD_UNCHANGED)
        return None if img is None else (int(img.shape[1]), int(img.shape[0]))


_ASPECT_TOL = 0.005  # relative; covers truncation rounding (e.g. GVM 1820x1024 vs 1920x1080)


def _check_geometry(got, want, label: str) -> bool:
    """True when ``got`` differs from ``want`` but is aspect-compatible (needs resize).

    Raises V2InputError on a clearly different aspect ratio. Unknown ``want`` -> False.
    """
    if want is None or tuple(want) == tuple(got):
        return False
    gw, gh = got
    ww, wh = want
    if gh > 0 and wh > 0 and ww > 0 and abs((gw / gh) / (ww / wh) - 1.0) <= _ASPECT_TOL:
        return True
    raise V2InputError(
        f"AlphaHint {label} is {gw}x{gh} but the input is {ww}x{wh} — the aspect ratio "
        "differs. Provide an AlphaHint with the clip's aspect ratio or set the hint mode "
        "to None.")


def _check_hint_dims(want, hint_path: str) -> bool:
    """Validate one hint header; True when it is a compatible different size."""
    got = _image_size(hint_path)
    if got is None:
        raise V2InputError(f"Could not read AlphaHint frame '{hint_path}'.")
    return _check_geometry(got, want, f"frame '{os.path.basename(hint_path)}'")


def _prepare_hint(clip, input_stems: list[str], staging: Path, check_cancel) -> str:
    asset = clip.alpha_asset
    if asset is None:
        raise V2InputError(_NO_HINT_MSG)
    if asset.asset_type == "video":
        got, want = _video_frame_count(asset.path), len(input_stems)
        if got != want:
            raise V2InputError(
                f"AlphaHint video has {got} frames but the input has {want} — regenerate "
                "the AlphaHint for the whole clip or turn off 'Use AlphaHint'.")
        return asset.path
    ordered, resize = _validate_dense(clip, input_stems)
    dst = staging / "hint"
    if resize:  # temporary source-sized PNG copies; originals are never touched
        want = clip.input_asset.get_dimensions()
        shutil.rmtree(dst, ignore_errors=True)
        os.makedirs(dst, exist_ok=True)
        for i, f in enumerate(ordered):
            if check_cancel is not None:
                check_cancel()
            src = os.path.join(asset.path, f)
            _write_matte(imread_unicode(src, cv2.IMREAD_UNCHANGED), want,
                         f"frame '{f}'", str(dst / f"{i:06d}.png"))
        return str(dst)
    if ordered == _runtime_listing(asset.path):
        return asset.path
    _stage_files([os.path.join(asset.path, f) for f in ordered], str(dst), check_cancel)
    return str(dst)


def _resolve_selected(files: list[str], source_stems: list[str],
                      indices: list[int], offset: int = 0) -> dict[int, str]:
    """Selected-frame lookup with the dense policies (exact stem, verified legacy ordinal).

    Only the selected frames must resolve; unselected files are ignored.
    """
    by_stem = _hint_by_stem(files)
    exact_any = any(s.lower() in by_stem for s in source_stems)
    ordinal: dict[int, list[str]] = {}
    if not exact_any:  # sparse: ignore non-matching extras; only selected duplicates are ambiguous
        for f in files:
            m = _HINT_RE.match(_stem(f))
            if m:
                ordinal.setdefault(int(m.group("n")), []).append(f)
    out: dict[int, str] = {}
    for k in indices:
        stem = source_stems[k]
        try:
            found = _pick(by_stem.get(stem.lower(), [])) if exact_any else _pick(ordinal.get(k + offset, []))
        except V2InputError as exc:
            raise V2InputError(f"AlphaHint for keyframe {k} (frame '{stem}'): {exc}") from None
        if found is None:
            raise V2InputError(
                f"No AlphaHint matte found for keyframe {k} (frame '{stem}') — generate or "
                "import a matte for that frame, or unmark it.")
        out[k] = found
    return out


def _matte_plane(img: np.ndarray, label: str) -> np.ndarray:
    """Single-channel matte (engine hint semantics) as uint8/uint16 for PNG staging."""
    if img.ndim == 3:
        img = img[:, :, 3] if img.shape[2] == 4 else img[:, :, 0]
    if img.dtype in (np.uint8, np.uint16):
        return np.ascontiguousarray(img)
    if np.issubdtype(img.dtype, np.floating):
        f = np.nan_to_num(img.astype(np.float32), nan=0.0, posinf=1.0, neginf=0.0)
        return np.rint(np.clip(f, 0.0, 1.0) * 65535.0).astype(np.uint16)
    raise V2InputError(f"Unsupported AlphaHint pixel format {img.dtype} in {label}.")


def _write_matte(img, want, label: str, dst: str) -> None:
    if img is None:
        raise V2InputError(f"Could not read AlphaHint {label}.")
    got = (int(img.shape[1]), int(img.shape[0]))
    plane = _matte_plane(img, label)
    if _check_geometry(got, want, label):
        plane = cv2.resize(plane, (int(want[0]), int(want[1])), interpolation=cv2.INTER_LINEAR)
    if not imwrite_unicode(dst, plane):
        raise V2InputError(f"Could not write staged AlphaHint '{dst}'.")


def _prepare_keyframe_hint(clip, stems: list[str], staging: Path, check_cancel,
                           rng=None, want=_UNSET):
    marks, err = clip.hint_keyframes, clip.hint_keyframes_error
    if err:
        raise V2InputError(f"Keyframes are unusable: {err}")
    if not marks:
        raise V2InputError("Keyframes mode needs at least one marked frame.")
    raw = list(marks)
    for k in raw:
        if isinstance(k, bool) or not isinstance(k, numbers.Integral):
            raise V2InputError(f"Keyframes contain non-integer frame index {k!r}.")
    raw = [int(k) for k in raw]
    if len(set(raw)) != len(raw):
        dup = sorted({k for k in raw if raw.count(k) > 1})
        raise V2InputError(f"Keyframes contain duplicate frame indices {dup}.")
    off = 0
    if rng is not None:  # marks outside In/Out are ignored (they stay in the sidecar)
        off = rng[0]
        raw = [k - off for k in raw if rng[0] <= k <= rng[1]]
        if not raw:
            raise V2InputError("Needs keyframe marks (none inside In/Out)")
    idx = sorted(raw)
    if idx[0] < 0 or idx[-1] >= len(stems):
        bad = idx[0] if idx[0] < 0 else idx[-1]
        raise V2InputError(
            f"Keyframe {bad} is outside the clip (0-{len(stems) - 1}) — unmark it.")
    asset = clip.alpha_asset
    if asset is None:
        raise V2InputError(_NO_HINT_MSG)
    if rng is not None and asset.asset_type == "video":
        raise V2InputError(_RANGE_VIDEO_HINT_MSG)
    want = _dims(clip, want)
    dst = staging / "hint"
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst, exist_ok=True)
    paths = {k: str(dst / f"{k:06d}.png") for k in idx}
    if asset.asset_type == "video":
        got = _video_frame_count(asset.path)
        if got <= idx[-1]:
            raise V2InputError(
                f"AlphaHint video has {got} frames but keyframe {idx[-1]} "
                f"(frame '{stems[idx[-1]]}') is needed.")
        cap = open_video(asset.path)
        try:
            if not cap.isOpened():
                raise V2InputError(f"Could not open AlphaHint video '{asset.path}'.")
            for k in idx:
                if check_cancel is not None:
                    check_cancel()
                cap.set(cv2.CAP_PROP_POS_FRAMES, k)
                ok, frame = cap.read()
                _write_matte(frame if ok else None, want,
                             f"video frame {k} (frame '{stems[k]}')", paths[k])
        finally:
            cap.release()
    else:
        chosen = _resolve_selected(asset.get_frame_files(), stems, idx, off)
        for k in idx:
            if check_cancel is not None:
                check_cancel()
            src = os.path.join(asset.path, chosen[k])
            _write_matte(imread_unicode(src, cv2.IMREAD_UNCHANGED), want,
                         f"frame '{chosen[k]}' (keyframe {k})", paths[k])
    if len(idx) == 1:
        return V2PreparedInput("", [], 0, 0.0, False, None, paths[idx[0]], idx[0], None)
    return V2PreparedInput("", [], 0, 0.0, False, str(dst), None, 0, tuple(idx))


def _prepare_sequence(clip, staging: Path, check_cancel) -> tuple[str, list[str], bool]:
    asset = clip.input_asset
    ez = asset.get_frame_files()
    if not ez:
        raise V2InputError("The input image sequence has no frames.")
    _check_duplicate_stems(ez, "Input")
    stems = [_stem(f) for f in ez]
    if _runtime_listing(asset.path) == ez:
        return asset.path, stems, False
    dst = staging / "input"
    _stage_files([os.path.join(asset.path, f) for f in ez], str(dst), check_cancel)
    return str(dst), stems, True


def _prepare_hint_range(clip, stems: list[str], offset: int, want, staging: Path,
                        check_cancel) -> str:
    """Dense hint for frames ``offset..offset+len(stems)-1`` only, staged as ``{i:06d}``."""
    asset = clip.alpha_asset
    if asset is None:
        raise V2InputError(_NO_HINT_MSG)
    if asset.asset_type == "video":
        raise V2InputError(_RANGE_VIDEO_HINT_MSG)
    dst = staging / "hint"
    shutil.rmtree(dst, ignore_errors=True)
    os.makedirs(dst, exist_ok=True)
    files = asset.get_frame_files()
    _check_duplicate_stems(files, "AlphaHint")
    by_stem = _hint_by_stem(files)
    ordered = [_pick(by_stem.get(s.lower(), [])) for s in stems]
    if all(o is None for o in ordered):
        ordered = _match_by_ordinal(files, len(stems), offset)
    missing = [s for s, o in zip(stems, ordered) if o is None]
    if missing:
        raise V2InputError(
            f"AlphaHint is missing {len(missing)} of {len(stems)} frames in the range "
            f"(e.g. {', '.join(missing[:5])}) — regenerate the AlphaHint for this range.")
    if len(set(ordered)) != len(ordered):
        raise V2InputError("AlphaHint has ambiguous names for frames in the range.")
    resize = False
    for f in ordered:  # only in-range hint headers are read
        resize |= _check_hint_dims(want, os.path.join(asset.path, f))
    if resize:
        for i, f in enumerate(ordered):
            if check_cancel is not None:
                check_cancel()
            _write_matte(imread_unicode(os.path.join(asset.path, f), cv2.IMREAD_UNCHANGED),
                         want, f"frame '{f}'", str(dst / f"{i:06d}.png"))
    else:
        _stage_files([os.path.join(asset.path, f) for f in ordered], str(dst), check_cancel)
    return str(dst)


def range_dimensions(clip, frame_range: tuple[int, int]) -> tuple[int, int]:
    """(width, height) from the FIRST FRAME OF THE RANGE; never touches out-of-range frames."""
    asset = clip.input_asset
    files = asset.get_frame_files() if asset is not None and asset.asset_type == "sequence" else []
    a, b = frame_range
    if not 0 <= a <= b < len(files):
        raise V2InputError(f"Range {a}-{b} is outside the clip (0-{len(files) - 1}).")
    size = _image_size(os.path.join(asset.path, files[a]))
    if size is None:
        raise V2InputError(f"Could not read frame '{files[a]}'.")
    return size


def _prepare_range(clip, mode: str, staging: Path, check_cancel, rng) -> V2PreparedInput:
    asset = clip.input_asset
    if asset.asset_type != "sequence":
        raise V2InputError("Range keying needs an image sequence; re-import or extract this clip")
    if mode != "none" and clip.alpha_asset is not None and clip.alpha_asset.asset_type == "video":
        raise V2InputError(_RANGE_VIDEO_HINT_MSG)  # before any file is read or staged
    a, b = rng
    ez = asset.get_frame_files()
    if not ez:
        raise V2InputError("The input image sequence has no frames.")
    _check_duplicate_stems(ez, "Input")
    want = range_dimensions(clip, rng)
    files = ez[a:b + 1]
    stems = [_stem(f) for f in files]
    dst = staging / "input"
    _stage_files([os.path.join(asset.path, f) for f in files], str(dst), check_cancel)  # always
    common = (str(dst), stems, len(stems), _DEFAULT_FPS, True)
    if mode == "keyframes":
        kf = _prepare_keyframe_hint(clip, stems, staging, check_cancel, rng, want)
        return V2PreparedInput(*common, kf.hint_path, kf.initial_hint_path,
                               kf.initial_hint_index, kf.hint_frame_indices, a)
    hint = _prepare_hint_range(clip, stems, a, want, staging, check_cancel) if mode == "full" else None
    return V2PreparedInput(*common, hint, range_offset=a)


def prepare_v2_inputs(clip, *, use_alpha_hint: bool, staging_dir: str | Path,
                      check_cancel=None, hint_mode: str | None = None,
                      frame_range: tuple[int, int] | None = None) -> V2PreparedInput:
    """Resolve input (and optional dense/sparse hint) paths for ``run_inference``.

    ``hint_mode`` (none/full/keyframes) wins when given; otherwise ``use_alpha_hint``.
    ``frame_range=(a, b)`` (inclusive, absolute) stages and exposes ONLY those frames; hint
    frames are matched by stem and keyframes are filtered/rebased to the range.
    """
    mode = hint_mode if hint_mode is not None else ("full" if use_alpha_hint else "none")
    if mode not in ("none", "full", "keyframes"):
        raise V2InputError(f"Unknown hint mode '{mode}'.")
    asset = clip.input_asset
    if asset is None:
        raise V2InputError("This clip has no input media.")
    staging = Path(staging_dir)
    created = [staging / "input", staging / "hint"]
    pre_existing = [p.exists() for p in created]
    try:
        if frame_range is not None:
            return _prepare_range(clip, mode, staging, check_cancel, tuple(frame_range))
        if asset.asset_type == "video":
            count = asset.frame_count or _video_frame_count(asset.path)
            path, stems, staged = asset.path, [f"{i:05d}" for i in range(count)], False
            fps = _video_fps(clip)
        else:
            path, stems, staged = _prepare_sequence(clip, staging, check_cancel)
            fps = _DEFAULT_FPS
        if mode == "keyframes":
            kf = _prepare_keyframe_hint(clip, stems, staging, check_cancel)
            return V2PreparedInput(path, stems, len(stems), fps, staged, kf.hint_path,
                                   kf.initial_hint_path, kf.initial_hint_index,
                                   kf.hint_frame_indices)
        hint = _prepare_hint(clip, stems, staging, check_cancel) if mode == "full" else None
        return V2PreparedInput(path, stems, len(stems), fps, staged, hint)
    except BaseException:
        for p, existed in zip(created, pre_existing):
            if not existed:
                shutil.rmtree(p, ignore_errors=True)
        raise
