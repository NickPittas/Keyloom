"""Bounded upstream-metadata resolution for the model catalog (maintenance use).

Fetches ONLY official metadata (HF API trees/model info, GitHub release API,
optional HEAD for plain HTTP assets) to pin revisions and record the hashes
and sizes upstream itself publishes. Never downloads model bytes.

Rules enforced here:
  * every request uses a short timeout (default 10s, hard cap 10s);
  * HF git blob ``oid`` (SHA-1) is stored as ``git_blob_sha1`` — never as
    ``sha256``; only ``lfs.oid`` (sha256) or a GitHub ``digest`` becomes a
    trusted ``sha256``;
  * a resolution failure leaves the field ``null`` (explicitly unresolved)
    — the artifact then reports unverified, never "probably fine";
  * an artifact already pinned keeps its revision: if upstream HEAD moved,
    drift is reported, not silently followed.

Importable without huggingface_hub; only urllib is used.
"""
from __future__ import annotations

import json
import urllib.error
import urllib.request
from dataclasses import dataclass, field

MAX_TIMEOUT = 10.0
HF_API = "https://huggingface.co/api"
GH_API = "https://api.github.com"
USER_AGENT = "EZ-CorridorKey2-model-catalog/1.0 (metadata only)"


class MetadataUnavailable(Exception):
    """Network/API failure resolving an artifact — resolution stays unresolved."""


@dataclass
class Resolution:
    artifact_id: str
    ok: bool
    revision: str | None = None
    revision_source: str | None = None  # "existing-pin" | "hf-api-head"
    revision_drift: str | None = None  # upstream HEAD when it differs from the pin
    gated: str | None = None
    files: list[dict] = field(default_factory=list)
    problems: list[str] = field(default_factory=list)
    filled: list[str] = field(default_factory=list)  # fields newly resolved


def fetch_json(url: str, timeout: float = MAX_TIMEOUT) -> object:
    timeout = min(timeout, MAX_TIMEOUT)
    req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return json.loads(resp.read().decode("utf-8"))
    except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
        raise MetadataUnavailable(f"{url}: {e}") from e


def hf_model_info(repo_id: str, timeout: float = MAX_TIMEOUT) -> dict:
    data = fetch_json(f"{HF_API}/models/{repo_id}", timeout)
    if not isinstance(data, dict):
        raise MetadataUnavailable(f"{repo_id}: unexpected info payload")
    return data


def hf_tree(repo_id: str, revision: str, timeout: float = MAX_TIMEOUT) -> list[dict]:
    """Full recursive tree, following the API's Link-header pagination."""
    url = f"{HF_API}/models/{repo_id}/tree/{revision}?recursive=true"
    entries: list[dict] = []
    for _ in range(64):  # bounded: 64 pages x 1000 entries is far beyond any repo here
        try:
            with urllib.request.urlopen(
                urllib.request.Request(url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"}),
                timeout=min(timeout, MAX_TIMEOUT),
            ) as resp:
                page = json.loads(resp.read().decode("utf-8"))
                link = resp.headers.get("Link")
        except (urllib.error.URLError, TimeoutError, OSError, ValueError) as e:
            raise MetadataUnavailable(f"{repo_id}@{revision}: {e}") from e
        if not isinstance(page, list):
            raise MetadataUnavailable(f"{repo_id}@{revision}: unexpected tree payload")
        entries.extend(e for e in page if isinstance(e, dict) and e.get("type") == "file")
        nxt = _next_link(link)
        if not nxt:
            return entries
        url = nxt
    raise MetadataUnavailable(f"{repo_id}@{revision}: tree pagination exceeded bound")


def _next_link(link: str | None) -> str | None:
    if not link:
        return None
    for part in link.split(","):
        bits = part.split(";")
        if len(bits) == 2 and 'rel="next"' in bits[1]:
            return bits[0].strip().strip("<>")
    return None


def gh_release_assets(owner_repo: str, tag: str, timeout: float = MAX_TIMEOUT) -> dict[str, dict]:
    data = fetch_json(f"{GH_API}/repos/{owner_repo}/releases/tags/{tag}", timeout)
    if not isinstance(data, dict) or not isinstance(data.get("assets"), list):
        raise MetadataUnavailable(f"{owner_repo}@{tag}: unexpected release payload")
    out: dict[str, dict] = {}
    for asset in data["assets"]:
        if isinstance(asset, dict) and isinstance(asset.get("name"), str):
            out[asset["name"]] = asset
    return out


def http_head(url: str, timeout: float = MAX_TIMEOUT) -> dict:
    req = urllib.request.Request(url, method="HEAD", headers={"User-Agent": USER_AGENT})
    try:
        with urllib.request.urlopen(req, timeout=min(timeout, MAX_TIMEOUT)) as resp:
            return {"length": resp.headers.get("Content-Length"),
                    "digest": resp.headers.get("Digest"),  # e.g. GCS "sha256=..." if offered
                    "md5": resp.headers.get("Content-MD5"),
                    "etag": resp.headers.get("ETag")}
    except (urllib.error.URLError, TimeoutError, OSError) as e:
        raise MetadataUnavailable(f"{url}: {e}") from e


def _hf_file_entry(entry: dict, required: bool, note: str | None = None) -> dict:
    lfs = entry.get("lfs")
    is_lfs = isinstance(lfs, dict)
    out = {
        "path": entry["path"],
        "size": entry.get("size"),
        "lfs": is_lfs,
        "required": required,
        "git_blob_sha1": entry.get("oid"),
        "sha256": None,
        "sha256_source": None,
    }
    if is_lfs and isinstance(lfs.get("oid"), str) and len(lfs["oid"]) == 64:
        out["sha256"] = lfs["oid"].lower()
        out["sha256_source"] = "hf-lfs-oid"
        out["size"] = lfs.get("size", out["size"])
    if note:
        out["note"] = note
    return out


def _doc_or_meta(name: str) -> bool:
    base = name.rsplit("/", 1)[-1].lower()
    return base in (".gitattributes", ".gitignore") or base.startswith(("readme", "license", "notice", "code_of_conduct"))


def resolve_hf_artifact(art: dict, timeout: float = MAX_TIMEOUT) -> Resolution:
    """Resolve one HF-backed catalog artifact entry (dict form, mutated copy returned).

    ``art`` is the artifact's JSON dict from the catalog. Doc files are kept
    with ``required=false`` (downloaded by snapshots, not needed at runtime);
    everything else in the tree is required, minus per-artifact ignore rules.
    """
    aid = art.get("id", "?")
    res = Resolution(artifact_id=aid, ok=False)
    info = hf_model_info(art["repo_id"], timeout)
    head = info.get("sha")
    res.gated = info.get("gated")

    pinned = art.get("revision")
    if pinned:
        res.revision = pinned
        res.revision_source = "existing-pin"
        if isinstance(head, str) and head != pinned:
            res.revision_drift = head
    elif isinstance(head, str) and len(head) == 40:
        res.revision = head
        res.revision_source = "hf-api-head"
        res.filled.append("revision")
    else:
        res.problems.append("upstream HEAD unavailable; revision stays unresolved")
        return res

    if art["kind"] == "hf_cache_file":
        # Single checkpoint file: resolve just that file's metadata.
        want = art["hf_filename"]
        for entry in hf_tree(art["repo_id"], res.revision, timeout):
            if entry.get("path") == want:
                f = _hf_file_entry(entry, required=True)
                res.files = [f]
                break
        if not res.files:
            res.problems.append(f"file {want} not found in tree at pinned revision")
            return res
    elif art["kind"] == "hf_files":
        # Only the authored file list is downloaded; resolve exactly those.
        wanted = [f["path"] for f in art.get("files", []) if f.get("path")]
        if not wanted:
            res.problems.append("hf_files artifact authored without a file list")
            return res
        by_path = {e.get("path"): e for e in hf_tree(art["repo_id"], res.revision, timeout)}
        missing = [w for w in wanted if w not in by_path]
        if missing:
            res.problems.append(f"authored files missing from tree at pinned revision: {missing}")
            return res
        res.files = [_hf_file_entry(by_path[w], required=True) for w in wanted]
    else:
        ignore = art.get("ignore_patterns", [])
        out = []
        for entry in hf_tree(art["repo_id"], res.revision, timeout):
            path = entry.get("path", "")
            if any(path.endswith(p.replace("*", "")) for p in ignore if p.startswith("*")):
                out.append(_hf_file_entry(entry, required=False, note="excluded by downloader ignore pattern; unused duplicate"))
            elif _doc_or_meta(path):
                out.append(_hf_file_entry(entry, required=False, note="documentation/notice file"))
            else:
                out.append(_hf_file_entry(entry, required=True))
        res.files = out

    remap = art.get("path_remap", {})
    for f in res.files:
        layout = f["path"]
        for src, dst in remap.items():
            if layout == src or layout.startswith(src.rstrip("/") + "/"):
                tail = layout[len(src.rstrip("/")):].lstrip("/")
                layout = dst.rstrip("/") + "/" + tail if tail else dst.rstrip("/")
        f["layout_path"] = layout

    # Authored per-file required overrides (runtime loader analysis beats heuristic).
    for path, ov in art.get("required_overrides", {}).items():
        for f in res.files:
            if f["path"] == path:
                if "required" in ov:
                    f["required"] = bool(ov["required"])
                if "note" in ov:
                    f["note"] = ov["note"]

    res.ok = True
    return res


def resolve_http_artifact(art: dict, timeout: float = MAX_TIMEOUT) -> Resolution:
    """Resolve an http_file artifact from GitHub release metadata (or HEAD)."""
    aid = art.get("id", "?")
    res = Resolution(artifact_id=aid, ok=False)
    url = art.get("url", "")
    if "github.com" in url and "/releases/download/" in url:
        try:
            owner_repo = url.split("github.com/", 1)[1].split("/releases", 1)[0]
            tag = url.split("/releases/download/", 1)[1].split("/", 1)[0]
            filename = url.rsplit("/", 1)[1]
        except IndexError:
            res.problems.append(f"unparseable GitHub release URL {url}")
            return res
        assets = gh_release_assets(owner_repo, tag, timeout)
        asset = assets.get(filename)
        if asset is None:
            res.problems.append(f"asset {filename} missing from release {owner_repo}@{tag}")
            return res
        digest = asset.get("digest")
        sha, src = None, None
        if isinstance(digest, str) and digest.startswith("sha256:") and len(digest) == 7 + 64:
            sha, src = digest[7:].lower(), "github-release-digest"
        elif isinstance(digest, str) and digest.startswith("sha256=") and len(digest) == 7 + 64:
            sha, src = digest[7:].lower(), "github-release-digest"
        res.files = [{
            "path": filename,
            "layout_path": filename,
            "size": asset.get("size"),
            "lfs": False,
            "required": True,
            "sha256": sha,
            "sha256_source": src,
            "git_blob_sha1": None,
        }]
        res.revision = tag
        res.revision_source = "github-release-tag"
        if sha:
            res.filled.append("sha256")
        res.ok = True
        return res

    # Plain HTTP asset (e.g. GCS RVM): HEAD only; most servers offer no hash.
    try:
        head = http_head(url, timeout)
    except MetadataUnavailable as e:
        res.problems.append(str(e))
        return res
    length = head.get("length")
    res.files = [{
        "path": url.rsplit("/", 1)[-1],
        "layout_path": url.rsplit("/", 1)[-1],
        "size": int(length) if length and length.isdigit() else None,
        "lfs": False,
        "required": True,
        "sha256": None,
        "sha256_source": None,
        "git_blob_sha1": None,
        "note": "no content hash offered by server metadata; verify via md5_b64 if upstream supplies one",
    }]
    res.revision = url
    res.revision_source = "url"
    res.ok = True
    return res
