# Cut 6: the lock-model debt the Rust lock would be checked against

**Status:** approved by the owner 2026-09-26 (brainstorming; AGY-FIRST on scope, on the design forks, and one
AGY-NEGOTIATE round on declaring seeds). **Base:** `main` at `878af1f`, plus the sequencing decision
`docs/superpowers/specs/2026-09-26-sequencing-after-cut-5-design.md` on this branch.

## Goal

Before cut 7 implements the destination lock in Rust against the TLA+ model in `models/lockproto/`, close the
places where that model's guarantees about the lock cannot be told from `TRUE` or are blind to a mutation. Cut 6
changes the MODEL'S NET ONLY - invariants, witnesses, seeds, runs, and the runner's coverage gate. It changes no
protocol behaviour; any item whose fix would change what the protocol does stops and goes to the owner.

**Done means:** each item below is either CLOSED - its named mutant, which leaves the suite green today, is
killed by a named run, measured - or NARROWED, with its residue written into `TODO.md` in place of the old entry.

## Scope - 13 items

The sequencing decision placed 11 `TODO.md` items in cut 6. Two hygiene items were pulled in by the owner
because two of the 11 cannot close without them: "`S240_5_s6`'s two refusing branches" closes only through the
branch-granular union, and M7's past-ids half is vacuous under strong identity.

## Principles (owner-approved)

1. **Reachability through the branch-granular union, not new ghosts.** A ghost added only to satisfy a gate is
   model pollution; the union reads coverage TLC already emits.
2. **Non-vacuity through one seed per conjunct or site.** A seeded run halts at the first violating state, so a
   seed that breaks two things at once lets a single gutting survive. Each seed must violate the PROPERTY, not
   pin its own value (the `Classifiable` lesson).
3. **Every closure is proven by its mutant.** The mutant is applied, the named run goes RED, the mutant is
   reverted; a closure with no red run is not closed.
4. **New `SEED_*` constants are added by a one-off script** to every config and every run's `constants` in
   `expected.toml` / `expected-extended.toml`, as `FALSE` except in their own seeded run. The runner's
   exact-match check (`run.py:859-868`) is unchanged. Agreed with agy after one negotiation round: committed
   configs stay complete, so each still runs under raw TLC and the TLA+ Toolbox, which reject an unassigned
   constant.

## The items

### 1. The branch-granular coverage union (first: items 2, 3 and 6 depend on it)

Today `parse_coverage` keeps only TLC's 2772 action lines, so the coverage gate is LABEL-granular, and
`parse_cost_nodes` (`run.py:995`) already parses the 2221 expression lines but keeps `(module, line, col, count,
depth)` and drops the end of the span.

- **Measure first.** No fixture shows TLC's 2221 output for the shapes this cut relies on. Extend
  `testdata/CoverageFixture.tla` with (a) a label holding `with (i \in ...) { if ... else if ... else ... }`
  with one arm dead, and (b) an operator in a second module with a `LET` whose `CASE` has a dead arm (the shape
  of `FsModel.tla`'s `FsHostCrash`, whose `"old"` and torn picks are `CASE` arms inside `LET newContent(o)`).
  Re-record with `testdata/record_fixtures.py` (a short TLC run; needs Java). The recorded output settles:
  whether each arm gets its own node; whether an arm's count means its body executed (not merely that its guard
  was evaluated); how nodes under `\E`/`with` are counted; and whether an arm producing only duplicate successors
  still counts. **If the measurement shows a count does not mean the body ran, items 2, 3 and 6's union-based
  closures stop and return to the owner** - they rest on that meaning.
- **Data model.** `parse_cost_nodes` keeps the full span (`module, line, col, end_line, end_col, count, depth`);
  nodes are grouped under the most recent 2772 action. Its callers (`run.py:1369`, `test_run.py:2085-2110`)
  follow the shape change.
- **Expectations.** A top-level `branches` list in `expected.toml`, entries
  `{ label = "...", arm = "<verbatim text of the arm's first line>", reason = "..." }`. Resolution at load time
  finds the arm text inside that label's action in the GENERATED module (`--check-translation` pins it), so an
  anchor that also appears elsewhere - `IF ident # tobj[self]` occurs in both `S240_5_s4` and `S240_5_s6` - is
  unambiguous. Missing or ambiguous anchor: fail closed. Summed count over the union's logs of zero: fail.
- **Where it is judged.** In `union_from_logs` (`run.py:1251`), beside the label union, so both `just model` and
  the CI `union` job (`--union-from`) enforce it. It costs no TLC time.
- **Tests (`test_run.py`).** Arm parsing and depth from the new fixture; two logs where only one covers an arm
  (passes); an arm at zero (fails); a missing anchor and an ambiguous anchor (fail closed); an anchor scoped away
  from the same text in another label.
- Out of this cut: the companion hygiene item "Say which `never_reached` claims an exhaustive run supports".

### 2. `S240_5_s6`'s two refusing branches

Both arms set `refused := "RESTART"` with identical successors (`algorithm.txt:549` and the `ident # tobj`
arm below it), so no state predicate can tell them apart. **Closure:** a `branches` entry for the `ident # tobj`
arm. **Mutant:** short-circuit that arm (make its guard `FALSE`) - the union fails.

### 3. `tornRead` is not set at `S240_5_s5`

The takeover's read of a torn record falls through and continues (`algorithm.txt:492-494`); the omission is
deliberate (`:487`), and with item 4 the ghost is gone anyway. **Closure:** a `branches` entry for the arm that
continues on a non-record read at `S240_5_s5`. **Mutant:** make a torn read at `S240_5_s5` refuse or restart -
the arm's count goes to zero and the union fails. The `TODO.md` entry is closed by this item and item 4
together.

### 4. M5 - `NeverTornRead` is placement-blind

`tornRead` is set at `algorithm.txt:144`, before the judgement, so an unconditional set keeps the five witness
runs violated. **Closure:** delete `tornRead` (`algorithm.txt:35`, `:144`); redefine
`NeverTornRead == ~\E p \in Procs : seenRec[p] = Torn /\ classified[p] = "uncertain"` (the `TODO.md` entry's own
suggestion). **Mutants:** (a) judge Torn as anything but uncertain (the `SEED_TORN_AS_FOREIGN` shape) - the five
`NeverTornRead` witness runs stop violating and fail; (b) drop `seenRec[self] := seen` (`:143`) - same.

### 5. M6 - `recoveredAfterCrash`'s guard is always true

Without seeds `Replaceable` needs "dead", which the oracle offers only for a crashed owner, and `crashed` is only
ever set `TRUE`, so the guard at `algorithm.txt:374` is always true where it runs. **Closure (owner, agy
aligned):** turn it into a safety claim. Replace the witness ghost with `replacedLive`, set at `S240_3_s5` when the
replaced lock's owner is a process that has NOT crashed; add `ReplacedOnlyDead == ~replacedLive` to every check
run's invariants; add `SEED_RECOVER_LIVE` (a `Replaceable` disjunct admitting a live owner) with a seeded run
expecting `ReplacedOnlyDead` violated. The five `NeverRecoveredAfterCrash` witness runs are retired: they restated
`S240_3_s5`'s label coverage (the `TODO.md` entry). **Mutants:** drop the `crashed` test in the ghost's guard (the
seeded run fails: no violation); delete the set (same). **Measure first:** whether the seeded path reaches
`S240_3_s5` past a live owner's OS-native lock. If it cannot, the seed moves to where the live owner is judged, or
the item returns to the owner.

### 6. M7 - host-crash net holes

- **Flipped comparison** in `hostCrashChangedLock`'s update (`algorithm.txt:1023`). A witness run passes on any
  violation, so a flip that fires on every host crash still passes. **Closure:** a NEGATIVE CONTROL - a check run
  in which a host crash cannot change the lock, listing `NeverHostCrashChangedLock` as an invariant that must
  HOLD. The design fixes the configuration in the plan (a constant permitting a host crash only before any actor
  step works because the initial lock is durable, `FsModel.tla:94`). **Mutant:** the flip - the control fails.
- **"Always keep new content" and "`Unflushed == {}`".** Both remove the old/torn outcomes of a host crash
  (`FsModel.tla`, `FsHostCrash`'s `CASE` arms; `Unflushed` feeds them). **Closure:** `branches` entries on the
  `"old"` and torn arms (resting on item 1's measurement for `CASE`-in-`LET` in another module). **Mutants:** the
  two above - the arms' counts go to zero.
- **`FsInvariants == TRUE`.** Predicted already killed by `SEED_FS_LOCK_WITHOUT_HANDLE`
  (`recovery-posix-seeded-SEED_FS_LOCK_WITHOUT_HANDLE`, `expected.toml:82-89`, added after the M7 entry was
  written). **Closure:** run the mutant and record the red run; nothing else if it is red.
- **The past-ids half of `IdsNotReused`** (`FsModel.tla:345`). **Closure:** `SEED_FS_PAST_UNALLOCATED`, adding
  `fs.next + 1` to a `past` set; inert to the protocol under strong identity, because `past` is read only under
  weak (`FsModel.tla:266`). **Mutant:** drop the `past` conjunct - its seeded run fails.

### 7. `FsOk`'s other two conjuncts

`FsOk` is `NoDoubleOpen /\ LockImpliesHandle /\ IdsNotReused` (`FsModel.tla:348`); only the middle one has a seed.
**Closure:** `SEED_FS_DOUBLE_HANDLE` (a second handle record for the same process and object, differing in `del`)
kills dropping `NoDoubleOpen`; `SEED_FS_ENTRY_UNALLOCATED` (`entries[P][c] := fs.next + 1`, when
`fs.next < MaxObjs`) kills dropping `IdsNotReused`'s entries conjunct; item 6's `SEED_FS_PAST_UNALLOCATED` kills
its past conjunct. One run each. **Residue (owner):** on POSIX a real double open is invisible, because `handles`
is a SET and `share` forces `del = TRUE` (`FsModel.tla:69`, `:131`, `:145`), so a duplicate collapses. Changing
the representation is not in this cut; the residue goes to `TODO.md`.

### 8. `Classifiable`'s seed pins the seed

`SEED_FS_ALIEN_CONTENT` writes `NoProc` (`algorithm.txt:1063`), so `Classifiable` gutted to a `NoProc` check stays
green. **Closure:** a second, separate seeded run, `SEED_FS_ALIEN_RECORD`, writing
`[tag |-> "record", op |-> NoProc, kind |-> "operation"]` - `IsRecord` accepts it (`FsModel.tla:53`) but it is not
in `Records`. Not one nondeterministic seed: TLC stops at the first violation, so the `NoProc` branch alone would
keep the pin mutant green. **Mutants:** the `NoProc` pin, and a tag-only gutting - both fail the new run.

### 9. M8 - the liveness consequent can be weakened

**Closure (owner, agy aligned):** one seed, `SEED_RECOVERER_GIVES_UP`, at `rec_decide`'s
`if (Replaceable(self)) { goto rec_recover; }` (`algorithm.txt:810`): the Recoverer ends without acting and with
`refused` left `"none"`, so no invariant fires first. A seeded temporal run
(`run.py:831` already supports one) with `PROPERTY DeadLockEventuallyCleared` expects it violated. **Mutants:**
`DeadLockEventuallyCleared == TRUE`; `UncertainReported == TRUE`; a widening that admits `"none"`. A widening that
admits only `"RESTART"` is not caught - stated as a residue.

### 10. `RefusalJustified`'s two unseeded `lostLock` sites

`SEED_CHECK_REFUSES_UNTOUCHED` pins `S99_check` only: with it on, `S99_check` always refuses, so
`S99_release_check` is never reached, and `S21_1_s3` needs a Breaker the seeded run does not have. **Closure:**
two per-site seeds written inline at the guards of `S99_release_check` (`algorithm.txt:659`) and `S21_1_s3`
(`:926`), so `StillOwned` keeps its three readers - one `recovery-posix-seeded-...` run and one
`breaklock-posix-seeded-...` run. **Mutants:** hardwire `refusedOk[self] := TRUE` at each site; each site's run
fails.

### 11. `PlainNeverOwnsUncertain`'s ghost is wired to one label

`touchedUncertain` is set only at `algorithm.txt:308` (`S240_3_s2`, the move-aside); the `TODO.md` entry cites
`:299`, which is `robj := LockObj;`. **Closure (owner, agy aligned): narrow.** Rewrite the invariant's comment
(`invariants.txt:39-40`) to claim only what is instrumented - a process never moves aside a lock it judged
uncertain - and write the rest (removing, renaming, overwriting, creating) into `TODO.md` as a residue.
**Measure first:** whether a Breaker, which calls `Recover` through `brk_recover` (`algorithm.txt:924-925`), can
set the ghost. The comment says the property is about a process WITHOUT `--break-lock`; if a Breaker can set it,
the comment is wrong and the narrowing says which actors it covers.

### 12. `ForeignUntouched`'s content half

`SEED_RECOVER_FOREIGN` breaks only the identity half, and the content half is "unreachable by reasoning". Every
exhaustive check lists `ForeignUntouched` and starts from a `Foreign` state, which measures that the half HOLDS;
nothing measures that it CAN FAIL. **Closure:** `SEED_TAKEOVER_FOREIGN` - a Breaker that classified the lock
"foreign" is sent to takeover (`S21_1_restart_decide` to `brk_takeover`), so `S240_5_s6_write_begin` writes the
foreign object while it is still the lock object: the content half breaks and the identity half holds. One
`breaklock-posix-seeded-...` run. **Mutant:** drop the content conjunct - the run fails.

### 13. Weak identity is dead

Every run pins `IdentityStrength = "strong"`. The design doc dropped weak identity because "the scenario's only
identity query is on `BrokenOf(self)`" (`2026-09-11-lock-protocol-model-check-design.md:1125`); that is stale -
the recovery path's `S240_3_s4_lock_verify` queries `FsIdentityChoices(fs, P, LockName)` (`algorithm.txt:348`),
and `LockName` is re-created after a move-aside. **Closure:** weak-identity check runs for the recovery and mixed
scenarios on POSIX, where a crash between re-creations gives `past[LockName]` two ids. **Mutant:** `past[d][c] = {}`
(the gutting the strong seed of item 6 cannot see). **Measure:** the weak runs' state counts and times, recorded
in `expected.toml` and the README like every other run; the design doc's rationale is corrected. If a weak run
exceeds the CI budget, the plan splits it the way `breaklock-remote` was split.

## Run inventory (new runs)

Seeded, halting at the first violation: `SEED_RECOVER_LIVE`, `SEED_FS_PAST_UNALLOCATED`, `SEED_FS_DOUBLE_HANDLE`,
`SEED_FS_ENTRY_UNALLOCATED`, `SEED_FS_ALIEN_RECORD`, the two per-site `RefusalJustified` seeds,
`SEED_TAKEOVER_FOREIGN`. Seeded temporal: `SEED_RECOVERER_GIVES_UP` (a full liveness exploration; the existing
`recovery-posix-liveness` run is the nearest recorded cost). Negative control: one host-crash check. Weak identity:
the recovery and mixed POSIX checks. Retired: the five `NeverRecoveredAfterCrash` witness runs. Changed: every
check run (new invariant `ReplacedOnlyDead`; `tornRead` deleted), so every state count is re-measured in one CI
Model run, as `TODO.md` asks ("batch them rather than paying three times").

## Verification

- `test_run.py` passes, including the union tests above; `run.py --check-translation` passes; the stamp tests
  pass.
- Each item's mutant is applied, its named run goes red, and the mutant is reverted. The record (mutant, run,
  outcome) goes into the plan's execution notes and the test-audit ledger.
- One full CI Model run on the final commit, green; its run ID and every changed state count recorded.

## Residues, stated (written to `TODO.md` by this cut)

- POSIX double open invisible to `NoDoubleOpen` (item 7).
- M8: a widening admitting only `"RESTART"` (item 9).
- `PlainNeverOwnsUncertain` covers only the move-aside (item 11).
- The companion hygiene item on `never_reached` claims stays open.

## Stand-downs

(Filled by the adversarial panel.)

## Consult record

- **Scope** (brief `.clavity/seams/cut6-scope.md`): agy proposed pulling both hygiene items in rather than adding a
  ghost; the driver verified `parse_cost_nodes` exists (`run.py:995`) and refuted agy's M7 mechanism (`FsOk` is
  already listed in the host-crash check's invariants, `configs/recovery-posix-hostcrash-check.cfg`). Owner: pull both.
- **Design forks** (brief `.clavity/seams/cut6-design.md`): aligned on M6, M8, `NoDoubleOpen`,
  `PlainNeverOwnsUncertain` and union-over-ghosts. The driver's reservations are now "measure first" steps in
  items 1, 5 and 11. Owner approved all five.
- **Seeds** (brief `.clavity/seams/cut6-seeds-neg1.md`): agy first wanted seeds defaulted in the runner, the driver
  a scripted edit, and the driver proposed a synthesis (seeds listed only when TRUE, injected FALSE at
  materialization). agy found the synthesis's flaw - committed configs would no longer run under raw TLC - and
  both settled on the scripted edit. Owner asked for the negotiated answer; this is it.
