# Lock-protocol model check

A TLA+/PlusCal model of Flux's target-lock protocol, checked with TLC, plus Rust probes that confirm the model's
filesystem assumptions on real operating systems. Design:
[`docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md`](../../docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md).

The work lands in three plans. Plan 1 (this state) provides the tooling: the runner, its self-test, the drift stamp
and traceability check, the filesystem probes, and CI. Plan 2 adds `FsModel.tla` and `LockProtocol.tla`; plan 3 adds
`Claims.tla`.

## Running it

| Command | What it does | Needs |
|---|---|---|
| `just model` | runs every run in `expected.toml` | Java 11+, Python 3.11+ |
| `just model <scenario>` | runs one scenario, for example `just model selftest` | Java 11+, Python 3.11+ |
| `just model-test` | unit tests of `run.py` (recorded TLC output, no Java) | Python 3.11+ |
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
| `testdata/` | recorded TLC output for the unit tests; `record_fixtures.py` re-records it after the TLC pin changes |
| `Smoke.tla` | runner self-test model (the `selftest` scenario); not part of the protocol |
| `spec-sections.stamp` | the spec file and the spec headings the model encodes (design Section 9) |
| `trace.toml` | every unit of those headings, its hash, and the labels that implement it (design Section 9.1) |

## How a run is judged

- `check` runs use `-continue` and must report exactly their `violated` witnesses plus their `open_findings`; a
  `check` run lists at least one witness, so a model that reaches nothing cannot pass.
- `liveness` runs check one temporal property (TLC does not name the property it reports violated, so a config
  lists exactly one) and every safety invariant in the config, with no symmetry; they pass with no violation other
  than their `open_findings`.
- `seeded` runs stop at the first violation, which must be the one named.
- A run with open findings is run a second time with the findings' fix flags set (`<run>-fixed`), and must then
  report no open finding.
- TLC's deadlock check always stays on; a config may not set `CHECK_DEADLOCK`.

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
