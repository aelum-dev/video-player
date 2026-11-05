#!/usr/bin/env bash
# Build the UI test harness into ./build/ (gitignored).
#
# Needs: wayland-scanner, cc, wayland-client, and write access to /dev/uinput
# for the input tools (uiclick/udrag/ukey/uwheel). The capture tool needs a
# compositor that offers wlr-screencopy (Hyprland does).
set -euo pipefail
cd "$(dirname "$0")"

mkdir -p build
# One directory for everything the harness writes (captures, fixtures). Safe to
# delete at any time; nothing here is an input.
mkdir -p /tmp/videoplayer
# shot.c includes "screencopy-client-protocol.h", so the header has to be
# generated under that name; a hand-written protocol XML is wrong in the
# details (capture_output_region's new_id comes first, ready() carries
# tv_sec_hi/tv_sec_lo/tv_nsec) — use the vendored one.
wayland-scanner private-code wlr-screencopy-unstable-v1.xml build/screencopy-protocol.c
wayland-scanner client-header wlr-screencopy-unstable-v1.xml build/screencopy-client-protocol.h
cc -O2 -Ibuild -o build/shot shot.c build/screencopy-protocol.c -lwayland-client

for tool in uiclick udrag ukey uwheel; do
    cc -O2 -o "build/$tool" "$tool.c"
done

chmod +x pxstats.py
echo "built build/{shot,uiclick,udrag,ukey,uwheel} — usage in README.md"
echo "test artifacts go to /tmp/videoplayer/"
