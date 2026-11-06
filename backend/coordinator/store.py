"""Durable coordinator state: SQLite on LOCAL disk, one BEGIN IMMEDIATE transaction per operation.
The coordinator only records who owns which attempt; it never runs a worker or touches a client path.
No timer ever reassigns a started attempt: only an explicit receiver terminal event ends it."""
import hashlib
import hmac
import json
import os
import secrets
import sqlite3
import threading
import time
import uuid
from contextlib import contextmanager

from . import PROTOCOL
from .schema import (ApiError, MAX_BATCH, canonical, digest, validate_event, validate_session,
                     validate_spec, validate_stats, validate_target, ident, obj, text, bad)

SCHEMA_VERSION = 2
MAX_INSTALLATIONS = 64  # joins stop here
SESSION_TTL = 120  # s without any contact before a registered PC shows as offline (it is never removed)
APP_SESSION = "keyloom-app-session"  # the one session id of the app's own in-process publication
# Unfinished work of one login ({i} = its id column or a placeholder, used three times).
_IN_USE = ("(EXISTS(SELECT 1 FROM jobs j WHERE (j.target={i} OR j.submitter={i}) AND j.state IN ('queued','offered','started')) OR "
           "EXISTS(SELECT 1 FROM attempts a WHERE a.installation_id={i} AND a.state IN ('offered','started')))")
MAX_ACTIVE_JOBS = 2000

_DDL = """
CREATE TABLE installations(id TEXT PRIMARY KEY, name TEXT NOT NULL, is_admin INTEGER NOT NULL,
  token_hash TEXT NOT NULL UNIQUE, created_at REAL NOT NULL, revoked_at REAL);
CREATE TABLE pairings(code_hash TEXT PRIMARY KEY, is_admin INTEGER NOT NULL, expires_at REAL NOT NULL,
  used_at REAL, installation_id TEXT);
CREATE TABLE sessions(installation_id TEXT PRIMARY KEY, session_id TEXT NOT NULL, build TEXT NOT NULL,
  started_at REAL NOT NULL, last_seen REAL NOT NULL);
CREATE TABLE gpu_slots(slot_id TEXT PRIMARY KEY, installation_id TEXT NOT NULL, gpu_uuid TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL, vram_total_mb INTEGER NOT NULL, ram_total_mb INTEGER NOT NULL, enabled INTEGER NOT NULL);
CREATE TABLE jobs(seq INTEGER PRIMARY KEY AUTOINCREMENT, job_id TEXT NOT NULL UNIQUE, submitter TEXT NOT NULL,
  target TEXT, spec_json TEXT NOT NULL, roots_json TEXT NOT NULL, models_json TEXT NOT NULL, state TEXT NOT NULL,
  cancel_requested INTEGER NOT NULL DEFAULT 0, attempt_id TEXT, reason TEXT, created_at REAL NOT NULL, updated_at REAL NOT NULL);
CREATE INDEX jobs_queue ON jobs(state, seq);
CREATE TABLE submissions(submission_id TEXT PRIMARY KEY, submitter TEXT NOT NULL, payload_hash TEXT NOT NULL,
  response_json TEXT NOT NULL);
CREATE TABLE attempts(attempt_id TEXT PRIMARY KEY, job_id TEXT NOT NULL, installation_id TEXT NOT NULL,
  slot_id TEXT NOT NULL, state TEXT NOT NULL, offered_at REAL NOT NULL, started_at REAL, ended_at REAL,
  start_hash TEXT, effective_plan_json TEXT, resources_json TEXT, last_seq INTEGER NOT NULL DEFAULT 0, reason TEXT);
CREATE UNIQUE INDEX one_active_slot ON attempts(slot_id) WHERE state IN ('offered','started');
CREATE UNIQUE INDEX one_active_job ON attempts(job_id) WHERE state IN ('offered','started');
CREATE TABLE claims(claim_id TEXT PRIMARY KEY, installation_id TEXT NOT NULL, request_hash TEXT NOT NULL, attempt_id TEXT NOT NULL);
CREATE TABLE events(id INTEGER PRIMARY KEY AUTOINCREMENT, attempt_id TEXT NOT NULL, job_id TEXT NOT NULL, seq INTEGER NOT NULL,
  type TEXT NOT NULL, payload_json TEXT NOT NULL, payload_hash TEXT NOT NULL, created_at REAL NOT NULL, UNIQUE(attempt_id, seq));
CREATE INDEX events_job ON events(job_id, id);
CREATE TABLE job_rejections(job_id TEXT NOT NULL, installation_id TEXT NOT NULL, PRIMARY KEY(job_id, installation_id));
"""
# Schema 2 added reusable invitations. Both `pairings` and `shared_invitations` stay in place for old databases; nothing writes them now.
_DDL_V2 = """
CREATE TABLE shared_invitations(code_hash TEXT PRIMARY KEY, created_by TEXT NOT NULL, created_at REAL NOT NULL, expires_at REAL NOT NULL);
"""
# App inbox marker; the additive table keeps the database readable by schema-2 builds.
_DDL_V3 = """
CREATE TABLE inbox_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
"""


# Destinations: PCs that registered here AND told this app their own endpoint (reverse publication). Metadata only: the
# credential this app uses to push to that PC lives in a private file, never in the database.
_DDL_V4 = """
CREATE TABLE destinations(installation_id TEXT PRIMARY KEY, name TEXT NOT NULL, host_installation_id TEXT NOT NULL,
  network_id TEXT NOT NULL, fingerprint_sha256 TEXT NOT NULL, endpoints_json TEXT NOT NULL, creds_path TEXT NOT NULL,
  updated_at REAL NOT NULL, removed INTEGER NOT NULL DEFAULT 0);
"""


def token_hash(token):
    return hashlib.sha256(token.encode()).hexdigest()


def new_id(prefix):
    return f"{prefix}_{uuid.uuid4().hex}"


class Store:
    def __init__(self, path, clock=time.time, on_commit=None):
        self.clock = clock
        self.on_commit = on_commit  # (kind, job_id) after a NEW submission or a cancel commits; never called under the lock
        self._lock = threading.Lock()
        self._stats_lock = threading.Lock()
        self._stats = {}  # installation_id -> latest sample of the current session; memory only, no history
        self._seen = {}  # installation_id -> time of its last authenticated call (memory only): who is online
        self._avail = {}  # installation_id -> {receiving, busy, held} the app last published for itself (memory only)
        self._self_iid = None
        # A corrupt or newer database raises here and is left untouched; it is never recreated silently.
        self.db = sqlite3.connect(str(path), isolation_level=None, check_same_thread=False, timeout=10)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        ver = self.db.execute("PRAGMA user_version").fetchone()[0]
        if ver > SCHEMA_VERSION:
            raise RuntimeError(f"coordinator database is schema {ver}; this build understands {SCHEMA_VERSION}")
        if ver == 0:
            if self.db.execute("SELECT count(*) FROM sqlite_master").fetchone()[0]:
                raise RuntimeError("coordinator database has tables but no schema version; refusing to touch it")
            with self._tx():
                for stmt in (_DDL + _DDL_V2 + _DDL_V3 + _DDL_V4).split(";\n"):
                    if stmt.strip():
                        self.db.execute(stmt)
                self.db.execute("INSERT INTO inbox_meta VALUES('migrated_from', '0')")
                self.db.execute(f"PRAGMA user_version={SCHEMA_VERSION}")
        else:
            if ver == 1:
                self._backup(path, ver)
                with self._tx():
                    self.db.execute(_DDL_V2.strip().rstrip(";"))
                    self.db.execute(f"PRAGMA user_version={SCHEMA_VERSION}")
            if not self.db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='inbox_meta'").fetchone():
                self._backup(path, ver)
                with self._tx():
                    self.db.execute(_DDL_V3.strip().rstrip(";"))
            if not self.db.execute("SELECT 1 FROM sqlite_master WHERE type='table' AND name='destinations'").fetchone():
                self._backup(path, ver)
                with self._tx():
                    self.db.execute(_DDL_V4.strip().rstrip(";"))
            if not self.db.execute("SELECT 1 FROM inbox_meta WHERE key='migrated_from'").fetchone():
                self._backup(path, ver)
                with self._tx():
                    now = self.clock()
                    for att in self.db.execute("SELECT * FROM attempts WHERE state='offered'").fetchall():
                        self._requeue_offer(self.db, att, "offer cleared by the app inbox upgrade", now)
                    self.db.execute("INSERT INTO inbox_meta VALUES('migrated_from', ?)", (str(ver),))

    def _backup(self, path, ver):
        """Consistent copy of the old database beside it, taken once before the first in-place upgrade."""
        dest = f"{path}.v{ver}.bak"
        if not os.path.exists(dest):
            out = sqlite3.connect(dest)
            try:
                self.db.backup(out)
            finally:
                out.close()
            try:
                os.chmod(dest, 0o600)
            except OSError:
                pass

    def _notify(self, kind, job_id):
        if self.on_commit:
            try:
                self.on_commit(kind, job_id)
            except Exception:
                pass  # a broken listener must never undo or fail a committed submission

    def close(self):
        self.db.close()

    @contextmanager
    def _tx(self):
        with self._lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                yield self.db
            except BaseException:
                self.db.execute("ROLLBACK")
                raise
            self.db.execute("COMMIT")

    def _requeue_offer(self, db, att, why, now):
        """Compare-and-set on the OFFERED state only; a started attempt is never touched. The job goes back to the
        queue (a late start is then refused 409 because the attempt is no longer offered)."""
        if db.execute("UPDATE attempts SET state='revoked', ended_at=?, reason=? WHERE attempt_id=? AND state='offered'",
                      (now, why, att["attempt_id"])).rowcount:
            db.execute("UPDATE jobs SET state='queued', attempt_id=NULL, updated_at=? WHERE job_id=? AND state='offered'",
                       (now, att["job_id"]))

    # ---- identity -----------------------------------------------------------------------------------

    def _issue(self, db, name, is_admin, now):
        token, iid = secrets.token_urlsafe(32), new_id("inst")
        db.execute("INSERT INTO installations VALUES(?,?,?,?,?,NULL)", (iid, name, int(is_admin), token_hash(token), now))
        return iid, token

    def _room(self, db):
        if db.execute("SELECT count(*) FROM installations WHERE revoked_at IS NULL").fetchone()[0] >= MAX_INSTALLATIONS:
            raise ApiError(409, "network_full", "this network already has the maximum number of installations")

    def join(self, name):
        """Open join (the caller has already authorised the transport path): a unique non-admin installation + token."""
        text(name, "name", 64)
        with self._tx() as db:
            self._room(db)
            return self._issue(db, name, False, self.clock())

    def authenticate(self, bearer):
        """`<installation_id>.<token>` -> installation row, constant-time hash compare, revoked refused."""
        iid, _, token = bearer.partition(".")
        with self._lock:
            row = self.db.execute("SELECT * FROM installations WHERE id=?", (iid,)).fetchone()
        ok = hmac.compare_digest(row["token_hash"] if row else "0" * 64, token_hash(token))
        if row and not ok and row["revoked_at"] is None:
            ok = self._promote_pending(iid, token)
        if not row or not ok or row["revoked_at"] is not None:
            raise ApiError(401, "unauthorized", "invalid credentials")
        self._seen[iid] = self.clock()  # a successful call is the proof that PC is on
        return dict(row)

    def installation_active(self, installation_id):
        with self._lock:
            return self.db.execute("SELECT 1 FROM installations WHERE id=? AND revoked_at IS NULL", (installation_id,)).fetchone() is not None

    def _online(self, iid, last_seen=None):
        """True while this PC made an authenticated call, or the app published itself, within SESSION_TTL."""
        t = max(self._seen.get(iid) or 0, last_seen or 0)
        return bool(t) and self.clock() - t < SESSION_TTL

    def session_online(self, installation_id):
        with self._lock:
            row = self.db.execute("SELECT last_seen FROM sessions WHERE installation_id=?", (installation_id,)).fetchone()
        return self._online(installation_id, row["last_seen"] if row else None)

    def revoke(self, installation_id, only_if_unused=False, only_if_offline=False):
        """Returns the STARTED attempts that stay fenced: revoking credentials proves nothing about a running writer.
        only_if_unused: refuse with 409 in_use, in the same transaction, while the login still has unfinished work."""
        now = self.clock()
        with self._tx() as db:
            if not db.execute("SELECT 1 FROM installations WHERE id=?", (installation_id,)).fetchone():
                raise ApiError(404, "unknown_installation", "no such installation")
            if only_if_unused and db.execute("SELECT " + _IN_USE.format(i="?"), (installation_id,) * 3).fetchone()[0]:
                raise ApiError(409, "in_use", "this login has unfinished jobs")
            if only_if_offline:
                session = db.execute("SELECT last_seen FROM sessions WHERE installation_id=?", (installation_id,)).fetchone()
                if self._online(installation_id, session["last_seen"] if session else None):
                    raise ApiError(409, "online", "this login is online")
            db.execute("UPDATE installations SET revoked_at=? WHERE id=? AND revoked_at IS NULL", (now, installation_id))
            db.execute("UPDATE gpu_slots SET enabled=0 WHERE installation_id=?", (installation_id,))
            db.execute("DELETE FROM sessions WHERE installation_id=?", (installation_id,))
            for att in db.execute("SELECT * FROM attempts WHERE installation_id=? AND state='offered'", (installation_id,)).fetchall():
                self._requeue_offer(db, att, "installation revoked before start", now)
            return [r[0] for r in db.execute("SELECT attempt_id FROM attempts WHERE installation_id=? AND state='started'", (installation_id,))]

    def abandon(self, attempt_id, reason):
        """Operator decision for a lost STARTED attempt: terminal 'failed', never requeued. Does not stop or prove
        stopped any writer and releases no filesystem lock; the receiver's later events get 409 attempt_terminal."""
        text(reason, "reason", 200)
        now = self.clock()
        with self._tx() as db:
            att = db.execute("SELECT * FROM attempts WHERE attempt_id=?", (attempt_id,)).fetchone()
            if not att:
                raise ApiError(404, "unknown_attempt", "no such attempt")
            if att["state"] != "started":
                raise ApiError(409, "attempt_not_started", f"attempt is {att['state']}; only a started attempt can be abandoned")
            why = f"abandoned by operator (writer NOT proven stopped): {reason}"
            db.execute("UPDATE attempts SET state='failed', ended_at=?, reason=? WHERE attempt_id=? AND state='started'", (now, why, attempt_id))
            db.execute("UPDATE jobs SET state='failed', reason=?, updated_at=? WHERE job_id=?", (why, now, att["job_id"]))
        return {"attempt_id": attempt_id, "job_id": att["job_id"], "state": "failed"}

    # ---- sessions ---------------------------------------------------------------------------------

    def _session(self, db, iid, session_id):
        ident(session_id, "session_id")
        row = db.execute("SELECT * FROM sessions WHERE installation_id=?", (iid,)).fetchone()
        if not row or row["session_id"] != session_id:
            raise ApiError(409, "stale_session", "register a session first (or another session now owns this installation)")
        db.execute("UPDATE sessions SET last_seen=? WHERE installation_id=?", (self.clock(), iid))

    def _register_gpu(self, inst, body):
        validate_session(body)
        g, now, iid = body["gpus"][0], self.clock(), inst["id"]
        slot = f"{iid}:{g['uuid']}"
        with self._tx() as db:
            cur = db.execute("SELECT * FROM sessions WHERE installation_id=?", (iid,)).fetchone()
            owner = db.execute("SELECT g.installation_id FROM gpu_slots g JOIN installations i ON i.id=g.installation_id "
                               "WHERE g.gpu_uuid=? AND i.revoked_at IS NULL", (g["uuid"],)).fetchone()
            if owner and owner[0] != iid:
                raise ApiError(409, "gpu_identity_conflict", "this physical GPU is already registered by another installation")
            if db.execute("SELECT 1 FROM gpu_slots WHERE installation_id=? AND gpu_uuid<>?", (iid, g["uuid"])).fetchone():
                raise ApiError(409, "gpu_identity_conflict", "this installation already registered a different GPU")
            db.execute("DELETE FROM gpu_slots WHERE gpu_uuid=? AND installation_id<>?", (g["uuid"], iid))
            db.execute("INSERT INTO gpu_slots VALUES(?,?,?,?,?,?,1) ON CONFLICT(slot_id) DO UPDATE SET name=excluded.name,"
                       "vram_total_mb=excluded.vram_total_mb, ram_total_mb=excluded.ram_total_mb",
                       (slot, iid, g["uuid"], g["name"], g["vram_total_mb"], g["ram_total_mb"]))
            if "name" in body:
                db.execute("UPDATE installations SET name=? WHERE id=?", (body["name"], iid))
            started = cur["started_at"] if cur and cur["session_id"] == body["session_id"] else now
            db.execute("INSERT OR REPLACE INTO sessions VALUES(?,?,?,?,?)", (iid, body["session_id"], body["build"], started, now))

    def publish_self(self, iid, name=None, gpu=None, stats=None, build="app", avail=None):
        """The app writes its OWN row in-process (no HTTP, no session handshake): display name, GPU slot and the latest load
        sample the other PCs read from `installations`. Called with the same 5 s cadence the load was always reported at."""
        if not self.installation_active(iid):
            raise ApiError(404, "unknown_installation", "this PC's installation is not registered here")
        if gpu:
            body = {"protocol": PROTOCOL, "session_id": APP_SESSION, "build": build, "gpus": [gpu]}
            if name:
                body["name"] = name
            self._register_gpu({"id": iid}, body)
        elif name:
            with self._tx() as db:
                db.execute("UPDATE installations SET name=? WHERE id=?", (name, iid))
        if stats:
            self.report_stats({"id": iid}, {"session_id": APP_SESSION, **stats})
        self._seen[iid] = self.clock()
        self._self_iid = iid
        if avail is not None:
            self._avail[iid] = {k: bool(avail.get(k)) for k in ("receiving", "busy", "held")}

    def _not_free(self, db):
        """Why this PC is not free for an Automatic shot right now (None = free): what the app last published, plus anything
        already in the inbox (a shot that landed after that publication)."""
        a = self._avail.get(self._self_iid)
        if a is None or not a["receiving"]:
            return "Receive jobs is off or this PC has not reported yet"
        if a["busy"] or a["held"]:
            return "this PC is busy or holds an unsettled shot"
        if db.execute("SELECT 1 FROM jobs WHERE state IN ('queued','offered','started')").fetchone():
            return "this PC already has a shot"
        return None

    # ---- submission -------------------------------------------------------------------------------

    def submit(self, inst, body, only_if_free=False, accepting=None):
        obj(body, "submission", ("submission_id", "spec", "target"))
        sid = ident(body["submission_id"], "submission_id")
        target = validate_target(body["target"])
        info = validate_spec(body["spec"])
        phash = digest({"spec": body["spec"], "target": target})
        with self._tx() as db:
            prior = db.execute("SELECT * FROM submissions WHERE submission_id=?", (sid,)).fetchone()
            if prior:
                if prior["submitter"] != inst["id"] or prior["payload_hash"] != phash:
                    raise ApiError(409, "submission_conflict", "submission_id was already used with different content")
                return json.loads(prior["response_json"])
            if accepting is not None and not accepting():
                raise ApiError(409, "not_free" if only_if_free else "not_receiving", "this PC is not taking jobs from other PCs (Receive jobs is off)")
            if target and not db.execute("SELECT 1 FROM installations WHERE id=? AND revoked_at IS NULL", (target,)).fetchone():
                bad("target installation does not exist")
            if only_if_free and (why := self._not_free(db)):  # an Automatic shot is only taken by a PC that is still free
                raise ApiError(409, "not_free", why)
            if db.execute("SELECT count(*) FROM jobs WHERE state IN ('queued','offered','started')").fetchone()[0] >= MAX_ACTIVE_JOBS:
                raise ApiError(429, "queue_full", "too many active jobs")
            now, jid = self.clock(), new_id("job")
            db.execute("INSERT INTO jobs(job_id,submitter,target,spec_json,roots_json,models_json,state,created_at,updated_at)"
                       " VALUES(?,?,?,?,?,?,'queued',?,?)",
                       (jid, inst["id"], target, canonical(body["spec"]), json.dumps(info["roots"]), canonical(info["models"]), now, now))
            resp = {"job_id": jid, "state": "queued"}
            db.execute("INSERT INTO submissions VALUES(?,?,?,?)", (sid, inst["id"], phash, canonical(resp)))
        self._notify("submitted", jid)  # after COMMIT: the app learns of the shot only once it is durable
        return resp

    def _ctl(self, db, att):
        job = db.execute("SELECT cancel_requested FROM jobs WHERE job_id=?", (att["job_id"],)).fetchone()
        cur = db.execute("SELECT state, last_seq FROM attempts WHERE attempt_id=?", (att["attempt_id"],)).fetchone()
        return {"state": cur["state"], "last_seq": cur["last_seq"], "cancel_requested": bool(job["cancel_requested"])}

    # ---- events -----------------------------------------------------------------------------------

    def _record(self, db, att, evs, attempt_id):
        if att["state"] == "offered":
            raise ApiError(409, "attempt_not_started", "acknowledge start before reporting events")
        last, state, now = att["last_seq"], att["state"], self.clock()
        for i, e in enumerate(evs):
            payload, ph = canonical(e), digest(e)
            if e["seq"] <= last:  # replay: identical content is a no-op, anything else is a conflict
                old = db.execute("SELECT payload_hash FROM events WHERE attempt_id=? AND seq=?", (attempt_id, e["seq"])).fetchone()
                if not old or old[0] != ph:
                    raise ApiError(409, "seq_conflict", f"seq {e['seq']} was already reported with different content")
                continue
            if state != "started":
                raise ApiError(409, "attempt_terminal", "attempt already ended")
            if e["seq"] != last + 1:
                raise ApiError(409, "seq_gap", f"expected seq {last + 1}, got {e['seq']}")
            if e["type"] == "terminal" and i != len(evs) - 1:
                bad("terminal event must be last in its batch")
            db.execute("INSERT INTO events(attempt_id,job_id,seq,type,payload_json,payload_hash,created_at) VALUES(?,?,?,?,?,?,?)",
                       (attempt_id, att["job_id"], e["seq"], e["type"], payload, ph, now))
            last = e["seq"]
            if e["type"] == "terminal":
                state = e["data"]["state"]
                db.execute("UPDATE attempts SET state=?, ended_at=?, reason=? WHERE attempt_id=?",
                           (state, now, e["data"].get("message"), attempt_id))
                db.execute("UPDATE jobs SET state=?, reason=?, updated_at=? WHERE job_id=?",
                           (state, e["data"].get("message"), now, att["job_id"]))
        db.execute("UPDATE attempts SET last_seq=? WHERE attempt_id=?", (last, attempt_id))
        att = db.execute("SELECT * FROM attempts WHERE attempt_id=?", (attempt_id,)).fetchone()
        return self._ctl(db, att)

    # ---- submitter side ---------------------------------------------------------------------------

    def _job(self, db, inst, job_id):
        job = db.execute("SELECT * FROM jobs WHERE job_id=?", (job_id,)).fetchone()
        if not job or not (inst["is_admin"] or job["submitter"] == inst["id"]):  # owner or admin only
            raise ApiError(404, "unknown_job", "no such job")
        return job

    def job_view(self, inst, job_id):
        with self._tx() as db:
            job = self._job(db, inst, job_id)
            view = {"job_id": job_id, "state": job["state"], "cancel_requested": bool(job["cancel_requested"]),
                    "attempt_id": job["attempt_id"], "reason": job["reason"], "target": job["target"]}
            if job["attempt_id"]:
                a = db.execute("SELECT * FROM attempts WHERE attempt_id=?", (job["attempt_id"],)).fetchone()
                view.update(attempt_state=a["state"], installation_id=a["installation_id"], last_seq=a["last_seq"],
                            effective_plan=json.loads(a["effective_plan_json"]) if a["effective_plan_json"] else None)
            if job["state"] == "queued":
                view["queue_position"] = db.execute("SELECT count(*) FROM jobs WHERE state='queued' AND seq<=?", (job["seq"],)).fetchone()[0]
            for kind in ("progress", "terminal"):  # the sender's whole answer: how far, and how it ended
                r = db.execute("SELECT payload_json FROM events WHERE job_id=? AND type=? ORDER BY id DESC LIMIT 1", (job_id, kind)).fetchone()
                if r:
                    view["result" if kind == "terminal" else kind] = json.loads(r[0])["data"]
            return view

    def cancel(self, inst, job_id):
        with self._tx() as db:
            job = self._job(db, inst, job_id)
            now, st = self.clock(), job["state"]
            if st == "queued":
                db.execute("UPDATE jobs SET state='cancelled', cancel_requested=1, updated_at=? WHERE job_id=?", (now, job_id))
            elif st == "offered":  # never started: end safely; a late start is then refused
                db.execute("UPDATE attempts SET state='revoked', ended_at=?, reason='cancelled before start' WHERE attempt_id=? AND state='offered'",
                           (now, job["attempt_id"]))
                db.execute("UPDATE jobs SET state='cancelled', cancel_requested=1, updated_at=? WHERE job_id=?", (now, job_id))
            elif st == "started":  # request only: the receiver must report its own terminal state
                db.execute("UPDATE jobs SET cancel_requested=1, updated_at=? WHERE job_id=?", (now, job_id))
        if st in ("queued", "offered", "started"):
            self._notify("cancelled", job_id)
        return self.job_view(inst, job_id)

    def job_events(self, inst, job_id, after=0, limit=100):
        if not 0 <= after < 2**62 or not 1 <= limit <= MAX_BATCH:
            bad("after/limit out of range")
        with self._tx() as db:
            self._job(db, inst, job_id)
            rows = db.execute("SELECT id, attempt_id, seq, payload_json FROM events WHERE job_id=? AND id>? ORDER BY id LIMIT ?",
                              (job_id, after, limit)).fetchall()
        evs = [{"cursor": r["id"], "attempt_id": r["attempt_id"], **json.loads(r["payload_json"])} for r in rows]
        return {"events": evs, "cursor": evs[-1]["cursor"] if evs else after}

    # ---- app inbox (the receiving app is the only reader; no session, offer or claim) -------------

    def inbox_pending(self, host_id):
        """Queued shots for this PC (aimed at it or at nobody), oldest first. Read at startup and on a commit notice."""
        with self._lock:
            rows = self.db.execute("SELECT job_id, spec_json, cancel_requested FROM jobs WHERE state='queued' AND (target IS NULL OR target=?)"
                                   " ORDER BY seq", (host_id,)).fetchall()
        return [{"job_id": r["job_id"], "spec": json.loads(r["spec_json"]), "cancel_requested": bool(r["cancel_requested"])} for r in rows]

    def inbox_held(self, host_id):
        """Started shots with no terminal event: uncertain after a restart. Never replayed; the user resolves them (abandon).
        A started attempt that belonged to ANOTHER PC (an old database that served several PCs) is listed with `foreign`
        and that PC's name: nothing here can run or settle it any more."""
        with self._lock:
            rows = self.db.execute("SELECT a.job_id, a.attempt_id, a.started_at, a.last_seq, a.effective_plan_json, a.installation_id, i.name "
                                   "FROM attempts a LEFT JOIN installations i ON i.id=a.installation_id WHERE a.state='started' ORDER BY a.started_at").fetchall()
        return [{"job_id": r["job_id"], "attempt_id": r["attempt_id"], "started_at": r["started_at"], "last_seq": r["last_seq"],
                 "plan": json.loads(r["effective_plan_json"]) if r["effective_plan_json"] else {},
                 **({"foreign": True, "pc": r["name"] or r["installation_id"]} if r["installation_id"] != host_id else {})} for r in rows]

    def inbox_foreign(self, host_id):
        """Queued shots an OLD database holds for ANOTHER registered PC (offers were already put back in the queue). The app sends
        each to that PC's own app with its original submission id, so a retry never makes a second shot."""
        with self._lock:
            rows = self.db.execute("SELECT job_id, target, spec_json FROM jobs WHERE state='queued' AND target IS NOT NULL AND target<>? ORDER BY seq", (host_id,)).fetchall()
            out = []
            for r in rows:
                sub = self.db.execute("SELECT submission_id FROM submissions WHERE response_json=?", (canonical({"job_id": r["job_id"], "state": "queued"}),)).fetchone()
                out.append({"job_id": r["job_id"], "target": r["target"], "submission_id": sub[0] if sub else None, "spec": json.loads(r["spec_json"])})
        return out

    def inbox_forwarded(self, job_id, note):
        """The target PC's app accepted a shot that was queued here for it: close it here, with where it went. Never re-run."""
        text(note, "note", 400)
        with self._tx() as db:
            db.execute("UPDATE jobs SET state='forwarded', reason=?, updated_at=? WHERE job_id=? AND state='queued'", (note, self.clock(), job_id))
        return {"job_id": job_id, "state": "forwarded"}

    def inbox_begin(self, host_id, job_id, slot, plan):
        """The app starts one queued shot on its own worker `slot`. A busy slot refuses (the shot stays queued)."""
        if not isinstance(plan, dict) or len(canonical(plan)) > 8192:
            bad("plan must be an object of at most 8 KiB")
        now, aid = self.clock(), new_id("att")
        with self._tx() as db:
            job = db.execute("SELECT * FROM jobs WHERE job_id=?", (job_id,)).fetchone()
            if not job or (job["target"] not in (None, host_id)):
                raise ApiError(404, "unknown_job", "no such job")
            if job["state"] != "queued":
                raise ApiError(409, "not_queued", f"job is {job['state']}")
            if db.execute("SELECT 1 FROM attempts WHERE slot_id=? AND state IN ('offered','started')", (slot,)).fetchone():
                raise ApiError(409, "slot_busy", "that worker already runs a shot")
            db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at,started_at,start_hash,effective_plan_json)"
                       " VALUES(?,?,?,?,'started',?,?,?,?)", (aid, job_id, host_id, slot, now, now, digest(plan), canonical(plan)))
            db.execute("UPDATE jobs SET state='started', attempt_id=?, updated_at=? WHERE job_id=?", (aid, now, job_id))
        return {"attempt_id": aid}

    def inbox_report(self, host_id, job_id, events):
        """Progress/terminal events from the app's own worker; same sequencing rules as a remote report."""
        if not isinstance(events, list) or len(events) > MAX_BATCH:
            bad(f"events must be a list of at most {MAX_BATCH}")
        for e in events:
            validate_event(e)
        with self._tx() as db:
            att = db.execute("SELECT * FROM attempts WHERE job_id=? AND installation_id=? ORDER BY offered_at DESC LIMIT 1", (job_id, host_id)).fetchone()
            if not att:
                raise ApiError(404, "unknown_job", "no such job")
            return self._record(db, att, events, att["attempt_id"])

    def inbox_cancel_requested(self, job_id):
        with self._lock:
            r = self.db.execute("SELECT cancel_requested FROM jobs WHERE job_id=?", (job_id,)).fetchone()
        return bool(r and r[0])

    # ---- destinations (reverse publication) -----------------------------------------------------

    def issue_destination(self, host_key, name, pending_file):
        """The login another PC (host_key = its certificate fingerprint) uses to push to THIS PC. ONE installation id per host, kept for
        good: it owns that host's earlier sends. Asking again only prepares a replacement token for the SAME id (kept in the private
        `pending_file`); the old token keeps working until the host first uses the new one (`authenticate` promotes it). Asking again
        before that returns the same offer, so a retried or lost publication never mints another login."""
        text(name, "name", 64)
        key = f"reverse:{host_key}"
        with self._tx() as db:
            old = db.execute("SELECT i.id FROM inbox_meta m JOIN installations i ON i.id=m.value WHERE m.key=? AND i.revoked_at IS NULL", (key,)).fetchone()
            if not old:  # the identity starts with a token nobody has; the pending token below is its first real login
                self._room(db)
                iid, _ = self._issue(db, name, False, self.clock())
                db.execute("INSERT OR REPLACE INTO inbox_meta VALUES(?,?)", (key, iid))
            else:
                iid = old[0]
            pkey = f"reversepend:{iid}"
            if db.execute("SELECT 1 FROM inbox_meta WHERE key=?", (pkey,)).fetchone() and os.path.exists(pending_file):
                with open(pending_file, encoding="utf-8") as f:
                    return iid, f.read().strip()
            token = secrets.token_urlsafe(32)
            tmp = f"{pending_file}.{uuid.uuid4().hex[:8]}"
            from . import pki
            pki.write_private_new(tmp, token)
            os.replace(tmp, pending_file)
            db.execute("INSERT OR REPLACE INTO inbox_meta VALUES(?,?)", (pkey, json.dumps({"hash": token_hash(token), "file": str(pending_file)})))
            return iid, token

    def _promote_pending(self, iid, token):
        """The host used its replacement token: it becomes the login and the old one stops working."""
        with self._tx() as db:
            r = db.execute("SELECT value FROM inbox_meta WHERE key=?", (f"reversepend:{iid}",)).fetchone()
            if not r:
                return False
            p = json.loads(r[0])
            if not hmac.compare_digest(p["hash"], token_hash(token)):
                return False
            db.execute("UPDATE installations SET token_hash=? WHERE id=?", (p["hash"], iid))
            db.execute("DELETE FROM inbox_meta WHERE key=?", (f"reversepend:{iid}",))
        try:
            os.unlink(p["file"])
        except OSError:
            pass
        return True

    def set_destination(self, iid, meta):
        """Remember how to push to the PC registered here as `iid` (same PC id as its registration). Re-publishing replaces it."""
        with self._tx() as db:
            if not db.execute("SELECT 1 FROM installations WHERE id=? AND revoked_at IS NULL", (iid,)).fetchone():
                raise ApiError(404, "unknown_installation", "no such installation")
            db.execute("INSERT OR REPLACE INTO destinations VALUES(?,?,?,?,?,?,?,?,0)",
                       (iid, meta["name"], meta["host_installation_id"], meta["network_id"], meta["fingerprint_sha256"],
                        json.dumps(meta["endpoints"]), meta["creds_path"], self.clock()))
        self._notify("destination", iid)

    def destinations(self):
        with self._lock:
            rows = self.db.execute("SELECT d.* FROM destinations d JOIN installations i ON i.id=d.installation_id "
                                   "WHERE d.removed=0 AND i.revoked_at IS NULL ORDER BY d.updated_at").fetchall()
        return [{"pc_id": r["installation_id"], "name": r["name"], "host_installation_id": r["host_installation_id"],
                 "network_id": r["network_id"], "fingerprint_sha256": r["fingerprint_sha256"],
                 "endpoints": json.loads(r["endpoints_json"]), "credentials": r["creds_path"]} for r in rows]

    def remove_destination(self, iid):
        with self._tx() as db:
            db.execute("UPDATE destinations SET removed=1 WHERE installation_id=?", (iid,))

    # ---- live stats (separate from job events; works while idle) ----------------------------------

    def report_stats(self, inst, body):
        validate_stats(body)
        now = self.clock()
        with self._tx() as db:
            self._session(db, inst["id"], body["session_id"])  # also counts as a heartbeat
        sample = {k: v for k, v in body.items() if k not in ("session_id", "measured_age_ms")}
        with self._stats_lock:
            cur = self._stats.get(inst["id"])
            if cur and cur["session_id"] == body["session_id"] and body["seq"] <= cur["seq"]:
                return {"accepted": False, "seq": cur["seq"]}  # replay/out-of-order: latest wins, no error
            self._stats[inst["id"]] = {"session_id": body["session_id"], "seq": body["seq"], "sample": sample,
                                       "measured_at": now - body["measured_age_ms"] / 1000, "received_at": now}
        return {"accepted": True, "seq": body["seq"]}

    def local_installations(self, own):
        with self._lock:
            rows = self.db.execute("SELECT i.id, i.name, i.created_at, s.last_seen, "
                + _IN_USE.format(i="i.id") + " AS in_use "
                "FROM installations i LEFT JOIN sessions s ON s.installation_id=i.id "
                "WHERE i.revoked_at IS NULL ORDER BY i.created_at").fetchall()
        return {"installations": [{"installation_id": r["id"], "name": r["name"], "created_at": r["created_at"],
                "last_seen": r["last_seen"], "connected": self._online(r["id"], r["last_seen"]),
                "in_use": bool(r["in_use"]), "is_this_pc": r["id"] == own} for r in rows]}

    def installations(self, _inst):
        """Any paired installation may see every client's status (for per-client cards). Ages are server-computed;
        `connected` and `stats` are separate so unavailable/stale is never shown as 0."""
        now = self.clock()
        with self._tx() as db:
            rows = db.execute("SELECT i.id, i.name, s.session_id, s.last_seen, g.slot_id, g.name AS gpu, g.vram_total_mb "
                              "FROM installations i LEFT JOIN sessions s ON s.installation_id=i.id "
                              "LEFT JOIN gpu_slots g ON g.installation_id=i.id AND g.enabled=1 WHERE i.revoked_at IS NULL ORDER BY i.created_at").fetchall()
        out = []
        for r in rows:
            st = self._stats.get(r["id"])
            live = st if st and st["session_id"] == r["session_id"] else None
            av = self._avail.get(r["id"]) or {}
            out.append({"installation_id": r["id"], "name": r["name"], "gpu_slot": r["slot_id"], "gpu_name": r["gpu"],
                        "receiving": av.get("receiving"), "busy": av.get("busy"), "held": av.get("held"),
                        "connected": self._online(r["id"], r["last_seen"]),
                        "last_seen_age_s": None if r["last_seen"] is None else round(now - r["last_seen"], 3),
                        "stats": None if not live else {**live["sample"], "measured_age_s": round(now - live["measured_at"], 3),
                                                        "received_age_s": round(now - live["received_at"], 3)}})
        return {"installations": out}
