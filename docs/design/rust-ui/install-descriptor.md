# Install descriptor contract (L1 runtime foundation)

Status: implemented 2026-10-22 in `rust-ui/src/runtime.rs` (authoritative Rust) and
`backend/install_paths.py` (authoritative Python, stdlib only). Shared schema fixture:
`rust-ui/tests/fixtures/install/keyloom-install.json` — parsed and asserted by BOTH
languages' tests (`runtime::tests` in `rust-ui/src/runtime.rs`, `tests/test_install_paths.py`),
so a contract drift fails CI on one side or the other. This document is the handoff for
downstream consumers (model manager / installer / setup helper); it changes nothing by itself.

## File and discovery

One `keyloom-install.json` per install, in the **install root** (next to/near the app
binary; the packaged helper tree sits below it). Discovery, first match wins:

1. `CK_INSTALL` env — explicit path to the descriptor **file or its folder**. If it points
   somewhere without a descriptor, that is an **error** (no fallback).
2. Ancestors of the executable (Rust) / of the helper root `backend/..` (Python), 4 levels up.

Never the CWD: a packaged release must launch from any folder. Resolution is lexical only
(no `~`, no symlink canonicalization) so the whole tree relocates by moving it.

## Schema (version 1)

```json
{
  "schema": 1,
  "app_name": "Keyloom",
  "app_version": "2.1.5",
  "roots": {
    "python": "runtime/python/bin/python3",
    "helper": "app",
    "models": "models",
    "cache": "../var-cache",
    "scratch": null,
    "user_data": null
  }
}
```

- `app_root` is **implicit**: the descriptor's own folder (never a field — it cannot drift).
- `roots` values: string (relative → resolved against `app_root`, absolute → kept; `..`
  allowed) or `null`/absent → platform default / consumer default.
- Required: `python`, `helper`. `helper` is the repo-like tree containing `backend/worker/`
  and `rust-ui/tools/ck_import.py`. Optional: `models`, `cache`, `scratch`, `user_data`.
- Unknown top-level or `roots` fields, non-integer or newer `schema`, missing/empty required
  roots, invalid JSON → **hard error naming file + field + remedy** ("update the app",
  "reinstall or repair", "fix/unset CK_INSTALL"). A found-but-broken descriptor disables the
  runtime actions; it NEVER falls back to a developer checkout or platform defaults.
- `null` optional roots mean "use the platform default" — no moves, no deletes.

## Root precedence (single order, both languages)

| Consumer | Order |
| --- | --- |
| repo/helper + python (import, AI worker) | `--repo`/caller argument → valid `CK_REPO` → descriptor (`helper`+`python`) → developer checkout discovery (exe/CWD ancestors with `backend/worker/__main__.py`) |
| interpreter | target's explicit python (settings) → `CK_PYTHON` env → descriptor `python` → `<repo>/.venv` |
| models root (`backend.v2_paths.get_v2_models_dir`) | `CORRIDORKEY_V2_MODELS_DIR` env → descriptor `models` → platform default (`~/.local/share/Keyloom/EngineV2` / `%LOCALAPPDATA%\Keyloom\EngineV2`) when no descriptor |
| settings file | `keyloom/settings.json` under XDG/`~/.config`; Windows now `%APPDATA%` then `%LOCALAPPDATA`, no `HOME` needed |
| default new-projects dir | `--data-dir` → descriptor `user_data` → repo root (dev) → platform user data |

## Environment contract for child processes

The Rust app injects, only when absent (user-set values always win), when spawning the
worker (`python -m backend.worker`) and the import helper (`rust-ui/tools/ck_import.py`):

- `CK_INSTALL` = absolute descriptor path.

Python helpers should re-read the descriptor via `backend/install_paths.py`
(`discover()`, `setup_helper_env(base)`) rather than hard-coding roots. `setup_helper_env`
additionally seeds `CORRIDORKEY_V2_MODELS_DIR` from descriptor `models` when unset — never
overriding explicit values.

## Platform defaults when roots are `null`

- `user_data`: `%APPDATA%\Keyloom` (else `%LOCALAPPDATA%\Keyloom`) / `$XDG_DATA_HOME` or
  `~/.local/share` + `/Keyloom`. No `HOME` required on Windows.
- `cache`: `%LOCALAPPDATA%\Keyloom\cache` (else `%APPDATA%\Keyloom\cache`) / `$XDG_CACHE_HOME` or `~/.cache` + `/Keyloom`.
- `scratch`: `<user_data>/scratch`.
- models: the platform default (see table above).

The app root may be **read-only**: nothing resolves from, or writes into, it at run time;
user settings/cache/scratch live outside it.

## Test obligations (per rust-ui/AGENTS.md)

Temp relocated layouts from an unrelated CWD, Windows-without-HOME settings/data paths,
override precedence, unknown-field/newer-schema errors, and the existing import workflow are
covered by: `cargo test --offline --lib` (runtime, settings), `--test {import,settings,worker,
ai_ui,scheduler,jobs,loader}`, and `pytest tests/test_install_paths.py tests/test_v2_paths.py`
— all through `tools/isolated-test.sh`, each command ≤15s. No downloads, GPU, real projects,
desktop or OS interaction.
