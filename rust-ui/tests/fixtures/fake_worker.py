"""Fake CorridorKey worker for Rust tests: speaks protocol v1 over stdio with stdlib only.
Behaviour per job comes from job["params"]["fake"]; worker identity from FAKE_CFG (JSON env)."""
import json, os, queue, struct, sys, threading, time, zlib

cfg = json.loads(os.environ.get("FAKE_CFG", "{}"))
out = sys.stdout


def emit(o, v=1):
    out.write(json.dumps({"v": v, **o}) + "\n")
    out.flush()


def log(o):
    if cfg.get("log"):
        with open(cfg["log"], "a") as f:
            f.write(json.dumps(o) + "\n")


def png():
    raw = b"\x00\xff\x00\x00"
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 1, 1, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


gpu = int(sys.argv[sys.argv.index("--gpu") + 1]) if "--gpu" in sys.argv else 0
log({"event": "start", "pid": os.getpid(), "argv": sys.argv, "cwd": os.getcwd(), "ssh_argv": os.environ.get("FAKE_SSH_ARGV")})
cmds = queue.Queue()
eof = False


def reader():
    for line in sys.stdin:
        cmds.put(json.loads(line))
    log({"event": "stdin_eof"})
    cmds.put(None)


threading.Thread(target=reader, daemon=True).start()
time.sleep(cfg.get("hello_delay", 0))
emit({
    "event": "hello", "protocol": cfg.get("protocol", 1), "worker_rev": "fake", "engine": "corridorkey-v2",
    "host": cfg.get("host", "fakehost"),
    "gpu": None if cfg.get("no_gpu") else {"index": cfg.get("gpu", gpu), "name": "Fake GPU", "vram_gb": cfg.get("vram", 24.0)},
    "models": {"corridorkey_v2": {"path": "/m/ck.safetensors", "bytes": 1, "sha256": cfg.get("sha")}},
    "capabilities": {"key": True, "generate_hint": False, "publish_modes": ["final"]},
})


def cancelled(job_id, wait=0.0):
    try:
        c = cmds.get(timeout=wait) if wait else cmds.get_nowait()
    except queue.Empty:
        return False
    if c is None:  # stdin EOF mid-job = cancel, like the real worker; leave after `done`
        global eof
        eof = True
        return True
    return c.get("cmd") == "cancel" and c.get("job_id") == job_id


def run(job):
    jid, p = job["job_id"], job["params"]
    mode = p.get("fake", "ok")
    files = [path for paths in job["outputs"].values() for path in paths]
    log({"event": "job", "job_id": jid, "mode": mode, "first": files[0], "clip_root": job["clip_root"],
         "require": job["require"], "path_map": job["path_map"], "manifest_dir": job["manifest_dir"]})
    if mode == "reject":
        return emit({"event": "rejected", "job_id": jid, "code": "exists", "message": "nope"})
    emit({"event": "accepted", "job_id": jid})
    if mode == "crash":
        emit({"event": "progress", "job_id": jid, "phase": "stage1", "done": 1, "total": len(files)})
        os._exit(3)
    if mode == "grandchild":  # a descendant keeps stderr/stdout open after the worker exits
        import subprocess
        subprocess.Popen(["sleep", "4"])
        sys.stdout.flush()
        os._exit(0)
    if mode == "lock":  # leave this job's lease and a temp behind, then die
        d = job["manifest_dir"]
        os.makedirs(d, exist_ok=True)
        open(os.path.join(d, ".corridorkey_job.lock"), "w").write(json.dumps({"job_id": jid}))
        open(os.path.join(d, ".tmp_marker"), "w").write("x")
        import hashlib  # same naming as backend/service/key_job.py `_tmp_path` and `_probe_link`
        ext = os.path.splitext(files[0])[1]
        tag = hashlib.sha1(files[0].encode("utf-8", "surrogateescape")).hexdigest()[:12]
        for n in (".ck-%s-%s%s" % (jid, tag, ext), ".ck-probe-%s-a" % jid):
            open(os.path.join(os.path.dirname(files[0]), n), "w").write("t")
        os._exit(9)
    if mode == "exit_clean":
        sys.exit(0)
    if mode == "malformed":
        out.write("this is not json\n"); out.flush(); time.sleep(30)
    if mode == "oversized":
        out.write("x" * (2 << 20) + "\n"); out.flush(); time.sleep(30)
    if mode == "badversion":
        emit({"event": "progress", "job_id": jid, "phase": "x", "done": 0, "total": 1}, v=2); time.sleep(30)
    if mode == "mismatch":
        emit({"event": "progress", "job_id": "other-job", "phase": "x", "done": 0, "total": 1}); time.sleep(30)
    if mode == "hold":
        emit({"event": "progress", "job_id": jid, "phase": "stage1", "done": 0, "total": len(files)})
        while not cancelled(jid, 0.05):
            pass
        return emit({"event": "done", "job_id": jid, "outcome": "cancelled", "message": None, "error_kind": None,
                     "manifest": None, "published": 0, "skipped": 0, "expected": len(files), "manifest_complete": False})
    delay, pub, skipped = p.get("delay", 0.0), 0, 0
    for n, path in enumerate(files):
        if cancelled(jid):
            return emit({"event": "done", "job_id": jid, "outcome": "cancelled", "message": None, "error_kind": None,
                         "manifest": None, "published": pub, "skipped": skipped, "expected": len(files), "manifest_complete": False})
        os.makedirs(os.path.dirname(path), exist_ok=True)
        try:  # no-clobber, like the real worker
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL)
            with os.fdopen(fd, "wb") as f:
                f.write(png())
            pub += 1
            emit({"event": "published", "job_id": jid, "stems": [os.path.splitext(os.path.basename(path))[0]]})
        except FileExistsError:
            skipped += 1
        emit({"event": "progress", "job_id": jid, "phase": "publish", "done": n + 1, "total": len(files)})
        if delay:
            time.sleep(delay)
    emit({"event": "done", "job_id": jid, "outcome": "failed" if mode == "fail" else "complete",
          "message": "boom" if mode == "fail" else None, "error_kind": "engine" if mode == "fail" else None,
          "manifest": "/etc/elsewhere/.corridorkey_manifest.json" if mode == "badmanifest" else job["manifest_dir"] + "/.corridorkey_manifest.json", "published": pub, "skipped": skipped,
          "expected": len(files), "manifest_complete": mode != "fail"})


while True:
    c = cmds.get()
    if c is None or c.get("cmd") == "shutdown":
        break
    if c.get("cmd") == "key":
        run(c["job"])
        if eof:
            break
