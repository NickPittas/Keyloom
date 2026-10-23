#!/bin/sh
# CorridorKey @VERSION_ID@ self-extracting Linux user installer (needs only sh, tar, gzip, awk, sha256sum).
# No root, no services, no listeners. Installs under PREFIX; user data stays outside it.
set -eu
PAYLOAD_SHA256="@PAYLOAD_SHA256@"
PREFIX="${CORRIDORKEY_PREFIX:-$HOME/.local/opt/CorridorKey}"
VERB=install; MODELS=full; MODELS_SET=0; EXTRACT=""
usage() { cat <<U
Usage: $(basename "$0") [--prefix DIR] [--repair] [--models full|skip] [--model-arg ARG]... [--extract-only DIR]
  default       install/update into PREFIX ($PREFIX); previous version kept for rollback
  --repair      restore damaged bundle files from this payload; models are re-verified only with --models full
  --models skip install the app only (setup stays marked INCOMPLETE until: corridorkey-setup models)
  --model-arg   pass one argument to scripts/setup_models.py, repeatable, spaces preserved
                (e.g. --model-arg --hf-cache --model-arg "/my cache")
Later: PREFIX/bin/corridorkey-setup {repair|rollback|models|status|uninstall}
Server mode (opt-in, starts nothing by itself): PREFIX/bin/corridorkey-coordinator {serve|pair|revoke|abandon} ...
U
}
# Rotate args: scalar options are consumed; --model-arg values are re-appended verbatim so "$@" ends up
# holding only the model args and quoting/spaces/globs survive.
N=$#; I=0
while [ "$I" -lt "$N" ]; do
  A=$1; shift; I=$((I+1))
  case "$A" in
    --repair) VERB=repair;;
    -h|--help) usage; exit 0;;
    --prefix|--models|--model-arg|--extract-only)
      [ "$I" -lt "$N" ] || { echo "$A needs a value" >&2; exit 64; }
      V=$1; shift; I=$((I+1))
      case "$A" in
        --prefix) PREFIX="$V";;
        --models) MODELS="$V"; MODELS_SET=1;;
        --extract-only) EXTRACT="$V";;
        --model-arg) set -- "$@" "--model-arg=$V";;
      esac;;
    *) echo "unknown option $A" >&2; usage >&2; exit 64;;
  esac
done
case "$MODELS" in full|skip) ;; *) echo "--models must be full or skip" >&2; exit 64;; esac
[ "$(uname -m)" = x86_64 ] || { echo "ERROR: x86_64 Linux only" >&2; exit 1; }
SELF=$(readlink -f "$0")
START=$(awk '/^__PAYLOAD_BELOW__$/ {print NR+1; exit}' "$SELF")
echo "Verifying installer payload ..."
GOT=$(tail -n +"$START" "$SELF" | sha256sum | cut -d' ' -f1)
[ "$GOT" = "$PAYLOAD_SHA256" ] || { echo "ERROR: payload checksum mismatch (download damaged?)" >&2; exit 1; }
if [ -n "$EXTRACT" ]; then
  mkdir -p "$EXTRACT"; tail -n +"$START" "$SELF" | tar -xzf - -C "$EXTRACT"; echo "Extracted to $EXTRACT"; exit 0
fi
mkdir -p "$PREFIX"
STAGE=$(mktemp -d "$PREFIX/.staging.XXXXXX")  # same filesystem as PREFIX so the final move is a rename
trap 'rm -rf "$STAGE"' EXIT
trap 'exit 130' INT HUP TERM  # signals run the EXIT trap too
echo "Extracting ..."
tail -n +"$START" "$SELF" | tar -xzf - -C "$STAGE"
PY="$STAGE/payload/runtime/python/bin/python3"
LC="$STAGE/payload/app/packaging/lifecycle.py"
set +e
if [ "$VERB" = repair ]; then
  if [ "$MODELS_SET" = 1 ] && [ "$MODELS" = full ]; then
    "$PY" -I -B "$LC" --prefix "$PREFIX" repair --payload "$STAGE/payload" --models "$@"
  else
    "$PY" -I -B "$LC" --prefix "$PREFIX" repair --payload "$STAGE/payload" "$@"
  fi
else
  "$PY" -I -B "$LC" --prefix "$PREFIX" install --payload "$STAGE/payload" --models "$MODELS" "$@"
fi
RC=$?
exit $RC
__PAYLOAD_BELOW__
