# TODO

Near-term work. Release-level scope lives in [ROADMAP.md](ROADMAP.md).

## Open decisions

- [ ] **Final dependency selection** (spec §67). `[workspace.dependencies]` pins
      versions for the candidate crates but no member crate depends on any of them
      yet. Each must be judged on performance, correctness, portability,
      maintenance, licensing and API stability before adoption.
- [ ] **Directory walker.** Spec §67 lists `jwalk`, but crates.io currently ships
      it described as "Use `dua-core` instead" and it has not moved since 0.9.0.
      Decide between `walkdir`, a maintained parallel walker, or our own — the
      deterministic-ordering requirement (spec §7) may force a custom one anyway.
- [ ] **Persistent state format** for the topology store and operation manifest
      (spec §17, §19). Must scale past RAM and survive a crash mid-write.
- [ ] **Model-check the lock protocol before implementing it** (spec §96.1, §99,
      §240.3, §240.5, §259.6, and the claim/`COMMIT` ordering of §241.5 and
      §182). The V16 adversarial review stopped at its six-round cap still finding
      defects there each round, and round 6's fixes went unreviewed. Model two
      recoverers, two `--break-lock` takeovers, a stalled prior owner, and a plain
      run (for example with TLA+, or `loom`/`shuttle` against the Rust
      implementation) and check that at most one operation ever owns a target.
- [ ] Repository housekeeping: enable GitHub private vulnerability reporting (see
      [SECURITY.md](SECURITY.md)), and decide whether `main` gets branch protection.

## Phase 2 — portable copy (next)

- [ ] `flux-fs`: define the portable filesystem trait surface
- [ ] `flux-platform`: Linux / macOS / Windows implementations behind it
- [ ] `flux-core`: single-file copy with metadata preservation
- [ ] `flux-core`: recursive directory copy
- [ ] `flux-core`: error taxonomy and mapping (spec §68.1 lists error mapping as
      a required unit test)
- [ ] `flux-core`: statistics collection
- [ ] `flux-cli`: wire `flux copy` to the above
- [ ] Integration tests: single file, directory, nested directory, multiple
      sources, zero-byte files, Unicode, spaces, newlines (spec §68.2)

## Scaffolding follow-ups

- [ ] Install `cargo-mutants` (`cargo binstall -y cargo-mutants`) — it is the one
      tool in `.claude/recommended-tools.json` not yet present on the dev box
- [ ] Run `lefthook install` in each clone (or add it to a bootstrap recipe)
- [ ] Replace the placeholder `benches/copy.rs` once there is a pipeline to measure
- [ ] Replace the placeholder test in `tests/integration/mod.rs` with the first
      real case from spec §68.2

## Repository and CI hygiene

Promoted from the local anomalies inbox (triage of 2026-09-14); each was re-measured on `origin/main` that day.

- [ ] **`build:` commits vanish from the changelog.** `cliff.toml` sets `filter_unconventional = true` and has
      parsers for `feat fix docs perf refactor test ci chore` only, while `CONTRIBUTING.md` lists `build` as an
      allowed type, so `build: ...` is silently omitted from `CHANGELOG.md`. Add a `^build` parser or drop the type.
      Check: commit `build: x`, run `git-cliff --unreleased`, confirm the entry.
- [ ] **No MSRV job.** Every `ci.yml` job uses the stable toolchain and Dependabot auto-merges cargo minor and
      patch bumps, so a dependency raising its MSRV past the workspace's `rust-version` (1.85) is not caught.
      Add a job running `cargo +1.85 check --workspace --all-targets`.
- [ ] **`ci.yml` has no `permissions:` or `concurrency:` block**, so its jobs get the repository's default token
      scope and superseded pull-request runs are not cancelled. Add `permissions: contents: read` and a
      concurrency group.
- [ ] **`_typos.toml` excludes the spec by its literal file name**, so renaming the spec (a new version) makes the
      typos job scan it for the first time. Match the spec by a pattern instead.

## Spec gaps from the filesystem probes

Measured by the lock-model probes (`crates/flux-platform/tests/fs_semantics.rs`, branch `model/lock-protocol`)
against spec V16.

- [ ] **Windows replacing rename is unnamed.** `std::fs::rename` (POSIX-semantics rename) replaces a target that
      is open with delete-sharing, but `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` fails with "Access is denied" whenever
      the target is open. §241.5 names `MoveFileEx` for the no-replace publish only; the spec should name the API
      of the replacing rename, which is what the model's Windows row assumes.
- [ ] **Windows file identity source.** §107 and §109.1 give only strength classes. The 64-bit file index is not
      guaranteed unique on ReFS (Dev Drives are ReFS); a strong identity needs `FILE_ID_INFO` (volume serial plus
      128-bit file id) from `GetFileInformationByHandleEx`.
- [ ] **WSL 9p mounts break two lock assumptions.** On `/mnt/c` (v9fs), `renameat2(RENAME_NOREPLACE)` onto a free
      name fails with `EINVAL`, and a file renamed while open is listed but cannot be `stat`ed until the handle
      closes. Lock-capability detection (§96.1, §99) must treat such a mount as lacking both, and fall back to the
      directory lock or refuse.

## Lock-model follow-ups

On branch `model/lock-protocol-wip` (PR #3). Each was verified by measurement; M5 to M8 come from the plan-2 test
audit (`docs/agy-test-audit-ledger.md`), where the owner deferred them as minor.

- [ ] **Cache the TLC jar in CI.** Each `model.yml` scenario job downloads and checksums `tla2tools.jar` on its own;
      an `actions/cache` step keyed on the pinned tag removes the repeated downloads and their failure chances.
- [ ] **`--no-renames` is unpinned.** Removing it from `model.yml`'s change detection keeps every test green, and
      then a pull request that only moves a file out of `models/` skips the model jobs. Test the step itself, or move
      change detection into `run.py`.
- [ ] **`open_findings[].tracking` is not validated.** Design Section 4 says it names a `TODO.md` or anomalies
      entry, but `run.py` accepts any non-empty text. Check it at load time, or restate the field as free text.
- [ ] **cp1252 stdout.** Python on Windows writes a piped stdout as cp1252, so a non-ASCII `reason` or TLC
      message crashes `run.py`. Call `sys.stdout.reconfigure(encoding="utf-8")` in `main`, or set
      `PYTHONIOENCODING` in the workflows.
- [ ] **M5, `NeverTornRead` is placement-blind.** Setting `tornRead` unconditionally at `S240_1_read` keeps the
      witness violated. Replace the ghost with a state predicate over `seenRec` and `classified`.
- [ ] **M6, `recoveredAfterCrash`'s guard is always true** at `S240_3_s5`, so moving or dropping it changes no
      run; the witness restates the label's coverage.
- [ ] **M7, host-crash net holes.** Flipping the comparison in the `hostCrashChangedLock` update, making
      `FsHostCrash` always keep new content, emptying `Unflushed`, or `FsInvariants == TRUE` all leave the host-crash
      runs green; the past-ids half of `IdsNotReused` is vacuous under strong identity.
- [ ] **M8, the liveness consequent can be weakened undetectably.** Widening `UncertainReported` or making
      `DeadLockEventuallyCleared` trivially true keeps its witness violated and the liveness run green. Add a recovery
      seed (a Recoverer that gives up) that must violate the property.
