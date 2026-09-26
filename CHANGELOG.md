# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.5.0](https://github.com/ckir/flux/compare/v0.4.0...v0.5.0) - 2026-09-26

### Bug fixes

- *(tests)* the tool table also refuses an entry the hook cannot evaluate
- *(tests)* the tool table refuses entries the hook skips, and values with line breaks
- *(just)* check-linux checks its prerequisites on native Linux too, and keys its target dir on the full path
- *(flux-core)* stop discard destroying the error taxonomy
- *(flux-core)* copy_tree refuses a destination directory that is the source root
- *(flux-core)* the fake created a file over a directory, and miscoded a missing component
- *(core)* poison the walk before panicking on a bad entry name
- *(core)* refuse a directory entry name that is not one component
- *(flux-core)* a rename of a missing source fails instead of conjuring the destination
- *(flux-core)* the fake mints identity at creation instead of hashing the path
- *(flux-fs)* state the interior-NUL rule instead of leaving it to two kernels
- *(flux-fs)* an unpaired surrogate smuggled any name past every check
- a name that is not one component, a create that followed links, and a twin status
- a wrapped name length, a directory that remove_file deleted, and a miscoded not-found
- *(flux-platform)* a handle-created file can read its own attributes on Windows
- *(flux-platform)* the handle rename_replace refuses a read-only target on Windows
- *(flux-platform)* the handle rename_replace refuses a read-only target on POSIX
- *(flux-platform)* report a directory as IsADirectory from remove_file on macOS
- *(flux-platform)* build the handle-relative stat paths on macOS and Linux clippy
- *(flux-platform)* let the variant carry the reparse bit, and pin the row the table was missing
- *(flux-platform)* stop reading every stat failure as a vanished name, and record why the rename has no fallback
- *(flux-platform)* reparse_tag_of broke every directory on a volume that cannot answer
- *(flux-platform)* ask the removal guard twice, and judge it on the tag
- snake-case the junction-root test, and drop a stray file from the tree
- *(flux-platform)* a guard that failed open, a race this cut exists to remove, and an unvalidated root
- *(flux-platform)* three capstone findings in the Windows handle arm
- *(flux-platform)* ungate std_file_from, and let both Windows arms share one Metadata builder
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

- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(docs)* generate the required-tools table from recommended-tools.json
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- *(flux-core)* a special file is skipped, a symlink fails; tree counts for --json
- *(flux-core)* a tree abort keeps its partial outcome
- *(flux-core)* copy_tree, the safe engine
- *(flux-core)* the tree outcome types
- *(flux-core)* under NoReplace, Step 2a refuses an existing destination before copying
- *(flux-core)* CopyError names the step a copy failed at
- *(flux-fs)* DirHandle::identity, the resolved directory's own identity
- *(flux-core)* refuse a copy onto the source itself by identity (Step 2a)
- *(core)* refuse an entry that stopped being a directory
- *(core)* ancestor-set cycle detection gated on Strong identity
- *(core)* the ordered depth-first walk
- *(fs)* add read_dir and create_dir across the trait and all implementors
- *(fs)* [**breaking**] Metadata carries file_type instead of is_file
- *(flux-core)* FaultFs carries configurable object identities
- *(flux-fs)* Metadata carries a file identity
- *(flux-core)* copy_file, with metadata applied before publication
- *(flux-fs)* add SYMLINK_CREATION_UNAVAILABLE
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the Windows DirHandle
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- Cargo.lock records flux-core's new dev-dependencies
- *(just)* check-linux and check-mac, the other two legs of the gate as commands
- *(typos)* exclude the spec by version pattern, not by its literal name
- scaffold the Rust workspace and dev toolchain

### Refactoring

- *(flux-core)* copy_file writes through a destination handle (copy_file_at)
- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

## [0.4.0](https://github.com/ckir/flux/compare/v0.3.0...v0.4.0) - 2026-09-25

### Bug fixes

- *(flux-platform)* ungate std_file_from, and let both Windows arms share one Metadata builder
- *(flux-core)* stop discard destroying the error taxonomy
- *(flux-core)* the fake created a file over a directory, and miscoded a missing component
- *(core)* poison the walk before panicking on a bad entry name
- *(core)* refuse a directory entry name that is not one component
- *(flux-core)* a rename of a missing source fails instead of conjuring the destination
- *(flux-core)* the fake mints identity at creation instead of hashing the path
- *(flux-fs)* state the interior-NUL rule instead of leaving it to two kernels
- *(flux-fs)* an unpaired surrogate smuggled any name past every check
- a name that is not one component, a create that followed links, and a twin status
- a wrapped name length, a directory that remove_file deleted, and a miscoded not-found
- *(flux-platform)* report a directory as IsADirectory from remove_file on macOS
- *(flux-platform)* build the handle-relative stat paths on macOS and Linux clippy
- *(flux-platform)* let the variant carry the reparse bit, and pin the row the table was missing
- *(flux-platform)* stop reading every stat failure as a vanished name, and record why the rename has no fallback
- *(flux-platform)* reparse_tag_of broke every directory on a volume that cannot answer
- *(flux-platform)* ask the removal guard twice, and judge it on the tag
- snake-case the junction-root test, and drop a stray file from the tree
- *(flux-platform)* a guard that failed open, a race this cut exists to remove, and an unvalidated root
- *(flux-platform)* three capstone findings in the Windows handle arm
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

- lint on macOS as well as Linux
- allowlist the POSIX open flag names for typos
- publish cargo doc to GitHub Pages

### Features

- *(flux-platform)* the Windows DirHandle
- *(flux-cli)* wire flux copy to the engine
- *(core)* refuse an entry that stopped being a directory
- *(core)* ancestor-set cycle detection gated on Strong identity
- *(core)* the ordered depth-first walk
- *(fs)* add read_dir and create_dir across the trait and all implementors
- *(fs)* [**breaking**] Metadata carries file_type instead of is_file
- *(flux-core)* FaultFs carries configurable object identities
- *(flux-fs)* Metadata carries a file identity
- *(flux-core)* copy_file, with metadata applied before publication
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- *(just)* arm auto-merge on every PR `just pr` opens
- scaffold the Rust workspace and dev toolchain

### Other

- Merge remote-tracking branch 'origin/main' into feat/handle-relative-writes

### Refactoring

- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

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
