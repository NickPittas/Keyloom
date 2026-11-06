"""Durable state machine, driven directly (no network)."""
import copy

import pytest

from backend.coordinator.schema import ApiError
from backend.coordinator.store import APP_SESSION, SESSION_TTL, Store

from .conftest import SPEC, begin, gpu, make_inst, sid, spec


def code(exc):
    return exc.value.code


def setup_pair(store, clock):
    sub, _ = make_inst(store, "submitter")
    rec, _ = make_inst(store, "receiver")
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


def test_submit_replay_is_allowed_after_receiving_turns_off(store, clock):
    sub, _ = setup_pair(store, clock)
    body = {"submission_id": sid(1), "spec": SPEC, "target": None}
    accepted = store.submit(sub, body, accepting=lambda: True)
    assert store.submit(sub, body, accepting=lambda: False) == accepted
    with pytest.raises(ApiError) as e:
        store.submit(sub, {**body, "spec": spec(clip_id="changed")}, accepting=lambda: False)
    assert code(e) == "submission_conflict"
    with pytest.raises(ApiError) as e:
        store.submit(sub, {**body, "submission_id": sid(2)}, accepting=lambda: False)
    assert code(e) == "not_receiving"
    with pytest.raises(ApiError) as e:
        store.submit(sub, {**body, "submission_id": sid(3)}, only_if_free=True, accepting=lambda: False)
    assert code(e) == "not_free"


def test_retire_unused_rechecks_online_state_inside_store(store, clock):
    rec, _ = make_inst(store, "receiver")
    with pytest.raises(ApiError) as e:
        store.revoke(rec["id"], only_if_unused=True, only_if_offline=True)
    assert code(e) == "online"
    clock.t += SESSION_TTL
    store.revoke(rec["id"], only_if_unused=True, only_if_offline=True)
    assert not store.installation_active(rec["id"])
    active, _ = make_inst(store, "retire-one")
    store.revoke(active["id"], only_if_unused=True)
    assert not store.installation_active(active["id"])


def test_frozen_spec_roundtrip(store, clock):
    sub, rec = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})
    row = store.inbox_pending(rec["id"])[0]
    assert row["spec"] == SPEC and row["spec"]["params"]["temporal_window"] == "1/1"


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


def test_targeted_job_only_for_target(store, clock):
    sub, rec = setup_pair(store, clock)
    other, _ = make_inst(store, "o")
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": rec["id"]})["job_id"]
    assert store.inbox_pending(other["id"]) == []
    with pytest.raises(ApiError) as e:
        begin(store, other, j)
    assert code(e) == "unknown_job"
    assert [r["job_id"] for r in store.inbox_pending(rec["id"])] == [j]


def _started(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    return sub, rec, j, begin(store, rec, j)


def ev(seq, t="progress", data=None):
    return {"seq": seq, "type": t, "data": data or {"done": seq, "total": 10}}


def test_cancel_before_start_ends_the_shot_and_it_can_no_longer_begin(store, clock):
    sub, rec = setup_pair(store, clock)
    j = store.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    assert store.cancel(sub, j)["state"] == "cancelled"
    with pytest.raises(ApiError) as e:
        begin(store, rec, j)
    assert code(e) == "not_queued"
    assert store.inbox_pending(rec["id"]) == []
    assert store.cancel(sub, j)["state"] == "cancelled"  # idempotent


def test_begin_is_once_per_shot_and_one_shot_per_slot(store, clock):
    sub, rec, j, a = _started(store, clock)
    with pytest.raises(ApiError) as e:
        begin(store, rec, j)
    assert code(e) == "not_queued"
    j2 = store.submit(sub, {"submission_id": sid(2), "spec": spec(clip_id="two"), "target": None})["job_id"]
    with pytest.raises(ApiError) as e:
        begin(store, rec, j2)  # same worker slot
    assert code(e) == "slot_busy"
    assert begin(store, rec, j2, slot="gpu1")


def test_events_ordering_replay_and_conflicts(store, clock):
    sub, rec, j, a = _started(store, clock)
    store.inbox_report(rec["id"], j, [ev(1), ev(2)])
    r = store.inbox_report(rec["id"], j, [ev(1), ev(2), ev(3)])  # replay + new
    assert r["last_seq"] == 3
    assert len(store.job_events(sub, j)["events"]) == 3
    with pytest.raises(ApiError) as e:
        store.inbox_report(rec["id"], j, [ev(2, data={"done": 9, "total": 10})])
    assert code(e) == "seq_conflict"
    with pytest.raises(ApiError) as e:
        store.inbox_report(rec["id"], j, [ev(5)])
    assert code(e) == "seq_gap"
    with pytest.raises(ApiError):  # atomic: bad batch leaves nothing behind
        store.inbox_report(rec["id"], j, [ev(4), ev(4, "note", {"message": "x"})])
    assert store.job_view(sub, j)["last_seq"] == 3
    cur = store.job_events(sub, j)["cursor"]
    assert store.job_events(sub, j, after=cur)["events"] == []
    assert len(store.job_events(sub, j, after=1, limit=1)["events"]) == 1


def test_terminal_rules(store, clock):
    sub, rec, j, a = _started(store, clock)
    term = ev(1, "terminal", {"state": "succeeded"})
    with pytest.raises(ApiError):  # terminal must be last
        store.inbox_report(rec["id"], j, [term, ev(2)])
    store.inbox_report(rec["id"], j, [term])
    assert store.job_view(sub, j)["state"] == "succeeded"
    store.inbox_report(rec["id"], j, [term])  # replay still fine
    with pytest.raises(ApiError) as e:
        store.inbox_report(rec["id"], j, [ev(2)])
    assert code(e) == "attempt_terminal"
    assert store.cancel(sub, j)["state"] == "succeeded"  # cancel cannot rewrite history


def test_wrong_owner_cannot_report(store, clock):
    sub, rec, j, a = _started(store, clock)
    thief, _ = make_inst(store, "thief")
    with pytest.raises(ApiError) as e:
        store.inbox_report(thief["id"], j, [ev(1)])
    assert e.value.status == 404


def test_cancel_started_sets_request_only(store, clock):
    sub, rec, j, a = _started(store, clock)
    v = store.cancel(sub, j)
    assert v["state"] == "started" and v["cancel_requested"]
    r = store.inbox_report(rec["id"], j, [])
    assert r["cancel_requested"] and r["state"] == "started" and store.inbox_cancel_requested(j)
    store.inbox_report(rec["id"], j, [ev(1, "terminal", {"state": "cancelled"})])
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


def test_restart_leaves_started_fenced(tmp_path, clock):
    path = tmp_path / "d.sqlite3"
    s = Store(path, clock)
    sub, rec = setup_pair(s, clock)
    j = s.submit(sub, {"submission_id": sid(1), "spec": SPEC, "target": None})["job_id"]
    j2 = s.submit(sub, {"submission_id": sid(2), "spec": spec(clip_id="two"), "target": None})["job_id"]
    a = begin(s, rec, j)
    s.inbox_report(rec["id"], j, [ev(1)])
    s.close()
    clock.t += 10 * SESSION_TTL  # long outage: no timer may free the attempt
    s = Store(path, clock)
    v = s.job_view(sub, j)
    assert v["state"] == "started" and v["attempt_id"] == a and v["last_seq"] == 1
    held = s.inbox_held(rec["id"])
    assert [(h["job_id"], h["last_seq"]) for h in held] == [(j, 1)] and held[0]["plan"] == {"x": 1}
    with pytest.raises(ApiError) as e:  # the worker slot stays fenced: the second queued shot is not started on it
        begin(s, rec, j2)
    assert code(e) == "slot_busy"
    s.inbox_report(rec["id"], j, [ev(2)])  # the app can still settle the first one
    s.close()


def test_duplicate_gpu_across_installations(store, clock):
    _, rec = setup_pair(store, clock)
    store.publish_self(rec["id"], gpu=gpu())
    twin, _ = make_inst(store, "second address")
    with pytest.raises(ApiError) as e:
        store.publish_self(twin["id"], gpu=gpu())
    assert code(e) == "gpu_identity_conflict"
    store.revoke(rec["id"])  # a revoked owner releases the physical GPU
    store.publish_self(twin["id"], gpu=gpu())
    with pytest.raises(ApiError) as e:  # one GPU per installation
        store.publish_self(twin["id"], gpu=gpu("GPU-bbbbbbbb"))
    assert code(e) == "gpu_identity_conflict"


def test_revoked_token_refused(store, clock):
    inst, bearer = make_inst(store)
    store.revoke(inst["id"])
    with pytest.raises(ApiError) as e:
        store.authenticate(bearer)
    assert e.value.status == 401


def test_join_stores_token_hash_only(store, clock, tmp_path):
    iid, tok = store.join("a")
    assert store.authenticate(f"{iid}.{tok}")["id"] == iid
    raw = open(tmp_path / "c.sqlite3", "rb").read()
    assert tok.encode() not in raw


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
    st = {"seq": 5, "measured_age_ms": 2000, "cpu_percent": 12.5, "ram_used_bytes": 1, "ram_total_bytes": 2,
          "gpus": [{"uuid": "GPU-aaaaaaaa", "index": 0, "name": "RTX", "utilization_percent": None, "vram_used_bytes": 5, "vram_total_bytes": 9}]}
    store.publish_self(rec["id"], gpu=gpu(), stats=st)
    store.publish_self(rec["id"], gpu=gpu(), stats={**st, "seq": 4, "cpu_percent": 99})  # an older seq never replaces
    clock.t += 30
    me = {i["installation_id"]: i for i in store.installations(sub)["installations"]}
    r = me[rec["id"]]
    assert r["connected"] and r["stats"]["cpu_percent"] == 12.5 and r["gpu_name"] == "RTX"
    assert r["stats"]["measured_age_s"] == 32 and r["stats"]["received_age_s"] == 30  # the sender cannot claim freshness
    assert me[sub["id"]]["stats"] is None  # unreported != zero
    clock.t += SESSION_TTL
    assert not {i["installation_id"]: i for i in store.installations(sub)["installations"]}[rec["id"]]["connected"]
    with pytest.raises(ApiError) as e:  # a PC without the app's session cannot report load
        store.report_stats(sub, {"session_id": APP_SESSION, **st})
    assert code(e) == "stale_session"


@pytest.mark.parametrize("patch", [{"cpu_percent": 101}, {"cpu_percent": True}, {"ram_used_bytes": 3}, {"measured_age_ms": 61000},
                                   {"gpus": [{"uuid": "x", "index": 0, "name": "n"}]}, {"extra": 1}, {"seq": 0},
                                   {"gpus": [{"uuid": "GPU-aaaaaaaa", "index": 0, "name": "n", "vram_used_bytes": 9, "vram_total_bytes": 1}]}])
def test_stats_validation(store, clock, patch):
    _, rec = setup_pair(store, clock)
    base = {"seq": 1, "measured_age_ms": 0, "ram_used_bytes": 1, "ram_total_bytes": 2}
    with pytest.raises(ApiError) as e:
        store.publish_self(rec["id"], gpu=gpu(), stats={**base, **patch})
    assert e.value.status == 400
    store.publish_self(rec["id"], gpu=gpu(), stats={"seq": 1, "measured_age_ms": 0})  # all metrics optional


def _workflow(**hint):
    s = copy.deepcopy(SPEC)
    s["kind"] = "workflow"
    s["hint"] = {"guide": "sam", "marks_pre": True, "marks_hint": False, **hint}
    s["hint_stages"] = ["sam"]
    return s


def test_workflow_hint_marks_flags_accepted(store, clock):
    sub, _ = setup_pair(store, clock)
    store.submit(sub, {"submission_id": sid(7), "spec": _workflow(), "target": None})


@pytest.mark.parametrize("key", ["marks_pre", "marks_hint"])
def test_workflow_hint_marks_flags_must_be_boolean(store, clock, key):
    sub, _ = setup_pair(store, clock)
    with pytest.raises(ApiError) as e:
        store.submit(sub, {"submission_id": sid(8), "spec": _workflow(**{key: "yes"}), "target": None})
    assert code(e) == "invalid"
