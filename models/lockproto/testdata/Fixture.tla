---- MODULE Fixture ----
\* Recorded-output fixture for test_run.py: each case in record_fixtures.py runs TLC on
\* this module (or Broken.tla) and saves the output, so the parser tests need no Java.
EXTENDS Naturals

VARIABLE x

Init == x = 0

Step == x < 3 /\ x' = x + 1

Stop == x = 3 /\ UNCHANGED x

Spec == Init /\ [][Step \/ Stop]_x /\ WF_x(Step)

NeverTwo == x /= 2

NeverThree == x /= 3

NotZero == x /= 0

Bounded == x <= 3

EventuallyFive == <>(x = 5)

EvalError == <<1, 2>>[x + 5] = 1
====
