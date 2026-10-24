#!/bin/sh
# Copyright (c) 2026 Costas Kirgoussios
#
# This software is licensed under the PolyForm Noncommercial License 1.0.0.
# You may obtain a copy of the License at:
# https://polyformproject.org/licenses/noncommercial/1.0.0/

# The macOS archiver for build scripts in `just check-mac` on a non-mac Unix host (see the justfile).
# Not measured on such a host yet; the Windows .cmd twin is.
exec zig ar "$@"
