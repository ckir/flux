# Copy-side verification (cut 13) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --verify[=none|source-stream|destination|full]` hashes the source stream and, for `destination` and `full`, re-reads the finished temporary file (and for `full` the source again) and compares digests before publication; a mismatch fails that file, the tree continues, the run exits 1.

**Architecture:** One change inside `stage_file` (`crates/flux-core/src/copy.rs:717`), the funnel every copy passes through: an inline BLAKE3 hasher in the stream loop, then a verification block between the stream and the metadata step. A `verified` flag travels `Staged` -> `Outcome` -> `finish_copy` (the single `files_copied` counting site, `tree.rs:4224`) into the report. No new threads, no persisted format.

**Tech Stack:** Rust 2024 workspace; `blake3` 1.8 (already a workspace dependency); clap derive; `FaultFs` (the in-memory fake) for engine tests; `cargo nextest`; the gate is `just check` (fmt-check, clippy, typos, doc-check, license, test).

**Spec:** `docs/superpowers/specs/2026-10-26-copy-verification-design.md` (owner-approved 2026-10-26, agy panel GREEN). Master spec: `FLUX_FULL_UPDATED_SPEC_V16.md`. Read the design first; this plan only adds what the design leaves to the builder and the code citations, which were read at `b9b64d9`.

## Global Constraints

- Default level is `none`; a run without `--verify` must behave and cost as before (design C1, C12).
- `--hash` accepts only `blake3`; any other value is a usage error, exit 2 (master lines 711-714).
- Bare `--verify` means `destination` (master line 709). Because `flux copy` takes positional `SOURCE... DEST`, the flag MUST use `require_equals = true` so `flux copy --verify a b` does not swallow `a` as the level.
- A mismatch is a failed file: temporary discarded through `discard()`, the destination NAME never touched, exit 1, `files_mismatched <= files_failed` (design 4, 6.4).
- `--resume` combined with a `--verify` level other than `none` (including bare `--verify` and `source-stream`) is a usage error, exit 2, before anything is touched (design 6.3, C4).
- Re-reads go through the directory handle (`DirHandle::open_read`, `crates/flux-fs/src/fs.rs:484`) for the destination and `fs.open_read(src)` by path for the source (C11). A buffer kept from writing never substitutes (master §135).
- New files carry the licence header of `header.txt` (the `license` gate checks it). Conventional commit messages; each commit ends with `Co-Authored-By: Claude Sonnet 5.5 <noreply@anthropic.com>`.
- No push, no PR, no commit to `main`. Work on `feat/verify-copy` (Task 0). Never `just pr`.

## Review Focus (failure modes the design implies; each has its test in the owning task)

1. An EMPTY file under `--verify=destination` (the stream loop never runs): verified, digest of zero bytes on both sides. Task 3.
2. A file LARGER than one stream buffer (`COPY_BUF_MAX` = 256 KiB, `copy.rs:183`): the digest must cover every chunk, not the first. Task 3.
3. A mismatch while OVERWRITING an existing file: the old file is untouched and still readable. Task 3.
4. `--verify` with a hardlink pair and with `--durability=strict` batching: anchor verified once, member not counted, one bad file in a batch does not stop the others. Task 4.
5. `flux copy --verify a b` (flag before positionals) parses as bare `--verify` + two paths, not `--verify=a`. Task 5.

## File map

- `crates/flux-hash/src/lib.rs`, `Cargo.toml` (Task 1); `crates/flux-core/Cargo.toml` gains `flux-hash = { workspace = true }`.
- `crates/flux-fs/src/error.rs`, `options.rs` (Task 2).
- `crates/flux-core/src/copy.rs` (Task 3; the engine and its tests live in this file's `mod tests`).
- `crates/flux-core/src/tree.rs`, `crates/flux-core/src/run/tests.rs` (Task 4).
- `crates/flux-cli/src/main.rs`, `report.rs`, `crates/flux-cli/tests/copy.rs` (Task 5).
- `README.md`, `ROADMAP.md`, `TODO.md`, a real-filesystem test (Task 6).

---

### Task 0: Branch and baseline

- [ ] **Step 1:** `git switch spec/verify-copy && git switch -c feat/verify-copy && git branch --unset-upstream` (the new branch must not track `origin`). `git status --short` must be empty.
- [ ] **Step 2:** Record the baseline: `cargo nextest run --workspace --no-tests=pass --no-fail-fast`. Write the pass/skip counts in your report. Expected: all pass (the last known figure on this repo was 1938 passed locally; the exact number may differ).

### Task 1: `flux-hash`, a streaming BLAKE3 hasher

**Files:** Modify `crates/flux-hash/Cargo.toml`, `crates/flux-hash/src/lib.rs`, `crates/flux-core/Cargo.toml`.

**Interfaces — Produces** (Task 3 consumes exactly these):
```rust
pub struct Hasher { /* wraps blake3::Hasher */ }
impl Hasher {
    pub fn new() -> Self;
    pub fn update(&mut self, bytes: &[u8]);
    pub fn finalize(&self) -> Digest;
}
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Digest([u8; 32]);
impl Digest {
    pub fn as_bytes(&self) -> &[u8; 32];
    pub fn to_hex(&self) -> String;
}
/// Reads `reader` to EOF in `buf`-sized reads, calling `each()` after every successful read (the caller's heartbeat; an
/// `Err` from it stops the read and is returned). Returns the digest and the byte count.
pub fn hash_reader<R: std::io::Read>(
    reader: &mut R,
    buf: &mut [u8],
    each: &mut dyn FnMut() -> std::io::Result<()>,
) -> std::io::Result<(Digest, u64)>;
```
`Hasher` also implements `Default`. The crate-level doc comment ("Scaffold only") is replaced.

- [ ] **Step 1: Tests first** (in `lib.rs` `#[cfg(test)]`), these are already specified, implement until they pass:
  - `empty_input_has_the_known_blake3_digest`: `Hasher::new().finalize().to_hex()` equals `af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262`.
  - `incremental_equals_one_shot`: for inputs of 0, 1, 1023, 1024, 1025 and 300_000 bytes (a deterministic pattern, `i % 251`), splitting at every of several offsets, `update` pieces then `finalize` equals `*blake3::hash(input).as_bytes()`. (The oracle is the `blake3` crate; add it as a dev-dependency only if it is not already a dependency of this crate.)
  - `hash_reader_counts_bytes_and_matches_one_shot`: a `Cursor` over 300_000 bytes with a 4096-byte `buf` returns that length and the one-shot digest; `each` was called `ceil(300000/4096)` times.
  - `hash_reader_stops_when_the_callback_fails`: `each` returns `Err` on the 2nd call; the error is returned and the reader was read exactly twice.
  - `a_different_byte_gives_a_different_digest` (the distractor): flipping one byte of a multi-chunk input changes the digest.
- [ ] **Step 2:** Run `cargo nextest run -p flux-hash`; the new tests fail to compile or fail. Then implement. `Cargo.toml` gets `blake3 = { workspace = true }`; `crates/flux-core/Cargo.toml` gets `flux-hash = { workspace = true }` (the root `Cargo.toml:90` already declares it).
- [ ] **Step 3:** `cargo nextest run -p flux-hash` passes; `cargo clippy -p flux-hash --all-targets -- -D warnings` clean; `cargo build -p flux-core` still compiles.
- [ ] **Step 4:** Commit `feat(hash): streaming BLAKE3 hasher for copy verification`.

### Task 2: Vocabulary in `flux-fs`

**Files:** Modify `crates/flux-fs/src/error.rs`, `crates/flux-fs/src/options.rs`, and every `CopyOptions` and `Outcome` struct literal the compiler names (include `benches/copy.rs:33`, `crates/flux-cli/src/main.rs:185`, test helpers `opts()` in `copy.rs:1293`, `run/multi.rs:1005`, `run/resume.rs:163`, `run/tests.rs:55`, `copy.rs:2845`/`4428` `strict()`, and the `Outcome` literals in `exit_code.rs`, `report.rs` tests, `tree.rs:3245/3303`, `run/mod.rs:1574/1585`, `copy.rs:751`). The compiler is the oracle for the list: search repo-wide, tests and benches included; do not trust this enumeration.

**Interfaces — Produces:**
```rust
// error.rs
Code::VerifyMismatch            // as_str() == "VERIFY_MISMATCH"
// options.rs
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum VerifyLevel { #[default] None, SourceStream, Destination, Full }
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HashAlgorithm { #[default] Blake3 }
// CopyOptions gains:   pub verify: VerifyLevel,  pub hash: HashAlgorithm,
// Outcome gains:       pub verified: bool,
```
Export `VerifyLevel` and `HashAlgorithm` from `flux-fs`'s `lib.rs` as the other option enums are.

- [ ] **Step 1: Pin tests.** In `error.rs` beside `assert_eq!(Code::SourceChanged.as_str(), ...)` (line ~161): `assert_eq!(Code::VerifyMismatch.as_str(), "VERIFY_MISMATCH")`. In `options.rs` tests: `verify_defaults_to_none_and_hash_to_blake3` asserts `VerifyLevel::default() == VerifyLevel::None` and `HashAlgorithm::default() == HashAlgorithm::Blake3`; the existing `Outcome` literal at `options.rs:170` gets `verified: false`.
- [ ] **Step 2:** Add the variants and fields. Every `CopyOptions` full literal gets `verify: VerifyLevel::None, hash: HashAlgorithm::Blake3`; every `Outcome` literal gets `verified: false` (production code in Task 3 sets `true`). `..opts()` literals need nothing.
- [ ] **Step 3:** `cargo build --workspace --all-targets` compiles; `cargo nextest run --workspace --no-tests=pass --no-fail-fast` equals the Task 0 baseline plus the new pins. `git status` lists only files the compiler forced plus the two above (check BOTH `git status` and `git log --stat <base>..HEAD`).
- [ ] **Step 4:** Commit `feat(fs): verification levels, VERIFY_MISMATCH and Outcome.verified`.

### Task 3: The engine — hash the stream, verify before publication

**Files:** Modify `crates/flux-core/src/copy.rs` (production code and its `mod tests`).

**Interfaces — Consumes:** Task 1's `Hasher`/`Digest`/`hash_reader`; Task 2's `VerifyLevel`, `Code::VerifyMismatch`, `Outcome.verified`. **Produces:** `CopyStep::Verify`; `Staged.verified: bool` (`copy.rs:531`); `publish_staged` copies it into the `Outcome` it returns (`copy.rs:1149`).

Behaviour to implement in `stage_file` (all line numbers at `b9b64d9`; read them first, STATE_MISMATCH if the code differs):

1. **Hasher.** After `let Verdict { start, carry, discard: why, proven } = verdict;` (line 782): if `opts.verify != VerifyLevel::None` and `start` is `Start::Resume { .. }`, return `Err(CopyError::at(CopyStep::Verify, FsError::new(Code::CopyFailed, io::Error::other("verification of a resumed file is not supported"))))` before `open_existing` (design C9; the partial is left untouched). Otherwise create `let mut hasher = (opts.verify != VerifyLevel::None).then(flux_hash::Hasher::new)`.
2. **Hash what is read, before it is written** (design C8): in the `carry` block (lines 843-853) update the hasher with `carry` before `write_all` (carry is empty on a fresh copy; hashing it keeps the digest correct if it ever is not), and in the loop (lines 854-878) update with `&buf[..n]` right after the read and before `write_all`.
3. **Verification block**, inserted after the `match streamed {...}` (line 897) and before "6. metadata" (line 899), only when `opts.verify` is `Destination` or `Full`:
   a. `let source_digest = hasher.finalize()`.
   b. Early source recheck (design C13): extract the existing step 7 block (lines 955-976, the `fs.metadata(src)` comparison producing `SourceChanged` via `discard(.., CopyStep::Recheck, ..)`) into a local `fn recheck_source(...)` and call it here AND at step 7 (existing behaviour unchanged; existing tests are its oracle).
   c. Re-read the temporary: `parent.open_read(&temp)`, read with `flux_hash::hash_reader` using a buffer of `copy_buffer_len(src_meta.len)`, `each` = `beat()` mapped like the stream loop (a failed beat discards with `CopyStep::Heartbeat`, `e.code`, `e.source`, `copy.rs:875-877`). An open error or a read error discards with `CopyStep::Verify` and the error's own code (`copy_code(&e)` for an `io::Error`, `e.code` for an `FsError`), NOT `VerifyMismatch` (design C3).
   d. Compare the digest AND the byte count with `source_digest` and `bytes_copied`'s total (`reused` is 0 on this path). Any difference: `discard(parent, &temp, CopyStep::Verify, Code::VerifyMismatch, io::Error::other("destination bytes differ from the source stream"), guard)`.
   e. `Full` only: `fs.open_read(src)` (by path, C11), same hashing, compare with `source_digest`; a difference discards with `Code::VerifyMismatch` and the text "source bytes differ between the copy and the second read".
   f. Set `verified = true` for `Destination` and `Full`; it is `false` for `None` and `SourceStream` (a `SourceStream` digest is computed and compared with nothing).
4. `Staged` gets `verified`; `publish_staged`'s destructure (line ~1107) and its returned `Outcome` carry it. The skip `Outcome` (`copy.rs:751`) keeps `verified: false`.

- [ ] **Step 1: Tests first**, in `copy.rs` `mod tests`, using `FaultFs` (helpers `opts()`, `stage_with`, `stage_hello`, `unguarded`, `no_heartbeat` exist near `copy.rs:1292-1320`). A `FaultFs` corruption is `fs.on_nth("open_read", n, |fs| fs.write_file("/dst.flux-partial.op1", b"HELLX"))` (the hook runs just before the nth `open_read` call; `DirHandle::open_read` delegates to the fake's `open_read`, so the source open is call 1, the temporary's re-read call 2, `full`'s second source read call 3). Determine each ordinal from a reference run's `fs.calls()` (as the cut 12 crash tests do) and assert it in the test, so a refactor that moves a call fails loudly instead of testing nothing. Names and assertions:
  - `destination_clean_copy_is_verified_and_published`: `opts.verify = Destination`, `copy_file` -> `Ok`, `out.verified`, `/dst` has the bytes, the call log contains an `open_read(/dst.flux-partial.op1)` after the stream.
  - `destination_mismatch_is_discarded_and_never_published`: corrupt the temporary's bytes before the re-read; `Err` with `code() == VerifyMismatch`, `step == CopyStep::Verify`; `!fs.exists("/dst")`, `!fs.exists("/dst.flux-partial.op1")`.
  - `a_mismatch_over_an_existing_file_leaves_it_untouched` (Review Focus 3): `/dst` pre-seeded with `b"old"`, `Publish::Replace`; after the failed verify `/dst` still reads `b"old"`.
  - `an_empty_file_verifies` (Review Focus 1): empty source, `Destination` -> `Ok`, `verified`, empty `/dst`.
  - `a_multi_buffer_file_verifies_and_a_late_corruption_is_caught` (Review Focus 2): a source of `COPY_BUF_MAX * 3 + 17` bytes; clean -> `Ok`; corrupting ONLY the last byte of the temporary -> `VerifyMismatch`.
  - `a_short_or_long_reread_is_a_mismatch`: truncate the temporary by one byte (hook), then append one byte; each -> `VerifyMismatch`.
  - `a_reread_io_error_is_not_a_mismatch`: `fs.fail_read_nth(".flux-partial", 1, ErrorKind::Other)` armed so the re-read fails; `Err` with `code() != VerifyMismatch`, `step == Verify`, temporary removed.
  - `full_catches_a_source_that_changes_between_the_reads`: `Full`; hook before the 3rd `open_read` rewrites `/src`; `VerifyMismatch` with the source-side text; `!fs.exists("/dst")`. Also `full_clean_copy_reads_the_source_twice` (two `open_read(/src)` in the log).
  - `source_stream_hashes_but_compares_nothing`: `SourceStream`; corrupting the temporary after the stream does not fail the copy; `!out.verified`; no `open_read` of the temporary in the log.
  - `none_adds_no_filesystem_calls`: the call log for a `None` copy equals the log recorded from the pre-change reference run, pinned as a literal `Vec<String>` in the test (record it BEFORE editing `stage_file`, from the unchanged code).
  - `the_early_recheck_runs_before_the_reread`: source rewritten (length change) by a hook before the temporary's re-read; the error is `SourceChanged` at `CopyStep::Recheck` and the log has NO `open_read` of the temporary.
  - `a_failed_heartbeat_during_the_reread_discards_the_temporary`: a `beat` closure that fails on its Nth call counted from a reference run so it lands inside the re-read; `step == Heartbeat`, temporary removed.
  - `verifying_a_resumed_partial_is_refused_before_it_is_touched`: drive `stage_file` with a checkpoint hook over a valid partial (copy the setup of the cut 9f-1 resume tests at `copy.rs:3692-3830`) and `verify = Destination`; `Err`, `step == Verify`, the partial's bytes and length unchanged.
  - `publish_staged_reports_verified`: staged with `verified` true -> `Outcome.verified`.
- [ ] **Step 2:** Run `cargo nextest run -p flux-core copy::tests --no-fail-fast`; the new tests fail (or do not compile) for the right reason. Write `none_adds_no_filesystem_calls`'s literal log from the UNCHANGED code first.
- [ ] **Step 3:** Implement as described. Make the new tests pass; the existing `copy::tests` pass unchanged (they are the oracle for the extracted recheck).
- [ ] **Step 4: Non-vacuity.** For each mutant, apply it, run `cargo nextest run -p flux-core copy::tests --no-fail-fast`, confirm the NAMED new test is among the failures (and that nothing else you did not expect went red), then revert: (M1) compare `!=` -> `==` on the destination digest: `destination_mismatch_is_discarded_and_never_published` and `destination_clean_copy_is_verified_and_published` fail; (M2) skip hashing `&buf[..n]` in the loop: `a_multi_buffer_file_verifies_and_a_late_corruption_is_caught` and the clean tests fail; (M3) drop the byte-count comparison: `a_short_or_long_reread_is_a_mismatch` fails; (M4) in the mismatch branch return `Err(..)` without `discard`: `destination_mismatch_is_discarded_and_never_published` fails on the leftover temporary; (M5) drop the `Resume` refusal: `verifying_a_resumed_partial_is_refused_before_it_is_touched` fails; (M6) move the early recheck after the re-read: `the_early_recheck_runs_before_the_reread` fails. Write the mutant -> red-test table in your report.
- [ ] **Step 5:** `cargo clippy -p flux-core --all-targets -- -D warnings`; full `cargo nextest run --workspace --no-tests=pass --no-fail-fast` equals baseline plus new tests. Commit `feat(core): verify the destination (and the source) before publication`.

### Task 4: Counters through the tree

**Files:** Modify `crates/flux-core/src/tree.rs` (struct `TreeOutcome` at line 40, `finish_copy` at 4214, `report()` at ~4395), tests in `tree.rs` `mod tests` and `crates/flux-core/src/run/tests.rs`.

**Interfaces — Produces:** `TreeOutcome.files_verified: u64`, `TreeOutcome.files_mismatched: u64` (both `pub`, default 0).

- [ ] **Step 1: Tests first** (names and assertions):
  - `a_verified_file_is_counted_where_it_is_published`: tree of 3 files, `verify = Destination`: `files_copied == 3`, `files_verified == 3`, `files_mismatched == 0`.
  - `a_mismatch_fails_that_file_and_the_tree_goes_on`: 3 files, corrupt the second file's re-read (ordinal from a reference run): `files_copied == 2`, `files_verified == 2`, `files_mismatched == 1`, `failures.copy == 1`, the failure's `cause` is `Copy(e)` with `e.code() == VerifyMismatch`, the other two files are published, the bad one is absent and its temporary is gone.
  - `none_and_source_stream_count_no_verified_files`: both levels -> `files_verified == 0`.
  - `a_skipped_file_is_not_verified` (`SkipExisting` over a pre-existing destination): `files_skipped == 1`, `files_verified == 0`.
  - In `run/tests.rs`, with `durability = Strict` (the batching path, `SyncAt::Barrier`, `tree.rs:3547-3570`): `strict_batch_with_one_bad_file_publishes_the_others` (Review Focus 4): 4 files, one corrupted: three published and verified, one `VerifyMismatch`, no leftover temporaries, `files_verified == 3`.
  - `a_barrier_sync_failure_uncounts_only_that_entry`: Strict batch of 3 verified files where one entry's sync fails (copy the injection used by the existing cut 9e barrier tests): `files_verified` equals `files_copied` equals 2.
  - `hardlink_pair_verifies_the_anchor_once`: two names of one source file, `verify = Destination`, `--hardlinks=auto` semantics: `files_hardlinked == 1`, `files_verified == 1`; with `Hardlinks::Copy` (both copied) `files_verified == 2`.
- [ ] **Step 2:** Implement. `finish_copy` (`tree.rs:4222-4225`): after `out.files_copied += 1`, `if o.verified { out.files_verified += 1; }`. In `report()` (`tree.rs:4395`): `if matches!(&cause, TreeFailureCause::Copy(e) if e.code() == Code::VerifyMismatch) { out.files_mismatched += 1; }` before `out.failures.count(&cause)`. A linked member's `Outcome` (`tree.rs:3245/3303`) stays `verified: false`.
- [ ] **Step 3: Non-vacuity.** (M7) drop the `if o.verified` increment: the verified-count tests fail; (M8) never increment `files_mismatched` in `report()`: `a_mismatch_fails_that_file_and_the_tree_goes_on` fails on `files_mismatched`; (M9) increment `files_mismatched` for EVERY `Copy` failure, not only `VerifyMismatch`: add to `a_mismatch_fails_that_file_and_the_tree_goes_on` a fourth file whose create fails with `IoError` (`failures.copy == 2` but `files_mismatched == 1`) so this mutant goes red. Report mutant -> test.
- [ ] **Step 4:** Gate as Task 3 step 5. Commit `feat(core): count verified files and mismatches in the tree outcome`.

### Task 5: CLI, report and help text

**Files:** Modify `crates/flux-cli/src/main.rs` (args at line 69, `options()` at 181, `copy()` at 308, the sites that set `rep.dry_run = args.dry_run`), `crates/flux-cli/src/report.rs` (`Report::tree` 134, `Report::file` 161, `summary_line` 473), tests in both and `crates/flux-cli/tests/copy.rs`.

**Interfaces — Consumes:** Tasks 2 and 4. **Produces:** `flux copy --verify[=LEVEL]` and `--hash=blake3`.

Decisions this task fixes (the design leaves wording to the builder, not these):
- Args (in `CopyArgs`): `#[arg(long, value_enum, num_args = 0..=1, require_equals = true, default_missing_value = "destination")] verify: Option<VerifyArg>` with `enum VerifyArg { None, SourceStream, Destination, Full }` (clap names: `none`, `source-stream`, `destination`, `full`); `#[arg(long, value_enum, default_value_t = HashArg::Blake3)] hash: HashArg` with the single variant `Blake3`.
- `options()` maps them to `CopyOptions { verify, hash, .. }`; absent `--verify` is `VerifyLevel::None`.
- `copy()` refuses, beside the `--resume-verify=metadata` refusal (`main.rs:309`) and before `copy_several` is reached: `args.resume && verify_level != None` prints `error: --resume cannot be combined with --verify in this version; verify a resumed copy afterwards, or use --restart` and returns `exit_code::USAGE`. `--restart`, `--dry-run` and a plain run are not refused. `--resume --verify=none` is allowed.
- Every site in `main.rs` that builds a `Report` and sets `rep.dry_run = args.dry_run` (lines ~333, 340, 366, 448, and the several-sources path ~567-580) also sets `rep.verify_level` to the requested level's string (`"none"`, `"source-stream"`, `"destination"`, `"full"`), including a dry run and a run refused after resolution; the report for a refusal at exit 2 is not built.
- `Report::tree` sets `files_verified` and `files_mismatched` from `TreeOutcome`; `Report::file` sets `files_verified = u64::from(o.verified)` on `Ok`, and on `Err(e)` `files_mismatched = u64::from(e.code() == Code::VerifyMismatch)`.
- `summary_line`: extend the existing suffix array (`report.rs:479-483`) with `(r.files_verified, "verified")` and `(r.files_mismatched, "mismatched")`, same rule (only when non-zero).
- `--help` text on `--verify` states the guarantees of design 6.5 in the doc comment, one sentence per level, including that the `destination` re-read may be served by the operating system's cache and does not prove stable storage.

- [ ] **Step 1: Tests first** (unit tests in `main.rs` `mod tests` use the existing `parse(&[...])` and `options(&parse(..))` helpers, see `main.rs:743`):
  - `verify_flag_forms`: `parse(&[])` -> `VerifyLevel::None`; `--verify` -> `Destination`; `--verify=full` -> `Full`; `--verify=none`; `--verify=source-stream`.
  - `a_bare_verify_before_the_positionals_does_not_eat_a_path` (Review Focus 5): `parse(&["--verify", "a", "b"])` has `paths == ["a", "b"]` and `Destination`.
  - `an_unknown_level_and_an_unknown_hash_are_usage_errors`: `try_parse` of `--verify=bogus` and `--hash=sha256` return `Err` whose `exit_code()` is 2.
  - `hash_defaults_to_blake3`.
  - In `crates/flux-cli/tests/copy.rs` (follow the neighbouring cut 12 tests for the harness): `resume_with_verify_is_refused_and_changes_nothing` for each of `--verify`, `--verify=destination`, `--verify=full`, `--verify=source-stream`: exit 2, stderr contains `cannot be combined`, the destination directory is exactly as before (empty or unchanged, no `.flux`); `resume_with_verify_none_is_allowed`; `restart_with_verify_runs`; `dry_run_with_verify_reads_nothing_and_reports_the_level` (`--dry-run --json --verify=full`: `verify_level == "full"`, `files_verified == 0`, destination untouched); `json_reports_the_requested_level` for a single file, a tree and several sources, for each level; `a_verified_tree_reports_files_verified` (real filesystem, `--verify=full`, 3 files: `files_verified == 3`, `files_mismatched == 0`, exit 0, summary line contains `3 verified`).
  - `report.rs` unit tests: `file` on `Ok(Outcome{verified:true,..})` -> `files_verified == 1`; on `Err` with `VerifyMismatch` -> `files_mismatched == 1 && files_failed == 1 && errors == 1`; `tree` copies both counters; `summary_line` has `; 3 verified` and `; 1 mismatched` only when non-zero and is byte-identical to before for a zero report (pin the old literal).
- [ ] **Step 2:** Implement. `cargo nextest run -p flux-cli --no-fail-fast` passes.
- [ ] **Step 3: Non-vacuity.** (M10) drop the `resume && verify != None` refusal: the refusal tests fail; (M11) remove `require_equals`: `a_bare_verify_before_the_positionals_does_not_eat_a_path` fails; (M12) set `rep.verify_level` only on the tree path: the single-file and several-sources JSON tests fail. Report mutant -> test.
- [ ] **Step 4:** Gate as before; commit `feat(cli): --verify and --hash for flux copy`.

### Task 6: Real filesystem test, docs, gate

**Files:** Create `crates/flux-core/tests/verify_std_fs.rs` (licence header; follow `crates/flux-core/tests/hardlinks_std_fs.rs` for the harness); modify `README.md` (the flags section that lists `--hardlinks`), `ROADMAP.md` (phase 6 row), `TODO.md`.

- [ ] **Step 1: Real-filesystem tests** (`StdFs`, a `tempfile` directory): `destination_verification_passes_on_a_real_filesystem` (a 1 MiB + 3 byte file, `Destination` and `Full`, published, `verified`); `a_second_handle_sees_every_written_byte` (this pins design C14's coherence claim: the re-read of an unsynced temporary equals the source). Both must pass on Linux; they are cross-platform code, CI reads Windows/macOS.
- [ ] **Step 2: Docs.** `README.md`: add `--verify[=none|source-stream|destination|full]` and `--hash` next to `--hardlinks`, with the one-line guarantee for each level and the page-cache sentence. `ROADMAP.md` phase 6 row: copy-side verification done (cut 13); `flux verify`, the `source-stream` default and resumed-file verification remain; record the deviation "default level is `none`, not the spec's `source-stream` (cut 13, call C1)". `TODO.md`: add three open items: (1) flip the default to `source-stream` after a benchmark (needs the owner's OK to measure); (2) `--resume` with `--verify` (cut 15; the whole-file digest for a resumed file needs the prefix hashed from the source; BLAKE3 digests of 1 MiB chunks do not compose into it); (3) the page-cache boundary of the `destination` re-read. Do NOT edit `docs/accepted-boundaries.md` (that file is written by the test audit).
- [ ] **Step 3:** `just check` passes end to end (fmt-check, clippy, typos, doc-check, license, test). Report the final pass/skip counts against the Task 0 baseline.
- [ ] **Step 4:** Commit `docs: verification levels, the deviation and the follow-ups`.

---

## Self-review (author)

- **Spec coverage:** design 2 goals -> Tasks 1-5; 6.1 levels -> Task 3; 6.2 steps 1-6 -> Task 3 (1b = the extracted recheck); 6.3 table rows: fresh/tree, linked member, degraded member, skip, overwrite, empty, special/symlink/dir, strict, dry-run, resume refusal, restart, resume-without-verify -> Tests in Tasks 3-5 (symlinks and directories are untouched code and have no test of their own: the existing symlink/dir tests stay the oracle that nothing regressed); 6.4 counters -> Task 4/5; 6.5 text -> Task 5 help + Task 6 README; C1-C15 -> Global Constraints and the tasks that cite them; section 9 test list -> Tasks 1, 3, 4, 5, 6.
- **Types:** `Hasher`/`Digest`/`hash_reader` (Task 1) match Task 3's use; `VerifyLevel`/`HashAlgorithm`/`Outcome.verified`/`Code::VerifyMismatch` (Task 2) match Tasks 3-5; `TreeOutcome.files_verified/files_mismatched` (Task 4) match `Report::tree` (Task 5).
- **Open items that depend on an implementer reading code, each owned:** the `FaultFs` ordinals for `on_nth` (Task 3, from a reference run); which injection fails one barrier sync (Task 4, copy the cut 9e tests); the exact set of `main.rs` report sites (Task 5, grep `dry_run = args.dry_run`).
- **Deviation from the design, recorded:** design 11 proposed an `accepted-boundaries.md` entry for the page-cache limit; that file is the test audit's ledger, so this plan records the limit in `--help`, the README and `TODO.md` instead.
