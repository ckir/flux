# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.9.0](https://github.com/ckir/flux/compare/v0.8.0...v0.9.0) - 2026-10-09

### Bug fixes

- failed listings exit 1, cross-platform e2e snapshot and destination, lease-gate reason text, single-file pointer guard (cut 9b final fixes)
- report a lock this run created and could not remove when the run fails
- *(flux-core)* stop discard destroying the error taxonomy
- the cleanup lock acquisition applies the lease gate to a dead owner too; skipped rows are reported; pass order pinned (cut 9b)
- discovery - failed listings are reported, refusals proven before any probe, an untrusted lock is an UNCERTAIN row (cut 9b)
- final review follow-ups - foreign-claim pin, count() mapping, nested key pins, known limits 6 and 11 (cut 9a)
- compare resume paths through absolute_lexical so the tests hold on Windows (cut 9a)
- an adopted workspace survives a refused-unchanged copy; pin single-file adoption fields (cut 9a)
- refuse a fold onto an entry another target owns, make learned spellings O(1), document that closing the store commits (cut 8b)
- created directories plan targets as new, pin the second claim and the symlink target (cut 8b)
- create state.db after the workspace is published (Windows refuses to rename a directory with an open file) (cut 8b)
- separator-insensitive call-log counts in the NameIndex tests (cut 8b)
- separator-insensitive call-log assertions in the case-insensitive fake tests (cut 8b)
- separator-insensitive call-log assertion in the claim-store fake test (cut 8b)
- cut 8a final review fold (copy_tree containment, probe error cleanup reported, retire removes probe files)
- *(core)* a target name is one component or PATH_COMPONENT_INVALID; the ownership refusal says what to do; conformance matches whole labels (capstone round 2)
- *(core)* the give-up message names no flag a break-lock run already used; recover's comment states its reorder (quality review)
- open_lock takes no controlling terminal; the fake forgets a removed name's type (Task 2 quality review)
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
- *(fs)* read_file refuses a socket on both platforms; Windows lists one as Other (Part 3a capstone round 1)
- *(platform)* a name deleted while open vanishes at once on Windows (cut 7a Part 1 capstone)
- *(platform)* boot_session_id review fixes (Task 5 quality review)
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

- flux cleanup DEST - the command, its report and exit codes (cut 9b)
- a copy finishes a prior operation's recorded cleanup and removes workspace debris (cut 9b)
- flux copy --resume, the resume report lines and the cut 9a known limits (cut 9a)
- --resume adopts the one resumable prior as the same operation (cut 9a)
- resume::validate - the compatibility check for --resume (cut 9a)
- manifest format 3 - roots, options and the configuration fingerprint (cut 9a)
- a directory copy replaces existing files under claims, with skip and update (cut 8b)
- replacement counts, ClaimNotRecorded and the report fields (cut 8b)
- the copy path's before_create callback and the single-file existing-destination policy (cut 8b)
- ExistingPolicy, Outcome.skipped and the --overwrite/--update/--skip-existing flags (cut 8b)
- probe the destination for no-replace publication inside the unpublished workspace (cut 8a, Part P)
- render the mount-root and containment warnings (cut 8a)
- *(cli)* flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)
- *(cli)* a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)
- *(cli)* the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- the cleanup deletion pass - revalidate under the lock, leftovers first, workspace last (cut 9b)
- Classified::Orphan, obtain_cleanup_lock, checked_held, finish_retire, sweep_partials (cut 9b seams)
- cleanup discovery - the transient lock probe, the listing, the facts (cut 9b)
- the cleanup status table (cut 9b)
- cleanup artifact rules - validated relative partial names, deleted through handles (cut 9b)
- the walk skips files the prior run completed and claimed (cut 9a)
- the fake's open_claim_store, count and set_claim_store_format (cut 9a)
- RedbClaimStore::open_file and count, open_claim_store on both platforms (cut 9a)
- Code::StateCorrupt and IncompatibleState, ClaimStore::count, DirHandle::open_claim_store (cut 9a)
- guarded directory-end and final claim syncs (cut 8b)
- the workspace creates, passes to the walk and removes state.db (cut 8b)
- NameIndex, name resolution by listing and identity (cut 8b)
- DirHandle::create_claim_store on the fake, POSIX and Windows (cut 8b)
- refuse a destination whose resolved location lies inside the source, before the lock (cut 8a, Part A)
- never merge into a pre-existing destination mount root (cut 8a, Part M)
- DirHandle::canonical_path from the open handle on every platform (cut 8a, Part A's query)
- DirHandle::mount_root on the fake, POSIX and Windows (cut 8a, Part M's query)
- *(core)* RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)
- *(core)* the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)
- *(core)* --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)
- *(core)* the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)
- *(core)* Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)
- *(core)* a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)
- *(core)* retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)
- *(core)* copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)
- *(core)* copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)
- *(core)* the §21.1 prior-state scan for trees and single files (cut 7a Part 3a)
- *(core)* the crash-safe state write, and a workspace that never exists without its manifest (cut 7a Part 3a)
- *(core)* the version-1 operation state codec and the three state refusal codes (cut 7a Part 3a)
- *(fs)* DirHandle::sync flushes a directory (cut 7a Part 3a)
- *(fs)* DirHandle::remove_dir, refusing files, links and full directories (cut 7a Part 3a)
- *(fs)* DirHandle::read_file, refusing links, directories and special files (cut 7a Part 3a)
- *(fs)* DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part 3a)
- *(core)* obtain a destination lock through the checked protocol (cut 7a Part 2)
- *(core)* take an uncertain lock over in place (§240.5) (cut 7a Part 2)
- *(core)* recover a dead owner's lock (§240.3) (cut 7a Part 2)
- *(core)* classify an existing lock (§240.1) (cut 7a Part 2)
- *(core)* acquire a destination lock (§96.1) and hold it (cut 7a Part 2)
- *(core)* lock errors, lock sites, keys and workspace trust (cut 7a Part 2)
- *(core)* the lock record codec, format_version 1 (cut 7a Part 1)
- *(core)* random operation and owner-instance ids (cut 7a Part 1)
- lock capability from a built-in local allowlist (cut 7a Part 1, F1)
- lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)
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
- the redb claim store and its crash acceptance test (cut 8b)
- the claim model, FluxPathKey, the ClaimStore trait and its conformance suite (cut 8b)
- *(fs)* TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)
- *(fs)* LockCapability and the LockFile trait (cut 7a Part 1)
- *(flux-fs)* add SYMLINK_CREATION_UNAVAILABLE
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(platform)* the host's boot session id (cut 7a Part 1)
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the Windows DirHandle
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- just pr merges at once when GitHub refuses --auto on an already-mergeable PR
- cleanup of the deferred review findings (cut 8b)
- scaffold the Rust workspace and dev toolchain
- rustfmt the heartbeat copy tests
- wrap the open_lock flag comment

### Other

- the 5 s heartbeat, overridable in a debug build; end to end, a stalled run's lock record shows it
- the heartbeat during the copy, retried once; its failure is one error naming the lock, and a torn record ends with nothing written
- return the published target's identity, read from the temporary's handle before the rename; prepare_file returns the source's
- Pulse::beat's doc says what it returns after a failure (capstone r2, DEBT)
- --restart heartbeats before each ownership check of its sweep; a failure there stops the run at the lock step
- a heartbeat callback beside the guard, every 64 KiB and before each guarded mutation; its failure aborts a tree
- Held::heartbeat rewrites the record's time without a flush; a rewrite keeps the newest time
- a failed COMPLETED write leaves a FAILED record the next run can read
- a single-file record's artifact_type is "state" - the artifact's role, not the operation's kind
- write version 2 - the single-file record's §249.1 fields at creation, one time for the state and its lock record; the COMPLETED write carries cleanup_pending, the leftovers and the published target's identity (§218)
- FileHandle::identity - a file's identity from its own handle, POSIX fstat, Windows FileIdInfo, and the fake
- version 2 - cleanup_pending, the single-file record's §249.1 fields, identity text, native-unit hex; version 1 still read and never upgraded

### Performance

- size the copy buffer from the source, capped at 256 KiB

### Refactoring

- *(flux-core)* copy_file writes through a destination handle (copy_file_at)
- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

## [0.8.0](https://github.com/ckir/flux/compare/v0.7.0...v0.8.0) - 2026-10-09

### Bug fixes

- failed listings exit 1, cross-platform e2e snapshot and destination, lease-gate reason text, single-file pointer guard (cut 9b final fixes)
- report a lock this run created and could not remove when the run fails
- *(flux-core)* stop discard destroying the error taxonomy
- the cleanup lock acquisition applies the lease gate to a dead owner too; skipped rows are reported; pass order pinned (cut 9b)
- discovery - failed listings are reported, refusals proven before any probe, an untrusted lock is an UNCERTAIN row (cut 9b)
- final review follow-ups - foreign-claim pin, count() mapping, nested key pins, known limits 6 and 11 (cut 9a)
- compare resume paths through absolute_lexical so the tests hold on Windows (cut 9a)
- an adopted workspace survives a refused-unchanged copy; pin single-file adoption fields (cut 9a)
- refuse a fold onto an entry another target owns, make learned spellings O(1), document that closing the store commits (cut 8b)
- created directories plan targets as new, pin the second claim and the symlink target (cut 8b)
- create state.db after the workspace is published (Windows refuses to rename a directory with an open file) (cut 8b)
- separator-insensitive call-log counts in the NameIndex tests (cut 8b)
- separator-insensitive call-log assertions in the case-insensitive fake tests (cut 8b)
- separator-insensitive call-log assertion in the claim-store fake test (cut 8b)
- cut 8a final review fold (copy_tree containment, probe error cleanup reported, retire removes probe files)
- *(core)* a target name is one component or PATH_COMPONENT_INVALID; the ownership refusal says what to do; conformance matches whole labels (capstone round 2)
- *(core)* the give-up message names no flag a break-lock run already used; recover's comment states its reorder (quality review)
- open_lock takes no controlling terminal; the fake forgets a removed name's type (Task 2 quality review)
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
- *(fs)* read_file refuses a socket on both platforms; Windows lists one as Other (Part 3a capstone round 1)
- *(platform)* a name deleted while open vanishes at once on Windows (cut 7a Part 1 capstone)
- *(platform)* boot_session_id review fixes (Task 5 quality review)
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

- the cleanup deletion pass - revalidate under the lock, leftovers first, workspace last (cut 9b)
- Classified::Orphan, obtain_cleanup_lock, checked_held, finish_retire, sweep_partials (cut 9b seams)
- flux cleanup DEST - the command, its report and exit codes (cut 9b)
- a copy finishes a prior operation's recorded cleanup and removes workspace debris (cut 9b)
- flux copy --resume, the resume report lines and the cut 9a known limits (cut 9a)
- --resume adopts the one resumable prior as the same operation (cut 9a)
- resume::validate - the compatibility check for --resume (cut 9a)
- manifest format 3 - roots, options and the configuration fingerprint (cut 9a)
- a directory copy replaces existing files under claims, with skip and update (cut 8b)
- replacement counts, ClaimNotRecorded and the report fields (cut 8b)
- the copy path's before_create callback and the single-file existing-destination policy (cut 8b)
- ExistingPolicy, Outcome.skipped and the --overwrite/--update/--skip-existing flags (cut 8b)
- probe the destination for no-replace publication inside the unpublished workspace (cut 8a, Part P)
- render the mount-root and containment warnings (cut 8a)
- *(cli)* flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)
- *(cli)* a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)
- *(cli)* the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- cleanup discovery - the transient lock probe, the listing, the facts (cut 9b)
- the cleanup status table (cut 9b)
- cleanup artifact rules - validated relative partial names, deleted through handles (cut 9b)
- the walk skips files the prior run completed and claimed (cut 9a)
- the fake's open_claim_store, count and set_claim_store_format (cut 9a)
- RedbClaimStore::open_file and count, open_claim_store on both platforms (cut 9a)
- Code::StateCorrupt and IncompatibleState, ClaimStore::count, DirHandle::open_claim_store (cut 9a)
- guarded directory-end and final claim syncs (cut 8b)
- the workspace creates, passes to the walk and removes state.db (cut 8b)
- NameIndex, name resolution by listing and identity (cut 8b)
- DirHandle::create_claim_store on the fake, POSIX and Windows (cut 8b)
- refuse a destination whose resolved location lies inside the source, before the lock (cut 8a, Part A)
- never merge into a pre-existing destination mount root (cut 8a, Part M)
- DirHandle::canonical_path from the open handle on every platform (cut 8a, Part A's query)
- DirHandle::mount_root on the fake, POSIX and Windows (cut 8a, Part M's query)
- *(core)* RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)
- *(core)* the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)
- *(core)* --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)
- *(core)* the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)
- *(core)* Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)
- *(core)* a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)
- *(core)* retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)
- *(core)* copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)
- *(core)* copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)
- *(core)* the §21.1 prior-state scan for trees and single files (cut 7a Part 3a)
- *(core)* the crash-safe state write, and a workspace that never exists without its manifest (cut 7a Part 3a)
- *(core)* the version-1 operation state codec and the three state refusal codes (cut 7a Part 3a)
- *(fs)* DirHandle::sync flushes a directory (cut 7a Part 3a)
- *(fs)* DirHandle::remove_dir, refusing files, links and full directories (cut 7a Part 3a)
- *(fs)* DirHandle::read_file, refusing links, directories and special files (cut 7a Part 3a)
- *(fs)* DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part 3a)
- *(core)* obtain a destination lock through the checked protocol (cut 7a Part 2)
- *(core)* take an uncertain lock over in place (§240.5) (cut 7a Part 2)
- *(core)* recover a dead owner's lock (§240.3) (cut 7a Part 2)
- *(core)* classify an existing lock (§240.1) (cut 7a Part 2)
- *(core)* acquire a destination lock (§96.1) and hold it (cut 7a Part 2)
- *(core)* lock errors, lock sites, keys and workspace trust (cut 7a Part 2)
- *(core)* the lock record codec, format_version 1 (cut 7a Part 1)
- *(core)* random operation and owner-instance ids (cut 7a Part 1)
- lock capability from a built-in local allowlist (cut 7a Part 1, F1)
- lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)
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
- the redb claim store and its crash acceptance test (cut 8b)
- the claim model, FluxPathKey, the ClaimStore trait and its conformance suite (cut 8b)
- *(fs)* TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)
- *(fs)* LockCapability and the LockFile trait (cut 7a Part 1)
- *(flux-fs)* add SYMLINK_CREATION_UNAVAILABLE
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(platform)* the host's boot session id (cut 7a Part 1)
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the Windows DirHandle
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- cleanup of the deferred review findings (cut 8b)
- scaffold the Rust workspace and dev toolchain
- rustfmt the heartbeat copy tests
- wrap the open_lock flag comment

### Other

- the 5 s heartbeat, overridable in a debug build; end to end, a stalled run's lock record shows it
- the heartbeat during the copy, retried once; its failure is one error naming the lock, and a torn record ends with nothing written
- return the published target's identity, read from the temporary's handle before the rename; prepare_file returns the source's
- Pulse::beat's doc says what it returns after a failure (capstone r2, DEBT)
- --restart heartbeats before each ownership check of its sweep; a failure there stops the run at the lock step
- a heartbeat callback beside the guard, every 64 KiB and before each guarded mutation; its failure aborts a tree
- Held::heartbeat rewrites the record's time without a flush; a rewrite keeps the newest time
- a failed COMPLETED write leaves a FAILED record the next run can read
- a single-file record's artifact_type is "state" - the artifact's role, not the operation's kind
- write version 2 - the single-file record's §249.1 fields at creation, one time for the state and its lock record; the COMPLETED write carries cleanup_pending, the leftovers and the published target's identity (§218)
- FileHandle::identity - a file's identity from its own handle, POSIX fstat, Windows FileIdInfo, and the fake
- version 2 - cleanup_pending, the single-file record's §249.1 fields, identity text, native-unit hex; version 1 still read and never upgraded

### Performance

- size the copy buffer from the source, capped at 256 KiB

### Refactoring

- *(flux-core)* copy_file writes through a destination handle (copy_file_at)
- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

## [0.7.0](https://github.com/ckir/flux/compare/v0.6.0...v0.7.0) - 2026-10-09

### Bug fixes

- report a lock this run created and could not remove when the run fails
- *(flux-core)* stop discard destroying the error taxonomy
- final review follow-ups - foreign-claim pin, count() mapping, nested key pins, known limits 6 and 11 (cut 9a)
- compare resume paths through absolute_lexical so the tests hold on Windows (cut 9a)
- an adopted workspace survives a refused-unchanged copy; pin single-file adoption fields (cut 9a)
- refuse a fold onto an entry another target owns, make learned spellings O(1), document that closing the store commits (cut 8b)
- created directories plan targets as new, pin the second claim and the symlink target (cut 8b)
- create state.db after the workspace is published (Windows refuses to rename a directory with an open file) (cut 8b)
- separator-insensitive call-log counts in the NameIndex tests (cut 8b)
- separator-insensitive call-log assertions in the case-insensitive fake tests (cut 8b)
- separator-insensitive call-log assertion in the claim-store fake test (cut 8b)
- cut 8a final review fold (copy_tree containment, probe error cleanup reported, retire removes probe files)
- *(core)* a target name is one component or PATH_COMPONENT_INVALID; the ownership refusal says what to do; conformance matches whole labels (capstone round 2)
- *(core)* the give-up message names no flag a break-lock run already used; recover's comment states its reorder (quality review)
- open_lock takes no controlling terminal; the fake forgets a removed name's type (Task 2 quality review)
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
- *(fs)* read_file refuses a socket on both platforms; Windows lists one as Other (Part 3a capstone round 1)
- *(platform)* a name deleted while open vanishes at once on Windows (cut 7a Part 1 capstone)
- *(platform)* boot_session_id review fixes (Task 5 quality review)
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

- flux copy --resume, the resume report lines and the cut 9a known limits (cut 9a)
- --resume adopts the one resumable prior as the same operation (cut 9a)
- resume::validate - the compatibility check for --resume (cut 9a)
- manifest format 3 - roots, options and the configuration fingerprint (cut 9a)
- a directory copy replaces existing files under claims, with skip and update (cut 8b)
- replacement counts, ClaimNotRecorded and the report fields (cut 8b)
- the copy path's before_create callback and the single-file existing-destination policy (cut 8b)
- ExistingPolicy, Outcome.skipped and the --overwrite/--update/--skip-existing flags (cut 8b)
- probe the destination for no-replace publication inside the unpublished workspace (cut 8a, Part P)
- render the mount-root and containment warnings (cut 8a)
- *(cli)* flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)
- *(cli)* a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)
- *(cli)* the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- the walk skips files the prior run completed and claimed (cut 9a)
- the fake's open_claim_store, count and set_claim_store_format (cut 9a)
- RedbClaimStore::open_file and count, open_claim_store on both platforms (cut 9a)
- Code::StateCorrupt and IncompatibleState, ClaimStore::count, DirHandle::open_claim_store (cut 9a)
- guarded directory-end and final claim syncs (cut 8b)
- the workspace creates, passes to the walk and removes state.db (cut 8b)
- NameIndex, name resolution by listing and identity (cut 8b)
- DirHandle::create_claim_store on the fake, POSIX and Windows (cut 8b)
- refuse a destination whose resolved location lies inside the source, before the lock (cut 8a, Part A)
- never merge into a pre-existing destination mount root (cut 8a, Part M)
- DirHandle::canonical_path from the open handle on every platform (cut 8a, Part A's query)
- DirHandle::mount_root on the fake, POSIX and Windows (cut 8a, Part M's query)
- *(core)* RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)
- *(core)* the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)
- *(core)* --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)
- *(core)* the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)
- *(core)* Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)
- *(core)* a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)
- *(core)* retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)
- *(core)* copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)
- *(core)* copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)
- *(core)* the §21.1 prior-state scan for trees and single files (cut 7a Part 3a)
- *(core)* the crash-safe state write, and a workspace that never exists without its manifest (cut 7a Part 3a)
- *(core)* the version-1 operation state codec and the three state refusal codes (cut 7a Part 3a)
- *(fs)* DirHandle::sync flushes a directory (cut 7a Part 3a)
- *(fs)* DirHandle::remove_dir, refusing files, links and full directories (cut 7a Part 3a)
- *(fs)* DirHandle::read_file, refusing links, directories and special files (cut 7a Part 3a)
- *(fs)* DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part 3a)
- *(core)* obtain a destination lock through the checked protocol (cut 7a Part 2)
- *(core)* take an uncertain lock over in place (§240.5) (cut 7a Part 2)
- *(core)* recover a dead owner's lock (§240.3) (cut 7a Part 2)
- *(core)* classify an existing lock (§240.1) (cut 7a Part 2)
- *(core)* acquire a destination lock (§96.1) and hold it (cut 7a Part 2)
- *(core)* lock errors, lock sites, keys and workspace trust (cut 7a Part 2)
- *(core)* the lock record codec, format_version 1 (cut 7a Part 1)
- *(core)* random operation and owner-instance ids (cut 7a Part 1)
- lock capability from a built-in local allowlist (cut 7a Part 1, F1)
- lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)
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
- the redb claim store and its crash acceptance test (cut 8b)
- the claim model, FluxPathKey, the ClaimStore trait and its conformance suite (cut 8b)
- *(fs)* TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)
- *(fs)* LockCapability and the LockFile trait (cut 7a Part 1)
- *(flux-fs)* add SYMLINK_CREATION_UNAVAILABLE
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(platform)* the host's boot session id (cut 7a Part 1)
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the Windows DirHandle
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- cleanup of the deferred review findings (cut 8b)
- scaffold the Rust workspace and dev toolchain
- rustfmt the heartbeat copy tests
- wrap the open_lock flag comment

### Other

- the 5 s heartbeat, overridable in a debug build; end to end, a stalled run's lock record shows it
- the heartbeat during the copy, retried once; its failure is one error naming the lock, and a torn record ends with nothing written
- return the published target's identity, read from the temporary's handle before the rename; prepare_file returns the source's
- Pulse::beat's doc says what it returns after a failure (capstone r2, DEBT)
- --restart heartbeats before each ownership check of its sweep; a failure there stops the run at the lock step
- a heartbeat callback beside the guard, every 64 KiB and before each guarded mutation; its failure aborts a tree
- Held::heartbeat rewrites the record's time without a flush; a rewrite keeps the newest time
- a failed COMPLETED write leaves a FAILED record the next run can read
- a single-file record's artifact_type is "state" - the artifact's role, not the operation's kind
- write version 2 - the single-file record's §249.1 fields at creation, one time for the state and its lock record; the COMPLETED write carries cleanup_pending, the leftovers and the published target's identity (§218)
- FileHandle::identity - a file's identity from its own handle, POSIX fstat, Windows FileIdInfo, and the fake
- version 2 - cleanup_pending, the single-file record's §249.1 fields, identity text, native-unit hex; version 1 still read and never upgraded

### Performance

- size the copy buffer from the source, capped at 256 KiB

### Refactoring

- *(flux-core)* copy_file writes through a destination handle (copy_file_at)
- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

## [0.6.0](https://github.com/ckir/flux/compare/v0.5.0...v0.6.0) - 2026-10-08

### Bug fixes

- final review follow-ups - foreign-claim pin, count() mapping, nested key pins, known limits 6 and 11 (cut 9a)
- refuse a fold onto an entry another target owns, make learned spellings O(1), document that closing the store commits (cut 8b)
- create state.db after the workspace is published (Windows refuses to rename a directory with an open file) (cut 8b)
- report a lock this run created and could not remove when the run fails
- *(flux-core)* stop discard destroying the error taxonomy
- compare resume paths through absolute_lexical so the tests hold on Windows (cut 9a)
- an adopted workspace survives a refused-unchanged copy; pin single-file adoption fields (cut 9a)
- created directories plan targets as new, pin the second claim and the symlink target (cut 8b)
- separator-insensitive call-log counts in the NameIndex tests (cut 8b)
- separator-insensitive call-log assertions in the case-insensitive fake tests (cut 8b)
- separator-insensitive call-log assertion in the claim-store fake test (cut 8b)
- cut 8a final review fold (copy_tree containment, probe error cleanup reported, retire removes probe files)
- *(core)* a target name is one component or PATH_COMPONENT_INVALID; the ownership refusal says what to do; conformance matches whole labels (capstone round 2)
- *(core)* the give-up message names no flag a break-lock run already used; recover's comment states its reorder (quality review)
- open_lock takes no controlling terminal; the fake forgets a removed name's type (Task 2 quality review)
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
- *(fs)* read_file refuses a socket on both platforms; Windows lists one as Other (Part 3a capstone round 1)
- *(platform)* a name deleted while open vanishes at once on Windows (cut 7a Part 1 capstone)
- *(platform)* boot_session_id review fixes (Task 5 quality review)
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

- cache the pinned TLC jar across the model jobs
- restrict the CI token to contents: read
- the benchmark workflows run on main only
- measure and publish the benchmarks; the harness tests; the bench pages in the docs deploy
- *(deps)* bump actions/download-artifact from 7 to 8
- publish cargo doc to GitHub Pages

### Features

- flux copy --resume, the resume report lines and the cut 9a known limits (cut 9a)
- the redb claim store and its crash acceptance test (cut 8b)
- --resume adopts the one resumable prior as the same operation (cut 9a)
- resume::validate - the compatibility check for --resume (cut 9a)
- manifest format 3 - roots, options and the configuration fingerprint (cut 9a)
- a directory copy replaces existing files under claims, with skip and update (cut 8b)
- replacement counts, ClaimNotRecorded and the report fields (cut 8b)
- the copy path's before_create callback and the single-file existing-destination policy (cut 8b)
- ExistingPolicy, Outcome.skipped and the --overwrite/--update/--skip-existing flags (cut 8b)
- probe the destination for no-replace publication inside the unpublished workspace (cut 8a, Part P)
- render the mount-root and containment warnings (cut 8a)
- *(cli)* flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)
- *(cli)* a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)
- *(cli)* the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- the walk skips files the prior run completed and claimed (cut 9a)
- the fake's open_claim_store, count and set_claim_store_format (cut 9a)
- RedbClaimStore::open_file and count, open_claim_store on both platforms (cut 9a)
- Code::StateCorrupt and IncompatibleState, ClaimStore::count, DirHandle::open_claim_store (cut 9a)
- guarded directory-end and final claim syncs (cut 8b)
- the workspace creates, passes to the walk and removes state.db (cut 8b)
- NameIndex, name resolution by listing and identity (cut 8b)
- DirHandle::create_claim_store on the fake, POSIX and Windows (cut 8b)
- refuse a destination whose resolved location lies inside the source, before the lock (cut 8a, Part A)
- never merge into a pre-existing destination mount root (cut 8a, Part M)
- DirHandle::canonical_path from the open handle on every platform (cut 8a, Part A's query)
- DirHandle::mount_root on the fake, POSIX and Windows (cut 8a, Part M's query)
- *(core)* RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)
- *(core)* the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)
- *(core)* --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)
- *(core)* the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)
- *(core)* Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)
- *(core)* a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)
- *(core)* retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)
- *(core)* copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)
- *(core)* copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)
- *(core)* the §21.1 prior-state scan for trees and single files (cut 7a Part 3a)
- *(core)* the crash-safe state write, and a workspace that never exists without its manifest (cut 7a Part 3a)
- *(core)* the version-1 operation state codec and the three state refusal codes (cut 7a Part 3a)
- *(fs)* DirHandle::sync flushes a directory (cut 7a Part 3a)
- *(fs)* DirHandle::remove_dir, refusing files, links and full directories (cut 7a Part 3a)
- *(fs)* DirHandle::read_file, refusing links, directories and special files (cut 7a Part 3a)
- *(fs)* DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part 3a)
- *(core)* obtain a destination lock through the checked protocol (cut 7a Part 2)
- *(core)* take an uncertain lock over in place (§240.5) (cut 7a Part 2)
- *(core)* recover a dead owner's lock (§240.3) (cut 7a Part 2)
- *(core)* classify an existing lock (§240.1) (cut 7a Part 2)
- *(core)* acquire a destination lock (§96.1) and hold it (cut 7a Part 2)
- *(core)* lock errors, lock sites, keys and workspace trust (cut 7a Part 2)
- *(core)* the lock record codec, format_version 1 (cut 7a Part 1)
- *(core)* random operation and owner-instance ids (cut 7a Part 1)
- lock capability from a built-in local allowlist (cut 7a Part 1, F1)
- lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)
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
- the claim model, FluxPathKey, the ClaimStore trait and its conformance suite (cut 8b)
- *(fs)* TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)
- *(fs)* LockCapability and the LockFile trait (cut 7a Part 1)
- *(flux-fs)* add SYMLINK_CREATION_UNAVAILABLE
- *(flux-fs)* add the DirHandle and DestinationRoot traits
- *(flux-fs)* add the no-replace publication and namespace collision codes
- *(fs)* add Code::DirectoryChangedDuringScan
- *(fs)* add FileType and DirEntry
- *(flux-fs)* portable object identity with its reliability class
- *(flux-fs)* the FileSystem and FileHandle traits
- *(flux-fs)* copy options, outcome, and the normative temp name
- *(flux-fs)* the spec's error codes and the io::Error mapping
- *(platform)* the host's boot session id (cut 7a Part 1)
- *(flux-platform)* the Windows handle-relative helpers
- *(flux-platform)* the Windows DirHandle
- *(flux-platform)* the POSIX DirHandle
- *(flux-platform)* publish without replacing atomically
- *(flux-platform)* real object identity on both platforms
- *(flux-platform)* StdFileSystem over std::fs

### Miscellaneous

- cleanup of the deferred review findings (cut 8b)
- scaffold the Rust workspace and dev toolchain
- rustfmt the heartbeat copy tests
- wrap the open_lock flag comment

### Other

- Model/lock protocol ([#61](https://github.com/ckir/flux/pull/61))
- both dispatches are attempted after a publish; the name rule says what it checks (capstone r2)
- a hung copy times out; calibrations do not pile up (capstone r1)
- the performance section
- the trend page
- publish results to the data branch, redoing the update on a rejected push
- data.json, stability.json and latest.svg
- time Flux against the comparators, check every copy, write result-<os>.json
- the rounds, the rotation and the three cases
- the comparator registry and its rules
- a single-file record's artifact_type is "state" - the artifact's role, not the operation's kind
- the preliminary speed probe - Flux against robocopy and FastCopy, and the two throughput gaps
- the 5 s heartbeat, overridable in a debug build; end to end, a stalled run's lock record shows it
- the heartbeat during the copy, retried once; its failure is one error naming the lock, and a torn record ends with nothing written
- return the published target's identity, read from the temporary's handle before the rename; prepare_file returns the source's
- Pulse::beat's doc says what it returns after a failure (capstone r2, DEBT)
- --restart heartbeats before each ownership check of its sweep; a failure there stops the run at the lock step
- a heartbeat callback beside the guard, every 64 KiB and before each guarded mutation; its failure aborts a tree
- Held::heartbeat rewrites the record's time without a flush; a rewrite keeps the newest time
- a failed COMPLETED write leaves a FAILED record the next run can read
- write version 2 - the single-file record's §249.1 fields at creation, one time for the state and its lock record; the COMPLETED write carries cleanup_pending, the leftovers and the published target's identity (§218)
- FileHandle::identity - a file's identity from its own handle, POSIX fstat, Windows FileIdInfo, and the fake
- version 2 - cleanup_pending, the single-file record's §249.1 fields, identity text, native-unit hex; version 1 still read and never upgraded

### Performance

- size the copy buffer from the source, capped at 256 KiB

### Refactoring

- *(flux-core)* copy_file writes through a destination handle (copy_file_at)
- *(fs)* sort the tests by name instead of deriving Ord on FileType
- *(core)* key FaultFs by PathBuf and unify its type state
- *(flux-core)* make a leaking `?` fail to compile
- *(flux-platform)* separate the reparse judgement from the syscalls, so its branches can be tested

## [0.5.0](https://github.com/ckir/flux/compare/v0.4.0...v0.5.0) - 2026-10-01

### Bug fixes

- *(core)* a target name is one component or PATH_COMPONENT_INVALID; the ownership refusal says what to do; conformance matches whole labels (capstone round 2)
- *(tests)* the tool table also refuses an entry the hook cannot evaluate
- *(tests)* the tool table refuses entries the hook skips, and values with line breaks
- *(just)* check-linux checks its prerequisites on native Linux too, and keys its target dir on the full path
- *(flux-core)* stop discard destroying the error taxonomy
- *(core)* the give-up message names no flag a break-lock run already used; recover's comment states its reorder (quality review)
- open_lock takes no controlling terminal; the fake forgets a removed name's type (Task 2 quality review)
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
- *(fs)* read_file refuses a socket on both platforms; Windows lists one as Other (Part 3a capstone round 1)
- *(platform)* a name deleted while open vanishes at once on Windows (cut 7a Part 1 capstone)
- *(platform)* boot_session_id review fixes (Task 5 quality review)
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

- check-mac builds C for macOS with zig and now covers the whole workspace
- publish cargo doc to GitHub Pages

### Features

- *(core)* the version-1 operation state codec and the three state refusal codes (cut 7a Part 3a)
- *(core)* random operation and owner-instance ids (cut 7a Part 1)
- *(platform)* the host's boot session id (cut 7a Part 1)
- *(flux-cli)* record lines, warnings, summary and the §53 JSON report
- *(docs)* generate the required-tools table from recommended-tools.json
- *(cli)* flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)
- *(cli)* a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)
- *(cli)* the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)
- *(flux-cli)* flux copy copies folders, reports, and exits per §55
- *(flux-cli)* resolve SOURCE and DEST per §4.1, K6 and K7
- *(flux-cli)* a library target, and the §55 exit codes
- *(flux-fs)* Safety on CopyOptions and identity_degraded on Outcome
- *(flux-cli)* wire flux copy to the engine
- *(core)* RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)
- *(core)* the single-file run - B1 checks first, the record beside the target, the same finish (cut 7a Part 3b-1)
- *(core)* --restart supersedes each resumable prior under a full still_owned before every mutation (cut 7a Part 3b-1)
- *(core)* the run - a tree copy under the destination's lock, from the capability gate to the release (cut 7a Part 3b-1)
- *(core)* Held::rewrite_record rewrites the record in place, keeping its owner (Q-K, cut 7a Part 3b-1)
- *(core)* a single file's records are looked up by the target's own name (E3, cut 7a Part 3b-1)
- *(core)* retire a workspace before removing it, remove a record with its temporary, check the control plane (cut 7a Part 3b-1)
- *(core)* copy_tree_at writes through a given root under a guard; reserved .flux paths are refused (cut 7a Part 3b-1)
- *(core)* copy_file_guarded runs a guard before every destination mutation (cut 7a Part 3b-1)
- *(core)* the §21.1 prior-state scan for trees and single files (cut 7a Part 3a)
- *(core)* the crash-safe state write, and a workspace that never exists without its manifest (cut 7a Part 3a)
- *(fs)* DirHandle::sync flushes a directory (cut 7a Part 3a)
- *(fs)* DirHandle::remove_dir, refusing files, links and full directories (cut 7a Part 3a)
- *(fs)* DirHandle::read_file, refusing links, directories and special files (cut 7a Part 3a)
- *(fs)* DirHandle::read_dir on POSIX, Windows and the fake (cut 7a Part 3a)
- *(core)* obtain a destination lock through the checked protocol (cut 7a Part 2)
- *(core)* take an uncertain lock over in place (§240.5) (cut 7a Part 2)
- *(core)* recover a dead owner's lock (§240.3) (cut 7a Part 2)
- *(core)* classify an existing lock (§240.1) (cut 7a Part 2)
- *(core)* acquire a destination lock (§96.1) and hold it (cut 7a Part 2)
- *(core)* lock errors, lock sites, keys and workspace trust (cut 7a Part 2)
- *(core)* the lock record codec, format_version 1 (cut 7a Part 1)
- lock capability from a built-in local allowlist (cut 7a Part 1, F1)
- lock files through a directory handle, with a non-blocking OS-native lock (cut 7a Part 1)
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
- *(fs)* TARGET_LOCK_BUSY and CONTROL_PLANE_NAMESPACE_CONFLICT codes for the engine (cut 7a Part 3b-1)
- *(fs)* LockCapability and the LockFile trait (cut 7a Part 1)
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

- every text file is LF in the working tree, whatever core.autocrlf says
- Cargo.lock records flux-core's new dev-dependencies
- *(just)* check-linux and check-mac, the other two legs of the gate as commands
- *(typos)* exclude the spec by version pattern, not by its literal name
- scaffold the Rust workspace and dev toolchain
- wrap the open_lock flag comment

### Other

- Merge branch 'main' into spec/cut-7a

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
