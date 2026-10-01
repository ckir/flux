# Cut 7a Part 3b-2: `flux copy` under the destination's lock - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or
> superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy` runs every copy through `flux_core::run` (Part 3b-1): the `--restart` and `--break-lock` flags,
exit statuses and messages for every refusal, a debug-build-only crash hook, and the spec's end-to-end tests including
a real crash.

**Architecture:**
- `flux-core::run` gains one optional test hook in `RunConfig`, called before every guarded destination mutation, and
  a lock refusal's message gains the lock's path. Nothing else in the core changes.
- `flux-cli` builds a `RunConfig` per invocation, calls `run::tree` / `run::file`, renders the run's stop and warnings,
  and maps the run to an exit status. In a debug build only, two environment variables install the crash hook.
- The `--json` report keeps exactly §53's fields: a stop outside the copy counts as one error.

**Tech Stack:** Rust 2024; `clap` 4 (`requires`); no new dependency.

**Spec:** `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md` - "Errors and exit statuses", the
refusal-guidance table, "Testing" item 4; `FLUX_FULL_UPDATED_SPEC_V16.md` §53 (spec:2805-2840) and §96.2.

---

## What this plan rests on (read and verified at `ece672b`)

- `crates/flux-core/src/run/mod.rs`: `#[derive(Debug, Clone)] pub struct RunConfig` (`:35-48`, five fields ending with
  `pub boot_session_id: String,`); in `tree` the line `let guard = || guarded(&locked.held);` (`:194`) and in `file`
  the same line (`:295`); `pub use` of nothing - the module's items are `pub` and reached as `flux_core::run::...`.
- `crates/flux-core/src/run/session.rs`: `pub(crate) fn from_lock` (`:243-248`); `open_operation` calls
  `from_lock(e, RunStep::Lock, &lock_shown, ...)` for `obtain`'s errors, and `run::tree` / `run::file` call it with
  `RunStep::Lock` and the lock's directory for the capability and lock-site refusals.
- `crates/flux-core/src/run/tests.rs`: `fn cfg()` (`:24-32`); helpers `fake`, `run_tree`, `run_file`, `ok`, `LOCK`,
  `ID`, `dead_lock`.
- `crates/flux-cli/src/exit_code.rs`: `for_tree` (`:15-26`), `for_file` (`:28-35`), `fn is_refusal` (`:38-40`), and
  its tests (`:42-127`, helpers `err` and `abort`).
- `crates/flux-cli/src/report.rs`: `Report` (the 18 §53 fields, all `pub`), `Report::tree`, `Report::file`,
  `Report::pre_engine`, `record_lines`, `warning_lines`, `file_warning`, `complaint_line`, `summary_line`; its tests
  module.
- `crates/flux-cli/src/main.rs`: `struct CopyArgs` (`:31-57`, last field `json: bool`), `fn options` (`:76-95`, which
  sets `operation_id: OperationId::new(format!("{}", std::process::id()))`), `fn copy` (`:97-173`), `err`, `json`,
  `millis`, and its tests (`:189-245`, helper `parse`).
- `crates/flux-cli/tests/copy.rs`: helpers `flux()`, `stderr`, `json`, `tree_in`, the 18-key `KEYS` list.
- `flux_core::lock::record::LockRecord` (8 `pub` fields) and `encode()`; `flux_core::lock::{LockCode, Refusal}`;
  `flux_core::ids::{new_id, is_id}`; `flux_platform::boot_session_id()`.
- Classification ignores a record's lock keys: a dead owner's valid record naming `operations/<id>` whose workspace is
  missing is `ARTIFACT_OWNERSHIP_UNCERTAIN` (`crates/flux-core/src/lock/classify.rs:44-50`, `lock/site.rs` `workspace`).
- `TODO.md`: the items "A leftover temporary from a PREVIOUS run is never removed" (`:143`), "A blocking pre-existing
  temporary is not reported" (`:177`) and "WSL 9p mounts break two lock assumptions" (`:354`).

## Decisions (AGY-FIRST, then the owner, 2026-10-01)

Seam `.clavity/seams/cut7a-p3b2-forks.md`, reply `.clavity/scratch/cut7a-p3b2/forks-reply.md`.

1. **Q-1: the JSON report keeps exactly §53's fields** ("Complete field list (not only an example)", spec:2811). A run
   that stopped before the copy prints `Report::pre_engine(false)` (`errors` = 1); a run that copied and then stopped
   adds 1 to `errors`. The refusal code, the holder, the warnings and the operation id go to stderr only. (agy first
   chose added fields, then changed its call after reading spec:2811 and the key-count test.)
2. **Q-2: the crash hook is an optional callback in `RunConfig`** (`before_mutation`), so the library never reads the
   environment. The CLI installs it only under `cfg(debug_assertions)`, from `FLUX_TEST_STALL_AT` (the call number) and
   `FLUX_TEST_STALL_FILE` (the file it creates before it blocks).
3. **Q-3: the hook runs before every guarded mutation** of the copy - inside the guard closure, before `guarded`.
4. **Q-4: a refusal renders as `CODE: detail`, then one indented line per holder field** (§96.2), then an indented
   `not removed:` line where the run could not remove what it created. A warning is one `warning: ...` line. (agy's
   call; the driver had leaned to one line.)
5. **Q-5: the end-to-end refusals.** `TARGET_LOCK_BUSY` is staged with a child `flux copy` held by the crash hook;
   `ARTIFACT_OWNERSHIP_UNCERTAIN` by writing a `LockRecord` that names a missing workspace; `TARGET_LOCK_UNCERTAIN`,
   `STATE_CORRUPT`, `INCOMPATIBLE_STATE` and `CONTROL_PLANE_NAMESPACE_CONFLICT` by writing files. `REMOTE_LOCK_UNSAFE`
   has no end-to-end test (the core's fake covers it).
6. **`--break-lock` requires `--restart`** (owner ruling, the design spec): clap's `requires`, a usage error, exit 2.

### Decisions this plan makes (no fork)

7. **A lock refusal names the lock's path** - the refusal-guidance table's "the lock path and the holder" / "the lock
   path and what was read". `session::from_lock` prefixes a `RunStep::Lock` refusal's detail with the path it was
   given (the lock, or the directory that would hold it). The scan's refusals already name their state's path.
8. **The CLI's per-run ids**: `operation_id` and `owner_instance_id` are two `flux_core::ids::new_id()`;
   `CopyOptions::operation_id` is a placeholder the run replaces.
9. **`exit_code::for_tree` uses `TreeAbort::refused_unchanged`** (Part 3b-1), the same rule, defined once; `is_refusal`
   goes.
10. **`TODO.md`** closes the previous-run temporary and the WSL 9p items, and records what the persisted id changes
    for the blocking-temporary item without closing it (the reporting gap is still there); the design spec's "What this
    closes" is corrected to match.

## File structure

- Modify `crates/flux-core/src/run/mod.rs` - `BeforeMutation`, `RunConfig::before_mutation`, the hook in both guards
  (Task 1).
- Modify `crates/flux-core/src/run/session.rs` - `from_lock` names the lock (Task 1).
- Modify `crates/flux-core/src/run/tests.rs` - `cfg()` and three tests (Task 1).
- Modify `crates/flux-cli/src/exit_code.rs` - `for_tree_run`, `for_file_run` (Task 2).
- Modify `crates/flux-cli/src/report.rs` - `stop_lines`, `run_warning_line` (Task 3).
- Modify `crates/flux-cli/src/main.rs` - the flags, `run_config`, `debug_hook`, `copy` through the run (Task 4).
- Create `crates/flux-cli/tests/run.rs` - the end-to-end tests (Task 5).
- Modify `TODO.md` and the design spec's "What this closes" (Task 6).

## Commands

- `cargo test -p flux-core --lib run::`, `cargo test -p flux-cli` (unit and integration tests).
- Gates: `just check`, `just check-linux`, `just check-mac`. Baseline at `ece672b`: 538 and 524 tests.
- Edit Rust files with an editor tool, never through a shell heredoc or `python -` (a hook on this machine collapses
  `\\` to `\` there).

---

### Task 1: The core's hook, and a lock refusal that names the lock

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs` (`:35-48`, `:194`, `:295`)
- Modify: `crates/flux-core/src/run/session.rs` (`:243-248`)
- Modify: `crates/flux-core/src/run/tests.rs` (`:3-14` imports, `:24-32`, and the end of the file)

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests.** In `run/tests.rs`, add to the imports
  `use std::sync::atomic::{AtomicUsize, Ordering};`, add the field `before_mutation: None,` as the last field of the
  `RunConfig` in `fn cfg()`, and append at the end of the file:

```rust
// Part 3b-2.

#[test]
fn a_refusal_of_the_lock_names_the_lock_path() {
    let fs = fake();
    let p = fs.destination_root(Path::new("/p")).unwrap();
    dead_lock(&p, "dest.flux-lock", b"");
    let (r, _) = run_tree(&fs, &cfg());
    let Some(RunError::Refused { refusal, .. }) = &r.stop else { panic!("{:?}", r.stop) };
    assert_eq!(refusal.code, LockCode::TargetLockUncertain);
    assert!(refusal.detail.replace('\\', "/").starts_with(LOCK), "{}", refusal.detail);
}

#[test]
fn the_hook_runs_once_before_each_guarded_mutation() {
    // A tree's guarded mutations: `a`'s sweep, temporary and publish; `sub`'s creation; `sub/b`'s three. A single
    // file's: its sweep, temporary and publish.
    for (tree_run, expected) in [(true, 7), (false, 3)] {
        let fs = fake();
        let count = Arc::new(AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        let hook: BeforeMutation = Arc::new(move || {
            seen.fetch_add(1, Ordering::SeqCst);
        });
        let c = RunConfig { before_mutation: Some(hook), ..cfg() };
        if tree_run {
            ok(&run_tree(&fs, &c).0);
        } else {
            let r = run_file(&fs, &c);
            assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
        }
        assert_eq!(count.load(Ordering::SeqCst), expected, "tree {tree_run}");
    }
}

#[test]
fn a_hook_that_never_returns_stops_the_copy_before_that_mutation() {
    let fs = fake();
    let count = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&count);
    // The third guarded mutation is `a`'s publish; the CLI's hook blocks there forever, this one panics instead.
    let hook: BeforeMutation = Arc::new(move || {
        if seen.fetch_add(1, Ordering::SeqCst) + 1 == 3 {
            panic!("the test's stall point");
        }
    });
    let c = RunConfig { before_mutation: Some(hook), ..cfg() };
    let stalled = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_tree(&fs, &c)));
    assert!(stalled.is_err(), "the hook stopped the run");
    assert_eq!(count.load(Ordering::SeqCst), 3);
    assert!(fs.exists(format!("/p/dest/a.flux-partial.{ID}")), "the temporary was made");
    assert!(!fs.exists("/p/dest/a"), "the publish did not happen");
}
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-core --lib run::`
Expected: `error[E0560]: struct `RunConfig` has no field named `before_mutation`` (and `BeforeMutation` not found).

- [ ] **Step 3: Implement.** In `run/mod.rs`, replace `#[derive(Debug, Clone)]` and `pub struct RunConfig { ... }`
  (`:35-48`) with:

```rust
/// A test hook (Part 3b-2): called before every guarded destination mutation of the copy, before its §99 check. The CLI
/// installs one only in a debug build, to stop a run at a known point and kill it there.
pub type BeforeMutation = std::sync::Arc<dyn Fn() + Send + Sync>;

/// What the operator asked of the run, beyond the copy itself.
#[derive(Clone)]
pub struct RunConfig {
    /// `--restart`: supersede every resumable prior operation.
    pub restart: bool,
    /// `--break-lock`: take an uncertain lock over (§240.5). The CLI allows it only with `--restart` (exit 2).
    pub break_lock: bool,
    /// This operation's id (`ids::new_id`): it names the state, the record and every temporary.
    pub operation_id: String,
    /// This process's id, in the same form.
    pub owner_instance_id: String,
    /// `flux_platform::boot_session_id()`, or `unknown`.
    pub boot_session_id: String,
    /// `None` outside a test.
    pub before_mutation: Option<BeforeMutation>,
}

impl std::fmt::Debug for RunConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunConfig")
            .field("restart", &self.restart)
            .field("break_lock", &self.break_lock)
            .field("operation_id", &self.operation_id)
            .field("owner_instance_id", &self.owner_instance_id)
            .field("boot_session_id", &self.boot_session_id)
            .field("before_mutation", &self.before_mutation.is_some())
            .finish()
    }
}
```

  Replace both `let guard = || guarded(&locked.held);` lines (`:194` in `tree`, `:295` in `file`) with:

```rust
        let guard = || {
            if let Some(hook) = &cfg.before_mutation {
                hook();
            }
            guarded(&locked.held)
        };
```

  In `run/session.rs`, replace `from_lock` (`:243-248`) with:

```rust
/// A lock-protocol error as the run's: a refusal keeps its code; an I/O error is the run's failure at `step`, `path`.
/// A refusal of the lock names the path to act on - the lock, or the directory that would hold it - as the
/// refusal-guidance table requires (Part 3b-2 decision 7).
pub(crate) fn from_lock(e: LockError, step: RunStep, path: &Path, changed: bool) -> RunError {
    match e {
        LockError::Refused(mut refusal) => {
            if step == RunStep::Lock {
                refusal.detail = format!("{}: {}", path.display(), refusal.detail);
            }
            RunError::Refused { refusal, changed, not_removed: None }
        }
        LockError::Io(error) => RunError::Failed { step, path: path.to_path_buf(), error },
    }
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-core --lib run::`
Expected: `42 passed` (39 + 3). Then `cargo test -p flux-core --lib`: `305 passed`; clippy and fmt clean.

- [ ] **Step 5: Prove the tests can fail** (restore after each):
  - `from_lock` without the `if step == RunStep::Lock` prefix: `a_refusal_of_the_lock_names_the_lock_path` FAILED;
  - the hook call removed from `tree`'s guard: both hook tests FAILED; from `file`'s guard:
    `the_hook_runs_once_before_each_guarded_mutation` FAILED (`tree false`).

- [ ] **Step 6: Commit**

```bash
git add crates/flux-core/src/run
git commit -m "feat(core): RunConfig's test hook before each guarded mutation; a lock refusal names the lock (cut 7a Part 3b-2)"
```

---

### Task 2: The exit status of a run

**Files:**
- Modify: `crates/flux-cli/src/exit_code.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of the `mod tests` in `exit_code.rs`:

```rust
    fn refused(changed: bool) -> RunError {
        RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::TargetLockBusy,
                holder: None,
                detail: "x".to_string(),
            }),
            changed,
            not_removed: None,
        }
    }

    fn failed() -> RunError {
        RunError::Failed {
            step: RunStep::State,
            path: PathBuf::from("p"),
            error: FsError::new(Code::IoError, std::io::Error::other("x")),
        }
    }

    #[test]
    fn a_tree_runs_own_stop_decides_before_its_copy() {
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new() };
        assert_eq!(for_tree_run(&run(None, Some(refused(false)))), REFUSED);
        assert_eq!(for_tree_run(&run(None, Some(refused(true)))), FAILED);
        assert_eq!(for_tree_run(&run(None, Some(failed()))), FAILED);
        assert_eq!(for_tree_run(&run(Some(Ok(TreeOutcome::default())), Some(failed()))), FAILED);
        assert_eq!(for_tree_run(&run(Some(Ok(TreeOutcome::default())), None)), SUCCESS);
        let rolled_back = abort(Code::SafetyRejected, |_| {});
        assert_eq!(for_tree_run(&run(Some(rolled_back), None)), REFUSED, "a refusal rolled back");
    }

    #[test]
    fn a_file_runs_own_stop_decides_before_its_copy() {
        let run = |copy, stop| Run { copy, stop, warnings: Vec::new() };
        let copied =
            || Ok(Outcome { bytes_copied: 1, metadata_failures: Vec::new(), identity_degraded: None });
        assert_eq!(for_file_run(&run(None, Some(refused(false)))), REFUSED);
        assert_eq!(for_file_run(&run(Some(copied()), Some(refused(true)))), FAILED);
        assert_eq!(for_file_run(&run(Some(copied()), None)), SUCCESS);
        assert_eq!(for_file_run(&run(Some(Err(err(Code::SafetyRejected))), None)), REFUSED);
    }
```

  and add to the test module's imports:

```rust
    use flux_core::lock::{LockCode, Refusal};
    use flux_core::run::{Run, RunError, RunStep};
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-cli --lib exit_code`
Expected: `error[E0425]: cannot find function `for_tree_run``.

- [ ] **Step 3: Implement.** In `exit_code.rs`, add `use flux_core::run::{Run, RunError};` to the imports, replace the
  `Err(a) if !a.changed() && a.outcome.failures.is_empty() && is_refusal(a.error.code()) => {` arm of `for_tree` (and
  its body `REFUSED` and closing `}`) with `Err(a) if a.refused_unchanged() => REFUSED,`, delete `fn is_refusal` and its
  doc comment, and add after `for_file`:

```rust
/// A run under the destination's lock (cut 7a): its own stop decides first - a refusal that changed nothing is 3
/// (§55), any other stop is 1 - and with no stop the copy's own rule decides. Warnings never change it.
pub fn for_tree_run(r: &Run<Result<TreeOutcome, TreeAbort>>) -> u8 {
    for_stop(&r.stop).unwrap_or_else(|| for_tree(r.copy.as_ref().expect("a run with no stop has a copy")))
}

/// `for_tree_run`, for a single file.
pub fn for_file_run(r: &Run<Result<Outcome, CopyError>>) -> u8 {
    for_stop(&r.stop).unwrap_or_else(|| for_file(r.copy.as_ref().expect("a run with no stop has a copy")))
}

fn for_stop(stop: &Option<RunError>) -> Option<u8> {
    match stop {
        None => None,
        Some(RunError::Refused { changed: false, .. }) => Some(REFUSED),
        Some(_) => Some(FAILED),
    }
}
```

  If `Code` is then unused outside the tests, keep its import only where it is used (clippy denies an unused import).

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-cli --lib exit_code`
Expected: all `exit_code` tests pass, the 7 existing and the 2 new.

- [ ] **Step 5: Prove the tests can fail** (restore after each): `for_stop` returning `Some(FAILED)` for every stop -
  both new tests FAILED; `for_stop` returning `None` for a stop - both FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-cli/src/exit_code.rs
git commit -m "feat(cli): the exit status of a run - its own stop first, then the copy's rule (cut 7a Part 3b-2)"
```

---

### Task 3: How a run's stop and warnings read on stderr

**Files:**
- Modify: `crates/flux-cli/src/report.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing tests** at the end of the `mod tests` in `report.rs`:

```rust
    fn holder() -> flux_core::lock::record::LockRecord {
        flux_core::lock::record::LockRecord {
            complete_lock_key: "k".to_string(),
            operation_id: "1".repeat(32),
            owner_instance_id: "2".repeat(32),
            boot_session_id: "boot".to_string(),
            target_path_key: "t".to_string(),
            workspace_path: "operations/x".to_string(),
            creation_wall_time: 5,
            last_heartbeat_wall_time: 7,
        }
    }

    #[test]
    fn a_refusal_prints_its_code_and_detail_then_one_line_per_holder_field() {
        let e = RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::TargetLockBusy,
                holder: Some(holder()),
                detail: "/p/d.flux-lock: another run holds the lock; wait for it to finish".to_string(),
            }),
            changed: false,
            not_removed: None,
        };
        assert_eq!(
            stop_lines(&e),
            vec![
                "TARGET_LOCK_BUSY: /p/d.flux-lock: another run holds the lock; wait for it to finish".to_string(),
                format!("  holder owner_instance_id: {}", "2".repeat(32)),
                "  holder boot_session_id: boot".to_string(),
                "  holder last_heartbeat_wall_time: 7".to_string(),
                "  holder workspace_path: operations/x".to_string(),
            ]
        );
    }

    #[test]
    fn a_refusal_names_what_it_could_not_remove_and_a_failure_names_its_step_and_path() {
        let e = RunError::Refused {
            refusal: Box::new(Refusal {
                code: LockCode::StateCorrupt,
                holder: None,
                detail: "D/m: not JSON".to_string(),
            }),
            changed: true,
            not_removed: Some((
                PathBuf::from("P/d.flux-lock"),
                FsError::new(Code::PermissionDenied, std::io::Error::other("denied")),
            )),
        };
        assert_eq!(
            stop_lines(&e),
            vec!["STATE_CORRUPT: D/m: not JSON".to_string(), "  not removed: P/d.flux-lock (denied)".to_string()]
        );
        let f = RunError::Failed {
            step: RunStep::State,
            path: PathBuf::from("D/m"),
            error: FsError::new(Code::DiskFull, std::io::Error::other("full")),
        };
        assert_eq!(stop_lines(&f), vec!["DISK_FULL: writing the operation's state at D/m: full".to_string()]);
    }

    #[test]
    fn every_run_warning_is_one_warning_line_naming_its_path() {
        let io = || FsError::new(Code::IoError, std::io::Error::other("busy"));
        let p = || PathBuf::from("X/the-path");
        let all = [
            RunWarning::BrokenLeftover(p()),
            RunWarning::NotRemoved { path: p(), error: io() },
            RunWarning::StateKept(p()),
            RunWarning::PartialKept { path: p(), error: io(), kept: PathBuf::from("X/kept") },
            RunWarning::OwnershipLostAfterCompletion(p()),
        ];
        for w in &all {
            let line = run_warning_line(w);
            assert!(line.starts_with("warning: ") && line.contains("the-path"), "{line}");
            assert!(!line.contains('\n'), "{line}");
        }
        assert!(run_warning_line(&all[3]).contains("X/kept"));
    }
```

  and add to the test module's imports:

```rust
    use flux_core::lock::{LockCode, Refusal};
    use flux_core::run::{RunError, RunStep, RunWarning};
```

- [ ] **Step 2: Run them to see them fail to compile**

Run: `cargo test -p flux-cli --lib report`
Expected: `error[E0425]: cannot find function `stop_lines``.

- [ ] **Step 3: Implement.** In `report.rs`, add `use flux_core::run::{RunError, RunWarning};` to the imports and add,
  after `summary_line`:

```rust
/// A run's stop (cut 7a), as stderr lines. A refusal is `CODE: detail` - the detail names the path and what to do next
/// (the refusal-guidance table) - then one indented line per field of the holder's record where it was readable
/// (§96.2), then what the run created and could not remove again. A failure of the run's own step is
/// `CODE: <step> at <path>: <error>`.
pub fn stop_lines(e: &RunError) -> Vec<String> {
    match e {
        RunError::Refused { refusal, not_removed, .. } => {
            let mut v = vec![format!("{}: {}", refusal.code.as_str(), refusal.detail)];
            if let Some(h) = &refusal.holder {
                v.push(format!("  holder owner_instance_id: {}", h.owner_instance_id));
                v.push(format!("  holder boot_session_id: {}", h.boot_session_id));
                v.push(format!("  holder last_heartbeat_wall_time: {}", h.last_heartbeat_wall_time));
                v.push(format!("  holder workspace_path: {}", h.workspace_path));
            }
            if let Some((path, why)) = not_removed {
                v.push(format!("  not removed: {} ({})", path.display(), why.source));
            }
            v
        }
        RunError::Failed { step, path, error } => vec![format!(
            "{}: {} at {}: {}",
            error.code.as_str(),
            step.as_str(),
            path.display(),
            error.source
        )],
    }
}

/// One of a run's warnings (F6: never a failure), as one stderr line.
pub fn run_warning_line(w: &RunWarning) -> String {
    match w {
        RunWarning::BrokenLeftover(p) => format!(
            "warning: a dead run's lock was moved aside to {} and could not be deleted; remove it when no Flux run is active",
            p.display()
        ),
        RunWarning::NotRemoved { path, error } => format!(
            "warning: could not remove {} ({}); the copy itself is complete",
            path.display(),
            error.source
        ),
        RunWarning::StateKept(p) => format!(
            "warning: a temporary this run could not remove keeps its state at {}, the record that names it",
            p.display()
        ),
        RunWarning::PartialKept { path, error, kept } => format!(
            "warning: could not delete the superseded partial {} ({}); its operation's state stays at {}",
            path.display(),
            error.source,
            kept.display()
        ),
        RunWarning::OwnershipLostAfterCompletion(p) => format!(
            "warning: the lock {} was taken over after this copy completed; it was left in place",
            p.display()
        ),
    }
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-cli --lib report`
Expected: every `report` test passes (the existing ones and the 3 new).

- [ ] **Step 5: Prove the tests can fail** (restore after each): drop the `holder workspace_path` line - the first new
  test FAILED; drop the `not removed` line - the second FAILED; make `run_warning_line` return `p.display()` without
  the `warning: ` prefix for `StateKept` - the third FAILED.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-cli/src/report.rs
git commit -m "feat(cli): a run's stop and warnings on stderr, the holder one field per line (cut 7a Part 3b-2)"
```

---

### Task 4: `flux copy` through the run

**Files:**
- Modify: `crates/flux-cli/src/main.rs`

The tests are written below; implement until they pass.

- [ ] **Step 1: Add the failing test** at the end of the `mod tests` in `main.rs`:

```rust
    #[test]
    fn break_lock_needs_restart_and_each_run_gets_fresh_ids() {
        assert!(Cli::try_parse_from(["flux", "copy", "a", "b", "--break-lock"]).is_err());
        let both = parse(&["--restart", "--break-lock"]);
        assert!(both.restart && both.break_lock);
        let cfg = run_config(&parse(&["--restart"]));
        assert!(cfg.restart && !cfg.break_lock);
        assert!(flux_core::ids::is_id(&cfg.operation_id) && flux_core::ids::is_id(&cfg.owner_instance_id));
        assert_ne!(cfg.operation_id, cfg.owner_instance_id);
        assert_ne!(run_config(&parse(&[])).operation_id, cfg.operation_id, "one id per invocation");
    }
```

- [ ] **Step 2: Run it to see it fail to compile**

Run: `cargo test -p flux-cli --bin flux`
Expected: `error[E0609]: no field `restart` on type `CopyArgs``.

- [ ] **Step 3: Implement.** In `main.rs`:

  (a) Add after the `json: bool,` field of `CopyArgs` (`:57`):

```rust
    /// Supersede every resumable prior operation on DEST: its state is marked ABANDONED and its partial files are
    /// deleted, then this copy runs.
    #[arg(long)]
    restart: bool,
    /// With --restart: take over a lock that a crashed run left empty or unreadable (§240.5).
    #[arg(long, requires = "restart")]
    break_lock: bool,
```

  (b) Add `use flux_core::run::{RunConfig, RunError, RunWarning};` to the imports.

  (c) In `fn options`, replace `operation_id: OperationId::new(format!("{}", std::process::id())),` with:

```rust
        // A placeholder: the run replaces it with its own operation id (`RunConfig::operation_id`).
        operation_id: OperationId::new(String::new()),
```

  (d) Add after `fn options`:

```rust
/// The run's configuration: two fresh ids for this invocation (the operation and this process, cut 7a) and the boot
/// session; in a debug build, the crash hook `debug_hook` reads from the environment.
fn run_config(args: &CopyArgs) -> RunConfig {
    RunConfig {
        restart: args.restart,
        break_lock: args.break_lock,
        operation_id: flux_core::ids::new_id(),
        owner_instance_id: flux_core::ids::new_id(),
        boot_session_id: flux_platform::boot_session_id(),
        before_mutation: debug_hook(),
    }
}

/// Part 3b-2, decision 2 (debug builds only): with `FLUX_TEST_STALL_AT=<n>` and `FLUX_TEST_STALL_FILE=<path>`, the
/// run creates `<path>` and then blocks forever just before its n-th guarded mutation, so an end-to-end test can kill
/// it at a known point. A release build has no hook.
#[cfg(debug_assertions)]
fn debug_hook() -> Option<flux_core::run::BeforeMutation> {
    let at: u64 = std::env::var("FLUX_TEST_STALL_AT").ok()?.parse().ok()?;
    let file = PathBuf::from(std::env::var_os("FLUX_TEST_STALL_FILE")?);
    let seen = std::sync::atomic::AtomicU64::new(0);
    Some(std::sync::Arc::new(move || {
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1 == at {
            let _ = std::fs::write(&file, b"stalled");
            loop {
                std::thread::sleep(std::time::Duration::from_secs(60));
            }
        }
    }))
}

#[cfg(not(debug_assertions))]
fn debug_hook() -> Option<flux_core::run::BeforeMutation> {
    None
}
```

  (e) Replace `fn copy` (`:97-173`) with:

```rust
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
    let cfg = run_config(args);
    let fs = flux_platform::StdFileSystem;
    match job {
        Job::Tree { src, dst } => {
            let started = Instant::now();
            let run = flux_core::run::tree(&fs, &src, &dst, &opts, &cfg, &mut |f| {
                for line in report::record_lines(&f) {
                    err(&line);
                }
            });
            let ms = millis(started);
            let rep = match &run.copy {
                Some(result) => {
                    let (outcome, aborted) = match result {
                        Ok(out) => (out, false),
                        Err(a) => {
                            err(&a.error.to_string());
                            (&a.outcome, true)
                        }
                    };
                    for line in report::warning_lines(&outcome.warnings) {
                        err(&line);
                    }
                    run_lines(&run.stop, &run.warnings);
                    let mut rep = Report::tree(outcome, aborted, ms);
                    rep.errors += u64::from(run.stop.is_some());
                    err(&report::summary_line(&rep, outcome.directories_created));
                    rep
                }
                None => {
                    run_lines(&run.stop, &run.warnings);
                    Report::pre_engine(false)
                }
            };
            json(args, &rep);
            exit_code::for_tree_run(&run)
        }
        Job::File { src, dst, target_existed } => {
            let started = Instant::now();
            let run = flux_core::run::file(&fs, &src, &dst, &opts, &cfg);
            let ms = millis(started);
            let rep = match &run.copy {
                Some(result) => {
                    match result {
                        Ok(o) => {
                            let target = dst.display().to_string();
                            for m in &o.metadata_failures {
                                err(&report::complaint_line(&target, m));
                            }
                            if let Some(w) = o
                                .identity_degraded
                                .as_ref()
                                .and_then(|d| report::file_warning(d, &dst))
                            {
                                err(&w);
                            }
                        }
                        // `CopyError`'s Display is "CODE: source" plus a staging temporary that
                        // could not be removed - the one thing the user needs to clean up.
                        Err(e) => err(&e.to_string()),
                    }
                    run_lines(&run.stop, &run.warnings);
                    let mut rep = Report::file(result, target_existed, ms);
                    rep.errors += u64::from(run.stop.is_some());
                    err(&report::summary_line(&rep, 0));
                    rep
                }
                None => {
                    run_lines(&run.stop, &run.warnings);
                    Report::pre_engine(false)
                }
            };
            json(args, &rep);
            exit_code::for_file_run(&run)
        }
    }
}

/// The run's own stop and warnings (cut 7a), after the copy's lines and before the summary.
fn run_lines(stop: &Option<RunError>, warnings: &[RunWarning]) {
    if let Some(e) = stop {
        for line in report::stop_lines(e) {
            err(&line);
        }
    }
    for w in warnings {
        err(&report::run_warning_line(w));
    }
}
```

- [ ] **Step 4: Run the tests to see them pass**

Run: `cargo test -p flux-cli`
Expected: every unit test passes (the new one included), and every test in `tests/copy.rs` still passes - each copy
now runs under the lock, and each leaves nothing behind.

- [ ] **Step 5: Prove the test can fail.** Drop `requires = "restart"`: the new test FAILED. Restore.

- [ ] **Step 6: Commit**

```bash
git add crates/flux-cli/src/main.rs
git commit -m "feat(cli): flux copy runs under the destination's lock, with --restart and --break-lock (cut 7a Part 3b-2)"
```

---

### Task 5: The end-to-end tests (the spec's Testing item 4)

**Files:**
- Create: `crates/flux-cli/tests/run.rs`

These tests exercise Tasks 1-4 together on the real filesystem. Write the file, then run it.

- [ ] **Step 1: Write the tests.** Create `crates/flux-cli/tests/run.rs`:

```rust
//! `flux copy` under the destination's lock (cut 7a Part 3b-2): the design's Testing item 4, end to end on the real
//! filesystem. The crash hook exists only in a debug build, which is what `cargo test` builds.

use flux_core::lock::record::LockRecord;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn flux() -> Command {
    Command::new(env!("CARGO_BIN_EXE_flux"))
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// `src/a` (1 byte) and `src/sub/b` (2 bytes) under `d`.
fn tree_in(d: &Path) -> PathBuf {
    let src = d.join("src");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a"), b"A").unwrap();
    std::fs::write(src.join("sub").join("b"), b"BB").unwrap();
    src
}

/// The names in `dir`, sorted.
fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn os(s: &str) -> &std::ffi::OsStr {
    std::ffi::OsStr::new(s)
}

fn copy(args: &[&std::ffi::OsStr]) -> Output {
    flux().arg("copy").args(args).output().unwrap()
}

/// A `flux copy src dst` stopped just before its `at`-th guarded mutation, holding what it holds there (the debug
/// build's hook, Part 3b-2). It is killed when dropped.
struct Stalled(Child);

impl Stalled {
    fn start(src: &Path, dst: &Path, at: u32, marks: &Path) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = flux()
            .arg("copy")
            .arg(src)
            .arg(dst)
            .env("FLUX_TEST_STALL_AT", at.to_string())
            .env("FLUX_TEST_STALL_FILE", &announced)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut s = Stalled(child);
        let deadline = Instant::now() + Duration::from_secs(60);
        while !announced.exists() {
            if let Some(status) = s.0.try_wait().unwrap() {
                panic!("the run exited before its stall point: {status}");
            }
            assert!(Instant::now() < deadline, "the run never reached its stall point");
            std::thread::sleep(Duration::from_millis(20));
        }
        s
    }

    /// A crash: the process dies holding what it held.
    fn kill(mut self) {
        self.0.kill().unwrap();
        self.0.wait().unwrap();
    }
}

impl Drop for Stalled {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_tree_copy_and_a_file_copy_leave_no_state_or_lock_behind() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"]);
    let t = d.path().join("t");
    let out = copy(&[src.join("a").as_os_str(), t.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["dst", "src", "t"]);
}

#[test]
fn a_killed_run_leaves_a_resumable_operation_that_restart_supersedes() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // `a`'s guarded mutations: its sweep (1), its temporary (2), its publish (3): killed with the temporary written.
    Stalled::start(&src, &dst, 3, marks.path()).kill();
    let left = names(&dst);
    assert!(left.iter().any(|n| n.starts_with("a.flux-partial.")), "{left:?}");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("RESUMABLE_OPERATION_EXISTS") && e.contains("--restart"), "{e}");
    let out = copy(&[os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"], "the killed run's temporary and state are gone");
    assert_eq!(names(d.path()), ["dst", "src"], "and its lock");
}

#[test]
fn a_live_holder_makes_another_run_busy_and_names_the_lock_and_the_holder() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let holder = Stalled::start(&src, &dst, 1, marks.path());
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("TARGET_LOCK_BUSY") && e.contains("dst.flux-lock"), "{e}");
    assert!(e.contains("  holder owner_instance_id: "), "§96.2: {e}");
    holder.kill();
}

#[test]
fn an_empty_lock_is_uncertain_until_restart_break_lock_takes_it_over() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(d.path().join("dst.flux-lock"), b"").unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("TARGET_LOCK_UNCERTAIN") && e.contains("--restart --break-lock"), "{e}");
    let out = copy(&[os("--restart"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "--restart alone never takes a lock over: {}", stderr(&out));
    assert!(stderr(&out).contains("TARGET_LOCK_UNCERTAIN"), "{}", stderr(&out));
    let out = copy(&[os("--break-lock"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(2), "--break-lock alone is a usage error: {}", stderr(&out));
    let out = copy(&[os("--restart"), os("--break-lock"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(0), "{}", stderr(&out));
    assert_eq!(names(&dst), ["a", "sub"]);
    assert_eq!(names(d.path()), ["dst", "src"]);
}

#[test]
fn unreadable_or_newer_state_is_refused_and_preserved() {
    let corrupt: &[u8] = b"garbage";
    let newer: &[u8] = br#"{"format_version":2}"#;
    for (bytes, code) in [(corrupt, "STATE_CORRUPT"), (newer, "INCOMPATIBLE_STATE")] {
        let d = TempDir::new().unwrap();
        let src = tree_in(d.path());
        let dst = d.path().join("dst");
        let workspace = dst.join(".flux").join("operations").join("1".repeat(32));
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("manifest"), bytes).unwrap();
        let out = copy(&[src.as_os_str(), dst.as_os_str()]);
        assert_eq!(out.status.code(), Some(3), "{code}: {}", stderr(&out));
        assert!(stderr(&out).contains(code), "{}", stderr(&out));
        assert_eq!(std::fs::read(workspace.join("manifest")).unwrap(), bytes, "{code}: preserved");
        assert!(!dst.join("a").exists(), "{code}: nothing copied");
    }
}

#[test]
fn a_foreign_object_at_dest_flux_is_a_control_plane_conflict() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::create_dir(&dst).unwrap();
    std::fs::write(dst.join(".flux"), b"not Flux's").unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));
    assert_eq!(names(&dst), [".flux"]);
}

#[test]
fn a_source_entry_landing_in_a_reserved_control_path_fails_alone() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    std::fs::create_dir_all(src.join(".flux").join("operations")).unwrap();
    std::fs::write(src.join(".flux").join("operations").join("x"), b"x").unwrap();
    std::fs::write(src.join(".flux").join("other"), b"data").unwrap();
    let dst = d.path().join("dst");
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(1), "{}", stderr(&out));
    assert!(stderr(&out).contains("CONTROL_PLANE_NAMESPACE_CONFLICT"), "{}", stderr(&out));
    assert_eq!(std::fs::read(dst.join(".flux").join("other")).unwrap(), b"data", "the rest of .flux is data");
    assert!(!dst.join(".flux").join("operations").exists());
    assert_eq!(std::fs::read(dst.join("sub").join("b")).unwrap(), b"BB");
}

#[test]
fn a_dead_owners_record_naming_a_missing_workspace_is_artifact_ownership_uncertain() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    let id = "a".repeat(32);
    let record = LockRecord {
        complete_lock_key: "none/00".to_string(),
        operation_id: id.clone(),
        owner_instance_id: "b".repeat(32),
        boot_session_id: "unknown".to_string(),
        target_path_key: "00".to_string(),
        workspace_path: format!("operations/{id}"),
        creation_wall_time: 1,
        last_heartbeat_wall_time: 1,
    }
    .encode();
    let lock = d.path().join("dst.flux-lock");
    std::fs::write(&lock, &record).unwrap();
    let out = copy(&[src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let e = stderr(&out);
    assert!(e.contains("ARTIFACT_OWNERSHIP_UNCERTAIN"), "{e}");
    assert!(e.contains(&format!("  holder workspace_path: operations/{id}")), "{e}");
    assert_eq!(std::fs::read(&lock).unwrap(), record, "preserved");
}

#[test]
fn a_target_name_too_long_for_its_state_record_is_refused_before_anything_is_made() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let long = d.path().join("n".repeat(230));
    let out = copy(&[src.join("a").as_os_str(), long.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    assert!(stderr(&out).contains("PATH_COMPONENT_INVALID"), "{}", stderr(&out));
    assert_eq!(names(d.path()), ["src"]);
}

#[test]
fn json_on_a_refusal_has_exactly_the_section_53_fields_and_one_error() {
    let d = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    std::fs::write(d.path().join("dst.flux-lock"), b"").unwrap();
    let out = copy(&[os("--json"), src.as_os_str(), dst.as_os_str()]);
    assert_eq!(out.status.code(), Some(3), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v.as_object().unwrap().len(), 18, "§53's complete field list: {v}");
    assert_eq!(v["errors"].as_u64(), Some(1));
}
```

- [ ] **Step 2: Run them**

Run: `cargo test -p flux-cli --test run`
Expected: `test result: ok. 10 passed`. If one fails, find out why by measurement (its stderr is in the assertion
message) and report it - never loosen an assertion to make it pass.

- [ ] **Step 3: Prove they can fail** (restore after each):
  - in `crates/flux-core/src/run/session.rs`, make `open_operation`'s scan arm accept resumable priors without
    `--restart` (`Ok(scan) if scan.resumable.is_empty() || cfg.restart || true => ...`):
    `a_killed_run_leaves_a_resumable_operation_that_restart_supersedes` FAILED;
  - in `crates/flux-core/src/run/session.rs`, take an uncertain lock over under `--restart` alone
    (`let mode = if cfg.break_lock || cfg.restart { ... }`): `an_empty_lock_is_uncertain_until_restart_break_lock...`
    FAILED (panel round 1);
  - in `crates/flux-cli/src/report.rs`, drop the holder lines: `a_live_holder_makes_another_run_busy...` and
    `a_dead_owners_record...` FAILED;
  - in `crates/flux-cli/src/main.rs`, in both `None =>` arms of `copy`, print
    `Report { errors: 0, ..Report::pre_engine(false) }` instead of `Report::pre_engine(false)`:
    `json_on_a_refusal_has_exactly_the_section_53_fields_and_one_error` FAILED.

- [ ] **Step 4: Commit**

```bash
git add crates/flux-cli/tests/run.rs
git commit -m "test(cli): flux copy under the lock, end to end - refusals, --restart, --break-lock and a real crash (cut 7a Part 3b-2)"
```

---

### Task 6: `TODO.md` and the spec's "What this closes"

**Files:**
- Modify: `TODO.md` (`:143`, `:177`, `:354`)
- Modify: `docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md` ("What this closes")

- [ ] **Step 1: Close two items.** In `TODO.md`, change `- [ ] **A leftover temporary from a PREVIOUS run is never
  removed**` to `- [x]` and append to that item, as a new paragraph indented like its body:

```markdown
  DONE in cut 7a: each operation's id is persisted in its state, and `flux copy --restart` deletes every
  `<name>.flux-partial.<id>` of the operation it supersedes (the design's `--restart`, step 3).
```

  Change `- [ ] **WSL 9p mounts break two lock assumptions.**` to `- [x]` and append:

```markdown
      DONE in cut 7a: 9p is not on the lock-capability allowlist, so a copy onto such a mount is refused
      `REMOTE_LOCK_UNSAFE` before anything is created.
```

- [ ] **Step 2: Record, without closing, the blocking-temporary item.** Append to the item `**A blocking
  pre-existing temporary is not reported.**` (keep `- [ ]`):

```markdown
      (Cut 7a: the operation id is now persisted and per operation, so a blocking temporary names an operation
      whose state finds it; but the copy still returns `leftover: None` in this case, so the reporting gap stays
      open.)
```

- [ ] **Step 3: Correct the spec.** In the design spec's "What this closes", replace the bullet
  `- "A blocking pre-existing temporary is not reported": the id is persisted, so a blocking temporary names its
  operation.` with:

```markdown
- Not closed: "A blocking pre-existing temporary is not reported". The id is persisted, so such a temporary names its
  operation, but the copy still reports no leftover in that case (`TODO.md`).
```

- [ ] **Step 4: Check and commit**

Run: `just typos` (expected: no finding).

```bash
git add TODO.md docs/superpowers/specs/2026-09-30-cut-7a-destination-lock-design.md
git commit -m "docs: TODO and the spec's closures after cut 7a's CLI (Part 3b-2)"
```

---

### Task 7: Verify, push, CI

- [ ] **Step 1: The gates**

Run: `just check` - expected `Summary [...] 557 tests run: 557 passed, 3 skipped` (538 + 3 core + 6 CLI unit + 10
end-to-end). Run: `just check-linux` - expected `543 passed` (524 + 19). Run: `just check-mac` - a clean `Finished`.
(The counts are estimates; a difference is reported, not forced.)

- [ ] **Step 2: Push and watch CI**

```bash
git push origin spec/cut-7a
```

Then `gh run list --branch spec/cut-7a --limit 1` and `gh run watch <id> --exit-status`: every job green, including
the end-to-end tests on Windows, Ubuntu and macOS.

- [ ] **Step 3: Record** the commits and gates in the execution ledger (memory), then the AGY-CAPSTONE over
  `<plan commit>..HEAD` (code and tests, excluding `docs/` and `Cargo.lock`).

## Self-review

- **Spec coverage, Testing item 4:** no `.flux` state left (tree and file) - `a_tree_copy_and_a_file_copy...`; each
  refusal's exit status and message - BUSY, UNCERTAIN, RESUMABLE, STATE_CORRUPT, INCOMPATIBLE_STATE,
  CONTROL_PLANE_NAMESPACE_CONFLICT, ARTIFACT_OWNERSHIP_UNCERTAIN, PATH_COMPONENT_INVALID, usage 2 (REMOTE_LOCK_UNSAFE:
  decision 5); a reserved-path source entry - `a_source_entry_landing...`; `--restart` supersedes a killed run's state,
  and the real crash - `a_killed_run_leaves...`; `--break-lock` clears an empty lock - `an_empty_lock_is_uncertain...`.
- **"Errors and exit statuses":** every row's exit status is produced by `for_stop` or the copy's own rule (Task 2).
- **Types:** `BeforeMutation`, `RunConfig::before_mutation`, `for_tree_run`, `for_file_run`, `stop_lines`,
  `run_warning_line`, `run_config`, `debug_hook`, `run_lines` are each defined once and used with the same names.

## Stand-downs

- **Panel (agy, 2 rounds): GREEN at round 2.** Briefs `.clavity/seams/cut7a-p3b2-plan-panel-r1.md` and `-r2.md`, replies
  `.clavity/scratch/cut7a-p3b2-plan-panel/r1-reply.md` and `r2-reply.md`. FOLDED (round 1, `030375a`): `--restart` alone
  against an empty lock was untested; Task 5 now asserts exit 3 and `TARGET_LOCK_UNCERTAIN`, with its mutant.
- REJECTED (round 1): "the tree copy is concurrent, so the hook's count is reordered" - `walk_into` is one sequential loop
  over a walk sorted by name (`crates/flux-core/src/tree.rs` `walk_into`, `walk.rs` `sorted`), so `a`'s three mutations
  always precede `sub`'s. Withdrawn by the reviewer.
- REJECTED (round 1): "`Code` is not imported in `main.rs`" - `crates/flux-cli/src/main.rs:8` imports it. Withdrawn.
- REJECTED (round 2): "the new tests in `exit_code.rs` and `report.rs` cannot see `FsError` or `Code`" - both test
  modules import `FsError` (`exit_code.rs:46`, `report.rs:220`) and reach `Code` through `use super::*`. Withdrawn.

