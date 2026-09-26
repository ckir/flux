# Sequencing after cut 4a: a prep PR, then cut 4b

**Status:** approved by the owner 2026-09-26 (brainstorming, AGY-FIRST consult aligned after one
counter-turn). **Base:** `main` at `d357e67` (PR #48, cut 4a, merged).

## The decision

1. A small **prep PR** lands first (contents below).
2. Then **cut 4b** (`copy_tree`): spec, plan, execution, on the base the prep PR leaves.
3. After 4b merges, **re-run this sequencing decision** rather than fixing the later order now.

In scope for this decision: the `TODO.md` items that touch the copy path or the dev/CI loop. Out of scope:
the lock-model follow-ups (their own track).

The priority the owner set: reach `copy_tree` soon, but carry no known hole into it. An item goes before
4b only if leaving it would make 4b's plan wrong or its execution slower; "good hygiene" is not a reason.

## Why the prep PR exists

- **Stale `TODO.md` entries mislead planning.** MEASURED during this decision: the AGY-FIRST peer's first
  recommendation rested on the entry "`rename_no_replace` is a check-then-rename race", which is fixed on
  every arm (path Unix `renameat_with(NOREPLACE)`, `crates/flux-platform/src/std_fs.rs:316`; path Windows
  `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`, `std_fs.rs:363`; handle Unix
  `crates/flux-platform/src/dir_unix.rs:167`; handle Windows `rename_at(.., false)`). The 4b plan will cite
  `TODO.md`, so its author would hit the same trap.
- **The Linux and macOS gates are hand-run.** Cut 4a's execution ran a WSL gate script and the macOS
  cross-clippy by hand at every task; 4b will run them more often.

## Prep PR contents

1. **`TODO.md` staleness sweep** over every open item in scope (copy path + CI/dev loop). Each entry is
   verified against the code at the branch base and ends in exactly one outcome:
   - **Close** - moved to `## Closed` with the `file:line` or commit that proves it done. Known candidates:
     both `rename_no_replace` race entries (duplicates of each other); the done boxes of "Phase 2 -
     portable copy": the trait surface (`crates/flux-fs/src/fs.rs`, `trait FileSystem`, `trait DirHandle`,
     `trait DestinationRoot`), the platform implementations (`crates/flux-platform/src/std_fs.rs`,
     `dir_unix.rs`, `dir_windows.rs`), single-file copy with metadata (`crates/flux-core/src/copy.rs`,
     `copy_file` / `copy_file_at`), and the CLI wiring (`crates/flux-cli/src/main.rs`, `Commands::Copy`).
     The sweep checks each against the spec's own wording of the box before ticking it: a box whose
     wording asks for more than exists (e.g. every metadata class) is narrowed, not closed.
   - **Narrow** - rewritten to the part still true. Known candidates: "A transient failure and an
     unsupported filesystem both report `Unavailable`" (a failed open now returns the real error;
     `Unavailable` comes only from `GetFileInformationByHandleEx` failing on an open handle,
     `std_fs.rs:766`); "Walker TOCTOU" (writes now go through handles; source reads are still by path,
     `crates/flux-core/src/walk.rs:357`); the "Windows file identity source" spec gap (the code uses
     `FILE_ID_INFO`; whether the SPEC still lacks it is what to check).
     Also narrowed: "A blocking pre-existing temporary is not reported" (`crates/flux-core/src/copy.rs:294`
     is real, but a crashed CLI run's leftover never shares a later run's name - see the 4b table below).
   - **Keep** - left as is, still accurate. Verified live: `destination_is_write_protected`'s
     `Err(_) => false` (`std_fs.rs:83`, `:133`); a previous run's leftover is never removed; `flux copy`
     cannot ask for `Preserve::Off`.
   No entry is closed or narrowed on reasoning alone: each outcome cites what was read.
2. **Local cross-platform gate recipes** in the `justfile`:
   - `just check-linux` - the WSL leg: `cargo clippy --workspace --all-targets -- -D warnings` and
     `cargo nextest run --workspace --no-tests=pass --no-fail-fast`, with a Linux-side `CARGO_TARGET_DIR`
     (a shared Windows target dir would thrash). Distribution `Ubuntu-26.04`.
   - `just check-mac` - `cargo clippy --workspace --exclude flux --all-targets --target
     aarch64-apple-darwin -- -D warnings`. The root crate is excluded because criterion's `alloca` build
     script needs a macOS C toolchain.
   - **Prerequisites checked, not assumed.** `check-mac` needs the `aarch64-apple-darwin` rustup target;
     `check-linux` needs the WSL distribution with `cargo` and `cargo-nextest` on its login `PATH`. Each
     recipe tests for its prerequisite first and, if it is missing, exits non-zero naming the install
     command, rather than failing inside cargo with an unrelated-looking error. A prerequisite that
     `.claude/recommended-tools.json`'s declarative checks (`in_path`, `file_exists`) can express is also
     declared there; a rustup target inside a WSL distribution cannot be, so the recipe's own check is the
     only guard for those.
   - A comment stating what each leg can and cannot catch: the macOS leg compiles and lints only; it runs
     no test, so a macOS runtime difference (e.g. APFS enforcing UTF-8 names) is still CI-only.
   - The "local gate is Windows-only" entry in `TODO.md` is then narrowed to the macOS runtime residue.
3. **`_typos.toml`** excludes the spec by a filename pattern instead of its literal name, so a spec version
   bump does not make the typos job scan the whole spec for the first time.

The exact recipe text (shell, quoting, the prerequisite tests, their messages) is the plan's to give,
written against the `justfile` as it stands; this spec fixes only what each recipe must do.

Process: no plan panel (the plan would be as long as the change). AGY-CAPSTONE on the committed recipes
before `just pr`, since they are executable.

## What cut 4b takes on, and what it does not

**Carried into the 4b spec:**
- The amendment's decisions (item 113's abort on the first "primitive unavailable" publish, the
  fold-collision set, the destination handle stack, create_dir-then-open_dir) carry forward unchanged.

**Out of 4b, each with its reason:**

| Item | Why it stays out |
|---|---|
| Windows guard asks "may I delete", not "may I write" | 4b publishes with mandatory `NoReplace` (amendment, lines 26 and 86); the guard is `Replace`-only |
| Handle/path disagree under an `(RA)`-only deny | same |
| `FaultFs::move_object` strands children | 4b renames only files |
| §42 mount boundaries | its own cut (owner, 2026-09-25); manual 4b testing stays inside temporary directories |
| A previous run's leftover is never removed | needs the §18 operation workspace, which no cut builds |
| A blocking pre-existing temporary is not reported | not naturally reachable: the CLI names temporaries with a per-invocation id, the process id (`crates/flux-cli/src/main.rs:38`), so a crashed run's leftover never shares a name with a later run's. Reachable only on process-id reuse against the same file, or a library caller reusing an id. Becomes live when §18.1 makes ids derivable; the prep sweep narrows the entry to say so (owner, 2026-09-26, after panel round 1) |
| Weak-identity cycle detection | bounded by the depth cap and warned about |
| No test pins the bare-name leftover spelling | owner deferred; needs a working directory in `FaultFs` |
| `flux copy` cannot ask for `Preserve::Off` | a CLI flag - cut 5 |
| `destination_is_write_protected` swallows errors | the path guard; the rename still fails safely |
| `cargo-mutants` false `MISSED` | affects `flux-platform` only; 4b's code lives in `flux-core`, whose lib-test mutants are trustworthy |
| Empty release PRs, `ci.yml` permissions/concurrency, MSRV | cost CI noise, never a wrong 4b plan |

## After 4b

Re-run this decision. Candidates: cut 5 (the CLI surface for trees, including `--safety=strict`), a CI
hygiene batch (the `cargo-mutants` config, the empty-release gate, `ci.yml` permissions and concurrency,
the MSRV question), the §42 cut, and the Windows guard design decision (needs an AGY-FIRST consult on a
write-denial probe that does not hydrate a cloud placeholder).

Also after 4b, placed here so none drops out of the timeline (panel round 1): the open decision "Final
dependency selection"; "Persistent state format" (arrives with the §17/§19 workspace, not before); the
"Scaffolding follow-ups" (install `cargo-mutants`, `lefthook install` per clone, replace the placeholder
`benches/copy.rs`, replace the placeholder `tests/integration/mod.rs` test); and "Repository
housekeeping" (GitHub settings, which nothing in the tree records). The prep sweep may still close any of
them that it finds already done.

**The catch-all rule (panel round 2): every in-scope `TODO.md` entry that is still open after the prep
sweep, and is not placed elsewhere in this document, is AFTER 4b.** The entries the sweep is expected to leave open are
placed below, two of them by an exception the bullet states:

- "Phase 2 - `flux-core`: recursive directory copy" - this IS cut 4b; the 4b PR closes it.
- "Phase 2 - error taxonomy and mapping", "statistics collection", "Integration tests" - after 4b. The 4b
  spec may consume or extend them (a tree outcome counts files), but closing them is not 4b's job.
- "A transient failure and an unsupported filesystem both report `Unavailable`" (narrowed) - after 4b.
- "Walker TOCTOU" (narrowed to source reads) - after 4b. The 4b spec states its source-read stance from
  the walker design and the amendment, and does not change it.
- "Windows file identity source" (narrowed to the spec text) - after 4b; a spec edit, not code.
- "The read-only guard cannot be atomic" - out of 4b for the same reason as the Windows guard items (the
  guard is `Replace`-only; 4b publishes `NoReplace`).

## Consult record

AGY-FIRST brief `.clavity/seams/seq-4b-todo.md`, reply and counter-turn in `.clavity/scratch/seq-4b-todo/`
(local, not committed), agy cascade `dab4b353-0c93-4e13-b2e1-cca9ee269d73`. The peer's first
recommendation (fix `rename_no_replace` before 4b) was REJECTED by measurement; after reading the four
cited functions it withdrew it and revised to the prep-then-4b sequence recorded here, moving the
`cargo-mutants` and release-PR items after 4b. Its point that `NoReplace` keeps both Windows guard items
out of 4b was CONFIRMED against the amendment.
