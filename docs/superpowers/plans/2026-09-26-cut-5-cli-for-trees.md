# Cut 5: the `flux copy` CLI for trees - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` copies a folder's contents with `copy_tree`, maps one source per spec §4.1, renders every record, warning and a summary on stderr plus the §53 JSON report, and exits 0/1/2/3 per §55.

**Architecture:** Three small engine changes in `crates/flux-core/src/tree.rs` (an abort keeps its partial outcome; a special file is skipped, not failed; `files_total`/`files_degraded` counts), one new `Code`, then a `flux_cli` LIBRARY target holding the pure logic (`exit_code`, `report`, `resolve`), and `main.rs` reduced to clap parsing plus wiring.

**Tech Stack:** Rust (edition 2024, stable 1.98.1), `clap` 4 (derive), `serde` + `serde_json` (workspace deps), `tempfile` (tests), `cargo nextest`, `just`.

**Design authority:** `docs/superpowers/specs/2026-09-26-cut-5-cli-for-trees-design.md` (owner-approved; adversarial panel GREEN at round 3, marker `01f586d`). Every citation below was read at `01f586d` (`main` `c44fb13` plus the spec commits).

---

## Ground rules for every task

- **Worktree:** `E:\Rust\flux-engine`, branch `spec/cut-5` (off `main` at `c44fb13`, no upstream). NEVER run a bare `git push`; publishing is `just pr` after the capstone and test audit, with owner approval.
- **Step 0 - state verification.** Confirm each quoted "current" text before editing. If it differs, STOP and report `STATE_MISMATCH: <file>: <what differs>`. Do not adapt.
- **Shape-divergence stop.** If making it compile changes a type, name, variant, field, code, message, JSON key or value shown in this plan, STOP and report `[plan] -> [yours] because <reason>`. `cargo fmt --all` rewrapping is fine.
- **Tests are the oracle.** The tests here are already written: add them verbatim, implement until they pass, never edit a test to match the code. A test that fails after the plan's code is implemented exactly is a plan defect: STOP and report it.
- **Gates (exact; run from Git Bash in the worktree):** `just check` (Windows; expect `Summary [...] N tests run: N passed, K skipped`, exit 0); `just check-linux` (WSL; clippy + nextest, exit 0); `just check-mac` (macOS clippy, exit 0). Baseline at `c44fb13`: `just check` 280 passed, 3 skipped; `just check-linux` 262 passed, 3 skipped.
- **Filtered runs use `cargo nextest run`** (never `cargo test`, which exits 0 on a zero-match filter). **Mutation checks** use `--no-fail-fast` so a count is never truncated; restore each mutant and confirm `git diff` shows only the task's intended change.
- **Write files with the Write/Edit tools, never a Bash heredoc.** Commit messages may use a heredoc (Git Bash).
- Commits end with:
  `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`
  `Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4`

## Where this plan refines the spec (declared, not silent)

1. **`flux-cli` gains a library target** (`crates/flux-cli/src/lib.rs`, crate `flux_cli`) holding `exit_code`, `report` and `resolve`. The spec says "new modules hold the logic so it is unit-testable without a process". In a BIN-only crate each module would be `dead_code` under `-D warnings` until `main.rs` calls it in Task 7 (unit tests do not clear `dead_code`: clippy builds the target without `cfg(test)` first), so every intermediate commit would fail the gate. `pub` items of a library are never dead code.
2. **`copy_tree` becomes a thin wrapper over a private `run_tree`** that takes `out: &mut TreeOutcome`; every `?` in `run_tree` is an abort, and the wrapper pairs it with `out`. This is how "every outer-`Err` site wraps its `CopyError` with the outcome accumulated so far" is implemented without touching each site.
3. **The dangling-destination-link test is Linux-only.** On Linux `open_dir`'s `O_NOFOLLOW|O_DIRECTORY` on a link answers `ENOTDIR`, which `crates/flux-platform/src/dir_unix.rs` `classify` maps to `SAFETY_REJECTED` (verified by reading: the `Errno::NOTDIR` arm, and its comment "with `O_DIRECTORY` also set the kernel reports the type mismatch first"). macOS's answer was not measured; if it is `ELOOP` the same run is `IO_ERROR`, exit 1 - still safe, nothing written.
4. **The single-file weak-identity warning names the TARGET path** (the destination after §4.1 mapping), since the Step 2a comparison is about the destination. The spec says "the path the user supplied".
5. **A single-file run now prints the summary line too** (spec: "Then one summary line"), with `created 0 directories`.

## File map

| File | Change |
|---|---|
| `crates/flux-fs/src/error.rs` | `Code::SymlinkCreationUnavailable` (Task 1) |
| `crates/flux-core/src/tree.rs` | `TreeAbort`, `run_tree` (Task 2); cause split, counts, `on_report` (Task 3); tests |
| `crates/flux-core/src/lib.rs` | export `TreeAbort` (Task 2) |
| `crates/flux-core/tests/tree_std_fs.rs` | `run` unwraps `TreeAbort` (Task 2) |
| `crates/flux-cli/src/lib.rs` | NEW library root (Task 4, grows in 5 and 6) |
| `crates/flux-cli/src/exit_code.rs` | NEW §55 mapping + tests (Task 4) |
| `crates/flux-cli/src/report.rs` | NEW lines, warnings, summary, JSON + tests (Task 5) |
| `crates/flux-cli/Cargo.toml` | `serde`, `serde_json` (Task 5) |
| `crates/flux-cli/src/resolve.rs` | NEW §4.1, K6, K7 + tests (Task 6) |
| `crates/flux-cli/src/main.rs` | clap surface + wiring + unit tests (Task 7) |
| `crates/flux-cli/tests/copy.rs` | end-to-end tests (Task 7) |
| `TODO.md` | bookkeeping (Task 8) |

---

### Task 1: `Code::SymlinkCreationUnavailable`

**Files:** Modify `crates/flux-fs/src/error.rs`.

- [ ] **Step 0: verify.** `Code` ends with `DestinationError,` then `IoError,`; `as_str` has `Code::DestinationError => "DESTINATION_ERROR",` then `Code::IoError => "IO_ERROR",`; test `every_code_has_the_spec_string` ends with `assert_eq!(Code::DestinationError.as_str(), "DESTINATION_ERROR");`. `rg -n "Code::DestinationError =>" crates` shows ONLY `error.rs` (no other exhaustive match to update).

- [ ] **Step 1: add the test assertion.** In `every_code_has_the_spec_string`, after the `DestinationError` line add:

```rust
        assert_eq!(Code::SymlinkCreationUnavailable.as_str(), "SYMLINK_CREATION_UNAVAILABLE");
```

- [ ] **Step 2: run it, expect a compile failure** (`no variant ... SymlinkCreationUnavailable`):
  `cargo nextest run -p flux-fs -E 'test(every_code_has_the_spec_string)'` → error E0599.

- [ ] **Step 3: add the variant** between `DestinationError,` and `IoError,`:

```rust
    /// §127, §259.11. A symlink cannot be created at the destination; action-scoped.
    /// This version recreates no symlinks at all, so the CLI reports every symlink it
    /// meets - inside a tree, or given as SOURCE - with this code (cut 5, K5 and K7).
    SymlinkCreationUnavailable,
```

and the `as_str` arm between `DestinationError` and `IoError`:

```rust
            Code::SymlinkCreationUnavailable => "SYMLINK_CREATION_UNAVAILABLE",
```

- [ ] **Step 4:** `cargo nextest run -p flux-fs -E 'test(every_code_has_the_spec_string)'` → `1 passed`.
- [ ] **Step 5:** `just check` → exit 0.
- [ ] **Step 6: commit.**

```bash
git add crates/flux-fs/src/error.rs
git commit -F - <<'EOF'
feat(flux-fs): add SYMLINK_CREATION_UNAVAILABLE

The registry code (§127, §259.11) the CLI uses for a symlink it cannot recreate,
inside a tree or given as SOURCE (cut 5).

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 2: an abort keeps its partial outcome (`TreeAbort`)

**Files:** Modify `crates/flux-core/src/tree.rs`, `crates/flux-core/src/lib.rs`, `crates/flux-core/tests/tree_std_fs.rs`.

- [ ] **Step 0: verify.** In `tree.rs`: `copy_tree` returns `std::result::Result<TreeOutcome, CopyError>` and its body contains exactly these six places that this task edits - `let mut out = TreeOutcome::default();` (one line, after the lexical floor), the `enter_dir(` call's argument `&mut out,`, `report(&mut out, on_failure, e.path, TreeFailureCause::Walk(e.cause));`, `copy_one(fs, src_root, dir, path, &opts, &mut out, on_failure)?;`, two multi-line `report(` calls whose first argument is `&mut out,` (the `Symlink` and `Other` arms), and the final `Ok(out)` before the function's closing `}`. `&mut out.warnings` appears twice (the two `preflight` calls) and is NOT changed. The test helper `fn run(` returns `(std::result::Result<TreeOutcome, CopyError>, Vec<TreeFailure>)`. In `lib.rs` the `pub use tree::{...}` list is `DegradedGroup, FailureTally, TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings, copy_tree,`.

- [ ] **Step 1: tests are already written - add them.** In `tree.rs`'s `mod tests`, REPLACE the helper `fn run` with these two helpers:

```rust
    fn run(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, CopyError>, Vec<TreeFailure>) {
        let (r, got) = run_full(fs, src, dst, o);
        (r.map_err(|a| a.error), got)
    }

    /// `run`, keeping the whole `TreeAbort` (cut 5).
    fn run_full(
        fs: &FaultFs,
        src: &str,
        dst: &str,
        o: &CopyOptions,
    ) -> (std::result::Result<TreeOutcome, TreeAbort>, Vec<TreeFailure>) {
        let mut got = Vec::new();
        let r = copy_tree(fs, Path::new(src), Path::new(dst), o, &mut |f| got.push(f));
        (r, got)
    }
```

and append these tests at the end of `mod tests`:

```rust
    #[test]
    fn an_abort_before_anything_is_created_carries_an_empty_outcome() {
        // The lexical floor (a weak source identity keeps the pre-flight out of it).
        let fs = tree();
        fs.set_identity("/src", FileIdentity::Weak(ObjectId { volume: 5, index: 1 }));

        let (r, _) = run_full(&fs, "/src", "/src/backup", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::SafetyRejected);
        assert_eq!((a.outcome.files_copied, a.outcome.directories_created), (0, 0));
        assert!(!a.changed());
    }

    #[test]
    fn an_abort_into_an_existing_root_with_nothing_published_is_unchanged() {
        // The exit-3 case of decision 3: the root pre-exists, the first publish aborts,
        // and its temporary was removed.
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_no_replace_support(false);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::NoReplacePublishUnavailable);
        assert!(a.error.leftover.is_none());
        assert!(!a.changed());
    }

    #[test]
    fn an_abort_after_the_root_was_created_reports_a_change() {
        // `/dst` is absent, so step 5 creates it; the first publish then aborts.
        let fs = tree();
        fs.set_no_replace_support(false);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::NoReplacePublishUnavailable);
        assert_eq!((a.outcome.directories_created, a.outcome.files_copied), (1, 0));
        assert!(a.changed(), "the root this operation created is a change");
    }

    #[test]
    fn an_abort_mid_walk_carries_what_was_copied_before_it() {
        // `a` sorts before `sub`: it is copied, then `sub` (the destination by identity)
        // aborts the operation.
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_identity("/src/sub", identity_of(&fs, "/dst"));

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!(a.error.code(), Code::SafetyRejected);
        assert_eq!(
            (a.outcome.files_copied, a.outcome.bytes_copied, a.outcome.directories_created),
            (1, 1, 0)
        );
        assert!(a.changed());
    }

    #[test]
    fn an_abort_that_leaves_a_temporary_reports_a_change() {
        // Nothing created, nothing published - but the aborted publish's temporary could
        // not be removed, so the destination differs (§55: exit 1, not 3).
        let fs = tree();
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.set_no_replace_support(false);
        fs.fail_always("remove_file", Code::PermissionDenied);

        let (r, _) = run_full(&fs, "/src", "/dst", &opts());

        let a = r.unwrap_err();
        assert_eq!((a.outcome.directories_created, a.outcome.files_copied), (0, 0));
        assert!(a.error.leftover.is_some(), "precondition: the temporary stayed");
        assert!(a.changed());
    }
```

- [ ] **Step 2: run, expect a compile failure** (`cannot find type TreeAbort`):
  `cargo nextest run -p flux-core -E 'test(/an_abort_/)'` → error E0412.

- [ ] **Step 3: add `TreeAbort`** in `tree.rs` immediately after the `TreeOutcome` struct's closing `}`:

```rust
/// The operation stopped as a whole (cut 5, K1). `outcome` holds what was counted
/// before it stopped, so a caller can report it and tell §55's exit 3 ("refused before
/// changing anything") from exit 1.
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

- [ ] **Step 4: split `copy_tree`.** Replace the signature lines

```rust
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, CopyError> {
```

with

```rust
pub fn copy_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<TreeOutcome, TreeAbort> {
    let mut out = TreeOutcome::default();
    match run_tree(fs, src_root, dst_root, opts, &mut out, on_failure) {
        Ok(()) => Ok(out),
        Err(error) => Err(TreeAbort { error, outcome: out }),
    }
}

/// `copy_tree`'s body. Every `?` here is an abort; `copy_tree` pairs it with `out`,
/// which holds whatever was counted before it.
fn run_tree<F: DestinationRoot>(
    fs: &F,
    src_root: &Path,
    dst_root: &Path,
    opts: &CopyOptions,
    out: &mut TreeOutcome,
    on_failure: &mut dyn FnMut(TreeFailure),
) -> std::result::Result<(), CopyError> {
```

  Then, inside `run_tree`'s body, make exactly these edits (the six places from Step 0):
  1. DELETE the line `    let mut out = TreeOutcome::default();` and the blank line after it.
  2. In the `enter_dir(` call: `&mut out,` → `out,`.
  3. `report(&mut out, on_failure, e.path, TreeFailureCause::Walk(e.cause));` → `report(out, on_failure, e.path, TreeFailureCause::Walk(e.cause));`
  4. `copy_one(fs, src_root, dir, path, &opts, &mut out, on_failure)?;` → `copy_one(fs, src_root, dir, path, &opts, out, on_failure)?;`
  5. In the `WalkEvent::Symlink` and `WalkEvent::Other` arms: each `report(` call's first argument `&mut out,` → `out,`.
  6. The final `    Ok(out)` → `    Ok(())`.

  Leave `&mut out.warnings` (two `preflight` calls) as is: with `out: &mut TreeOutcome` it still borrows the field.

- [ ] **Step 5: export it.** In `crates/flux-core/src/lib.rs` replace the `pub use tree::{...}` list with:

```rust
pub use tree::{
    DegradedGroup, FailureTally, TreeAbort, TreeFailure, TreeFailureCause, TreeOutcome,
    WeakIdentityWarnings, copy_tree,
};
```

- [ ] **Step 6: keep the real-filesystem tests compiling.** In `crates/flux-core/tests/tree_std_fs.rs` replace

```rust
    let r = copy_tree(&StdFileSystem, src, dst, &opts(), &mut |f| got.push(f));
```

with

```rust
    let r = copy_tree(&StdFileSystem, src, dst, &opts(), &mut |f| got.push(f)).map_err(|a| a.error);
```

- [ ] **Step 7:** `cargo nextest run -p flux-core --no-fail-fast` → every test passes, including the 5 new `an_abort_*` tests.

- [ ] **Step 8: mutation checks** (each: apply, run `cargo nextest run -p flux-core --no-fail-fast -E 'test(/an_abort_/)'`, confirm the named test is RED and report which others went red, restore):
  - M1: in `changed()` delete `self.outcome.directories_created > 0 ||` → `an_abort_after_the_root_was_created_reports_a_change` red.
  - M2: delete `|| self.outcome.files_copied > 0` → `an_abort_mid_walk_carries_what_was_copied_before_it` red.
  - M3: delete `|| self.error.leftover.is_some()` → `an_abort_that_leaves_a_temporary_reports_a_change` red.
  - M4: in `copy_tree`, `outcome: out` → `outcome: TreeOutcome::default()` → `an_abort_after_the_root_was_created_reports_a_change` and `an_abort_mid_walk_carries_what_was_copied_before_it` red.
  - M5: `changed()` body → `true` → `an_abort_before_anything_is_created_carries_an_empty_outcome` and `an_abort_into_an_existing_root_with_nothing_published_is_unchanged` red.
  After restoring: `git diff --stat` shows only the three files of this task.

- [ ] **Step 9:** `just check` → exit 0; `just check-linux` → exit 0.
- [ ] **Step 10: commit.**

```bash
git add crates/flux-core/src/tree.rs crates/flux-core/src/lib.rs crates/flux-core/tests/tree_std_fs.rs
git commit -F - <<'EOF'
feat(flux-core): a tree abort keeps its partial outcome

copy_tree returns TreeAbort { error, outcome } so the CLI can tell §55's exit 3
(refused, nothing changed) from exit 1, and still report what was copied before
the abort. changed() is true for a directory created, a file published, or a
temporary left behind. Cut 5, K1.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 3: special files are skipped, symlinks fail; `files_total`, `files_degraded`

**Files:** Modify `crates/flux-core/src/tree.rs`.

- [ ] **Step 0: verify.** `TreeFailureCause` has the variant `Unsupported(FileType),` with the doc line `/// A symlink or other non-regular entry, which this cut does not recreate.`; `FailureTally` has `pub unsupported: u64,`; `count` has `TreeFailureCause::Unsupported(_) => self.unsupported += 1,`; the `use flux_fs::{...}` list includes `FileType`; `rg -n "FileType" crates/flux-core/src/tree.rs` shows, outside the tests, only the import, the `Unsupported` variant, and the two `TreeFailureCause::Unsupported(FileType::...)` sites in `run_tree`; `on_failure` occurs 16 times in the file (`rg -c on_failure`: the 14 at `c44fb13` plus Task 2's `run_tree` parameter and call argument). The `run_tree` loop's arms read (after Task 2):

```rust
            WalkEvent::File { path } => {
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    copy_one(fs, src_root, dir, path, &opts, out, on_failure)?;
                }
            }
            WalkEvent::Symlink { path } => {
                if live {
                    report(
                        out,
                        on_failure,
                        path,
                        TreeFailureCause::Unsupported(FileType::Symlink),
                    );
                }
            }
            WalkEvent::Other { path } => {
                if live {
                    report(
                        out,
                        on_failure,
                        path,
                        TreeFailureCause::Unsupported(FileType::Other),
                    );
                }
            }
```

- [ ] **Step 1: tests are already written - change and add them.** In `mod tests`:

  (a) In `the_tally_counts_each_cause_in_its_own_field`, replace from `t.count(&TreeFailureCause::Unsupported(FileType::Symlink));` through `assert_eq!(t.total(), 5);` with:

```rust
        t.count(&TreeFailureCause::Symlink);
        t.count(&TreeFailureCause::PublishedWithComplaints(Vec::new()));
        t.count(&TreeFailureCause::SpecialFileSkipped);
        assert_eq!(
            t,
            FailureTally { walk: 1, create_dir: 1, copy: 1, symlink: 1, published_with_complaints: 1 }
        );
        assert_eq!(t.total(), 5, "a skipped special file is not a failure");
```

  (b) Replace the whole test `a_special_file_is_reported_unsupported_and_never_copied` with:

```rust
    #[test]
    fn a_special_file_is_skipped_reported_and_not_a_failure() {
        // §233.1: SKIP + DURABLE WARNING, reported per record; the run may still succeed.
        let fs = tree();
        fs.add_special("/src/dev");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].path, Path::new("dev"));
        assert!(matches!(got[0].cause, TreeFailureCause::SpecialFileSkipped));
        assert!(!fs.exists("/dst/dev"));
        assert!(out.failures.is_empty(), "skipped, not failed");
        assert_eq!(out.special_files_skipped, 1);
        assert_eq!(out.files_copied, 2, "the rest of the tree still copies");
    }
```

  (c) Replace the whole test `a_symlink_is_reported_unsupported_and_never_copied` with:

```rust
    #[test]
    fn a_symlink_is_a_failure_and_never_copied() {
        let fs = tree();
        fs.add_symlink("/src/link");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].path, Path::new("link"));
        assert!(matches!(got[0].cause, TreeFailureCause::Symlink));
        assert!(!fs.exists("/dst/link"));
        assert_eq!(out.failures.symlink, 1);
        assert_eq!(out.special_files_skipped, 0);
    }
```

  (d) In `a_weak_directory_warns_under_default_and_aborts_under_strict`, after the `assert_eq!(out.warnings.weak[&7], ...)` statement add:

```rust
        assert_eq!(out.files_degraded, 0, "a directory's skipped comparison is not a file's");
```

  (e) In `a_file_whose_destination_cannot_be_inspected_lands_in_the_unavailable_bucket`, after `assert_eq!(out.files_copied, 1);` add:

```rust
        assert_eq!(out.files_degraded, 1);
```

  (f) Append:

```rust
    #[test]
    fn files_total_counts_every_non_directory_entry_even_under_a_skipped_subtree() {
        let fs = tree(); // /src/a, /src/sub/b
        fs.add_symlink("/src/link");
        fs.add_special("/src/sub/dev");
        fs.create_dir(Path::new("/dst")).unwrap();
        fs.write_file("/dst/sub", b"a file where a directory belongs");

        let (r, got) = run(&fs, "/src", "/dst", &opts());

        let out = r.unwrap();
        // `a` and `link`, and - under the skipped `sub` - `b` and `dev`.
        assert_eq!(out.files_total, 4);
        assert_eq!(out.files_copied, 1);
        assert_eq!(out.special_files_skipped, 0, "nothing under a skipped subtree is reported");
        assert_eq!(got.len(), 2, "the symlink, and `sub` once: {got:?}");
    }
```

- [ ] **Step 2: run, expect compile failures** (`no variant Symlink`, `no field special_files_skipped`, ...): `cargo nextest run -p flux-core` → errors.

- [ ] **Step 3: implement.**

  (a) Replace the `Unsupported` variant and its doc line with:

```rust
    /// A symlink: §25's default is to copy it AS a link, which this version cannot do,
    /// so the action fails (the CLI renders `SYMLINK_CREATION_UNAVAILABLE`).
    Symlink,
    /// NOT a failure: a device, socket or FIFO, skipped as §233.1 prescribes ("SKIP +
    /// DURABLE WARNING") and streamed so each one is reported by its own record. The
    /// one variant `FailureTally` does not count; `TreeOutcome` counts it in
    /// `special_files_skipped`.
    SpecialFileSkipped,
```

  (b) Insert this doc comment directly above `TreeFailure`'s `#[derive(Debug)]` line:

```rust
/// One streamed record: every failure, plus the one non-failure `SpecialFileSkipped`.
```

  (c) In `FailureTally`: `pub unsupported: u64,` → `pub symlink: u64,`; in `total()`: `self.unsupported` → `self.symlink`; in `count`, replace the `Unsupported` arm with:

```rust
            TreeFailureCause::Symlink => self.symlink += 1,
            // Not a failure (§233.1): `report` counts it in
            // `TreeOutcome::special_files_skipped`; named here so the match stays exhaustive.
            TreeFailureCause::SpecialFileSkipped => {}
```

  (d) In `TreeOutcome`, after `pub warnings: WeakIdentityWarnings,` add:

```rust
    /// Every non-directory entry the walk yielded - file, symlink or special - including
    /// those under a skipped subtree (the walk still yields them).
    pub files_total: u64,
    /// Files whose Step 2a identity comparison was skipped for a weak side. Directories'
    /// skipped comparisons are in `warnings` only; §51's field is "files degraded".
    pub files_degraded: u64,
    /// Special files skipped (§233.1). Not failures.
    pub special_files_skipped: u64,
```

  (e) Rename the sink everywhere in the file: replace every `on_failure` with `on_report` (all 16 occurrences, including the `copy_tree` doc line; `rg -c on_failure` then prints nothing). Then change that doc line from `/// - Every other failure goes to \`on_report\`, and the walk continues.` to:

```rust
/// - Every other failure, and every skipped special file (§233.1, not a failure), goes
///   to `on_report`, and the walk continues.
```

  (f) Replace the three loop arms shown in Step 0 (now with `on_report`) with:

```rust
            WalkEvent::File { path } => {
                out.files_total += 1;
                if let Some(Frame::Live { dir, .. }) = stack.last() {
                    copy_one(fs, src_root, dir, path, &opts, out, on_report)?;
                }
            }
            WalkEvent::Symlink { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::Symlink);
                }
            }
            WalkEvent::Other { path } => {
                out.files_total += 1;
                if live {
                    report(out, on_report, path, TreeFailureCause::SpecialFileSkipped);
                }
            }
```

  (g) In `copy_one`, replace

```rust
            if let Some(weaker) = o.identity_degraded {
                out.warnings.record(weaker, &path);
            }
```

  with

```rust
            if let Some(weaker) = o.identity_degraded {
                out.files_degraded += 1;
                out.warnings.record(weaker, &path);
            }
```

  (h) Replace `report` (doc, signature and body) with:

```rust
/// Count the record, then stream it. The only place either happens.
fn report(
    out: &mut TreeOutcome,
    on_report: &mut dyn FnMut(TreeFailure),
    path: PathBuf,
    cause: TreeFailureCause,
) {
    if matches!(cause, TreeFailureCause::SpecialFileSkipped) {
        out.special_files_skipped += 1;
    }
    out.failures.count(&cause);
    on_report(TreeFailure { path, cause });
}
```

  (i) Remove `FileType` from the `use flux_fs::{...}` list (nothing outside the tests names it any more, and clippy's `unused_imports` would fail the gate).

- [ ] **Step 4:** `cargo nextest run -p flux-core --no-fail-fast` → all pass.

- [ ] **Step 5: mutation checks** (`cargo nextest run -p flux-core --no-fail-fast`; restore each):
  - M1: in `report`, delete the `if matches!(...) { ... }` block → `a_special_file_is_skipped_reported_and_not_a_failure` red.
  - M2: in `count`, `TreeFailureCause::SpecialFileSkipped => {}` → `TreeFailureCause::SpecialFileSkipped => self.symlink += 1,` → `the_tally_counts_each_cause_in_its_own_field` and `a_special_file_is_skipped_reported_and_not_a_failure` red.
  - M3: in the `File` arm, move `out.files_total += 1;` inside the `if let` block → `files_total_counts_every_non_directory_entry_even_under_a_skipped_subtree` red.
  - M4: delete `out.files_degraded += 1;` → `a_file_whose_destination_cannot_be_inspected_lands_in_the_unavailable_bucket` red.
  - M5: in the `Other` arm, `TreeFailureCause::SpecialFileSkipped` → `TreeFailureCause::Symlink` → `a_special_file_is_skipped_reported_and_not_a_failure` red.

- [ ] **Step 6:** `just check` → exit 0; `just check-linux` → exit 0; `just check-mac` → exit 0.
- [ ] **Step 7: commit.**

```bash
git add crates/flux-core/src/tree.rs
git commit -F - <<'EOF'
feat(flux-core): a special file is skipped, a symlink fails; tree counts for --json

TreeFailureCause::Unsupported splits into Symlink (a failure: §25 copies a link as
a link, which this version cannot) and SpecialFileSkipped (§233.1 SKIP + DURABLE
WARNING: streamed per record, counted in special_files_skipped, not a failure).
TreeOutcome gains files_total (every non-directory entry, skipped subtrees
included) and files_degraded (files only). The sink is renamed on_report. Cut 5.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 4: `flux_cli` library + the §55 exit codes

**Files:** Create `crates/flux-cli/src/lib.rs`, `crates/flux-cli/src/exit_code.rs`.

- [ ] **Step 0: verify.** `crates/flux-cli/src/` contains only `main.rs`; `crates/flux-cli/Cargo.toml` declares `[[bin]] name = "flux" path = "src/main.rs"` and depends on `flux-core`, `flux-fs`, `flux-platform`, `clap`. `crates/flux-core/src/copy.rs` `pub struct CopyError` has exactly the fields `pub cause: FsError`, `pub leftover: Option<(std::path::PathBuf, std::io::Error)>`, `pub step: CopyStep` (so it can be built with a struct literal from another crate).

- [ ] **Step 1: create `crates/flux-cli/src/lib.rs`:**

```rust
//! The logic behind `flux copy` (cut 5), in a library so each part is unit-tested and
//! lands before `main.rs` uses it: `exit_code` (§55), `report` (what is printed) and
//! `resolve` (the two command-line paths to what the engine is asked to do).

pub mod exit_code;
```

- [ ] **Step 2: tests are already written.** Create `crates/flux-cli/src/exit_code.rs` with the tests module first (the functions follow in Step 4):

```rust
//! §55's exit codes, decided from what the engine returned (cut 5).

use flux_core::copy::CopyError;
use flux_core::{TreeAbort, TreeOutcome};
use flux_fs::{Code, Outcome};

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::copy::CopyStep;
    use flux_fs::{FsError, MetadataFailure, MetadataItem};
    use std::path::PathBuf;

    fn err(code: Code) -> CopyError {
        CopyError {
            cause: FsError::new(code, std::io::Error::other("x")),
            leftover: None,
            step: CopyStep::Resolve,
        }
    }

    fn abort(code: Code, f: impl FnOnce(&mut TreeOutcome)) -> Result<TreeOutcome, TreeAbort> {
        let mut outcome = TreeOutcome::default();
        f(&mut outcome);
        Err(TreeAbort { error: err(code), outcome })
    }

    #[test]
    fn a_clean_tree_exits_0_and_any_failure_exits_1() {
        assert_eq!(for_tree(&Ok(TreeOutcome::default())), SUCCESS);
        let mut out = TreeOutcome::default();
        out.failures.copy = 1;
        assert_eq!(for_tree(&Ok(out)), FAILED);
    }

    #[test]
    fn warnings_and_skipped_special_files_still_exit_0() {
        let mut out = TreeOutcome::default();
        out.special_files_skipped = 3;
        out.files_degraded = 2;
        assert_eq!(for_tree(&Ok(out)), SUCCESS);
    }

    #[test]
    fn an_unchanged_refusal_exits_3() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |_| {})), REFUSED);
        assert_eq!(for_tree(&abort(Code::NoReplacePublishUnavailable, |_| {})), REFUSED);
    }

    #[test]
    fn a_refusal_after_a_change_exits_1() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.directories_created = 1)), FAILED);
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.files_copied = 1)), FAILED);
        let mut e = err(Code::NoReplacePublishUnavailable);
        e.leftover = Some((PathBuf::from("a.flux-partial.1"), std::io::Error::other("busy")));
        let left = Err(TreeAbort { error: e, outcome: TreeOutcome::default() });
        assert_eq!(for_tree(&left), FAILED);
    }

    #[test]
    fn a_refusal_after_a_streamed_failure_exits_1() {
        assert_eq!(for_tree(&abort(Code::SafetyRejected, |o| o.failures.walk = 1)), FAILED);
    }

    #[test]
    fn an_unchanged_abort_that_is_not_a_refusal_exits_1() {
        assert_eq!(for_tree(&abort(Code::IoError, |_| {})), FAILED);
    }

    #[test]
    fn single_file_exit_codes() {
        let ok = |complaints: usize| {
            Ok(Outcome {
                bytes_copied: 1,
                metadata_failures: (0..complaints)
                    .map(|_| MetadataFailure {
                        item: MetadataItem::Times,
                        error: FsError::new(Code::MetadataApplyFailed, std::io::Error::other("x")),
                    })
                    .collect(),
                identity_degraded: None,
            })
        };
        assert_eq!(for_file(&ok(0)), SUCCESS);
        assert_eq!(for_file(&ok(1)), FAILED);
        assert_eq!(for_file(&Err(err(Code::SafetyRejected))), REFUSED);
        assert_eq!(for_file(&Err(err(Code::IoError))), FAILED);
    }
}
```

- [ ] **Step 3: run, expect compile failure** (`cannot find function for_tree`): `cargo nextest run -p flux-cli --lib` → error E0425.

- [ ] **Step 4: implement** - insert between the `use` lines and `#[cfg(test)]`:

```rust
pub const SUCCESS: u8 = 0;
pub const FAILED: u8 = 1;
pub const USAGE: u8 = 2;
pub const REFUSED: u8 = 3;

/// A tree copy. Exit 3 needs a refusal that changed nothing AND followed no streamed
/// failure: a run that acted on some paths before the refusal is §55's "a partial run
/// hit a refusal on some paths only", exit 1.
pub fn for_tree(r: &Result<TreeOutcome, TreeAbort>) -> u8 {
    match r {
        Ok(out) if out.failures.is_empty() => SUCCESS,
        Ok(_) => FAILED,
        Err(a) if !a.changed() && a.outcome.failures.is_empty() && is_refusal(a.error.code()) => {
            REFUSED
        }
        Err(_) => FAILED,
    }
}

/// A single-file copy. Both of `copy_file`'s `SAFETY_REJECTED` sites (Step 0 and the
/// Step 2a gate) run before the temporary exists, so such a refusal changed nothing.
pub fn for_file(r: &Result<Outcome, CopyError>) -> u8 {
    match r {
        Ok(o) if o.metadata_failures.is_empty() => SUCCESS,
        Ok(_) => FAILED,
        Err(e) if e.code() == Code::SafetyRejected && e.leftover.is_none() => REFUSED,
        Err(_) => FAILED,
    }
}

/// §55's exit-3 refusals this engine can raise.
fn is_refusal(code: Code) -> bool {
    matches!(code, Code::SafetyRejected | Code::NoReplacePublishUnavailable)
}
```

- [ ] **Step 5:** `cargo nextest run -p flux-cli --lib` → `7 passed`.

- [ ] **Step 6: mutation checks** (`cargo nextest run -p flux-cli --lib --no-fail-fast`; restore each):
  - M1: delete `&& a.outcome.failures.is_empty()` → `a_refusal_after_a_streamed_failure_exits_1` red.
  - M2: delete `!a.changed() &&` → `a_refusal_after_a_change_exits_1` red.
  - M3: `is_refusal` → `matches!(code, Code::SafetyRejected)` → `an_unchanged_refusal_exits_3` red.
  - M4: in `for_file`, `REFUSED` → `FAILED` → `single_file_exit_codes` red.

- [ ] **Step 7:** `just check` → exit 0.
- [ ] **Step 8: commit.**

```bash
git add crates/flux-cli/src/lib.rs crates/flux-cli/src/exit_code.rs
git commit -F - <<'EOF'
feat(flux-cli): a library target, and the §55 exit codes

exit 3 only for a refusal (SAFETY_REJECTED, NOREPLACE_PUBLISH_UNAVAILABLE) that
changed nothing and followed no streamed failure; a single-file SAFETY_REJECTED is
refused before the temporary exists. The logic lives in a library so each module
is unit-tested before main.rs uses it. Cut 5, K1.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 5: `report` - record lines, warnings, summary, the §53 JSON

**Files:** Create `crates/flux-cli/src/report.rs`; modify `crates/flux-cli/src/lib.rs`, `crates/flux-cli/Cargo.toml`.

- [ ] **Step 0: verify.** Root `Cargo.toml` `[workspace.dependencies]` has `serde = { version = "1", features = ["derive"] }` and `serde_json = "1"`. `crates/flux-fs/src/options.rs` `MetadataFailure { pub item: MetadataItem, pub error: crate::FsError }` and `MetadataItem { Times, Permissions }`. After Task 3, `TreeFailureCause` is exactly `Walk(FsError)`, `CreateDir(FsError)`, `Copy(CopyError)`, `Symlink`, `SpecialFileSkipped`, `PublishedWithComplaints(Vec<MetadataFailure>)`.

- [ ] **Step 1: dependencies.** In `crates/flux-cli/Cargo.toml` `[dependencies]`, after `flux-platform = { path = "../flux-platform" }` add:

```toml
serde = { workspace = true }
serde_json = { workspace = true }
```

  and in `crates/flux-cli/src/lib.rs` after `pub mod exit_code;` add `pub mod report;`.

- [ ] **Step 2: tests are already written.** Create `crates/flux-cli/src/report.rs` with the header, the `use` lines and the tests (implementation follows in Step 4):

```rust
//! What `flux copy` prints (cut 5): one stderr line per streamed record, the
//! aggregated warnings, a summary line, and the §53 JSON report. Pure: every function
//! returns text, and `main` writes it.

use flux_core::copy::CopyError;
use flux_core::{TreeFailure, TreeFailureCause, TreeOutcome, WeakIdentityWarnings};
use flux_fs::{Code, FileIdentity, MetadataFailure, MetadataItem, Outcome};
use serde::Serialize;
use std::path::Path;

#[cfg(test)]
mod tests {
    use super::*;
    use flux_core::DegradedGroup;
    use flux_core::copy::CopyStep;
    use flux_fs::{FsError, ObjectId};
    use std::path::PathBuf;

    /// §53's complete field list, in its order.
    const KEYS: [&str; 18] = [
        "files_total",
        "files_copied",
        "files_skipped",
        "files_overwritten",
        "files_hardlinked",
        "files_reflinked",
        "files_degraded",
        "files_verified",
        "files_mismatched",
        "files_failed",
        "bytes_total",
        "bytes_copied",
        "bytes_skipped",
        "errors",
        "duration_ms",
        "average_bytes_per_second",
        "verify_level",
        "hash_algorithm",
    ];

    fn io(msg: &str) -> std::io::Error {
        std::io::Error::other(msg.to_owned())
    }

    #[test]
    fn the_json_has_every_section_53_field_in_order() {
        let s = Report::pre_engine(false).to_json();
        let v: serde_json::Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v.as_object().unwrap().len(), KEYS.len(), "{s}");
        let at: Vec<usize> = KEYS.iter().map(|k| s.find(&format!("\"{k}\":")).unwrap()).collect();
        assert!(at.windows(2).all(|w| w[0] < w[1]), "out of order: {s}");
        assert_eq!(v["verify_level"], "none");
        assert_eq!(v["hash_algorithm"], "blake3");
    }

    #[test]
    fn a_tree_report_is_true_to_the_outcome() {
        let mut out = TreeOutcome::default();
        out.files_total = 7;
        out.files_copied = 2;
        out.bytes_copied = 30;
        out.special_files_skipped = 1;
        out.files_degraded = 1;
        out.failures.copy = 1;
        out.failures.symlink = 1;
        out.failures.walk = 1;
        out.failures.published_with_complaints = 1;

        let r = Report::tree(&out, false, 10);

        assert_eq!((r.files_total, r.files_copied, r.files_skipped), (7, 2, 1));
        assert_eq!(r.files_degraded, 1);
        assert_eq!(r.files_failed, 2, "copy + symlink: walk and complaints are not failed files");
        assert_eq!((r.bytes_total, r.bytes_copied), (30, 30));
        assert_eq!(r.errors, 4);
        assert_eq!((r.duration_ms, r.average_bytes_per_second), (10, 3000));
        assert_eq!(
            (r.files_overwritten, r.files_hardlinked, r.files_reflinked, r.files_verified),
            (0, 0, 0, 0)
        );
        assert_eq!(Report::tree(&out, true, 10).errors, 5, "an abort counts as one");
    }

    #[test]
    fn a_single_file_report() {
        let ok: Result<Outcome, CopyError> = Ok(Outcome {
            bytes_copied: 5,
            metadata_failures: Vec::new(),
            identity_degraded: Some(FileIdentity::Unavailable),
        });
        let r = Report::file(&ok, true, 0);
        assert_eq!((r.files_total, r.files_copied, r.files_overwritten), (1, 1, 1));
        assert_eq!((r.files_degraded, r.bytes_total, r.errors, r.files_failed), (1, 5, 0, 0));
        assert_eq!(r.average_bytes_per_second, 5000, "a 0 ms run divides by 1");

        let bad: Result<Outcome, CopyError> = Err(CopyError {
            cause: FsError::new(Code::IoError, io("x")),
            leftover: None,
            step: CopyStep::Stream,
        });
        let r = Report::file(&bad, true, 0);
        assert_eq!((r.files_total, r.files_copied, r.files_overwritten), (1, 0, 0));
        assert_eq!((r.files_failed, r.errors), (1, 1));
    }

    #[test]
    fn a_pre_engine_report() {
        let r = Report::pre_engine(false);
        assert_eq!((r.files_total, r.files_failed, r.errors), (0, 0, 1));
        let r = Report::pre_engine(true);
        assert_eq!((r.files_total, r.files_failed, r.errors), (1, 1, 1));
    }

    #[test]
    fn paths_are_rendered_with_forward_slashes_and_the_root_as_a_dot() {
        assert_eq!(rel(&Path::new("sub").join("b")), "sub/b");
        assert_eq!(rel(Path::new("")), ".");
    }

    #[test]
    fn each_record_renders_its_code_first() {
        let line = |path: &str, cause| record_lines(&TreeFailure { path: PathBuf::from(path), cause });
        assert_eq!(
            line("dev", TreeFailureCause::SpecialFileSkipped),
            ["SPECIAL_FILE_UNSUPPORTED: dev: object_type=special action=skipped"]
        );
        assert!(line("l", TreeFailureCause::Symlink)[0].starts_with("SYMLINK_CREATION_UNAVAILABLE: l: "));
        assert_eq!(
            line("s", TreeFailureCause::Walk(FsError::new(Code::PermissionDenied, io("denied")))),
            ["PERMISSION_DENIED: s: denied"]
        );
        let copy = CopyError {
            cause: FsError::new(Code::DestinationNamespaceCollision, io("exists")),
            leftover: Some((Path::new("sub").join("a.flux-partial.1"), io("busy"))),
            step: CopyStep::Gate,
        };
        assert_eq!(
            line("sub/a", TreeFailureCause::Copy(copy)),
            ["DESTINATION_NAMESPACE_COLLISION: sub/a: exists; temporary left at sub/a.flux-partial.1 (busy)"]
        );
        let complaint = |item| MetadataFailure {
            item,
            error: FsError::new(Code::PermissionDenied, io("no")),
        };
        let v = vec![complaint(MetadataItem::Times), complaint(MetadataItem::Permissions)];
        assert_eq!(
            line("a", TreeFailureCause::PublishedWithComplaints(v)),
            ["METADATA_APPLY_FAILED: a: times: no", "METADATA_APPLY_FAILED: a: permissions: no"]
        );
    }

    #[test]
    fn one_warning_per_weak_volume_and_one_for_unavailable() {
        let mut w = WeakIdentityWarnings::default();
        assert!(warning_lines(&w).is_empty());
        w.weak.insert(0x2a, DegradedGroup { count: 3, example: PathBuf::from("sub") });
        w.unavailable = Some(DegradedGroup { count: 1, example: PathBuf::new() });

        let v = warning_lines(&w);

        assert_eq!(v.len(), 2);
        for needle in ["0x2a", "3 checks", "e.g. sub", "--safety=strict"] {
            assert!(v[0].contains(needle), "{needle}: {}", v[0]);
        }
        assert!(v[1].contains("e.g. ."), "the root renders as a dot: {}", v[1]);
        assert!(v[1].contains("--safety=strict"));
    }

    #[test]
    fn a_single_file_warning_names_the_target() {
        let weak = FileIdentity::Weak(ObjectId { volume: 0x10, index: 1 });
        let line = file_warning(&weak, Path::new("out.txt")).unwrap();
        assert!(line.contains("0x10") && line.contains("out.txt") && line.contains("--safety=strict"));
        let strong = FileIdentity::Strong(ObjectId { volume: 1, index: 1 });
        assert_eq!(file_warning(&strong, Path::new("x")), None);
    }

    #[test]
    fn the_summary_counts_an_abort_as_a_failure() {
        let r = Report::tree(&TreeOutcome::default(), true, 0);
        assert_eq!(
            summary_line(&r, 0),
            "copied 0 files (0 bytes), created 0 directories; 1 failed, 0 skipped"
        );
    }
}
```

- [ ] **Step 3: run, expect compile failure** (`cannot find struct Report`): `cargo nextest run -p flux-cli --lib` → errors.

- [ ] **Step 4: implement** - insert between the `use` lines and `#[cfg(test)]`:

```rust
/// Why a symlink fails, shared by the tree record and the SOURCE refusal (K7).
pub const SYMLINK_WHY: &str = "a symlink is copied as a link (§25), which this version cannot create";

/// §53's complete field list, in its order. Every value is true for what this version
/// does: nothing hardlinks, reflinks or verifies, so those stay 0 and `verify_level`
/// is `"none"`.
#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct Report {
    pub files_total: u64,
    pub files_copied: u64,
    pub files_skipped: u64,
    pub files_overwritten: u64,
    pub files_hardlinked: u64,
    pub files_reflinked: u64,
    pub files_degraded: u64,
    pub files_verified: u64,
    pub files_mismatched: u64,
    pub files_failed: u64,
    pub bytes_total: u64,
    pub bytes_copied: u64,
    pub bytes_skipped: u64,
    pub errors: u64,
    pub duration_ms: u64,
    pub average_bytes_per_second: u64,
    pub verify_level: &'static str,
    pub hash_algorithm: &'static str,
}

impl Report {
    fn zero() -> Self {
        Report {
            files_total: 0,
            files_copied: 0,
            files_skipped: 0,
            files_overwritten: 0,
            files_hardlinked: 0,
            files_reflinked: 0,
            files_degraded: 0,
            files_verified: 0,
            files_mismatched: 0,
            files_failed: 0,
            bytes_total: 0,
            bytes_copied: 0,
            bytes_skipped: 0,
            errors: 0,
            duration_ms: 0,
            average_bytes_per_second: 0,
            verify_level: "none",
            hash_algorithm: "blake3",
        }
    }

    /// A tree copy, finished or aborted. `bytes_total` is `bytes_copied`: the walk never
    /// stats a file, so a failed file's size is unknown (owner, cut 5). A skipped special
    /// file is a skipped target (§233.1 `action=skipped`).
    pub fn tree(out: &TreeOutcome, aborted: bool, duration_ms: u64) -> Self {
        let mut r = Self::zero();
        r.files_total = out.files_total;
        r.files_copied = out.files_copied;
        r.files_skipped = out.special_files_skipped;
        r.files_degraded = out.files_degraded;
        r.files_failed = out.failures.copy + out.failures.symlink;
        r.bytes_total = out.bytes_copied;
        r.bytes_copied = out.bytes_copied;
        r.errors = out.failures.total() + u64::from(aborted);
        r.timed(duration_ms)
    }

    /// A single-file copy. `target_existed` was read once at resolution and only feeds
    /// `files_overwritten`.
    pub fn file(r: &Result<Outcome, CopyError>, target_existed: bool, duration_ms: u64) -> Self {
        let mut rep = Self::zero();
        rep.files_total = 1;
        match r {
            Ok(o) => {
                rep.files_copied = 1;
                rep.files_overwritten = u64::from(target_existed);
                rep.files_degraded = u64::from(o.identity_degraded.is_some());
                rep.bytes_total = o.bytes_copied;
                rep.bytes_copied = o.bytes_copied;
                rep.errors = u64::from(!o.metadata_failures.is_empty());
            }
            Err(_) => {
                rep.files_failed = 1;
                rep.errors = 1;
            }
        }
        rep.timed(duration_ms)
    }

    /// A failure before the engine ran. A symlink SOURCE (K7) is one known entry that
    /// failed; a missing SOURCE or a resolution failure counts nothing.
    pub fn pre_engine(symlink_source: bool) -> Self {
        let mut r = Self::zero();
        r.errors = 1;
        if symlink_source {
            r.files_total = 1;
            r.files_failed = 1;
        }
        r
    }

    fn timed(mut self, duration_ms: u64) -> Self {
        self.duration_ms = duration_ms;
        self.average_bytes_per_second =
            self.bytes_copied.saturating_mul(1000) / duration_ms.max(1);
        self
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self)
            .expect("a flat struct of integers and static strings always serializes")
    }
}

/// A tree-relative path with `/` separators (§233.1); the root itself is `.`.
pub fn rel(path: &Path) -> String {
    let parts: Vec<_> = path.components().map(|c| c.as_os_str().to_string_lossy()).collect();
    if parts.is_empty() { ".".to_owned() } else { parts.join("/") }
}

/// The stderr lines for one streamed record: one line, except a file published with
/// several metadata complaints, which gets one per complaint.
pub fn record_lines(f: &TreeFailure) -> Vec<String> {
    let p = rel(&f.path);
    match &f.cause {
        TreeFailureCause::Walk(e) | TreeFailureCause::CreateDir(e) => {
            vec![format!("{}: {p}: {}", e.code.as_str(), e.source)]
        }
        TreeFailureCause::Copy(e) => vec![copy_line(&p, e)],
        TreeFailureCause::Symlink => {
            vec![format!("{}: {p}: {SYMLINK_WHY}", Code::SymlinkCreationUnavailable.as_str())]
        }
        TreeFailureCause::SpecialFileSkipped => vec![format!(
            "{}: {p}: object_type=special action=skipped",
            Code::SpecialFileUnsupported.as_str()
        )],
        TreeFailureCause::PublishedWithComplaints(v) => {
            v.iter().map(|m| complaint_line(&p, m)).collect()
        }
    }
}

/// A failed copy in a tree: its code, the path, the cause, and a leftover temporary
/// when one could not be removed - the path the user needs to clean up.
fn copy_line(path: &str, e: &CopyError) -> String {
    let mut s = format!("{}: {path}: {}", e.code().as_str(), e.cause.source);
    if let Some((left, why)) = &e.leftover {
        s.push_str(&format!("; temporary left at {} ({why})", rel(left)));
    }
    s
}

/// One metadata item that could not be applied to a file that IS published (§44.1).
pub fn complaint_line(path: &str, m: &MetadataFailure) -> String {
    let item = match m.item {
        MetadataItem::Times => "times",
        MetadataItem::Permissions => "permissions",
    };
    format!("{}: {path}: {item}: {}", Code::MetadataApplyFailed.as_str(), m.error.source)
}

/// One line per weak volume, then one for the `Unavailable` bucket (walker design:
/// warn once per filesystem). Empty when nothing degraded.
pub fn warning_lines(w: &WeakIdentityWarnings) -> Vec<String> {
    let mut v: Vec<String> = w
        .weak
        .iter()
        .map(|(volume, g)| weak_line(&format!("on volume {volume:#x}"), g.count, &rel(&g.example)))
        .collect();
    if let Some(g) = &w.unavailable {
        v.push(weak_line(UNAVAILABLE, g.count, &rel(&g.example)));
    }
    v
}

/// The single-file form. `degraded` is the weaker side; `target` is the destination
/// after §4.1 mapping. `None` for `Strong`, which is not a degradation.
pub fn file_warning(degraded: &FileIdentity, target: &Path) -> Option<String> {
    let scope = match degraded {
        FileIdentity::Strong(_) => return None,
        FileIdentity::Weak(id) => format!("on volume {:#x}", id.volume),
        FileIdentity::Unavailable => UNAVAILABLE.to_owned(),
    };
    Some(weak_line(&scope, 1, &target.display().to_string()))
}

const UNAVAILABLE: &str = "where the filesystem reports no identity";

fn weak_line(scope: &str, count: u64, example: &str) -> String {
    format!(
        "warning: identity could not be compared with full confidence {scope} ({count} checks, e.g. {example}); copied anyway - --safety=strict refuses instead"
    )
}

/// The last stderr line of a run that reached the engine. Its failure figure is the
/// JSON `errors` value, so an abort counts and a non-zero exit never reads as clean.
pub fn summary_line(r: &Report, directories_created: u64) -> String {
    format!(
        "copied {} files ({} bytes), created {directories_created} directories; {} failed, {} skipped",
        r.files_copied, r.bytes_copied, r.errors, r.files_skipped
    )
}
```

- [ ] **Step 5:** `cargo nextest run -p flux-cli --lib` → `16 passed` (7 from Task 4 + 9 here).

- [ ] **Step 6: mutation checks** (`cargo nextest run -p flux-cli --lib --no-fail-fast`; restore each):
  - M1: `r.files_failed = out.failures.copy + out.failures.symlink;` → `r.files_failed = out.failures.total();` → `a_tree_report_is_true_to_the_outcome` red.
  - M2: `verify_level: "none"` → `verify_level: "destination"` → `the_json_has_every_section_53_field_in_order` red.
  - M3: `u64::from(aborted)` → `0` → `a_tree_report_is_true_to_the_outcome` and `the_summary_counts_an_abort_as_a_failure` red.
  - M4: in `rel`, `parts.join("/")` → `path.display().to_string()` → on Windows `paths_are_rendered_with_forward_slashes_and_the_root_as_a_dot` red (run this one on Windows).
  - M5: in `Report::file`, `u64::from(target_existed)` → `1` → `a_single_file_report` red.

- [ ] **Step 7:** `just check` → exit 0; `just check-linux` → exit 0.
- [ ] **Step 8: commit.**

```bash
git add crates/flux-cli/Cargo.toml crates/flux-cli/src/lib.rs crates/flux-cli/src/report.rs Cargo.lock
git commit -F - <<'EOF'
feat(flux-cli): record lines, warnings, summary and the §53 JSON report

Every §53 field is present and true: no verification ran, so verify_level is
"none"; bytes_total is bytes_copied (a failed file's size is unknown); files_failed
counts file-level failures only; a skipped special file is a skipped target. The
summary's failure figure equals errors, so an abort never reads as clean. Cut 5, K3.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

  (`Cargo.lock` changes only if the lockfile gains `flux-cli -> serde, serde_json` edges; `git status` shows whether it changed. If it did not change, drop it from `git add`.)

---

### Task 6: `resolve` - §4.1 mapping, K7, K6

**Files:** Create `crates/flux-cli/src/resolve.rs`; modify `crates/flux-cli/src/lib.rs`.

- [ ] **Step 0: verify.** `crates/flux-cli/Cargo.toml` `[dev-dependencies]` has `tempfile = { workspace = true }`. `flux_fs::FsError::classify(&std::io::Error) -> Code` exists (`crates/flux-fs/src/error.rs`) and maps `NotFound` to `Code::IoError`.

- [ ] **Step 1:** in `crates/flux-cli/src/lib.rs` after `pub mod report;` add `pub mod resolve;`.

- [ ] **Step 2: tests are already written.** Create `crates/flux-cli/src/resolve.rs`:

```rust
//! From the two command-line paths to what the engine is asked to do (cut 5): §4.1's
//! one-source mapping, K7 (a symlink SOURCE is never followed) and K6
//! (canonicalization for a folder copy).

use flux_fs::{Code, FsError};
use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind};
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn canonical_tmp() -> (TempDir, PathBuf) {
        let d = TempDir::new().unwrap();
        let c = std::fs::canonicalize(d.path()).unwrap();
        (d, c)
    }

    #[test]
    fn a_trailing_separator_is_detected_on_the_raw_argument() {
        assert!(ends_with_separator(OsStr::new("out/")));
        assert!(!ends_with_separator(OsStr::new("out")));
        assert_eq!(ends_with_separator(OsStr::new("out\\")), cfg!(windows));
    }

    #[test]
    fn the_absent_remainder_is_appended_to_the_nearest_existing_ancestor() {
        let (_d, tmp) = canonical_tmp();
        let p = tmp.join("x").join("y");
        assert_eq!(canonical_with_remainder(&p).unwrap(), p);
        assert_eq!(canonical_with_remainder(&tmp).unwrap(), tmp);
    }

    #[test]
    fn a_folder_onto_an_existing_file_is_a_usage_error() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("f"), b"x").unwrap();
        assert!(matches!(job(&tmp.join("src"), &tmp.join("f")), Err(Stop::Usage(_))));
    }

    #[test]
    fn a_folder_becomes_a_tree_job_on_canonical_paths() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("src")).unwrap();
        let got = job(&tmp.join("src"), &tmp.join("new").join("dst")).unwrap();
        assert_eq!(
            got,
            Job::Tree { src: tmp.join("src"), dst: tmp.join("new").join("dst") }
        );
    }

    #[test]
    fn a_file_into_an_existing_folder_or_a_separator_path_takes_its_own_name() {
        let (_d, tmp) = canonical_tmp();
        std::fs::write(tmp.join("a"), b"x").unwrap();
        std::fs::create_dir(tmp.join("out")).unwrap();

        let into = job(&tmp.join("a"), &tmp.join("out")).unwrap();
        assert_eq!(
            into,
            Job::File { src: tmp.join("a"), dst: tmp.join("out").join("a"), target_existed: false }
        );

        let mut raw = tmp.join("missing").into_os_string();
        raw.push("/");
        let sep = job(&tmp.join("a"), Path::new(&raw)).unwrap();
        assert!(
            matches!(&sep, Job::File { dst, .. } if dst.file_name() == Some(OsStr::new("a"))),
            "{sep:?}"
        );
    }

    #[test]
    fn a_file_onto_an_existing_file_records_that_it_existed() {
        let (_d, tmp) = canonical_tmp();
        std::fs::write(tmp.join("a"), b"x").unwrap();
        std::fs::write(tmp.join("b"), b"y").unwrap();
        assert_eq!(
            job(&tmp.join("a"), &tmp.join("b")).unwrap(),
            Job::File { src: tmp.join("a"), dst: tmp.join("b"), target_existed: true }
        );
    }

    #[test]
    fn a_missing_source_is_a_failure_line_naming_it() {
        let (_d, tmp) = canonical_tmp();
        match job(&tmp.join("nope"), &tmp.join("dst")) {
            Err(Stop::Failed(line)) => {
                assert!(line.starts_with("IO_ERROR: "), "{line}");
                assert!(line.contains("nope"), "{line}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_source_is_never_followed() {
        let (_d, tmp) = canonical_tmp();
        std::fs::create_dir(tmp.join("dir")).unwrap();
        std::os::unix::fs::symlink(tmp.join("dir"), tmp.join("to-dir")).unwrap();
        std::os::unix::fs::symlink(tmp.join("nowhere"), tmp.join("dangling")).unwrap();
        for link in ["to-dir", "dangling"] {
            assert!(
                matches!(job(&tmp.join(link), &tmp.join("dst")), Err(Stop::SymlinkSource)),
                "{link}"
            );
        }
    }
}
```

- [ ] **Step 3: run, expect compile failure:** `cargo nextest run -p flux-cli --lib` → errors (`cannot find function job`).

- [ ] **Step 4: implement** - insert between the `use` lines and `#[cfg(test)]`:

```rust
/// What the engine is asked to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Job {
    /// A folder's CONTENTS onto `dst` (§4.1). Both paths canonical (K6), the absent
    /// remainder of `dst` appended.
    Tree { src: PathBuf, dst: PathBuf },
    /// One file to `dst`, as given (after §4.1's `DEST/<name>`). `target_existed` feeds
    /// only the JSON `files_overwritten`.
    File { src: PathBuf, dst: PathBuf, target_existed: bool },
}

/// Why nothing was handed to the engine.
#[derive(Debug)]
pub enum Stop {
    /// Exit 2 with this message: a folder onto an existing file (§4.1).
    Usage(String),
    /// K7: SOURCE is a symlink, which is never followed.
    SymlinkSource,
    /// Exit 1 with this line: a missing SOURCE, or a resolution failure.
    Failed(String),
}

/// §4.1 for one source.
pub fn job(source: &Path, dest: &Path) -> Result<Job, Stop> {
    let meta = std::fs::symlink_metadata(source).map_err(|e| failed(source, &e))?;
    if meta.file_type().is_symlink() {
        return Err(Stop::SymlinkSource);
    }
    if meta.is_dir() {
        // DEST is classified FOLLOWING links, because the engine follows a user-named
        // destination (§149.7). Absent or dangling is "anything else".
        if matches!(std::fs::metadata(dest), Ok(m) if !m.is_dir()) {
            return Err(Stop::Usage(format!(
                "a folder cannot be copied onto the existing file {}",
                dest.display()
            )));
        }
        let src = std::fs::canonicalize(source).map_err(|e| failed(source, &e))?;
        let dst = canonical_with_remainder(dest).map_err(|e| failed(dest, &e))?;
        return Ok(Job::Tree { src, dst });
    }
    let into_folder = ends_with_separator(dest.as_os_str())
        || std::fs::metadata(dest).is_ok_and(|m| m.is_dir());
    let dst = if into_folder {
        let Some(name) = source.file_name() else {
            return Err(Stop::Failed(format!(
                "{}: {}: the source has no file name",
                Code::IoError.as_str(),
                source.display()
            )));
        };
        dest.join(name)
    } else {
        dest.to_path_buf()
    };
    let target_existed = std::fs::symlink_metadata(&dst).is_ok();
    Ok(Job::File { src: source.to_path_buf(), dst, target_existed })
}

fn failed(path: &Path, e: &io::Error) -> Stop {
    Stop::Failed(format!("{}: {}: {e}", FsError::classify(e).as_str(), path.display()))
}

/// Tested on the RAW argument: `Path` drops a trailing separator.
pub fn ends_with_separator(p: &OsStr) -> bool {
    match p.as_encoded_bytes().last() {
        Some(b'/') => true,
        Some(b'\\') => cfg!(windows),
        _ => false,
    }
}

/// K6: canonicalize the nearest existing ancestor and append the absent remainder
/// lexically (components that do not exist cannot be links). Climbs ONLY on
/// `NotFound` (absent, or a dangling link); any other error is returned. A relative
/// path is joined to the current directory first, so the climb ends at a root.
pub fn canonical_with_remainder(p: &Path) -> io::Result<PathBuf> {
    let abs = if p.is_absolute() { p.to_path_buf() } else { std::env::current_dir()?.join(p) };
    let mut rest: Vec<OsString> = Vec::new();
    let mut cur: &Path = &abs;
    loop {
        match std::fs::canonicalize(cur) {
            Ok(mut c) => {
                c.extend(rest.iter().rev());
                return Ok(c);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let (Some(name), Some(parent)) = (cur.file_name(), cur.parent()) else {
                    return Err(e);
                };
                rest.push(name.to_owned());
                cur = parent;
            }
            Err(e) => return Err(e),
        }
    }
}
```

- [ ] **Step 5:** `cargo nextest run -p flux-cli --lib` → `23 passed` on Windows (the unix-only test is compiled out), `24 passed` under WSL.

- [ ] **Step 6: mutation checks** (restore each):
  - M1: `Some(b'\\') => cfg!(windows),` → `Some(b'\\') => true,` → under WSL (`just check-linux`, or `wsl.exe --exec bash -lc 'cd /mnt/e/Rust/flux-engine && cargo nextest run -p flux-cli --lib --no-fail-fast'`) `a_trailing_separator_is_detected_on_the_raw_argument` red.
  - M2: in `job`, delete the `if meta.file_type().is_symlink() { ... }` block → under WSL `a_symlink_source_is_never_followed` red.
  - M3: in `canonical_with_remainder`, `c.extend(rest.iter().rev());` → `c.extend(rest.iter());` → `the_absent_remainder_is_appended_to_the_nearest_existing_ancestor` red.
  - M4: delete the `if matches!(std::fs::metadata(dest), ...) { return Err(Stop::Usage(..)); }` block → `a_folder_onto_an_existing_file_is_a_usage_error` red.

- [ ] **Step 7:** `just check` → exit 0; `just check-linux` → exit 0; `just check-mac` → exit 0.
- [ ] **Step 8: commit.**

```bash
git add crates/flux-cli/src/lib.rs crates/flux-cli/src/resolve.rs
git commit -F - <<'EOF'
feat(flux-cli): resolve SOURCE and DEST per §4.1, K6 and K7

A folder's contents go onto DEST (canonical, absent remainder appended; the climb
moves up only on NotFound); a file into an existing folder or a separator path
takes its own name; a folder onto an existing file is a usage error; a symlink
SOURCE is never followed (§25). Cut 5.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 7: `main.rs` - the clap surface and the wiring; end-to-end tests

**Files:** Modify `crates/flux-cli/src/main.rs`, `crates/flux-cli/tests/copy.rs`.

- [ ] **Step 0: verify.** `main.rs` is the single-file CLI shown at `c44fb13` (65 lines: `Commands::Copy { source, destination, preserve_times }`, `Publish::Replace`, `OperationId::new(format!("{}", std::process::id()))`, every error exits 1). `tests/copy.rs` has 5 tests; two of them contain the line `    assert!(!out.status.success(), "must be refused");` (`a_copy_onto_a_hardlink_of_its_own_source_is_refused`) and `    assert!(!out.status.success(), "must be refused");` (`a_copy_onto_its_own_source_by_another_spelling_is_refused`).

- [ ] **Step 1: end-to-end tests are already written.** In `crates/flux-cli/tests/copy.rs`:

  (a) Replace the first two lines (`use std::process::Command;` / `use tempfile::TempDir;`) with:

```rust
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;
```

  (b) After the `fn flux()` helper add:

```rust
fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {:?}", out.stdout))
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
fn tree_in(d: &Path) -> PathBuf {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    src
}

/// §53's complete field list.
const KEYS: [&str; 18] = [
    "files_total",
    "files_copied",
    "files_skipped",
    "files_overwritten",
    "files_hardlinked",
    "files_reflinked",
    "files_degraded",
    "files_verified",
    "files_mismatched",
    "files_failed",
    "bytes_total",
    "bytes_copied",
    "bytes_skipped",
    "errors",
    "duration_ms",
    "average_bytes_per_second",
    "verify_level",
    "hash_algorithm",
];
```

  (c) In BOTH refusal tests replace `    assert!(!out.status.success(), "must be refused");` with:

```rust
    assert_eq!(out.status.code(), Some(3), "refused before anything changed (§55)");
```

  (d) Append:

```rust
#[test]
fn a_folders_contents_are_copied_onto_a_fresh_destination() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"A");
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
    assert!(!dst.join("src").exists(), "§4.1: the CONTENTS land on DEST");
    assert!(
        stderr(&out).contains("copied 2 files (3 bytes), created 2 directories; 0 failed"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_folder_merges_into_an_existing_folder_and_reports_each_collision() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join("a"), b"old").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let err = stderr(&out);
    assert!(err.lines().any(|l| l.starts_with("DESTINATION_NAMESPACE_COLLISION: a: ")), "{err}");
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"old", "never replaced");
    assert_eq!(
        std::fs::read(dst.join("sub").join("b")).unwrap(),
        b"BB",
        "the walk continued past the collision"
    );
}

#[test]
fn a_folder_onto_an_existing_file_is_a_usage_error() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("f");
    std::fs::write(&dst, b"x").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    assert!(stderr(&out).starts_with("error: "), "{}", stderr(&out));
    assert_eq!(std::fs::read(&dst).unwrap(), b"x");
    assert!(out.stdout.is_empty(), "no JSON on a usage error");
}

#[test]
fn a_destination_inside_the_source_is_refused_with_exit_3() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());

    let out = flux().arg("copy").arg(&src).arg(src.join("backup")).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("SAFETY_REJECTED"), "{}", stderr(&out));
    assert!(!src.join("backup").exists());
}

#[test]
fn a_file_into_an_existing_folder_lands_under_its_own_name() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    std::fs::create_dir(d.path().join("out")).unwrap();

    let out = flux().arg("copy").arg(d.path().join("a")).arg(d.path().join("out")).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(d.path().join("out").join("a")).unwrap(), b"hello");
}

#[test]
fn a_file_to_a_path_ending_in_a_separator_lands_under_its_own_name() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    std::fs::create_dir(d.path().join("out")).unwrap();
    let mut dest = d.path().join("out").into_os_string();
    dest.push("/");

    let out = flux().arg("copy").arg(d.path().join("a")).arg(&dest).output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(std::fs::read(d.path().join("out").join("a")).unwrap(), b"hello");
}

#[test]
fn a_file_to_a_separator_path_whose_folder_is_missing_fails() {
    let d = TempDir::new().unwrap();
    std::fs::write(d.path().join("a"), b"hello").unwrap();
    let mut dest = d.path().join("missing").into_os_string();
    dest.push("/");

    let out = flux().arg("copy").arg(d.path().join("a")).arg(&dest).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(!d.path().join("missing").exists(), "§4.1 does not create it");
}

#[test]
fn json_reports_every_section_53_field() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());

    let out = flux().arg("copy").arg(&src).arg(d.path().join("dst")).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = json(&out);
    assert_eq!(v.as_object().unwrap().len(), KEYS.len());
    for k in KEYS {
        assert!(v.get(k).is_some(), "missing {k}");
    }
    assert_eq!((v["files_total"].as_u64(), v["files_copied"].as_u64()), (Some(2), Some(2)));
    assert_eq!((v["bytes_copied"].as_u64(), v["bytes_total"].as_u64()), (Some(3), Some(3)));
    assert_eq!(v["errors"].as_u64(), Some(0));
    assert_eq!(v["verify_level"], "none");
    assert_eq!(v["hash_algorithm"], "blake3");
}

#[test]
fn json_is_printed_when_the_copy_fails() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join("a"), b"old").unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(1));
    let v = json(&out);
    assert_eq!((v["errors"].as_u64(), v["files_failed"].as_u64()), (Some(1), Some(1)));
    assert_eq!(v["files_copied"].as_u64(), Some(1));
}

#[test]
fn a_missing_source_exits_1_and_counts_one_error() {
    let d = TempDir::new().unwrap();
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(d.path().join("nope")).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(1));
    assert!(stderr(&out).contains("nope"), "{}", stderr(&out));
    let v = json(&out);
    assert_eq!((v["errors"].as_u64(), v["files_total"].as_u64()), (Some(1), Some(0)));
    assert!(!dst.exists());
}

#[test]
fn a_third_positional_is_a_usage_error() {
    let out = flux().args(["copy", "a", "b", "c"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}

#[cfg(unix)]
#[test]
fn a_fifo_in_a_folder_is_skipped_reported_and_exits_0() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let made = Command::new("mkfifo").arg(src.join("pipe")).status().unwrap();
    assert!(made.success(), "this test needs mkfifo");
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).arg("--json").output().unwrap();

    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out)
            .lines()
            .any(|l| l == "SPECIAL_FILE_UNSUPPORTED: pipe: object_type=special action=skipped"),
        "{}",
        stderr(&out)
    );
    assert!(std::fs::symlink_metadata(dst.join("pipe")).is_err());
    let v = json(&out);
    assert_eq!((v["files_skipped"].as_u64(), v["files_total"].as_u64()), (Some(1), Some(3)));
}

#[cfg(unix)]
#[test]
fn a_symlink_in_a_folder_fails_the_run_with_exit_1() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::os::unix::fs::symlink("a", src.join("link")).unwrap();
    let dst = d.path().join("dst");

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert!(
        stderr(&out).lines().any(|l| l.starts_with("SYMLINK_CREATION_UNAVAILABLE: link: ")),
        "{}",
        stderr(&out)
    );
    assert!(std::fs::symlink_metadata(dst.join("link")).is_err());
    assert_eq!(std::fs::read(dst.join("a")).unwrap(), b"A");
}

#[cfg(unix)]
#[test]
fn a_symlink_given_as_source_is_never_followed() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::os::unix::fs::symlink(&src, d.path().join("to-dir")).unwrap();
    std::os::unix::fs::symlink(src.join("a"), d.path().join("to-file")).unwrap();
    std::os::unix::fs::symlink(d.path().join("nowhere"), d.path().join("dangling")).unwrap();

    for link in ["to-dir", "to-file", "dangling"] {
        let dst = d.path().join(format!("dst-{link}"));
        let out = flux().arg("copy").arg(d.path().join(link)).arg(&dst).arg("--json").output().unwrap();

        assert_eq!(out.status.code(), Some(1), "{link}: {}", stderr(&out));
        assert!(stderr(&out).starts_with("SYMLINK_CREATION_UNAVAILABLE: "), "{link}: {}", stderr(&out));
        assert!(std::fs::symlink_metadata(&dst).is_err(), "{link}: nothing was created");
        let v = json(&out);
        assert_eq!(
            (v["files_total"].as_u64(), v["files_failed"].as_u64(), v["errors"].as_u64()),
            (Some(1), Some(1), Some(1)),
            "{link}"
        );
    }
}

/// Linux only: the exit code rests on `open_dir` answering ENOTDIR for a link (see the
/// plan's refinement 3); macOS was not measured.
#[cfg(target_os = "linux")]
#[test]
fn a_dangling_destination_link_is_refused_with_exit_3() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::os::unix::fs::symlink(d.path().join("nowhere"), &dst).unwrap();

    let out = flux().arg("copy").arg(&src).arg(&dst).output().unwrap();

    assert_eq!(out.status.code(), Some(3), "stderr: {}", stderr(&out));
    assert!(stderr(&out).contains("SAFETY_REJECTED"), "{}", stderr(&out));
    assert!(std::fs::symlink_metadata(&dst).unwrap().file_type().is_symlink());
    assert!(!d.path().join("nowhere").exists(), "nothing written through the link");
}
```

- [ ] **Step 2: run, expect failures:** `cargo nextest run -p flux-cli --no-fail-fast` → the new tree/JSON/mapping tests and the two exit-3 assertions FAIL against the old `main.rs` (it copies only files and exits 1 on every error). Record which failed.

- [ ] **Step 3: replace `crates/flux-cli/src/main.rs` entirely with:**

```rust
//! Flux CLI (spec §3.6): argument parsing and command dispatch. The logic lives in the
//! `flux_cli` library (`resolve`, `report`, `exit_code`), where it is unit-tested.

use clap::{Args, Parser, Subcommand, ValueEnum};
use flux_cli::exit_code;
use flux_cli::report::{self, Report};
use flux_cli::resolve::{self, Job, Stop};
use flux_fs::{Code, CopyOptions, Durability, OperationId, Preserve, Publish, Safety};
use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

#[derive(Parser)]
#[command(name = "flux", version)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Copy a file, or a folder's contents, to DEST (§4.1).
    ///
    /// A folder copy never replaces an existing file at DEST: each one is reported as
    /// DESTINATION_NAMESPACE_COLLISION and left intact (exit 1) until replacement is
    /// supported. A symlink given as SOURCE is not followed. Several sources are not
    /// supported yet.
    Copy(CopyArgs),
}

#[derive(Args)]
struct CopyArgs {
    source: PathBuf,
    destination: PathBuf,
    /// Fail a file if its timestamps cannot be applied (§44.1).
    #[arg(long)]
    preserve_times: bool,
    /// Fail a file if its permissions cannot be applied (§44.1).
    #[arg(long)]
    preserve_permissions: bool,
    /// Both --preserve-times and --preserve-permissions.
    #[arg(long)]
    preserve: bool,
    /// How durably each published file is written (§141, §165).
    #[arg(long, value_enum, default_value_t = DurabilityArg::Normal)]
    durability: DurabilityArg,
    /// `strict` refuses whenever source and destination cannot be compared by a strong
    /// filesystem identity, instead of warning and continuing.
    #[arg(long, value_enum, default_value_t = SafetyArg::Default)]
    safety: SafetyArg,
    /// Print the §53 report as JSON on stdout. bytes_total counts copied bytes only:
    /// the size of a file that failed is not known.
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum DurabilityArg {
    Normal,
    Strict,
}

#[derive(Clone, Copy, ValueEnum)]
enum SafetyArg {
    Default,
    Strict,
}

fn main() -> ExitCode {
    let Commands::Copy(args) = Cli::parse().command;
    ExitCode::from(copy(&args))
}

/// Explicitly requested preservation is Strict; otherwise best effort (§44.1).
fn options(args: &CopyArgs) -> CopyOptions {
    let preserve = |asked: bool| {
        if asked || args.preserve { Preserve::Strict } else { Preserve::Default }
    };
    CopyOptions {
        preserve_times: preserve(args.preserve_times),
        preserve_permissions: preserve(args.preserve_permissions),
        durability: match args.durability {
            DurabilityArg::Normal => Durability::Normal,
            DurabilityArg::Strict => Durability::Strict,
        },
        // The single-file default (§5.1 --overwrite); copy_tree forces NoReplace (§241.5).
        publish: Publish::Replace,
        safety: match args.safety {
            SafetyArg::Default => Safety::Default,
            SafetyArg::Strict => Safety::Strict,
        },
        operation_id: OperationId::new(format!("{}", std::process::id())),
    }
}

fn copy(args: &CopyArgs) -> u8 {
    let job = match resolve::job(&args.source, &args.destination) {
        Ok(job) => job,
        Err(Stop::Usage(msg)) => {
            err(&format!("error: {msg}"));
            return exit_code::USAGE;
        }
        Err(Stop::SymlinkSource) => {
            err(&format!(
                "{}: {}: {}; name its target to copy what it points at",
                Code::SymlinkCreationUnavailable.as_str(),
                args.source.display(),
                report::SYMLINK_WHY
            ));
            json(args, &Report::pre_engine(true));
            return exit_code::FAILED;
        }
        Err(Stop::Failed(line)) => {
            err(&line);
            json(args, &Report::pre_engine(false));
            return exit_code::FAILED;
        }
    };
    let opts = options(args);
    let fs = flux_platform::StdFileSystem;
    match job {
        Job::Tree { src, dst } => {
            let started = Instant::now();
            let result = flux_core::copy_tree(&fs, &src, &dst, &opts, &mut |f| {
                for line in report::record_lines(&f) {
                    err(&line);
                }
            });
            let ms = millis(started);
            let (outcome, aborted) = match &result {
                Ok(out) => (out, false),
                Err(a) => {
                    err(&a.error.to_string());
                    (&a.outcome, true)
                }
            };
            for line in report::warning_lines(&outcome.warnings) {
                err(&line);
            }
            let rep = Report::tree(outcome, aborted, ms);
            err(&report::summary_line(&rep, outcome.directories_created));
            json(args, &rep);
            exit_code::for_tree(&result)
        }
        Job::File { src, dst, target_existed } => {
            let started = Instant::now();
            let result = flux_core::copy_file(&fs, &src, &dst, &opts);
            let ms = millis(started);
            match &result {
                Ok(o) => {
                    let target = dst.display().to_string();
                    for m in &o.metadata_failures {
                        err(&report::complaint_line(&target, m));
                    }
                    if let Some(w) =
                        o.identity_degraded.as_ref().and_then(|d| report::file_warning(d, &dst))
                    {
                        err(&w);
                    }
                }
                // `CopyError`'s Display is "CODE: source" plus a staging temporary that
                // could not be removed - the one thing the user needs to clean up.
                Err(e) => err(&e.to_string()),
            }
            let rep = Report::file(&result, target_existed, ms);
            err(&report::summary_line(&rep, 0));
            json(args, &rep);
            exit_code::for_file(&result)
        }
    }
}

/// stderr, never panicking: a closed pipe must not turn a finished copy into exit 101.
fn err(line: &str) {
    let _ = writeln!(std::io::stderr(), "{line}");
}

fn json(args: &CopyArgs, r: &Report) {
    if args.json {
        let _ = writeln!(std::io::stdout(), "{}", r.to_json());
    }
}

fn millis(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &[&str]) -> CopyArgs {
        let mut argv = vec!["flux", "copy", "a", "b"];
        argv.extend_from_slice(extra);
        let Commands::Copy(args) = Cli::try_parse_from(argv).unwrap().command;
        args
    }

    #[test]
    fn the_defaults_are_best_effort_normal_and_default_safety() {
        let o = options(&parse(&[]));
        assert_eq!((o.preserve_times, o.preserve_permissions), (Preserve::Default, Preserve::Default));
        assert_eq!(o.durability, Durability::Normal);
        assert_eq!(o.safety, Safety::Default);
        assert_eq!(o.publish, Publish::Replace);
    }

    #[test]
    fn safety_strict_reaches_the_options() {
        assert_eq!(options(&parse(&["--safety=strict"])).safety, Safety::Strict);
    }

    #[test]
    fn preserve_sets_both_items_strict_and_each_flag_sets_its_own() {
        let both = options(&parse(&["--preserve"]));
        assert_eq!((both.preserve_times, both.preserve_permissions), (Preserve::Strict, Preserve::Strict));
        let times = options(&parse(&["--preserve-times"]));
        assert_eq!((times.preserve_times, times.preserve_permissions), (Preserve::Strict, Preserve::Default));
        let perms = options(&parse(&["--preserve-permissions"]));
        assert_eq!((perms.preserve_times, perms.preserve_permissions), (Preserve::Default, Preserve::Strict));
    }

    #[test]
    fn durability_strict_reaches_the_options() {
        assert_eq!(options(&parse(&["--durability=strict"])).durability, Durability::Strict);
    }

    #[test]
    fn an_unknown_value_is_a_usage_error() {
        assert!(Cli::try_parse_from(["flux", "copy", "a", "b", "--safety=loose"]).is_err());
    }
}
```

- [ ] **Step 4:** `cargo nextest run -p flux-cli --no-fail-fast` → all pass (on Windows the unix-only and Linux-only tests are compiled out).

- [ ] **Step 5: mutation checks** (restore each):
  - M1: in `options`, `SafetyArg::Strict => Safety::Strict` → `SafetyArg::Strict => Safety::Default` → `safety_strict_reaches_the_options` red.
  - M2: in `copy`, the `Err(Stop::Usage(msg))` arm returns `exit_code::FAILED` → `a_folder_onto_an_existing_file_is_a_usage_error` red.
  - M3: in the `Job::Tree` arm, delete the `for line in report::record_lines(&f) { ... }` loop body (keep an empty closure `|_| {}`) → `a_folder_merges_into_an_existing_folder_and_reports_each_collision` red.
  - M4: in `copy`, delete `json(args, &Report::pre_engine(false));` → `a_missing_source_exits_1_and_counts_one_error` red.

- [ ] **Step 6:** `just check` → exit 0; `just check-linux` → exit 0 (the unix and Linux-only tests run here: confirm `a_fifo_in_a_folder_is_skipped_reported_and_exits_0`, `a_symlink_in_a_folder_fails_the_run_with_exit_1`, `a_symlink_given_as_source_is_never_followed`, `a_dangling_destination_link_is_refused_with_exit_3` and `a_symlink_source_is_never_followed` appear as PASS); `just check-mac` → exit 0.
- [ ] **Step 7: commit.**

```bash
git add crates/flux-cli/src/main.rs crates/flux-cli/tests/copy.rs
git commit -F - <<'EOF'
feat(flux-cli): flux copy copies folders, reports, and exits per §55

main.rs is clap plus wiring: resolve (§4.1, K6, K7), copy_tree or copy_file,
records streamed to stderr, warnings and a summary, the §53 JSON with --json, and
exit 0/1/2/3. New flags: --preserve-permissions, --preserve, --durability,
--safety, --json. A single-file self-copy refusal now exits 3. Cut 5.

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 8: `TODO.md` bookkeeping

**Files:** Modify `TODO.md`.

- [ ] **Step 0: verify** the three quoted lines below exist verbatim.

- [ ] **Step 1:** replace

```
- [x] `flux-core`: recursive directory copy — done: `copy_tree` (cut 4b, `crates/flux-core/src/tree.rs`); the CLI surface is cut 5.
```

with

```
- [x] `flux-core`: recursive directory copy — done: `copy_tree` (cut 4b, `crates/flux-core/src/tree.rs`), exposed by `flux copy` in cut 5.
```

- [ ] **Step 2:** replace

```
- [x] `flux-cli`: wire `flux copy` to the above — done:
      `crates/flux-cli/src/main.rs:30` `Commands::Copy`
```

with

```
- [x] `flux-cli`: wire `flux copy` to the above — done: a file (cut 1) and a folder's contents
      (cut 5): `crates/flux-cli/src/main.rs` `Commands::Copy`, logic in
      `crates/flux-cli/src/{resolve,report,exit_code}.rs`
```

- [ ] **Step 3:** after the paragraph that ends `existing-dir` reports one collision per pre-existing file.` (the item **A tree copy never replaces an existing destination file.**), append these items:

```
- [ ] **`flux copy --json`'s `bytes_total` counts copied bytes only.** The walk never stats a file, so
      a failed file's size is unknown; a pre-scan would double metadata I/O (owner, cut 5). `--help`
      says so.
- [ ] **A skipped special file is reported as `object_type=special`.** The walk types it `Other` from
      `read_dir` and does not say FIFO, socket or device (§233.1 wants the object type).
- [ ] **Several sources are a usage error.** §4.1's last row and §18.3 need the operation workspace;
      `flux copy` takes exactly two positionals until then (cut 5).
- [ ] **A dangling destination link is exit 3 on Linux only by measurement.** macOS's `open_dir` answer
      for a link (ENOTDIR vs ELOOP) was not measured; if ELOOP, the same run is `IO_ERROR`, exit 1 -
      safe, but not §55's refusal code (cut 5 plan, refinement 3).
```

- [ ] **Step 4:** `just check` → exit 0 (the typos gate covers `TODO.md`).
- [ ] **Step 5: commit.**

```bash
git add TODO.md
git commit -F - <<'EOF'
docs: TODO bookkeeping for cut 5

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>
Claude-Session: https://claude.ai/code/session_01W9b8SHj3k1nPb4EAp6YuQ4
EOF
```

---

### Task 9: final verification (no publishing)

- [ ] **Step 1:** `just check` → exit 0; record the Summary line.
- [ ] **Step 2:** `just check-linux` → exit 0; record the Summary line.
- [ ] **Step 3:** `just check-mac` → exit 0.
- [ ] **Step 4:** `git status --short` clean; `git log --oneline origin/main..HEAD` lists the spec commits, this plan's commit(s) and Tasks 1-8.
- [ ] **Step 5: STOP.** AGY-CAPSTONE runs next on the range, then AGY-TEST-AUDIT, then (owner-approved) `just pr`, which arms auto-merge.

## Self-review (done by the plan author)

- **Spec coverage:**
  - Engine 1 (`TreeAbort`, `changed()`, every abort carries its outcome) → Task 2 (wrapper + 5 tests covering the floor, an unchanged decision-3 abort, the created root, a mid-walk abort, a leftover).
  - Engine 2 (Symlink failure, `SpecialFileSkipped` streamed and not tallied, `special_files_skipped`, `on_report`, exhaustive tally) → Task 3; `SYMLINK_CREATION_UNAVAILABLE` → Task 1.
  - Engine 3 (`files_total` incl. skipped subtrees, `files_degraded` files only) → Task 3.
  - Resolve (K6 both sides, climb on `NotFound` only, relative joined to cwd; K7 before the engine; DEST classified following links; separator on the raw argument; missing separator parent fails) → Task 6 (+ Task 7 end-to-end).
  - Map (§4.1 four rows, usage error, third positional) → Tasks 6 and 7.
  - Flags (K4) → Task 7 `CopyArgs`/`options` + unit tests.
  - Exit codes (0/1/2/3 incl. no-streamed-failure, leftover, single-file refusal) → Task 4 + Task 7.
  - Output: record lines, warnings (hex volume, count, example, lever), summary = errors, pre-engine prints one line and no summary, no panic on closed pipe → Tasks 5 and 7.
  - JSON: all 18 fields in order, truthful values, printed on every exit but 2 incl. pre-engine → Tasks 5 and 7.
  - Known gap in `--help` and TODO → Task 7 (`Commands::Copy` doc) and Task 8.
  - Testing section: engine tests → Tasks 2-3; CLI tests → Tasks 4-7, the exit-1-after-change case pinned at unit level (Task 4), as the spec states.
  - Residues → TODO entries (Task 8); nothing built for "Out of cut 5".
- **Placeholders:** none; every code step shows its code.
- **Consistency:** `TreeAbort { error, outcome }`, `changed()`, `TreeFailureCause::{Symlink, SpecialFileSkipped}`, `FailureTally.symlink`, `TreeOutcome.{files_total, files_degraded, special_files_skipped}`, `Report` fields/constructors (`tree`, `file`, `pre_engine`, `to_json`), `record_lines`, `complaint_line`, `warning_lines`, `file_warning`, `summary_line`, `SYMLINK_WHY`, `rel`, `Job`, `Stop`, `job`, `ends_with_separator`, `canonical_with_remainder`, `exit_code::{SUCCESS, FAILED, USAGE, REFUSED, for_tree, for_file}` are identical wherever they appear.
