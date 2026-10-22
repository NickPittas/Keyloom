#!/usr/bin/env python3
"""Refresh model catalog metadata from official upstream APIs (maintenance tool).

Performs bounded metadata-only requests (<=10s each, no model bytes) to pin
revisions and record the sha256/size upstream itself publishes, then writes
the merged catalog back (with --write). Never invents hashes: fields with no
upstream metadata source stay null and the artifact reports unverified.

Usage:
    python scripts/refresh_model_metadata.py            # dry-run, print summary
    python scripts/refresh_model_metadata.py --write    # update backend/model_catalog.json
    python scripts/refresh_model_metadata.py --artifact gvm --artifact sam2-large
"""
from __future__ import annotations

import argparse
import json
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT))

from backend import model_metadata as mm  # noqa: E402
from backend.model_catalog import CatalogError, catalog_path, load_catalog  # noqa: E402

# Doc/notice files: kept by snapshot downloads, never required at runtime.
# Populated by the resolver itself; listed here for documentation.


def _merge_artifact(art: dict, res: mm.Resolution) -> list[str]:
    """Merge a successful Resolution into the artifact dict. Returns changes."""
    changes: list[str] = []
    if res.gated is not None and art.get("gated") != res.gated:
        art["gated"] = res.gated
        changes.append(f"gated={res.gated}")
    if res.revision and art.get("revision") != res.revision:
        art["revision"] = res.revision
        art["revision_status"] = "pinned"
        changes.append("revision")
    if res.revision_drift:
        art["upstream_head"] = res.revision_drift
        changes.append(f"drift (kept pin {res.revision[:8]})")
    elif "upstream_head" in art and res.revision and res.revision_drift is None:
        if art.get("upstream_head") == res.revision:
            art.pop("upstream_head", None)
            changes.append("drift cleared")
    if res.files:
        art["files"] = res.files
        changes.append(f"files({len(res.files)})")
    return changes


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--write", action="store_true", help="write merged catalog in place")
    ap.add_argument("--artifact", action="append", default=[], help="limit to artifact id (repeatable)")
    ap.add_argument("--timeout", type=float, default=mm.MAX_TIMEOUT)
    args = ap.parse_args()

    path = catalog_path()
    with open(path, encoding="utf-8") as f:
        catalog = json.load(f)

    wanted = args.artifact or None
    unknown = sorted(set(wanted or ()) - set(catalog["artifacts"]))
    if unknown:
        print(f"unknown artifact id(s): {', '.join(unknown)}", file=sys.stderr)
        return 2
    failures: list[str] = []
    for aid, art in sorted(catalog["artifacts"].items()):
        if wanted and aid not in wanted:
            continue
        if art.get("kind") in ("local_package", "local_code"):
            print(f"[skip]    {aid}: no remote metadata source ({art['kind']})")
            continue
        if not art.get("repo_id") and not art.get("url"):
            print(f"[skip]    {aid}: no repo_id/url")
            continue
        print(f"[resolve] {aid}: {art.get('repo_id') or art.get('url')}")
        try:
            if art["kind"] == "http_file":
                res = mm.resolve_http_artifact(art, args.timeout)
            else:
                res = mm.resolve_hf_artifact(art, args.timeout)
        except mm.MetadataUnavailable as e:
            res = mm.Resolution(artifact_id=aid, ok=False, problems=[str(e)])
        if res.ok:
            changes = _merge_artifact(art, res)
            sha_n = sum(1 for f in res.files if f.get("sha256"))
            print(f"  ok       rev={res.revision[:12] if res.revision else None} "
                  f"files={len(res.files)} trusted_sha256={sha_n} changes={changes or ['none']}")
            if res.revision_drift:
                print(f"  DRIFT    upstream HEAD {res.revision_drift[:12]} != pin; pin kept")
        else:
            failures.append(aid)
            for p in res.problems:
                print(f"  UNRESOLVED {p}")

    if failures:
        print(f"\n{len(failures)} artifact(s) left explicitly unresolved: {', '.join(failures)}")
        print("Unresolved metadata cannot report verified; artifacts keep installed/unverified status.")
    if args.write:
        catalog["updated"] = date.today().isoformat()
        tmp = path.with_suffix(".json.tmp")
        with open(tmp, "w", encoding="utf-8") as f:
            json.dump(catalog, f, indent=2, sort_keys=True, ensure_ascii=False)
            f.write("\n")
        try:
            load_catalog(tmp)
        except CatalogError as e:
            tmp.unlink(missing_ok=True)
            print(f"catalog update rejected: {e}", file=sys.stderr)
            return 1
        tmp.replace(path)
        print(f"wrote {path}")
    else:
        print("dry-run; re-run with --write to update the catalog")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
