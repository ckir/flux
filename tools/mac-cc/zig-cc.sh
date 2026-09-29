#!/bin/sh
# The macOS C compiler for build scripts in `just check-mac` on a non-mac Unix host (see the justfile).
# Not measured on such a host yet; the Windows .cmd twin is.
exec zig cc -target aarch64-macos "$@"
