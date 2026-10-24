#!/bin/sh
# Keyloom (powered by CorridorKey) @VERSION_ID@ self-extracting Linux user installer (needs only sh, tar, gzip, awk, sha256sum).
# No root (only an optional, asked-for sudo for UFW rules). Configures a per-user network host; user data stays outside PREFIX.
set -eu
# Layout after this stub: boot.tar.gz, app.tar.gz, runtime.tar.gz (independent gzip streams). Offsets are relative to the first
# byte after the stub, which is STUB_BYTES long.
STUB_BYTES=@STUB_BYTES@;
BOOT_OFFSET=@BOOT_OFFSET@; BOOT_LEN=@BOOT_LEN@; BOOT_SHA256="@BOOT_SHA256@"
APP_OFFSET=@APP_OFFSET@; APP_LEN=@APP_LEN@; APP_SHA256="@APP_SHA256@"
RUNTIME_OFFSET=@RUNTIME_OFFSET@; RUNTIME_LEN=@RUNTIME_LEN@; RUNTIME_SHA256="@RUNTIME_SHA256@"; RUNTIME_ID="@RUNTIME_ID@"
# A pre-rename install is upgraded in place; only a fresh machine gets the Keyloom folder.
# A pre-rename install (~/.local/opt/CorridorKey) is taken over: Keyloom installs into ~/.local/opt/Keyloom, reuses the unchanged
# runtime files by hard link, moves the startup entry/launcher/network service across, and leaves the old folder as the rollback holder.
OLD="$HOME/.local/opt/CorridorKey"; ADOPT=""
PREFIX="${KEYLOOM_PREFIX:-${CORRIDORKEY_PREFIX:-$HOME/.local/opt/Keyloom}}"
PREFIX_SET=0
VERB=install; FIREWALL=yes; MODELS=skip; MODELS_SET=0; EXTRACT=""
usage() { cat <<U
Usage: $(basename "$0") [--prefix DIR] [--repair] [--models full|skip] [--model-arg ARG]... [--firewall yes|skip | --no-firewall] [--extract-only DIR]
  default       install/update into PREFIX ($PREFIX); previous version kept for rollback. Installing the build that is
                already installed and verified changes nothing. Settings, models and network identity are never touched
  --repair      restore damaged bundle files from this payload; models are re-verified only with --models full
  --models full explicitly download and verify ALL models now (default skip: models are never downloaded by an
                install, update or repair; use the app's Models page or: keyloom-setup models)
  --model-arg   pass one argument to scripts/setup_models.py, repeatable, spaces preserved
                (e.g. --model-arg --hf-cache --model-arg "/my cache")
  --firewall    yes (default): if UFW is active, use sudo (a password prompt follows) to allow this PC's trusted networks
                (TCP 8443-8452, UDP 44554); without a terminal the step is left pending. skip: do not touch the firewall
  --no-firewall same as --firewall skip
Later: PREFIX/bin/keyloom-setup {repair|rollback|models|status|uninstall}
Network setup: open Keyloom > Clients to host or join a render network.
U
}
# Rotate args: scalar options are consumed; --model-arg values are re-appended verbatim so "$@" ends up
# holding only the model args and quoting/spaces/globs survive.
N=$#; I=0
while [ "$I" -lt "$N" ]; do
  A=$1; shift; I=$((I+1))
  case "$A" in
    --repair) VERB=repair;;
    --no-firewall) FIREWALL=skip;;
    -h|--help) usage; exit 0;;
    --prefix|--models|--model-arg|--firewall|--extract-only)
      [ "$I" -lt "$N" ] || { echo "$A needs a value" >&2; exit 64; }
      V=$1; shift; I=$((I+1))
      case "$A" in
        --prefix) PREFIX="$V"; PREFIX_SET=1;;
        --models) MODELS="$V"; MODELS_SET=1;;
        --firewall) FIREWALL="$V";;
        --extract-only) EXTRACT="$V";;
        --model-arg) set -- "$@" "--model-arg=$V";;
      esac;;
    *) echo "unknown option $A" >&2; usage >&2; exit 64;;
  esac
done
if [ "$PREFIX_SET" = 0 ] && [ -z "${KEYLOOM_PREFIX:-}${CORRIDORKEY_PREFIX:-}" ] && [ -f "$OLD/receipt.json" ] && [ ! -f "$PREFIX/receipt.json" ]; then ADOPT="$OLD"; fi
case "$MODELS" in full|skip) ;; *) echo "--models must be full or skip" >&2; exit 64;; esac
case "$FIREWALL" in yes|skip|ask) ;; *) echo "--firewall must be yes or skip" >&2; exit 64;; esac
[ "$(uname -m)" = x86_64 ] || { echo "ERROR: x86_64 Linux only" >&2; exit 1; }
SELF=$(readlink -f "$0")
if [ "$VERB" = install ] && [ -x "$PREFIX/current/runtime/python/bin/python3" ] && [ -z "$EXTRACT" ] && \
   "$PREFIX/current/runtime/python/bin/python3" -I -B "$PREFIX/current/app/packaging/lifecycle.py" --prefix "$PREFIX" check --version "@VERSION_ID@" 2>/dev/null; then
  echo "Nothing to do: this exact build is already installed and verified. Use --repair to restore damaged files."; exit 0
fi
# STAGE is private to this run (same filesystem as PREFIX so the lifecycle's final move is a rename) and holds
#   boot/     the bundled minimal Python that runs everything below: it lives OUTSIDE payload/, which the lifecycle renames
#   payload/  app (+ runtime only when the installed one cannot be attached)
if [ -n "$EXTRACT" ]; then STAGE=$(mktemp -d)
else mkdir -p "$PREFIX"; STAGE=$(mktemp -d "$PREFIX/.staging.XXXXXX"); fi
trap '[ -z "${_P:-}" ] || _killtree "$_P"; rm -rf "$STAGE"' EXIT  # a signal also stops the supervised child tree
trap 'exit 130' INT HUP TERM  # signals run the EXIT trap too
# No install-time budget: a supervised step is stopped (whole process group killed and reaped, the install FAILS) only when it
# shows no progress for STALL_S seconds (hung child). Time the lifecycle reports as "waiting for the user" (the asked-for
# sudo/firewall prompt, via the KEYLOOM_WAIT_FILE it creates while waiting) counts as progress.
STALL_S=300  # a step is stopped only after this long with no output at all: a hung child, never a slow disk or network
# --- sup begin (pure sh, tested under dash). No job control: there is no controlling terminal to rely on, so the child's
# process TREE is found through /proc (parent links) and only that tree is stopped, killed and reaped.
_tree() { # _tree PID: PID and every descendant, space separated
  _root=$1; _all=""
  for _f in /proc/[0-9]*/stat; do
    { read -r _s < "$_f"; } 2>/dev/null || continue
    _q=${_f#/proc/}; _q=${_q%/stat}; _r=${_s##*) }; set -- $_r; _all="$_all $_q:$2"
  done
  _out=$_root; _new=$_root
  while [ -n "$_new" ]; do
    _nx=""
    for _c in $_new; do for _e in $_all; do if [ "${_e#*:}" = "$_c" ]; then _nx="$_nx ${_e%%:*}"; fi; done; done
    _new=$_nx; _out="$_out$_nx"
  done
  echo $_out
}
_live() { # running (a finished-but-unreaped child is a zombie: not live)
  { read -r _s < "/proc/$1/stat"; } 2>/dev/null || return 1
  _r=${_s##*) }; [ "${_r%% *}" != Z ]
}
_killtree() { # freeze the tree first (a stopped process cannot fork), re-scan, then kill and reap
  _a=$(_tree "$1"); kill -STOP $_a 2>/dev/null
  _b=$(_tree "$1"); kill -STOP $_b 2>/dev/null
  kill -KILL $_b $_a 2>/dev/null; wait "$1" 2>/dev/null; return 0
}
# sup LABEL CMD...: heartbeat every second (liveness, plus the size of $PF while it grows); kill the tree when $PF has not
# grown for STALL_S seconds. stdin is kept.
sup() {
  _L=$1; shift
  "$@" <&0 & _P=$!
  _N=0; _T=0; _Z=0; _S=-1
  while _live "$_P"; do
    sleep 0.2; _T=$((_T + 1))
    [ $((_T % 5)) -eq 0 ] || continue
    _N=$((_N + 1))
    _W=$(wc -c < "$PF" 2>/dev/null || echo 0)
    if [ "$_W" != "$_S" ]; then _S=$_W; _Z=0; else _Z=$((_Z + 1)); fi
    if [ "$_Z" -ge "$STALL_S" ]; then
      _killtree "$_P"; _P=""
      echo "FAILED: $_L made no progress for ${STALL_S}s; stopped and reaped" >&2; return 124
    fi
    echo "[$_L] supervisor heartbeat: ${_N}s (liveness only)${PF:+; $_W bytes written so far}"
  done
  _R=0; wait "$_P" || _R=$?; _P=""; return $_R
}
# --- sup end
echo "Unpacking bootstrap ..."
PF="$STAGE/boot.tgz"
sup bootstrap sh -c 'tail -c +"$1" "$2" | head -c "$3" > "$4/boot.tgz" && [ "$(sha256sum < "$4/boot.tgz" | cut -d" " -f1)" = "$5" ] \
  && tar -xzf "$4/boot.tgz" -C "$4" && rm -f "$4/boot.tgz"' sh "$((STUB_BYTES + BOOT_OFFSET + 1))" "$SELF" "$BOOT_LEN" "$STAGE" "$BOOT_SHA256" \
  || { echo "ERROR: bootstrap could not be unpacked in time or its checksum differs (download damaged?)" >&2; exit 1; }
PF=""
PY="$STAGE/boot/python/bin/python3"; IP="$STAGE/boot/installer_payload.py"
# seg LABEL OFFSET LEN SHA256 DEST REQUIRE [FORBID]: supervised hash + extract of ONE segment (live byte counters + heartbeat)
seg() {
  "$PY" -I -B "$IP" extract --run "$SELF" --base "$STUB_BYTES" --label "$1" --offset "$2" --len "$3" --sha256 "$4" --dest "$5" \
    --require "$6" ${7:+--forbid "$7"}
}
if [ -n "$EXTRACT" ]; then  # explicit user request: both segments, no installation; the same stall guard applies
  mkdir -p "$EXTRACT"
  seg app "$APP_OFFSET" "$APP_LEN" "$APP_SHA256" "$EXTRACT" payload/ payload/runtime
  seg runtime "$RUNTIME_OFFSET" "$RUNTIME_LEN" "$RUNTIME_SHA256" "$EXTRACT" payload/runtime
  echo "Extracted to $EXTRACT"; exit 0
fi
seg app "$APP_OFFSET" "$APP_LEN" "$APP_SHA256" "$STAGE" payload/ payload/runtime
LC="$STAGE/payload/app/packaging/lifecycle.py"
# Runtime bytes are read only when needed. A repair restores from them. Otherwise `runtime-available` decides:
#   0 = the installed runtime (same runtime_id) is attached as a whole directory by the lifecycle: no runtime byte is read
#   1 = no such runtime here (fresh install / the runtime really changed): extract just the new runtime, with visible progress
#   3 = same runtime but not trusted: FAIL, a normal update never silently re-verifies/extracts it (use --repair)
#   anything else (error, crash, missing CLI, 124 timeout) = FAIL, never a silent full-runtime fallback
INPLACE=0
if [ "$VERB" != repair ]; then
  # Target classification FIRST (read-only, supervised; core's `target-state`): 0 = nothing to place (this build is active, or it is
  # recorded + inactive + identical + trusted and is activated where it stands), 1 = files must be placed (fresh/leftover/replace;
  # core's `install --payload` handles leftover/replace under its lock), 4 = refused (shared runtime in use), other = error.
  # Neither 0 nor 4 reads any runtime byte; everything else falls through to the runtime decision below.
  TS=0
  "$PY" -I -B "$IP" run --label target-state -- "$PY" -I -B "$LC" --prefix "$PREFIX" target-state \
    --manifest "$STAGE/payload/install-manifest.json" ${ADOPT:+--from "$ADOPT"} || TS=$?
  case "$TS" in
    0) CR=0
       "$PY" -I -B "$IP" run --label check -- "$PY" -I -B "$LC" --prefix "$PREFIX" check --version "@VERSION_ID@" || CR=$?
       if [ "$CR" = 0 ]; then echo "Nothing to do: this exact build is already installed and verified. Use --repair to restore damaged files."; exit 0; fi
       [ "$CR" = 1 ] || { echo "ERROR: the installed-build check failed (code $CR); nothing was changed." >&2; exit 1; }
       INPLACE=1; echo "This build is already installed (inactive): activating it in place; no files or runtime are placed.";;
    1) ;;
    4) echo "ERROR: this build's folder is damaged and other versions use its runtime. Run this installer with --repair." >&2; exit 4;;
    *) echo "ERROR: the target check failed (code $TS); nothing was changed." >&2; exit 1;;
  esac
fi
if [ "$VERB" = repair ]; then NEEDRT=1
elif [ "$INPLACE" = 1 ]; then NEEDRT=0
else
  RA=0
  "$PY" -I -B "$IP" run --label runtime-available -- "$PY" -I -B "$LC" --prefix "$PREFIX" runtime-available \
    --manifest "$STAGE/payload/install-manifest.json" ${ADOPT:+--from "$ADOPT"} || RA=$?
  case "$RA" in
    0) NEEDRT=0; echo "Installed runtime $RUNTIME_ID will be attached; the runtime part of this installer is not read.";;
    1) NEEDRT=1;;
    3) echo "ERROR: the installed runtime matches but is not trusted for a fast update. Run this installer with --repair (full verification)." >&2; exit 3;;
    *) echo "ERROR: runtime check failed (code $RA); not falling back to extracting the whole runtime. Run with --repair." >&2; exit 1;;
  esac
fi
if [ "$NEEDRT" = 1 ]; then
  echo "Runtime $RUNTIME_ID must be extracted (not reused)."
  seg runtime "$RUNTIME_OFFSET" "$RUNTIME_LEN" "$RUNTIME_SHA256" "$STAGE" payload/runtime
fi
# Attestation: only after the runtime segment was fully authenticated (hash + size + members, `seg` returned success under set -e) and
# only for a normal install that extracted it. Repair, attach and in-place paths get none (an attached runtime is trusted by the
# lifecycle's runtime_trusted and not hashed; repair hashes everything). If the attest step fails, the lifecycle hashes the freshly
# extracted runtime itself, once. The non-runtime files are always hashed by the lifecycle.
NONCE=""
if [ "$NEEDRT" = 1 ] && [ "$VERB" != repair ]; then
  if "$PY" -I -B "$IP" attest --label attest --lifecycle "$LC" --dest "$STAGE/payload" --producer segments --out "$STAGE/nonce"; then
    NONCE=$(cat "$STAGE/nonce")
  fi
fi
set +e
# install/repair are supervised too (own process group, heartbeat, SIGTERM then SIGKILL on a stall; the lifecycle's
# journal makes an interrupted run recoverable). The lifecycle creates $KEYLOOM_WAIT_FILE (content = why) while it waits for
# the user (sudo/firewall consent) or runs a visible network/model transfer: that counts as progress and the heartbeat says so.
# No password is ever read here.
KEYLOOM_WAIT_FILE="$STAGE/waiting-for-user"; export KEYLOOM_WAIT_FILE
set -- "$@"
if [ "$VERB" = repair ]; then  # repair restores from the payload's runtime bytes: never --attach-runtime
  if [ "$MODELS_SET" = 1 ] && [ "$MODELS" = full ]; then set -- --models "$@"; fi
  set -- repair --payload "$STAGE/payload" --firewall "$FIREWALL" "$@"
elif [ "$INPLACE" = 1 ]; then  # recorded identical inactive target: core journals and activates it where it stands
  set -- install --version "@VERSION_ID@" --models "$MODELS" --firewall "$FIREWALL" ${ADOPT:+--adopt-from "$ADOPT"} "$@"
else
  [ "$NEEDRT" = 1 ] || set -- --attach-runtime ${ADOPT:+--runtime-src "$ADOPT"} "$@"  # whole-directory attach; fails if unavailable
  set -- install --payload "$STAGE/payload" --models "$MODELS" --firewall "$FIREWALL" ${ADOPT:+--adopt-from "$ADOPT"} ${NONCE:+--payload-attested "$NONCE"} "$@"
fi
"$PY" -I -B "$IP" run --label "$VERB" --log "$PREFIX/install.log" --log-title "Keyloom @VERSION_ID@ linux-offline installer" --wait-file "$KEYLOOM_WAIT_FILE" -- "$PY" -I -B "$LC" --prefix "$PREFIX" "$@"
RC=$?
exit $RC
__PAYLOAD_BELOW__
