# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0](https://github.com/ckir/flux/compare/v0.2.0...v0.3.0) - 2026-09-25

### Bug fixes

- *(release)* the GitHub release published no notes at all
- *(flux-core)* stop discard destroying the error taxonomy
- *(core)* poison the walk before panicking on a bad entry name
- *(core)* refuse a directory entry name that is not one component
- *(flux-core)* a rename of a missing source fails instead of conjuring the destination
- *(flux-core)* the fake mints identity at creation instead of hashing the path
- *(flux-platform)* refuse a same-object publish where identity cannot answer
- *(flux-platform)* refuse a name published onto itself on macOS too
- *(flux-platform)* stop the same-object veto front-running the filesystem
- *(flux-platform)* close the same-object hole where identity is unavailable
- *(flux-platform)* veto a same-object publish on the Windows arm
- *(flux-platform)* reject interior NULs, and pin the atomic body's error order
- *(test)* macOS refuses non-UTF-8 filenames, so skip rather than assume
- *(platform)* derive Windows metadata and identity from one handle
- *(test)* let the symlink-follow test build on non-Windows
- *(flux-platform)* Windows must ask the same question as Unix
- *(flux-platform)* ask whether we can write the destination, not whether a bit is set
- *(flux-platform)* rename_no_replace must judge the name too
- *(flux-platform)* the read-only guard must judge the name, not its target
- *(flux-platform)* refuse to publish over a read-only destination

### Build and CI

- publish cargo doc to GitHub Pages

### Features

- *(flux-cli)* wire flux copy to the engine
- *(core)* refuse an entry that stopped being a directory
- *(core)* ancestor-set cycle detection gated on Strong identity
- *(core)* the ordered depth-first walk
- *(fs)* add read_dir and create_dir across the trait and all implementors
- *(fs)* [**breaking**] Metadata carries file_type instead of is_file
- *(flux-core)* FaultFs carries configurable object identities
- *(flux-fs)* Metadata carries a file identity
- *(flux-core)* copy_file, with metadata applied before publication
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- scaffold the Rust workspace and dev toolchain

### Refactoring

- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile

## [0.2.0](https://github.com/ckir/flux/compare/v0.1.1...v0.2.0) - 2026-09-25

### Bug fixes

- *(release)* the changelog dropped every commit under crates/
- *(flux-core)* stop discard destroying the error taxonomy
- *(core)* poison the walk before panicking on a bad entry name
- *(core)* refuse a directory entry name that is not one component
- *(flux-core)* a rename of a missing source fails instead of conjuring the destination
- *(flux-core)* the fake mints identity at creation instead of hashing the path
- *(flux-platform)* refuse a same-object publish where identity cannot answer
- *(flux-platform)* refuse a name published onto itself on macOS too
- *(flux-platform)* stop the same-object veto front-running the filesystem
- *(flux-platform)* close the same-object hole where identity is unavailable
- *(flux-platform)* veto a same-object publish on the Windows arm
- *(flux-platform)* reject interior NULs, and pin the atomic body's error order
- *(test)* macOS refuses non-UTF-8 filenames, so skip rather than assume
- *(platform)* derive Windows metadata and identity from one handle
- *(test)* let the symlink-follow test build on non-Windows
- *(flux-platform)* Windows must ask the same question as Unix
- *(flux-platform)* ask whether we can write the destination, not whether a bit is set
- *(flux-platform)* rename_no_replace must judge the name too
- *(flux-platform)* the read-only guard must judge the name, not its target
- *(flux-platform)* refuse to publish over a read-only destination

### Build and CI

- track the current stable release, 1.98.1, as the declared rust-version
- raise the declared MSRV to 1.88, which is what the code has needed
- *(model)* notify and resolve only act on main
- *(model)* the failure issue closes again when the tier recovers
- *(model)* the extended tier opens an issue when it fails
- *(model)* split breaklock-remote posix, fixed runs into their own job
- *(model)* move the deep breaklock liveness run to the nightly tier
- publish cargo doc to GitHub Pages

### Features

- *(flux-cli)* wire flux copy to the engine
- *(core)* refuse an entry that stopped being a directory
- *(core)* ancestor-set cycle detection gated on Strong identity
- *(core)* the ordered depth-first walk
- *(fs)* add read_dir and create_dir across the trait and all implementors
- *(fs)* [**breaking**] Metadata carries file_type instead of is_file
- *(flux-core)* FaultFs carries configurable object identities
- *(flux-fs)* Metadata carries a file identity
- *(flux-core)* copy_file, with metadata applied before publication
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- add guarded push and pr recipes
- scaffold the Rust workspace and dev toolchain

### Other

- Merge remote-tracking branch 'origin/main' into spec/flux-fs-copy
- Revert "temp: force an extended-tier failure to verify the notify job"
- Revert "temp: fail the breaklock liveness run fast too, for the same verification"
- Revert "temp: fail every extended-tier run fast, so notify can be verified at all"
- fail every extended-tier run fast, so notify can be verified at all
- fail the breaklock liveness run fast too, for the same verification
- force an extended-tier failure to verify the notify job

### Refactoring

- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile

## [0.1.1](https://github.com/ckir/flux/compare/v0.1.0...v0.1.1) - 2026-09-21

### Build and CI

- build the release binary from the package that defines it

## [0.1.0](https://github.com/ckir/flux/releases/tag/v0.1.0) - 2026-09-21

### Build and CI

- a release profile for the shipped binary: `opt-level = 3`, fat LTO, one codegen unit, `panic = "abort"`, symbols stripped ([#18](https://github.com/ckir/flux/pull/18))

### Miscellaneous

- scaffold the Rust workspace and dev toolchain
