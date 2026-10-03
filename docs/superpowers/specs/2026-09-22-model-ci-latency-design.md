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

> **Correction, added after implementation (2026-09-22).** Every figure in this section is a
> **single sample from one run**, and that turned out to matter. Measured afterwards, the same
> `breaklock-remote-posix-check` invocation — identical code, identical config — took 2722s here and
> 3806s / 3829s on two later runs, a **40% spread with nothing changed**. The untouched
> `Scenario mixed (posix)` job ranged 26.9 → 31.1 → 49.0 min across the three. GitHub's hosted
> runners are shared and their speed is not controlled by this repository, so a single sample cannot
> support a target. The shape of the problem below is correct and still holds — two jobs dominate,
> and inside them single runs dominate. The absolute minute figures, and every prediction derived
> from them, are not reliable. See **What this achieves** for the corrected, within-run result.

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

**Goal:** cut the critical path of the `Model` workflow roughly in half, without weakening
any safety invariant checked on a pull request — and make the nightly tier's verdict visible, since
this change is what moves a class of defect into it. Changes A, B and D serve the first half;
change E serves the second, and the two are one piece of work because the first creates the need for
the second.

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

The two "about" figures were projected from the baseline run and **both came in higher** — 73.0 /
73.3 and 40.9 / 44.2 measured. The split itself is unaffected: what the `job` key controls is which
runs share a job, and that landed exactly as described.

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

The *price* was computed from the baseline run and is therefore wrong in magnitude — on the runners
actually measured, the third shard would have saved closer to 10 minutes than 7. The *decision* is
unchanged, and arguably strengthened: the runner variance alone (18 minutes on an untouched job) is
larger than the gap being traded away, so buying it back at the cost of splitting a check from its
witnesses would be paying a permanent debuggability cost for a difference the runner noise swamps.

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

> **Correction: the rule is right, the table was one sample, and a second run now breaks the rule.**
> Re-measured on the two post-change runs, with declared timeouts unchanged:
>
> | used | actual | declared | run |
> |---|---|---|---|
> | **53%** | 63.4m / 63.8m | 120m | `breaklock-remote-posix-check` — **was listed at 38%** |
> | 37% | 43.9m | 120m | `breaklock-remote-posix-fixed-check` — was listed at 26% |
>
> `breaklock-remote-posix-check` did not change; the runner did. It is now the run sitting closest
> to its timeout, in the same position `breaklock-posix-liveness` occupied when this rule was
> written — and it is the critical path, so a timeout there fails the whole PR verdict.
>
> **This is a headroom gap the spec has not closed.** Applying this spec's own rule, that run needs
> `timeout_minutes` of at least 128, and by the precedent set above (90 → 120 for a run at 49.5%)
> something like 150 would be the consistent choice. That is a change beyond the approved A/B/D/E
> scope and is **recorded here, not made**, for the owner to scope.
>
> The deeper lesson the rule should carry: a percentage-of-timeout figure is only as stable as the
> runner it was measured on. A 40% runner swing moved a run from 38% to 53% with no code change, so
> the 50% rule needs headroom for runner variance built in, not just for state-space growth.

State-space growth is not linear in the constants, so a run at 66% is closer to its cliff than the
number suggests. The point of the rule is that a timeout should fail on a genuine hang, not on
ordinary growth.

## Change E — the nightly tier says when it fails

> **The alarm must also clear.** As first written, change E only opened or commented on an issue; it
> had no recovery path, so the issue stayed open after the tier went green. The capstone on the
> shipped code caught this, and it is a defect in the salience argument below rather than a missing
> nicety: an issue that never auto-closes stops meaning "`main` is broken now" and starts meaning
> "`main` broke at some point", which trains the reader to ignore the only signal this tier has.
>
> A symmetric `resolve` job closes it on recovery. Two of its guards are load-bearing and were not
> obvious: `github.event_name != 'pull_request'` (without it, a passing PR carrying the
> `model-extended` label would CLOSE a genuine open issue about a broken `main` — a false all-clear
> from an unrelated branch), and `needs.tier.result == 'success'` rather than `success()` alone
> (`success()` is also true when `tier` was SKIPPED on an empty matrix, which would close the issue
> having verified nothing).

Moving a check from a pull request to a cron changes who finds out when it breaks. A PR check is
synchronous and addressed to the person who caused it; a cron job is neither. Since this spec is
what puts `breaklock` liveness into that tier, the mechanism that makes the tier's verdict visible
belongs to the same change rather than to a someday list.

**Measured: `model-extended.yml` contains no notification of any kind** — grepping every workflow in
`.github/workflows/` for `slack`, `discord`, `webhook`, `notify`, `actions/github-script`,
`create-issue` and `peter-evans` returns nothing, and no workflow holds `issues: write`.

Add a final job to `model-extended.yml` that opens a GitHub issue when the tier fails:

- **A separate job**, with `needs: [plan, tier]` and `if: failure()`. NOT a step inside `tier`:
  `tier` is a matrix job, so a failure step within it fires once per failing entry and would open
  several issues for one bad night.
- **`issues: write`** added to the workflow's `permissions`, which is currently `contents: read`.
- **`gh issue create`**, following the pattern already used in `dependabot-automerge.yml` and
  `update-auto-merge-prs.yml`; the CLI is present on GitHub-hosted runners and this repository
  already relies on that.
- **Deduplicated.** A persistent failure would otherwise open a fresh issue every night until it is
  fixed. Search for an open issue with the agreed title first and comment on it instead of creating
  another. The dedup key is the title, not the run id, because the point is one issue per ongoing
  breakage rather than one per occurrence.
- The issue body carries the run URL, the failing job, and the commit, so it is actionable without
  going to find them.

### Why this, and why nothing for the release path

The other half of the same problem is that a release can ship without the model tier ever having
run: `release-plz.yml` opens a release pull request on every push to `main`, `release.yml` builds on
a tag push, and `grep -c model` in both returns `0`.

**That is deliberately NOT gated, and change E is the reason it does not need to be.**

Hard-gating a synchronous release pipeline on an asynchronous cron is a tarpit: either `release.yml`
runs the suite itself, which inflicts a multi-hour wait on someone who only wants to ship, or a
script polls the API for the last cron's verdict and becomes a new thing that can be wrong. Neither
is justified here.

What closes the risk instead is salience. Merging a release pull request means visiting the
repository, and an open "nightly model tier failed" issue sits beside the Pull requests tab while it
is unresolved. The human at the merge button becomes the gate, holding the context at the moment the
decision is made. That is enough **because this is a one-person repository** — `git log` lists a
single human author and `dependabot[bot]`, so the repository owner, the workflow's editor and the
author of any commit that introduces a deadlock are the same person. The risk was never that the
signal reaches the wrong human; it was that an automated email is easy to skim past. An issue is not.

If this repository ever gains a second regular contributor, that reasoning expires and the release
path needs revisiting.

### Verifying E

`if: failure()` cannot be demonstrated by a passing run. Verify it on a branch, before merge:

1. Push a branch with a deliberately failing extended-tier run — for example a `timeout_minutes` of
   `1` on one extended-tier run — and apply the `model-extended` label to its pull request, which is
   what `model-extended.yml:31` gates on.
2. Confirm exactly ONE issue is opened, not one per matrix entry.
3. Re-run the same workflow and confirm the second failure comments on the existing issue rather
   than opening a second one.
4. Revert the deliberate failure before merging.

Step 2 is the one that matters: it is the check that distinguishes a correct separate-job
implementation from a failure step placed inside the matrix job.

> **This procedure no longer works, and the reason is the point.** It was executed as written, by
> dispatching the workflow on the branch, and it did open exactly one issue (#33) — which is how the
> capstone later found that `notify` had **no branch constraint at all**. Run `35743039548`, on
> branch `spec/model-ci-latency`, opened a repository issue announcing that the *nightly* tier had
> failed, when what had failed was a deliberately broken feature branch. `workflow_dispatch` accepts
> any ref, so the verification procedure and the defect were the same action.
>
> Both `notify` and `resolve` now require `github.ref == 'refs/heads/main'`. The consequence is that
> **this procedure cannot be run from a branch any more** — verifying either job means dispatching
> on `main` with a deliberate failure, which is a far heavier thing to do and should be weighed
> against simply trusting the two jobs' conditions. The verification that was performed remains
> valid evidence for the *one issue, not one per matrix entry* property, which is what step 2 exists
> to establish and which the branch guard does not affect.

## What this achieves, and the limit

**This section was rewritten after the change shipped, and the original prediction was wrong.** It
claimed a ~52-minute critical path. The measured figure is **73.0 and 73.3 minutes** across two
runs. The shortfall is not a failed split — it is the single-sample baseline described in the
correction above. Both numbers below are measured, not predicted.

### Measured, after (two samples)

| CI job | run `35744606862` | run `35744782885` |
|---|---|---|
| `Scenario breaklock-remote (posix)` — **the critical path** | **73.0 min** | **73.3 min** |
| `Scenario breaklock-remote (posix-fixed)` (new) | 40.9 min | 44.2 min |
| `Scenario breaklock (posix)` | 40.1 min | 37.6 min |

Both runs completed green, **including `Suite-wide coverage union`** — the job that would catch
change A silently dropping label coverage, and therefore the result that matters most.

### The win, measured within a single run

A before-and-after across runs cannot be trusted here, because the runners differ between them. So
the effect is stated **within each run**, where the runner is held constant by construction:

| | sample 1 | sample 2 |
|---|---|---|
| `breaklock-remote`, had it stayed one job (`posix` + `posix-fixed`, sequential) | ~113.9 min | ~117.5 min |
| `breaklock (posix)`, had the liveness run stayed in it | **≥ 99.5 min** | **≥ 97.0 min** |
| critical path without changes A and B | **≥ ~114 min** | **≥ ~118 min** |
| critical path as shipped | **73.0 min** | **73.3 min** |

That is a reduction of **at least 36%**, established without comparing across runners.

The `breaklock (posix)` row is a **lower bound**, and deliberately so: `breaklock-posix-liveness`
now runs in the nightly tier, so its duration on *these* runners was never measured. The bound uses
its 59.4-minute figure from the faster baseline runner. The true value is very likely higher — the
one run measured on both runner populations got 40% slower — so the bound holds with room to spare.
It is quoted as a bound rather than a scaled estimate because a scaled estimate would be exactly the
kind of derived-from-one-sample number this spec already got wrong once.

### The limit

**No amount of further sharding gets past the cost of one TLC invocation.**
`breaklock-remote-posix-check` is a single indivisible search; a `job` key subdivides a list of
runs, never one run's state space. That run measured 45.4 min on the baseline runner and 63.4 /
63.8 min on these two — so the floor is **a property of the runner, not a fixed number of minutes**,
and quoting it as one (as this spec originally did) is the same mistake in miniature.

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

- **Per-PR liveness coverage for `breaklock` is lost until the deferred shallow run lands.**
  Accepted. Change E is what makes it tolerable: the deep run still executes nightly, and now says
  so when it fails.
- **A liveness regression can reach a release without the model tier having run.** Measured:
  `release-plz.yml` fires on every push to `main`, and neither it nor `release.yml` references the
  model tier at all — zero matches for "model" in both files. The decision HAS been taken, and it is
  to leave that path ungated: see change E, which closes the risk through salience rather than
  through a gate, and gives the reasoning. The residual risk is that a person merges a release pull
  request while an open nightly-failure issue is sitting beside it. That is a human judgement the
  spec deliberately leaves to a human, and it expires as a defensible position if this repository
  ever gains a second regular contributor.
- **The single-TLC-invocation floor**, as above. Originally written here as a fixed 45.4 minutes;
  corrected, because that run measured 63.4 / 63.8 min on later runners. The floor is real, but it
  is set by the runner, not by a number this spec can quote.

## Background: the gap change E closes

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

**Change E closes this**, and the release path is deliberately left ungated for the reasons given
there. An earlier draft of this spec left both open as owner decisions; they were negotiated and
settled, and the resolution is change E rather than a follow-up.

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
