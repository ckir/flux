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
- G3. Everything detectable before transfer is refused before ANY source content is copied and before any workspace or
  state exists (spec §18.3), including prefix collisions decided by the destination filesystem (`Data` vs `data`).
  Deciding that collision needs lock files in DEST, so DEST and the lock files may exist for a moment; a refusal removes
  them again (§3.4).
- G4. A one-source command behaves exactly as today. No existing test changes meaning.
- G5. The on-disk layout is the one the later resume / restart / dry-run cut will build on. The state format
  version 5 introduced here is that cut's format too, so it adds no second migration.

Non-goals (each is a later cut or stays in TODO.md)
- N1. `--resume`, `--restart`, `--break-lock`, `--resume-verify`, `--dry-run` with several sources: usage error (owner
  ruling 2026-10-22).
- N2. `--exclude` (own cut; the cut 11a spec N5 lists it separately).
- N3. Hardlink grouping across roots (phase 4 is not built). Only the ORDER that grouping needs is honoured (§3.6).
- N4. Parallel workers, queues, a planner (phase 3).
- N5. A per-root breakdown in `--json` beyond the optional `roots` array of §3.8 (the 18 keys stay).
- N6. The two cut 11c leftovers `TODO.md:619` and `:620`. I proposed them as riders earlier; on reading the code this
  cut does not touch `flush_batch`, so they stay in TODO.md and are not part of this cut.

Anti-goals (a failure even if the tests pass)
- A1. Any `.flux` directory, lock file or state file left inside a copied folder (`DEST/<name>/.flux`). Control state is
  not source content (spec §18, "The `.flux` directory is control state").
- A2. Any source content copied, or any workspace or state created, before every pre-transfer check has passed; and any
  DEST, lock file or other residue left behind by a refusal.
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
- A source whose name is `.flux`, `.flux-dir.lock` or `.flux-root.lock` (ASCII case-insensitive, as `reserved_path`
  compares; the last two are the spec §96.1 directory and root lock names, `lock/site.rs:12` and `lock/acquire.rs:53`),
  or ends in `.flux-lock` (ASCII case-insensitive), is refused before transfer with `CONTROL_PLANE_NAMESPACE_CONFLICT`.
  Every name must also pass `check_component` (`flux-fs/src/name.rs`: not empty, not `.` or `..`, no separator, no NUL,
  no stream separator). That function does not reject the names Windows cannot create (the DOS device names `CON`,
  `PRN`, `AUX`, `NUL`, `COM1`-`COM9`, `LPT1`-`LPT9`, and a trailing dot or space; a panel finding, read at
  `name.rs:75-99`), so on Windows a name of that kind is refused in group 1 with `PATH_COMPONENT_INVALID`, on the other
  platforms it is an ordinary name. The plan verifies whether the destination code already refuses them and where. `reserved_path`
  (`tree.rs:982`) alone does not do this: it reserves only `.flux/operations`, `.flux/standalone` and `.flux/atomic`,
  two components deep, so a root named `.flux` would land ON the control directory, and a root `x.flux-lock` would
  collide with root `x`'s prefix lock `DEST/x.flux-lock`.

### 3.3 Pre-transfer phase (no source content is copied, no workspace exists)
Order: (1) the CLI resolves every source in command-line order and stops at the FIRST failure (my call: simple and
deterministic; collecting all failures is a later nicety). (2) The core, in two groups.
Group 1, before any lock, touching only the sources and the names (no destination state is read):
- byte-equal names: `DESTINATION_NAMESPACE_COLLISION` (spec §18.3), a refusal that changed nothing (exit 3, the
  existing mapping of `RunError::Refused { changed: false }`);
- the name rules of §3.2 and the length rule of §3.4;
- for a folder root, what `prepare_source` does today with `dst_root = DEST/<name>`: the source opens, its identity,
  the lexical floor (destination inside the source). For a file root, what `prepare_file` does today: the source file,
  its identity;
Group 2, AFTER the operation lock and the prefix locks (§3.4) and BEFORE the workspace is created, because it reads
state inside DEST that another operation could change before a lock is held:
- the section 129 identity pre-flight per root against `DEST/<name>`, so `flux copy d/a.txt d/b.txt d` (targets are the
  sources themselves) is refused;
- an existing `DEST/<name>` of the wrong kind (a file where a folder source lands, a folder where a file source lands)
  is a refusal before transfer (my call; per-entry type clashes INSIDE a folder stay per-entry failures as in a tree).
Any failure in either group refuses the whole command; nothing is copied, and a group 2 refusal releases the locks and
removes a DEST this run made, as §3.4 describes.

### 3.4 Locks (spec §96, §96.1, §97, §98)
Order, all before any transfer:
1. The destination-root lock, taken exactly as a tree run takes it today (beside DEST, in DEST's parent). It is the
   OPERATION lock of spec §98 step 1. It carries the heartbeat and the section 99 revalidation, as now.
2. One prefix lock per root, `DEST/<name>.flux-lock`, created exclusively in DEST (spec §18.3, §96.1). A creation that
   fails against another root's lock of this same operation means the names alias on the destination filesystem:
   `DESTINATION_NAMESPACE_COLLISION`, exit 3, and every lock taken is released. A lock held by another operation is the
   existing `TARGET_LOCK_BUSY` / uncertain classification.
   How "this same operation" is recognised (panel findings, round 1 MEASURED, round 2 fix): today a lock is created
   EMPTY and its record is written only after the state exists (`lock/obtain.rs`: "a lock this run holds with no record
   yet: create the state, then `Held::write_record`"), and `classify` reads an empty lock whose OS lock another handle
   holds as `Busy(None)` (`lock/classify.rs`, `Decoded::Uncertain(_) if !got`), which `obtain` reports as
   `TARGET_LOCK_BUSY`, exit 1. So with today's order a failed creation cannot tell the run it hit its own lock. The fix
   is the order, not a new comparison: a prefix lock's record is written as soon as the lock is held (its
   `workspace_path` is `none` under R7, so it needs no workspace to exist first), and the next root's creation starts
   only after that. A creation that then fails finds a record, `classify` returns `Busy(Some(record))` (the arm
   `Decoded::Record(record) if !got`), and the run compares `record.operation_id` with its own: equal is exactly spec
   §96.1's "record names this operation, for a different target spelling", which is
   `DESTINATION_NAMESPACE_COLLISION`; different is the normal busy or dead classification. This works because the
   OS-native lock is `flock` on Unix and `LockFileEx` on Windows, never `fcntl` record locks
   (`flux-platform/src/lock_file.rs:3`): a second handle of the same process does NOT acquire it, so the run cannot
   read its own live lock as dead and take it over. A comparison of file identities was considered and dropped: a
   `FileIdentity` is `PartialEq` over `Strong`, `Weak` and `Unavailable` (`flux-fs/src/fs.rs:88-92`), so two
   `Unavailable` identities (FAT32, exFAT, some SMB and NFS) compare equal and a foreign lock would be reported as an
   alias. The write must be COMPLETE before the next root's creation starts: the record is written with the lock's own
   handle and flushed or synced as `Held::write_record` does for an operation lock, with no buffered writer left open,
   so that the next creation's `classify`, which opens the same file with a second handle and reads it, sees the whole
   record. The strict order for each root is: create the lock, hold its OS-native lock, write and complete the record,
   and only then attempt the next root's creation (a panel finding, round 3). A test makes the window observable (V10).
Length rule (a finding of the panel): spec §96.1 says that when `<name>.flux-lock` would exceed the directory's
name-length limit the acquirer takes the directory lock `P/.flux-dir.lock` instead, and that per-name locks and the
directory lock exclude each other (`lock/acquire.rs:53` already refuses a per-name acquirer when `.flux-dir.lock` is
present). One operation holding a directory lock for one root and per-name locks for the others would refuse itself, and
two long-named roots would both want the same file and read each other as an alias. This cut therefore does not use the
fallback: a several-source command with a root whose `<name>.flux-lock` would exceed the limit is refused in group 1 with
`PATH_COMPONENT_INVALID` and a message naming the root. Limitation L4. (`file_names_fit`, `run/mod.rs`, is the existing
single-file analogue; the plan reads its limit.)
Source count (a finding of the panel): every prefix lock is a file in DEST and an open handle with an OS-native lock for
the whole run, and all must be held before any transfer, so n cannot be lazy. The command is refused in group 1 (exit 2)
when the number of sources exceeds a cap, with a message giving the cap. The cap is a number the plan sets from a
measurement (the process's open-file limit less the descriptors the run already needs); this spec fixes the behaviour,
not the number. Limitation L5.
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
Crash residue (a finding of the panel, MEASURED, and a master-spec gap): a crash leaves the prefix lock files behind with
a dead owner. `classify` resolves a recorded `workspace_path` of `operations/<id>` RELATIVE TO THE LOCK'S OWN TARGET
(`lock/site.rs:152-186`, `operation_dir_exists`): for the prefix lock of `<name>` that is `DEST/<name>/.flux/operations/<id>`,
which never exists, so the dead lock is an `Orphan`, `obtain` refuses it, and the refusal's hint ("flux cleanup DEST
removes a dead owner's lock whose workspace is gone", `lock/obtain.rs`) is false for it, because `flux cleanup` examines
the root lock beside DEST and the operations listing, not `DEST/*.flux-lock` (`cleanup/discover.rs:~205-235`; the plan
re-reads it). An operator would loop between the refusal and the hint. The master spec has no rule for this: §120 and
§259.6 trust a recorded `workspace_path` only if it is one of two paths derived from the lock's own target, or `none`
(which they define for a cleanup lock). Ruling R7 (mine, OWNER DECISION): a prefix lock's record carries
`workspace_path` = `none`, extending §259.6's `none` from cleanup locks to prefix locks. The operation lock beside DEST
already names the real workspace, and a prefix lock protects no recovery state of its own. A dead prefix lock is then
`Trusted` (`site.rs`: `path == "none"`), classified `Dead`, and recovered like any dead lock by the next acquirer of that
name. Consequence, limitation L6: a dead prefix lock for a name no later run uses stays in DEST as a stale file until
that name is used again. The alternative, teaching `flux cleanup` to remove the dead prefix locks that a version-5
manifest names, is not taken in this cut; it can be added without a format change.

### 3.5 Operation state
- One workspace `DEST/.flux/operations/<id>/` (never under `DEST/<name>/`), kind tree, `roots` with n entries in root
  order (§3.6). Each root records what today's single-root records store for its kind, except the prefix: a folder
  root `{source_root: native_hex of the absolute folder, source_identity: the folder's, destination_prefix: native_hex
  of its name}` (today's tree record, `run/place.rs:567-572`, with the prefix filled in instead of `""`); a file root
  `{source_root: native_hex of the absolute FILE path, source_identity: the file's, destination_prefix: native_hex of
  its name}` (exactly today's single-file record, `run/place.rs:808-814`). A several-source operation has no empty
  prefix. The identity pre-flight, the one-source `--resume` mismatch refusal and `cleanup` all read these fields;
  keeping today's conventions means none of them learns a new one, with ONE addition (a finding of the panel): a version-5
  `Root` also records `kind` (`"tree"` or `"file"`). `Root` has no kind today (`state.rs:61-71`) because the record's
  own kind said it, and a version-5 record is `Kind::Tree` with roots of both kinds. Without it the later resume cut
  could not tell a file root from a folder root once the source path is gone or replaced (a prefix and an identity look
  the same for both), which would force a version 6. `Root` is `deny_unknown_fields`, so the field is added for
  version 5 only and version 4 records are byte-identical; the validator requires `kind` on every version-5 root and
  rejects it on a version-3 or version-4 root.
- One claim store `state.db`, as a tree's. Claims are keyed by destination directory identity, so the roots share it.
- A NEW state format version, written ONLY by a several-source run (my call, R3). The current record validator rejects
  any record whose `roots` is not exactly one (`crates/flux-core/src/state.rs:572-574`, "roots has {} entries, not 1"),
  and `FORMAT_VERSION` is 4 (`state.rs:17`). So a several-source manifest cannot be written under format 4 without
  older binaries reading it as a damaged record. Version 5 is written only when `roots` has two or more entries
  (for it, the validator requires >= 2 roots and a non-empty prefix on each); every single-source run keeps writing
  format 4, byte-identical to today, so no existing state changes. An older binary meets a version-5 record as an
  unknown `format_version` (`INCOMPATIBLE_STATE`, `state.rs:338`); whether that refusal is what an older `flux cleanup` and an older `flux copy`
actually do is part of V1, not assumed. The plan
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
- Keys (a finding of the panel, MEASURED): the checkpoint log identifies a file by `file_id: key.encode()`
  (`tree.rs:1518`), where the key is the `FluxPathKey` built from the path RELATIVE TO THE WALK ROOT
  (`FluxPathKey::from_relative(&path)`, `tree.rs` in `copy_one`). With one tree that key is the §103 key. With several
  roots, `a/big.bin` and `b/big.bin` would both be `big.bin` and share one checkpoint entry, so one file's chunk digests
  could be read as the other's. Every key a several-source run stores or compares (the checkpoint `file_id` and every
  other persisted key) is therefore the §103 key: the root's prefix components followed by the root-relative components.
  The plan lists every site that calls `FluxPathKey::from_relative` and states for each whether it is persisted, and
  tests the collision directly (V6).
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
- `--json` (a finding of the panel): stderr is not seen by a program reading the report, and after an abort it cannot
  tell which roots finished. A several-source report therefore carries one additional OPTIONAL key `roots`, an array in
  prefix order of `{ "name": <string>, "status": "completed" | "failed" | "not_started", "files_copied": <n>,
  "files_failed": <n> }`, absent from every one-source report (the precedent is `mounts_skipped`, `dry_run` and
  `unprobed`, which are written only when set). So the 18 keys and a one-source report are unchanged. Ruling R8 (mine,
  owner may veto): without it an aborted run is opaque to automation.
- A report for a command refused before transfer carries no `roots` (nothing ran), as `Report::pre_engine` builds it today.
- stderr: the existing per-entry lines, one line per finished root (`root <name>: <files> copied, <failed> failed`, my
  call: an aborted run leaves some roots on disk and the operator must see which), then one summary line in today's
  format over the totals.
- A prior-operation refusal that names a several-source workspace, and the usage error for `--restart` with several
  sources, both say what to run: `flux cleanup --force DEST` (L1). The existing hint text suggests `--resume` or
  `--restart`, which would send the operator into a second refusal.
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
- V7. The name rules of §3.2 (including Windows-reserved names and trailing dots or spaces, which §103 reports as `PATH_COMPONENT_INVALID`) against the real engine: `x` with `x.flux-lock`, `.flux`, `.FLUX`, and a name that is
  `reserved_path`-adjacent; the `file_names_fit` limit (`NAME_LIMIT`) for every name Flux keeps beside a target.
- V2. The exact seam for a multi-root run in `run/mod.rs` (`tree` at ~265 builds one `TreePlace` and one `Shared`;
  `open_operation` takes one `LockSite`). Where the prefix locks live (a `Locked` holds one `Held`).
- V3. File roots: `copy_one` (`tree.rs:~1263`) is a step of a directory traversal; it takes the directory's `NameIndex`,
  a `Batch`, the parent's `claim_parent` and a walk-relative `path`, so using it for a top-level file means building a
  DEST-level frame of those (a finding of the panel: the mechanism is not free). The alternative is a small claim-aware
  single-file function beside it. The plan chooses by reading both and must justify the choice; observable behaviour
  (existing-file policies, claims, atomic publication, report counters) must be the same either way and is tested
  against a two-positional single-file copy for all three policies.
- V4. Prefix-lock liveness: whether a prefix lock needs the heartbeat, or the OS-native lock proves liveness (spec
  §96.1, §240, §252). Intent: heartbeat on the operation lock only. Supporting but NOT verified evidence: the
  classifier's tests build a live owner as a lock file with an OS-native lock held (`live_lock`) and a dead one without
  (`dead_lock`) in `lock/classify.rs`; the plan reads `classify` itself to confirm liveness never depends on heartbeat
  age, because a prefix lock a competing operation reads as abandoned would let two operations write `DEST/<name>`.
- V8. Crash residue: after a crash the prefix lock files stay. What a later several-source run, a one-source tree run onto
  DEST (spec §97.1(b)) and `flux cleanup DEST` each do with them; whether `cleanup` removes them. Test obligation in §7.
- V9. The descriptor cost of one held prefix lock on each platform, to set the cap of §3.4.
- V10. Read-after-write across two handles of one process (the writer's and `classify`'s) on each platform; it is
  expected on Unix (one page cache), and is UNVERIFIED on Windows, where the Windows CI job must show it. The alias
  decision of §3.4 on a case-insensitive destination, including the window between a prefix lock being
  created and its record being written (a root's creation must not start inside it); and that a prefix lock closed
  early would be read as dead (`classify`: an unheld lock with a record is `Dead`, agy confirmed this from the same
  lines), which is why the locks are held for the whole run.
- V11. Whether the source walk skips Flux control files. `walk.rs` has no such skip (a grep found none), so a stale
  `*.flux-lock` left in a DEST that is later used as a SOURCE may be copied as payload. This is already true of a stale
  single-file target lock today; this cut adds more possible stale files (L6). The plan states what is copied and
  decides, with the owner, whether a walk skip is in scope; it is not assumed.
- V5. The fake (`fault_fs.rs`) can create, alias and release the prefix locks, and has a case-insensitive mode for the
  alias test; otherwise a real-fs test gated by a case-sensitivity probe, run on the macOS and Windows CI jobs.
- V6. `Shared` can be rebuilt per root without copying state that must be global (the batch, the checkpoint log).

## 6. Known limitations this cut leaves (to state in `--help`, TODO.md and accepted-boundaries)
- L1. After an aborted or crashed several-source run, re-running the same command is refused by the prior-operation
  scan (spec §21.1). With several sources `--restart` is refused in this cut, so the way forward is
  `flux cleanup --force DEST`, or `flux copy --restart` with one source. The resume cut removes this.
- L2. The pre-transfer phase stops at the first bad source, not all of them.
- L3. A several-source run is not atomic across roots.
- L4. A root whose name is so long that `<name>.flux-lock` exceeds the directory's limit is refused; §96.1's
  directory-lock fallback is not used by several-source runs.
- L5. A cap on the number of sources (set by the plan from a measurement).
- L6. A dead prefix lock for a name no later run uses stays in DEST as a stale file (R7).

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
- Keys: two roots holding a large file at the same root-relative path (use the checkpoint policy seam so a small file
  checkpoints): each file's checkpoint entry is its own, and the copies are byte-correct.
- Length and count: a name whose lock exceeds the limit is refused with `PATH_COMPONENT_INVALID` and nothing is made; one
  source over the cap is refused with exit 2.
- Crash residue: a run killed after the prefix locks leaves them with `workspace_path` `none`; a later several-source run
  using the same names recovers them (no `ARTIFACT_OWNERSHIP_UNCERTAIN`, no loop with the cleanup hint); a later
  one-source tree run onto DEST and `flux cleanup DEST` each behave as V8 settled, with a test per behaviour.
- Alias window: with a case-insensitive destination, the second creation of `Data`/`data` is decided by the record that
  names this operation, and gives `DESTINATION_NAMESPACE_COLLISION` exit 3 (not `TARGET_LOCK_BUSY`, exit 1). Two mutants
  must be red: the first prefix lock's record is written AFTER the next root's creation, and the `operation_id`
  comparison is removed (both end in exit 1). On a destination that reports `Unavailable` file identities (the fake can
  be set to) the same test gives the same answer, so a comparison of identities cannot satisfy it by accident.
- State format, root kind: a version-5 record round-trips `kind` for a mixed file and folder run; a version-5 root
  without `kind`, and a version-3 or -4 root with it, are rejected.
- JSON: a several-source report has `roots` with the right statuses after a mid-run abort; a one-source report has none;
  a refusal before transfer (the report built by `Report::pre_engine`) has no `roots` key and does not fail.
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
- R6: the §96.1 directory-lock fallback is not used; over-long names and over-cap source counts are refused (L4, L5).
- R7: prefix lock records carry `workspace_path` `none` (§3.4). OWNER DECISION: it extends a master-spec rule.
- R8: an optional `roots` array in the several-source `--json` report (§3.8). Owner may veto.
- Builder check (2026-10-22, a fresh reader of this document) raised seven questions: the lock/DEST/workspace order and
  what an alias refusal leaves behind (§3.4), R1 and R3 (owner), the file root's manifest fields (§3.5), the
  existing-file policy for a file root (§3.6), the key order across platforms (§3.6), the symlink check before
  canonicalisation (§3.2), nested roots (R5). All folded above.
- Owner: fresh copies only; files included; B.

## Stand-downs
Panel round 1 (solo, 2026-10-22). Dispositions of every finding raised: F1 wording of G3/A2 against the lock files the
alias check needs: FOLDED (G3, A2, §3.3). F2 the §96.1 `.flux-dir.lock` fallback against per-root prefix locks: FOLDED
(§3.4 length rule, L4, R6). F3 checkpoint `file_id` collision across roots, measured at `tree.rs:1518`: FOLDED (§3.6
Keys, V6 test). F4 destination-side checks before a lock is held: FOLDED (§3.3 groups). F5 unbounded prefix locks and
descriptors: FOLDED (§3.4 source count, L5, V9). F6 operator guidance after an abort and root progress: FOLDED (§3.8).
F7 crash residue of prefix locks: FOLDED as V8 and a test; the behaviour is for the plan to settle. F8 the unsupported
claim about cut 9f-1's bump: FOLDED (§3.5 reworded). F9 Windows-reserved and malformed prefix names: FOLDED into V7.
Discarded below the floor: none this round. Unverified-accepted: none.

Panel round 1, agy escalation (reply verified line by line; agy's verdict NEGOTIATE). A1 prefix-lock crash residue is an
orphan and cleanup does not clear it: FOLDED (§3.4 crash residue, R7, L6; the mechanism is confirmed at `lock/site.rs:152-186`
and `lock/obtain.rs`, the cleanup half is read from `cleanup/discover.rs` and re-read by the plan). A2 the empty lock record
makes the alias check read as `TARGET_LOCK_BUSY`: FOLDED (§3.4, V10; the identity rule first written here was replaced in
round 2 by the record rule; confirmed at `lock/classify.rs`
`Decoded::Uncertain(_) if !got` and `lock/obtain.rs` "no record yet"). A3 a version-5 `Root` has no kind: FOLDED (§3.5,
tests). A4 `--json` is blind to which roots finished: FOLDED as R8. A5 root names `.flux-dir.lock` and `.flux-root.lock`:
FOLDED (§3.2). A6 the file-root seam is deferred wholesale: FOLDED (V3 reworded; the choice stays with the plan, which
must cite both mechanisms). A7 agy's proposal to close the prefix lock handles after validation, or hold only the active
root's: REJECTED: an unheld lock with a record classifies as `Dead` (`lock/classify.rs`, `Decoded::Record(record)` with
`got`), so a competitor could take it over, and the alias check needs every lock held at once before any transfer
(spec §18.3, "Before any transfer ... takes a lock named after each root's prefix"). A8 descriptor exhaustion: already
folded in round 1 (cap, L5). Counter-turn on A7 owed to agy and sent in round 2.

Panel round 2, agy escalation (cited lines read and verified before folding). B1 alias identity compared with `==` is
true for two `Unavailable` identities: FOLDED, with a different fix from the one agy proposed (a hybrid of identity and
record): the identity comparison is dropped and the record, written at once, decides (§3.4); the claim is confirmed at
`flux-fs/src/fs.rs:88-92`. B2 Windows device names and trailing dots are not rejected by `check_component`: FOLDED
(§3.2). B3 command-line routing: no new findings; the seat read `main.rs` only, so that is a thin clean result. B4 stale
prefix locks copied as payload when a DEST becomes a source: FOLDED as V11 (confirmed only in part: `walk.rs` has no skip;
whether another layer excludes them is the plan's to read). B5 the report built before transfer carries no `roots`:
FOLDED (§3.8, test). The counter-turn on closing the prefix handles early is closed: agy read `lock/classify.rs` and
`lock/site.rs` itself and agreed that an unheld lock with a record is classified `Dead`, and that a `none` record is
`Trusted` and recovered by `recover`. Discarded below the floor: none. Unverified-accepted: none.

Panel round 3, agy escalation (narrow: five round-2 edits plus the lock design against the master spec; the cited lines and
both probe quotes were checked against the artifact). C1 the section 7 alias-window test still said "decided by identity"
and "a mutant that decides by record is red", contradicting section 3.4: FOLDED (section 7 now names the record and two
mutants). agy filed it below the floor; it is not, because an implementer would build the wrong test. C2 a record must be
complete and readable by a second handle before the next root's creation starts: FOLDED (section 3.4 strict order, V10;
read-after-write on Windows is UNVERIFIED and is a plan obligation, not an accepted risk). C3 the lock design departs from
the master spec only at R1, R6 and R7, all stated as rulings: no new findings. The other four fold-audit items: no new
findings. Echo: agy quoted the artifact's last line without its backticks, so the driver flagged ECHO MISSING; both probes
and the cited lines are correct, and the flag is recorded here rather than waved off.

