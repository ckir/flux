# Cut 9a: resuming an interrupted copy, file by file

Status: DRAFT for owner review (2026-10-08), branch `spec/cut-9`. Scope negotiated with agy (scoping consult
`.clavity/scratch/cut9-scope/`, forks consult `.clavity/scratch/cut9a-forks/`); every pick below was approved by the owner on
2026-10-08. Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 17-23, 53, 99, 119-121, 140, 180, 241.5, 249).

## Why this cut, and why it is the first slice of "cut 9"

A tree copy that was killed or failed cannot be continued today: `RESUMABLE_OPERATION_EXISTS` is the only answer, and `--restart` throws the
progress away. Cut 9 as the spec describes it (resume, the write-ahead log, commit recovery, chunk checkpoints, cleanup) is too large for one
design and parts of it are blocked on undefined spec text (gaps G2 to G5 of the fact digest: how a claim and a WAL `COMMIT` share "one durable
transaction", which publications get `PREPARE_COMMIT`/`COMMIT`, what a claim without a rename means, the record types). The slicing, agreed by the
owner, agy and the controller:

| Slice | Content | State |
|---|---|---|
| **9a (this spec)** | `--resume` for tree and single-file copies, file-granular, **no WAL**; the prior run's claim records are the progress record | designed here |
| 9b | cleanup lifecycle: finish a prior `cleanup_pending`, stale-lease classification (section 102), `.creating`/`.removing` debris, orphan partials | later |
| 9c | the WAL, `PREPARE_COMMIT`/`COMMIT`, commit recovery | blocked on the spec gaps |
| 9d | chunk checkpoints, `--resume-verify`, snapshots and compaction | after 9c |

The progress model of 9a is stated once: **claims record durable progress; the destination filesystem plus the existing-file policy reconcile
whatever the claims did not record.** That is safe because every published file was completed before its rename, and a file whose claim was lost
(known limit 4a of cut 8b: up to 1000 claims under `Durability::Normal`) is simply seen as an existing destination file.

## Decisions (all owner-approved)

1. **Resume identity.** `--resume` continues the SAME operation: it adopts the prior operation id, workspace and `state.db`; the lock record is rewritten for
   the new owner instance; the manifest goes to `TRANSFERRING`. (Spec 21.1: "`--resume` continues the operation from its saved state"; `--restart` is the
   one that makes a new operation.) Temporary names `<name>.flux-partial.<id>` therefore match the killed run's, and the existing step-1 leftover sweep in
   `copy_file_guarded` removes the killed run's partial for the same target.
2. **A target that finds its own `Created` claim** verifies the destination entry by **size and, when `preserve_times` is not `Off`, modification time**
   against the source (the spec's `metadata` verification level; stronger levels are slice 9d). Match: skip (counted as resumed). Mismatch or absent: the normal path decides (the existing-file policy, exactly as for any existing destination file), so `--update` and `--skip-existing` never clobber a destination file someone changed during the downtime. With `preserve_times = Off` there is no source mtime to compare and a size alone is never enough (spec invariant 14), so the check cannot pass and the normal path decides; the CLI cannot request `Off` today (its flags yield `default` or `strict`).
3. **A published file whose claim was lost** is reconciled by the existing-file policy: overwrite re-copies, update compares, skip-existing leaves it. No
   shortcut that treats a metadata-equal unclaimed file as ours (invariants 14 and 15 of the spec: size or timestamp equality alone is never strong validation).
4. **Configuration fingerprint.** The manifest stores an `options` object (compared field by field, so a mismatch names the option) and a
   `configuration_fingerprint` (BLAKE3 of a canonical byte form of the exact-match options). Directional options are compared by rule, not by hash (gap G8).
5. **Mapping check.** The source root path (as native-hex), the source root's identity and the destination prefix must all match; a mismatch is `INCOMPATIBLE_STATE`
   (exit 3). Identities are compared only when both are `Strong`; otherwise the path alone decides and a warning says so. The stored source root is the absolute path the run resolves (the argument made absolute against the working directory and lexically normalized, links not resolved), so `./src` and `/abs/src` compare equal.
6. **Manifest format 3.** Formats 1 and 2 keep reading unchanged. A resumable prior at format 1 or 2 has no options to compare: `--resume` refuses it with
   `INCOMPATIBLE_STATE` and a hint to use `--restart`.
7. **Reopening `state.db`.** A new `DirHandle::open_claim_store` opens the existing file; `RedbClaimStore` splits into create (writes `meta.format`) and open
   (rejects a zero-length file, calls `create_file` on the opened handle, which repairs, and checks `meta.format`; see Reopening). Corrupt: `STATE_CORRUPT`. A different `format`: `INCOMPATIBLE_STATE`. **A manifest with no `state.db`** is zero progress
   (a fresh `state.db` is created) **only while the manifest says `CREATED`**; in any later state it is `STATE_CORRUPT`.
8. **Progress.** The `RESUMABLE_OPERATION_EXISTS` refusal names the operation, its state and its manifest path; it does **not** count claims. Section 21.1 requires the refusal to change nothing, and reading the count needs a repairing open of `state.db` (a killed store, the case that matters, fails a read-only open with `RepairAborted`). The count is shown once the run has adopted the operation: `resuming operation <id> (<n> entries claimed)` (redb `Table::len`, O(1): the root page stores the count). **Owner decision pending: this amends the approved pick (a count inside the refusal).**
9. **Report.** Files skipped because the prior run completed them count in `files_skipped` and `bytes_skipped`; one human line says how many were resumed. The JSON
   stays at the closed 18 keys of section 53 (an existing test asserts exactly that).
10. **Single files.** `--resume` adopts the prior id and copies the whole file again (the old partial is removed by the step-1 sweep); validating a partial and
    continuing from its end is slice 9d.
11. **Flags.** `--resume` with `--restart` is a usage error (exit 2). `--break-lock` already requires `--restart` (clap `requires`). `--resume` with no prior operation starts
    a new one and prints `no prior operation; starting new`. `--dry-run` does not exist yet and stays out.

## Behaviour

### The run (spec 21.1; code: `run/session.rs` `open_operation`)

Steps 3 and 4 are unchanged: take the destination lock (recovering a dead owner's), then scan `DEST/.flux/operations/` (or the adjacent record for a single file).
With `cfg.resume`:

| Scan result | Result |
|---|---|
| no resumable prior | start new (message above); a terminal or `cleanup_pending` prior is passed over as today |
| exactly one resumable prior (`CREATED`, `TRANSFERRING`, `FAILED`) | validate (next section), then **adopt** |
| more than one resumable prior | refuse, `RESUMABLE_OPERATION_EXISTS`, naming all of them (exit 3); `--restart` supersedes all of them. Ambiguity is never guessed away. |
| a prior at format 1 or 2 | `INCOMPATIBLE_STATE` with the `--restart` hint (exit 3) |
| live owner, uncertain owner, corrupt state | the refusals already in place |

**Adopt** replaces step 5's creation: `Place::adopt(&mut self, prior: &OperationState, warnings)` (new, beside `create`) opens the existing workspace
(a tree: `operations/<id>` and its `state.db` through `open_claim_store`; a single file: the existing `<target>.flux-state.<id>`), the run's **effective
operation id becomes the prior's** (`locked.state.operation_id`; the places in `run/mod.rs` that read `cfg.operation_id` for `CopyOptions` and records read
the effective id instead), `record_for` writes the lock record with the prior's `workspace_path`, and the manifest is written `TRANSFERRING` under section 99
(`owned(...)` first). For a single-file record the adoption bumps `artifact_generation` and takes a new `attempt_id`. A directional change (durability normal to strict) is stored in `options` at that write.

**Order inside adoption** (a crash between any two steps leaves a state the next `--resume` accepts): (1) validate the prior (format, compatibility); (2) open `state.db`, or for a manifest in `CREATED` with none, create it and let its creation commit durably; a failure here leaves the prior workspace untouched, and the lock this run took is released as for any refusal; (3) write the lock record naming the prior id and workspace (`record_for` must take the effective id: it reads `cfg.operation_id` today, `session.rs:285`); (4) write the manifest `TRANSFERRING`. `TRANSFERRING` is therefore never on disk without a `state.db`, which is what lets decision 7 call a missing `state.db` in any later state corrupt.

### Compatibility (spec 121)

| Compared | Rule | On mismatch |
|---|---|---|
| `roots` (source root path as native-hex, source root identity, destination prefix) | equal; identity only when both `Strong` | `INCOMPATIBLE_STATE`, naming the mapping |
| `preserve_times`, `preserve_permissions`, `safety`, `existing` | equal | `INCOMPATIBLE_STATE`, naming the option |
| `durability` | `normal` to `strict` allowed (recorded); `strict` to `normal` refused | `INCOMPATIBLE_STATE` |
| `configuration_fingerprint` | recomputed from the stored `options` and compared with the stored hash first; a difference means the stored options were changed or damaged since they were written (the fingerprint's purpose is to prevent accidental resume with incompatible semantics, section 19; `roots` and `durability` are compared directly, not through it) | `STATE_CORRUPT` |

`retries` and `--resume-verify` do not exist yet; the object grows with the options. The fingerprint covers the exact-match options only: canonical bytes
`flux-config-v1\n` then one `key=value\n` line per option in this fixed order: `preserve_times`, `preserve_permissions`, `safety`, `existing`, with the values `off|default|strict` for both `preserve_times` and `preserve_permissions` (the library type; the CLI's boolean flags reach `default` and `strict`), `default|strict` for `safety`, and `overwrite|update|skip-existing` for `existing`.

### Manifest format 3 (`state.rs`)

`FORMAT_VERSION = 3`. A tree manifest gains `roots: [{source_root, source_identity, destination_prefix}]` (`source_root` and `destination_prefix` native-hex,
`source_identity` as `identity_text`; a tree has one root, `destination_prefix` empty), `options: {preserve_times, preserve_permissions, durability, safety, existing}`
and `configuration_fingerprint` (lowercase hex BLAKE3). A single-file record gains the same `options` and `configuration_fingerprint`; its source and target
identities already exist (cut 7b). Unknown keys stay `STATE_CORRUPT`; the 64 KiB `STATE_LIMIT` is unchanged. A new run writes format 3; nothing rewrites an old one.

### The walk (`tree.rs` `copy_one`)

`Shared` gains `resume: bool`. In the `Resolved::Entry { stored, meta }` arm, **after** the in-memory fold check (`names.owner(stored)` held by another target of this run: the cut 8b final-review fix) and before the policy decision, when `resume` and `claim_parent` is `Some`:

0. The source is stat-ed first; a failure is that target's `Copy` failure at `CopyStep::Source`, as on the normal path.
1. `claims.get((D.identity, stored))`; a store error is that target's failure at `CopyStep::Claim`.
2. `Some(r)` with `r.target == T` and `r.status == Created`: verify the destination (decision 2). Match: `files_resumed += 1`, `bytes_resumed += source length`, no write, no claim, and `names` records `T` as the owner of `stored` (as a publication would) so a later target folding onto the same entry is still refused. Mismatch: continue on the normal path: the directory guard first (a destination that became a directory is that target's `DESTINATION_ERROR`), then the policy decision.
3. `Some(r)` with `r.target == T` and `Existing`: redo (the cut 8b rule: a target that finds its own claim proceeds).
4. `Some(r)` with another target: the existing collision.
5. `None`: the existing path (the policy decides).

`Resolved::Absent` takes the new-file path as today (an own `Created` claim already there is accepted by `record_claim`). Weak-identity directories (`claim_parent == None`) keep degrading to no-replace and are never resumed-skipped. No claim was ever recorded for them, so the ownership of any existing file there is unknown: a resumed run reports each pre-existing file as `DESTINATION_NAMESPACE_COLLISION` (exit 1), exactly as a second run does today; resume cannot help such directories (known limit 7). `TreeOutcome` gains `files_resumed` and `bytes_resumed` (internal);
`Report::tree` adds them to `files_skipped` and `bytes_skipped` and prints `resumed <n> already-complete files` when `n > 0`.

### Reopening `state.db`

`flux-fs`: `DirHandle::open_claim_store(&self, name: &OsStr, durability: Durability) -> Result<Self::Claims>` (no create, never follows a link); `ClaimStore` gains
`fn count(&self) -> Result<u64>`. `flux-platform`: `RedbClaimStore::create_file` (today's `from_file`, writes `meta.format`) and `RedbClaimStore::open_file(file, durability)`: redb has no open-from-`File` entry point (`Database::open` and `Builder::open` take a path; only `Builder::create_file(File)` takes a handle, redb-4.3.0 `db.rs:2242`), so `open_file` calls `create_file` on the `File` that `open_claim_store` opened without `O_CREAT` (Windows: `FILE_OPEN`), which opens and repairs an existing valid database (the `claims_kill` harness already relies on this); because `create_file` would silently initialise an EMPTY file as a fresh database, `open_file` first rejects a zero-length file as `STATE_CORRUPT`, then reads `meta.format` (an absent `meta` or `claims` table is corrupt; a value other than 1 is incompatible); POSIX opens `O_RDWR | O_NOFOLLOW | O_CLOEXEC`,
Windows reuses `open_file_at` with `FILE_OPEN`. The fake (`FaultFs`) gets the same method and the conformance suite gains an open case. The errors reach the run as
`STATE_CORRUPT` and `INCOMPATIBLE_STATE` (the plan decides whether that needs two new `flux_fs::Code` values or a typed error).

### Messages and exit codes

| Situation | Output | Exit |
|---|---|---|
| `--resume`, no prior | `no prior operation; starting new` | as the run |
| `--resume` adopted | `resuming operation <id> (<n> entries claimed)` then the normal report, with `resumed <m> already-complete files` | as the run |
| resumable prior, no flag | `RESUMABLE_OPERATION_EXISTS: operation <id> is <STATE> (<manifest path>); run again with --resume to continue it or --restart to supersede it (its partials are deleted)` | 3 |
| `--resume --restart` | clap usage error | 2 |
| incompatible | `INCOMPATIBLE_STATE: <what differs>` | 3 |

### Crash table (a crash inside the resume itself)

| Crash point | Left on disk | Next `--resume` |
|---|---|---|
| after the lock, before the manifest write | prior workspace untouched; lock record maybe rewritten | adopts again |
| after the manifest write (`TRANSFERRING`) | as above | adopts again |
| mid-walk | `state.db` holds the union of every run's synced claims | adopts again; files published and claimed are skipped, the rest redone |
| during the final sync | as a killed run | as above |

Resuming a resume is safe by induction: the operation id never changes, claims are never released, and every redo goes through the verified path.

## Known limits (to file in `TODO.md` with the implementation)

1. Claims are keyed by directory identity: a destination directory deleted and recreated between runs has a new identity, so its old claims miss and its files are copied again (safe). If the filesystem reuses an inode number, a stale claim can make a later target see a foreign claim and fail loudly with `DESTINATION_NAMESPACE_COLLISION` (never a silent overwrite).
2. A destination changed during the downtime is caught only by the size and mtime check; a same-size, same-mtime change is not.
3. Files published in the unsynced window (limit 4a of cut 8b) are copied again under overwrite.
4. Orphan partials of targets that vanished between the runs are not swept (slice 9b).
5. A manifest-only workspace in state `CREATED` resumes with zero progress; one in any later state without `state.db` is `STATE_CORRUPT`.
6. Claim keys contain the filesystem's device number. Linux does not guarantee that `st_dev` is stable across a reboot (device numbers of removable, LVM or network-backed volumes can change), and an interrupted copy is often followed by a reboot: when the numbers change every claim misses and the resume copies everything again under the existing-file policy (safe, but the resume saves nothing). Not testable in CI; to be measured on real systems before slice 9c.
7. Weak-identity destination directories never get claims: a resumed run reports their existing files as collisions (see the walk section).

## Not in this cut

The WAL, `PREPARE_COMMIT`/`COMMIT`, commit recovery; chunk checkpoints and `--resume-verify`; `--dry-run` (including `--dry-run --resume`); the `SCANNING`, `PAUSED` and `COMMITTING` states;
lock-first discovery and `P/.flux/atomic/` (section 120); `flux cleanup`, `cleanup_pending` completion and stale-lease classification (slice 9b); several resumable priors being resumed (refused).

## Testing

- **Fake matrix** (`run/tests.rs`): adopt for each prior state; no prior; two priors; format 1 and 2 priors; every compatibility row (each option, durability both directions, mapping path and identity mismatch); the walk's five claim cases plus mismatch-by-size, by-mtime, by absence, under each of the three policies; a weak-identity directory (every pre-existing file is a collision); a manifest with no `state.db` in `CREATED` (fresh store) and in `TRANSFERRING` (`STATE_CORRUPT`); a zero-length `state.db` (`STATE_CORRUPT`); the fingerprint recomputation mismatch; a source that vanished or became unreadable between runs; a destination file replaced by a directory between runs; the lock record carrying the adopted id.
- **`state.db` reopen:** conformance cases for open on the fake and the real store; open after a kill (the `claims_kill` harness gains a reopen-and-continue case); wrong `meta.format`; missing tables.
- **End to end, real filesystems, all three CI systems:** kill a tree copy at a stall point, run `--resume`, assert the destination equals the source byte for byte, the second run copied fewer files than the total, and (under the default overwrite policy; with `--update` or `--skip-existing` a file published in the unsynced window is never claimed) the claim count equals the file count; the same for a single file; `--resume` against an incompatible option exits 3; a second kill during the resume followed by a third run.
- Each new behaviour gets a one-line mutant (skip the verify; adopt a new id; ignore the options; overwrite `meta.format` on open) that must turn a named test red.

## Files expected to change

`crates/flux-fs/src/{fs.rs,claims.rs}` (`open_claim_store`, `count`); `crates/flux-platform/src/{claims.rs,dir_unix.rs,dir_windows.rs}`; `crates/flux-core/src/{state.rs,prior.rs,tree.rs,fault_fs.rs}`;
`crates/flux-core/src/run/{mod.rs,session.rs,place.rs}`; `crates/flux-cli/src/{main.rs,report.rs,exit_code.rs}`; tests in `crates/flux-core/src/run/tests.rs`, `crates/flux-platform/tests/`, `crates/flux-cli/tests/`; `TODO.md`.

## Open points for the plan (decided there, not here)

The typed error for "corrupt" versus "incompatible" at `open_claim_store`; how `Place::adopt` shares code with `create` for the single-file record; whether `RunConfig` carries `resume` or a `ResumeIntent` with the compared options; the exact human wording of the new lines.

## Stand-downs

Panel round 1 (solo panel plus agy), 2026-10-08. Every finding was disposed of; the two stand-downs:

- `DISCARDED-BELOW-FLOOR: the fingerprint does not cover roots or durability` unreachable as a defect because both are compared directly against the invocation in the Compatibility table above, and the fingerprint's purpose is to prevent accidental resume with incompatible semantics (`FLUX_FULL_UPDATED_SPEC_V16.md` section 19, line 1475), not to detect manifest tampering.
- `REJECTED: destination root identity is missing from the roots check` because the workspace lives inside DEST (`DEST/.flux/operations/<id>`, found by `prior.rs` `scan_tree` through the handle opened at DEST itself) and claim keys carry the volume identity of each directory (`crates/flux-fs/src/claims.rs` `ClaimKey`), so a different volume mounted at the same path has neither a prior to find nor claims that match.
- Owner decisions raised by the panel and pending: decision 8 (the refusal no longer carries a claim count); decision 2 as amended (a mismatch goes to the normal path, not a forced redo).
