# Phase 3 measurements

Measurement-only record for the Phase 3 cut (cut 22). Sections are added task by task.

## Syscalls per file

These are syscall counts, not timing. No duration is reported here, and none of
the numbers below is a speed claim.

### Method

- Binary: `cargo build --release -p flux-cli` with
  `CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=limited` and
  `--target-dir <scratchpad>/target` (flux 0.19.8, LTO as in the repo profile).
  Built from commit `8f0e7c58aef55cd96f4105e8be2195857436f7e3` (branch
  `feat/phase3-measure`). `file` reports: ELF 64-bit LSB pie executable, x86-64,
  `with debug_info, not stripped`.
- Host: Linux 7.0.0-38-generic, ext4 (`stat -f` reports `ext2/ext3`, the shared
  magic number), not tmpfs; strace 6.19.
- Fixtures: `benches/phase3/make_tree.py` (`small` and `links`), 100 directories
  each: 1,000 files and 2,000 files per kind, plus an empty `small` tree (0 files,
  100 directories) for the fixed cost. `links` is the hard-link fixture: half of
  its entries are hard links (0.5 `linkat` per file below).
- Census: `strace -f -c -o strace-<name>.txt flux copy <src> <dst> --durability
  <normal|strict>`. Every destination was removed and confirmed absent
  (`rm -rf` then `test ! -e`) before its run. Every run exited 0 and produced the
  expected number of files.
- Per-file count of a call = `(count at 2,000 files - count at 1,000 files) / 1,000`.
  The empty tree gives the fixed cost, reported separately.
- One run per pair. Counts are near-deterministic, not exactly so: a second run of
  `small` 1,000 gave identical stat, read, write, rename, openat, fsync and
  fdatasync counts for both durabilities and differed only in `pwrite64` (+1 and
  +2) and in `brk`/`ftruncate`. The `futex` count was not tabulated or compared.
  Values like 8.998 below are that jitter, not a real fractional cost.

### Per-file counts (calls per file, 2,000 minus 1,000, divided by 1,000)

| call | small normal | small strict | links normal | links strict | fixed (empty tree, either durability) |
|---|---|---|---|---|---|
| `stat`/`newfstatat`/`statx` | 7.0 | 9.0 | 7.5 | 7.5 | 421 |
| `pread64`/`read` | 8.0 | 8.0 | 7.0 | 7.0 | 420 |
| `write` | 1.0 | 1.0 | 0.5 | 0.5 | 5 |
| `pwrite64` | 0.01 | 0.01 | 0.02 | 13.0 | 17 |
| `fsync`/`fdatasync` | 0.0 | 1.0 | 0.0 | 2.5 | 19 |
| `rename`/`renameat`/`renameat2` | 1.0 | 1.0 | 1.0 | 1.0 | 6 |
| `openat` | 2.0 | 2.0 | 1.5 | 1.5 | 230 |
| `getdents64` | 0.0 | 0.0 | 0.0 | 0.0 | 202 |
| `fcntl` | 0.0 | 0.0 | 0.0 | 0.0 | 7 |
| `linkat` | 0.0 | 0.0 | 0.5 | 0.5 | 0 |
| `utimensat` | 1.0 | 1.0 | 0.5 | 0.5 | 0 |
| `fchmod`/`fchmodat` | 1.0 | 1.0 | 0.5 | 0.5 | 0 |
| `mkdirat` | 0.0 | 0.0 | 0.0 | 0.0 | 104 |

Rows group the calls named in the brief; in this trace the stat family was `statx` and `newfstatat`, the rename family was `renameat2` (plus 3 `renameat`), and `fchmod` (not `fchmodat`) was used. Other calls (`close`, `fstat`, `unlinkat`,
`readlink`, `futex`, ...) are in the raw `strace-*.txt` files and are not part of
the requested column list.

### Raw counts (1,000 / 2,000 files)

| call | small normal | small strict | links normal | links strict |
|---|---|---|---|---|
| stat family | 7425 / 14425 | 9325 / 18323 | 7931 / 15430 | 7929 / 15428 |
| pread64 + read | 8420 / 16420 | 8621 / 16621 | 7421 / 14421 | 7421 / 14421 |
| write | 1005 / 2005 | 1005 / 2005 | 505 / 1005 | 505 / 1005 |
| pwrite64 | 1204 / 1214 | 1310 / 1316 | 1232 / 1253 | 12969 / 25985 |
| fsync + fdatasync | 119 / 119 | 1219 / 2219 | 126 / 126 | 2526 / 5026 |
| rename family | 1006 / 2006 | 1006 / 2006 | 1006 / 2006 | 1006 / 2006 |
| openat | 2230 / 4230 | 2231 / 4231 | 1731 / 3231 | 1731 / 3231 |
| getdents64 | 202 / 202 | 202 / 202 | 202 / 202 | 202 / 202 |
| fcntl | 7 / 7 | 7 / 7 | 14 / 14 | 14 / 14 |
| linkat | 0 / 0 | 0 / 0 | 500 / 1000 | 500 / 1000 |
| utimensat | 1000 / 2000 | 1000 / 2000 | 500 / 1000 | 500 / 1000 |
| fchmod | 1000 / 2000 | 1000 / 2000 | 500 / 1000 | 500 / 1000 |
| mkdirat | 104 / 104 | 104 / 104 | 104 / 104 | 104 / 104 |

### Which calls come from the lock check and which from redb

Method: one census without `-c`, `strace -f -k -o strace-k.txt flux copy <100-file
small tree> <fresh dst> --durability normal`. `strace -k` is supported (strace
6.19), but it printed most Rust frames as bare `flux() [0xOFFSET]`. The offsets
were resolved with `addr2line -f -C -i` against the same binary (virtual address =
printed offset + 0x1000 for the text segment), and each syscall was classified by
the function names in its resolved stack. Inlined frames are included. Two
further `-k` censuses of the same kind were run for strict (`small` 100 and
`links` 100) and for the empty tree.

Frames, from the resolved stacks (file:line from the debug info):

- Lock check, `newfstatat`: `<rustix::fs::at::statat>` called from
  `<StdDir as DirHandle>::metadata` (`dir_unix.rs:251`) called from
  `<flux_core::lock::held::Held<StdDir>>::still_owned` (`held.rs:101`).
- Lock check, `pread64`: `<std::fs::File as FileExt>::read_at` from
  `<StdLock as LockFile>::read_all` (`lock_file.rs:60`, `read_loop` at
  `lock_file.rs:161`) from `Held::still_owned` (`held.rs:110`) from
  `flux_core::run::session::owned` (`session.rs:438`) from `open_operation`
  (`session.rs:313`) from `flux_core::run::drive` (`mod.rs:460`).
- redb, `fdatasync`: `File::sync_data` from
  `<redb ... FileBackend as StorageBackend>::sync_data` (`optimized.rs:351`) from
  `PagedCachedFile::flush` (`cached_file.rs:597`) from
  `TransactionalMemory::new` (`page_manager.rs:1129`) from `Database::new`
  (`db.rs:1564`) from `Builder::create_file` (`db.rs:2170`), reached through
  `flux_platform::claims::io_err` / `RedbClaimStore`.
- redb, `pwrite64`: `File::write_all_at` from `FileBackend::write`
  (`optimized.rs:356`) from `PagedCachedFile::flush_write_buffer`
  (`cached_file.rs:493`) from `flush` (`cached_file.rs:595`), same
  `Database::new` path.
- Copy path (neither): `fchmod` from `flux_core::copy::stage_file`
  (`copy.rs:1086`); `write` of the state file from `flux_core::state::write_state`
  (`state.rs:957`); `statx` of the source from `flux_cli::resolve::job`
  (`resolve.rs:41`); `renameat2`, `utimensat`, `openat`, `read`, `getdents64`,
  `mkdirat`, `fsync` of the data file from the copy and tree code.

In the `small` 100-file normal census, classified by stack (counts over the whole
run, so they include the fixed cost; the empty-tree census gives that part):

| call | lock check (100 files) | lock check (empty tree) | redb (100 files) | other (100 files) |
|---|---|---|---|---|
| `newfstatat` | 503 | 202 | 0 | 106 |
| `pread64` | 1006 | 404 | 1 | 3 |
| `fdatasync` | 0 | 0 | 108 | 0 |
| `pwrite64` | 0 | 0 | 636 | 5 |
| `fcntl` / `flock` | 0 | 0 | 7 / 2 | 0 / 1 |
| `statx` | 0 | 0 | 7 | 507 |

Reading it: `(503 - 202) / 100 = 3.0` lock-check `newfstatat` and
`(1006 - 404) / 100 = 6.0` lock-check `pread64` per file. The 100-file figures
reproduce the normal-mode slope for `pread64` (6 lock-check + 2 `read` = the 8.0
above). The lock check is also run per directory (the empty tree, 100 directories,
already has 202 `newfstatat` and 404 `pread64` from it). redb's calls are
`fdatasync`, `pwrite64`, `fcntl`/`flock` (opening the database file) and 7 `statx`;
none of them grows per file in normal mode (`fsync`/`fdatasync` stay at 119 for
both 1,000 and 2,000 files).

What the strict-mode stacks show (strict, `small` 100 files):
`fsync` from the copy path rose from 11 to 111 (one per file), and the redb
`fdatasync` count was 208 against 108 in normal. The per-file strict slope in the
table (1.0 `fsync`/`fdatasync` total) is lower than that 100-file census suggests
(3 per file), so the redb sync count does not scale linearly with file count, and
the 100-file census cannot be extrapolated to per-file figures for strict. Per-file
numbers in the table come only from the 1,000/2,000 delta. For `links` strict the
100-file census has the redb `fdatasync` at 208 and `fsync` from the copy path at
61, and 1,234 redb `pwrite64`; the table's 13.0 `pwrite64` and 2.5 sync calls per
file are from the delta. The extra `stat` calls in `small` strict (9.0 against 7.0
per file) were not attributed: the lock-check `newfstatat` in the 100-file strict
census rose by only about 1 per file, which does not account for 2. That
difference is unexplained.

The inferred part: nothing above is inferred from call patterns; every
attribution is from a resolved stack. One caveat is that identical-code folding
and inlining in an LTO build can attach a shared leaf to several callers, so the
leaf names in a stack were read with their callers, not alone.
