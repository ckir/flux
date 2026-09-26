# Flux Roadmap

Release scope follows spec §77–§80; the build order follows spec §81. The State
column below is the build order's progress, each state naming the merged pull
requests behind it (checked 2026-09-26).

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
| 2 | Portable copy — files, directories, metadata, errors, statistics | in progress (PR #32: files, metadata, errors; PR #48: single-file copies write through a directory handle; directories and full statistics not merged) |
| 3 | Streaming — scanner, selection, bounded queues, planner, workers; demonstrate bounded memory | not started |
| 4 | Deterministic topology — ordered traversal, `FileIdentity`, hardlink canonicalization, persistent topology store, excluded members, dependency-aware links | in progress (PR #36, #37: ordered traversal and `FileIdentity`; hardlink canonicalization, topology store, excluded members and dependency-aware links not merged) |
| 5 | Persistent operation state — manifest, operation ID, lock, heartbeat, state database, partial-file mapping, crash recovery, cleanup | not started |
| 6 | Verification — BLAKE3, source-stream hashing, destination verification, `verify` command | not started |
| 7 | Safety — filesystem identity, destination nesting, mount boundaries, symlink policy, source mutation | in progress (PR #32: source mutation, symlink not followed; PR #48: filesystem identity gate; destination nesting and mount boundaries not merged) |
| 8 | UX — progress, JSON, logging, dry-run, statistics | not started |
| 9 | Advanced filesystem operations — resume, checkpoint validation, atomic replacement, capacity planning, sparse, reflink | not started |
| 10 | Performance — worker count, queue depth, buffer size, filesystem APIs, metadata calls, topology lookup, small-file scheduling | not started |

Phase 10 is measured, not guessed: performance regressions must be benchmarked
before and after (spec §76).
