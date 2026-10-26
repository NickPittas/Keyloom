"""Import helper for the native UI: reuses backend project creation + FFmpeg extraction.

    ck_import.py [--dry-run] new --name N (--data-dir D | --parent-dir D) [--video=F]... [--folder=DIR]... [--sequence=DIR]...
    ck_import.py probe [--video=F]... [--folder=DIR]... [--sequence=DIR]...
    ck_import.py [--dry-run] add --project P [--video=F]... [--folder=DIR]... [--sequence=DIR]...
    ck_import.py extract --clip-root C

stdout is JSON lines only (logs go to stderr). A stdin line `cancel`, or stdin closing, cancels extraction.
--data-dir D creates D/Projects/<stamp>_<name>; --parent-dir D creates <stamp>_<name> directly inside D (claimed with an exclusive mkdir: an existing folder is never reused).
Exactly one of them, explicitly non-empty: there is no default location. `probe` plans the sources with no
destination at all (always a dry run) for a drop that has not chosen where to save yet.
Not covered: two processes adding to the SAME existing project at once (project.json is read-modified-written
without a lock, so the last writer wins); new projects are exclusive. Everything that can fail before a write (FFmpeg/OpenEXR present, probe, folder-name exhaustion, corrupt
project.json) is checked first; the plan is the same code path for --dry-run and the real run.
"""
import argparse
import json
import os
import re
import sys
import threading
from datetime import datetime

REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
sys.path.insert(0, REPO)
os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")

_OUT = sys.stdout
sys.stdout = sys.stderr  # stray prints must never corrupt the JSON protocol

from backend import project as P  # noqa: E402
from backend.project_media import add_sequences_to_project  # noqa: E402
from backend.ffmpeg_tools import (  # noqa: E402
    build_video_metadata, extract_frames, probe_video, require_ffmpeg_install, write_video_metadata,
)

SEQ_EXTS = {".png", ".exr"}  # what the native viewer can decode
cancel = threading.Event()


def emit(**kw):
    _OUT.write(json.dumps(kw) + "\n")
    _OUT.flush()


def fatal(msg):
    emit(event="error", message=msg)
    sys.exit(1)


def watch_stdin():
    def run():
        for line in sys.stdin:
            if line.strip() == "cancel":
                break
        cancel.set()  # `cancel` line or EOF (UI gone)

    threading.Thread(target=run, daemon=True).start()


def free_name(base, taken):
    """Same `_2.._99` scheme as backend.project, decided BEFORE anything is created."""
    if base and base not in taken:
        return base
    for i in range(2, 100):
        if base and f"{base}_{i}" not in taken:
            return f"{base}_{i}"
    return None


def project_candidates(name):
    """`<yymmdd_hhmmss>_<name>` then `_2.._99`, the backend's naming scheme."""
    stem = re.sub(r"_+", "_", re.sub(r"[^\w\-]", "_", name.strip())).strip("_")[:60]
    base = f"{datetime.now():%y%m%d_%H%M%S}_{stem}"
    return [base] + [f"{base}_{i}" for i in range(2, 100)]


def project_folder_problem(root, name):
    """Plan-time check only (the real claim is `claim_project`, which cannot race)."""
    names = project_candidates(name)
    if all(os.path.exists(os.path.join(root, n)) for n in names):
        return f"No free project folder name for {names[0]} in {root}"
    return None


def claim_project(root, name):
    """Create the new project folder with an exclusive mkdir: a folder that exists, even one another process
    made a moment ago, is never reused or written into. Then v2 layout + project.json (ours alone)."""
    os.makedirs(root, exist_ok=True)
    for n in project_candidates(name):
        d = os.path.join(root, n)
        try:
            os.mkdir(d)
        except FileExistsError:
            continue
        try:
            os.mkdir(os.path.join(d, "clips"))
            P.write_project_json(d, {"version": 2, "created": datetime.now().isoformat(),
                                     "display_name": name.strip(), "clips": []})
        except OSError as e:
            fatal(f"Could not set up the new project: {e} (partial project left at {d})")
        return d
    fatal(f"No free project folder name in {root}")


def seq_problem(folder):
    files = [f for f in os.listdir(folder) if os.path.isfile(os.path.join(folder, f)) and P.is_image_file(f)]
    bad = sorted({os.path.splitext(f)[1].lower() for f in files} - SEQ_EXTS)
    if bad:
        return f"Unsupported frame type {', '.join(bad)}: this build shows PNG and EXR only"
    dup = P.validate_sequence_stems(folder)
    if dup:
        return f"Same frame number in two files (e.g. {dup[0]}.png and {dup[0]}.exr)"
    return None


def collect(a):
    """Expand --video/--folder/--sequence into [(kind, path)] following the Qt Import-folder rules."""
    norm = os.path.abspath  # one spelling per path: a trailing slash must not change basename()/clip names
    out = [("video", norm(v)) for v in a.video]
    seqs = [("sequence", norm(s)) for s in a.sequence]
    for d in map(norm, a.folder):
        if not os.path.isdir(d):
            out.append(("missing", d))
            continue
        vids = P.filter_companion_hints(
            [os.path.join(d, f) for f in sorted(os.listdir(d)) if P.is_video_file(f)
             and os.path.isfile(os.path.join(d, f))])
        out += [("video", v) for v in vids]
        if P.folder_has_image_sequence(d):
            seqs.append(("sequence", d))
        for sub in sorted(os.listdir(d)):
            sp = os.path.join(d, sub)
            if (os.path.isdir(sp) and not sub.startswith((".", "_")) and P.folder_has_image_sequence(sp)
                    and not any(k in sub.lower() for k in P._HINT_KEYWORDS)):
                seqs.append(("sequence", sp))
    return out + seqs  # videos first, then sequences: the order the backend creates them


def make_plan(a):
    """Returns (items, clips_dir_or_None). Raises SystemExit via fatal() on whole-import problems."""
    add = a.cmd == "add"
    if add:
        proj = a.project
        if not proj.strip() or not os.path.isdir(proj):
            fatal(f"Not a folder: {proj}")
        if not P.is_v2_project(proj):
            fatal("This is a v1 project (no clips/ folder); adding clips would change its layout. Import into a new project instead.")
        meta = P.read_project_json(proj)
        if not isinstance(meta, dict):
            fatal("project.json is missing or unreadable; refusing to add clips to it. Fix or restore it first.")
        lists = (meta.get("clips", []), meta.get("removed_clips", []))
        if not all(isinstance(v, list) and all(isinstance(x, str) for x in v) for v in lists):
            fatal("project.json has a malformed clips list (not a list of names); refusing to rewrite it. Fix or restore it first.")
        if meta.get("version") != 2:
            fatal(f"Unsupported project version {meta.get('version')!r}: only version 2 projects can be added to.")
        clips_dir = os.path.join(proj, "clips")
        taken = set(os.listdir(clips_dir))  # incl. hidden/removed folders: never reuse a name on disk
    else:
        clips_dir, taken = None, set()
    items, seen = [], set()
    for kind, path in collect(a):
        it = {"source": path, "kind": kind, "status": "ok", "clip": None, "reason": None}
        items.append(it)
        key = os.path.normcase(os.path.abspath(path))
        if kind == "missing" or not os.path.exists(path):
            it.update(status="skip", reason="Not found")
        elif (kind == "video") == os.path.isdir(path):
            it.update(status="skip", reason="Expected a movie file, got a folder" if kind == "video" else "Expected a folder")
        elif key in seen:
            it.update(status="skip", reason="Listed twice")
        elif add and (dup := P.find_clip_by_source(a.project, path)):
            it.update(status="skip", reason=f"Already in this project as '{dup}'")
        elif kind == "video":
            if not P.is_video_file(path):
                it.update(status="skip", reason="Not a video file")
            else:
                try:
                    info = probe_video(path)
                    it["info"] = {k: info[k] for k in ("fps", "width", "height", "frame_count", "codec")}
                except Exception as e:
                    it.update(status="skip", reason=f"Cannot read video: {e}")
        else:
            if not P.folder_has_image_sequence(path):
                it.update(status="skip", reason="No image files")
            elif (why := seq_problem(path)):
                it.update(status="skip", reason=why)
            else:
                it["info"] = {"frame_count": P.count_sequence_frames(path)}
        seen.add(key)
    ok_videos = [i for i in items if i["status"] == "ok" and i["kind"] == "video"]
    if ok_videos:
        try:  # before any copy: FFmpeg + the OpenEXR/OpenCV recompress stack
            require_ffmpeg_install(require_probe=True)
            import cv2, numpy, OpenEXR, Imath  # noqa: F401,E401
        except Exception as e:
            fatal(f"Movie import is unavailable: {e}")
    for it in items:
        if it["status"] != "ok":
            continue
        base = P.sanitize_stem(os.path.basename(it["source"])) if it["kind"] == "video" else \
            P.sanitize_stem(os.path.basename(it["source"].rstrip("/\\")))
        name = free_name(base, taken)
        if name is None:
            it.update(status="skip", reason="No usable clip folder name (empty, or 98 clips already share it)")
            continue
        taken.add(name)
        it["clip"] = name
        if name != base:
            it["renamed_from"] = base
    return items, clips_dir


def extract_clip(clip_root):
    """Source/ video -> Frames/ (via a hidden temp dir so a half-extracted clip never looks playable)."""
    src = next((os.path.join(clip_root, "Source", f) for f in sorted(os.listdir(os.path.join(clip_root, "Source")))
                if P.is_video_file(f)), None)
    if not src:
        raise RuntimeError("No source video in Source/")
    frames = os.path.join(clip_root, "Frames")
    if os.path.isdir(frames) and os.listdir(frames):
        raise RuntimeError("Frames/ already has files; not touching it")
    info = probe_video(src)
    name = os.path.basename(clip_root)
    tmp = os.path.join(clip_root, ".frames_importing")
    total = info.get("frame_count", 0)
    n = extract_frames(
        src, tmp,
        on_progress=lambda d, t: emit(event="progress", clip=name, phase="extract", done=d, total=t),
        on_recompress_progress=lambda d, t: emit(event="progress", clip=name, phase="compress", done=d, total=t),
        cancel_event=cancel, total_frames=total)
    if cancel.is_set():
        return None
    if n <= 0 or not os.path.isfile(os.path.join(tmp, ".dwab_done")):
        raise RuntimeError("Extraction did not complete (no frames or compression unfinished)")
    write_video_metadata(clip_root, build_video_metadata(src, info, n))  # before the rename: fps/colour first
    if os.path.isdir(frames):
        os.rmdir(frames)
    os.replace(tmp, frames)
    return n


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dry-run", action="store_true")
    sub = ap.add_subparsers(dest="cmd", required=True)
    for c in ("new", "add", "probe"):
        s = sub.add_parser(c)
        s.add_argument("--video", action="append", default=[])
        s.add_argument("--folder", action="append", default=[])
        s.add_argument("--sequence", action="append", default=[])
        if c == "new":
            s.add_argument("--name", required=True)
            where = s.add_mutually_exclusive_group(required=True)
            where.add_argument("--data-dir")
            where.add_argument("--parent-dir")
        elif c == "add":
            s.add_argument("--project", required=True)
    sub.add_parser("extract").add_argument("--clip-root", required=True)
    a = ap.parse_args()
    if a.cmd == "probe":
        a.dry_run = True  # no destination exists, so nothing may ever be written
    if a.cmd == "new":  # before any other work: an empty value must not fall back to a default location
        if not (a.data_dir if a.parent_dir is None else a.parent_dir).strip():
            fatal("Destination folder is empty; choose a folder explicitly")
        if not a.name.strip():
            fatal("Project name is empty")

    if not a.dry_run:
        watch_stdin()  # before planning, so a cancel sent at spawn is seen before the first write
    if a.cmd == "extract":
        try:
            require_ffmpeg_install(require_probe=True)
            n = extract_clip(a.clip_root)
        except Exception as e:
            emit(event="clip_error", clip=os.path.basename(a.clip_root), message=str(e))
            return emit(event="finished")
        emit(event="cancelled") if n is None else emit(event="clip_done", clip=os.path.basename(a.clip_root), frames=n)
        return emit(event="finished")

    if a.cmd == "new":
        if a.parent_dir:
            if os.path.exists(a.parent_dir) and not os.path.isdir(a.parent_dir):
                fatal(f"Not a folder: {a.parent_dir}")
            a.root = os.path.abspath(a.parent_dir)
        else:
            a.root = os.path.join(os.path.abspath(a.data_dir), "Projects")
        if (why := project_folder_problem(a.root, a.name)):
            fatal(why)
    items, clips_dir = make_plan(a)
    oks = [i for i in items if i["status"] == "ok"]
    emit(event="plan", items=items)
    if a.dry_run:
        return
    if not oks:
        fatal("Nothing to import")
    emit(event="stage", message="Creating clips and copying source movies")
    # One item at a time, in plan order (so the backend's dedup gives the planned names), checking cancel
    # before each copy. An already-copied movie stays; project.json is rewritten atomically after each.
    proj = None if a.cmd == "new" else a.project
    made = []
    fresh = None  # a project folder THIS run created: named in errors, never deleted
    for it in oks:
        if cancel.is_set():
            break
        v = [it["source"]] if it["kind"] == "video" else []
        q = [it["source"]] if it["kind"] == "sequence" else []
        if proj is None:
            proj = claim_project(a.root, a.name)
            fresh = proj
        try:
            if v:
                P.add_clips_to_project(proj, v, copy_source=True)
            else:
                add_sequences_to_project(proj, q, copy_source=False)
        except Exception as e:  # noqa: BLE001
            fatal(f"{type(e).__name__}: {e}" + (f" (partial project kept at {fresh})" if fresh else ""))
        made.append(it)
    if not made:
        return emit(event="cancelled")
    oks = made
    clips = []
    for it in oks:  # trust but verify: the folder we planned really is this source
        root = os.path.join(proj, "clips", it["clip"])
        meta = P.read_clip_json(root) or {}
        if os.path.normcase(meta.get("source", {}).get("original_path", "")) != os.path.normcase(os.path.abspath(it["source"])):
            fatal(f"Clip folder {it['clip']} does not match its source; stopping before extraction"
                  + (f" (partial project kept at {fresh})" if fresh else ""))
        clips.append({"name": it["clip"], "root": root, "kind": it["kind"]})
    emit(event="created", project=proj, clips=clips)
    for c in clips:
        if c["kind"] != "video":
            emit(event="clip_done", clip=c["name"], frames=P.count_sequence_frames(
                P.read_clip_json(c["root"])["source"]["original_path"]))
            continue
        if cancel.is_set():
            return emit(event="cancelled")
        try:
            n = extract_clip(c["root"])
        except Exception as e:
            emit(event="clip_error", clip=c["name"], message=str(e))
            continue
        if n is None:
            return emit(event="cancelled")
        emit(event="clip_done", clip=c["name"], frames=n)
    emit(event="finished")


if __name__ == "__main__":
    try:
        main()
    except SystemExit:
        raise
    except BaseException as e:  # noqa: BLE001
        emit(event="error", message=f"{type(e).__name__}: {e}")
        sys.exit(1)
