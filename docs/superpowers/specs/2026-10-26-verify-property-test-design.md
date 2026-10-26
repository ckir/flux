# A property test of the `flux verify` merge against a reference model (cut 14a) - design

Status: scope owner-approved 2026-10-26 after AGY-FIRST round 1 (reply `-3098`) and AGY-NEGOTIATE round 1 (reply `-3164`,
both ALIGNED; the peer yielded on the two open differences). Design awaiting owner approval. Branch `feat/verify-proptest`
off `main` at `7d39489` (the merge of PR #132, cut 14). Design under test: `docs/superpowers/specs/2026-10-26-verify-command-design.md`
(cited as "cut 14 design, section N").

## 1. Intent

The verify engine (`crates/flux-core/src/verify.rs`) merges two walks into one record stream with a small state machine per
side (`Side`: one event of lookahead for the mount flag, one held item, one pruned prefix, two filters). The cut 14 test
audit found that this state is exercised only through 39 hand-written fixtures, so a regression that drops or reorders one
item in a specific interleaving is caught only if that interleaving happens to appear in a fixture. The audit peer proposed
a property-based model comparison: generate pairs of trees, run the real engine on the test fake, and compare its ordered
record sequence and summary with a simple non-streaming reference that applies the cut 14 design's rules directly.

What this test IS: a check that the STREAMING implementation gives the same answer as a BATCH implementation of the same
rules, over many generated topologies. What it is NOT: an oracle of the rules themselves. The reference is written from the
same design sections by the same people, so a misreading of section 6.2 shared by both stays green. The fixtures remain the
rules' oracle; this test guards the cursor.

## 2. Goals, non-goals, anti-goals

Goals
- One property: for every generated case, the engine's `Vec<Record>` (in emission order) and its `Summary` equal the
  reference's, and the emitted paths are strictly increasing in `FluxPathKey` order.
- The generated universe covers all four entry kinds, content equal and differing, type changes with subtrees, entries on
  one side only, prefix-colliding sibling names, destination control state and its near-misses, mount roots under both
  mount policies, source symlinks under both link policies, and listing faults on non-root directories.
- Seed-reproducible, deterministic on every CI OS (the fake has no I/O), 256 cases per run by default.
- A failure prints a pasteable fixture in the style of the `verify.rs` tests, and proptest's regression file is committed.
- Success criterion (measured by the driver in the plan's mutant table, not a committed test): at least two logic mutants
  of the merge that leave all 39 fixtures green are caught by the property within its default budget. The merge is small
  and the fixtures are good, so most single-point mutants are either killed by a fixture or EQUIVALENT (same output on
  every input); the candidates below were chosen by reading for a shape the fixtures lack and the generator has:
  (h) `under_pruned` tests a BYTE prefix (`path.as_os_str().as_encoded_bytes().starts_with(..)`) instead of the
  component-wise `Path::starts_with`, so after pruning `a` the sibling `a-b` or `a.txt` is wrongly dropped (no fixture
  prunes a directory that has a prefix-sharing later sibling; the vocabulary of 6.3 produces that often);
  (n) `prune` keeps the FIRST prefix instead of replacing it (`self.pruned.get_or_insert(..)`), so a second pruned
  directory's descendants leak (no fixture has two prunes in one run; generated cases with several faults or mounts do);
  (p) `pair`'s failed arm takes the DESTINATION error first (`y.failed.or(x.failed)` instead of `x.failed.or(y.failed)`,
  `verify.rs:551`): no fixture faults BOTH sides of one directory (the both-sides fixture fails only the destination's
  listing), while the generator does so with differing codes and the reference asserts source-first precedence.
  Measured dead already, not a candidate: the mount fold in `pair` pruning only one side is killed in both directions by
  `a_mount_point_on_either_side_is_matched_once_and_pruned` (its `m/x` lies on the un-pruned side in each case).
  Mutants analysed as EQUIVALENT or FIXTURE-KILLED, which the plan must NOT count: `prune` not clearing `held` (a child is
  never held without its parent having been paired or pruned first); `advance` not consuming the `peeked` `MountSkipped`
  (the stale event is returned by the next `raw()` and dropped by the `continue` arm); `under_pruned` using equality
  (killed by the mount fixture above, whose `m/x` lies under the pruned `m`); either `alone` arm pruning only its own side
  (the other side has nothing under a path it lacks); `pair`'s failed arm not pruning the FAILED side (its walker already
  skipped the subtree); the order of the two `continue` filters in `advance`. If the plan's mutant table finds fewer than
  two fixture-green, property-red mutants, the owner rules on the criterion; the property's other value (a reference run
  over thousands of topologies on every CI run) stands on its own.

Non-goals
- Faults on the roots (`Side::new` fails before the merge; pinned by `an_unlistable_source_root_is_one_unreadable_record`
  and its destination twin). Mid-file read faults (`fail_read_nth`; they need a per-file read index and are pinned by
  fixtures). Faults from `mount_root` or `canonical_path`. `verify_file`. The CLI, its output and exit codes. The real
  filesystem. Replacing any fixture. Refactoring `Side` to take a synthetic event stream (deferred; it reopens the engine).

Anti-goals (a test with any of these has failed even if green)
- A reference that calls engine code (`Side`, `verify_tree`, `compare_pair`, `is_control_state`).
- A comparison that sorts either sequence before comparing.
- A generator that can produce only identical trees, or that never produces a fault, a mount fold, a control-state drop or
  a type change (test 3 in section 9 pins this).
- Added CI time beyond a few seconds per OS.

## 3. Constraints (verified against the code at 7d39489)

- The fake is test-only: `#[cfg(test)] pub mod fault_fs;` (`crates/flux-core/src/lib.rs:29`). An integration test under
  `crates/flux-core/tests/` cannot see it, so the property test is an in-crate `#[cfg(test)]` module.
- `proptest = "1.11"` is pinned in the workspace `Cargo.toml` (test tooling) and used by no crate yet; `flux-core` adds it
  as a dev-dependency. `cargo deny` allows MIT and Apache-2.0 (`deny.toml`), which proptest is dual-licensed under.
- The fake lists only what `create_dir` made (`FaultFs::read_dir`, `fault_fs.rs:1782`); files are seeded with `write_file`,
  symlinks with `add_symlink_to(path, payload, LinkKind::Unspecified)`, specials with `add_special`, mount answers with
  `set_mount_root(path, MountRoot::{Yes, No, Unknown})`. Paths are normalised with `norm` (identity unless the
  case-insensitive mode is on).
- `fail_nth("read_dir", n, ..)` fails the n-th listing by GLOBAL call index; no path-targeted listing fault exists today
  (`fail_canonical_path_of` and `fail_read_nth` are the path-targeted shapes). A generated fault needs a path-targeted one.
- The walker asks the mount question BEFORE listing, and a `MountRoot::Yes` under `MountPolicy::Stay` returns the `Dir`
  with an EMPTY frame (`walk.rs:412-418`, `entries: Vec::new().into_iter()`): no `read_dir`, and the children of that
  directory NEVER appear in that side's stream, whatever the other side holds. Under `MountPolicy::Cross` no mount question
  is asked. `MountRoot::Unknown` yields `MountUnknown`, which the engine ignores (the directory is listed).
- A failed item that stands alone carries its own kind, always `dir` (`verify.rs:532`); a failed PAIR carries the SOURCE
  item's kind (`verify.rs:552`, `x.kind`), so a destination directory that cannot be listed against a source file is
  `unreadable` with `kind: file` and the destination's code.
- Every `WalkError` is a directory (the walker raises one only while listing), so its record has `kind: dir`.
- Keys: `FluxPathKey::from_relative` is the Normal components' bytes joined by `0x00` (`flux-fs/src/claims.rs:14-46`), so
  `a/b` (`a\0b`) sorts before `a-b` and before `a.txt`.
- Control state dropped on the destination side only (`Side::is_control_state`, `verify.rs:201-223`): a top-level `.flux`
  that is a directory, and everything under a top-level `.flux`; a top-level FILE named `.flux-root.lock`; a File or Symlink
  whose name is `<head>.flux-partial.<32 lowercase hex>` with a non-empty head (`state::id_after`). Not dropped: a top-level
  FILE named `.flux`, a top-level DIRECTORY named `.flux-root.lock`, a 31-hex or uppercase id, an empty head, a nested
  `.flux`, a DIRECTORY named like a partial.
- `Links::Skip` drops source-side symlinks before the merge; the destination side is never filtered by it.
- Equal-path results and their details (cut 14 design 6.3; `compare_pair`, `verify.rs:580-640`): File/File hashes both and
  on a difference emits `mismatched`, `VerifyMismatch`, detail `"{s} != {d}"` with the first 8 hex characters of each BLAKE3
  digest (`compare_files`, line 350); `bytes_hashed` counts BOTH sides. Symlink/Symlink compares payload bytes, detail
  `"symlink payloads differ"`. Other/Other is `unreadable`, `SpecialFileUnsupported`, detail `SPECIAL_NOT_COMPARED`. Any
  other pair is `mismatched`, `VerifyMismatch`, detail `"source {k1}, destination {k2}"` with `Kind::as_str`. Dir/Dir is
  `match`, detail `MOUNT_NOT_ENTERED` when either side's flag is set, which also increments `not_entered` and prunes both
  sides (`pair`, lines 557-572).
- Faults in the merge (`verify.rs:520-556`): a source-only failed item is `unreadable` with the error's code and text and
  prunes both sides; a destination-only failed item is `extra` with `code: Some(e.code)` and the text, and prunes both; a
  pair where either side failed is `unreadable` with the SOURCE side's code and text when the source failed, else the
  destination's (`x.failed.or(y.failed)`, line 551), and prunes both. The text is `io::Error::from(kind).to_string()`.
- `Record` and `Summary` derive `PartialEq` (`verify.rs:63, 84`), so direct `assert_eq!` works. `MOUNT_NOT_ENTERED` and
  `SPECIAL_NOT_COMPARED` are `pub` (lines 79, 81). `crate::copy::COPY_BUF_MAX` is 256 KiB (`copy.rs:185`).
- Depth limit `DEFAULT_MAX_DEPTH` is 256; generated depth stays at most 4.
- The repo gate: `just check` (nextest with fail-fast, clippy, fmt, typos; no short git sha anywhere). CI runs the suite on
  Linux, macOS and Windows; the property test must be identical on all three (it is, the fake is in memory).
- proptest's default failure persistence is `SourceParallel("proptest-regressions")`: for `src/verify_proptest.rs` the file
  is `crates/flux-core/proptest-regressions/verify_proptest.txt`. No `.gitignore` rule excludes it.

## 4. Decisions the owner made (2026-10-26)

1. Scope as negotiated with the peer, five items: the fake primitive; the generator (joint trees, prefix-colliding names,
   all kinds, control state and near-misses, mount roots, non-root listing faults; no root or read faults); the isolated
   reference with source-first error precedence; the committed regression file plus a printed fixture; the key-order
   invariant, ordered comparison, summary equality, 256 cases.
2. Branch from `main` after PR #132 merged (not stacked).

## 5. Approaches considered

- **A. Full reference model over the fake (chosen).** The engine runs on `FaultFs` exactly as the fixtures run it; a pure
  function computes the expected records from the generated case. Catches cursor state bugs across arbitrary topologies.
  Trade-off: the reference re-implements the rules, so it is blind to a shared misreading; it needs one fake primitive.
- **B. Metamorphic properties only** (`verify(T, T)` all match; swapping sides maps missing to extra; one added entry adds
  one record; order strictly increasing). No reference to get wrong, but blind to every ASYMMETRIC rule: destination-only
  control-state filtering, the three-way `WalkError` rule, mount folding. Kept as an extra invariant (key order), not as the
  oracle.
- **C. A model of `Side` alone** fed synthetic event streams. Smallest and fastest, but `Side` holds the concrete walker, so
  it needs the engine made generic over its event source, which reopens the cut 14 capstone. Deferred.
- Inside A, two forks were negotiated with the peer and settled: listing faults in version one (the peer had deferred them;
  three of the four prune sites are failure arms, so a fault-free generator reaches prune only through the mount fold) and
  committing the regression file (proptest's README and the header it writes into the file both say to).

## 6. Design

### 6.1 Where it lives
- `crates/flux-core/src/verify_proptest.rs`, registered in `lib.rs` as `#[cfg(test)] mod verify_proptest;` directly after
  the `fault_fs` registration, carrying the repository's licence header (`just check` runs `license`, an `addlicense`
  check over tracked `*.rs`, besides `fmt-check clippy typos doc-check test`).
- `crates/flux-core/Cargo.toml` gains, under `[dev-dependencies]`, `proptest = { workspace = true, default-features =
  false, features = ["std"] }`: the default feature set adds `fork` and `timeout` (and their `rusty-fork`, `tempfile`
  trees), which this test does not use and `deny.toml` asks to avoid ("must stay dependency-light"); add `bit-set` only if
  a strategy the builder uses needs it (the compiler says). `Cargo.lock` changes with it; `cargo deny check` must stay
  green (it runs in CI).
- The property uses the `proptest!` macro: it is what sets `source_file`, and without it proptest persists NO regression
  file (`proptest-1.11.0/src/sugar.rs:966`, `failure_persistence/file.rs:86-90`).
- `crates/flux-core/proptest-regressions/verify_proptest.txt` is committed the first time proptest writes it (a failure);
  the commit that fixes the failure keeps the seed line until a named fixture pins the case, then may delete it. A seed
  replays the RNG, not the case: a later change to the generator (vocabulary, weights, shapes) makes old seeds produce
  different cases, so the promoted FIXTURE is the durable pin and the seed line is a tripwire until then.

### 6.2 The fake primitive
`pub fn fail_read_dir_of(&self, path: impl AsRef<Path>, code: Code, kind: std::io::ErrorKind)` on `FaultFs`:
- stores `(code, kind)` in a new `Inner` field `read_dir_faults: HashMap<PathBuf, (Code, std::io::ErrorKind)>`, keyed by
  `self.norm(path)`, computed BEFORE taking the `inner` lock (`norm` locks it itself, `fault_fs.rs:1344`; locking first
  deadlocks); persistent (every listing of that path fails), like `fail_canonical_path_of`;
- `read_dir` consults it AFTER `self.record(..)` (so the call is logged and a global `fail`/`fail_nth` on `"read_dir"` still
  fires first) and AFTER the "is a directory" check (a missing path stays `NotFound`), returning
  `Err(FsError::new(code, std::io::Error::from(kind)))` before listing.
- Test infrastructure only (the module is `cfg(test)`); no engine code changes.

### 6.3 The generated case
```
struct Case { base: Vec<(RelPath, Spec)>, edits: Vec<Edit> /* one per base entry, same index */,
              extras: Vec<(Which, RelPath, Spec)>, mounts: MountPolicy, links: Links }
enum Which { Source, Dest }
type RelPath = Vec<&'static str>;   // 1 to 4 vocabulary components, joined for the fake with `/`
enum Spec  { File { len: usize, fill: u8 }, Dir { mount: MountRoot, list_error: Option<Code> }, Symlink { payload: &'static str }, Other }
enum Edit  { KeepBoth, EditContent(Which) /* File: flips `fill`, or sets `len` to 1 when it was 0; Symlink: the other payload;
             a no-op on Dir and Other */, ChangeKind(Which, Spec) /* that side's entry becomes `Spec` */, DropFromDest, DropFromSource }
```
- A file's bytes are `vec![fill; len]`, built in `materialise` (a generated `Vec<u8>` of 256 KiB would make proptest's
  element-wise shrinking crawl); `fixture_snippet` prints them the same way.
- Names come from a fixed vocabulary chosen for prefix collisions: `a`, `a-b`, `a.txt`, `a0`, `b`, `sub`, `z`. A relative
  path is 1 to 4 such components. The base holds 3 to 10 entries, deduplicated by path (the first wins); every entry's
  parent is a `Dir` in the base (the strategy builds directories first and places children under them).
- File sizes from `{0, 1, 17, COPY_BUF_MAX + 1}` (empty, tiny, small, multi-buffer); symlink payloads from `{"t", "u"}`;
  `mount` from `{No, Yes, Unknown}` with `Yes` at about one in four directories; `list_error` `Some` for about one in five
  non-root directories, code from `{PermissionDenied, IoError}` with the matching `io::ErrorKind`.
- Each base entry gets one `Edit` (`edits[i]` applies to `base[i]`); `KeepBoth` has the highest weight so `match` is
  common; the other four have fixed minimum weights, and `Which` is drawn evenly (section 9, test 3 pins that all are
  reached on both sides).
- `extras` are entries added to ONE side after the edits, 0 to 4 per case, each placed under the root or under any `Dir`
  that side holds after the edits. Two sources of names, weighted about evenly: (i) the ordinary vocabulary with any
  `Spec` (this is what puts a subtree on one side under a path the other side holds as a file, the shape of
  `a_type_mismatch_drains_the_children_as_extra_or_missing`); (ii) on the destination side only, the control-state list:
  at the TOP LEVEL only, `.flux/` (a Dir) with a child file, a FILE `.flux`, a FILE `.flux-root.lock`, a DIR
  `.flux-root.lock`; under the root OR any Dir, `name.flux-partial.<32 lowercase hex>` as a File and as a Symlink,
  `name.flux-partial.<31 hex>`, `name.flux-partial.<32 UPPERCASE hex>`, `.flux-partial.<32 lowercase hex>` (empty head),
  a DIR named `name.flux-partial.<32 hex>`; and `sub/.flux` (a Dir, nested) under a Dir. Faults and mount answers live in
  `Dir` specs, so a ONE-SIDED fault or mount answer arises only through `ChangeKind(which, Dir { .. })` or an extra `Dir`
  on one side; test 3 pins that both arise.
- `mounts` from `{Stay, Cross}`, `links` from `{Copy, Skip}`, each about half.
- Deriving the two sides: start both from `base`; apply each edit to the side it names (the drops remove from one side);
  append each extra to its side; then on EACH side drop every entry whose parent on that side is not a `Dir` (a changed
  or dropped directory takes its children with it on that side). Roots are fixed: `/s` and `/d`, never generated.

### 6.4 Materialising a side on the fake
`create_dir` the root, then every `Dir` parents-first (sorted by component count), then `write_file`, `add_symlink_to(..,
LinkKind::Unspecified)`, `add_special`; `set_mount_root(path, mount)` for every `Dir` whose `mount` is not `No`;
`fail_read_dir_of(path, code, kind)` for every NON-ROOT `Dir` with a `list_error`. Record the call count after setup (the
fixtures' `assert_no_writes` idiom) so the run's calls can be checked.

### 6.5 The reference (a pure function of `Case`, std collections plus `FluxPathKey`, `flux_hash`, `Kind`, `Outcome`, `Code`, the two `pub` detail constants)
0. Walker view, per side: when `mounts == Stay`, drop every entry strictly BELOW a `Dir { mount: Yes }` on that side (the
   walker returns such a directory with an empty frame; section 3). The mount root itself stays. Its `list_error`, if
   any, is inert because it is never listed.
1. Source filter: when `links == Skip`, drop source `Symlink` entries.
2. Destination filter: drop control state by the rules in section 3, restated in the reference's own code (never by calling
   `is_control_state`).
3. Key both sides: `BTreeMap<Vec<u8>, (PathBuf, Spec)>` keyed by `FluxPathKey::from_relative(path).0`.
4. Merge the two key sequences in order with one `pruned: Option<Vec<u8>>` prefix shared by both sides (the engine prunes
   both sides at once; preorder makes one slot enough, exactly as in `Side`). An entry is under the prefix when its key
   equals it or starts with it followed by `0x00`. For each step, with `s` the next unpruned source entry and `d` the next
   unpruned destination entry:
   - a `Dir` with a `list_error` counts as FAILED unless `mounts == Stay` and its `mount == Yes` (the walker never lists a
     mount root; section 3); a fault is evaluated only when its entry is reached, so one below a pruned or unlisted
     directory never produces a record;
   - source only (`ks < kd` or `d` exhausted): failed Dir -> `unreadable`, `kind: dir`, `Some(code)`, text; prune. Else
     `missing`, `code: None`, empty detail.
   - destination only: failed Dir -> `extra`, `kind: dir`, `Some(code)`, text; prune. Else `extra`, `None`, empty.
   - equal keys: either failed -> `unreadable`, `kind` = the SOURCE entry's kind (section 3: `pair` emits `x.kind`), the
     source's `(code, text)` if the source failed else the destination's; prune. Else by kinds: Dir/Dir -> `match`; if `mounts == Stay` and either `mount == Yes`, detail
     `MOUNT_NOT_ENTERED`, `not_entered += 1`, prune. File/File -> BLAKE3 both; equal -> `match`, else `mismatched`,
     `VerifyMismatch`, `"{s8} != {d8}"`; `bytes_hashed += both lengths` either way. Symlink/Symlink -> payload bytes equal
     -> `match`, else `mismatched`, `VerifyMismatch`, `"symlink payloads differ"`. Other/Other -> `unreadable`,
     `SpecialFileUnsupported`, `SPECIAL_NOT_COMPARED`. Any other pair -> `mismatched`, `VerifyMismatch`,
     `"source {k1}, destination {k2}"`.
5. `Summary`: the counts of each outcome, `not_entered`, `bytes_hashed`.
The text of a fault is `std::io::Error::from(kind).to_string()`.

### 6.6 The property and its invariants
Run `verify_tree(&fs, "/s", "/d", &VerifyOptions { links, mounts, hash: Blake3 }, sink)` and assert, in this order:
1. the engine's `Vec<Record>` equals the reference's, element by element, in emission order (never sorted);
2. the engine's `Summary` equals the reference's;
3. the records' `FluxPathKey`s are strictly increasing;
4. no `open_read(` was logged for a path whose kind on either side is not `File`, and every logged call after setup is a
   read (`metadata(`, `read_dir(`, `open_read(`, `read_link(`, `source_mount_root(`, `canonical_path(`, `identity(`).
On any failure the assertion message carries `fixture_snippet(&case)`: the `tree(&fs, "/s", &[..], &[..])` lines, the
`add_symlink_to`, `add_special`, `set_mount_root`, `fail_read_dir_of` calls, and the options, ready to paste into
`verify.rs`'s `mod tests` as a named fixture.

### 6.7 Budget and configuration
`ProptestConfig::default()`, whose `cases` is already 256 and which is the ONLY place proptest reads `PROPTEST_CASES`
(`config.rs:164, 231`): an explicit `cases:` field would make the environment override inert. Failure persistence left at
the default; shrinking is proptest's (a shrunk case still fails, so a fault needed for the failure stays).

## 7. Calls made by the author (for the owner to check)
1. `MountPolicy` and `Links` are generated parameters, not fixed to `Stay`/`Copy`: the reference handles both in one line
   each and the engine's `cross_filesystems_enters_mount_points` and `links_skip_...` behaviours come under the property.
2. `MountRoot::Unknown` is generated (it is a third answer the walker handles, and it costs nothing).
3. A mount root under `Stay` is never listed, so its `list_error` is inert; the reference encodes that precedence.
4. The fault check in `read_dir` runs after the call is logged and after the directory-exists check.
5. Fault codes limited to `PermissionDenied` and `IoError`; the text is the `io::ErrorKind`'s standard description.
6. The vocabulary, sizes and weights in 6.3 are starting values; the builder may tune them but test 3 of section 9 must
   still pass with the default seed policy.
7. The peer's concrete uncovered interleaving (a source file against a destination mount root) becomes a named fixture in
   `verify.rs` as well as being reachable by the property. The peer's own expectation for it (the mount root's children
   drain as `extra`) was wrong: the walker never lists a mount root, so nothing below it exists on that side (section 3).
8. Edits and extras are side-parameterised (`Which`), so both asymmetric shapes (a directory on either side against a
   non-directory on the other, with children) are generated; the first draft edited the destination only and could not
   produce the shape of `a_type_mismatch_drains_the_children_as_extra_or_missing` (builder check, folded).
9. Two of the three originally named success-criterion mutants are equivalent mutants (section 2); the criterion now
   asks for two fixture-green mutants from a candidate list the plan measures.

## 8. Interfaces (names and shapes; bodies are the builder's)
- `FaultFs::fail_read_dir_of(&self, path: impl AsRef<Path>, code: Code, kind: std::io::ErrorKind)`.
- In `verify_proptest.rs`, which re-declares the few helpers it needs (`Row`, `row`, a `tree`-like builder, the `READS`
  list) because `verify::tests`'s are private to that module: `fn case() -> impl Strategy<Value = Case>`; `fn sides(case: &Case) -> (BTreeMap<PathBuf, Spec>,
  BTreeMap<PathBuf, Spec>)` (after edits, extras and parent pruning; BEFORE the walker view of 6.5 step 0, which the reference applies itself); `fn materialise(case: &Case) -> (FaultFs, usize)` (the
  fake and the setup call count); `fn reference(case: &Case) -> (Vec<Record>, Summary)`; `fn fixture_snippet(case: &Case)
  -> String`.

## 9. Tests (the oracle for each is this document or the cut 14 design; none may be edited to match the code)
1. `the_streaming_merge_equals_the_reference` (the property, 256 cases): 6.6's four assertions.
2. `fail_read_dir_of_fails_that_directory_only_and_logs_the_call` (in `fault_fs.rs`'s tests): `/d/x` armed; `read_dir(/d)`
   succeeds; `read_dir(/d/x)` fails with the code twice in a row; both calls appear in the log.
3. `generated_cases_reach_every_outcome_and_every_prune_arm`: over 512 cases from a FIXED seed
   (`TestRunner::new_with_rng(ProptestConfig::default(), TestRng::from_seed(RngAlgorithm::ChaCha, &[..32 bytes]))` and
   `case().new_tree(&mut runner).unwrap().current()` per case; `runner.rs:344`, `rng.rs:365`), the reference's output
   contains each of the five outcomes, a source-only fault, a destination-only fault, a both-sides fault, a fault on
   exactly one side of a path both hold, a mount answer `Yes` on exactly one side of a path both hold, a failed
   destination directory paired with a source file (`unreadable`, `kind: file`), a mount fold, a mount root on ONE side
   under `Stay` with children on that side (they vanish), a control-state drop (an entry present in `extras` and absent
   from the records), a type change on EACH side, a subtree on one side under a non-directory on the other, and a
   `Links::Skip` drop, each at least FIVE times (a bare "at least once" would not notice a weight collapse) (defeats the identical-trees failure mode; cut 14 design section 6.2 to 6.4 for what each is).
4. `the_reference_agrees_with_three_fixtures`: hand-built `Case`s mirroring `a_type_mismatch_drains_the_children_as_extra_or_missing`,
   `a_mount_point_on_either_side_is_matched_once_and_pruned` and
   `a_destination_only_directory_that_cannot_be_listed_is_extra_with_its_error_and_never_fails` give the reference the rows
   those fixtures assert; for the third, compare `(path, outcome, kind, code)` only, because that fixture's detail is the
   `fail_nth` message (`verify.rs:1090`, `contains("injected")`) while the primitive's text is the `io::ErrorKind`'s (guards the reference
   against the rules the fixtures already pin).
5. `a_source_file_against_a_destination_mount_point_is_a_mismatch_and_nothing_below_it_is_seen` (in `verify.rs`'s tests):
   `/s/m` a file, `/d/m` a directory with `MountRoot::Yes` holding a child `x`, `MountPolicy::Stay`: exactly one row, `m`
   `mismatched` `"source file, destination dir"`; `not_entered == 0` (cut 14 design 6.2: the fold applies to Dir/Dir only);
   `read_dir(/d/m)` was never called (the walker never lists a mount root), so `m/x` is neither `extra` nor anything else.
6. Non-vacuity (driver-measured, recorded in the plan's mutant table): the three `Side` mutants of section 2 each redden
   test 1 and leave the 42 fixtures green.

## 10. Left to the builder
Strategy composition (how `base` is grown so parents precede children), the weights' exact numbers within 6.3's guidance,
the snippet's formatting, helper naming inside the module.

## 11. Boundaries this cut records
- Version one excludes root faults, mid-file read faults, `mount_root`/`canonical_path` faults; the fixtures pin those.
- The reference is not an oracle of the design; a shared misreading stays green (section 1).
- The peer's proposed location `crates/flux-core/tests/property_verify.rs` is not possible: the fake is `cfg(test)`.
- The regression file is committed (proptest README, "The first thing we should do is add these to source control").

## 12. Stand-downs (adversarial panel, round 1: a seven-seat solo panel and the agy escalation)
- REJECTED, the panel's replacement mutants "`alone`'s `extra` arm not pruning the source side" and "`pair`'s failed arm
  not pruning the destination side": both equivalent by the argument in section 2 (the un-pruned side holds nothing under
  the path, or its walker already skipped the subtree); the section 2 candidates (h), (n), (d) replace them.
- DISCARDED-BELOW-FLOOR: the design's `walk.rs:404-418` range (the mount guard spans to 426); the citation was tightened to
  the lines actually relied on (412-418).
- DISCARDED-BELOW-FLOOR: the proptest RNG being OS-seeded per run (only a replayed seed is deterministic): true, folded as
  one sentence in 6.1, no design change.
- Round 2 (agy, on the final text; six seats, Blindspot Auditor rotated in; no findings): candidate (d) stood down as
  fixture-killed and replaced by (p) in section 2; `TestRng::deterministic_rng` noted as an equivalent shortcut to
  `from_seed` for test 3 (builder's choice). Feature set `["std"]` confirmed sufficient by both the peer and the author
  against proptest's `cfg` gates (`bit-set` gates only `src/bits.rs`; `fork` and `timeout` gate runner internals the
  design does not use; persistence is under `std`).

## 13. Exhaustiveness audit (author, before handoff)
Contracts named: the primitive's signature and semantics (6.2); `Case`, `Spec`, `Edit`, `Which` shapes (6.3); the
derivation of the two sides incl. parent pruning (6.3); the walker view of mount roots (6.5 step 0); every record field
for every arm incl. the kind of a failed pair (6.5); the four assertions and their order (6.6); configuration (6.7); file
paths and the Cargo change (6.1). Placeholders: none ("about one in four" weights are stated as tunable with test 3 as
the floor). Edge cases covered: mount root with a fault, mount root on one side with children, `Unknown` mount answer, a
changed-kind directory's children on either side, a failed destination directory against a source file, control-state
near-misses, empty and multi-buffer files, both policies for mounts and links. Builder check (one Sonnet reviewer, before
this handoff): three questions and two false claims, all folded above (calls 7 to 9). Solo panel (seven seats, Sonnet):
23 findings; 7 were already folded from the builder check, 13 folded here (the `kind` of a failed pair, mount-root
children, the success criterion, `File { len, fill }`, `RelPath`, dedupe, extra placement, one-sided faults in test 3,
minimum counts, the fixed-seed API, the env-inert config, default features and `Cargo.lock`, the licence header, the
`proptest!` macro, the `norm` lock order, helper visibility, the snippet's large files, test 4's detail), 3 stood down in
section 12. Open to the owner: the nine calls in section 7.
