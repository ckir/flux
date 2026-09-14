---- MODULE Broken ----
\* Recorded-output fixture: a module TLC cannot parse.
EXTENDS Naturals

VARIABLE x

Init == x = 0

Next == x' = x + + 1
====
