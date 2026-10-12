# Cut 9f-1, plan 1 of 2: the `flux-fs` primitives and the WAL module - Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build and test, in isolation, everything cut 9f-1 needs below the copy loop: five `flux-fs` handle primitives with their Unix, Windows and fake implementations, and a `wal` module in `flux-core` (per-chunk digests, the record format, two-pass replay, and the single writer thread with its flush policy). Nothing in this plan changes what `flux copy` or `--resume` does; plan 2 wires it in after this plan merges.

**Architecture:**
- `FileHandle` gains `duplicate` (an OS-level handle duplicate for `sync_all` on another thread) and `truncate_to` (set the length AND position the next write there). `DirHandle` gains `open_read` (a streaming, non-following read of a child) and `open_existing` (a non-following, identity-checked write open of an existing child, without truncation). Each has a `StdDir` Unix arm, a `StdDir` Windows arm and a `FakeDirHandle` model.
- `crates/flux-core/src/wal/` is new: `digest.rs` (a `ChunkHasher` that turns 256 KiB writes into 1 MiB BLAKE3 chunk digests), `record.rs` (encode/decode with checksum, torn-tail and corruption classification), `replay.rs` (pass 1 scans and indexes; pass 2 loads one file's state), `writer.rs` (one thread per operation behind a bounded channel; flushes on 1 s or 64 MiB; under Strict syncs the partial through its duplicate before it appends, then syncs the WAL).
- Rollout: Tasks 1-3 are additive trait methods (no caller); Tasks 4-7 are a new module with no caller. `main` stays shippable after every task.

**Tech Stack:** Rust 2024, `rust-version = "1.98.1"` (`Cargo.toml:10-11`); `std::sync::mpsc::sync_channel` and `std::thread::spawn` (no new crate); `rustix::fs::openat` on Unix and `NtCreateFile` on Windows exactly as the existing dir handles use them; `blake3 = "1.8"` (workspace, already a `flux-core` dependency); the `FaultFs` fake; nextest through `just`.

**Spec:** `docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md` (owner accepted Rulings 1 and 2 on 2026-10-12; panel 3 rounds, not green). Executors read it with this plan. Where the two differ the spec wins and the executor reports the conflict, except in the numbered "Plan rulings" below, which settle what the spec leaves to the plan or got wrong about the code (each such ruling is also written into the spec by the task that implements it).

**Code base:** written against `main` at `c652f8b` (2026-10-12); every citation below was read on that tree. Each task's Step 0 re-verifies its cited lines before the first edit.

## Global Constraints

- The gate is `just check` = `fmt-check clippy typos test` (`justfile:46`), where `test` is `cargo nextest run --workspace --no-tests=pass` then `cargo test --doc --workspace` (`justfile:12-14`). Run it before every commit; use `cargo nextest run --workspace --no-fail-fast` when classifying failures (a fail-fast summary of "5/9 run" means cancelled, not green). Push after every task and read CI on that commit: the Windows and macOS arms run only there.
- Spec constants, verbatim: `chunk_size = 1 MiB (1,048,576 bytes), fixed, not configurable` (V16:6349); `checkpoint flush interval = 1 second`, `checkpoint batch threshold = 64 MiB` (V16:7005-7006); `no fsync per 1 MiB chunk by default`, `no database transaction per chunk by default`, `no unbounded checkpoint queue` (V16:8482-8484); the WAL lives at `wal/000001` in the operation workspace, the directory "created only by the slice that writes a WAL file" (V16:8003-8011).
- Every new `DirHandle` or `FileHandle` method gets all three implementations: `StdDir` in `crates/flux-platform/src/dir_unix.rs`, `StdDir` in `dir_windows.rs`, `FakeDirHandle`/`FakeHandle` in `crates/flux-core/src/fault_fs.rs`, plus `StdFile` in `std_fs.rs`. The compiler is the oracle for completeness: build `--workspace --all-targets` on Linux, then read the Windows CI job; do not pin a count of touch points.
- Containment contract for every new open: judge the NAME, never follow a link. The precedent is `DirHandle::create_new`'s doc (`crates/flux-fs/src/fs.rs:258-263`: "MUST FAIL ... if the name is taken by anything, INCLUDING a link") and `read_file`'s refusals (`fs.rs:353-359`: name-surrogate is `Code::SafetyRejected`, a directory is `Code::DestinationError` with kind `IsADirectory`, missing is `Code::IoError` with kind `NotFound`).
- No `unwrap` on filesystem or channel results in non-test code; errors keep their `Code`.
- The WAL module is not wired to any caller in this plan: no change to `copy.rs`, `tree.rs`, `run/`, the CLI, or `flux_fs::Code`.

## Plan rulings (left to the plan by the spec, or where the spec's picture of the code was wrong; each is written into the spec by the task that implements it)

1. **Digests live in `flux-core`, not `flux-hash`.** The spec says "crate `flux-hash`"; that crate is a 3-line scaffold with no API (`crates/flux-hash/src/lib.rs`: "Scaffold only — no implementation yet") and nothing depends on it. `wal::digest` calls `blake3` directly, as `lock/record.rs:77` and `state.rs:123` already do. Task 4.
2. **Spec Interfaces (2) and (3) are one method.** `FileHandle::truncate_to(&mut self, len: u64)` sets the length to `len` and positions the next write at `len`; resume never needs the two apart, and one call keeps the fake trivial (its writes append to the stored bytes, `fault_fs.rs:664-680`). Extending (`len` beyond the current length) is refused with `Code::DestinationError`, kind `InvalidInput`: a hole of zeros is never trusted bytes. Task 1.
3. **There is no `FileSystem`/`DirHandle` conformance suite.** Only `ClaimStore` has one (`crates/flux-fs/src/claims.rs:232`, `run_all` at `:247`); the spec's "covered by the conformance suite the existing methods use" was wrong. The primitives are pinned where the existing methods are: `crates/flux-platform/tests/dir_handle.rs` (`mod posix` at `:2`, `mod windows_arm` at `:569`), `crates/flux-platform/tests/std_fs.rs`, and the `fault_fs.rs` test module. Tasks 1-3.
4. **Spec Interfaces (5) is `DirHandle::open_read`** with a new `type Reader: std::io::Read`. `DirHandle::read_file` (`fs.rs:359`) returns a `Vec<u8>` capped by `limit`, which cannot validate a multi-GiB partial. `open_read` keeps `read_file`'s refusals and its non-following open flags. Task 2.
5. **`Writer` gains `Send`.** The WAL thread owns the WAL file handle and the registered duplicates, so `type Writer: FileHandle + Sync` (`fs.rs:125`, `:227`) becomes `+ Sync + Send`; `wal::open` bounds the workspace handle with `D: DirHandle + Send + 'static` (a bound on the function, not on the trait). `StdFile(File)` and `FakeHandle` (an `Arc<Mutex<_>>` and plain data, `fault_fs.rs:631-640`) are both `Send` already. Task 1.
6. **The writer thread owns the flush trigger**, not the copy side. Spec decision 5 put the trigger on the copy side "because under Strict the data sync must come before the WAL append"; panel round 2 moved that sync onto the writer thread (decision 6), so the premise is gone, and a thread-side timer also flushes while the copy thread is blocked in a slow source read, which a copy-side trigger cannot. The timer runs from the FIRST pending record, so an idle writer never touches the file. Plan 2 may still send `Msg::Flush` at file completion. Task 7 amends spec decision 5.
7. **`open_existing` refusals.** A name-surrogate is `Code::SafetyRejected` (as `read_file`); a directory or other non-regular file, an identity that does not equal the one the caller passes, and a caller passing `FileIdentity::Unavailable` are all `Code::DestinationError` (kind `InvalidData` for the identity cases); missing is `Code::IoError`, kind `NotFound`. Task 3.
8. **Record encoding.** Fixed little-endian; the checksum is the first 16 bytes of BLAKE3 over every byte before it, the construction `lock/record.rs:78` and `:96` use; `MAGIC = b"FWAL"`, `FORMAT_VERSION: u16 = 1`, `CHECKSUM_VERSION: u8 = 1`, `COMMIT: u8 = 0xC3`. The header carries `operation_id` as length-prefixed bytes (V16:7507-7516 lists it per record). Task 5.
9. **Duplicate detection** keeps a map `(generation, sequence) -> checksum` (16 bytes per record): a re-seen pair with the same checksum is skipped (idempotent, V16:7934); a different checksum is `WAL_SEQUENCE_CONFLICT` (V16:7936-7939). Task 6.
10. **Pass 2 is sequential.** `load_file` takes `R: Read` and skips forward to each indexed offset; no `Seek` bound on `Reader`. The WAL is about 32 bytes per MiB copied, so a re-read per staged file is cheap. Task 6.
11. **Channel disconnect without `Stop` is abandonment**: the thread closes its duplicates and exits WITHOUT flushing (spec decision 9: after a lost lock "no NEW append or flush begins"). `Msg::Stop` and `Msg::Drain` flush first. Task 7.
12. **The sender's retry loop** calls the `Heartbeat` (`copy.rs:193`, `dyn Fn() -> flux_fs::Result<()>`) on every `Full` and sleeps `SEND_RETRY = 10 ms` between tries; a `beat()` error is returned as `SendError::Heartbeat(e)`; `Disconnected` is `SendError::WalFailed`. `CHANNEL_CAP = 64` messages. Task 7.
13. **The WAL module has its own `enum WalError`**; the taxonomy strings `WAL_CORRUPT`, `WAL_SEQUENCE_CONFLICT`, `WAL_FORMAT_UNSUPPORTED`, `WAL_WRITE_FAILED` and the `flux_fs::Code` variants that carry them are plan 2's (reporting is plan 2). Task 5.
15. **A declared length past the end of the stream is `TornTail`, even mid-file.** A flipped length field cannot be told from a torn last record. `scan` streams the segment through a sliding buffer: when `decode_next` answers `TornTail` and the stream is NOT exhausted, it reads more and retries; `TornTail` with the stream exhausted is the real torn tail, so memory is bounded by one record (a 64 MiB range is about 2 KiB), not by the segment. Everything from a true torn tail is truncated, including any valid records after a mid-file length corruption. The loss is redo of copied work, never trust of bad bytes, the direction V16:7043-7044 allows. Tasks 5 and 6.
17. **`wal::open` consumes a directory handle the caller obtains for it and keeps its own.** `DirHandle` has no `Clone` and no `duplicate` (`fs.rs:226`), and the thread must own the handle it creates `wal/` in, so plan 2 opens a SECOND handle to the workspace (`operations.open_dir(<id>)`, from `TreePlace.operations: Option<F::Dir>`, `run/place.rs:245-262`) and passes that; it reads the segment back for `load_file` through its own handle (`open_dir("wal")?.open_read("000001")`). The fake allows this (`destination_root` twice). Task 7.
18. **A record's `payload_len` is bounded: `MAX_PAYLOAD = 1 << 20`.** The largest legitimate payload is a `Range` of one flush (at most 65 digests, about 2 KiB, since the flush fires at 64 MiB) or a `FileBegin` holding two paths; a `payload_len` above the bound is `Corrupt`, never `TornTail`, so a flipped length cannot make the sliding window grow to the end of the segment. Task 5.
19. **Opening with a generation below the segment's last is refused** (`WalError::SequenceConflict { generation, sequence: 1 }`): appending `(g, 1)` after `(g' > g, n)` would be the regression the next scan rejects as `Corrupt`. Task 7.
20. **A WAL on a filesystem whose identity is `Unavailable` cannot be reopened**: `open` returns `WalError::Io` with kind `Unsupported` before touching the segment (ruling 7 refuses `open_existing` with `Unavailable`, and the control-plane file gets no weaker rule than a partial). Plan 2 maps that to "checkpoints unavailable on this filesystem: recopy", reported, never an abort. A fresh segment on such a filesystem is still written (no reopen is involved). Task 7.
21. **Failures cross the thread as a `Clone` record, not as `FsError`.** `FsError` is not `Clone` (`crates/flux-fs/src/error.rs:80` derives only `Debug` and `thiserror::Error`; its `source` is a `std::io::Error`), and its doc forbids rebuilding the source (`Error::other(..)` destroys `raw_os_error()`, `error.rs:83-88`). So the writer captures `pub(crate) struct WalFault { pub code: Code, pub kind: std::io::ErrorKind, pub raw_os_error: Option<i32>, pub message: String }` from the original error at the moment it happens (`code`, `source.kind()`, `source.raw_os_error()`, `source.to_string()`), and `Failure`, `Close`'s ack and `Drain`'s payload carry `WalFault`. Plan 2 reports from the record; nothing downstream needs the original `io::Error`. Task 7.
22. **Strict makes the new segment's directory entries durable** (added during the SDD run). Under `Strict`, the flush that CREATES the segment syncs the segment, then `wal/`, then the workspace directory, so an acknowledgement is true of the name as well as the bytes; a failure there is a WAL failure. A reopened segment issues no directory sync.
23. **Trailing damage is torn, not corrupt** (owner-approved 2026-10-12, agy-first aligned; V16 s160/s174/s191). A `Corrupt` or `Unsupported` record is a torn tail (truncated, never an error) when every byte from its offset to the end of the stream is 0x00 (a leading `MAGIC` is allowed), or when it starts with valid `MAGIC`, a supported version, a readable `operation_id_len` and `payload_len <= MAX_PAYLOAD` and its declared span ends exactly at the end of the stream (non-zero unknown versions never qualify by the span test). Anything else stays `WalError::Corrupt`/`Unsupported`; no forward scan for the next `MAGIC`. `Scan.discarded_tail` reports every cut (`ZeroFilled`, `SpanReachesEnd`, `TornRecord`).
16. **Sequence numbers restart at 1 in each generation.** V16 s161 (`:7569-7578`) numbers sequences "within an operation generation" and its example starts a generation at `seq 1`; the 9f-1 spec's decision 7.1 sentence "On resume the writer starts its sequence after the highest valid one" contradicts that and is amended to: "after the highest valid one WITHIN the current generation; a new generation (every `--resume` bumps it) starts at 1". Tasks 6 and 7 implement the V16 rule.
14. **`FileBegin.source_identity` and `file_id` are opaque bytes** to the WAL. Plan 2 decides the encodings (the claim key's `ClaimKey::encode()`, `crates/flux-fs/src/claims.rs:59`, and the identity string `FileFields.source_identity` uses, `state.rs:242`); the WAL stores and returns them unchanged. `source_mtime` is `Option<(i64, u32)>` (seconds, nanoseconds since the Unix epoch). Task 5.

## Review Focus

Failure modes the spec implies that no single mechanism obviously owns; each has a test in the named task.

1. A link planted at the partial's name between the crash and the resume: `open_existing` refuses it (a symlink as `SafetyRejected`; a junction on Windows as `DestinationError`/`IsADirectory`, because `FILE_NON_DIRECTORY_FILE` answers first) and never writes through it. Task 3.
2. A torn tail that stops ONE byte short (everything but the commit marker present) is `TornTail`, not `Corrupt`; the file is truncated at the previous record and replay continues. Task 5 (every prefix length) and Task 7 (truncation on open).
3. A `FILE_BEGIN` of a later generation whose `valid_through` is LOWER than ranges already recorded drops every digest at or past it; a stale higher range can never resurface. Task 6.
4. The writer thread is gone (stopped and joined, or dead) and the copy side sends: the send fails as `WalFailed` instead of blocking forever. Task 7.
5. The 1 s timer fires with nothing pending: no record, no `wal/` directory, no sync. Task 7.

---

### Task 1: `Writer: Send`, `FileHandle::duplicate`, `FileHandle::truncate_to`

**Files:**
- Modify: `crates/flux-fs/src/fs.rs:102-107` (`FileHandle`), `:125` and `:227` (the `Writer` bounds)
- Modify: `crates/flux-platform/src/std_fs.rs:30-56` (`StdFile`, its `FileHandle` impl)
- Modify: `crates/flux-core/src/fault_fs.rs:631-640` (`FakeHandle`), `:684-730` (its `FileHandle` impl)
- Test: `crates/flux-platform/tests/std_fs.rs`; `crates/flux-core/src/fault_fs.rs` (test module)
- Docs: the spec's Interfaces section (rulings 2 and 5)

**Interfaces:**
- Produces: `trait FileHandle: Write { fn sync_all(&self) -> Result<()>; fn identity(&self) -> Result<FileIdentity>; fn duplicate(&self) -> Result<Self> where Self: Sized; fn truncate_to(&mut self, len: u64) -> Result<()>; }`; `type Writer: FileHandle + Sync + Send` on both `FileSystem` and `DirHandle`.
- `duplicate`: an OS-level duplicate (`File::try_clone`: `dup` on POSIX, `DuplicateHandle` on Windows) sharing the open file description; `sync_all` on it syncs the same file. `truncate_to(len)`: the file is `len` bytes long afterwards and the next `write` lands at `len`; `len` greater than the current length is `Code::DestinationError`, kind `InvalidInput`.

- [ ] **Step 0: State verification.** Open the cited lines and confirm: `FileHandle` has exactly `sync_all` and `identity` (`fs.rs:102-107`); both `Writer` bounds read `FileHandle + Sync` (`:125`, `:227`); `StdFile(File)` at `std_fs.rs:30` with `sync_all` at `:42-43`; `FakeHandle` has the six fields `path, buf, read_pos, sink, sync_fault, write_fault` (`fault_fs.rs:631-640`) and its `write` appends to `g.files[path]` (`:664-680`). If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests.** In `crates/flux-platform/tests/std_fs.rs` (real filesystem under a `tempfile` dir, the file's existing pattern), with `StdFileSystem.create_new(&path)` as the writer source:
  - `a_duplicate_syncs_the_same_file_from_another_thread`: write `b"hello"`, `let d = w.duplicate().unwrap()`, inside `std::thread::scope` move `d` to a thread that calls `d.sync_all().unwrap()`; after the join write `b"!"` through `w`; `std::fs::read(&path) == b"hello!"`.
  - `truncate_to_cuts_the_tail_and_positions_the_next_write`: write `b"hello world"`, `w.truncate_to(5)`, write `b"!"`; file reads `b"hello!"`.
  - `truncate_to_refuses_to_extend`: write `b"hello"`; `w.truncate_to(100)` is `Err` with `code == Code::DestinationError` and `source.kind() == InvalidInput`; the file still reads `b"hello"`.
  In `fault_fs.rs`'s test module the same three against the fake (`FaultFs::new()`, `fs.create_new(Path::new("/p"))`, read back with `fs.read_file("/p")`), plus `the_fake_records_duplicate_and_truncate_to`: after the calls, `fs.calls()` contains `"duplicate(/p)"` and `"truncate_to(/p, 5)"` in that order.

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-platform -p flux-core --no-fail-fast duplicate truncate_to`. Expected: compile errors naming `duplicate` and `truncate_to` (no such method).

- [ ] **Step 3: Add the trait methods and the `Send` bound** in `fs.rs` with doc comments stating the contracts in Interfaces, including the extend refusal and "never follows anything: this is a handle".

- [ ] **Step 4: Implement `StdFile`.** `duplicate`: `self.0.try_clone().map(StdFile).map_err(FsError::from_io)`. `truncate_to`: `self.0.metadata()?.len()`, refuse if `len > current` with `FsError::new(Code::DestinationError, io::Error::new(InvalidInput, "truncate_to cannot extend"))`, then `set_len(len)` and `Seek::seek(SeekFrom::Start(len))`, each `map_err(FsError::from_io)`.

- [ ] **Step 5: Implement `FakeHandle`.** `duplicate`: a new `FakeHandle` with the same `path`, `sink`, `sync_fault` and `write_fault` (the `Arc`s cloned, so an injected `sync_all` fault reaches whichever handle syncs first), empty `buf`, `read_pos: 0`; records `duplicate(<path>)` through the sink's `calls` (a handle with no sink is a reader: `Code::DestinationError`). `truncate_to`: no sink is `DestinationError`; otherwise lock the sink, refuse `len > files[path].len()` as in Step 4, else `truncate(len)`; record `truncate_to(<path>, <len>)`. Note the `Drop` impl at `:644` records `drop_writer` for a handle with a sink: the duplicate has one, so dropping it logs a second `drop_writer(<path>)`; that is intended (Task 7 asserts on it).

- [ ] **Step 6: Build the workspace and run the tests.** `cargo build --workspace --all-targets` (the `Send` bound is checked here), then `cargo nextest run -p flux-platform -p flux-core --no-fail-fast duplicate truncate_to`. Expected: all pass. Then `just check`.

- [ ] **Step 7: Write rulings 2 and 5 into the spec's Interfaces section** (replace items (2) and (3) with `truncate_to`, add the `Send` bound), one sentence each, citing this plan.

- [ ] **Step 8: Commit and push; read CI** (the Windows arm exercises `try_clone` through `DuplicateHandle`).

```bash
git add crates/flux-fs/src/fs.rs crates/flux-platform/src/std_fs.rs crates/flux-platform/tests/std_fs.rs crates/flux-core/src/fault_fs.rs docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md
git commit -m "feat(fs): FileHandle::duplicate and truncate_to; Writer is Send (cut 9f-1 plan 1, Task 1)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 2: `DirHandle::open_read`

**Files:**
- Modify: `crates/flux-fs/src/fs.rs:226-233` (associated types), after `:359` (`read_file`)
- Modify: `crates/flux-platform/src/std_fs.rs:21` (`StdReader`), the `std_file_from` constructor's neighbourhood (a sibling `std_reader_from`)
- Modify: `crates/flux-platform/src/dir_unix.rs:68-69` (impl header, `type Writer`), `:333-352` (`read_file`, the flags and the `map_err` arm to reuse)
- Modify: `crates/flux-platform/src/dir_windows.rs:114` (impl header), `:330` (`read_file`), `:465` (`read_file_at`, whose open half becomes `open_read_at`)
- Modify: `crates/flux-core/src/fault_fs.rs:1588-1591` (`FakeDirHandle` impl header), its `read_file` arm (Step 0 locates it), `:1222` (`FaultFs::open_read`, reused)
- Test: `crates/flux-platform/tests/dir_handle.rs` (`mod posix` from `:2`, `mod windows_arm` from `:569`); `fault_fs.rs` test module
- Docs: the spec's Interfaces item (5) (ruling 4)

**Interfaces:**
- Produces: on `DirHandle`, `type Reader: std::io::Read;` and `fn open_read(&self, name: &OsStr) -> Result<Self::Reader>;`. `StdDir::Reader = crate::StdReader` on both platforms; `FakeDirHandle::Reader = FakeHandle`.
- Refusals exactly as `read_file` (`fs.rs:353-358`): name-surrogate `SafetyRejected`; directory `DestinationError` kind `IsADirectory`; other non-regular where the platform can tell `DestinationError`; missing `IoError` kind `NotFound`. Never follows a link.

- [ ] **Step 0: State verification.** Confirm `DirHandle` has no `Reader` type (`fs.rs:226-233` lists `Writer`, `Lock`, `Claims`); `read_file` doc and signature at `:353-359`; Unix `read_file` opens with `OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::NOCTTY | OFlags::CLOEXEC` (`dir_unix.rs:342`) and maps the open error at `:345`; Windows `read_file` at `dir_windows.rs:330` delegates to `read_file_at` (`:465`); `FaultFs::open_read` at `fault_fs.rs:1222`. Locate `FakeDirHandle`'s `read_file` and note its refusal arms (a link, a directory, missing). If any cited line differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests.** `dir_handle.rs`, `mod posix`: `open_read_streams_a_child_file` (create `f` with 300,000 bytes through `create_new`, `let mut r = dir.open_read(OsStr::new("f"))`, `read_to_end` equals the bytes); `open_read_refuses_a_symlink_as_safety_rejected` (`std::os::unix::fs::symlink("f", dir/"l")`, `open_read("l")` is `Err` with `code == SafetyRejected`); `open_read_refuses_a_directory` (`create_dir("d")`, `open_read("d")` is `DestinationError` with kind `IsADirectory`); `open_read_of_a_missing_name_is_not_found`. `mod windows_arm`: the same four, except the surrogate case: a junction is the only name-surrogate CI can plant (a file symlink needs a privilege the runner lacks, `dir_windows.rs:638-640`), and `FILE_NON_DIRECTORY_FILE` answers `STATUS_FILE_IS_A_DIRECTORY` BEFORE the reparse-tag check (`read_file_at`, `:494-502`), so the assertion is: `Err` whose code is `DestinationError` with kind `IsADirectory` (a symlink, if the runner could make one, would be `SafetyRejected`), and the junction's target directory is untouched. `fault_fs.rs`: `the_fake_dir_open_read_streams_and_refuses_a_directory` (both arms in one test) and `the_fake_dir_open_read_records_the_call` (`fs.calls()` contains an `open_read(` entry for the child path, which `FaultFs::open_read` already records).

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-platform -p flux-core --no-fail-fast open_read`. Expected: compile errors, no `open_read` on the dir handle.

- [ ] **Step 3: Add `type Reader` and `open_read` to `DirHandle`** with the doc in Interfaces.

- [ ] **Step 4: Unix.** `type Reader = crate::StdReader;`. `open_read`: `check_component(name)?`, `openat` with `read_file`'s flags (`:342`) and `Mode::empty()`, the same `map_err` arm as `:345`, then `rustix::fs::fstat` and `FileType`: a directory is `DestinationError`/`IsADirectory`, any other non-regular is `DestinationError`; return `crate::std_fs::std_reader_from(File::from(fd))`. Add `pub fn std_reader_from(f: File) -> StdReader` next to `std_file_from`.

- [ ] **Step 5: Windows.** Split `read_file_at` (`:465`): the `NtCreateFile` open plus the `reparse_tag_of`/`is_name_surrogate` refusal (the pattern at `:225-240`) becomes `fn open_read_at(p: &OwnedHandle, n: &OsStr) -> Result<OwnedHandle>`, with access `FILE_GENERIC_READ | SYNCHRONIZE`, disposition `FILE_OPEN`, flags `FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT`; `read_file_at` calls it and keeps its read-with-limit half unchanged. `open_read` wraps the handle in `std_reader_from(File::from(handle))`.

- [ ] **Step 6: Fake.** `type Reader = FakeHandle;`. `open_read`: `check_component(name)?`, the child path, the refusal arms Step 0 found in the fake's `read_file` (link -> `SafetyRejected`, directory -> `DestinationError`/`IsADirectory`), then `self.fs().open_read(&child_path)`.

- [ ] **Step 7: Build, run, gate.** `cargo build --workspace --all-targets`; `cargo nextest run -p flux-platform -p flux-core --no-fail-fast open_read`; `just check`. Expected: pass.

- [ ] **Step 8: Write ruling 4 into the spec's Interfaces item (5).**

- [ ] **Step 9: Commit and push; read CI** (the junction test runs only on the Windows arm).

```bash
git commit -am "feat(fs): DirHandle::open_read, a streaming non-following read of a child (cut 9f-1 plan 1, Task 2)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 3: `DirHandle::open_existing`

**Files:**
- Modify: `crates/flux-fs/src/fs.rs` after `:264` (`create_new`)
- Modify: `crates/flux-platform/src/dir_unix.rs:99-110` (`create_new`, the sibling), `:26-37` (`classify`, for the regular-file test's idiom)
- Modify: `crates/flux-platform/src/dir_windows.rs:253-255` (`create_new`), `:605-650` (`create_new_at`, the sibling: access `:626`, share `:631`, disposition `:632`, flags `:647`), `:225-240` (the surrogate refusal to copy)
- Modify: `crates/flux-core/src/fault_fs.rs:1683-1687` (`FakeDirHandle::create_new`), `:1241-1258` (`FaultFs::create_new`, how a sink handle is built), `:723` (`FakeHandle::identity`)
- Test: `crates/flux-platform/tests/dir_handle.rs` (both modules); `fault_fs.rs` test module
- Docs: the spec's Interfaces item (1) (ruling 7)

**Interfaces:**
- Produces: `fn open_existing(&self, name: &OsStr, expected: &FileIdentity) -> Result<Self::Writer>;` on `DirHandle`. Opens an existing child REGULAR FILE for writing, no truncation, positioned at 0 (the caller then calls `truncate_to`). Refusals per ruling 7. The returned handle's `identity()` equals `expected`.

- [ ] **Step 0: State verification.** Confirm `create_new` at `fs.rs:264` with its doc at `:258-263`; Unix `create_new` flags `OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC` (`dir_unix.rs:104`) returning `std_file_from(File::from(fd))` (`:108`); Windows `create_new_at` access `FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES | SYNCHRONIZE` (`:626`), share `FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE` (`:631`), disposition `FILE_CREATE` (`:632`), flags `FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT` (`:647`); `FILE_OPEN` is imported (`:21`); the surrogate refusal at `:225-240`; the fake's `create_new` inserts `Vec::new()` at `fault_fs.rs:1258` and builds the sink handle nearby. If any differs, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests.** `mod posix`: `open_existing_appends_after_the_prefix` (create `f` with `b"abc"`, capture `id = w.identity()`, drop `w`; `let mut w2 = dir.open_existing("f", &id)`, `w2.truncate_to(2)`, write `b"Z"`, `dir.read_file("f", 100) == b"abZ"`); `open_existing_refuses_a_planted_symlink` (create `f`, capture `id`, remove `f`, `symlink("elsewhere", dir/"f")`: `open_existing("f", &id)` is `SafetyRejected`, and the link's target was not created); `open_existing_refuses_an_identity_mismatch` (create `f`, capture `id`, `remove_file("f")`, create `f` again: `open_existing("f", &id)` is `DestinationError` with kind `InvalidData`); `open_existing_refuses_an_unavailable_expectation` (`open_existing("f", &FileIdentity::Unavailable)` is `DestinationError` without opening); `open_existing_of_a_missing_name_is_not_found`; `open_existing_refuses_a_directory`. `mod windows_arm`: the same six; the planted-surrogate case uses a junction and asserts as Task 2's Windows arm does (`DestinationError`/`IsADirectory` from `FILE_NON_DIRECTORY_FILE`, nothing created inside the junction's target). `fault_fs.rs`: the same six against the fake, plus `fs.calls()` containing `open_existing(/d/f)`.

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-platform -p flux-core --no-fail-fast open_existing`. Expected: compile errors.

- [ ] **Step 3: Add `open_existing` to `DirHandle`** with its doc: the containment sentence from `create_new`'s doc, the regular-file requirement, the identity contract, and ruling 7's codes.

- [ ] **Step 4: Unix.** `check_component(name)?`; if `expected` is `Unavailable` return `DestinationError`/`InvalidData` before any syscall; `openat(&self.0, name, OFlags::WRONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())`, mapping `ELOOP` to `SafetyRejected` and `ENOENT` to `NotFound` (the arm at `:345` is the model); `fstat`: directory -> `DestinationError`/`IsADirectory`, other non-regular -> `DestinationError`; wrap with `std_file_from`; `if handle.identity()? != *expected` return `DestinationError`/`InvalidData` ("identity mismatch"). The handle is dropped on every refusal path.

- [ ] **Step 5: Windows.** `fn open_existing_at(p: &OwnedHandle, n: &OsStr) -> Result<crate::StdFile>`: `create_new_at`'s access, share and flags verbatim, disposition `FILE_OPEN`; after the open, `reparse_tag_of` + `is_name_surrogate` -> `SafetyRejected` exactly as `:225-240` (drop the handle on the reject path, as that comment explains); map the status exactly as `read_file_at` does at `:494-507` (`STATUS_FILE_IS_A_DIRECTORY` first, to `DestinationError`/`IsADirectory`; `STATUS_OBJECT_NAME_NOT_FOUND | STATUS_OBJECT_PATH_NOT_FOUND` to `Code::IoError`; `STATUS_ACCESS_DENIED` to `PermissionDenied`), copied, not shared. `open_existing` then applies the `Unavailable` and identity checks as Step 4.

- [ ] **Step 6: Fake.** `check_component`, the child path; `Unavailable` -> `DestinationError`; a link (the fake's link model, from Task 2 Step 0) -> `SafetyRejected`; a directory -> `DestinationError`/`IsADirectory`; absent from `files` -> `NotFound`; otherwise build a sink handle for the path the way `FaultFs::create_new` does (without touching the stored bytes), record `open_existing(<path>)`, and compare `handle.identity()?` with `expected` as Step 4.

- [ ] **Step 7: Build, run, gate.** `cargo build --workspace --all-targets`; `cargo nextest run -p flux-platform -p flux-core --no-fail-fast open_existing`; `just check`.

- [ ] **Step 8: Write ruling 7 into the spec's Interfaces item (1).**

- [ ] **Step 9: Commit and push; read CI.**

```bash
git commit -am "feat(fs): DirHandle::open_existing, a non-following identity-checked write open (cut 9f-1 plan 1, Task 3)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 4: `wal::digest::ChunkHasher`

**Files:**
- Create: `crates/flux-core/src/wal/mod.rs`, `crates/flux-core/src/wal/digest.rs`
- Modify: `crates/flux-core/src/lib.rs` (add `pub(crate) mod wal;`; Step 0 finds the module list)
- Test: inline in `digest.rs`
- Docs: the spec's decision 3 (ruling 1)

**Interfaces:**
- Produces, in `wal/mod.rs`: `pub(crate) const CHUNK_SIZE: u64 = 1 << 20;`, `pub(crate) type Digest = [u8; 32];`, `pub(crate) mod digest; pub(crate) mod record; pub(crate) mod replay; pub(crate) mod writer;` (the last three added by their tasks).
- Produces, in `digest.rs`: `pub(crate) struct ChunkHasher`, `fn new() -> Self`, `fn update(&mut self, bytes: &[u8]) -> Vec<(u64, Digest)>` (the `(chunk_index, digest)` pairs COMPLETED by this call, in order; a call may complete zero, one or several), `fn finish(self) -> Option<(u64, Digest)>` (the final short chunk if any bytes are pending), `fn bytes_seen(&self) -> u64`. Digest = `blake3::hash` of exactly the chunk's bytes.

- [ ] **Step 0: State verification.** Confirm `crates/flux-core/src/lib.rs` has no `wal` module and `crates/flux-core/Cargo.toml:14` has `blake3 = { workspace = true }`. If not, STOP and report `STATE_MISMATCH: <what>`.

- [ ] **Step 1: Write the failing tests** in `digest.rs`: `one_mib_yields_one_digest_equal_to_blake3_of_it`; `four_256_kib_updates_complete_a_chunk_on_the_fourth` (first three return empty, the fourth returns `[(0, _)]`, `finish()` is `None`); `a_short_tail_is_finished_separately` (1 MiB + 10 bytes: `update` returns chunk 0, `finish()` returns `Some((1, blake3(tail)))`); `an_exact_multiple_has_no_tail`; `an_update_spanning_a_boundary_splits_at_it` (one 1.5 MiB update returns `[(0, blake3(first MiB))]` and `finish()` hashes the remaining 0.5 MiB); `two_chunks_in_one_update` (2 MiB + 1 returns two pairs).

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast wal::digest`. Expected: compile error, no module.

- [ ] **Step 3: Implement.** `ChunkHasher { hasher: blake3::Hasher, in_chunk: u64, index: u64 }`; `update` feeds `bytes` in slices of at most `CHUNK_SIZE - in_chunk`, emitting `(index, hasher.finalize())` and resetting at each boundary.

- [ ] **Step 4: Run, gate.** `cargo nextest run -p flux-core --no-fail-fast wal::digest`; `just check`. Expected: pass.

- [ ] **Step 5: Write ruling 1 into the spec's decision 3** (replace "crate `flux-hash`" with `wal::digest` and the reason).

- [ ] **Step 6: Commit and push.**

```bash
git add crates/flux-core/src/wal crates/flux-core/src/lib.rs docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md
git commit -m "feat(wal): ChunkHasher, 1 MiB BLAKE3 chunk digests from arbitrary writes (cut 9f-1 plan 1, Task 4)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 5: `wal::record`, the encoding and its corruption classes

**Files:**
- Create: `crates/flux-core/src/wal/record.rs`
- Modify: `crates/flux-core/src/wal/mod.rs` (the `mod record;` line, the constants of ruling 8)
- Test: inline in `record.rs`; `crates/flux-core/src/lock/record.rs:180-200` is the mutation-case idiom to copy

**Interfaces:**
- Produces: `pub(crate) struct Header { pub operation_id: Vec<u8>, pub generation: u64, pub sequence: u64 }`; `pub(crate) struct FileBegin { pub file_id: Vec<u8>, pub attempt_id: u64, pub destination: Vec<u8>, pub source_len: u64, pub source_mtime: Option<(i64, u32)>, pub source_identity: Vec<u8>, pub chunk_size: u64, pub valid_through: u64 }`; `pub(crate) struct Range { pub file_id: Vec<u8>, pub attempt_id: u64, pub range_start: u64, pub range_end: u64, pub digests: Vec<Digest> }`; `pub(crate) enum Body { FileBegin(FileBegin), Range(Range) }`; `pub(crate) fn encode(h: &Header, b: &Body) -> Vec<u8>`; `pub(crate) enum Decoded { Record { header: Header, body: Body, len: usize, checksum: [u8; 16] }, End, TornTail, Corrupt(String), Unsupported { format: u16, checksum: u8 } }`; `pub(crate) fn decode_next(bytes: &[u8]) -> Decoded`.
- Byte layout (little-endian): `MAGIC(4) FORMAT_VERSION(u16) CHECKSUM_VERSION(u8) record_type(u8) operation_id_len(u16) operation_id generation(u64) sequence(u64) payload_len(u32) payload checksum(16) COMMIT(1)`; `record_type` 1 = FileBegin, 2 = Range. Payloads: every `Vec<u8>` is `u32` length-prefixed; `source_mtime` is one byte `0` when `None`, or the byte `1` followed by `i64` and `u32` (13 bytes) when `Some`; `digests` is a `u32` count then 32 bytes each. The fixed header prefix is 10 bytes (`MAGIC` through `operation_id_len`), then `operation_id`, then 20 bytes (`generation`, `sequence`, `payload_len`), then `payload`, then 17 bytes (checksum and `COMMIT`): `decode_next` returns `TornTail` as soon as the input is shorter than the next field it must read (fewer than 10 bytes; fewer than `10 + operation_id_len + 20`; fewer than the full record).
- `decode_next`: empty input is `End`; fewer bytes than the lengths announce is `TornTail`; bad magic, a checksum mismatch, a wrong commit byte with the full length present, an unknown `record_type`, or a `Range` breaking its invariant (`range_start % CHUNK_SIZE != 0`, `range_end <= range_start`, `digests.len() as u64 != (range_end - range_start).div_ceil(CHUNK_SIZE)`) is `Corrupt(why)`; a `payload_len` above `MAX_PAYLOAD = 1 << 20` is `Corrupt` the moment the field is read, before any `TornTail` judgement (ruling 18); a `FORMAT_VERSION` or `CHECKSUM_VERSION` other than 1 is `Unsupported` (checked before the checksum, since the checksum rule itself is versioned).

- [ ] **Step 0: State verification.** Confirm `lock/record.rs:78` and `:96` compute `blake3::hash(..).as_bytes()[..16]` (the checksum construction to copy) and that `wal/mod.rs` exists from Task 4. If not, STOP and report `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests** in `record.rs`: `a_file_begin_round_trips` and `a_range_round_trips` (encode, `decode_next` is `Record` with equal fields and `len == bytes.len()`); `two_records_decode_in_sequence` (concatenate, decode twice, then `End`); `every_shorter_prefix_is_a_torn_tail` (for `n in 0..bytes.len()`: `decode_next(&bytes[..n])` is `TornTail`, except `n == 0` which is `End`); `every_single_byte_flip_is_never_a_record` (for each position, flip one bit: positions 4-6 may yield `Unsupported`; the two length fields may yield `TornTail` when the flipped length exceeds the bytes present (ruling 15); every other position yields `Corrupt`; no position yields `Record`); `a_range_with_the_wrong_digest_count_is_corrupt`; `an_unaligned_range_start_is_corrupt`; `an_empty_range_is_corrupt`; `an_unknown_record_type_is_corrupt`; `a_payload_length_above_the_bound_is_corrupt_not_torn` (a 30-byte input whose `payload_len` reads `0x7FFF_FFFF` is `Corrupt`, ruling 18).

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast wal::record`. Expected: compile error.

- [ ] **Step 3: Implement `encode` and `decode_next`** per Interfaces. Decoding reads each length field only after checking the bytes for it are present (`TornTail` otherwise); the checksum covers `bytes[..checksum_at]`.

- [ ] **Step 4: Run, gate.** `cargo nextest run -p flux-core --no-fail-fast wal::record`; `just check`.

- [ ] **Step 5: Commit and push.**

```bash
git add crates/flux-core/src/wal
git commit -m "feat(wal): record encoding with BLAKE3-16 checksum, torn-tail and corruption classes (cut 9f-1 plan 1, Task 5)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 6: `wal::replay`, two passes

**Files:**
- Create: `crates/flux-core/src/wal/replay.rs`
- Modify: `crates/flux-core/src/wal/mod.rs`
- Test: inline in `replay.rs` (streams built with Task 5's `encode`)

**Interfaces:**
- Consumes: `record::{Header, Body, FileBegin, Range, Decoded, encode, decode_next}`; `wal::{CHUNK_SIZE, Digest}`.
- Produces: `pub(crate) struct Scan { pub per_file: HashMap<Vec<u8>, Vec<u64>>, pub last: Option<(u64, u64)>, pub valid_len: u64 }` (`per_file`: file_id -> the byte offsets of that file's records in stream order; `last`: the highest `(generation, sequence)` seen; `valid_len`: bytes up to and including the last valid record); `pub(crate) enum WalError { Corrupt { at: u64, why: String }, SequenceConflict { generation: u64, sequence: u64 }, Unsupported { format: u16, checksum: u8 }, Io(FsError) }`; `pub(crate) fn scan<R: Read>(reader: R, operation_id: &[u8]) -> Result<Scan, WalError>`; `pub(crate) struct FileState { pub begin: FileBegin, pub digests: BTreeMap<u64, Digest> }` (chunk index -> digest, the effective set); `pub(crate) fn load_file<R: Read>(reader: R, scan: &Scan, file_id: &[u8]) -> Result<Option<FileState>, WalError>`.
- `scan` rules: a `TornTail` ends the scan (not an error; `valid_len` excludes it); `Corrupt` before the physical end is `WalError::Corrupt`; `Unsupported` is `WalError::Unsupported`; a header `operation_id` other than the argument is `Corrupt`; sequence order: the first record is `(g, 1)` for some `g`; each next record is `(g, s + 1)` or `(g' > g, 1)`; a pair already in `seen` is a duplicate: same checksum -> skipped (its bytes still count toward `valid_len`), different -> `SequenceConflict`; a pair at or below `last` that is NOT in `seen`, or any other order, is `Corrupt("sequence regression")`.
- `load_file` rules (apply the file's records in offset order): `FileBegin` -> `begin = this`, retain only `digests` with `index * CHUNK_SIZE < valid_through`; `Range` -> insert each digest at `range_start / CHUNK_SIZE + i`. A file with no `FileBegin` yet is skipped until it has one; no records at all -> `Ok(None)`.

- [ ] **Step 0: State verification.** Confirm Task 5's `Decoded` variants and that `std_fs.rs`'s `StdReader` implements only `Read` (so ruling 10 holds). If not, STOP and report `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests** in `replay.rs` (helper: `fn stream(records: &[(u64, u64, Body)]) -> Vec<u8>` with a fixed operation id): `scan_indexes_offsets_per_file_and_tracks_last` (begin A, range A, begin B: `per_file[A] == [0, len0]`, `per_file[B] == [len0 + len1]`, `last == Some((1, 3))`, `valid_len == total`); `a_torn_tail_ends_the_scan_at_the_last_valid_record` (append a prefix of a fourth record; `valid_len` equals the three-record length; `last == (1,3)`); `a_record_larger_than_the_read_window_scans` (a `Range` with a 70,000-byte `destination`-free payload built from 2,000 digests, larger than one 64 KiB read, decodes; proves the retry-on-`TornTail` loop); `a_corrupt_middle_record_is_wal_corrupt` (flip a byte in record 2 of 3); `an_identical_duplicate_is_idempotent` (record 2 appended twice: `per_file` lists it once, `valid_len` counts both); `a_conflicting_duplicate_is_sequence_conflict` (same `(1, 2)`, different payload); `a_generation_bump_restarts_the_sequence` (`(1,1) (1,2) (2,1)` scans; `(1,1) (1,2) (2,2)` is `Corrupt`); `a_sequence_regression_is_corrupt` (`(1,1) (1,3)`, and `(1,1) (1,2) (1,3) (1,1)'` where the re-sent `(1,1)'` has the same bytes as the first: that one IS skipped as a duplicate; but `(1,2) (1,3) (1,1)` with no earlier `(1,1)` is `Corrupt`); `a_foreign_operation_id_is_corrupt`; `load_file_merges_ranges_into_chunk_digests` (begin, range 0..2 MiB, range 2..3 MiB: indices 0,1,2); `a_later_file_begin_drops_digests_at_or_past_valid_through` (`FileBegin { valid_through: 0 }`, range 0..4 MiB, then `FileBegin { valid_through: 2 MiB }`: indices 0 and 1 remain, 2 and 3 are gone; a following range 2..3 MiB re-adds 2); `load_file_of_an_unknown_file_is_none`.

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast wal::replay`. Expected: compile error.

- [ ] **Step 3: Implement `scan`**: a `Vec<u8>` window filled by 64 KiB reads; `decode_next` on the window; on `Record` advance the absolute offset by `len` and `drain(..len)`; on `TornTail` read more and retry, and only when the read returns 0 bytes treat it as the torn tail (ruling 15); `seen: HashMap<(u64, u64), [u8; 16]>` for ruling 9. **Implement `load_file`**: iterate `scan.per_file[file_id]` in order, skipping forward on the reader by `io::copy(&mut reader.by_ref().take(gap), &mut io::sink())`, decode one record at each offset (a decode that is not `Record` here is `Corrupt`: the scan already validated it).

- [ ] **Step 4: Run, gate.** `cargo nextest run -p flux-core --no-fail-fast wal::replay`; `just check`.

- [ ] **Step 5: Commit and push.**

```bash
git add crates/flux-core/src/wal
git commit -m "feat(wal): two-pass replay, scan index and per-file state with FILE_BEGIN reset (cut 9f-1 plan 1, Task 6)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```

---

### Task 7: `wal::writer`, the thread, the channel and the flush policy

**Files:**
- Create: `crates/flux-core/src/wal/writer.rs`
- Modify: `crates/flux-core/src/wal/mod.rs`
- Test: inline in `writer.rs`, against `FaultFs` (`FaultFs::new()`, `fs.create_dir("/ws")`, `fs.destination_root(Path::new("/ws"))` gives the `FakeDirHandle` that plays the workspace; `fs.calls()` is the order oracle, `fs.read_file("/ws/wal/000001")` the content oracle)
- Docs: the spec's decision 5 (ruling 6), decision 9 (ruling 11), decision 2 (ruling 12)

**Interfaces:**
- Consumes: Task 1 (`duplicate`, `truncate_to`, `Writer: Send`), Task 2 (`open_read` for the scan on reopen), Task 3 (`open_existing` to truncate a torn tail), Task 5 (`encode`, `Header`, `Body`), Task 6 (`scan`, `Scan`, `WalError`); `crate::copy::Heartbeat` (`copy.rs:193`); `flux_fs::Durability`.
- Produces: `pub(crate) struct WalConfig { pub flush_interval: Duration, pub flush_bytes: u64, pub channel_cap: usize, pub durability: Durability }` with `pub(crate) const DEFAULT: WalConfig` = `1 s`, `64 << 20`, `64`, `Durability::Normal`; `pub(crate) const DIR_NAME: &str = "wal"; pub(crate) const SEGMENT_NAME: &str = "000001"; pub(crate) const SEND_RETRY: Duration = 10 ms`.
- `pub(crate) enum Msg<W> { FileBegin { begin: FileBegin, dup: Option<W> }, Range(Range), Flush, Close { file_id: Vec<u8>, ack: Option<std::sync::mpsc::SyncSender<Option<WalFault>>> }, Drain(std::sync::mpsc::SyncSender<Failure>), Stop }`. `Close`'s ack, when present, answers AFTER the flush `Close` triggers: that file's entry in `Failure.files` if present, else `Failure.wal` if the log has failed, else `None` (the rule in Thread rules); `Drain`'s receives a clone of the whole `Failure` after its flush. A send on an ack whose receiver is gone is ignored, never a panic.
- `pub(crate) struct WalFault { pub code: Code, pub kind: std::io::ErrorKind, pub raw_os_error: Option<i32>, pub message: String }` (`Clone`, `Debug`, `PartialEq`), with `fn from_error(e: &FsError) -> WalFault` and `fn new(code: Code, kind: std::io::ErrorKind, message: &str) -> WalFault` (`raw_os_error: None`) for the synthetic "no duplicate registered" fault (ruling 21). `pub(crate) struct Failure { pub wal: Option<WalFault>, pub files: HashMap<Vec<u8>, WalFault> }` (`Clone`, `Default`): `wal` is the first WAL error (later ones are not recorded); `files` holds, per file, the first data-sync (or no-duplicate) error for it, so two files failing in one flush are both reported. `fn is_clean(&self) -> bool`.
- `pub(crate) struct WalSender<W> { .. }`: `fn send(&self, msg: Msg<W>, beat: &Heartbeat<'_>) -> Result<(), SendError>` (ruling 12); `pub(crate) enum SendError { WalFailed, Heartbeat(FsError) }`: a `beat()` error during the retry loop is returned as `Heartbeat(e)` unchanged, so the caller (plan 2) can take the heartbeat-failure branch it already has; `WalFailed` is the channel being `Disconnected`. `fn failure(&self) -> Failure`.
- `pub(crate) struct WalThread { .. }`: `fn join(self) -> Failure` (the final state after `Stop`'s flush; a caller that ignores it has been told).
- `pub(crate) fn open<D: DirHandle + Send + 'static>(workspace: D, operation_id: Vec<u8>, generation: u64, cfg: WalConfig) -> Result<(WalSender<D::Writer>, WalThread, Option<Scan>), WalError>`. `workspace` is a handle opened FOR the WAL (ruling 17); the thread owns it. If `wal/000001` exists: `metadata("000001")` first, and an `Unavailable` identity is `WalError::Io` with kind `Unsupported` (ruling 20); `scan` it through `open_read` (returned as `Some`); `open_existing` it with that identity and `truncate_to(scan.valid_len)`; the starting sequence comes from `scan.last`: `None` -> `(generation, 1)`; `Some((g, s))` with `generation == g` -> `s + 1`; `generation > g` -> `1`; `generation < g` -> `Err(WalError::SequenceConflict { generation, sequence: 1 })` (ruling 19). If the segment is absent, nothing is created yet (`None`). Spawns the thread.
- Thread rules: pending records accumulate in order; `pending_bytes` sums `range_end - range_start`. A flush happens when `pending_bytes >= flush_bytes`, when `flush_interval` has elapsed since the FIRST pending record, on `Flush`, `Drain`, `Stop`. A flush with nothing pending does nothing (no file, no directory, no sync). The first flush with a record creates `wal/` (via `create_dir`; `AlreadyExists` is fine) and `000001` (via `create_new`). Under `Strict`, before appending, for each file with pending records whose `dup` is registered: `dup.sync_all()`; on failure record it in `Failure.files[file_id]` (first error wins), drop every pending record of that file (nothing appended for it), continue; then `write_all` every remaining pending record (sequence assigned at append), then `wal.sync_all()`. Under `Normal`: `write_all` only. Any WAL `create_dir`/`create_new`/`write_all`/`sync_all` error sets `Failure.wal` AND drops every registered duplicate at once (an open duplicate would block a rename on Windows; the log is dead, so no sync through it is ever needed again); afterwards every message is received and dropped (the sender never blocks on a dead log), `Close`/`Drain` acks still answer, until `Stop` or disconnect. `Close { file_id }`: if any pending record belongs to `file_id`, flush FIRST (file completion is a permitted early flush, V16:7016), then drop that file's `dup`; a pending record is never appended after its duplicate is gone. The ack answers `Failure.files[file_id]` if present, else `Failure.wal` if the log has failed (a file whose duplicate was dropped by a WAL failure never had its sync run, so "no error" would be untrue), else `None`. Under `Strict`, a `FileBegin` with `dup: None`, or a `Range` for a file with no registered duplicate, is a caller bug the writer fails closed on: at flush those records are dropped and `Failure.files[file_id]` is set to a `WalFault` with `Code::DestinationError`, kind `InvalidInput`, message "no duplicate registered", so no checkpoint is ever appended without the data sync in front of it. `Drain(ack)` flushes then sends a clone of `Failure`. `Stop` flushes, drops the dups, exits. A `RecvTimeout::Disconnected` without `Stop` drops the dups and exits WITHOUT a flush (ruling 11).

- [ ] **Step 0: State verification.** Confirm `copy.rs:193` defines `pub type Heartbeat<'g> = dyn Fn() -> flux_fs::Result<()> + 'g;`; `FaultFs` implements `DestinationRoot` (`fault_fs.rs:1507`) and `FakeDirHandle::create_dir` exists; the fake's `sync_all` records `sync_all(<path>)` (`:715`) and the `Drop` impl records `drop_writer(<path>)` (`:644`); `fs.delay("sync_all", d)` exists (`:963`). If any differs, STOP and report `STATE_MISMATCH`.

- [ ] **Step 1: Write the failing tests** in `writer.rs`. Helpers: `fn ws() -> (FaultFs, FakeDirHandle)`; `fn cfg(bytes: u64, interval: Duration, durability: Durability) -> WalConfig`; `fn begin(file: &[u8]) -> FileBegin`; `fn range(file: &[u8], start_mib: u64, end_mib: u64) -> Range` (digests of zeros); `let beat: &Heartbeat = &|| Ok(())`; `fn wal_bytes(fs) -> Option<Vec<u8>>`.
  - `nothing_is_created_until_the_first_flush_with_a_record`: open, send `FileBegin`, `wal_bytes()` is `None` and `fs.calls()` has no `create_dir(/ws/wal)` entry; `Stop`, join; now `wal_bytes()` decodes to exactly one `FileBegin`.
  - `the_byte_threshold_flushes`: `cfg(2 MiB, 1 h, Normal)`; begin, range 0..1, range 1..2: after a bounded wait (poll `wal_bytes` for up to 5 s), three records decode; no `sync_all(` in the log.
  - `the_timer_flushes_from_the_first_pending_record`: `cfg(1 GiB, 50 ms, Normal)`; begin + one range; `wal_bytes` is `Some` within 5 s.
  - `an_idle_timer_writes_nothing`: `cfg(1 GiB, 20 ms, Normal)`; open, sleep 200 ms, `Stop`, join: `wal_bytes()` is `None`, no `create_dir`, no `sync_all` (Review Focus 5).
  - `strict_syncs_the_partial_then_appends_then_syncs_the_wal`: `cfg(1 MiB, 1 h, Strict)`; `let p = fs.create_new("/ws/p")`; send `FileBegin { dup: Some(p.duplicate()) }` and range 0..1; `Drain`; the log, filtered to `sync_all(` entries, is exactly `["sync_all(/ws/p)", "sync_all(/ws/wal/000001)"]` in that order, and an `on_nth("sync_all", 1, ..)` hook installed before the sends captured `wal_bytes()` as `None` at the moment of the partial's sync (data before checkpoint).
  - `normal_never_syncs`: same sends with `Normal`; `Drain`; no `sync_all(` entry.
  - `a_failed_partial_sync_flags_the_file_and_appends_nothing_for_it`: `Strict`; create `q`'s partial FIRST, then `fs.fail("sync_all", Code::StrictDurabilityUnavailable)`, then `create_new("/ws/p")` (the fault is consumed by the NEXT `create_new` and travels with that handle and its duplicate, `fault_fs.rs:1263`); send begin+range for `q` (its dup) and for `p` (its dup), then `Drain`: `sender.failure().files` has exactly the key `b"p"` (a `WalFault` with `code == Code::StrictDurabilityUnavailable`), `wal` is `None`, and `wal_bytes()` decodes to exactly `q`'s two records (nothing for `p`).
  - `a_failed_wal_create_sets_the_wal_failure_and_later_sends_do_not_block`: `fs.fail("create_new", Code::DiskFull)` after creating the partials; begin + range + `Drain`: `failure().wal` is `Some`; the log has `drop_writer(/ws/p)` BEFORE the 100 further `Range` sends that follow, which all return `Ok` promptly (BS-2); `Stop`; `join()` returns a `Failure` whose `wal` is `Some`.
  - `close_drops_the_duplicate`: `Strict`; begin with dup; `Close`; `Drain`: the log has exactly one `drop_writer(/ws/p)` entry before the `Stop` (the original `p` is still alive in the test).
  - `close_flushes_that_files_pending_records_before_dropping_the_duplicate`: `Strict`, `cfg(1 GiB, 1 h, ..)`; begin with dup, range 0..1, `Close`, `Stop`: the log has `sync_all(/ws/p)` BEFORE `drop_writer(/ws/p)`, and the WAL decodes to both records (AB-2).
  - `strict_without_a_duplicate_appends_nothing_and_flags_the_file`: `Strict`; `Msg::FileBegin { begin: begin(b"p"), dup: None }` + range + `Drain`: the `Failure` the drain returns has `files[b"p"]` with kind `InvalidInput`, `wal_bytes()` is `None`.
  - `two_files_failing_in_one_flush_are_both_reported`: `Strict`; `fs.fail_always("sync_all", Code::StrictDurabilityUnavailable)` after both partials exist... NOTE: `fail` is consumed per handle at `create_new` (`:1263`), so instead create `p`, `fail(..)`, create `q`, `fail(..)`, create `r`; register all three; one range each; `Drain`: `files` has keys `q` and `r`, not `p` (Blindspot finding 1).
  - `drain_returns_the_failure_state_and_join_returns_the_final_one`: a failed dup sync then `Drain`: the drained `Failure.files` is non-empty; `Stop`; `join().files` equals it (CA-1).
  - `close_acks_that_files_failure`: `Strict`, a faulted partial `p`; begin with dup, range, `Close { file_id: p, ack: Some(tx) }`: `rx.recv()` is `Some(f)` with `f.code == Code::StrictDurabilityUnavailable`; a healthy file's `Close` ack is `None` (CA-3's mechanism; publication is gated by the 9e/inline sync on the ORIGINAL handle regardless, spec decision 6).
  - `close_after_a_wal_failure_acks_the_wal_error`: `Strict`; `fs.fail("create_new", Code::DiskFull)` after the partial exists; begin with dup, range, `Drain` (sets `wal`), then `Close { ack: Some(tx) }` for that file: `rx.recv()` is `Some(f)` with `f.code == Code::DiskFull`, not `None` (round-3 State Corruptor).
  - `open_with_a_lower_generation_is_refused`: write `(2,1)` and `Stop`; `open(.., generation 1)` is `Err(WalError::SequenceConflict { generation: 1, sequence: 1 })` and the file is unchanged (ruling 19).
  - `open_on_an_unavailable_identity_is_unsupported`: write a segment and `Stop`; `fs.set_identity("/ws/wal/000001", FileIdentity::Unavailable)` (`fault_fs.rs:2068` is the precedent); `open` is `Err(WalError::Io(e))` with `e.source.kind() == Unsupported`, and the log has no `open_existing(` entry (ruling 20).
  - `a_dropped_sender_without_stop_flushes_nothing`: begin + range, `drop(sender)`, join: `wal_bytes()` is `None`, and the log has `drop_writer(/ws/p)` (the dup was closed).
  - `stop_flushes_pending`: begin + range, `Stop`, join: two records.
  - `a_full_channel_blocks_the_sender_and_beats`: `cfg(.., Strict)` with `channel_cap: 1` and `fs.delay("sync_all", 300 ms)`; a counting beat (`Cell<u32>`); send begin (with dup), `Flush`, then three ranges: the beat count is `>= 1` by the time the sends return.
  - `send_after_join_is_wal_failed`: `Stop`, join, then `send(Range)` is `Err(SendError::WalFailed)` (Review Focus 4).
  - `a_heartbeat_error_while_the_channel_is_full_is_returned_as_is`: `channel_cap: 1`, `fs.delay("sync_all", 300 ms)`, a beat that returns `Err(FsError::new(Code::TargetLockBusy, ..))`: the blocked send returns `Err(SendError::Heartbeat(e))` with `e.code == TargetLockBusy`.
  - `reopen_continues_the_sequence_in_the_same_generation_and_restarts_in_a_new_one`: write `(1,1) (1,2)` through a writer and `Stop`; `open(.., generation 1)` returns `Some(scan)` with `last == (1,2)` and the next record is `(1,3)`; a fresh `open(.., generation 2)` on that file writes `(2,1)`.
  - `reopen_truncates_a_torn_tail`: after two records, append 7 garbage bytes with `fs.write_file("/ws/wal/000001", bytes + garbage)`; `open` returns `valid_len` equal to the two-record length and the file length equals it (`truncate_to` ran: the log has `truncate_to(/ws/wal/000001, <len>)`); a new record appends cleanly and the whole file scans.

- [ ] **Step 2: Run them to see them fail.** `cargo nextest run -p flux-core --no-fail-fast wal::writer`. Expected: compile error.

- [ ] **Step 3: Implement** per Interfaces: `std::sync::mpsc::sync_channel(cfg.channel_cap)`; the thread loop uses `recv_timeout(deadline - now)` where `deadline` is `first_pending_at + flush_interval` or `Duration::MAX`-ish when nothing is pending; `Failure` lives in an `Arc<Mutex<Failure>>` shared with the sender and returned by `join`; every error is converted with `WalFault::from_error` at the call site that observed it, so the original `FsError` is never stored (ruling 21); `WalSender::send` is a `try_send` loop (ruling 12). On `open` with an existing segment, the scan uses `workspace.open_dir("wal")?.open_read("000001")?` and the truncation `open_existing("000001", &dir.metadata("000001")?.identity)?.truncate_to(valid_len)`, then that same handle is the WAL writer (positioned at `valid_len`).

- [ ] **Step 4: Run, gate.** `cargo nextest run -p flux-core --no-fail-fast wal::writer`; `just check`. The two timer tests poll with a 5 s deadline; if either is flaky on CI, report it rather than lengthening a sleep.

- [ ] **Step 5: Amend the spec:** decision 5 (ruling 6, with the reason), decision 7.1 (ruling 16's sentence), decision 9 (ruling 11's exact wording of abandonment vs stop), decision 2 (`CHANNEL_CAP = 64`, `SEND_RETRY = 10 ms`, and `Close` flushing first); add a "Plan 1 rulings" ledger line listing rulings 1-21 by number, and write ruling 17 (the second workspace handle) and ruling 20 (`Unavailable` identity) into the spec's decision 7 and Interfaces as plan-2 obligations.

- [ ] **Step 6: Commit and push; read CI** on all three arms.

```bash
git add crates/flux-core/src/wal docs/superpowers/specs/2026-10-12-cut-9f1-chunk-checkpoints-design.md
git commit -m "feat(wal): single writer thread, bounded channel, 1 s / 64 MiB flush, Strict data-before-checkpoint (cut 9f-1 plan 1, Task 7)

Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>"
```


> **Superseded by the SDD rulings (final review fixes).** The Task 7 "Thread rules" and tests above are overtaken in these places: (1) abandonment is the shared `Arc<AtomicBool>` flag set when a `WalSender` is dropped without a delivered `Stop`, not channel disconnection (queued messages are delivered before a disconnect is reported); (2) the Strict ordering test reads the FaultFs call log (`sync_all(<partial>)` before the WAL's `create_dir`/`create_new` and `sync_all(<wal>)`), not an `on_nth("sync_all")` hook, which the fake's `sync_all` never reaches; (3) ruling 22 (Strict directory syncs) and ruling 23 (the torn-tail rule) apply to the writer's `open` and flush; (4) the oversize refusal is exact (`payload_len > MAX_PAYLOAD`, pinned by `the_payload_bound_is_exact`); (5) a re-sent `FileBegin` REPLACES the registered duplicate (`None` removes it) and clears that file id's entry in `Failure.files`; (6) `open` refuses an `operation_id` over 65,535 bytes with `WalError::Io` (`InvalidInput`).

---

## Self-review (done 2026-10-12, before handoff)

1. **Spec coverage, plan 1's share.** Interfaces (1) Task 3, (2)+(3) Task 1, (4) Task 1, (5) Task 2; decision 3 digests Task 4; decision 4 records Task 5 (alignment invariant, `wal_format`, `FILE_BEGIN` reset in Task 6); decision 5 flush policy Task 7 (amended, ruling 6); decision 6 Strict ordering and the duplicate lifecycle Task 7 (register once, counted descriptors is plan 2's cap accounting); decision 7.1 replay Task 6, truncation on open Task 7; decision 9 stops Task 7; decision 13's `Failure::Wal` signal Task 7 (the run-level disposition is plan 2); decision 14 handle-relative creation Task 7 (`create_dir`/`create_new` on the workspace handle). Left to plan 2, by design: everything in `copy.rs`, `tree.rs`, `run/`, reporting, `Code` variants, the manifest generation field, acceptance measurement, the crash matrix on real filesystems.
2. **Step scan.** Every implement step names a signature and a file; bodies appear only for the two algorithms the tests do not determine (the `update` slicing in Task 4, the `scan` sequencing rules in Task 6). "Step 0 locates" appears three times (the fake's `read_file` arms, the Windows status-mapping helper, the `lib.rs` module list): each is a lookup with one answer, not a decision.
3. **Type consistency.** `Digest`, `CHUNK_SIZE` (Task 4) are what Tasks 5-7 use; `FileBegin`, `Range`, `Body`, `Header`, `Decoded` (Task 5) are what Task 6 decodes and Task 7 encodes; `Scan`, `WalError` (Task 6) are what `open` returns; `Msg<W>` is generic over the `Writer` Task 1 made `Send`.
4. **Review Focus.** All five have a named test (Tasks 3, 5+7, 6, 7, 7).
5. **Proportion.** About 370 lines of plan for seven tasks and an estimated 1,500-2,000 lines of code and tests; no step is a transcript.

Exhaustiveness audit: contracts named with field types (Task 5 Interfaces, Task 7 `Msg`/`Failure`); no TBD; edge cases enumerated as tests (torn tail at every length, every byte flip, duplicate and conflict, generation bump, empty flush, dead thread, planted link, identity mismatch, extend refusal); each spec requirement in plan 1's share maps to a task (item 1 above). Open, each with its owner: the `file_id`/`source_identity` encodings (plan 2, ruling 14); the descriptor-cap accounting for registered duplicates (plan 2, where `OPEN_WRITERS_MAX` is applied); macOS `F_FULLFSYNC` through `File::sync_all` on a duplicate (plan 2's `verifying-platform-assumptions` pass, since no plan-1 test runs on macOS except CI's unit arm).

## Panel ledger (adversarial-panel-review; every finding carries one disposition)

- Solo round (driver): 5 findings, all FOLDED (Windows junction code; length-flip `TornTail`; unseen pair below `last`; failed-sync test ordering; Windows status mapping).
- agy round 1: 9 findings, all FOLDED (rulings 15-16; `Close` flushes first; Strict fails closed without a duplicate; `SendError`; `source_mtime` and framing stages; `div_ceil`; the `valid_through` test's opening `FileBegin`; streaming `scan`; the large-record test). Fold Auditor: the 5 solo edits CLEAN.
- agy round 2: FOLDED CA-1 (`Drain`/`join` return `Failure`), CA-2 (ruling 19), Blindspot-1 (`Failure.files` map), Blindspot-2 (ruling 18), Blindspot-3 (test syntax), BS-1 (ruling 20), BS-2 (duplicates dropped on a WAL failure), direct answer (c) (ruling 17). CA-3: mechanism FOLDED (the `Close` ack); its stated consequence REJECTED: a publication is gated by the 9e/inline sync on the ORIGINAL handle, "separate from, and in addition to" the WAL's (spec decision 6), so no rename ever waits on the WAL's duplicate sync. Fold Auditor: edits 1-9 CLEAN, with the regressions it found folded as CA-2 and Blindspot-1.
- agy round 3 (bespoke Executor Dry-Runner, State Corruptor re-aimed, Fold Auditor): Dry-Runner reached the commit step of Tasks 3 and 7 without a guess; Fold Auditor: the 9 round-2 edits CLEAN. FOLDED: the `Close` ack answered `None` after a WAL failure had dropped the file's duplicate (now answers the WAL error); the stale "`Drain(ack)` ... sends `()`" sentence (below floor, fixed).
- agy round 4 (narrow: the two round-3 edits; State Corruptor, Fold Auditor): State Corruptor "no new findings"; Fold Auditor: the Interfaces summary of the `Close` ack lagged the thread rule (below floor, aligned). Its unverified item, folded after measurement: `FsError` is not `Clone` (`error.rs:80`), so `Failure` carries a `WalFault` record instead (ruling 21). The mutation question was answered correctly: deleting the `else Failure.wal` branch turns `close_after_a_wal_failure_acks_the_wal_error` red.
- agy round 5 (narrow: ruling 21; Protocol Pedant, Fold Auditor): both "no new findings" (the Pedant checked that the CLI reports print only `code.as_str()` and the source's Display, `report.rs:172`, which `WalFault` keeps). Its unverified item folded: a `WalFault::new` constructor for the synthetic fault.
- PANEL VERDICT: GREEN at round 5. Rounds 1-3 were full panels (9 and 11 seats across them, plus a bespoke dry-runner); rounds 4 and 5 were NARROW re-audits of the previous round's edits, each with a sanctioned "no new findings" licence, and each came back clean. Hard cap (6) not reached.
- Stand-downs: DISCARDED-BELOW-FLOOR, a Dependency Cynic seat: this plan adds no dependency and no new constant import (`FILE_OPEN` is imported at `dir_windows.rs:21`; `blake3` at `crates/flux-core/Cargo.toml:14`).
