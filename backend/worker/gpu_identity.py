"""Identity of the CUDA device this worker actually opened, as a canonical `GPU-<uuid>` or an explicit reason why not.

Rules (no guessing): the uuid comes from the opened device's CUDA properties (`torch.cuda.get_device_properties(idx).uuid`;
in torch 2.9.1 the stub says `str` but the value is a `_CUuuid` object whose `str()` is the uuid), never from an NVML *index* (CUDA_VISIBLE_DEVICES remaps ordinals; `--gpu 1` is ordinal 0 here).
NVML must enumerate that uuid for the identity to count as verified (physical GPUs only); no NVML = unverified. Name + memory
are never an identity. MIG is unsupported (unverified).
Native GPU/Windows behaviour of the torch property is not verified by tests (they use fakes)."""
import os
import re

_HEX = re.compile(r"(?:GPU-)?([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})\Z")


def canonical_uuid(raw):
    """`GPU-xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx` (lowercase) or None. Accepts the optional GPU- prefix only."""
    m = _HEX.match(raw) if isinstance(raw, str) else None
    return f"GPU-{m.group(1).lower()}" if m else None


def nvml_uuids():
    """Canonical uuids NVML lists, or None when NVML cannot be used (the check is then simply not available)."""
    try:
        import pynvml  # nvidia-ml-py (already a dependency)
        pynvml.nvmlInit()
        try:
            out = []
            for i in range(pynvml.nvmlDeviceGetCount()):
                u = pynvml.nvmlDeviceGetUUID(pynvml.nvmlDeviceGetHandleByIndex(i))
                out.append(canonical_uuid(u.decode() if isinstance(u, bytes) else u) or str(u))
            return out
        finally:
            pynvml.nvmlShutdown()
    except Exception:  # noqa: BLE001 - NVML missing/old driver: no cross-check
        return None


def device_identity(props, nvml=None):
    """-> {"uuid": str|None, "uuid_status": "verified"|"unverified", "uuid_reason": str|None}. `props` = opened device's CUDA properties.

    Fail closed: VERIFIED needs the opened CUDA device's uuid to be one of the physical GPUs NVML enumerates. Without NVML (or with an
    empty list) the uuid cannot be told from a MIG instance's (torch reports those as BARE hex, so a `MIG-` prefix test is useless), hence
    unverified; an explicit MIG in CUDA_VISIBLE_DEVICES is always refused. Local rendering never depends on any of this."""
    nvml = nvml_uuids if nvml is None else nvml  # resolved at call time, so a test's replacement is the one used
    raw = getattr(props, "uuid", None)  # torch's `_CUuuid` object (its __str__ is the uuid), not a plain str
    if raw is None:
        return _no("this torch build does not expose the device uuid")
    if "MIG-" in os.environ.get("CUDA_VISIBLE_DEVICES", "").upper():
        return _no("MIG devices are not supported as a network GPU identity")
    uuid = canonical_uuid(raw if isinstance(raw, str) else str(raw))
    if uuid is None:
        return _no("the device uuid is malformed (or a MIG identity, which is not supported)")
    known = nvml()
    if known is None:
        return _no("NVML is unavailable: the uuid cannot be confirmed as a physical GPU")
    if not known:
        return _no("NVML lists no GPUs: the uuid was not cross-checked")
    if uuid not in known:
        return _no("the device uuid is not among the physical NVML GPUs (a MIG instance is not supported)")
    return {"uuid": uuid, "uuid_status": "verified", "uuid_reason": None}


def _no(why):
    return {"uuid": None, "uuid_status": "unverified", "uuid_reason": why}
