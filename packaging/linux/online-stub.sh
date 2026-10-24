#!/bin/sh
# Keyloom (powered by CorridorKey) @VERSION_ID@ online installer for Linux (needs only sh, tar, gzip, head, tail, sha256sum).
# Carries Keyloom itself (app, UI, FFmpeg, bundled Python and every small dependency). Only the large public wheels
# (torch, triton, NVIDIA CUDA libraries) are downloaded from PyPI / download.pytorch.org: no account, no sign-in.
# No root (only an optional, asked-for sudo for UFW rules). Configures a per-user network host. Models download from the app.
# User data, settings, models, network identities and jobs are never touched.
set -eu
# Layout after this stub: boot.tar.gz (minimal Python + installer_payload.py), online.tar.gz (keyloom-online/: payload/ and
# wheel-sources.json). Independent gzip streams; offsets are relative to the first byte after the stub, which is STUB_BYTES long.
STUB_BYTES=@STUB_BYTES@;
BOOT_OFFSET=@BOOT_OFFSET@; BOOT_LEN=@BOOT_LEN@; BOOT_SHA256="@BOOT_SHA256@"
ONLINE_OFFSET=@ONLINE_OFFSET@; ONLINE_LEN=@ONLINE_LEN@; ONLINE_SHA256="@ONLINE_SHA256@"
# A pre-rename install is upgraded in place; only a fresh machine gets the Keyloom folder.
# A pre-rename install (~/.local/opt/CorridorKey) is taken over: Keyloom installs into ~/.local/opt/Keyloom, reuses the installed
# files, moves the startup entry/launcher/network service across and leaves the old folder as the rollback holder.
OLD="$HOME/.local/opt/CorridorKey"; ADOPT=""; PREFIX_SET=0
PREFIX="${KEYLOOM_PREFIX:-${CORRIDORKEY_PREFIX:-$HOME/.local/opt/Keyloom}}"; REPAIR=""; FIREWALL=ask
usage() { cat <<U
Usage: $(basename "$0") [--prefix DIR] [--repair] [--firewall ask|skip]
  default       install or update Keyloom @VERSION_ID@ into PREFIX ($PREFIX); files of the installed versions
                are reused, only the wheels still needed are downloaded (several GB on a first install)
  --repair      re-verify and restore damaged files of @VERSION_ID@
  --firewall    ask (default): if UFW is active, ask once before using sudo to allow this PC's trusted networks
                (TCP 8443-8452, UDP 44554); without a terminal the step is left pending. skip: never ask
Quit Keyloom first (File > Quit).
U
}
while [ $# -gt 0 ]; do
  case "$1" in
    --repair) REPAIR=1; shift;;
    --prefix) [ $# -ge 2 ] || { echo "--prefix needs a value" >&2; exit 64; }; PREFIX="$2"; PREFIX_SET=1; shift 2;;
    --firewall) [ $# -ge 2 ] || { echo "--firewall needs a value" >&2; exit 64; }; FIREWALL="$2"; shift 2;;
    -h|--help) usage; exit 0;;
    *) echo "unknown option $1" >&2; usage >&2; exit 64;;
  esac
done
if [ "$PREFIX_SET" = 0 ] && [ -z "${KEYLOOM_PREFIX:-}${CORRIDORKEY_PREFIX:-}" ] && [ -f "$OLD/receipt.json" ] && [ ! -f "$PREFIX/receipt.json" ]; then ADOPT="$OLD"; fi
case "$FIREWALL" in ask|skip) ;; *) echo "--firewall must be ask or skip" >&2; exit 64;; esac
[ "$(uname -m)" = x86_64 ] || { echo "ERROR: x86_64 Linux only" >&2; exit 1; }
SELF=$(readlink -f "$0")
mkdir -p "$PREFIX"
STAGE=$(mktemp -d "$PREFIX/.staging.XXXXXX")  # same filesystem as PREFIX (not a possibly small tmpfs /tmp); private to this run
trap '[ -z "${_P:-}" ] || _killtree "$_P"; rm -rf "$STAGE"' EXIT  # a signal also stops the supervised child tree
trap 'exit 130' INT HUP TERM
# Local time gates: each local phase <= PHASE_S, all together <= TOTAL_S (shared clock T0). A phase over its limit is stopped
# (its whole process tree killed and reaped) and the install FAILS; it is never extended or retried. The clock is paused only
# while online.py/lifecycle declare an explicit wait for the user or a visible download via $KEYLOOM_WAIT_FILE.
TOTAL_S=30; PHASE_S=10; T0=$(date +%s)
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
# sup LABEL LIMIT CMD...: heartbeat every second (liveness, plus the size of $PF while it grows); at LIMIT kill the tree. stdin is kept.
sup() {
  _L=$1; _D=$2; shift 2
  "$@" <&0 & _P=$!
  _N=0; _T=0
  while _live "$_P"; do
    sleep 0.2; _T=$((_T + 1))
    [ $((_T % 5)) -eq 0 ] || continue
    _N=$((_N + 1))
    if [ "$_N" -ge "$_D" ]; then
      _killtree "$_P"; _P=""
      echo "FAILED: $_L exceeded its ${_D}s limit; stopped and reaped" >&2; return 124
    fi
    echo "[$_L] supervisor heartbeat: ${_N}s of ${_D}s (liveness only)${PF:+; $(wc -c < "$PF" 2>/dev/null || echo 0) bytes written so far}"
  done
  _R=0; wait "$_P" || _R=$?; _P=""; return $_R
}
# --- sup end
echo "Unpacking bootstrap ..."
PF="$STAGE/boot.tgz"
sup bootstrap "$PHASE_S" sh -c 'tail -c +"$1" "$2" | head -c "$3" > "$4/boot.tgz" && [ "$(sha256sum < "$4/boot.tgz" | cut -d" " -f1)" = "$5" ] \
  && tar -xzf "$4/boot.tgz" -C "$4" && rm -f "$4/boot.tgz"' sh "$((STUB_BYTES + BOOT_OFFSET + 1))" "$SELF" "$BOOT_LEN" "$STAGE" "$BOOT_SHA256" \
  || { echo "ERROR: bootstrap could not be unpacked in time or its checksum differs (download damaged?)" >&2; exit 1; }
PF=""
PY="$STAGE/boot/python/bin/python3"; IP="$STAGE/boot/installer_payload.py"   # outside keyloom-online/payload: survives every move
lim() { "$PY" -I -B "$IP" limit --start "$T0" --total "$TOTAL_S" --phase "$1"; }
# Same build already installed and verified? Decided BEFORE the heavy payload is unpacked: the installed, owned lifecycle checks the
# exact version id built into this installer (fast ordinary check: version, pointer, manifest identity; no runtime rehash, no engine
# import, no GPU probe). Supervised like every other automatic step. A negative or failed probe just continues to the normal path.
if [ -z "$REPAIR" ] && [ -x "$PREFIX/current/runtime/python/bin/python3" ] && [ -f "$PREFIX/current/app/packaging/lifecycle.py" ]; then
  _CL=$(lim "$PHASE_S") || { echo "FAILED: local ${TOTAL_S}s budget spent before the installed-build check" >&2; exit 124; }
  _CR=0
  "$PY" -I -B "$IP" run --label check --deadline "$_CL" -- "$PREFIX/current/runtime/python/bin/python3" -I -B \
    "$PREFIX/current/app/packaging/lifecycle.py" --prefix "$PREFIX" check --version "@VERSION_ID@" || _CR=$?
  if [ "$_CR" = 0 ]; then
    echo "Nothing to do: Keyloom @VERSION_ID@ is already installed and verified. Use --repair to restore damaged files."; exit 0
  fi
fi
_SL=$(lim "$PHASE_S") || { echo "FAILED: local ${TOTAL_S}s budget spent before the payload step" >&2; exit 124; }
"$PY" -I -B "$IP" extract --run "$SELF" --base "$STUB_BYTES" --label online --offset "$ONLINE_OFFSET" --len "$ONLINE_LEN" \
  --sha256 "$ONLINE_SHA256" --dest "$STAGE" --deadline "$_SL" --require keyloom-online
W="$STAGE/keyloom-online"
set -- --payload-dir "$W/payload" --sources "$W/wheel-sources.json" --prefix "$PREFIX" --firewall "$FIREWALL"
[ -z "$REPAIR" ] || set -- "$@" --repair
[ -z "$ADOPT" ] || set -- "$@" --adopt-from "$ADOPT"
# online.py owns the reuse/attach decision (and passes --attach-runtime/--runtime-src through the lifecycle itself); it is
# supervised like every other automatic step. Its download phases (visible bytes/speed) and the sudo consent pause the clock
# only through $KEYLOOM_WAIT_FILE.
KEYLOOM_WAIT_FILE="$STAGE/waiting-for-user"; export KEYLOOM_WAIT_FILE
_IL=$(lim "$TOTAL_S") || { echo "FAILED: local ${TOTAL_S}s budget spent before the install step" >&2; exit 124; }
set +e
"$PY" -I -B "$IP" run --label online --deadline "$_IL" --wait-file "$KEYLOOM_WAIT_FILE" -- "$PY" -I -B "$W/payload/app/packaging/online.py" "$@"
RC=$?
[ "$RC" -ne 4 ] || echo "Keyloom is running: quit it (File > Quit), then run this installer again." >&2
exit $RC
__PAYLOAD_BELOW__
