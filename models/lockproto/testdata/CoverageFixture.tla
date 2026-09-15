---- MODULE CoverageFixture ----
\* Test fixture for the coverage gate (design Section 4): two process sets, one emptied by its
\* cfg (its procedure's label is exempt), a procedure called transitively through a second
\* procedure, a label whose guard never holds (a genuine coverage gap, not exempt), and a
\* process declared with `= VALUE` (always instantiated). Not part of the lock-protocol model;
\* used only by test_run.py's coverage tests. testdata/record_fixtures.py records its TLC output.
EXTENDS Naturals, Sequences

CONSTANTS Alphas, Betas

(* --algorithm CoverageFixture {
  variables
    x = 0;

  procedure OnlyAlpha()
  {
    only_alpha_step:
      x := x + 1;
      return;
  }

  procedure Helper()
  {
    helper_step:
      x := x + 1;
      return;
  }

  procedure CallsHelper()
  {
    calls_helper_step:
      call Helper();
    calls_helper_after:
      x := x + 1;
      return;
  }

  process (a \in Alphas)
  {
    a_step:
      call OnlyAlpha();
    a_done:
      skip;
  }

  process (b \in Betas)
  {
    b_step:
      call CallsHelper();
    b_guarded:
      await FALSE;
      x := x + 1;
  }

  process (env = "env")
  {
    env_step:
      skip;
  }
}
*)
\* BEGIN TRANSLATION (chksum(pcal) = "dee24a61" /\ chksum(tla) = "1e1b5f7e")
VARIABLES x, pc, stack

vars == << x, pc, stack >>

ProcSet == (Alphas) \cup (Betas) \cup {"env"}

Init == (* Global variables *)
        /\ x = 0
        /\ stack = [self \in ProcSet |-> << >>]
        /\ pc = [self \in ProcSet |-> CASE self \in Alphas -> "a_step"
                                        [] self \in Betas -> "b_step"
                                        [] self = "env" -> "env_step"]

only_alpha_step(self) == /\ pc[self] = "only_alpha_step"
                         /\ x' = x + 1
                         /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                         /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]

OnlyAlpha(self) == only_alpha_step(self)

helper_step(self) == /\ pc[self] = "helper_step"
                     /\ x' = x + 1
                     /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                     /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]

Helper(self) == helper_step(self)

calls_helper_step(self) == /\ pc[self] = "calls_helper_step"
                           /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "Helper",
                                                                    pc        |->  "calls_helper_after" ] >>
                                                                \o stack[self]]
                           /\ pc' = [pc EXCEPT ![self] = "helper_step"]
                           /\ x' = x

calls_helper_after(self) == /\ pc[self] = "calls_helper_after"
                            /\ x' = x + 1
                            /\ pc' = [pc EXCEPT ![self] = Head(stack[self]).pc]
                            /\ stack' = [stack EXCEPT ![self] = Tail(stack[self])]

CallsHelper(self) == calls_helper_step(self) \/ calls_helper_after(self)

a_step(self) == /\ pc[self] = "a_step"
                /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "OnlyAlpha",
                                                         pc        |->  "a_done" ] >>
                                                     \o stack[self]]
                /\ pc' = [pc EXCEPT ![self] = "only_alpha_step"]
                /\ x' = x

a_done(self) == /\ pc[self] = "a_done"
                /\ TRUE
                /\ pc' = [pc EXCEPT ![self] = "Done"]
                /\ UNCHANGED << x, stack >>

a(self) == a_step(self) \/ a_done(self)

b_step(self) == /\ pc[self] = "b_step"
                /\ stack' = [stack EXCEPT ![self] = << [ procedure |->  "CallsHelper",
                                                         pc        |->  "b_guarded" ] >>
                                                     \o stack[self]]
                /\ pc' = [pc EXCEPT ![self] = "calls_helper_step"]
                /\ x' = x

b_guarded(self) == /\ pc[self] = "b_guarded"
                   /\ FALSE
                   /\ x' = x + 1
                   /\ pc' = [pc EXCEPT ![self] = "Done"]
                   /\ stack' = stack

b(self) == b_step(self) \/ b_guarded(self)

env_step == /\ pc["env"] = "env_step"
            /\ TRUE
            /\ pc' = [pc EXCEPT !["env"] = "Done"]
            /\ UNCHANGED << x, stack >>

env == env_step

(* Allow infinite stuttering to prevent deadlock on termination. *)
Terminating == /\ \A self \in ProcSet: pc[self] = "Done"
               /\ UNCHANGED vars

Next == env
           \/ (\E self \in ProcSet:  \/ OnlyAlpha(self) \/ Helper(self)
                                     \/ CallsHelper(self))
           \/ (\E self \in Alphas: a(self))
           \/ (\E self \in Betas: b(self))
           \/ Terminating

Spec == Init /\ [][Next]_vars

Termination == <>(\A self \in ProcSet: pc[self] = "Done")

\* END TRANSLATION 
====
