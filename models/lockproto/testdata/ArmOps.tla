---- MODULE ArmOps ----
\* Coverage-shape fixture for cut 6 (design 2026-09-26-cut-6, item 1): an operator in a SECOND module
\* with a LET whose CASE has a dead arm and a constant-level body, like FsModel.tla's FsHostCrash.
EXTENDS Naturals

Dead == [tag |-> "dead"]            \* a constant-level CASE body, like FsModel.tla's Torn

Pick(v, which) ==
    LET out(x) ==
            CASE which = "old" -> x
              [] which = "new" -> x + 1
              [] which = "never" -> 1000
              [] OTHER -> IF Dead.tag = "dead" THEN 7 ELSE 8
    IN out(v)
====
