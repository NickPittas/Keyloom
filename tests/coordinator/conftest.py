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


def gpu(uuid="GPU-aaaaaaaa"):
    return {"uuid": uuid, "name": "RTX", "vram_total_mb": 24000, "ram_total_mb": 64000}


def begin(store, host, job_id, slot="gpu0", plan=None):
    """The receiving app starts a queued shot on its own worker: the one way a shot becomes `started` now."""
    return store.inbox_begin(host["id"], job_id, slot, plan or {"x": 1})["attempt_id"]


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
