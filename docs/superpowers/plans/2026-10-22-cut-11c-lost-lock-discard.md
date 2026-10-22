# Cut 11c: no `state.db` write after a lost lock — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** After this run has lost the destination lock, no path in `tree.rs` writes `state.db` to discard a note or settle a failed batch; the fake answers the two `31a000d` codes.

**Architecture:** One rule (spec 4.1): on a failure path, call `cx.guard` immediately before a post-failure `state.db` write; skip the write and stop `TargetLockBusy` when it fails. Three sites in `crates/flux-core/src/tree.rs`: `discard_note_on_failure` (site 1, four callers), `flush_batch`'s STOP-HELD loop (site 2) and its settling transaction (site 3). Success paths gain no guard call. Two fake changes in `fault_fs.rs`. Recovery is untouched.

**Tech Stack:** Rust (edition with `if let` chains, as `run/mod.rs:431` uses), `cargo nextest`, the `FaultFs` fake and its call log.

**Spec:** `docs/superpowers/specs/2026-10-22-cut-11c-lost-lock-discard-design.md` (approved 2026-10-22). Sections cited below as "spec 4.2" etc.

## Global Constraints

- Branch: `spec/cut-11c-lost-lock-discard` (holds the spec commit `ef87f60`); implement on it.
- The repo gate, as-is: `just check` = `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `typos`, `cargo nextest run --workspace --no-tests=pass` (`justfile:12-22,46`). Do not add flags.
- No change to `run/recover.rs`, the note format, exit codes, or the single-file copy path (spec N4).
- No guard call on a path with no failure (spec N5): a green run's call log must show the same guard count as today. The CLI stall tests (`crates/flux-cli/tests/run.rs:151,190,236,505,604`) count guard calls on green paths only, so no stall index shifts.
- A `TargetLockBusy` from the re-check stops the run even when `still_owned`'s error was transient (spec 4.1; `session.rs:465-478` maps every error to `TargetLockBusy`). Intended; do not special-case.
- Test-fixture rule (spec 6, last bullet): any guard-call index a test pins is derived from the call log and asserted with `calls.get()`, never guessed. The derivations below are expectations; if a measured count differs, the test's comment and index are corrected to the MEASURED event, and the event itself (which call fails) is asserted by the log.
- Every commit message ends with the session's attribution trailer (the `Co-Authored-By` line the session states); the commands below omit it for brevity only.
- Dispatch rule for every implementer: STATE-VERIFICATION first (open the cited lines; on a mismatch report `STATE_MISMATCH` and stop); SHAPE-DIVERGENCE STOP (no change to any value's type or encoding beyond what a task states).

## Review Focus

1. The original failure is a heartbeat failure (step `Heartbeat`) and the re-check then fails: `finish_copy` would have returned the heartbeat as the stop; spec 4.2 says the heartbeat is REPORTED for the entry and the stop is the lost lock. Test added to Task 2 (`t2c`).
2. The original failure is a publish `AlreadyExists`: when reported via site 1 it must still read `DestinationNamespaceCollision` (spec 4.2, the remap). Test added to Task 2 (`t2b`).
3. A leftover listed twice (the run collects every reported leftover, `run/mod.rs:430-435`): the stop carries it, the per-entry report must not. Pinned by the `leftovers(&got, Some(&error))` equality in T2 and T3.
4. After site 2 returns `TargetLockBusy`, `drain_batches` must switch the other frames to `KeepAll` (`tree.rs:2954`), not `RemoveAll`. Pinned by T3's second frame.
5. The state site 3 leaves (renamed + note, failed + no temporary) must be what recovery finalizes. Pinned by T4-run's resume leg.

---

## File structure

| File | Change |
|---|---|
| `crates/flux-core/src/fault_fs.rs` | `undecodable_claim()` for `get`; special-file arm in `open_claim_store`; tests |
| `crates/flux-core/src/tree.rs` | `discard_note_on_failure` (new signature, site 1); `remap_collision` extracted from `finish_copy`; site 2 and site 3 guard checks in `flush_batch`; tests in the `tests` module beside the cut 9d batch tests (`tree.rs:5507+`) |
| `crates/flux-core/src/run/tests.rs` | T4-run (site 3 end to end, with the resume leg) |
| `docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md` | decision 6 amendment (spec 4.5) |
| `TODO.md` | close three items (spec 8), plus "Cut 9c debt" item 1 (closed by `t2d`) |

Test helpers already present in `tree.rs`'s test module (verified): `n_files(n)` (5535), `sources(&[..])` (5519), `policy(files)` (5514), `batched` (5544), `batched_with(fs, policy, durability, guard, beat)` (5553), `log` (5593), `count` (5597), `positions` (5602), `first` (5606), `created` (5613), `renamed` (5618), `temp_of` (5975), `leftovers(got, err)` (5980), `claimed` (5994), `published` (5999), `failing_after(owned, &calls)` (5315: passes the first `owned` calls, then `TargetLockBusy` for every later call — a lost lock does not come back), `link_run` and `strict()` (4648), `LINK_GUARD_AT_GATE/CREATE` (4745-4746). In `run/tests.rs`: `fake()` (63: `/src/a`, `/src/sub/b`, `/p` present, `/p/dest` absent), `cfg()` (26), `resume()` (2841), `strict()` (4415), `run_tree_with` (2121), `aborted` (880), `calls` (83), `count` (1545), `publish_of` (4420), `step_of` (3699), `LOCK` (20), `ID` (19), `strong`, `fs.claim`, `fs.prepared_count`.

Guard-call accounting used below (from the comments of pinned tests, `tree.rs:5333,5349,6160,6223`): an unbatched FILE makes guard calls sweep (1), create (2), publish (3); a batched file's staging makes 2 (sweep, create), a directory's creation 1, a flush's opening guard 1, each entry's publish 1; a `discard` (`copy.rs:214-224`) makes 1. Heartbeats: a staged file beats 3 times (sweep, create, one chunk), a directory's creation 1, a flush's opening beat 1, each publish 1 (`publish_staged`, `copy.rs:1118-1121`: beat, then guard, then rename).

---

### Task 1: The fake answers the two `31a000d` codes (spec 4.4, G3, T6)

**Files:**
- Modify: `crates/flux-core/src/fault_fs.rs:277-310` (`corrupt_claim`, `FakeClaimStore::get`), `:2154-2166` (`open_claim_store`, the symlink arm), tests at `:3871-3890`
- Test: same file, `mod tests`

**Interfaces:**
- Produces: `fn undecodable_claim() -> FsError` (`Code::StateCorrupt`, message `undecodable claim record`); `corrupt_claim()` unchanged (`IoError`) for `insert_if_absent` (295) and `upgrade_own_claim` (320).

- [ ] **Step 1: Write the failing tests**

Beside `open_claim_store_refuses_what_is_not_a_store` (3871):

```rust
#[test]
fn a_claim_that_does_not_decode_is_state_corrupt_on_get_and_io_error_on_write() {
    use flux_fs::{ClaimKey, ClaimRecord, ClaimStatus, ClaimStore, DestinationRoot, DirHandle, Durability, FluxPathKey, ObjectId};
    let fs = FaultFs::new();
    fs.create_dir(Path::new("/d")).unwrap();
    let d = fs.destination_root(Path::new("/d")).unwrap();
    let mut s = d.create_claim_store(OsStr::new("state.db"), Durability::Normal).unwrap();
    let k = ClaimKey::new(ObjectId { volume: 1, index: 1 }, OsStr::new("a"));
    // Version byte 9: no build of this format reads it (the idiom of `a_note_that_does_not_decode_...`, run/tests.rs).
    fs.put_raw_claim("/d/state.db", &k, &[9, 0, 0, 0, 0]);
    let got = s.get(&k).err().expect("undecodable");
    assert_eq!(got.code, Code::StateCorrupt, "as RedbClaimStore::read: {got:?}");
    assert!(got.source.to_string().contains("undecodable claim record"), "{got:?}");
    let r = ClaimRecord { target: FluxPathKey(b"a".to_vec()), status: ClaimStatus::Created };
    assert_eq!(s.insert_if_absent(&k, &r).err().expect("undecodable").code, Code::IoError, "N1: as the real store");
    assert_eq!(s.upgrade_own_claim(&k, &r.target).err().expect("undecodable").code, Code::IoError, "N1: as the real store");
}
```

And in `open_claim_store_refuses_what_is_not_a_store`, after the symlink assertion:

```rust
    fs.add_special("/d/sock.db");
    let err = open("sock.db").err().expect("special");
    assert_eq!(err.code, Code::DestinationError, "as open_lock and dir_unix.rs: {err:?}");
    assert!(err.source.to_string().contains("a claim store path holds something other than a regular file"), "{err:?}");
```

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core -- fault_fs::tests::a_claim_that_does_not_decode fault_fs::tests::open_claim_store_refuses`
Expected: both FAIL (`get` answers `IoError`; the special file answers `StateCorrupt` "zero-length claim store" or `NotFound`).

- [ ] **Step 3: Implement**

In `fault_fs.rs`: add `fn undecodable_claim() -> FsError` next to `corrupt_claim` (277), returning `FsError::new(Code::StateCorrupt, std::io::Error::other("undecodable claim record"))`; `get` (307) uses it; 295 and 320 keep `corrupt_claim`. In `open_claim_store`, directly after the symlink arm (2160-2165) and before the directory arm, add an arm for `g.types.get(&child_path) == Some(&FileType::Other)` returning `Code::DestinationError` with the message above (the shape of `open_lock`'s arm, 2259-2265).

- [ ] **Step 4: Run the tests and the file's suite**

Run: `cargo nextest run -p flux-core -- fault_fs::`
Expected: all PASS (the summary line's failed count is 0).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/fault_fs.rs
git commit -m "fix(core): the fake answers StateCorrupt for an undecodable claim on get and DestinationError for a special file at the store (cut 11c, 31a000d parity)"
```

---

### Task 2: Site 1 — the unbatched Strict discard re-checks the guard (spec 4.2, T1, T2, T5)

**Files:**
- Modify: `crates/flux-core/src/tree.rs:3008-3020` (`discard_note_on_failure`), its four callers `:1589`, `:1696`, `:2011`, `:2079`, and `finish_copy:3097-3104` (extract the remap)
- Test: `tree.rs` tests module, beside `files_one_is_the_unbatched_path` (5712) for files and beside `a_lost_lock_before_the_create_is_a_stop_and_creates_nothing` (4749) for the link

**Interfaces:**
- Produces:
  ```rust
  /// Cut 11c (spec 4.2). Returns the copy's result, replaced by a `TargetLockBusy` stop when the guard fails.
  fn discard_note_on_failure<C: ClaimStore + ?Sized>(
      claims: &std::cell::RefCell<C>,
      key: &ClaimKey,
      wrote: &std::cell::Cell<bool>,
      guard: &Guard<'_>,
      path: &Path,
      result: std::result::Result<flux_fs::Outcome, CopyError>,
      out: &mut TreeOutcome,
      on_report: &mut dyn FnMut(TreeFailure),
  ) -> std::result::Result<flux_fs::Outcome, CopyError>
  ```
  ```rust
  /// F3 and §241.5 (moved out of `finish_copy`): an `AlreadyExists` at the gate or the publish is a namespace collision.
  fn remap_collision(e: &mut CopyError)
  ```
- Consumes: `Guard` (`copy.rs:170`), `report` (`tree.rs:3238`), `CopyError { cause, leftover, step }` (`copy.rs:114`).

Behaviour of the new `discard_note_on_failure`, the complete case list:
1. `Ok(_)`, or `!wrote.get()`: return `result` unchanged. No guard call.
2. `Err(e)` with `e.code() == Code::TargetLockBusy`: return `Err(e)` unchanged. No discard, no guard call (spec 4.2 bullet 1).
3. `Err(e)`, other code, `guard()` is `Ok`: `let _ = claims.borrow_mut().discard_prepared(key);` return `Err(e)` (today's behaviour).
4. `Err(mut e)`, other code, `guard()` is `Err(lost)`: `let step = e.step; let leftover = e.leftover.take(); remap_collision(&mut e); report(out, on_report, path.to_path_buf(), TreeFailureCause::Copy(e)); return Err(CopyError { cause: lost, leftover, step })`. The leftover is still relative to the entry's directory; `finish_copy` (3080-3082) rebuilds it and returns the stop.

Each caller becomes `let result = discard_note_on_failure(claims, &key /*or key_stored*/, &wrote, cx.guard, &path, result, out, on_report);` in place of the statement there; the lines after it are unchanged. Step 0 confirms `path` and `out`/`on_report` are in scope at all four sites (they are passed to `finish_copy` on the next lines).

- [ ] **Step 1: Write the failing tests** (file tests use `n_files(2)`, `policy(1)`, `Durability::Strict`; a `let calls = std::cell::Cell::new(0);` per guard)

```rust
#[test]
fn t1_unbatched_publish_guard_failure_after_the_note_keeps_it_and_stops() {
    // Spec 4.2 bullet 1: a `TargetLockBusy` copy failure is returned as is: no discard and NO re-check.
    let fs = n_files(2);
    let calls = std::cell::Cell::new(0);
    // f0: sweep (1), create (2) pass; its publish guard (3) fails, after `write_note`.
    let guard = failing_after(2, &calls);
    let (r, out, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
    let error = r.unwrap_err();
    assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Publish), "{error:?}");
    assert_eq!(calls.get(), 3, "no re-check after a TargetLockBusy failure");
    assert!(got.is_empty(), "a stop, not a per-file failure: {got:?}");
    let c = log(&fs);
    assert_eq!(count(&c, "claim_prepare("), 1, "{c:?}");
    assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
    assert_eq!(fs.prepared_count(), 1, "the note stays for recovery");
    assert!(fs.exists("/dst/f0.flux-partial.op1") && !fs.exists("/dst/f0"));
    assert!(!fs.called("create_new(/dst/f1"), "no later file is attempted: {c:?}");
    assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")], "reported once: {got:?}");
    assert_eq!(out.files_copied, 0);
}

#[test]
fn t2_unbatched_rename_failure_then_a_lost_lock_keeps_the_note_reports_the_failure_and_stops() {
    // Spec 4.2 bullets 2-4: the re-check. Mutant: drop the re-check (today's unconditional discard).
    let fs = n_files(2);
    let calls = std::cell::Cell::new(0);
    fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Other);
    // f0: sweep (1), create (2), publish (3) pass; the rename fails; the copy path's discard guard (4) fails (the
    // temporary is kept); the re-check (5) fails.
    let guard = failing_after(3, &calls);
    let (r, out, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
    let error = r.unwrap_err();
    assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Publish), "{error:?}");
    assert_eq!(error.leftover.as_ref().map(|(p, _)| p.clone()), Some(temp_of("f0")), "relative to DEST");
    assert_eq!(calls.get(), 5, "the re-check ran once");
    let c = log(&fs);
    assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
    assert_eq!(fs.prepared_count(), 1, "the note stays for recovery");
    assert_eq!(got.len(), 1, "{got:?}");
    assert_eq!(got[0].path, PathBuf::from("f0"));
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish), "the original failure: {e:?}");
    assert!(e.leftover.is_none(), "the leftover travels in the stop only: {e:?}");
    assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")], "once: {got:?}");
    assert!(fs.exists("/dst/f0.flux-partial.op1"));
    assert!(!fs.called("create_new(/dst/f1"), "no later file is attempted: {c:?}");
    assert_eq!(out.files_copied, 0);
}

#[test]
fn t2b_the_collision_remap_survives_the_lost_lock_report() {
    // Review focus 2. Between f0's create and its rename someone writes /dst/f0; the no-replace rename refuses it.
    let fs = n_files(2);
    let calls = std::cell::Cell::new(0);
    fs.on_nth("rename_no_replace", 1, |fs| fs.write_file("/dst/f0", b"theirs"));
    let guard = failing_after(3, &calls);
    let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &no_heartbeat);
    assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!(e.code(), Code::DestinationNamespaceCollision, "{e:?}");
    assert_eq!(fs.read_file("/dst/f0").as_deref(), Some(&b"theirs"[..]));
    // Without these the test passes against today's code too (f1's sweep guard stops the run anyway).
    assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
    assert_eq!(fs.prepared_count(), 1, "the note stays");
}

#[test]
fn t2c_a_heartbeat_failure_after_the_note_under_a_lost_lock_is_reported_and_the_stop_is_the_lost_lock() {
    // Review focus 1 (spec 4.2, "whatever the original's code"). f0 beats sweep (1), create (2), chunk (3); its
    // publish beat (4) fails before the publish guard; the discard guard (3) fails; the re-check (4) fails.
    let fs = n_files(2);
    let calls = std::cell::Cell::new(0);
    let beats = std::cell::Cell::new(0);
    let beat = || { beats.set(beats.get() + 1); if beats.get() > 3 { Err(FsError::new(Code::IoError, std::io::Error::other("beat"))) } else { Ok(()) } };
    let guard = failing_after(2, &calls);
    let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &guard, &beat);
    let error = r.unwrap_err();
    assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Heartbeat), "the original's step: {error:?}");
    assert_eq!((beats.get(), calls.get()), (4, 4));
    assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
    assert_eq!(fs.prepared_count(), 1);
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat), "{e:?}");
    assert_eq!(leftovers(&got, Some(&error)), [temp_of("f0")]);
}

#[test]
fn t2d_a_heartbeat_failure_after_the_note_with_the_lock_held_discards_it() {
    // Closes "Cut 9c debt" item 1. Held: the discard is legal and `finish_copy` stops on the heartbeat as today.
    let fs = n_files(2);
    let beats = std::cell::Cell::new(0);
    let beat = || { beats.set(beats.get() + 1); if beats.get() > 3 { Err(FsError::new(Code::IoError, std::io::Error::other("beat"))) } else { Ok(()) } };
    let (r, _, got) = batched_with(&fs, policy(1), Durability::Strict, &unguarded, &beat);
    assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
    assert!(got.is_empty(), "a stop, not a per-file failure: {got:?}");
    assert_eq!(count(&log(&fs), "claim_discard_prepared("), 1);
    assert_eq!(fs.prepared_count(), 0);
    assert!(!fs.exists("/dst/f0.flux-partial.op1"));
}

#[test]
fn t5_unbatched_rename_failure_with_the_lock_held_still_discards_the_note() {
    // Spec T5: the re-check does not suppress a legal write. Mutant: skip the discard whenever the guard is consulted.
    let fs = n_files(2);
    fs.fail_nth("rename_no_replace", 1, Code::IoError, ErrorKind::Other);
    let (r, out, got) = batched(&fs, policy(1), Durability::Strict);
    r.unwrap();
    let c = log(&fs);
    assert_eq!(count(&c, "claim_discard_prepared("), 1, "{c:?}");
    assert_eq!(fs.prepared_count(), 0);
    assert_eq!(got.len(), 1, "{got:?}");
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Publish));
    assert!(e.leftover.is_none() && !fs.exists("/dst/f0.flux-partial.op1"));
    assert!(published(&fs, "f1") && claimed(&fs, "f1"));
    assert_eq!(out.files_copied, 1);
}

#[test]
fn t1_link_publish_guard_failure_after_the_note_keeps_it() {
    // Sites 2011/2079 (links) share the helper; one pin for a Strict link: gate (1), create (2) pass, publish (3) fails.
    let fs = links(&[("n", "p")]);
    let calls = std::cell::Cell::new(0);
    let guard = failing_after(LINK_GUARD_AT_CREATE, &calls);
    let o = CopyOptions { durability: Durability::Strict, ..opts() }; // a link writes a note only under Durability::Strict (tree.rs:1991); `strict()` is Safety::Strict only
    let (r, _, _) = link_run(&fs, &o, false, policy(1), &guard, no_seed);
    assert_eq!(r.unwrap_err().code(), Code::TargetLockBusy);
    assert_eq!(calls.get(), 3);
    assert_eq!(count(&log(&fs), "claim_discard_prepared("), 0);
    assert_eq!(fs.prepared_count(), 1, "the link's note stays");
}
```

(`links`, `no_seed` are the helpers the test at 4719 uses; Step 0 confirms their names.) Expectation for `t1_link`: under Strict the link path writes a note before its publish (cut 10a); if `prepared_count` is 0 BEFORE the fix with the lock held, the fixture is wrong — report it, do not adapt.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core --no-fail-fast -- tree::tests::t1_ tree::tests::t2 tree::tests::t5_`
Expected: `t1_unbatched`, `t2_`, `t2b`, `t2c`, `t1_link` FAIL (a `claim_discard_prepared(` call is logged, `prepared_count` is 0); `t2d` and `t5_` PASS already (held behaviour is unchanged) — they are the T5/T7 anchors.

- [ ] **Step 3: Implement** `remap_collision` (extract 3100-3104, called from `finish_copy` at that spot) and the new `discard_note_on_failure` with the four callers, per Interfaces. Update the function's doc comment to the cut 11c rule (spec 4.2).

- [ ] **Step 4: Run the tree and run suites**

Run: `cargo nextest run -p flux-core --no-fail-fast -- tree:: run::`
Expected: all PASS. A failure elsewhere means an existing test pinned the old behaviour; spec 6's last bullet: list it, with the reason, in the commit message, and change its expectation only if the spec's table (section 5) says the new behaviour differs — otherwise it is a defect in the implementation.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/tree.rs
git commit -m "fix(core): the unbatched Strict discard re-checks the lock and keeps the note after a loss (cut 11c site 1)"
```

---

### Task 3: Site 2 — the STOP-HELD loop re-checks the guard before `apply_recovery` (spec 4.3, T3)

**Files:**
- Modify: `crates/flux-core/src/tree.rs:2733-2748` (`flush_batch`, the STOP-HELD arm)
- Test: `tree.rs` tests module, beside `a_heartbeat_failure_with_the_lock_held_commits_the_renamed_and_discards_the_rest` (6220)

**Interfaces:**
- Consumes: `report` (3238), `CopyError::at` (`copy.rs:145`), `cx.guard`.

New code, between the `for q in rest { .. }` loop (2738-2746) and `let _ = claims.borrow_mut().apply_recovery(&ops);` (2747):

```rust
// Cut 11c (spec 4.3 site 2): the lock may have gone during the discards above. Then nothing more is written: the
// renamed keep their notes (recovery row 1), the stop that began this arm is reported for its entry (with the
// leftover it carries, already relative to DEST), and the lost lock is the stop, with no leftover of its own.
if let Err(lost) = (cx.guard)() {
    report(out, on_report, path, TreeFailureCause::Copy(stop));
    return Err(CopyError::at(CopyStep::Publish, lost));
}
```

`path` is the entry's path (owned; `finish_copy(path.clone(), ..)` at 2720 cloned it). Step 0 confirms `path` is still owned at 2747.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn t3_a_lost_lock_inside_the_stop_held_loop_writes_nothing_and_keeps_every_later_frame() {
    // Spec 4.3 site 2. Mutant: drop the re-check (apply_recovery runs; drain_batches removes the root's batch).
    // Two frames: `a` pending in the root; m's DirEnd flush holds x, y, z.
    // Each file holds its own relative name, so `published` (which compares bytes to the name) applies.
    let fs = sources(&[("a", b"a"), ("m/x", b"m/x"), ("m/y", b"m/y"), ("m/z", b"m/z")]);
    let calls = std::cell::Cell::new(0);
    let beats = std::cell::Cell::new(0);
    // Beats: a staged (1-3), m created (4), x y z staged (5-13), m's flush opening beat (14), x's publish (15):
    // y's publish beat (16) fails with the lock held.
    let beat = || { beats.set(beats.get() + 1); if beats.get() > 15 { Err(FsError::new(Code::IoError, std::io::Error::other("beat"))) } else { Ok(()) } };
    // Guards: a staged (1-2), m created (3), x y z staged (4-9), m's flush opening guard (10), x's publish (11),
    // y's discard (12, held: removed); z's discard (13) fails (kept); the re-check (14) fails.
    let guard = failing_after(12, &calls);
    let (r, out, got) = batched_with(&fs, policy(64), Durability::Strict, &guard, &beat);
    let error = r.unwrap_err();
    assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Publish), "{error:?}");
    assert!(error.leftover.is_none(), "{error:?}");
    assert_eq!((beats.get(), calls.get()), (16, 14), "the events the indices name");
    let c = log(&fs);
    assert_eq!(count(&c, "claim_apply_recovery"), 0, "{c:?}");
    assert_eq!(count(&c, "claim_discard_prepared("), 0, "{c:?}");
    assert_eq!(fs.prepared_count(), 3, "x, y, z keep their notes");
    assert!(published(&fs, "m/x"), "renamed");
    assert!(fs.claim(strong_id(&fs, "/dst/m"), "x").is_none(), "not committed; finalized at --resume");
    assert!(!fs.exists("/dst/m/y.flux-partial.op1"), "removed while the lock was held");
    assert!(fs.exists("/dst/m/z.flux-partial.op1") && fs.exists("/dst/a.flux-partial.op1"), "kept");
    // y: the original stop, reported (no leftover); z: kept in the loop; a: the root's batch, KeepAll in the drain.
    let y = got.iter().find(|f| f.path == PathBuf::from("m/y")).expect("y reported");
    let TreeFailureCause::Copy(e) = &y.cause else { panic!("{got:?}") };
    assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat), "{e:?}");
    assert!(e.leftover.is_none());
    assert_eq!(leftovers(&got, Some(&error)), [temp_of("m/z"), temp_of("a")], "{got:?}");
    assert_eq!(out.files_copied, 1);
}
```

`published` (5999) reads `/dst/{f}` and compares the bytes to `f`; `claimed` (5994) keys by `/dst`'s id, so `m/x`'s claim is read through `fs.claim` with `/dst/m`'s id, as written.

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo nextest run -p flux-core -- tree::tests::t3_`
Expected: FAIL at `count(&c, "claim_apply_recovery") == 0` (today it is 1) — or earlier at `calls.get()` if a derived index is off; then fix the index to the MEASURED event (the failing call must be z's discard) and re-run until the failure is the `apply_recovery` one.

- [ ] **Step 3: Implement** the arm per Interfaces.

- [ ] **Step 4: Run the suites**

Run: `cargo nextest run -p flux-core --no-fail-fast -- tree:: run::`
Expected: all PASS, including 6220 (the held counterpart: `unguarded`, unchanged).

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/tree.rs
git commit -m "fix(core): the batched STOP-HELD cleanup re-checks the lock before discarding notes (cut 11c site 2)"
```

---

### Task 4: Site 3 — the settling transaction re-checks the guard when an entry failed (spec 4.3, T4, T5)

**Files:**
- Modify: `crates/flux-core/src/tree.rs:2752-2754` (`flush_batch`, the settling transaction)
- Test: `tree.rs` tests module beside `a_failed_rename_discards_its_note_inside_the_one_apply_recovery` (6115); `crates/flux-core/src/run/tests.rs` beside `a_lost_lock_after_the_notes_keeps_them_and_the_temporaries` (4584)

**Interfaces:** `RecoveryOp::Discard { key }` marks a failed entry (2721, 2737); `RecoveryOp::Commit` a renamed one.

New code, replacing `if !ops.is_empty() {` at 2753:

```rust
if !ops.is_empty() {
    // Cut 11c (spec 4.3 site 3): a batch with a failed entry re-checks the lock before it is settled; a batch with no
    // failure does not (N5: the same inherent window as an unbatched commit after its rename).
    let any_failed = ops.iter().any(|op| matches!(op, RecoveryOp::Discard { .. }));
    if any_failed && let Err(lost) = (cx.guard)() {
        return Err(CopyError::at(CopyStep::Publish, lost));
    }
    let settled = claims.borrow_mut().apply_recovery(&ops);
    ...unchanged...
```

- [ ] **Step 1: Write the failing tests**

In `tree.rs`:

```rust
#[test]
fn t4_a_lost_lock_before_the_settling_transaction_writes_nothing_when_an_entry_failed() {
    // Spec 4.3 site 3. Mutant: drop the re-check (apply_recovery commits f0 and f2 and discards f1 after the loss).
    let fs = n_files(3);
    let calls = std::cell::Cell::new(0);
    fs.fail_nth("rename_no_replace", 2, Code::IoError, ErrorKind::Other);
    // Guards: staging (1-6), the flush's opening guard (7), f0's publish (8), f1's publish (9), f1's rename fails,
    // f1's discard (10, held: removed), f2's publish (11); the settling re-check (12) fails.
    let guard = failing_after(11, &calls);
    let (r, out, got) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
    let error = r.unwrap_err();
    assert_eq!((error.code(), error.step), (Code::TargetLockBusy, CopyStep::Publish), "{error:?}");
    assert!(error.leftover.is_none(), "{error:?}");
    assert_eq!(calls.get(), 12);
    let c = log(&fs);
    for none in ["claim_apply_recovery", "claim_discard_prepared(", "claim_commit_prepared("] {
        assert_eq!(count(&c, none), 0, "{none}: {c:?}");
    }
    assert_eq!(fs.prepared_count(), 3, "every note stays");
    for f in ["f0", "f2"] {
        assert!(published(&fs, f) && !claimed(&fs, f), "{f}: renamed, finalized at --resume");
    }
    assert!(!fs.exists("/dst/f1.flux-partial.op1") && !fs.exists("/dst/f1"));
    assert_eq!(got.len(), 1, "{got:?}");
    let TreeFailureCause::Copy(e) = &got[0].cause else { panic!("{got:?}") };
    assert_eq!((got[0].path.clone(), e.code(), e.step), (PathBuf::from("f1"), Code::IoError, CopyStep::Publish));
    assert!(leftovers(&got, Some(&error)).is_empty(), "{got:?}");
    assert_eq!(out.files_copied, 2);
}

#[test]
fn t4_no_failed_entry_means_no_settling_re_check() {
    // N5. Mutant: re-check on every batch. A green 3-file batch: staging (1-6), opening (7), three publishes (8-10).
    let fs = n_files(3);
    let calls = std::cell::Cell::new(0);
    let guard = failing_after(u32::MAX, &calls);
    let (r, out, _) = batched_with(&fs, policy(4), Durability::Strict, &guard, &no_heartbeat);
    r.unwrap();
    assert_eq!((calls.get(), out.files_copied), (10, 3));
}
```

In `run/tests.rs` (the resume leg; spec T4 "a `--resume` over the resulting state (fake) finalizes the renamed entries"):

```rust
#[test]
fn a_lost_lock_before_the_batch_settles_leaves_the_renamed_for_resume_to_finalize() {
    // Cut 11c site 3, end to end. `/src/c` joins `a` in the root's batch (sub/b flushes first at sub's DirEnd).
    // `rename_no_replace`: the probe (1), the workspace publish (2), sub/b (3), a (4: fails, I/O), c (5: just before
    // it the lock is taken over, after c's publish guard passed). The settling re-check then finds the lock gone.
    let fs = fake();
    fs.write_file("/src/c", b"C");
    fs.fail_nth("rename_no_replace", 4, Code::IoError, std::io::ErrorKind::Other);
    fs.on_nth("rename_no_replace", 5, |fs| fs.write_file(LOCK, b"another run's bytes"));
    let (r, got) = run_tree_with(&fs, &cfg(), &strict());
    assert!(r.stop.is_none(), "the copy's abort is the report: {:?}", r.stop);
    let a = aborted(&r);
    assert_eq!(a.error.code(), Code::TargetLockBusy, "{:?}", a.error);
    assert!(a.error.leftover.is_none(), "{:?}", a.error);
    assert_eq!(fs.read_file("/p/dest/c").as_deref(), Some(&b"C"[..]), "c was renamed");
    assert_eq!(fs.claim(strong(&fs, "/p/dest"), "c"), None, "not committed after the loss");
    assert_eq!(fs.prepared_count(), 2, "a's and c's notes stay");
    let c = calls(&fs);
    let root_rename = publish_of(&c, "c");
    assert_eq!(count(&c, "claim_apply_recovery"), 1, "sub's only: {c:?}");
    assert!(at(&c, "claim_apply_recovery") < root_rename, "{c:?}");
    for none in ["claim_discard_prepared(", "claim_commit_prepared("] {
        assert_eq!(count(&c, none), 0, "{none}: {c:?}");
    }
    assert_eq!(got.iter().filter(|f| f.path == PathBuf::from("a")).count(), 1, "{got:?}");
    assert_eq!(step_of(got.iter().find(|f| f.path == PathBuf::from("a")).unwrap()), CopyStep::Publish);
    // The resume (engine-level `break_lock`, as the CLI would with --restart --break-lock, here over the foreign
    // lock the takeover left): recovery row 1 commits c (RENAMED); a's note (temporary removed, name absent) is
    // GONE and discarded; a is copied again.
    let (r2, got2) = run_tree_with(&fs, &RunConfig { break_lock: true, ..resume() }, &strict());
    let out2 = ok(&r2);
    assert!(matches!(r2.resumed, Some(ResumeNote::Adopted { .. })), "{:?}", r2.resumed);
    assert!(got2.is_empty(), "{got2:?}");
    assert_eq!(fs.prepared_count(), 0);
    assert!(fs.claim(strong(&fs, "/p/dest"), "c").is_some(), "finalized by recovery");
    assert_eq!(fs.read_file("/p/dest/a").as_deref(), Some(&b"A"[..]));
    assert!(out2.files_copied >= 1, "{out2:?}");
}
```

(`at` (`run/tests.rs:88`) is that file's first-index helper; `run/tests.rs` has no `first`.) If the engine refuses `break_lock` with `resume` (a `Refused` stop on the second leg), STOP and report `STATE_MISMATCH: resume+break_lock refused: <detail>`; do not change the first leg's assertions to compensate.

- [ ] **Step 2: Run them to verify they fail**

Run: `cargo nextest run -p flux-core --no-fail-fast -- tree::tests::t4_ run::tests::a_lost_lock_before_the_batch_settles`
Expected: `t4_a_lost_lock_before_the_settling_transaction` and the run test FAIL (an `apply_recovery` is logged after the loss); `t4_no_failed_entry` PASSES already (the N5 anchor).

- [ ] **Step 3: Implement** the re-check per Interfaces.

- [ ] **Step 4: Run the whole workspace gate**

Run: `just check`
Expected: fmt, clippy (`-D warnings`), typos clean; nextest summary with 0 failed. 6115 and `run/tests.rs:4558` (held counterparts, `unguarded` / a held real lock) pass unchanged: each now makes one extra guard call on its failure path, and neither counts guard calls.

- [ ] **Step 5: Commit**

```bash
git add crates/flux-core/src/tree.rs crates/flux-core/src/run/tests.rs
git commit -m "fix(core): a batch with a failed entry re-checks the lock before it is settled (cut 11c site 3)"
```

---

### Task 5: Docs — the cut 9c amendment and TODO closures (spec 4.5, 8)

**Files:**
- Modify: `docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md:81-83` (decision 6)
- Modify: `TODO.md:524` ("Cut 9c debt" item 1), `:526-530` (item 3), `:616` (STOP-HELD), `:618` (fake drift)

- [ ] **Step 1: Amend decision 6**

In line 82, replace the parenthetical `(the heartbeat, the guard, the rename itself failing, a lost lock)` with `(the heartbeat, the rename itself failing, or another failure, while this run still holds the lock)`, and append after the sentence ending `row 4 below).` this sentence: `After a lost lock nothing is discarded: the note stays and recovery decides it by the matrix (cut 9d decision 8; cut 11c).` Lines 76-77 stay.

- [ ] **Step 2: Close the TODO items**

Each closed item becomes `- [x] **<its existing bold or first clause>** — closed by cut 11c (spec `docs/superpowers/specs/2026-10-22-cut-11c-lost-lock-discard-design.md`, <section>)`, keeping the item's original text after the dash where it is useful: 9c debt item 1 (test `t2d_...`, section 6), 9c debt item 3 (site 1, 4.2), main-review STOP-HELD item (site 2, 4.3), main-review fake item (4.4; note that N1's `insert_if_absent`/`upgrade_own_claim` `IoError` is kept). Items 2 and 4 of the main review stay open (spec N3). The format of a closed item is the one at `TODO.md:640` (`- [x] ... — closed by cut 11b ...`).

- [ ] **Step 3: Verify**

Run: `typos && git diff --stat`
Expected: typos clean; exactly the two files changed.

- [ ] **Step 4: Commit**

```bash
git add TODO.md docs/superpowers/specs/2026-10-09-cut-9c-commit-recovery-design.md
git commit -m "docs: cut 9c decision 6 no longer discards after a lost lock; close the cut 11c TODO items"
```

---

### Task 6: Mutation check (spec T7; run by the DRIVER, not delegated)

For each mutant, apply it in the working tree, run the named test, confirm RED, then `git checkout -- crates/flux-core/src/tree.rs`:

| Mutant | Must turn red |
|---|---|
| Site 1: remove the `TargetLockBusy` early return (case 2 falls into case 3/4) | `t1_unbatched_publish_guard_failure_after_the_note_keeps_it_and_stops` (`calls.get()` becomes 4 and, with the guard still failing, the note is kept anyway — so the `calls.get() == 3` assertion is the kill; keep it) |
| Site 1: remove the re-check (always discard) | `t2_unbatched_rename_failure_then_a_lost_lock_...`, `t2c`, `t1_link` |
| Site 2: remove the re-check | `t3_a_lost_lock_inside_the_stop_held_loop_...` |
| Site 3: remove the re-check | `t4_a_lost_lock_before_the_settling_transaction_...`, `a_lost_lock_before_the_batch_settles_...` |
| Site 3: re-check on every batch | `t4_no_failed_entry_means_no_settling_re_check` |

Run: `cargo nextest run -p flux-core --no-fail-fast -- <test name>` per mutant. Record the five RED results in the execution-status memory, then proceed to AGY-CAPSTONE over the committed range.

---

## Self-review (done at authoring)

- Spec coverage: G1 → Tasks 2-4; G2 → the `TargetLockBusy` stop and `KeepAll` assertions (T2, T3) and T4-run's resume leg; G3 → Task 1; G4 → Task 5. Section 5's table rows 1-4 → T1, T2, T3, T4; row 5 → T5, 6115, 6220, `run/tests.rs:4558`, `t4_no_failed_entry`. Section 7's latitude is decided: the guard reaches site 1 as a parameter; the stop is built inline; `corrupt_claim` is not split, `undecodable_claim` is added. Section 8 → Task 5. T7 → Task 6.
- Types: `discard_note_on_failure` returns the result it takes; `remap_collision(&mut CopyError)`; `CopyError::at(CopyStep::Publish, lost)` builds the leftover-less stops (sites 2 and 3); `kept` (2778) is NOT used because the stops of sites 2 and 3 carry no leftover.
- Gaps found and closed: the `claimed` helper's parent (Task 3 Step 1 note); `first` vs `at` in `run/tests.rs` (Task 4 note); `break_lock` + `resume` is an engine-only combination (`session.rs:127` is the sole engine consumer; the CLI pairs it with `--restart`, `main.rs:709-714`) — accepted as a test convenience with a STATE_MISMATCH stop if refused.
- Known unverified (to be measured by the tests' `calls.get()`/`beats.get()` assertions): the two-frame derivation in T3 (a directory's creation beat/guard counts come from `tree.rs:6223,6310` comments, not from a run of this fixture).

## Stand-downs

None. Panel record: solo round 1 (4 folded: T1 `got.is_empty()`, T3 fixture bytes and claim id, `at` for `first`, attribution trailer); agy round 1 GREEN (no findings; it recomputed the T3 indices, 16 beats and 14 guards over two frames, and its sampled citations grep-verified).
