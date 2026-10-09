# Cut 9c: recoverable publication under `--durability=strict`

Status: DRAFT for owner review (2026-10-09), branch `spec/cut-9c` off the merged cut 9b. The four scope decisions below were negotiated with agy
(consults `.clavity/seams/cut9c-{scope,forks,negotiate-r1}.md`, replies in `.clavity/scratch/cut9c-scope/`) and approved by the owner on 2026-10-09;
section "Writer's rulings" lists what the owner has not yet seen. Parent spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (sections 119, 156-163, 170-178,
180-184, 189, 193-194, 241.5, 249, 259.8, 55). Previous slices: `docs/superpowers/specs/2026-10-08-cut-9a-resume-design.md`,
`docs/superpowers/specs/2026-10-08-cut-9b-cleanup-design.md`.

## Why this cut, and why it is not "the WAL"

Cut 9a's slice table named 9c "the WAL, PREPARE_COMMIT/COMMIT, commit recovery", blocked on spec gaps G2-G5. Reading the code against the spec showed what the
slice is for:

- A file is published by a rename, and its claim (the record that makes `--resume` skip it) is written AFTER the rename (`tree.rs`, the new-target and
  replacement branches). A crash between the two leaves a published file with no `Created` claim. `--resume` then treats it as an existing destination
  file and the existing-file policy decides (`--overwrite` copies it again, `--update` compares, `--skip-existing` keeps it). That is safe and costs one
  re-copy; nothing is lost or corrupted.
- Under `--durability=strict` every claim commit is already synced (`flux-platform/src/claims.rs`, `Strict => Immediate`), so the only window is the
  instant between the rename and the claim commit. Under `Normal` claims are batched and synced every 1000 commits or at a directory end; a `SIGKILL`
  loses every unsynced batched commit (measured on 2026-10-09: `a_killed_run_leaves_the_claims_up_to_the_last_sync` copied to a scratch test, 300 files in
  3 directories, killed inside the second directory: exactly the first directory's 100 claims survived; redb documents `Durability::None` as "will not be
  persisted ... unless followed by a commit with `Durability::Immediate`", `transactions.rs:379`).
- So a per-file commit record is only worth writing where it is itself durable before the rename: under Strict. Under Normal it would be lost by the same
  kill that loses the claims, and syncing it per file is the cost Normal exists to avoid.
- An append-only log (`wal/` segments, the section 159 record format, a single writer, torn-tail truncation, compaction) earns its cost only for a
  high-rate stream such as chunk checkpoints (section 148, every 64 MiB or 1 s). That is cut 9d. A per-file commit already fits an ACID key-value store,
  and section 182 allows it: "The exact optimization may collapse records, but recovery semantics must remain equivalent."

So 9c is: a V16 spec amendment that resolves G2-G5 for the commit protocol, a durable `Prepared` note in the claim store written before the rename and turned
into the claim in the same transaction after it, and commit recovery at `--resume`. The practical gain is small and is stated as such: under Strict, a
crash inside the rename-to-claim window no longer costs a re-copy and is decided from evidence instead of by policy; the real value is spec conformance
and a base for 9d.

## Scope

IN:
1. The V16 amendment (appendix "Amendment text"): the commit record of a Strict tree publication lives in the claim store; G2, G3, G4, G5 resolved; the
   exit-code registry; the claim-store format.
2. Claim-store format 2: a `prepared` table written in the same redb transaction as the claim row (`prepare`, `commit_prepared`, `discard_prepared`,
   `prepared`); format-1 stores stay readable.
3. Tree publications under Strict: a `Prepared` note durable before the rename, flipped to the claim after it.
4. Commit recovery at `--resume` adoption, before the walk plans anything: classify every note, then apply, or refuse with `COMMIT_STATE_UNCERTAIN`.
5. `COMMIT_STATE_UNCERTAIN` as a refusal (exit 3), the resume report line, tests (crash-point matrix), `TODO.md` limits.

OUT (stay in TODO.md): `wal/` segments, the record format, a writer, compaction, snapshots and chunk checkpoints (9d); single-file operations (the
amendment states their rule, the code does not implement it); `--durability=normal` (unchanged from 9a); hardlink groups (not built); `flux verify`.

## Decisions

Owner-approved (2026-10-09):

1. **Which runs (F1).** Only `--durability=strict`. Under Normal nothing changes from 9a: no note, no extra commit, the existing-file policy reconciles.
2. **Which operations (F2).** Directory (tree) operations only. The amendment states the same rule for single-file operations (a `commit` field in the
   adjacent record) so a later slice implements it without a new decision.
3. **Storage (F3).** The claim store `state.db`: `meta.format` becomes 2 and a table `prepared` holds the notes; a note is written and removed in the same
   redb transaction as the claim row it relates to. An older binary opens a format-2 store and refuses it with `INCOMPATIBLE_STATE` (the 9a mechanism).
4. **Ambiguity (F6).** A note that recovery cannot decide refuses the whole adoption before any mutation: `COMMIT_STATE_UNCERTAIN`, exit 3, naming the
   file and the evidence; the way out is `--restart` or fixing the destination by hand.

Derived from the above and the spec (the matrix and the failure rule were agreed with agy; see the consult replies):

5. **The note** is one row of `prepared`, keyed like the claim it will become (the parent directory's `FileIdentity` and the entry's stored name):
   `PreparedRecord { target: FluxPathKey, temp_name: bytes, temp_identity: FileIdentity, dir_path: native_hex of the entry's directory relative to DEST
   ("" for DEST itself), name: bytes (the stored name), planned_name: bytes (the name the plan used when the filesystem stored it under another
   spelling, empty when equal to `name`), replacement: bool }`. `temp_identity` is `copy_file_guarded`'s `published_identity`, read from the
   open writer handle immediately before the rename (`copy.rs`, "a rename keeps it").
   **Encoding (version 1):** one version byte `1`, then in this order `target`, `temp_name`, `identity`, `dir_path`, `name`, `planned_name`, each as a
   4-byte big-endian length followed by that many bytes (`identity` is the ASCII `identity_text` of `state.rs`: `strong:<volume>:<index>`,
   `weak:<volume>:<index>` or `unavailable`; `dir_path` is the lowercase hex of `native_hex`), then one byte `replacement` (0 or 1). A record that does not
   decode, has trailing bytes, or carries a version other than 1 is UNCERTAIN. **Validation in phase 1 before any filesystem access:** `name` and
   `planned_name` (when not empty) are single normal path components; `temp_name` equals `<planned_name or, when that is empty, name>.flux-partial.<this operation's id>` (derived: `copy_file_guarded` names the temporary
   from the name the plan used, `copy.rs` `temp_name(name, ..)`, so a hostile row cannot name another file and a case-folding replacement validates);
   `dir_path` is the empty string (the entry is directly under DEST) or decodes with `from_native_hex` to a relative path of normal components
   (`from_native_hex("")` is `None`, so the empty string is handled first); and once the directory is resolved, its
   `FileIdentity` equals the row key's `parent` (when either is not `Strong` the row is UNCERTAIN). Any failure is UNCERTAIN.
6. **Order of a Strict publication:** copy, verify, metadata, flush (as today); the final recheck of the source (as today); NEW `prepare` (one
   synced commit) through a hook called once, after that recheck and BEFORE the final heartbeat and section 99 guard, so the ownership check still
   immediately precedes the rename and the synced commit does not widen its window (a lost lock then leaves a note whose temporary was never renamed:
   recovery row 2); the heartbeat and the guard (as today); the rename; NEW `commit_prepared` (one synced commit: the claim becomes `Created`,
   the note is deleted). A failing `prepare` aborts the publication: the temporary is discarded as for any failure before the rename and the file fails
   with `CopyStep::Claim` (no new code). A failing `commit_prepared` after a successful rename is the existing `ClaimNotRecorded` report (the file IS
   published); the note stays and recovery settles it on the next resume. **Any other failure after a successful `prepare` and before the rename** (the
   heartbeat, the guard, the rename itself failing, a lost lock) makes `tree.rs` call `discard_prepared` for that key at once, best effort: the copy path
   has already removed (or kept as a leftover) its temporary, and a note left behind would describe a publication that did not happen. A `discard_prepared`
   that itself fails leaves the note, which recovery decides by the matrix (row 4 below).
7. **Recovery order:** `Place::adopt` opens the store, then runs recovery, then returns; the walk starts afterwards (section 241.5: "before any other target
   is planned or published").

## Recovery at `--resume`

Recovery runs on every adoption of a format-2 store whose `prepared` table is not empty, whatever the resume's `--durability` (9a already refuses a Strict operation resumed under Normal, `resume.rs`, so notes and a Normal resume cannot meet). Phase 1 (read-only) classifies every row of `prepared`. For a row: open the entry's directory component by component from DEST through link-refusing
handles (the `artifacts::remove_validated` pattern; a component that is missing or is not a directory makes both T and D absent), then read T, the
state of `temp_name` there (`Absent`, or `Present(identity)` when it is a regular file), and D, the state of the entry `name` (`Absent`, or `Entry(identity)`;
never following a link). With R the recorded `temp_identity`:

| # | Evidence | Verdict | Action in phase 2 |
|---|---|---|---|
| 1 | D is `Entry(i)`, R and `i` both `Strong`, and `i == R` | RENAMED | `commit_prepared(key, target, planned)` (the row's `planned_name` becomes the second `Created` claim); then, if T is `Present(j)` with `j == R` (the `link()`+`unlink()` no-replace fallback of section 241.5 leaves both names), remove the temp name; a failed removal is a warning (`PartialKept` shape: the object IS published, the stray name is the J1 sweep's) and never fails the adoption |
| 2 | T is `Present` and D is `Absent` | NOT RENAMED | `discard_prepared`; the normal path redoes the file (its step-1 sweep removes the temporary: the name is reserved to this operation, so T's identity is not needed) |
| 3 | T is `Present` and the row is a replacement (`replacement == true`; `rename_replace` is atomic and never leaves two names) | NOT RENAMED | as row 2 (D is the old object, whatever its identity) |
| 3b | T is `Present(j)` with R and `j` `Strong` and `j == R`, and D is `Entry(i)` with `i` `Strong` and `i != R` (a no-replace row whose entry was taken by another object) | NOT RENAMED | as row 2 |
| 4 | T is `Absent` and either D is `Absent`, or D is `Entry(i)` with R and `i` both `Strong` and `i != R` | GONE | `discard_prepared`; no object of the operation's exists at either name (an aborted publication whose note outlived its temporary, or our object replaced afterwards by someone else), so the normal path decides by the existing-file policy exactly as in 9a |
| 5 | anything else: a `Weak` or `Unavailable` identity where a comparison needs a `Strong` one (R or D); T present with D present for a no-replace row when the identities cannot show `i != R`; a row that fails validation; an unreadable directory | UNCERTAIN | none |

Rows are evaluated in order and the first that matches decides (so a RENAMED row 1 is never read as row 3).

Row 1 needs strong identity on both sides (section 259.8: existence, size or timestamp alone never prove the rename). Rows 2, 3 and 4 (the `D absent` form) need no identity: a rename moves the name, so
`T present and D absent` cannot follow a completed rename, a replacement never leaves two names, and with neither name present there is no object of ours
to protect. Row 4's second form needs both identities `Strong`: if R is the recorded object and D is a different object while T is gone, the object this
operation made is not at either name, so redo is the 9a behaviour (the policy may replace D, as it would without any note).

Phase 2 runs only when no row is UNCERTAIN. It applies every verdict in ONE redb transaction (synced) so recovery is all-or-nothing. If any row is
UNCERTAIN, nothing is applied and adoption is refused: `COMMIT_STATE_UNCERTAIN`, exit 3, `changed: false` (the refusal changes nothing, as the exit-3
rule requires), detail `<path>: cannot tell whether this file was published (<evidence>); the operation's state is preserved. Move or delete the entry you do not want so it reads as absent (the next
resume then redoes the file), or run again with --restart to supersede this operation`, naming up to ten uncertain targets (each with the evidence that kept it undecided: `identity unavailable`, `destination holds another object`, `unreadable directory`, `invalid note`) and the total count, so the operator sees the whole set at once. `--restart` supersedes the
operation (ABANDONED, partials swept by id, workspace removed) and never touches the object in doubt.

A RENAMED file's claim is now `Created`; the walk finds it, passes the size and mtime check (9a) and skips it as resumed. The report gets one extra line
after the resume note: `recovered <k> interrupted publications` when `k > 0` (RENAMED rows only).

## Formats and compatibility

- `FORMAT` of the claim store becomes 2; `open_file` accepts 1 and 2. New stores are created at 2 (under both durabilities: the table exists, Normal
  never writes to it). An older binary refuses a format-2 store (`INCOMPATIBLE_STATE`, exit 3) before it looks at `--restart`: known limit 2, the same as
  9a's manifest format 3.
- A format-1 store (a 9a or 9b operation) is adopted as before and is NOT upgraded in place: `supports_prepared()` is false, a Strict run on it writes no
  notes and behaves as 9a. (Upgrading would make that operation unresumable by the binary that created it.)
- The manifest and the section 249 records are unchanged. The tree manifest stays at format 3.

## Units

- `flux-fs/src/claims.rs`: `PreparedRecord` (encode/decode, one version byte, then the fields in decision 5's order, each length-prefixed); `ClaimStore`
  gains `fn supports_prepared(&self) -> bool`, `fn prepare(&mut self, key: &ClaimKey, record: &PreparedRecord) -> Result<()>` (inserts the note; a note
  already at `key` is `Code::IoError`), `fn commit_prepared(&mut self, key: &ClaimKey, target: &FluxPathKey, planned: Option<&ClaimKey>) -> Result<()>` (one transaction: the
  claim at `key` becomes `Created` for this target, inserted if absent and upgraded if `Existing` of the same target; when `planned` is given, a
  `Created` claim for it is inserted too (a replacement the filesystem stored under another spelling, as `tree.rs` records today); the note is
  deleted; a foreign claim is an error and nothing changes), `fn discard_prepared(&mut self, key: &ClaimKey) -> Result<()>` (idempotent: a key with no note is `Ok`), `fn prepared(&self) -> Result<Vec<(ClaimKey, PreparedRecord)>>`;
  conformance cases for each (including atomicity: a failed `commit_prepared` leaves both note and claim as they were).
- `flux-platform/src/claims.rs`: format 2, the `prepared` table (`TableDefinition<&[u8], &[u8]>::new("prepared")`: key `ClaimKey::encode()`, value `PreparedRecord::encode()`), the methods above with `Strict`/`Normal` commit modes (the `prepare` and
  `commit_prepared` commits are ALWAYS `Immediate`; they are only called under Strict), format-1 reading.
- `flux-core/src/fault_fs.rs`: the fake store implements the same methods with fault keys (`prepare`, `commit_prepared`, `discard_prepared`).
- `flux-core/src/copy.rs`: `PublishIntent { temp: OsString, identity: FileIdentity }` and a `BeforePublish` hook called exactly once per copy that reaches the
  rename, after the source recheck (the identity is read from the writer handle there) and before the final heartbeat and guard; its error stops the
  copy there with the temporary discarded (`CopyStep::Claim`).
  `pub type BeforePublish<'g> = dyn Fn(&PublishIntent) -> std::result::Result<(), CopyError> + 'g;` and `no_before_publish` is the default. The hook arrives beside `before_create` (the plan decides whether that is a ninth parameter or a bundle).
- `flux-core/src/tree.rs`: under `opts.durability == Strict` and `claims.supports_prepared()`, the new-target and replacement branches pass a hook that
  calls `prepare`, replace the post-rename `insert_if_absent`/`upgrade_own_claim` (and the second claim for a planned name) by one `commit_prepared`, and
  call `discard_prepared` when the copy fails after `prepare`.
- `flux-core/src/run/recover.rs` (new): `recover_publications(dest, store, durability) -> Result<Recovered, RecoverError>` with phase 1 and phase 2.
- `flux-core/src/run/place.rs` / `mod.rs`: `TreePlace::adopt` calls it after `count()`; `ResumeNote::Adopted` gains `recovered: u64`; `LockCode` gains
  `CommitStateUncertain` ("COMMIT_STATE_UNCERTAIN"), mapped to exit 3 (`flux-cli/src/exit_code.rs` is unchanged: a refusal that changed nothing is 3).
- `flux-cli/src/report.rs`: the `recovered` line.

## Known limits (added to TODO.md when this lands)

1. Under `--durability=normal` nothing is recoverable beyond 9a: a kill loses up to 1000 batched claims and their files are re-copied by the existing-file
   policy.
2. A format-2 store cannot be resumed by a binary older than this cut (`INCOMPATIBLE_STATE`; `--restart` or the newer binary).
3. A 9a or 9b operation resumed under 9c behaves as 9a (format 1, no notes).
4. Strict pays one extra synced commit per published file (two instead of one).
5. A note whose directory was renamed, replaced by a link or removed while the operation was down is UNCERTAIN or GONE by the matrix; the operator resolves it.
6. Single-file operations have no commit recovery yet (the rule is in the amendment); the J1 sweep and the existing-file policy still apply to them.
7. The walk is sequential today, so no other target can claim an entry between its `prepare` and its `commit_prepared`; a parallel walk must claim the
   entry at `prepare` time (insert-if-absent), which the row's key already allows.
8. `--restart` and `flux cleanup` never run commit recovery: they supersede or remove the operation, sweep its partials by id and never touch the
   object a note leaves in doubt.
9. The parent directory of an UNCERTAIN note may be on a case-insensitive filesystem under another spelling: the directory is opened by the recorded
   spelling, so a different case reads as absent and the verdict is GONE or UNCERTAIN, never RENAMED.

## Writer's rulings (owner: confirm or change at spec review)

W1. **T absent with D absent, or with D a different Strong object, is GONE (redo), not UNCERTAIN.** agy's matrix said any unmatched destination is
    UNCERTAIN; but with no object of ours at either name nothing is preserved or overwritten by anything beyond what the 9a policy already does, and
    leaving it UNCERTAIN would wedge an operation after a benign crash between the temporary's removal and the note's removal (panel round 1).
W2. **Classify all, then apply all in one transaction, refuse changing nothing.** So the exit-3 rule ("nothing was changed") holds even when some notes
    were decidable.
W3. **A format-1 store is not upgraded in place**, and a Strict run on it writes no notes.
W4. **`link()`+`unlink()` leftover temp name is removed in recovery** only when its identity equals the recorded one.
W5. **A failing `commit_prepared` after a successful rename is the existing `ClaimNotRecorded` report**, not a new failure: the file is published and the
    note settles it at the next resume.
W6. **`COMMIT_STATE_UNCERTAIN` is a `LockCode` refusal** (like `INCOMPATIBLE_STATE`), exit 3, and appears in the exit-3 examples of section 55.
W7. **Replacement notes never leave two names** (row 3), because `rename_replace` is atomic; the `link()`+`unlink()` ambiguity exists only for no-replace
    publications.
W8. **A failed publication after `prepare` discards its note in `tree.rs`** (best effort), so the table stays small and the matrix is the backstop.
W9. **The record encoding (version 1) and the validation list** are this spec's (panel round 1: the plan would otherwise guess them).
W10. **Spec gaps found:** G2 to G5 and the 259.8 wording "may finalize the commit" (the amendment says recovery MUST finalize when row 1 holds).

## Tests (Review Focus; each is pinned by a named test in the plan)

1. The recovery matrix, one test per row and a distractor for each neighbour (R weak, D weak, T present with another identity, D a different object).
2. The crash-point matrix of section 180 for a Strict publication: after the guard and before `prepare`; after `prepare` and before the rename (T present,
   D absent or the old object); after the rename and before `commit_prepared` (D holds R); after `commit_prepared`; each with the `link()`+`unlink()`
   fallback (both names present). Fault injection on the fake and real-redb end-to-end runs with hand-built states.
3. All-or-nothing: two notes, one RENAMED and one UNCERTAIN: nothing is applied, the refusal is exit 3 and the store is byte-identical afterwards.
4. Normal is unchanged: the same crash under Normal writes no note and resumes exactly as in 9a (a pinned test of the call log: no `prepare` call).
5. `prepare` failing under Strict: the file fails before the rename, nothing is published, no note remains.
6. Format: an older reader refuses a format-2 store; a format-2 store with an empty `prepared` table resumes as 9a; a format-1 store is adopted without notes.
7. Atomicity of `commit_prepared`: a foreign claim at the key changes nothing; a crash inside the transaction leaves note and claim as they were.
8. A hostile note: `dir_path` naming a path through a link, an absolute path, `..`; the verdict is UNCERTAIN and nothing is touched.
9. The report line, the exit code and the refusal text.
10. No wedge: a rename (or guard) failing after `prepare` leaves no note; a note whose temporary is gone while D is the old Strong object is GONE and the
    file is redone; a replacement note with T present and a Weak D is NOT RENAMED; a no-replace note with T present and a Weak D is UNCERTAIN (the link
    fallback); a failed removal of the stray temp name in row 1 is a warning and the adoption succeeds.
11. A hostile note (unknown version, trailing bytes, a `temp_name` that is not `<name>.flux-partial.<id>`, a `dir_path` whose resolved identity differs from
    the key's parent): UNCERTAIN, nothing touched.

## Appendix: Amendment text for `FLUX_FULL_UPDATED_SPEC_V16.md`

A. Section 182, after the ordering block, add: "**182.1 Collapsed commit record.** A tree publication under `--durability=strict` records PREPARE_COMMIT and
   COMMIT as one `prepared` row in the operation's claim store (`state.db`): the row is written and synced before the rename and is replaced by the
   target's `Created` claim in the same transaction after it. Recovery semantics are those of Section 183. Under `--durability=normal` no commit record
   is written and Section 183 does not apply: the existing-file policy reconciles a published but unclaimed file. A single-file operation follows the same
   rule with a `commit` field in its adjacent state record. An append-only WAL file is required only for the record kinds that need it (chunk
   checkpoints, Section 158)."
B. Section 241.5, the sentence "A target's created-entry claim is written in the same durable transaction as its `COMMIT` record" becomes: "...as its
   commit record (Section 182.1), so a committed target always has its claim." The sentence on recovery keeps its meaning with "PREPARE_COMMIT without
   COMMIT" read as "a `prepared` row".
C. Section 183: add: "Recovery classifies every `prepared` row first and applies the outcomes only if none is undecidable; it finalizes (writes the claim,
   removes the row) exactly when the evidence of Section 259.8 establishes the rename, discards the row when it establishes that no rename happened or
   that no object of the operation exists at either name, and otherwise refuses with COMMIT_STATE_UNCERTAIN."
D. Section 259.8: "recovery may finalize the commit" becomes "recovery finalizes the commit".
E. Section 55: add `COMMIT_STATE_UNCERTAIN` to the exit-3 examples ("refused before changing anything").
F. Section 119/178: the `wal/` directory is created only by the slice that writes a WAL file (chunk checkpoints); a workspace without it is valid.
G. Section 193: the claim store carries its own format number (`meta.format`), independent of the manifest's.
H. G2: the commit record and the claim are rows of one redb file, so "the same durable transaction" is literal. G3: the publications that get a commit
   record are the tree publications of a `--durability=strict` run. G4: a crashed publication leaves its `prepared` row; an `Existing` claim of a
   replacement stays blocking, as 241.5 says ("never released"). G5: the record is the row of decision 5; the WAL record-type list (Section 224) is not a
   closed list for this cut and stays "conceptual".
