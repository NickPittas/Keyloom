"""A1: the app's durable inbox and in-process endpoint (backend/app_api.py + the coordinator store it sits on)."""
import json
import socket

import pytest

from backend import app_api, discovery
from backend.coordinator import pki
from backend.coordinator.client import Client, join_to_file
from backend.coordinator.schema import ApiError
from backend.coordinator.store import SESSION_TTL, Store
from tests.coordinator.conftest import Clock, claim_body, make_inst, session, sid, spec
from tests.coordinator.specs import SPEC


def push(store, sub, n, **over):
    return store.submit(sub, {"submission_id": sid(n), "spec": spec(**over), "target": None})


@pytest.fixture
def inbox(tmp_path):
    seen = []
    s = Store(tmp_path / "c.sqlite3", on_commit=lambda kind, job: seen.append((kind, job)))
    yield s, seen
    s.close()


def ev(seq, kind, data):
    return {"seq": seq, "type": kind, "data": data}


def test_push_is_durable_and_repeat_makes_no_duplicate(tmp_path):
    seen = []
    path = tmp_path / "c.sqlite3"
    s = Store(path, on_commit=lambda *a: seen.append(a))
    sub, _ = make_inst(s, "sender")
    first = push(s, sub, 1)
    assert push(s, sub, 1) == first  # a retry after a lost reply returns the same job
    assert seen == [("submitted", first["job_id"])]  # one commit notice, not two
    with pytest.raises(ApiError) as e:
        push(s, sub, 1, clip_id="other")  # same id, different content
    assert e.value.code == "submission_conflict"
    s.close()
    again = Store(path)  # app restarted: the shot is still there
    assert [j["job_id"] for j in again.inbox_pending("anyone")] == [first["job_id"]]
    assert again.db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1
    again.close()


def test_broken_listener_cannot_undo_a_commit(tmp_path):
    def boom(*a):
        raise RuntimeError("listener bug")
    s = Store(tmp_path / "c.sqlite3", on_commit=boom)
    sub, _ = make_inst(s, "sender")
    job = push(s, sub, 1)
    assert s.inbox_pending("x")[0]["job_id"] == job["job_id"]
    s.close()


def test_busy_receiver_queues_and_runs_in_order(inbox):
    s, _ = inbox
    sub, _ = make_inst(s, "sender")
    host, _ = make_inst(s, "host")
    a, b = push(s, sub, 1)["job_id"], push(s, sub, 2, clip_id="clip-2")["job_id"]
    s.inbox_begin(host["id"], a, "gpu0", {"gpu": 0})
    with pytest.raises(ApiError) as e:  # the worker is busy: the second shot is not started, not rejected
        s.inbox_begin(host["id"], b, "gpu0", {"gpu": 0})
    assert e.value.code == "slot_busy"
    view = s.job_view(sub, b)
    assert view["state"] == "queued" and view["queue_position"] == 1
    assert [j["job_id"] for j in s.inbox_pending(host["id"])] == [b]
    s.inbox_report(host["id"], a, [ev(1, "terminal", {"state": "succeeded"})])
    s.inbox_begin(host["id"], b, "gpu0", {"gpu": 0})  # free again: the queued shot starts
    assert s.job_view(sub, b)["state"] == "started"


def test_status_progress_result_and_cancel(inbox):
    s, seen = inbox
    sub, _ = make_inst(s, "sender")
    host, _ = make_inst(s, "host")
    a, b, c = (push(s, sub, n, clip_id=f"c{n}")["job_id"] for n in (1, 2, 3))
    s.inbox_begin(host["id"], a, "gpu0", {})
    s.inbox_report(host["id"], a, [ev(1, "progress", {"done": 3, "total": 10})])
    v = s.job_view(sub, a)
    assert v["state"] == "started" and v["progress"] == {"done": 3, "total": 10} and "result" not in v
    s.inbox_report(host["id"], a, [ev(2, "terminal", {"state": "succeeded", "message": "ok"})])
    v = s.job_view(sub, a)
    assert v["state"] == "succeeded" and v["result"] == {"state": "succeeded", "message": "ok"}
    assert s.job_events(sub, a)["events"][-1]["type"] == "terminal"
    # a queued shot is cancelled outright; a started one only records the request for the app to act on
    assert s.cancel(sub, b)["state"] == "cancelled"
    s.inbox_begin(host["id"], c, "gpu0", {})
    assert s.cancel(sub, c)["cancel_requested"] and s.inbox_cancel_requested(c)
    assert ("cancelled", b) in seen and ("cancelled", c) in seen
    s.inbox_report(host["id"], c, [ev(1, "terminal", {"state": "cancelled"})])
    assert s.job_view(sub, c)["state"] == "cancelled"
    other, _ = make_inst(s, "other")
    with pytest.raises(ApiError):  # another PC cannot read or cancel this PC's shots
        s.job_view(other, a)


def test_offline_registered_pc_is_kept(tmp_path):
    clock = Clock()
    s = Store(tmp_path / "c.sqlite3", clock)
    pc, bearer = make_inst(s, "kamino")
    s.register(pc, session(pc))
    assert s.local_installations(None)["installations"][0]["connected"]
    clock.t += SESSION_TTL * 100  # offline for ages, with work queued in between
    push(s, make_inst(s, "sender")[0], 1)
    s.inbox_pending("x")
    rows = {r["installation_id"]: r for r in s.local_installations(None)["installations"]}
    assert rows[pc["id"]]["connected"] is False and s.installation_active(pc["id"])
    assert s.authenticate(bearer)["id"] == pc["id"]  # still able to log in when it returns
    s.close()


def _old_layout(path):
    """A schema-2 database with one queued shot, one offered (unstarted) and one started shot."""
    clock = Clock()
    s = Store(path, clock)
    sub, sub_tok = make_inst(s, "sender")
    rec, rec_tok = make_inst(s, "receiver")
    s.register(rec, session(rec))
    ids = [push(s, sub, n, clip_id=f"c{n}")["job_id"] for n in (1, 2, 3)]
    s.claim(rec, claim_body(clock, claim_n="c1"))  # offers job 1
    off = s.db.execute("SELECT attempt_id FROM attempts").fetchone()[0]
    s.start(rec, off, {"session_id": sid("s1"), "effective_plan": {"x": 1}})  # job 1 started (uncertain after restart)
    s.db.execute("UPDATE jobs SET state='queued', attempt_id=NULL WHERE job_id=?", (ids[1],))
    s.db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at) VALUES(?,?,?,?,'offered',?)",
                 ("att_" + "9" * 32, ids[1], rec["id"], f"{rec['id']}:other", clock.t))
    s.db.execute("UPDATE jobs SET state='offered', attempt_id=? WHERE job_id=?", ("att_" + "9" * 32, ids[1]))
    s.db.execute("DROP TABLE inbox_meta")
    s.db.execute("PRAGMA user_version=2")
    s.close()
    return ids, (sub["id"], sub_tok), (rec["id"], rec_tok)


def test_old_inbox_reopens_after_migration(tmp_path):
    path = tmp_path / "coordinator.sqlite3"
    ids, (sub_id, sub_tok), (rec_id, rec_tok) = _old_layout(path)
    s = app_api.open_inbox(tmp_path)
    assert s.db.execute("PRAGMA user_version").fetchone()[0] == 2
    assert (tmp_path / "coordinator.sqlite3.v2.bak").stat().st_size > 0  # backed up before the in-place change
    assert s.db.execute("SELECT value FROM inbox_meta WHERE key='migrated_from'").fetchone()[0] == "2"
    assert {j["job_id"] for j in s.inbox_pending("x")} == {ids[1], ids[2]}  # the unstarted offer is queued again
    assert [h["job_id"] for h in s.inbox_held(rec_id)] == [ids[0]]  # the started shot is held, never replayed
    # Schema-2 service checks still accept and query the upgraded database.
    assert s.db.execute("PRAGMA user_version").fetchone()[0] == 2
    assert s.db.execute("SELECT count(*) FROM installations").fetchone()[0] == 2
    assert s.authenticate(rec_tok)["id"] == rec_id  # logins untouched
    assert s.authenticate(sub_tok)["id"] == sub_id
    s.close()
    s = app_api.open_inbox(tmp_path)  # reopening again is a no-op
    assert s.db.execute("SELECT count(*) FROM inbox_meta").fetchone()[0] == 1
    s.close()


def test_inbox_only_exposes_and_reports_this_pc_attempt(inbox):
    s, _ = inbox
    sub, _ = make_inst(s, "sender")
    host, _ = make_inst(s, "host")
    other, _ = make_inst(s, "other")
    job = push(s, sub, 1)["job_id"]
    attempt = s.inbox_begin(other["id"], job, "gpu0", {})["attempt_id"]
    assert s.inbox_held(host["id"]) == []
    with pytest.raises(ApiError) as e:
        s.inbox_report(host["id"], job, [ev(1, "terminal", {"state": "succeeded"})])
    assert e.value.code == "unknown_job"
    assert s.db.execute("SELECT state FROM attempts WHERE attempt_id=?", (attempt,)).fetchone()[0] == "started"


def test_old_attempt_file_is_read_not_replayed(tmp_path):
    f = tmp_path / "credentials.json.attempt.json"
    f.write_text(json.dumps({"attempt_id": "att_" + "a" * 32, "job_id": "job_" + "b" * 32, "seq": 4, "stages": ["s1"], "meta": {}}))
    assert app_api.read_attempt_file(f) == {"attempt_id": "att_" + "a" * 32, "job_id": "job_" + "b" * 32, "seq": 4, "stages": ["s1"]}
    assert app_api.read_attempt_file(tmp_path / "missing.json") is None
    f.write_text(json.dumps({"attempt_id": "../x", "job_id": "j", "seq": "no", "stages": 1}))
    assert app_api.read_attempt_file(f) is None


def test_endpoint_runs_in_process_accepts_push_and_closes(tmp_path, monkeypatch):
    class NoDiscovery:  # the real responder owns UDP 44554, which a running app on this machine may hold
        def __init__(self, *a): pass
        def start(self): return None
        def stop(self): pass
    monkeypatch.setattr(discovery, "Responder", NoDiscovery)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    sd = tmp_path / "state"
    sd.mkdir(mode=0o700)
    pki.write_private_new(str(sd / "service.json"), json.dumps({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": True}))
    seen = []
    ep = app_api.AppEndpoint(sd, on_commit=lambda *a: seen.append(a)).start()
    try:
        with pytest.raises(app_api.EndpointError) as e:  # a second owner of the same state is refused, not doubled
            app_api.AppEndpoint(sd).start()
        assert e.value.code == "service_conflict"
        creds = tmp_path / "creds.json"
        join_to_file(f"https://127.0.0.1:{port}", str(creds), "sender", pem=(sd / "server-cert.pem").read_text())
        c = Client(pki.read_credentials(str(creds)))
        job = c.submit(sid(1), SPEC)
        assert c.job(job["job_id"])["state"] == "queued" and seen == [("submitted", job["job_id"])]
        assert c.submit(sid(1), SPEC) == job
        assert [r["name"] for r in ep.registered()] == ["sender"]
    finally:
        ep.stop()
    ep.stop()  # idempotent
    with pytest.raises(OSError):
        socket.create_connection(("127.0.0.1", port), 1).close()  # Quit closes the socket
    assert not (sd / "local-join.token").exists()
    again = app_api.AppEndpoint(sd).start()  # the same state reopens with the same identity
    try:
        assert again.fp == pki.fingerprint((sd / "server-cert.pem").read_text())
        assert [j["job_id"] for j in again.store.inbox_pending(None)] == [job["job_id"]]
    finally:
        again.stop()
