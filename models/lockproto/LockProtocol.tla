---- MODULE LockProtocol ----
\* The V16 lock protocol: acquisition (96.1), classification and recovery (240.1 to 240.3), a plain
\* rerun's decision (21.1), cleanup (251.1), and commit-time revalidation (99), on the abstract
\* filesystem of FsModel.tla. Design: docs/superpowers/specs/2026-09-11-lock-protocol-model-check-design.md,
\* Sections 6.1, 7 and 12. Each label is named after the spec step it implements, and each performs
\* at most one filesystem operation, so other actors run in between (design Section 5.2).
EXTENDS Naturals, FiniteSets, Sequences, TLC

CONSTANTS
    Owners,        \* operations that acquire the lock and publish
    Recoverers,    \* new invocations that recover a dead owner's lock (240.3)
    PlainRuns,     \* new invocations with no flags (21.1)
    Cleanups,      \* flux cleanup DEST (251.1)
    NoProc,        \* model value: nobody
    P,             \* the target's parent directory
    T,             \* the target's name
    LockName,      \* P/<name>.flux-lock (96.1)
    DirLockName,   \* P/.flux-dir.lock, the long-name fallback (96.1); never created here
    MaxObjs,       \* how many objects this scenario's actors can create (bounds the state space)
    Platform, IdentityStrength, LockCapability

Procs == Owners \cup Recoverers \cup PlainRuns \cup Cleanups
\* <lock-name>.broken.<operation-id> beside the lock (240.3 step 2)
BrokenOf(p) == <<"broken", p>>
Names == {T, LockName, DirLockName} \cup {BrokenOf(p) : p \in Procs}
Dirs == {P}
Fold == [n \in Names |-> n]          \* the recovery scenario needs no name folding

INSTANCE FsModel WITH Dirs <- Dirs, Names <- Names, Procs <- Procs, NoProc <- NoProc,
                      MaxObjs <- MaxObjs, Fold <- Fold, Platform <- Platform,
                      IdentityStrength <- IdentityStrength, LockCapability <- LockCapability

\* How a classifier judged the lock path (design Section 6.1).
RecovererPerms == Permutations(Recoverers)
Judgements == {"none", "empty", "foreign", "uncertain", "cleanuplock", "live", "dead"}

(* --algorithm LockProtocol {
     \* Each label performs at most ONE filesystem operation (design Section 5.2). A purely local
     \* decision that follows a call - a judgement over what was just read, a branch on whether the
     \* call succeeded - belongs to the same label: no other actor can observe the state between
     \* them, so giving it a label of its own would only add interleavings (Lipton reduction).
     variables
       fs = FsInit,                              \* the filesystem (FsModel.tla)
       classified = [p \in Procs |-> "none"],     \* ghost: each process's last judgement
       ownerLive = [p \in Procs |-> "none"],      \* what it decided about the record's owner
       sawLive = [p \in Procs |-> FALSE],         \* its classification found a live owner
       lastRecord = EmptyFile,                    \* ghost: the content most recently at the lock path
       crashed = [p \in Procs |-> FALSE],         \* ghost: this process died
       holding = [p \in Procs |-> FALSE],         \* this process owns the target lock now
       checked = [p \in Procs |-> FALSE],         \* its last Section 99 check passed
       recoveredAfterCrash = FALSE,               \* witness: a lock left by a crash was replaced
       tornRead = FALSE,                          \* witness: a process read a torn record
       touchedUncertain = FALSE,                  \* a lock judged uncertain was mutated
       touchedForeign = FALSE,                    \* a Foreign object was mutated
       refusedOk = [p \in Procs |-> FALSE],       \* the refusal below was justified
       refused = [p \in Procs |-> "none"];        \* what a refusing actor reported

     define {
       LockObj == At(fs, P, LockName)
       OwnRecord(p) == Rec(p, IF p \in Cleanups THEN "cleanup" ELSE "operation")
       \* "Still owned" (Section 99): a lock file at the lock path holding this operation's record.
       StillOwned(p) == LockObj # NoObj /\ fs.content[LockObj] = OwnRecord(p)
       \* The lock path holds a record whose owner is alive: what justifies TARGET_LOCK_BUSY
       \* (Section 7, 240.2). A takeover's record is the `breaklock` scenario's business.
       BusyJustified == /\ LockObj # NoObj
                        /\ IsRecord(fs.content[LockObj])
                        /\ ~crashed[fs.content[LockObj].op]
       ForeignAtLock == LockObj # NoObj /\ fs.content[LockObj] = Foreign
       \* A lock this actor may replace through 240.3: a dead owner's operation lock, or a cleanup
       \* lock whose owner is dead (251.1, 259.6).
       Replaceable(p) == \/ classified[p] = "dead"
                         \/ (classified[p] = "cleanuplock" /\ ownerLive[p] = "dead")
       \* A lock whose owner is dead, or uncertain, sits at the lock path: the states the liveness
       \* properties are about, and the two state witnesses (Section 7).
       DeadOwnerLock == /\ LockObj # NoObj
                        /\ IsRecord(fs.content[LockObj])
                        /\ crashed[fs.content[LockObj].op]
       TornLock == LockObj # NoObj /\ fs.content[LockObj] = Torn
     }

     \* Classify what is at the lock path: open it, try its OS-native lock without waiting, read the
     \* record, then judge (design Section 6.1). `keep` says whether the caller keeps the handle and
     \* the lock it may have taken, as 240.3 does through its move-aside.
     procedure Classify(keep)
       variables obj = 0, got = FALSE;
     {
       S240_1_open:
         \* No entry at the lock path: the open fails and the judgement is "empty" (96.1).
         with (r = FsOpen(fs, P, LockName, self, TRUE)) {
           if (r.ok) { fs := r.fs; obj := r.val; }
           else { classified[self] := "empty"; return; };
         };
       S240_1_trylock:
         with (r = FsTryLock(fs, self, obj)) {
           got := r.ok;
           if (r.ok) { fs := r.fs; };
         };
       S240_1_read:
         \* Read the record, then judge what was read (Section 6.1's table). A torn or unreadable
         \* record cannot show a dead owner: uncertain (96.1, 240.4, 259.6).
         with (seen = fs.content[obj]) {
           if (seen = Torn) { tornRead := TRUE; };
           if (seen = Torn \/ seen = EmptyFile) {
             classified[self] := "uncertain";
             ownerLive[self] := "none";
             sawLive[self] := FALSE;
           } else if (seen = Foreign) {
             classified[self] := "foreign";
             ownerLive[self] := "none";
             sawLive[self] := FALSE;
           } else {
             \* The OS-native lock proves the owner is alive (240.2); otherwise the oracle decides
             \* what no filesystem fact can (Section 6.1), consistent with the truth and free to say
             \* "uncertain" either way.
             with (alive \in IF ~got /\ LockCapability = "strong" THEN {"live"}
                            ELSE IF crashed[seen.op] THEN {"dead", "uncertain"}
                            ELSE {"live", "uncertain"}) {
               ownerLive[self] := alive;
               sawLive[self] := alive = "live";
               \* A cleanup lock is its own case (251.1); an operation's lock takes its judgement
               \* from its owner (240.1 to 240.4).
               if (seen.kind = "cleanup") { classified[self] := "cleanuplock"; }
               else { classified[self] := alive; };
             };
           };
         };
       S240_1_unlock:
         \* Unless the caller keeps the lock through its next steps, the classifier gives it back at
         \* once, so a refusal never leaves a lock held on another process's file (Section 6.1).
         if (got /\ (~keep \/ ~Replaceable(self))) { fs := FsUnlock(fs, self, obj).fs; };
       S240_1_close:
         if (~keep \/ ~Replaceable(self)) { fs := FsClose(fs, self, obj).fs; };
         return;
     }

     \* Acquire the target lock (96.1): create it exclusively, check the directory lock is absent,
     \* take its OS-native lock, then write this operation's record.
     procedure Acquire()
       variables obj = 0;
     {
       S96_1_create:
         \* Another operation created the lock first: start the acquisition again (21.1 step 1).
         with (r = FsCreate(fs, P, LockName, self, TRUE)) {
           if (r.ok) { fs := r.fs; obj := r.val; }
           else { refused[self] := "RESTART"; return; };
         };
       S96_1_dircheck:
         \* The per-name acquirer announces, then checks (96.1). The directory lock never exists in
         \* this scenario, so the conflict below is the `dirlock` scenario's business.
         if (FsLookup(fs, P, DirLockName)) { goto S96_1_backoff; };
       S96_1_ownlock:
         with (r = FsTryLock(fs, self, obj)) {
           if (r.ok) { fs := r.fs; };
         };
       S96_1_record_begin:
         fs := FsWriteBegin(fs, obj).fs;
       S96_1_record_end:
         fs := FsWriteEnd(fs, obj, OwnRecord(self)).fs;
         lastRecord := OwnRecord(self);
         holding[self] := TRUE;
         return;
       S96_1_backoff:
         \* On a conflict the acquirer removes what it created and reports TARGET_LOCK_BUSY (96.1).
         refusedOk[self] := BusyJustified \/ sawLive[self];
         with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
         refused[self] := "TARGET_LOCK_BUSY";
         return;
     }

     \* Replace a dead owner's lock by moving it aside (240.3 steps 1 to 5). The caller has
     \* classified the lock as replaceable and still holds its handle and its OS-native lock.
     procedure Recover()
       variables robj = 0, victim = NoProc;
     {
       S240_3_s1:
         \* Re-read the lock: proceed only if it still names the same dead owner.
         robj := LockObj;
         if (IsRecord(lastRecord)) { victim := lastRecord.op; };
         if (LockObj = NoObj \/ fs.content[LockObj] # lastRecord) { goto S240_3_restart; };
       S240_3_s2:
         \* Only one of several concurrent recoverers can move it; the others find the lock gone and
         \* start the acquisition again (240.3 step 2).
         if (classified[self] = "uncertain") { touchedUncertain := TRUE; };
         if (ForeignAtLock) { touchedForeign := TRUE; };
         with (r = FsRenameNoReplace(fs, P, LockName, BrokenOf(self))) {
           if (r.ok) { fs := r.fs; } else { goto S240_3_restart; };
         };
       S240_3_s3:
         \* Check the moved file is the one step 1 re-read, by identity AND by its record: a takeover
         \* rewrites the record in the same file, so identity alone is not enough (240.3 step 3).
         with (ident \in FsIdentityChoices(fs, P, BrokenOf(self))) {
           if (ident # robj \/ fs.content[robj] # lastRecord) { goto S240_3_putback; };
         };
       S240_3_s4:
         \* Create its own lock exclusively (step 4). If that fails, another operation owns the
         \* target: delete the moved file, whose owner is dead, and start again.
         with (r = FsCreate(fs, P, LockName, self, TRUE)) {
           if (r.ok) { fs := r.fs; } else { goto S240_3_s4_drop; };
         };
       S240_3_s4_lock:
         with (r = FsTryLock(fs, self, LockObj)) {
           if (r.ok) { fs := r.fs; };
         };
       S240_3_s4_record_begin:
         fs := FsWriteBegin(fs, LockObj).fs;
       S240_3_s4_record_end:
         fs := FsWriteEnd(fs, LockObj, OwnRecord(self)).fs;
         lastRecord := OwnRecord(self);
         holding[self] := TRUE;
       S240_3_s5:
         \* Delete the moved file (step 5).
         with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, BrokenOf(self), c).fs; };
         if (victim \in Procs) { if (crashed[victim]) { recoveredAfterCrash := TRUE; }; };
         goto S240_3_release;
       S240_3_s4_drop:
         with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, BrokenOf(self), c).fs; };
         refused[self] := "RESTART";
         goto S240_3_release;
       S240_3_putback:
         \* Rename the file back without replacing, then start the acquisition again (step 3).
         with (r = FsRenameNoReplace(fs, P, BrokenOf(self), LockName)) {
           if (r.ok) { fs := r.fs; };
         };
         refused[self] := "RESTART";
         goto S240_3_release;
       S240_3_restart:
         \* Start the acquisition again (21.1 step 1). This model stops here instead of looping: one
         \* pass reaches every state a further one would, and the bound is in the README.
         refused[self] := "RESTART";
       S240_3_release:
         if (robj # NoObj /\ OpenBy(fs, self, robj)) { fs := FsClose(fs, self, robj).fs; };
         return;
     }

     \* Publish under the lock, revalidating before the write (Section 99), then release it.
     procedure Publish()
     {
       S99_check:
         \* "Still owned" is a read of the lock path; the decision that follows is local.
         if (StillOwned(self)) { checked[self] := TRUE; }
         else {
           checked[self] := FALSE;
           refusedOk[self] := BusyJustified \/ sawLive[self];
           refused[self] := "TARGET_LOCK_BUSY";
           holding[self] := FALSE;
           return;
         };
       S99_write:
         \* The write itself: a separate label, because the spec's check must come immediately before
         \* it and another actor can act in between (design Section 11's check-to-call window). What
         \* it writes is not part of the lock protocol here, so it sets a ghost rather than creating
         \* an object: the `nested` scenario, which is about what gets written where, creates it.
         skip;
       S99_release:
         checked[self] := FALSE;
         holding[self] := FALSE;
         with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
       S99_close:
         if (LockObj # NoObj /\ OpenBy(fs, self, LockObj)) { fs := FsClose(fs, self, LockObj).fs; };
         return;
     }

     \* A normal operation: acquire the lock, publish, release.
     process (own \in Owners)
     {
       own_start:
         call Acquire();
       own_publish:
         if (holding[self]) { call Publish(); };
     }

     \* A new invocation with no flags (21.1): classify what it finds, then act or refuse.
     process (plain \in PlainRuns)
     {
       plain_start:
         call Classify(FALSE);
       S21_1_decide:
         refusedOk[self] := BusyJustified \/ sawLive[self];
         if (classified[self] = "empty") { goto plain_acquire; }
         else if (Replaceable(self) /\ classified[self] = "cleanuplock") {
           \* A cleanup lock names no resumable operation (259.6), so a dead one is removed as 240.3
           \* describes and this invocation proceeds (21.1).
           goto plain_recover;
         }
         else if (classified[self] = "live") { refused[self] := "TARGET_LOCK_BUSY"; }
         else if (classified[self] = "uncertain" \/ ownerLive[self] = "uncertain") {
           refused[self] := "TARGET_LOCK_UNCERTAIN";
         }
         else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
         else if (classified[self] = "cleanuplock") { refused[self] := "TARGET_LOCK_BUSY"; }
         else {
           \* A resumable prior operation, with neither --resume nor --restart: refuse without
           \* mutating anything (21.1).
           refused[self] := "RESUMABLE_OPERATION_EXISTS";
         };
       S21_1_refused:
         goto Done;
       plain_recover:
         call Recover();
       plain_recovered:
         if (holding[self]) { call Publish(); };
       plain_recovered_done:
         goto Done;
       plain_acquire:
         call Acquire();
       plain_publish:
         if (holding[self]) { call Publish(); };
     }

     \* A new invocation that finds a dead owner's lock, recovers it (240.3), then continues as an
     \* owner: the recovery path of 21.1 step 1.
     process (rec \in Recoverers)
     {
       rec_start:
         call Classify(TRUE);
       rec_decide:
         refusedOk[self] := BusyJustified \/ sawLive[self];
         if (Replaceable(self)) { goto rec_recover; }
         else if (classified[self] = "empty") { goto rec_acquire; }
         else if (classified[self] = "uncertain" \/ ownerLive[self] = "uncertain") {
           refused[self] := "TARGET_LOCK_UNCERTAIN";
         }
         else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
         else { refused[self] := "TARGET_LOCK_BUSY"; };
       rec_refused:
         goto Done;
       rec_recover:
         call Recover();
       rec_publish:
         if (holding[self]) { call Publish(); };
       rec_publish_done:
         goto Done;
       rec_acquire:
         call Acquire();
       rec_acquired:
         if (holding[self]) { call Publish(); };
     }

     \* flux cleanup DEST: classify the lock, remove a dead owner's or a dead cleanup lock through
     \* 240.3 under its own cleanup lock, then delete that lock last (251.1, 259.6).
     process (clean \in Cleanups)
     {
       clean_start:
         call Classify(TRUE);
       S251_1_classify:
         refusedOk[self] := BusyJustified \/ sawLive[self];
         if (Replaceable(self)) { goto clean_recover; }
         else if (classified[self] = "empty") { refused[self] := "NOTHING_TO_CLEAN"; }
         else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
         else if (classified[self] = "uncertain" \/ ownerLive[self] = "uncertain") {
           \* Artifacts of uncertain ownership need `flux cleanup --target PATH --break-lock` (251.2).
           refused[self] := "TARGET_LOCK_UNCERTAIN";
         }
         else { refused[self] := "TARGET_LOCK_BUSY"; };
       clean_refused:
         goto Done;
       clean_recover:
         call Recover();
       S251_1_delete:
         \* Its own cleanup lock goes last, while it still holds the OS-native lock (240.5, 251.1).
         if (holding[self]) {
           holding[self] := FALSE;
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
         };
       S251_1_close:
         if (LockObj # NoObj /\ OpenBy(fs, self, LockObj)) { fs := FsClose(fs, self, LockObj).fs; };
     }
   } *)
\* BEGIN TRANSLATION (chksum(pcal) = "26687287" /\ chksum(tla) = "cf40566c")
\* Procedure variable obj of procedure Classify at line 85 col 18 changed to obj_
CONSTANT defaultInitValue
VARIABLES fs, classified, ownerLive, sawLive, lastRecord, crashed, holding, 
          checked, recoveredAfterCrash, tornRead, touchedUncertain, 
          touchedForeign, refusedOk, refused, pc, stack

(* define statement *)
LockObj == At(fs, P, LockName)
OwnRecord(p) == Rec(p, IF p \in Cleanups THEN "cleanup" ELSE "operation")

StillOwned(p) == LockObj # NoObj /\ fs.content[LockObj] = OwnRecord(p)


BusyJustified == /\ LockObj # NoObj
                 /\ IsRecord(fs.content[LockObj])
                 /\ ~crashed[fs.content[LockObj].op]
ForeignAtLock == LockObj # NoObj /\ fs.content[LockObj] = Foreign


Replaceable(p) == \/ classified[p] = "dead"
                  \/ (classified[p] = "cleanuplock" /\ ownerLive[p] = "dead")


DeadOwnerLock == /\ LockObj # NoObj
                 /\ IsRecord(fs.content[LockObj])
                 /\ crashed[fs.content[LockObj].op]
TornLock == LockObj # NoObj /\ fs.content[LockObj] = Torn

VARIABLES keep, obj_, got, obj, robj, victim

vars == << fs, classified, ownerLive, sawLive, lastRecord, crashed, holding, 
           checked, recoveredAfterCrash, tornRead, touchedUncertain, 
           touchedForeign, refusedOk, refused, pc, stack, keep, obj_, got, 
           obj, robj, victim >>

ProcSet == (Owners) \cup (PlainRuns) \cup (Recoverers) \cup (Cleanups)

Init == (* Global variables *)
        /\ fs = FsInit
        /\ classified = [p \in Procs |-> "none"]
        /\ ownerLive = [p \in Procs |-> "none"]
        /\ sawLive = [p \in Procs |-> FALSE]
        /\ lastRecord = EmptyFile
        /\ crashed = [p \in Procs |-> FALSE]
        /\ holding = [p \in Procs |-> FALSE]
        /\ checked = [p \in Procs |-> FALSE]
        /\ recoveredAfterCrash = FALSE
        /\ tornRead = FALSE
        /\ touchedUncertain = FALSE
        /\ touchedForeign = FALSE
        /\ refusedOk = [p \in Procs |-> FALSE]
        /\ refused = [p \in Procs |-> "none"]
        (* Procedure Classify *)
        /\ keep = [ self \in ProcSet |-> defaultInitValue]
        /\ obj_ = [ self \in ProcSet |-> 0]
        /\ got = [ self \in ProcSet |-> FALSE]
        (* Procedure Acquire *)
        /\ obj = [ self \in ProcSet |-> 0]
        (* Procedure Recover *)
        /\ robj = [ self \in ProcSet |-> 0]
        /\ victim = [ self \in ProcSet |-> NoProc]
        /\ stack = [self \in ProcSet |-> << >>]
        /\ pc = [self \in ProcSet |-> CASE self \in Owners -> "own_start"
                                        [] self \in PlainRuns -> "plain_start"
                                        [] self \in Recoverers -> "rec_start"
                                        [] self \in Cleanups -> "clean_start"]

S240_1_open(self) == /\ pc[self] = "S240_1_open"
                     /\ LET r == FsOpen(fs, P, LockName, self, TRUE) IN
                          IF r.ok
                             THEN /\ fs' = r.fs
                                  /\ obj_' = [obj_ EXCEPT ![self] = r.val]
                                  /\ pc' = [pc EXCEPT ![self] = "S240_1_trylock"]
                                  /\ UNCHANGED << classified, stack, keep, got >>
                             ELSE /\ classified' = [classified EXCEPT ![self] = "empty"]
                                  /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                  /\ obj_' = [obj_ EXCEPT ![self] = Head(stack[self]).obj_]
                                  /\ got' = [got EXCEPT ![self] = Head(stack[self]).got]
                                  /\ keep' = [keep EXCEPT ![self] = Head(stack[self]).keep]
                                  /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                                  /\ fs' = fs
                     /\ UNCHANGED << ownerLive, sawLive, lastRecord, crashed, 
                                     holding, checked, recoveredAfterCrash, 
                                     tornRead, touchedUncertain, 
                                     touchedForeign, refusedOk, refused, obj, 
                                     robj, victim >>

S240_1_trylock(self) == /\ pc[self] = "S240_1_trylock"
                        /\ LET r == FsTryLock(fs, self, obj_[self]) IN
                             /\ got' = [got EXCEPT ![self] = r.ok]
                             /\ IF r.ok
                                   THEN /\ fs' = r.fs
                                   ELSE /\ TRUE
                                        /\ fs' = fs
                        /\ pc' = [pc EXCEPT ![self] = "S240_1_read"]
                        /\ UNCHANGED << classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, refused, stack, keep, obj_, 
                                        obj, robj, victim >>

S240_1_read(self) == /\ pc[self] = "S240_1_read"
                     /\ LET seen == fs.content[obj_[self]] IN
                          /\ IF seen = Torn
                                THEN /\ tornRead' = TRUE
                                ELSE /\ TRUE
                                     /\ UNCHANGED tornRead
                          /\ IF seen = Torn \/ seen = EmptyFile
                                THEN /\ classified' = [classified EXCEPT ![self] = "uncertain"]
                                     /\ ownerLive' = [ownerLive EXCEPT ![self] = "none"]
                                     /\ sawLive' = [sawLive EXCEPT ![self] = FALSE]
                                ELSE /\ IF seen = Foreign
                                           THEN /\ classified' = [classified EXCEPT ![self] = "foreign"]
                                                /\ ownerLive' = [ownerLive EXCEPT ![self] = "none"]
                                                /\ sawLive' = [sawLive EXCEPT ![self] = FALSE]
                                           ELSE /\ \E alive \in  IF ~got[self] /\ LockCapability = "strong" THEN {"live"}
                                                                ELSE IF crashed[seen.op] THEN {"dead", "uncertain"}
                                                                ELSE {"live", "uncertain"}:
                                                     /\ ownerLive' = [ownerLive EXCEPT ![self] = alive]
                                                     /\ sawLive' = [sawLive EXCEPT ![self] = alive = "live"]
                                                     /\ IF seen.kind = "cleanup"
                                                           THEN /\ classified' = [classified EXCEPT ![self] = "cleanuplock"]
                                                           ELSE /\ classified' = [classified EXCEPT ![self] = alive]
                     /\ pc' = [pc EXCEPT ![self] = "S240_1_unlock"]
                     /\ UNCHANGED << fs, lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, touchedUncertain, 
                                     touchedForeign, refusedOk, refused, stack, 
                                     keep, obj_, got, obj, robj, victim >>

S240_1_unlock(self) == /\ pc[self] = "S240_1_unlock"
                       /\ IF got[self] /\ (~keep[self] \/ ~Replaceable(self))
                             THEN /\ fs' = FsUnlock(fs, self, obj_[self]).fs
                             ELSE /\ TRUE
                                  /\ fs' = fs
                       /\ pc' = [pc EXCEPT ![self] = "S240_1_close"]
                       /\ UNCHANGED << classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim >>

S240_1_close(self) == /\ pc[self] = "S240_1_close"
                      /\ IF ~keep[self] \/ ~Replaceable(self)
                            THEN /\ fs' = FsClose(fs, self, obj_[self]).fs
                            ELSE /\ TRUE
                                 /\ fs' = fs
                      /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                      /\ obj_' = [obj_ EXCEPT ![self] = Head(stack[self]).obj_]
                      /\ got' = [got EXCEPT ![self] = Head(stack[self]).got]
                      /\ keep' = [keep EXCEPT ![self] = Head(stack[self]).keep]
                      /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                      /\ UNCHANGED << classified, ownerLive, sawLive, 
                                      lastRecord, crashed, holding, checked, 
                                      recoveredAfterCrash, tornRead, 
                                      touchedUncertain, touchedForeign, 
                                      refusedOk, refused, obj, robj, victim >>

Classify(self) == S240_1_open(self) \/ S240_1_trylock(self)
                     \/ S240_1_read(self) \/ S240_1_unlock(self)
                     \/ S240_1_close(self)

S96_1_create(self) == /\ pc[self] = "S96_1_create"
                      /\ LET r == FsCreate(fs, P, LockName, self, TRUE) IN
                           IF r.ok
                              THEN /\ fs' = r.fs
                                   /\ obj' = [obj EXCEPT ![self] = r.val]
                                   /\ pc' = [pc EXCEPT ![self] = "S96_1_dircheck"]
                                   /\ UNCHANGED << refused, stack >>
                              ELSE /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                                   /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                   /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                                   /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                                   /\ fs' = fs
                      /\ UNCHANGED << classified, ownerLive, sawLive, 
                                      lastRecord, crashed, holding, checked, 
                                      recoveredAfterCrash, tornRead, 
                                      touchedUncertain, touchedForeign, 
                                      refusedOk, keep, obj_, got, robj, victim >>

S96_1_dircheck(self) == /\ pc[self] = "S96_1_dircheck"
                        /\ IF FsLookup(fs, P, DirLockName)
                              THEN /\ pc' = [pc EXCEPT ![self] = "S96_1_backoff"]
                              ELSE /\ pc' = [pc EXCEPT ![self] = "S96_1_ownlock"]
                        /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, refused, stack, keep, obj_, 
                                        got, obj, robj, victim >>

S96_1_ownlock(self) == /\ pc[self] = "S96_1_ownlock"
                       /\ LET r == FsTryLock(fs, self, obj[self]) IN
                            IF r.ok
                               THEN /\ fs' = r.fs
                               ELSE /\ TRUE
                                    /\ fs' = fs
                       /\ pc' = [pc EXCEPT ![self] = "S96_1_record_begin"]
                       /\ UNCHANGED << classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim >>

S96_1_record_begin(self) == /\ pc[self] = "S96_1_record_begin"
                            /\ fs' = FsWriteBegin(fs, obj[self]).fs
                            /\ pc' = [pc EXCEPT ![self] = "S96_1_record_end"]
                            /\ UNCHANGED << classified, ownerLive, sawLive, 
                                            lastRecord, crashed, holding, 
                                            checked, recoveredAfterCrash, 
                                            tornRead, touchedUncertain, 
                                            touchedForeign, refusedOk, refused, 
                                            stack, keep, obj_, got, obj, robj, 
                                            victim >>

S96_1_record_end(self) == /\ pc[self] = "S96_1_record_end"
                          /\ fs' = FsWriteEnd(fs, obj[self], OwnRecord(self)).fs
                          /\ lastRecord' = OwnRecord(self)
                          /\ holding' = [holding EXCEPT ![self] = TRUE]
                          /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                          /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                          /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                          /\ UNCHANGED << classified, ownerLive, sawLive, 
                                          crashed, checked, 
                                          recoveredAfterCrash, tornRead, 
                                          touchedUncertain, touchedForeign, 
                                          refusedOk, refused, keep, obj_, got, 
                                          robj, victim >>

S96_1_backoff(self) == /\ pc[self] = "S96_1_backoff"
                       /\ refusedOk' = [refusedOk EXCEPT ![self] = BusyJustified \/ sawLive[self]]
                       /\ \E c \in FsUnlinkChoices:
                            fs' = FsUnlink(fs, P, LockName, c).fs
                       /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                       /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                       /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                       /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                       /\ UNCHANGED << classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, keep, 
                                       obj_, got, robj, victim >>

Acquire(self) == S96_1_create(self) \/ S96_1_dircheck(self)
                    \/ S96_1_ownlock(self) \/ S96_1_record_begin(self)
                    \/ S96_1_record_end(self) \/ S96_1_backoff(self)

S240_3_s1(self) == /\ pc[self] = "S240_3_s1"
                   /\ robj' = [robj EXCEPT ![self] = LockObj]
                   /\ IF IsRecord(lastRecord)
                         THEN /\ victim' = [victim EXCEPT ![self] = lastRecord.op]
                         ELSE /\ TRUE
                              /\ UNCHANGED victim
                   /\ IF LockObj = NoObj \/ fs.content[LockObj] # lastRecord
                         THEN /\ pc' = [pc EXCEPT ![self] = "S240_3_restart"]
                         ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s2"]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, stack, keep, obj_, got, obj >>

S240_3_s2(self) == /\ pc[self] = "S240_3_s2"
                   /\ IF classified[self] = "uncertain"
                         THEN /\ touchedUncertain' = TRUE
                         ELSE /\ TRUE
                              /\ UNCHANGED touchedUncertain
                   /\ IF ForeignAtLock
                         THEN /\ touchedForeign' = TRUE
                         ELSE /\ TRUE
                              /\ UNCHANGED touchedForeign
                   /\ LET r == FsRenameNoReplace(fs, P, LockName, BrokenOf(self)) IN
                        IF r.ok
                           THEN /\ fs' = r.fs
                                /\ pc' = [pc EXCEPT ![self] = "S240_3_s3"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_restart"]
                                /\ fs' = fs
                   /\ UNCHANGED << classified, ownerLive, sawLive, lastRecord, 
                                   crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, refusedOk, 
                                   refused, stack, keep, obj_, got, obj, robj, 
                                   victim >>

S240_3_s3(self) == /\ pc[self] = "S240_3_s3"
                   /\ \E ident \in FsIdentityChoices(fs, P, BrokenOf(self)):
                        IF ident # robj[self] \/ fs.content[robj[self]] # lastRecord
                           THEN /\ pc' = [pc EXCEPT ![self] = "S240_3_putback"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s4"]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, stack, keep, obj_, got, obj, robj, 
                                   victim >>

S240_3_s4(self) == /\ pc[self] = "S240_3_s4"
                   /\ LET r == FsCreate(fs, P, LockName, self, TRUE) IN
                        IF r.ok
                           THEN /\ fs' = r.fs
                                /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_lock"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_drop"]
                                /\ fs' = fs
                   /\ UNCHANGED << classified, ownerLive, sawLive, lastRecord, 
                                   crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, stack, keep, obj_, got, obj, robj, 
                                   victim >>

S240_3_s4_lock(self) == /\ pc[self] = "S240_3_s4_lock"
                        /\ LET r == FsTryLock(fs, self, LockObj) IN
                             IF r.ok
                                THEN /\ fs' = r.fs
                                ELSE /\ TRUE
                                     /\ fs' = fs
                        /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_record_begin"]
                        /\ UNCHANGED << classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, refused, stack, keep, obj_, 
                                        got, obj, robj, victim >>

S240_3_s4_record_begin(self) == /\ pc[self] = "S240_3_s4_record_begin"
                                /\ fs' = FsWriteBegin(fs, LockObj).fs
                                /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_record_end"]
                                /\ UNCHANGED << classified, ownerLive, sawLive, 
                                                lastRecord, crashed, holding, 
                                                checked, recoveredAfterCrash, 
                                                tornRead, touchedUncertain, 
                                                touchedForeign, refusedOk, 
                                                refused, stack, keep, obj_, 
                                                got, obj, robj, victim >>

S240_3_s4_record_end(self) == /\ pc[self] = "S240_3_s4_record_end"
                              /\ fs' = FsWriteEnd(fs, LockObj, OwnRecord(self)).fs
                              /\ lastRecord' = OwnRecord(self)
                              /\ holding' = [holding EXCEPT ![self] = TRUE]
                              /\ pc' = [pc EXCEPT ![self] = "S240_3_s5"]
                              /\ UNCHANGED << classified, ownerLive, sawLive, 
                                              crashed, checked, 
                                              recoveredAfterCrash, tornRead, 
                                              touchedUncertain, touchedForeign, 
                                              refusedOk, refused, stack, keep, 
                                              obj_, got, obj, robj, victim >>

S240_3_s5(self) == /\ pc[self] = "S240_3_s5"
                   /\ \E c \in FsUnlinkChoices:
                        fs' = FsUnlink(fs, P, BrokenOf(self), c).fs
                   /\ IF victim[self] \in Procs
                         THEN /\ IF crashed[victim[self]]
                                    THEN /\ recoveredAfterCrash' = TRUE
                                    ELSE /\ TRUE
                                         /\ UNCHANGED recoveredAfterCrash
                         ELSE /\ TRUE
                              /\ UNCHANGED recoveredAfterCrash
                   /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                   /\ UNCHANGED << classified, ownerLive, sawLive, lastRecord, 
                                   crashed, holding, checked, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, stack, keep, obj_, got, obj, robj, 
                                   victim >>

S240_3_s4_drop(self) == /\ pc[self] = "S240_3_s4_drop"
                        /\ \E c \in FsUnlinkChoices:
                             fs' = FsUnlink(fs, P, BrokenOf(self), c).fs
                        /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                        /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, stack, keep, obj_, got, obj, 
                                        robj, victim >>

S240_3_putback(self) == /\ pc[self] = "S240_3_putback"
                        /\ LET r == FsRenameNoReplace(fs, P, BrokenOf(self), LockName) IN
                             IF r.ok
                                THEN /\ fs' = r.fs
                                ELSE /\ TRUE
                                     /\ fs' = fs
                        /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                        /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, stack, keep, obj_, got, obj, 
                                        robj, victim >>

S240_3_restart(self) == /\ pc[self] = "S240_3_restart"
                        /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                        /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, stack, keep, obj_, got, obj, 
                                        robj, victim >>

S240_3_release(self) == /\ pc[self] = "S240_3_release"
                        /\ IF robj[self] # NoObj /\ OpenBy(fs, self, robj[self])
                              THEN /\ fs' = FsClose(fs, self, robj[self]).fs
                              ELSE /\ TRUE
                                   /\ fs' = fs
                        /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                        /\ robj' = [robj EXCEPT ![self] = Head(stack[self]).robj]
                        /\ victim' = [victim EXCEPT ![self] = Head(stack[self]).victim]
                        /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                        /\ UNCHANGED << classified, ownerLive, sawLive, 
                                        lastRecord, crashed, holding, checked, 
                                        recoveredAfterCrash, tornRead, 
                                        touchedUncertain, touchedForeign, 
                                        refusedOk, refused, keep, obj_, got, 
                                        obj >>

Recover(self) == S240_3_s1(self) \/ S240_3_s2(self) \/ S240_3_s3(self)
                    \/ S240_3_s4(self) \/ S240_3_s4_lock(self)
                    \/ S240_3_s4_record_begin(self)
                    \/ S240_3_s4_record_end(self) \/ S240_3_s5(self)
                    \/ S240_3_s4_drop(self) \/ S240_3_putback(self)
                    \/ S240_3_restart(self) \/ S240_3_release(self)

S99_check(self) == /\ pc[self] = "S99_check"
                   /\ IF StillOwned(self)
                         THEN /\ checked' = [checked EXCEPT ![self] = TRUE]
                              /\ pc' = [pc EXCEPT ![self] = "S99_write"]
                              /\ UNCHANGED << holding, refusedOk, refused, 
                                              stack >>
                         ELSE /\ checked' = [checked EXCEPT ![self] = FALSE]
                              /\ refusedOk' = [refusedOk EXCEPT ![self] = BusyJustified \/ sawLive[self]]
                              /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                              /\ holding' = [holding EXCEPT ![self] = FALSE]
                              /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                              /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, recoveredAfterCrash, 
                                   tornRead, touchedUncertain, touchedForeign, 
                                   keep, obj_, got, obj, robj, victim >>

S99_write(self) == /\ pc[self] = "S99_write"
                   /\ TRUE
                   /\ pc' = [pc EXCEPT ![self] = "S99_release"]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, stack, keep, obj_, got, obj, robj, 
                                   victim >>

S99_release(self) == /\ pc[self] = "S99_release"
                     /\ checked' = [checked EXCEPT ![self] = FALSE]
                     /\ holding' = [holding EXCEPT ![self] = FALSE]
                     /\ \E c \in FsUnlinkChoices:
                          fs' = FsUnlink(fs, P, LockName, c).fs
                     /\ pc' = [pc EXCEPT ![self] = "S99_close"]
                     /\ UNCHANGED << classified, ownerLive, sawLive, 
                                     lastRecord, crashed, recoveredAfterCrash, 
                                     tornRead, touchedUncertain, 
                                     touchedForeign, refusedOk, refused, stack, 
                                     keep, obj_, got, obj, robj, victim >>

S99_close(self) == /\ pc[self] = "S99_close"
                   /\ IF LockObj # NoObj /\ OpenBy(fs, self, LockObj)
                         THEN /\ fs' = FsClose(fs, self, LockObj).fs
                         ELSE /\ TRUE
                              /\ fs' = fs
                   /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                   /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                   /\ UNCHANGED << classified, ownerLive, sawLive, lastRecord, 
                                   crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, keep, obj_, got, obj, robj, victim >>

Publish(self) == S99_check(self) \/ S99_write(self) \/ S99_release(self)
                    \/ S99_close(self)

own_start(self) == /\ pc[self] = "own_start"
                   /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                            pc        |->  "own_publish",
                                                            obj       |->  obj[self] ] >>
                                                        \o stack[self]]
                   /\ obj' = [obj EXCEPT ![self] = 0]
                   /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, keep, obj_, got, robj, victim >>

own_publish(self) == /\ pc[self] = "own_publish"
                     /\ IF holding[self]
                           THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                         pc        |->  "Done" ] >>
                                                                     \o stack[self]]
                                /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "Done"]
                                /\ stack' = stack
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, keep, obj_, got, obj, 
                                     robj, victim >>

own(self) == own_start(self) \/ own_publish(self)

plain_start(self) == /\ pc[self] = "plain_start"
                     /\ /\ keep' = [keep EXCEPT ![self] = FALSE]
                        /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Classify",
                                                                 pc        |->  "S21_1_decide",
                                                                 obj_      |->  obj_[self],
                                                                 got       |->  got[self],
                                                                 keep      |->  keep[self] ] >>
                                                             \o stack[self]]
                     /\ obj_' = [obj_ EXCEPT ![self] = 0]
                     /\ got' = [got EXCEPT ![self] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = "S240_1_open"]
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, obj, robj, victim >>

S21_1_decide(self) == /\ pc[self] = "S21_1_decide"
                      /\ refusedOk' = [refusedOk EXCEPT ![self] = BusyJustified \/ sawLive[self]]
                      /\ IF classified[self] = "empty"
                            THEN /\ pc' = [pc EXCEPT ![self] = "plain_acquire"]
                                 /\ UNCHANGED refused
                            ELSE /\ IF Replaceable(self) /\ classified[self] = "cleanuplock"
                                       THEN /\ pc' = [pc EXCEPT ![self] = "plain_recover"]
                                            /\ UNCHANGED refused
                                       ELSE /\ IF classified[self] = "live"
                                                  THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                                  ELSE /\ IF classified[self] = "uncertain" \/ ownerLive[self] = "uncertain"
                                                             THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_UNCERTAIN"]
                                                             ELSE /\ IF classified[self] = "foreign"
                                                                        THEN /\ refused' = [refused EXCEPT ![self] = "CONTROL_PLANE_NAMESPACE_CONFLICT"]
                                                                        ELSE /\ IF classified[self] = "cleanuplock"
                                                                                   THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                                                                   ELSE /\ refused' = [refused EXCEPT ![self] = "RESUMABLE_OPERATION_EXISTS"]
                                            /\ pc' = [pc EXCEPT ![self] = "S21_1_refused"]
                      /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                      lastRecord, crashed, holding, checked, 
                                      recoveredAfterCrash, tornRead, 
                                      touchedUncertain, touchedForeign, stack, 
                                      keep, obj_, got, obj, robj, victim >>

S21_1_refused(self) == /\ pc[self] = "S21_1_refused"
                       /\ pc' = [pc EXCEPT ![self] = "Done"]
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim >>

plain_recover(self) == /\ pc[self] = "plain_recover"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                                pc        |->  "plain_recovered",
                                                                robj      |->  robj[self],
                                                                victim    |->  victim[self] ] >>
                                                            \o stack[self]]
                       /\ robj' = [robj EXCEPT ![self] = 0]
                       /\ victim' = [victim EXCEPT ![self] = NoProc]
                       /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, keep, obj_, got, 
                                       obj >>

plain_recovered(self) == /\ pc[self] = "plain_recovered"
                         /\ IF holding[self]
                               THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                             pc        |->  "plain_recovered_done" ] >>
                                                                         \o stack[self]]
                                    /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                               ELSE /\ pc' = [pc EXCEPT ![self] = "plain_recovered_done"]
                                    /\ stack' = stack
                         /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                         lastRecord, crashed, holding, checked, 
                                         recoveredAfterCrash, tornRead, 
                                         touchedUncertain, touchedForeign, 
                                         refusedOk, refused, keep, obj_, got, 
                                         obj, robj, victim >>

plain_recovered_done(self) == /\ pc[self] = "plain_recovered_done"
                              /\ pc' = [pc EXCEPT ![self] = "Done"]
                              /\ UNCHANGED << fs, classified, ownerLive, 
                                              sawLive, lastRecord, crashed, 
                                              holding, checked, 
                                              recoveredAfterCrash, tornRead, 
                                              touchedUncertain, touchedForeign, 
                                              refusedOk, refused, stack, keep, 
                                              obj_, got, obj, robj, victim >>

plain_acquire(self) == /\ pc[self] = "plain_acquire"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                                pc        |->  "plain_publish",
                                                                obj       |->  obj[self] ] >>
                                                            \o stack[self]]
                       /\ obj' = [obj EXCEPT ![self] = 0]
                       /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, keep, obj_, got, 
                                       robj, victim >>

plain_publish(self) == /\ pc[self] = "plain_publish"
                       /\ IF holding[self]
                             THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                           pc        |->  "Done" ] >>
                                                                       \o stack[self]]
                                  /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                             ELSE /\ pc' = [pc EXCEPT ![self] = "Done"]
                                  /\ stack' = stack
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, keep, obj_, got, 
                                       obj, robj, victim >>

plain(self) == plain_start(self) \/ S21_1_decide(self)
                  \/ S21_1_refused(self) \/ plain_recover(self)
                  \/ plain_recovered(self) \/ plain_recovered_done(self)
                  \/ plain_acquire(self) \/ plain_publish(self)

rec_start(self) == /\ pc[self] = "rec_start"
                   /\ /\ keep' = [keep EXCEPT ![self] = TRUE]
                      /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Classify",
                                                               pc        |->  "rec_decide",
                                                               obj_      |->  obj_[self],
                                                               got       |->  got[self],
                                                               keep      |->  keep[self] ] >>
                                                           \o stack[self]]
                   /\ obj_' = [obj_ EXCEPT ![self] = 0]
                   /\ got' = [got EXCEPT ![self] = FALSE]
                   /\ pc' = [pc EXCEPT ![self] = "S240_1_open"]
                   /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                   lastRecord, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   touchedUncertain, touchedForeign, refusedOk, 
                                   refused, obj, robj, victim >>

rec_decide(self) == /\ pc[self] = "rec_decide"
                    /\ refusedOk' = [refusedOk EXCEPT ![self] = BusyJustified \/ sawLive[self]]
                    /\ IF Replaceable(self)
                          THEN /\ pc' = [pc EXCEPT ![self] = "rec_recover"]
                               /\ UNCHANGED refused
                          ELSE /\ IF classified[self] = "empty"
                                     THEN /\ pc' = [pc EXCEPT ![self] = "rec_acquire"]
                                          /\ UNCHANGED refused
                                     ELSE /\ IF classified[self] = "uncertain" \/ ownerLive[self] = "uncertain"
                                                THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_UNCERTAIN"]
                                                ELSE /\ IF classified[self] = "foreign"
                                                           THEN /\ refused' = [refused EXCEPT ![self] = "CONTROL_PLANE_NAMESPACE_CONFLICT"]
                                                           ELSE /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                          /\ pc' = [pc EXCEPT ![self] = "rec_refused"]
                    /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                    lastRecord, crashed, holding, checked, 
                                    recoveredAfterCrash, tornRead, 
                                    touchedUncertain, touchedForeign, stack, 
                                    keep, obj_, got, obj, robj, victim >>

rec_refused(self) == /\ pc[self] = "rec_refused"
                     /\ pc' = [pc EXCEPT ![self] = "Done"]
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, stack, keep, obj_, 
                                     got, obj, robj, victim >>

rec_recover(self) == /\ pc[self] = "rec_recover"
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                              pc        |->  "rec_publish",
                                                              robj      |->  robj[self],
                                                              victim    |->  victim[self] ] >>
                                                          \o stack[self]]
                     /\ robj' = [robj EXCEPT ![self] = 0]
                     /\ victim' = [victim EXCEPT ![self] = NoProc]
                     /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, keep, obj_, got, obj >>

rec_publish(self) == /\ pc[self] = "rec_publish"
                     /\ IF holding[self]
                           THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                         pc        |->  "rec_publish_done" ] >>
                                                                     \o stack[self]]
                                /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "rec_publish_done"]
                                /\ stack' = stack
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, keep, obj_, got, obj, 
                                     robj, victim >>

rec_publish_done(self) == /\ pc[self] = "rec_publish_done"
                          /\ pc' = [pc EXCEPT ![self] = "Done"]
                          /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                          lastRecord, crashed, holding, 
                                          checked, recoveredAfterCrash, 
                                          tornRead, touchedUncertain, 
                                          touchedForeign, refusedOk, refused, 
                                          stack, keep, obj_, got, obj, robj, 
                                          victim >>

rec_acquire(self) == /\ pc[self] = "rec_acquire"
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                              pc        |->  "rec_acquired",
                                                              obj       |->  obj[self] ] >>
                                                          \o stack[self]]
                     /\ obj' = [obj EXCEPT ![self] = 0]
                     /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, keep, obj_, got, robj, 
                                     victim >>

rec_acquired(self) == /\ pc[self] = "rec_acquired"
                      /\ IF holding[self]
                            THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                          pc        |->  "Done" ] >>
                                                                      \o stack[self]]
                                 /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                            ELSE /\ pc' = [pc EXCEPT ![self] = "Done"]
                                 /\ stack' = stack
                      /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                      lastRecord, crashed, holding, checked, 
                                      recoveredAfterCrash, tornRead, 
                                      touchedUncertain, touchedForeign, 
                                      refusedOk, refused, keep, obj_, got, obj, 
                                      robj, victim >>

rec(self) == rec_start(self) \/ rec_decide(self) \/ rec_refused(self)
                \/ rec_recover(self) \/ rec_publish(self)
                \/ rec_publish_done(self) \/ rec_acquire(self)
                \/ rec_acquired(self)

clean_start(self) == /\ pc[self] = "clean_start"
                     /\ /\ keep' = [keep EXCEPT ![self] = TRUE]
                        /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Classify",
                                                                 pc        |->  "S251_1_classify",
                                                                 obj_      |->  obj_[self],
                                                                 got       |->  got[self],
                                                                 keep      |->  keep[self] ] >>
                                                             \o stack[self]]
                     /\ obj_' = [obj_ EXCEPT ![self] = 0]
                     /\ got' = [got EXCEPT ![self] = FALSE]
                     /\ pc' = [pc EXCEPT ![self] = "S240_1_open"]
                     /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                     lastRecord, crashed, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     touchedUncertain, touchedForeign, 
                                     refusedOk, refused, obj, robj, victim >>

S251_1_classify(self) == /\ pc[self] = "S251_1_classify"
                         /\ refusedOk' = [refusedOk EXCEPT ![self] = BusyJustified \/ sawLive[self]]
                         /\ IF Replaceable(self)
                               THEN /\ pc' = [pc EXCEPT ![self] = "clean_recover"]
                                    /\ UNCHANGED refused
                               ELSE /\ IF classified[self] = "empty"
                                          THEN /\ refused' = [refused EXCEPT ![self] = "NOTHING_TO_CLEAN"]
                                          ELSE /\ IF classified[self] = "foreign"
                                                     THEN /\ refused' = [refused EXCEPT ![self] = "CONTROL_PLANE_NAMESPACE_CONFLICT"]
                                                     ELSE /\ IF classified[self] = "uncertain" \/ ownerLive[self] = "uncertain"
                                                                THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_UNCERTAIN"]
                                                                ELSE /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                    /\ pc' = [pc EXCEPT ![self] = "clean_refused"]
                         /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                         lastRecord, crashed, holding, checked, 
                                         recoveredAfterCrash, tornRead, 
                                         touchedUncertain, touchedForeign, 
                                         stack, keep, obj_, got, obj, robj, 
                                         victim >>

clean_refused(self) == /\ pc[self] = "clean_refused"
                       /\ pc' = [pc EXCEPT ![self] = "Done"]
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim >>

clean_recover(self) == /\ pc[self] = "clean_recover"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                                pc        |->  "S251_1_delete",
                                                                robj      |->  robj[self],
                                                                victim    |->  victim[self] ] >>
                                                            \o stack[self]]
                       /\ robj' = [robj EXCEPT ![self] = 0]
                       /\ victim' = [victim EXCEPT ![self] = NoProc]
                       /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                       /\ UNCHANGED << fs, classified, ownerLive, sawLive, 
                                       lastRecord, crashed, holding, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, keep, obj_, got, 
                                       obj >>

S251_1_delete(self) == /\ pc[self] = "S251_1_delete"
                       /\ IF holding[self]
                             THEN /\ holding' = [holding EXCEPT ![self] = FALSE]
                                  /\ \E c \in FsUnlinkChoices:
                                       fs' = FsUnlink(fs, P, LockName, c).fs
                             ELSE /\ TRUE
                                  /\ UNCHANGED << fs, holding >>
                       /\ pc' = [pc EXCEPT ![self] = "S251_1_close"]
                       /\ UNCHANGED << classified, ownerLive, sawLive, 
                                       lastRecord, crashed, checked, 
                                       recoveredAfterCrash, tornRead, 
                                       touchedUncertain, touchedForeign, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim >>

S251_1_close(self) == /\ pc[self] = "S251_1_close"
                      /\ IF LockObj # NoObj /\ OpenBy(fs, self, LockObj)
                            THEN /\ fs' = FsClose(fs, self, LockObj).fs
                            ELSE /\ TRUE
                                 /\ fs' = fs
                      /\ pc' = [pc EXCEPT ![self] = "Done"]
                      /\ UNCHANGED << classified, ownerLive, sawLive, 
                                      lastRecord, crashed, holding, checked, 
                                      recoveredAfterCrash, tornRead, 
                                      touchedUncertain, touchedForeign, 
                                      refusedOk, refused, stack, keep, obj_, 
                                      got, obj, robj, victim >>

clean(self) == clean_start(self) \/ S251_1_classify(self)
                  \/ clean_refused(self) \/ clean_recover(self)
                  \/ S251_1_delete(self) \/ S251_1_close(self)

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == (\E self \in ProcSet:  \/ Classify(self) \/ Acquire(self)
                               \/ Recover(self) \/ Publish(self))
           \/ (\E self \in Owners: own(self))
           \/ (\E self \in PlainRuns: plain(self))
           \/ (\E self \in Recoverers: rec(self))
           \/ (\E self \in Cleanups: clean(self))
           \/ Terminating

Spec == Init /\ [][Next]_vars

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 

\* ------------------------------------------------------------------------------------------
\* Properties (design Section 7).
\*
\* Safety invariants hold in every reachable state and are checked in the scenario's `check` run.
\* Reachability is proved two ways: a step a label performs is proved by that label's TLC coverage
\* count (design Section 4), and the two facts no single label states - a read that found a torn
\* record, and a replaced lock that a crash had left - keep a ghost flag and are checked as witnesses
\* in their own small run without `-continue`, which stops at the first violation and prints one
\* trace. To get a trace for any other path, re-run the configuration with that witness as an
\* invariant and no `-continue`.

\* The filesystem model's own assumptions (FsModel.tla), so a simplification there cannot pass
\* unnoticed.
FsOk == FsInvariants(fs)

\* Every object's content is one of the forms the protocol knows, so a classifier's judgement table
\* covers every case it can meet (`Classifiable`, Section 7).
Classifiable == \A o \in Objs : fs.content[o] = NoContent \/ fs.content[o] \in Contents

\* For a target, at most one process is inside a publishing step whose last Section 99 check passed.
\* The one exception the spec accepts - a stalled owner's in-flight call issued before an operator
\* `--break-lock` - belongs to the `breaklock` scenario, which has the actors for it.
SingleWriter == Cardinality({p \in Procs : checked[p]}) <= 1

\* A process without `--break-lock` never removes, renames, or overwrites a lock it classified as
\* uncertain, and creates a lock only at an empty lock path (Section 7).
PlainNeverOwnsUncertain == ~touchedUncertain

\* A `Foreign` object at a lock path is never written, renamed, or deleted (Section 7).
ForeignUntouched == ~touchedForeign

\* Every TARGET_LOCK_BUSY refusal rests on evidence that the lock path held, or the refusing
\* process's handle referred to, a lock whose owner was alive or another takeover's (Section 7). The
\* refusing label records that, because the refusal may be reported after the owner it saw has
\* finished: the spec's refusal rests on what the classifier observed (240.2), not on a re-read.
RefusalJustified == \A p \in Procs : refused[p] = "TARGET_LOCK_BUSY" => refusedOk[p]

\* The two ghost witnesses, for the witness run.
NeverTornRead == ~tornRead
NeverRecoveredAfterCrash == ~recoveredAfterCrash

\* The two state witnesses the liveness runs need: the states their properties are about do occur
\* (design Section 7).
NeverDeadOwnerLock == ~DeadOwnerLock
NeverTornLock == ~TornLock
====
