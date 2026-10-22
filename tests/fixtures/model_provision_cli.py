"""Test-owned setup CLI fixture with a deterministic, local transport gate."""
from __future__ import annotations

import argparse
import hashlib
import os
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT))

fixture_parser = argparse.ArgumentParser(add_help=False)
fixture_parser.add_argument("--fixture-ready-file")
fixture_parser.add_argument("--fixture-crash-after-old-move", action="store_true")
fixture_args, cli_args = fixture_parser.parse_known_args()

import backend.model_catalog as model_catalog  # noqa: E402
import backend.model_provision as model_provision  # noqa: E402

payload = b"fixture-weights"
catalog = model_catalog.Catalog(
    1,
    "test",
    {},
    {
        "fixture-weights": model_catalog.Artifact(
            id="fixture-weights",
            label="fixture weights",
            capability="test",
            kind="hf_snapshot",
            required_profile=True,
            layout_root="data_dir",
            layout_rel="fixture/weights",
            repo_id="local/fixture",
            revision="a" * 40,
            revision_status="pinned",
            files=(model_catalog.FileSpec(
                "weights.bin",
                "weights.bin",
                len(payload),
                hashlib.sha256(payload).hexdigest(),
                "fixture-sha256",
                True,
                True,
            ),),
        )
    },
)
model_catalog.load_catalog = lambda: catalog

if fixture_args.fixture_crash_after_old_move:
    def write_payload_then_crash(self, request, _artifact_id):
        payload_path = Path(request["local_dir"]) / "weights.bin"
        payload_path.parent.mkdir(parents=True, exist_ok=True)
        payload_path.write_bytes(payload)
        replace = Path.replace

        def crash_after_backup(source, target):
            result = replace(source, target)
            if Path(target).name.endswith(".previous"):
                os._exit(77)
            return result

        Path.replace = crash_after_backup

    model_provision.ProvisionManager._run_download_worker = write_payload_then_crash
elif fixture_args.fixture_ready_file or any(
        action in cli_args for action in ("--download", "--repair")
):
    if fixture_args.fixture_ready_file:
        ready_file = Path(fixture_args.fixture_ready_file)
    else:
        try:
            cache_arg = cli_args.index("--cache-dir")
            ready_file = Path(cli_args[cache_arg + 1]) / "model-provision-fixture.ready"
        except (ValueError, IndexError):
            raise SystemExit("fixture downloads require --cache-dir for the ready-file handshake")

    def wait_for_cancel(self, _request, artifact_id):
        ready_file.parent.mkdir(parents=True, exist_ok=True)
        ready_file.touch(exist_ok=True)
        marker = self._cancel_marker_path(self.operation_id)
        deadline = time.monotonic() + 8
        while not marker.is_file() and time.monotonic() < deadline:
            time.sleep(0.01)
        if not marker.is_file():
            raise RuntimeError("cancel helper did not signal before fixture timeout")
        self._check_cancel(artifact_id)

    model_provision.ProvisionManager._run_download_worker = wait_for_cancel

from scripts import setup_models  # noqa: E402

sys.argv = [str(ROOT / "scripts" / "setup_models.py"), *cli_args]
raise SystemExit(setup_models.main())
