#!/bin/sh
# Unprivileged older-glibc build of the Rust UI: pinned zig (task-local, sha256-checked) acts as the
# C compiler/linker with a glibc 2.31 target, so no docker/root/sysroot is needed. The UI links only
# libc/libm/libgcc_s (wgpu/wayland/x11 are dlopen'd at run time), so no native -dev packages are required.
#   sh packaging/linux/build-ui-old-glibc.sh            # resumable: rerun after any interruption/timeout
#   -> rust-ui/target-oldglibc/x86_64-unknown-linux-gnu/release/keyloom-ui  (+ glibc floor check)
# Env: CK_TOOLCHAIN (default ~/.cache/ck-toolchain), CK_GLIBC (default 2.31), CK_CARGO_FLAGS (default --locked).
set -eu
ZIG_VER=0.14.1
ZIG_SHA=24aeeec8af16c381934a6cd7d95c807a8cb2cf7df9fa40d359aa884195c4716c
TC="${CK_TOOLCHAIN:-$HOME/.cache/ck-toolchain}"
GLIBC="${CK_GLIBC:-2.31}"
REPO=$(cd "$(dirname "$0")/../.." && pwd)
ZIG_DIR="$TC/zig-x86_64-linux-$ZIG_VER"
mkdir -p "$TC"
if [ ! -x "$ZIG_DIR/zig" ]; then
  curl -fsSL -o "$TC/zig.tar.xz" "https://ziglang.org/download/$ZIG_VER/zig-x86_64-linux-$ZIG_VER.tar.xz"
  [ "$(sha256sum "$TC/zig.tar.xz" | cut -d' ' -f1)" = "$ZIG_SHA" ] || { echo "zig checksum mismatch" >&2; rm -f "$TC/zig.tar.xz"; exit 1; }
  tar -C "$TC" -xf "$TC/zig.tar.xz"
fi
# cc/linker shim: rustc asks for -lgcc_s, which zig supplies as libunwind.
cat > "$TC/zigcc" <<EOF
#!/bin/sh
for a; do shift; case "\$a" in -lgcc_s) a=-lunwind;; esac; set -- "\$@" "\$a"; done
exec "$ZIG_DIR/zig" cc -target x86_64-linux-gnu.$GLIBC "\$@"
EOF
chmod +x "$TC/zigcc"
cd "$REPO/rust-ui"
export CARGO_TARGET_DIR="$REPO/rust-ui/target-oldglibc"
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$TC/zigcc"
export CC_x86_64_unknown_linux_gnu="$TC/zigcc"
export RUSTC="${RUSTC:-/usr/bin/rustc}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
# shellcheck disable=SC2086
/usr/bin/cargo build --release --target x86_64-unknown-linux-gnu --bin keyloom-ui ${CK_CARGO_FLAGS:---locked}
OUT="$CARGO_TARGET_DIR/x86_64-unknown-linux-gnu/release/keyloom-ui"
python3 - "$OUT" "$GLIBC" "$REPO/packaging" <<'PY'
import sys
sys.path.insert(0, sys.argv[3])
import bundle
best = (0, 0)
for m in bundle.GLIBC_RE.finditer(open(sys.argv[1], "rb").read()):
    best = max(best, (int(m[1]), int(m[2])))
print("glibc floor of %s = %d.%d" % (sys.argv[1], *best))
sys.exit(0 if best <= tuple(map(int, sys.argv[2].split("."))) else "glibc floor above target")
PY
