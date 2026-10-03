---- MODULE ArmFixture ----
\* Coverage-shape fixture for cut 6 (design 2026-09-26-cut-6, item 1). Not part of the lock-protocol
\* model; testdata/record_fixtures.py records its TLC output as arm_coverage.out.
EXTENDS Naturals, ArmOps

(* --algorithm ArmFixture {
  variables
    x = 0, y = 0, z = 0;

  process (p = "p")
  {
    branchy:
      with (i \in {0, 1, 2}) {
        if (i = 0) { x := 1; }
        else if (i = 1) { x := 2; }
        else if (i = 7) { x := 99; }
        else { x := 3; };
      };
    twins:
      with (j \in {0, 1}) {
        if (j = 0) { y := 5; }
        else if (j # 0) { y := 5; };
      };
    two_sites:
      with (w \in {"old", "new", "other"}) {
        z := Pick(x, w) + Pick(y, "old");
      };
  }
}
*)
\* BEGIN TRANSLATION (chksum(pcal) = "8ea06453" /\ chksum(tla) = "cc0a044")
VARIABLES x, y, z, pc

vars == << x, y, z, pc >>

ProcSet == {"p"}

Init == (* Global variables *)
        /\ x = 0
        /\ y = 0
        /\ z = 0
        /\ pc = [self \in ProcSet |-> "branchy"]

branchy == /\ pc["p"] = "branchy"
           /\ \E i \in {0, 1, 2}:
                IF i = 0
                   THEN /\ x' = 1
                   ELSE /\ IF i = 1
                              THEN /\ x' = 2
                              ELSE /\ IF i = 7
                                         THEN /\ x' = 99
                                         ELSE /\ x' = 3
           /\ pc' = [pc EXCEPT !["p"] = "twins"]
           /\ UNCHANGED << y, z >>

twins == /\ pc["p"] = "twins"
         /\ \E j \in {0, 1}:
              IF j = 0
                 THEN /\ y' = 5
                 ELSE /\ IF j # 0
                            THEN /\ y' = 5
                            ELSE /\ TRUE
                                 /\ y' = y
         /\ pc' = [pc EXCEPT !["p"] = "two_sites"]
         /\ UNCHANGED << x, z >>

two_sites == /\ pc["p"] = "two_sites"
             /\ \E w \in {"old", "new", "other"}:
                  z' = Pick(x, w) + Pick(y, "old")
             /\ pc' = [pc EXCEPT !["p"] = "Done"]
             /\ UNCHANGED << x, y >>

p == branchy \/ twins \/ two_sites

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == p
           \/ Terminating

Spec == Init /\ [][Next]_vars

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 
====
