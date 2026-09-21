---
name: verifying-platform-assumptions
description: Use when a spec, design, model or protocol WRITES DOWN how an external system behaves - a filesystem, kernel, network protocol, database or service - so that later work rests on that claim. Especially when you feel no doubt about the claim, or when two readings are both plausible and one of them makes your design look safer. Also use it when a third exception is being accepted to the same invariant.
---

# Verifying platform assumptions

## Overview

A model checks your protocol against the platform **you described**, not the one your code will run on. A
wrong description buys a green run and nothing else. The same is true of a spec: it is only as sound as its
claims about the things it does not control.

**Core rule: every borrowed behaviour is named, and each one is in exactly one of three states — `verified`,
`refuted`, or `unverified`.**

Each state costs something different, and saying which is the point of having them:

- `verified` — settled. Build on it.
- `refuted` — the design resting on it is unsupported. Stop and say so.
- `unverified` — **every result resting on this row is provisional**, including a model run that passes.
  Provisional is a real state you may proceed from, as long as it is written where the result is read.
- *no state at all* — not a fourth state. The rule is unmet, which is weaker than provisional: you do not
  yet know what you are resting on, so you cannot say what it costs.

`verified` is the one with a trap in it. A row is verified only when the cited source covers **this** claim -
not a neighbouring one on the same page. A citation is not a verification, and the worked example below is
this page failing exactly that way.

## The failure this prevents

Measured, twice, with agents asked to model a lease-based lock (2026-09-16):

> "A lapsed lease clears the lock-holder entry but does **not** revoke the handle or block writes through it."
> "A non-blocking re-lock attempt on the same open handle succeeds if the lock is unheld... nothing about
> `self` having just lost the lock makes that acquisition special."

Both are false on Linux NFSv4. [`fcntl_locking(2)`](https://man7.org/linux/man-pages/man2/fcntl_locking.2.html)
(retrieved 2026-09-21) says it directly:

> "When the filesystem determines that a lock has been lost, future read(2) or write(2) requests may fail
> with the error EIO. This error will persist until the lock is removed or the file descriptor is closed."

and, since Linux 3.12, I/O by a process that "thinks" it holds the lock fails "until that process closes and
reopens the file". So the descriptor is poisoned and re-locking it is not the recovery; reopening is.

An earlier draft of this page named the kernel's internal `NFS_LOCK_LOST` state here and cited that same man
page for it. **The man page does not mention that flag** — the citation covered the `EIO` behaviour and not
the claim attached to it. It was caught by fetching the link during this page's own review, which is the
cheapest probe there is and the one nobody runs.

Notice the shape: a correct mental model (advisory locks do not gate I/O) applied to a system where it does
not hold, stated with confidence, with no source, and the agent then argued *against* the correct behaviour
("if you guard it here, you've quietly designed the bug away"). It is fluent and it is wrong.

## The procedure

0. **Check which door you came in by.** If you are here because a third exception is being accepted to the
   same invariant, there is no external system to name: go straight to **The companion rule** at the end.
   The procedure below is for a borrowed behaviour.
1. **Name the external system** and the exact call or state involved.
2. **List each borrowed behaviour as one row, in the artifact that depends on it** - the design document,
   the model's own comments, the commit message. One row per claim, not one row per subsystem. A list that
   exists only in your reasoning is not a list: nobody can check it later, and later is when it matters.
3. **Cite, refute, or mark.** A primary source is a specification, a manual page, the implementation's own
   source, or **a probe you actually ran** - not one you named. Another model's answer, a blog post, or your
   own recollection is a lead to check, never a source.
4. **Quote the words, not just the reference.** This is the one guard that works without a second reader.
   A reference is a promise that a source says something; the sentence itself is checkable on sight, and a
   claim that has drifted from its quote is visible to anyone reading the row. It would have caught this
   page's own failure below with no fetch at all. Typing a reference because a blog post mentioned it is
   how a lead crosses into the Source column, and a quote is what that cannot survive.
5. **Record what makes it re-checkable, and what would end it.** A page that can change needs the date you
   read it; a long document needs the section, not just its number; a probe needs its command and what it
   printed. A probe you ran and did not record is indistinguishable later from one you imagined - without
   this the rule for unverified rows is stricter than the rule for verified ones, which is backwards.
   **And say what would put the row back to `unverified`**: the platform version it was true of, the
   paragraph that could be revised. A date nothing ever reads is filing, not verification - the row it
   dates goes stale in place and still reads `verified`.
6. **A refuted row is a result, not a failure.** If the source or the probe says the platform does NOT behave
   as assumed, say so in the row and follow what it costs: the design resting on it is now unsupported, and
   that is the whole return on doing this.
7. **Check the self-serving ones first.** Of three such errors found in one project, every one was the
   reading that made the protocol look safer, and every one had already been built on by the time it surfaced.
8. **An unverified row names the probe that would settle it** - concrete enough that someone else could
   attempt it without asking you what you meant. "Write a test for it" is not a probe; a command, a named
   tool, or a setup someone could build is. Naming a probe does not verify anything; it records what is
   owed, and a vague one records nothing.
9. **Say how many rows there are, and how you know that is all of them.** A list of zero borrowed behaviours
   satisfies every rule above and proves nothing - the commonest way this comes out green is by naming
   nothing. If the count is low, say which calls you went looking through to get it.

## Quick reference

The Source column carries the words, not just the reference (step 4). Note what that costs: two rows below
are **not** verified, because writing this table is what showed that nobody here had ever read the sentence.

| Claim | Source, and the words that say so | State |
|---|---|---|
| A lost lease poisons the descriptor; re-locking it does not help | `fcntl_locking(2)`, "Lost locks", retrieved 2026-09-21: *"future read(2) or write(2) requests may fail with the error EIO. This error will persist until the lock is removed or the file descriptor is closed."* | verified; ends if the page is revised for a kernel past 3.12 |
| *(assumed)* Re-locking a descriptor whose lease lapsed recovers the lock | same sentence - it says the error persists until the descriptor is closed, which is the opposite | **refuted** - the design resting on it was rebuilt |
| `rename` over an existing target is atomic to a reader | RFC 7530 §16.27 (RENAME). **Quote owed** - the section is located, the sentence is not transcribed | unverified; probe: read §16.27 and paste the sentence, or say it is not there |
| A stale `unlink` removes whatever holds the name when the server runs it | RFC 7530 §16.26 (REMOVE). **Quote owed**, same reason | unverified; probe: read §16.26 and paste the sentence |
| A failed `rename` proves the rename did not happen | - | unverified; probe: retransmit against a server with a duplicate-request cache |

**5 rows, and here is why that is all of them:** they are every behaviour this protocol borrows from the
filesystem - the four calls it makes at the lock path (`rename`, `unlink`, the lock acquisition, the
re-lock after a lapse) and the lease that underlies them. Walked by re-reading each filesystem call in the
model and asking what it assumes. Step 9 without this line is the part everyone skips, which is why the
example carries it.

Two rows moved from `verified` to `unverified` the moment the quote was asked for. They had said "RFC 7530"
for months and nobody could have told from the table whether anyone had opened it. That is the rule earning
its place, and it is also the honest state of this list: the sections are right, the sentences are owed.

## Red flags

- "Obviously it just..." about somebody else's software.
- A behaviour chosen because the other reading would be inconvenient to model.
- A platform claim in a design document with no link.
- The same rule applied to two platforms at once ("POSIX and Windows both...") without checking the second.
- An agent or colleague warning you *off* the stricter behaviour. Ask them for the citation.

## Rationalizations

| Excuse | Reality |
|---|---|
| "That's the premise we already built" | The premise is a claim about the platform. It gets a citation like any other. |
| "Guarding it would design the bug away" | Only if the guard is imaginary. If the platform really does fail that call, modelling success invents safety. |
| "Advisory locks never gate I/O" | True locally. False for a lease the client believes it lost. Cite the one you mean. |
| "No primitive exists for that" | Often right, and worth one link. Absence claims are the easiest to check and the most load-bearing. |
| "It's a detail, the protocol is what matters" | Every measured error of this kind had already changed a decision before it surfaced. |

## The companion rule

This one is here rather than in its own page because it comes from the same place: an invariant is a claim
about a system, and the exceptions are what you pay when the claim is not quite the one you meant. The
frontmatter routes here for it, so it is reachable on its own.

**When a third exception is accepted to the same invariant, the invariant is the problem.** Two accepted
windows is a protocol with caveats; three means you are defending a proxy for the property you actually care
about. Model the real property and see whether the exceptions survive.

## Stand-downs

Findings this page's own adversarial panel raised and stood down, recorded here because the panel
produces no other durable record (round 1, 2026-09-21).

- `DISCARDED-BELOW-FLOOR`: "Absence claims are the easiest to check" (Rationalizations) overstates -
  proving a negative can be hard. Unreachable as a defect: the row is a rhetorical answer to an excuse,
  not a step any reader executes, and nothing in **The procedure** branches on it.
- `DISCARDED-BELOW-FLOOR`: the man-page citation points at man7.org, a mirror, rather than at the Linux
  man-pages project itself. Guarded by the retrieval date beside it, which is what makes a mirror
  checkable - and step 4 now asks that of every row, so the guard is a rule rather than an accident of
  this one example.
- `REJECTED` (round 3): that the core rule grants a rule-violating row "provisional" status. Measured -
  the reviewer quoted the round-1 wording, while the file it was sent says a row in no state is "weaker
  than provisional", which is the opposite. Superseded text, and the point was already in the
  do-not-re-raise ledger it had been given.
