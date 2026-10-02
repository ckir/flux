# Cut 7b: operation state version 2, `cleanup_pending`, and the heartbeat

Builds on cut 7a (`docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md`, merged as `f380220`). The
spec is `FLUX_FULL_UPDATED_SPEC_V16.md` (`spec:N` below). Branch `spec/cut-7b`.

## Why

Two spec MUSTs bind the runs 7a already makes, and 7a meets neither:
- **§218** (spec:9318-9361): a successful run MUST record `COMPLETED` with `cleanup_pending = true`, plus the outstanding
  artifacts, durably before it removes anything or releases its lock. Today a run whose temporary could not be removed
  keeps a plain `COMPLETED` state, with no flag and no list (7a decision G2).
- **§249.1** (spec:11233-11257): each single-file state record "must contain" 16 fields. 7a writes 8 of them.

7a's spec also handed the heartbeat to 7b (lines 154, 39-42). The owner chose to build it now, in the lock record, at
natural points of the copy.

## Scope (owner, after AGY-FIRST; briefs `.clavity/seams/cut7b-forks.md`, `cut7b-forks-2.md`)

**In 7b:**
- State `format_version` 2, for both kinds, and reading version 1.
- §249.1's missing fields in the single-file record.
- §218's `cleanup_pending` and `cleanup_pending_artifacts` in both kinds, in §218's order.
- The heartbeat: `last_heartbeat_wall_time` in the lock record, refreshed during the copy.

**Deferred, each to the cut that first reads it (F-1):**

| Item | Cut | Why |
|---|---|---|
| `configuration_fingerprint` (§19, §121) | 9 | read only by resume; most §121 options do not exist yet |
| checkpoints and `last_checkpoint` (§19, §139, §148, §164) | 8/9 | the spec defines them only through the WAL and `state.db` (§119 layout, spec:5715-5726) |
| `roots` (§19, §18.3) | 10 | one root until several sources exist |
| `last_heartbeat_monotonic` (§229.2) | 9 | "diagnostic/current-session data only" |
| finishing a PRIOR run's `cleanup_pending` (§21.1 "may", spec:1617) | 9 | cleanup of a prior operation is cut 9's row |
| stale classification from heartbeat age (§102) | 9 | heartbeats are read there |

Each deferral is added to `TODO.md` with its cut.

## Decisions (owner rulings)

1. **F-1 scope** is the list above.
2. **F-2:** no checkpoint and no `last_checkpoint` key in version 2. A later version adds them with their writer.
3. **F-4:** both kinds carry `cleanup_pending` and `cleanup_pending_artifacts`. The run writes them in §218's order.
   Finishing a prior run's cleanup is cut 9's.
4. **F-5 (agreed by agy and driver after negotiation, `.clavity/scratch/cut7b/f5-negotiation.md`):** the single-file
   record's new fields hold real values, never an identity of another kind. "Path Identity = FluxPathKey ... Object
   Identity = FileIdentity ... These identities must never be substituted for one another" (spec:11138-11152). The
   values are in "Version 2" below.
5. **F-6:** a version-1 state is read as version 1 and classified exactly as 7a classifies it. `--restart`
   superseding a version-1 prior rewrites it AS VERSION 1, changing only `state` and `superseded_by`. Rewriting it as
   version 2 would invent the fields its writer never recorded.
6. **H-1:** the heartbeat fires inside the copy's 64 KiB read/write loop (`crates/flux-core/src/copy.rs:437`) and at
   every §99 guard call. A monotonic clock decides: the record is rewritten only once the interval has passed since the
   last write (§101: 5 s; "must not be emitted more frequently than necessary", spec:5008). No thread. A read that
   blocks emits nothing meanwhile. §100 allows that: heartbeats are advisory and never authorise deletion while the
   lock is held (spec:4962-4995).
7. **H-2:** a heartbeat write that fails is retried once at once. A second failure stops the run with an error naming
   the lock path. Warning and continuing is unsafe, measured from the code: `still_owned`
   (`crates/flux-core/src/lock/held.rs:73-91`) decodes the on-disk record. A torn record therefore fails every later
   guard, reports `TARGET_LOCK_BUSY` (blaming another run), and `release` leaves the lock behind (`Released::NotOwned`).
8. **H-3:** the heartbeat writes through a new NON-syncing path. `rewrite_record` syncs (held.rs:61-62); a sync every
   5 s would stall the copy for an advisory value. A power loss can tear the record whether or not it was synced.
9. **H-4:** no `still_owned` check before a heartbeat write. The held handle owns the OS-native lock, and a
   `--break-lock` takeover must acquire that lock first (`crates/flux-core/src/lock/takeover.rs:61`, established in the
   7a Part 3b-1 capstone).
10. **C-1:** EVERY `COMPLETED` write sets `cleanup_pending = true`, before the run removes anything, its own state
    included. `cleanup_pending_artifacts` lists the temporaries the copy could not remove, and is empty when there were
    none. A clean run then removes its state as 7a does, which clears the flag with the record. A run with leftovers
    keeps its state, flag and list for cut 9 (7a G2: no finish-time retry). A crash during the run's own removal
    therefore leaves `COMPLETED` with `cleanup_pending = true`, never a `COMPLETED` that looks finished.

## Version 2

One JSON object, as in version 1. `format_version` is still judged before any other key. Unknown keys are still
`STATE_CORRUPT`. Every key below is REQUIRED in version 2; "nullable" means the key is present with `null`.

**Both kinds:** the eight version-1 keys, with `"format_version": 2`, plus:

| Key | Value |
|---|---|
| `cleanup_pending` | `false` until the `COMPLETED` write, `true` from it on (decision 10) |
| `cleanup_pending_artifacts` | an array of strings, `[]` until the `COMPLETED` write. Each string is a leftover temporary's path, in the hex encoding `target_path_key` uses (the hex of the OS-encoded bytes, `crates/flux-core/src/lock/site.rs:116-117`). For a tree it is relative to DEST, with `/` between components; for a single file it is the name in the target's directory. Lossless, so a non-UTF-8 name can never be listed wrongly. |

**A single file adds** (§249.1; decision 4):

| Key | Value |
|---|---|
| `artifact_type` | `"file"` |
| `attempt_id` | a fresh 32-hex id, generated once per run with the same generator as `operation_id`. It is an `AttemptId`, not the ordinal `attempt_number` (spec:4244-4250). |
| `artifact_generation` | `1` (a counter; one generation per operation until retries exist) |
| `source_identity` | the source file's `FileIdentity`, taken by the run's source check before the lock (7a B1) |
| `target_identity` | nullable. At creation: the identity of the object already at the target name, or `null` if none. In the `COMPLETED` write: the published target's identity. |
| `target_path_key` | the same value as this run's lock record |
| `owner_instance_id`, `boot_session_id`, `creation_wall_time`, `last_heartbeat_wall_time` | the same values as this run's lock record at creation. The state writes `last_heartbeat_wall_time` once (owner); only the lock record's copy is refreshed. |

A `FileIdentity` is a string:
- `"strong:<volume>:<id>"`, `"weak:<volume>:<id>"`, or `"unavailable"`;
- `volume` and `id` are the decimal `ObjectId` fields (`crates/flux-fs/src/fs.rs:52-76`; `id` is a u128).

**A tree manifest** carries only the "both kinds" keys. §249.1 governs the adjacent record. §218's minimum list
(`attempt_id`, `target_identity`, `source_identity`, `artifact_generation`, spec:9330-9340) sits under "For isolated
single-file operations" (spec:9296-9297), so it binds the adjacent record too. The tree's identity metadata arrives
with the cuts that read it.

**Consistency (version 2; a violation is `STATE_CORRUPT`):**
- `cleanup_pending = true` only with `state` `COMPLETED`. A `COMPLETED` record always has it `true` (decision 10).
- A non-empty `cleanup_pending_artifacts` only with `cleanup_pending = true`. Each entry is non-empty lowercase hex of
  even length.
- In a single-file record, `COMPLETED` requires a non-null `target_identity`. A published target whose identity cannot
  be read records `"unavailable"`, never `null`.
- `attempt_id`, `owner_instance_id` and `boot_session_id` are 32-hex ids, and the two times are decimal digits, as
  version 1 already checks for `operation_id` and `created_at`.

**Reading.** A reader accepts versions 1 and 2. A version-1 state yields the same classification 7a gives, and its new
fields are absent. Version 3 or higher is `INCOMPATIBLE_STATE`, as an unknown version is today. 7a's test
`format_version_is_judged_before_any_other_key` changes from version 2 to version 3 as its incompatible example.

**Writing.** A new run writes version 2. A rewrite of an existing state keeps that state's version: the run's own
FAILED and COMPLETED writes, and `--restart` setting ABANDONED and `superseded_by` on a prior (decision 5).

## The heartbeat

- `RunConfig` gains the heartbeat interval. The CLI passes 5 s. A debug build reads
  `FLUX_TEST_HEARTBEAT_INTERVAL_MS`, as it reads the 7a stall hook (`crates/flux-cli/src/main.rs` `debug_hook`), so an
  end-to-end test can watch the value move. A release build has no override.
- The run keeps the monotonic instant of its last record write. The copy loop and the guard call one heartbeat
  function. When the interval has passed, it rewrites the lock record with only `last_heartbeat_wall_time` changed:
  - through the held handle, with no sync;
  - `operation_id` and `owner_instance_id` unchanged, so `still_owned` still holds;
  - the held record updated too, so a later `rewrite_record` (Q-K) carries the newest value.
- Both run kinds heartbeat:
  - a tree in each file's loop and at every guard call of the walk;
  - a single file in its loop;
  - both at every other ownership check the run makes after its record exists, so a long `--restart` sweep (one
    ownership check before each deletion, 7a Q-H) heartbeats too.
- Before the record is written (7a step 5), the heartbeat does nothing: there is no record to refresh.
- **Failure (decision 7):** after the retry fails, the copy stops at that point and the run's stop is a failure at
  the lock (`RunError::Failed`, naming the lock path and the I/O error), never `TARGET_LOCK_BUSY`. The finish takes the
  failure path, and whatever the record then holds decides it:
  - **Intact:** FAILED and the release proceed as for any failure.
  - **Torn:** the ownership check fails, nothing more is written, and the lock is closed without unlinking. The next run
    meets `TARGET_LOCK_UNCERTAIN`, which `--restart --break-lock` clears.
  - Either way the report names the heartbeat failure, not a lost lock.
  - Once a heartbeat has failed, the run makes no further heartbeat attempt. The copy's removal of its temporary, the
    guards around it, and the finish run without one, so a second failure can never replace or nest inside the first.

## The finish (changed from 7a)

`complete` writes `COMPLETED` with `cleanup_pending = true` and the leftover list, durably and while owned, as 7a's
single `COMPLETED` write does today. A single file also sets `target_identity` to the published target's identity in
that write. Then, as in 7a:
- no leftovers: Q-K, then remove the state, then the release;
- leftovers: keep the state, then the release.

A single-file copy that completes leaves no temporary in 7a (a failed publish is `FAILED`, not `COMPLETED`), so its list
is `[]`.

## Crash table additions

| Crash | Left behind | Next run |
|---|---|---|
| during a heartbeat write (power loss) | a torn lock record | `TARGET_LOCK_UNCERTAIN`; `--restart --break-lock` proceeds |
| after the `COMPLETED` write, before the state's removal | `COMPLETED`, `cleanup_pending = true`, its list | proceeds (§21.1 "completed (including cleanup_pending)"); the state is cut 9's |

## Model

`models/lockproto` does not represent heartbeat time (`trace.toml:460`, `:502`). A heartbeat rewrite keeps the owner,
so the model's ownership predicate is unchanged, and a torn record is the torn state the model already has. The
implementation map needs no new item. This is a claim for the plan panel to check.

## Testing

All on the fake unless named:
- **Codec:** version 2 round-trips for both kinds; every required key is checked (a missing one is `STATE_CORRUPT`);
  version 1 still decodes and classifies as before; version 3 is `INCOMPATIBLE_STATE`; a superseded version-1 prior is
  rewritten as version 1; a `FileIdentity` string round-trips each variant, including a u128 id above u64.
- **Single-file record:** all 16 §249.1 keys are present. `target_identity` is `null` for a new target, holds the old
  object's identity when the copy replaces one, and holds the published identity after `COMPLETED`. `attempt_id`
  differs between runs and from `operation_id`.
- **`cleanup_pending`:** a clean run's `COMPLETED` write carries `true` and `[]`, shown by a fault on the state's
  removal that leaves the record behind. A run with an undeletable temporary keeps `true` and lists it in the hex
  encoding, including a non-UTF-8 name on Linux (real filesystem).
- **Heartbeat:** with a zero interval, the lock record's `last_heartbeat_wall_time` advances during a copy and
  `operation_id`/`owner_instance_id` stay. A failed heartbeat write is retried once; two failures stop the run naming
  the lock path; a torn heartbeat write is reported as that failure, not `TARGET_LOCK_BUSY`. No heartbeat write
  syncs.
- **End to end (CLI):** a stalled debug run with a short `FLUX_TEST_HEARTBEAT_INTERVAL_MS` shows the lock record's
  heartbeat move.

Every new test is proven red under a mutant of the behaviour it pins.

## Spec refinements recorded by this cut

- §218's lifecycle is satisfied in 7b by one `COMPLETED` write carrying `cleanup_pending = true` and the list. "Clear on
  success" is the state's removal. A listed artifact is never retried in 7b (G2); cut 9 completes it.
- §249.1's `last_heartbeat_wall_time` in the adjacent record is the creation value. The lock record carries the live
  heartbeat (§229.2 places the lease in the lock record).
- The state's version follows its writer: a rewrite never upgrades a record.
