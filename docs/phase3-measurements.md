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
`readlink`, `futex`, ...) are in the raw `strace-*.txt` files (kept only in the session scratchpad, not in the repository) and are not part of
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

## Peak memory against directory size

Method. Binary: the symbol-carrying release build of commit 8f0e7c5 (Task 2), reused without rebuilding. Fixtures built by `benches/phase3/make_tree.py` on ext4 under `target/bench-data` (git-ignored); the `flat` fixtures hold ZERO-BYTE files, and during the measured runs stderr (one `action=would_copy` line per file under `--dry-run`) went to a file: `flat` with 100,000 entries (`rss-flat-100k`) and 500,000 entries (`rss-flat-500k`), and `small` with 10,000 files of 4096 bytes in 100 directories (`rss-small-10k`). Inode headroom before building: 35,972,739 free of 37,506,560 (need about 1.1 million). Each fixture was run three times as a full copy and three times with `--dry-run`, the destination `rss-dst` absent before each run and removed after it.

Commands: `/usr/bin/time -v flux copy SRC DST` and `/usr/bin/time -v flux copy --dry-run SRC DST`; the figure is `Maximum resident set size (kbytes)` (KiB).

| Fixture | Mode | Run 1 | Run 2 | Run 3 | Median (KiB) |
|---|---|---|---|---|---|
| flat 100,000 | full copy | 56256 | 56216 | 56364 | 56256 |
| flat 100,000 | --dry-run | 14004 | 14000 | 13896 | 14000 |
| flat 500,000 | full copy | 282156 | 282356 | 282296 | 282296 |
| flat 500,000 | --dry-run | 43712 | 43788 | 43684 | 43712 |
| small 10,000 | full copy | 6472 | 6644 | 6416 | 6472 |
| small 10,000 | --dry-run | 4980 | 4984 | 4968 | 4980 |

Slope = (median RSS at 500k - median RSS at 100k) * 1024 / 400,000, in bytes per entry (denominator: 400,000 entries):

- `--dry-run`: (43712 - 14000) * 1024 / 400,000 = 76.1 bytes per entry.
- full copy: (282296 - 56256) * 1024 / 400,000 = 578.7 bytes per entry.

Not interpreted here. `--dry-run` on a flat tree works: it prints one `action=would_copy` line per file on stderr and exits 0.

Peak RSS is not a wall-clock figure; no timing is claimed in this section.

## Wall-time attribution of the small-file Normal copy

Fixture: 10,000 files of 4096 bytes in 100 directories (`make_tree.py small`), `flux copy SRC DST --durability normal`, ext4, warm page cache, binary of commit 8f0e7c5 built with symbols. Timings are wall-clock figures taken on a machine that was NOT controlled: before the runs the machine read 6-8% busy at idle (the session's own `claude`, `agy`, `clavity-ls` and `Xorg` processes, plus services nobody here controls), and the idleness probe was shown to see load (the same probe read 20.2% with one `yes > /dev/null` running). No subagent was running. Every run used a fresh destination removed afterwards, with `sync` run outside the timed region.

### Baseline wall time (Step 3)

| run | durability | `duration_ms` | user (s) | sys (s) | user+sys / elapsed |
|---|---|---|---|---|---|
| 1 (cold, discarded) | normal | 2984 | 0.90 | 1.66 | 0.856 |
| 2 (warm 1) | normal | 2902 | 0.89 | 1.61 | 0.862 |
| 3 (warm 2) | normal | 2821 | 0.87 | 1.57 | 0.865 |
| 1 (cold, discarded) | strict | 10275 | 1.86 | 12.05 | |
| 2 (warm 1) | strict | 7507 | 1.35 | 7.28 | |
| 3 (warm 2) | strict | 9803 | 1.63 | 10.77 | |

Normal: warm figures 2902 and 2821 ms (range 2821-2902), about 0.28-0.29 ms per file, with roughly 86% of the elapsed time on CPU and about two thirds of the CPU time in the kernel. Strict varies by 27% across the three runs (7507-10275 ms; 23% between the two warm runs, 7507 and 9803 ms), so no Strict figure is used by any rule.

### `perf` attribution (Steps 4 and 5): UNRESOLVED

Two recordings with the plan's command (`perf record -e cpu-clock -F 999 -g --call-graph dwarf`) and two more with a 64 KiB stack dump (`--call-graph dwarf,65528`) all lost their call chains:

| recording | samples | `main` in chain | `copy_one`/`walk_into`/`run_tree` in chain | `stage_file` | `still_owned` | leaf frame `[unknown]` (kernel) |
|---|---|---|---|---|---|---|
| default stack, 1 | 2700 | 0 (0.0%) | 6 (0.2%) | 5 | 0 | |
| default stack, 2 | 2342 | 0 (0.0%) | 6 (0.3%) | 10 | 6 | |
| 64 KiB stack, 1 | 2466 | 0 (0.0%) | 13 (0.5%) | 24 | 6 | |
| 64 KiB stack, 2 | 2730 (2670 with a non-empty chain; the percentages use 2670) | 0 (0.0%) | 38 (1.4%) | 32 | 8 | 1758 kernel-address leaves of 2670 (66%); 1952 (73%) counting every `[unknown]` leaf |

Kernel symbols are restricted on this machine (`kptr_restrict`), user chains stop within 13-17 frames of the leaf, and the root-most frame is `[unknown]` in 1257 of 2670 samples. No inclusive share of `insert_if_absent`, `flush`, `still_owned`, `stage_file` or `getdents` can be read from these recordings, so by Rule 5 every perf row is UNRESOLVED, and no wall-scaled perf share is reported. No lock-check share nested under `stage_file` could be measured either (0 samples).

Step 5 cross-check of the ablation against the Task 2 census: the lock check costs 3.0 `newfstatat` and 6.0 `pread64` per file (the 100-file stack census and the per-file slope; the lock-ablated binary on 1,000 files drops 3,203 stat and 6,406 read calls, which also contains the per-directory checks: stat family 7425 -> 4222, `pread64`+`read` 8420 -> 2014, everything else unchanged). That is about 9.0 of the roughly 26 syscalls a small Normal file makes (26.0 per file by the 1,000 -> 2,000 slope of all calls, 25.0 without `futex`), a third by count. A count share is not a time share, so this does not contradict a double-digit time share but does not show one either. The claim-store commit under Normal makes almost no per-file syscalls; the claims-ablated binary (stat family 7425 to 7421, read calls unchanged) drops `pwrite64` from 1205 to 118, about 11 fewer per directory, which is INFERRED to be the directory-end `flush` (no stack was taken of it), and about 1,000 `futex` calls (1106 to 106).

### Ablation (Step 6)

Two throwaway builds from scratch worktrees (never committed): `RedbClaimStore::insert_if_absent` returning `Ok(ClaimOutcome::Inserted)` right after `self.unsynced = after` (claims), and `Held::still_owned` returning `Ok(true)` as its first statement (lock; the type is `Held`, `crates/flux-core/src/lock/held.rs:99`). Timed in rounds, each round running claims, lock and an unmodified control one after the other. Rounds 1-3 are the plan's set (round 1 cold, discarded); rounds 4-6 are a replicate added after the control drifted. Shares below are against the control of the SAME round.

| round | control (ms) | claims-ablated (ms) | claims share | lock-ablated (ms) | lock share |
|---|---|---|---|---|---|
| 1 (cold) | 2944 | 2231 | 24.2% | 2781 | 5.5% |
| 2 | 2423 | 1982 | 18.2% | 1779 | 26.6% |
| 3 | 2225 | 1530 | 31.2% | 1746 | 21.5% |
| 4 | 2911 | 1861 | 36.1% | 2234 | 23.3% |
| 5 | 2658 | 1776 | 33.2% | 2492 | 6.2% |
| 6 | 2904 | 2313 | 20.4% | 2411 | 17.0% |

Plan pairing (ablated warm runs 2 and 3 against the Step 3 baseline runs 2 and 3, 2902 and 2821 ms): claims 31.7% and 45.8%, lock 38.7% and 38.1%. These overstate the shares: the control itself moved from 2944 to 2225 ms inside rounds 1-3 and back to 2911 ms in round 4, so a baseline taken minutes earlier is not comparable. The same-round control is the figure to use.

Warm rounds 2-6 against their controls: claims median 31.2% (range 18.2-36.1%), lock median 21.5% (range 6.2-26.6%). Two lock rounds (1 and 5) fall below 10%. The ablation removes correctness work as well (the claim record, the ownership check), so every figure is an UPPER bound on the real share. The run-to-run spread is far above 5 percentage points for both rows.

On process CPU time (`user+sys` of the same runs, which is less exposed to writeback and scheduler noise than wall time; a supplementary view, not the plan's metric): claims share 20.5%, 16.0%, 28.9%, 32.7%, 28.5%, 13.8% for rounds 1-6 (warm median 28.5%, range 13.8-32.7%); lock share 11.8%, 29.2%, 24.9%, 25.5%, 9.2%, 18.6% (warm median 24.9%, range 9.2-29.2%). One claims round (6) falls just under 15% and one lock round (5) just under 10%, so this view narrows the claims spread and leaves the lock row straddling its threshold; no outcome changes.

### What this section does not establish

Windows, macOS, spinning or networked storage, files larger than 4 KiB, a cold page cache (no root, `drop_caches` was not run), any Strict attribution (only its wall time was taken), any timing of the hard-link (`links`) tree, and any machine other than this one 8-core Linux box. The background load was uncontrolled and drifted visibly during the session. No decision rule is applied here; Task 5 does that.

## Decision

Rules written before any measurement (`docs/superpowers/plans/2026-10-30-phase3-measure.md`, "Pre-registered decision rule") applied to the figures above. No threshold was edited after the data existed.

| Rule | Input | Threshold | Outcome |
|---|---|---|---|
| 1 Batch Normal-mode claim writes | no `perf` share (UNRESOLVED); ablation upper bound against the same-round control: 18.2% and 31.2% (the plan's warm rounds 2 and 3); 18.2-36.1% over warm rounds 2-6 | >= 15% in both runs | FIRES PROVISIONALLY (ablation-only, owner confirms). Every warm round is above the threshold, but the figure is an upper bound and the spread is about 18 points |
| 2 Coalesce lock checks | no `perf` share (UNRESOLVED); ablation against the same-round control: 26.6% and 21.5% (rounds 2 and 3); 6.2-26.6% over warm rounds 2-6 | >= 10% in both runs | FIRES PROVISIONALLY (ablation-only, owner confirms), NOT ROBUST: round 5 of the replicate is 6.2%, below the threshold. On `user+sys` the lock share is 9.2-29.2% (median 24.9%), still one round under 10%. Independent of timing, the lock check is about 9.0 of the roughly 26 syscalls each file makes (3.0 stat + 6.0 read, counted), a third by count |
| 3 No Cut 1 and the Amdahl ceiling | Rules 1 and 2 both fired provisionally | applies only if neither joins | NOT APPLICABLE. `p` (the parallelisable staging share) could not be measured: the `perf` call chains were lost. It stays unmeasured; the worker-pool spec must say so |
| 4 Directory memory | `--dry-run` slope 76.1 bytes per entry (14,000 KiB at 100,000 and 43,712 KiB at 500,000 flat entries) | >= 64 B/entry | FIRES: recorded as a finding against spec section 9 for a bounded-listing cut; the owner scopes it. Context, not used by the rule: a full copy grows by 578.7 B/entry (56,256 KiB to 282,296 KiB) |
| 5 Unresolved rows | every `perf` row: frames lost in all four recordings | | applied: the ablation ran for Rules 1 and 2 and stands alone as an upper bound; neither row was counted as below threshold |

### Outcome

- **Cut 1 scope, provisional:** batch the Normal-mode claim writes and reduce the lock-ownership checks (`Held::still_owned`, three calls per file today), on the single-worker pipeline the AGY-FIRST consult proposed. Both items are ablation upper bounds, not attributed shares, and the lock item is not robust across the replicate. The owner decides whether to confirm them before a design spec is written.
- **A confirmation measurement is cheap and should come first:** a frame-pointer build (`-C force-frame-pointers=yes`) so that `perf --call-graph fp` can attribute the two paths directly, replacing the upper bounds with shares.
- **Separate cut, owner to scope:** a bounded directory listing (Rule 4).
- **Worker pool:** nothing in this cut contradicts the AGY-FIRST design (workers stage, the coordinator owns claims, hardlinks, heartbeat, rename and log), but the serial fraction is unmeasured, so no speed-up is promised.
- **Strict:** 7.5-10.3 s for the same tree and a 27% spread between runs; nothing here attributes it.

### Plan defects found while executing (for the PR body)

1. The plan names `LockHeld::still_owned`; the type is `Held` (`crates/flux-core/src/lock/held.rs:17`).
2. Task 4 Step 3 pairs the ablated runs with a baseline taken minutes earlier. The machine drifted by about 25% inside the session, so the pairing overstated the shares (claims 31.7% and 45.8%, lock 38.7% and 38.1%); the same-round control is used above.
3. `perf record --call-graph dwarf` (default or 64 KiB stack) cannot unwind this LTO release binary on this machine; a frame-pointer build is needed.
4. The plan's commands say `python`; this machine has only `python3` (CI is unaffected).
5. Task 3 did not state that the flat fixtures hold zero-byte files and that the dry-run output went to a file, which matters when reading the 578.7 B/entry full-copy slope.

### Not controlled and not measured

Background load was uncontrolled and drifted (the control moved between 2,225 and 2,944 ms). The Windows and macOS engines, other storage, files larger than 4 KiB, a cold cache, Strict attribution (only its wall time), the hard-link tree's timing and any machine other than this one 8-core Linux box were not measured.
