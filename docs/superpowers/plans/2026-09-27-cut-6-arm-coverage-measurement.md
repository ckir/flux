# Cut 6, item 1: what TLC's coverage actually reports for arms inside actions

Findings from `models/lockproto/testdata/arm_coverage.out` (2221 lines, recorded by
`testdata/record_fixtures.py`'s `arm_coverage` case against `ArmFixture.tla` + `ArmOps.tla`),
answering Task 1's six questions. All line numbers below are lines of `arm_coverage.out` itself
unless noted; all quoted text is verbatim from that file. Source-text substrings (in quotes,
labelled "source") were looked up directly in the committed `ArmFixture.tla` / `ArmOps.tla` by
line/col, to interpret what a coverage line refers to - they are not TLC output and are not
guesses at what TLC "would" print.

## Q1: Does each arm of `branchy` get its own node, with the dead arm (`i = 7`, body `x := 99`)
at 0 and the live arms > 0?

Yes. `branchy`'s translated `IF`-ladder (source `ArmFixture.tla:46-52`) reports one node per
guard and one node per body:

```
54:  line 46, col 20 to line 46, col 24 of module ArmFixture: 3      (source: "i = 0")
57:  line 47, col 25 to line 47, col 33 of module ArmFixture: 1      (source: "/\ x' = 1")
60:  line 48, col 31 to line 48, col 35 of module ArmFixture: 2      (source: "i = 1")
63:  line 49, col 36 to line 49, col 44 of module ArmFixture: 1      (source: "/\ x' = 2")
66:  line 50, col 42 to line 50, col 46 of module ArmFixture: 1      (source: "i = 7")
69:  line 51, col 50 to line 51, col 56 of module ArmFixture: 0      (source: "x' = 99")
72:  line 52, col 47 to line 52, col 55 of module ArmFixture: 1      (source: "/\ x' = 3")
```

The dead arm's BODY node (line 51, `x' = 99`) is exactly 0. The three live arms' body nodes
(lines 47, 49, 52 - `x' = 1`, `x' = 2`, `x' = 3`) are each 1, i.e. > 0.

## Q2: Are an arm's GUARD and BODY separate nodes, and which is 0 for the dead arm?

For `branchy`'s three real comparisons (`i = 0`, `i = 1`, `i = 7`), yes - guard and body are
reported as two distinct line/col spans, as shown in Q1: e.g. the `i = 7` guard is its own node
(line 66, count 1) and the `x' = 99` body is a separate node (line 69, count 0). The GUARD is
non-zero (1: the guard was reached and evaluated, and evaluated false); the BODY is the one that
is 0.

This guard/body split is not universal, though - it depends on the arm's body shape. In `ArmOps`'s
`Pick` (module ArmOps, Q5 below), the "old" and "new" arms split guard from body the same way:

```
141:  |||||line 10, col 18 to line 10, col 30 of module ArmOps: 9   (source: 'which = "old"', guard)
144:  |||||line 10, col 35 to line 10, col 35 of module ArmOps: 3   (source: "x", body)
147:  |||||line 11, col 18 to line 11, col 30 of module ArmOps: 6   (source: 'which = "new"', guard)
150:  |||||line 11, col 35 to line 11, col 39 of module ArmOps: 3   (source: "x + 1", body)
```

but the "never" and `OTHER` arms, whose bodies are a bare literal (`1000`) or a body with no free
state variables, are reported as ONE combined guard+body node instead of two:

```
153:  |||||line 12, col 18 to line 12, col 40 of module ArmOps: 3   (source: 'which = "never" -> 1000')
156:  |||||line 13, col 18 to line 13, col 60 of module ArmOps: 3   (source: 'OTHER -> IF Dead.tag = "dead" THEN 7 ELSE 8')
```

So the split is per-arm-shape, not guaranteed for every arm - see Q5 for why that matters.

## Q3: Under `with (i \in ...)`: are counts per value of `i`, or per action?

Per value of `i` (equivalently: per element the bounded quantifier enumerates), not once per
action invocation. `branchy` fires once (1 source state, one `pc = "branchy"` state), yet the
guard counts step down as the ladder is walked once per candidate value of `i \in {0, 1, 2}`:
`i = 0` is checked 3 times (line 54, once per candidate value of `i`), `i = 1` is checked 2 times
(line 60, once for each of the 2 remaining values after `i = 0` is excluded), and `i = 7` is
checked 1 time (line 66, once for the 1 remaining value after `i = 0` and `i = 1` are excluded).
If counts were per-action rather than per-value, every guard under the same `\E`/`with` would
report the same count (matching the action header's own count); instead they step down
3, 2, 1 - one evaluation per surviving candidate value.

## Q4: `twins`: both bodies yield the same successor. Does each body node count > 0?

Yes. `twins`'s two live arms (`j = 0` and `j # 0`, source `ArmFixture.tla:58-62`) both set
`y' = 5` - literally the same successor value - and TLC still gives each body its own non-zero
count:

```
93:  line 58, col 18 to line 58, col 22 of module ArmFixture: 6     (source: "j = 0", guard)
96:  line 59, col 23 to line 59, col 31 of module ArmFixture: 3     (source: "/\ y' = 5", body of j=0)
99:  line 60, col 29 to line 60, col 33 of module ArmFixture: 3     (source: "j # 0", guard)
102:  line 61, col 34 to line 61, col 42 of module ArmFixture: 3     (source: "/\ y' = 5", body of j#0)
105:  line 63, col 37 to line 63, col 42 of module ArmFixture: 0     (source: "y' = y", the translator's own unreachable ELSE-fallback, since j \in {0,1} already covers both arms)
```

Both `y' = 5` bodies (lines 96 and 102) report 3, not 0 - an arm whose body produces only
duplicate successors (already reached via the other arm, from the same source state) still counts.
As a bonus, the translator's own synthesized catch-all `ELSE /\ TRUE /\ y' = y` (unreachable here
since `j = 0 \/ j # 0` is exhaustive) is the file's other observed dead arm, at 0 (line 105) -
confirming Q1's finding independently.

## Q5: `Pick`'s `CASE` in module `ArmOps`: does each arm body get a node, is `"never"`'s 0, and
does the `OTHER` body (built from the constant `Dead`) get a node?

Every arm gets a node, but `"never"`'s is NOT 0, and it does not mean what a bare 0/non-0 reading
would suggest - this is the measurement's most important result. The four arms of
`CASE which = "old" -> x [] which = "new" -> x + 1 [] which = "never" -> 1000 [] OTHER -> IF
Dead.tag = "dead" THEN 7 ELSE 8` (source `ArmOps.tla:10-13`), as reported under the one call site
that gets a node (see Q6):

```
141:  |||||line 10, col 18 to line 10, col 30 of module ArmOps: 9   ('which = "old"' guard)
144:  |||||line 10, col 35 to line 10, col 35 of module ArmOps: 3   ("x" body of "old")
147:  |||||line 11, col 18 to line 11, col 30 of module ArmOps: 6   ('which = "new"' guard)
150:  |||||line 11, col 35 to line 11, col 39 of module ArmOps: 3   ("x + 1" body of "new")
153:  |||||line 12, col 18 to line 12, col 40 of module ArmOps: 3   ('which = "never" -> 1000', guard+body FUSED)
156:  |||||line 13, col 18 to line 13, col 60 of module ArmOps: 3   ('OTHER -> IF Dead.tag = "dead" THEN 7 ELSE 8', guard-marker+body FUSED)
```

`"never"`'s node (line 153) is 3, not 0 - even though `which` is only ever `"old"`, `"new"` or
`"other"` in this fixture (never the string `"never"`), so the literal body `1000` never actually
contributes to any successor. The count of 3 comes entirely from the GUARD being reached and
evaluated (and failing) 3 times, once for each of the calls where `which = "other"` - it is not
evidence the body ran, because for this arm shape TLC does not give the guard and the body
separate nodes (contrast the "old"/"new" arms in Q2, which do split guard from body). This is
exactly the ambiguity the spec's item 1 warned about: a non-zero count here does NOT mean the
body executed.

The `OTHER` arm (line 156, also 3) is different: `OTHER` has no guard that can fail once reached
(it is the CASE's unconditional fallthrough), so for this arm specifically a non-zero count does
mean its body - the constant-referencing `IF Dead.tag = "dead" THEN 7 ELSE 8` - ran. So: yes, the
constant-level body gets a node, and it is non-zero (3), but it is fused with the arm's own
guard-marker into a single span rather than split like `Q2`'s regular arms.

## Q6: `two_sites` calls `Pick` twice: are `ArmOps`'s nodes printed once per call site, or once in
total?

Neither, cleanly. `two_sites` calls `Pick` at two source spans on the same line - `Pick(x, w)`
(source `ArmFixture.tla:69`, cols 24-33) and `Pick(y, "old")` (same line, cols 37-50):

```
119:  line 69, col 19 to line 69, col 50 of module ArmFixture: 9    ("z' = Pick(x, w) + Pick(y, "old")")
122:  |line 69, col 24 to line 69, col 50 of module ArmFixture: 9   ("Pick(x, w) + Pick(y, "old")")
125:  ||line 69, col 24 to line 69, col 33 of module ArmFixture: 9  ("Pick(x, w)" - call site 1)
128:  |||line 14, col 8 to line 14, col 13 of module ArmOps: 9      ("out(v)" - Pick's body, nested under call site 1)
  ... (Q5's ArmOps nodes, all nested under this same call site 1)
158:  |||line 69, col 29 to line 69, col 29 of module ArmFixture: 6 ("x", the argument at call site 1)
161:  |||line 69, col 32 to line 69, col 32 of module ArmFixture: 9 ("w", the argument at call site 1)
164:  ||line 69, col 37 to line 69, col 50 of module ArmFixture: 9  ("Pick(y, "old")" - call site 2)
```

`ArmOps`'s internal nodes (line 14's `out(v)`, and everything nested under it - all of Q5's arm
nodes) appear exactly ONCE in the whole file (verified: `grep -n "ArmOps" arm_coverage.out` finds
only the module-loading lines plus these 8 nested lines), all nested under call site 1
(`Pick(x, w)`, line 125). Call site 2 (`Pick(y, "old")`, line 164) is a leaf in the printed tree -
it has no nested children of its own at all, even though it invokes the exact same operator body
in `ArmOps` on every one of its 9 firings (always with `which = "old"`).

This is neither "printed once per call site" (that would show a second, parallel `out(v)` /
`CASE` hierarchy nested under line 164) nor "summed in total" (the "old" arm's body node, line
144, would then have to be at least 12 - the 3 firings where call site 1 has `w = "old"` plus the
9 firings where call site 2 always has `which = "old"` - but it is 3). What is observed is that
the operator's internal cost nodes are attached to only ONE of the two call sites in the source,
and reflect only that call site's own invocation count; the second call site's executions of the
identical operator body leave no trace in this recording at all.

## Consequences (spec item 1, "Each outcome has a consequence")

The spec states four consequences. Against what this recording shows:

- **"if arms get no nodes of their own, or a count does not mean the body ran, items 2, 3 and 6's
  union closures return to the owner"** - TRIGGERED. Every arm does get a node (Q1, Q5), but Q5
  shows a non-zero count that does NOT mean the body ran: the `"never"` arm's fused guard+body
  node is 3 while its body is provably unreachable in this fixture. Any arm whose body has no
  free state variables (a bare literal, or - per this recording - anything simple enough that TLC
  fuses its guard and body into one span) is at risk of the same ambiguity. Items 2, 3 and 6's
  union closures go to the owner.
- **"if a duplicate-successor arm does not count, item 2 returns to the owner"** - NOT triggered.
  Q4 shows both of `twins`'s identical-bodied, duplicate-successor arms count (3 and 3, both > 0).
  Item 2 does not need to return to the owner on this ground.
- **"if a constant body gets no node, item 6's torn arm returns to the owner"** - NOT triggered on
  its own terms: Q5 shows the constant-referencing `OTHER` arm DOES get a node (count 3, non-zero).
  (It returns to the owner anyway under the first consequence above, since that arm's neighbor -
  the "never" arm - demonstrates a count that does not reliably mean "body ran".)
- **"if per-call-site nodes print separately, their counts are summed per source span"** - its
  precondition does not hold as stated: Q6 shows per-call-site nodes do NOT print separately (only
  one call site gets any `ArmOps`-nested nodes at all), so there is nothing to sum. This is a
  cannot-determine-cleanly case for the stated consequence itself: the recording shows a THIRD
  behavior the spec's two anticipated outcomes ("once in total" vs "printed separately") didn't
  cover - one call site's contribution is visible, the other's is silently absent. This goes to the
  owner as a new finding, not a match for either of the spec's two anticipated branches.

Net: items 2, 3 and 6's union closures, and the multi-call-site question, go to the owner. Item
2's duplicate-successor concern and item 6's "does a constant body get a node at all" question are
each resolved in the fixture's favor, but item 6 is still gated by the first consequence.
