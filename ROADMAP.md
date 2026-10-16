# Flux Roadmap

Release scope follows spec §77–§80; the build order follows spec §81. The State
column below is the build order's progress, each state naming the merged pull
requests behind it (checked 2026-10-14 against `main` at `5502b44`, PR #92).

## Release scope

### 0.1 — the working copier (spec §77)

recursive copy · multiple sources · parallel workers · bounded scanner queues ·
deterministic discovery · hardlink preservation · hardlink exclusions · overwrite ·
update · skip-existing · progress · statistics · dry-run · BLAKE3 source-stream
hashing · timestamp preservation · portable permissions · symlink copying ·
source-mutation detection · filesystem-aware safety · cross-platform builds ·
operation manifests · operation locking · persistent topology store

The architecture must already accommodate resume, atomic replacement, reflink and
sparse files even though they do not ship in 0.1.

### 0.2 — durability (spec §78)

resume · chunk/checkpoint validation · atomic replacement · capacity estimation ·
sparse files · reflinks · JSON output · cleanup · stale-operation management

### 0.3 — tuning (spec §79)

adaptive scheduler · small-file optimization · benchmark command · advanced
metadata · xattrs · ACLs where practical

### 0.4+ — potential (spec §80)

sync · delete · restore · network transfers · snapshots · advanced recovery ·
packed small-file transfers

## Build order (spec §81)

| Phase | Deliverable | State |
|---|---|---|
| 1 | Workspace — crates created and compiling | **done** (no PR — direct commit `7d86f7b`, before the PR workflow started) |
| 2 | Portable copy — files, directories, metadata, errors, statistics | **done** for files and trees (PR #32: files, metadata, errors; PR #48: single-file copies write through a directory handle; PR #52, #53: `copy_tree` and `flux copy` on folders; the `--json` report carries the statistics). Advanced metadata (xattrs, ACLs) is 0.3 scope, not built |
| 3 | Streaming — scanner, selection, bounded queues, planner, workers; demonstrate bounded memory | in progress (PR #37: the ordered walk; PR #52: the sequential tree copy). Selection and filtering, bounded queues, a planner and parallel copy workers are not built: the walk and the copy are sequential |
| 4 | Deterministic topology — ordered traversal, `FileIdentity`, hardlink canonicalization, persistent topology store, excluded members, dependency-aware links | in progress (PR #36, #37: ordered traversal and `FileIdentity`; hardlink canonicalization, the persistent topology store, excluded members and dependency-aware links not merged) |
| 5 | Persistent operation state — manifest, operation ID, lock, heartbeat, state database, partial-file mapping, crash recovery, cleanup | **done** (PR #57: the destination lock and operation state; PR #59: state version 2, `cleanup_pending` and the heartbeat; PR #63: claim records; PR #68, #89, #91: `--resume`; PR #74: `flux cleanup`; PR #78: recoverable publication under strict durability). The persistent topology store is phase 4. Two deliberate deviations: Flux installs no SIGINT/SIGTERM handler (an interrupt is a crash, recovered by the next run; spec §56.1, ruled 2026-10-14), and §23's recorded temporary path and `RESUME_INVALID` became a name derived from the operation id and a reported recopy (cut 9f-1, Ruling 2) |
| 6 | Verification — BLAKE3, source-stream hashing, destination verification, `verify` command | in progress (BLAKE3 in `flux-hash`; a resume verifies and skips the files a prior run completed, and `--resume-verify=full` hashes a partial against the source: PR #89, #91). The `verify` command, destination verification on a plain copy and the verification levels are not built (`--json` reports `verify_level: none`) |
| 7 | Safety — filesystem identity, destination nesting, mount boundaries, symlink policy, source mutation | in progress (PR #32: source mutation, symlink not followed; PR #36, #48: filesystem identity gate; PR #62: destination containment and refusing a destination mount root). Symlink copying (§25, `--links=copy|skip`) is done for tree copies (cut 10a). Source mount boundaries (§42, `--cross-filesystems`) are enforced by the walk (cut 10b); a destination-side bind mount of a source directory stays known limit 1 of cut 8a (TODO.md) |
| 8 | UX — progress, JSON, logging, dry-run, statistics | in progress (`--json` and the run report with statistics). Progress, logging and dry-run for `copy` are not built (`flux-ui` is an empty crate) |
| 9 | Advanced filesystem operations — resume, checkpoint validation, atomic replacement, capacity planning, sparse, reflink | in progress (PR #63: atomic replacement under claim records, with `--overwrite`, `--update` and `--skip-existing`; PR #68, #89, #91: resume with chunk checkpoint validation for trees and a single file). Capacity planning, sparse files and reflinks are not built |
| 10 | Performance — worker count, queue depth, buffer size, filesystem APIs, metadata calls, topology lookup, small-file scheduling | in progress (PR #67: buffer sizing; PR #60, #80: CI benchmarks and criterion benches; PR #82, #84: group commit and a parallel data-sync barrier for strict durability). Worker count, queue depth, filesystem APIs, small-file scheduling and topology lookup are not done |

Phase 10 is measured, not guessed: performance regressions must be benchmarked
before and after (spec §76).
