"""Fake worker for the AI UI tests: protocol v1, stdlib only. Unlike fake_worker.py it writes a real manifest v3
(before the first frame, complete at the end) and publishes SOURCE stems, like the real worker.
Mode comes from the clip folder name: late* (normal job, but the process exits 1.5 s after stdin EOF), hold* (stay running until cancelled), slow* (like hold, but answers a cancel only after 1.5 s), crash* (die after 1 frame), else ok."""
import json, os, queue, struct, sys, threading, time, zlib

out = sys.stdout
LOG = os.environ.get("AI_FAKE_LOG")


def emit(o):
    out.write(json.dumps({"v": 1, **o}) + "\n")
    out.flush()


def log(o):
    if LOG:
        with open(LOG, "a") as f:
            f.write(json.dumps(o) + "\n")


def png():
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(b"\x00\xff\x00\x00")) + chunk(b"IEND", b""))


LATE = False
cmds = queue.Queue()
threading.Thread(target=lambda: ([cmds.put(json.loads(l)) for l in sys.stdin], cmds.put(None)), daemon=True).start()
emit({"event": "hello", "protocol": 1, "worker_rev": "fake", "engine": "corridorkey-v2", "host": "fakehost",
      "gpu": {"index": 0, "name": "Fake GPU", "vram_gb": 24.0},
      "models": {"corridorkey_v2": {"path": "/m/ck", "bytes": 1, "sha256": "aa"}},
      "capabilities": {"key": True, "generate_hint": False, "publish_modes": ["final"]}})


def cancelled(jid, wait=0.0):
    try:
        c = cmds.get(timeout=wait) if wait else cmds.get_nowait()
    except queue.Empty:
        return False
    if c is None:
        sys.exit(0)
    return c.get("cmd") == "cancel" and c.get("job_id") == jid


def write_manifest(job, stems, complete):
    mdir = job["manifest_dir"]
    files = {k: [os.path.relpath(p, mdir).replace(os.sep, "/") for p in v] for k, v in job["outputs"].items()}
    cfg = job["output_config"]
    m = {"version": 3, "engine": "corridorkey-v2", "clip_id": job["clip_id"], "job_id": job["job_id"],
         "enabled_outputs": [k for k in ("fg", "matte", "comp", "processed") if cfg.get(k + "_enabled")],
         "formats": {k: cfg.get(k + "_format") for k in ("fg", "matte", "comp", "processed")},
         "exr_compression": cfg.get("exr_compression"), "params": job["params"], "range": job["range"],
         "frame_range": [job["range"]["in_point"], job["range"]["out_point"]], "source_stems": stems, "files": files,
         "effective_profile": {"hann_chunk": 80, "hann_stride": 40, "low_vram": False}, "complete": complete}
    if job.get("hint_keyframes_abs") is not None:
        m["hint_keyframes"] = job["hint_keyframes_abs"]
    tmp = os.path.join(mdir, ".m.tmp")
    with open(tmp, "w") as f:
        json.dump(m, f)
    os.replace(tmp, os.path.join(mdir, ".corridorkey_manifest.json"))


def run(job):
    global LATE
    jid = job["job_id"]
    base = os.path.basename(job["clip_root"])
    LATE = base.startswith("late")
    mode = "hold" if base.startswith(("hold", "slow")) else "crash" if base.startswith("crash") else "ok"
    log({"event": "job", "job_id": jid, "mode": mode, "mdir": job["manifest_dir"]})
    r = job["range"]
    frames = sorted(os.listdir(os.path.join(job["clip_root"], "Frames")))
    stems = [os.path.splitext(f)[0] for f in frames][r["in_point"]:r["out_point"] + 1]
    emit({"event": "accepted", "job_id": jid})
    os.makedirs(job["manifest_dir"], exist_ok=True)
    write_manifest(job, stems, False)
    pub = 0
    for i, stem in enumerate(stems):
        if cancelled(jid):
            return emit({"event": "done", "job_id": jid, "outcome": "cancelled", "message": None, "error_kind": None,
                         "manifest": None, "published": pub, "skipped": 0, "expected": len(stems), "manifest_complete": False})
        for paths in job["outputs"].values():
            os.makedirs(os.path.dirname(paths[i]), exist_ok=True)
            fd = os.open(paths[i], os.O_WRONLY | os.O_CREAT | os.O_EXCL)
            with os.fdopen(fd, "wb") as f:
                f.write(png())
        pub += 1
        emit({"event": "published", "job_id": jid, "stems": [stem]})
        emit({"event": "progress", "job_id": jid, "phase": "publish", "done": i + 1, "total": len(stems)})
        if mode == "crash":
            os._exit(3)
        if mode == "hold" and i == 0:
            while True:  # a cancel command or stdin EOF ends the wait
                try:
                    c = cmds.get(timeout=0.05)
                except queue.Empty:
                    continue
                if c is None:
                    cmds.put(None)  # EOF: still to be seen by the main loop, which then exits
                    break
                if c.get("cmd") == "cancel" and c.get("job_id") == jid:
                    break
            if base.startswith("slow"):
                time.sleep(1.5)
            return emit({"event": "done", "job_id": jid, "outcome": "cancelled", "message": None, "error_kind": None,
                         "manifest": None, "published": pub, "skipped": 0, "expected": len(stems), "manifest_complete": False})
        time.sleep(0.05)
    write_manifest(job, stems, True)
    emit({"event": "done", "job_id": jid, "outcome": "complete", "message": None, "error_kind": None,
          "manifest": os.path.join(job["manifest_dir"], ".corridorkey_manifest.json"), "published": pub, "skipped": 0,
          "expected": len(stems), "manifest_complete": True})


while True:
    c = cmds.get()
    if c is None or c.get("cmd") == "shutdown":
        break
    if c.get("cmd") == "key":
        run(c["job"])
if LATE:  # the job is done, but the process takes its time to exit (model unload, CUDA teardown)
    time.sleep(1.5)
