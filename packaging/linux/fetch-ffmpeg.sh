#!/bin/sh
# Fetch the pinned static Linux FFmpeg (GPL build, FFmpeg 7.0.2 from johnvansickle.com) into DIR/bin.
# backend/ffmpeg_tools accepts any non-development release >= 7 on Linux; the Windows 8.1.2 pin does not apply here.
# Integrity: sha256 pinned below (computed from the archive; its md5 also equals the one the site publishes).
#   sh packaging/linux/fetch-ffmpeg.sh [DIR]     -> prints DIR/bin ; pass that as --ffmpeg-dir
set -eu
DIR="${1:-$HOME/.cache/ck-ffmpeg}"
SHA=abda8d77ce8309141f83ab8edf0596834087c52467f6badf376a6a2a4c87cf67
URL=https://johnvansickle.com/ffmpeg/releases/ffmpeg-7.0.2-amd64-static.tar.xz
mkdir -p "$DIR"
if [ ! -x "$DIR/bin/ffmpeg" ]; then
  curl -fsSL -o "$DIR/ff.tar.xz" "$URL"
  [ "$(sha256sum "$DIR/ff.tar.xz" | cut -d' ' -f1)" = "$SHA" ] || { echo "ffmpeg checksum mismatch" >&2; rm -f "$DIR/ff.tar.xz"; exit 1; }
  rm -rf "$DIR/bin.tmp"; mkdir "$DIR/bin.tmp"
  tar -xf "$DIR/ff.tar.xz" -C "$DIR/bin.tmp" --strip-components=1
  mv "$DIR/bin.tmp" "$DIR/bin"
fi
echo "$DIR/bin"
