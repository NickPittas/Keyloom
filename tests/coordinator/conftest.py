import copy
import threading

import pytest

from backend.coordinator import pki
from backend.coordinator.client import join_to_file
from backend.coordinator.server import CoordinatorServer
from backend.coordinator.store import Store

from .specs import CAPS, SPEC  # noqa: E402,F401


def spec(**over):
    s = copy.deepcopy(SPEC)
    s.update(over)
    return s


def sid(n):  # 16+ char ids
    return f"{n:0>20}"


class Clock:
    def __init__(self): self.t = 1_800_000_000.0
    def __call__(self): return self.t


@pytest.fixture
def clock():
    return Clock()


@pytest.fixture
def store(tmp_path, clock):
    s = Store(tmp_path / "c.sqlite3", clock)
    yield s
    s.close()


def make_inst(store, name="pc", admin=False):
    iid, tok = store.join(name)
    if admin:
        store.db.execute("UPDATE installations SET is_admin=1 WHERE id=?", (iid,))
    return store.authenticate(f"{iid}.{tok}"), f"{iid}.{tok}"


def session(inst, n="s1", gpu="GPU-aaaaaaaa"):
    return {"protocol": 1, "session_id": sid(n), "build": "t", "gpus": [{"uuid": gpu, "name": "RTX", "vram_total_mb": 24000, "ram_total_mb": 64000}]}


def claim_body(clock, session_n="s1", claim_n="c1", caps=CAPS):
    return {"session_id": sid(session_n), "claim_id": sid(claim_n), "capabilities": caps,
            "resources": {"sampled_at": int(clock.t), "free_vram_mb": 20000, "free_ram_mb": 30000}}


@pytest.fixture
def tls_server(tmp_path, store):
    cert_pem, key_pem = pki.generate_identity(["127.0.0.1"])
    c, k = tmp_path / "cert.pem", tmp_path / "key.pem"
    pki.write_private_new(str(k), key_pem)
    c.write_text(cert_pem)
    srv = CoordinatorServer(("127.0.0.1", 0), pki.server_context(str(c), str(k)), store, join_enabled=True)
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    url = f"https://127.0.0.1:{srv.server_address[1]}"
    yield srv, url, cert_pem
    srv.shutdown()
    srv.server_close()


def paired_client(store, url, cert_pem, admin=False):
    import tempfile
    with tempfile.TemporaryDirectory() as directory:
        path = directory + "/credentials.json"
        join_to_file(url, path, "pc", pem=cert_pem)
        return pki.read_credentials(path), None
