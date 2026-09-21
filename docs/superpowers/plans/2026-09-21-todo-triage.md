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
python - <<'EOF' > <scratch>/items.tsv
lines = open("TODO.md", encoding="utf-8").read().splitlines()
sec, cur, out = None, None, []
for i, l in enumerate(lines, 1):
    if l.startswith("## "):
        sec, cur = l[3:].strip(), None
    elif l.startswith("- [ ]"):
        cur = [i, sec, [l[6:].strip()]]; out.append(cur)
    elif cur is not None and l.startswith("      "):
        cur[2].append(l.strip())
    elif not l.strip():
        cur = None
for i, s, parts in out:
    print(f"{i}\t{s}\t" + " ".join(parts).replace("\t", " "))
EOF
wc -l < <scratch>/items.tsv
awk -F'\t' '{print length($3)}' <scratch>/items.tsv | sort -n | tail -1
```

Expected: `49`, and a longest-item length well over 80 — most items run to several lines, and all
101 continuation lines in the file are indented exactly six spaces (measured), which is what the
`startswith("      ")` branch matches. **The text is captured whole.** Once an item is deleted the
commit message is the working record of what it said; an inventory that keeps only first lines
produces a record of fragments. The full text remains recoverable from `git show <parent>:TODO.md`,
because `TODO.md` is tracked — but that is a fallback, not a reason to write a poor message.

---

## Task 2: Pass one — the candidate list

Pass one is reasoned, not measured, and is **already done**. Record it; do not re-derive it.
**This deviates from the spec, deliberately.** `SPEC.md` describes pass one as part of the execution;
doing it at authoring time is what makes this plan executable by someone with no context. Candidacy is
not a verdict — it decides only what gets measured, and the bias is toward over-inclusion, so an error
costs one extra measurement rather than a wrong closure. To have the executor do it blind instead,
delete the list below and have them derive it from `items.tsv`; nothing downstream depends on who
produced it.

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
cd "$(dirname "$0")/todotriage" || { echo "FATAL: worktree missing - every check below would report NO MATCH from the wrong directory"; exit 1; }
test -f TODO.md || { echo "FATAL: not at the worktree root"; exit 1; }
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
grep -n "cargo-mutants" .claude/recommended-tools.json || echo "NOT DECLARED"

# WARNING: the two checks above and the two below measure THE EXECUTOR'S MACHINE, not the
# repository. The spec's bar is re-checkable from the repository alone, so L42 and L44 are
# UNVERIFIABLE by construction, exactly like L24. What CAN be closed from the repository is the
# adjacent half - whether cargo-mutants is declared in recommended-tools.json, and whether the
# justfile carries a lefthook bootstrap recipe - which makes each a SPLIT, not a closure.

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
awk -F'\t' '{print $1"\t"$2"\t"$3"\tLIVE\t"}' <scratch>/items.tsv > <scratch>/verdicts.tsv
awk -F'\t' 'NF!=5 {print "MALFORMED ROW:", NR}' <scratch>/verdicts.tsv
```

Expected: `49`. Every item starts as `LIVE` with empty evidence, so an item can only leave that state
by having evidence written against it — the default is the safe one.

- [ ] **Step 2: Fill in the verdicts that the evidence changes**

One row per item, **five tab-separated fields**: `line`, `section`, `text`, `verdict`, `evidence`. Leave `text` exactly as seeded — the seed, this instruction and Task 7's
generator must agree on five fields or the generator raises `ValueError` on unpacking. Every `DONE`,
rows carry the reason they cannot be settled.

- [ ] **Step 3: Check the acceptance bar mechanically**

```bash
awk -F'\t' '$4=="DONE"||$4=="OVERTAKEN"||$4=="SPLIT"' <scratch>/verdicts.tsv | grep -c "" 
awk -F'\t' '($4=="DONE"||$4=="OVERTAKEN"||$4=="SPLIT") && $5 !~ /[A-Za-z0-9_.\/-]+:[0-9]+|[0-9a-f]{7,40}|`[^`]+`/' <scratch>/verdicts.tsv
```

Expected: the second command prints **nothing**.

**This is a structural test, not a length test.** An earlier draft asked only that the evidence field
be 20 characters or more, which "the grep showed nothing" satisfies at 22 while being no evidence at
all — a gate measuring effort instead of substance is the compliance theatre this plan exists to
avoid. The regex demands one of three shapes a reader can follow: a `file:line` reference, a commit
sha of 7 to 40 hex characters, or a backtick-quoted command or output. **Do not widen it to make a
row pass**; widen it only if a legitimate evidence shape is genuinely missing, and say so in the
commit message.

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

**First, print what is about to be destroyed and read it.**

```bash
awk -F'\t' '$4=="DONE"||$4=="OVERTAKEN"' <scratch>/verdicts.tsv | cut -c1-160
```

This is the last point at which a wrong verdict is cheap.

**Then delete BOTTOM-UP.** `verdicts.tsv` is keyed by line number, and removing a line shifts every
line beneath it — so a top-down pass would, from the second deletion onward, hit whatever slid into
the recorded position and silently destroy items nobody evaluated.

```bash
awk -F'\t' '$4=="DONE"||$4=="OVERTAKEN" {print $1}' <scratch>/verdicts.tsv | sort -rn > <scratch>/kill.txt
wc -l < <scratch>/kill.txt
python - <<'EOF'
kill = [int(x) for x in open(r"<scratch>/kill.txt")]
lines = open("TODO.md", encoding="utf-8").read().splitlines(keepends=True)
for n in kill:                       # descending already
    assert lines[n-1].startswith("- [ ]"), f"line {n} is not an item: {lines[n-1]!r}"
    end = n
    while end < len(lines) and lines[end].startswith("      "):
        end += 1
    del lines[n-1:end]
open("TODO.md", "w", encoding="utf-8").writelines(lines)
EOF
```

The `assert` is the guard: if a recorded line no longer points at an item, the mapping has already
drifted and the script stops rather than deleting something arbitrary.

- [ ] **Step 2: Rewrite each `SPLIT` item to its live half**

live half only, rewritten as a standalone item that does not reference the closed half.

- [ ] **Step 2: Add the pointer line**

Directly under the `# TODO` heading's intro line, add:

```markdown
Last triaged 2026-09-22: <N> items closed, <M> remain. The verdicts and the measurement behind
each closure are in the commit titled `docs(todo): triage the 49 open items`, which
`git log --grep='triage the 49'` finds — this file keeps only surviving work.
```

`<N>` and `<M>` come from `wc -l` over `verdicts.tsv`, **not** from memory. The commit `7c085bf`
claimed a promoted count that was wrong by two; do not repeat that.

- [ ] **Step 3: Verify the arithmetic**

```bash
grep -c "^- \[ \]" TODO.md
awk -F'\t' '$4=="LIVE"||$4=="UNVERIFIABLE"' <scratch>/verdicts.tsv | wc -l
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
rows = [l.rstrip("\n").split("\t") for l in open(r"<scratch>/verdicts.tsv", encoding="utf-8") if l.strip()]
by = collections.defaultdict(list)
for line, sec, text, verdict, ev in rows:
    by[verdict].append((line, sec, text, ev))
print("docs(todo): triage the 49 open items")
print()
closed = sum(len(by[v]) for v in ("DONE", "OVERTAKEN", "SPLIT"))
print(f"{closed} closed, {len(rows)-closed} remain. Every closure carries the measurement that")
print("settled it; this message is the only durable record, because TODO.md keeps only live work.")
for v in ("DONE", "OVERTAKEN", "SPLIT", "UNVERIFIABLE", "LIVE"):
    if not by[v]:
        continue
    print()
    print(f"{v} ({len(by[v])})")
    for line, sec, text, ev in by[v]:
        print(f"  TODO.md:{line}  [{sec}]  {text}")
        print(f"        evidence: {ev}")
EOF
head -12 <scratch>/triage-message.txt
```

Expected: a subject line, the counts, then one block per verdict. Confirm the counts in the body match
Task 6's pointer line — if they disagree, one of them was written from memory.

- [ ] **Step 3: Commit**

```bash
git commit -F <scratch>/triage-message.txt
git log -1 --format='%h %s' 
grep -n 'Last triaged' TODO.md
```

Expected: the commit exists, and the pointer line names it **by subject, not by hash**.

**A tracked file cannot contain the hash of the commit that modifies it.** An earlier draft had
Task 6 write `PENDING` and Task 7 amend the real hash in — but amending changes the content, which
changes the hash, so the file would name the previous commit and 'amend again with the new hash'
is an infinite regress. The pointer therefore names the subject line, which `git log --grep` finds
and which survives a rebase or a squash that a hash would not.

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
