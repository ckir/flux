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
    LockName,      \* P/<name>.flux-lock (96.1)
    DirLockName,   \* P/.flux-dir.lock, the long-name fallback (96.1); never created here
    MaxObjs,       \* how many objects this scenario's actors can create (bounds the state space)
    MaxCrashes,    \* how many crashes a run may have, both kinds together (design Section 5.2)
    HostCrashes,   \* whether a host crash is one of the crashes this run explores
    Platform, IdentityStrength, LockCapability,
    SEED_RECOVER_FOREIGN,    \* seeded defects (design Section 8); FALSE outside their seeded runs
    SEED_RECOVER_UNCERTAIN,
    SEED_DEAD_AS_BUSY

Procs == Owners \cup Recoverers \cup PlainRuns \cup Cleanups
\* <lock-name>.broken.<operation-id> beside the lock (240.3 step 2). Only an actor that can move a
\* lock aside needs such a name, and every name is an entry class in every state, so the Owners do
\* not get one. The target's own name is not here either: publishing sets a ghost in this scenario
\* (the `nested` scenario, which is about what gets written where, creates the object).
Movers == Recoverers \cup PlainRuns \cup Cleanups
BrokenOf(p) == <<"broken", p>>
Names == {LockName, DirLockName} \cup {BrokenOf(p) : p \in Movers}
Dirs == {P}
Fold == [n \in Names |-> n]          \* the recovery scenario needs no name folding

INSTANCE FsModel WITH Dirs <- Dirs, Names <- Names, Procs <- Procs, NoProc <- NoProc,
                      MaxObjs <- MaxObjs, Fold <- Fold, Platform <- Platform,
                      IdentityStrength <- IdentityStrength, LockCapability <- LockCapability

\* How a classifier judged the lock path (design Section 6.1).
RecovererPerms == Permutations(Recoverers)
Judgements == {"none", "empty", "foreign", "uncertain", "cleanuplock", "live", "dead"}

(* --fair algorithm LockProtocol {
     \* Each label performs at most ONE filesystem operation (design Section 5.2). A purely local
     \* decision that follows a call - a judgement over what was just read, a branch on whether the
     \* call succeeded - belongs to the same label: no other actor can observe the state between
     \* them, so giving it a label of its own would only add interleavings (Lipton reduction).
     \*
     \* Every label of every actor begins by checking whether this process has crashed. A crash is
     \* performed by the `env` process below, which releases the handles and OS-native locks of the
     \* process it kills (Section 5.2); the process itself then performs nothing further and ends at
     \* Done, which is what keeps TLC's deadlock check meaningful.
     variables
       \* The filesystem (FsModel.tla). A run starts from an empty lock path, or from one holding an object
       \* that is not a Flux lock record, which no actor ever writes (design Section 7, ForeignUntouched).
       fs \in {FsInit, FsWith(P, LockName, Foreign)},
       foreignObj = At(fs, P, LockName),         \* ghost: that Foreign object, or NoObj
       classified = [p \in Procs |-> "none"],     \* ghost: each process's last judgement
       ownerLive = [p \in Procs |-> "none"],      \* what it decided about the record's owner
       sawLive = [p \in Procs |-> FALSE],         \* its classification found a live owner
       seenRec = [p \in Procs |-> EmptyFile],     \* ghost: what this process last read at the lock path
       crashed = [p \in Procs |-> FALSE],         \* ghost: this process died
       live = [p \in Procs |-> FALSE],            \* it has started and has neither finished nor died
       holding = [p \in Procs |-> FALSE],         \* this process owns the target lock now
       checked = [p \in Procs |-> FALSE],         \* its last Section 99 check passed
       recoveredAfterCrash = FALSE,               \* witness: a lock left by a crash was replaced
       tornRead = FALSE,                          \* witness: a process read a torn record
       hostCrashChangedLock = FALSE,              \* witness: a host crash changed what is at the lock path
       touchedUncertain = FALSE,                  \* a lock judged uncertain was mutated
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
       \* The evidence a TARGET_LOCK_BUSY refusal rests on, recorded by every refusing label through this
       \* one definition, so SEED_DEAD_AS_BUSY guards all of them (design Section 7, RefusalJustified).
       RefusalEvidence(p) == BusyJustified \/ sawLive[p]
       \* The actor's judgement was uncertain: a torn or empty record, or an owner the oracle cannot judge.
       JudgedUncertain(p) == classified[p] = "uncertain" \/ ownerLive[p] = "uncertain"
       \* A lock this actor may replace through 240.3: a dead owner's operation lock, or a cleanup
       \* lock whose owner is dead (251.1, 259.6). The two seeds re-introduce the defects of design Section 8.
       Replaceable(p) == \/ classified[p] = "dead"
                         \/ (classified[p] = "cleanuplock" /\ ownerLive[p] = "dead")
                         \/ (SEED_RECOVER_UNCERTAIN /\ JudgedUncertain(p))
                         \/ (SEED_RECOVER_FOREIGN /\ classified[p] = "foreign")
       \* The states the liveness properties are about, and the two state witnesses (Section 7).
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
         if (crashed[self]) { goto classify_crashed; }
         else {
           \* No entry at the lock path: the open fails and the judgement is "empty" (96.1).
           with (r = FsOpen(fs, P, LockName, self, TRUE)) {
             if (r.ok) { fs := r.fs; obj := r.val; }
             else { classified[self] := "empty"; seenRec[self] := EmptyFile; return; };
         };
         };
       S240_1_trylock:
         if (crashed[self]) { goto classify_crashed; }
         else {
           with (r = FsTryLock(fs, self, obj)) {
             got := r.ok;
             if (r.ok) { fs := r.fs; };
         };
         };
       S240_1_read:
         if (crashed[self]) { goto classify_crashed; }
         else {
           \* Read the record, then judge what was read (Section 6.1's table). A torn or unreadable
           \* record cannot show a dead owner: uncertain (96.1, 240.4, 259.6).
           with (seen = fs.content[obj]) {
             seenRec[self] := seen;
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
                 \* A "live" forced by a failed try-lock shows the lock is held, which is what BUSY means
                 \* (240.2); it does not show who holds it.
                 sawLive[self] := alive = "live";
                 \* A cleanup lock is its own case (251.1); an operation's lock takes its judgement
                 \* from its owner (240.1 to 240.4).
                 if (seen.kind = "cleanup") { classified[self] := "cleanuplock"; }
                 else { classified[self] := alive; };
         };
             };
           };
         };
       S240_1_close:
         \* Unless the caller keeps the handle through its next steps, the classifier closes it at
         \* once, which also gives back the OS-native lock (Section 5.1), so a refusal never leaves a
         \* lock held on another process's file (Section 6.1).
         if (crashed[self]) { goto classify_crashed; }
         else {
           if (~keep \/ ~Replaceable(self)) { fs := FsClose(fs, self, obj).fs; };
           return;
         };
       classify_crashed:
         return;
     }

     \* Acquire the target lock (96.1): create it exclusively, check the directory lock is absent,
     \* take its OS-native lock, then write this operation's record.
     procedure Acquire()
       variables obj = 0;
     {
       S96_1_create:
         if (crashed[self]) { goto acquire_crashed; }
         else {
           \* Another operation created the lock first: start the acquisition again (21.1 step 1).
           with (r = FsCreate(fs, P, LockName, self, TRUE)) {
             if (r.ok) { fs := r.fs; obj := r.val; }
             else { refused[self] := "RESTART"; return; };
         };
         };
       S96_1_dircheck:
         \* The per-name acquirer announces, then checks (96.1). The directory lock never exists in
         \* this scenario, so the conflict below is the `dirlock` scenario's business.
         if (crashed[self]) { goto acquire_crashed; }
         else {
           if (FsLookup(fs, P, DirLockName)) { goto S96_1_backoff; };
         };
       S96_1_ownlock:
         \* A classifier can open the file just created and take its lock before this step does.
         if (crashed[self]) { goto acquire_crashed; }
         else {
           with (r = FsTryLock(fs, self, obj)) {
             if (r.ok) { fs := r.fs; }
             else { goto S96_1_ownlock_backoff; };
         };
         };
       S96_1_record_begin:
         if (crashed[self]) { goto acquire_crashed; }
         else {
           fs := FsWriteBegin(fs, obj).fs;
         };
       S96_1_record_end:
         \* A crash between the two halves of the write leaves the record torn (Section 5.2), which
         \* is what the `NeverTornRead` witness run is about.
         if (crashed[self]) { goto acquire_crashed; }
         else {
           fs := FsWriteEnd(fs, obj, OwnRecord(self)).fs;
           seenRec[self] := OwnRecord(self);
           holding[self] := TRUE;
           return;
         };
       S96_1_backoff:
         \* On a conflict the acquirer removes what it created and reports TARGET_LOCK_BUSY (96.1).
         if (crashed[self]) { goto acquire_crashed; }
         else {
           refusedOk[self] := RefusalEvidence(self);
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
           refused[self] := "TARGET_LOCK_BUSY";
           return;
         };
       S96_1_ownlock_backoff:
         \* Without the OS-native lock the record would prove nothing, and 240.2 would read whoever does
         \* hold the lock as the owner: remove the file and start again (96.1).
         if (crashed[self]) { goto acquire_crashed; }
         else {
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
           refused[self] := "RESTART";
           return;
         };
       acquire_crashed:
         return;
     }

     \* Replace a dead owner's lock by moving it aside (240.3 steps 1 to 5). The caller has
     \* classified the lock as replaceable and still holds its handle and its OS-native lock.
     procedure Recover()
       variables robj = 0, victim = NoProc;
     {
       S240_3_s1:
         \* Re-read the lock: proceed only if it still names the same dead owner.
         if (crashed[self]) { goto recover_crashed; }
         else {
           robj := LockObj;
           if (IsRecord(seenRec[self])) { victim := seenRec[self].op; };
           if (LockObj = NoObj \/ fs.content[LockObj] # seenRec[self]) { goto S240_3_restart; };
         };
       S240_3_s2:
         \* Only one of several concurrent recoverers can move it; the others find the lock gone and
         \* start the acquisition again (240.3 step 2).
         if (crashed[self]) { goto recover_crashed; }
         else {
           if (JudgedUncertain(self)) { touchedUncertain := TRUE; };
           with (r = FsRenameNoReplace(fs, P, LockName, BrokenOf(self))) {
             if (r.ok) { fs := r.fs; } else { goto S240_3_restart; };
         };
         };
       S240_3_s3:
         \* Check the moved file is the one step 1 re-read, by identity AND by its record: a takeover
         \* rewrites the record in the same file, so identity alone is not enough (240.3 step 3).
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (ident \in FsIdentityChoices(fs, P, BrokenOf(self))) {
             if (ident # robj \/ fs.content[robj] # seenRec[self]) { goto S240_3_putback; };
         };
         };
       S240_3_s4:
         \* Create its own lock exclusively (step 4). If that fails, another operation owns the
         \* target: delete the moved file, whose owner is dead, and start again.
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (r = FsCreate(fs, P, LockName, self, TRUE)) {
             if (r.ok) { fs := r.fs; } else { goto S240_3_s4_drop; };
         };
         };
       S240_3_s4_lock:
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (r = FsTryLock(fs, self, LockObj)) {
             if (r.ok) { fs := r.fs; }
             else { goto S240_3_s4_lock_backoff; };
         };
         };
       S240_3_s4_record_begin:
         if (crashed[self]) { goto recover_crashed; }
         else {
           fs := FsWriteBegin(fs, LockObj).fs;
         };
       S240_3_s4_record_end:
         if (crashed[self]) { goto recover_crashed; }
         else {
           fs := FsWriteEnd(fs, LockObj, OwnRecord(self)).fs;
           seenRec[self] := OwnRecord(self);
           holding[self] := TRUE;
         };
       S240_3_s5:
         \* Delete the moved file (step 5).
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, BrokenOf(self), c).fs; };
           if (victim \in Procs) { if (crashed[victim]) { recoveredAfterCrash := TRUE; }; };
           goto S240_3_release;
         };
       S240_3_s4_drop:
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, BrokenOf(self), c).fs; };
           refused[self] := "RESTART";
           goto S240_3_release;
         };
       S240_3_s4_lock_backoff:
         \* The same rule at 240.3 step 4: remove the lock this recoverer just created, then drop the
         \* moved file as a failed create does, and start again.
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
           goto S240_3_s4_drop;
         };
       S240_3_putback:
         \* Rename the file back without replacing, then start the acquisition again (step 3).
         if (crashed[self]) { goto recover_crashed; }
         else {
           with (r = FsRenameNoReplace(fs, P, BrokenOf(self), LockName)) {
             if (r.ok) { fs := r.fs; };
           };
           refused[self] := "RESTART";
           goto S240_3_release;
         };
       S240_3_restart:
         \* Start the acquisition again (21.1 step 1). This model stops here instead of looping: one
         \* pass reaches every state a further one would, and the bound is in the README.
         refused[self] := "RESTART";
       S240_3_release:
         if (crashed[self]) { goto recover_crashed; }
         else {
           if (robj # NoObj /\ OpenBy(fs, self, robj)) { fs := FsClose(fs, self, robj).fs; };
           return;
         };
       recover_crashed:
         return;
     }

     \* Publish under the lock, revalidating before the write (Section 99), then release it.
     procedure Publish()
     {
       S99_check:
         \* "Still owned" is a read of the lock path; the decision that follows is local.
         if (crashed[self]) { goto publish_crashed; }
         else {
           if (StillOwned(self)) { checked[self] := TRUE; }
           else {
             checked[self] := FALSE;
             refusedOk[self] := RefusalEvidence(self);
             refused[self] := "TARGET_LOCK_BUSY";
             holding[self] := FALSE;
             return;
         };
         };
       S99_write:
         \* The write itself: a separate label, because the spec's check must come immediately before
         \* it and another actor can act in between (design Section 11's check-to-call window). What
         \* it writes is not part of the lock protocol here, so no object is created: the `nested`
         \* scenario, which is about what gets written where, creates it.
         if (crashed[self]) { goto publish_crashed; }
         else {
           skip;
         };
       S99_release:
         if (crashed[self]) { goto publish_crashed; }
         else {
           checked[self] := FALSE;
           holding[self] := FALSE;
           with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
         };
       S99_close:
         if (crashed[self]) { goto publish_crashed; }
         else {
           if (LockObj # NoObj /\ OpenBy(fs, self, LockObj)) { fs := FsClose(fs, self, LockObj).fs; };
           return;
         };
       publish_crashed:
         checked[self] := FALSE;
         return;
     }

     \* A normal operation: acquire the lock, publish, release.
     process (own \in Owners)
     {
       own_start:
         live[self] := TRUE;
         call Acquire();
       own_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       own_end:
         live[self] := FALSE;
     }

     \* A new invocation with no flags (21.1): classify what it finds, then act or refuse.
     process (plain \in PlainRuns)
     {
       plain_start:
         live[self] := TRUE;
         call Classify(FALSE);
       S21_1_decide:
         if (crashed[self]) { goto plain_end; }
         else {
           refusedOk[self] := RefusalEvidence(self);
           if (classified[self] = "empty") { goto plain_acquire; }
           else if (Replaceable(self) /\ classified[self] = "cleanuplock") {
             \* A cleanup lock names no resumable operation (259.6), so a dead one is removed as 240.3
             \* describes and this invocation proceeds (21.1).
             goto plain_recover;
           }
           else if (classified[self] = "live" \/ (SEED_DEAD_AS_BUSY /\ classified[self] = "dead")) {
             refused[self] := "TARGET_LOCK_BUSY";
           }
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
         };
       S21_1_refused:
         goto plain_end;
       plain_recover:
         call Recover();
       plain_recovered:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       plain_recovered_done:
         goto plain_end;
       plain_acquire:
         call Acquire();
       plain_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       plain_end:
         live[self] := FALSE;
     }

     \* A new invocation that finds a dead owner's lock, recovers it (240.3), then continues as an
     \* owner: the recovery path of 21.1 step 1.
     process (rec \in Recoverers)
     {
       rec_start:
         live[self] := TRUE;
         call Classify(TRUE);
       rec_decide:
         if (crashed[self]) { goto rec_end; }
         else {
           refusedOk[self] := RefusalEvidence(self);
           if (Replaceable(self)) { goto rec_recover; }
           else if (classified[self] = "empty") { goto rec_acquire; }
           else if (classified[self] = "uncertain" \/ ownerLive[self] = "uncertain") {
             refused[self] := "TARGET_LOCK_UNCERTAIN";
           }
           else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
           else { refused[self] := "TARGET_LOCK_BUSY"; };
         };
       rec_refused:
         goto rec_end;
       rec_recover:
         call Recover();
       rec_publish:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       rec_publish_done:
         goto rec_end;
       rec_acquire:
         call Acquire();
       rec_acquired:
         if (~crashed[self] /\ holding[self]) { call Publish(); };
       rec_end:
         live[self] := FALSE;
     }

     \* flux cleanup DEST: classify the lock, remove a dead owner's or a dead cleanup lock through
     \* 240.3 under its own cleanup lock, then delete that lock last (251.1, 259.6).
     process (clean \in Cleanups)
     {
       clean_start:
         live[self] := TRUE;
         call Classify(TRUE);
       S251_1_classify:
         if (crashed[self]) { goto clean_end; }
         else {
           refusedOk[self] := RefusalEvidence(self);
           if (Replaceable(self)) { goto clean_recover; }
           else if (classified[self] = "empty") { refused[self] := "NOTHING_TO_CLEAN"; }
           else if (classified[self] = "foreign") { refused[self] := "CONTROL_PLANE_NAMESPACE_CONFLICT"; }
           else if (classified[self] = "uncertain" \/ ownerLive[self] = "uncertain") {
             \* Artifacts of uncertain ownership need `flux cleanup --target PATH --break-lock` (251.2).
             refused[self] := "TARGET_LOCK_UNCERTAIN";
           }
           else { refused[self] := "TARGET_LOCK_BUSY"; };
         };
       clean_refused:
         goto clean_end;
       clean_recover:
         call Recover();
       S251_1_delete:
         \* Its own cleanup lock goes last, while it still holds the OS-native lock (240.5, 251.1).
         if (crashed[self]) { goto clean_end; }
         else {
           if (holding[self]) {
             holding[self] := FALSE;
             with (c \in FsUnlinkChoices) { fs := FsUnlink(fs, P, LockName, c).fs; };
         };
         };
       S251_1_close:
         if (crashed[self]) { goto clean_end; }
         else {
           if (LockObj # NoObj /\ OpenBy(fs, self, LockObj)) { fs := FsClose(fs, self, LockObj).fs; };
         };
       clean_end:
         live[self] := FALSE;
     }

     \* The environment: the crashes of Section 5.2. A process crash releases the handles, their
     \* sharing restrictions and the OS-native locks of the process it kills, and leaves everything it
     \* wrote as it is, so a record write caught between its two halves stays torn. A host crash is a
     \* process crash of every process that has started, and then resolves every unflushed object and
     \* every unflushed entry operation. At most `MaxCrashes` crashes happen in a run, counted across
     \* both kinds, and a host crash counts as one however many processes it stops.
     process (env = "env")
       variables crashes = 0;
     {
       env_loop:
         while (crashes < MaxCrashes) {
           either {
             \* Crash one running process.
             with (p \in {q \in Procs : live[q] /\ ~crashed[q]}) {
               fs := FsProcCrash(fs, p);
               crashed[p] := TRUE;
               \* It loses its in-memory state (Section 5.2): it is no longer inside a publishing
               \* step and no longer owns anything, whatever its lock file still says.
               checked[p] := FALSE;
               holding[p] := FALSE;
               crashes := crashes + 1;
             };
           }
           or {
             \* The whole host goes down.
             await HostCrashes;
             with (pick \in HostCrashPicks(fs), dirs \in HostCrashDirs) {
               \* An unflushed create, move-aside or removal at the lock path undone (Section 11).
               hostCrashChangedLock := hostCrashChangedLock \/ At(FsHostCrash(fs, pick, dirs), P, LockName) # LockObj;
               fs := FsHostCrash(fs, pick, dirs);
             };
             crashed := [q \in Procs |-> IF live[q] THEN TRUE ELSE crashed[q]];
             checked := [q \in Procs |-> IF live[q] THEN FALSE ELSE checked[q]];
             holding := [q \in Procs |-> IF live[q] THEN FALSE ELSE holding[q]];
             crashes := crashes + 1;
           }
           or {
             \* Nothing crashes after all: the environment may simply stop.
             goto env_done;
           };
         };
       env_done:
         skip;
     }
   } *)
\* BEGIN TRANSLATION (chksum(pcal) = "a75dc3f5" /\ chksum(tla) = "df90ed91")
\* Procedure variable obj of procedure Classify at line 107 col 18 changed to obj_
CONSTANT defaultInitValue
VARIABLES fs, foreignObj, classified, ownerLive, sawLive, seenRec, crashed, 
          live, holding, checked, recoveredAfterCrash, tornRead, 
          hostCrashChangedLock, touchedUncertain, refusedOk, refused, pc, 
          stack

(* define statement *)
LockObj == At(fs, P, LockName)
OwnRecord(p) == Rec(p, IF p \in Cleanups THEN "cleanup" ELSE "operation")

StillOwned(p) == LockObj # NoObj /\ fs.content[LockObj] = OwnRecord(p)


BusyJustified == /\ LockObj # NoObj
                 /\ IsRecord(fs.content[LockObj])
                 /\ ~crashed[fs.content[LockObj].op]


RefusalEvidence(p) == BusyJustified \/ sawLive[p]

JudgedUncertain(p) == classified[p] = "uncertain" \/ ownerLive[p] = "uncertain"


Replaceable(p) == \/ classified[p] = "dead"
                  \/ (classified[p] = "cleanuplock" /\ ownerLive[p] = "dead")
                  \/ (SEED_RECOVER_UNCERTAIN /\ JudgedUncertain(p))
                  \/ (SEED_RECOVER_FOREIGN /\ classified[p] = "foreign")

DeadOwnerLock == /\ LockObj # NoObj
                 /\ IsRecord(fs.content[LockObj])
                 /\ crashed[fs.content[LockObj].op]
TornLock == LockObj # NoObj /\ fs.content[LockObj] = Torn

VARIABLES keep, obj_, got, obj, robj, victim, crashes

vars == << fs, foreignObj, classified, ownerLive, sawLive, seenRec, crashed, 
           live, holding, checked, recoveredAfterCrash, tornRead, 
           hostCrashChangedLock, touchedUncertain, refusedOk, refused, pc, 
           stack, keep, obj_, got, obj, robj, victim, crashes >>

ProcSet == (Owners) \cup (PlainRuns) \cup (Recoverers) \cup (Cleanups) \cup {"env"}

Init == (* Global variables *)
        /\ fs \in {FsInit, FsWith(P, LockName, Foreign)}
        /\ foreignObj = At(fs, P, LockName)
        /\ classified = [p \in Procs |-> "none"]
        /\ ownerLive = [p \in Procs |-> "none"]
        /\ sawLive = [p \in Procs |-> FALSE]
        /\ seenRec = [p \in Procs |-> EmptyFile]
        /\ crashed = [p \in Procs |-> FALSE]
        /\ live = [p \in Procs |-> FALSE]
        /\ holding = [p \in Procs |-> FALSE]
        /\ checked = [p \in Procs |-> FALSE]
        /\ recoveredAfterCrash = FALSE
        /\ tornRead = FALSE
        /\ hostCrashChangedLock = FALSE
        /\ touchedUncertain = FALSE
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
        (* Process env *)
        /\ crashes = 0
        /\ stack = [self \in ProcSet |-> << >>]
        /\ pc = [self \in ProcSet |-> CASE self \in Owners -> "own_start"
                                        [] self \in PlainRuns -> "plain_start"
                                        [] self \in Recoverers -> "rec_start"
                                        [] self \in Cleanups -> "clean_start"
                                        [] self = "env" -> "env_loop"]

S240_1_open(self) == /\ pc[self] = "S240_1_open"
                     /\ IF crashed[self]
                           THEN /\ pc' = [pc EXCEPT ![self] = "classify_crashed"]
                                /\ UNCHANGED << fs, classified, seenRec, stack, 
                                                keep, obj_, got >>
                           ELSE /\ LET r == FsOpen(fs, P, LockName, self, TRUE) IN
                                     IF r.ok
                                        THEN /\ fs' = r.fs
                                             /\ obj_' = [obj_ EXCEPT ![self] = r.val]
                                             /\ pc' = [pc EXCEPT ![self] = "S240_1_trylock"]
                                             /\ UNCHANGED << classified, 
                                                             seenRec, stack, 
                                                             keep, got >>
                                        ELSE /\ classified' = [classified EXCEPT ![self] = "empty"]
                                             /\ seenRec' = [seenRec EXCEPT ![self] = EmptyFile]
                                             /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                             /\ obj_' = [obj_ EXCEPT ![self] = Head(stack[self]).obj_]
                                             /\ got' = [got EXCEPT ![self] = Head(stack[self]).got]
                                             /\ keep' = [keep EXCEPT ![self] = Head(stack[self]).keep]
                                             /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                                             /\ fs' = fs
                     /\ UNCHANGED << foreignObj, ownerLive, sawLive, crashed, 
                                     live, holding, checked, 
                                     recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, obj, robj, victim, 
                                     crashes >>

S240_1_trylock(self) == /\ pc[self] = "S240_1_trylock"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "classify_crashed"]
                                   /\ UNCHANGED << fs, got >>
                              ELSE /\ LET r == FsTryLock(fs, self, obj_[self]) IN
                                        /\ got' = [got EXCEPT ![self] = r.ok]
                                        /\ IF r.ok
                                              THEN /\ fs' = r.fs
                                              ELSE /\ TRUE
                                                   /\ fs' = fs
                                   /\ pc' = [pc EXCEPT ![self] = "S240_1_read"]
                        /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, refused, 
                                        stack, keep, obj_, obj, robj, victim, 
                                        crashes >>

S240_1_read(self) == /\ pc[self] = "S240_1_read"
                     /\ IF crashed[self]
                           THEN /\ pc' = [pc EXCEPT ![self] = "classify_crashed"]
                                /\ UNCHANGED << classified, ownerLive, sawLive, 
                                                seenRec, tornRead >>
                           ELSE /\ LET seen == fs.content[obj_[self]] IN
                                     /\ seenRec' = [seenRec EXCEPT ![self] = seen]
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
                                /\ pc' = [pc EXCEPT ![self] = "S240_1_close"]
                     /\ UNCHANGED << fs, foreignObj, crashed, live, holding, 
                                     checked, recoveredAfterCrash, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, stack, keep, obj_, 
                                     got, obj, robj, victim, crashes >>

S240_1_close(self) == /\ pc[self] = "S240_1_close"
                      /\ IF crashed[self]
                            THEN /\ pc' = [pc EXCEPT ![self] = "classify_crashed"]
                                 /\ UNCHANGED << fs, stack, keep, obj_, got >>
                            ELSE /\ IF ~keep[self] \/ ~Replaceable(self)
                                       THEN /\ fs' = FsClose(fs, self, obj_[self]).fs
                                       ELSE /\ TRUE
                                            /\ fs' = fs
                                 /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                 /\ obj_' = [obj_ EXCEPT ![self] = Head(stack[self]).obj_]
                                 /\ got' = [got EXCEPT ![self] = Head(stack[self]).got]
                                 /\ keep' = [keep EXCEPT ![self] = Head(stack[self]).keep]
                                 /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                      /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                      sawLive, seenRec, crashed, live, holding, 
                                      checked, recoveredAfterCrash, tornRead, 
                                      hostCrashChangedLock, touchedUncertain, 
                                      refusedOk, refused, obj, robj, victim, 
                                      crashes >>

classify_crashed(self) == /\ pc[self] = "classify_crashed"
                          /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                          /\ obj_' = [obj_ EXCEPT ![self] = Head(stack[self]).obj_]
                          /\ got' = [got EXCEPT ![self] = Head(stack[self]).got]
                          /\ keep' = [keep EXCEPT ![self] = Head(stack[self]).keep]
                          /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                          /\ UNCHANGED << fs, foreignObj, classified, 
                                          ownerLive, sawLive, seenRec, crashed, 
                                          live, holding, checked, 
                                          recoveredAfterCrash, tornRead, 
                                          hostCrashChangedLock, 
                                          touchedUncertain, refusedOk, refused, 
                                          obj, robj, victim, crashes >>

Classify(self) == S240_1_open(self) \/ S240_1_trylock(self)
                     \/ S240_1_read(self) \/ S240_1_close(self)
                     \/ classify_crashed(self)

S96_1_create(self) == /\ pc[self] = "S96_1_create"
                      /\ IF crashed[self]
                            THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                 /\ UNCHANGED << fs, refused, stack, obj >>
                            ELSE /\ LET r == FsCreate(fs, P, LockName, self, TRUE) IN
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
                      /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                      sawLive, seenRec, crashed, live, holding, 
                                      checked, recoveredAfterCrash, tornRead, 
                                      hostCrashChangedLock, touchedUncertain, 
                                      refusedOk, keep, obj_, got, robj, victim, 
                                      crashes >>

S96_1_dircheck(self) == /\ pc[self] = "S96_1_dircheck"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                              ELSE /\ IF FsLookup(fs, P, DirLockName)
                                         THEN /\ pc' = [pc EXCEPT ![self] = "S96_1_backoff"]
                                         ELSE /\ pc' = [pc EXCEPT ![self] = "S96_1_ownlock"]
                        /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, refused, 
                                        stack, keep, obj_, got, obj, robj, 
                                        victim, crashes >>

S96_1_ownlock(self) == /\ pc[self] = "S96_1_ownlock"
                       /\ IF crashed[self]
                             THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                  /\ fs' = fs
                             ELSE /\ LET r == FsTryLock(fs, self, obj[self]) IN
                                       IF r.ok
                                          THEN /\ fs' = r.fs
                                               /\ pc' = [pc EXCEPT ![self] = "S96_1_record_begin"]
                                          ELSE /\ pc' = [pc EXCEPT ![self] = "S96_1_ownlock_backoff"]
                                               /\ fs' = fs
                       /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       stack, keep, obj_, got, obj, robj, 
                                       victim, crashes >>

S96_1_record_begin(self) == /\ pc[self] = "S96_1_record_begin"
                            /\ IF crashed[self]
                                  THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                       /\ fs' = fs
                                  ELSE /\ fs' = FsWriteBegin(fs, obj[self]).fs
                                       /\ pc' = [pc EXCEPT ![self] = "S96_1_record_end"]
                            /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                            sawLive, seenRec, crashed, live, 
                                            holding, checked, 
                                            recoveredAfterCrash, tornRead, 
                                            hostCrashChangedLock, 
                                            touchedUncertain, refusedOk, 
                                            refused, stack, keep, obj_, got, 
                                            obj, robj, victim, crashes >>

S96_1_record_end(self) == /\ pc[self] = "S96_1_record_end"
                          /\ IF crashed[self]
                                THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                     /\ UNCHANGED << fs, seenRec, holding, 
                                                     stack, obj >>
                                ELSE /\ fs' = FsWriteEnd(fs, obj[self], OwnRecord(self)).fs
                                     /\ seenRec' = [seenRec EXCEPT ![self] = OwnRecord(self)]
                                     /\ holding' = [holding EXCEPT ![self] = TRUE]
                                     /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                     /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                                     /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                          /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                          sawLive, crashed, live, checked, 
                                          recoveredAfterCrash, tornRead, 
                                          hostCrashChangedLock, 
                                          touchedUncertain, refusedOk, refused, 
                                          keep, obj_, got, robj, victim, 
                                          crashes >>

S96_1_backoff(self) == /\ pc[self] = "S96_1_backoff"
                       /\ IF crashed[self]
                             THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                  /\ UNCHANGED << fs, refusedOk, refused, 
                                                  stack, obj >>
                             ELSE /\ refusedOk' = [refusedOk EXCEPT ![self] = RefusalEvidence(self)]
                                  /\ \E c \in FsUnlinkChoices:
                                       fs' = FsUnlink(fs, P, LockName, c).fs
                                  /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                  /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                  /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                                  /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                       /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, keep, obj_, got, robj, 
                                       victim, crashes >>

S96_1_ownlock_backoff(self) == /\ pc[self] = "S96_1_ownlock_backoff"
                               /\ IF crashed[self]
                                     THEN /\ pc' = [pc EXCEPT ![self] = "acquire_crashed"]
                                          /\ UNCHANGED << fs, refused, stack, 
                                                          obj >>
                                     ELSE /\ \E c \in FsUnlinkChoices:
                                               fs' = FsUnlink(fs, P, LockName, c).fs
                                          /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                                          /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                          /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                                          /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                               /\ UNCHANGED << foreignObj, classified, 
                                               ownerLive, sawLive, seenRec, 
                                               crashed, live, holding, checked, 
                                               recoveredAfterCrash, tornRead, 
                                               hostCrashChangedLock, 
                                               touchedUncertain, refusedOk, 
                                               keep, obj_, got, robj, victim, 
                                               crashes >>

acquire_crashed(self) == /\ pc[self] = "acquire_crashed"
                         /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                         /\ obj' = [obj EXCEPT ![self] = Head(stack[self]).obj]
                         /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                         /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                         sawLive, seenRec, crashed, live, 
                                         holding, checked, recoveredAfterCrash, 
                                         tornRead, hostCrashChangedLock, 
                                         touchedUncertain, refusedOk, refused, 
                                         keep, obj_, got, robj, victim, 
                                         crashes >>

Acquire(self) == S96_1_create(self) \/ S96_1_dircheck(self)
                    \/ S96_1_ownlock(self) \/ S96_1_record_begin(self)
                    \/ S96_1_record_end(self) \/ S96_1_backoff(self)
                    \/ S96_1_ownlock_backoff(self) \/ acquire_crashed(self)

S240_3_s1(self) == /\ pc[self] = "S240_3_s1"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                              /\ UNCHANGED << robj, victim >>
                         ELSE /\ robj' = [robj EXCEPT ![self] = LockObj]
                              /\ IF IsRecord(seenRec[self])
                                    THEN /\ victim' = [victim EXCEPT ![self] = seenRec[self].op]
                                    ELSE /\ TRUE
                                         /\ UNCHANGED victim
                              /\ IF LockObj = NoObj \/ fs.content[LockObj] # seenRec[self]
                                    THEN /\ pc' = [pc EXCEPT ![self] = "S240_3_restart"]
                                    ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s2"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, live, holding, 
                                   checked, recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, crashes >>

S240_3_s2(self) == /\ pc[self] = "S240_3_s2"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                              /\ UNCHANGED << fs, touchedUncertain >>
                         ELSE /\ IF JudgedUncertain(self)
                                    THEN /\ touchedUncertain' = TRUE
                                    ELSE /\ TRUE
                                         /\ UNCHANGED touchedUncertain
                              /\ LET r == FsRenameNoReplace(fs, P, LockName, BrokenOf(self)) IN
                                   IF r.ok
                                      THEN /\ fs' = r.fs
                                           /\ pc' = [pc EXCEPT ![self] = "S240_3_s3"]
                                      ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_restart"]
                                           /\ fs' = fs
                   /\ UNCHANGED << foreignObj, classified, ownerLive, sawLive, 
                                   seenRec, crashed, live, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, refusedOk, refused, 
                                   stack, keep, obj_, got, obj, robj, victim, 
                                   crashes >>

S240_3_s3(self) == /\ pc[self] = "S240_3_s3"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                         ELSE /\ \E ident \in FsIdentityChoices(fs, P, BrokenOf(self)):
                                   IF ident # robj[self] \/ fs.content[robj[self]] # seenRec[self]
                                      THEN /\ pc' = [pc EXCEPT ![self] = "S240_3_putback"]
                                      ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s4"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, live, holding, 
                                   checked, recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, robj, victim, crashes >>

S240_3_s4(self) == /\ pc[self] = "S240_3_s4"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                              /\ fs' = fs
                         ELSE /\ LET r == FsCreate(fs, P, LockName, self, TRUE) IN
                                   IF r.ok
                                      THEN /\ fs' = r.fs
                                           /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_lock"]
                                      ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_drop"]
                                           /\ fs' = fs
                   /\ UNCHANGED << foreignObj, classified, ownerLive, sawLive, 
                                   seenRec, crashed, live, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, robj, victim, crashes >>

S240_3_s4_lock(self) == /\ pc[self] = "S240_3_s4_lock"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                   /\ fs' = fs
                              ELSE /\ LET r == FsTryLock(fs, self, LockObj) IN
                                        IF r.ok
                                           THEN /\ fs' = r.fs
                                                /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_record_begin"]
                                           ELSE /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_lock_backoff"]
                                                /\ fs' = fs
                        /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, refused, 
                                        stack, keep, obj_, got, obj, robj, 
                                        victim, crashes >>

S240_3_s4_record_begin(self) == /\ pc[self] = "S240_3_s4_record_begin"
                                /\ IF crashed[self]
                                      THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                           /\ fs' = fs
                                      ELSE /\ fs' = FsWriteBegin(fs, LockObj).fs
                                           /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_record_end"]
                                /\ UNCHANGED << foreignObj, classified, 
                                                ownerLive, sawLive, seenRec, 
                                                crashed, live, holding, 
                                                checked, recoveredAfterCrash, 
                                                tornRead, hostCrashChangedLock, 
                                                touchedUncertain, refusedOk, 
                                                refused, stack, keep, obj_, 
                                                got, obj, robj, victim, 
                                                crashes >>

S240_3_s4_record_end(self) == /\ pc[self] = "S240_3_s4_record_end"
                              /\ IF crashed[self]
                                    THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                         /\ UNCHANGED << fs, seenRec, holding >>
                                    ELSE /\ fs' = FsWriteEnd(fs, LockObj, OwnRecord(self)).fs
                                         /\ seenRec' = [seenRec EXCEPT ![self] = OwnRecord(self)]
                                         /\ holding' = [holding EXCEPT ![self] = TRUE]
                                         /\ pc' = [pc EXCEPT ![self] = "S240_3_s5"]
                              /\ UNCHANGED << foreignObj, classified, 
                                              ownerLive, sawLive, crashed, 
                                              live, checked, 
                                              recoveredAfterCrash, tornRead, 
                                              hostCrashChangedLock, 
                                              touchedUncertain, refusedOk, 
                                              refused, stack, keep, obj_, got, 
                                              obj, robj, victim, crashes >>

S240_3_s5(self) == /\ pc[self] = "S240_3_s5"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                              /\ UNCHANGED << fs, recoveredAfterCrash >>
                         ELSE /\ \E c \in FsUnlinkChoices:
                                   fs' = FsUnlink(fs, P, BrokenOf(self), c).fs
                              /\ IF victim[self] \in Procs
                                    THEN /\ IF crashed[victim[self]]
                                               THEN /\ recoveredAfterCrash' = TRUE
                                               ELSE /\ TRUE
                                                    /\ UNCHANGED recoveredAfterCrash
                                    ELSE /\ TRUE
                                         /\ UNCHANGED recoveredAfterCrash
                              /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                   /\ UNCHANGED << foreignObj, classified, ownerLive, sawLive, 
                                   seenRec, crashed, live, holding, checked, 
                                   tornRead, hostCrashChangedLock, 
                                   touchedUncertain, refusedOk, refused, stack, 
                                   keep, obj_, got, obj, robj, victim, crashes >>

S240_3_s4_drop(self) == /\ pc[self] = "S240_3_s4_drop"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                   /\ UNCHANGED << fs, refused >>
                              ELSE /\ \E c \in FsUnlinkChoices:
                                        fs' = FsUnlink(fs, P, BrokenOf(self), c).fs
                                   /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                                   /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, stack, 
                                        keep, obj_, got, obj, robj, victim, 
                                        crashes >>

S240_3_s4_lock_backoff(self) == /\ pc[self] = "S240_3_s4_lock_backoff"
                                /\ IF crashed[self]
                                      THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                           /\ fs' = fs
                                      ELSE /\ \E c \in FsUnlinkChoices:
                                                fs' = FsUnlink(fs, P, LockName, c).fs
                                           /\ pc' = [pc EXCEPT ![self] = "S240_3_s4_drop"]
                                /\ UNCHANGED << foreignObj, classified, 
                                                ownerLive, sawLive, seenRec, 
                                                crashed, live, holding, 
                                                checked, recoveredAfterCrash, 
                                                tornRead, hostCrashChangedLock, 
                                                touchedUncertain, refusedOk, 
                                                refused, stack, keep, obj_, 
                                                got, obj, robj, victim, 
                                                crashes >>

S240_3_putback(self) == /\ pc[self] = "S240_3_putback"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                   /\ UNCHANGED << fs, refused >>
                              ELSE /\ LET r == FsRenameNoReplace(fs, P, BrokenOf(self), LockName) IN
                                        IF r.ok
                                           THEN /\ fs' = r.fs
                                           ELSE /\ TRUE
                                                /\ fs' = fs
                                   /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                                   /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, stack, 
                                        keep, obj_, got, obj, robj, victim, 
                                        crashes >>

S240_3_restart(self) == /\ pc[self] = "S240_3_restart"
                        /\ refused' = [refused EXCEPT ![self] = "RESTART"]
                        /\ pc' = [pc EXCEPT ![self] = "S240_3_release"]
                        /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, stack, 
                                        keep, obj_, got, obj, robj, victim, 
                                        crashes >>

S240_3_release(self) == /\ pc[self] = "S240_3_release"
                        /\ IF crashed[self]
                              THEN /\ pc' = [pc EXCEPT ![self] = "recover_crashed"]
                                   /\ UNCHANGED << fs, stack, robj, victim >>
                              ELSE /\ IF robj[self] # NoObj /\ OpenBy(fs, self, robj[self])
                                         THEN /\ fs' = FsClose(fs, self, robj[self]).fs
                                         ELSE /\ TRUE
                                              /\ fs' = fs
                                   /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                   /\ robj' = [robj EXCEPT ![self] = Head(stack[self]).robj]
                                   /\ victim' = [victim EXCEPT ![self] = Head(stack[self]).victim]
                                   /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                        /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                        sawLive, seenRec, crashed, live, 
                                        holding, checked, recoveredAfterCrash, 
                                        tornRead, hostCrashChangedLock, 
                                        touchedUncertain, refusedOk, refused, 
                                        keep, obj_, got, obj, crashes >>

recover_crashed(self) == /\ pc[self] = "recover_crashed"
                         /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                         /\ robj' = [robj EXCEPT ![self] = Head(stack[self]).robj]
                         /\ victim' = [victim EXCEPT ![self] = Head(stack[self]).victim]
                         /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                         /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                         sawLive, seenRec, crashed, live, 
                                         holding, checked, recoveredAfterCrash, 
                                         tornRead, hostCrashChangedLock, 
                                         touchedUncertain, refusedOk, refused, 
                                         keep, obj_, got, obj, crashes >>

Recover(self) == S240_3_s1(self) \/ S240_3_s2(self) \/ S240_3_s3(self)
                    \/ S240_3_s4(self) \/ S240_3_s4_lock(self)
                    \/ S240_3_s4_record_begin(self)
                    \/ S240_3_s4_record_end(self) \/ S240_3_s5(self)
                    \/ S240_3_s4_drop(self) \/ S240_3_s4_lock_backoff(self)
                    \/ S240_3_putback(self) \/ S240_3_restart(self)
                    \/ S240_3_release(self) \/ recover_crashed(self)

S99_check(self) == /\ pc[self] = "S99_check"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "publish_crashed"]
                              /\ UNCHANGED << holding, checked, refusedOk, 
                                              refused, stack >>
                         ELSE /\ IF StillOwned(self)
                                    THEN /\ checked' = [checked EXCEPT ![self] = TRUE]
                                         /\ pc' = [pc EXCEPT ![self] = "S99_write"]
                                         /\ UNCHANGED << holding, refusedOk, 
                                                         refused, stack >>
                                    ELSE /\ checked' = [checked EXCEPT ![self] = FALSE]
                                         /\ refusedOk' = [refusedOk EXCEPT ![self] = RefusalEvidence(self)]
                                         /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                         /\ holding' = [holding EXCEPT ![self] = FALSE]
                                         /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                                         /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, live, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   keep, obj_, got, obj, robj, victim, crashes >>

S99_write(self) == /\ pc[self] = "S99_write"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "publish_crashed"]
                         ELSE /\ TRUE
                              /\ pc' = [pc EXCEPT ![self] = "S99_release"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, live, holding, 
                                   checked, recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, robj, victim, crashes >>

S99_release(self) == /\ pc[self] = "S99_release"
                     /\ IF crashed[self]
                           THEN /\ pc' = [pc EXCEPT ![self] = "publish_crashed"]
                                /\ UNCHANGED << fs, holding, checked >>
                           ELSE /\ checked' = [checked EXCEPT ![self] = FALSE]
                                /\ holding' = [holding EXCEPT ![self] = FALSE]
                                /\ \E c \in FsUnlinkChoices:
                                     fs' = FsUnlink(fs, P, LockName, c).fs
                                /\ pc' = [pc EXCEPT ![self] = "S99_close"]
                     /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, 
                                     recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, stack, keep, obj_, 
                                     got, obj, robj, victim, crashes >>

S99_close(self) == /\ pc[self] = "S99_close"
                   /\ IF crashed[self]
                         THEN /\ pc' = [pc EXCEPT ![self] = "publish_crashed"]
                              /\ UNCHANGED << fs, stack >>
                         ELSE /\ IF LockObj # NoObj /\ OpenBy(fs, self, LockObj)
                                    THEN /\ fs' = FsClose(fs, self, LockObj).fs
                                    ELSE /\ TRUE
                                         /\ fs' = fs
                              /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                              /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                   /\ UNCHANGED << foreignObj, classified, ownerLive, sawLive, 
                                   seenRec, crashed, live, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, keep, obj_, got, obj, 
                                   robj, victim, crashes >>

publish_crashed(self) == /\ pc[self] = "publish_crashed"
                         /\ checked' = [checked EXCEPT ![self] = FALSE]
                         /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                         /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]
                         /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                         sawLive, seenRec, crashed, live, 
                                         holding, recoveredAfterCrash, 
                                         tornRead, hostCrashChangedLock, 
                                         touchedUncertain, refusedOk, refused, 
                                         keep, obj_, got, obj, robj, victim, 
                                         crashes >>

Publish(self) == S99_check(self) \/ S99_write(self) \/ S99_release(self)
                    \/ S99_close(self) \/ publish_crashed(self)

own_start(self) == /\ pc[self] = "own_start"
                   /\ live' = [live EXCEPT ![self] = TRUE]
                   /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                            pc        |->  "own_publish",
                                                            obj       |->  obj[self] ] >>
                                                        \o stack[self]]
                   /\ obj' = [obj EXCEPT ![self] = 0]
                   /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, keep, obj_, got, robj, 
                                   victim, crashes >>

own_publish(self) == /\ pc[self] = "own_publish"
                     /\ IF ~crashed[self] /\ holding[self]
                           THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                         pc        |->  "own_end" ] >>
                                                                     \o stack[self]]
                                /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "own_end"]
                                /\ stack' = stack
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, keep, obj_, got, obj, 
                                     robj, victim, crashes >>

own_end(self) == /\ pc[self] = "own_end"
                 /\ live' = [live EXCEPT ![self] = FALSE]
                 /\ pc' = [pc EXCEPT ![self] = "Done"]
                 /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                 sawLive, seenRec, crashed, holding, checked, 
                                 recoveredAfterCrash, tornRead, 
                                 hostCrashChangedLock, touchedUncertain, 
                                 refusedOk, refused, stack, keep, obj_, got, 
                                 obj, robj, victim, crashes >>

own(self) == own_start(self) \/ own_publish(self) \/ own_end(self)

plain_start(self) == /\ pc[self] = "plain_start"
                     /\ live' = [live EXCEPT ![self] = TRUE]
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
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, obj, robj, victim, 
                                     crashes >>

S21_1_decide(self) == /\ pc[self] = "S21_1_decide"
                      /\ IF crashed[self]
                            THEN /\ pc' = [pc EXCEPT ![self] = "plain_end"]
                                 /\ UNCHANGED << refusedOk, refused >>
                            ELSE /\ refusedOk' = [refusedOk EXCEPT ![self] = RefusalEvidence(self)]
                                 /\ IF classified[self] = "empty"
                                       THEN /\ pc' = [pc EXCEPT ![self] = "plain_acquire"]
                                            /\ UNCHANGED refused
                                       ELSE /\ IF Replaceable(self) /\ classified[self] = "cleanuplock"
                                                  THEN /\ pc' = [pc EXCEPT ![self] = "plain_recover"]
                                                       /\ UNCHANGED refused
                                                  ELSE /\ IF classified[self] = "live" \/ (SEED_DEAD_AS_BUSY /\ classified[self] = "dead")
                                                             THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                                             ELSE /\ IF classified[self] = "uncertain" \/ ownerLive[self] = "uncertain"
                                                                        THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_UNCERTAIN"]
                                                                        ELSE /\ IF classified[self] = "foreign"
                                                                                   THEN /\ refused' = [refused EXCEPT ![self] = "CONTROL_PLANE_NAMESPACE_CONFLICT"]
                                                                                   ELSE /\ IF classified[self] = "cleanuplock"
                                                                                              THEN /\ refused' = [refused EXCEPT ![self] = "TARGET_LOCK_BUSY"]
                                                                                              ELSE /\ refused' = [refused EXCEPT ![self] = "RESUMABLE_OPERATION_EXISTS"]
                                                       /\ pc' = [pc EXCEPT ![self] = "S21_1_refused"]
                      /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                      sawLive, seenRec, crashed, live, holding, 
                                      checked, recoveredAfterCrash, tornRead, 
                                      hostCrashChangedLock, touchedUncertain, 
                                      stack, keep, obj_, got, obj, robj, 
                                      victim, crashes >>

S21_1_refused(self) == /\ pc[self] = "S21_1_refused"
                       /\ pc' = [pc EXCEPT ![self] = "plain_end"]
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       stack, keep, obj_, got, obj, robj, 
                                       victim, crashes >>

plain_recover(self) == /\ pc[self] = "plain_recover"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                                pc        |->  "plain_recovered",
                                                                robj      |->  robj[self],
                                                                victim    |->  victim[self] ] >>
                                                            \o stack[self]]
                       /\ robj' = [robj EXCEPT ![self] = 0]
                       /\ victim' = [victim EXCEPT ![self] = NoProc]
                       /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       keep, obj_, got, obj, crashes >>

plain_recovered(self) == /\ pc[self] = "plain_recovered"
                         /\ IF ~crashed[self] /\ holding[self]
                               THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                             pc        |->  "plain_recovered_done" ] >>
                                                                         \o stack[self]]
                                    /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                               ELSE /\ pc' = [pc EXCEPT ![self] = "plain_recovered_done"]
                                    /\ stack' = stack
                         /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                         sawLive, seenRec, crashed, live, 
                                         holding, checked, recoveredAfterCrash, 
                                         tornRead, hostCrashChangedLock, 
                                         touchedUncertain, refusedOk, refused, 
                                         keep, obj_, got, obj, robj, victim, 
                                         crashes >>

plain_recovered_done(self) == /\ pc[self] = "plain_recovered_done"
                              /\ pc' = [pc EXCEPT ![self] = "plain_end"]
                              /\ UNCHANGED << fs, foreignObj, classified, 
                                              ownerLive, sawLive, seenRec, 
                                              crashed, live, holding, checked, 
                                              recoveredAfterCrash, tornRead, 
                                              hostCrashChangedLock, 
                                              touchedUncertain, refusedOk, 
                                              refused, stack, keep, obj_, got, 
                                              obj, robj, victim, crashes >>

plain_acquire(self) == /\ pc[self] = "plain_acquire"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                                pc        |->  "plain_publish",
                                                                obj       |->  obj[self] ] >>
                                                            \o stack[self]]
                       /\ obj' = [obj EXCEPT ![self] = 0]
                       /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       keep, obj_, got, robj, victim, crashes >>

plain_publish(self) == /\ pc[self] = "plain_publish"
                       /\ IF ~crashed[self] /\ holding[self]
                             THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                           pc        |->  "plain_end" ] >>
                                                                       \o stack[self]]
                                  /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                             ELSE /\ pc' = [pc EXCEPT ![self] = "plain_end"]
                                  /\ stack' = stack
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       keep, obj_, got, obj, robj, victim, 
                                       crashes >>

plain_end(self) == /\ pc[self] = "plain_end"
                   /\ live' = [live EXCEPT ![self] = FALSE]
                   /\ pc' = [pc EXCEPT ![self] = "Done"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, robj, victim, crashes >>

plain(self) == plain_start(self) \/ S21_1_decide(self)
                  \/ S21_1_refused(self) \/ plain_recover(self)
                  \/ plain_recovered(self) \/ plain_recovered_done(self)
                  \/ plain_acquire(self) \/ plain_publish(self)
                  \/ plain_end(self)

rec_start(self) == /\ pc[self] = "rec_start"
                   /\ live' = [live EXCEPT ![self] = TRUE]
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
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, obj, robj, victim, 
                                   crashes >>

rec_decide(self) == /\ pc[self] = "rec_decide"
                    /\ IF crashed[self]
                          THEN /\ pc' = [pc EXCEPT ![self] = "rec_end"]
                               /\ UNCHANGED << refusedOk, refused >>
                          ELSE /\ refusedOk' = [refusedOk EXCEPT ![self] = RefusalEvidence(self)]
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
                    /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                    sawLive, seenRec, crashed, live, holding, 
                                    checked, recoveredAfterCrash, tornRead, 
                                    hostCrashChangedLock, touchedUncertain, 
                                    stack, keep, obj_, got, obj, robj, victim, 
                                    crashes >>

rec_refused(self) == /\ pc[self] = "rec_refused"
                     /\ pc' = [pc EXCEPT ![self] = "rec_end"]
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, stack, keep, obj_, 
                                     got, obj, robj, victim, crashes >>

rec_recover(self) == /\ pc[self] = "rec_recover"
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                              pc        |->  "rec_publish",
                                                              robj      |->  robj[self],
                                                              victim    |->  victim[self] ] >>
                                                          \o stack[self]]
                     /\ robj' = [robj EXCEPT ![self] = 0]
                     /\ victim' = [victim EXCEPT ![self] = NoProc]
                     /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, keep, obj_, got, obj, 
                                     crashes >>

rec_publish(self) == /\ pc[self] = "rec_publish"
                     /\ IF ~crashed[self] /\ holding[self]
                           THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                         pc        |->  "rec_publish_done" ] >>
                                                                     \o stack[self]]
                                /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                           ELSE /\ pc' = [pc EXCEPT ![self] = "rec_publish_done"]
                                /\ stack' = stack
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, keep, obj_, got, obj, 
                                     robj, victim, crashes >>

rec_publish_done(self) == /\ pc[self] = "rec_publish_done"
                          /\ pc' = [pc EXCEPT ![self] = "rec_end"]
                          /\ UNCHANGED << fs, foreignObj, classified, 
                                          ownerLive, sawLive, seenRec, crashed, 
                                          live, holding, checked, 
                                          recoveredAfterCrash, tornRead, 
                                          hostCrashChangedLock, 
                                          touchedUncertain, refusedOk, refused, 
                                          stack, keep, obj_, got, obj, robj, 
                                          victim, crashes >>

rec_acquire(self) == /\ pc[self] = "rec_acquire"
                     /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Acquire",
                                                              pc        |->  "rec_acquired",
                                                              obj       |->  obj[self] ] >>
                                                          \o stack[self]]
                     /\ obj' = [obj EXCEPT ![self] = 0]
                     /\ pc' = [pc EXCEPT ![self] = "S96_1_create"]
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, live, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, keep, obj_, got, robj, 
                                     victim, crashes >>

rec_acquired(self) == /\ pc[self] = "rec_acquired"
                      /\ IF ~crashed[self] /\ holding[self]
                            THEN /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Publish",
                                                                          pc        |->  "rec_end" ] >>
                                                                      \o stack[self]]
                                 /\ pc' = [pc EXCEPT ![self] = "S99_check"]
                            ELSE /\ pc' = [pc EXCEPT ![self] = "rec_end"]
                                 /\ stack' = stack
                      /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                      sawLive, seenRec, crashed, live, holding, 
                                      checked, recoveredAfterCrash, tornRead, 
                                      hostCrashChangedLock, touchedUncertain, 
                                      refusedOk, refused, keep, obj_, got, obj, 
                                      robj, victim, crashes >>

rec_end(self) == /\ pc[self] = "rec_end"
                 /\ live' = [live EXCEPT ![self] = FALSE]
                 /\ pc' = [pc EXCEPT ![self] = "Done"]
                 /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                 sawLive, seenRec, crashed, holding, checked, 
                                 recoveredAfterCrash, tornRead, 
                                 hostCrashChangedLock, touchedUncertain, 
                                 refusedOk, refused, stack, keep, obj_, got, 
                                 obj, robj, victim, crashes >>

rec(self) == rec_start(self) \/ rec_decide(self) \/ rec_refused(self)
                \/ rec_recover(self) \/ rec_publish(self)
                \/ rec_publish_done(self) \/ rec_acquire(self)
                \/ rec_acquired(self) \/ rec_end(self)

clean_start(self) == /\ pc[self] = "clean_start"
                     /\ live' = [live EXCEPT ![self] = TRUE]
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
                     /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                     sawLive, seenRec, crashed, holding, 
                                     checked, recoveredAfterCrash, tornRead, 
                                     hostCrashChangedLock, touchedUncertain, 
                                     refusedOk, refused, obj, robj, victim, 
                                     crashes >>

S251_1_classify(self) == /\ pc[self] = "S251_1_classify"
                         /\ IF crashed[self]
                               THEN /\ pc' = [pc EXCEPT ![self] = "clean_end"]
                                    /\ UNCHANGED << refusedOk, refused >>
                               ELSE /\ refusedOk' = [refusedOk EXCEPT ![self] = RefusalEvidence(self)]
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
                         /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                         sawLive, seenRec, crashed, live, 
                                         holding, checked, recoveredAfterCrash, 
                                         tornRead, hostCrashChangedLock, 
                                         touchedUncertain, stack, keep, obj_, 
                                         got, obj, robj, victim, crashes >>

clean_refused(self) == /\ pc[self] = "clean_refused"
                       /\ pc' = [pc EXCEPT ![self] = "clean_end"]
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       stack, keep, obj_, got, obj, robj, 
                                       victim, crashes >>

clean_recover(self) == /\ pc[self] = "clean_recover"
                       /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Recover",
                                                                pc        |->  "S251_1_delete",
                                                                robj      |->  robj[self],
                                                                victim    |->  victim[self] ] >>
                                                            \o stack[self]]
                       /\ robj' = [robj EXCEPT ![self] = 0]
                       /\ victim' = [victim EXCEPT ![self] = NoProc]
                       /\ pc' = [pc EXCEPT ![self] = "S240_3_s1"]
                       /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       holding, checked, recoveredAfterCrash, 
                                       tornRead, hostCrashChangedLock, 
                                       touchedUncertain, refusedOk, refused, 
                                       keep, obj_, got, obj, crashes >>

S251_1_delete(self) == /\ pc[self] = "S251_1_delete"
                       /\ IF crashed[self]
                             THEN /\ pc' = [pc EXCEPT ![self] = "clean_end"]
                                  /\ UNCHANGED << fs, holding >>
                             ELSE /\ IF holding[self]
                                        THEN /\ holding' = [holding EXCEPT ![self] = FALSE]
                                             /\ \E c \in FsUnlinkChoices:
                                                  fs' = FsUnlink(fs, P, LockName, c).fs
                                        ELSE /\ TRUE
                                             /\ UNCHANGED << fs, holding >>
                                  /\ pc' = [pc EXCEPT ![self] = "S251_1_close"]
                       /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                       sawLive, seenRec, crashed, live, 
                                       checked, recoveredAfterCrash, tornRead, 
                                       hostCrashChangedLock, touchedUncertain, 
                                       refusedOk, refused, stack, keep, obj_, 
                                       got, obj, robj, victim, crashes >>

S251_1_close(self) == /\ pc[self] = "S251_1_close"
                      /\ IF crashed[self]
                            THEN /\ pc' = [pc EXCEPT ![self] = "clean_end"]
                                 /\ fs' = fs
                            ELSE /\ IF LockObj # NoObj /\ OpenBy(fs, self, LockObj)
                                       THEN /\ fs' = FsClose(fs, self, LockObj).fs
                                       ELSE /\ TRUE
                                            /\ fs' = fs
                                 /\ pc' = [pc EXCEPT ![self] = "clean_end"]
                      /\ UNCHANGED << foreignObj, classified, ownerLive, 
                                      sawLive, seenRec, crashed, live, holding, 
                                      checked, recoveredAfterCrash, tornRead, 
                                      hostCrashChangedLock, touchedUncertain, 
                                      refusedOk, refused, stack, keep, obj_, 
                                      got, obj, robj, victim, crashes >>

clean_end(self) == /\ pc[self] = "clean_end"
                   /\ live' = [live EXCEPT ![self] = FALSE]
                   /\ pc' = [pc EXCEPT ![self] = "Done"]
                   /\ UNCHANGED << fs, foreignObj, classified, ownerLive, 
                                   sawLive, seenRec, crashed, holding, checked, 
                                   recoveredAfterCrash, tornRead, 
                                   hostCrashChangedLock, touchedUncertain, 
                                   refusedOk, refused, stack, keep, obj_, got, 
                                   obj, robj, victim, crashes >>

clean(self) == clean_start(self) \/ S251_1_classify(self)
                  \/ clean_refused(self) \/ clean_recover(self)
                  \/ S251_1_delete(self) \/ S251_1_close(self)
                  \/ clean_end(self)

env_loop == /\ pc["env"] = "env_loop"
            /\ IF crashes < MaxCrashes
                  THEN /\ \/ /\ \E p \in {q \in Procs : live[q] /\ ~crashed[q]}:
                                  /\ fs' = FsProcCrash(fs, p)
                                  /\ crashed' = [crashed EXCEPT ![p] = TRUE]
                                  /\ checked' = [checked EXCEPT ![p] = FALSE]
                                  /\ holding' = [holding EXCEPT ![p] = FALSE]
                                  /\ crashes' = crashes + 1
                             /\ pc' = [pc EXCEPT !["env"] = "env_loop"]
                             /\ UNCHANGED hostCrashChangedLock
                          \/ /\ HostCrashes
                             /\ \E pick \in HostCrashPicks(fs):
                                  \E dirs \in HostCrashDirs:
                                    /\ hostCrashChangedLock' = (hostCrashChangedLock \/ At(FsHostCrash(fs, pick, dirs), P, LockName) # LockObj)
                                    /\ fs' = FsHostCrash(fs, pick, dirs)
                             /\ crashed' = [q \in Procs |-> IF live[q] THEN TRUE ELSE crashed[q]]
                             /\ checked' = [q \in Procs |-> IF live[q] THEN FALSE ELSE checked[q]]
                             /\ holding' = [q \in Procs |-> IF live[q] THEN FALSE ELSE holding[q]]
                             /\ crashes' = crashes + 1
                             /\ pc' = [pc EXCEPT !["env"] = "env_loop"]
                          \/ /\ pc' = [pc EXCEPT !["env"] = "env_done"]
                             /\ UNCHANGED <<fs, crashed, holding, checked, hostCrashChangedLock, crashes>>
                  ELSE /\ pc' = [pc EXCEPT !["env"] = "env_done"]
                       /\ UNCHANGED << fs, crashed, holding, checked, 
                                       hostCrashChangedLock, crashes >>
            /\ UNCHANGED << foreignObj, classified, ownerLive, sawLive, 
                            seenRec, live, recoveredAfterCrash, tornRead, 
                            touchedUncertain, refusedOk, refused, stack, keep, 
                            obj_, got, obj, robj, victim >>

env_done == /\ pc["env"] = "env_done"
            /\ TRUE
            /\ pc' = [pc EXCEPT !["env"] = "Done"]
            /\ UNCHANGED << fs, foreignObj, classified, ownerLive, sawLive, 
                            seenRec, crashed, live, holding, checked, 
                            recoveredAfterCrash, tornRead, 
                            hostCrashChangedLock, touchedUncertain, refusedOk, 
                            refused, stack, keep, obj_, got, obj, robj, victim, 
                            crashes >>

env == env_loop \/ env_done

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == env
           \/ (\E self \in ProcSet:  \/ Classify(self) \/ Acquire(self)
                                     \/ Recover(self) \/ Publish(self))
           \/ (\E self \in Owners: own(self))
           \/ (\E self \in PlainRuns: plain(self))
           \/ (\E self \in Recoverers: rec(self))
           \/ (\E self \in Cleanups: clean(self))
           \/ Terminating

Spec == /\ Init /\ [][Next]_vars
        /\ WF_vars(Next)

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

\* A `Foreign` object at a lock path is never written, renamed, or deleted (Section 7): the one an initial
\* state holds is still the object at the lock path and still holds Foreign. A state predicate, so no
\* misplaced ghost can make it vacuous.
ForeignUntouched == foreignObj # NoObj => LockObj = foreignObj /\ fs.content[foreignObj] = Foreign

\* A REGRESSION GUARD, not a liveness check. TARGET_LOCK_BUSY means the target's lock is held, not that
\* its recorded owner is alive (240.2): a held OS-native lock cannot tell the owner from another
\* invocation inspecting or recovering it, so no refusal-time check can establish owner liveness on
\* that path. What this catches is a refusal with nothing behind it - a decision table that refuses
\* BUSY for a lock it judged dead, or a publisher refusing it at S99_check when the lock path is empty
\* or foreign. The refusing label records its evidence because the refusal may be reported after what
\* it saw has changed: the refusal rests on what the classifier observed, not on a re-read.
RefusalJustified == \A p \in Procs : refused[p] = "TARGET_LOCK_BUSY" => refusedOk[p]

\* The ghost witnesses, for the witness runs.
NeverTornRead == ~tornRead
\* A process passed its Section 99 check, so SingleWriter's ghost is set (design Section 7).
NeverChecked == \A p \in Procs : ~checked[p]
NeverRecoveredAfterCrash == ~recoveredAfterCrash
\* A host crash changed which object the lock path names: an unflushed create, move-aside or removal
\* was undone. Label coverage cannot show this, because the host crash shares `env_loop` with the
\* process crash (design Section 11).
NeverHostCrashChangedLock == ~hostCrashChangedLock

\* The two state witnesses the liveness runs need: the states their properties are about do occur
\* (design Section 7).
NeverDeadOwnerLock == ~DeadOwnerLock
NeverTornLock == ~TornLock

\* ------------------------------------------------------------------------------------------
\* Temporal properties (design Section 7). A configuration that checks one lists exactly one
\* PROPERTY, because TLC reports a temporal violation without naming the property it belongs to.

\* No further crash can happen: the environment has stopped, or has spent every crash it has.
EnvQuiet == pc["env"] \in {"env_done", "Done"} \/ crashes = MaxCrashes

\* Some actor entitled to replace a dead lock (240.3, 251.1) has not started yet and has not been
\* killed, so it will still classify what is at the lock path. A PlainRun is not one: a plain rerun
\* refuses a dead operation's lock with RESUMABLE_OPERATION_EXISTS (21.1).
PendingMover == \E p \in Recoverers \cup Cleanups :
                    /\ ~crashed[p]
                    /\ pc[p] \in {"rec_start", "clean_start"}

\* An actor reported that it could not establish the owner was dead. 240.4 preserves such a lock, so
\* leaving it in place is the protocol working, not failing.
UncertainReported == \E p \in Recoverers \cup Cleanups : refused[p] = "TARGET_LOCK_UNCERTAIN"

\* A lock left behind by an owner that died does not stay there, PROVIDED someone is left to act on it
\* and nothing more can go wrong: while no further crash can occur and an entitled actor has yet to
\* run, the lock is eventually replaced or removed, or an actor reports it cannot tell the owner is
\* dead (design Section 7, lines 565-568). Without those conditions the property is false in every
\* model whose actors all finish, because the last crash can always fall after the last actor has
\* acted: measured, and that counterexample is why they are here.
DeadLockEventuallyCleared ==
    [](DeadOwnerLock /\ EnvQuiet /\ PendingMover => <>(~DeadOwnerLock \/ UncertainReported))

\* The witness for that property's antecedent. Its run must stop with this violated, which is what
\* stops the liveness run from passing over a state space that never reaches the case it is about.
NeverDeadLockWithPendingMover == ~(DeadOwnerLock /\ EnvQuiet /\ PendingMover)
====
