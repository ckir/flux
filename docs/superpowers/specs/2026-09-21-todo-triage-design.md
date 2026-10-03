# TODO triage — design

**Date:** 2026-09-21
**Status:** approved, not yet executed
**Scope:** classify the 49 open items in `TODO.md`; fix only what is already true.

## The problem

`TODO.md` carries 49 open items across seven sections — open decisions, Phase 2 product work,
scaffolding chores, CI hygiene, spec gaps, and two sets of lock-model follow-ups. They share no
interfaces and no test surface, so they are not one project and cannot be one implementation plan.

Some of them are also no longer true. The largest item under **Open decisions** reads:

> Model-check the lock protocol before implementing it (spec §96.1, §99, §240.3, §240.5, §259.6 …).
> Model two recoverers, two `--break-lock` takeovers, a stalled prior owner, and a plain run … and
> check that at most one operation ever owns a target.

Plans 1 to 3 did that. `recovery` runs two Recoverers, `breaklock` runs two Breakers, crashes model
the stalled owner, `PlainRuns` covers the plain run, and `SingleWriter` is the "at most one owner"
invariant. So at least one item is already satisfied, and a list that contains one such item probably
contains more.

**Designing work before establishing which work exists is the error this avoids.** Triage first; the
next plan is chosen from what survives.

## "Fix only what is already true" — what that licenses

Two different things share that phrase, and conflating them would let this grow into a work programme:

- **Satisfied by landed work.** The item asks for something that now exists. Verdict `DONE`, evidence
  is the landed work. Nothing is written.
- **True but unrecorded.** The item names a state the repository should be in, reaching it is a
  one-liner, and no judgement is involved — `.antigravityignore` being neither committed nor ignored is
  the example. The one-liner is applied, and the item is then `DONE` with that commit as its evidence.

Anything needing a decision, a test, a measurement run, or more than a single obvious edit is `LIVE`.
If it is arguable whether an edit is obvious, it is not obvious.

## Non-goals

- Starting any item. This produces verdicts, not features.
- Refactoring `TODO.md`'s structure. Sections stay as they are.
- Resolving open *decisions*. A decision nobody has taken is live however much surrounding work landed.

## Mechanism — two passes

### Pass one: candidates, cheap

Read all 49 items. Sort each into *closure candidate* or *stays open*. No evidence gathered, no
verdicts assigned. An item becomes a candidate if anything in the last three weeks plausibly satisfies
or obsoletes it.

**Bias toward marking candidates.** A false candidate costs one measurement in pass two. A missed one
stays open forever.

### Pass two: measurement, batched

For candidates only. Derive every check first, run them in as few calls as possible, and read all the
evidence **before** assigning any verdict.

The ordering is the point. Assigning a verdict and then looking for evidence produces evidence fitted
to the verdict; this session has already retracted one result reached that way. Deriving the checks up
front also forces the inputs to be named, which is where the session's discarded measurements went
wrong — a reused worktree, a wrong config file, a suppressed stderr, a detached HEAD.

### Why two passes

The evidence bar is asymmetric by decision: **measure to close, reason to keep.** Closing an item
wrongly loses real work; leaving one open costs nothing. Two passes make that asymmetry structural
rather than a discipline to remember, and confine the expensive work to where the error cost is.

## Verdict vocabulary

Closed set. Nothing sits "noted".

| verdict | means | evidence required |
|---|---|---|
| `DONE` | already satisfied by work that landed | a measurement: `file:line`, test name, commit, or artifact |
| `OVERTAKEN` | what it asks for no longer applies | a measurement of the thing that changed |
| `SPLIT` | really two items, one of which is `DONE` | measurement for the done half; the live half is rewritten as its own item |
| `LIVE` | still wanted | one-line reason, no proof |
| `UNVERIFIABLE` | cannot be settled either way | the reason it cannot, and it stays open |

`UNVERIFIABLE` exists so that "I could not check this" cannot quietly become `LIVE` by default, which
would hide that nobody knows. The closed set is the lesson from AGY-SCOPE: an open-ended vocabulary
lets items accumulate in a limbo that never resolves.

`SPLIT` is expected at least once. The model-check item above reads as satisfied, but it also carries
**"before implementing it"** — and whether that constrains Phase 2's start is a different question
from whether the modelling happened.

## Outputs

**Amended 2026-09-22, after three adversarial panel rounds. The original design is recorded below it,
because the reason for the change matters more than the change.**

Everything happens **in `TODO.md` itself**.

1. **A closed item moves to a `## Closed` section** at the bottom of the file, rewritten as one line
   carrying its verdict and the measurement that settled it:

   ```markdown
   ## Closed

   Triaged 2026-09-22. Kept here rather than deleted: the evidence for a closure belongs where the
   item was, not only in a commit message.

   - `DONE` **Model-check the lock protocol before implementing it.** `recovery` runs two Recoverers
     and `breaklock` two Breakers; `SingleWriter` is checked by 43 configs — `models/lockproto/expected.toml`.
   ```

2. **Nothing else.** No intermediate file, no generated commit message, no pointer line.

The **git diff is the durable record**: it shows exactly which item moved, what evidence came with it,
and nothing else. The commit message summarises; it does not carry the only copy.

### Why this replaced the original design

The original said closed items were **deleted** and their reasons lived **only in the commit message**,
with a pointer line in `TODO.md` naming that commit. Implementing it required keying items by line
number, a five-field TSV, a deletion script, a message generator, and a pointer line naming its own
commit.

Three panel rounds found 17 defects in that plan. **Every blocking one came from that machinery** — a
line-shifting cascade that would have destroyed un-evaluated items, a schema that disagreed with its
own instructions, a name used but never bound, a heredoc with no terminator, duplicate line numbers
driving an irreversible delete, and a pointer line that could never converge because a commit cannot
contain its own hash.

None of those defects are about triage. They are about moving 49 lines of Markdown across a data
pipeline in order to throw the lines away. The reviewer's verdict on the plan was that it "spends
hundreds of lines of fragile bash, awk, and Python ... purely to prevent a human from making a mistake
while checking boxes", and that is correct.

**Keeping the items in the file removes the pipeline and satisfies this spec's own acceptance bar
better than the original did.** The bar is that a closure be re-checkable from the repository alone;
evidence on the line, in a tracked file, meets that more directly than evidence in a commit message
that a reader must first know to look for.

The cost is that `TODO.md` stops being "surviving work only". That was a preference, not a
requirement, and it is worth less than the defects it generated.

## Edge cases

- **A decision, not a fact.** "Decide between `walkdir` or our own" can never be `DONE` by
  measurement, because nobody has decided. `LIVE` regardless of surrounding work. This guards against
  mistaking activity for resolution.
- **Evidence that ages out.** Prefer in-repo proof — a test name, `file:line`, a committed artifact —
  over a CI run id. A run id is unverifiable after 90 days of log retention, which is this branch's own
  structural finding; closing an item on one would repeat it.
- **Measurement turns up something new.** It does not get folded in. It goes to
  `.clavity/local-anomalies.md` and is named in the report. Scope stays closed.
- **A candidate turns out not to be done.** It becomes `LIVE`. That is the price of biasing pass one
  toward candidates.
- **Genuine ambiguity.** `UNVERIFIABLE`, stays open, surfaced to the owner rather than resolved
  unilaterally.

## Acceptance

Every `DONE` verdict must be re-checkable by someone else **from the repository alone** — no CI logs,
no memory of this session. A verdict that cannot meet that bar is not `DONE`; it is `UNVERIFIABLE`.

The triage is complete when all 49 items carry one of the five verdicts, `TODO.md` contains only
surviving work plus its pointer line, and the commit message carries the table.

## What comes after

The surviving list decides the next plan. On current reading the natural candidates are the lock-model
follow-ups (24 items, of which three share one re-measurement and five are CI-only seed mutants) or
the repository and CI hygiene group (7 items, small and independent). That choice is the owner's, and
it is made **after** the triage, not before — which is the whole point of doing this first.

## Stand-downs

From the adversarial panel, rounds 1 to 3 (2026-09-22). This discipline has no ledger file, so the
artifact carries its own record.

- `DISCARDED-BELOW-FLOOR`: `<scratch>` used as a literal path in the plan's shell blocks — an executor
  substitutes it, and the plan now defines it in its Conventions section.
- `DISCARDED-BELOW-FLOOR`: `gh api` assuming an authenticated executor. Guarded: the only items it
  touches are `UNVERIFIABLE` by construction anyway, so the assumption changes no verdict.
- `UNVERIFIED-ACCEPTED`: the check script's positive controls (for example `grep -c "MoveFileEx"`)
  were not confirmed by the reviewer against the real spec file, which it could not see. They are
  verified in the plan instead, against `origin/main`.
- **Process, recorded because it was mine:** round 2's do-not-re-raise ledger told the reviewer a
  clobber guard had been folded. It had not — the fix lived in a script that aborted before writing.
  The reviewer caught it in round 3. A ledger entry is a claim like any other and deserves the same
  measurement as a finding.
