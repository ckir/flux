# Cut 9f-1: batched chunk checkpoints and partial-file resume

Status: DRAFT 2026-10-12; the owner ACCEPTED Rulings 1 and 2 as written on 2026-10-12; the rest is not yet approved. Written without a question round (the owner chose to skip questions). Every call below that the owner did not
make is marked **[my call]**; the two that reinterpret the parent spec are collected under "Rulings needed". Adversarial panel: not yet run.
Scope was set by an AGY-FIRST consult and a one-round AGY-NEGOTIATE (briefs `.clavity/seams/next-cut-fork.md`, `9f-split-negotiate-r1.md`,
`9f1-design-fork.md`; replies under `.clavity/scratch/`). Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 23, 35, 136-139, 148, 155-165, 171-175, 178, 185, 195).
Written against `main` f27574e. This is a SPEC, not a plan: it states contracts and decisions. Line-level work waits for the plan, written against the code as it is then.

## Slicing (agreed with the owner 2026-10-12)

Cut 9f is three cuts. **9f-1 (this one):** durable, batched chunk checkpoints in a WAL file, and `--resume` continuing a half-copied large file from them.
**9f-2:** `--resume-verify`. **9f-3:** snapshots, compaction, rotation. Cancellation and pause (sections 132, 187) are none of the three, but 9f-1 must not make
them impossible: the writer drains on request (decision 9).

## Why this cut

Today `--resume` adopts the prior run's id and recopies every unfinished file from byte 0: `stage_file` removes the prior partial (`crates/flux-core/src/copy.rs:519`,
reached under `--resume` because `run/mod.rs:278-279` makes the effective id the prior's). For a 500 GiB file interrupted at 499 GiB that is the whole transfer again.
Success: after a crash or a kill, `--resume` continues each large partial from the last chunk it can PROVE is on disk and matches the source, and never
trusts bytes it cannot prove.

## Goals, non-goals, anti-goals

Goals: checkpoints batched per section 148.3 (1 s or 64 MiB, never per chunk); a WAL that survives torn writes (160, 174); resume that validates before it trusts (35, 137);
no change to the published result of an uninterrupted copy.

Non-goals: `--resume-verify` (9f-2); snapshots, compaction, rotation, a second segment (9f-3); cancellation and pause; retries and attempt isolation (section 225; `attempt_id` is
a constant 1 until retries exist); whole-file worker pools; the s231 emergency persistence path; any transform of the bytes (the design relies on destination bytes equalling source bytes).

Anti-goals (a failure even if every test passes): a checkpoint that names bytes the resume then trusts without validating; a WAL write after a lost lock; the heartbeat
starved by a full checkpoint channel; a measurable slowdown of an uninterrupted large copy that the acceptance run does not report.

## Facts this design rests on (read at main f27574e; the plan re-reads them)

- Chunk model: `chunk_size` = 1 MiB fixed (V16:6349). The copy buffer is `COPY_BUF_MAX` = 256 KiB (`copy.rs:167-172`), one heartbeat per buffer (`copy.rs:586`).
- No checkpoint, WAL or per-chunk digest code exists in `crates/`. Per-operation state is redb `state.db` (`flux-platform/src/claims.rs:21-23`). `blake3` is a workspace dependency (`Cargo.toml:64`; the chunk digest lives in `flux-core` as `wal::digest`, not in `flux-hash`, because the chunk framing (1 MiB boundaries across arbitrary writes) is WAL logic, not a general hash utility).
- The 9e barrier syncs the writers of COMPLETED files, once per batch (`copy.rs` step 6b comment). It never runs inside a file, so it cannot make an in-flight file's checkpoint durable.
- Under `--durability=normal` no data fsync happens at all (9e).
- Spec V16:6415-6416: "No state record may claim bytes are valid if their durable destination content is not known to be valid."
- Spec V16:8003-8006: the WAL is a file in the operation workspace's `wal/` directory, "created only by the slice that writes a WAL file (chunk checkpoints); a workspace without it is valid."

## Decisions

1. **The WAL is one file, `wal/000001`, in the operation workspace (V16 s170, s178).** Created by the first flush that has a record to write; a workspace without it stays valid. One segment only in 9f-1:
   the writer never rotates, and replay reads any segments present ordered by (generation, sequence), never by name (s178).
2. **One WAL writer per operation (s157), fed by a bounded channel (s172).** Copy code sends `CheckpointMessage`s and never touches the file. The writer assigns sequence numbers. The channel capacity is a constant
   pinned by a test (like `BatchPolicy::DEFAULT`): `CHANNEL_CAP = 64` messages, and a blocked sender retries every `SEND_RETRY = 10 ms`, calling the heartbeat on each retry (plan 1 ruling 12). A `Close` for a file flushes that file's pending records FIRST and only then drops its duplicate, so a pending record is never appended after its duplicate is gone (plan 1, Task 7). A full channel blocks the sender, **but the blocked sender keeps calling the heartbeat** (see decision 8).
3. **Digests. [my call, grounded in s35.2 and s138]** Each 1 MiB chunk of the SOURCE gets a BLAKE3 digest (32 bytes), computed incrementally on the copy path across the 256 KiB reads (the last chunk is shorter).
   One digest serves as both the source and the destination checkpoint hash, because 9f-1 copies bytes unchanged (a non-goal if that ever changes). Reason: under Normal no data fsync happens, so a record can survive a
   power loss that dropped the data it names; only digest validation at resume makes the record self-checking.
4. **Records.** Logical contract (s159; the plan fixes the byte layout, which must be versioned, fixed-endian, with the checksum covering header, sequence, type and payload, and a trailing commit marker):
   every record carries `magic, format_version, operation_id, generation, sequence_number, record_type, payload_length, payload, checksum, commit marker`. The checksum is BLAKE3 truncated to 16 bytes, the same
   construction the lock record uses (V16:415) **[my call]**. Per-chunk digests are what s210 (V16:9046-9083) requires to stay individually recoverable, retained until the operation is finalized (s211), which is also why decision 11 keeps the WAL until then. Two record types:
   - `FILE_BEGIN { file_id, attempt_id, destination (display path), source_len, source_mtime, source_identity, chunk_size, valid_through }`, appended whenever a run starts or continues copying that file, before its next range.
     `valid_through` is the resume offset (0 for a fresh copy). A FILE_BEGIN RESETS that file's reconstructed range set: ranges from earlier records are kept only below `valid_through`, so a recopy after a changed source cannot mix
     old digests with new ones even though `attempt_id` is a constant. Ranges after the latest FILE_BEGIN (in generation, sequence order) are added to the set.
   - `CHECKPOINT_RANGE { file_id, attempt_id, range_start, range_end, digests: [32-byte digest per chunk in the range] }`. Adjacent completed chunks coalesce into one range (s173), but the PER-CHUNK digests are kept, so a
     mismatch can be located to the first bad chunk (s137). Cost: 64 digests x 32 bytes = 2 KiB of payload per 64 MiB range. Invariant: `range_start` is a multiple of `chunk_size`, `range_end` is a multiple of it or the file length,
     and the digest count is `ceil((range_end - range_start) / chunk_size)`; replay rejects a record that breaks this as middle-record corruption.
   - The WAL carries its own `wal_format` number, independent of the manifest's and the claim store's (s193). An unknown `wal_format` or checksum version is `WAL_FORMAT_UNSUPPORTED` (s191); nothing is parsed heuristically.
   `file_id` is the binary claim key encoding (the identity; the plan confirms it is stable across a resume); `destination` in FILE_BEGIN is only the relative path as a display string, for reports and diagnostics.
5. **Flush policy (s148.3, s171). [amended by plan 1 ruling 6]** The WRITER THREAD owns the flush trigger, not the copy side. The original premise (under Strict the data sync must come before the WAL append, so the copy side must decide)
   no longer holds: panel round 2 moved that sync onto the writer thread (decision 6), and a thread-side timer also flushes while the copy thread is blocked in a slow source read, which a copy-side trigger cannot. The thread flushes when 64 MiB of completed ranges are pending, when 1 s has elapsed since the FIRST
   pending record (an idle writer never touches the file: a flush with nothing pending creates no `wal/`, no segment and syncs nothing), and on `Flush`, `Drain`, `Stop` and file completion (`Close`). The copy side may still send `Flush` at file completion. Both constants are pinned by one test.
6. **Durability ordering, by mode (s164, s165). [Ruling 1 needed for Normal.]**
   - **Strict:** at a flush, the copy side sends the range message and a flush command; the writer thread syncs the partial's data through a DUPLICATE of the partial's write handle, then appends, syncs the WAL, and acknowledges. "Duplicate" means an OS-level duplicate
     (`dup` on POSIX, `DuplicateHandle` on Windows), not a Rust clone of the wrapper. The duplicate is registered with the writer ONCE per checkpointed file (a register message sent with FILE_BEGIN, never one per flush, so the channel holds no handles)
     and closed by the writer on that file's close message, on a stop, or when the channel closes. Each registered duplicate counts as one open descriptor against the 9e open-writer cap (`OPEN_WRITERS_MAX`, `tree.rs:355`, lowered to a quarter
     of `RLIMIT_NOFILE`), so at most one extra descriptor per checkpointed file in flight. EVERY exit of a checkpointed file (success, any failure, a stop) sends the close message, using a send that keeps beating the heartbeat; if the channel has closed, the writer's drop closes the duplicate. A test aborts a file on a source error and asserts the descriptor count returns to its baseline. If `sync_all` on the duplicate FAILS, that is a destination-data failure, NOT a WAL failure: nothing is appended for that flush (data-before-checkpoint, s164),
     the writer flags the failure, and the copy thread aborts that file at its next buffer boundary with `StrictDurabilityUnavailable` at `CopyStep::Durability` and discards the partial exactly as today's Strict sync failure does; any earlier records
     for it are harmless, because a missing partial discards its checkpoints (decision 7.3). The copy
     thread keeps copying and keeps beating the heartbeat, and does not wait for the acknowledgement except at file completion, a drain request and a stop. Ranges in the message cover only chunks written before the flush was issued.
     (`sync_all` is per file, not per handle, on POSIX; on Windows `FlushFileBuffers` needs write access on the duplicate; on macOS `F_FULLFSYNC` is per descriptor. The plan verifies each with the `verifying-platform-assumptions`
     skill; a platform where the duplicate is unsound falls back to an inline sync with a smaller flush threshold, documented there.) A FaultFs call-log test pins the PROGRAM order data sync, WAL append, WAL sync; it proves ordering of calls, not durability
     (the fake does not reorder; the real-filesystem crash tests do). This sync is separate from, and in addition to, the 9e end-of-file barrier. A full channel behind a slow sync blocks the copy thread, which keeps beating while it retries (decision 2).
     Under Strict, the flush that CREATES `wal/000001` also makes its directory entries durable: after the segment's own `sync_all`, it syncs the `wal/` directory handle and the workspace handle, before the flush is acknowledged; a failure there is a WAL failure (plan 1 ruling 22). Later flushes and a reopened segment need none; Normal never syncs directories.
   - **Normal:** no data fsync and no WAL fsync; the writer appends and writes through to the OS. A Normal checkpoint is an ADVISORY hint, never a durable fact. It is trusted only after the validation in decision 7.
     The design says so in the report text and the docs; it does not call Normal checkpoints durable.
7. **Resume (s23, s35, s137). [Ruling 2 needed for the failure disposition.]** Order against cut 9c: commit recovery (`run/recover.rs`, over the prepared notes of a Strict tree publication) runs FIRST. A file with a prepared note is a
   COMPLETE staged temporary, and 9c decides whether it is published or rolled back; 9f-1 considers only partials with no prepared note, and the plan verifies that nothing 9c does to a temporary can leave a WAL range that validation would
   trust (a missing or shortened partial already discards the file's checkpoints, step 3 below). One path is real and covered here: 9c's `NotRenamed` and `Gone` arms discard only the prepared note and leave the temporary (`recover.rs:284`),
   so a later run can find a COMPLETE temporary whose whole range validates. It is then not published directly: with the resume offset equal to the file length the copy loop reads nothing, and the file continues through the normal tail of the
   pipeline (metadata, the Strict sync, a fresh prepared note, the publication). Under Strict that sync covers the reused bytes too, because `sync_all` is per file. Validation runs lazily, per file, at the moment that file would be staged, never in one up-front pass, and it calls the heartbeat as it
   hashes (a terabyte of reads with the lease unrefreshed would let the lock look stale). Replay is two-pass so that memory does not grow with the operation: pass 1 validates framing, checksums and sequence and builds an index from `file_id` to the offsets of that file's records (a few bytes per record; a 500 GiB file is
   about 8,000 records); pass 2 loads one file's digests when that file is about to be staged and drops them when it is done, re-reading from the WAL if the file is staged again. The bound is the digests of the single file in hand
   (32 bytes per MiB, about 16 MiB for 500 GiB) plus the index, never the digests of every file in the WAL (s188, V16:8304). Under `--resume`, after 9a adoption bumps the operation's WAL generation (s162; the manifest records it durably, which needs a manifest field
   and a format-version decision the plan makes against the 9a format rules): replay the WAL, then per partial file:
   1. Replay: scan sequentially, stop and truncate at the first torn trailing record (s160); corruption before the physical end is `WAL_CORRUPT` and stops the operation (s174); a repeated (operation, generation, sequence)
      with identical payload is idempotent, with a different payload it is `WAL_SEQUENCE_CONFLICT` (s175). On resume the writer starts its sequence after the highest valid one WITHIN the current generation; a new generation (every `--resume` bumps it) starts at 1 (V16 s161; plan 1 ruling 16). Opening with a generation below the segment's last is refused (`WAL_SEQUENCE_CONFLICT`, ruling 19).
   2. Source gate (cheap, first, s35.1): the source must still exist and match `FILE_BEGIN` on length, mtime and identity. If not, discard that file's checkpoints.
   3. Partial gate: the partial must exist and be at least as long as the highest checkpoint end it claims. Shorter: keep only the chunks that fit. Missing: discard that file's checkpoints.
   4. Chunk validation (s35.2): for each completed chunk in order, hash the SOURCE chunk and the DESTINATION chunk and compare both with the recorded digest. Stop at the first mismatch; the resume offset is the end of the
      last chunk that matched on both sides.
   5. Truncate the partial to the resume offset (`set_len`; bytes past it are never trusted, including bytes written after the last flush), reopen it WITHOUT truncating, seek, and continue copying from there. The 9a leftover
      sweep at `copy.rs:519` runs only when no validated checkpoint set exists for the target.
   If nothing validates, the file recopies from byte 0, exactly as today.
   Plan 2 obligations from plan 1 (rulings 17 and 20): `wal::open` consumes a directory handle the caller obtains FOR it, so plan 2 opens a SECOND handle to the operation workspace (`operations.open_dir(<id>)`) and keeps its own for reading the segment back (`open_dir("wal")?.open_read("000001")`); and a segment whose identity is `Unavailable` cannot be reopened (`open` answers `WalError::Io` kind `Unsupported` before touching it), which plan 2 reports as "checkpoints unavailable on this filesystem: recopy", never as an abort.
8. **Heartbeat and lock.** The copy loop never blocks on the WAL except on a full channel, and then it retries with a timeout and calls the heartbeat on each retry, so the lease stays alive (`TODO.md` heartbeat note: a chunk must
   complete within 25 s). WAL work runs off the main thread, as 9e's data syncs do. The WAL belongs to the run that holds the destination lock (s185).
9. **Stops. [follows the 9e rulings]** After a lost lock or a heartbeat failure no NEW append or flush begins; pending messages are dropped (progress is lost, never invented). An append already in flight may complete or tear; replay
   handles both (s160), so the claim is "nothing begins", not "nothing lands". Abandonment and stop differ (plan 1 ruling 11): the channel disconnecting WITHOUT a `Stop` is abandonment, so the writer closes its duplicates and exits WITHOUT flushing; `Stop` and `Drain` flush first. On an orderly stop or a drain request
   the writer flushes what is pending, in the ordering of decision 6, then returns. A test pins both.
10. **Small files. [my call]** A file shorter than 64 MiB (one flush threshold) writes no checkpoints at all: recopying it costs about a second, and 10,000 small files would otherwise flood the WAL. The constant is pinned by a test.
    Completed files need no record; the claim table already says they are done (`resumed_complete`, `tree.rs:1085`).
11. **WAL lifecycle in 9f-1. [my call; deviates from the negotiated "purge on publication"]** An append-only log cannot drop one file's records without compaction (9f-3). So the WAL lives until the operation workspace
    is removed at successful completion; replay ignores records for files whose claim is already `Created`. Size is dominated by the digests: 32 bytes per MiB of a checkpointed file (about 0.003% of the bytes copied; arithmetic from the spec constants, NOT a measurement) plus a small header per flush. The
    acceptance run RECORDS WAL bytes per GiB copied; if it is not small, compaction (9f-3) is promoted before anything else. s190's disk-full trigger for compaction does not exist yet: WAL space exhaustion falls under decision 13.
12. **Reporting.** `--resume` reports, per file, bytes reused and bytes recopied, and names every file whose checkpoints were discarded and why (source changed, partial missing, mismatch at offset N). Nothing is discarded silently.
13. **WAL failure (s189-s191). [my call]** If an append or a sync of the WAL fails (disk full, I/O error, permission), the run (1) stops the file in flight AT ONCE as a failure and KEEPS its partial, with the checkpoints that were durable before the failure (resume continues from the last good flush, so the loss is the window since it, not the file; this needs a
    keep-the-partial failure disposition, because today's `discard` removes the temporary, and the plan adds it), (2) starts no further file, (3) reports plainly that resume state is NOT trustworthy for the affected files, and (4) ends with an error using a
    new taxonomy code (`WAL_WRITE_FAILED`; the plan registers it and its exit code with the existing codes). Files already published are `Created` claims, so the next `--resume` continues with the remaining ones. Letting the file finish unprotected was
    considered and rejected: for a multi-hundred-GiB file it runs for hours with no crash protection and then still exits with an error. It never continues claiming resumable progress. The plan picks the existing stop class this reuses, and a FaultFs test injects the failure at the second append and pins the kept partial, the checkpoints that survive, and the exit code. The emergency persistence path of s231 is NOT built in 9f-1 (stated as a non-goal above). The next `--resume` reuses the kept partial up to the last durable checkpoint; if the WAL cannot be read at all, it recopies the affected files from byte 0, as today.
14. **Integrity and exposure (s192).** The WAL is created and opened handle-relative from the operation workspace's directory handle, like every other workspace file, and follows the workspace's permission policy. Records hold
    hashes and metadata, never source bytes. The WAL is not a security boundary: a forged record can at worst make validation reject a chunk, because decision 7.4 recomputes both sides from the real bytes.
15. **Platform behaviour designed for, not assumed.** After a crash a partial's length may include regions whose content never reached disk (zero or stale). Digest validation handles this by design; the real-filesystem crash tests
    (the `TODO.md` crash-injection debt item) are what prove it per platform, and the plan names which parts CI can run on Linux, macOS and Windows.

## Interfaces this cut needs (verified missing at main f27574e)

`FileHandle` is `Write` plus `sync_all` and `identity` only (`crates/flux-fs/src/fs.rs:102-107`), and `DirHandle` creates a partial with `create_new` only (`fs.rs:264`); there is no way to open an existing partial for writing, truncate it, or position it.
9f-1 therefore adds, in `flux-fs`, with an implementation for the real filesystems and a model in `FaultFs` (the plan counts the touch points; none is measured here): (1) open an EXISTING child of a directory handle for writing without
truncation, handle-relative like `create_new`, with the Windows share flags the existing publication and cleanup paths need, and with the containment contract of `create_new` (`fs.rs:258-264`): it judges the NAME, never its target, so it
must not follow a symlink or junction planted at the partial's name between the crash and the resume (non-following open flags, then a check that the handle is a regular file whose `FileIdentity` equals the one the gate saw); a planted link is refused
as a safety rejection, and a test plants one. Ruling 7 of plan 1, Task 3 fixes the signature and refusals: `DirHandle::open_existing(&self, name, expected: &FileIdentity) -> Result<Self::Writer>`, no truncation, positioned at 0, the returned handle's `identity()` equal to `expected`. A name-surrogate is `SafetyRejected`; a directory is `DestinationError` kind `IsADirectory`; another non-regular file is `DestinationError`; an identity that differs from `expected`, and an `expected` of `FileIdentity::Unavailable` (checked before any syscall, since it proves nothing), are `DestinationError` kind `InvalidData`; a missing name is `IoError` kind `NotFound`. On Windows a junction at the name is answered `STATUS_FILE_IS_A_DIRECTORY` (`DestinationError`, `IsADirectory`) before the reparse-tag check, because the open uses `FILE_NON_DIRECTORY_FILE`; (2) `truncate_to(len)`, which cuts the file to `len` and positions the next write there, and refuses to extend (ruling 2 of plan 1, Task 1: `set_len` and positioning are one method, so a caller cannot truncate without positioning); (3) an OS-level duplicate of a write handle usable for `sync_all` on another thread (decision 6);
(4) `FileSystem::Writer` and `DirHandle::Writer` are `Send` as well as `Sync` (ruling 5 of plan 1, Task 1: the duplicate and the writer move to other threads); (5) `DirHandle::open_read(&self, name) -> Result<Self::Reader>`, with `type Reader: std::io::Read` (ruling 4 of plan 1, Task 2): a handle-relative, streaming, non-following read of a child, for chunk validation of the destination partial, which `DirHandle::read_file` cannot do (it returns a `Vec<u8>` capped by `limit`) and `FileSystem::open_read` does not do by handle (it is path-based, `fs.rs:127`). It keeps `read_file`'s refusals and non-following open flags: a name-surrogate `SafetyRejected`, a directory `DestinationError` kind `IsADirectory`, another non-regular file `DestinationError`, a missing name `IoError` kind `NotFound`. On Windows a junction at the name is answered `STATUS_FILE_IS_A_DIRECTORY` (`DestinationError`, `IsADirectory`) before the reparse-tag check, because the open uses `FILE_NON_DIRECTORY_FILE`. Each new method is covered by the conformance suite the existing methods use.

Plan 1 obligations on the interfaces above (rulings 17 and 20): `wal::writer::open(workspace: D, ..)` takes ownership of a workspace handle the caller opened for it (the trait has no `Clone`/`duplicate` for directories), and returns `WalError::Io` kind `Unsupported` for a segment with an `Unavailable` identity.

## Plan 1 rulings (ledger)

Plan 1 (`docs/superpowers/plans/2026-10-12-cut-9f1-primitives-and-wal.md`) fixed rulings 1-22, listed by number: 1 digests in `flux-core`; 2 `truncate_to` is one method; 3 no conformance suite; 4 `open_read`; 5 `Writer: Send`; 6 the writer thread owns the flush trigger (decision 5); 7 `open_existing` refusals; 8 record encoding; 9 duplicate detection; 10 sequential pass 2; 11 disconnect is abandonment (decision 9); 12 sender retry and heartbeat (decision 2); 13 `WalError`; 14 opaque `file_id` and `source_identity`; 15 torn-tail streaming; 16 sequences restart per generation (decision 7.1); 17 the second workspace handle (decision 7); 18 `MAX_PAYLOAD`; 19 lower generation refused; 20 `Unavailable` identity (decision 7); 21 `WalFault`; 22 Strict makes the new segment's directory entries durable (decision 6). Task 7 adds: a record whose encoded payload exceeds `MAX_PAYLOAD` (1 MiB), which for a Range is about 32,768 digests less the `file_id` length, is refused at flush and flags its file; dropping the sender without a delivered `Stop` abandons the run even for messages still queued, and a file whose data sync already failed gets no later records.

## Rulings (both ACCEPTED by the owner 2026-10-12, as written)

- **Ruling 1, Normal durability (decision 6).** V16 s164 says a checkpoint must not become durable before its data crosses "the configured durability boundary". Under Normal that boundary is a plain write, so a Normal
  checkpoint can reach disk before its data does. My reading: Normal checkpoints are advisory and become trusted only through digest validation (s138 is satisfied because no record is used as a claim before it is validated).
  The alternative is to fsync the data and the WAL at every flush in Normal too, which contradicts s165's "normal optimizes throughput".
- **Ruling 2, validation failure (decision 7).** V16 s35.3 says a failed validation is `RESUME_INVALID` and "must not silently continue", while s137 says to invalidate from the mismatch onward and resume from the earliest invalid region.
  My reading: a chunk mismatch is s137 (partial rollback, reported); a changed source or a missing partial discards that file's checkpoints and recopies it with a reported reason, not a hard `RESUME_INVALID`, because today's
  `--resume` already recopies and the owner-visible behaviour should not regress to an error. The alternative is a hard `RESUME_INVALID` for those two cases.

## Folded from the agy consult (verified against the files; ledger)

- FOLDED, reversing my first draft: checkpoint durability must NOT wait for the 9e barrier, which syncs completed files only (agy Axiom 1; verified at `copy.rs` step 6b).
- FOLDED: validate the source identity before reading tens of GiB (cheap gate first, decision 7.2); a resume that restarts sequence numbers at 0 would corrupt the log (decision 7.1); the `copy.rs:519` sweep must become conditional (decision 7.5);
  the 256 KiB buffers feed a 1 MiB chunk digest incrementally (decision 3); Normal checkpoints are advisory, not durable (decision 6).
- REJECTED: a sidecar checkpoint file per partial (contradicts s178's `wal/` and s195's "no per-worker WAL file"); one hash per 64 MiB range (loses the first-bad-chunk location s137 needs; the 2 KiB per range is cheap).
- NOT ADOPTED: agy's figures (5-15% hashing slowdown, 15 minutes to validate 45 GiB, hundreds of MB of WAL). They are unmeasured hypotheses; the acceptance run measures them.

## Folded from the adversarial panel, round 1 (verified; ledger)

- FOLDED, memory bound on replay (agy AB-1): two-pass replay with a per-file index, decision 7. FOLDED, the Strict data sync starved the heartbeat when inline (agy CA-1): the sync runs on the writer thread through a duplicate handle, decision 6.
- FOLDED, WAL failure was ambiguous (agy SC-1), WITH A DIFFERENT FIX than agy's: agy proposed aborting without publishing and leaving the partial; that discards good copied data while the lock is still held, so the in-flight file publishes and the run then stops, decision 13.
- FOLDED, missing `flux-fs` primitives (agy LI-1): the Interfaces section. FOLDED, 9c `NotRenamed` leaves a complete temporary (agy, direct question c; verified at `recover.rs:284`): decision 7. FOLDED, redundant `destination` and `file_id` (agy PP-1). FOLDED, the call-log test cannot see heartbeat starvation (agy MG-1): the deterministic slow-sync test.
- Panel round 2 (agy; new seats Resource Vampire, Boundary Smuggler, Blindspot Auditor and a fold auditor): FOLDED, buffered duplicate handles escaped the descriptor cap (registered once per file and counted against `OPEN_WRITERS_MAX`, decision 6); FOLDED, opening an existing partial must not follow a planted link (Interfaces (1)); FOLDED, a failed data sync on the duplicate is a copy failure, not a WAL failure (decision 6);
  FOLDED, REVERSING my round-1 disposition of agy's WAL-failure fix: aborting at once and keeping the partial loses only the window since the last good flush, while finishing unprotected risks the whole file (decision 13; I had rejected the fix on the wrong premise that abort discards progress). agy's section numbers for the name-surrogate rule did not match the spec (s210 is Chunk Digest Retention), so only the verified contract at `fs.rs:258-264` is cited.
- Panel round 3 (agy; seats Axiom Breaker, State Corruptor, a bespoke Platform Archaeologist, and a fold auditor on the five round-2 edits): FOLDED, decision 13 still ended with the round-1 sentence "recopies from byte 0" after the keep-the-partial reversal (my own leftover; fixed and the file grepped for other stale sentences);
  FOLDED (below the floor, cheap), the duplicate handle needs a close message on every exit path including aborts, with a descriptor-count test. State Corruptor: no new findings. The platform documentation quotes (dup, fsync, ftruncate, ValidDataLength) were NOT verified by me; they stay on the plan's `verifying-platform-assumptions` list. The code quotes at
  `dir_windows.rs:647` and `94-96` were checked; `dir_unix.rs:78` is a directory open, so it is precedent for the flags, not proof for files.
- Panel status: NOT GREEN. Three rounds, each folded; round 3 still had one live finding, and its fix (one sentence and one test) was not re-reviewed by agy. The owner may ask for a narrow round 4 or accept this state.
- Reply handling: the driver flagged `[13b] TRUNCATED REPLY` because the payload asked for a `[VERDICT: ...]` token while this discipline closes with a `PANEL VERDICT` line; the peer's reply file held the whole report, so nothing was missing and no re-ask was spent.

## Acceptance (the plan turns each into a named test or measurement)

Tests: every record type round-trips; a torn tail is truncated and the log replays; mid-log corruption is `WAL_CORRUPT`; a duplicate is idempotent and a conflicting duplicate is `WAL_SEQUENCE_CONFLICT`; sequence continues after resume; the Strict
ordering call log; flush thresholds; the small-file threshold; a full channel keeps the heartbeat going, and a deliberately slow data sync keeps it going too (deterministic: a FaultFs `on_nth` hook blocks the sync on a channel the test releases, and the test asserts the heartbeat count rose while it was blocked; no sleeps); a lost lock appends nothing; source changed, partial missing, partial shorter, garbage tail, and a mid-file mismatch each give the
documented offset and report line; the resumed output equals the source byte for byte AND the resume actually reused the checkpoints (a byte-equality check alone passes if resume silently recopies from 0): each resume test asserts the reported bytes reused and the
count of source bytes read in the resumed run (`file length - resume offset` plus the validation reads), and a test with deliberately wrong digests asserts the recopy; an uninterrupted copy's output is unchanged. Crash matrix from V16 s180 cases 1-7, 15 and 16 (cases 8-14 are snapshot, compaction and rotation: 9f-3).

Measurements (owner-gated, top level only, two runs, range quoted, background load stated as uncontrolled; the figures are the cut's own, nothing here predicts them): uninterrupted large-copy throughput with and without
checkpointing; the longest heartbeat gap under checkpointing; WAL bytes per GiB; resume validation latency for a partial of a stated size on this box.

## Exhaustiveness audit (done 2026-10-12, before handoff)

Requirement-to-section map: s136 chunk model (3), s137 validation (7.4), s138 (3, 6, Ruling 1), s139 and s164 ordering (6), s148 batching (5), s155-163 WAL (1, 2, 4, 7.1), s165 modes (6), s171-175 (5, 7.1), s185 ownership (8),
s195 anti-requirements (2, 5, 6; no per-chunk fsync, no per-chunk transaction, bounded queue, one WAL file), s23 partial pairing (4, 7), s35 (7.2-7.4).
Left open, each with where it is resolved: the byte layout and channel capacity (plan); `file_id` stability across resume (plan, verified against the claim key code); the manifest generation field and its format-version bump (plan, against the 9a
format rules); which crash tests run in CI per platform (plan); retries and `attempt_id` (a later cut); the `FILE_BEGIN` source-identity representation per platform (plan, with the `verifying-platform-assumptions` skill, since inode, file id and
mtime resolution differ on Linux, macOS and Windows); the exact `wal/` directory creation and its durability (plan).
Known conflict with the parent spec, stated rather than hidden: s164 under Normal (Ruling 1) and s35.3 (Ruling 2).

## Stand-downs

- DISCARDED-BELOW-FLOOR: a forged or tampered WAL tricking resume into trusting bad bytes, unreachable because decision 7.4 recomputes the digest of BOTH the source chunk and the destination chunk from the real bytes and compares each with the record; a forged record can only make a chunk fail validation (decision 14).
- DISCARDED-BELOW-FLOOR: a Dependency Cynic seat, unreachable because 9f-1 adds no dependency (`blake3` is already at `Cargo.toml:64`; `flux-hash` exists).
