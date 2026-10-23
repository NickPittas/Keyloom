#!/bin/bash
# Keep tests away from the user's compositor, session bus, input devices and files.
set -euo pipefail
repo=$(cd -- "$(dirname -- "$0")/../.." && pwd)
# Optional larger (private) Xvfb screen for large-window evidence; unset = the acceptance size.
env_extra=()
if [ -n "${CK_DISPLAY_SIZE:-}" ]; then
  env_extra+=(--setenv CK_DISPLAY_SIZE "$CK_DISPLAY_SIZE")
fi
exec /usr/bin/bwrap --unshare-all --die-with-parent --new-session \
  --ro-bind / / --bind "$repo/rust-ui" "$repo/rust-ui" \
  --dev /dev --proc /proc --tmpfs /tmp --tmpfs /run \
  --clearenv --setenv PATH /usr/bin:/bin --setenv HOME /tmp \
  --setenv CARGO_HOME /home/npittas/.cargo --setenv RUSTC /usr/bin/rustc \
  --setenv XDG_RUNTIME_DIR /tmp/ck-runtime "${env_extra[@]}" \
  --chdir "$repo/rust-ui" /bin/bash -c \
  'mkdir -m 700 -p /tmp/ck-runtime; exec "$@"' isolated-test "$@"
