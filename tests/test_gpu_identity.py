"""Verified GPU identity in the worker hello: derived from the opened CUDA device, cross-checked by uuid with NVML,
never from an index or name+memory. Fake torch/NVML only: no CUDA, no GPU."""
import os
import sys
import types

import pytest

from backend.worker import gpu_identity as gi
from backend.worker import probe_hello

@pytest.fixture(autouse=True)
def no_real_nvml(monkeypatch):
    """Every route is faked: a real NVML import/call fails the test instead of reading the host."""
    def boom(*a, **k):
        raise AssertionError("real NVML was used")
    monkeypatch.setattr(gi, "nvml_uuids", boom)
    poison = types.ModuleType("pynvml")
    poison.nvmlInit = boom
    monkeypatch.setitem(sys.modules, "pynvml", poison)


_REAL_NVML_LISTING = gi.nvml_uuids  # the function under test in the pynvml-fake test; every other use is poisoned
A = "GPU-11111111-2222-3333-4444-555555555555"
B = "GPU-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"


class CUuuid:
    """Like torch 2.9.1's `_CUuuid`: NOT a str; str() gives the uuid."""
    def __init__(self, v): self.v = v
    def __str__(self): return self.v


class Props:
    def __init__(self, name="RTX Same", mem=24 << 30, **kw):
        self.name, self.total_memory = name, mem
        self.__dict__.update(kw)


# ---- canonical form ---------------------------------------------------------------------------------

@pytest.mark.parametrize("raw,want", [
    (A, A), (A[4:], A), (B.upper().replace("GPU-", "GPU-"), B), (A[4:].upper(), A),
    ("GPU-" + A[4:].upper(), A),
])
def test_canonical_forms(raw, want):
    assert gi.canonical_uuid(raw) == want


@pytest.mark.parametrize("raw", [None, "", 5, b"x", "MIG-11111111-2222-3333-4444-555555555555", "GPU-123", A + "0", " " + A,
                                 A[4:].replace("-", ""), "gpu-" + A[4:] + "\n", "GPU-GGGGGGGG-2222-3333-4444-555555555555"])
def test_malformed_is_none(raw):
    assert gi.canonical_uuid(raw) is None


# ---- device_identity (fail closed: verified needs NVML to enumerate the uuid) -----------------------------

def test_verified_when_nvml_enumerates_it_in_any_order_and_object_or_str_uuid():
    for raw in (A[4:], A, CUuuid(A), CUuuid(A[4:].upper())):
        r = gi.device_identity(Props(uuid=raw), nvml=lambda: [B, A])
        assert r == {"uuid": A, "uuid_status": "verified", "uuid_reason": None}, raw


@pytest.mark.parametrize("known,why", [(None, "NVML is unavailable"), ([], "lists no GPUs"), ([B], "not among the physical")])
def test_without_a_matching_physical_nvml_gpu_it_is_unverified_with_the_right_reason(known, why):
    r = gi.device_identity(Props(uuid=A), nvml=lambda: known)
    assert r["uuid"] is None and r["uuid_status"] == "unverified" and why in r["uuid_reason"]
    assert "mismatch" not in r["uuid_reason"] and r["uuid_reason"]


def test_bare_hex_mig_uuid_is_unverified_without_nvml_and_refused_with_it():
    mig = "abcdef01-2345-6789-abcd-ef0123456789"  # torch reports MIG instances as bare hex (no MIG- prefix)
    assert gi.device_identity(Props(uuid=CUuuid(mig)), nvml=lambda: None)["uuid"] is None
    r = gi.device_identity(Props(uuid=CUuuid(mig)), nvml=lambda: [A, B])  # not a physical GPU NVML lists
    assert r["uuid"] is None and "not among the physical" in r["uuid_reason"]


def test_explicitly_visible_mig_is_always_refused_even_for_canonical_props(monkeypatch):
    monkeypatch.setenv("CUDA_VISIBLE_DEVICES", "MIG-" + A[4:])
    r = gi.device_identity(Props(uuid=A), nvml=lambda: [A])
    assert r["uuid"] is None and "MIG" in r["uuid_reason"]
    monkeypatch.setenv("CUDA_VISIBLE_DEVICES", "mig-gpu-1111/1/0")
    assert gi.device_identity(Props(uuid=A), nvml=lambda: [A])["uuid"] is None


@pytest.mark.parametrize("props,why", [
    (Props(), "does not expose"), (Props(uuid="not-a-uuid"), "malformed"), (Props(uuid=""), "malformed"), (Props(uuid=7), "malformed"),
    (Props(uuid="MIG-" + A[4:]), "malformed"),
])
def test_unverified_never_falls_back_to_name_and_memory(props, why):
    r = gi.device_identity(props, nvml=lambda: [A])
    assert r["uuid"] is None and r["uuid_status"] == "unverified" and why in r["uuid_reason"]


def test_default_nvml_is_resolved_at_call_time_and_the_fake_is_the_one_called(monkeypatch):
    calls = []
    monkeypatch.setattr(gi, "nvml_uuids", lambda: calls.append(1) or [A])
    assert gi.device_identity(Props(uuid=A))["uuid"] == A and calls == [1]
    monkeypatch.setattr(gi, "nvml_uuids", lambda: (_ for _ in ()).throw(AssertionError("real NVML was used")))
    with pytest.raises(AssertionError):  # and the poison really would fire if the default had been bound early
        gi.device_identity(Props(uuid=A))


def test_nvml_listing_uses_pynvml_canonically(monkeypatch):
    class N:
        @staticmethod
        def nvmlInit(): pass
        @staticmethod
        def nvmlShutdown(): pass
        @staticmethod
        def nvmlDeviceGetCount(): return 2
        @staticmethod
        def nvmlDeviceGetHandleByIndex(i): return i
        @staticmethod
        def nvmlDeviceGetUUID(h): return [A.upper().replace("GPU-", "GPU-").encode(), B][h]
    monkeypatch.setitem(sys.modules, "pynvml", N)  # a fake module: no host NVML is touched
    assert _REAL_NVML_LISTING() == [A, B]

    class Boom(N):
        @staticmethod
        def nvmlInit(): raise RuntimeError("no driver")
    monkeypatch.setitem(sys.modules, "pynvml", Boom)
    assert _REAL_NVML_LISTING() is None


# ---- probe_hello with fake torch: --gpu remap, inherited visibility, same-name GPUs -----------------------

class FakeTorch(types.ModuleType):
    """Physical devices; CUDA_VISIBLE_DEVICES (a comma list of physical ordinals) remaps what torch calls ordinal 0..n."""

    def __init__(self, physical):
        super().__init__("torch")
        self.physical = physical
        self.cuda = types.SimpleNamespace(is_available=lambda: True, current_device=lambda: 0, get_device_properties=self._props)

    def _visible(self):
        v = os.environ.get("CUDA_VISIBLE_DEVICES")
        return list(range(len(self.physical))) if v is None else [int(x) for x in v.split(",")]

    def _props(self, idx):
        return self.physical[self._visible()[idx]]


@pytest.fixture
def nvml_reversed(monkeypatch):
    calls = []
    monkeypatch.setattr(gi, "nvml_uuids", lambda: calls.append(1) or [B, A])  # NVML index 0 is B while CUDA ordinal 0 is A
    monkeypatch.delenv("CUDA_VISIBLE_DEVICES", raising=False)
    return calls


def hello(monkeypatch, physical, gpu_arg, visible=None):
    monkeypatch.setitem(sys.modules, "torch", FakeTorch(physical))
    if visible is not None:
        monkeypatch.setenv("CUDA_VISIBLE_DEVICES", visible)  # what main() does for --gpu N
    return probe_hello(gpu_arg)


def test_gpu_arg_remap_reports_the_opened_device_not_the_nvml_index(monkeypatch, nvml_reversed):
    calls = nvml_reversed
    phys = [Props(uuid=CUuuid(A)), Props(uuid=CUuuid(B))]  # same name and memory; torch's object type
    h = hello(monkeypatch, phys, gpu_arg=1, visible="1")
    g = h["gpu"]
    assert (g["index"], g["uuid"], g["uuid_status"]) == (1, B, "verified")  # NVML[1] would have been A
    h0 = hello(monkeypatch, phys, gpu_arg=0, visible="0")
    assert h0["gpu"]["uuid"] == A and h0["gpu"]["name"] == h["gpu"]["name"] and h0["gpu"]["vram_gb"] == h["gpu"]["vram_gb"]
    assert calls, "the fake NVML (never the host's) was consulted"


def test_inherited_visibility_without_gpu_arg(monkeypatch, nvml_reversed):
    phys = [Props(uuid=A), Props(uuid=B)]
    assert hello(monkeypatch, phys, None)["gpu"]["uuid"] == A  # first visible device
    h = hello(monkeypatch, phys, None, visible="1")  # inherited CUDA_VISIBLE_DEVICES=1
    assert h["gpu"]["uuid"] == B and h["gpu"]["index"] == 0  # index stays torch's ordinal when no --gpu was given


def test_unusable_identity_keeps_the_worker_usable_with_a_reason(monkeypatch, nvml_reversed):
    for props in (Props(), Props(uuid="junk"), Props(uuid="MIG-" + A[4:])):
        h = hello(monkeypatch, [props], gpu_arg=0, visible="0")
        g = h["gpu"]
        assert g["uuid"] is None and g["uuid_status"] == "unverified" and g["uuid_reason"]
        assert g["name"] and g["vram_gb"] == 24.0 and h["capabilities"]["key"] in (True, False)  # still a usable local GPU


def test_identity_probe_crash_does_not_fail_the_hello(monkeypatch, nvml_reversed):
    import backend.worker.gpu_identity as m
    monkeypatch.setattr(m, "device_identity", lambda p: (_ for _ in ()).throw(RuntimeError("boom")))
    g = hello(monkeypatch, [Props(uuid=A)], gpu_arg=0, visible="0")["gpu"]
    assert g["uuid"] is None and g["uuid_status"] == "unverified" and "RuntimeError" in g["uuid_reason"]


def test_no_cuda_means_no_gpu_object(monkeypatch):
    t = FakeTorch([Props(uuid=A)])
    t.cuda.is_available = lambda: False
    monkeypatch.setitem(sys.modules, "torch", t)
    assert probe_hello(None)["gpu"] is None
