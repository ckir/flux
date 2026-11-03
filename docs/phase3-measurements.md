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
- **A confirmation measurement is cheap and should come first (done, see the next section):** a frame-pointer build (`-C force-frame-pointers=yes`) so that `perf --call-graph fp` can attribute the two paths directly, replacing the upper bounds with shares.
- **Separate cut, owner to scope:** a bounded directory listing (Rule 4).
- **Worker pool:** nothing in this cut contradicts the AGY-FIRST design (workers stage, the coordinator owns claims, hardlinks, heartbeat, rename and log), but the serial fraction is unmeasured, so no speed-up is promised.
- **Strict:** 7.5-10.3 s for the same tree and a 27% spread between runs; nothing here attributes it.

### Attributed profile (frame-pointer build, added after the decision)

The binary was rebuilt with `RUSTFLAGS="-C force-frame-pointers=yes"`, symbols kept and line tables on, in a separate target directory (`CARGO_PROFILE_RELEASE_STRIP=false CARGO_PROFILE_RELEASE_DEBUG=line-tables-only`; the repository's profile is unchanged). Three recordings of the same 10,000 x 4 KiB tree, Normal durability: `perf record -e cpu-clock -F 999 -g --call-graph fp`, destination removed and `sync` between them, no other tool call during the runs; the one-minute load average at the start was 1.95, and background load was uncontrolled. The call chains survived: `main` is in 99.8-99.95% of the samples (it was 0% before). Samples: 1,684, 2,002 and 2,151.

Inclusive (`--children`) share of on-CPU samples; the wall-scaled column multiplies by 0.86, the on-CPU fraction of the earlier warm runs (it was not re-measured for these recordings, so treat it as approximate):

| Frame | Rec. 1 | Rec. 2 | Rec. 3 | Wall-scaled |
|---|---|---|---|---|
| `insert_if_absent` | 16.39% | 16.88% | 16.92% | 14.1-14.6% |
| claim-store `flush` | 4.16% | 6.29% | 4.60% | 3.6-5.4% |
| Rule 1 path (the two added) | 20.55% | 23.17% | 21.52% | 17.7-19.9% |
| `still_owned` (Rule 2) | 22.51% | 20.03% | 18.64% | 16.0-19.4% |
| `stage_file` (context) | 53.09% | 49.65% | 52.16% | |

- **Rule 1 holds with an attributed share**, in all three recordings, at 17.7-19.9% of wall time against the 15% threshold. It is close to the line: `insert_if_absent` alone is 14.1-14.6% scaled, under 15%, so the outcome depends on counting `flush` as the rule's text says. The `flush` row used is the one equal to its `drop_glue<RefMut<dyn ClaimStore>>` parent (4.16, 6.29, 4.60%), which attributes it to the claim store; the second `flush` row (2.55, 5.14, 3.25%) is redb's internal one. I did not open the caller chains to prove this.
- **Rule 2 holds with an attributed share**, in all three recordings, at 16.0-19.4% against 10%. The ablation had shown a replicate round at 6.2% and called the rule not robust; the profile does not reproduce that low value, so the "not robust" flag is withdrawn on this evidence. The 16-19% includes the checks made outside `stage_file` as well as inside it, and the symbol is inlined, so it covers its `fstat` and `pread` kernel time too.
- The two shares are not additive: whether `insert_if_absent` and `still_owned` overlap in any recording was not checked.
- `p` for Rule 3 is still not computed (the lock checks nested inside `stage_file` were not separated), which does not matter while Rules 1 and 2 both fire.
- Not measured: the effect of the change (a profile share is an upper bound on the gain, `1 / (1 - s)`: about 1.2x for 17.7% and 1.24x for 19.9%), Strict, other filesystems.
- Raw `perf.data` files are session scratch and are not kept in the repository.

### Plan defects found while executing (for the PR body)

1. The plan names `LockHeld::still_owned`; the type is `Held` (`crates/flux-core/src/lock/held.rs:17`).
2. Task 4 Step 3 pairs the ablated runs with a baseline taken minutes earlier. The machine drifted by about 25% inside the session, so the pairing overstated the shares (claims 31.7% and 45.8%, lock 38.7% and 38.1%); the same-round control is used above.
3. `perf record --call-graph dwarf` (default or 64 KiB stack) cannot unwind this LTO release binary on this machine; a frame-pointer build is needed.
4. The plan's commands say `python`; this machine has only `python3` (CI is unaffected).
5. Task 3 did not state that the flat fixtures hold zero-byte files and that the dry-run output went to a file, which matters when reading the 578.7 B/entry full-copy slope.

### Not controlled and not measured

Background load was uncontrolled and drifted (the control moved between 2,225 and 2,944 ms). The Windows and macOS engines, other storage, files larger than 4 KiB, a cold cache, Strict attribution (only its wall time), the hard-link tree's timing and any machine other than this one 8-core Linux box were not measured.

## Cut A: no name bookkeeping for a directory the run created

Change (`feat/fresh-dir-names`, commits e3662c2 and 4a69b0f): a directory this run created, or, dry, one that does not exist, gets a `NameIndex` that records nothing (`NameIndex::for_created_dir`); nothing reads it there (`tree.rs` resolves names only for a frame that is not fresh). Every other directory is built as before.

Method. Peak RSS (`/usr/bin/time -v`, `Maximum resident set size`, KiB), `flux copy SRC DST --durability normal` onto an absent destination, destination removed and `sync` between runs, the baseline and the changed binary alternating, three runs each. Both binaries are release builds with `-C force-frame-pointers=yes` and symbols kept; the baseline is `main` before the change, the changed one is the branch tip. Fixtures from `benches/phase3/make_tree.py`: `flat` (every file in the root) and `small --bytes 0 --dirs 1` (every file in one created subdirectory `d00`, so one `enter_dir` frame holds them all; `--dirs 100` would drop each frame at its directory end and hide a leak). Peak memory is not a wall-clock figure; no timing is claimed. Each run copied every file (`find DST -type f`).

| Fixture | Entries | Baseline median | Changed median | Baseline runs | Changed runs |
|---|---|---|---|---|---|
| flat, copy | 100,000 | 56364 | 16320 | 55956 56452 56364 | 16320 16228 16352 |
| flat, copy | 500,000 | 281528 | 53596 | 281528 281756 281472 | 53644 53580 53596 |
| one subdirectory, copy | 100,000 | 56640 | 16596 | 56824 56592 56640 | 16596 16504 17136 |
| one subdirectory, copy | 500,000 | 281708 | 53580 | 281632 281888 281708 | 53580 53496 53728 |
| flat, `--dry-run` | 100,000 | 13788 | 13876 | 13788 13716 13792 | 13748 13876 13932 |
| flat, `--dry-run` | 500,000 | 43592 | 43540 | 43608 43592 43420 | 43552 43540 43508 |

Slope = (median at 500,000 - median at 100,000) * 1024 / 400,000, bytes per entry:

| Fixture | Baseline | Changed |
|---|---|---|
| flat, copy | 576.4 | 95.4 |
| one subdirectory, copy | 576.2 | 94.7 |
| flat, `--dry-run` | 76.3 | 75.9 |

Acceptance (written in the plan before the data): the changed full-copy slope at most 120 B/entry on both fixtures, every run copied all files, the `--dry-run` slope within 5 B/entry of 76.1. **PASS** on all three. The full-copy slope falls by about 481 B/entry (83%), as the throwaway ablation predicted (95.4 B/entry; recorded in the plan's "Measured basis", `docs/superpowers/plans/2026-10-30-fresh-dir-names.md`, not in this report); `--dry-run` is unchanged, as it should be, because a dry run never publishes.

What this cut does NOT bound: a directory that already existed when the run started (an empty destination made with `mkdir`, or a `--resume` over an interrupted run) is not fresh and keeps its tracked index, about 577 B/entry (inferred from the baseline slope, not measured separately; it applies to a directory that has a claim key, because a directory without one never touches its index). The source walk's listing (about 76 B/entry, in `walk.rs`) is cut B. Background load was uncontrolled; peak RSS varied by under 1% across the three runs of each cell except two: the changed one-subdirectory 100,000-entry copy (17136 against 16504 and 16596) and the changed flat 100,000-entry `--dry-run` (13748 to 13932, 1.3%).

## Cut 23 baseline: peak memory of the unbounded directory structures

Measured before the bounded-listing cut, so its gates have a number to beat. Binary: release build of `main` at `c286819` (`~/.cache/flux-b-target/release/flux`). Fixtures: flat zero-byte trees from `benches/phase3/make_tree.py` (`flux-rss-flat-N`). Method: `/usr/bin/time -v`, "Maximum resident set size", two runs each, KiB. Peak RSS is not a timing.

| Command | 500,000 | 1,000,000 | 2,000,000 | Slope 1M to 2M |
|---|---|---|---|---|
| copy onto a pre-existing destination (default `--overwrite`) | 322224, 322224 | 622268, 621968 | 1221640, 1221720 | 614 B/entry |
| the same with `--resume` | 322260, 322052 | 622204, 622112 | 1221860, 1221780 | 614 B/entry |
| `verify` (both sides the same fixture) | not run | 145340, 145392 | 285972, 285656 | 144 B/entry |
| one file copied into a directory holding N entries | not run | 67340, 67272 | 129920, 129772 | 64 B/entry |

The inferred 577 B/entry for a pre-existing destination (see Cut A) is now measured: 614 B/entry, and the run-to-run spread is under 0.1%. The `--resume` runs followed a completed copy, so they exercise the same index build as the overwrite runs and not a partly-written tree; they agree to within 0.1%.

### redb store micro-benchmark (spec 6.3), criterion MISSED

Throwaway program outside the repo, `redb = "=4.4.0"`. Keys: 49 bytes (a fixed 25-byte prefix plus 24 pseudo-random bytes), value 8 bytes, random insertion order, 16 MiB cache, 100,000 inserts per write transaction with `Durability::None` and one final `Immediate` commit. Database on ext4 (`~/.cache`, not tmpfs). Lookups: 100,000 random keys after the ingest, OS page cache warm (it cannot be dropped without root). Peak RSS and wall time from `/usr/bin/time -v`. Two runs per size.

| names | ingest rate (2 runs) | database size | lookup median / p99 | peak RSS |
|---|---|---|---|---|
| 10,000,000 | 32,137 and 34,730 /s (288-311 s) | 2.16 GB (216 B/name) | 11.9-12.5 us / 36.6-39.2 us | 39.1-39.4 MB |
| 100,000,000 | 10,660 and 12,420 /s (8,051-9,380 s) | 12.9 GB (129 B/name) | 27.6-27.9 us / 896-956 us | 117 MB (both runs) |

Pre-registered criterion: at least 100,000 names/s at both sizes, bounded RSS, lookup median under 1 ms. **Ingest missed by 3x at 10M and 8-9x at 100M, and RSS grew from 39 MB to 117 MB, so it is not flat in n either.** Lookup passed. At 100M the rate extrapolates to roughly 7 hours for 300M names. Background load was uncontrolled (a browser and the desktop).

Caveat found afterwards, in the redb source: every commit but the last used `Durability::None`, and redb keeps the page bookkeeping of non-durable commits in memory until a durable commit (`redb-4.4.0/src/tree_store/page_store/page_manager.rs:378-403`, cleared at `:1281` and `:1764`). The RSS growth, and possibly the rate and database size, are therefore partly an artifact of this setting. A rerun with periodic `Immediate` commits was not made. The ingest-rate miss is a result for random-order inserts only; sorted-order inserts were not measured.

Not controlled: background load (a browser and the desktop session were running; the field check showed no other `flux` or build process).

### Hybrid destination index micro-benchmark (spec 6.3, amended at `6c8c25e`), criteria MISSED in both tiers

Throwaway program outside the tree (`.clavity/scratch/bounded-listing-hybrid/`, gitignored; built in the session scratchpad), run once by the orchestrator at top level with no other tool calls during the suite. Data on ext4 under `~/.cache` (not tmpfs), each program deleting its files at exit. Peak RSS and wall time from `/usr/bin/time -v`; `RssAnon` and `VmRSS` read from `/proc/self/status` by the program. Two runs per size, back to back; the load average was 6.59 (1 min) on 8 cores when the suite started and 5.35 when it ended, from uncontrolled background processes (an editor agent and a desktop process at up to one core).

**Tier 1** builds both always-built families, `names` (u16 length, u8 type, 24-byte pseudo-random name) and `inodes` (u64 `d_ino` big-endian, then the name record), for N entries: sorted runs of `RUN_ENTRIES = 262,144`, cascade merge with fan-in 64 and 64 KiB buffers, final run packed into 4 KiB blocks with a static multi-level block index, 256-block cache shared by both families. `build_rate` is N over the wall time of building BOTH families. Lookups: 100,000 random present keys per family after the build, through the block cache; the OS page cache is whatever the build left (stated, not controlled: at 100M the build wrote roughly 19 GB through a 23 GB box).

| entries | build rate (2 runs) | runs / merge passes | index bytes `names` + `inodes` | levels | peak RSS | `RssAnon` after build | `names` median / p99 | `inodes` median / p99 | block cache hit rate |
|---|---|---|---|---|---|---|---|---|---|
| 10,000,000 | 461,443 and 435,629 names/s (21.7 and 23.0 s) | 39 / 1 | 274 MB + 357 MB (27 + 36 B/entry) | 3 | 24.6 MB (both) | 3,884 and 3,880 KB | 4.8-5.0 us / 21.3-21.9 us | 3.6 us / 11.6-14.5 us | 54% |
| 100,000,000 | 221,139 and 325,857 names/s (452 and 307 s) | 382 / 2 | 2.74 GB + 3.57 GB | 3 | 24.6 and 24.5 MB | 5,316 KB (both) | 452 and 332 us / 8,740 and 4,531 us | 56.4 and 36.3 us / 5,198 and 3,248 us | 43% |

Against the pre-registered criteria (spec 6.3): **build rate at least 1,000,000 names/s MISSED at both sizes** (0.44-0.46 M/s at 10M; 0.22-0.33 M/s at 100M, where the second pass over 382 runs is added; counting both families' rows doubles the figures and still misses at 10M). **Resident memory flat PASSED**: peak RSS 24.6 MB at both sizes (within 8 MiB: the spread is 0.1 MB), `RssAnon` 3,884 KB to 5,316 KB over 90M more entries is 0.016 B/entry. **Lookups PASSED at 10M** (median 3.6-5.0 us, p99 under 22 us) and **MISSED at 100M** (medians 36-452 us, p99 3.2-8.7 ms). The 100M lookup figures are consistent with 2 to 3 reads per lookup reaching the device: the 1 MiB block cache holds the root and the second level but not the ~20 MB first level of a 100M-entry family, and the page cache was not warm after the build; the per-run `time` output was overwritten by later runs, so the page-fault count for those runs is not available. The 10M-to-100M build spread (452 vs 307 s) is a 47% gap between two consecutive runs of the same binary and the same input; the background load was uncontrolled.

**Tier 2** inserts N rows (25-byte prefix plus 24 pseudo-random bytes, 8-byte value) into one `redb = "=4.4.0"` table in random key order, 16 MiB cache, committing with `Durability::Immediate` every 1,024 inserts and rotating the write transaction after each commit (spec 6.3 wording). Lookups: 100,000 random present keys after the ingest.

| rows | ingest rate (2 runs) | commits | per commit | database | bytes written (from `time`) | `RssAnon` at 5M / 10M | lookup median / p99 | peak RSS |
|---|---|---|---|---|---|---|---|---|
| 10,000,000 | 3,533 and 3,476 rows/s (2,831 and 2,877 s) | 9,766 | about 290 ms | 1.08 GB (108 B/row) | 73 GB (143,444,120 blocks of 512 B, run 2) | 18,268 / 18,372 and 18,368 KB | 13.9-16.1 us / 38.4-46.5 us | 22.0 and 21.9 MB |

Against the criteria: **sustained rate at least 5,000 inserts/s MISSED** (3,476-3,533/s, 70% of the bar). **`RssAnon` slope PASSED**: about 100 KB between 5M and 10M rows, 0.02 B/row, so the periodic durable commit does remove the growth the earlier benchmark saw. The process had 26% of a CPU (user 319 s, system 443 s of 2,877 s wall), so the time goes to waiting on the durable commits; at 7.3 MB written per commit of 1,024 rows (copy-on-write of the leaf and its path for random keys) the commits are write-bound, not CPU-bound. The smoke run at 50,000 rows ingested at 178,557/s (6 ms per commit), so the per-commit cost grows with the tree.

Spec 6.3 and R1 pre-register the consequence: a missed criterion stops the plan and returns it to the owner with the numbers; no other store is designed speculatively. The criteria were the peer's proposal adopted as a veto-able call. Not controlled: background load (load average 5.4-6.6 on 8 cores throughout; an editor agent and a desktop process were running; the field check showed no other benchmark, `flux` or build process, and a stale wait loop was killed before the start).

#### Optimization rounds (same day; owner ruled the misses unacceptable)

Each candidate change became a flag of the same program (default: the original behaviour; the original binary was kept as the baseline), so each could be measured alone and together. Every run's `verify` mode and the byte-identical index sizes pin that no change altered the merged order or the record layout. Two runs per configuration at 10M; load averages 2.9-5.6 at the starts and ends (uncontrolled: an editor agent, the display server, once a transient pre-push hook from another repository).

Tier 1 at 10M, build rate in names/s (both families in the time), two runs each:

| configuration | build rate | peak RSS | random `names` lookup median / p99 |
|---|---|---|---|
| original | 521,724 and 542,564 | 24.5 MB | 4.8-4.9 us / 12.0-13.1 us |
| 8-byte key prefix in the spill sort | 1,011,916 and 1,334,756 | 28.7 MB | 4.7-4.9 us / 8.8-21.4 us |
| two threads, one family each | 515,481 and 602,859 | 28.8 MB | 4.4-4.7 us / 11.7-12.0 us |
| prefix sort + two threads | 1,631,658 and 1,747,346 | 28.7 MB | 4.4-4.7 us / 8.8-18.4 us |
| prefix sort + threads + prefix in the merge heap | 1,568,076 and 1,932,506 | 28.7-28.9 MB | (walk-order probes) 1.3-1.4 us / 3.4-3.8 us |

The spill sort's indirect slice comparisons were about half of the build time (the prefix alone doubled the rate); the two threads add about 1.1x because the single-threaded generation and spill phase dominates once the sort is fast. The 16-byte sort entry costs 4 MB of peak RSS.

Tier 1 at 100M, all with prefix sort and two threads:

| configuration | passes | build rate | peak RSS | `names` lookups median / p99 | `inodes` lookups (random) median / p99 |
|---|---|---|---|---|---|
| fan-in 384, 64 KiB merge buffers | 1 | 846,878 and 949,894 | 53.6-53.9 MB | walk order 1.4 us / 2.9 us; random with levels warmed 215 us / 1,596 us | 265-283 us / 2.4-3.2 ms |
| fan-in 384, 64 KiB, prefix merge | 1 | 1,215,727 | 53.8 MB | walk order 1.4 us / 3.4 us | 224 us / 1.2 ms |
| fan-in 384, 16 KiB, prefix merge | 1 | 1,210,000 and 1,217,771 | 28.8 and 28.7 MB | walk order 1.4 us / 3.1-3.4 us | 249-288 us / 1.6-3.8 ms |
| fan-in 64 (the spec's value), 64 KiB, prefix merge | 2 | 931,718 | 28.7 MB | walk order 1.4 us / 3.6 us | 14.2 us / 1.0 ms |

At fan-in 384 the 382 runs merge in one pass; the second pass costs about 1.3x. With 64 KiB buffers the two families' 384 readers hold 48 MB and peak RSS rises to 54 MB, which the "within 8 MiB of the 10M figure" criterion does not allow; 16 KiB buffers (6 MiB per family) keep it at 28.8 MB, equal to the 10M figure, at the same build rate. Walk-order `names` probes (the first L names in index order, read from the data blocks: the production step-1 pattern under C1) take 1.4 us at 100M because consecutive probes share blocks. Random probes at 100M still pay one device read per probe for the data block (about 200 us on this disk) even with the level blocks pre-read; the page-cache state after the build is not controlled (one run had the `inodes` file cached and measured 14 us).

Tier 2 at 10M rows, `Durability::Immediate` every 1,024, 16 MiB cache:

| configuration | ingest rate | first half | written | `RssAnon` 5M / 10M | lookup median / p99 |
|---|---|---|---|---|---|
| original (fsync, random keys) | 3,476 and 3,533 rows/s | | 73 GB | 18.3 / 18.4 MB | 14-16 us / 38-47 us |
| no-op `sync_data` backend, random keys | 25,473 rows/s | 28,904 | 11 GB | 18.2 / 18.4 MB | 11.8 us / 45.1 us |
| no-op `sync_data` backend, ascending keys | 568,532 rows/s | 647,360 | 0.7 GB | 17.5 / 17.5 MB | 13.2 us / 54.9 us |
| fsync, ascending keys | 159,992 rows/s | 169,881 | 1.1 GB | 17.5 / 17.5 MB | 13.0 us / 53.2 us |

The backend implements `redb::StorageBackend` (`db.rs:69-100`) over a `File` with `sync_data` returning `Ok(())`; `Database::builder().create_with_backend` (`db.rs:2182`). The durable commit path (`page_manager.rs:1739-1764`) still writes every dirty page and clears the non-durable bookkeeping at `:1764`, so the memory property is kept; only the `fdatasync` calls go (strace: zero with the backend, 59 for 49 commits without it). The store is an unlinked scratch no later run reads, so durability buys nothing there. Ascending keys are the production order of `names2`, `owners` and `learned` (publications arrive in walk order within a directory, C1); the order of `ids` keys is not established.

Against the criteria with the full set (prefix sort, prefix merge, two threads, fan-in 384 at 16 KiB, no-op `sync_data`): tier 1 build PASSES at both sizes (1.57-1.93 M and 1.21-1.22 M names/s), peak RSS flat PASSES (28.7-28.9 MB at both sizes), walk-order lookups PASS; random-probe lookups at 100M MISS as written (the data block read reaches the device); tier 2 PASSES (25k rows/s on random keys, 568k on ascending) with the `RssAnon` slope flat. Peer consult: two AGY-FIRST rounds (briefs `.clavity/seams/bounded-listing-hybrid-opt-r1.md`, `-r2.md`; replies under `.clavity/scratch/bounded-listing-hybrid-opt/`). Round 1 proposed the no-op `sync_data` backend, fan-in 384, the two threads, the prefix sort and the page-cache warming; it ranked the spill sort too low (third at 15-20%; measured about half), over-predicted the threads (1.7x; measured 1.1x) and the tier 2 gain (15-30x; measured 7.3x), omitted the second family's arena from the merge-phase RSS, and expected warmed levels to fix random probes (they fix the level read, not the data-block read). Round 2 accepted all five corrections and ALIGNED on the measured set with fixed constants `MERGE_FAN_IN = 384`, `MERGE_BUFFER = 16 KiB` (one pass to about 100M entries per directory, two passes above, measured price 1.3x; a 1,024 fan-in or a byte-budgeted dynamic fan-in was rejected as either 32 MiB of reader buffers or four times the read syscalls), the no-op `sync_data` backend as spec text (line 145), the two finish threads (line 133; the listing pass stays single), the prefix comparisons as an implementation requirement, and a rewording of the lookup criterion by probe pattern (walk-order `names` as the production step-1 figure; random probes stated under the page-cache state they ran in) and of the memory criterion as "bounded by the line 195 formula with the chosen constants". The owner adopted the set, the constants, the memory wording and the `tick` placement, and sent the lookup wording to a third round.

Round 3 (lookup criterion) ALIGNED on a combined clause: latency gates where the index controls the environment (walk-order `names`, the production step-1 pattern), structural gates (block-cache misses per probe, which are a property of the index and the probe stream, not of the machine) for random probes, random latencies reported with the page-cache state. Per-loop counters were then added and measured with the full set (`out4.txt`, one run each, load average 2.8 at the start and 2.0 at the end):

| probes at | `names` walk order: misses/probe, median / p99 | `names` random: misses/probe, median / p99 | `inodes` random: misses/probe, median / p99 |
|---|---|---|---|
| 10M | 0.0067, 1.4 us / 7.7 us | 1.79, 5.1 us / 20.1 us (files in the page cache) | 1.89, 4.3 us / 17.5-20.0 us |
| 100M, levels pre-read | 0.0067, 1.5 us / 3.8 us | 2.15, 256 us / 1,227 us | 2.41, 219-243 us / 1,165-1,475 us |
| 100M, nothing pre-read | | 2.15, 263 us / 1,451 us | 2.41, 224 us / 1,592 us |

The miss counts do not change with the page-cache state (identical to the probe with and without the pre-read); the latencies do. A random probe reads one block per level (3) plus the data block; the root and most of the second level stay in the 256-block cache, so 2.15 and 2.41 of the 4 reads miss. The peer's round 3 text put the `names` gate at 2.0 and the `inodes` gate at 2.5; the measured 2.15 refutes the first and leaves the second a 4% margin, so the numbers proposed to the owner are the structural bound of 3 misses per probe (levels plus the data block, minus the always-resident root) for both families, and 0.05 per probe for walk order (7x the measured 0.0067, which is one data block per about 150 consecutive probes). Walk-order latency gates stay at the peer's 5 us median and 20 us p99 (3.5x and 2.6x the measured figures).

## Cut 23 plan 2: the walk's spill

Commit measured: tip of `feat/bounded-listing` after Task 7 (`87bf540`); the test file is `crates/flux-core/tests/walk_cascade_memory.rs`. Peak RSS is not a timing and no timing is claimed here; the wall time of the run (584 s, dominated by creating 3,000,000 files) is incidental and UNVERIFIED as a figure.

### GATE-C (library): the cascade holds memory flat

Method: a counting global allocator (the pattern of `dry_run_memory.rs`), a flat directory of 1,000,000 and one of 2,000,000 zero-byte files made in permuted order under `~/.cache` (ext4), walked with `Thresholds::new(512, 4_096, 8, 16_384, 65_536)` (488 runs at 2,000,000, two cascade levels), the space placed on a second directory. Every yielded name is compared with the sorted sequence as it arrives (the zero-padded names sort numerically), `spilled_directories() == 1` for each walk, and the live bytes are compared with their baseline after the walk and the space are dropped.

| entries | peak live bytes above the baseline | live bytes left after drop |
|---|---|---|
| 1,000,000 | 597,860 | 0 |
| 2,000,000 | 604,748 | 0 |

Criterion: `peak_2m <= peak_1m + 1 MiB` (met: the difference is 6,888 B) and `LIVE` back at its baseline (met: 0 B, tolerance 64 KiB). One run; the allocation counts are deterministic and do not depend on machine load. Not mutation-tested: the test was not run against a deliberately leaking or non-cascading walk (each run takes about ten minutes); the non-vacuity of the pieces it relies on is covered by the unit tests of plan 2 (Tasks 3 and 4).

### GATE-A and GATE-X: release binary, peak RSS

Binary: release build of `feat/bounded-listing` at `7fb94df` (`~/.cache/flux-c23-target/release/flux`). Fixtures: `benches/phase3/make_tree.py` zero-byte trees, flat (`flux-rss-flat-N`) and one created subdirectory holding every entry (`flux-rss-one-N`, `--dirs 1`), N = 500,000, 1,000,000, 2,000,000, plus a flat 10,000,000. Method: `/usr/bin/time -v` "Maximum resident set size" (KiB), two runs each; `Anonymous` from `/proc/<pid>/smaps_rollup`, the sample with the largest `Rss` of a 50 ms sampler (it can miss a short peak, so it is a breakdown, not the peak). `--dry-run` (DEST absent), a fresh `copy` (DEST removed before each run), and `verify` over the freshly copied identical tree, all at `-vv`; the "listing spilled" lines on stderr were counted (stdout to `/dev/null`, stderr through a counting pipe). Peak RSS is not a timing; no wall-clock figure is claimed. Background load was NOT controlled (a browser at about 30% of a core and the review peer were running; the box had been idle of my own work); the field check found no stale measurement process and a probe (`yes`) read 100%.

Peak RSS, KiB, two runs each:

| Case | 500,000 | 1,000,000 | 2,000,000 | Slope 1M to 2M |
|---|---|---|---|---|
| flat, `--dry-run` | 29068, 29468 | 29300, 29548 | 29176, 29576 | -0.05 B/entry |
| flat, fresh `copy` | 30340, 30264 | 30260, 30320 | 30260, 30040 | -0.14 B/entry |
| flat, `verify` | 29384, 29668 | 29268, 29460 | 29424, 29668 | +0.19 B/entry |
| one dir, `--dry-run` | 28452, 29468 | 28920, 29696 | 29260, 29572 | +0.11 B/entry |
| one dir, fresh `copy` | 30312, 30336 | 30452, 30168 | 30348, 30376 | +0.05 B/entry |
| one dir, `verify` | 29592, 29724 | 29476, 29608 | 29692, 29512 | +0.06 B/entry |

Before this plan (`Cut 23 baseline` above, `main` at `c286819`): `--dry-run` grew by about 76 B/entry, a copy onto a pre-existing destination by 614, `verify` by 144. Now every slope is inside the run-to-run spread (up to about 1 MiB between two runs of one cell), so the figures are "flat", not "fractionally positive". Criterion: slope at most 5 B/entry (met in all six rows).

`Anonymous` at the sampled peak, KiB (two runs): dry-run 24880/24884 (500k), 24876/24876 (1M), 24876/24876 (2M); copy 24976/24980, 24976/24980, 24980/24980; verify 24892/24892, 24924/24920, 24988/24988; the one-dir rows are within 20 KiB of these. The anonymous slope 1M to 2M is 0 to 0.07 B/entry (at most 1 B required: met). The file-backed part is the binary and the shared libraries (`Rss` minus `Anonymous` is about 4.6 to 5 MiB, constant).

Spill counts: every `--dry-run` and `copy` run reported one "listing spilled" line (`entries=1000000 runs=4` at 1M), every `verify` run two (one per side), so the threshold was crossed in every cell; at N = 500,000 and above the production constants (8,192 inline, 262,144 per run) apply.

GATE-X (one `--dry-run` over the flat 10,000,000-entry fixture): peak RSS 29216 and 29072 KiB against 29176 and 29576 KiB at 2,000,000 (the difference is at most 0.5 MiB; the criterion is within 64 MiB: met); the `-vv` line reads `listing spilled entries=10000000 runs=39`, the 39 runs the spec predicts (10,000,000 / 262,144 rounded up).

A first attempt at this run was discarded: it took `ru_maxrss` from `wait4` inside a Python driver, and a forked child's peak includes the driver's own memory (the driver grew while deleting a 2,000,000-entry destination), so its figures climbed with N for a reason unrelated to the program; it also captured `-vv` stderr to a file that exhausted the `/tmp` user quota. The figures above come from `/usr/bin/time -v` around the binary (a small parent process), with stderr counted through a pipe.

Not measured: GATE-B and GATE-D (plan 3 and plan 1), a destination above the threshold (structure B), Windows, any timing. Windows CI has not run (no push without the owner's go-ahead): the Windows branches are compile-checked only.

## Cut 23 plan 3: the destination index

Commits measured: the harness numbers come from `e5bef9a` (plan 3 Tasks 1-10 on `feat/bounded-listing-3`); the first (missed) tier-1 numbers from `e296661`, the intermediate ones from `f0905d3`. Release builds in `~/.cache/flux-c23-target`; scale tests are `index::scale_tests::tier1_criteria` and `store::scale_tests::tier2_criteria` (all `#[ignore]`, `FLUX_SCALE_N`, `FLUX_SCALE_DIR=~/.cache/flux-scale`, run with `cargo nextest run --release -p flux-core --run-ignored only --no-capture`); GATE-B drives the release `flux` over `benches/phase3/make_tree.py flat` sources and `make_dest.sh` destinations with `/proc/PID/smaps_rollup` sampled every 50 ms (`Anonymous` = RssAnon, `Rss - Anonymous` = file-backed) plus `ru_maxrss`. Timing was approved by the owner for this task only (2026-11-02, 2026-11-03).

Machine and what was not controlled: the box of section 4.2 (8 cores, 24 GB, a QEMU virtual disk, `ROTA 0`, ext4 on `/dev/sda1`, `~/.cache` on it). The load average was 0.86-0.98 (one minute) before each timed run, higher between runs and 2-7 during GATE-B (other sessions, tools, an idle peer in `tmux`); the machine was not idle in the sense of the discipline and nothing background was stopped. The idleness probe was proven on a known-busy process (`yes` read `pcpu` 100-101 every time). Each figure was taken twice and the range is quoted. During the final GATE-B re-run light editing and waiting went on, so no wall-clock figure of GATE-B is claimed (RSS is not timing). Page cache: tier-1 files are 0.27 + 0.36 GB at 10M and 2.7 + 3.6 GB at 100M, so at 100M part of the random probes read the page cache and part the device; the latencies are reported, not gated.

### The first measurement missed the build-rate criterion; two fixes were measured

| Binary | Build, 10M (names/s, 2 runs) | Build, 100M | Phase split, 10M (push / finish) |
|---|---|---|---|
| `e296661` (first measurement) | 419,306 and 469,707 | 391,748 and 373,932 | 17.1-18.6 s / 3.7-3.8 s of 21-22 s |
| `f0905d3` (Task 9: runs buffered as encoded bytes, descriptors sorted) | 1,195,624 and 1,097,173 | 891,016 and 921,675 (MISS) | 5.0-5.5 s / 3.4-3.6 s |
| `e5bef9a` (Task 10: merge feeds the packer encoded records) | 1,242,050 and 1,309,537 | 1,163,568 and 1,128,387 | 5.2-5.9 s / 2.1-2.5 s |

Inside the push loop of the first build (10M, two instrumented runs, 20.1 s each): freeing the per-entry names after a run 7.5-7.9 s, `sort_keyed` over 72-byte values 6.5-7.0 s, `write_run` 4.3-5.1 s, the rest about 1 s; the name generator alone 0.29-0.35 s. `GLIBC_TUNABLES=glibc.malloc.hugetlb=1` changed nothing (push 17.5-18.3 s). The build time of the harness includes the pseudo-random name generator (0.3 s at 10M, 2.8-3.0 s at 100M) and both families.

### Tier 1 (`e5bef9a`), two runs each

| | 10M | 100M |
|---|---|---|
| build | 1,242,050 and 1,309,537 names/s (8.05, 7.64 s) | 1,163,568 and 1,128,387 names/s (85.9, 88.6 s) |
| runs / levels | 39 per family / 3 | 382 per family / 3 (`names`), 4 (`inodes`) |
| index bytes `names` + `inodes` | 273,825,792 + 357,122,048 | 2,738,180,096 + 3,571,171,328 |
| peak RSS of the test process (`/usr/bin/time -v`) | 49,480 and 49,240 KiB | 49,460 and 49,360 KiB |
| `RssAnon` after the build | 3,792 KiB (both) | 25,400 KiB (both) |
| `names`, walk order: median / p99, misses per probe | 1.7-1.8 us / 4.9-5.2 us, 0.007 | 1.6-1.9 us / 4.2-7.0 us, 0.007 |
| `names`, random: median / p99, misses per probe | 6.6-6.7 us / 21.8-24.5 us, 1.805 | 8.3-8.5 us / 26.7-27.1 us, 2.082 |
| `inodes`, random: median / p99, misses per probe | 6.5-7.0 us / 23.4-24.9 us, 1.882 | 7.7-8.3 us / 25.1-26.6 us, 2.346 |

`RssAnon` after the build is read after the builders have freed their buffers, so it does not see the build's peak; the peak is the `ru_maxrss` row (49 MB at both sizes: slope 0), and the `RssAnon` difference 10M to 100M is (25,400 - 3,792) KiB over 90,000,000 entries = 0.25 B/entry. The random-probe latencies depend on the page cache (the second 100M run of the first measurement, `e296661`, showed a p99 of 1.9 ms where the first run showed 38 us, under the same binary); the pre-registered gate is on misses per probe, not on these.

### Tier 2 (`redb`, unchanged by Tasks 9-10; first run, `e296661`), two runs

10,000,000 `ids_row`-shaped inserts in random key order, `publication_commit = 1,024`: 24,671 and 24,442 inserts/s (405 and 409 s); `RssAnon` 19,184 and 19,188 KiB at 5M rows, 19,272 and 19,276 KiB at 10M (slope 0.02 B/row); 1,000 random rows read back; `ids_rows` point lookups after the ingest: median 16.2 and 17.8 us, p99 83.8 and 54.3 us (files in the page cache). The harness's `has_name` probe reads the `names2` table, which `ids_row` never fills, so its 0.9 us figure is not a lookup figure and is not used. Tier 2 was not run at 100M (at 24.5k inserts/s it takes over an hour; the gate needs 10M).

### GATE-B (destination into a pre-existing directory), release `flux` at `e5bef9a`

Overwrite of an old copy (`flux copy -vv SRC DEST` where DEST already holds the same names), peak, KiB, two runs:

| | 500,000 | 1,000,000 | 2,000,000 | Slope 1M to 2M |
|---|---|---|---|---|
| `ru_maxrss` | 42,116; 41,684 | 42,368; 42,232 | 45,188; 44,996 | 2.8-2.9 B/entry |
| `RssAnon` at the sampled peak | 36,456; 36,436 | 36,776; 36,836 | 39,696; 39,700 | 2.9-3.0 B/entry |

Before this plan: 322,224 / 622,268 / 1,221,640 KiB (614 B/entry; table under "Cut 23 baseline"). With the first index build (`e296661`) the same cells read `ru_maxrss` 65,192-65,744 / 70,776-70,880 / 71,052-70,876 KiB and `RssAnon` 60,424 / 65,420 / 65,420 KiB (slope 0 on that binary, the buffers of a 262,144-entry run being the ceiling), and a `--resume` after a finished copy the same to within 1 MiB (66,144 / 65,420 / 65,420 KiB). The resume cells were not re-run on `e5bef9a`. Every run printed one `destination index spilled` line and two `indexing` lines (`entries=2000001 family=names runs=8`, `family=inodes runs=8` at 2M). The spec's criterion (marginal slope at most 5 B/entry) is met with a slope of about 3 B/entry, but the plan added a second pass line, an `RssAnon` slope of at most 1 B/entry, and that is MISSED (2.9-3.0; 500,000 to 1,000,000 is 0.7); it was reported to the owner; why the slope is not zero on the lighter final builder (the ceiling fell by 29 MiB and a growing term shows through) was not investigated and is a TODO row.

### Criteria of spec 6.3, the verdict

| Criterion | Result | |
|---|---|---|
| Tier 1 build at least 1,000,000 names/s at 10M and 100M | 1.24-1.31 M and 1.13-1.16 M | PASS (after Tasks 9-10; the first measurement MISSED at 0.37-0.47 M) |
| Tier 1 `RssAnon` slope at most 1 B/entry | 0.25 B/entry; peak RSS flat at 49 MB | PASS |
| `names` walk order: median under 5 us, p99 under 20 us | 1.6-1.9 us, 4.2-7.0 us | PASS |
| Walk-order at most 0.05 misses/probe; random at most 3 | 0.007; 1.8-2.3 | PASS |
| Tier 2 at least 5,000 inserts/s | 24.4-24.7 k | PASS |
| Tier 2 `RssAnon` slope at most 1 B/row | 0.02 | PASS |
| Lookup latencies reported with the page-cache state and the device | above | reported |
| GATE-B marginal slope 1M to 2M at most 5 B/entry (spec section 7) | `ru_maxrss` 2.8-2.9, `RssAnon` 2.9-3.0 B/entry (614 before) | PASS |
| GATE-B `RssAnon` slope at most 1 B/entry (the plan's additional pass line, Task 8 Step 4) | 2.9-3.0 B/entry | MISS (spec bar met; reported to the owner, TODO row) |

### Step 5: the bounded-memory argument with the production constants

Constants: `RUN_ENTRIES` 262,144, `MERGE_FAN_IN` 384, `MERGE_BUFFER` 16 KiB, block cache 1 MiB, store cache 16 MiB, inline index `INDEX_INLINE` 8,192 names per directory. Building the two families, a run is the encoded records plus a 24-byte descriptor per entry: `names` 262,144 x (27 + 24) B = 13.4 MB and `inodes` 262,144 x (35 + 24) B = 15.5 MB for 24-byte names (a `Vec` that doubled holds up to twice the record arena), both at once = 28.9 MB; finishing, two families merge at once: `2 * 384 * 16 KiB` = 12 MiB of cursor buffers (a record longer than the buffer grows its cursor's buffer), plus the packer's frame and the level entries held in memory (about 1/100 of level 0); plus 1 MiB of block cache, the store's 16 MiB cache and `256 * 8,192` inline names. The phases do not coexist within a directory (the arenas are cleared at each run's write and the cursors exist only after `into_pending`). Measured against that bound: a test process peak of 49 MB at 10M and at 100M (the harness adds its own probe arrays), and a copy of 2M entries peaking at 45 MB; the bound does not depend on the entry count, and the measured peak does not either to within the 3 B/entry above.

Windows: not run. The owner declined the push on 2026-11-02, so the cut-23 Windows branches (`close()` order, positional I/O, `wtf8` paths of the index) have never executed on Windows CI.
