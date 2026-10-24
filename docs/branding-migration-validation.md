# Keyloom rename, branding and reinstall preservation: validation record

**Status: source frozen, candidates built, live install/migration (B4) NOT yet performed. B1 passed only on fixtures; B4 pending.**

## Candidates (canonical `python packaging/build_release.py`, exit 0 on both, one build per OS from the same frozen source)
| OS | Payload id | Run dir | Notes |
|---|---|---|---|
| Linux | `2.4.0-bc04563e46` | `packaging/dist/20261024-180047` (`build.log`, `build-result.json`) | offline `.run` sha256 `964f4f09…d754b`, online `.run` sha256 `566ed83b…089d3` |
| Windows (built on Kamino3) | `2.4.0-a52e85185a` | `C:\Users\Nick\kb-out\20261024-180048` | setup exe `dad6da87…85226`, online exe `13758e3f…48c77`; the new Inno script compiled |

All earlier candidates (`089c591d45`, `f11b087b6b`, `defb03b29b`, `6ddc7fc93c`, `1fa8e69522`) are superseded.

## What changed in this pass
- **Firewall message (Endor "added 4 UFW rules" with `--firewall skip`)**: the "already settled" shortcut returned the earlier record with its old `added N rule(s)` text; skip never ran sudo. Now reports "recorded by an earlier install ... nothing was added or changed". Regression: `test_reinstall_over_settled_rules_never_claims_new_rules_or_uses_sudo` (skip and ask).
- **Kamino1 "service-install.json is unreadable"**: the file was intact (valid JSON, no BOM, owner-only ACL). The reader treated any `OSError` as fatal, so a Windows sharing violation during the atomic replace by the service or installer showed as "unreadable". `_read_json` now retries and names the real error. Not yet proven live on Kamino1 (B4).
- **Windows firewall prompt for a new path**: rules are per program path (Kamino1 has Allow rules for the old `pythonw.exe` paths). The installer now reads the firewall (no admin), adds nothing if an Allow rule for the new `pythonw.exe` exists, else asks once and runs one UAC step adding TCP 8443-8452 + UDP 44554 for that program, Private+Public, from LocalSubnet/Tailscale/ZeroTier only; rules are named `Keyloom network ... (<version>)`, recorded in the receipt, and only those are removed on uninstall. Silent install or no terminal: nothing added, exact PowerShell command printed. Read-only detection verified live on Kamino1 (True for the real rules, False for a fake path). The UAC step itself was not exercised (needs an interactive administrator approval).
- **Windows offline installer**: same build already installed and verified -> message and `ExitProcess(0)` in `InitializeSetup` before any file, registry, uninstall-data, service or settings write (previous version returned `False` = exit 1). Runtime: `[Files]` split; the runtime entry uses `Check: NeedRuntime`, which runs `lifecycle link-runtime` (all or nothing, hash-verified hard links from the active version; any doubt extracts instead). Default folder is `...\Programs\Keyloom`; an install in the old default folder is taken over (`--adopt-from`); a user-chosen folder is updated in place. `--firewall skip` is passed for `/SILENT`. Models task is unchecked by default. **Only compiled so far; the real installer has not been run (B4).**
- **Prefix hand-off** (`lifecycle install --adopt-from OLD`, Linux stub and online installers pass it; Windows Inno passes it): new version installed into `.../Keyloom`, unchanged runtime hard-linked, owned launcher/menu entry/sign-in entry/network service re-keyed to the new prefix (same service identity, certificate, database), model/firewall records carried over, old prefix left as an inert rollback holder (`holder_of`), uninstalling a holder no longer removes the shared shortcut, startup entry or firewall rules. Fixtures cover it (`tests/test_branding_migration.py`).
- **Settings folder**: `corridorkey-native` -> `keyloom` is one atomic rename (nothing copied), only when no app or foreign service is running, with a journal, a compatibility link under the old name, textual path rewrite in `settings.json` only (unknown keys, custom roots, order and spacing untouched), crash recovery before/after the rename, and a service restart marker. The online installer renames it before taking the instance lock (Windows cannot rename a folder holding our own lock file).
- **Qt front end removed**: `backend/annotations.py` (verbatim AnnotationModel, Qt-free, test that it imports no Qt) is used by `backend/service`; removed `ui/` (except `ui/theme/keyloom*` icons used by the Rust app and installers), `main.py`, `clip_manager.py`, `backend/{version_sync,error_reporting,update_verify}.py`, the Qt-only scripts, v1 launchers/docker/CI workflows that called them, and 53 GUI tests (kept `test_project`, restored `test_v2_no_legacy_paths` with a backend scan). `PySide6` stays in the runtime lock on purpose: `backend` uses `PySide6.QtCore.QSettings`, `selfcheck` imports `QtCore`, and the Windows VC runtime DLLs are staged from the PySide6 wheel (compat packaging input, not an app UI).
- **Telemetry/credits**: no report endpoint remains; Patreon/Ko-fi removed; issue links point to `NickPittas/Keyloom`; the About box says Keyloom.

## Gates
| Gate | State |
|---|---|
| B0 scope | done |
| B1 focused fixtures | passed: branding migration 20, packaging 45, firewall 35, build_release 14, install_paths, coordinator; Rust runtime/settings migration tests. |
| B2 residual audit | below; master hash `fc1f5307…4046` equals the repo master; ICO 16–256; Windows exe carries the 7 icon resources |
| B3 canonical builds | done (above) |
| B4 live Endor/Kamino1/Kamino3 | **pending**: install the candidates, same-build rerun, 60 s interaction, Kamino1 service status |
| UAC firewall step, real Inno no-op/exit code/runtime link, Add/Remove entry after moving prefix | **pending (Windows B4)** |

## Pre-existing failures (not from this work)
`tests/test_key_job.py`, `test_key_replace.py`, `test_worker_protocol.py`, `test_v2_setup_models.py` and `tests/netlink` fail the same way on a clean `HEAD` copy / predate this task. The Rust `models::tests` helper tests are flaky (`Text file busy`).

## Limits and residuals
- Model/data folder `CorridorKey` (user-data root) is **not** renamed: it can hold very large models and model receipts with absolute paths. It keeps working under its old name.
- A user-chosen Windows/Linux install folder is updated in place (not moved).
- Old `corridorkey*` shims and the old prefix remain in the rollback holder until `corridorkey-setup uninstall` is run on it.
- Windows offline updates that cannot link (cross-volume, permissions, corrupt source) extract the runtime again.
- Engine/package identifiers (`CorridorKeyV2`, `CorridorKeyModule`, `corridorkey_v2`, `corridorkey-weights`, `CORRIDORKEY_V2_*`), project files (`.corridorkey_*`), catalog `notices`, vendored licences, "powered by CorridorKey" and README credits, historical `docs/design/*`, release notes, CHANGELOG, `docs/upstream-readme.md`, i18n READMEs, `PLAN-*.md`, and the legacy-name compatibility readers (descriptor, receipt app id, exe name, owner markers, settings/user-data folder names) are intentional.
