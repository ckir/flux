# Cut 7b Part 2: the heartbeat - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** While a run holds its destination's lock, it refreshes the lock record's `last_heartbeat_wall_time` every
5 s (§101): inside the copy's 64 KiB loop, before each of the copy's guarded mutations, and before each ownership check
of the `--restart` sweep. A heartbeat write that fails twice stops the run with one error naming the lock path, never
`TARGET_LOCK_BUSY`.

**Architecture:**
- **The lock (`lock/held.rs`):** `Held::heartbeat(&self, now)` rewrites the record with only its time changed, through
  the held handle, with no sync (H-3) and no `still_owned` first (H-4). The newest time lives in a `Cell` in `Held`, and
  `rewrite_record` (Q-K) carries it.
- **The copy (`copy.rs`, `tree.rs`):** a second callback, `Heartbeat`, beside the guard. The copy calls it before each
  guard call that precedes a mutation and after every 64 KiB written. Its failure is a new `CopyStep::Heartbeat`, which
  a tree treats as an abort. The unlocked copies pass `no_heartbeat`.
- **The run (`run/`):** `RunConfig::heartbeat_interval`; a `Pulse` in `Locked` holds the interval, the instant of the
  last write and a `failed` flag in `Cell`s. `Pulse::beat` makes the interval decision and the one retry. After a
  failure, `fail` writes nothing when the record no longer proves ownership, and closes without unlinking.
- **The CLI:** passes `HEARTBEAT_INTERVAL` (5 s). A debug build reads `FLUX_TEST_HEARTBEAT_INTERVAL_MS`.

**Tech Stack:** Rust 2024, the `FaultFs` fake, nextest via `just`.

**Spec:** `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, "The heartbeat" and decisions
6-9 (H-1..H-4). Built on Part 1 (`spec/cut-7b` at `810d996`; capstone GREEN `c0fb55d`, test audit `fc6258b`).

---

## Decisions this plan makes (each traced to the spec or to measured code)

1. **The copy makes the heartbeat call, right before its guard call - not the run's guard closure.** The spec puts
   the call in "the run's guard closure, before each guarded mutation". Measured, that cannot carry the failure: the
   copy maps every guard error to the step it guards (`copy.rs:409`, `:432` map to `CopyStep::Create`), and a tree
   aborts only on `TARGET_LOCK_BUSY` (`tree.rs:558-560`). Any other code at `Create` is ONE file's failure, reported
   and walked past (`tree.rs:575`). A heartbeat failure inside the guard would therefore be either `TARGET_LOCK_BUSY`
   (which decision 7 forbids) or a per-file failure the walk continues past. So the copy takes the heartbeat as the
   spec's "second callback beside the guard", and calls it immediately before the guard at each guarded mutation:
   - the step-1 sweep (`copy.rs:409`);
   - the create (`copy.rs:432`);
   - the publish (`copy.rs:529`);
   - a tree's directory creation (`tree.rs:389`).

   Its failure is `CopyStep::Heartbeat`, which `copy_one` returns as an abort. The placement is the same as the spec's
   (before each guarded mutation), so H-1's "at every §99 guard call" holds. `discard`'s guard (`copy.rs:151`) gets no
   heartbeat: it runs only after a failure, and one more advisory write there buys nothing. Task 6 records this in the
   spec.
2. **One loop heartbeat per 64 KiB written:** after each `write_all` in the stream loop (`copy.rs:446-449`). An empty
   file streams no chunk, but it still heartbeats at its sweep, create and publish.
3. **`Pulse::beat` returns the plain I/O error; each caller names the lock.**
   - **During the copy:** the run's beat closure wraps the error: the message names the lock path, the `ErrorKind` is
     kept, and a `TARGET_LOCK_BUSY` code becomes `IO_ERROR` (decision 7: never `TARGET_LOCK_BUSY`). The CLI prints a
     copy error as `CODE: <file>: <source>` (`flux-cli/src/report.rs:183-189`), so the lock path must be in the source.
   - **During the `--restart` sweep:** the error becomes `RunError::Failed { step: RunStep::Lock, path: <lock>, .. }`,
     as the spec says. The path is already the field, so the message does not repeat it.
4. **After a failed heartbeat, the guard's "lost" reason names the heartbeat.** The only guard call after a heartbeat
   failure is `discard`'s (decision 1). When the record is torn, that guard fails, and the leftover's reason would
   otherwise read "another run took it over" (`session.rs:233-234`). Spec: "the report names the heartbeat failure,
   not a lost lock." `guarded` takes the `Pulse` and picks its message from `failed()`.
5. **The torn-record test tears the record with the fake's `on_nth` hook.** It overwrites the lock's bytes just before
   the failing write (`fault_fs.rs:654-663`: the hook runs, then the nth fault fires, so the write itself never
   happens). `write_file` keeps a path's identity (`fault_fs.rs:161-164`), so `still_owned` fails on the record, not
   on the identity: exactly a torn record. The second failure (the retry) is a one-shot `fail` armed inside the same
   hook. The fake allows one hook and one nth fault per call name (`fault_fs.rs:654-672`).
6. **The existing tests do not heartbeat.** The run tests' `cfg()` (`run/tests.rs:25-34`) gets an interval of one
   hour, so every existing call-count assertion stays as it is. New tests that want heartbeats pass `Duration::ZERO`,
   because `elapsed() < ZERO` is never true.
7. **`HEARTBEAT_INTERVAL` lives in `flux-core::run`** (§101's 5 s), and the CLI passes it. Task 5 adds the debug
   override.
8. **The model needs no change.** The spec's "Model" section is confirmed: `models/lockproto/trace.toml:460` and
   `:502` mark heartbeat time `not_modelled`. A heartbeat keeps `operation_id` and `owner_instance_id` (asserted in
   `Held::heartbeat` below), so the ownership predicate is unchanged, and a torn record is the model's existing torn
   state.

## File structure

| File | Change |
|---|---|
| `crates/flux-core/src/lock/held.rs` | `beat: Cell<Option<u64>>`, `heartbeat(&self, now)`, `rewrite_record` carries the newest time |
| `crates/flux-core/src/lock/acquire.rs`, `lock/takeover.rs` | the two `Held { .. }` literals gain `beat` |
| `crates/flux-core/src/copy.rs` | `Heartbeat` type, `no_heartbeat`, `CopyStep::Heartbeat`, `copy_file_guarded`'s `beat` parameter and its calls |
| `crates/flux-core/src/tree.rs` | `Shared::beat`; the directory-creation heartbeat; `copy_one` aborts on `CopyStep::Heartbeat` |
| `crates/flux-core/src/run/mod.rs` | `RunConfig::heartbeat_interval`, `HEARTBEAT_INTERVAL`; the beat closures; `Ended` after a failure; `fail` after a failure |
| `crates/flux-core/src/run/session.rs` | `Pulse`; `Locked::pulse`; `Fault::Heartbeat`; `checked`; `guarded` takes the pulse; `beat_error` |
| `crates/flux-core/src/run/restart.rs`, `run/place.rs` | the sweep's checks become `checked`; `Place::sweep` takes `&Locked` |
| `crates/flux-core/src/run/tests.rs` | `cfg()` interval; the heartbeat tests |
| `crates/flux-cli/src/main.rs` | `heartbeat_interval()`; the debug override |
| `crates/flux-cli/tests/run.rs` | `Stalled::start_env`; the end-to-end heartbeat tests |
| `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md` | decision 1's refinement |

## Commands (every task)

- Build and test, from `E:\Rust\flux-engine`: `cargo test --workspace`. Expected: every `test result:` line reads
  `ok`, with `0 failed`.
- One test: `cargo test -p flux-core <test_name>`.
- Lint, before each commit: `cargo clippy --workspace --all-targets -- -D warnings`. Expected: no output after
  `Finished`.
- Never `git stash`. Never script an edit through a `python -` heredoc. Use the Edit tool.

---

### Task 1: `Held::heartbeat`

**Files:**
- Modify: `crates/flux-core/src/lock/held.rs:1-65` (imports, struct, `rewrite_record`), tests at `:117-167`
- Modify: `crates/flux-core/src/lock/acquire.rs:83-89`
- Modify: `crates/flux-core/src/lock/takeover.rs:99-105`

**Oracle:** spec "The heartbeat" bullets 2-3 (only `last_heartbeat_wall_time` changes; held handle; no sync; the
held record carries the newest value), decision 8 (H-3), decision 9 (H-4).

- [ ] **Step 0: Verify the state.** Open the three files. Confirm:
  - `Held` has exactly the fields `dir`, `lock_name`, `lock`, `identity`, `record` (`held.rs:10-18`);
  - `rewrite_record` is at `held.rs:54-65`;
  - the only `Held {` struct literals are `acquire.rs:83` and `takeover.rs:99`.

  If anything differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: The tests are already written: add them to `held.rs`'s `mod tests`, after
  `a_rewrite_never_changes_the_owner`.**

```rust
    #[test]
    fn a_heartbeat_rewrites_only_its_time_and_never_flushes() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let Ok(Obtained::Held { mut held, .. }) =
            obtain(&site, LockCapability::LocalStrong, Mode::Plain, ID)
        else {
            panic!("a fresh lock is acquired");
        };
        // Before the record exists there is nothing to refresh.
        held.heartbeat(5).unwrap();
        assert!(!fs.called("write_at_start("), "{:?}", fs.calls());
        let first = record(&site, ID, &format!("operations/{ID}"));
        held.write_record(first.clone()).unwrap();
        let later = first.last_heartbeat_wall_time + 7;
        held.heartbeat(later).unwrap();
        let on_disk = decode(&fs.read_file("/p/dest.flux-lock").unwrap());
        let beaten = LockRecord { last_heartbeat_wall_time: later, ..first };
        assert_eq!(on_disk, Decoded::Record(beaten));
        assert!(held.still_owned().unwrap(), "the same file, the same owner");
        let calls = fs.calls();
        let count = |p: &str| calls.iter().filter(|c| c.starts_with(p)).count();
        assert_eq!(
            (count("write_at_start("), count("lock_sync_all(")),
            (2, 1),
            "the heartbeat writes, and only the record write flushes: {calls:?}"
        );
    }

    #[test]
    fn a_rewrite_after_a_heartbeat_keeps_the_newest_time() {
        let (fs, d) = fake();
        let site = LockSite::directory(&d, OsStr::new("dest")).unwrap();
        let Ok(Obtained::Held { mut held, .. }) =
            obtain(&site, LockCapability::LocalStrong, Mode::Plain, ID)
        else {
            panic!("a fresh lock is acquired");
        };
        let first = record(&site, ID, &format!("operations/{ID}"));
        held.write_record(first.clone()).unwrap();
        let later = first.last_heartbeat_wall_time + 7;
        held.heartbeat(later).unwrap();
        // Q-K builds its record from `record()`, whose time is the creation one.
        let none = LockRecord { workspace_path: "none".to_string(), ..first };
        held.rewrite_record(none.clone()).unwrap();
        let newest = LockRecord { last_heartbeat_wall_time: later, ..none };
        let on_disk = decode(&fs.read_file("/p/dest.flux-lock").unwrap());
        assert_eq!(on_disk, Decoded::Record(newest.clone()));
        assert_eq!(held.record(), Some(&newest));
    }
```

- [ ] **Step 2: Run them; they fail to compile** (`no method named heartbeat`).

Run: `cargo test -p flux-core heartbeat`
Expected: `error[E0599]: no method named `heartbeat``.

- [ ] **Step 3: Implement.**

In `held.rs`, the import line `use std::ffi::OsString;` becomes:

```rust
use std::cell::Cell;
use std::ffi::OsString;
```

The struct gains a field after `record`:

```rust
    pub(crate) record: Option<Box<LockRecord>>,
    /// Cut 7b: the newest `last_heartbeat_wall_time` a heartbeat wrote, `None` before the first. Kept beside the record,
    /// because a heartbeat runs where the run holds only a shared borrow of its lock. `rewrite_record` carries it.
    pub(crate) beat: Cell<Option<u64>>,
```

`rewrite_record`'s body starts by carrying the newest time; the rest is unchanged:

```rust
    pub fn rewrite_record(&mut self, record: LockRecord) -> LockResult<()> {
        let mine = self.record.as_deref().expect("rewrite_record follows write_record");
        assert!(
            record.operation_id == mine.operation_id
                && record.owner_instance_id == mine.owner_instance_id,
            "a rewrite keeps the owner"
        );
        // Cut 7b: a rewrite never moves the heartbeat back.
        let record = match self.beat.get() {
            Some(t) => LockRecord { last_heartbeat_wall_time: t, ..record },
            None => record,
        };
        self.lock.write_at_start(&record.encode())?;
        self.lock.sync_all()?;
        self.record = Some(Box::new(record));
        Ok(())
    }
```

Add the doc line "Cut 7b: the record keeps the newest heartbeat time (`heartbeat`), whatever `record` carries." to the
end of `rewrite_record`'s doc comment (`held.rs:49-53`).

Add the method after `rewrite_record`:

```rust
    /// Cut 7b (§101, H-1, H-3, H-4): refresh this operation's record with `last_heartbeat_wall_time` = `now`, every
    /// other field unchanged, so `still_owned` still holds - one write through the handle holding the lock, NOT flushed:
    /// the value is advisory, and a crash can tear the record whether or not it was flushed. No `still_owned` first: the
    /// held handle owns the OS-native lock, which a `--break-lock` takeover must acquire before it writes. Does nothing
    /// before `write_record`: there is no record to refresh.
    pub fn heartbeat(&self, now: u64) -> LockResult<()> {
        let Some(mine) = self.record.as_deref() else { return Ok(()) };
        let beaten = LockRecord { last_heartbeat_wall_time: now, ..mine.clone() };
        self.lock.write_at_start(&beaten.encode())?;
        self.beat.set(Some(now));
        Ok(())
    }
```

In `acquire.rs:83-89` and `takeover.rs:99-105`, add `beat: std::cell::Cell::new(None),` after the `record:` line of
each literal.

- [ ] **Step 4: Run.**

Run: `cargo test --workspace`
Expected: all `ok`, including the two new tests.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/flux-core/src/lock/held.rs crates/flux-core/src/lock/acquire.rs crates/flux-core/src/lock/takeover.rs
git commit -m "lock: Held::heartbeat rewrites the record's time without a flush; a rewrite keeps the newest time"
```

---

### Task 2: the copy's heartbeat callback

**Files:**
- Modify: `crates/flux-core/src/copy.rs:34-54` (`CopyStep`), `:122-130` (`Guard`, `unguarded`), `:373-450` (`copy_file_at`,
  `copy_file_guarded`), `:527-537` (the publish guard), tests at `:1347-1414`
- Modify: `crates/flux-core/src/tree.rs:7`, `:258-266` (`Shared`), `:316`, `:359-365`, `:389`, `:533`, `:557-560`, tests at
  `:1333-1415`
- Modify: `crates/flux-core/src/run/mod.rs:17`, `:221`, `:330` (pass `&no_heartbeat` for now; Task 3 wires the real one)

**Oracle:** spec "The heartbeat": "the copy loop, every 64 KiB ... `copy_file_guarded` and the tree copy that calls it
take the heartbeat as a second callback beside the guard. A copy outside a run ... passes one that does nothing."
Plan decisions 1-2.

- [ ] **Step 0: Verify the state.** Confirm:
  - `CopyStep`'s variants are exactly `Resolve, Source, Gate, Create, Stream, Durability, Metadata, Recheck, Publish`
    (`copy.rs:34-54`);
  - the guard calls in `copy_file_guarded` are at `:409`, `:432` and `:529`, and the loop at `:436-450`;
  - `Shared` has exactly `fs, src_root, src_identity, opts, guard` (`tree.rs:259-266`);
  - `Shared` is built at `tree.rs:316`, `:359`, `:1342`, `:1404` and `run/mod.rs:221`;
  - `copy_file_guarded` is called at `copy.rs:382`, `:1367`, `:1383`, `:1407`, `tree.rs:533` and `run/mod.rs:330`.

  If anything differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: The tests are already written.**

Add to `copy.rs`'s `mod tests`, after `a_guard_that_fails_before_a_discard_keeps_the_temporary_as_a_leftover`:

```rust
    /// A heartbeat that counts its calls, and fails the `fail_at`-th and every one after (0: never).
    fn beat_counting(
        fail_at: u32,
        calls: &std::cell::Cell<u32>,
    ) -> impl Fn() -> flux_fs::Result<()> + '_ {
        move || {
            calls.set(calls.get() + 1);
            if fail_at != 0 && calls.get() >= fail_at {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn the_copy_heartbeats_before_each_guarded_mutation_and_every_64_kib() {
        let fs = FaultFs::new();
        fs.write_file("/src", &vec![7u8; 130 * 1024]);
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        let beat = beat_counting(0, &calls);
        copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &unguarded, &beat)
            .unwrap();
        // The sweep, the create, three chunks (64 + 64 + 2 KiB), the publish.
        assert_eq!(calls.get(), 6);
    }

    #[test]
    fn a_heartbeat_failure_in_the_loop_removes_the_temporary_and_never_publishes() {
        let fs = FaultFs::new();
        fs.write_file("/src", &vec![7u8; 130 * 1024]);
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // The sweep (1) and the create (2) pass; the first chunk's (3) fails.
        let beat = beat_counting(3, &calls);
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &unguarded, &beat)
            .unwrap_err();
        assert_eq!((e.code(), e.step), (Code::IoError, CopyStep::Heartbeat));
        assert_eq!(calls.get(), 3, "no heartbeat after the failed one");
        assert!(e.leftover.is_none() && !fs.exists("/dst.flux-partial.op1"), "the temporary is removed");
        assert!(!fs.exists("/dst") && !fs.called("rename_"), "{:?}", fs.calls());
    }

    #[test]
    fn a_heartbeat_failure_before_the_create_creates_nothing() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        let beat = beat_counting(1, &calls);
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &unguarded, &beat)
            .unwrap_err();
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.called("remove_file") && !fs.called("create_new"), "{:?}", fs.calls());
    }

    #[test]
    fn a_heartbeat_failure_at_the_publish_removes_the_temporary() {
        let fs = FaultFs::new();
        fs.write_file("/src", b"hello");
        let root = fs.destination_root(Path::new("/")).unwrap();
        let calls = std::cell::Cell::new(0);
        // The sweep (1), the create (2), one chunk (3); the publish's (4) fails.
        let beat = beat_counting(4, &calls);
        let e = copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"), &opts(), &unguarded, &beat)
            .unwrap_err();
        assert_eq!(e.step, CopyStep::Heartbeat);
        assert!(!fs.exists("/dst") && !fs.exists("/dst.flux-partial.op1"), "{:?}", fs.calls());
    }
```

In the same tests, the three existing calls `copy_file_guarded(&fs, Path::new("/src"), &root, OsStr::new("dst"),
&opts(), &lost)` (`:1367`) and `(.., &guard)` (`:1383`, `:1407`) gain a last argument `&no_heartbeat`.

Add to `tree.rs`'s `mod tests`, after `the_guard_runs_before_each_directory_is_created`:

```rust
    #[test]
    fn a_failed_heartbeat_aborts_the_whole_tree() {
        let fs = tree();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let calls = std::cell::Cell::new(0);
        // `a`'s sweep, create, one chunk and publish pass (1-4); `sub`'s creation's (5) fails.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 5 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let cx = Shared {
            fs: &fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard: &unguarded,
            beat: &beat,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| reported += 1);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!(calls.get(), 5, "no heartbeat after the failed one");
        assert_eq!((reported, out.failures.total()), (0, 0), "an abort, never a per-file failure");
        assert!(fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }

    #[test]
    fn a_heartbeat_failure_inside_a_file_aborts_the_whole_tree() {
        let fs = tree();
        let source = prepare_source(&fs, Path::new("/src"), Path::new("/dst")).unwrap();
        fs.create_dir(Path::new("/dst")).unwrap();
        let root = fs.destination_root(Path::new("/dst")).unwrap();
        let o = opts();
        let calls = std::cell::Cell::new(0);
        // `a`'s sweep (1) and create (2) pass; its chunk's (3) fails.
        let beat = || {
            calls.set(calls.get() + 1);
            if calls.get() >= 3 {
                Err(FsError::new(Code::IoError, std::io::Error::other("beat")))
            } else {
                Ok(())
            }
        };
        let cx = Shared {
            fs: &fs,
            src_root: Path::new("/src"),
            src_identity: source.identity,
            opts: &o,
            guard: &unguarded,
            beat: &beat,
        };
        let mut out = TreeOutcome::default();
        let mut reported = 0;
        let (_root, r) = copy_tree_at(&cx, source.events, root, &mut out, &mut |_| reported += 1);
        assert_eq!(r.unwrap_err().step, CopyStep::Heartbeat);
        assert_eq!((reported, out.files_copied), (0, 0), "the walk stops at the failed file");
        assert!(!fs.exists("/dst/a") && !fs.exists("/dst/sub"));
    }
```

The two existing `Shared { .. }` literals in `tree.rs`'s tests (`:1342`, `:1404`) gain `beat: &no_heartbeat,` after
`guard`. `tree.rs`'s test imports must reach `no_heartbeat` (it comes in through `use super::*` once the line-7 import
names it).

**Oracle for the counts:** the fake's reader returns `min(remaining, buffer)` per read (`fault_fs.rs:287-293`), so a
130 KiB file streams 64 + 64 + 2 KiB. `tree()` holds `a` before `sub` (the order `the_guard_runs_before_each_directory_is_created`
pins at `tree.rs:1387`). If a count differs, STOP and report it. Do not edit the test.

- [ ] **Step 2: Run; they fail to compile** (wrong argument count, no `CopyStep::Heartbeat`, no field `beat`).

Run: `cargo test -p flux-core heartbeat`
Expected: compile errors.

- [ ] **Step 3: Implement.**

`copy.rs`, `CopyStep` gains a last variant after `Publish`:

```rust
    /// Step 7: the publishing rename.
    Publish,
    /// Cut 7b: refreshing the destination lock's heartbeat (§101). Its failure stops the copy at that point, and a
    /// tree's whole walk.
    Heartbeat,
}
```

After `unguarded` (`copy.rs:127-130`):

```rust
/// §101's heartbeat (cut 7b): called before every guarded destination mutation and after every 64 KiB written. The run
/// refreshes its lock record there once the interval has passed; its failure stops the copy at `CopyStep::Heartbeat`. A
/// copy that holds no lock passes `&no_heartbeat`.
pub type Heartbeat<'g> = dyn Fn() -> flux_fs::Result<()> + 'g;

/// The heartbeat of a copy that holds no lock.
pub(crate) fn no_heartbeat() -> flux_fs::Result<()> {
    Ok(())
}
```

`copy_file_at` (`:375-383`) passes it:

```rust
    copy_file_guarded(fs, src, parent, name, opts, &unguarded, &no_heartbeat)
```

and its doc line `:373-374` reads: "Copy `src` to `name` inside `parent`, writing ONLY through `parent`, holding no lock:
`copy_file_guarded` with a guard that always passes and no heartbeat."

`copy_file_guarded`:
- Signature: add `beat: &Heartbeat<'_>,` after `guard: &Guard<'_>,`.
- Doc comment: append the paragraph "`beat` runs immediately before each of those guard calls except the removal's,
  and after every 64 KiB written (cut 7b). Its failure is `CopyStep::Heartbeat`: before the create it creates nothing;
  after it, the temporary is removed as for any other failure."
- Step 1 (`:409`) becomes:

```rust
    beat().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
```

- Step 3 (`:432`) becomes:

```rust
    beat().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
    guard().map_err(|e| CopyError::at(CopyStep::Create, e))?;
```

- The loop (`:438-450`) becomes:

```rust
    loop {
        let n = match std::io::Read::read(&mut reader, &mut buf) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) => {
                return Err(discard(parent, &temp, CopyStep::Stream, copy_code(&e), e, guard));
            }
        };
        if let Err(e) = writer.write_all(&buf[..n]) {
            return Err(discard(parent, &temp, CopyStep::Stream, copy_code(&e), e, guard));
        }
        bytes_copied += n as u64;
        // §101 (cut 7b): the heartbeat, once per 64 KiB chunk.
        if let Err(e) = beat() {
            return Err(discard(parent, &temp, CopyStep::Heartbeat, e.code, e.source, guard));
        }
    }
```

- Before the publish guard (`:527-529`), insert, after the comment block and before `if let Err(lost) = guard() {`:

```rust
    if let Err(e) = beat() {
        return Err(discard(parent, &temp, CopyStep::Heartbeat, e.code, e.source, guard));
    }
```

`tree.rs`:
- Line 7's import adds `Heartbeat` and `no_heartbeat`.
- `Shared` gains, after `guard`:

```rust
    /// §101's heartbeat (cut 7b), beside the guard; `&no_heartbeat` for a copy that holds no lock.
    pub(crate) beat: &'c Heartbeat<'c>,
```

- `run_tree`'s literal (`:316`) adds `beat: &no_heartbeat`.
- `walk_into`'s re-built `cx` (`:359-365`) adds `beat: cx.beat,`.
- The directory creation (`:388-389`) becomes:

```rust
                    // §101, then §99, before the directory's creation.
                    (cx.beat)().map_err(|e| CopyError::at(CopyStep::Heartbeat, e))?;
                    (cx.guard)().map_err(|e| CopyError::at(CopyStep::Create, e))?;
```

- `copy_one`'s call (`:533`) passes `cx.beat` after `cx.guard`.
- `copy_one`'s abort (`:557-560`) becomes:

```rust
            // §99 (cut 7a Part 3b): this run no longer owns the destination's lock; or §101 (cut 7b): its heartbeat
            // failed. The whole operation stops.
            if e.code() == Code::TargetLockBusy || e.step == CopyStep::Heartbeat {
                return Err(e);
            }
```

- `copy_tree_at`'s doc (`:323-326`) adds the sentence "`cx.beat` runs before each of those guard calls; its failure
  aborts the whole copy too (cut 7b)."

`run/mod.rs`: line 17 imports `no_heartbeat` from `crate::copy`. The `Shared` literal (`:221`) adds
`beat: &no_heartbeat`. The `copy_file_guarded` call (`:330`) passes `&no_heartbeat` last. Task 3 replaces both.

- [ ] **Step 4: Run.**

Run: `cargo test --workspace`
Expected: all `ok`, including the six new tests.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean. (`copy_file_guarded` now takes seven parameters; clippy's `too_many_arguments` fires only above
seven.)

- [ ] **Step 5: Commit.**

```bash
git add crates/flux-core/src/copy.rs crates/flux-core/src/tree.rs crates/flux-core/src/run/mod.rs
git commit -m "copy: a heartbeat callback beside the guard, every 64 KiB and before each guarded mutation; its failure aborts a tree"
```

---

### Task 3: the run's heartbeat during the copy

**Files:**
- Modify: `crates/flux-core/src/run/mod.rs:37-69` (`RunConfig`), `:213-245` (the tree's step 6 and `Ended`), `:322-346`
  (the file's), `:452-455` (`fail`)
- Modify: `crates/flux-core/src/run/session.rs:13-25` (imports, `Locked`), `:115`, `:119` (the two `Locked` literals),
  `:233-244` (`guarded`); a new `Pulse` and `beat_error`
- Modify: `crates/flux-cli/src/main.rs:108-117` (`run_config`)
- Test: `crates/flux-core/src/run/tests.rs:25-34` (`cfg`), new tests at the end

**Oracle:** spec "The heartbeat" (interval, one retry, the failure paths "Intact" and "Torn", "never `TARGET_LOCK_BUSY`",
"no further heartbeat attempt") and Testing's "Heartbeat" and "No heartbeat in the finish" bullets. Plan decisions 3-6.

- [ ] **Step 0: Verify the state.** Confirm:
  - `RunConfig` has exactly the fields `restart, break_lock, operation_id, owner_instance_id, boot_session_id,
    before_mutation` (`mod.rs:43-56`);
  - `Locked` has exactly `held, state, lock_shown` (`session.rs:20-25`), built at `session.rs:115` and `:119`;
  - `guarded(held)` is at `session.rs:238-244`, called at `mod.rs:219` and `:328`;
  - `fail` starts at `mod.rs:452`;
  - `RunConfig {` is built only at `run/tests.rs:26` and `flux-cli/src/main.rs:109`.

  If anything differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: The tests are already written.**

In `run/tests.rs`, `cfg()` gains the field (after `before_mutation: None,`):

```rust
        // Plan decision 6: the existing tests never heartbeat.
        heartbeat_interval: std::time::Duration::from_secs(3600),
```

Append to `run/tests.rs`:

```rust
// Cut 7b Part 2: the heartbeat.

/// Every heartbeat due at once.
fn beating() -> RunConfig {
    RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..cfg() }
}

fn lock_record(bytes: &[u8]) -> LockRecord {
    match decode(bytes) {
        Decoded::Record(r) => r,
        other => panic!("expected a record, got {other:?}"),
    }
}

fn count(calls: &[String], prefix: &str) -> usize {
    calls.iter().filter(|c| c.starts_with(prefix)).count()
}

/// Calls 1 (this run's record) and on succeed; the `nth` and the `nth + 1` `write_at_start` fail - a heartbeat and its
/// retry. With `tear`, the lock's bytes are torn just before the first of them.
fn fail_heartbeat(fs: &FaultFs, nth: u32, lock: &'static str, tear: bool) {
    fs.on_nth("write_at_start", nth, move |fs| {
        if tear {
            fs.write_file(lock, b"torn");
        }
        fs.fail("write_at_start", Code::IoError);
    });
    fs.fail_nth("write_at_start", nth, Code::IoError, std::io::ErrorKind::Other);
}

fn file_error(r: &Run<Result<flux_fs::Outcome, CopyError>>) -> &CopyError {
    match &r.copy {
        Some(Err(e)) => e,
        other => panic!("expected a failed copy, got {other:?}"),
    }
}

#[test]
fn a_heartbeat_refreshes_the_lock_record_keeping_its_owner_and_never_flushes() {
    let fs = fake();
    let seen = Arc::new(Mutex::new(None));
    let keep = Arc::clone(&seen);
    // `rename_replace`: CREATED (1), TRANSFERRING (2), the publish (3), after the copy's heartbeats.
    fs.on_nth("rename_replace", 3, move |fs| *keep.lock().unwrap() = fs.read_file(T_LOCK));
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    let during = lock_record(&seen.lock().unwrap().clone().expect("the lock, read at the publish"));
    assert!(
        during.last_heartbeat_wall_time > during.creation_wall_time,
        "the heartbeat moved: {during:?}"
    );
    assert_eq!((during.operation_id.as_str(), during.owner_instance_id), (ID, id(0xee)));
    assert_eq!(during.workspace_path, format!("adjacent/{ID}"));
    let c = calls(&fs);
    assert!(count(&c, "write_at_start(") > 2, "heartbeats were written: {c:?}");
    assert_eq!(count(&c, "lock_sync_all("), 2, "only the record and Q-K flush: {c:?}");
}

#[test]
fn a_heartbeat_waits_for_its_interval() {
    let fs = fake();
    let r = run_file(&fs, &cfg());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
    assert_eq!(count(&calls(&fs), "write_at_start("), 2, "the record and Q-K, no heartbeat");
}

#[test]
fn a_failed_heartbeat_write_is_retried_once() {
    let fs = fake();
    // The first heartbeat (write 2) fails; its retry (write 3) succeeds.
    fs.fail_nth("write_at_start", 2, Code::IoError, std::io::ErrorKind::Other);
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?} {:?}", r.stop, r.copy);
    assert!(!fs.exists(T_LOCK) && !fs.exists(record_path(ID)), "a clean run");
}

#[test]
fn two_failed_heartbeat_writes_stop_the_copy_naming_the_lock_and_the_run_records_failed() {
    let fs = fake();
    fail_heartbeat(&fs, 2, T_LOCK, false);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::IoError));
    let message = e.cause.source.to_string().replace('\\', "/");
    assert!(message.contains(T_LOCK), "names the lock: {message}");
    assert!(r.stop.is_none(), "one error for it: {:?}", r.stop);
    assert_eq!(file_record(&fs, ID).state, OpState::Failed, "the record was intact");
    assert!(!fs.exists(T_LOCK), "released");
    assert!(!fs.exists("/p/t"), "never published");
}

#[test]
fn a_torn_heartbeat_is_that_failure_never_a_lost_lock() {
    let fs = fake();
    fail_heartbeat(&fs, 2, T_LOCK, true);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!((e.step, e.code()), (CopyStep::Heartbeat, Code::IoError));
    assert!(r.stop.is_none(), "no stop of the finish's own, no TARGET_LOCK_BUSY: {:?}", r.stop);
    assert_eq!(
        file_record(&fs, ID).state,
        OpState::Transferring,
        "nothing written without proof of ownership"
    );
    assert_eq!(fs.read_file(T_LOCK).as_deref(), Some(&b"torn"[..]), "closed without unlinking");
}

#[test]
fn a_temporary_kept_after_a_torn_heartbeat_names_the_heartbeat_not_another_run() {
    let fs = fake();
    // Writes: the record (1), then the sweep's (2), the create's (3) and the chunk's (4) heartbeats; the publish's (5)
    // and its retry (6) fail, the record torn.
    fail_heartbeat(&fs, 5, T_LOCK, true);
    let r = run_file(&fs, &beating());
    let e = file_error(&r);
    assert_eq!(e.step, CopyStep::Heartbeat);
    let (left, why) = e.leftover.as_ref().expect("the temporary stays: the guard fails on the torn record");
    assert!(left.to_string_lossy().contains("t.flux-partial."), "{left:?}");
    let why = why.to_string();
    assert!(why.contains("heartbeat") && !why.contains("another run"), "{why}");
    assert!(fs.exists(format!("/p/t.flux-partial.{ID}")));
}

#[test]
fn a_tree_stops_at_a_failed_heartbeat_and_records_failed() {
    let fs = fake();
    fail_heartbeat(&fs, 2, LOCK, false);
    let (r, got) = run_tree(&fs, &beating());
    let a = aborted(&r);
    assert_eq!((a.error.step, a.error.code()), (CopyStep::Heartbeat, Code::IoError));
    assert!(got.is_empty(), "an abort, never a file's failure: {got:?}");
    assert!(r.stop.is_none(), "{:?}", r.stop);
    assert_eq!(manifest(&fs, ID).state, OpState::Failed);
    assert!(!fs.exists(LOCK), "released");
    assert!(!fs.exists("/p/dest/a"));
}

#[test]
fn the_finish_never_heartbeats() {
    let fs = fake();
    let r = run_file(&fs, &beating());
    assert!(r.stop.is_none() && matches!(r.copy, Some(Ok(_))), "{:?}", r.stop);
    let c = calls(&fs);
    let publish = at(&c, &format!("rename_replace(/p/t.flux-partial.{ID}"));
    let after = &c[publish..];
    assert_eq!(
        (count(after, "write_at_start("), count(after, "lock_sync_all(")),
        (1, 1),
        "after the publish only Q-K writes the record: {after:?}"
    );
}
```

The test module's imports (`run/tests.rs:7`) become `use crate::lock::record::{Decoded, LockRecord, decode};`.

**Oracle for the counts:** the single file's heartbeats, in order, are: the sweep's, the create's, one chunk's (`/src/a`
is 1 byte) and the publish's. They are `write_at_start` calls 2-5, after the record's (1). The record's
`workspace_path` for a single file is `adjacent/<id>` (`Place::workspace_path`, `run/place.rs:34`). Read the
`FilePlace` impl to confirm it. If it differs, STOP and report; do not edit the test.

- [ ] **Step 2: Run; they fail to compile** (no field `heartbeat_interval`).

Run: `cargo test -p flux-core heartbeat`
Expected: `error[E0560]: struct `RunConfig` has no field named `heartbeat_interval``.

- [ ] **Step 3: Implement.**

`run/mod.rs`, `RunConfig` gains, after `before_mutation`:

```rust
    /// §101: how often the run refreshes its lock record's heartbeat (cut 7b). The CLI passes `HEARTBEAT_INTERVAL`.
    pub heartbeat_interval: std::time::Duration,
```

Its `Debug` impl adds `.field("heartbeat_interval", &self.heartbeat_interval)` before `.finish()`. After the
`RunConfig` impl:

```rust
/// §101's heartbeat interval: 5 s.
pub const HEARTBEAT_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
```

`run/session.rs`:
- Imports add `use std::cell::Cell;` and `use std::time::{Duration, Instant};`, and `wall_time_ns` is already
  imported from `crate::state`.
- `Locked` gains, after `lock_shown`:

```rust
    /// The heartbeat (cut 7b): due, done, or failed.
    pub(crate) pulse: Pulse,
```

- Both literals (`:115`, `:119`) add `pulse: Pulse::new(cfg.heartbeat_interval)`.
- After `Locked`:

```rust
/// The run's heartbeat (cut 7b, §101, decisions 6-7): the interval, the instant of the last record write, and whether
/// a heartbeat has failed. In `Cell`s: the heartbeat runs where the run holds only a shared borrow of its lock - the
/// copy's callbacks and the `--restart` sweep's checks.
pub(crate) struct Pulse {
    interval: Duration,
    last: Cell<Instant>,
    failed: Cell<bool>,
}

impl Pulse {
    /// Made as the record is written: the interval counts from then.
    pub(crate) fn new(interval: Duration) -> Self {
        Pulse { interval, last: Cell::new(Instant::now()), failed: Cell::new(false) }
    }

    /// A heartbeat has failed: the run makes no further attempt, and the finish handles a record it can no longer read
    /// as its own (`fail`).
    pub(crate) fn failed(&self) -> bool {
        self.failed.get()
    }

    /// Refresh `held`'s record once the interval has passed since the last write; a failed write is retried once, at
    /// once (decision 7). After a failure, nothing: a second failure can never replace or nest inside the first. The
    /// error is the plain I/O error; each caller names the lock.
    pub(crate) fn beat<D: DirHandle>(&self, held: &Held<'_, D>) -> flux_fs::Result<()> {
        if self.failed.get() || self.last.get().elapsed() < self.interval {
            return Ok(());
        }
        let now = wall_time_ns();
        match held.heartbeat(now).or_else(|_| held.heartbeat(now)) {
            Ok(()) => {
                self.last.set(Instant::now());
                Ok(())
            }
            Err(e) => {
                self.failed.set(true);
                Err(lock_io(e))
            }
        }
    }
}

/// A heartbeat failure as the copy reports it (decision 7): the I/O error, naming the lock path, never
/// TARGET_LOCK_BUSY.
pub(crate) fn beat_error(lock_shown: &Path, e: FsError) -> FsError {
    let code = if e.code == Code::TargetLockBusy { Code::IoError } else { e.code };
    let message = format!(
        "the heartbeat could not refresh the lock record {}: {}",
        lock_shown.display(),
        e.source
    );
    FsError::new(code, std::io::Error::new(e.source.kind(), message))
}
```

- `guarded` (`:236-244`) takes the pulse:

```rust
const AFTER_HEARTBEAT: &str =
    "kept: a heartbeat write failed, so this run cannot tell whether the destination's lock is still its own";

/// The copy's guard (`copy_file_guarded`, `copy_tree_at`): §99 as the `FsError` the engine aborts on. A check that
/// cannot tell counts as lost: the copy must not go on writing. After a failed heartbeat (cut 7b) its reason names the
/// heartbeat: the record may be the one the heartbeat tore, not another run's.
pub(crate) fn guarded<D: DirHandle>(held: &Held<'_, D>, pulse: &Pulse) -> flux_fs::Result<()> {
    let why = if pulse.failed() { AFTER_HEARTBEAT } else { LOST };
    match held.still_owned() {
        Ok(true) => Ok(()),
        Ok(false) => Err(FsError::new(Code::TargetLockBusy, std::io::Error::other(why))),
        Err(e) if pulse.failed() => {
            Err(FsError::new(Code::TargetLockBusy, std::io::Error::other(format!("{why}: {}", lock_io(e).source))))
        }
        Err(e) => Err(FsError::new(Code::TargetLockBusy, lock_io(e).source)),
    }
}
```

`run/mod.rs`, the tree's step 6 (`:214-221`):

```rust
    let walked = {
        let guard = || {
            if let Some(hook) = &cfg.before_mutation {
                hook();
            }
            guarded(&locked.held, &locked.pulse)
        };
        let beat =
            || locked.pulse.beat(&locked.held).map_err(|e| beat_error(&locked.lock_shown, e));
        let cx = Shared {
            fs,
            src_root,
            src_identity: source.identity,
            opts: &opts,
            guard: &guard,
            beat: &beat,
        };
```

The tree's `Ended` (`:239-244`) checks the heartbeat first:

```rust
    let ended = match &result {
        Ok(_) => Ended::Completed { leftovers, published: None },
        // Cut 7b: a failed heartbeat is the copy's failure, whatever its code.
        Err(_) if locked.pulse.failed() => Ended::Failed,
        Err(a) if a.error.code() == Code::TargetLockBusy => Ended::Lost,
        Err(a) if a.refused_unchanged() => Ended::RefusedUnchanged,
        Err(_) => Ended::Failed,
    };
```

The single file's step 6 (`:323-331`):

```rust
    let copied = {
        let guard = || {
            if let Some(hook) = &cfg.before_mutation {
                hook();
            }
            guarded(&locked.held, &locked.pulse)
        };
        let beat =
            || locked.pulse.beat(&locked.held).map_err(|e| beat_error(&locked.lock_shown, e));
        copy_file_guarded(fs, src, &parent, name, &opts, &guard, &beat)
    }
```

The single file's `Ended` (`:339-346`):

```rust
    let ended = match &copied {
        Ok(o) => Ended::Completed { leftovers: Vec::new(), published: Some(o.published_identity) },
        // Cut 7b: a failed heartbeat is the copy's failure, whatever its code.
        Err(_) if locked.pulse.failed() => Ended::Failed,
        Err(e) if e.code() == Code::TargetLockBusy => Ended::Lost,
        Err(e) if e.code() == Code::SafetyRejected && e.leftover.is_none() => {
            Ended::RefusedUnchanged
        }
        Err(_) => Ended::Failed,
    };
```

Line 17's import drops `no_heartbeat` (Task 2's stand-in). Line 30 becomes
`use session::{Locked, beat_error, from_lock, guarded, lock_io, open_operation, owned};`.

`fail` (`:452-455`) becomes:

```rust
fn fail<D: DirHandle, P: Place<D>>(place: &P, mut locked: Locked<'_, D>) -> Option<RunError> {
    if let Err(fault) = owned(&locked.held, &locked.lock_shown) {
        // Cut 7b decision 7, "Torn": after a failed heartbeat, a record that no longer proves ownership is most likely
        // the one the heartbeat tore. That failure is already the report: write nothing, add no stop, and close without
        // unlinking as `locked` drops. The next run meets TARGET_LOCK_UNCERTAIN, which `--restart --break-lock` clears.
        if locked.pulse.failed() {
            return None;
        }
        return Some(fault.into_error(RunStep::State));
    }
```

The rest of `fail` is unchanged. `fail`'s doc comment (`:450-451`) adds: "After a failed heartbeat, a failed check ends
it with nothing written (cut 7b)."

`flux-cli/src/main.rs`, `run_config` (`:108-117`) adds, after `before_mutation: debug_hook(),`:

```rust
        heartbeat_interval: flux_core::run::HEARTBEAT_INTERVAL,
```

- [ ] **Step 4: Run.**

Run: `cargo test --workspace`
Expected: all `ok`, including the eight new tests.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/flux-core/src/run/mod.rs crates/flux-core/src/run/session.rs crates/flux-core/src/run/tests.rs crates/flux-cli/src/main.rs
git commit -m "run: the heartbeat during the copy, retried once; its failure is one error naming the lock, and a torn record ends with nothing written"
```

---

### Task 4: the heartbeat during `--restart`

**Files:**
- Modify: `crates/flux-core/src/run/session.rs:133-142` (supersede's fault), `:207-222` (`Fault`); a new `checked`
- Modify: `crates/flux-core/src/run/restart.rs:5`, `:26`, `:34`, `:36`
- Modify: `crates/flux-core/src/run/place.rs:5`, `:8`, `:44-52`, `:217-223`, `:270`, `:364-370`, `:377`
- Test: `crates/flux-core/src/run/tests.rs`, new tests at the end

**Oracle:** spec "The heartbeat": "the `--restart` sweep, before each ownership check" and "during the `--restart`
sweep, before any copy, the run's stop (`RunError::Failed` at the lock step)". Testing's "Heartbeat during
`--restart`" bullet.

- [ ] **Step 0: Verify the state.** Confirm:
  - `Fault` has exactly `Lost` and `Io(PathBuf, FsError)` (`session.rs:208-213`);
  - `supersede` calls `owned(&locked.held, &locked.lock_shown)?` at `restart.rs:26` and `:36`, and
    `place.sweep(priors, &locked.held, &locked.lock_shown, warnings)?` at `:34`;
  - both `sweep` impls call `owned(held, lock_shown)?` (`place.rs:270`, `:377`), and `place.rs` uses `owned` and
    `Held` nowhere else.

  If anything differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: The tests are already written.** Append to `run/tests.rs`:

```rust
#[test]
fn restart_heartbeats_during_its_sweep_before_the_copy() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    let (r, _) = run_tree(&fs, &RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..restart() });
    ok(&r);
    let c = calls(&fs);
    let before = &c[..at(&c, &format!("remove_file({partial})"))];
    // The record (1), then the heartbeats before the ABANDONED write and before the partial's deletion.
    assert_eq!(count(before, "write_at_start("), 3, "{before:?}");
    assert_eq!(count(before, "lock_sync_all("), 1, "only the record flushes: {before:?}");
}

#[test]
fn a_failed_heartbeat_during_restart_stops_the_run_at_the_lock_step_and_copies_nothing() {
    let fs = fake();
    prior(&fs, 5, OpState::Failed);
    let partial = format!("/p/dest/old.flux-partial.{}", id(5));
    fs.write_file(&partial, b"half");
    // The first heartbeat, before the ABANDONED write, and its retry.
    fail_heartbeat(&fs, 2, LOCK, false);
    let (r, _) = run_tree(&fs, &RunConfig { heartbeat_interval: std::time::Duration::ZERO, ..restart() });
    assert_eq!(failed_at(&r.stop), (RunStep::Lock, LOCK.to_string()));
    assert!(r.copy.is_none(), "no copy runs");
    assert!(r.warnings.is_empty(), "one error for it: {:?}", r.warnings);
    assert_eq!(manifest(&fs, &id(5)).state, OpState::Failed, "the prior is untouched");
    assert!(fs.exists(&partial));
    assert_eq!(manifest(&fs, ID).state, OpState::Failed, "the record was intact");
    assert!(!fs.exists(LOCK), "released");
}
```

**Oracle for the count:** with one prior and one partial, `supersede` heartbeats before the ABANDONED write
(`restart.rs:26`) and the sweep heartbeats before the partial's deletion (`place.rs:270`). Both come before
`remove_file(<partial>)`, so three writes precede it. If the count differs, STOP and report; do not edit the test.

- [ ] **Step 2: Run; the first fails, the second fails.**

Run: `cargo test -p flux-core restart`
Expected: `restart_heartbeats_during_its_sweep_before_the_copy` fails on the count (1, not 3).
`a_failed_heartbeat_during_restart_stops_the_run_at_the_lock_step_and_copies_nothing` fails on `failed_at`, because
the first heartbeat is then the copy's.

- [ ] **Step 3: Implement.**

`session.rs`, `Fault` gains a variant, and `into_error` its arm:

```rust
/// Why a §99 check did not pass.
pub(crate) enum Fault {
    /// `still_owned` said no: another run took the lock over or removed it.
    Lost,
    /// It could not tell, or a step after it failed, at this path.
    Io(PathBuf, FsError),
    /// The heartbeat before it failed, twice (cut 7b decision 7), at the lock's path.
    Heartbeat(PathBuf, FsError),
}

impl Fault {
    pub(crate) fn into_error(self, step: RunStep) -> RunError {
        match self {
            Fault::Lost => lost(),
            Fault::Io(path, error) => RunError::Failed { step, path, error },
            // Decision 7: a heartbeat outside the copy fails the lock step, whatever step it interrupted.
            Fault::Heartbeat(path, error) => RunError::Failed { step: RunStep::Lock, path, error },
        }
    }
}
```

After `owned`:

```rust
/// The `--restart` sweep's check (cut 7b, "The heartbeat"): the heartbeat, then §99. Two calls: `owned` stays a pure
/// check.
pub(crate) fn checked<D: DirHandle>(locked: &Locked<'_, D>) -> Result<(), Fault> {
    if let Err(e) = locked.pulse.beat(&locked.held) {
        return Err(Fault::Heartbeat(locked.lock_shown.clone(), e));
    }
    owned(&locked.held, &locked.lock_shown)
}
```

`open_operation`'s supersede match (`:135-142`):

```rust
        if let Err(fault) = supersede(place, &locked, &priors, warnings) {
            return Err(match fault {
                Fault::Lost => lost(),
                Fault::Io(path, error) => {
                    stop_after_record(place, locked, RunStep::Restart, path, error)
                }
                // Decision 7: the run's stop, at the lock step, naming the lock.
                Fault::Heartbeat(path, error) => {
                    stop_after_record(place, locked, RunStep::Lock, path, error)
                }
            });
        }
```

`restart.rs`:
- Line 5: `use super::session::{Fault, Locked, checked};`
- Lines 26 and 36: `checked(locked)?;`
- Line 34: `let gone = place.sweep(priors, locked, warnings)?;`
- The doc comment's `S21_1_s3` paragraph (`:14-17`) adds: "Each check is preceded by the heartbeat (cut 7b)."

`place.rs`:
- Line 5: `use super::session::{Fault, Locked, checked, failed, from_lock};`
- Line 8: `use crate::lock::LockResult;`
- The trait method (`:44-52`):

```rust
    /// `--restart`: delete the superseded operations' partials, the heartbeat and `still_owned` before each deletion.
    /// Returns the ids whose partials are all gone; a partial that stays keeps its operation's ABANDONED state as its
    /// record (J1).
    fn sweep(
        &self,
        priors: &[PriorOp],
        locked: &Locked<'_, D>,
        warnings: &mut Vec<RunWarning>,
    ) -> Result<Vec<String>, Fault>;
```

- The tree's impl (`:217-223`) takes `locked: &Locked<'_, F::Dir>` in place of `held` and `lock_shown`, and line 270
  becomes `checked(locked)?;`.
- The file's impl (`:364-370`) takes `locked: &Locked<'_, D>`, and line 377 becomes `checked(locked)?;`.

- [ ] **Step 4: Run.**

Run: `cargo test --workspace`
Expected: all `ok`.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/flux-core/src/run/session.rs crates/flux-core/src/run/restart.rs crates/flux-core/src/run/place.rs crates/flux-core/src/run/tests.rs
git commit -m "run: --restart heartbeats before each ownership check of its sweep; a failure there stops the run at the lock step"
```

---

### Task 5: the CLI's interval and its end-to-end test

**Files:**
- Modify: `crates/flux-cli/src/main.rs:106-141`
- Test: `crates/flux-cli/tests/run.rs:4`, `:45-79` (`Stalled`), new tests after
  `a_killed_single_file_run_leaves_a_resumable_record_that_restart_supersedes`

**Oracle:** spec "The heartbeat" bullet 1 ("The CLI passes 5 s. A debug build reads `FLUX_TEST_HEARTBEAT_INTERVAL_MS`
... A release build has no override") and Testing's "End to end (CLI)" bullet.

- [ ] **Step 0: Verify the state.** Confirm:
  - `run_config` is at `main.rs:108-118` with `heartbeat_interval: flux_core::run::HEARTBEAT_INTERVAL` (Task 3);
  - `debug_hook` has a `#[cfg(debug_assertions)]` and a `#[cfg(not(debug_assertions))]` form (`main.rs:122-141`);
  - `Stalled::start` is at `tests/run.rs:50-72`.

  If anything differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: The tests are already written.**

`tests/run.rs` line 4 becomes `use flux_core::lock::record::{Decoded, LockRecord, decode};`.

`Stalled::start` (`:50-72`) becomes a call to a new `start_env`:

```rust
    fn start(src: &Path, dst: &Path, at: u32, marks: &Path) -> Self {
        Self::start_env(src, dst, at, marks, &[])
    }

    /// `start`, with extra environment variables for the run.
    fn start_env(src: &Path, dst: &Path, at: u32, marks: &Path, env: &[(&str, &str)]) -> Self {
        let announced = marks.join(format!("stalled-{at}"));
        let child = flux()
            .arg("copy")
            .arg(src)
            .arg(dst)
            .env("FLUX_TEST_STALL_AT", at.to_string())
            .env("FLUX_TEST_STALL_FILE", &announced)
            .envs(env.iter().copied())
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
```

New tests:

```rust
/// The lock record a stalled run holds at `lock`.
fn held_record(lock: &Path) -> LockRecord {
    match decode(&std::fs::read(lock).unwrap()) {
        Decoded::Record(r) => r,
        other => panic!("expected a record, got {other:?}"),
    }
}

#[test]
fn a_running_copy_moves_its_lock_records_heartbeat() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // Interval 0: `a`'s heartbeat runs before its first guarded mutation, where the run stalls.
    let s = Stalled::start_env(&src, &dst, 1, marks.path(), &[("FLUX_TEST_HEARTBEAT_INTERVAL_MS", "0")]);
    let r = held_record(&d.path().join("dst.flux-lock"));
    assert!(r.last_heartbeat_wall_time > r.creation_wall_time, "{r:?}");
    s.kill();
}

#[test]
fn without_the_override_a_short_run_keeps_its_creation_heartbeat() {
    let d = TempDir::new().unwrap();
    let marks = TempDir::new().unwrap();
    let src = tree_in(d.path());
    let dst = d.path().join("dst");
    // 5 s have not passed by the first guarded mutation.
    let s = Stalled::start(&src, &dst, 1, marks.path());
    let r = held_record(&d.path().join("dst.flux-lock"));
    assert_eq!(r.last_heartbeat_wall_time, r.creation_wall_time, "{r:?}");
    s.kill();
}
```

**Oracle:** the lock is read while held. The Windows OS-native lock covers only byte `1 << 62`
(`flux-platform/src/lock_file.rs:29`, `:126-135`), and `flock` blocks no read, so a read succeeds on every platform.

- [ ] **Step 2: Run; the first fails, the second passes.**

Run: `cargo test -p flux-cli heartbeat`
Expected: `a_running_copy_moves_its_lock_records_heartbeat` FAILS: no override yet, so the times are equal.

- [ ] **Step 3: Implement.** In `main.rs`, `run_config` (Task 3's line) becomes
`heartbeat_interval: heartbeat_interval(),`. Its doc (`:106-107`) reads: "The run's configuration: two fresh ids for
this invocation (the operation and this process, cut 7a), the boot session, and §101's heartbeat interval; in a debug
build, the crash hook `debug_hook` and the interval override `heartbeat_interval` read from the environment."

After the two `debug_hook` forms:

```rust
/// §101's 5 s (cut 7b). In a debug build, `FLUX_TEST_HEARTBEAT_INTERVAL_MS=<n>` replaces it, so an end-to-end test can
/// watch the lock record's heartbeat move. A release build has no override.
#[cfg(debug_assertions)]
fn heartbeat_interval() -> std::time::Duration {
    std::env::var("FLUX_TEST_HEARTBEAT_INTERVAL_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(flux_core::run::HEARTBEAT_INTERVAL)
}

#[cfg(not(debug_assertions))]
fn heartbeat_interval() -> std::time::Duration {
    flux_core::run::HEARTBEAT_INTERVAL
}
```

- [ ] **Step 4: Run.**

Run: `cargo test --workspace`
Expected: all `ok`, including both new tests.

Run: `cargo clippy --workspace --all-targets -- -D warnings`
Expected: clean.

- [ ] **Step 5: Commit.**

```bash
git add crates/flux-cli/src/main.rs crates/flux-cli/tests/run.rs
git commit -m "cli: the 5 s heartbeat, overridable in a debug build; end to end, a stalled run's lock record shows it"
```

---

### Task 6: the spec records the plan's refinement

**Files:**
- Modify: `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md:148-157` ("The heartbeat"), `:241-247`
  ("Spec refinements recorded by this cut")

- [ ] **Step 1:** In "The heartbeat", replace the sub-bullet "the run's guard closure, before each guarded mutation;"
  (`:152`) with:

```markdown
    - the copy, immediately before each guard call that precedes a guarded mutation (Part 2 plan, decision 1: inside
      the guard closure its failure would be a per-file failure at `CopyStep::Create`, or `TARGET_LOCK_BUSY`);
```

- [ ] **Step 2:** Append to "Spec refinements recorded by this cut":

```markdown
- The copy calls the heartbeat beside its guard, not inside the run's guard closure: its failure is
  `CopyStep::Heartbeat`, which a tree treats as an abort, as it treats `TARGET_LOCK_BUSY`. A failed copy's removal of
  its temporary is guarded but does not heartbeat. After a failed heartbeat, the guard's reason for keeping a temporary
  names the heartbeat (Part 2 plan, decisions 1 and 4).
```

- [ ] **Step 3: Commit.**

```bash
git add docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md
git commit -m "spec: the copy calls the heartbeat beside its guard (cut 7b Part 2 plan)"
```

---

## After the tasks (driver, not a subagent)

1. **Gates:** `just check`, `just check-linux`, `just check-mac`; then push, and CI.
2. **Mutants.** Each new test must go red under its mutant. Run with `cargo test --workspace --no-fail-fast`, and
   confirm the NAMED test is among the failures:

| Mutant (file: change) | Must turn red |
|---|---|
| `held.rs` `heartbeat`: `last_heartbeat_wall_time: now` -> `mine.last_heartbeat_wall_time` | `a_heartbeat_rewrites_only_its_time_and_never_flushes`, `a_heartbeat_refreshes_the_lock_record_keeping_its_owner_and_never_flushes` |
| `held.rs` `heartbeat`: add `self.lock.sync_all()?;` after the write | both `..._never_flushes` tests |
| `held.rs` `rewrite_record`: drop the `beat` match | `a_rewrite_after_a_heartbeat_keeps_the_newest_time` |
| `copy.rs` loop: delete the per-chunk `beat()` | `the_copy_heartbeats_before_each_guarded_mutation_and_every_64_kib`, `a_heartbeat_failure_in_the_loop_removes_the_temporary_and_never_publishes` |
| `copy.rs` publish: delete the `beat()` before the guard | `a_heartbeat_failure_at_the_publish_removes_the_temporary` |
| `tree.rs` `copy_one`: drop `\|\| e.step == CopyStep::Heartbeat` | `a_heartbeat_failure_inside_a_file_aborts_the_whole_tree`, `a_tree_stops_at_a_failed_heartbeat_and_records_failed` |
| `tree.rs` dir creation: delete the `(cx.beat)()` line | `a_failed_heartbeat_aborts_the_whole_tree` |
| `session.rs` `beat`: drop `\|\| self.last.get().elapsed() < self.interval` | `a_heartbeat_waits_for_its_interval` |
| `session.rs` `beat`: drop `.or_else(\|_\| held.heartbeat(now))` | `a_failed_heartbeat_write_is_retried_once` |
| `session.rs` `beat_error`: keep `e.source` unwrapped | `two_failed_heartbeat_writes_stop_the_copy_naming_the_lock_and_the_run_records_failed` |
| `mod.rs` `fail`: delete the `pulse.failed()` early return | `a_torn_heartbeat_is_that_failure_never_a_lost_lock` |
| `session.rs` `guarded`: `why` always `LOST` | `a_temporary_kept_after_a_torn_heartbeat_names_the_heartbeat_not_another_run` |
| `mod.rs` `complete`: add `let _ = locked.pulse.beat(&locked.held);` first | `the_finish_never_heartbeats` |
| `restart.rs`: `checked(locked)?` -> `owned(&locked.held, &locked.lock_shown)?` at `:26` | `restart_heartbeats_during_its_sweep_before_the_copy`, `a_failed_heartbeat_during_restart_stops_the_run_at_the_lock_step_and_copies_nothing` |
| `main.rs` debug `heartbeat_interval`: always `HEARTBEAT_INTERVAL` | `a_running_copy_moves_its_lock_records_heartbeat` |
| `main.rs` debug `heartbeat_interval`: `unwrap_or(Duration::ZERO)` | `without_the_override_a_short_run_keeps_its_creation_heartbeat` |

3. **AGY-CAPSTONE** on `810d996..HEAD`, then **AGY-TEST-AUDIT**, each with its ledger row and marker.
