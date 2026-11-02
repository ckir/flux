#!/usr/bin/env bash
# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/
#
# make_dest.sh N DIR: a flat directory of N zero-byte files named exactly as `make_tree.py flat DIR --files N`
# names them (f000000, f000001, ...), so a GATE-B source and destination hold the same names.
set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: make_dest.sh N DIR" >&2
    exit 2
fi
n=$1
dir=$2
case "$n" in
    '' | *[!0-9]*)
        echo "make_dest.sh: N must be a non-negative integer: $n" >&2
        exit 2
        ;;
esac
if [ -e "$dir" ] && [ ! -d "$dir" ]; then
    echo "make_dest.sh: not a directory: $dir" >&2
    exit 1
fi
if [ -d "$dir" ] && [ -n "$(ls -A "$dir")" ]; then
    echo "make_dest.sh: directory is not empty: $dir" >&2
    exit 1
fi
mkdir -p "$dir"
cd "$dir"
awk -v n="$n" 'BEGIN { for (i = 0; i < n; i++) printf "f%06d\0", i }' | xargs -0 -n 5000 touch --
echo "$n"
