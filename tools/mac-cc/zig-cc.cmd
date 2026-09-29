@rem The macOS C compiler for build scripts in `just check-mac` on a Windows host (see the justfile).
@zig cc -target aarch64-macos %*
