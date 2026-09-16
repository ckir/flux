---
name: verifying-platform-assumptions
description: Use when a model, spec, design or implementation depends on how an external system behaves - a filesystem, kernel, network protocol, database or service - and especially when two readings of that behaviour are both plausible and one of them makes your design look safer.
---

# Verifying platform assumptions

## Overview

A model checks your protocol against the platform **you described**, not the one your code will run on. A
wrong description buys a green run and nothing else. The same is true of a spec: it is only as sound as its
claims about the things it does not control.

**Core rule: every borrowed behaviour is named, and each one is either cited to a primary source or marked
unverified with a probe. No third option.**

## The failure this prevents

Measured, twice, with agents asked to model a lease-based lock (2026-09-16):

> "A lapsed lease clears the lock-holder entry but does **not** revoke the handle or block writes through it."
> "A non-blocking re-lock attempt on the same open handle succeeds if the lock is unheld... nothing about
> `self` having just lost the lock makes that acquisition special."

Both are false on Linux NFSv4. The kernel keeps an `NFS_LOCK_LOST` state, I/O through that descriptor fails
with `EIO`, re-locking the same descriptor does not clear it, and the documented recovery is to close the
file and open it again ([fcntl_locking(2)](https://man7.org/linux/man-pages/man2/fcntl_locking.2.html)).

Notice the shape: a correct mental model (advisory locks do not gate I/O) applied to a system where it does
not hold, stated with confidence, with no source, and the agent then argued *against* the correct behaviour
("if you guard it here, you've quietly designed the bug away"). It is fluent and it is wrong.

## The procedure

1. **Name the external system** and the exact call or state involved.
2. **List each borrowed behaviour as one row.** One row per claim, not one row per subsystem.
3. **Cite or mark.** A primary source is a specification, a manual page, the implementation's own source, or
   a probe you ran. Another model's answer, a blog post, or your own recollection is a lead to check, never a
   source.
4. **Check the self-serving ones first.** Of three such errors found in one project, every one was the
   reading that made the protocol look safer, and every one had already been built on by the time it surfaced.
5. **An unverified row names its probe** - the command or test that would settle it - or it is a guess, and
   the result that rests on it is provisional.

## Quick reference

| Claim | Source | State |
|---|---|---|
| `rename` over an existing target is atomic to a reader | RFC 7530 | verified |
| A stale `unlink` removes whatever holds the name when the server runs it | RFC 7530 (`REMOVE` carries the name) | verified |
| A lost lease poisons the descriptor; re-locking it does not help | `fcntl_locking(2)`, kernel `NFS_LOCK_LOST` | verified |
| A failed `rename` proves the rename did not happen | - | unverified; probe: retransmit against a server with a duplicate-request cache |

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

**When a third exception is accepted to the same invariant, the invariant is the problem.** Two accepted
windows is a protocol with caveats; three means you are defending a proxy for the property you actually care
about. Model the real property and see whether the exceptions survive.
