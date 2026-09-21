# TODO Triage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Give each of the 49 open items in `TODO.md` one of five verdicts, backed by measurement for every closure, so the next plan is chosen from work that actually exists.

**Architecture:** Two passes. Pass one (already done while authoring this plan, because it is cheap and reasoned) marks 21 closure candidates. Pass two runs every check as a batch, reads all evidence, and only then assigns verdicts. Closed items leave `TODO.md`; their reasons live in the commit message, which a pointer line in `TODO.md` makes findable.

**Tech Stack:** `git`, `grep`, `python` — no build, no tests to run. This is an analysis task whose artifacts are a Markdown file and a commit message.

---

## Conventions

`<scratch>` below means one directory you create at Task 1 and use for every intermediate file;
nothing in it is committed. `<sha>`, `<N>`, `<M>` are values computed during execution, never guessed.

## Spec

`docs/superpowers/specs/2026-09-21-todo-triage-design.md` (commit `b7570a8`, branch `spec/todo-triage`).

Read it first. The five verdicts and the acceptance bar come from it and are not restated in full here.

## Verified facts this plan rests on

All checked against `origin/main` at `76e0ff1` while writing. Re-check if `main` has moved.

| fact | how it was checked |
|---|---|
| `TODO.md` has exactly 49 `- [ ]` items in 7 sections | counted by script, below |
| Sections start at lines 5, 27, 40, 49, 62, 79, 105 | `grep -n "^## " TODO.md` |
| `.github/workflows/ci.yml`, `model.yml`, `models/lockproto/run.py`, `_typos.toml`, `lefthook.yml`, `justfile`, `FLUX_FULL_UPDATED_SPEC_V16.md` all exist | `test -e` on each |
| `.antigravityignore` IS tracked on `main` | `git ls-files .antigravityignore` returns it |

**A correction this plan must carry.** The triage commit `7c085bf` says `PROMOTED (16)`; it added **18**
items (`git show 7c085bf -- TODO.md | grep -c "^+- \[ \]"` → 18). Its arithmetic also does not close:
16+4+5+2 = 27 against a stated "20 entries". That commit is merged and cannot be amended, so the
pointer line this plan writes must carry counts that are correct, and Task 6 states them from a count
rather than from memory.

## File structure

| file | responsibility | change |
|---|---|---|
| `TODO.md` | surviving work only, plus one pointer line | modified |
| `.clavity/local-anomalies.md` | anything new that measurement turns up | appended, if needed |
| the triage commit message | the durable verdict table | written at Task 7 |

No new files. No code.

---

## Task 1: Branch and working file

**Files:**
- Create: `C:/Users/user/AppData/Local/Temp/claude/.../scratchpad/verdicts.tsv` (scratch, not committed)

- [ ] **Step 1: Branch from current main**

```bash
git -C C:/Users/user/.claude/jobs/60989684/tmp/wt-close fetch -q origin main
git -C C:/Users/user/.claude/jobs/60989684/tmp/wt-close worktree add -b chore/todo-triage <scratch>/todotriage origin/main
cd <scratch>/todotriage
git log --oneline -1
```

Expected: the commit `main` is on. If it is not `76e0ff1`, re-run the fact table above before continuing.

- [ ] **Step 2: Emit the item inventory**

```bash
python -c "
import sys
lines=open('TODO.md',encoding='utf-8').read().splitlines()
sec=None
for i,l in enumerate(lines,1):
    if l.startswith('## '): sec=l[3:].strip()
    elif l.startswith('- [ ]'): print(f'{i}\t{sec}\t{l[6:80]}')
" > <scratch>/items.tsv
wc -l < <scratch>/items.tsv
```

Expected: `49`. If not 49, `main` has moved and the candidate list below must be re-derived.

---

## Task 2: Pass one — the candidate list

Pass one is reasoned, not measured, and is **already done**. Record it; do not re-derive it.

21 of 49 items are closure candidates. The other 28 are `LIVE` by default: all 8 of Phase 2 (no product
code has been written), 3 of the 5 open decisions (a decision nobody has taken cannot be `DONE`), 2
scaffolding placeholders, and 15 of the 18 plan-3 items (written today from measured evidence).

- [ ] **Step 1: Write the candidate list to the working file**

```
L17   open-decisions   model-check the lock protocol            SPLIT-suspect
L24   open-decisions   repo housekeeping: vuln reporting + branch protection   SPLIT-suspect
L42   scaffolding      install cargo-mutants
L44   scaffolding      lefthook install in each clone           SPLIT-suspect
L53   ci-hygiene       no MSRV job
L56   ci-hygiene       ci.yml lacks permissions/concurrency
L59   ci-hygiene       _typos.toml excludes the spec by literal name
L67   spec-gaps        Windows replacing rename unnamed
L71   spec-gaps        Windows file identity source
L74   spec-gaps        WSL 9p mounts
L84   plan2            cache the TLC jar in CI
L86   plan2            --no-renames unpinned
L89   plan2            open_findings[].tracking not validated
L92   plan2            cp1252 stdout
L95   plan2            M5 NeverTornRead placement-blind         DEDUP-suspect vs plan-3 tornRead
L97   plan2            M6 recoveredAfterCrash guard always true
L98   plan2            M7 host-crash net holes
L100  plan2            M8 liveness consequent weakening
L121  plan3            tornRead second site                     DEDUP-suspect vs M5
L196  plan3            .antigravityignore untracked
L114  plan3            pendingUnlink could be a boolean         DEDUP-suspect vs nothing; check it is not already done
```

- [ ] **Step 2: Commit nothing yet.** Pass one produces no repository change.

---

## Task 3: Derive every check before reading any result

This ordering is the spec's central requirement. Write all commands down first; run them in Task 4.

- [ ] **Step 1: Write the check script**

Create `<scratch>/checks.sh`:

```bash
set -u
cd "$(dirname "$0")/todotriage"
echo "=== L53 MSRV job ==="
grep -n "1\.85\|msrv\|MSRV\|toolchain" .github/workflows/ci.yml || echo "NO MATCH"
echo "--- positive control (must print jobs:) ---"; grep -c "^jobs:" .github/workflows/ci.yml

echo "=== L56 permissions / concurrency ==="
grep -n "^permissions:\|^concurrency:" .github/workflows/ci.yml || echo "NO MATCH"
echo "--- positive control ---"; grep -c "^on:" .github/workflows/ci.yml

echo "=== L59 typos excludes the spec ==="
grep -n "extend-exclude\|FLUX_FULL\|V1[56]" _typos.toml || echo "NO MATCH"

echo "=== L84 TLC jar cache ==="
grep -n "actions/cache\|tla2tools" .github/workflows/model.yml || echo "NO MATCH"

echo "=== L86 --no-renames ==="
grep -n "no-renames" .github/workflows/model.yml || echo "NO MATCH"

echo "=== L89 open_findings tracking validated ==="
grep -n "tracking" models/lockproto/run.py || echo "NO MATCH"

echo "=== L92 cp1252 stdout ==="
grep -n "reconfigure\|PYTHONIOENCODING" models/lockproto/run.py .github/workflows/model.yml || echo "NO MATCH"

echo "=== L67/L71/L74 spec gaps ==="
grep -c "MoveFileEx" FLUX_FULL_UPDATED_SPEC_V16.md
grep -c "FILE_ID_INFO" FLUX_FULL_UPDATED_SPEC_V16.md
grep -c "9p\|v9fs" FLUX_FULL_UPDATED_SPEC_V16.md
echo "--- positive control (a term certainly in the spec) ---"; grep -c "lock" FLUX_FULL_UPDATED_SPEC_V16.md

echo "=== L95 vs L121 tornRead dedup ==="
grep -n "tornRead" models/lockproto/algorithm.txt

echo "=== L114 pendingUnlink ==="
grep -n "pendingUnlink" models/lockproto/algorithm.txt | head -5

echo "=== L42 cargo-mutants ==="
command -v cargo-mutants >/dev/null && echo INSTALLED || echo ABSENT

echo "=== L44 lefthook ==="
test -f .git/hooks/pre-push && echo "HOOK PRESENT" || echo "HOOK ABSENT"
grep -n "lefthook" justfile || echo "NO BOOTSTRAP RECIPE"

echo "=== L196 .antigravityignore ==="
git ls-files .antigravityignore | grep . && echo TRACKED || echo UNTRACKED

echo "=== L17 model-check item ==="
grep -c "SingleWriter" models/lockproto/invariants.txt
python models/lockproto/run.py --list-jobs | head -c 200; echo

echo "=== L24 branch protection (SEE THE WARNING BELOW) ==="
gh api repos/ckir/flux/branches/main/protection --jq '.required_status_checks.strict' 2>&1 | head -1
gh api repos/ckir/flux/private-vulnerability-reporting --jq '.enabled' 2>&1 | head -1
```

**L24 cannot be closed by this check, and the plan says so rather than letting the executor discover
it.** The spec's acceptance bar is that every closure is re-checkable *from the repository alone*.
Branch protection and vulnerability reporting are GitHub settings; nothing in the tree records them,
and a `gh api` result is exactly the kind of evidence that stops being checkable later. So L24's
verdict is `UNVERIFIABLE` **by construction**, whatever the API returns — the API output goes in the
row as context, not as proof. Closing it would need something in-repo, which is its own piece of work
and is not in this scope.

**Every grep that could return nothing carries a positive control.** A negative grep with no control
cannot return its failing answer; this project has twice nearly drawn a conclusion from one.

- [ ] **Step 2: Do not run it yet.** Read the script through once and confirm each command names a file
      from the verified-facts table.

---

## Task 4: Run the checks, as one batch

- [ ] **Step 1: Run**

```bash
bash <scratch>/checks.sh 2>&1 | tee <scratch>/evidence.txt
```

Expected: every section prints either matches or an explicit `NO MATCH` / `ABSENT` / `UNTRACKED`, and
every positive control prints a non-zero count. **If any positive control prints `0`, the probe is
broken — fix it and re-run before reading anything else.**

- [ ] **Step 2: Read the whole file before judging anything**

```bash
wc -l <scratch>/evidence.txt
```

Do not assign a verdict while running commands. Read `evidence.txt` end to end first.

---

## Task 5: Assign the 49 verdicts

- [ ] **Step 1: Seed the verdict file from the inventory**

```bash
awk -F'	' '{print $1"	"$2"	LIVE	"}' <scratch>/items.tsv > <scratch>/verdicts.tsv
wc -l < <scratch>/verdicts.tsv
```

Expected: `49`. Every item starts as `LIVE` with empty evidence, so an item can only leave that state
by having evidence written against it — the default is the safe one.

- [ ] **Step 2: Fill in the verdicts that the evidence changes**

One row per item: `line`, `section`, `verdict`, `evidence`. Every `DONE`, `OVERTAKEN` and `SPLIT` row
must quote a measurement from `evidence.txt`. `LIVE` rows carry a one-line reason. `UNVERIFIABLE`
rows carry the reason they cannot be settled.

- [ ] **Step 3: Check the acceptance bar mechanically**

```bash
awk -F'\t' '$3=="DONE"||$3=="OVERTAKEN"||$3=="SPLIT"' <scratch>/verdicts.tsv | grep -c "" 
awk -F'\t' '($3=="DONE"||$3=="OVERTAKEN"||$3=="SPLIT") && length($4)<20' <scratch>/verdicts.tsv
```

Expected: the second command prints **nothing**. Any row it prints is a closure whose evidence is too
thin to be re-checked by someone else, which the spec says makes it `UNVERIFIABLE`, not closed.

- [ ] **Step 4: Confirm the count**

```bash
wc -l < <scratch>/verdicts.tsv
```

Expected: `49`.

---

## Task 6: Rewrite `TODO.md`

**Files:**
- Modify: `TODO.md`

- [ ] **Step 1: Remove closed items and rewrite `SPLIT` items to their live half**

Delete the lines of every `DONE` and `OVERTAKEN` item. For each `SPLIT`, replace the item with its
live half only, rewritten as a standalone item that does not reference the closed half.

- [ ] **Step 2: Add the pointer line**

Directly under the `# TODO` heading's intro line, add:

```markdown
Last triaged 2026-09-21 (commit `<sha>`): <N> items closed, <M> remain. The verdicts and the
measurement behind each closure are in that commit's message — this file keeps only surviving work.
```

`<N>` and `<M>` come from `wc -l` over `verdicts.tsv`, **not** from memory. The commit `7c085bf`
claimed a promoted count that was wrong by two; do not repeat that.

- [ ] **Step 3: Verify the arithmetic**

```bash
grep -c "^- \[ \]" TODO.md
awk -F'\t' '$3=="LIVE"||$3=="UNVERIFIABLE"' <scratch>/verdicts.tsv | wc -l
```

Expected: the two numbers are **equal**, and equal to `<M>`. If they differ, an item was dropped or
kept by accident.

- [ ] **Step 4: Lint**

```bash
typos
```

Expected: exit 0, no output.

---

## Task 7: Commit the triage

- [ ] **Step 1: Stage only `TODO.md`**

```bash
git add TODO.md
git status --short
```

Expected: exactly one line, ` M TODO.md`. Staging anything else means an already-true fix leaked into
the analysis commit; unstage it for Task 8.

- [ ] **Step 2: Write the commit message file**

```bash
python - <<'EOF' > <scratch>/triage-message.txt
import collections
rows = [l.rstrip("
").split("	") for l in open(r"<scratch>/verdicts.tsv", encoding="utf-8")]
by = collections.defaultdict(list)
for line, sec, verdict, ev in rows:
    by[verdict].append((line, sec, ev))
closed = sum(len(by[v]) for v in ("DONE", "OVERTAKEN", "SPLIT"))
print("docs(todo): triage the 49 open items")
print()
print(f"{closed} closed, {len(rows)-closed} remain. Every closure carries the measurement that")
print("settled it; this message is the only durable record, because TODO.md keeps only live work.")
for v in ("DONE", "OVERTAKEN", "SPLIT", "UNVERIFIABLE", "LIVE"):
    if not by[v]:
        continue
    print()
    print(f"{v} ({len(by[v])})")
    for line, sec, ev in by[v]:
        print(f"  TODO.md:{line}  [{sec}]  {ev}")
EOF
head -12 <scratch>/triage-message.txt
```

Expected: a subject line, the counts, then one block per verdict. Confirm the counts in the body match
Task 6's pointer line — if they disagree, one of them was written from memory.

- [ ] **Step 3: Commit**

```bash
git commit -F <scratch>/triage-message.txt
git log -1 --format=%B | head -20
```

---

## Task 8: Already-true fixes, separately

Only items whose fix is a single obvious edit with no judgement involved. Anything needing a decision,
a test or a measurement run is `LIVE` and is not touched here.

- [ ] **Step 1: Apply them, if any survived Task 5**

Authoring-time checking already found that the one candidate earmarked for this — `.antigravityignore`
— needs **no repository change**: it is tracked on `main`, and only appears untracked in the primary
working tree because that tree sits on the stale `model/lock-protocol` branch. Its verdict is decided
in Task 5 from the evidence, and the remedy is to move that tree, not to edit the repository.

If Task 5 produced no other already-true item, **skip this task and say so** rather than inventing work.

- [ ] **Step 2: Commit separately, if there was anything**

```bash
git add <the paths>
git commit -m "chore: <what became true>"
```

---

## Task 9: Push and open the pull request

- [ ] **Step 1: Push**

```bash
git push -u origin chore/todo-triage
```

- [ ] **Step 2: Open the PR**

Body: the counts, the verdict distribution, and the two or three closures most likely to be argued
with. Not the whole table — that is in the commit message, and duplicating it lets the two drift.

- [ ] **Step 3: Report anything measurement turned up**

If Task 4 surfaced a new problem, append it to `.clavity/local-anomalies.md` and name it in the PR. Do
not fold it into the triage.
