#!/bin/sh
# Build the C oracle: tests/c-oracle/build.sh <output-binary>
# Needs a C compiler and glib (spice-common's log.c uses it).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
common="$root/spice-gtk/subprojects/spice-common"
out=${1:-"$here/build/oracle"}
mkdir -p "$(dirname "$out")"
${CC:-cc} -O1 -w -I"$here" -I"$common" -I"$common/common" -I"$root/spice-protocol" \
    "$here/oracle.c" "$common/common/quic.c" "$common/common/lz.c" "$common/common/log.c" \
    $(pkg-config --cflags --libs glib-2.0) -o "$out"
