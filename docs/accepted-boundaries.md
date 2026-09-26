# Accepted test boundaries

Behaviours deliberately NOT covered through a given harness, because covering them there needs a brittle
mock or a seam that does not exist, and something else compensates. Written by AGY-TEST-AUDIT; each entry
is do-not-re-raise ONLY while its compensation still exists - a later audit re-checks the anchor first,
and an entry whose compensation is gone becomes a live gap again. Closed entries are removed. Deferred
gaps do not live here (they ride `.clavity/local-anomalies.md` to a tracked item).

One section per audited change, newest at the bottom.

## Cut 5 - the `flux copy` CLI for trees (test audit 2026-09-26)

- **The weak-identity warning lines are not wired end-to-end.** `crates/flux-cli/src/main.rs` prints
  `report::warning_lines(&outcome.warnings)` after a tree copy and `report::file_warning(..)` after a
  single-file copy; deleting either loop leaves every test green (measured: 45/45 on Windows). An
  end-to-end test needs a filesystem that reports a weak or unavailable identity, and the build host's
  filesystems report Strong ones; reaching it otherwise needs a seam that lets `main` run against
  `FaultFs`, which this cut does not have.
  **Compensation:** the text of both forms is pinned by `report.rs`
  `one_warning_per_weak_volume_and_one_for_unavailable` and `a_single_file_warning_names_the_target`;
  the wiring is one `for` loop and one `if let` in `copy`, each writing through the same `err` as every
  pinned line. Owner-accepted 2026-09-26.
