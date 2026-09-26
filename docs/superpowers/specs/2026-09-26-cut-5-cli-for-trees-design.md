# Cut 5: the `flux copy` CLI for trees - design

Walker cut 5 of 5. Cut 4b (PR #52, `c44fb13`) merged `copy_tree`, a safe recursive copy engine with no
command-line surface. This cut exposes it through `flux copy`, renders its outcome for people and for
machines, and makes the three small engine changes the rendering needs. The walker design assigned this
cut "`flux copy` dispatching a directory source to `copy_tree`, the reporting of `TreeOutcome`, the
rendering of `TreeOutcome::warnings` ... and the `--safety=strict` flag"
(`docs/superpowers/specs/2026-09-23-directory-walker-design.md`, delivery item 5).

**Code wins.** This spec names types and behaviour, not line numbers. The plan is authored against `main`
and grep-verifies every citation.

## Decisions

| # | Fork | Decision | Peer |
|---|---|---|---|
| K1 | Exit code when `copy_tree` aborts | the abort carries the partial `TreeOutcome`; exit 3 only when nothing changed | same pick |
| K2 | §4.1 destination mapping | the whole one-source table | same pick |
| K3 | Output | human output on stderr AND `--json` now, every field truthful | peer's pick (driver preferred deferring `--json`); owner chose it |
| K4 | Flags | `--safety`, `--preserve-permissions`, `--preserve`, `--durability`, beside `--preserve-times` and `--json`; no `Preserve::Off` flag | same pick |
| K5 | Symlink vs special file | a special file is SKIPPED and reported per record (§233.1, exit 0); a symlink is a failure (exit 1); the split lives in the engine | same pick |
| K6 | Canonicalization | the CLI resolves the source root and the destination's nearest existing ancestor, never SOURCE's final component | same pick |
| K7 | A symlink given as SOURCE | never followed: `SYMLINK_CREATION_UNAVAILABLE`, exit 1, before the engine is called | converged after two AGY-NEGOTIATE rounds (peer opened with "never resolve", driver with "resolve"; the driver's cp/rsync precedent was wrong and withdrawn); owner asked for one agreed option |
| - | `bytes_total` | `bytes_copied`; a failed file's size is not counted (no pre-scan) | owner |

Consult: AGY-FIRST brief `.clavity/seams/cut5-forks.md`, reply `.clavity/scratch/cut5/forks.md` (local,
gitignored), agy cascade `dab4b353-0c93-4e13-b2e1-cca9ee269d73`.

## Engine changes (`crates/flux-core/src/tree.rs`)

### 1. An abort keeps its partial outcome (K1)

`copy_tree` returns `Result<TreeOutcome, TreeAbort>`:

```rust
/// The operation stopped as a whole. `outcome` holds what happened before it stopped.
#[derive(Debug)]
pub struct TreeAbort {
    pub error: CopyError,
    pub outcome: TreeOutcome,
}

impl TreeAbort {
    /// Whether the destination may differ from before the run: a directory this
    /// operation created, a file it published, or a temporary it could not remove.
    pub fn changed(&self) -> bool {
        self.outcome.directories_created > 0
            || self.outcome.files_copied > 0
            || self.error.leftover.is_some()
    }
}
```

Every outer-`Err` site in 4b's algorithm wraps its `CopyError` with the outcome accumulated so far. Steps 1-4
(source root, lexical floor, resolve, pre-flight) always carry an empty outcome. Step 5 counts the root in
`directories_created` only when `create_dir` made it, so an abort after creating the root reports a
change. The step-6 aborts (the Dir-rule `SafetyRejected`, a `Strict` abort mid-walk, the destination
directory that is the source root, `NoReplacePublishUnavailable` at a publish) carry everything counted
before them.

A per-entry failure never becomes a `TreeAbort`: what aborts is decided by position, never by `Code`
(4b's rule, unchanged).

**Residue:** a file that fails after its temporary is removed, and the partial temporary of an aborted
publish that WAS removed, leave no trace, so they do not count as changes. That matches §55: "a lock,
probe file, or workspace that Flux created and removed again while refusing does not count".

### 2. A special file is skipped, not failed (K5)

§233.1 defaults special files to "SKIP + DURABLE WARNING", "The operation may otherwise complete
successfully", and requires the result to report `SPECIAL_FILE_UNSUPPORTED` with `relative_path`,
`object_type` and `action=skipped` **per record**. §25's default for a symlink is "copy the symlink
itself", which this cut cannot do, so a symlink is an unmet action - a failure.

- `TreeFailureCause::Unsupported(FileType)` is replaced by two variants:
  - `Symlink`: a failure, counted in `FailureTally` (field renamed `symlink`), rendered with the new
    `Code::SymlinkCreationUnavailable` (`SYMLINK_CREATION_UNAVAILABLE`, already in the spec's registry,
    "action-scoped"; added to `flux-fs`'s `Code` with its `as_str` arm).
  - `SpecialFileSkipped`: NOT a failure. `FailureTally` does not count it; `TreeOutcome` gains
    `special_files_skipped: u64`. It is still streamed through the sink, so each one is reported
    individually with bounded memory.
- `FailureTally` stays an exhaustive match with no wildcard; its `count` treats `SpecialFileSkipped` as
  not-a-failure explicitly.
- Because the sink now carries a record that is not a failure, the parameter is renamed `on_failure` ->
  `on_report` and its doc says so. The item type keeps its name `TreeFailure` (renaming it would churn
  every 4b test for no behaviour); its doc states that `SpecialFileSkipped` is the one non-failure it
  carries.

`object_type` for a special file is reported as `special`: the walk types it `Other` from `read_dir` and
does not say FIFO, socket or device. Stated residue.

### 3. Counts `--json` needs

- `TreeOutcome.files_total: u64` - every non-directory entry the walk yields (file, symlink, special),
  INCLUDING entries under a skipped subtree (the walk still yields them; today they are silently passed
  over). Counted on the event, before the live/skipped test.
- `TreeOutcome.files_degraded: u64` - FILES whose Step 2a identity comparison was skipped for a weak side,
  incremented in `copy_one` when `Outcome::identity_degraded` is `Some`. Stored, not derived from
  `WeakIdentityWarnings`: those groups also record DIRECTORY comparisons (the pre-flight and the Dir rule),
  and §51's field is "files degraded". (Panel round 1 corrected the approved "derived" wording, which would
  have counted directories.)

`copy_file` and its `Outcome` are unchanged.

## CLI (`crates/flux-cli`)

`main.rs` keeps argument parsing and dispatch; new modules hold the logic so it is unit-testable without a
process: `resolve` (paths and §4.1), `report` (human lines and JSON), and `exit_code` (the §55 mapping).
`flux-ui` stays a scaffold: it owns progress display (§52), which this cut does not build.

### Resolve (K6)

1. `symlink_metadata(SOURCE)`. Missing: print the error, exit 1.
2. **SOURCE's final component is never resolved.** If `symlink_metadata(SOURCE)` reports a symlink
   (dangling or not, to a folder or to a file), the CLI does NOT call the engine. It prints one record and
   exits 1:
   `SYMLINK_CREATION_UNAVAILABLE: <SOURCE as typed>: a symlink is copied as a link (§25), which this
   version cannot create; name its target to copy what it points at`.
   `--json` prints `files_total: 1`, `files_failed: 1`, `errors: 1`. This is the same code the tree
   reports for a symlink met inside the walk, so one concept has one code; without the check the engine
   would say `SPECIAL_FILE_UNSUPPORTED` "not a regular file" for a file link (`copy_file_at` step 2) and
   refuse a folder link as a non-directory root, both misdescribing a link.
   §25: "copy the symlink itself", "Following symlinks is opt-in". This SUPERSEDES the walker design's
   assignment that the CLI resolves a symlinked source root (settled by AGY-NEGOTIATE at the owner's
   request, see Decisions).
3. For a folder source, canonicalize the source root (its final component is a real directory by step 2,
   so only INTERMEDIATE links resolve - path resolution, not copying a link), and canonicalize DEST's nearest existing ancestor
   and append the absent remainder lexically (components that do not exist cannot be links). Both sides
   are canonicalized or neither, so Windows' `\\?\` prefix is on both.
   The nearest existing ancestor is found by trying `canonicalize` on DEST, then on each `parent()` in
   turn, moving up ONLY on `ErrorKind::NotFound` (absent, or a dangling link) and collecting the skipped
   components; any other error is a resolution failure (exit 1). A relative DEST is first joined to the
   current directory, so the climb always ends at a root that exists.

**Why it is still needed after 4b.** 4b resolves the destination and compares identities, which catches a
symlinked destination when identity is `Strong`. On FAT32 and exFAT identity is weak, the identity checks
degrade to warnings, and only the lexical floor stands. Without canonicalization
`flux copy /usb/data /usb/link/backup` (`link` -> `data`) passes the floor. §128: "symlink traversal must
not invalidate the containment decision".

Every message shows paths as the user typed them; a `\\?\` path is never printed. Tree records are
relative to the destination root and use `/` separators (§233.1).

Step 2 applies to both kinds of source. Otherwise the single-file path keeps its paths as given: its Step 0 lexical check and Step 2a identity gate already
cover self-copy, and a degraded weak-identity case stages through a distinct temporary (the bytes
published are the source's own).

### Map (§4.1, one source)

| Source | DEST | Action |
|---|---|---|
| folder | an existing file | usage error, exit 2 |
| folder | anything else | `copy_tree(src, DEST)` - the folder's CONTENTS land on DEST |
| file | an existing folder, or a path ending in a separator | `copy_file(src, DEST/<name>)` |
| file | anything else | `copy_file(src, DEST)` |

"Ends in a separator" is tested on the raw argument (`/`, and `\` on Windows). DEST is classified with
the FOLLOWING `std::fs::metadata`, because 4b follows a user-named destination (§149.7): a DEST link to a
folder is "an existing folder", a DEST link to a file is "an existing file". `NotFound` - absent, or a
dangling link - is "anything else"; for a folder source the engine then meets the dangling link at its
root creation and refuses it (`open_dir` does not follow a link), which is an unchanged refusal, exit 3.
A file source with a trailing-separator DEST that does not exist is sent to `copy_file(src, DEST/<name>)`
and fails because the parent is missing (exit 1); §4.1 does not say to create it.

Several sources (§18.3) stay out: they need the operation workspace. `flux copy` keeps exactly two
positionals, so a third is clap's usage error (exit 2), and `--help` says several sources are not yet
supported. The single-file path keeps `Publish::Replace` (§5.1's
default `--overwrite`).

### Flags (K4)

| Flag | Sets |
|---|---|
| `--preserve-times` (exists) | `preserve_times = Strict` |
| `--preserve-permissions` | `preserve_permissions = Strict` |
| `--preserve` | both |
| `--durability=<normal\|strict>` (default `normal`) | `durability` |
| `--safety=<default\|strict>` (default `default`) | `safety` |
| `--json` | the §53 report on stdout |

Without a preserve flag each item stays `Preserve::Default` (best effort). No flag reaches
`Preserve::Off`: the spec defines none (TODO item unchanged). `--safety` is not in §5's option list; its
basis is invariant 23 ("explicit strict failure") and the walker design, and `--help` says what it does.

### Exit codes (§55)

| Code | When |
|---|---|
| 0 | no failure. Weak-identity warnings and skipped special files still exit 0. |
| 1 | a symlink SOURCE (K7); any streamed failure (including `PublishedWithComplaints`), a single-file error other than the refusal below, or a `TreeAbort` that is not an exit-3 refusal |
| 2 | a usage error: clap's own, or the §4.1 folder-onto-file case |
| 3 | a `TreeAbort` with `changed() == false`, NO streamed failure before it (`outcome.failures.is_empty()`), and code `SAFETY_REJECTED` or `NOREPLACE_PUBLISH_UNAVAILABLE`; a single-file `SAFETY_REJECTED` (both of `copy_file`'s sites, Step 0 and the Step 2a gate, run before the temporary is created, so nothing has changed) |

A mid-walk abort that follows streamed failures is 1 even when nothing changed: the run acted on some
paths before the refusal, which is §55's "a partial run hit a refusal on some paths only" (panel round 1).
A failure of the CLI's own resolution - `canonicalize` on SOURCE or on DEST's nearest existing ancestor
(for example a symlink loop, or a permission error) - prints the error and exits 1: it is not one of §55's
named refusals, and nothing was attempted.

A missing or unreadable source is 1, not 3: §55 reserves 3 for a refusal "because of the state of the
destination, a prior operation, or the platform". An exit-3 condition that changed something is 1, as §55
requires.

### Known gap, stated in `--help` and `TODO.md`

§5.1's default `--overwrite` is unmet for trees: 4b never replaces an existing destination file (the
§241.5 claim store does not exist). `flux copy dir existing-dir` reports one
`DESTINATION_NAMESPACE_COLLISION` per pre-existing file and exits 1. No policy flag
(`--overwrite`/`--update`/`--skip-existing`) is added.

## Output

### Human (stderr)

- Each streamed record prints one line as it arrives: `CODE: relative/path: detail`, and a leftover
  temporary when the `CopyError` carries one (via its existing `Display`). A skipped special file prints
  `SPECIAL_FILE_UNSUPPORTED: relative/path: object_type=special action=skipped`.
- A `TreeAbort` prints its error the same way, after any streamed lines.
- After the run (on success and on abort), warnings:
  - one line per weak volume: its volume id, count, the example path, and the lever
    (`--safety=strict` refuses instead);
  - one line for the `Unavailable` bucket, same shape;
  - the single-file path renders `Outcome::identity_degraded` as one such line, with the path the user
    supplied.
- Then one summary line: files copied, bytes, directories created, failures, special files skipped. Its
  failure figure is the JSON `errors` value (so a `TreeAbort` counts as one), and a run that exits
  non-zero never prints a summary claiming zero failures (panel round 2).

The plan fixes the exact wording. Tests pin the contract, not the prose: a weak-volume line contains the
volume id (hex), the count, the example path, and `--safety=strict`; a record line starts with its `CODE:`.

Writes to stdout and stderr never panic: a closed pipe (`flux copy --json ... | head -c0`) is ignored,
not a crash, and the exit code is the operation's. (`println!`/`eprintln!` panic on a failed write, which
would turn a finished copy into exit 101.)

### `--json` (stdout, one object, at the end)

Printed on every exit except a usage error (exit 2): 0, 1 or 3, single file or tree, success, failure or
abort. A failure BEFORE the engine is called - a missing SOURCE, or the CLI's own resolution failing -
prints every count 0 with `errors: 1` (K7's symlink SOURCE is the one pre-engine case with
`files_total: 1`, `files_failed: 1`, because the named object is known to be one entry). Human output still goes to stderr. §53 calls its
list "Complete field list (not only an example)", so every field is present, and each value is true for
what this cut does:

| Field | Value |
|---|---|
| `files_total` | `TreeOutcome.files_total`; 1 for a single file |
| `files_copied` | engine count (0 or 1 for a single file) |
| `files_skipped` | `TreeOutcome.special_files_skipped` (tree); 0 for a single file |
| `files_overwritten` | single file: 1 if the target existed when resolved and the copy succeeded; tree: 0 |
| `files_hardlinked`, `files_reflinked`, `files_verified`, `files_mismatched` | 0 - none exist in this cut |
| `files_degraded` | `TreeOutcome.files_degraded`; single file: 1 if `identity_degraded` is `Some` |
| `files_failed` | tree: the tally's `copy + symlink` (file-level failures; walk and create-dir failures are not files, and published-with-complaints files ARE at the destination). Single file: 1 on an error (including K7), else 0 |
| `bytes_total` | `bytes_copied` (see below) |
| `bytes_copied` | engine count |
| `bytes_skipped` | 0 |
| `errors` | tree: the tally's total, plus 1 for a `TreeAbort`. Single file: 1 on an error or on published-with-complaints, else 0 |
| `duration_ms` | measured by the CLI around the engine call |
| `average_bytes_per_second` | `bytes_copied * 1000 / max(duration_ms, 1)` |
| `verify_level` | `"none"` - this cut verifies nothing; never `"destination"` |
| `hash_algorithm` | `"blake3"` - the only value `--hash` accepts (§5); no hashing runs |

Note on `files_skipped`: a skipped special file IS a skipped target (§233.1 `action=skipped`; §124 treats
a skipped symlink "as for unsupported special files"), so it is counted there, and §233.3's "never
disappear from the operation summary" holds in the JSON too. No skip POLICY (§5.1) exists in this cut, so
this is the field's only source. (Panel round 1.)

`bytes_total` is `bytes_copied` by the owner's choice: the walk never stats a file, so a failed file's
size is unknown, and a pre-scan would double metadata I/O. Documented in `--help`'s JSON note and here.

`files_overwritten` for a single file reads the target's `symlink_metadata` once at resolution. It is a
report, not a decision, so the race to the copy only affects the count.

Serialization uses `serde` + `serde_json` (already workspace dependencies), added to `flux-cli`.

## Testing

**Engine (`FaultFs` unit tests, each with a mutation check):**
- a `TreeAbort` at each abort site carries the right outcome: empty for the lexical floor and the
  pre-flight; `directories_created == 1` when the root was created and a later step aborts; counts
  before a mid-walk Dir-rule abort; `changed()` on each.
- a special file is streamed as `SpecialFileSkipped`, counted in `special_files_skipped`, and absent
  from `FailureTally::total()`; a symlink is a failure.
- `files_total` counts entries under a skipped subtree.
- `files_degraded` counts a degraded FILE and not a degraded directory.

**CLI (`crates/flux-cli/tests`, real temporary directories):**
- each §4.1 row, including the trailing-separator case and folder-onto-file = exit 2;
- exit 0 for a clean tree; exit 1 for a collision in an existing destination; exit 3 for a destination
  inside the source with nothing created. Exit 1 for an abort AFTER a change has no portable real-disk
  trigger (each mid-walk abort needs a bind mount, a weak-identity volume, or a filesystem without the
  no-replace primitive), so it is pinned in the `exit_code` module's unit tests with a constructed
  `TreeAbort`, one case per `changed()` input; the engine tests pin that each abort site sets them;
- the collision test asserts more than the exit code: another file in the same run WAS copied and the
  collision's record line was printed, so an engine that aborted on the first collision fails it;
- the single-file hardlink refusal now exits 3 (the existing tests assert only `!success`; they gain the
  code);
- `--safety=strict` sets `Safety::Strict` (pinned in the `exit_code`/options unit tests; a real weak-identity
  volume is not on CI);
- `--json` parses, has every §53 key, and pins `files_copied`, `bytes_copied`, `verify_level`;
- unix only: a FIFO is skipped with exit 0 and a record line; a symlink inside the tree fails with exit 1;
  a symlink given as SOURCE (to a folder, to a file, and dangling) exits 1 with the
  `SYMLINK_CREATION_UNAVAILABLE` record and changes nothing at DEST. (Unix only because creating a symlink
  on Windows needs a privilege CI does not grant.)

The workspace-level integration placeholder (`tests/integration/mod.rs`) is untouched.

## Out of cut 5

Several sources (§18.3); the §5.1 policy flags and replacing existing files in trees (the claim store);
`--dry-run`, `--verify`, `--exclude`, `--workers`, `-v`/`--quiet` and `tracing`; progress UI (§52,
`flux-ui`); §42 mount boundaries; item 113's up-front probe; recreating symlinks; hardlink topology;
directory metadata.

## Residues, stated

- `bytes_total` omits the size of files that failed.
- `object_type=special` does not name the kind of special file.
- The §5.1 `--overwrite` gap for trees (above).
- 4b's residues carry forward unchanged (Step 2a-to-publish window, mid-walk aborts leave earlier copies,
  weak-identity folds merge silently, subdirectory aliases are §42's).

## Stand-downs

- `REJECTED: a file published and then failed would escape changed()` - `copy_file_at`'s publish rename is
  its final step; `Ok(Outcome ..)` follows it directly (`crates/flux-core/src/copy.rs`, the `published`
  match), so there is no post-publish failure to miss.
- `REJECTED: files_total cannot count a skipped subtree without extra I/O` - under a `Skipped` frame the
  walk still reads and yields events; `copy_tree` passes over them (`crates/flux-core/src/tree.rs`, "Under
  a skipped subtree the walk still reads"), so counting costs nothing.
- `REJECTED: resolved root paths leak through CopyError's Display` - `Display` writes the `FsError` and an
  optional leftover (`crates/flux-core/src/copy.rs`, `impl Display for CopyError`); tree leftovers are
  rebuilt destination-relative, and no root path is embedded.
- `REJECTED: dunce keeps the Windows verbatim prefix on long paths` - this spec uses no `dunce`; it prints user-typed paths.
- `REJECTED: the exit-3 row omits the no-streamed-failure condition` (panel round 2) - the row carries it
  ("NO streamed failure before it (`outcome.failures.is_empty()`)"); the quote offered does not appear in
  the spec at `dd4b7d7`.
- `REJECTED: TreeAbort is undefined` (panel round 2) - defined under "An abort keeps its partial outcome".
