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
killed by a named run for the reason stated, measured - or NARROWED, with its residue written into `TODO.md` in
place of the old entry. A "measure first" step can also RETURN an item to the owner; such an item's `TODO.md`
entry is rewritten with what the measurement showed, and the cut cannot close until the owner rules on it
(closed differently, narrowed, or moved out of the cut).

## Scope - 13 items

The sequencing decision placed 11 `TODO.md` items in cut 6. Two hygiene items were pulled in by the owner
because two of the 11 cannot close without them: "`S240_5_s6`'s two refusing branches" closes only through the
branch-granular union, and M7's past-ids half is vacuous under strong identity.

## Principles (owner-approved)

1. **Reachability through the branch-granular union, not new ghosts.** A ghost added only to satisfy a gate is
   model pollution; the union reads coverage TLC already emits. Its limits, measured in panel round 2: an anchor
   must name the node of the arm's BODY (a guard's node counts evaluations of the guard, not the arm running);
   the union is only as complete as the runs it reads (halting runs give a prefix); and it cannot tell apart two
   inputs that take the same arm.
2. **Non-vacuity through one seed per conjunct or site.** A seeded run halts at the first violating state, so a
   seed that breaks two things at once lets a single gutting survive. Each seed must violate the PROPERTY, not
   pin its own value (the `Classifiable` lesson).
3. **Every closure is proven by its mutant.** The mutant is applied, the named run goes RED, the mutant is
   reverted; a closure with no red run is not closed.
4. **Every new model constant - each `SEED_*`, and the M7 negative control's constant - is added by a one-off script** to every config and every run's `constants` in
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
  was evaluated); how nodes under `\E`/`with` are counted; whether an arm producing only duplicate successors
  still counts; whether a constant-level `CASE` body (`Torn`, `FsModel.tla:48`) gets a node at all; and how an
  operator called at several sites in one action (`FsHostCrash` is called twice at `algorithm.txt:1023-1024`) prints
  its nodes. **Each outcome has a consequence, stated in advance:** if arms get no nodes of their own, or a count
  does not mean the body ran, items 2, 3 and 6's union closures return to the owner; if a duplicate-successor arm
  does not count, item 2 returns to the owner (its two arms have identical bodies, `LockProtocol.tla:2196-2208`);
  if a constant body gets no node, item 6's torn arm returns to the owner; if per-call-site nodes print
  separately, their counts are summed per source span.
- **Data model.** `parse_cost_nodes` keeps the full span (`module, line, col, end_line, end_col, count, depth`);
  nodes are grouped under the most recent 2772 action. Its callers (`run.py:1369`, `test_run.py:2085-2110`)
  follow the shape change.
- **Expectations.** A top-level `branches` list in `expected.toml`, entries
  `{ label = "...", arm = "<verbatim text of the arm's first line>", reason = "..." }` for `LockProtocol`, and
  `{ module = "FsModel", operator = "FsHostCrash", arm = "...", reason = "..." }` for an arm in another module,
  which has no PlusCal label. The runner validates the list like its other top-level lists: an unknown key, or an
  entry with both or neither of `label` and `operator`, fails at load. Resolution at load time
  finds the arm text inside that label's action (or operator) in the GENERATED module (`--check-translation` pins
  it), so an anchor that also appears elsewhere - `IF ident # tobj[self]` occurs in both `S240_5_s4` and
  `S240_5_s6` - is unambiguous, and resolves it to the node of the arm's BODY, per item 1's measurement. The anchor
  text may be the arm's guard; what is COUNTED is its body. Missing or ambiguous anchor: fail closed.
- **Which logs count.** Counts are summed over the logs of runs that model the protocol AS SPECIFIED: not `-fixed`
  runs and not seeded runs. The label union counts seeded runs positively (`run.py:1246-1247`), but a seed
  reaches arms the protocol never does - item 12's `SEED_TAKEOVER_FOREIGN` sends a `Foreign` read into
  `S240_5_s5`'s continue arm. A summed count of zero fails.
- **Where it is judged.** Twice, over the logs each tier already writes, so it costs no TLC time: in
  `union_from_logs` (`run.py:1251`) for `expected.toml` (`just model` and the per-PR CI `union` job,
  `.github/workflows/model.yml:166`), and - new in this cut - in a union step of the extended tier
  (`model-extended.yml`) for `expected-extended.toml`, whose exhaustive host-crash checks are the only complete
  runs that evaluate `FsHostCrash` (item 6). A `branches` entry names the tier that judges it
  (`tier = "per-pr"` default, or `"extended"`).
- **Tests (`test_run.py`).** Arm parsing and depth from the new fixture; two logs where only one covers an arm
  (passes); an arm at zero (fails); a missing anchor and an ambiguous anchor (fail closed); an anchor scoped away
  from the same text in another label.
- Out of this cut: the companion hygiene item "Say which `never_reached` claims an exhaustive run supports".

### 2. `S240_5_s6`'s two refusing branches

Both arms set `refused := "RESTART"` with identical successors (`algorithm.txt:549` and the `ident # tobj`
arm below it), so no state predicate can tell them apart. **Closure:** a `branches` entry anchored on the guard
`IF ident # tobj[self]` inside `S240_5_s6`, counting its THEN body. **Mutant:** keep the anchor text and make the
arm unreachable - conjoin `FALSE` to its guard (`IF ident # tobj[self] /\ FALSE`) - so the body's count goes to zero;
expected red reason: the union's zero-count failure for that entry, not a load error. Depends on item 1's
duplicate-successor measurement.

### 3. `tornRead` is not set at `S240_5_s5`

The takeover's read of a torn record falls through and continues (`algorithm.txt:492-494`). **NARROWED (owner,
agy aligned, after panel round 2):** the continue arm takes EVERY non-record read, so an `EmptyFile` lock
(classified uncertain, `algorithm.txt:145`) reaching a takeover keeps its count above zero whether or not a torn
read is mishandled; and the arm's generated text occurs twice inside `S240_5_s5` (`LockProtocol.tla:2089`,
`:2093`). **Closure, narrowed:** a `branches` entry anchored on `S240_5_s5`'s `IsRecord(seen)` test, counting the
non-record body - it proves that a takeover meets a non-record read and continues. **Mutant:** make every
non-record read at `S240_5_s5` refuse; expected red reason: that entry's zero count. **Residue:** torn versus
empty at takeover is not distinguishable without a ghost; written to `TODO.md`. With item 4 deleting `tornRead`,
the comment explaining the omission (`algorithm.txt:487-491`) is rewritten in the same commit.

### 4. M5 - `NeverTornRead` is placement-blind

`tornRead` is set at `algorithm.txt:144`, before the judgement, so an unconditional set keeps the five witness
runs violated. **Closure:** delete `tornRead` (`algorithm.txt:35`, `:144`); redefine
`NeverTornRead == ~\E p \in Procs : seenRec[p] = Torn /\ classified[p] = "uncertain"` (the `TODO.md` entry's own
suggestion). The predicate covers the CLASSIFY read only: `S240_5_s5` reads into a local and writes neither
`seenRec` nor `classified`, so a takeover's torn read is not covered by it (item 3, narrowed). **Mutants:** (a) judge Torn as
anything but uncertain (the `SEED_TORN_AS_FOREIGN` shape) - the five
`NeverTornRead` witness runs stop violating and fail; (b) drop `seenRec[self] := seen` (`:143`) - same.

### 5. M6 - `recoveredAfterCrash`'s guard is always true

Without seeds `Replaceable` needs "dead", which the oracle offers only for a crashed owner, and `crashed` is only
ever set `TRUE`, so the guard at `algorithm.txt:374` is always true where it runs. **Closure (owner, agy
aligned):** turn it into a safety claim. Replace the witness ghost with `replacedLive`, set at `S240_3_s5` when the
replaced lock's owner is a process that has NOT crashed; add `ReplacedOnlyDead == ~replacedLive` to every check
run's invariants; add `SEED_RECOVER_LIVE` (a `Replaceable` disjunct admitting a live owner) with a seeded run
expecting `ReplacedOnlyDead` violated. The five `NeverRecoveredAfterCrash` witness runs are retired: they restated
`S240_3_s5`'s label coverage (the `TODO.md` entry). **Mutants:** drop the `crashed` test in the ghost's guard - the
ghost now fires on an ordinary dead-owner recovery, so every check run fails; delete the set - the seeded run finds
no violation and fails. **Measure first:** whether the seeded path reaches
`S240_3_s5` past a live owner's OS-native lock. If it cannot, the seed moves to where the live owner is judged, or
the item returns to the owner.

### 6. M7 - host-crash net holes

- **Flipped comparison** in `hostCrashChangedLock`'s update (`algorithm.txt:1023`). A witness run passes on any
  violation, so a flip that fires on every host crash still passes. **Closure:** a NEGATIVE CONTROL - a check run
  in which a host crash cannot change the lock, listing `NeverHostCrashChangedLock` as an invariant that must
  HOLD. The constant is `HostCrashAtStartOnly` (TRUE only in this run). It is a conjunct on the environment's
  host-crash branch, whose guard is `await HostCrashes /\ crashes < MaxCrashes;` (`algorithm.txt:1015`):
  `/\ (~HostCrashAtStartOnly \/ <no actor has taken a step>)`, the start predicate written over `pc` with each
  role's start label - NOT over `live`, which returns to FALSE at `own_end` (`algorithm.txt:734`), so a later host
  crash could restore an unflushed unlinked lock and redden the correct model. FALSE everywhere else, the
  conjunct changes no other run's graph. A host crash before any actor step cannot change the lock, because the
  initial state is durable (`FsModel.tla:96`). The run is `recovery-posix-hostcrash-control-check` (a `check` kind:
  `run.py:42` has no `control` kind, and the name must end in the kind, `run.py:736`), the recovery pairing with
  `HostCrashes = TRUE`, in the per-pull-request suite. With `MaxCrashes = 2` up to two such crashes occur, both on
  a durable state, so the state space is `recovery-posix-check`'s plus the smaller graphs a reduced crash budget
  leaves - far below the extended tier's 10.7-22.8 million POSIX states (`models/lockproto/README.md:152`);
  measured. **Mutant:** the flip - expected red reason: `NeverHostCrashChangedLock` violated in the control.
- **"Always keep new content" and "`Unflushed == {}`".** Both remove the old/torn outcomes of a host crash
  (`FsModel.tla`, `FsHostCrash`'s `CASE` arms; `Unflushed` feeds them). **Closure (owner, agy aligned, after panel
  round 2): judged in the EXTENDED tier.** The per-PR union has no complete run that evaluates `FsHostCrash`: its
  only `HostCrashes = true` runs are five halting witnesses (`expected.toml:210, 613, 703, 969, 979`), whose coverage
  is a prefix, and the control above crashes only on durable states, where `Unflushed` is empty. So the entries
  `{ module = "FsModel", operator = "FsHostCrash", arm = "CASE pick[o] = \"old\" ->", tier = "extended" }` and
  `{ module = "FsModel", operator = "FsHostCrash", arm = "[] OTHER -> Torn", tier = "extended" }` (arm texts as
  they stand at `FsModel.tla:315`, `:317`) count the arms' BODIES over the exhaustive host-crash checks of
  `expected-extended.toml`. A regression is caught nightly, on request, or on a pull request labelled
  `model-extended` - not on every pull request; that trade is the owner's. Rests on item 1's measurements for
  `CASE`-in-`LET` in another module. **Mutants:** the two above; expected red reason: each entry's zero count in
  the extended union.
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
(`run.py:831` already supports one) with `PROPERTY DeadLockEventuallyCleared` expects it violated. Its config is
built from `configs/recovery-posix-liveness.cfg` (one `PROPERTY`, no `SYMMETRY`), not from a recovery seeded
config: those carry `SYMMETRY` (`run.py:844` forbids it in a temporal run) and no `PROPERTY` (`run.py:838`).
Measure: whether the seed must stop both Recoverers for the property to fail. **Mutants:**
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
and `LockName` is re-created after a move-aside - `S240_3_s3_create` calls `FsCreate(fs, P, LockName, self, TRUE)`
(`algorithm.txt:327`), and under weak identity `FsCreate` adds the new id to `past` (`FsModel.tla:135`), so after an
Owner's create and a Recoverer's re-create `past[P][ClassOf(LockName)]` holds two ids. **Closure:** weak-identity
check runs for the recovery and mixed scenarios on POSIX, plus a state-predicate WITNESS in each,
`NeverSecondPastId == \A d \in Dirs, c \in Classes : Cardinality(fs.past[d][c]) <= 1`, which must be VIOLATED -
proof that the weak runs exercise weak semantics at all (owner, agy aligned, after panel round 2: the mutant
`past[d][c] = {}` alone has no failure channel, because it reduces `FsIdentityChoices` to `{o}`, exactly strong
semantics, `FsModel.tla:266`, and state counts are not pinned). **Mutant:** `past[d][c] = {}`; expected red reason:
the witness is no longer violated. **Measure:** the weak runs' state counts and times, recorded in `expected.toml`
and the README like every other run; the design doc's rationale and `models/lockproto/README.md:144` ("the
weak-identity variant explores an identical state graph here, because no name in this scenario is reused") are
corrected with the new measurement. The weak runs are
measured FIRST, before the plan fixes the run list: `past` accumulates ids, so their size is not predictable from
the strong runs. If one exceeds its tier's timeout, the plan splits it the way `breaklock-remote` was split, or the
owner narrows item 13 to the recovery pairing.

## Run placement (every new seeded run)

Each new seeded run copies an existing seeded run of its scenario and changes only its own flag, except the
seeded TEMPORAL run (item 9), which copies the liveness config. Every new run's name follows
`<scenario>-<variant>-<kind>[-<SEED_FLAG>]` (`run.py:736`, `:727-729`). The new
`SEED_FS_*` seeds act where the existing ones do - an environment step beside `SEED_FS_LOCK_WITHOUT_HANDLE`
(`algorithm.txt:1055`) - and run in the recovery POSIX pairing, like `recovery-posix-seeded-SEED_FS_ALIEN_CONTENT`.
`SEED_RECOVER_LIVE` and `SEED_RECOVERER_GIVES_UP` also run in the recovery POSIX pairing (Owners o1, Recoverers
r1 r2). The `S99_release_check` seed runs in that pairing too; the `S21_1_s3` seed and `SEED_TAKEOVER_FOREIGN` run
in the breaklock POSIX pairing, which has a Breaker. The plan names each run and copies its constants.

## Run inventory (new runs)

Seeded, halting at the first violation: `SEED_RECOVER_LIVE`, `SEED_FS_PAST_UNALLOCATED`, `SEED_FS_DOUBLE_HANDLE`,
`SEED_FS_ENTRY_UNALLOCATED`, `SEED_FS_ALIEN_RECORD`, the two per-site `RefusalJustified` seeds,
`SEED_TAKEOVER_FOREIGN`. Seeded temporal: `SEED_RECOVERER_GIVES_UP` (a full liveness exploration; the existing
`recovery-posix-liveness` run is the nearest recorded cost). Negative control: one host-crash check. Weak identity:
the recovery and mixed POSIX checks, each with its `NeverSecondPastId` witness run. Runner: the extended tier's new
union step. Retired: the five `NeverRecoveredAfterCrash` witness runs. Changed: every
check run (new invariant `ReplacedOnlyDead`; `tornRead` deleted), so every state count is re-measured in one CI
Model run, as `TODO.md` asks ("batch them rather than paying three times").

## Verification

- `test_run.py` passes, including the union tests above; `run.py --check-translation` passes; the stamp tests
  pass.
- Each item's mutant is applied, its named run goes red FOR THE EXPECTED REASON stated in its item, and the
  mutant is reverted. A red for another reason - a load error, a missing anchor, a timeout, "cannot judge the
  union" (`run.py:1290-1293`) - is not a kill: fix the mutant or the closure and re-run. The record (mutant, run,
  expected reason, observed message) goes into the plan's execution notes and the test-audit ledger.
- One full CI Model run on the final commit, green; its run ID and every changed state count recorded.

## Residues, stated (written to `TODO.md` by this cut)

- POSIX double open invisible to `NoDoubleOpen` (item 7).
- M8: a widening admitting only `"RESTART"` (item 9).
- `PlainNeverOwnsUncertain` covers only the move-aside (item 11).
- A takeover's torn read is not distinguished from an empty one (item 3).
- `FsHostCrash`'s content arms are judged in the extended tier, not on every pull request (item 6).
- Any item a "measure first" step returns to the owner, with what the measurement showed.
- The companion hygiene item on `never_reached` claims stays open.

## Stand-downs

- **Panel round 2 was reviewed by an independent Opus subagent, not agy** (owner, 2026-09-27): agy's two attempts
  at round 2 ended in error steps before any reply, and no agy console was open to read the cause. agy was back
  for the AGY-FIRST consult on the three forks that round raised.
- DISCARDED-BELOW-FLOOR (round 2): none. Two round-2 items were citation slips and are folded (`FsModel.tla:96`,
  `README.md:152`).

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
- **Three closures the union cannot carry** (brief `.clavity/seams/cut6-forks2.md`, after panel round 2): the owner
  chose item 3 narrowed, item 6 judged in the extended tier, and item 13's `NeverSecondPastId` witness; agy aligned
  on all three and traced the two-id path the witness needs (`algorithm.txt:327`, `FsModel.tla:135`), which the
  driver verified.
