# Copy-side verification (cut 13, roadmap phase 6) - design

Status: DRAFT for owner review, 2026-10-26. Branch `spec/verify-copy` (off `origin/main` 6900ef4).
Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (cited as "master §N" with line numbers at that revision).

## 1. Intent

`flux copy` reports `verify_level: "none"` and `files_verified: 0` today (`crates/flux-cli/src/report.rs:110-119`), and
nothing reads a written file back. A copy that lands on failing storage reports success. This cut adds the
`--verify` levels of master §32 to the copy engine, so an operator can ask Flux to prove, before a file is published,
that the bytes on the destination equal the bytes read from the source.

This is the first of three slices agreed with the owner after a consult with the agy peer (both positions converged):
cut 13 = in-engine copy verification (this doc); cut 14 = `flux verify SOURCE DEST` (master §4.2); cut 15 = making
`--resume` and `--verify` work together. Reflink verification (master §39) is not planned until reflinks exist.

## 2. Goals, non-goals, anti-goals

Goals
- `--verify[=none|source-stream|destination|full]` (bare `--verify` = `destination`) and `--hash=blake3` on `flux copy`,
  master lines 708-715.
- A whole-file BLAKE3 digest of the source bytes as they are streamed (master §134).
- `destination`: re-read the temporary file anew, hash it, compare with the digest, all BEFORE publication (§135).
- `full`: `destination`, plus a second independent read of the source compared with the stream digest (§32).
- A mismatch fails that file at once; the tree continues; the run exits 1 (owner ruling 2026-10-26, section 4).
- Real values in the report: `verify_level`, `hash_algorithm`, `files_verified`, `files_mismatched`.
- Help text and docs that state exactly what each level guarantees (master §32: "The CLI must document exactly what
  guarantee each level provides").

Non-goals (this cut)
- `flux verify SOURCE DEST` (cut 14). `--resume` with `--verify` (cut 15). Reflink verification (phase 9).
- Changing the default level. Benchmarks. A worker thread for hashing (section 5).
- Comparing symlink payloads or directories: copy verification covers regular file content only.

Anti-goals (a failure even if it "works")
- A verified result that rests on a buffer kept from writing (master §135).
- A resumed file reported verified when only its tail was compared.
- A mismatch that leaves bytes at the destination NAME, or a temporary that is not discarded.
- Any cost for a run that does not ask for verification beyond a level check per file.

## 3. Constraints (verified against the code at 6900ef4)

- Every file copy, single-file and tree, goes through `stage_file` (`crates/flux-core/src/copy.rs:717`); callers at
  `tree.rs:3547` and `copy.rs:1068, 1321, 1420, 1634`. The verification belongs in `stage_file`, once.
- `stage_file` order today: leftover sweep, `plan_file`, open source, exclusive create, `plan_resume`, stream loop
  (`copy.rs:854-878`), metadata (899-932), durability sync when `SyncAt::Staging` (938-950), source recheck by length and
  mtime only (955-976), staged. Master §134 puts destination verification after the source digest and before metadata.
- `discard(parent, &temp, step, code, source, guard)` removes the temporary through the directory handle and never
  touches the destination name (used by every failure above). A failed guard leaves the temporary reported as `leftover`.
- `DirHandle::open_read(&self, name)` (`crates/flux-fs/src/fs.rs:484`) opens a child regular file by name through the
  parent handle, never following a link: this is the "opens the written file anew" primitive.
- Under `--durability=strict` a tree batches files: `SyncAt::Barrier` defers the data sync to the batch flush
  (`tree.rs:3547-3570`); `Staging` syncs inside `stage_file`.
- `crates/flux-hash` is a scaffold (9 lines, no dependency). `blake3` 1.8 is a workspace dependency already used by
  `flux-core` (`checkpoint/decide.rs:68`, per-1-MiB chunk digests for resume). Those chunk digests are not
  composable into a whole-file digest (agy; not independently measured, and not needed for this design).
- `report.rs` already carries `files_verified`, `files_mismatched`, `verify_level`, `hash_algorithm` (hardcoded).
- Exit codes: 1 for "verification found a mismatch" (master §55); `VERIFY_MISMATCH` is in the registry (master line 2956)
  but not in `flux_fs::Code`.

## 4. Decisions the owner made

- A mismatch is a failed file: fail at once, discard the temporary, continue the tree, exit 1. No automatic recopy.
  Rationale given: a recopy would hide flaky hardware.
- Approach 1 (section 5): hashing and the re-read run inline on the copy thread.
- Scope as in section 1, with the default level kept at `none` and `--resume` combined with `--verify` refused in this
  cut (approved 2026-10-26).

## 5. Approaches considered

1. Inline (chosen). `hasher.update` right after each read in the existing loop; the `destination`/`full` reads happen in
   `stage_file` after the loop, before metadata. No new threads. Costs hashing time on the copy thread; the re-read may
   be served by the page cache.
2. A hashing worker like the cut 9h checkpoint sink. Overlaps hashing with I/O; needs buffer hand-off, a second thread per
   file and coordination with the checkpoint hasher. An optimization with no measurement behind it.
3. Verify after the durability sync at the batch barrier. Closer to what is on disk, but departs from master §134's order,
   holds every temporary longer, complicates discarding after a sync, and the page cache can still serve the read.

Option 2 can replace 1 later without changing the CLI, the report or the failure semantics.

## 6. Behaviour

### 6.1 Levels

| level | during the stream | after the stream (before metadata) | `files_verified` |
|---|---|---|---|
| `none` (default, call C1) | nothing | nothing | no |
| `source-stream` | hash source bytes | nothing (digest computed, not compared) | no (master line 2753) |
| `destination` | hash source bytes | re-read temp via `open_read`, hash, compare | yes |
| `full` | hash source bytes | `destination`, then re-read the source via `fs.open_read(src)`, hash, compare | yes |

### 6.2 Steps added to `stage_file` (after the stream loop ends and the sink completes; before step 6)

1. Finalize the source digest `S`.
1b. `destination`/`full` only (call C13): the master §134 "source mutation validation" BEFORE the re-read: `fs.metadata(src)`
   compared with `src_meta` by length and mtime, same comparison and same `SourceChanged` discard as step 7, so a source
   that already changed does not cost a full read-back. Step 7's recheck stays where it is: it also covers the metadata and
   sync window.
2. `destination`/`full`: `parent.open_read(&temp)`, read to EOF in the same buffer size, hash, compare with `S`. Also
   compare byte counts. A short or long read is a mismatch.
3. `full`: open the source anew, read to EOF, hash, compare with `S`.
4. Any mismatch: `discard(parent, &temp, CopyStep::Verify, Code::VerifyMismatch, <error naming which side>, guard)`.
   The error text names the side that differed ("destination bytes differ from the source stream" / "source bytes differ
   between the copy and the second read"). A read error during step 2 or 3 is NOT a mismatch: it discards with the read's
   own code (`IoError` etc.) and `CopyStep::Verify`.
5. The heartbeat (`beat`) runs per chunk of the re-reads, as the stream loop does, and a failed beat discards as the
   stream loop does.
6. Metadata, durability, the existing length/mtime recheck and the publish are unchanged and follow.

The temporary's writer stays open during the re-read (it is needed for metadata and, under `SyncAt::Barrier`, for the
batch sync). `open_read` opens a second, independent handle by name.

### 6.3 Interactions

| situation | behaviour |
|---|---|
| fresh single-file copy and tree copy | verified per file as above |
| a hardlink member that is LINKED (cut 12) | no bytes are streamed; it shares the anchor's object, which was verified when copied; not counted in `files_verified`; counted `files_hardlinked` as before |
| a member copied independently (degraded hardlink) | an ordinary copy: verified |
| file skipped by `--skip-existing`/`--update` | not read, not verified, not counted |
| overwritten file | the NEW temporary is verified before it replaces the old one |
| empty file | digest of zero bytes; the re-read must also yield zero bytes; counted verified |
| symlinks, directories, skipped special files | not content-verified, not counted (special files never reach `stage_file`) |
| `--durability=strict`, any `SyncAt` | verification runs at staging, before the sync/barrier; see 6.5 |
| `--dry-run` | nothing is read or written; `verify_level` reports the requested level; `files_verified` stays 0 |
| `--resume` with `--verify=destination\|full` or bare `--verify` | usage error, exit 2, message below, nothing changed |
| `--resume` with `--verify=source-stream` | call C4 |
| `--restart` with `--verify` | allowed: a restart supersedes prior state and copies fresh |
| a `--verify` run that crashed, then `--resume` WITHOUT `--verify` | allowed: resume behaves as today; the report says `none` |

Refusal text (author's wording, builder may polish): `error: --resume cannot be combined with --verify in this version;
verify a resumed copy afterwards, or use --restart`. Placed beside the existing `--resume-verify=metadata` refusal
(`crates/flux-cli/src/main.rs` near line 311), before any state is touched.

### 6.4 Counters and report

- `files_mismatched`: files whose digest comparison failed (destination or `full` source read). A subset of
  `files_failed` (owner: a mismatch is a failed file); `errors` counts it once as any failed file.
- `files_verified`: files that passed a `destination` or `full` check AND were published. Counted at publish, not at the
  comparison, so `files_verified <= files_copied` and a file that verified but failed to publish is not verified. The rule
  is per entry: `files_verified` increments exactly where `files_copied` increments for an entry whose `Outcome.verified`
  is true. A Barrier sync failure affects only the entries whose own sync result failed (`flush_batch` takes per-entry
  results, `tree.rs:3640`), so only those are un-counted.
- `verify_level`: `"none"`, `"source-stream"`, `"destination"`, `"full"`. `hash_algorithm`: `"blake3"` (already).
- Failure line for a mismatch uses the existing per-failure rendering with code `VERIFY_MISMATCH` and the error text of
  6.2 step 4. The summary line gains a suffix when `files_mismatched > 0` in the style of the cut 12 suffixes; absent at
  zero. JSON fields exist already; no new field.
- Exit code: a tree run with any mismatch exits 1 through the existing failed-file path; `VerifyMismatch` needs no special
  mapping in `exit_code.rs` (it is a path-scoped failure like `IoError`; the builder confirms with a test).

### 6.5 What each level guarantees (this text goes into `--help` and the docs)

- `none`: no content check.
- `source-stream`: the source bytes are hashed while copied. Nothing is compared in this version.
- `destination`: after writing, the file is read again through the filesystem and compared with the source bytes that were
  read. It detects a destination that returns different bytes than were written. It does NOT prove the bytes reached stable
  storage: the read may be served by the operating system's cache. `--durability=strict` still governs when data is synced,
  and the sync comes after the verification.
- `full`: `destination`, plus the source is read a second time and compared; this also detects source bytes that changed or
  were misread during the copy. The existing length/mtime recheck still runs.

## 7. Calls made by the author (for the owner to check)

- C1. Default stays `none`, a deviation from master §32 (default `source-stream`). Reason: `source-stream` alone compares
  nothing and never raises `files_verified`, so a default would charge CPU for no visible assurance. Flip after a
  benchmark. To be recorded in `docs/accepted-boundaries.md` or the ROADMAP row, builder's choice.
- C2. `full`'s second-read mismatch uses `VERIFY_MISMATCH`, NOT `SOURCE_CHANGED`. The agy peer proposed `SOURCE_CHANGED`;
  I differ: `SOURCE_CHANGED` is defined as a file whose length or mtime moved (`flux-fs/src/error.rs:25-28`), the
  registry says `VERIFY_MISMATCH` is "a verification digest did not match" (master line 2956), and a misread source
  moves neither.
- C3. A read error during a re-read discards with the read's own code, not `VERIFY_MISMATCH`.
- C4. `--resume` with `--verify=source-stream`: REFUSED too, same message. agy would refuse it; I first leaned to allow it
  with a "not whole-file hashed" note. Refusing is simpler, and `source-stream` verifies nothing observable, so refusal
  costs nothing. The owner may override.
- C5. `files_verified` counts at publish (6.4).
- C6. `--dry-run` reports the requested level without reading anything.
- C7. No manifest field: a verify run cannot be resumed with `--verify`, and `--resume` ignores the earlier level.
- C8. Hash the bytes as read (before `write_all`), per master §134 order.
- C9. Library callers (`copy_file_guarded` with a checkpoint hook) that pass a verify level together with a partial that
  `plan_resume` would resume: `stage_file` returns an error with `CopyStep::Verify` and `Code::CopyFailed`, text
  "verification of a resumed file is not supported", before touching the partial. Unreachable from the CLI; it keeps the
  engine honest. (Builder: confirm `plan_resume`'s verdict is available before the partial is reopened, `copy.rs:783-796`.)
- C10. New `CopyStep::Verify`, new `Code::VerifyMismatch` ("VERIFY_MISMATCH", exit 1). Registry entry already exists.
- C11. The `full` second read opens the source BY PATH with `fs.open_read(src)`, as the first read did (`copy.rs:762`);
  master §32 requires an independent read, so the first handle is not reused or rewound. A source swapped for another
  object between the reads therefore surfaces as a digest mismatch on the source side (and the step 1b/7 rechecks catch a
  length or mtime change first).
- C12. The hasher is `Option<Hasher>` (or equivalent) inside the stream loop; one predictable branch per chunk is accepted
  under `none`. No attempt is made to share the cut 9h checkpoint worker's chunk reads with the new hasher.
- C13. The cheap source recheck also runs before the re-read (6.2 step 1b), per master §134's order. Found by the agy panel.
- C14. Verified facts the builder asked about: the `plan_resume` verdict exists before the partial is reopened
  (`copy.rs:775-796`), so C9's guard is an early return after the verdict, before `open_existing`; a failed file exits 1
  through the existing paths (`exit_code.rs:15, 24-38`: errors default to `FAILED`); `CopyOptions` is not serialized
  (`options.rs` has no serde), so adding fields touches only struct literals; the destination `Writer` is `StdFile` with no
  user-space buffer (`dir_unix.rs:132`), so a second handle sees every written byte, and the real-filesystem test pins it.
- C15. If reflinks are built later, that cut decides how `--verify` applies to a cloned file (master §39, line 2423); this
  cut neither refuses nor special-cases it.

## 8. Interfaces (names and shapes; bodies are the builder's)

- `flux-hash`: `Hasher::new()`, `update(&mut self, &[u8])`, `finalize(&self) -> Digest`; `Digest` is 32 bytes with
  `as_bytes()`, `to_hex()`, `PartialEq`; `hash_reader<R: Read>(&mut R, buf) -> io::Result<(Digest, u64)>` returning the
  byte count. Depends on `blake3 = { workspace = true }`.
- `flux-fs/src/options.rs`: `enum VerifyLevel { None, SourceStream, Destination, Full }` (`Default` = `None`),
  `enum HashAlgorithm { Blake3 }`; both fields added to `CopyOptions` (every struct literal of `CopyOptions` changes:
  the compiler names them; search repo-wide, tests and benches included).
- `flux-fs/src/error.rs`: `Code::VerifyMismatch` with `as_str() == "VERIFY_MISMATCH"` and a pinning test beside the others.
- `flux-core/src/copy.rs`: `CopyStep::Verify`; the steps of 6.2 inside `stage_file`; `Staged` carries `verified: bool`
  (true only when a `destination`/`full` comparison passed) and `publish_staged` copies it into the returned
  `flux_fs::Outcome` (new field `pub verified: bool`, `options.rs:122-138`), because `finish_copy` and `Report::file` see
  only `Outcome`, never `Staged`. Every `Outcome` literal gains the field (compiler-named; search repo-wide).
- `flux-core/src/tree.rs`: the publish path adds `verified` files to a new `TreeOutcome.files_verified`, and a failure whose
  code is `VerifyMismatch` to `files_mismatched` (still counted failed).
- `flux-cli/src/main.rs`: `--verify` (`num_args = 0..=1`, `default_missing_value = "destination"`), `--hash` (only
  `blake3`; other values are a usage error, exit 2 per master lines 711-714), the refusal of 6.3.
- `flux-cli/src/report.rs`: the four fields get real values; the summary suffix; per-failure rendering.

## 9. Tests (the oracle for each is the spec line or this section; none may be edited to match the code)

Each new test must be shown non-vacuous by a logic mutant of the guarded line (flip the comparison, skip the re-read),
with the specific new test the one that goes red. `FaultFs` is the harness (it can corrupt a read or a write by key).

- hasher: known BLAKE3 vectors (empty, "abc"-class, multi-chunk); `hash_reader` byte count; incremental == one-shot.
- `destination` clean copy: published, `files_verified == 1`, digest compared (the fake logs an `open_read` of the temp).
- `destination` corrupted write (fake flips a byte on the temp's re-read): file NOT published, no bytes at the name, temp
  removed, `VERIFY_MISMATCH`, `files_mismatched == 1`, `files_failed == 1`, the tree's other files published, exit 1.
- `destination` mismatch over an EXISTING destination file: the old file is untouched.
- `destination` short re-read and long re-read (byte-count mismatch): each a mismatch.
- `destination` re-read I/O error: discarded with the read's code, `files_mismatched == 0`.
- `full`: source changed between the stream and the second read (the fake alters the second read): mismatch, discarded.
- `full` clean: two source reads logged; `source-stream`: no destination re-read logged; `none`: no hashing at all (no
  extra `open_read` calls; the call log equals the pre-cut log for the same copy).
- Strict durability, both `SyncAt::Staging` and `Barrier`: verification precedes the sync; a mismatch discards before any
  barrier entry exists for it; a tree batch with one bad file still publishes the others.
- hardlink pair: the anchor verified once, the linked member not counted; a degraded (independently copied) member is verified.
- counters: `files_verified` counted at publish (a publish failure after a good comparison does not count).
- guard/heartbeat failure during a re-read: temporary reported as leftover or removed exactly as the stream loop's failures.
- CLI: `--verify` bare == destination; `--verify=bogus` exit 2; `--hash=sha256` exit 2; `--resume --verify` (each level
  and bare) exit 2, nothing created or changed at the destination; `--restart --verify` works; `--dry-run --verify` reads
  nothing and reports the level; JSON field values for every level; summary suffix present at mismatch, absent at zero.
- real-filesystem test (std fs): a tree copy with `--verify=full` passes and reports `files_verified == N`.
- `Code::VerifyMismatch` string pin.

## 10. Left to the builder

The module layout of the re-read helper (shared by `destination` and `full`), the buffer size for re-reads (reuse
`copy_buffer_len`), exact summary-suffix and refusal wording, whether the `source-stream` digest is returned in `Staged`
for later cuts (it is not needed now), docs placement (`--help` text is required; the site/README CLI pages if they list
flags), and the ROADMAP row update. Commit messages are conventional (release-plz generates the changelog).

## 11. Spec deviations and boundaries this cut records

- Default level `none` instead of `source-stream` (C1). Resume with verify refused (cut 15 lifts it). `source-stream`
  compares nothing. A page-cache re-read is not proof of stable storage (6.5): proposed for `docs/accepted-boundaries.md`
  with the compensation "the level text in `--help`; durability is governed by `--durability`".

## 12. Exhaustiveness audit (author, before handoff)

Closed in this document: levels x behaviour (6.1), failure dispositions (mismatch, read error, short/long read,
guard/heartbeat), strict durability both sync positions, hardlink members, skip/overwrite/empty/symlink/directory, dry-run,
resume and restart interplay, counters and their relationship (`files_mismatched <= files_failed`, `files_verified <=
files_copied`), exit codes, flag grammar, API shapes, test list with oracle and non-vacuity rule.

Reviews: builder check (8 questions; 4 answered from the code and folded as C14, 1 as C13/C11, the rest are plan items);
agy panel round 1 `PANEL VERDICT: GREEN` with 3 minors, all folded (`Outcome.verified`, step 1b, special files).

Remaining, and where each resolves: wording of the suffix/refusal (plan); `--hash` help text for the single value
(plan); the benchmark that justifies flipping the default (a later cut, needs owner approval to measure); the cut 15 design
of resumed-file verification (its own brainstorm).
