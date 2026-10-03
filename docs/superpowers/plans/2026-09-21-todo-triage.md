# TODO Triage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each of the 49 open items in `TODO.md` one of five verdicts, backed by measurement for every closure, so the next plan is chosen from work that actually exists.

**Architecture:** Everything happens in `TODO.md`. Pass one (done while authoring, because it is cheap and reasoned) marks 21 closure candidates. Pass two runs every check as one batch and reads all the evidence before assigning any verdict. Closed items move to a `## Closed` section with their evidence on the line; the git diff is the record.

**Tech Stack:** `git`, `grep`, a text editor. No intermediate files, no generated commit message, no scripts that rewrite `TODO.md`.

---

## Conventions

`<scratch>` is one directory you create at Task 1 for the check output; nothing in it is committed.

## Spec

`docs/superpowers/specs/2026-09-21-todo-triage-design.md`. **Read its `## Outputs` section first** —
it was amended after three panel rounds and it explains why this plan has no data pipeline.

## Verified facts this plan rests on

Checked against `origin/main` at `76e0ff1` while writing. Re-check if `main` has moved.

| fact | how it was checked |
|---|---|
| `TODO.md` has exactly 49 `- [ ]` items in 7 sections | counted by script |
| Sections start at lines 5, 27, 40, 49, 62, 79, 105 | `grep -n "^## " TODO.md` |
| All 101 continuation lines are indented exactly 6 spaces | counted by script |
| `ci.yml`, `model.yml`, `run.py`, `_typos.toml`, `lefthook.yml`, `justfile`, `FLUX_FULL_UPDATED_SPEC_V16.md` exist | `test -e` on each |
| `.antigravityignore` IS tracked on `main` | `git ls-files .antigravityignore` returns it |

**A correction to carry.** Commit `7c085bf` says `PROMOTED (16)` and added **18**; its arithmetic sums
to 27 against a stated "20". It is merged and cannot be amended, so any count this plan writes comes
from `grep -c`, never from memory.

---

## Task 1: Branch

- [ ] **Step 1: Branch from current main**

```bash
git -C C:/Users/user/.claude/jobs/60989684/tmp/wt-close fetch -q origin main
git -C C:/Users/user/.claude/jobs/60989684/tmp/wt-close worktree add -b chore/todo-triage <scratch>/todotriage origin/main
cd <scratch>/todotriage
git log --oneline -1
grep -c "^- \[ \]" TODO.md
```

Expected: the head commit, then `49`. If not 49, `main` has moved and the candidate list in Task 2
must be re-derived before continuing.

---

## Task 2: Pass one — the candidate list

Pass one is reasoned, not measured, and is **already done**. Record it; do not re-derive it.

**This deviates from the spec deliberately.** The spec puts pass one in the execution; doing it at
authoring time makes the plan executable with no context. Candidacy is not a verdict — it decides only
what gets measured, and the bias is toward over-inclusion, so a mistake costs one extra measurement
rather than a wrong closure.

21 of 49 items are closure candidates. The other 28 are `LIVE` by default: all 8 of Phase 2 (no product
code exists), 3 of the 5 open decisions (a decision nobody has taken cannot be `DONE`), 2 scaffolding
placeholders, and 15 of the 18 plan-3 items (written today from measured evidence).

| line | section | item | note |
|---|---|---|---|
| 17 | open decisions | model-check the lock protocol | `SPLIT`-suspect |
| 24 | open decisions | vulnerability reporting + branch protection | `SPLIT`-suspect |
| 42 | scaffolding | install `cargo-mutants` | machine-state, see Task 3 |
| 44 | scaffolding | `lefthook install` in each clone | machine-state, see Task 3 |
| 53 | CI hygiene | no MSRV job | |
| 56 | CI hygiene | `ci.yml` lacks permissions/concurrency | |
| 59 | CI hygiene | `_typos.toml` excludes the spec by literal name | |
| 67, 71, 74 | spec gaps | Windows rename, file identity, WSL 9p | |
| 84, 86, 89, 92 | plan 2 | TLC jar cache, `--no-renames`, `tracking`, cp1252 | |
| 95, 97, 98, 100 | plan 2 | M5 to M8 | 95 is a dedup-suspect against 121 |
| 114, 121, 196 | plan 3 | `pendingUnlink`, `tornRead`, `.antigravityignore` | |

- [ ] **Step 1: Read the table above and confirm each line number still names that item**

```bash
sed -n '17p;24p;42p;44p;53p;56p;59p;67p;71p;74p;84p;86p;89p;92p;95p;97p;98p;100p;114p;121p;196p' TODO.md | cut -c1-70
```

Expected: 21 lines, each starting `- [ ]` and matching the table. A mismatch means `main` moved.

---

## Task 3: Derive every check before reading any result

Write the commands down first; run them in Task 4. Assigning a verdict and then looking for evidence
produces evidence fitted to the verdict.

- [ ] **Step 1: Write `<scratch>/checks.sh`**

```bash
set -u
cd "$(dirname "$0")/todotriage" || { echo "FATAL: worktree missing - every check below would report NO MATCH from the wrong directory"; exit 1; }
test -f TODO.md || { echo "FATAL: not at the worktree root"; exit 1; }

echo "=== L53 MSRV job ==="
grep -n "1\.85\|msrv\|MSRV\|toolchain" .github/workflows/ci.yml || echo "NO MATCH"
echo "--- control ---"; grep -c "^jobs:" .github/workflows/ci.yml

echo "=== L56 permissions / concurrency ==="
grep -n "^permissions:\|^concurrency:" .github/workflows/ci.yml || echo "NO MATCH"
echo "--- control ---"; grep -c "^on:" .github/workflows/ci.yml

echo "=== L59 typos excludes the spec ==="
grep -n "extend-exclude\|FLUX_FULL\|V1[56]" _typos.toml || echo "NO MATCH"

echo "=== L84 TLC jar cache ==="; grep -n "actions/cache\|tla2tools" .github/workflows/model.yml || echo "NO MATCH"
echo "=== L86 --no-renames ==="; grep -n "no-renames" .github/workflows/model.yml || echo "NO MATCH"
echo "=== L89 tracking validated ==="; grep -n "tracking" models/lockproto/run.py || echo "NO MATCH"
echo "=== L92 cp1252 stdout ==="; grep -n "reconfigure\|PYTHONIOENCODING" models/lockproto/run.py .github/workflows/model.yml || echo "NO MATCH"

echo "=== L67/L71/L74 spec gaps ==="
grep -c "MoveFileEx" FLUX_FULL_UPDATED_SPEC_V16.md
grep -c "FILE_ID_INFO" FLUX_FULL_UPDATED_SPEC_V16.md
grep -c "9p\|v9fs" FLUX_FULL_UPDATED_SPEC_V16.md
echo "--- control (a term certainly present) ---"; grep -c "lock" FLUX_FULL_UPDATED_SPEC_V16.md

echo "=== L95 vs L121 tornRead dedup ==="; grep -n "tornRead" models/lockproto/algorithm.txt
echo "=== L114 pendingUnlink ==="; grep -n "pendingUnlink" models/lockproto/algorithm.txt | head -5
echo "=== L196 .antigravityignore ==="; git ls-files .antigravityignore | grep . && echo TRACKED || echo UNTRACKED
echo "=== L17 model-check ==="; grep -c "SingleWriter" models/lockproto/invariants.txt; python models/lockproto/run.py --list-jobs | head -c 200; echo

echo "=== L42/L44 - MACHINE STATE, NOT REPOSITORY STATE ==="
command -v cargo-mutants >/dev/null && echo INSTALLED || echo ABSENT
grep -n "cargo-mutants" .claude/recommended-tools.json || echo "NOT DECLARED"
test -f .git/hooks/pre-push && echo "HOOK PRESENT" || echo "HOOK ABSENT"
grep -n "lefthook" justfile || echo "NO BOOTSTRAP RECIPE"

echo "=== L24 - GITHUB SETTING, NOT REPOSITORY STATE ==="
gh api repos/ckir/flux/branches/main/protection --jq '.required_status_checks.strict' 2>&1 | head -1
```

**Every grep that can return nothing carries a positive control.** A negative grep cannot return its
failing answer; this project has twice nearly concluded something from one.

**Three items cannot be closed by their own checks.** `command -v cargo-mutants`,
`test -f .git/hooks/pre-push` and the `gh api` call measure the executor's machine or a GitHub
setting, not the repository. The spec's bar is re-checkable from the repository alone, so L42, L44 and
L24 are `UNVERIFIABLE` by construction whatever those commands print. Their **in-repo halves** —
declared in `recommended-tools.json`, a lefthook recipe in the `justfile` — can close, which makes
each a `SPLIT`.

---

## Task 4: Run the checks, as one batch

- [ ] **Step 1: Run**

```bash
bash <scratch>/checks.sh 2>&1 | tee <scratch>/evidence.txt
```

Expected: every section prints matches or an explicit `NO MATCH` / `ABSENT` / `UNTRACKED`, and every
control prints a non-zero count. **If a control prints `0`, the probe is broken — fix it and re-run
before reading anything else.**

- [ ] **Step 2: Read `evidence.txt` end to end before judging anything**

---

## Task 5: Triage in `TODO.md`

**Files:** Modify: `TODO.md`

Work one item at a time, top to bottom, in an editor. There is no script: the file is 199 lines and a
human reading it is the check.

- [ ] **Step 1: Add the `## Closed` section at the end of the file**

```markdown
## Closed

Triaged 2026-09-22. Kept here rather than deleted: the evidence for a closure belongs where the item
was, not only in a commit message.
```

- [ ] **Step 2: Move each closed item into it, one line each**

For every item the evidence settles, cut it from its section and write one line in `## Closed`:

```markdown
- `DONE` **Model-check the lock protocol before implementing it.** `recovery` runs two Recoverers and
  `breaklock` two Breakers; `SingleWriter` is checked by 43 configs — `models/lockproto/expected.toml`.
```

Rules, all from the spec:

- `DONE`, `OVERTAKEN` and `SPLIT` need a **measurement** — a `file:line`, a test name, a commit, or a
  quoted command output from `evidence.txt`. If you cannot write one, the verdict is `UNVERIFIABLE`
  and the item **stays where it is**.
- A `SPLIT` leaves its live half in place, rewritten to stand alone, and moves only the closed half.
- `LIVE` and `UNVERIFIABLE` items are not touched at all.

- [ ] **Step 3: Check the arithmetic**

```bash
grep -c "^- \[ \]" TODO.md
grep -c "^- \`" TODO.md
```

Expected: the two numbers sum to **49**. If they do not, an item was lost or duplicated while moving —
`git diff` will show which.

- [ ] **Step 4: Read the diff, which is the record**

```bash
git diff --stat
git diff TODO.md | head -80
```

Every closed item should appear once as a removal and once as an addition under `## Closed`. A removal
with no matching addition is a lost item.

- [ ] **Step 5: Lint**

```bash
typos
```

Expected: exit 0, no output.

---

## Task 6: Commit

- [ ] **Step 1: Stage only `TODO.md`**

```bash
git add TODO.md
git status --short
```

Expected: exactly ` M TODO.md`. Anything else means an already-true fix leaked in; unstage it for
Task 7.

- [ ] **Step 2: Commit**

Subject: `docs(todo): triage the 49 open items`. The body gives the counts and names the two or three
closures most likely to be argued with. **It is a summary, not the only record** — the evidence is in
the file and the diff.

```bash
git commit
git show --stat HEAD
```

---

## Task 7: Already-true fixes, separately

Only an item that names a state the repository should be in, where reaching it is a single obvious
edit and no judgement is involved. Anything needing a decision, a test or a measurement run is `LIVE`.

- [ ] **Step 1: Apply them, if Task 5 produced any**

Authoring-time checking already settled the one candidate earmarked for this: `.antigravityignore`
**needs no repository change.** It is tracked on `main`, and appears untracked only in the primary
working tree because that tree sits on the stale `model/lock-protocol` branch. The remedy is to move
that tree, not to edit the repository.

If nothing else qualifies, **skip this task and say so** rather than inventing work.

---

## Task 8: Push and open the pull request

- [ ] **Step 1: Push**

```bash
git push -u origin chore/todo-triage
```

- [ ] **Step 2: Open the PR**

Body: the counts, the verdict distribution, and the closures most likely to be argued with. Link the
spec's `## Outputs` section, which explains why closed items are kept rather than deleted.

- [ ] **Step 3: Report anything the measurement turned up**

If Task 4 surfaced a new problem, append it to `.clavity/local-anomalies.md` and name it in the PR. Do
not fold it into the triage.
