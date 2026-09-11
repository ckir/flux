# AGY-CAPSTONE ledger

One row per completed capstone review of a committed implementation: an independent peer (agy) tore down the code a
plan produced, every finding was verified by measurement before it was folded, and the owner adjudicated GREEN. The
evidence column names things a reader can check: fold commits, and the review transcript.

| Date | Range reviewed | Rounds | Verdict | Evidence | Discarded or deferred (anti-sweep) |
|---|---|---|---|---|---|
| 2026-09-11 | `ce31690..6c11719` (lock-model plan 1, tooling; branch `model/lock-protocol`) | 3 (2 + 1 owner-authorized verification round on `dc40569..6c11719`) | GREEN, owner-confirmed at `6c11719` | Folds: `2c65020` (a `check` run must list a reachability witness), `6c11719` (stamp closes fences by the CommonMark rule; README override `just python=python model`). Transcript: agy cascade `8976a01d-55fa-4532-9021-02ad9adf8cb5`. Gate at `6c11719`: `just check` 29 run / 29 passed / 1 skipped. | CA-1 rejected (numbered steps inside fences are units by design Section 9.1). FA-1 deferred: a backtick fence opener whose info string holds a backtick; latent, 0 instances in the spec; with plan 2's stamp work. Owner ruled two design gaps into plan 2: new sibling spec sections are invisible to the stamp; `liveness` runs have no reachability guard. Round 2's peer wrote a stray copy of a committed file; it was deleted, and the verification round ran with stricter no-write rules and wrote nothing. |
