# Sequencing after cut 5: the operation workspace, model debt first

**Status:** approved by the owner 2026-09-26 (brainstorming; AGY-FIRST consult aligned after one
negotiation round). **Base:** `main` at `878af1f` (PR #53, cut 5, merged).

## The decision

The next five cuts, in order. Each cut gets its own spec, plan and execution. **Re-run this decision after
cut 8 merges**, rather than fixing the later order now.

| Cut | Delivers | Closes or unblocks | Depends on |
|---|---|---|---|
| **6** | Lock-model debt that weakens what the model proves about behaviour the Rust lock will implement (the 11 items listed below) | those 11 `TODO.md` items | nothing |
| **7** | The destination lock: §96.1, §99 commit-time revalidation, and §240 staleness and liveness including `--break-lock`. Also the workspace (`DEST/.flux/operations/<id>/`, spec:1378-1391), a versioned manifest (§19), a persisted operation ID, and `--restart` (§21.1). A prior operation's state is handled by §21.1's classification (spec:1595-1640): a live owner is `OPERATION_LOCKED` or `TARGET_LOCK_BUSY`; uncertain ownership or corrupt state is preserved; completed or ABANDONED lets the new run proceed; a resumable one fails `RESUMABLE_OPERATION_EXISTS` without mutating anything, unless `--restart` supersedes it. `--resume` is cut 9's. | "A leftover temporary from a PREVIOUS run is never removed"; "A blocking pre-existing temporary is not reported" (the id makes it live); "WSL 9p mounts break two lock assumptions" (the lock must handle it or refuse); the manifest half of "Persistent state format" | cut 6 |
| **8** | `state.db`, with its technology chosen in this cut next to its first consumer; the §241.5 claim records | "A tree copy never replaces an existing destination file"; "Item 113's up-front no-replace probe is not built"; the `state.db` half of "Persistent state format"; the state-DB part of "Final dependency selection" | cut 7 |
| **9** | Resume (§21), GC (§24.3), completed-operation cleanup (§218), stale-operation management | Phase 5's crash recovery and cleanup | cut 8 |
| **10** | Several sources (§18.3: one workspace, the manifest maps each source root) | "Several sources are a usage error"; the multiple-sources part of "Integration tests" | cut 7 (workspace); after 9 so a multi-source run is resumable like any other |

**Why this order.** The owner ruled the priority is **unblock the most first**, with the lock-model
follow-ups in scope. The operation workspace (spec §81 Phase 5, spec:3714-3727) is what the most open items
wait on. Directory-operation discovery starts from the destination root's lock, whose record names the
workspace (spec:5745-5753), so the lock and the workspace ship together (cut 7). Replacement needs durable
claim records in `state.db` (spec:10816-10828), so overwrite waits for cut 8.

**Why the model debt goes first.** The Rust lock will be checked against the TLA+ model in
`models/lockproto/`. An item that leaves one of the model's guarantees about the lock indistinguishable from
`TRUE`, or blind to a mutation, means the implementation would be verified against a model with a known
hole. `TODO.md` says those re-measurements should be batched, because one scenario once took ~2.5h, so cut 6
takes all 11 at once.

**Why the state-DB technology is chosen in cut 8, not before.** The spec names no technology; the format is
"implementation-defined but must be versioned" (spec:5717-5718). Choosing it next to the claim workload,
its first real consumer, lets that workload refute a bad choice before anything depends on it.

**Why cut 7 need not implement cleanup to be safe.** Dead-owner recovery is part of the lock protocol, not of
cleanup: §240's recovery decision order, abandoned owner, uncertain ownership and `--break-lock` (spec:10528,
:10535, :10569, :10613, :10640). A crashed run's lock is therefore recoverable from cut 7 on; what waits for
cut 9 is reclaiming the disk its workspace used. Resume is always an explicit operator command
(spec:13603-13604), so omitting it in cut 7 leaves no path that silently resumes.

**Every cut that adds durable state bumps the workspace format version** (the spec requires it versioned, spec:5717-5718), and a later binary that meets an earlier version classifies it under §21.1 - never misparses it. A crash under a cut-7 binary must leave a workspace a cut-8 or cut-9 binary handles.

**Cut 7's size is decided in its own spec** (owner). agy proposed a seam if it is too large: 7a the lock, an
empty workspace and `--restart`; 7b the versioned manifest. That seam writes no durable state except the
modeled lock record.

## Cut 6's scope - the lock-model follow-ups

**In cut 6 (they weaken what the model proves about the lock):**

- M5, `NeverTornRead` is placement-blind
- M6, `recoveredAfterCrash`'s guard is always true
- M7, host-crash net holes
- M8, the liveness consequent can be weakened undetectably
- `tornRead` is set at one of the two labels that read a record
- `FsOk`'s other two conjuncts are still indistinguishable from `TRUE`
- `Classifiable`'s seed pins the seed
- Two of `RefusalJustified`'s three `lostLock` sites remain unseeded
- `PlainNeverOwnsUncertain`'s ghost is wired to one label
- `ForeignUntouched`'s content half is accepted as unreachable by reasoning
- `S240_5_s6`'s two refusing branches are indistinguishable to every gate

**Model hygiene, after this sequence (they do not change what the model proves about the lock):**
"Cache the TLC jar in CI", "`--no-renames` is unpinned", "`open_findings[].tracking` is not validated",
"cp1252 stdout", "`pendingUnlink` could be a boolean", "`IdentityStrength = "weak"` is dead across every
run", "Build the BRANCH-granular coverage union", "Say which `never_reached` claims an exhaustive run
supports", "Commit the union verdict as an artifact", "The stamp cannot see an acceptance test", "The model
cites RFC 7530 for a sentence it does not contain", "A halting run's state count is not reproducible",
"`trace.toml`'s exemption for family 259 states the wrong reason", "Make the SEED-flag complement a test".

The 11 + 14 are all 25 open items of the two "Lock-model follow-ups" sections of `TODO.md` at `878af1f`
(counted mechanically).

## Everything else in `TODO.md` - after this sequence

**Catch-all: every open `TODO.md` item not placed above is decided when this decision is re-run after cut 8.**
None of them waits on the workspace, and none makes a cut above wrong if it stays open. For the record:

- *Copy-path limits:* statistics collection; the rest of "Integration tests"; `destination_is_write_protected`
  swallows errors; `Preserve::Off`; the read-only guard cannot be atomic; the Windows guard asks "may I
  delete"; handle and path `rename_replace` disagree on read-attributes; no test pins the bare-name
  leftover's spelling; `bytes_total` counts copied bytes only; `object_type=special`; the dangling
  destination link is exit 3 on Linux only.
- *Features outside Phase 5:* symlink recreation (§25); the §52 progress UI; §42 mount boundaries.
- *Engine and walker:* `FaultFs::move_object` strands a renamed directory's children (needed by the first cut
  that renames a directory - revisit if cut 7 or 8 stages a directory); a transient failure and an
  unsupported filesystem both report `Unavailable`; identity-based cycle detection on weak-identity
  filesystems; the walker TOCTOU; the Windows file identity source (a spec gap).
- *Open decisions:* the rest of "Final dependency selection"; repository housekeeping.
- *Scaffolding and CI:* the four scaffolding follow-ups; the five "Repository and CI hygiene" items.

Two notes for the re-run: symlink recreation was proposed for early placement and rejected here because it
needs a link-creating primitive and, on Windows, a privilege or Developer Mode, and closes one item; §52
progress needs the walk to know totals up front.

## Consult record

AGY-FIRST (brief `.clavity/seams/seq-after-cut5.md`). agy answered `NEGOTIATE`: it held that option A
("foundation first", with recovery and cleanup in a later cut) would leave a crashed run's lock
"bricking" the destination, reframed the measure as "risk retired", proposed choosing the state-DB
technology together with its first consumer, and proposed symlink recreation as a cheap early cut.

The driver measured: §240 puts dead-owner recovery inside the lock protocol (spec:10528-10640), so the
bricking mechanism is overstated; symlink recreation is not cheap (Windows privilege, `TODO.md`); M7 is holes
in the host-crash net, not total blindness. Adopted: the state-DB timing, and model debt before the Rust lock.

AGY-NEGOTIATE round 1 (brief `.clavity/seams/seq-after-cut5-neg1.md`): agy withdrew the bricking objection,
confirmed "refuse or restart, never resume" is safe in cut 7 (spec:13603-13604; `--restart` keeps the target
locked throughout, spec item 103 at :359), supplied the 11/14 triage and the cut-7 seam. `[VERDICT: ALIGNED]`.
The owner approved the sequence, all 11 items in cut 6, and deciding cut 7's size in its own spec.
