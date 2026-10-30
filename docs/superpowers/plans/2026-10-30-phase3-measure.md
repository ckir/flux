# Phase 3 Cut 22 (measure first) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Measure where the sequential copy engine spends its time and memory, so the first real Phase 3 cut (batched claims, coalesced lock checks, or straight to a worker pool) is chosen from numbers, not from a guess.

**Architecture:** No product code changes. One small stdlib-Python fixture generator (`benches/phase3/`), a symbol-carrying release build in the scratch directory, three measurements (syscall counts, peak RSS, wall-time attribution), and one report that applies a decision rule written down BEFORE the data exists.

**Tech Stack:** Python 3.14 (stdlib, `unittest`), `strace`, `/usr/bin/time -v`, `perf` (needs an owner decision, Task 4), cargo env-var profile overrides.

**Spec:** `FLUX_FULL_UPDATED_SPEC_V16.md` Phase 3 (lines 3697-3710), sections 8-10 (bounded RAM, backpressure), 47-49 (streaming planner, workers). Ordering decided 2026-10-30: the owner chose "measure first" after an AGY-FIRST consult (`.clavity/seams/phase3-sequencing.md`, reply `/home/user/.clavity/agy-replies/98b57167-9441-4bdd-b461-6fd9551537e8-6.md`). There is no design document: the owner opted out of the questions. Cut 1 and the worker pool are SPECS to write after this cut's report; they depend on numbers that do not exist yet, so no line-level plan for them is written here.

## Global Constraints

- No change under `crates/`, `Cargo.toml` or `Cargo.lock`. The symbol build uses `CARGO_PROFILE_RELEASE_STRIP=none` and `CARGO_PROFILE_RELEASE_DEBUG=limited` in the environment (`limited` keeps the inlined-call records `perf` needs; `line-tables-only` may not), with `--target-dir` in the scratchpad.
- Fixtures live on the repository disk (ext4, measured `stat -f` reports `ext2/ext3`), never tmpfs (`benches/copy.rs:12-13` refuses it for the same reason: fsync costs nothing there).
- Python 3.14, standard library only, warnings are errors: tests run as `python -W error -m unittest discover -s benches/phase3 -p "test_*.py"`.
- Timing discipline (CLAUDE.md): TIMING IS MEASURED AT THE TOP LEVEL ONLY, after the owner's go-ahead, with no subagent and no other tool call during a run; two runs, quote the range, name the warm figure, state the uncontrolled background load, and prove the idleness probe can see a busy process. Counts (Tasks 2, 3) are not timing and may run any time.
- Branch `feat/phase3-measure` off `main` (869cc76). `just pr` arms auto-merge, so every gate runs before it.

## Review Focus

- One directory holding every file (spec 9: "never a list proportional to the total number of files"): Task 3 measures peak RSS against entry count in a flat directory.
- Many tiny directories (the 10,000 files in 100 directories shape of `TODO.md:1087`): Task 1 builds it, Tasks 2 and 4 measure it.
- Hardlinked sources (they route through the redb hardlink index, a different cost centre from plain files): Task 1 builds a `links` tree, Task 2 counts its syscalls.
- A zero-file source: the per-file figure subtracts an empty-tree baseline, so Task 1 pins that a zero-file build works.
- A fixture destination that already holds files: the generator refuses it (Task 1) so a stale tree cannot be measured as a fresh one.

## Pre-registered decision rule (written before any measurement)

Applied mechanically in Task 5. "Share" means a fraction of the median warm wall time of `flux copy --durability normal` on `small` (10,000 files x 4096 bytes in 100 directories).

Why these thresholds: a share `s` of wall time caps the gain from removing it at `1/(1-s)`, so 15% is at most 1.18x and 10% at most 1.11x; below that the refactor is not worth its risk (it touches the claim store and the lock path, both of which the recovery matrix depends on).

1. **Batch Normal-mode claim writes** joins Cut 1 iff the redb claim-commit path is >= 15% in BOTH runs. That path is the INCLUSIVE (`--children`) cost of `RedbClaimStore::insert_if_absent` (`crates/flux-platform/src/claims.rs:213`, one non-syncing commit per file under Normal) PLUS `RedbClaimStore::flush` (`claims.rs:265`, the synced commit at each directory end). Every share in these rules is inclusive; self-only samples are never used.
2. **Coalesce lock checks** joins Cut 1 iff the lock-ownership check (`LockHeld::still_owned`, `crates/flux-core/src/lock/held.rs:99`: one stat plus one read) is >= 10% in BOTH runs.
3. If neither joins AND neither row is UNRESOLVED, there is no Cut 1 (with an UNRESOLVED row the decision waits for the Step 6 ablation and then the owner, per Rule 5): Phase 3 goes straight to the worker-pool spec, and the report states the serial fraction `1 - p` and the Amdahl ceiling (with no `perf` recording `p` cannot be measured: "no Cut 1" still stands when both ablations are conclusively below threshold, and `1 - p` and the ceilings are reported as UNRESOLVED, "no perf recording"). `p` is the wall-scaled inclusive share of the file-staging work (open source, create temporary, read/write, set permissions and times); everything else (walk and `getdents`, name and claim bookkeeping, the lock check, the publish rename, reporting) stays on the coordinator under the design agy recommended in the consult (an assumption for this bound, not an owner decision), so it is `1 - p`. The ceiling is `1 / ((1-p) + p/N)` for N = 2, 4, 8.
4. **Directory memory:** from Task 3, the slope in bytes per entry between 100,000 and 500,000 flat entries of `flux copy --dry-run` (walk and plan only, so redb's 16 MiB cache and the copy buffers do not enter the slope). The full-copy slope is reported beside it as context, not used by the rule. A slope >= 64 B/entry is recorded as a finding against spec 9 for a bounded-listing cut; the owner scopes it.
5. A share the profile cannot attribute (frames lost to inlining, run-to-run spread > 5 percentage points) is reported as UNRESOLVED, the rule does not fire on it, and the ablation of Task 4 Step 6 runs for that row before the owner decides. An UNRESOLVED row never silently counts as "below threshold". When there is no `perf` share at all (the owner declined the sysctl, or `perf` recorded nothing), the ablation share stands alone as an UPPER bound: below the threshold it is conclusive (the true share is lower still, so the rule does not fire); at or above the threshold the rule fires PROVISIONALLY, marked "ablation-only" for the owner to confirm.

---

### Task 1: Fixture generator

**Files:**
- Create: `benches/phase3/make_tree.py`
- Create: `benches/phase3/test_make_tree.py`
- Modify: `.github/workflows/ci.yml:165` (add a second `run:` line, same shape, for `benches/phase3`)

**Interfaces:**
- Produces: `build(kind: str, dest: pathlib.Path, files: int, size: int, dirs: int) -> int` returns the number of files created. `kind` is `"small"` (files spread round-robin over `dirs` directories named `d00`.., content from a per-index deterministic fill), `"flat"` (all files directly in `dest`, no subdirectory, zero bytes, `size` and `dirs` ignored), `"links"` (`files` names spread round-robin over `dirs` directories; file `2k` and file `2k+1` are hardlinks of one inode, so every file has `st_nlink == 2`; `files` must be even; `small` and `links` create ALL `dirs` directories even when `files` is 0, so the zero-file `small` baseline is `dirs` empty directories; directory names are `d` plus the index zero-padded to `max(2, len(str(dirs-1)))` digits, file names are `f` plus the index zero-padded to 6 digits). Raises `FileExistsError` when `dest` exists and is not empty, `ValueError` for an unknown kind or an odd `files` with `links`.
- Produces: CLI `python benches/phase3/make_tree.py KIND DEST [--files N] [--bytes B] [--dirs D]`, defaults `--files 10000 --bytes 4096 --dirs 100`, prints the file count.

- [ ] **Step 1: Write the failing tests** in `test_make_tree.py` (`unittest`, `tempfile.TemporaryDirectory`): `test_small_builds_files_across_dirs` (`build("small", d, 10, 4096, 2) == 10`, two directories of five files, each 4096 bytes); `test_small_content_is_deterministic` (two builds are byte-identical); `test_flat_puts_everything_in_one_dir` (one directory, N empty files); `test_links_pairs_share_an_inode` (every file has `st_nlink == 2`); `test_links_rejects_an_odd_count` (`ValueError`); `test_zero_files_builds_an_empty_tree` (`build("small", d, 0, 4096, 3) == 0`, and `d` holds exactly the three empty directories `d00`, `d01`, `d02`); `test_refuses_a_non_empty_destination` (`FileExistsError`); `test_unknown_kind` (`ValueError`).
- [ ] **Step 2: Run to see them fail**
  Run: `python -W error -m unittest discover -s benches/phase3 -p "test_*.py"`
  Expected: errors importing `make_tree`.
- [ ] **Step 3: Implement `build` and the CLI** in `make_tree.py`. File content for index `i` is `(i + 1).to_bytes(8, "little")` repeated to `size` bytes (any deterministic fill is acceptable). Add the licence header every file in `benches/` carries (copy it from `benches/copy.rs:1-5` in the `#` comment form).
- [ ] **Step 4: Run to see them pass**
  Run: the command from Step 2. Expected: `Ran 8 tests ... OK`.
- [ ] **Step 5: Wire CI.** Add `- run: python -W error -m unittest discover -s benches/phase3 -p "test_*.py"` after `ci.yml:165`.
- [ ] **Step 6: Commit** `git add benches/phase3 .github/workflows/ci.yml && git commit -m "bench: add the phase 3 fixture generator"`

### Task 2: Symbol build and syscall census (counts, not timing)

**Files:**
- Create: `docs/phase3-measurements.md` (the report; Task 5 completes it)

**Interfaces:**
- Consumes: `make_tree.build` from Task 1.
- Produces: a table "syscalls per file, by call" for `small` and `links`, `--durability normal` and `strict`, in the report.

- [ ] **Step 1: Build the binary with symbols**
  Run: `CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=limited cargo build --release -p flux-cli --target-dir <scratchpad>/target`
  Expected: `Finished`; `file <scratchpad>/target/release/flux` says `not stripped`. Record the commit it was built from in the report.
- [ ] **Step 2: Build fixtures** under `$FLUX_BENCH_DIR` (default `target/bench-data`, gitignored; confirm with `git check-ignore`): `small` with 1,000 and 2,000 files (100 directories), `links` with 1,000 and 2,000, and an empty `small` (0 files).
- [ ] **Step 3: Count syscalls.** For every (fixture, durability) run `strace -f -c -o <scratchpad>/strace-<name>.txt <flux> copy <src> <fresh dst> --durability <d>` with a destination that is absent before each run (`rm -rf <dst>` and `test ! -e <dst>` first, outside the measured command; never reuse or retry onto an existing destination). Per-file count of a call = `(count at 2,000 - count at 1,000) / 1,000`; the empty tree gives the fixed cost, reported separately. Counts are deterministic, so one run each is enough; say so in the report.
- [ ] **Step 4: Tabulate** in `docs/phase3-measurements.md`: per-file counts of `stat`/`newfstatat`/`statx`, `pread64`/`read`, `write`, `pwrite64`, `fsync`/`fdatasync`, `rename`/`renameat`/`renameat2`, `openat`, `getdents64`, `fcntl`, `linkat`, `utimensat`, `fchmod`/`fchmodat`, `mkdirat`. Name which of these come from the lock check and which from redb by running one separate census WITHOUT `-c` (`strace -f -k -o <scratchpad>/strace-k.txt ...`; `-c` suppresses the per-call lines that `-k` annotates) on a 100-file tree and quoting the frames (strace `-k` needs the symbols from Step 1; if `-k` is unsupported, say so and attribute by call pattern, labelled as inferred).
- [ ] **Step 5: Commit** the report skeleton and table: `git add docs/phase3-measurements.md && git commit -m "docs: record the per-file syscall census"`

### Task 3: Peak memory against directory size

**Files:**
- Modify: `docs/phase3-measurements.md`

**Interfaces:**
- Consumes: the Task 2 binary and `make_tree`.
- Produces: peak RSS (KiB) for `flux copy` of `flat` at 100,000 and 500,000 entries, and of `small` at 10,000 files in 100 directories, plus the slope in bytes per entry.

- [ ] **Step 1: Build the fixtures**: `flat` at 100,000 and 500,000 entries, and `small` at 10,000 files of 4096 bytes in 100 directories (check inode headroom first with `df -i .`: the need is both fixtures' inodes plus one destination of the larger size, about 1.1 million, since each destination is removed after its run; stop and report if free inodes are under 1.5x that).
- [ ] **Step 2: Measure.** For each fixture, three runs of `/usr/bin/time -v <flux> copy <src> <dst>` and three of the same with `--dry-run` (destination absent before each, removed after each); take `Maximum resident set size`. Peak RSS is not a wall-clock figure, so the timing rules do not apply, but quote all three values.
- [ ] **Step 3: Compute** slope = `(median RSS at 500k - median RSS at 100k) * 1024 / 400,000` bytes per entry, once for the `--dry-run` runs and once for the full copies, and put the table and the slope in the report. Do not interpret it yet (Task 5 does, by rule 4).
- [ ] **Step 4: Commit** `git commit -am "docs: record peak memory against directory size"`

### Task 4: Wall-time attribution (OWNER GO-AHEAD REQUIRED, top level only)

**Files:**
- Modify: `docs/phase3-measurements.md`

**Interfaces:**
- Consumes: the Task 2 binary and the `small` 10,000 x 4096 fixture (`make_tree.py small DEST --files 10000 --bytes 4096 --dirs 100`).
- Produces: median warm wall time and the shares for rules 1 and 2, per run, with the spread.

- [ ] **Step 1: Ask the owner for the go-ahead and for the `perf` decision.** `cat /proc/sys/kernel/perf_event_paranoid` was `4` on 2026-10-30, which denies `perf record` to an unprivileged user. The owner either runs `! sudo sysctl kernel.perf_event_paranoid=1` (reverted afterwards) or picks the ablation fallback in Step 6. Do not proceed without an answer.
- [ ] **Step 2: Clear the field.** No subagent running, no background job. Enumerate processes for any stale `flux`, `cargo`, `perf`, `strace` and kill them. Prove the idleness probe works: start `yes > /dev/null`, confirm the probe (`ps -o pcpu` and `/proc/stat` delta) reports it busy, kill it, then read the idle baseline. Record the result; background services (indexing, updates) are uncontrolled and stated as such.
- [ ] **Step 3: Wall time.** Backgrounded, then no tool call until the completion notification: three runs of `/usr/bin/time -v <flux> copy <src> <dst> --json --durability normal`, the first discarded as cold, the other two reported (`duration_ms` from the `--json` report, `crates/flux-cli/src/report.rs:46`, and `user`, `sys` and elapsed from `time -v`, which is where the scaling factor of Step 4 comes from); the destination is absent before each run, and after each run it is removed and `sync` is run, both outside the timed region, so one run's journal and writeback work cannot overlap the next. Repeat for `--durability strict`. Quote the range, name the warm figure.
- [ ] **Step 4: Profile.** Backgrounded, no other calls: `perf record -e cpu-clock -F 999 -g --call-graph dwarf -o <scratchpad>/perf-normal-<n>.data -- <flux> copy <src> <dst> --durability normal` for `<n>` = 1 and 2 (destination absent before each, removed and `sync` after each, as in Step 3), and `perf report -i <scratchpad>/perf-normal-<n>.data` for each. `cpu-clock` samples only on-CPU time, so also take `user+sys` and elapsed from the UNPROFILED Step 3 runs (`/usr/bin/time -v` never wraps `perf`, whose own stack copying would inflate `user+sys`) and report each share twice: of on-CPU samples, and scaled by `(user+sys)/elapsed` to a share of wall time, where recording `<n>` uses the factor of the `<n>`-th REPORTED warm run of Step 3 (the second and third runs are warm runs 1 and 2). Rules 1 and 2 use the wall-scaled figure. `perf report --children --sort symbol` (inclusive, the only column the rules use; `--no-children` may be shown for context) for the frames under `RedbClaimStore::insert_if_absent`, `RedbClaimStore::flush`, `LockHeld::still_owned`, `flux_core::copy::stage_file` (`crates/flux-core/src/copy.rs:797`), `getdents`. Share = samples under the frame / total samples, per recording. Rule 3's `p` is, per recording, the inclusive `stage_file` share MINUS the inclusive `LockHeld::still_owned` share found NESTED under `stage_file` in the expanded call graph (`perf report --stdio` with the call graph shown), never the flat symbol row, which also counts the lock checks made outside `stage_file` (`stage_file` itself makes several, `copy.rs:817`, `:851`, `:874`) (the lock check belongs to `1 - p`); Rule 3 reports the ceiling for each recording and quotes the lower one.
- [ ] **Step 5: Cross-check against the census.** A share for a SYSCALL-BOUND frame (`still_owned`, `stage_file`'s create/write, `getdents`) must be consistent with the per-file syscall counts of Task 2 (a 20% share with 0.1 syscalls per file would be suspect); say where it is not and mark that row UNRESOLVED. The redb per-file commit under Normal is user-space work with almost no syscalls (`claims.rs:190-191`), so it is exempt from this check and is cross-checked by the ablation of Step 6 instead.
- [ ] **Step 6: ablation.** Always for the redb per-file commit (it has no syscall cross-check, Step 5); for every other row only if the owner declines the sysctl or the row is UNRESOLVED. In a scratch `git worktree` created under `<scratchpad>` (never inside the repository; never committed, never pushed; removed afterwards with `git worktree remove --force <path>` and confirmed gone with `git worktree list`) make `RedbClaimStore::insert_if_absent` (`claims.rs:213`) return `Ok(ClaimOutcome::Inserted)` immediately after setting `self.unsynced = after` from `next_commit()` and without touching the database (so `flush` still runs); separately make `LockHeld::still_owned` (`crates/flux-core/src/lock/held.rs:99`) return `Ok(true)` as its first statement (its callers are `run/session.rs:438` and `:473`). Build, then time as in Step 3: three runs each, the first discarded as cold, the other two paired by index with the Step 3 baseline runs two and three. For each ablation the share of paired run `i` is `(baseline_i - ablated_i) / baseline_i`. With no inserts the database has no dirty pages, so `flush` also becomes cheap: the ablation drop therefore covers the WHOLE claim path of Rule 1 (`insert_if_absent` plus `flush`) and is compared with the `perf` share of that whole path. The `perf` share governs the rule. Each paired run is compared individually, and a gap of more than 5 percentage points in either pair makes the row UNRESOLVED. The drop is an UPPER bound on the share (it also removes correctness work) and the report labels it so.
- [ ] **Step 7: Restore the machine.** If `kernel.perf_event_paranoid` was lowered, the owner restores it (`! sudo sysctl kernel.perf_event_paranoid=4`) and `cat /proc/sys/kernel/perf_event_paranoid` must print `4`; do this even when an earlier step aborted. Remove the Task 3 `flat` fixtures and the Task 2 and 4 fixtures and destinations under `$FLUX_BENCH_DIR`, and confirm `git worktree list` shows no scratch worktree.
- [ ] **Step 8: Commit** `git commit -am "docs: record the small-file wall-time attribution"`

### Task 5: Decision record and handover

**Files:**
- Modify: `docs/phase3-measurements.md` (add "Decision" section)
- Modify: `ROADMAP.md` (Phase 3 row of the build-order table: one clause naming the measurement and the result)
- Modify: `TODO.md` (a "Cut 22 findings" entry for anything the rule left UNRESOLVED or for the memory finding of rule 4)

- [ ] **Step 1: Apply rules 1-5 mechanically** to the Task 3 and 4 numbers; write each rule's input, threshold, and outcome in a table. Do not edit a threshold after the data exists; if a rule looks wrong, say so in the report and leave the owner to overrule.
- [ ] **Step 2: State the outcome**: the Cut 1 scope (or "no Cut 1"), the Amdahl ceilings if rule 3 fired, and the memory finding if rule 4 fired. Say what was not controlled (background load) and what was not measured (Windows and macOS, networked and HDD storage, files larger than 4 KiB, `--durability strict` attribution if only wall time was taken).
- [ ] **Step 3: Verify the claims in the ROADMAP and TODO edits against the report** (every number appears in the report).
- [ ] **Step 4: Gates** `just check` (expected: the project's usual pass line, no new warnings) and `python -W error -m unittest discover -s benches/phase3 -p "test_*.py"` (`OK`).
- [ ] **Step 5: Commit** `git commit -am "docs: decide the first Phase 3 cut from the measurements"`.
- [ ] **Step 6: Gates before any PR**: AGY-CAPSTONE over the committed range (the only executable code is `make_tree.py`; point agy at the report's numbers as well and have it re-derive each rule's outcome), then AGY-TEST-AUDIT, then `just pr` only with the owner's go. Next artefact after merge: the Cut 1 (or worker pool) design spec.

## Stand-downs

- DISCARDED-BELOW-FLOOR: `docs/superpowers/plans/2026-10-30-phase3-measure.md` line 11 cites the agy reply by its `/home/user/.clavity/agy-replies/` path while a copy also exists under `.clavity/scratch/agy-replies/`: both paths are real, so no change.
- DISCARDED-BELOW-FLOOR: `.github/workflows/ci.yml:157` still names only `benches/publish/` in its comment; cosmetic, the new step runs correctly (same job, same Python 3.14, discover from the repository root, `benches/publish/` has no `__init__.py` either).
- DISCARDED-BELOW-FLOOR: Task 3 Step 1's inode figure "about 1.1 million" is 100k + 500k + 10k + 500k = 1.11 million; the 1.5x margin absorbs the rounding.

Panel record: five rounds (FINDINGS-12, -9, -7, -1, then CLEAN). Round 5 was narrow by design (the round-4 folds, the CI wiring, one whole-rules read); the other lenses were covered in rounds 1-4.
