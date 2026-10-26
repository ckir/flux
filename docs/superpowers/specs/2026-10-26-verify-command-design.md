# The `flux verify` command (cut 14, roadmap phase 6) - design

Status: owner-adopted calls (section 4, item 4), 2026-10-26; implementation plan `docs/superpowers/plans/2026-10-26-verify-command.md` (its refinements R1-R7 are folded here where they touch a shape). Branch `spec/verify-command` (off `origin/main` f19db29).
Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` (cited as "master §N" or "master line N" at that revision).

## 1. Intent

Cut 13 proves a file correct while it is copied. Nothing yet answers the question an operator asks afterwards:
"is this finished copy still what the source says?" (after a power cut, a transfer to another disk, a month on a
shelf). `flux verify SOURCE DEST` answers it without changing anything: it compares the two trees, reads the bytes on
both sides, and reports every path as `match`, `missing`, `mismatched`, `extra` or `unreadable` (master §4.2,
lines 649-690; items 53 and 76).

This is the second of the three slices (cut 13 copy-side verification, shipped in PR #129; cut 14 this doc; cut 15
`--resume` with `--verify`).

## 2. Goals, non-goals, anti-goals

Goals
- `flux verify SOURCE DEST` for ONE source: file to file, file into a folder, folder to folder, with the destination
  mapping of master §4.1 (the same `resolve::job` the copy uses).
- Compare each selected source file by a BLAKE3 digest of the bytes on both sides (master line 660; `--hash=blake3`).
  Symlinks compare their payload, directories compare presence, metadata is not compared (lines 661-662).
- Walk both sides together in `FluxPathKey` order (master §7.2, lines 888-891) with no whole-tree buffering
  (line 3578), and report each record as it is found.
- Never compare Flux's own control state (line 665-667).
- Exit 0 when nothing is missing, mismatched or unreadable; 1 when something is; 2 for a usage error (lines 681-684).
  `extra` paths are reported and do not change the exit status.
- Take no lock and create no workspace (line 686).
- Supported selection options: `--links=copy|skip`, `--cross-filesystems`. Also `--hash`, `--json`.

Non-goals (deferred; each is a tracked follow-up)
- `--exclude` and a `--recursive` toggle: not built anywhere (ROADMAP phase 3). Verify gets them when copy does, through
  the shared walker (section 5).
- Several sources (`flux verify S1 S2 DEST`): cut 14b. The walker side is ready for it; the report and the mapping are
  not designed.
- Metadata comparison, hardlink topology (see section 6.8), ACL / xattr comparison.
- Parallel hashing: the walk and the hash are sequential, like the copy.
- Verifying against `.flux` claim records (agy's option D): `flux verify` must work on any two directories.

Anti-goals (a pass that would make the command wrong)
- Reporting `match` for a path whose bytes were not read and compared.
- Opening a FIFO, socket or device for reading (it can block the run forever).
- Writing anything: no temporary file, no state, no lock, no directory creation (a DEST that does not exist stays
  absent).
- Hiding an ordinary user file because its name resembles a control artifact.

## 3. Constraints (verified against the code at f19db29)

- The walker (`crates/flux-core/src/walk.rs`) yields `WalkEvent::{Dir, File, Symlink, Other, DirEnd, ...}` in sorted order
  (it sorts each directory's entries by `name.as_encoded_bytes()`, `walk.rs:161-163`), carries a depth cap
  (`DEFAULT_MAX_DEPTH = 256`, `walk.rs:21`), a cycle guard (the `ancestors` set), a mount policy
  (`MountPolicy`, `walk_with`), and treats an `Err` item as NOT terminal: it skips that subtree and continues
  (`walk.rs`, the `Iterator` doc). A missing root or a root that is not a directory is a `Result::Err` at construction.
- Its memory is, per side, the not-yet-yielded entries of every directory on the current path (one `Frame` per level
  holds an `IntoIter<DirEntry>`). That is bounded by the sum of the directory sizes along one path, never by the tree.
  Master line 3578 ("no full-tree buffering") holds; the earlier "O(depth)" wording does not, and nothing here says it.
- `flux_hash::hash_reader` hashes a `Read` to a `Digest` and a byte count (`flux-hash/src/lib.rs:54`). `Code::VerifyMismatch`
  exists (cut 13) and maps to exit 1; `Code::SpecialFileUnsupported` exists.
- The CLI prints, for `copy`, one stderr line per streamed record and, with `--json`, ONE JSON object on stdout at
  the end (`report.rs`, `main.rs`). Exit codes: `exit_code.rs` (0 success, 1 failed, 2 usage, 3 refused).
- `flux verify` is already listed in the `Commands` section of the README but is not a subcommand (`main.rs:30-51`
  has `Copy` and `Cleanup`).
- The walker does no case folding. On a case-insensitive destination a name differing from the source only in case is
  a different name to the walker.

## 4. Decisions the owner made (2026-10-26)

1. Scope: one source and DEST, BLAKE3, the five outcomes, `--links`, `--cross-filesystems`, `--hash`, `--json`; no
   `--exclude`, no several sources (agy consulted first: ALIGNED; my reading agreed; one line of the brief's axis was
   rejected by agy: the right sequencing is the streaming invariant first, then user value).
2. Mechanism: approach A, two walkers merged in lockstep (section 5). Rejected: B, a second recursive traversal (copies the
   cycle guard, depth cap and mount policy, doubles every future selection change); C, two passes with path probes
   (breaks master line 663-664 "walked together", can report one path both missing and extra when DEST changes
   between passes, and a probe on a case-insensitive destination can report `match` for a name that differs).
3. Records are streamed as found, not collected and sorted (section 6.6).
4. The author's calls V1-V11, V5 and the open items were negotiated with agy in two rounds (briefs
   `.clavity/seams/verify-design-negotiate.md` and `-r2.md`, local) and the common proposal was adopted by the owner on
   2026-10-26. Changes it made: V5 gains `--verbose`; V11 gains a `not_entered` count and notice lines; V3 gains a
   leading stderr line; the root pairs that are refused are fixed (6.1, V12); `--verbose` is V13.

## 5. Approach

Two `Walk` iterators, one over SOURCE and one over DEST, are merged by a cursor that compares the relative paths of
their next events. Both iterators already emit in `FluxPathKey` order, so one pass over each, advancing whichever is
behind, visits every path of either side exactly once.

The comparison key is `flux_fs::claims::FluxPathKey::from_relative(rel)` (`claims.rs:18-55`: the Normal components'
encoded bytes joined by 0x00), compared as bytes. That equals the COMPONENT-WISE order (master lines 888-891) and
avoids the trap of comparing the joined display string: the walker orders `a` before `a.txt`, and `a/b` (inside
directory `a`) before `a.txt`, while a bytewise compare of `a/b` and `a.txt` puts `a.txt` first because `.` (0x2e)
sorts before `/` (0x2f). A path with a NUL byte makes `from_relative` fail with `DestinationError`; that path is
reported `unreadable` (it cannot be keyed) and its subtree skipped.

Rejected alternatives are in section 4.

## 6. Behaviour

### 6.1 Mapping and the roots

`resolve::job(source, dest)` decides the pair exactly as for `copy` (master §4.1): a folder SOURCE compares against
the folder DEST; a file SOURCE compares against DEST, or against `DEST/<name>` when DEST is an existing folder or ends
with a separator. Its refusals are reused unchanged: a folder SOURCE against an existing FILE DEST is a usage error
(exit 2); a missing SOURCE is a failure (exit 1, one stderr line); a SYMLINK SOURCE keeps the K7 outcome copy gives it
(never followed), with the same exit code. `Stop::*` maps to the same codes as in `copy`.

DEST absent (folder SOURCE): treated as an empty destination. Every source path is `missing`, the run exits 1, and
nothing is created. Before the walk starts one stderr line is printed, `destination does not exist: <DEST> (every
source path is reported missing)`, so a mistyped DEST is not mistaken for a wiped one (V3). DEST present and a file with a file SOURCE: the single pair is compared. DEST a folder with a
file SOURCE resolves to `DEST/<name>`; if that is absent the one record is `missing`.

Refused roots, for a folder verify, BEFORE any read: exit 2 with a usage message and no records, when the resolved
DEST is the resolved SOURCE (an alias, a bind mount or a symlink to it included), when DEST lies inside SOURCE, or when
SOURCE lies inside DEST. Master lines 2478-2492 reject `source == destination` and a destination nested in the source
and "favor rejection rather than complicated self-exclusion"; the third case is beyond that text: `verify /data/project
/data` pairs `project/file` with an extra `project/` subtree and reports a flood of `missing` and `extra`. The check
reuses the copy engine's containment test (`tree.rs` `containment`, ~4351-4380, which resolves both roots) in both
directions; where the identity of either root cannot be confirmed (the engine's "degraded" case) the lexical test is
used and the doc says so in the message. Exit 2, not copy's exit 3, because verify never mutates and its exit codes are
0, 1, 2 (master lines 681-684) (V12). A FILE verify is not subject to this check.

A root that cannot be listed (SOURCE or DEST folder unreadable) is one `unreadable` record with path `.` (the root), the
error text, and exit 1.

### 6.2 The merge, per pair of next events

The cursor reads ENTRY events from each walker: `Dir`, `File`, `Symlink`, `Other` (`walk.rs:24-38`). The other events
are not entries:

- `DirEnd` is DISCARDED by the adapter that feeds the cursor. Both walkers emit in one total preorder (master lines
  888-891), so ordering is carried by the keys alone and `DirEnd` carries none. Merging by key alone is enough: every
  descendant of a path sorts before the next sibling, so the children of a `missing` or `extra` directory, and of a
  type-mismatched path (6.3), drain through the first two bullets below without any "until its `DirEnd`" rule.
- `MountSkipped(p)` / `MountUnknown(p)` follow their own `Dir(p)` immediately (`walk.rs:277-278`). The adapter folds
  them into that `Dir` event as a flag, `not_entered` for `MountSkipped`; they never produce a record of their own.
  The flag matters only when `Dir(p)` is paired with a `Dir(p)` on the other side: then `p` is recorded `match` ONCE
  (detail "mount point not entered" if either side's flag is set), and every entry of the OTHER side under `p` is
  skipped (a key-prefix test, no record) so a side that listed below a mount point the other did not enter is not
  reported as `missing`/`extra` (V11). If `p` is on one side only, or the pair is not Dir/Dir, the flag is ignored
  and 6.2/6.3 decide. `MountUnknown` is ignored (the directory was listed). A pruned mount point is never silent
  (6.6): it increments `not_entered` and prints a notice line.

Let `s` and `d` be the next ENTRY events and `ks`, `kd` their `FluxPathKey`s (section 5).

- `ks < kd`, or `d` exhausted: record `missing` for `s`. Advance the source.
- `ks > kd`, or `s` exhausted: record `extra` for `d`. Advance DEST. (Control state, 6.4, is dropped before this point.)
- `ks == kd`: compare as in 6.3, record once, advance both. Whatever follows in each stream (the children of a
  directory on one side only, or of a type-mismatched path) is merged by the same rules.

A `WalkError` (an `Err` item) on either side is `unreadable` for that path and the cursor drops the entries under it
on BOTH sides by the same key-prefix test (V7); the walker has already skipped its own side's subtree.

### 6.3 Comparing an equal path

| source | dest | result |
|---|---|---|
| Dir | Dir | `match` (presence only) |
| File | File | hash both (6.5): equal digests `match`, else `mismatched` |
| Symlink | Symlink | read both payloads; equal `match`, else `mismatched` |
| Other | Other | `unreadable` (not compared; see below) |
| any pair of different types | | `mismatched`, VERIFY_MISMATCH; neither side is opened |

Type difference with a subtree: source `a` is a File, DEST `a` is a Dir. The path `a` is `mismatched`; DEST's children
are then `extra` (they follow `a` in the stream). Source `a` a Dir, DEST `a` a File: `a` is `mismatched`; the source's
children are `missing`.

`Other` (FIFO, socket, device, anything the walker does not call a file, directory or symlink) is never opened.
`flux_fs::FileType` has only `File`, `Dir`, `Symlink`, `Other` (`fs.rs:36-41`), so two `Other` entries cannot be
told apart (a FIFO and a socket look the same); calling them `match` would be a false assurance. `Other` against
`Other` is therefore `unreadable` with detail "special file not compared"; `Other` against a different type is
`mismatched`; on one side only it is `missing` or `extra` as usual. A copy never produces an `Other` at the
destination (it fails with `SPECIAL_FILE_UNSUPPORTED`), so the `unreadable` outcome points the operator at a path
verify cannot vouch for. This is a call (V6).

An error reading a payload or a file (open, read, hash) on either side makes that path `unreadable` with the error's code
and text; the other side's outcome is not guessed. A `WalkError` from the source or DEST walker is recorded as
`unreadable` for that path and its subtree is skipped on both sides (the cursor drops the paired subtree: it
does not report the other side's children as `missing`/`extra`, because it could not see this side's).

### 6.4 Control state (master line 665-667)

The DEST walk output is filtered before the merge. Dropped, exactly: the top-level directory `.flux` of the destination
root with everything under it, the file `.flux-root.lock` at the destination root, and any file whose name is a Flux
partial artifact name (the syntax the copy engine writes: `<name>.flux-partial.<id>`, builder confirms in `names.rs`).
A user file named `.flux-partial.txt`, `x.flux-partial` or `.flux` deeper in the tree is NOT dropped. The source side is
never filtered. A SOURCE that itself contains a `.flux` directory compares it like any other directory. Whether a
source `.flux` should have been copied is the copy's business, not verify's; if the destination lacks it the record is
`missing`.

### 6.5 Hashing a pair

Each side is opened and hashed in one pass with `hash_reader` (one buffer of the copy's maximum, `COPY_BUF_MAX` = 256 KiB,
allocated once per run and reused; plan R6). The file is read
twice in total (once per side), sequentially: source first, then destination. The digests are compared. A size
shortcut (different `len` implies mismatch without reading) is NOT taken: the point of the command is to read the bytes,
and a size taken from `lstat` can be stale (V8). `bytes_hashed` counts the bytes read on both sides.

### 6.6 Records and output

A record is `{path, outcome, kind, code, detail}`: `path` relative to the walk root (`.` for the root record), `outcome` one
of the five, `kind` `file|dir|symlink|other` of the SOURCE side when it exists, else of the DEST side, `code` (the `Code` name, e.g. `VERIFY_MISMATCH` for a `mismatched` record, the error's code for `unreadable`, else
null), and `detail`
(the error text for `unreadable`, the two kinds for a type mismatch, the two digests' first 8 hex characters
for a file mismatch, empty otherwise). Records are emitted AS the cursor decides them, in path order; nothing is
collected.

Human mode (stdout): one line per record whose outcome is not `match`, then a summary line to stdout. A match is not
printed by default (a million-file tree would print a million lines); the summary counts them. `--verbose` also prints
one line per `match` record, which is also the only liveness signal of a long run (V5, V13). Each pruned mount point
prints a notice line on stderr, `mount point not entered: <path> (use --cross-filesystems to verify its contents)`, with or
without `--verbose`, and the summary line carries the `not_entered` count.

`--json` (stdout): newline-delimited JSON. One object per record (including `match`), then one final object
`{"summary": {...}}` with `source`, `dest`, `hash`, `matched`, `missing`, `mismatched`, `extra`, `unreadable`,
`not_entered`, `bytes_hashed`, `exit`. `not_entered` counts pruned mount points; each of them is also a `match` record
whose `detail` is "mount point not entered". NDJSON is a deliberate difference from `copy --json` (one object at the end):
a streamed report on a large tree cannot be a single array without holding it (V4).

Stderr carries only operational lines (the usage error, a root failure, the exit-2 text, the absent-DEST line, the
mount notices), not the records.

### 6.7 Options

- `--links=copy|skip`: `copy` (default) compares symlinks as in 6.3. `skip` removes SOURCE symlinks from the selection:
  a source symlink is not compared and not reported; a DEST symlink with no source counterpart is still `extra`.
  This mirrors how `copy --links=skip` selects.
- `--cross-filesystems`: the same `MountPolicy` is given to both walkers. A mount point met on either side prunes
  that subtree on both (6.2); `--help` says verify does not look inside mount points unless this flag is given.
- `--verbose`: human mode also prints `match` records (6.6). It has no effect on `--json`, which always prints them.
- `--hash=blake3` (the only value, as in `copy`). `--json`. Everything else `copy` takes (overwrite modes, durability,
  safety, resume, restart, dry-run, hardlinks, workers, verify) is not accepted by `verify` and is a clap usage error
  (exit 2).

### 6.8 What a pass guarantees (this text goes into `--help` and the README)

- `match` means the bytes the operating system returned for both files, at the moment each was read, hash equal. The
  destination read may be served from the page cache, so it proves the destination returns those bytes, not that they
  are on stable storage (the same boundary as `copy --verify=destination`).
- Hardlinks are not compared. Two names of one source file copied as two independent files still `match`. Use `copy
  --hardlinks` to preserve them.
- `extra` paths are reported but do not change the exit status. A script that needs an exact replica must read the
  summary (`extra` is a field of it).
- The result is a snapshot with no lock; a destination another run is changing can give stale results.
- A name that differs only in case on a case-insensitive destination is a `missing` plus an `extra`, not a `match`. The
  same holds for two Unicode normalisations of one name (macOS NFD against NFC): names are compared as raw bytes.
- A mount point the walker did not enter (no `--cross-filesystems`) is reported `match` for its presence only; its
  contents were NOT compared. The summary's `not_entered` count and the notice line say so, and exit 0 does not mean
  those contents were verified.

## 7. Calls made by the author (for the owner to check)

- V1. The cursor lives in `flux-core` (`verify.rs`), the printing and exit codes in `flux-cli`. The core function takes
  two walkers (or two roots plus the `FileSystem`) and a `&mut dyn FnMut(Record)` sink and returns a `VerifySummary`.
  Cost if wrong: moving a module.
- V2. Hash source then destination sequentially (6.5), not interleaved. Cost: a slow second read on a cold disk.
- V3. DEST absent means "empty destination", exit 1, nothing created, with a leading stderr line (6.1).
- V4. `--json` is newline-delimited with a final summary object, not one object (6.6).
- V5. Human mode prints non-match records by default and every record with `--verbose` (owner-adopted, 2026-10-26).
- V6. `Other` is never opened; `Other` against `Other` is `unreadable`, not `match` (6.3).
- V7. A `WalkError` drops the paired subtree on both sides (6.2, 6.3).
- V8. No size shortcut (6.5).
- V9. The `kind` of a record is the SOURCE's when the source side exists.
- V10. Hardlink topology is not verified (6.8).
- V11. A mount point not entered on either side is pruned on both and reported `match` with a detail, not as a flood of
  `missing`/`extra` (6.2), AND counted in `not_entered` with a notice line (6.6) so exit 0 is never silent about it.
  Rejected: `unreadable` (fails a legitimate copy made without `--cross-filesystems`), a sixth outcome (breaks the
  closed list of master §4.2).
- V12. A folder verify refuses DEST == SOURCE, DEST inside SOURCE and SOURCE inside DEST with exit 2 (6.1); copy's
  exit 3 is not used because verify never mutates.
- V13. `--verbose` lists `match` records; there is no separate progress mode.

## 8. Interfaces (names and shapes; bodies are the builder's)

- `flux_core::verify`:
  - `pub enum Outcome { Match, Missing, Mismatched, Extra, Unreadable }`
  - `pub enum Kind { File, Dir, Symlink, Other }`
  - `pub struct Record { pub path: PathBuf, pub outcome: Outcome, pub kind: Kind, pub code: Option<Code>, pub detail: String }`
  - `pub struct Summary { pub matched, missing, mismatched, extra, unreadable, not_entered: u64, pub bytes_hashed: u64 }`
  - `pub fn verify_tree<F: DestinationRoot>(fs: &F, src: &Path, dst: &Path, opts: &VerifyOptions, sink: &mut dyn FnMut(Record)) -> Result<Summary, VerifyError>`
    (`DestinationRoot`, not `FileSystem`: the reused containment test takes a destination handle; plan R1)
  - `pub fn verify_file<F: FileSystem>(fs: &F, src: &Path, dst: &Path, opts: &VerifyOptions, sink: ...) -> Result<Summary, VerifyError>`
    (one record whose `path` is the destination's file name; plan R4)
  - `pub struct VerifyOptions { pub links: Links, pub mounts: MountPolicy, pub hash: HashAlgorithm }`
  - `VerifyError` for a failure that stops the whole command (a root that is not what the mapping said).
- `flux-cli`: `Commands::Verify(VerifyArgs)` with `SOURCE`, `DEST` (exactly two), `--links`, `--cross-filesystems`,
  `--hash`, `--json`, `--verbose`; a `verify_report` module (the `cleanup_report.rs` precedent; plan R3) for the lines
  and the NDJSON; `exit_code::for_verify(&Summary)`.
- `Code::VerifyMismatch` is reused for a `mismatched` record's `code` field (6.6); no new code is added.

## 9. Tests (the oracle for each is the master line or this section; none may be edited to match the code)

On the in-memory/fault filesystem (`flux-core` unit tests):
1. identical trees: all `match`, summary counts equal, exit 0.
2. one file differing by a single byte in the LAST buffer of a multi-buffer file: `mismatched`, the record names the path.
3. missing file, missing directory (all descendants `missing`, not hashed), extra file, extra directory (descendants `extra`).
4. file vs directory at the same path, both directions (section 6.3): the type record plus children as `extra` /
   `missing`; assert the exact record sequence.
5. component-wise order: `a`, `a/b`, `a-b`, `a.txt` in both trees, in different creation orders: no spurious
   `missing`/`extra`.
6. symlink payload equal / different / symlink vs file; `--links=skip` drops source symlinks but keeps a DEST-only
   symlink as `extra`.
7. `Other` entries are never opened (assert no `open_read` call on them); `Other` vs `Other` is `unreadable` (detail names
   the reason), `Other` vs file `mismatched`, `Other` on one side `missing` / `extra`.
8. an unreadable file on the source and on the destination (injected read error): `unreadable`, the other side
   not guessed; an unreadable directory: `unreadable`, subtree skipped on both sides, siblings still compared.
9. control state: `.flux/`, `.flux-root.lock`, `<n>.flux-partial.<32-hex id>` at the DEST root are never reported; a
   user file `.flux-partial.txt`, `x.flux-partial.1` (the id is not 32 hex characters, so it is not a partial name:
   `state.rs` `id_after`; plan R2) and a nested `sub/.flux` ARE reported (`extra`).
10. DEST absent: every source path `missing`, exit 1, no directory created (assert no write call on the fake), and the
    leading stderr line is printed before the first record.
11. nothing is ever written: the fake records no create/write/rename/remove call across every test above.
12. streaming: a sink that records the call order sees records in path order and the first record before the walkers are
    exhausted (use a source whose later directory errors on listing).
13b. mount points: a paired `Dir` with `MountSkipped` on one side records `match` once with the detail, increments
    `not_entered`, prints the notice, and skips the other side's entries under it; `MountSkipped` on a path present on one
    side only does not record `match`; the summary's `not_entered` equals the number of notices.
13c. roots: DEST == SOURCE, DEST inside SOURCE and SOURCE inside DEST each exit 2 with no read (assert no `read_dir` /
    `open_read` call), including a symlink alias of SOURCE as DEST; unrelated siblings with a common prefix
    (`/d/a` and `/d/ab`) are NOT refused.
13d. `--verbose` prints a line per `match` and the default does not; `--json` is identical with and without it.
13. case-insensitive destination fake: `Readme` vs `README` is `missing` + `extra`.

CLI (`flux-cli/tests/verify.rs`, real binary):
14. exit codes: 0 (all match, and with extras only), 1 (missing / mismatched / unreadable), 2 (a folder SOURCE onto
    an existing file DEST; an unknown `--exclude`; `--resume`).
15. human lines and the summary; NDJSON: every line parses, the last has `summary`, field names exactly the section 6.6
    list, `match` records present.
16. a real `copy` then `verify` is clean; flip one byte on the destination with the file system and `verify`
    reports `mismatched` and exit 1.
17. `verify` against a destination the copy left a partial in: the partial is not reported.

Mutants the driver runs on the new tests: invert the order comparison (bytewise instead of component-wise), skip
the control-state filter, treat a type mismatch as a match, count `extra` toward the exit code, open an `Other`, drop the
`not_entered` increment, drop the containment check in one direction, print `match` without `--verbose`.
Each must turn the named test red and only it.

## 10. Left to the builder

The cursor's data structure, how the paired subtree skip is implemented, the exact human line wording (it must name the
path and the outcome), the `--help` text from 6.8.

## 11. Spec deviations and boundaries this cut records

- Master §4.2 names `--exclude` and `--recursive`: not delivered; deferred with the phase 3 selection (ROADMAP,
  TODO).
- Master §4.2 reports `match` records; human mode here prints only non-matches unless `--verbose` (V5, V13). `--json` prints all.
- Master §4.2 gives usage errors exit 2 and says nothing of nested roots; this cut refuses nested and identical roots with
  exit 2 (V12), which differs from copy's exit 3.
- The `--json` shape is NDJSON (V4).
- Several sources: cut 14b.
- Windows and macOS: unmeasured until CI.

## 12. Exhaustiveness audit (author, before handoff)

Checked and closed in this document: the five outcomes and their exit effect (2, 6.3, 6.6); the walk order and the
comparator (5); memory (3); control state both ways (6.4); roots and DEST absent (6.1); type mismatches with subtrees
(6.3); special files (6.3); read errors and walker errors (6.3); options and the refused ones (6.7); no-write (2, 9.11);
streaming (6.6, 9.12); platform boundary (6.8, 11).

Open, with where each resolves:
- None on `Other` (closed in 6.3) or on `FluxPathKey` (it exists, section 5).
- Windows reparse points and junctions as `Other`/`Symlink`: classified as the walker classifies them; their behaviour is
  CI evidence (plan).
- Output wording of the human lines: builder.
- Closed by the owner-adopted proposal: DEST inside SOURCE and its mirror (6.1, V12); the engine's containment test has a
  "degraded" case when a root's identity cannot be confirmed: the plan reads `tree.rs` ~4351-4380 and states the
  exact message and which of the lexical test or the identity test fires.

## 13. Stand-downs (adversarial panel, 2 rounds, agy escalation each round)

Round 1 findings, all FOLDED: the merge had no rule for `DirEnd` against an unrelated entry (blocking; fixed by
discarding `DirEnd` and merging by key alone, 6.2); `MountSkipped` double-recorded a path and could call an absent
directory `match` (blocking; folded into its `Dir` as a flag, 6.2); the record schema named `code` in two sections
inconsistently (debt, 6.6). Round 2: the `Record` struct in section 8 lacked `code` (debt, FOLDED). The hand trace of the
asymmetric input (source `a/`+`a/x`+`b`, DEST file `a` + dir `b`+`b/y`) gives four records: `a` mismatched, `a/x` missing,
`b` mismatched, `b/y` extra; agy and the author produced the same sequence independently.

- DISCARDED-BELOW-FLOOR: the reused error text "a folder cannot be copied onto the existing file" says "copied" (cosmetic;
  `resolve.rs:49-52`, the message is a user-visible string only and does not change the exit code 2).
- DISCARDED-BELOW-FLOOR: the NDJSON summary has no `duration_ms` (copy's report carries a placeholder `0`,
  `report.rs:118`; omitting a metric cannot make the command wrong).
- DECIDED 2026-10-26: V5 gains `--verbose` (see section 4, item 4).

Negotiation (2 rounds with agy, then the owner's adoption): the calls V1, V2, V4, V6, V7, V8 and V10 were confirmed unchanged.
Round 2's reply was flagged `ECHO MISSING` by the driver (it quoted the wrong closing line of its brief); every file:line it
cited was verified by the author directly (`tree.rs:4377-4378`, master lines 2478-2492 and 2886), and nothing was folded
on the peer's word alone.
