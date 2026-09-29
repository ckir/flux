# Cut 7a: the destination lock and minimal operation state

**Status:** designed with the owner 2026-09-30 (brainstorming; an AGY-FIRST consult on every fork, and two
AGY-NEGOTIATE rounds, on F1 and F6). **Base:** `spec/seq-after-cut5`, after cut 6 (PR #56). **Governing documents:**
`FLUX_FULL_UPDATED_SPEC_V16.md` (cited below as `spec:LINE`) and the sequencing decision
`docs/superpowers/specs/2026-09-26-sequencing-after-cut-5-design.md` (cut 7's row). The Rust lock implements the
protocol the TLA+ model in `models/lockproto/` checks.

## Goal

Every `flux copy` - of a tree or of a single file - holds the destination's target lock for its whole run. It gets
the lock under the checked protocol:
- acquisition, §96.1;
- classifying an existing lock, §240.1-240.2;
- dead-owner recovery, §240.3;
- uncertain ownership, §240.4;
- `--break-lock`, §240.5;
- revalidation before each commit, §99.

It records a minimal versioned operation state, so that a later run can classify it (§21.1) and `--restart` can
supersede it. After cut 7a, a crashed run can no longer leave a destination that the next run silently mixes into,
and two runs can no longer write one destination at once.

## Scope: cut 7 splits into 7a and 7b (owner, agy aligned)

**7a (this spec):**
- The full lock protocol for both operation kinds, including:
  - the lock record format;
  - the OS-native lock;
  - lock capability detection;
  - acquire, classify, §240.3 recovery and §240.5 `--break-lock`;
  - §99 revalidation;
  - release.
- A minimal versioned operation state: the tree workspace and its manifest, or a single file's adjacent state record.
- §21.1 classification of prior state.
- `--restart` and `--break-lock`.
- The completion path of a successful run.

**7b:**
- The rest of §19: `roots`, `configuration_fingerprint`, `last_checkpoint`, checkpoints.
- §218's `cleanup_pending` record.
- The heartbeat (§101).

**Out of cut 7 (owner):**
- §97.1 nested destination roots. The model's `nested` scenario is still planned (`models/lockproto/trace.toml:37`).
  Deferral is safe while tree copies publish without replacing: two nested copies cannot overwrite each other's files,
  and the second writer of a file fails. **The model's nested scenario plus §97.1 (a) and (b) are a prerequisite of
  cut 8's replacement.**
- The §96.1 directory-lock acquirer, used when `<name>.flux-lock` would exceed the name-length limit (spec:4717-4736;
  the model's `dirlock` scenario is planned). 7a refuses such a destination. The per-name acquirer's check that
  `P/.flux-dir.lock` is absent IS in 7a, because the model checks it (`S96_1_dircheck`).
- Remote lock capability (F1 below), `--resume` (cut 9), GC of abandoned state (cut 9), the §250 standalone catalog
  (with `flux cleanup`), and several sources (cut 10).

## Decisions (each an owner ruling after an AGY-FIRST consult; briefs in `.clavity/seams/cut7*`)

| # | Question | Ruling |
|---|---|---|
| Split | how to cut cut 7 | as above; agy rejected its own earlier seam (lock without a manifest), because `--restart` must durably mark the prior operation ABANDONED (spec:1646-1660) |
| F1 | lock capability | built-in LOCAL allowlist only; every other filesystem is `Unsupported` and refused `REMOTE_LOCK_UNSAFE`; no flag overrides it. The model's remote runs show that a lease lapse lets a takeover land between an owner's passing check and its write (open `SingleWriter` finding, `FIX_REMOTE_LEASE_SPEC`, design `docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md:978-985`); neither a declaration nor a probe closes that window. Revisited after the §240.5 amendment |
| F2 | single-file state | the adjacent record `target.flux-state.<id>` (§218, spec:9296-9309: "the only on-disk name for the single-file state record"; the `.flux` control plane "MUST NOT be required"); no §250 catalog in 7a |
| F3 | locks per operation | ONE: the target lock is also the operation lock, for a directory copy too, as §98 rules for a single file (spec:4840-4843); the workspace `lock` file (spec:1389) is not created. With the lock created first, a separate workspace operation lock would invert §98's order (spec:4832-4836), and the model has one lock |
| F4 | manifest encoding | JSON, `format_version` checked before any other field; written crash-safely (temporary file, flush, rename over, flush the directory); unreadable or malformed = `STATE_CORRUPT`, unknown `format_version` = `INCOMPATIBLE_STATE`; never parsed heuristically (§193, spec:8385-8401) |
| F5 | creation order | (B): create the lock file and take the OS-native lock (as §96.1 does), THEN create the workspace or state record, THEN write the lock record naming it. **General rule (panel round 2): a lock record is never written naming state that does not already exist durably** - at acquisition, and at a §240.5 takeover's in-place overwrite. A crash before the record leaves an EMPTY lock - uncertain, "cleared by `--break-lock`" (spec:4709-4711) - never a record naming missing state, which `--break-lock` may not override (spec:10655-10659) |
| F6 | a successful run cannot remove its workspace | exit 0 and report the leftover path as a warning; the COMPLETED manifest is the durable record of the leftover, and §21.1 lets later runs proceed past it. "A failed cleanup MUST NOT invalidate an otherwise successful transfer" (spec:9363) |
| `.flux` | a source root containing `.flux` | copy it as ordinary data (§259.3, spec:12891-12915). Reserve `DEST/.flux/operations/`, `DEST/.flux/standalone/` and `DEST/.flux/atomic/`: a source entry whose destination is one of them, or below one, fails `CONTROL_PLANE_NAMESPACE_CONFLICT` (path-scoped, exit 1) |
| ABANDONED | the spec contradicts itself (spec:2990-2991 "remain resumable" vs §20's table, spec:1526, "no (terminal)") | 7a follows §20 and §21.1: ABANDONED is terminal, and a new run proceeds past it |

## Components

### flux-platform (OS primitives)

- **OS-native lock** on an open file, non-blocking and exclusive:
  - Unix: `flock(LOCK_EX | LOCK_NB)`;
  - Windows: `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY)` over the full byte range;
  - never `fcntl` record locks, which a process loses when it closes any descriptor of the file
    (`crates/flux-platform/tests/fs_semantics.rs:10-11`).
  - The lock is released when the handle closes, and when the process dies.
- **Open an existing file for writing without creating it** (§240.5 step 2), **read a whole file through a handle**,
  and **the file identity of an open handle**.
- **Lock capability** of the filesystem that holds a directory handle, by allowlist:
  - Linux, the `statfs` `f_type` magic: ext2/3/4 `0xEF53`, XFS `0x58465342`, Btrfs `0x9123683E`, tmpfs `0x01021994`,
    F2FS `0xF2F52010`;
  - macOS, `f_fstypename`: `apfs`, `hfs`;
  - Windows: the volume's filesystem name `NTFS` or `ReFS`, on a `DRIVE_FIXED` or `DRIVE_REMOVABLE` volume.

  Anything else - SMB, NFS, 9p (WSL's `/mnt/*`), FUSE, FAT - is `Unsupported`. A detection failure is also
  `Unsupported`: prefer refusing (§235.4, spec:10269-10274).
- **`boot_session_id`**:
  - Linux: `/proc/sys/kernel/random/boot_id`;
  - macOS: `kern.boottime`;
  - Windows: the boot time, derived from the current time minus `GetTickCount64`, rounded to the second.

  If it is unavailable, the string `unknown`. A record whose `boot_session_id` is `unknown` never helps prove a
  reboot (§229).

### flux-fs (traits and the fake filesystem)

- `DirHandle` and `FileHandle` gain the primitives above.
- `FaultFs` models them, including OS-native locks held by other simulated owners, and per-call fault injection. This
  lets every protocol branch run as a deterministic unit test.

### flux-core (new modules)

- **`lock`**: the record type and its codec; `acquire`, `classify`, `recover` (§240.3), `take_over` (§240.5),
  `still_owned` (§99) and `release`. Each maps to model labels; see "Model conformance".
- **`state`**: the manifest and the single-file state record (one JSON schema, below); the operation states; the
  crash-safe write.
- **`prior`**: §21.1 classification of a destination's prior state, and the `--restart` sequence.
- **Engine hook**: `copy_file` and `copy_tree` call `still_owned` immediately before each mutation - creating a
  temporary, creating a directory, and each publishing rename (§99, spec:4858-4906). A failed check stops the operation with `TARGET_LOCK_BUSY`, and its state stays resumable.

### flux-cli

- `--restart` and `--break-lock`.
- The new error codes and their exit statuses.
- A collision-resistant operation ID, generated once per invocation, replacing today's process id
  (`crates/flux-cli/src/main.rs:93`).

## Formats

### Operation ID and owner instance ID

- `operation_id`: a random version-4 UUID, written as 32 lowercase hex digits without dashes. It names the workspace
  `DEST/.flux/operations/<operation_id>/`, the state record `target.flux-state.<operation_id>` and temporaries
  `<name>.flux-partial.<operation_id>`. Generated once per invocation, and persisted in the lock record and the state.
  (§18.1 requires "collision-resistant", spec:1366-1367; §57 recommends a UUID.)
- `owner_instance_id`: a separate random version-4 UUID per process, in the same form.

### Lock record, `format_version` 1 (§259.6, spec:12961-12993)

A fixed **4096 bytes**, written with one write call through the handle that holds the OS-native lock:

| Bytes | Content |
|---|---|
| 0-7 | magic `FLUXLOCK` (ASCII) |
| 8-11 | `format_version`, u32 little-endian = 1 |
| 12- | the eight remaining fields in the order of spec:12975-12982 (`complete_lock_key` to `last_heartbeat_wall_time`; `format_version` is bytes 8-11), each a u16 little-endian byte length followed by UTF-8 |
| ... | zero padding up to byte 4079 |
| 4080-4095 | the first 16 bytes of the BLAKE3 hash of bytes 0-4079 |

Field values:
- `complete_lock_key` and `target_path_key`: in `FluxPathKey` encoding.
- `workspace_path`, one of three literals:
  - `operations/<operation_id>` (meaning `DEST/.flux/operations/<id>/`);
  - `adjacent/<operation_id>` (meaning `target.flux-state.<id>`);
  - `none`.
- `creation_wall_time` and `last_heartbeat_wall_time`: nanoseconds since the Unix epoch, UTC, as decimal ASCII. In 7a
  `last_heartbeat_wall_time` equals `creation_wall_time`; 7b's heartbeat updates it.

Every field is bounded:
- the lock key is one name plus a directory identity (spec:12967);
- the workspace path is one of three literals.

So 4096 bytes always fits. An encoder that would exceed it is a bug (it panics in tests), never a truncation.

Decoding, in this order:
- An EMPTY file: uncertain (spec:4709-4711).
- A file of at most 4096 bytes whose first 8 bytes are each EITHER zero OR the corresponding byte of `FLUXLOCK` (all
  zero, a partly written magic such as `FLUX` followed by zeros, or the whole magic): a torn record, uncertain - unless
  it is a whole valid record (below). The first write into an empty lock file can leave any prefix of it unwritten
  after a host crash; that is a torn record, never a foreign object (spec:12986-12987).
- A file that starts with the magic `FLUXLOCK` but is not exactly 4096 bytes, or whose checksum fails: uncertain (a
  torn record; the model reaches a torn read, `algorithm.txt:144`). A torn IN-PLACE overwrite (§240.5 step 6) always
  keeps the magic, because the old and the new record both start with it.
- Any other content - a first-8-byte pattern that is not zero-or-magic byte by byte, or more than 4096 bytes without
  the magic:
  `CONTROL_PLANE_NAMESPACE_CONFLICT`, a foreign object, never overwritten (spec:4662-4677). The residue of the
  zero-or-magic rule: a foreign file of at most 4096 bytes whose first 8 bytes happen to fit it, at a lock path, is read
  as uncertain, so a `--break-lock` could overwrite it; accepted as exotic (the name `<target>.flux-lock` is Flux's).
- An unknown `format_version` with a valid checksum: uncertain (a newer binary owns it). 7a never guesses at its
  layout.

### Operation state, `format_version` 1 (the manifest and the single-file state record)

One JSON object; the reader checks `format_version` before interpreting any other key:

```json
{"format_version": 1, "operation_id": "<32 hex>", "kind": "tree" | "file",
 "destination_root": "<path the operator gave>", "state": "CREATED" | "TRANSFERRING" | "FAILED" | "COMPLETED" | "ABANDONED",
 "superseded_by": null | "<32 hex>", "created_at": "<ns since epoch>",
 "takeover": null | {"operation_id": "...", "owner_instance_id": "...", "boot_session_id": "...",
                     "last_heartbeat_wall_time": "..." | "unreadable", "creation_wall_time": "<ns since epoch>"}}
```

`takeover` is the §240.5 takeover record (spec:10705-10709): the prior holder's `operation_id`, `owner_instance_id`,
`boot_session_id` and `last_heartbeat_wall_time` as read (each "unreadable" when the record was not readable), and the
takeover's own `creation_wall_time`; `null` when this run took no lock over.

- A tree copy keeps it at `DEST/.flux/operations/<id>/manifest`. A single-file copy keeps it at
  `target.flux-state.<id>`, beside the target.
- Unknown keys are refused as `STATE_CORRUPT`: a newer writer bumps `format_version` instead of adding keys.
- Written crash-safely: `<name>.tmp.<id>` in the same directory, `sync`, rename over the final name, `sync` the
  directory.
- 7b bumps `format_version` to 2 and owns reading version 1 (the sequencing doc: every cut that adds durable state
  bumps the version and reads every earlier one).

## The run (a directory copy; a single-file copy differs where noted)

The lock is:
- `P/<DEST-name>.flux-lock` for a directory copy (P is DEST's parent);
- `T/.flux-root.lock` for a filesystem root (spec:4738-4748);
- `target.flux-lock` for a single file.

1. **Capability.** Classify the destination filesystem. `Unsupported` → `REMOTE_LOCK_UNSAFE`, exit 3, nothing touched.
2. **`DEST/.flux`.** If it exists and is not a directory, or a reserved subdirectory is not a directory:
   `CONTROL_PLANE_NAMESPACE_CONFLICT`, exit 3. (A single file skips this step.)
3. **Acquire** (§96.1, spec:4647-4715; model `S96_1_*`):
   - Create the lock file exclusively.
   - Check that `P/.flux-dir.lock` is absent (`S96_1_dircheck`). If it is present, remove what this run created and
     refuse `TARGET_LOCK_BUSY`.
   - Take the OS-native lock without waiting. If it is not granted, retry only while the path still names the same
     file and that file is still empty; otherwise close without removing and start again (`S96_1_ownlock_wait`).
   - Holding it, re-verify the identity and emptiness (`S96_1_ownlock_verify`).

   If the exclusive creation fails, **classify** the existing lock (below). A lock name that exceeds the name-length
   limit is refused `PATH_COMPONENT_INVALID`, exit 3; the directory-lock fallback is out of 7a.
4. **Prior state (§21.1, spec:1595-1671).**
   - A directory copy reads every entry of `DEST/.flux/operations/`; a single-file copy reads every
     `target.flux-state.*` beside the target. The entry named with THIS run's own `operation_id` is skipped: after a
     `--break-lock` takeover this run's own CREATED state already exists (panel round 3), and it is never a prior
     operation.
   - Each is classified (next section).
   - A refusal removes what this run created (the lock file, closed first) and exits 3. If that removal fails, it
     reports the path and exits 1 (spec:2896-2902).
5. **State (F5).** Create `DEST/.flux/`, `operations/` and `operations/<id>/` as needed, then write the manifest (or,
   for a single file, the adjacent record) with state `CREATED`. **Then** write the lock record naming it
   (`S96_1_record_begin/_end`), and set the state to `TRANSFERRING`.
6. **Copy.** The existing engine runs. Before every publishing rename it calls `still_owned` (§99; `S99_check`). A
   source entry that would land in a reserved `.flux` subdirectory fails `CONTROL_PLANE_NAMESPACE_CONFLICT`, and the
   rest continues. `still_owned` also runs before each temporary is created and before each directory is created, since
   §99 covers every write, rename and unlink (spec:4858-4870), not only the publishing rename.
7. **Finish.**
   - Success:
     1. state `COMPLETED` (durable);
     2. remove the run's own temporaries;
     3. remove the workspace directory (or the adjacent record);
     4. `rmdir` `operations/` and `.flux/` if they are empty (a failure because they are not empty is not an error);
     5. `still_owned` (`S99_release_check`);
     6. unlink the lock by name, then close the handle, which releases the OS-native lock (`S99_release`).
   - A removal failure in steps 2-4 (temporaries, the workspace or record, the empty directories) is reported as a
     warning, and the exit stays 0 (F6).
   - Failure: state `FAILED`, release the lock the same way, exit 1. `FAILED` is resumable, so the next run gets
     `RESUMABLE_OPERATION_EXISTS` until `--restart`.
   - Ownership lost (a failed `still_owned`) BEFORE the state is COMPLETED: stop, leave the state as it is, close the
     handle without unlinking (the path may now name another operation's lock, `S99_refuse_close`), exit 1 with
     `TARGET_LOCK_BUSY`.
   - Ownership lost at step 5 of the success path, AFTER COMPLETED: close without unlinking (`S99_refuse_close`), report
     it as a warning, exit 0 - the transfer is complete and durable, and a failure after it "MUST NOT invalidate" it
     (spec:9363, F6).

## Classifying an existing lock (§240.1-240.4; model `S240_1_*`)

Open the file without creating it, try the OS-native lock without waiting, read the record, close.

| Observed | Outcome |
|---|---|
| the OS-native lock is held by another process | `TARGET_LOCK_BUSY` (it may be transient, spec:10570-10578), exit 3 |
| empty file, a failed checksum, or an unknown record version | `TARGET_LOCK_UNCERTAIN`, exit 3; with `--restart --break-lock`, §240.5 takeover |
| not a Flux record | `CONTROL_PLANE_NAMESPACE_CONFLICT`, exit 3, never overwritten |
| a valid record, the lock granted to us, and a LocalStrong filesystem | the owner is dead (on a local filesystem the OS releases a dead process's lock): §240.3 recovery |
| a valid record naming a trusted workspace that is missing, or an untrusted `workspace_path` | `ARTIFACT_OWNERSHIP_UNCERTAIN`, exit 3, preserved (§120, spec:5755-5760; §249.4) |

Refusals report the holder's `owner_instance_id`, `boot_session_id`, `last_heartbeat_wall_time` and `workspace_path`
where the record is readable (§96.2, spec:4755-4761).

**§240.3 recovery** (spec:10588-10624; model `S240_3_*`):
1. Re-read: the record must still name the same dead owner; take the OS-native lock without waiting, and on failure
   start again.
2. Rename the lock to `<lock-name>.broken.<new-operation-id>`.
3. Verify the moved file by identity AND record. On a mismatch, rename it back without replacing and start again.
4. Create its own lock exclusively and take the OS-native lock. If creation fails, delete the moved file and classify
   the new occupant.
5. Delete the moved file.

A leftover `.broken.*` file is classified by its recorded owner. An uncertain lock is never moved aside.

**§21.1 classification of a prior operation's state:**

| Prior state | Outcome |
|---|---|
| `COMPLETED` | proceed; its leftover stays for cut 9's cleanup |
| `ABANDONED` | proceed; left to GC (cut 9) |
| `CREATED`, `TRANSFERRING`, `FAILED` | resumable: `RESUMABLE_OPERATION_EXISTS`, exit 3, nothing mutated (spec:1633-1636); with `--restart`, the sequence below |
| unreadable or malformed | `STATE_CORRUPT`, exit 3, preserved |
| unknown `format_version` | `INCOMPATIBLE_STATE`, exit 3, preserved |
| a workspace directory with no manifest | `STATE_CORRUPT`, exit 3, preserved (missing state, §249.4) |

## `--restart` and `--break-lock`

**`--restart`** (spec:1646-1668), holding the lock throughout:
1. The prior operation's lock was already acquired or recovered at step 3.
2. Durably set every resumable prior operation to `ABANDONED` with `superseded_by` = this operation.
3. `still_owned` (§99; `S21_1_s3`).
4. Delete its partials, then its workspace or record. 7a's state does not list partials (their records are `state.db`'s,
   cut 8), so they are found by the prior operation's EXACT id: every `<name>.flux-partial.<prior-id>` under DEST (a walk
   that skips only the three reserved subdirectories `DEST/.flux/operations/`, `standalone/` and `atomic/`, because
   user files may legally live elsewhere under `DEST/.flux/`), or `target.flux-partial.<prior-id>` beside a single-file target. Authority comes from the
   valid prior state naming that id, never from the name pattern alone (spec:9369-9371 forbids that only when the
   record is missing or corrupt), and `still_owned` runs before EACH deletion (spec:9365-9367).
5. Continue at step 5 of the run: this operation's state, then the record written through the held handle
   (`S21_1_s5_write_begin/_end`).

A crash in 3-5 leaves the prior operation ABANDONED, and the next run proceeds past it. `--restart` never overrides a
live owner, or missing or corrupt state (spec:1664-1666).

**`--break-lock`** is valid only with `--restart` (otherwise a usage error, exit 2), and only for
`TARGET_LOCK_UNCERTAIN` (spec:10655-10659). §240.5 (spec:10655-10722; model `S240_5_*`), after reporting the holder:
1. Strong file identity and a LocalStrong capability, decided from the capability, never by a trial lock; otherwise
   `TARGET_LOCK_UNCERTAIN`.
2. Open the existing file without creating it.
3. Try the OS-native lock; on failure, `TARGET_LOCK_BUSY`.
4. The identity must match the path.
5. Read the record. A live owner → BUSY; a different holder → start again.
6. Create this operation's state (the workspace and manifest, or the adjacent record) with state `CREATED` and
   `takeover` = `null`, durably, THEN overwrite the record in place in one write naming it, flush, and re-check the
   identity; a mismatch → start again, not BUSY. Only after the flush succeeds, write the `takeover` key into the state
   (a second crash-safe state write), so a takeover whose flush failed is never recorded (spec:10700-10701). (F5's rule: otherwise a crash between the overwrite and the state's creation would leave a record
   naming missing state, `ARTIFACT_OWNERSHIP_UNCERTAIN`, which `--break-lock` may not clear.) Step 5 of "The run" then
   finds this state already created and does not write the record again.

Errors are exit 3 before the write and exit 1 after it. The takeover record goes in this operation's `takeover` key;
a takeover whose flush failed is never recorded (spec:10700-10701).

## Errors and exit statuses

| Code | When | Exit |
|---|---|---|
| `REMOTE_LOCK_UNSAFE` | the destination filesystem is not on the allowlist | 3 |
| `TARGET_LOCK_BUSY` | a lock held by another process; the `.flux-dir.lock` conflict | 3 |
| `TARGET_LOCK_BUSY` | ownership lost mid-run (§99) | 1 |
| `TARGET_LOCK_UNCERTAIN` | an empty, torn, checksum-failed or newer-version lock | 3 |
| `CONTROL_PLANE_NAMESPACE_CONFLICT` | a foreign lock file, or a foreign object at `DEST/.flux` or a reserved subdirectory | 3 |
| `CONTROL_PLANE_NAMESPACE_CONFLICT` | a source entry landing in a reserved subdirectory (path-scoped) | 1 |
| `RESUMABLE_OPERATION_EXISTS` | a CREATED, TRANSFERRING or FAILED prior operation, without `--restart` | 3 |
| `STATE_CORRUPT` | an unreadable or malformed state, or a workspace with no manifest | 3 |
| `INCOMPATIBLE_STATE` | a state with an unknown `format_version` | 3 |
| `ARTIFACT_OWNERSHIP_UNCERTAIN` | a record naming a missing or untrusted workspace | 3 |
| `PATH_COMPONENT_INVALID` | a lock name over the name-length limit (the directory-lock fallback is not in 7a) | 3 |
| usage error | `--break-lock` without `--restart` | 2 |

- Exit 3 always means nothing changed. A lock or workspace this run created and removed again does not count; if
  removing it fails, the exit is 1 (spec:2896-2902).
- `OPERATION_LOCKED` is not used: with one lock it cannot be told from `TARGET_LOCK_BUSY` (the model does not
  distinguish them either, `trace.toml:130`).

### What each crash window leaves

| Crash point | What it leaves | Next run |
|---|---|---|
| before the lock file exists | nothing | proceeds |
| between the lock file and its record (F5's window) | an empty lock, and possibly a CREATED state | `TARGET_LOCK_UNCERTAIN`; recovered with `--restart --break-lock`, which supersedes the CREATED state |
| during a `--break-lock` takeover, after its state is created and before its overwrite | the prior (uncertain) lock unchanged, and the new run's CREATED state | `TARGET_LOCK_UNCERTAIN` again; a new `--restart --break-lock` takes over and supersedes both prior states |
| during the copy | a TRANSFERRING state and a dead owner's lock | recovers the lock (§240.3), then `RESUMABLE_OPERATION_EXISTS`; recovered with `--restart` |
| during `--restart` after ABANDONED | an ABANDONED prior | proceeds |
| after COMPLETED | a completed state or its leftover | proceeds |
| during §240.3 recovery, between the move-aside and the new lock | no lock, and `<lock-name>.broken.<id>` beside the lock path | acquires a fresh lock and proceeds. 7a does NOT collect the `.broken.*` file (the spec classifies it by its recorded owner, spec:10620-10624, which is cut 9's cleanup); a run that sees one beside its lock path reports it as a warning |

## Model conformance

A committed table (`models/lockproto/impl-map.toml`) maps every protocol label of `algorithm.txt` to the Rust item
that implements it, or marks it `not-in-7a` with a reason. The protocol labels are:
- `S96_1_*`, `S240_1_*`, `S240_3_*`, `S240_5_*`, `S99_*`;
- `S21_1_*` and `S251_1_*` (`flux cleanup`, not in 7a).

A test (beside `tests/model_stamp.rs`) fails:
- if a protocol label has no entry;
- if an entry names a Rust item that does not exist (checked by a `grep` of the source for the named function);
- if a `not-in-7a` label is referenced by code.

The plan cites these entries step by step.

## Testing

1. **Protocol unit tests on `FaultFs`.** Every branch of acquire, classify, recover, take-over, `still_owned` and
   release, with locks held by other simulated owners and a fault injected at each call. Each outcome the model
   distinguishes gets its own test.
2. **Model conformance** (above).
3. **Real OS locks across processes.** The test binary re-invokes itself as a child that holds `flock` /
   `LockFileEx`. The parent's classification must be BUSY while the child holds it, and dead after the child is
   killed. Also:
   - the record round trip and its checksum;
   - capability detection returns LocalStrong for the test's temporary directory.

   Run on Windows and Linux; cross-checked for macOS by `just check-mac`.
4. **CLI end to end** (`crates/flux-cli/tests`):
   - a tree copy and a single-file copy leave no `.flux` state behind;
   - each refusal's exit status and message;
   - a reserved-path source `.flux` entry;
   - `--restart` supersedes a killed run's state;
   - `--break-lock` clears an empty lock;
   - a real crash: a child `flux copy` killed mid-copy, after which the next run gets `RESUMABLE_OPERATION_EXISTS`
     and `--restart` recovers.
5. **Non-vacuity.** Every new test is proven red under a logic mutant of the code it guards (the repo's
   assertion-strength rule). The gates are `just check`, `just check-linux` and `just check-mac`, then the capstone
   and the test audit.

## What this closes, and what it leaves (TODO.md)

**Closes:**
- "A leftover temporary from a PREVIOUS run is never removed": `--restart` deletes a superseded operation's partials,
  found through its state.
- "A blocking pre-existing temporary is not reported": the id is persisted, so a blocking temporary names its
  operation.
- "WSL 9p mounts break two lock assumptions": 9p is not on the allowlist, so it is refused `REMOTE_LOCK_UNSAFE`.
- The manifest half of "Persistent state format", for version 1 (7b extends it).

**New residues:**
- §97.1 nested roots (a prerequisite of cut 8).
- The §96.1 directory-lock fallback for long names.
- Remote lock capability (after the §240.5 amendment).
- The spec's ABANDONED contradiction (spec:2990-2991 vs spec:1526), for an owner ruling on the spec text.
- A leftover COMPLETED workspace is reclaimed only by cut 9.

## Declared refinements of the spec

1. **One lock** for a single-destination directory copy (F3). The workspace `lock` file (spec:1389) is not created.
2. **Creation order (F5).** The lock record is written after the operation state exists.
3. **The lock record's byte layout, size (4096) and `workspace_path` literals**, which the spec leaves to
   `format_version`. `adjacent/<id>` is added to §120's trusted forms (spec:5755-5760) for a single-file lock, whose
   state is the adjacent record (F2), not a workspace.
4. **The state record is JSON with eight keys**, a subset of §19's `OperationManifest` (spec:1448-1466). 7b adds the
   rest under `format_version` 2.
5. **ABANDONED is terminal** (§20), not "resumable until GC" (spec:2990-2991).
6. **`PATH_COMPONENT_INVALID`** for a lock name that would need the directory-lock fallback. The spec has no code for
   "fallback not implemented"; this names what is wrong with the path.
7. **`--restart` finds a superseded run's partials by its exact operation id**, because 7a's state lists no
   artifacts; the authority is the valid prior state, and every deletion is revalidated.
8. **A `--break-lock` takeover creates this operation's state before its in-place overwrite** (§240.5 step 6). The
   model's `TakeOver` (`S240_5_s6_write_begin/_end`) writes only the record; the added state I/O touches only paths
   under `DEST/.flux/` or the adjacent `target.flux-state.<id>`, which no model actor reads, so it lengthens the step in
   time without adding states to the lock protocol. It exists so that no record ever names missing state (F5).

## Consult record

Seams in `.clavity/seams/`, replies in `.clavity/scratch/cut7/`:
- `cut7-split.md`: agy chose split (b); its earlier seam is withdrawn.
- `cut7a-forks.md` (F1-F6): agy's first picks F1=i, F2=adjacent, F3=one, F4=text, F5=lock-first, F6=exit-1.
- `cut7a-f1-negotiate.md`: (i) held, on measured model evidence.
- `cut7a-f5.md`: agy moved to order (B) once its "discovery starts at the lock" premise was refuted by spec:1598-1601.
- `cut7a-f6.md`: agy moved to exit 0 on spec:9363.
- `cut7a-dotflux.md`: agy widened the reservation to every control subdirectory.
- `cut7a-nested.md`: agy chose to defer §97.1.

The driver corrected its harm analysis: no-replace publication means nested copies cannot overwrite each other in 7a.
