# Selection: `--exclude` and `--recursive` / `--no-recursive` (cut 16) - design

Status: DRAFT (panel rounds 1-4 folded), 2026-10-27, branch `feat/exclude` (off main 67a03b8f6d9520e6d0cb5cdce7806bfb4b778620).
Owner decisions already taken in conversation (not open): the cut is selection through the shared walker; the `--exclude`
grammar of section 5.1; `--recursive` is the default and `--no-recursive` copies only the top level; skipped
subdirectories stay SILENT; ONE `excluded` counter reports what `--exclude` removed (call R4). Every other open call is the author's (section 7), for the owner to override.
Peer consults (AGY-FIRST): `.clavity/seams/next-cut-fork-6.md` (order: selection, then several-source
`--restart`/`--dry-run`, then progress), `.clavity/seams/exclude-grammar-fork.md`, `.clavity/seams/recursive-fork.md`.
Prior art: `TODO.md:1214`, `ROADMAP.md:43,45`, spec Sections 5 (options, `:685-700`), 14 (`:1162-1198`), 15, 19
(`:1482`), 121 (`:5801-5802`).

## 1. Intent

`flux copy` and `flux verify` select everything under the source root. The spec lists `--exclude <PATTERN>` and
`--recursive` / `--no-recursive` among the copy options that `verify` shares and that a resume must agree on, and the
0.1 list names "hardlink exclusions" (`:3600`). Both are usage errors today. This cut adds them to the shared walker so
that copy, dry run, resume and verify select the same set, and so that Phase 3's bounded queues later sit behind an
already-filtered stream.

## 2. Goals, non-goals, anti-goals

Goals
- A mistyped or over-broad `--exclude` is visible: the report carries ONE `excluded` count (section 5.9).
- `--exclude PATTERN` (repeatable) and `--recursive` / `--no-recursive` on `flux copy` (single source, several sources,
  `--dry-run`, `--resume`) and on `flux verify` (one and several sources).
- One predicate in the walker decides selection, so that every consumer sees only selected entries. An excluded
  directory is never entered (no `read_dir`).
- Hardlink topology is considered among selected entries only (spec `:495-504`, `:1189-1197`, `:3385-3387`): the walk
  never yields an excluded member, so it never registers, never becomes canonical and never forces a copy.
- A resume refuses when the selection differs (spec `:1482`), and every pre-existing state keeps its fingerprint.
- `flux verify` applies the same selection predicate to BOTH sides, so an excluded or not-descended name at the destination is not reported `extra` (the predicate is per entry and kind-dependent, section 5.6).

Non-goals
- Negation (`!`), character classes (`[...]`), `--exclude-from FILE`, `--exclude-dir` / `--exclude-file`, `--max-depth`.
  Reserved syntax is rejected now so a later release can add it without changing a working command line.
- An `EXCLUDE` record per excluded entry, or any per-entry listing of what was excluded (call R4 chose a count only).
- Parallel workers, bounded queues, a planner (Phase 3 remainder). A matcher dependency crate (call R10).
- `--cross-filesystems` and `--links` are unchanged; they already exist.

Anti-goals
- A selection that lets a `read_dir` happen under an excluded directory, or that builds a global list of selected paths
  (spec `:511`).
- A change to any existing option's behaviour or to any existing fingerprint.

## 3. Facts this design rests on (read at 67a03b8f6d9520e6d0cb5cdce7806bfb4b778620)

- The walker is one pull iterator for copy and verify. `Walk::next` takes the next entry and joins its name onto the
  frame's relative path at `crates/flux-core/src/walk.rs:326`; the dispatch `match step` is at `walk.rs:330-347`
  (`File`, `Symlink`, `Other` return an event; `Dir` calls `enter`). `enter` does the depth check (`walk.rs:366`), the
  metadata call, the mount check, the ancestor push and `read_dir` (`walk.rs:379-453`). A filter between `walk.rs:326` and
  `walk.rs:330` that `continue`s the loop skips a non-directory, and skips a directory before ANY I/O on it.
- `WalkOptions { max_depth, mounts }` is `Copy` (`walk.rs:82-86`); `walk_with` destructures it exhaustively
  (`walk.rs:204`). Its constructors: `walk.rs:185`, `walk.rs:194`, `tree.rs:426` (`prepare_source`), `verify.rs:174`
  (`Side::new`), and tests in `walk.rs`.
- `max_depth` is an ERROR tripwire, not a prune: exceeding it yields a `WalkError` (`walk.rs:366-376`) which the copy
  reports as a failure (`tree.rs:1059-1064`). `--no-recursive` therefore must NOT be built on it.
- Hardlink canonical choice is the first path the walk yields that reaches `hardlink_group` (`tree.rs:1737`,
  `meet_group` `tree.rs:2454-2492`). An entry the walk never yields never reaches it. `files_total` counts the
  non-directory entries the walk yielded (`tree.rs:54-56`), so a walk-level skip leaves excluded entries out of it.
- Options travel as `CopyOptions` (`crates/flux-fs/src/options.rs:119`, `Clone`, not `Copy`) from
  `options(&CopyArgs)` (`crates/flux-cli/src/main.rs:265`) through `run::tree` / `run::trees`, `prepare_source`
  (`run/mod.rs:320`, `run/multi.rs:476`). `VerifyOptions` is `Copy` (`verify.rs:108-113`), built at `main.rs:739`.
- Fingerprint: `Options` (`crates/flux-core/src/state.rs:94-125`, `deny_unknown_fields`) is built by `Options::of`
  (`state.rs:128-160`); `canonical_bytes` (`state.rs:165-179`) adds `links=skip` and `cross_filesystems=true` only when
  non-default. `validate` compares six values and refuses with `INCOMPATIBLE_STATE` (`run/resume.rs:104-120`), exit 3
  (`flux-cli/src/exit_code.rs:76`). `Config::new` takes an `Options` VALUE (`state.rs:197-200`), so adding fields leaves its ~15 call sites alone; only literal `Options { .. }` constructions break, and the compiler names every one (the plan's FILES clause lists them).
- `STATE_LIMIT` is 64 KiB: `decode` rejects a longer manifest as `Corrupt` (`state.rs:34`, `state.rs:477-478`) and the read is capped at the same size (`state.rs:936`); nothing checks the size when the state is WRITTEN.
- Precedent for an optional report counter: `mounts_skipped` (`crates/flux-cli/src/report.rs:55`, absent from JSON at zero, test at `report.rs:1029`); verify prints `not_entered` only when non-zero (`crates/flux-cli/src/verify_report.rs:135-136`).
- Verify merges two sorted walks and reports `Missing`, `Extra`, `Mismatched`, `Unreadable`, `Match`
  (`verify.rs:626-648`); extras print but never fail the run (`verify.rs:101-104`). The destination `Side` takes a
  `ControlFilter` (`verify.rs:617`).
- Flag parsing: clap derive structs `CopyArgs` (`main.rs:99-183`) and `VerifyArgs` (`main.rs:63-83`). `--exclude` is
  unknown to clap today; the tests that pin that are `main.rs:1030-1036` and `crates/flux-cli/tests/verify.rs:168-175`
  and must change. `Cli::parse()` exits 2 on a parse error.
- The report prints no per-entry record for a successful entry; no `EXCLUDE` or `COPY` token exists in the code. The
  spec's example output (`:1183-1187`) is illustrative of intent, not of a shipped format.
- Stale doc comments met on the way (fixed in this cut, section 5.8): `state.rs:164`, `run/resume.rs:35`.

## 4. Approaches

A. A selection predicate inside the walker, consulted before `match step`. Prunes before any I/O; copy, dry run, resume
   and verify get it for free; hardlinks need no change. Cost: `WalkOptions` must carry a handle to the compiled
   selection, and `VerifyOptions` (`Copy`) must as well.
B. The consumers filter (`tree.rs`, `verify.rs` each skip events). Rejected: `read_dir` has already happened for an
   excluded directory, two consumers duplicate the logic, and a skipped `Dir` event must be matched by its `DirEnd`.
C. A pre-pass producing the selected list. Rejected: spec `:511` forbids an unbounded global list.
Chosen: A.

## 5. Design (approach A)

### 5.1 The `--exclude` grammar (owner-approved)

```
pattern     = [ "/" ] segment { "/" segment } [ "/" ]
segment     = "**" | component
component   = unit { unit }                      (* never exactly "**", never "." or ".." *)
unit        = literal | star | "?"
star        = "*"                                (* not adjacent to another "*" inside a component *)
literal     = any Unicode scalar except "/" "\" "*" "?" "!" "[" "]" and any control character
```

The production is the LEXICAL shape; the rules after "Meaning" decide validity and are authoritative where the two seem
to differ (`***`, `a**`, `**x`, `.`, `..` and `**/**` are all rejected by them). A "control character" is Unicode
category Cc: U+0000-U+001F and U+007F-U+009F (NUL, newline and tab included). Sizes below are UTF-8 BYTES.

Meaning
- `*` matches zero or more characters within one component; `?` matches exactly one character within one component (a Unicode scalar when the component being matched is valid UTF-8 (`OsStr::to_str` succeeds), a byte of its encoded form otherwise, decided once per component; call R2).
- A segment `**` matches whole path components. Interior or leading `**` matches ZERO or more components (`docs/**/n`
  matches `docs/n`). A TRAILING `**` matches ONE or more components, so `private/**` excludes everything inside
  `private` but not `private` itself.
- Anchoring: a pattern with a leading `/` or an inner `/` is ANCHORED and matched against the path relative to the
  source root (the pattern's `/` separates its segments; the PATH is walked component by component, see 5.3). A pattern with neither is matched against the entry's own name at ANY depth
  (`*.tmp`, `foo`).
- A trailing `/` restricts the pattern to directories; it is stripped FIRST, and the `**` rules then apply to the rest (`a/**/` is a trailing `**`, dir-only). A symlink to a directory is a symlink, not a directory, so `foo/` never matches it. A bare `**` (or `/**`) is a trailing `**` and matches every entry; `**/` is the same pattern made dir-only, so it matches every DIRECTORY below the root (each is pruned) and leaves top-level files alone.
- Anchoring is decided AFTER the trailing `/` is stripped, so `foo/` is unanchored and dir-only (`foo/` matches the directory `a/b/foo`, not the file). An anchored pattern must match the WHOLE relative path, not a prefix. `*` and `?` match a leading `.` (there is no hidden-file rule).
- The entry name an unanchored pattern sees is the LAST component of its relative path.
- Matching is case-sensitive on every platform and does no Unicode normalisation (call R1).
- An entry is excluded when ANY pattern matches it. Order is irrelevant. A matched directory is never entered.
- A pattern is a usage error (exit 2, at argument parsing) when it is empty, equals only `/`, has an empty segment
  (`a//b`), a `.` or `..` segment, a character in the excluded set above, a `**` that is not a whole segment (`a**`, `**x`, `***`), two adjacent `**` segments, more than 1024 bytes, or more than 64 segments. ACROSS the command line at most 64 patterns and 16 KiB of pattern bytes are accepted (section 5.5 says why). A non-UTF-8 argument is rejected by clap (exit 2). A pattern that starts with `-` is written `--exclude=-foo`; `--help` says so. The message names the pattern and
  the reason; a backslash gets "use `/` as the separator".
- A literal `!`, `[`, `]` or `\` cannot be written yet (reserved syntax); a name containing one is reached with `?` or `*` (`report?1?.txt`), and `--help` says so.
- Cost: one match is linear in `pattern x name` (iterative, no backtracking blow-up). Per run the cost is bounded by `entries x patterns x (path length x pattern length)` with the depth cap (256) and the caps above; the matcher takes a step counter in tests so the bound is asserted, not timed.

### 5.2 `--recursive` / `--no-recursive`

`recursive` defaults to true. `--recursive` is an explicit synonym of the default; the two flags together are a usage
error (clap `conflicts_with`). With `--no-recursive` the walker treats every `Dir` entry as excluded: only the regular
files, symlinks and other entries directly inside the source root are yielded, no subdirectory is created and none is
entered, silently (owner ruling). A symlink to a directory is `FileType::Symlink` and copies as a symlink under
`--links=copy`, never followed. A single-file source ignores both flags. The depth cap is untouched.

### 5.3 Where it lives

New `crates/flux-core/src/select.rs`: `Selection` (compiled patterns, `recursive: bool`), `Selection::compile(&[String],
recursive) -> Result<Selection, SelectError>`, and `Selection::select(&self, rel: &Path, ft: FileType) -> Verdict`, where `Verdict` is `Keep`, `Excluded` (a pattern matched) or `Skipped` (`--no-recursive` dropped a directory). Patterns are tested FIRST, so an entry both a pattern and `--no-recursive` would remove is `Excluded` and counts (call R11). The walker `continue`s on both removals and counts only `Excluded`. `rel` is the `PathBuf` that `Walk::next` builds with `join` (`walk.rs:326`): its separator is the OS one (`\` on Windows), with no prefix and no leading separator. The matcher therefore walks `rel.components()` (`Component::Normal` names) and NEVER splits the path's bytes on `/`; treating `\` as a second separator would be wrong on Linux, where a name may contain one. Per entry it decides the byte-or-scalar mode once per component, takes only `file_name()` for unanchored patterns, and returns early when there are no patterns and recursion is on (the walker then holds `None`). `Selection` stays `Sync` (Phase 3 shares it); the test-only step count comes from a counted generic entry point, not a `Cell`.
`WalkOptions` gains an optional borrowed handle to it (`Option<&'a Selection>` keeps the struct `Copy`; the plan
decides the lifetime plumbing). `Walk::next` calls it for every entry before `match step`, and `continue`s on `Excluded` and `Skipped`.
The CLI validates each `--exclude` argument with the same parser (a clap `value_parser`) so a bad pattern fails at
argument time with exit 2; the engine receives `CopyOptions.exclude: Vec<String>` and `CopyOptions.recursive: bool`
(`flux-fs`, raw strings and a bool) and compiles once per run. `VerifyOptions` carries the same two fields or a borrowed
`Selection`; the plan chooses, keeping `Copy` if cheap.

### 5.4 Copy, dry run, resume

All walks start in `prepare_source` (`tree.rs:418`), so passing the selection there covers live, dry and resume runs,
single and several sources. Several sources: the patterns apply to EACH source root, relative to that root. A source that is itself a FILE (single-file source, or a file among several sources) has no relative path and is copied whatever the patterns say (call R7). No per-entry logic in `copy_tree` changes: it never sees an excluded entry (only the count's plumbing does, section 5.9). `--exclude 'private/**'` still enters `private`, so an unreadable `private` is a reported failure (exit 1) and the empty `private/` is created; `--exclude private/` avoids both. `files_total` therefore counts selected entries only.

### 5.5 Resume fingerprint

`Options` gains `exclude: Vec<String>` (sorted, deduplicated at construction; `skip_serializing_if` empty) and
`recursive: bool`. BOTH carry `#[serde(default)]` (`exclude` defaults to empty, `recursive` through a function to `true`), because `Options` is `deny_unknown_fields`: without the default every pre-cut manifest would fail with a missing field and read as `Corrupt`. `exclude` is `skip_serializing_if` empty and `recursive` `skip_serializing_if` true. `canonical_bytes` appends one `exclude=<pattern>\n`
line per pattern in sorted order, then `recursive=false\n`, each only when non-default, after the existing lines, so
every state written before this cut keeps its hash. `validate` compares both and refuses with `INCOMPATIBLE_STATE`
(exit 3) naming the option. Because a pattern can never contain a control character (section 5.1), a line cannot be forged by one. The 64-pattern / 16 KiB caps keep the manifest well under `STATE_LIMIT` (64 KiB), which nothing checks on write; a manifest the run could write but never read back would be an unresumable run. `decode` recompiles every stored pattern, and one that fails is `Corrupt`. The refusal message shows the stored and requested lists, each joined with a space. A state written with a non-default selection is unreadable by a pre-cut binary (`deny_unknown_fields`); that has precedent in cuts 10a and 10b, and a default-selection state stays byte-identical and readable by one.

### 5.6 `flux verify`

`VerifyArgs` gains the same three flags. The selection is applied to BOTH walks in `Side::new` (`verify.rs:174`, called
at `verify.rs:607` and `verify.rs:617`), so a destination entry that is excluded, or lies below the first level under `--no-recursive`, is neither `Extra` nor compared (call R6). The predicate looks at each entry's OWN kind, so it can select the two sides of one name differently: a source FILE `foo` against a destination DIRECTORY `foo` under the dir-only pattern `foo/` (or under `--no-recursive`) reports `Missing` for `foo`, not a type mismatch, and the mirror case reports `Extra`. That is by design and pinned by a test. `verify_file` ignores the options as today. Several sources:
each source root is walked with the selection, as in copy.

### 5.7 CLI and docs

- `--exclude <PATTERN>` (repeatable, `ArgAction::Append`), `--recursive`, `--no-recursive` on `CopyArgs` and `VerifyArgs`,
  with `--help` text stating the grammar in a few lines.
- The two tests that pin "`--exclude` is a usage error" (`main.rs:1030-1036`, `tests/verify.rs:168-175`) keep their
  `--resume` half and drop `--exclude`.
- Docs and tests that become false and are updated in this cut: `README.md:90` (verify synopsis) and `README.md:105-106` (option prose); the verify GUARANTEES text `crates/flux-cli/src/verify_report.rs:26-30` (a directory not entered is no longer only a mount: add the `--no-recursive` and `--exclude` guarantee that selected-out entries are not compared); ROADMAP rows 3, 4 and 6 (`ROADMAP.md:42,43,45`); `TODO.md:1214`; the two tests that pin `--exclude` as an error (`main.rs:1030-1036`, `tests/verify.rs:168-175`); clap `value_name = "PATTERN"` so `--help` shows `--exclude <PATTERN>`; the stand-downs below get TODO items.

### 5.8 Cleanup folded in

The doc comments at `state.rs:164` (omits `cross_filesystems`) and `run/resume.rs:35` ("the four exact-match options", six are compared today and eight after this cut) are corrected where this cut edits those functions.

### 5.9 The `excluded` count (owner decision, call R4)

What it counts. The entries the `--exclude` patterns removed, counted ONCE at the point the walker drops an entry: a pruned
directory is 1 (its subtree is never visited), a skipped file, symlink or other entry is 1, an entry matched by two patterns
is 1. Entries removed by `--no-recursive` do NOT count (owner ruling: silent). The count is "entries excluded among those
the run reached": a root that never started (`NotStarted`, `tree.rs:779-828`) contributes 0, and exclusions inside a subtree
whose directory failed still count, as `files_total` does. It is never persisted: not in `Options`, the manifest, the
fingerprint or the WAL; a resume recounts it from its own walk.

Plumbing (copy). `walk_into` (`tree.rs:1043`, "every `?` here is an abort") owns the `Walk` and drops it on every return, so
the count cannot be added after its loop. Instead `walk_into` takes `&mut Walk` and `copy_tree_frames` (`tree.rs:950`) adds
`out.excluded += events.excluded()` immediately after `let walked = walk_into(..)` (`tree.rs:971`), before `drain_mode`.
`out` is the `TreeOutcome` every caller moves into `TreeAbort { error, outcome }`, so an abort carries the count reached so
far. `copy_roots_at` needs no extra sum (`out` is shared across roots). A root that fails before its walk starts
(`tree.rs:706`, `:850`, `:961`, `run/mod.rs:1486`) contributes 0. The tests pin the abort path.

Plumbing (verify). `Side::new` (`verify.rs:174`) builds both sides alike; the verify engine reads the SOURCE side's count before `verify_root` returns and ignores the destination side's; `Side::empty()` (no walk) reports 0. Exclusions beneath a subtree verify has already pruned still count (the walk keeps running), consistent with "entries the run reached". `Summary` gains `excluded`. It is folded field by field at `verify.rs:540-548` (and in the
test fold at `verify.rs:2372` and the proptest model's own running sum at `crates/flux-core/src/verify_proptest.rs:329`), which is NOT compiler-visible, so the plan lists every fold in
its FILES clause and a two-root test pins the sum.

Report. Copy JSON: `excluded: u64` after `mounts_skipped` (so `mounts_skipped < excluded < hardlinks_degraded`, pinned beside `report.rs:2380`), `skip_serializing_if` zero, so the pinned 18-key set
(`report.rs:~946`, `tests/copy.rs:288,315`) holds when it is zero. Verify JSON: `excluded` in `SummaryBody` after `not_entered` and before `bytes_hashed`,
ALSO omitted at zero (unlike `not_entered`, which is always present), so `SUMMARY_KEYS` and `SEVERAL_SUMMARY_KEYS`
(`tests/verify.rs:55,562`) hold at zero; with a non-zero count the key is present and the key-order test names its position.
Text: a `; N excluded` suffix, only when non-zero, on `summary_line` (`report.rs:478`, the LAST entry of its suffix loop, after `mismatched`),
on `dry_run_summary_line` (`report.rs:499`, appended at the end of the line) and on verify's summary line (`verify_report.rs:~135`, after the
`mount points not entered` clause). `RootJson` and `root_lines` (several sources) gain NO per-root count; the resume lines are
unchanged. `files_total`, `files_skipped` and every other counter keep their meaning (`excluded` never leaks into
`files_skipped`). A typo pattern that matches nothing therefore shows no `excluded` anywhere, and `--exclude '**'` shows the
number of top-level entries removed.

## 6. Tests (the oracle; written before the code, named)

Each test is pinned by a named case; the plan assigns the exact names. Matcher (`select.rs`, a table test over
(pattern, relative path, kind, excluded?)):
- `private/**`: `private/file` YES, `private` (dir) NO, `private/a/b` YES.
- `private` and `private/` vs dir `private` YES; `private/` vs FILE `private` NO.
- `*.tmp`: `cache.tmp` YES, `sub/deep/cache.tmp` YES; `sub/*.tmp`: `sub/c.tmp` YES, `other/sub/c.tmp` NO.
- `/foo` vs `foo` YES, `a/foo` NO; `foo` vs `a/b/foo` YES.
- `docs/**/notes.txt`: `docs/notes.txt` YES, `docs/2026/10/notes.txt` YES; `build/*` vs `build/x/y` NO.
- `**` alone and `/**`: every entry YES; `**/`: directory `d` YES, file `f` NO; `**/foo`: `foo` YES, `a/foo` YES; `foo/` vs directory `a/b/foo` YES and vs file `a/b/foo` NO; `*` vs `.hidden` YES; `sub/*.tmp` vs `sub/x.tmp/y` NO (whole path, not prefix).
- Cost: `a/**/a/**/a/**/b` against a deep non-matching path stays within the step bound (a naive recursive matcher fails it).
- Limits accepted exactly at the cap: a 1024-byte pattern, 64 segments, 64 patterns, 16 KiB of pattern bytes.
- `?`: `fil?` vs `filé` YES (one scalar), `fil??` vs `filé` NO; a 3-byte (`€`) and a 4-byte (emoji) scalar each match ONE `?`; `fil?` vs the non-UTF-8 name `fil\xff` YES and `??` vs the invalid pair `\xe2\x82` YES (byte mode); a name invalid anywhere is matched in byte mode as a whole.
- Case: `README` vs `readme` NO, and `É` vs `é` NO (no Unicode folding).
- Rejected at compile: `""`, `/`, `a//b`, `..`, `a/../b`, `./a`, `a\b`, `a!b`, `[a]`, `a**`, `***`, `**/**`, `**a`, `a*b**`, a pattern with a newline, a tab, U+0085 or NUL, a 1025-byte pattern, a 65-segment pattern, 65 patterns, 16 KiB + 1 of pattern bytes, and a pathological `a*a*a*...*b` pattern that
  still completes within a bounded step count (linearity).
Walker (`walk.rs`): a SYMLINK and an `Other` entry are excluded by name; an excluded directory at depth 2 (not only at the root) yields no event and the `FaultFs` call log shows no `read_dir` / `metadata`
for it; `private/**` still yields `private` and filters its children; `--no-recursive` yields only top-level non-directories and does NOT trip the depth cap, and a top-level symlink to a directory is yielded as a symlink (and copied as one under `--links=copy`); an excluded directory deeper than the cap raises no depth error; the root is never filtered; selection plus mounts.
Count (`excluded`): a pruned directory with a large subtree counts 1; a file, a symlink and an `Other` entry excluded by name count 1 each; one entry matched by two patterns counts 1; two excluded entries in one directory count 2 (not `= 1`); `--no-recursive` alone counts 0 and `--no-recursive` with a pattern counts only the pattern's removals; no pattern, or a typo pattern that matches nothing, leaves `excluded` absent from the JSON and off every summary line (JSON AND text asserted); `--exclude '**'` counts the top-level entries INCLUDING a directory; it is added to `files_skipped` never; summed over two roots where the FIRST excludes and the second does not, and where both do (kills `=` for `+=`); a directory removed by BOTH a pattern and `--no-recursive` counts 1 (patterns first), and a top-level symlink to a directory under `--no-recursive` counts 0; verify with an absent destination (`Side::empty`) and a non-zero source count; one exclusion in the root directory plus one in a subdirectory at depth 2 or more sum (kills a per-frame counter); an abort mid-walk after an excluded entry still reports the count; a failed first root leaves later roots at 0; an excluded directory that is the only entry counts 1 with `files_total` 0; exclusions inside a failed subtree count; a file operand adds 0; dry run, live and resume report the same NON-ZERO number (resume with already-completed files must not add `files_resumed`); verify counts source-side removals only with ASYMMETRIC sides (source 1, destination 2), sums two roots, shows the clause on the text summary and the key in JSON only when non-zero; the `a_summary_json_has_exactly_the_eleven_fields` literal at `verify_report.rs:339` and the `Report::zero()` literal (`report.rs:104-127`) are updated.
Copy (`tree.rs` / run tests): exclusion leaves the destination without the entries; hardlink group with one excluded
member copies the remaining member as an ordinary file (`files_hardlinked == 0`, no degradation, strict mode passes),
two selected members still link, with one case excluding the FIRST-walked (canonical) member and another the last; `files_total` counts selected only; `--no-recursive` creates no subdirectory; dry run
and live agree; several sources apply per root.
Fingerprint / resume: no new option adds no line (the pre-cut fingerprint of a fixed `Options` is pinned byte for byte); a pre-cut manifest (no `exclude`, no `recursive` key) decodes and resumes; the CLI caps keep a manifest under `STATE_LIMIT`; a stored pattern that fails to compile reads as `Corrupt`; `--exclude A --exclude B` equals `--exclude B --exclude A` and duplicates; a resume with a different exclude set
or a different recursion mode exits 3 with `INCOMPATIBLE_STATE` naming the option (one test varies only `exclude`, one only `recursive`).
Verify: an excluded name present at the destination is not `Extra`, and one present at the SOURCE is not `Missing`; the file-versus-directory cases of section 5.6; with `--no-recursive` the destination's
subdirectories are not reported and exit stays 0 on a matching top level; a mismatch at the top level still exits 1.
CLI (`tests/copy.rs`, `tests/verify.rs`): `--exclude=-foo` is accepted and matches a name `-foo` while `--exclude -foo` is refused; `--recursive` alone is accepted; `flux copy file.txt dst --exclude file.txt` still copies the file (R7), as does a file operand among several sources, and `verify_file` ignores the options; `--help` shows `<PATTERN>`, the grammar and the R7 note; the `INCOMPATIBLE_STATE` text names the stored and requested lists; bad pattern exits 2 with the reason and creates NO state and no destination, `--recursive --no-recursive` exits 2,
`--exclude` works end to end for copy and verify, `--resume` without a prior still refuses as before.
Mutants the plan must run by hand: drop the filter call; apply it to files and directories but not symlinks; drop the SOURCE-side (not only the destination-side) filter in verify; sort in `Options::of` but not in `canonical_bytes`; validate the pattern in the engine instead of at argument time; omit `#[serde(default)]` on either field; invert `dir_only`; make trailing `**` zero-or-more;
make `?` a byte; drop `recursive=false` from `canonical_bytes`; drop the destination-side filter in verify.

## 7. Author's calls (owner may override)

- R1. Case-sensitive on every platform, no Unicode normalisation (so an NFC pattern misses an NFD name on macOS). Case-insensitivity is a property of the volume, not the OS; a later `--exclude-ignore-case` can add it.
- R2. `?` is one Unicode scalar for valid UTF-8 names, one byte otherwise.
- R3. Control characters (including newline) are rejected in a pattern, so a fingerprint line is unambiguous.
- R11. Precedence: patterns are tested before `--no-recursive`, so an entry both would remove counts once as `Excluded` (the user's pattern did match it); pure recursion removals never count.
- R4. (Owner decision: option (a).) One `excluded` count, no per-entry record and no `EXCLUDE` line; the spec's example output (`:1186`) is illustrative and no per-entry success record exists today. Counted once per removal, a pruned directory as one, `--no-recursive` removals not counted (R5). Verify counts the source side only (section 5.9).
- R5. (Owner ruling.) Skipped subdirectories under `--no-recursive` are silent.
- R6. Verify applies the selection to both sides, so nothing selected-out shows as `extra`.
- R7. On a single-file source, and on a file among several sources, `--exclude` and the recursion flags are accepted and ignored: an operand the user named explicitly is their own selection and has no path relative to a source root. `--help` says so. (rsync applies the pattern to the name; the alternative is to match unanchored patterns against it.)
- R8. A trailing `**` means "one or more" (gitignore's rule), so `private/**` keeps the directory itself; the empty
  `private/` is therefore created at the destination.
- R9. Specifying `--hardlinks=preserve` (strict) with an exclusion is not a failure: the spec requires topology only among
  selected members (`:1189-1197`), and a lone selected member is an ordinary copy.
- R10. A hand-written matcher in `flux-core`, no new dependency (a ~150-line module; the matcher crates were not
  adopted, `Cargo.toml` pin list is a candidate list only).

## 8. Self-audit (gaps and where they close)

- Walker `Copy` constraint: a borrowed handle keeps it; if the plan finds the lifetime plumbing spreads, an `Arc<Selection>`
  makes `WalkOptions` `Clone` and the few `Copy` uses change. Decided in the plan, pinned by the compiler.
- `MountSkipped` / `MountUnknown` notes are produced inside `enter` for directories that pass the filter; an excluded
  mount directory yields no note (nothing was looked at). Pinned by a walker test.
- The root itself is never filtered (`--exclude .` is rejected; `**` cannot match the root).
- Depth: an excluded-but-too-deep directory no longer raises the depth `WalkError` (the filter runs first); intended.
- `Config::new` / `Options { .. }` literals in tests need the two new fields: the compiler names every one (FILES clause
  for the plan lists them).
- Windows: the walker's `rel` uses `\`; the matcher reads `components()`, so a `/`-separated pattern segment meets a component, not a byte (section 5.3). A name with a lone surrogate fails `to_str` and is matched in byte mode on its WTF-8 form (`?` then matches one WTF-8 byte). Section 6's table fixtures build `rel` with `PathBuf::join`, and one walker test uses an anchored multi-component pattern at depth 2, so Windows CI exercises the real input; it is the only evidence for the Windows legs.

## Stand-downs

- A per-entry `EXCLUDE` record or listing (R4); `--exclude-from`, negation, classes, ignore-case; `--max-depth`; a per-pattern or per-entry selection summary in the `--json` report. Each gets a TODO line in this cut's docs task.

## Panel record (AGY-AFTER)

Rounds 1 and 2 (each: a solo Sonnet panel and the agy peer), then one negotiation turn on the Windows separator.
- FOLDED (round 1): wrong citations; `#[serde(default)]` on both new `Options` fields; the 64-pattern / 16 KiB cap against
  `STATE_LIMIT`; `decode` recompiling stored patterns; the EBNF restated; `?` rows; kind-dependent verify predicate;
  unreadable `private`; single-file operands; extra mutants and rows.
- FOLDED (round 2): the matcher reads `rel.components()` (`walk.rs:326` builds `rel` with the OS separator); `**/` versus
  bare `**`; anchoring after the trailing `/`; per-component `?` mode; cost mechanics and the `Sync` step counter; the
  stale-docs list; missing test rows.
- NEGOTIATED: agy's round-2 fix (treat `0x2F` and `0x5C` both as separators) was REJECTED with a measured reason, a Linux
  name may contain `\` (`crates/flux-fs/src/name.rs:19-22`, `crates/flux-core/src/walk.rs:176-179`: `is_one_component`
  accepts it on Linux), so `a\b` would match `a/b`. agy re-read those lines and withdrew it; both sides now hold the
  `components()` mechanism.
- DISCARDED-BELOW-FLOOR: the clap error echoes the rejected pattern including a control character (stderr cosmetic, no
  state is touched before argument parsing); case-sensitive matching surprising users on NTFS (decided, R1).
- FOLDED (round 3, solo): the count's path out of the consumed walk (including abort), the verify fold with no compiler oracle,
  the verify and copy JSON zero-omission against the pinned key sets, the summary-line suffix and dry-run line, per-root
  representation, the meaning under a failed root, count tests. agy round 3 (complete, ALIGNED) found only the same
  `Summary` literal and per-root points as minor.
- FOLDED (round 4, solo): the count is added in `copy_tree_frames` after `walk_into` returns, with `walk_into` taking `&mut Walk` (`walk_into` drops the walk on every return); `Verdict` tri-state and R11; the `=`/`+=` and both-rules test rows; key-order pins; roots failing before the walk count 0.
- ROUND 5 (solo): NO LIVE CHALLENGE; folded its minor items (stale `false` wording, a cross-directory sum row, the `:961` cite, pruned-subtree note).
- OWNER DECISION (R4): one `excluded` counter (section 5.9).
