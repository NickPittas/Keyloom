"""Round 3 T6: is an existing hint the render a queue would make, and has it changed since it was queued?

``check`` compares each requested frame's provenance record (``.corridorkey_hint.json`` ``ranges``, written by
``task_job.publish_hint_dir``) with the render about to be queued: kind, generation settings, source frames,
upstream masks and SAM prompt fingerprint. The Rust twin ``hints::check_hint`` is what the app runs; this one is the
test oracle for the same rules (nothing in production calls ``check``). ``identity`` /
``verify_identity`` freeze and recheck what Key reads (size + mtime per frame), for hints without records too.
Key-only settings never enter the comparison. Reads only; never writes or deletes.
"""
from __future__ import annotations

import os

COMPATIBLE, UNRECORDED, MISMATCH, MISSING = "compatible", "unrecorded", "mismatch", "missing"
_IMG = (".png", ".jpg", ".jpeg", ".tif", ".tiff", ".bmp", ".exr")


def _files(folder: str) -> dict:
    """stem -> filename for the image files directly inside ``folder`` (dotfiles ignored)."""
    try:
        # sorted: when a stem exists twice (f1.png, f1.exr) the last name wins, as in Rust `hints::hint_files`
        return {os.path.splitext(f)[0]: f for f in sorted(os.listdir(folder))
                if not f.startswith(".") and f.lower().endswith(_IMG)}
    except OSError:
        return {}


def stat_id(path: str) -> str:
    st = os.stat(path)
    return f"{st.st_size}:{st.st_mtime_ns}"


def identity(folder: str, stems) -> dict | None:
    """stem -> size:mtime_ns of those hint frames; None when any is missing or unreadable."""
    have, out = _files(folder), {}
    try:
        for s in stems:
            out[s] = stat_id(os.path.join(folder, have[s]))
    except (KeyError, OSError):
        return None
    return out


def verify_identity(folder: str, frozen: dict) -> str | None:
    """Why the hint frames no longer match what was frozen at queue time (None = unchanged)."""
    now = identity(folder, frozen)
    if now is None:
        return "a hint frame is missing or unreadable"
    changed = sorted(s for s in frozen if frozen[s] != now[s])
    return f"hint frames changed since queued (e.g. {changed[0]})" if changed else None


def _same(want, have) -> bool:
    """Every setting in ``want`` has the same value in ``have`` (``have`` may carry worker-filled defaults)."""
    if isinstance(want, dict):
        return isinstance(have, dict) and all(k in have and _same(v, have[k]) for k, v in want.items())
    if isinstance(want, bool) or isinstance(have, bool):
        return want is have
    if isinstance(want, (int, float)) and isinstance(have, (int, float)):
        return float(want) == float(have)
    return want == have


def _via_guide(r: dict, stem: str, want: dict, guide: dict) -> bool:
    """Rust ``hints::via_guide``: the producer writing the hint directly and the promotion of that producer's
    pre-matte (``prematte_hint``) are the same render. ``guide`` = ``certified_stems`` of the pre-matte folder."""
    g = guide.get(stem)
    if g is None:
        return False
    pm = "prematte_hint"

    def params(v):
        return {k: x for k, x in (v.get("params") or {}).items() if k != "source_map"}

    def src(v):
        return (v.get("source") or {}).get(stem)

    def cleanup_ok(w, h):
        return "mask_cleanup" in w and "mask_cleanup" in h and _same(w["mask_cleanup"], h["mask_cleanup"])

    rk, rp, gp = r.get("kind"), params(r), params(g)
    if want["kind"] == pm and rk != pm:
        w = (want.get("source") or {}).get(stem)
        return (rk == g.get("kind") and _same(gp, rp) and _same(rp, gp) and src(r) is not None and src(r) == src(g)
                and (w is None or w == src(r)) and cleanup_ok(want.get("params") or {}, rp))
    if rk == pm and want["kind"] != pm:
        wp = params(want)
        return (want["kind"] == g.get("kind") and _same(wp, gp) and src(g) is not None
                and src(g) == (want.get("source") or {}).get(stem)
                and (r.get("upstream") or {}).get(stem) == g["digests"].get(stem) and cleanup_ok(wp, rp))
    return False


def _why(r: dict, stem: str, want: dict, guide: dict | None = None) -> str | None:
    """None = this record proves ``stem`` was made as ``want`` says; else the first difference."""
    if r.get("kind") != want["kind"]:
        if guide and _via_guide(r, stem, want, guide):
            return None
        return f"made by {r.get('kind')}, not {want['kind']}"
    if not _same({k: v for k, v in (want.get("params") or {}).items() if k != "source_map"}, r.get("params") or {}):
        return "generation settings differ"
    for field, label in (("source", "source frame"), ("upstream", "pre-matte")):
        w = (want.get(field) or {}).get(stem)
        if w is not None and (r.get(field) or {}).get(stem) != w:
            return f"{label} {stem} differs from the one this was made from"
    fp = want.get("prompt_fp")
    if fp is not None and r.get("prompt_fp") != fp:
        return "prompts differ"
    return None


def check(folder: str, stems, want: dict) -> tuple:
    """(status, reason) for rendering ``want`` ({kind, params, source, upstream, prompt_fp, guide_folder}) into ``folder``.

    missing: a requested frame is absent. mismatch: a record proves it was made differently or the file changed.
    unrecorded: no record certifies a frame (legacy/imported); the caller lets the user decide.
    Frames made by a broader range still serve a subset (dense output does not depend on the Key range)."""
    from .service.task_job import certified_stems  # lazy: task_job imports key_job, which imports this module

    have = _files(folder)
    absent = [s for s in stems if s not in have]
    if absent:
        return MISSING, f"{len(absent)} frame(s) missing, e.g. {absent[0]}"
    certified = certified_stems(folder)
    guide = certified_stems(want["guide_folder"]) if want.get("guide_folder") else None
    unrecorded = []
    for s in stems:
        r = certified.get(s)
        if r is None:
            unrecorded.append(s)
            continue
        why = _why(r, s, want, guide)
        if why:
            return MISMATCH, why
    # A frame that has a digest but no longer matches it was edited or replaced after it was made.
    from .service.task_job import read_hint_ranges
    recorded = {s for r in read_hint_ranges(folder) if not r.get("legacy") for s in r["digests"]}
    edited = [s for s in unrecorded if s in recorded]
    if edited:
        return MISMATCH, f"frame {edited[0]} was changed after it was made"
    if unrecorded:
        return UNRECORDED, "Settings not recorded"
    return COMPATIBLE, "made with these settings from these frames"
