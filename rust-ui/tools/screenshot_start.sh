#!/bin/bash
# usage: screenshot_start.sh WxH Dark|Light FIXTURE OUT.png [extra app args...]  (sandboxed Xvfb, HOME=/tmp, sparse model files)
set -euo pipefail
rt=$(cd -- "$(dirname -- "$0")/.." && pwd)
size=$1 theme=$2 fx=$3 out=$4; shift 4
CK_DISPLAY_SIZE=${size} exec bash "$rt/tools/virtual-display-test.sh" /bin/bash -c '
set -euo pipefail
export HOME=/tmp
mkdir -p /tmp/.config/keyloom
printf "{\"version\":1,\"interface\":{\"theme\":\"%s\"}}" "$2" > /tmp/.config/keyloom/settings.json
/home/npittas/EZ-CorridorKey2/.venv/bin/python "$1/tools/screenshot_models.py" >/dev/null
exec /usr/bin/timeout 30 "$1/target/release/keyloom-ui" --window "$3" --recent-fixture "$4" --screenshot "$5" --screenshot-after 3 "${@:6}"
' sh "$rt" "$theme" "$size" "$fx" "$out" "$@"
