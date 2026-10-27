# Selection: `--exclude` and `--recursive` / `--no-recursive` (cut 16) - design

Status: DRAFT, 2026-10-27, branch `feat/exclude` (off main 67a03b8f6d9520e6d0cb5cdce7806bfb4b778620).
Owner decisions already taken in conversation (not open): the cut is selection through the shared walker; the `--exclude`
grammar of section 5.1; `--recursive` is the default and `--no-recursive` copies only the top level; skipped
subdirectories stay SILENT. Every other open call is the author's (section 7), for the owner to override.
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
- An `EXCLUDE` record per excluded entry and an excluded counter in the report (call R4).
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
- `*` matches zero or more characters within one component; `?` matches exactly one character within one component (a
  Unicode scalar when the WHOLE name is valid UTF-8 (`OsStr::to_str` succeeds), a byte of its encoded form otherwise; call R2).
- A segment `**` matches whole path components. Interior or leading `**` matches ZERO or more components (`docs/**/n`
  matches `docs/n`). A TRAILING `**` matches ONE or more components, so `private/**` excludes everything inside
  `private` but not `private` itself.
- Anchoring: a pattern with a leading `/` or an inner `/` is ANCHORED and matched against the path relative to the
  source root (components joined by `/`). A pattern with neither is matched against the entry's own name at ANY depth
  (`*.tmp`, `foo`).
- A trailing `/` restricts the pattern to directories; it is stripped FIRST, and the `**` rules then apply to the rest (`a/**/` is a trailing `**`, dir-only). A symlink to a directory is a symlink, not a directory, so `foo/` never matches it. A bare `**` (or `**/`) is a trailing `**`: it matches every entry.
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
recursive) -> Result<Selection, SelectError>`, and `Selection::is_selected(&self, rel: &Path, ft: FileType) -> bool`.
`WalkOptions` gains an optional borrowed handle to it (`Option<&'a Selection>` keeps the struct `Copy`; the plan
decides the lifetime plumbing). `Walk::next` calls it for every entry before `match step` and `continue`s on `false`.
The CLI validates each `--exclude` argument with the same parser (a clap `value_parser`) so a bad pattern fails at
argument time with exit 2; the engine receives `CopyOptions.exclude: Vec<String>` and `CopyOptions.recursive: bool`
(`flux-fs`, raw strings and a bool) and compiles once per run. `VerifyOptions` carries the same two fields or a borrowed
`Selection`; the plan chooses, keeping `Copy` if cheap.

### 5.4 Copy, dry run, resume

All walks start in `prepare_source` (`tree.rs:418`), so passing the selection there covers live, dry and resume runs,
single and several sources. Several sources: the patterns apply to EACH source root, relative to that root. A source that is itself a FILE (single-file source, or a file among several sources) has no relative path and is copied whatever the patterns say (call R7). Nothing in `copy_tree` changes: it never sees an excluded entry. `--exclude 'private/**'` still enters `private`, so an unreadable `private` is a reported failure (exit 1) and the empty `private/` is created; `--exclude private/` avoids both. `files_total` therefore counts selected entries only.

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
- README option list, ROADMAP rows 3, 4 and 6, and `TODO.md:1214` are updated; the stand-downs below get TODO items.

### 5.8 Cleanup folded in

The doc comments at `state.rs:164` (omits `cross_filesystems`) and `run/resume.rs:35` ("the four exact-match options",
six are compared) are corrected where this cut edits those functions.

## 6. Tests (the oracle; written before the code, named)

Each test is pinned by a named case; the plan assigns the exact names. Matcher (`select.rs`, a table test over
(pattern, relative path, kind, excluded?)):
- `private/**`: `private/file` YES, `private` (dir) NO, `private/a/b` YES.
- `private` and `private/` vs dir `private` YES; `private/` vs FILE `private` NO.
- `*.tmp`: `cache.tmp` YES, `sub/deep/cache.tmp` YES; `sub/*.tmp`: `sub/c.tmp` YES, `other/sub/c.tmp` NO.
- `/foo` vs `foo` YES, `a/foo` NO; `foo` vs `a/b/foo` YES.
- `docs/**/notes.txt`: `docs/notes.txt` YES, `docs/2026/10/notes.txt` YES; `build/*` vs `build/x/y` NO.
- `**` alone: every entry YES; `**/foo`: `foo` YES, `a/foo` YES.
- `?`: `caf?` vs `café` YES (one scalar), `caf??` vs `café` NO; a 3-byte (`€`) and a 4-byte (emoji) scalar each match ONE `?`; `caf?` vs the non-UTF-8 name `caf\xff` YES and `??` vs the invalid pair `\xe2\x82` YES (byte mode); a name invalid anywhere is matched in byte mode as a whole.
- Case: `README` vs `readme` NO, and `É` vs `é` NO (no Unicode folding).
- Rejected at compile: `""`, `/`, `a//b`, `..`, `a/../b`, `./a`, `a\b`, `a!b`, `[a]`, `a**`, `***`, `**/**`, `**a`, `a*b**`, a pattern with a newline, a tab, U+0085 or NUL, a 1025-byte pattern, a 65-segment pattern, 65 patterns, 16 KiB + 1 of pattern bytes, and a pathological `a*a*a*...*b` pattern that
  still completes within a bounded step count (linearity).
Walker (`walk.rs`): a SYMLINK and an `Other` entry are excluded by name; an excluded directory at depth 2 (not only at the root) yields no event and the `FaultFs` call log shows no `read_dir` / `metadata`
for it; `private/**` still yields `private` and filters its children; `--no-recursive` yields only top-level
non-directories and does NOT trip the depth cap; selection plus mounts.
Copy (`tree.rs` / run tests): exclusion leaves the destination without the entries; hardlink group with one excluded
member copies the remaining member as an ordinary file (`files_hardlinked == 0`, no degradation, strict mode passes),
two selected members still link, with one case excluding the FIRST-walked (canonical) member and another the last; `files_total` counts selected only; `--no-recursive` creates no subdirectory; dry run
and live agree; several sources apply per root.
Fingerprint / resume: no new option adds no line (the pre-cut fingerprint of a fixed `Options` is pinned byte for byte); a pre-cut manifest (no `exclude`, no `recursive` key) decodes and resumes; the CLI caps keep a manifest under `STATE_LIMIT`; a stored pattern that fails to compile reads as `Corrupt`; `--exclude A --exclude B` equals `--exclude B --exclude A` and duplicates; a resume with a different exclude set
or a different recursion mode exits 3 with `INCOMPATIBLE_STATE` naming the option (one test varies only `exclude`, one only `recursive`).
Verify: an excluded name present at the destination is not `Extra`, and one present at the SOURCE is not `Missing`; the file-versus-directory cases of section 5.6; with `--no-recursive` the destination's
subdirectories are not reported and exit stays 0 on a matching top level; a mismatch at the top level still exits 1.
CLI (`tests/copy.rs`, `tests/verify.rs`): bad pattern exits 2 with the reason and creates NO state and no destination, `--recursive --no-recursive` exits 2,
`--exclude` works end to end for copy and verify, `--resume` without a prior still refuses as before.
Mutants the plan must run by hand: drop the filter call; apply it to files and directories but not symlinks; drop the SOURCE-side (not only the destination-side) filter in verify; sort in `Options::of` but not in `canonical_bytes`; validate the pattern in the engine instead of at argument time; omit `#[serde(default)]` on either field; invert `dir_only`; make trailing `**` zero-or-more;
make `?` a byte; drop `recursive=false` from `canonical_bytes`; drop the destination-side filter in verify.

## 7. Author's calls (owner may override)

- R1. Case-sensitive on every platform, no Unicode normalisation (so an NFC pattern misses an NFD name on macOS). Case-insensitivity is a property of the volume, not the OS; a later `--exclude-ignore-case` can add it.
- R2. `?` is one Unicode scalar for valid UTF-8 names, one byte otherwise.
- R3. Control characters (including newline) are rejected in a pattern, so a fingerprint line is unambiguous.
- R4. OPEN FOR THE OWNER (O1, not yet decided). Both panels found that a typo (`--exclude .tmp` for `*.tmp`) or `--exclude '**'` exits 0 with no hint. The spec's example prints `EXCLUDE private/file` (`:1186`) but no per-entry record exists for success today. The owner's silence ruling (R5) covers skipped subdirectories only. Author's recommendation: ONE counter, `excluded`, the entries the filter removed (a pruned directory counts once, not its subtree), in the `--json` report when non-zero and as a single summary line in the text report only when non-zero; no per-entry record. It costs one `u64` in the walker. Options: (a) that, (b) silent as first written.
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
- Windows: the matcher works on `encoded_bytes` (WTF-8); no `\` is accepted in a pattern, so the separator is `/` on every
  platform. Windows CI is the only evidence for the Windows legs.

## Stand-downs

- `EXCLUDE` record / excluded counter (R4); `--exclude-from`, negation, classes, ignore-case; `--max-depth`; selection
  summary in the `--json` report. Each gets a TODO line in this cut's docs task.
