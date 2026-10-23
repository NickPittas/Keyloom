"""Durable state machine, driven directly (no network)."""
import copy

import pytest

from backend.coordinator.schema import ApiError
from backend.coordinator.store import SESSION_TTL, Store

from .conftest import CAPS, SPEC, claim_body, make_inst, session, sid, spec


def code(exc):
    return exc.value.code


def setup_pair(store, clock):
    sub, _ = make_inst(store, "submitter")
    rec, _ = make_inst(store, "receiver")
    store.register(rec, session(rec))
    return sub, rec


def test_submit_replay_same_and_different(store, clock):
    sub, _ = setup_pair(store, clock)
    a = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    assert store.submit(sub, {"submission_id": sid(1), "spec": copy.deepcopy(SPEC), "target": None}) == a
    with pytest.raises(ApiError) as e:
        store.submit(sub, {"submission_id": sid(1), "spec": spec(clip_id="other"), "target": None})
    assert e.value.status == 409 and code(e) == "submission_conflict"
    other, _ = make_inst(store, "other")  # another installation cannot hijack the id either
    with pytest.raises(ApiError) as e:
        store.submit(other, {"submission_id": sid(1), "spec": SPEC, "target": None})
    assert e.value.status == 409


def test_frozen_spec_roundtrip(store, clock):
    sub, rec = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    off = store.claim(rec, claim_body(clock))["offer"]
    assert off["spec"] == SPEC and off["spec"]["params"]["temporal_window"] == "1/1"


@pytest.mark.parametrize("mutate,expect", [
    (lambda s: s.update(mode="resume"), "unsupported_mode"),
    (lambda s: s.update(mode="replace"), "unsupported_mode"),
    (lambda s: s.update(kind="hint"), "unsupported_kind"),
    (lambda s: s.update(protocol=2), "unsupported_protocol"),
    (lambda s: s.update(path_map=[["a", "b"]]), "invalid"),
    (lambda s: s.update(clip_root="/mnt/nas/clip"), "invalid"),
    (lambda s: s.update(clip_root="shared:projects/../etc"), "invalid"),
    (lambda s: s.update(clip_root="shared:projects/C:"), "invalid"),
    (lambda s: s.update(manifest_dir="shared:out/NUL"), "invalid"),
    (lambda s: s["params"].update(temporal_window="7/3"), "invalid"),
    (lambda s: s["params"].update(despill_strength=True), "invalid"),
    (lambda s: s["params"].update(despill_strength=1.5), "invalid"),
    (lambda s: s["params"].update(hint_mode="keyframes", use_alpha_hint=True), "invalid"),
    (lambda s: s["output_config"].update(fg_enabled=False, matte_enabled=False), "invalid"),
    (lambda s: s["output_config"].update(comp_enabled=True), "invalid"),  # three outputs, still has {output}; make collision below
    (lambda s: s["range"].update(out_point=-1), "invalid"),
    (lambda s: s["output"].update(file_template="x_{bogus}_##"), "invalid"),
    (lambda s: s["output"].update(folder_template="../up"), "invalid"),
    (lambda s: s["output"].update(folder_template="a", file_template="x_##"), "invalid"),
    (lambda s: s.update(outputs={"fg": ["f1.exr"] * 3}), "invalid"),  # no per-frame arrays
    (lambda s: s.pop("require"), "invalid"),
])
def test_schema_rejects(store, clock, mutate, expect):
    sub, _ = setup_pair(store, clock)
    s = copy.deepcopy(SPEC)
    mutate(s)
    if s["output_config"]["comp_enabled"]:
        s["output"]["folder_template"], s["output"]["file_template"] = "a", "x_##"
    with pytest.raises(ApiError) as e:
        store.submit(sub, {"submission_id": sid(2), "spec": s, "target": None})
    assert e.value.status in (400, 413) and code(e) == expect


def test_claim_retry_same_attempt_and_race_single_owner(store, clock):
    sub, rec = setup_pair(store, clock)
    rec2, _ = make_inst(store, "second")
    store.register(rec2, session(rec2, "s2", "GPU-bbbbbbbb"))
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    a = store.claim(rec, claim_body(clock))["offer"]
    assert store.claim(rec, claim_body(clock))["offer"] == a
    assert store.claim(rec2, claim_body(clock, "s2", "c2"))["offer"] is None
    with pytest.raises(ApiError) as e:  # same claim id, different request
        store.claim(rec, claim_body(clock, caps={**CAPS, "maps": ["out"]}))
    assert code(e) == "claim_conflict"
    again = store.claim(rec, claim_body(clock, claim_n="c9"))["offer"]  # lost response: same pending offer
    assert again["attempt_id"] == a["attempt_id"]
    assert a["attempt_id"] != a["job_id"]


def test_claim_threads_race(store, clock):
    import threading
    sub, _ = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    insts = []
    for i in range(6):
        r, _ = make_inst(store, f"r{i}")
        store.register(r, session(r, f"s{i}", f"GPU-{i:08d}"))
        insts.append((r, i))
    got = []
    ts = [threading.Thread(target=lambda r=r, i=i: got.append(store.claim(r, claim_body(clock, f"s{i}", f"c{i}"))["offer"]))
          for r, i in insts]
    [t.start() for t in ts]
    [t.join() for t in ts]
    assert len([g for g in got if g]) == 1


def test_eligibility_prefilter(store, clock):
    sub, rec = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    for i, caps in enumerate(({**CAPS, "maps": ["out"]}, {**CAPS, "models": {}}, {**CAPS, "kinds": []})):
        assert store.claim(rec, claim_body(clock, claim_n=f"x{i}", caps=caps))["offer"] is None
    assert store.claim(rec, claim_body(clock, claim_n="ok"))["offer"] is not None


def test_stale_resource_snapshot_rejected_but_none_allowed(store, clock):
    sub, rec = setup_pair(store, clock)
    b = claim_body(clock)
    b["resources"]["sampled_at"] -= 1000
    with pytest.raises(ApiError) as e:
        store.claim(rec, b)
    assert code(e) == "stale_resources"
    b["resources"] = None
    assert store.claim(rec, b)["offer"] is None


def test_targeted_job_only_for_target(store, clock):
    sub, rec = setup_pair(store, clock)
    other, _ = make_inst(store, "o")
    store.register(other, session(other, "s2", "GPU-bbbbbbbb"))
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": rec["id"]})
    assert store.claim(other, claim_body(clock, "s2", "c2"))["offer"] is None
    assert store.claim(rec, claim_body(clock))["offer"]


def _started(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    off = store.claim(rec, claim_body(clock))["offer"]
    store.start(rec, off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {"x": 1}})
    return sub, rec, j["job_id"], off["attempt_id"]


def ev(seq, t="progress", data=None):
    return {"seq": seq, "type": t, "data": data or {"done": seq, "total": 10}}


def test_start_after_revoke_forbidden_and_cancel_offered(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    off = store.claim(rec, claim_body(clock))["offer"]
    assert store.cancel(sub, j)["state"] == "cancelled"
    with pytest.raises(ApiError) as e:
        store.start(rec, off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {}})
    assert code(e) == "attempt_not_offered"
    assert store.claim(rec, claim_body(clock, claim_n="c2"))["offer"] is None  # slot freed, job not requeued
    assert store.cancel(sub, j)["state"] == "cancelled"  # idempotent


def test_start_idempotent_and_conflict(store, clock):
    sub, rec, j, a = _started(store, clock)
    body = {"session_id": sid("s1"), "effective_plan": {"x": 1}}
    assert store.start(rec, a, body)["state"] == "started"
    with pytest.raises(ApiError) as e:
        store.start(rec, a, {**body, "effective_plan": {"x": 2}})
    assert code(e) == "attempt_not_offered"


def test_events_ordering_replay_and_conflicts(store, clock):
    sub, rec, j, a = _started(store, clock)
    sess = sid("s1")
    store.report(rec, a, {"session_id": sess, "events": [ev(1), ev(2)]})
    r = store.report(rec, a, {"session_id": sess, "events": [ev(1), ev(2), ev(3)]})  # replay + new
    assert r["last_seq"] == 3
    assert len(store.job_events(sub, j)["events"]) == 3
    with pytest.raises(ApiError) as e:
        store.report(rec, a, {"session_id": sess, "events": [ev(2, data={"done": 9, "total": 10})]})
    assert code(e) == "seq_conflict"
    with pytest.raises(ApiError) as e:
        store.report(rec, a, {"session_id": sess, "events": [ev(5)]})
    assert code(e) == "seq_gap"
    with pytest.raises(ApiError):  # atomic: bad batch leaves nothing behind
        store.report(rec, a, {"session_id": sess, "events": [ev(4), ev(4, "note", {"message": "x"})]})
    assert store.job_view(sub, j)["last_seq"] == 3
    cur = store.job_events(sub, j)["cursor"]
    assert store.job_events(sub, j, after=cur)["events"] == []
    assert len(store.job_events(sub, j, after=1, limit=1)["events"]) == 1


def test_terminal_rules(store, clock):
    sub, rec, j, a = _started(store, clock)
    sess = sid("s1")
    term = ev(1, "terminal", {"state": "succeeded"})
    with pytest.raises(ApiError):  # terminal must be last
        store.report(rec, a, {"session_id": sess, "events": [term, ev(2)]})
    store.report(rec, a, {"session_id": sess, "events": [term]})
    assert store.job_view(sub, j)["state"] == "succeeded"
    store.report(rec, a, {"session_id": sess, "events": [term]})  # replay still fine
    with pytest.raises(ApiError) as e:
        store.report(rec, a, {"session_id": sess, "events": [ev(2)]})
    assert code(e) == "attempt_terminal"
    assert store.cancel(sub, j)["state"] == "succeeded"  # cancel cannot rewrite history


def test_events_before_start_refused(store, clock):
    sub, rec = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    off = store.claim(rec, claim_body(clock))["offer"]
    with pytest.raises(ApiError) as e:
        store.report(rec, off["attempt_id"], {"session_id": sid("s1"), "events": [ev(1)]})
    assert code(e) == "attempt_not_started"


def test_wrong_owner_and_stale_session(store, clock):
    sub, rec, j, a = _started(store, clock)
    thief, _ = make_inst(store, "thief")
    store.register(thief, session(thief, "s9", "GPU-cccccccc"))
    with pytest.raises(ApiError) as e:
        store.report(thief, a, {"session_id": sid("s9"), "events": [ev(1)]})
    assert e.value.status == 404
    with pytest.raises(ApiError) as e:
        store.report(rec, a, {"session_id": sid("zz"), "events": [ev(1)]})
    assert code(e) == "stale_session"


def test_cancel_started_sets_request_only(store, clock):
    sub, rec, j, a = _started(store, clock)
    v = store.cancel(sub, j)
    assert v["state"] == "started" and v["cancel_requested"]
    r = store.report(rec, a, {"session_id": sid("s1"), "events": []})
    assert r["cancel_requested"] and r["state"] == "started"
    store.report(rec, a, {"session_id": sid("s1"), "events": [ev(1, "terminal", {"state": "cancelled"})]})
    assert store.job_view(sub, j)["state"] == "cancelled"


def test_read_cancel_authorization(store, clock):
    sub, rec, j, a = _started(store, clock)
    other, _ = make_inst(store, "other")
    admin, _ = make_inst(store, "admin", admin=True)
    for fn in (store.job_view, store.cancel, store.job_events):
        with pytest.raises(ApiError) as e:
            fn(other, j)
        assert e.value.status == 404
    with pytest.raises(ApiError):
        store.cancel(rec, j)  # the receiver is not the submitter
    assert store.job_view(admin, j)["job_id"] == j
    assert store.cancel(admin, j)["cancel_requested"]


def test_reject_requeues_automatic_and_excludes_installation(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    off = store.claim(rec, claim_body(clock))["offer"]
    store.reject(rec, off["attempt_id"], {"session_id": sid("s1"), "code": "missing_map", "message": "no mount"})
    assert store.job_view(sub, j)["state"] == "queued"
    assert store.claim(rec, claim_body(clock, claim_n="c2"))["offer"] is None
    t = store.submit(sub, {"submission_id": sid(4), "spec": SPEC, "target": rec["id"]})["job_id"]
    off = store.claim(rec, claim_body(clock, claim_n="c3"))["offer"]
    store.reject(rec, off["attempt_id"], {"session_id": sid("s1"), "code": "busy", "message": ""})
    assert store.job_view(sub, t)["state"] == "failed"


def test_restart_leaves_started_fenced(tmp_path, clock):
    path = tmp_path / "d.sqlite3"
    s = Store(path, clock)
    sub, rec = setup_pair(s, clock)
    j = s.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    s.submit(sub, {"submission_id": sid(2), "spec": spec(clip_id="two"), "target": None})
    off = s.claim(rec, claim_body(clock))["offer"]
    s.start(rec, off["attempt_id"], {"session_id": sid("s1"), "effective_plan": {}})
    s.report(rec, off["attempt_id"], {"session_id": sid("s1"), "events": [ev(1)]})
    s.close()
    clock.t += 10 * SESSION_TTL  # long outage: no timer may free the attempt
    s = Store(path, clock)
    v = s.job_view(sub, j)
    assert v["state"] == "started" and v["attempt_id"] == off["attempt_id"] and v["last_seq"] == 1
    # the receiver's slot stays fenced; the second queued job is not handed to it
    s.register(rec, session(rec))
    assert s.claim(rec, claim_body(clock, claim_n="c5"))["offer"] is None
    s.report(rec, off["attempt_id"], {"session_id": sid("s1"), "events": [ev(2)]})
    s.close()


def test_session_conflict_and_takeover(store, clock):
    _, rec = setup_pair(store, clock)
    with pytest.raises(ApiError) as e:
        store.register(rec, session(rec, "s2"))
    assert code(e) == "session_conflict"
    store.register(rec, session(rec))  # same session id: idempotent
    clock.t += SESSION_TTL + 1
    store.register(rec, session(rec, "s2"))


def test_duplicate_gpu_across_installations_and_multi_gpu(store, clock):
    _, rec = setup_pair(store, clock)
    twin, _ = make_inst(store, "second address")
    with pytest.raises(ApiError) as e:
        store.register(twin, session(twin, "s2"))
    assert code(e) == "gpu_identity_conflict"
    store.revoke(rec["id"])  # revoked owner releases the physical GPU
    store.register(twin, session(twin, "s2"))
    s = session(twin, "s3")
    s["gpus"] *= 2
    with pytest.raises(ApiError) as e:
        store.register(twin, s)
    assert code(e) == "unsupported_multi_gpu"


def test_revoked_token_refused(store, clock):
    inst, bearer = make_inst(store)
    store.revoke(inst["id"])
    with pytest.raises(ApiError) as e:
        store.authenticate(bearer)
    assert e.value.status == 401


def test_pairing_one_use_expiry_and_hashes_only(store, clock, tmp_path):
    code_ = store.create_pairing()
    iid, tok = store.pair(code_, "a")
    with pytest.raises(ApiError) as e:
        store.pair(code_, "b")
    assert e.value.status == 403
    old = store.create_pairing(ttl=5)
    clock.t += 6
    with pytest.raises(ApiError):
        store.pair(old, "c")
    raw = open(tmp_path / "c.sqlite3", "rb").read()
    assert tok.encode() not in raw and code_.replace("-", "").encode() not in raw


def test_corrupt_or_newer_db_is_preserved(tmp_path, clock):
    p = tmp_path / "bad.sqlite3"
    p.write_bytes(b"not a database" * 100)
    with pytest.raises(Exception):
        Store(p, clock)
    assert p.read_bytes().startswith(b"not a database")
    q = tmp_path / "new.sqlite3"
    s = Store(q, clock)
    s.db.execute("PRAGMA user_version=99")
    s.close()
    with pytest.raises(RuntimeError):
        Store(q, clock)


def test_schema_matches_backend_constants():
    core = pytest.importorskip("backend.service.core")
    from backend.coordinator import schema
    assert (schema.VRAM_PROFILES, schema.EXR_COMPRESSIONS, schema.OUTPUT_FORMATS, schema.HINT_MODES, schema.TEMPORAL_WINDOWS) == (
        core.VALID_VRAM_PROFILES, core.VALID_EXR_COMPRESSIONS, core.VALID_OUTPUT_FORMATS, core.VALID_HINT_MODES, core.VALID_TEMPORAL_WINDOWS)


def test_stats_latest_only_ages_and_isolation(store, clock):
    sub, rec = setup_pair(store, clock)
    st = {"session_id": sid("s1"), "seq": 5, "measured_age_ms": 2000, "cpu_percent": 12.5, "ram_used_bytes": 1, "ram_total_bytes": 2,
          "gpus": [{"uuid": "GPU-aaaaaaaa", "index": 0, "name": "RTX", "utilization_percent": None, "vram_used_bytes": 5, "vram_total_bytes": 9}]}
    assert store.report_stats(rec, st)["accepted"]
    assert not store.report_stats(rec, {**st, "seq": 4, "cpu_percent": 99})["accepted"]  # older seq never replaces
    clock.t += 30
    me = {i["installation_id"]: i for i in store.installations(sub)["installations"]}
    r = me[rec["id"]]
    assert r["connected"] and r["stats"]["cpu_percent"] == 12.5
    assert r["stats"]["measured_age_s"] == 32 and r["stats"]["received_age_s"] == 30  # client cannot claim freshness
    assert me[sub["id"]]["stats"] is None and not me[sub["id"]]["connected"]  # unreported/never-seen != zero
    clock.t += SESSION_TTL
    assert not {i["installation_id"]: i for i in store.installations(sub)["installations"]}[rec["id"]]["connected"]
    with pytest.raises(ApiError) as e:  # cannot report into someone else's session
        store.report_stats(sub, st)
    assert code(e) == "stale_session"
    # a new session drops the previous session's sample
    store.register(rec, session(rec, "s2"))
    assert {i["installation_id"]: i for i in store.installations(sub)["installations"]}[rec["id"]]["stats"] is None


@pytest.mark.parametrize("patch", [{"cpu_percent": 101}, {"cpu_percent": True}, {"ram_used_bytes": 3}, {"measured_age_ms": 61000},
                                   {"gpus": [{"uuid": "x", "index": 0, "name": "n"}]}, {"extra": 1}, {"seq": 0},
                                   {"gpus": [{"uuid": "GPU-aaaaaaaa", "index": 0, "name": "n", "vram_used_bytes": 9, "vram_total_bytes": 1}]}])
def test_stats_validation(store, clock, patch):
    _, rec = setup_pair(store, clock)
    base = {"session_id": sid("s1"), "seq": 1, "measured_age_ms": 0, "ram_used_bytes": 1, "ram_total_bytes": 2}
    with pytest.raises(ApiError) as e:
        store.report_stats(rec, {**base, **patch})
    assert e.value.status == 400
    store.report_stats(rec, {"session_id": sid("s1"), "seq": 1, "measured_age_ms": 0})  # all metrics optional
