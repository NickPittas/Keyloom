"""Durable coordinator state: SQLite on LOCAL disk, one BEGIN IMMEDIATE transaction per operation.
The coordinator only records who owns which attempt; it never runs a worker or touches a client path.
No timer ever reassigns a started attempt: only an explicit receiver terminal event ends it."""
import hashlib
import hmac
import json
import secrets
import sqlite3
import threading
import time
import uuid
from contextlib import contextmanager

from . import PROTOCOL
from .schema import (ApiError, MAX_BATCH, canonical, digest, validate_claim, validate_event, validate_session,
                     validate_spec, validate_stats, validate_target, ident, obj, text, choice, bad, REJECT_CODES)

SCHEMA_VERSION = 2
MAX_INSTALLATIONS = 64  # joins by address / reusable invitation stop here; one-use pairings are unaffected
MAX_SHARED_INVITATIONS = 16
SHARED_TTL = 7 * 86400
SESSION_TTL = 120  # s without any call before another session id may take over the installation
MAX_ACTIVE_JOBS = 2000
_CODE_ALPHABET = "ABCDEFGHJKMNPQRSTUVWXYZ23456789"

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
# Schema 2 only adds reusable (non-admin, multi-use, expiring) invitations; the one-use `pairings` table is untouched.
_DDL_V2 = """
CREATE TABLE shared_invitations(code_hash TEXT PRIMARY KEY, created_by TEXT NOT NULL, created_at REAL NOT NULL, expires_at REAL NOT NULL);
"""


def token_hash(token):
    return hashlib.sha256(token.encode()).hexdigest()


def _code_hash(code):
    return hashlib.sha256("".join(c for c in code.upper() if c.isalnum()).encode()).hexdigest()


def new_id(prefix):
    return f"{prefix}_{uuid.uuid4().hex}"


class Store:
    def __init__(self, path, clock=time.time):
        self.clock = clock
        self._lock = threading.Lock()
        self._ready = False
        self._stats_lock = threading.Lock()
        self._stats = {}  # installation_id -> latest sample of the current session; memory only, no history
        self._caps = {}  # installation_id -> capabilities of its latest claim (to explain why a queued job waits)
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
                for stmt in (_DDL + _DDL_V2).split(";\n"):
                    if stmt.strip():
                        self.db.execute(stmt)
                self.db.execute(f"PRAGMA user_version={SCHEMA_VERSION}")
        elif ver == 1:  # in-place, additive, one transaction: existing installations, jobs and unused one-use codes are kept
            with self._tx():
                self.db.execute(_DDL_V2.strip().rstrip(";"))
                self.db.execute(f"PRAGMA user_version={SCHEMA_VERSION}")
        self._ready = True

    def close(self):
        self.db.close()

    @contextmanager
    def _tx(self):
        with self._lock:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                if self._ready:
                    self._expire_offers(self.db)
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

    def _expire_offers(self, db):
        """An offer whose owner has no live session is unstarted work nobody holds: revoke and requeue it."""
        now = self.clock()
        for att in db.execute("SELECT a.* FROM attempts a LEFT JOIN sessions s ON s.installation_id=a.installation_id "
                              "WHERE a.state='offered' AND (s.last_seen IS NULL OR ? - s.last_seen > ?)",
                              (now, SESSION_TTL)).fetchall():
            self._requeue_offer(db, att, "offer expired: owner session lapsed before start", now)

    # ---- pairing / identity -----------------------------------------------------------------------

    def create_pairing(self, is_admin=False, ttl=600):
        code = "".join(secrets.choice(_CODE_ALPHABET) for _ in range(10))
        with self._tx() as db:
            db.execute("INSERT INTO pairings VALUES(?,?,?,NULL,NULL)", (_code_hash(code), int(is_admin), self.clock() + ttl))
        return f"{code[:5]}-{code[5:]}"

    def create_shared_invitation(self, creator, ttl=SHARED_TTL):
        """Reusable code for any number of NON-ADMIN installations until it expires (7 days). Returns (code, expires_at)."""
        code = "".join(secrets.choice(_CODE_ALPHABET) for _ in range(20))
        with self._tx() as db:
            now = self.clock()
            db.execute("DELETE FROM shared_invitations WHERE expires_at<?", (now,))
            if db.execute("SELECT count(*) FROM shared_invitations").fetchone()[0] >= MAX_SHARED_INVITATIONS:
                raise ApiError(409, "too_many_invitations", "too many active invitations; wait for one to expire")
            db.execute("INSERT INTO shared_invitations VALUES(?,?,?,?)", (_code_hash(code), creator["id"], now, now + ttl))
        return "-".join(code[i:i + 5] for i in range(0, 20, 5)), now + ttl

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

    def pair(self, code, name):
        """One-use code (or a reusable invitation): the same transaction creates the installation. Token is returned once."""
        text(name, "name", 64)
        if not isinstance(code, str) or len(code) > 32:
            bad("code must be the pairing code")
        now = self.clock()
        with self._tx() as db:
            h = _code_hash(code)
            row = db.execute("SELECT * FROM pairings WHERE code_hash=?", (h,)).fetchone()
            if row and row["used_at"] is None and row["expires_at"] >= now:
                iid, token = self._issue(db, name, row["is_admin"], now)
                db.execute("UPDATE pairings SET used_at=?, installation_id=? WHERE code_hash=? AND used_at IS NULL", (now, iid, h))
                return iid, token
            if not row and db.execute("SELECT 1 FROM shared_invitations WHERE code_hash=? AND expires_at>=?", (h, now)).fetchone():
                self._room(db)
                return self._issue(db, name, False, now)
            raise ApiError(403, "pairing_rejected", "pairing code is unknown, used or expired")

    def authenticate(self, bearer):
        """`<installation_id>.<token>` -> installation row, constant-time hash compare, revoked refused."""
        iid, _, token = bearer.partition(".")
        with self._lock:
            row = self.db.execute("SELECT * FROM installations WHERE id=?", (iid,)).fetchone()
        ok = hmac.compare_digest(row["token_hash"] if row else "0" * 64, token_hash(token))
        if not row or not ok or row["revoked_at"] is not None:
            raise ApiError(401, "unauthorized", "invalid credentials")
        return dict(row)

    def revoke(self, installation_id):
        """Returns the STARTED attempts that stay fenced: revoking credentials proves nothing about a running writer."""
        now = self.clock()
        with self._tx() as db:
            if not db.execute("SELECT 1 FROM installations WHERE id=?", (installation_id,)).fetchone():
                raise ApiError(404, "unknown_installation", "no such installation")
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

    def register(self, inst, body):
        validate_session(body)
        g, now, iid = body["gpus"][0], self.clock(), inst["id"]
        slot = f"{iid}:{g['uuid']}"
        with self._tx() as db:
            cur = db.execute("SELECT * FROM sessions WHERE installation_id=?", (iid,)).fetchone()
            if cur and cur["session_id"] != body["session_id"] and now - cur["last_seen"] < SESSION_TTL:
                raise ApiError(409, "session_conflict", "another live session already owns this installation")
            owner = db.execute("SELECT g.installation_id FROM gpu_slots g JOIN installations i ON i.id=g.installation_id "
                               "WHERE g.gpu_uuid=? AND i.revoked_at IS NULL", (g["uuid"],)).fetchone()
            if owner and owner[0] != iid:
                raise ApiError(409, "gpu_identity_conflict", "this physical GPU is already registered by another installation")
            if db.execute("SELECT 1 FROM gpu_slots WHERE installation_id=? AND gpu_uuid<>?", (iid, g["uuid"])).fetchone():
                raise ApiError(409, "gpu_identity_conflict", "this installation already registered a different GPU")
            if db.execute("SELECT 1 FROM attempts a JOIN gpu_slots g ON g.slot_id=a.slot_id WHERE g.gpu_uuid=? AND g.installation_id<>? "
                          "AND a.state IN ('offered','started')", (g["uuid"], iid)).fetchone():
                raise ApiError(409, "gpu_identity_conflict", "a previous installation still has an active attempt on this GPU; abandon it first")
            db.execute("DELETE FROM gpu_slots WHERE gpu_uuid=? AND installation_id<>?", (g["uuid"], iid))
            db.execute("INSERT INTO gpu_slots VALUES(?,?,?,?,?,?,1) ON CONFLICT(slot_id) DO UPDATE SET name=excluded.name,"
                       "vram_total_mb=excluded.vram_total_mb, ram_total_mb=excluded.ram_total_mb",
                       (slot, iid, g["uuid"], g["name"], g["vram_total_mb"], g["ram_total_mb"]))
            if "name" in body:
                db.execute("UPDATE installations SET name=? WHERE id=?", (body["name"], iid))
            started = cur["started_at"] if cur and cur["session_id"] == body["session_id"] else now
            db.execute("INSERT OR REPLACE INTO sessions VALUES(?,?,?,?,?)", (iid, body["session_id"], body["build"], started, now))
        return {"installation_id": iid, "session_id": body["session_id"], "slot_id": slot, "protocol": PROTOCOL}

    # ---- submission -------------------------------------------------------------------------------

    def submit(self, inst, body):
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
            if target and not db.execute("SELECT 1 FROM installations WHERE id=? AND revoked_at IS NULL", (target,)).fetchone():
                bad("target installation does not exist")
            if db.execute("SELECT count(*) FROM jobs WHERE state IN ('queued','offered','started')").fetchone()[0] >= MAX_ACTIVE_JOBS:
                raise ApiError(429, "queue_full", "too many active jobs")
            now, jid = self.clock(), new_id("job")
            db.execute("INSERT INTO jobs(job_id,submitter,target,spec_json,roots_json,models_json,state,created_at,updated_at)"
                       " VALUES(?,?,?,?,?,?,'queued',?,?)",
                       (jid, inst["id"], target, canonical(body["spec"]), json.dumps(info["roots"]), canonical(info["models"]), now, now))
            resp = {"job_id": jid, "state": "queued"}
            db.execute("INSERT INTO submissions VALUES(?,?,?,?)", (sid, inst["id"], phash, canonical(resp)))
        return resp

    # ---- claim / reject / start -------------------------------------------------------------------

    @staticmethod
    def _skip_reason(job, cap):
        """Why a receiver with these capabilities cannot take this queued job (None = eligible). Concrete, never a
        silent downgrade: the first mismatch is named."""
        kind = json.loads(job["spec_json"]).get("kind")
        if kind not in cap["kinds"]:
            return f"does not support {kind!r} jobs (older build?)"
        missing = sorted({n.lower() for n in json.loads(job["roots_json"])} - {n.lower() for n in cap["maps"]})
        if missing:
            return f"no path mapping for {', '.join(missing)}"
        for n, d in json.loads(job["models_json"]).items():
            if n not in cap["models"]:
                return f"model {n} is not installed"
            if d is not None and cap["models"][n] != d:
                return f"model {n} differs (different digest)"
        return None

    def _offer(self, db, att):
        job = db.execute("SELECT spec_json, cancel_requested FROM jobs WHERE job_id=?", (att["job_id"],)).fetchone()
        return {"attempt_id": att["attempt_id"], "job_id": att["job_id"], "state": att["state"],
                "cancel_requested": bool(job["cancel_requested"]), "spec": json.loads(job["spec_json"])}

    def claim(self, inst, body):
        validate_claim(body)
        iid, cap = inst["id"], body["capabilities"]
        rhash = digest({k: v for k, v in body.items() if k != "claim_id"})
        now = self.clock()
        self._caps[iid] = (self.clock(), cap)
        with self._tx() as db:
            self._session(db, iid, body["session_id"])
            prior = db.execute("SELECT * FROM claims WHERE claim_id=?", (body["claim_id"],)).fetchone()
            if prior:
                if prior["installation_id"] != iid or prior["request_hash"] != rhash:
                    raise ApiError(409, "claim_conflict", "claim_id was already used with a different request")
                att = db.execute("SELECT * FROM attempts WHERE attempt_id=?", (prior["attempt_id"],)).fetchone()
                return {"offer": self._offer(db, att)}
            if body["resources"] and abs(body["resources"]["sampled_at"] - now) > 120:
                bad("resources.sampled_at is not fresh; resample before claiming", "stale_resources")
            slot = db.execute("SELECT * FROM gpu_slots WHERE installation_id=? AND enabled=1", (iid,)).fetchone()
            if not slot:
                return {"offer": None, "reason": "no_enabled_gpu"}
            live = db.execute("SELECT * FROM attempts WHERE slot_id=? AND state IN ('offered','started')", (slot["slot_id"],)).fetchone()
            if live:
                if live["state"] == "started":
                    return {"offer": None, "reason": "slot_busy"}
                # Lost the earlier response: hand the same pending offer to the new claim id.
                db.execute("INSERT INTO claims VALUES(?,?,?,?)", (body["claim_id"], iid, rhash, live["attempt_id"]))
                return {"offer": self._offer(db, live)}
            # Targeting and rejection filters are in SQL and the queue is bounded by MAX_ACTIVE_JOBS, so no job can be starved
            # by a long run of ineligible ones ahead of it.
            for job in db.execute("SELECT * FROM jobs j WHERE state='queued' AND (target IS NULL OR target=?) AND NOT EXISTS("
                                  "SELECT 1 FROM job_rejections r WHERE r.job_id=j.job_id AND r.installation_id=?) ORDER BY seq",
                                  (iid, iid)):
                if self._skip_reason(job, cap):
                    continue
                aid = new_id("att")
                res = canonical(body["resources"]) if body["resources"] else None
                db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at,resources_json)"
                           " VALUES(?,?,?,?,'offered',?,?)", (aid, job["job_id"], iid, slot["slot_id"], now, res))
                db.execute("UPDATE jobs SET state='offered', attempt_id=?, updated_at=? WHERE job_id=? AND state='queued'",
                           (aid, now, job["job_id"]))
                db.execute("INSERT INTO claims VALUES(?,?,?,?)", (body["claim_id"], iid, rhash, aid))
                return {"offer": self._offer(db, db.execute("SELECT * FROM attempts WHERE attempt_id=?", (aid,)).fetchone())}
        return {"offer": None, "reason": "no_eligible_job"}

    def _attempt(self, db, inst, attempt_id, session_id):
        self._session(db, inst["id"], session_id)
        att = db.execute("SELECT * FROM attempts WHERE attempt_id=?", (attempt_id,)).fetchone()
        if not att or att["installation_id"] != inst["id"]:  # same answer for unknown and foreign ids
            raise ApiError(404, "unknown_attempt", "no such attempt for this installation")
        return att

    def reject(self, inst, attempt_id, body):
        obj(body, "reject", ("session_id", "code", "message"))
        choice(body["code"], "code", REJECT_CODES)
        text(body["message"], "message", 1024, 0)
        with self._tx() as db:
            att = self._attempt(db, inst, attempt_id, body["session_id"])
            if att["state"] != "offered":
                raise ApiError(409, "attempt_not_offered", f"attempt is {att['state']}; only an offered attempt can be rejected")
            now = self.clock()
            reason = f"{body['code']}: {body['message']}"
            db.execute("UPDATE attempts SET state='rejected', ended_at=?, reason=? WHERE attempt_id=?", (now, reason, attempt_id))
            job = db.execute("SELECT * FROM jobs WHERE job_id=?", (att["job_id"],)).fetchone()
            if job["target"] is None and not job["cancel_requested"]:  # never started: safe to try another client
                if body["code"] != "busy":  # busy is this moment's state, not a property of the receiver
                    db.execute("INSERT OR IGNORE INTO job_rejections VALUES(?,?)", (att["job_id"], inst["id"]))
                db.execute("UPDATE jobs SET state='queued', attempt_id=NULL, updated_at=? WHERE job_id=?", (now, att["job_id"]))
            else:
                db.execute("UPDATE jobs SET state='failed', reason=?, updated_at=? WHERE job_id=?", (reason, now, att["job_id"]))
        return {"state": "rejected"}

    def start(self, inst, attempt_id, body):
        obj(body, "start", ("session_id", "effective_plan"))
        plan = body["effective_plan"]
        if not isinstance(plan, dict) or len(canonical(plan)) > 8192:
            bad("effective_plan must be an object of at most 8 KiB (receiver-reported, not validated by the server)")
        h = digest(plan)
        with self._tx() as db:
            att = self._attempt(db, inst, attempt_id, body["session_id"])
            if att["state"] == "started" and att["start_hash"] == h:
                return self._ctl(db, att)
            if att["state"] != "offered":
                raise ApiError(409, "attempt_not_offered", f"attempt is {att['state']}; it can no longer be started")
            now = self.clock()
            db.execute("UPDATE attempts SET state='started', started_at=?, start_hash=?, effective_plan_json=? "
                       "WHERE attempt_id=? AND state='offered'", (now, h, canonical(plan), attempt_id))
            db.execute("UPDATE jobs SET state='started', updated_at=? WHERE job_id=?", (now, att["job_id"]))
            return self._ctl(db, att)

    def _ctl(self, db, att):
        job = db.execute("SELECT cancel_requested FROM jobs WHERE job_id=?", (att["job_id"],)).fetchone()
        cur = db.execute("SELECT state, last_seq FROM attempts WHERE attempt_id=?", (att["attempt_id"],)).fetchone()
        return {"state": cur["state"], "last_seq": cur["last_seq"], "cancel_requested": bool(job["cancel_requested"])}

    # ---- events -----------------------------------------------------------------------------------

    def report(self, inst, attempt_id, body):
        obj(body, "events", ("session_id", "events"))
        evs = body["events"]
        if not isinstance(evs, list) or len(evs) > MAX_BATCH:
            bad(f"events must be a list of at most {MAX_BATCH}")
        for e in evs:
            validate_event(e)
        with self._tx() as db:
            att = self._attempt(db, inst, attempt_id, body["session_id"])
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
                view["ineligible"] = self._ineligible(db, job)
            return view

    def _ineligible(self, db, job):
        """Per paired installation that has claimed recently: why it cannot take this queued job. An installation that
        never claimed (receiving off, or no capabilities reported) is simply not listed."""
        out, now = [], self.clock()
        for r in db.execute("SELECT i.id, i.name FROM installations i JOIN gpu_slots g ON g.installation_id=i.id AND g.enabled=1 "
                            "WHERE i.revoked_at IS NULL AND (? IS NULL OR i.id=?) AND NOT EXISTS("
                            "SELECT 1 FROM job_rejections x WHERE x.job_id=? AND x.installation_id=i.id)",
                            (job["target"], job["target"], job["job_id"])).fetchall():
            seen = self._caps.get(r["id"])
            if not seen or now - seen[0] > SESSION_TTL:
                continue
            why = self._skip_reason(job, seen[1])
            if why:
                out.append({"installation_id": r["id"], "name": r["name"], "reason": why})
        return out

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
            out.append({"installation_id": r["id"], "name": r["name"], "gpu_slot": r["slot_id"], "gpu_name": r["gpu"],
                        "connected": r["last_seen"] is not None and now - r["last_seen"] < SESSION_TTL,
                        "last_seen_age_s": None if r["last_seen"] is None else round(now - r["last_seen"], 3),
                        "stats": None if not live else {**live["sample"], "measured_age_s": round(now - live["measured_at"], 3),
                                                        "received_age_s": round(now - live["received_at"], 3)}})
        return {"installations": out}
