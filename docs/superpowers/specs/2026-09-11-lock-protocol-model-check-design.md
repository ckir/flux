# Lock-protocol model check — design

Date: 2026-09-11. Status: approved design, revised after adversarial review rounds 1 to 6, and on 2026-09-12 with the
owner's rulings for plan 2 (liveness witnesses, Sections 4 and 7; sibling headings and pending units, Section 9; build
order, Section 12).
Branch: `model/lock-protocol`, on top of `spec/v16-resolution`.

## 1. Goal

Flux's V16 specification (`FLUX_FULL_UPDATED_SPEC_V16.md`) defines how operations lock their targets, replace dead
or uncertain locks, revalidate ownership before every write, and claim destination entries at publication. A
six-round adversarial review of that text stopped at its cap still finding defects in exactly these rules each round,
and the last round's fixes are unreviewed (`TODO.md`). This project replaces further prose review of those rules
with an exhaustive, bounded model check that stays in the repository and keeps checking the spec as it changes.

The model must find every reachable violation of the protocol's safety properties within stated bounds, show that
the protocol still makes progress, and stay tied to the spec text.

## 2. Decisions already made

| Decision | Choice |
|---|---|
| What the work leaves behind | A living model in the repository, run by `just` and CI, that must stay green whenever the spec's lock sections change. Findings become spec fixes. |
| Scope | Lock replacement core; crashes at every step and torn lock records; nested destination roots; claims with COMMIT; and (added by the owner after review round 2) Section 96.1's directory-lock fallback and its announce-then-check exclusion. |
| Filesystem semantics | Two configurations of one filesystem model: POSIX and Windows (plus weak-identity and weak-lock-capability variants, Section 5). |
| Method | TLA+/PlusCal checked with TLC, plus Rust tests that confirm the filesystem assumptions on real operating systems. Chosen independently by the owner's two reviewers (Claude and agy). |

## 3. Spec sections the model encodes

These sections are the model's contract. The drift stamp and the traceability check (Section 9) cover exactly this
list.

| Section | Rule | Model |
|---|---|---|
| 96.1 | Target lock file created exclusively; classification when creation fails (another operation's record, this operation's record for a different spelling, unreadable, foreign); OS-native lock for liveness; in-place overwrite only by a takeover; the directory-lock fallback `P/.flux-dir.lock` and its announce-then-check exclusion with per-name locks; the root lock `T/.flux-root.lock` | LockProtocol. Two parts are "not modelled": the different-spelling branch (LockProtocol uses one spelling per target; the branch is a deterministic comparison with no interleaving, pinned by spec acceptance test 54) and the root lock (the same exclusive creation at a different path, which adds no interleaving to an abstract lock path) |
| 96.2 | A lock refusal reports the holder | LockProtocol (report content is not checked, only that the refusal happens) |
| 97.1 | Nested destination roots: ancestor check after creating one's own lock; descendant check before creating or writing into a directory | LockProtocol |
| 99 | Commit-time revalidation: "still owned" for every lock held; failure outcomes; supersession checked before cancellation | LockProtocol, Claims |
| 99.1 | Weak or unavailable filesystem identity does not invalidate the name-based target lock; a lock is no proof of object continuity | LockProtocol (the `IdentityStrength = weak` variants) |
| 21.1 | `--restart` steps 1-5; the lock never released between steps 1 and 5 | LockProtocol |
| 240.1-240.5 | Recovery decision order; live owner; dead-owner move-aside (240.3 steps 1-5); uncertain ownership; `--break-lock` in-place takeover (240.5 steps 1-6) and its cleanup variant | LockProtocol |
| 251.1, 251.2 | Cleanup classification of locks, orphan locks, moved locks, cleanup locks | LockProtocol |
| 259.6 | Lock record: fixed size, checksum, cleanup lock record | LockProtocol |
| 120 | Accepted `workspace_path` values, including `none` | LockProtocol |
| 235.1 | `RemoteUnverified` and `Unsupported` lock capabilities refuse every operation needing target exclusivity (`REMOTE_LOCK_UNSAFE`) | LockProtocol (the `LockCapability = weak` variant) |
| 241.5 | Directory-entry claims; created-entry claim with COMMIT; aliasing; hardlink dependents | Claims |
| 182, 183 | Per-file commit pipeline, including its lock revalidation; recovery of PREPARE_COMMIT without COMMIT | Claims |

## 4. Layout and tooling

```text
models/lockproto/
    FsModel.tla            abstract filesystem shared by both models (Section 5)
    LockProtocol.tla       PlusCal: lock acquisition, replacement, revalidation, cleanup, nesting (Section 6.1)
    Claims.tla             PlusCal: claims and COMMIT for one operation (Section 6.2)
    configs/*.cfg          one per run (Section 12): check runs, liveness runs, seeded runs
    expected.toml          the run list and each run's expected result (format below)
    trace.toml             the traceability map: every label and every spec step (Section 9.1)
    run.py                 the runner (Python 3.11 or later, standard library only: it reads TOML with tomllib)
    spec-sections.stamp    drift stamp (Section 9)
    README.md              traceability table, probe table, bounds, unverified assumptions, how to run
tests/model_stamp.rs     the drift-stamp test (Section 9)
crates/flux-platform/tests/fs_semantics.rs    the filesystem probes (Section 10)
.github/workflows/model.yml                   the model CI workflow
```

`expected.toml` holds one `[[run]]` table per TLC run with these fields:

| Field | Meaning |
|---|---|
| `name` | unique run name, used in CI and in the runner's report: `<scenario>-<variant>-<kind>`, plus `-<seed flag>` for a seeded run and `-<invariant>` for a witness run; a variant is one or more hyphen-separated parts of lowercase letters and digits, as a recovery pairing's is (for example `breaklock-windows-check`, `recovery-posix-plain-check`, `mixed-posix-seeded-SEED_TORN_AS_FOREIGN`, `recovery-posix-witness-NeverTornRead`) |
| `module` | the TLA+ module the run checks, named without `.tla`; the runner accepts any identifier whose `.tla` file exists beside `expected.toml`. The modules are `LockProtocol`, `Claims` (plan 3) and `Smoke`, the runner's own `selftest` model |
| `config` | path of the `.cfg` file |
| `scenario` | the Section 12 scenario the run belongs to; one of the names in `expected.toml`'s top-level `scenarios` list, which is the only place scenario names are defined; CI builds its matrix from that list |
| `kind` | `check`, `liveness`, `witness`, or `seeded` |
| `violated` | for `seeded` and `witness`: a list of exactly one name, the invariant or property that must be the first violation; absent for `check` and `liveness` |
| `open_findings` | optional, for `check` and `liveness`: safety invariants or properties that currently fail because of a spec defect not yet fixed, each as `{ name, tracking, fix_flag }`: `tracking` names its `TODO.md` or `.clavity/local-anomalies.md` entry, and `fix_flag` names a model flag that applies the proposed spec fix |
| `unreached` | optional, for `check` and `liveness`: labels of the run's own actors that this run cannot reach, each as `{ label, reason }` (the coverage check below) |
| `constants` | required: a table holding every literal constant the `.cfg` assigns (integers, booleans, strings, and sets of identifiers as string arrays), except model values and `FIX_*` flags, and a `FIX_*` key in this table is rejected (fix flags are set only by the derived `<name>-fixed` runs, below). The runner compares it with the `.cfg` in both directions and rejects a `.cfg` it cannot read in full: a `<-` substitution, a constant assigned twice, or an unparsable value (the list above explains why the `.cfg` is checked) |
| `symmetry` | `{ definition, over }`, present exactly when the `.cfg` has a `SYMMETRY` section. The `.cfg`'s `SYMMETRY` must name `definition`, the module must define `definition == Permutations(over)` outside comments, and `over` must be a set constant with at least two elements. Not allowed on a `liveness` run |
| `tightened` | optional, `liveness` runs only: `{ rung, measurement }`, which records the tightening-ladder rung taken (Section 12) and the measurement that forced it. The runner prints it with the run's result |
| `timeout_minutes` | the run's time limit |

How each kind of run is judged:

Every `check` and `liveness` run also runs TLC with `-coverage 1`, whose report lists each action with the states it
generated as `<distinct:found>`. Measured on TLC 2.19 (2026-09-12): PlusCal translates each label into one action, the
report carries one line per label rather than per process instance, a label whose guard can never hold reports `0:0`
even though TLC evaluates it at every state, and symmetry reduction lowers a label's counts but does not zero them
(`A 3:4` under `SYMMETRY` against `4:6` without). A run fails if a label of an actor it instantiates has no states
found. Without this, a label behind a condition that can never hold would still have a `trace.toml` entry mapping it to
a spec rule, and the traceability map would claim coverage that never runs.

Five things about reading that report are measured, and each of them silently breaks a gate that gets them wrong
(2026-09-12; every one of them was hit while building the `recovery` scenario). None of them is a documented
interface: they are the output format of the pinned TLC release, so they are pinned by the same SHA-256 that pins
the jar, and a change to that pin re-checks them and re-records `testdata/` with `record_fixtures.py` in the same
change. A gate reading an unpinned TLC would be reading an unspecified format.

- `-coverage 1` prints a snapshot every minute **and** a final report, and only the last block covers the whole run.
  In one log `S240_3_s4_drop` reads `0:0` in the one-minute snapshot and `1168:2835` in the final block of that same
  log. A gate that reads the first block reports labels as uncovered that the run reaches later.
- the block's terminator has two forms: `End of statistics.` and, on a large model, `End of statistics (please note
  that for performance reasons large models are best checked with coverage and cost statistics disabled).` A parser
  anchored on the punctuated form fails on exactly the big runs it matters for.
- a block holds two kinds of line, and only one of them is a label. An action's own line begins with `<` in the
  first column and carries the `distinct:found` pair; every line beneath it is a cost line for an expression inside
  that action, indented, carrying a SINGLE number, and prefixed with one `|` per level of nesting below the first
  (`  line 606, col 25 ...: 5856534`, then `  |line 606, col 25 ...: 5815368`). A gate must anchor on the leading
  `<` - not on the `|`, which the first level of cost lines does not carry - or it will read an expression's figure
  as a label's.
- "no states found" is the SECOND number. A label that only ever regenerates states already seen reports zero
  distinct and a large total: `publish_crashed` reports `0:32937`, and PlusCal's own `Terminating` action `0:1029`,
  in a run where an all-Done state is reachable. A gate reading the first number fails on both.
- `-coverage 1` is not free: the same state graph (7,254,481 generated, 2,174,196 distinct, exhausted both times)
  took 5min53s with it and 3min24s without, which is why the time budget below is quoted for runs that carry it.
  That pair is one measurement each, minutes apart on a machine whose background load was not controlled, so it
  sizes the cost and does not pin it; the state counts either side of it are exact and repeatable, the durations
  are not.

A bound must also be shown not to bind before its run's coverage means anything. `FsCreate` fails both when an entry
of the name exists and when `fs.next = MaxObjs`, so under a binding object bound a create-failure label is covered
for the wrong reason - the gate passes on the bound rather than on the protocol. Every configuration's `MaxObjs` is
therefore raised by one once and its state count confirmed unchanged. All eight `recovery` check runs were verified
this way before the acquirer rule of spec 96.1 became part of the model. That rule is not re-measured against the
bound by raising it, because the bound cannot bind in any `recovery` configuration, for a reason independent of the
rule: every actor makes at most one exclusive create - an acquisition or a recovery's replacement lock, never both,
since each actor makes one pass - so objects allocated never exceed the number of actors, and each configuration's
`MaxObjs` is at least that number. Measured where it is tightest, `recovery-posix-check`, whose three actors meet
`MaxObjs = 3`: `fs.next <= Cardinality(Procs)` holds over its complete state space (2026-09-13). A later model that
lets an actor create twice, for instance by retrying, reopens the question.

That check is per bound, and clearing one bound says nothing about the others. A configuration is a box with at
least three walls - `MaxObjs`, `MaxCrashes`, and the actor sets themselves - and a label can be pinned against any
of them while the other two sit slack. This scenario is its own example: at `MaxCrashes = 1`, `recover_crashed` was
unreachable no matter what, and `MaxObjs` was already provably non-binding at the time, so the object-bound check
passed and proved nothing about the label. Raising `MaxCrashes` to 2 is what reached it. A bound is therefore
checked when a label it could pin is uncovered, and the uncovered label names which wall to push on: a
create-failure label points at `MaxObjs`, a crash-path label at `MaxCrashes`, and an interference label at the
actor set - which is what the `recovery` pairing below is.

An uncovered label is a trigger for that check, not the whole of it. A bound constrains INTERLEAVINGS as well as
labels, and those it can cut while every label stays covered: a second crash removed from a run does not uncover a
label if some other path reaches the same label once, and nothing in the coverage report will say the run stopped
exploring the case where a crash lands inside a recovery. Coverage cannot see this, so the bounds do not rest on it.
Each configuration's bounds are justified in the README on their own terms - what the run is for and what its bounds
still let it explore - and that justification is what a reviewer reads, with the label check underneath it as a floor
that catches the cruder failure.

What the gate cannot see is which ARM of a label was taken. A label holds one filesystem call and the local decision
that follows it (Section 5.2), so `S21_1_decide` carries the whole of 21.1's judgement table: its coverage count
proves a plain rerun decided, never that it decided `RESUMABLE_OPERATION_EXISTS` against a dead owner. Coverage is a
reachability floor for a label, and the invariants, checked in every state of an exhausted graph, are what constrain
what happens inside one.

This coverage report is also how a run proves it reached the paths it is about, which is why no `check` or `liveness`
run carries witness invariants. A witness invariant is violated in every state after its path is reached, and TLC has
no flag that reports an invariant once: measured on the `recovery` scenario with four actors and no crashes, the
witnesses produced 10,057 violation reports and a 309.6 MB log with `-difftrace` already on, and the run had not
finished in ten minutes (2026-09-12). A label's coverage count proves the same reachability with no output at all. The
few facts no single label states - a read that found a torn record, a replaced lock that a crash had left, a lock whose
owner is dead or uncertain sitting at the lock path - keep a ghost flag or a state predicate and are checked as
`witness` runs (below). To get a counterexample trace for any path, re-run that configuration with the witness as an
invariant and without `-continue`; the gates need the fact, a person debugging needs the trace.

Four rules keep the coverage check honest without making it lie:

- the labels of an actor whose process set the run's configuration leaves empty are exempt. For a label inside a
  `process` block the runner needs to derive nothing: TLC reports no action at all for an empty process set
  (measured - the run with `Recoverers = {}` reports none of the nine `rec_` actions the others report), so such a
  label is simply absent from the report and cannot fail the gate. The attribution that DOES have to be derived is
  for the labels in the shared `procedure` blocks, which is where most of this model's labels live: `S240_1_*`,
  `S96_1_*`, `S240_3_*` and `S99_*` sit in `Classify`, `Acquire`, `Recover` and `Publish`, belong to no `process`
  block, and are reached by whichever actors call them. A procedure's label therefore belongs to the set of actors
  that call that procedure, and is exempt only when every one of those actors has an empty process set; a procedure
  no actor calls is never exempt, so a dead procedure fails its run rather than passing vacuously. Deriving it
  from the containing block instead would attribute those labels to nobody, which is both the larger half of the
  model and the half the exemption has to get right;
- a run may list `unreached` labels with a reason: steps its variant or its configuration cannot reach although their
  actor runs, such as 240.5 step 1's capability branch, which only the seeded run without the capability gate reaches
  (Section 5.1). The check is symmetric, as for witnesses: a listed label that does get covered fails the run too,
  naming the label and saying to remove it from `unreached`, so a label that becomes reachable cannot stay excused;
- `unreached` excuses a label in one run, never in the model. A run of every run in `expected.toml` (`just model` with
  no scenario) unions the coverage of all its runs, which TLC reports per run and never merges itself, and fails if any
  label of any model was covered by no run at all, so a label cannot be listed out of every run and left as dead code:
  whatever a scenario cannot reach, another scenario or a seeded run must. What the union proves is exactly that and
  no more - that the label is not dead - never that it was reached under the conditions its own scenario is about,
  since the runs it unions include other platforms and other actor sets. The stronger property is what the per-run
  requirement gives, which is why a label excused in one run still has to carry its reason there. A single-scenario run cannot judge this and
  says so in its report, and the CI matrix runs one scenario and platform per job, so `model-gate` does not enforce it; the full
  `just model` that Section 13 requires before the work is declared finished does. That is the weakest point in this
  rule: a pull request can be green for as long as it likes with a label no run covers. The union job of
  `model-extended.yml` closes most of that gap. It runs all of `expected.toml` in one invocation, every night and on
  a labelled pull request, because it needs every run's coverage, not one scenario's, and so it cannot be part of a
  per-scenario matrix. A label left uncovered therefore fails within a day of reaching `main`, not only at the end;
- one top-level `never_reached` list in `expected.toml`, each entry a label and a reason, holds the labels no run can
  cover. It is the only way out of the rule above, and it costs the label its traceability: a label in `never_reached`
  may not appear in any `[[unit]]` entry's labels (Section 9.1), so a unit it was meant to implement falls back on
  `not_modelled` or another label. A dead label can therefore never stand as the model of a spec rule, which is the
  only thing listing it could otherwise buy. Adding a scenario to reach an awkward label instead is a change to Section
  12's table with its reason, as a changed witness set is, never an edit to `expected.toml` alone - and so is adding a
  RUN to a scenario that already exists. That second half is the one the first misses on its own: the cheap way to
  clear an awkward label is not a new scenario at all but a degenerate new run under a scenario name already blessed,
  bounded down to almost nothing and aimed only at the label, which the suite-wide union then accepts. Section 12's
  table lists runs, not only scenarios, which is what makes a new run reviewable as a design change; the `recovery`
  pairing below is that list for its scenario, and each of its rows carries what only that pairing reaches. This one
  is review-gated rather than mechanised, and says so: no check can tell a run added to reach a real path from one
  added to clear a label, because the difference is intent. What it buys is that both edits - the new `[[run]]` and
  the new table row with its reason - appear in the same diff, and a `[[run]]` arriving without one is the thing a
  reviewer looks for. Section 9 already rests on the same footing for a re-stamped heading;
- one top-level `deferred` list in `expected.toml`, each entry `{ label, scenario, reason }`, holds the labels that no
  BUILT scenario covers but a planned one will. The model is built one scenario at a time (Section 12), and a
  procedure shared across scenarios carries branches whose state only a later scenario creates: `S96_1_backoff` needs
  the directory lock that only `dirlock` makes, and `S240_3_putback` needs the in-place record rewrite that only
  `breaklock` performs. `never_reached` is the wrong home for such a label, because it is reachable and keeps its
  traceability, so its `trace.toml` entries stay. `pending` (Section 9.1) is the wrong source too: it is a fact about
  a spec UNIT, which scenario still owes that unit labels, while the union needs a fact about a LABEL, and the two
  differ. 240.3 step 3 is fully modelled with no `pending`, yet its put-back branch waits for `breaklock`. The union
  accepts a `deferred` label as uncovered, and fails when a `deferred` label IS covered, as it does for `never_reached`,
  so the entry must go the moment a run reaches it. The drift-stamp test (Section 9.1), which already reads both
  files, requires each entry's `scenario` to be in `trace.toml`'s `planned_scenarios`, so an entry cannot outlive
  its scenario: the plan that builds the scenario moves its name out of `planned_scenarios` and must remove the entry
  or cover the label. The runner rejects a label listed in both `deferred` and `never_reached`, or twice in
  `deferred`, and a `deferred` label that is in no model named by a run. One way round this stays review-gated, and this rule says so: re-pointing an entry to a
  different scenario that is still planned. Like a new run, that change shows in the diff next to its reason;

- the derived `<name>-fixed` runs are not covered by the check. A proposed spec fix is meant to make the path it
  closes unreachable, so a fix-flag run would otherwise fail because the fix worked;
- a run that times out is a tooling failure (exit code 2) and its coverage is not judged at all, because a partial
  state space says nothing about what is reachable.

- `check`: TLC runs with `-continue`, so it explores the whole reachable state space and reports every violated
  invariant. The configuration lists the scenario's safety invariants and no witness. The run passes only if the set
  reported equals the run's `open_findings` exactly: every open finding still violated, no other safety invariant
  violated, and every label of every actor it runs covered (above). An open finding that stops failing also fails the
  run, so the entry is removed together with the spec fix.
- `witness`: TLC runs without `-continue` and the configuration lists exactly one invariant, the negation of a fact the
  scenario must reach that no label's coverage states (Section 7). Passes only if TLC stops with that invariant
  violated, which both proves the fact and yields the one trace showing how it happens. These runs are small: TLC stops
  at the first violating state.
- `liveness`: TLC runs with `-continue` and checks the configuration's one temporal property and every safety invariant
  of the scenario, with no symmetry reduction (symmetry and liveness checking together are unsound in TLC). Passes only
  if the invariants reported violated are exactly its `open_findings`, the property is not violated unless it is itself
  an open finding, and every label of every actor it runs is covered. Its scenario's `witness` runs are what show the
  states the property is about do occur, so a configuration that constrained its state space to almost nothing could not
  pass them. TLC 2.19 with `-continue` does still check the temporal property over the complete state space after
  reporting invariant violations, and reports both (measured 2026-09-12), so a liveness run with an open finding is
  judged on both. A liveness run's configuration must also declare no `SYMMETRY`, and the runner rejects one that does
  before running it, exactly as it rejects a `PROPERTY` in a `check` configuration. Prose alone will not hold this: symmetry and
  liveness together are unsound in TLC but TLC does not refuse the combination, and a `SYMMETRY` line is the
  cheapest way there is to make a run that will not fit its budget suddenly fit - it shrinks the state space and
  changes no result the gate looks at. The rule belongs where the gate can see it. A configuration that checks a
  temporal property (a
  liveness run, or a seeded run whose "must fail" entry is a property) lists exactly one `PROPERTY`, because TLC
  reports a temporal violation without naming the property (measured against TLC 2.19).
- Fix-flag runs: for every `check` or `liveness` run with open findings, the runner also runs the same configuration
  with all their fix flags set, as a derived run named `<name>-fixed` of the same kind and time limit (it has no
  `expected.toml` entry of its own). It must match with no open finding at all: a new defect that violates the same
  invariant or property as a known one still fails there, and the proposed spec fix is checked before the spec
  changes. A scenario carrying open findings therefore runs longer; Section 12's time budget is for a scenario with
  none.
- `seeded`: TLC runs without `-continue`. Passes only if TLC stops with a violation of exactly the named invariant or
  property; a different violation, or none, fails the run. A seeded run's scenario must have no open finding on the
  same invariant, so that the seed, not the open defect, is what fails it.

TLC's deadlock check stays on. PlusCal's translation already allows the final state in which every process is done,
so a deadlock report means some process is stuck on a condition that can never become true, which is either a model
defect or a real stuck state. To keep that meaning, a crashed process ends at `Done` (a ghost flag records the crash),
and a process that waits for an event that may never happen (a resume that waits for a crash, the `LockLost`
environment action) may also finish without it.

The runner, `run.py`:

- stops with exit code 2 before running anything if Python is older than 3.11, if `expected.toml` is malformed, if a
  run's `scenario` is not in the `scenarios` list, if a listed scenario has no runs, or if `--scenario NAME` names an
  unknown scenario; `--list-scenarios` prints the `scenarios` list as a JSON array;
- downloads `tla2tools.jar` from the TLA+ project's GitHub releases (`github.com/tlaplus/tlaplus/releases`), at a
  release tag and SHA-256 both pinned in `run.py` (exact release chosen in the plan), into `target/tla/`, and verifies
  the hash before every use, including a cached copy; on a mismatch it deletes the file and downloads once more, and
  if the hash still does not match it stops with exit code 2. The pin protects against the download changing later,
  not against a pull request that changes the tag and hash together; such a change is visible in `run.py`'s diff and
  is reviewed as a dependency change;
- checks each run's configuration against what its `expected.toml` entry declares before running it, and stops with
  exit code 2 on any difference. A run declares its actor sets and its numeric bounds - the `MaxObjs`, `MaxCrashes`
  and process-set constants - and the runner reads the same constants out of the `.cfg` and compares them. This is
  what keeps a configuration honest, because the `.cfg` is the one file in this design that a person can edit to
  make a failing run pass: tightening a bound, emptying a process set, or adding a `SYMMETRY` line all shrink the
  state space, and none of them changes any result the rest of the gate looks at. A rule that lives only in prose
  about what a configuration "should" contain is a rule the `.cfg` can quietly break, so the bounds a run is
  entitled to are declared where the gate can read them and the `.cfg` may only agree. For the same reason a `.cfg`
  may not contain a `CONSTRAINT`, `ACTION_CONSTRAINT` or `VIEW` section: each cuts or merges states while every
  constant still agrees, so the runner rejects all three in every kind of run. It rejects TLC's `TYPE` and
  `TYPE_CONSTRAINT` sections too, whose effect on the explored states it cannot vouch for;
- rejects a `SYMMETRY` declaration in any run whose entry does not name the actor set it is over, whatever the run's
  kind. Symmetry with liveness is unsound, so a liveness run may declare none at all (above); but a false symmetry
  over actors that are not interchangeable is unsound in a `check` run too, and it is the single cheapest edit that
  makes an over-budget run fit. The permitted sets are Section 6.1's, named per run in `expected.toml`;
- runs the selected runs (all, or one scenario with `--scenario NAME`), each under its `timeout_minutes`, killing TLC
  on timeout; a run that fails in any way does not stop the others, so one report covers every selected run;
- runs TLC with `-tool`, whose output marks each message with a numeric message code, and decides each run's result
  from the invariant-violation and property-violation messages and the names they carry; the plan pins those codes
  and TLC's exit statuses against the pinned TLC release and keeps recorded TLC logs as test fixtures for the parser.
  A run is judged only when TLC's output shows that model checking finished (or stopped at a violation) and its exit
  status agrees; a parse or semantic error in a model or configuration, any other error message, or a missing
  completion message is a tooling failure (exit code 2), never a run with no violations;
- prints one line per run (name, expected, observed, states found, duration) and, for every unexpected result, the
  first 60 states of TLC's trace, saving the full TLC output under `target/tla/out/<name>.log`;
- exits 0 when every run matched, 1 when any run's result did not match, and 2 for a tooling failure (Java missing,
  download or checksum failure, TLC crash, out of memory, or timeout), naming the cause. A run that times out is
  reported as `TIMEOUT` with the states explored so far, never as a witness that was not violated, because a partial
  state space cannot show that a path is unreachable. When both happen, a mismatch outranks a tooling failure: the exit
  code is 1 if any run completed with an unexpected result, otherwise 2 if any run hit a tooling failure.

Recipes and CI:

- `just model` runs every run; `just model <scenario>` runs one scenario; `just model-test` runs `run.py`'s unit
  tests (recorded TLC output, no Java); `just model-stamp` runs `just model`
  first and rewrites the unit hashes (Section 9) only if every run matched, counting open findings as matched. None of
  these is part of `just check`, which stays Java-free.
- `.github/workflows/model.yml` runs on every pull request to `main`, on pushes to `main`, and on manual dispatch, with
  `permissions: contents: read` and no secrets, in three jobs:
  1. `plan` checks out the repository with full history (`fetch-depth: 0`), runs `run.py`'s unit tests under Python 3.14 and 3.11 (the oldest version `run.py` supports), lists the
     changed files with `git diff
     --name-only` from the merge base of the pull request's base and head (`base...head`), or from the push's previous
     commit, decides whether they touch
     `models/**`, the spec file (glob `FLUX_FULL_UPDATED_SPEC_V*.md`), `crates/flux-platform/tests/fs_semantics.rs`,
     the `justfile` (its model recipes), or the workflow itself, and outputs `run.py --list-jobs` as the matrix - one entry per scenario and platform, the platform being the first part of a run name's variant - or an empty matrix when nothing
     relevant changed. Manual dispatch, and any case where the changed files cannot be determined (a new branch, a
     force push whose previous commit is gone), count as touching, so an unknown change runs every scenario;
  2. `scenario`, one matrix job per scenario and platform, skipped when the matrix is empty (GitHub rejects an empty
     matrix, so the job carries a condition on `plan`'s output), installs Java with `actions/setup-java` (Temurin 21) and Python
     with `actions/setup-python` (3.14, the version the development machines use), runs `just model <scenario> <platform>`, and uploads `target/tla/out/`
     whether it passes or fails, because the logs carry the state counts. Splitting by platform is what lets a scenario
     whose runs are paired (Section 12) fit one job;
  3. `model-gate` always runs after the others and fails if `plan` failed, if any `scenario` job failed or was
     cancelled, or if `scenario` was skipped although the matrix was not empty; it passes when the matrix was empty.
  Because the matrix comes from `expected.toml`, a run can never belong to a scenario CI does not run.
- A second workflow, `.github/workflows/model-extended.yml`, runs the slower tier in `expected-extended.toml` (same
  schema; Section 12 says what is in it): nightly, on manual dispatch, and on a pull request labelled
  `model-extended`, one job per scenario and platform, with `run.py --expected models/lockproto/expected-extended.toml
  --scenario <s> --platform <p>`. It is not part of `model-gate` and not part of `just model`, and the tier's own runs
  judge no union, because they cover no label the runs in `expected.toml` miss. The same workflow's `union` job runs
  `run.py` over all of `expected.toml` and is where the suite-wide union is judged on CI. Nothing in a normal pull request would even read
  that file, so a unit test of `run.py` loads it: a broken entry or configuration fails the pull request instead of
  waiting for the nightly run.
- The drift-stamp test and the filesystem probes are ordinary Rust tests, so `just check` and the existing
  Linux/macOS/Windows test job run them.
- `.claude/recommended-tools.json` gains entries for Java 21 and Python 3.11 or later, the two non-Rust tools
  `just model` needs.
- The stamp and traceability tests (Section 9) are mechanical: they can be made to pass by rewriting the hashes
  without re-checking the model. Only the TLC runs check the model against the changed text, so the design
  recommends that the owner make `model-gate` a required status check; `model-gate` always reports, so requiring it
  never blocks a pull request that does not touch the model. No mechanism can make a person model a new spec rule
  rather than re-stamp it; what the design does is make the change visible, because the rewritten hashes appear in
  the pull request's diff next to the `trace.toml` entries and labels they cover.

## 5. Filesystem model (`FsModel.tla`)

State:

| Variable | Meaning |
|---|---|
| `entries` | per directory, entry name → file object id; two names are the same entry when `Fold` maps them to the same class. Object ids are never reused; they index `content`, `durable`, `handles`, and `oslock` |
| `content` | object id → `Record(op, kind)` (kind: `operation` or `cleanup`), `Torn`, or `Foreign`: what a reader sees now |
| `durable` | object id → the content that survives a host crash (Section 5.2) |
| `durableEntries` | per directory, the entries that survive a host crash; `entries` differs from it by the entry operations (create, hard link, rename, unlink) made since that directory's last flush |
| `handles` | set of `[proc, obj, shareDelete]` |
| `oslock` | object id → the process holding its OS-native lock, or none |
| `inflight` | set of filesystem calls issued by a process and not yet complete (Section 5.2) |
| `pastObjects` | per directory and name class, the object ids that name has held; used only by weak identity (Section 5.1) |

### 5.1 Operations

Each row is grounded by a filesystem probe (Section 10) where a real platform can confirm it.

| Operation | POSIX | Windows |
|---|---|---|
| exclusive create at a name | fails if an entry of the same class exists; otherwise creates an empty object and a handle | same |
| create a hard link | fails if an entry of the same class exists at the new name; otherwise adds an entry for the new name pointing at the existing object, whose other names stay (Section 16.1) | same |
| open existing without create | fails if absent; otherwise adds a handle (`shareDelete` is recorded as true and never chosen, because POSIX has no delete-sharing) | fails if absent; otherwise adds a handle with the chosen `shareDelete`; a new handle cannot be opened on an object with a pending delete |
| close a handle | removes the handle; releases its OS-native lock if held | same; lifts the sharing restriction that handle imposed, and removes a pending-delete object's name when its last handle closes |
| non-blocking OS-native lock | a lock scoped to the handle that takes it (`flock` or Linux open-file-description locks; never `fcntl` record locks, which the process loses when it closes any handle to the file); fails if another process holds it; unavailable when `LockCapability = weak` | a lock scoped to the handle (`LockFileEx`); fails if another process holds it; unavailable when `LockCapability = weak` |
| release an OS-native lock | clears `oslock` | same |
| rename, no-replace | fails if the target class exists; succeeds even if the object is open and OS-locked | fails if the target class exists; succeeds only if every open handle on the object has `shareDelete` |
| rename, replacing | atomically points the target entry at the source object, whether or not the target existed; the replaced object keeps its open handles and loses its name | same, and succeeds only if every open handle on the source object and on the replaced object has `shareDelete` |
| unlink | succeeds even if the object is open and OS-locked; open handles keep writing to the now-unnamed object | succeeds only if every open handle has `shareDelete`; then either removes the name at once (the POSIX delete semantics recent NTFS uses) or leaves it as a pending delete until the last handle closes, chosen nondeterministically per call |
| write a lock record | two steps: `WriteBegin` sets `content` to `Torn`, `WriteEnd` sets it to the new record; a reader in between sees `Torn` | same |
| flush a file | copies the object's `content` to `durable` | same |
| flush a directory | copies the directory's `entries` to `durableEntries` | same |
| look up a name | reports whether the name has an entry, without opening a handle | same |
| identity of a name | reports an identity value: under `IdentityStrength = strong`, the id of the object the name maps to; under `weak`, either that id or any id in the name's `pastObjects`. Only the reported value can repeat; the objects stay distinct | same |
| list a directory | not atomic: `ListBegin`, then one `ListNext` step per name in the directory's name set, in any order, each reading that name's entry at that moment; an entry present for the whole listing is always returned, one created or removed during it may or may not be | same |

Constants: `Platform ∈ {"posix", "windows"}`; `Fold`, the name-equivalence map (identity, or case folding);
`IdentityStrength ∈ {"strong", "weak"}`, where `weak` stands for FAT32/exFAT-like identity that CI cannot probe;
`LockCapability ∈ {"strong", "weak"}`, where `strong` stands for the spec's `LocalStrong` or `RemoteStrong` and `weak`
for `RemoteUnverified` or `Unsupported` (Section 235.1). Under `weak`, Section 235.1 refuses every operation that needs
target exclusivity with `REMOTE_LOCK_UNSAFE`, so every actor refuses before it acquires anything, and the unavailable
OS-native lock and 240.5 step 1's capability branch are reached only in the seeded run that removes that gate
(Section 8), on POSIX only; `ShareMode`, the `shareDelete` value Windows opens use. The spec does not yet state the
value at each open site (owner lock handle, classification read, recoverer, breaker, cleanup), so this work builds
only `ShareMode = "any"`: every Windows open chooses `shareDelete` nondeterministically and TLC explores every
combination, and the first Windows runs are expected to find which combinations are unsafe (Section 11). The spec fix
that states the values also adds them to the model as a per-site record.

### 5.2 Atomicity, in-flight calls, and crashes

Each row of the operations table is one atomic step, and each PlusCal label performs at most one filesystem
operation. A spec step that makes several filesystem calls (for example open, OS-lock attempt, then read) is
therefore several labels, and other actors can run between them.

A data write or rename that an operation issues as part of publishing is two steps: issue, then complete. Other
actors' steps can run between them, and the call's effect lands at completion. This is how the model represents a
stalled owner whose call "can still complete" (Section 240.5).

There are two kinds of crash.

A process crash (the process dies; the machine keeps running):

- releases its handles, their sharing restrictions, and its OS-native locks;
- leaves `content` as it is, so a record write interrupted between `WriteBegin` and `WriteEnd` stays `Torn` and every
  completed write stays visible, flushed or not;
- lands or drops, nondeterministically, each of its in-flight calls;
- loses its in-memory state; a crashed invocation never continues, and later work is done by other actors.

A host crash (power loss or reboot of the machine all actors run on):

- is a process crash of every process that has started;
- then sets, for every object written since its last flush, both `content` and `durable` to one of: the old durable
  content, the latest content, or `Torn`, chosen nondeterministically per object; a crash during a flush has the same
  outcomes. Objects flushed since their last write keep their content, and so does a file created and never
  written, which has nothing to tear;
- then keeps a prefix, in the order they were made, of the entry operations (create, hard link, rename, unlink) made
  since the last flush that covered them, and loses the rest; each kept operation applies whole (a rename is never
  half applied), and the prefix always includes every operation a directory flush covered. Both `entries` and
  `durableEntries` are set to the result. This is the ordering journaled filesystems give (ext4, APFS, NTFS); a
  filesystem without a journal can lose operations out of order, which the README lists as unverified.

  The model approximates that prefix, and the owner accepted the gap: per directory, a host crash keeps either all
  of the unflushed entry operations or none of them. A partial prefix is still reached on the filesystem, by a
  process crash of the actor whose later operations are lost, followed by a host crash that keeps the rest. But
  that costs a crash for each truncated actor on top of the host crash, where the real event costs one. So at
  `MaxCrashes = 2` the model never explores a host crash that keeps only part of what was done and then a further
  crash of a later invocation. Raising `MaxCrashes` narrows the gap only for a single truncated actor. A faithful
  prefix needs a per-directory log of unflushed entry operations and a cut point, which is not built.

These are the only ways `durable` and `durableEntries` become visible. Processes that had not started when the host
crashed run afterwards as the later invocations. At most two crashes happen in one run, counted across both kinds (one
in `claims`, Section 12); a host crash counts as one crash however many processes it stops.

In `liveness` runs, one actor of each kind a property relies on to make progress (Section 7) never has a process
crash, and a host crash, if one happens, happens before any such actor has started, so those actors run after it as
later invocations; every other actor crashes as in `check` runs. Without that rule a run where every actor able to clear a
lock crashes would violate the property for a reason the spec does not claim to cover (an operator re-running the
command is outside the model), and a host crash that spared a running process would be impossible.

The lock protocol's actors flush only where the spec tells them to (the record write of 240.5 step 6 is flushed); the
spec states no directory flush after creating, moving aside, or removing a lock file, so a host crash may undo any of
those entry operations, and the model explores what the next invocation then finds (Section 11).

## 6. Actors

### 6.1 `LockProtocol.tla`

Each PlusCal label is named `S<section>_<step>` after the spec step it implements, with dots in the section number
written as underscores. Where the spec numbers its steps, `<step>` is `s` and the number: Section 240.5 step 6 is
`S240_5_s6`. Where a heading has no numbered steps (for example 96.1, 97.1, 99, 120, 182, 183, 241.5), `<step>` is a
short name defined in `trace.toml` next to the spec sentence it implements, for example `S97_1_ancestor` or
`S182_prepare`. A step that needs more than one atomic action gets suffixes `a`, `b`, and so on (`S240_5_s6a`).

Encodings: a lock record is a TLA+ record `[op |-> <operation id>, kind |-> "operation" | "cleanup"]`; `Torn`,
`Foreign`, and `Empty` are distinct model values. Each process is a model value, and its operation id is its own
process value, so every value that names an actor (records, ghost variables, handles, in-flight calls) changes
consistently when TLC permutes processes. Symmetry reduction is declared only over interchangeable actors (same kind
and same target), and only in scenarios whose symmetry-free `liveness` runs also check every safety invariant (Section
4): the two Recoverers of `recovery` and the two Breakers of `breaklock`. All other runs use no symmetry; `dirlock` has one actor of
each kind, and `claims` has no liveness run. Every invariant and witness
quantifies over processes rather than naming one.

| Actor | Behaviour |
|---|---|
| Owner | A normal operation: acquires the target lock (96.1: exclusive create of `P/<name>.flux-lock`, then the per-name acquirer's check that `P/.flux-dir.lock` is absent; on a conflict it removes the lock it created, refuses with `TARGET_LOCK_BUSY`, and ends, as 96.1 states, with no retry) and holds its OS-native lock, runs its ancestor check (97.1 a), publishes with a Section 99 check before each write, releases the lock at completion; may crash |
| StalledOwner | An owner that stops making progress at any point and may later resume (its next Section 99 check then runs, and if the check fails it stops, closing its handles), may resume and complete normally, or may have a call in flight that completes later |
| DirOwner | An owner whose target name is too long for a per-name lock: takes `P/.flux-dir.lock` and lists `P` for per-name locks (96.1 announce-then-check); may crash |
| PlainRun | A new invocation without flags: classifies what it finds (21.1 table, 96.1, 120, 240) and acts or refuses; if it acquires the lock it continues as an Owner |
| Recoverer | A new invocation that finds a dead owner's lock and runs 240.3 steps 1-5, then continues as an Owner |
| Breaker | `flux copy --restart --break-lock`: 240.5 steps 1-6, then 21.1 steps 2-5, then continues as an Owner; refuses at 240.5 step 1 when `IdentityStrength = weak` or `LockCapability = weak` (the step requires both, spec lines 10628-10631; under `LockCapability = weak` 235.1 refuses first, so that branch is reached only in the seeded run); may crash, including between the `WriteBegin` and `WriteEnd` of its step 6 overwrite |
| CleanupBreaker | `flux cleanup --target PATH --break-lock`: 240.5 steps 1-6 with a cleanup lock record, deletes artifacts, deletes its lock; may crash |
| Cleanup | `flux cleanup DEST`: classifies and removes orphan, dead, and cleanup locks via 240.3 |
| NestedOwner | An owner whose destination is a child of another owner's destination (97.1) |

A scenario has one actor of each kind it names unless Section 12 gives a count. "Continues as an Owner" means the
actor then runs the Owner's publishing steps under its own operation id, so `SingleWriter` compares it with every
other writer of the target.

Classification of a lock is a sequence of labelled filesystem steps followed by one judgement (Section 5.2): open the
lock file, attempt its OS-native lock without blocking, read the record, then judge by what was read:

| What the steps found | Judgement |
|---|---|
| no entry at the lock path (the open failed because it is absent) | empty |
| `Foreign` | foreign |
| `Torn`, or a read that failed | uncertain (96.1, 259.6), without consulting the oracle |
| a record whose `kind` is `cleanup` | cleanup lock (251.1) |
| a record of an operation, and the OS-native lock attempt failed because another process holds it | live |
| a record of an operation, otherwise | the oracle's judgement of the record's owner, below |

An owner gives up its OS-native lock only by crashing, by completing, or by stopping after a failed Section 99 check
(it closes its handles); none of these removes a lock file whose record is not its own. So while an owner runs, the
fifth row applies to its lock.

In the last row an oracle decides the part of Section 240.2 and 240.3 evidence that is not a filesystem fact (whether
the owner's process or host still exists), consistent with the truth: live only if the owner is alive, dead only if
it has crashed, uncertain in either case. Two classifiers may disagree at different moments, as Section 240 allows.

Because the steps are separate, the lock file can be renamed, unlinked, or rewritten between the open and the read,
and the classifier then judges what its handle reads. A classifier's OS-native lock attempt can succeed; unless the
spec step it is in keeps that handle (240.3 holds the OS lock through the move-aside; 240.5 holds it through the
takeover), the classifier closes the handle, releasing the lock, as its next step after the judgement, so a refusal
never leaves a lock held on another process's file.

Ghost variables, used only by the properties: `classified[p]`, the last classification each process made of the lock
path (live, dead, uncertain, cleanup lock, foreign, empty), and `lastRecord[path]`, the record most recently present
at each lock path.

### 6.2 `Claims.tla`

One directory operation publishes into one destination directory on a case-insensitive filesystem (`Fold` is case
folding), so aliasing happens. Targets:

| Target | Role |
|---|---|
| `A` and `a` | two independent source files whose names fold to one entry; an existing destination entry `A` may or may not be present |
| `H1`, `H2` | a hardlink group: canonical `H1`, dependent `H2` whose name maps to a different entry |
| `D1`, `d1` | a hardlink group whose dependent's name folds onto the canonical's entry |

Besides the filesystem model, `Claims.tla` has one variable for the operation's state database (`state.db` with its
WAL): the set of durable records per target (PREPARE_COMMIT, COMMIT) and the set of claims. A database transaction
is one atomic step and is durable when it completes; a crash of either kind keeps every completed transaction and
none of an incomplete one.

Each target runs the Section 182 pipeline as separate steps: plan with its existing-entry claim (a transaction),
write the temporary file, flush it, PREPARE_COMMIT (a transaction), revalidate locks (Section 99), rename (a
filesystem call, issued then completed, Section 5.2; no-replace for a target planned as new, replacing for one
planned as a replacement, Section 241.5), flush the destination directory, then one transaction that writes COMMIT
and the target's created-entry claim together. Planning uses the look-up operation (Section 5.1) to decide whether a
target is planned as new or as a replacement; a no-replace rename that fails because the name is now taken is reported
as `DESTINATION_NAMESPACE_COLLISION` (241.5) and ends that target. A transaction includes its WAL flush, so it is durable when it
completes. The steps are separate, so a crash can fall between any two; a crash between the rename and COMMIT is the
case Section 183 recovery handles, and a host crash before the directory flush can undo the rename.
Dependents run the Section 16.1 link procedure (a hard link at a partial name, then a rename over the target). An
environment action, `LockLost`, can take the operation's lock over, at most once per run, at any point (a `--break-lock` from outside), after which every worker's next revalidation fails and it stops. A crash
can happen between any two steps; a resume then runs Section 183 recovery before continuing.

The scenario runs one configuration per target group, so that each state space stays small: `A`/`a` (one
configuration whose initial states include both an existing entry `A` and none), `H1`/`H2`, and `D1`/`d1`. In each, two workers take the group's targets from a shared queue
in any order and interleave freely.

## 7. Properties

Safety invariants, which must hold in every reachable state:

| Name | Statement |
|---|---|
| `SingleWriter` | For a target, at most one process is inside a publishing step whose last Section 99 check passed. The one exception the spec accepts, a stalled prior owner's in-flight call issued before an operator `--break-lock` and completing after it, is permitted only in that case. |
| `PlainNeverOwnsUncertain` | A process without `--break-lock` never removes, renames, or overwrites a lock it classified as uncertain, and creates a lock only at an empty lock path. "Classified as uncertain" means either judgement: `classified` is uncertain (a torn or empty record), or `ownerLive` is (the oracle could not tell a cleanup lock's owner is dead), because the decision tables refuse `TARGET_LOCK_UNCERTAIN` for both. |
| `RefusalJustified` | A regression guard, not a liveness check. Every `TARGET_LOCK_BUSY` refusal rests on evidence that the lock was held, that its owner was alive, or that the lock path holds a live owner's record. Since spec 240.2 was corrected, a held OS-native lock shows only that the lock is held - it cannot tell the owner from another invocation inspecting or recovering it - so this invariant cannot establish owner liveness. What it catches is a refusal with nothing behind it: a decision table that refuses `TARGET_LOCK_BUSY` for a lock it judged dead, or a publisher refusing it at the Section 99 check when the lock path is empty or foreign. Every refusing label records its evidence through one definition, `RefusalEvidence`, so the seeded run of Section 8 guards all of them at once; a change that bypasses the definition at a single label is not caught (accepted, test audit 2026-09-14). |
| `ForeignUntouched` | A `Foreign` object at a lock path is never written, renamed, or deleted. It is a state predicate, not a ghost flag: the `Foreign` object an initial state holds, if any, is still the object at the lock path and still holds `Foreign`. No actor ever writes `Foreign`, so an invariant about it means something only where an initial state holds one: every `recovery` configuration starts from either an empty lock path or one holding a `Foreign` object (`FsWith`), and the empty start keeps the whole acquisition prefix a `check` run needs (Section 12). `SEED_RECOVER_FOREIGN` exercises the identity half; nothing exercises the content half, because a `Foreign` object exists only in the initial state, every record write in the model follows its writer's own successful exclusive create at the lock path (96.1's record write, 240.3 step 4's), and no such create succeeds while the `Foreign` object holds the lock path. The write operation itself checks no handle, so this rests on that ordering, not on a guard. A `Foreign` object that appears mid-run, after an actor's check and before its remove by name, is not modelled (test audit 2026-09-14). |
| `Classifiable` | In every state, each lock path classifies into exactly one case of the Section 6.1 judgement table: empty, foreign, uncertain, cleanup lock, live, or (by the oracle) dead; a `Torn` record classifies as uncertain. |
| `NestedExclusion` | Two operations whose destinations nest never both write objects under the inner destination. |
| `DirLockExclusion` | A per-name lock holder and a directory-lock holder in the same directory `P` are never both past their 96.1 check. |
| `NoSilentOverwrite` (Claims) | No target overwrites an entry this operation already published. |
| `CommittedHasClaim` (Claims) | Every target with COMMIT has its created-entry claim. |
| `CommittedIsDurable` (Claims) | After any host crash, every target with COMMIT still has its destination entry pointing at the object it published. |
| `NoSelfCollision` (Claims) | A resumed target never collides with its own claim; a hardlink dependent whose name maps to a different entry never collides with its group's claims; a dependent folded onto its canonical's entry is reported as a collision. |
| `NoPublishAfterLockLost` (Claims) | No rename lands after `LockLost` unless it was issued (in flight, Section 5.2) before `LockLost`: the same single exception `SingleWriter` accepts, following 240.5's "a filesystem call it had already started can still complete". A revalidation that passed before `LockLost` does not by itself permit a later rename. |

Reachability: every path the model claims to explore is proved reached, and how depends on what the path is.

A path a LABEL performs is proved by that label's coverage count in its scenario's `check` and `liveness` runs
(Section 4), which is stronger than a witness invariant: the run fails unless EVERY label of every actor it runs is
covered, not only the ones someone thought to name. The table below says which label proves each path the earlier
design named as a witness, so the intent is kept and nothing is lost:

| Path | Proved by the coverage of |
|---|---|
| an operation acquires the target lock | `S96_1_record_end` |
| a Recoverer completes 240.3 | `S240_3_s5` |
| a Breaker or CleanupBreaker completes 240.5 | `S240_5_s6` |
| Cleanup removes a lock | `S251_1_delete` |
| a stalled owner's in-flight call completes after a takeover | `S240_5_inflight_lands` |
| a DirOwner passes its 96.1 check holding the directory lock | `S96_1_dirowner_check` |
| a per-name or directory acquirer backs off on a 96.1 conflict | `S96_1_backoff` |
| a target commits with its claim (Claims) | `S182_commit` |

A fact no single label states keeps a ghost flag or a state predicate, and its scenario has a `witness` run for it
(Section 4): TLC without `-continue` stops at the first state that has it, which proves the fact and prints one trace.

| Witness run's invariant | Passes when TLC stops because |
|---|---|
| `NeverTornRead` | a process read a `Torn` record |
| `NeverChecked` | a process passed its Section 99 check; without it, deleting the one assignment that sets `checked` would leave `SingleWriter` true in every state and every run green (test audit 2026-09-14) |
| `NeverRecoveredAfterCrash` | a lock a crash had left was replaced |
| `NeverClassifiedCleanupLock` | a PlainRun or Recoverer classified a cleanup lock |
| `NeverDeadLockWithPendingMover` | a lock whose owner is dead sat at the lock path while no further crash could occur and a Recoverer or Cleanup had yet to run |
| `NeverUncertainOwnerLock` | a lock whose owner is uncertain sat at the lock path |
| `NeverLockLostMidCommit` (Claims) | `LockLost` fell between a PREPARE_COMMIT and its rename |

Each scenario lists its `witness` runs in Section 12. `NeverDeadLockWithPendingMover` is the witness for the antecedent
of `DeadLockEventuallyCleared` below; `NeverUncertainOwnerLock` stands in for `UncertainLockEventuallyCleared`'s until
that property's scenarios are built, when it has to be narrowed to the conditioned antecedent in the same way. A
scenario that checks such a property always has the matching `witness` run: that is what keeps a liveness run from
passing over a state space that never reaches its antecedent.

Liveness, checked in `liveness` runs under weak fairness. PlusCal's `--fair algorithm` generates `WF_vars(Next)`, which
is fairness over the whole next-state relation rather than per process; it is enough here only because no process
loops, so none can be starved forever. Fairness constrains infinite behaviours only, and does not change any `check`
run's reachable states (measured: identical counts before and after it was added).

- `DeadLockEventuallyCleared`:
  `[](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>(~DeadOwnerLock \/ UncertainReported))`. A lock whose owner is
  dead is eventually removed or replaced, or an actor reports it cannot establish that the owner is dead - which
  Section 240.4 preserves rather than clears - provided no further crash can occur (`EnvQuiet`) and a Recoverer or
  Cleanup has yet to run (`PendingMover`).
- `UncertainLockEventuallyCleared`: the same for an uncertain owner's lock, conditioned the same way on a Breaker or
  CleanupBreaker that has yet to run (only an operator action clears it, Section 240.4). Not yet measured: its
  scenarios are not built, and its witness above has to be narrowed to that antecedent when they are.

The two conditions are what make the property checkable at all, not a weakening chosen for convenience. Measured on
`recovery` (2026-09-12), the unconditioned `[](DeadOwnerLock => <>(~DeadOwnerLock))` is violated. The reason is not
particular to `recovery`, so it is argued, not measured, for every other model of the same shape: with a fixed, finite
set of actors that all finish, the last crash can always fall after the last actor has acted, and no action is enabled
in the state that leaves. No retry and no fairness condition changes that - even a recoverer that retries without
bound correctly exits when it sees a live owner, and the owner can die afterwards. The prose of this section already
said "given a Recoverer or Cleanup actor"; the formula had dropped it.

The conditions also bound what a passing run shows, and the bound is worth stating. `PendingMover` requires an actor
that has not started, so the property says nothing about a lock that dies while every Recoverer and Cleanup is already
under way - one whose owner crashes after the last of them has begun classifying. Whether those in-flight actors still
clear it is left to the `check` runs' invariants and to coverage, neither of which proves it. A later liveness property
over actors already in flight would have to condition on where each one is, and is not attempted here.

These are linear-time properties TLC can check. The stronger "from every state some action clears it" is branching
time and is not claimed.

## 8. Defect-seeded configurations

Each seeded configuration sets one flag that re-introduces a defect the adversarial review fixed, or removes one rule
the spec states (the rows without a review finding), in the scenario that can reach it. `just model` requires each to stop with the listed violation; if one passes, the model has lost the
ability to see that defect and the run fails.

| Flag | Defect re-introduced (review finding) | Scenario | Must fail |
|---|---|---|---|
| `SEED_RECOVER_FOREIGN` | a `Foreign` object at the lock path is judged replaceable and moved aside through 240.3, where the spec refuses it with `CONTROL_PLANE_NAMESPACE_CONFLICT` | `recovery`, POSIX, Owner and 2 Recoverers | `ForeignUntouched` |
| `SEED_RECOVER_UNCERTAIN` | a lock whose record is torn or empty, judged uncertain, is moved aside through 240.3, where 240.4 preserves it; only the `classified` half of `PlainNeverOwnsUncertain`'s judgement reports it | `recovery`, POSIX, Owner and 2 Recoverers | `PlainNeverOwnsUncertain` |
| `SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK` | a cleanup lock whose owner the oracle could not judge dead is moved aside through 240.3 (251.1 and 259.6 replace only a dead one); only the `ownerLive` half of `PlainNeverOwnsUncertain`'s judgement reports it | `recovery`, POSIX, Owner, Recoverer and Cleanup | `PlainNeverOwnsUncertain` |
| `SEED_DEAD_AS_BUSY` | a plain rerun refuses a dead owner's lock with `TARGET_LOCK_BUSY` instead of `RESUMABLE_OPERATION_EXISTS` (21.1) | `recovery`, POSIX, Owner, Recoverer and PlainRun | `RefusalJustified` |
| `SEED_RECOVERER_IDENTITY_ONLY` | 240.3 step 3 checks identity only, not the record (round 6, IMC-4) | `mixed` | `SingleWriter` |
| `SEED_EMPTY_PATH_BUSY` | 240.5 step 6 refuses when the lock path is empty instead of retrying (round 6, IMC-3) | `breaklock` | `RefusalJustified` |
| `SEED_RESTART_RELEASES` | 21.1 step 5 releases the lock before the new operation starts (round 3) | `breaklock` | `SingleWriter` |
| `SEED_MOVE_ASIDE_FOR_UNCERTAIN` | an uncertain owner's lock is moved aside, leaving the path empty (round 5) | `breaklock` | `PlainNeverOwnsUncertain` |
| `SEED_RENAME_OVER_TAKEOVER` | takeover by renaming a new record over the lock (round 3) | `breaklock` | `SingleWriter` |
| `SEED_NO_IDENTITY_RECHECK` | 240.5 step 6 skips its identity check after the write (round 6) | `breaklock` | `SingleWriter` |
| `SEED_NO_CAPABILITY_GATE` | neither Section 235.1's refusal nor 240.5 step 1's capability check is applied, so operations and takeovers run with no OS-native lock and two Breakers are no longer serialized by it | `breaklock`, weak-capability variant | `SingleWriter` |
| `SEED_CLEANUP_LOCK_UNVERIFIABLE` | Section 120 rejects `workspace_path = none` (round 6) | `cleanup-crash` | `DeadLockEventuallyCleared` |
| `SEED_TORN_AS_FOREIGN` | a checksum-failing record is treated as a foreign object (round 5); `mixed` has both a record torn by the Owner's crash and a Breaker, the only actor that can clear it | `mixed` | `UncertainLockEventuallyCleared` |
| `SEED_NO_ANCESTOR_CHECK` | 97.1 (a) skipped | `nested` | `NestedExclusion` |
| `SEED_DESCENDANT_EXISTING_ONLY` | 97.1 (b) checks only directories that already exist (round 2) | `nested` | `NestedExclusion` |
| `SEED_CHECK_BEFORE_ANNOUNCE` | the directory acquirer lists `P` before creating `P/.flux-dir.lock` (96.1's announce-then-check order reversed) | `dirlock` | `DirLockExclusion` |
| `SEED_PERNAME_SKIPS_DIR_CHECK` | a per-name acquirer does not check that `P/.flux-dir.lock` is absent | `dirlock` | `DirLockExclusion` |
| `SEED_CLAIM_AFTER_COMMIT` | created-entry claim written after COMMIT (round 6, CPE-4) | `claims`, group `A`/`a` | `CommittedHasClaim` |
| `SEED_CLAIM_BY_OBJECT_ID` | claims keyed by object identity (round 3) | `claims`, group `H1`/`H2` | `NoSelfCollision` |
| `SEED_NO_PUBLICATION_CLAIM` | publications do not claim the entry they create (round 2) | `claims`, group `A`/`a` | `NoSilentOverwrite` |
| `SEED_NO_REVALIDATE_BEFORE_RENAME` | Section 182 renames without revalidating locks | `claims`, group `A`/`a` | `NoPublishAfterLockLost` |

The four `recovery` rows came from the test audit of 2026-09-14 and the capstone reviews that followed it, which found that no run of the scenario could tell
its safety invariants from `TRUE`: an invariant no seeded run violates passes just as well with its ghost deleted. So
every protocol invariant a scenario checks either has a seeded run in that scenario or states which scenario's seeded
run guards it. `FsOk` and `Classifiable` are exempt: they check the model's own assumptions (Section 5 and the
judgement table of Section 6.1), not a rule of the spec, so no protocol defect is theirs to catch. `SingleWriter`'s seeds need a takeover or a second recoverer judging by identity alone, so they belong to
`mixed` and `breaklock`; in `recovery` the witness `NeverChecked` (Section 7) shows at least that its ghost is set. A
seeded configuration lists the scenario's safety invariants, not only the one it must fail, so a seed that breaks a
different invariant first fails its run.

A seeded run whose "must fail" entry is a liveness property is still a `seeded` run (its `violated` names the property),
checked with the liveness settings: TLC checks the scenario's temporal properties, run
without symmetry. Every later fix that the model drives adds a row here.

`SEED_CLEANUP_LOCK_UNVERIFIABLE` and `SEED_TORN_AS_FOREIGN` mean something only against the conditioned liveness
properties of Section 7. Their unconditioned forms are violated with or without a seed - measured for
`DeadLockEventuallyCleared` on `recovery`, and argued for these two seeds' own scenarios, which are not built, from the
same last-crash reason - so a seeded run against them would stop with the named violation and pass while proving
nothing about the seed - and
Section 4 already forbids a seeded run whose scenario carries an open finding on the same property, which an
unconditioned property would always be. Each of these seeded runs is therefore judged against a scenario whose own
liveness run passes the conditioned property first.

## 9. Spec-drift stamp

`spec-sections.stamp` begins with one line `spec: <path>` naming the spec file relative to the repository root; this
is the only place the model tooling names it, and the test fails if that file does not exist, so renaming the spec
(a V17) fails `just check` until the stamp names the new file. Then it lists, one per line, each spec heading the
model encodes, exactly as the heading line appears in the spec: 96.1, 96.2, 97.1, 99, 99.1, 21.1, 240.1, 240.2,
240.3, 240.4, 240.5, 251.1, 251.2, 259.6, 120, 235.1, 241.5, 182, 183 at the end of the work; a heading is added to
the stamp, with its `trace.toml` entries, when the model starts to encode it. A heading's own text runs from its heading line up
to, not including, the next heading line of any level, so a subsection the model does not encode is not part of its
parent's text; a subsection the model does encode is listed as its own line. Heading lines are ATX headings (`#` to
`######`) outside fenced code blocks, and, for the family rule below only, a Setext heading (a line of text underlined
by `=` or `-`), so that a new rule cannot be introduced under a heading the check does not see; the spec has 264 such
underline lines today and none follows a text line, so none is a Setext heading (measured 2026-09-12). Line endings are
normalised to LF before hashing.

A heading's number is the first word after its `#` marks, without a trailing `.` (`96.1` in `## 96.1 Name-Equivalent
Target Locks`, `99` in `# 99. Commit-Time Lock Revalidation`), when that word is digits separated by dots. A heading
without such a number (for example `## Stand-downs`) takes, for the family rule below, the number of the nearest
numbered heading above it; if there is none (the document's title), it has no number and belongs to no family. A new
heading next to a stamped one changes no stamped unit (a `96.3` added between 96.2 and 97 leaves 96.1's text as it
was), so the test also checks each stamped heading's family. For a stamped heading whose number has more than one
part, let `M` be the number without its last part (`96` for `96.1`, `259` for `259.6`); its family is every heading
numbered `M` or beginning `M.`. For a stamped heading with a one-part number `N` (`99`, `120`), its family is every
heading numbered `N` or beginning `N.`, so an unnumbered heading placed under `# 99.` belongs to it. Each heading in a
family other than a stamped one must be listed in `trace.toml` as a `[[heading]]` entry: the key `heading` holding the
heading line exactly as in the spec, the key `family` holding the number whose family it is in (`96`, `99`), and
exactly one of `not_modelled = "<reason>"` or `pending = [<scenario>, ...]` (Section 9.1); a heading whose text the model
encodes is stamped, never listed. Because the same heading line can appear more than once in the spec (`## Normal`
appears twice today, in sections the model does not encode), an entry also carries `count`, the number of times that
line occurs in that family, and the test fails when the number changes; so a new section cannot hide behind the name of
one already listed. So a new
sibling or child heading, numbered or not, fails the check until someone decides whether the model encodes it.

The hashes live in `trace.toml`, one per unit (Section 9.1): the BLAKE3 hash, in lowercase hex, of the unit's text.
`tests/model_stamp.rs`, an auto-discovered test target of the root package (so `just check` runs it), recomputes
them. It fails if a listed heading is missing or appears more than once, and, for each unit whose hash differs, names
the heading, the unit's ordinal, and the labels that `trace.toml` says implement it, so a spec change points at the
exact model steps to re-check. For a family heading with no entry it names that heading line and the stamped heading
whose family it belongs to, so the reader knows which section grew. `blake3` is added to the root package's `[dev-dependencies]` from the workspace, and
`toml` (added to `[workspace.dependencies]`, subject to `deny.toml`) for reading `trace.toml`. The same file holds an
`#[ignore]`d test that rewrites the unit hashes; `just model-stamp` runs it after a green `just model` (Section 4).
The workflow after a spec change: re-check each unit the test names against the model, update the model and
`trace.toml`, run `just model`, then run `just model-stamp`.

### 9.1 Traceability check

`trace.toml` is the single traceability map. Its unit is a piece of a stamped heading's own text (Section 9), and every
text line after the heading line belongs to exactly one unit: each block of lines separated by blank lines or by
fence-marker lines (which belong to no unit) is a unit, except that each numbered step, indented or not, starts a new
unit, and a numbered step together with the indented lines after it is one unit even across blank lines. The numbered
steps of 21.1 and 240.1 sit inside fences, and each is a unit. Prose before, between, or after numbered steps is
therefore covered like any other text. A unit's text, which is what is hashed, is its lines joined by LF. Each entry names its heading and the unit's ordinal, quotes
a sentence from that unit, carries the unit's hash (Section 9), and gives the labels that implement it,
`not_modelled = "<reason>"`, or `pending = [<scenario>, ...]`, and at least one of the three. A `pending` entry also
carries the labels the scenarios already built contribute to that unit, if any, so that partial work is recorded and
every label in a model file still has an entry naming it. Labels and `not_modelled` together mean a partly modelled
unit: the labels implement what the model encodes, and the reason says what it does not and why.

`pending` marks a unit, or a family heading, that scenarios not yet built will model: the model is built one scenario
at a time (Section 12), and a heading such as 96.1 holds rules for several scenarios. `trace.toml` lists the scenarios
still to be built in one top-level `planned_scenarios` array. A `pending` array names every scenario still owed labels
for that unit; each name must be in `planned_scenarios`, and a scenario in `planned_scenarios` must not be in
`expected.toml`'s `scenarios` list. A misspelt name therefore fails at once, and moving a scenario from
`planned_scenarios` into `expected.toml`, as the plan that builds it must, fails every entry that still names it: that
plan adds the labels its scenario contributes and removes its name, so no scenario can leave its share of a shared unit
to the next one. The entry keeps `pending` while other scenarios are still owed, and the key goes away with the last
name rather than being left as an empty array. When every scenario is built, `planned_scenarios` is empty and no
`pending` entry remains.

The same test file checks, without Java:

- every unit of every stamped heading has exactly one entry, each quoted sentence occurs in its unit, and each hash
  matches its unit;
- every heading in a stamped heading's family (Section 9) is stamped or covered by a `[[heading]]` entry for its line
  and family, and every `[[heading]]` entry names a family that some stamped heading defines, quotes a heading line
  that occurs in that family exactly `count` times, is not stamped, and is the only entry for that line and family;
- every name in a `pending` array is in `planned_scenarios`, and no scenario is in both `planned_scenarios` and
  `expected.toml`'s `scenarios`;
- every `scenario` in `expected.toml`'s `deferred` list (Section 4) is in `planned_scenarios`, and each `deferred`
  label exists in a model file and appears in no more than one `deferred` entry;
- every label in `LockProtocol.tla` and `Claims.tla` (matched by `S\d+(_\d+)*_\w+:`) appears in `trace.toml`;
- every label `trace.toml` names exists in a model file.

Because units are counted from the spec text, an entry cannot be left out, and a spec edit that adds a unit fails the
check until the map covers it. The README renders the map for readers but is not the source of truth.

## 10. Filesystem probes

Rust tests in `crates/flux-platform/tests/fs_semantics.rs`, run on Linux, macOS, and Windows by the existing test
job. They use `rustix` on Unix (`flock` for the OS-native lock, `fstat`, `renameat2`/`renamex_np` for no-replace
renames) and
`windows-sys` on Windows (`CreateFileW` share flags, `LockFileEx`, `GetFileInformationByHandleEx` with `FileIdInfo`, `DeleteFileW`, and
`MoveFileExW` with and without `MOVEFILE_REPLACE_EXISTING`) as `flux-platform` dev-dependencies, subject to `deny.toml`; no standard
library API newer than the workspace's `rust-version` (1.85) is used, because clippy's MSRV lint would fail the gate.
`tempfile` (already pinned in the workspace) provides each probe's scratch directory. Each probe is compiled only on
the platforms its row lists (`#[cfg]`), and each platform runs at least four probes. Each probe first asserts its own
precondition, so it cannot pass without exercising what it claims: FS-4, for example, confirms the OS-native lock is
held (a second handle's non-blocking attempt fails) before it renames.

| Probe | Assumption | Platforms |
|---|---|---|
| FS-1 | a second exclusive create of the same name fails | all |
| FS-2 | a no-replace rename fails when the target name exists | all |
| FS-3 | a non-blocking OS-native lock fails while another handle holds it | all |
| FS-4 | renaming an open, OS-locked file succeeds | Linux, macOS |
| FS-5 | unlinking an open file succeeds and the open handle still writes to the unnamed object | Linux, macOS |
| FS-6 | renaming or deleting a file open without delete-sharing fails, and so does a replacing rename onto a file open without delete-sharing | Windows |
| FS-7 | renaming or deleting a file open with delete-sharing succeeds, and so does a replacing rename onto one; whether the deleted name disappears at once or stays pending until the handle closes (the probe prints which; both are allowed by the model) | Windows |
| FS-8 | a name replaced by a new file reports a different file identity | all |
| FS-9 | closing a handle releases its OS-native lock | all |
| FS-10 | a directory listing returns every entry that exists for the whole listing, including one created just before it starts | all |
| FS-11 | the OS-native lock is scoped to its handle: a process that holds it, opens a second handle to the same file, and closes that second handle still holds the lock | all |

The replacing renames of FS-6 and FS-7 use `std::fs::rename`, which on current Rust performs a POSIX-semantics rename
on Windows; that is the behaviour the model's Windows row describes. Measured on Windows 11 NTFS while plan 1 was
written, `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` refuses any open target even with delete-sharing, so FS-7 prints
what it does. The spec does not name the Windows API for a replacing rename; plan 2 records this as a finding.

FS-8 reads a Windows file's identity as the 64-bit volume serial number plus the 128-bit file id from
`GetFileInformationByHandleEx(FileIdInfo)`, and a Unix file's as `(st_dev, st_ino)` widened to the same type. The
64-bit index from `GetFileInformationByHandle` is documented as not guaranteed unique on ReFS, which Windows Dev Drives
use. The probes pass on NTFS and ReFS (measured while plan 1 was executed). The spec does not say where a Windows
identity comes from; plan 2 records this as a finding.

Each probe prints the filesystem type of its scratch directory (`statfs` on Unix, `GetVolumeInformationW` on Windows),
or `unknown` if that call fails; the call's failure never fails the probe, and `unknown` counts as not native for
triage. A failing probe is never weakened to pass. It is triaged first as environment or platform: if the filesystem type is
not the platform's usual local one (for example an overlay or network filesystem on a runner), the failure is
reproduced on a native local filesystem of that platform before anything changes; if it reproduces, the model's
assumption for that platform is wrong and the model is fixed.
Some model rules no probe can confirm, and the README lists them as unverified assumptions: `IdentityStrength =
weak` and the case-folding behaviour of FAT32/exFAT (no such filesystem on CI runners), including that, having no
journal, they may lose entry operations out of order after a host crash; both crash rules of Section
5.2, including which unflushed writes and entry operations survive a host crash (no probe can cut power; the rules
follow the platforms' documented guarantees, spec Sections 166 to 168); and whether an entry created or removed
during a listing is returned (the model allows either, which covers both).

## 11. Handling findings

A counterexample in a `check` or `liveness` run is saved with its trace and triaged:

- Model defect: fixed in the model.
- Spec defect: recorded in `TODO.md` or `.clavity/local-anomalies.md` and listed in the failing runs'
  `open_findings` (Section 4), so the gates stay green for every other change while it is open; then fixed in the
  spec on the spec branch. A design fork goes through an agy-first consult and then to the owner. The fix removes the
  `open_findings` entries in the same change, gets a new seeded configuration (Section 8), and the unit hashes are
  updated.

Findings the design already expects, each to be confirmed or refuted by the first runs:

- Windows sharing mode: with `ShareMode = "any"`, some combination of open sites without delete-sharing should break
  an invariant, which forces the spec to state the mode at each open site.
- The check-to-call window: Section 99 requires revalidation and rename "without an intervening operation-state
  transition that would invalidate ownership", while Section 240.5 accepts only a call "already started" when a
  takeover lands. A takeover between a passing check and the issue of the call is covered by neither, and no
  implementation can close that window, so `SingleWriter` and `NoPublishAfterLockLost` should report it; the fix
  is a spec statement of the accepted window.
- The directory acquirer lists `P` "for *.flux-lock held by other operations" (96.1), without saying whether a dead or
  uncertain owner's per-name lock counts as held. The model classifies each listed lock as Section 240 does and treats
  live and uncertain as held and dead as not, as 97.1 (a) does for ancestor locks; `trace.toml` records this reading
  as an assumption until the spec states it.
- A `--break-lock` takeover of a per-name lock (Section 240.5) never checks `P/.flux-dir.lock`, which Section 240.5
  does not mention. If a directory acquirer judged that per-name lock dead and proceeded, a Breaker that judges the
  same lock uncertain and takes it over in place would hold it alongside the directory lock, so the `dirlock`
  scenario includes a Breaker and `DirLockExclusion` should report it.
- The spec never flushes the directory after creating, moving aside, or removing a lock file (Section 5.2). A host
  crash can therefore undo a lock's creation, leaving an operation recorded in `state.db` whose lock is gone, or undo
  a move-aside, bringing a replaced lock back; the model explores what the next invocation does in each case.
- Section 96.1 allows "a byte-range lock or `flock`" as the OS-native lock. On POSIX a byte-range lock taken with
  `fcntl` is lost when the process closes any handle to the file, which the model does not allow (Section 5.1, FS-11);
  the expected fix is a spec statement that the lock is scoped to its handle (`flock`, open-file-description locks,
  or `LockFileEx`).

## 12. Scenarios, bounds, and time budget

Each configuration is one scenario on one platform variant. A scenario names the actors that run together; keeping
them apart keeps each state space small. Besides the protocol scenarios below, `expected.toml` has a `selftest`
scenario: a small model (`Smoke.tla`) with a check run carrying a witness and an open finding, a liveness run, and a
seeded run, which exercises every way `run.py` judges a run in a few seconds. It lands with plan 1, before any
protocol model exists, and stays as the runner's own regression check.

| Scenario | Actors | Variants | Its `witness` runs (Section 7); every other path is proved by label coverage |
|---|---|---|---|
| `recovery` | Owner (crashes), 2 Recoverers, PlainRun, Cleanup - **paired across four `check` runs, never all at once** (below) | POSIX, Windows | `NeverTornRead`, `NeverChecked`, `NeverRecoveredAfterCrash`, `NeverDeadLockWithPendingMover`; POSIX also `NeverHostCrashChangedLock` (Section 12, host crashes) |
| `breaklock` | StalledOwner, 2 Breakers, PlainRun | POSIX, Windows; POSIX weak-capability (seeded run only: with `LockCapability = weak` every actor refuses under 235.1, so no path of the scenario is reached there) | `NeverTornRead`, `NeverUncertainOwnerLock` |
| `mixed` | Owner (crashes; its lock is dead), Breaker (may see it uncertain), Recoverer (may see it dead), PlainRun | POSIX, Windows, POSIX weak-identity | `NeverRecoveredAfterCrash`, `NeverUncertainOwnerLock`; in the weak-identity variant the Breaker refuses at 240.5 step 1, so its labels are listed `unreached` there (Section 4) |
| `cleanup` | StalledOwner, CleanupBreaker, Breaker | POSIX, Windows | `NeverUncertainOwnerLock` |
| `cleanup-crash` | CleanupBreaker (crashes), PlainRun, Recoverer, Cleanup | POSIX, Windows | `NeverClassifiedCleanupLock`, `NeverDeadLockWithPendingMover` |
| `nested` | Owner on the parent destination, NestedOwner on the child, PlainRun | POSIX | none: every path of this scenario is a label |
| `dirlock` | an Owner with a per-name lock in directory `P` (may crash), DirOwner on `P` (may crash), Recoverer, Breaker; one per-name Owner suffices for every `dirlock` path and seed, and keeps the state space within budget | POSIX, Windows | `NeverRecoveredAfterCrash` |
| `claims` | Claims model, 2 workers, `LockLost` at most once, one crash and resume; one configuration per target group (Section 6.2) | POSIX with case folding (as APFS by default), Windows | `NeverLockLostMidCommit` |

Every variant of a scenario has the `witness` runs in its row, except where the row says otherwise, and its `check`
run covers every label of the actors it runs (Section 4). A variant that cannot reach a path is a change to this table,
or an `unreached` entry with its reason, never a silent edit to `expected.toml`.

`recovery` runs its actors in pairs because all four together do not finish: measured, 6,290,483 distinct states at
about twenty-one minutes with 1,866,860 still queued and growing, against a ten-minute per-run budget. Three actors
do finish, so each POSIX `check` run of the scenario takes one pairing, and all four are measured to exhaustion and
clean (`MaxCrashes = 2`; first on 2026-09-12, and again with the counts below):

| Run | Actors | `MaxObjs` | Distinct states, POSIX / Windows | What only this pairing reaches |
|---|---|---|---|---|
| `recovery-<platform>-check` | Owner, 2 Recoverers (`SYMMETRY`) | 3 | 950,004 / 1,393,668 | two recoverers racing for the same lock, which is what 240.3 step 2 is about |
| `recovery-<platform>-plain-check` | Owner, Recoverer, PlainRun | 4 | 1,645,500 / 2,334,831 | a plain rerun (21.1) meeting a dead owner's lock a recoverer is working on |
| `recovery-<platform>-cleanup-check` | Owner, Recoverer, Cleanup | 5 | 1,199,193 / 1,709,754 | two movers of different kinds, both entitled to move the lock aside |
| `recovery-<platform>-plain-cleanup-check` | Owner, PlainRun, Cleanup | 6 | 1,067,868 / 1,471,158 | the plain rerun's own 240.3 path, which needs a dead cleanup lock |

Counts are as measured on CI on 2026-09-14, with the `Foreign` start state of Section 7, which adds 24,810 states to each
run (12,669 under `SYMMETRY`), 0.9% to 2.4%. Before that, on 2026-09-13, the acquirer rule of spec 96.1 and 240.3 step 4 became unconditional in the model,
and each state space shrank by about a fifth, because an actor that cannot take its OS-native
lock now removes its file and stops instead of carrying on.

The fourth run carries no Recoverer deliberately. `plain_recover`, `plain_recovered` and `plain_recovered_done` are
reached only by a plain rerun that finds a dead CLEANUP lock, which only a Cleanup that created one and then crashed
leaves behind; no other pairing covers them. Together the four cover every label of the scenario's actors except
`S96_1_backoff`, the 96.1 directory-lock conflict that belongs to `dirlock`, and `S240_3_putback`, which needs a
record rewritten in place by a 240.5 takeover and so belongs to `breaklock`; both are `unreached` entries in all
four.

Pairing moves work onto the `unreached` lists, and those lists are per run. A label an actor of the run owns but
this pairing cannot reach must be listed in THAT run's entry with its reason, even though another pairing covers it:
the three plain-rerun recovery labels above are `unreached` in `recovery-posix-plain-check`, which runs a PlainRun
and so owns them, and covered in `recovery-posix-plain-cleanup-check`. The suite-wide rule is what makes that safe -
it is the union across runs that may leave nothing uncovered, not any single run. A label belonging to an actor a
pairing does not instantiate needs no entry at all: TLC reports no action for an empty process set, so those labels
never appear in that run's report.

Windows pairs the same way, with the same four actor sets under `Platform = "windows"`, named
`recovery-windows-check` and so on. The pairing is forced by the number of concurrent actors, which the platform
does not change.

The POSIX weak-identity variant is dropped from this scenario. Measured, it explores a state graph identical to the
strong-identity one (1,531,965 generated, 473,328 distinct, an identical coverage table): the scenario's only
identity query is on `BrokenOf(self)`, a name each recoverer renames into exactly once, so `past` can never hold a
second id for it and `FsIdentityChoices` returns a singleton either way. A scenario that reuses a name is what
exercises weak identity.

Liveness runs: `DeadLockEventuallyCleared` in `recovery` and `cleanup-crash`, whose `NeverDeadLockWithPendingMover`
witness run shows the state it is about occurs; `UncertainLockEventuallyCleared` in `breaklock`, `cleanup`, and `mixed`
(so that `SEED_TORN_AS_FOREIGN` is judged against a passing run of the same scenario), with `NeverUncertainOwnerLock`;
POSIX variant only, without symmetry.

What `recovery` found, and what changed because of it. Its liveness run, once the property was conditioned, was
violated over the complete state space: an acquirer could lose the race for its own freshly created lock file to an
invocation inspecting it, write its record holding nothing, and a later classifier would read the other invocation's
OS-native lock as a live owner, so two recoverers each refused a dead owner's lock and it was never cleared. Spec
96.1 and 240.3 step 4 now require an acquirer to hold its OS-native lock before writing its record, and to remove its
file and start again when it cannot; with that rule the liveness run holds (1,869,534 distinct states) and every
`check` run passes on both platforms. The same root cause also produced a misreport the model then saw once its
refusal invariant was tightened: `TARGET_LOCK_BUSY` naming a dead owner because another recoverer held the lock. A
bounded retry before judging "live" does not remove that in an untimed model - the scheduler can always run the retry
before the other invocation moves - so it is left to the CLI as a heuristic, and spec 240.2 and 96.2 instead say what
the refusal can honestly claim: that the lock is held, not that its recorded owner is alive. Under that meaning the
refusal is correct rather than a misreport, so the model no longer flags it: the tightening that exposed it was
removed, and `RefusalJustified` is now the regression guard Section 7 describes.

That liveness run covers an Owner and two Recoverers only - `PlainRuns` and `Cleanups` are empty in its configuration -
and the gap is not academic. With an Owner, one Recoverer and one PlainRun (one crash, no symmetry), the conditioned
property is violated over the complete state space (583,626 distinct states, 2026-09-13): a plain rerun takes the lock
to inspect it and judges the owner dead, the waiting Recoverer's try-lock fails meanwhile so it refuses
`TARGET_LOCK_BUSY`, and the plain rerun then correctly refuses `RESUMABLE_OPERATION_EXISTS` (Section 21.1) and lets go.
Both finish and the dead lock stays. The two-Recoverer run never shows this because there a tester that does not
recover is a Recoverer reporting the owner uncertain, an outcome the property admits; a plain rerun is the one tester
that refuses for another reason. Admitting "some invocation was refused `TARGET_LOCK_BUSY`" as an outcome would not
fix this: the counterexample that produced the acquirer rule ended with both recoverers refused exactly that way, and
would have passed.

This is accepted, not fixed: the stranded lock is swept up by the next attempt, which spec 240.2 already says can
succeed. The alternative was modelled and measured first (2026-09-13). With classifiers inspecting under a SHARED
OS-native lock and owners holding it exclusively, the plain-rerun case holds - 582,756 distinct states - but a new race
appears: two recoverers can now both classify a dead lock, one passes 240.3 step 1 and stalls while the other completes
the whole recovery, and the stalled one then renames the fresh lock aside by path, so the new owner's publish check
refuses. Safety held throughout (the five safety invariants over the complete state space), yet `RefusalJustified`
failed on an ordinary `check` run; exclusive inspection had been serializing recoverers. Inspecting shared and recovering
exclusive does not rescue it: neither `flock` nor `LockFileEx` upgrades a hold atomically, so the race returns in the
upgrade window, and a recoverer blocked by an inspector must restart in a loop that either livelocks under a steady
stream of inspectors or strands the lock. What decided it is where each design fails. Exclusive inspection fails a
colliding invocation at the door, before it has changed anything; shared inspection fails a rightful owner after it
has recovered and reached publish, and leaves the lock it puts back orphaned.

Two smaller items were found along the way and are deferred, not fixed. The model's refusal at the Section 99 check
returns without closing its own handle, so a refusing owner finishes still holding its OS-native lock, which a real
process gives up on exit; the path is unreachable under exclusive inspection, so it is latent. And spec 240.3 step 1's
"re-read the lock" does not say whether it reads the lock path or the handle already open. The model reads the path,
and with that reading safety is measured to hold; that step 3's post-move identity check would equally keep a handle
re-read safe is argued, not measured.

Build order: the scenarios are built one plan at a time, not all at once, so that the first TLC runs measure real
state-space sizes and their findings are triaged before more actors are built on the same model. Plan 2 builds
`FsModel.tla` and the `recovery` scenario (the Owner, Recoverer, PlainRun, and Cleanup actors). The remaining
LockProtocol scenarios follow in plans grouped once plan 2's measurements exist, and `claims` last. Until a scenario
is built it is listed in `trace.toml`'s `planned_scenarios` (Section 9.1). A later plan changes files an earlier plan
created (the runner, the stamp test, the self-test configuration); those changes are shown in the later plan, and the
earlier plan document stays as it was executed.

Common bounds: one target lock path (plus the parent's for `nested`, and the per-name lock path and the directory
lock path for `dirlock`); at most two crashes per run in total, and one in `claims`; `LockLost` at most once; symmetry
only as Section 6.1 states (`recovery` and `breaklock`, in `check` and `seeded` runs). These bounds come from
order-of-magnitude estimates made during review (`dirlock` and the `claims` group `A`/`a` were the two at risk), not
from measured runs; the first runs measure them. The `recovery` liveness run is measured first of all, because liveness
checking cannot use symmetry (Section 4) and has to build the whole state graph: five actors interleaving filesystem
steps with two crashes each may not fit ten minutes. The `check` runs now say this is near certain: three of those
five actors already take 1.2 to 2.2 million states WITH symmetry available, and all four together did not finish at
all. When the liveness run does not fit, the tightenings, in this order, are one crash instead of two in that run,
then one Recoverer instead of two, then dropping the `PlainRun` from the liveness configuration; each is recorded in
the README with the measurement that forced it.

The ladder has a floor, because each rung removes some of what the property is about. A liveness run must keep at
least one crash, or no lock is ever dead and `DeadLockEventuallyCleared` holds vacuously, and at least one actor
able to clear the lock, or it cannot hold at all; below that the run is not a weaker check of the property but a
check of a different one. Between the floor and the top rung the run still proves the property, and proves it under
less contention than the scenario allows, so the README records not just which rung was taken but what the tightened
run no longer says: a liveness run with one Recoverer says a dead lock is cleared, not that it is cleared while
another recoverer races for it. The `check` runs, which keep the contention, are where that question is answered.

A README note is not enough on its own, because the gate would go on reporting an unqualified pass for a run that
now checks less. A tightened liveness run says so in `expected.toml`, as a `tightened` entry naming the rung taken
and the measurement that forced it, and the runner prints that alongside the run's result. The point is not to fail
the run - a tightened run is a legitimate one - but that nobody should be able to read the gate's output as saying
the property holds under the scenario's full contention when it was not checked there. A fourth tightening is available to the liveness run alone and to no
`check` run: start it from an initial state that already holds a dead owner's lock, which drops the whole prefix in
which the owner acquires and dies. A `check` run may not do that, because those prefix states are where the
scenario's dangerous interleavings are; a liveness run asks only whether a dead lock is eventually cleared, and that
question begins at the state the seed sets up.

Seeding that state takes two things, not one, and `FsWith` is only the first. `FsWith` builds a filesystem holding
the record; what makes a lock DEAD is the ghost `crashed` flag of the process the record names, because a classifier
reads `crashed[seen.op]` and can otherwise judge the lock only live or uncertain. A seeded liveness configuration
must therefore also start `crashed` true for that owner, which in turn makes the owner's own process take one step
to its end label and stop - the prefix the seed exists to remove. A configuration that seeds the filesystem alone
does not model a dead owner at all; it models a live one, and the liveness property it checks is a different
property from the one intended. A run that does not fit its time limit gets tighter bounds, and the tighter bounds
are written into the README, never raised silently.

The figures once quoted here for `recovery` (about twenty-nine minutes for its eight `check` runs, about fourteen for
its liveness run) were single samples taken on a developer machine while other work shared its CPU, so the owner
ruled them unverified, and the model check itself was loading that machine past use. The model's runs therefore
moved to CI, where they were measured on 2026-09-13. The runners are GitHub-hosted `ubuntu-latest` machines whose
hardware is not controlled, so every figure is a range from two samples:
- POSIX `check` runs: 44-45, 63-65, 49 and 43-45 seconds.
- Windows `check` runs: 45-65, 67-92, 50-68 and 45-59 seconds. The two runners differed by up to about 40%.
- The liveness run, `DeadLockEventuallyCleared` over 1,869,534 distinct states: 258-261 seconds.
- A whole `recovery` POSIX job (nine runs): 8m06s-8m10s. A Windows job: 3m49s-5m09s.
The liveness run fits its 30-minute limit several times over, so no tightening rung is needed. The
`timeout_minutes` of 30 therefore stand as generous ceilings, not tight budgets. The owner chose the lever for
jobs: one CI job per scenario and platform (Section 4), with a 180-minute job limit, since paired runs make a
scenario's job long. A job still carries its
scenario's `witness` and seeded runs and, compounding worst, a `<name>-fixed` second run for every `check` run with
an open finding.

Host crashes form their own tier. With `HostCrashes = TRUE` the four POSIX pairings exhaust with no violation, but
their state spaces are 10 to 14 times those without. Measured on CI after the fix to torn empty files in Section
5.2, each with its per-run coverage gate passing on the same `unreached` lists as its process-crash counterpart:
Owner and two Recoverers 13,365,123 distinct states; with a PlainRun 22,816,212; with a Cleanup 12,214,986; PlainRun
and Cleanup 10,718,214 (re-measured 2026-09-14 with the `Foreign` start state of Section 7). The owner
ruled that per-pull-request CI keeps the process-crash runs and a separate tier, `expected-extended.toml` (Section 4),
runs the host-crash pairings: nightly, on request, and on a labelled pull request. What that stops proving on an
ordinary pull request is stated plainly: a change that relies on an unflushed write or entry operation being durable
passes the pull request's checks, and the tier catches it afterwards. Label coverage cannot even show that a host
crash ran, because it shares the label `env_loop` with the process crash, so `expected.toml` keeps one cheap host-crash
run of its own, the witness `NeverHostCrashChangedLock`, which stops once a host crash has changed which object the
lock path names.

The tier also carries the four Windows host-crash pairings, measured on CI with no violation and their coverage gates
passing: 16,993,629 / 28,380,459 / 16,112,895 / 13,766,550 distinct states (re-measured 2026-09-14). The Windows job took 49m56s against
POSIX's 40m00s, and the two jobs run in parallel. The POSIX host-crash runs, timed twice, took 523-558, 847-907,
462-481 and 414-448 seconds. The owner kept the Windows pairings against the peer's recommendation to drop them. The
peer's argument: a host crash releases every handle, and objects it leaves without a name cannot be opened, so the
directory tree after the crash is one the Windows process-crash runs already explore. The reason for keeping them:
one host crash stops every live process and uses a single unit of `MaxCrashes`. "A host crash, then a further crash
of a later invocation" therefore fits at `MaxCrashes = 2` only in this tier, and that further process crash releases
handles, which is where Windows sharing rules apply. No counterexample is known either way.

## 13. Success criteria

A spec defect the model finds is carried as an `open_finding` (Sections 4 and 11) until the spec is fixed; criteria 1
to 7 hold with open findings counted as expected results, and the work is finished when no open finding remains.

1. Every seeded run stops with its named violation.
2. Every `check` run reports exactly its listed witnesses and its open findings violated, and no other safety
   invariant, and no fix-flag run reports any. Every `witness` run stops with its own invariant violated. In every
   `check` and `liveness` run, every label of every actor the run instantiates is covered except the labels it lists as
   `unreached`, and each of those is indeed uncovered; a full `just model` reports every label of every model covered by
   at least one run, apart from any in `never_reached`, and no `never_reached` label is named by a `trace.toml` unit
   (Section 4). When the work is finished `deferred` is empty, because every scenario it could name has been built.
3. Every `liveness` run passes.
4. Every run finishes within its time limit on CI.
5. The traceability check (Section 9.1) passes: every label maps to a spec step, and every spec step of the stamped
   headings maps to a label or to a `not_modelled` reason. When the last scenario is built, `planned_scenarios` is
   empty and no `pending` entry remains. Dropping a scenario from Section 12 removes its name from
   `planned_scenarios` and from every `pending` array in the same change, and every entry that named it gains a
   `not_modelled` reason saying what that scenario would have modelled, keeping whatever labels it already has; an
   entry must never become silently complete while part of its unit has no model.
6. The drift-stamp test, the traceability check, and FS-1 to FS-11 pass in the existing test job.
7. `model-gate` passes on the pull request that adds the model.

## 14. Out of scope

- Concurrency testing of the Rust implementation (loom, shuttle, or madsim), once lock code exists; recorded in
  `TODO.md`.
- Filesystems beyond the modelled variants (SMB, NFS), and clock behaviour beyond the liveness oracle: listed in the
  README as unverified assumptions.
- The content of refusal reports (96.2) and exit codes: the model checks which outcome happens, not its wording.
- Configuring branch protection: the design recommends requiring `model-gate` (Section 4); the owner sets it.

## Stand-downs

Findings the 2026-09-12 panel stood down below its severity floor, with the guard that makes each one moot:

- A genuinely new rule-bearing heading can be listed as `not_modelled` with a vague reason rather than modelled.
  Section 4 already states that no mechanism can force a person to model a new rule rather than re-stamp it; the
  reason and the hashes appear in the pull request's diff.
- Ordinary prose beginning with `#` inside a stamped section is read as a heading and can fail the family check.
  That is what the markdown the spec is written in means (Section 9 counts ATX headings outside fenced blocks), so
  the check agrees with every other reader of the file.
- The coverage parser could mistake a wrapped cost line for an action line, or break if TLC changed the report's
  indentation. Every cost line is indented and every action line starts at column 1 (measured on the pinned release
  across four runs), and the report's format is pinned by the same SHA-256 as the jar, with `record_fixtures.py`
  re-recording `testdata/` when that pin moves - so a format change cannot arrive unnoticed between pins.
- Three `recovery` pairings set `MaxObjs` to 4, 5 and 6 although each runs three actors, so the bound looks inflated.
  Section 4 claims only that `MaxObjs` is at least the actor count, never that it equals it, and a bound that does not
  bind leaves the state space unchanged: raising each of these by one gave identical state counts, so tightening them
  would cost a re-measurement for no difference in what is checked.
