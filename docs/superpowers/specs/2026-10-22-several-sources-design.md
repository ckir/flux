# Several sources: `flux copy a b c DEST` (fresh copies only)

Status: DRAFT for owner review (2026-10-22). Branch `spec/several-sources` off main `1b69bd4`.
Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (cited as "spec §N"). Roadmap: ROADMAP.md, 0.1 scope item "multiple sources".

## 1. Intent

`flux copy` takes exactly one source today; several are a usage error (`TODO.md:231`). Spec §4.1's last row and §18.3
define the behaviour: with several sources, DEST is a folder (created if missing) and each source lands at
`DEST/<name>`, all under ONE operation workspace `DEST/.flux/operations/<operation-id>/`.

Why now: it is a stated 0.1 scope item, it needs no third-party crate (the walk is still sequential), and the manifest
already carries a `roots` list (`crates/flux-core/src/state.rs:61-71`, "exactly one today") waiting for it.

## 2. Goals, non-goals, anti-goals

Goals
- G1. `flux copy SRC1 SRC2 ... SRCn DEST` (n >= 2) copies each source, a folder or a file, to `DEST/<name>`.
- G2. One operation: one lock on the destination root, one workspace at `DEST/.flux/operations/<id>/`, one manifest
  whose `roots` has n entries, one claim store, one aggregated report.
- G3. Everything detectable before transfer is refused before ANY destination mutation (spec §18.3), including prefix
  collisions decided by the destination filesystem (`Data` vs `data`).
- G4. A one-source command behaves exactly as today. No existing test changes meaning.
- G5. The on-disk layout is the one the later resume / restart / dry-run cut will build on. The state format
  version 5 introduced here is that cut's format too, so it adds no second migration.

Non-goals (each is a later cut or stays in TODO.md)
- N1. `--resume`, `--restart`, `--break-lock`, `--resume-verify`, `--dry-run` with several sources: usage error (owner
  ruling 2026-10-22).
- N2. `--exclude` (own cut; the cut 11a spec N5 lists it separately).
- N3. Hardlink grouping across roots (phase 4 is not built). Only the ORDER that grouping needs is honoured (§3.6).
- N4. Parallel workers, queues, a planner (phase 3).
- N5. A per-root breakdown in `--json` (the 18 keys stay).
- N6. The two cut 11c leftovers `TODO.md:619` and `:620`. I proposed them as riders earlier; on reading the code this
  cut does not touch `flush_batch`, so they stay in TODO.md and are not part of this cut.

Anti-goals (a failure even if the tests pass)
- A1. Any `.flux` directory, lock file or state file left inside a copied folder (`DEST/<name>/.flux`). Control state is
  not source content (spec §18, "The `.flux` directory is control state").
- A2. Any destination mutation before every pre-transfer check has passed.
- A3. A silent overwrite or a silent skip of a source because two names collided.

## 3. Design

### 3.1 Command line
- `flux copy SRC... DEST` takes one or more sources and a DEST; the LAST positional is DEST. Two positionals are the
  single-source command of today, unchanged. Three or more positionals are the several-source command.
- With several sources, a usage error (exit 2, message names the flag and says "with several sources is not supported
  yet") for each of `--resume`, `--restart`, `--dry-run`. (`--break-lock` requires `--restart` and `--resume-verify`
  requires `--resume`, so clap already rejects them without their parent.) The `Copy` doc comment's "Several sources are
  not supported yet" sentence is rewritten.
- Every other flag applies to all roots: `--overwrite|--update|--skip-existing`, `--links`, `--cross-filesystems`,
  `--preserve*`, `--durability`, `--safety`, `--json`.

### 3.2 Mapping (spec §4.1, §18.3)
- DEST is a folder. If it exists and is not a directory (followed, as `resolve.rs` does today) the command is a usage
  error, exit 2 (§4.1). If it is absent it is created by the run (like a tree's DEST today).
- Each source `S` has a name: the final component of `S` after canonicalisation (my call: it makes `.` and `../x` and
  `dir/` usable, which §18.3's "the root's final path component" leaves open). A source with no final component
  (the filesystem root) fails with exit 1 and the existing "the source has no file name" line. The prefix of the root
  is that one component; the source lands at `DEST/<name>`.
- Each source is resolved exactly as `resolve::job` resolves the one source today, in this order: `symlink_metadata`
  on the path AS GIVEN (a symlink source is refused, K7, exit 1; a missing source fails, exit 1 with the platform line),
  then the kind, then canonicalisation for the name. A trailing separator on a link therefore behaves as it does today
  for one source; this cut changes nothing there.
- A source whose name is `.flux` (ASCII case-insensitive, as `reserved_path` compares), or ends in `.flux-lock`
  (ASCII case-insensitive), is refused before transfer with `CONTROL_PLANE_NAMESPACE_CONFLICT`. `reserved_path`
  (`tree.rs:982`) alone does not do this: it reserves only `.flux/operations`, `.flux/standalone` and `.flux/atomic`,
  two components deep, so a root named `.flux` would land ON the control directory, and a root `x.flux-lock` would
  collide with root `x`'s prefix lock `DEST/x.flux-lock`.

### 3.3 Pre-transfer phase (nothing is created in DEST)
Order: (1) the CLI resolves every source in command-line order and stops at the FIRST failure (my call: simple and
deterministic; collecting all failures is a later nicety). (2) The core, for every root, before the lock:
- byte-equal names: `DESTINATION_NAMESPACE_COLLISION` (spec §18.3), a refusal that changed nothing (exit 3, the
  existing mapping of `RunError::Refused { changed: false }`);
- for a folder root, what `prepare_source` does today with `dst_root = DEST/<name>`: the source opens, its identity,
  the lexical floor (destination inside the source). For a file root, what `prepare_file` does today: the source file,
  its identity;
- the section 129 identity pre-flight per root against `DEST/<name>`, so `flux copy d/a.txt d/b.txt d` (targets are the
  sources themselves) is refused;
- an existing `DEST/<name>` of the wrong kind (a file where a folder source lands, a folder where a file source lands)
  is a refusal before transfer (my call; per-entry type clashes INSIDE a folder stay per-entry failures as in a tree).
Any failure refuses the whole command; nothing is copied.

### 3.4 Locks (spec §96, §96.1, §97, §98)
Order, all before any transfer:
1. The destination-root lock, taken exactly as a tree run takes it today (beside DEST, in DEST's parent). It is the
   OPERATION lock of spec §98 step 1. It carries the heartbeat and the section 99 revalidation, as now.
2. One prefix lock per root, `DEST/<name>.flux-lock`, created exclusively in DEST (spec §18.3, §96.1). A creation that
   fails against another root's lock of this same operation means the names alias on the destination filesystem:
   `DESTINATION_NAMESPACE_COLLISION`, exit 3, and every lock taken is released. A lock held by another operation is the
   existing `TARGET_LOCK_BUSY` / uncertain classification.
Ruling R1 (mine, to confirm): spec §18.3 and §98 are silent on whether a several-source operation also takes the
destination-root lock. I take it (step 1) because the workspace lives under `DEST/.flux/` and one root lock serialises
every operation on DEST, as for a single tree; the prefix locks add alias detection and protect each `DEST/<name>`
against a single-source tree run onto DEST (spec §97.1(b)). The alternative, prefix locks only, would leave
`DEST/.flux/` unprotected. Recorded as a ruling in this spec and in the plan's ledger.
Order of operations (answers builder question 1): (1) the operation lock, beside DEST; (2) DEST created where absent,
as a tree's step 5 does today (E1; `created_dest`); the prefix locks live INSIDE DEST, so DEST must exist first;
(3) the prefix locks; (4) the workspace `DEST/.flux/operations/<id>/` and the claim store. An alias or busy prefix lock
at step 3 therefore finds NO workspace: every prefix lock taken is released (the last close removes the file), the
operation lock is released, and a DEST this run created is removed again. That is `Refused { changed: false }`, exit 3,
by the existing convention ("`changed` is false when nothing changed, or what the run created was removed again",
`run/mod.rs:153-157`); a DEST that cannot be removed is `not_removed`, exit 1, as today. The section 7 test for
refusals reads "DEST not present afterwards", which covers both a DEST never made and one made and removed.
Prefix locks are released when the operation ends, on every path, before the workspace is retired.

### 3.5 Operation state
- One workspace `DEST/.flux/operations/<id>/` (never under `DEST/<name>/`), kind tree, `roots` with n entries in root
  order (§3.6). Each root records what today's single-root records store for its kind, except the prefix: a folder
  root `{source_root: native_hex of the absolute folder, source_identity: the folder's, destination_prefix: native_hex
  of its name}` (today's tree record, `run/place.rs:567-572`, with the prefix filled in instead of `""`); a file root
  `{source_root: native_hex of the absolute FILE path, source_identity: the file's, destination_prefix: native_hex of
  its name}` (exactly today's single-file record, `run/place.rs:808-814`). A several-source operation has no empty
  prefix. The identity pre-flight, the one-source `--resume` mismatch refusal and `cleanup` all read these fields;
  keeping today's conventions means none of them learns a new one.
- One claim store `state.db`, as a tree's. Claims are keyed by destination directory identity, so the roots share it.
- A NEW state format version, written ONLY by a several-source run (my call, R3). The current record validator rejects
  any record whose `roots` is not exactly one (`crates/flux-core/src/state.rs:572-574`, "roots has {} entries, not 1"),
  and `FORMAT_VERSION` is 4 (`state.rs:17`). So a several-source manifest cannot be written under format 4 without
  older binaries reading it as a damaged record. Version 5 is written only when `roots` has two or more entries
  (for it, the validator requires >= 2 roots and a non-empty prefix on each); every single-source run keeps writing
  format 4, byte-identical to today, so no existing state changes. An older binary meets a version-5 record as an
  unknown `format_version` (`INCOMPATIBLE_STATE`, `state.rs:338`), the same refusal cut 9f-1's bump relied on. The plan
  must work out the other version-4 coupling in the validator (`state.rs:~563-568`: the `wal` field is tied to
  `format_version == FORMAT_VERSION`) and the decode path, and cite the lines.
- On normal completion the workspace and the prefix locks are removed exactly as a tree run's (`finish`).
- On an abort or a crash the workspace stays, as for a tree. In this cut nothing resumes it: `flux cleanup` classifies
  and removes it (to verify, §5), and a one-source `flux copy --restart` supersedes it. See the limitation in §6.

### 3.6 Execution
- Roots are processed sequentially in the bytewise order of their prefix `FluxPathKey` (spec §103), not command-line
  order. With single-component prefixes this is global key order without buffering (§18.3); it is the order a later
  hardlink cut needs for canonical selection. The key is the name's exact platform byte form (§103: bytes on Unix,
  WTF-8 of the WTF-16 units on Windows), so for non-ASCII names the order, and therefore the manifest's root order, may
  differ between platforms; §103 permits that and this cut does not fold it. Tests of the order use ASCII names and
  are platform-independent (my call).
- Nested roots (`flux copy a a/sub DEST`) are allowed (my call, R5): the prefixes `a` and `sub` are distinct, which is
  all §18.3 requires, and `cp` and `rsync` both allow it; the content under `a/sub` is copied twice, to `DEST/a/sub` and
  to `DEST/sub`. Refusing it would need a source-overlap rule the spec does not state.
- Folder root: `DEST/<name>` is created (guarded) or opened if it exists (a copy merges onto an existing folder, as a
  tree onto an existing DEST), then the existing tree walk copies into it with its own source root and identity, the
  run's guard, heartbeat, claim store, batch policy and checkpoint log.
- File root: copied through the same per-file path a tree uses for a file, with DEST as the parent directory and the
  file's parent folder as the walk-relative source root (the manifest still records the file itself, §3.5). An existing
  `DEST/<name>` file follows the operation's `--overwrite|--update|--skip-existing` through the claim path, as a file
  inside a tree does since cut 8b (ROADMAP phase 9: "atomic replacement under claim records, with `--overwrite`,
  `--update` and `--skip-existing`"); the plan verifies (V3) that this is observably the same as a two-positional
  single-file copy for all three policies, so G4's "a file is a file" holds whichever command copied it. My proposal;
  the plan must cite the lines that show the per-file path fits or choose the alternative named in V3.
- All roots obey the one guard: a lost lock or a failed heartbeat aborts the operation.

### 3.7 Failure semantics (during transfer)
- A per-entry failure is reported and the walk continues, as today.
- An abort-class error (lost lock, failed heartbeat, a destination root that cannot be used) ends the operation: roots
  after the aborted one are NOT started, stderr names them ("not started: a, b"), the workspace is kept, exit 1.
- Roots finished before the abort stay on disk. The operation is not atomic.

### 3.8 Report and exit codes
- One aggregated `Report` (all 18 JSON keys, same names): counters are sums over roots; `directories_created` counts
  `DEST` if the run made it and every `DEST/<name>` it made; a file root counts as one file.
- stderr: the existing per-entry lines, then one summary line in today's format over the totals.
- Exit codes are today's: 0 success; 1 any failure or abort; 2 usage; 3 refusal before any change.

## 4. Approaches considered

| | Approach | Trade-off |
|---|---|---|
| A | CLI loop: one existing run per source at `DEST/<name>` | Smallest. Lock falls out (a tree run locks beside its destination, so `DEST/<name>.flux-lock`). But each run's state goes to `DEST/<name>/.flux` (`place.rs:244`: "its state lives under `DEST/.flux/operations/`" of the run's DEST), inside the copied folder; collisions are found after the first source has copied; N reports to merge; later resume has N workspaces. Violates §18.3 and anti-goal A1. |
| B | One operation across all roots (chosen) | Matches §18.3 and the manifest. Needs a new run entry, multi-root locking and a per-root driver. |
| C | A now, B later | A plus a rewrite; ships a layout that must be abandoned. |
| D | B for folder sources only | agy's variant. Smaller, but `flux copy a.txt b.txt dir/` is probably the commonest multi-source use. Owner chose B with files (2026-10-22). |

Consult: AGY-FIRST r1 (`.clavity/seams/several-sources-fork-1.md`) ALIGNED on B first. agy was wrong that the prefix lock
does not fall out for free in A; it was right that A puts state in the payload. Its claims that a mid-walk abort stops
later roots and that `flux cleanup` cannot see sub-folder workspaces are UNVERIFIED and not relied on (the first is my
ruling in §3.7, the second is moot under B).

## 5. What the plan must verify before it cites anything (nothing below is asserted as fact)
- V1. Every reader of a state record handles version 5 with `roots.len() >= 2` and a non-empty folder prefix:
  `state.rs` (decode, validate, the version constants), `prior.rs`, `cleanup/` (`discover.rs`, `status.rs`,
  `delete.rs`), `run/place.rs` (`roots()` at its two sites), `run/session.rs`, `recover.rs`, `run/dry_prior.rs`, and
  `run/resume.rs` (the `roots.len()` mismatch at `resume.rs:70` is the one-source `--resume` refusal to keep). The
  grep run while writing this spec found `.roots` read only in `resume.rs`, `session.rs`, `dry_prior.rs`, `mod.rs` and
  `state.rs`; the plan repeats it.
- V7. The name rules of §3.2 against the real engine: `x` with `x.flux-lock`, `.flux`, `.FLUX`, and a name that is
  `reserved_path`-adjacent; the `file_names_fit` limit (`NAME_LIMIT`) for every name Flux keeps beside a target.
- V2. The exact seam for a multi-root run in `run/mod.rs` (`tree` at ~265 builds one `TreePlace` and one `Shared`;
  `open_operation` takes one `LockSite`). Where the prefix locks live (a `Locked` holds one `Held`).
- V3. File roots: can `copy_one` (`tree.rs:~1263`) run with `src_root` = the file's parent and a DEST-level frame? If
  not, the alternative is a small claim-aware single-file function beside it; observable behaviour must be the same.
- V4. Prefix-lock liveness: whether a prefix lock needs the heartbeat, or the OS-native lock proves liveness (spec
  §96.1, §240, §252). Intent: heartbeat on the operation lock only.
- V5. The fake (`fault_fs.rs`) can create, alias and release the prefix locks, and has a case-insensitive mode for the
  alias test; otherwise a real-fs test gated by a case-sensitivity probe, run on the macOS and Windows CI jobs.
- V6. `Shared` can be rebuilt per root without copying state that must be global (the batch, the checkpoint log).

## 6. Known limitations this cut leaves (to state in `--help`, TODO.md and accepted-boundaries)
- L1. After an aborted or crashed several-source run, re-running the same command is refused by the prior-operation
  scan (spec §21.1). With several sources `--restart` is refused in this cut, so the way forward is
  `flux cleanup --force DEST`, or `flux copy --restart` with one source. The resume cut removes this.
- L2. The pre-transfer phase stops at the first bad source, not all of them.
- L3. A several-source run is not atomic across roots.

## 7. Tests (obligations; each new assertion proven red under a mutant)
- CLI: 3+ positionals parse; `--resume`, `--restart`, `--dry-run` with several sources are exit 2 naming the flag; DEST is
  an existing file: exit 2; two positionals unchanged.
- Mapping: two folders; two files; mixed; a source given as `.`, `../x`, `dir/`; output independent of command-line
  order (same tree, same manifest root order).
- Refusals before any change, with the destination listed afterwards as untouched (and DEST not created): missing
  source; symlink source; byte-equal names (`a/x`, `b/x`) and the same source twice; reserved name; `.flux` source;
  the target is the source (`d/a.txt d/b.txt d`); DEST inside a folder source; wrong-kind existing `DEST/<name>`.
- Aliases: `Data` and `data` refused with `DESTINATION_NAMESPACE_COLLISION` and all locks released (fake with a
  case-insensitive mode; real-fs on macOS and Windows jobs); on Linux both copy.
- Layout (the A1 regression): after a run, no `.flux`, `*.flux-lock` or `*.flux-state.*` inside any `DEST/<name>`; the
  workspace is `DEST/.flux/operations/<id>` with n roots, the prefixes in key order; completed run leaves none.
- Locks: DEST lock taken before the prefix locks; a held prefix lock refuses with `TARGET_LOCK_BUSY` and nothing is
  copied; a lock lost between root 1 and root 2 aborts and root 2 is "not started".
- Failure: a per-entry failure in root 1 does not stop root 2; an abort keeps the workspace and names the unstarted
  roots.
- State format: a several-source run writes version 5 with n roots; a single-source run still writes version 4
  byte-for-byte (golden comparison against today's output); the validator accepts exactly the forms it should and
  rejects version 5 with one root, version 4 with several, and an empty prefix on a version-5 root.
- Later-cut compatibility: a one-source `--resume` against a several-source prior refuses (source mismatch);
  `flux cleanup DEST` classifies and removes an aborted several-source workspace; one-source `--restart` supersedes it.
- Report: JSON keys unchanged, counters are sums; exit codes.
- Windows, macOS and Linux CI green; the existing single-source suite unchanged.

## 8. Documentation to change with the cut
`--help` (the `Copy` doc comment), `TODO.md:231` closed, `TODO.md:1119` (`roots`) narrowed to the resume cut,
`ROADMAP.md` phase 2/3 wording, `docs/accepted-boundaries.md` (L1 to L3 and ruling R1), the cut 11a spec N5 note.

## 9. Rulings and calls (mine unless marked)
- R1: destination-root lock plus prefix locks (§3.4). Owner to confirm.
- R2: name from the canonical source, first-failure reporting, wrong-kind `DEST/<name>` refused before transfer.
- R3: state format version 5, written only by several-source runs; format 4 and every single-source run unchanged.
  Owner to confirm: it is a state-format decision.
- R4: riders `TODO.md:619` and `:620` are not in this cut.
- R5: nested roots are allowed, not refused (§3.6). Owner may veto.
- Builder check (2026-10-22, a fresh reader of this document) raised seven questions: the lock/DEST/workspace order and
  what an alias refusal leaves behind (§3.4), R1 and R3 (owner), the file root's manifest fields (§3.5), the
  existing-file policy for a file root (§3.6), the key order across platforms (§3.6), the symlink check before
  canonicalisation (§3.2), nested roots (R5). All folded above.
- Owner: fresh copies only; files included; B.
