---- MODULE Smoke ----
\* Runner self-test model. It exercises every way run.py judges a run (a check run with
\* a witness and an open finding, its fix-flag run, a liveness run, and a seeded run)
\* without touching the lock protocol. It is not a protocol scenario.
EXTENDS Naturals

CONSTANTS SEED_OVERSHOOT, FIX_BOUND

VARIABLES x, reachedTwo

vars == <<x, reachedTwo>>

Limit == IF FIX_BOUND THEN 3 ELSE 4

Init == x = 0 /\ reachedTwo = FALSE

Step ==
    /\ x < Limit
    /\ x' = x + 1
    /\ reachedTwo' = (reachedTwo \/ x' = 2)

\* The seeded defect: jump past the limit.
Overshoot ==
    /\ SEED_OVERSHOOT
    /\ x = Limit
    /\ x' = Limit + 2
    /\ UNCHANGED reachedTwo

\* Final stuttering step, so a finished run is not a deadlock.
Finished == x >= Limit /\ UNCHANGED vars

Next == Step \/ Overshoot \/ Finished

Spec == Init /\ [][Next]_vars /\ WF_vars(Step)

\* Safety invariant; the seeded run must violate it.
WithinBound == x <= Limit

\* Safety invariant carried as an open finding; FIX_BOUND makes it hold.
AtMostThree == x <= 3

\* Reachability witness: must be reported violated.
NeverReachedTwo == ~reachedTwo

\* Liveness property.
ReachesLimit == <>(x >= Limit)
====
