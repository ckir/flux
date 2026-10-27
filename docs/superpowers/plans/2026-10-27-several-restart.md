# `flux copy --restart` with several sources (cut 17) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `flux copy --restart [--break-lock] a b c DEST` supersedes every resumable prior at DEST and copies again, immediately after a crash, with honest refusal texts.

**Architecture:** One narrow engine fix (`MultiPlace::remove` releases this run's prefix locks only for its own operation id) lets the existing `open_operation` restart path (`crates/flux-core/src/run/session.rs`) run unchanged for several sources. A second seam in `MultiPlace::prefix_error` rewrites two refusal details that today promise help that cannot come. The CLI gate loses its `--restart` arm; four texts stop naming `flux cleanup --force`. `--break-lock` keeps covering the destination lock only (owner decision P1).

**Tech Stack:** Rust 2024 workspace; `FaultFs` and the lock fakes for engine tests; the real binary with the debug-build stall hook for the CLI test. No new dependency.

**Spec:** `docs/superpowers/specs/2026-10-27-several-restart-design.md` (design panel GREEN at d2246b55c71c52bb0655293dd30aeb3606542ba2). Line numbers below are of the tree at that commit.

## Global Constraints

- No state format change: format 5 and format 4 manifests stay byte-identical (design G4). `takeover` stays one `Option<Takeover>` (`crates/flux-core/src/state.rs:369`).
- `--break-lock` applies to the destination lock only; `PrefixLocks::take` keeps `Mode::Plain` (design 3.4, owner P1).
- `--resume` and `--dry-run` with several sources stay usage errors, exit 2, no JSON (design N1, N4).
- A prefix lock refusal detail never contains `--break-lock`; an `ARTIFACT_OWNERSHIP_UNCERTAIN` prefix refusal never contains `flux cleanup` (design A2, 3.4).
- The `Place` trait does not change (design 3.2).
- Every new test is proven non-vacuous with the logic mutant its step names; the mutant is reverted before the commit.
- Gate: `just check` as is (`justfile:71`: `fmt-check clippy typos unused-deps doc-check license test`; `doc-check` is `-D warnings` over private items, so every rewritten doc comment must still build). Windows-only lock behaviour is CI's to show.
- nextest filters below use a tail-anchored regex, `test(/(^|::)NAME$/)`, because a library unit test's name carries its module path (`run::multi::trees_tests::NAME`; MEASURED 2026-10-27: `cargo nextest list -E 'test(=NAME)'` lists nothing for such a test, while `test(=run::multi::trees_tests::NAME)` and `test(/::NAME$/)` list it) and an integration test's name is bare; a substring filter also selects neighbours (`stale_prefix_locks_of_a_crashed_run_...`, `a_resume_after_a_crash_...`, `a_live_prefix_lock_with_an_old_heartbeat_...`).
- Commit trailer: `Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>` (the implementer substitutes its own model name).

## Review Focus

1. A restart whose source set differs from the crashed run's: the prior's unused `<name>.flux-lock` stays (L6) and the run must still exit 0 (Task 4, T7).
2. `--restart --break-lock` with BOTH locks unreadable: nothing may be written; the rerun with the same flags must work (Task 5, T4b).
3. A live one-source copy into `DEST/<name>` while a several-source `--restart` runs: `TARGET_LOCK_BUSY`, and the live run's prior untouched (Task 5, T5).
4. Two resumable priors at once (a several-source crash, then a one-source restart that crashed before its ABANDONED write): one restart supersedes both (Task 5, T8).
5. A FILE root's partial sits beside it in DEST, not under a directory: the order probe counts `create_new` outside `DEST/.flux` (Task 4, T2).

---

### Task 1: `MultiPlace::remove` releases the prefix locks only for this run's id

**Files:**
- Modify: `crates/flux-core/src/run/multi.rs:300-309` (`remove`), `:443-448` (the `debug_assert!` and its comment), `:54` (the `RefCell` doc)
- Test: `crates/flux-core/src/run/multi.rs` `mod tests` (the one with `fx()` at `:675` and `place()` at `:695`)

**Interfaces:**
- Produces: `MultiPlace::remove(&self, id: &str)` unchanged in signature; for `id == self.prefix_id.operation_id` it releases the prefix locks (before `self.tree.remove(id)`, as today) and reports the first release failure as today; for any other id it returns `self.tree.remove(id)` and touches no lock. The `debug_assert!` at `:445-448` becomes `debug_assert!(!cfg.resume, "several sources do not support --resume")` and the comment at `:443-444` says adoption is the unsupported case.

- [ ] **Step 1: Write the failing tests** in the `fx()` module, after `create_orders_dest_then_locks_then_checks_then_workspace`:

```rust
#[test]
fn remove_of_a_prior_keeps_this_runs_prefix_locks() {
    let fx = fx();
    let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
    let mut warnings = Vec::new();
    p.create(&state(), &mut warnings).unwrap();
    let prior = "22222222222222222222222222222222";
    fx.fs.create_dir(Path::new(&format!("/p/dest/.flux/operations/{prior}"))).unwrap();
    p.remove(prior).unwrap();
    assert!(!fx.fs.exists(format!("/p/dest/.flux/operations/{prior}")), "the prior's workspace is retired");
    assert_eq!(lock_files(&fx.fs), vec!["/p/dest/data.flux-lock".to_string()], "this run's prefix lock is still held");
    assert!(p.prefix.borrow().is_some());
}

#[test]
fn remove_of_this_run_releases_its_prefix_locks() {
    let fx = fx();
    let mut p = place(&fx, vec![root("data", RootKind::Tree)]);
    let mut warnings = Vec::new();
    p.create(&state(), &mut warnings).unwrap();
    p.remove(ID).unwrap();
    assert!(lock_files(&fx.fs).is_empty(), "{:?}", lock_files(&fx.fs));
    assert!(p.prefix.borrow().is_none());
}
```

- [ ] **Step 2: Run them**: `cargo nextest run -p flux-core remove_of_` — expected: `remove_of_a_prior_keeps_this_runs_prefix_locks` FAILS on the `lock_files` assertion (the lock was released), `remove_of_this_run_releases_its_prefix_locks` passes.
- [ ] **Step 3: Implement** the id comparison in `remove` (`multi.rs:300-309`); narrow the `debug_assert!` and rewrite both comments (`:54`, `:443-444`).
- [ ] **Step 4: Run** `cargo nextest run -p flux-core remove_of_` — expected: 2 passed. Mutant: make `remove` ignore `id` again; expected: the first test red; revert.
- [ ] **Step 5: Commit** `feat: MultiPlace::remove keeps this run's prefix locks when retiring a prior (cut 17 task 1)`.

### Task 2: the prefix refusal seam

**Files:**
- Modify: `crates/flux-core/src/run/prefix.rs:44-51` (the enum), `:104`, `:107`, `:123`, `:147`, `:149` (every `PrefixRefusal::Lock` construction), `:229`, `:243`, `:260`, `:298` (test patterns); `crates/flux-core/src/run/multi.rs:130-143` (`prefix_error`); `crates/flux-core/src/lock/obtain.rs:61` (`UNTRUSTED_DETAIL` becomes `pub(crate)`), `crates/flux-core/src/lock/mod.rs:30` (`obtain` is a private module, `lock/mod.rs:15`, so add `UNTRUSTED_DETAIL` to the `pub(crate) use obtain::{..}` re-export block there; `run::multi` then names it `crate::lock::UNTRUSTED_DETAIL`)
- Test: `crates/flux-core/src/run/multi.rs` `trees_tests` (the module with `fake()` at `:1023`)

**Interfaces:**
- Produces: `PrefixRefusal::Lock { name: OsString, error: LockError }`. `MultiPlace::prefix_error` maps a `Lock` refusal through `from_lock(error, RunStep::Lock, &self.lock_path(&name), false)` (the path is the LOCK FILE, for every `Lock` refusal), after rewriting `refusal.detail` when `error` is `LockError::Refused(r)` and: `r.code == LockCode::TargetLockUncertain` -> `"this prefix lock has an unreadable record; if no Flux run is active on this destination, remove that file and run again with the same flags"`; `r.code == LockCode::ArtifactOwnershipUncertain` -> `UNTRUSTED_DETAIL` (no cleanup suffix). Other codes keep their detail.

- [ ] **Step 1: Write the failing tests** in `trees_tests`. First add the helper (refusal details are NOT slash-normalised, unlike `calls()`; `lock_path` joins with the platform separator, `multi.rs:93-97`, so on Windows the detail starts `/p/dest\a.flux-lock: `; the repo's portable idiom is `Path::new("/p/dest").join(..).display()`, `multi.rs:1614-1616`):

```rust
fn lock_prefix(name: &str) -> String {
    format!("{}: ", Path::new("/p/dest").join(format!("{name}.flux-lock")).display())
}
```

Then the tests:

```rust
#[test]
fn an_unreadable_prefix_lock_names_the_file_once_and_promises_no_break_lock() {
    let fs = fake();
    fs.create_dir(Path::new("/p/dest")).unwrap();
    fs.write_file("/p/dest/a.flux-lock", b""); // an empty record, nobody holds it: Uncertain under Mode::Plain
    let (r, _) = go(&fs, &["a", "b"]);
    let detail = match &r.run.stop {
        Some(RunError::Refused { refusal, changed: false, .. }) => {
            assert_eq!(refusal.code, LockCode::TargetLockUncertain);
            refusal.detail.clone()
        }
        other => panic!("expected TARGET_LOCK_UNCERTAIN, got {other:?}"),
    };
    assert!(detail.starts_with(&lock_prefix("a")), "{detail}");
    assert_eq!(detail.matches("a.flux-lock").count(), 1, "{detail}");
    assert!(detail.contains("the same flags") && !detail.contains("--break-lock"), "{detail}");
    assert!(!fs.exists("/p/dest/.flux"), "nothing was created");
}

#[test]
fn an_orphan_prefix_lock_says_remove_by_hand_not_cleanup() {
    let fs = fake();
    fs.create_dir(Path::new("/p/dest")).unwrap();
    // A one-source copy into DEST/a crashed (its record names operations/<id> under DEST/a), then DEST/a was removed.
    let dest = fs.destination_root(Path::new("/p/dest")).unwrap();
    let site = crate::lock::LockSite::directory(&dest, &OsString::from("a")).unwrap();
    let rec = crate::lock::test_support::record(&site, "33333333333333333333333333333333", "operations/33333333333333333333333333333333");
    crate::lock::test_support::dead_lock(&dest, "a.flux-lock", &rec.encode());
    let (r, _) = go(&fs, &["a", "b"]);
    let detail = match &r.run.stop {
        Some(RunError::Refused { refusal, .. }) => {
            assert_eq!(refusal.code, LockCode::ArtifactOwnershipUncertain);
            refusal.detail.clone()
        }
        other => panic!("expected ARTIFACT_OWNERSHIP_UNCERTAIN, got {other:?}"),
    };
    assert!(detail.starts_with(&lock_prefix("a")), "{detail}");
    assert!(detail.contains("remove it by hand") && !detail.contains("flux cleanup"), "{detail}");
}
```

The `record` helper's signature is `record(site, operation_id, workspace_path)` (`crates/flux-core/src/lock/test_support.rs:26`, heartbeat and creation time 1); `dead_lock` is at `:44-48`. If `test_support` is not reachable from `trees_tests`, add the `pub(crate) use` it needs; do not copy the helpers.

- [ ] **Step 2: Run** `cargo nextest run -p flux-core -E 'test(/(^|::)an_unreadable_prefix_lock_names_the_file_once_and_promises_no_break_lock$/) | test(/(^|::)an_orphan_prefix_lock_says_remove_by_hand_not_cleanup$/)'` — expected: both FAIL (the detail starts with `/p/dest: ` and the first contains `--break-lock`).
- [ ] **Step 3: Implement** the enum change, every construction and pattern site listed above, `pub(crate) const UNTRUSTED_DETAIL`, and `prefix_error` as the Interfaces block says. Also extend the existing Busy test `a_held_prefix_lock_on_the_last_root_refuses_and_nothing_is_copied` (`multi.rs:1520`) with `assert!(refusal.detail.starts_with(&lock_prefix("c")), "{}", refusal.detail);` (pins the path prefix for a non-Uncertain refusal).
- [ ] **Step 4: Run** the same filter plus `| test(/(^|::)a_held_prefix_lock_on_the_last_root_refuses_and_nothing_is_copied$/)` — expected: 3 passed. Mutants, one at a time: (a) pass `dest_shown` instead of `lock_path(&name)`: three tests red; (b) skip the `TargetLockUncertain` rewrite: the first test red; (c) skip the `ArtifactOwnershipUncertain` rewrite: the second red. Revert.
- [ ] **Step 5: Commit** `feat: prefix lock refusals name the lock file and promise only what helps (cut 17 task 2)`.

### Task 3: the CLI gate and the four texts

**Files:**
- Modify: `crates/flux-cli/src/main.rs:36-42` (the `Copy` doc), `:140-143` (`--restart` help), `:157-158` (`--break-lock` help), `:646-662` (`several_flag_gate`); `crates/flux-core/src/prior.rs:180-224` (`resumable_refusal`) and its test `:633-656`; `crates/flux-cli/src/report.rs:929-945` (`abort_hint_lines` and its doc) and its test `:1848-1859` (the `other` assertions at `:1852-1858` stay)
- Test: `crates/flux-cli/tests/several_sources.rs:150-176`

**Interfaces:**
- Produces: `several_flag_gate` returns `Some` only for `--resume` (`"error: --resume with several sources is not supported yet; run again with --restart to supersede an aborted several-source run"`) and `--dry-run` (text unchanged); `--restart` alone returns `None`. `resumable_refusal`'s version-5 single arm: `"operation {id} is {state} ({shown}), created by a several-source run, so --resume cannot continue it; run again with --restart to supersede it {wipe}"`; its several-prior suffix becomes `"; a several-source prior is superseded like the others"`. `abort_hint_lines(.., Some(dest))` returns `["run again with --restart and the same sources to supersede the aborted run"]`.

- [ ] **Step 1: Change the tests first.** In `prior.rs:634` rename the test to `a_version_5_prior_names_restart_not_resume_or_cleanup` and assert the parts `[id, "FAILED", "manifest", "--restart to supersede", "several-source"]`, `!d.contains("--resume to continue")`, `!d.contains("cleanup")`; for `many` assert `!many.contains("cleanup")` and `many.contains("--restart")`. In `report.rs:1848` rename to `a_wal_write_failure_hint_names_restart_for_several_sources` and assert the new line. In `several_sources.rs:150` the loop runs over `["--resume", "--dry-run"]`, with `err.contains("run again with --restart")` asserted for `--resume` only (pins P3); the cleanup assertion becomes `assert!(!err.contains("cleanup"), ..)`; add an arm: `--restart --dry-run` exits 2 with `--dry-run with several sources is not supported yet`; add a test `restart_with_break_lock_is_accepted_on_a_clean_dest` running `flux copy --restart --break-lock --json alpha beta out` and asserting exit 0 and both files copied.
- [ ] **Step 2: Run** `cargo nextest run -p flux-core -p flux-cli -E 'test(/(^|::)a_version_5_prior_names_restart_not_resume_or_cleanup$/) | test(/(^|::)a_wal_write_failure_hint_names_restart_for_several_sources$/) | test(/(^|::)resume_restart_and_dry_run_exit_2_naming_the_flag$/) | test(/(^|::)restart_with_break_lock_is_accepted_on_a_clean_dest$/)'` — expected: 4 failed (old texts; `--restart --break-lock` exits 2). (Add `#[allow]`-free code: after the rewrite `abort_hint_lines`'s `Some(dest)` binding is unused; write `Some(_)`, or clippy `-D warnings` goes red.)
- [ ] **Step 3: Implement** the texts and the gate; rewrite the three help/doc comments (`main.rs:36-42`: "--resume and --dry-run need exactly one source; --restart supersedes several-source priors too"; `:140-143` unchanged in substance; `:157`: "take over the DESTINATION lock that a crashed run left empty or unreadable (§240.5); a prefix lock is never taken over").
- [ ] **Step 4: Run** the same filter — expected: all passed. Then `cargo nextest run -p flux-cli -p flux-core` — expected: no other test red (the old strings are pinned nowhere else: `rg "cleanup --force" crates` lists only these sites and the cleanup command's own tests).
- [ ] **Step 5: Commit** `feat: --restart with several sources passes the CLI gate; texts name --restart (cut 17 task 3)`.

### Task 4: the several-source crash helper, T2, T3, T7

**Files:**
- Test: `crates/flux-core/src/run/multi.rs` `trees_tests`

**Interfaces:**
- Produces (test helpers in `trees_tests`): `fn crash(fs: &FaultFs, names: &[&str], op: &str, nth: u32)`: installs `fs.on_nth(op, nth, |_| panic!("the test's crash point"))` and runs `catch_unwind(AssertUnwindSafe(|| go(fs, names)))`, asserting `is_err()`; `nth` comes from `reference(fake, names, op, needle)` on an identical fixture, as every crash test here does. Unwinding drops the run's `FakeLock`s, which releases the fake OS locks and leaves every file (`fault_fs.rs:1050-1056`); no `settle` runs, so the prefix lock files, the destination lock and the TRANSFERRING manifest stay. (The helpers `manifest_of`, `nth_of_from` and `watch_manifest_of` are DEFINED in Task 5, which first calls them; a helper nothing calls is `dead_code` under ``clippy --all-targets -- -D warnings`, `justfile:21-22`.) `fn manifest_of(fs: &FaultFs, id: &str) -> OperationState` reads `/p/dest/.flux/operations/{id}/manifest`. `fn nth_of_from(fs: &FaultFs, from: usize, op: &str, needle: &str) -> u32`: like `nth_of` (`multi.rs:1066`) but returns the 1-based count among ALL `op(` calls of the first match at log position `>= from` (the fake's `on_nth` counts from the fake's creation, `fault_fs.rs:1495-1499`, and a crashed run's calls stay in the log, so a needle that also matches the crash phase, such as `operations/{ID}` which the workspace publish `rename_no_replace(<id>.creating -> <id>)` at `state.rs:998` carries, must be counted after a mark). `fn watch_manifest_of(fs, id, op, nth) -> Seen`: `watch_manifest` (`multi.rs:1095-1104`) with the id a parameter (`watch_manifest` itself reads the PRIOR's manifest, `ID`, which is what T2 needs). `fn restart_cfg(n: u8) -> RunConfig` is `RunConfig { operation_id: format!("{:032x}", n), restart: true, ..cfg() }`.

- [ ] **Step 1: Write T2** (`a_several_source_restart_supersedes_a_crashed_run_abandoning_it_first`):
  1. `let n = reference(fake, &["a", "b"], "rename_no_replace", "/p/dest/a/x")` (the publish of `a/x`); `fs = fake()`; `crash(&fs, &["a","b"], "rename_no_replace", n)`. Assert the crash state: `fs.exists(format!("/p/dest/a/x.flux-partial.{ID}"))`, `manifest(&fs).state == OpState::Transferring`, `prefix_lock_files(&fs).len() == 2`, `fs.exists(LOCK)`.
  2. Reference restart: `fs2` built the same way (crash again on a fresh `fake()`), then `go_with(&fs2, specs a b, &opts(), &restart_cfg(0x22))`, and `k = nth_of(&fs2, "rename_no_replace", &format!("operations/{ID}.removing"))` (the retiring rename, `state.rs:1032-1034`; the `.removing` suffix is unique to it, the publish rename ends in the bare id).
  3. The probed restart on `fs`: `let seen = watch_manifest(&fs, "rename_no_replace", k)` (it reads `operations/{ID}/manifest`, the prior's); `let mark = fs.calls().len()`; run `go_with(.., &restart_cfg(0x22))`; `ok(&r)`.
  4. Assert: `seen` is `ABANDONED` with `superseded_by == Some(restart id)`; `!fs.exists(format!("/p/dest/a/x.flux-partial.{ID}"))`; `!fs.exists(format!("/p/dest/.flux/operations/{ID}"))`; `fs.read_file("/p/dest/a/x").as_deref() == Some(&b"ax"[..])` and the same for `/p/dest/b/k` with `bk`; ORDER: in `calls(&fs)[mark..]`, the index of the first entry starting with `create_new(` whose path does not start with `/p/dest/.flux` is lower than the index of the entry EQUAL to `remove_file(/p/dest/a.flux-lock)` and of the one equal to `remove_file(/p/dest/b.flux-lock)` (exact strings: recovery removes `<name>.flux-lock.broken.<restart id>`, `recover.rs:48, 89`, which a prefix match would hit); END STATE: `lock_files(&fs).is_empty()` and `!fs.exists(format!("/p/dest/{n}.flux-lock.broken.{restart id}"))` for `n` in `a`, `b`.
- [ ] **Step 2: Write T3** (`a_restart_right_after_a_crash_does_not_wait_for_a_lease`): same crash; read the destination lock record (`fs.read_file(LOCK)` decoded with `crate::lock::record::decode`) and assert `last_heartbeat_wall_time == creation_wall_time` of the crashed run (a "now" heartbeat); restart with `restart_cfg(0x23)` and `ok(&r)`. Comment that `classify`'s `Dead` arm reads no heartbeat (`classify.rs:54-57`) and the mutant is the cleanup lease gate (`obtain.rs:239-249`) applied to `obtain`'s `Dead` arm.
- [ ] **Step 3: Write T7** (`a_restart_with_other_sources_leaves_the_unused_prefix_lock_and_succeeds`): crash `["a", "b"]` as in T2; restart with `["a", "c"]` and `restart_cfg(0x24)`; `ok(&r)`; assert `fs.exists("/p/dest/b.flux-lock")` (L6 residue, named on purpose), `!fs.exists("/p/dest/a.flux-lock")`, `!fs.exists("/p/dest/c.flux-lock")`, `!fs.exists(LOCK)`, and `/p/dest/c/z == b"cz"`.
- [ ] **Step 4: Run** `cargo nextest run -p flux-core -E 'test(/(^|::)a_several_source_restart_supersedes_a_crashed_run_abandoning_it_first$/) | test(/(^|::)a_restart_right_after_a_crash_does_not_wait_for_a_lease$/) | test(/(^|::)a_restart_with_other_sources_leaves_the_unused_prefix_lock_and_succeeds$/)'` — expected: 3 passed. Mutants: (a) in `supersede` (`restart.rs:38`) skip `place.write(&abandoned)`: T2 red on `seen`; (b) Task 1's `remove` ignoring the id: T2 red on ORDER (only T2's ORDER assertion goes red; T7 stays green); (c) the cleanup lease gate (`obtain.rs:239-249`, a clock plus the threshold, several lines) copied into `obtain`'s `Dead` arm: T3 red (T2 and T7 red too); (d) remove every `*.flux-lock` in DEST at `finish`: T7 red. Revert each.
- [ ] **Step 5: Commit** `test: a several-source restart over a crashed run, its order, its lease and its residue (cut 17 task 4)`.

### Task 5: T4 (destination takeover), T4b, T5, T6 mirror, T8

**Files:**
- Test: `crates/flux-core/src/run/multi.rs` `trees_tests`

**Interfaces:**
- Consumes: `crash`, `restart_cfg` from Task 4.
- Produces (defined here, first used here): `manifest_of`, `nth_of_from`, `watch_manifest_of` as specified in Task 4's Interfaces block.

- [ ] **Step 1: T4, destination half** (`break_lock_takes_over_an_empty_destination_lock_for_several_sources`): `fs = fake()`; `fs.create_dir("/p/dest")`; `fs.write_file(LOCK, b"")`; the manifest must be read DURING the run, because a completed run retires its own workspace (`remove_own`, `run/mod.rs:1186`; precedent `run/tests.rs:319-331`): reference run on an identical fixture to get `k = nth_of(&fs2, "rename_no_replace", "operations/<0x25 id>.removing")`, then on `fs`: `let seen = watch_manifest_of(&fs, <0x25 id>, "rename_no_replace", k)`; `go_with(.., &RunConfig { break_lock: true, ..restart_cfg(0x25) })`; `ok(&r)`; `seen.lock().unwrap().as_ref().unwrap().takeover.is_some()`; `!fs.exists(LOCK)` at the end.
- [ ] **Step 2: T4b** (`both_locks_unreadable_refuses_at_the_prefix_lock_and_writes_nothing`): the T4 fixture (`fs.create_dir("/p/dest")` first; `write_file` creates no parents) plus `fs.write_file(LOCK, b"")` and `fs.write_file("/p/dest/a.flux-lock", b"")`; same config; assert `TargetLockUncertain` with the detail of Task 2; `fs.read_file(LOCK).as_deref() == Some(&b""[..])` (bytes unchanged); `!fs.exists("/p/dest/.flux/operations/<0x25 id>")`.
- [ ] **Step 3: T5** (`a_live_prefix_lock_refuses_a_restart_and_leaves_the_prior_untouched`): crash `["a", "b"]` (Task 4); then hold `a` live: `PrefixLocks::acquire(&dest, LockCapability::LocalStrong, &os(&["a"]), &other.as_ref())` with `other` as at `multi.rs:1524-1529` (it recovers the dead lock and holds it); snapshot `before = (fs.read_file(manifest path of ID), fs.exists(partial), fs.exists(workspace))`; `go_with(.., &restart_cfg(0x26))`; assert `TargetLockBusy`, `refusal.detail.starts_with(&lock_prefix("a"))`, and the snapshot unchanged. Mutant (single point): in `MultiPlace::create` pass `&[]` instead of `&names` to `PrefixLocks::acquire` (`multi.rs:255`): no prefix lock is taken, nothing is Busy, the restart supersedes the live run's prior, and both halves go red.
- [ ] **Step 4: T6 mirror** (`a_several_source_restart_supersedes_a_failed_one_source_tree_run`; mutant: in `scan_tree`'s resumable push (the `if state.state.is_resumable()` inside `scan_tree`, `prior.rs:45-112`; NOT the one in `scan_file` at `:169`), add `&& state.format_version >= MULTI_ROOT_FORMAT`, so a format-4 prior is never listed): `n = ` the `create_dir` index of `/p/dest/sub` in a reference `crate::run::tree(&fs, "/src/a", "/p/dest", ..)` run (same `nth_of` technique as `multi.rs:1824`); `fs.fail_nth("create_dir", n, Code::IoError, ErrorKind::Other)`; run `tree` with `cfg()`; assert its manifest `Failed`; then `go_with(a b, restart_cfg(0x27))`; `ok(&r)`; `!fs.exists(format!("/p/dest/.flux/operations/{ID}"))`; `lock_files(&fs).is_empty()`.
- [ ] **Step 5: T8** (`one_restart_supersedes_a_crashed_run_and_a_restart_that_crashed_before_abandoning_it`): crash `["a","b"]` (prior 1 = `ID`); then a one-source tree restart (`crate::run::tree` with `RunConfig { restart: true, operation_id: id2, ..cfg() }`, `id2 = format!("{:032x}", 0x28)`, wrapped in its own `catch_unwind(AssertUnwindSafe(..))` as `run/tests.rs:1477-1482` does, because `crash` wraps `go`) crashed at its ABANDONED write of the prior: `nth_of_from(&fs2, mark2, "create_new", &format!("operations/{ID}/manifest"))` with `mark2` the log length after the first crash on a reference fixture (the crash run's own TRANSFERRING manifest write matches the same needle earlier, `state.rs:955`); assert both `manifest_of(ID).state == Transferring` and `manifest_of(id2)` exists; then `go_with(a b, restart_cfg(0x29))`; `ok(&r)`; both workspaces gone; copy content present. Mutant: in `open_operation`'s `--restart` arm (`session.rs:149`) keep only the first resumable prior (`scan.resumable.truncate(1)`): red.
- [ ] **Step 5b: T9** is the existing one-source restart suite, unchanged and green through every task: `restart_supersedes_a_prior_deleting_its_partials_then_its_workspace`, `restart_deletes_a_prior_operations_link_temporary`, `a_partial_restart_cannot_delete_keeps_its_prior_abandoned`, `a_version_2_prior_superseded_by_restart_stays_version_2`, `restart_with_nothing_to_supersede_is_a_plain_run`, `restart_stops_when_ownership_is_lost_and_deletes_nothing_after`, `restart_probes_again_and_a_refusal_there_leaves_the_priors_resumable`, `restart_supersedes_a_single_files_prior_and_its_partial`, `a_directory_the_restart_sweep_cannot_read_keeps_every_prior`, `a_version_2_single_file_prior_superseded_by_restart_keeps_its_fields`, `restart_heartbeats_during_its_sweep_before_the_copy`, `a_failed_heartbeat_during_restart_stops_the_run_at_the_lock_step_and_copies_nothing`, `restart_heartbeats_and_checks_ownership_before_removing_a_superseded_state`, `restart_stops_before_removing_a_superseded_state_once_ownership_is_lost`, `restart_still_supersedes_everything_and_ignores_resume_semantics`, `prior_cleanup_runs_after_a_restart_supersede`, `restart_never_reads_the_prior_index` (`crates/flux-core/src/run/tests.rs`); `a_killed_run_leaves_a_resumable_operation_that_restart_supersedes`, `a_killed_single_file_run_leaves_a_resumable_record_that_restart_supersedes`, `an_empty_lock_is_uncertain_until_restart_break_lock_takes_it_over` (`crates/flux-cli/tests/run.rs`); `restart_with_verify_runs` (`crates/flux-cli/tests/copy.rs`); `dry_run_combines_with_resume_and_restart`, `test_79_dry_resume_and_restart_report_and_change_nothing`, `a_single_file_restart_lists_its_discards` (`crates/flux-cli/tests/dry_run.rs`). No mutant: it is the existing suite.
- [ ] **Step 6: Run** `cargo nextest run -p flux-core -E 'test(/(^|::)break_lock_takes_over_an_empty_destination_lock_for_several_sources$/) | test(/(^|::)both_locks_unreadable_refuses_at_the_prefix_lock_and_writes_nothing$/) | test(/(^|::)a_live_prefix_lock_refuses_a_restart_and_leaves_the_prior_untouched$/) | test(/(^|::)a_several_source_restart_supersedes_a_failed_one_source_tree_run$/) | test(/(^|::)one_restart_supersedes_a_crashed_run_and_a_restart_that_crashed_before_abandoning_it$/)'` — expected: 5 passed. Mutants: T4: skip `locked.state.takeover = Some(..)` (`session.rs:272`): red; T4b: in `MultiPlace::create`'s `Err(refusal)` arm (`multi.rs:257-261`) fall back to `PrefixLocks::acquire(dir, self.capability, &[], &id).unwrap()` instead of returning, so the run goes on without prefix locks: red on `!fs.exists(operations/<id>)` (the workspace gets written) and on the refusal code; T5 and T6 as named in their steps; T8 as named. Revert each.
- [ ] **Step 7: Commit** `test: takeover, compound refusal, a live prefix lock, and mixed priors under a several-source restart (cut 17 task 5)`.

### Task 6: the CLI kill test

**Files:**
- Modify: `crates/flux-cli/tests/several_sources.rs` (add a `Stalled` copy and two tests)

**Interfaces:**
- Produces: `struct Stalled(Child)` copied from `crates/flux-cli/tests/run.rs:51-121` (with the imports it needs: `std::process::{Child, Stdio}`, `std::time::{Duration, Instant}`; `several_sources.rs:9-12` has neither) with one constructor `start_several(srcs: &[&Path], dst: &Path, at: u32, marks: &Path, args: &[&str]) -> Self` (the same env pins: `FLUX_TEST_STALL_AT`, `FLUX_TEST_STALL_FILE`, `FLUX_TEST_BATCH_AGE_MS=3600000`, `FLUX_TEST_OPEN_WRITERS=256`; `.args(args)` before the paths), `kill(self)`, and the `Drop`. The stall index for `[folder("alpha"), folder("beta")]` into `out` is **4** (measured 2026-10-27 on the debug binary, Linux: the run stalls with `out/alpha/a.flux-partial.<id>`, both `out/*.flux-lock` and `out.flux-lock` on disk).

- [ ] **Step 1: Write** `a_killed_several_source_run_is_superseded_by_restart`: `Stalled::start_several(&[&alpha, &beta], &out, 4, marks.path(), &[]).kill()`; assert a `out/alpha/a.flux-partial.*` exists and `out.flux-lock` exists; a plain rerun exits 3 with stderr containing `--restart to supersede` and not `cleanup --force`; then `flux copy --restart --json alpha beta out` exits 0; assert the four files' bytes (`A-alpha`, `BB-alpha`, `A-beta`, `BB-beta`), no `*.flux-partial.*` under `out`, `control_plane_names(&out).is_empty()` (helper at `several_sources.rs:54`), and `!out.flux-lock.exists()`.
- [ ] **Step 2: Write** `a_killed_run_restarted_with_other_sources_keeps_the_stale_prefix_lock`: kill as above; `let gamma = folder(d.path(), "gamma")`; `flux copy --restart alpha gamma out` (several sources, a different set) exits 0; `out/beta.flux-lock` still exists (L6; design 3.3), `out/alpha.flux-lock` and `out/gamma.flux-lock` do not, `out/gamma/a` holds `A-gamma`. (A ONE-source restart would leave every prefix lock, `run/place.rs:519-521`, so it cannot be this test's vehicle.)
- [ ] **Step 3: Run** `cargo nextest run -p flux-cli killed_several_source killed_run_restarted` — expected: 2 passed (debug build; the hook is live under `cargo nextest`). If the run "exited before its stall point", re-measure the index: run the debug binary by hand with `FLUX_TEST_STALL_AT=<n>` (n = 1 upward), `FLUX_TEST_STALL_FILE=<marks>/stalled-<n>`, `FLUX_TEST_BATCH_AGE_MS=3600000`, `FLUX_TEST_OPEN_WRITERS=256` against two folders, kill each stalled run, and take the first `n` whose DEST holds `alpha/a.flux-partial.*` with both prefix locks on disk; fix the constant with the measurement in its comment.
- [ ] **Step 4: Commit** `test: a killed several-source run is superseded by --restart end to end (cut 17 task 6)`.

### Task 7: documentation

**Files:**
- Modify: `TODO.md:1127` ("What remains is resume, restart and dry-run over several sources"), `TODO.md:1134` (L1 entry), `TODO.md:1138` (the cleanup lease entry no longer describes the only way out), `TODO.md` (two new entries under the several-sources heading), `ROADMAP.md:41` (the phase-2 row's "`--resume`, `--restart` and `--dry-run` over several sources are a later cut"), `docs/accepted-boundaries.md:106` (L1), `docs/superpowers/specs/2026-10-22-several-sources-design.md:30-31` (N1) and `:333-335` (L1), `docs/superpowers/specs/2026-10-27-several-restart-design.md` section 3.7 (README has no several-sources sentence to change: `README.md:9-11, 58-61` only; say so in 3.7)

- [ ] **Step 1: Edit.** The spec's `Status:` line (`2026-10-27-several-restart-design.md:3`) becomes "approved by the owner 2026-10-27; design panel GREEN at d2246b55c71c52bb0655293dd30aeb3606542ba2". L1 in `TODO.md` and `docs/accepted-boundaries.md` says `--resume` and `--dry-run` are the usage errors and `--restart` supersedes (dated). New `TODO.md` entries: (a) "A several-source restart leaves the crashed run's prefix lock files for names it does not reuse (L6); `flux cleanup` does not remove them" citing `MultiPlace::remove`; (c) "`--break-lock` with several sources covers the destination lock only (owner decision P1); an unreadable prefix lock is refused and removed by hand. P2, taking over and recording each prefix takeover, needs a decision on the single `takeover` slot (`state.rs:369`, `deny_unknown_fields`) or a format 6", citing design N2; (b) "A one-source copy that crashed into `DEST/<name>` leaves a lock a several-source restart recovers while its resumable workspace under `DEST/<name>/.flux` is never scanned" citing `prefix.rs:73-95`. N1 and L1 in the older design get a one-line dated note pointing at the new design. `ROADMAP.md:41` says "`--resume` and `--dry-run` over several sources are a later cut; `--restart` landed in cut 17". `TODO.md:1127` and `:1138` are reworded to match.
- [ ] **Step 2: Run** `just check` — expected: exit 0 (the typos gate reads every file above; `rg "cleanup --force" TODO.md ROADMAP.md README.md docs/accepted-boundaries.md` afterwards names only text about the cleanup command itself; the older designs and plans under `docs/superpowers/` and `docs/agy-capstone-ledger.md` keep their historical text).
- [ ] **Step 3: Commit** `docs: --restart with several sources (cut 17 task 7)`.

---

## Panel round 1 folds (solo Sonnet seat, 2026-10-27; agy CLEAN)
Folded: T4's manifest must be read during the run (a completed run retires its workspace); Task 6's residue test uses a several-source restart with a different set; the `nth_of` needles matched the crash phase (`.removing` and `nth_of_from`); exact-name filters and pass counts; realisable single-point mutants for T5 and T6; Task 7 targets (`ROADMAP.md:41`, `TODO.md:1127, 1138`, no README sentence, no tracker gate); the gate list; T9's suite listed; `Some(_)`; Task 6 imports; exact `remove_file` match; `.broken.<restart id>` end state.

## Panel round 2 folds (solo Sonnet seat, 2026-10-27; agy CLEAN)
Folded: filters are tail-anchored regexes (measured); `UNTRUSTED_DETAIL` is re-exported from `lock/mod.rs`; the three probe helpers move to Task 5; T8's `tree` crash gets its own `catch_unwind`; line corrections (`multi.rs:255`, `report.rs:1848-1859`, L1 `:333-335`, `Stalled` `:51-121`); the unverified index-13 aside dropped; `.as_deref()` comparisons; the spec's `Status:` line joins Task 7.

## Panel round 3 folds (solo Sonnet seat, 2026-10-27; agy CLEAN)
Folded: the pinned refusal-detail prefixes use `lock_prefix(name)` built with `Path::join` (Windows CI); Task 7 adds a TODO entry for N2 (`--break-lock` on prefix locks, P2); T4b's fixture names `create_dir`; the false "T7 goes red" remark; `justfile:21-22`.

## Panel round 4 folds (solo Sonnet seat; agy CLEAN): CLEAN
Folded minors: the `--resume` pointer is asserted (P3); the Task 7 `rg` check lists its real scope; the stall re-measure procedure is written out; T8's `id2`. Noted, not defects: the stall index is Linux-measured; T6's mutant also reddens the existing format-4 restart suite; `MultiPlace::remove` keys on `cfg.operation_id`, which equals `remove_own`'s id today because `--resume` is refused for several sources (`multi.rs:955-967`).

## Plan rulings

- P1. T8's fixture is a crashed restart (two resumable priors arise only from a restart that died before its ABANDONED write, or from manual planting), not the planting helper the design named; the design's T8 intent (both priors superseded by one restart) is unchanged.
- P2. T2's seeded partial is the one the crash leaves (`a/x.flux-partial.<id>` at the publish rename), not a hand-written file.
- P3. The `several_flag_gate` `--resume` text gains the `--restart` pointer in Task 3 (design 3.1).
