# The `flux verify` command (cut 14) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux verify SOURCE DEST` compares a source tree (or file) with a destination without writing anything, hashes every paired file on both sides with BLAKE3, streams one record per path (`match`, `missing`, `mismatched`, `extra`, `unreadable`) and exits 0, 1 or 2.

**Architecture:** A new engine module `flux_core::verify` merges two `Walk` iterators (one per root) in lockstep by `FluxPathKey` bytes and emits each record to a sink closure as it is decided; nothing is collected. The CLI gains a `verify` subcommand, a `verify_report` module for the human lines and the NDJSON, and `exit_code::for_verify`. The copy engine's root-containment test and identity pre-flight are reused for the refused root pairs. No new dependency, no persisted format, no lock.

**Tech Stack:** Rust 2024 workspace; `flux-hash` (BLAKE3, already a `flux-core` dependency); clap derive; `serde`/`serde_json` (already in `flux-cli`); `FaultFs` (`crates/flux-core/src/fault_fs.rs`, test-only) for engine tests; `cargo nextest`; the gate is `just check`.

**Spec:** `docs/superpowers/specs/2026-10-26-verify-command-design.md` (owner-adopted 2026-10-26; panel round 2 ALIGNED). Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md` §4.2 (lines 649-690). Read the design first; this plan adds only what the design leaves to the builder plus the code citations, which were read at `022953b` (the tree is identical to `origin/main` `f19db29` outside `docs/`).

## Global Constraints

- Verify WRITES NOTHING: no `create_new`, `create_dir`, `create_symlink`, `create_hardlink`, `open_existing`, `rename_*`, `remove_file`, lock or claim-store call, ever (design 2 anti-goals, test 11). The engine tests assert it on the fake's call log.
- Records are streamed to the sink in path order as decided; the engine holds no list of records (design 6.6, master line 3578).
- The comparison key is `flux_fs::FluxPathKey::from_relative(rel)` (`crates/flux-fs/src/claims.rs:26-49`), compared as bytes. Never compare joined display strings (design 5).
- No size shortcut: both files are read in full before a verdict (design 6.5, V8). `Other` entries are never opened (design 6.3, V6).
- Exit codes: 0 when `missing + mismatched + unreadable == 0`; 1 otherwise; 2 for a usage error or a refused root pair (design 2, 6.1). `extra` never changes the exit code.
- `--json` is newline-delimited: one object per record, then one `{"summary": {...}}` object with exactly the fields `source, dest, hash, matched, missing, mismatched, extra, unreadable, not_entered, bytes_hashed, exit` (design 6.6, V4). Stdout carries nothing else under `--json`.
- Human records go to STDOUT (design 6.6), unlike `copy`'s stderr records. Stderr carries only operational lines.
- The flag that takes a value (`--links`, `--hash`) uses clap's default `--flag value` form as `copy` does (`main.rs:125-126`); `verify` has no bare-or-valued flag, so `require_equals` is not needed.
- New files carry the licence header of `header.txt` (the `license` gate: `just license`). Conventional commit subjects as in cut 13 (`feat(core): ...`, `feat(cli): ...`, `test: ...`, `docs: ...`); each commit ends with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>` (the model that makes the commit; Sonnet for the subagents this plan is written for).
- No push, no PR, no commit to `main`, never `just pr`. Work on `feat/verify-command` (Task 0).
- Every test is non-vacuous: the mutants listed in design section 9 (and the per-task ones below) must turn the named test red. The driver runs them; the builder's job is to make the tests specific enough that they can.

## Review Focus (failure modes the design implies; each has its test in the owning task)

1. Zero-length files: an empty file on both sides is `match` (BLAKE3 of zero bytes), an empty source against a 1-byte destination is `mismatched` with both sides READ (no size shortcut). Task 1.
2. A SOURCE directory named `.flux` (user data) when DEST holds its control `.flux`: the DEST side is filtered, so the source's is `missing`, never `match` (design 6.4). Task 2.
3. `--links=skip` with a source FILE and a DEST SYMLINK of the same name: the pair is `mismatched` (skip removes only source symlinks). Task 2.
4. Two empty trees: zero records, every count 0, exit 0, and no `open_read` call. Task 1.
5. A DEST root that exists but cannot be listed: exactly one record, path `.`, `unreadable`, exit 1, and the source is never hashed (`open_read` never called). Task 3.

## File map

- `crates/flux-core/src/verify.rs` (new; Tasks 1-4) and `crates/flux-core/src/lib.rs` (`pub mod verify;`).
- `crates/flux-core/src/tree.rs` is NOT edited: `containment`, `preflight` and `lexically_within` are `pub(crate)` already (`tree.rs:4351`, `4292`, `4391`).
- `crates/flux-cli/src/verify_report.rs` (new; Task 5), `crates/flux-cli/src/lib.rs` (`pub mod verify_report;`), `crates/flux-cli/src/exit_code.rs`, `crates/flux-cli/src/main.rs`.
- `crates/flux-cli/tests/verify.rs` (new; Tasks 5-6).
- `README.md`, `ROADMAP.md`, `TODO.md`, `site/index.html` (Task 6).

## Refinements of the design made by this plan (recorded here and in the design, same commit)

- R1. `verify_tree` is generic over `F: DestinationRoot` (not `FileSystem`): the reused containment test takes a `DestinationRoot` handle (`tree.rs:4351-4358`). `verify_file` stays `F: FileSystem`. `StdFileSystem` and `FaultFs` both implement `DestinationRoot` (`flux-platform/src/std_fs.rs:719` and `:734`, one per platform; `fault_fs.rs:1864`).
- R2. A partial artifact name is `<name>.flux-partial.<32 lowercase hex>` and nothing else: `crate::state::id_after(name, PARTIAL_INFIX)` (`state.rs:866-878`) is the parser the engine uses. Design test 9's literal `<n>.flux-partial.1` is therefore a USER file and is reported `extra`; the hidden one needs a 32-hex id (tests use `"0".repeat(32)`, as `state.rs:899` does).
- R3. The reporting functions live in a new module `flux_cli::verify_report` (the precedent is `cleanup_report.rs`), not in `report.rs` (2451 lines).
- R4. A file verify's single record has `path` = the destination's file name (`dst.file_name()`), the analogue of "relative to the root" when the root is DEST's parent.
- R5. Root identity: besides the containment test in both directions, the copy engine's identity pre-flight (`tree.rs:4292-4313`) is run so a bind-mount alias (same identity, different path) is refused too. Exact messages in Task 3.
- R6. The hashing buffer is one `vec![0u8; crate::copy::COPY_BUF_MAX]` (256 KiB, `copy.rs:185`) allocated once per `verify_tree`/`verify_file` call and reused for every file. The design's "1 MiB-class" wording meant "the copy's buffer"; the copy's maximum is 256 KiB.
- R7. With `--json`, a run that stops before any walk with exit 1 (missing SOURCE, symlink SOURCE) still prints the summary object (all counts 0, `exit: 1`), so a script always finds a summary. Exit 2 prints nothing on stdout (as clap's own usage errors print nothing there).

---

### Task 0: Branch and baseline

- [ ] **Step 1:** `git switch spec/verify-command && git switch -c feat/verify-command && git branch --unset-upstream` (the new branch must not track `origin`; `git branch -vv` shows no `[origin/...]` for it). `git status --short` must be empty and `git log --oneline -1` must show `022953b` or a later commit on this branch.
- [ ] **Step 2:** Record the baseline: `cargo nextest run --workspace --no-tests=pass --no-fail-fast`. Write the pass/skip counts in your report. Expected: all pass (the last figure on this tree was 1987 passed, 6 skipped; the exact number may differ).

### Task 1: The merge: two walkers, one record stream

**Files:** Create `crates/flux-core/src/verify.rs`; modify `crates/flux-core/src/lib.rs:22` (add `pub mod verify;` after `pub mod walk;`).

**Interfaces — Produces** (Tasks 2-5 consume exactly these):

```rust
// crates/flux-core/src/verify.rs
use flux_fs::{Code, DestinationRoot, FileSystem, HashAlgorithm, Links};
use crate::walk::MountPolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome { Match, Missing, Mismatched, Extra, Unreadable }
impl Outcome { pub fn as_str(self) -> &'static str }   // "match" | "missing" | "mismatched" | "extra" | "unreadable"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { File, Dir, Symlink, Other }
impl Kind { pub fn as_str(self) -> &'static str }      // "file" | "dir" | "symlink" | "other"

#[derive(Debug, Clone, PartialEq, Eq)]   // drop `Eq` if `flux_fs::Code` does not derive it
pub struct Record {
    /// Relative to the walk root; `.` for the root record (design 6.6). For a file verify, the destination's file name (R4).
    pub path: PathBuf,
    pub outcome: Outcome,
    /// The SOURCE side's kind when it exists, else the DEST side's (V9).
    pub kind: Kind,
    /// `Some(Code::VerifyMismatch)` for every `mismatched`; the error's code for `unreadable`; `None` otherwise.
    pub code: Option<Code>,
    /// Error text for `unreadable`; `"source {a}, destination {b}"` for a type mismatch; `"{s} != {d}"` (first 8 hex
    /// characters of each digest) for a file mismatch; `"symlink payloads differ"`; `MOUNT_NOT_ENTERED`; else empty.
    pub detail: String,
}

/// The detail of a `match` record for a directory that is a mount point neither walker entered (design 6.2).
pub const MOUNT_NOT_ENTERED: &str = "mount point not entered";
/// The detail of an `unreadable` record for two `Other` entries (design 6.3).
pub const SPECIAL_NOT_COMPARED: &str = "special file not compared";

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Summary {
    pub matched: u64, pub missing: u64, pub mismatched: u64, pub extra: u64, pub unreadable: u64,
    /// Mount points not entered (each is also one `matched`).
    pub not_entered: u64,
    /// Bytes read by completed hashes on both sides; a read that failed adds nothing.
    pub bytes_hashed: u64,
}
impl Summary { pub fn failed(&self) -> bool }  // missing + mismatched + unreadable > 0

#[derive(Debug, Clone, Copy)]
pub struct VerifyOptions { pub links: Links, pub mounts: MountPolicy, pub hash: HashAlgorithm }

#[derive(Debug)]
pub enum VerifyError {
    /// Exit 2: the two roots are the same object, or one lies inside the other; nothing was read (design 6.1).
    RefusedRoots { reason: String },
}

pub fn verify_tree<F: DestinationRoot>(
    fs: &F, src: &Path, dst: &Path, opts: &VerifyOptions, sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError>;
```

In this task `verify_tree` does NOT yet check the roots (Task 3) or handle an absent DEST (Task 3): both roots exist and are directories. Symlinks and `Other` entries are paired in Task 2; in Task 1 they may be treated as `File` for the merge (the tests below use only files and directories).

**The algorithm** (the design fixes it in 6.2; the data structure is the builder's, these are the decisions it must honour):

- One `Side` per root wraps `walk_with(fs, root, WalkOptions { max_depth: DEFAULT_MAX_DEPTH, mounts: opts.mounts })` (`walk.rs:199-203`) and yields `Item { path: PathBuf, key: Vec<u8>, kind: Kind, not_entered: bool, failed: Option<flux_fs::FsError> }`:
  - `WalkEvent::DirEnd` is discarded (`walk.rs:52-54`). `MountUnknown` is discarded. `MountSkipped { path }` follows its own `Dir { path, .. }` immediately (`walk.rs:273-280`, `414-421`): the side keeps one event of lookahead after a `Dir` and sets `not_entered = true` when it is `MountSkipped` for the same path.
  - An `Err(WalkError { path, cause })` (`walk.rs:98-102`) is an item too: `kind: Dir`, `failed: Some(cause)`, keyed by its path, so it merges in order like any entry.
  - `key` is `FluxPathKey::from_relative(&path)?.0`; a `from_relative` error (a NUL byte, `claims.rs:32-40`) makes the item `failed: Some(that error)` with `kind` from the event.
  - `prune(path)`: later items whose `path.starts_with(&pruned)` are skipped; the item the merge currently HOLDS from that side is discarded too when it lies under the pruned path.
- The merge holds the next item of each side and loops: both `None` ends. One side `None`: the other's item is `missing` (source only) or `extra` (DEST only), advance it. Else compare keys as bytes: `<` → source item alone (`missing`), `>` → DEST item alone (`extra`), `==` → a pair, advance both.
- An item ALONE with `failed: Some(e)` is `unreadable` (code `e.code`, detail `e.source.to_string()`) and prunes BOTH sides at its path (design 6.2, V7). A pair with either side failed: one `unreadable` record (the source's error if it failed, else DEST's), prune both.
- A pair `Dir`/`Dir`: `match` (kind dir). If either `not_entered`: detail `MOUNT_NOT_ENTERED`, `summary.not_entered += 1`, prune both at that path (Task 3 tests it; implement the branch now).
- A pair `File`/`File`: `compare_files` (below). Any other pair of different kinds: `mismatched`, code `VerifyMismatch`, detail `format!("source {}, destination {}", a.as_str(), b.as_str())`, no open, no prune (the children drain as `missing`/`extra` by the key order, design 6.3).
- `compare_files(fs, src_root.join(path), dst_root.join(path), buf)`: `fs.open_read(src)` then `flux_hash::hash_reader(&mut reader, buf, &mut || Ok(()))` (`flux-hash/src/lib.rs:54-73`); an `FsError` from open → `unreadable` with its code and text; an `io::Error` from the hash → `unreadable` with code `FsError::classify(&e)` (`flux-fs/src/error.rs:131`) and `e.to_string()`. Source first; DEST is not opened when the source failed. Equal digests → `match`; else `mismatched`, code `VerifyMismatch`, detail `format!("{} != {}", &s.to_hex()[..8], &d.to_hex()[..8])`. `bytes_hashed` adds each completed read's count.
- Every record passes through ONE function that increments the matching `Summary` counter and calls the sink.

- [ ] **Step 1: Write the failing tests** in `mod tests` at the bottom of `verify.rs`. Fixture helpers (write them once; every later task reuses them):

```rust
use crate::fault_fs::FaultFs;
use flux_fs::{Links, HashAlgorithm};
use crate::walk::MountPolicy;

fn opts() -> VerifyOptions { VerifyOptions { links: Links::Copy, mounts: MountPolicy::Cross, hash: HashAlgorithm::Blake3 } }

/// `fs.create_dir` for every directory (the fake lists only what `create_dir` made, `fault_fs.rs:1782-1809`), then the files.
fn tree(fs: &FaultFs, root: &str, dirs: &[&str], files: &[(&str, &[u8])]) { ... }

/// Run and collect `(path as "/"-joined components, outcome, kind, code, detail)` tuples plus the summary.
fn run(fs: &FaultFs, src: &str, dst: &str, o: &VerifyOptions) -> (Vec<(String, Outcome, Kind, Option<Code>, String)>, Summary) { ... }

/// Design test 11: every logged call is a READ. The allow-list is the fake's read-side call names
/// (`fault_fs.rs`: `metadata(`, `read_dir(`, `open_read(`, `read_link(`, `source_mount_root(`, `canonical_path(`,
/// `identity(`); extend it ONLY with a name you have confirmed in `fault_fs.rs` performs no mutation.
fn assert_no_writes(fs: &FaultFs) { for c in fs.calls() { assert!(READS.iter().any(|r| c.starts_with(r)), "{c}"); } }
```

Tests (names are the oracle for the mutant runs; keep them):

```rust
#[test] fn identical_trees_match_and_exit_clean()             // design 9.1: /s and /d each `a`, `sub/`, `sub/b`; records all Match in order [a, sub, sub/b]; summary matched 3, others 0, bytes_hashed = 2 * (len a + len b); !summary.failed(); assert_no_writes
#[test] fn a_byte_in_the_last_buffer_is_a_mismatch()          // design 9.2: one file of COPY_BUF_MAX + 5 bytes, DEST differs in the LAST byte; one Mismatched record naming the path, code Some(VerifyMismatch), detail is 8 hex, "!=", 8 hex; summary.failed()
#[test] fn missing_and_extra_files_and_directories()          // design 9.3: source-only file `m`, source-only dir `md/` with `md/x`; DEST-only file `e`, DEST-only dir `ed/` with `ed/y`; records exactly [e Extra, ed Extra, ed/y Extra, m Missing, md Missing, md/x Missing] in key order; `open_read` was NOT called for md/x or ed/y (fs.calls())
#[test] fn a_type_mismatch_drains_the_children_as_extra_or_missing() // design 9.4 + the panel's trace: source `a/` + `a/x` + file `b`; DEST file `a` + dir `b/` + `b/y`. Records exactly [a Mismatched(kind dir, "source dir, destination file"), a/x Missing, b Mismatched(kind file, "source file, destination dir"), b/y Extra]; no open_read on a or b
#[test] fn component_wise_order_is_not_a_bytewise_sort()      // design 9.5, ASYMMETRIC on purpose: source `a/` + `a/b` + `a.txt` + `a-b`; DEST `a/` + `a.txt` + `a-b` (no a/b). Records exactly [a Match, a/b Missing, a-b Match, a.txt Match] (siblings sort bytewise, `-` 0x2d before `.` 0x2e: `walk.rs:161-163`; the first draft listed the last two swapped). A comparator on the joined string puts `a.txt` before `a/b` and reports a spurious Extra a.txt + Missing a.txt.
#[test] fn records_stream_before_the_walk_is_exhausted()      // design 9.12: source `a`, `z/`, `z/q`; DEST `a` only; `fs.fail("read_dir", Code::IoError)` is NOT usable (it hits the first read_dir), so instead the sink asserts on its FIRST record that `!fs.called("read_dir(/s/z)")`; after the run `fs.called("read_dir(/s/z)")`
#[test] fn empty_files_match_and_an_empty_against_a_byte_is_read_on_both_sides() // Review Focus 1: `e` empty both sides → Match; `f` empty vs b"x" → Mismatched, and `open_read(/d/f)` WAS called (no size shortcut)
#[test] fn two_empty_trees_give_no_records()                  // Review Focus 4: zero records, Summary::default(), no `open_read(` call
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast`
Expected: compile error (the module does not exist). After adding the module skeleton with `todo!()` bodies, every test FAILS.

- [ ] **Step 3: Implement** `verify.rs` as specified above, and add `pub mod verify;` to `crates/flux-core/src/lib.rs`.

- [ ] **Step 4: Run the tests and the lint**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast && cargo clippy -p flux-core --all-targets -- -D warnings && cargo fmt --check`
Expected: 8 passed; clippy clean; fmt clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/verify.rs crates/flux-core/src/lib.rs
git commit -m "feat(core): merge two walks into one stream of verify records"
```

### Task 2: Symlinks, special files, read errors, the control-state filter

**Files:** Modify `crates/flux-core/src/verify.rs`.

**Interfaces — Consumes:** Task 1's types. **Produces:** no new public names; `VerifyOptions.links` now takes effect.

Rules (design 6.3, 6.4, 6.7):
- Pair `Symlink`/`Symlink`: `fs.read_link` both (`fs.rs:238`), source first; an error → `unreadable` with its code and text, the other side not read. Compare `payload.as_encoded_bytes()` only (not `kind`): equal `match`, else `mismatched`, code `VerifyMismatch`, detail `"symlink payloads differ"`.
- Pair `Other`/`Other`: `unreadable`, code `Code::SpecialFileUnsupported`, detail `SPECIAL_NOT_COMPARED`; nothing opened. `Other` against another kind: `mismatched` as any type difference. `Other` alone: `missing`/`extra`.
- `Links::Skip`: the SOURCE side drops `Symlink` items (not compared, not reported, not counted). The DEST side never drops a symlink.
- Control state (design 6.4), DEST side only, applied in the DEST `Side` before an item is yielded: a top-level `.flux` directory is pruned (its own `Dir` item and everything under it, by the first path component equal to `crate::state::FLUX_DIR`, `state.rs:843`, bytes-exact); the ROOT-level file `.flux-root.lock` (`crate::lock::site::ROOT_LOCK_NAME`, `lock/site.rs:18`; the path has exactly one component) is dropped; any `File` or `Symlink` item, at any depth, whose file name satisfies `crate::state::id_after(name, crate::state::PARTIAL_INFIX).is_some()` (`state.rs:866-878`) is dropped. Nothing else is dropped; the source side is never filtered.

- [ ] **Step 1: Write the failing tests** (same helpers; `fs.add_symlink_to(path, payload, LinkKind::Unspecified)` seeds a link, `fs.add_special(path)` an `Other`; `fault_fs.rs:1359-1386`; `fs.fail_read_nth(path_contains, 1, ErrorKind::Other)` fails the first `read` on a reader whose path contains the text, `fault_fs.rs:1444`; `fs.fail_kind("open_read", code, kind)` fails the next `open_read`, `fault_fs.rs:1108`; `fs.fail("read_dir", code)` fails the next `read_dir` — order the fixture so the failing call is the one you mean, and assert which path failed from the record):

```rust
#[test] fn symlink_payloads_equal_differ_and_against_a_file()   // design 9.6: `l1` same payload both → Match(kind symlink); `l2` payloads differ → Mismatched "symlink payloads differ"; `l3` source symlink vs DEST file → Mismatched "source symlink, destination file"; no open_read on any of them
#[test] fn links_skip_drops_source_symlinks_and_keeps_a_dest_only_one_as_extra() // design 9.6 + Review Focus 3: with Links::Skip: source symlink `s` (DEST also has `s`) → NO record for `s` from the source and DEST's `s` is Extra; DEST-only symlink `d` → Extra; source FILE `f` vs DEST SYMLINK `f` → Mismatched (skip touches only source symlinks)
#[test] fn special_files_are_never_opened()                      // design 9.7: `o1` Other both → Unreadable(code SpecialFileUnsupported, detail SPECIAL_NOT_COMPARED); `o2` source Other vs DEST file → Mismatched; `o3` source Other only → Missing; `o4` DEST Other only → Extra; `!fs.calls().iter().any(|c| c.starts_with("open_read(") && c.contains("/o"))`
#[test] fn an_unreadable_file_on_either_side_is_unreadable_and_the_other_side_is_not_guessed() // design 9.8: `a`: fail_read_nth("/s/a", 1, Other) → Unreadable with code = FsError::classify of that error, detail non-empty, and `open_read(/d/a)` NOT called; `b`: fail_kind("open_read", Code::PermissionDenied, PermissionDenied) armed so it hits `open_read(/d/b)` (the source `b` was opened first and succeeded) → Unreadable code PermissionDenied; `c` normal → Match; bytes_hashed counts only completed reads (len of /s/b + both reads of c)
#[test] fn an_unreadable_directory_prunes_both_sides_and_siblings_continue() // design 9.8: source `bad/` + `bad/x` + `good/` + `good/y`; DEST the same plus `bad/z`; fail the read_dir of /s/bad: `verify_tree` constructs the source walk, then the DEST walk, then pulls the SOURCE head before the DEST head (fix both orders in the code), so the calls are read_dir(/s), read_dir(/d), read_dir(/s/bad): `fail_nth("read_dir", 3, Code::PermissionDenied, ErrorKind::PermissionDenied)`; assert from the record that `bad` is the path that failed; records exactly [bad Unreadable(kind dir), good Match, good/y Match]; neither bad/x nor bad/z is reported
#[test] fn control_state_at_the_destination_root_is_never_reported() // design 9.9 (R2): DEST has `.flux/operations/<32 zeros>/state`, `.flux-root.lock`, `a.flux-partial.<32 zeros>` (file), `l.flux-partial.<32 zeros>` (symlink), plus user files `.flux-partial.txt`, `x.flux-partial.1`, `sub/.flux` (a FILE named .flux, nested) and `sub/b.flux-partial.<32 zeros>`; source has only `sub/`. Records exactly: [.flux-partial.txt Extra, sub Match, sub/.flux Extra, x.flux-partial.1 Extra]. Also assert no `read_dir(/d/.flux` call (the subtree is pruned, not listed)
#[test] fn a_source_dot_flux_directory_is_missing_when_dest_has_only_control_state() // Review Focus 2: source `.flux/` + `.flux/data`; DEST `.flux/operations/...`; records [.flux Missing, .flux/data Missing]
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast`
Expected: the 7 new tests FAIL (symlinks compared as files, controls reported as extra); Task 1's 8 still pass.

- [ ] **Step 3: Implement** the rules above in `verify.rs`.

- [ ] **Step 4: Run the tests and the lint**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast && cargo clippy -p flux-core --all-targets -- -D warnings && cargo fmt --check`
Expected: 15 passed; clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): symlinks, special files, read errors and the control-state filter in verify"
```

### Task 3: Roots: refused pairs, an absent destination, an unlistable root, mount points

**Files:** Modify `crates/flux-core/src/verify.rs`.

**Interfaces — Consumes:** `crate::tree::{containment, preflight, lexically_within}` (`tree.rs:4351-4384`, `4292-4313`, `4391-4397`; all `pub(crate)`), `crate::tree::TreeOutcome` (`Default`, fields `warnings`, `containment_degraded`: `tree.rs:39-75`), `flux_fs::Safety::Default` (`options.rs:61-67`), `flux_fs::DirHandle::identity` (`fs.rs:303`). **Produces:** `VerifyError::RefusedRoots` is now returned; the sink may now receive the root record `.`.

**The root check**, run by `verify_tree` BEFORE any `read_dir` or `open_read` (design 6.1, V12; R5):

1. `fs.destination_root(dst)` (`fs.rs:539`): `Err` with kind `NotFound` → DEST is ABSENT: if `lexically_within(dst, src)` refuse with reason A below; else continue with an EMPTY destination side (every source item is `missing`; no DEST walk is created). Any other `Err` → the root record `.` `unreadable` (step 5). `Ok(d)` → continue.
2. `let s = fs.destination_root(src)`; `Err` → root record `.` `unreadable`.
3. Identity: call `s.identity()` and `d.identity()` (each `Result<FileIdentity, FsError>`); an `Err` from either → the root record `.` `unreadable` (step 5), NOT `?` (`VerifyError` has no `From<FsError>`; it carries only `RefusedRoots`). With both `Ok`: `preflight(si, di, Safety::Default, &mut out.warnings)` with a fresh `TreeOutcome`; its `Err(_)` (a `CopyError`) → refuse with reason A. (`preflight` only errs on two equal `Strong` identities under `Safety::Default`, `tree.rs:4298-4312`.)
4. Containment, direction 1: `containment(fs, src, &d, dst, Safety::Default, &mut out)`; `Err(e) if e.code() == Code::SafetyRejected` → refuse, reason A; other `Err(e)` → root record `.` `unreadable` with `e.cause`. Direction 2: `containment(fs, dst, &s, src, Safety::Default, &mut out)`; `Err` SafetyRejected → reason B; other → `.` unreadable. Then, if `out.containment_degraded.is_some()` (a canonical path was unsupported or an identity was not confirmed; `tree.rs:4360-4374`, `4380-4382`), run the lexical floor on the CLI's canonical paths: `lexically_within(dst, src)` → reason A + suffix; `lexically_within(src, dst)` → reason B + suffix.
5. The root record: `Record { path: ".", outcome: Unreadable, kind: Dir, code: Some(e.code), detail: e.source.to_string() }`, `summary.unreadable = 1`, return `Ok(summary)` without walking. Also used when `walk_with` itself fails for SOURCE or DEST (`walk.rs:208-215`): the error names which root in its detail: prefix `"source: "` or `"destination: "`.

Exact messages (`VerifyError::RefusedRoots { reason }`; the CLI prints `error: {reason}` and exits 2):
- Reason A: `the destination is the source or lies inside it; nothing was compared`
- Reason B: `the source lies inside the destination; nothing was compared`
- Suffix (degraded, lexical test decided): ` (decided by path alone: a root's identity could not be confirmed)`

Which test fires: with both canonical paths available, `containment`'s own lexical test on the HANDLES' canonical paths (`tree.rs:4377`) decides, whether or not identities were confirmed; when either `canonical_path` is unsupported (`tree.rs:4369-4374`), `containment` decides nothing and verify's lexical test on the CLI paths decides, with the suffix. Equal identities are caught by `preflight` before either.

**Mount points** (design 6.2, 6.6; the branch exists since Task 1): `MountPolicy::Stay` makes the walker ask `mount_root` of every directory below the root (`walk.rs:409-426`); a `Dir`/`Dir` pair with `not_entered` on either side records ONE `match` with detail `MOUNT_NOT_ENTERED`, adds 1 to `not_entered`, and prunes both sides.

- [ ] **Step 1: Write the failing tests** (`fs.set_identity(path, FileIdentity)`, `fs.set_canonical_path(dir, canonical)`, `fs.fail_canonical_path_of(dir, code, kind)`, `fs.set_mount_root(dir, MountRoot::Yes)`: `fault_fs.rs:1281-1312`; `FaultFs::destination_root` needs the directory to exist, `fault_fs.rs:1866-1897`; `canonical_path` defaults to the directory's own path, `fault_fs.rs:1971-1978`):

```rust
fn refused(fs: &FaultFs, src: &str, dst: &str) -> String  // runs verify_tree, expects Err(RefusedRoots{reason}), asserts no `read_dir(` and no `open_read(` call, returns the reason
#[test] fn dest_equal_to_source_is_refused_before_any_read()        // design 9.13c: /s with a file; refused(/s, /s) == reason A
#[test] fn dest_inside_source_and_source_inside_dest_are_refused()  // /s, /s/inner: refused(/s, /s/inner) == A; refused(/s/inner, /s) == B
#[test] fn an_alias_of_the_source_is_refused_by_path_or_by_identity() // /alias dir with set_canonical_path("/alias", "/s") → A (the lexical test on canonical paths); a second fake with /alias2 given `fs.metadata("/s").identity` via set_identity and NO canonical override → A (preflight, identity)
#[test] fn a_degraded_root_falls_back_to_the_lexical_test_and_says_so() // fail_canonical_path_of("/s/inner", Code::IoError, ErrorKind::Unsupported): refused(/s, /s/inner) == A + suffix; and with /d unrelated plus the same fault on /d: Ok, all Match (no refusal)
#[test] fn siblings_with_a_common_prefix_are_not_refused()          // /d/a vs /d/ab, both with one equal file: Ok, 1 matched
#[test] fn an_absent_destination_is_an_empty_one_and_nothing_is_created() // design 9.10: DEST /nope absent; source `a`, `sub/`, `sub/b`; records [a Missing, sub Missing, sub/b Missing]; summary.failed(); assert_no_writes; !fs.exists("/nope")
#[test] fn an_absent_destination_inside_the_source_is_still_refused()   // /s vs /s/nope (absent): refused == A
#[test] fn an_unlistable_destination_root_is_one_unreadable_record()  // Review Focus 5: `verify_tree` constructs the SOURCE walk first, then the DEST walk (fix that order in the code), and the root check makes no `read_dir` call, so `read_dir(/d)` is the 2nd `read_dir`: `fail_nth("read_dir", 2, Code::PermissionDenied, ErrorKind::PermissionDenied)`; records exactly [. Unreadable(kind dir, code PermissionDenied, detail starting "destination: ")]; summary.unreadable 1; no `open_read(` call
#[test] fn a_mount_point_on_either_side_is_matched_once_and_pruned()  // design 9.13b: opts.mounts = Stay; source `m/` + `m/x`, DEST `m/` (set_mount_root("/d/m", Yes)); records exactly [m Match(detail MOUNT_NOT_ENTERED)]; summary matched 1, not_entered 1, missing 0; and the mirror (mount on /s/m, DEST lists m/x) gives the same records with extra 0
#[test] fn a_mount_point_present_on_one_side_only_is_not_a_match()    // Stay; source `m/` as mount (set_mount_root("/s/m", Yes)), DEST has no m: records [m Missing]; not_entered 0
#[test] fn cross_filesystems_enters_mount_points()                     // opts.mounts = Cross with set_mount_root("/d/m", Yes): records [m Match (empty detail), m/x Match or Missing per fixture]; not_entered 0
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast`
Expected: the 11 new tests FAIL; the earlier 15 pass (they use unrelated roots).

- [ ] **Step 3: Implement** the root check, the empty destination and the root record. Note that `containment` and `preflight` return `CopyError`; use `e.code()` (`copy.rs:158-160`) and `e.cause` (`copy.rs:122-125`).

- [ ] **Step 4: Run the tests and the lint**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast && cargo clippy -p flux-core --all-targets -- -D warnings && cargo fmt --check`
Expected: 26 passed; clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): refuse nested or identical roots, treat an absent destination as empty, prune mount points"
```

### Task 4: `verify_file`

**Files:** Modify `crates/flux-core/src/verify.rs`.

**Interfaces — Produces:**

```rust
/// One source file against one destination path (the CLI has already applied §4.1: `dst` is `DEST` or `DEST/<name>`).
/// Exactly one record, whose `path` is `dst.file_name()` (R4). No root check (design 6.1: "a FILE verify is not subject").
pub fn verify_file<F: FileSystem>(
    fs: &F, src: &Path, dst: &Path, opts: &VerifyOptions, sink: &mut dyn FnMut(Record),
) -> Result<Summary, VerifyError>;
```

Rules: `fs.metadata(src)` (lstat, `fs.rs:172`): `Err` → `unreadable` (kind `file`, the error). `fs.metadata(dst)`: `Err` kind `NotFound` → `missing` (kind = source's); other `Err` → `unreadable`. Both present: the design 6.3 table on the pair of `FileType`s (`fs.rs:36-41`) through the SAME compare code Task 1 and 2 wrote (`File`/`File` hashes both, `Other`/`Other` is `unreadable` SPECIAL_NOT_COMPARED, different kinds `mismatched`). `opts.links` is ignored here (a symlink SOURCE never reaches the engine, K7, `resolve.rs:42-44`). The result is always `Ok`.

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn verify_file_matches_mismatches_and_misses()      // /s/a vs /d/a equal → [a Match], bytes_hashed 2*len; differing → [a Mismatched, detail 8hex != 8hex]; /d/a absent → [a Missing]; assert_no_writes each
#[test] fn verify_file_against_a_directory_or_a_special_is_a_mismatch_or_unreadable() // dst a dir → Mismatched "source file, destination dir"; src Other (add_special) vs dst Other → Unreadable SPECIAL_NOT_COMPARED and no open_read
#[test] fn verify_file_reports_a_source_read_error_without_opening_the_destination() // fail_read_nth("/s/a", 1, Other) → Unreadable; !fs.called("open_read(/d/a)")
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast`
Expected: compile error (`verify_file` undefined), then 3 FAIL with a stub.

- [ ] **Step 3: Implement** `verify_file`, sharing the pair-comparison function with `verify_tree`.

- [ ] **Step 4: Run the tests and the lint**

Run: `cargo nextest run -p flux-core verify::tests --no-fail-fast && cargo clippy -p flux-core --all-targets -- -D warnings && cargo fmt --check`
Expected: 29 passed; clean.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/verify.rs
git commit -m "feat(core): verify_file for a single source file"
```

### Task 5: The `verify` subcommand

**Files:** Create `crates/flux-cli/src/verify_report.rs`, `crates/flux-cli/tests/verify.rs`; modify `crates/flux-cli/src/lib.rs:11-14` (add `pub mod verify_report;`), `crates/flux-cli/src/exit_code.rs`, `crates/flux-cli/src/main.rs` (`Commands` at `:30-51`, `main` at `:224-229`, a new `fn verify(args: &VerifyArgs) -> u8` beside `cleanup` at `:681-706`).

**Interfaces — Consumes:** `flux_core::verify::{verify_tree, verify_file, Record, Summary, Outcome, Kind, VerifyOptions, VerifyError, MOUNT_NOT_ENTERED}`; `flux_core::walk::MountPolicy`; `flux_cli::resolve::{job, Job, Stop}` (`resolve.rs:19-70`); `report::SYMLINK_WHY` (`report.rs:23-24`); `report::rel` (`report.rs:286-289`); `flux_platform::StdFileSystem` (`main.rs:410`); `err()` (`main.rs:716-718`).

**Produces:**

```rust
// crates/flux-cli/src/exit_code.rs
/// `flux verify` (cut 14): 1 when anything is missing, mismatched or unreadable (master lines 681-684); extras never count.
pub fn for_verify(s: &flux_core::verify::Summary) -> u8;   // s.failed() → FAILED else SUCCESS

// crates/flux-cli/src/verify_report.rs  (pure: returns text; main writes it)
pub fn record_line(r: &Record) -> String;        // "{outcome}: {path}" + ": {CODE}" when code is Some + ": {detail}" when detail is non-empty; path via report::rel
pub fn record_json(r: &Record) -> String;        // one line: {"path":"..","outcome":"..","kind":"..","code":null|"VERIFY_MISMATCH","detail":".."} (serde; key order as listed)
pub fn summary_line(s: &Summary) -> String;      // "verified: {matched} matched, {missing} missing, {mismatched} mismatched, {extra} extra, {unreadable} unreadable; {bytes_hashed} bytes hashed" + "; {not_entered} mount points not entered" when > 0
pub fn summary_json(source: &Path, dest: &Path, hash: &'static str, s: &Summary, exit: u8) -> String; // {"summary":{"source":..,"dest":..,"hash":"blake3","matched":..,"missing":..,"mismatched":..,"extra":..,"unreadable":..,"not_entered":..,"bytes_hashed":..,"exit":N}} (paths via Path::display)
pub fn mount_notice_line(path: &Path) -> String; // "mount point not entered: {path} (use --cross-filesystems to verify its contents)"
pub fn dest_absent_line(dest: &Path) -> String;  // "destination does not exist: {dest} (every source path is reported missing)"
pub const GUARANTEES: &str;                      // the design 6.8 text, verbatim, for --help's long_about (one paragraph per bullet)
```

```rust
// crates/flux-cli/src/main.rs
#[derive(Args)]
struct VerifyArgs {
    /// The source file or folder.
    source: PathBuf,
    /// The destination to compare it with (§4.1's mapping).
    dest: PathBuf,
    /// `copy` (the default) compares a symlink's payload; `skip` leaves source symlinks out.
    #[arg(long, value_enum, default_value_t = LinksArg::Copy)]
    links: LinksArg,
    /// Also compare the contents of directories that are other mounts; by default each is reported as present and not entered.
    #[arg(long)]
    cross_filesystems: bool,
    /// The hash used to compare files.
    #[arg(long, value_enum, default_value_t = HashArg::Blake3)]
    hash: HashArg,
    /// One JSON object per record on stdout, then one {"summary": ...} object.
    #[arg(long)]
    json: bool,
    /// Also print each `match` record (the default prints only what is not a match).
    #[arg(long)]
    verbose: bool,
}
// Commands::Verify(VerifyArgs) with doc comment = design 6.8 (`#[command(long_about = verify_report::GUARANTEES)]` or the doc comment carrying the same text).
```

`fn verify(args: &VerifyArgs) -> u8`, in this order (design 6.1, 6.6; R7):
1. `resolve::job(&args.source, &args.dest)`: `Stop::Usage(msg)` → `err("error: {msg}")`, return `USAGE` (nothing on stdout). `Stop::SymlinkSource` → the same stderr line `copy` prints (`main.rs:386-392`), then with `--json` the summary object (zeros, `exit: 1`, `source`/`dest` = the arguments as given, since nothing was resolved), return `FAILED`. `Stop::Failed(line)` → `err(&line)`, the same JSON rule, `FAILED`.
2. `opts = VerifyOptions { links, mounts: if cross_filesystems { Cross } else { Stay }, hash: Blake3 }`.
3. The sink, shared by both jobs: for each record, if `args.json` → `println!(record_json)`; else if `outcome != Match || args.verbose` → `println!(record_line)`. If `record.detail == MOUNT_NOT_ENTERED` → `err(&mount_notice_line(&record.path))` (stderr, always). Use `writeln!(std::io::stdout(), ..)` with the result ignored, as `main.rs:722` does.
4. `Job::Tree { src, dst }`: if `std::fs::metadata(&dst)` is `Err` of kind `NotFound` → `err(&dest_absent_line(&dst))` BEFORE calling the engine. Then `verify_tree(&StdFileSystem, &src, &dst, &opts, &mut sink)`: `Err(VerifyError::RefusedRoots { reason })` → `err("error: {reason}")`, return `USAGE`. `Ok(summary)` → step 6.
5. `Job::File { src, dst, .. }` → `verify_file(&StdFileSystem, &src, &dst, &opts, &mut sink)` → `Ok(summary)` → step 6.
6. `let code = exit_code::for_verify(&summary)`; `--json` → `println!(summary_json(&src, &dst, "blake3", &summary, code))`; else `println!(summary_line(&summary))`. Return `code`.

- [ ] **Step 1: Write the failing unit tests** in `verify_report.rs` (`mod tests`), `exit_code.rs` and `main.rs`'s `mod tests` (reuse the `parse_cleanup` shape at `main.rs:743-748` as `parse_verify`):

```rust
// exit_code.rs
#[test] fn verify_exits_1_only_for_missing_mismatched_or_unreadable()  // Summary::default() → 0; extra: 5 → 0; each of missing/mismatched/unreadable = 1 → 1; not_entered: 2 alone → 0
// verify_report.rs
#[test] fn a_record_line_names_the_outcome_the_path_the_code_and_the_detail() // Mismatched sub/b VerifyMismatch "ab12cd34 != ef56ab78" → "mismatched: sub/b: VERIFY_MISMATCH: ab12cd34 != ef56ab78"; Match "a" → "match: a"; Missing "." → "missing: ."
#[test] fn a_record_json_has_exactly_the_five_keys_in_order()           // parse with serde_json; keys == ["path","outcome","kind","code","detail"]; code is null for a Match and "VERIFY_MISMATCH" for a Mismatched
#[test] fn a_summary_json_has_exactly_the_eleven_fields()                // keys of ["summary"] == the design 6.6 list in order; exit copied
#[test] fn the_summary_line_mentions_not_entered_only_when_non_zero()
// main.rs
#[test] fn verify_parses_its_flags_and_needs_exactly_two_paths()        // ["flux","verify","s","d"] → defaults (links Copy, !cross, !json, !verbose); with "--links","skip","--cross-filesystems","--json","--verbose" → set; ["flux","verify","s"] and ["flux","verify","s","d","e"] → Cli::try_parse_from is Err; ["flux","verify","s","d","--resume"] → Err; ["flux","verify","s","d","--exclude","x"] → Err
```

- [ ] **Step 2: Write the failing CLI tests** in `crates/flux-cli/tests/verify.rs` (copy the helpers `flux()`, `stderr()` from `tests/copy.rs:11-17`; add `stdout(out)`, `ndjson(out) -> Vec<serde_json::Value>` that parses EVERY stdout line and panics on one that does not parse, and `tree_in(d)` from `copy.rs:24-30`):

```rust
#[test] fn exit_0_for_identical_trees_and_with_extras_only()      // design 9.14: copy the tree by std::fs into dst; verify → 0; add dst/extra → still 0, stdout has "extra: extra"
#[test] fn exit_1_for_missing_mismatched_and_unreadable()         // remove dst/a → 1 + "missing: a"; flip a byte in dst/sub/b → 1 + "mismatched: sub/b: VERIFY_MISMATCH"; `#[cfg(unix)]` leg: `mkfifo` (via std::process::Command) a FIFO `p` in BOTH trees → 1 + "unreadable: p: SPECIAL_FILE_UNSUPPORTED: special file not compared", and the command returns (a FIFO that was opened would block forever: give the child a 10 s timeout by polling `try_wait`, and fail the test on expiry)
#[test] fn exit_2_for_a_folder_onto_a_file_an_unknown_flag_and_resume() // design 9.14: dst a FILE → 2 with "error:" on stderr and EMPTY stdout; `--exclude x` → 2; `--resume` → 2
#[test] fn exit_2_for_nested_or_identical_roots_with_nothing_on_stdout() // verify src src → 2, stderr contains "nothing was compared"; verify src src/sub → 2; verify src/sub src → 2; stdout empty each time
#[test] fn an_absent_destination_prints_the_leading_line_and_every_path_missing() // design 9.10 (CLI half): stderr contains dest_absent_line; stdout lines are "missing: a", "missing: sub", "missing: sub/b", then the summary line; exit 1; the dst path still does not exist
#[test] fn human_output_hides_matches_unless_verbose()             // design 9.13d: default stdout is just the summary line for identical trees; --verbose adds "match: a", "match: sub", "match: sub/b" before it
#[test] fn ndjson_has_a_record_per_path_and_a_final_summary()      // design 9.15: --json on identical trees; every line parses; lines[..n] have keys exactly [path,outcome,kind,code,detail] and include outcome "match" records; the last has "summary" with the 11 keys, exit 0, matched 3; `--verbose --json` output is byte-identical to `--json`
#[test] fn a_file_source_maps_like_copy()                          // file vs file → 0 "match"; file into an existing folder → compares DEST/<name>; file vs absent → 1 "missing: <name>"
#[test] fn a_symlink_source_is_refused_like_copy()                 // #[cfg(unix)]: exit 1, stderr starts with "SYMLINK_CREATION_UNAVAILABLE:", `--json` stdout is exactly one summary object with exit 1
```

- [ ] **Step 3: Run them to verify they fail**

Run: `cargo nextest run -p flux-cli --no-fail-fast`
Expected: compile errors (`verify_report`, `for_verify`, `Commands::Verify` undefined); the integration tests fail with exit 2 from clap ("unrecognized subcommand").

- [ ] **Step 4: Implement** `verify_report.rs`, `for_verify`, `VerifyArgs`, `Commands::Verify`, `fn verify`, and the `main` arm (`main.rs:225-228`).

- [ ] **Step 5: Run the tests and the lint**

Run: `cargo nextest run -p flux-cli --no-fail-fast && cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --check`
Expected: every flux-cli test passes (the 6 new unit tests and 9 new integration tests among them); clean.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-cli/src/verify_report.rs crates/flux-cli/src/lib.rs crates/flux-cli/src/exit_code.rs crates/flux-cli/src/main.rs crates/flux-cli/tests/verify.rs
git commit -m "feat(cli): flux verify SOURCE DEST"
```

### Task 6: Real copy then verify; the documents; the gate

**Files:** Modify `crates/flux-cli/tests/verify.rs`, `README.md:56-62` (Commands) and after `README.md:86` (a `### Verify` section), `ROADMAP.md:45` (phase 6 row), `TODO.md` (after line 1201, the cut 13 block), `site/index.html:129`.

- [ ] **Step 1: Write the failing CLI tests** (design 9.16, 9.17):

```rust
#[test] fn a_real_copy_verifies_clean_and_a_flipped_byte_is_caught()  // `flux copy src dst` (exit 0), then `flux verify src dst --json`: exit 0, summary matched 3 and extra 0 (whatever the copy left under dst/.flux is filtered); flip the LAST byte of dst/sub/b through std::fs; verify → exit 1, one record outcome "mismatched" path "sub/b" code "VERIFY_MISMATCH", summary mismatched 1
#[test] fn control_state_the_copy_leaves_is_not_reported()             // hand-made in dst: `.flux/operations/<32 zeros>/state`, `.flux-root.lock`, `a.flux-partial.<32 zeros>`; plus the user file `note.flux-partial.txt`; verify --json: records contain exactly one "extra", path "note.flux-partial.txt"; summary extra 1, exit 0
```

- [ ] **Step 2: Run them to verify they fail / pass**

Run: `cargo nextest run -p flux-cli verify --no-fail-fast`
Expected: both PASS already if Tasks 1-5 are right (they pin the end-to-end behaviour on the real filesystem, which no earlier test did). If either fails, the engine or the filter is wrong: fix the engine, never the test.

- [ ] **Step 3: Documents.** README: add `### Verify` after the Verification section: the synopsis `flux verify SOURCE DEST [--links=copy|skip] [--cross-filesystems] [--hash=blake3] [--json] [--verbose]`, the five outcomes with one line each (master lines 673-681 wording), the exit codes, the NDJSON shape (one object per record, then `{"summary": ...}` with the eleven fields), and the design 6.8 guarantees verbatim. ROADMAP phase 6 row: replace "Remaining: the `flux verify` command, the `source-stream` default, ..." with "`flux verify SOURCE DEST` is done (cut 14: one source, BLAKE3, the five outcomes, `--links`, `--cross-filesystems`, `--json`, `--verbose`; `--exclude` waits for phase 3 selection; several sources are cut 14b). Remaining: the `source-stream` default and verification of a resumed file". TODO.md: a new block `## Cut 14 open (flux verify; spec docs/superpowers/specs/2026-10-26-verify-command-design.md)` with three items: several sources for verify (cut 14b); `--exclude`/`--recursive` through the shared walker when phase 3 lands; Windows reparse points and junctions as `Other`/`Symlink` in verify are CI evidence only. `site/index.html:129`: `flux copy`, `flux verify` and `flux cleanup` work; `flux benchmark` does not exist yet.

- [ ] **Step 4: The gate.** Run `just check` (fmt-check, clippy, typos, doc-check, license, test). Expected: every recipe passes; nextest reports the baseline count plus the new tests (29 engine, 6 CLI unit, 11 CLI integration), 0 failed.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-cli/tests/verify.rs README.md ROADMAP.md TODO.md site/index.html
git commit -m "docs: the flux verify command, its guarantees and the cut 14 follow-ups"
```

---

## Mutants the driver runs after Task 6 (each must turn ONLY the named test red; `--no-fail-fast`)

| mutant (temporary edit) | test that must go red |
|---|---|
| compare `path.display().to_string()` bytes instead of the key | `component_wise_order_is_not_a_bytewise_sort` |
| skip the control-state filter | `control_state_at_the_destination_root_is_never_reported` |
| treat a different-kind pair as `match` | `a_type_mismatch_drains_the_children_as_extra_or_missing` |
| `Summary::failed` also counts `extra` | `verify_exits_1_only_for_missing_mismatched_or_unreadable`, `exit_0_for_identical_trees_and_with_extras_only` |
| open an `Other` before deciding | `special_files_are_never_opened` |
| drop the `not_entered` increment | `a_mount_point_on_either_side_is_matched_once_and_pruned` |
| drop containment direction 2 | `dest_inside_source_and_source_inside_dest_are_refused` (the B half) |
| drop `preflight` | `an_alias_of_the_source_is_refused_by_path_or_by_identity` (the identity half) |
| drop the degraded lexical floor | `a_degraded_root_falls_back_to_the_lexical_test_and_says_so` |
| print `match` lines without `--verbose` | `human_output_hides_matches_unless_verbose` |
| compare `len` first and skip the read when equal | `empty_files_match_and_an_empty_against_a_byte_is_read_on_both_sides` (if lengths differ) — and a second mutant "equal lengths → match without reading" must redden `a_byte_in_the_last_buffer_is_a_mismatch` |
| open DEST before the source read failed | `an_unreadable_file_on_either_side_is_unreadable_and_the_other_side_is_not_guessed` |
| do not prune the other side after a WalkError | `an_unreadable_directory_prunes_both_sides_and_siblings_continue` |

## Self-review (run by the author, 2026-10-26)

1. Spec coverage: design 5 (key, Task 1); 6.1 roots, absent DEST, unlistable root (Task 3), mapping and `Stop` (Task 5); 6.2 merge, DirEnd, mount fold, WalkError (Tasks 1, 3); 6.3 table (Tasks 1, 2), type mismatch with subtree (Task 1); 6.4 (Task 2); 6.5 (Task 1); 6.6 records, human, NDJSON, notices (Task 5); 6.7 options (Task 5), `--links=skip` (Task 2), `--cross-filesystems` (Task 3); 6.8 text (Task 6); section 8 interfaces (Tasks 1, 4, 5, with R1-R4); section 9 tests 1-17 and 13b-13d: each named above; the mutants: the table. Deviations (section 11): README/ROADMAP/TODO in Task 6.
2. Step scan: every test step carries its name and assertions; every code step a signature and the pinned values; no "handle edge cases". The algorithm block in Task 1 is the one place a body is described, because the signature does not determine it.
3. Types: `Record`, `Summary`, `VerifyOptions`, `VerifyError`, `MOUNT_NOT_ENTERED`, `SPECIAL_NOT_COMPARED`, `verify_tree<F: DestinationRoot>`, `verify_file<F: FileSystem>` are used with the same names in Tasks 1-5; `for_verify(&Summary)`; `verify_report::{record_line, record_json, summary_line, summary_json, mount_notice_line, dest_absent_line, GUARANTEES}`.
4. Review Focus: five lines, each with a test in Tasks 1, 2 and 3.
5. Proportion: the design is 382 lines; this plan is longer by the test lists, which are the deliverable the design asked the plan to carry (section 9 names them; the plan fixes their assertions). Code appears only as signatures and test names.
