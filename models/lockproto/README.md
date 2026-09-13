# Lock-protocol model check

A TLA+/PlusCal model of Flux's target-lock protocol, checked with TLC, plus Rust probes that confirm the model's
filesystem assumptions on real operating systems. Design:
[`docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md`](../../docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md).

The work lands in several plans. Plan 1 provides the tooling: the runner, its self-test, the drift stamp and
traceability check, the filesystem probes, and CI. Plan 2 (this state) adds `FsModel.tla`, `LockProtocol.tla`, and
its first scenario, `recovery`, end to end. Later plans add the remaining scenarios of design Section 12, which
`trace.toml` lists as `planned_scenarios`, and `Claims.tla`.

## Running it

| Command | What it does | Needs |
|---|---|---|
| `just model` | runs every run in `expected.toml` | Java 11+, Python 3.11+ |
| `just model <scenario>` | runs one scenario, for example `just model selftest` | Java 11+, Python 3.11+ |
| `just model-test` | unit tests of `run.py` (recorded TLC output, no Java) and of the CI workflow's change detection | Python 3.11+ |
| `just model-stamp` | runs `just model`, then rewrites the unit hashes in `trace.toml` if every run matched | Java, Python, Rust |

`run.py` downloads `tla2tools.jar` (release and SHA-256 pinned in `run.py`) into `target/tla/`, checks its hash
before every use, and writes each run's full TLC output to `target/tla/out/<run>.log`. It exits 0 when every run
matched its expectation, 1 when one did not, and 2 for a tooling failure (Java missing, download or checksum
failure, a TLC error, or a timeout). `just check` runs the Rust side (the stamp and traceability test and the
filesystem probes) and needs no Java or Python.

In CI, `.github/workflows/model.yml` runs the scenarios when a change touches `models/`, the spec, the probes, the
`justfile`, or the workflow, and its `model-gate` job always reports. If cancelling a run ever leaves `model-gate`
queued rather than finished, cancel the run again from the workflow's page (reported as actions/runner#4411 for
matrix jobs with `if: always()`; that issue is closed, and whether it is fixed is not known).

To use another interpreter: `just python=python model` (any shell; in a POSIX shell, `PYTHON=python just model` also
works). Run one `just model` at a time in a checkout:
runs write their TLC state and logs under `target/tla/`, keyed by run name.

## Files

| File | Purpose |
|---|---|
| `expected.toml` | every TLC run and what it must report (design Section 4) |
| `configs/*.cfg` | one TLC configuration per run |
| `run.py`, `test_run.py` | the runner and its unit tests |
| `test_workflow.py` | checks the path pattern `.github/workflows/model.yml` uses to decide whether to run the scenarios |
| `testdata/` | recorded TLC output for the unit tests; `record_fixtures.py` re-records it after the TLC pin changes |
| `Smoke.tla` | runner self-test model (the `selftest` scenario); not part of the protocol |
| `FsModel.tla` | the filesystem model: objects, entries, handles, OS-native locks, crashes (design Section 5) |
| `LockProtocol.head`, `algorithm.txt`, `invariants.txt` | the sources of `LockProtocol.tla`: its header, the PlusCal algorithm, and the properties (design Sections 6.1 and 7) |
| `LockProtocol.tla` | generated: `cat LockProtocol.head algorithm.txt invariants.txt`, then `pcal.trans`; committed so `run.py` needs no translator |
| `spec-sections.stamp` | the spec file and the spec headings the model encodes (design Section 9) |
| `trace.toml` | every unit of those headings, its hash, and the labels that implement it (design Section 9.1) |

## How a run is judged

- `check` runs use `-continue`, so they explore the whole reachable state space and report every violated invariant.
  The configuration lists the scenario's safety invariants and no witness. A run passes only if the invariants
  reported violated are exactly its `open_findings`, and only if every label of every actor it runs is covered.
- `witness` runs use no `-continue` and list exactly one invariant, the negation of a fact the scenario must reach
  that no label's coverage states. They pass only if TLC stops with that invariant violated, which both proves the
  fact and yields the one trace showing how it happens.
- `liveness` runs check one temporal property (TLC does not name the property it reports violated, so a config
  lists exactly one) and every safety invariant in the config, with no symmetry; they pass with no violation other
  than their `open_findings`, and their labels must be covered too.
- `seeded` runs stop at the first violation, which must be the one named.

Reachability is proved by label coverage, not by witness invariants inside a check run. A witness invariant is
violated in every state after its path is reached and TLC has no flag that reports an invariant once, so a check run
carrying witnesses spends its budget printing the same trace: measured, 10,057 violation reports and a 309.6 MB log
with `-difftrace` already on. A label's coverage count proves the same reachability with no output at all. To get a
trace for any path, re-run that configuration with the witness as an invariant and without `-continue`.
- A run with open findings is run a second time with the findings' fix flags set (`<run>-fixed`), and must then
  report no open finding.
- TLC's deadlock check always stays on; a config may not set `CHECK_DEADLOCK`.
- Coverage is read from the FINAL `-coverage` block only: TLC prints a snapshot every minute and ends a block with
  either of two messages, so a run whose last block is unfinished is a tooling failure, never judged from an earlier
  snapshot. A label a run's actors cannot reach is listed in that run's `unreached` with its reason, and fails the run
  if it is covered after all.
- A full `just model` also unions the coverage of every run. A label no run covers must be in `deferred`, naming
  the planned scenario that will cover it (today `S96_1_backoff`, for `dirlock`, and `S240_3_putback`, for
  `breaklock`), or in `never_reached`, if no run can ever cover it. Either list fails when its label is covered.

## Bounds

Each configuration's bounds, and what they still let it explore (design Section 4). The state counts are TLC's and
do not depend on the machine; no wall-clock figure is recorded here until the controlled timing measurement.

`recovery` runs one Owner, which may crash, against the actors entitled to find and replace its lock. All four actor
kinds at once do not finish, so the check runs pair them, and each pairing is exhaustive:

| Pairing | Actors | What only this pairing explores | Distinct states, POSIX / Windows |
|---|---|---|---|
| `recovery-<platform>-check` | Owner, 2 Recoverers, with `SYMMETRY` over the Recoverers | two recoverers racing to move the same dead lock aside (240.3 step 2) | 937,335 / 1,380,999 |
| `recovery-<platform>-plain-check` | Owner, Recoverer, PlainRun | a plain rerun (21.1) meeting a lock a recoverer is working on | 1,620,690 / 2,310,021 |
| `recovery-<platform>-cleanup-check` | Owner, Recoverer, Cleanup | two movers of different kinds, both entitled to move the lock aside | 1,174,383 / 1,684,944 |
| `recovery-<platform>-plain-cleanup-check` | Owner, PlainRun, Cleanup | the plain rerun's own 240.3 path, which needs a dead cleanup lock | 1,043,058 / 1,446,348 |

- `MaxCrashes = 2` in every run. With one crash, the path where a recovering actor itself crashes was unreachable,
  so a second crash is what the crash-inside-recovery interleavings need.
- `MaxObjs` (3 to 6, one per actor) cannot bind. Every actor makes at most one exclusive create, and
  `fs.next <= Cardinality(Procs)` was checked over the whole state space of the tightest run.
- `IdentityStrength = "strong"` only. Measured, the weak-identity variant explores an identical state graph here,
  because no name in this scenario is reused.
- `LockCapability = "strong"` only. The weak capability refuses every operation under 235.1, which `breaklock`'s
  seeded run covers.
- `HostCrashes = FALSE` in every run, so only process crashes are explored. This bound is not yet justified: design
  Section 5.2 says the model explores what the next invocation finds after a host crash undoes unflushed lock-file
  operations. It is an open question for plan 2.
- The liveness run (`DeadLockEventuallyCleared`, Owner and 2 Recoverers, no symmetry) is not yet in
  `expected.toml`. Its time limit and any tightening wait for the controlled timing measurement.

## Filesystem probes

`crates/flux-platform/tests/fs_semantics.rs` holds FS-1 to FS-11 (design Section 10). Each prints the filesystem type
of its scratch directory. The OS-native lock they probe covers the whole file: `flock` on Unix, and `LockFileEx` over
the full byte range on Windows; an implementation that locks a different range would not be covered by them. A failing
probe is never weakened: reproduce it on a native local filesystem of that platform, and if it still fails, the
model's assumption is wrong and the model changes.

Measured while plan 1 was written (Windows 11 NTFS, rustc 1.98): a replacing rename onto a file that is open with
delete-sharing succeeds with `std::fs::rename` (POSIX-semantics rename) but fails with
`MoveFileExW(MOVEFILE_REPLACE_EXISTING)`, which refuses any open target. FS-6 and FS-7 test the POSIX-semantics rename,
the one the model's Windows row describes, and FS-7 prints what `MoveFileExW` does. The spec does not yet say which
API a replacing rename uses; plan 2 records this as a finding.

## Unverified assumptions

No probe can confirm these, so they are listed here rather than tested:

- weak file identity and case folding on FAT32/exFAT (no such filesystem on CI runners), including that, having no
  journal, they may lose entry operations out of order after a host crash;
- both crash rules of design Section 5.2, including which unflushed writes and entry operations survive a host
  crash (no probe can cut power; the rules follow the platforms' documented guarantees, spec Sections 166 to 168);
- whether an entry created or removed during a directory listing is returned (the model allows either);
- filesystems beyond the modelled variants (SMB, NFS), and clock behaviour beyond the liveness oracle.
