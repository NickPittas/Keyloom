#!/bin/bash
# usage: screenshot_page.sh WxH Dark|Light FLAG FIXTURE OUT.png [extra app args...]  (sandboxed Xvfb, HOME=/tmp, sparse model files)
# e.g. screenshot_page.sh 1920x1080 Dark --models-fixture tests/fixtures/models-30.json out.png
set -euo pipefail
rt=$(cd -- "$(dirname -- "$0")/.." && pwd)
size=$1 theme=$2 flag=$3 fx=$4 out=$5; shift 5
CK_DISPLAY_SIZE=${size} exec bash "$rt/tools/virtual-display-test.sh" /bin/bash -c '
set -euo pipefail
export HOME=/tmp
mkdir -p /tmp/.config/keyloom
printf "{\"version\":1,\"interface\":{\"theme\":\"%s\"}}" "$2" > /tmp/.config/keyloom/settings.json
/home/npittas/EZ-CorridorKey2/.venv/bin/python "$1/tools/screenshot_models.py" >/dev/null
exec /usr/bin/timeout 30 "$1/target/release/keyloom-ui" --window "$3" "$4" "$5" --screenshot "$6" --screenshot-after 3 "${@:7}"
' sh "$rt" "$theme" "$size" "$flag" "$fx" "$out" "$@"
