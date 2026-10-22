#!/bin/bash
# GPU variant of isolated-test.sh: same isolation (no display, input, D-Bus, network, home), plus
# ONLY the NVIDIA compute device nodes. Root is read-only (model assets included); writable:
# rust-ui/ and a tmpfs /tmp for scratch and every cache. Original isolated-test.sh is untouched.
set -euo pipefail
repo=$(cd -- "$(dirname -- "$0")/../.." && pwd)
gpu=${CK_GPU_INDEX:-0}
models=${CORRIDORKEY_V2_MODELS_DIR:-$HOME/.local/share/EZ-CorridorKey2/CorridorKeyV2}
binds=()
for n in /dev/nvidiactl /dev/nvidia-uvm /dev/nvidia-uvm-tools "/dev/nvidia$gpu"; do
  [ -e "$n" ] || { echo "missing device node $n" >&2; exit 2; }
  binds+=(--dev-bind "$n" "$n")
done
exec /usr/bin/bwrap --unshare-all --die-with-parent --new-session \
  --ro-bind / / --bind "$repo/rust-ui" "$repo/rust-ui" \
  --dev /dev --proc /proc --tmpfs /tmp --tmpfs /run "${binds[@]}" \
  --clearenv --setenv PATH /usr/bin:/bin --setenv HOME /tmp \
  --setenv XDG_CACHE_HOME /tmp/cache --setenv HF_HOME /tmp/hf --setenv TORCH_HOME /tmp/torch \
  --setenv HF_HUB_OFFLINE 1 --setenv TRANSFORMERS_OFFLINE 1 \
  --setenv CORRIDORKEY_V2_MODELS_DIR "$models" \
  --setenv CORRIDORKEY_V2_SCRATCH_DIR /tmp/ck-scratch \
  --setenv CARGO_HOME /home/npittas/.cargo --setenv RUSTC /usr/bin/rustc \
  --setenv XDG_RUNTIME_DIR /tmp/ck-runtime \
  --chdir "$repo/rust-ui" /bin/bash -c \
  'mkdir -m 700 -p /tmp/ck-runtime /tmp/ck-scratch; exec "$@"' gpu-isolated-test "$@"
