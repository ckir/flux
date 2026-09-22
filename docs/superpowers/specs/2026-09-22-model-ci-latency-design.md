# Cutting the model suite's time-to-verdict

**Status:** design approved 2026-09-22. Negotiated with the agy peer question by question; every
option presented to the owner carried both endorsements.

## The problem, measured

A pull request that touches the model or the spec waits about **98 minutes** for the `Model`
workflow. Measured on run `35712320171`, a completed success:

| CI job | wall clock |
|---|---|
| **Scenario breaklock (posix)** | **98.4 min** — the critical path |
| Scenario breaklock-remote (posix) | 83.6 min |
| Scenario mixed-remote (posix) | 29.1 min |
| Scenario mixed (posix) | 26.9 min |
| Scenario breaklock-remote (posix-plain) | 24.9 min |
| Scenario recovery (posix) | 23.2 min |
| Scenario breaklock (windows) | 14.3 min |
| Scenario recovery (windows) | 11.8 min |
| Scenario mixed (windows) | 11.2 min |
| Plan, selftest, union, translation, gate | under 1 min each |

Total compute is 324.6 minutes; wall clock is 98.4 because the jobs run in parallel. **The suite is
not slow because it is big.** Two jobs dominate and the other eleven finish inside half an hour.

Inside those two jobs, single runs dominate again:

```
breaklock (posix), 98.4 min:
  breaklock-posix-liveness       states=17,377,074   3566s = 59.4 min   <- 60% of the job
  breaklock-posix-check          states= 8,696,631    514s =  8.6 min
  breaklock-posix-plain-check    states= 6,914,751    375s =  6.3 min
  (about twenty seeded and witness runs, each fast)

breaklock-remote (posix), 83.6 min:
  breaklock-remote-posix-check         2722s = 45.4 min
  breaklock-remote-posix-fixed-check   1862s = 31.0 min
  (six witnesses, together about 7 min)
```

So the wait is set by **one liveness run and one safety check**, not by the matrix width.

## Goal and non-goals

**Goal:** cut the critical path of the `Model` workflow from ~98 minutes to ~52, without weakening
any safety invariant checked on a pull request.

**Non-goals, stated so they are not smuggled in later:**

- Not running TLC on a developer machine. TLC and heavy Java model checks run in CI only. That is a
  standing constraint, not a tradeoff to revisit here.
- Not weakening the constants of any run that stays on the PR.
- Not changing `model.yml`'s trigger topology or its changed-path gate. A pure-Rust change already
  yields `matrix=[]` and skips the suite entirely; that works and is left alone.
- Not adding a shallow per-PR liveness run. That is real work with its own coverage review and gets
  its own spec — see "Deferred" below.

## The mechanism this rests on

Splitting a scenario across CI jobs is **configuration, not code**. `models/lockproto/run.py`
assigns each run a CI job id:

```python
job = raw.get("job", platform)      # run.py, in the run-parsing path
```

and a comment in the same file notes that a run "may name a longer id to split a scenario that does
not fit one". The precedent already exists: the four `breaklock-remote ... posix-plain` runs each
carry an explicit `job` key, which is why `Scenario breaklock-remote (posix-plain)` is its own CI
job. Every other run defaults to its platform.

Moving a run between tiers is equally configuration: `model.yml` lists jobs from `expected.toml`,
while `model-extended.yml` passes `--expected models/lockproto/expected-extended.toml`.

## Change A — the deep liveness run moves to the nightly tier

Move `breaklock-posix-liveness` (`Breakers: ['b1','b2']`, `MaxObjs: 3`, 59.4 min) from
`expected.toml` to `expected-extended.toml`.

**Effect:** `Scenario breaklock (posix)` drops from 98.4 to about 39 minutes.

**This costs the coverage union nothing, and that is measured rather than assumed.** The suite-wide
union counts a label as covered if ANY non-fixed run of the module reports it with TOTAL > 0. Using
`run.py`'s own `parse_coverage` on the uploaded TLC logs from run `35712320171`:

```
breaklock-posix-liveness covers                         70 labels
all other runs in that same CI job cover                84 labels
labels covered ONLY by the liveness run                  0
```

Every label it covers is covered by something else, so no `unreached` or `deferred` list needs
recomputing for this move. Had the answer been anything but zero, this change would not be
config-only.

**What IS lost:** liveness *property* verification on the pull request for the `breaklock` scenario.
Label coverage and property verification are different things, and the measurement above speaks only
to the first. A deadlock or starvation regression in that scenario would be caught by the nightly
tier rather than by the PR. Safety invariants — the `SingleWriter` class, which is what prevents
data corruption — are untouched and still run on every PR.

**Only this one run moves.** `mixed-posix-liveness` (18.4 min), `recovery-posix-liveness` and
`selftest-posix-liveness` all stay. Moving `mixed-posix-liveness` would take its job from 26.9 to
8.5 minutes and change the critical path by nothing, so it would be discarding per-PR liveness
verification to buy zero. Three of the four liveness runs remain on every pull request.

## Change B — `breaklock-remote (posix)` splits into two jobs

Give the `fixed-*` runs an explicit `job = "posix-fixed"` key, leaving the rest as `posix`:

| new job | runs | about |
|---|---|---|
| `posix` | `check`, `window-witness-SingleWriter`, `witness-NeverTornRead`, `witness-NeverUncertainLockWithPendingBreaker`, `witness-NeverInflightLandedAfterTakeover`, `hostcrash-witness-NeverHostCrashChangedLock` | 52 min |
| `posix-fixed` | `fixed-check`, `fixed-witness-NoLiveWriter` | 31 min |

The split is by the same axis the `posix-plain` precedent uses — a scenario variant, keeping a check
together with the witnesses that establish its reachability.

### Two shards rather than three, priced deliberately

A three-way split — `check` alone, `fixed-*`, witnesses — would reach 45.4 minutes instead of 52.
That was considered and rejected.

The first objection raised against it was per-job setup overhead, and **that objection is false**:
`Scenario mixed (posix)` ran 26.9 minutes against runs totalling 26.6, and the entire
`Scenario selftest (posix)` job including checkout, Java, Python and TLC finished in 0.3 minutes.
Setup costs about 18 seconds.

The real trade is 7 minutes against debuggability: splitting the witnesses away from their check
means a failing check and the witnesses proving its reachability land in two different CI logs, and
that cost is paid exactly when developer friction is highest. Both 45 and 52 minutes fall in the
same "go and do something else" bucket. **The 7 minutes is a priced decision, not an oversight.**

## Change D — timeout headroom

Declared timeouts against measured runtime, same run:

| used | actual | declared | run |
|---|---|---|---|
| **66%** | 59.4m | 90m | `breaklock-posix-liveness` |
| 38% | 45.4m | 120m | `breaklock-remote-posix-check` |
| 26% | 31.0m | 120m | `breaklock-remote-posix-fixed-check` |
| 20% | 18.4m | 90m | `mixed-posix-liveness` |
| 14% or less | | | everything else measured |

One run is an outlier. `breaklock-posix-liveness` sits at 66% of its declared timeout, with the next
tightest at 38% and a clear gap between them.

**Rule this spec adopts: a run should not consume more than 50% of its declared timeout.** That is
not an arbitrary number — it is drawn from the distribution above, where every run except one
already satisfies it. Raise `breaklock-posix-liveness` to `timeout_minutes = 120`, which puts it at
50%. The tier it is moving to has ample room for that: `model-extended.yml` sets
`timeout-minutes: 360` on its job, where `model.yml` sets 180 on the scenario job the run is
leaving.

State-space growth is not linear in the constants, so a run at 66% is closer to its cliff than the
number suggests. The point of the rule is that a timeout should fail on a genuine hang, not on
ordinary growth.

## What this achieves, and the limit

| | before | after |
|---|---|---|
| `Scenario breaklock (posix)` | 98.4 min | ~39 min |
| `Scenario breaklock-remote (posix)` | 83.6 min | ~52 min |
| new `Scenario breaklock-remote (posix-fixed)` | — | ~31 min |
| **critical path** | **98.4 min** | **~52 min** |

**The floor is 45.4 minutes and no further sharding reaches past it.**
`breaklock-remote-posix-check` is a single indivisible TLC invocation; a `job` key subdivides a list
of runs, never one run's state-space search.

Getting below that floor needs **vertical** scaling rather than horizontal: TLC's search is
concurrent and scales roughly with cores, so larger runners would cut the floor itself. That is out
of scope here — it costs money rather than configuration — but it is the named next lever, recorded
so the limit does not get rediscovered.

## Verification

TLC cannot run on the developer machine, so the change is verified in three layers:

**`--list-jobs` is NOT sufficient on its own, and a panel round caught the draft assuming it was.**
It emits one entry per CI job as `{scenario, platform, job}` — ten entries, and **no run names**.
Two consequences the implementer must know:

- The `breaklock (posix)` job does NOT disappear when `breaklock-posix-liveness` moves out of it;
  other breaklock runs remain. "The job is gone" is an unsatisfiable check, and a correct
  implementation would appear to fail it.
- A run left in the wrong job is INVISIBLE to it. Add `job = "posix-fixed"` to the witnesses but
  forget `fixed-check`, and the matrix still shows a `posix-fixed` entry while the 31-minute run
  stays in `posix` — the check passes and the critical path silently stays near 76 minutes.

So verification is run-level, not job-level:

1. **Run-to-job assignment, the real oracle for B.** Locally, no TLC:

   A CI job is identified by **(scenario, job)**, not by `job` alone — grouping on `job` alone
   collapses every posix scenario into one bucket and tells you nothing. This grouping reproduces
   the ten entries `--list-jobs` emits, which is how you know it models the right thing:

   Needs **Python 3.11 or newer** locally, for `tomllib`. The repository targets 3.14 and the
   workflows pin `python-version: "3.14"`, so this matches CI; it is stated because the command is
   run on a developer machine, where the default interpreter may be older.

   ```bash
   python - <<'EOF'
   import tomllib, pathlib, collections
   d = tomllib.loads(pathlib.Path("models/lockproto/expected.toml").read_text(encoding="utf-8"))
   by = collections.defaultdict(list)
   for r in d["run"]:
       plat = "windows" if "-windows" in r["name"] else "posix"
       by[(r["scenario"], r.get("job", plat))].append(r["name"])
   for k in sorted(by):
       print(f"  {k[0]:<18} {k[1]:<12} {len(by[k]):>3} runs")
   print(f"
{len(by)} CI jobs")
   EOF
   ```

   Run it **before** the change and keep the output; judge the change as a DELTA against that,
   not against the absolute numbers below. At the time of writing it printed 10 CI jobs with
   `breaklock posix` at 16 runs and `breaklock-remote posix` at 8, but `main` moves and another
   merged change can shift those totals, which would make a correct implementation look wrong.

   The delta that must hold: **exactly one more CI job than before**, and:

   | scenario | job | runs |
   |---|---|---|
   | `breaklock` | `posix` | **15** (16 minus the moved liveness run) |
   | `breaklock-remote` | `posix` | **6** (8 minus the two `fixed-*` runs) |
   | `breaklock-remote` | `posix-fixed` | **2**, exactly `breaklock-remote-posix-fixed-check` and `breaklock-remote-posix-fixed-witness-NoLiveWriter` |

   Every other row unchanged. This is the check that catches a run left in the wrong job.

2. **The move, for A.** `grep -c breaklock-posix-liveness models/lockproto/expected.toml` must be
   `0`, and the same grep against `models/lockproto/expected-extended.toml` must be `2`. Two, not
   one: the name appears on the run's own `name` line and again in its `config` path
   (`configs/breaklock-posix-liveness.cfg`). Both lines live inside the same `[[run]]` block and
   move together, so the pair is the signature of a complete move. The `.cfg` file itself stays
   where it is in `models/lockproto/configs/` — only the tier that references it changes.

3. **`run.py --list-jobs` and the extended-tier equivalent** still run, but only to confirm the
   matrix parses and gains the `posix-fixed` entry. They are a syntax check, not the oracle.
3. **`models/lockproto/test_run.py` and `test_workflow.py`**, plus the repository gate `just check`.
4. **CI is the only oracle for the timings**, and the moved run is verified BEFORE the merge, not
   after. `model-extended.yml:31` gates on
   `github.event_name != 'pull_request' || contains(github.event.pull_request.labels.*.name, 'model-extended')`,
   so applying the **`model-extended`** label to the implementation PR runs the extended tier on
   that PR. Do that: it proves `breaklock-posix-liveness` passes in its new home while the change is
   still revertible. Waiting for the cron would verify it only after it had already merged, and an
   earlier draft of this spec said exactly that — it was wrong.

The coverage union is the risk to watch: if the union fails on the first run after A, the measured
"zero unique labels" result was wrong and the change must be reverted rather than papered over by
widening a coverage exemption. The per-run key for such an exemption is `unreached` (with a
separate `deferred`); `never_reached` is the parameter name inside `run.py`'s `judge_union` and
appears nowhere in `expected.toml`, so do not go looking for it there.

## Interactions checked, because a new CI job touches more than its own runs

Each of these was a gap found by auditing the draft, and each is resolved in favour of the change.
They are recorded so the implementer does not have to rediscover them.

- **The new shard's artifact name fits the union's glob.** `model.yml` uploads
  `tlc-output-${{ matrix.job.scenario }}-${{ matrix.job.job }}` and the union job downloads
  `pattern: tlc-output-*`. A `posix-fixed` job produces `tlc-output-breaklock-remote-posix-fixed`,
  which the glob matches. No change is needed to either step.
- **The gate job's dependency graph does not change.** `model-gate` declares
  `needs: [plan, scenario, translation, union]`. A shard is another entry in the `scenario` matrix,
  not a new job name, so the graph is untouched.
- **The nightly tier does not judge the coverage union**, so moving a run into it creates no new
  coverage obligation there. `model-extended.yml` records why: the union "is NOT judged here any
  more", because the job that did it by re-running `expected.toml` whole hit its cap and was killed
  every time. `model.yml` computes the union from the scenario artifacts instead.
- **The moved run simply stops contributing to the PR union**, which is precisely the case the
  zero-unique-labels measurement covers. Nothing else has to compensate for its absence.

## Risks and things this spec deliberately leaves open

- **Per-PR liveness coverage for `breaklock` is lost until the follow-up lands.** Accepted, with the
  qualification below.
- **A liveness regression can reach a release before the nightly catches it.** Measured:
  `release-plz.yml` fires on every push to `main`, and neither it nor `release.yml` references the
  model tier at all — zero matches for "model" in both files. So the common argument for deferring
  per-PR liveness ("the nightly catches it within 24 hours, and releases are gated on the nightly")
  does **not** hold in this repository today. This gap pre-dates this spec and is not made worse by
  it, but it is recorded here because it was found while checking that argument, and because it is
  the strongest reason to land the deferred follow-up sooner rather than later. The decision about
  whether to gate `release-plz` is deliberately not taken here.
- **The 45.4-minute floor**, as above.

## The gap this change widens: nobody is told when the nightly tier fails

A pull-request check is synchronous and addressed to the person who caused it — it blocks their
merge and they see it immediately. A cron job is neither. Moving `breaklock-posix-liveness` into the
nightly tier therefore changes WHO finds out when it breaks, and that is a property of the change,
not a pre-existing condition to wave at.

**Measured: `model-extended.yml` contains no notification of any kind.** Grepping it for
`notify`, `slack`, `webhook`, `issue`, `mail` and `alert` returns nothing. A failing nightly run
raises GitHub's default notification and nothing else, which does not reach the author of the commit
that broke it — they may not even be watching the repository.

Compounding it: `release-plz` opens a release pull request on every push to `main`, and no release
workflow consults the model tier. So the sequence "deadlock merges, nightly goes red at 03:17,
nobody is told, a release ships" is available today.

This spec does NOT fix that, because inventing a notification mechanism here would widen it well
past CI latency and the choice of mechanism belongs to the owner. What it does is state the gap in
the terms the decision needs:

- The consequence of a red nightly tier is currently borne by whoever happens to look.
- After this change, one more class of defect — `breaklock` liveness — lands in that tier.
- Until either a routing mechanism exists or the deferred shallow per-PR run lands, the window
  between "a deadlock merges" and "a human notices" is unbounded.

Owner decision, recorded as a follow-up rather than taken here.

## Stand-downs

Findings the panel raised and did not fold, with the reason.

- `REJECTED`: "the moved run will default to `job = 'posix'` and bundle with existing breaklock
  runs in the extended tier, serialising a 59-minute run behind them." Measured — the extended tier
  contains exactly two CI jobs today, `recovery/posix` (4 runs) and `recovery/windows` (4 runs).
  There is no `breaklock/posix` job there, so the moved run forms its own job and has nothing to
  queue behind. No explicit `job` key is needed. If a future change adds breaklock runs to that
  tier, this becomes live again.

## Deferred to its own spec

A **shallow per-PR liveness run** for `breaklock`, with `Breakers` reduced from `['b1','b2']` to
`['b1']`, restoring baseline deadlock verification on every pull request while the deep two-breaker
run stays nightly. The evidence that this is cheap: `mixed-posix-liveness` carries a LARGER
`MaxObjs` of 4 yet finishes in 18.4 minutes with a single breaker, so the breaker count — not the
object count — is the dominant term in the state-space explosion.

It is deferred because it is not a configuration change. A new run needs its own `.cfg`, its own
`expected.toml` entry, and its own `unreached` list — and `run.py`'s `judge_coverage` judges each
run individually even though the suite-wide union does not, so fewer breakers means more unreachable
labels, each of which needs a written justification. That is a coverage review, and mixing it with a
CI topology change would stall the latency win behind it.
