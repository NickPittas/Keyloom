#!/bin/bash
# Always enter the isolated namespace before starting a display or a test.
set -euo pipefail
tools_dir=$(cd -- "$(dirname -- "$0")" && pwd)
exec bash "$tools_dir/isolated-test.sh" /bin/bash -c '
set -euo pipefail
ulimit -c 0
mkdir -p /tmp/.X11-unix
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json
# CK_DISPLAY_SIZE lets a run use a larger (still private, still Xvfb) screen, e.g. 2400x1500 for a
# large-window evidence shot. The default stays the acceptance size.
/home/npittas/.cache/corridorkey-test-display/Xvfb :99 -screen 0 "${CK_DISPLAY_SIZE:-1440x900}"x24 -nolisten tcp -extension GLX >/tmp/xvfb.log 2>&1 &
display_pid=$!
trap "kill $display_pid 2>/dev/null || true" EXIT
export DISPLAY=:99
for attempt in {1..50}; do
  [ -S /tmp/.X11-unix/X99 ] && break
  kill -0 "$display_pid" || { cat /tmp/xvfb.log; exit 1; }
  sleep 0.1
done
[ -S /tmp/.X11-unix/X99 ] || { cat /tmp/xvfb.log; exit 1; }
"$@"
' virtual-display-test "$@"
