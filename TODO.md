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
- [ ] Repository housekeeping: enable GitHub private vulnerability reporting (see
      [SECURITY.md](SECURITY.md)), and decide whether `main` gets branch protection.
      `UNVERIFIABLE` 2026-09-22: both halves are GitHub settings, which nothing in the tree
      records, so no closure here could be re-checked from the repository alone. Observed at the
      time: `required_status_checks.strict` is `true`, so branch protection is on.

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
      `UNVERIFIABLE` 2026-09-22: completion is the state of one machine, which the repository
      cannot record. Observed at the time: `command -v cargo-mutants` says INSTALLED, and the tool
      is declared in `.claude/recommended-tools.json`.
- [ ] Run `lefthook install` in each clone (or add it to a bootstrap recipe)
- [ ] Replace the placeholder `benches/copy.rs` once there is a pipeline to measure
- [ ] Replace the placeholder test in `tests/integration/mod.rs` with the first
      real case from spec §68.2

## Repository and CI hygiene

Promoted from the local anomalies inbox (triage of 2026-09-14); each was re-measured on `origin/main` that day.

- [ ] **No MSRV job.** Every `ci.yml` job uses the stable toolchain and Dependabot auto-merges cargo minor and
      patch bumps, so a dependency raising its MSRV past the workspace's `rust-version` (1.85) is not caught.
      Add a job running `cargo +1.85 check --workspace --all-targets`.
- [ ] **`ci.yml` has no `permissions:` or `concurrency:` block**, so its jobs get the repository's default token
      scope and superseded pull-request runs are not cancelled. Add `permissions: contents: read` and a
      concurrency group.
- [ ] **`_typos.toml` excludes the spec by its literal file name**, so renaming the spec (a new version) makes the
      typos job scan it for the first time. Match the spec by a pattern instead.
- [ ] **An empty release pull request is indistinguishable from a real one.** `release-plz` opens one on
      every push to `main`, and `release-plz.toml` deliberately skips `spec`, `design`, `plan`, `model`,
      `docs`, `skills` and `test` — which is most work in this repository — so a typical push produces a
      version bump with **zero** changelog entries. Four have been closed for that reason so far (#22 was a
      duplicate of `v0.1.1`'s own content; #24, #26 and #29 were empty), and the next one will look exactly
      as actionable as a real release. The distinguishing check is one command,
      `gh pr diff <n> | grep -c '^+- '`, returning zero — so make it a gate rather than a habit: a job on
      release pull requests that fails when the changelog diff adds no entries, or a `release-plz` setting
      that declines to open one. A habit is not a control; the person who merges it will not be the person
      who learned the habit.

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

From plan 2, now merged (was PR #3). Each was verified by measurement; M5 to M8 come from the plan-2 test
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

## Lock-model follow-ups (plan 3)

Triage of the local anomalies inbox, 2026-09-21: six capstone rounds, two test-audit rounds, and the
v0.1.0/v0.1.1 release. Each was verified by measurement; the inbox entry carried the evidence and was deleted
by the commit that added this section.

**One re-measurement covers the first three.** Each is a model change needing a fresh full run, and
`breaklock-remote` alone was ~2.5h before its split, so batch them rather than paying three times.

- [ ] **`pendingUnlink` could be a boolean.** Every use tests it only against `NoObj`; its captured identity
      became dead when `LandUnlink` moved to resolving the lock NAME at the landing. As a per-process object id
      it carries `MaxObjs + 1` values per process against two for a boolean, and the remote checks run at 60M+
      states — a real state-space reduction, not tidiness.
- [ ] **`tornRead` is set at one of the two labels that read a record.** The classify read sets it; `S240_5_s5`
      does not, so a torn read during a takeover goes unrecorded and the ghost under-reports its own declared
      meaning. It feeds only `NeverTornRead`, which already fires from the classify path, so it buys no coverage
      today — do it while the states are being re-measured anyway.
- [ ] **`IdentityStrength = "weak"` is dead across every run.** All 74 runs pin `"strong"` while `FsModel.tla`
      implements the weak value at four sites. The README justifies the pin only for `recovery`, and the one
      protocol decision that discriminates on it (`algorithm.txt:434`) is in the Breaker/TakeOver path, which
      `recovery` has no actor for. Give it a `breaklock`-scoped justification, or drop the value.

**Coverage, where the gate is weaker than it reads.**

- [ ] **Build the BRANCH-granular coverage union — it is debt, not a limit.** TLC's `-coverage 1` is
      expression-granular (message 2221, nested, zeros included) and the repo's own fixture proves the shape at
      `models/lockproto/testdata/smoke_check_coverage.out:109`. `run.py` already parses that message and
      deliberately discards it, so a branch union is buildable from logs on disk at no TLC cost. Settle two things
      first: the nodes key off `line,col` of the GENERATED module (which `--check-translation` pins), and the
      shape TLC emits for a PlusCal `if`/`else if` INSIDE a label was never measured. This blind spot has already
      produced two findings.
- [ ] **Say which `never_reached` claims an exhaustive run supports.** 52 of 77 runs halt at a counterexample, so
      their coverage is a PREFIX. Positive coverage from a prefix is sound and `uncovered` fails closed, but the
      one check that fails OPEN — a label a halting run would have covered later — is blind across two-thirds of
      the suite. The exhaustive check and liveness runs alone carry those claims; the union could mark each claim
      supported or not, turning a suite-wide caveat into a per-claim one.
- [ ] **Commit the union verdict as an artifact.** "103 labels covered, 0 never_reached, 1 deferred" exists only
      in prose nobody can confirm or refute. A `union.txt` beside `expected.toml`, or the verdict pinned as a
      fixture, makes the most-cited number in this work re-readable. Cheapest item here, and it answers the
      review's structural finding: of ~84 factual claims in this branch's commit messages, ~45 are permanently
      unverifiable — every state count, every timing, all sixteen CI run ids — because GitHub log retention is 90
      days and commit messages are forever.

**Seeds that pin their own seed rather than the property.** Each needs TLC, so each is CI-only; the mutant is
stated so the next audit starts from a prediction rather than a hunt.

- [ ] **`FsOk`'s other two conjuncts are still indistinguishable from `TRUE`.** `FsOk` is
      `NoDoubleOpen /\ LockImpliesHandle /\ IdsNotReused`, and `SEED_FS_LOCK_WITHOUT_HANDLE` breaks only the
      middle one. Mutant `FsInvariants(fs) == LockImpliesHandle(fs)` is predicted GREEN.
- [ ] **`Classifiable`'s seed pins the seed.** `SEED_FS_ALIEN_CONTENT` writes `NoProc` specifically, so the
      mutant `Classifiable == \A o \in Objs : fs.content[o] # NoProc` is predicted GREEN — the invariant gutted
      to a NoProc-check and still satisfied by its own seed.
- [ ] **Two of `RefusalJustified`'s three `lostLock` sites remain unseeded.** `SEED_CHECK_REFUSES_UNTOUCHED`
      (added 2026-09-21) pins `S99_check`, because a seeded run halts at the first violating state. A mutant
      hardwiring `refusedOk` at `S99_release_check` or `S21_1_s3` still survives; closing it needs one run per
      site — the same shape that needed five `NeverRefusedUnsafe` witnesses.
- [ ] **`PlainNeverOwnsUncertain`'s ghost is wired to one label.** `touchedUncertain` is assigned at
      `algorithm.txt:299` and nowhere else, so only the move-aside is instrumented against an invariant that also
      covers removing, renaming and creating.
- [ ] **`ForeignUntouched`'s content half is accepted as unreachable by reasoning.** The plan-2 audit
      dispositioned it so and re-validation at HEAD agrees, but the argument is reasoned, not measured. Run its
      mutant alongside the others rather than on its own.

**Smaller, each independently shippable.**

- [ ] **The stamp cannot see an acceptance test.** `spec-sections.stamp` lists twelve headings, and §259.14 —
      which opens "A conforming implementation must test at least:" and is therefore normative — is not among
      them. So a rule can be amended under a stamped heading while the acceptance item testing it keeps demanding
      the old outcome, and `model_stamp` passes. That is exactly what `788267a` did. Stamping 259.14 is cheap and
      would have caught it.
- [ ] **The model cites RFC 7530 for a sentence it does not contain.** `models/lockproto/algorithm.txt:44` says
      "the server resolves that name when it EXECUTES the call (RFC 7530, design Section 5.3)". §16.26 supports
      only the name-based half — *"removes (deletes) a directory entry named by filename"* — and says nothing
      about timing. Measured with a positive control: `REMOVE` 14 hits, `filename` 3, `directory` 15, against
      `retransmit` / `resolve` / `executes` / `stale` all 0. The claim is sound as an INFERENCE; the citation
      overstates it. Prose, not behaviour, but load-bearing — this reading forced three remote windows to be
      re-measured. The fix edits a PlusCal comment, so it needs `pcal.trans` + SANY + `--check-translation`.
- [ ] **A halting run's state count is not reproducible.** `run.py:1138` runs TLC with `-workers auto`, so a run
      stopping at the first violating state stops wherever the workers had got to: the same model on two commits
      differing by one docs line reported 6,105 against 6,976 states. 52 of 77 runs halt. Either use `-workers 1`
      for halting runs, or stop quoting exact counts for them — they evidence "the seed fires, roughly this
      deep", never a number.
- [ ] **`S240_5_s6`'s two refusing branches are indistinguishable to every gate.** Both set
      `refused := "RESTART"` with identical successor state, `RefusalJustified` no longer reaches that label, and
      coverage is label-granular — so nothing can witness that the "another file is there" case is reached.
      Closed by the branch-granular union above, if that lands.
- [ ] **`trace.toml`'s exemption for family 259 states the wrong reason.** It reads "normative precedence between
      spec sections is outside the lock-protocol model's scope", but §259.13 ranks RUNTIME SIGNALS, not spec
      sections. The exemption may well be right; its stated reason describes something the section does not say.
- [ ] **Make the SEED-flag complement a test.** A review seat computed it with a scratch script — collect
      `SEED_[A-Z0-9_]+` from `LockProtocol.tla`, collect `SEED_... = TRUE` across `configs/*.cfg`, subtract —
      which is how "the complement is empty" came to be measured rather than asserted. It belongs in
      `test_run.py`.

## Closed

Triaged 2026-09-22. Kept here rather than deleted: the evidence for a closure belongs where the
item was, not only in a commit message.

- `DONE` **Model-check the lock protocol before implementing it.** Done by lock-model plans 1 to 3.
  The item asked for two recoverers, two `--break-lock` takeovers, a stalled prior owner and a plain
  run, checking that at most one operation ever owns a target: `recovery` runs `Recoverers = {r1, r2}`,
  `breaklock` runs `Breakers = {b1, b2}`, crashes model the stalled owner, `PlainRuns` covers the plain
  run, and `SingleWriter` is the at-most-one-owner invariant — 5 occurrences in
  `models/lockproto/invariants.txt`, checked across the scenarios `run.py --list-jobs` reports. The item
  offered TLA+ *or* `loom`/`shuttle` as alternatives; TLA+ satisfies it.
- `OVERTAKEN` **`.antigravityignore` is untracked in the primary working tree.** It is tracked on `main`:
  `git ls-files .antigravityignore` returns it. It reads as untracked only in `E:/Rust/flux`, which sits
  on the stale `model/lock-protocol` branch — the remedy is to move that tree, and there is nothing to
  change in the repository.
