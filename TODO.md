# TODO

Near-term work. Release-level scope lives in [ROADMAP.md](ROADMAP.md).

## Open decisions

- [ ] **Final dependency selection** (spec §67). `[workspace.dependencies]` pins
      versions for the candidate crates; `clap` and `thiserror` are now adopted by
      member crates (`crates/flux-cli/Cargo.toml`, `crates/flux-fs/Cargo.toml`), and
      `walkdir`/`jwalk` were decided against (see the walker decision above). Still
      undecided and depended on by no member crate: `rayon`, `crossbeam-channel`,
      `blake3`, `indicatif`, `tracing`, `tracing-subscriber`, `anyhow`, `serde`,
      `serde_json`. Each must be judged on performance, correctness, portability,
      maintenance, licensing and API stability before adoption.
      (Narrowed 2026-09-26: `clap`/`thiserror` adopted and `walkdir`/`jwalk`
      rejected already; the remaining nine candidates are unchanged.)
- [x] **Directory walker — DECIDED 2026-09-23: no third-party walker.** Add a
      single-level `read_dir` primitive to `flux_fs::FileSystem`, returning one
      level of entries WITH their file type, and implement the ordered
      depth-first walk in `flux-core` over an explicit stack. Negotiated with
      the agy peer; both positions converged, and the decisive argument was the
      peer's. Reasoning, all of it measured rather than recalled:

      - **§7.2's ordering needs no crate and no global sort.** The spec says "a
        depth-first scanner that sorts each directory's entries by name alone
        emits component-wise order without buffering". Verified against the
        spec's own normative example: DFS with per-directory byte sorting gives
        `a/x  a/y/z  a-b  a0`, the component-wise order, while flat `/`-joined
        byte sorting gives `a-b  a/x  a/y/z  a0`, exactly the order §7.2 warns
        is wrong. Per-directory sorting is sufficient.
      - **`walkdir` is out because it cannot see our trait.** Measured in
        walkdir 2.5.0 source: `pub struct WalkDir` carries no filesystem
        generic, `new<P: AsRef<Path>>` is generic only over the path type, and
        `src/lib.rs:114` and `:909` bind it to `std::fs::read_dir`. Using it in
        the engine would bypass `FaultFs` and leave recursive copy untestable by
        the fault-injection harness the rest of the suite depends on.
      - **`jwalk` is out twice over.** Its own crates.io description reads "Use
        `dua-core` instead" and it has not moved since 0.9.0, failing §67's
        maintenance criterion. Worse, it is a PARALLEL walker: parallel
        traversal yields entries in completion order, restoring §7 order then
        needs a reorder buffer, and spec line 511 says "the scanner never
        creates an unbounded global list of discovered" entries. It fights the
        spec rather than merely failing to help.
      - **Parallelism belongs on the copy, not the walk.** Traversal is
        metadata-bound and must be ordered anyway. Sequential ordered discovery
        feeding a parallel worker pool over a bounded queue is the shape spec
        lines 508-511 describe.
      - **FD exhaustion is a non-issue**, contrary to my initial worry: sorting a
        directory forces collecting its entries into a `Vec`, so the directory
        handle drops before recursing and only one is ever open.

      Two risks this decision does NOT solve, and no crate would have:

- [x] **Symlink-loop detection for the walker — RESOLVED by the design of
      2026-09-23, and it was the wrong shape of worry.** The default walk never
      follows a symlink (§25, §26), and descending only on `is_dir()` gives that
      for free on both platforms, so no symlink loop is reachable at all. What IS
      reachable without any symlink is a bind mount: MEASURED on WSL, after
      `mount --bind a a/b` the path `a/b` is not a symlink (`-L` no), is a
      directory (`-d` yes), and `stat` reports `dev=120 ino=13` for both `a` and
      `a/b`. The design handles it with an ancestor set of object identities
      rather than `dev`/`ino` tracking of everything visited, which would grow
      with the tree and breach spec line 997.
- [ ] **A transient failure and an unsupported filesystem both report `Unavailable`.**
      `identity_of` on Windows returns `FileIdentity::Unavailable` when the open fails
      - a sharing violation, a denial, a path that just vanished - and also when the
      volume does not implement the `FileIdInfo` class at all. A caller cannot tell "this
      filesystem never has identity" from "I could not read this one right now", so a
      transient error silently downgrades cycle detection for that entry instead of being
      reported. Bounded by the depth cap and the lexical containment test, so not unsafe,
      but misleading: the operation would warn about filesystem capability when it
      actually hit a locked file. Fixing it means deciding what the walker should DO with
      the difference, so it belongs with the walker rather than here.
      (Narrowed 2026-09-26: the Windows `metadata` open failing now returns the real
      error rather than folding into `Unavailable` — `std_fs.rs`'s `metadata` fn's
      `.open(path).map_err(FsError::from_io)?`; `Unavailable` comes only from
      `GetFileInformationByHandleEx` failing on an already-open handle, `std_fs.rs:766`.
      The ambiguity above is narrower than stated: it is confined to that one call.)
- [ ] **Identity-based cycle detection is unavailable on weak-identity filesystems**
      (§107 names FAT32, exFAT and some SMB and NFS configurations). Negotiated to
      a bounded degradation rather than a refusal: the lexical containment test and
      the depth cap both still run, so the residual harm is a cycle copied up to
      256 times — bounded, warned about once per filesystem, and cleanable — instead
      of an unbounded silent loop. `--safety=strict` refuses outright. What stays
      unmet on weak identity is §149.6's "even if filesystem namespace relationships
      change after startup", since only identity can catch containment established
      through a mount or a rename. A real fix needs a trustworthy identity on those
      filesystems, which is where this item lives.
- [ ] **Walker TOCTOU.** If `read_dir` yields bare paths, the walk inherits the
      same races `walkdir` has: an entry can change type between the listing and
      the visit. Mitigated by returning the file type WITH the entry so no
      re-stat is needed; a full fix wants `openat`-style directory-relative
      operations, which `std` does not expose.
      (Narrowed 2026-09-26: the destination side of a copy now writes through
      directory handles, not paths, since PR #44 / #48. What remains is the
      source side: the walk still reads by path — `crates/flux-core/src/walk.rs:357`,
      `self.fs.read_dir(&abs)` — so the race is confined to source reads.)
- [ ] **Persistent state format** for the topology store and operation manifest
      (spec §17, §19). Must scale past RAM and survive a crash mid-write.
- [ ] Repository housekeeping: enable GitHub private vulnerability reporting (see
      [SECURITY.md](SECURITY.md)), and decide whether `main` gets branch protection.
      `UNVERIFIABLE` 2026-09-22: both halves are GitHub settings, which nothing in the tree
      records, so no closure here could be re-checked from the repository alone. Observed at the
      time: `required_status_checks.strict` is `true`, so branch protection is on.

## Phase 2 — portable copy (next)

- [x] `flux-fs`: define the portable filesystem trait surface — done:
      `crates/flux-fs/src/fs.rs:106` `trait FileSystem`, `:223` `trait DirHandle`,
      `:299` `trait DestinationRoot`
- [x] `flux-platform`: Linux / macOS / Windows implementations behind it — done:
      `crates/flux-platform/src/std_fs.rs`, `dir_unix.rs`, `dir_windows.rs`; CI's
      three test-matrix legs (`.github/workflows/ci.yml`, `Test (${{ matrix.os }})`)
- [x] `flux-core`: single-file copy with metadata preservation — done:
      `crates/flux-core/src/copy.rs` `copy_file` / `copy_file_at`, metadata applied
      at steps 5-6 before publish
- [x] `flux-core`: recursive directory copy — done: `copy_tree` (cut 4b, `crates/flux-core/src/tree.rs`), exposed by `flux copy` in cut 5.
- [x] `flux-core`: error taxonomy and mapping (spec §68.1 lists error mapping as
      a required unit test) — done: `crates/flux-fs/src/error.rs` `Code` enum,
      `classify`/`from_io` (lines 80/91), unit-tested for `DiskFull` / `PermissionDenied`
      / `IoError` (lines 127, 133, 147)
- [ ] `flux-core`: statistics collection
- [x] `flux-cli`: wire `flux copy` to the above — done: a file (cut 1) and a folder's contents
      (cut 5): `crates/flux-cli/src/main.rs` `Commands::Copy`, logic in
      `crates/flux-cli/src/{resolve,report,exit_code}.rs`
- [ ] Integration tests: single file, directory, nested directory, multiple
      sources, zero-byte files, Unicode, spaces, newlines (spec §68.2)

### Known limits of the first single-file copy (2026-09-22)

- [ ] **`destination_is_write_protected` swallows every `symlink_metadata` error**

  Both arms read `Err(_) => false`, and the comment justifies only the NotFound case — "Nothing
  occupies the name, so there is nothing to protect", which is correct and is the dominant case. A
  sharing violation or a denial also lands there, and the guard then reports "not protected" for a
  destination it could not inspect. The operation still fails safely, because the rename itself fails;
  what is lost is the precise refusal, so the user gets the rename's error instead of "the destination
  is write-protected".

  Found by the PR 1 capstone widening its lens beyond that PR's range. Not fixed there because the
  function is PR #32's code and untouched by PR 1.

- [x] **A leftover temporary from a PREVIOUS run is never removed**

  `<target>.flux-partial.<operation-id>` is named with a per-invocation id (the pid), so the leftover
  sweep only ever removes this invocation's own temporary — which, with an id that is never persisted,
  means it removes nothing at all. §18.1 wants an id "deterministic enough for discovery"; that needs
  operation state to record it, which this cut does not have. A crashed run therefore leaves a
  temporary that nothing collects.

  DONE in cut 7a: each operation's id is persisted in its state, and `flux copy --restart` deletes every
  `<name>.flux-partial.<id>` of the operation it supersedes (the design's `--restart`, step 3).

- [ ] **`flux copy` cannot ask for `Preserve::Off`**

  `CopyOptions` has three preservation states and `copy_file` honours all three, but the CLI only
  ever builds `Strict` (when `--preserve-times` is given) or `Default`. The spec's flags mean
  "strict when present" and define no `--no-preserve-*`, so there is no way from the command line to
  say "do not attempt metadata at all". `Off` is reachable only by a library caller in this cut, and
  is tested as one.

- [x] **The self-copy refusal compares paths, not filesystem identity**

  §2 Foundational Invariants item 22: *"Safety checks use filesystem identity and object identity where
  available, not only lexical path comparisons."* `copy_file` refuses only when `src == dst` as paths,
  so `flux copy a ./a`, or a copy through a hardlink or a junction, is not refused. It is not
  destructive — the copy is staged in a distinct temporary and the published bytes are the source's
  own — but the invariant asks for identity and this cut does not supply it.

  Blocked on the same primitive the lock protocol needs: `dev`+`ino` on Unix is one call, Windows needs
  `FILE_ID_INFO`, which is already an open item above. Do both at once.

  DONE in cut 4a: `copy_file_at`'s Step 2a gate refuses a destination that is the source by identity;
  the lexical Step 0 refusal stays in front of it.

## Known gaps in the single-file copy (from the PR #32 capstone)

Each was measured, and each is deliberately NOT fixed in that PR.

- [ ] **A blocking pre-existing temporary is not reported.** If the step-1 leftover
      sweep fails and `create_new` then fails with `AlreadyExists`, `copy_file` returns
      `leftover: None` even though a temporary genuinely sits at the path and is
      blocking the copy. The caller is told nothing was left behind.
      (Narrowed 2026-09-26: `crates/flux-cli/src/main.rs` `options` names each temporary with
      `std::process::id()`, so a crashed run's leftover never shares a later run's name
      unless the OS reuses that pid against the same target — reachable, but narrower
      than stated; it becomes live more broadly once §18.1 gives operations a
      persisted, derivable id.)
      (Cut 7a: the operation id is now persisted and per operation, so a blocking temporary names an operation
      whose state finds it; but the copy still returns `leftover: None` in this case, so the reporting gap stays
      open.)
- [ ] **The read-only guard cannot be atomic.** `rename_replace` checks whether this
      process may replace the destination and then renames; a permission change landing
      in between is not seen, and `rename` is precisely what does not consult the file.
      `std::fs::rename` takes paths rather than the handle probed with, and neither
      platform offers "rename only if I may replace the target". `cp` has the same
      window. Documented in the code; recorded here so it is not rediscovered.
- [ ] **The Windows read-only guard asks "may I delete", not "may I write".** A file whose ACL
      denies only `(WD,AD)` cannot be opened for writing by this user, yet BOTH `rename_replace`
      forms replace it (measured 2026-09-26, cut 4a test audit: the write open fails with code 5, both
      renames succeed and the contents are gone). The POSIX twin refuses a file the user may not
      write (`accessat W_OK`), so the platforms diverge, and `destination_is_write_protected`'s doc
      comment ("asking the SAME question as the Unix half") is false for this ACL. The DELETE probe is
      deliberate (`crates/flux-platform/src/std_fs.rs`: rename needs DELETE; a write-intent probe may
      hydrate a cloud placeholder), so fixing it is a design decision: needs an AGY-FIRST consult on a
      write-denial probe that does not hydrate. Owner ruled 2026-09-26: track, fix later.
- [ ] **Handle and path `rename_replace` disagree when an ACL denies only read-attributes.** Under a
      deny of `(RA)` alone, the handle form is refused by `NtSetInformationFile` (`0xC0000022`) and
      reported as `IoError`, while the path form replaces the file (measured 2026-09-26). The user
      CAN open that file for writing, so the path outcome matches the "may write" rule and the handle's
      refusal is spurious and mislabelled (`rename_at` maps every `NtSetInformationFile` failure to
      `IoError`). Rare ACL; decide it together with the item above.
- [ ] **No test pins the leftover's spelling for a bare-name destination.** `copy_file` rebuilds a
      leftover temporary's path from `dst` (`dst.with_file_name`), so `flux copy a b` reports
      `b.flux-partial.<id>`, not `./b...`. Replacing that with `parent_path.join` leaves every test green
      (measured, cut 4a test audit): the only leftover test uses `/dst`, where the two agree. `FaultFs`
      refuses relative paths by design, so the test needs a working directory in the fake or a
      real-filesystem fixture that can make removal fail. Owner deferred 2026-09-26.
- [x] **A tree copy never replaces an existing destination file — DONE in cut 8b (branch `spec/cut-8b`, no PR yet).**
      Was: `copy_tree` published every file no-replace and reported an existing one
      `DESTINATION_NAMESPACE_COLLISION`, untouched (cut 4b, F3), because §241.5's replacement claim needs the
      durable `state.db` of the operation workspace. Now a folder copy replaces an existing destination file by
      default (§5.1's `--overwrite`), under a claim recorded in `state.db`; `--update` and `--skip-existing`
      select the other two policies. Design: `docs/superpowers/specs/2026-10-07-cut-8b-replacement-design.md`.
      The lockless `copy_tree` stays no-replace. Open follow-ups: "Cut 8b known limits" and "Cut 8b debt".
- [ ] **`flux copy --json`'s `bytes_total` counts copied bytes only.** The walk never stats a file, so
      a failed file's size is unknown; a pre-scan would double metadata I/O (owner, cut 5). `--help`
      says so.
- [ ] **A skipped special file is reported as `object_type=special`.** The walk types it `Other` from
      `read_dir` and does not say FIFO, socket or device (§233.1 wants the object type).
- [ ] **Several sources are a usage error.** §4.1's last row and §18.3 need the operation workspace;
      `flux copy` takes exactly two positionals until then (cut 5).
- [ ] **A dangling destination link is exit 3 on Linux only by measurement.** macOS's `open_dir` answer
      for a link (ENOTDIR vs ELOOP) was not measured; if ELOOP, the same run is `IO_ERROR`, exit 1 -
      safe, but not §55's refusal code (cut 5 plan, refinement 3).
- [ ] **Symlinks are never recreated.** §25's default copies a symlink AS a link; this version has no
      link-creating primitive, so every symlink in a tree fails with `SYMLINK_CREATION_UNAVAILABLE` (exit
      1) and a symlink given as SOURCE is refused before the engine (K7). Needs a `symlink` operation on
      the filesystem trait (Windows: file vs directory links, and the privilege they need) (cut 5).
- [ ] **No progress output during a long copy.** `flux copy` prints each record as it happens and one
      summary at the end; a large tree runs silently in between. §52's progress UI (files, bytes, speed,
      ETA, skipped, degraded, errors) is not built; it needs the walk to know the totals up front (cut 5).

## Engine (walker cut 4) prerequisites

Cut 4 is split (see `docs/superpowers/specs/2026-09-25-cut-4-after-handle-relative-writes.md`); both
entries below are cut 4b's.

- [x] **Item 113's up-front no-replace probe — DONE in cut 8a (`d236fd7`).** A directory operation
      against a destination with no no-replace publication primitive is refused with
      `NOREPLACE_PUBLISH_UNAVAILABLE` (exit 3) before anything changes (spec item 113, probe at §241.5).
      The probe (`probe_no_replace`, `crates/flux-core/src/run/place.rs`) stages `noreplace-probe.tmp`
      inside `<id>.creating`, the unpublished workspace, and publishes it onto `noreplace-probe` with
      `rename_no_replace`; any failure that `primitive_unavailable` classifies refuses the run, never an error
      kind match (the real WSL 9p answer is `InvalidInput`, the fake's is `Unsupported`). It sits inside
      `<id>.creating` because `publish_workspace`'s own `rename_no_replace` would otherwise stop the run first.
      The first-publish backstop in `copy_tree` stays.
- [ ] **`FaultFs::move_object` strands a renamed directory's children.** It re-keys only the exact
      `from` and `to` paths; nothing walks the `from/` prefix, so every child keeps its old path. That is
      the stage-then-publish shape `copy_tree` will use, so an engine test that stages a tree and
      publishes it by rename would observe a wrong tree. Fix with the fake's move to node-id keys, which
      §149.7's handle-relative writes (PR #44) also need. DECIDED 2026-09-25: not needed in cut 4 --
      the engine renames only files. Stays debt for the first cut that renames a directory.
- [ ] **§42 mount boundaries are not enforced.** The walk descends into a directory on another volume
      (a mount), and would read `/proc` inside a copied tree. Needs a walk-level rule (the walk must not
      even read the mounted subtree), a `--cross-filesystems` option, and a report channel. Its own cut,
      decided 2026-09-25. **The destination half too:** a bind mount INSIDE the destination can present a
      source directory under a destination name, and `copy_tree` merges into it (`open_dir` refuses only
      name-surrogates), so new files can land in the source - never overwriting or deleting (every file
      publishes no-replace), and with no loop. Cut 4b refuses the case where that directory is the source
      ROOT (`copy_tree`'s Dir arm, capstone round 1, owner). Cut 8a refuses a pre-existing destination mount
      root (Part M); the remaining case is known limit 1 under "Cut 8a known limits".

## Cut 8a known limits

Recorded by cut 8a (`docs/superpowers/specs/2026-10-03-cut-8a-safety-design.md`, "Known limits").

- [ ] **DEST, or any of its ancestors, is a same-filesystem bind mount of a source subdirectory** (for example DEST
      `/dst/b/out`, with `/dst/b` a bind mount of `/src/sub`). M checks only directories the copy merges into below
      DEST, and A's canonical paths show the mount's own path, not its target. Refusing every DEST or ancestor that is
      a mount root would refuse ordinary targets such as `/mnt/usb`. Detecting this case needs the source
      identities that the memory bound forbids (spec line 997: no in-memory list proportional to the tree).
- [ ] **Linux without `STATX_ATTR_MOUNT_ROOT`** (kernels before 5.8, or a filesystem that does not report it): a
      same-filesystem bind mount is "cannot tell", so the default warns and merges. On kernels before 5.8 (the device-number
      fallback) a btrfs subvolume or other non-mount with a different `st_dev` is reported as a mount root and its
      subtree skipped (a false positive), not only "cannot tell".
- [ ] **Source-side mounts** (section 42's walk rule and `--cross-filesystems`) stay with the dedicated mount-boundary
      cut.

## Cut 8a debt

- [ ] **Part A's containment compare is case-sensitive.** `containment` compares the canonical paths with
      `lexically_within`, which compares components exactly. On case-insensitive macOS (APFS) `F_GETPATH` may
      report different casing for two handles to one directory reached through differently cased paths, so the
      check could pass; the walk's own dynamic check (`enter_dir`) would still stop the run, after sibling files
      were written into the source. Unverified (no Mac to measure on); reported by the cut 8a capstone, round 1.
      A fix compares identities of the ancestors instead (needs a parent-handle ascent), or folds case on
      case-insensitive volumes. (Single-file copies are not subject to Part A by design: it guards the walk.)
From the final review of cut 8a; none is a reachable defect without a race or privilege.

- [ ] **Part A's source handle is not compared with `src_identity`.** `containment` opens the source with
      `fs.destination_root(src_root)` and never checks it against the identity `prepare_source` took, so a source
      path swapped between the two would make Part A check another directory.
- [ ] **`locate_tree`'s filesystem-root branch opens DEST twice.** It runs `containment` on `holder` but then opens
      DEST again by path (`fs.destination_root(dst_root)`), so the checked handle and the used handle are two opens.
- [ ] **POSIX `create_dir` is `mkdirat` then `open_dir(name)`** (`dir_unix.rs`). A mount placed over the new name in
      that window is merged into unchecked, contradicting "a directory this run created cannot be a mount root".
- [ ] **An early stop drops the containment warnings.** When the run stops before the copy (for example a probe
      refusal), `run.copy` is `None`, so `containment_degraded` and the identity warnings set in `locate_tree` are
      never printed (the tree job in `main.rs`).
- [x] **`give_back` dropped a failed lock discard for any `RunError::Failed` - RESOLVED in the stabilize PR (commit
      `fix: report a lock this run created and could not remove when the run fails`).** Was: the disposal result was
      reported only for `RunError::Refused` (`crates/flux-core/src/run/session.rs`, `give_back`), so a `place.create`
      failure (the probe-error arm included) plus a lock that cannot be removed left the lock file unreported. Now
      `RunError::Failed` gained `not_removed`, mirroring `Refused`, and the report prints the leftover lock. Found by
      the cut 8a capstone, round 1.
- [ ] **Two cut 8a test gaps (minor, left out of the owner's A-D scope).** The strict containment refusal test
      (`an_unsupported_canonical_path_query_warns_under_default_and_refuses_under_strict`) asserts only the code, not the
      strict message, so a mutant swapping in the inside-source message survives; the degraded `anchor_shown` is asserted
      only for an absent DEST, not an existing DEST or a filesystem-root DEST. No test marks a pre-existing directory
      `MountRoot::No` explicitly (the fake's default does it implicitly); the Linux `Unknown` outcome of `mount_root`
      and the macOS device comparison run only on CI.

## Cut 8b known limits

Recorded by cut 8b (`docs/superpowers/specs/2026-10-07-cut-8b-replacement-design.md`, "Known limits"; the numbers
are the spec's).

- [ ] **1. A crash between a publish rename and its claim write** leaves an unclaimed published entry until the WAL cut.
- [ ] **2. Not in this cut:** resume, `PREPARE_COMMIT` / `COMMIT` and commit recovery (sections 182-184), hardlink
      groups (section 253.7), `--atomic`, `--dry-run` and the TLA+ `claims` scenario.
      (file-granular resume landed in cut 9a; the rest stays)
- [ ] **3. WSL 9p and network filesystems:** whether the backend's (`redb`) file locking works there is unmeasured.
- [ ] **4. A directory with millions of entries** holds its listing in memory while the walk is inside it (the source
      side already does).
- [ ] **4a. Under `Durability::Normal` a process kill can lose the current directory's claims** (up to 1000 commits):
      claims written since the last directory end or the store's 1000-claim cap are lost on a process crash. Documented;
      cut 9's WAL/recovery must reconcile the resulting "unclaimed published entries".
- [ ] **5. A replacement reads the destination's metadata twice** (the policy decision, then the section 129 gate): one
      extra stat per replaced file, accepted. If an outside process removes the entry between the two reads, the gate
      sees `NotFound`, the claim is still written and the file is published as new but counted as an overwrite (the
      bounded window `copy.rs` `identity_gate` documents). It never overwrites anything the engine did not plan to
      replace.
- [ ] **6. The claim insertion and the claim upgrade live in two modules:** insertion in the copy path
      (`before_create`), the upgrade and frame update in `tree.rs` after the copy path returns. Accepted, to keep the
      claim after the gate and guard.
- [ ] **7. A junction at the destination name on case-insensitive Windows:** `metadata` reports it as a link.
      Measured on windows-latest in October 2026 (a throwaway probe branch, CI run 37728541751): replacing a file over
      a directory junction fails that target alone with `IO_ERROR` / `PermissionDenied`
      (`NtSetInformationFile: 0xC0000022`, access denied) at the publish step; the junction is left intact, the
      junction's target directory is unchanged, no temporary is left, and the run completes with that one failure.
      The test `a_file_over_a_junction_at_the_destination_name` pins exactly that (commit `test: pin the measured
      Windows outcome of a file over a directory junction`). So a per-target result, never a traversal of the
      junction.
- [ ] **8. A hardlink alias on a case-insensitive destination:** an alias of an existing entry in the same directory
      makes the alias's target fail rather than replace (no unique identity match).
- [ ] **9. Closing the claim store always commits:** `redb` 4.3.0 `Database::drop` is an `Immediate` write commit plus
      a header fsync, so `state.db` is written and synced once on every path, including `Lost` (a write without
      ownership, though only to this run's own workspace file) and a clean `Completed`; the spec's "never on a clean
      `Completed` / `Lost`" covers only the explicit final sync. A non-committing close needs `redb` support or a
      different backend.
- [ ] **10. A FIFO, device or other special file at a destination name** is replaced like a regular file by a
      replacement (spec section 4 is silent). A later cut may refine it.
- [ ] **11. In a weak-identity destination directory `--skip-existing`** reports existing files as collisions (exit 1)
      instead of skipping, as the spec text says. A later cut may refine it.
- [ ] **12. A destination entry changed by an external process during the payload transfer is overwritten.** The
      identity gate (section 129) runs before the bytes stream and `rename_replace` publishes after; Flux does not
      re-stat the destination in between, and does not lock against non-Flux actors. The claim serializes Flux's own
      targets only. Same model as the existing Walker TOCTOU entry; named by capstone round 2 of cut 8b.

## Cut 8b debt

- [ ] **Propose the section 241.5 clarifications to the spec owner.** Cut 8b made these choices where the spec is
      silent: the claim key is the parent directory's identity plus the stored entry name; a claim is never released; a
      claim is written before the temporary is created and after the section 129 gate; a published new entry gets a
      `Created` claim (the upgrade of the target's own record, plus a second claim for a differing spelling after
      publication); a directory whose identity is not `Strong` degrades to no-replace.
- [ ] **`state.db` is created right after the workspace is published** (Windows refuses to rename a directory that
      contains an open file). A crash between the publish and the creation leaves a manifest-only workspace that
      `--restart` supersedes.
- [x] **The Windows kill test is slow - RESOLVED in the hygiene sweep PR (commit `test: skip the large strict-mode kill
      thresholds (an owner-approved gate change)`).** Was: `claims_kill` (`crates/flux-platform/tests/claims_kill.rs`)
      took about 5 minutes on windows-latest (strict mode, an fsync per claim, 24 thresholds). Now, in strict mode only,
      thresholds >= 333 are skipped (strict rows 24 -> 16); `normal` and `normal-nocap-test` keep all 24, so the
      cap-sync coverage is unchanged. Windows `killed_mid_stream_the_store_reopens_with_a_prefix`: PASS in 86.7 s in CI
      run 37722253880, versus 311.7 s before. One run each, not a controlled measurement (hosted runners swing about 40%
      run to run).
- [ ] **The fake `FaultFs` case-insensitive mode is ASCII-only** and does not normalize `read_file`, `remove_dir`,
      `create_lock`, `open_lock`, `create_claim_store` or the path-level `open_read`.
- [x] **The claim store's cache bound was not pinned - RESOLVED in the stabilize PR (commit `test: pin the claim
      store's cache bound`).** Was: removing `Builder::set_cache_size(CACHE_BYTES)` in
      `crates/flux-platform/src/claims.rs` left every test green. Now `RedbClaimStore::from_file` delegates to a
      private `with_cache_size`; the test `the_cache_size_setting_reaches_redb` (a 1 MiB bound, about 1,000 claims of
      about 4,000 bytes, asserts evictions > 0; red without `set_cache_size`) and `the_cache_bound_is_16_mib` pin it.
      redb's `cache_stats()` reads zero unless its `cache_metrics` feature is on, so flux-platform enables it as a
      dev-dependency feature only (the normal dependency graph is unchanged). Remaining uncovered mutant: `from_file`
      passing a number other than `CACHE_BYTES` (a one-line delegation).

## Cut 9a known limits

Recorded by cut 9a (`docs/superpowers/specs/2026-10-08-cut-9a-resume-design.md`, "Known limits"; the numbers
are the spec's).

- [ ] **1. Claims are keyed by directory identity**: a destination directory deleted and recreated between runs has a
      new identity, so its old claims miss and its files are copied again (safe). If the filesystem reuses an inode
      number, a stale claim can make a later target see a foreign claim and fail loudly with
      `DESTINATION_NAMESPACE_COLLISION` (never a silent overwrite).
- [ ] **2. A destination changed during the downtime** is caught only by the size and mtime check; a same-size,
      same-mtime change is not.
- [ ] **3. Files published in the unsynced window** (limit 4a of cut 8b) are copied again under overwrite.
- [ ] **4. Orphan partials of targets that vanished between the runs** are swept only when their operation is removed
      by `flux cleanup` (STALE or `--force`d) or superseded by `--restart` (the J1 walk, by operation id); a run
      resumed to completion does not record them, so they stay (cut 9b spec, known limit 2).
- [ ] **5. A manifest-only workspace in state `CREATED`** resumes with zero progress; one in any later state without
      `state.db` is `STATE_CORRUPT`.
- [ ] **6. Claim keys contain the filesystem's device number**. Linux does not guarantee that `st_dev` is stable
      across a reboot (device numbers of removable, LVM or network-backed volumes can change), and an interrupted copy
      is often followed by a reboot: when the numbers change every claim misses and the resume copies everything again
      under the existing-file policy (safe, but the resume saves nothing). Not testable in CI; to be measured on real
      systems before slice 9c.
      The source root's `Strong` identity also contains `st_dev`: if the source volume's device number changes across a
      reboot, `--resume` is refused with `INCOMPATIBLE_STATE` ("the source root differs"); use `--restart`.
- [ ] **7. Weak-identity destination directories never get claims**: a resumed run reports their existing files as
      collisions (see the walk section).
- [ ] **8. A destination that cannot take the source's modification times** (`preserve_times = Default` records the
      failure and still publishes) holds the copy time instead, so the mtime check never matches: every file of such a
      destination is copied again on each resume (safe, but the resume saves nothing there). `--preserve-times`
      (strict) fails those files instead.
- [ ] **9. A binary older than this cut** (cut 8b and earlier) reads format 3 as an unknown version: it refuses with
      `INCOMPATIBLE_STATE` (exit 3) before it looks at `--restart`, so it cannot supersede a format-3 prior. Use the
      newer binary, or remove `DEST/.flux/operations/<id>` by hand. The same holds for every future format and cannot
      be changed in binaries already shipped.
- [ ] **10. A destination used from two environments that identify things differently** (native Windows and WSL on one
      NTFS volume) cannot be resumed from the other side: source paths are encoded and written differently (UTF-16
      versus UTF-8 hex) and object identities are derived differently, so the mapping check refuses with
      `INCOMPATIBLE_STATE` or every claim misses. Resume in the environment that started the copy (see also limit 6).
- [ ] **11. Progress is discarded when a tree finishes with per-file failures**: the walk returns `Ok`, the run
      completes, the workspace and its claims are removed, so a later `--resume` starts new and copies everything again
      under the existing-file policy (safe; a retry of only the failed files is a later cut).

## Cut 9a debt

- [ ] Stale-lease classification (9b) should know that a long resume skip refreshes no heartbeat; a live resume holds
      the lock and is LIVE, so only an unlocked operation depends on the lease (cut 9b known limit 5).
- [ ] State writes do not enforce `STATE_LIMIT` (a source root of more than 16K units on Windows makes a format-3
      manifest unreadable).
- [ ] The resume note prints after the per-file failure lines (spec order: note, then report).
- [ ] A single-file case-variant target on a case-insensitive filesystem is offered `--resume` by the refusal but
      refused by the byte-exact `destination_prefix` check.

## Cut 9b known limits

Recorded by cut 9b (`docs/superpowers/specs/2026-10-08-cut-9b-cleanup-design.md`, "Known limits"; limits 1-8 are
the spec's numbers, 9-11 were learned during execution).

- [ ] **1. A copy that starts in the instant a cleanup (or a classification) probes the lock** is refused with
      `TARGET_LOCK_BUSY` (D1). Cleanup probes once per operation directory and holds nothing across the listing.
- [ ] **2. Partials of a vanished target** are swept only when their operation is removed by `flux cleanup` (STALE or
      forced) or superseded by `--restart` (the J1 walk). A run resumed to completion does not record them: they stay
      (9a limit 4, narrowed, not closed).
- [ ] **3. Single-file operations' state records** (`<target>.flux-state.<id>`) are completed only by a copy to that
      target; `flux cleanup DEST` does not list them (the standalone catalog is out of scope).
- [ ] **4. Retention reads the manifest's modification time**: a restore from backup or a tool that rewrites times
      changes the age. The ownership checks, not the age, are what keep a live or ambiguous operation safe.
- [ ] **5. A long `--resume` skip refreshes no heartbeat** (9a debt); a live resume holds the lock and is LIVE, so only
      an unlocked operation depends on the lease.
- [ ] **6. `--force` can delete a RESUMABLE operation** and with it the progress `--resume` would have used; this is
      its purpose.
- [ ] **7. A boot-session mismatch is not used**: liveness is decided by the OS lock, the lease gate and the clock rule
      only (macOS and Windows boot identifiers are boot times and are unmeasured under clock steps).
- [ ] **8. `--break-lock` for cleanup, `--target`, the standalone catalog and `P/.flux/atomic/` do not exist**; an
      UNCERTAIN row can only be reported.
- [ ] **9. A dead owner's lock with a young or future heartbeat** (a killed `flux cleanup`, a crashed resume; a future
      heartbeat fails the gate like a young one) makes a deleting pass skip every eligible row with
      `skipped <id>: destination lock busy` and exit 0. The real reason, "the previous owner's lock is younger than the
      lease threshold", is not in the line. A younger orphan lock is refused as `ARTIFACT_OWNERSHIP_UNCERTAIN` and the
      pass exits 1.
- [ ] **10. An unreadable `DEST/.flux`** is reported as a whole-run refusal (exit 3) because `check_control_plane`
      stats and opens it before discovery lists anything, not as a failed listing (exit 1).
- [ ] **11. A `<lock>.broken.*` file** is classified by its own record whether or not the workspace it names exists
      (spec row 11 wording corrected).

## Cut 9b debt

- [ ] The single-file orphan-lock refusal also appends the `flux cleanup DEST` pointer sentence although `flux cleanup`
      does not handle single-file operations (`crates/flux-core/src/lock/obtain.rs`, the Orphan arm of `obtain`).
- [ ] The `Skipped` reason text for a young dead lock (see limit 9).
- [ ] `S251_1_close` stays `not-in-7a` only because `tests/model_impl_map.rs:141` needs one such label.
- [ ] The lock model has no notion of time or leases, so it does not capture the lease gate on the Dead and Orphan arms.
- [ ] The age test does not pin 86_399 -> 23h.

## Scaffolding follow-ups

- [ ] Run `lefthook install` in each clone (or add it to a bootstrap recipe)
- [ ] Replace the placeholder `benches/copy.rs` once there is a pipeline to measure
- [ ] Replace the placeholder test in `tests/integration/mod.rs` with the first
      real case from spec §68.2

## Repository and CI hygiene

Promoted from the local anomalies inbox (triage of 2026-09-14, and of 2026-09-24 for the first item);
each was re-measured on `origin/main` that day.

- [ ] **The local gate is Windows-only, and two non-Windows compile breaks reached CI in PR #37.**
      `just check` runs on the dev machine alone, so a `#[cfg]` mistake cannot fail locally. MEASURED
      twice in one PR: an ungated `#[test]` calling a `#[cfg(windows)]` helper (`error[E0425]` on Linux,
      caught only because a WSL cross-check was run by hand), and a test assuming `cfg(unix)` permits
      non-UTF-8 filenames (macOS APFS/HFS+ enforce UTF-8 and returned `Os { code: 92, "Illegal byte
      sequence" }` — caught only by CI). Add a `just check-linux` recipe wrapping the WSL leg so the
      cross-check is a command rather than a habit, and note in the justfile that **macOS has no local
      equivalent on this machine**, so the macOS leg is CI-only by construction.
      (Narrowed 2026-09-26: `just check-linux` and `just check-mac` now exist, commit `4e2fc4d` — the
      Linux leg runs clippy plus the full test suite through WSL, closing that half. What remains: the
      macOS leg runs clippy only and no test, so a macOS RUNTIME difference — such as the `cfg(unix)`
      non-UTF-8-filename assumption above — is still caught only by CI.)

- [ ] **No MSRV job, and the policy change makes the original one moot.** `rust-version` now tracks the
      current stable release (1.98.1) rather than the oldest toolchain that compiles, so the job this item
      originally asked for — build at the floor, catch a dependency that outgrew it — cannot fail by
      construction: nothing can require a rustc newer than the latest. What the policy needs instead is the
      OPPOSITE check, that `rust-version` has not fallen behind stable, because a floor pinned to a specific
      patch release goes stale the moment the next one ships and nothing in the repo notices. That is how the
      previous value drifted: it read 1.85 while the code had needed 1.88 for some time, and `criterion`
      already required 1.86. Decide which of the two this repo actually wants before writing either job.
- [x] **`ci.yml` token scope - DONE in the hygiene sweep PR (commit `ci: restrict the CI token to contents: read`).**
      Was: jobs got the repository's default token scope. `ci.yml` now sets a top-level `permissions: contents: read`.
      The `concurrency:` block already existed (`group: ci-${{ github.event_name }}-${{ github.head_ref ||
      github.ref_name }}`, added by commit `e94b20c`), so nothing was added there.
- [ ] **An empty release pull request is indistinguishable from a real one.** `release-plz` opens one on
      every push to `main`, and `release-plz.toml` deliberately skips `spec`, `design`, `plan`, `model`,
      `docs`, `skills` and `test` — which is most work in this repository — so a typical push produces a
      version bump with **zero** changelog entries. Four have been closed for that reason so far (#22 was a
      duplicate of `v0.1.1`'s own content; #24, #26 and #29 were empty), and the next one will look exactly
      as actionable as a real release. The distinguishing check is one command,
      `gh pr diff <n> | grep -c '^+- '`, returning zero — so make it a gate rather than a habit: a job on
      release pull requests that fails when the changelog diff adds no entries, or a `release-plz` setting
      that declines to open one. A habit is not a control; the person who merges it will not be the person
      who learned the habit.

Triage of 2026-09-23 adds one more, re-measured that day.

- [ ] **A `flux-platform` mutants report covers the Unix half only.** The original observation (12 of 26 mutants missed,
      made on the previous Windows development machine) is unverified here and was not reproduced on Linux: for `cargo
      mutants -p flux-platform --re destination_is_write_protected --no-shuffle`, 10 mutants ran, 5 CAUGHT and 5 MISSED.
      All 5 `#[cfg(unix)]` mutants (including `-> true` and `-> false`) are caught; the 5 MISSED are the
      `#[cfg(windows)]` twin, which is not compiled on Linux, so no Linux test can catch them. No `.cargo/mutants.toml`
      is needed for the Linux result. A Windows-only mutant can be judged only on Windows (the CI Windows leg or a
      Windows box), so read a `flux-platform` mutants report as covering the Unix half only until then.

## Spec gaps from the filesystem probes

Measured by the lock-model probes (`crates/flux-platform/tests/fs_semantics.rs`, branch `model/lock-protocol`)
against spec V16.

- [ ] **Windows file identity source.** §107 and §109.1 give only strength classes. The 64-bit file index is not
      guaranteed unique on ReFS (Dev Drives are ReFS); a strong identity needs `FILE_ID_INFO` (volume serial plus
      128-bit file id) from `GetFileInformationByHandleEx`.
      (Narrowed 2026-09-26: the code side is done — `identity_of_handle` already reads `FILE_ID_INFO` via
      `GetFileInformationByHandleEx`, `crates/flux-platform/src/std_fs.rs:749-774`. What remains is the spec
      text: §107 and §109.1 in `FLUX_FULL_UPDATED_SPEC_V16.md` still give only the `Strong`/`Weak`/`Unavailable`
      strength classes and name neither `FILE_ID_INFO` nor `GetFileInformationByHandleEx`.)
- [x] **WSL 9p mounts break two lock assumptions.** On `/mnt/c` (v9fs), `renameat2(RENAME_NOREPLACE)` onto a free
      name fails with `EINVAL`, and a file renamed while open is listed but cannot be `stat`ed until the handle
      closes. Lock-capability detection (§96.1, §99) must treat such a mount as lacking both, and fall back to the
      directory lock or refuse.
      DONE in cut 7a: 9p is not on the lock-capability allowlist, so a copy onto such a mount is refused
      `REMOTE_LOCK_UNSAFE` before anything is created.

## Lock-model follow-ups

From plan 2, now merged (was PR #3). Each was verified by measurement; M5 to M8 come from the plan-2 test
audit (`docs/agy-test-audit-ledger.md`), where the owner deferred them as minor.

- [x] **Cache the TLC jar in CI - DONE in the hygiene sweep PR (commit `ci: cache the pinned TLC jar across the model
      jobs`).** Was: each `model.yml` scenario job downloaded and checksummed `tla2tools.jar` on its own. Now
      `actions/cache@v6` restores the jar, keyed on the pin read from `models/lockproto/run.py`, and `run.py` still
      SHA-256 checks the restored jar. A manual `Model` dispatch run on the branch (GitHub Actions run 37722300785) was
      started to exercise a cold cache; it was still in progress when this was written, so its result is not recorded
      here.
- [ ] **`--no-renames` is unpinned.** Removing it from `model.yml`'s change detection keeps every test green, and
      then a pull request that only moves a file out of `models/` skips the model jobs. Test the step itself, or move
      change detection into `run.py`.
- [ ] **`open_findings[].tracking` is not validated.** Design Section 4 says it names a `TODO.md` or anomalies
      entry, but `run.py` accepts any non-empty text. Check it at load time, or restate the field as free text.
- [ ] **cp1252 stdout.** Python on Windows writes a piped stdout as cp1252, so a non-ASCII `reason` or TLC
      message crashes `run.py`. Call `sys.stdout.reconfigure(encoding="utf-8")` in `main`, or set
      `PYTHONIOENCODING` in the workflows.
- [x] **M5, `NeverTornRead` is placement-blind.** Setting `tornRead` unconditionally at `S240_1_read` keeps the
      witness violated. Replace the ghost with a state predicate over `seenRec` and `classified`.
      Closed in cut 6 (`ea28f57`): `NeverTornRead` is now a state predicate over `seenRec`/`classified`, no ghost.
- [x] **M6, `recoveredAfterCrash`'s guard is always true** at `S240_3_s5`, so moving or dropping it changes no
      run; the witness restates the label's coverage.
      Closed in cut 6 (`bc3f599`): `ReplacedOnlyDead` replaces the always-true `recoveredAfterCrash` witness.
- [x] **M7, host-crash net holes.** Making `FsHostCrash` always keep new content, or emptying `Unflushed`, left
      the host-crash runs green.
      The negative control (flipping the comparison in the `hostCrashChangedLock` update) and the past-ids seed
      for `IdsNotReused` landed in cut 6 (`7a52dd3`). Closed in cut 6 Part 2 (`8cb8947`): the single-Owner witness
      `NeverRevertedOwnWrite` must be violated, and both mutants make it unreachable. The torn outcome is a residue
      (below).
- [ ] **M8, the liveness consequent can be weakened undetectably.** Widening `UncertainReported` or making
      `DeadLockEventuallyCleared` trivially true keeps its witness violated and the liveness run green. Add a recovery
      seed (a Recoverer that gives up) that must violate the property.

## Lock-model follow-ups (plan 3)

Triage of the local anomalies inbox, 2026-09-21: six capstone rounds, two test-audit rounds, and the
v0.1.0/v0.1.1 release. Each was verified by measurement; the inbox entry carried the evidence and was deleted
by the commit that added this section.

**One re-measurement covers the first three.** Each is a model change needing a fresh full run, and
`breaklock-remote` alone was ~2.5h before its split, so batch them rather than paying three times.

- [ ] **`pendingUnlink` could be a boolean.** Every use tests it only against `NoObj`; its captured identity
      became dead when `LandUnlink` moved to resolving the lock NAME at the landing. As a per-process object id
      it carries `MaxObjs + 1` values per process against two for a boolean, and the remote checks run at 60M+
      states — a real state-space reduction, not tidiness.
- [x] **`tornRead` is set at one of the two labels that read a record.** The classify read sets it; `S240_5_s5`
      does not, so a torn read during a takeover goes unrecorded and the ghost under-reports its own declared
      meaning. It feeds only `NeverTornRead`, which already fires from the classify path, so it buys no coverage
      today — do it while the states are being re-measured anyway.
      Narrowed in cut 6 (`ea28f57`): the `tornRead` ghost is gone. Torn-versus-empty at takeover is a residue,
      tracked below.
- [x] **`IdentityStrength = "weak"` is dead across every run.** All 74 runs pin `"strong"` while `FsModel.tla`
      implements the weak value at four sites. The README justifies the pin only for `recovery`, and the one
      protocol decision that discriminates on it (`algorithm.txt:443`) is in the Breaker/TakeOver path, which
      `recovery` has no actor for. Give it a `breaklock`-scoped justification, or drop the value.
      Closed in cut 6 (`0e4e2cc`): weak-identity checks and the `NeverSecondPastId` witness give the value its own runs.

**Coverage, where the gate is weaker than it reads.**

- [x] **Build the BRANCH-granular coverage union — it is debt, not a limit.** Closed in cut 6 Part 2
      (`1e9e235`, `b446616`): `branches` in `expected.toml`, judged by `--union-from`. TLC's `-coverage 1` is
      expression-granular (message 2221, nested, zeros included) and the repo's own fixture proves the shape at
      `models/lockproto/testdata/smoke_check_coverage.out:109`. `run.py` already parses that message and
      deliberately discards it, so a branch union is buildable from logs on disk at no TLC cost. Settle two things
      first: the nodes key off `line,col` of the GENERATED module (which `--check-translation` pins), and the
      shape TLC emits for a PlusCal `if`/`else if` INSIDE a label was never measured. This blind spot has already
      produced two findings.
- [ ] **Say which `never_reached` claims an exhaustive run supports.** 52 of 77 runs halt at a counterexample, so
      their coverage is a PREFIX. Positive coverage from a prefix is sound and `uncovered` fails closed, but the
      one check that fails OPEN — a label a halting run would have covered later — is blind across two-thirds of
      the suite. The exhaustive check and liveness runs alone carry those claims; the union could mark each claim
      supported or not, turning a suite-wide caveat into a per-claim one.
- [ ] **Commit the union verdict as an artifact.** "103 labels covered, 0 never_reached, 1 deferred" exists only
      in prose nobody can confirm or refute. A `union.txt` beside `expected.toml`, or the verdict pinned as a
      fixture, makes the most-cited number in this work re-readable. Cheapest item here, and it answers the
      review's structural finding: of ~84 factual claims in this branch's commit messages, ~45 are permanently
      unverifiable — every state count, every timing, all sixteen CI run ids — because GitHub log retention is 90
      days and commit messages are forever.

**Seeds that pin their own seed rather than the property.** Each needs TLC, so each is CI-only; the mutant is
stated so the next audit starts from a prediction rather than a hunt.

- [x] **`FsOk`'s other two conjuncts are still indistinguishable from `TRUE`.** `FsOk` is
      `NoDoubleOpen /\ LockImpliesHandle /\ IdsNotReused`, and `SEED_FS_LOCK_WITHOUT_HANDLE` breaks only the
      middle one. Mutant `FsInvariants(fs) == LockImpliesHandle(fs)` is predicted GREEN.
      Closed in cut 6 (`5f8155f`): one seed per `FsOk` conjunct.
- [x] **`Classifiable`'s seed pins the seed.** `SEED_FS_ALIEN_CONTENT` writes `NoProc` specifically, so the
      mutant `Classifiable == \A o \in Objs : fs.content[o] # NoProc` is predicted GREEN — the invariant gutted
      to a NoProc-check and still satisfied by its own seed.
      Closed in cut 6 (`921c71d`): a record-shaped alien seed, so `Classifiable` cannot be gutted to its
      `NoProc` case.
- [x] **Two of `RefusalJustified`'s three `lostLock` sites remain unseeded.** `SEED_CHECK_REFUSES_UNTOUCHED`
      (added 2026-09-21) pins `S99_check`, because a seeded run halts at the first violating state. A mutant
      hardwiring `refusedOk` at `S99_release_check` or `S21_1_s3` still survives; closing it needs one run per
      site — the same shape that needed five `NeverRefusedUnsafe` witnesses.
      Closed in cut 6 (`fa9f5ce`): seeded runs for `RefusalJustified`'s release-check and restart-check sites.
- [x] **`PlainNeverOwnsUncertain`'s ghost is wired to one label.** `touchedUncertain` is assigned at
      `algorithm.txt:299` and nowhere else, so only the move-aside is instrumented against an invariant that also
      covers removing, renaming and creating.
      Narrowed in cut 6 (`1e9f7ee`): the invariant's claim is narrowed to the move-aside it instruments. The
      uncovered cases (remove, rename, overwrite, create) are a residue, tracked below.
- [x] **`ForeignUntouched`'s content half is accepted as unreachable by reasoning.** The plan-2 audit
      dispositioned it so and re-validation at HEAD agrees, but the argument is reasoned, not measured. Run its
      mutant alongside the others rather than on its own.
      Closed in cut 6 (`a87a5c3`): `ForeignUntouched` is split into `ForeignStaysAtLockPath` and
      `ForeignContentUntouched`, and the content half is seeded (`SEED_TAKEOVER_FOREIGN`).

**Smaller, each independently shippable.**

- [ ] **The stamp cannot see an acceptance test.** `spec-sections.stamp` lists twelve headings, and §259.14 —
      which opens "A conforming implementation must test at least:" and is therefore normative — is not among
      them. So a rule can be amended under a stamped heading while the acceptance item testing it keeps demanding
      the old outcome, and `model_stamp` passes. That is exactly what `788267a` did. Stamping 259.14 is cheap and
      would have caught it.
- [ ] **The model cites RFC 7530 for a sentence it does not contain.** `models/lockproto/algorithm.txt:44` says
      "the server resolves that name when it EXECUTES the call (RFC 7530, design Section 5.3)". §16.26 supports
      only the name-based half — *"removes (deletes) a directory entry named by filename"* — and says nothing
      about timing. Measured with a positive control: `REMOVE` 14 hits, `filename` 3, `directory` 15, against
      `retransmit` / `resolve` / `executes` / `stale` all 0. The claim is sound as an INFERENCE; the citation
      overstates it. Prose, not behaviour, but load-bearing — this reading forced three remote windows to be
      re-measured. The fix edits a PlusCal comment, so it needs `pcal.trans` + SANY + `--check-translation`.
- [ ] **A halting run's state count is not reproducible.** `run.py:1138` runs TLC with `-workers auto`, so a run
      stopping at the first violating state stops wherever the workers had got to: the same model on two commits
      differing by one docs line reported 6,105 against 6,976 states. 52 of 77 runs halt. Either use `-workers 1`
      for halting runs, or stop quoting exact counts for them — they evidence "the seed fires, roughly this
      deep", never a number.
- [x] **`S240_5_s6`'s two refusing branches are indistinguishable to every gate.** Both set
      `refused := "RESTART"` with identical successor state, `RefusalJustified` no longer reaches that label, and
      coverage is label-granular — so nothing can witness that the "another file is there" case is reached.
      Closed in cut 6 Part 2 (`d56eefc`, `5fd5040`): the `s240-5-s6-another-file` branch entry and its mutant.
- [ ] **`trace.toml`'s exemption for family 259 states the wrong reason.** It reads "normative precedence between
      spec sections is outside the lock-protocol model's scope", but §259.13 ranks RUNTIME SIGNALS, not spec
      sections. The exemption may well be right; its stated reason describes something the section does not say.
- [ ] **Make the SEED-flag complement a test.** A review seat computed it with a scratch script — collect
      `SEED_[A-Z0-9_]+` from `LockProtocol.tla`, collect `SEED_... = TRUE` across `configs/*.cfg`, subtract —
      which is how "the complement is empty" came to be measured rather than asserted. It belongs in
      `test_run.py`.

**Residues from cut 6.**

- [ ] **POSIX double open is invisible to `NoDoubleOpen`.** `share` forces `del = TRUE`, so a real double open
      collapses into one handle record (cut 6, item 7).
- [ ] **M8: a widening of `UncertainReported` that admits only `"RESTART"` is not killed by the gives-up seed**
      (cut 6, item 9).
- [ ] **`PlainNeverOwnsUncertain` covers only the move-aside (`S240_3_s2`).** Removing, renaming, overwriting or
      creating an uncertain lock are not covered (cut 6, item 11).
- [ ] **Torn versus empty at takeover.** No witness distinguishes a torn read from an empty one at `S240_5_s5`
      (cut 6, item 3).
- [ ] **A failing check or liveness run with `-continue` can be reported as "TLC error 2111" instead of "X
      violated".** TLC failed internally while printing many violation traces (CI run 36497846414, mutants). It
      stays red, never green; the mutant runner avoids it by narrowing (`e29dc38`); normal runs still use
      `-continue` (`models/lockproto/run.py`, `tlc_command`).
- [ ] **TLC 2.19 reported a valid property violated when a `<>` argument is a constant.** The mutant
      `[](... => <>TRUE)` on `recovery-posix-seeded-SEED_RECOVERER_GIVES_UP` came back "Temporal properties were
      violated" (CI run 36506185735); the state-level tautology replaced it. Real properties have state-level
      arguments; a new property with a constant `<>` argument would need checking.
- [ ] **`FsHostCrash`'s torn outcome has no net.** TLC gives the `CASE` arms inside its function constructor no
      coverage node (measured: no node for `FsModel.tla:312-318` in any extended host-crash log), and no state
      predicate separates a torn crash from a crash in the middle of a write (cut 6, item 6).

## Performance (preliminary speed probe, 2026-10-02)

The figures below were measured on the previous Windows development machine (NTFS C:, Defender on). The development
machine is now a Linux ext4 box with 8 cores and no `robocopy`, so they are not a baseline here. A re-probe needs
a fresh baseline on this machine with comparators that exist here (`cp`, `rsync`); a `robocopy` comparison needs a
Windows machine or the CI Windows leg. Any measurement waits for the owner's OK, an idle machine and two runs
quoted as a range.

Measured at cut 7b Part 1 (`f755c50`, release build) against robocopy and FastCopy 5.12.0 (`fcp.exe`). Method:
- C: (NTFS, not the ReFS Dev Drive, where Windows' own copy can clone blocks);
- every copy checked for file count and bytes;
- tool order rotated so each tool went first once per case;
- the figures are the three runs where the tool was NOT first.

Running first costs every tool extra time, apparently because Defender's on-access scan is paid by whoever touches a file first. An earlier probe that always ran Flux first overstated the small-file gap at 7x.

Uncontrolled: Defender real-time protection was on for every tool, with about 24% background CPU on a 4-core machine.

| Case | Flux | robocopy | robocopy `/MT:8` | FastCopy |
|---|---|---|---|---|
| 1 x 4 GiB | 31.1-34.1 s (~126 MiB/s) | 14.7-17.6 s | 20.2-21.5 s | 28.2-30.7 s |
| 10,000 x 4 KiB | 9.2 ms/file | 3.1-3.3 ms/file | 1.1-1.2 ms/file | 7.1-7.4 ms/file |
| 2,000 files, 10.5 GiB | 93.5-95.8 s (~112 MiB/s) | 56.4-57.7 s | 65.5-66.0 s | 79.4-88.0 s |

The lock costs little. A pre-7a build (`5fe1f62`, no lock) copied the 10,000 files at 8.4-8.5 ms/file against 8.9-9.3 for the locked build, about 5-10%.

- [ ] **Large-file throughput: Flux is ~2.1x slower than robocopy on one 4 GiB file.** The copy streams through a
      64 KiB user-space read/write loop (`crates/flux-core/src/copy.rs`, step 4). Unverified candidates:
      - a larger buffer;
      - `CopyFileEx` / `copy_file_range` / reflink fast paths;
      - unbuffered I/O for large files, as FastCopy does from 64 MiB.

      Measure before choosing.

      Progress 2026-10-08: the first lever, a larger user-space buffer, is done in the commit with the subject `perf:
      size the copy buffer from the source, capped at 256 KiB` (buffer = source length clamped to 4 KiB..256 KiB; on
      Linux about 25% lower median wall time for a 4 GiB file and no slow outliers; not yet measured on Windows or
      macOS). The Windows figure stands until re-measured on Windows. Remaining candidates unchanged (`copy_file_range`
      / reflink / `CopyFileEx` fast paths, unbuffered I/O for large files).
- [ ] **Small-file throughput: Flux is ~2.8x slower than robocopy and ~8x slower than robocopy `/MT:8`.** Flux
      copies one file at a time. `/MT:8` shows that copying several files at once is the largest lever. The per-file
      work also counts: three ownership checks, each a stat plus a 4 KiB read of the lock, the temporary-then-rename
      publish, and extra source stats.

      Profile one run, for example with Windows Performance Recorder, before choosing.

      Linux figures 2026-10-08 (see the re-probe below): Flux is about 5x slower than `cp -r` on 10,000 x 4 KiB,
      per-file work dominates, and the buffer change is neutral.
- [ ] **Re-probe after each change, with the order rotated.** Single runs and a fixed tool order are not
      measurements here (see the first-toucher effect above).

## Re-probe on the Linux development box (2026-10-08)

Method: this box is a Linux (Ubuntu, kernel 7.0) VM, 8 vCPUs (AMD EPYC), 23 GiB RAM, ext4.

- all runs warm page cache (no cache drop);
- each run followed by `sync` (timed separately, about 3-4 s for 4 GiB for every tool);
- release binaries built from the same commit differing only in the copy buffer;
- tool order rotated each pass so every tool went first once per rotation;
- machine idle (whole-machine busy 1.7-4.1% before, between and after the runs, with a control loop that added 10-14
  points, proving the check can see load).

Uncontrolled: `agy`, `opencode` and the controller's own process in the background (about 2% busy), VM neighbours (steal
time not measured), writeback throttling by the kernel (the dominant noise source: `cp` alone ranged 2.6-10.9 s on one
input).

Results:
- One 4 GiB file, the old fixed 64 KiB buffer versus 256 KiB, 1 MiB and 4 MiB (two independent rotated passes of 6 runs
  each, 12 per tool): wall medians 7.77 s (64 KiB), 4.96 s (256 KiB), 5.11 s (1 MiB), 5.13 s (4 MiB), `cp` 6.47 s. 64
  KiB is slower than each larger size (permutation test on the difference of medians, p = 0.003, 0.026, 0.004); 256 KiB,
  1 MiB and 4 MiB are indistinguishable. 6 of 12 runs of 64 KiB took over 8 s, versus 0, 1 and 0 for the larger sizes.
  CPU time (user+sys, kernel time dominates; second pass, 6 runs per tool): median 5.54 s (64 KiB) versus 3.92 s (256
  KiB), 3.83 s (1 MiB), 3.92 s (4 MiB).
- Re-probe of the shipped change (old main = fixed 64 KiB versus new = buffer sized from the source length, clamped to 4
  KiB..256 KiB; 6 runs each): one 4 GiB file wall median 6.09 s (old; one 35.09 s outlier) versus 5.62 s (new; range
  4.89-6.43), `cp` 4.99 s. Pooled over all three sets of large-file runs (18 per side: the 64 KiB buffer versus the 256
  KiB buffer, old/new binaries included): median 7.02 s versus 5.28 s (25% lower), permutation p = 0.006, 7 of 18 runs
  over 8 s versus 0 of 18.
- 10,000 files of 4 KiB in 100 directories (folder copy, 6 runs each): Flux old 2.49 s median (0.25 ms/file), Flux new
  2.26 s (p = 0.20 for the difference, so no regression), `cp -r` 0.42 s, `rsync -r` 0.99 s. So on this box Flux's
  small-file throughput is about 5x slower than `cp -r` and 2.3x slower than `rsync -r`, which the buffer change does
  not touch.

These figures are NOT comparable with the table above (different machine, filesystem and tools). Raw results and scripts
are kept on the development box under `~/flux-probe` (not in the repository).

- [ ] **Heartbeat cadence per chunk.** The copy loop calls the heartbeat once per chunk; a chunk is now up to 256 KiB
      (was 64 KiB), so beats per byte fell fourfold for large files. The beat is rate-limited to once per 5 s and
      section 101 sets no maximum gap per chunk. The 30 s stale-lease threshold comes from the spec (items at lines
      4186 and 5005 of `FLUX_FULL_UPDATED_SPEC_V16.md`), not from a constant in the code, which has only the 5 s
      `HEARTBEAT_INTERVAL` (`crates/flux-core/src/run/mod.rs`). Because the beat can lag the last call by up to 5 s,
      a chunk must complete within 25 s to keep the heartbeat under 30 s old: the worst case holds above about
      10 KiB/s of sustained throughput (256 KiB / 25 s = 10.24 KiB/s; it was about 2.6 KiB/s at 64 KiB). Very slow
      media could appear stale during one slow chunk, though staleness also requires that no live process holds the
      lock (section 102). This is a documented trade-off, not a defect.

## Deferred from cut 7b

Each is deferred to the cut that first reads it (cut 7b spec, `docs/superpowers/specs/2026-10-02-cut-7b-state-v2-and-heartbeat-design.md`, Scope).

- [ ] **`configuration_fingerprint` (§19, §121)** - cut 9: read only by resume; most §121 options do not exist yet.
- [ ] **Checkpoints and `last_checkpoint` (§19, §139, §148, §164)** - cuts 8/9: the spec defines them only through
      the WAL and `state.db` (§119's layout).
- [ ] **`roots` (§19, §18.3)** - cut 10: one root until several sources exist.
- [ ] **`last_heartbeat_monotonic` (§229.2)** - cut 9: "diagnostic/current-session data only".
- [ ] **Finishing a prior run's `cleanup_pending` (§21.1 "may", §218)** - cut 9: a COMPLETED prior with
      `cleanup_pending = true` and its artifact list is proceeded past and left.
- [ ] **Stale classification from heartbeat age (§102)** - cut 9.

## Closed

Triaged 2026-09-22. Kept here rather than deleted: the evidence for a closure belongs where the
item was, not only in a commit message.

- `DONE` **Validate `cleanup_pending_artifacts` entries as relative paths without `..`** (was under "Cut 9a
  debt"). Done by cut 9b: `cleanup::artifacts::validate` in `crates/flux-core/src/cleanup/artifacts.rs` applies the
  artifact rules before anything is deleted by an entry.
- `DONE` **Model-check the lock protocol before implementing it.** Done by lock-model plans 1 to 3.
  The item asked for two recoverers, two `--break-lock` takeovers, a stalled prior owner and a plain
  run, checking that at most one operation ever owns a target: `recovery` runs `Recoverers = {r1, r2}`,
  `breaklock` runs `Breakers = {b1, b2}`, crashes model the stalled owner, `PlainRuns` covers the plain
  run, and `SingleWriter` is the at-most-one-owner invariant — 5 occurrences in
  `models/lockproto/invariants.txt`, checked across the scenarios `run.py --list-jobs` reports. The item
  offered TLA+ *or* `loom`/`shuttle` as alternatives; TLA+ satisfies it.
- `OVERTAKEN` **`.antigravityignore` is untracked in the primary working tree.** It is tracked on `main`:
  `git ls-files .antigravityignore` returns it. It reads as untracked only in `E:/Rust/flux`, which sits
  on the stale `model/lock-protocol` branch — the remedy is to move that tree, and there is nothing to
  change in the repository.
- `DONE` **Windows replacing rename is unnamed.** §241.5 now names the POSIX-semantics rename for
  replacing publication and states why `MoveFileExW(MOVEFILE_REPLACE_EXISTING)` cannot serve, citing
  the FS-6 and FS-7 probes. Implemented as `StdFileSystem::rename_replace`.
- `DONE` **`rename_no_replace` is a check-then-rename race** (was under "Known limits of the first
  single-file copy"). `rename_no_replace` now publishes atomically instead of check-then-rename:
  `crates/flux-platform/src/std_fs.rs:316` (Unix: `renameat_with` + `RenameFlags::NOREPLACE`),
  `std_fs.rs:363` (Windows: `MoveFileExW` without `MOVEFILE_REPLACE_EXISTING`), and the `DirHandle`
  forms at `crates/flux-platform/src/dir_unix.rs:167` and `dir_windows.rs`'s `fn rename_no_replace` ->
  `rename_at(.., false)`. Landed in PR #38 (`feat/copy-tree`, "atomic no-replace publication").
- `DONE` **`rename_no_replace` is check-then-act.** (was under "Known gaps in the single-file copy",
  duplicate of the entry above). Same citations and same fix, PR #38.
- `DONE` **`_typos.toml` excludes the spec by its literal file name** (was under "Repository and CI
  hygiene"). Fixed by this branch's Task 1, commit `2f25822`: `_typos.toml`'s exclude is now the glob
  `"FLUX_FULL_UPDATED_SPEC_V*.md"`.
