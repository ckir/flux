# Cut 9b: the cleanup lifecycle (`flux cleanup`, and finishing a prior run's cleanup)

Status: DRAFT for owner review (2026-10-08), branch `spec/cut-9b` off the merged cut 9a. Scope and every fork below were negotiated with agy
(consults in `.clavity/seams/cut9b-*.md`, replies in `.clavity/scratch/cut9b-scope/`) and decided by the owner on 2026-10-08; section "Writer's
rulings" lists what the owner has not yet seen and must confirm at this review. Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 21.1, 24,
102, 130, 216-218, 222-223, 229.5, 234.2, 240.1, 240.3, 251, 259.6). Previous slice: `docs/superpowers/specs/2026-10-08-cut-9a-resume-design.md`.

## Why this cut

Since cut 7b a finished run leaves `cleanup_pending` records, kept workspaces and `.creating` / `.removing` directories that nothing ever removes
("left for cut 9's cleanup" appears in `prior.rs`, `state.rs`, `run/mod.rs`), and a crashed or failed run that nobody resumes stays for ever.
This cut gives them an owner: a normal `flux copy` finishes the cleanup of a prior run, and a new `flux cleanup DEST` command lists and removes
what is left.

## Scope (owner-approved 2026-10-08)

IN:
1. Validation of `cleanup_pending_artifacts` before any deletion by it (relative, no `..`; 9a debt).
2. A `flux copy` into a destination finishes a prior COMPLETED operation's `cleanup_pending` (tree and single file), never blocking the copy.
3. Removal of `<id>.creating` and `<id>.removing` debris under `DEST/.flux/operations/`.
4. Classification into the six statuses of section 251.1 with an eligibility marker, the 30 s lease gate, the 7-day retention and the
   backward-clock rule.
5. Revalidate-then-delete in the crash-safe order of section 223, including a crash-orphaned root lock.
6. `flux cleanup DEST [--dry-run] [--force] [--json]` for directory operations.

OUT (stays in TODO.md): the standalone catalog (section 250) and `DEST/.flux/standalone/`, `flux cleanup --target`, `--break-lock` for cleanup,
`P/.flux/atomic/` staging, automatic garbage collection by a normal run (section 24.3: only the explicit command and the copy's prior-cleanup
completion exist), the WAL (9c), and the sweep of partials of vanished targets that no recorded operation names (9a known limit 4, narrowed below).

## Decisions

Owner-approved (2026-10-08):

1. **Who finishes a prior's cleanup (F1).** Both `flux copy` and `flux cleanup`. A copy does it in its existing prior-scan step, under the
   root lock it already holds, before its own work; every failure is a warning and never changes the copy's outcome or exit code (section
   21.1 "may complete the prior cleanup", acceptance 49, section 218 "a failed cleanup MUST NOT invalidate an otherwise successful transfer").
2. **Which lock (F2).** Discovery and classification hold no lock. Each deletion pass takes DEST's root lock without waiting, re-reads and
   re-classifies under it, deletes, releases. A busy lock means nothing in that DEST is deleted in this pass (see "Locking"). `--dry-run`
   never acquires the lock for ownership (it only probes, D1).
3. **Retention configuration (F3).** 7 days, a constant (`DEFAULT_RETENTION`) carried in `CleanupConfig.retention`. No flag, no environment
   variable. Tests set the field. `--force` is the only user-facing bypass and bypasses retention only.
4. **The 30 s lease (F4).** A gate: an unlocked, not-yet-completed operation whose lease is younger than 30 s is never STALE and never eligible.
   The OS-native lock stays the primary authority; the lease only adds to it. A wall clock behind the recorded time gives UNCERTAIN (section 229.5).
5. **Liveness probe (D1).** To tell LIVE from dead, classification reads the lock record first, then makes one `try_lock` and drops the handle at
   once. "Takes no lock" (spec lines 72, 1789, 3082) is read as: no lock is held across any decision and nothing is written. Linux uses `flock`
   (`flux-platform/src/lock_file.rs`), which `F_GETLK` cannot query, and Windows `LockFileEx` has no status query, so no non-acquiring probe exists.
   Cost: a copy that starts in that instant is refused with `TARGET_LOCK_BUSY` (known limit 1).
6. **Deleting a non-COMPLETED tree operation (F5).** Mark the workspace ABANDONED (persisted cleanup intent), sweep `DEST` for that one operation's
   `<name>.flux-partial.<id>` files exactly as `--restart` does (`Place::sweep`, the J1 walk: known id only, reserved control directories skipped,
   deletion through handles opened one component at a time), and retire the workspace last, only when every partial is gone. If a partial stays, the
   ABANDONED workspace stays as the record and the next cleanup retries. This is not a search for unknown artifacts (sections 234.2, 251.3): the id is
   known from the workspace being removed.
7. **The age clock (F6).** Retention age = now minus the manifest file's modification time (every state change is a temp-and-rename write, so it is
   the last state change). Lease age = now minus the lock record's `last_heartbeat_wall_time` when a record names this operation, else the same
   manifest age. A modification time that cannot be read, or lies in the future, gives UNCERTAIN.
8. **Cleanup's own lock (F7).** Cleanup acquires `P/<DEST-name>.flux-lock` (or the root and fallback names of section 96.1) through the normal
   acquire path with `workspace_path = "none"` (section 259.6; `lock/site.rs` already treats it as a cleanup lock), deletes, and releases it
   normally. The normal acquire REFUSES a free lock whose decodable record names a missing workspace (`Classified::Untrusted`,
   `ARTIFACT_OWNERSHIP_UNCERTAIN`, `lock/obtain.rs`; a copy still refuses so). Cleanup's acquisition (`obtain_cleanup_lock`) differs from `obtain`
   in exactly that case: when the owner is demonstrably gone (the probe's `try_lock` succeeded, the record decodes) and the 30 s lease gate holds, it
   recovers the lock by the section 240.3 move-aside (`lock/recover.rs`), which hands back a held lock that cleanup then uses as its own. Never a
   plain unlink. Every other classification of the existing lock behaves as in `obtain`.

## Statuses, eligibility and the table

An **entry** is one of: `operation` (a workspace `DEST/.flux/operations/<id>/`), `debris` (`<id>.creating` or `<id>.removing`), `root-lock` (DEST's
root lock file when its record names a workspace that does not exist, and each `<lock-name>.broken.*` beside it; found by listing the lock's
directory non-recursively, section 251.1). Every entry gets one of the six names of section 251.1 and an `eligible` flag.

Evaluated per entry in this order; the first match wins. "Lock free" means the probe's `try_lock` succeeded, "busy" that it did not.

| # | Entry and facts | Status | Eligible |
|---|---|---|---|
| 1 | The lock is busy and its record names this operation | `LIVE` | no |
| 2 | Manifest missing, not a regular file, undecodable, naming another id, or not a tree record | `CORRUPT` (kept for diagnosis) | never |
| 3 | Manifest of a format this binary does not know | `UNCERTAIN` (note: "format N") | never |
| 4 | `COMPLETED` (any format; `cleanup_pending` holds by the format-2/3 invariant, and a format-1 manifest has no artifact list, so it has none; the workspace or a leftover still exists) | `COMPLETED_BUT_UNCLEAN` | lock free |
| 5 | `ABANDONED` | `STALE` | lock free |
| 6 | `CREATED`, `TRANSFERRING`, `FAILED`: mtime unreadable or in the future, or a record heartbeat in the future | `UNCERTAIN` (`LEASE_AGE_UNCERTAIN`) | never |
| 7 | resumable, lease age < 30 s | `RESUMABLE` | never (not even `--force`) |
| 8 | resumable, retention age >= 7 days, lease age >= 30 s | `STALE` | lock free |
| 9 | resumable otherwise (so lease age >= 30 s here) | `RESUMABLE` | only with `--force`, lock free |
| 10 | `debris` (`.creating` / `.removing`) | `STALE` (note: debris) | lock free |
| 11 | `root-lock` (the lock, or a `.broken.*`): free, record decodes, the workspace it names is missing, heartbeat not in the future, lease age >= 30 s | `STALE` (note: orphan lock) | yes (acquisition reclaims it) |
| 12 | `root-lock` as row 11 but lease age < 30 s, or the heartbeat is in the future | `UNCERTAIN` (note: "lease younger than 30 s", or `LEASE_AGE_UNCERTAIN`) | never |
| 13 | `root-lock` or lock file whose bytes are torn, empty, newer-versioned or not a Flux record | `UNCERTAIN` (note: needs `--break-lock`, not in 9b) | never |

When the lock is busy and its record names another operation, or names none, every other row of this DEST is classified by the table but all are
ineligible in this pass and carry the note "destination lock held by <op or unknown>". The classification never turns a row into LIVE except row 1.

The `eligible` marker means "would be deleted now"; `--dry-run` prints it without acting. A row eligible at listing time may be skipped at
deletion time (revalidation); that is reported as an action, not an error.

### Output

Text, one line per entry, then one line per action, then a summary (all on stdout; errors and warnings on stderr):

```
STATUS                 ELIGIBLE  KIND       ID                  STATE         AGE   NOTE
COMPLETED_BUT_UNCLEAN  yes       operation  <id>                COMPLETED     2d    2 leftovers
STALE                  yes       debris     <id>.creating       -             -     debris
removed <path>
kept <path>: <error>
skipped <id>: <reason>
cleanup: <entries> entries, <removed> removed, <kept> kept, <skipped> skipped
```

For a `root-lock` entry the `ID` is the lock file's name (`<name>.flux-lock`, `.flux-dir.lock`, `.flux-root.lock` or `<lock>.broken.<id>`), `STATE` is `-` / `null`,
and AGE is the lease age. AGE prints the retention age as `<n>s`, `<n>m`, `<n>h` or `<n>d` (largest unit with a non-zero count), `-` when unknown. With `--dry-run` the action
lines are absent and the summary reads `cleanup (dry run): <entries> entries, <eligible> eligible`.

`--json` prints one object on stdout: `{"destination": <string>, "dry_run": <bool>, "entries": [{"status","eligible","kind","id","state","age_seconds","note"}],
"actions": [{"action": "removed"|"kept"|"skipped","path","reason"}], "summary": {"entries","removed","kept","skipped"}}`; `state`, `age_seconds` and
`reason` are `null` when absent. This is the cleanup command's own report; the copy report keeps its 18 keys.

### Exit codes (section 251, section 55)

- 0: classification completed (LIVE, RESUMABLE, UNCERTAIN, CORRUPT rows are reported, not failures), and every deletion attempted succeeded or was
  skipped by revalidation.
- 1: a deletion that was attempted failed, or a directory cleanup needed to list could not be listed (its classification is incomplete).
- 2: usage error (clap; a missing DEST).
- 3: refused as a whole: DEST exists and is not a directory (`DESTINATION_ERROR`); DEST's parent cannot be opened (`DESTINATION_ERROR`);
  `DEST/.flux` or `DEST/.flux/operations` is not a directory (`CONTROL_PLANE_NAMESPACE_CONFLICT`); the lock capability check of `lock/obtain.rs`
  fails (`REMOTE_LOCK_UNSAFE`), checked before any probe.
  The root lock (and its `.broken.*` files) lives beside DEST, not in it, so it is ALWAYS examined, including when DEST has no `.flux/operations`
  and when DEST does not exist at all (a copy that crashed between taking the lock and creating DEST leaves exactly that, and a copy refuses it
  with a pointer to `flux cleanup DEST`). Such a DEST yields only the lock's rows (possibly none), exit 0; nothing is an error just because the
  operations directory is absent.

## Locking

Classification, including `--dry-run`, probes the lock once (decision 5) and holds nothing. A deleting pass then calls `obtain_cleanup_lock`
(decision 8). `TARGET_LOCK_BUSY` is not an error: every eligible row is reported `skipped <id>: destination lock busy`, nothing is retried, and the
exit code is 0 (a live run is the normal reason). If the pass later loses ownership (the `checked` test fails), it stops, the remaining eligible rows are
`skipped <id>: lock lost`, and the exit code is 1.

## Deletion procedure (per eligible row, under DEST's root lock)

The pass acquires the root lock once (decision 8) after listing, and for each eligible row: re-read the manifest, re-probe nothing (the lock is
held), re-classify; if the row is no longer eligible, report `skipped <id>: changed since listing`. Before every destructive step call the same
ownership check a `--restart` uses (`session::checked`), so a lost lock stops the pass.

- **COMPLETED_BUT_UNCLEAN.** Delete each leftover named by `cleanup_pending_artifacts` (artifact rules below), then `retire_workspace`
  (`state.rs`: rename to `<id>.removing`, remove `manifest`, `manifest.tmp`, the probe files and `state.db`, remove the directory), then the empty
  control directories (`remove_empty_control_dirs`). A leftover that cannot be deleted is `kept` and the workspace stays (its record still names it).
- **STALE or forced RESUMABLE operation (decision 6).** Write ABANDONED (`superseded_by` absent); sweep that id's partials; retire the workspace only
  if none stays; else `kept`.
- **Debris.** `<id>.removing`: finish what `retire_workspace` would have done (remove the known names, then the directory). `<id>.creating`: remove the
  known names (`manifest`, `manifest.tmp`), then the directory. A name inside that is not in the known list is `kept` with a warning and the
  directory stays (never recursive deletion).
- **Orphan root lock.** Done by the acquisition itself (decision 8): the move-aside, then removal of the moved-aside `<lock>.broken.*` once the new
  lock is held; a failure to remove it is `kept`. It comes first, since nothing else can be deleted without the lock.
- **A `<lock>.broken.*` file** (a dead owner's moved-aside lock, left when its removal failed): removed as a file once the pass holds the lock.
- **Order inside a pass.** Acquire (reclaiming an orphan lock if there is one), then debris and COMPLETED rows, then abandoned-and-swept operations,
  then `<lock>.broken.*` files.

A crash anywhere leaves only: a `.removing` directory, an ABANDONED workspace, or a moved-aside lock; each is a row of the next cleanup.

### Artifact rules (the 9a debt, and the name guard)

A `cleanup_pending_artifacts` entry (native hex, `state.rs`) is used for deletion only when all hold:

1. It decodes, is relative, and every component is a normal name (no `..`, `.`, empty component, root, prefix or NUL). Absolute paths decode (the
   format-3 `source_root` needs that) but are refused here.
2. Its final component is `<name>.flux-partial.<id>` with `<id>` equal to the record's own `operation_id`. A corrupt record cannot name a user
   file such as `report.doc`.
3. It is reached one component at a time through `DirHandle::open_dir`, which refuses links (`SAFETY_REJECTED` or `DESTINATION_ERROR`; either is
   `kept`), and its last component is a regular file (checked by `metadata` on the handle, not by following a path).

4. A component that does not exist (`NotFound`) at any step of the walk means the leftover is already gone: it counts as removed, not kept. A cleanup
   killed after deleting some leftovers therefore converges on re-run instead of sticking on the first missing one.

For a single file, the artifact must equal that target's own partial name for that id exactly (the name `copy::temp_path` yields for this target and
id); anything else is refused. A refused entry is reported (`kept <path>: not a recognized leftover`), the operation stays COMPLETED_BUT_UNCLEAN, and
nothing else is deleted for it.

## What `flux copy` does

`prior.rs`'s scan (the run's step 4) only reads. The deletions happen where `--restart`'s `supersede` runs, after the run holds the root lock and has
written its own state and lock record (so `checked` can prove ownership):

- `scan_tree` also returns every COMPLETED prior with its record and every `.creating` / `.removing` entry whose id is not this run's. Corrupt or
  unknown-format manifests still make the scan fail as today (unchanged behaviour).
- For each such COMPLETED prior and each debris entry the run applies the deletion procedure above (the lock is already held, so there is no second
  acquisition and no re-probe), calling `checked(locked)` before every mutation. COMPLETED priors need no retention; STALE and RESUMABLE priors are
  NOT touched by a copy (a RESUMABLE one still triggers `RESUMABLE_OPERATION_EXISTS`; `--restart` still supersedes it).
- `scan_file` likewise returns a COMPLETED prior record of this target; the run deletes its validated artifact and `remove_record`s it.
- Every failure is a stderr warning: `warning: could not finish the cleanup of the earlier operation <id>: <path>: <error>`. Success prints
  `note: finished cleaning up the earlier operation <id> (<n> leftovers removed)`. The copy's report, JSON keys (18) and exit code are unchanged.
- With `--resume` the same step runs; the adopted prior is not a candidate.
- The existing refusal for an orphan root lock (`ARTIFACT_OWNERSHIP_UNCERTAIN`, exit 3) keeps its code; its text gains a pointer to
  `flux cleanup DEST` as the way to remove it.

## Units

- `flux-core/src/cleanup/artifacts.rs`: the artifact rules; pure on strings, plus the handle walk.
- `.../status.rs`: `classify(Facts) -> (Status, Eligible)`, the table above; pure, no I/O, table-tested.
- `.../discover.rs`: lock-free listing of `DEST/.flux/operations/`, the probe (D1), the facts; produces the entries.
- `.../delete.rs`: the deletion procedure; reuses `state::retire_workspace`, `Place::sweep` and `lock::recover`.
- `CleanupConfig { retention, lease_threshold, now, force, dry_run }` with `DEFAULT_RETENTION = 7 days` and `LEASE_THRESHOLD = 30 s`; `now` is an
  injected wall-clock reading in nanoseconds (tests move it).
- `DEST` and its parent are opened exactly as `flux copy` opens them (`run/place.rs`), so the lock site and the capability check are the same.
- The J1 walk is extracted from `TreePlace::sweep` into a free function taking the filesystem, DEST's handle and shown path, the set of ids and a check
  callback; `sweep` and `delete.rs` both call it (a plan-level refactor with the existing `--restart` tests as its oracle).
- `obtain_cleanup_lock(site, capability, cfg: &CleanupConfig, operation_id)`: `operation_id` is a fresh id recorded with `workspace_path = "none"`;
  the config supplies `now` and the lease threshold for the gate on reclaiming an orphan lock.
- The end-to-end kill test uses the debug-build stall hook (`FLUX_TEST_STALL_AT`, `flux-cli/src/main.rs`), which counts guarded mutations; cleanup's
  mutations pass through the same `checked` guard so the hook can stop it between any two.
- `prior.rs`: `Scan` gains `completed` and `debris`; `session.rs` calls the delete procedure; `lock/` gains `obtain_cleanup_lock`.
- `flux-cli`: `Commands::Cleanup`, `cleanup_report.rs` (text and JSON), the exit-code mapping.
- `FaultFs` (`fault_fs.rs`) gains whatever the tests need to move modification times and fail removals (it has `set_modified`, a claim-count fault key).

## Known limits (added to TODO.md when this lands)

1. A copy that starts in the instant a cleanup (or a classification) probes the lock is refused with `TARGET_LOCK_BUSY` (D1). Cleanup probes once per
   operation directory and holds nothing across the listing.
2. Partials of a vanished target are swept only when their operation is removed by `flux cleanup` (STALE or forced) or superseded by `--restart`
   (the J1 walk). A run resumed to completion does not record them: they stay (9a limit 4, narrowed, not closed).
3. Single-file operations' state records (`<target>.flux-state.<id>`) are completed only by a copy to that target; `flux cleanup DEST` does not
   list them (the standalone catalog is out of scope).
4. Retention reads the manifest's modification time: a restore from backup or a tool that rewrites times changes the age. The ownership checks,
   not the age, are what keep a live or ambiguous operation safe.
5. A long `--resume` skip refreshes no heartbeat (9a debt); a live resume holds the lock and is LIVE, so only an unlocked operation depends on the
   lease.
6. `--force` can delete a RESUMABLE operation and with it the progress `--resume` would have used; this is its purpose.
7. A boot-session mismatch is not used: liveness is decided by the OS lock, the lease gate and the clock rule only (macOS and Windows boot
   identifiers are boot times and are unmeasured under clock steps).
8. `--break-lock` for cleanup, `--target`, the standalone catalog and `P/.flux/atomic/` do not exist; an UNCERTAIN row can only be reported.

## Writer's rulings (owner: confirm or change at spec review)

W1. **COMPLETED_BUT_UNCLEAN has no retention gate.** Section 222 lists eight conditions without retention; section 218 says cleanup must finish a
    completed operation's recorded work; section 130's "older than retention" is for persistent state of operations that may still be resumed.
    Section 251.1 ("passes every deletion precondition now (130, 216, 217, 222)") is ambiguous; this reads it as 222's list.
W2. **Debris is eligible only with the lock acquired**, because a live run holds the lock while its own `.creating` exists and while it retires to
    `.removing`; a busy lock makes the whole DEST ineligible in the pass.
W3. **A copy also sweeps debris** (not only COMPLETED priors), under the lock it holds, warnings only.
W4. **The exit-3 cases** (missing DEST, namespace conflict, remote lock) and the empty report for a DEST without `.flux/operations`.
W5. **Unknown manifest format is UNCERTAIN, not CORRUPT** (a newer binary's record is not damage).
W6. **Report shapes** (text columns, the JSON object, the age unit rule) are this spec's; section 251 fixes only the status names and exit codes.
W7. **The orphan root lock also needs the 30 s lease gate** (consistency with section 102).
W8. **A `<lock>.broken.*` leftover is cleaned too** (section 251.1 classifies it by its record's owner), and the orphan lock is reclaimed inside the
    acquisition rather than as a separate deletion, because a normal acquisition would refuse it.
W9. **Spec gaps found:** section 251.1 does not say whether `COMPLETED_BUT_UNCLEAN` carries a retention gate (W1), and says "takes no lock" for
    `--dry-run` while LIVE can only be learned by a probe (decision 5).

## Tests (Review Focus, each pinned by a named test in the plan)

1. Status table: one test per row of the table, plus a distractor for each neighbouring row (lease 29 s vs 30 s, retention 6d23h vs 7d, mtime equal to
   now vs in the future, a record naming another operation).
2. Artifact rules: `..`, absolute, empty component, a name for another id, `report.doc`, a symlinked directory component, a directory or symlink as the
   last component, a single-file artifact that is not that target's partial; none deletes anything.
3. Revalidation: the manifest changes between listing and deletion (to ABANDONED, to TRANSFERRING with a live lock); the lock turns busy; the row is
   skipped and nothing is removed.
4. Crash safety: a failure injected after each step of each deletion leaves only a `.removing` directory, an ABANDONED workspace or a moved-aside
   lock, and a second cleanup finishes it. At least one end-to-end kill test with a real process.
5. A copy finishes a prior's leftovers and still succeeds (exit 0, 18 JSON keys) when the leftover cannot be removed.
6. `--force` bypasses retention only: a RESUMABLE operation with lease age under 30 s, a LIVE one, an UNCERTAIN one and a CORRUPT one stay.
7. A backward clock gives UNCERTAIN and deletes nothing. An orphan root lock with lease age 29 s is UNCERTAIN and stays; at 30 s it is reclaimed.
   A leftover already deleted (the first of two) does not stop the second from being deleted on re-run.
8. Locking: a busy lock skips every eligible row with exit 0; a lock lost mid-pass stops the pass with exit 1. Probe: a held lock is LIVE in `--dry-run` and in a real run; a free lock is not left held afterwards.
9. CLI: exit codes 0, 1, 2, 3; text and JSON shapes; `--dry-run` changes nothing on disk (a before/after listing).
10. Orphan root lock (also with no `.flux/operations`, and with no DEST at all): a dead owner's lock naming a missing workspace is reclaimed by cleanup (and only reported by `--dry-run`); a copy still refuses
    it with the pointer text; a live owner, a torn record and a foreign file are never touched; a `.broken.*` is removed only when its record decodes
    and its owner is gone.
11. Partials: a STALE operation's partial in a subdirectory and of a vanished target is swept by its id; a partial of another id is untouched.
