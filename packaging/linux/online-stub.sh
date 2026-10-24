#!/bin/sh
# Keyloom (powered by CorridorKey) @VERSION_ID@ online installer for Linux (needs only sh, tar, gzip, awk, sha256sum).
# Carries Keyloom itself (app, UI, FFmpeg, bundled Python and every small dependency). Only the large public wheels
# (torch, triton, NVIDIA CUDA libraries) are downloaded from PyPI / download.pytorch.org: no account, no sign-in.
# No root, no services, no listeners. Models are not installed here: download them from the app.
# User data, settings, models, network identities and jobs are never touched.
set -eu
PAYLOAD_SHA256="@PAYLOAD_SHA256@"
PREFIX="${CORRIDORKEY_PREFIX:-$HOME/.local/opt/CorridorKey}"; REPAIR=""
usage() { cat <<U
Usage: $(basename "$0") [--prefix DIR] [--repair]
  default       install or update Keyloom @VERSION_ID@ into PREFIX ($PREFIX); files of the installed versions
                are reused, only the wheels still needed are downloaded (several GB on a first install)
  --repair      re-verify and restore damaged files of @VERSION_ID@
Quit Keyloom first (File > Quit).
U
}
while [ $# -gt 0 ]; do
  case "$1" in
    --repair) REPAIR=1; shift;;
    --prefix) [ $# -ge 2 ] || { echo "--prefix needs a value" >&2; exit 64; }; PREFIX="$2"; shift 2;;
    -h|--help) usage; exit 0;;
    *) echo "unknown option $1" >&2; usage >&2; exit 64;;
  esac
done
[ "$(uname -m)" = x86_64 ] || { echo "ERROR: x86_64 Linux only" >&2; exit 1; }
SELF=$(readlink -f "$0")
START=$(awk '/^__PAYLOAD_BELOW__$/ {print NR+1; exit}' "$SELF")
echo "Verifying installer payload ..."
GOT=$(tail -n +"$START" "$SELF" | sha256sum | cut -d' ' -f1)
[ "$GOT" = "$PAYLOAD_SHA256" ] || { echo "ERROR: payload checksum mismatch (download damaged?)" >&2; exit 1; }
mkdir -p "$PREFIX"
STAGE=$(mktemp -d "$PREFIX/.staging.XXXXXX")  # same filesystem as PREFIX (not a possibly small tmpfs /tmp)
trap 'rm -rf "$STAGE"' EXIT
trap 'exit 130' INT HUP TERM
echo "Extracting ..."
tail -n +"$START" "$SELF" | tar -xzf - -C "$STAGE"
W="$STAGE/keyloom-online"
set -- --payload-dir "$W/payload" --sources "$W/wheel-sources.json" --prefix "$PREFIX"
[ -z "$REPAIR" ] || set -- "$@" --repair
set +e
"$W/payload/runtime/python/bin/python3" -I -B "$W/payload/app/packaging/online.py" "$@"
RC=$?
[ "$RC" -ne 4 ] || echo "Keyloom is running: quit it (File > Quit), then run this installer again." >&2
exit $RC
__PAYLOAD_BELOW__
