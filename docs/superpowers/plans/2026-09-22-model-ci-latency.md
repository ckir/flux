# Model-CI Latency Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Cut the `Model` workflow's critical path from ~98 minutes to ~52, and make the nightly tier say when it fails.

**Architecture:** Four changes, all configuration. One TLA+ run moves from the per-PR tier to the nightly tier; one CI job splits in two on an existing `job` key; one timeout gains headroom; and the nightly workflow gains a job that opens a deduplicated GitHub issue on failure. No Rust, no Python, no TLA+ is modified.

**Tech Stack:** TOML config read by `models/lockproto/run.py`, GitHub Actions YAML, the `gh` CLI (already used by two other workflows in this repo).

---

## Spec

`docs/superpowers/specs/2026-09-22-model-ci-latency-design.md` (commit `bbb42ba73f195cdbbb4469795dbce6ae83957c75`).

Read it first. The load-bearing measurement is that `breaklock-posix-liveness` covers **70 labels and
zero of them uniquely**, which is why moving it costs the coverage union nothing.

## Verified facts this plan rests on

Checked against the worktree at `6472a57` while writing. Re-check if `main` has moved.

| fact | how it was checked |
|---|---|
| the `breaklock-posix-liveness` block is `expected.toml:617-633` | `grep -n`, then `sed -n '630,638p'` |
| its name appears on exactly 2 lines, `name` (618) and `config` (620) | `grep -n breaklock-posix-liveness` |
| `expected-extended.toml:12` declares `scenarios = ["recovery"]` | `head -14` |
| `run.py` REJECTS a run whose scenario is not in that list | `_load_run`: `_require(scenario in scenarios, ...)` |
| `job` is written immediately after `scenario` | the `posix-plain` precedent, `expected.toml:816-817` |
| `breaklock-remote-posix-fixed-check` is at `expected.toml:787`, its `scenario` at 790 | `sed -n '786,792p'` |
| `breaklock-remote-posix-fixed-witness-NoLiveWriter` is at 803, its `scenario` at 806 | `sed -n '802,808p'` |
| `model-extended.yml` permissions are `contents: read` at lines 19-20 | `sed -n '15,25p'` |
| its jobs are `plan` (27) and `tier` (49); the file is 82 lines | `grep -n '^  [a-z-]*:$'`, `wc -l` |
| `gh` is already used on runners here | `dependabot-automerge.yml:37`, `update-auto-merge-prs.yml:45` |
| the repo gate is `just check` = `fmt-check clippy typos test` | `justfile:41` |

**Two deviations from the spec, made deliberately and flagged here rather than applied silently.**
Both are in Task 5 and are explained where they occur:

1. `issues: write` goes on the **notify job**, not the workflow. Least privilege, and
   `docs.yml` already establishes job-level elevation in this repo.
2. Verification of change E uses **`workflow_dispatch`**, not the `model-extended` label. The label
   fires a `pull_request` event, and the notify job deliberately excludes pull requests — a failing
   labelled PR is already visible on the PR and does not need an issue. Dispatch exercises the real
   (non-PR) path.

## File structure

| file | what changes |
|---|---|
| `models/lockproto/expected.toml` | one `[[run]]` block removed; `job` added to two runs |
| `models/lockproto/expected-extended.toml` | that block added, `scenarios` widened, header comment corrected |
| `.github/workflows/model-extended.yml` | one job appended |

`models/lockproto/configs/breaklock-posix-liveness.cfg` does **not** move. Both tier files reference
configs by the same relative path.

---

## Task 1: Capture the baseline

The change is judged as a delta. Without this, the later checks have nothing to compare against.

**Files:** none — this task only reads.

- [ ] **Step 1: Record the run-to-job map BEFORE any edit**

Run this from the repository root and SAVE the output where you can see it later:

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
print(f"\n{len(by)} CI jobs")
EOF
```

Needs Python 3.11 or newer, for `tomllib`. The repository targets 3.14 and the workflows pin
`python-version: "3.14"`.

Expected at the time of writing — if your numbers differ because `main` moved, use YOUR numbers as
the baseline and apply the same deltas:

```
  breaklock          posix         16 runs
  breaklock          windows        4 runs
  breaklock-remote   posix          8 runs
  breaklock-remote   posix-plain    4 runs
  mixed              posix          6 runs
  mixed              windows        3 runs
  mixed-remote       posix          7 runs
  recovery           posix         17 runs
  recovery           windows        8 runs
  selftest           posix          4 runs

10 CI jobs
```

- [ ] **Step 2: Record the extended tier's baseline too**

```bash
python - <<'EOF'
import tomllib, pathlib, collections
d = tomllib.loads(pathlib.Path("models/lockproto/expected-extended.toml").read_text(encoding="utf-8"))
by = collections.defaultdict(list)
for r in d.get("run", []):
    plat = "windows" if "-windows" in r["name"] else "posix"
    by[(r["scenario"], r.get("job", plat))].append(r["name"])
for k in sorted(by):
    print(f"  {k[0]:<18} {k[1]:<12} {len(by[k]):>3} runs")
print("scenarios =", d["scenarios"])
EOF
```

Expected:

```
  recovery           posix          4 runs
  recovery           windows        4 runs
scenarios = ['recovery']
```

Nothing to commit. Move on.

---

## Task 2: Change A and D — move the deep liveness run to the nightly tier

This task implements **Change A** and **Change D** from the spec together, because D edits one line
of the very block A moves and splitting them would mean touching it twice.

**Change A:** `breaklock-posix-liveness` is 59.4 minutes of a 98.4-minute job and covers no label
uniquely, so it moves to the nightly tier.

**Change D:** the same run sat at 66% of its declared 90-minute timeout, where every other measured
run is at 38% or below. It gains headroom on the way across.

**Files:**
- Modify: `models/lockproto/expected.toml` (remove lines 617-633)
- Modify: `models/lockproto/expected-extended.toml` (add the block, widen `scenarios`, fix the header)

- [ ] **Step 1: Confirm the block is where this plan says it is**

```bash
sed -n '617,620p' models/lockproto/expected.toml
sed -n '633p' models/lockproto/expected.toml
```

Expected: lines 617-620 are `[[run]]`, `name = "breaklock-posix-liveness"`,
`module = "LockProtocol"`, `config = "configs/breaklock-posix-liveness.cfg"`, and line 633 is
`timeout_minutes = 90`.

If it differs, STOP and report `STATE_MISMATCH` — do not hunt for the block and adapt, because the
line numbers here are load-bearing for the cut.

- [ ] **Step 2: Cut the block out of `expected.toml`**

Delete lines **617 through 633 inclusive**, and the single blank line that follows at 634. Line 617
is the `[[run]]` opener; line 633 is `timeout_minutes = 90`. Do not touch the comment block at
635-636 — it introduces the NEXT scenario, not this run.

After the cut, `grep -c breaklock-posix-liveness models/lockproto/expected.toml` must print `0`.

- [ ] **Step 3: Widen `scenarios` in the extended tier**

In `models/lockproto/expected-extended.toml`, line 12 currently reads:

```toml
scenarios = ["recovery"]
```

Change it to:

```toml
scenarios = ["breaklock", "recovery"]
```

**This is not optional and is not cosmetic.** `run.py`'s `_load_run` enforces
`_require(scenario in scenarios, ...)`, so without it the file fails to load with
`run 'breaklock-posix-liveness': scenario 'breaklock' is not in 'scenarios'`.

- [ ] **Step 4: Correct the extended tier's header comment**

`expected-extended.toml` opens by describing itself as the host-crash tier, and line 5-6 currently
claim:

```
# The suite-wide union is not judged here: host crashes share `env_loop`, so these runs cover no label
# the process-crash runs in expected.toml miss.
```

That reasoning stops describing the whole file once a breaklock liveness run lives in it. Replace
those two lines with:

```
# The suite-wide union is not judged here. That held originally because host crashes share `env_loop`
# and so cover no label the process-crash runs in expected.toml miss; it also holds for the
# breaklock liveness run moved here on 2026-09-22, which was measured to cover 70 labels and zero of
# them uniquely.
```

- [ ] **Step 5: Paste the block into `expected-extended.toml`, with the new timeout**

Append this at the END of the file, after the last existing `[[run]]` block, separated by one blank
line. It is the block you cut in Step 2 with `timeout_minutes` changed from `90` to `120`:

```toml
[[run]]
name = "breaklock-posix-liveness"
module = "LockProtocol"
config = "configs/breaklock-posix-liveness.cfg"
scenario = "breaklock"
kind = "liveness"
constants = { Owners = ["o1"], Recoverers = [], PlainRuns = [], Cleanups = [], Breakers = ["b1", "b2"], MaxObjs = 3, MaxCrashes = 2, HostCrashes = false, MaxLeaseExpiries = 0, Platform = "posix", IdentityStrength = "strong", LockCapability = "strong", SEED_RECOVER_FOREIGN = false, SEED_RECOVER_UNCERTAIN = false, SEED_DEAD_AS_BUSY = false, SEED_RECOVER_UNCERTAIN_CLEANUP_LOCK = false, SEED_ACQUIRER_UNLINKS_BY_NAME = false, SEED_RESTART_RELEASES = false, SEED_MOVE_ASIDE_FOR_UNCERTAIN = false, SEED_RENAME_OVER_TAKEOVER = false, SEED_NO_CAPABILITY_GATE = false, SEED_TORN_AS_FOREIGN = false, SEED_FS_LOCK_WITHOUT_HANDLE = false, SEED_FS_ALIEN_CONTENT = false, SEED_CHECK_REFUSES_UNTOUCHED = false }
unreached = [
    { label = "S96_1_backoff", reason = "the 96.1 directory-lock conflict: this scenario has no directory lock; dirlock reaches it" },
    { label = "S240_3_putback", reason = "240.3 step 3 finds a different file only after a 240.5 in-place takeover rewrote the record while another actor was inside the recovery path, which needs a lapsed lease; the remote scenarios reach it" },
    { label = "S99_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S21_1_s3_refuse_close", reason = "a Section 99 or 21.1 step 3 check fails only for a holder whose lock was taken while it ran; under a strong capability a running holder keeps its OS-native lock, so no takeover wins against it; breaklock-remote reaches it" },
    { label = "S240_5_seed_write_begin", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_write_end", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
    { label = "S240_5_seed_rename", reason = "reached only with SEED_RENAME_OVER_TAKEOVER; its seeded run reaches it" },
]
timeout_minutes = 120
```

Copy it VERBATIM apart from that one number. The `unreached` list is a set of individually justified
coverage exemptions; a dropped entry silently changes what the run is allowed to miss.

- [ ] **Step 6: Verify the move loaded**

```bash
grep -c breaklock-posix-liveness models/lockproto/expected.toml
grep -c breaklock-posix-liveness models/lockproto/expected-extended.toml
python models/lockproto/run.py --expected models/lockproto/expected-extended.toml --list-jobs
```

Expected: `0`, then `2` (the `name` line and the `config` path — both live in the run's own block and
move together), then JSON containing an entry
`{"scenario": "breaklock", "platform": "posix", "job": "posix"}` alongside the two recovery entries.

If `--list-jobs` raises `scenario 'breaklock' is not in 'scenarios'`, Step 3 was skipped.

- [ ] **Step 7: Commit**

```bash
git add models/lockproto/expected.toml models/lockproto/expected-extended.toml
git commit -m "ci(model): move the deep breaklock liveness run to the nightly tier

It is 59.4 minutes of a 98.4-minute job and covers 70 labels, zero of them
uniquely, so the per-PR coverage union is unaffected - measured with run.py's
own parse_coverage over the artifacts of run 35712320171.

Its timeout goes 90 to 120. It ran at 66% of the declared 90 where every other
measured run sits at 38% or below, and state-space growth is not linear in the
constants, so a timeout should fail on a hang rather than on ordinary growth.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 3: Change B — split `breaklock-remote (posix)` into two CI jobs

That job is 83.6 minutes: a 45.4-minute `check`, a 31-minute `fixed-check`, and six short witnesses.
Moving the two `fixed-*` runs into their own job leaves ~52 and ~31.

**Files:**
- Modify: `models/lockproto/expected.toml` (two `job` keys added)

- [ ] **Step 1: Confirm both runs are where this plan says**

```bash
grep -n 'name = "breaklock-remote-posix-fixed-check"' models/lockproto/expected.toml
grep -n 'name = "breaklock-remote-posix-fixed-witness-NoLiveWriter"' models/lockproto/expected.toml
```

Expected: line numbers near 787 and 803. They will have shifted by 18 after Task 2's cut — that is
expected and fine; what matters is that each exists exactly once.

If either returns nothing or more than one line, STOP and report `STATE_MISMATCH`.

- [ ] **Step 2: Add the job key to both runs**

In the `breaklock-remote-posix-fixed-check` block, immediately after its
`scenario = "breaklock-remote"` line, add:

```toml
job = "posix-fixed"
```

In the `breaklock-remote-posix-fixed-witness-NoLiveWriter` block, immediately after its
`scenario = "breaklock-remote"` line, add the same line:

```toml
job = "posix-fixed"
```

Placement after `scenario` follows the existing `posix-plain` runs, which is the precedent this
mechanism already has in the file. **Add it to exactly these two runs and no others** — the six
witnesses in that job stay with their check, deliberately, so a failing check and the witnesses that
establish its reachability land in one CI log.

- [ ] **Step 3: Verify the assignment — this is the real oracle**

`run.py --list-jobs` CANNOT catch a mistake here: it emits `{scenario, platform, job}` with no run
names, so a run left in the wrong job is invisible to it. Use the run-level map:

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
print(f"\n{len(by)} CI jobs")
EOF
```

Compare against the Task 1 baseline. The delta that must hold:

| scenario | job | runs | delta |
|---|---|---|---|
| `breaklock` | `posix` | **15** | −1, the moved liveness run |
| `breaklock-remote` | `posix` | **6** | −2 |
| `breaklock-remote` | `posix-fixed` | **2** | new |

and **exactly one more CI job than the baseline** — 11 where Task 1 printed 10. Every other row
unchanged.

If `posix-fixed` shows anything other than 2 runs, or `breaklock-remote posix` anything other than
6, a `job` key landed in the wrong block. Fix it before moving on; this is precisely the failure the
job-level check cannot see.

- [ ] **Step 4: Commit**

```bash
git add models/lockproto/expected.toml
git commit -m "ci(model): split breaklock-remote posix, fixed runs into their own job

The job is 83.6 minutes: a 45.4-minute check, a 31-minute fixed-check and six
short witnesses. The two fixed-* runs move to job = posix-fixed, leaving about
52 and 31.

The witnesses stay with their check on purpose. A three-way split would reach
the 45.4-minute floor, and the objection that per-job setup would eat the gain
is false - setup measures about 18 seconds - but it would put a failing check
and the witnesses establishing its reachability in two different CI logs. The 7
minutes is a priced decision.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 4: Confirm the config changes pass the repository gate

**Files:** none — this task only runs checks.

- [ ] **Step 1: Run the gate**

```bash
just check
```

Expected: exit 0. This runs `fmt-check`, `clippy -D warnings`, `typos` and the test suite. No Rust
changed, so the interesting part is `typos` over the edited TOML and the `model_stamp` test.

- [ ] **Step 2: Confirm the model's own tests still pass**

```bash
python -m pytest models/lockproto/test_run.py -q
```

Expected: `206 passed, 45 subtests passed`, in about a minute. That was the count before these
changes; the tests exercise `run.py`'s config loading, which is exactly what Tasks 2 and 3 touched,
so the count should not move. A LOWER count means a test errored out rather than failed visibly.

If a test fails naming `scenarios` or a missing run, re-read Task 2 Step 3.

Do NOT run `just model` or any TLC check locally. TLC runs in CI only; that is a standing constraint
of this repository, not a preference.

---

## Task 5: Change E — the nightly tier reports its own failures

Moving a check from a pull request to a cron changes who finds out when it breaks. `model-extended.yml`
has no notification of any kind today.

**Files:**
- Modify: `.github/workflows/model-extended.yml` (append one job)

- [ ] **Step 1: Confirm the file's shape**

```bash
grep -n '^  [a-z-]*:$' .github/workflows/model-extended.yml
wc -l .github/workflows/model-extended.yml
tail -3 .github/workflows/model-extended.yml
```

Expected: jobs `plan:` and `tier:`, 82 lines, and the file ending with the `Upload TLC output`
step's `path: target/tla/out/`.

- [ ] **Step 2: Append the notify job**

Add this at the END of the file, after the last line, separated by one blank line:

```yaml

  notify:
    # Moving a check off the pull request and onto a cron changes who finds out when it breaks: a PR
    # check is synchronous and addressed to whoever caused it, a cron job is neither. This is the
    # only notification in the repository, added when the deep breaklock liveness run moved here.
    #
    # A SEPARATE job, not a step inside `tier`: `tier` is a matrix, so a failure step within it fires
    # once per failing entry and would open several issues for one bad night.
    #
    # Pull requests are excluded. The extended tier runs on a PR carrying the `model-extended` label,
    # and a failure there is already visible on the PR itself; an issue would be noise.
    name: Report a failure
    needs: [plan, tier]
    if: failure() && github.event_name != 'pull_request'
    runs-on: ubuntu-latest
    # Job-level rather than workflow-level: every other job here needs only `contents: read`, and
    # docs.yml already establishes this pattern for a single privileged job.
    permissions:
      issues: write
    steps:
      - name: Open or update the failure issue
        env:
          GH_TOKEN: ${{ github.token }}
          GH_REPO: ${{ github.repository }}
          TITLE: Nightly model tier failed
          RUN_URL: ${{ github.server_url }}/${{ github.repository }}/actions/runs/${{ github.run_id }}
          SHA: ${{ github.sha }}
        run: |
          set -euo pipefail
          # Deduplicate on the title: an ongoing breakage should be ONE issue that grows, not one
          # issue per night. `--search ... in:title` is a fuzzy match, so the exact comparison is
          # done here rather than trusted to the search.
          number=$(gh issue list --state open --limit 50 --json number,title \
            --jq "map(select(.title == \"$TITLE\")) | .[0].number // empty")
          body=$(printf '%s\n\n%s\n%s\n' \
            "The extended-tier model check failed." \
            "Commit: $SHA" \
            "Run: $RUN_URL")
          if [ -n "$number" ]; then
            gh issue comment "$number" --body "$body"
            echo "commented on existing issue #$number"
          else
            gh issue create --title "$TITLE" --body "$body"
            echo "opened a new issue"
          fi
```

- [ ] **Step 3: Check the workflow is valid before pushing anything**

```bash
actionlint .github/workflows/model-extended.yml
typos .github/workflows/model-extended.yml
```

Expected: both silent, exit 0. `actionlint` also runs `shellcheck` over the `run:` block, which is
the part most likely to be wrong.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/model-extended.yml
git commit -m "ci(model): the extended tier opens an issue when it fails

There was no notification of any kind in any workflow in this repository, and
this spec is what moves a defect class into the nightly tier, so the mechanism
that makes the tier's verdict visible belongs with it.

A separate job rather than a step inside tier, because tier is a matrix job and
a failure step in it fires once per failing entry. Deduplicated on the title,
so an ongoing breakage is one issue that grows rather than one per night. Pull
requests are excluded: the tier runs on a labelled PR and a failure there is
already visible on the PR.

issues: write sits on this job alone rather than on the workflow, following the
job-level elevation docs.yml already uses.

This is also why the release path is deliberately NOT gated. Hard-gating a
synchronous release pipeline on an asynchronous cron means either running the
suite inside release.yml or polling the API for the last cron's verdict. What
closes the risk instead is salience: merging a release PR means visiting the
repository, where an open failure issue sits beside the Pull requests tab.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

---

## Task 6: Prove the notify job actually fires

`if: failure()` cannot be demonstrated by a passing run. This task deliberately breaks something,
observes the notification, and reverts.

**Files:**
- Modify then revert: `models/lockproto/expected-extended.toml`

- [ ] **Step 1: Break one extended-tier run**

In `models/lockproto/expected-extended.toml`, find the run named
`recovery-posix-hostcrash-check` and change its `timeout_minutes` from **`150`** to **`1`**. One
minute is far below what it needs, so it fails on timeout rather than on anything ambiguous. Note
the original value here — Step 5 reverts to it, and `150` is what you are checking for.

Commit it on the branch with a message that says it is temporary:

```bash
git add models/lockproto/expected-extended.toml
git commit -m "temp: force an extended-tier failure to verify the notify job

Reverted in the next commit. Verifies change E end to end.

Co-Authored-By: Claude Opus 5 <noreply@anthropic.com>"
```

- [ ] **Step 2: Push the branch and dispatch the extended tier**

Pushing is an outward action. **STOP HERE and ask the user for approval before running this.**

```bash
git push -u origin spec/model-ci-latency
gh workflow run model-extended.yml --ref spec/model-ci-latency
```

`workflow_dispatch` is used rather than the `model-extended` label because the notify job excludes
pull requests, so a label-triggered run would correctly NOT notify and would prove nothing.

- [ ] **Step 3: Watch it fail and check the issue**

```bash
gh run list --workflow "Model (extended tier)" --limit 1
gh issue list --state open --search "Nightly model tier failed in:title"
```

Expected: the run concludes `failure`, and **exactly ONE** issue exists with that title.

**The count is the whole point of this task.** One issue means the notify job is a separate job, as
intended. Several issues means it was written as a step inside the `tier` matrix and fired once per
failing entry — go back to Task 5 Step 2.

- [ ] **Step 4: Dispatch a second time and confirm deduplication**

```bash
gh workflow run model-extended.yml --ref spec/model-ci-latency
```

When it finishes, re-run the issue list. Expected: still exactly ONE issue, now carrying a second
comment. A second issue means the dedup query in Task 5 is wrong.

- [ ] **Step 5: Revert the deliberate breakage and close the issue**

```bash
git revert --no-edit HEAD~1
```

Adjust the ref if other commits landed in between; the target is the `temp:` commit from Step 1.
Confirm the revert landed:

```bash
grep -A20 'name = "recovery-posix-hostcrash-check"' models/lockproto/expected-extended.toml | grep -m1 timeout_minutes
```

Expected: `timeout_minutes = 150`.

Close the verification issue by hand — it describes a failure you caused on purpose:

```bash
gh issue close <number> --comment "Deliberate failure, verifying the notify job. Reverted."
```

- [ ] **Step 6: Commit**

The revert is already a commit. Push it:

```bash
git push origin spec/model-ci-latency
```

---

## Task 7: Confirm the latency win in CI

**Files:** none.

- [ ] **Step 1: Trigger a full Model run on the branch**

The branch changes `models/`, which is in `model.yml`'s watched-path pattern, so pushing already
triggered it. Find the run:

```bash
gh run list --workflow Model --branch spec/model-ci-latency --limit 1
```

- [ ] **Step 2: Measure the critical path when it finishes**

```bash
RUN=$(gh run list --workflow Model --branch spec/model-ci-latency --limit 1 --json databaseId -q '.[0].databaseId')
gh run view "$RUN" --json jobs -q '.jobs[] | "\(.name)  \(.startedAt)  \(.completedAt)"'
```

Expected shape, against the before-numbers from the spec:

| job | before | after |
|---|---|---|
| `Scenario breaklock (posix)` | 98.4 min | about 39 min |
| `Scenario breaklock-remote (posix)` | 83.6 min | about 52 min |
| `Scenario breaklock-remote (posix-fixed)` | — | about 31 min, NEW |

The critical path should be about **52 minutes**, down from 98.4. These are wall-clock measurements
on shared runners, so treat anything within a few minutes as agreement; what must hold is that
`breaklock (posix)` no longer dominates and a `posix-fixed` job exists.

- [ ] **Step 3: Confirm the coverage union still passes**

In the same run, the `Suite-wide coverage union` job must conclude `success`.

**If it fails, the change must be reverted, not patched.** The spec's central measurement was that
the moved run covers no label uniquely; a failing union means that measurement was wrong, and
widening a coverage exemption to make it pass would be hiding the error rather than finding it.

---

## Task 8: Open the pull request

**STOP. Pushing and opening a pull request are outward actions. Ask the user for approval before
this task, and do not run it as part of an automated sequence.**

- [ ] **Step 1: Run the gate one final time**

```bash
just check
```

- [ ] **Step 2: Open the PR**

```bash
gh pr create --base main --title "ci(model): cut the model suite's time-to-verdict to about 52 minutes"
```

Body: the measured before-and-after critical path, that the moved run covers no label uniquely, that
the release path is deliberately ungated and why, and the two deferred items — the shallow per-PR
liveness run, and the 45.4-minute floor that only larger runners get past.
