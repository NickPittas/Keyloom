"""A1: the app's durable inbox and in-process endpoint (backend/app_api.py + the coordinator store it sits on)."""
import json
import socket

import pytest

from backend import app_api, discovery
from backend.coordinator import pki
from backend.coordinator.client import Client, CoordinatorError, join_to_file
from backend.coordinator.schema import ApiError
from backend.coordinator.store import SESSION_TTL, Store
from tests.coordinator.conftest import Clock, gpu, make_inst, sid, spec
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


def test_offline_registered_pc_is_kept_and_online_again_after_contact(tmp_path):
    clock = Clock()
    s = Store(tmp_path / "c.sqlite3", clock)
    pc, bearer = make_inst(s, "kamino")  # its login call is a contact
    assert s.local_installations(None)["installations"][0]["connected"]
    clock.t += SESSION_TTL * 100  # offline for ages, with work queued in between
    push(s, make_inst(s, "sender")[0], 1)
    s.inbox_pending("x")
    rows = {r["installation_id"]: r for r in s.local_installations(None)["installations"]}
    assert rows[pc["id"]]["connected"] is False and s.installation_active(pc["id"])  # offline is a flag, never a removal
    assert s.authenticate(bearer)["id"] == pc["id"]  # still able to log in when it returns ...
    rows = {r["installation_id"]: r for r in s.local_installations(None)["installations"]}
    assert rows[pc["id"]]["connected"] is True  # ... and shown online again from that contact
    s.close()


def test_online_comes_from_contact_or_the_app_publishing_never_from_a_session(tmp_path):
    clock = Clock()
    s = Store(tmp_path / "c.sqlite3", clock)
    host, _ = make_inst(s, "host")
    never = s.join("never called")[0]
    clock.t += SESSION_TTL * 2
    assert not s.session_online(host["id"]) and not s.session_online(never)
    s.publish_self(host["id"], name="Studio", gpu=gpu(), stats={"seq": 1, "measured_age_ms": 0, "cpu_percent": 2.0})
    me = {i["installation_id"]: i for i in s.installations(host)["installations"]}
    assert me[host["id"]]["connected"] and me[host["id"]]["name"] == "Studio" and me[host["id"]]["stats"]["cpu_percent"] == 2.0
    assert me[never]["connected"] is False and me[never]["gpu_name"] is None
    with pytest.raises(ApiError):  # only a registered PC can be published
        s.publish_self("inst_unknown")
    s.close()


def _old_layout(path):
    """A schema-2 database with one queued shot, one offered (unstarted) and one started shot."""
    clock = Clock()
    s = Store(path, clock)
    sub, sub_tok = make_inst(s, "sender")
    rec, rec_tok = make_inst(s, "receiver")
    ids = [push(s, sub, n, clip_id=f"c{n}")["job_id"] for n in (1, 2, 3)]
    s.db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at,started_at,start_hash,effective_plan_json)"
                 " VALUES(?,?,?,?,'started',?,?,?,?)", ("att_" + "8" * 32, ids[0], rec["id"], f"{rec['id']}:slot", clock.t, clock.t, "h", '{"x": 1}'))
    s.db.execute("UPDATE jobs SET state='started', attempt_id=? WHERE job_id=?", ("att_" + "8" * 32, ids[0]))  # job 1 started (uncertain after restart)
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
    # Another PC's started shot is listed only as a foreign one (it is shown as held with that PC's name), never as this PC's own.
    assert [(h["attempt_id"], h.get("foreign"), h.get("pc")) for h in s.inbox_held(host["id"])] == [(attempt, True, "other")]
    assert [h["attempt_id"] for h in s.inbox_held(other["id"]) if "foreign" not in h] == [attempt]
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
        rows = ep.registered()
        assert {r["name"] for r in rows} >= {"sender"} and len([r for r in rows if r["is_this_pc"]]) == 1  # the app registered itself once
        assert c.hello()["receiver_online"] is True  # the app answers: its receiver is on
        ep.configure(receive=False)  # Receive jobs off: a push is refused, nothing is stored
        with pytest.raises(CoordinatorError) as refused:
            c.submit(sid(2), SPEC)
        assert refused.value.code == "not_receiving" and ep.store.db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1
        ep.configure(receive=True)
        ep.publish(name="Studio PC", gpu=gpu(), stats={"seq": 1, "measured_age_ms": 0, "cpu_percent": 1.0})  # this PC's own entry, in-process
        mine = [i for i in c.installations()["installations"] if i["name"] == "Studio PC"]
        assert mine and mine[0]["connected"] and mine[0]["gpu_name"] == "RTX" and mine[0]["stats"]["cpu_percent"] == 1.0
        # Retire: never this PC itself, and not a login that still has an unfinished shot (the sender's is queued)
        own_id = next(r["installation_id"] for r in ep.registered() if r["is_this_pc"])
        sender_id = next(r["installation_id"] for r in ep.registered() if r["name"] == "sender")
        with pytest.raises(app_api.EndpointError) as own:
            ep.retire(own_id)
        assert own.value.code == "cannot_retire_self"
        with pytest.raises(ApiError) as busy:
            ep.retire(sender_id)
        assert busy.value.code == "in_use"
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


# ---- B3 fixes: old shots for another PC, reverse publication, Automatic only on a free PC ---------------------

def _old_foreign_layout(path):
    """A schema-2 database that served several PCs: three shots aimed at ANOTHER registered PC (kamino-3): one started there (uncertain),
    one offered (not started) and one queued. The app that opens it is the host (`inst_host`), not kamino-3."""
    clock = Clock()
    s = Store(path, clock)
    sub, _ = make_inst(s, "sender")
    kam, _ = make_inst(s, "kamino-3")
    ids = [s.submit(sub, {"submission_id": sid(n), "spec": spec(clip_id=f"c{n}"), "target": kam["id"]})["job_id"] for n in (1, 2, 3)]
    att = "att_" + "7" * 32
    s.db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at,started_at,start_hash,effective_plan_json)"
                 " VALUES(?,?,?,?,'started',?,?,?,?)", (att, ids[0], kam["id"], f"{kam['id']}:slot", clock.t, clock.t, "h", '{"x": 1}'))
    s.db.execute("UPDATE jobs SET state='started', attempt_id=? WHERE job_id=?", (att, ids[0]))
    s.db.execute("INSERT INTO attempts(attempt_id,job_id,installation_id,slot_id,state,offered_at) VALUES(?,?,?,?,'offered',?)",
                 ("att_" + "6" * 32, ids[1], kam["id"], f"{kam['id']}:other", clock.t))
    s.db.execute("UPDATE jobs SET state='offered', attempt_id=? WHERE job_id=?", ("att_" + "6" * 32, ids[1]))
    s.db.execute("DROP TABLE inbox_meta")
    s.db.execute("DROP TABLE destinations")
    s.db.execute("PRAGMA user_version=2")
    s.close()
    return ids, att, sub, kam


def test_old_shots_for_another_pc_are_moved_not_stranded_and_started_ones_are_held(tmp_path):
    ids, att, sub, kam = _old_foreign_layout(tmp_path / "coordinator.sqlite3")
    s = app_api.open_inbox(tmp_path)
    host = "inst_host"
    assert s.inbox_pending(host) == []  # they are not this PC's shots ...
    rows = {r["job_id"]: r for r in s.inbox_foreign(host)}  # ... they are listed to be sent to their PC's app (the offer is queued again)
    assert set(rows) == {ids[1], ids[2]}
    assert rows[ids[1]]["target"] == kam["id"] and rows[ids[1]]["submission_id"] == sid(2) and rows[ids[2]]["submission_id"] == sid(3)
    held = s.inbox_held(host)  # the started one is held for the user with the PC's name, never re-run
    assert [(h["job_id"], h["attempt_id"], h["foreign"], h["pc"]) for h in held] == [(ids[0], att, True, "kamino-3")]
    assert s.db.execute("SELECT state FROM jobs WHERE job_id=?", (ids[0],)).fetchone()[0] == "started"
    # The target PC's app accepted one: it is closed here; a retry of the same submission makes no second shot.
    assert s.inbox_forwarded(ids[1], "Sent to kamino-3 by the upgrade as job job_x")["state"] == "forwarded"
    assert [r["job_id"] for r in s.inbox_foreign(host)] == [ids[2]]
    assert s.job_view(sub, ids[1])["state"] == "forwarded"
    again = s.submit(sub, {"submission_id": sid(2), "spec": spec(clip_id="c2"), "target": kam["id"]})
    assert again["job_id"] == ids[1] and s.db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 3
    # Clear: failed for good, still never run; the registration can be retired afterwards.
    s.abandon(att, "cleared by the user")
    assert s.inbox_held(host) == [] and s.db.execute("SELECT state FROM jobs WHERE job_id=?", (ids[0],)).fetchone()[0] == "failed"
    s.close()


def _free(s, host, **over):
    s.publish_self(host["id"], avail={"receiving": True, "busy": False, "held": False, **over})


def test_automatic_needs_a_receiving_idle_unheld_pc_and_a_race_is_refused_not_free(tmp_path):
    s = Store(tmp_path / "c.sqlite3")
    sub, _ = make_inst(s, "sender")
    host, _ = make_inst(s, "host")
    body = lambda n: {"submission_id": sid(n), "spec": spec(clip_id=f"c{n}"), "target": host["id"]}
    seen = lambda: {i["installation_id"]: i for i in s.installations(sub)["installations"]}[host["id"]]
    with pytest.raises(ApiError) as e:  # nothing published yet: unknown is not free
        s.submit(sub, body(1), only_if_free=True)
    assert e.value.code == "not_free"
    for over, why in (({"receiving": False}, "Receive jobs off"), ({"busy": True}, "local or received work"), ({"held": True}, "a held shot")):
        _free(s, host, **over)
        a = seen()
        assert (a["receiving"], a["busy"], a["held"]) == (over.get("receiving", True), over.get("busy", False), over.get("held", False)), why
        with pytest.raises(ApiError) as e:
            s.submit(sub, body(1), only_if_free=True)
        assert e.value.code == "not_free", why
        assert s.submit(sub, body(1))["state"] == "queued", why  # a PC picked explicitly still queues the shot
        s.db.execute("DELETE FROM jobs")
        s.db.execute("DELETE FROM submissions")
    _free(s, host)
    first = s.submit(sub, body(1), only_if_free=True)  # free: taken
    assert s.submit(sub, body(1), only_if_free=True) == first  # a retry of an accepted shot is never refused
    with pytest.raises(ApiError) as e:  # the race: it just got a shot, so a second Automatic one is refused
        s.submit(sub, body(2), only_if_free=True)
    assert e.value.code == "not_free"
    assert s.db.execute("SELECT count(*) FROM jobs").fetchone()[0] == 1  # nothing was stored for the refused one
    assert s.submit(sub, body(2))["state"] == "queued"  # an explicit pick queues behind the busy PC
    s.close()


def _endpoint(tmp_path, monkeypatch, name):
    class NoDiscovery:
        def __init__(self, *a): pass
        def start(self): return None
        def stop(self): pass
    monkeypatch.setattr(discovery, "Responder", NoDiscovery)
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    sd = tmp_path / name
    sd.mkdir(mode=0o700)
    pki.write_private_new(str(sd / "service.json"), json.dumps({"version": 1, "address": "127.0.0.1", "port": port, "allow_address_join": True}))
    seen = []
    ep = app_api.AppEndpoint(sd, on_commit=lambda *a: seen.append(a)).start()
    return ep, port, seen


def test_automatic_flag_over_http_is_refused_not_free_and_receive_off_is_not_free_too(tmp_path, monkeypatch):
    ep, port, _ = _endpoint(tmp_path, monkeypatch, "state")
    try:
        creds = tmp_path / "creds.json"
        join_to_file(f"https://127.0.0.1:{port}", str(creds), "sender", pem=(ep.sd / "server-cert.pem").read_text())
        c = Client(pki.read_credentials(str(creds)))
        host = ep.live["iid"]
        with pytest.raises(CoordinatorError) as e:
            c.submit(sid(1), SPEC, host, automatic=True)  # the app has not said it is free
        assert e.value.code == "not_free"
        ep.publish(avail={"receiving": True, "busy": False, "held": False})
        row = next(i for i in c.installations()["installations"] if i["installation_id"] == host)
        assert (row["receiving"], row["busy"], row["held"]) == (True, False, False)
        assert c.submit(sid(1), SPEC, host, automatic=True)["state"] == "queued"
        ep.configure(receive=False)
        with pytest.raises(CoordinatorError) as off:  # Receive jobs off: an Automatic shot is not free; an explicit one is not_receiving
            c.submit(sid(2), SPEC, host, automatic=True)
        assert off.value.code == "not_free"
        with pytest.raises(CoordinatorError) as explicit:
            c.submit(sid(3), SPEC, host)
        assert explicit.value.code == "not_receiving"
    finally:
        ep.stop()


def test_a_joined_pc_tells_the_host_its_endpoint_and_the_host_can_push_back(tmp_path, monkeypatch):
    host, host_port, seen = _endpoint(tmp_path, monkeypatch, "host")
    a, a_port, a_seen = _endpoint(tmp_path, monkeypatch, "a")
    try:
        creds_a = tmp_path / "a-on-host.json"  # A is joined to the host (its saved connection)
        join_to_file(f"https://127.0.0.1:{host_port}", str(creds_a), "kamino-3", pem=(host.sd / "server-cert.pem").read_text())
        reg_id = pki.read_credentials(str(creds_a))["installation_id"]
        body = a.destination_offer(str(creds_a))  # A tells the host its endpoint over that authenticated connection
        assert Client(pki.read_credentials(str(creds_a))).destination(body) == {"stored": True}
        assert ("destination", reg_id) in seen
        [d] = host.destinations()
        assert d["pc_id"] == reg_id and d["host_installation_id"] == a.live["iid"] and d["legacy_network_id"] == host.network_id  # the PC id stays
        assert d["fingerprint_sha256"] == a.fp and d["endpoints"] and all(e.endswith(f":{a_port}") for e in d["endpoints"])
        # The host pushes to A with the stored credential: it lands in A's inbox.
        c = Client(pki.read_credentials(d["credentials"]))
        job = c.submit(sid(1), SPEC, a.live["iid"])
        assert [j["job_id"] for j in a.store.inbox_pending(a.live["iid"])] == [job["job_id"]] and ("submitted", job["job_id"]) in a_seen
        # Publishing again (the next app open) keeps the SAME login id for the host (no pile-up); the PC id is unchanged.
        before = a.store.db.execute("SELECT count(*) FROM installations WHERE revoked_at IS NULL").fetchone()[0]
        Client(pki.read_credentials(str(creds_a))).destination(a.destination_offer(str(creds_a)))
        assert a.store.db.execute("SELECT count(*) FROM installations WHERE revoked_at IS NULL").fetchone()[0] == before
        [d2] = host.destinations()
        assert d2["pc_id"] == reg_id
        assert c.submit(sid(2), SPEC, a.live["iid"])["state"] == "queued"  # the old token still works until the host uses the new one
        assert Client(pki.read_credentials(d2["credentials"])).submit(sid(3), SPEC, a.live["iid"])["state"] == "queued"
        # Only a PC registered on the host can tell it anything, and only LAN-like addresses are kept.
        with pytest.raises(CoordinatorError) as lan:
            Client(pki.read_credentials(str(creds_a))).destination({**body, "endpoints": ["https://8.8.8.8:8443"], "installation_id": body["installation_id"]})
        assert lan.value.code in ("invalid", "bad_request") or lan.value.status == 400
        host.remove_destination(reg_id)  # the user removed it: it is gone from the list, and the registration is untouched
        assert host.destinations() == [] and host.store.installation_active(reg_id)
    finally:
        a.stop()
        host.stop()


def test_a_hosts_sender_identity_survives_a_receiver_restart_and_republication(tmp_path, monkeypatch):
    host, host_port, _ = _endpoint(tmp_path, monkeypatch, "host")
    a, a_port, _ = _endpoint(tmp_path, monkeypatch, "a")
    try:
        creds_a = tmp_path / "a-on-host.json"
        join_to_file(f"https://127.0.0.1:{host_port}", str(creds_a), "kamino-3", pem=(host.sd / "server-cert.pem").read_text())
        host_client = Client(pki.read_credentials(str(creds_a)))
        offer1 = a.destination_offer(str(creds_a))
        assert a.destination_offer(str(creds_a)) == offer1  # a retried publication reuses the same offer
        host_client.destination(offer1)
        c = Client(pki.read_credentials(host.destinations()[0]["credentials"]))
        job = c.submit(sid(1), SPEC, a.live["iid"])  # the first send uses the first token (which becomes the login)
        a.stop()  # the receiver restarts and publishes again
        a2 = app_api.AppEndpoint(a.sd).start()
        try:
            offer2 = a2.destination_offer(str(creds_a))
            assert offer2["installation_id"] == offer1["installation_id"] and offer2["token"] != offer1["token"]  # same identity, new token
            assert c.job(job["job_id"])["state"] == "queued"  # the old token works until the new one is used
            host_client.destination(offer2)
            c2 = Client(pki.read_credentials(host.destinations()[0]["credentials"]))
            assert c2.job(job["job_id"])["job_id"] == job["job_id"]  # the host still reads its earlier shot with the new token
            assert c2.submit(sid(1), SPEC, a2.live["iid"]) == job  # a retried lost acknowledgment returns the same job
            c2.cancel(job["job_id"])  # and can cancel it
            with pytest.raises(CoordinatorError):  # the old token stopped once the new one was used
                c.job(job["job_id"])
        finally:
            a2.stop()
    finally:
        host.stop()
