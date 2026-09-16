# Fenced publication: a measured alternative to the accepted remote windows

**Status:** a design note, for the owner to accept or reject. Nothing here is built into the spec.
**Evidence:** `models/lockproto/Fenced.tla` on branch `model/fenced-prototype`, run in
[ckir/flux-model-scratch](https://github.com/ckir/flux-model-scratch) (throwaway). Every number below is a
TLC result, not an argument.
**Date:** 2026-09-16.

## 1. The problem this is about

Plan 3 measured the lock protocol under `LockCapability = remote`, where a lease can lapse under a running
holder. Three times in a row, a `-fixed` run produced a new way for two operations to be inside a publishing
step at once, and each time the answer was to accept another window:

| Measured | What happened | Ruling |
|---|---|---|
| CI 34963852420 | An Owner passes its Section 99 check, its lease lapses, a Breaker takes over and passes its own | the check-to-call window, accepted (`FIX_REMOTE_LEASE_SPEC`) |
| CI 35006174783 | A lease-lapsed Owner's record write lands after a takeover, restores its record, and its check passes on the record alone | Section 99 "still owned" must also test the lock |
| CI 35017494171 | A stalled Owner's release unlink lands after a takeover, deletes the Breaker's lock, and a fresh acquirer checks alongside the Breaker | accepted through tenure staleness |

Each ruling was sound on its own terms. The pattern is the problem: the protocol defends `SingleWriter`, a
property about the protocol's own internal state, while the destination - the thing a copy tool exists to get
right - is not modelled at all. A proxy cannot be defended on a filesystem whose locks are advisory, so the
window count grows.

## 2. What the prototype changes

`Fenced.tla` is the same protocol with three changes, all consequences of one idea: **the lock is advisory, and
the last atomic operation decides the outcome.**

1. **Ownership is the file you hold open.** "Still owned" means the lock path still names the very file this
   process has open, tested by identity. No record comparison (a record is descriptive, 99.1) and no
   OS-native lock test (a lease can lapse unheard, and on Windows the question is not even askable).
2. **A takeover creates a new file** and renames it over the lock, instead of overwriting the lock in place.
   The prior owner keeps its handle on the old file, so its in-flight calls land on a file no name reaches.
3. **Publication is one rename.** The output goes to a private file, which is renamed over the destination in
   a single atomic step, and the operation then re-checks before reporting success.

It also models the destination, which `LockProtocol.tla` does not, and two filesystem behaviours the main
model does not (Section 5).

## 3. The properties, and the result

Stated about the destination rather than about the protocol's internals:

- `TargetComplete` - the destination always holds one operation's complete output: never a mixture of two,
  never a file caught between the halves of a write, never an empty file.
- `PublishedIsOwn` / `NeverMisreported` - no operation is entitled to report success while the destination
  holds someone else's output.
- `SingleWriter` - the main model's proxy, measured here as a witness.

Measured (scratch repo, commit `97faf42`, POSIX, `LockCapability = remote`, an Owner and a Breaker, one crash,
one lease expiry - the setting the main model needs three accepted windows for):

```
OK  fenced-posix-check                    expected=-               observed=-               states=253,911  21s
OK  fenced-posix-witness-NeverPublished   expected=NeverPublished  observed=NeverPublished  states=5,864    3s
OK  fenced-posix-witness-SingleWriter     expected=SingleWriter    observed=SingleWriter    states=137,938  10s
```

**No invariant is violated, with no accepted window and no fix flag.** Coverage is complete for the pairing,
and the `NeverPublished` witness shows a publication is reached, so the two destination properties are not
vacuous. `SingleWriter` is violated, as predicted: two operations can be inside a publishing step while the
destination stays correct, which is the whole claim.

## 4. What the prototype does NOT buy

The measured limit matters more than the green run.

**A lease-lapsed operation's publication rename is not fenced.** Measured (scratch repo, 255,711 states): the
Owner's lease lapses, its ownership check passes (file identity cannot see a lapsed lease), a Breaker takes
over correctly, and the Owner's rename lands *after* the Breaker's. The destination then holds the stale
operation's output while the Breaker legitimately holds the lock.

The `EIO` rule of Section 5 does not help: it poisons writes through the descriptor whose lock was lost, and a
rename is a directory operation. No filesystem primitive closes this. **On a lease-based remote filesystem,
the lock winner's data is not guaranteed to survive.**

What can be done is refuse to lie about it, which is what the prototype does:

- the post-publication check **reads the destination back** and compares it, instead of only re-checking the
  lock;
- an operation whose output was replaced reports **`DESTINATION_REPLACED`**, a reason the spec does not have.
  Reporting `TARGET_LOCK_BUSY` there broke `RefusalJustified`, and rightly: the lock was never busy.

## 5. Filesystem facts this rests on

Verified from the platform documentation, not assumed:

- **A lock lost to an expired lease is not reclaimed** on Linux NFSv4, and `read`/`write` through that
  descriptor then fail with `EIO` until it is closed (kernel: `NFS_LOCK_LOST`, `nfs4_select_rw_stateid`). The module parameter `recover_lost_locks` re-enables
  reclaim and is documented as risking corruption.
  ([fcntl_locking(2)](https://man7.org/linux/man-pages/man2/fcntl_locking.2.html),
  [Red Hat](https://access.redhat.com/solutions/1179643))
- **`rename` is atomic to the client**, including over an existing target.
  ([RFC 7530](https://datatracker.ietf.org/doc/html/rfc7530))
- **`REMOVE` carries the directory handle and the NAME**, resolved when the server executes the call. A stale
  unlink therefore removes whatever holds the name at that moment. ([RFC 7530](https://datatracker.ietf.org/doc/html/rfc7530))
- **Windows cannot make the Section 99 lock test.** `LockFileEx` on a range the process already holds fails,
  and a second handle cannot touch it, so "do I still hold my lock?" is not answerable.
  ([LockFileEx](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-lockfileex))

Two of these were open questions in the first draft. Both were put to Gemini, ChatGPT and Grok on
2026-09-16, and the answers that survived my own verification are folded in here (Grok declined to answer
without an account):

- **A lease-lost lock cannot be re-taken on the same descriptor.** The descriptor is poisoned: the kernel
  keeps an `NFS_LOCK_LOST` state and returns `-EIO` for I/O through it, and re-acquiring with `F_SETLK` or
  `flock` on that same descriptor does not clear it. The documented recovery is to **close the file and open
  it again**. This refutes the optimistic branch in the prototype, where a lapsed lock is quietly taken back
  on the same handle, and it refutes the same assumption in `FIX_REMOTE_LEASE_SPEC`'s relock test in
  `LockProtocol.tla` (Section 8, decision 6).
- **SMB3 is not Linux here, and the difference favours SMB3.** When a durable handle's oplock or lease must
  be broken and the client is disconnected, `[MS-SMB2]` requires the server to **close the Open**, and the
  later durable-handle reconnect fails (`STATUS_OBJECT_NAME_NOT_FOUND`). A stale handle's buffered writes
  therefore cannot land. A handle that is still valid and connected, by contrast, keeps writing normally: a
  lease break is a caching mechanism, not a write fence. So the Linux `EIO` behaviour must **not** be used as
  a generic abstraction for SMB3.
  ([MS-SMB2 oplock break](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/2a09fc40-1615-42df-bda6-2865b8d6da95),
  [durable reconnect](https://learn.microsoft.com/en-us/openspecs/windows_protocols/ms-smb2/3309c3d1-3daf-4448-9faa-81d2d6aa3315))

**Reported but not verified here:** that on NFS a failed `rename()` does not prove the rename did not happen,
because a retransmitted request can be answered from the server's duplicate-request cache. If true it matters
for publication, and it is worth a probe of its own.

**Still unverified:** nothing load-bearing, but the prototype's own behaviour under a Windows pairing has not
been measured at all - every fenced run is POSIX.

## 6. A soundness gap in the CURRENT model, independent of this note

`LockProtocol.tla`'s `LandUnlink` has an in-flight unlink resolve the lock path when the call is **issued**,
and remove the file only if the path still names it. Per RFC 7530 the name is resolved when the server runs
the call. The model is therefore optimistic exactly where a stale delete is dangerous, and **all three
accepted remote windows were measured under that assumption.** The prototype models the pessimistic reading.

This is a finding against plan 3 whatever happens to the rest of this note.

## 7. What each spec section would say

| Section | Today | Under this note |
|---|---|---|
| 96.1 | the acquirer's lock is the record it wrote plus the OS-native lock | ownership is the open file, tested by identity against the lock path |
| 99 | "still owned" is a record comparison; a failed check reports `TARGET_LOCK_BUSY` | the check also reads the destination back; a replaced output reports `DESTINATION_REPLACED` |
| 235.1 | `RemoteStrong` is treated as giving target exclusivity | a lease-based lock gives a complete destination and honest reporting, not exclusivity |
| 240.5 | a takeover overwrites the lock record in place | a takeover renames a new file over the lock |

## 8. Decisions for the owner

1. **Adopt, reject, or park** the fenced design as the basis of a spec change.
2. **Independently of 1:** does Section 99 gain the read-back and `DESTINATION_REPLACED`? They are cheap, and
   the measured false-success case exists in the current protocol too.
3. **Independently of 1:** fix `LandUnlink` in `LockProtocol.tla` to the pessimistic reading, and re-measure
   the three accepted windows against it.
4. **`SingleWriter`'s future:** keep it as a gate property with its accepted windows, or demote it to a
   witness and gate on destination properties once the destination is modelled (the `nested` scenario).
5. **A probe** for the NFS duplicate-request-cache question in Section 5, and a Windows pairing for the
   prototype, before any of this reaches the spec.
6. **`FIX_REMOTE_LEASE_SPEC`'s relock test is now known to be wrong** (Section 5): a poisoned descriptor
   cannot take its lock back, so the fix flag's Section 99 test should fail the check outright rather than
   model a retake. This changes what the accepted windows were measured against, whatever is decided about
   the rest of this note.

## 9. How to reproduce

```bash
git checkout model/fenced-prototype
python models/lockproto/run.py --expected models/lockproto/expected-fenced.toml --scenario fenced --platform posix
```

The same command runs on every push in the scratch repository, which exists only because the main model's
workflow is a gate and cannot be dispatched from a branch.
